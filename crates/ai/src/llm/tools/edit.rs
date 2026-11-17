//! The tools that edit. Each builds `Command`s against the session's draft,
//! applies them there (so the next call sees the change) and records them
//! for the proposal the turn ends with.

use aitrack_model::command::{DeviceSlot, TrackProp};
use aitrack_model::{
    AutomationLane, AutomationPoint, AutomationTarget, Clip, Color, Command, Device, Key, Note, NoteSource, Output, Placement, Scale, ScaleKind, Send, Source,
    Track, TrackId, TrackKind,
};
use serde_json::Value;

use super::Session;
use super::refs::{self, Res};

fn color(s: &Session, input: &Value) -> Color {
    let n = refs::opt_num(input, "color").map_or(s.draft.tracks.len(), |v| v.max(0.) as usize);
    Color::palette(n)
}

/// A stock device carrying `params` (by name) and an optional preset.
fn make_device(s: &mut Session, plugin: &str, input: &Value) -> Res<(Device, String)> {
    let pl = refs::plugin(plugin)?;
    let mut d = Device::stock(s.draft.alloc_device_id(), pl.id, pl.name);
    let mut what = pl.name.to_owned();
    if let Some(name) = refs::opt_str(input, "preset") {
        let (pname, values) = preset(pl.id, name)?;
        d.params.extend(values);
        d.preset = Some(pname.clone());
        what = format!("{} ({pname})", pl.name);
    }
    if let Some(obj) = input.get("params").and_then(Value::as_object) {
        let info = refs::plugin_info(pl.id).ok_or("unknown stock device")?;
        for (k, v) in obj {
            let pi = refs::param(&info, k)?;
            d.params.insert(pi.id, refs::param_value(pi, v)?);
        }
    }
    Ok((d, what))
}

/// A factory preset of `plugin` by name or `Category/Name`.
fn preset(plugin: &str, name: &str) -> Res<(String, Vec<(u32, f32)>)> {
    let n = name.trim().trim_end_matches(".json").to_lowercase();
    let list = aitrack_stock::presets::list(plugin);
    let hit = list
        .iter()
        .find(|r| r.name.to_lowercase() == n || format!("{}/{}", r.category, r.name).to_lowercase() == n)
        .or_else(|| {
            let hits: Vec<_> = list.iter().filter(|r| r.name.to_lowercase().contains(&n)).collect();
            (hits.len() == 1).then(|| hits[0])
        })
        .ok_or_else(|| format!("no preset \"{name}\" for {plugin}; call list_presets"))?;
    let p = aitrack_stock::presets::load(&hit.path).map_err(|e| e.to_string())?;
    let values = p.params.iter().filter_map(|(k, v)| Some((k.parse::<u32>().ok()?, v.clamp(0., 1.)))).collect();
    Ok((p.name, values))
}

pub fn create_track(s: &mut Session, input: &Value) -> Res<String> {
    let name = refs::opt_str(input, "name").ok_or("missing \"name\"")?.to_owned();
    let kind = refs::track_kind(refs::opt_str(input, "kind").unwrap_or("midi"))?;
    let id = s.draft.alloc_track_id();
    let mut t = Track::new(id, kind, &name, color(s, input));
    let mut what = String::new();
    if kind == TrackKind::Instrument {
        let plugin = refs::opt_str(input, "instrument").unwrap_or(aitrack_stock::instrument::wavetable::ID);
        let (d, w) = make_device(s, plugin, input)?;
        what = format!(" with {w} (device #{})", d.id.0);
        t.devices.push(d);
    }
    if let Some(v) = refs::opt_num(input, "volume_db") {
        t.volume_db = v.clamp(-70., 6.) as f32;
    }
    if let Some(v) = refs::opt_num(input, "pan") {
        t.pan = v.clamp(-1., 1.) as f32;
    }
    let index = match input.get("after").filter(|v| !v.is_null()) {
        Some(v) => s.draft.track_index(refs::track(&s.draft, v, s.selected)?).map_or(0, |i| i + 1),
        None => refs::insert_index(&s.draft, kind),
    };
    let label = format!("New {} track \"{name}\"{what}", crate::llm::context::kind_name(kind));
    s.apply(&label, vec![Command::AddTrack { track: Box::new(t), index }])?;
    Ok(format!("{label}: track #{}", id.0))
}

pub fn update_track(s: &mut Session, input: &Value) -> Res<String> {
    let id = refs::track_field(&s.draft, input, "track", s.selected)?;
    let name = s.draft.track(id).map(|t| t.name.clone()).unwrap_or_default();
    let mut cmds = Vec::new();
    let mut what = Vec::new();
    let mut set = |change: TrackProp, w: String| {
        cmds.push(Command::SetTrack { id, change });
        what.push(w);
    };
    if let Some(n) = refs::opt_str(input, "name") {
        set(TrackProp::Name(n.to_owned()), format!("renamed to \"{n}\""));
    }
    if let Some(v) = refs::opt_num(input, "volume_db") {
        let v = v.clamp(-70., 6.) as f32;
        set(TrackProp::VolumeDb(v), format!("fader {v:+.1} dB"));
    }
    if let Some(v) = refs::opt_num(input, "pan") {
        let v = v.clamp(-1., 1.) as f32;
        set(TrackProp::Pan(v), format!("pan {v:+.2}"));
    }
    if let Some(v) = refs::opt_bool(input, "mute") {
        set(TrackProp::Mute(v), if v { "muted" } else { "unmuted" }.into());
    }
    if let Some(v) = refs::opt_bool(input, "solo") {
        set(TrackProp::Solo(v), if v { "soloed" } else { "unsoloed" }.into());
    }
    if let Some(v) = refs::opt_num(input, "color") {
        set(TrackProp::Color(Color::palette(v.max(0.) as usize)), "new colour".into());
    }
    if let Some(v) = input.get("output").filter(|v| !v.is_null()) {
        let to = refs::track(&s.draft, v, s.selected)?;
        let out = if to == s.draft.main_track().id { Output::Main } else { Output::Track(to) };
        let to_name = s.draft.track(to).map(|t| t.name.clone()).unwrap_or_default();
        set(TrackProp::Output(out), format!("out → {to_name}"));
    }
    if cmds.is_empty() {
        return Err("nothing to change: give name, volume_db, pan, mute, solo, color or output".into());
    }
    let label = format!("{name}: {}", what.join(", "));
    s.apply(&label, cmds)?;
    Ok(label)
}

pub fn delete_track(s: &mut Session, input: &Value) -> Res<String> {
    let id = refs::track_field(&s.draft, input, "track", s.selected)?;
    let name = s.draft.track(id).map(|t| t.name.clone()).unwrap_or_default();
    let label = format!("Delete track \"{name}\"");
    s.apply(&label, vec![Command::RemoveTrack { id }])?;
    Ok(label)
}

pub fn group_tracks(s: &mut Session, input: &Value) -> Res<String> {
    let list = refs::field(input, "tracks")?.as_array().ok_or("\"tracks\" is a list")?;
    let ids = list.iter().map(|v| refs::track(&s.draft, v, s.selected)).collect::<Res<Vec<TrackId>>>()?;
    let name = refs::opt_str(input, "name").unwrap_or("Group").to_owned();
    let group = s.draft.alloc_track_id();
    let mut cmds = aitrack_model::groups::group_commands(&s.draft, &ids, group).ok_or("those tracks cannot be grouped")?;
    cmds.push(Command::SetTrack { id: group, change: TrackProp::Name(name.clone()) });
    let label = format!("Group {} tracks into \"{name}\"", ids.len());
    s.apply(&label, cmds)?;
    Ok(format!("{label}: group #{}", group.0))
}

pub fn add_device(s: &mut Session, input: &Value) -> Res<String> {
    let track = refs::track_field(&s.draft, input, "track", s.selected)?;
    let plugin = refs::opt_str(input, "plugin").ok_or("missing \"plugin\"")?.to_owned();
    let (d, what) = make_device(s, &plugin, input)?;
    let id = d.id;
    let len = s.draft.track(track).map_or(0, |t| t.devices.len());
    let index = refs::opt_num(input, "position").map_or(len, |v| (v.max(0.) as usize).min(len));
    let tname = s.draft.track(track).map(|t| t.name.clone()).unwrap_or_default();
    let label = format!("{tname}: add {what}");
    s.apply(&label, vec![Command::AddDevice { slot: DeviceSlot::Track { track, index }, device: Box::new(d) }])?;
    Ok(format!("{label} as device #{} at position {index}", id.0))
}

pub fn set_params(s: &mut Session, input: &Value) -> Res<String> {
    let id = refs::device(&s.draft, refs::field(input, "device")?)?;
    let d = s.draft.device(id).ok_or("no such device")?.clone();
    let mut cmds = Vec::new();
    let mut what = Vec::new();
    if let Some(obj) = input.get("params").and_then(Value::as_object).filter(|o| !o.is_empty()) {
        let plugin = refs::stock_id(&d).ok_or("third-party plugin parameters cannot be set by the assistant")?;
        let info = refs::plugin_info(plugin).ok_or("unknown stock device")?;
        for (k, v) in obj {
            let pi = refs::param(&info, k)?;
            let n = refs::param_value(pi, v)?;
            cmds.push(Command::SetParam { device: id, param: pi.id, value: Some(n) });
            what.push(format!("{} {}", pi.name, pi.format(n)));
        }
    }
    if let Some(on) = refs::opt_bool(input, "enabled") {
        cmds.push(Command::SetDeviceEnabled { device: id, enabled: on });
        what.push(if on { "on" } else { "bypassed" }.into());
    }
    if cmds.is_empty() {
        return Err("give \"params\" (name → value) or \"enabled\"".into());
    }
    let label = format!("{}: {}", d.name, what.join(", "));
    s.apply(&label, cmds)?;
    Ok(label)
}

pub fn load_preset(s: &mut Session, input: &Value) -> Res<String> {
    let id = refs::device(&s.draft, refs::field(input, "device")?)?;
    let d = s.draft.device(id).ok_or("no such device")?.clone();
    let plugin = refs::stock_id(&d).ok_or("only stock devices have factory presets")?;
    let (name, values) = preset(plugin, refs::opt_str(input, "preset").ok_or("missing \"preset\"")?)?;
    let mut cmds: Vec<Command> = values.into_iter().map(|(param, v)| Command::SetParam { device: id, param, value: Some(v) }).collect();
    cmds.push(Command::SetDeviceState { device: id, state: d.state.clone(), preset: Some(name.clone()) });
    let label = format!("{}: preset \"{name}\"", d.name);
    s.apply(&label, cmds)?;
    Ok(label)
}

pub fn remove_device(s: &mut Session, input: &Value) -> Res<String> {
    let id = refs::device(&s.draft, refs::field(input, "device")?)?;
    let name = s.draft.device(id).map(|d| d.name.clone()).unwrap_or_default();
    let label = format!("Remove {name}");
    s.apply(&label, vec![Command::RemoveDevice { id }])?;
    Ok(label)
}

pub fn set_send(s: &mut Session, input: &Value) -> Res<String> {
    let id = refs::track_field(&s.draft, input, "track", s.selected)?;
    let to = refs::track_field(&s.draft, input, "to", s.selected)?;
    let t = s.draft.track(id).ok_or("no such track")?;
    if s.draft.track(to).is_none_or(|r| r.kind != TrackKind::Return) {
        return Err("sends go to a return track (create_track kind \"return\")".into());
    }
    let level = refs::num(input, "level_db")?.clamp(-70., 6.) as f32;
    let mut sends = t.sends.clone();
    let pre = refs::opt_bool(input, "pre_fader").unwrap_or(false);
    let off = level <= -69.;
    match sends.iter_mut().find(|x| x.target == to) {
        Some(x) => {
            x.level_db = level;
            x.pre_fader = pre;
        }
        None if !off => sends.push(Send { target: to, level_db: level, pre_fader: pre }),
        None => {}
    }
    if off {
        sends.retain(|x| x.target != to);
    }
    let rname = s.draft.track(to).map(|r| r.name.clone()).unwrap_or_default();
    let label = if off { format!("{}: no send to {rname}", t.name) } else { format!("{}: send {level:+.1} dB → {rname}", t.name) };
    s.apply(&label, vec![Command::SetTrack { id, change: TrackProp::Sends(sends) }])?;
    Ok(label)
}

pub fn set_sidechain(s: &mut Session, input: &Value) -> Res<String> {
    let id = refs::device(&s.draft, refs::field(input, "device")?)?;
    let src = match input.get("source").filter(|v| !v.is_null()) {
        Some(v) => Some(refs::track(&s.draft, v, s.selected)?),
        None => None,
    };
    let dname = s.draft.device(id).map(|d| d.name.clone()).unwrap_or_default();
    let label = match src {
        Some(t) => format!("{dname}: sidechain from {}", s.draft.track(t).map(|t| t.name.clone()).unwrap_or_default()),
        None => format!("{dname}: no sidechain"),
    };
    s.apply(&label, vec![Command::SetSidechain { device: id, source: src }])?;
    Ok(label)
}

pub fn set_song(s: &mut Session, input: &Value) -> Res<String> {
    let mut cmds = Vec::new();
    let mut what = Vec::new();
    if let Some(bpm) = refs::opt_num(input, "bpm") {
        let bpm = bpm.clamp(20., 300.);
        cmds.push(Command::SetTempo { bpm });
        what.push(format!("{bpm:.1} BPM"));
    }
    if let Some(root) = refs::opt_str(input, "key") {
        let key = Key::from_name(root).ok_or_else(|| format!("unknown key \"{root}\" (C, F#, Bb…)"))?;
        let kind_s = refs::opt_str(input, "scale").unwrap_or("minor").to_lowercase().replace([' ', '-'], "_");
        let kind = ScaleKind::ALL.into_iter().find(|k| k.label().to_lowercase().replace(' ', "_") == kind_s).ok_or_else(|| format!("unknown scale \"{kind_s}\""))?;
        let sc = Scale::new(key, kind);
        cmds.push(Command::SetScale { scale: Some(sc) });
        what.push(format!("key {}", sc.display()));
    }
    if let (Some(a), Some(b)) = (refs::opt_num(input, "loop_start_bar"), refs::opt_num(input, "loop_end_bar")) {
        let (a, b) = (refs::bar_to_tick(&s.draft, a), refs::bar_to_tick(&s.draft, b));
        if b <= a {
            return Err("the loop must end after it starts".into());
        }
        cmds.push(Command::SetLoop { region: Some((a, b)) });
        what.push("loop".into());
    }
    if let Some(n) = refs::opt_str(input, "name") {
        cmds.push(Command::RenameProject { name: n.to_owned() });
        what.push(format!("named \"{n}\""));
    }
    if cmds.is_empty() {
        return Err("give bpm, key (+scale), loop_start_bar/loop_end_bar or name".into());
    }
    let label = format!("Song: {}", what.join(", "));
    s.apply(&label, cmds)?;
    Ok(label)
}

/// Placements on `track` that overlap `[a, b)`.
fn overlapping(s: &Session, track: TrackId, a: i64, b: i64) -> Vec<aitrack_model::PlacementId> {
    let p = &s.draft;
    p.track(track).map_or_else(Vec::new, |t| {
        t.arrangement.iter().filter(|pl| p.clips.get(&pl.clip).is_some_and(|c| pl.start < b && pl.start + c.length > a)).map(|pl| pl.id).collect()
    })
}

pub fn write_clip(s: &mut Session, input: &Value) -> Res<String> {
    let track = refs::track_field(&s.draft, input, "track", s.selected)?;
    let t = s.draft.track(track).ok_or("no such track")?;
    if t.kind != TrackKind::Instrument {
        return Err(format!("\"{}\" is not a MIDI track", t.name));
    }
    let tname = t.name.clone();
    let tcolor = t.color;
    let start = refs::bar_to_tick(&s.draft, refs::opt_num(input, "start_bar").unwrap_or(1.));
    let bars = refs::num(input, "length_bars")?.clamp(0.25, 256.);
    let length = (bars * refs::bar_ticks(&s.draft) as f64).round() as i64;
    let list = refs::field(input, "notes")?.as_array().ok_or("\"notes\" is a list")?;
    let mut notes = NoteSource::default();
    for (i, n) in list.iter().enumerate() {
        let key = n.get("pitch").and_then(Value::as_f64).ok_or_else(|| format!("note {i}: missing pitch"))?.round().clamp(0., 127.) as u8;
        let at = refs::beats_to_ticks(n.get("start").and_then(Value::as_f64).ok_or_else(|| format!("note {i}: missing start"))?.max(0.));
        let len = refs::beats_to_ticks(n.get("length").and_then(Value::as_f64).unwrap_or(0.25).max(1. / 64.));
        let vel = n.get("velocity").and_then(Value::as_f64).unwrap_or(0.8);
        let vel = if vel > 1. { vel / 127. } else { vel }.clamp(0.01, 1.) as f32;
        if at >= length {
            continue;
        }
        notes.notes.push(Note::new(s.draft.alloc_note_id(), key, at, len.min(length - at), vel));
    }
    notes.sort();
    let count = notes.notes.len();
    let name = refs::opt_str(input, "name").map_or_else(|| tname.clone(), str::to_owned);
    let mut cmds = Vec::new();
    if refs::opt_bool(input, "replace").unwrap_or(true) {
        for id in overlapping(s, track, start, start + length) {
            cmds.push(Command::RemovePlacement { track, id });
        }
    }
    let src = s.draft.alloc_source_id();
    let clip = s.draft.alloc_clip_id();
    let pl = s.draft.alloc_placement_id();
    cmds.push(Command::AddSource { id: src, source: Box::new(Source::Notes(notes)) });
    cmds.push(Command::AddClip { id: clip, clip: Box::new(Clip::new(&name, tcolor, src, length)) });
    cmds.push(Command::PlaceClip { track, placement: Placement { id: pl, clip, start, lane: 0 } });
    let label = format!("{tname}: \"{name}\", {count} notes, bars {:.0}–{:.0}", refs::tick_to_bar(&s.draft, start), refs::tick_to_bar(&s.draft, start + length));
    s.apply(&label, cmds)?;
    Ok(format!("{label} (clip #{}, placement #{})", clip.0, pl.0))
}

pub fn copy_clip(s: &mut Session, input: &Value) -> Res<String> {
    let clip = aitrack_model::ClipId(refs::field(input, "clip")?.as_u64().ok_or("\"clip\" is a clip id from get_track")?);
    let c = s.draft.clips.get(&clip).ok_or_else(|| format!("no clip #{}", clip.0))?.clone();
    let track = match input.get("track").filter(|v| !v.is_null()) {
        Some(v) => refs::track(&s.draft, v, s.selected)?,
        None => s.draft.tracks.iter().find(|t| t.arrangement.iter().any(|pl| pl.clip == clip)).map(|t| t.id).ok_or("give the track to place it on")?,
    };
    let start = refs::bar_to_tick(&s.draft, refs::num(input, "start_bar")?);
    let times = refs::opt_num(input, "times").unwrap_or(1.).clamp(1., 64.) as i64;
    let mut cmds = Vec::new();
    for id in overlapping(s, track, start, start + c.length * times) {
        cmds.push(Command::RemovePlacement { track, id });
    }
    for i in 0..times {
        let id = s.draft.alloc_placement_id();
        cmds.push(Command::PlaceClip { track, placement: Placement { id, clip, start: start + i * c.length, lane: 0 } });
    }
    let tname = s.draft.track(track).map(|t| t.name.clone()).unwrap_or_default();
    let label = format!("{tname}: \"{}\" ×{times} from bar {:.0}", c.name, refs::tick_to_bar(&s.draft, start));
    s.apply(&label, cmds)?;
    Ok(label)
}

pub fn move_clip(s: &mut Session, input: &Value) -> Res<String> {
    let track = refs::track_field(&s.draft, input, "track", s.selected)?;
    let id = aitrack_model::PlacementId(refs::field(input, "placement")?.as_u64().ok_or("\"placement\" is a placement id from get_track")?);
    let to_track = match input.get("to_track").filter(|v| !v.is_null()) {
        Some(v) => refs::track(&s.draft, v, s.selected)?,
        None => track,
    };
    let start = refs::bar_to_tick(&s.draft, refs::num(input, "start_bar")?);
    let label = format!("Move clip to bar {:.2}", refs::tick_to_bar(&s.draft, start));
    s.apply(&label, vec![Command::MovePlacement { track, id, to_track, start, lane: 0 }])?;
    Ok(label)
}

pub fn clear_bars(s: &mut Session, input: &Value) -> Res<String> {
    let tracks: Vec<TrackId> = match input.get("tracks").and_then(Value::as_array) {
        Some(list) => list.iter().map(|v| refs::track(&s.draft, v, s.selected)).collect::<Res<_>>()?,
        None => vec![refs::track_field(&s.draft, input, "track", s.selected)?],
    };
    let a = refs::bar_to_tick(&s.draft, refs::num(input, "start_bar")?);
    let b = refs::bar_to_tick(&s.draft, refs::num(input, "end_bar")?);
    let mut cmds = Vec::new();
    for t in &tracks {
        let p = &s.draft;
        if let Some(tr) = p.track(*t) {
            for pl in &tr.arrangement {
                if pl.start >= a && pl.start < b {
                    cmds.push(Command::RemovePlacement { track: *t, id: pl.id });
                }
            }
        }
    }
    if cmds.is_empty() {
        return Ok("No clip starts in those bars; nothing removed.".into());
    }
    let label = format!("Remove {} clips from bars {:.0}–{:.0}", cmds.len(), refs::tick_to_bar(&s.draft, a), refs::tick_to_bar(&s.draft, b));
    s.apply(&label, cmds)?;
    Ok(label)
}

pub fn set_automation(s: &mut Session, input: &Value) -> Res<String> {
    let track = refs::track_field(&s.draft, input, "track", s.selected)?;
    let target_s = refs::opt_str(input, "target").ok_or("missing \"target\" (volume, pan, param)")?.to_lowercase();
    let (target, conv): (AutomationTarget, Box<dyn Fn(f64) -> Res<f32>>) = match target_s.as_str() {
        "volume" => (AutomationTarget::Volume, Box::new(|v| Ok(refs::volume_normalized(v as f32)))),
        "pan" => (AutomationTarget::Pan, Box::new(|v| Ok(((v.clamp(-1., 1.) + 1.) / 2.) as f32))),
        "param" | "device" => {
            let dev = refs::device(&s.draft, refs::field(input, "device")?)?;
            let d = s.draft.device(dev).ok_or("no such device")?;
            let info = refs::plugin_info(refs::stock_id(d).ok_or("third-party plugin parameters cannot be automated by the assistant")?).ok_or("unknown device")?;
            let pi = refs::param(&info, refs::opt_str(input, "param").ok_or("missing \"param\"")?)?.clone();
            (AutomationTarget::DeviceParam { device: dev, param: pi.id }, Box::new(move |v| Ok(pi.to_normalized(v as f32))))
        }
        other => return Err(format!("unknown target \"{other}\"")),
    };
    let list = refs::field(input, "points")?.as_array().ok_or("\"points\" is a list of {bar, value}")?;
    let mut lane = s.draft.track(track).and_then(|t| t.automation.iter().find(|l| l.target == target).cloned()).filter(|_| !refs::opt_bool(input, "replace").unwrap_or(true)).unwrap_or_else(|| AutomationLane::new(target));
    for (i, pt) in list.iter().enumerate() {
        let bar = pt.get("bar").and_then(Value::as_f64).ok_or_else(|| format!("point {i}: missing bar"))?;
        let value = pt.get("value").and_then(Value::as_f64).ok_or_else(|| format!("point {i}: missing value"))?;
        let curve = pt.get("curve").and_then(Value::as_f64).unwrap_or(0.).clamp(-1., 1.) as f32;
        lane.insert(AutomationPoint { tick: refs::bar_to_tick(&s.draft, bar), value: conv(value)?, curve });
    }
    let n = lane.points.len();
    let tname = s.draft.track(track).map(|t| t.name.clone()).unwrap_or_default();
    let label = format!("{tname}: {target_s} automation, {n} points");
    s.apply(&label, vec![Command::SetAutomation { track, target, lane: Some(lane) }])?;
    Ok(label)
}
