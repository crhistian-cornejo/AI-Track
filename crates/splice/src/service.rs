//! The Splice worker: searches, previews, covers, waveforms and sign-in on
//! background threads, answered through a channel the UI drains once per
//! tick. Nothing here blocks the caller.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use crossbeam_channel::{Receiver, Sender, unbounded};
use parking_lot_free::Shared;

use crate::auth::{AuthConfig, TokenStore};
use crate::client::Client;
use crate::model::{SearchPage, SpliceSample};
use crate::query::SearchParams;

/// What the worker reports back.
#[derive(Debug)]
pub enum Event {
    /// A search finished; `generation` is the one passed to [`Service::search`].
    Results { generation: u64, page: Result<SearchPage, String> },
    /// A preview is ready to play from this file.
    Preview { uuid: String, result: Result<PathBuf, String> },
    Cover { url: String, result: Result<PathBuf, String> },
    Waveform { uuid: String, bars: Vec<f32> },
    /// Open this sign-in page in the system browser.
    OpenUrl(String),
    SignedIn(Result<(), String>),
    SignedOut,
}

enum Asset {
    Cover(String),
    Waveform(String, String),
}

/// Bars of a waveform outline.
pub const WAVE_BARS: usize = 48;
const SIGN_IN_TIMEOUT: Duration = Duration::from_secs(300);

mod parking_lot_free {
    //! A tiny shared slot (std only).
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Default)]
    pub struct Shared<T>(Arc<Mutex<T>>);

    impl<T: Clone> Shared<T> {
        pub fn get(&self) -> T {
            self.0.lock().map(|g| g.clone()).unwrap_or_else(|e| e.into_inner().clone())
        }

        pub fn set(&self, v: T) {
            match self.0.lock() {
                Ok(mut g) => *g = v,
                Err(e) => *e.into_inner() = v,
            }
        }
    }
}

pub struct Service {
    search_tx: Sender<(u64, SearchParams)>,
    preview_tx: Sender<SpliceSample>,
    asset_tx: Sender<Asset>,
    events_tx: Sender<Event>,
    rx: Receiver<Event>,
    token: Shared<Option<String>>,
    store: Arc<dyn TokenStore>,
    /// Read from the store on the worker; `None` until then.
    signed_in: Shared<Option<bool>>,
}

impl Service {
    /// Start the worker threads. `cache` holds previews and covers.
    pub fn spawn(cache: PathBuf, store: Arc<dyn TokenStore>) -> Self {
        let (events_tx, rx) = unbounded();
        let (search_tx, search_rx) = unbounded::<(u64, SearchParams)>();
        let (preview_tx, preview_rx) = unbounded::<SpliceSample>();
        let (asset_tx, asset_rx) = unbounded::<Asset>();
        let token: Shared<Option<String>> = Shared::default();
        let signed_in: Shared<Option<bool>> = Shared::default();
        {
            let (tx, token, store, signed_in) = (events_tx.clone(), token.clone(), store.clone(), signed_in.clone());
            spawn("splice-search", move || {
                // The keychain read stays off the caller's thread.
                let saved = store.get();
                signed_in.set(Some(saved.is_some()));
                token.set(saved);
                let mut client = Client::new(token.get());
                while let Ok(mut job) = search_rx.recv() {
                    // Only the newest query matters.
                    while let Ok(newer) = search_rx.try_recv() {
                        job = newer;
                    }
                    client.set_token(token.get());
                    let page = client.search(&job.1).map_err(|e| e.to_string());
                    if tx.send(Event::Results { generation: job.0, page }).is_err() {
                        break;
                    }
                }
            });
        }
        {
            let (tx, cache) = (events_tx.clone(), cache.clone());
            spawn("splice-preview", move || {
                let client = Client::new(None);
                while let Ok(mut s) = preview_rx.recv() {
                    while let Ok(newer) = preview_rx.try_recv() {
                        s = newer;
                    }
                    let result = client.preview(&s, &cache).map_err(|e| e.to_string());
                    if tx.send(Event::Preview { uuid: s.uuid, result }).is_err() {
                        break;
                    }
                }
            });
        }
        {
            let tx = events_tx.clone();
            spawn("splice-assets", move || {
                let client = Client::new(None);
                while let Ok(job) = asset_rx.recv() {
                    let ev = match job {
                        Asset::Cover(url) => Event::Cover { result: client.cover(&url, &cache).map_err(|e| e.to_string()), url },
                        Asset::Waveform(uuid, url) => Event::Waveform { bars: client.waveform(&url, WAVE_BARS).unwrap_or_default(), uuid },
                    };
                    if tx.send(ev).is_err() {
                        break;
                    }
                }
            });
        }
        Self { search_tx, preview_tx, asset_tx, events_tx, rx, token, store, signed_in }
    }

    pub fn search(&self, generation: u64, params: SearchParams) {
        let _ = self.search_tx.send((generation, params));
    }

    pub fn preview(&self, sample: SpliceSample) {
        let _ = self.preview_tx.send(sample);
    }

    pub fn cover(&self, url: String) {
        let _ = self.asset_tx.send(Asset::Cover(url));
    }

    pub fn waveform(&self, uuid: String, url: String) {
        let _ = self.asset_tx.send(Asset::Waveform(uuid, url));
    }

    /// `None` while the stored token has not been read yet.
    pub fn signed_in(&self) -> Option<bool> {
        self.signed_in.get()
    }

    /// Start the browser sign-in ([`crate::auth::sign_in`]); answers with
    /// [`Event::OpenUrl`] then [`Event::SignedIn`].
    pub fn sign_in(&self) {
        let Some(cfg) = AuthConfig::from_env() else {
            let _ = self.events_tx.send(Event::SignedIn(Err(
                "Splice sign-in is not configured in this build (no OAuth client id). Search and previews work without it.".into(),
            )));
            return;
        };
        let (tx, store, token, signed_in) = (self.events_tx.clone(), self.store.clone(), self.token.clone(), self.signed_in.clone());
        spawn("splice-sign-in", move || {
            let open = |url: &str| {
                let _ = tx.send(Event::OpenUrl(url.to_owned()));
            };
            let result = crate::auth::sign_in(&cfg, store.as_ref(), open, SIGN_IN_TIMEOUT).map_err(|e| e.to_string());
            if result.is_ok() {
                token.set(store.get());
                signed_in.set(Some(true));
            }
            let _ = tx.send(Event::SignedIn(result));
        });
    }

    pub fn sign_out(&self) {
        let (tx, store, token, signed_in) = (self.events_tx.clone(), self.store.clone(), self.token.clone(), self.signed_in.clone());
        spawn("splice-sign-out", move || {
            let _ = store.clear();
            token.set(None);
            signed_in.set(Some(false));
            let _ = tx.send(Event::SignedOut);
        });
    }

    /// Everything that arrived since the last call.
    pub fn poll(&self) -> Vec<Event> {
        self.rx.try_iter().collect()
    }
}

fn spawn(name: &str, f: impl FnOnce() + Send + 'static) {
    if let Err(e) = std::thread::Builder::new().name(name.into()).spawn(f) {
        log::warn!("splice: cannot start {name}: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::MemoryStore;

    #[test]
    fn reads_the_stored_token_off_thread_and_signs_out() {
        let store = Arc::new(MemoryStore::default());
        store.set("t").unwrap();
        let s = Service::spawn(std::env::temp_dir().join("aitrack-splice-test"), store.clone());
        let t0 = std::time::Instant::now();
        while s.signed_in().is_none() && t0.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(s.signed_in(), Some(true));
        s.sign_out();
        let t0 = std::time::Instant::now();
        let mut out = false;
        while !out && t0.elapsed() < Duration::from_secs(5) {
            out = s.poll().iter().any(|e| matches!(e, Event::SignedOut));
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(out && store.get().is_none() && s.signed_in() == Some(false));
    }
}
