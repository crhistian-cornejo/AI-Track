//! Grow pack: one press makes a whole starting idea. A Bloom pack
//! (`aitrack_stock::instrument::bloom::pack`, one producer's palette) lands
//! as one new MIDI track per member, each with a Bloom of that kind holding
//! the pack's sounds and a clip from `aitrack_ai::generate::pack` (one key,
//! one progression, one groove), as **one undo step**.
//!
//! Wiring (the Bloom window's Tools tab and the command palette):
//!
//! ```ignore
//! use crate::views::device_ui::bloom::window::pack::{PackOpts, grow_pack};
//! grow_pack(ws, PackOpts::default(), cx);
//! ```
//!
//! `PackOpts` picks the members, bars, genre, style and seed; the default is
//! drums, bass, lead, pad, pluck and FX over four bars at the loop's start
//! (or bar 1), in the project's key and a genre from its tempo.

use aitrack_ai::generate::pack::{self as plan, PackRequest};
use aitrack_ai::generate::{Genre, digest};
use aitrack_model::{Clip, Color, Command, Device, Note, NoteSource, Placement, Project, Source, Tick, Track, TrackId, TrackKind};
use aitrack_stock::instrument::bloom::pack::{self as bpack, Pack, PackCharacter, Sounds, Style};
use aitrack_stock::instrument::bloom::patch::Kind;
use aitrack_stock::instrument::bloom::{self, MAX_SEED};
use gpui::Context;

use crate::workspace::Workspace;

/// What [`grow_pack`] makes.
#[derive(Clone, Debug, PartialEq)]
pub struct PackOpts {
    /// The members, in track order (a kind listed twice is grown once).
    pub kinds: Vec<Kind>,
    /// Bars of every clip (clamped to 1..=16).
    pub bars: u32,
    /// `None`: from the project's tempo.
    pub genre: Option<Genre>,
    /// `None`: the seed chooses the pack's leaning.
    pub style: Option<Style>,
    /// `None`: a new pack each press.
    pub seed: Option<u64>,
    /// Where the clips start; `None`: the loop's first bar, else bar 1.
    pub start: Option<Tick>,
}

impl Default for PackOpts {
    fn default() -> Self {
        Self { kinds: bpack::DEFAULT_KINDS.to_vec(), bars: 4, genre: None, style: None, seed: None, start: None }
    }
}

/// The bar where the loop starts, else the first bar.
fn region_start(p: &Project) -> Tick {
    let bar = p.tempo.signature_at(0).bar_ticks().max(1);
    p.loop_region.map_or(0, |(s, _)| s.max(0) / bar * bar)
}

/// A track colour of the palette per kind.
fn colour(kind: Kind) -> Color {
    Color::palette(match kind {
        Kind::Drums => 1,
        Kind::Bass => 3,
        Kind::Lead => 11,
        Kind::Pad => 8,
        Kind::Pluck => 6,
        Kind::Keys => 5,
        Kind::Fx => 13,
        Kind::Texture => 9,
    })
}

/// The member's fader: a balanced starting mix, set from the audition's
/// measurements (`bloom_pack_audition`). A drum scene in Kit mode plays
/// every hit near full scale (about −7 dB RMS, peaks over 0 dBFS over a
/// groove) while a tonal Bloom levels each held note to its kind's target
/// (a part comes out near −21 to −28 dB RMS), so the drums go down 16 dB,
/// the bass stays at unity and the rest sit back: drums ≈ −23, bass ≈ −27,
/// lead / pad ≈ −28, pluck ≈ −30 dB RMS,
/// a mix near −20 dB RMS with its peaks under −3 dBFS.
pub fn fader_db(kind: Kind) -> f32 {
    match kind {
        Kind::Drums => -16.0,
        Kind::Bass => 0.0,
        Kind::Lead => -5.0,
        Kind::Pad => -7.0,
        Kind::Pluck => -6.0,
        Kind::Keys => -6.0,
        Kind::Fx => -4.0,
        Kind::Texture => -8.0,
    }
}

/// The seed of a press with no seed asked for.
fn fresh_seed() -> u64 {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos() as u64);
    aitrack_stock::instrument::bloom::rng::splitmix64(nanos) % (MAX_SEED + 1)
}

/// What [`pack_commands`] made.
pub struct Grown {
    pub commands: Vec<Command>,
    /// The new tracks, in order.
    pub tracks: Vec<TrackId>,
    pub pack: Pack,
}

/// The commands that add pack `seed` to `p`: per member a MIDI track
/// ("Bloom Bass — Velvet Growl") with a Bloom of that kind playing the
/// pack's sounds and a clip of its part, placed at `opts.start`. Ids are
/// allocated from `p`; nothing is applied.
pub fn pack_commands(p: &mut Project, opts: &PackOpts, seed: u64) -> Grown {
    let seed = seed.min(MAX_SEED);
    let bars = opts.bars.clamp(1, 16);
    let start = opts.start.unwrap_or_else(|| region_start(p)).max(0);
    let ctx = digest(p, None, start, bars);
    let scale = ctx.scale.unwrap_or(aitrack_model::Scale::new(aitrack_model::Key(9), aitrack_model::ScaleKind::Minor));
    let key = Some((scale.root.0 % 12, !matches!(scale.kind, aitrack_model::ScaleKind::Major | aitrack_model::ScaleKind::Lydian | aitrack_model::ScaleKind::Mixolydian | aitrack_model::ScaleKind::MajorPentatonic)));
    let character = PackCharacter::of(seed, opts.style, key);
    let pack = bpack::pack(seed, &character, &opts.kinds);
    let sounds: Vec<Kind> = pack.members.iter().map(|m| m.kind).collect();
    let drums = pack.members.iter().find_map(|m| match &m.sounds {
        Sounds::Drums(s) => Some(&**s),
        Sounds::Tonal(_) => None,
    });
    let req = PackRequest { bars, genre: opts.genre, key: Some(scale), seed };
    let parts = plan::plan(&sounds, drums, &ctx, &req);
    let stamp = p.scale.is_none();
    let first = p.tracks.iter().position(|t| matches!(t.kind, TrackKind::Return | TrackKind::Vca | TrackKind::Main)).unwrap_or(p.tracks.len());
    let (mut tracks_cmds, mut clip_cmds, mut tracks) = (Vec::new(), Vec::new(), Vec::new());
    for (index, (m, part)) in (first..).zip(pack.members.iter().zip(&parts.parts)) {
        let id = p.alloc_track_id();
        let color = colour(m.kind);
        let mut t = Track::new(id, TrackKind::Instrument, format!("Bloom {} — {}", m.kind.label(), m.name), color);
        t.volume_db = fader_db(m.kind);
        let (params, state) = bloom::member_device(&pack, m);
        let mut d = Device::stock(p.alloc_device_id(), bloom::ID, "Bloom");
        d.params = params;
        d.state = Some(state);
        t.devices.push(d);
        tracks_cmds.push(Command::AddTrack { track: Box::new(t), index });
        let notes: Vec<Note> = part
            .notes
            .iter()
            .filter(|n| n.start >= 0 && n.start < part.clip_ticks)
            .map(|n| {
                let mut note = Note::new(p.alloc_note_id(), n.key, n.start, n.length.min(part.clip_ticks - n.start).max(1), n.velocity);
                note.slide = n.slide;
                note
            })
            .collect();
        let (src, clip_id, placement) = (p.alloc_source_id(), p.alloc_clip_id(), p.alloc_placement_id());
        let mut clip = Clip::new(format!("{} · {}", m.kind.label(), m.name), color, src, part.clip_ticks);
        if stamp && m.kind.is_tonal() {
            clip.scale = Some(scale);
        }
        clip_cmds.push(Command::AddSource { id: src, source: Box::new(Source::Notes(NoteSource { notes, ..Default::default() })) });
        clip_cmds.push(Command::AddClip { id: clip_id, clip: Box::new(clip) });
        clip_cmds.push(Command::PlaceClip { track: id, placement: Placement { id: placement, clip: clip_id, start, lane: 0 } });
        tracks.push(id);
    }
    tracks_cmds.extend(clip_cmds);
    Grown { commands: tracks_cmds, tracks, pack }
}

/// Grow a Bloom pack into the project as one undo step ("Grow Bloom
/// pack"): the tracks, their Blooms and their clips; the first new track is
/// selected and a toast names the pack.
pub fn grow_pack(ws: &mut Workspace, opts: PackOpts, cx: &mut Context<Workspace>) {
    let seed = opts.seed.unwrap_or_else(fresh_seed);
    let grown = pack_commands(&mut ws.s.project, &opts, seed);
    let n = grown.tracks.len();
    if n == 0 {
        return;
    }
    ws.s.seal();
    if ws.s.edit("Grow Bloom pack", grown.commands) {
        ws.s.selected_track = grown.tracks.first().copied();
        let name = grown.pack.members.iter().find(|m| m.kind.is_tonal()).map_or_else(String::new, |m| m.name.clone());
        ws.toast(format!("Grew a Bloom pack: {n} tracks · {name} · seed {seed}"), cx);
    }
    ws.s.seal();
}

#[cfg(test)]
mod tests {
    use super::*;
    use aitrack_model::{DeviceKind, Key, Scale, ScaleKind};
    use aitrack_stock::instrument::bloom::p;

    fn fresh() -> crate::state::AppState {
        let mut project = Project::new("Pack test");
        project.scale = Some(Scale::new(Key(4), ScaleKind::Minor));
        crate::state::AppState::new(project)
    }

    #[test]
    fn a_pack_is_six_bloom_tracks_with_clips_in_one_undo_step() {
        let mut s = fresh();
        let tracks_before = s.project.tracks.len();
        let steps = s.history.steps().count();
        let opts = PackOpts { bars: 8, ..Default::default() };
        let grown = pack_commands(&mut s.project, &opts, 4_242);
        assert_eq!(grown.tracks.len(), 6);
        assert!(s.edit("Grow Bloom pack", grown.commands));
        assert_eq!(s.history.steps().count(), steps + 1, "one undo step");
        assert_eq!(s.project.tracks.len(), tracks_before + 6);
        let scale = s.project.scale.unwrap();
        let infos = bloom::params();
        let plain = |d: &Device, id: u32| infos.iter().find(|i| i.id == id).unwrap().to_plain(d.params[&id]);
        let bar = s.project.tempo.signature_at(0).bar_ticks();
        for (k, (&id, m)) in grown.tracks.iter().zip(&grown.pack.members).enumerate() {
            let t = s.project.track(id).unwrap();
            assert_eq!(t.kind, TrackKind::Instrument);
            assert_eq!(t.name, format!("Bloom {} — {}", m.kind.label(), m.name));
            assert_eq!(m.kind, bpack::DEFAULT_KINDS[k]);
            // One Bloom of that kind, holding the pack's sounds.
            assert_eq!(t.devices.len(), 1);
            let d = &t.devices[0];
            assert!(matches!(&d.kind, DeviceKind::Stock { plugin_id } if plugin_id == bloom::ID));
            assert_eq!(plain(d, p::KIND) as usize, m.kind.index());
            assert_eq!(bloom::seed_of(plain(d, p::SEED)), m.seed());
            match &m.sounds {
                Sounds::Tonal(scene) => {
                    assert_eq!(bloom::parse_patches(d.state.as_deref()).as_ref(), Some(&**scene));
                    assert_eq!(bloom::effective_patches(Some(scene), m.seed(), m.kind), **scene, "the device plays the pack's patches");
                    assert_eq!(plain(d, p::SELECTED) as usize, m.selected);
                }
                Sounds::Drums(scene) => {
                    assert_eq!(bloom::parse_state(d.state.as_deref()).0.as_ref(), Some(&**scene));
                    assert_eq!(plain(d, p::MODE), 0.0, "Kit mode");
                }
            }
            // One clip of 8 bars at bar 1.
            assert_eq!(t.arrangement.len(), 1);
            let pl = &t.arrangement[0];
            assert_eq!(pl.start, 0);
            let clip = &s.project.clips[&pl.clip];
            assert_eq!(clip.length, bar * 8);
            let Some(Source::Notes(src)) = s.project.sources.get(&clip.source) else { panic!("a notes clip") };
            assert!(!src.notes.is_empty(), "{:?} plays", m.kind);
            for n in &src.notes {
                assert!(n.start >= 0 && n.end() <= clip.length);
                // Drum notes hit Bloom's pads (Base 36 + cell).
                if let Sounds::Drums(scene) = &m.sounds {
                    assert!((36..52).contains(&n.key), "drum note {}", n.key);
                    assert!(aitrack_ai::generate::pack::bloom_pads(scene, 36).keys().contains(&n.key));
                }
            }
        }
        // The tonal notes are in key (or tones of the bar's chord, like a V's leading tone).
        let in_key = grown
            .tracks
            .iter()
            .zip(&grown.pack.members)
            .filter(|(_, m)| m.kind.is_tonal())
            .flat_map(|(id, _)| {
                let t = s.project.track(*id).unwrap();
                let clip = &s.project.clips[&t.arrangement[0].clip];
                let Some(Source::Notes(src)) = s.project.sources.get(&clip.source) else { panic!() };
                src.notes.iter().map(|n| scale.contains(n.key)).collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        let share = in_key.iter().filter(|b| **b).count() as f32 / in_key.len() as f32;
        assert!(share > 0.9, "{:.0} % of the notes in E minor", share * 100.0);
        // One undo removes it all.
        assert!(s.undo());
        assert_eq!(s.project.tracks.len(), tracks_before);
        assert!(grown.tracks.iter().all(|id| s.project.track(*id).is_none()));
        assert!(s.project.clips.is_empty() && s.project.sources.is_empty());
    }

    #[test]
    fn packs_are_repeatable_and_options_pick_the_members() {
        let mut a = fresh();
        let mut b = fresh();
        let opts = PackOpts { kinds: vec![Kind::Bass, Kind::Keys, Kind::Drums], bars: 4, genre: Some(Genre::Techno), style: Some(Style::Dark), seed: None, start: Some(0) };
        let ga = pack_commands(&mut a.project, &opts, 99);
        let gb = pack_commands(&mut b.project, &opts, 99);
        assert_eq!(ga.pack, gb.pack);
        assert_eq!(ga.commands.len(), gb.commands.len());
        assert!(a.edit("Grow Bloom pack", ga.commands));
        let names: Vec<String> = ga.tracks.iter().map(|id| a.project.track(*id).unwrap().name.clone()).collect();
        assert_eq!(names.len(), 3);
        assert!(names[0].starts_with("Bloom Bass — ") && names[1].starts_with("Bloom Keys — ") && names[2].starts_with("Bloom Drums — "), "{names:?}");
        assert!(ga.pack.character.bright < 0.0, "a dark pack");
        // Each track has its own colour from the palette.
        let colours: Vec<Color> = ga.tracks.iter().map(|id| a.project.track(*id).unwrap().color).collect();
        assert!(colours.iter().all(|c| Color::PALETTE.contains(c)));
        assert_ne!(colours[0], colours[1]);
    }

    /// Renders a pack through Bloom devices built from the commands (each
    /// member playing its clip), into per-member and mixed WAVs and PNG
    /// spectrograms, and prints levels:
    /// `BLOOM_PACK_OUT=<dir> cargo test -p aitrack-app --release --bin aitrack bloom_pack_audition -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn bloom_pack_audition() {
        use aitrack_dsp::{AudioBuffer, Event, EventKind, Processor, Transport};
        let out = std::path::PathBuf::from(std::env::var("BLOOM_PACK_OUT").unwrap_or_else(|_| "bloom-pack".into()));
        std::fs::create_dir_all(&out).unwrap();
        let seeds: Vec<u64> = std::env::var("BLOOM_PACK_SEEDS").ok().map(|s| s.split(',').filter_map(|x| x.trim().parse().ok()).collect()).unwrap_or_else(|| vec![4_242, 77]);
        for seed in seeds {
            let mut s = fresh();
            s.project.tempo = aitrack_model::TempoMap::constant(124.0);
            let grown = pack_commands(&mut s.project, &PackOpts { bars: 4, ..Default::default() }, seed);
            assert!(s.edit("Grow Bloom pack", grown.commands));
            let sr = 48_000.0f64;
            let bpm = 124.0;
            let ppq = aitrack_model::PPQ as f64;
            let tick_frames = |t: Tick| (t as f64 / ppq * 60.0 / bpm * sr) as usize;
            let mut mix: Vec<[f32; 2]> = Vec::new();
            println!("pack {seed}: {:?} {:?}", grown.pack.character.flavour, grown.pack.character.species);
            let mut members = Vec::new();
            for &id in &grown.tracks {
                let t = s.project.track(id).unwrap().clone();
                let clip = s.project.clips[&t.arrangement[0].clip].clone();
                let Some(Source::Notes(src)) = s.project.sources.get(&clip.source).cloned() else { panic!() };
                let mut b = bloom::Bloom::new();
                for (pid, v) in &t.devices[0].params {
                    b.set_param(*pid, *v);
                }
                b.load_state(t.devices[0].state.as_deref().unwrap());
                const BLOCK: usize = 256;
                b.prepare(sr, BLOCK);
                // Twice through the clip, so the loop's seam and the tails are heard.
                let len = tick_frames(clip.length);
                let total = len * 2 + (sr * 1.5) as usize;
                let mut evs: Vec<(usize, EventKind)> = Vec::new();
                for rep in 0..2 {
                    for n in &src.notes {
                        let on = rep * len + tick_frames(n.start);
                        let off = rep * len + tick_frames(n.end());
                        evs.push((on, EventKind::NoteOn { channel: 0, key: n.key, velocity: n.velocity, note_id: -1 }));
                        evs.push((off.max(on + 1), EventKind::NoteOff { channel: 0, key: n.key, velocity: 0.0, note_id: -1 }));
                    }
                }
                evs.sort_by_key(|e| (e.0, matches!(e.1, EventKind::NoteOn { .. })));
                let mut buf = AudioBuffer::new(2, BLOCK);
                let tr = Transport { playing: true, ..Default::default() };
                let mut rendered: Vec<[f32; 2]> = Vec::with_capacity(total);
                let fader = aitrack_dsp::db_to_gain(t.volume_db);
                let mut pos = 0;
                let mut e = 0;
                while pos < total {
                    let n = BLOCK.min(total - pos);
                    buf.set_frames(n);
                    buf.clear();
                    let mut events = Vec::new();
                    while e < evs.len() && evs[e].0 < pos + n {
                        events.push(Event { offset: (evs[e].0.saturating_sub(pos)) as u32, kind: evs[e].1 });
                        e += 1;
                    }
                    let mut ctx = aitrack_dsp::ProcessContext { sample_rate: sr, buffer: &mut buf, sidechain: None, events: &events, transport: &tr };
                    b.process(&mut ctx);
                    for i in 0..n {
                        rendered.push([buf.channel(0)[i] * fader, buf.channel(1)[i] * fader]);
                    }
                    pos += n;
                }
                let rms = (rendered.iter().map(|f| f[0] * f[0] + f[1] * f[1]).sum::<f32>() / (2 * rendered.len()) as f32).sqrt();
                let peak = rendered.iter().fold(0f32, |m, f| m.max(f[0].abs()).max(f[1].abs()));
                println!("  {:<40} rms {:>6.1} dB  peak {:>6.1} dB  notes {}", t.name, 20.0 * rms.max(1e-9).log10(), 20.0 * peak.max(1e-9).log10(), src.notes.len());
                if mix.len() < rendered.len() {
                    mix.resize(rendered.len(), [0.0; 2]);
                }
                for (m, r) in mix.iter_mut().zip(&rendered) {
                    m[0] += r[0];
                    m[1] += r[1];
                }
                let file = format!("{seed}-{}", t.name.split(" — ").next().unwrap_or("member").to_lowercase().replace(' ', "-"));
                members.push((file, rendered));
            }
            let peak = mix.iter().fold(0f32, |m, f| m.max(f[0].abs()).max(f[1].abs()));
            let rms = (mix.iter().map(|f| f[0] * f[0] + f[1] * f[1]).sum::<f32>() / (2 * mix.len()) as f32).sqrt();
            println!("  mix rms {:.1} dB peak {:.1} dB", 20.0 * rms.log10(), 20.0 * peak.log10());
            members.push((format!("{seed}-mix"), mix));
            for (name, x) in &members {
                write_wav(&out.join(format!("{name}.wav")), x, sr as u32);
                let mono: Vec<f32> = x.iter().map(|f| (f[0] + f[1]) * 0.5).collect();
                spectrogram(&mono, sr as f32, &out.join(format!("{name}.png")));
            }
        }
        println!("wrote {}", out.display());
    }

    fn write_wav(path: &std::path::Path, x: &[[f32; 2]], sr: u32) {
        let spec = hound::WavSpec { channels: 2, sample_rate: sr, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
        let mut w = hound::WavWriter::create(path, spec).unwrap();
        for f in x {
            w.write_sample((f[0].clamp(-1.0, 1.0) * 32_767.0) as i16).unwrap();
            w.write_sample((f[1].clamp(-1.0, 1.0) * 32_767.0) as i16).unwrap();
        }
        w.finalize().unwrap();
    }

    /// A log-frequency spectrogram (30 Hz–16 kHz, 90 dB), one Goertzel
    /// filter per row: slow but dependency-free.
    fn spectrogram(x: &[f32], sr: f32, path: &std::path::Path) {
        const H: u32 = 200;
        const N: usize = 4096;
        let width = 900u32;
        let hop = (x.len() / width as usize).max(1);
        let freqs: Vec<f32> = (0..H).map(|r| 30.0 * (16_000.0f32 / 30.0).powf(r as f32 / H as f32)).collect();
        let win: Vec<f32> = (0..N).map(|i| 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / N as f32).cos()).collect();
        let mut cols = Vec::with_capacity(width as usize);
        let mut gmax = 1e-12f32;
        for c in 0..width as usize {
            let start = c * hop;
            let frame: Vec<f32> = (0..N).map(|i| x.get(start + i).copied().unwrap_or(0.0) * win[i]).collect();
            let col: Vec<f32> = freqs
                .iter()
                .map(|f| {
                    let w = std::f32::consts::TAU * f / sr;
                    // Goertzel at f.
                    let coeff = 2.0 * w.cos();
                    let (mut s1, mut s2) = (0.0f32, 0.0f32);
                    for v in &frame {
                        let s0 = v + coeff * s1 - s2;
                        s2 = s1;
                        s1 = s0;
                    }
                    s1 * s1 + s2 * s2 - coeff * s1 * s2
                })
                .collect();
            gmax = col.iter().fold(gmax, |m, v| m.max(*v));
            cols.push(col);
        }
        let mut img = image::RgbImage::new(width, H);
        for (c, col) in cols.iter().enumerate() {
            for (r, e) in col.iter().enumerate() {
                let t = ((10.0 * (e / gmax).max(1e-12).log10() + 90.0) / 90.0).clamp(0.0, 1.0);
                let px = [(t * 255.0) as u8, (t * t * 200.0) as u8, ((1.0 - t) * t * 4.0 * 160.0) as u8];
                img.put_pixel(c as u32, H - 1 - r as u32, image::Rgb(px));
            }
        }
        img.save(path).unwrap();
    }
}
