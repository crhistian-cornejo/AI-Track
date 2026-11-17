//! Words to a [`GenRequest`]: Spanish and English keywords for the simple
//! cases ("un bajo rodante en La menor de 2 compases", "acordes para el
//! drop", "a hats pattern with swing"). It reads a kind, a genre, the number
//! of bars, a key, density, energy, swing and a few named patterns. Anything
//! it does not know is left at the default; the text stays in
//! `free_text` for a model to read later.

use aitrack_model::{Key, Scale, ScaleKind};

use super::GenContext;
use super::spec::{Energy, Feel, GenRequest, Genre, Kind, Pattern, Role};

/// Lowercase, no accents, words split on anything but letters, digits and `#/&`.
fn tokens(text: &str) -> Vec<String> {
    let plain: String = text
        .to_lowercase()
        .chars()
        .map(|c| match c {
            'á' | 'à' | 'ä' => 'a',
            'é' | 'è' | 'ë' => 'e',
            'í' | 'ì' | 'ï' => 'i',
            'ó' | 'ò' | 'ö' => 'o',
            'ú' | 'ù' | 'ü' => 'u',
            'ñ' => 'n',
            '♯' => '#',
            '♭' => 'b',
            c => c,
        })
        .collect();
    plain.split(|c: char| !(c.is_alphanumeric() || matches!(c, '#' | '/' | '&'))).filter(|w| !w.is_empty()).map(str::to_owned).collect()
}

fn has(t: &[String], words: &[&str]) -> bool {
    t.iter().any(|w| words.contains(&w.as_str()))
}

fn has_seq(t: &[String], seq: &[&str]) -> bool {
    t.windows(seq.len()).any(|w| w.iter().zip(seq).all(|(a, b)| a == b))
}

fn number(w: &str) -> Option<u32> {
    w.parse().ok().or(match w {
        "un" | "una" | "uno" | "one" => Some(1),
        "dos" | "two" => Some(2),
        "tres" | "three" => Some(3),
        "cuatro" | "four" => Some(4),
        "seis" | "six" => Some(6),
        "ocho" | "eight" => Some(8),
        "dieciseis" | "sixteen" => Some(16),
        _ => None,
    })
}

fn bars(t: &[String]) -> Option<u32> {
    t.windows(2).find_map(|w| {
        let unit = w[1].starts_with("compas") || w[1].starts_with("bar") || w[1].starts_with("measure");
        if unit { number(&w[0]) } else { None }
    })
}

fn mode(w: &str) -> Option<ScaleKind> {
    Some(match w {
        "minor" | "min" | "menor" | "m" => ScaleKind::Minor,
        "major" | "maj" | "mayor" => ScaleKind::Major,
        "dorian" | "dorico" => ScaleKind::Dorian,
        "phrygian" | "frigio" => ScaleKind::Phrygian,
        "lydian" | "lidio" => ScaleKind::Lydian,
        "mixolydian" | "mixolidio" => ScaleKind::Mixolydian,
        "locrian" | "locrio" => ScaleKind::Locrian,
        _ => return None,
    })
}

/// A note name: "a", "f#", "bb", "la", "sol", "do#".
fn note(w: &str) -> Option<Key> {
    let (base, rest) = match w {
        w if w.starts_with("sol") => (7, &w[3..]),
        w if w.starts_with("do") => (0, &w[2..]),
        w if w.starts_with("re") => (2, &w[2..]),
        w if w.starts_with("mi") => (4, &w[2..]),
        w if w.starts_with("fa") => (5, &w[2..]),
        w if w.starts_with("la") => (9, &w[2..]),
        w if w.starts_with("si") => (11, &w[2..]),
        w if w.len() <= 2 && w.chars().next().is_some_and(|c| ('a'..='g').contains(&c)) => (match &w[..1] {
            "c" => 0,
            "d" => 2,
            "e" => 4,
            "f" => 5,
            "g" => 7,
            "a" => 9,
            _ => 11,
        }, &w[1..]),
        _ => return None,
    };
    let shift: i32 = match rest {
        "" => 0,
        "#" => 1,
        "b" => -1,
        _ => return None,
    };
    Some(Key((base + shift).rem_euclid(12) as u8))
}

fn key_of(t: &[String]) -> Option<Scale> {
    for (i, w) in t.iter().enumerate() {
        // "la menor", "a minor", "f# major", "sol dorico". The English article "a" only
        // counts after a word that introduces a key ("in a minor"), never in "a major chord".
        let introduced = i > 0 && matches!(t[i - 1].as_str(), "en" | "in" | "key" | "tonalidad" | "de" | "of");
        if let (Some(k), Some(kind)) = (note(w), t.get(i + 1).and_then(|n| mode(n)))
            && (w != "a" || introduced)
        {
            return Some(Scale::new(k, kind));
        }
        // "am", "f#m", "cmin", "ebminor": the mode glued on.
        for suffix in ["minor", "min", "major", "maj", "m"] {
            if let Some(stem) = w.strip_suffix(suffix)
                && !stem.is_empty()
                && stem.len() <= 2
                && let Some(k) = note(stem)
            {
                let explicit = suffix != "m" || stem.contains('#') || stem.len() == 2;
                let after_in = i > 0 && matches!(t[i - 1].as_str(), "en" | "in" | "key" | "tonalidad" | "de" | "of");
                if explicit || after_in {
                    let kind = if suffix.starts_with("maj") { ScaleKind::Major } else { ScaleKind::Minor };
                    return Some(Scale::new(k, kind));
                }
            }
        }
    }
    None
}

fn genre_of(t: &[String], ctx: &GenContext, pattern: Option<Pattern>) -> Genre {
    let g = if has_seq(t, &["deep", "house"]) || has(t, &["deephouse"]) {
        Genre::DeepHouse
    } else if has_seq(t, &["tech", "house"]) || has(t, &["techhouse"]) {
        Genre::TechHouse
    } else if has(t, &["techno"]) {
        Genre::Techno
    } else if has(t, &["trance", "progressive", "progresivo"]) {
        Genre::Trance
    } else if has_seq(t, &["drum", "and", "bass"]) || has_seq(t, &["drum", "n", "bass"]) || has(t, &["dnb", "d&b", "jungle"]) {
        Genre::Dnb
    } else if has(t, &["dubstep", "halftime", "half-time"]) || has_seq(t, &["half", "time"]) {
        Genre::Dubstep
    } else if has(t, &["trap", "drill"]) {
        Genre::Trap
    } else if has(t, &["garage", "2step", "2-step"]) {
        Genre::UkGarage
    } else if has(t, &["reggaeton", "dembow", "reguetón"]) {
        Genre::Reggaeton
    } else if has(t, &["house"]) {
        Genre::House
    } else if has(t, &["pop"]) {
        Genre::Pop
    } else {
        Genre::Other
    };
    if g != Genre::Other {
        return g;
    }
    // A rolling bass wants a techno-ish context; otherwise the tempo hints.
    if pattern == Some(Pattern::Rolling) {
        return Genre::TechHouse;
    }
    match ctx.bpm {
        b if b >= 165. => Genre::Dnb,
        b if b <= 95. => Genre::Trap,
        b if (129. ..=140.).contains(&b) => Genre::TechHouse,
        _ => Genre::House,
    }
}

/// The part the words ask for, with the drum roles when they name some.
fn kind_of(t: &[String]) -> Option<(Kind, Vec<Role>)> {
    if has(t, &["arpegio", "arpegios", "arp", "arpeggio", "arpeggios", "arpegiado"]) {
        return Some((Kind::Arp, vec![]));
    }
    if has(t, &["melodia", "melodias", "melody", "lead", "melodic", "melodica"]) {
        return Some((Kind::Melody, vec![]));
    }
    if has(t, &["acordes", "acorde", "chords", "chord", "stabs", "stab", "pad", "pads", "progresion", "progression", "armonia", "harmony"]) {
        return Some((Kind::Chords, vec![]));
    }
    if has(t, &["bajo", "bajos", "bass", "bassline", "sub", "808"]) {
        return Some((Kind::Bass, vec![]));
    }
    if has(t, &["hats", "hat", "hihat", "hihats", "charles", "hi-hat"]) {
        return Some((Kind::Drums, vec![Role::Hat, Role::OpenHat]));
    }
    if has(t, &["bombo", "kick", "kicks"]) {
        return Some((Kind::Drums, vec![Role::Kick]));
    }
    if has(t, &["caja", "snare", "clap", "claps", "palmas"]) {
        return Some((Kind::Drums, vec![Role::Snare, Role::Clap]));
    }
    if has(t, &["perc", "percusion", "percussion", "shaker", "bongos", "congas"]) {
        return Some((Kind::Perc, vec![]));
    }
    if has(t, &["bateria", "drums", "drum", "beat", "ritmo", "groove", "patron"]) {
        return Some((Kind::Drums, vec![]));
    }
    None
}

/// A request from free text, or `None` if it names no kind of part.
pub fn parse(text: &str, ctx: &GenContext) -> Option<GenRequest> {
    let t = tokens(text);
    let (kind, only) = kind_of(&t)?;
    Some(build(kind, only, text, &t, ctx))
}

/// A request for a known kind (a `/bass` command) with optional words after it.
pub fn parse_for(kind: Kind, text: &str, ctx: &GenContext) -> GenRequest {
    let t = tokens(text);
    let only = match (kind, kind_of(&t)) {
        (Kind::Drums, Some((Kind::Drums, only))) => only,
        _ => vec![],
    };
    build(kind, only, text, &t, ctx)
}

fn build(kind: Kind, only: Vec<Role>, text: &str, t: &[String], ctx: &GenContext) -> GenRequest {
    let mut req = GenRequest::new(kind);
    req.free_text = text.trim().to_owned();
    req.only = only;
    req.bars = bars(t).unwrap_or(4).clamp(1, super::spec::MAX_BARS);
    req.key = key_of(t);

    // Named patterns.
    req.pattern = if has(t, &["rodante", "rolling", "rueda"]) {
        Some(Pattern::Rolling)
    } else if has(t, &["offbeat", "contratiempo", "contratiempos", "off-beat"]) {
        Some(Pattern::Offbeat)
    } else if has(t, &["octavas", "octaves", "octava"]) {
        Some(Pattern::Octaves)
    } else if has(t, &["808", "sub808"]) && kind == Kind::Bass {
        Some(Pattern::Sub808)
    } else if has_seq(t, &["ida", "y", "vuelta"]) || has_seq(t, &["up", "down"]) || has(t, &["updown", "pingpong", "ping-pong"]) {
        Some(Pattern::UpDown)
    } else if has(t, &["aleatorio", "random"]) {
        Some(Pattern::Random)
    } else if has(t, &["descendente", "descending", "abajo", "down"]) && kind == Kind::Arp {
        Some(Pattern::Down)
    } else if has(t, &["ascendente", "ascending", "arriba", "up"]) && kind == Kind::Arp {
        Some(Pattern::Up)
    } else if has(t, &["stabs", "stab"]) && kind == Kind::Chords {
        Some(Pattern::StabsOffbeat)
    } else if has(t, &["sincopa", "sincopado", "sincopados", "syncopated", "syncopation", "sync"]) {
        Some(Pattern::Sync332)
    } else if has(t, &["sostenido", "sostenidos", "largos", "held", "sustained", "whole"]) && kind == Kind::Chords {
        Some(Pattern::Whole)
    } else {
        None
    };

    req.genre = genre_of(t, ctx, req.pattern);

    // "replace the notes of this clip".
    req.replace = has(t, &["reemplaza", "reemplazar", "sustituye", "sustituir", "replace", "overwrite", "sobrescribe", "rellena"]) || has_seq(t, &["en", "este", "clip"]) || has_seq(t, &["in", "this", "clip"]);

    // Groove.
    if has_seq(t, &["sin", "swing"]) || has_seq(t, &["no", "swing"]) || has(t, &["recto", "straight", "rigido"]) {
        req.feel = Some(Feel::Straight);
    } else if has(t, &["shuffle"]) {
        req.feel = Some(Feel::Shuffle);
    } else if has_seq(t, &["light", "swing"]) || has_seq(t, &["swing", "ligero"]) || has_seq(t, &["ligero", "swing"]) {
        req.feel = Some(Feel::LightSwing);
    } else if has(t, &["swing", "swingueado", "swingueada", "swung"]) {
        req.feel = Some(Feel::Swing);
    } else if has(t, &["suelto", "suelta", "loose", "humano", "humana"]) {
        req.feel = Some(Feel::Loose);
    }
    if has_seq(t, &["sin", "humanizar"]) || has_seq(t, &["no", "humanize"]) || has(t, &["cuantizado", "cuantizada", "quantized", "robotico", "robot", "exacto"]) {
        req.humanize = false;
    }

    // Density and energy.
    if has(t, &["denso", "densa", "dense", "busy", "lleno", "llena", "complejo", "compleja", "complex", "1/16", "semicorcheas", "sixteenths"]) {
        req.density = 0.85;
    } else if has(t, &["aireado", "aireada", "airy", "sparse", "minimal", "simple", "ligero", "ligera", "sutil", "1/8", "corcheas", "eighths", "espaciado"]) {
        req.density = 0.2;
    }
    if has(t, &["drop", "potente", "powerful", "peak", "energico", "energetic", "agresivo", "aggressive"]) || has_seq(t, &["high", "energy"]) || has_seq(t, &["alta", "energia"]) {
        req.energy = Energy::High;
    } else if has(t, &["breakdown", "break", "calmado", "calmada", "suave", "chill", "calm", "soft", "intro"]) || has_seq(t, &["low", "energy"]) || has_seq(t, &["baja", "energia"]) {
        req.energy = Energy::Low;
    }

    // Mood moves the mode when no key was named.
    if req.key.is_none() {
        let root = ctx.scale.map_or(Key(9), |s| s.root);
        let minorish = ctx.scale.is_none_or(|s| s.kind.intervals().get(2) == Some(&3));
        if has(t, &["oscuro", "oscura", "dark", "sombrio", "sombria"]) {
            req.key = Some(Scale::new(root, if minorish { ScaleKind::Phrygian } else { ScaleKind::Minor }));
        } else if has(t, &["brillante", "bright", "luminoso", "luminosa", "alegre", "happy"]) {
            req.key = Some(Scale::new(root, if minorish { ScaleKind::Dorian } else { ScaleKind::Lydian }));
        }
    }

    // "seed 12" / "semilla 12".
    req.seed = t.windows(2).find_map(|w| if matches!(w[0].as_str(), "seed" | "semilla") { w[1].parse().ok() } else { None });
    req
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_es(text: &str) -> GenRequest {
        parse(text, &GenContext::default()).unwrap_or_else(|| panic!("no kind in {text:?}"))
    }

    #[test]
    fn a_rolling_bass_in_a_minor_of_two_bars() {
        let r = parse_es("un bajo rodante en La menor de 2 compases");
        assert_eq!(r.kind, Kind::Bass);
        assert_eq!(r.pattern, Some(Pattern::Rolling));
        assert_eq!(r.key, Some(Scale::new(Key(9), ScaleKind::Minor)));
        assert_eq!(r.bars, 2);
        assert_eq!(r.genre, Genre::TechHouse);
    }

    #[test]
    fn chords_for_the_drop() {
        let r = parse_es("acordes para el drop");
        assert_eq!((r.kind, r.energy, r.bars), (Kind::Chords, Energy::High, 4));
        assert_eq!(r.key, None, "'el' and 'para' are not notes");
    }

    #[test]
    fn a_hats_pattern_with_swing() {
        let r = parse_es("un patrón de hats con swing");
        assert_eq!(r.kind, Kind::Drums);
        assert_eq!(r.only, vec![Role::Hat, Role::OpenHat]);
        assert_eq!(r.feel, Some(Feel::Swing));
    }

    #[test]
    fn an_arpeggio_up_and_down_in_sixteenths() {
        let r = parse_es("un arpegio ida y vuelta de 1/16 sobre estos acordes");
        assert_eq!((r.kind, r.pattern), (Kind::Arp, Some(Pattern::UpDown)));
        assert!(r.density > 0.7);
    }

    #[test]
    fn english_works_too() {
        let r = parse_es("a deep house bass in F# minor, 8 bars, no swing, sparse");
        assert_eq!((r.kind, r.genre, r.bars, r.feel), (Kind::Bass, Genre::DeepHouse, 8, Some(Feel::Straight)));
        assert_eq!(r.key, Some(Scale::new(Key(6), ScaleKind::Minor)));
        assert!(r.density < 0.3);
        let r = parse_es("tech house drums, unquantized? no: quantized");
        assert!(!r.humanize && r.genre == Genre::TechHouse);
    }

    #[test]
    fn keys_in_several_spellings() {
        let key = |s: &str| key_of(&tokens(s));
        assert_eq!(key("en sol mayor"), Some(Scale::new(Key(7), ScaleKind::Major)));
        assert_eq!(key("c minor"), Some(Scale::new(Key(0), ScaleKind::Minor)));
        assert_eq!(key("in Am please"), Some(Scale::new(Key(9), ScaleKind::Minor)));
        assert_eq!(key("F#m"), Some(Scale::new(Key(6), ScaleKind::Minor)));
        assert_eq!(key("Bbmaj"), Some(Scale::new(Key(10), ScaleKind::Major)));
        assert_eq!(key("re dorico"), Some(Scale::new(Key(2), ScaleKind::Dorian)));
        assert_eq!(key("a bass line"), None, "an article is not a key");
        assert_eq!(key("I am here"), None);
        assert_eq!(key("in a minor"), Some(Scale::new(Key(9), ScaleKind::Minor)));
        assert_eq!(key("give me a major chord progression"), None, "an article, not a key");
        assert_eq!(key("a minor change"), None);
        assert_eq!(key("la bateria"), None);
    }

    #[test]
    fn replace_words_are_read() {
        assert!(parse_es("reemplaza el bajo de este clip").replace);
        assert!(parse_es("bass in this clip").replace);
        assert!(!parse_es("un bajo rodante").replace);
    }

    #[test]
    fn nothing_to_go_on_is_none() {
        assert!(parse("hola, qué tal", &GenContext::default()).is_none());
        assert!(parse("", &GenContext::default()).is_none());
    }

    #[test]
    fn a_command_with_words_after_it() {
        let ctx = GenContext { bpm: 172., ..Default::default() };
        let r = parse_for(Kind::Drums, "snare only, dnb", &ctx);
        assert_eq!((r.genre, r.only), (Genre::Dnb, vec![Role::Snare, Role::Clap]));
        let r = parse_for(Kind::Bass, "", &ctx);
        assert_eq!((r.kind, r.genre, r.bars), (Kind::Bass, Genre::Dnb, 4), "the tempo hints the genre");
        let r = parse_for(Kind::Chords, "oscuro seed 7", &GenContext::default());
        assert_eq!(r.seed, Some(7));
        assert_eq!(r.key.map(|s| s.kind), Some(ScaleKind::Phrygian));
    }

    #[test]
    fn wild_text_never_panics() {
        for t in ["💥💥 bajo 💥", "bajo ".repeat(2000).as_str(), "999999999999999999999 compases bajo", "0 bars bass", "bass in # minor", "bajo en ♯ menor"] {
            let r = parse(t, &GenContext::default()).unwrap();
            assert!((1..=16).contains(&r.bars));
        }
    }
}
