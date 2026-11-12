//! Mix plans: the moves "mix this track" suggests (fader, pan, low cut, EQ
//! bands, compression), either parsed from a model reply or worked out by
//! rules from the track's name and type. The app turns a plan into
//! `Command`s and offers them as one proposal; nothing here edits a project.

use serde::Deserialize;

use aitrack_model::{Track, TrackKind};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BandKind {
    #[default]
    Bell,
    LowShelf,
    HighShelf,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct EqMove {
    #[serde(default, rename = "type")]
    pub kind: BandKind,
    pub freq_hz: f32,
    pub gain_db: f32,
    #[serde(default = "default_q")]
    pub q: f32,
}

fn default_q() -> f32 {
    1.0
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct CompMove {
    pub threshold_db: f32,
    pub ratio: f32,
    #[serde(default)]
    pub attack_ms: Option<f32>,
    #[serde(default)]
    pub release_ms: Option<f32>,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
pub struct MixPlan {
    #[serde(default)]
    pub volume_db: Option<f32>,
    /// -1 (left) … 1 (right).
    #[serde(default)]
    pub pan: Option<f32>,
    #[serde(default)]
    pub high_pass_hz: Option<f32>,
    #[serde(default)]
    pub eq: Vec<EqMove>,
    #[serde(default)]
    pub compressor: Option<CompMove>,
    /// One short reason per move, shown on the proposal card.
    #[serde(default)]
    pub notes: Vec<String>,
}

impl MixPlan {
    pub fn is_empty(&self) -> bool {
        self.volume_db.is_none() && self.pan.is_none() && self.high_pass_hz.is_none() && self.eq.is_empty() && self.compressor.is_none()
    }

    /// Keep every value inside what the devices accept.
    pub fn clamped(mut self) -> Self {
        self.volume_db = self.volume_db.map(|v| v.clamp(-60., 6.));
        self.pan = self.pan.map(|v| v.clamp(-1., 1.));
        self.high_pass_hz = self.high_pass_hz.map(|v| v.clamp(20., 1_000.));
        self.eq.truncate(5); // bands 1–5 of the 8-band EQ; band 0 is the low cut
        for b in &mut self.eq {
            b.freq_hz = b.freq_hz.clamp(20., 20_000.);
            b.gain_db = b.gain_db.clamp(-12., 12.);
            b.q = b.q.clamp(0.1, 18.);
        }
        if let Some(c) = &mut self.compressor {
            c.threshold_db = c.threshold_db.clamp(-60., 0.);
            c.ratio = c.ratio.clamp(1., 20.);
        }
        self
    }
}

/// Appended to the request when the user runs "mix this track".
pub const INSTRUCTIONS: &str = "The user asked you to mix the selected track. Explain your moves in a few short bullet points, \
then end with exactly one fenced ```json block holding the plan, with only these optional fields: \
{\"volume_db\": number (fader, -60..6), \"pan\": number (-1 left .. 1 right), \"high_pass_hz\": number, \
\"eq\": [{\"type\": \"bell\"|\"low_shelf\"|\"high_shelf\", \"freq_hz\": number, \"gain_db\": number, \"q\": number}] (at most 5), \
\"compressor\": {\"threshold_db\": number, \"ratio\": number, \"attack_ms\": number, \"release_ms\": number}, \
\"notes\": [string]}. Leave out anything that should stay as it is.";

/// The plan in the last fenced JSON block of a reply, if any.
pub fn parse_reply(text: &str) -> Option<MixPlan> {
    let mut best = None;
    let mut rest = text;
    while let Some(open) = rest.find("```") {
        let after = &rest[open + 3..];
        let body_start = after.find('\n').map_or(0, |i| i + 1);
        let lang = after[..body_start].trim();
        let Some(close) = after[body_start..].find("```") else { break };
        let body = &after[body_start..body_start + close];
        if (lang.is_empty() || lang.eq_ignore_ascii_case("json"))
            && let Ok(plan) = serde_json::from_str::<MixPlan>(body.trim())
        {
            best = Some(plan.clamped());
        }
        rest = &after[body_start + close + 3..];
    }
    best
}

/// What a track probably holds, from its name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Kick,
    Bass,
    Snare,
    Hats,
    Percussion,
    Vocal,
    Pad,
    Lead,
    Bus,
    Other,
}

pub fn role(track: &Track) -> Role {
    if matches!(track.kind, TrackKind::Group | TrackKind::Return | TrackKind::Main) {
        return Role::Bus;
    }
    let n = track.name.to_lowercase();
    let has = |words: &[&str]| words.iter().any(|w| n.contains(w));
    if has(&["kick", "bd", "bombo"]) {
        Role::Kick
    } else if has(&["bass", "sub", "808", "bajo"]) {
        Role::Bass
    } else if has(&["snare", "clap", "rim", "caja"]) {
        Role::Snare
    } else if has(&["hat", "hh", "cymbal", "ride", "crash", "shaker"]) {
        Role::Hats
    } else if has(&["perc", "conga", "bongo", "tom", "tamb"]) {
        Role::Percussion
    } else if has(&["vox", "vocal", "voice", "voz", "chant"]) {
        Role::Vocal
    } else if has(&["pad", "string", "atmo", "ambient", "choir"]) {
        Role::Pad
    } else if has(&["lead", "synth", "arp", "pluck", "keys", "piano", "chord"]) {
        Role::Lead
    } else {
        Role::Other
    }
}

/// Fader, pan, low cut, EQ, compressor and the reason, per role.
type Moves = (f32, Option<f32>, Option<f32>, Vec<EqMove>, Option<CompMove>, &'static str);

/// A starting point by rules, for when no model is connected.
pub fn heuristic(track: &Track) -> MixPlan {
    let bell = |freq_hz: f32, gain_db: f32, q: f32| EqMove { kind: BandKind::Bell, freq_hz, gain_db, q };
    let comp = |threshold_db: f32, ratio: f32, attack: f32, release: f32| Some(CompMove { threshold_db, ratio, attack_ms: Some(attack), release_ms: Some(release) });
    let (fader, pan, hpf, eq, compressor, why): Moves = match role(track) {
        Role::Kick => (-6., Some(0.), Some(30.), vec![bell(60., 2., 1.2), bell(350., -3., 1.5), bell(4_000., 2., 1.)], comp(-12., 4., 10., 80.), "kick: low cut under the sub, less box at 350 Hz, click at 4 kHz"),
        Role::Bass => (-8., Some(0.), Some(30.), vec![bell(250., -2., 1.2), bell(800., 1.5, 1.)], comp(-18., 4., 20., 120.), "bass: centred and steady, room cleared at 250 Hz for the kick"),
        Role::Snare => (-8., None, Some(120.), vec![bell(200., 2., 1.2), bell(900., -2., 2.), bell(6_000., 2., 0.8)], comp(-14., 3., 5., 100.), "snare: body at 200 Hz, less ring at 900 Hz, snap up top"),
        Role::Hats => (-14., Some(0.25), Some(300.), vec![bell(8_000., -1.5, 1.)], None, "hats: low cut at 300 Hz, a little to the side, softened at 8 kHz"),
        Role::Percussion => (-12., Some(-0.3), Some(150.), vec![bell(500., -2., 1.4)], None, "percussion: low cut and panned opposite the hats"),
        Role::Vocal => (-6., Some(0.), Some(90.), vec![bell(300., -2., 1.2), EqMove { kind: BandKind::HighShelf, freq_hz: 10_000., gain_db: 2., q: 0.7 }], comp(-18., 3., 5., 80.), "vocal: low cut, less mud at 300 Hz, air at 10 kHz, gentle levelling"),
        Role::Pad => (-14., None, Some(150.), vec![bell(400., -3., 1.)], None, "pad: low cut and less low-mid so it sits behind the drums"),
        Role::Lead => (-10., None, Some(100.), vec![bell(350., -2., 1.2), bell(3_000., 1.5, 1.)], comp(-16., 2.5, 10., 120.), "lead: low cut, presence at 3 kHz"),
        Role::Bus => (track.volume_db.min(-3.), None, None, vec![], comp(-10., 2., 30., 200.), "bus: headroom and glue compression"),
        Role::Other => (-10., None, Some(60.), vec![bell(300., -2., 1.2)], None, "headroom, a low cut and a little less mud"),
    };
    MixPlan {
        volume_db: (track.volume_db - fader).abs().gt(&0.5).then_some(fader),
        pan: pan.filter(|p| (track.pan - p).abs() > 0.02),
        high_pass_hz: hpf,
        eq,
        compressor,
        notes: vec![why.to_owned()],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aitrack_model::{Color, TrackId};

    #[test]
    fn parses_the_last_json_block() {
        let reply = "Here's the idea:\n- cut lows\n```json\n{\"volume_db\": 99}\n```\nBetter:\n```json\n{\"volume_db\": -7.5, \"pan\": 0.1, \"high_pass_hz\": 35, \
                     \"eq\": [{\"type\": \"bell\", \"freq_hz\": 320, \"gain_db\": -3, \"q\": 1.4}], \"compressor\": {\"threshold_db\": -14, \"ratio\": 3}, \"notes\": [\"less mud\"]}\n```";
        let p = parse_reply(reply).unwrap();
        assert_eq!(p.volume_db, Some(-7.5));
        assert_eq!(p.pan, Some(0.1));
        assert_eq!(p.high_pass_hz, Some(35.));
        assert_eq!(p.eq[0], EqMove { kind: BandKind::Bell, freq_hz: 320., gain_db: -3., q: 1.4 });
        assert_eq!(p.compressor.as_ref().unwrap().ratio, 3.);
        assert_eq!(p.notes, vec!["less mud".to_owned()]);
    }

    #[test]
    fn clamps_wild_values_and_ignores_prose() {
        assert!(parse_reply("no plan here").is_none());
        let p = parse_reply("```json\n{\"volume_db\": 40, \"pan\": -3}\n```").unwrap();
        assert_eq!((p.volume_db, p.pan), (Some(6.), Some(-1.)));
    }

    #[test]
    fn rules_follow_the_track_name() {
        let kick = Track::new(TrackId(1), TrackKind::Instrument, "Kick 909", Color::palette(0));
        let plan = heuristic(&kick);
        assert_eq!(role(&kick), Role::Kick);
        assert_eq!(plan.high_pass_hz, Some(30.));
        assert!(plan.compressor.is_some());
        let hats = Track::new(TrackId(2), TrackKind::Audio, "Open Hats", Color::palette(0));
        assert_eq!(heuristic(&hats).pan, Some(0.25));
        assert!(!heuristic(&hats).is_empty());
    }
}
