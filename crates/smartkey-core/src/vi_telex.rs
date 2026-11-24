use std::borrow::Cow;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Resolves the installed `fcitx5-lotus` Vietnamese syllable dictionary path in priority order:
/// `SMARTKEY_LOTUS_DICT` -> `/usr/share/fcitx5/lotus/vietnamese.cm.dict` ->
/// `/usr/local/share/fcitx5/lotus/vietnamese.cm.dict` -> `~/.local/share/fcitx5/lotus/vietnamese.cm.dict`.
pub fn resolve_lotus_dict_path() -> Option<PathBuf> {
    if let Ok(env_path) = std::env::var("SMARTKEY_LOTUS_DICT") {
        let trimmed = env_path.trim();
        if !trimmed.is_empty() {
            let p = PathBuf::from(trimmed);
            if p.is_file() {
                return Some(p);
            }
        }
    }
    for sys in [
        "/usr/share/fcitx5/lotus/vietnamese.cm.dict",
        "/usr/local/share/fcitx5/lotus/vietnamese.cm.dict",
    ] {
        let p = PathBuf::from(sys);
        if p.is_file() {
            return Some(p);
        }
    }
    if let Some(home) = crate::event_log::resolve_home_dir() {
        let p = home.join(".local/share/fcitx5/lotus/vietnamese.cm.dict");
        if p.is_file() {
            return Some(p);
        }
    }
    None
}

/// Reads and normalizes a `vietnamese.cm.dict` file into a sorted, deduplicated syllable set.
/// Returns `None` if unreadable or empty after stripping blanks/comments.
pub fn load_lotus_dict_from_path(path: &Path) -> Option<Vec<Box<str>>> {
    let raw = std::fs::read_to_string(path).ok()?;
    let mut words: Vec<Box<str>> = raw
        .lines()
        .map(str::trim)
        .filter(|s| !s.is_empty() && !s.starts_with('#'))
        .map(|s| s.to_string().into_boxed_str())
        .collect();
    words.sort_unstable();
    words.dedup();
    if words.is_empty() {
        None
    } else {
        Some(words)
    }
}

/// Computes `(added, removed)` set difference between two sorted, deduplicated syllable slices.
pub fn diff_syllable_sets(lotus: &[Box<str>], pack: &[Box<str>]) -> (usize, usize) {
    let mut i = 0usize;
    let mut j = 0usize;
    let mut added = 0usize;
    let mut removed = 0usize;
    while i < lotus.len() && j < pack.len() {
        match lotus[i].as_ref().cmp(pack[j].as_ref()) {
            std::cmp::Ordering::Less => {
                added += 1;
                i += 1;
            }
            std::cmp::Ordering::Greater => {
                removed += 1;
                j += 1;
            }
            std::cmp::Ordering::Equal => {
                i += 1;
                j += 1;
            }
        }
    }
    added += lotus.len().saturating_sub(i);
    removed += pack.len().saturating_sub(j);
    (added, removed)
}

#[inline]
pub fn is_dict_word_in(dict: &[Box<str>], word_lower: &str) -> bool {
    !word_lower.is_empty()
        && dict
            .binary_search_by(|w| w.as_ref().cmp(word_lower))
            .is_ok()
}

#[inline]
pub fn has_dict_prefix_in(dict: &[Box<str>], word_lower: &str) -> bool {
    if word_lower.is_empty() {
        return false;
    }
    let idx = dict.partition_point(|w| w.as_ref() < word_lower);
    dict.get(idx)
        .is_some_and(|w| w.as_ref().starts_with(word_lower))
}

fn cached_system_lotus_dict() -> Option<&'static [Box<str>]> {
    static SYSTEM_LOTUS: OnceLock<Option<Vec<Box<str>>>> = OnceLock::new();
    SYSTEM_LOTUS
        .get_or_init(|| {
            for sys in [
                "/usr/share/fcitx5/lotus/vietnamese.cm.dict",
                "/usr/local/share/fcitx5/lotus/vietnamese.cm.dict",
            ] {
                let p = PathBuf::from(sys);
                if p.is_file() {
                    if let Some(words) = load_lotus_dict_from_path(&p) {
                        return Some(words);
                    }
                }
            }
            if let Some(home) = crate::event_log::resolve_home_dir() {
                let p = home.join(".local/share/fcitx5/lotus/vietnamese.cm.dict");
                if p.is_file() {
                    if let Some(words) = load_lotus_dict_from_path(&p) {
                        return Some(words);
                    }
                }
            }
            None
        })
        .as_deref()
}

fn resolve_default_vi_syllables() -> Cow<'static, [Box<str>]> {
    if let Ok(env_path) = std::env::var("SMARTKEY_LOTUS_DICT") {
        let trimmed = env_path.trim();
        if !trimmed.is_empty() {
            let p = PathBuf::from(trimmed);
            if p.is_file() {
                if let Some(words) = load_lotus_dict_from_path(&p) {
                    return Cow::Owned(words);
                }
            }
        }
    }
    if let Some(sys) = cached_system_lotus_dict() {
        return Cow::Borrowed(sys);
    }
    if let Some(lex) = crate::lexicon::bundled_vi_lexicon() {
        return Cow::Borrowed(lex.syllable_dict());
    }
    Cow::Borrowed(&[])
}

/// Checks whether `word_lower` is an exact syllable in the active Lotus / Vietnamese dictionary.
pub fn is_vietnamese_dict_word(word_lower: &str) -> bool {
    if word_lower.is_empty() {
        return false;
    }
    let dict = resolve_default_vi_syllables();
    is_dict_word_in(&dict, word_lower)
}

/// Checks whether `word_lower` is in the bundled Vietnamese slang/loanword shield or shorthand trigger set.
/// Note: Kept completely separate from `vietnamese.cm.dict` so `simulate_lotus_word` remains 1:1 with `fcitx5-lotus`.
pub fn is_vietnamese_slang_word(word_lower: &str) -> bool {
    if word_lower.is_empty() {
        return false;
    }
    crate::lexicon::bundled_vi_lexicon()
        .is_some_and(|lex| lex.is_slang_word(word_lower) || lex.lookup_shorthand(word_lower).is_some())
}

/// Looks up a curated unambiguous Vietnamese shorthand auto-expansion (`ko -> không`, `dc -> được`)
/// from the bundled v3 Vietnamese pack's `SHORTHAND` section.
pub fn lookup_vietnamese_shorthand(word_lower: &str) -> Option<&'static str> {
    if word_lower.is_empty() {
        return None;
    }
    crate::lexicon::bundled_vi_lexicon()?.lookup_shorthand(word_lower)
}

const VOWELS: [char; 72] = [
    'a', 'à', 'á', 'ả', 'ã', 'ạ', // 0..6
    'ă', 'ằ', 'ắ', 'ẳ', 'ẵ', 'ặ', // 6..12
    'â', 'ầ', 'ấ', 'ẩ', 'ẫ', 'ậ', // 12..18
    'e', 'è', 'é', 'ẻ', 'ẽ', 'ẹ', // 18..24
    'ê', 'ề', 'ế', 'ể', 'ễ', 'ệ', // 24..30
    'i', 'ì', 'í', 'ỉ', 'ĩ', 'ị', // 30..36
    'o', 'ò', 'ó', 'ỏ', 'õ', 'ọ', // 36..42
    'ô', 'ồ', 'ố', 'ổ', 'ỗ', 'ộ', // 42..48
    'ơ', 'ờ', 'ớ', 'ở', 'ỡ', 'ợ', // 48..54
    'u', 'ù', 'ú', 'ủ', 'ũ', 'ụ', // 54..60
    'ư', 'ừ', 'ứ', 'ử', 'ữ', 'ự', // 60..66
    'y', 'ỳ', 'ý', 'ỷ', 'ỹ', 'ỵ', // 66..72
];

const FIRST_CONSONANTS: &[&str] = &[
    "b", "c", "ch", "d", "đ", "g", "gh", "gi", "h", "k", "kh", "kr", "l", "m", "n", "ng", "ngh",
    "nh", "p", "ph", "qu", "r", "s", "t", "th", "tr", "v", "x", "z",
];

const VOWEL_ROWS: &[&[&str]] = &[
    &["ê", "i", "ua", "uê", "uy", "y"],
    &["a", "iê", "oa", "uyê", "yê"],
    &[
        "â", "ă", "e", "o", "oo", "ô", "ơ", "oe", "u", "ư", "uâ", "uô", "ươ",
    ],
    &["oă"],
    &["uơ"],
    &[
        "ai", "ao", "au", "âu", "ay", "ây", "eo", "êu", "ia", "iêu", "iu", "oai", "oao", "oay",
        "oeo", "oi", "ôi", "ơi", "ưa", "uây", "ui", "ưi", "uôi", "ươi", "ươu", "ưu", "uya", "uyu",
        "uêu", "yêu",
    ],
    &["ă", "u"],
    &["i"],
];

const LAST_CONSONANT_ROWS: &[&[&str]] = &[
    &["ch", "nh"],
    &["c", "ng"],
    &["m", "n", "p", "t"],
    &["k"],
    &["c"],
];

const VC_MATRIX: &[&[usize]] = &[&[0, 2], &[0, 1, 2], &[1, 2], &[1, 2], &[], &[], &[3], &[4]];

/// Projects user/sentence capitalization onto a canonical shorthand expansion (`Ko -> Không`, `Pls -> Please`,
/// `idk -> I don't know`, `vn -> Việt Nam`) without lowercasing proper nouns inside the expansion.
pub fn project_shorthand_case(
    raw_typed: &str,
    canonical_expansion: &str,
    force_cap_initial: bool,
) -> String {
    let mut typed_chars = raw_typed.chars().filter(|c| c.is_alphabetic());
    let first_alpha = typed_chars.next();
    let cap_first = force_cap_initial || first_alpha.is_some_and(|c| c.is_uppercase());
    if !cap_first {
        return canonical_expansion.to_string();
    }
    let mut exp_chars = canonical_expansion.chars();
    let Some(first) = exp_chars.next() else {
        return String::new();
    };
    let mut out = String::with_capacity(canonical_expansion.len() + 4);
    for uc in first.to_uppercase() {
        out.push(uc);
    }
    out.extend(exp_chars);
    out
}

/// Checks whether `word_lower` is a word or prefix of any word in the active Lotus / Vietnamese dictionary
/// (1:1 with `lotus::Dictionary::hasWordOrPrefix`).
pub fn has_vietnamese_dict_prefix(word_lower: &str) -> bool {
    if word_lower.is_empty() {
        return false;
    }
    let dict = resolve_default_vi_syllables();
    has_dict_prefix_in(&dict, word_lower)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
enum Mark {
    None = 0,
    Hat = 1,
    Breve = 2,
    Horn = 3,
    Dash = 4,
    Raw = 5,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
enum Tone {
    None = 0,
    Grave = 1,
    Acute = 2,
    Hook = 3,
    Tilde = 4,
    Dot = 5,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EffectType {
    Appending,
    MarkTransformation(Mark),
    ToneTransformation(Tone),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Rule {
    key: char,
    effect_type: EffectType,
    effect_on: char,
    result: char,
}

#[derive(Debug, Clone)]
struct Transformation {
    id: usize,
    target: Option<usize>,
    rule: Rule,
    is_upper_case: bool,
}

fn find_vowel_position(ch: char) -> Option<usize> {
    let lower = ch.to_lowercase().next().unwrap_or(ch);
    VOWELS.iter().position(|&v| v == lower)
}

fn is_vowel(ch: char) -> bool {
    find_vowel_position(ch).is_some()
}

fn mark_family(ch: char) -> Option<[char; 5]> {
    match ch {
        'a' | 'â' | 'ă' => Some(['a', 'â', 'ă', '_', '_']),
        'd' | 'đ' => Some(['d', '_', '_', '_', 'đ']),
        'e' | 'ê' => Some(['e', 'ê', '_', '_', '_']),
        'o' | 'ô' | 'ơ' => Some(['o', 'ô', '_', 'ơ', '_']),
        'u' | 'ư' => Some(['u', '_', '_', 'ư', '_']),
        _ => None,
    }
}

fn add_mark_to_toneless_char(ch: char, mark: Mark) -> char {
    let Some(family) = mark_family(ch) else {
        return ch;
    };
    let slot = mark as usize;
    if slot >= family.len() || family[slot] == '_' {
        return ch;
    }
    family[slot]
}

fn find_tone_from_char(ch: char) -> Tone {
    let Some(pos) = find_vowel_position(ch) else {
        return Tone::None;
    };
    match pos % 6 {
        1 => Tone::Grave,
        2 => Tone::Acute,
        3 => Tone::Hook,
        4 => Tone::Tilde,
        5 => Tone::Dot,
        _ => Tone::None,
    }
}

fn add_tone_to_char(ch: char, tone: Tone) -> char {
    let Some(pos) = find_vowel_position(ch) else {
        return ch;
    };
    let target = pos + (tone as usize) - (pos % 6);
    VOWELS.get(target).copied().unwrap_or(ch)
}

fn add_mark_to_char(ch: char, mark: Mark) -> char {
    let tone = find_tone_from_char(ch);
    let bare = add_tone_to_char(ch, Tone::None);
    let marked = add_mark_to_toneless_char(bare, mark);
    add_tone_to_char(marked, tone)
}

pub fn is_vietnamese_rune(ch: char) -> bool {
    let lower = ch.to_lowercase().next().unwrap_or(ch);
    if lower.is_ascii() {
        return false;
    }
    if find_tone_from_char(lower) != Tone::None {
        return true;
    }
    lower != add_mark_to_toneless_char(lower, Mark::None)
}

/// Returns `true` if `word` contains any Vietnamese diacritic or tone mark (`ă, â, đ, ê, ô, ơ, ư` or tones).
pub fn has_any_vietnamese_rune(word: &str) -> bool {
    word.chars().any(is_vietnamese_rune)
}

/// Returns `true` when `composed_lower` ends in a toned open `ă`/`â` vowel
/// (`ằ, ắ, ẳ, ẵ, ặ, ầ, ấ, ẩ, ẫ, ậ`). In Vietnamese phonology, `ă` and `â` require
/// a coda/glide and never form real toned open syllables (unlike toneless `ă`/`â`/`đ`
/// used in `"chữ ă"`, `"chữ â"`, `"chữ đ"`, and `"kpă"`).
pub fn is_impossible_standalone_vi_syllable(composed_lower: &str) -> bool {
    composed_lower.chars().last().is_some_and(|ch| {
        matches!(
            ch,
            'ằ' | 'ắ' | 'ẳ' | 'ẵ' | 'ặ' | 'ầ' | 'ấ' | 'ẩ' | 'ẫ' | 'ậ'
        )
    })
}

fn has_any_vietnamese_vowel(word: &str) -> bool {
    word.chars().any(is_vowel)
}

fn telex_rules() -> &'static Vec<Rule> {
    static RULES: OnceLock<Vec<Rule>> = OnceLock::new();
    RULES.get_or_init(|| {
        let mut rules = Vec::new();
        let tone_order = [
            Tone::None,
            Tone::Dot,
            Tone::Acute,
            Tone::Grave,
            Tone::Hook,
            Tone::Tilde,
        ];

        let add_mark_dsl =
            |rules: &mut Vec<Rule>, key: char, effective_ons: &[char], results: &[char]| {
                for (&eff_on, &res) in effective_ons.iter().zip(results.iter()) {
                    let Some(fam) = mark_family(res) else {
                        continue;
                    };
                    let Some(mark_idx) = fam.iter().position(|&c| c == res) else {
                        continue;
                    };
                    let effect = match mark_idx {
                        1 => Mark::Hat,
                        2 => Mark::Breve,
                        3 => Mark::Horn,
                        4 => Mark::Dash,
                        _ => Mark::None,
                    };
                    let Some(eff_fam) = mark_family(eff_on) else {
                        continue;
                    };
                    for &cp in &eff_fam {
                        if cp == '_' {
                            continue;
                        }
                        if cp == res {
                            rules.push(Rule {
                                key,
                                effect_type: EffectType::MarkTransformation(Mark::None),
                                effect_on: res,
                                result: eff_on,
                            });
                        } else if is_vowel(cp) {
                            for &tone in &tone_order {
                                rules.push(Rule {
                                    key,
                                    effect_type: EffectType::MarkTransformation(effect),
                                    effect_on: add_tone_to_char(cp, tone),
                                    result: add_tone_to_char(res, tone),
                                });
                            }
                        } else {
                            rules.push(Rule {
                                key,
                                effect_type: EffectType::MarkTransformation(effect),
                                effect_on: cp,
                                result: res,
                            });
                        }
                    }
                }
            };

        add_mark_dsl(&mut rules, 'a', &['a'], &['â']);
        add_mark_dsl(&mut rules, 'd', &['d'], &['đ']);
        add_mark_dsl(&mut rules, 'e', &['e'], &['ê']);
        rules.push(Rule {
            key: 'f',
            effect_type: EffectType::ToneTransformation(Tone::Grave),
            effect_on: '\0',
            result: '\0',
        });
        rules.push(Rule {
            key: 'j',
            effect_type: EffectType::ToneTransformation(Tone::Dot),
            effect_on: '\0',
            result: '\0',
        });
        add_mark_dsl(&mut rules, 'o', &['o'], &['ô']);
        rules.push(Rule {
            key: 'r',
            effect_type: EffectType::ToneTransformation(Tone::Hook),
            effect_on: '\0',
            result: '\0',
        });
        rules.push(Rule {
            key: 's',
            effect_type: EffectType::ToneTransformation(Tone::Acute),
            effect_on: '\0',
            result: '\0',
        });
        add_mark_dsl(&mut rules, 'w', &['u', 'o', 'a'], &['ư', 'ơ', 'ă']);
        rules.push(Rule {
            key: 'x',
            effect_type: EffectType::ToneTransformation(Tone::Tilde),
            effect_on: '\0',
            result: '\0',
        });
        rules.push(Rule {
            key: 'z',
            effect_type: EffectType::ToneTransformation(Tone::None),
            effect_on: '\0',
            result: '\0',
        });

        rules
    })
}

fn applicable_rules(key: char) -> Vec<Rule> {
    let lower = key.to_ascii_lowercase();
    telex_rules()
        .iter()
        .copied()
        .filter(|r| r.key == lower)
        .collect()
}

#[derive(Debug, Clone, Copy)]
struct FlattenMode {
    english_mode: bool,
    tone_less: bool,
    mark_less: bool,
    lower_case: bool,
}

impl FlattenMode {
    const VIETNAMESE: Self = Self {
        english_mode: false,
        tone_less: false,
        mark_less: false,
        lower_case: false,
    };
    const VIETNAMESE_LOWER: Self = Self {
        english_mode: false,
        tone_less: false,
        mark_less: false,
        lower_case: true,
    };
    const VIETNAMESE_TONELESS_LOWER: Self = Self {
        english_mode: false,
        tone_less: true,
        mark_less: false,
        lower_case: true,
    };
    const ENGLISH: Self = Self {
        english_mode: true,
        tone_less: false,
        mark_less: false,
        lower_case: false,
    };
    const ENGLISH_BARE_LOWER: Self = Self {
        english_mode: true,
        tone_less: true,
        mark_less: true,
        lower_case: true,
    };
}

fn flatten(composition: &[Transformation], mode: FlattenMode) -> String {
    let mut appending_list: Vec<&Transformation> = Vec::with_capacity(composition.len());
    let mut effects: Vec<&Transformation> = Vec::new();

    for trans in composition {
        let is_appending = trans.rule.effect_type == EffectType::Appending;
        if mode.english_mode || is_appending {
            if trans.rule.key == '\0' {
                continue;
            }
            appending_list.push(trans);
        } else if trans.target.is_some() {
            effects.push(trans);
        }
    }

    let mut out = String::with_capacity(appending_list.len());
    for app in appending_list {
        let mut ch = if mode.english_mode {
            app.rule.key
        } else {
            let mut c = app.rule.effect_on;
            for eff in &effects {
                if eff.target == Some(app.id) {
                    match eff.rule.effect_type {
                        EffectType::MarkTransformation(Mark::Raw) => {
                            c = app.rule.key;
                        }
                        EffectType::MarkTransformation(mark) => {
                            c = add_mark_to_char(c, mark);
                        }
                        EffectType::ToneTransformation(tone) => {
                            c = add_tone_to_char(c, tone);
                        }
                        EffectType::Appending => {}
                    }
                }
            }
            c
        };
        if mode.tone_less {
            ch = add_tone_to_char(ch, Tone::None);
        }
        if mode.mark_less {
            ch = add_mark_to_char(ch, Mark::None);
        }
        if mode.lower_case {
            for lc in ch.to_lowercase() {
                out.push(lc);
            }
        } else if app.is_upper_case {
            for uc in ch.to_uppercase() {
                out.push(uc);
            }
        } else {
            out.push(ch);
        }
    }
    out
}

fn find_root_target(composition: &[Transformation], target_id: usize) -> usize {
    let mut cur = target_id;
    while let Some(node) = composition.iter().find(|t| t.id == cur) {
        if let Some(parent) = node.target {
            cur = parent;
        } else {
            return cur;
        }
    }
    cur
}

fn extract_cvc_appending(
    appending: &[Transformation],
) -> (
    Vec<Transformation>,
    Vec<Transformation>,
    Vec<Transformation>,
) {
    let n = appending.len();
    if n == 0 {
        return (Vec::new(), Vec::new(), Vec::new());
    }

    let mut lc_start = n;
    while lc_start > 0 {
        let t = &appending[lc_start - 1];
        if t.target.is_none() && is_vowel(t.rule.result) {
            break;
        }
        lc_start -= 1;
    }

    let mut vo_start = lc_start;
    while vo_start > 0 {
        let t = &appending[vo_start - 1];
        if t.target.is_none() && !is_vowel(t.rule.result) {
            break;
        }
        vo_start -= 1;
    }

    let mut fc = appending[0..vo_start].to_vec();
    let mut vo = appending[vo_start..lc_start].to_vec();
    let mut lc = appending[lc_start..n].to_vec();

    if !lc.is_empty() && vo.is_empty() && fc.is_empty() {
        fc = lc;
        lc = Vec::new();
    }

    if fc.len() == 1
        && !vo.is_empty()
        && ((fc[0].rule.result == 'g'
            && vo[0].rule.result == 'i'
            && vo.len() > 1
            && (vo[1].rule.result != 'e' || lc.is_empty()))
            || (fc[0].rule.result == 'q' && vo[0].rule.result == 'u'))
    {
        fc = appending[0..vo_start + 1].to_vec();
        vo = appending[vo_start + 1..lc_start].to_vec();
    }

    (fc, vo, lc)
}

fn extract_cvc_trans(
    composition: &[Transformation],
) -> (
    Vec<Transformation>,
    Vec<Transformation>,
    Vec<Transformation>,
) {
    if composition.is_empty() {
        return (Vec::new(), Vec::new(), Vec::new());
    }
    let appending: Vec<Transformation> = composition
        .iter()
        .filter(|t| t.target.is_none())
        .cloned()
        .collect();
    let (mut fc, mut vo, mut lc) = extract_cvc_appending(&appending);
    if appending.len() == composition.len() {
        return (fc, vo, lc);
    }

    let vo_ids: Vec<usize> = vo.iter().map(|t| t.id).collect();
    let fc_ids: Vec<usize> = fc.iter().map(|t| t.id).collect();
    let lc_ids: Vec<usize> = lc.iter().map(|t| t.id).collect();

    for trans in composition {
        let Some(target_id) = trans.target else {
            continue;
        };
        if vo_ids.contains(&target_id) {
            vo.push(trans.clone());
        } else if fc_ids.contains(&target_id) {
            fc.push(trans.clone());
        } else if lc_ids.contains(&target_id) {
            lc.push(trans.clone());
        }
    }
    (fc, vo, lc)
}

fn lookup_seq(
    rows: &[&[&str]],
    input: &str,
    input_is_full: bool,
    input_is_complete: bool,
) -> Vec<usize> {
    let input_chars: Vec<char> = input.chars().collect();
    let input_len = input_chars.len();
    let mut result = Vec::new();

    for (row_idx, row) in rows.iter().enumerate() {
        for token in *row {
            let token_chars: Vec<char> = token.chars().collect();
            if token_chars.len() < input_len || (input_is_full && token_chars.len() > input_len) {
                continue;
            }
            let mut is_match = true;
            for k in 0..input_len {
                if input_chars[k] != token_chars[k]
                    && (input_is_complete
                        || add_mark_to_toneless_char(token_chars[k], Mark::None) != input_chars[k])
                {
                    is_match = false;
                    break;
                }
            }
            if is_match {
                result.push(row_idx);
                break;
            }
        }
    }
    result
}

fn is_valid_cvc(fc: &str, vo: &str, lc: &str, input_is_full_complete: bool) -> bool {
    let mut fc_idxs = Vec::new();
    let mut vo_idxs = Vec::new();
    let mut lc_idxs = Vec::new();

    if !fc.is_empty() {
        fc_idxs = lookup_seq(
            &[FIRST_CONSONANTS],
            fc,
            input_is_full_complete || !vo.is_empty(),
            true,
        );
        if fc_idxs.is_empty() {
            return false;
        }
    }
    if !vo.is_empty() {
        vo_idxs = lookup_seq(
            VOWEL_ROWS,
            vo,
            input_is_full_complete || !lc.is_empty(),
            input_is_full_complete,
        );
        if vo_idxs.is_empty() {
            return false;
        }
    }
    if !lc.is_empty() {
        lc_idxs = lookup_seq(LAST_CONSONANT_ROWS, lc, input_is_full_complete, true);
        if lc_idxs.is_empty() {
            return false;
        }
    }
    if vo_idxs.is_empty() {
        return !fc_idxs.is_empty();
    }
    if !lc_idxs.is_empty() {
        for &v in &vo_idxs {
            if let Some(allowed_row) = VC_MATRIX.get(v) {
                for &allowed in *allowed_row {
                    if lc_idxs.contains(&allowed) {
                        return true;
                    }
                }
            }
        }
        return false;
    }
    true
}

fn has_valid_tone(composition: &[Transformation], tone: Tone) -> bool {
    if matches!(tone, Tone::None | Tone::Acute | Tone::Dot) {
        return true;
    }
    let (_, _, lc) = extract_cvc_trans(composition);
    if lc.is_empty() {
        return true;
    }
    let lc_str = flatten(&lc, FlattenMode::ENGLISH_BARE_LOWER);
    !matches!(lc_str.as_str(), "c" | "k" | "p" | "t" | "ch")
}

fn is_valid_composition(composition: &[Transformation], input_is_full_complete: bool) -> bool {
    if composition.len() <= 1 {
        return true;
    }
    for trans in composition.iter().rev() {
        if let EffectType::ToneTransformation(tone) = trans.rule.effect_type {
            if !has_valid_tone(composition, tone) {
                return false;
            }
            break;
        }
    }
    let (fc, vo, lc) = extract_cvc_trans(composition);
    let mode = FlattenMode::VIETNAMESE_TONELESS_LOWER;
    is_valid_cvc(
        &flatten(&fc, mode),
        &flatten(&vo, mode),
        &flatten(&lc, mode),
        input_is_full_complete,
    )
}

fn find_tone_target_id(composition: &[Transformation], std_style: bool) -> Option<usize> {
    if composition.is_empty() {
        return None;
    }
    let (_, vowel, last_consonant) = extract_cvc_trans(composition);
    let vowels: Vec<&Transformation> = vowel
        .iter()
        .filter(|t| t.rule.effect_type == EffectType::Appending)
        .collect();
    if vowels.len() == 1 {
        return Some(vowels[0].id);
    } else if vowels.len() == 2 && std_style {
        let mut target = None;
        for trans in &vowel {
            if trans.rule.result == 'ơ' || trans.rule.result == 'ê' {
                target = Some(trans.target.unwrap_or(trans.id));
            }
        }
        if target.is_none() {
            target = Some(if last_consonant.is_empty() {
                vowels[0].id
            } else {
                vowels[1].id
            });
        }
        return target;
    } else if vowels.len() == 2 {
        if !last_consonant.is_empty() {
            return Some(vowels[1].id);
        }
        let vowel_clones: Vec<Transformation> = vowels.iter().map(|&t| t.clone()).collect();
        let flattened = flatten(&vowel_clones, FlattenMode::ENGLISH_BARE_LOWER);
        if matches!(flattened.as_str(), "oa" | "oe" | "uy" | "ue" | "uo") {
            return Some(vowels[1].id);
        }
        return Some(vowels[0].id);
    } else if vowels.len() == 3 {
        let vowel_clones: Vec<Transformation> = vowels.iter().map(|&t| t.clone()).collect();
        if flatten(&vowel_clones, FlattenMode::ENGLISH_BARE_LOWER) == "uye" {
            return Some(vowels[2].id);
        }
        return Some(vowels[1].id);
    }
    None
}

fn find_mark_target(
    composition: &[Transformation],
    rules: &[Rule],
    next_id: usize,
) -> Option<(usize, Rule)> {
    let cur_str = flatten(composition, FlattenMode::VIETNAMESE);
    for trans in composition.iter().rev() {
        for &rule in rules {
            let EffectType::MarkTransformation(mark) = rule.effect_type else {
                continue;
            };
            if trans.rule.result != rule.effect_on || mark == Mark::None {
                continue;
            }
            let target_id = find_root_target(composition, trans.id);
            let mut with_eff = composition.to_vec();
            with_eff.push(Transformation {
                id: next_id,
                target: Some(target_id),
                rule,
                is_upper_case: false,
            });
            if cur_str == flatten(&with_eff, FlattenMode::VIETNAMESE) {
                continue;
            }
            if is_valid_composition(&with_eff, false) {
                return Some((target_id, rule));
            }
        }
    }
    None
}

fn is_tone_free(composition: &[Transformation], target_id: usize) -> bool {
    !composition.iter().any(|node| {
        node.target == Some(target_id)
            && matches!(node.rule.effect_type, EffectType::ToneTransformation(_))
    })
}

fn find_last_appending_vowel_id(composition: &[Transformation]) -> Option<usize> {
    let last_app = composition
        .iter()
        .rev()
        .find(|t| t.rule.effect_type == EffectType::Appending)?;
    if is_vowel(last_app.rule.effect_on) {
        Some(last_app.id)
    } else {
        None
    }
}

fn find_target(
    composition: &[Transformation],
    rules: &[Rule],
    std_style: bool,
    free_marking: bool,
    next_id: usize,
) -> Option<(usize, Rule)> {
    let cur_str = flatten(composition, FlattenMode::VIETNAMESE);
    for &rule in rules {
        let EffectType::ToneTransformation(tone) = rule.effect_type else {
            continue;
        };
        let target_opt = if free_marking {
            if has_valid_tone(composition, tone) {
                find_tone_target_id(composition, std_style)
            } else {
                None
            }
        } else {
            find_last_appending_vowel_id(composition)
        };
        let Some(target_id) = target_opt else {
            continue;
        };
        let mut with_eff = composition.to_vec();
        with_eff.push(Transformation {
            id: next_id,
            target: Some(target_id),
            rule,
            is_upper_case: false,
        });
        if cur_str == flatten(&with_eff, FlattenMode::VIETNAMESE) {
            continue;
        }
        let target_node = composition.iter().find(|t| t.id == target_id)?;
        if tone == Tone::None
            && is_tone_free(composition, target_id)
            && find_tone_from_char(target_node.rule.result) == Tone::None
        {
            return None;
        }
        return Some((target_id, rule));
    }
    find_mark_target(composition, rules, next_id)
}

fn matches_uh_o(text: &str) -> bool {
    text.contains("ưo") || text.contains("ươ")
}

fn matches_uoh_tail(text: &str) -> bool {
    for head in ["uơ", "ưo"] {
        for (idx, _) in text.match_indices(head) {
            let tail = idx + head.len();
            if tail < text.len() && text[tail..].chars().next().is_some_and(char::is_alphabetic) {
                return true;
            }
        }
    }
    false
}

/// Result of running a sequence of raw ASCII keystrokes through Lotus's Telex `KeyProcessor`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LotusSimulation {
    pub raw_keys: String,
    /// Exact string in `LotusState::oldPreBuffer_` right before `Space` / word-break punctuation.
    pub preedit_before_break: String,
    /// Exact word Lotus commits at `Space` / word-break punctuation (without the trailing boundary char).
    pub committed_at_break: String,
    /// Unicode scalar character count of `preedit_before_break` on screen before `Space`.
    pub screen_chars: usize,
    /// `true` if `preedit_before_break` still held Vietnamese runes and `mustFallbackToEnglish()`
    /// triggered a uinput replacement burst ON the `Space` keystroke itself.
    pub needs_uinput_restore_on_break: bool,
    /// `true` if `committed_at_break` is in `vietnamese.cm.dict`.
    pub is_valid_vietnamese: bool,
    /// `true` if `committed_at_break` contains at least one Vietnamese diacritic/tone rune.
    pub is_composed_vietnamese: bool,
}

/// Lotus `W2U` (`w -> ư`) behavior configured in `~/.config/fcitx5/conf/lotus.conf`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum W2uMode {
    Disabled,
    #[default]
    NonStart,
    Everywhere,
}

/// Effective `fcitx5-lotus` engine options mirrored by `LotusSimEngine`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LotusOptions {
    pub auto_non_vn_restore: bool,
    pub dd_free_style: bool,
    pub spell_check_with_dicts: bool,
    pub modern_style: bool,
    pub free_marking: bool,
    pub w2u: W2uMode,
    pub bracket_transform: bool,
}

impl Default for LotusOptions {
    fn default() -> Self {
        Self {
            auto_non_vn_restore: true,
            dd_free_style: true,
            spell_check_with_dicts: true,
            modern_style: true,
            free_marking: true,
            w2u: W2uMode::NonStart,
            bracket_transform: false,
        }
    }
}

impl LotusOptions {
    /// Parses `fcitx5-lotus`'s INI config (`~/.config/fcitx5/conf/lotus.conf`), falling back to
    /// upstream `fcitx5-lotus` 6.x defaults for any omitted or commented-out keys.
    pub fn from_fcitx5_conf_str(raw: &str) -> Self {
        let mut opts = Self::default();
        for line in raw.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with('[') {
                continue;
            }
            let Some((k, v)) = trimmed.split_once('=') else {
                continue;
            };
            let key = k.trim();
            let val = v.trim().trim_matches('"');
            let parse_bool = |s: &str| -> Option<bool> {
                match s.to_ascii_lowercase().as_str() {
                    "true" | "1" | "yes" | "on" => Some(true),
                    "false" | "0" | "no" | "off" => Some(false),
                    _ => None,
                }
            };
            match key {
                "AutoNonVnRestore" => {
                    if let Some(b) = parse_bool(val) {
                        opts.auto_non_vn_restore = b;
                    }
                }
                "DdFreeStyle" => {
                    if let Some(b) = parse_bool(val) {
                        opts.dd_free_style = b;
                    }
                }
                "SpellCheck" => {
                    if let Some(b) = parse_bool(val) {
                        opts.spell_check_with_dicts = b;
                    }
                }
                "ModernStyle" => {
                    if let Some(b) = parse_bool(val) {
                        opts.modern_style = b;
                    }
                }
                "FreeMarking" => {
                    if let Some(b) = parse_bool(val) {
                        opts.free_marking = b;
                    }
                }
                "W2U" => {
                    opts.w2u = match val.to_ascii_lowercase().as_str() {
                        "disabled" | "false" | "off" | "none" => W2uMode::Disabled,
                        "everywhere" | "always" | "true" => W2uMode::Everywhere,
                        _ => W2uMode::NonStart,
                    };
                }
                "BracketTransform" => {
                    opts.bracket_transform = !matches!(
                        val.to_ascii_lowercase().as_str(),
                        "disabled" | "false" | "off" | "none"
                    );
                }
                _ => {}
            }
        }
        opts
    }
}

struct LotusSimEngine<'a> {
    composition: Vec<Transformation>,
    next_id: usize,
    opts: LotusOptions,
    dict: &'a [Box<str>],
}

impl<'a> LotusSimEngine<'a> {
    fn with_options(opts: LotusOptions, dict: &'a [Box<str>]) -> Self {
        Self {
            composition: Vec::new(),
            next_id: 1,
            opts,
            dict,
        }
    }

    #[inline]
    fn std_style(&self) -> bool {
        !self.opts.modern_style
    }

    fn alloc_id(&mut self) -> usize {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    fn make_appending(&mut self, key: char, is_upper_case: bool) -> Transformation {
        Transformation {
            id: self.alloc_id(),
            target: None,
            rule: Rule {
                key,
                effect_type: EffectType::Appending,
                effect_on: key,
                result: key,
            },
            is_upper_case,
        }
    }

    fn generate_undo_transformations(&mut self, rules: &[Rule]) -> Vec<Transformation> {
        let mut out = Vec::new();
        let cur_str = flatten(&self.composition, FlattenMode::VIETNAMESE_TONELESS_LOWER);
        let std_style = self.std_style();
        let free_marking = self.opts.free_marking;

        for &rule in rules {
            match rule.effect_type {
                EffectType::ToneTransformation(tone) => {
                    let target_opt = if free_marking {
                        if has_valid_tone(&self.composition, tone) {
                            find_tone_target_id(&self.composition, std_style)
                        } else {
                            None
                        }
                    } else {
                        find_last_appending_vowel_id(&self.composition)
                    };
                    let Some(target_id) = target_opt else {
                        continue;
                    };
                    out.push(Transformation {
                        id: self.alloc_id(),
                        target: Some(target_id),
                        rule: Rule {
                            key: '\0',
                            effect_type: EffectType::ToneTransformation(Tone::None),
                            effect_on: '\0',
                            result: '\0',
                        },
                        is_upper_case: false,
                    });
                }
                EffectType::MarkTransformation(_) => {
                    let rev_snapshot = self.composition.clone();
                    for trans in rev_snapshot.iter().rev() {
                        if trans.rule.result != rule.effect_on {
                            continue;
                        }
                        let root_id = find_root_target(&self.composition, trans.id);
                        let undo = Transformation {
                            id: self.alloc_id(),
                            target: Some(root_id),
                            rule: Rule {
                                key: '\0',
                                effect_type: EffectType::MarkTransformation(Mark::None),
                                effect_on: '\0',
                                result: '\0',
                            },
                            is_upper_case: false,
                        };
                        let mut with_eff = self.composition.clone();
                        with_eff.push(undo.clone());
                        if cur_str == flatten(&with_eff, FlattenMode::VIETNAMESE_TONELESS_LOWER) {
                            continue;
                        }
                        out.push(undo);
                    }
                }
                EffectType::Appending => {}
            }
        }
        out
    }

    fn refresh_last_tone_target(&mut self, comp: &[Transformation]) -> Vec<Transformation> {
        if !is_valid_composition(comp, false) {
            return Vec::new();
        }
        let (_, vo, _) = extract_cvc_trans(comp);
        if vo.is_empty() {
            return Vec::new();
        }
        let Some(last_tone) = comp
            .iter()
            .rev()
            .find(|t| {
                matches!(t.rule.effect_type, EffectType::ToneTransformation(_))
                    && t.target.is_some()
            })
            .cloned()
        else {
            return Vec::new();
        };
        let new_target = find_tone_target_id(comp, self.std_style());
        if last_tone.target != new_target {
            let mut out = Vec::with_capacity(2);
            out.push(Transformation {
                id: self.alloc_id(),
                target: last_tone.target,
                rule: Rule {
                    key: '\0',
                    effect_type: EffectType::ToneTransformation(Tone::None),
                    effect_on: '\0',
                    result: '\0',
                },
                is_upper_case: false,
            });
            let mut override_rule = last_tone.rule;
            override_rule.key = '\0';
            out.push(Transformation {
                id: self.alloc_id(),
                target: new_target,
                rule: override_rule,
                is_upper_case: false,
            });
            return out;
        }
        Vec::new()
    }

    fn apply_uow_shortcut(&mut self, comp: &[Transformation]) -> Option<Transformation> {
        if self.opts.w2u == W2uMode::Disabled {
            return None;
        }
        let str_toneless = flatten(comp, FlattenMode::VIETNAMESE_TONELESS_LOWER);
        if matches_uoh_tail(&str_toneless) {
            let rules = applicable_rules('w');
            if let Some((target_id, mut rule)) = find_target(
                comp,
                &rules,
                self.std_style(),
                self.opts.free_marking,
                self.next_id,
            ) {
                rule.key = '\0';
                return Some(Transformation {
                    id: self.alloc_id(),
                    target: Some(target_id),
                    rule,
                    is_upper_case: false,
                });
            }
        }
        None
    }

    fn process_key(&mut self, ch: char, english_mode: bool) {
        let lower_key = ch.to_ascii_lowercase();
        let is_upper_case = ch.is_uppercase();
        let is_bracket_key = self.opts.bracket_transform && matches!(ch, '[' | ']' | '{' | '}');

        if english_mode || (!lower_key.is_ascii_alphabetic() && !is_bracket_key) {
            let app = self.make_appending(lower_key, is_upper_case);
            self.composition.push(app);
            return;
        }

        if is_bracket_key {
            if let Some(last) = self.composition.last() {
                let open_stroke = matches!(ch, '[' | '{');
                let close_stroke = matches!(ch, ']' | '}');
                let open_match = open_stroke
                    && matches!(last.rule.key, '[' | '{')
                    && matches!(last.rule.result, 'ơ' | 'Ơ');
                let close_match = close_stroke
                    && matches!(last.rule.key, ']' | '}')
                    && matches!(last.rule.result, 'ư' | 'Ư');
                if open_match || close_match {
                    let last_id = last.id;
                    let undo = Transformation {
                        id: self.alloc_id(),
                        target: Some(last_id),
                        rule: Rule {
                            key: '\0',
                            effect_type: EffectType::MarkTransformation(Mark::Raw),
                            effect_on: '\0',
                            result: '\0',
                        },
                        is_upper_case: false,
                    };
                    self.composition.push(undo);
                    return;
                }
            }
            let (mapped, upper) = match ch {
                '[' => ('ơ', false),
                '{' => ('ơ', true),
                ']' => ('ư', false),
                '}' => ('ư', true),
                _ => unreachable!(),
            };
            let mut app = self.make_appending(ch, upper);
            app.rule.result = mapped;
            app.rule.effect_on = mapped;
            let mut refresh_src = self.composition.clone();
            refresh_src.push(app.clone());
            let refreshed = self.refresh_last_tone_target(&refresh_src);
            self.composition.push(app);
            self.composition.extend(refreshed);
            return;
        }

        let rules = if self.opts.w2u == W2uMode::Disabled && lower_key == 'w' {
            // Even with W2U disabled, `w` still acts as Horn mark transformation in Telex (`ow -> ơ`, `uw -> ư`, `aw -> ă`)!
            applicable_rules(lower_key)
        } else {
            applicable_rules(lower_key)
        };
        let mut transformations: Vec<Transformation> = Vec::new();
        let std_style = self.std_style();
        let free_marking = self.opts.free_marking;

        // Double typing `w + w -> w` when `w` produced `ư`.
        if let Some(last) = self.composition.last() {
            if last.rule.effect_type == EffectType::Appending
                && last.rule.key == lower_key
                && last.rule.key != last.rule.result
            {
                let last_id = last.id;
                transformations.push(Transformation {
                    id: self.alloc_id(),
                    target: Some(last_id),
                    rule: Rule {
                        key: '\0',
                        effect_type: EffectType::MarkTransformation(Mark::Raw),
                        effect_on: '\0',
                        result: '\0',
                    },
                    is_upper_case: false,
                });
            }
        }

        if transformations.is_empty() {
            if let Some((target_id, rule)) = find_target(
                &self.composition,
                &rules,
                std_style,
                free_marking,
                self.next_id,
            ) {
                let trans = Transformation {
                    id: self.alloc_id(),
                    target: Some(target_id),
                    rule,
                    is_upper_case,
                };
                transformations.push(trans);
                if matches!(rule.effect_type, EffectType::MarkTransformation(_)) {
                    let mut new_comp = self.composition.clone();
                    new_comp.extend(transformations.iter().cloned());
                    if !is_valid_composition(&new_comp, true) {
                        if let Some((v_target, mut v_rule)) =
                            find_target(&new_comp, &rules, std_style, free_marking, self.next_id)
                        {
                            v_rule.key = '\0';
                            transformations.push(Transformation {
                                id: self.alloc_id(),
                                target: Some(v_target),
                                rule: v_rule,
                                is_upper_case: false,
                            });
                        }
                    }
                }
            } else {
                let cur_toneless =
                    flatten(&self.composition, FlattenMode::VIETNAMESE_TONELESS_LOWER);
                let mut handled_uho = false;
                if matches_uh_o(&cur_toneless) {
                    let (_, vo, _) = extract_cvc_trans(&self.composition);
                    let vowels: Vec<&Transformation> = vo
                        .iter()
                        .filter(|t| t.rule.effect_type == EffectType::Appending)
                        .collect();
                    if let Some(v0) = vowels.first() {
                        let v0_id = v0.id;
                        let clear_eff = Transformation {
                            id: self.alloc_id(),
                            target: Some(v0_id),
                            rule: Rule {
                                key: '\0',
                                effect_type: EffectType::MarkTransformation(Mark::None),
                                effect_on: '\0',
                                result: '\0',
                            },
                            is_upper_case: false,
                        };
                        let mut with_eff = self.composition.clone();
                        with_eff.push(clear_eff.clone());
                        if let Some((new_target, new_rule)) =
                            find_target(&with_eff, &rules, std_style, free_marking, self.next_id)
                        {
                            if new_target != v0_id {
                                transformations.push(clear_eff);
                                transformations.push(Transformation {
                                    id: self.alloc_id(),
                                    target: Some(new_target),
                                    rule: new_rule,
                                    is_upper_case,
                                });
                                handled_uho = true;
                            }
                        }
                    }
                }

                if !handled_uho {
                    let undos = self.generate_undo_transformations(&rules);
                    if !undos.is_empty() {
                        transformations.extend(undos);
                        let app = self.make_appending(lower_key, is_upper_case);
                        transformations.push(app);
                    }
                }
            }
        }

        if transformations.is_empty() {
            let mut app = self.make_appending(lower_key, is_upper_case);
            let can_apply_w2u = match self.opts.w2u {
                W2uMode::Disabled => false,
                W2uMode::NonStart => !self.composition.is_empty(),
                W2uMode::Everywhere => true,
            };
            if can_apply_w2u && lower_key == 'w' {
                app.rule.result = 'ư';
                app.rule.effect_on = 'ư';
            }
            transformations.push(app);
        }

        let mut new_comp = self.composition.clone();
        new_comp.extend(transformations.iter().cloned());
        if let Some(virt) = self.apply_uow_shortcut(&new_comp) {
            transformations.push(virt);
        }

        let mut refresh_src = self.composition.clone();
        refresh_src.extend(transformations.iter().cloned());
        let refreshed = self.refresh_last_tone_target(&refresh_src);
        transformations.extend(refreshed);

        self.composition.extend(transformations);
    }

    fn should_fallback_to_english(&self, check_vn_rune: bool) -> bool {
        if !self.opts.auto_non_vn_restore {
            return false;
        }
        let vn_seq = flatten(&self.composition, FlattenMode::VIETNAMESE_LOWER);
        if vn_seq.is_empty() {
            return false;
        }
        if self.opts.dd_free_style
            && !has_any_vietnamese_vowel(&vn_seq)
            && (vn_seq.ends_with('d') || vn_seq.contains('đ'))
        {
            return false;
        }
        if check_vn_rune && !has_any_vietnamese_rune(&vn_seq) {
            return false;
        }
        if self.opts.spell_check_with_dicts && has_dict_prefix_in(self.dict, &vn_seq) {
            return false;
        }
        !is_valid_composition(&self.composition, false)
    }

    fn must_fallback_to_english(&self) -> bool {
        if !self.opts.auto_non_vn_restore {
            return false;
        }
        let vn_seq = flatten(&self.composition, FlattenMode::VIETNAMESE_LOWER);
        if vn_seq.is_empty() {
            return false;
        }
        // `key_processor.cpp:488`: `dd` is preserved even in non-dictionary sequences when `ddFreeStyle` is on!
        if self.opts.dd_free_style && vn_seq.contains('đ') {
            return false;
        }
        if self.opts.spell_check_with_dicts {
            return !is_dict_word_in(self.dict, &vn_seq);
        }
        !is_valid_composition(&self.composition, true)
    }

    fn preedit_string(&self) -> String {
        if self.should_fallback_to_english(true) {
            flatten(&self.composition, FlattenMode::ENGLISH)
        } else {
            flatten(&self.composition, FlattenMode::VIETNAMESE)
        }
    }
}

/// Simulates typing `raw_keys` in `fcitx5-lotus` using default Lotus options.
pub fn simulate_lotus_word(raw_keys: &str) -> LotusSimulation {
    simulate_lotus_word_with_options(raw_keys, LotusOptions::default())
}

/// Simulates typing `raw_keys` in `fcitx5-lotus` under explicit `LotusOptions`.
pub fn simulate_lotus_word_with_options(raw_keys: &str, opts: LotusOptions) -> LotusSimulation {
    let dict = resolve_default_vi_syllables();
    simulate_lotus_word_with_dict(raw_keys, opts, &dict)
}

/// Simulates typing `raw_keys` in `fcitx5-lotus` against an explicit syllable authority slice.
pub fn simulate_lotus_word_with_dict(
    raw_keys: &str,
    opts: LotusOptions,
    dict: &[Box<str>],
) -> LotusSimulation {
    let mut eng = LotusSimEngine::with_options(opts, dict);
    for ch in raw_keys.chars() {
        let english_mode = eng.should_fallback_to_english(false);
        eng.process_key(ch, english_mode);
    }

    let preedit_before_break = eng.preedit_string();
    let screen_chars = preedit_before_break.chars().count();

    let (committed_at_break, needs_uinput_restore_on_break) =
        if has_any_vietnamese_rune(&preedit_before_break) && eng.must_fallback_to_english() {
            let restored = flatten(&eng.composition, FlattenMode::ENGLISH);
            let diff = restored != preedit_before_break;
            (restored, diff)
        } else {
            (preedit_before_break.clone(), false)
        };

    let committed_lower = committed_at_break.to_lowercase();
    let is_valid_vietnamese = is_dict_word_in(dict, &committed_lower);
    let is_composed_vietnamese = has_any_vietnamese_rune(&committed_at_break);

    LotusSimulation {
        raw_keys: raw_keys.to_string(),
        preedit_before_break,
        committed_at_break,
        screen_chars,
        needs_uinput_restore_on_break,
        is_valid_vietnamese,
        is_composed_vietnamese,
    }
}

/// Rescued Vietnamese Telex word produced by `rescue_telex_typo`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TelexRescue {
    /// Final accented Vietnamese word with user casing preserved (e.g. `"tiếng"`, `"Tiếng"`).
    pub replacement: String,
    /// Repaired raw ASCII Telex sequence (e.g. `"tieengs"`).
    pub repaired_keys: String,
    /// Exact number of ASCII characters on screen before the word-break key (`7` for `"tieeengs"`, `8` for `"ttieengs"`).
    pub screen_chars: usize,
}

fn apply_vietnamese_casing(raw_typed: &str, lower_vi: &str) -> String {
    let mut chars = raw_typed.chars();
    let Some(first) = chars.next() else {
        return lower_vi.to_string();
    };
    let rest: Vec<char> = chars.collect();

    // ALL CAPS (e.g. "TTIEENGS" -> "TIẾNG"):
    if first.is_uppercase() && !rest.is_empty() && rest.iter().all(|c| !c.is_lowercase()) {
        return lower_vi.to_uppercase();
    }

    // Title Case or Initial Double-Cap Shift Slip ("Ttieengs" or "TTieengs" -> "Tiếng"):
    if first.is_uppercase() {
        let mut vi_chars = lower_vi.chars();
        let Some(vi_first) = vi_chars.next() else {
            return String::new();
        };
        let mut out = String::with_capacity(lower_vi.len() + 4);
        for uc in vi_first.to_uppercase() {
            out.push(uc);
        }
        out.extend(vi_chars);
        return out;
    }

    lower_vi.to_string()
}

fn has_vietnamese_mark(word: &str) -> bool {
    word.chars().any(|c| {
        let lower = c.to_lowercase().next().unwrap_or(c);
        let toneless = add_tone_to_char(lower, Tone::None);
        matches!(toneless, 'ă' | 'â' | 'đ' | 'ê' | 'ô' | 'ơ' | 'ư')
    })
}

fn has_vietnamese_tone(word: &str) -> bool {
    word.chars().any(|c| {
        let lower = c.to_lowercase().next().unwrap_or(c);
        find_tone_from_char(lower) != Tone::None
    })
}

fn has_coda_or_diphthong(word: &str) -> bool {
    let lower_chars: Vec<char> = word.chars().flat_map(|c| c.to_lowercase()).collect();
    let vowel_count = lower_chars.iter().filter(|&&c| is_vowel(c)).count();
    let ends_with_consonant = lower_chars.last().is_some_and(|&c| !is_vowel(c));
    vowel_count >= 2 || ends_with_consonant
}

fn try_candidate_repair(
    repaired_lower: &str,
    raw_typed: &str,
    screen_chars: usize,
    opts: LotusOptions,
    dict: &[Box<str>],
) -> Option<TelexRescue> {
    if repaired_lower.len() < 2 {
        return None;
    }
    let sim = simulate_lotus_word_with_dict(repaired_lower, opts, dict);
    // Confidence guard:
    // 1. Must be a complete valid word in `vietnamese.cm.dict`.
    // 2. Must carry at least one Vietnamese diacritic or tone mark (`ă, â, đ, ê, ô, ơ, ư` or tones)
    //    so short English words (`inn -> in`, `see -> se`, `too -> to`) are NEVER converted.
    // 3. If it is a single-vowel open syllable (no coda and only 1 vowel) that does not start with `đ`,
    //    require BOTH a vowel mark (`ă, â, ê, ô, ơ, ư`) AND a tone mark so English Floss-rule words
    //    (`off`, `boss`, `loss`, `less`, `mess`, `pass`, `miss`, `kiss`) and Telex 3-key escapes
    //    (`seee -> see`, `tooo -> too`, `treee -> tree`) are never undone.
    if sim.is_valid_vietnamese && sim.is_composed_vietnamese {
        let committed = &sim.committed_at_break;
        let is_triple_d_prefix = raw_typed.len() >= 4
            && raw_typed.as_bytes()[..3].eq_ignore_ascii_case(b"ddd")
            && (committed.starts_with('đ') || committed.starts_with('Đ'));
        if !has_coda_or_diphthong(committed)
            && !is_triple_d_prefix
            && !(has_vietnamese_mark(committed) && has_vietnamese_tone(committed))
        {
            return None;
        }
        let replacement = apply_vietnamese_casing(raw_typed, committed);
        return Some(TelexRescue {
            replacement,
            repaired_keys: repaired_lower.to_string(),
            screen_chars,
        });
    }
    None
}

fn is_double_mark_key(b: u8) -> bool {
    matches!(b, b'a' | b'e' | b'o' | b'd')
}

fn is_tone_or_horn_key(b: u8) -> bool {
    matches!(b, b's' | b'f' | b'r' | b'x' | b'j' | b'w')
}

/// Telex tone (`s f r x j z`) + mark (`a e o d w`) keys — the only final key the substitution
/// rescue (strategy 5) may rewrite, and the only characters it may be rewritten to.
const TELEX_FUNCTIONAL_KEYS: &[u8] = b"sfrxjzwaeod";

fn is_telex_functional_key(b: u8) -> bool {
    TELEX_FUNCTIONAL_KEYS.contains(&b)
}

fn is_strict_tone_key(b: u8) -> bool {
    matches!(b, b's' | b'f' | b'r' | b'x' | b'j')
}

/// Strategy 5a: rescues restore-class `ie`/`ye` + coda words whose circumflex key (`e`) was
/// omitted (`tienf -> tieenf -> tiền`, `vietj -> vieetj -> việt`, `nguyenx -> nguyeenx -> nguyễn`).
fn try_ie_ye_circumflex_rescue(
    lower: &str,
    raw_keys: &str,
    screen_chars: usize,
    opts: LotusOptions,
    dict: &[Box<str>],
) -> Option<TelexRescue> {
    let bytes = lower.as_bytes();
    let n = bytes.len();
    if n < 4 || !is_strict_tone_key(bytes[n - 1]) || lower.contains("ee") {
        return None;
    }
    let stem = &lower[..n - 1];
    let tone = bytes[n - 1] as char;

    for (pos, pat) in stem.match_indices("ie").chain(stem.match_indices("ye")) {
        if pat == "ye" && pos > 0 && !matches!(stem.as_bytes()[pos - 1], b'u' | b'q') {
            continue;
        }
        let coda = &stem[pos + 2..];
        if !matches!(coda, "c" | "m" | "n" | "ng" | "p" | "t" | "u") {
            continue;
        }
        let cand = format!("{}e{coda}{tone}", &stem[..pos + 2]);
        if let Some(rescued) = try_candidate_repair(&cand, raw_keys, screen_chars, opts, dict) {
            return Some(rescued);
        }
    }
    None
}

/// Strategy 6: rescues restore-class words missing a single Telex mark (`a/e/o/d` double or `w`)
/// only when exactly one distinct dictionary-valid candidate exists (`tuans -> tuấn`, while
/// ambiguous `nuocs -> nuốc|nước` and `nguoif -> nguồi|người` stay untouched).
fn try_unique_single_mark_rescue(
    lower: &str,
    raw_keys: &str,
    screen_chars: usize,
    opts: LotusOptions,
    dict: &[Box<str>],
) -> Option<TelexRescue> {
    let bytes = lower.as_bytes();
    let n = bytes.len();
    if n < 3 || !is_strict_tone_key(bytes[n - 1]) {
        return None;
    }
    let stem = &lower[..n - 1];
    let sb = stem.as_bytes();
    let tone = bytes[n - 1] as char;

    let mut cands = Vec::new();
    if sb[0] == b'd' && sb.get(1) != Some(&b'd') {
        cands.push(format!("d{stem}{tone}"));
    }
    for i in 0..sb.len() {
        if matches!(sb[i], b'a' | b'e' | b'o')
            && (i == 0 || sb[i - 1] != sb[i])
            && sb.get(i + 1) != Some(&sb[i])
        {
            cands.push(format!(
                "{}{}{}{}",
                &stem[..=i],
                sb[i] as char,
                &stem[i + 1..],
                tone
            ));
        }
        if matches!(sb[i], b'a' | b'o' | b'u') && sb.get(i + 1) != Some(&b'w') {
            cands.push(format!("{}w{}{}", &stem[..=i], &stem[i + 1..], tone));
        }
    }

    let mut found: Option<TelexRescue> = None;
    for cand in cands {
        if let Some(rescued) = try_candidate_repair(&cand, raw_keys, screen_chars, opts, dict) {
            match &found {
                Some(prev) if !prev.replacement.eq_ignore_ascii_case(&rescued.replacement) => {
                    return None;
                }
                None => found = Some(rescued),
                Some(_) => {}
            }
        }
    }
    found
}

/// Collapses all 3x mark keys (`eee -> ee`, `ddd -> dd`, `ooo -> oo`, `aaa -> aa`) and
/// 2x non-mark keys (`tt -> t`, `ss -> s`, `ww -> w`, `nn -> n`, etc.) in a single pass.
fn collapse_telex_chatter(lower: &str) -> String {
    let bytes = lower.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());

    for &b in bytes {
        let len = out.len();
        if is_double_mark_key(b) {
            // Allow up to 2 consecutive (`aa`, `ee`, `oo`, `dd`), collapse 3+ -> 2:
            if len >= 2 && out[len - 1] == b && out[len - 2] == b {
                continue;
            }
        } else {
            // For all other keys (consonants, tone keys `s,f,r,x,j`, horn `w`, vowels `i,u,y`),
            // collapse 2+ consecutive identical keys -> 1:
            if len >= 1 && out[len - 1] == b {
                continue;
            }
        }
        out.push(b);
    }

    String::from_utf8(out).unwrap_or_default()
}

/// Attempts to rescue a broken Vietnamese Telex word (`ttieengs`, `tieeengs`, `tieengss`, `hnuxng`, `dddi`)
/// under default Lotus options.
pub fn rescue_telex_typo(raw_keys: &str) -> Option<TelexRescue> {
    rescue_telex_typo_with_options(raw_keys, LotusOptions::default())
}

/// Attempts to rescue a broken Vietnamese Telex word under explicit `LotusOptions`.
pub fn rescue_telex_typo_with_options(raw_keys: &str, opts: LotusOptions) -> Option<TelexRescue> {
    let dict = resolve_default_vi_syllables();
    rescue_telex_typo_with_dict(raw_keys, opts, &dict)
}

/// Attempts to rescue a broken Vietnamese Telex word under explicit `LotusOptions` and syllable authority.
pub fn rescue_telex_typo_with_dict(
    raw_keys: &str,
    opts: LotusOptions,
    dict: &[Box<str>],
) -> Option<TelexRescue> {
    if raw_keys.len() < 2 || raw_keys.len() > 16 || !raw_keys.is_ascii() {
        return None;
    }
    if !raw_keys.chars().all(|c| c.is_ascii_alphabetic()) {
        return None;
    }

    let orig_sim = simulate_lotus_word_with_dict(raw_keys, opts, dict);
    // If Lotus already composed this into a valid Vietnamese word, no rescue is needed.
    if orig_sim.is_valid_vietnamese {
        return None;
    }

    let screen_chars = orig_sim.screen_chars;
    let lower = raw_keys.to_ascii_lowercase();
    let bytes = lower.as_bytes();
    let n = bytes.len();

    // 1. Single-character deletion of a duplicate/triple Telex key:
    //    - 3x -> 2x mark key (`tieeengs -> tieengs`, `dddi -> ddi`, `khooong -> khoong`)
    //    - 2x -> 1x tone/horn key (`tieengss -> tieengs`, `vieetjj -> vieetj`, `nuwwocs -> nuwocs`)
    //    - 2x -> 1x duplicate consonant/vowel (`ttieengs -> tieengs`, `tieenngs -> tieengs`, `khhoong -> khoong`)
    for i in 1..n {
        if bytes[i] == bytes[i - 1] {
            let mut cand = String::with_capacity(n - 1);
            cand.push_str(&lower[..i]);
            cand.push_str(&lower[i + 1..]);
            if let Some(rescued) = try_candidate_repair(&cand, raw_keys, screen_chars, opts, dict) {
                return Some(rescued);
            }
        }
    }

    // 2. Non-adjacent duplicate tone/mark undo key (e.g. `tiesngs -> tiesng -> tiếng`, `tieengse -> tieengs -> tiếng`):
    for i in 1..n {
        let b = bytes[i];
        if (is_tone_or_horn_key(b) || is_double_mark_key(b)) && bytes[..i].contains(&b) {
            let mut cand = String::with_capacity(n - 1);
            cand.push_str(&lower[..i]);
            cand.push_str(&lower[i + 1..]);
            if let Some(rescued) = try_candidate_repair(&cand, raw_keys, screen_chars, opts, dict) {
                return Some(rescued);
            }
        }
    }

    // 3. Multi-chatter collapse (`ttieeengs -> tieengs`, `ttieengss -> tieengs`):
    let collapsed = collapse_telex_chatter(&lower);
    if collapsed != lower {
        if let Some(rescued) = try_candidate_repair(&collapsed, raw_keys, screen_chars, opts, dict) {
            return Some(rescued);
        }
    }

    // 4. Adjacent transposition (e.g. `hnuxng -> nhuxng -> nhũng`, `hnuwngx -> nhuwngx -> những`,
    //    `rtoong -> troong -> trông`, `hkoong -> khoong -> không`, `tieegns -> tieengs -> tiếng`):
    if n >= 4 {
        for i in 0..(n - 1) {
            if bytes[i] == bytes[i + 1] {
                continue;
            }
            let mut swapped = bytes.to_vec();
            swapped.swap(i, i + 1);
            if let Ok(cand) = std::str::from_utf8(&swapped) {
                if let Some(rescued) =
                    try_candidate_repair(cand, raw_keys, screen_chars, opts, dict)
                {
                    return Some(rescued);
                }
            }
        }

        // 4b. Split coda transposition across a Telex tone/horn or mark key (e.g. `hoagfn -> hoanfg -> hoàng`,
        //     `tieegsn -> tieensg -> tiếng`, `thahsn -> thanhs -> thánh`, `lihjc -> licjh -> lịch`):
        //     when rolling a 2-key coda (`ng`, `nh`, `ch`) together with a tone/mark key, the Telex key
        //     can land between the reversed coda consonants (`g-f-n`), which a single adjacent swap
        //     cannot untangle (`g <-> f` gives `hoafgn`; `f <-> n` gives `hoagnf`).
        for i in 1..(n - 2) {
            let is_reversed_coda = matches!(
                (bytes[i], bytes[i + 2]),
                (b'g', b'n') | (b'h', b'n') | (b'h', b'c')
            );
            let mid = bytes[i + 1];
            let is_telex_mid = is_tone_or_horn_key(mid)
                || (is_double_mark_key(mid) && bytes[..i + 1].contains(&mid));
            if !is_reversed_coda || !is_telex_mid {
                continue;
            }
            let mut swapped = bytes.to_vec();
            swapped.swap(i, i + 2);
            if let Ok(cand) = std::str::from_utf8(&swapped) {
                if let Some(rescued) =
                    try_candidate_repair(cand, raw_keys, screen_chars, opts, dict)
                {
                    return Some(rescued);
                }
            }
        }
    }

    let has_midword_vi_rune =
        orig_sim.needs_uinput_restore_on_break || orig_sim.is_composed_vietnamese;

    // 5a. Omitted circumflex on restore-class (or `ddFreeStyle`) `ie`/`ye` + coda words
    //     (`tienf -> tiền`, `vietj -> việt`, `nguyenx -> nguyễn`, `ddiems -> điếm`).
    //     Runs BEFORE Strategy 5 so `tienf` is never mis-rescued to `tiẻn`.
    if has_midword_vi_rune {
        if let Some(rescued) =
            try_ie_ye_circumflex_rescue(&lower, raw_keys, screen_chars, opts, dict)
        {
            return Some(rescued);
        }
    }

    // 5. Final tone/mark key substitution (`duongx -> duongw -> dương`, `duongj -> duongw -> dương`):
    //    only when Lotus itself composed runes mid-word and `mustFallbackToEnglish()` restored the
    //    raw keys (`needs_uinput_restore_on_break`) — a strong prior that the user WAS typing
    //    Vietnamese but hit one wrong FINAL tone/mark key. Only the last key participates, and only
    //    when it is itself a Telex tone/mark key: substituting mid-word positions lets a phantom
    //    tone stack onto an existing vowel (`duongx -> dufngx -> dũng` via grave-then-tilde
    //    override) and "rescue" nonsense sequences into dict words, and rewriting plain consonants
    //    would change the word instead of fixing a slip. The same `try_candidate_repair`
    //    confidence guards apply (valid dict syllable + composed runes + coda/diphthong or
    //    mark+tone). Pure English words never compose runes mid-word, so they never enter this
    //    strategy.
    if orig_sim.needs_uinput_restore_on_break {
        if let Some(&last) = bytes.last() {
            if is_telex_functional_key(last) {
                for &sub in TELEX_FUNCTIONAL_KEYS {
                    if sub == last {
                        continue;
                    }
                    let mut cand = bytes.to_vec();
                    cand[n - 1] = sub;
                    if let Ok(cand_str) = std::str::from_utf8(&cand) {
                        if let Some(rescued) =
                            try_candidate_repair(cand_str, raw_keys, screen_chars, opts, dict)
                        {
                            return Some(rescued);
                        }
                    }
                }
            }
        }
    }

    // 6. Unambiguous single mark insertion on restore-class (or `ddFreeStyle`) words ending in a
    //    tone key (`buocs -> bước`, `dduocj -> được`, while `nuocs -> nuốc|nước` stays untouched).
    if has_midword_vi_rune {
        if let Some(rescued) =
            try_unique_single_mark_rescue(&lower, raw_keys, screen_chars, opts, dict)
        {
            return Some(rescued);
        }
    }

    // 7. Dropped `n` before trailing `g` (`nhuwg -> nhưng`, `nhuwxg -> những`, `duowg -> dương`,
    //    `hoafg -> hoàng`): require the prefix to compose Vietnamese runes mid-word (either via
    //    `is_composed_vietnamese` or `needs_uinput_restore_on_break` for incomplete diphthong stems
    //    like `duow` -> `dươ`) so English `-g` words (`bag`, `thing`, `config`, `frog`) never enter.
    if n >= 3 && bytes[n - 1] == b'g' && bytes[n - 2] != b'n' {
        let prefix = &lower[..n - 1];
        let prefix_sim = simulate_lotus_word_with_dict(prefix, opts, dict);
        if prefix_sim.is_composed_vietnamese || prefix_sim.needs_uinput_restore_on_break {
            let cand = format!("{prefix}ng");
            if let Some(rescued) = try_candidate_repair(&cand, raw_keys, screen_chars, opts, dict) {
                return Some(rescued);
            }
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dict_lookup_and_prefix() {
        assert!(is_vietnamese_dict_word("tiếng"));
        assert!(is_vietnamese_dict_word("những"));
        assert!(is_vietnamese_dict_word("việt"));
        assert!(is_vietnamese_dict_word("không"));
        assert!(is_vietnamese_dict_word("được"));
        assert!(is_vietnamese_dict_word("người"));
        assert!(is_vietnamese_dict_word("anh"));
        assert!(is_vietnamese_dict_word("em"));
        assert!(is_vietnamese_dict_word("nha"));
        assert!(has_vietnamese_dict_prefix("tiê"));
        assert!(!is_vietnamese_dict_word("ttieengs"));
    }

    #[test]
    fn test_lotus_screen_chars_simulation() {
        // Valid Vietnamese word:
        let sim_ok = simulate_lotus_word("tieengs");
        assert_eq!(sim_ok.committed_at_break, "tiếng");
        assert!(sim_ok.is_valid_vietnamese);
        assert!(sim_ok.is_composed_vietnamese);

        // 2x initial consonant `ttieengs` -> 8 ASCII chars on screen:
        let sim_tt = simulate_lotus_word("ttieengs");
        assert_eq!(sim_tt.preedit_before_break, "ttieengs");
        assert_eq!(sim_tt.screen_chars, 8);
        assert!(!sim_tt.is_valid_vietnamese);

        // 3x vowel mark `tieeengs` -> 3rd `e` undoes `ê -> ee`, leaving 7 ASCII chars ("tieengs") on screen!
        let sim_eee = simulate_lotus_word("tieeengs");
        assert_eq!(sim_eee.preedit_before_break, "tieengs");
        assert_eq!(sim_eee.screen_chars, 7);
        assert!(!sim_eee.is_valid_vietnamese);

        // 2x tone key `tieengss` -> 2nd `s` undoes acute tone leaving `ê`, triggering EnglishMode fallback -> 8 chars!
        let sim_ss = simulate_lotus_word("tieengss");
        assert_eq!(sim_ss.preedit_before_break, "tieengss");
        assert_eq!(sim_ss.screen_chars, 8);
        assert!(!sim_ss.is_valid_vietnamese);

        // 3x `d` (`dddi`) -> 3rd `d` undoes `đ -> dd`, leaving 3 ASCII chars ("ddi") on screen!
        let sim_ddd = simulate_lotus_word("dddi");
        assert_eq!(sim_ddd.preedit_before_break, "ddi");
        assert_eq!(sim_ddd.screen_chars, 3);
        assert!(!sim_ddd.is_valid_vietnamese);
    }

    #[test]
    fn test_rescue_telex_typos() {
        let r1 = rescue_telex_typo("ttieengs").expect("should rescue ttieengs");
        assert_eq!(r1.replacement, "tiếng");
        assert_eq!(r1.screen_chars, 8);

        let r2 = rescue_telex_typo("tieenngs").expect("should rescue tieenngs");
        assert_eq!(r2.replacement, "tiếng");
        assert_eq!(r2.screen_chars, 8);

        let r3 = rescue_telex_typo("tieeengs").expect("should rescue tieeengs");
        assert_eq!(r3.replacement, "tiếng");
        assert_eq!(r3.screen_chars, 7);

        let r4 = rescue_telex_typo("tieengss").expect("should rescue tieengss");
        assert_eq!(r4.replacement, "tiếng");
        assert_eq!(r4.screen_chars, 8);

        let r5 = rescue_telex_typo("hnuxng").expect("should rescue hnuxng");
        assert_eq!(r5.replacement, "nhũng");
        assert_eq!(r5.screen_chars, 6);

        let r5b = rescue_telex_typo("hnuwngx").expect("should rescue hnuwngx");
        assert_eq!(r5b.replacement, "những");
        assert_eq!(r5b.screen_chars, 7);

        let r5c = rescue_telex_typo("hnwxng").expect("should rescue hnwxng");
        assert_eq!(r5c.replacement, "những");
        assert_eq!(r5c.screen_chars, 6);

        let r6 = rescue_telex_typo("dddi").expect("should rescue dddi");
        assert_eq!(r6.replacement, "đi");
        assert_eq!(r6.screen_chars, 3);

        let r7 = rescue_telex_typo("khhoong").expect("should rescue khhoong");
        assert_eq!(r7.replacement, "không");

        let r8 = rescue_telex_typo("khooong").expect("should rescue khooong");
        assert_eq!(r8.replacement, "không");

        let r9 = rescue_telex_typo("vieetjj").expect("should rescue vieetjj");
        assert_eq!(r9.replacement, "việt");

        let r10 = rescue_telex_typo("tieegns").expect("should rescue tieegns");
        assert_eq!(r10.replacement, "tiếng");

        let r11 = rescue_telex_typo("hoagnf").expect("should rescue hoagnf");
        assert_eq!(r11.replacement, "hoàng");
        assert_eq!(r11.screen_chars, 6);

        let r12 = rescue_telex_typo("hoafgn").expect("should rescue hoafgn");
        assert_eq!(r12.replacement, "hoàng");
        assert_eq!(r12.screen_chars, 6);

        let r13 = rescue_telex_typo("hoagfn").expect("should rescue hoagfn");
        assert_eq!(r13.replacement, "hoàng");
        assert_eq!(r13.screen_chars, 6);
        assert_eq!(rescue_telex_typo("Hoagfn").unwrap().replacement, "Hoàng");

        let r14 = rescue_telex_typo("tieegsn").expect("should rescue tieegsn");
        assert_eq!(r14.replacement, "tiếng");

        let r15 = rescue_telex_typo("lihjc").expect("should rescue lihjc");
        assert_eq!(r15.replacement, "lịch");

        let r16 = rescue_telex_typo("thahsn").expect("should rescue thahsn");
        assert_eq!(r16.replacement, "thánh");

        // Casing preservation:
        assert_eq!(rescue_telex_typo("Ttieengs").unwrap().replacement, "Tiếng");
        assert_eq!(rescue_telex_typo("TTieengs").unwrap().replacement, "Tiếng");
        assert_eq!(rescue_telex_typo("TTIEENGS").unwrap().replacement, "TIẾNG");
    }

    #[test]
    fn test_english_words_never_falsely_rescued_to_toneless_vietnamese() {
        for word in [
            "inn", "see", "too", "off", "all", "add", "egg", "odd", "ill", "bee", "fee", "free",
            "tree", "feel", "need", "book", "look", "good", "food", "cool", "tool", "issue",
            "class", "glass", "grass", "pass", "less", "mess", "boss", "loss", "full", "pull",
            "dont", "cant", "wont", "teh", "chagnes", "github",
        ] {
            assert_eq!(
                rescue_telex_typo(word),
                None,
                "English word {word:?} should not be falsely converted to Vietnamese"
            );
        }
    }

    #[test]
    fn test_rescue_single_key_substitution_on_uinput_restore() {
        // `duongx`: Lotus composes runic `duõng` mid-word, `mustFallbackToEnglish()` restores the
        // raw keys at the break (`needs_uinput_restore_on_break`). The user hit ONE wrong tone key
        // (`x` instead of `w`); substituting within Telex-functional keys reaches `duongw -> dương`.
        let r1 = rescue_telex_typo("duongx").expect("should rescue duongx -> dương");
        assert_eq!(r1.replacement, "dương");
        assert_eq!(r1.screen_chars, 5);

        // Casing preservation for the substitution class:
        assert_eq!(rescue_telex_typo("Duongx").unwrap().replacement, "Dương");

        // Restore-class words whose only fix is a wrong tone/mark key stay rescueable:
        assert_eq!(rescue_telex_typo("duongj").unwrap().replacement, "dương");
    }

    #[test]
    fn test_no_substitution_rescue_outside_restore_class() {
        // Words Lotus never composed mid-word (no runes on screen, `needs_uinput_restore_on_break`
        // == false) must never enter the substitution strategy — the user may be typing English
        // or slang deliberately:
        assert_eq!(rescue_telex_typo("was"), None);
        assert_eq!(rescue_telex_typo("helo"), None);
        assert_eq!(rescue_telex_typo("ax"), None);
        // Already-valid dict words never rescue:
        assert_eq!(rescue_telex_typo("chàng"), None);
        assert_eq!(rescue_telex_typo("changf"), None);
        // Non-functional consonant positions are never rewritten (`duongj` rescues only via its
        // `j` tone key, never by rewriting the `g`), and no substitution may reach a dict word
        // from a non-restore token:
        assert_eq!(rescue_telex_typo("heloj"), None);
    }

    #[test]
    fn test_rescue_omitted_ie_ye_circumflex_and_unique_mark_without_bare_ascii_fp() {
        // Strategy 5a: `tienf` previously hit Strategy 5 and mis-rescued to `tiẻn`; must rescue
        // to `tiền` (along with other omitted `ie`/`ye`+coda words) with casing preserved:
        assert_eq!(rescue_telex_typo("tienf").unwrap().replacement, "tiền");
        assert_eq!(rescue_telex_typo("Tienf").unwrap().replacement, "Tiền");
        assert_eq!(rescue_telex_typo("TIENF").unwrap().replacement, "TIỀN");
        assert_eq!(rescue_telex_typo("tiens").unwrap().replacement, "tiến");
        assert_eq!(rescue_telex_typo("tienj").unwrap().replacement, "tiện");
        assert_eq!(rescue_telex_typo("tienx").unwrap().replacement, "tiễn");
        assert_eq!(rescue_telex_typo("vietj").unwrap().replacement, "việt");
        assert_eq!(rescue_telex_typo("Vietj").unwrap().replacement, "Việt");
        assert_eq!(rescue_telex_typo("viets").unwrap().replacement, "viết");
        assert_eq!(rescue_telex_typo("chuyenf").unwrap().replacement, "chuyền");
        assert_eq!(rescue_telex_typo("nguyenx").unwrap().replacement, "nguyễn");
        assert_eq!(rescue_telex_typo("ddiems").unwrap().replacement, "điếm");

        // Strategy 6: unambiguous 1-mark checked-coda & `ddFreeStyle` words rescue cleanly:
        assert_eq!(rescue_telex_typo("buocs").unwrap().replacement, "bước");
        assert_eq!(rescue_telex_typo("buocj").unwrap().replacement, "buộc");
        assert_eq!(rescue_telex_typo("dduocj").unwrap().replacement, "được");
        assert_eq!(rescue_telex_typo("cuops").unwrap().replacement, "cướp");

        // Ambiguous 1-mark checked-coda words stay untouched (neither is level-tone):
        for ambig in ["nuocs", "thuocs", "duocj", "luots", "truotj"] {
            assert_eq!(
                rescue_telex_typo(ambig),
                None,
                "ambiguous 1-mark word {ambig:?} must stay untouched"
            );
        }

        // Bare toneless ASCII (names/unaccented words) and invalid `duonxg` stay untouched:
        for bare in [
            "duong", "Duong", "tien", "Tien", "nguyen", "Nguyen", "tuan", "moi", "duonxg",
        ] {
            assert_eq!(
                rescue_telex_typo(bare),
                None,
                "bare/non-restore word {bare:?} must stay untouched"
            );
        }
    }

    #[test]
    fn test_rescue_dropped_n_before_trailing_g() {
        let r1 = rescue_telex_typo("nhuwg").expect("should rescue nhuwg -> nhưng");
        assert_eq!(r1.replacement, "nhưng");
        assert_eq!(r1.screen_chars, 5);
        assert_eq!(rescue_telex_typo("Nhuwg").unwrap().replacement, "Nhưng");

        let r2 = rescue_telex_typo("nhuwxg").expect("should rescue nhuwxg -> những");
        assert_eq!(r2.replacement, "những");
        assert_eq!(r2.screen_chars, 6);

        assert_eq!(rescue_telex_typo("duowg").unwrap().replacement, "dương");
        assert_eq!(rescue_telex_typo("hoafg").unwrap().replacement, "hoàng");
        assert_eq!(rescue_telex_typo("khoog").unwrap().replacement, "không");

        // Plain English `-g` words must never rescue:
        for eng in ["bag", "thing", "config", "frog", "dug", "blog"] {
            assert_eq!(
                rescue_telex_typo(eng),
                None,
                "English word {eng:?} must not rescue as dropped -ng"
            );
        }
    }

    #[test]
    fn test_is_impossible_standalone_vi_syllable() {
        for impossible in ["ắ", "ằ", "ẳ", "ẵ", "ặ", "ấ", "ầ", "ẩ", "ẫ", "ậ", "chấ", "đẵ"] {
            assert!(
                is_impossible_standalone_vi_syllable(impossible),
                "{impossible:?} should be impossible standalone"
            );
        }
        for allowed in ["ă", "â", "đ", "kpă", "đi", "mã", "tét", "thí", "chấn", "đẵn"] {
            assert!(
                !is_impossible_standalone_vi_syllable(allowed),
                "{allowed:?} must NOT be treated as impossible standalone"
            );
        }
    }
}
