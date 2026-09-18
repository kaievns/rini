# Architecture: crates by bounded context

rini is a Cargo workspace. Each crate is one bounded context with a hard
boundary, and dependencies point one way. The compiler enforces the direction:
a cycle between crates does not build, which is the reason the split is by
crate and not by module (see "Why crates").

## Target crate graph

```
rini-protocol   wire types for the Mach IPC API (exists)
rini-core       shared kernel: ids, collections, geometry, log, channel   -> protocol
rini-macos      sys/: AX, SkyLight, event tap, run loop, executor, mach   -> core
rini-config     config file, validation, hot reload, hotkey specs        -> core, macos
rini-layout     scrolling strip, virtual workspaces, floating, app
                rules, layout.ron persistence, display affinity          -> core, config
rini-motion     animation plans, easing, strip geometry, z-groups (pure) -> core
rini-overlay    CA tile renderer, snapshot cache and capture, flights    -> core, macos, motion
rini-ipc        mach server, subscriptions, cli_exec                     -> protocol, macos
rini-client     client for the API (exists)                              -> protocol
rini-wm         reactor and its state, input taps, wm_controller,
                the overlay mode state machine                           -> all of the above
rini, rini-cli  binaries
```

`rini-overlay` knows tiles and flights and nothing about workspaces, strips,
or grids. Every user of the overlay (strip animation today; expose and a window
switcher later) is a mode owned by `rini-wm` that turns its own scene into tile
targets and hands them over.

## Current state

| Crate | Status |
|---|---|
| `rini-protocol`, `rini-client` | done before the split |
| `rini-core` | done: `ids`, `collections`, `geometry`, `log`, `util`, `channel` |
| `rini-macos` | next: `src/sys/` has no imports from `actor`, `layout_engine`, `model`, `ui` or `common` any more |
| everything else | still modules inside the `rini-wm` crate |

Inside `rini-wm`, `crate::actor::app::WindowId`, `crate::sys::window_server::WindowServerId`
and `crate::sys::screen::SpaceId` are re-exports of `rini_core::ids`. They exist so
brace imports keep compiling until each module is lifted, and go away with it.

## What belongs in `rini-core`

Types every crate agrees on and nothing that touches a window or a display:

- `ids`: `WindowId` (pid + per-process index, serde accepts struct, seq, and
  its own `Debug` string), `WindowServerId` (CGWindowID), `SpaceId`, `pid_t`,
  and the `From` impls between them and `rini_protocol::WindowId`.
- `collections`: Fx-hashed `HashMap`/`HashSet`, and `BTreeExt::remove_all_for_pid`,
  which relies on `WindowId` ordering by pid first.
- `geometry`: rounding, tolerance comparison (`same_as` is 0.1pt), containment,
  and serde adapters for CoreFoundation geometry.
- `log`: tracing setup and the `show_timing` histogram dump.
- `channel`: the one channel type every loop uses, an unbounded tokio mpsc with
  the sender's tracing span attached to each message.

`Direction` and `ResizeOrientation` are re-exported from `rini-protocol` because
they are both wire and domain types.

## Why crates

Inside one crate `use crate::anything` always compiles. Before the split ten
module pairs imported each other; the platform layer reached into the reactor
for `WindowId` and into the layout engine for `Direction`. A crate boundary
makes that a build error. Two further effects: a crate with no `objc2`
dependency is provably free of macOS types, and `cargo test -p rini-layout`
builds a few thousand lines of plain Rust instead of linking AppKit.

The costs: `pub(crate)` becomes `pub` at the boundary, the orphan rule forbids
`impl ForeignTrait for ForeignType` (newtype or move the impl), and shared test
helpers need a `test-support` feature.

## Where documentation lives

A finding lives in one place. Detail that belongs to one crate goes in that
crate's `docs/`; detail that spans crates stays in the repo-level `docs/`.
Code points at the doc by path when a reader would otherwise be stuck. When a
crate is lifted, the docs that describe only its code move with it.
