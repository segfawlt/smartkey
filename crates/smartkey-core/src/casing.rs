/// Mixed-case technical, scientific, and brand tokens that must NEVER be altered.
pub const CASING_EXCLUSIONS: &[&str] = &[
    "iOS",
    "macOS",
    "tvOS",
    "watchOS",
    "pH",
    "mRNA",
    "tRNA",
    "rRNA",
    "dsRNA",
    "ssRNA",
    "cDNA",
    "eBay",
    "iPad",
    "iPhone",
    "iMac",
    "iPod",
    "iCloud",
    "dBi",
    "dBm",
    "dBA",
    "kHz",
    "MHz",
    "GHz",
    "THz",
    "kB",
    "mB",
    "gB",
    "tB",
    "KiB",
    "MiB",
    "GiB",
    "TiB",
    "mA",
    "mV",
    "mW",
    "kW",
    "MW",
    "GitHub",
    "GitLab",
    "YouTube",
    "JavaScript",
    "TypeScript",
    "PostgreSQL",
    "MySQL",
    "FastAPI",
    "GraphQL",
    "Neovim",
    "HeliBoard",
    "OpenBoard",
    "FlorisBoard",
    "SwiftKey",
    "Gboard",
    "SmartKey",
    "Wayland",
    "XWayland",
    "OpenGL",
    "WebGL",
    "WebGPU",
    "SQLite",
    "PyTorch",
    "TensorFlow",
];

/// Two-letter `xY` units/notations excluded from 2-char Caps Lock inversion (`iS -> Is`).
const TWO_CHAR_CAPSLOCK_EXCLUSIONS: &[&str] = &[
    "pH", "dB", "kB", "mB", "gB", "tB", "mA", "mV", "mW", "kW", "kV", "nM", "uM", "mM", "iO",
];

/// Valid English leading-apostrophe colloquial contractions that keep their leading `'`.
pub const LEADING_APOSTROPHE_CONTRACTIONS: &[&str] = &[
    "'em", "'cause", "'til", "'90s", "'80s", "'70s", "'60s", "'round", "'n'", "'tis", "'twas",
];

/// Abbreviations whose trailing period does NOT end a sentence (suppresses `auto_capitalize_sentence`).
pub const ABBREVIATIONS: &[&str] = &[
    "e.g", "eg", "i.e", "ie", "etc", "vs", "mr", "mrs", "ms", "dr", "prof", "inc", "ltd", "jr",
    "sr", "st", "ave", "approx", "dept", "fig", "vol", "al", "jan", "feb", "mar", "apr", "jun",
    "jul", "aug", "sep", "sept", "oct", "nov", "dec",
];

/// Returns true if `punct` requires holding `Shift` on standard ANSI/ISO keyboards.
#[inline]
pub fn is_shifted_punctuation(punct: char) -> bool {
    matches!(
        punct,
        ':' | '!'
            | '?'
            | '"'
            | ')'
            | '}'
            | ']'
            | '~'
            | '+'
            | '*'
            | '&'
            | '%'
            | '$'
            | '@'
            | '^'
            | '<'
            | '>'
            | '|'
            | '_'
    )
}

/// Returns true if `word` (without trailing `.`) is a common abbreviation that suppresses
/// automatic next-sentence capitalization.
pub fn is_abbreviation(word: &str) -> bool {
    let trimmed = word.trim_end_matches('.');
    if trimmed.is_empty() {
        return false;
    }
    let lower = trimmed.to_lowercase();
    ABBREVIATIONS.contains(&lower.as_str())
}

/// Result of pre-processing a raw token for outer quotes, bypass guards, and Shift/CapsLock slips.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenAnalysis {
    /// Code, URL, number, acronym, or `camelCase` identifier: skip completely (`do_not_learn = true`).
    Bypass,
    /// Valid colloquial leading-apostrophe token (`'90s`, `'em`, `'cause`).
    LeadingApostropheValid,
    /// Expressive trailing character stretch (`soooo`, `trờiiii`): keep verbatim, do not learn.
    StretchedWord,
    /// Prose candidate ready for Layer 1 evaluation.
    Evaluate {
        leading_quotes: String,
        core: String,
        trailing_quotes: String,
        /// True if `core` already had a physical Shift/CapsLock slip corrected relative to input.
        shift_slip_fixed: bool,
        /// True if input started with intentional TitleCase (`Apple`, `Chagnes`).
        user_title_case: bool,
    },
}

/// Strips outer single quotes (`'apple'` -> `("'", "apple", "'")`) while preserving valid
/// leading-apostrophe contractions (`'em`, `'cause`, `'til`, `'90s`) and trailing possessives/clitics.
pub fn split_outer_single_quotes(raw: &str) -> (&str, &str, &str) {
    let lower = raw.to_lowercase();
    if LEADING_APOSTROPHE_CONTRACTIONS.contains(&lower.as_str()) {
        return ("", raw, "");
    }

    let mut start = 0usize;
    let mut end = raw.len();

    // Strip leading single quotes (`'` or `‘`)
    while start < end {
        let ch = raw[start..].chars().next().unwrap();
        if ch == '\'' || ch == '‘' {
            let rem_lower = raw[start..end].to_lowercase();
            if LEADING_APOSTROPHE_CONTRACTIONS.contains(&rem_lower.as_str()) {
                break;
            }
            start += ch.len_utf8();
        } else {
            break;
        }
    }

    // Strip trailing single quotes (`'` or `’`)
    // Note: if there was a leading quote (`'apple'`), strip matching trailing quote cleanly;
    // also strip lone trailing quote (`apple'`) unless part of `'n'`.
    while end > start {
        let ch = raw[start..end].chars().next_back().unwrap();
        if ch == '\'' || ch == '’' {
            end -= ch.len_utf8();
        } else {
            break;
        }
    }

    if start >= end {
        return ("", raw, "");
    }

    (&raw[..start], &raw[start..end], &raw[end..])
}

/// Returns true if `core` is a pluralized acronym like `IDs`, `PRs`, `UIs`, `OSes`, `URLs`, `VMs`.
fn is_plural_acronym(chars: &[char]) -> bool {
    let n = chars.len();
    if n < 3 {
        return false;
    }
    // Check `...S` -> `...[A-Z]{2,}s`
    if chars[n - 1] == 's' && chars[..n - 1].iter().all(|c| c.is_uppercase()) {
        return true;
    }
    // Check `...es` -> `...[A-Z]{2,}es` (e.g. `OSes`)
    if n >= 4
        && chars[n - 2] == 'e'
        && chars[n - 1] == 's'
        && chars[..n - 2].iter().all(|c| c.is_uppercase())
    {
        return true;
    }
    false
}

/// 5-step expressive stretch stemmer (`soooo`, `trờiiii`, `yessss`, `noooo`).
/// Collapses a trailing run of `>= 3` identical alphabetic characters down to 1 or 2 characters
/// to check if the stem is a valid word (or non-ASCII Unicode word like Vietnamese `trời`).
pub fn is_expressive_stretch(core: &str, is_valid_word: impl Fn(&str) -> bool) -> bool {
    let chars: Vec<char> = core.chars().collect();
    let n = chars.len();
    if n < 4 {
        return false;
    }
    let last_lower = chars[n - 1].to_lowercase().next().unwrap_or(chars[n - 1]);
    if !last_lower.is_alphabetic() {
        return false;
    }

    let mut run_len = 1usize;
    while run_len < n {
        let prev = chars[n - 1 - run_len];
        let prev_lower = prev.to_lowercase().next().unwrap_or(prev);
        if prev_lower == last_lower {
            run_len += 1;
        } else {
            break;
        }
    }

    if run_len < 3 {
        return false;
    }

    let prefix_len = n - run_len;
    if prefix_len == 0 {
        return false;
    }

    let stem_1: String = chars[..prefix_len + 1]
        .iter()
        .collect::<String>()
        .to_lowercase();
    let stem_2: String = chars[..prefix_len + 2]
        .iter()
        .collect::<String>()
        .to_lowercase();

    // Non-ASCII alphabetic words (e.g. Vietnamese `trờiiii -> trời`) pass immediately!
    if stem_1.chars().any(|c| !c.is_ascii() && c.is_alphabetic()) && prefix_len + 1 >= 2 {
        return true;
    }

    is_valid_word(&stem_1) || is_valid_word(&stem_2)
}

/// Analyzes a token for code/URL bypasses, outer quotes, stretched tails, and physical Shift/CapsLock slips.
pub fn analyze_token(
    raw: &str,
    boundary_punct: Option<char>,
    at_sentence_start: bool,
    fix_shift_slips: bool,
    is_valid_word: impl Fn(&str) -> bool,
) -> TokenAnalysis {
    if raw.is_empty() {
        return TokenAnalysis::Bypass;
    }

    // Prefix symbol guards (`@mention`, `#channel`, `/boost`, `~/path`, `./script`)
    let first_raw = raw.chars().next().unwrap();
    if matches!(first_raw, '@' | '#' | '/' | '~' | '.' | '\\' | '`') {
        return TokenAnalysis::Bypass;
    }

    let (leading_q, inner, trailing_q) = split_outer_single_quotes(raw);
    let inner_lower = inner.to_lowercase();
    if LEADING_APOSTROPHE_CONTRACTIONS.contains(&inner_lower.as_str()) {
        return TokenAnalysis::LeadingApostropheValid;
    }

    if CASING_EXCLUSIONS.contains(&inner) {
        return TokenAnalysis::Bypass;
    }

    // Check internal characters: allow alphabetic, `'`, `’`, and provisional `;` (for `don;t`).
    // Any digit, underscore, dot, slash, hyphen, or code operator bypasses cleanly.
    let mut alpha_chars: Vec<char> = Vec::new();
    let mut semicolon_count = 0usize;
    for ch in inner.chars() {
        if ch.is_alphabetic() {
            alpha_chars.push(ch);
        } else if ch == '\'' || ch == '’' {
            continue;
        } else if ch == ';' {
            semicolon_count += 1;
            if semicolon_count > 1 {
                return TokenAnalysis::Bypass;
            }
        } else {
            return TokenAnalysis::Bypass;
        }
    }

    if alpha_chars.is_empty() {
        return TokenAnalysis::Bypass;
    }

    // Pluralized acronyms (`IDs`, `PRs`, `OSes`, `URLs`, `VMs`) -> Bypass
    if is_plural_acronym(&alpha_chars) {
        return TokenAnalysis::Bypass;
    }

    // `ALL_CAPS` (`len >= 2`: `API`, `TODO`, `NVME`, `NEVER`) -> Bypass
    if alpha_chars.len() >= 2 && alpha_chars.iter().all(|c| c.is_uppercase()) {
        return TokenAnalysis::Bypass;
    }

    // Expressive trailing stretch (`soooo`, `trờiiii`) -> Keep verbatim, never learn
    if semicolon_count == 0 && is_expressive_stretch(inner, &is_valid_word) {
        return TokenAnalysis::StretchedWord;
    }

    // Inspect casing pattern across characters of `inner`
    let inner_chars: Vec<char> = inner.chars().collect();
    let alpha_len = alpha_chars.len();

    // Case 1: Pure lowercase (`apple`, `github`, `chagnes`, `don;t`)
    if alpha_chars.iter().all(|c| c.is_lowercase()) {
        return TokenAnalysis::Evaluate {
            leading_quotes: leading_q.to_string(),
            core: inner.to_string(),
            trailing_quotes: trailing_q.to_string(),
            shift_slip_fixed: false,
            user_title_case: false,
        };
    }

    // Case 2: Clean TitleCase (1st char uppercase, all remaining alphabetic chars lowercase: `Apple`, `Chagnes`, `I`)
    if inner_chars[0].is_alphabetic()
        && inner_chars[0].is_uppercase()
        && alpha_chars[1..].iter().all(|c| c.is_lowercase())
    {
        return TokenAnalysis::Evaluate {
            leading_quotes: leading_q.to_string(),
            core: inner.to_string(),
            trailing_quotes: trailing_q.to_string(),
            shift_slip_fixed: false,
            user_title_case: alpha_len >= 2,
        };
    }

    if !fix_shift_slips {
        return TokenAnalysis::Bypass;
    }

    // Case 3: Held-Shift (`THat's`, `THis`, `APple`, `CHagnes`):
    // First 2 chars are alphabetic uppercase, remaining `>= 1` alphabetic chars are all lowercase.
    if alpha_len >= 3
        && inner_chars.len() >= 3
        && inner_chars[0].is_alphabetic()
        && inner_chars[0].is_uppercase()
        && inner_chars[1].is_alphabetic()
        && inner_chars[1].is_uppercase()
        && alpha_chars[2..].iter().all(|c| c.is_lowercase())
    {
        let mut normalized = String::with_capacity(inner.len());
        normalized.push(inner_chars[0]);
        for c in inner_chars[1].to_lowercase() {
            normalized.push(c);
        }
        for &ch in &inner_chars[2..] {
            normalized.push(ch);
        }
        return TokenAnalysis::Evaluate {
            leading_quotes: leading_q.to_string(),
            core: normalized,
            trailing_quotes: trailing_q.to_string(),
            shift_slip_fixed: true,
            user_title_case: true,
        };
    }

    // Case 4: Early-Shift before shifted punctuation (`situatioN:`, `hellO!`, `whatS?`):
    // Boundary is shifted punctuation (`: ! ? " ) ...`), final char is uppercase, preceding chars are lowercase (or TitleCase).
    if alpha_len >= 3
        && boundary_punct.is_some_and(is_shifted_punctuation)
        && inner_chars
            .last()
            .is_some_and(|c| c.is_alphabetic() && c.is_uppercase())
    {
        let prefix_alphas = &alpha_chars[..alpha_len - 1];
        let is_lower_body = prefix_alphas.iter().all(|c| c.is_lowercase());
        let is_title_body =
            prefix_alphas[0].is_uppercase() && prefix_alphas[1..].iter().all(|c| c.is_lowercase());
        if is_lower_body || is_title_body {
            let mut normalized = String::with_capacity(inner.len());
            for &ch in &inner_chars[..inner_chars.len() - 1] {
                normalized.push(ch);
            }
            for c in inner_chars[inner_chars.len() - 1].to_lowercase() {
                normalized.push(c);
            }
            return TokenAnalysis::Evaluate {
                leading_quotes: leading_q.to_string(),
                core: normalized,
                trailing_quotes: trailing_q.to_string(),
                shift_slip_fixed: true,
                user_title_case: is_title_body,
            };
        }
    }

    // Case 5: Accidental Caps Lock Inversion (`tHIS -> This`, `hELLO -> Hello`, `iS -> Is`):
    // 1st alphabetic char is lowercase, all remaining alphabetic chars (`>= 1`) are uppercase.
    if alpha_len >= 2
        && inner_chars[0].is_alphabetic()
        && inner_chars[0].is_lowercase()
        && alpha_chars[1..].iter().all(|c| c.is_uppercase())
        && !(alpha_len == 2 && TWO_CHAR_CAPSLOCK_EXCLUSIONS.contains(&inner))
    {
        let lower_check = inner.to_lowercase();
        if is_valid_word(&lower_check) {
            let mut title = String::with_capacity(inner.len());
            for c in inner_chars[0].to_uppercase() {
                title.push(c);
            }
            for &ch in &inner_chars[1..] {
                for lc in ch.to_lowercase() {
                    title.push(lc);
                }
            }
            return TokenAnalysis::Evaluate {
                leading_quotes: leading_q.to_string(),
                core: title,
                trailing_quotes: trailing_q.to_string(),
                shift_slip_fixed: true,
                user_title_case: true,
            };
        }
    }

    // Case 6: Sentence-Start Late-Shift (`tHis -> This`, `tHe -> The`, `wHat -> What`):
    // Strictly gated to `at_sentence_start == true` so mid-sentence `mMap`, `iNode`, `vTable`, `oAuth` stay untouched!
    if at_sentence_start
        && alpha_len >= 3
        && inner_chars.len() >= 3
        && inner_chars[0].is_alphabetic()
        && inner_chars[0].is_lowercase()
        && inner_chars[1].is_alphabetic()
        && inner_chars[1].is_uppercase()
        && alpha_chars[2..].iter().all(|c| c.is_lowercase())
    {
        let lower_check = inner.to_lowercase();
        if is_valid_word(&lower_check) {
            let mut title = String::with_capacity(inner.len());
            for c in inner_chars[0].to_uppercase() {
                title.push(c);
            }
            for c in inner_chars[1].to_lowercase() {
                title.push(c);
            }
            for &ch in &inner_chars[2..] {
                title.push(ch);
            }
            return TokenAnalysis::Evaluate {
                leading_quotes: leading_q.to_string(),
                core: title,
                trailing_quotes: trailing_q.to_string(),
                shift_slip_fixed: true,
                user_title_case: true,
            };
        }
    }

    // Any other mixed-case token (`useState`, `smartKey`, `mMap`, `apPle`) is treated as an identifier -> Bypass!
    TokenAnalysis::Bypass
}

/// Applies user casing (`TitleCase` or `auto_capitalize_sentence`) onto a lowercase replacement,
/// while preserving explicit contraction capitalization (`I'm`, `I've`, `I`).
pub fn project_case(target: &str, capitalize_initial: bool) -> String {
    if !capitalize_initial || target.is_empty() {
        return target.to_string();
    }
    let mut chars = target.chars();
    let Some(first) = chars.next() else {
        return String::new();
    };
    if first.is_uppercase() {
        return target.to_string();
    }
    let mut out = String::with_capacity(target.len() + 2);
    for up in first.to_uppercase() {
        out.push(up);
    }
    out.push_str(chars.as_str());
    out
}
