# bnk8

Open-source tools for the **Binepad BNK8 AI Edition** macropad (8 keys + a push knob): a command-line tool, a desktop configurator, and documentation of the device's USB protocol.

It programs key bindings, knob gestures, macros, lighting and timing directly over USB. You don't need Binepad's own software.

> Not affiliated with or endorsed by Binepad. Tested on macOS with firmware 6.0.0 (reported internally as 6.0.25). Windows and Linux builds should work but are untested.

## What's in here

| Part | Path | What it does |
|---|---|---|
| `bnk8` CLI | `src/` | List devices, dump the config as TOML, apply a TOML config, change single bindings, factory reset |
| `bnk8-gui` | `gui/` | Desktop configurator: keypad editor, macros, timing, lighting, layer templates |
| Protocol notes | [`PROTOCOL.md`](PROTOCOL.md) | Reverse-engineered frame format, commands and 2000-byte config layout |

## Download (macOS)

Get the latest build from [Releases](https://github.com/alexdibattista/bnk8-ai/releases). Both downloads are signed and notarized by Apple and run on Apple Silicon and Intel Macs with macOS 11 or newer.

- **App:** open `BNK8-Configurator-<version>-macos-universal.dmg` and drag **BNK8 Configurator** to Applications.
- **CLI:** extract `bnk8-<version>-macos-universal.tar.gz` and move `bnk8` somewhere on your `PATH`, e.g. `/usr/local/bin`.

Maintainers: see [RELEASING.md](RELEASING.md).

## Build

Requires Rust 1.92 or newer (the GUI's file-dialog dependency, `rfd`, sets that minimum).

```sh
cargo build --release                 # CLI  -> target/release/bnk8
cargo build --release -p bnk8-gui     # GUI  -> target/release/bnk8-gui
```

Or install both onto your `PATH`:

```sh
cargo install --path .
cargo install --path gui
```

On Linux, `hidapi` needs the udev development headers (`libudev-dev` on Debian/Ubuntu), and your user needs access to the device's hidraw node, usually through a udev rule for vendor `4249`, product `4e41`.

## Desktop app

```sh
cargo run --release -p bnk8-gui
```

Plug the BNK8 in first; the app finds and loads it on startup (press **Refresh** if you plug it in later).

- **Keys & lighting**: click a key or the knob on the keypad, then choose what each gesture does (tap, double tap, hold, tap then hold; the knob also has turn left/right). Set the layer's effect, brightness, speed and colour.
- **Macros**: up to 15 macros of up to 7 key steps, with optional delays.
- **Timing**: double-tap window and hold threshold.
- **Layer templates**: **+ Add from template…** adds a ready-made layer for **Zoom** or **Google Meet** (macOS or Windows/Linux shortcuts). Shortcuts with modifier keys are stored as macros, so each template uses up to 6 free macro slots.
- Edits stay local until you press **Apply to device**. The bottom bar shows how many changes are pending, and you can list them. **Import/Export TOML** uses the same format as the CLI.

## Command line

```sh
bnk8 list                         # attached devices
bnk8 info                         # firmware, capacity, config state
bnk8 dump -o my-config.toml       # current config as TOML
bnk8 apply my-config.toml --dry-run   # show per-item changes only
bnk8 apply my-config.toml         # validate, show changes, write
bnk8 reset                        # restore the factory config
```

Change one setting without a file:

```sh
bnk8 set key 1 keyboard.up                     # K1 tap = up arrow
bnk8 set key 7 keyboard.f9 --slot hold         # K7 hold = F9
bnk8 set knob cw media.volume_up --layer 2     # knob turn right on layer 2
bnk8 set lighting --layer 0 --effect 1 --color "#730000" --brightness 80
```

Keys are numbered 1 to 8 from top left to bottom right, skipping the knob (top right). With several devices attached, add `--serial <SERIAL>` (see `bnk8 list`).

## Config format

```toml
device = "binepad.bnk8"

[timing]
double_tap_ms = 260
hold_ms = 350

[[layers]]                     # up to 10 layers
keys = [
    "keyboard.up",             # K1 tap only
    "keyboard.down",           # K2
    "macro.0",                 # K3 plays macro 0
    "media.play_pause",        # K4
    "none",                    # K5 unbound
    "keyboard.escape",         # K6
    { hold = "keyboard.f9" },  # K7: several gestures as a table
    { tap = "media.mute", double_tap = "media.next" },  # K8
]

[layers.knob]                  # tap, double_tap, hold, tap_hold, ccw, cw
tap = "layer.next"
ccw = "media.volume_down"
cw = "media.volume_up"

[layers.lighting]
effect = 1                     # 0 = off, up to 50
brightness = 80                # up to 180
speed = 120
color = "#730000"
flags = 1

[macros]                       # up to 15 macros, up to 7 key steps each
0 = ["press keyboard.left_ctrl", "delay 10", "tap keyboard.q", "delay 10", "release keyboard.left_ctrl"]
```

[`configs/omp.toml`](configs/omp.toml) is a complete real-world config.

**Action names**

| Group | Examples |
|---|---|
| Keyboard | `keyboard.a`, `keyboard.f13`, `keyboard.left_ctrl`, `keyboard.numpad.7`, `keyboard.page_up` |
| Media | `media.volume_up`, `media.volume_down`, `media.mute`, `media.play_pause`, `media.next`, `media.previous` |
| Layers | `layer.next`, `layer.previous`, `layer.0` … `layer.9` |
| Lighting | `rgb.toggle`, `rgb.brightness.up`, `rgb.effect.next`, `rgb.speed.down`, … |
| Macros | `macro.0` … `macro.14` |
| Other | `none` (unbound); `raw.<14 hex>` for entries bnk8 can't name, e.g. Binepad Studio's AI actions, kept byte for byte |

Macro steps are `tap <key>`, `press <key>`, `release <key>` and `delay <ms>` (a multiple of 10, up to 2550 per step). `<key>` is a `keyboard.*` or `media.*` name.

Keyboard bindings send a single key. For a shortcut like Cmd+Shift+A, put it in a macro and bind `macro.N`.

**Gestures.** `tap` fires immediately when a key has no other gestures; otherwise it waits out the double-tap window. `hold` presses when the hold threshold passes and releases with the key, which makes push-to-talk possible.

Keep a `layer.next` binding (factory: knob press) on every layer so you can always switch layers.

## How reading the config works

The device has no command to read its config back. It does report a CRC-32 of the active config, so bnk8 saves every config it writes to `~/.local/state/bnk8/<serial>.bin` (or `$XDG_STATE_HOME/bnk8/`). It trusts that file, or the built-in factory config, only when its CRC matches the device.

If the device was last programmed by another tool (such as Binepad Studio), its config shows as **unknown**: editing starts from the factory defaults, and applying writes the whole config. Every write replaces all 2000 bytes; the per-item changes shown are for your review.

Details are in [`PROTOCOL.md`](PROTOCOL.md). bnk8 never sends the firmware-update command.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.

Unless you explicitly state otherwise, any contribution you intentionally submit for inclusion in this work, as defined in the Apache-2.0 license, is dual licensed as above, without any additional terms or conditions.

The bundled Inter fonts (`gui/assets/fonts/`) are under the SIL Open Font License 1.1; see `gui/assets/fonts/Inter-LICENSE.txt`. `assets/factory-config.bin` is the device's factory configuration, included so a factory reset works offline. It is not covered by this project's license.
