# Linux release build image. Debian 12 is the oldest distro Clipmonger is
# tested on, so binaries built here need no glibc newer than its 2.36
# (seg-12 §2). Build with scripts/build-linux-release.sh.
FROM debian:12-slim

ARG RUST_TOOLCHAIN
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates curl build-essential pkg-config \
 && rm -rf /var/lib/apt/lists/*

ENV RUSTUP_HOME=/opt/rustup CARGO_HOME=/opt/cargo PATH=/opt/cargo/bin:$PATH
RUN test -n "$RUST_TOOLCHAIN" \
 && curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
    | sh -s -- -y --no-modify-path --profile minimal --default-toolchain "$RUST_TOOLCHAIN" \
 && chmod -R a+rX /opt/rustup /opt/cargo
