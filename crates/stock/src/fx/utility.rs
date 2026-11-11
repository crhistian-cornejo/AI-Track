//! `fx.utility` — gain, pan, width, mono / bass-mono, polarity, swap and DC
//! blocking. Zero latency.
//!
//! Meter layout: `[peak L, peak R, correlation (−1..1)]`.

use crate::common::{ParamStore, Segmented, drive, for_frames, load_state, sanitize, save_state};
use aitrack_dsp::crossover::{Lr4, Lr4Coeffs};
use aitrack_dsp::smooth::Smoother;
use aitrack_dsp::svf::DcBlocker;
use aitrack_dsp::{Category, EventKind, ParamInfo, PluginInfo, ProcessContext, Processor, db_to_gain};

pub const ID: &str = "fx.utility";

pub mod p {
    pub const GAIN: u32 = 0;
    pub const PAN: u32 = 1;
    pub const WIDTH: u32 = 2;
    pub const MONO: u32 = 3;
    pub const BASS_MONO: u32 = 4;
    pub const BASS_FREQ: u32 = 5;
    pub const INVERT_L: u32 = 6;
    pub const INVERT_R: u32 = 7;
    pub const SWAP: u32 = 8;
    pub const DC_BLOCK: u32 = 9;
    pub const MODE: u32 = 10;
    pub const MUTE: u32 = 11;
}

const OFF_ON: &[&str] = &["Off", "On"];
const MODES: &[&str] = &["Stereo", "Left", "Right", "Mid", "Side"];

pub fn params() -> Vec<ParamInfo> {
    vec![
        ParamInfo::new(p::GAIN, "Gain", -36.0, 36.0, 0.0).unit("dB"),
        ParamInfo::new(p::PAN, "Pan", -100.0, 100.0, 0.0).unit("%"),
        ParamInfo::new(p::WIDTH, "Width", 0.0, 200.0, 100.0).unit("%"),
        ParamInfo::new(p::MONO, "Mono", 0.0, 1.0, 0.0).choices(OFF_ON),
        ParamInfo::new(p::BASS_MONO, "Bass Mono", 0.0, 1.0, 0.0).choices(OFF_ON).short("B.Mono"),
        ParamInfo::new(p::BASS_FREQ, "Bass Mono Freq", 20.0, 500.0, 120.0).unit("Hz").log().short("B.Freq"),
        ParamInfo::new(p::INVERT_L, "Invert Left", 0.0, 1.0, 0.0).choices(OFF_ON).short("Ø L"),
        ParamInfo::new(p::INVERT_R, "Invert Right", 0.0, 1.0, 0.0).choices(OFF_ON).short("Ø R"),
        ParamInfo::new(p::SWAP, "Swap L/R", 0.0, 1.0, 0.0).choices(OFF_ON).short("Swap"),
        ParamInfo::new(p::DC_BLOCK, "DC Block", 0.0, 1.0, 0.0).choices(OFF_ON).short("DC"),
        ParamInfo::new(p::MODE, "Channel Mode", 0.0, 4.0, 0.0).choices(MODES).short("Mode"),
        ParamInfo::new(p::MUTE, "Mute", 0.0, 1.0, 0.0).choices(OFF_ON),
    ]
}

pub struct Utility {
    info: PluginInfo,
    params: ParamStore,
    sr: f64,
    gain_l: Smoother,
    gain_r: Smoother,
    width: Smoother,
    xo: Lr4Coeffs,
    xo_l: Lr4,
    xo_r: Lr4,
    dc: [DcBlocker; 2],
    peak: [f32; 2],
    corr_num: f32,
    corr_den_l: f32,
    corr_den_r: f32,
    first: bool,
}

pub fn create() -> Box<dyn Processor> {
    Box::new(Utility::new())
}

impl Default for Utility {
    fn default() -> Self {
        Self::new()
    }
}

impl Utility {
    pub fn new() -> Self {
        let params = params();
        let mut u = Self {
            info: PluginInfo {
                id: ID,
                name: "Utility",
                vendor: "AI-Track",
                category: Category::Utility,
                tags: &["utility", "gain", "pan", "width", "mono"],
                params: params.clone(),
                inputs: 2,
                outputs: 2,
                has_sidechain: false,
            },
            params: ParamStore::new(&params),
            sr: 48_000.0,
            gain_l: Smoother::new(1.0),
            gain_r: Smoother::new(1.0),
            width: Smoother::new(1.0),
            xo: Lr4Coeffs::default(),
            xo_l: Lr4::default(),
            xo_r: Lr4::default(),
            dc: [DcBlocker::default(); 2],
            peak: [0.0; 2],
            corr_num: 0.0,
            corr_den_l: 0.0,
            corr_den_r: 0.0,
            first: true,
        };
        u.prepare(48_000.0, 512);
        u
    }

    fn update(&mut self) {
        let mute = self.params.on(p::MUTE);
        let g = if mute { 0.0 } else { db_to_gain(self.params.plain(p::GAIN)) };
        // Stereo balance: the far side is attenuated with an equal-power
        // curve, the near side stays at unity (centre is unity).
        let pan = self.params.plain(p::PAN) / 100.0;
        let (pl, pr) = if pan >= 0.0 {
            ((pan * std::f32::consts::FRAC_PI_2).cos(), 1.0)
        } else {
            (1.0, (pan * std::f32::consts::FRAC_PI_2).cos())
        };
        let k = 1.0;
        let il = if self.params.on(p::INVERT_L) { -1.0 } else { 1.0 };
        let ir = if self.params.on(p::INVERT_R) { -1.0 } else { 1.0 };
        self.gain_l.set_target(g * pl * k * il);
        self.gain_r.set_target(g * pr * k * ir);
        let w = if self.params.on(p::MONO) { 0.0 } else { self.params.plain(p::WIDTH) / 100.0 };
        self.width.set_target(w);
        self.xo = Lr4Coeffs::new(self.params.plain(p::BASS_FREQ), self.sr);
        if self.first {
            self.gain_l.snap_to_target();
            self.gain_r.snap_to_target();
            self.width.snap_to_target();
            self.first = false;
        }
    }
}

impl Segmented for Utility {
    fn event(&mut self, kind: &EventKind) {
        if let EventKind::Param { id, value } = *kind {
            self.params.set(id, value);
        }
    }

    fn render(&mut self, ctx: &mut ProcessContext<'_>, start: usize, end: usize) {
        if self.params.take_dirty() {
            self.update();
        }
        let swap = self.params.on(p::SWAP);
        let dc = self.params.on(p::DC_BLOCK);
        let bass_mono = self.params.on(p::BASS_MONO);
        let mode = self.params.choice(p::MODE);
        let mut peak = self.peak;
        let (mut num, mut dl, mut dr) = (self.corr_num, self.corr_den_l, self.corr_den_r);
        let decay = (-1.0 / (0.3 * self.sr as f32)).exp();
        for_frames(ctx.buffer, start, end, |_, mut l, mut r| {
            if swap {
                std::mem::swap(&mut l, &mut r);
            }
            if dc {
                l = self.dc[0].process(l);
                r = self.dc[1].process(r);
            }
            if bass_mono {
                let (ll, lh) = self.xo_l.split(&self.xo, l);
                let (rl, rh) = self.xo_r.split(&self.xo, r);
                let m = 0.5 * (ll + rl);
                l = m + lh;
                r = m + rh;
            }
            let w = self.width.next();
            let m = 0.5 * (l + r);
            let s = 0.5 * (l - r) * w;
            l = m + s;
            r = m - s;
            l *= self.gain_l.next();
            r *= self.gain_r.next();
            let (ol, or) = match mode {
                1 => (l, l),
                2 => (r, r),
                3 => {
                    let m = 0.5 * (l + r);
                    (m, m)
                }
                4 => {
                    let s = 0.5 * (l - r);
                    (s, s)
                }
                _ => (l, r),
            };
            peak[0] = (peak[0] * decay).max(ol.abs());
            peak[1] = (peak[1] * decay).max(or.abs());
            num = num * decay + ol * or;
            dl = dl * decay + ol * ol;
            dr = dr * decay + or * or;
            (sanitize(ol), sanitize(or))
        });
        self.peak = peak;
        self.corr_num = if num.is_finite() { num } else { 0.0 };
        self.corr_den_l = if dl.is_finite() { dl } else { 0.0 };
        self.corr_den_r = if dr.is_finite() { dr } else { 0.0 };
    }
}

impl Processor for Utility {
    fn info(&self) -> &PluginInfo {
        &self.info
    }

    fn prepare(&mut self, sample_rate: f64, _max_frames: usize) {
        self.sr = sample_rate;
        for s in [&mut self.gain_l, &mut self.gain_r, &mut self.width] {
            s.set_time(10.0, sample_rate);
        }
        self.dc = [DcBlocker::new(10.0, sample_rate); 2];
        self.first = true;
        self.params.mark_dirty();
        self.reset();
    }

    fn reset(&mut self) {
        self.xo_l.reset();
        self.xo_r.reset();
        for d in &mut self.dc {
            d.reset();
        }
        self.peak = [0.0; 2];
        self.corr_num = 0.0;
        self.corr_den_l = 0.0;
        self.corr_den_r = 0.0;
    }

    fn process(&mut self, ctx: &mut ProcessContext<'_>) {
        drive(self, ctx);
    }

    fn set_param(&mut self, id: u32, normalized: f32) {
        self.params.set(id, normalized);
    }

    fn get_param(&self, id: u32) -> f32 {
        self.params.get(id)
    }

    fn save_state(&self) -> Vec<u8> {
        save_state(&self.params, serde_json::Value::Null)
    }

    fn load_state(&mut self, data: &[u8]) {
        load_state(&mut self.params, data);
    }

    fn meter(&self, out: &mut [f32]) -> usize {
        let den = (self.corr_den_l * self.corr_den_r).sqrt();
        let corr = if den > 1e-12 { (self.corr_num / den).clamp(-1.0, 1.0) } else { 0.0 };
        crate::common::write_meter(out, 0, &[self.peak[0], self.peak[1], corr])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::{process_stereo, set_plain};

    #[test]
    fn unity_by_default() {
        let mut u = Utility::new();
        let (l, r) = process_stereo(&mut u, |i| ((i as f32 * 0.01).sin(), (i as f32 * 0.013).cos()), 512);
        for i in 0..512 {
            assert!((l[i] - (i as f32 * 0.01).sin()).abs() < 1e-5);
            assert!((r[i] - (i as f32 * 0.013).cos()).abs() < 1e-5);
        }
    }

    #[test]
    fn mono_and_invert() {
        let mut u = Utility::new();
        set_plain(&mut u, p::MONO, 1.0);
        set_plain(&mut u, p::INVERT_R, 1.0);
        let (l, r) = process_stereo(&mut u, |_| (1.0, 0.0), 4096);
        assert!((l[4000] - 0.5).abs() < 1e-3);
        assert!((r[4000] + 0.5).abs() < 1e-3);
    }

    #[test]
    fn gain_and_pan() {
        let mut u = Utility::new();
        set_plain(&mut u, p::GAIN, -6.0);
        set_plain(&mut u, p::PAN, 100.0);
        let (l, r) = process_stereo(&mut u, |_| (1.0, 1.0), 4096);
        assert!(l[4000].abs() < 1e-3);
        assert!((r[4000] - db_to_gain(-6.0)).abs() < 1e-3);
    }
}
