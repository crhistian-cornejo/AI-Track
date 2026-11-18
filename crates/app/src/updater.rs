//! The updater's state in the app. `aitrack-update` does the work (manifest,
//! download, verification, install); this drives it from background threads so
//! the UI thread never waits on the network or the disk, and keeps the settings.
//!
//! Checks run 30 s after start and every 6 h, plus on demand. A newer version is
//! downloaded in the background (not while recording or exporting) and installed
//! when the user presses Restart or, with "install automatically", on quit.

use aitrack_update::{Update, UpdateError};
use crossbeam_channel::{Receiver, Sender, unbounded};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// First automatic check after start-up, and the gap between the next ones.
pub const FIRST_CHECK: Duration = Duration::from_secs(30);
pub const CHECK_EVERY: Duration = Duration::from_secs(6 * 3600);

fn yes() -> bool {
    true
}

/// The user's choices, in `<config>/updates.json`. Missing file or field = default.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct UpdatePrefs {
    #[serde(default = "yes")]
    pub check_automatically: bool,
    #[serde(default = "yes")]
    pub install_automatically: bool,
    /// Unix seconds of the last check that reached the server.
    #[serde(default)]
    pub last_check: Option<u64>,
}

impl Default for UpdatePrefs {
    fn default() -> Self {
        Self { check_automatically: true, install_automatically: true, last_check: None }
    }
}

fn prefs_path() -> PathBuf {
    crate::appearance::config_dir().join("updates.json")
}

impl UpdatePrefs {
    pub fn load() -> Self {
        std::fs::read_to_string(prefs_path()).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
    }

    /// Written on a background thread; a failure only goes to the log.
    pub fn save_in_background(&self) {
        let Ok(json) = serde_json::to_string_pretty(self) else { return };
        std::thread::spawn(move || {
            let path = prefs_path();
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            if let Err(e) = std::fs::write(&path, json) {
                log::warn!("could not save {}: {e}", path.display());
            }
        });
    }
}

/// A version on offer.
#[derive(Clone, Debug, PartialEq)]
pub struct Offer {
    pub version: String,
    pub notes: String,
}

#[derive(Clone, Debug, PartialEq)]
pub enum State {
    Idle,
    Checking,
    UpToDate,
    Available(Offer),
    Downloading { offer: Offer, done: u64, total: u64 },
    Ready(Offer),
    Failed(String),
    Installing,
}

/// What the quit handler needs, shared with it because it runs outside the
/// workspace: the verified download to install when the app closes, and
/// whether something (recording, exporting) forbids that.
pub struct QuitHook {
    ready: Mutex<Option<PathBuf>>,
    busy: AtomicBool,
}

static QUIT_HOOK: QuitHook = QuitHook { ready: Mutex::new(None), busy: AtomicBool::new(false) };

/// Called as the app quits: installs a ready update (no relaunch: the user is
/// closing the app) when "install automatically" is on and nothing is busy.
pub fn install_on_quit() {
    if QUIT_HOOK.busy.load(Ordering::Relaxed) {
        return;
    }
    let Some(path) = QUIT_HOOK.ready.lock().ok().and_then(|mut r| r.take()) else { return };
    let Ok(exe) = std::env::current_exe() else { return };
    match aitrack_update::install_downloaded(&path, &exe, &aitrack_update::SystemLauncher, false) {
        Ok(()) => log::info!("installed the update on quit"),
        Err(e) => log::warn!("installing the update on quit failed: {e}"),
    }
}

enum Event {
    Checked { manual: bool, result: Result<Option<Update>, UpdateError> },
    Progress { done: u64, total: u64 },
    Downloaded(Result<PathBuf, UpdateError>),
    Installed(Result<(), UpdateError>),
}

/// How the updater behaves in this process.
#[derive(Clone, Debug)]
pub struct Config {
    /// `AITRACK_UPDATE=0` turns the updater off entirely.
    pub enabled: bool,
    /// Whether it may check by itself.
    pub auto: bool,
    pub url: String,
    pub root: PathBuf,
    pub current: &'static str,
}

impl Config {
    pub fn from_env() -> Self {
        let enabled = std::env::var("AITRACK_UPDATE").map_or(true, |v| v.trim() != "0");
        let custom_url = std::env::var("AITRACK_UPDATE_URL").ok().filter(|u| !u.trim().is_empty());
        // Stub runs, hidden visual checks and dev views never touch the network; a debug build only with a URL of its own.
        let quiet = std::env::var_os("AITRACK_STUBS").is_some() || crate::dev_hidden::enabled() || std::env::var_os("AITRACK_DEV_VIEW").is_some();
        let auto = enabled && !quiet && (!cfg!(debug_assertions) || custom_url.is_some());
        Self { enabled, auto, url: custom_url.unwrap_or_else(|| aitrack_update::DEFAULT_MANIFEST_URL.to_owned()), root: crate::appearance::config_dir().join("updates"), current: aitrack_update::CURRENT_VERSION }
    }
}

pub struct Updater {
    pub prefs: UpdatePrefs,
    pub state: State,
    pub config: Config,
    update: Option<Update>,
    ready_path: Option<PathBuf>,
    tx: Sender<Event>,
    rx: Receiver<Event>,
    cancel: Arc<AtomicBool>,
    next_check: Option<Instant>,
    download_wanted: bool,
    /// The state before the check in flight, restored when an automatic check fails.
    before_check: Option<State>,
    /// A short message for the toast after a check the user asked for.
    notice: Option<String>,
    quit_app: bool,
}

impl Updater {
    pub fn new(now: Instant, config: Config, prefs: UpdatePrefs) -> Self {
        let (tx, rx) = unbounded();
        if config.enabled {
            // What the last update left behind, off the UI thread.
            let (root, current) = (config.root.clone(), config.current);
            std::thread::spawn(move || {
                if let Ok(exe) = std::env::current_exe() {
                    aitrack_update::cleanup_leftovers(&exe);
                }
                aitrack_update::prune_downloads(&root, current);
            });
        }
        let next_check = config.auto.then_some(now + FIRST_CHECK);
        Self {
            prefs,
            state: State::Idle,
            config,
            update: None,
            ready_path: None,
            tx,
            rx,
            cancel: Arc::new(AtomicBool::new(false)),
            next_check,
            download_wanted: false,
            before_check: None,
            notice: None,
            quit_app: false,
        }
    }

    pub fn current_version(&self) -> &'static str {
        self.config.current
    }

    /// Why this build cannot install an update, if so.
    pub fn cannot_install(&self) -> Option<String> {
        if !aitrack_update::key::CONFIGURED {
            return Some(UpdateError::Unconfigured.to_string());
        }
        None
    }

    /// The toast text after a check the user asked for, once.
    pub fn take_notice(&mut self) -> Option<String> {
        self.notice.take()
    }

    /// The install finished and the app should quit now, once.
    pub fn take_quit(&mut self) -> bool {
        std::mem::take(&mut self.quit_app)
    }

    /// "Check for Updates…".
    pub fn check_now(&mut self, now: Instant) {
        if !self.config.enabled {
            self.state = State::Failed("Updates are turned off (AITRACK_UPDATE=0).".into());
            return;
        }
        match self.state {
            State::Checking | State::Downloading { .. } | State::Installing => return,
            State::Ready(_) => {
                self.notice = Some("An update is ready: restart to install it".into());
                return;
            }
            _ => {}
        }
        self.begin_check(true, now);
    }

    fn begin_check(&mut self, manual: bool, now: Instant) {
        self.before_check = Some(std::mem::replace(&mut self.state, State::Checking));
        self.next_check = self.config.auto.then_some(now + CHECK_EVERY);
        let (url, current, tx) = (self.config.url.clone(), self.config.current, self.tx.clone());
        std::thread::spawn(move || {
            let result = aitrack_update::check(&url, current);
            let _ = tx.send(Event::Checked { manual, result });
        });
    }

    /// Start downloading the offered version (also what the status item does when
    /// the download is not automatic).
    pub fn start_download(&mut self) {
        let State::Available(offer) = self.state.clone() else { return };
        let (Some(update), true) = (self.update.clone(), self.config.enabled) else { return };
        let key = match aitrack_update::key::public_key() {
            Ok(k) => k,
            Err(e) => {
                self.state = State::Failed(e.to_string());
                return;
            }
        };
        self.download_wanted = false;
        self.cancel.store(false, Ordering::Relaxed);
        self.state = State::Downloading { offer, done: 0, total: update.asset.size };
        let (root, tx, cancel) = (self.config.root.clone(), self.tx.clone(), self.cancel.clone());
        std::thread::spawn(move || {
            let result = aitrack_update::download(&update, &root, key, &cancel, &mut |p| {
                let _ = tx.send(Event::Progress { done: p.done, total: p.total });
            });
            let _ = tx.send(Event::Downloaded(result));
        });
    }

    /// Install the downloaded update; the app quits when [`Updater::take_quit`] says so.
    /// The caller has made the project safe first. Returns false if nothing is ready.
    /// `before` is the project's last autosave still being written: it finishes first.
    pub fn install_now(&mut self, before: Option<std::thread::JoinHandle<()>>) -> bool {
        let (State::Ready(_), Some(path)) = (&self.state, self.ready_path.clone()) else { return false };
        // The quit handler must not install it a second time.
        if let Ok(mut r) = QUIT_HOOK.ready.lock() {
            *r = None;
        }
        self.state = State::Installing;
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            if let Some(h) = before {
                let _ = h.join();
            }
            let result = std::env::current_exe().map_err(|e| UpdateError::io("finding the app", e)).and_then(|exe| aitrack_update::install_downloaded(&path, &exe, &aitrack_update::SystemLauncher, true));
            let _ = tx.send(Event::Installed(result));
        });
        true
    }

    /// Housekeeping: apply what the threads reported, start due checks and downloads.
    /// `busy` is true while recording or exporting. Returns whether anything changed on screen.
    pub fn tick(&mut self, now: Instant, busy: bool) -> bool {
        QUIT_HOOK.busy.store(busy, Ordering::Relaxed);
        let mut changed = false;
        while let Ok(event) = self.rx.try_recv() {
            changed |= self.apply(event, now);
        }
        if self.prefs.check_automatically
            && self.config.auto
            && !busy
            && self.next_check.is_some_and(|t| now >= t)
            && matches!(self.state, State::Idle | State::UpToDate | State::Failed(_) | State::Available(_))
        {
            self.begin_check(false, now);
            changed = true;
        }
        if self.download_wanted && !busy && matches!(self.state, State::Available(_)) {
            self.start_download();
            changed = true;
        }
        self.sync_quit_hook();
        changed
    }

    fn sync_quit_hook(&self) {
        let path = if self.prefs.install_automatically && matches!(self.state, State::Ready(_)) { self.ready_path.clone() } else { None };
        if let Ok(mut r) = QUIT_HOOK.ready.lock() {
            *r = path;
        }
    }

    fn apply(&mut self, event: Event, _now: Instant) -> bool {
        match event {
            Event::Checked { manual, result } => {
                let before = self.before_check.take().unwrap_or(State::Idle);
                match result {
                    Ok(found) => {
                        let secs = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
                        self.prefs.last_check = Some(secs);
                        self.prefs.save_in_background();
                        match found {
                            Some(update) => {
                                let offer = Offer { version: update.version.to_string(), notes: update.notes.clone() };
                                log::info!("update {} is available", offer.version);
                                if manual {
                                    self.notice = Some(format!("Update {} is available", offer.version));
                                }
                                self.update = Some(update);
                                self.download_wanted = self.prefs.install_automatically && self.cannot_install().is_none();
                                self.state = State::Available(offer);
                            }
                            None => {
                                if manual {
                                    self.notice = Some(format!("AI-Track {} is up to date", self.config.current));
                                }
                                self.state = State::UpToDate;
                            }
                        }
                    }
                    Err(e) if manual => {
                        log::warn!("update check failed: {e}");
                        self.state = State::Failed(e.to_string());
                        self.notice = Some(format!("Could not check for updates: {e}"));
                    }
                    // Without a network the automatic check says nothing.
                    Err(e) => {
                        log::info!("update check failed: {e}");
                        self.state = before;
                    }
                }
                true
            }
            Event::Progress { done, total } => {
                if let State::Downloading { done: d, total: t, .. } = &mut self.state {
                    *d = done;
                    *t = total;
                    return true;
                }
                false
            }
            Event::Downloaded(result) => {
                let State::Downloading { offer, .. } = self.state.clone() else { return false };
                match result {
                    Ok(path) => {
                        log::info!("update {} downloaded and verified", offer.version);
                        self.ready_path = Some(path);
                        self.state = State::Ready(offer);
                    }
                    Err(e) => {
                        log::error!("downloading update {} failed: {e}", offer.version);
                        self.state = State::Failed(e.to_string());
                    }
                }
                true
            }
            Event::Installed(result) => {
                match result {
                    Ok(()) => self.quit_app = true,
                    Err(e) => {
                        log::error!("installing the update failed: {e}");
                        self.state = State::Failed(e.to_string());
                        self.ready_path = None;
                    }
                }
                true
            }
        }
    }

    /// Dev view: put the updater in a state without any network (`AITRACK_DEV_VIEW`).
    pub fn seed(&mut self, state: State) {
        if matches!(state, State::Ready(_)) {
            self.ready_path = Some(PathBuf::from("seed"));
        }
        self.state = state;
        self.next_check = None;
    }
}

/// The percentage of a download, 0..=100.
pub fn percent(done: u64, total: u64) -> u32 {
    if total == 0 { 0 } else { ((done.min(total) as f64 / total as f64) * 100.) as u32 }
}

/// "5 minutes ago", "never".
pub fn last_check_label(last: Option<u64>) -> String {
    let Some(secs) = last else { return "never".into() };
    let then = UNIX_EPOCH + Duration::from_secs(secs);
    crate::autosave::ago(then, SystemTime::now())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quiet(auto: bool) -> Config {
        Config { enabled: true, auto, url: "http://127.0.0.1:1/latest.json".into(), root: std::env::temp_dir().join("aitrack-updater-test"), current: "0.0.1" }
    }

    fn offer() -> Offer {
        Offer { version: "0.0.2".into(), notes: "Fixes".into() }
    }

    #[test]
    fn missing_settings_mean_the_defaults() {
        assert_eq!(serde_json::from_str::<UpdatePrefs>("{}").unwrap(), UpdatePrefs::default());
        let p = serde_json::from_str::<UpdatePrefs>(r#"{"check_automatically":false}"#).unwrap();
        assert!(!p.check_automatically && p.install_automatically && p.last_check.is_none());
        assert!(UpdatePrefs::default().check_automatically && UpdatePrefs::default().install_automatically);
    }

    #[test]
    fn the_first_automatic_check_waits_30_seconds() {
        let t0 = Instant::now();
        let mut u = Updater::new(t0, quiet(true), UpdatePrefs::default());
        u.tick(t0 + Duration::from_secs(29), false);
        assert_eq!(u.state, State::Idle);
        u.tick(t0 + FIRST_CHECK, false);
        assert_eq!(u.state, State::Checking);
        // The next one is 6 h after.
        assert_eq!(u.next_check, Some(t0 + FIRST_CHECK + CHECK_EVERY));
    }

    #[test]
    fn nothing_checks_when_it_must_not() {
        let t0 = Instant::now();
        let late = t0 + Duration::from_secs(60);
        let mut u = Updater::new(t0, quiet(false), UpdatePrefs::default());
        u.tick(late, false);
        assert_eq!(u.state, State::Idle, "auto off (stubs, debug builds)");
        let mut u = Updater::new(t0, quiet(true), UpdatePrefs { check_automatically: false, ..Default::default() });
        u.tick(late, false);
        assert_eq!(u.state, State::Idle, "the user turned it off");
        let mut off = quiet(true);
        off.enabled = false;
        let mut u = Updater::new(t0, off, UpdatePrefs::default());
        u.check_now(t0);
        assert!(matches!(u.state, State::Failed(_)), "AITRACK_UPDATE=0");
    }

    #[test]
    fn a_found_update_is_downloaded_unless_busy_or_manual_mode() {
        let t0 = Instant::now();
        let found = |u: &mut Updater| {
            let update = aitrack_update::evaluate(
                &aitrack_update::Manifest::parse(r#"{"version":"0.0.2","notes":"Fixes","platforms":{"macos-universal":{"url":"https://x/a","signature":"s","sha256":"a","size":5},"windows-x86_64":{"url":"https://x/a","signature":"s","sha256":"a","size":5}}}"#).unwrap(),
                "0.0.1",
                aitrack_update::platform_key().unwrap_or("macos-universal"),
            )
            .unwrap();
            u.before_check = Some(State::Idle);
            u.state = State::Checking;
            u.apply(Event::Checked { manual: false, result: Ok(update) }, t0);
        };
        let mut u = Updater::new(t0, quiet(false), UpdatePrefs::default());
        found(&mut u);
        assert_eq!(u.state, State::Available(offer()));
        // Recording: it waits.
        u.tick(t0, true);
        assert!(matches!(u.state, State::Available(_)));
        // A manual-download setup never starts by itself.
        let mut m = Updater::new(t0, quiet(false), UpdatePrefs { install_automatically: false, ..Default::default() });
        found(&mut m);
        m.tick(t0, false);
        assert!(matches!(m.state, State::Available(_)));
    }

    #[test]
    fn an_automatic_check_that_fails_stays_quiet_a_manual_one_says_so() {
        let t0 = Instant::now();
        let mut u = Updater::new(t0, quiet(false), UpdatePrefs::default());
        u.state = State::Checking;
        u.before_check = Some(State::UpToDate);
        u.apply(Event::Checked { manual: false, result: Err(UpdateError::Network("offline".into())) }, t0);
        assert_eq!(u.state, State::UpToDate);
        assert!(u.take_notice().is_none());
        u.state = State::Checking;
        u.apply(Event::Checked { manual: true, result: Err(UpdateError::Network("offline".into())) }, t0);
        assert!(matches!(u.state, State::Failed(_)));
        assert!(u.take_notice().unwrap().contains("offline"));
        u.state = State::Checking;
        u.apply(Event::Checked { manual: true, result: Ok(None) }, t0);
        assert_eq!(u.state, State::UpToDate);
        assert!(u.take_notice().unwrap().contains("up to date"));
    }

    #[test]
    fn download_and_install_results_move_the_state() {
        let t0 = Instant::now();
        let mut u = Updater::new(t0, quiet(false), UpdatePrefs::default());
        u.state = State::Downloading { offer: offer(), done: 0, total: 10 };
        u.apply(Event::Progress { done: 5, total: 10 }, t0);
        assert_eq!(percent(5, 10), 50);
        u.apply(Event::Downloaded(Err(UpdateError::HashMismatch)), t0);
        assert!(matches!(&u.state, State::Failed(m) if m.contains("checksum")));
        u.state = State::Downloading { offer: offer(), done: 10, total: 10 };
        u.apply(Event::Downloaded(Ok(PathBuf::from("/x"))), t0);
        assert_eq!(u.state, State::Ready(offer()));
        u.state = State::Installing;
        u.apply(Event::Installed(Err(UpdateError::NotWritable(PathBuf::from("/Applications")))), t0);
        assert!(matches!(u.state, State::Failed(_)));
        assert!(!u.take_quit());
        u.apply(Event::Installed(Ok(())), t0);
        assert!(u.take_quit() && !u.take_quit());
    }

    #[test]
    fn percent_is_safe() {
        assert_eq!(percent(0, 0), 0);
        assert_eq!(percent(20, 10), 100);
        assert_eq!(last_check_label(None), "never");
    }
}
