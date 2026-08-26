<div align="center">

# Rini

**A scrollable-tiling window manager for macOS. One layout, done properly.**

Rini is a hard fork of [rift](https://github.com/acsandmann/rift) that keeps the
scrolling layout and throws the rest away. If you want [niri](https://github.com/YaLTeR/niri)
on a Mac, this is an attempt at that and nothing else.

</div>

## Why fork

Rift is a good, general-purpose window manager: five layout modes, a menu bar picker,
broad configurability. That generality is the problem for what I wanted.

I use one layout — the niri-style scrollable strip. Every other mode is code that has to
keep working, be configured around, and be reasoned about when something breaks. Worse, a
lot of the hard bugs in a scrolling layout are *specific to it*: what a column's width
means, what happens to a scrolled-away window, what "the same workspace" means when two
displays show different parts of it. Those questions have no good answer that is also
correct for BSP and master-stack, so in a multi-layout codebase they get answered
conservatively, or not at all.

Rini answers them for scrolling only, and accepts breaking changes to do it. A soft fork
tracking upstream could not: nearly every fix here changes shared behaviour, deletes a
subsystem, or reshapes persisted state. So this is a hard fork with no intention of
merging back.

## Direction

**One layout.** Scrollable columns. `traditional`, `bsp`, `master_stack` and `stack` are
deleted, not deprecated — along with the layout picker, since a menu offering one choice
is noise.

**Opinionated defaults over configurability.** Where rift offers a setting, Rini prefers a
decision. Fewer knobs, each one load-bearing.

**Multi-display correctness as a first-class concern.** This is where most of the work has
gone, and where the design differs most from upstream:

- **Workspaces are global; each display shows one independently.** The built-in can be on
  `comms` while the external is on `coding`. Switching one display never moves the other.
- **A window's display is durable, not inferred.** Displays get a UUID-keyed identity,
  because macOS mints a brand-new space id on every reconnect (observed 479 → 484 → 487 →
  516 → 552 → 1138 for one monitor in a single session). Unplug and replug returns *your*
  windows in *their* order.
- **Window width belongs to the display, not the workspace.** A window sized to fill a
  laptop panel keeps that size across every workspace on it, and adopts whatever it last
  had on the 32" when it moves there. Half of 3008pt and half of 1728pt are not the same
  request.
- **Never guess a window's display from its coordinates.** Off-workspace windows are
  parked off-screen, and macOS will not keep a window fully outside every display — so
  parked coordinates land on a *neighbour*. Trusting them creates a feedback loop that
  walks every window onto one display. Rini trusts space membership instead.

**Behaviour rift leaves to macOS, where macOS gets it wrong.** ⌘\` only cycles windows on
the visible workspace, so an app with windows on three workspaces silently cycles two.
Rini does the rotation itself and brings the display along.

**Diagnosability.** `rini-cli query diagnostics` dumps the whole topology — every display,
every workspace, column origins and widths, which windows are parked, and where each
window's durable home is. It exists because reasoning about multi-display state from
per-space queries produced three wrong conclusions in a row.

## Status

**Personal project, used daily on one machine.** It works well for me. I am not
soliciting users, I make breaking changes without warning, and persisted layout state has
no migration guarantees. If you want a stable macOS tiling WM with a community behind it,
use [rift](https://github.com/acsandmann/rift) — it is actively maintained and good.

Known rough edges are tracked as I hit them. Two upstream tests fail on `main` and are
not yet mine to fix.

## Building

```sh
./bin/build.sh                     # build, then re-sign with the stable local identity
./target/release/rini --validate   # parse config + layout file, exit
```

Use the script rather than `cargo build --release` directly. macOS grants Accessibility to a code
identity, and an ad-hoc signature — which is what cargo leaves — makes that identity the hash of the
binary, so every plain rebuild costs an Accessibility re-grant and rini manages nothing until it is
given. `docs/signing.md` has the detail and how to create the certificate.

Requires a Rust toolchain and macOS. Does **not** require disabling SIP. Works with
"Displays have separate Spaces" enabled — in fact it assumes it.

Formatting needs nightly, because `rustfmt.toml` enables unstable options:

```sh
rustup toolchain install nightly
cargo +nightly fmt
```

Running stable `cargo fmt` silently ignores those options and reflows the whole file,
which makes every diff a conflict.

## Configuration

Config lives at `~/.config/rini/config.toml`; saved layout state at `~/.rini/layout.ron`.
`rini.default.toml` in this repo is the annotated reference and is compiled in as the
default.

The config format is inherited from rift and has diverged. Upstream's
[wiki](https://github.com/acsandmann/rift/wiki/Config) is still the best explanation of
the shared parts, but scrolling-specific and multi-display settings differ.

## Interop

Same Mach-port IPC as rift, under `git.kaievns.rini`:

```sh
rini-cli query diagnostics       # whole-topology dump
rini-cli query displays
rini-cli execute workspace switch 2
```

Signals fire on startup, workspace switches, and workspace window changes, so a status bar
can subscribe rather than poll. My SketchyBar indicator reads `query diagnostics` and draws
one row set per display, pinned with SketchyBar's `display` property, so each bar shows
only its own display's position.

## Credits

Rini is a hard fork of [rift](https://github.com/acsandmann/rift) by
[acsandmann](https://github.com/acsandmann), which is itself a fork of
[glide-wm](https://github.com/glide-wm/glide) by tmandry. Rift did the overwhelming
majority of the engineering here — the actor architecture, the private-API work, the
animation system, the IPC layer — and this fork would not exist without it. Copyright and
license are unchanged; see [LICENSE](LICENSE).

It uses private APIs reverse engineered by yabai and other projects. Not affiliated with
rift, glide-wm, or yabai.
