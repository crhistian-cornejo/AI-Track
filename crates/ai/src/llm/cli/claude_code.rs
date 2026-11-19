//! Claude Code in print mode with stream-json in and out. AI-Track's tools
//! go in as an "sdk" MCP server: Claude Code sends the MCP messages over
//! the same stdio channel as `control_request`s and we answer them.

use std::time::{Duration, Instant};

use crossbeam_channel::{RecvTimeoutError, Sender};
use serde_json::{Value, json};

use super::{CliEvent, ModelInfo, Proc, Provider, TurnSpec, call_tool, command, lines, resolve, write_json};
use crate::CancelToken;
use crate::llm::agent::Effort;

const SERVER: &str = "aitrack";

/// Runs a tool: its result text and whether it is an error (`None` if stopped).
pub(super) type ToolCall<'a> = dyn FnMut(&str, Value) -> Option<(String, bool)> + 'a;

fn base_args() -> Vec<String> {
    [
        "--print",
        "--verbose",
        "--no-session-persistence",
        "--input-format",
        "stream-json",
        "--output-format",
        "stream-json",
        "--settings",
        "{\"disableAllHooks\":true}",
    ]
    .map(str::to_owned)
    .to_vec()
}

/// The models `list_models` offers, one per resolved model, best first.
pub fn catalog() -> Result<Vec<ModelInfo>, String> {
    let path = resolve("claude").ok_or("Claude Code is not installed")?;
    let mut child = command(&path)
        .args(base_args())
        .args(["--tools", "", "--strict-mcp-config", "--mcp-config", "{\"mcpServers\":{}}"])
        .spawn()
        .map_err(|e| format!("could not start Claude Code: {e}"))?;
    let mut stdin = child.stdin.take().ok_or("no stdin")?;
    let rx = lines(child.stdout.take().ok_or("no stdout")?);
    let result = (|| {
        write_json(&mut stdin, &json!({"type":"control_request","request_id":"init","request":{"subtype":"initialize"}}))?;
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut asked = false;
        loop {
            let line = rx.recv_timeout(deadline.saturating_duration_since(Instant::now())).map_err(|_| "Claude Code did not list its models in time".to_owned())?;
            let Ok(v) = serde_json::from_str::<Value>(&line) else { continue };
            let id = v["response"]["request_id"].as_str();
            if id == Some("init") && !asked {
                asked = true;
                write_json(&mut stdin, &json!({"type":"control_request","request_id":"models","request":{"subtype":"list_models"}}))?;
            }
            if id == Some("models") {
                if v["response"]["subtype"] != "success" {
                    return Err(v["response"]["error"].as_str().unwrap_or("Claude Code could not list its models").to_owned());
                }
                return Ok(models(&v["response"]["response"]["models"]));
            }
        }
    })();
    drop(stdin);
    let _ = child.kill();
    let _ = child.wait();
    result
}

/// `list_models` rows → models, skipping the "default" alias and repeats.
fn models(rows: &Value) -> Vec<ModelInfo> {
    let mut seen = Vec::new();
    let mut out = Vec::new();
    for row in rows.as_array().into_iter().flatten() {
        let value = row["value"].as_str().unwrap_or_default();
        let resolved = row["resolvedModel"].as_str().unwrap_or(value).split('[').next().unwrap_or_default().to_owned();
        if value.is_empty() || value == "default" || row["hidden"] == true || row["disabled"] == true || seen.contains(&resolved) {
            continue;
        }
        seen.push(resolved);
        out.push(ModelInfo {
            provider: Provider::ClaudeCode,
            id: value.to_owned(),
            name: row["displayName"].as_str().unwrap_or(value).to_owned(),
            hint: row["description"].as_str().unwrap_or_default().to_owned(),
        });
    }
    out
}

fn effort(e: Effort) -> &'static str {
    match e {
        Effort::Low => "low",
        Effort::Medium => "medium",
        Effort::High => "high",
        Effort::Max => "max",
    }
}

/// The MCP answer to one JSON-RPC message from Claude Code, or `None` for
/// a notification. `tools/call` goes through `call`.
pub(super) fn mcp(msg: &Value, tools: &[Value], call: &mut ToolCall<'_>) -> Value {
    let id = msg.get("id").cloned();
    let method = msg["method"].as_str().unwrap_or_default();
    let result = match method {
        "initialize" => json!({
            "protocolVersion": msg["params"]["protocolVersion"].as_str().unwrap_or("2025-06-18"),
            "capabilities": { "tools": {} },
            "serverInfo": { "name": SERVER, "version": env!("CARGO_PKG_VERSION") },
        }),
        "tools/list" => json!({ "tools": tools.iter().map(|t| json!({ "name": t["name"], "description": t["description"], "inputSchema": t["input_schema"] })).collect::<Vec<_>>() }),
        "tools/call" => {
            let name = msg["params"]["name"].as_str().unwrap_or_default();
            let args = msg["params"]["arguments"].clone();
            let args = if args.is_object() { args } else { json!({}) };
            match call(name, args) {
                Some((text, is_error)) => json!({ "content": [{ "type": "text", "text": text }], "isError": is_error }),
                None => json!({ "content": [{ "type": "text", "text": "Stopped by the user." }], "isError": true }),
            }
        }
        "ping" => json!({}),
        m if m.starts_with("notifications/") || id.is_none() => return json!({ "jsonrpc": "2.0", "result": {} }),
        _ => return json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32601, "message": format!("unknown method {method}") } }),
    };
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

/// The user message: screenshots, then the text.
fn user_message(spec: &TurnSpec) -> Value {
    let mut content: Vec<Value> = spec
        .images
        .iter()
        .map(|(mime, data)| json!({ "type": "image", "source": { "type": "base64", "media_type": mime, "data": crate::llm::base64::encode(data) } }))
        .collect();
    content.push(json!({ "type": "text", "text": spec.prompt }));
    json!({ "type": "user", "message": { "role": "user", "content": content } })
}

/// Start Claude Code for `spec` and send the handshake; the user message
/// goes in once it is answered ([`run`]).
pub(super) fn spawn(spec: &TurnSpec) -> Result<Proc, String> {
    let path = resolve("claude").ok_or("Claude Code is not installed. Install it from claude.com/code and sign in, then try again.")?;
    let mcp_config = json!({ "mcpServers": { SERVER: { "type": "sdk", "name": SERVER } } }).to_string();
    let mut child = command(&path)
        .args(base_args())
        .args(["--include-partial-messages", "--model", &spec.model, "--effort", effort(spec.effort)])
        .args(["--tools", "", "--strict-mcp-config", "--mcp-config", &mcp_config, "--allowedTools", &format!("mcp__{SERVER}")])
        .args(["--system-prompt", &spec.system])
        // A question to the user waits for them: no tool timeout.
        .env("MCP_TOOL_TIMEOUT", "86400000")
        .spawn()
        .map_err(|e| format!("could not start Claude Code: {e}"))?;
    let mut stdin = child.stdin.take().ok_or("no stdin")?;
    let lines = lines(child.stdout.take().ok_or("no stdout")?);
    if let Err(e) = write_json(&mut stdin, &json!({"type":"control_request","request_id":"init","request":{"subtype":"initialize","sdkMcpServers":[SERVER]}})) {
        let _ = child.kill();
        let _ = child.wait();
        return Err(e);
    }
    Ok(Proc { child, stdin, lines })
}

pub(super) fn run(spec: &TurnSpec, p: &mut Proc, cancel: &CancelToken, tx: &Sender<CliEvent>) -> Result<(), String> {
    let (stdin, rx) = (&mut p.stdin, &p.lines);
    let prefix = format!("mcp__{SERVER}__");
    let mut sent = false;
    loop {
        if cancel.is_cancelled() {
            return Ok(());
        }
        let line = match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(l) => l,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => return Err("Claude Code stopped before it answered. Check that you are signed in (run `claude` once in a terminal).".into()),
        };
        let Ok(v) = serde_json::from_str::<Value>(&line) else { continue };
        match v["type"].as_str().unwrap_or_default() {
            "control_response" if v["response"]["request_id"] == "init" => {
                if v["response"]["subtype"] != "success" {
                    return Err(v["response"]["error"].as_str().unwrap_or("Claude Code refused to start").to_owned());
                }
                if !sent {
                    sent = true;
                    write_json(stdin, &user_message(spec))?;
                }
            }
            "control_request" => {
                let rid = v["request_id"].clone();
                let req = &v["request"];
                let response = match req["subtype"].as_str() {
                    Some("mcp_message") => {
                        let mut call = |name: &str, input: Value| call_tool(tx, cancel, name, input);
                        json!({ "mcp_response": mcp(&req["message"], &spec.tools, &mut call) })
                    }
                    // Only our tools are allowed; anything else asking is refused.
                    Some("can_use_tool") => {
                        let ours = req["tool_name"].as_str().is_some_and(|n| n.starts_with(&prefix));
                        if ours { json!({ "behavior": "allow", "updatedInput": req["input"] }) } else { json!({ "behavior": "deny", "message": "Only AI-Track's tools are available here." }) }
                    }
                    _ => json!({}),
                };
                write_json(stdin, &json!({ "type": "control_response", "response": { "subtype": "success", "request_id": rid, "response": response } }))?;
            }
            "stream_event" => {
                let e = &v["event"];
                if e["type"] == "content_block_start"
                    && e["content_block"]["type"] == "tool_use"
                    && let Some(name) = e["content_block"]["name"].as_str().and_then(|n| n.strip_prefix(&prefix))
                {
                    let _ = tx.send(CliEvent::ToolStart(name.to_owned()));
                }
                if e["type"] == "content_block_delta" {
                    let d = &e["delta"];
                    match d["type"].as_str() {
                        Some("text_delta") => {
                            let _ = tx.send(CliEvent::Text(d["text"].as_str().unwrap_or_default().to_owned()));
                        }
                        Some("thinking_delta") => {
                            let _ = tx.send(CliEvent::Thinking(d["thinking"].as_str().unwrap_or_default().to_owned()));
                        }
                        _ => {}
                    }
                }
            }
            "result" => {
                let failed = v["is_error"] == true || v["subtype"].as_str().is_some_and(|s| s != "success");
                return if failed { Err(v["result"].as_str().filter(|s| !s.is_empty()).unwrap_or("Claude Code could not finish the request").to_owned()) } else { Ok(()) };
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_models_skips_default_and_repeats() {
        let rows = json!([
            {"value":"default","resolvedModel":"claude-opus-5-5","displayName":"Default (recommended)"},
            {"value":"opus","resolvedModel":"claude-opus-5-5","displayName":"Opus 5.5","description":"Best"},
            {"value":"opus[1m]","resolvedModel":"claude-opus-5-5[1m]","displayName":"Opus (1M)"},
            {"value":"fable","resolvedModel":"claude-fable-5-2","displayName":"Fable 5.2"},
            {"value":"x","resolvedModel":"claude-x","displayName":"X","hidden":true},
            {"value":"sonnet","resolvedModel":"claude-sonnet-5-5","displayName":"Sonnet 5.5"},
        ]);
        let m = models(&rows);
        assert_eq!(m.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(), ["opus", "fable", "sonnet"]);
        assert_eq!(m[0].hint, "Best");
    }

    #[test]
    fn mcp_answers_initialize_list_call_and_notifications() {
        let tools = vec![json!({"name":"get_project","description":"d","input_schema":{"type":"object","properties":{}}})];
        let mut calls = Vec::new();
        let mut call = |n: &str, i: Value| {
            calls.push((n.to_owned(), i));
            Some(("ok".to_owned(), false))
        };
        let init = mcp(&json!({"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":"2025-11-25"}}), &tools, &mut call);
        assert_eq!(init["result"]["protocolVersion"], "2025-11-25");
        let list = mcp(&json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}), &tools, &mut call);
        assert_eq!(list["result"]["tools"][0]["inputSchema"]["type"], "object");
        let res = mcp(&json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"get_project","arguments":{"a":1}}}), &tools, &mut call);
        assert_eq!(res["result"]["content"][0]["text"], "ok");
        assert_eq!(res["result"]["isError"], false);
        let note = mcp(&json!({"jsonrpc":"2.0","method":"notifications/initialized"}), &tools, &mut call);
        assert!(note.get("id").is_none());
        let unknown = mcp(&json!({"jsonrpc":"2.0","id":3,"method":"resources/list"}), &tools, &mut call);
        assert_eq!(unknown["error"]["code"], -32601);
        assert_eq!(calls, vec![("get_project".to_owned(), json!({"a":1}))]);
    }
}
