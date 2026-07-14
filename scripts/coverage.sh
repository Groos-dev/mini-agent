#!/usr/bin/env bash
set -euo pipefail

if ! command -v cargo-llvm-cov >/dev/null 2>&1; then
    echo "cargo-llvm-cov is required. Install it with: cargo install cargo-llvm-cov" >&2
    exit 1
fi

cargo llvm-cov \
    --workspace \
    --all-targets \
    --fail-under-lines 85 \
    --lcov \
    --output-path lcov.info
