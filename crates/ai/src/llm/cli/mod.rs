//! The assistant on the user's own subscriptions: Claude Code and Codex run
//! as child processes the user already signed in to, and AI-Track lends
//! them its tools. Claude Code gets them as an in-process MCP server over
//! its stream-json control channel; Codex as app-server dynamic tools. No
//! port is opened and AI-Track never sees a credential.
//!
//! A turn runs on its own thread. Text and thinking stream back as
//! [`CliEvent`]s; a tool call arrives with a reply channel and the thread
//! waits until the app (which owns the project draft) answers it.

mod claude_code;
mod codex;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use crossbeam_channel::{Receiver, Sender, bounded, unbounded};
use serde_json::Value;

use super::agent::Effort;
use crate::CancelToken;

/// Who runs the model.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Provider {
    /// Claude through the Anthropic API with the user's key.
    ClaudeApi,
    /// Claude Code with the user's Claude subscription.
    ClaudeCode,
    /// Codex with the user's ChatGPT subscription.
    Codex,
}

impl Provider {
    pub const ALL: [Provider; 3] = [Provider::ClaudeCode, Provider::Codex, Provider::ClaudeApi];

    pub fn id(self) -> &'static str {
        match self {
            Provider::ClaudeApi => "api",
            Provider::ClaudeCode => "claude-code",
            Provider::Codex => "codex",
        }
    }

    pub fn parse(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.id() == id)
    }

    pub fn label(self) -> &'static str {
        match self {
            Provider::ClaudeApi => "Anthropic API",
            Provider::ClaudeCode => "Claude subscription",
            Provider::Codex => "ChatGPT subscription",
        }
    }

    /// The vendor whose logo it wears.
    pub fn vendor(self) -> Vendor {
        match self {
            Provider::ClaudeApi | Provider::ClaudeCode => Vendor::Anthropic,
            Provider::Codex => Vendor::OpenAi,
        }
    }

    /// The program it runs, if any.
    pub fn program(self) -> Option<&'static str> {
        match self {
            Provider::ClaudeApi => None,
            Provider::ClaudeCode => Some("claude"),
            Provider::Codex => Some("codex"),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Vendor {
    Anthropic,
    OpenAi,
}

/// A model in the picker.
#[derive(Clone, Debug, PartialEq)]
pub struct ModelInfo {
    pub provider: Provider,
    /// What the provider takes (`opus`, `gpt-6-luna`, `claude-opus-5-5`).
    pub id: String,
    pub name: String,
    pub hint: String,
}

/// How many models each provider offers in the picker.
pub const PER_PROVIDER: usize = 4;

/// The models offered when the live list is not in yet (or cannot be read).
pub fn fallback(provider: Provider) -> Vec<ModelInfo> {
    let m = |id: &str, name: &str, hint: &str| ModelInfo { provider, id: id.into(), name: name.into(), hint: hint.into() };
    match provider {
        Provider::ClaudeApi => super::MODELS.iter().map(|c| m(c.id, c.name, c.hint)).collect(),
        Provider::ClaudeCode => vec![
            m("opus", "Opus 5.5", "Best for everyday, complex tasks"),
            m("fable", "Fable 5.2", "Most capable, for the hardest tasks"),
            m("sonnet", "Sonnet 5.5", "Efficient for routine tasks"),
            m("haiku", "Haiku 5.5", "Fastest for quick answers"),
        ],
        Provider::Codex => vec![m("gpt-6-luna", "GPT-6-Luna", "Fast and affordable"), m("gpt-5.6-terra", "GPT-5.6-Terra", "Balanced"), m("gpt-5.6-luna", "GPT-5.6-Luna", "Fast and efficient")],
    }
}

/// Where to find a CLI: `PATH` first, then the places installers use (an
/// app opened from the Dock gets a short `PATH`).
pub fn resolve(program: &str) -> Option<PathBuf> {
    let exe = if cfg!(windows) { [format!("{program}.exe"), format!("{program}.cmd")].to_vec() } else { vec![program.to_owned()] };
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect()).unwrap_or_default();
    if let Some(home) = dirs::home_dir() {
        for d in [".local/bin", ".claude/local", ".npm-global/bin", ".bun/bin", ".volta/bin", ".cargo/bin", "AppData/Roaming/npm"] {
            dirs.push(home.join(d));
        }
    }
    dirs.extend(["/opt/homebrew/bin", "/usr/local/bin"].map(PathBuf::from));
    dirs.iter().flat_map(|d| exe.iter().map(move |e| d.join(e))).find(|p| p.is_file())
}

/// Whether the provider can run here.
pub fn available(provider: Provider) -> bool {
    provider.program().is_none_or(|p| resolve(p).is_some())
}

/// A command for a CLI at `path`, quiet, in a scratch folder. `.cmd` shims
/// on Windows only start through the command interpreter.
fn command(path: &Path) -> Command {
    let mut c = if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("cmd")) {
        let mut c = Command::new("cmd");
        c.arg("/C").arg(path);
        c
    } else {
        Command::new(path)
    };
    let cwd = std::env::temp_dir().join("aitrack-assistant");
    let _ = std::fs::create_dir_all(&cwd);
    c.current_dir(cwd).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
    // The CLIs look for node and their helpers on PATH.
    if let Some(dir) = path.parent() {
        let mut paths: Vec<PathBuf> = vec![dir.to_owned(), PathBuf::from("/opt/homebrew/bin"), PathBuf::from("/usr/local/bin")];
        if let Some(p) = std::env::var_os("PATH") {
            paths.extend(std::env::split_paths(&p));
        }
        if let Ok(joined) = std::env::join_paths(paths) {
            c.env("PATH", joined);
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    c
}

/// The provider's live model list, best first, at most [`PER_PROVIDER`].
/// Blocks for a few seconds: call it off the UI thread.
pub fn catalog(provider: Provider) -> Result<Vec<ModelInfo>, String> {
    let mut models = match provider {
        Provider::ClaudeApi => return Ok(fallback(provider)),
        Provider::ClaudeCode => claude_code::catalog()?,
        Provider::Codex => codex::catalog()?,
    };
    models.truncate(PER_PROVIDER);
    if models.is_empty() {
        return Err("the model list came back empty".into());
    }
    Ok(models)
}

/// One request to a subscription CLI.
#[derive(Clone, Debug)]
pub struct TurnSpec {
    pub provider: Provider,
    pub model: String,
    pub effort: Effort,
    /// Instructions and the project snapshot.
    pub system: String,
    /// The conversation so far and the new request, as one message.
    pub prompt: String,
    /// Screenshots: MIME type and bytes.
    pub images: Vec<(String, Arc<Vec<u8>>)>,
    /// Tool definitions (`name`, `description`, `input_schema`).
    pub tools: Vec<Value>,
}

/// What a running turn reports.
#[derive(Debug)]
pub enum CliEvent {
    Text(String),
    Thinking(String),
    /// The model asked for a tool. Answer on `reply` with the result text
    /// and whether it is an error; the turn waits for it.
    Tool { name: String, input: Value, reply: Sender<(String, bool)> },
    /// The turn ended; `Err` carries what went wrong.
    Done(Result<(), String>),
}

pub struct CliTurn {
    pub events: Receiver<CliEvent>,
    pub cancel: CancelToken,
}

/// Start `spec` on a worker thread.
pub fn start(spec: TurnSpec) -> CliTurn {
    let (tx, events) = unbounded();
    let cancel = CancelToken::new();
    let token = cancel.clone();
    let spawned = std::thread::Builder::new().name("aitrack-cli-turn".into()).spawn(move || {
        let result = match spec.provider {
            Provider::ClaudeCode => claude_code::run(&spec, &token, &tx),
            Provider::Codex => codex::run(&spec, &token, &tx),
            Provider::ClaudeApi => Err("the API runs in the app's own loop".into()),
        };
        if !token.is_cancelled() {
            let _ = tx.send(CliEvent::Done(result));
        }
    });
    if let Err(e) = spawned {
        let (tx, events) = unbounded();
        let _ = tx.send(CliEvent::Done(Err(format!("could not start: {e}"))));
        return CliTurn { events, cancel };
    }
    CliTurn { events, cancel }
}

/// Ask the app to run a tool and wait for its answer (or the cancel).
fn call_tool(tx: &Sender<CliEvent>, cancel: &CancelToken, name: &str, input: Value) -> Option<(String, bool)> {
    let (reply, answer) = bounded(1);
    tx.send(CliEvent::Tool { name: name.to_owned(), input, reply }).ok()?;
    loop {
        if cancel.is_cancelled() {
            return None;
        }
        match answer.recv_timeout(Duration::from_millis(100)) {
            Ok(r) => return Some(r),
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => return None,
        }
    }
}

/// Lines from a child's stdout on their own thread, so reads can time out.
fn lines(stdout: std::process::ChildStdout) -> Receiver<String> {
    let (tx, rx) = unbounded();
    let _ = std::thread::Builder::new().name("aitrack-cli-read".into()).spawn(move || {
        use std::io::BufRead;
        let mut r = std::io::BufReader::new(stdout);
        let mut line = String::new();
        loop {
            line.clear();
            match r.read_line(&mut line) {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    if tx.send(line.trim_end().to_owned()).is_err() {
                        break;
                    }
                }
            }
        }
    });
    rx
}

fn write_json(stdin: &mut std::process::ChildStdin, v: &Value) -> Result<(), String> {
    use std::io::Write;
    let mut s = v.to_string();
    s.push('\n');
    stdin.write_all(s.as_bytes()).and_then(|_| stdin.flush()).map_err(|e| format!("the CLI closed its input: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn providers_round_trip_and_have_fallbacks() {
        for p in Provider::ALL {
            assert_eq!(Provider::parse(p.id()), Some(p));
            assert!(!fallback(p).is_empty());
            assert!(fallback(p).len() <= PER_PROVIDER.max(super::super::MODELS.len()));
        }
        assert_eq!(Provider::Codex.vendor(), Vendor::OpenAi);
        assert!(available(Provider::ClaudeApi));
    }

    /// A real turn on the user's subscription: the model must call our tool
    /// and use its answer. `cargo test -p aitrack-ai live_ -- --ignored`.
    fn live(provider: Provider, model: &str) {
        let tool = serde_json::json!({ "name": "get_project", "description": "The open song: tempo and tracks. Call it to answer questions about the song.", "input_schema": { "type": "object", "properties": {} } });
        let turn = start(TurnSpec {
            provider,
            model: model.into(),
            effort: Effort::Low,
            system: "You are a test. Use the tools you are given.".into(),
            prompt: "What is the tempo of the open song? Call get_project, then answer with the number only.".into(),
            images: Vec::new(),
            tools: vec![tool],
        });
        let mut called = false;
        let mut text = String::new();
        loop {
            match turn.events.recv_timeout(Duration::from_secs(120)).expect("the turn timed out") {
                CliEvent::Text(t) => text.push_str(&t),
                CliEvent::Thinking(_) => {}
                CliEvent::Tool { name, reply, .. } => {
                    assert_eq!(name, "get_project");
                    called = true;
                    reply.send(("Song \"Night\": 137 BPM, tracks: Kick, Bass".into(), false)).unwrap();
                }
                CliEvent::Done(r) => {
                    r.expect("the turn failed");
                    break;
                }
            }
        }
        assert!(called, "no tool call; text: {text}");
        assert!(text.contains("137"), "{text}");
    }

    #[test]
    #[ignore = "runs Claude Code on the user's subscription"]
    fn live_claude_code_calls_a_tool() {
        live(Provider::ClaudeCode, "haiku");
    }

    #[test]
    #[ignore = "runs Codex on the user's subscription"]
    fn live_codex_calls_a_tool() {
        live(Provider::Codex, "gpt-6-luna");
    }

    #[test]
    #[ignore = "starts both CLIs"]
    fn live_catalogs_list_four_models() {
        for p in [Provider::ClaudeCode, Provider::Codex] {
            let m = catalog(p).unwrap();
            assert!(!m.is_empty() && m.len() <= PER_PROVIDER, "{m:?}");
            eprintln!("{p:?}: {:?}", m.iter().map(|m| &m.name).collect::<Vec<_>>());
        }
    }
}
