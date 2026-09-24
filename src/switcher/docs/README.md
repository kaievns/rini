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

## Reading order

`domain/candidates.rs`, then `domain/selection.rs`. Both are pure and fully tested without a
reactor, a window server or a screen.

## Detail

Requirements are in [`specs/switcher.md`](../../../specs/switcher.md). Cross-feature detail is
at the top of the tree: [`docs/architecture.md`](../../../docs/architecture.md).
