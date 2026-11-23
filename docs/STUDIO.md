# Binepad Studio: features and how they map to the BNK8

Findings from the decompiled Binepad Studio .NET assemblies (`Binepad.Studio`, `Binepad.Agent`, `Binepad.Core`, `Binepad.Protocol`), checked against `PROTOCOL.md` and, where noted, against the BNK8 on the bench (read-only). This file describes behaviour and data formats for interoperability; it contains no Binepad code.

Paths in brackets point into the decompiled sources (`work/decomp/<Assembly>/…`, not in this repo). `[LIVE]` = confirmed on the device. `[INFERENCE]` = not confirmed.

## 1. Architecture

| Part | Role |
|---|---|
| **Studio** (Avalonia UI) | Editor: mapping, lighting, macros, automation, integrations, OLED, settings, updates. Talks to the agent over a named pipe. [Studio/…Services/AgentPipeClient] |
| **Agent** (background process) | Owns the HID connection, receives key events, runs desktop and AI actions, switches layers per app, sends lighting feedback, writes configs. Started as a macOS LaunchAgent. [Agent/Program.cs, Studio/…Views/AtelierMacStartup] |
| **Core** | Profile model (JSON), action catalogue, integrations (Codex, Claude, MIDI, weather), signed update client. |
| **Protocol** | Frame codec, per-device config codecs (BNK8, BNK9, BNK16, KnobX1, CandyPad), the on-device profile store, factory images. |
| **Device packs** | `device.json` files per product (geometry, USB ids, capabilities), loaded from disk; not in the assemblies. [Core/…/DevicePackCatalog] |

Supported products: `binepad.bnk8` (VID `0x4249`, PID `0x4E41`), `binepad.bnk9` (`0x4249`/`0x4E39`), `binepad.knobx1` (`0x5831`/`0x4249`), `binepad.bnk16`, `binepad.candypad(-claude)` (`0x4249`/`0x4350`). [Studio/…Services/CompatibleApplication]

Capabilities enum: KeyMapping, Encoders, Layers, TapBehaviors, OnDeviceMacros, RgbMatrix, RgbPerKey, Oled, Wireless, FirmwareUpdate.

The macOS platform layer that posts keystrokes and drives the Accessibility API lives in an assembly that wasn't decompiled. The agent requires Accessibility access (`HasAccessibilityAccess`).

## 2. Feature inventory (BNK8-relevant)

| Feature | What it does | Device side |
|---|---|---|
| Key mapping | Per layer, per control: Tap, DoubleTap, Hold, TapHold; knob also RotateCounterClockwise/RotateClockwise | Keymap bytes (same layout bnk8 uses) |
| Layer names | Free text names ("Codex", "Claude", "Media", "Blank" in the factory profile) | Not in the native config; stored in the on-device profile archive (§5) |
| Per-key colours | Lighting editor sets a colour per key on top of the layer effect | Lighting bytes 8..39 (§4) |
| Shared lighting | One lighting setup for all layers | Lighting flag bit 2 (§4) |
| Effects | 20 named effects + Off (§4) | Lighting byte 0 |
| On-device macros | 15 slots × 7 steps (Tap / KeyDown / KeyUp + delay) | Macro region |
| Desktop macros | Slots 16–25, unlimited steps, run by the agent | Bound as kind-1 (Agent) actions; never stored on the device |
| Desktop actions | `system.*`: copy, paste, undo, lock, screenshot, browser back, window snap, media rewind/fast-forward, dictation, emoji, Bluetooth, app volume… (126 ids) | Kind 1; some have native params the firmware can execute alone (§3) |
| App launch | `app.open`, `app.focus` | Kind 1 (agent) |
| Profiles | `profile.next/previous` (host-side profile switching) | Kind 1 |
| Automation | Switch layer when an app comes to the front (`applicationBindings`, `autoSwitchApplications`, `restorePreviousLayerOnAppExit`) | Agent sends `0x13` ActivateLayer |
| Codex integration | 113 `codex.*` actions: approve/decline, model/effort knob, dictation, new/fork/next task, fast mode… | Kind 2 (Integration) |
| Claude integration | 118 `claude.*` actions: accept/reject, voice, sessions, model/effort knob, side chat, browser… mostly via Claude Desktop's menu items | Kind 2 |
| AI feedback lighting | Keys light up for pending approvals, permission prompts, task done/attention, fast mode | `0x30` IntegrationMessage (§6) |
| MIDI | 79 `midi.*` actions (notes, CC, NRPN, MMC, clock…) | BNK9/KnobX1 only (`0x31` NativeMidi) |
| OLED dashboard, haptics, battery, Bluetooth | — | Other products only |
| Read back from device | Loads the exact layout from the keypad, including layer names | On-device profile store, `0x15` (§5) `[LIVE]` |
| Factory reset | Restores factory layout; requires USB | `0x42` (§7) |
| Firmware update | BNK8 6.0.14 … 6.0.27 images (SHA-256 pinned); STM32 bootloader | `0x40` EnterBootloader (§7) |
| Studio self-update | Signed manifest from `raw.githubusercontent.com/binepad/binepad-studio-updates`, ECDSA-verified | — |
| Weather | OLED weather via `open-meteo.com` | Not BNK8 |

## 3. Actions

### Kinds

The action table's kind byte is Studio's `ActionExecution`: **0 Firmware**, **1 Agent** (desktop action run by the Studio agent), **2 Integration** (Codex/Claude). The id is FNV-1a-32 of the lower-cased action id, as bnk8 assumes.

### Names Studio uses (differences from bnk8)

The firmware ignores the id for kind 0, so these don't change what the keypad does. They matter for matching Studio-written configs and for showing the same names Studio does.

| Function | Studio id | bnk8 name today |
|---|---|---|
| Arrow keys | `keyboard.arrow_up/down/left/right` | `keyboard.up/down/left/right` |
| `.` key | `keyboard.period` | `keyboard.dot` |
| Modifiers | `modifier.left_control`, `modifier.left_shift`, `modifier.left_alt`, `modifier.left_gui` (+ `right_*`) | `keyboard.left_ctrl`, … |
| Go to layer | `layer.1` … `layer.10` (1-based; shown as "Layer 0…9") | `layer.0` … `layer.9` |
| Macro | `macro.1` … `macro.15` (1-based) | `macro.0` … `macro.14` |
| Previous effect | `rgb.effect.previous` | `rgb.effect.prev` |
| Numpad clear | `keyboard.numpad.clear` (param 156) | missing |

### Params bnk8 doesn't name yet

| Param | Studio id | Meaning |
|---|---|---|
| `0x2300 + n−1` | `layer.mo.n` (n = 1…10) | Momentary layer while held (Tap gesture only) |
| `0x2310 + n−1` | `layer.tg.n` | Toggle layer on/off |
| `0x2200 + n−1` | `wireless.btn` (n = 1…3) | Bluetooth slot (wireless products) |
| `0x600D` | `rgb.restore` | Restore saved lighting |
| `0x600E` | `rgb.effect.solid` | Solid colour |
| `0x600F` | `rgb.effect.breathing` | Breathing |
| `0x6010` | `rgb.effect.wave` | Wave |
| `0x6011` | `rgb.effect.reactive` | Light on keypress |
| `0x6012` | `rgb.effect.rainbow` | Rainbow (Studio offers it on BNK9/KnobX1 only) |
| `0x1101…0x1116` | `system.*` (Windows) | Native desktop shortcuts, kind 1 |
| `0x1204…0x1217` | `system.*` (macOS) | Native desktop shortcuts, kind 1 |

Native desktop params, kind 1 [Core/…Models/NativeDesktopActions]: rewind `0x1101`, fast-forward `0x1102`, stop `0x1103` (Windows only); browser back/forward/refresh `0x1104–06` (Win) / `0x1204–06` (Mac); browser home `0x1107` (Win); search `0x1108`/`0x1208`; copy, paste, cut, undo, redo, select-all, find `0x1109–0F` / `0x1209–0F`; calculator `0x1110`, maximize `0x1111`, snap left/right `0x1113–14` (Win); next window `0x1112`/`0x1212`; lock `0x1115`/`0x1215`; screenshot `0x1116`/`0x1216`; minimize `0x1217` (Mac). Actions without a native param are sent with param 0 and need the agent.

The kind-1 ids `PROTOCOL.md` found hard-coded in the firmware resolve to `system.edit.undo` (`0xA3B315DA`), `system.window.next` (`0x4D19FA85`) and `system.edit.select-all` (`0x38895B3E`). `0xA3B2C9BE` doesn't match any id in Studio.

### The `raw.*` entries in `configs/omp.toml`

| raw | Action |
|---|---|
| `raw.3f4d5904000002` | `claude.code.session.previous` |
| `raw.67c88f2d000002` | `claude.code.session.next` |
| `raw.bd7302e6000002` | `claude.chat.new` |
| `raw.f1db1315000002` | `claude.fast.toggle` |
| `raw.165687f0000002` | `claude.code.session.fork` |
| `raw.403d6e25000002` | `claude.reject` |
| `raw.0f6407ea000002` | `claude.voice` |
| `raw.dfddf9e8000002` | `claude.accept` |
| `raw.6bbc48b6000001` | `system.media.rewind` (kind 1, param 0 = macOS) |
| `raw.bec97270000001` | `system.media.fast-forward` |

## 4. Native config (2000 bytes) — fields now named

| Bytes | Studio writes |
|---|---|
| 9, 10, 11 | `15`, `0`, `180` (constants for BNK8) |
| 0x0C–0x0D ("timing A") | `180` (constant) |
| Action table order | Sorted by action id, then param |
| Macro step mode | 0 Tap, 1 KeyDown, 2 KeyUp; delay byte = `(ms + 5) / 10` |
| Lighting byte 3 (flags) | bit 0 = effect on (`effect ≠ 0`), bit 1 = per-key colours present, bit 2 = shared lighting across layers |
| Lighting bytes 8..39 | 8 records of `[enabled, R, G, B]` for K1…K8 (Studio's `key.0.0, key.0.1, key.1.0, key.1.1, key.1.2, key.2.0, key.2.1, key.2.2`) |

Effect ids and names (byte 0): 0 Off, 1 Solid, 3 Gradient Up Down, 4 Gradient Left Right, 5 Breathing, 12 Cycle All, 13 Wave, 14 Cycle Up Down, 15 Rainbow Chevron, 16 Cycle Out In, 21 Rainbow Beacon, 24 Raindrops, 26 Hue Breathing, 28 Hue Wave, 34 Reactive Simple, 35 Reactive, 36 Reactive Wide, 38 Reactive Cross, 42 Splash, 43 Multisplash, 44 Solid Splash. This is exactly the firmware's effect cycle list.

Factory images: Studio ships the current BNK8 AI factory config (CRC `0xCA2BFF5F`, the same as `assets/factory-config.bin`) and those of firmware 6.0.24 (`0xEBD4C372`), 6.0.21 (`0x9AC52986`), 6.0.19 (`0x0FB1AEA0`) and 6.0.18 (`0xE1F4F4CE`). They differ only in layer colours, except 6.0.18, which has no Media layer bindings. Like bnk8, Studio recognises a factory config by CRC.

## 5. On-device profile store (`0x15`) — read-back

Studio's `0x15` is **PortableProfile**: a flash store holding `[2000-byte native config][profile archive]`. Studio saves through it, and the firmware activates the native part after the save. It is also how Studio reads a layout back. [Protocol/…/UsbPortableStoreTransfer]

- **Geometry (BNK8):** native 2000 bytes; archive capacity 6160 bytes (`0x1810`).
- **Status (sub 0)**, reply 22 bytes: `[02][phase][revision u32][native len u16][archive len u16][capacity u16][received u16][pending archive len u16][payload CRC32 u32][00 00]`. The payload CRC covers native + archive.
- **Phases:** 0 ScanHeader, 1 ScanBody, **2 Idle**, 3 Erase, 4 Receive, 5 Verify, 6 Publish, 7 Confirm, 8 Failed, 9 ScanEmpty, 10 Activating, 11 ActivationFailed. These are the "states" in `PROTOCOL.md`; its finalize blocker is Verify → Failed.
- **Read (sub 1):** `[01][revision u32][offset u16][count ≤ 22]`, offsets over native + archive.
- **Save:**
  - sub 2 `[02][expected revision u32][archive len u16][payload CRC32 u32]` begins a save.
  - sub 3 `[03][offset u16][≤ 16 bytes]` writes data, never crossing a 64-byte boundary.
  - sub 8 `[08][start u16][len u16]` copies unchanged 64-byte blocks from the previous revision.
  - Studio then polls status until Idle with `revision + 1`.
- **Cancel or rescan (sub 4):** `[04][revision u32]`. Studio sends it once when the phase is Failed; if it still fails, Studio tells the user to reconnect, "do not factory reset".
- **Archive format:**
  - The archive begins with a 48-byte header: `"BPP1"`, version `1` (u32), JSON length (i32), compressed length (i32), then the SHA-256 of the JSON.
  - After the header comes the Brotli-compressed JSON `{format: 1, deviceId, profile}`. `profile` is Studio's DeviceProfile, which includes layer names, application bindings and per-key colours.
  - `"BPP2"` is a delta format used for incremental saves.

**Live check (read-only, this BNK8):**
- Status shows phase Idle, revision 4, and an archive of 912 bytes.
- Reading 2,912 bytes with sub 1 returned a native config with CRC `0xCA0AFB3A`. That equals the device's `0x10` token B, so it is exactly the active config.
- The archive is valid `BPP1`. It decodes to Studio's profile, saved 2026-11-20 13:15 UTC: the factory layout with layer 0's colour changed to `#560000`.

This is why `bnk8 info` reported the config as unknown: Studio had saved it.

For `PROTOCOL.md`'s finalize blocker this suggests a lead `[INFERENCE]`: the firmware probably checks the archive header during Verify. A test that streamed a payload without a valid `BPP1` archive would then fail exactly as recorded.

## 6. Messages Studio uses that `PROTOCOL.md` names differently

| msg | Studio name | Notes |
|---|---|---|
| `0x02` | Capabilities | |
| `0x03` | Ping | `PROTOCOL.md`: echo |
| `0x10` | ReadConfiguration (status) | Reply `[generation u32][length u16][CRC32 u32][active bank u8][migrated u8]`: token A = config generation, token B = config CRC |
| `0x11`, `0x12` | WriteConfiguration, CommitConfiguration | Begin also has a **PatchBegin** form (`ConfigurationOperation`) |
| `0x13` | ActivateLayer | `[layer]`; the agent uses it for per-app layers |
| `0x14` | ReadLightingState | `[layer]` (decoded for BNK16) |
| `0x15` | PortableProfile | §5 |
| `0x16` | FastRgbPreview | BNK16 |
| `0x20` | ControlEvent | 11 bytes: `[layer][control index][gesture 0–5][pressed][kind 0–2][action id u32][param u16]`. The agent runs kind 1/2 actions from this. |
| `0x21` | DeviceStateEvent | `[active layer]…`; wireless products add battery/connection bytes |
| `0x22`, `0x23` | DisplayState, OledFrame | OLED products |
| `0x30` | IntegrationMessage | AI feedback lighting, sub-type in byte 0 (below) |
| `0x31` | NativeMidi | BNK9/KnobX1 |
| `0x40` | EnterBootloader | `PROTOCOL.md`: "session auth". Stage 2 = challenge + `0xB007` → bootloader for firmware update |
| `0x41` | FirmwareHealth | Sub-commands for version/UID/diagnostics |
| `0x42` | FactoryReset | `PROTOCOL.md`: "firmware update". Stage 2 = challenge + `0xFA37`. Studio offers it as the user-facing Factory Reset (USB only), then clears its cached store state |

Studio's `0x42` handling suggests it restores the factory layout rather than bricking the device. `PROTOCOL.md`'s reading of the firmware says it rewrites application flash. Until that is resolved, bnk8 keeps refusing to send it.

Hello (`0x01`) reply: `[nonce 8][product code hash u32][hardware rev][fw major][fw minor][fw build][capability bits u32][min protocol][max protocol]`. `PROTOCOL.md` calls the hash a serial hash `[INFERENCE]`.

Frame flag bits: 1 Response, 2 Error, 4 MoreFragments, 8 AcknowledgementRequired. Error codes: 1 InvalidCommand, 2 InvalidPayload, 3 Busy, 4 Conflict, 5 InvalidConfiguration, 6 StorageFailure, 7 Unauthorized, 8 Unsupported.

### `0x30` AI feedback (BNK8 firmware 6.0.25+ gets a brightness wrapper)

| Byte 0 | Purpose | Layout |
|---|---|---|
| `0xC0` | Codex approval choices | `[C0 01 choices(3 bits) lease]`; wave: `[C0 02 colour 1–2 id u32]` |
| `0xC2` | Codex feedback | `[C2 01 flags pattern lease]` |
| `0xC3` | Claude permission alert | `[C3 01 flags(lights, haptic, …) pattern choices layer token u32 allow-mask u16 reject-mask u16 always-mask u16]` |
| `0xC4` | Attention alert | `[C4 01 pending pattern lease]` |
| `0xC5` | Task notification | `[C5 01 layer pattern amber-mask u16 green-mask u16 vibration u32]` |
| `0xC6` | Claude feedback | `[C6 01 flags 00 lease]` |
| `0xC7` | BNK8 brightness wrapper | `[C7 01 level ≤ 180 <inner>]` or `[C7 02 8 per-key levels <C5 message>]` |

Key masks are bit-per-key; "lease" = 3 s. `PROTOCOL.md` reads `0x30` as RGB control and gives these sub-types different field names.

## 7. Firmware update and factory reset

- **Firmware update:** Studio knows BNK8 firmware 6.0.14 … 6.0.27 by size and SHA-256. It enters the STM32 bootloader with `0x40` and copies `firmware.bin` to the USB volume the bootloader mounts.
- **Factory reset:** `0x42` with challenge + `0xFA37`. Studio requires a wired connection.
- **Reset defaults:** for firmware 6.0.17–6.0.25, Studio's "reset defaults" re-saves the factory profile through the profile store once it recognises the factory config.

## 8. Gaps in bnk8, most valuable first

1. **Read-back through the profile store.** `info`, `dump` and the GUI could load the exact config Studio (or bnk8) saved, instead of "unknown". This was proven read-only on this device.
2. **Show Studio's layer names and per-key colours** from the archive. Optionally write bnk8 configs back through the store so Studio stays in sync.
3. **Per-key colours** in the TOML and GUI (lighting bytes 8..39, flag bit 1) and the **shared lighting** flag (bit 2).
4. **Name the remaining actions:**
   - `layer.mo.N`, `layer.tg.N`, `rgb.restore`, the `rgb.effect.*` presets and `keyboard.numpad.clear`.
   - The `claude.*`, `codex.*` and `system.*` ids, so configs show names instead of `raw.*`.
5. **Align names with Studio:** arrows, period, modifiers, 1-based `layer.N`/`macro.N`, `rgb.effect.previous`. This breaks existing bnk8 TOML files, so it needs a migration or aliases.
6. **Effect names** in the GUI and TOML instead of bare numbers.
7. **Recognise older factory configs** (6.0.18 … 6.0.24) by CRC.
8. **Correct `PROTOCOL.md`'s message names** and the `0x20` layout; record the `0x40`/`0x42` discrepancy.
9. **Host-side features are out of scope for the firmware.** Desktop/AI actions, per-app layers and AI feedback lighting need a running agent; bnk8 would need its own daemon to offer them.
