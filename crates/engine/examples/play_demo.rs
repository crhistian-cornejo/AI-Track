//! Manual check of the live path: opens the default audio device, plays a
//! short demo (chords on the built-in test synth plus the metronome) for a
//! few seconds, prints meters and CPU load, then exits.
//!
//! ```text
//! cargo run -p aitrack-engine --example play_demo            # audible
//! cargo run -p aitrack-engine --example play_demo -- --quiet # Main muted
//! cargo run -p aitrack-engine --example play_demo -- --list  # list devices
//! ```

use aitrack_engine::fallback::{FallbackFactory, TEST_SYNTH};
use aitrack_engine::{AudioConfig, Engine, EngineConfig};
use aitrack_model::*;
use std::sync::Arc;
use std::time::Duration;

fn demo_project(quiet: bool) -> (Project, TrackId) {
    let mut p = Project::new("demo");
    p.tempo = TempoMap::constant(124.0);
    if quiet {
        let main = p.main_track().id;
        p.track_mut(main).unwrap().mute = true;
    }
    let tid = p.alloc_track_id();
    let mut t = Track::new(tid, TrackKind::Instrument, "Chords", Color::palette(3));
    t.volume_db = -12.0;
    let did = p.alloc_device_id();
    let mut synth = Device::stock(did, TEST_SYNTH, "Synth");
    synth.params.insert(0, 1.0); // saw
    synth.params.insert(1, 0.01); // attack
    synth.params.insert(4, 0.1); // release
    synth.params.insert(5, 0.3);
    t.devices.push(synth);

    let sid = p.alloc_source_id();
    let mut src = NoteSource::default();
    let chords: [[u8; 3]; 4] = [[57, 60, 64], [53, 57, 60], [48, 52, 55], [55, 59, 62]];
    for (bar, chord) in chords.iter().enumerate() {
        for &key in chord {
            let nid = p.alloc_note_id();
            src.notes.push(Note::new(nid, key, bar as Tick * 4 * PPQ, 4 * PPQ - PPQ / 4, 0.8));
        }
    }
    p.sources.insert(sid, Source::Notes(src));
    let cid = p.alloc_clip_id();
    p.clips.insert(
        cid,
        Clip {
            name: "Chords".into(),
            color: Color::palette(3),
            source: sid,
            offset: 0,
            length: 16 * PPQ,
            looping: Some(LoopRegion { start: 0, end: 16 * PPQ }),
            gain_db: 0.0,
            envelopes: Vec::new(),
            launch: LaunchSettings::default(),
            muted: false,
            scale: None,
        },
    );
    let pid = p.alloc_placement_id();
    t.arrangement.push(Placement { id: pid, clip: cid, start: 0, lane: 0 });
    p.tracks.push(t);
    (p, tid)
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--list") {
        for d in aitrack_engine::audio_io::devices() {
            println!("{:10} {:40} out {:2} in {:2} rates {:?} buffers {:?}", d.host, d.name, d.output_channels, d.input_channels, d.sample_rates, d.buffer_range);
        }
        return;
    }
    let quiet = args.iter().any(|a| a == "--quiet");
    let config = EngineConfig { audio: Some(AudioConfig { buffer_size: Some(256), ..AudioConfig::default() }), ..EngineConfig::default() };
    let (engine, _) = Engine::new(config, Arc::new(FallbackFactory)).expect("engine");
    match engine.active_audio() {
        Some(a) => println!("playing on {} / {} at {} Hz, buffer {:?} {:?}", a.host, a.output_device, a.sample_rate, a.buffer_size, a.notes),
        None => {
            println!("no audio device: {:?}", engine.last_error());
            return;
        }
    }
    let (project, track) = demo_project(quiet);
    engine.update_project(&project);
    engine.flush();
    // The metronome is mixed after Main, so `--quiet` must disable it too.
    engine.set_metronome(!quiet);
    engine.play();
    for _ in 0..8 {
        std::thread::sleep(Duration::from_millis(500));
        let m = engine.meters(track).unwrap_or_default();
        let (cpu, peak) = engine.cpu_load();
        println!(
            "tick {:8.0}  peak {:5.3}  rms {:5.3}  cpu {:4.1}% (peak {:4.1}%)  xruns {}",
            engine.position_ticks(),
            m.peak[0],
            m.rms[0],
            cpu * 100.0,
            peak * 100.0,
            engine.xruns()
        );
    }
    engine.stop();
    std::thread::sleep(Duration::from_millis(100));
}
