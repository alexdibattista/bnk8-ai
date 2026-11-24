//! Binepad Studio's profile archive, stored on the device after the native
//! config (PROTOCOL.md `0x15`): a 48-byte `BPP1` header — magic, version 1
//! (u32), JSON length (i32), compressed length (i32), SHA-256 of the JSON —
//! then the Brotli-compressed JSON `{format: 1, deviceId, profile}`.
//!
//! The firmware checks only the archive's length; Studio decodes it, and
//! treats `profile` as the source of truth for the device's layout.

use std::io::{Read, Write};

use anyhow::{Context, Result, bail, ensure};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use crate::config::DEVICE;
use crate::protocol::STREAM_SIZE_MAX;

const HEADER_LEN: usize = 48;
const MAX_JSON: usize = 1 << 20;

/// A decoded archive: the device's portable identity and Studio's `DeviceProfile` JSON.
#[derive(Debug, Clone, PartialEq)]
pub struct Document {
    pub device_id: String,
    pub profile: Value,
}

fn le_u32(b: &[u8], off: usize) -> u32 {
    u32::from_le_bytes(b[off..off + 4].try_into().unwrap())
}

/// Decodes and verifies a `BPP1` archive (Studio writes no other kind for the BNK8).
pub fn decode(archive: &[u8]) -> Result<Document> {
    ensure!(archive.len() >= HEADER_LEN, "archive is {} bytes, shorter than its header", archive.len());
    match &archive[..4] {
        b"BPP1" => {}
        b"BPP2" => bail!("delta archives (BPP2) are not supported"),
        m => bail!("archive magic {m:02x?}, expected BPP1"),
    }
    let version = le_u32(archive, 4);
    ensure!(version == 1, "archive version {version}, expected 1");
    let json_len = le_u32(archive, 8) as i32;
    ensure!((1..=MAX_JSON as i32).contains(&json_len), "archive JSON length {json_len} out of range");
    let packed_len = le_u32(archive, 12) as i32;
    let body = &archive[HEADER_LEN..];
    ensure!(
        usize::try_from(packed_len).is_ok_and(|n| n == body.len()),
        "archive compressed length {packed_len}, but {} bytes follow the header",
        body.len()
    );

    let mut json = Vec::with_capacity(json_len as usize);
    brotli::Decompressor::new(body, 4096)
        .take(json_len as u64 + 1)
        .read_to_end(&mut json)
        .context("decompressing the profile archive")?;
    ensure!(json.len() == json_len as usize, "archive JSON is {} bytes, header says {json_len}", json.len());
    ensure!(Sha256::digest(&json)[..] == archive[16..HEADER_LEN], "profile archive fails its SHA-256 check");

    let mut doc: Map<String, Value> = serde_json::from_slice(&json).context("parsing the profile archive")?;
    let format = doc.get("format").and_then(Value::as_u64);
    ensure!(format == Some(1), "profile archive format {format:?}, expected 1");
    let device_id = doc.get("deviceId").and_then(Value::as_str).context("profile archive has no deviceId")?.to_string();
    let profile = doc.remove("profile").context("profile archive has no profile")?;
    let device = profile["devicePackId"].as_str();
    ensure!(device == Some(DEVICE), "profile is for {device:?}, expected {DEVICE}");
    Ok(Document { device_id, profile })
}

/// Encodes a `BPP1` archive; errors when it exceeds the device's archive capacity.
pub fn encode(doc: &Document) -> Result<Vec<u8>> {
    let json = serde_json::to_vec(&json!({"format": 1, "deviceId": doc.device_id, "profile": doc.profile}))?;
    ensure!(json.len() <= MAX_JSON, "profile JSON is {} bytes, over {MAX_JSON}", json.len());
    let mut out = vec![0u8; HEADER_LEN];
    {
        let mut w = brotli::CompressorWriter::new(&mut out, 4096, 11, 22);
        w.write_all(&json)?;
    }
    let packed = out.len() - HEADER_LEN;
    ensure!(
        out.len() <= STREAM_SIZE_MAX as usize,
        "profile archive is {} bytes; the device stores at most {STREAM_SIZE_MAX}",
        out.len()
    );
    out[..4].copy_from_slice(b"BPP1");
    out[4..8].copy_from_slice(&1u32.to_le_bytes());
    out[8..12].copy_from_slice(&(json.len() as i32).to_le_bytes());
    out[12..16].copy_from_slice(&(packed as i32).to_le_bytes());
    out[16..HEADER_LEN].copy_from_slice(&Sha256::digest(&json));
    Ok(out)
}

/// The named layers of a profile as `(index, name)`.
pub fn layer_names(profile: &Value) -> Vec<(usize, String)> {
    profile["layers"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|l| {
            let index = usize::try_from(l["index"].as_u64()?).ok()?;
            let name = l["name"].as_str().filter(|n| !n.is_empty())?;
            Some((index, name.to_string()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(device: &str) -> Document {
        Document {
            device_id: "00000000-0000-0000-0000-000000000001".into(),
            profile: json!({"devicePackId": device, "layers": [{"index": 0, "name": "Codex"}, {"index": 1, "name": ""}]}),
        }
    }

    #[test]
    fn round_trips_and_reads_named_layers() {
        let d = doc(DEVICE);
        let back = decode(&encode(&d).unwrap()).unwrap();
        assert_eq!(back, d);
        assert_eq!(layer_names(&back.profile), [(0, "Codex".to_string())]);
    }

    #[test]
    fn rejects_tampering_delta_and_other_devices() {
        let mut a = encode(&doc(DEVICE)).unwrap();
        a[20] ^= 1; // SHA-256 byte
        assert!(decode(&a).unwrap_err().to_string().contains("SHA-256"));
        let mut a = encode(&doc(DEVICE)).unwrap();
        a[..4].copy_from_slice(b"BPP2");
        assert!(decode(&a).unwrap_err().to_string().contains("BPP2"));
        let e = decode(&encode(&doc("binepad.bnk9")).unwrap()).unwrap_err().to_string();
        assert!(e.contains("binepad.bnk9"), "{e}");
    }

    #[test]
    fn refuses_archives_over_capacity() {
        // Incompressible names push the archive past the device's capacity.
        let names: Vec<Value> = (0..1000u64)
            .map(|i| json!({"index": i, "name": format!("{:016x}", i.wrapping_mul(0x9E37_79B9_7F4A_7C15))}))
            .collect();
        let d = Document { device_id: "x".into(), profile: json!({"devicePackId": DEVICE, "layers": names}) };
        assert!(encode(&d).unwrap_err().to_string().contains("at most"));
    }
}
