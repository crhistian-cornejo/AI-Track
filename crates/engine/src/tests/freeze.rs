//! Freeze: a track rendered to a file plays back like the live track did,
//! runs none of its devices, and falls back to live when the file is gone.

use super::*;

fn temp_wav(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("aitrack-freeze-{}-{name}.wav", std::process::id()))
}

/// A synth track with a latency device and a gain device, one two-beat note,
/// a quiet fader and a pan, plus a second synth track that must not be frozen with it.
fn song() -> (P, TrackId, TrackId) {
    let mut b = P::new(120.0);
    let (t, _) = b.synth_track("lead");
    b.device(t, TEST_LATENCY, &[], Some(777u32.to_le_bytes().to_vec()));
    b.device(t, TEST_GAIN, &[(0, 0.25)], None);
    b.t(t).volume_db = -6.0;
    b.t(t).pan = 0.3;
    let c = b.notes(&[(60, PPQ, 2 * PPQ), (64, 3 * PPQ, PPQ)], 4 * PPQ, None);
    b.place(t, c, 0);
    let (other, _) = b.synth_track("bass");
    let c2 = b.notes(&[(40, 0, 4 * PPQ)], 4 * PPQ, None);
    b.place(other, c2, 0);
    (b, t, other)
}

fn edit(p: &mut Project, cmd: Command) {
    cmd.apply(p).expect("command applies");
}

fn freeze(b: &mut P, t: TrackId, path: &std::path::Path) -> AudioSource {
    let file = freeze_track_to_wav(&b.p, t, 48_000, path, None, &FallbackFactory, None).expect("freeze renders");
    edit(&mut b.p, Command::FreezeTrack { track: t, file: file.clone() });
    file
}

#[test]
fn a_frozen_track_plays_what_the_live_track_did() {
    let (mut b, t, other) = song();
    let live = render(&b.p, (0, 4 * PPQ));
    assert!(peak(live.channel(0)) > 0.1);

    // Freezing renders the track alone, before its fader.
    let path = temp_wav("null");
    let file = freeze(&mut b, t, &path);
    assert_eq!((file.channels, file.sample_rate), (2, 48_000));
    assert!(file.frames >= 4 * BEAT as u64, "at least the clip range");
    assert_eq!((file.warp.enabled, file.warp.original_bpm), (true, Some(120.0)));

    let g = compile_plain(&b.p).unwrap();
    let node = &g.tracks[g.track_index(t).unwrap()];
    assert!(node.devices.is_empty(), "a frozen track runs no devices");
    assert!(!g.tracks[g.track_index(other).unwrap()].devices.is_empty(), "other tracks are untouched");
    assert!(g.warnings.is_empty(), "{:?}", g.warnings);
    drop(g);

    // Same samples as live (the fader and pan still apply on top of the render).
    let frozen = render(&b.p, (0, 4 * PPQ));
    for ch in 0..2 {
        let (a, f) = (live.channel(ch), frozen.channel(ch));
        let worst = a.iter().zip(f).map(|(x, y)| (x - y).abs()).fold(0.0f32, f32::max);
        assert!(worst < 1e-4, "channel {ch} differs by {worst}");
    }

    // The mixer is still live while frozen.
    edit(&mut b.p, Command::SetTrack { id: t, change: TrackProp::Mute(true) });
    edit(&mut b.p, Command::SetTrack { id: other, change: TrackProp::Mute(true) });
    assert_eq!(peak(render(&b.p, (0, 4 * PPQ)).channel(0)), 0.0);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn the_render_is_only_the_track_and_keeps_its_ring_out() {
    let (mut b, t, _) = song();
    // The file holds the lead before its fader: louder than the mix of it with the fader at -6 dB.
    let path = temp_wav("alone");
    let file = freeze_track_to_wav(&b.p, t, 48_000, &path, None, &FallbackFactory, None).unwrap();
    let (decoded, rate) = crate::sample_cache::decode_file(&path).unwrap();
    assert_eq!((decoded.len(), rate), (2, 48_000));
    let lead_peak = peak(&decoded[0]);
    assert!(lead_peak > 0.05);
    // The bass plays from beat 0, the lead only from beat 1.
    assert!(peak(&decoded[0][..BEAT - 1]) < 1e-6, "the other track is not in the render");
    assert!(file.frames as usize >= 4 * BEAT);
    // The fader (-6 dB) and the pan are not baked in: the file is as loud as the lead's own output.
    b.t(t).volume_db = 0.0;
    b.t(t).pan = 0.0;
    let (lead, _) = (b.p.track(t).unwrap().id, ());
    b.t(b.p.tracks.iter().find(|x| x.name == "bass").unwrap().id).mute = true;
    let straight = render(&b.p, (0, 4 * PPQ));
    assert_eq!(lead, t);
    assert!((peak(straight.channel(0)) - lead_peak).abs() < 1e-3, "{} vs {lead_peak}", peak(straight.channel(0)));
    let _ = std::fs::remove_file(&path);
}

#[test]
fn a_missing_frozen_file_falls_back_to_the_devices() {
    let (mut b, t, _) = song();
    let live = render(&b.p, (0, 4 * PPQ));
    let path = temp_wav("gone");
    freeze(&mut b, t, &path);
    std::fs::remove_file(&path).unwrap();
    let g = compile_plain(&b.p).unwrap();
    let node = &g.tracks[g.track_index(t).unwrap()];
    assert!(!node.devices.is_empty(), "back to live");
    assert!(g.warnings.iter().any(|w| w.contains("frozen")), "{:?}", g.warnings);
    drop(g);
    let again = render(&b.p, (0, 4 * PPQ));
    let worst = live.channel(0).iter().zip(again.channel(0)).map(|(x, y)| (x - y).abs()).fold(0.0f32, f32::max);
    assert!(worst < 1e-6);
}

#[test]
fn a_cancelled_freeze_leaves_no_file_and_other_kinds_are_refused() {
    let (b, t, _) = song();
    let path = temp_wav("cancel");
    let mut stop = |_: f32| false;
    let r = freeze_track_to_wav(&b.p, t, 48_000, &path, None, &FallbackFactory, Some(&mut stop));
    assert!(matches!(r, Err(crate::EngineError::Cancelled)));
    assert!(!path.exists());
    let main = b.main();
    assert!(freeze_track_to_wav(&b.p, main, 48_000, &path, None, &FallbackFactory, None).is_err());
}

#[test]
fn a_flattened_track_plays_the_render_without_devices() {
    let (mut b, t, _) = song();
    let live = render(&b.p, (0, 4 * PPQ));
    let path = temp_wav("flat");
    freeze(&mut b, t, &path);
    let (source, clip, placement) = (b.p.alloc_source_id(), b.p.alloc_clip_id(), b.p.alloc_placement_id());
    edit(&mut b.p, Command::FlattenTrack { track: t, source, clip, placement });
    let track = b.p.track(t).unwrap();
    assert_eq!((track.kind, track.devices.len()), (TrackKind::Audio, 0));
    let flat = render(&b.p, (0, 4 * PPQ));
    let worst = live.channel(1).iter().zip(flat.channel(1)).map(|(x, y)| (x - y).abs()).fold(0.0f32, f32::max);
    assert!(worst < 1e-4, "flattened audio differs by {worst}");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn a_silent_take_does_not_lengthen_the_freeze() {
    let (mut b, t, _) = song();
    let end = crate::freeze_end(&b.p, t);
    assert_eq!(end, 4 * PPQ);
    edit(&mut b.p, Command::AddTakeLane { track: t, index: 0, name: "Take 1".into() });
    let long = b.notes(&[(60, 0, PPQ)], 40 * PPQ, None);
    let id = b.p.alloc_placement_id();
    edit(&mut b.p, Command::PlaceClip { track: t, placement: Placement { id, clip: long, start: 0, lane: 1 } });
    assert_eq!(crate::freeze_end(&b.p, t), end, "a take lane is silent");
}
