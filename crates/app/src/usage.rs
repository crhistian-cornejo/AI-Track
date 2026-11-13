//! How much of the machine AI-Track itself is using: process CPU and memory,
//! sampled about once a second for the status bar. The audio engine's own
//! load (time spent in the callback) is reported separately by the engine.

use std::time::{Duration, Instant};
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};

const EVERY: Duration = Duration::from_millis(1000);

pub struct Usage {
    sys: Option<System>,
    pid: Pid,
    cores: f32,
    at: Instant,
    /// Share of the whole machine's CPU, 0..=1.
    pub cpu: f32,
    /// Resident memory in bytes.
    pub memory: u64,
}

impl Default for Usage {
    fn default() -> Self {
        let cores = std::thread::available_parallelism().map_or(1, |n| n.get()) as f32;
        let mut this = Self {
            // Unavailable on a platform without process information: read as zero.
            sys: System::new().ok(),
            pid: Pid::from_u32(std::process::id()),
            cores,
            at: Instant::now() - EVERY,
            cpu: 0.,
            memory: 0,
        };
        this.sample();
        this
    }
}

impl Usage {
    /// Refresh if the last sample is older than the sampling interval (1s, or 3s in low-power mode).
    pub fn tick(&mut self, low_power: bool) {
        let interval = if low_power { EVERY * 3 } else { EVERY };
        if self.at.elapsed() >= interval {
            self.sample();
        }
    }

    fn sample(&mut self) {
        self.at = Instant::now();
        let Some(sys) = &mut self.sys else { return };
        sys.refresh_processes_specifics(ProcessesToUpdate::Some(&[self.pid]), false, ProcessRefreshKind::nothing().with_cpu().with_memory());
        if let Some(p) = sys.process(self.pid) {
            // sysinfo reports percent of one core; show the share of the machine.
            self.cpu = (p.cpu_usage() / 100. / self.cores).clamp(0., 1.);
            self.memory = p.memory();
        }
    }

    /// `"412 MB"` or `"1.3 GB"`.
    pub fn memory_label(&self) -> String {
        format_bytes(self.memory)
    }
}

pub fn format_bytes(bytes: u64) -> String {
    let mb = bytes as f64 / (1024. * 1024.);
    if mb >= 1024. { format!("{:.1} GB", mb / 1024.) } else { format!("{mb:.0} MB") }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_this_process() {
        let u = Usage::default();
        assert!(u.memory > 1024 * 1024, "a running process uses more than 1 MB");
        assert!((0. ..=1.).contains(&u.cpu));
    }

    #[test]
    fn byte_labels() {
        assert_eq!(format_bytes(106 * 1024 * 1024), "106 MB");
        assert_eq!(format_bytes(1536 * 1024 * 1024), "1.5 GB");
    }
}
