#!/usr/bin/env bash
# Coverage floor (seg-11 §9): line coverage of core, platform and ipc must
# stay at or above 80%. Needs cargo-llvm-cov and the llvm-tools component:
#   rustup component add llvm-tools-preview && cargo install cargo-llvm-cov --locked
# Calibrated 2026-11-13 on Linux: 89.2% lines, 87.5% regions.
set -euo pipefail
FLOOR=${COVERAGE_FLOOR:-80}
JOBS=${JOBS:-2}
cd "$(dirname "$0")/.."
cargo llvm-cov -p clipmonger-core -p clipmonger-platform -p clipmonger-ipc \
    --features clipmonger-ipc/server -j "$JOBS" --summary-only --fail-under-lines "$FLOOR"
