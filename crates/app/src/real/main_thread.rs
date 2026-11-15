//! Work that has to run on the UI (main) thread.
//!
//! Hosted VST3/CLAP plugins bind their main-thread half to the thread that
//! loads them, and their editor windows need that thread's message loop.
//! The engine asks for processors on its control thread, so the hosted
//! plugin factory sends the load (and later `prepare`, state and drop) here
//! and waits; the UI drains the queue from its housekeeping timer.

use crossbeam_channel::{Receiver, Sender, bounded, unbounded};
use std::thread::ThreadId;
use std::time::Duration;

type Job = Box<dyn FnOnce() + Send>;

#[derive(Clone)]
pub struct MainThread {
    tx: Sender<Job>,
    rx: Receiver<Job>,
    main: ThreadId,
}

impl MainThread {
    /// Create on the main thread: the calling thread becomes "main".
    pub fn new() -> Self {
        let (tx, rx) = unbounded();
        Self { tx, rx, main: std::thread::current().id() }
    }

    pub fn is_main(&self) -> bool {
        std::thread::current().id() == self.main
    }

    /// Run `f` on the main thread without waiting (inline when already there).
    pub fn post(&self, f: impl FnOnce() + Send + 'static) {
        if self.is_main() {
            f();
        } else {
            let _ = self.tx.send(Box::new(f));
        }
    }

    /// Run `f` on the main thread and wait up to `timeout` for its result.
    /// `None` when the main thread did not get to it in time (the job still
    /// runs later; its result is dropped there).
    pub fn run<R: Send + 'static>(&self, timeout: Duration, f: impl FnOnce() -> R + Send + 'static) -> Option<R> {
        if self.is_main() {
            return Some(f());
        }
        let (tx, rx) = bounded(1);
        self.tx
            .send(Box::new(move || {
                let _ = tx.send(f());
            }))
            .ok()?;
        rx.recv_timeout(timeout).ok()
    }

    /// Like [`run`](Self::run), but a result that is late is not lost: on a
    /// timeout the receiver comes back, and the result arrives on it once
    /// the main thread gets to the job. The caller can go on meanwhile.
    pub fn run_or_later<R: Send + 'static>(&self, timeout: Duration, f: impl FnOnce() -> R + Send + 'static) -> Result<R, Receiver<R>> {
        if self.is_main() {
            return Ok(f());
        }
        let (tx, rx) = bounded(1);
        let sent = self.tx.send(Box::new(move || {
            let _ = tx.send(f());
        }));
        if sent.is_err() {
            return Err(rx);
        }
        rx.recv_timeout(timeout).map_err(|_| rx)
    }

    /// Main thread: run everything queued. Returns how many jobs ran.
    pub fn pump(&self) -> usize {
        debug_assert!(self.is_main());
        let mut n = 0;
        while let Ok(job) = self.rx.try_recv() {
            job();
            n += 1;
        }
        n
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runs_inline_on_main_and_queued_elsewhere() {
        let m = MainThread::new();
        assert_eq!(m.run(Duration::from_secs(1), || 7), Some(7));
        let other = m.clone();
        let t = std::thread::spawn(move || other.run(Duration::from_secs(5), || std::thread::current().id()));
        // Pump until the worker's job arrives.
        let start = std::time::Instant::now();
        while m.pump() == 0 && start.elapsed() < Duration::from_secs(5) {
            std::thread::yield_now();
        }
        assert_eq!(t.join().unwrap(), Some(std::thread::current().id()));
    }

    #[test]
    fn a_late_result_is_kept_for_the_caller() {
        let m = MainThread::new();
        let other = m.clone();
        let t = std::thread::spawn(move || other.run_or_later(Duration::from_millis(20), || 5));
        // Nobody pumps in time: the caller gets the receiver instead.
        let rx = t.join().unwrap().expect_err("the main thread was busy");
        assert!(rx.try_recv().is_err());
        assert_eq!(m.pump(), 1);
        assert_eq!(rx.try_recv(), Ok(5));
        // On the main thread itself it just runs.
        assert_eq!(m.run_or_later(Duration::from_secs(1), || 9).ok(), Some(9));
    }
}
