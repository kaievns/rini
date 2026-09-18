# Architecture: crates by bounded context

rini is a Cargo workspace. Each crate is one bounded context with a hard
boundary, and dependencies point one way. The compiler enforces the direction:
a cycle between crates does not build, which is the reason the split is by
crate and not by module (see "Why crates").

## Target crate graph

```
rini-protocol   wire types for the Mach IPC API (exists)
rini-shared     ids, collections, geometry, log, channel (see below)     -> protocol
rini-macos      sys/: AX, SkyLight, event tap, run loop, executor, mach   -> shared
rini-config     config file, validation, hot reload, bindable commands   -> shared, macos, protocol
rini-layout     scrolling strip, virtual workspaces, window records,
                app rules, layout.ron persistence, display affinity      -> shared, config, macos (for now)
rini-motion     animation plans, easing, strip geometry, z-groups (pure) -> shared
rini-overlay    CA tile renderer, snapshot cache and capture, flights    -> shared, macos, motion
rini-ipc        mach server, subscriptions, cli_exec; `Backend` trait     -> protocol, macos, config
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
| `rini-shared` | done: `ids`, `collections`, `geometry`, `log`, `util`, `channel` |
| `rini-macos` | done: the former `src/sys/`. Its `test-support` feature swaps the window-server reads for thread-local overrides (see `docs/testing.md`) |
| `rini-config` | done: `Config` schema/parse/validate/save, the `ConfigActor` (takes an `OnChange` callback instead of the reactor's channel), the file watcher, and `commands` (`WmCommand`, `WmCmd`, `ExecCmd`, `Command`: everything a key can be bound to) |
| `rini-layout` | done: the layout engine, `virtual_workspace`, `window_store` (the window record store the layout is projected from, incl. `WindowState`), `app_rules`, `display_affinity`, `launch_memory`, `floating_position_store`, `hidden_window_placement`, `broadcast`. Depends on `rini-macos` for `WindowInfo`/`AppInfo`/`ScreenInfo`; making it platform-free means moving those to a neutral home first. `test-support` feature exposes test-only accessors |
| `rini-ipc` | done: the Mach server, subscriptions and CLI hooks. It sees the window manager through the `Backend` trait, which `rini-wm` implements for `ReactorHandle`; the protocol conversions live in that impl |
| `rini-motion` | done: `z_group` (bands are `Tiled`/`Floating`), `fit` (picture/frame predicates), `surface` (a fixed surface under a travelling viewport, `TileGeometry`, `SurfaceWindow`), `plan` (rigid-group flight plans and mid-flight merging, `RigidGroup`/`GroupKey::{Rigid, Loose, Floating}`), `easing`, `tile`, `travel`. The park predicate `is_off_screen` moved to `rini_shared::geometry` so motion does not depend on layout |
| `rini-overlay` | done: `engine` (`FlightEngine`, the former `workspace_animation` actor; the reactor reaches it through `Event`s and it places real frames through a `PlaceFrames` callback), `overlay` (`TileOverlay`, the Core Animation tile window), `window_snapshot`, `snapshot_service`, `edge_dressing`. Its docs are `crates/rini-overlay/docs/{animation-smoothness,capture-overlay-research}.md` |
| everything else | still modules inside the `rini-wm` crate |

Inside `rini-wm`, `crate::layout_engine` and `crate::model` re-export `rini_layout`,
`crate::actor::app::WindowId` and `crate::actor::{Sender, Receiver, channel}` re-export
`rini_shared`, `model::reactor::Command` and `wm_controller::{WmCommand, WmCmd, ExecCmd}`
re-export `rini_config`, and `rini_macos::window_server::WindowServerId` and
`rini_macos::screen::SpaceId` re-export `rini_shared::ids`. They exist so brace imports keep
compiling until each module is lifted, and go away with it.

## IPC is one context in three crates

`rini-protocol`, `rini-client` and `rini-ipc` are one bounded context, the Mach
IPC API, split because they sit at different heights of the graph. The protocol
is a leaf and `rini-shared` depends on it. The client must depend on nothing but
the protocol, so a companion tool can link it without the window manager. The
server needs the reactor's query handle, the config actor and `macos::mach`, so
it sits just under `rini-wm`. Merged, the three would form a cycle.

## What belongs in `rini-shared`, and why it should shrink

Types every crate agrees on and nothing that touches a window or a display. It
exists because these had no better home while the split is in progress; the
intent is to dissolve it once the domain crates exist. Likely destinations:
`ids` and the `Direction` re-exports to `rini-protocol` (`WindowId` already has
a wire twin there), `geometry` to `rini-macos`, and `channel`, `log`,
`collections` to whichever crate is left using them.

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
