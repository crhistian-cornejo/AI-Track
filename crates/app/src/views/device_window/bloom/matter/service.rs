//! The app's matter render thread: one wgpu device for every Bloom visual,
//! created lazily on this thread (never the UI or audio thread). The UI
//! posts a job per frame (a few hundred bytes of uniforms) and picks up the
//! newest finished frame, one frame behind, so it never waits for the GPU.
//!
//! On macOS frames are NV12 IOSurfaces GPUI shows as surfaces (zero-copy);
//! elsewhere, or when that fails, they are read back as BGRA bytes the UI
//! uploads as an image. If no adapter is found the status says so and the
//! visual draws its CPU field instead.

use super::gpu::{Gpu, Target};
use super::uniforms::Uniforms;
use crossbeam_channel::{Receiver, Sender, unbounded};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

#[derive(Clone, Debug, PartialEq)]
pub enum Status {
    Starting,
    Ready { adapter: String, zero_copy: bool },
    Failed(String),
}

/// A finished frame's pixels.
pub enum Image {
    /// Premultiplied BGRA, tightly packed.
    Bgra(Vec<u8>),
    #[cfg(target_os = "macos")]
    Surface(super::surface::Frame),
}

pub struct Frame {
    /// The job's sequence number (frames of one panel only ever get newer).
    pub seq: u64,
    pub width: u32,
    pub height: u32,
    pub image: Image,
}

struct Job {
    key: u64,
    seq: u64,
    width: u32,
    height: u32,
    uniforms: Uniforms,
}

enum Msg {
    Render(Box<Job>),
    Forget(u64),
}

struct Shared {
    status: Mutex<Status>,
    frames: Mutex<HashMap<u64, Frame>>,
    /// Render-thread time per frame (GPU work included), ms, smoothed.
    cost: Mutex<f32>,
}

pub struct Service {
    tx: Sender<Msg>,
    shared: Arc<Shared>,
}

static SERVICE: OnceLock<Service> = OnceLock::new();

/// The service, started on first use.
pub fn service() -> &'static Service {
    SERVICE.get_or_init(Service::start)
}

/// Whether the GPU visual can draw (without starting the service).
pub fn ready() -> bool {
    SERVICE.get().is_some_and(|s| matches!(s.status(), Status::Ready { .. }))
}

impl Service {
    fn start() -> Service {
        let (tx, rx) = unbounded();
        let shared = Arc::new(Shared { status: Mutex::new(Status::Starting), frames: Mutex::new(HashMap::new()), cost: Mutex::new(0.0) });
        let s = shared.clone();
        let spawned = std::thread::Builder::new().name("bloom-matter".into()).spawn(move || run(rx, s));
        if let Err(e) = spawned {
            *shared.status.lock().unwrap_or_else(|p| p.into_inner()) = Status::Failed(e.to_string());
        }
        Service { tx, shared }
    }

    pub fn status(&self) -> Status {
        self.shared.status.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }

    /// Ask for a frame of panel `key` (any id unique to a panel in a window).
    pub fn submit(&self, key: u64, seq: u64, width: u32, height: u32, uniforms: Uniforms) {
        let _ = self.tx.send(Msg::Render(Box::new(Job { key, seq, width, height, uniforms })));
    }

    /// The newest finished frame of `key`, when newer than `seen`.
    pub fn take(&self, key: u64, seen: u64) -> Option<Frame> {
        let mut frames = self.shared.frames.lock().unwrap_or_else(|p| p.into_inner());
        if frames.get(&key).is_some_and(|f| f.seq > seen) { frames.remove(&key) } else { None }
    }

    /// Free a panel's buffers (it is no longer shown).
    pub fn forget(&self, key: u64) {
        let _ = self.tx.send(Msg::Forget(key));
    }

    /// Render-thread milliseconds per frame (moving average).
    pub fn cost_ms(&self) -> f32 {
        *self.shared.cost.lock().unwrap_or_else(|p| p.into_inner())
    }
}

/// One panel's buffers on the render thread.
struct Panel {
    target: Target,
    #[cfg(target_os = "macos")]
    ring: Option<super::surface::Ring>,
}

fn run(rx: Receiver<Msg>, shared: Arc<Shared>) {
    let gpu = match Gpu::new() {
        Ok(g) => g,
        Err(e) => {
            log::warn!("Bloom visual: {e}; drawing the CPU field");
            *shared.status.lock().unwrap_or_else(|p| p.into_inner()) = Status::Failed(e);
            return;
        }
    };
    gpu.device.on_uncaptured_error(Arc::new(|e: wgpu::Error| log::error!("Bloom visual: {e}")));
    #[cfg(target_os = "macos")]
    let zero_copy = match super::surface::Ring::new(&gpu, 16, 16) {
        Ok(_) => true,
        Err(e) => {
            log::warn!("Bloom visual: no IOSurface frames ({e}); reading frames back");
            false
        }
    };
    #[cfg(not(target_os = "macos"))]
    let zero_copy = false;
    log::info!("Bloom visual on {} ({:?}), zero-copy {zero_copy}", gpu.adapter, gpu.backend);
    *shared.status.lock().unwrap_or_else(|p| p.into_inner()) = Status::Ready { adapter: gpu.adapter.clone(), zero_copy };
    let mut panels: HashMap<u64, Panel> = HashMap::new();
    while let Ok(first) = rx.recv() {
        // Only the newest job of each panel matters.
        let mut jobs: HashMap<u64, Job> = HashMap::new();
        let handle = |m: Msg, jobs: &mut HashMap<u64, Job>, panels: &mut HashMap<u64, Panel>| match m {
            Msg::Render(j) => {
                jobs.insert(j.key, *j);
            }
            Msg::Forget(k) => {
                jobs.remove(&k);
                panels.remove(&k);
                shared.frames.lock().unwrap_or_else(|p| p.into_inner()).remove(&k);
            }
        };
        handle(first, &mut jobs, &mut panels);
        while let Ok(m) = rx.try_recv() {
            handle(m, &mut jobs, &mut panels);
        }
        for (key, job) in jobs {
            let t0 = Instant::now();
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| render(&gpu, &mut panels, &job, zero_copy)));
            match result {
                Ok(Ok(frame)) => {
                    shared.frames.lock().unwrap_or_else(|p| p.into_inner()).insert(key, frame);
                }
                Ok(Err(e)) => log::warn!("Bloom visual frame: {e}"),
                Err(_) => {
                    *shared.status.lock().unwrap_or_else(|p| p.into_inner()) = Status::Failed("the renderer stopped".into());
                    return;
                }
            }
            let ms = t0.elapsed().as_secs_f32() * 1000.0;
            let mut c = shared.cost.lock().unwrap_or_else(|p| p.into_inner());
            *c = if *c == 0.0 { ms } else { *c * 0.9 + ms * 0.1 };
        }
    }
}

fn render(gpu: &Gpu, panels: &mut HashMap<u64, Panel>, job: &Job, zero_copy: bool) -> Result<Frame, String> {
    #[cfg(target_os = "macos")]
    let (w, h) = if zero_copy { super::surface::Ring::size_for(job.width, job.height) } else { (job.width, job.height) };
    #[cfg(not(target_os = "macos"))]
    let (w, h) = (job.width, job.height);
    let fresh = panels.get(&job.key).is_none_or(|p| p.target.width != w || p.target.height != h);
    if fresh {
        let target = gpu.target(w, h);
        #[cfg(target_os = "macos")]
        let ring = if zero_copy { Some(super::surface::Ring::new(gpu, w, h)?) } else { None };
        panels.insert(job.key, Panel { target, #[cfg(target_os = "macos")] ring });
    }
    let p = panels.get_mut(&job.key).ok_or("no panel")?;
    let mut u = job.uniforms;
    // The uniforms were laid out for the job's size; the target may be a pixel smaller.
    u.res[0] = p.target.width as f32;
    u.res[1] = p.target.height as f32;
    #[cfg(target_os = "macos")]
    if let Some(ring) = &mut p.ring {
        let f = ring.render(gpu, &p.target, &u)?;
        return Ok(Frame { seq: job.seq, width: ring.width, height: ring.height, image: Image::Surface(f) });
    }
    let _ = zero_copy;
    let px = gpu.render_bgra(&p.target, &u)?;
    Ok(Frame { seq: job.seq, width: p.target.width, height: p.target.height, image: Image::Bgra(px) })
}
