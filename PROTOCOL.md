# BNK8 AI Edition — config-channel protocol

Reverse-engineered from `BNK8-AI-6.0.27-FACTORY-APPLICATION.bin`
(sha256 `effcacba…232e`, `binepad/binepad-updates` release
`bnk8-6.0.27-factory-record`), ARM Cortex-M Thumb image at `0x08000000`,
and verified against the live device (fw 6.0.0 reported by USB bcdDevice,
firmware reports `6.0.27` internally — see msg `0x01`).

Status: frame format and command table **confirmed in firmware**;
config-blob semantics partially mapped; items marked `[LIVE]` were also
verified on the device.

## Transport

USB HID interface 1: usage page `0xFF42`, usage `0x49`, 32-byte input and
output reports, no report ID. Write = 33 bytes (`0x00` report-ID prefix +
32-byte frame). The device is chatty: see "Async frames" — a reader must
match replies by msg_type/seq and skip async frames.

Replies arrive within 1 ms. `[LIVE]`

## Frame layout (32 bytes)

| Offset | Size | Meaning |
|---|---|---|
| 0..1 | 2 | magic `B1 5D` |
| 2 | 1 | protocol version = `0x01` |
| 3 | 1 | reserved, `0x00` |
| 4 | 1 | msg_type |
| 5 | 1 | status: `0` = request/async, `1` = ACK, `3` = NAK |
| 6 | 1 | sequence, echoed from request |
| 7 | 1 | payload length `n` (0..22) |
| 8..29 | n | payload |
| 30..31 | 2 | CRC-16/CCITT-FALSE over bytes 0..29, big-endian |

All offsets confirmed in the dispatcher: it reads only `[4]`, `[6]`, `[7]`;
the reply builder (`FUN_08002258`) sets `[5] = 1` and the NAK path
(`FUN_080022b0`) ORs `[5] |= 2`, yielding `3`. Requests may put anything
in `[3]`, `[5]` (we send `0`). `[LIVE]` on all points.

CRC: poly `0x1021`, init `0xFFFF`, no reflection, xorout `0`, stored BE
(the firmware's table at `0x080096F0` is the standard CCITT-FALSE table).

## Async frames (unsolicited, `[5] = 0`)

- **`0x21` heartbeat** — payload 2 bytes: `[current_layer, flag]`.
  Period is 2000 ms when idle; for ~3000 ms after any valid command it
  drops to 40 ms, then returns to 2000 ms. `[LIVE]` — observed
  `b15d0100210000020001…d2d4` at 40 ms spacing after commands.
- **`0x20` key event** — payload 11 bytes: `[layer][control index]
  [gesture 0–5][pressed][kind 0–2][action id u32][param u16]`
  (offsets 8..18). Sent on physical key/knob events; Studio's agent runs
  kind 1/2 actions from it and its key tester shows it. Layout from
  Binepad Studio's decoder (`docs/STUDIO.md` §6).

## Commands (`[4]` msg_type, replies echo it)

NAK = reply with `[5] = 3`, payload = 1 byte error code:

| Code | Meaning (from dispatch sites) |
|---|---|
| 1 | unknown msg_type |
| 2 | bad length/parameter |
| 3 | bad state (busy/session), or commit validation failure |
| 4 | bad token/session id |
| 5 | bad CRC or incomplete chunk bitmap |
| 6 | stream: read/write failure |
| 7 | auth challenge mismatch or timeout |
| 8 | (0x22) always NAK'd — command retired |

### `0x01` hello — len 8 `[LIVE]`
Payload: 8 arbitrary bytes, echoed. Reply len 22:
`[echo 8][serial_hash u32][01 06 00 19][0100027f][01 01]`.
`serial_hash` = FNV-style hash of the 12-char USB serial (`FUN_08000664`).
The version byte is `0x19` = 25 on the tested device (fw 6.0.25); the
published factory image is 6.0.27 and emits `0x1b`. USB bcdDevice says
`6.0.0` regardless.

### `0x03` echo — any len `[LIVE]`
Reply echoes msg_type `0x03` with the same payload. Ping.

### `0x10` status — len 0 `[LIVE]`
Reply len 12: `[tokenA u32][07 d0][tokenB u32][b1][b2]`.
Observed: tokenA `0x00000001`, tokenB `0xCA2BFF5F` (per-device).
`0x07D0` = 2000 = config blob size. The tokens gate the `0x11`
transfer session (tokenA) and the `0x11` sub-4 read session (both).

### `0x11` transfer session — sub-command in payload[0] `[LIVE end-to-end 2026-11-17]`

Session state lives in a 0x8E0-byte struct: active flag, timestamps,
tokenA copy, expected CRC32, declared size, session id, then the
2000-byte buffer at `+0x16` and a 250-byte chunk bitmap at `+0x7E6`.

**Gate:** begins (sub 0/3/4) are NAK 3 while the `0x15` stream manager
is not idle. Verified: with stream state 8, all begins NAK 3; after
`0x15` sub 4 reset to state 2, sub 0 begin succeeds. Always check
`0x15` sub 0 status first and reset if `state != 2`.

- **sub 0** (begin fresh) / **sub 3** (begin modify), len 11:
  `[sub][tokenA u32][size u16=2000][crc32 u32]`.
  `tokenA` must equal the value from msg `0x10`; `size` must be 2000.
  Sub 3 additionally preloads the buffer with the current config and
  marks the whole bitmap received (read-modify-write with partial
  chunks). Reply len 4: `[session_id u32]` where
  `id = 0xB15D0300 ^ (now_ms ^ tokenA)`, or `1` if that xor equals the
  magic. `[LIVE: observed ids 0xB5428650 / 0xB542CE1E, Δtick 18.4 s ≈ Δwall]`
- **sub 1** (write chunk), len 7..22: `[1][session_id u32][offset u16][data…]`.
  Max 15 data bytes per chunk (134 chunks for 2000 bytes). Bitmap bits
  are set per byte. Reply len 0 (ACK). `[LIVE: 134 chunks accepted]`
- **sub 2** (abort), len 5: `[2][session_id u32]`. Clears session. ACK.
- **sub 4** (begin RAM-only "apply/preview" session), len 15:
  `[4][tokenA u32][tokenB u32][size u16=2000][crc32 u32]`.
  Requires no active session, both channel-state checks, and the
  **stream token ≠ 0** (a valid `0x15` stream slot must exist).
  Preloads the buffer with the current config, marks the bitmap full,
  replies `[session_id u32]`. A subsequent `0x12` commit on this session
  applies the buffer to the active config in RAM **without writing
  flash** (see below). This is the safe way to test a config — and the
  only write path that cannot destroy the stored config.
  `[BLOCKED on the test unit: stream slot invalid — see 0x15 finalize blocker]`

### `0x12` commit — len 4
`[session_id u32]`. Validates: session token, complete chunk bitmap,
CRC32 (ISO-HDLC, poly `0xEDB88320`, via `FUN_08000630`) of the
2000-byte buffer equal to the `crc32` given at begin, and the structural
validation `FUN_080029c0`. Then:

- **write session (sub 0/3):** copies buffer → active config RAM, writes
  both flash record slots (see "Persistence"), clears the session.
- **RAM-only session (sub 4):** same RAM copy plus runtime profile
  rebuild, but **no flash write**. Refuses while the matrix reports
  activity (NAK 3) — don't touch keys during apply.

Reply len 12 (same layout as `0x10`). Failures: NAK 2 (bad length),
NAK 4 (bad session/token), NAK 5 (bad CRC/incomplete bitmap),
NAK 3 (validation). `[LIVE 2026-11-17: factory-config commit accepted;
reply [02 000000 d007 <config crc> 0100]; tokenA incremented 1→2 and
tokenB became the config CRC 0xCA2BFF5F, matching the persistence
record layout below]`

### `0x13` activate layer — len 1
`[layer]`; must be `< config[5]` (layer count). Sets the active-layer
bitmask. Reply echoes payload. Studio's agent uses it to switch layers
per application. `[LIVE]`

### `0x14` read lighting state — len 1
`[index]`; reply len 12 built from the 0x16-byte runtime profile entry
plus two config bytes at `0x5E7/0x5E8 + index*0x28` (per-profile
metadata block). Profile metadata only — **not** the keymap.
`[LIVE: profile 0 of the factory config replies 00 01 50 78 FF FF FF 00 00 00 00 50]`

### `0x15` portable profile store (flash stream area) — sub in payload[0]

Binepad Studio's name: PortableProfile (`docs/STUDIO.md` §5). A flash
store holding `[2000-byte native config][profile archive]`; Studio saves
through it (the firmware activates the native part after the save) and
reads layouts back from it.

Two 0x2000-byte slots at flash `0x08018020`, each with a 0x20-byte
`"BPP1"` header (magic, revision, 2000, archive length, CRC32, header
CRC). Archive length `∈ [0x30, 0x1810]`; total stream = 2000 + archive
length, so a slot is `[2000-byte native config][archive]`.

**Phases** (byte 1 of the status reply): 0 ScanHeader, 1 ScanBody,
**2 Idle**, 3 Erase, 4 Receive, 5 Verify, 6 Publish, 7 Confirm,
8 Failed, 9 ScanEmpty, 10 Activating, 11 ActivationFailed.

**Archive** (after the native config): a 48-byte header — `"BPP1"`,
version `1` (u32), JSON length (i32), compressed length (i32), SHA-256
of the JSON — then the Brotli-compressed JSON
`{format: 1, deviceId, profile}`, where `profile` is Studio's device
profile (layer names, application bindings, per-key colours). `"BPP2"`
is a delta format Studio uses for incremental saves.

- **sub 5** (device ID), len 1: `[05]`. Reply len 22:
  `[03][serial_hash u32][UID 12][d0 07][10 18][01]` — the 12-byte STM32
  unique ID equals the USB serial prefix; `0x07D0` = 2000,
  `0x1810` = archive capacity. Works unconditionally. `[LIVE]`
- **sub 0** (status), len 1: `[00]`. Reply len 22:
  `[02][phase][revision u32][native len u16][archive len u16]
  [capacity u16][received u16][pending archive len u16]
  [payload CRC32 u32][00 00]`. The payload CRC covers native + archive.
  Revision 0 = no valid slot. `[LIVE: phase 2, revision 4, archive 912
  on a Studio-saved device]`
- **sub 1** (read), len 8: `[1][revision u32][offset u16][count u8]`,
  count 1..0x16, offsets over native + archive. Requires phase 2, a
  valid slot and a revision match. Reply len = count with the requested
  bytes from `0x08018020 + slot*0x2000 + offset`. NAK 2 when no valid
  slot exists. `[LIVE: 2912 bytes read; the native part's CRC equals
  0x10 token B]`
- **sub 2** (begin save), len 11:
  `[2][expected revision u32][archive len u16][payload CRC32 u32]`;
  archive length in `[0x30, 0x1810]`; the revision must equal the
  current one (0 when no valid slot) and not be `0xFFFFFFFF`. New
  revision = old + 1. Phase 2 → 3. `[LIVE: accepted, phase 3 → 4]`
- **sub 8** (keep blocks), len 5: `[8][start u16][len u16]`; both
  64-aligned; phase 4. Copies unchanged 64-byte blocks from the previous
  revision (the manager prefills staging from flash and writes it
  back); not needed for an all-data write.
- **sub 3** (write data), len 4..19: `[3][offset u16][≤ 16 bytes]` into
  a 0x40-byte staging buffer, never crossing a 64-byte boundary;
  `offset` must equal the running received count; phase 4 (also
  accepted in 8). A background task (`FUN_08002D24`) programs 0x40-byte
  blocks to flash and drives the phases (2→3→4→5→7→2, or →8 on error).
  Studio then polls status until Idle with `revision + 1`.
  `[LIVE: 2048/2048 bytes accepted, phase 4 → 5]`
- **sub 4** (cancel/rescan), len 5: `[4][revision u32]`; phase 3..6 → 2,
  phase 7/8 → full reset (memsets the stream session, marks slots
  invalid). Studio sends it once when the phase is Failed.
  `[LIVE: recovers phase 8 → 2]`

**Finalize blocker `[BLOCKED on the test unit]`:** after all data is
accepted (phase 5, Verify), the manager writes the 0x20 `"BPP1"` slot
header, then a 0x10-byte `"BPOW"` directory record to flash page
`0x0801C000`, then validates readback. On the test unit the transition
5 → 8 (Verify → Failed) is deterministic. The flash writer
(`FUN_08002B68`) refuses any 0 → 1 bit transition (`src & ~dst == 0`
per byte), so one suspect is a `0x0801C000` page that holds unexpected
data and is **not** coverable by any protocol erase (the stream erase
spans only `[slot_base, slot_base+0x2000)`; the directory lies exactly
one past the end). Another `[INFERENCE]`: the firmware checks the
archive header during Verify, and the test payload had no valid `BPP1`
archive. Studio saves through this store successfully, so the store
itself works. Boot recovery paths exist (`+0x84` sentinels
`0xFFFF`/`0xFFFE`) but require the same page to be blank/valid first.

### `0x22` retired — always NAK 8.

### `0x30` integration message (AI feedback lighting) — sub in payload[0]

Handlers in `FUN_080033D8`; ACK is reply `0x30` len 0. Studio's agent
sends these to light keys for AI feedback (`docs/STUDIO.md` §6):

| Byte 0 | Purpose | Layout (Studio) |
|---|---|---|
| `0xC0` | Codex approval choices | `[C0 01 choices(3 bits) lease]`; wave: `[C0 02 colour 1–2 id u32]` |
| `0xC2` | Codex feedback | `[C2 01 flags pattern lease]` |
| `0xC3` | Claude permission alert | `[C3 01 flags pattern choices layer token u32 allow-mask u16 reject-mask u16 always-mask u16]` |
| `0xC4` | Attention alert | `[C4 01 pending pattern lease]` |
| `0xC5` | Task notification | `[C5 01 layer pattern amber-mask u16 green-mask u16 vibration u32]` |
| `0xC6` | Claude feedback | `[C6 01 flags 00 lease]` |
| `0xC7` | Brightness wrapper | `[C7 01 level ≤ 180 <inner>]` or `[C7 02 8 per-key levels <C5 message>]`; handled in the dispatcher |

Key masks are bit-per-key; "lease" = 3 s. Firmware length checks:
`C0` len 4 or 7, `C3` len 16, `C4` len 5, `C5` len 12, `C6` len 5,
`C7` len > 6.

### `0x40` enter bootloader — stage 1 len 0
Reply len 6: `[challenge u32][u16 3000]`.
Stage 2 len 6: `[challenge u32][07 b0]` (echo challenge, append `0xB007`)
within 3000 ms → ACK len 0. Binepad Studio uses it to enter the STM32
bootloader for a firmware update (`docs/STUDIO.md` §7); the firmware
analysis saw only a session-unlock flag being set.

### `0x41` device info — len 0 `[LIVE]`
Reply len 16: `[06001901][tokenA u32][tokenB u32][00 00 01 00]`
(observed). The first word is `0x01 1b 00 06` in fw 6.0.27; `0x19` = 25
on the tested 6.0.25 device. tokenA/tokenB match msg `0x10`.

### `0x42` factory reset (Studio); flash rewrite per firmware analysis, unresolved — ⚠ DANGEROUS
Stage 1 len 0 → reply len 6 `[challenge u32][u16 5000]`.
Stage 2 len 6 `[challenge u32][37 fa]` within 5000 ms. The firmware
analysis reads this as **erasing and rewriting application flash**.
Binepad Studio sends exactly this sequence (challenge + `0xFA37`) as its
user-facing Factory Reset over USB, which suggests it restores the
factory layout instead. Until that is resolved, bnk8 refuses to send
it. Never send.

## Config blob (2000 bytes, `FUN_080029c0` validation)

All layer/macro regions are validated for **every** slot (10 layers, 15
macros), not just the active count, so unused slots must stay in range
(bnk8 zeroes them).

| Offset | Constraint | Meaning |
|---|---|---|
| 0..3 | must equal `42 50 4E 38` `"BPN8"` (flash `0x0800B90D`) | blob magic — SOLVED, see below |
| 4 | `1` | version |
| 5 | 1..10 | layer count (Studio vocabulary: "layer"; firmware: profile) |
| 6 | `9` | matrix positions (8 keys + knob) |
| 7 | `6` | knob binding slots |
| 8 | ≤ 0x60 | action-table entry count |
| 9..0xB | — | factory `0F 00 B4`; unmodelled, preserved |
| 0xC..0xD | u16, 50..500 | timing A (factory 180), use not traced |
| 0xE..0xF | u16, 100..800 | double-tap window, ms (factory 260) |
| 0x10..0x11 | u16, 150..2000 | hold threshold, ms (factory 350) |
| 0x14..0x17 | CRC32 (ISO-HDLC) of bytes 0x18..0x7CF | integrity |
| 0x18 + i*7 | byte 6 ≤ 2 | action table, 96 entries max (see below) |
| 0x2B8 + p*0x26 | every byte ≤ action count | keymap of layer p (10 slots) |
| 0x434 + m*0x1D | byte 0 ≤ 7 | macro m, 15 slots (see below) |
| 0x5E7 + p*0x28 | byte 0 ≤ 0x32, byte 1 ≤ 0xB4 | lighting of layer p (10 slots) |
| 0x777..0x7CF | — | zero in factory; unmodelled, preserved |

### Keymap (38 bytes per layer)

Each byte is a **1-based index into the action table**; `0` = unbound.
Lookup `FUN_08000790(layer, position, slot)`.

| Offset | Position | Slots |
|---|---|---|
| 0, 4 | K1, K2 (matrix 0, 1) | 4 |
| 8..13 | knob (matrix 2) | 6 |
| 14, 18, 22, 26, 30, 34 | K3..K8 (matrix 3..8) | 4 |

Slot semantics, from the key state machine (`0x08005144..`, timer
`0x08007CE0..`):

| Slot | Name | Fires |
|---|---|---|
| 0 | tap | on press when slots 1..3 are empty, else after the double-tap window expires |
| 1 | double_tap | second press within the double-tap window |
| 2 | hold | key held past the hold threshold; released with the key |
| 3 | tap_hold | tap, then press-and-hold |
| 4 | ccw (knob) | encoder step, direction bit 0 |
| 5 | cw (knob) | encoder step, direction bit 1 |

Slot 0/1/4/5 keyboard actions are *taps* (press + release); slots 2/3
press on fire and release with the key. If all four key slots are empty
the firmware falls back to a built-in default key.

### Action table entry (7 bytes)

`[id u32][param u16][kind u8]`, all LE. `id` = **FNV-1a-32 of Studio's
dotted action name** (confirmed: `media.volume_up/volume_down/mute/
play_pause/next/previous`, `layer.next`).

- **kind 0 — device action**, dispatched on `param` only (`id` ignored):

  | param | Action | Names used by bnk8 |
  |---|---|---|
  | `0x0004..0x00A4`, `0x00E0..0x00E7` | QMK basic keycode / HID usage | `keyboard.a`, `keyboard.left_ctrl`, `keyboard.numpad.7`… |
  | `0x1001..0x1006` | media via table `A9 AA A8 AE AB AC` | `media.volume_up`, `volume_down`, `mute`, `play_pause`, `next`, `previous` |
  | `0x2001` / `0x2002` | next / previous layer | `layer.next` (Studio name), `layer.previous` `[INFERENCE: name]` |
  | `0x2100 \| n` | select layer n | `layer.N` |
  | `0x2300 \| n` .. `0x2350 \| n` | layer momentary/toggle variants | unnamed (`raw.`) |
  | `0x3000 \| n` | play macro n (n < 15) | `macro.N` |
  | `0x4000..0x4012` | numeric accumulator (×10 + digit), use unknown | unnamed |
  | `0x6000` / `01` / `02` | lighting toggle / on / off | `rgb.toggle`, `rgb.on`, `rgb.off` |
  | `0x6003` / `04` | brightness ±13 (cap 180) | `rgb.brightness.up/down` |
  | `0x6005` / `06` | hue shift ±8 `[INFERENCE]` | `rgb.hue.up/down` |
  | `0x6007` / `08` | saturation ±13 `[INFERENCE]` | `rgb.saturation.up/down` |
  | `0x6009` / `0A` | next / previous effect in the cycle list | `rgb.effect.next/prev` |
  | `0x600B` / `0C` | speed ±13 | `rgb.speed.up/down` |
  | `0x600D`..`0x6012` | lighting reset / preset effects | unnamed |

- **kind 1** — dispatched on `id` against firmware constants
  (`0xA3B315DA`, `0xA3B2C9BE`, `0x4D19FA85`, `0x38895B3E`, …; names
  not recovered); unmatched ids are forwarded to the host like kind 2.
- **kind 2 — host action**: forwarded to Studio as a `0x20` async
  frame (`FUN_08002160`) carrying `[id][param][kind]`. The factory
  layers 0/1 bind kind-2 "AI" actions that only do something while
  Studio runs.

bnk8 names kind-0 entries whose `id` equals the FNV of its name and
writes every other entry as `raw.<14 hex>`, byte-for-byte.

### Macro (29 bytes)

`[step count 0..7][7 × step]`, step = `[mode][code u16][delay]`:
`mode` 1 = press, 2 = release, other = tap; `code` = keycode
(`< 0x1000`) or media `0x1001..0x1006`; `delay` × 10 ms elapses
**before** the step (player `FUN_080044D4`). Played by action
`0x3000 | n`; keys still held by a previous macro are released first.

### Lighting (40 bytes per layer)

| Byte | Meaning |
|---|---|
| 0 | effect id, 0 = off, ≤ 50 (renderer handles ≤ 44); cycle list: 1 3 4 5 12 13 14 15 16 21 24 26 28 34 35 36 38 42 43 44 |
| 1 | brightness, ≤ 180 |
| 2 | speed: animation period = (375 − speed) × 8 ms |
| 3 | flags (factory `01`; bit 2 checked by `FUN_080007EC`) |
| 4..6 | colour R, G, B |
| 7..39 | zero in factory; renderer reads 4-byte groups from 8 `[INFERENCE: per-LED overrides]` |

## Persistence

`0x12` commit (write session) builds a 0x7E0-byte record —
`[counter u32][crc32(config)][00][slot][config 2000][crc32(record)]` —
and writes it to flash page `0x0801E000`, mirrored in RAM at
`0x20001100` (EEPROM-emulation layer: read `FUN_080015E8`, write
`FUN_080019D8` with per-word change detection and page rotation).
Two records at page offsets `0x25` and `0x805`; the boot loader picks
the valid record with the higher counter. A 1-byte commit marker is
written after the data so interrupted writes fail validation.

Boot: if neither record validates, the firmware copies a **default
config** from the factory data page (`FUN_080007D4`: 2000 bytes from
`0x0800BC97` → RAM `0x20002EE4`) and commits it as a fresh record.

## Config magic — SOLVED 2026-11-17

The validator memcpys blob `[0..4]` against the 4 bytes at runtime flash
`0x0800B90D` (factory data page). The published app image
**embeds a copy of that data page at file offset `0x98F0`**, mirrored
at runtime `+0x2000`:

| File offset | Runtime flash | Content |
|---|---|---|
| `0x98F0` | `0x0800B8F0` | 16-byte `"BPOW"` stream directory record (`BPOW 01000000 BD AF B0 A8 FE FF FF FF`) |
| `0x9900` | `0x0800B900` | 12-char USB serial + `BPN8` string constant |
| `0x990D` | `0x0800B90D` | **config magic `42 50 4E 38` = `"BPN8"`** |
| `0x9C6F` | `0x0800BC6F` | second `"BPOW"` record (directory validation table) |
| `0x9C97` | `0x0800BC97` | **factory default config, 2000 bytes** |

The factory default config was extracted and its inner CRC verified
(`[0x14..0x17]` = `0xB149A7F3` = CRC32 of `[0x18..0x7CF]`). Its header:
magic `BPN8`, version 1, 4 layers, `[6]=9 [7]=6`, 27 actions,
`[9..0xB]=0F 00 B4`, timings 180/260/350. It is the repo asset
`assets/factory-config.bin`, embedded in bnk8 for `reset`.

**Verified write, 2026-11-17:** the factory config was persisted via
`0x11` sub 0 + 134 chunks + `0x12` commit → COMMIT ACCEPTED; tokenA
incremented 1 → 2, tokenB = config CRC. The write path is unblocked.
~~Consented probing~~ and ~~waiting for Studio~~ are no longer needed
for the magic itself.

## The read model (CRC-verified snapshot or profile store)

There is **no bulk config-read command**. Every reply in the firmware
funnels through five call sites; the complete set of host-visible reads:

| Command | Yields |
|---|---|
| `0x01` | echo + serial hash + version |
| `0x10` | session tokens + config size |
| `0x41` | device identifiers + tokens |
| `0x14` | 12 bytes of per-profile metadata |
| `0x15` sub 5 / sub 0 | UID, stream status |
| `0x15` sub 1 | bytes from the stream flash area only |

The 2000-byte active config (keymap, macros, RGB) has no direct read
command. The `0x11` sub-4 session preloads it into the session buffer
but offers no read-out. bnk8's own saves through the `0x15` store fail
on the test unit (see the `0x15` finalize blocker), but Binepad Studio
saves through it successfully.

Whenever Binepad Studio saved last, the `0x15` profile store holds a
readable copy of the native config: status phase 2 (Idle), revision
≠ 0, native length 2000, and the first 2000 bytes read with sub 1 have
the CRC of `0x10` token B. `[LIVE]` bnk8 reads the active config back
from there when its own snapshot and the factory config don't match.

**What bnk8 does:** `0x10` token B is the CRC-32 of the active
config (`[LIVE]`: factory blob CRC `0xCA2BFF5F` = token B before and
after the factory write). bnk8 stores every blob it writes at
`$XDG_STATE_HOME/bnk8/<serial>.bin` (default `~/.local/state/bnk8/`).
`dump`, `set` and the `apply` diff use that snapshot, the embedded
factory blob, or the profile store's copy — each only when its CRC
equals token B, i.e. it is provably the device's config. A config that
matches none of them is reported as unknown; `apply` then writes the
whole file over the factory base, and `reset` restores the factory
blob.

Writes are whole-blob: there is no per-item write command, so
`apply`/`set` print per-item diffs but always persist all 2000 bytes.

## Firmware notes

- SoC: Cortex-M, app at `0x08000000` (tested device reports fw 6.0.25,
  published image is 6.0.27; protocol identical on all verified
  commands). Config page `0x0801E000`; stream slots `0x08018020` +
  slot*0x2000; factory data page `0x0800B000+`.
- Timer tick ≈ 1 ms (`FUN_0800334C`), used for all timeouts.
- HID report descriptors in the data page match the observed interfaces
  (`0x0800B9CB`: `06 42 FF 09 49 …` = 32-byte raw HID).
