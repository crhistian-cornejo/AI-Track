//! Finding what a tool call names: tracks by id or name, devices, stock
//! parameters by name, bars and beats as ticks.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use aitrack_dsp::{ParamInfo, ParamScale, PluginInfo};
use aitrack_model::{Device, DeviceId, DeviceKind, Project, Tick, TrackId, TrackKind, PPQ};
use serde_json::Value;

pub type Res<T> = Result<T, String>;

/// Ticks in one bar (the signature at the start of the song).
pub fn bar_ticks(p: &Project) -> Tick {
    p.tempo.signature_at(0).bar_ticks().max(1)
}

/// Bar numbers start at 1; `1.5` is half way through bar 1.
pub fn bar_to_tick(p: &Project, bar: f64) -> Tick {
    ((bar.max(1.) - 1.) * bar_ticks(p) as f64).round() as Tick
}

pub fn tick_to_bar(p: &Project, tick: Tick) -> f64 {
    tick as f64 / bar_ticks(p) as f64 + 1.
}

pub fn beats_to_ticks(beats: f64) -> Tick {
    (beats * PPQ as f64).round() as Tick
}

pub fn ticks_to_beats(ticks: Tick) -> f64 {
    ticks as f64 / PPQ as f64
}

/// A required field.
pub fn field<'a>(input: &'a Value, name: &str) -> Res<&'a Value> {
    input.get(name).filter(|v| !v.is_null()).ok_or_else(|| format!("missing \"{name}\""))
}

pub fn num(input: &Value, name: &str) -> Res<f64> {
    field(input, name)?.as_f64().ok_or_else(|| format!("\"{name}\" must be a number"))
}

pub fn opt_num(input: &Value, name: &str) -> Option<f64> {
    input.get(name).and_then(Value::as_f64)
}

pub fn opt_str<'a>(input: &'a Value, name: &str) -> Option<&'a str> {
    input.get(name).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty())
}

pub fn opt_bool(input: &Value, name: &str) -> Option<bool> {
    input.get(name).and_then(Value::as_bool)
}

fn norm(s: &str) -> String {
    s.trim().to_lowercase()
}

/// A track named by `v`: its id (number or `"#12"`), its name, or
/// `"selected"` / `"main"`.
pub fn track(p: &Project, v: &Value, selected: Option<TrackId>) -> Res<TrackId> {
    if let Some(n) = v.as_u64() {
        return p.track(TrackId(n)).map(|t| t.id).ok_or_else(|| format!("no track #{n}"));
    }
    let s = v.as_str().ok_or("a track is its id or its name")?;
    let n = norm(s);
    let digits = n.trim_start_matches('#');
    if let Ok(id) = digits.parse::<u64>()
        && p.track(TrackId(id)).is_some()
    {
        return Ok(TrackId(id));
    }
    match n.as_str() {
        "selected" | "this" | "this track" => return selected.filter(|id| p.track(*id).is_some()).ok_or_else(|| "no track is selected".to_owned()),
        "main" | "master" | "master bus" | "main bus" => return Ok(p.main_track().id),
        _ => {}
    }
    if let Some(t) = p.tracks.iter().find(|t| norm(&t.name) == n) {
        return Ok(t.id);
    }
    let hits: Vec<_> = p.tracks.iter().filter(|t| norm(&t.name).contains(&n)).collect();
    match hits.as_slice() {
        [one] => Ok(one.id),
        [] => Err(format!("no track called \"{s}\"; call get_project for the names")),
        many => Err(format!("\"{s}\" matches {}: use the id", many.iter().map(|t| format!("#{} {}", t.id.0, t.name)).collect::<Vec<_>>().join(", "))),
    }
}

pub fn track_field(p: &Project, input: &Value, name: &str, selected: Option<TrackId>) -> Res<TrackId> {
    track(p, field(input, name)?, selected)
}

/// A device by id (number or `"#12"`).
pub fn device(p: &Project, v: &Value) -> Res<DeviceId> {
    let id = v.as_u64().or_else(|| v.as_str().and_then(|s| s.trim().trim_start_matches('#').parse().ok())).ok_or("a device is its id (from get_track)")?;
    p.device(DeviceId(id)).map(|d| d.id).ok_or_else(|| format!("no device #{id}"))
}

pub fn stock_id(d: &Device) -> Option<&str> {
    match &d.kind {
        DeviceKind::Stock { plugin_id } => Some(plugin_id),
        _ => None,
    }
}

/// A stock plugin's info, made once per plugin.
pub fn plugin_info(id: &str) -> Option<Arc<PluginInfo>> {
    static CACHE: OnceLock<Mutex<HashMap<String, Arc<PluginInfo>>>> = OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    if let Some(i) = cache.lock().ok()?.get(id) {
        return Some(i.clone());
    }
    let info = Arc::new(aitrack_stock::create(id)?.info().clone());
    cache.lock().ok()?.insert(id.to_owned(), info.clone());
    Some(info)
}

/// A stock plugin by id (`fx.eq`) or by name (`EQ`, `reverb`).
pub fn plugin(name: &str) -> Res<&'static aitrack_stock::StockPlugin> {
    let n = norm(name);
    let reg = aitrack_stock::registry();
    reg.iter()
        .find(|p| p.id == n || norm(p.name) == n)
        .or_else(|| reg.iter().find(|p| p.id.rsplit('.').next() == Some(n.as_str())))
        .or_else(|| {
            let hits: Vec<_> = reg.iter().filter(|p| norm(p.name).contains(&n) || p.id.contains(&n)).collect();
            (hits.len() == 1).then(|| hits[0])
        })
        .ok_or_else(|| format!("no stock device \"{name}\"; call list_plugins"))
}

/// A parameter by its id or name (full or short, any case). Names like
/// "Band 2 Gain" also match "band2 gain".
pub fn param<'a>(info: &'a PluginInfo, key: &str) -> Res<&'a ParamInfo> {
    let k = norm(key);
    if let Ok(id) = k.parse::<u32>()
        && let Some(p) = info.params.iter().find(|p| p.id == id)
    {
        return Ok(p);
    }
    let squash = |s: &str| s.chars().filter(|c| c.is_alphanumeric()).collect::<String>().to_lowercase();
    let ks = squash(key);
    info.params
        .iter()
        .find(|p| norm(p.name) == k || norm(p.short) == k)
        .or_else(|| info.params.iter().find(|p| squash(p.name) == ks || squash(p.short) == ks))
        .ok_or_else(|| format!("{} has no parameter \"{key}\"; call get_device for the list", info.name))
}

/// The normalised value for `v`: a number in the parameter's own units, or
/// a choice label for stepped parameters ("Bell", "Low Cut").
pub fn param_value(p: &ParamInfo, v: &Value) -> Res<f32> {
    if let Some(n) = v.as_f64() {
        return Ok(p.to_normalized(n as f32));
    }
    if let Some(b) = v.as_bool() {
        return Ok(p.to_normalized(if b { p.max } else { p.min }));
    }
    let s = v.as_str().ok_or_else(|| format!("{}: give a number in {} or a choice", p.name, if p.unit.is_empty() { "its units" } else { p.unit }))?;
    if let Some(i) = p.choices.iter().position(|c| norm(c) == norm(s)) {
        return Ok(p.to_normalized(p.min + i as f32));
    }
    if let Ok(n) = s.trim().trim_end_matches(p.unit).trim().parse::<f32>() {
        return Ok(p.to_normalized(n));
    }
    Err(format!("{}: \"{s}\" is not one of {}", p.name, if p.choices.is_empty() { "its values".to_owned() } else { p.choices.join(", ") }))
}

/// One line about a parameter: `12 Freq = 1.20 kHz (20 – 20000 Hz, log)`.
pub fn describe_param(p: &ParamInfo, normalized: f32) -> String {
    let range = if p.choices.is_empty() {
        format!("{} – {}{}", trim(p.min), trim(p.max), if p.unit.is_empty() { String::new() } else { format!(" {}", p.unit) })
    } else {
        p.choices.join("|")
    };
    let scale = match p.scale {
        ParamScale::Log => ", log",
        _ => "",
    };
    format!("{} {} = {} ({range}{scale})", p.id, p.name, p.format(normalized))
}

fn trim(v: f32) -> String {
    if v.fract() == 0. { format!("{v:.0}") } else { format!("{v}") }
}

/// The track kinds the tools create.
pub fn track_kind(s: &str) -> Res<TrackKind> {
    match norm(s).as_str() {
        "midi" | "instrument" => Ok(TrackKind::Instrument),
        "audio" => Ok(TrackKind::Audio),
        "group" | "bus" => Ok(TrackKind::Group),
        "return" | "fx" | "send" => Ok(TrackKind::Return),
        other => Err(format!("unknown track kind \"{other}\" (midi, audio, group, return)")),
    }
}

/// Where a new track goes: before the returns, VCAs and Main.
pub fn insert_index(p: &Project, kind: TrackKind) -> usize {
    let tail = |k: TrackKind| matches!(k, TrackKind::Return | TrackKind::Vca | TrackKind::Main);
    if kind == TrackKind::Return {
        return p.tracks.iter().position(|t| matches!(t.kind, TrackKind::Vca | TrackKind::Main)).unwrap_or(p.tracks.len());
    }
    p.tracks.iter().position(|t| tail(t.kind)).unwrap_or(p.tracks.len())
}

/// The fader's normalised automation value for `db` (as the engine reads it).
pub fn volume_normalized(db: f32) -> f32 {
    const MIN: f32 = -70.;
    const MAX: f32 = 6.;
    ((db - MIN) / (MAX - MIN)).clamp(0., 1.)
}

#[cfg(test)]
mod tests {
    use super::*;
    use aitrack_model::{Color, Track};
    use serde_json::json;

    #[test]
    fn tracks_by_id_name_and_alias() {
        let mut p = Project::new("t");
        let id = p.alloc_track_id();
        p.tracks.insert(0, Track::new(id, TrackKind::Instrument, "Kick 909", Color::palette(0)));
        assert_eq!(track(&p, &json!(id.0), None).unwrap(), id);
        assert_eq!(track(&p, &json!(format!("#{}", id.0)), None).unwrap(), id);
        assert_eq!(track(&p, &json!("kick 909"), None).unwrap(), id);
        assert_eq!(track(&p, &json!("kick"), None).unwrap(), id);
        assert_eq!(track(&p, &json!("selected"), Some(id)).unwrap(), id);
        assert_eq!(track(&p, &json!("master"), None).unwrap(), p.main_track().id);
        assert!(track(&p, &json!("snare"), None).is_err());
    }

    #[test]
    fn parameters_by_name_and_values_by_unit_or_choice() {
        let info = plugin_info(aitrack_stock::fx::compressor::ID).unwrap();
        let first = &info.params[0];
        assert_eq!(param(&info, first.name).unwrap().id, first.id);
        assert_eq!(param(&info, &first.id.to_string()).unwrap().id, first.id);
        assert!(param(&info, "no such knob").is_err());
        let v = param_value(first, &json!(first.max)).unwrap();
        assert!((v - 1.).abs() < 1e-4 || first.max < first.min);
        assert_eq!(plugin("Compressor").unwrap().id, aitrack_stock::fx::compressor::ID);
        assert_eq!(plugin("fx.eq").unwrap().id, aitrack_stock::fx::eq::ID);
    }

    #[test]
    fn bars_are_one_based() {
        let p = Project::new("t");
        assert_eq!(bar_to_tick(&p, 1.), 0);
        assert_eq!(bar_to_tick(&p, 3.), 2 * bar_ticks(&p));
        assert!((tick_to_bar(&p, bar_ticks(&p)) - 2.).abs() < 1e-9);
    }
}
