# `switcher` — every window, in the order you last used them

The strip answers "what is beside this window on this display". The switcher answers a
different question: "every window I have, wherever it is". Different question, different
feature — it reads the workspace assignments rather than any one layout tree, and it groups
nothing.

## What it owns

| | |
|---|---|
| **Who is offered** | `domain/candidates.rs` — `Candidate`, `Scope`, `switch_list`, `opening_selection` |
| **Where the cursor is** | `domain/selection.rs` — `Selection`: stepping with wrap, clicking, and following its row when the list changes under it |
| **Which binding holds it** | `domain/trigger.rs` — the session's keys, derived from the `switch_window` binding rather than configured twice |
| **Where the rows go** | `domain/layout.rs` — the panel rect, one rect per row, the scroll that keeps the selection visible, and the hit test |
| **The popup** | `platform/panel.rs` — the `NSPanel` and its layer tree; `platform/actor.rs` — the main thread it must live on |

Focus order itself is not here. It is a fact about window focus, so it lives with the rest
of the focus tracking in `src/windows/domain/focus_order.rs` and this feature reads it.

## The shape that matters

**One row per window, never per application.** A row standing for four Slack windows cannot
take you to the third one. This is the whole reason the switcher is not a wrapper over
`cycle_app_windows`, which is deliberately scoped to the focused application.

**Focus order first, then a stable tail.** The window you want next is nearly always the one
you were in before this one. But a window rini has never seen focused has no place in that
order, and the enumeration it arrives in is an `FxHashMap` walk — unspecified, and different
run to run. The tail is sorted by `(space, workspace index, window id)` so two presses of the
same key walk the list instead of jumping around it.

**A switch opens on the SECOND entry.** The first is the window you are already in. Opening
there would make a quick tap do nothing, and "tap to get back to the last window" is what the
key is for.

**Minimised windows are offered.** macOS's own switcher shows them, and leaving them out means
a window you minimised becomes unreachable by the one key whose job is reaching windows.
`WindowState.info.is_minimized` is the reliable flag; `WindowVisibility::Minimized` is not,
because the window-server visible/hidden sweeps overwrite it.

**One owner for the selection.** Three things move the cursor — the trigger key repeating, the
arrow keys, and a mouse click. Any two of them keeping their own idea of the selected row is a
race that shows up as the popup highlighting one window while the release focuses another.

**Scope is a parameter, not a second code path.** The global switcher was asked for first and a
per-workspace one is wanted later; they differ only in which candidates are admitted.

**The tap holds a flag, not a list.** `src/input/domain/switch_session.rs` answers "swallow or pass,
and what do I tell the reactor" for every key event, because an active event tap sits in the event
delivery path and the window server blocks each matching event until the callback returns. The list and
the cursor live on the reactor side, where the popup will be drawn.

**Three rules there are about not stranding the user**, and each one has a test that fails when it is
removed: the modifier's release is never swallowed, a session has a hard deadline checked against
arriving events rather than a timer, and a key the switch has no use for passes through.

**Its own window, not the animation overlay's.** The overlay is opaque black across the whole display,
only ever shown with a captured desktop behind it, sets `ignoresMouseEvents(true)`, and is shown by
fading its alpha — which a window that accepts clicks cannot do, because an alpha-0 window still
hit-tests. So the panel orders in and out instead, at ~14ms a gesture.

**An `NSPanel` with `NonactivatingPanel`.** rini runs as an Accessory application, so a plain window
taking a click would activate rini and deactivate the application being switched away from — inverting
the one thing a switcher exists to do.

**The popup is cosmetic.** The reactor holds the list and the cursor and runs on its own thread; the
panel is a main-thread actor fed rows over a channel. A slow or missing panel delays a picture and
nothing else, because the switch is already correct on the other side.

## Reading order

`domain/candidates.rs`, then `domain/selection.rs`, then `domain/layout.rs`. All three are pure and
fully tested without a reactor, a window server or a screen. `platform/` after that.

## Detail

Requirements are in [`specs/switcher.md`](../../../specs/switcher.md). Cross-feature detail is
at the top of the tree: [`docs/architecture.md`](../../../docs/architecture.md).
