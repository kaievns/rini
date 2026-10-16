# `rini-skylight-sys` — raw SkyLight and CGS declarations

The private window-server API, declared. Reverse engineered by yabai and other projects;
rini uses it without disabling SIP and without a scripting addition.

## What it owns

The `SLS*` and `CGS*` function declarations, and the plain value types that come with
them: `SpaceId` (a `CGSSpaceID`), `WindowServerId` (a `CGWindowID`),
`DisplayReconfigFlags`, `KnownCGSEvent`.

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
[`src/decorations/docs/rest-windows.md`](../../../src/decorations/docs/rest-windows.md)). `SLSOrderWindow` is
not declared: against another process's window it did nothing (probe P1, same section).

`SLSNewConnection` opens a further connection of rini's own. Every read that waits on the last Core
Animation transaction of the connection it is given is made on one such, which no window of rini's
belongs to, because on the main connection it deadlocks with the main thread's commit for 500ms.
`SLSGetOnScreenWindowList`, `SLSCopyWindowsWithOptionsAndTags` and `SLSWindowIsOrderedIn` wait on
the main connection whatever they are given, so they are not declared: which calls wait, on which
connection, what rini reads instead, and why each other call stays on `SLSMainConnectionID`, is
[`src/windows/docs/window-server-reads.md`](../../../src/windows/docs/window-server-reads.md).

## Notify-proc event numbers

`SLSRegisterConnectionNotifyProc` takes an event number. `KnownCGSEvent` declares the ones
rini registers (`src/main.rs`) or matches on; this is every number known, with what is known of
its payload. The notes come from reading where the window server posts each one: "seen" marks a
payload observed in rini's callback, "likely" a guess.

| Number | Name | Note |
|---|---|---|
| 102 | `DisplayWillSleep` | |
| 103 | `DisplayDidWake` | |
| 723 | `WindowUpdated` | |
| 724 | `kCGSEventNotificationSystemDefined` | possibly loginwindow becoming active |
| 804 | `WindowClosed` | |
| 805 | `WindowDisplayChanged` | |
| 806 | `WindowMoved` | |
| 807 | `WindowResized` | |
| 808 | `WindowReordered` | |
| 811 | `WindowLevelChanged` | |
| 815 | `WindowUnhidden` | |
| 816 | `WindowHidden` | |
| 1204 | `MissionControlEntered` | |
| 1308 | `PackagesStatusBarSpaceChanged` | named in `_WSLogStringForNotifyType`; seen when the active display or status-bar space changes, including current-space and capability updates |
| 1322 | `WindowTitleChanged` | |
| 1325 | `SpaceWindowCreated` | |
| 1326 | `SpaceWindowDestroyed` | |
| 1327 | `SpaceCreated` | |
| 1328 | `SpaceDestroyed` | |
| 1329 | `SpaceCurrentChanged` | posted by `managed_display_set_current_space` through `post_space_lifecycle_notification`; likely carries the display's new current space id |
| 1333 | `WindowManagerActivatingClickOrdering` | posted during activating-click ordering; the payload looks like window and order metadata, layout unknown |
| 1334 | `WindowManagerSpaceFrontConnectionChanged` | the current space's front connection changed |
| 1335 | `WindowManagerGlobalFrontConnectionChanged` | the global front connection changed |
| 1336 | `WindowOrderingGroupChanged` | posted from `finish_order_windows`; payload seen as 3 × u32 |
| 1338 | `SpaceWindowTransactionCommitted` | posted by `-[PKGSpaceWindowManager_commitTransaction]`; a transaction boundary even when per-window membership notifications race |
| 1339 | `SpaceWindowBatchReassociated` | posted from `finishBatchReassociateWindows`; payload starts with a u64 key or space, then a u32 count and that many window ids |
| 1340 | `SpaceWindowManagementCapabilitiesChanged` | posted through `__XSetSpaceWindowManagementCapabilities`; likely a display's or space's window-management mode changing |
| 1341 | `WindowParentChanged` | posted from `_WSWindowSetParent` and related reassociation paths |
| 1342 | `ManagedSpaceMembershipUpdated` | posted from `managed_space_update_membership`; likely marks a finished space-membership change, possibly with space and window ids |
| 1400 | `WorkspaceWillChange` | |
| 1401 | `WorkspaceDidChange` | |
| 1402 | `WorkspaceWindowIsViewable` | |
| 1403 | `WorkspaceWindowIsNotViewable` | |
| 1404 | `WorkspaceWindowDidMove` | |
| 1405 | `WorkspacePrefsDidChange` | |
| 1411 | `WorkspacesWindowDragDidStart` | |
| 1412 | `WorkspacesWindowDragDidEnd` | |
| 1413 | `WorkspacesWindowDragWillEnd` | |
| 1414 | `WorkspacesShowSpaceForProcess` | |
| 1415 | `WorkspacesWindowDidOrderInOnNonCurrentManagedSpacesOnly` | |
| 1416 | `WorkspacesWindowDidOrderOutOnNonCurrentManagedSpaces` | |
| 1508 | `FrontmostApplicationChanged` | |
| 1700 | `TransitionDidFinish` | |
| `0xFFFF_FFFF` | `All` | |

## Measured behaviour

Capture options and the desktop's levels are recorded in
[`src/pictures/docs/capture-research.md`](../../../src/pictures/docs/capture-research.md), the overlay's level
and notification semantics in
[`src/animation/docs/capture-overlay-research.md`](../../../src/animation/docs/capture-overlay-research.md),
and `SLSCaptureWindowsContentsToRectWithOptions` on the menu extras in
[`src/bar/docs/menu-extras.md`](../../../src/bar/docs/menu-extras.md).
Private API behaviour is measured rather than documented upstream, so a claim here
without a measurement behind it should be distrusted.
