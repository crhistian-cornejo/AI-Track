//! Search by meaning in the palette: the state of the CLAP model (checked, downloaded and
//! removed off the UI thread) shown in Preferences › Sounds and used by the search chip.
//!
//! With `AITRACK_STUBS` nothing is downloaded: the model is "installed" after a short fake
//! download, so the interface can be seen and screenshotted. `AITRACK_DEV_VIEW=semantic` opens the
//! palette on a semantic query; `semantic-download` shows the Preferences row mid-download.

use aitrack_ai::clap;
use aitrack_ai::models::ids;
use aitrack_ai::{ModelManager, progress::CancelToken};
use crossbeam_channel::{Receiver, Sender, unbounded};
use std::time::{Duration, Instant};

/// A word of `AITRACK_DEV_VIEW`.
pub fn dev_word(word: &str) -> bool {
    std::env::var("AITRACK_DEV_VIEW").is_ok_and(|v| v.split([',', ' ']).any(|w| w == word))
}

pub fn stubs() -> bool {
    std::env::var_os("AITRACK_STUBS").is_some()
}

#[derive(Clone, Debug, PartialEq)]
pub enum ModelState {
    /// Looking at the disk.
    Checking,
    Missing,
    Downloading { fraction: f32, message: String },
    Ready,
    Failed(String),
}

/// What Preferences shows before the download starts.
#[derive(Clone, Debug, PartialEq)]
pub struct ModelInfo {
    pub name: String,
    pub size: Option<u64>,
    pub license: String,
    pub license_url: String,
    pub source: String,
    pub notice: String,
}

enum Msg {
    State(ModelState),
    Done,
}

pub struct SemanticUi {
    pub state: ModelState,
    pub info: ModelInfo,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    cancel: CancelToken,
    /// Nothing is read or downloaded (stub bridges, tests).
    fake: bool,
}

fn info() -> ModelInfo {
    let manager = ModelManager::with_dir(std::env::temp_dir());
    match manager.entry(ids::CLAP_GENERAL) {
        Ok(e) => ModelInfo {
            name: e.name.clone(),
            size: e.download_size(),
            license: e.license.clone(),
            license_url: e.license_url.clone(),
            source: e.source.clone(),
            notice: e.notice.clone().unwrap_or_default(),
        },
        Err(_) => ModelInfo { name: "CLAP".into(), size: None, license: "Apache-2.0".into(), license_url: String::new(), source: String::new(), notice: String::new() },
    }
}

fn manager() -> ModelManager {
    ModelManager::new().unwrap_or_else(|_| ModelManager::with_dir(std::env::temp_dir().join("AI-Track").join("models")))
}

impl SemanticUi {
    pub fn new() -> Self {
        Self::with_mode(stubs())
    }

    pub fn with_mode(fake: bool) -> Self {
        let (tx, rx) = unbounded();
        let mut ui = Self { state: ModelState::Checking, info: info(), tx, rx, cancel: CancelToken::new(), fake };
        if dev_word("semantic-download") {
            ui.state = ModelState::Downloading { fraction: 0.42, message: "text_model_quantized.onnx 53.2 MB".into() };
        } else if fake {
            ui.state = if dev_word("semantic") { ModelState::Ready } else { ModelState::Missing };
        } else {
            let tx = ui.tx.clone();
            std::thread::spawn(move || {
                let ready = clap::is_ready(&manager());
                let _ = tx.send(Msg::State(if ready { ModelState::Ready } else { ModelState::Missing }));
            });
        }
        ui
    }

    pub fn ready(&self) -> bool {
        self.state == ModelState::Ready
    }

    pub fn downloading(&self) -> bool {
        matches!(self.state, ModelState::Downloading { .. })
    }

    /// Starts the download (the model, and ONNX Runtime when it is missing).
    pub fn download(&mut self) {
        if self.downloading() {
            return;
        }
        self.cancel = CancelToken::new();
        self.state = ModelState::Downloading { fraction: 0.0, message: "Starting…".into() };
        let (tx, cancel) = (self.tx.clone(), self.cancel.clone());
        if self.fake {
            std::thread::spawn(move || {
                let t0 = Instant::now();
                while t0.elapsed() < Duration::from_secs(3) {
                    if cancel.is_cancelled() {
                        let _ = tx.send(Msg::State(ModelState::Missing));
                        return;
                    }
                    let f = t0.elapsed().as_secs_f32() / 3.0;
                    let _ = tx.send(Msg::State(ModelState::Downloading { fraction: f, message: format!("{:.0} MB", f * 205.0) }));
                    std::thread::sleep(Duration::from_millis(100));
                }
                let _ = tx.send(Msg::State(ModelState::Ready));
                let _ = tx.send(Msg::Done);
            });
            return;
        }
        std::thread::spawn(move || {
            let mut last = Instant::now() - Duration::from_secs(1);
            let r = clap::install(
                &manager(),
                &mut |p| {
                    if last.elapsed() >= Duration::from_millis(100) {
                        last = Instant::now();
                        let _ = tx.send(Msg::State(ModelState::Downloading { fraction: p.fraction, message: p.message }));
                    }
                },
                &cancel,
            );
            match r {
                Ok(()) => {
                    let _ = tx.send(Msg::State(ModelState::Ready));
                    let _ = tx.send(Msg::Done);
                }
                Err(aitrack_ai::Error::Cancelled) => {
                    let _ = tx.send(Msg::State(ModelState::Missing));
                }
                Err(e) => {
                    log::warn!("CLAP download: {e}");
                    let _ = tx.send(Msg::State(ModelState::Failed(e.to_string())));
                }
            }
        });
    }

    pub fn cancel(&mut self) {
        self.cancel.cancel();
    }

    /// Takes the worker's news. `.0`: something changed (repaint); `.1`: the model just became ready.
    pub fn poll(&mut self) -> (bool, bool) {
        let (mut changed, mut done) = (false, false);
        while let Ok(m) = self.rx.try_recv() {
            changed = true;
            match m {
                Msg::State(s) => self.state = s,
                Msg::Done => done = true,
            }
        }
        (changed, done)
    }
}

impl Default for SemanticUi {
    fn default() -> Self {
        Self::new()
    }
}

/// What Preferences draws: the state and the facts, copied so the page can borrow the workspace.
#[derive(Clone, Debug)]
pub struct ModelView {
    pub state: ModelState,
    pub info: ModelInfo,
}

impl SemanticUi {
    pub fn clone_view(&self) -> ModelView {
        ModelView { state: self.state.clone(), info: self.info.clone() }
    }
}

/// "205 MB".
pub fn mb(bytes: Option<u64>) -> String {
    bytes.map_or_else(|| "size unknown".into(), |b| format!("{:.0} MB", b as f64 / 1e6))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_info_shows_license_and_size_before_any_download() {
        let i = info();
        assert_eq!(i.license, "Apache-2.0");
        assert_eq!(mb(i.size), "206 MB");
        assert!(i.notice.contains("Apache-2.0") && !i.source.is_empty());
        assert_eq!(mb(None), "size unknown");
    }

    #[test]
    fn a_stub_download_goes_missing_downloading_ready() {
        let mut ui = SemanticUi::with_mode(true);
        assert_eq!(ui.state, ModelState::Missing);
        ui.download();
        assert!(ui.downloading());
        let t0 = Instant::now();
        let mut done = false;
        while !done && t0.elapsed() < Duration::from_secs(10) {
            done = ui.poll().1;
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(done && ui.ready());
    }
}
