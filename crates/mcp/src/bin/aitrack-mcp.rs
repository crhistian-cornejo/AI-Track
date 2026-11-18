//! stdio shim for MCP clients that cannot speak HTTP: it forwards every
//! JSON-RPC line on stdin to the running AI-Track's MCP endpoint (port and
//! token are read from the config folder) and prints the answers on stdout.

use std::io::{self, BufReader};
use std::process::ExitCode;

fn main() -> ExitCode {
    let endpoint = match aitrack_mcp::shim::Endpoint::from_config(&aitrack_mcp::settings::config_dir()) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("aitrack-mcp: {e}");
            return ExitCode::FAILURE;
        }
    };
    let stdin = io::stdin();
    let mut stdout = io::stdout().lock();
    match aitrack_mcp::shim::run(BufReader::new(stdin.lock()), &mut stdout, &endpoint) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("aitrack-mcp: {e}");
            ExitCode::FAILURE
        }
    }
}
