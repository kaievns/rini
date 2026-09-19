# Architecture: one crate per bounded context

rini is a Cargo workspace. Each crate is a bounded context: a vertical slice
that owns its model, its macOS adapters, its persistence, its settings, its
tests and its docs. Crates are not layers. There is no "macOS crate", no
"pure logic crate" and no "shared crate"; a context that needs an AX call
makes it itself, behind its own API.

## The contexts

| crate | domain language | owns |
|---|---|---|
| `rini-displays` | screens, native spaces, coordinates, which windows sit on which space | `ScreenId`, `SpaceId`, `ScreenInfo`, coordinate conversion, the topology snapshot (`ForwardedSpaceState`); CGDisplay/NSScreen/SLS-space adapters, space switching, display churn; the spaces actor and the cursor-warp actor. Emits `displays::Event` |
| `rini-windows` | windows and the apps that own them | `WindowId`, `WindowServerId`, `pid_t`, `AppInfo`, `WindowInfo`, the window catalogue (`WindowStore`), app rules and what counts as manageable; the per-app AX actor (observe, move, resize, raise, close), window-server reads (ids, order, levels), the Carbon front-app listener; app-rule and snapping settings. Emits `windows::Event` |
| `rini-tiling` | strips, columns, the scrolling layout | the layout tree and its operations, gaps, insertion; pure, no adapters; tiling settings |
| `rini-workspaces` | virtual workspaces | assignment of windows to workspaces, activation, stacked-workspace geometry; `layout.ron` save/restore, launch memory, floating positions; workspace settings |
| `rini-input` | what the user asked for | key specs, the binding table, gesture and drag-swap recognition; CGEventTap adapters, Carbon hotkeys; key and gesture settings. Emits `rini-ipc` commands, so the reactor cannot tell a hotkey from a CLI call |
| `rini-animation` | movement on screen | flight plans, easing, z-bands; window snapshots and capture (ScreenCaptureKit, SkyLight), the Core Animation tile overlay, the flight engine; animation settings |
| `rini-ipc` | the command, query and event language | the wire types, the Mach server, subscriptions, CLI exec, and the client library |
| `rini-config` | the `rini.toml` file | parsing into each context's settings type, validation that spans contexts, watching and reload. Depends on every context; nothing depends on it |
| `rini-wm` | the application | the reactor as orchestrator over the contexts, wiring, the `rini` and `rini-cli` binaries. Depends on everything; nothing depends on it |

Technical libraries, each doing one thing and containing no domain:

| crate | contents |
|---|---|
| `rini-runloop` | the CFRunLoop executor, timers, and the span-carrying channel every actor uses |
| `rini-skylight-sys` | raw declarations for the private SkyLight/CGS API, and the two identities it mints: `WindowServerId` (CGWindowID) and `SpaceId` (CGSSpaceID). No policy |
| `rini-geometry` | rect arithmetic, rounding, tolerance comparison, serde adapters for CoreFoundation geometry |

## Dependency rules

1. A context depends on technical libraries and on the published language of
   the contexts upstream of it. `windows` is the most upstream context: it
   knows nothing about screens. `displays` depends on it, because "which
   windows are on this space" is a window-server query over window ids and the
   windows context owns those reads. `tiling` and `workspaces` sit above both;
   `input` and `animation` beside them, depending on `windows` and `displays`
   only for ids and frames.
2. `config` and `wm` depend downward on everything. Nothing depends on them.
3. No crate is both widely depended on and widely dependent. A crate that
   every context reads and that knows every context's vocabulary is a hub,
   and every cut crosses a hub. `rini-config` was one: it sat under the layout
   for settings and above the platform for hotkey parsing. It is inverted:
   each context owns its settings type, config fills them.
4. A context talks upward by emitting its own event type. The reactor
   converts (`From<windows::Event> for reactor::Event`). A context never
   imports the reactor.
5. Identity lives with whoever mints it: `WindowId` is rini's own and lives in
   `rini-windows`; `WindowServerId` and `SpaceId` are the window server's and
   are declared in `rini-skylight-sys`, re-exported by the context that speaks
   them (`rini_windows::ids`, `rini_displays::ids`). `rini-ipc` has wire twins
   and `From` impls.
6. No re-export shims. When a type moves, its importers change.

The compiler enforces the direction: a cycle between crates does not build.
Inside one crate `use crate::anything` always compiles, which is how the
monolith grew ten mutually importing module pairs.

## Current state

The first attempt at the split cut by concern, not by context, and produced
layers: `rini-shared`, `rini-macos`, `rini-config` (a hub), `rini-layout`
(five contexts in one crate), `rini-motion`/`rini-overlay` (one context in
two layers). Those crates exist today and dissolve into the contexts above,
one context at a time. Each step: the context's crate appears with its full
slice, importers repoint, the old crate shrinks, tests move with the code.

| step | status |
|---|---|
| `rini-runloop`, `rini-skylight-sys` | done: lifted out of `rini-macos` (and `channel` out of `rini-shared`) so `rini-windows` depends on libraries, not a layer |
| `rini-windows` | done, first cut: `ids`, `state`, `rules` (matching; the settings types `AppWorkspaceRule`/`AppRulePosition`/`AppRuleSize` moved here from config, the first piece of the config inversion), `transaction`, `event` (`Event` + `EventSink`), the AX adapters (`ax`, `app`), `process`/`carbon`, `mouse`, `window_server`, the per-app `app_actor` and the Carbon `lifecycle` actor. Still to come here: the window catalogue (`WindowStore` stays in `rini-workspaces` until the assignment index is taken out of it), the SkyLight notification actor (`src/actor/window_notify.rs`, straddles windows and displays), `raise_manager`, and the window sub-level Mach query (`rini_macos::mach`). `MouseState` lives here because the app actor stamps its events with it; the event tap writes it |
| `rini-displays` | done: `ids`, `screen` (cache, `ScreenInfo`, `CoordinateConverter`, SLS display/space queries), `topology` (`ForwardedSpaceState`, `TopologyWindowDelta`, `SpaceEventKind`), `space_query`, `space_switch`, `display_churn`, the spaces actor (inbound `Notification`, outbound `Event` through an `EventSink`; the application routes the topology snapshot to the event tap as well) and `cursor_warp` (owns `StackedUpperSide`, which config re-exports). `rini-shared::ids` is gone. Still in `rini-wm`: `notification_center` (also feeds app lifecycle and power events) and `mission_control_observer` (an AX observer on the Dock); in `rini-macos`: `focus_desktop_window`, `mission_control_dock_overlay_visible` |
| `rini-tiling` | done: `LayoutSystem`/`LayoutId`/`LayoutSystemKind`, `ScrollingLayoutSystem`, constraints, the tiling area, and `settings` (`LayoutSettings`, `ScrollingLayoutSettings`, `GapSettings` and kin, moved out of config, which re-exports them). No space or workspace in it |
| `rini-workspaces` | done, first cut: `rini-layout` renamed and given its settings (`VirtualWorkspaceSettings`, `MAX_WORKSPACES`, out of config); the `crate::layout_engine` and `crate::model` re-export shims in `rini-wm` are gone. Still inside it, each its own step: the `WindowStore` catalogue half (to `rini-windows`; the assignment index stays), `broadcast` (the engine sends IPC events itself; that belongs to the application, driven by `EventResponse`), and `LayoutEngine` itself, which orchestrates workspaces, floating, persistence and display affinity in one 4.4k-line type |
| `rini-input` | dissolves the rest of `rini-macos` with `displays` |
| `rini-animation` | merges `rini-motion` and `rini-overlay` |
| `rini-ipc` | merges `rini-protocol`, `rini-client`, `rini-ipc`, `macos::mach` |
| `rini-config` inversion | settings types move to their contexts; `WmCommand` splits by context |
| `rini-wm` | the reactor becomes a dispatcher over the contexts. Largest single job; last |

## The costs of crates

`pub(crate)` becomes `pub` at the boundary; the orphan rule forbids
`impl ForeignTrait for ForeignType` (newtype or move the impl); test helpers
another crate needs sit behind a `test-support` cargo feature, because
`cfg(test)` never fires across crates. In exchange, a crate with no `objc2`
dependency is provably free of macOS types, and `cargo test -p rini-tiling`
builds plain Rust instead of linking AppKit.

## Where documentation lives

A finding lives in one place. Detail that belongs to one context goes in that
crate's `docs/`; detail that spans contexts stays in the repo-level `docs/`.
Code points at the doc by path when a reader would otherwise be stuck. When
code moves between crates, its docs move with it.
