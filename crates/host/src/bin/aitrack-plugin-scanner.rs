//! Out-of-process plugin scanner: `aitrack-plugin-scanner <vst3|clap> <path>`.
//!
//! Loads one module, describes its plugins and prints a single result line
//! (prefixed with `aitrack_host::scan::RESULT_SENTINEL`) on stdout. If the
//! plugin crashes or hangs, only this process dies; the DAW's scanner
//! blocklists the file.

fn main() {
    aitrack_host::sandbox::disable_crash_dialogs();
    let args: Vec<String> = std::env::args().collect();
    std::process::exit(aitrack_host::scan::scanner_main(&args));
}
