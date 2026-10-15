#!/usr/bin/env bash
#
# Rustc wrapper: compile, then give the rini binary its stable signing identity.
#
# Wired in through `.cargo/config.toml` as the workspace wrapper, so EVERY `cargo build` produces a
# signed binary — not just the ones that remember to go through `bin/build.sh`. The launch agent
# points at the build output with KeepAlive, so a bare cargo build that leaves an ad-hoc signature
# costs an Accessibility re-grant at the next service restart; see docs/signing.md.
#
# This has to wrap rustc rather than the linker: `strip = true` makes rustc run strip AFTER the
# linker, and Apple's strip re-signs the binary ad-hoc, so a signature applied at link time is
# clobbered before rustc returns. Cargo copies the artifact out of deps/ only after rustc exits,
# which is why signing here reaches `target/release/rini`.
#
# Signs only the `rini` bin. A missing certificate (CI, another machine) skips signing rather than
# failing the build.
#
# Refuses, before rustc runs, any compile into this checkout's `target/release` unless RINI_DEPLOY=1:
# launchd runs `target/release/rini` with KeepAlive, so a build there is a deploy.
set -euo pipefail

crate_name=""
out_dir=""
extra=""
prev=""
for arg in "$@"; do
    case "$prev" in
        --crate-name) crate_name="$arg" ;;
        --out-dir) out_dir="$arg" ;;
        -C) [[ "$arg" == extra-filename=* ]] && extra="${arg#extra-filename=}" ;;
    esac
    prev="$arg"
done

physical() {
    if [[ -d "$1" ]]; then
        (cd "$1" && pwd -P)
    else
        local parent
        parent="$(physical "$(dirname "$1")")"
        printf '%s/%s\n' "${parent%/}" "$(basename "$1")"
    fi
}

if [[ -n "$out_dir" && "${RINI_DEPLOY:-}" != 1 ]]; then
    deploy_dir="$(physical "$(dirname "${BASH_SOURCE[0]}")/../target/release")"
    resolved="$(physical "$out_dir")"
    if [[ "$resolved" == "$deploy_dir" || "$resolved" == "$deploy_dir"/* ]]; then
        cat >&2 <<EOF
sign-rustc.sh: refusing to compile into $deploy_dir.
launchd runs $deploy_dir/rini with KeepAlive, so a build there is a deploy.
Build anywhere else with CARGO_TARGET_DIR=target-dev (cargo test --release and cargo bench too);
deploy with bin/build.sh, which sets RINI_DEPLOY=1. See docs/signing.md.
EOF
        exit 1
    fi
fi

"$@"

[[ "$crate_name" == "rini" && -n "$out_dir" ]] || exit 0
binary="$out_dir/rini$extra"
[[ -x "$binary" ]] || exit 0

IDENTITY="${RINI_SIGNING_IDENTITY:-rini-dev}"
if security find-certificate -c "$IDENTITY" >/dev/null 2>&1; then
    codesign -f -s "$IDENTITY" --identifier git.kaievns.rini "$binary"
fi
