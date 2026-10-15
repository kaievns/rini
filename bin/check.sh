#!/usr/bin/env bash
#
# The checks a change passes, run the same way here and in CI (.github/workflows/rust.yml): nightly
# formatting, a build of every target with warnings as errors, and the suite. Every step runs, so a
# failing one does not hide the others; the exit status is non-zero if any failed.
#
# Builds into target-dev unless CARGO_TARGET_DIR names another: target/release in the main checkout
# is the binary the service runs (README.md, "A release build in the main checkout is a deploy"), and
# the -D warnings flags would otherwise rebuild a plain build's artifacts every time.
set -uo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
cd "$root"

export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$root/target-dev}"
export RUSTFLAGS="${RUSTFLAGS:+$RUSTFLAGS }-D warnings"

failed=()

step() {
    local name="$1"
    shift
    echo "==> $name: $*"
    "$@" || failed+=("$name")
}

# rustfmt.toml enables unstable options, which only nightly's rustfmt reads. RUSTFMT names it,
# because a cargo installed outside rustup finds the rustfmt on PATH even under `rustup run`.
nightly_fmt() {
    local rustfmt
    rustfmt="$(rustup which --toolchain nightly rustfmt)" || return 1
    RUSTFMT="$rustfmt" rustup run nightly cargo fmt --all --check
}

step fmt nightly_fmt
step build cargo build --locked --workspace --all-targets
# clippy goes here, between the build and the suite, once the workspace is clean of its warnings.
step test cargo test --locked --workspace

if ((${#failed[@]})); then
    echo "==> failed: ${failed[*]}" >&2
    exit 1
fi
echo "==> all checks passed"
