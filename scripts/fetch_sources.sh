#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SRC_DIR="$ROOT_DIR/data/sources"
mkdir -p "$SRC_DIR"

OPENBOARD_REF="${OPENBOARD_REF:-v1.4.5}"
TYPOS_REF="${TYPOS_REF:-v1.29.4}"
AB_REF="${AB_REF:-master}"

echo "Fetching AOSP en_US and en_GB combined wordlists (Apache-2.0, ${OPENBOARD_REF})..."
curl -sL --fail --retry 3 --max-time 30 \
  "https://raw.githubusercontent.com/openboard-team/openboard/${OPENBOARD_REF}/dictionaries/en_wordlist.combined.gz" \
  -o "$SRC_DIR/en_US.combined.gz"
curl -sL --fail --retry 3 --max-time 30 \
  "https://raw.githubusercontent.com/openboard-team/openboard/${OPENBOARD_REF}/dictionaries/en_GB_wordlist.combined.gz" \
  -o "$SRC_DIR/en_GB.combined.gz"
gzip -dc "$SRC_DIR/en_US.combined.gz" > "$SRC_DIR/en_US.combined"
gzip -dc "$SRC_DIR/en_GB.combined.gz" > "$SRC_DIR/en_GB.combined"

echo "Fetching crate-ci/typos words.csv (MIT/Apache-2.0, ${TYPOS_REF})..."
curl -sL --fail --retry 3 --max-time 30 \
  "https://raw.githubusercontent.com/crate-ci/typos/${TYPOS_REF}/crates/typos-dict/assets/words.csv" \
  -o "$SRC_DIR/typos_words.csv"

echo "Fetching British-to-American spelling map (MIT, ${AB_REF})..."
curl -sL --fail --retry 3 --max-time 30 \
  "https://raw.githubusercontent.com/hyperreality/American-British-English-Translator/${AB_REF}/data/british_to_american.json" \
  -o "$SRC_DIR/british_to_american.json" || \
curl -sL --fail --retry 3 --max-time 30 \
  "https://raw.githubusercontent.com/hyperreality/American-British-English-Translator/${AB_REF}/data/british_spellings.json" \
  -o "$SRC_DIR/british_to_american.json"

echo "Fetching Norvig 2-word prose bigram counts..."
curl -sL --fail --retry 3 --max-time 30 \
  "https://norvig.com/ngrams/count_2w.txt" \
  -o "$SRC_DIR/count_2w.txt"

echo "Done. Sources saved in $SRC_DIR"
