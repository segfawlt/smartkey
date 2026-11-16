use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use fst::MapBuilder;
use serde::Deserialize;
use toml_edit::DocumentMut;

use crate::keyboard::ascii_letter_mask;
use crate::lexicon::{
    lint_proper_nouns, serialize_skdict_v3, LanguageRules, PackHeader, PackLexicon, PackTier,
    ReplacementKind, SkdictV3Payload, SourceManifestEntry, WordMeta, FLAG_AMBIGUOUS_PROPER,
    FLAG_OFFENSIVE, FLAG_SHIELD, PROPER_NOUN_MIN_CASE_GAP,
};

const SHA256_H0: [u32; 8] = [
    0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
];

const SHA256_K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

/// Pure-Rust FIPS 180-4 SHA-256 digest (`32` bytes).
pub fn sha256_bytes(bytes: &[u8]) -> [u8; 32] {
    let mut state = SHA256_H0;

    let bit_len = (bytes.len() as u64).wrapping_mul(8);
    let mut padded = Vec::with_capacity(bytes.len() + 72);
    padded.extend_from_slice(bytes);
    padded.push(0x80);
    while (padded.len() % 64) != 56 {
        padded.push(0);
    }
    padded.extend_from_slice(&bit_len.to_be_bytes());

    for chunk in padded.as_chunks::<64>().0 {
        let mut w = [0u32; 64];
        for (i, &word_bytes) in chunk.as_chunks::<4>().0.iter().enumerate() {
            w[i] = u32::from_be_bytes(word_bytes);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }

        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = state;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let temp1 = h
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(SHA256_K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = s0.wrapping_add(maj);

            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(temp1);
            d = c;
            c = b;
            b = a;
            a = temp1.wrapping_add(temp2);
        }

        state[0] = state[0].wrapping_add(a);
        state[1] = state[1].wrapping_add(b);
        state[2] = state[2].wrapping_add(c);
        state[3] = state[3].wrapping_add(d);
        state[4] = state[4].wrapping_add(e);
        state[5] = state[5].wrapping_add(f);
        state[6] = state[6].wrapping_add(g);
        state[7] = state[7].wrapping_add(h);
    }

    let mut out = [0u8; 32];
    for (i, word) in state.iter().enumerate() {
        out[i * 4..(i + 1) * 4].copy_from_slice(&word.to_be_bytes());
    }
    out
}

/// Pure-Rust FIPS 180-4 SHA-256 lowercase hex digest (`64` hex chars).
pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = sha256_bytes(bytes);
    let mut s = String::with_capacity(64);
    for b in digest {
        use std::fmt::Write as _;
        let _ = write!(&mut s, "{b:02x}");
    }
    s
}

#[derive(Debug, Clone, Deserialize)]
pub struct PackRecipe {
    pub pack: PackMeta,
    pub variants: BTreeMap<String, VariantSpec>,
    #[serde(default)]
    pub sources: BTreeMap<String, SourceSpec>,
    #[serde(default)]
    pub overlays: OverlaysSpec,
    #[serde(default)]
    pub build: BuildSpec,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PackMeta {
    pub code: String,
    #[serde(default)]
    pub name: String,
    pub version: String,
    pub format_version: u32,
    pub tier: String,
    #[serde(default)]
    pub maintainer: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub license: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct VariantSpec {
    pub freq_slot: u8,
    pub default: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SourceSpec {
    pub format: String,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub cache_file: Option<String>,
    #[serde(default)]
    pub decompress: Option<String>,
    #[serde(default)]
    pub sha256: String,
    #[serde(default)]
    pub license: String,
    #[serde(default)]
    pub variants: Vec<String>,
    #[serde(default)]
    pub capture_bigrams: bool,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct OverlaysSpec {
    pub shorthand: Vec<String>,
    pub slang: Vec<String>,
    pub proper_nouns: Vec<String>,
    pub dev_terms: Vec<String>,
    pub rules: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct BuildSpec {
    pub bigram_top_n: usize,
    pub smash_min_freq: u8,
    pub proper_noun_freq: u8,
    pub tech_freq: u8,
    pub syllable_dict: bool,
}

impl Default for BuildSpec {
    fn default() -> Self {
        Self {
            bigram_top_n: 90_000,
            smash_min_freq: 105,
            proper_noun_freq: 160,
            tech_freq: 172,
            syllable_dict: false,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct PackBuildOptions<'a> {
    pub sources_cache_dir: Option<&'a Path>,
    pub fetch: bool,
    pub allow_ambiguous: bool,
    pub skip_sources: &'a [&'a str],
}

#[derive(Debug, Clone)]
pub struct BuiltPack {
    pub header: PackHeader,
    pub bytes: Vec<u8>,
    pub warnings: Vec<String>,
    pub manifest: Vec<SourceManifestEntry>,
    pub word_count: usize,
    pub typo_count: usize,
    pub bigram_count: usize,
    pub smash_count: usize,
    pub proper_noun_count: usize,
    pub shorthand_count: usize,
    pub slang_count: usize,
    pub syllable_count: usize,
}

/// Parses `pack.toml` while preserving the TOML declaration order of `[variants.*]`.
pub fn parse_pack_recipe(toml_str: &str) -> Result<(PackRecipe, Vec<(String, VariantSpec)>), String> {
    let recipe: PackRecipe =
        toml_edit::de::from_str(toml_str).map_err(|e| format!("invalid pack.toml: {e}"))?;
    let doc: DocumentMut = toml_str
        .parse()
        .map_err(|e| format!("invalid pack.toml document: {e}"))?;

    let mut ordered_variants = Vec::new();
    if let Some(table) = doc.get("variants").and_then(|v| v.as_table()) {
        for (k, _) in table.iter() {
            if let Some(spec) = recipe.variants.get(k) {
                ordered_variants.push((k.to_string(), spec.clone()));
            }
        }
    }
    if ordered_variants.len() != recipe.variants.len() {
        for (k, spec) in &recipe.variants {
            if !ordered_variants.iter().any(|(ok, _)| ok == k) {
                ordered_variants.push((k.clone(), spec.clone()));
            }
        }
    }
    Ok((recipe, ordered_variants))
}

fn parse_semver(version: &str) -> Result<[u8; 3], String> {
    let parts: Vec<&str> = version.trim().split('.').collect();
    if parts.len() != 3 {
        return Err(format!(
            "invalid semver {version:?} (expected MAJOR.MINOR.PATCH)"
        ));
    }
    let major = parts[0]
        .parse::<u8>()
        .map_err(|e| format!("invalid major version in {version:?}: {e}"))?;
    let minor = parts[1]
        .parse::<u8>()
        .map_err(|e| format!("invalid minor version in {version:?}: {e}"))?;
    let patch = parts[2]
        .parse::<u8>()
        .map_err(|e| format!("invalid patch version in {version:?}: {e}"))?;
    Ok([major, minor, patch])
}

fn license_has_nc_or_nd(license: &str) -> bool {
    license
        .split(|c: char| !c.is_ascii_alphanumeric())
        .any(|tok| {
            tok.eq_ignore_ascii_case("NC")
                || tok.eq_ignore_ascii_case("ND")
                || tok.eq_ignore_ascii_case("NONCOMMERCIAL")
                || tok.eq_ignore_ascii_case("NODERIVS")
        })
}

fn check_license_warnings(
    src_name: &str,
    license: &str,
    warnings: &mut Vec<String>,
) -> Result<(), String> {
    let trimmed = license.trim();
    if trimmed.is_empty() {
        return Err(format!(
            "source {src_name:?} is missing required `license` field in pack.toml"
        ));
    }
    if trimmed.eq_ignore_ascii_case("UNVERIFIED") {
        let msg = format!(
            "source {src_name:?} has license = \"UNVERIFIED\" — recorded in SOURCE_MANIFEST; review redistribution terms"
        );
        eprintln!("warning: {msg}");
        warnings.push(msg);
    } else if license_has_nc_or_nd(trimmed) {
        let msg = format!(
            "source {src_name:?} has restrictive NC/ND license {trimmed:?} — recorded in SOURCE_MANIFEST"
        );
        eprintln!("warning: {msg}");
        warnings.push(msg);
    }
    Ok(())
}

fn validate_sha256_hex(src_name: &str, hex_str: &str) -> Result<String, String> {
    let clean = hex_str.trim().to_ascii_lowercase();
    if clean.len() != 64 || !clean.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
        return Err(format!(
            "source {src_name:?} has invalid sha256 {hex_str:?} (expected 64 hex chars)"
        ));
    }
    Ok(clean)
}

fn resolve_cache_dir(pack_dir: &Path, opts: &PackBuildOptions<'_>) -> PathBuf {
    if let Some(dir) = opts.sources_cache_dir {
        return dir.to_path_buf();
    }
    if let Some(root) = pack_dir.parent().and_then(|p| p.parent()) {
        return root.join("data/sources");
    }
    PathBuf::from("data/sources")
}

fn resolve_source_path(
    pack_dir: &Path,
    cache_dir: &Path,
    lang: &str,
    src_name: &str,
    spec: &SourceSpec,
) -> Result<(PathBuf, bool), String> {
    if let Some(rel) = spec.path.as_deref().filter(|s| !s.trim().is_empty()) {
        return Ok((pack_dir.join(rel), true));
    }
    if let Some(rel) = spec.cache_file.as_deref().filter(|s| !s.trim().is_empty()) {
        return Ok((cache_dir.join(rel), false));
    }
    if let Some(url) = spec.url.as_deref().filter(|s| !s.trim().is_empty()) {
        let base = url
            .rsplit('/')
            .next()
            .filter(|s| !s.is_empty())
            .unwrap_or(src_name);
        let decompressed_name = if spec.decompress.as_deref() == Some("gzip") {
            base.strip_suffix(".gz").unwrap_or(base)
        } else {
            base
        };
        return Ok((cache_dir.join(lang).join(decompressed_name), false));
    }
    Err(format!(
        "source {src_name:?} must specify `path`, `cache_file`, or `url`"
    ))
}

fn fetch_source_if_needed(
    src_name: &str,
    spec: &SourceSpec,
    dest_path: &Path,
    expected_sha: &str,
) -> Result<(), String> {
    if dest_path.is_file() {
        if let Ok(existing) = fs::read(dest_path) {
            if sha256_hex(&existing) == expected_sha {
                return Ok(());
            }
        }
    }

    let url = spec.url.as_deref().filter(|s| !s.trim().is_empty()).ok_or_else(|| {
        format!("cannot fetch source {src_name:?}: missing `url` in pack.toml")
    })?;
    if let Some(parent) = dest_path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("failed to create cache dir {}: {e}", parent.display()))?;
    }

    let decompress = spec.decompress.as_deref().unwrap_or("none");
    let pid = std::process::id();
    let tmp_dest = dest_path.with_file_name(format!(
        ".{}.tmp.{pid}",
        dest_path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or(src_name)
    ));

    match decompress {
        "none" => {
            println!("Fetching {src_name} from {url}...");
            let status = Command::new("curl")
                .args([
                    "-fsSL",
                    "--retry",
                    "3",
                    "--max-time",
                    "60",
                    url,
                    "-o",
                    tmp_dest.to_str().ok_or("non-UTF-8 temp path")?,
                ])
                .status()
                .map_err(|e| format!("failed to run curl for {src_name:?}: {e}"))?;
            if !status.success() {
                let _ = fs::remove_file(&tmp_dest);
                return Err(format!("curl failed for {src_name:?} ({url}): {status}"));
            }
        }
        "gzip" => {
            let gz_path = dest_path.with_extension(format!(
                "{}gz",
                dest_path
                    .extension()
                    .and_then(|s| s.to_str())
                    .map(|e| format!("{e}."))
                    .unwrap_or_default()
            ));
            let tmp_gz = gz_path.with_file_name(format!(
                ".{}.tmp.{pid}",
                gz_path
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("download.gz")
            ));
            println!("Fetching {src_name} (gzip) from {url}...");
            let status = Command::new("curl")
                .args([
                    "-fsSL",
                    "--retry",
                    "3",
                    "--max-time",
                    "60",
                    url,
                    "-o",
                    tmp_gz.to_str().ok_or("non-UTF-8 gzip temp path")?,
                ])
                .status()
                .map_err(|e| format!("failed to run curl for {src_name:?}: {e}"))?;
            if !status.success() {
                let _ = fs::remove_file(&tmp_gz);
                return Err(format!("curl failed for {src_name:?} ({url}): {status}"));
            }
            fs::rename(&tmp_gz, &gz_path)
                .map_err(|e| format!("failed to save {}: {e}", gz_path.display()))?;

            let output = Command::new("gzip")
                .args(["-dc", gz_path.to_str().ok_or("non-UTF-8 gz path")?])
                .output()
                .map_err(|e| format!("failed to run gzip for {src_name:?}: {e}"))?;
            if !output.status.success() {
                return Err(format!(
                    "gzip -dc failed for {src_name:?}: {}",
                    String::from_utf8_lossy(&output.stderr)
                ));
            }
            let mut file = fs::File::create(&tmp_dest)
                .map_err(|e| format!("failed to create {}: {e}", tmp_dest.display()))?;
            file.write_all(&output.stdout)
                .map_err(|e| format!("failed to write {}: {e}", tmp_dest.display()))?;
        }
        other => {
            return Err(format!(
                "source {src_name:?} has unsupported decompress mode {other:?}"
            ));
        }
    }

    fs::rename(&tmp_dest, dest_path)
        .map_err(|e| format!("failed to commit {}: {e}", dest_path.display()))?;
    Ok(())
}

#[derive(Default, Clone, Copy)]
struct RawWord {
    freq0: u8,
    freq1: u8,
    variant_mask: u8,
    flags: u8,
}

fn add_word_for_targets(
    entry: &mut RawWord,
    freq: u8,
    target_variants: &[(usize, u8)],
) {
    for &(v_idx, slot) in target_variants {
        entry.variant_mask |= 1u8 << v_idx;
        if slot == 0 {
            entry.freq0 = entry.freq0.max(freq);
        } else if slot == 1 {
            entry.freq1 = entry.freq1.max(freq);
        }
    }
}

fn add_word_all_slots(
    entry: &mut RawWord,
    freq: u8,
    all_variants_mask: u8,
    has_slot0: bool,
    has_slot1: bool,
) {
    entry.variant_mask |= all_variants_mask;
    if has_slot0 {
        entry.freq0 = entry.freq0.max(freq);
    }
    if has_slot1 {
        entry.freq1 = entry.freq1.max(freq);
    }
}

fn parse_aosp_combined(
    content: &str,
    target_variants: &[(usize, u8)],
    capture_bigrams: bool,
    words: &mut BTreeMap<String, RawWord>,
    aosp_bigrams: &mut Vec<(String, String, u8)>,
) {
    let mut current_word: Option<String> = None;
    for line in content.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("word=") {
            current_word = None;
            let mut raw_w = "";
            let mut freq: u8 = 0;
            let mut not_a_word = false;
            let mut offensive = false;
            let mut is_abbrev = false;

            for part in rest.split(',') {
                if raw_w.is_empty() && !part.contains('=') {
                    raw_w = part;
                } else if let Some(val) = part.strip_prefix("f=") {
                    freq = val.parse::<u8>().unwrap_or(0);
                } else if part == "not_a_word=true" {
                    not_a_word = true;
                } else if part == "possibly_offensive=true" {
                    offensive = true;
                } else if let Some(fl) = part.strip_prefix("flags=") {
                    if fl.contains("abbreviation") {
                        is_abbrev = true;
                    }
                }
            }

            if raw_w.is_empty() || not_a_word {
                continue;
            }

            let alpha: Vec<char> = raw_w.chars().filter(|c| c.is_alphabetic()).collect();
            if alpha.is_empty() {
                continue;
            }
            if (is_abbrev || alpha.len() >= 2) && alpha.iter().all(|c| c.is_uppercase()) {
                continue;
            }
            if !raw_w
                .chars()
                .all(|c| c.is_alphabetic() || c == '\'' || c == '-')
            {
                continue;
            }
            if raw_w.contains('-') {
                continue;
            }

            let lower = raw_w.to_lowercase();
            let entry = words.entry(lower.clone()).or_default();
            if offensive || freq == 0 {
                entry.flags |= FLAG_OFFENSIVE;
            } else {
                add_word_for_targets(entry, freq, target_variants);
                if capture_bigrams {
                    current_word = Some(lower);
                }
            }
        } else if capture_bigrams {
            if let (Some(prev), Some(rest)) =
                (current_word.as_ref(), trimmed.strip_prefix("bigram="))
            {
                let mut target = "";
                let mut rank: u8 = 3;
                for part in rest.split(',') {
                    if target.is_empty() && !part.contains('=') {
                        target = part;
                    } else if let Some(fval) = part.strip_prefix("f=") {
                        rank = fval.parse::<u8>().unwrap_or(3);
                    }
                }
                if !target.is_empty() {
                    let score = match rank {
                        1 => 210,
                        2 => 185,
                        _ => 160,
                    };
                    aosp_bigrams.push((prev.clone(), target.to_lowercase(), score));
                }
            }
        }
    }
}

fn parse_wordlist(
    content: &str,
    target_variants: &[(usize, u8)],
    words: &mut BTreeMap<String, RawWord>,
) {
    for line in content.lines() {
        let s = line.trim();
        if s.is_empty() || s.starts_with('#') {
            continue;
        }
        let (raw_w, freq) = if let Some((w, f_str)) = s.split_once('\t') {
            let parsed_f = f_str.trim().parse::<u8>().unwrap_or(160);
            (w.trim(), parsed_f)
        } else if let Some((w, f_str)) = s.split_once(' ') {
            if let Ok(parsed_f) = f_str.trim().parse::<u8>() {
                (w.trim(), parsed_f)
            } else {
                (s, 160)
            }
        } else {
            (s, 160)
        };
        if raw_w.is_empty() || freq == 0 {
            continue;
        }
        let alpha: Vec<char> = raw_w.chars().filter(|c| c.is_alphabetic()).collect();
        if alpha.is_empty() {
            continue;
        }
        if alpha.len() >= 2 && alpha.iter().all(|c| c.is_uppercase()) {
            continue;
        }
        if !raw_w.chars().all(|c| c.is_alphabetic() || c == '\'') {
            continue;
        }
        let lower = raw_w.to_lowercase();
        let entry = words.entry(lower).or_default();
        add_word_for_targets(entry, freq, target_variants);
    }
}

fn parse_fcitx_cm_dict(content: &str, syllables: &mut BTreeSet<String>) {
    for line in content.lines() {
        let s = line.trim();
        if s.is_empty() || s.starts_with('#') {
            continue;
        }
        syllables.insert(s.to_string());
    }
}

/// Builds a deterministic `.skdict` v3 binary image from `dicts/<lang>/pack.toml`.
pub fn build_pack_from_dir(pack_dir: &Path, opts: &PackBuildOptions<'_>) -> Result<BuiltPack, String> {
    let pack_toml_path = pack_dir.join("pack.toml");
    let pack_toml_raw = fs::read_to_string(&pack_toml_path)
        .map_err(|e| format!("failed to read {}: {e}", pack_toml_path.display()))?;
    let (recipe, ordered_variants) = parse_pack_recipe(&pack_toml_raw)?;

    if recipe.pack.format_version != 3 {
        return Err(format!(
            "{}: unsupported format_version {} (expected 3)",
            pack_toml_path.display(),
            recipe.pack.format_version
        ));
    }
    if recipe.pack.code.is_empty()
        || recipe.pack.code.len() > 255
        || !recipe
            .pack
            .code
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
    {
        return Err(format!(
            "{}: invalid [pack].code {:?}",
            pack_toml_path.display(),
            recipe.pack.code
        ));
    }
    if recipe.pack.license.trim().is_empty() {
        return Err(format!(
            "{}: missing required [pack].license",
            pack_toml_path.display()
        ));
    }
    let pack_version = parse_semver(&recipe.pack.version)?;
    let tier = match recipe.pack.tier.as_str() {
        "data" => PackTier::Data,
        "ime_bridge" => PackTier::ImeBridge,
        other => {
            return Err(format!(
                "{}: invalid [pack].tier {other:?} (expected \"data\" or \"ime_bridge\")",
                pack_toml_path.display()
            ))
        }
    };

    if !(1..=8).contains(&ordered_variants.len()) {
        return Err(format!(
            "{}: [variants] must declare 1..=8 variants, found {}",
            pack_toml_path.display(),
            ordered_variants.len()
        ));
    }

    let mut variant_names = Vec::with_capacity(ordered_variants.len());
    let mut variant_slots = Vec::with_capacity(ordered_variants.len());
    let mut default_idxs = Vec::new();
    let mut max_slot = 0u8;
    for (idx, (vname, vspec)) in ordered_variants.iter().enumerate() {
        if vname.is_empty() || vname.len() > 255 || !vname.is_ascii() {
            return Err(format!("invalid variant tag {vname:?}"));
        }
        if vspec.freq_slot > 1 {
            return Err(format!(
                "variant {vname:?} has freq_slot {} (supported: 0 or 1)",
                vspec.freq_slot
            ));
        }
        if vspec.default {
            default_idxs.push(idx as u8);
        }
        max_slot = max_slot.max(vspec.freq_slot);
        variant_names.push(vname.clone());
        variant_slots.push(vspec.freq_slot);
    }
    if default_idxs.len() > 1 {
        return Err(format!(
            "{}: multiple variants marked default = true",
            pack_toml_path.display()
        ));
    }
    let default_variant_idx = default_idxs.first().copied().unwrap_or(0);

    let freq_slot_count = (max_slot as usize) + 1;
    let mut freq_slots = vec![0u8; freq_slot_count];
    for (idx, &slot) in variant_slots.iter().enumerate() {
        freq_slots[slot as usize] |= 1u8 << idx;
    }
    let has_slot0 = !freq_slots.is_empty() && freq_slots[0] != 0;
    let has_slot1 = freq_slots.len() > 1 && freq_slots[1] != 0;
    let all_variants_mask: u8 = ((1u16 << variant_names.len()) - 1) as u8;

    let cache_dir = resolve_cache_dir(pack_dir, opts);
    let mut warnings = Vec::new();
    let mut manifest = Vec::new();

    let mut words: BTreeMap<String, RawWord> = BTreeMap::new();
    let mut aosp_bigrams: Vec<(String, String, u8)> = Vec::new();
    let mut aosp_wordlists: Vec<String> = Vec::new();
    let mut syllables: BTreeSet<String> = BTreeSet::new();
    let mut typos_csv_texts: Vec<String> = Vec::new();
    let mut variant_map_sources: Vec<(SourceSpec, String)> = Vec::new();
    let mut bigram_count_texts: Vec<String> = Vec::new();

    let resolve_source_targets = |src_variants: &[String]| -> Result<Vec<(usize, u8)>, String> {
        if src_variants.is_empty() {
            return Ok(variant_slots.iter().copied().enumerate().collect());
        }
        let mut out = Vec::with_capacity(src_variants.len());
        for tag in src_variants {
            let idx = variant_names
                .iter()
                .position(|v| v == tag)
                .ok_or_else(|| format!("source references unknown variant {tag:?}"))?;
            out.push((idx, variant_slots[idx]));
        }
        Ok(out)
    };

    for (src_name, src_spec) in &recipe.sources {
        if opts
            .skip_sources
            .iter()
            .any(|s| *s == src_name || *s == src_spec.format)
        {
            let msg = format!(
                "skipping source {src_name:?} (format={:?}) via build option",
                src_spec.format
            );
            eprintln!("warning: {msg}");
            warnings.push(msg);
            continue;
        }

        check_license_warnings(src_name, &src_spec.license, &mut warnings)?;
        let expected_sha = validate_sha256_hex(src_name, &src_spec.sha256)?;
        let (src_path, is_local_path) =
            resolve_source_path(pack_dir, &cache_dir, &recipe.pack.code, src_name, src_spec)?;

        if opts.fetch && !is_local_path {
            fetch_source_if_needed(src_name, src_spec, &src_path, &expected_sha)?;
        }

        let raw_bytes = fs::read(&src_path).map_err(|e| {
            format!(
                "missing source {src_name:?} at {}: {e}{}",
                src_path.display(),
                if !is_local_path && !opts.fetch {
                    " (run with --fetch to download)"
                } else {
                    ""
                }
            )
        })?;
        let actual_digest = sha256_bytes(&raw_bytes);
        let actual_hex = sha256_hex(&raw_bytes);
        if actual_hex != expected_sha {
            return Err(format!(
                "SHA-256 mismatch for source {src_name:?} ({}): expected {expected_sha}, got {actual_hex}",
                src_path.display()
            ));
        }

        manifest.push(SourceManifestEntry {
            name: src_name.clone(),
            sha256: actual_digest,
            license: src_spec.license.trim().to_string(),
        });

        let text = String::from_utf8(raw_bytes).map_err(|e| {
            format!(
                "source {src_name:?} ({}) is not valid UTF-8: {e}",
                src_path.display()
            )
        })?;

        let target_variants = resolve_source_targets(&src_spec.variants)?;
        match src_spec.format.as_str() {
            "aosp_combined" => {
                parse_aosp_combined(
                    &text,
                    &target_variants,
                    src_spec.capture_bigrams,
                    &mut words,
                    &mut aosp_bigrams,
                );
                aosp_wordlists.push(text);
            }
            "wordlist" => {
                parse_wordlist(&text, &target_variants, &mut words);
            }
            "fcitx_cm_dict" => {
                parse_fcitx_cm_dict(&text, &mut syllables);
            }
            "typos_csv" => {
                typos_csv_texts.push(text);
            }
            "variant_map_json" => {
                variant_map_sources.push((src_spec.clone(), text));
            }
            "bigram_counts" => {
                bigram_count_texts.push(text);
            }
            other => {
                return Err(format!(
                    "source {src_name:?} has unsupported format {other:?}"
                ));
            }
        }
    }

    // Overlays: shorthand
    let mut shorthand_map: BTreeMap<String, String> = BTreeMap::new();
    for rel in &recipe.overlays.shorthand {
        let p = pack_dir.join(rel);
        let raw = fs::read_to_string(&p)
            .map_err(|e| format!("failed to read shorthand overlay {}: {e}", p.display()))?;
        for (line_no, line) in raw.lines().enumerate() {
            let trimmed_eol = line.trim_end_matches(['\r', '\n']);
            let trimmed = trimmed_eol.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            let Some((raw_trig, rest)) = trimmed_eol.split_once('\t') else {
                return Err(format!(
                    "{}:{}: malformed shorthand row (missing TAB delimiter)",
                    p.display(),
                    line_no + 1
                ));
            };
            // Allow optional `<TAB>#notes` 3rd column.
            let raw_exp = rest.split('\t').next().unwrap_or("");
            let trig = raw_trig.trim().to_lowercase();
            let exp = raw_exp.trim();
            if trig.is_empty() || exp.is_empty() || exp.starts_with('#') {
                return Err(format!(
                    "{}:{}: empty shorthand trigger or expansion",
                    p.display(),
                    line_no + 1
                ));
            }
            if let Some(prev_exp) = shorthand_map.insert(trig.clone(), exp.to_string()) {
                return Err(format!(
                    "{}:{}: duplicate shorthand trigger {:?} (previous={:?}, new={:?})",
                    p.display(),
                    line_no + 1,
                    trig,
                    prev_exp,
                    exp
                ));
            }
        }
    }
    let shorthand_pairs: Vec<(String, String)> = shorthand_map.into_iter().collect();
    if tier == PackTier::Data && !recipe.build.syllable_dict {
        for (trig, _) in &shorthand_pairs {
            let entry = words.entry(trig.clone()).or_default();
            entry.flags = (entry.flags & !FLAG_OFFENSIVE) | FLAG_SHIELD;
            entry.variant_mask |= all_variants_mask;
        }
    }

    // Overlays: slang
    let mut slang_words: BTreeSet<String> = BTreeSet::new();
    for rel in &recipe.overlays.slang {
        let p = pack_dir.join(rel);
        let raw = fs::read_to_string(&p)
            .map_err(|e| format!("failed to read slang overlay {}: {e}", p.display()))?;
        for line in raw.lines() {
            let w = line.trim();
            if w.is_empty() || w.starts_with('#') {
                continue;
            }
            let lower = w.to_lowercase();
            slang_words.insert(lower.clone());
            if tier == PackTier::Data && !recipe.build.syllable_dict {
                let entry = words.entry(lower).or_default();
                entry.flags = (entry.flags & !FLAG_OFFENSIVE) | FLAG_SHIELD;
                entry.variant_mask |= all_variants_mask;
            }
        }
    }

    // Overlays: dev_terms
    for rel in &recipe.overlays.dev_terms {
        let p = pack_dir.join(rel);
        let raw = fs::read_to_string(&p)
            .map_err(|e| format!("failed to read dev_terms overlay {}: {e}", p.display()))?;
        for line in raw.lines() {
            let w = line.trim();
            if w.is_empty() || w.starts_with('#') {
                continue;
            }
            let lower = w.to_lowercase();
            let entry = words.entry(lower).or_default();
            entry.flags = (entry.flags & !FLAG_OFFENSIVE) | FLAG_SHIELD;
            add_word_all_slots(
                entry,
                recipe.build.tech_freq,
                all_variants_mask,
                has_slot0,
                has_slot1,
            );
        }
    }

    // Overlays: proper_nouns + homograph lint
    let mut proper_raw_combined = String::new();
    for rel in &recipe.overlays.proper_nouns {
        let p = pack_dir.join(rel);
        let raw = fs::read_to_string(&p)
            .map_err(|e| format!("failed to read proper_nouns overlay {}: {e}", p.display()))?;
        proper_raw_combined.push_str(&raw);
        proper_raw_combined.push('\n');
    }

    let aosp_refs: Vec<&str> = aosp_wordlists.iter().map(String::as_str).collect();
    let lint = if !proper_raw_combined.is_empty() && !aosp_refs.is_empty() {
        lint_proper_nouns(&aosp_refs, &proper_raw_combined)
    } else {
        Default::default()
    };
    for w in &lint.warnings {
        let msg = format!("proper_nouns.tsv: {w}");
        warnings.push(msg);
    }

    let mut ambiguous_lowers: BTreeSet<String> = BTreeSet::new();
    if !lint.errors.is_empty() {
        if opts.allow_ambiguous {
            for err in &lint.errors {
                if let Some(after_quote) = err.strip_prefix('"') {
                    if let Some((lower, _)) = after_quote.split_once('"') {
                        ambiguous_lowers.insert(lower.to_string());
                    }
                }
                let msg = format!("proper_nouns.tsv (ambiguous flagged): {err}");
                eprintln!("warning: {msg}");
                warnings.push(msg);
            }
        } else {
            return Err(format!(
                "proper_nouns.tsv homograph lint failed (gap < {PROPER_NOUN_MIN_CASE_GAP}; pass --allow-ambiguous to mark FLAG_AMBIGUOUS_PROPER):\n  {}",
                lint.errors.join("\n  ")
            ));
        }
    }

    let mut proper_nouns: Vec<(String, String)> = Vec::new();
    for line in proper_raw_combined.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (lower, proper) = if let Some((l, p)) = line.split_once('\t') {
            (l.trim().to_lowercase(), p.trim().to_string())
        } else {
            (line.to_lowercase(), line.to_string())
        };
        if lower.is_empty() || proper.is_empty() || lower == proper {
            continue;
        }
        let entry = words.entry(lower.clone()).or_default();
        entry.flags &= !FLAG_OFFENSIVE;
        if ambiguous_lowers.contains(&lower) {
            entry.flags |= FLAG_AMBIGUOUS_PROPER;
        }
        add_word_all_slots(
            entry,
            recipe.build.proper_noun_freq,
            all_variants_mask,
            has_slot0,
            has_slot1,
        );
        proper_nouns.push((lower, proper));
    }
    proper_nouns.sort_unstable();
    proper_nouns.dedup_by(|a, b| a.0 == b.0);

    // Overlays: rules.toml -> LanguageRules
    let mut language_rules: Option<LanguageRules> = None;
    for rel in &recipe.overlays.rules {
        let p = pack_dir.join(rel);
        let raw = fs::read_to_string(&p)
            .map_err(|e| format!("failed to read rules overlay {}: {e}", p.display()))?;
        let parsed: LanguageRules = toml_edit::de::from_str(&raw)
            .map_err(|e| format!("failed to parse {}: {e}", p.display()))?;
        match &mut language_rules {
            None => language_rules = Some(parsed),
            Some(existing) => {
                existing.capitalize_standalone_i |= parsed.capitalize_standalone_i;
                existing.function_words.extend(parsed.function_words);
                existing.clitics.suffixes.extend(parsed.clitics.suffixes);
                existing.clitics.prefixes.extend(parsed.clitics.prefixes);
                existing
                    .casing
                    .leading_apostrophes
                    .extend(parsed.casing.leading_apostrophes);
                existing
                    .casing
                    .abbreviations
                    .extend(parsed.casing.abbreviations);
                existing.casing.exclusions.extend(parsed.casing.exclusions);
                existing.bigram_boosts.extend(parsed.bigram_boosts);
                existing.replacements.extend(parsed.replacements);
            }
        }
    }

    // Variant rewrites (`variant_map_json`)
    let mut rewrites_by_target: BTreeMap<u8, Vec<(String, String)>> = BTreeMap::new();
    for (src_spec, json_text) in &variant_map_sources {
        let targets = resolve_source_targets(&src_spec.variants)?;
        if targets.len() != 2 {
            return Err(format!(
                "variant_map_json requires exactly 2 variants, found {}",
                targets.len()
            ));
        }
        let (to_idx, to_slot) = targets[0];
        let (from_idx, from_slot) = targets[1];

        let raw_map: BTreeMap<String, String> = serde_json::from_str(json_text)
            .map_err(|e| format!("invalid variant_map_json: {e}"))?;
        let mut forward = Vec::new();
        for (from_raw, to_raw) in raw_map {
            let from_w = from_raw.to_lowercase();
            let to_w = to_raw.to_lowercase();
            if from_w == to_w
                || from_w.contains(' ')
                || to_w.contains(' ')
                || from_w.contains('-')
                || to_w.contains('-')
            {
                continue;
            }
            let slot_freq = |w: &str, slot: u8| -> u8 {
                words
                    .get(w)
                    .map(|m| if slot == 0 { m.freq0 } else { m.freq1 })
                    .unwrap_or(0)
            };
            let to_f = slot_freq(&to_w, to_slot);
            let from_f = slot_freq(&from_w, from_slot);
            let shared_f = to_f.max(from_f).max(120);
            {
                let e = words.entry(from_w.clone()).or_default();
                add_word_for_targets(e, shared_f, &[(from_idx, from_slot)]);
            }
            {
                let e = words.entry(to_w.clone()).or_default();
                add_word_for_targets(e, shared_f, &[(to_idx, to_slot)]);
            }
            forward.push((from_w, to_w));
        }
        forward.sort_unstable();
        forward.dedup_by(|a, b| a.0 == b.0);
        let mut reverse: Vec<(String, String)> = forward
            .iter()
            .map(|(from_w, to_w)| (to_w.clone(), from_w.clone()))
            .collect();
        reverse.sort_unstable_by(|a, b| a.0.cmp(&b.0));
        reverse.dedup_by(|a, b| a.0 == b.0);

        if !forward.is_empty() {
            rewrites_by_target.insert(to_idx as u8, forward);
        }
        if !reverse.is_empty() {
            rewrites_by_target.insert(from_idx as u8, reverse);
        }
    }
    let variant_rewrites: Vec<(u8, Vec<(String, String)>)> =
        rewrites_by_target.into_iter().collect();

    // Assign `word_id` (`1..=65535`) to top 65,535 non-offensive words by max freq
    let mut freq_rank: Vec<(&String, u8)> = words
        .iter()
        .filter(|(_, m)| (m.flags & FLAG_OFFENSIVE) == 0 && m.freq0.max(m.freq1) > 0)
        .map(|(w, m)| (w, m.freq0.max(m.freq1)))
        .collect();
    freq_rank.sort_unstable_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));

    let mut word_ids: HashMap<String, u16> = HashMap::with_capacity(freq_rank.len().min(65_535));
    for (idx, (w, _)) in freq_rank.into_iter().take(65_535).enumerate() {
        word_ids.insert(w.clone(), (idx + 1) as u16);
    }

    // Build `WORDS_FST` & `SMASH_INDEX`
    let word_count = words.len();
    let mut words_fst_bytes = Vec::new();
    let mut smash_entries: Vec<(u32, u8, u8, String)> = Vec::new();
    if !words.is_empty() {
        let mut words_builder = MapBuilder::memory();
        for (w, m) in &words {
            let wid = word_ids.get(w).copied().unwrap_or(0);
            let meta = WordMeta {
                freq0: m.freq0,
                freq1: m.freq1,
                variant_mask: m.variant_mask,
                flags: m.flags,
                word_id: wid,
            };
            words_builder.insert(w.as_bytes(), meta.pack()).unwrap();

            let max_f = m.freq0.max(m.freq1);
            if (m.flags & FLAG_OFFENSIVE) == 0
                && w.is_ascii()
                && !w.contains('\'')
                && (7..=16).contains(&w.len())
                && max_f >= recipe.build.smash_min_freq
            {
                smash_entries.push((ascii_letter_mask(w), w.len() as u8, max_f, w.clone()));
            }
        }
        words_fst_bytes = words_builder.into_inner().unwrap();
    }

    // Build `KNOWN_TYPOS`
    let mut typos_map: BTreeMap<String, String> = BTreeMap::new();
    for csv_text in &typos_csv_texts {
        for line in csv_text.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let mut parts = line.split(',');
            let Some(raw_typo) = parts.next() else {
                continue;
            };
            let typo = raw_typo.trim().to_lowercase();
            if typo.chars().count() < 3 || !typo.chars().all(|c| c.is_alphabetic()) {
                continue;
            }
            if let Some(existing) = words.get(&typo) {
                if (existing.variant_mask & all_variants_mask) != 0
                    || (existing.flags & (FLAG_SHIELD | FLAG_OFFENSIVE)) != 0
                {
                    continue;
                }
            }
            let targets: Vec<&str> = parts
                .map(str::trim)
                .filter(|t| !t.is_empty() && !t.contains(' '))
                .collect();
            if targets.is_empty() {
                continue;
            }
            typos_map.insert(typo, targets.join("\n"));
        }
    }
    if let Some(rules) = &language_rules {
        for r in rules
            .replacements
            .iter()
            .filter(|r| r.kind == ReplacementKind::TypoOverride)
        {
            typos_map.insert(r.typed.clone(), r.fix.clone());
        }
    }

    let typo_count = typos_map.len();
    let mut typos_fst_bytes = Vec::new();
    let mut typo_pool: Vec<u8> = Vec::new();
    if !typos_map.is_empty() {
        let mut typos_builder = MapBuilder::memory();
        for (typo, joined_targets) in &typos_map {
            let offset = typo_pool.len() as u64;
            let len = joined_targets.len() as u64;
            typo_pool.extend_from_slice(joined_targets.as_bytes());
            let packed = offset | (len << 32);
            typos_builder.insert(typo.as_bytes(), packed).unwrap();
        }
        typos_fst_bytes = typos_builder.into_inner().unwrap();
    }

    // Build `BIGRAMS`
    let mut bigram_map: BTreeMap<u32, u8> = BTreeMap::new();
    for (p, n, sc) in aosp_bigrams {
        if let (Some(&pid), Some(&nid)) = (word_ids.get(&p), word_ids.get(&n)) {
            let key = ((pid as u32) << 16) | (nid as u32);
            let e = bigram_map.entry(key).or_insert(0);
            *e = (*e).max(sc);
        }
    }
    if !bigram_count_texts.is_empty() && !word_ids.is_empty() {
        let mut raw_counts: HashMap<u32, u64> = HashMap::new();
        for counts_text in &bigram_count_texts {
            for line in counts_text.lines() {
                let Some((pair, count_str)) = line.split_once('\t') else {
                    continue;
                };
                let Some((w1, w2)) = pair.split_once(' ') else {
                    continue;
                };
                let Ok(cnt) = count_str.trim().parse::<u64>() else {
                    continue;
                };
                let l1 = w1.to_lowercase();
                let l2 = w2.to_lowercase();
                if let (Some(&pid), Some(&nid)) = (word_ids.get(&l1), word_ids.get(&l2)) {
                    let key = ((pid as u32) << 16) | (nid as u32);
                    *raw_counts.entry(key).or_insert(0) += cnt;
                }
            }
        }
        let mut ranked_bg: Vec<(u32, u64)> = raw_counts.into_iter().collect();
        ranked_bg.sort_unstable_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        for (key, cnt) in ranked_bg.into_iter().take(recipe.build.bigram_top_n) {
            let ln_val = (cnt as f64).ln();
            let norm = ((ln_val - 10.5) / 10.0).clamp(0.0, 1.0);
            let q = (80.0 + norm * 175.0).round() as u8;
            let e = bigram_map.entry(key).or_insert(0);
            *e = (*e).max(q);
        }
    }
    if let Some(rules) = &language_rules {
        for b in &rules.bigram_boosts {
            if let (Some(&pid), Some(&nid)) = (word_ids.get(&b.prev), word_ids.get(&b.next)) {
                let key = ((pid as u32) << 16) | (nid as u32);
                bigram_map.insert(key, b.score);
            }
        }
    }

    let syllable_dict: Vec<String> = if recipe.build.syllable_dict {
        syllables.into_iter().collect()
    } else {
        Vec::new()
    };
    let slang_set: Vec<String> = if recipe.build.syllable_dict || tier == PackTier::ImeBridge {
        slang_words.into_iter().collect()
    } else {
        Vec::new()
    };

    let header = PackHeader {
        lang: recipe.pack.code,
        pack_version,
        tier,
        variants: variant_names,
        freq_slots,
        default_variant_idx,
        feature_flags: 0,
    };

    let bytes = serialize_skdict_v3(&SkdictV3Payload {
        header: &header,
        words_fst: &words_fst_bytes,
        typos_fst: &typos_fst_bytes,
        typo_pool: &typo_pool,
        variant_rewrites: &variant_rewrites,
        bigrams: &bigram_map,
        smash_entries: &smash_entries,
        proper_nouns: &proper_nouns,
        shorthand: &shorthand_pairs,
        slang_set: &slang_set,
        rules: language_rules.as_ref(),
        syllable_dict: &syllable_dict,
        source_manifest: &manifest,
    });

    // Self-verify deterministic binary roundtrip before returning.
    let verified = PackLexicon::from_owned_bytes(bytes.clone())
        .map_err(|e| format!("built pack failed roundtrip verification: {e}"))?;
    if verified.header() != &header {
        return Err("built pack header mismatch on roundtrip verification".to_string());
    }

    Ok(BuiltPack {
        header,
        bytes,
        warnings,
        manifest,
        word_count,
        typo_count,
        bigram_count: bigram_map.len(),
        smash_count: smash_entries.len(),
        proper_noun_count: proper_nouns.len(),
        shorthand_count: shorthand_pairs.len(),
        slang_count: slang_set.len(),
        syllable_count: syllable_dict.len(),
    })
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;
    use crate::types::Dialect;

    fn tmp_pack_dir(tag: &str) -> PathBuf {
        static NEXT_ID: AtomicU64 = AtomicU64::new(1);
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "smartkey_pack_builder_test_{tag}_{}_{}",
            std::process::id(),
            id
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn sha256_matches_nist_fips_180_4_vectors() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            sha256_hex(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
        // 2-block NIST vector (112 bytes)
        assert_eq!(
            sha256_hex(
                b"abcdefghbcdefghicdefghijdefghijkefghijklfghijklmghijklmnhijklmnoijklmnopjklmnopqklmnopqrlmnopqrsmnopqrstnopqrstu"
            ),
            "cf5b16a778af8380036ce59e7b0492370b249b11e8f07a51afac45037afee9d1"
        );
    }

    #[test]
    fn sha256_mismatch_and_missing_license_fail_hard() {
        let dir = tmp_pack_dir("bad_sha");
        fs::write(dir.join("words.txt"), "hola\t200\n").unwrap();

        let pack_toml = format!(
            "[pack]\ncode = \"es\"\nversion = \"0.1.0\"\nformat_version = 3\ntier = \"data\"\nlicense = \"GPL-3.0-or-later\"\n\n[variants.es_ES]\nfreq_slot = 0\ndefault = true\n\n[sources.wl]\nformat = \"wordlist\"\npath = \"words.txt\"\nsha256 = \"{}\"\nlicense = \"Apache-2.0\"\n",
            "0".repeat(64)
        );
        fs::write(dir.join("pack.toml"), pack_toml).unwrap();
        let err = build_pack_from_dir(&dir, &PackBuildOptions::default()).unwrap_err();
        assert!(err.contains("SHA-256 mismatch"), "got: {err}");

        // Missing license fails hard
        let good_sha = sha256_hex(b"hola\t200\n");
        let missing_lic = format!(
            "[pack]\ncode = \"es\"\nversion = \"0.1.0\"\nformat_version = 3\ntier = \"data\"\nlicense = \"GPL-3.0-or-later\"\n\n[variants.es_ES]\nfreq_slot = 0\ndefault = true\n\n[sources.wl]\nformat = \"wordlist\"\npath = \"words.txt\"\nsha256 = \"{good_sha}\"\n"
        );
        fs::write(dir.join("pack.toml"), missing_lic).unwrap();
        let err = build_pack_from_dir(&dir, &PackBuildOptions::default()).unwrap_err();
        assert!(err.contains("missing required `license`"), "got: {err}");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn warnings_manifest_homograph_and_shorthand_lints_work() {
        let dir = tmp_pack_dir("lints");
        let aosp = " word=polish,f=92\n word=Polish,f=126\n word=cleanword,f=180\n";
        let bg = "polish cleanword\t500000\n";
        fs::write(dir.join("words.combined"), aosp).unwrap();
        fs::write(dir.join("bigrams.txt"), bg).unwrap();
        fs::write(dir.join("proper.tsv"), "polish\tPolish\n").unwrap();
        fs::write(dir.join("short.tsv"), "pls\tplease\npls\tplease2\n").unwrap();

        let pack_toml = format!(
            "[pack]\ncode = \"en\"\nversion = \"0.3.0\"\nformat_version = 3\ntier = \"data\"\nlicense = \"GPL-3.0-or-later\"\n\n[variants.en_US]\nfreq_slot = 0\ndefault = true\n\n[sources.main]\nformat = \"aosp_combined\"\npath = \"words.combined\"\nsha256 = \"{}\"\nlicense = \"CC-BY-NC-4.0\"\n\n[sources.bigram_counts]\nformat = \"bigram_counts\"\npath = \"bigrams.txt\"\nsha256 = \"{}\"\nlicense = \"UNVERIFIED\"\n\n[overlays]\nshorthand = [\"short.tsv\"]\nproper_nouns = [\"proper.tsv\"]\n",
            sha256_hex(aosp.as_bytes()),
            sha256_hex(bg.as_bytes())
        );
        fs::write(dir.join("pack.toml"), pack_toml).unwrap();

        // 1. Duplicate shorthand trigger fails hard
        let err = build_pack_from_dir(&dir, &PackBuildOptions::default()).unwrap_err();
        assert!(err.contains("duplicate shorthand trigger"), "got: {err}");

        // Fix duplicate, check empty expansion fails hard
        fs::write(dir.join("short.tsv"), "pls\t\n").unwrap();
        let err = build_pack_from_dir(&dir, &PackBuildOptions::default()).unwrap_err();
        assert!(err.contains("empty shorthand"), "got: {err}");

        // Fix shorthand, now homograph error fails unless --allow-ambiguous is set
        fs::write(dir.join("short.tsv"), "pls\tplease\n").unwrap();
        let err = build_pack_from_dir(&dir, &PackBuildOptions::default()).unwrap_err();
        assert!(err.contains("homograph lint failed"), "got: {err}");

        // With --allow-ambiguous: succeeds, sets FLAG_AMBIGUOUS_PROPER, records manifest & warnings
        let built = build_pack_from_dir(
            &dir,
            &PackBuildOptions {
                allow_ambiguous: true,
                ..PackBuildOptions::default()
            },
        )
        .expect("allow_ambiguous build passes");
        assert!(built.warnings.iter().any(|w| w.contains("NC/ND")));
        assert!(built.warnings.iter().any(|w| w.contains("UNVERIFIED")));
        assert_eq!(built.manifest.len(), 2);
        assert!(built.bigram_count > 0);

        let lex = PackLexicon::from_owned_bytes(built.bytes).unwrap();
        let meta = lex.lookup_word("polish").unwrap();
        assert!(meta.is_ambiguous_proper());
        assert!(lex.contains_valid("cleanword", Dialect::Mixed));

        // With skip_sources = ["bigram_counts"]: bigram_counts skipped cleanly
        let ablated = build_pack_from_dir(
            &dir,
            &PackBuildOptions {
                allow_ambiguous: true,
                skip_sources: &["bigram_counts"],
                ..PackBuildOptions::default()
            },
        )
        .unwrap();
        assert_eq!(ablated.manifest.len(), 1);
        assert_eq!(ablated.bigram_count, 0);

        let _ = fs::remove_dir_all(&dir);
    }
}
