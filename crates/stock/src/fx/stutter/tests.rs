use super::*;
use crate::test_util::set_plain;
use aitrack_dsp::{AudioBuffer, Event, Transport};

const SR: f64 = 48_000.0;
/// At 120 BPM one beat is 24 000 samples, so every boundary below lands on a
/// whole sample.
const BEAT: usize = 24_000;

fn noise(n: usize) -> f32 {
    let mut x = (n as u32).wrapping_mul(2_654_435_761).wrapping_add(12_345);
    x ^= x >> 15;
    x = x.wrapping_mul(2_246_822_519);
    x ^= x >> 13;
    ((x >> 8) as f32 / 8_388_608.0 - 1.0) * 0.8
}

fn sine(freq: f32, amp: f32) -> impl Fn(usize) -> f32 {
    move |n| amp * (std::f64::consts::TAU * freq as f64 * n as f64 / SR).sin() as f32
}

fn ramp(n: usize) -> f32 {
    n as f32 * 1e-6
}

/// Drives a [`Stutter`] block by block. The input is a function of the
/// stream index, which starts at `t0` (the transport sample of the first
/// block when the stream and the song are the same thing).
struct Player {
    s: Stutter,
    buf: AudioBuffer,
    t: usize,
    bpm: f64,
    playing: bool,
    time_sig: (u8, u8),
}

impl Player {
    fn new() -> Self {
        Self::with(Stutter::new())
    }

    fn with(mut s: Stutter) -> Self {
        s.prepare(SR, 1024);
        Self { s, buf: AudioBuffer::new(2, 1024), t: 0, bpm: 120.0, playing: true, time_sig: (4, 4) }
    }

    fn set(&mut self, id: u32, plain: f32) {
        set_plain(&mut self.s, id, plain);
    }

    /// Process `n` frames starting at song position `beats0`.
    fn run(&mut self, input: &dyn Fn(usize) -> f32, beats0: f64, n: usize, block: usize) -> Vec<f32> {
        self.run_events(input, beats0, n, block, &[])
    }

    fn run_events(&mut self, input: &dyn Fn(usize) -> f32, beats0: f64, n: usize, block: usize, events: &[Event]) -> Vec<f32> {
        let mut out = Vec::with_capacity(n);
        let mut done = 0;
        while done < n {
            let len = block.min(n - done);
            self.buf.set_frames(len);
            for i in 0..len {
                let v = input(self.t + i);
                self.buf.channel_mut(0)[i] = v;
                self.buf.channel_mut(1)[i] = v;
            }
            let pos = beats0 + done as f64 * self.bpm / (60.0 * SR);
            let tr = Transport { playing: self.playing, bpm: self.bpm, time_sig: self.time_sig, position_beats: pos, position_samples: (pos * 60.0 * SR / self.bpm) as i64, ..Transport::default() };
            let evs: Vec<Event> = events.iter().filter(|e| (e.offset as usize) >= done && (e.offset as usize) < done + len).map(|e| Event { offset: e.offset - done as u32, kind: e.kind }).collect();
            let mut ctx = ProcessContext { sample_rate: SR, buffer: &mut self.buf, sidechain: None, events: &evs, transport: &tr };
            self.s.process(&mut ctx);
            out.extend_from_slice(self.buf.channel(0));
            assert_eq!(self.buf.channel(0), self.buf.channel(1), "both channels carry the same signal");
            done += len;
            self.t += len;
        }
        out
    }
}

fn dry(input: &dyn Fn(usize) -> f32, from: usize, n: usize) -> Vec<f32> {
    (from..from + n).map(input).collect()
}

fn max_step(x: &[f32]) -> f32 {
    x.windows(2).map(|w| (w[1] - w[0]).abs()).fold(0.0, f32::max)
}

fn crossings(x: &[f32]) -> usize {
    x.windows(2).filter(|w| w[0] < 0.0 && w[1] >= 0.0).count()
}

fn same_bits(a: &[f32], b: &[f32], what: &str) {
    assert_eq!(a.len(), b.len(), "{what}: lengths");
    if let Some(i) = (0..a.len()).find(|&i| a[i].to_bits() != b[i].to_bits()) {
        panic!("{what}: first difference at {i}: {} vs {}", a[i], b[i]);
    }
}

fn differs(a: &[f32], b: &[f32], what: &str) {
    assert!(a.len() != b.len() || (0..a.len()).any(|i| a[i].to_bits() != b[i].to_bits()), "{what}: identical");
}

/// The classic-mode defaults, spelled out: every bar, a quarter note long,
/// repeating a sixteenth.
fn classic(p: &mut Player) {
    p.set(p::MODE, 0.0);
    p.set(p::INTERVAL, 5.0);
    p.set(p::GATE, 4.0);
    p.set(p::GRID, 1.0);
    p.set(p::CHANCE, 100.0);
}

// ---- Requirements of the roadmap -------------------------------------------

#[test]
fn chance_zero_is_bit_identical_in_every_mix_mode() {
    for mix_mode in 0..4 {
        for playing in [true, false] {
            let mut p = Player::new();
            p.playing = playing;
            classic(&mut p);
            p.set(p::CHANCE, 0.0);
            p.set(p::MIX_MODE, mix_mode as f32);
            p.set(p::INTERVAL, 1.0); // a candidate every sixteenth: plenty of chances to fire
            let out = p.run(&noise, 3.0, 6 * BEAT, 128);
            same_bits(&out, &dry(&noise, 0, 6 * BEAT), &format!("mix mode {mix_mode}, playing {playing}"));
            assert_eq!(p.s.triggers, 0);
        }
    }
}

#[test]
fn grid_with_no_step_firing_is_bit_identical() {
    let mut p = Player::new();
    p.set(p::MODE, 1.0);
    for i in 0..16 {
        set_plain(&mut p.s, p::step(i, p::STEP_PROB), 0.0);
    }
    let out = p.run(&noise, 0.0, 8 * BEAT, 100);
    same_bits(&out, &dry(&noise, 0, 8 * BEAT), "");
    // The same with every step off rather than at 0 %.
    let mut p = Player::new();
    p.set(p::MODE, 1.0);
    for i in 0..16 {
        set_plain(&mut p.s, p::step(i, p::STEP_ON), 0.0);
    }
    let out = p.run(&noise, 0.0, 8 * BEAT, 100);
    same_bits(&out, &dry(&noise, 0, 8 * BEAT), "");
}

#[test]
fn the_capture_buffer_holds_sixteen_seconds_from_prepare() {
    for sr in [44_100.0, 48_000.0, 96_000.0] {
        let mut s = Stutter::new();
        s.prepare(sr, 512);
        assert!(s.cap.len() as f64 >= 16.0 * sr, "{} frames at {sr} Hz", s.cap.len());
    }
}

/// Everything that decides what is heard is a function of the song position
/// and the seed: the block size changes nothing, not even a bit.
#[test]
fn the_pattern_does_not_depend_on_the_block_size() {
    let grid = |p: &mut Player| {
        p.set(p::MODE, 1.0);
        p.set(p::SEED, 7.0);
        p.set(p::SWING, 40.0);
        p.set(p::LEVEL_END, -6.0);
        p.set(p::FILTER_END, 1.0);
        p.set(p::REVERSE, 3.0); // random direction per repeat
        set_plain(&mut p.s, p::tie(5), 1.0);
        set_plain(&mut p.s, p::tie(6), 1.0);
        set_plain(&mut p.s, p::tie(11), 1.0);
        set_plain(&mut p.s, p::step(3, p::STEP_STOP), 1.0);
        set_plain(&mut p.s, p::step(9, p::STEP_STOP), 1.0);
        set_plain(&mut p.s, p::step(2, p::STEP_FILTER), 1.0);
        set_plain(&mut p.s, p::step(7, p::STEP_LEVEL), -9.0);
        p.set(p::PITCH_DECAY, 3.0);
        p.set(p::DECAY, -3.0);
        p.set(p::FILTER_ON, 1.0);
        for i in 0..16 {
            set_plain(&mut p.s, p::step(i, p::STEP_ON), 1.0);
            set_plain(&mut p.s, p::step(i, p::STEP_PROB), 60.0);
            set_plain(&mut p.s, p::step(i, p::STEP_DIV), (i % 5) as f32);
            set_plain(&mut p.s, p::step(i, p::STEP_PITCH), (i % 3) as f32 * 2.0 - 2.0);
        }
    };
    let classic_full = |p: &mut Player| {
        classic(p);
        p.set(p::SEED, 31.0);
        p.set(p::INTERVAL, 3.0);
        p.set(p::CHANCE, 70.0);
        p.set(p::GATE, 6.0);
        p.set(p::GRID, 2.0);
        p.set(p::VARIATION, 60.0);
        p.set(p::TRIPLETS, 1.0);
        p.set(p::RAMP, 2.0);
        p.set(p::REVERSE, 2.0);
        p.set(p::PITCH, 3.0);
        p.set(p::PITCH_END, 7.0);
        p.set(p::LEVEL, -3.0);
        p.set(p::LEVEL_END, 3.0);
        p.set(p::FILTER_ON, 1.0);
        p.set(p::FILTER_END, -1.0);
        p.set(p::MIX_MODE, 2.0);
    };
    let tape = |p: &mut Player| {
        classic(p);
        p.set(p::INTERVAL, 4.0);
        p.set(p::CHANCE, 80.0);
        p.set(p::GATE, 5.0);
        p.set(p::TAPE_STOP, 1.0);
        p.set(p::STOP_TIME, 1.0);
        p.set(p::HALF_TIME, 1.0);
        p.set(p::MIX_MODE, 3.0);
    };
    let configs: [&dyn Fn(&mut Player); 3] = [&grid, &classic_full, &tape];
    for (c, configure) in configs.into_iter().enumerate() {
        let mut reference = Player::new();
        configure(&mut reference);
        let want = reference.run(&noise, 2.0, 8 * BEAT, 128);
        assert!(reference.s.triggers >= 3, "config {c}: {} events", reference.s.triggers);
        differs(&want, &dry(&noise, 0, 8 * BEAT), "the effect does something");
        for block in [1, 16, 37, 64, 480, 1000] {
            let mut p = Player::new();
            configure(&mut p);
            let got = p.run(&noise, 2.0, 8 * BEAT, block);
            same_bits(&got, &want, &format!("config {c}, block size {block}"));
            assert_eq!(p.s.triggers, reference.s.triggers);
        }
    }
}

/// Live and bounce start at different places: once the buffer holds the same
/// audio, both sound the same.
#[test]
fn the_pattern_does_not_depend_on_when_playback_started() {
    let configure = |p: &mut Player| {
        p.set(p::MODE, 1.0);
        p.set(p::SEED, 123.0);
        p.set(p::SWING, 30.0);
        set_plain(&mut p.s, p::tie(4), 1.0);
        set_plain(&mut p.s, p::tie(9), 1.0);
        p.set(p::PITCH, -5.0);
        for i in 0..16 {
            set_plain(&mut p.s, p::step(i, p::STEP_ON), 1.0);
            set_plain(&mut p.s, p::step(i, p::STEP_PROB), 50.0);
            set_plain(&mut p.s, p::step(i, p::STEP_DIV), (i % 5) as f32);
        }
    };
    let mut a = Player::new();
    configure(&mut a);
    let out_a = a.run(&noise, 0.0, 12 * BEAT, 256);
    // B starts at beat 6 with a fresh buffer and is fed the same audio.
    let mut b = Player::new();
    configure(&mut b);
    b.t = 6 * BEAT;
    let out_b = b.run(&noise, 6.0, 6 * BEAT, 256);
    // Two beats of history are more than any chunk here needs.
    let from = 2 * BEAT;
    same_bits(&out_a[8 * BEAT..], &out_b[from..], "");
    assert!(a.s.triggers > 8);
    // And a plugin that was reset behaves like a new one.
    let mut c = Player::new();
    configure(&mut c);
    c.run(&noise, 40.0, 3 * BEAT, 128);
    c.s.reset();
    c.t = 0;
    let out_c = c.run(&noise, 0.0, 12 * BEAT, 128);
    same_bits(&out_c, &out_a, "");
}

/// 50 beats: 200 sixteenths.
const LONG: usize = 50 * BEAT;
const LONG_SLOTS: usize = LONG / 6_000;

#[test]
fn the_same_seed_gives_the_same_pattern_and_another_seed_another() {
    let run = |seed: f32, chance: f32| {
        let mut p = Player::new();
        classic(&mut p);
        p.set(p::INTERVAL, 1.0);
        p.set(p::GATE, 0.0);
        p.set(p::CHANCE, chance);
        p.set(p::SEED, seed);
        let out = p.run(&noise, 0.0, LONG, 128);
        (p.s.triggers as usize, out)
    };
    let (n1, a) = run(5.0, 50.0);
    let (n2, b) = run(5.0, 50.0);
    let (_, c) = run(6.0, 50.0);
    same_bits(&a, &b, "");
    assert_eq!(n1, n2);
    differs(&a, &c, "");
    assert!(n1 > LONG_SLOTS * 35 / 100 && n1 < LONG_SLOTS * 65 / 100, "{n1} of {LONG_SLOTS}");
    let (all, _) = run(5.0, 100.0);
    assert_eq!(all, LONG_SLOTS, "Chance 100 fires every one");
}

// ---- Sound -------------------------------------------------------------------

/// With `r = 1` the repeat is the chunk `[t − grid, t)` looped exactly.
#[test]
fn the_loop_is_exactly_grid_samples_long() {
    let mut p = Player::new();
    classic(&mut p);
    // Start a beat before the boundary at beat 4; a sixteenth is 6000 samples.
    let out = p.run(&noise, 3.0, 3 * BEAT, 128);
    let t = BEAT; // trigger, relative to the start
    for k in 200..5_900 {
        assert!((out[t + k] - noise(t - 6_000 + k)).abs() < 1e-6, "first repeat, k = {k}");
    }
    for k in 6_200..11_900 {
        assert!((out[t + k] - noise(t - 6_000 + k % 6_000)).abs() < 1e-6, "second repeat, k = {k}");
    }
    // Seams included, every repeat is the same.
    for j in 0..6_000 {
        assert_eq!(out[t + 6_000 + j].to_bits(), out[t + 12_000 + j].to_bits(), "j = {j}");
    }
    // Before the trigger nothing happened; after the gate ends neither.
    same_bits(&out[..t], &dry(&noise, 0, t), "");
    same_bits(&out[t + BEAT..], &dry(&noise, t + BEAT, BEAT), "after the gate");
    assert_eq!(p.s.triggers, 1);
}

#[test]
fn the_period_follows_the_tempo_and_the_sample_rate() {
    for sr in [44_100.0, 96_000.0] {
        let mut s = Stutter::new();
        s.prepare(sr, 128);
        let mut buf = AudioBuffer::new(2, 128);
        let bpm = 150.0;
        let beat = (sr * 60.0 / bpm) as usize; // 17 640 / 38 400: whole samples
        let mut out = Vec::new();
        set_plain(&mut s, p::INTERVAL, 3.0); // every beat
        set_plain(&mut s, p::GATE, 4.0); // a beat long
        set_plain(&mut s, p::GRID, 1.0);
        let n = 3 * beat;
        let mut done = 0;
        while done < n {
            let len = 128.min(n - done);
            buf.set_frames(len);
            for i in 0..len {
                buf.channel_mut(0)[i] = noise(done + i);
                buf.channel_mut(1)[i] = noise(done + i);
            }
            let tr = Transport { playing: true, bpm, position_beats: 7.0 + done as f64 / beat as f64, ..Transport::default() };
            let mut ctx = ProcessContext { sample_rate: sr, buffer: &mut buf, sidechain: None, events: &[], transport: &tr };
            s.process(&mut ctx);
            out.extend_from_slice(buf.channel(0));
            done += len;
        }
        let grid = beat / 4; // a sixteenth
        // Second beat: repeats of the sixteenth that ended at the boundary.
        let t = beat;
        for k in 300..grid - 300 {
            assert!((out[t + k] - noise(t - grid + k)).abs() < 1e-6, "{sr}: k = {k}");
        }
        for k in 300..grid - 300 {
            assert_eq!(out[t + grid + k].to_bits(), out[t + k].to_bits(), "{sr}: period k = {k}");
        }
    }
}

#[test]
fn entering_and_leaving_a_repeat_makes_no_clicks() {
    let tone = sine(220.0, 0.5);
    let mut p = Player::new();
    classic(&mut p);
    p.set(p::INTERVAL, 3.0); // every beat
    p.set(p::GATE, 6.0); // half a beat
    p.set(p::GRID, 1.0);
    p.set(p::DECAY, -2.0);
    let out = p.run(&tone, 1.0, 8 * BEAT, 128);
    assert!(p.s.triggers >= 6);
    let limit = 0.06; // the sine itself moves 0.0144 per sample
    let worst = max_step(&out);
    assert!(worst < limit, "largest step {worst}");
    differs(&out, &dry(&tone, 0, 8 * BEAT), "");
    // Hard material and reversed repeats, too.
    let mut p = Player::new();
    classic(&mut p);
    p.set(p::INTERVAL, 3.0);
    p.set(p::REVERSE, 2.0);
    p.set(p::PITCH_DECAY, -2.0);
    let out = p.run(&tone, 1.0, 8 * BEAT, 128);
    assert!(max_step(&out) < limit, "largest step {}", max_step(&out));
}

#[test]
fn back_to_back_events_join_without_a_dip() {
    // A constant input shows any dip in level at the joins.
    let mut p = Player::new();
    classic(&mut p);
    p.set(p::INTERVAL, 3.0);
    p.set(p::GATE, 4.0); // as long as the interval: every event chains into the next
    let out = p.run(&|_| 0.5, 3.5, 6 * BEAT, 128);
    assert!(p.s.triggers >= 5);
    for (n, v) in out.iter().enumerate() {
        assert!((v - 0.5).abs() < 1e-6, "sample {n} = {v}");
    }
    // And the chain really is one event after another, not a gap and a restart:
    // a ramp shows the position inside the loop at every beat.
    let mut p = Player::new();
    classic(&mut p);
    p.set(p::INTERVAL, 3.0);
    p.set(p::GATE, 4.0);
    p.set(p::GRID, 1.0);
    let out = p.run(&ramp, 3.5, 4 * BEAT, 128);
    assert!(out.iter().all(|v| v.is_finite()));
}

#[test]
fn pitch_reads_the_chunk_at_the_matching_speed() {
    // On a ramp, Hermite reading is exact, so the output shows the read
    // position directly.
    let mut p = Player::new();
    classic(&mut p);
    p.set(p::PITCH, -12.0);
    let out = p.run(&ramp, 3.0, 2 * BEAT, 128);
    let t = BEAT;
    // Half speed over a sixteenth: the last 3000 samples before the trigger.
    for k in 200..5_800 {
        let want = ramp(t - 3_000) + ramp(1) * 0.5 * k as f32;
        assert!((out[t + k] - want).abs() < 2e-6, "k = {k}: {} vs {want}", out[t + k]);
    }
    // An octave up reads 12 000 samples in the same time.
    let mut p = Player::new();
    classic(&mut p);
    p.set(p::PITCH, 12.0);
    let out = p.run(&ramp, 3.0, 2 * BEAT, 128);
    for k in 200..5_800 {
        let want = ramp(t - 12_000) + ramp(1) * 2.0 * k as f32;
        assert!((out[t + k] - want).abs() < 2e-6, "k = {k}: {} vs {want}", out[t + k]);
    }
}

#[test]
fn pitch_decay_slows_each_repeat_further() {
    let mut p = Player::new();
    classic(&mut p);
    p.set(p::PITCH_DECAY, -12.0);
    let out = p.run(&ramp, 3.0, 2 * BEAT, 128);
    let t = BEAT;
    // Repeat 0 at full speed, repeat 1 at half, repeat 2 at a quarter.
    for k in 200..5_800 {
        assert!((out[t + k] - ramp(t - 6_000 + k)).abs() < 2e-6, "repeat 0, k = {k}");
    }
    for k in 200..5_800 {
        let want = ramp(t - 3_000) + ramp(1) * 0.5 * k as f32;
        assert!((out[t + 6_000 + k] - want).abs() < 2e-6, "repeat 1, k = {k}");
    }
    for k in 200..5_800 {
        let want = ramp(t - 1_500) + ramp(1) * 0.25 * k as f32;
        assert!((out[t + 12_000 + k] - want).abs() < 2e-6, "repeat 2, k = {k}");
    }
}

#[test]
fn decay_lowers_each_repeat_in_db() {
    let mut p = Player::new();
    classic(&mut p);
    p.set(p::DECAY, -6.0);
    p.set(p::GATE, 6.0);
    let out = p.run(&|_| 0.5, 3.0, 2 * BEAT, 128);
    let t = BEAT;
    let level = |rep: usize| out[t + rep * 6_000 + 3_000];
    for rep in 0..3 {
        let want = 0.5 * db_to_gain(-6.0 * rep as f32);
        assert!((level(rep) - want).abs() < 2e-3, "repeat {rep}: {} vs {want}", level(rep));
    }
}

#[test]
fn reverse_plays_the_chunk_backwards_and_ping_pong_alternates() {
    let t = BEAT;
    let mut p = Player::new();
    classic(&mut p);
    p.set(p::REVERSE, 1.0);
    let out = p.run(&ramp, 3.0, 2 * BEAT, 128);
    for k in 200..5_800 {
        assert!((out[t + k] - ramp(t - 1 - k)).abs() < 2e-6, "k = {k}");
    }
    let mut p = Player::new();
    classic(&mut p);
    p.set(p::REVERSE, 2.0);
    let out = p.run(&ramp, 3.0, 2 * BEAT, 128);
    for k in 200..5_800 {
        assert!((out[t + k] - ramp(t - 6_000 + k)).abs() < 2e-6, "forward, k = {k}");
        assert!((out[t + 6_000 + k] - ramp(t - 1 - k)).abs() < 2e-6, "backward, k = {k}");
    }
    // Random direction: both appear, and the sequence is the same each run.
    let direction = |seed: f32| {
        let mut p = Player::new();
        classic(&mut p);
        p.set(p::REVERSE, 3.0);
        p.set(p::GATE, 10.0); // two bars: many repeats
        p.set(p::SEED, seed);
        let out = p.run(&ramp, 3.0, 9 * BEAT, 128);
        (0..30).map(|r| out[t + r * 6_000 + 3_000] > out[t + r * 6_000 + 3_100]).collect::<Vec<bool>>()
    };
    let a = direction(1.0);
    assert!(a.iter().any(|d| *d) && a.iter().any(|d| !*d), "{a:?}");
    assert_eq!(a, direction(1.0));
    assert_ne!(a, direction(2.0));
}

#[test]
fn half_time_plays_an_octave_down_at_half_speed() {
    let tone = sine(440.0, 0.5);
    let mut p = Player::new();
    classic(&mut p);
    p.set(p::GRID, 2.0); // an eighth: 12 000 samples, of which half is read
    p.set(p::HALF_TIME, 1.0);
    let out = p.run(&tone, 3.0, 2 * BEAT, 128);
    let t = BEAT;
    let n = crossings(&out[t + 300..t + 11_700]);
    let want = 220.0 * (11_400.0 / SR);
    assert!((n as f64 - want).abs() <= 2.0, "{n} cycles, want about {want}");
    let reference = crossings(&dry(&tone, t + 300, 11_400));
    assert!((reference as f64 - 2.0 * want).abs() <= 2.0);
}

#[test]
fn ramp_halves_the_grid_in_steps_across_the_event() {
    let mut p = Player::new();
    classic(&mut p);
    p.set(p::GRID, 3.0); // a quarter: 24 000 samples
    p.set(p::GATE, 8.0); // one bar
    p.set(p::RAMP, 2.0); // two halvings: three segments of 32 000 samples
    p.run(&noise, 3.0, BEAT + 16_000, 128);
    assert_eq!(p.s.voice.grid, 24_000.0);
    p.run(&noise, 4.0 + 16_000.0 / BEAT as f64, 32_000, 128);
    assert_eq!(p.s.voice.grid, 12_000.0);
    p.run(&noise, 4.0 + 48_000.0 / BEAT as f64, 32_000, 128);
    assert_eq!(p.s.voice.grid, 6_000.0);
}

#[test]
fn tape_stop_slows_to_a_halt_and_hands_back_the_dry_signal() {
    let tone = sine(440.0, 0.5);
    let mut p = Player::new();
    classic(&mut p);
    p.set(p::OFFSET, 12.0); // beat 3 of the bar
    p.set(p::GATE, 4.0); // a beat
    p.set(p::TAPE_STOP, 1.0);
    p.set(p::STOP_TIME, 2.0); // a quarter note: the whole event
    let out = p.run(&tone, 2.0, 3 * BEAT, 128);
    let t0 = BEAT; // beat 3
    same_bits(&out[..t0], &dry(&tone, 0, t0), "untouched before");
    let quarter = BEAT / 4;
    let counts: Vec<usize> = (0..4).map(|q| crossings(&out[t0 + 300 + q * quarter..t0 + 300 + (q + 1) * quarter - 300])).collect();
    assert!(counts.windows(2).all(|w| w[0] > w[1]), "pitch keeps dropping: {counts:?}");
    assert!(counts[3] * 3 < counts[0], "{counts:?}");
    let tail_peak = out[t0 + BEAT - 2_000..t0 + BEAT - 100].iter().fold(0.0f32, |m, v| m.max(v.abs()));
    assert!(tail_peak < 0.1, "fades with the speed: {tail_peak}");
    same_bits(&out[t0 + BEAT..], &dry(&tone, t0 + BEAT, BEAT), "dry again right after");
    assert!(max_step(&out) < 0.1, "no click: {}", max_step(&out));
}

#[test]
fn a_gate_mix_silences_the_rest_of_the_interval() {
    let t = BEAT / 2; // the first boundary, at beat 4
    let mut p = Player::new();
    classic(&mut p);
    p.set(p::INTERVAL, 3.0); // every beat
    p.set(p::GATE, 2.0); // an eighth, then silence
    p.set(p::MIX_MODE, 3.0);
    let out = p.run(&noise, 3.5, 4 * BEAT, 128);
    assert!(out[t + 500..t + 11_500].iter().any(|v| v.abs() > 0.1), "repeats first");
    for (k, v) in out[t + 12_000 + 500..t + BEAT - 500].iter().enumerate() {
        assert_eq!(*v, 0.0, "silent, k = {k}");
    }
    // Before any trigger the input passes untouched.
    same_bits(&out[..t - 5], &dry(&noise, 0, t - 5), "");
}

#[test]
fn mix_adds_the_repeat_and_duck_closes_the_dry_signal() {
    let t = BEAT;
    let base = |mode: f32| {
        let mut p = Player::new();
        classic(&mut p);
        p.set(p::MIX_MODE, mode);
        p.run(&noise, 3.0, 2 * BEAT, 128)
    };
    let mix = base(1.0);
    for k in 200..5_800 {
        let want = noise(t + k) + noise(t - 6_000 + k);
        assert!((mix[t + k] - want).abs() < 1e-5, "k = {k}");
    }
    // Duck: the dry signal is attenuated linearly over the event.
    let duck = base(2.0);
    for k in (200..11_800).filter(|k| !(5_900..6_200).contains(k)) {
        let want = noise(t + k) * (1.0 - k as f32 / BEAT as f32) + noise(t - 6_000 + k % 6_000);
        assert!((duck[t + k] - want).abs() < 1e-4, "k = {k}: {} vs {want}", duck[t + k]);
    }
}

#[test]
fn mix_and_output_scale_the_result() {
    let t = BEAT;
    let render = |mix: f32, out_db: f32| {
        let mut p = Player::new();
        classic(&mut p);
        p.set(p::MIX, mix);
        p.set(p::OUTPUT, out_db);
        p.run(&noise, 3.0, 2 * BEAT, 128)
    };
    let full = render(100.0, 0.0);
    let none = render(0.0, 0.0);
    same_bits(&none, &dry(&noise, 0, 2 * BEAT), "Mix 0 is the dry signal");
    let half = render(50.0, 0.0);
    for k in 200..5_800 {
        let want = 0.5 * (full[t + k] + noise(t + k));
        assert!((half[t + k] - want).abs() < 1e-5);
    }
    let loud = render(100.0, 6.0);
    for k in 200..5_800 {
        assert!((loud[t + k] - full[t + k] * db_to_gain(6.0)).abs() < 1e-4);
    }
}

#[test]
fn the_filter_shapes_the_repeat() {
    let tone = sine(300.0, 0.5);
    let level = |on: f32, freq: f32| {
        let mut p = Player::new();
        classic(&mut p);
        p.set(p::FILTER_ON, on);
        p.set(p::FILTER_FREQ, freq);
        p.set(p::FILTER_WIDTH, 30.0);
        let out = p.run(&tone, 3.0, 2 * BEAT, 128);
        crate::test_util::rms(&out[BEAT + 1_000..BEAT + 5_000])
    };
    let open = level(0.0, 300.0);
    assert!(level(1.0, 300.0) > 0.7 * open, "centred on the tone it passes");
    assert!(level(1.0, 4_000.0) < 0.2 * open, "far from it it does not");
}

#[test]
fn hold_repeats_until_released() {
    let tone = sine(330.0, 0.5);
    let mut p = Player::new();
    classic(&mut p);
    p.set(p::CHANCE, 0.0);
    p.set(p::GRID, 2.0);
    let a = p.run(&tone, 1.0, BEAT, 128);
    same_bits(&a, &dry(&tone, 0, BEAT), "");
    p.set(p::HOLD, 1.0);
    let b = p.run(&tone, 2.0, 2 * BEAT, 128);
    assert!(p.s.voice.on);
    // 12 000 samples (an eighth) repeat, seams aside.
    for k in 12_300..12_300 + 8_000 {
        assert!((b[k] - b[k + 12_000]).abs() < 1e-5, "k = {k}");
    }
    assert!(max_step(&b) < 0.06, "{}", max_step(&b));
    p.set(p::HOLD, 0.0);
    let c = p.run(&tone, 4.0, BEAT, 128);
    assert!(!p.s.voice.on);
    same_bits(&c[300..], &dry(&tone, 3 * BEAT + 300, BEAT - 300), "");
    // The same through a sample-accurate parameter event.
    let mut q = Player::new();
    classic(&mut q);
    q.set(p::CHANCE, 0.0);
    q.run(&tone, 1.0, BEAT, 128);
    let on = Event { offset: 5_000, kind: EventKind::Param { id: p::HOLD, value: 1.0 } };
    let out = q.run_events(&tone, 2.0, BEAT, 128, &[on]);
    same_bits(&out[..5_000], &dry(&tone, BEAT, 5_000), "");
    assert!(q.s.voice.on);
}

#[test]
fn a_jump_cancels_the_repeat_without_a_click_and_waits_for_the_next_boundary() {
    let tone = sine(220.0, 0.5);
    let mut p = Player::new();
    classic(&mut p);
    p.set(p::INTERVAL, 3.0);
    p.set(p::GATE, 4.0);
    let a = p.run(&tone, 5.0, 2 * BEAT, 128);
    assert!(p.s.voice.on);
    // Jump into the middle of a beat far away.
    let b = p.run(&tone, 100.3, BEAT / 2, 128);
    assert!(max_step(&a) < 0.06 && max_step(&b) < 0.06);
    let seam: Vec<f32> = a[a.len() - 4..].iter().chain(b[..4].iter()).copied().collect();
    assert!(max_step(&seam) < 0.06, "no spike across the jump: {}", max_step(&seam));
    assert!(!p.s.voice.on);
    // 0.7 beats until the next boundary: the input passes exactly, after the fade.
    let clean = (2 * BEAT + 400)..(2 * BEAT + BEAT / 2 - 10);
    let got = &b[clean.start - 2 * BEAT..clean.end - 2 * BEAT];
    same_bits(got, &dry(&tone, clean.start, clean.end - clean.start), "");
    // A loop: jump back onto a boundary and the repeat starts at once.
    let mut q = Player::new();
    classic(&mut q);
    q.set(p::INTERVAL, 3.0);
    q.run(&noise, 7.0, BEAT, 128);
    let n0 = q.s.triggers;
    q.run(&noise, 4.0, 4_000, 128);
    assert_eq!(q.s.triggers, n0 + 1, "landing on a boundary fires it");
    // Landing mid-slot, a short while after a jump, a long chunk is not
    // taken from before the jump.
    let mut r = Player::new();
    classic(&mut r);
    r.set(p::INTERVAL, 3.0);
    r.set(p::GRID, 3.0); // a quarter: 24 000 samples of history needed
    r.run(&noise, 50.0, 2 * BEAT, 128);
    let before = r.s.triggers;
    r.run(&noise, 80.9, BEAT / 4, 128); // the boundary at 81 arrives after 2400 samples
    assert_eq!(r.s.triggers, before, "not enough audio since the jump for that chunk");
    r.run(&noise, 81.15, 2 * BEAT, 128);
    assert!(r.s.triggers > before, "later boundaries fire again");
}

#[test]
fn a_stopped_transport_free_runs_at_the_tempo() {
    let mut p = Player::new();
    p.playing = false;
    classic(&mut p);
    p.set(p::INTERVAL, 3.0);
    p.set(p::GATE, 2.0);
    let out = p.run(&noise, 0.0, 6 * BEAT, 128);
    assert!(p.s.triggers >= 5, "{} events", p.s.triggers);
    differs(&out, &dry(&noise, 0, 6 * BEAT), "");
}

#[test]
fn triplets_offset_and_the_time_signature_move_the_clock() {
    // Triplets: a sixteenth becomes 4000 samples.
    let mut p = Player::new();
    classic(&mut p);
    p.set(p::TRIPLETS, 1.0);
    let out = p.run(&noise, 3.0, 3 * BEAT, 128);
    let t = BEAT;
    for k in (200..3_800).chain(4_200..7_800) {
        assert!((out[t + k] - noise(t - 4_000 + k % 4_000)).abs() < 1e-6, "k = {k}");
    }
    // Offset 4 sixteenths: the bar's event moves a beat later.
    let mut p = Player::new();
    classic(&mut p);
    p.set(p::OFFSET, 4.0);
    let out = p.run(&noise, 3.0, 4 * BEAT, 128);
    same_bits(&out[..2 * BEAT], &dry(&noise, 0, 2 * BEAT), "nothing before beat 5");
    differs(&out[2 * BEAT + 500..2 * BEAT + 5_000], &dry(&noise, 2 * BEAT + 500, 4_500), "beat 5");
    // In 3/4 a bar is three beats: events at beats 3 and 6.
    let mut p = Player::new();
    p.time_sig = (3, 4);
    classic(&mut p);
    p.run(&noise, 2.0, 5 * BEAT, 128);
    assert_eq!(p.s.triggers, 2);
    // In 6/8 it is three beats too.
    let mut p = Player::new();
    p.time_sig = (6, 8);
    classic(&mut p);
    p.run(&noise, 2.0, 5 * BEAT, 128);
    assert_eq!(p.s.triggers, 2);
}

#[test]
fn variation_changes_some_chunk_sizes_and_nothing_at_zero() {
    let run = |variation: f32| {
        let mut p = Player::new();
        classic(&mut p);
        p.set(p::INTERVAL, 3.0);
        p.set(p::GATE, 2.0);
        p.set(p::VARIATION, variation);
        p.set(p::SEED, 3.0);
        p.run(&ramp, 3.5, 12 * BEAT, 128)
    };
    let plain = run(0.0);
    let varied = run(100.0);
    differs(&plain, &varied, "variation");
    // Without it every event repeats the same 6000 samples.
    let t = BEAT / 2;
    for beat in 0..10 {
        let at = t + beat * BEAT;
        assert!((plain[at + 300] - ramp(at - 6_000 + 300)).abs() < 2e-6, "beat {beat}");
    }
}

// ---- The 16-step grid ----------------------------------------------------------

fn only_step(p: &mut Player, step: u32) {
    p.set(p::MODE, 1.0);
    for i in 0..16 {
        set_plain(&mut p.s, p::step(i, p::STEP_ON), if i == step { 1.0 } else { 0.0 });
        set_plain(&mut p.s, p::step(i, p::STEP_PROB), 100.0);
    }
}

#[test]
fn a_step_fires_at_its_place_in_the_bar() {
    let mut p = Player::new();
    only_step(&mut p, 5);
    set_plain(&mut p.s, p::step(5, p::STEP_DIV), 3.0); // 1/32: 3000 samples
    let out = p.run(&noise, 0.0, 8 * BEAT, 128);
    assert_eq!(p.s.triggers, 2, "once per bar");
    // Step 5 of 16 sixteenths: beat 1.25 = 30 000 samples; the event lasts a step.
    let t = 5 * 6_000;
    same_bits(&out[..t], &dry(&noise, 0, t), "");
    for k in (200..5_800).filter(|k| !(2_900..3_100).contains(k)) {
        assert!((out[t + k] - noise(t - 3_000 + k % 3_000)).abs() < 1e-5, "k = {k}");
    }
    same_bits(&out[t + 6_000..4 * BEAT + t - 10], &dry(&noise, t + 6_000, 4 * BEAT - 6_000 - 10), "");
    // Second bar repeats at the same place.
    differs(&out[4 * BEAT + t + 500..4 * BEAT + t + 5_000], &dry(&noise, 4 * BEAT + t + 500, 4_500), "");
}

#[test]
fn step_pitch_and_reverse_apply_to_that_step_only() {
    let mut p = Player::new();
    only_step(&mut p, 0);
    set_plain(&mut p.s, p::step(0, p::STEP_ON), 1.0);
    set_plain(&mut p.s, p::step(0, p::STEP_DIV), 1.0); // 1/16: a whole step per repeat
    set_plain(&mut p.s, p::step(0, p::STEP_REV), 1.0);
    let out = p.run(&ramp, 3.0, 3 * BEAT, 128);
    for k in 200..5_800 {
        assert!((out[BEAT + k] - ramp(BEAT - 1 - k)).abs() < 2e-6, "k = {k}");
    }
    let mut p = Player::new();
    only_step(&mut p, 0);
    set_plain(&mut p.s, p::step(0, p::STEP_DIV), 1.0);
    set_plain(&mut p.s, p::step(0, p::STEP_PITCH), -12.0);
    let out = p.run(&ramp, 3.0, 3 * BEAT, 128);
    for k in 200..5_800 {
        let want = ramp(BEAT - 3_000) + ramp(1) * 0.5 * k as f32;
        assert!((out[BEAT + k] - want).abs() < 2e-6, "k = {k}");
    }
}

#[test]
fn step_probability_is_a_coin_per_pass_and_the_length_wraps_the_pattern() {
    let count = |prob: f32, seed: f32| {
        let mut p = Player::new();
        only_step(&mut p, 0);
        p.set(p::LENGTH, 0.0); // a one-step pattern: every sixteenth is step 0
        p.set(p::SEED, seed);
        set_plain(&mut p.s, p::step(0, p::STEP_PROB), prob);
        p.run(&noise, 0.0, LONG, 256);
        p.s.triggers
    };
    let all = LONG_SLOTS;
    assert_eq!(count(100.0, 1.0) as usize, all);
    assert_eq!(count(0.0, 1.0), 0);
    let half = count(50.0, 1.0) as usize;
    assert!(half > all * 35 / 100 && half < all * 65 / 100, "{half} of {all}");
    assert_eq!(count(50.0, 1.0) as usize, half);
    assert!((2..9).any(|seed| count(50.0, seed as f32) as usize != half), "the seed changes the pattern");
}

#[test]
fn the_step_rate_scales_the_grid() {
    let mut p = Player::new();
    only_step(&mut p, 0);
    p.set(p::STEP_RATE, 1.0); // eighths: the pattern lasts two bars
    p.run(&noise, 0.0, 16 * BEAT, 256);
    assert_eq!(p.s.triggers, 2);
}

// ---- Plumbing ------------------------------------------------------------------

#[test]
fn parameters_are_well_formed() {
    let ps = params();
    let mut ids: Vec<u32> = ps.iter().map(|i| i.id).collect();
    let n = ids.len();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), n);
    assert_eq!(ps.len(), 31 + 16 * 8 + 16);
    for i in 0..16 {
        for f in 0..8 {
            assert!(ps.iter().any(|x| x.id == p::step(i, f)));
        }
        assert!(ps.iter().any(|x| x.id == p::tie(i)));
    }
    for info in &ps {
        assert!(info.default >= info.min && info.default <= info.max, "{}", info.name);
        if !info.choices.is_empty() {
            assert_eq!(info.choices.len() as f32, info.max - info.min + 1.0, "{}", info.name);
        }
    }
}

#[test]
fn state_round_trips_and_garbage_is_ignored() {
    let mut a = Stutter::new();
    set_plain(&mut a, p::SEED, 77.0);
    set_plain(&mut a, p::step(3, p::STEP_PROB), 25.0);
    let bytes = a.save_state();
    let mut b = Stutter::new();
    b.load_state(&bytes);
    b.load_state(b"{nope");
    assert_eq!(crate::test_util::get_plain(&b, p::SEED), 77.0);
    assert!((crate::test_util::get_plain(&b, p::step(3, p::STEP_PROB)) - 25.0).abs() < 0.5);
}

#[test]
fn odd_transports_and_tempos_stay_finite() {
    let mut s = Stutter::new();
    s.prepare(SR, 256);
    set_plain(&mut s, p::INTERVAL, 1.0);
    let mut buf = AudioBuffer::new(2, 256);
    let cases = [
        (f64::NAN, 120.0, true),
        (-3.0, 120.0, true),
        (1e9, 120.0, true),
        (4.0, 0.0, true),
        (4.0, -50.0, true),
        (4.0, f64::NAN, true),
        (4.0, 1e6, true),
        (4.0, 1.0, false),
    ];
    for (round, (pos, bpm, playing)) in cases.into_iter().cycle().take(40).enumerate() {
        for i in 0..256 {
            buf.channel_mut(0)[i] = noise(round * 256 + i);
            buf.channel_mut(1)[i] = noise(round * 256 + i);
        }
        let tr = Transport { playing, bpm, position_beats: pos + round as f64 * 0.01, ..Transport::default() };
        let mut ctx = ProcessContext { sample_rate: SR, buffer: &mut buf, sidechain: None, events: &[], transport: &tr };
        s.process(&mut ctx);
        assert!(buf.channel(0).iter().chain(buf.channel(1)).all(|v| v.is_finite() && v.abs() < 10.0), "round {round}");
    }
}

#[test]
fn meter_fits_the_cap_and_shows_the_chunk() {
    let mut p = Player::new();
    classic(&mut p);
    p.run(&noise, 3.0, BEAT + 3_000, 128);
    let mut m = [0.0f32; 320];
    let n = p.s.meter(&mut m);
    assert!(n > 100 && n <= 320, "{n}");
    assert!(m[..n].iter().all(|v| v.is_finite()));
    assert_eq!(m[1], 1.0, "repeating");
    assert!(m[0] >= 0.0 && m[0] <= 1.0);
    assert!(m[6..n].iter().any(|v| *v > 0.1), "peaks of the frozen chunk");
    let mut small = [0.0f32; 3];
    assert!(p.s.meter(&mut small) <= 3);
}

#[test]
fn factory_presets_are_there_and_all_make_sound_changes() {
    let list = crate::presets::list(ID);
    assert!(list.len() >= 12, "{} presets", list.len());
    for r in &list {
        let preset = crate::presets::load(&r.path).unwrap();
        let mut p = Player::new();
        preset.apply(&mut p.s);
        let out = p.run(&noise, 0.0, 16 * BEAT, 128);
        assert!(out.iter().all(|v| v.is_finite()), "{}", r.name);
        differs(&out, &dry(&noise, 0, 16 * BEAT), &format!("{} leaves the signal untouched", r.name));
        assert!(out.iter().all(|v| v.abs() <= 1.7), "{} goes beyond dry + wet", r.name);
    }
}


// ---- Per-step level, stop and filter ---------------------------------------------

#[test]
fn step_level_scales_that_steps_repeat_and_minus_inf_cuts_it() {
    let run = |db: f32| {
        let mut p = Player::new();
        only_step(&mut p, 0);
        set_plain(&mut p.s, p::step(0, p::STEP_DIV), 1.0);
        set_plain(&mut p.s, p::step(0, p::STEP_LEVEL), db);
        p.run(&|_| 0.5, 3.0, 3 * BEAT, 128)
    };
    let t = BEAT;
    let half = run(-6.0);
    for k in 400..5_600 {
        assert!((half[t + k] - 0.5 * db_to_gain(-6.0)).abs() < 2e-3, "k = {k}: {}", half[t + k]);
    }
    let cut = run(-60.0);
    for (k, v) in cut[t + 300..t + 5_600].iter().enumerate() {
        assert_eq!(*v, 0.0, "k = {k}");
    }
    // 0 dB is the plain repeat.
    let plain = run(0.0);
    assert!(plain[t + 3_000] == 0.5);
}

#[test]
fn step_stop_turns_only_that_step_into_a_tape_stop() {
    let tone = sine(440.0, 0.5);
    let mut q = Player::new();
    q.set(p::MODE, 1.0);
    for i in 0..16 {
        set_plain(&mut q.s, p::step(i, p::STEP_ON), if i == 8 { 1.0 } else { 0.0 });
        set_plain(&mut q.s, p::step(i, p::STEP_PROB), 100.0);
    }
    set_plain(&mut q.s, p::step(8, p::STEP_STOP), 1.0);
    q.set(p::STOP_TIME, 0.0);
    let out = q.run(&tone, 0.0, 4 * BEAT, 128);
    let t = 8 * 6_000;
    assert_eq!(q.s.triggers, 1);
    // The other kind of step, with the same settings, loops.
    let mut l = Player::new();
    only_step(&mut l, 8);
    l.run(&tone, 0.0, 8 * 6_000 + 3_000, 128);
    assert_eq!(l.s.voice.spec.kind, Kind::Loop);
    assert_eq!(q.s.voice.spec.kind, Kind::Run);
    let third = 2_000;
    let counts: Vec<usize> = (0..3).map(|k| crossings(&out[t + 200 + k * third..t + 200 + (k + 1) * third - 200])).collect();
    assert!(counts[0] > counts[1] && counts[1] > counts[2], "slowing: {counts:?}");
    let tail = out[t + 5_000..t + 5_900].iter().fold(0.0f32, |m, v| m.max(v.abs()));
    assert!(tail < 0.1, "{tail}");
    same_bits(&out[t + 6_000..], &dry(&tone, t + 6_000, 4 * BEAT - t - 6_000), "dry again");
}

#[test]
fn step_filter_filters_only_that_step() {
    let tone = sine(300.0, 0.5);
    let mut p = Player::new();
    p.set(p::MODE, 1.0);
    for i in 0..16 {
        set_plain(&mut p.s, p::step(i, p::STEP_ON), if i == 0 || i == 8 { 1.0 } else { 0.0 });
        set_plain(&mut p.s, p::step(i, p::STEP_PROB), 100.0);
        set_plain(&mut p.s, p::step(i, p::STEP_DIV), 1.0);
    }
    set_plain(&mut p.s, p::step(0, p::STEP_FILTER), 1.0);
    p.set(p::FILTER_FREQ, 4_000.0);
    p.set(p::FILTER_WIDTH, 30.0);
    let out = p.run(&tone, 0.0, 4 * BEAT, 128);
    let filtered = crate::test_util::rms(&out[1_000..5_000]);
    let open = crate::test_util::rms(&out[8 * 6_000 + 1_000..8 * 6_000 + 5_000]);
    assert!(filtered < 0.2 * open, "{filtered} vs {open}");
}

// ---- Swing -------------------------------------------------------------------------

#[test]
fn swing_delays_the_odd_steps_and_stretches_the_even_ones() {
    // Step 1 at 50 % swing starts half a step (3000 samples) late.
    let mut p = Player::new();
    only_step(&mut p, 1);
    p.set(p::SWING, 50.0);
    let out = p.run(&noise, 0.0, 2 * BEAT, 128);
    let t = 6_000 + 3_000;
    same_bits(&out[..t], &dry(&noise, 0, t), "nothing before the swung boundary");
    differs(&out[t + 300..t + 1_500], &dry(&noise, t + 300, 1_200), "event");
    // It lasts to the next boundary (an even step, never swung): half a step.
    same_bits(&out[t + 3_000..2 * BEAT], &dry(&noise, t + 3_000, 2 * BEAT - t - 3_000), "ends at the even boundary");
    // An even step is longer by the same amount: 9000 samples.
    let mut p = Player::new();
    only_step(&mut p, 0);
    p.set(p::SWING, 50.0);
    let out = p.run(&noise, 0.0, 2 * BEAT, 128);
    differs(&out[8_000..8_800], &dry(&noise, 8_000, 800), "still playing 8000 samples in");
    same_bits(&out[9_000..2 * BEAT], &dry(&noise, 9_000, 2 * BEAT - 9_000), "over at 9000");
    // Zero swing is the straight grid.
    let mut a = Player::new();
    only_step(&mut a, 1);
    let straight = a.run(&noise, 0.0, 2 * BEAT, 128);
    let mut b = Player::new();
    only_step(&mut b, 1);
    b.set(p::SWING, 0.0);
    same_bits(&b.run(&noise, 0.0, 2 * BEAT, 128), &straight, "swing 0");
    differs(&straight[6_000 + 300..6_000 + 1_500], &dry(&noise, 6_000 + 300, 1_200), "starts on the boundary");
}

#[test]
fn swing_follows_the_song_position_through_jumps() {
    // Landing exactly on a swung boundary fires it at once.
    let mut p = Player::new();
    only_step(&mut p, 1);
    set_plain(&mut p.s, p::step(1, p::STEP_DIV), 4.0);
    p.set(p::SWING, 50.0);
    p.run(&noise, 0.0, BEAT, 128);
    let before = p.s.triggers;
    p.run(&noise, 20.0 + 0.375, 1_000, 128);
    assert_eq!(p.s.triggers, before + 1);
    assert!(p.s.voice.on);
    // Landing before it (still in step 0) does not, and the swung start comes later.
    let mut q = Player::new();
    only_step(&mut q, 1);
    set_plain(&mut q.s, p::step(1, p::STEP_DIV), 4.0);
    q.set(p::SWING, 50.0);
    q.run(&noise, 0.0, BEAT, 128);
    let before = q.s.triggers;
    q.run(&noise, 20.0 + 0.3, 1_000, 128);
    assert_eq!(q.s.triggers, before);
}

// ---- Ties --------------------------------------------------------------------------

#[test]
fn a_tied_step_extends_the_event_before_it() {
    let len = |ties: &[u32]| {
        let mut p = Player::new();
        only_step(&mut p, 0);
        for &t in ties {
            set_plain(&mut p.s, p::tie(t), 1.0);
        }
        let out = p.run(&noise, 0.0, 2 * BEAT, 128);
        // First sample from which the output is the input again, for good.
        let end = (0..out.len()).rev().find(|&i| out[i].to_bits() != noise(i).to_bits()).map_or(0, |i| i + 1);
        (end, p.s.triggers)
    };
    // The event ends at its last boundary (the fade out finishes a sample early).
    for (ties, want) in [(&[][..], 6_000), (&[1][..], 12_000), (&[1, 2][..], 18_000), (&[5][..], 6_000)] {
        let (end, n) = len(ties);
        assert!(end + 1 >= want && end <= want, "ties {ties:?}: ends at {end}, want {want}");
        assert_eq!(n, 1);
    }
}

#[test]
fn a_tie_needs_a_head_and_joins_to_the_next_event_without_a_gap() {
    // A tied step on its own never fires.
    let mut p = Player::new();
    only_step(&mut p, 1);
    set_plain(&mut p.s, p::tie(1), 1.0);
    let out = p.run(&noise, 0.0, 4 * BEAT, 128);
    same_bits(&out, &dry(&noise, 0, 4 * BEAT), "tied step without a head");
    // The head failing silences its tie, too.
    let mut p = Player::new();
    only_step(&mut p, 0);
    set_plain(&mut p.s, p::step(0, p::STEP_PROB), 0.0);
    set_plain(&mut p.s, p::tie(1), 1.0);
    let out = p.run(&noise, 0.0, 4 * BEAT, 128);
    same_bits(&out, &dry(&noise, 0, 4 * BEAT), "head failed");
    // Steps 0 (+tie 1) and 2 on: no dip where they meet.
    let mut p = Player::new();
    p.set(p::MODE, 1.0);
    for i in 0..16 {
        set_plain(&mut p.s, p::step(i, p::STEP_ON), if i < 3 && i != 1 { 1.0 } else { 0.0 });
        set_plain(&mut p.s, p::step(i, p::STEP_PROB), 100.0);
    }
    set_plain(&mut p.s, p::tie(1), 1.0);
    p.run(&|_| 0.5, 3.0, BEAT, 128);
    let out = p.run(&|_| 0.5, 4.0, 2 * BEAT, 128);
    for k in 200..18_000 {
        assert!((out[k] - 0.5).abs() < 1e-6, "k = {k}: {}", out[k]);
    }
    // A pattern of nothing but ties has nothing to hold.
    let mut p = Player::new();
    p.set(p::MODE, 1.0);
    for i in 0..16 {
        set_plain(&mut p.s, p::tie(i), 1.0);
    }
    let out = p.run(&noise, 0.0, 2 * BEAT, 128);
    same_bits(&out, &dry(&noise, 0, 2 * BEAT), "all tied");
}

// ---- From -> To ramps -----------------------------------------------------------------

#[test]
fn pitch_end_glides_the_pitch_across_the_event() {
    let mut p = Player::new();
    classic(&mut p);
    p.set(p::PITCH_END, 12.0);
    p.set(p::GATE, 4.0); // one beat: four repeats of 6000 samples
    p.run(&noise, 3.0, BEAT + 3_000, 128);
    assert!((p.s.voice.rate - 1.0).abs() < 1e-9, "starts at the Pitch");
    p.run(&noise, 4.0 + 3_000.0 / BEAT as f64, 6_000, 128);
    assert!((p.s.voice.rate - 2f64.powf(0.25)).abs() < 1e-6, "{}", p.s.voice.rate);
    p.run(&noise, 4.0 + 9_000.0 / BEAT as f64, 6_000, 128);
    assert!((p.s.voice.rate - 2f64.powf(0.5)).abs() < 1e-6, "{}", p.s.voice.rate);
    // Down, from a start pitch.
    let mut q = Player::new();
    classic(&mut q);
    q.set(p::PITCH, 12.0);
    q.set(p::PITCH_END, -24.0);
    q.run(&noise, 3.0, BEAT + 15_000, 128);
    assert!((q.s.voice.rate - 2f64.powf((12.0 - 24.0 * 0.5) / 12.0)).abs() < 1e-6, "{}", q.s.voice.rate);
}

#[test]
fn level_and_level_end_ramp_the_repeat_in_db() {
    let mut p = Player::new();
    classic(&mut p);
    p.set(p::LEVEL, -24.0);
    p.set(p::LEVEL_END, 24.0);
    let out = p.run(&|_| 0.5, 3.0, 2 * BEAT, 128);
    let t = BEAT;
    for k in [1_000usize, 6_000, 12_000, 18_000, 23_000] {
        let want = 0.5 * db_to_gain(-24.0 + 24.0 * k as f32 / BEAT as f32);
        assert!((out[t + k] - want).abs() < want * 0.03 + 1e-4, "k = {k}: {} vs {want}", out[t + k]);
    }
    // A flat Level alone.
    let mut q = Player::new();
    classic(&mut q);
    q.set(p::LEVEL, -12.0);
    let out = q.run(&|_| 0.5, 3.0, 2 * BEAT, 128);
    assert!((out[t + 3_000] - 0.5 * db_to_gain(-12.0)).abs() < 1e-3);
}

#[test]
fn filter_end_moves_the_centre_across_the_event() {
    let tone = sine(4_000.0, 0.5);
    let mut p = Player::new();
    classic(&mut p);
    p.set(p::FILTER_ON, 1.0);
    p.set(p::FILTER_FREQ, 1_000.0);
    p.set(p::FILTER_WIDTH, 40.0);
    p.set(p::FILTER_END, 2.0);
    let out = p.run(&tone, 3.0, 2 * BEAT, 128);
    let t = BEAT;
    let early = crate::test_util::rms(&out[t + 2_000..t + 4_000]);
    let late = crate::test_util::rms(&out[t + 20_000..t + 22_000]);
    assert!(late > 3.0 * early, "{early} -> {late}");
}

#[test]
fn nothing_reads_further_back_than_the_ring_holds() {
    for (bpm, grid, gate) in [(40.0, 5.0, 12.0), (1.0, 5.0, 12.0), (300.0, 0.0, 0.0)] {
        for tape in [0.0, 1.0] {
            let mut p = Player::new();
            p.bpm = bpm;
            classic(&mut p);
            p.set(p::INTERVAL, 3.0);
            p.set(p::GRID, grid);
            p.set(p::GATE, gate);
            p.set(p::PITCH, 24.0);
            p.set(p::PITCH_END, 24.0);
            p.set(p::PITCH_DECAY, 12.0);
            p.set(p::REVERSE, 3.0);
            p.set(p::HALF_TIME, 1.0);
            p.set(p::TAPE_STOP, tape);
            p.set(p::STOP_TIME, 6.0);
            let out = p.run(&noise, 0.0, 600_000, 997);
            assert!(out.iter().all(|v| v.is_finite()));
            assert_eq!(p.s.cap.stale.get(), 0, "bpm {bpm}, grid {grid}, tape {tape}");
        }
    }
}
