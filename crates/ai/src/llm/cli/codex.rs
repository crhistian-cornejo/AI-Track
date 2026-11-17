//! Codex through `codex app-server` (JSON-RPC over stdio). AI-Track's tools
//! go in as the thread's dynamic tools; Codex calls them with
//! `item/tool/call` requests that we answer.

use std::time::{Duration, Instant};

use crossbeam_channel::{RecvTimeoutError, Sender};
use serde_json::{Value, json};

use super::{CliEvent, ModelInfo, Provider, TurnSpec, call_tool, command, lines, resolve, write_json};
use crate::CancelToken;
use crate::llm::agent::Effort;

fn initialize() -> Value {
    json!({"id":1,"method":"initialize","params":{"clientInfo":{"name":"aitrack","title":"AI-Track","version":env!("CARGO_PKG_VERSION")},"capabilities":{"experimentalApi":true}}})
}

/// The models `model/list` offers, best first.
pub fn catalog() -> Result<Vec<ModelInfo>, String> {
    let path = resolve("codex").ok_or("Codex is not installed")?;
    let mut child = command(&path).arg("app-server").spawn().map_err(|e| format!("could not start Codex: {e}"))?;
    let mut stdin = child.stdin.take().ok_or("no stdin")?;
    let rx = lines(child.stdout.take().ok_or("no stdout")?);
    let result = (|| {
        write_json(&mut stdin, &initialize())?;
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let line = rx.recv_timeout(deadline.saturating_duration_since(Instant::now())).map_err(|_| "Codex did not list its models in time".to_owned())?;
            let Ok(v) = serde_json::from_str::<Value>(&line) else { continue };
            if v.get("method").is_some() && v.get("id").is_some() {
                write_json(&mut stdin, &json!({"id":v["id"],"error":{"code":-32601,"message":"unsupported"}}))?;
                continue;
            }
            if v["id"] == 1 {
                if let Some(e) = v.get("error").filter(|e| !e.is_null()) {
                    return Err(e["message"].as_str().unwrap_or("Codex did not start").to_owned());
                }
                write_json(&mut stdin, &json!({"method":"initialized"}))?;
                write_json(&mut stdin, &json!({"id":2,"method":"model/list","params":{"limit":50}}))?;
            }
            if v["id"] == 2 {
                if let Some(e) = v.get("error").filter(|e| !e.is_null()) {
                    return Err(e["message"].as_str().unwrap_or("Codex could not list its models").to_owned());
                }
                return Ok(models(&v["result"]["data"]));
            }
        }
    })();
    drop(stdin);
    let _ = child.kill();
    let _ = child.wait();
    result
}

fn models(rows: &Value) -> Vec<ModelInfo> {
    let mut out: Vec<ModelInfo> = rows
        .as_array()
        .into_iter()
        .flatten()
        .filter(|r| r["hidden"] != true)
        .filter_map(|r| {
            let id = r["id"].as_str().or_else(|| r["model"].as_str())?;
            Some((r["isDefault"] == true, ModelInfo { provider: Provider::Codex, id: id.to_owned(), name: r["displayName"].as_str().unwrap_or(id).to_owned(), hint: r["description"].as_str().unwrap_or_default().to_owned() }))
        })
        .collect::<Vec<_>>()
        .into_iter()
        .fold(Vec::new(), |mut acc: Vec<(bool, ModelInfo)>, m| {
            if !acc.iter().any(|(_, x)| x.id == m.1.id) {
                acc.push(m);
            }
            acc
        })
        .into_iter()
        .map(|(_, m)| m)
        .collect();
    // The default (the newest recommended) first; the rest keep Codex's order.
    if let Some(i) = rows.as_array().and_then(|r| r.iter().filter(|r| r["hidden"] != true).position(|r| r["isDefault"] == true))
        && i < out.len()
    {
        let d = out.remove(i);
        out.insert(0, d);
    }
    out
}

fn effort(e: Effort) -> &'static str {
    match e {
        Effort::Low => "low",
        Effort::Medium => "medium",
        Effort::High => "high",
        Effort::Max => "xhigh",
    }
}

/// The dynamic tool specs for `thread/start`.
pub(super) fn dynamic_tools(tools: &[Value]) -> Vec<Value> {
    tools.iter().map(|t| json!({ "type": "function", "name": t["name"], "description": t["description"], "inputSchema": t["input_schema"] })).collect()
}

/// The answer to an `item/tool/call`.
pub(super) fn tool_answer(text: &str, is_error: bool) -> Value {
    json!({ "success": !is_error, "contentItems": [{ "type": "inputText", "text": text }] })
}

pub fn run(spec: &TurnSpec, cancel: &CancelToken, tx: &Sender<CliEvent>) -> Result<(), String> {
    let path = resolve("codex").ok_or("Codex is not installed. Install it (npm i -g @openai/codex) and sign in with ChatGPT, then try again.")?;
    // Screenshots go in as files.
    let dir = std::env::temp_dir().join("aitrack-assistant");
    let mut images = Vec::new();
    for (i, (mime, data)) in spec.images.iter().enumerate() {
        let ext = mime.rsplit('/').next().unwrap_or("png");
        let p = dir.join(format!("shot-{}-{i}.{ext}", std::process::id()));
        if std::fs::write(&p, data.as_slice()).is_ok() {
            images.push(p);
        }
    }
    let mut child = command(&path).arg("app-server").spawn().map_err(|e| format!("could not start Codex: {e}"))?;
    let mut stdin = child.stdin.take().ok_or("no stdin")?;
    let rx = lines(child.stdout.take().ok_or("no stdout")?);
    let result = (|| {
        write_json(&mut stdin, &initialize())?;
        loop {
            if cancel.is_cancelled() {
                return Ok(());
            }
            let line = match rx.recv_timeout(Duration::from_millis(100)) {
                Ok(l) => l,
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => return Err("Codex stopped before it answered. Check that you are signed in (run `codex` once in a terminal).".into()),
            };
            let Ok(v) = serde_json::from_str::<Value>(&line) else { continue };
            if let Some(e) = v.get("error").filter(|e| !e.is_null() && v.get("method").is_none()) {
                return Err(e["message"].as_str().unwrap_or("Codex refused the request").to_owned());
            }
            match v["id"].as_u64().filter(|_| v.get("method").is_none()) {
                Some(1) => {
                    write_json(&mut stdin, &json!({"method":"initialized"}))?;
                    write_json(
                        &mut stdin,
                        &json!({"id":2,"method":"thread/start","params":{
                            "cwd": dir, "approvalPolicy": "never", "sandbox": "read-only", "ephemeral": true,
                            "model": spec.model, "developerInstructions": spec.system, "dynamicTools": dynamic_tools(&spec.tools),
                        }}),
                    )?;
                }
                Some(2) => {
                    let thread = v["result"]["thread"]["id"].as_str().ok_or("Codex gave no thread")?.to_owned();
                    let mut input = vec![json!({ "type": "text", "text": spec.prompt, "text_elements": [] })];
                    input.extend(images.iter().map(|p| json!({ "type": "localImage", "path": p })));
                    write_json(&mut stdin, &json!({"id":3,"method":"turn/start","params":{"threadId":thread,"input":input,"effort":effort(spec.effort)}}))?;
                }
                _ => {}
            }
            let p = &v["params"];
            match v["method"].as_str().unwrap_or_default() {
                "item/agentMessage/delta" => {
                    let _ = tx.send(CliEvent::Text(p["delta"].as_str().unwrap_or_default().to_owned()));
                }
                "item/reasoning/summaryTextDelta" | "item/reasoning/textDelta" => {
                    let _ = tx.send(CliEvent::Thinking(p["delta"].as_str().unwrap_or_default().to_owned()));
                }
                "item/tool/call" => {
                    let name = p["tool"].as_str().unwrap_or_default();
                    let args = if p["arguments"].is_object() { p["arguments"].clone() } else { json!({}) };
                    let (text, err) = call_tool(tx, cancel, name, args).unwrap_or_else(|| ("Stopped by the user.".into(), true));
                    write_json(&mut stdin, &json!({ "id": v["id"], "result": tool_answer(&text, err) }))?;
                }
                // Codex's own shell and file edits are not part of this job.
                "item/commandExecution/requestApproval" | "item/fileChange/requestApproval" | "execCommandApproval" | "applyPatchApproval" => {
                    write_json(&mut stdin, &json!({ "id": v["id"], "result": { "decision": "decline" } }))?;
                }
                "turn/completed" => {
                    let turn = &p["turn"];
                    return match turn["status"].as_str() {
                        Some("completed") | None => Ok(()),
                        Some("interrupted") => Err("Codex stopped the turn.".into()),
                        Some(_) => Err(turn["error"]["message"].as_str().unwrap_or("Codex could not finish the request").to_owned()),
                    };
                }
                "error" => {
                    if p["willRetry"] != true {
                        return Err(p["error"]["message"].as_str().unwrap_or("Codex reported an error").to_owned());
                    }
                }
                m if !m.is_empty() && v.get("id").is_some() => {
                    write_json(&mut stdin, &json!({ "id": v["id"], "error": { "code": -32601, "message": "unsupported" } }))?;
                }
                _ => {}
            }
        }
    })();
    drop(stdin);
    let _ = child.kill();
    let _ = child.wait();
    for p in images {
        let _ = std::fs::remove_file(p);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_list_puts_the_default_first_and_hides_hidden() {
        let rows = json!([
            {"id":"gpt-5.6-terra","displayName":"GPT-5.6-Terra","description":"Older","isDefault":false},
            {"id":"secret","displayName":"S","hidden":true},
            {"id":"gpt-6-luna","displayName":"GPT-6-Luna","description":"Fast","isDefault":true},
        ]);
        let m = models(&rows);
        assert_eq!(m.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(), ["gpt-6-luna", "gpt-5.6-terra"]);
    }

    #[test]
    fn tools_and_answers_have_the_app_server_shape() {
        let t = dynamic_tools(&[json!({"name":"get_project","description":"d","input_schema":{"type":"object"}})]);
        assert_eq!(t[0]["type"], "function");
        assert_eq!(t[0]["inputSchema"]["type"], "object");
        assert_eq!(tool_answer("bad", true)["success"], false);
        assert_eq!(tool_answer("ok", false)["contentItems"][0]["type"], "inputText");
    }
}
