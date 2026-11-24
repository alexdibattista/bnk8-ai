use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail, ensure};
use clap::{Parser, Subcommand};
use hidapi::HidApi;

use bnk8::config::{self, Changes, Config, FACTORY, KEY_SLOTS, KNOB_SLOTS, Key, StudioIds};
use bnk8::frame::{self, Frame, REPORT_LEN};
use bnk8::store::{self, Current, Source};
use bnk8::{actions, protocol, transport};

mod mcp;

/// Program the Binepad BNK8 AI Edition.
#[derive(Parser)]
#[command(version)]
struct Cli {
    /// Select a device by USB serial number when several are attached.
    #[arg(long, global = true)]
    serial: Option<String>,
    #[command(subcommand)]
    cmd: Command,
}

#[derive(Subcommand)]
enum Command {
    /// List attached BNK8 AI Edition devices.
    List,
    /// Print device identity, firmware and config state.
    Info,
    /// Write the device's config as TOML (read from the device's profile
    /// store or the verified local snapshot).
    Dump {
        /// Output file; stdout when omitted.
        #[arg(short, long)]
        output: Option<String>,
    },
    /// Validate a TOML config, show per-item changes, and write it.
    Apply {
        file: String,
        /// Show the changes without writing.
        #[arg(long)]
        dry_run: bool,
    },
    /// Change one binding or lighting setting.
    Set {
        #[command(subcommand)]
        what: Set,
    },
    /// Restore the factory default config.
    Reset,
    /// Run an MCP server on stdin/stdout so Claude Desktop or Claude Code can
    /// read the config, preview edits and apply them.
    Mcp,
    /// Internal reverse-engineering probes.
    #[command(hide = true)]
    Probe {
        #[command(subcommand)]
        what: Probe,
    },
    /// Send one raw 32-byte report and print every frame received during the listen window
    /// (reverse engineering). The device also emits an unsolicited frame every ~2 s, so the
    /// arrival offset is printed to tell replies from that heartbeat.
    #[command(hide = true)]
    Raw {
        /// Report bytes as hex; spaces allowed. Zero-padded to 32 bytes.
        #[arg(required = true, num_args = 1..)]
        hex: Vec<String>,
        /// Seal the report: hex may be at most 30 bytes, CRC goes in bytes 30..32.
        #[arg(long)]
        crc: bool,
        /// How long to collect frames after sending, in milliseconds.
        #[arg(long, default_value_t = 2500)]
        listen_ms: u64,
    },
}

#[derive(Subcommand)]
enum Set {
    /// Bind a key (1..8, top-left to bottom-right, skipping the knob).
    Key {
        #[arg(value_parser = clap::value_parser!(u8).range(1..=8))]
        key: u8,
        /// Action name, e.g. keyboard.a, media.mute, macro.1, claude.accept, none.
        action: String,
        #[arg(long, default_value_t = 0)]
        layer: usize,
        #[arg(long, default_value = "tap", value_parser = KEY_SLOTS)]
        slot: String,
    },
    /// Bind a knob gesture.
    Knob {
        #[arg(value_parser = KNOB_SLOTS)]
        slot: String,
        action: String,
        #[arg(long, default_value_t = 0)]
        layer: usize,
    },
    /// Change a layer's lighting.
    Lighting {
        #[arg(long, default_value_t = 0)]
        layer: usize,
        /// Effect name (solid, breathing, …) or id 0..=50.
        #[arg(long)]
        effect: Option<String>,
        /// 0..=180.
        #[arg(long)]
        brightness: Option<u8>,
        #[arg(long)]
        speed: Option<u8>,
        /// #RRGGBB.
        #[arg(long)]
        color: Option<String>,
    },
}

#[derive(Subcommand)]
enum Probe {
    /// Reset a stuck stream session (0x15 sub 4).
    Finish,
    /// Dump the raw 22-byte stream status payload.
    Status,
    /// Write the profile store's active slot (native config, then archive) to a file.
    StoreDump {
        #[arg(short, long)]
        output: String,
    },
    /// Save the profile store's active slot again, byte for byte, logging every
    /// store status (exercises the save sequence without changing the config).
    StoreResave,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let serial = cli.serial.as_deref();
    match cli.cmd {
        Command::List => list(serial),
        Command::Info => info(serial),
        Command::Dump { output } => dump(serial, output.as_deref()),
        Command::Apply { file, dry_run } => apply(serial, &file, dry_run),
        Command::Set { what } => set(serial, what),
        Command::Reset => reset(serial),
        Command::Mcp => mcp::serve(serial, std::io::stdin().lock(), std::io::stdout().lock()),
        Command::Probe { what } => match what {
            Probe::Finish => probe_finish(serial),
            Probe::Status => probe_status(serial),
            Probe::StoreDump { output } => probe_store_dump(serial, &output),
            Probe::StoreResave => probe_store_resave(serial),
        },
        Command::Raw {
            hex,
            crc,
            listen_ms,
        } => raw(serial, &hex.concat(), crc, listen_ms),
    }
}

fn unknown_config(crc: u32) -> anyhow::Error {
    anyhow::anyhow!(
        "the device's config (CRC {crc:#010x}) matches neither bnk8's snapshot, the factory config, \
         nor the device's profile store; run `bnk8 reset` or `bnk8 apply FILE` with a complete config first"
    )
}

/// The device's current config, with layer names; errors when it cannot be identified.
fn known(dev: &transport::Bnk8) -> Result<Config> {
    match store::current(dev)? {
        Current::Known(blob, source) => store::config(&blob, &source),
        Current::Unknown { crc } => Err(unknown_config(crc)),
    }
}

/// Prints what writing `cfg` changes on the device, and writes it unless `dry_run`.
fn write(dev: &transport::Bnk8, cfg: &Config, dry_run: bool) -> Result<()> {
    let prepared = store::prepare(dev, cfg)?;
    for n in &prepared.notes {
        println!("note: {n}");
    }
    let changes = match &prepared.changes {
        Changes::Items(lines) => {
            for l in lines {
                println!("{l}");
            }
            lines.len()
        }
        Changes::Unknown => {
            println!("device config unknown: the whole config will be written");
            config::flatten(&config::from_blob(&prepared.native)?).len()
        }
    };
    if changes == 0 {
        println!("0 changes");
        return Ok(());
    }
    if dry_run {
        println!("{changes} changes (dry run, nothing written)");
        return Ok(());
    }
    store::commit(dev, &prepared)?;
    println!("applied {changes} changes");
    Ok(())
}

fn dump(serial: Option<&str>, output: Option<&str>) -> Result<()> {
    let dev = transport::open(serial)?;
    let text = config::to_toml(&known(&dev)?)?;
    match output {
        Some(path) => std::fs::write(path, text).with_context(|| format!("writing {path}")),
        None => {
            print!("{text}");
            Ok(())
        }
    }
}

fn apply(serial: Option<&str>, file: &str, dry_run: bool) -> Result<()> {
    let text = std::fs::read_to_string(file).with_context(|| format!("reading {file}"))?;
    let cfg = config::from_toml(&text).with_context(|| format!("parsing {file}"))?;
    // Validate before touching the device.
    config::to_blob(&cfg, FACTORY, &StudioIds::new()).with_context(|| format!("validating {file}"))?;
    let dev = transport::open(serial)?;
    write(&dev, &cfg, dry_run)
}

fn layer_mut(cfg: &mut Config, layer: usize) -> Result<&mut config::Layer> {
    let n = cfg.layers.len();
    ensure!(layer < n, "--layer {layer}: the config has layers 0..={}", n - 1);
    Ok(&mut cfg.layers[layer])
}

fn set(serial: Option<&str>, what: Set) -> Result<()> {
    let dev = transport::open(serial)?;
    let mut cfg = known(&dev)?;
    let binding = |action: String| (action != "none").then_some(action);
    match what {
        Set::Key { key, action, layer, slot } => {
            let k = &mut layer_mut(&mut cfg, layer)?.keys[key as usize - 1];
            let mut s = k.slots();
            *s.slot_mut(&slot).unwrap() = binding(action);
            *k = Key::from_slots(s);
        }
        Set::Knob { slot, action, layer } => {
            *layer_mut(&mut cfg, layer)?.knob.slot_mut(&slot).unwrap() = binding(action);
        }
        Set::Lighting { layer, effect, brightness, speed, color } => {
            ensure!(
                effect.is_some() || brightness.is_some() || speed.is_some() || color.is_some(),
                "nothing to set: pass --effect, --brightness, --speed or --color"
            );
            let l = &mut layer_mut(&mut cfg, layer)?.lighting;
            if let Some(e) = effect {
                l.effect = config::parse_effect(&e)?;
            }
            l.brightness = brightness.unwrap_or(l.brightness);
            l.speed = speed.unwrap_or(l.speed);
            l.color = color.unwrap_or_else(|| l.color.clone());
            // Re-derive the flags byte from the new settings.
            l.flags = None;
        }
    }
    write(&dev, &cfg, false)
}

/// Restores the factory bindings and lighting; on a device with a profile
/// store, layers keep their names.
fn reset(serial: Option<&str>) -> Result<()> {
    let dev = transport::open(serial)?;
    write(&dev, &config::from_blob(FACTORY)?, false)
}

/// Writes the profile store's active slot (native config, then archive) to `output`.
fn probe_store_dump(serial: Option<&str>, output: &str) -> Result<()> {
    let dev = transport::open(serial)?;
    let s = dev.store_wait_idle()?;
    ensure!(s.revision != 0, "the device has no profile store slot");
    let payload = dev.profile_store_read(s.revision, s.native_len as usize + s.archive_len as usize)?;
    ensure!(protocol::crc32(&payload) == s.payload_crc, "profile store payload fails its CRC");
    std::fs::write(output, &payload).with_context(|| format!("writing {output}"))?;
    println!("revision {}: {} bytes to {output}", s.revision, payload.len());
    Ok(())
}

fn probe_store_resave(serial: Option<&str>) -> Result<()> {
    let dev = transport::open(serial)?;
    let s = dev.store_wait_idle()?;
    ensure!(s.revision != 0, "the device has no profile store slot");
    let payload = dev.profile_store_read(s.revision, s.native_len as usize + s.archive_len as usize)?;
    ensure!(protocol::crc32(&payload) == s.payload_crc, "profile store payload fails its CRC");
    println!("re-saving revision {} ({} bytes, CRC {:#010x})", s.revision, payload.len(), s.payload_crc);
    let start = Instant::now();
    let mut last = (u8::MAX, u16::MAX);
    dev.store_save(s.revision, &payload, &mut |st| {
        // Log phase changes, and every 512 bytes received.
        if st.phase != last.0 || st.received / 512 != last.1 / 512 {
            println!(
                "  +{:>5}ms phase={:>2} revision={} received={}",
                start.elapsed().as_millis(),
                st.phase,
                st.revision,
                st.received
            );
            last = (st.phase, st.received);
        }
    })?;
    let after = dev.stream_status()?;
    println!(
        "  +{:>5}ms done: revision {}, token B {:#010x}, read back identical",
        start.elapsed().as_millis(),
        after.revision,
        dev.status()?.token_b
    );
    Ok(())
}

fn probe_finish(serial: Option<&str>) -> Result<()> {
    let dev = transport::open(serial)?;
    let s = dev.stream_status()?;
    println!("before: phase={} revision={:#010x}", s.phase, s.revision);
    dev.store_cancel(s.revision)?;
    let s = dev.stream_status()?;
    println!("after:  phase={} revision={:#010x}", s.phase, s.revision);
    Ok(())
}

fn probe_status(serial: Option<&str>) -> Result<()> {
    let dev = transport::open(serial)?;
    println!("{}", to_hex(&dev.stream_status_payload()?));
    Ok(())
}

fn list(serial: Option<&str>) -> Result<()> {
    let api = HidApi::new().context("initialising hidapi")?;
    let devices: Vec<_> = transport::find(&api)
        .into_iter()
        .filter(|d| serial.is_none_or(|s| d.serial_number() == Some(s)))
        .collect();
    if devices.is_empty() {
        return Err(transport::not_found(serial));
    }
    for d in devices {
        println!(
            "{}  {}  {}",
            transport::display_serial(d.serial_number()),
            transport::format_fw(d.release_number()),
            d.path().to_string_lossy()
        );
    }
    Ok(())
}

fn info(serial: Option<&str>) -> Result<()> {
    let dev = transport::open(serial)?;
    let hello = dev.hello()?;
    let id = dev.device_id()?;
    let patch_info = dev.info()?;
    ensure!(
        patch_info == hello.patch,
        "patch version mismatch: hello {:#04x}, info {:#04x}",
        hello.patch,
        patch_info
    );
    println!("BNK8 AI Edition {} ({})", dev.serial(), config::DEVICE);
    println!(
        "  firmware:  {} (USB), internal 6.0.{}",
        transport::format_fw(dev.fw()),
        hello.patch
    );
    println!("  protocol:  1");
    println!("  uid:       {}", to_hex(&id.uid));
    println!("  serial hash: {:#010x}", hello.serial_hash);
    println!(
        "  capacity:  {} layers, {} macros x 7 steps, 96 actions",
        actions::MAX_LAYERS,
        actions::MACRO_SLOTS
    );
    match store::current(&dev)? {
        Current::Known(blob, source) => {
            let cfg = store::config(&blob, &source)?;
            let label = match &source {
                Source::Factory => "factory default".to_string(),
                Source::OlderFactory(version) => format!("factory default of firmware {version}"),
                Source::Snapshot => "matches the local snapshot".to_string(),
                Source::ProfileStore { .. } => "read from the device's profile store".to_string(),
            };
            println!(
                "  config:    {label}, CRC {:#010x}, {} layers, {} macros",
                protocol::crc32(&*blob),
                cfg.layers.len(),
                cfg.macros.len()
            );
            let named: Vec<String> = cfg
                .layers
                .iter()
                .enumerate()
                .filter_map(|(i, l)| Some(format!("{i} {}", l.name.as_ref()?)))
                .collect();
            if !named.is_empty() {
                println!("  layers:    {}", named.join(", "));
            }
        }
        Current::Unknown { crc } => {
            println!("  config:    unknown to bnk8 (CRC {crc:#010x}); dump/set need `reset` or `apply` first")
        }
    }
    Ok(())
}

fn raw(serial: Option<&str>, hex: &str, seal: bool, listen_ms: u64) -> Result<()> {
    let bytes = parse_hex(hex)?;
    let max = if seal { REPORT_LEN - 2 } else { REPORT_LEN };
    if bytes.len() > max {
        bail!("report is {} bytes; at most {max} allowed", bytes.len());
    }
    let mut report = [0u8; REPORT_LEN];
    report[..bytes.len()].copy_from_slice(&bytes);
    if seal {
        frame::seal(&mut report);
    }
    // 0x42 is Studio's factory reset, or an application flash rewrite per PROTOCOL.md's
    // firmware analysis; unresolved, so never send it.
    if report[..2] == frame::MAGIC && report[4] == 0x42 {
        bail!(
            "refusing to send msg 0x42 (factory reset, or an application flash rewrite per PROTOCOL.md; \
             unresolved): it can brick the device"
        );
    }

    let dev = transport::open(serial)?;
    eprintln!(
        "BNK8 {} fw {}",
        dev.serial(),
        transport::format_fw(dev.fw())
    );
    dev.send(&report)?;
    let sent = Instant::now();
    let window = Duration::from_millis(listen_ms);
    let mut frames = 0;
    while let Some(left) = window.checked_sub(sent.elapsed()) {
        // Round up: a sub-millisecond remainder must not become a non-blocking 0 ms read.
        let left_ms = i32::try_from(left.as_millis() + 1).unwrap_or(i32::MAX);
        let Some(reply) = dev.recv(left_ms)? else {
            break;
        };
        frames += 1;
        println!("{}", to_hex(&reply));
        let at = sent.elapsed().as_millis();
        match frame::verify(&reply) {
            Ok(()) => {
                let f = Frame(reply);
                println!(
                    "  +{at}ms version={} flags=0x{:02x} msg_type=0x{:02x} seq={} payload_len={} payload={}",
                    f.version(),
                    f.flags(),
                    f.msg_type(),
                    f.seq(),
                    f.payload_len(),
                    to_hex(f.payload())
                );
            }
            Err(e) => println!("  +{at}ms {e}"),
        }
    }
    if frames == 0 {
        bail!("no response from BNK8 within {listen_ms} ms");
    }
    Ok(())
}

fn parse_hex(s: &str) -> Result<Vec<u8>> {
    let digits: Vec<u8> = s.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
    if !digits.len().is_multiple_of(2) {
        bail!("hex has an odd number of digits");
    }
    digits
        .chunks(2)
        .map(|pair| {
            let pair = std::str::from_utf8(pair).context("hex is not ASCII")?;
            u8::from_str_radix(pair, 16).with_context(|| format!("invalid hex byte {pair:?}"))
        })
        .collect()
}

fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::parse_hex;

    #[test]
    fn hex_with_spaces() {
        assert_eq!(
            parse_hex("b1 5d01\t0a").unwrap(),
            vec![0xb1, 0x5d, 0x01, 0x0a]
        );
        assert!(parse_hex("b1 5").is_err());
        assert!(parse_hex("zz").is_err());
    }
}
