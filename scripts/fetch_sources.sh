#!/usr/bin/env bash
# Deprecated wrapper kept for convenience; canonical fetch/verify entrypoint is `smartkey-dev build-pack <lang> --fetch`.
set -euo pipefail

export PATH="$HOME/.cargo/bin:$PATH"

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
echo "Note: scripts/fetch_sources.sh is deprecated; delegating to smartkey-dev build-pack en --fetch (SHA-256 verified)..."
cargo run --release --manifest-path "${ROOT_DIR}/Cargo.toml" -p smartkey-cli --bin smartkey-dev -- build-pack en --fetch
