//! Building the GraphQL request for a sample search.
//!
//! Splice's web app searches with an `assetsSearch` query on its public
//! GraphQL endpoint. The query text here is our own and asks only for the
//! fields the palette shows.

use serde_json::{Value, json};

/// The public GraphQL endpoint of Splice's web app.
pub const GRAPHQL_URL: &str = "https://surfaces-graphql.splice.com/graphql";

/// Results per page (Splice's web app uses 50).
pub const DEFAULT_LIMIT: u32 = 50;

/// What the results are ordered by.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Sort {
    /// Best match for the text first.
    #[default]
    Relevance,
    Popularity,
    Recency,
    /// A shuffled order, stable for one [`SearchParams::seed`].
    Random,
    Bpm,
    Key,
    Duration,
    Name,
}

impl Sort {
    pub fn slug(self) -> &'static str {
        match self {
            Sort::Relevance => "relevance",
            Sort::Popularity => "popularity",
            Sort::Recency => "recency",
            Sort::Random => "random",
            Sort::Bpm => "bpm",
            Sort::Key => "key",
            Sort::Duration => "duration",
            Sort::Name => "name",
        }
    }
}

/// Loops or one-shots (`asset_category_slug`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AssetKind {
    Loop,
    OneShot,
}

impl AssetKind {
    pub fn slug(self) -> &'static str {
        match self {
            AssetKind::Loop => "loop",
            AssetKind::OneShot => "oneshot",
        }
    }
}

/// One page of a sample search.
#[derive(Clone, Debug, PartialEq)]
pub struct SearchParams {
    pub text: String,
    pub page: u32,
    pub limit: u32,
    pub sort: Sort,
    pub ascending: bool,
    pub kind: Option<AssetKind>,
    /// Inclusive BPM range.
    pub bpm: Option<(u32, u32)>,
    /// Root as Splice writes it ("C", "C#" … "B") and `major`/`minor`.
    pub key: Option<(String, Option<String>)>,
    /// Tag uuids (from a previous page's tag summary).
    pub tags: Vec<String>,
    /// Seed for [`Sort::Random`].
    pub seed: Option<String>,
}

impl Default for SearchParams {
    fn default() -> Self {
        Self { text: String::new(), page: 1, limit: DEFAULT_LIMIT, sort: Sort::Relevance, ascending: false, kind: None, bpm: None, key: None, tags: Vec::new(), seed: None }
    }
}

const SEARCH_QUERY: &str = "query SamplesSearch($query: String, $order: SortOrder = DESC, $sort: AssetSortType = relevance, \
$random_seed: String, $tags: [ID], $key: String, $chord_type: String, $min_bpm: Int, $max_bpm: Int, $limit: Int = 50, \
$asset_category_slug: AssetCategorySlug, $page: Int = 1) {
  assetsSearch(
    filter: {legacy: true, published: true, asset_type_slug: sample, query: $query, tag_ids: $tags, key: $key, chord_type: $chord_type, min_bpm: $min_bpm, max_bpm: $max_bpm, asset_category_slug: $asset_category_slug}
    pagination: {page: $page, limit: $limit}
    sort: {sort: $sort, order: $order, random_seed: $random_seed}
  ) {
    items {
      ... on IAsset { uuid name tags { uuid label } files { uuid name asset_file_type_slug url } }
      ... on IAssetChild { parents(filter: {asset_type_slug: pack}) { items { ... on PackAsset { uuid name permalink_slug permalink_base_url files { asset_file_type_slug url } } } } }
      ... on SampleAsset { bpm key chord_type duration asset_category_slug }
    }
    tag_summary { count tag { uuid label } }
    pagination_metadata { currentPage totalPages }
    response_metadata { records }
  }
}";

/// Splice's root spelling for a pitch class (sharps, as its key filter uses).
pub fn key_root(pitch_class: u8) -> &'static str {
    ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"][(pitch_class % 12) as usize]
}

/// The JSON body to POST to [`GRAPHQL_URL`].
pub fn request_body(p: &SearchParams) -> Value {
    let text = p.text.trim();
    let (min_bpm, max_bpm) = match p.bpm {
        Some((a, b)) => (Some(a.min(b)), Some(a.max(b))),
        None => (None, None),
    };
    let (key, chord) = match &p.key {
        Some((k, c)) => (Some(k.clone()), c.clone()),
        None => (None, None),
    };
    json!({
        "operationName": "SamplesSearch",
        "query": SEARCH_QUERY,
        "variables": {
            "query": if text.is_empty() { Value::Null } else { Value::String(text.to_owned()) },
            "page": p.page.max(1),
            "limit": p.limit.clamp(1, 100),
            "sort": p.sort.slug(),
            "order": if p.ascending { "ASC" } else { "DESC" },
            "random_seed": if p.sort == Sort::Random { p.seed.clone().map_or(Value::Null, Value::String) } else { Value::Null },
            "tags": p.tags,
            "key": key,
            "chord_type": chord,
            "min_bpm": min_bpm,
            "max_bpm": max_bpm,
            "asset_category_slug": p.kind.map(AssetKind::slug),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_text_and_filters_are_null() {
        let b = request_body(&SearchParams::default());
        let v = &b["variables"];
        assert_eq!(b["operationName"], "SamplesSearch");
        assert!(v["query"].is_null());
        assert!(v["key"].is_null() && v["min_bpm"].is_null() && v["asset_category_slug"].is_null());
        assert_eq!(v["sort"], "relevance");
        assert_eq!(v["order"], "DESC");
        assert_eq!(v["limit"], 50);
        assert!(b["query"].as_str().unwrap().contains("assetsSearch"));
    }

    #[test]
    fn filters_map_to_variables() {
        let p = SearchParams {
            text: "  dark kick ".into(),
            page: 3,
            limit: 500,
            sort: Sort::Bpm,
            ascending: true,
            kind: Some(AssetKind::OneShot),
            bpm: Some((140, 120)),
            key: Some(("A".into(), Some("minor".into()))),
            tags: vec!["t1".into()],
            seed: Some("7".into()),
        };
        let b = request_body(&p);
        let v = &b["variables"];
        assert_eq!(v["query"], "dark kick");
        assert_eq!(v["page"], 3);
        assert_eq!(v["limit"], 100, "clamped");
        assert_eq!(v["sort"], "bpm");
        assert_eq!(v["order"], "ASC");
        assert_eq!(v["asset_category_slug"], "oneshot");
        assert_eq!((v["min_bpm"].as_u64(), v["max_bpm"].as_u64()), (Some(120), Some(140)));
        assert_eq!(v["key"], "A");
        assert_eq!(v["chord_type"], "minor");
        assert_eq!(v["tags"][0], "t1");
        assert!(v["random_seed"].is_null(), "the seed only goes with random order");
    }

    #[test]
    fn shuffle_sends_its_seed() {
        let p = SearchParams { sort: Sort::Random, seed: Some("42".into()), ..Default::default() };
        assert_eq!(request_body(&p)["variables"]["random_seed"], "42");
        assert_eq!(key_root(13), "C#");
    }
}
