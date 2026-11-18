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
}

impl Default for Animator {
    fn default() -> Self {
        Self { time: 0.0, look: SoundLook::default(), focus: 0, focus_w: 0.0, excite: 0.0, swell: 0.0, puff: 0.0, shudder: 0.0, cursor: (0.0, 0.0), presence: 0.0, started: false }
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
        self.excite > 0.004 || self.swell > 0.004 || self.puff > 0.01 || self.shudder > 0.004 || settling || self.focus_w > 0.01
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

    fn panel() -> Panel {
        Panel::fit(400.0, 300.0, (0.0, 0.0, 400.0, 300.0), 8.0, [0.02; 3], false, [0.1; 3])
    }

    #[test]
    fn a_hit_spikes_and_puffs_then_settles() {
        let l = looks();
        let quiet = [0.0; SOUNDS];
        let mut a = Animator::default();
        let live = Live { env: &quiet, pulse: &quiet, looks: &l, selected: 3 };
        a.step(1.0 / 60.0, &live, None, false);
        let rest = a.uniforms(Material::Obsidian, &panel(), 0.0);
        let (mut env, mut pulse) = ([0.0; SOUNDS], [0.0; SOUNDS]);
        env[9] = 1.0;
        pulse[9] = 1.0;
        for _ in 0..6 {
            a.step(1.0 / 60.0, &Live { env: &env, pulse: &pulse, looks: &l, selected: 3 }, None, false);
        }
        let hit = a.uniforms(Material::Obsidian, &panel(), 0.0);
        assert!(hit.spikes[0] > rest.spikes[0] + 0.2, "spikes grow on a hit");
        assert!(hit.smoke0[1] > 0.1, "the vapour puffs");
        assert!(hit.shape0[1] > 0.2, "it swells");
        // The look moves towards the sound that played.
        assert!((hit.mat2[1] - l[9].film_shift).abs() < (rest.mat2[1] - l[9].film_shift).abs());
        for _ in 0..900 {
            a.step(1.0 / 60.0, &Live { env: &quiet, pulse: &quiet, looks: &l, selected: 3 }, None, false);
        }
        let after = a.uniforms(Material::Obsidian, &panel(), 0.0);
        assert!((after.spikes[0] - rest.spikes[0]).abs() < 0.01);
        assert!((after.mat2[1] - l[3].film_shift).abs() < 0.01, "back to the selected sound");
        assert!(!a.step(1.0 / 60.0, &Live { env: &quiet, pulse: &quiet, looks: &l, selected: 3 }, None, false), "idle again");
    }

    #[test]
    fn reduced_motion_is_one_still_frame() {
        let l = looks();
        let mut env = [0.0; SOUNDS];
        env[2] = 1.0;
        let mut a = Animator::default();
        let live = Live { env: &env, pulse: &env, looks: &l, selected: 5 };
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
            a.step(1.0 / 60.0, &Live { env: &q, pulse: &q, looks: &l, selected: 0 }, Some((0.7, 0.0)), false);
        }
        assert!(a.pull() > 0.5);
        for _ in 0..60 {
            a.step(1.0 / 60.0, &Live { env: &q, pulse: &q, looks: &l, selected: 0 }, Some((0.1, 0.0)), false);
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
            a.step(1.0 / 60.0, &Live { env: &env, pulse: &env, looks: &l, selected: k % SOUNDS }, Some((0.3, 0.1)), false);
            sink += a.uniforms(Material::ALL[k % 7], &p, 0.0).bytes()[17] as f32;
        }
        let per = t0.elapsed().as_secs_f64() * 1000.0 / n as f64;
        assert!(sink >= 0.0);
        // Debug builds are slow; the release cost is a few microseconds.
        assert!(per < 0.05, "{per} ms per frame");
    }
}
