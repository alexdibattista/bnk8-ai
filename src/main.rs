use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail, ensure};
use clap::{Parser, Subcommand};
use hidapi::HidApi;

use bnk8::config::{self, Blob, Changes, Config, FACTORY, KEY_SLOTS, KNOB_SLOTS, Key};
use bnk8::frame::{self, Frame, REPORT_LEN};
use bnk8::store::{self, Current, Source};
use bnk8::{actions, protocol, transport};

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
        /// Action name, e.g. keyboard.a, media.mute, macro.0, none.
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
        /// Effect id 0..=50 (0 = off).
        #[arg(long)]
        effect: Option<u8>,
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
    /// Initialise the stream flash area so RAM-only config sessions unlock.
    Stream,
    /// Reset a stuck stream session (0x15 sub 4).
    Finish,
    /// Dump the raw 22-byte stream status payload.
    Status,
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
        Command::Probe { what } => match what {
            Probe::Stream => probe_stream(serial),
            Probe::Finish => probe_finish(serial),
            Probe::Status => probe_status(serial),
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

/// The device's current config; errors when it cannot be identified.
fn known(dev: &transport::Bnk8) -> Result<Blob> {
    match store::current(dev)? {
        Current::Known(blob, _) => Ok(*blob),
        Current::Unknown { crc } => Err(unknown_config(crc)),
    }
}

/// Encodes `cfg` over `base`, prints the per-item diff against `before`
/// (if known), and writes unless `dry_run`.
fn write(dev: &transport::Bnk8, before: Option<&Blob>, base: &Blob, cfg: &Config, dry_run: bool) -> Result<()> {
    let blob = config::to_blob(cfg, base)?;
    let changes = match config::changes(before, &blob)? {
        Changes::Items(lines) => {
            for l in &lines {
                println!("{l}");
            }
            lines.len()
        }
        Changes::Unknown => {
            println!("device config unknown: the whole config will be written");
            config::flatten(&config::from_blob(&blob)?).len()
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
    store::write(dev, &blob)?;
    println!("applied {changes} changes");
    Ok(())
}

fn dump(serial: Option<&str>, output: Option<&str>) -> Result<()> {
    let dev = transport::open(serial)?;
    let text = config::to_toml(&config::from_blob(&known(&dev)?)?)?;
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
    config::to_blob(&cfg, FACTORY).with_context(|| format!("validating {file}"))?;
    let dev = transport::open(serial)?;
    match store::current(&dev)? {
        Current::Known(cur, _) => write(&dev, Some(&cur), &cur, &cfg, dry_run),
        Current::Unknown { .. } => write(&dev, None, FACTORY, &cfg, dry_run),
    }
}

fn layer_mut(cfg: &mut Config, layer: usize) -> Result<&mut config::Layer> {
    let n = cfg.layers.len();
    ensure!(layer < n, "--layer {layer}: the config has layers 0..={}", n - 1);
    Ok(&mut cfg.layers[layer])
}

fn set(serial: Option<&str>, what: Set) -> Result<()> {
    let dev = transport::open(serial)?;
    let cur = known(&dev)?;
    let mut cfg = config::from_blob(&cur)?;
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
            l.effect = effect.unwrap_or(l.effect);
            l.brightness = brightness.unwrap_or(l.brightness);
            l.speed = speed.unwrap_or(l.speed);
            l.color = color.unwrap_or_else(|| l.color.clone());
        }
    }
    write(&dev, Some(&cur), &cur, &cfg, false)
}

fn reset(serial: Option<&str>) -> Result<()> {
    let dev = transport::open(serial)?;
    store::write(&dev, FACTORY)?;
    println!("restored the factory config");
    Ok(())
}

const STREAM_SIZE: u16 = 0x30; // minimum accepted stream payload size

fn poll_state(dev: &transport::Bnk8, want: u8, tries: u32) -> Result<protocol::StreamStatus> {
    for _ in 0..tries {
        let s = dev.stream_status()?;
        println!("  phase={} revision={:#010x} archive={:#06x}", s.phase, s.revision, s.archive_len);
        if s.phase == want {
            return Ok(s);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    bail!("stream phase never reached {want}");
}

/// Drive the `0x15` stream write flow to create a valid stream slot,
/// which unlocks `0x11` sub-4 RAM-only config sessions.
fn probe_stream(serial: Option<&str>) -> Result<()> {
    let dev = transport::open(serial)?;
    let s = dev.stream_status()?;
    println!("before: phase={} revision={:#010x} archive={:#06x}", s.phase, s.revision, s.archive_len);
    let total = protocol::CONFIG_LEN + STREAM_SIZE as usize;
    let data = vec![0u8; total];
    let crc = protocol::crc32(&data);
    println!("begin: revision={:#010x} archive={STREAM_SIZE:#06x} crc={crc:#010x}", s.revision);
    dev.stream_begin(s.revision, STREAM_SIZE, crc)?;
    poll_state(&dev, 4, 40)?;
    println!("phase 4 (ready for data), pushing {total} zero bytes");
    let mut sent = 0usize;
    let mut retries = 0u32;
    while sent < total {
        let n = (total - sent).min(16);
        match dev.stream_data(sent as u16, &data[sent..sent + n]) {
            Ok(()) => {
                sent += n;
                retries = 0;
                if sent % 256 < 16 {
                    let p = dev.stream_status_payload()?;
                    println!(
                        "  {sent}/{total} phase={} counter={}",
                        p[1],
                        u16::from_le_bytes([p[12], p[13]])
                    );
                }
            }
            Err(e) => {
                retries += 1;
                ensure!(retries < 2000, "stuck at {sent}/{total}: {e}");
                std::thread::sleep(Duration::from_millis(2));
            }
        }
    }
    println!("all data accepted, waiting for finalize");
    let s = poll_state(&dev, protocol::PHASE_IDLE, 60)?;
    ensure!(s.revision != 0, "stream slot still invalid after write");
    let readback = dev.stream_read(s.revision, 0, 16)?;
    println!(
        "slot valid: revision={:#010x} archive={:#06x} read[0..16]={}",
        s.revision,
        s.archive_len,
        to_hex(&readback)
    );
    Ok(())
}

fn probe_finish(serial: Option<&str>) -> Result<()> {
    let dev = transport::open(serial)?;
    let s = dev.stream_status()?;
    println!("before: phase={} revision={:#010x}", s.phase, s.revision);
    dev.stream_finish(s.revision)?;
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
            d.serial_number().unwrap_or("<no serial>"),
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
            let cfg = config::from_blob(&blob)?;
            let source = match source {
                Source::Factory => "factory default",
                Source::Snapshot => "matches the local snapshot",
                Source::ProfileStore => "read from the device (saved by Binepad Studio)",
            };
            println!(
                "  config:    {source}, CRC {:#010x}, {} layers, {} macros",
                protocol::crc32(&*blob),
                cfg.layers.len(),
                cfg.macros.len()
            );
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
