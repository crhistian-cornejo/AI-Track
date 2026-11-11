# aitrack-host

Third-party plugin hosting for AI-Track: **VST3** and **CLAP** (no VST2, no AU),
on Windows and macOS.

| Piece | Where |
|---|---|
| Format-neutral description stored in the cache and in projects | `src/info.rs` (`HostedPluginInfo`) |
| Plugin as an `aitrack_dsp::Processor` + main-thread handle | `src/plugin.rs` (`HostedPlugin`, `PluginController`) |
| VST3 back end (`vst3` crate, MIT/Apache, generated from the MIT VST3 SDK 3.8) | `src/vst3/` |
| CLAP back end (`clack-host` / `clack-extensions`, MIT/Apache) | `src/clap/` |
| Editor windows (HWND / NSWindow) | `src/editor/` |
| Scanner, cache, blocklist, `moduleinfo.json` | `src/scan/`, `src/bin/aitrack-plugin-scanner.rs` |
| Out-of-process sandbox | `src/sandbox/`, `src/bin/aitrack-plugin-host.rs` |

No GPL code or bindings: `vst3-sys` (GPLv3) is banned in `deny.toml`.

## Using it

```rust
use aitrack_dsp::Processor;
use aitrack_host::{HostedPlugin, scan::Scanner};

// Main thread.
let mut scanner = Scanner::new(cache_dir.join("plugins.json"));
let report = scanner.scan()?;                 // incremental, out of process
let info = scanner.plugins().into_iter().find(|p| p.name == "Surge XT").unwrap();

let mut plugin = HostedPlugin::load(&info)?;  // this thread becomes its main thread
plugin.prepare(48_000.0, 512);                // activate (main thread, not processing)
let controller = plugin.controller();         // Clone + Send + Sync handle
let edits = plugin.take_param_edits();        // GUI edits → automation/undo
controller.open_editor()?;

// Move `plugin` to the audio thread; call `process`, `set_param`, `get_param`.
// On the UI tick: `controller.idle()`; if `plugin.take_needs_restart()`,
// bring it back and call `prepare` again.
```

`SandboxedPlugin::spawn(&info)` gives the same `Processor` interface with the
plugin running in an `aitrack-plugin-host` process.

## Threading model

A hosted plugin has two halves.

- **Audio half** (`HostedPlugin`, `Send`): `process`, `set_param`, `get_param`,
  `reset`, `latency`, `tail`. Allocation-free and lock-free: event lists and
  parameter queues are preallocated, parameter values live in an atomic table,
  GUI edits arrive through `rtrb` queues.
- **Main half**: VST3 `IEditController`/`IPlugView`/`IComponent` state, CLAP
  `[main-thread]` calls, the editor window. It sits in a `MainThreadCell`
  owned by the thread that called `HostedPlugin::load`; every access checks
  the calling thread and fails with `HostError::WrongThread` elsewhere.
  `PluginController` is the handle to it.

Rules:

1. Load, `prepare`, `save_state`/`load_state`, editor calls and `idle` run on
   the main thread. `prepare` also needs the audio half (it re-activates), so
   the engine calls it while the node is out of the graph.
2. `idle()` from the UI tick (30–60 Hz) delivers CLAP `request_callback`,
   CLAP timers, VST3 `restartComponent` flags, host→controller parameter sync
   (`setParamNormalized`), editor resize and close.
3. Plugin → host callbacks never do work in place: they set atomics
   (`needs_restart`, `latency_changed`, `state_dirty`, resize requests) that
   `idle` or the next `prepare` act on.
4. GUI edits: VST3 `IComponentHandler::performEdit` (main thread) pushes to a
   queue drained by `process` (into `inputParameterChanges`) and to the DAW's
   `ParamEdit` queue (`Begin` / `Value` / `End` = one undo step). CLAP GUI
   edits come back as output events of `process` and take the same queue.
5. Drop a `HostedPlugin` on its main thread. Dropped elsewhere it is leaked on
   purpose rather than destroyed on the wrong thread.

Parameters are exposed as normalised `0..=1` `ParamInfo`s. VST3 is natively
normalised (stepped parameters become `Stepped` with `max = stepCount`); CLAP
plain values are mapped linearly between `min_value` and `max_value`.
Automation (`EventKind::Param`) is sample-accurate in both formats. VST3 has
no MIDI CC events, so CC, pitch bend and channel pressure go through
`IMidiMapping` to parameters.

Crash containment in process: every call into a plugin is wrapped in
`catch_unwind`. A panic disables the plugin (silence from then on);
non-finite output is replaced by zeros. Access violations inside a plugin
cannot be caught in process; they are the sandbox's job.

## Scanner

- Search paths: Windows `%CommonProgramFiles%\VST3|CLAP` and
  `%LOCALAPPDATA%\Programs\Common\VST3|CLAP`; macOS
  `/Library/Audio/Plug-Ins/VST3|CLAP` and `~/Library/Audio/Plug-Ins/VST3|CLAP`;
  plus `CLAP_PATH` and user folders (`Scanner::add_search_path`).
- Cache: JSON keyed by path, validated by the binary's mtime and size; written
  atomically. Unchanged plugins are never loaded again.
- VST3 bundles with `Contents/Resources/moduleinfo.json` are listed without
  loading the binary (parameter counts are then unknown, `probed = false`).
- Everything else is loaded by `aitrack-plugin-scanner <vst3|clap> <path>` in
  its own process, several in parallel, killed after 20 s. The result is the
  last stdout line prefixed with `@@AITRACK_SCAN_RESULT@@`, so plugin chatter
  is ignored; the process exits without running plugin destructors.
- Timeouts, crashes and load errors go to a persistent blocklist with the
  same key; a changed file or `Scanner::unblock` retries it.
- VST3 class ids are stored in the platform-independent `INLINE_UID` form
  (four 32-bit words), the same as `moduleinfo.json`, even though Windows
  keeps the bytes COM-swapped in memory.

## Sandbox protocol

One `aitrack-plugin-host` process per sandboxed plugin (grouping several per
process is planned).

**Control** (not real-time): JSON lines on the child's stdin/stdout. Replies
start with `@@AITRACK_SBX@@`. Requests: `load`, `prepare`, `save_state`,
`load_state`, `open_editor`, `close_editor`, `shutdown`. EOF on stdin (parent
gone) shuts the child down.

**Audio** (real-time): one shared-memory region per instance
(`CreateFileMappingW` / `shm_open`) and two signals, `request` and `done`
(named auto-reset events on Windows, POSIX named semaphores on macOS):

```
ShmHeader (repr C, 64-byte aligned)
  magic, version, max_frames, in/out channels, capacities
  DAW → sandbox: seq (AtomicU64), frames, n_events, sample_rate, transport
  sandbox → DAW: done_seq (AtomicU64), n_edits, latency, tail, status
audio in   [in_channels][max_frames] f32
audio out  [out_channels][max_frames] f32
events     [1024] ShmEvent   (note on/off, expression, param, CC, bend…)
edits      [256]  ShmEdit    (GUI parameter edits: begin/value/end)
```

Per block, **in the same audio callback**:

1. The DAW writes input audio, events and transport, stores `seq = n`
   (release) and posts `request`.
2. The sandbox's RT thread wakes, runs the plugin, writes output audio, edits
   and latency, stores `done_seq = n` (release) and posts `done`.
3. The DAW waits on `done` until `done_seq == n` or the budget runs out (70 %
   of the block period by default). On time, the output is used in the same
   cycle, so the sandbox adds **no buffer of latency**. Late, the node outputs
   silence; no new request is issued until the sandbox catches up
   (`done_seq == seq`), so both sides never touch the same buffers.

Measured on the test fixture (debug build, Windows): about 14 µs round trip
per 256-frame block, no late blocks out of 500.

Editors in the sandbox are top-level windows of the child process (NSViews
cannot be embedded across processes on macOS).

Status: working end to end (load, process, sample-accurate parameters, GUI
edits back to the DAW, latency, state, editor open/close). Missing: several
plugins per process, sidechain over shared memory, MMCSS / audio-workgroup
joining of the sandbox RT thread, restart of a crashed sandbox with its last
state. In-process hosting stays the default until those land.

## macOS signing

A signed DAW with the **hardened runtime** refuses to load libraries signed by
other teams. Every binary that loads plugins (`aitrack`, `aitrack-plugin-host`,
`aitrack-plugin-scanner`) needs:

```xml
<key>com.apple.security.cs.disable-library-validation</key><true/>
```

Also:

- `com.apple.security.device.audio-input` on the main app for recording.
- `com.apple.security.cs.allow-unsigned-executable-memory` and/or
  `com.apple.security.cs.allow-jit` are needed by some plugins (JIT engines,
  copy protection). Give them **only** to `aitrack-plugin-host`, which is
  another reason to run third-party plugins in the sandbox.
- Sign with Developer ID + hardened runtime (`codesign --options runtime
  --entitlements …`) and notarise; the helper binaries inside the app bundle
  are signed individually with their own entitlements.
- An `x86_64` build of `aitrack-plugin-host` running under Rosetta can host
  Intel-only plugins from the native arm64 DAW.

## Tests

```
cargo test -p aitrack-host
cargo test -p aitrack-host --test system_plugins -- --ignored --nocapture
```

The integration tests build `tests/fixtures/test-gain` (a standalone crate,
not a workspace member) into `target/host-fixtures` and lay it out as
`AITrackTestGain.clap` and `AITrackTestGain.vst3`. The one library exports
a hand-written CLAP gain plugin and a split-component VST3 gain plugin, so
both back ends, the scanner and the sandbox are tested deterministically
without any installed plugin. The `system_plugins` tests scan the real
folders and load the first installed VST3.

On Windows, if a rebuild fails with `LNK1103` (corrupt debug information),
run `cargo clean -p aitrack-host` and build again. It comes from very large
generic VST3 binding code; `debug = "line-tables-only"` for this crate's dev
profile avoids it.
