//! The tools that only read: the project with ids, a track in full, a
//! device's parameters, the stock devices and presets, a clip's notes.

use std::fmt::Write;

use aitrack_model::{AutomationTarget, ClipId, Output, Project, Source, Track, TrackId};
use serde_json::Value;

use super::refs::{self, Res};
use crate::llm::context::kind_name;

fn track_name(p: &Project, id: TrackId) -> String {
    p.track(id).map_or_else(|| "?".to_owned(), |t| t.name.clone())
}

fn devices_line(t: &Track) -> String {
    let mut out = Vec::new();
    aitrack_model::visit_devices(&t.devices, &mut |d| {
        let what = refs::stock_id(d).map_or_else(|| "plugin".to_owned(), str::to_owned);
        out.push(format!("#{} {} ({what}{})", d.id.0, d.name, if d.enabled { "" } else { ", off" }));
    });
    if out.is_empty() { "none".to_owned() } else { out.join(" → ") }
}

fn track_head(p: &Project, t: &Track) -> String {
    let mut s = format!("#{} \"{}\" [{}] {:+.1} dB pan {:+.2}", t.id.0, t.name, kind_name(t.kind), t.volume_db, t.pan);
    for (on, w) in [(t.mute, "muted"), (t.solo, "solo")] {
        if on {
            let _ = write!(s, " {w}");
        }
    }
    match t.output {
        Output::Track(id) => {
            let _ = write!(s, " → {}", track_name(p, id));
        }
        Output::None => s.push_str(" → no output"),
        _ => {}
    }
    for snd in &t.sends {
        let _ = write!(s, ", send {:+.1} dB → {}", snd.level_db, track_name(p, snd.target));
    }
    s
}

fn span(p: &Project, t: &Track) -> Option<(f64, f64)> {
    let start = t.arrangement.iter().map(|pl| pl.start).min()?;
    let end = t.arrangement.iter().filter_map(|pl| p.clips.get(&pl.clip).map(|c| pl.start + c.length)).max()?;
    Some((refs::tick_to_bar(p, start), refs::tick_to_bar(p, end)))
}

/// `get_project`: the song and every track with ids.
pub fn project(p: &Project) -> String {
    let mut s = String::new();
    let sig = p.tempo.signature_at(0);
    let _ = writeln!(s, "\"{}\": {:.2} BPM, {}/{}, key {}", p.name, p.tempo.initial_bpm(), sig.numerator, sig.denominator, p.scale.map_or_else(|| "not set".to_owned(), |k| k.display()));
    if let Some((a, b)) = p.loop_region {
        let _ = writeln!(s, "Loop: bar {:.2} to bar {:.2}", refs::tick_to_bar(p, a), refs::tick_to_bar(p, b));
    }
    let end = p.tracks.iter().filter_map(|t| span(p, t)).map(|(_, e)| e).fold(1., f64::max);
    let _ = writeln!(s, "Song ends at bar {end:.0}. Tracks top to bottom:");
    for t in &p.tracks {
        let depth = std::iter::successors(t.parent, |g| p.track(*g).and_then(|t| t.parent)).take(8).count();
        let pad = "  ".repeat(depth);
        let _ = writeln!(s, "{pad}- {}", track_head(p, t));
        let _ = writeln!(s, "{pad}  devices: {}", devices_line(t));
        if let Some((a, b)) = span(p, t) {
            let _ = writeln!(s, "{pad}  clips: {} on the timeline, bars {a:.1}–{b:.1}", t.arrangement.len());
        }
    }
    s
}

/// `get_track`: devices with the values that differ from default, every
/// clip on the timeline, automation lanes.
pub fn track(p: &Project, id: TrackId) -> String {
    let Some(t) = p.track(id) else { return "no such track".into() };
    let mut s = track_head(p, t);
    s.push('\n');
    let mut chain = Vec::new();
    aitrack_model::visit_devices(&t.devices, &mut |d| chain.push(d.clone()));
    if chain.is_empty() {
        s.push_str("Devices: none\n");
    }
    for d in &chain {
        let _ = write!(s, "Device #{} {} ({}{})", d.id.0, d.name, refs::stock_id(d).unwrap_or("third-party plugin"), if d.enabled { "" } else { ", off" });
        if let Some(pr) = &d.preset {
            let _ = write!(s, ", preset \"{pr}\"");
        }
        if let Some(sc) = d.sidechain {
            let _ = write!(s, ", sidechain from {}", track_name(p, sc));
        }
        s.push('\n');
        if let Some(info) = refs::stock_id(d).and_then(refs::plugin_info) {
            let changed: Vec<String> = info.params.iter().filter_map(|pi| d.params.get(&pi.id).filter(|v| (**v - pi.default_normalized()).abs() > 1e-4).map(|v| format!("{} {}", pi.name, pi.format(*v)))).collect();
            if !changed.is_empty() {
                let more = changed.len().saturating_sub(24);
                let _ = writeln!(s, "  set: {}{}", changed.iter().take(24).cloned().collect::<Vec<_>>().join(", "), if more > 0 { format!(" (+{more})") } else { String::new() });
            }
        }
    }
    if !t.arrangement.is_empty() {
        s.push_str("Timeline:\n");
        let mut pls = t.arrangement.clone();
        pls.sort_by_key(|pl| pl.start);
        for pl in pls.iter().take(64) {
            if let Some(c) = p.clips.get(&pl.clip) {
                let what = match p.sources.get(&c.source) {
                    Some(Source::Notes(n)) => format!("MIDI, {} notes", n.notes.len()),
                    Some(Source::Audio(a)) => format!("audio {}", a.path.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default()),
                    None => "missing".into(),
                };
                let _ = writeln!(s, "  placement #{} clip #{} \"{}\" at bar {:.2}, {:.2} bars, {what}", pl.id.0, pl.clip.0, c.name, refs::tick_to_bar(p, pl.start), c.length as f64 / refs::bar_ticks(p) as f64);
            }
        }
        if pls.len() > 64 {
            let _ = writeln!(s, "  (+{} more)", pls.len() - 64);
        }
    }
    for lane in &t.automation {
        let what = match lane.target {
            AutomationTarget::Volume => "volume".to_owned(),
            AutomationTarget::Pan => "pan".to_owned(),
            AutomationTarget::DeviceParam { device, param } => format!("device #{} param {param}", device.0),
            other => format!("{other:?}").to_lowercase(),
        };
        let _ = writeln!(s, "Automation: {what}, {} points", lane.points.len());
    }
    s
}

/// `get_device`: every parameter with its value and range, and the presets.
pub fn device(p: &Project, input: &Value) -> Res<String> {
    let id = refs::device(p, refs::field(input, "device")?)?;
    let d = p.device(id).ok_or("no such device")?;
    let Some(plugin) = refs::stock_id(d) else { return Ok(format!("#{} {} is a third-party plugin: its parameters are not visible to the assistant.", d.id.0, d.name)) };
    let info = refs::plugin_info(plugin).ok_or("unknown stock device")?;
    let mut s = format!("#{} {} ({plugin}){}\n", d.id.0, d.name, if info.has_sidechain { ", takes a sidechain" } else { "" });
    for pi in &info.params {
        let v = d.params.get(&pi.id).copied().unwrap_or_else(|| pi.default_normalized());
        let _ = writeln!(s, "{}", refs::describe_param(pi, v));
    }
    let presets = aitrack_stock::presets::list(plugin);
    if !presets.is_empty() {
        let _ = writeln!(s, "{} factory presets (list_presets)", presets.len());
    }
    Ok(s)
}

/// `list_plugins`.
pub fn plugins(input: &Value) -> String {
    use aitrack_dsp::Category;
    let want = refs::opt_str(input, "category").map(str::to_lowercase);
    let mut s = String::new();
    for pl in aitrack_stock::registry() {
        let cat = match pl.category {
            Category::Instrument => "instrument",
            Category::MidiEffect => "midi",
            Category::Utility => "utility",
            Category::Analyzer => "analyzer",
            _ => "effect",
        };
        if want.as_deref().is_some_and(|w| w != "any" && w != cat) {
            continue;
        }
        let n = aitrack_stock::presets::list(pl.id).len();
        let _ = writeln!(s, "{} \"{}\" [{cat}] {}{}", pl.id, pl.name, pl.tags.join(", "), if n > 0 { format!(" · {n} presets") } else { String::new() });
    }
    s
}

/// `list_presets`.
pub fn presets(input: &Value) -> Res<String> {
    let pl = refs::plugin(refs::opt_str(input, "plugin").ok_or("missing \"plugin\"")?)?;
    let q = refs::opt_str(input, "query").map(str::to_lowercase);
    let all = aitrack_stock::presets::list(pl.id);
    let hits: Vec<_> = all.iter().filter(|r| q.as_ref().is_none_or(|q| r.name.to_lowercase().contains(q) || r.category.to_lowercase().contains(q))).collect();
    if hits.is_empty() {
        return Ok(format!("{} has {} presets; none match.", pl.name, all.len()));
    }
    let mut s = format!("{} presets for {}:\n", hits.len(), pl.name);
    for r in hits.iter().take(80) {
        let _ = writeln!(s, "{}/{}", r.category, r.name);
    }
    if hits.len() > 80 {
        let _ = writeln!(s, "(+{} more; pass a query)", hits.len() - 80);
    }
    Ok(s)
}

/// `get_notes`.
pub fn notes(p: &Project, input: &Value) -> Res<String> {
    let id = refs::field(input, "clip")?.as_u64().ok_or("\"clip\" is a clip id from get_track")?;
    let c = p.clips.get(&ClipId(id)).ok_or_else(|| format!("no clip #{id}"))?;
    let Some(Source::Notes(n)) = p.sources.get(&c.source) else { return Ok(format!("\"{}\" is an audio clip.", c.name)) };
    let mut s = format!("\"{}\": {:.2} bars, {} notes (pitch, start beat, length beats, velocity):\n", c.name, c.length as f64 / refs::bar_ticks(p) as f64, n.notes.len());
    for note in n.notes.iter().take(400) {
        let _ = writeln!(s, "{} {:.3} {:.3} {:.2}", note.key, refs::ticks_to_beats(note.start - c.offset), refs::ticks_to_beats(note.length), note.velocity);
    }
    if n.notes.len() > 400 {
        let _ = writeln!(s, "(+{} more)", n.notes.len() - 400);
    }
    Ok(s)
}
