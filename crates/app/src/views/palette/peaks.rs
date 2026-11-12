//! Mini waveforms for the palette's result rows: a few dozen bars per file,
//! decoded on two background threads and kept in a small LRU. The
//! arrangement's [`crate::waveform::WaveformCache`] keeps full-resolution
//! peaks; a result list only needs an outline.

use crossbeam_channel::{Receiver, Sender, unbounded};
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Bars per mini waveform.
pub const BARS: usize = 48;
const WORKERS: usize = 2;
const KEEP: usize = 600;

/// Peak magnitude of `BARS` equal slices of the mono mix, normalised to 0..=1.
pub fn bars(planar: &[Vec<f32>], n: usize) -> Vec<f32> {
    let frames = planar.iter().map(Vec::len).max().unwrap_or(0);
    if frames == 0 || n == 0 {
        return Vec::new();
    }
    let mut out: Vec<f32> = (0..n)
        .map(|i| {
            let a = i * frames / n;
            let b = ((i + 1) * frames / n).max(a + 1).min(frames);
            planar.iter().flat_map(|ch| ch.get(a..b.min(ch.len())).unwrap_or(&[]).iter()).fold(0f32, |m, v| m.max(v.abs()))
        })
        .collect();
    let max = out.iter().copied().fold(0f32, f32::max);
    if max > 1e-6 {
        out.iter_mut().for_each(|v| *v /= max);
    }
    out
}

enum Slot {
    Loading,
    Ready(Arc<Vec<f32>>),
    Failed,
}

pub struct MiniPeaks {
    slots: HashMap<PathBuf, Slot>,
    order: VecDeque<PathBuf>,
    tx: Sender<PathBuf>,
    rx: Receiver<(PathBuf, Option<Vec<f32>>)>,
}

impl Default for MiniPeaks {
    fn default() -> Self {
        let (tx, jobs) = unbounded::<PathBuf>();
        let (done, rx) = unbounded();
        for i in 0..WORKERS {
            let (jobs, done): (Receiver<PathBuf>, Sender<_>) = (jobs.clone(), done.clone());
            let run = move || {
                while let Ok(path) = jobs.recv() {
                    let work = std::panic::AssertUnwindSafe(|| aitrack_engine::sample_cache::decode_file(&path).ok().map(|(p, _)| bars(&p, BARS)));
                    let result = std::panic::catch_unwind(work).ok().flatten();
                    if done.send((path, result)).is_err() {
                        break;
                    }
                }
            };
            let _ = std::thread::Builder::new().name(format!("palette-peaks-{i}")).spawn(run);
        }
        Self { slots: HashMap::new(), order: VecDeque::new(), tx, rx }
    }
}

impl MiniPeaks {
    /// The bars of `path` once decoded; the first call queues it.
    pub fn get(&mut self, path: &Path) -> Option<Arc<Vec<f32>>> {
        match self.slots.get(path) {
            Some(Slot::Ready(b)) => Some(b.clone()),
            Some(_) => None,
            None => {
                self.slots.insert(path.to_owned(), Slot::Loading);
                let _ = self.tx.send(path.to_owned());
                None
            }
        }
    }

    /// Take finished decodes; true when some arrived.
    pub fn poll(&mut self) -> bool {
        let mut any = false;
        while let Ok((path, bars)) = self.rx.try_recv() {
            any = true;
            self.order.push_back(path.clone());
            self.slots.insert(path, bars.map_or(Slot::Failed, |b| Slot::Ready(Arc::new(b))));
        }
        while self.order.len() > KEEP {
            if let Some(old) = self.order.pop_front() {
                self.slots.remove(&old);
            }
        }
        any
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bars_are_normalised_slice_peaks() {
        let l: Vec<f32> = (0..100).map(|i| if i < 50 { 0.25 } else { -0.5 }).collect();
        let b = bars(&[l], 4);
        assert_eq!(b, vec![0.5, 0.5, 1., 1.]);
        assert!(bars(&[], 4).is_empty());
        assert_eq!(bars(&[vec![0.; 10]], 2), vec![0., 0.]);
    }
}
