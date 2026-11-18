//! Slice to Drum Rack through the real engine and the real Sampler: the
//! commands of `aitrack_model::slice` on a one-bar loop, then the new track
//! rendered offline must sound like the loop it was cut from, and every pad's state
//! must load in a Sampler.

use crate::export::render_offline;
use crate::factory::ProcessorFactory;
use aitrack_dsp::{AudioBuffer, Processor};
use aitrack_model::slice::{BeatDivision, SliceMode, slice_to_drum_rack};
use aitrack_model::*;

const SR: u32 = 48_000;

/// Every stock device is the real one.
struct Real;

impl ProcessorFactory for Real {
    fn create(&self, device: &Device, _: f64, _: usize) -> Option<Box<dyn Processor>> {
        let DeviceKind::Stock { plugin_id } = &device.kind else { return None };
        aitrack_stock::create(plugin_id)
    }
}

/// A one-bar (2 s at 120 BPM) loop: a decaying sine burst on every beat, each
/// beat a different pitch, so a wrong slice or a wrong place is audible.
fn write_loop(name: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!("aitrack-slice-{}-{name}.wav", std::process::id()));
    let spec = hound::WavSpec { channels: 2, sample_rate: SR, bits_per_sample: 32, sample_format: hound::SampleFormat::Float };
    let mut w = hound::WavWriter::create(&path, spec).unwrap();
    for i in 0..2 * SR as usize {
        let beat = i / (SR as usize / 2);
        let t = (i % (SR as usize / 2)) as f32 / SR as f32;
        let hz = 220.0 * 2f32.powf(beat as f32 / 2.0);
        let v = 0.6 * (-t * 14.0).exp() * (std::f32::consts::TAU * hz * t).sin();
        w.write_sample(v).unwrap();
        w.write_sample(v).unwrap();
    }
    w.finalize().unwrap();
    path
}

fn project(path: &std::path::Path) -> (Project, TrackId, PlacementId) {
    let mut p = Project::new("slice");
    let (t, src, clip, pl) = (p.alloc_track_id(), p.alloc_source_id(), p.alloc_clip_id(), p.alloc_placement_id());
    Command::AddTrack { track: Box::new(Track::new(t, TrackKind::Audio, "Loop", Color(0))), index: 0 }.apply(&mut p).unwrap();
    let a = AudioSource { path: path.into(), hash: None, sample_rate: SR, channels: 2, frames: 2 * SR as u64, warp: Warp::default(), key: None };
    Command::AddSource { id: src, source: Box::new(Source::Audio(a)) }.apply(&mut p).unwrap();
    Command::AddClip { id: clip, clip: Box::new(Clip::new("Loop", Color(0), src, 4 * PPQ)) }.apply(&mut p).unwrap();
    Command::PlaceClip { track: t, placement: Placement { id: pl, clip, start: 0, lane: 0 } }.apply(&mut p).unwrap();
    (p, t, pl)
}

fn correlation(a: &[f32], b: &[f32]) -> f64 {
    let (mut ab, mut aa, mut bb) = (0f64, 0f64, 0f64);
    for (x, y) in a.iter().zip(b) {
        ab += f64::from(*x) * f64::from(*y);
        aa += f64::from(*x) * f64::from(*x);
        bb += f64::from(*y) * f64::from(*y);
    }
    ab / (aa.sqrt() * bb.sqrt()).max(1e-12)
}

fn render(p: &Project) -> AudioBuffer {
    render_offline(p, &Real, (0, 4 * PPQ), SR as f64, 256).unwrap()
}

#[test]
fn the_sliced_track_plays_the_loop_back_and_the_original_can_be_silenced() {
    let wav = write_loop("beats");
    let (mut p, t, pl) = project(&wav);
    let original = render(&p);
    assert!(original.channel(0).iter().any(|s| s.abs() > 0.1), "the loop sounds");

    let sliced = slice_to_drum_rack(&mut p, t, pl, SliceMode::Beats(BeatDivision::Quarter), &[]).unwrap();
    assert_eq!(sliced.slices, 4);
    for c in sliced.commands {
        c.apply(&mut p).unwrap();
    }
    p.track_mut(t).unwrap().mute = true;
    let again = render(&p);
    let corr = correlation(original.channel(0), again.channel(0));
    assert!(corr > 0.98, "the pads rebuild the loop: correlation {corr}");
    let (a, b): (f32, f32) = (original.channel(0).iter().map(|s| s * s).sum(), again.channel(0).iter().map(|s| s * s).sum());
    assert!((a / b - 1.0).abs() < 0.05, "and at the same level: {a} vs {b}");
    let _ = std::fs::remove_file(wav);
}

#[test]
fn a_rearranged_midi_clip_plays_the_slices_in_the_new_order() {
    let wav = write_loop("order");
    let (mut p, t, pl) = project(&wav);
    let original = render(&p);
    let sliced = slice_to_drum_rack(&mut p, t, pl, SliceMode::Beats(BeatDivision::Quarter), &[]).unwrap();
    for c in sliced.commands {
        c.apply(&mut p).unwrap();
    }
    p.track_mut(t).unwrap().mute = true;
    // Play the last slice's pad first: the first beat must now carry the last beat's pitch.
    let midi = p.track(sliced.track).unwrap().arrangement[0].clone();
    let src = p.clips[&midi.clip].source;
    let Some(Source::Notes(n)) = p.sources.get_mut(&src) else { panic!("notes") };
    n.notes[0].key = 39;
    let out = render(&p);
    let beat = SR as usize / 2;
    let want = &original.channel(0)[3 * beat..3 * beat + 4_000];
    let got = &out.channel(0)[..4_000];
    assert!(correlation(want, got) > 0.98, "pad 4 sounds at beat 1");
    let _ = std::fs::remove_file(wav);
}

#[test]
fn every_pad_state_loads_in_a_sampler() {
    let wav = write_loop("shared");
    let (mut p, t, pl) = project(&wav);
    let sliced = slice_to_drum_rack(&mut p, t, pl, SliceMode::Beats(BeatDivision::Sixteenth), &[]).unwrap();
    assert_eq!(sliced.slices, 16);
    // Each pad's state restores in a Sampler (the file decodes once and is shared).
    let mut samplers: Vec<Box<dyn Processor>> = Vec::new();
    for c in &sliced.commands {
        let Command::AddTrack { track, .. } = c else { continue };
        let DeviceKind::Rack(r) = &track.devices[0].kind else { panic!("rack") };
        for chain in &r.chains {
            let d = &chain.devices[0];
            let mut s = aitrack_stock::create("instrument.sampler").unwrap();
            s.load_state(d.state.as_ref().unwrap());
            samplers.push(s);
        }
    }
    assert_eq!(samplers.len(), 16);
    let _ = std::fs::remove_file(wav);
}
