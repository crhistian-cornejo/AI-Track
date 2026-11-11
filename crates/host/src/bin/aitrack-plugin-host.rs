//! Sandbox process: hosts one plugin for the DAW over shared memory.
//! Spawned by `aitrack_host::sandbox::SandboxedPlugin`; speaks the control
//! protocol on stdin/stdout. See `aitrack_host::sandbox::protocol`.

fn main() {
    std::process::exit(aitrack_host::sandbox::server::host_main());
}
