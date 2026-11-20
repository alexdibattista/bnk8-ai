//! BNK8 AI Edition config-channel frame codec.
//!
//! The only place the magic, header offsets and CRC are defined.
//!
//! Bytes `0..2` are [`MAGIC`]; bytes `30..32` are CRC-16/CCITT-FALSE over
//! bytes `0..30`, stored big-endian. Header offsets confirmed against
//! firmware 6.0.27 (`PROTOCOL.md`): `[2]` version, `[3]` reserved, `[4]`
//! msg_type, `[5]` status (0 request/async, 1 ACK, 3 NAK), `[6]` seq echo,
//! `[7]` payload length.

use std::fmt;

pub const REPORT_LEN: usize = 32;
pub const MAGIC: [u8; 2] = [0xB1, 0x5D];

/// Bytes covered by the CRC.
const CRC_SPAN: usize = REPORT_LEN - 2;
/// Start of the payload area.
const PAYLOAD_OFFSET: usize = 8;

/// CRC-16/CCITT-FALSE (poly 0x1021, init 0xFFFF, no reflection, xorout 0).
/// `CRC_16_IBM_3740` is the catalogue name for the same parameters.
const CCITT_FALSE: crc::Crc<u16> = crc::Crc::<u16>::new(&crc::CRC_16_IBM_3740);

pub fn crc(bytes: &[u8]) -> u16 {
    CCITT_FALSE.checksum(bytes)
}

/// Writes the CRC of bytes `0..30` big-endian into bytes `30..32`.
pub fn seal(buf: &mut [u8; REPORT_LEN]) {
    let sum = crc(&buf[..CRC_SPAN]);
    buf[CRC_SPAN..].copy_from_slice(&sum.to_be_bytes());
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameError {
    BadMagic([u8; 2]),
    BadCrc { expected: u16, got: u16 },
}

impl fmt::Display for FrameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FrameError::BadMagic([a, b]) => {
                write!(f, "bad magic {a:02x}{b:02x}, expected b15d")
            }
            FrameError::BadCrc { expected, got } => {
                write!(f, "bad CRC: computed {expected:04x}, frame has {got:04x}")
            }
        }
    }
}

impl std::error::Error for FrameError {}

/// Checks magic and CRC.
pub fn verify(buf: &[u8; REPORT_LEN]) -> Result<(), FrameError> {
    if buf[..2] != MAGIC {
        return Err(FrameError::BadMagic([buf[0], buf[1]]));
    }
    let expected = crc(&buf[..CRC_SPAN]);
    let got = u16::from_be_bytes([buf[CRC_SPAN], buf[CRC_SPAN + 1]]);
    if expected != got {
        return Err(FrameError::BadCrc { expected, got });
    }
    Ok(())
}

/// A 32-byte report with header accessors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Frame(pub [u8; REPORT_LEN]);

impl Frame {
    /// Protocol version, byte 2 (always 1).
    pub fn version(&self) -> u8 {
        self.0[2]
    }

    /// Reserved, byte 3 (always 0).
    pub fn flags(&self) -> u8 {
        self.0[3]
    }

    /// Message type, byte 4. Confirmed in firmware (dispatcher switch).
    pub fn msg_type(&self) -> u8 {
        self.0[4]
    }

    /// Status, byte 5: 0 = request/async frame, 1 = ACK, 3 = NAK.
    /// Confirmed in firmware (reply builder sets 1, NAK path ORs 2).
    pub fn status(&self) -> u8 {
        self.0[5]
    }

    /// Sequence number, byte 6, echoed from the request.
    /// Confirmed in firmware (dispatcher reads byte 6, reply echoes it).
    pub fn seq(&self) -> u8 {
        self.0[6]
    }

    /// Payload length, byte 7.
    pub fn payload_len(&self) -> u8 {
        self.0[7]
    }

    /// Payload, bytes 8.., truncated to `payload_len()` and the CRC boundary.
    pub fn payload(&self) -> &[u8] {
        let end = (PAYLOAD_OFFSET + self.payload_len() as usize).min(CRC_SPAN);
        &self.0[PAYLOAD_OFFSET..end]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reply observed 2026-11-16 to the VIA `get_protocol_version` report.
    const OBSERVED: [u8; REPORT_LEN] = [
        0xb1, 0x5d, 0x01, 0x00, 0x21, 0x00, 0x00, 0x02, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0xd2, 0xd4,
    ];

    #[test]
    fn verify_accepts_observed_reply() {
        assert_eq!(verify(&OBSERVED), Ok(()));
    }

    #[test]
    fn flipping_any_crc_covered_byte_fails() {
        // Bytes 0..2 are the magic, so a flip there is reported as BadMagic first.
        for i in 2..CRC_SPAN {
            let mut buf = OBSERVED;
            buf[i] ^= 0xff;
            assert!(
                matches!(verify(&buf), Err(FrameError::BadCrc { got: 0xd2d4, .. })),
                "byte {i}"
            );
        }
        for i in 0..2 {
            let mut buf = OBSERVED;
            buf[i] ^= 0xff;
            assert!(
                matches!(verify(&buf), Err(FrameError::BadMagic(_))),
                "byte {i}"
            );
            // The CRC itself still covers the magic.
            assert_ne!(crc(&buf[..CRC_SPAN]), 0xd2d4, "byte {i}");
        }
    }

    #[test]
    fn seal_restores_crc() {
        let mut buf = OBSERVED;
        buf[30] = 0;
        buf[31] = 0;
        seal(&mut buf);
        assert_eq!(buf, OBSERVED);
    }

    #[test]
    fn header_fields_of_observed_reply() {
        let f = Frame(OBSERVED);
        assert_eq!(f.version(), 1);
        assert_eq!(f.flags(), 0);
        assert_eq!(f.msg_type(), 0x21);
        assert_eq!(f.seq(), 0);
        assert_eq!(f.payload(), &[0x00, 0x01]);
    }

    #[test]
    fn payload_is_clamped_to_crc_boundary() {
        let mut buf = OBSERVED;
        buf[7] = 0xff;
        assert_eq!(Frame(buf).payload().len(), CRC_SPAN - PAYLOAD_OFFSET);
    }
}
