//! Identifying the device's active config, and the local snapshot of the
//! last config bnk8 wrote, per device serial.
//!
//! `0x10` status token B is the CRC-32 of the active config, so any blob
//! whose CRC equals token B is exactly what the device holds. There are three
//! candidates, cheapest first (PROTOCOL.md "The read model"): bnk8's local
//! snapshot, the factory config, and the copy of the native config in the
//! device's profile store, which Binepad Studio writes when it saves.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::config::{Blob, FACTORY};
use crate::protocol::{CONFIG_LEN, Nak, PHASE_IDLE, crc32};
use crate::transport::Bnk8;

pub enum Source {
    Snapshot,
    Factory,
    /// Read back from the device's profile store (last saved by Binepad Studio).
    ProfileStore,
}

/// The device's current config, if bnk8 can identify it.
pub enum Current {
    Known(Box<Blob>, Source),
    Unknown { crc: u32 },
}

fn dir() -> Result<PathBuf> {
    if let Some(d) = std::env::var_os("XDG_STATE_HOME").filter(|d| !d.is_empty()) {
        return Ok(PathBuf::from(d).join("bnk8"));
    }
    let home = std::env::var_os("HOME").context("HOME is not set")?;
    Ok(PathBuf::from(home).join(".local/state/bnk8"))
}

fn path(serial: &str) -> Result<PathBuf> {
    Ok(dir()?.join(format!("{serial}.bin")))
}

pub fn current(dev: &Bnk8) -> Result<Current> {
    let crc = dev.status()?.token_b;
    // Without a state directory there is no snapshot, but the factory check still applies.
    if let Ok(p) = path(dev.serial())
        && let Ok(bytes) = std::fs::read(&p)
        && let Ok(blob) = Blob::try_from(bytes.as_slice())
        && crc32(&blob) == crc
    {
        return Ok(Current::Known(Box::new(blob), Source::Snapshot));
    }
    if crc32(FACTORY) == crc {
        return Ok(Current::Known(Box::new(*FACTORY), Source::Factory));
    }
    // Firmware without the profile store NAKs the status request.
    let stream = match dev.stream_status() {
        Ok(s) => s,
        Err(e) if e.downcast_ref::<Nak>().is_some() => return Ok(Current::Unknown { crc }),
        Err(e) => return Err(e),
    };
    if stream.phase == PHASE_IDLE && stream.revision != 0 && stream.native_len as usize == CONFIG_LEN {
        let bytes = dev.profile_store_read(stream.revision, CONFIG_LEN)?;
        let blob = Blob::try_from(bytes.as_slice()).unwrap();
        if crc32(&blob) == crc {
            // Not saved as a snapshot: the snapshot records bnk8's own writes.
            return Ok(Current::Known(Box::new(blob), Source::ProfileStore));
        }
    }
    Ok(Current::Unknown { crc })
}

fn save(p: &Path, blob: &Blob) -> Result<()> {
    let tmp = p.with_extension("tmp");
    std::fs::write(&tmp, blob).with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, p).with_context(|| format!("replacing {}", p.display()))
}

/// Persists `blob` to the device and records it as the snapshot. The snapshot
/// directory is prepared first, so an unusable one fails before the device is touched.
pub fn write(dev: &Bnk8, blob: &Blob) -> Result<()> {
    let p = path(dev.serial())?;
    let d = p.parent().unwrap();
    std::fs::create_dir_all(d).with_context(|| format!("creating {}", d.display()))?;
    dev.write_config(blob)?;
    save(&p, blob).context(
        "the device was updated, but its local snapshot was not saved: \
         bnk8 will report the config as unknown until the next apply or reset",
    )
}
