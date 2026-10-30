#!/usr/bin/env bash
# Local quality gate (MoriNet scripts/check.sh shape + axum-absence guard).
# Everything runs HERE, not on GitHub runners.
set -euo pipefail
cd "$(dirname "$0")/.."

echo "==> fmt"
cargo fmt --check

echo "==> clippy -D warnings"
cargo clippy --all-targets -- -D warnings

echo "==> zero-real-LLM guard (D-14)"
if grep -q 'OpenAiCompat' crates/saussunet-core/tests/llm_provider.rs crates/saussunet-core/tests/queue.rs; then
  echo 'FORBIDDEN: real provider in mock-only tests' >&2
  exit 1
fi

echo "==> tests (zero-real-LLM: key env provably unset)"
env -u SAUSSUNET_LLM__API_KEY cargo test

echo "==> axum-absence guard (Anti-Pattern 1)"
if grep -q 'axum' crates/saussunet-core/Cargo.toml; then
  echo 'FORBIDDEN: axum in saussunet-core' >&2
  exit 1
fi

echo "==> write-funnel guard (PIPE-06)"
# Single-writer funnel: the only sanctioned disk writes in core src are the
# atomic primitive (vault/atomic.rs) and raw records (vault/raw.rs). Pipeline
# pages must go through vault::writer::write_generated_page — the LLM never
# writes to the vault directly, and CI enforces it, not review.
if grep -rn --include='*.rs' -E 'fs::write|File::create' crates/saussunet-core/src/ | grep -v 'vault/atomic.rs' | grep -v 'vault/raw.rs' | grep -v '//'; then
  echo 'FORBIDDEN: direct fs write outside vault/atomic.rs / vault/raw.rs — use write_generated_page (PIPE-06 funnel)' >&2
  exit 1
fi

echo "==> release build"
cargo build --release

echo "check: OK"
