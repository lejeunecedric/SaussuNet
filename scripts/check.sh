#!/usr/bin/env bash
# Local quality gate (MoriNet scripts/check.sh shape + axum-absence guard).
# Everything runs HERE, not on GitHub runners.
set -euo pipefail
cd "$(dirname "$0")/.."

echo "==> fmt"
cargo fmt --check

echo "==> clippy -D warnings"
cargo clippy --all-targets -- -D warnings

echo "==> tests"
cargo test

echo "==> axum-absence guard (Anti-Pattern 1)"
if grep -q 'axum' crates/saussunet-core/Cargo.toml; then
  echo 'FORBIDDEN: axum in saussunet-core' >&2
  exit 1
fi

echo "==> release build"
cargo build --release

echo "check: OK"
