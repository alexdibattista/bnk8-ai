//! Typed commands from PROTOCOL.md on top of the transport.
//!
//! Reply matching: the device emits unsolicited `0x20`/`0x21` frames, so
//! every exchange reads frames until one matches both msg_type and seq.
//! NAK replies (status 3) map to [`Nak`] with the device error code.

use std::fmt;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail, ensure};

use crate::frame::{self, Frame, MAGIC, REPORT_LEN};
use crate::transport::Bnk8;

/// Config blob size, fixed by the firmware (`0x10` reply, `0x11` begin).
pub const CONFIG_LEN: usize = 2000;
/// Stream area slot size and max stream data, from `0x15` sub 5.
pub const STREAM_SIZE_MAX: u16 = 0x1810;

/// CRC-32/ISO-HDLC, the firmware's config and session checksum.
pub fn crc32(data: &[u8]) -> u32 {
    crc::Crc::<u32>::new(&crc::CRC_32_ISO_HDLC).checksum(data)
}

const MSG_HELLO: u8 = 0x01;
const MSG_STATUS: u8 = 0x10;
const MSG_TRANSFER: u8 = 0x11;
const MSG_COMMIT: u8 = 0x12;
const MSG_STREAM: u8 = 0x15;
/// Max data bytes in one `0x11` sub-1 chunk (22-byte payload minus 7 header bytes).
pub const CHUNK_DATA_MAX: usize = 15;
const MSG_INFO: u8 = 0x41;

/// Unsolicited message types, skipped while awaiting a reply.
const ASYNC: [u8; 2] = [0x20, 0x21];

const EXCHANGE_TIMEOUT: Duration = Duration::from_millis(1000);

/// The device rejected a command (status 3) with this error code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Nak(pub u8);

impl fmt::Display for Nak {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let meaning = match self.0 {
            1 => "unknown message type",
            2 => "bad length/parameter",
            3 => "bad state or validation failure",
            4 => "bad token/session id",
            5 => "bad CRC or incomplete transfer",
            6 => "stream read/write failure",
            7 => "auth challenge mismatch or timeout",
            8 => "command retired",
            _ => "unmapped error",
        };
        write!(f, "device NAK {}: {meaning}", self.0)
    }
}

impl std::error::Error for Nak {}

/// Reply to [`Bnk8::hello`].
#[derive(Debug)]
pub struct Hello {
    /// FNV-style hash of the 12-char USB serial, computed on-device.
    pub serial_hash: u32,
    /// Internal patch version (25 on fw 6.0.25, 27 on fw 6.0.27).
    pub patch: u8,
}

/// Reply to [`Bnk8::status`].
#[derive(Debug)]
pub struct Status {
    /// Gate token for the `0x11` transfer session.
    pub token_a: u32,
    /// Second gate token for the `0x11` sub-4 RAM-only session.
    pub token_b: u32,
}

/// Reply to [`Bnk8::device_id`] (`0x15` sub 5).
#[derive(Debug)]
pub struct DeviceId {
    /// STM32 unique ID; matches the first 24 chars of the USB serial.
    pub uid: [u8; 12],
}

/// Reply to [`Bnk8::stream_status`] (`0x15` sub 0).
#[derive(Debug)]
pub struct StreamStatus {
    /// Stream state machine value; 2 = idle/ready.
    pub state: u8,
    /// Current stream token; 0 when no valid slot exists.
    pub token: u32,
    /// Declared stream data size of the active slot.
    pub size: u16,
}

impl Bnk8 {
    fn next_seq(&self) -> u8 {
        let s = self.seq.get().wrapping_add(1);
        self.seq.set(s);
        s
    }

    /// Sends one command frame and waits for its reply, skipping async
    /// frames and mismatched/stale ones. Maps NAK to [`Nak`].
    fn exchange(&self, msg: u8, payload: &[u8]) -> Result<Frame> {
        ensure!(
            payload.len() <= REPORT_LEN - 10,
            "payload {} bytes exceeds 22",
            payload.len()
        );
        let seq = self.next_seq();
        let mut buf = [0u8; REPORT_LEN];
        buf[..2].copy_from_slice(&MAGIC);
        buf[2] = 1;
        buf[4] = msg;
        buf[6] = seq;
        buf[7] = payload.len() as u8;
        buf[8..8 + payload.len()].copy_from_slice(payload);
        frame::seal(&mut buf);
        self.send(&buf)?;

        let deadline = Instant::now() + EXCHANGE_TIMEOUT;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                bail!("no reply to msg 0x{msg:02x} within {EXCHANGE_TIMEOUT:?}");
            }
            let ms = left.as_millis().clamp(1, i32::MAX as u128) as i32;
            let Some(raw) = self.recv(ms)? else { continue };
            frame::verify(&raw).context("reply failed frame verification")?;
            let f = Frame(raw);
            if ASYNC.contains(&f.msg_type()) {
                continue;
            }
            if f.msg_type() != msg || f.seq() != seq {
                continue; // stale or foreign frame
            }
            if f.status() == 3 {
                let code = f.payload().first().copied().unwrap_or(0);
                return Err(Nak(code).into());
            }
            return Ok(f);
        }
    }

    /// `0x01` hello: echoes 8 bytes, returns the serial hash and the
    /// internal patch version.
    pub fn hello(&self) -> Result<Hello> {
        let probe = [0x42, 0x4E, 0x4B, 0x38, 0x2D, 0x41, 0x49, 0x21]; // "BNK8-AI!"
        let f = self.exchange(MSG_HELLO, &probe)?;
        let p = f.payload();
        ensure!(p.len() == 22, "hello reply {} bytes, expected 22", p.len());
        ensure!(
            p[..8] == probe,
            "hello echo mismatch: {:02x?}",
            &p[..8]
        );
        Ok(Hello {
            serial_hash: u32::from_le_bytes(p[8..12].try_into().unwrap()),
            patch: p[15],
        })
    }

    /// `0x10` status: session tokens and config size.
    pub fn status(&self) -> Result<Status> {
        let f = self.exchange(MSG_STATUS, &[])?;
        let p = f.payload();
        ensure!(p.len() == 12, "status reply {} bytes, expected 12", p.len());
        let config_len = u16::from_le_bytes([p[4], p[5]]);
        ensure!(
            config_len as usize == CONFIG_LEN,
            "device reports config size {config_len}, expected {CONFIG_LEN}"
        );
        Ok(Status {
            token_a: u32::from_le_bytes(p[0..4].try_into().unwrap()),
            token_b: u32::from_le_bytes(p[6..10].try_into().unwrap()),
        })
    }

    /// `0x41` device info: identifier word `0x01VV0006` (VV = internal
    /// patch version) plus the same tokens as `0x10`. Returns VV.
    pub fn info(&self) -> Result<u8> {
        let f = self.exchange(MSG_INFO, &[])?;
        let p = f.payload();
        ensure!(p.len() == 16, "info reply {} bytes, expected 16", p.len());
        ensure!(
            p[0] == 0x06 && p[1] == 0x00 && p[3] == 0x01,
            "info identifier {:#010x?}, expected 06 00 VV 01",
            &p[..4]
        );
        Ok(p[2])
    }

    /// `0x15` sub 5: device ID — serial hash, MCU unique ID, stream max.
    pub fn device_id(&self) -> Result<DeviceId> {
        let f = self.exchange(MSG_STREAM, &[5])?;
        let p = f.payload();
        ensure!(p.len() == 22, "device-id reply {} bytes, expected 22", p.len());
        ensure!(p[0] == 3, "device-id reply tag {:#04x}, expected 0x03", p[0]);
        let stream_max = u16::from_le_bytes([p[19], p[20]]);
        ensure!(
            stream_max == STREAM_SIZE_MAX,
            "device reports stream max {stream_max:#06x}, expected {STREAM_SIZE_MAX:#06x}"
        );
        Ok(DeviceId {
            uid: p[5..17].try_into().unwrap(),
        })
    }

    /// `0x15` sub 0: stream session status.
    pub fn stream_status(&self) -> Result<StreamStatus> {
        let p = self.stream_status_payload()?;
        Ok(StreamStatus {
            state: p[1],
            token: u32::from_le_bytes(p[2..6].try_into().unwrap()),
            size: u16::from_le_bytes([p[8], p[9]]),
        })
    }

    /// Raw 22-byte `0x15` sub 0 status block, for reverse engineering.
    pub fn stream_status_payload(&self) -> Result<[u8; 22]> {
        let f = self.exchange(MSG_STREAM, &[0])?;
        let p = f.payload();
        ensure!(p.len() == 22, "stream status reply {} bytes, expected 22", p.len());
        Ok(p.try_into().unwrap())
    }

    /// `0x15` sub 2: begin a stream write. `token` is the current stream
    /// token (0 when no valid slot), `size` in 0x30..=0x1810, `crc` the
    /// CRC-32/ISO-HDLC of the 2000+size data bytes to be transferred.
    /// Moves the stream state machine 2 -> 3; the device then prepares
    /// flash and advances to state 4 on its own.
    pub fn stream_begin(&self, token: u32, size: u16, crc: u32) -> Result<()> {
        let mut p = [0u8; 11];
        p[0] = 2;
        p[1..5].copy_from_slice(&token.to_le_bytes());
        p[5..7].copy_from_slice(&size.to_le_bytes());
        p[7..11].copy_from_slice(&crc.to_le_bytes());
        self.exchange(MSG_STREAM, &p)?;
        Ok(())
    }

    /// `0x15` sub 8: declare the next 64-aligned block. `transferred`
    /// must equal the device's running total; `len` is the block length
    /// (non-zero, 64-aligned like `transferred`).
    pub fn stream_seek(&self, transferred: u16, len: u16) -> Result<()> {
        let mut p = [0u8; 5];
        p[0] = 8;
        p[1..3].copy_from_slice(&transferred.to_le_bytes());
        p[3..5].copy_from_slice(&len.to_le_bytes());
        self.exchange(MSG_STREAM, &p)?;
        Ok(())
    }

    /// `0x15` sub 3: append data to the staging buffer. `seq` must equal
    /// the device's running total of transferred bytes.
    pub fn stream_data(&self, seq: u16, data: &[u8]) -> Result<()> {
        ensure!(data.len() <= 16, "stream chunk {} bytes exceeds 16", data.len());
        let mut p = [0u8; 19];
        p[0] = 3;
        p[1..3].copy_from_slice(&seq.to_le_bytes());
        p[3..3 + data.len()].copy_from_slice(data);
        self.exchange(MSG_STREAM, &p[..3 + data.len()])?;
        Ok(())
    }

    /// `0x15` sub 4: finish/reset the stream session. From states 3..6
    /// returns the machine to state 2; from 7/8 performs a full reset.
    pub fn stream_finish(&self, token: u32) -> Result<()> {
        let mut p = [0u8; 5];
        p[0] = 4;
        p[1..5].copy_from_slice(&token.to_le_bytes());
        self.exchange(MSG_STREAM, &p)?;
        Ok(())
    }

    /// `0x15` sub 1: read `len` bytes (1..=0x16) at `offset` from the
    /// active stream slot. Requires a valid slot and matching token.
    pub fn stream_read(&self, token: u32, offset: u16, len: u8) -> Result<Vec<u8>> {
        let mut p = [0u8; 8];
        p[0] = 1;
        p[1..5].copy_from_slice(&token.to_le_bytes());
        p[5..7].copy_from_slice(&offset.to_le_bytes());
        p[7] = len;
        let f = self.exchange(MSG_STREAM, &p)?;
        Ok(f.payload().to_vec())
    }

    /// `0x11` sub 0: begin a flash-persist transfer session (empty buffer,
    /// all 2000 bytes must be supplied via sub-1 chunks). Requires the
    /// `0x10` token A only. `crc` is the expected CRC-32 of the final
    /// 2000-byte buffer. Returns the session id.
    pub fn transfer_begin_persist(&self, token_a: u32, crc: u32) -> Result<u32> {
        let mut p = [0u8; 11];
        p[0] = 0;
        p[1..5].copy_from_slice(&token_a.to_le_bytes());
        p[5..7].copy_from_slice(&(CONFIG_LEN as u16).to_le_bytes());
        p[7..11].copy_from_slice(&crc.to_le_bytes());
        let f = self.exchange(MSG_TRANSFER, &p)?;
        let r = f.payload();
        ensure!(r.len() == 4, "begin reply {} bytes, expected 4", r.len());
        Ok(u32::from_le_bytes(r.try_into().unwrap()))
    }

    /// `0x11` sub 1: write a chunk into the session buffer.
    pub fn transfer_chunk(&self, session: u32, offset: u16, data: &[u8]) -> Result<()> {
        ensure!(
            data.len() <= CHUNK_DATA_MAX,
            "transfer chunk {} bytes exceeds {CHUNK_DATA_MAX}",
            data.len()
        );
        let mut p = [0u8; 22];
        p[0] = 1;
        p[1..5].copy_from_slice(&session.to_le_bytes());
        p[5..7].copy_from_slice(&offset.to_le_bytes());
        p[7..7 + data.len()].copy_from_slice(data);
        self.exchange(MSG_TRANSFER, &p[..7 + data.len()])?;
        Ok(())
    }

    /// `0x11` sub 2: abort the session.
    pub fn transfer_abort(&self, session: u32) -> Result<()> {
        let mut p = [0u8; 5];
        p[0] = 2;
        p[1..5].copy_from_slice(&session.to_le_bytes());
        self.exchange(MSG_TRANSFER, &p)?;
        Ok(())
    }

    /// `0x12`: commit the session. For a sub-4 RAM-only session this
    /// applies the buffer to the active config in RAM (no flash write).
    /// Returns the 12-byte status reply payload.
    pub fn transfer_commit(&self, session: u32) -> Result<[u8; 12]> {
        let f = self.exchange(MSG_COMMIT, &session.to_le_bytes())?;
        let p = f.payload();
        ensure!(p.len() == 12, "commit reply {} bytes, expected 12", p.len());
        Ok(p.try_into().unwrap())
    }

    /// Persists a full config: `0x11` sub-0 session, 134 chunks, `0x12`
    /// commit. Clears a non-idle stream manager first (begins NAK 3
    /// otherwise). On success the device's token B equals the blob's CRC.
    pub fn write_config(&self, blob: &[u8; CONFIG_LEN]) -> Result<()> {
        let stream = self.stream_status()?;
        if stream.state != 2 {
            self.stream_finish(stream.token)
                .context("resetting the stream manager before a config write")?;
        }
        let crc = crc32(blob);
        let status = self.status()?;
        // The stream manager is idle here, so NAK 3 means another transfer session is
        // still open, e.g. from a write that was interrupted. Its id cannot be recovered
        // to abort it; a power cycle clears the session RAM.
        let session = self.transfer_begin_persist(status.token_a, crc).map_err(|e| match e.downcast_ref::<Nak>() {
            Some(Nak(3)) => e.context(
                "the device refused to start a write, likely because an earlier write was \
                 interrupted; unplug the BNK8, plug it back in, and try again",
            ),
            _ => e,
        })?;
        let upload = (|| {
            for (i, chunk) in blob.chunks(CHUNK_DATA_MAX).enumerate() {
                self.transfer_chunk(session, (i * CHUNK_DATA_MAX) as u16, chunk)?;
            }
            self.transfer_commit(session)
        })();
        let reply = match upload {
            Ok(r) => r,
            Err(e) => {
                let _ = self.transfer_abort(session);
                return Err(e.context("config rejected by the device"));
            }
        };
        let token_b = u32::from_le_bytes(reply[6..10].try_into().unwrap());
        ensure!(
            token_b == crc,
            "device reports config CRC {token_b:#010x} after commit, expected {crc:#010x}"
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nak_display_maps_known_codes() {
        assert_eq!(Nak(1).to_string(), "device NAK 1: unknown message type");
        assert_eq!(Nak(4).to_string(), "device NAK 4: bad token/session id");
        assert!(Nak(99).to_string().contains("unmapped"));
    }
}
