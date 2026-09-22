# Architecture: an application in `src/`, libraries in `crates/`

rini is one Cargo package with a workspace beside it.

`src/` is the application. `crates/` holds the libraries it is built out of, and
none of them knows what a window, a workspace or a layout is.

```text
src/app/          the application layer
src/windows/      windows and the apps that own them
src/displays/     screens, native spaces, coordinates
src/layout/       the scrolling strip
src/workspaces/   virtual workspaces and their persistence
src/input/        keys, bindings, gestures, drags
src/animation/    movement on screen

crates/rini-core/           identity types and file locations
crates/rini-geometry/       rect arithmetic
crates/rini-runloop/        the CFRunLoop executor, timers, span channels
crates/rini-ipc/            the wire language and its Mach transport
crates/rini-mach-sys/       raw Mach messaging
crates/rini-skylight-sys/   raw SkyLight/CGS declarations
crates/rini-cli/            the `rini-cli` binary
```

## The features

Each folder beside `app/` is a feature: a vertical slice that owns its model, its
macOS adapters, its settings and its tests. A feature splits the same way every
time.

| folder | what goes in it |
|---|---|
| `domain/` | the model, the value objects, the decisions, and the ports. No macOS, no reads of `platform/`. Provably free of AppKit |
| `platform/` | the adapters and the actors that drive them: Accessibility, the window server, CGEventTaps, Core Animation |
| feature root | `mod.rs` with the feature's public surface, `event.rs` with what it emits, `settings.rs` with what config fills in |

A feature with nothing to adapt has no `platform/`: `layout` is pure geometry and
touches nothing outside itself.

| feature | domain language | owns |
|---|---|---|
| `windows` | windows and the apps that own them | `AppInfo`, `WindowInfo`, `WindowServerInfo`, the window records, app rules and what counts as manageable, frame transaction ids, focus tracking (`MainWindowTracker` over `FocusEvent`) with the raise-echo and activation-focus rules, the raise manager, and the `Request`/`Quiet` port into the per-app thread. `platform/`: the per-app AX actor (observe, move, resize, raise, close), window-server reads (ids, order, levels), the Carbon front-app listener, the process watcher, the SkyLight sub-level port. Emits `windows::event::Event` |
| `displays` | screens, native spaces, coordinates, which windows sit on which space | `ScreenInfo`, `CoordinateConverter`, `usable_frame` (what the menu bar and Dock leave), the topology snapshot (`ForwardedSpaceState`) and screen selection over it, the space activation policy. `platform/`: CGDisplay/NSScreen/SLS-space adapters, space switching, display churn, the spaces actor, the cursor-warp actor, the CGS notification stream. Emits `displays::event::Event` |
| `layout` | strips, columns, the scrolling layout | the layout tree and its operations, gaps, insertion; `ScrollingLayoutSystem` is the seam; tiling settings |
| `workspaces` | virtual workspaces | assignment of windows to workspaces, activation, stacked-workspace geometry, `WindowStore`; `layout.ron` save/restore in `engine/persistence/`, launch memory, floating positions; workspace settings |
| `input` | what the user asked for | `Modifiers`, `KeyCode` and `Hotkey`, the token and spec parsing that needs no keyboard, the binding table, gesture and drag-swap recognition, and the rules the taps apply (`domain::gesture`, `domain::hotkey`). `platform/`: CGEventTap adapters, the layout-dependent `FromStr` impls (`platform::keyboard`), Carbon hotkeys, the haptic engine, the cursor. Emits `rini-ipc` commands, so the reactor cannot tell a hotkey from a CLI call |
| `animation` | movement on screen | flight plans, easing, z-bands, the workspace strip stack, the sorting of a layout pass (`domain::pass`), which final frame writes go out, the `AnimationRequest`/`SnapshotTarget` ports, and the flight engine's own decisions: when work happens (`domain::timing`), what a flight is and when it may capture (`domain::flight`), and how a request arriving mid-flight is admitted (`domain::admission`). `platform/`: window snapshots and capture (ScreenCaptureKit, SkyLight), the Core Animation tile overlay, the flight engine |

## The application layer

`src/app/` is what turns the features into rini. Nothing in a feature imports it.

| module | what it is |
|---|---|
| `reactor/` | the orchestrator. Holds every feature's store, reduces the events they emit (`reactor/events/`), and drives them back. `reactor/state.rs` is `RiniState`; `reactor/query.rs` answers from a borrowed `StateView`; `reactor/managers.rs` holds the handles |
| `reactor/commands.rs` | what a command means before it is carried out: which window "index 3" is, which display "next" is, what frame a move lands on |
| `reactor/observations.rs` | the other half of the inbox: what the system reported. Gathers what a reducer needs about a window, a space or a drag, and hands it over settled |
| `config/` | the `config.toml` file: parsing into each feature's settings type, validation that spans features, watching and reload |
| `api/` | the outward surface: the Mach IPC backend bound to the reactor (`api/backend.rs`) and the shapes a query answers in (`api/dto.rs`) |
| `notifications.rs` | the NSWorkspace demultiplexer, feeding the two features that want it |
| `hotkeys.rs` | the hotkey controller: lowers a `WmCmd` alias to a `Command` |
| `launch_agent.rs` | the launch agent plist and the service subcommands |
| `startup.rs` | the configured startup commands |
| `logging.rs` | tracing setup |
| `channels.rs` | the span-carrying channel every actor is wired with |

`src/main.rs` is the composition root: it parses the flags, takes the AX
permission, builds every actor and joins them.

## The rules

1. **A feature never names `crate::app`.** The application knows the features;
   the features do not know the application. A feature talks upward by emitting
   its own `event` type, and the reactor converts
   (`From<windows::event::Event> for reactor::Event`).
2. **`domain/` never touches macOS and never reads its own `platform/`.** A type
   the domain needs and an adapter fills is a port: it is declared in `domain/`
   and `platform/` imports it. `windows/domain/info.rs` and
   `windows/domain/request.rs` are those ports for `windows`;
   `animation/domain/request.rs` for `animation`.
   `objc2_core_foundation`'s `CGRect`/`CGPoint`/`CGSize` are exempt — they are
   the arithmetic every layout decision is in. So are the window server's plain
   value types (`SpaceId`, `WindowServerId`, `DisplayReconfigFlags`), which are
   vocabulary rather than API.
3. **Features depend on features in one direction.** `windows` is the most
   upstream: it knows nothing about screens. `displays` depends on it, because
   "which windows are on this space" is a window-server query over window ids.
   `layout` and `workspaces` sit above both; `input` and `animation` beside them,
   reaching only for ids and frames.
4. **Nothing in `crates/` knows what a workspace is.** A crate there is a
   library: identity, geometry, a run loop, a wire protocol, two FFI surfaces.
   `rini-cli` is the client binary and depends on `rini-core` and `rini-ipc`
   only — it cannot link the daemon, which is what keeps the wire language
   honest.
5. **Identity lives in `rini-core`.** `WindowId` is rini's own; `WindowServerId`
   (CGWindowID) and `SpaceId` (CGSSpaceID) are the window server's, declared in
   `rini-skylight-sys` and re-exported; `ScreenId` is the CGDirectDisplayID.
   `rini-ipc` has wire twins and `From` impls.
6. **No re-export shims.** When a type moves, its importers change.

`tests/architecture.rs` checks rules 1 and 2 against the tree on every
`cargo test`, with comments stripped so prose about `platform` is not a
dependency on it. One file is named as an exception, and a further test fails if
it stops needing to be. Rule 4 the compiler checks: a crate cannot name the
application's modules. Rules 3, 5 and 6 are conventions.

## Why this shape and not one crate per feature

The previous layout gave each feature its own crate. The dependency graph that
produced was a near-total order — `windows` was a dependency of eight of the
thirteen crates, `displays` of five — so a change to `windows` already rebuilt
everything downstream of it. The split bought almost no incremental compile time,
and it cost:

- a `pub` on every type another slice touches, because `pub(crate)` stops at the
  crate boundary;
- two `test-support` cargo features standing in for `cfg(test)`, because
  `cfg(test)` never fires across a crate boundary, plus a dev-dependency on each
  of them from the reactor's tests;
- `rini-config` and `rini-wm` sitting under everything and above nothing, which
  is a hub however the arrows are drawn;
- a repository with no application in it. `src/` was gone, so there was nowhere
  for "what rini does when it starts" to live.

The rules a crate boundary enforced are worth keeping; the boundary was not.
`tests/architecture.rs` enforces them instead, and the one thing crates gave for
free — a provable absence of macOS types — is what rule 2 now checks.

`layout` is the closest thing here to a genuinely reusable library: pure, with a
`ScrollingLayoutSystem` surface and no platform coupling. It stays in `src/` because nothing
else will ever use rini's scrolling layout, and it is the core domain rather than
a dependency of it.

## What is still not where the map says

| where | what | why it waits |
|---|---|---|
| `src/displays/screen.rs` | `ScreenCache` and the reads that fill it. The model left for `domain::screen` (`ScreenInfo`, `CoordinateConverter`, `menu_bar_inset`, `usable_frame`, the space ordering) | the `System` trait the cache is generic over is not a complete port: the generic `refresh_snapshot` calls `CGSManagedDisplayGetCurrentSpace` itself, and `ScreenCache.uuids` holds `CFRetained<CFString>`. Finishing it means widening the port to cover the space lookup and holding UUIDs as `String`, which changes the caching path rather than moving code. Named in `tests/architecture.rs` so nothing joins it |
| `src/animation/platform/engine.rs` | 2.2k code lines, 1.6k of them one `impl FlightEngine`, plus 3.9k lines of tests | the decisions have left (`domain/timing.rs`, `domain/flight.rs`, `domain/admission.rs`); what remains is the actor: the overlay, the snapshot cache, the timer, and the main-thread work they need. Its tests are still grouped by investigation episode (`exploration`, `render_stability_fix`, `still_passes`) rather than by unit, so they did not move with the decisions they cover |
| `src/app/config/` | one `Settings` struct that every feature's fields hang off, filled from one file | it depends on every feature and nothing depends on it, so it is a hub at the edge, which is the tolerable kind. Its own decomposition (one table per feature, `deny_unknown_fields` prevents `flatten`) is a schema question for the config file, not an architecture one |
| `src/workspaces/broadcast.rs` | `LayoutEngine` sends IPC events itself | belongs to `app/api/`, driven by `EventResponse`; needs the reactor to own the broadcast channel |
| `src/workspaces/engine.rs` | 3.3k code lines orchestrating workspaces, floating, persistence and display affinity. Its three dispatchers went from 908 lines to 482; what left them is named (`on_windows_on_screen_updated`, `move_window_to_workspace`, the three toggles) | the seam to `layout` is clean (`ScrollingLayoutSystem`). The largest remaining methods are `handle_command` (287), `calculate_layout_with_virtual_workspaces` (207), `move_window_to_workspace` (176) and `on_windows_on_screen_updated` (155). The first is 20 small arms and reads; the other three are single jobs that would need splitting on their own logic rather than on a dispatch boundary |
| `src/app/hotkeys/mod.rs` | lowers `WmCmd` aliases to `Command` | lowering them at parse time in `input` changes what a binding parses to |
| `src/app/notifications.rs` | feeds two features | it is the application's demultiplexer of NSWorkspace notifications; splitting it by feature is possible but doubles the subscriptions |
| `src/app/reactor/` | 5.3k lines in `mod.rs` plus `events/` (3.4k, half of it tests), still owning every feature's store | what did not need the reactor has already left: pure feature logic to its `domain/` (transactions, manageability, space activation, screen selection, focus rules, strip stack, frame writes, the layout-pass sort, app-rule follow-up `AfterRules`, the topology diff `analyze_space_snapshot`, the stale-window verdict `looks_gone`); the two focus actors (`RaiseManager`, `MainWindowTracker`) run in `windows` against an `EventSink`; cross-feature reads became borrowed views (`reactor::space_affinity::SpaceAffinity`, `reactor::query::StateView`). What remains is orchestration: `apply_event_outcome` (271 lines), `handle_layout_response` (299), `handle_authoritative_space_snapshot` (251), the strip-movement builders, and the `events/` reducers, which read and write several stores per event and so belong to the application. `dispatch_workflow` is no longer among them: it was 924 lines of arms that resolved their own targets inline, and is now a 280-line dispatch table over `commands.rs` and `observations.rs`. The next cut, if one is wanted, is the stores themselves: each feature's store owned by its own actor with the reactor holding handles, which changes the event model rather than moving code |

## The costs of this layout

`use crate::anything` always compiles, which is how the monolith that preceded
the crate split grew ten mutually importing module pairs. That is what
`tests/architecture.rs` is for, and why it is a ratchet rather than a style
guide: the one file that already breaks rule 2 is named in it, and nothing else
may join it.

`cargo test -p rini-geometry` still builds plain Rust instead of linking AppKit.
`cargo test` on the application does link it, so the win is now "this module has
no macOS in it, provably" rather than "this crate cannot link AppKit".

## Where documentation lives

A finding lives in one place, and that place is beside the code it is about.

Every feature and every crate has its own `docs/`, with a `README.md` explaining what
the module owns, the shape that matters, a reading order, and its known debt:

```text
src/<feature>/docs/README.md    what this feature is, and how to read it
src/<feature>/docs/<topic>.md   a finding that belongs to this feature
crates/<crate>/docs/README.md   same, for a library
docs/                           only what genuinely spans features
```

What is left at the top of the tree is cross-cutting: this file, `testing.md`,
`signing.md`, `permissions-and-the-launch-agent.md`, and the implementation audit.
Anything that names one feature belongs inside it.

Code points at the doc by path when a reader would otherwise be stuck. When code moves,
its docs move with it — and since a feature's docs now live inside the feature, moving a
folder moves both.
