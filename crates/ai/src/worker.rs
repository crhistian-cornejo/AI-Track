//! Background execution: jobs never run on the audio thread or the UI thread.
//!
//! * [`Worker`]: an in-process thread pool. `submit` returns a [`JobHandle`]
//!   with a progress/result channel and a cancel token.
//! * The `aitrack-ml-worker` binary speaks the same jobs as JSON lines over
//!   stdin/stdout ([`Request`] in, [`Response`] out), so heavy inference (Demucs
//!   uses ~1 GB) can live in a separate process the DAW can kill or restart.
//!   [`ProcessWorker`] is the client side.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use aitrack_model::Note;
use crossbeam_channel::{Receiver, Sender, unbounded};
use serde::{Deserialize, Serialize};

use crate::analysis::{SampleAnalysis, analyze};
use crate::chords::{ChordOptions, ChordSegment, chords_per_bar, detect_chords};
use crate::midi::{MidiOptions, extract_midi_with};
use crate::models::{InstalledModel, ModelManager, ModelStatus};
use crate::progress::{CancelToken, Progress};
use crate::stems::{StemOptions, split_stems_with};
use crate::{Error, Result, audio};

/// A unit of work.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Job {
    Analyze {
        path: PathBuf,
    },
    ExtractMidi {
        path: PathBuf,
        #[serde(default)]
        options: MidiOptions,
    },
    SplitStems {
        path: PathBuf,
        out_dir: PathBuf,
        #[serde(default)]
        options: StemOptions,
    },
    DetectChords {
        path: PathBuf,
        #[serde(default)]
        options: ChordOptions,
    },
    ListModels,
    EnsureModel {
        id: String,
    },
    AcceptLicense {
        id: String,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum JobOutput {
    Analysis {
        analysis: Box<SampleAnalysis>,
    },
    Notes {
        notes: Vec<Note>,
    },
    Files {
        files: Vec<PathBuf>,
    },
    Chords {
        segments: Vec<ChordSegment>,
        bars: Vec<String>,
    },
    Models {
        models: Vec<ModelStatus>,
    },
    Model {
        model: InstalledModel,
    },
    Ok,
}

/// Run one job synchronously on the current thread.
pub fn run_job(
    manager: &ModelManager,
    job: &Job,
    progress: &mut dyn FnMut(Progress),
    cancel: &CancelToken,
) -> Result<JobOutput> {
    match job {
        Job::Analyze { path } => Ok(JobOutput::Analysis {
            analysis: Box::new(analyze(path)?),
        }),
        Job::ExtractMidi { path, options } => Ok(JobOutput::Notes {
            notes: extract_midi_with(manager, path, options, progress, cancel)?,
        }),
        Job::SplitStems {
            path,
            out_dir,
            options,
        } => Ok(JobOutput::Files {
            files: split_stems_with(manager, path, out_dir, options, progress, cancel)?,
        }),
        Job::DetectChords { path, options } => {
            let a = audio::load(path)?;
            let mono = a.to_mono();
            let mut opts = options.clone();
            if opts.bpm.is_none() {
                opts.bpm = crate::tempo::detect_bpm(&mono, a.sample_rate).map(|b| b.bpm);
            }
            let segments = detect_chords(&mono, a.sample_rate, &opts);
            let first = opts
                .first_downbeat
                .unwrap_or_else(|| segments.first().map_or(0.0, |s| s.start));
            let bars = chords_per_bar(
                &segments,
                opts.bpm.unwrap_or(120.0),
                opts.beats_per_bar,
                first,
            );
            Ok(JobOutput::Chords { segments, bars })
        }
        Job::ListModels => Ok(JobOutput::Models {
            models: manager.list(),
        }),
        Job::EnsureModel { id } => Ok(JobOutput::Model {
            model: manager.ensure_cancellable(id, progress, cancel)?,
        }),
        Job::AcceptLicense { id } => {
            manager.accept_license(id)?;
            Ok(JobOutput::Ok)
        }
    }
}

/// Events a job emits.
#[derive(Clone, Debug, PartialEq)]
pub enum JobEvent {
    Progress(Progress),
    Done(std::result::Result<JobOutput, String>),
}

pub struct JobHandle {
    pub id: u64,
    pub events: Receiver<JobEvent>,
    pub cancel: CancelToken,
}

impl JobHandle {
    /// Block until the job finishes, forwarding progress to `on_progress`.
    pub fn wait(
        self,
        mut on_progress: impl FnMut(Progress),
    ) -> std::result::Result<JobOutput, String> {
        for ev in self.events.iter() {
            match ev {
                JobEvent::Progress(p) => on_progress(p),
                JobEvent::Done(r) => return r,
            }
        }
        Err("worker stopped".into())
    }
}

type Queued = (u64, Job, CancelToken, Sender<JobEvent>);

/// In-process worker pool.
pub struct Worker {
    tx: Option<Sender<Queued>>,
    threads: Vec<JoinHandle<()>>,
    next: AtomicU64,
}

impl Worker {
    /// `threads` workers sharing one model manager. ONNX Runtime parallelises
    /// inside a session, so 1–2 threads is usually right.
    pub fn new(manager: ModelManager, threads: usize) -> Self {
        let (tx, rx) = unbounded::<Queued>();
        let manager = Arc::new(manager);
        let threads = (0..threads.max(1))
            .map(|i| {
                let rx = rx.clone();
                let manager = manager.clone();
                std::thread::Builder::new()
                    .name(format!("aitrack-ml-{i}"))
                    .spawn(move || {
                        for (_, job, cancel, events) in rx.iter() {
                            let ev = events.clone();
                            let mut progress = |p: Progress| {
                                let _ = ev.send(JobEvent::Progress(p));
                            };
                            let r = if cancel.is_cancelled() {
                                Err(Error::Cancelled)
                            } else {
                                // A panicking job must not take the pool down.
                                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                    run_job(&manager, &job, &mut progress, &cancel)
                                }))
                                .unwrap_or_else(|_| Err(Error::Runtime("job panicked".into())))
                            };
                            let _ = events.send(JobEvent::Done(r.map_err(|e| e.to_string())));
                        }
                    })
                    .expect("spawn worker thread")
            })
            .collect();
        Self {
            tx: Some(tx),
            threads,
            next: AtomicU64::new(1),
        }
    }

    pub fn submit(&self, job: Job) -> JobHandle {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let (etx, erx) = unbounded();
        let cancel = CancelToken::new();
        if let Some(tx) = &self.tx {
            let _ = tx.send((id, job, cancel.clone(), etx));
        }
        JobHandle {
            id,
            events: erx,
            cancel,
        }
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.tx.take();
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
    }
}

/// One line on the worker's stdin.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Request {
    Run { id: u64, job: Job },
    Cancel { cancel: u64 },
    Shutdown { shutdown: bool },
}

/// One line on the worker's stdout.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Response {
    Ready { version: String },
    Progress { id: u64, progress: Progress },
    Done { id: u64, output: JobOutput },
    Error { id: u64, message: String },
}

/// Serve the JSON-lines protocol until stdin closes or `shutdown` arrives.
pub fn serve(manager: ModelManager, input: impl BufRead, output: impl Write + Send + 'static) {
    let out = Arc::new(Mutex::new(output));
    let send = {
        let out = out.clone();
        move |r: &Response| {
            if let Ok(mut o) = out.lock() {
                let _ = serde_json::to_writer(&mut *o, r);
                let _ = o.write_all(b"\n");
                let _ = o.flush();
            }
        }
    };
    send(&Response::Ready {
        version: env!("CARGO_PKG_VERSION").into(),
    });
    let worker = Worker::new(manager, 1);
    let cancels: Arc<Mutex<HashMap<u64, CancelToken>>> = Arc::default();
    let mut forwarders = Vec::new();
    for line in input.lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<Request>(&line) {
            Ok(Request::Run { id, job }) => {
                let handle = worker.submit(job);
                cancels
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .insert(id, handle.cancel.clone());
                let send = send.clone();
                let cancels = cancels.clone();
                forwarders.push(std::thread::spawn(move || {
                    for ev in handle.events.iter() {
                        match ev {
                            JobEvent::Progress(p) => send(&Response::Progress { id, progress: p }),
                            JobEvent::Done(r) => {
                                send(&match r {
                                    Ok(output) => Response::Done { id, output },
                                    Err(message) => Response::Error { id, message },
                                });
                                break;
                            }
                        }
                    }
                    cancels
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .remove(&id);
                }));
            }
            Ok(Request::Cancel { cancel }) => {
                if let Some(c) = cancels
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .get(&cancel)
                {
                    c.cancel();
                }
            }
            Ok(Request::Shutdown { .. }) => {
                // Explicit shutdown cancels running work; plain EOF lets it finish.
                for c in cancels.lock().unwrap_or_else(|p| p.into_inner()).values() {
                    c.cancel();
                }
                break;
            }
            Err(e) => send(&Response::Error {
                id: 0,
                message: format!("bad request: {e}"),
            }),
        }
    }
    drop(worker);
    for f in forwarders {
        let _ = f.join();
    }
}

/// Client for a spawned `aitrack-ml-worker` process.
pub struct ProcessWorker {
    child: Child,
    stdin: Mutex<ChildStdin>,
    pending: Arc<Mutex<HashMap<u64, Sender<JobEvent>>>>,
    next: AtomicU64,
    reader: Option<JoinHandle<()>>,
}

impl ProcessWorker {
    /// Spawn the worker executable (usually next to the app binary).
    pub fn spawn(exe: impl AsRef<std::path::Path>) -> Result<Self> {
        let exe = exe.as_ref();
        let mut child = Command::new(exe)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| Error::io(exe, e))?;
        let stdin = child.stdin.take().expect("piped stdin");
        let stdout = child.stdout.take().expect("piped stdout");
        let pending: Arc<Mutex<HashMap<u64, Sender<JobEvent>>>> = Arc::default();
        let p = pending.clone();
        let reader = std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                let Ok(resp) = serde_json::from_str::<Response>(&line) else {
                    continue;
                };
                let mut map = p.lock().unwrap_or_else(|e| e.into_inner());
                match resp {
                    Response::Ready { .. } => {}
                    Response::Progress { id, progress } => {
                        if let Some(tx) = map.get(&id) {
                            let _ = tx.send(JobEvent::Progress(progress));
                        }
                    }
                    Response::Done { id, output } => {
                        if let Some(tx) = map.remove(&id) {
                            let _ = tx.send(JobEvent::Done(Ok(output)));
                        }
                    }
                    Response::Error { id, message } => {
                        if let Some(tx) = map.remove(&id) {
                            let _ = tx.send(JobEvent::Done(Err(message)));
                        }
                    }
                }
            }
            // Process gone: fail everything still pending.
            for (_, tx) in p.lock().unwrap_or_else(|e| e.into_inner()).drain() {
                let _ = tx.send(JobEvent::Done(Err("ml worker exited".into())));
            }
        });
        Ok(Self {
            child,
            stdin: Mutex::new(stdin),
            pending,
            next: AtomicU64::new(1),
            reader: Some(reader),
        })
    }

    fn send(&self, req: &Request) -> Result<()> {
        let mut line = serde_json::to_vec(req).expect("request serialises");
        line.push(b'\n');
        let mut s = self.stdin.lock().unwrap_or_else(|e| e.into_inner());
        s.write_all(&line)
            .and_then(|_| s.flush())
            .map_err(|e| Error::io("ml worker stdin", e))
    }

    /// Submit a job. Cancelling the returned token is forwarded to the process
    /// by [`ProcessWorker::cancel`].
    pub fn submit(&self, job: Job) -> Result<(u64, Receiver<JobEvent>)> {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = unbounded();
        self.pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id, tx);
        self.send(&Request::Run { id, job })?;
        Ok((id, rx))
    }

    pub fn cancel(&self, id: u64) -> Result<()> {
        self.send(&Request::Cancel { cancel: id })
    }
}

impl Drop for ProcessWorker {
    fn drop(&mut self) {
        let _ = self.send(&Request::Shutdown { shutdown: true });
        if self.child.wait().is_err() {
            let _ = self.child.kill();
        }
        if let Some(r) = self.reader.take() {
            let _ = r.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsp::synth;

    fn temp_wav(name: &str, x: &[f32], sr: u32) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("aitrack-worker-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join(name);
        audio::write_wav(&p, &[x.to_vec()], sr, audio::WavFormat::Float32).unwrap();
        p
    }

    #[test]
    fn protocol_json_shapes() {
        let r: Request =
            serde_json::from_str(r#"{"id":3,"job":{"cmd":"analyze","path":"a.wav"}}"#).unwrap();
        assert_eq!(
            r,
            Request::Run {
                id: 3,
                job: Job::Analyze {
                    path: "a.wav".into()
                }
            }
        );
        let c: Request = serde_json::from_str(r#"{"cancel":3}"#).unwrap();
        assert_eq!(c, Request::Cancel { cancel: 3 });
        let m: Request = serde_json::from_str(r#"{"id":4,"job":{"cmd":"extract_midi","path":"x.wav","options":{"engine":"monophonic","bpm":128}}}"#).unwrap();
        let Request::Run {
            job: Job::ExtractMidi { options, .. },
            ..
        } = m
        else {
            panic!()
        };
        assert_eq!(options.bpm, 128.0);
        assert_eq!(options.onset_threshold, 0.5);
        let s = serde_json::to_string(&Response::Error {
            id: 1,
            message: "x".into(),
        })
        .unwrap();
        assert_eq!(s, r#"{"event":"error","id":1,"message":"x"}"#);
    }

    #[test]
    fn pool_runs_analysis_and_midi_off_thread() {
        let sr = 22_050;
        let kick = temp_wav("kick.wav", &synth::kick(sr, 0.5), sr);
        let worker = Worker::new(
            ModelManager::with_dir(std::env::temp_dir().join("aitrack-worker-models")),
            2,
        );
        let h = worker.submit(Job::Analyze { path: kick.clone() });
        let JobOutput::Analysis { analysis } = h.wait(|_| {}).unwrap() else {
            panic!()
        };
        assert_eq!(analysis.category, crate::SampleCategory::Kick);

        let tone = temp_wav("tone.wav", &synth::tone(440.0, 1.0, sr, &[0.5]), sr);
        let opts = MidiOptions {
            engine: crate::MidiEngine::Monophonic,
            ..Default::default()
        };
        let h = worker.submit(Job::ExtractMidi {
            path: tone,
            options: opts,
        });
        let JobOutput::Notes { notes } = h.wait(|_| {}).unwrap() else {
            panic!()
        };
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].key, 69);
    }

    #[test]
    fn serve_over_pipes() {
        let sr = 22_050;
        let kick = temp_wav("serve-kick.wav", &synth::kick(sr, 0.4), sr);
        let req = format!(
            "{}\n{}\n",
            serde_json::to_string(&Request::Run {
                id: 7,
                job: Job::Analyze { path: kick }
            })
            .unwrap(),
            serde_json::to_string(&Request::Run {
                id: 8,
                job: Job::ListModels
            })
            .unwrap()
        );
        let buf = SharedBuf::default();
        serve(
            ModelManager::with_dir(std::env::temp_dir().join("aitrack-worker-models")),
            req.as_bytes(),
            buf.clone(),
        );
        let text = String::from_utf8(buf.0.lock().unwrap().clone()).unwrap();
        let resp: Vec<Response> = text
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert!(matches!(resp[0], Response::Ready { .. }));
        assert!(resp.iter().any(|r| matches!(
            r,
            Response::Done {
                id: 7,
                output: JobOutput::Analysis { .. }
            }
        )));
        assert!(resp.iter().any(|r| matches!(
            r,
            Response::Done {
                id: 8,
                output: JobOutput::Models { .. }
            }
        )));
    }

    #[derive(Clone, Default)]
    struct SharedBuf(Arc<Mutex<Vec<u8>>>);
    impl Write for SharedBuf {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
}
