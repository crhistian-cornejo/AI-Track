//! Splice results in the palette (opt-in, Preferences → Sounds). The
//! [`aitrack_splice::Service`] does every network call on its own threads;
//! this keeps what it answered for the rows.

use aitrack_model::{Key, Scale, ScaleKind};
use aitrack_splice::auth::KeychainStore;
use aitrack_splice::{AssetKind, Event, SearchParams, Service, Sort, SpliceSample, TagCount};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use super::search::KindFilter;

pub struct SpliceUi {
    service: Service,
    generation: u64,
    last: Option<SearchParams>,
    pub results: Vec<SpliceSample>,
    pub tags: Vec<TagCount>,
    pub total: u64,
    pub loading: bool,
    pub error: Option<String>,
    covers: HashMap<String, Option<PathBuf>>,
    waves: HashMap<String, Arc<Vec<f32>>>,
    asked_waves: std::collections::HashSet<String>,
    /// The preview being fetched to play when it arrives.
    pub wanted_preview: Option<String>,
    pub previewing: Option<String>,
    pub signing_in: bool,
    pub signed_in: Option<bool>,
}

/// What the UI should do after a poll.
#[derive(Default)]
pub struct Polled {
    pub changed: bool,
    pub play: Option<PathBuf>,
    pub open_url: Option<String>,
    pub message: Option<String>,
}

pub fn cache_dir() -> PathBuf {
    crate::appearance::config_dir().join("splice-cache")
}

/// The query as Splice's key filter wants it.
pub fn splice_key(s: Scale) -> (String, Option<String>) {
    let minor = matches!(s.kind, ScaleKind::Minor | ScaleKind::HarmonicMinor | ScaleKind::MelodicMinor | ScaleKind::Dorian | ScaleKind::Phrygian | ScaleKind::MinorPentatonic);
    (aitrack_splice::query::key_root(s.root.0).to_owned(), Some(if minor { "minor" } else { "major" }.to_owned()))
}

/// A Splice key ("a" + "minor") as a scale, for sorting and in-key marks.
pub fn scale_of(s: &SpliceSample) -> Option<Scale> {
    let root = Key::from_name(s.key.as_deref()?)?;
    let minor = s.chord_type.as_deref().is_some_and(|c| c.eq_ignore_ascii_case("minor"));
    Some(Scale::new(root, if minor { ScaleKind::Minor } else { ScaleKind::Major }))
}

pub fn params(text: &str, kind: KindFilter, bpm: Option<(u32, u32)>, key: Option<Scale>, tags: &[String], shuffle: Option<u64>) -> SearchParams {
    SearchParams {
        text: text.trim().to_owned(),
        kind: match kind {
            KindFilter::All => None,
            KindFilter::Loops => Some(AssetKind::Loop),
            KindFilter::OneShots => Some(AssetKind::OneShot),
        },
        bpm,
        key: key.map(splice_key),
        tags: tags.to_vec(),
        sort: if shuffle.is_some() { Sort::Random } else if text.trim().is_empty() { Sort::Popularity } else { Sort::Relevance },
        seed: shuffle.map(|s| s.to_string()),
        ..Default::default()
    }
}

impl SpliceUi {
    pub fn new() -> Self {
        Self {
            service: Service::spawn(cache_dir(), Arc::new(KeychainStore)),
            generation: 0,
            last: None,
            results: Vec::new(),
            tags: Vec::new(),
            total: 0,
            loading: false,
            error: None,
            covers: HashMap::new(),
            waves: HashMap::new(),
            asked_waves: Default::default(),
            wanted_preview: None,
            previewing: None,
            signing_in: false,
            signed_in: None,
        }
    }

    /// Search unless the same query is already answered or on its way.
    pub fn search(&mut self, p: SearchParams) {
        if self.last.as_ref() == Some(&p) {
            return;
        }
        self.generation += 1;
        self.last = Some(p.clone());
        self.loading = true;
        self.service.search(self.generation, p);
    }

    pub fn preview(&mut self, s: &SpliceSample) {
        self.wanted_preview = Some(s.uuid.clone());
        self.service.preview(s.clone());
    }

    pub fn cover(&mut self, url: &str) -> Option<PathBuf> {
        match self.covers.get(url) {
            Some(p) => p.clone(),
            None => {
                self.covers.insert(url.to_owned(), None);
                self.service.cover(url.to_owned());
                None
            }
        }
    }

    pub fn wave(&mut self, s: &SpliceSample) -> Option<Arc<Vec<f32>>> {
        if let Some(w) = self.waves.get(&s.uuid) {
            return Some(w.clone());
        }
        if let Some(url) = &s.waveform_url
            && self.asked_waves.insert(s.uuid.clone())
        {
            self.service.waveform(s.uuid.clone(), url.clone());
        }
        None
    }

    pub fn sign_in(&mut self) {
        self.signing_in = true;
        self.service.sign_in();
    }

    pub fn sign_out(&mut self) {
        self.service.sign_out();
    }

    /// Refresh the signed-in flag without draining events.
    pub fn poll_sign_in(&mut self) -> Option<bool> {
        self.signed_in = self.service.signed_in();
        self.signed_in
    }

    pub fn poll(&mut self) -> Polled {
        let mut out = Polled::default();
        if self.signed_in != self.service.signed_in() {
            self.signed_in = self.service.signed_in();
            out.changed = true;
        }
        for ev in self.service.poll() {
            out.changed = true;
            match ev {
                Event::Results { generation, page } if generation == self.generation => {
                    self.loading = false;
                    match page {
                        Ok(p) => {
                            self.results = p.samples;
                            self.tags = p.tags;
                            self.total = p.total;
                            self.error = None;
                        }
                        Err(e) => {
                            self.error = Some(e);
                            self.results.clear();
                            // Let the same query be retried.
                            self.last = None;
                        }
                    }
                }
                Event::Results { .. } => {}
                Event::Preview { uuid, result } => match result {
                    Ok(path) if self.wanted_preview.as_deref() == Some(uuid.as_str()) => {
                        self.previewing = Some(uuid);
                        out.play = Some(path);
                    }
                    Ok(_) => {}
                    Err(e) => out.message = Some(format!("Splice preview: {e}")),
                },
                Event::Cover { url, result } => {
                    self.covers.insert(url, result.ok());
                }
                Event::Waveform { uuid, bars } => {
                    if !bars.is_empty() {
                        self.waves.insert(uuid, Arc::new(bars));
                    }
                }
                Event::OpenUrl(url) => out.open_url = Some(url),
                Event::SignedIn(r) => {
                    self.signing_in = false;
                    out.message = Some(match r {
                        Ok(()) => "Signed in to Splice".into(),
                        Err(e) => e,
                    });
                }
                Event::SignedOut => out.message = Some("Signed out of Splice".into()),
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_round_trip_and_params() {
        let am = Scale::new(Key(9), ScaleKind::Minor);
        assert_eq!(splice_key(am), ("A".to_owned(), Some("minor".to_owned())));
        let s = SpliceSample {
            uuid: "u".into(),
            name: "n".into(),
            tags: vec![],
            bpm: None,
            key: Some("a".into()),
            chord_type: Some("minor".into()),
            duration: 1.,
            is_loop: true,
            pack: None,
            preview_url: None,
            waveform_url: None,
        };
        assert_eq!(scale_of(&s), Some(am));
        let p = params(" kick ", KindFilter::OneShots, Some((120, 130)), Some(am), &[], Some(9));
        assert_eq!(p.text, "kick");
        assert_eq!(p.kind, Some(AssetKind::OneShot));
        assert_eq!(p.sort, Sort::Random);
        assert_eq!(p.seed.as_deref(), Some("9"));
        assert_eq!(params("", KindFilter::All, None, None, &[], None).sort, Sort::Popularity);
    }
}
