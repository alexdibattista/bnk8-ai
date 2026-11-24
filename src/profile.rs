//! Binepad Studio's `DeviceProfile` JSON (the profile store archive's
//! `profile`) and its mapping to and from bnk8's [`Config`].
//!
//! Studio treats the archive's profile as the source of truth: its agent
//! recompiles the profile (`Bnk8ProfileCodec.Compile`) and re-saves it when
//! the result differs from the device's active config. So a profile bnk8
//! writes must compile to exactly the native blob bnk8 writes.
//! [`from_profile`] follows Studio's compiler, and the store checks
//! `to_blob(from_profile(to_profile(cfg))) == to_blob(cfg)` before saving.

use std::collections::BTreeMap;

use anyhow::{Context, Result, bail, ensure};
use serde_json::{Map, Value, json};

use crate::actions::{self, MACRO_SLOTS, MAX_LAYERS, fnv1a};
use crate::studio_gestures::RESTRICTED;
use crate::config::{
    self, Blob, Config, DEVICE, EFFECTS, FORMAT, KEY_SLOTS, KEYS, KNOB_SLOTS, Key, Layer, Lighting, Slots, StudioIds,
    Timing,
};

/// Timing Studio's compiler always writes; its profile has no timing settings.
pub const STUDIO_TIMING: Timing = Timing { double_tap_ms: 260, hold_ms: 350 };

/// Studio control ids of K1..K8, and of the knob.
const KEY_IDS: [&str; KEYS] = ["key.0.0", "key.0.1", "key.1.0", "key.1.1", "key.1.2", "key.2.0", "key.2.1", "key.2.2"];
const KNOB_ID: &str = "encoder.main";
/// Studio gestures, in bnk8 slot order ([`KNOB_SLOTS`]; keys use the first four).
const GESTURES: [&str; 6] = ["Tap", "DoubleTap", "Hold", "TapHold", "RotateCounterClockwise", "RotateClockwise"];
const MACRO_EVENTS: [(&str, &str); 3] = [("tap", "Tap"), ("press", "KeyDown"), ("release", "KeyUp")];
const NONE: &str = "none";

/// Studio action references for actions bnk8 can only name as `raw.*`
/// (actions with arguments, desktop macros, ids bnk8 doesn't know), keyed by
/// that raw name. Taken from the previous profile, so they survive an edit.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Aliases {
    refs: BTreeMap<String, Value>,
}

impl Aliases {
    /// The Studio action ids, for ordering the action table.
    pub fn ids(&self) -> StudioIds {
        self.refs
            .iter()
            .filter_map(|(name, r)| Some((name.clone(), r["actionId"].as_str()?.to_string())))
            .collect()
    }
}

/// Studio's effect name for an effect id: bnk8's name in title case.
fn studio_effect(id: u8) -> Option<String> {
    let snake = config::effect_name(id)?;
    let words: Vec<String> = snake
        .split('_')
        .map(|w| w[..1].to_uppercase() + &w[1..])
        .collect();
    Some(words.join(" "))
}

fn effect_from_studio(name: &str) -> Option<u8> {
    EFFECTS.iter().map(|(id, _)| *id).find(|id| studio_effect(*id).as_deref() == Some(name))
}

fn rgb(v: &Value) -> Result<[u8; 3]> {
    let c = |k: &str| v[k].as_u64().and_then(|n| u8::try_from(n).ok()).with_context(|| format!("colour {k} in {v}"));
    Ok([c("red")?, c("green")?, c("blue")?])
}

fn rgb_json(hex: &str) -> Result<Value> {
    let [r, g, b] = config::parse_color(hex).with_context(|| format!("`{hex}` is not #RRGGBB"))?;
    Ok(json!({"red": r, "green": g, "blue": b}))
}

fn str_field<'a>(v: &'a Value, k: &str) -> Result<&'a str> {
    v[k].as_str().with_context(|| format!("profile field `{k}` missing in {v}"))
}

fn index_of(v: &Value) -> Result<usize> {
    v["index"].as_u64().map(|n| n as usize).with_context(|| format!("layer without index: {v}"))
}

/// The bnk8 name Studio's action reference `r` compiles to, if bnk8 can
/// encode it itself: no arguments, not a desktop macro, a canonical bnk8 name.
fn known_name(r: &Value, desktop_macros: &[String]) -> Option<String> {
    let id = r["actionId"].as_str()?;
    let no_args = r.get("arguments").is_none_or(|a| a.is_null() || a.as_object().is_some_and(Map::is_empty));
    if !no_args || desktop_macros.iter().any(|m| m == id) {
        return None;
    }
    let a = actions::encode(id).ok()?;
    (!id.starts_with("raw.") && actions::decode(a) == id).then(|| id.to_string())
}

/// Decodes Studio's profile into a config, as Studio's compiler encodes it.
/// Actions bnk8 can't encode itself are taken from `native` (the device's
/// config compiled from this profile) at the same keymap position, checked by
/// hash, and recorded in the returned [`Aliases`].
pub fn from_profile(p: &Value, native: &Blob) -> Result<(Config, Aliases)> {
    ensure!(p["devicePackId"].as_str() == Some(DEVICE), "profile is not for {DEVICE}");
    let decoded = config::from_blob(native)?;
    let layers = p["layers"].as_array().context("profile has no layers")?;
    let n = layers.len();
    ensure!((1..=MAX_LAYERS).contains(&n), "profile has {n} layers, expected 1..={MAX_LAYERS}");
    ensure!(decoded.layers.len() == n, "profile has {n} layers, the device's config {}", decoded.layers.len());
    let macros = p["macros"].as_array().cloned().unwrap_or_default();
    let desktop_macros: Vec<String> = macros
        .iter()
        .filter(|m| m["storage"] == "Desktop")
        .filter_map(|m| Some(format!("macro.{}", m["slot"].as_u64()? + 1)))
        .collect();

    // Shared lighting: every layer gets layer 0's lighting (Compile).
    let shared = p["sharedRgbAcrossLayers"].as_bool().unwrap_or(false);
    let layer0 = layers.iter().find(|l| l["index"] == 0).context("profile has no layer 0")?;

    let mut aliases = Aliases::default();
    let mut out: Vec<Option<Layer>> = vec![None; n];
    for l in layers {
        let i = index_of(l)?;
        ensure!(i < n && out[i].is_none(), "profile layer indices are not 0..{n}");
        let mut keys = vec![Slots::default(); KEYS];
        let mut knob = Slots::default();
        for a in l["assignments"].as_array().into_iter().flatten() {
            let control = str_field(a, "controlId")?;
            let key = KEY_IDS.iter().position(|k| *k == control);
            ensure!(key.is_some() || control == KNOB_ID, "layer {i}: unknown control `{control}`");
            for g in a["gestures"].as_array().into_iter().flatten() {
                let gesture = str_field(g, "gesture")?;
                let s = GESTURES.iter().position(|x| *x == gesture).with_context(|| format!("unknown gesture `{gesture}`"))?;
                let slot = KNOB_SLOTS[s];
                let r = &g["action"];
                let name = match known_name(r, &desktop_macros) {
                    Some(name) => name,
                    None => {
                        // Opaque to bnk8: the native entry Studio compiled at this position.
                        let at = match key {
                            Some(k) => decoded.layers[i].keys[k].slots(),
                            None => decoded.layers[i].knob.clone(),
                        };
                        let name = at.get(slot).cloned().with_context(|| {
                            format!("layer {i} {control} {gesture}: the device's config has no action there")
                        })?;
                        let id = str_field(r, "actionId")?;
                        ensure!(
                            actions::encode(&name)?.id == fnv1a(&id.to_lowercase()),
                            "layer {i} {control} {gesture}: the device's config holds `{name}`, not Studio's `{id}`"
                        );
                        aliases.refs.insert(name.clone(), r.clone());
                        name
                    }
                };
                let target = match key {
                    Some(k) => {
                        ensure!(s < KEY_SLOTS.len(), "layer {i}: key {control} can't turn");
                        &mut keys[k]
                    }
                    None => &mut knob,
                };
                *target.slot_mut(slot).unwrap() = Some(name);
            }
        }
        let source = if shared { layer0 } else { l };
        let lighting = lighting_from(&source["lighting"], shared).with_context(|| format!("layer {i} lighting"))?;
        let name = l["name"].as_str().filter(|s| !s.is_empty()).map(str::to_string);
        out[i] = Some(Layer { name, keys: keys.into_iter().map(Key::from_slots).collect(), knob, lighting });
    }

    let mut cfg_macros = BTreeMap::new();
    for m in macros.iter().filter(|m| m["storage"] == "OnDevice") {
        let slot = m["slot"].as_u64().map(|s| s as usize).filter(|s| *s < MACRO_SLOTS);
        let slot = slot.with_context(|| format!("macro slot out of range: {m}"))?;
        let steps = m["steps"].as_array().context("macro without steps")?;
        ensure!(steps.len() <= config::MACRO_STEPS, "macro {} has {} steps", slot + 1, steps.len());
        let mut out_steps = Vec::new();
        for s in steps {
            let ms = s["delayMilliseconds"].as_u64().unwrap_or(0);
            ensure!(ms <= 2550, "macro {} delay {ms} ms exceeds 2550", slot + 1);
            let units = (ms + 5) / 10;
            if units > 0 {
                out_steps.push(format!("delay {}", units * 10));
            }
            let event = s["event"].as_str().unwrap_or("Tap");
            let verb = MACRO_EVENTS.iter().find(|(_, e)| *e == event).with_context(|| format!("macro event `{event}`"))?.0;
            let key = str_field(s, "actionId")?;
            let code = actions::encode_key(key).with_context(|| format!("macro {}", slot + 1))?;
            ensure!(
                actions::key_name(code).as_deref() == Some(key),
                "macro {}: `{key}` is not a canonical key name",
                slot + 1
            );
            out_steps.push(format!("{verb} {key}"));
        }
        // An empty macro compiles to an unused slot.
        if !steps.is_empty() {
            cfg_macros.insert(config::macro_key(slot), out_steps);
        }
    }

    let cfg = Config {
        format: FORMAT,
        device: DEVICE.into(),
        timing: STUDIO_TIMING,
        layers: out.into_iter().map(Option::unwrap).collect(),
        macros: cfg_macros,
    };
    Ok((cfg, aliases))
}

fn lighting_from(l: &Value, shared: bool) -> Result<Lighting> {
    if l.is_null() {
        return Ok(Lighting {
            effect: 0,
            brightness: 0,
            speed: 0,
            color: config::rgb_hex(&[0, 0, 0]),
            shared,
            key_colors: None,
            flags: None,
            extra: None,
        });
    }
    let effect = str_field(l, "effect")?;
    let effect = effect_from_studio(effect).with_context(|| format!("unsupported effect `{effect}`"))?;
    let byte = |k: &str| l[k].as_u64().and_then(|n| u8::try_from(n).ok()).with_context(|| format!("lighting `{k}`"));
    let mut colors: Vec<String> = vec![NONE.into(); KEYS];
    for k in l["keys"].as_array().into_iter().flatten() {
        let id = str_field(k, "controlId")?;
        if let Some(i) = KEY_IDS.iter().position(|x| *x == id) {
            colors[i] = config::rgb_hex(&rgb(&k["color"])?);
        }
    }
    Ok(Lighting {
        effect,
        brightness: byte("brightness")?.min(config::MAX_BRIGHTNESS),
        speed: byte("speed")?,
        color: config::rgb_hex(&rgb(&l["baseColor"])?),
        shared,
        key_colors: colors.iter().any(|c| c != NONE).then_some(colors),
        flags: None,
        extra: None,
    })
}

/// Refuses what a Studio profile can't express, naming the setting.
fn check_representable(cfg: &Config, aliases: &Aliases) -> Result<()> {
    ensure!(
        cfg.timing == STUDIO_TIMING,
        "timing: keypads Binepad Studio has saved to use double_tap_ms = {} and hold_ms = {} \
         (Studio's profile has no timing settings)",
        STUDIO_TIMING.double_tap_ms,
        STUDIO_TIMING.hold_ms
    );
    let shared = cfg.layers.first().is_some_and(|l| l.lighting.shared);
    for (p, layer) in cfg.layers.iter().enumerate() {
        let l = &layer.lighting;
        let path = format!("layers[{p}].lighting");
        ensure!(l.flags.is_none(), "{path}.flags: a custom flags byte can't be saved in Binepad Studio's profile");
        ensure!(l.extra.is_none(), "{path}.extra: raw lighting bytes can't be saved in Binepad Studio's profile");
        ensure!(
            studio_effect(l.effect).is_some(),
            "{path}.effect: effect {} has no name in Binepad Studio; use a named effect",
            l.effect
        );
        if shared && p > 0 {
            ensure!(
                *l == cfg.layers[0].lighting,
                "{path}: with shared lighting, every layer must have layer 0's lighting"
            );
        }
        let keys = layer.keys.iter().enumerate().map(|(k, key)| (format!("layers[{p}].keys[{k}]"), key.slots()));
        for (at, s) in keys.chain([(format!("layers[{p}].knob"), layer.knob.clone())]) {
            for (i, slot) in KNOB_SLOTS.iter().enumerate() {
                let Some(name) = s.get(slot).filter(|n| *n != NONE) else { continue };
                if name.starts_with("raw.") {
                    ensure!(
                        aliases.refs.contains_key(name),
                        "{at}.{slot}: `{name}` is not an action Binepad Studio knows, so it can't be saved there"
                    );
                    continue;
                }
                if let Some((_, allowed)) = RESTRICTED.iter().find(|(n, _)| n == name)
                    && allowed & (1 << i) == 0
                {
                    let ok: Vec<&str> = (0..KNOB_SLOTS.len()).filter(|b| allowed & (1 << b) != 0).map(|b| KNOB_SLOTS[b]).collect();
                    bail!("{at}.{slot}: Binepad Studio allows `{name}` only as {}", ok.join(", "));
                }
                ensure!(
                    !NEEDS_APPLICATION.contains(&name.as_str()),
                    "{at}.{slot}: `{name}` needs an application, which only Binepad Studio can set"
                );
            }
        }
        check_knob_selectors(p, layer)?;
    }
    for (slot, steps) in &cfg.macros {
        ensure!(
            !steps.iter().any(|s| s.starts_with("raw.")),
            "macros.{slot}: raw steps can't be saved in Binepad Studio's profile"
        );
    }
    Ok(())
}

/// Actions Studio's validator rejects without an `applicationId` argument.
const NEEDS_APPLICATION: [&str; 3] = ["app.open", "app.focus", "system.app-volume"];
/// Model/effort selectors that must take the whole knob (Studio's `WholeKnobAssignment`);
/// the combined ones leave the knob's Hold free.
const KNOB_SELECTORS: [&str; 6] = [
    "claude.model.knob",
    "claude.effort.knob",
    "claude.model-effort.knob",
    "codex.composer.model.knob",
    "codex.composer.effort.knob",
    "codex.composer.model-effort.knob",
];
const COMBINED_SELECTORS: [&str; 2] = ["claude.model-effort.knob", "codex.composer.model-effort.knob"];

/// A selector on a layer must be the knob's press and both turns, with no
/// other knob gestures (Hold stays free for the combined ones).
fn check_knob_selectors(p: usize, layer: &Layer) -> Result<()> {
    let used = |s: &Slots, id: &str| KNOB_SLOTS.iter().any(|slot| s.get(slot).is_some_and(|n| n == id));
    for id in KNOB_SELECTORS {
        let on_keys = layer.keys.iter().any(|k| used(&k.slots(), id));
        if !on_keys && !used(&layer.knob, id) {
            continue;
        }
        let k = &layer.knob;
        let is = |slot: &str| k.get(slot).is_some_and(|n| n == id);
        let free = |slot: &str| k.get(slot).is_none_or(|n| n == NONE);
        let hold_ok = COMBINED_SELECTORS.contains(&id) || free("hold");
        ensure!(
            !on_keys && is("tap") && is("ccw") && is("cw") && free("double_tap") && free("tap_hold") && hold_ok,
            "layers[{p}]: Binepad Studio requires `{id}` to take the whole knob: tap, ccw and cw, \
             with no other knob gestures{}",
            if COMBINED_SELECTORS.contains(&id) { " except hold" } else { "" }
        );
    }
    Ok(())
}

fn action_ref(name: &str, aliases: &Aliases) -> Value {
    aliases.refs.get(name).cloned().unwrap_or_else(|| json!({"actionId": name}))
}

fn assignments(layer: &Layer, aliases: &Aliases) -> Value {
    let controls = KEY_IDS[..2]
        .iter()
        .zip(layer.keys[..2].iter().map(Key::slots))
        .chain([(&KNOB_ID, layer.knob.clone())])
        .chain(KEY_IDS[2..].iter().zip(layer.keys[2..].iter().map(Key::slots)));
    let mut out = Vec::new();
    for (control, s) in controls {
        let gestures: Vec<Value> = KNOB_SLOTS
            .iter()
            .zip(GESTURES)
            .filter_map(|(slot, g)| {
                let name = s.get(slot).filter(|n| *n != NONE)?;
                Some(json!({"gesture": g, "action": action_ref(name, aliases)}))
            })
            .collect();
        if !gestures.is_empty() {
            out.push(json!({"controlId": control, "gestures": gestures}));
        }
    }
    Value::Array(out)
}

fn lighting_json(index: usize, l: &Lighting, prev: Option<&Value>) -> Result<Value> {
    let keys: Vec<Value> = match &l.key_colors {
        Some(colors) => colors
            .iter()
            .zip(KEY_IDS)
            .filter(|(c, _)| *c != NONE)
            .map(|(c, id)| Ok(json!({"controlId": id, "color": rgb_json(c)?})))
            .collect::<Result<_>>()?,
        None => Vec::new(),
    };
    let mut m = Map::new();
    m.insert("layer".into(), json!(index));
    m.insert("effect".into(), json!(studio_effect(l.effect)));
    m.insert("brightness".into(), json!(l.brightness));
    m.insert("speed".into(), json!(l.speed));
    m.insert("baseColor".into(), rgb_json(&l.color)?);
    m.insert("keys".into(), Value::Array(keys));
    // Indicator settings are Studio-only; keep them.
    for k in ["indicatorColor", "indicatorBrightness"] {
        if let Some(v) = prev.and_then(|p| p.get(k)) {
            m.insert(k.into(), v.clone());
        }
    }
    Ok(Value::Object(m))
}

fn macro_steps(steps: &[String]) -> Result<Vec<Value>> {
    let mut out = Vec::new();
    let mut delay = 0u64;
    for step in steps {
        let (verb, arg) = step.split_once(' ').with_context(|| format!("macro step `{step}`"))?;
        if verb == "delay" {
            delay += arg.parse::<u64>().with_context(|| format!("macro step `{step}`"))?;
            continue;
        }
        let event = MACRO_EVENTS.iter().find(|(v, _)| *v == verb).with_context(|| format!("macro step `{step}`"))?.1;
        out.push(json!({"delayMilliseconds": delay, "actionId": arg, "event": event}));
        delay = 0;
    }
    Ok(out)
}

/// Builds the profile to save with `cfg`, starting from the device's
/// previous profile `prev`. bnk8 rewrites the layers' bindings, names and
/// lighting, the on-device macros, the shared-lighting flag and `updatedAt`;
/// everything else (app rules, desktop macros, Studio-only settings) is kept.
/// Layers without a name keep the previous name at their position.
pub fn to_profile(cfg: &Config, prev: &Value, aliases: &Aliases, now: &str) -> Result<Value> {
    check_representable(cfg, aliases)?;
    let mut out = prev.as_object().context("the previous profile is not a JSON object")?.clone();
    let prev_layers = prev["layers"].as_array().cloned().unwrap_or_default();
    let prev_layer = |i: usize| prev_layers.iter().find(|l| l["index"] == i).and_then(Value::as_object).cloned();

    let mut layers = Vec::with_capacity(cfg.layers.len());
    for (i, l) in cfg.layers.iter().enumerate() {
        let mut m = prev_layer(i).unwrap_or_default();
        let name = match &l.name {
            Some(n) => n.clone(),
            None => m.get("name").and_then(Value::as_str).map_or_else(|| format!("Layer {}", i + 1), str::to_string),
        };
        let lighting = lighting_json(i, &l.lighting, m.get("lighting"))?;
        m.insert("index".into(), json!(i));
        m.insert("name".into(), json!(name));
        m.insert("assignments".into(), assignments(l, aliases));
        m.insert("lighting".into(), lighting);
        layers.push(Value::Object(m));
    }
    out.insert("layers".into(), Value::Array(layers));

    let prev_macros = prev["macros"].as_array().cloned().unwrap_or_default();
    let prev_name = |slot: usize| {
        prev_macros
            .iter()
            .find(|m| m["storage"] == "OnDevice" && m["slot"] == slot)
            .and_then(|m| m["name"].as_str())
            .map(str::to_string)
    };
    let mut macros = Vec::new();
    for (key, steps) in &cfg.macros {
        let slot: usize = key.parse().with_context(|| format!("macros.{key}"))?;
        let slot = slot - 1;
        macros.push(json!({
            "slot": slot,
            "name": prev_name(slot).unwrap_or_else(|| format!("Macro {}", slot + 1)),
            "storage": "OnDevice",
            "steps": macro_steps(steps).with_context(|| format!("macros.{key}"))?,
        }));
    }
    macros.sort_by_key(|m| m["slot"].as_u64());
    macros.extend(prev_macros.into_iter().filter(|m| m["storage"] != "OnDevice"));
    out.insert("macros".into(), Value::Array(macros));
    out.insert("sharedRgbAcrossLayers".into(), json!(cfg.layers[0].lighting.shared));
    out.insert("updatedAt".into(), json!(now));
    Ok(Value::Object(out))
}

/// The current UTC time as RFC 3339 (`2026-11-24T12:34:56Z`), for `updatedAt`.
pub fn now_utc() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let (days, rem) = (secs / 86_400, secs % 86_400);
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", rem / 3600, rem % 3600 / 60, rem % 60)
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::*;
    use crate::config::FACTORY;
    use crate::protocol::CONFIG_LEN;

    fn factory() -> Config {
        config::from_blob(FACTORY).unwrap()
    }

    /// A minimal previous profile: what Studio-only fields bnk8 must keep.
    fn prev() -> Value {
        json!({
            "schemaVersion": 1, "id": "p", "name": "Mine", "devicePackId": DEVICE,
            "updatedAt": "2026-01-01T00:00:00Z",
            "layers": [{"index": 0, "name": "Codex", "assignments": [], "lighting": null}],
            "applicationBindings": [{"platform": "MacOS", "applicationId": "x", "layer": 0}],
            "macros": [{"slot": 16, "name": "Desk", "storage": "Desktop", "steps": []}],
            "autoSwitchApplications": true,
        })
    }

    fn blob(cfg: &Config, aliases: &Aliases) -> Blob {
        config::to_blob(cfg, FACTORY, &aliases.ids()).unwrap()
    }

    #[test]
    fn factory_config_round_trips_through_a_profile() {
        let mut cfg = factory();
        cfg.macros.insert("3".into(), vec!["press modifier.left_gui".into(), "delay 20".into(), "tap keyboard.4".into(), "release modifier.left_gui".into()]);
        cfg.layers[0].keys[7] = Key::Tap("macro.3".into());
        cfg.layers[1].lighting.key_colors = Some(vec!["#FF0000".into(), NONE.into(), NONE.into(), NONE.into(), NONE.into(), NONE.into(), NONE.into(), "#00FF00".into()]);
        let aliases = Aliases::default();
        let p = to_profile(&cfg, &prev(), &aliases, "2026-11-24T00:00:00Z").unwrap();
        let (back, back_aliases) = from_profile(&p, &blob(&cfg, &aliases)).unwrap();
        assert_eq!(blob(&back, &back_aliases), blob(&cfg, &aliases));
        // Unnamed layers keep the previous name, or get a default.
        assert_eq!(p["layers"][0]["name"], "Codex");
        assert_eq!(p["layers"][1]["name"], "Layer 2");
        // Studio-only settings survive; desktop macros stay after the on-device ones.
        assert_eq!(p["applicationBindings"], prev()["applicationBindings"]);
        assert_eq!(p["macros"][0]["slot"], 2);
        assert_eq!(p["macros"][1]["storage"], "Desktop");
        assert_eq!(p["macros"][0]["steps"][1], json!({"delayMilliseconds": 20, "actionId": "keyboard.4", "event": "Tap"}));
        assert_eq!(p["updatedAt"], "2026-11-24T00:00:00Z");
    }

    #[test]
    fn opaque_actions_keep_their_studio_reference() {
        // An agent action with arguments: bnk8 only knows its native entry.
        let r = json!({"actionId": "app.open", "arguments": {"path": "/Applications/Notes.app"}});
        let raw = format!("raw.{:08x}0000{:02x}", fnv1a("app.open").swap_bytes(), 1);
        let aliases = Aliases { refs: BTreeMap::from([(raw.clone(), r.clone())]) };
        let mut cfg = factory();
        cfg.layers[3].keys[0] = Key::Tap(raw);
        let p = to_profile(&cfg, &prev(), &aliases, "t").unwrap();
        assert_eq!(p["layers"][3]["assignments"][0]["gestures"][0]["action"], r);
        let (back, back_aliases) = from_profile(&p, &blob(&cfg, &aliases)).unwrap();
        assert_eq!(back_aliases, aliases);
        assert_eq!(blob(&back, &back_aliases), blob(&cfg, &aliases));
    }

    #[test]
    fn refuses_what_a_studio_profile_cannot_hold() {
        let refused = |edit: &dyn Fn(&mut Config)| {
            let mut cfg = factory();
            edit(&mut cfg);
            format!("{:#}", to_profile(&cfg, &prev(), &Aliases::default(), "t").unwrap_err())
        };
        assert!(refused(&|c| c.timing.hold_ms = 400).contains("hold_ms = 350"));
        assert!(refused(&|c| c.layers[1].lighting.flags = Some(3)).contains("layers[1].lighting.flags"));
        assert!(refused(&|c| c.layers[1].lighting.extra = Some("00".repeat(33))).contains("layers[1].lighting.extra"));
        assert!(refused(&|c| c.layers[2].lighting.effect = 7).contains("effect 7 has no name"));
        assert!(refused(&|c| c.layers[0].keys[0] = Key::Tap("raw.0102030400000200".into())).contains("raw.0102030400000200"));
        assert!(refused(&|c| {
            c.layers.iter_mut().for_each(|l| l.lighting.shared = true);
            c.layers[1].lighting.brightness = 1;
        })
        .contains("layer 0's lighting"));
        // Studio's per-action gesture rules and whole-knob selectors.
        let e = refused(&|c| c.layers[3].knob.cw = Some("layer.mo.2".into()));
        assert!(e.contains("layers[3].knob.cw: Binepad Studio allows `layer.mo.2` only as tap"), "{e}");
        let e = refused(&|c| c.layers[3].keys[0] = Key::Tap("system.app-volume".into()));
        assert!(e.contains("only as ccw, cw"), "{e}");
        let e = refused(&|c| c.layers[0].knob.cw = Some("media.volume_up".into()));
        assert!(e.contains("`codex.composer.model-effort.knob` to take the whole knob"), "{e}");
        let e = refused(&|c| c.layers[3].keys[1] = Key::Tap("claude.model.knob".into()));
        assert!(e.contains("`claude.model.knob` to take the whole knob"), "{e}");
    }

    /// bnk8's encoding and gesture table against Studio's catalogue, as dumped
    /// by `work/oracle/run.sh rules` into `work/fixtures/studio-rules.tsv`.
    #[test]
    fn studio_rules_match_bnk8() {
        let Some(path) = work("fixtures/studio-rules.tsv") else { return };
        let text = std::fs::read_to_string(path).unwrap();
        let mut lines = text.lines();
        let header: Vec<&str> = lines.next().unwrap().split('\t').collect();
        let col = |name: &str| header.iter().position(|h| *h == name).unwrap();
        for line in lines {
            let f: Vec<&str> = line.split('\t').collect();
            let name = f[col("name")];
            assert_eq!(f[col("status")], "KNOWN", "{name}");
            let a = actions::encode(name).unwrap();
            let kind = match f[col("execution")] {
                "Firmware" => 0,
                "Agent" => 1,
                _ => 2,
            };
            let studio = (u32::from_str_radix(f[col("hash")], 16).unwrap(), u16::from_str_radix(f[col("param")], 16).unwrap(), kind);
            assert_eq!((a.id, a.param, a.kind), studio, "{name}: bnk8's entry differs from Studio's");
            let mask = f[col("gestures")]
                .split(',')
                .map(|g| 1u8 << GESTURES.iter().position(|x| *x == g).unwrap())
                .fold(0, |m, b| m | b);
            let table = RESTRICTED.iter().find(|(n, _)| *n == name).map_or(0b11_1111, |(_, m)| *m);
            assert_eq!(table, mask, "{name}: gesture table differs from Studio's");
            let step = actions::encode_key(name).ok().map(|c| format!("yes:{c:04x}"));
            assert_eq!(step.as_deref().unwrap_or("no"), f[col("stepok")], "{name}: macro step");
        }
    }

    #[test]
    fn utc_timestamps_are_rfc3339() {
        let t = now_utc();
        assert_eq!(t.len(), 20, "{t}");
        assert!(t.ends_with('Z') && t.as_bytes()[10] == b'T', "{t}");
    }

    // ---- Local goldens against Binepad Studio's own data (`work/`, never committed) ----

    fn work(rel: &str) -> Option<PathBuf> {
        let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("work").join(rel);
        p.exists().then_some(p)
    }

    const STUDIO_FACTORY: [&str; 5] = ["bnk8-ai", "bnk8-ai-6024", "bnk8-ai-6021", "bnk8-ai-6019", "bnk8-ai-6018"];

    #[test]
    fn studio_factory_profiles_compile_to_studios_images() {
        let Some(dir) = work("decomp/Protocol") else { return };
        for name in STUDIO_FACTORY {
            let json: Value = serde_json::from_slice(
                &std::fs::read(dir.join(format!("Binepad.Protocol.FactoryImages.{name}.json"))).unwrap(),
            )
            .unwrap();
            let bin: Blob = std::fs::read(dir.join(format!("Binepad.Protocol.FactoryImages.{name}.bin")))
                .unwrap()
                .try_into()
                .unwrap();
            let (cfg, aliases) = from_profile(&json, &bin).unwrap_or_else(|e| panic!("{name}: {e:#}"));
            assert!(blob(&cfg, &aliases) == bin, "{name}: bnk8's encoding differs from Studio's image");
        }
    }

    #[test]
    fn live_profile_store_payload_round_trips() {
        let Some(path) = work("fixtures/rev4.bin") else { return };
        let payload = std::fs::read(path).unwrap();
        let native: Blob = payload[..CONFIG_LEN].try_into().unwrap();
        let doc = crate::archive::decode(&payload[CONFIG_LEN..]).unwrap();
        let (cfg, aliases) = from_profile(&doc.profile, &native).unwrap();
        assert!(blob(&cfg, &aliases) == native, "bnk8's encoding of the saved profile differs from the device's");
        // Rewriting an unchanged config keeps every field but `updatedAt`.
        let mut again = to_profile(&cfg, &doc.profile, &aliases, "t").unwrap();
        let (back, _) = from_profile(&again, &native).unwrap();
        assert_eq!(back, cfg);
        again["updatedAt"] = doc.profile["updatedAt"].clone();
        for (k, v) in doc.profile.as_object().unwrap() {
            if k != "layers" {
                assert_eq!(&again[k], v, "profile field {k}");
            }
        }
    }

    /// Compiles `profile` with Studio's real compiler (`work/oracle`).
    fn studio_compile(profile: &Value) -> Option<Blob> {
        let run = work("oracle/run.sh")?;
        let dir = std::env::temp_dir().join(format!("bnk8-oracle-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (json, bin) = (dir.join("p.json"), dir.join("p.bin"));
        std::fs::write(&json, serde_json::to_vec(profile).unwrap()).unwrap();
        let out = std::process::Command::new(run).args(["compile"]).arg(&json).arg(&bin).output().unwrap();
        assert!(out.status.success(), "Studio rejected the profile: {}", String::from_utf8_lossy(&out.stderr));
        Some(std::fs::read(&bin).unwrap().try_into().unwrap())
    }

    #[test]
    fn studio_compiles_bnk8_profiles_to_bnk8_blobs() {
        let Some(path) = work("fixtures/rev4.bin") else { return };
        if work("oracle/run.sh").is_none() {
            return;
        }
        let payload = std::fs::read(path).unwrap();
        let native: Blob = payload[..CONFIG_LEN].try_into().unwrap();
        let doc = crate::archive::decode(&payload[CONFIG_LEN..]).unwrap();
        let (base, aliases) = from_profile(&doc.profile, &native).unwrap();
        let mut edits = vec![base.clone()];
        // The live test's edit: K8 on layer 1 plays Cmd+Shift+4 from macro 1.
        let mut c = base.clone();
        c.macros.insert("1".into(), ["press modifier.left_gui", "press modifier.left_shift", "tap keyboard.4", "release modifier.left_shift", "release modifier.left_gui"].map(String::from).to_vec());
        c.layers[0].keys[7] = Key::Tap("macro.1".into());
        edits.push(c);
        // Layers, names, per-key colours, shared lighting, timing-free knob edits.
        let mut c = config::from_blob(FACTORY).unwrap();
        c.layers.push(c.layers[3].clone());
        c.layers[4].name = Some("Extra".into());
        c.layers[4].knob.cw = Some("layer.tg.2".into());
        c.layers[2].lighting.key_colors = Some(vec!["#123456".into(); KEYS]);
        edits.push(c);
        let mut c = config::from_blob(FACTORY).unwrap();
        let l0 = c.layers[0].lighting.clone();
        c.layers.iter_mut().for_each(|l| l.lighting = Lighting { shared: true, ..l0.clone() });
        edits.push(c);
        for (i, cfg) in edits.iter().enumerate() {
            let p = to_profile(cfg, &doc.profile, &aliases, &now_utc()).unwrap();
            let studio = studio_compile(&p).unwrap();
            assert!(studio == blob(cfg, &aliases), "edit {i}: Studio compiles bnk8's profile differently");
        }
    }
}
