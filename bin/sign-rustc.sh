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
set -euo pipefail

"$@"

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

[[ "$crate_name" == "rini" && -n "$out_dir" ]] || exit 0
binary="$out_dir/rini$extra"
[[ -x "$binary" ]] || exit 0

IDENTITY="${RINI_SIGNING_IDENTITY:-rini-dev}"
if security find-certificate -c "$IDENTITY" >/dev/null 2>&1; then
    codesign -f -s "$IDENTITY" --identifier git.kaievns.rini "$binary"
fi
