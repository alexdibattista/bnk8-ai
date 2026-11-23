//! Binepad Studio's profile archive, stored on the device after the native
//! config (PROTOCOL.md `0x15`): a 48-byte `BPP1` header — magic, version 1
//! (u32), JSON length (i32), compressed length (i32), SHA-256 of the JSON —
//! then the Brotli-compressed JSON `{format: 1, deviceId, profile}`.

use std::io::Read;

use anyhow::{Context, Result, bail, ensure};
use serde_json::Value;

use crate::config::DEVICE;

const HEADER_LEN: usize = 48;
const MAX_JSON: usize = 1 << 20;

fn le_u32(b: &[u8], off: usize) -> u32 {
    u32::from_le_bytes(b[off..off + 4].try_into().unwrap())
}

/// The named layers of a `BPP1` archive as `(index, name)`. The SHA-256 is
/// not checked: callers match the store's payload CRC first.
pub fn layer_names(archive: &[u8]) -> Result<Vec<(usize, String)>> {
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
    brotli_decompressor::Decompressor::new(body, 4096)
        .take(json_len as u64 + 1)
        .read_to_end(&mut json)
        .context("decompressing the profile archive")?;
    ensure!(json.len() == json_len as usize, "archive JSON is {} bytes, header says {json_len}", json.len());

    let doc: Value = serde_json::from_slice(&json).context("parsing the profile archive")?;
    let format = doc["format"].as_u64();
    ensure!(format == Some(1), "profile archive format {format:?}, expected 1");
    let profile = &doc["profile"];
    let device = profile["devicePackId"].as_str();
    ensure!(device == Some(DEVICE), "profile is for {device:?}, expected {DEVICE}");
    let layers = profile["layers"].as_array().context("profile has no layers")?;
    Ok(layers
        .iter()
        .filter_map(|l| {
            let index = usize::try_from(l["index"].as_u64()?).ok()?;
            let name = l["name"].as_str().filter(|n| !n.is_empty())?;
            Some((index, name.to_string()))
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    fn archive(magic: &[u8; 4], device: &str) -> Vec<u8> {
        let json = format!(
            r#"{{"format":1,"deviceId":"00000000-0000-0000-0000-000000000001","profile":{{"devicePackId":"{device}","layers":[{{"index":0,"name":"Codex"}},{{"index":1,"name":""}}]}}}}"#
        );
        let mut packed = Vec::new();
        {
            let mut w = brotli::CompressorWriter::new(&mut packed, 4096, 9, 22);
            w.write_all(json.as_bytes()).unwrap();
        }
        let mut out = magic.to_vec();
        out.extend(1u32.to_le_bytes());
        out.extend((json.len() as i32).to_le_bytes());
        out.extend((packed.len() as i32).to_le_bytes());
        out.extend([0u8; 32]);
        out.extend(packed);
        out
    }

    #[test]
    fn reads_named_layers() {
        assert_eq!(layer_names(&archive(b"BPP1", DEVICE)).unwrap(), [(0, "Codex".to_string())]);
    }

    #[test]
    fn rejects_delta_and_other_devices() {
        let e = layer_names(&archive(b"BPP2", DEVICE)).unwrap_err().to_string();
        assert!(e.contains("BPP2"), "{e}");
        let e = layer_names(&archive(b"BPP1", "binepad.bnk9")).unwrap_err().to_string();
        assert!(e.contains("binepad.bnk9"), "{e}");
    }
}
