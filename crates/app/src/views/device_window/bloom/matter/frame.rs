//! The UI side of a frame: the mass's motion (breathing, the swell and
//! spikes of hits, the vapour's puffs, the magnet) and the uniforms it
//! becomes. Pure and cheap: a few dozen float operations per frame.

use super::mapping::{SoundLook, blend};
use super::material::Material;
use super::uniforms::Uniforms;

/// Sounds in a scene.
pub const SOUNDS: usize = 16;

/// What plays now, from the device meter.
#[derive(Clone, Copy, Debug)]
pub struct Live<'a> {
    /// Envelope per sound, 0..1.
    pub env: &'a [f32; SOUNDS],
    /// Transient pulse per sound (1 at a hit, then decaying).
    pub pulse: &'a [f32; SOUNDS],
    /// Each sound's look.
    pub looks: &'a [SoundLook; SOUNDS],
    /// The selected sound.
    pub selected: usize,
    /// Hit counter per sound (any change is a new hit).
    pub hits: &'a [f32; SOUNDS],
    /// Output RMS, linear.
    pub level: f32,
}

/// Where the panel is drawn, in target pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Panel {
    pub width: f32,
    pub height: f32,
    /// Centre of the mass and pixels per unit.
    pub cx: f32,
    pub cy: f32,
    pub scale: f32,
    pub corner: f32,
    /// The panel colour (linear) and whether the theme is light.
    pub bg: [f32; 3],
    pub light: bool,
    /// The colour around the panel's corners (sRGB-encoded).
    pub outer: [f32; 3],
}

impl Panel {
    /// The mass `z` times bigger, centred in the same place (small stages,
    /// where the vapour has no room to rise anyway, want it to fill them).
    pub fn zoomed(mut self, z: f32) -> Panel {
        self.scale *= z;
        self
    }

    /// A panel of `width` × `height` px with the mass centred in the
    /// rectangle `(x, y, w, h)` of it (the space left by any overlays).
    pub fn fit(width: f32, height: f32, area: (f32, f32, f32, f32), corner: f32, bg: [f32; 3], light: bool, outer: [f32; 3]) -> Panel {
        let (x, y, w, h) = area;
        let scale = (w.min(h) * 0.54).max(1.0);
        // The mass sits a little low, leaving room for its vapour.
        Panel { width, height, cx: x + w * 0.5, cy: y + h * 0.56, scale, corner, bg, light, outer }
    }

    /// A pointer position (target pixels) in the shader's units.
    pub fn units(&self, x: f32, y: f32) -> (f32, f32) {
        ((x - self.cx) / self.scale, (self.cy - y) / self.scale)
    }
}

/// Sounds shown at once in their own colours (the uniforms' voices).
pub const VOICES: usize = 4;
/// How long a hit's ripple and droplet live, s.
pub const RIPPLE_S: f32 = 1.4;

/// One sound currently showing in its own colour: a tinted patch of the
/// film around its place on the mass, a ripple and a droplet at each hit.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Voice {
    pub sound: usize,
    /// Its envelope, smoothed (0 = the slot is free).
    pub level: f32,
    /// Seconds since its last hit.
    pub age: f32,
    /// How hard that hit was, 0..1.
    pub kick: f32,
    /// Its colour (linear), eased.
    pub colour: [f32; 3],
}

impl Default for Voice {
    fn default() -> Self {
        Voice { sound: 0, level: 0.0, age: RIPPLE_S * 2.0, kick: 0.0, colour: [0.0; 3] }
    }
}

/// Where sound `i` lives on the mass: spread over the side facing the
/// viewer (golden-angle spiral), so every sound has its own patch.
pub fn voice_dir(i: usize) -> [f32; 3] {
    let k = i as f32 + 0.5;
    let a = k * 2.399_963;
    let y = 0.78 - 1.36 * k / SOUNDS as f32;
    let r = (1.0 - y * y).max(0.0).sqrt();
    let (x, z) = (a.cos() * r * 0.9, 0.45 + 0.55 * a.sin().abs() * r);
    let n = (x * x + y * y + z * z).sqrt();
    [x / n, y / n, z / n]
}

/// The slot sound `i` should take: its own, else a free one, else the
/// quietest when `i` is louder than it.
fn slot_for(voices: &[Voice; VOICES], i: usize, loudness: f32) -> Option<usize> {
    if let Some(k) = voices.iter().position(|v| v.sound == i && v.level > 0.0) {
        return Some(k);
    }
    let quiet = (0..VOICES).min_by(|&a, &b| voices[a].level.total_cmp(&voices[b].level))?;
    (voices[quiet].level < 0.01 || voices[quiet].level < loudness * 0.8).then_some(quiet)
}

/// The mass's motion between frames.
#[derive(Clone, Copy, Debug)]
pub struct Animator {
    pub time: f32,
    look: SoundLook,
    focus: usize,
    focus_w: f32,
    excite: f32,
    swell: f32,
    puff: f32,
    shudder: f32,
    cursor: (f32, f32),
    presence: f32,
    started: bool,
    /// The sounds showing in their own colours.
    pub voices: [Voice; VOICES],
    seen_hits: Option<[f32; SOUNDS]>,
    /// The output level, smoothed (the glow behind the mass).
    glow: f32,
}

impl Default for Animator {
    fn default() -> Self {
        Self {
            time: 0.0,
            look: SoundLook::default(),
            focus: 0,
            focus_w: 0.0,
            excite: 0.0,
            swell: 0.0,
            puff: 0.0,
            shudder: 0.0,
            cursor: (0.0, 0.0),
            presence: 0.0,
            started: false,
            voices: [Voice::default(); VOICES],
            seen_hits: None,
            glow: 0.0,
        }
    }
}

#[inline]
fn approach(x: &mut f32, target: f32, dt: f32, up: f32, down: f32) {
    let tau = if target > *x { up } else { down };
    *x += (target - *x) * (1.0 - (-dt / tau.max(1e-4)).exp());
}

impl Animator {
    /// Advance by `dt` seconds. `cursor`: the pointer in units, when over the
    /// panel. Returns whether anything is still moving faster than the idle
    /// breathing (the caller then draws at the full frame rate).
    pub fn step(&mut self, dt: f32, live: &Live, cursor: Option<(f32, f32)>, reduced: bool) -> bool {
        let selected = live.selected.min(SOUNDS - 1);
        if reduced {
            // One still frame of the selected sound.
            self.look = live.looks[selected];
            self.focus_w = 0.0;
            self.excite = 0.0;
            self.swell = 0.0;
            self.puff = 0.0;
            self.shudder = 0.0;
            self.presence = 0.0;
            self.started = true;
            self.voices = [Voice::default(); VOICES];
            self.glow = 0.0;
            self.seen_hits = Some(*live.hits);
            return false;
        }
        let dt = dt.clamp(0.0, 0.1);
        self.time += dt;
        if !self.started {
            self.look = live.looks[selected];
            self.started = true;
        }
        // The sound playing loudest takes the stage while it sounds.
        let (mut loud, mut level) = (selected, 0.0f32);
        let (mut hit, mut pulse) = (selected, 0.0f32);
        for i in 0..SOUNDS {
            if live.env[i] > level {
                level = live.env[i];
                loud = i;
            }
            if live.pulse[i] > pulse {
                pulse = live.pulse[i];
                hit = i;
            }
        }
        if pulse > 0.3 {
            self.focus = hit;
        } else if level > 0.02 && self.focus_w < 0.05 {
            self.focus = loud;
        }
        let focus_level = live.env[self.focus].clamp(0.0, 1.0);
        approach(&mut self.focus_w, (focus_level * 1.6).min(1.0), dt, 0.03, 0.55);
        let target = if self.focus == selected { live.looks[selected] } else { blend(&live.looks[selected], &live.looks[self.focus], self.focus_w) };
        let k = 1.0 - (-dt / 0.12).exp();
        self.look = blend(&self.look, &target, k);

        // Hits: spikes rise at once and sink slowly (a ferrofluid relaxing),
        // the surface shudders for an instant, the vapour puffs.
        let gain = live.looks[hit].spike_gain;
        approach(&mut self.excite, pulse * gain, dt, 0.012, 0.32);
        approach(&mut self.shudder, pulse, dt, 0.005, 0.09);
        let swell = level.clamp(0.0, 1.0) * live.looks[loud].swell;
        approach(&mut self.swell, swell, dt, 0.025, 0.3);
        if pulse > 0.2 {
            self.puff = (self.puff + pulse * dt * 6.0).min(1.0);
        }
        approach(&mut self.puff, 0.0, dt, 1.0, 1.4);

        // Every sound that plays shows in its own colour; a hit sends a
        // ripple across its patch and a droplet off it.
        let seen = self.seen_hits.unwrap_or(*live.hits);
        for (i, was) in seen.iter().enumerate() {
            let hit = live.hits[i] != *was;
            let loud = live.env[i].clamp(0.0, 1.0);
            if (hit || loud > 0.04)
                && let Some(s) = slot_for(&self.voices, i, loud)
            {
                let v = &mut self.voices[s];
                if v.sound != i || v.level <= 0.0 {
                    *v = Voice { sound: i, level: 0.001, colour: live.looks[i].accent, ..Voice::default() };
                }
                if hit {
                    v.age = 0.0;
                    v.kick = live.pulse[i].clamp(0.6, 1.0).max(loud);
                }
            }
        }
        self.seen_hits = Some(*live.hits);
        for v in &mut self.voices {
            if v.level <= 0.0 {
                continue;
            }
            let target = live.env[v.sound].clamp(0.0, 1.0);
            approach(&mut v.level, target.max(0.001), dt, 0.02, 0.45);
            v.age = (v.age + dt).min(RIPPLE_S * 2.0);
            for (a, b) in v.colour.iter_mut().zip(live.looks[v.sound].accent) {
                *a += (b - *a) * k;
            }
            if v.level < 0.004 && v.age > RIPPLE_S {
                *v = Voice::default();
            }
        }
        approach(&mut self.glow, live.level.clamp(0.0, 1.0), dt, 0.03, 0.4);
        let voicing = self.voices.iter().any(|v| v.level > 0.004 || v.age < RIPPLE_S) || self.glow > 0.004;

        // The pointer: a magnet that attracts from outside and pushes in.
        match cursor {
            Some(c) => {
                let a = 1.0 - (-dt / 0.06).exp();
                self.cursor.0 += (c.0 - self.cursor.0) * a;
                self.cursor.1 += (c.1 - self.cursor.1) * a;
                approach(&mut self.presence, 1.0, dt, 0.25, 0.25);
            }
            None => approach(&mut self.presence, 0.0, dt, 0.4, 0.4),
        }
        let settling = (self.presence > 0.01 && self.presence < 0.99) || cursor.is_some();
        self.excite > 0.004 || self.swell > 0.004 || self.puff > 0.01 || self.shudder > 0.004 || settling || self.focus_w > 0.01 || voicing
    }

    /// Where voice `v`'s droplet is (units from the mass's centre) and its
    /// radius: it leaves the patch at the hit, rises, spins and falls back in.
    pub fn droplet(&self, v: &Voice) -> ([f32; 3], f32) {
        let fly = (v.age / RIPPLE_S).clamp(0.0, 1.0);
        if fly >= 1.0 {
            return ([0.0; 3], 0.0);
        }
        let d = voice_dir(v.sound);
        let size = self.look.size;
        // Up and back down: a parabola that lands where it left.
        let h = 4.0 * fly * (1.0 - fly) * (0.24 + 0.22 * v.kick) * size;
        let spin = fly * 1.4 * if v.sound.is_multiple_of(2) { 1.0 } else { -1.0 };
        let (s, c) = spin.sin_cos();
        let dir = [d[0] * c - d[2] * s, d[1], d[0] * s + d[2] * c];
        let r0 = 0.6 * size;
        let at = [dir[0] * (r0 + h), dir[1] * (r0 + h) + 0.45 * h, dir[2] * (r0 + h)];
        (at, (0.035 + 0.04 * v.kick) * size * (1.0 - 0.45 * fly))
    }

    /// How far from the centre the droplets reach (units), for the march's bound.
    fn reach(&self) -> f32 {
        self.voices
            .iter()
            .filter(|v| v.level > 0.0)
            .map(|v| {
                let (at, r) = self.droplet(v);
                if r > 0.0 { (at[0] * at[0] + at[1] * at[1] + at[2] * at[2]).sqrt() + r } else { 0.0 }
            })
            .fold(0.0, f32::max)
    }

    /// The pull of the magnet: attraction from outside the mass, a push
    /// once the pointer is over it.
    pub fn pull(&self) -> f32 {
        let r = (self.cursor.0 * self.cursor.0 + self.cursor.1 * self.cursor.1).sqrt();
        let near = 1.0 - ((r - 0.55) / 0.45).clamp(0.0, 1.0);
        if r < 0.42 { -0.7 } else { near }
    }

    /// The uniforms of this frame.
    pub fn uniforms(&self, material: Material, panel: &Panel, seed: f32) -> Uniforms {
        let m = material.params();
        let l = &self.look;
        let excite = self.excite;
        let rough = (m.roughness + l.roughness * (1.0 - 0.6 * m.metal)).clamp(0.0, 1.0);
        let light = if panel.light { 1.0 } else { 0.0 };
        Uniforms {
            res: [panel.width, panel.height, self.time, seed],
            frame: [panel.cx, panel.cy, panel.scale, panel.corner],
            bg: [panel.bg[0], panel.bg[1], panel.bg[2], light],
            outer: [panel.outer[0], panel.outer[1], panel.outer[2], 0.0],
            shape0: [l.size, self.swell, 1.25, l.churn],
            shape1: [l.lumps * m.lumps, l.fold_freq, l.wobble_freq, l.wobble_amp * (1.0 + self.swell)],
            spikes: [(0.04 + excite * 0.95) * m.spikes, l.spike_sharp, l.spike_count, self.shudder],
            cursor: [self.cursor.0, self.cursor.1, self.pull(), self.presence],
            mat0: [m.albedo[0], m.albedo[1], m.albedo[2], m.metal],
            mat1: [rough, m.clearcoat, m.iridescence, m.film],
            mat2: [m.film_range + l.film_range, l.film_shift, m.rim, m.glass],
            mat3: [m.emission[0], m.emission[1], m.emission[2], m.emission_amount * (1.0 + excite * 0.8)],
            mat4: [m.wrap, m.exposure, 1.0, m.warmth],
            smoke0: [l.smoke_amount * m.smoke * 0.8, self.puff, l.smoke_rise, l.turbulence],
            smoke_a: [l.smoke[0][0], l.smoke[0][1], l.smoke[0][2], 0.0],
            smoke_b: [l.smoke[1][0], l.smoke[1][1], l.smoke[1][2], 0.0],
            smoke_c: [l.smoke[2][0], l.smoke[2][1], l.smoke[2][2], 0.0],
            tint: [l.accent[0], l.accent[1], l.accent[2], 0.3],
            glow: [self.glow, self.voices.iter().map(|v| v.level).sum::<f32>().min(1.5), self.reach(), if self.voices.iter().any(|v| v.level > 0.0 && v.age < RIPPLE_S) { 1.0 } else { 0.0 }],
            voice: std::array::from_fn(|k| {
                let v = &self.voices[k];
                [v.colour[0], v.colour[1], v.colour[2], v.level]
            }),
            vdir: std::array::from_fn(|k| {
                let v = &self.voices[k];
                let d = voice_dir(v.sound);
                let ripple = if v.level > 0.0 && v.age < RIPPLE_S { v.age } else { -1.0 };
                [d[0], d[1], d[2], ripple]
            }),
            drop: std::array::from_fn(|k| {
                let v = &self.voices[k];
                let (at, r) = if v.level > 0.0 { self.droplet(v) } else { ([0.0; 3], 0.0) };
                [at[0], at[1], at[2], r]
            }),
            kick: std::array::from_fn(|k| self.voices[k].kick),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::mapping::sound_look;
    use super::*;
    use aitrack_stock::instrument::bloom::timbre::Timbre;

    fn looks() -> [SoundLook; SOUNDS] {
        std::array::from_fn(|i| {
            let t = Timbre { centroid: 80.0 * (1.35f32).powi(i as i32), noisiness: (i as f32 / 15.0), energy: 0.5, attack_ms: 1.0, decay_ms: 200.0 - i as f32 * 12.0, inharmonic: 0.1, wobble: 0.0 };
            sound_look(&t, false)
        })
    }

    const NO_HITS: [f32; SOUNDS] = [0.0; SOUNDS];

    fn panel() -> Panel {
        Panel::fit(400.0, 300.0, (0.0, 0.0, 400.0, 300.0), 8.0, [0.02; 3], false, [0.1; 3])
    }

    #[test]
    fn several_sounds_show_at_once_in_their_own_colours() {
        let l = looks();
        let mut a = Animator::default();
        let (mut env, mut pulse, mut hits) = ([0.0; SOUNDS], [0.0; SOUNDS], [0.0; SOUNDS]);
        let dt = 1.0 / 60.0;
        a.step(dt, &Live { env: &env, pulse: &pulse, looks: &l, selected: 0, hits: &hits, level: 0.0 }, None, false);
        for i in [2, 9, 13] {
            env[i] = 0.8;
            pulse[i] = 1.0;
            hits[i] += 1.0;
        }
        for _ in 0..4 {
            a.step(dt, &Live { env: &env, pulse: &pulse, looks: &l, selected: 0, hits: &hits, level: 0.4 }, None, false);
        }
        let mut shown: Vec<usize> = a.voices.iter().filter(|v| v.level > 0.0).map(|v| v.sound).collect();
        shown.sort();
        assert_eq!(shown, vec![2, 9, 13]);
        let u = a.uniforms(Material::Obsidian, &panel(), 0.0);
        let (k, v) = a.voices.iter().enumerate().find(|(_, v)| v.sound == 9).unwrap();
        assert!((u.voice[k][0] - l[9].accent[0]).abs() < 0.05, "its own colour");
        assert!(u.vdir[k][3] >= 0.0 && u.vdir[k][3] < 0.2, "its ripple runs");
        assert!(u.drop[k][3] > 0.0, "its droplet flies");
        assert!(u.glow[0] > 0.1 && u.glow[2] > 0.6, "the glow swells, the droplets reach out");
        assert!(v.kick >= 0.6);
        // Another hit of the same sound restarts its ripple, in the same slot.
        for _ in 0..30 {
            a.step(dt, &Live { env: &env, pulse: &pulse, looks: &l, selected: 0, hits: &hits, level: 0.4 }, None, false);
        }
        assert!(a.voices[k].age > 0.4);
        hits[9] += 1.0;
        a.step(dt, &Live { env: &env, pulse: &pulse, looks: &l, selected: 0, hits: &hits, level: 0.4 }, None, false);
        assert_eq!(a.voices[k].sound, 9);
        assert!(a.voices[k].age < 0.05);
        // Silence: every voice fades, the droplets land, the slots free up.
        let quiet = [0.0; SOUNDS];
        for _ in 0..600 {
            a.step(dt, &Live { env: &quiet, pulse: &quiet, looks: &l, selected: 0, hits: &hits, level: 0.0 }, None, false);
        }
        assert!(a.voices.iter().all(|v| v.level == 0.0));
        let u = a.uniforms(Material::Obsidian, &panel(), 0.0);
        assert!(u.drop.iter().all(|d| d[3] == 0.0) && u.vdir.iter().all(|d| d[3] < 0.0));
        assert!(!a.step(dt, &Live { env: &quiet, pulse: &quiet, looks: &l, selected: 0, hits: &hits, level: 0.0 }, None, false), "idle again");
    }

    #[test]
    fn a_fifth_sound_takes_the_quietest_slot() {
        let l = looks();
        let mut a = Animator::default();
        let (mut env, mut hits) = ([0.0; SOUNDS], [0.0; SOUNDS]);
        let dt = 1.0 / 60.0;
        a.step(dt, &Live { env: &env, pulse: &env, looks: &l, selected: 0, hits: &hits, level: 0.0 }, None, false);
        for (i, e) in [(0, 0.9), (1, 0.8), (2, 0.05), (3, 0.7)] {
            env[i] = e;
            hits[i] = 1.0;
        }
        for _ in 0..30 {
            a.step(dt, &Live { env: &env, pulse: &env, looks: &l, selected: 0, hits: &hits, level: 0.0 }, None, false);
        }
        env[5] = 1.0;
        hits[5] = 1.0;
        a.step(dt, &Live { env: &env, pulse: &env, looks: &l, selected: 0, hits: &hits, level: 0.0 }, None, false);
        let shown: Vec<usize> = a.voices.iter().map(|v| v.sound).collect();
        assert!(shown.contains(&5) && !shown.contains(&2), "{shown:?}");
    }

    #[test]
    fn droplets_rise_and_fall_back_into_their_patch() {
        let a = Animator::default();
        let v = |age: f32| Voice { sound: 4, level: 0.5, age, kick: 1.0, colour: [0.5; 3] };
        let len = |p: [f32; 3]| (p[0] * p[0] + p[1] * p[1] + p[2] * p[2]).sqrt();
        let (start, r0) = a.droplet(&v(0.0));
        let (top, _) = a.droplet(&v(RIPPLE_S * 0.5));
        let (end, r1) = a.droplet(&v(RIPPLE_S * 0.98));
        assert!(len(top) > len(start) + 0.25, "it flies out");
        assert!(len(end) < len(top) - 0.25, "and comes back");
        assert!(r0 > r1 && r1 > 0.0);
        assert_eq!(a.droplet(&v(RIPPLE_S)).1, 0.0, "landed");
        // Every sound has its own patch, on the side facing the viewer.
        for i in 0..SOUNDS {
            let d = voice_dir(i);
            assert!((len(d) - 1.0).abs() < 1e-4 && d[2] > 0.3, "{i}: {d:?}");
            for j in 0..i {
                let e = voice_dir(j);
                assert!(d[0] * e[0] + d[1] * e[1] + d[2] * e[2] < 0.995, "{i} and {j} apart");
            }
        }
    }

    #[test]
    fn a_hit_spikes_and_puffs_then_settles() {
        let l = looks();
        let quiet = [0.0; SOUNDS];
        let mut a = Animator::default();
        let live = Live { env: &quiet, pulse: &quiet, looks: &l, selected: 3, hits: &NO_HITS, level: 0.0 };
        a.step(1.0 / 60.0, &live, None, false);
        let rest = a.uniforms(Material::Obsidian, &panel(), 0.0);
        let (mut env, mut pulse) = ([0.0; SOUNDS], [0.0; SOUNDS]);
        env[9] = 1.0;
        pulse[9] = 1.0;
        for _ in 0..6 {
            a.step(1.0 / 60.0, &Live { env: &env, pulse: &pulse, looks: &l, selected: 3, hits: &NO_HITS, level: 0.0 }, None, false);
        }
        let hit = a.uniforms(Material::Obsidian, &panel(), 0.0);
        assert!(hit.spikes[0] > rest.spikes[0] + 0.2, "spikes grow on a hit");
        assert!(hit.smoke0[1] > 0.1, "the vapour puffs");
        assert!(hit.shape0[1] > 0.2, "it swells");
        // The look moves towards the sound that played.
        assert!((hit.mat2[1] - l[9].film_shift).abs() < (rest.mat2[1] - l[9].film_shift).abs());
        for _ in 0..900 {
            a.step(1.0 / 60.0, &Live { env: &quiet, pulse: &quiet, looks: &l, selected: 3, hits: &NO_HITS, level: 0.0 }, None, false);
        }
        let after = a.uniforms(Material::Obsidian, &panel(), 0.0);
        assert!((after.spikes[0] - rest.spikes[0]).abs() < 0.01);
        assert!((after.mat2[1] - l[3].film_shift).abs() < 0.01, "back to the selected sound");
        assert!(!a.step(1.0 / 60.0, &Live { env: &quiet, pulse: &quiet, looks: &l, selected: 3, hits: &NO_HITS, level: 0.0 }, None, false), "idle again");
    }

    #[test]
    fn reduced_motion_is_one_still_frame() {
        let l = looks();
        let mut env = [0.0; SOUNDS];
        env[2] = 1.0;
        let mut a = Animator::default();
        let live = Live { env: &env, pulse: &env, looks: &l, selected: 5, hits: &NO_HITS, level: 0.0 };
        assert!(!a.step(0.016, &live, Some((0.1, 0.2)), true));
        let u1 = a.uniforms(Material::Pearl, &panel(), 1.0);
        a.step(0.5, &live, Some((0.4, 0.2)), true);
        assert_eq!(u1, a.uniforms(Material::Pearl, &panel(), 1.0), "time stands still and nothing reacts");
        assert_eq!(u1.cursor[3], 0.0);
    }

    #[test]
    fn the_magnet_attracts_outside_and_pushes_inside() {
        let mut a = Animator::default();
        let l = looks();
        let q = [0.0; SOUNDS];
        for _ in 0..60 {
            a.step(1.0 / 60.0, &Live { env: &q, pulse: &q, looks: &l, selected: 0, hits: &NO_HITS, level: 0.0 }, Some((0.7, 0.0)), false);
        }
        assert!(a.pull() > 0.5);
        for _ in 0..60 {
            a.step(1.0 / 60.0, &Live { env: &q, pulse: &q, looks: &l, selected: 0, hits: &NO_HITS, level: 0.0 }, Some((0.1, 0.0)), false);
        }
        assert!(a.pull() < 0.0);
        assert!(a.uniforms(Material::Obsidian, &panel(), 0.0).cursor[3] > 0.9);
    }

    #[test]
    fn panels_fit_their_area_and_map_the_pointer() {
        let p = Panel::fit(400.0, 300.0, (0.0, 0.0, 400.0, 240.0), 0.0, [0.0; 3], true, [0.0; 3]);
        assert_eq!(p.scale, 240.0 * 0.54);
        let (x, y) = p.units(p.cx + p.scale * 0.5, p.cy - p.scale);
        assert!((x - 0.5).abs() < 1e-5 && (y - 1.0).abs() < 1e-5);
    }

    #[test]
    fn a_frame_costs_microseconds() {
        let l = looks();
        let mut env = [0.0; SOUNDS];
        let mut a = Animator::default();
        let p = panel();
        let n = 20_000;
        let t0 = std::time::Instant::now();
        let mut sink = 0.0;
        for k in 0..n {
            env[k % SOUNDS] = (k % 7) as f32 / 7.0;
            a.step(1.0 / 60.0, &Live { env: &env, pulse: &env, looks: &l, selected: k % SOUNDS, hits: &NO_HITS, level: 0.0 }, Some((0.3, 0.1)), false);
            sink += a.uniforms(Material::ALL[k % 7], &p, 0.0).bytes()[17] as f32;
        }
        let per = t0.elapsed().as_secs_f64() * 1000.0 / n as f64;
        assert!(sink >= 0.0);
        // Debug builds are slow; the release cost is a few microseconds.
        assert!(per < 0.05, "{per} ms per frame");
    }
}
