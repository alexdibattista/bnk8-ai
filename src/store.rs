//! Identifying the device's active config, and the local snapshot of the
//! last config bnk8 wrote, per device serial.
//!
//! `0x10` status token B is the CRC-32 of the active config, so any blob
//! whose CRC equals token B is exactly what the device holds. The candidates,
//! cheapest first (PROTOCOL.md "The read model"): bnk8's local snapshot, the
//! factory config (of the current or an older firmware), and the copy of the
//! native config in the device's profile store, which Binepad Studio writes
//! when it saves.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::archive;
use crate::config::{Blob, FACTORY, OLDER_FACTORY, older_factory};
use crate::protocol::{CONFIG_LEN, Nak, PHASE_IDLE, STREAM_SIZE_MAX, crc32};
use crate::transport::Bnk8;

pub enum Source {
    Snapshot,
    Factory,
    /// The factory config of an older firmware version.
    OlderFactory(&'static str),
    /// Read back from the device's profile store (last saved by Binepad Studio),
    /// with Studio's layer names from its archive, one entry per layer.
    ProfileStore { layer_names: Vec<Option<String>> },
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

/// The snapshot's bytes; `None` when there is none yet. Other I/O errors
/// propagate, so an unreadable snapshot is not mistaken for an unknown config.
fn read_snapshot(p: &Path) -> Result<Option<Vec<u8>>> {
    match std::fs::read(p) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("reading the snapshot {}", p.display())),
    }
}

fn path(serial: &str) -> Result<PathBuf> {
    Ok(dir()?.join(format!("{serial}.bin")))
}

pub fn current(dev: &Bnk8) -> Result<Current> {
    let crc = dev.status()?.token_b;
    // Without a state directory there is no snapshot, but the factory check still applies.
    if let Ok(p) = path(dev.serial())
        && let Some(bytes) = read_snapshot(&p)?
        && let Ok(blob) = Blob::try_from(bytes.as_slice())
        && crc32(&blob) == crc
    {
        return Ok(Current::Known(Box::new(blob), Source::Snapshot));
    }
    if crc32(FACTORY) == crc {
        return Ok(Current::Known(Box::new(*FACTORY), Source::Factory));
    }
    if let Some((version, _, colors)) = OLDER_FACTORY.iter().find(|(_, c, _)| *c == crc) {
        return Ok(Current::Known(Box::new(older_factory(colors)), Source::OlderFactory(version)));
    }
    // Firmware without the profile store NAKs the status request.
    let stream = match dev.stream_status() {
        Ok(s) => s,
        Err(e) if e.downcast_ref::<Nak>().is_some() => return Ok(Current::Unknown { crc }),
        Err(e) => return Err(e),
    };
    if stream.phase == PHASE_IDLE
        && stream.revision != 0
        && stream.native_len as usize == CONFIG_LEN
        && stream.archive_len <= STREAM_SIZE_MAX
    {
        let payload = dev.profile_store_read(stream.revision, CONFIG_LEN + stream.archive_len as usize)?;
        let blob = Blob::try_from(&payload[..CONFIG_LEN]).unwrap();
        if crc32(&blob) == crc {
            // Token B already proves the native config; names are best effort.
            let mut layer_names = vec![None; blob[5] as usize]; // byte 5: layer count
            if crc32(&payload) == stream.payload_crc
                && let Ok(found) = archive::layer_names(&payload[CONFIG_LEN..])
            {
                for (i, name) in found {
                    if let Some(slot) = layer_names.get_mut(i) {
                        *slot = Some(name);
                    }
                }
            }
            // Not saved as a snapshot: the snapshot records bnk8's own writes.
            return Ok(Current::Known(Box::new(blob), Source::ProfileStore { layer_names }));
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
