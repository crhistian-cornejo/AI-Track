//! The scene's small ambience: stereo early reflections (six taps a side)
//! into a pair of damped feedback combs. Every voice sends to it by its
//! Space gene; the Space macro sets how loud it comes back. Buffers are
//! allocated in `prepare` only.

const TAPS_L: [f32; 6] = [7.1, 11.3, 17.9, 23.7, 31.1, 41.3];
const TAPS_R: [f32; 6] = [8.3, 13.1, 19.7, 26.9, 35.3, 44.9];
const COMB_L: f32 = 47.3;
const COMB_R: f32 = 53.9;

pub struct Space {
    buf_l: Vec<f32>,
    buf_r: Vec<f32>,
    comb_l: Vec<f32>,
    comb_r: Vec<f32>,
    pos: usize,
    cpos_l: usize,
    cpos_r: usize,
    taps_l: [usize; 6],
    taps_r: [usize; 6],
    lp_l: f32,
    lp_r: f32,
    pub level: f32,
}

impl Space {
    pub fn new() -> Self {
        let mut s = Self {
            buf_l: Vec::new(),
            buf_r: Vec::new(),
            comb_l: Vec::new(),
            comb_r: Vec::new(),
            pos: 0,
            cpos_l: 0,
            cpos_r: 0,
            taps_l: [1; 6],
            taps_r: [1; 6],
            lp_l: 0.0,
            lp_r: 0.0,
            level: 0.0,
        };
        s.prepare(48_000.0);
        s
    }

    pub fn prepare(&mut self, sr: f32) {
        let ms = |t: f32| ((t * 0.001 * sr) as usize).max(1);
        let len = ms(50.0) + 2;
        self.buf_l = vec![0.0; len];
        self.buf_r = vec![0.0; len];
        self.comb_l = vec![0.0; ms(COMB_L)];
        self.comb_r = vec![0.0; ms(COMB_R)];
        self.taps_l = TAPS_L.map(ms);
        self.taps_r = TAPS_R.map(ms);
        self.reset();
    }

    pub fn reset(&mut self) {
        self.buf_l.fill(0.0);
        self.buf_r.fill(0.0);
        self.comb_l.fill(0.0);
        self.comb_r.fill(0.0);
        self.pos = 0;
        self.cpos_l = 0;
        self.cpos_r = 0;
        self.lp_l = 0.0;
        self.lp_r = 0.0;
        self.level = 0.0;
    }

    /// Mix the reflections of `send` into `l` / `r` at `amount` (0..1).
    pub fn process(&mut self, send: &[f32], l: &mut [f32], r: &mut [f32], amount: f32) {
        let len = self.buf_l.len();
        if len < 2 || self.comb_l.is_empty() || self.comb_r.is_empty() {
            return;
        }
        let mut peak = 0f32;
        for i in 0..send.len().min(l.len()).min(r.len()) {
            let x = send[i];
            self.buf_l[self.pos] = x;
            self.buf_r[self.pos] = x;
            let (mut el, mut er) = (0.0, 0.0);
            let mut gain = 0.55;
            for k in 0..6 {
                el += self.buf_l[(self.pos + len - self.taps_l[k]) % len] * gain;
                er += self.buf_r[(self.pos + len - self.taps_r[k]) % len] * gain;
                gain *= 0.8;
            }
            // Damped combs for a short tail.
            let cl = self.comb_l[self.cpos_l];
            let cr = self.comb_r[self.cpos_r];
            self.lp_l += 0.45 * (cl - self.lp_l);
            self.lp_r += 0.45 * (cr - self.lp_r);
            self.comb_l[self.cpos_l] = el * 0.5 + self.lp_r * 0.5;
            self.comb_r[self.cpos_r] = er * 0.5 + self.lp_l * 0.5;
            self.cpos_l = (self.cpos_l + 1) % self.comb_l.len();
            self.cpos_r = (self.cpos_r + 1) % self.comb_r.len();
            let (ol, or) = ((el + cl) * amount, (er + cr) * amount);
            l[i] += ol;
            r[i] += or;
            peak = peak.max(ol.abs()).max(or.abs());
            self.pos = (self.pos + 1) % len;
        }
        if self.lp_l.abs() < 1e-20 {
            self.lp_l = 0.0;
        }
        if self.lp_r.abs() < 1e-20 {
            self.lp_r = 0.0;
        }
        self.level = self.level * 0.8 + peak * 0.2;
    }
}

impl Default for Space {
    fn default() -> Self {
        Self::new()
    }
}
