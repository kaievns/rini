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
src/input/        keys, bindings, drags
src/decorations/  what a window wears: its hairline, shadow, focus ring and border
src/pictures/     pictures of windows, the desktop and the bar
src/animation/    how things move on screen
src/transitions/  when things happen in a transition
src/switcher/     the window switcher popup
src/bar/          the menu bar across the top of every display

crates/rini-core/           identity types and file locations
crates/rini-geometry/       rect arithmetic and coordinate spaces
crates/rini-runloop/        the CFRunLoop executor, timers, span channels, turns timed
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
| `windows` | windows and the apps that own them | `AppInfo`, `WindowInfo`, `WindowServerInfo` and the one corner radius a window's reported corners round at (`domain::info`), the window records, app rules, the matcher they share with the rules for what a window wears, which reads whether a window holds focus, floats or is drawn urgent as well as what it is (`domain::rules::applies`), and what counts as manageable, frame transaction ids, which Accessibility writes a frame takes (`domain::frame_write`), when a resize is an app's title-bar zoom (`domain::zoom`), focus tracking (`MainWindowTracker` over `FocusEvent`) with the raise-echo and activation-focus rules, the raise manager, the strip regroup (`domain::raise_order`: when a raise has to lift the strip back in front of a window off it, over `StackGroup`), which running applications have their per-app thread (`domain::app_threads`), which applications ask for attention (`domain::attention`), and the `Request`/`Quiet` port into it. `platform/`: the per-app AX actor (observe, move, resize, raise, close), window-server reads (ids, order, levels, corner radii), each that waits on Core Animation on a connection of rini's own (`READ_CONNECTION`), and the level and sub-level one of rini's own windows takes, the Carbon launch, quit and front-app listener, the activation-policy and finished-launching observers, the process-type check, the SkyLight sub-level port, the thread that reads from LaunchServices which applications ask for attention. Emits `windows::event::Event` |
| `displays` | screens, native spaces, coordinates, which windows sit on which space | `ScreenInfo`, `CoordinateConverter`, `usable_frame` (what the menu bar, a band reserved at the top, and the Dock leave), the topology snapshot (`ForwardedSpaceState`) and screen selection over it, the space activation policy, which windows a burst of move and resize notifications names (`domain::frames`). `platform/`: CGDisplay/NSScreen/SLS-space adapters, display churn, the spaces actor, the cursor-warp actor, the CGS notification stream. Emits `displays::event::Event`. Reads the window server for the application on one frames thread, in the order asked through `Following`'s queue (`ToRead`): each moved window's frame, each window it reorders, hides, unhides or closes with the level and sub-level after a reorder or an unhide (`ServerOrder`, `StackLevel`), and each window the application introduces, whole (`ServerIntro`), handed to the callbacks the application wires (`OnServerRead`), while it is `Following` them |
| `layout` | strips, columns, the scrolling layout | the layout tree and its operations, gaps, insertion; a tiled window inside its slot, the border each window wears between them (`domain::tile`); `ScrollingLayoutSystem` is the seam; tiling settings |
| `workspaces` | virtual workspaces | assignment of windows to workspaces, activation, stacked-workspace geometry, `WindowStore`; `layout.ron` save/restore in `engine/persistence/`, launch memory, floating positions; workspace settings |
| `input` | what the user asked for | `Modifiers`, `KeyCode` and `Hotkey`, the token and spec parsing that needs no keyboard, what a bind names (`domain::binding`, the config's actions and their knus decoding), the binding table (`domain::hotkey`), which keys are held (`domain::held_keys`), the switcher's session (`domain::switch_session`), drag-swap recognition, and the order the tap asks them in for each key event, with what it swallows (`domain::router`). `platform/`: the CGEventTap adapter, the layout-dependent `FromStr` impls (`platform::keyboard`, reading the layout through Carbon's input sources), the cursor. Emits `input::event::Event`: a bind fires its `Action`, which the application lowers to the `rini-ipc` command a CLI call would send (`app/hotkeys/lower`), so the reactor cannot tell a hotkey from a CLI call. Hands every left press to a callback the application wires (`OnLeftPress`) |
| `decorations` | what a window wears | the hairline's, the shadow's, the focus ring's and the border's shape: the corner radius a tile is drawn with, reported or the fallback, the outline's rect, the shadow's silhouette and its mask, a band outside the window, the width each band is laid at in device pixels and how far they reach, and the continuous corner every path is drawn on (`domain::shape`); the config's colours and gradients, and each gradient's axis and stops as Core Animation takes them (`domain::paint`); what each `window-rule` says a window wears, folded over the layout's settings for one window whenever it is asked for (`domain::rules`); their paint by focus and attention, what a window wears that rini draws, the corner a rule gives its bands among it, and through a pass (`Decoration`, `Worn`), and who wears the ring and who is drawn urgent (`Rings`) (`domain::style`); and how deep each sits against its picture (`domain::stack`); at rest, which windows show a band and when, which are asked of the window server as the model first names them, what waits from a flight's begin for its place ahead of the lift, how many rest windows each display keeps, by its fullest workspace, which rest window draws each and what it changes to draw its look, which are ordered again and when one takes its window's level (`domain::rest`); the ring's and the border's settings and the rules', and the width a tiled window gives up to `layout › border` (`settings`). `platform/`: the Core Animation layers each tile wears them on, the ring and the border among them, each only while its window wears it, in `CGColor`s made once a colour (`Bands`), and what a resize animates on them (`platform::layers`, over the `Resize` trait the overlay fills from a tile's track); the main-thread actor that draws the bands at rest in windows of rini's own above the windows they decorate, from what the frames thread read of the window server (`platform::rest`). Reads `windows` and `displays` |
| `pictures` | pictures of windows, the desktop and the bar | whether a capture covers its window and fits the frame it is drawn into (`domain::coverage`), the picture cache and its rules (`domain::cache`), where a window lies in a batched composite (`domain::region`), what is known of each window, its translucency and material (`domain::knowledge`), whether two pictures of a window show the same thing (`domain::print`), the colour a window's content sits on (`domain::ground`), the blur merge (`domain::translucency`), how far the screen after a lift is from the overlay before it (`domain::handover`), when a window has finished repainting (`domain::settle`), which window flies the stand-in (`domain::stand_in`), the desktop and bar levels (`domain::backdrop`), the `PictureTarget` port (`domain::target`), and the capture schedule: which call comes next, held off while a transition runs, a handover check's two pictures first (`domain::schedule`), the windows on screen a transition asks for and watches land, change and stay live, delivered under its tag (`domain::watch`), and the windows pictured afresh at rest once their look has turned, through SkyLight where one is wholly on the display and ScreenCaptureKit elsewhere (`domain::refresh`). `platform/`: the pictures service, its thread and the one a handover check is compared on, the cache, each window's focused look and the `Pictures` handle every client holds (`platform::service`), the `Picture` type and its stand-in, every SkyLight and `CGWindowList` capture but the bar's own menu extras and ground (`bar/platform/menu_extras.rs`), the batched capture of the windows on screen, alpha and baked, and a handover check's overlay alone and whole screen (`platform::skylight`), a picture's band print, body samples and ground, what a window's first picture says of it, and a picture drawn whole for a handover check (`platform::measure`), the ScreenCaptureKit listing, capture and desktop render, the window-server listings of the desktop and the bar, bitmaps, and handing a picture to a layer. Emits `pictures::event::Event` to the sinks the application wires |
| `animation` | how things move on screen | rigid pieces and how a pass arriving mid-flight merges into them (`domain::motion::plan`), what a strip movement carries (`domain::motion::surface`), legs, easing, the score, its edits and the media clock its tracks begin on (`domain::motion::score`), the z-bands, the edge bounce's and a move's nudge's shapes, the accents a press asks for and the tracks they play, a step only while none plays and a push from where each container's bounce is drawn (`domain::motion::accents`), how a tile fits its picture, cropped or, where scaling is exact, stretched, with its window's ground where the picture ends, a tile's leg, in one movement or, growing on one axis as it shrinks on the other, two steps turning at a corner, and snaps onto its window's real frame (`domain::motion::tile`), where the overlay is holed over a window that ends on screen (`domain::motion::holes`), and the looks: each one's staging of a press on the strip (`Look::stage`, over the press's `Cue`) and the `Choreography` that composes a flight's score, folds a press before its motion and bends it at a merge (`domain::choreography`, Slide the default). `platform/`: the renderer, the main thread's actor, which plays the flight engine's batches of render commands on the Core Animation tile overlay, one transaction a drain, answers each drain (`event`), and reads the media clock (`media_clock`) |
| `transitions` | when things happen in a transition | the `AnimationRequest`, `SurfaceWindow` and `Cause` port (`domain::request`), the sorting of a layout pass and each tiled window's place on the strip before and after it (`domain::pass`), where each tile starts and ends (`domain::travel`), where a move along the strip starts its pair, and the look's staging of it put on the surface (`domain::strip_move`), the workspace strip stack (`domain::strip_stack`), how a request arriving mid-flight is admitted (`domain::admission`), what a flight is (`domain::flight`), a flight's score and the accents waiting for its motion (`domain::flight_score`), what waits for a batch of render commands to be drawn (`domain::drawn`), the constants work is timed by (`domain::timing`), which final frame writes go out (`domain::frame_writes`), the lifecycle every transition runs through as a pure state machine, with when each tile is drawn focused and wears its ring (`domain::lifecycle`), and its journal: the press each request answers, every command the lifecycle gives and every event it takes but a picture and frame zero, a tick only by the first `at_rest` and `landed` it reads in a generation, every delivery of pictures to it, every drain the renderer commits, each picture a tile takes, each held lift's hide and handover and each turn of the main thread over a frame, a mark in a ring kept with the coordinator, the burst due once a transition settles, and which lifts are held for the overlay's picture and when one hides (`domain::journal`); whether the journal is written and the handover measured (`settings`). `platform/`: the flight engine, the actor on the `transitions` thread that holds the lifecycle with its journal and carries out its commands on `animation`'s renderer, on the main thread, and `pictures`' service, and the `journal` thread that writes each burst as JSON lines. Driven by the reactor; made from the `Parts` the application wires: the `Draw` callback to the renderer, whose answers come back as `Event::Rendered`, the `PlaceFrames` and `OnFlight` callbacks it answers through, the renderer's media clock every track begins on, and the clock it reads every instant from |
| `switcher` | the window switcher: every window, by application or one by one, in the order you last used them | who a switch offers and in what order (`domain::candidates`), what a switch steps between, a window or an application's stack (`domain::entries`), the cursor over the list (`domain::selection`), each switcher's session keys (`domain::trigger`), where the rows and a stack's cards go and the hit test (`domain::layout`), how the ring moves (`domain::motion`), when the popup appears (`domain::reveal`); switcher settings. `platform/`: the popup `NSPanel` and its layer tree, and the main-thread actor that draws it. Focus order is `windows`' and is read here |
| `bar` | the menu bar across the top of every display | what each display's bar shows (`domain::model`: `BarInput` in, `BarModel` out, `Action` back), where each piece goes by its ink and the hit test (`domain::layout`), how each piece is set (`domain::style`, `domain::palette`), which menu extras are drawn (`domain::extras`), the words (`domain::format`, `domain::glyphs`); bar settings. `platform/`: the main-thread actor that owns every display's bar window, and the thread that pictures the menu extras. Sent its model by the reactor, never asks |

## The application layer

`src/app/` is what turns the features into rini. Nothing in a feature imports it.

| module | what it is |
|---|---|
| `reactor/` | the orchestrator. Holds every feature's store, reduces the events they emit (`reactor/events/`), and drives them back. `reactor/state.rs` is `RiniState`; `reactor/query.rs` answers from a borrowed `StateView`; `reactor/managers.rs` holds the handles |
| `reactor/commands.rs` | what a command means before it is carried out: which window "index 3" is, which display "next" is, what frame a move lands on |
| `reactor/observations.rs` | the other half of the inbox: what the system reported. Gathers what a reducer needs about a window, a space or a drag, and hands it over settled |
| `config/` | the `config.kdl` file: decoded section by section into part types (`config/kdl/`), laid over each feature's settings type with the checks that span sections, watching and reload. The format's decisions are `docs/config.md` |
| `api/` | the outward surface: the Mach IPC backend bound to the reactor (`api/backend.rs`) and the shapes a query answers in (`api/dto.rs`) |
| `notifications.rs` | the NSWorkspace demultiplexer, feeding the two features that want it |
| `power.rs` | Low Power Mode, read at start and kept current from its notification: the input tap and the reactor's flight decision read it |
| `system_reads.rs` | `SystemReads`, everything the reactor asks of the machine as one value: the window-server port, the pointer, Low Power Mode, display churn, the space kinds, the running applications, and the activity clock and window notifications it hands back. `Reactor::new` builds the live one, a test reactor a fake that knows nothing |
| `hotkeys/` | the hotkey controller: lowers a bound `Action` to a `Command` |
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
   `pictures/domain/target.rs` for `pictures`; `animation/domain/motion/surface.rs`
   (`SurfaceGeometry`) for `animation`; `transitions/domain/request.rs` for `transitions`.
   `objc2_core_foundation`'s `CGRect`/`CGPoint`/`CGSize` are exempt for the
   features not yet on rini-geometry's spaces, where they are the arithmetic
   every layout decision is in; `TYPED` in `tests/architecture.rs` lists those
   that are, and outside their `platform/` they name no CG geometry and convert
   to or from it nowhere. So are the window server's
   plain value types (`SpaceId`, `WindowServerId`, `DisplayReconfigFlags`), which
   are vocabulary rather than API.
3. **Features depend on features in one direction.** `windows` is the most
   upstream: it knows nothing about screens. `displays` depends on it, because
   "which windows are on this space" is a window-server query over window ids.
   `layout` and `workspaces` sit above both; `input` and `animation` beside them,
   reaching only for ids and frames, and for `animation` the stacking group a window is in
   (`windows::domain::raise_order::StackGroup`) and what each tile wears (`decorations`).
   `decorations` reads `windows` (a place's frames, the calls a rest window takes a level and
   sub-level with, and the window rules' matcher) and `displays` (what its frames thread reads,
   `ServerRead`: frames, orders with their levels and introductions with radius and ordered-in, in
   the order asked, and `Following` to ask it), and draws at rest by itself. `animation` draws with it, and `transitions` names
   `decorations::domain::style` to carry each window's `Worn` from the reactor to its tile,
   and `decorations::domain::shape::outset` for how far its bands reach when it holes the overlay;
   the overlay paints each tile's bands as `Worn::style` gives them, where the flight ends, from its
   show.
   `layout` takes the border each tiled window wears as plain widths the application hands it, so it
   names `decorations` no more than anything else.
   `pictures` reads the window server through `windows` and is upstream of `animation`, `transitions`, `switcher` and `bar`:
   a tile asks it whether its picture still fits, the flight engine and the
   switcher hold its service's handle, and the renderer, the switcher and the bar
   draw with its layer and bitmap helpers. `transitions` sits above `pictures` and
   `animation`: its engine plays each flight on `animation`'s renderer with
   `pictures`' pictures, and the reactor drives it; neither of the two names it.
   `switcher` and `bar` are the most
   downstream: they draw what the reactor sends them, reading `windows`,
   `displays`, `workspaces` and `input` where they need to. `switcher` takes its colours from
   `bar::domain::palette`, and no other feature depends on either.
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
7. **A change leaves nothing behind.** Moving functionality means the old path is
   gone, not unreachable: no branch that can no longer be taken, no helper with
   one caller doing what its replacement does, no state written and never read,
   no second function computing what the first one already decided. The bug this
   answers was exactly that shape — `ctrl-R` wrote a column width while a
   separate maximise flag made the layout ignore it, so two records disagreed and
   the key looked dead. When a fix reveals a stale sibling, the sibling is part of
   the fix.

`tests/architecture.rs` checks rules 1 and 2 against the tree on every
`cargo test`, with comments stripped so prose about `platform` is not a
dependency on it. One file is named as an exception, and a further test fails if
it stops needing to be. `a_typed_feature_names_no_cg_geometry_outside_platform`
holds each feature in `TYPED` to rini-geometry's spaces outside its `platform/`, no CG geometry and
no `from_cg`, `to_cg`, `FromCg` or `ToCg` there, and keeps `cast_unit` in `src/` and `crates/` to
`Respace`'s two impls. `transitions_never_touches_core_animation_or_app_kit` keeps Core Animation,
AppKit, the main-thread markers, `TileOverlay` and the renderer's `media_clock` out of every code
line in `src/transitions/`, whose engine runs on its own thread, reaches the overlay only by message
and reads the media clock it is handed. Rule 4 the compiler checks: a crate cannot name the
application's modules. `cargo build` catches the crudest breaches of rule 7 —
an unread field or an unreachable branch is a warning, and the build carries
none. Rules 3, 5, 6 and the rest of 7 are conventions.

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
| `src/transitions/platform/engine.rs` | the flight engine, past the file limit and with `start`, `merge_now` and `start_surface` past the function limit, each held at its size by `no_function_or_file_grows_past_its_limit` | beside the actor (the renderer it hands each event's batch, the frame clock, the coordinator's commands carried out on the renderer and the pictures service, and the window-server reads and the spawn capture they need) the pure rules that remain are over a `RunningAnimation`, an `OverlayTile` or a `Picture` (merges, entrances, frame zero's pictures, the focus handover, the tiles the coordinator hears of, depth, corner clips, focused looks), and stay because each carries a platform `Picture`: they and their tests, grouped by unit, move to `domain/` once a tile holds a picture handle. `summary`, which assembles the coordinator's `StageSummary` from them and from what the pictures service knows, is the actor's glue rather than a rule; every rule over ids, rects and tags alone is in `domain/` |
| `src/main.rs` | the main thread joins seven actors that draw nothing (the WM controller, notifications, spaces, window notify, mission control, process lifecycle, the cursor warp) beside the four that do (the renderer, the switcher, the bar, the rest borders), so a busy one holds a batch the renderer is waiting to draw. Each actor's polls are timed into the main thread's turn, and a turn over 8ms is journalled with each actor's share, the time outside them and the window changes before it (`main_turn` in `src/transitions/docs/journal.md`), so which one holds a batch is read rather than guessed | the cursor warp needs no main thread, as `src/main.rs` says where it makes it: it calls Core Graphics alone (a null-source `CGEvent`'s location, `CGWarpMouseCursorPosition`, which the input tap already calls on the `input` thread, and `CGAssociateMouseAndMouseCursorPosition`) and waits on its channel and a `rini_runloop` sleep, which run on whichever loop polls them, so it is the first to move. Which of the other six need it is not verified. By a grep of their own files, spaces alone names a type objc2 marks main-thread-only (`MainThreadMarker`, for `ScreenCache`), and the WM controller dispatches onto the main queue; what each calls further down, and what its run-loop sources need, is not known |
| `windows`, `displays`, `layout`, `workspaces`, `input`, `switcher`, `bar`, `src/app/` | CG geometry (`CGRect`, `CGPoint`, `CGSize`) in their interfaces and `domain/`, converted at each call into `pictures`, `animation` and `transitions`, which speak rini-geometry's spaces | typing them is a step of its own, and `displays` would add a unit for Cocoa's bottom-left space, which only its `CoordinateConverter` and the `NSWindow` sizing see |
| `src/app/config/` | one `Settings` struct that every feature's fields hang off, filled from one file | it depends on every feature and nothing depends on it, so it is a hub at the edge, which is the tolerable kind. The file's sections are niri's, not the features', and `config/kdl/apply.rs` is the one place that maps one onto the other |
| `src/workspaces/engine.rs` | the orchestrator of workspaces, floating, persistence and display affinity, past the file limit | the seam to `layout` is clean (`ScrollingLayoutSystem`). The methods past the function limit (`calculate_layout_with_virtual_workspaces`, `move_window_to_workspace`, `on_windows_on_screen_updated`, `handle_event_inner`, `move_window_to_space`, `move_focus_internal`) are single jobs that would need splitting on their own logic rather than on a dispatch boundary |
| `src/app/hotkeys/mod.rs` | lowers each bound `Action` to a `Command` | lowering them at decode time in `input` would make the config's words the wire's again, which is what tied a config to the protocol before |
| `src/app/notifications.rs` | feeds two features | it is the application's demultiplexer of NSWorkspace notifications; splitting it by feature is possible but doubles the subscriptions |
| `src/app/reactor/` | `mod.rs`, past the file limit, and `events/`, still owning every feature's store | what did not need the reactor has already left: pure feature logic to its `domain/` (transactions, space activation, screen selection, focus rules, strip stack, frame writes, the layout-pass sort, app-rule follow-up `AfterRules`, the topology diff `analyze_space_snapshot`); the manageability verdict `manageable` and the stale-window verdict `looks_gone` to `windows/domain/manageable.rs`, pure functions over the facts the window server port answers; the reactor's reads of the machine to one `SystemReads` value it is handed, which its reducers take their answers from as parameters; the two focus actors (`RaiseManager`, `MainWindowTracker`) run in `windows` against an `EventSink`; cross-feature reads became borrowed views (`reactor::space_affinity::SpaceAffinity`, `reactor::query::StateView`). What remains is orchestration: `dispatch_workflow`, a dispatch table over `commands.rs` and `observations.rs`, `apply_event_outcome`, `handle_layout_response`, `handle_authoritative_space_snapshot`, the strip-movement builders, and the `events/` reducers, which read and write several stores per event and so belong to the application. The ones past the function limit are named in `LONG_FNS`. The next cut, if one is wanted, is the stores themselves: each feature's store owned by its own actor with the reactor holding handles, which changes the event model rather than moving code |

## The costs of this layout

`use crate::anything` always compiles, which is how the monolith that preceded
the crate split grew ten mutually importing module pairs. That is what
`tests/architecture.rs` is for, and why it is a ratchet rather than a style
guide: the one file that already breaks rule 2 is named in it, and nothing else
may join it.

The same test holds sizes. No non-test function may pass 100 code lines and no non-test
file 1500, counting neither comments, blank lines nor `#[cfg(test)]` code. The ones past
either limit are `LONG_FNS` and `LONG_FILES` in `tests/architecture.rs`, each with a
ceiling at its size: an entry may not grow, a change that shrinks one lowers its ceiling to
the new size, and an entry leaves the list once it is back under
(`no_function_or_file_grows_past_its_limit`). Those lists are the record of how long each
function and file is now; a doc that needs a size points at them.

`cargo test -p rini-geometry` still builds plain Rust instead of linking AppKit.
`cargo test` on the application does link it, so the win is now "this module has
no macOS in it, provably" rather than "this crate cannot link AppKit".

## Where documentation lives

Three kinds of written record, in three places, and they do not overlap.

**`specs/` is what the software must do.** Requirements, as obligations with their reasons, plus the
report that gave rise to each one. Read `specs/README.md` first: it carries the rule that anything the
user reports — a bug, a quirk, a behaviour ask — becomes a requirement there in the same change that
acts on it. A test says the code does something; it does not say the behaviour was asked for, and this
repo has already had one bug "fixed three times" because only the symptom was ever recorded.

**`docs/` is how the code came to be that way.** Measurements, findings, rejected approaches, audit
history. A requirement links to its evidence here; the evidence does not restate the requirement.

**A finding lives in one place, and that place is beside the code it is about.**

Every feature and every crate has its own `docs/`, with a `README.md` explaining what
the module owns, the shape that matters, a reading order, and its known debt:

```text
src/<feature>/docs/README.md    what this feature is, and how to read it
src/<feature>/docs/<topic>.md   a finding that belongs to this feature
crates/<crate>/docs/README.md   same, for a library
docs/                           only what genuinely spans features
```

What is left at the top of the tree is cross-cutting: this file, `testing.md`,
`signing.md`, `permissions-and-the-launch-agent.md`, `config.md` (the config format's
decisions), `pictures-motion-transitions.md` (how four features share drawing a moving
window), the implementation audit with `audit-progress.md` beside it, `cleanup-round-1.md`,
and `data/`, the measured runs the docs cite.
Anything that names one feature belongs inside it.

Code points at the doc by path when a reader would otherwise be stuck. When code moves,
its docs move with it — and since a feature's docs now live inside the feature, moving a
folder moves both.

Five tests in `tests/architecture.rs` hold this rather than good intentions:
`every_feature_and_crate_documents_itself` requires the READMEs to exist,
`every_domain_and_reactor_module_is_named_in_its_readme` requires each one to name, by its path
from the feature's root, every module of its `domain/` (and `src/app/docs/README.md` every module
of `reactor/` and `reactor/events/`),
`every_documented_path_resolves` fails on any path into `src/`, `crates/`, `tests/`, `docs/`,
`specs/` or `tools/` that the docs, the code or the tools mention and that no longer exists,
`every_long_name_a_doc_cites_is_still_defined` fails on a backticked snake_case name of four words
or more in the docs, a test's included, that is no longer a function or a field (the names quoted
from other code are `OTHERS_NAMES`, the ones recorded as deleted `GONE_NAMES`), and
`every_spec_is_listed_in_the_index` stops a spec nothing links to. A stale sentence reads exactly like
a current one, which is why none of this is left to discipline.
