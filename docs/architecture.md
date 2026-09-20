# Architecture: one crate per bounded context

rini is a Cargo workspace. Each crate is a bounded context: a vertical slice
that owns its model, its macOS adapters, its persistence, its settings, its
tests and its docs. Crates are not layers. There is no "macOS crate", no
"pure logic crate" and no "shared crate"; a context that needs an AX call
makes it itself, behind its own API.

## The contexts

| crate | domain language | owns |
|---|---|---|
| `rini-displays` | screens, native spaces, coordinates, which windows sit on which space | `ScreenId`, `SpaceId`, `ScreenInfo`, coordinate conversion, the topology snapshot (`ForwardedSpaceState`) and screen selection over it, the space activation policy; CGDisplay/NSScreen/SLS-space adapters, space switching, display churn; the spaces actor and the cursor-warp actor. Emits `displays::Event` |
| `rini-windows` | windows and the apps that own them | `WindowId`, `WindowServerId`, `pid_t`, `AppInfo`, `WindowInfo`, the window catalogue (`WindowStore`), app rules and what counts as manageable, frame transaction ids, focus tracking (`MainWindowTracker` over `FocusEvent`) with the raise-echo and activation-focus rules, the raise manager; the per-app AX actor (observe, move, resize, raise, close), window-server reads (ids, order, levels), the Carbon front-app listener; app-rule and snapping settings. Emits `windows::Event` |
| `rini-tiling` | strips, columns, the scrolling layout | the layout tree and its operations, gaps, insertion; pure, no adapters; tiling settings |
| `rini-workspaces` | virtual workspaces | assignment of windows to workspaces, activation, stacked-workspace geometry; `layout.ron` save/restore, launch memory, floating positions; workspace settings |
| `rini-input` | what the user asked for | key specs, the binding table, gesture and drag-swap recognition; CGEventTap adapters, Carbon hotkeys; key and gesture settings. Emits `rini-ipc` commands, so the reactor cannot tell a hotkey from a CLI call |
| `rini-animation` | movement on screen | flight plans, easing, z-bands, the workspace strip stack, the sorting of a layout pass (`pass`: what moves, what stays, what warms), which final frame writes go out; window snapshots and capture (ScreenCaptureKit, SkyLight), the Core Animation tile overlay, the flight engine; animation settings |
| `rini-ipc` | the command, query and event language | the wire types, the Mach server, subscriptions, CLI exec, and the client library |
| `rini-config` | the `rini.toml` file | parsing into each context's settings type, validation that spans contexts, watching and reload. Depends on every context; nothing depends on it |
| `rini-wm` | the application | the reactor as orchestrator over the contexts, the macOS notification demultiplexers, the launch agent, wiring, and the `rini` binary. Depends on everything; nothing depends on it |
| `rini-cli` | the command-line client | a `clap` front end over `rini-ipc`'s client; depends on the wire language and on `rini-config` for file locations, never on `rini-wm` |

Technical libraries, each doing one thing and containing no domain:

| crate | contents |
|---|---|
| `rini-runloop` | the CFRunLoop executor, timers, and the span-carrying channel every actor uses |
| `rini-skylight-sys` | raw declarations for the private SkyLight/CGS API, and the two identities it mints: `WindowServerId` (CGWindowID) and `SpaceId` (CGSSpaceID). No policy |
| `rini-mach-sys` | raw Mach messaging: `mach_msg`, ports, bootstrap lookup, message helpers. Used by `rini-ipc` for rini's service and by `rini-windows` for SkyLight's sub-level port |
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

Every context in the table above exists as its own crate, and the layers the
first attempt at the split produced (`rini-shared`, `rini-macos`,
`rini-layout`, `rini-motion`/`rini-overlay`, `rini-protocol`/`rini-client`)
are gone. Each cut kept behaviour and test counts; the commit messages record
what moved where and the inversions that made it possible (`Event` enums with
an `EventSink`, `From<&Config>` for a context's settings, `Backend` speaking
wire types).

`src/` is the application: the reactor and its reducers, tests and query view
(`actor/reactor/`, 16.5k lines of which 7.9k are tests), the NSWorkspace
demultiplexer (`notification_center`), the hotkey controller (`wm_controller`),
the query DTOs (`model/server`), the launch agent (`platform/service`), the IPC
backend, startup, logging, and `bin/rini.rs`. No context adapter or model
lives there any more.

What is still not where the map says:

| where | what | why it waits |
|---|---|---|
| `rini-config` | is a hub in shape: one `Settings` struct that every context's fields hang off, filled from one file | it now depends on every context and nothing depends on it, so it is a hub at the edge, which is the tolerable kind. Its own decomposition (one table per context, `deny_unknown_fields` prevents `flatten`) is a schema question for the config file, not an architecture one |
| `rini-workspaces` | `broadcast`: `LayoutEngine` sends IPC events itself | belongs to the application, driven by `EventResponse`; needs the reactor to own the broadcast channel |
| `rini-workspaces` | `LayoutEngine`, 4.4k lines orchestrating workspaces, floating, persistence and display affinity | the seam to tiling is clean (`LayoutSystem`); the seams inside are not yet |
| `rini-wm` | `wm_controller` lowers `WmCmd` aliases to `Command` | lowering them at parse time in `rini-input` changes what a binding parses to |
| `rini-wm` | `notification_center` feeds two contexts | it is the application's demultiplexer of NSWorkspace notifications; splitting it by context is possible but doubles the subscriptions. (`window_notify` moved to `rini-displays` with a windows sink and a displays sink.) |
| `rini-wm` | the reactor: 5.9k lines plus `events/` (2.9k), still owning every context's store | the decomposition so far moved what did not need the reactor: pure context logic to its crate (transactions, manageability, space activation, screen selection, focus rules, strip stack, frame writes, the layout-pass sort `rini_animation::pass`, app-rule follow-up `AfterRules`, the topology diff `analyze_space_snapshot`, the stale-window verdict `looks_gone`); the two focus actors (`RaiseManager`, `MainWindowTracker`) run in `rini-windows` against `EventSink`/`FocusEvent`; cross-context reads became borrowed views inside `rini-wm` (`space_affinity::SpaceAffinity`, `query::StateView`). What remains is orchestration: `dispatch_workflow`, `apply_event_outcome`, the layout-response and space-snapshot handlers, the strip-movement builders, and the `events/` reducers, which read and write several stores per event and so belong to the application. The next cut, if one is wanted, is the stores themselves: each context's store owned by its own actor with the reactor holding handles, which changes the event model rather than moving code |

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
