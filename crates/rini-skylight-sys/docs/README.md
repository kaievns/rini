# `rini-skylight-sys` — raw SkyLight and CGS declarations

The private window-server API, declared. Reverse engineered by yabai and other projects;
rini uses it without disabling SIP and without a scripting addition.

## What it owns

The `SLS*` and `CGS*` function declarations, and the plain value types that come with
them: `SpaceId` (a `CGSSpaceID`), `WindowServerId` (a `CGWindowID`),
`DisplayReconfigFlags`, `CGSEventType`, `KnownCGSEvent`.

One function is declared as a pointer type and not linked:
`SLSWindowIteratorGetCornerRadiiFn`. A symbol linked here that some macOS lacks stops rini
launching there, and which macOS first has this one is not known, so its caller resolves it by
`dlsym` (`src/windows/platform/window_server.rs`, beside the query keys it resolves the same way)
and reads no radius without it. The crate stays declarations only.

## The shape that matters

**The value types are vocabulary, the functions are API.** `SpaceId` is a number the
domain has to carry; `SLSSpaceGetType` is a call it must not make. That distinction is
what `SKYLIGHT_VALUE_TYPES` in [`tests/architecture.rs`](../../../tests/architecture.rs)
encodes, so a pure module may name the first and not the second.

`rini-core` re-exports `SpaceId` and `WindowServerId`, so nothing takes a dependency on
this crate just to hold a number.

`SLSSetWindowLevel` and `SLSSetWindowSubLevel` act only on a window of the caller's own
connection: rini sets them on its rest windows, copied from the window each decorates, and lets
AppKit order the two ("Ordering a rest window" in
[`src/decorations/docs/README.md`](../../../src/decorations/docs/README.md)). `SLSOrderWindow` is
not declared: against another process's window it did nothing (probe P1, same section).

`SLSNewConnection` opens a further connection of rini's own. Every read that waits on the last Core
Animation transaction of the connection it is given is made on one such, which no window of rini's
belongs to, because on the main connection it deadlocks with the main thread's commit for 500ms.
`SLSGetOnScreenWindowList`, `SLSCopyWindowsWithOptionsAndTags` and `SLSWindowIsOrderedIn` wait on
the main connection whatever they are given, so they are not declared: which calls wait, on which
connection, what rini reads instead, and why each other call stays on `SLSMainConnectionID`, is
[`src/windows/docs/window-server-reads.md`](../../../src/windows/docs/window-server-reads.md).

## Measured behaviour

Capture options and the desktop's levels are recorded in
[`src/pictures/docs/capture-research.md`](../../../src/pictures/docs/capture-research.md), the overlay's level
and notification semantics in
[`src/animation/docs/capture-overlay-research.md`](../../../src/animation/docs/capture-overlay-research.md),
and `SLSCaptureWindowsContentsToRectWithOptions` on the menu extras in
[`src/bar/docs/menu-extras.md`](../../../src/bar/docs/menu-extras.md).
Private API behaviour is measured rather than documented upstream, so a claim here
without a measurement behind it should be distrusted.
