//! [`AiBridge`] over `aitrack-ai`: an in-process [`Worker`] (one thread;
//! ONNX Runtime parallelises inside a session) runs audio-to-MIDI, stem
//! separation and analysis. Chat still goes to the stub assistant until an
//! agent is connected over MCP.

use crate::bridge::*;
use aitrack_ai::worker::{Job, JobEvent, JobHandle, JobOutput, Worker};
use aitrack_ai::{MidiEngine, MidiOptions, ModelManager, StemOptions};
use aitrack_model::{PPQ, Project};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

enum Kind {
    Midi { name: String },
    Stems,
    Analyze(PathBuf),
}

pub struct RealAi {
    worker: Worker,
    manager: ModelManager,
    jobs: BTreeMap<JobId, (JobHandle, Kind)>,
    next: JobId,
    chat: StubAi,
}

fn manager() -> ModelManager {
    ModelManager::new().unwrap_or_else(|_| ModelManager::with_dir(std::env::temp_dir().join("AI-Track").join("models")))
}

/// "Song - Drums" → "Drums".
fn stem_name(path: &Path) -> String {
    let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    stem.rsplit(" - ").next().unwrap_or(&stem).to_owned()
}

impl RealAi {
    pub fn new() -> Self {
        Self { worker: Worker::new(manager(), 1), manager: manager(), jobs: BTreeMap::new(), next: 0, chat: StubAi::default() }
    }

    fn submit(&mut self, job: Job, kind: Kind) -> JobId {
        // Ids above the stub chat's range so both can share the chat log.
        self.next += 1;
        let id = 1_000_000 + self.next;
        let handle = self.worker.submit(job);
        self.jobs.insert(id, (handle, kind));
        id
    }
}

impl AiBridge for RealAi {
    fn extract_midi(&mut self, path: &Path, mode: MidiMode, bpm: f64) -> JobId {
        let engine = match mode {
            MidiMode::Polyphonic => MidiEngine::BasicPitch,
            MidiMode::Monophonic => MidiEngine::Monophonic,
            MidiMode::Drums => MidiEngine::Drums,
        };
        let options = MidiOptions { engine, bpm, quantize: Some(PPQ / 4), ..MidiOptions::default() };
        let name = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "Audio".into());
        self.submit(Job::ExtractMidi { path: path.to_path_buf(), options }, Kind::Midi { name: format!("{name} (MIDI)") })
    }

    fn split_stems(&mut self, path: &Path) -> JobId {
        let song = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "Audio".into());
        let out_dir = crate::appearance::config_dir().join("stems").join(&song);
        self.submit(Job::SplitStems { path: path.to_path_buf(), out_dir, options: StemOptions::default() }, Kind::Stems)
    }

    fn stem_license(&self) -> Option<ModelNotice> {
        let id = StemOptions::default().model;
        let e = self.manager.entry(&id).ok()?;
        if !e.requires_acceptance || self.manager.is_accepted(&id) {
            return None;
        }
        Some(ModelNotice {
            model: id.clone(),
            name: e.name.clone(),
            size: e.download_size(),
            license: e.license.clone(),
            source: e.source.clone(),
            notice: e.notice.clone(),
        })
    }

    fn accept_license(&mut self, model: &str) -> Result<(), String> {
        self.manager.accept_license(model).map_err(|e| e.to_string())
    }

    fn analyze(&mut self, path: &Path) -> JobId {
        self.submit(Job::Analyze { path: path.to_path_buf() }, Kind::Analyze(path.to_path_buf()))
    }

    fn chat(&mut self, text: &str, project: &Project) {
        self.chat.chat(text, project);
    }

    fn poll(&mut self) -> Vec<AiEvent> {
        let mut out = self.chat.poll();
        let mut finished = Vec::new();
        for (&id, (handle, kind)) in &self.jobs {
            while let Ok(ev) = handle.events.try_recv() {
                match ev {
                    JobEvent::Progress(p) => {
                        let label = if p.message.is_empty() { format!("{:?}", p.stage) } else { p.message.clone() };
                        out.push(AiEvent::Progress { job: id, label, fraction: p.fraction });
                    }
                    JobEvent::Done(result) => {
                        finished.push(id);
                        out.push(match (result, kind) {
                            (Ok(JobOutput::Notes { notes }), Kind::Midi { name }) => AiEvent::MidiReady {
                                job: id,
                                source_name: name.clone(),
                                notes: notes.iter().map(|n| DetectedNote { key: n.key, start: n.start, length: n.length, velocity: n.velocity }).collect(),
                            },
                            (Ok(JobOutput::Files { files }), Kind::Stems) => {
                                AiEvent::StemsReady { job: id, stems: files.iter().map(|f| (stem_name(f), f.clone())).collect() }
                            }
                            (Ok(JobOutput::Analysis { analysis: a }), Kind::Analyze(path)) => AiEvent::Analyzed {
                                job: id,
                                path: path.clone(),
                                analysis: AudioAnalysis {
                                    key: a.key.map(|(s, _)| s),
                                    bpm: a.bpm.map(|(b, _)| b as f64),
                                    sample_rate: a.sample_rate,
                                    channels: a.channels,
                                    frames: (a.duration * a.sample_rate as f64).round() as u64,
                                },
                            },
                            (Ok(other), _) => AiEvent::Failed { job: id, message: format!("unexpected result {other:?}") },
                            (Err(message), _) => AiEvent::Failed { job: id, message },
                        });
                        break;
                    }
                }
            }
        }
        for id in finished {
            self.jobs.remove(&id);
        }
        out
    }

    fn busy(&self) -> bool {
        !self.jobs.is_empty() || self.chat.busy()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stem_names_come_from_the_file_suffix() {
        assert_eq!(stem_name(Path::new("C:/x/Song - Drums.wav")), "Drums");
        assert_eq!(stem_name(Path::new("Vocals.wav")), "Vocals");
    }
}
