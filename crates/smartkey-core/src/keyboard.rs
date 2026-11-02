/// Physical ANSI QWERTY key coordinates `(row, col_x2)` for fast motor-slip distance calculation.
/// Using integer half-column coordinates (`col * 2`) models staggered physical switch offsets
/// without floating-point trig on the hot path.
#[inline]
fn qwerty_pos(c: u8) -> Option<(i8, i8)> {
    let lower = c.to_ascii_lowercase();
    match lower {
        // Row 0: Q W E R T Y U I O P
        b'q' => Some((0, 0)),
        b'w' => Some((0, 2)),
        b'e' => Some((0, 4)),
        b'r' => Some((0, 6)),
        b't' => Some((0, 8)),
        b'y' => Some((0, 10)),
        b'u' => Some((0, 12)),
        b'i' => Some((0, 14)),
        b'o' => Some((0, 16)),
        b'p' => Some((0, 18)),
        // Row 1: A S D F G H J K L ; ' (staggered right by ~0.5 key = +1 in x2 units)
        b'a' => Some((2, 1)),
        b's' => Some((2, 3)),
        b'd' => Some((2, 5)),
        b'f' => Some((2, 7)),
        b'g' => Some((2, 9)),
        b'h' => Some((2, 11)),
        b'j' => Some((2, 13)),
        b'k' => Some((2, 15)),
        b'l' => Some((2, 17)),
        b';' => Some((2, 19)),
        b'\'' => Some((2, 21)),
        // Row 2: Z X C V B N M (staggered right by +1 key = +2 in x2 units)
        b'z' => Some((4, 2)),
        b'x' => Some((4, 4)),
        b'c' => Some((4, 6)),
        b'v' => Some((4, 8)),
        b'b' => Some((4, 10)),
        b'n' => Some((4, 12)),
        b'm' => Some((4, 14)),
        _ => None,
    }
}

/// Returns true if two ASCII letters are physical neighbors on ANSI QWERTY.
#[inline]
pub fn are_keys_adjacent(a: u8, b: u8) -> bool {
    if a.eq_ignore_ascii_case(&b) {
        return true;
    }
    match (qwerty_pos(a), qwerty_pos(b)) {
        (Some((r1, c1)), Some((r2, c2))) => {
            let dr = (r1 - r2).abs();
            let dc = (c1 - c2).abs();
            dr <= 2 && dc <= 2 && (dr + dc) <= 3
        }
        _ => false,
    }
}

#[inline]
fn is_ascii_vowel(b: u8) -> bool {
    matches!(b.to_ascii_lowercase(), b'a' | b'e' | b'i' | b'o' | b'u')
}

/// Computes a 26-bit presence bitmask (`POPCNT` ready) for ASCII letters in `s`.
#[inline]
pub fn ascii_letter_mask(s: &str) -> u32 {
    let mut mask = 0u32;
    for b in s.bytes() {
        let lower = b.to_ascii_lowercase();
        if lower.is_ascii_lowercase() {
            mask |= 1u32 << (lower - b'a');
        }
    }
    mask
}

/// Computes character-multiset overlap ratio (`0.0..=1.0`) between two ASCII lowercase words.
#[inline]
pub fn multiset_overlap(a: &str, b: &str) -> f32 {
    let mut counts_a = [0u8; 26];
    let mut counts_b = [0u8; 26];
    let mut len_a = 0usize;
    let mut len_b = 0usize;
    for byte in a.bytes() {
        let l = byte.to_ascii_lowercase();
        if l.is_ascii_lowercase() {
            counts_a[(l - b'a') as usize] = counts_a[(l - b'a') as usize].saturating_add(1);
            len_a += 1;
        }
    }
    for byte in b.bytes() {
        let l = byte.to_ascii_lowercase();
        if l.is_ascii_lowercase() {
            counts_b[(l - b'a') as usize] = counts_b[(l - b'a') as usize].saturating_add(1);
            len_b += 1;
        }
    }
    let max_len = len_a.max(len_b);
    if max_len == 0 {
        return 0.0;
    }
    let mut shared = 0usize;
    for i in 0..26 {
        shared += counts_a[i].min(counts_b[i]) as usize;
    }
    shared as f32 / max_len as f32
}

/// Unweighted Damerau–Levenshtein distance (optimal string alignment) over Unicode scalar chars.
pub fn damerau_levenshtein_chars(a: &str, b: &str) -> usize {
    let ca: Vec<char> = a.chars().collect();
    let cb: Vec<char> = b.chars().collect();
    let n = ca.len();
    let m = cb.len();
    if n == 0 {
        return m;
    }
    if m == 0 {
        return n;
    }
    let width = m + 1;
    let mut dp = vec![0usize; (n + 1) * width];
    for i in 0..=n {
        dp[i * width] = i;
    }
    for (j, slot) in dp.iter_mut().enumerate().take(m + 1) {
        *slot = j;
    }
    for i in 1..=n {
        for j in 1..=m {
            let cost = usize::from(ca[i - 1] != cb[j - 1]);
            let mut best = (dp[(i - 1) * width + j] + 1)
                .min(dp[i * width + (j - 1)] + 1)
                .min(dp[(i - 1) * width + (j - 1)] + cost);
            if i > 1 && j > 1 && ca[i - 1] == cb[j - 2] && ca[i - 2] == cb[j - 1] {
                best = best.min(dp[(i - 2) * width + (j - 2)] + 1);
            }
            dp[i * width + j] = best;
        }
    }
    dp[n * width + m]
}

/// Physical 10-finger keyboard weighted edit cost `E_layout(typed, candidate)`.
///
/// Models real desktop motor errors:
/// - Adjacent transposition (`chagnes -> changes`, `woryr -> worry`, `cna -> can`): `0.48`
/// - Key bounce / geminate slip (`thhe -> the`, `nnext -> next`, `suport -> support`): `0.50`
/// - Neighboring-key insertion (`nothiong -> nothing`, `disctionary -> dictionary`): `0.62`
/// - Neighboring-key substitution (`fomr` vs adjacent): `0.68`
/// - Unstressed vowel substitution in long words (`len >= 6`, `definately`): `0.72`
/// - Omitted letter in longer words (`len >= 5`, `implmenet`): `0.82`
/// - Unrelated substitution or initial-letter deletion on short words (`khum -> hum`): `1.45+`
pub fn weighted_keyboard_cost(typed: &str, cand: &str) -> f32 {
    let tb = typed.as_bytes();
    let cb = cand.as_bytes();
    if !tb.iter().all(|b| b.is_ascii()) || !cb.iter().all(|b| b.is_ascii()) {
        return damerau_levenshtein_chars(typed, cand) as f32;
    }

    let n = tb.len();
    let m = cb.len();
    if n == 0 {
        return m as f32 * 1.2;
    }
    if m == 0 {
        return n as f32 * 1.2;
    }

    let width = m + 1;
    let mut dp = vec![f32::INFINITY; (n + 1) * width];
    dp[0] = 0.0;

    for i in 1..=n {
        // Deleting extra char in `typed` (insertion error by user)
        let extra = tb[i - 1];
        let is_bounce = (i > 1 && extra == tb[i - 2]) || (i < n && extra == tb[i]);
        let del_cost = if is_bounce {
            0.46
        } else if i == 1 && n <= 4 {
            // Protect short slang like `khum -> hum` from deleting the initial letter!
            1.55
        } else {
            0.88
        };
        dp[i * width] = dp[(i - 1) * width] + del_cost;
    }
    for j in 1..=m {
        dp[j] = dp[j - 1] + if j == 1 { 1.35 } else { 0.95 };
    }

    let long_word = m >= 6;

    for i in 1..=n {
        let tc = tb[i - 1].to_ascii_lowercase();
        for j in 1..=m {
            let cc = cb[j - 1].to_ascii_lowercase();

            // 1. Match or Substitution
            let sub_cost = if tc == cc {
                0.0
            } else if are_keys_adjacent(tc, cc) {
                if i == 1 && m <= 4 {
                    0.95
                } else {
                    0.68
                }
            } else if long_word && is_ascii_vowel(tc) && is_ascii_vowel(cc) {
                0.72
            } else if i == 1 && m <= 4 {
                1.55
            } else {
                1.38
            };
            let mut best = dp[(i - 1) * width + (j - 1)] + sub_cost;

            // 2. User typed an EXTRA character (`tb[i - 1]` deleted to match `cand`)
            let is_bounce = (i > 1 && tc == tb[i - 2].to_ascii_lowercase()) || (j >= 1 && tc == cc);
            let touches_neighbor = (i > 1 && are_keys_adjacent(tc, tb[i - 2]))
                || (i < n && are_keys_adjacent(tc, tb[i]))
                || are_keys_adjacent(tc, cc);
            let extra_char_cost = if is_bounce {
                0.46
            } else if i == 1 && n <= 4 {
                1.55
            } else if touches_neighbor {
                0.62
            } else {
                0.86
            };
            best = best.min(dp[(i - 1) * width + j] + extra_char_cost);

            // 3. User OMITTED a character (`cb[j - 1]` inserted to reach `cand`)
            let is_geminate = (j > 1 && cc == cb[j - 2].to_ascii_lowercase())
                || (j < m && cc == cb[j].to_ascii_lowercase());
            let omit_cost = if is_geminate && m >= 5 {
                // e.g. `suport -> support`, `occured -> occurred`
                0.52
            } else if j == 1 && m <= 4 {
                1.45
            } else if m >= 5 && is_ascii_vowel(cc) {
                // Omitted vowel in 5+ char words (`ther -> there/their`, `implmenet`)
                0.70
            } else if long_word {
                0.80
            } else {
                0.92
            };
            best = best.min(dp[i * width + (j - 1)] + omit_cost);

            // 4. Adjacent Transposition (`chagnes -> changes`, `woryr -> worry`, `hte -> the`)
            if i > 1
                && j > 1
                && tc == cb[j - 2].to_ascii_lowercase()
                && tb[i - 2].to_ascii_lowercase() == cc
            {
                best = best.min(dp[(i - 2) * width + (j - 2)] + 0.46);
            }

            // 5. 3-character two-hand rollover & gapped transposition (`apdater -> adapter`, `implmenet -> implement`)
            if i > 2 && j > 2 {
                let t0 = tb[i - 3].to_ascii_lowercase();
                let t1 = tb[i - 2].to_ascii_lowercase();
                let t2 = tc;
                let c0 = cb[j - 3].to_ascii_lowercase();
                let c1 = cb[j - 2].to_ascii_lowercase();
                let c2 = cc;

                // Gapped swap: `a b c -> c b a`
                if t2 == c0 && t1 == c1 && t0 == c2 {
                    best = best.min(dp[(i - 3) * width + (j - 3)] + 0.72);
                }
                // Rollover rotation: `p d a -> d a p` (`t0 == c2 && t1 == c0 && t2 == c1`)
                // or `d a p -> p d a` (`t0 == c1 && t1 == c2 && t2 == c0`)
                if (t0 == c2 && t1 == c0 && t2 == c1) || (t0 == c1 && t1 == c2 && t2 == c0) {
                    best = best.min(dp[(i - 3) * width + (j - 3)] + 0.58);
                }
            }

            dp[i * width + j] = best;
        }
    }

    dp[n * width + m]
}
