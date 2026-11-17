//! Text matching shared by every search box: lower case, no accents, words,
//! and one way of saying how well a name answers what was typed.
//!
//! The library's database folds accents with FTS5 itself; this is the same
//! rule for the places that match in memory (the sample rows after the
//! database answered, the command and device lists of the palette, the free
//! plugin catalog), so all of them agree on what "electrico" finds.

/// `s` in lower case without accents: "Eléctrico" → "electrico".
pub fn fold(s: &str) -> String {
    if s.is_ascii() {
        return s.to_ascii_lowercase();
    }
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        fold_char(c, &mut out);
    }
    out
}

fn fold_char(c: char, out: &mut String) {
    let base = match c {
        'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' | 'ā' | 'ă' | 'ą' | 'À' | 'Á' | 'Â' | 'Ã' | 'Ä' | 'Å' | 'Ā' | 'Ă' | 'Ą' => 'a',
        'ç' | 'ć' | 'ĉ' | 'č' | 'Ç' | 'Ć' | 'Ĉ' | 'Č' => 'c',
        'ď' | 'đ' | 'Ď' | 'Đ' => 'd',
        'è' | 'é' | 'ê' | 'ë' | 'ē' | 'ĕ' | 'ė' | 'ę' | 'ě' | 'È' | 'É' | 'Ê' | 'Ë' | 'Ē' | 'Ĕ' | 'Ė' | 'Ę' | 'Ě' => 'e',
        'ĝ' | 'ğ' | 'ġ' | 'ģ' | 'Ĝ' | 'Ğ' | 'Ġ' | 'Ģ' => 'g',
        'ì' | 'í' | 'î' | 'ï' | 'ĩ' | 'ī' | 'ĭ' | 'į' | 'ı' | 'Ì' | 'Í' | 'Î' | 'Ï' | 'Ĩ' | 'Ī' | 'Ĭ' | 'Į' | 'İ' => 'i',
        'ł' | 'ľ' | 'ĺ' | 'ļ' | 'Ł' | 'Ľ' | 'Ĺ' | 'Ļ' => 'l',
        'ñ' | 'ń' | 'ň' | 'ņ' | 'Ñ' | 'Ń' | 'Ň' | 'Ņ' => 'n',
        'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ø' | 'ō' | 'ŏ' | 'ő' | 'Ò' | 'Ó' | 'Ô' | 'Õ' | 'Ö' | 'Ø' | 'Ō' | 'Ŏ' | 'Ő' => 'o',
        'ŕ' | 'ř' | 'Ŕ' | 'Ř' => 'r',
        'ś' | 'ŝ' | 'ş' | 'š' | 'Ś' | 'Ŝ' | 'Ş' | 'Š' => 's',
        'ţ' | 'ť' | 'Ţ' | 'Ť' => 't',
        'ù' | 'ú' | 'û' | 'ü' | 'ũ' | 'ū' | 'ŭ' | 'ů' | 'ű' | 'ų' | 'Ù' | 'Ú' | 'Û' | 'Ü' | 'Ũ' | 'Ū' | 'Ŭ' | 'Ů' | 'Ű' | 'Ų' => 'u',
        'ý' | 'ÿ' | 'ŷ' | 'Ý' | 'Ÿ' | 'Ŷ' => 'y',
        'ź' | 'ż' | 'ž' | 'Ź' | 'Ż' | 'Ž' => 'z',
        'ß' => {
            out.push_str("ss");
            return;
        }
        'æ' | 'Æ' => {
            out.push_str("ae");
            return;
        }
        'œ' | 'Œ' => {
            out.push_str("oe");
            return;
        }
        // A combining accent left over from a decomposed letter.
        '\u{300}'..='\u{36f}' => return,
        c => {
            out.extend(c.to_lowercase());
            return;
        }
    };
    out.push(base);
}

/// The words of what was typed, folded: letters and digits (and `#`, as in
/// "F#m") separate them.
pub fn words(text: &str) -> Vec<String> {
    fold(text).split(|c: char| !c.is_alphanumeric() && c != '#').filter(|w| !w.is_empty()).map(str::to_owned).collect()
}

/// How well one folded `word` answers a folded `name`, 0 = not at all:
/// the whole name, its start, a whole word of it, the start of a word, any part.
fn word_score(word: &str, name: &str) -> f32 {
    if name == word {
        return 6.;
    }
    if name.starts_with(word) {
        return 4.;
    }
    let mut parts = name.split(|c: char| !c.is_alphanumeric() && c != '#').filter(|p| !p.is_empty());
    let mut prefix = false;
    for p in &mut parts {
        if p == word {
            return 3.5;
        }
        prefix |= p.starts_with(word);
    }
    if prefix {
        3.
    } else if name.contains(word) {
        2.
    } else {
        0.
    }
}

/// Every typed word must be in the `name` (a lower score for a part of a
/// word) or in the `extra` texts (tags, folders, pack: worth the least).
/// `None` when a word is nowhere. No words: everything matches, equally.
pub fn score(words: &[String], name: &str, extra: &[&str]) -> Option<f32> {
    let name = fold(name);
    let mut total = 0.;
    let mut extra_folded: Option<Vec<String>> = None;
    for w in words {
        let s = word_score(w, &name);
        if s > 0. {
            total += s;
            continue;
        }
        let ex = extra_folded.get_or_insert_with(|| extra.iter().map(|e| fold(e)).collect());
        if ex.iter().any(|e| e.contains(w.as_str())) {
            total += 1.;
        } else {
            return None;
        }
    }
    Some(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folding_drops_case_and_accents() {
        assert_eq!(fold("Eléctrico ÑANDÚ Straße Œuvre"), "electrico nandu strasse oeuvre");
        assert_eq!(fold("Cafe\u{301}"), "cafe", "a decomposed accent goes too");
        assert_eq!(fold("808 Kick"), "808 kick");
    }

    #[test]
    fn words_split_on_anything_but_letters_digits_and_sharp() {
        assert_eq!(words("  Kick_01, F#m-Loop "), vec!["kick", "01", "f#m", "loop"]);
        assert!(words(" ._- ").is_empty());
    }

    #[test]
    fn a_name_that_starts_with_it_beats_a_word_beats_a_part() {
        let w = words("kick");
        let s = |n: &str| score(&w, n, &[]).unwrap_or(0.);
        assert!(s("Kick") > s("Kick Hard"));
        assert!(s("Kick Hard") > s("Hard Kick"));
        assert!(s("Hard Kick") > s("Hard Kicks"));
        assert!(s("Hard Kicks") > s("HardKick"));
        assert_eq!(score(&w, "Snare", &["kick pack"]), Some(1.), "extras count least");
        assert_eq!(score(&w, "Snare", &["clap pack"]), None);
    }

    #[test]
    fn every_word_has_to_match() {
        let w = words("dark kic");
        assert_eq!(score(&w, "Dark Kick", &[]), Some(4. + 3.));
        assert_eq!(score(&w, "Dark Snare", &[]), None);
        assert_eq!(score(&[], "Anything", &[]), Some(0.));
    }

    #[test]
    fn accents_match_both_ways() {
        assert!(score(&words("electrico"), "Bajo Eléctrico", &[]).is_some());
        assert!(score(&words("ELÉCTRICO"), "bajo electrico", &[]).is_some());
    }
}
