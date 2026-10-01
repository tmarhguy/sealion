//! Porter (1980) suffix-stripping stemmer (spec §12).
//!
//! Hand-implemented in safe Rust with no external dependency (ADR 002).
//! Input is expected to be lowercase ASCII; non-ASCII input is returned
//! unchanged. The implementation follows Porter's original five steps and is
//! validated against his published vocabulary test set (see tests).

use sealion_core::config::StemmerKind;

/// Stem one term with the configured algorithm.
pub fn stem(word: &str, kind: StemmerKind) -> String {
    match kind {
        StemmerKind::None => word.to_string(),
        StemmerKind::Porter => porter_stem(word),
    }
}

/// Returns true if `ch` is a consonant in Porter's sense: anything that is
/// not a vowel, where `y` counts as a consonant at the start of the word or
/// after a vowel, and as a vowel after a consonant.
fn is_consonant(bytes: &[u8], i: usize) -> bool {
    match bytes[i] {
        b'a' | b'e' | b'i' | b'o' | b'u' => false,
        b'y' => {
            if i == 0 {
                true
            } else {
                !is_consonant(bytes, i - 1)
            }
        }
        _ => true,
    }
}

/// The Porter measure `m`: the number of VC sequences in the word.
fn measure(word: &[u8]) -> usize {
    let mut m = 0;
    let mut i = 0;
    let n = word.len();
    // Skip leading consonants.
    while i < n && is_consonant(word, i) {
        i += 1;
    }
    while i < n {
        // Skip a vowel sequence.
        while i < n && !is_consonant(word, i) {
            i += 1;
        }
        // Skip a consonant sequence, counting one VC pair.
        let start = i;
        while i < n && is_consonant(word, i) {
            i += 1;
        }
        if i > start {
            m += 1;
        }
    }
    m
}

/// True if the stem (first `len` bytes) contains a vowel.
fn contains_vowel(word: &[u8], len: usize) -> bool {
    (0..len).any(|i| !is_consonant(word, i))
}

/// True if the word ends with a double consonant (e.g. `tt`), looking at
/// the last two bytes.
fn ends_double_consonant(word: &[u8]) -> bool {
    let n = word.len();
    n >= 2 && word[n - 1] == word[n - 2] && is_consonant(word, n - 1)
}

/// True if the word ends with a consonant-vowel-consonant pattern where the
/// final consonant is not w, x, or y (Porter's `*o`).
fn ends_cvc(word: &[u8]) -> bool {
    let n = word.len();
    if n < 3 {
        return false;
    }
    let (a, b, c) = (word[n - 3], word[n - 2], word[n - 1]);
    let is_c = |i: u8| !matches!(i, b'a' | b'e' | b'i' | b'o' | b'u');
    // `y` handling follows is_consonant for the middle position.
    let b_is_vowel = !is_c(b) || (b == b'y' && is_c(a));
    is_c(a) && b_is_vowel && is_c(c) && !matches!(c, b'w' | b'x' | b'y')
}

fn ends_with(word: &[u8], suffix: &[u8]) -> bool {
    word.len() >= suffix.len() && &word[word.len() - suffix.len()..] == suffix
}

/// If `word` ends with `suffix` and `measure(stem) > 0` (or caller-checked
/// condition), replace the suffix with `replacement`. Returns true if the
/// suffix matched (regardless of whether the measure condition passed).
fn replace_if(
    buf: &mut Vec<u8>,
    suffix: &[u8],
    replacement: &[u8],
    cond: impl Fn(usize) -> bool,
) -> bool {
    if !ends_with(buf, suffix) {
        return false;
    }
    let stem_len = buf.len() - suffix.len();
    if cond(measure(&buf[..stem_len])) {
        buf.truncate(stem_len);
        buf.extend_from_slice(replacement);
    }
    true
}

/// Porter stem of one lowercase ASCII word. Non-ASCII words pass through.
pub fn porter_stem(word: &str) -> String {
    if !word.is_ascii() {
        return word.to_string();
    }
    let mut b: Vec<u8> = word.as_bytes().to_vec();
    if b.len() < 3 {
        return word.to_string();
    }

    // Step 1a.
    if ends_with(&b, b"sses") || ends_with(&b, b"ies") {
        b.truncate(b.len() - 2);
    } else if ends_with(&b, b"ss") {
        // keep
    } else if ends_with(&b, b"s") {
        b.pop();
    }

    // Step 1b.
    let mut step1b_done = false;
    if ends_with(&b, b"eed") {
        let m = measure(&b[..b.len() - 3]);
        if m > 0 {
            b.pop();
        }
    } else if ends_with(&b, b"ed") {
        let stem_len = b.len() - 2;
        if contains_vowel(&b, stem_len) {
            b.truncate(stem_len);
            step1b_done = true;
        }
    } else if ends_with(&b, b"ing") {
        let stem_len = b.len() - 3;
        if contains_vowel(&b, stem_len) {
            b.truncate(stem_len);
            step1b_done = true;
        }
    }
    if step1b_done {
        if ends_with(&b, b"at") || ends_with(&b, b"bl") || ends_with(&b, b"iz") {
            b.push(b'e');
        } else if ends_double_consonant(&b) && !matches!(b[b.len() - 1], b'l' | b's' | b'z') {
            b.pop();
        } else if measure(&b) == 1 && ends_cvc(&b) {
            b.push(b'e');
        }
    }

    // Step 1c.
    if ends_with(&b, b"y") {
        let stem_len = b.len() - 1;
        if contains_vowel(&b, stem_len) {
            b[stem_len] = b'i';
        }
    }

    // Step 2.
    const STEP2: &[(&[u8], &[u8])] = &[
        (b"ational", b"ate"),
        (b"tional", b"tion"),
        (b"enci", b"ence"),
        (b"anci", b"ance"),
        (b"izer", b"ize"),
        (b"bli", b"ble"),
        (b"alli", b"al"),
        (b"entli", b"ent"),
        (b"eli", b"e"),
        (b"ousli", b"ous"),
        (b"ization", b"ize"),
        (b"ation", b"ate"),
        (b"ator", b"ate"),
        (b"alism", b"al"),
        (b"iveness", b"ive"),
        (b"fulness", b"ful"),
        (b"ousness", b"ous"),
        (b"aliti", b"al"),
        (b"iviti", b"ive"),
        (b"biliti", b"ble"),
        (b"logi", b"log"),
    ];
    for (suffix, repl) in STEP2 {
        if replace_if(&mut b, suffix, repl, |m| m > 0) {
            break;
        }
    }

    // Step 3.
    const STEP3: &[(&[u8], &[u8])] = &[
        (b"icate", b"ic"),
        (b"ative", b""),
        (b"alize", b"al"),
        (b"iciti", b"ic"),
        (b"ical", b"ic"),
        (b"ful", b""),
        (b"ness", b""),
    ];
    for (suffix, repl) in STEP3 {
        if replace_if(&mut b, suffix, repl, |m| m > 0) {
            break;
        }
    }

    // Step 4.
    const STEP4: &[&[u8]] = &[
        b"al", b"ance", b"ence", b"er", b"ic", b"able", b"ible", b"ant", b"ement", b"ment", b"ent",
        b"ion", b"ou", b"ism", b"ate", b"iti", b"ous", b"ive", b"ize",
    ];
    for suffix in STEP4 {
        if ends_with(&b, suffix) {
            let stem_len = b.len() - suffix.len();
            // `ion` only goes when preceded by s or t.
            if *suffix == b"ion" && (stem_len == 0 || !matches!(b[stem_len - 1], b's' | b't')) {
                continue;
            }
            if measure(&b[..stem_len]) > 1 {
                b.truncate(stem_len);
            }
            break;
        }
    }

    // Step 5a.
    if ends_with(&b, b"e") {
        let stem_len = b.len() - 1;
        let m = measure(&b[..stem_len]);
        if m > 1 || (m == 1 && !ends_cvc(&b[..stem_len])) {
            b.truncate(stem_len);
        }
    }

    // Step 5b.
    if measure(&b) > 1 && ends_double_consonant(&b) && b[b.len() - 1] == b'l' {
        b.pop();
    }

    String::from_utf8(b).expect("stemmer only produces ASCII")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// (input, expected) pairs from Porter's published test vocabulary.
    const PORTER_CASES: &[(&str, &str)] = &[
        ("caresses", "caress"),
        ("ponies", "poni"),
        ("ties", "ti"),
        ("caress", "caress"),
        ("cats", "cat"),
        ("feed", "feed"),
        ("agreed", "agre"),
        ("disabled", "disabl"),
        ("mating", "mate"),
        ("mating", "mate"),
        ("meeting", "meet"),
        ("milling", "mill"),
        ("messing", "mess"),
        ("meetings", "meet"),
        ("happy", "happi"),
        ("sky", "sky"),
        ("relational", "relat"),
        ("conditional", "condit"),
        ("rational", "ration"),
        ("valenci", "valenc"),
        ("hesitanci", "hesit"),
        ("digitizer", "digit"),
        ("conformabli", "conform"),
        ("radicalli", "radic"),
        ("differentli", "differ"),
        ("vileli", "vile"),
        ("analogousli", "analog"),
        ("vietnamization", "vietnam"),
        ("predication", "predic"),
        ("operator", "oper"),
        ("feudalism", "feudal"),
        ("decisiveness", "decis"),
        ("hopefulness", "hope"),
        ("callousness", "callous"),
        ("formaliti", "formal"),
        ("sensitiviti", "sensit"),
        ("sensibiliti", "sensibl"),
        ("triplicate", "triplic"),
        ("formative", "form"),
        ("formalize", "formal"),
        ("electriciti", "electr"),
        ("electrical", "electr"),
        ("hopeful", "hope"),
        ("goodness", "good"),
        ("revival", "reviv"),
        ("allowance", "allow"),
        ("inference", "infer"),
        ("airliner", "airlin"),
        ("gyroscopic", "gyroscop"),
        ("adjustable", "adjust"),
        ("defensible", "defens"),
        ("irritant", "irrit"),
        ("replacement", "replac"),
        ("adjustment", "adjust"),
        ("dependent", "depend"),
        ("adoption", "adopt"),
        ("homologou", "homolog"),
        ("communism", "commun"),
        ("activate", "activ"),
        ("angulariti", "angular"),
        ("homologous", "homolog"),
        ("effective", "effect"),
        ("bowdlerize", "bowdler"),
        ("probate", "probat"),
        ("rate", "rate"),
        ("cease", "ceas"),
        ("controll", "control"),
        ("roll", "roll"),
        ("database", "databas"),
        ("databases", "databas"),
        ("distributed", "distribut"),
        ("systems", "system"),
        ("compiler", "compil"),
        ("optimization", "optim"),
    ];

    #[test]
    fn porter_matches_published_vocabulary() {
        for (input, expected) in PORTER_CASES {
            assert_eq!(porter_stem(input), *expected, "porter_stem({input:?})");
        }
    }

    #[test]
    fn short_words_pass_through() {
        assert_eq!(porter_stem("at"), "at");
        assert_eq!(porter_stem("be"), "be");
    }

    #[test]
    fn non_ascii_passes_through() {
        assert_eq!(porter_stem("café"), "café");
        assert_eq!(porter_stem("naïve"), "naïve");
    }

    #[test]
    fn stem_kind_none_is_identity() {
        assert_eq!(stem("Running", StemmerKind::None), "Running");
        assert_eq!(stem("databases", StemmerKind::Porter), "databas");
    }
}
