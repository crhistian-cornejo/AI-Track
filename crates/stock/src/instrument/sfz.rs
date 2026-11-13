//! Minimal, standards-faithful SFZ v2 parser (`<control>`, `<global>`,
//! `<group>`, `<region>`): key ranges, note names (`c4`, `f#3`, `bb2`),
//! velocity layers (`lovel`/`hivel`), round-robin (`seq_length`/`seq_position`),
//! `trigger=attack|release|first|legato`, loop opcodes, pitch, volume, and pan.

use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Trigger {
    #[default]
    Attack,
    Release,
    First,
    Legato,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum LoopMode {
    #[default]
    NoLoop,
    OneShot,
    Continuous,
    Sustain,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RegionDef {
    pub sample: PathBuf,
    pub lokey: u8,
    pub hikey: u8,
    pub pitch_keycenter: i16,
    pub lovel: u8,
    pub hivel: u8,
    pub seq_length: u8,
    pub seq_position: u8,
    pub trigger: Trigger,
    pub loop_mode: LoopMode,
    pub loop_start: usize,
    pub loop_end: Option<usize>,
    pub offset: usize,
    pub end: Option<usize>,
    pub tune_cents: f32,
    pub transpose: i32,
    pub volume_db: f32,
    pub pan: f32,
}

impl Default for RegionDef {
    fn default() -> Self {
        Self {
            sample: PathBuf::new(),
            lokey: 0,
            hikey: 127,
            pitch_keycenter: 60,
            lovel: 0,
            hivel: 127,
            seq_length: 1,
            seq_position: 1,
            trigger: Trigger::Attack,
            loop_mode: LoopMode::NoLoop,
            loop_start: 0,
            loop_end: None,
            offset: 0,
            end: None,
            tune_cents: 0.0,
            transpose: 0,
            volume_db: 0.0,
            pan: 0.0,
        }
    }
}

#[derive(Clone, Debug, Default)]
struct ScopeState {
    default_path: String,
    region: RegionDef,
    sample_raw: Option<String>,
}

/// Parse MIDI note number (`0..=127`) or SFZ note name (`c4` -> 60, `a4` -> 69, `c-1` -> 0).
pub fn parse_note(s: &str) -> Option<u8> {
    let s = s.trim();
    if let Ok(n) = s.parse::<i32>() {
        return Some(n.clamp(0, 127) as u8);
    }
    let bytes = s.as_bytes();
    if bytes.is_empty() {
        return None;
    }
    let base = match (bytes[0] as char).to_ascii_lowercase() {
        'c' => 0,
        'd' => 2,
        'e' => 4,
        'f' => 5,
        'g' => 7,
        'a' => 9,
        'b' => 11,
        _ => return None,
    };
    let mut idx = 1;
    let mut semi = base;
    if idx < bytes.len() {
        match bytes[idx] as char {
            '#' => {
                semi += 1;
                idx += 1;
            }
            'b' | 'B' => {
                semi -= 1;
                idx += 1;
            }
            _ => {}
        }
    }
    let octave: i32 = s[idx..].parse().ok()?;
    let midi = (octave + 1) * 12 + semi;
    Some(midi.clamp(0, 127) as u8)
}

#[derive(Clone, Debug)]
enum Token<'a> {
    Header(&'a str),
    Opcode(&'a str, &'a str),
}

fn tokenize(src: &str) -> Vec<Token<'_>> {
    // Strip `//` comments line-by-line first into clean byte slices won't preserve indices,
    // so process each line after stripping `//...`.
    let mut out = Vec::new();
    for raw_line in src.lines() {
        let line = raw_line.split("//").next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let mut i = 0;
        let bytes = line.as_bytes();
        while i < bytes.len() {
            while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            if i >= bytes.len() {
                break;
            }
            if bytes[i] == b'<'
                && let Some(rel_end) = line[i..].find('>')
            {
                let hdr = &line[i + 1..i + rel_end];
                out.push(Token::Header(hdr.trim()));
                i += rel_end + 1;
                continue;
            }
            // Scan `key=value` where value stops right before the next `<header>` or next `opcode=`.
            let Some(eq_rel) = line[i..].find('=') else { break };
            let key = line[i..i + eq_rel].trim();
            let val_start = i + eq_rel + 1;
            let val_end = find_next_boundary(line, val_start);
            let val = line[val_start..val_end].trim();
            if !key.is_empty() {
                out.push(Token::Opcode(key, val));
            }
            i = val_end;
        }
    }
    out
}

fn find_next_boundary(line: &str, start: usize) -> usize {
    let bytes = line.as_bytes();
    let mut i = start;
    while i < bytes.len() {
        if bytes[i] == b'<' {
            return i;
        }
        if bytes[i].is_ascii_whitespace() {
            // Check if the next non-whitespace word looks like `identifier=`.
            let mut j = i;
            while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                j += 1;
            }
            let k = j;
            while j < bytes.len() && (bytes[j].is_ascii_alphanumeric() || bytes[j] == b'_') {
                j += 1;
            }
            if j > k && j < bytes.len() && bytes[j] == b'=' {
                return i;
            }
        }
        i += 1;
    }
    bytes.len()
}

fn apply_opcode(state: &mut ScopeState, key: &str, val: &str) {
    match key.to_ascii_lowercase().as_str() {
        "default_path" => state.default_path = val.to_string(),
        "sample" => state.sample_raw = Some(val.to_string()),
        "lokey" => {
            if let Some(n) = parse_note(val) {
                state.region.lokey = n;
            }
        }
        "hikey" => {
            if let Some(n) = parse_note(val) {
                state.region.hikey = n;
            }
        }
        "key" => {
            if let Some(n) = parse_note(val) {
                state.region.lokey = n;
                state.region.hikey = n;
                state.region.pitch_keycenter = i16::from(n);
            }
        }
        "pitch_keycenter" => {
            if let Some(n) = parse_note(val) {
                state.region.pitch_keycenter = i16::from(n);
            }
        }
        "lovel" => {
            if let Ok(v) = val.parse::<u8>() {
                state.region.lovel = v.min(127);
            }
        }
        "hivel" => {
            if let Ok(v) = val.parse::<u8>() {
                state.region.hivel = v.min(127);
            }
        }
        "seq_length" => {
            if let Ok(v) = val.parse::<u8>() {
                state.region.seq_length = v.max(1);
            }
        }
        "seq_position" => {
            if let Ok(v) = val.parse::<u8>() {
                state.region.seq_position = v.max(1);
            }
        }
        "trigger" => {
            state.region.trigger = match val.to_ascii_lowercase().as_str() {
                "release" | "release_key" => Trigger::Release,
                "first" => Trigger::First,
                "legato" => Trigger::Legato,
                _ => Trigger::Attack,
            };
        }
        "loop_mode" => {
            state.region.loop_mode = match val.to_ascii_lowercase().as_str() {
                "one_shot" => LoopMode::OneShot,
                "loop_continuous" => LoopMode::Continuous,
                "loop_sustain" => LoopMode::Sustain,
                _ => LoopMode::NoLoop,
            };
        }
        "loop_start" => {
            if let Ok(v) = val.parse::<usize>() {
                state.region.loop_start = v;
            }
        }
        "loop_end" => {
            if let Ok(v) = val.parse::<usize>() {
                state.region.loop_end = Some(v);
            }
        }
        "offset" => {
            if let Ok(v) = val.parse::<usize>() {
                state.region.offset = v;
            }
        }
        "end" => {
            if let Ok(v) = val.parse::<isize>() {
                state.region.end = if v < 0 { None } else { Some(v as usize) };
            }
        }
        "tune" => {
            if let Ok(v) = val.parse::<f32>() {
                state.region.tune_cents = v;
            }
        }
        "transpose" => {
            if let Ok(v) = val.parse::<i32>() {
                state.region.transpose = v;
            }
        }
        "volume" => {
            if let Ok(v) = val.parse::<f32>() {
                state.region.volume_db = v;
            }
        }
        "pan" => {
            if let Ok(v) = val.parse::<f32>() {
                state.region.pan = v.clamp(-100.0, 100.0);
            }
        }
        _ => {}
    }
}

fn finish_region(base_dir: &Path, state: &ScopeState, out: &mut Vec<RegionDef>) {
    let Some(raw) = &state.sample_raw else { return };
    if raw.is_empty() || raw.starts_with('*') {
        return; // Ignore built-in SFZ generators like *sine/*noise if unmapped.
    }
    let clean_rel = raw.replace('\\', "/");
    let full = if state.default_path.is_empty() {
        base_dir.join(clean_rel)
    } else {
        base_dir.join(state.default_path.replace('\\', "/")).join(clean_rel)
    };
    let mut r = state.region.clone();
    r.sample = full;
    if r.lokey > r.hikey {
        std::mem::swap(&mut r.lokey, &mut r.hikey);
    }
    if r.lovel > r.hivel {
        std::mem::swap(&mut r.lovel, &mut r.hivel);
    }
    r.seq_length = r.seq_length.max(1);
    r.seq_position = r.seq_position.clamp(1, r.seq_length);
    out.push(r);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Section {
    Control,
    Global,
    Group,
    Region,
    Other,
}

/// Parse an SFZ document relative to `base_dir` into concrete `RegionDef` zones.
pub fn parse_sfz(src: &str, base_dir: &Path) -> Vec<RegionDef> {
    let mut global = ScopeState::default();
    let mut group = ScopeState::default();
    let mut current = ScopeState::default();
    let mut section = Section::Global;
    let mut regions = Vec::new();

    for tok in tokenize(src) {
        match tok {
            Token::Header(name) => {
                if section == Section::Region {
                    finish_region(base_dir, &current, &mut regions);
                }
                match name.to_ascii_lowercase().as_str() {
                    "control" => section = Section::Control,
                    "global" => {
                        section = Section::Global;
                        global = ScopeState { default_path: global.default_path, ..ScopeState::default() };
                    }
                    "group" => {
                        section = Section::Group;
                        group = global.clone();
                    }
                    "region" => {
                        section = Section::Region;
                        current = group.clone();
                    }
                    _ => section = Section::Other,
                }
            }
            Token::Opcode(k, v) => match section {
                Section::Control => apply_opcode(&mut global, k, v),
                Section::Global => apply_opcode(&mut global, k, v),
                Section::Group => apply_opcode(&mut group, k, v),
                Section::Region => apply_opcode(&mut current, k, v),
                Section::Other => {}
            },
        }
    }
    if section == Section::Region {
        finish_region(base_dir, &current, &mut regions);
    }
    regions
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn note_names_and_numbers_parse() {
        assert_eq!(parse_note("60"), Some(60));
        assert_eq!(parse_note("c4"), Some(60));
        assert_eq!(parse_note("A4"), Some(69));
        assert_eq!(parse_note("f#3"), Some(54));
        assert_eq!(parse_note("Bb2"), Some(46));
    }

    #[test]
    fn parses_global_group_region_round_robin_and_release() {
        let sfz = r#"
<control>
default_path=Samples/
<global>
loop_mode=no_loop
<group> lokey=60 hikey=64 seq_length=2
<region> seq_position=1 sample=Piano C4 rr1.wav
<region> seq_position=2 sample=Piano C4 rr2.wav
<group> trigger=release lokey=60 hikey=64
<region> sample=Key Rel.wav volume=-6.0
"#;
        let zones = parse_sfz(sfz, Path::new("/kit"));
        assert_eq!(zones.len(), 3);
        assert_eq!(zones[0].sample, PathBuf::from("/kit/Samples/Piano C4 rr1.wav"));
        assert_eq!(zones[0].seq_length, 2);
        assert_eq!(zones[0].seq_position, 1);
        assert_eq!(zones[1].seq_position, 2);
        assert_eq!(zones[2].trigger, Trigger::Release);
        assert!((zones[2].volume_db + 6.0).abs() < 1e-4);
    }
}
