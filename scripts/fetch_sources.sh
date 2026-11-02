#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SRC_DIR="$ROOT_DIR/data/sources"
mkdir -p "$SRC_DIR"

echo "Fetching AOSP en_US and en_GB combined wordlists (Apache-2.0)..."
curl -sL --fail --max-time 30 \
  "https://raw.githubusercontent.com/openboard-team/openboard/v1.4.5/dictionaries/en_wordlist.combined.gz" \
  -o "$SRC_DIR/en_US.combined.gz"
curl -sL --fail --max-time 30 \
  "https://raw.githubusercontent.com/openboard-team/openboard/v1.4.5/dictionaries/en_GB_wordlist.combined.gz" \
  -o "$SRC_DIR/en_GB.combined.gz"
gunzip -kf "$SRC_DIR/en_US.combined.gz" "$SRC_DIR/en_GB.combined.gz"

echo "Fetching crate-ci/typos words.csv (MIT/Apache-2.0)..."
curl -sL --fail --max-time 30 \
  "https://raw.githubusercontent.com/crate-ci/typos/master/crates/typos-dict/assets/words.csv" \
  -o "$SRC_DIR/typos_words.csv"

echo "Fetching British-to-American spelling map (MIT)..."
curl -sL --fail --max-time 30 \
  "https://raw.githubusercontent.com/hyperreality/American-British-English-Translator/master/data/british_to_american.json" \
  -o "$SRC_DIR/british_to_american.json" || \
curl -sL --fail --max-time 30 \
  "https://raw.githubusercontent.com/hyperreality/American-British-English-Translator/master/data/british_spellings.json" \
  -o "$SRC_DIR/british_to_american.json"

echo "Fetching Norvig 2-word prose bigram counts..."
curl -sL --fail --max-time 30 \
  "https://norvig.com/ngrams/count_2w.txt" \
  -o "$SRC_DIR/count_2w.txt"

echo "Done. Sources saved in $SRC_DIR"
