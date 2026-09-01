//! One representation for matching: letters only, no separators, homoglyphs,
//! leet and transliteration folded in, searched by substring. Morphology and
//! multi-word terms come free; word-boundary noise is the price.

use unicode_normalization::UnicodeNormalization;

#[derive(Debug, Clone, Default)]
pub struct Normalized {
    /// Letters only, no separators.
    pub text: String,
    /// For every char of `text`, the index of the source char.
    pub map: Vec<usize>,
    /// Source chars a word starts at.
    starts: Vec<bool>,
    /// Length in chars of the word a source char belongs to; 0 for whitespace.
    word_len: Vec<u16>,
}

impl Normalized {
    pub fn build(src: &str) -> Self {
        let mut out = Self::default();

        // Word starts in the source text, so a term cannot match across the
        // seam of two collapsed words («быстро копится» → «окоп»).
        let chars: Vec<char> = src.chars().collect();
        out.starts = (0..chars.len())
            .map(|i| chars[i].is_alphanumeric() && (i == 0 || !chars[i - 1].is_alphanumeric()))
            .collect();

        // Length of the whitespace-separated word each char belongs to: it
        // tells deliberate letter-spacing (all fragments one letter long)
        // from an accidental match glued across a space.
        out.word_len = vec![0; chars.len()];
        let mut i = 0;
        while i < chars.len() {
            if chars[i].is_whitespace() {
                i += 1;
                continue;
            }
            let start = i;
            while i < chars.len() && !chars[i].is_whitespace() {
                i += 1;
            }
            let len = (i - start).min(u16::MAX as usize) as u16;
            for j in start..i {
                out.word_len[j] = len;
            }
        }

        // A run is a chain of letters and digits with no separator.
        // Collapsing runs is what defeats `в.о.й.н.а`.
        let mut run: Vec<(char, usize)> = Vec::new();

        for (i, c) in src.chars().enumerate() {
            for fc in fold_char(c) {
                if fc.is_alphanumeric() {
                    run.push((fc, i));
                } else {
                    out.flush(&mut run);
                }
            }
        }
        out.flush(&mut run);
        out
    }

    fn flush(&mut self, run: &mut Vec<(char, usize)>) {
        if run.is_empty() {
            return;
        }
        // A run with no letter at all is a number («2024»): dropped, or leet
        // would turn it into letters and glue it to its neighbours.
        if !run.iter().any(|(c, _)| c.is_alphabetic()) {
            run.clear();
            return;
        }

        // leet runs per char and before splitting: the `1` of `put1n` must
        // become a letter before we decide where the Latin segment ends.
        let folded: Vec<(char, usize)> = run.iter().map(|&(c, at)| (leet(c), at)).collect();

        // Each Latin segment is transliterated on its own: one Cyrillic
        // letter used to switch transliteration off for the whole word.
        let mut i = 0;
        while i < folded.len() {
            let latin = folded[i].0.is_ascii_alphabetic();
            let start = i;
            while i < folded.len() && folded[i].0.is_ascii_alphabetic() == latin {
                i += 1;
            }
            let seg = &folded[start..i];
            // Latin inside a word is two different things: a look-alike
            // (`пyтин`), which skeleton folds and transliteration would
            // break, and real transliteration (`красikov`). Homoglyphs are a
            // closed set of seven letters; transliteration uses any.
            let homoglyphs_only = seg.iter().all(|(c, _)| HOMOGLYPH.contains(*c));
            if latin && !homoglyphs_only {
                // Transliteration changes the length, so source positions
                // are spread over the output proportionally.
                let raw: String = seg.iter().map(|(c, _)| *c).collect();
                let out: Vec<char> = translit(&raw).chars().collect();
                for (k, &c) in out.iter().enumerate() {
                    let at = seg[k * seg.len() / out.len().max(1)].1;
                    self.push(c, at);
                }
            } else {
                for &(c, at) in seg {
                    if c.is_alphabetic() {
                        self.push(c, at);
                    }
                }
            }
        }
        run.clear();
    }

    /// skeleton comes last, so that text, leet and transliteration land in
    /// one alphabet. Per char, to keep the position mapping.
    fn push(&mut self, c: char, at: usize) {
        let mut b = [0u8; 4];
        for sc in unicode_security::skeleton(c.encode_utf8(&mut b)) {
            self.text.push(sc);
            self.map.push(at);
        }
    }

    /// Russian inflects by suffix, so a stem must match from the start of a
    /// word: `антивоен` in `антивоенные` yes, `окоп` in `быстрокопится` no.
    pub fn at_word_start(&self, start: usize) -> bool {
        self.map
            .get(start)
            .and_then(|&src| self.starts.get(src))
            .copied()
            .unwrap_or(false)
    }

    /// A word start is not enough: the stem `наки` matched `на кино` —
    /// correct start, but crawling over the space. So a one-word term must
    /// not cross a space, unless every word it touches is one char long.
    pub fn single_word_ok(&self, start: usize, end: usize) -> bool {
        let (Some(&a), Some(&b)) = (self.map.get(start), self.map.get(end.saturating_sub(1)))
        else {
            return false;
        };
        let mut crosses = false;
        for i in a..=b.min(self.word_len.len().saturating_sub(1)) {
            match self.word_len[i] {
                0 => crosses = true, // a space inside the match
                1 => {}              // one-char fragment: obfuscation
                _ if crosses => return false,
                _ => {}
            }
        }
        if !crosses {
            return true;
        }
        // A space was crossed: allowed only if every piece is one char long.
        (a..=b.min(self.word_len.len().saturating_sub(1))).all(|i| self.word_len[i] <= 1)
    }

    /// The half-open source range a match maps back to.
    pub fn surface(&self, start: usize, end: usize) -> Option<(usize, usize)> {
        let a = *self.map.get(start)?;
        let b = *self.map.get(end.checked_sub(1)?)?;
        Some((a, b + 1))
    }
}

/// NFKC → drop combining marks → lowercase. No skeleton: that comes last.
fn fold_char(c: char) -> Vec<char> {
    let mut buf = [0u8; 4];
    let mut out = Vec::new();

    for nc in c.encode_utf8(&mut buf).nfkc() {
        if is_combining(nc) {
            continue;
        }
        out.extend(nc.to_lowercase());
    }
    out
}

fn is_combining(c: char) -> bool {
    matches!(c as u32,
        0x0300..=0x036F | 0x1AB0..=0x1AFF | 0x1DC0..=0x1DFF
        | 0x20D0..=0x20F0 | 0xFE20..=0xFE2F)
}

/// Latin letters that look like Cyrillic ones (а с е о р х у). Only these
/// mean a look-alike substitution rather than transliteration.
const HOMOGLYPH: &str = "aceopxy";

/// Digits and symbols used in place of letters. Which alphabet they land in
/// does not matter: skeleton folds Latin `o` and Cyrillic `о` at the end.
fn leet(c: char) -> char {
    match c {
        '0' => 'o',
        '1' => 'i',
        '@' => 'a',
        '$' => 's',
        '3' => 'з',
        '4' => 'ч',
        '6' => 'б',
        '7' => 'т',
        '8' => 'в',
        _ => c,
    }
}

/// Latin → Cyrillic, greedy on the longer digraphs. Lossy by design: the
/// goal is recall, not reversibility.
fn translit(s: &str) -> String {
    const MULTI: &[(&str, &str)] = &[
        ("shch", "щ"),
        ("sch", "щ"),
        ("zh", "ж"),
        ("kh", "х"),
        ("ch", "ч"),
        ("sh", "ш"),
        ("ts", "ц"),
        ("yu", "ю"),
        ("ya", "я"),
        ("yo", "ё"),
        ("ye", "е"),
        ("iy", "ий"),
        ("ay", "ай"),
        ("oy", "ой"),
        ("ey", "ей"),
    ];
    const SINGLE: &[(char, &str)] = &[
        ('a', "а"),
        ('b', "б"),
        ('c', "ц"),
        ('d', "д"),
        ('e', "е"),
        ('f', "ф"),
        ('g', "г"),
        ('h', "х"),
        ('i', "и"),
        ('j', "ж"),
        ('k', "к"),
        ('l', "л"),
        ('m', "м"),
        ('n', "н"),
        ('o', "о"),
        ('p', "п"),
        ('q', "к"),
        ('r', "р"),
        ('s', "с"),
        ('t', "т"),
        ('u', "у"),
        ('v', "в"),
        ('w', "в"),
        ('x', "кс"),
        ('y', "ы"),
        ('z', "з"),
    ];

    let cs: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len() * 2);
    let mut i = 0;

    'outer: while i < cs.len() {
        for (pat, rep) in MULTI {
            let n = pat.len();
            if i + n <= cs.len() && cs[i..i + n].iter().collect::<String>() == *pat {
                out.push_str(rep);
                i += n;
                continue 'outer;
            }
        }
        match SINGLE.iter().find(|(k, _)| *k == cs[i]) {
            Some((_, rep)) => out.push_str(rep),
            None => out.push(cs[i]),
        }
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mixed_script_word_still_transliterates() {
        assert_eq!(n("красikov"), n("красиков"));
        assert_eq!(n("движenie"), n("движение"));
        assert_eq!(n("Дwиjение"), n("движение"));
        assert_eq!(n("ЕлиZаVета"), n("елизавета"));
    }

    #[test]
    fn single_latin_letter_stays_homoglyph() {
        assert_eq!(n("путин"), n("пyтин"));
        assert_eq!(n("сурков"), n("cypкoв"));
        assert_eq!(n("россия"), n("poccия"));
    }

    #[test]
    fn single_script_words_unchanged() {
        assert_eq!(n("put1n"), n("путин"));
        assert_eq!(n("navalny"), n("навалны"));
    }

    fn n(s: &str) -> String {
        Normalized::build(s).text
    }

    /// Terms go through the same normalization as the corpus, so only
    /// normalized may be compared with normalized.
    fn has(text: &str, term: &str) -> bool {
        n(text).contains(&n(term))
    }

    #[test]
    fn separators_inside_word_collapse() {
        for s in ["в.о.й.н.а", "в-о-й-н-а", "в о й н а", "в_о_й_н_а"] {
            assert!(has(s, "война"), "{s}");
        }
    }

    #[test]
    fn homoglyphs_fold() {
        assert_eq!(n("путин"), n("пyтин"));
    }

    #[test]
    fn leet_folds_inside_words() {
        assert!(has("в0йна", "война"));
        assert!(has("3ло", "зло"));
        // `1` for `и` only works through transliteration: Cyrillic `и` and
        // Latin `i` are not confusables, skeleton does not fold them.
        assert!(has("put1n", "путин"));
    }

    #[test]
    fn standalone_numbers_are_dropped() {
        // Otherwise leet turns «2024» into letters glued to its neighbours.
        assert_eq!(n("это было в 2024 году"), n("это было в году"));
    }

    #[test]
    fn translit_maps_to_cyrillic() {
        assert!(has("voyna", "война"));
    }

    #[test]
    fn substring_search_gives_morphology_for_free() {
        for form in ["антивоенный", "антивоенные", "антивоенного"]
        {
            assert!(has(form, "антивоен"), "{form}");
        }
    }

    #[test]
    fn multiword_terms_collapse_into_one_run() {
        assert!(has("оформляю вид на жительство", "вид на жительств"));
    }

    #[test]
    fn match_must_start_at_word_start() {
        // The collapsed form glues words together and the stem sits on the seam.
        for phrase in ["быстро копится", "глубоко копаешь", "покопаться"]
        {
            let nz = Normalized::build(phrase);
            let term = n("окоп");
            let byte = nz.text.find(&term).expect("substring must be there");
            let start = nz.text[..byte].chars().count();
            assert!(!nz.at_word_start(start), "{phrase}");
        }
        let nz = Normalized::build("сидели в окопах");
        let term = n("окоп");
        let byte = nz.text.find(&term).unwrap();
        assert!(nz.at_word_start(nz.text[..byte].chars().count()));
    }

    #[test]
    fn single_word_term_must_not_cross_a_space() {
        // This stem matched across a space 264 times before the check.
        for phrase in ["пошли на кино", "мило в этом смысле", "бы ковать железо"]
        {
            let nz = Normalized::build(phrase);
            for term in ["наки", "милов", "быков"] {
                let t = n(term);
                if let Some(byte) = nz.text.find(&t) {
                    let start = nz.text[..byte].chars().count();
                    let end = start + t.chars().count();
                    assert!(!nz.single_word_ok(start, end), "{phrase} / {term}");
                }
            }
        }
    }

    #[test]
    fn letter_spaced_obfuscation_still_matches() {
        // `в о й н а` must cross spaces: every fragment is one letter.
        let nz = Normalized::build("началась в о й н а опять");
        let t = n("война");
        let byte = nz.text.find(&t).expect("substring is there");
        let start = nz.text[..byte].chars().count();
        assert!(nz.single_word_ok(start, start + t.chars().count()));
    }

    #[test]
    fn word_start_survives_separator_obfuscation() {
        // In `в.о.й.н.а` every letter after a dot is a word start too.
        let nz = Normalized::build("началась в.о.й.н.а");
        let term = n("война");
        let byte = nz.text.find(&term).unwrap();
        assert!(nz.at_word_start(nz.text[..byte].chars().count()));
    }

    #[test]
    fn distinct_words_stay_distinct() {
        assert!(!has("поехали на море", "война"));
    }

    #[test]
    fn surface_maps_back_to_original() {
        let src = "начал в.о.й.н.а закончил";
        let nz = Normalized::build(src);
        let term = n("война");
        let byte = nz.text.find(&term).expect("no match");
        let start = nz.text[..byte].chars().count();
        let (a, b) = nz
            .surface(start, start + term.chars().count())
            .expect("no mapping");
        let got: String = src.chars().skip(a).take(b - a).collect();
        assert_eq!(got, "в.о.й.н.а");
    }
}
