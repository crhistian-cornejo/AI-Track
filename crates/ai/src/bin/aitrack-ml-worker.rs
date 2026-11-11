//! `aitrack-ml-worker`: runs analysis, audio-to-MIDI and stem separation jobs
//! in a separate process, speaking JSON lines on stdin/stdout
//! (see `aitrack_ai::worker::{Request, Response}`). Logs go to stderr.
//!
//! ```text
//! > {"id":1,"job":{"cmd":"analyze","path":"kick.wav"}}
//! < {"event":"done","id":1,"output":{"type":"analysis","analysis":{...}}}
//! > {"cancel":1}
//! > {"shutdown":true}
//! ```
//!
//! `--models-dir <dir>` overrides the models directory.

use std::io::{self, BufReader};

use aitrack_ai::models::ModelManager;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let manager = match args
        .iter()
        .position(|a| a == "--models-dir")
        .and_then(|i| args.get(i + 1))
    {
        Some(dir) => ModelManager::with_dir(dir),
        None => match ModelManager::new() {
            Ok(m) => m,
            Err(e) => {
                eprintln!("aitrack-ml-worker: {e}");
                std::process::exit(2);
            }
        },
    };
    aitrack_ai::worker::serve(manager, BufReader::new(io::stdin()), io::stdout());
}
