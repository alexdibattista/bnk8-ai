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

/// Profile store phases (PROTOCOL.md `0x15`): the store is ready to read or save.
pub const PHASE_IDLE: u8 = 2;
/// The store accepts sub-3 data.
const PHASE_RECEIVE: u8 = 4;
/// Verify or Confirm failed; sub 4 rescans.
const PHASE_FAILED: u8 = 8;
/// The device is applying a newly saved slot.
const PHASE_ACTIVATING: u8 = 10;
/// Applying the slot failed; only a power cycle clears it.
const PHASE_ACTIVATION_FAILED: u8 = 11;

/// Store polling cadence and limits, as Binepad Studio uses them.
const STORE_POLL: Duration = Duration::from_millis(10);
const STORE_IDLE_TIMEOUT: Duration = Duration::from_secs(10);
const STORE_SAVE_TIMEOUT: Duration = Duration::from_secs(120);

/// Reply to [`Bnk8::stream_status`] (`0x15` sub 0): the portable profile
/// store's state (PROTOCOL.md `0x15`, docs/STUDIO.md §5).
#[derive(Debug)]
pub struct StreamStatus {
    /// Store phase, 0..=11; [`PHASE_IDLE`] = ready.
    pub phase: u8,
    /// Revision of the active slot; 0 when no valid slot exists.
    pub revision: u32,
    /// Length of the stored native config (2000 on the BNK8).
    pub native_len: u16,
    /// Length of the active slot's archive, stored after the native config.
    pub archive_len: u16,
    /// Archive capacity (`0x1810` on the BNK8).
    pub capacity: u16,
    /// Bytes received by the save in progress.
    pub received: u16,
    /// Archive length of the active slot when idle, of the save's target slot while saving.
    pub pending_archive_len: u16,
    /// CRC-32 of native config plus archive; same slot as `pending_archive_len`.
    pub payload_crc: u32,
}

impl StreamStatus {
    fn parse(p: &[u8]) -> Result<Self> {
        ensure!(p.len() == 22 && p[0] == 2, "stream status reply {:02x?} is not a 22-byte status", p);
        let u16_at = |i: usize| u16::from_le_bytes([p[i], p[i + 1]]);
        Ok(StreamStatus {
            phase: p[1],
            revision: u32::from_le_bytes(p[2..6].try_into().unwrap()),
            native_len: u16_at(6),
            archive_len: u16_at(8),
            capacity: u16_at(10),
            received: u16_at(12),
            pending_archive_len: u16_at(14),
            payload_crc: u32::from_le_bytes(p[16..20].try_into().unwrap()),
        })
    }
}

impl Bnk8 {
    fn next_seq(&self) -> u8 {
        let s = self.seq.get().wrapping_add(1);
        self.seq.set(s);
        s
    }

    /// Sends one command frame and waits for its reply (status 1, ACK),
    /// skipping async, request-shaped and mismatched/stale frames. Maps NAK
    /// (status 3) to [`Nak`].
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
            match f.status() {
                1 => return Ok(f),
                3 => return Err(Nak(f.payload().first().copied().unwrap_or(0)).into()),
                _ => continue, // not a reply
            }
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

    /// `0x15` sub 0: profile store status.
    pub fn stream_status(&self) -> Result<StreamStatus> {
        StreamStatus::parse(&self.stream_status_payload()?)
    }

    /// Raw 22-byte `0x15` sub 0 status block, for reverse engineering.
    pub fn stream_status_payload(&self) -> Result<[u8; 22]> {
        let f = self.exchange(MSG_STREAM, &[0])?;
        let p = f.payload();
        ensure!(p.len() == 22, "stream status reply {} bytes, expected 22", p.len());
        Ok(p.try_into().unwrap())
    }

    /// `0x15` sub 2: begin saving a new slot. `revision` is the active one (0
    /// when no valid slot), `archive_len` in 0x30..=0x1810, `crc` the CRC-32 of
    /// the 2000 + `archive_len` payload bytes. The store goes 2 -> 3 (erase) -> 4.
    fn store_begin(&self, revision: u32, archive_len: u16, crc: u32) -> Result<StreamStatus> {
        let mut p = [0u8; 11];
        p[0] = 2;
        p[1..5].copy_from_slice(&revision.to_le_bytes());
        p[5..7].copy_from_slice(&archive_len.to_le_bytes());
        p[7..11].copy_from_slice(&crc.to_le_bytes());
        StreamStatus::parse(self.exchange(MSG_STREAM, &p)?.payload())
    }

    /// `0x15` sub 3: payload bytes at `offset`, which must equal the received
    /// count; at most 16 bytes, not crossing a 64-byte boundary. NAK 3 while
    /// the staging buffer is being flushed.
    fn store_data(&self, offset: u16, data: &[u8]) -> Result<StreamStatus> {
        ensure!(data.len() <= 16, "store chunk {} bytes exceeds 16", data.len());
        let mut p = [0u8; 19];
        p[0] = 3;
        p[1..3].copy_from_slice(&offset.to_le_bytes());
        p[3..3 + data.len()].copy_from_slice(data);
        StreamStatus::parse(self.exchange(MSG_STREAM, &p[..3 + data.len()])?.payload())
    }

    /// `0x15` sub 4 with the active `revision`: cancels a save in phases 3..6;
    /// in 7/8 clears the store's RAM state and rescans flash (never erases).
    pub fn store_cancel(&self, revision: u32) -> Result<()> {
        let mut p = [0u8; 5];
        p[0] = 4;
        p[1..5].copy_from_slice(&revision.to_le_bytes());
        self.exchange(MSG_STREAM, &p)?;
        Ok(())
    }

    /// Waits until the profile store is idle, as Binepad Studio does: one
    /// sub-4 rescan from Failed, an error on ActivationFailed.
    pub fn store_wait_idle(&self) -> Result<StreamStatus> {
        let deadline = Instant::now() + STORE_IDLE_TIMEOUT;
        let mut rescanned = false;
        loop {
            let s = self.stream_status()?;
            match s.phase {
                PHASE_IDLE => return Ok(s),
                PHASE_FAILED if !rescanned => {
                    rescanned = true;
                    self.store_cancel(s.revision)?;
                }
                PHASE_FAILED => bail!(
                    "the device's profile store failed and did not recover; unplug the BNK8 and plug it back in"
                ),
                PHASE_ACTIVATION_FAILED => bail!(
                    "the device could not apply its saved profile; unplug the BNK8 and plug it back in"
                ),
                _ => {}
            }
            ensure!(
                Instant::now() < deadline,
                "the device's profile store stayed busy (phase {}) for {STORE_IDLE_TIMEOUT:?}; \
                 close Binepad Studio and try again",
                s.phase
            );
            std::thread::sleep(STORE_POLL);
        }
    }

    /// Saves `payload` (2000-byte native config, then the archive) as the
    /// profile store's next slot over active `revision`, and waits until the
    /// device has applied it. Then checks token B and reads the slot back.
    /// `on_status` sees every store status during the save.
    pub fn store_save(&self, revision: u32, payload: &[u8], on_status: &mut dyn FnMut(&StreamStatus)) -> Result<()> {
        let archive_len = payload.len().saturating_sub(CONFIG_LEN);
        ensure!(
            (0x30..=STREAM_SIZE_MAX as usize).contains(&archive_len),
            "profile archive is {archive_len} bytes, expected 48..={STREAM_SIZE_MAX}"
        );
        let archive_len = archive_len as u16;
        // The firmware refuses revision 0xFFFFFFFF; the new slot is `revision + 1`.
        ensure!(revision < u32::MAX, "the profile store is at its last revision {revision}");
        let crc = crc32(payload);
        let idle = self.store_wait_idle()?;
        ensure!(
            idle.revision == revision,
            "the device's profile changed (revision {}, expected {revision}); read it again",
            idle.revision
        );
        if let Err(e) = self.store_transfer(revision, payload, archive_len, crc, on_status) {
            // Studio's cleanup: cancel our own save if it is still open.
            if let Ok(s) = self.stream_status()
                && s.phase != PHASE_IDLE
                && s.revision == revision
                && s.pending_archive_len == archive_len
                && s.payload_crc == crc
            {
                let _ = self.store_cancel(revision);
            }
            let outcome = match self.store_wait_idle() {
                Ok(s) if s.revision == revision => "nothing was saved".to_string(),
                Ok(s) => format!("the device's profile is now revision {}; read it again", s.revision),
                Err(e) => format!("{e:#}"),
            };
            return Err(e.context(format!("saving to the profile store failed: {outcome}")));
        }
        let native_crc = crc32(&payload[..CONFIG_LEN]);
        let token_b = self.status()?.token_b;
        ensure!(
            token_b == native_crc,
            "device reports config CRC {token_b:#010x} after the save, expected {native_crc:#010x}"
        );
        let back = self.profile_store_read(revision + 1, payload.len())?;
        ensure!(back == payload, "the saved profile reads back differently from what was sent");
        Ok(())
    }

    /// Begins the save, streams the payload and follows the store's phases
    /// to Idle at `revision + 1` (Studio's `UsbPortableStoreTransfer.WriteAsync`).
    fn store_transfer(
        &self,
        revision: u32,
        payload: &[u8],
        archive_len: u16,
        crc: u32,
        on_status: &mut dyn FnMut(&StreamStatus),
    ) -> Result<()> {
        let total = payload.len();
        let deadline = Instant::now() + STORE_SAVE_TIMEOUT;
        let mut s = self.store_begin(revision, archive_len, crc)?;
        loop {
            on_status(&s);
            match s.phase {
                PHASE_IDLE => {
                    ensure!(
                        s.revision == revision + 1 && s.pending_archive_len == archive_len && s.payload_crc == crc,
                        "the save was not confirmed (store at revision {})",
                        s.revision
                    );
                    return Ok(());
                }
                PHASE_FAILED => bail!("the device could not verify the saved profile"),
                PHASE_ACTIVATION_FAILED => {
                    bail!("the device could not apply the saved profile; unplug the BNK8 and plug it back in")
                }
                // Activation reports the new revision; its slot fields are checked once idle.
                PHASE_ACTIVATING => ensure!(s.revision == revision + 1, "a different profile save is in progress"),
                _ => ensure!(
                    s.revision == revision && s.pending_archive_len == archive_len && s.payload_crc == crc,
                    "a different profile save is in progress"
                ),
            }
            ensure!(
                Instant::now() < deadline,
                "the save did not finish within {STORE_SAVE_TIMEOUT:?} (phase {}); don't touch the keypad while saving",
                s.phase
            );
            let received = s.received as usize;
            if s.phase == PHASE_RECEIVE && received < total {
                let n = 16.min(total - received).min(64 - received % 64);
                match self.store_data(received as u16, &payload[received..received + n]) {
                    Ok(next) => {
                        s = next;
                        continue;
                    }
                    // Staging busy (3) or offset out of step (4): resync from status.
                    Err(e) if matches!(e.downcast_ref::<Nak>(), Some(Nak(3 | 4))) => {}
                    Err(e) => return Err(e),
                }
            }
            std::thread::sleep(STORE_POLL);
            s = self.stream_status()?;
        }
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

    /// Reads the first `len` bytes (native config, then archive) of the
    /// profile store's slot `revision` with repeated `0x15` sub-1 reads.
    /// `len` is at most the stream area, `CONFIG_LEN + STREAM_SIZE_MAX`.
    pub fn profile_store_read(&self, revision: u32, len: usize) -> Result<Vec<u8>> {
        let max = CONFIG_LEN + STREAM_SIZE_MAX as usize;
        ensure!(len <= max, "profile store read of {len} bytes exceeds the {max}-byte stream area");
        let mut out = Vec::with_capacity(len);
        while out.len() < len {
            let offset = out.len();
            let n = (len - offset).min(22);
            let chunk = self.stream_read(revision, offset as u16, n as u8)?;
            ensure!(
                chunk.len() == n,
                "profile store read at {offset}: {} bytes, expected {n}",
                chunk.len()
            );
            out.extend_from_slice(&chunk);
        }
        Ok(out)
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
    /// commit. Waits for the profile store to be idle first (begins NAK 3
    /// otherwise; cancelling would abort a Binepad Studio save in progress).
    /// Refuses while the store holds a valid slot: the firmware then NAKs
    /// sub-0 begins, so such devices are written with [`Bnk8::store_save`].
    /// On success the device's token B equals the blob's CRC. Fails without
    /// writing unless the device still holds the config with CRC `expected`.
    pub fn write_config(&self, blob: &[u8; CONFIG_LEN], expected: u32) -> Result<()> {
        let stream = self.store_wait_idle()?;
        ensure!(
            stream.revision == 0,
            "this BNK8 has a profile store slot (revision {}), so the firmware refuses `0x11` \
             config writes; save through the profile store instead",
            stream.revision
        );
        let crc = crc32(blob);
        let status = self.status()?;
        ensure!(
            status.token_b == expected,
            "the device's config changed since the changes were worked out; read it again"
        );
        // With the stream manager idle and no profile-store slot, NAK 3 means another
        // transfer session is still open. The firmware drops it after 2.5 s without traffic.
        let session = self.transfer_begin_persist(status.token_a, crc).map_err(|e| match e.downcast_ref::<Nak>() {
            Some(Nak(3)) => e.context(
                "the device refused to start a write because another write session is open; \
                 try again in a few seconds",
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
