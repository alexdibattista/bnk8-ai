//! `bnk8 mcp`: a Model Context Protocol server on stdin/stdout, so Claude
//! Desktop or Claude Code (on the user's own subscription) can read the
//! config, preview edits as per-item changes, and apply them.
//!
//! Newline-delimited JSON-RPC 2.0, hand-rolled on `serde_json`. Only
//! JSON-RPC goes to stdout. The device is opened per tool call, so the GUI
//! or CLI can use the keypad between calls.

use std::io::{BufRead, Write};

use anyhow::{Result, bail};
use serde_json::{Map, Value, json};

use bnk8::store::{self, Current, Source};
use bnk8::{actions, config, protocol, transport};

const PROTOCOL_VERSIONS: [&str; 3] = ["2025-06-18", "2025-03-26", "2024-11-05"];

const INSTRUCTIONS: &str = r##"bnk8 configures a Binepad BNK8 AI Edition macropad: 8 keys (K1..K8, top left to bottom right, skipping the knob at top right) and a knob, up to 10 layers, and 15 macros of up to 7 key steps each.

Workflow: call get_config, edit the TOML it returns, then call preview_config with the complete edited TOML. Show the user the listed changes, and call apply_config with the same TOML and the preview id only after they agree. Always send the complete config, never a fragment, and keep every setting you weren't asked to change exactly as get_config returned it.

Config rules:
- Each layer has keys = [8 entries]. An entry is an action name, or a table of gestures {tap, double_tap, hold, tap_hold}. [layers.knob] takes those gestures plus ccw and cw (turns).
- Action names come from list_actions, e.g. keyboard.a, modifier.left_gui, media.volume_up, layer.next, layer.2, layer.mo.2, macro.3, claude.accept. "none" leaves a binding empty.
- A keyboard binding sends one key. For a shortcut such as Cmd+Shift+4, write a macro and bind macro.N, e.g. ["press modifier.left_gui", "press modifier.left_shift", "tap keyboard.4", "release modifier.left_shift", "release modifier.left_gui"].
- Macro steps are "tap <key>", "press <key>", "release <key>" and "delay <ms>" (a multiple of 10; at most 2550 ms before one step). <key> is a keyboard.*, modifier.* or media.* name. Release every key a macro presses. Macros live under [macros] with keys "1" to "15"; a macro needs at least one step. Pick a free slot unless the user names one.
- layer.N, layer.mo.N (hold for layer) and layer.tg.N (toggle layer) count layers from 1. Keep a layer.next binding on every layer so the user can always switch layers.
- Lighting per layer: effect (a name such as solid or breathing, or an id 0 to 50), brightness 0 to 180, speed 0 to 255, color "#RRGGBB", optional key_colors (8 entries, "#RRGGBB" or "none"). shared = true must be set on every layer or on none.
- timing.double_tap_ms is 100 to 800; timing.hold_ms is 150 to 2000.
- A layer may have name = "…" (shown in Binepad Studio). Names are kept only on keypads that Binepad Studio has saved to; there a layer without a name keeps its current one.
- If get_config says the config was read from the device's profile store, the keypad is also managed by Binepad Studio. Then keep timing at double_tap_ms = 260 and hold_ms = 350, use named effects only, and don't add lighting flags or extra; preview_config reports anything Studio's profile can't hold.
- Keep format = 2 and device = "binepad.bnk8"."##;

const STALE_PREVIEW: &str = "The config or the device changed since preview_config returned that id; \
                             call preview_config again and show the user the new changes.";

/// Serves MCP over newline-delimited JSON-RPC until `input` ends.
pub fn serve(serial: Option<&str>, input: impl BufRead, mut output: impl Write) -> Result<()> {
    for line in input.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        if let Some(resp) = handle(serial, &line) {
            output.write_all(serde_json::to_string(&resp)?.as_bytes())?;
            output.write_all(b"\n")?;
            output.flush()?;
        }
    }
    Ok(())
}

fn error(id: Value, code: i64, message: impl Into<String>) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message.into()}})
}

/// Answers one JSON-RPC message; `None` for notifications and client responses.
fn handle(serial: Option<&str>, line: &str) -> Option<Value> {
    let msg: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(e) => return Some(error(Value::Null, -32700, format!("parse error: {e}"))),
    };
    let Some(obj) = msg.as_object() else {
        return Some(error(Value::Null, -32600, "invalid request"));
    };
    let method = obj.get("method")?;
    let id = obj.get("id")?.clone();
    let Some(method) = method.as_str() else {
        return Some(error(id, -32600, "invalid request"));
    };
    let params = obj.get("params");
    let result = match method {
        "initialize" => Ok(initialize(params)),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({"tools": tools()})),
        "tools/call" => call(serial, params),
        _ => Err((-32601, format!("method not found: {method}"))),
    };
    Some(match result {
        Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
        Err((code, message)) => error(id, code, message),
    })
}

fn initialize(params: Option<&Value>) -> Value {
    let asked = params
        .and_then(|p| p.get("protocolVersion"))
        .and_then(Value::as_str);
    let version = PROTOCOL_VERSIONS
        .iter()
        .find(|v| Some(**v) == asked)
        .unwrap_or(&PROTOCOL_VERSIONS[0]);
    json!({
        "protocolVersion": version,
        "capabilities": {"tools": {"listChanged": false}},
        "serverInfo": {"name": "bnk8", "version": env!("CARGO_PKG_VERSION")},
        "instructions": INSTRUCTIONS,
    })
}

fn tools() -> Value {
    let serial = json!({
        "type": "string",
        "description": "USB serial number; needed only when several BNK8s are attached",
    });
    let toml = json!({"type": "string", "description": "The complete config as TOML"});
    let read_only = json!({"readOnlyHint": true, "openWorldHint": false});
    json!([
        {
            "name": "get_config",
            "description": "Read the BNK8's current config as TOML, with where it came from and any \
                            Binepad Studio layer names. Call this before editing.",
            "inputSchema": {
                "type": "object",
                "properties": {"serial": serial},
                "additionalProperties": false,
            },
            "annotations": read_only,
        },
        {
            "name": "list_actions",
            "description": "List the action names that keys, knob gestures and macro steps can use.",
            "inputSchema": {
                "type": "object",
                "properties": {"prefix": {
                    "type": "string",
                    "description": "Only names starting with this, e.g. keyboard. or claude.",
                }},
                "additionalProperties": false,
            },
            "annotations": read_only,
        },
        {
            "name": "preview_config",
            "description": "Validate a complete TOML config and show what applying it would change \
                            on the BNK8, without writing. Returns a preview id for apply_config.",
            "inputSchema": {
                "type": "object",
                "properties": {"toml": toml, "serial": serial},
                "required": ["toml"],
                "additionalProperties": false,
            },
            "annotations": read_only,
        },
        {
            "name": "apply_config",
            "description": "Write a complete TOML config to the BNK8. Needs the preview id \
                            preview_config returned for this exact TOML; show the user the \
                            previewed changes first.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "toml": toml,
                    "preview_id": {
                        "type": "string",
                        "description": "The id preview_config returned for this exact TOML",
                    },
                    "serial": serial,
                },
                "required": ["toml", "preview_id"],
                "additionalProperties": false,
            },
            "annotations": {
                "readOnlyHint": false,
                "destructiveHint": true,
                "idempotentHint": true,
                "openWorldHint": false,
            },
        },
    ])
}

type RpcError = (i64, String);

/// A string argument: `Ok(None)` when absent, an error when present but not a string.
fn arg<'a>(args: &'a Map<String, Value>, name: &str) -> Result<Option<&'a str>, RpcError> {
    match args.get(name) {
        None => Ok(None),
        Some(Value::String(s)) => Ok(Some(s)),
        Some(_) => Err(invalid_arg(name)),
    }
}

fn required<'a>(args: &'a Map<String, Value>, name: &str) -> Result<&'a str, RpcError> {
    arg(args, name)?.ok_or_else(|| invalid_arg(name))
}

fn invalid_arg(name: &str) -> RpcError {
    (-32602, format!("invalid params: {name} must be a string"))
}

fn call(serial: Option<&str>, params: Option<&Value>) -> Result<Value, RpcError> {
    let invalid = || (-32602, "invalid params: name must be a string".to_string());
    let params = params.and_then(Value::as_object).ok_or_else(invalid)?;
    let name = params.get("name").and_then(Value::as_str).ok_or_else(invalid)?;
    let empty = Map::new();
    let args = match params.get("arguments") {
        None => &empty,
        Some(Value::Object(m)) => m,
        Some(_) => return Err((-32602, "invalid params: arguments must be an object".into())),
    };
    let serial = arg(args, "serial")?.or(serial);
    let out = match name {
        "get_config" => get_config(serial),
        "list_actions" => list_actions(arg(args, "prefix")?),
        "preview_config" => preview_config(serial, required(args, "toml")?),
        "apply_config" => {
            let toml = required(args, "toml")?;
            apply_config(serial, toml, required(args, "preview_id")?)
        }
        _ => return Err((-32602, format!("unknown tool: {name}"))),
    };
    let (text, is_error) = match out {
        Ok(text) => (text, false),
        Err(e) => (format!("{e:#}"), true),
    };
    Ok(json!({"content": [{"type": "text", "text": text}], "isError": is_error}))
}

fn get_config(serial: Option<&str>) -> Result<String> {
    let dev = transport::open(serial)?;
    let mut lines = vec![format!(
        "BNK8 {}, firmware {}",
        dev.serial(),
        transport::format_fw(dev.fw())
    )];
    let cfg = match store::current(&dev)? {
        Current::Known(blob, source) => {
            let label = match &source {
                Source::Factory => "factory default".to_string(),
                Source::OlderFactory(v) => format!("factory default of firmware {v}"),
                Source::Snapshot => "last written by bnk8".to_string(),
                Source::ProfileStore { .. } => "read from the device's profile store".to_string(),
            };
            lines.push(format!("Config: {label}, CRC {:#010x}", protocol::crc32(&*blob)));
            store::config(&blob, &source)?
        }
        Current::Unknown { crc } => {
            lines.push(format!(
                "Config: unknown to bnk8 (CRC {crc:#010x}); applying writes the whole config. \
                 The TOML below is the factory config to start from."
            ));
            config::from_blob(config::FACTORY)?
        }
    };
    let toml = config::to_toml(&cfg)?;
    Ok(format!("{}\n\n{toml}", lines.join("\n")))
}

fn list_actions(prefix: Option<&str>) -> Result<String> {
    let prefix = prefix.unwrap_or("");
    let names: Vec<String> = actions::catalog()
        .into_iter()
        .filter(|n| n.starts_with(prefix))
        .collect();
    if names.is_empty() {
        return Ok(format!("No action names start with {prefix:?}."));
    }
    Ok(names.join("\n"))
}

/// Parses and validates `toml` without touching the device, like `bnk8 apply`.
fn validate(toml: &str) -> Result<config::Config> {
    let cfg = match config::from_toml(toml) {
        Ok(cfg) => cfg,
        Err(e) => bail!("invalid config: {e:#}"),
    };
    if let Err(e) = config::to_blob(&cfg, config::FACTORY, &config::StudioIds::new()) {
        bail!("invalid config: {e:#}");
    }
    Ok(cfg)
}

/// Prepares `cfg` for the device; the id covers the encoded config, the
/// device's current CRC and the change list (which includes layer names).
fn prepare(dev: &transport::Bnk8, cfg: &config::Config) -> Result<(store::Prepared, String)> {
    let p = store::prepare(dev, cfg)?;
    let lines = match &p.changes {
        config::Changes::Items(lines) => lines.join("\n"),
        config::Changes::Unknown => String::new(),
    };
    let id = format!(
        "{:08x}",
        protocol::crc32(&[&p.native[..], &p.device_crc.to_le_bytes(), lines.as_bytes()].concat())
    );
    Ok((p, id))
}

/// Settings written when the device's config is unknown (the whole config).
fn setting_count(blob: &config::Blob) -> Result<usize> {
    Ok(config::flatten(&config::from_blob(blob)?).len())
}

/// `notes` as lines ending in a newline, for the start of a tool result.
fn note_lines(notes: &[String]) -> String {
    notes.iter().map(|n| format!("Note: {n}\n")).collect()
}

fn preview_config(serial: Option<&str>, toml: &str) -> Result<String> {
    let cfg = validate(toml)?;
    let dev = transport::open(serial)?;
    let (p, id) = prepare(&dev, &cfg)?;
    let body = match &p.changes {
        config::Changes::Items(lines) if lines.is_empty() => {
            format!("No changes: the device already has this config.\nPreview id: {id}")
        }
        config::Changes::Items(lines) => {
            format!("{} change(s):\n{}\nPreview id: {id}", lines.len(), lines.join("\n"))
        }
        config::Changes::Unknown => format!(
            "The device's current config is unknown to bnk8, so applying writes the whole config \
             ({} settings).\nPreview id: {id}",
            setting_count(&p.native)?
        ),
    };
    Ok(note_lines(&p.notes) + &body)
}

fn apply_config(serial: Option<&str>, toml: &str, preview_id: &str) -> Result<String> {
    let cfg = validate(toml)?;
    let dev = transport::open(serial)?;
    let (p, id) = prepare(&dev, &cfg)?;
    if id != preview_id {
        bail!(STALE_PREVIEW);
    }
    if p.unchanged() {
        return Ok("No changes; nothing written.".into());
    }
    store::commit(&dev, &p)?;
    let body = match &p.changes {
        config::Changes::Items(lines) => format!("Applied {} change(s):\n{}", lines.len(), lines.join("\n")),
        config::Changes::Unknown => format!("Wrote the whole config ({} settings).", setting_count(&p.native)?),
    };
    Ok(note_lines(&p.notes) + &body)
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    fn req(id: u64, method: &str, params: Value) -> String {
        json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}).to_string()
    }

    fn call_tool(name: &str, arguments: Value) -> Value {
        handle(None, &req(1, "tools/call", json!({"name": name, "arguments": arguments}))).unwrap()
    }

    #[test]
    fn initialize_negotiates_version() {
        let r = handle(None, &req(1, "initialize", json!({"protocolVersion": "2025-03-26"}))).unwrap();
        assert_eq!(r["result"]["protocolVersion"], "2025-03-26");
        assert_eq!(r["result"]["serverInfo"]["name"], "bnk8");
        assert!(r["result"]["instructions"].as_str().unwrap().contains("preview_config"));
        let r = handle(None, &req(2, "initialize", json!({"protocolVersion": "1999-01-01"}))).unwrap();
        assert_eq!(r["result"]["protocolVersion"], "2025-06-18");
    }

    #[test]
    fn notifications_get_no_reply() {
        let n = json!({"jsonrpc": "2.0", "method": "notifications/initialized"}).to_string();
        assert!(handle(None, &n).is_none());
    }

    #[test]
    fn tools_list() {
        let r = handle(None, &req(1, "tools/list", json!({}))).unwrap();
        let tools = r["result"]["tools"].as_array().unwrap();
        let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert_eq!(names, ["get_config", "list_actions", "preview_config", "apply_config"]);
        assert_eq!(tools[3]["inputSchema"]["required"], json!(["toml", "preview_id"]));
    }

    #[test]
    fn protocol_errors() {
        let r = handle(None, &req(7, "resources/list", json!({}))).unwrap();
        assert_eq!(r["error"]["code"], -32601);
        assert_eq!(r["id"], 7);
        let r = handle(None, "{not json").unwrap();
        assert_eq!(r["error"]["code"], -32700);
        assert_eq!(r["id"], Value::Null);
    }

    #[test]
    fn list_actions_filters_by_prefix() {
        let r = call_tool("list_actions", json!({"prefix": "media."}));
        assert_eq!(r["result"]["isError"], false);
        let text = r["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.lines().any(|l| l == "media.volume_up"));
        assert!(!text.lines().any(|l| l.starts_with("keyboard.")));
    }

    #[test]
    fn preview_validates_before_device_access() {
        let r = call_tool("preview_config", json!({"toml": "format = 2"}));
        assert_eq!(r["result"]["isError"], true);
        let text = r["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.starts_with("invalid config:"), "{text}");
    }

    #[test]
    fn apply_requires_preview_id() {
        let r = call_tool("apply_config", json!({"toml": "format = 2"}));
        assert_eq!(r["error"]["code"], -32602);
    }

    #[test]
    fn serve_replies_to_requests_only() {
        let input = [
            req(1, "initialize", json!({"protocolVersion": "2025-06-18"})),
            json!({"jsonrpc": "2.0", "method": "notifications/initialized"}).to_string(),
            req(2, "tools/list", json!({})),
        ]
        .join("\n");
        let mut out = Vec::new();
        serve(None, Cursor::new(input), &mut out).unwrap();
        let out = String::from_utf8(out).unwrap();
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 2);
        for l in lines {
            serde_json::from_str::<Value>(l).unwrap();
        }
    }
}
