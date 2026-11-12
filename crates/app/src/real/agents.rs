//! The user's own coding agents as the assistant's brain.
//!
//! AI-Track never signs in to anything and never sees a credential. It runs
//! the agent CLI the user already installed and signed in to (Claude Code,
//! Codex) as a child process in headless mode, hands it the question on
//! standard input, and reads the answer from standard output. Whatever plan
//! or key that CLI uses is between the user and its vendor.
//!
//! For now the agent answers in text with the project as context. Edits will
//! arrive as reviewable proposals once the MCP server exists (PLAN.md F5).

use crate::bridge::{AgentKind, AgentStatus};
use aitrack_model::{DeviceKind, Project, TrackKind, visit_devices};
use crossbeam_channel::{Receiver, unbounded};
use std::io::Write;
use std::process::{Command, Stdio};

impl AgentKind {
    fn program(self) -> &'static str {
        match self {
            Self::ClaudeCode => "claude",
            Self::Codex => "codex",
        }
    }

    /// Headless arguments; the prompt goes in on standard input.
    fn args(self) -> &'static [&'static str] {
        match self {
            Self::ClaudeCode => &["-p", "--output-format", "text"],
            // Read-only sandbox: it answers, it does not touch files.
            Self::Codex => &["exec", "--skip-git-repo-check", "--sandbox", "read-only", "-"],
        }
    }
}

/// A command for an agent CLI. On Windows the CLIs installed through npm are
/// `.cmd` shims, which only the command interpreter can start; every argument
/// here is a fixed string, never user text.
fn command(kind: AgentKind, args: &[&str]) -> Command {
    let mut c = if cfg!(target_os = "windows") {
        let mut c = Command::new("cmd");
        c.arg("/C").arg(kind.program());
        c
    } else {
        Command::new(kind.program())
    };
    c.args(args).current_dir(std::env::temp_dir()).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW: no console flash
    }
    c
}

/// Whether the CLI is installed, and its version line.
pub fn detect(kind: AgentKind) -> AgentStatus {
    let version = command(kind, &["--version"])
        .stdin(Stdio::null())
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).lines().next().unwrap_or_default().trim().to_owned())
        .filter(|v| !v.is_empty());
    AgentStatus { kind, installed: version.is_some(), version: version.unwrap_or_default() }
}

/// Detect every known agent on a background thread.
pub fn detect_all() -> Receiver<Vec<AgentStatus>> {
    let (tx, rx) = unbounded();
    let _ = std::thread::Builder::new().name("aitrack-agents-detect".into()).spawn(move || {
        let _ = tx.send(AgentKind::ALL.map(detect).to_vec());
    });
    rx
}

/// What the agent is told about the open project.
pub fn project_context(p: &Project) -> String {
    let mut out = String::new();
    out.push_str("You are the assistant inside AI-Track, a digital audio workstation for electronic music. ");
    out.push_str("Answer briefly and practically, as a producer and mix engineer would. ");
    out.push_str("You cannot edit the project yet: when the user asks for a change, give the exact steps to do it by hand. ");
    out.push_str("Do not run tools or read files; everything you need is below.\n\nPROJECT\n");
    out.push_str(&format!("Name: {}\nTempo: {:.2} BPM\nKey: {}\n", p.name, p.tempo.initial_bpm(), p.scale.map_or("not set".to_owned(), |s| s.display())));
    out.push_str("Tracks (in order, with their device chains):\n");
    for t in &p.tracks {
        let kind = match t.kind {
            TrackKind::Instrument => "MIDI",
            TrackKind::Audio => "audio",
            TrackKind::Group => "group",
            TrackKind::Folder => "folder",
            TrackKind::Return => "return",
            TrackKind::Vca => "VCA",
            TrackKind::Main => "main",
        };
        let mut devices = Vec::new();
        visit_devices(&t.devices, &mut |d| {
            if !matches!(d.kind, DeviceKind::Rack(_)) {
                devices.push(d.name.clone());
            }
        });
        let parent = t.parent.and_then(|g| p.track(g)).map(|g| format!(", in group {}", g.name)).unwrap_or_default();
        out.push_str(&format!(
            "- {} ({kind}{parent}), {:+.1} dB{}: {}\n",
            t.name,
            t.volume_db,
            if t.mute { ", muted" } else { "" },
            if devices.is_empty() { "no devices".to_owned() } else { devices.join(" → ") }
        ));
    }
    out
}

/// Ask `kind` a question; the answer (or what went wrong) arrives on the channel.
pub fn ask(kind: AgentKind, context: String, question: String) -> Receiver<Result<String, String>> {
    let (tx, rx) = unbounded();
    let _ = std::thread::Builder::new().name("aitrack-agent".into()).spawn(move || {
        let _ = tx.send(run(kind, &context, &question));
    });
    rx
}

fn run(kind: AgentKind, context: &str, question: &str) -> Result<String, String> {
    let name = kind.name();
    let mut child = command(kind, kind.args()).spawn().map_err(|e| format!("Could not start {name}: {e}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        let prompt = format!("{context}\nQUESTION\n{question}\n");
        stdin.write_all(prompt.as_bytes()).map_err(|e| format!("Could not talk to {name}: {e}"))?;
    }
    let out = child.wait_with_output().map_err(|e| format!("{name} stopped: {e}"))?;
    let text = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    if out.status.success() && !text.is_empty() {
        return Ok(text);
    }
    let err = String::from_utf8_lossy(&out.stderr);
    let reason = err.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("no answer").trim().to_owned();
    Err(format!("{name} did not answer ({reason}). Open a terminal, run `{}` once and sign in, then try again.", kind.program()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_describes_the_project_without_secrets() {
        let text = project_context(&crate::demo::project());
        assert!(text.contains("Tempo: 128.00 BPM") && text.contains("Key: A Minor"));
        assert!(text.contains("- Lead (MIDI, in group Synths)"));
        assert!(text.contains("- Main (main)"));
        assert!(text.contains("→"), "device chains are listed in signal order");
        assert!(!text.to_lowercase().contains("token") && !text.to_lowercase().contains("password"));
    }

    #[test]
    fn a_missing_cli_reads_as_not_installed() {
        // `detect` must never panic or hang when the program is absent.
        for kind in AgentKind::ALL {
            let s = detect(kind);
            assert_eq!(s.installed, !s.version.is_empty());
        }
    }
}
