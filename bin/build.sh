#!/usr/bin/env bash
#
# Build, then re-sign with the stable local identity.
#
# macOS keys an Accessibility grant to the binary's code identity. An ad-hoc signature, which is what
# cargo leaves behind, makes that identity the hash of the binary itself:
#
#     designated => cdhash H"bc6e2d6fca246b8ed4e78c2d9286b97b7ed81059"
#
# so every rebuild is a different program as far as TCC is concerned, and the grant has to be given
# again. Signing with a certificate replaces the hash with the certificate:
#
#     designated => identifier "git.kaievns.rini" and certificate leaf = H"bfbde802..."
#
# which does not change when the binary does. The certificate is self-signed and lives in the login
# keychain; `docs/signing.md` has how it was made and how to replace it.
#
# Skipping this step is not a small mistake: the next restart costs an Accessibility re-grant, and
# rini does not manage windows until it is given.
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."

IDENTITY="${RINI_SIGNING_IDENTITY:-rini-dev}"
BUNDLE_ID="git.kaievns.rini"

cargo build --release "$@"

if ! security find-certificate -c "$IDENTITY" >/dev/null 2>&1; then
    echo "signing identity '$IDENTITY' not found in the keychain; see docs/signing.md" >&2
    exit 1
fi

codesign -f -s "$IDENTITY" --identifier "$BUNDLE_ID" target/release/rini
codesign -d -r- target/release/rini 2>&1 | grep -F 'certificate leaf' >/dev/null || {
    echo "signed, but the designated requirement still has no certificate in it" >&2
    exit 1
}

echo "built and signed as $BUNDLE_ID with $IDENTITY"
