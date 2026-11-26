//! Identifying the device's active config, writing new configs, and the
//! local snapshot of the last config bnk8 wrote, per device serial.
//!
//! `0x10` status token B is the CRC-32 of the active config, so any blob
//! whose CRC equals token B is exactly what the device holds. The candidates
//! (PROTOCOL.md "The read model"): the device's profile store, which Binepad
//! Studio (and bnk8) save through; bnk8's local snapshot; and the factory
//! config (of the current or an older firmware).
//!
//! Writes go through the profile store when the device has a slot there (the
//! firmware then refuses `0x11` writes), together with Studio's profile JSON,
//! updated so Studio and its agent see the same config. Devices without a
//! slot are written with `0x11`.

use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail, ensure};

use crate::archive::{self, Document};
use crate::config::{self, Blob, Changes, Config, FACTORY, Names, OLDER_FACTORY, StudioIds, older_factory};
use crate::profile;
use crate::protocol::{CONFIG_LEN, Nak, PHASE_IDLE, STREAM_SIZE_MAX, crc32};
use crate::transport::Bnk8;

pub enum Source {
    Snapshot,
    Factory,
    /// The factory config of an older firmware version.
    OlderFactory(&'static str),
    /// Read back from the device's profile store (saved by Binepad Studio or
    /// bnk8), with the layer and macro names from its profile.
    ProfileStore { names: Names },
}

/// The device's current config, if bnk8 can identify it.
pub enum Current {
    Known(Box<Blob>, Source),
    Unknown { crc: u32 },
}

/// The state directory: `$XDG_STATE_HOME/bnk8`, else `~/.local/state/bnk8`.
/// A relative `XDG_STATE_HOME` is ignored, as the XDG spec says; `HOME` must be absolute.
fn dir() -> Result<PathBuf> {
    if let Some(d) = std::env::var_os("XDG_STATE_HOME").map(PathBuf::from).filter(|d| d.is_absolute()) {
        return Ok(d.join("bnk8"));
    }
    let home = std::env::var_os("HOME").map(PathBuf::from).filter(|h| h.is_absolute());
    Ok(home.context("HOME is not set to an absolute path")?.join(".local/state/bnk8"))
}

/// Creates the state directory `d`, owner-only, if missing. Refuses one that
/// isn't a real directory owned by this user, or that others can write to,
/// since anyone who can write there can choose what bnk8 believes the device holds.
fn make_dir(d: &Path) -> Result<()> {
    let mut b = std::fs::DirBuilder::new();
    b.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut b, 0o700);
    b.create(d).with_context(|| format!("creating {}", d.display()))?;
    let m = std::fs::symlink_metadata(d).with_context(|| format!("checking {}", d.display()))?;
    ensure!(m.is_dir(), "{} is not a directory", d.display());
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        // SAFETY: geteuid has no preconditions and cannot fail.
        let uid = unsafe { libc::geteuid() };
        ensure!(m.uid() == uid, "{} belongs to another user", d.display());
        ensure!(m.mode() & 0o022 == 0, "{} is writable by other users", d.display());
    }
    Ok(())
}

/// Opens snapshot files without following a symlink at `p` (or blocking on a FIFO).
fn open_options(o: &mut OpenOptions) -> &mut OpenOptions {
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::custom_flags(o, libc::O_NOFOLLOW | libc::O_NONBLOCK);
    o
}

/// The snapshot; `None` when there is none yet or it isn't a config's size.
/// Other I/O errors, a symlink or a non-file propagate, so an unreadable
/// snapshot is not mistaken for an unknown config.
fn read_snapshot(p: &Path) -> Result<Option<Blob>> {
    let mut f = match open_options(OpenOptions::new().read(true)).open(p) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("opening the snapshot {}", p.display())),
    };
    let m = f.metadata().with_context(|| format!("checking the snapshot {}", p.display()))?;
    ensure!(m.is_file(), "the snapshot {} is not a regular file", p.display());
    if m.len() != CONFIG_LEN as u64 {
        return Ok(None);
    }
    let mut blob = [0; CONFIG_LEN];
    f.read_exact(&mut blob).with_context(|| format!("reading the snapshot {}", p.display()))?;
    Ok(Some(blob))
}

/// The snapshot file of the device with USB serial `serial`, which must be a
/// [`valid_serial`](crate::transport::valid_serial): it comes from the device.
fn path(serial: &str) -> Result<PathBuf> {
    ensure!(crate::transport::valid_serial(serial), "invalid USB serial number {serial:?}");
    Ok(dir()?.join(format!("{serial}.bin")))
}

/// The profile store's active slot.
struct Slot {
    revision: u32,
    /// Native config, then the archive.
    payload: Vec<u8>,
}

impl Slot {
    fn native(&self) -> Blob {
        self.payload[..CONFIG_LEN].try_into().unwrap()
    }

    fn archive(&self) -> &[u8] {
        &self.payload[CONFIG_LEN..]
    }
}

/// The profile store's active slot; `None` when there is none, or on firmware
/// without a profile store (it NAKs the status request).
fn read_slot(dev: &Bnk8) -> Result<Option<Slot>> {
    let s = match dev.stream_status() {
        Ok(s) if s.phase == PHASE_IDLE => s,
        Ok(_) => dev.store_wait_idle()?,
        Err(e) if e.downcast_ref::<Nak>().is_some() => return Ok(None),
        Err(e) => return Err(e),
    };
    if s.revision == 0 {
        return Ok(None);
    }
    ensure!(
        s.native_len as usize == CONFIG_LEN && s.archive_len <= STREAM_SIZE_MAX,
        "the profile store holds {} + {} bytes, not a BNK8 profile",
        s.native_len,
        s.archive_len
    );
    let payload = dev.profile_store_read(s.revision, CONFIG_LEN + s.archive_len as usize)?;
    ensure!(crc32(&payload) == s.payload_crc, "the profile store's contents fail their CRC check");
    Ok(Some(Slot { revision: s.revision, payload }))
}

pub fn current(dev: &Bnk8) -> Result<Current> {
    let crc = dev.status()?.token_b;
    if let Some(slot) = read_slot(dev)? {
        let native = slot.native();
        if crc32(&native) == crc {
            // Token B already proves the native config; names are best effort.
            let names = archive::decode(slot.archive()).map(|doc| profile::names(&doc.profile)).unwrap_or_default();
            return Ok(Current::Known(Box::new(native), Source::ProfileStore { names }));
        }
    }
    without_slot(dev, crc)
}

/// The config with CRC `crc` on a device whose profile store doesn't hold it:
/// bnk8's snapshot, or a factory config.
fn without_slot(dev: &Bnk8, crc: u32) -> Result<Current> {
    // Without a state directory there is no snapshot, but the factory check still applies.
    if let Ok(p) = path(dev.serial())
        && let Some(blob) = read_snapshot(&p)?
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
    Ok(Current::Unknown { crc })
}

/// Decodes a known config, with the profile store's layer and macro names.
pub fn config(blob: &Blob, source: &Source) -> Result<Config> {
    let mut cfg = config::from_blob(blob)?;
    if let Source::ProfileStore { names } = source {
        names.fill(&mut cfg);
    }
    Ok(cfg)
}

/// A config encoded for the device's current state, ready to [`commit`].
pub struct Prepared {
    /// The native config to write.
    pub native: Blob,
    /// What writing it changes, per item.
    pub changes: Changes,
    /// CRC of the device's active config when this was prepared.
    pub device_crc: u32,
    /// Remarks for the user, e.g. settings that can't be kept.
    pub notes: Vec<String>,
    /// Profile store slot revision to save over, and the archive to save.
    store: Option<(u32, Vec<u8>)>,
}

impl Prepared {
    /// True when the device already has this config.
    pub fn unchanged(&self) -> bool {
        matches!(&self.changes, Changes::Items(lines) if lines.is_empty())
    }

    /// True when the write goes through the profile store (layer and macro names are kept).
    pub fn saves_to_profile_store(&self) -> bool {
        self.store.is_some()
    }
}

/// The config `blob` holds, named like `names`.
fn named(blob: &Blob, names: &Config) -> Result<Config> {
    let mut cfg = config::from_blob(blob)?;
    Names::of(names).fill(&mut cfg);
    Ok(cfg)
}

/// Encodes `cfg` for the device's current state and works out what writing
/// it would change. Nothing is written.
///
/// On a device with a profile store slot, the device's saved Studio profile is
/// updated to match `cfg` (layers and macros without a name keep their previous
/// one); `cfg` is refused when a Studio profile can't express it exactly.
pub fn prepare(dev: &Bnk8, cfg: &Config) -> Result<Prepared> {
    let device_crc = dev.status()?.token_b;
    let Some(slot) = read_slot(dev)? else {
        let mut cfg = cfg.clone();
        let mut notes = Vec::new();
        if cfg.layers.iter().any(|l| l.name.is_some()) || cfg.macros.values().any(|m| m.name.is_some()) {
            notes.push(
                "layer and macro names are kept only on keypads Binepad Studio has saved to, so they are ignored".into(),
            );
            cfg.layers.iter_mut().for_each(|l| l.name = None);
            cfg.macros.values_mut().for_each(|m| m.name = None);
        }
        let ids = StudioIds::new();
        let (native, changes) = match without_slot(dev, device_crc)? {
            Current::Known(cur, _) => {
                let native = config::to_blob(&cfg, &cur, &ids)?;
                let changes = config::changes(Some(&cur), &native)?;
                (native, changes)
            }
            Current::Unknown { .. } => (config::to_blob(&cfg, FACTORY, &ids)?, Changes::Unknown),
        };
        return Ok(Prepared { native, changes, device_crc, notes, store: None });
    };

    let prev_native = slot.native();
    ensure!(
        crc32(&prev_native) == device_crc,
        "the device's active config differs from its saved profile; unplug the BNK8, plug it back in and try again"
    );
    let doc = archive::decode(slot.archive()).context("reading the device's saved Binepad Studio profile")?;
    let (prev_cfg, aliases) =
        profile::from_profile(&doc.profile, &prev_native).context("reading the device's saved Binepad Studio profile")?;
    let ids = aliases.ids();
    // bnk8's reading of Studio's profile must reproduce the device's config,
    // or the profile it writes back could compile to something else.
    ensure!(
        config::to_blob(&prev_cfg, FACTORY, &ids)? == prev_native,
        "bnk8 can't reproduce the device's saved Binepad Studio profile exactly, so it won't update it; \
         make this change in Binepad Studio"
    );

    let mut cfg = cfg.clone();
    Names::of(&prev_cfg).fill(&mut cfg);
    let native = config::to_blob(&cfg, FACTORY, &ids)?;
    let new_profile = profile::to_profile(&cfg, &doc.profile, &aliases, &profile::now_utc())?;
    // What Studio will compile from the new profile must be exactly `native`.
    let (check, check_aliases) = profile::from_profile(&new_profile, &native)?;
    let compiled = config::to_blob(&check, FACTORY, &check_aliases.ids())?;
    if compiled != native {
        let first = config::diff(&config::from_blob(&native)?, &config::from_blob(&compiled)?).into_iter().next();
        bail!(
            "bnk8 can't express this config exactly as a Binepad Studio profile ({})",
            first.unwrap_or_else(|| "action table order".into())
        );
    }
    let archive = archive::encode(&Document { device_id: doc.device_id, profile: new_profile })?;

    let mut lines = config::diff(&named(&prev_native, &prev_cfg)?, &named(&native, &cfg)?);
    if lines.is_empty() && native != prev_native {
        lines.push("action table: reordered, bindings unchanged".into());
    }
    Ok(Prepared {
        native,
        changes: Changes::Items(lines),
        device_crc,
        notes: Vec::new(),
        store: Some((slot.revision, archive)),
    })
}

/// Replaces the snapshot `p` with `blob` through a new owner-only temporary
/// file, so a crash or a file planted under the temporary name can't leave a
/// partial snapshot or redirect the write.
fn save(p: &Path, blob: &Blob) -> Result<()> {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.subsec_nanos());
    let tmp = p.with_extension(format!("{}.{nanos}.tmp", std::process::id()));
    let mut o = OpenOptions::new();
    o.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut o, 0o600);
    let mut f = open_options(&mut o).open(&tmp).with_context(|| format!("creating {}", tmp.display()))?;
    let written = f.write_all(blob).and_then(|()| f.sync_all()).and_then(|()| std::fs::rename(&tmp, p));
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    written.with_context(|| format!("replacing {}", p.display()))
}

/// Writes a prepared config, and records it as the snapshot. Fails without
/// writing if the device's config changed since [`prepare`]. The snapshot
/// directory is prepared first, so an unusable one fails before the device is touched.
pub fn commit(dev: &Bnk8, p: &Prepared) -> Result<()> {
    let snapshot = path(dev.serial())?;
    make_dir(snapshot.parent().unwrap())?;
    ensure!(
        dev.status()?.token_b == p.device_crc,
        "the device's config changed since the changes were worked out; read it again"
    );
    match &p.store {
        None => dev.write_config(&p.native, p.device_crc)?,
        Some((revision, archive)) => dev.store_save(*revision, &[&p.native[..], archive].concat(), &mut |_| {})?,
    }
    save(&snapshot, &p.native).context(
        "the device was updated, but its local snapshot was not saved: \
         bnk8 will report the config as unknown until the next apply or reset",
    )
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    #[test]
    fn snapshots_are_private_regular_files() {
        let d = std::env::temp_dir().join(format!("bnk8-store-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let state = d.join("state/bnk8");
        make_dir(&state).unwrap();
        assert_eq!(std::fs::metadata(&state).unwrap().permissions().mode() & 0o777, 0o700);

        // A written snapshot is owner-only and reads back; no temporary file is left.
        let p = state.join("SERIAL.bin");
        save(&p, FACTORY).unwrap();
        save(&p, FACTORY).unwrap();
        assert_eq!(std::fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(read_snapshot(&p).unwrap(), Some(*FACTORY));
        assert_eq!(std::fs::read_dir(&state).unwrap().count(), 1);

        // A file of the wrong size is no snapshot; a symlink or a directory is an error.
        std::fs::write(&p, [0; 3]).unwrap();
        assert_eq!(read_snapshot(&p).unwrap(), None);
        std::fs::remove_file(&p).unwrap();
        let elsewhere = d.join("elsewhere.bin");
        std::fs::write(&elsewhere, FACTORY).unwrap();
        std::os::unix::fs::symlink(&elsewhere, &p).unwrap();
        assert!(read_snapshot(&p).is_err());
        std::fs::remove_file(&p).unwrap();
        std::fs::create_dir(&p).unwrap();
        assert!(read_snapshot(&p).is_err());

        // A state directory others can write to is refused.
        std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o777)).unwrap();
        assert!(format!("{:#}", make_dir(&state).unwrap_err()).contains("writable by other users"));
        std::fs::remove_dir_all(&d).unwrap();
    }
}
