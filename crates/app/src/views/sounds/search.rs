//! The Sounds panel's search, off the UI thread.
//!
//! The library lives in SQLite and a big one takes long enough to answer that
//! asking on every keystroke made typing stutter. Searches run on one worker
//! thread (`sounds-search`), the way the Cmd+K palette's do: every request
//! carries a generation; when several are waiting the worker answers only the
//! newest (the rest are cancelled before they start), and the UI keeps only an
//! answer newer than the one it already shows, so a slow answer to an old
//! query never replaces a fresher one.

use crate::bridge::{SoundEnv, SoundPage, SoundQuery, SoundSearchFn};
use crossbeam_channel::{Receiver, Sender, unbounded};

struct Job {
    generation: u64,
    query: SoundQuery,
    env: SoundEnv,
}

pub struct Searcher {
    tx: Sender<Job>,
    rx: Receiver<(u64, SoundPage)>,
    /// The generation last asked for, and the newest one shown.
    asked: u64,
    answered: u64,
}

impl Searcher {
    pub fn spawn(mut search: SoundSearchFn) -> Self {
        let (tx, jobs) = unbounded::<Job>();
        let (out, rx) = unbounded();
        let run = move || {
            while let Ok(mut job) = jobs.recv() {
                // Typing outruns the search: only the newest request is worth answering.
                while let Ok(newer) = jobs.try_recv() {
                    job = newer;
                }
                let page = search(&job.query, &job.env);
                if out.send((job.generation, page)).is_err() {
                    break;
                }
            }
        };
        if let Err(e) = std::thread::Builder::new().name("sounds-search".into()).spawn(run) {
            log::warn!("sounds: cannot start the search thread: {e}");
        }
        Self { tx, rx, asked: 0, answered: 0 }
    }

    /// Ask for `query`; returns at once.
    pub fn ask(&mut self, query: SoundQuery, env: SoundEnv) {
        self.asked += 1;
        let _ = self.tx.send(Job { generation: self.asked, query, env });
    }

    /// The newest answer that has come in since the last call, if it is newer than what is shown.
    pub fn poll(&mut self) -> Option<SoundPage> {
        let mut fresh = None;
        for (generation, page) in self.rx.try_iter() {
            if generation > self.answered {
                self.answered = generation;
                fresh = Some(page);
            }
        }
        fresh
    }

    /// A request has not been answered yet.
    pub fn pending(&self) -> bool {
        self.answered < self.asked
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    fn page(tag: &str) -> SoundPage {
        SoundPage { tags: vec![(tag.to_owned(), 1)], ..Default::default() }
    }

    fn wait(s: &mut Searcher) -> SoundPage {
        let start = Instant::now();
        loop {
            if let Some(p) = s.poll() {
                return p;
            }
            assert!(start.elapsed() < Duration::from_secs(5), "no answer");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    #[test]
    fn asking_does_not_wait_for_the_search_and_runs_it_on_another_thread() {
        let ran_on = Arc::new(Mutex::new(None));
        let seen = ran_on.clone();
        let mut s = Searcher::spawn(Box::new(move |q, _| {
            std::thread::sleep(Duration::from_millis(150));
            *seen.lock().unwrap() = Some(std::thread::current().id());
            page(&q.text)
        }));
        let start = Instant::now();
        s.ask(SoundQuery { text: "kick".into(), ..Default::default() }, SoundEnv::default());
        assert!(start.elapsed() < Duration::from_millis(50), "asking took {:?}", start.elapsed());
        assert!(s.pending() && s.poll().is_none());
        assert_eq!(wait(&mut s).tags[0].0, "kick");
        assert!(!s.pending());
        assert_ne!(*ran_on.lock().unwrap(), Some(std::thread::current().id()));
    }

    #[test]
    fn typing_faster_than_the_search_runs_only_the_newest_query() {
        let ran = Arc::new(Mutex::new(Vec::<String>::new()));
        let log = ran.clone();
        let mut s = Searcher::spawn(Box::new(move |q, _| {
            std::thread::sleep(Duration::from_millis(60));
            log.lock().unwrap().push(q.text.clone());
            page(&q.text)
        }));
        let mut typed = String::new();
        for c in "snare drum".chars() {
            typed.push(c);
            s.ask(SoundQuery { text: typed.clone(), ..Default::default() }, SoundEnv::default());
            std::thread::sleep(Duration::from_millis(5));
        }
        // The last answer is for the last text, however many came before.
        let mut last = wait(&mut s);
        while s.pending() {
            last = wait(&mut s);
        }
        assert_eq!(last.tags[0].0, "snare drum");
        let ran = ran.lock().unwrap();
        assert!(ran.len() < "snare drum".len(), "cancelled the stale ones: {ran:?}");
        assert_eq!(ran.last().map(String::as_str), Some("snare drum"));
    }

    #[test]
    fn the_environment_travels_with_the_request() {
        let mut s = Searcher::spawn(Box::new(|_, env| page(&format!("{:?}/{}", env.splice_folder, env.scanning))));
        s.ask(SoundQuery::default(), SoundEnv { splice_folder: Some("/x".into()), scanning: true });
        assert_eq!(wait(&mut s).tags[0].0, "Some(\"/x\")/true");
    }
}
