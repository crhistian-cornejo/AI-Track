//! Filename and folder parsing: key, BPM, category, kind and genre hints.
//!
//! Commercial and free packs encode a lot in names: `Kick_Am_128bpm.wav`,
//! `Cymatics - Lofi Loop 1 - 90 BPM F#m.wav`, `808s/808 Sub (C).wav`,
//! `Loops/Drum Loops/Reggaeton Dembow 95.wav`. This module turns those names
//! into [`NameHints`]; the scanner merges them with audio metadata. English
//! and Spanish keywords are recognised.

use std::collections::HashMap;
use std::path::Path;
use std::sync::OnceLock;

use aitrack_model::{Key, Scale, ScaleKind};

use crate::taxonomy::{Category, SampleKind};

/// A key found in a name. `kind` is `None` when only the root is known
/// (e.g. `Bass C.wav`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyHint {
    pub root: Key,
    pub kind: Option<ScaleKind>,
}

impl KeyHint {
    pub fn scale(self) -> Option<Scale> {
        self.kind.map(|k| Scale::new(self.root, k))
    }
}

/// Everything the parser could infer from a path.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct NameHints {
    pub key: Option<KeyHint>,
    /// MIDI note of a multisample/one-shot (`C4` = 60), from tokens like `A0v1` or `C#3`.
    pub root_note: Option<u8>,
    pub bpm: Option<f32>,
    pub category: Option<Category>,
    pub subcategory: Option<&'static str>,
    /// Only set when the name says so explicitly (`loop`, `one shot`, `chop`).
    pub kind: Option<SampleKind>,
    /// "Top loop" (drum loop without kick).
    pub top_loop: bool,
    /// Canonical genre names, deduplicated, in order of appearance.
    pub genres: Vec<&'static str>,
}

// ---------------------------------------------------------------------------
// Keyword tables
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
enum Hit {
    /// Category, subcategory, priority.
    Cat(Category, Option<&'static str>, u8),
    Kind(SampleKind),
    Top,
    Genre(&'static str),
    /// Matches a category and also marks the sample as a loop (breakbeats).
    CatLoop(Category, Option<&'static str>, u8),
}

use Category::*;

const CAT_WORDS: &[(&str, Category, Option<&str>, u8)] = &[
    // Drums: specific pieces win over everything except FX transitions.
    ("kick", Drums, Some("Kicks"), 10),
    ("kicks", Drums, Some("Kicks"), 10),
    ("kik", Drums, Some("Kicks"), 10),
    ("kck", Drums, Some("Kicks"), 10),
    ("bd", Drums, Some("Kicks"), 10),
    ("bassdrum", Drums, Some("Kicks"), 10),
    ("bass drum", Drums, Some("Kicks"), 10),
    ("kick drum", Drums, Some("Kicks"), 10),
    ("bombo", Drums, Some("Kicks"), 10),
    ("bombos", Drums, Some("Kicks"), 10),
    ("snare", Drums, Some("Snares"), 10),
    ("snares", Drums, Some("Snares"), 10),
    ("snr", Drums, Some("Snares"), 10),
    ("sn", Drums, Some("Snares"), 10),
    ("sd", Drums, Some("Snares"), 10),
    ("caja", Drums, Some("Snares"), 10),
    ("redoblante", Drums, Some("Snares"), 10),
    ("clap", Drums, Some("Claps"), 10),
    ("claps", Drums, Some("Claps"), 10),
    ("clp", Drums, Some("Claps"), 10),
    ("cp", Drums, Some("Claps"), 10),
    ("palmas", Drums, Some("Claps"), 10),
    ("palma", Drums, Some("Claps"), 10),
    ("aplauso", Drums, Some("Claps"), 10),
    ("snap", Drums, Some("Claps"), 10),
    ("snaps", Drums, Some("Claps"), 10),
    ("chasquido", Drums, Some("Claps"), 10),
    ("hat", Drums, Some("Hats"), 10),
    ("hats", Drums, Some("Hats"), 10),
    ("hihat", Drums, Some("Hats"), 10),
    ("hihats", Drums, Some("Hats"), 10),
    ("hi hat", Drums, Some("Hats"), 10),
    ("hi hats", Drums, Some("Hats"), 10),
    ("hh", Drums, Some("Hats"), 10),
    ("chh", Drums, Some("Hats"), 10),
    ("ohh", Drums, Some("Hats"), 10),
    ("closedhat", Drums, Some("Hats"), 10),
    ("openhat", Drums, Some("Hats"), 10),
    ("charles", Drums, Some("Hats"), 10),
    ("perc", Drums, Some("Percs"), 10),
    ("percs", Drums, Some("Percs"), 10),
    ("percussion", Drums, Some("Percs"), 10),
    ("percusion", Drums, Some("Percs"), 10),
    ("tom", Drums, Some("Percs"), 10),
    ("toms", Drums, Some("Percs"), 10),
    ("conga", Drums, Some("Percs"), 10),
    ("congas", Drums, Some("Percs"), 10),
    ("bongo", Drums, Some("Percs"), 10),
    ("bongos", Drums, Some("Percs"), 10),
    ("shaker", Drums, Some("Percs"), 10),
    ("shakers", Drums, Some("Percs"), 10),
    ("tambourine", Drums, Some("Percs"), 10),
    ("tamb", Drums, Some("Percs"), 10),
    ("pandereta", Drums, Some("Percs"), 10),
    ("cowbell", Drums, Some("Percs"), 10),
    ("cencerro", Drums, Some("Percs"), 10),
    ("clave", Drums, Some("Percs"), 10),
    ("claves", Drums, Some("Percs"), 10),
    ("guiro", Drums, Some("Percs"), 10),
    ("guira", Drums, Some("Percs"), 10),
    ("guiras", Drums, Some("Percs"), 10),
    ("timbal", Drums, Some("Percs"), 10),
    ("timbale", Drums, Some("Percs"), 10),
    ("timbales", Drums, Some("Percs"), 10),
    ("woodblock", Drums, Some("Percs"), 10),
    ("rim", Drums, Some("Percs"), 10),
    ("rims", Drums, Some("Percs"), 10),
    ("rimshot", Drums, Some("Percs"), 10),
    ("tabla", Drums, Some("Percs"), 10),
    ("djembe", Drums, Some("Percs"), 10),
    ("cabasa", Drums, Some("Percs"), 10),
    ("maraca", Drums, Some("Percs"), 10),
    ("maracas", Drums, Some("Percs"), 10),
    ("triangle", Drums, Some("Percs"), 10),
    ("triangulo", Drums, Some("Percs"), 10),
    ("agogo", Drums, Some("Percs"), 10),
    ("cajon", Drums, Some("Percs"), 10),
    ("timpani", Drums, Some("Percs"), 7),
    ("timp", Drums, Some("Percs"), 7),
    ("gong", Drums, Some("Cymbals"), 7),
    ("cymbal", Drums, Some("Cymbals"), 10),
    ("cymbals", Drums, Some("Cymbals"), 10),
    ("cym", Drums, Some("Cymbals"), 10),
    ("crash", Drums, Some("Cymbals"), 10),
    ("crashes", Drums, Some("Cymbals"), 10),
    ("ride", Drums, Some("Cymbals"), 10),
    ("rides", Drums, Some("Cymbals"), 10),
    ("splash", Drums, Some("Cymbals"), 10),
    ("china", Drums, Some("Cymbals"), 10),
    ("platillo", Drums, Some("Cymbals"), 10),
    ("platillos", Drums, Some("Cymbals"), 10),
    ("808", Drums, Some("808s"), 9),
    ("808s", Drums, Some("808s"), 9),
    ("drum", Drums, None, 2),
    ("drums", Drums, None, 2),
    ("bateria", Drums, None, 2),
    ("fill", Drums, None, 2),
    ("fills", Drums, None, 2),
    ("beat", Drums, None, 1),
    // Bass.
    ("bass", Bass, None, 8),
    ("basses", Bass, None, 8),
    ("bassline", Bass, None, 8),
    ("basslines", Bass, None, 8),
    ("bajo", Bass, None, 8),
    ("bajos", Bass, None, 8),
    ("sub", Bass, None, 8),
    ("subbass", Bass, None, 8),
    ("sub bass", Bass, None, 8),
    ("reese", Bass, None, 8),
    ("wobble", Bass, None, 8),
    // Synths.
    ("lead", Synths, Some("Leads"), 7),
    ("leads", Synths, Some("Leads"), 7),
    ("pad", Synths, Some("Pads"), 7),
    ("pads", Synths, Some("Pads"), 7),
    ("colchon", Synths, Some("Pads"), 7),
    ("pluck", Synths, Some("Plucks"), 7),
    ("plucks", Synths, Some("Plucks"), 7),
    ("keys", Synths, Some("Keys"), 7),
    ("rhodes", Synths, Some("Keys"), 7),
    ("epiano", Synths, Some("Keys"), 8),
    ("e piano", Synths, Some("Keys"), 8),
    ("electric piano", Synths, Some("Keys"), 8),
    ("wurli", Synths, Some("Keys"), 7),
    ("wurlitzer", Synths, Some("Keys"), 7),
    ("clav", Synths, Some("Keys"), 7),
    ("clavinet", Synths, Some("Keys"), 7),
    ("arp", Synths, Some("Arps"), 7),
    ("arps", Synths, Some("Arps"), 7),
    ("arpeggio", Synths, Some("Arps"), 7),
    ("arpeggiator", Synths, Some("Arps"), 7),
    ("arpegio", Synths, Some("Arps"), 7),
    ("chord", Synths, Some("Chords"), 6),
    ("chords", Synths, Some("Chords"), 6),
    ("acorde", Synths, Some("Chords"), 6),
    ("acordes", Synths, Some("Chords"), 6),
    ("stab", Synths, Some("Chords"), 6),
    ("stabs", Synths, Some("Chords"), 6),
    ("synth", Synths, None, 3),
    ("synths", Synths, None, 3),
    ("synthesizer", Synths, None, 3),
    ("sintetizador", Synths, None, 3),
    ("sinte", Synths, None, 3),
    ("melody", Synths, None, 2),
    ("melodia", Synths, None, 2),
    // Vocals.
    ("vocal", Vocals, None, 8),
    ("vocals", Vocals, None, 8),
    ("vox", Vocals, None, 8),
    ("voice", Vocals, None, 8),
    ("voces", Vocals, None, 8),
    ("voz", Vocals, None, 8),
    ("acapella", Vocals, None, 8),
    ("acappella", Vocals, None, 8),
    ("adlib", Vocals, None, 8),
    ("adlibs", Vocals, None, 8),
    ("ad lib", Vocals, None, 8),
    ("chant", Vocals, None, 8),
    ("chants", Vocals, None, 8),
    ("shout", Vocals, None, 8),
    ("grito", Vocals, None, 8),
    // FX: transitions beat drum pieces ("snare riser" is a riser).
    ("riser", Fx, Some("Risers"), 11),
    ("risers", Fx, Some("Risers"), 11),
    ("uplifter", Fx, Some("Risers"), 11),
    ("buildup", Fx, Some("Risers"), 11),
    ("build up", Fx, Some("Risers"), 11),
    ("subida", Fx, Some("Risers"), 11),
    ("impact", Fx, Some("Impacts"), 6),
    ("impacts", Fx, Some("Impacts"), 6),
    ("boom", Fx, Some("Impacts"), 6),
    ("booms", Fx, Some("Impacts"), 6),
    ("golpe", Fx, Some("Impacts"), 6),
    ("explosion", Fx, Some("Impacts"), 6),
    ("subdrop", Fx, Some("Impacts"), 11),
    ("sub drop", Fx, Some("Impacts"), 11),
    ("downer", Fx, Some("Impacts"), 11),
    ("sweep", Fx, Some("Sweeps"), 11),
    ("sweeps", Fx, Some("Sweeps"), 11),
    ("whoosh", Fx, Some("Sweeps"), 11),
    ("woosh", Fx, Some("Sweeps"), 11),
    ("swoosh", Fx, Some("Sweeps"), 11),
    ("swish", Fx, Some("Sweeps"), 11),
    ("downlifter", Fx, Some("Sweeps"), 11),
    ("downsweep", Fx, Some("Sweeps"), 11),
    ("upsweep", Fx, Some("Sweeps"), 11),
    ("transition", Fx, Some("Sweeps"), 11),
    ("transicion", Fx, Some("Sweeps"), 11),
    ("barrido", Fx, Some("Sweeps"), 11),
    ("reverse", Fx, Some("Sweeps"), 5),
    ("atmos", Fx, Some("Atmos"), 6),
    ("atmosphere", Fx, Some("Atmos"), 6),
    ("atmospheres", Fx, Some("Atmos"), 6),
    ("atmosfera", Fx, Some("Atmos"), 6),
    ("ambience", Fx, Some("Atmos"), 6),
    ("ambiente", Fx, Some("Atmos"), 6),
    ("texture", Fx, Some("Atmos"), 6),
    ("textures", Fx, Some("Atmos"), 6),
    ("textura", Fx, Some("Atmos"), 6),
    ("drone", Fx, Some("Atmos"), 6),
    ("drones", Fx, Some("Atmos"), 6),
    ("soundscape", Fx, Some("Atmos"), 6),
    ("ambi", Fx, Some("Atmos"), 6),
    ("vinyl", Fx, None, 5),
    ("scratch", Fx, None, 5),
    ("noise", Fx, Some("Atmos"), 5),
    ("fx", Fx, None, 4),
    ("sfx", Fx, None, 4),
    ("effect", Fx, None, 4),
    ("effects", Fx, None, 4),
    ("efecto", Fx, None, 4),
    ("efectos", Fx, None, 4),
    ("foley", Fx, None, 4),
    ("laser", Fx, None, 5),
    ("zap", Fx, None, 5),
    ("glitch", Fx, None, 5),
    ("blip", Fx, None, 5),
    ("beep", Fx, None, 5),
    // Acoustic instruments.
    ("piano", Instruments, Some("Piano"), 7),
    ("pianos", Instruments, Some("Piano"), 7),
    ("grand piano", Instruments, Some("Piano"), 7),
    ("upright", Instruments, Some("Piano"), 6),
    ("strings", Instruments, Some("Strings"), 7),
    ("string", Instruments, Some("Strings"), 7),
    ("violin", Instruments, Some("Strings"), 7),
    ("violins", Instruments, Some("Strings"), 7),
    ("viola", Instruments, Some("Strings"), 7),
    ("violas", Instruments, Some("Strings"), 7),
    ("cello", Instruments, Some("Strings"), 7),
    ("cellos", Instruments, Some("Strings"), 7),
    ("violonchelo", Instruments, Some("Strings"), 7),
    ("contrabass", Instruments, Some("Strings"), 9),
    ("double bass", Instruments, Some("Strings"), 9),
    ("doublebass", Instruments, Some("Strings"), 9),
    ("cuerdas", Instruments, Some("Strings"), 7),
    ("pizzicato", Instruments, Some("Strings"), 7),
    ("pizz", Instruments, Some("Strings"), 7),
    ("harp", Instruments, Some("Strings"), 7),
    ("arpa", Instruments, Some("Strings"), 7),
    ("brass", Instruments, Some("Brass"), 7),
    ("trumpet", Instruments, Some("Brass"), 7),
    ("trumpets", Instruments, Some("Brass"), 7),
    ("trompeta", Instruments, Some("Brass"), 7),
    ("trombone", Instruments, Some("Brass"), 7),
    ("trombones", Instruments, Some("Brass"), 7),
    ("trombon", Instruments, Some("Brass"), 7),
    ("horn", Instruments, Some("Brass"), 7),
    ("horns", Instruments, Some("Brass"), 7),
    ("tuba", Instruments, Some("Brass"), 7),
    ("metales", Instruments, Some("Brass"), 7),
    ("woodwind", Instruments, Some("Woodwinds"), 7),
    ("woodwinds", Instruments, Some("Woodwinds"), 7),
    ("flute", Instruments, Some("Woodwinds"), 7),
    ("flutes", Instruments, Some("Woodwinds"), 7),
    ("flauta", Instruments, Some("Woodwinds"), 7),
    ("clarinet", Instruments, Some("Woodwinds"), 7),
    ("clarinete", Instruments, Some("Woodwinds"), 7),
    ("oboe", Instruments, Some("Woodwinds"), 7),
    ("bassoon", Instruments, Some("Woodwinds"), 9),
    ("fagot", Instruments, Some("Woodwinds"), 7),
    ("sax", Instruments, Some("Woodwinds"), 7),
    ("saxophone", Instruments, Some("Woodwinds"), 7),
    ("saxofon", Instruments, Some("Woodwinds"), 7),
    ("piccolo", Instruments, Some("Woodwinds"), 7),
    ("recorder", Instruments, Some("Woodwinds"), 6),
    ("marimba", Instruments, Some("Mallets"), 7),
    ("xylophone", Instruments, Some("Mallets"), 7),
    ("xilofono", Instruments, Some("Mallets"), 7),
    ("glockenspiel", Instruments, Some("Mallets"), 7),
    ("glock", Instruments, Some("Mallets"), 7),
    ("xylo", Instruments, Some("Mallets"), 7),
    ("vibes", Instruments, Some("Mallets"), 7),
    ("vibraphone", Instruments, Some("Mallets"), 7),
    ("celesta", Instruments, Some("Mallets"), 7),
    ("bell", Instruments, Some("Mallets"), 6),
    ("bells", Instruments, Some("Mallets"), 6),
    ("campana", Instruments, Some("Mallets"), 6),
    ("campanas", Instruments, Some("Mallets"), 6),
    ("kalimba", Instruments, Some("Mallets"), 7),
    ("chimes", Instruments, Some("Mallets"), 7),
    ("music box", Instruments, Some("Mallets"), 7),
    ("guitar", Instruments, Some("Guitar"), 7),
    ("guitars", Instruments, Some("Guitar"), 7),
    ("guitarra", Instruments, Some("Guitar"), 7),
    ("gtr", Instruments, Some("Guitar"), 7),
    ("guit", Instruments, Some("Guitar"), 7),
    ("ukulele", Instruments, Some("Guitar"), 7),
    ("banjo", Instruments, Some("Guitar"), 7),
    ("choir", Instruments, Some("Choir"), 8),
    ("choirs", Instruments, Some("Choir"), 8),
    ("coro", Instruments, Some("Choir"), 8),
    ("coros", Instruments, Some("Choir"), 8),
    ("organ", Instruments, Some("Organ"), 7),
    ("organo", Instruments, Some("Organ"), 7),
];

const KIND_WORDS: &[(&str, SampleKind)] = &[
    ("loop", SampleKind::Loop),
    ("loops", SampleKind::Loop),
    ("bucle", SampleKind::Loop),
    ("bucles", SampleKind::Loop),
    ("groove", SampleKind::Loop),
    ("grooves", SampleKind::Loop),
    ("chop", SampleKind::Chop),
    ("chops", SampleKind::Chop),
    ("chopped", SampleKind::Chop),
    ("slice", SampleKind::Chop),
    ("slices", SampleKind::Chop),
    ("sliced", SampleKind::Chop),
    ("one shot", SampleKind::OneShot),
    ("one shots", SampleKind::OneShot),
    ("oneshot", SampleKind::OneShot),
    ("oneshots", SampleKind::OneShot),
];

const GENRE_WORDS: &[(&str, &str)] = &[
    ("edm", "EDM"),
    ("big room", "EDM"),
    ("bigroom", "EDM"),
    ("house", "House"),
    ("techhouse", "House"),
    ("techno", "Techno"),
    ("minimal", "Techno"),
    ("trance", "Trance"),
    ("psytrance", "Trance"),
    ("dubstep", "Dubstep"),
    ("riddim", "Dubstep"),
    ("dnb", "Drum & Bass"),
    ("drum and bass", "Drum & Bass"),
    ("drum n bass", "Drum & Bass"),
    ("drumnbass", "Drum & Bass"),
    ("jungle", "Drum & Bass"),
    ("trap", "Trap"),
    ("drill", "Drill"),
    ("hiphop", "Hip-Hop"),
    ("hip hop", "Hip-Hop"),
    ("boom bap", "Hip-Hop"),
    ("boombap", "Hip-Hop"),
    ("rap", "Hip-Hop"),
    ("lofi", "Lo-Fi"),
    ("lo fi", "Lo-Fi"),
    ("chillhop", "Lo-Fi"),
    ("reggaeton", "Reggaeton"),
    ("regueton", "Reggaeton"),
    ("reggueton", "Reggaeton"),
    ("reggaetton", "Reggaeton"),
    ("perreo", "Reggaeton"),
    ("dembow", "Dembow"),
    ("latin", "Latin"),
    ("latino", "Latin"),
    ("moombahton", "Moombahton"),
    ("afro", "Afrobeats"),
    ("afrobeat", "Afrobeats"),
    ("afrobeats", "Afrobeats"),
    ("amapiano", "Amapiano"),
    ("pop", "Pop"),
    ("kpop", "Pop"),
    ("future bass", "Future Bass"),
    ("garage", "UK Garage"),
    ("ukg", "UK Garage"),
    ("phonk", "Phonk"),
    ("ambient", "Ambient"),
    ("cinematic", "Cinematic"),
    ("trailer", "Cinematic"),
    ("orchestral", "Cinematic"),
    ("orchestra", "Cinematic"),
    ("rnb", "R&B"),
    ("r b", "R&B"),
    ("funk", "Funk"),
    ("disco", "Disco"),
    ("jersey club", "Jersey Club"),
    ("chiptune", "Chiptune"),
    ("8bit", "Chiptune"),
    ("8 bit", "Chiptune"),
    ("synthwave", "Synthwave"),
    ("retrowave", "Synthwave"),
    ("hardstyle", "Hardstyle"),
    ("electro", "Electro"),
    ("dancehall", "Dancehall"),
];

/// All canonical genre names, for the virtual tree and UI pickers.
pub fn known_genres() -> Vec<&'static str> {
    let mut v: Vec<&'static str> = GENRE_WORDS.iter().map(|(_, g)| *g).collect();
    v.sort_unstable();
    v.dedup();
    v
}

fn table() -> &'static HashMap<&'static str, Vec<Hit>> {
    static T: OnceLock<HashMap<&'static str, Vec<Hit>>> = OnceLock::new();
    T.get_or_init(|| {
        let mut m: HashMap<&'static str, Vec<Hit>> = HashMap::new();
        for &(w, c, s, p) in CAT_WORDS {
            m.entry(w).or_default().push(Hit::Cat(c, s, p));
        }
        for &(w, k) in KIND_WORDS {
            m.entry(w).or_default().push(Hit::Kind(k));
        }
        for &(w, g) in GENRE_WORDS {
            m.entry(w).or_default().push(Hit::Genre(g));
        }
        for w in ["top", "tops"] {
            m.entry(w).or_default().push(Hit::Top);
        }
        for w in ["toploop", "toploops", "top loop", "top loops"] {
            let e = m.entry(w).or_default();
            e.push(Hit::Top);
            e.push(Hit::Kind(SampleKind::Loop));
        }
        for w in ["break", "breaks", "breakbeat", "breakbeats"] {
            m.entry(w)
                .or_default()
                .push(Hit::CatLoop(Drums, None, 3));
        }
        m
    })
}

// ---------------------------------------------------------------------------
// Tokenizing
// ---------------------------------------------------------------------------

fn fold_char(c: char) -> char {
    match c {
        'á' | 'à' | 'ä' | 'â' | 'Á' | 'À' | 'Ä' | 'Â' => 'a',
        'é' | 'è' | 'ë' | 'ê' | 'É' | 'È' | 'Ë' | 'Ê' => 'e',
        'í' | 'ì' | 'ï' | 'î' | 'Í' | 'Ì' | 'Ï' | 'Î' => 'i',
        'ó' | 'ò' | 'ö' | 'ô' | 'Ó' | 'Ò' | 'Ö' | 'Ô' => 'o',
        'ú' | 'ù' | 'ü' | 'û' | 'Ú' | 'Ù' | 'Ü' | 'Û' => 'u',
        'ñ' | 'Ñ' => 'n',
        'ç' | 'Ç' => 'c',
        c => c.to_ascii_lowercase(),
    }
}

/// Splits on anything that is not alphanumeric, `#`, `♯` or `♭`. Keeps case.
fn raw_tokens(s: &str) -> Vec<String> {
    s.split(|c: char| !(c.is_alphanumeric() || c == '#' || c == '♯' || c == '♭' || c == '.'))
        .flat_map(|t| {
            // Dots only survive inside numbers ("92.5bpm"); elsewhere they split.
            let keep = t.chars().any(|c| c.is_ascii_digit()) && t.contains('.');
            if keep {
                vec![t.trim_matches('.').to_string()]
            } else {
                t.split('.').map(str::to_string).collect()
            }
        })
        .filter(|t| !t.is_empty())
        .collect()
}

/// Lowercase keyword words: camelCase and letter/digit boundaries split.
fn words_of(raw: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut prev: Option<char> = None;
    for c in raw.chars() {
        if c == '#' || c == '♯' || c == '♭' {
            continue;
        }
        let boundary = match prev {
            Some(p) => {
                (p.is_lowercase() && c.is_uppercase())
                    || (p.is_ascii_digit() != c.is_ascii_digit() && c != '.' && p != '.')
            }
            None => false,
        };
        if boundary && !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
        }
        cur.push(fold_char(c));
        prev = Some(c);
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

// ---------------------------------------------------------------------------
// Keys and notes
// ---------------------------------------------------------------------------

/// Parses a note letter plus accidental at the start of `s`.
/// Returns the pitch class and the rest. The letter must be uppercase unless
/// an accidental follows (`f#m` is fine, `am` is not).
fn note_prefix(s: &str) -> Option<(u8, &str)> {
    let mut chars = s.char_indices();
    let (_, l) = chars.next()?;
    let base: i32 = match l.to_ascii_uppercase() {
        'C' => 0,
        'D' => 2,
        'E' => 4,
        'F' => 5,
        'G' => 7,
        'A' => 9,
        'B' => 11,
        _ => return None,
    };
    let rest = &s[l.len_utf8()..];
    let (shift, rest, has_acc) = if let Some(r) = rest.strip_prefix("sharp") {
        (1, r, true)
    } else if let Some(r) = rest.strip_prefix("flat") {
        (-1, r, true)
    } else if let Some(r) = rest.strip_prefix('#').or_else(|| rest.strip_prefix('♯')) {
        (1, r, true)
    } else if let Some(r) = rest.strip_prefix('♭') {
        (-1, r, true)
    } else if let Some(r) = rest.strip_prefix('b') {
        // "Bb", "Ebm", "Abmaj" but not "Bd"/"Bass": a lowercase 'b' is a flat
        // only when nothing alphabetic other than a mode suffix follows.
        if r.is_empty() || mode_suffix(r).is_some() || r.starts_with(|c: char| c.is_ascii_digit() || c == '-') {
            (-1, r, true)
        } else {
            (0, rest, false)
        }
    } else {
        (0, rest, false)
    };
    if !l.is_ascii_uppercase() && !has_acc {
        return None;
    }
    Some((((base + shift).rem_euclid(12)) as u8, rest))
}

/// Mode suffix attached to a note (`m`, `min`, `maj`, `minor`, `major`),
/// optionally followed by a chord extension (`m7`, `maj9`).
fn mode_suffix(s: &str) -> Option<ScaleKind> {
    let trimmed = s.trim_end_matches(|c: char| c.is_ascii_digit());
    let ext = &s[trimmed.len()..];
    if !ext.is_empty() && !matches!(ext, "6" | "7" | "9" | "11" | "13") {
        return None;
    }
    if trimmed == "m" {
        return Some(ScaleKind::Minor);
    }
    mode_word(trimmed)
}

/// A standalone mode word (case-insensitive except the single letter `m`).
fn mode_word(s: &str) -> Option<ScaleKind> {
    if s == "m" {
        return Some(ScaleKind::Minor);
    }
    match s.to_ascii_lowercase().as_str() {
        "min" | "minor" | "menor" | "moll" => Some(ScaleKind::Minor),
        "maj" | "major" | "mayor" | "dur" => Some(ScaleKind::Major),
        "dorian" | "dorico" => Some(ScaleKind::Dorian),
        "phrygian" | "frigio" => Some(ScaleKind::Phrygian),
        "lydian" | "lidio" => Some(ScaleKind::Lydian),
        "mixolydian" | "mixolidio" => Some(ScaleKind::Mixolydian),
        "harmonic" => Some(ScaleKind::HarmonicMinor),
        _ => None,
    }
}

const SOLFEGE: [(&str, u8); 7] = [
    ("sol", 7),
    ("do", 0),
    ("re", 2),
    ("mi", 4),
    ("fa", 5),
    ("la", 9),
    ("si", 11),
];

/// Spanish/Italian note names (`La`, `Fa#`, `Sib`). Only used when a mode word
/// follows or is attached, since `la`/`do`/`mi` are common words.
fn solfege_prefix(s: &str) -> Option<(u8, &str)> {
    let lower = s.to_ascii_lowercase();
    for (name, pc) in SOLFEGE {
        if lower.starts_with(name) {
            let rest = &s[name.len()..];
            let (shift, rest) = if let Some(r) = rest.strip_prefix('#') {
                (1, r)
            } else if let Some(r) = rest.strip_prefix('b') {
                if r.is_empty() || mode_suffix(r).is_some() {
                    (-1, r)
                } else {
                    (0, rest)
                }
            } else {
                (0, rest)
            };
            return Some((((pc as i32 + shift).rem_euclid(12)) as u8, rest));
        }
    }
    None
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum KeyTok {
    /// Note with an explicit mode (`Am`, `C#maj`).
    Full(KeyHint),
    /// Note with an accidental and no mode (`F#`, `Bb`): fairly reliable.
    Accidental(u8),
    /// Bare uppercase letter (`C`): only trusted for tonal material.
    Bare(u8),
    /// Note plus octave (`C4`, `A0v1`, `Db-1`): a multisample root.
    NoteOctave(u8),
    /// Camelot code (`8A`).
    Camelot(KeyHint),
}

fn key_token(tok: &str) -> Option<KeyTok> {
    if let Some(h) = camelot_token(tok) {
        return Some(KeyTok::Camelot(h));
    }
    let (pc, rest) = note_prefix(tok)?;
    let accidental = tok.len() > 1 && rest.len() < tok.len() - 1;
    if rest.is_empty() {
        return Some(if accidental {
            KeyTok::Accidental(pc)
        } else {
            KeyTok::Bare(pc)
        });
    }
    if let Some(kind) = mode_suffix(rest) {
        return Some(KeyTok::Full(KeyHint { root: Key(pc), kind: Some(kind) }));
    }
    // Note + octave (+ optional velocity layer "v3" / "v12").
    let r = rest.strip_prefix('-').map_or((rest, 1i32), |r| (r, -1));
    let (digits, sign) = r;
    let oct_len = digits.chars().take_while(|c| c.is_ascii_digit()).count();
    if oct_len == 1 {
        let after = &digits[oct_len..];
        let vel_ok = after.is_empty()
            || (after.starts_with(['v', 'V']) && after[1..].chars().all(|c| c.is_ascii_digit()) && after.len() > 1);
        if vel_ok {
            let oct: i32 = sign * digits[..1].parse::<i32>().ok()?;
            let midi = (oct + 1) * 12 + pc as i32;
            if (0..=127).contains(&midi) {
                return Some(KeyTok::NoteOctave(midi as u8));
            }
        }
    }
    None
}

fn camelot_token(tok: &str) -> Option<KeyHint> {
    let minor = match tok.chars().last()? {
        'A' => true,
        'B' => false,
        _ => return None,
    };
    let num = &tok[..tok.len() - 1];
    let n: u8 = num.parse().ok()?;
    if !(1..=12).contains(&n) || num.starts_with('0') {
        return None;
    }
    Some(from_camelot(n, minor))
}

/// Camelot number/letter → key. 8A = A minor, 8B = C major.
pub fn from_camelot(n: u8, minor: bool) -> KeyHint {
    // Each step on the wheel is a fifth (7 semitones); inverse of 7 mod 12 is 7.
    let step = ((n as i32 - 1) * 7).rem_euclid(12);
    let (base, kind) = if minor { (8, ScaleKind::Minor) } else { (11, ScaleKind::Major) };
    KeyHint { root: Key(((base + step) % 12) as u8), kind: Some(kind) }
}

// ---------------------------------------------------------------------------
// Per-segment analysis
// ---------------------------------------------------------------------------

#[derive(Default, Debug)]
struct SegmentHints {
    full_key: Option<KeyHint>,
    acc_key: Option<u8>,
    bare_key: Option<u8>,
    camelot: Option<KeyHint>,
    /// Lowercase key as the last token ("_c", "_em9"): trusted for tonal material only.
    lower_key: Option<KeyHint>,
    root_note: Option<u8>,
    strong_bpm: Option<f32>,
    weak_bpm: Option<f32>,
    cat: Option<(Category, Option<&'static str>, u8, usize)>,
    /// A subcategory match for the winning category, even if lower priority.
    kind: Option<SampleKind>,
    loop_from_break: bool,
    top: bool,
    genres: Vec<&'static str>,
}

fn parse_bpm_number(s: &str) -> Option<f32> {
    if s.is_empty() || !s.chars().all(|c| c.is_ascii_digit() || c == '.') {
        return None;
    }
    let v: f32 = s.parse().ok()?;
    (40.0..=300.0).contains(&v).then_some(v)
}

fn analyze_segment(seg: &str) -> SegmentHints {
    let mut h = SegmentHints::default();
    let raws = raw_tokens(seg);

    // Keys, notes and BPM work on raw (case-preserving) tokens.
    for (i, raw) in raws.iter().enumerate() {
        let lower = raw.to_ascii_lowercase();
        // "128bpm", "bpm128", "92.5BPM"
        if let Some(n) = lower.strip_suffix("bpm").and_then(parse_bpm_number) {
            h.strong_bpm.get_or_insert(n);
            continue;
        }
        if let Some(n) = lower.strip_prefix("bpm").and_then(parse_bpm_number) {
            h.strong_bpm.get_or_insert(n);
            continue;
        }
        if lower == "bpm" {
            let prev = i.checked_sub(1).and_then(|j| parse_bpm_number(&raws[j]));
            let next = raws.get(i + 1).and_then(|t| parse_bpm_number(t));
            if let Some(n) = prev.or(next) {
                h.strong_bpm.get_or_insert(n);
            }
            continue;
        }
        if lower.len() >= 2 && lower.len() <= 5 && lower.chars().all(|c| c.is_ascii_digit() || c == '.') {
            if let Some(n) = parse_bpm_number(&lower)
                && (60.0..=200.0).contains(&n)
                && !lower.starts_with('0')
            {
                h.weak_bpm.get_or_insert(n);
            }
            continue;
        }

        // Two-token keys: "A min", "C# major", "La menor".
        let next_mode = raws.get(i + 1).and_then(|t| mode_word(t));
        if let Some(kind) = next_mode {
            let note = note_prefix(raw).filter(|(_, r)| r.is_empty());
            if let Some((pc, _)) = note.or_else(|| solfege_prefix(raw).filter(|(_, r)| r.is_empty())) {
                h.full_key.get_or_insert(KeyHint { root: Key(pc), kind: Some(kind) });
                continue;
            }
        }
        // Attached solfège with mode: "Lam" is too ambiguous, but "Fa#m",
        // "Sibm", "Do#menor" are not.
        if let Some((pc, rest)) = solfege_prefix(raw)
            && rest.len() < raw.len() - 2
            && raw.len() > 2
        {
            let had_acc = raw.contains('#') || raw[..raw.len() - rest.len()].ends_with('b');
            if had_acc && let Some(kind) = mode_suffix(rest) {
                h.full_key.get_or_insert(KeyHint { root: Key(pc), kind: Some(kind) });
                continue;
            }
        }
        let mut tok = key_token(raw);
        if tok.is_none() && i + 1 == raws.len() && i > 0 && raw.starts_with(|c: char| c.is_ascii_lowercase()) {
            // Multisample and Sonic Pi style: "bass_16_1_ab2", "bass_hard_c", "guit_em9".
            let mut cap: String = raw[..1].to_ascii_uppercase();
            cap.push_str(&raw[1..]);
            tok = match key_token(&cap) {
                Some(KeyTok::Full(k)) => {
                    h.lower_key.get_or_insert(k);
                    None
                }
                Some(KeyTok::Bare(pc) | KeyTok::Accidental(pc)) => {
                    h.lower_key.get_or_insert(KeyHint { root: Key(pc), kind: None });
                    None
                }
                Some(t @ KeyTok::NoteOctave(_)) => Some(t),
                _ => None,
            };
        }
        match tok {
            Some(KeyTok::Full(k)) => {
                h.full_key.get_or_insert(k);
            }
            Some(KeyTok::Accidental(pc)) => {
                h.acc_key.get_or_insert(pc);
            }
            Some(KeyTok::Bare(pc)) => {
                // A bare letter followed by a mode word was handled above.
                // A lone letter as the very first token is usually a label.
                if i > 0 || raws.len() == 1 {
                    h.bare_key.get_or_insert(pc);
                }
            }
            Some(KeyTok::NoteOctave(m)) => {
                h.root_note.get_or_insert(m);
            }
            Some(KeyTok::Camelot(k)) => {
                h.camelot.get_or_insert(k);
            }
            None => {}
        }
    }

    // Keywords work on folded lowercase words, with 2- and 3-word phrases.
    let mut words: Vec<String> = Vec::new();
    for raw in &raws {
        words.extend(words_of(raw));
    }
    let t = table();
    let apply = |phrase: &str, pos: usize, h: &mut SegmentHints| {
        if let Some(hits) = t.get(phrase) {
            for hit in hits {
                match *hit {
                    Hit::Cat(c, s, p) | Hit::CatLoop(c, s, p) => {
                        if let Hit::CatLoop(..) = hit {
                            h.loop_from_break = true;
                        }
                        let better = match h.cat {
                            None => true,
                            Some((oc, os, op, _)) => {
                                p > op || (p == op && oc == c && os.is_none() && s.is_some())
                            }
                        };
                        if better {
                            h.cat = Some((c, s, p, pos));
                        }
                    }
                    Hit::Kind(k) => {
                        // Loop beats one-shot; chop beats loop ("chopped loop").
                        h.kind = Some(match (h.kind, k) {
                            (Some(SampleKind::Chop), _) | (_, SampleKind::Chop) => SampleKind::Chop,
                            (Some(SampleKind::Loop), _) | (_, SampleKind::Loop) => SampleKind::Loop,
                            _ => k,
                        });
                    }
                    Hit::Top => h.top = true,
                    Hit::Genre(g) => {
                        if !h.genres.contains(&g) {
                            h.genres.push(g);
                        }
                    }
                }
            }
        }
    };
    let mut consumed_until = 0usize;
    for i in 0..words.len() {
        // Longest phrase first so "drum and bass" is not also "bass".
        let p3 = (i + 2 < words.len()).then(|| format!("{} {} {}", words[i], words[i + 1], words[i + 2]));
        let p2 = (i + 1 < words.len()).then(|| format!("{} {}", words[i], words[i + 1]));
        let c2 = (i + 1 < words.len()).then(|| format!("{}{}", words[i], words[i + 1]));
        if i < consumed_until {
            continue;
        }
        if let Some(p) = p3.as_deref()
            && t.contains_key(p)
        {
            apply(p, i, &mut h);
            consumed_until = i + 3;
            continue;
        }
        if let Some(p) = p2.as_deref().filter(|p| t.contains_key(*p)).or(c2.as_deref().filter(|p| t.contains_key(*p))) {
            apply(p, i, &mut h);
            consumed_until = i + 2;
            continue;
        }
        apply(&words[i], i, &mut h);
    }
    // Whole raw tokens too ("HiHat" → "hihat").
    for raw in &raws {
        let folded: String = raw.chars().filter(|c| c.is_alphanumeric()).map(fold_char).collect();
        if words_of(raw).len() > 1 {
            apply(&folded, usize::MAX, &mut h);
        }
    }
    h
}

/// Parses a path. `root` (if given) stops the folder walk, so the library
/// root's own name ("Samples", "Downloads") is not used as a hint.
pub fn parse_path(path: &Path, root: Option<&Path>) -> NameHints {
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or_default();
    let rel = root.and_then(|r| path.strip_prefix(r).ok()).unwrap_or(path);
    let mut folders: Vec<&str> = rel
        .parent()
        .map(|p| p.iter().filter_map(|c| c.to_str()).collect())
        .unwrap_or_default();
    // Nearest folder first; ignore drive letters and separators.
    folders.retain(|f| !f.ends_with(':') && *f != "\\" && *f != "/");
    folders.reverse();
    parse_name(stem, &folders)
}

/// Parses a file stem and its folders (nearest first).
pub fn parse_name(stem: &str, folders_nearest_first: &[&str]) -> NameHints {
    let file = analyze_segment(stem);
    let dirs: Vec<SegmentHints> = folders_nearest_first.iter().map(|f| analyze_segment(f)).collect();

    let mut out = NameHints::default();

    // Category: the file name wins; folders fill in what is missing.
    let mut cat = file.cat.map(|(c, s, ..)| (c, s));
    // A weak word in the file ("Glass Break", "Beat 3") yields to a specific folder ("Foley").
    if let Some((fc, _, fp, _)) = file.cat
        && fp <= 3
        && let Some((dc, ds, ..)) = dirs.iter().find_map(|d| d.cat).filter(|(dc, _, dp, _)| *dp >= 4 && *dc != fc)
    {
        cat = Some((dc, ds));
    }
    for d in &dirs {
        match (cat, d.cat) {
            (None, Some((c, s, ..))) => cat = Some((c, s)),
            (Some((c, None)), Some((dc, Some(ds), ..))) if dc == c => cat = Some((c, Some(ds))),
            _ => {}
        }
    }
    if let Some((c, s)) = cat {
        out.category = Some(c);
        out.subcategory = s;
    }

    // Kind: explicit words anywhere in the path; breakbeats are loops.
    let mut kind = file.kind;
    for d in &dirs {
        if kind.is_none() {
            kind = d.kind;
        }
    }
    if kind.is_none()
        && matches!(cat, Some((Category::Drums, _)) | None)
        && (file.loop_from_break || dirs.iter().any(|d| d.loop_from_break))
    {
        kind = Some(SampleKind::Loop);
    }
    out.kind = kind;
    out.top_loop = file.top || dirs.iter().take(2).any(|d| d.top);

    let tonal_cat = out.category.is_some_and(|c| c.is_tonal()) || out.subcategory == Some("808s");
    let tonal = tonal_cat || kind == Some(SampleKind::Loop);
    let gated = |seg: &SegmentHints| -> Option<KeyHint> {
        seg.full_key
            .or(seg.acc_key.map(|pc| KeyHint { root: Key(pc), kind: None }))
            .or_else(|| {
                (tonal || seg.strong_bpm.is_some()).then_some(seg.camelot).flatten()
            })
            .or_else(|| {
                tonal_cat
                    .then_some(seg.bare_key.map(|pc| KeyHint { root: Key(pc), kind: None }).or(seg.lower_key))
                    .flatten()
            })
    };
    out.key = gated(&file).or_else(|| dirs.iter().find_map(|d| d.full_key.or(d.camelot)));
    // A multisample note in a drum file name is just a pad label; keep it for tonal stuff.
    out.root_note = file.root_note;

    // BPM: explicit "bpm" anywhere; a bare number only for loops or next to a key.
    out.bpm = file.strong_bpm.or_else(|| {
        let loopish = kind == Some(SampleKind::Loop);
        let key_here = file.full_key.is_some() || file.acc_key.is_some() || file.camelot.is_some();
        (loopish || key_here).then_some(file.weak_bpm).flatten()
    });
    if out.bpm.is_none() {
        out.bpm = dirs.iter().find_map(|d| d.strong_bpm);
    }

    for g in file.genres.iter().chain(dirs.iter().flat_map(|d| d.genres.iter())) {
        if !out.genres.contains(g) {
            out.genres.push(g);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(name: &str, kind: Option<ScaleKind>) -> Option<KeyHint> {
        Some(KeyHint { root: Key::from_name(name).unwrap(), kind })
    }
    const MIN: Option<ScaleKind> = Some(ScaleKind::Minor);
    const MAJ: Option<ScaleKind> = Some(ScaleKind::Major);

    fn p(path: &str) -> NameHints {
        parse_path(Path::new(path), None)
    }

    #[test]
    fn keys_table() {
        let cases: &[(&str, Option<KeyHint>)] = &[
            ("Bass_Am_128.wav", k("A", MIN)),
            ("Cymatics - Lofi Loop 1 - 90 BPM F#m.wav", k("F#", MIN)),
            ("Pad_C#maj_120.wav", k("C#", MAJ)),
            ("Synth_A min_128.wav", k("A", MIN)),
            ("Lead - A minor - 140.wav", k("A", MIN)),
            ("Pluck_Gmin.wav", k("G", MIN)),
            ("Keys Eb Major.wav", k("Eb", MAJ)),
            ("Chord_Dbm7.wav", k("Db", MIN)),
            ("Bbm_Arp_150bpm.wav", k("Bb", MIN)),
            ("Loop 128 8A.wav", k("A", MIN)),
            ("Melodic Loop 140 11B.wav", k("A", MAJ)),
            ("Guitarra La menor 90.wav", k("A", MIN)),
            ("Piano Do mayor.wav", k("C", MAJ)),
            ("Bajo Fa#m 95.wav", k("F#", MIN)),
            ("808 F#.wav", k("F#", None)),
            ("Bass C.wav", k("C", None)),
            ("vocal_chop_Cmaj_100bpm.wav", k("C", MAJ)),
            ("Abmaj9 stab.wav", k("Ab", MAJ)),
            ("Bm_Pad.wav", k("B", MIN)),
            ("D Dorian Lead.wav", k("D", Some(ScaleKind::Dorian))),
            ("Csharp minor pad.wav", k("C#", MIN)),
            ("Sibm Pad Reggaeton.wav", k("A#", MIN)),
            ("Melody_E_minor_93.wav", k("E", MIN)),
            ("bass_hard_c.flac", k("C", None)),
            ("guit_em9.flac", k("E", MIN)),
            ("Ab3 pad.wav", None),
            // No key in these.
            ("Kick_01.wav", None),
            ("Snare B.wav", None),
            ("FM Bass 01.wav", None),
            ("BD_Hard.wav", None),
            ("Bass Drum 3.wav", None),
            ("A Kick.wav", None),
            ("am_radio_noise.wav", None),
            ("Lam Pluck.wav", None),
            ("Hat 2A.wav", None),
            ("C4v8.wav", None),
        ];
        for (name, want) in cases {
            assert_eq!(p(name).key, *want, "{name}");
        }
    }

    #[test]
    fn bpm_table() {
        let cases: &[(&str, Option<f32>)] = &[
            ("Kick_Am_128bpm.wav", Some(128.0)),
            ("Loop 128 BPM.wav", Some(128.0)),
            ("drum_loop_140.wav", Some(140.0)),
            ("Bass_Am_128.wav", Some(128.0)),
            ("Trap Hat Loop_150_BPM.wav", Some(150.0)),
            ("Lofi Loop BPM90.wav", Some(90.0)),
            ("Vocal 92.5bpm Gm.wav", Some(92.5)),
            ("Reggaeton Dembow Loop 95.wav", Some(95.0)),
            ("Kick 140.wav", None),
            ("Snare_120.wav", None),
            ("Kick_01.wav", None),
            ("Hat 007.wav", None),
            ("Riser 8 bars.wav", None),
        ];
        for (name, want) in cases {
            assert_eq!(p(name).bpm, *want, "{name}");
        }
        assert_eq!(p("Loops/124 BPM/Groove A.wav").bpm, Some(124.0));
    }

    #[test]
    fn category_table() {
        let cases: &[(&str, Option<Category>, Option<&str>)] = &[
            ("Kick_01.wav", Some(Drums), Some("Kicks")),
            ("BD_Hard.wav", Some(Drums), Some("Kicks")),
            ("Bass Drum 3.wav", Some(Drums), Some("Kicks")),
            ("TR808_Kick_Long.wav", Some(Drums), Some("Kicks")),
            ("808 Snare.wav", Some(Drums), Some("Snares")),
            ("808 F#.wav", Some(Drums), Some("808s")),
            ("Sub 808 Bass C.wav", Some(Drums), Some("808s")),
            ("Closed HiHat 3.wav", Some(Drums), Some("Hats")),
            ("hi-hat_open.wav", Some(Drums), Some("Hats")),
            ("OHH 02.wav", Some(Drums), Some("Hats")),
            ("Clap_Reverb.wav", Some(Drums), Some("Claps")),
            ("Palmas Reggaeton 01.wav", Some(Drums), Some("Claps")),
            ("Bombo Dembow.wav", Some(Drums), Some("Kicks")),
            ("Caja Perreo 3.wav", Some(Drums), Some("Snares")),
            ("Guira Loop 100.wav", Some(Drums), Some("Percs")),
            ("Conga_Hi.wav", Some(Drums), Some("Percs")),
            ("Crash 1.wav", Some(Drums), Some("Cymbals")),
            ("Ride_Bell.wav", Some(Drums), Some("Cymbals")),
            ("Snare Roll Riser.wav", Some(Fx), Some("Risers")),
            ("White Noise Sweep Up.wav", Some(Fx), Some("Sweeps")),
            ("Downlifter_01.wav", Some(Fx), Some("Sweeps")),
            ("Impact Boom.wav", Some(Fx), Some("Impacts")),
            ("Sub Drop 2.wav", Some(Fx), Some("Impacts")),
            ("Atmos_Dark.wav", Some(Fx), Some("Atmos")),
            ("Textura Ambiente.wav", Some(Fx), Some("Atmos")),
            ("Laser Zap.wav", Some(Fx), None),
            ("Reese Bass Am.wav", Some(Bass), None),
            ("Bajo Fa#m 95.wav", Some(Bass), None),
            ("Lead_Saw.wav", Some(Synths), Some("Leads")),
            ("Pad_Warm.wav", Some(Synths), Some("Pads")),
            ("Pluck 03 Cm.wav", Some(Synths), Some("Plucks")),
            ("Rhodes Chords.wav", Some(Synths), Some("Keys")),
            ("Synth Chords.wav", Some(Synths), Some("Chords")),
            ("Arp_Fmin_128.wav", Some(Synths), Some("Arps")),
            ("Vocal Chop Cmaj.wav", Some(Vocals), None),
            ("Vox_Adlib_Yeah.wav", Some(Vocals), None),
            ("Voz Femenina.wav", Some(Vocals), None),
            ("Grand Piano C4.wav", Some(Instruments), Some("Piano")),
            ("Violin_Sus_A4.wav", Some(Instruments), Some("Strings")),
            ("Double Bass pizz.wav", Some(Instruments), Some("Strings")),
            ("Trumpet stac.wav", Some(Instruments), Some("Brass")),
            ("Flauta Do mayor.wav", Some(Instruments), Some("Woodwinds")),
            ("Marimba_C3.wav", Some(Instruments), Some("Mallets")),
            ("Guitarra La menor 90.wav", Some(Instruments), Some("Guitar")),
            ("Choir Aah.wav", Some(Instruments), Some("Choir")),
            ("Amen Break 170.wav", Some(Drums), None),
            ("Untitled 12.wav", None, None),
        ];
        for (name, cat, sub) in cases {
            let h = p(name);
            assert_eq!((h.category, h.subcategory), (*cat, *sub), "{name}");
        }
    }

    #[test]
    fn folders_fill_in() {
        let h = p("Packs/Trap Kit/Drums/Kicks/Hard 01.wav");
        assert_eq!((h.category, h.subcategory), (Some(Drums), Some("Kicks")));
        assert_eq!(h.genres, vec!["Trap"]);

        let h = p("Packs/House/Loops/Drum Loops/Groove 124.wav");
        assert_eq!(h.kind, Some(SampleKind::Loop));
        assert_eq!(h.category, Some(Drums));
        assert_eq!(h.bpm, Some(124.0));
        assert_eq!(h.genres, vec!["House"]);

        let h = p("Reggaeton Pack/Top Loops/Hats 95.wav");
        assert!(h.top_loop);
        assert_eq!(h.kind, Some(SampleKind::Loop));

        let h = p("Bass/A minor/One Shots/Reese 1.wav");
        assert_eq!(h.key, k("A", MIN));
        assert_eq!(h.kind, Some(SampleKind::OneShot));
        assert_eq!(h.category, Some(Bass));

        // A pack name that says "Bass" does not override the file's "Kick".
        let h = p("Bass House Essentials/Kick 3.wav");
        assert_eq!(h.subcategory, Some("Kicks"));

        // Drum category from file + subcategory from folder.
        let h = p("Snares/Drum 5.wav");
        assert_eq!(h.subcategory, Some("Snares"));
    }

    #[test]
    fn kinds_and_notes() {
        assert_eq!(p("Vocal Chop 3.wav").kind, Some(SampleKind::Chop));
        assert_eq!(p("Drum Loop 01.wav").kind, Some(SampleKind::Loop));
        assert_eq!(p("Kick One Shot.wav").kind, Some(SampleKind::OneShot));
        assert_eq!(p("Kick_Oneshot.wav").kind, Some(SampleKind::OneShot));
        assert_eq!(p("Kick.wav").kind, None);
        assert_eq!(p("Salamander/A0v1.wav").root_note, Some(21));
        assert_eq!(p("C4v16.wav").root_note, Some(60));
        assert_eq!(p("Violin_Sus_C#3.wav").root_note, Some(49));
        assert_eq!(p("Kick.wav").root_note, None);
        assert_eq!(p("Samples/all_all_all_ab3.wav").root_note, Some(56));
        assert_eq!(p("Samples/bass_16_1_db2.wav").root_note, Some(37));
        assert_eq!(p("Samples/blackheart/A1.wav").root_note, Some(33));
        assert_eq!(p("Hardstyle Kick 1 - G#1.flac").root_note, Some(32));
        // Lowercase notes elsewhere than the end are not notes.
        assert_eq!(p("a3 kick.wav").root_note, None);
    }

    #[test]
    fn genres_table() {
        let cases: &[(&str, &[&str])] = &[
            ("Deep House Chord Stab.wav", &["House"]),
            ("Drum and Bass Break 174.wav", &["Drum & Bass"]),
            ("Lo-Fi Hip Hop Kick.wav", &["Lo-Fi", "Hip-Hop"]),
            ("Reggaetón Perreo Loop.wav", &["Reggaeton"]),
            ("Dembow_Loop_100.wav", &["Dembow"]),
            ("Future Bass Chords.wav", &["Future Bass"]),
            ("Techno_Rumble_Kick.wav", &["Techno"]),
            ("Kick.wav", &[]),
        ];
        for (name, want) in cases {
            assert_eq!(p(name).genres, *want, "{name}");
        }
        let glass = p("Foley/Glass Break.flac");
        assert_eq!((glass.category, glass.kind), (Some(Fx), None));
        // "drum and bass" is a genre, not Bass.
        assert_eq!(p("Drum and Bass Break 174.wav").category, Some(Drums));
    }

    #[test]
    fn camelot_inverse() {
        for pc in 0..12u8 {
            for kind in [ScaleKind::Minor, ScaleKind::Major] {
                let s = Scale::new(Key(pc), kind);
                let code = s.camelot().unwrap();
                let (n, l) = code.split_at(code.len() - 1);
                let h = from_camelot(n.parse().unwrap(), l == "A");
                assert_eq!(h.scale(), Some(s), "{code}");
            }
        }
    }
}
