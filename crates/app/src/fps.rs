//! Frame-time measurement, off unless `AITRACK_FPS` is set (and then free of
//! charge for the rest of the app: one atomic load per window per frame).
//!
//! What is measured, per window: the CPU time of a frame from the start of the
//! root view's `render` to the end of the window's `paint` (so it includes
//! building the element tree, every nested view's render, layout, prepaint and
//! paint, but not the GPU submission that follows). A second series records the
//! gap between consecutive frames of the same window while it animates, which
//! is what a dropped frame looks like (a 60 Hz window that misses its slot
//! shows a gap of 33 ms).
//!
//! Usage: `AITRACK_FPS=1 aitrack`, use the app, quit; the summary goes to
//! stderr (and to `AITRACK_FPS_FILE` if set). Extras for scripted runs:
//! `AITRACK_FPS_QUIT=20` quits after 20 s, `AITRACK_FPS_SKIP=2` ignores the
//! first 2 s (default 2) so start-up work does not count.
//!
//! Wiring: a window's root view calls [`begin`] first thing in `render` and
//! wraps what it returns in [`probe`] (element first, so the window is only borrowed once the tree is built); `main` calls [`install`] once.

use gpui::{
    AnyElement, App, Bounds, Element, GlobalElementId, InspectorElementId, IntoElement, LayoutId, Pixels, Window, WindowId,
};
use std::{
    cell::RefCell,
    collections::HashMap,
    path::PathBuf,
    sync::OnceLock,
    time::{Duration, Instant},
};

/// One 60 Hz frame, in milliseconds.
pub const BUDGET_MS: f32 = 1000. / 60.;
/// A gap between two frames longer than this is a missed slot (1.5 frames).
const MISSED_GAP_MS: f32 = BUDGET_MS * 1.5;
/// A longer gap means the window was idle, not slow.
const IDLE_GAP: Duration = Duration::from_millis(100);
/// Samples kept per window and series (about 55 minutes at 60 fps).
const MAX_SAMPLES: usize = 200_000;

struct Config {
    on: bool,
    skip: Duration,
    quit_after: Option<Duration>,
    file: Option<PathBuf>,
    started: Instant,
}

fn config() -> &'static Config {
    static CONFIG: OnceLock<Config> = OnceLock::new();
    CONFIG.get_or_init(|| {
        let on = std::env::var("AITRACK_FPS").is_ok_and(|v| !v.is_empty() && v != "0");
        let secs = |name: &str| std::env::var(name).ok().and_then(|v| v.trim().parse::<f64>().ok()).filter(|s| *s >= 0.);
        Config {
            on,
            skip: Duration::from_secs_f64(secs("AITRACK_FPS_SKIP").unwrap_or(2.)),
            quit_after: secs("AITRACK_FPS_QUIT").map(Duration::from_secs_f64),
            file: std::env::var_os("AITRACK_FPS_FILE").map(PathBuf::from),
            started: Instant::now(),
        }
    })
}

/// Whether frame timing is on.
#[inline]
pub fn enabled() -> bool {
    config().on
}

/// Count, spread and tail of a series of millisecond samples.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Summary {
    pub n: usize,
    pub mean: f32,
    pub median: f32,
    pub p95: f32,
    pub p99: f32,
    pub max: f32,
    /// How many samples exceed `limit`.
    pub over: usize,
}

impl Summary {
    pub fn of(samples: &[f32], limit: f32) -> Summary {
        if samples.is_empty() {
            return Summary::default();
        }
        let mut sorted = samples.to_vec();
        sorted.sort_by(f32::total_cmp);
        let n = sorted.len();
        // Nearest rank: the smallest value with at least p of the samples at or below it.
        let rank = |p: f32| sorted[((p * n as f32).ceil() as usize).clamp(1, n) - 1];
        Summary {
            n,
            mean: sorted.iter().sum::<f32>() / n as f32,
            median: if n % 2 == 1 { sorted[n / 2] } else { (sorted[n / 2 - 1] + sorted[n / 2]) / 2. },
            p95: rank(0.95),
            p99: rank(0.99),
            max: sorted[n - 1],
            over: sorted.iter().filter(|v| **v > limit).count(),
        }
    }
}

struct Track {
    label: String,
    started: Option<(Instant, Option<Duration>)>,
    previous: Option<Instant>,
    work: Vec<f32>,
    /// The same frames in thread CPU time: unlike wall time it does not grow
    /// when the machine is busy with something else (a build, say).
    cpu: Vec<f32>,
    gaps: Vec<f32>,
}

/// CPU time used by the calling thread so far (macOS; elsewhere `None`).
#[cfg(target_os = "macos")]
pub(crate) fn thread_cpu() -> Option<Duration> {
    #[repr(C)]
    struct Timespec {
        sec: i64,
        nsec: i64,
    }
    unsafe extern "C" {
        fn clock_gettime(clock: u32, out: *mut Timespec) -> i32;
    }
    const CLOCK_THREAD_CPUTIME_ID: u32 = 16;
    let mut t = Timespec { sec: 0, nsec: 0 };
    // SAFETY: `t` is a valid out pointer for the whole call.
    (unsafe { clock_gettime(CLOCK_THREAD_CPUTIME_ID, &mut t) } == 0).then(|| Duration::new(t.sec as u64, t.nsec as u32))
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn thread_cpu() -> Option<Duration> {
    None
}

#[derive(Default)]
struct Registry {
    windows: HashMap<WindowId, Track>,
    warmed: bool,
}

thread_local! {
    static REGISTRY: RefCell<Registry> = RefCell::default();
}

/// Marks the start of a frame of `window`. Call it first in the root view's
/// `render`; `label` names the window in the summary (called once per window).
#[inline]
pub fn begin(window: &Window, label: impl FnOnce() -> String) {
    if !enabled() {
        return;
    }
    let now = Instant::now();
    let cfg = config();
    REGISTRY.with_borrow_mut(|reg| {
        if !reg.warmed && now.duration_since(cfg.started) >= cfg.skip {
            reg.warmed = true;
            // Start the engine-wide counters from here too.
            let _ = gpui::frame_stats::take();
        }
        let t = reg
            .windows
            .entry(window.window_handle().window_id())
            .or_insert_with(|| Track { label: label(), started: None, previous: None, work: Vec::new(), cpu: Vec::new(), gaps: Vec::new() });
        if reg.warmed
            && let Some(prev) = t.previous
        {
            let gap = now.duration_since(prev);
            if gap <= IDLE_GAP && t.gaps.len() < MAX_SAMPLES {
                t.gaps.push(gap.as_secs_f32() * 1000.);
            }
        }
        t.previous = Some(now);
        t.started = Some((now, thread_cpu()));
    });
}

fn finish(window: WindowId) {
    let (now, cpu_now) = (Instant::now(), thread_cpu());
    REGISTRY.with_borrow_mut(|reg| {
        let warmed = reg.warmed;
        if let Some(t) = reg.windows.get_mut(&window)
            && let Some((start, cpu_start)) = t.started.take()
            && warmed
            && t.work.len() < MAX_SAMPLES
        {
            t.work.push(now.duration_since(start).as_secs_f32() * 1000.);
            if let (Some(a), Some(b)) = (cpu_start, cpu_now) {
                t.cpu.push(b.saturating_sub(a).as_secs_f32() * 1000.);
            }
        }
    });
}

/// Wraps the root element of a window so the frame ends when it has painted.
/// Free when timing is off.
#[inline]
pub fn probe(child: impl IntoElement, window: &Window) -> AnyElement {
    if !enabled() {
        return child.into_any_element();
    }
    Probe { window: window.window_handle().window_id(), child: child.into_any_element() }.into_any_element()
}

struct Probe {
    window: WindowId,
    child: AnyElement,
}

impl IntoElement for Probe {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

impl Element for Probe {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<gpui::ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, window: &mut Window, cx: &mut App) -> (LayoutId, ()) {
        (self.child.request_layout(window, cx), ())
    }

    fn prepaint(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, _: Bounds<Pixels>, _: &mut (), window: &mut Window, cx: &mut App) {
        self.child.prepaint(window, cx);
    }

    fn paint(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, _: Bounds<Pixels>, _: &mut (), _: &mut (), window: &mut Window, cx: &mut App) {
        self.child.paint(window, cx);
        finish(self.window);
    }
}

/// The summary table as text (one block per window, the busiest first).
pub fn report() -> String {
    use std::fmt::Write;
    let mut rows: Vec<(String, Summary, Summary, Summary)> = REGISTRY.with_borrow(|reg| {
        reg.windows
            .values()
            .map(|t| (t.label.clone(), Summary::of(&t.work, BUDGET_MS), Summary::of(&t.cpu, BUDGET_MS), Summary::of(&t.gaps, MISSED_GAP_MS)))
            .collect()
    });
    rows.sort_by(|a, b| b.1.n.cmp(&a.1.n).then(a.0.cmp(&b.0)));
    let mut out = String::new();
    let _ = writeln!(out, "[fps] frame time = render + layout + paint of the window, ms (wall clock, then thread CPU time); budget {BUDGET_MS:.1} ms");
    let _ = writeln!(out, "[fps] {:<30} {:>6} {:>7} {:>7} {:>7} {:>7} {:>6} {:>7}   {:>7} {:>7}", "window", "frames", "median", "p95", "p99", "worst", ">16.7", "share", "gap p50", "missed");
    for (label, w, c, g) in rows {
        let share = |s: &Summary| if s.n > 0 { 100. * s.over as f32 / s.n as f32 } else { 0. };
        let _ = writeln!(out, "[fps] {:<30} {:>6} {:>7.2} {:>7.2} {:>7.2} {:>7.2} {:>6} {:>6.1}%   {:>7.2} {:>7}", label, w.n, w.median, w.p95, w.p99, w.max, w.over, share(&w), g.median, g.over);
        if c.n > 0 {
            let _ = writeln!(out, "[fps] {:<30} {:>6} {:>7.2} {:>7.2} {:>7.2} {:>7.2} {:>6} {:>6.1}%", "  cpu", c.n, c.median, c.p95, c.p99, c.max, c.over, share(&c));
        }
    }
    let s = gpui::frame_stats::take();
    let _ = writeln!(
        out,
        "[fps] gpui draws {} (layout {}, paint-only {}, full {}), views rendered {} / reused {}, mean draw {:.2} ms, worst draw {:.2} ms",
        s.frames,
        s.layout_frames,
        s.paint_frames,
        s.refresh_frames,
        s.views_rendered,
        s.views_reused,
        if s.frames > 0 { s.draw_time.as_secs_f32() * 1000. / s.frames as f32 } else { 0. },
        s.max_draw.as_secs_f32() * 1000.
    );
    out
}

fn emit() {
    let text = report();
    eprint!("{text}");
    if let Some(path) = &config().file {
        let _ = std::fs::write(path, text);
    }
}

/// Call once at start-up: turns the engine counters on, prints the summary when
/// the app quits and honours `AITRACK_FPS_QUIT`. Does nothing when timing is off.
pub fn install(cx: &mut App) {
    if !enabled() {
        return;
    }
    gpui::frame_stats::enable();
    cx.on_app_quit(|_| {
        emit();
        async {}
    })
    .detach();
    if let Some(after) = config().quit_after {
        cx.spawn(async move |cx| {
            cx.background_executor().timer(after).await;
            cx.update(|cx| cx.quit());
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn summary_of_nothing_is_zero() {
        assert_eq!(Summary::of(&[], BUDGET_MS), Summary::default());
    }

    #[test]
    fn summary_uses_nearest_rank_and_counts_the_slow_ones() {
        // 1..=100 ms: the median is 50.5, the 95th percentile the 95th value.
        let v: Vec<f32> = (1..=100).map(|i| i as f32).collect();
        let s = Summary::of(&v, 16.7);
        assert_eq!(s.n, 100);
        assert_eq!(s.median, 50.5);
        assert_eq!(s.p95, 95.);
        assert_eq!(s.p99, 99.);
        assert_eq!(s.max, 100.);
        assert_eq!(s.over, 84);
        assert!((s.mean - 50.5).abs() < 1e-4);
    }

    #[test]
    fn a_single_slow_frame_is_the_worst_case_not_the_p95() {
        let mut v = vec![4.0_f32; 99];
        v.push(40.);
        let s = Summary::of(&v, BUDGET_MS);
        assert_eq!((s.median, s.p95, s.max, s.over), (4., 4., 40., 1));
    }

    #[test]
    fn off_by_default() {
        // The test environment does not set AITRACK_FPS; timing must cost nothing.
        if std::env::var_os("AITRACK_FPS").is_none() {
            assert!(!enabled());
        }
    }

    /// Headless frame-time benchmark: draws the demo project in GPUI's test platform
    /// (real fonts, no GPU) at 60 Hz pace and prints the per-window frame times.
    ///
    /// ```text
    /// AITRACK_CONFIG_DIR=/tmp/x cargo test -p aitrack-app bench_frames -- --ignored --nocapture
    /// ```
    /// `AITRACK_BENCH_SECS` (default 3) sets the length of each scene and
    /// `AITRACK_BENCH_ONLY=granular` runs the scenes whose name contains that.
    #[test]
    #[ignore = "benchmark: run by hand"]
    fn bench_frames() {
        use gpui::{AppContext as _, TestAppContext, TestDispatcher, px, size};
        use std::sync::Arc;

        const DEVICES: &str = "instrument.granular+fx.stutter+fx.mbdyn+fx.compressor+instrument.wavetable+fx.analyzer+fx.eq";
        // (name, dev-view words). Device windows open on the Lead track.
        let mut scenes: Vec<(String, String)> = vec![
            ("main arrangement".into(), "play".into()),
            ("main mixer".into(), "play,mixer".into()),
            ("main mixer+meters".into(), "play,mixer,meters".into()),
            ("main piano roll".into(), "play,piano,track=Lead".into()),
            ("main device view".into(), "play,devices,track=Bass,big".into()),
            ("main session".into(), "play,session".into()),
        ];
        for d in DEVICES.split('+') {
            scenes.push((format!("window {d}"), format!("play,track=Lead,devwins={d}")));
        }
        scenes.push(("7 windows".into(), format!("play,track=Lead,devwins={DEVICES}")));
        scenes.push(("7 windows + mixer+meters".into(), format!("play,mixer,meters,track=Lead,devwins={DEVICES}")));
        // An audio clip (a 20 s stereo file made by the caller): `AITRACK_BENCH_WAV=/abs/x.wav`.
        if let Ok(wav) = std::env::var("AITRACK_BENCH_WAV") {
            scenes.push(("main audio clip".into(), format!("play,wav={wav},audio")));
            scenes.push(("main audio arrangement".into(), format!("play,wav={wav}")));
        }
        let secs: f64 = std::env::var("AITRACK_BENCH_SECS").ok().and_then(|v| v.parse().ok()).unwrap_or(3.);
        let only = std::env::var("AITRACK_BENCH_ONLY").unwrap_or_default();

        #[cfg(target_os = "macos")]
        let text_system: Arc<dyn gpui::PlatformTextSystem> = Arc::new(gpui_apple::AppleTextSystem::new());
        #[cfg(not(target_os = "macos"))]
        let text_system: Arc<dyn gpui::PlatformTextSystem> = Arc::new(gpui::NoopTextSystem);
        for (name, words) in scenes.iter().filter(|(n, _)| n.contains(&only)) {
            let mut app = TestAppContext::build_with_text_system(TestDispatcher::new(0), None, Arc::clone(&text_system));
            app.update(|cx| {
                crate::assets::load_fonts(cx);
                crate::actions::bind(cx);
                crate::keys::bind(cx);
            });
            let words = words.clone();
            let (_ws, cx) = app.add_window_view(move |_, cx| {
                let mut ws = crate::workspace::Workspace::new(crate::demo::project(), crate::bridge::Bridges::stubs(), cx);
                ws.apply_dev_words(&words, cx);
                ws
            });
            cx.simulate_resize(size(px(1480.), px(920.)));
            let main = cx.update(|_, cx| cx.windows())[0];
            let draw = |cx: &mut gpui::VisualTestContext, window: gpui::AnyWindowHandle| -> (f32, f32) {
                let (t0, c0) = (Instant::now(), thread_cpu());
                cx.update_window(window, |_, window, cx| window.draw(cx).clear(cx)).unwrap();
                let (t1, c1) = (Instant::now(), thread_cpu());
                ((t1 - t0).as_secs_f32() * 1000., c1.zip(c0).map_or(0., |(b, a)| (b - a).as_secs_f32() * 1000.))
            };
            // Let the device windows open, then warm up for a second.
            for _ in 0..30 {
                cx.run_until_parked();
                draw(cx, main);
                std::thread::sleep(Duration::from_millis(16));
            }
            let mut series: HashMap<String, (Vec<f32>, Vec<f32>)> = HashMap::new();
            let end = Instant::now() + Duration::from_secs_f64(secs);
            let mut next = Instant::now();
            while Instant::now() < end {
                cx.run_until_parked();
                let windows = cx.update(|_, cx| cx.windows());
                let (mut all_wall, mut all_cpu) = (0., 0.);
                for (i, w) in windows.iter().enumerate() {
                    let label = if *w == main { "main".to_owned() } else { format!("window {i}") };
                    let (wall, cpu) = draw(cx, *w);
                    let s = series.entry(label).or_default();
                    s.0.push(wall);
                    s.1.push(cpu);
                    all_wall += wall;
                    all_cpu += cpu;
                }
                // Every window shares the one UI thread: what a display tick costs in total.
                if windows.len() > 1 {
                    let s = series.entry("~all".to_owned()).or_default();
                    s.0.push(all_wall);
                    s.1.push(all_cpu);
                }
                next += Duration::from_micros(16_667);
                if let Some(rest) = next.checked_duration_since(Instant::now()) {
                    std::thread::sleep(rest);
                } else {
                    next = Instant::now();
                }
            }
            let mut labels: Vec<_> = series.keys().cloned().collect();
            labels.sort();
            for label in labels {
                let (wall, cpu) = &series[&label];
                let (w, c) = (Summary::of(wall, BUDGET_MS), Summary::of(cpu, BUDGET_MS));
                eprintln!("[bench] {name:<24} {label:<10} n {:>4}  wall med {:>6.2} p95 {:>6.2} max {:>6.2} over {:>3}   cpu med {:>6.2} p95 {:>6.2}", w.n, w.median, w.p95, w.max, w.over, c.median, c.p95);
            }
        }
    }
}
