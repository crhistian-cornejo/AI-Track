//! Parsing a search response into the rows the palette lists.

use serde::Deserialize;

use crate::{Error, Result};

/// One Splice sample, flattened for display.
#[derive(Clone, Debug, PartialEq)]
pub struct SpliceSample {
    pub uuid: String,
    /// File name without folders ("Kick_Dark_01.wav").
    pub name: String,
    pub tags: Vec<String>,
    pub bpm: Option<f32>,
    /// "A", "C#"… as Splice writes it.
    pub key: Option<String>,
    /// "major" or "minor".
    pub chord_type: Option<String>,
    /// Seconds.
    pub duration: f32,
    pub is_loop: bool,
    pub pack: Option<PackRef>,
    /// The (scrambled) MP3 preview; see [`crate::descramble`].
    pub preview_url: Option<String>,
    /// Waveform outline (JSON numbers, sometimes zlib-compressed).
    pub waveform_url: Option<String>,
}

impl SpliceSample {
    /// "Am", "C#", or `None`.
    pub fn short_key(&self) -> Option<String> {
        let k = self.key.as_deref()?;
        let minor = self.chord_type.as_deref().is_some_and(|c| c.eq_ignore_ascii_case("minor"));
        Some(format!("{}{}", k.to_uppercase(), if minor { "m" } else { "" }))
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct PackRef {
    pub uuid: String,
    pub name: String,
    pub cover_url: Option<String>,
    /// The pack's page on splice.com.
    pub page_url: Option<String>,
}

/// A tag Splice can filter by, with how many results carry it.
#[derive(Clone, Debug, PartialEq)]
pub struct TagCount {
    pub uuid: String,
    pub label: String,
    pub count: u64,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct SearchPage {
    pub samples: Vec<SpliceSample>,
    pub tags: Vec<TagCount>,
    pub total: u64,
    pub page: u32,
    pub pages: u32,
}

// ── Wire format ──

#[derive(Deserialize)]
struct Envelope {
    data: Option<Data>,
    #[serde(default)]
    errors: Vec<GqlError>,
}

#[derive(Deserialize)]
struct GqlError {
    message: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Data {
    assets_search: Option<AssetPage>,
}

#[derive(Deserialize)]
struct AssetPage {
    #[serde(default)]
    items: Vec<Asset>,
    #[serde(default)]
    tag_summary: Vec<TagSummary>,
    pagination_metadata: Option<Pagination>,
    response_metadata: Option<Records>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Pagination {
    current_page: Option<u32>,
    total_pages: Option<u32>,
}

#[derive(Deserialize)]
struct Records {
    records: Option<u64>,
}

#[derive(Deserialize)]
struct TagSummary {
    count: Option<u64>,
    tag: Option<Tag>,
}

#[derive(Deserialize)]
struct Tag {
    uuid: String,
    label: String,
}

#[derive(Deserialize)]
struct File {
    asset_file_type_slug: Option<String>,
    url: Option<String>,
}

#[derive(Deserialize)]
struct Parents {
    #[serde(default)]
    items: Vec<Pack>,
}

#[derive(Deserialize)]
struct Pack {
    uuid: Option<String>,
    name: Option<String>,
    permalink_slug: Option<String>,
    permalink_base_url: Option<String>,
    #[serde(default)]
    files: Vec<File>,
}

#[derive(Deserialize)]
struct Asset {
    uuid: Option<String>,
    name: Option<String>,
    #[serde(default)]
    tags: Vec<Tag>,
    #[serde(default)]
    files: Vec<File>,
    parents: Option<Parents>,
    bpm: Option<f32>,
    key: Option<String>,
    chord_type: Option<String>,
    /// Milliseconds.
    duration: Option<f64>,
    asset_category_slug: Option<String>,
}

/// The file of a kind, else the one at `fallback` (the web app's order:
/// preview first, waveform second; a pack's cover first).
fn file_url(files: &[File], slugs: &[&str], fallback: usize) -> Option<String> {
    let by_slug = files.iter().find(|f| f.asset_file_type_slug.as_deref().is_some_and(|s| slugs.iter().any(|w| s.contains(w))));
    by_slug.or_else(|| files.get(fallback)).and_then(|f| f.url.clone()).filter(|u| u.starts_with("https://"))
}

fn file_name(name: &str) -> String {
    name.rsplit(['/', '\\']).next().unwrap_or(name).to_owned()
}

fn pack(p: Pack) -> Option<PackRef> {
    let page_url = match (&p.permalink_base_url, &p.permalink_slug) {
        (Some(base), Some(slug)) if base.starts_with("https://") => Some(format!("{}/{}", base.trim_end_matches('/'), slug)),
        _ => None,
    };
    Some(PackRef { uuid: p.uuid?, name: p.name.unwrap_or_default(), cover_url: file_url(&p.files, &["cover", "image"], 0), page_url })
}

fn sample(a: Asset) -> Option<SpliceSample> {
    let uuid = a.uuid?;
    let name = file_name(a.name.as_deref().unwrap_or(""));
    Some(SpliceSample {
        preview_url: file_url(&a.files, &["preview"], 0),
        waveform_url: file_url(&a.files, &["waveform"], 1),
        uuid,
        name,
        tags: a.tags.into_iter().map(|t| t.label).collect(),
        bpm: a.bpm.filter(|b| *b > 0.),
        key: a.key.filter(|k| !k.is_empty()),
        chord_type: a.chord_type.filter(|c| !c.is_empty()),
        duration: (a.duration.unwrap_or(0.) / 1000.) as f32,
        is_loop: a.asset_category_slug.as_deref() == Some("loop"),
        pack: a.parents.and_then(|p| p.items.into_iter().next()).and_then(pack),
    })
}

/// Parse a `SamplesSearch` response body.
pub fn parse_search(body: &str) -> Result<SearchPage> {
    let env: Envelope = serde_json::from_str(body).map_err(|e| Error::Parse(e.to_string()))?;
    if let Some(e) = env.errors.first() {
        return Err(Error::Api(e.message.clone()));
    }
    let page = env.data.and_then(|d| d.assets_search).ok_or_else(|| Error::Parse("no assetsSearch in the response".into()))?;
    let mut tags: Vec<TagCount> = page
        .tag_summary
        .into_iter()
        .filter_map(|t| {
            let tag = t.tag?;
            Some(TagCount { uuid: tag.uuid, label: tag.label, count: t.count.unwrap_or(0) })
        })
        .collect();
    tags.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.label.cmp(&b.label)));
    let (cur, pages) = page.pagination_metadata.map_or((1, 1), |p| (p.current_page.unwrap_or(1), p.total_pages.unwrap_or(1)));
    let samples: Vec<SpliceSample> = page.items.into_iter().filter_map(sample).collect();
    let total = page.response_metadata.and_then(|r| r.records).unwrap_or(samples.len() as u64);
    Ok(SearchPage { samples, tags, total, page: cur, pages })
}

/// A waveform outline (Splice sends ~800 numbers) reduced to `n` bars in 0..=1.
pub fn waveform_bars(values: &[f64], n: usize) -> Vec<f32> {
    if values.is_empty() || n == 0 {
        return Vec::new();
    }
    let max = values.iter().fold(0f64, |m, v| m.max(v.abs())).max(1e-9);
    (0..n)
        .map(|i| {
            let a = i * values.len() / n;
            let b = ((i + 1) * values.len() / n).max(a + 1).min(values.len());
            (values[a..b].iter().fold(0f64, |m, v| m.max(v.abs())) / max) as f32
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = include_str!("../fixtures/samples_search.json");

    #[test]
    fn parses_samples_tags_and_paging() {
        let page = parse_search(FIXTURE).unwrap();
        assert_eq!(page.total, 1234);
        assert_eq!((page.page, page.pages), (1, 25));
        assert_eq!(page.samples.len(), 2);
        let loop_ = &page.samples[0];
        assert_eq!(loop_.name, "LV_dark_bass_loop_124_Am.wav");
        assert!(loop_.is_loop);
        assert_eq!(loop_.bpm, Some(124.));
        assert_eq!(loop_.short_key().as_deref(), Some("Am"));
        assert!((loop_.duration - 7.742).abs() < 1e-3);
        assert_eq!(loop_.tags, vec!["bass", "dark"]);
        assert_eq!(loop_.preview_url.as_deref(), Some("https://spliceproduction.s3.amazonaws.com/audio_samples/preview.mp3"));
        assert_eq!(loop_.waveform_url.as_deref(), Some("https://spliceproduction.s3.amazonaws.com/audio_samples/waveform.json"));
        let pack = loop_.pack.as_ref().unwrap();
        assert_eq!(pack.name, "Late Night Bass");
        assert_eq!(pack.cover_url.as_deref(), Some("https://spliceproduction.s3.amazonaws.com/packs/cover.jpg"));
        assert_eq!(pack.page_url.as_deref(), Some("https://splice.com/sounds/packs/late-night-bass"));

        let shot = &page.samples[1];
        assert!(!shot.is_loop);
        assert_eq!(shot.bpm, None, "zero BPM means none");
        assert_eq!(shot.short_key(), None);
        assert!(shot.pack.is_none());
        // Most-used tags first.
        assert_eq!(page.tags.iter().map(|t| t.label.as_str()).collect::<Vec<_>>(), vec!["bass", "dark", "kick"]);
    }

    #[test]
    fn graphql_errors_and_garbage_are_errors() {
        assert!(matches!(parse_search(r#"{"errors":[{"message":"rate limited"}]}"#), Err(Error::Api(m)) if m == "rate limited"));
        assert!(matches!(parse_search("<html>"), Err(Error::Parse(_))));
        assert!(matches!(parse_search(r#"{"data":{}}"#), Err(Error::Parse(_))));
    }

    #[test]
    fn non_https_urls_are_dropped() {
        let body = r#"{"data":{"assetsSearch":{"items":[{"uuid":"u","name":"a.wav","files":[{"asset_file_type_slug":"preview_mp3","url":"http://evil/x.mp3"}]}]}}}"#;
        assert_eq!(parse_search(body).unwrap().samples[0].preview_url, None);
    }

    #[test]
    fn waveform_reduces_to_bars() {
        let v: Vec<f64> = (0..800).map(|i| if i < 400 { 10. } else { 20. }).collect();
        let bars = waveform_bars(&v, 4);
        assert_eq!(bars, vec![0.5, 0.5, 1., 1.]);
        assert!(waveform_bars(&[], 4).is_empty());
    }
}
