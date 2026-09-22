# `windows` — windows and the apps that own them

The most upstream feature. It knows what a window is, which application owns it, and
what rini is allowed to do to it. It knows nothing about screens, spaces, workspaces
or layout: everything else depends on it and it depends on nothing.

## What it owns

| | |
|---|---|
| **The catalogue** | `domain/catalogue.rs` — every window rini has seen, keyed by `WindowId { pid, idx }`, with its window-server id, its placement, and the native-fullscreen records that let a window come back |
| **What a window IS** | `domain/info.rs` (`WindowInfo`, `AppInfo`, `WindowServerInfo`) and `domain/state.rs` (`WindowState`, manageability) |
| **What rini takes on** | `domain/admissible.rs` — the four reasons an Accessibility window is turned away before anything else knows it exists |
| **App rules** | `domain/rules.rs` — matching config rules against a window. Resolving a match against workspaces is `workspaces::domain::app_rules` |
| **Focus** | `domain/focus.rs` — `MainWindowTracker`, the raise-echo rule, and the activation-focus rule that stops cmd-tab moving a display |
| **Raising** | `domain/raise.rs` — the raise manager |
| **Raise order** | `domain/raise_order.rs` — which windows are worth raising, and in what order |
| **Frame transactions** | `domain/transaction.rs` — txids, so a frame report can be matched to the write that caused it |
| **The port into a window** | `domain/request.rs` — `Request`/`Quiet`, what the per-app thread accepts |

`platform/` is the macOS side: `app_actor.rs` is the per-app Accessibility actor (one
thread per application, observing and writing), `window_server.rs` the window-server
reads, `carbon.rs` the front-app listener, `process.rs` the process watcher,
`sub_level.rs` the SkyLight sub-level port.

## The shape that matters

**One thread per application.** An AX call blocks on the owning app, so a hung
application must not hang rini. Each app gets a thread with its own `CFRunLoop`, and
the reactor talks to it through `Request`.

**A window has two identities.** `WindowId { pid, idx }` is rini's own and dies with
the process. `WindowServerId` is macOS's, survives, and is recycled. The catalogue
keys by the first and indexes by the second, which is why a rekey is an operation
rather than an assignment.

**Accessibility and the window server disagree.** AX reports a window before the
server has it, so `admissible::has_visible_peer` treats "no server id yet" as visible
rather than absent. The reverse — an id the server does not report — means the window
is not on screen.

**A raise list is a z-order.** Raising is last-wins, so the order of the list IS the
stacking it produces. `domain/raise_order.rs` exists because that order kept being
thrown away: the batching step grouped the list through a hash map, which handed the
batches back in hash order and undid the strip regroup that had just been built. With
eight applications, `FxHashMap` returned them exactly reversed, so the focused window
was raised first instead of last and ended up behind the strip.

## Reading order

`domain/info.rs` → `domain/catalogue.rs` → `domain/admissible.rs` → then
`platform/app_actor.rs` for how the AX side drives it.

## Detail

- [`multi-display-focus.md`](multi-display-focus.md) — why rini keeps its own record
  of each app's focused window, and how cmd-tab is told apart from cmd-`

Cross-feature detail is at the top of the tree: [`docs/architecture.md`](../../../docs/architecture.md),
[`docs/testing.md`](../../../docs/testing.md).
