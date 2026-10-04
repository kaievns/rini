# `windows` — windows and the apps that own them

The most upstream feature. It knows what a window is, which application owns it, and
what rini is allowed to do to it. It knows nothing about screens, spaces, workspaces
or layout: everything else depends on it and it depends on nothing.

## What it owns

| | |
|---|---|
| **Which apps have a thread** | `domain/app_threads.rs` — when a running application gets its Accessibility thread: launched, observed ready, seen ready when the window server reports a new window of it, or seen ready when it next becomes frontmost; and never a second one |
| **The catalogue** | `domain/catalogue.rs` — every window rini has seen, keyed by `WindowId { pid, idx }`, with its window-server id, its placement, and the native-fullscreen records that let a window come back |
| **What a window IS** | `domain/info.rs` (`WindowInfo`, `AppInfo`, `WindowServerInfo`) and `domain/state.rs` (`WindowState`, manageability) |
| **The AX boundary's rules** | `domain/ax_events.rs` — whether a failed Accessibility request means the window is gone, and the codec for the one `usize` an observer callback carries |
| **What rini takes on** | `domain/admissible.rs` — the four reasons an Accessibility window is turned away before anything else knows it exists |
| **Window rules** | `domain/rules.rs` — every `window-rule` that applies to a window, folded in file order as niri does. Resolving the result against workspaces is `workspaces::domain::app_rules` |
| **Focus** | `domain/focus.rs` — `MainWindowTracker` and the key window it has heard of, which windows' looks a change of it turns, the raise-echo rule, and the activation-focus rule that stops cmd-tab moving a display |
| **Raising** | `domain/raise.rs` — the raise manager |
| **Raise order** | `domain/raise_order.rs` — which windows are worth raising, and in what order; `StackGroup`, on the strip or off it, and `regroup_tiled`, which raises the strip back in front of a window off it that sits inside it |
| **Frame transactions** | `domain/transaction.rs` — txids, so a frame report can be matched to the write that caused it |
| **Frame writes** | `domain/frame_write.rs` — which Accessibility writes a frame takes: the position alone when the window server already has the window at the desired size, size, position and size again otherwise, and again after a position written alone that read back at another size |
| **Title-bar zoom** | `domain/zoom.rs` — which window's title bar a double-click landed on, and what that window's resizes in the next second mean: the zoom, which toggles full width once, or the rest of it, which rini overrules |
| **The port into a window** | `domain/request.rs` — `Request`/`Quiet`, what the per-app thread accepts |

Two questions about raising and admission that were closed on evidence rather than code are in
`admission-and-raise-findings.md`.

`platform/` is the macOS side: `app_actor.rs` is the per-app Accessibility actor (one
thread per application, observing and writing), `window_server.rs` the window-server
reads, `lifecycle.rs` the launch, quit and front-app listener over `carbon.rs`'s Carbon
events, `app.rs` the activation-policy and finished-launching observers, `process.rs`
the process-type check, `sub_level.rs` the SkyLight sub-level port.

## The shape that matters

**One thread per application.** An AX call blocks on the owning app, so a hung
application must not hang rini. Each app gets a thread with its own `CFRunLoop`, and
the reactor talks to it through `Request`. An application slow to answer while it
launches is asked again rather than given up on, and one that becomes frontmost
without a thread is looked at again then (`launch-notifications.md`).

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
- [`launch-notifications.md`](launch-notifications.md) — what macOS reports while an
  application launches, measured, and how to read a launch back out of the unified log

Cross-feature detail is at the top of the tree: [`docs/architecture.md`](../../../docs/architecture.md),
[`docs/testing.md`](../../../docs/testing.md).
