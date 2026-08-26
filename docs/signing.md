# Why the build is signed, and how to redo it

Accessibility is granted to a **code identity**, not to a path. Cargo leaves an ad-hoc signature, and
for ad-hoc code that identity is the hash of the binary:

```
$ codesign -d -r- target/release/rini
designated => cdhash H"bc6e2d6fca246b8ed4e78c2d9286b97b7ed81059"
```

So every rebuild is a different program as far as TCC is concerned, and the grant has to be given
again. Measured over one afternoon: five re-grants, one per rebuild, each one a dialog and a
half-minute of rini not managing windows.

Two dead ends, both checked rather than assumed:

- **A stable path does not help.** `~/.local/bin/rini` and `target/release/rini` behave identically,
  because the path is not the identity.
- **Removing the signature does not help.** arm64 binaries must be signed to execute at all; a copy
  with `codesign --remove-signature` applied is killed on launch.

Signing with a certificate replaces the hash in the requirement with the certificate:

```
designated => identifier "git.kaievns.rini" and certificate leaf = H"bfbde802..."
```

Neither half changes when the binary does, so the grant survives a rebuild. This is the same
workaround yabai documents for the same reason. `bin/build.sh` builds and re-signs in one step and
fails loudly if the requirement comes out without a certificate in it.

## Recreating the certificate

Self-signed, in the login keychain, valid for 20 years. Nothing outside this machine trusts it and
nothing needs to.

```sh
mkdir -p ~/.config/rini/signing && cd ~/.config/rini/signing

openssl req -x509 -newkey rsa:2048 -sha256 -days 7300 -nodes \
    -keyout rini-dev.key -out rini-dev.crt -subj "/CN=rini-dev" \
    -addext "extendedKeyUsage=critical,codeSigning" \
    -addext "basicConstraints=critical,CA:false" \
    -addext "keyUsage=critical,digitalSignature"

# -legacy matters: Security.framework cannot read a PKCS#12 written with OpenSSL 3 defaults.
openssl pkcs12 -export -legacy -inkey rini-dev.key -in rini-dev.crt \
    -out rini-dev.p12 -name rini-dev -passout pass:rini

# -T lets codesign use the key without a prompt each time; -A allows it without the keychain password.
security import rini-dev.p12 -k ~/Library/Keychains/login.keychain-db \
    -P rini -T /usr/bin/codesign -A
```

`security find-identity -v -p codesigning` reports **0 valid identities** afterwards, because the
certificate is not trusted as a root. That does not matter: `codesign` signs with it regardless, and
what TCC compares is the certificate in the requirement, not a trust chain. Trusting it as a root
would need an admin prompt and buys nothing.

Replacing the certificate invalidates the grant once, because the leaf hash in the requirement
changes. Everything after that is free.

## When the grant does still have to be re-given

- The certificate is replaced or removed.
- The binary is signed ad-hoc again, which is what a bare `cargo build --release` does. Use
  `bin/build.sh`.
