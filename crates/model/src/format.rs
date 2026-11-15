//! Opening a project file: read `format_version` first, refuse files from a
//! newer AI-Track, and bring older ones up to date with a chain of
//! migrations (`MIGRATIONS[n]` takes version `n + 1` to `n + 2`).
//!
//! Only the model side lives here (text in, [`Project`] out). Writing the
//! file atomically, keeping the `.bak` and showing the error to the user is
//! the app's job (`app/src/project_io.rs`).

use crate::project::{Project, PROJECT_FORMAT_VERSION};
use serde::Deserialize;
use serde_json::Value;

/// Why a project file could not be opened.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LoadError {
    /// Not JSON, or JSON that is not a project of the version it declares.
    #[error("not an AI-Track project: {0}")]
    Malformed(String),
    /// There is no `format_version` (so the file is not one of ours).
    #[error("not an AI-Track project: it has no format_version")]
    MissingVersion,
    /// `format_version` is not a whole number from 1 up.
    #[error("not an AI-Track project: invalid format_version {0}")]
    InvalidVersion(String),
    /// Saved by a newer AI-Track. Opening it would drop what this version
    /// does not know when saving, so it is refused.
    #[error("this project was saved by a newer version of AI-Track (project format {found}, this version reads up to {supported}); update AI-Track to open it")]
    TooNew { found: u32, supported: u32 },
    /// An older format that has no migration to the current one.
    #[error("project format {found} is too old to open (this version reads {oldest} and later)")]
    TooOld { found: u32, oldest: u32 },
    /// A migration failed on this file.
    #[error("could not update the project from format {from} to {to}: {reason}")]
    Migration { from: u32, to: u32, reason: String },
}

/// One step of the migration chain: edits the raw JSON of a project saved
/// as version `n` so it reads as version `n + 1`. It does not set
/// `format_version`; [`migrate`] does after each step.
pub type Migration = fn(&mut Value) -> Result<(), String>;

/// The migrations from format 1 up to [`PROJECT_FORMAT_VERSION`]; entry `n`
/// goes from `n + 1` to `n + 2`. Format 1 is the first, so there are none
/// yet: when the format changes, bump `PROJECT_FORMAT_VERSION` and append a
/// `migrate_v1_to_v2`-style function here.
pub const MIGRATIONS: &[Migration] = &[];

/// The `format_version` a project's JSON declares, without reading the rest.
pub fn peek_format_version(json: &str) -> Result<u32, LoadError> {
    #[derive(Deserialize)]
    struct Head {
        format_version: Option<Value>,
    }
    let head: Head = serde_json::from_str(json).map_err(|e| LoadError::Malformed(e.to_string()))?;
    match head.format_version {
        None | Some(Value::Null) => Err(LoadError::MissingVersion),
        Some(Value::Number(n)) => match n.as_u64().and_then(|v| u32::try_from(v).ok()) {
            Some(v) if v >= 1 => Ok(v),
            _ => Err(LoadError::InvalidVersion(n.to_string())),
        },
        Some(other) => Err(LoadError::InvalidVersion(other.to_string())),
    }
}

/// Bring `value` (a project saved as `from`) up to `target` with `steps`.
/// `steps[n]` takes version `n + 1` to `n + 2`. Sets `format_version` after
/// every step.
pub fn migrate_with(value: &mut Value, from: u32, target: u32, steps: &[Migration]) -> Result<(), LoadError> {
    if from > target {
        return Err(LoadError::TooNew { found: from, supported: target });
    }
    if !value.is_object() {
        return Err(LoadError::Malformed("the project is not a JSON object".into()));
    }
    for v in from..target {
        let step = steps
            .get(v as usize - 1)
            .ok_or(LoadError::TooOld { found: from, oldest: target.saturating_sub(steps.len() as u32).max(1) })?;
        step(value).map_err(|reason| LoadError::Migration { from: v, to: v + 1, reason })?;
        value["format_version"] = Value::from(v + 1);
    }
    Ok(())
}

/// [`migrate_with`] along the real chain, up to [`PROJECT_FORMAT_VERSION`].
pub fn migrate(value: &mut Value, from: u32) -> Result<(), LoadError> {
    migrate_with(value, from, PROJECT_FORMAT_VERSION, MIGRATIONS)
}

/// Open project JSON: the version is read first, a newer one is refused
/// ([`LoadError::TooNew`]) and an older one is migrated before it becomes a
/// [`Project`]. A file of the current version is read in one pass.
pub(crate) fn open(json: &str) -> Result<Project, LoadError> {
    let version = peek_format_version(json)?;
    if version > PROJECT_FORMAT_VERSION {
        return Err(LoadError::TooNew { found: version, supported: PROJECT_FORMAT_VERSION });
    }
    let malformed = |e: serde_json::Error| LoadError::Malformed(e.to_string());
    let mut project: Project = if version == PROJECT_FORMAT_VERSION {
        serde_json::from_str(json).map_err(malformed)?
    } else {
        let mut value: Value = serde_json::from_str(json).map_err(malformed)?;
        migrate(&mut value, version)?;
        serde_json::from_value(value).map_err(malformed)?
    };
    project.format_version = PROJECT_FORMAT_VERSION;
    Ok(project)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn json_with_version(v: &str) -> String {
        let json = Project::new("t").to_json().unwrap();
        let mut value: Value = serde_json::from_str(&json).unwrap();
        value["format_version"] = serde_json::from_str(v).unwrap();
        serde_json::to_string(&value).unwrap()
    }

    #[test]
    fn a_newer_format_does_not_open() {
        let json = json_with_version("99");
        assert_eq!(Project::open_json(&json), Err(LoadError::TooNew { found: 99, supported: PROJECT_FORMAT_VERSION }));
        // The plain reader refuses it too, so no caller can drop its fields by saving.
        let err = Project::from_json(&json).unwrap_err().to_string();
        assert!(err.contains("newer version of AI-Track"), "{err}");
        // A version one above the current one is already too new.
        let next = PROJECT_FORMAT_VERSION + 1;
        assert!(matches!(Project::open_json(&json_with_version(&next.to_string())), Err(LoadError::TooNew { .. })));
        assert!(LoadError::TooNew { found: 99, supported: 1 }.to_string().contains("99"));
    }

    #[test]
    fn the_current_format_opens_and_keeps_ids_monotonic() {
        let mut p = Project::new("t");
        let id = p.alloc_track_id();
        p.tracks.push(crate::Track::new(id, crate::TrackKind::Audio, "A", crate::Color::palette(0)));
        let mut back = Project::open_json(&p.to_json().unwrap()).unwrap();
        assert_eq!(back.tracks, p.tracks);
        assert!(back.alloc_track_id().0 > id.0);
        assert_eq!(peek_format_version(&p.to_json().unwrap()), Ok(PROJECT_FORMAT_VERSION));
    }

    #[test]
    fn a_missing_or_invalid_version_is_a_typed_error() {
        let mut value: Value = serde_json::from_str(&Project::new("t").to_json().unwrap()).unwrap();
        value.as_object_mut().unwrap().remove("format_version");
        assert_eq!(Project::open_json(&value.to_string()), Err(LoadError::MissingVersion));
        for bad in ["\"2\"", "-1", "0", "1.5", "true", "4294967296"] {
            assert!(matches!(Project::open_json(&json_with_version(bad)), Err(LoadError::InvalidVersion(_))), "{bad}");
        }
        assert!(matches!(Project::open_json("not json"), Err(LoadError::Malformed(_))));
        assert!(matches!(Project::open_json("[1,2]"), Err(LoadError::Malformed(_))));
        // The right version but the wrong shape.
        assert!(matches!(Project::open_json(r#"{"format_version":1}"#), Err(LoadError::Malformed(_))));
    }

    fn v1_to_v2(v: &mut Value) -> Result<(), String> {
        // Pretend v1 called the project "title".
        let obj = v.as_object_mut().ok_or("not an object")?;
        let title = obj.remove("title").ok_or("no title")?;
        obj.insert("name".into(), title);
        Ok(())
    }

    fn v2_to_v3(v: &mut Value) -> Result<(), String> {
        let obj = v.as_object_mut().ok_or("not an object")?;
        let name = obj.get("name").and_then(Value::as_str).ok_or("no name")?.to_owned();
        obj.insert("name".into(), format!("{name} (v3)").into());
        Ok(())
    }

    #[test]
    fn migrations_run_in_order_up_to_the_target() {
        let steps: &[Migration] = &[v1_to_v2, v2_to_v3];
        let mut v: Value = serde_json::json!({ "format_version": 1, "title": "Song" });
        migrate_with(&mut v, 1, 3, steps).unwrap();
        assert_eq!(v, serde_json::json!({ "format_version": 3, "name": "Song (v3)" }));
        // From the middle of the chain only the later steps run.
        let mut v: Value = serde_json::json!({ "format_version": 2, "name": "Song" });
        migrate_with(&mut v, 2, 3, steps).unwrap();
        assert_eq!(v["name"], "Song (v3)");
        // Nothing to do at the target.
        let mut v: Value = serde_json::json!({ "format_version": 3, "name": "Song" });
        migrate_with(&mut v, 3, 3, steps).unwrap();
        assert_eq!(v["name"], "Song");
        // A failing step names the versions it was crossing.
        let mut v: Value = serde_json::json!({ "format_version": 1 });
        let err = migrate_with(&mut v, 1, 3, steps).unwrap_err();
        assert_eq!(err, LoadError::Migration { from: 1, to: 2, reason: "no title".into() });
        // A file newer than the target, or older than the chain starts, is refused.
        assert_eq!(migrate_with(&mut v, 4, 3, steps), Err(LoadError::TooNew { found: 4, supported: 3 }));
        let mut v: Value = serde_json::json!({ "format_version": 2, "name": "Song" });
        assert!(matches!(migrate_with(&mut v, 2, 5, steps), Err(LoadError::TooOld { found: 2, .. })));
    }

    #[test]
    fn the_real_chain_reaches_the_current_version() {
        assert_eq!(MIGRATIONS.len() as u32 + 1, PROJECT_FORMAT_VERSION, "one migration per format bump");
    }
}
