#!/usr/bin/env bash
# Build the Linux release binaries in a Debian 12 container, so they run on
# every supported distro (glibc 2.36 floor, seg-12 §2), then check the floor.
# Output: target/linux-release/release/{clipmonger,clipmonger-daemon,clipmonger-cli}
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
TOOLCHAIN=$(sed -n 's/^channel *= *"\(.*\)"/\1/p' "$ROOT/rust-toolchain.toml")
IMAGE="clipmonger-build:debian12-rust$TOOLCHAIN"
TARGET_DIR="$ROOT/target/linux-release"
JOBS=${JOBS:-4}

if ! docker image inspect "$IMAGE" >/dev/null 2>&1; then
    docker build --build-arg RUST_TOOLCHAIN="$TOOLCHAIN" -t "$IMAGE" "$ROOT/packaging/linux/build"
fi

mkdir -p "$TARGET_DIR/cargo-home"
# The host's user owns the output; the crate cache stays under target/.
docker run --rm --cpus "$JOBS" --user "$(id -u):$(id -g)" \
    -v "$ROOT:/src" -w /src \
    -e RUSTUP_TOOLCHAIN="$TOOLCHAIN" \
    -e CARGO_HOME=/src/target/linux-release/cargo-home \
    -e CARGO_TARGET_DIR=/src/target/linux-release \
    -e PATH=/opt/cargo/bin:/usr/local/bin:/usr/bin:/bin \
    "$IMAGE" \
    nice -n 10 cargo build --release --locked -j "$JOBS" \
        --bin clipmonger --bin clipmonger-daemon --bin clipmonger-cli

OUT="$TARGET_DIR/release"
python3 "$ROOT/scripts/check-glibc-floor.py" "$OUT/clipmonger" "$OUT/clipmonger-daemon" "$OUT/clipmonger-cli"
