//! The Bloom field: a metaball ("ferrofluid") blend of the sixteen cells and
//! the core, rendered on the CPU into a small premultiplied BGRA image that
//! the visual paints scaled up (GPUI has no custom shaders).
//!
//! Per frame:
//!
//! 1. **Splat**: every cell adds a compact kernel `(1 − q/4)³` (q = squared
//!    distance over the squared radius in that direction) to a scalar field,
//!    and the same weight times its colour to a colour field, only inside
//!    its own bounding box. The radius is modulated around the cell by its
//!    lobes (`T_n` of the direction cosine, a Chebyshev polynomial, so no
//!    `atan2` per pixel) and its FM wobble. A wider, softer kernel collects
//!    the glow. Transient rings add thin annuli.
//! 2. **Shade**: the iso-surface of the field is the liquid. Its gradient
//!    gives a normal, lit from the top left (diffuse plus a tight specular
//!    highlight for the glossy, metallic look), with a rim light at the
//!    edges; outside the surface the glow shows.
//!
//! Every loop runs over contiguous rows of `f32` slices, so the compiler can
//! vectorise them. A 160 × 120 grid with sixteen cells costs well under a
//! millisecond in a release build (see the `field_cost` test).

/// One blob to draw, in grid pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Blob {
    pub x: f32,
    pub y: f32,
    pub r: f32,
    /// Linear sRGB.
    pub rgb: [f32; 3],
    pub lobes: u32,
    pub spikes: f32,
    pub sharp: f32,
    pub wobble: f32,
    /// Rotation of the lobes, radians.
    pub phase: f32,
    /// Glow strength (0 = none).
    pub glow: f32,
}

/// A ring a transient emits.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Ring {
    pub x: f32,
    pub y: f32,
    pub radius: f32,
    pub width: f32,
    pub alpha: f32,
    pub rgb: [f32; 3],
}

/// The field's buffers (reused frame to frame).
#[derive(Default)]
pub struct Field {
    w: usize,
    h: usize,
    f: Vec<f32>,
    cr: Vec<f32>,
    cg: Vec<f32>,
    cb: Vec<f32>,
    gr: Vec<f32>,
    gg: Vec<f32>,
    gb: Vec<f32>,
    px: Vec<u8>,
    lut: Vec<u8>,
}

/// The surface's iso value: an isolated round cell's edge sits at its radius.
const ISO: f32 = 0.42;
const LUT: usize = 4096;

impl Field {
    fn resize(&mut self, w: usize, h: usize) {
        if self.w != w || self.h != h {
            let n = w * h;
            for b in [&mut self.f, &mut self.cr, &mut self.cg, &mut self.cb, &mut self.gr, &mut self.gg, &mut self.gb] {
                b.clear();
                b.resize(n, 0.0);
            }
            self.px = vec![0; n * 4];
            self.w = w;
            self.h = h;
        }
        if self.lut.is_empty() {
            self.lut = (0..LUT).map(|i| (super::look::encode(i as f32 / (LUT - 1) as f32) * 255.0 + 0.5) as u8).collect();
        }
        for b in [&mut self.f, &mut self.cr, &mut self.cg, &mut self.cb, &mut self.gr, &mut self.gg, &mut self.gb] {
            b.fill(0.0);
        }
    }

    /// Render `blobs` and `rings` into a `w` × `h` premultiplied BGRA image.
    /// `time` (seconds) moves the wobble; `light` is a light theme (softer glow).
    pub fn render(&mut self, w: usize, h: usize, blobs: &[Blob], rings: &[Ring], time: f32, light: bool) -> &[u8] {
        let (w, h) = (w.max(2), h.max(2));
        self.resize(w, h);
        for b in blobs {
            self.splat(b, time);
        }
        for r in rings {
            self.ring(r);
        }
        self.shade(light);
        &self.px
    }

    fn splat(&mut self, b: &Blob, time: f32) {
        let reach = b.r * (1.0 + b.spikes + b.wobble * 0.25);
        let glow_r = reach * 2.4;
        let (x0, x1) = ((b.x - glow_r).floor().max(0.0) as usize, ((b.x + glow_r).ceil() as usize).min(self.w - 1));
        let (y0, y1) = ((b.y - glow_r).floor().max(0.0) as usize, ((b.y + glow_r).ceil() as usize).min(self.h - 1));
        if x0 > x1 || y0 > y1 || b.r <= 0.0 {
            return;
        }
        let (ps, pc) = b.phase.sin_cos();
        let (ws, wc) = (b.phase * 0.7 + time * 1.3).sin_cos();
        let inv_r2 = 1.0 / (b.r * b.r);
        let inv_g2 = 1.0 / (glow_r * glow_r);
        let n = b.lobes.clamp(1, 12);
        let sharp = b.sharp.round().clamp(1.0, 8.0) as i32;
        for y in y0..=y1 {
            let dy = y as f32 + 0.5 - b.y;
            let row = y * self.w;
            let (f, cr, cg, cb) = (&mut self.f[row..row + self.w], &mut self.cr[row..row + self.w], &mut self.cg[row..row + self.w], &mut self.cb[row..row + self.w]);
            let (gr, gg, gb) = (&mut self.gr[row..row + self.w], &mut self.gg[row..row + self.w], &mut self.gb[row..row + self.w]);
            for x in x0..=x1 {
                let dx = x as f32 + 0.5 - b.x;
                let d2 = dx * dx + dy * dy;
                if d2 * inv_g2 >= 1.0 {
                    continue;
                }
                let inv = 1.0 / (d2 + 1e-4).sqrt();
                // Direction cosines against the lobe axis and the wobble axis.
                let u = (dx * pc + dy * ps) * inv;
                let v = (dx * wc + dy * ws) * inv;
                let (mut t0, mut t1) = (1.0f32, u);
                for _ in 1..n {
                    let t2 = 2.0 * u * t1 - t0;
                    t0 = t1;
                    t1 = t2;
                }
                let lobe = t1.max(0.0).powi(sharp);
                let wob = (2.0 * v * v - 1.0) * (4.0 * v * v * v - 3.0 * v);
                let rad = 1.0 + b.spikes * lobe + b.wobble * 0.22 * wob;
                let q = d2 * inv_r2 / (rad * rad);
                if q < 4.0 {
                    let t = 1.0 - q * 0.25;
                    let k = t * t * t;
                    f[x] += k;
                    cr[x] += k * b.rgb[0];
                    cg[x] += k * b.rgb[1];
                    cb[x] += k * b.rgb[2];
                }
                let t = 1.0 - d2 * inv_g2;
                let gk = t * t * b.glow;
                gr[x] += gk * b.rgb[0];
                gg[x] += gk * b.rgb[1];
                gb[x] += gk * b.rgb[2];
            }
        }
    }

    fn ring(&mut self, r: &Ring) {
        let out = r.radius + r.width;
        let (x0, x1) = ((r.x - out).floor().max(0.0) as usize, ((r.x + out).ceil() as usize).min(self.w - 1));
        let (y0, y1) = ((r.y - out).floor().max(0.0) as usize, ((r.y + out).ceil() as usize).min(self.h - 1));
        if x0 > x1 || y0 > y1 || r.alpha <= 0.0 {
            return;
        }
        let inv_w = 1.0 / r.width.max(0.5);
        for y in y0..=y1 {
            let dy = y as f32 + 0.5 - r.y;
            let row = y * self.w;
            for x in x0..=x1 {
                let dx = x as f32 + 0.5 - r.x;
                let d = (dx * dx + dy * dy).sqrt();
                let t = 1.0 - ((d - r.radius) * inv_w).abs();
                if t > 0.0 {
                    let k = t * t * r.alpha;
                    self.gr[row + x] += k * r.rgb[0];
                    self.gg[row + x] += k * r.rgb[1];
                    self.gb[row + x] += k * r.rgb[2];
                }
            }
        }
    }

    fn shade(&mut self, light: bool) {
        let (w, h) = (self.w, self.h);
        // Light from the top left, viewer straight on: half vector for the highlight.
        let (lx, ly, lz) = (-0.48f32, -0.62, 0.62);
        let ln = (lx * lx + ly * ly + lz * lz).sqrt();
        let (lx, ly, lz) = (lx / ln, ly / ln, lz / ln);
        let (hx, hy, hz) = (lx, ly, lz + 1.0);
        let hn = (hx * hx + hy * hy + hz * hz).sqrt();
        let (hx, hy, hz) = (hx / hn, hy / hn, hz / hn);
        let glow_gain = if light { 0.55 } else { 1.0 };
        let lut = &self.lut;
        let enc = |v: f32| lut[((v.clamp(0.0, 1.0)) * (LUT - 1) as f32) as usize];
        for y in 0..h {
            let (ya, yb) = (y.saturating_sub(1), (y + 1).min(h - 1));
            for x in 0..w {
                let i = y * w + x;
                let (xa, xb) = (x.saturating_sub(1), (x + 1).min(w - 1));
                let f = self.f[i];
                // Surface coverage, anti-aliased over a band of the field.
                let body = ((f - (ISO - 0.07)) / 0.14).clamp(0.0, 1.0);
                let body = body * body * (3.0 - 2.0 * body);
                let (mut r, mut g, mut b, mut a) = (0.0f32, 0.0, 0.0, 0.0);
                if body > 0.0 {
                    let inv = 1.0 / f.max(1e-4);
                    let (cr, cg, cb) = (self.cr[i] * inv, self.cg[i] * inv, self.cb[i] * inv);
                    let gx = self.f[y * w + xb] - self.f[y * w + xa];
                    let gy = self.f[yb * w + x] - self.f[ya * w + x];
                    let (nx, ny, nz) = (-gx * 1.6, -gy * 1.6, 1.0);
                    let nn = 1.0 / (nx * nx + ny * ny + nz * nz).sqrt();
                    let (nx, ny, nz) = (nx * nn, ny * nn, nz * nn);
                    let diff = (nx * lx + ny * ly + nz * lz).max(0.0);
                    let s = (nx * hx + ny * hy + nz * hz).max(0.0);
                    let s2 = s * s;
                    let s4 = s2 * s2;
                    let s16 = s4 * s4 * s4 * s4;
                    let spec = s16 * s16 * 0.85;
                    // Deeper inside the liquid is darker; the edge catches a rim light.
                    let depth = ((f - ISO) / 0.9).clamp(0.0, 1.0);
                    let rim = (1.0 - depth) * (1.0 - nz) * 0.9;
                    let shade = 0.42 + 0.7 * diff - 0.18 * depth;
                    r = (cr * shade + cr * rim * 0.6 + spec) * body;
                    g = (cg * shade + cg * rim * 0.6 + spec) * body;
                    b = (cb * shade + cb * rim * 0.6 + spec) * body;
                    a = body;
                }
                // Glow outside (and faintly through) the surface.
                let (gr, gg, gb) = (self.gr[i] * glow_gain, self.gg[i] * glow_gain, self.gb[i] * glow_gain);
                let ga = (gr.max(gg).max(gb) * 0.9).min(0.85) * (1.0 - a * 0.85);
                if ga > 0.0 {
                    let k = 1.0 - a * 0.85;
                    r += gr * k;
                    g += gg * k;
                    b += gb * k;
                    a += ga;
                }
                let o = i * 4;
                if a <= 1e-3 {
                    self.px[o..o + 4].copy_from_slice(&[0, 0, 0, 0]);
                    continue;
                }
                let a = a.min(1.0);
                // Straight colour → sRGB → premultiplied BGRA.
                let inv = 1.0 / a;
                let a8 = (a * 255.0 + 0.5) as u8;
                let pm = |c: f32| ((f32::from(enc(c * inv)) * a) + 0.5) as u8;
                self.px[o] = pm(b);
                self.px[o + 1] = pm(g);
                self.px[o + 2] = pm(r);
                self.px[o + 3] = a8;
            }
        }
    }
}

/// The grid size for a visual of `w` × `h` points at `scale` pixels per
/// point: about one cell every three device pixels, at most 220 wide.
pub fn grid_size(w: f32, h: f32, scale: f32) -> (usize, usize) {
    let target = (w * scale / 3.0).clamp(64.0, 220.0);
    let k = target / w.max(1.0);
    ((w * k).round().max(2.0) as usize, (h * k).round().max(2.0) as usize)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blob(x: f32, y: f32, r: f32) -> Blob {
        Blob { x, y, r, rgb: [0.9, 0.2, 0.1], lobes: 5, spikes: 0.0, sharp: 2.0, wobble: 0.0, phase: 0.0, glow: 0.0 }
    }

    fn alpha(px: &[u8], w: usize, x: usize, y: usize) -> u8 {
        px[(y * w + x) * 4 + 3]
    }

    #[test]
    fn a_cell_is_solid_inside_and_clear_outside() {
        let mut f = Field::default();
        let px = f.render(80, 60, &[blob(40.0, 30.0, 10.0)], &[], 0.0, false).to_vec();
        assert_eq!(alpha(&px, 80, 40, 30), 255, "the centre is solid");
        assert_eq!(alpha(&px, 80, 2, 2), 0, "far away is clear");
        // The edge sits near the radius.
        assert!(alpha(&px, 80, 40 + 7, 30) > 128);
        assert!(alpha(&px, 80, 40 + 13, 30) < 64);
        // Red dominates (BGRA order, premultiplied).
        let o = (30 * 80 + 40) * 4;
        assert!(px[o + 2] > px[o] && px[o + 2] > px[o + 1]);
    }

    #[test]
    fn two_near_cells_bridge_like_a_liquid() {
        let mut f = Field::default();
        // Each alone does not reach the midpoint...
        let one = f.render(100, 40, &[blob(40.0, 20.0, 9.0)], &[], 0.0, false).to_vec();
        assert_eq!(alpha(&one, 100, 50, 20), 0);
        // ...together their fields add up over the iso value there.
        let two = f.render(100, 40, &[blob(40.0, 20.0, 9.0), blob(60.0, 20.0, 9.0)], &[], 0.0, false).to_vec();
        assert!(alpha(&two, 100, 50, 20) > 0, "a bridge forms");
    }

    #[test]
    fn spikes_reach_further_along_their_lobes() {
        let mut f = Field::default();
        let mut spiky = blob(50.0, 50.0, 10.0);
        spiky.spikes = 0.8;
        spiky.lobes = 4;
        spiky.sharp = 4.0;
        let px = f.render(100, 100, &[spiky], &[], 0.0, false).to_vec();
        // A lobe points along +x (phase 0), the gap between lobes is diagonal.
        assert!(alpha(&px, 100, 50 + 15, 50) > 100, "along the lobe");
        assert!(alpha(&px, 100, 50 + 11, 50 + 11) < alpha(&px, 100, 50 + 15, 50));
    }

    #[test]
    fn rings_and_glow_light_up_the_background() {
        let mut f = Field::default();
        let ring = Ring { x: 50.0, y: 50.0, radius: 30.0, width: 3.0, alpha: 1.0, rgb: [0.2, 0.8, 1.0] };
        let px = f.render(100, 100, &[], &[ring], 0.0, false).to_vec();
        assert!(alpha(&px, 100, 80, 50) > 0 && alpha(&px, 100, 50, 50) == 0);
        let mut glowing = blob(50.0, 50.0, 6.0);
        glowing.glow = 1.0;
        let px = f.render(100, 100, &[glowing], &[], 0.0, false).to_vec();
        assert!(alpha(&px, 100, 50 + 10, 50) > 0, "a halo around the cell");
    }

    #[test]
    fn the_grid_follows_the_visual() {
        let (w, h) = grid_size(480.0, 240.0, 2.0);
        assert!((200..=220).contains(&w));
        assert_eq!(w / h, 2);
        assert!(grid_size(100.0, 100.0, 1.0).0 >= 64);
    }

    /// `cargo test -p aitrack-app --bin aitrack field_cost --release -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn field_cost() {
        let mut f = Field::default();
        let blobs: Vec<Blob> = (0..17)
            .map(|i| {
                let a = i as f32 * 0.39;
                Blob { x: 80.0 + a.cos() * 40.0, y: 60.0 + a.sin() * 34.0, r: 7.0 + (i % 3) as f32 * 2.0, rgb: [0.5, 0.4, 0.9], lobes: 3 + i % 6, spikes: 0.4, sharp: 3.0, wobble: 0.3, phase: a, glow: 0.6 }
            })
            .collect();
        let rings = [Ring { x: 80.0, y: 60.0, radius: 30.0, width: 3.0, alpha: 0.8, rgb: [1.0, 1.0, 1.0] }; 3];
        let t0 = std::time::Instant::now();
        for i in 0..200 {
            let _ = f.render(160, 120, &blobs, &rings, i as f32 / 60.0, false);
        }
        println!("field 160x120, 17 cells, 3 rings: {:.3} ms/frame", t0.elapsed().as_secs_f64() * 1000.0 / 200.0);
    }
}
