//! Local snapshot of the last config bnk8 wrote, per device serial.
//!
//! The device cannot read its config back (PROTOCOL.md "The read model"),
//! but `0x10` status token B is the CRC-32 of the active config. A snapshot
//! (or the factory blob) whose CRC equals token B is therefore exactly what
//! the device holds.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::config::{Blob, FACTORY};
use crate::protocol::crc32;
use crate::transport::Bnk8;

pub enum Source {
    Snapshot,
    Factory,
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
