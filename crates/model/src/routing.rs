//! Where audio goes: the routing graph of a project (track outputs and
//! sends, as the engine compiler reads them) and the checks that keep it a
//! graph without loops or dangling ends.
//!
//! `SetTrack` with an output or sends uses [`check_output`] and
//! [`check_sends`]; the mixer can ask [`routable_outputs`] and
//! [`routable_sends`] which targets it may offer.

use crate::command::CommandError;
use crate::ids::TrackId;
use crate::project::*;

/// The group an `Output::Main` track really feeds: the nearest enclosing
/// Group (walking up through Folders), as the engine compiler resolves it.
fn enclosing_group(p: &Project, t: &Track) -> Option<TrackId> {
    let mut at = t.parent;
    // Bounded by the track count, so a corrupt parent loop cannot hang.
    for _ in 0..p.tracks.len() {
        let parent = p.track(at?)?;
        match parent.kind {
            TrackKind::Group => return Some(parent.id),
            TrackKind::Folder => at = parent.parent,
            _ => return None,
        }
    }
    None
}

/// The track `id`'s output goes to: `Output::Main` resolves to the
/// enclosing group or else to Main; hardware, nowhere, a track that is not
/// there and a track that carries no audio resolve to nothing.
pub fn destination(p: &Project, id: TrackId) -> Option<TrackId> {
    let t = p.track(id)?;
    if t.kind == TrackKind::Main || !t.kind.carries_audio() {
        return None;
    }
    match t.output {
        Output::Main => enclosing_group(p, t).filter(|g| *g != id).or_else(|| Some(p.main_track().id)),
        Output::Track(to) => p.track(to).filter(|x| x.kind.carries_audio() && x.id != id).map(|x| x.id),
        Output::Hardware(_) | Output::None => None,
    }
}

/// The tracks `id` feeds directly: its destination and its send targets.
pub fn feeds(p: &Project, id: TrackId) -> Vec<TrackId> {
    let Some(t) = p.track(id) else { return Vec::new() };
    if !t.kind.carries_audio() {
        return Vec::new();
    }
    let sends = t.sends.iter().filter_map(|s| p.track(s.target)).filter(|x| x.kind.carries_audio() && x.id != id);
    let mut out: Vec<TrackId> = destination(p, id).into_iter().chain(sends.map(|x| x.id)).collect();
    out.dedup();
    out
}

/// Whether audio from `from` reaches `to` through outputs and sends (a track
/// does not reach itself unless it sits in a loop).
pub fn reaches(p: &Project, from: TrackId, to: TrackId) -> bool {
    let mut seen = vec![from];
    let mut stack = feeds(p, from);
    while let Some(at) = stack.pop() {
        if at == to {
            return true;
        }
        if !seen.contains(&at) {
            seen.push(at);
            stack.extend(feeds(p, at));
        }
    }
    false
}

fn cycle(from: TrackId, to: TrackId) -> CommandError {
    CommandError::RoutingCycle { from, to }
}

/// Whether `track` may take `output`: `Output::Track(x)` needs an audio
/// track other than itself that does not already (directly or through other
/// tracks) feed `track`. An output the track already has is never judged.
pub fn check_output(p: &Project, track: TrackId, output: Output) -> Result<(), CommandError> {
    let Output::Track(to) = output else { return Ok(()) };
    let t = p.track(track).ok_or(CommandError::TrackNotFound(track))?;
    if t.output == output {
        return Ok(());
    }
    if to == track {
        return Err(CommandError::Invalid("a track cannot be routed to itself".into()));
    }
    let target = p.track(to).ok_or(CommandError::TrackNotFound(to))?;
    if !target.kind.carries_audio() {
        return Err(CommandError::Invalid(format!("{to} carries no audio, so nothing can be routed to it")));
    }
    if reaches(p, to, track) {
        return Err(cycle(track, to));
    }
    Ok(())
}

/// Whether `track` may take the send list `sends`. Every target that is not
/// already in its list must be an audio track other than `track` that does
/// not feed `track`; targets already there are never judged (so a level
/// change works even in a project that came with a loop).
pub fn check_sends(p: &Project, track: TrackId, sends: &[Send]) -> Result<(), CommandError> {
    let t = p.track(track).ok_or(CommandError::TrackNotFound(track))?;
    for s in sends.iter().filter(|s| !t.sends.iter().any(|old| old.target == s.target)) {
        if s.target == track {
            return Err(CommandError::Invalid("a track cannot send to itself".into()));
        }
        let target = p.track(s.target).ok_or(CommandError::TrackNotFound(s.target))?;
        if !target.kind.carries_audio() {
            return Err(CommandError::Invalid(format!("{} carries no audio, so it cannot take a send", s.target)));
        }
        if reaches(p, s.target, track) {
            return Err(cycle(track, s.target));
        }
    }
    Ok(())
}

/// The tracks `track` could take as an output without a loop (for the
/// mixer's output selector), in track order.
pub fn routable_outputs(p: &Project, track: TrackId) -> Vec<TrackId> {
    p.tracks
        .iter()
        .filter(|t| t.id != track && t.kind != TrackKind::Main && check_output(p, track, Output::Track(t.id)).is_ok())
        .map(|t| t.id)
        .collect()
}

/// The tracks `track` could send to without a loop, in track order.
pub fn routable_sends(p: &Project, track: TrackId) -> Vec<TrackId> {
    let probe = |target| Send { target, level_db: 0.0, pre_fader: false };
    p.tracks
        .iter()
        .filter(|t| t.id != track && t.kind != TrackKind::Main && check_sends(p, track, &[probe(t.id)]).is_ok())
        .map(|t| t.id)
        .collect()
}

/// Everything that names a track (or send) that is not there: outputs,
/// sends, groups, VCAs, sidechains and send automation past the send list.
/// Empty for a sound project. Messages are for logs and tests.
pub fn dangling_references(p: &Project) -> Vec<String> {
    let mut out = Vec::new();
    for t in &p.tracks {
        let missing = |id: TrackId| p.track(id).is_none();
        if let Output::Track(to) = t.output
            && missing(to)
        {
            out.push(format!("{} outputs to missing {to}", t.id));
        }
        for s in t.sends.iter().filter(|s| missing(s.target)) {
            out.push(format!("{} sends to missing {}", t.id, s.target));
        }
        if let Some(g) = t.parent.filter(|g| missing(*g)) {
            out.push(format!("{} sits in missing {g}", t.id));
        }
        if let Some(v) = t.vca.filter(|v| missing(*v)) {
            out.push(format!("{} follows missing {v}", t.id));
        }
        visit_devices(&t.devices, &mut |d| {
            if let Some(s) = d.sidechain.filter(|s| missing(*s)) {
                out.push(format!("{} on {} listens to missing {s}", d.id, t.id));
            }
        });
        visit_devices(&t.devices, &mut |d| {
            if let DeviceKind::Rack(rack) = &d.kind {
                let mut inside = Vec::new();
                for c in rack.chains.iter().chain(&rack.returns) {
                    visit_devices(&c.devices, &mut |x| inside.push(x.id));
                }
                for (mi, m) in rack.macros.iter().enumerate() {
                    for mapping in m.mappings.iter().filter(|x| !inside.contains(&x.device)) {
                        out.push(format!("macro {mi} of {} drives {} which is not inside it", d.id, mapping.device));
                    }
                }
            }
        });
        for lane in &t.automation {
            if let AutomationTarget::Send { index } = lane.target
                && usize::from(index) >= t.sends.len()
            {
                out.push(format!("{} automates send {index} of {} sends", t.id, t.sends.len()));
            }
        }
    }
    out
}
