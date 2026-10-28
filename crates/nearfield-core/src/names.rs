//! Author-name keys for the offline index.
//!
//! The index is a hash table, so a lookup only works if arXiv's spelling and
//! OpenAlex's spelling collapse to the *same* string. The two sources agree on
//! the person and disagree on the typography: case ("NAGI I. YAGUBOV"),
//! dots and spaces around initials ("E.M.F. de Oliveira" / "E M F de
//! Oliveira"), hyphens in romanised given names ("Jeng-Yue" / "Jengyue"),
//! diacritics one side dropped ("Pérez" / "Perez"), letters that have no
//! decomposition at all ("Łukasz", "Søren"), and the German habit of writing
//! an umlaut as a digraph ("Thuerey" / "Thürey"). Each rule here exists
//! because one of those pairs was seen in real data; none is a guess at what
//! might go wrong.
//!
//! The price of folding is collisions: "Michal" and "Michał" become the same
//! key. For a *prior* that is acceptable — the paper's own HTML corrects it —
//! and the index resolves a collision to the more prolific author, which is
//! also what the paid search would have returned.

use unicode_normalization::char::is_combining_mark;
use unicode_normalization::UnicodeNormalization;

/// The canonical key: lowercase letters and digits, nothing else.
///
/// Spaces go too. That makes "Jeng-Yue Liu", "JengYue Liu" and "Jeng Yue
/// Liu" one key, which matters for romanised Chinese and Korean names where
/// every convention is in use somewhere.
pub fn key(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for c in name.nfkd() {
        if is_combining_mark(c) {
            continue;
        }
        match c {
            // Letters NFKD leaves alone, mapped to what an ASCII keyboard
            // would have typed.
            'ß' => out.push_str("ss"),
            'ø' | 'Ø' => out.push('o'),
            'ł' | 'Ł' => out.push('l'),
            'đ' | 'Đ' => out.push('d'),
            'ð' | 'Ð' => out.push('d'),
            'þ' | 'Þ' => out.push_str("th"),
            'æ' | 'Æ' => out.push_str("ae"),
            'œ' | 'Œ' => out.push_str("oe"),
            'ı' => out.push('i'),
            'ħ' | 'Ħ' => out.push('h'),
            'ŧ' | 'Ŧ' => out.push('t'),
            c if c.is_alphanumeric() => out.extend(c.to_lowercase()),
            _ => {}
        }
    }
    out
}

/// "Müller" written the way a German types it without an umlaut key:
/// "Mueller". `None` when the name has no umlaut, so callers can skip the
/// second lookup for the overwhelming majority of names.
pub fn umlaut_to_digraph(name: &str) -> Option<String> {
    let nfc: String = name.nfc().collect();
    if !nfc.chars().any(|c| matches!(c, 'ä' | 'ö' | 'ü' | 'Ä' | 'Ö' | 'Ü')) {
        return None;
    }
    let mut out = String::with_capacity(nfc.len() + 4);
    for c in nfc.chars() {
        match c {
            'ä' => out.push_str("ae"),
            'ö' => out.push_str("oe"),
            'ü' => out.push_str("ue"),
            'Ä' => out.push_str("Ae"),
            'Ö' => out.push_str("Oe"),
            'Ü' => out.push_str("Ue"),
            c => out.push(c),
        }
    }
    Some(out)
}

/// The reverse, on an already-folded key: "mueller" → "muller", for the case
/// where arXiv has the digraph and OpenAlex has neither the umlaut nor the e.
///
/// Only a last resort, because it is lossy: "que" and "gue" are left alone
/// since Enrique and Miguel outnumber Guenther, and even so "Michael"
/// becomes "michal". `None` when nothing changed.
pub fn collapse_digraphs(key: &str) -> Option<String> {
    let b = key.as_bytes();
    let mut out = String::with_capacity(key.len());
    let mut i = 0;
    let mut changed = false;
    while i < b.len() {
        let pair = &b[i..(i + 2).min(b.len())];
        let prev = if i > 0 { b[i - 1] } else { 0 };
        let collapse = match pair {
            b"ue" => prev != b'q' && prev != b'g',
            b"oe" | b"ae" => true,
            _ => false,
        };
        if collapse {
            out.push(pair[0] as char);
            i += 2;
            changed = true;
        } else {
            // Keys are ASCII except for non-Latin scripts, where no digraph
            // rule applies, so byte-wise stepping is safe for the rule and
            // the char is copied whole.
            let c = key[i..].chars().next().unwrap();
            out.push(c);
            i += c.len_utf8();
        }
    }
    changed.then_some(out)
}

/// Every key an OpenAlex name is filed under.
///
/// A name with an umlaut gets its digraph spelling too, so that the common
/// case — the author types ASCII on arXiv, OpenAlex keeps the umlaut — is an
/// exact hit rather than a fallback.
pub fn index_keys(name: &str) -> Vec<String> {
    let mut keys = Vec::with_capacity(2);
    push_unique(&mut keys, key(name));
    if let Some(t) = umlaut_to_digraph(name) {
        push_unique(&mut keys, key(&t));
    }
    keys
}

/// Every key worth trying for a name as arXiv prints it, exact first.
pub fn lookup_keys(name: &str) -> Vec<String> {
    let mut keys = index_keys(name);
    if let Some(c) = keys.first().and_then(|k| collapse_digraphs(k)) {
        push_unique(&mut keys, c);
    }
    keys
}

fn push_unique(keys: &mut Vec<String>, k: String) {
    if !k.is_empty() && !keys.contains(&k) {
        keys.push(k);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn case_spacing_and_punctuation_fold_away() {
        assert_eq!(key("  Yuanzhe   PENG "), "yuanzhepeng");
        assert_eq!(key("E.M.F. de Oliveira"), key("E M F de Oliveira"));
        assert_eq!(key("A. I. Lebedev"), key("A I Lebedev"));
        assert_eq!(key("O'Brien"), "obrien");
    }

    #[test]
    fn romanised_names_agree_across_hyphen_conventions() {
        assert_eq!(key("Jeng-Yue Liu"), key("JengYue Liu"));
        assert_eq!(key("Jeng-Yue Liu"), key("Jeng Yue Liu"));
        assert_eq!(key("Rodas-Pernillo"), "rodaspernillo");
    }

    #[test]
    fn diacritics_fold_to_ascii() {
        assert_eq!(key("Amílcar Josué Pérez"), key("Amilcar Josue Perez"));
        assert_eq!(key("Tommaso Treù"), "tommasotreu");
        assert_eq!(key("Łukasz Søren Đorđe"), "lukaszsorendorde");
        assert_eq!(key("Straße"), "strasse");
        assert_eq!(key("Ærø"), "aero");
    }

    #[test]
    fn non_latin_scripts_survive() {
        // Nothing to fold, but nothing to lose either.
        assert_eq!(key("Иван Петров"), "иванпетров");
        assert_eq!(key("李 伟"), "李伟");
    }

    #[test]
    fn thuerey_and_thurey_meet_in_the_index() {
        // The arXiv spelling is the ASCII digraph; OpenAlex may hold either.
        let indexed = index_keys("Nils Thürey");
        assert!(indexed.contains(&key("Nils Thuerey")), "{indexed:?}");
        assert!(indexed.contains(&key("Nils Thurey")), "{indexed:?}");
        assert_eq!(index_keys("Nils Thuerey"), vec!["nilsthuerey".to_string()]);
    }

    #[test]
    fn umlaut_query_finds_a_digraph_record() {
        // OpenAlex stores "Mueller", arXiv prints "Müller".
        assert!(lookup_keys("Thomas Müller").contains(&key("Thomas Mueller")));
    }

    #[test]
    fn digraph_query_falls_back_to_the_bare_vowel() {
        // OpenAlex stores "Muller", arXiv prints "Mueller": exact misses, the
        // last-resort key hits.
        let keys = lookup_keys("Thomas Mueller");
        assert_eq!(keys[0], "thomasmueller");
        assert!(keys.contains(&"thomasmuller".to_string()), "{keys:?}");
    }

    #[test]
    fn collapse_leaves_romance_names_alone() {
        assert_eq!(collapse_digraphs("enriquemiguel"), None);
        assert_eq!(collapse_digraphs("joergguenther").as_deref(), Some("jorgguenther"));
    }

    #[test]
    fn a_plain_name_costs_one_lookup() {
        assert_eq!(lookup_keys("Bryan Catanzaro").len(), 1);
        assert_eq!(index_keys("Kai Zhang").len(), 1);
    }

    #[test]
    fn an_empty_or_symbolic_name_yields_no_keys() {
        assert!(index_keys(" - ").is_empty());
        assert!(lookup_keys("").is_empty());
    }
}
