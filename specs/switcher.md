# The window switcher

A replacement for macOS's cmd-tab. The strip answers "what is beside this window on this display"; the
switcher answers "every window I have, wherever it is".

## What is offered

- The switcher MUST offer every window rini knows about: every workspace, every strip, every display.
  It is not scoped to a display or a workspace.
- Windows MUST NOT be bundled by application. One entry per window. A row standing for four Slack
  windows cannot take you to the third one, which is the whole failing of the native switcher here.
- Minimised windows MUST be offered. macOS's own switcher shows them, and leaving them out makes a
  window you minimised unreachable by the one key whose job is reaching windows.
- A window with no workspace assignment MAY be omitted: there is nowhere to switch to for a window rini
  has not placed.
- A narrower scope — the windows of the current workspace only — MUST be available on its own binding.
  It is the same machinery with a narrower candidate set, never a second code path.

## Order

- The order MUST be most-recently-focused first. That is what makes the switcher useful: the window you
  want next is nearly always the one you were in before this one.
- A window rini has never seen focused MUST still appear, after the ones it has, ordered by space, then
  workspace position, then window id. The enumeration arrives from a hash map whose order differs run
  to run, so the tail MUST be sorted explicitly or the list reshuffles between two presses of the same
  key.
- Workspace position MUST come from the canonical workspace order, not from a workspace id — the ids
  are opaque and sort into an order the user never sees.
- A switch MUST open on the SECOND entry. The first is the window you are already in, so opening there
  would make a quick step do nothing.
- Re-focusing the window already at the front MUST change nothing. One raise produces a focus report
  per window it touches, and raising a strip window lifts the whole visible strip, so a switcher that
  reshuffled on every report would destroy the order it exists to keep.

## Stepping

- A step MUST focus the target wherever it is, switching the owning display's workspace to follow. A
  step that leaves focus on a window parked off screen reads as a dead key.
- Stepping MUST wrap at both ends, in both directions.
- The step MUST be reachable without the keyboard, through the CLI. That is the only path that still
  works when the event tap has been stood down, and rini cannot hand a redirected chord back to macOS.

> **Reported 2026-09-24.** "What I want [is] a niri style app switcher on cmd-tab instead of the macOS
> built in one... show a popup strip with a small preview of each window instead of icons (although
> small icons next to previews would be handy) and unbundle windows of every app too. The switcher
> unlike the strip will have to be a global construct showing all windows from all workspaces, strips
> and displays. I'll need a separate keybinding to cycle through the current workspace only later too.
> The behaviour will need to be similar to the native though: a quick cmd-tab cycles windows without the
> popup, a cmd hold and tab-up while holding brings up the popup, releasing cmd closes it. It should
> also accept arrow keys and mouse clicks too like a normal window switcher popup."

## Holding it open

- A quick tap MUST step without showing anything. Holding the trigger's modifier MUST keep the switch
  open so further presses move a selection, and RELEASING the modifier MUST commit it.
- Nothing MUST be focused until the commit. Focusing as the selection moves raises every window it
  passes over — a burst of Accessibility work, and a visible flicker through windows nobody asked to
  see.
- The modifier's release MUST be passed through. Swallowing it would leave every application believing
  that modifier is held forever.
- A repeat of the trigger MUST step the selection. Holding the key is how a switcher is walked.
- The arrow keys MUST move the selection while a switch is open, and Escape MUST cancel it without
  focusing anything.
- A key the switch has no use for MUST pass through untouched, and MUST NOT end the switch. Swallowing
  everything would be the more thorough modal behaviour and is also how a live session becomes a dead
  keyboard.
- A switch MUST have a hard deadline, checked against arriving events rather than kept by a timer, and
  reaching it MUST commit. The event tap can be rebuilt by a config reload, stood down for ten seconds
  by its own re-enable governor, or have its held-key cache wiped — and a session that still believed
  its modifier was held would swallow the arrow keys with nothing left to release it.
- Replacing the event tap MUST end a live session, on EVERY path that replaces it. It MUST commit rather
  than cancel: the user pressed the key meaning to go somewhere, and rini losing the keyboard underneath
  them is not a reason to pretend they did not.
- The list MUST be snapshotted when the switch opens, not rebuilt per step. It is ordered by focus, and
  focus changes the moment anything commits.
- The modifier that holds a switch open MUST be derived from the trigger binding itself. Configuring it
  separately would be a second record of one fact, free to disagree.

## The trigger

- The trigger MUST be a configurable chord, never cmd-tab specifically.

  macOS cannot be made to give up cmd-tab from inside rini. The Dock's switcher is a WindowServer
  symbolic-hotkey target rather than a consumer of the keyboard event stream, so deleting the event from
  a session tap cannot retract a dispatch the WindowServer has already made. The private
  `SLSSetSymbolicHotKeyEnabled` route exists but turns every degraded state from "the key does its
  normal thing" into a dead key.

  The chord is redirected upstream instead, in the keyboard layer, which keeps the physical gesture and
  never touches the reserved combination.
- Releasing the held modifier MUST commit the selection, and the modifier's release event MUST be passed
  through untouched. Swallowing it would leave every application believing that modifier is still held.

> **Reported 2026-09-24.** "If it can't we can assign another key binding to it, I still want the
> feature." Which is what made the trigger a configuration detail rather than a blocker.

## Honest limitation

- Once the trigger chord is redirected upstream, rini CANNOT fail open to macOS's switcher: the reserved
  combination is no longer being sent. If rini is wedged, that key does nothing. This is the cost of the
  approach and MUST be stated rather than presented as graceful degradation. The CLI step is the
  mitigation.

## Where it lives

`src/switcher/domain/candidates.rs` is who is offered and in what order,
`src/switcher/domain/selection.rs` the cursor, `src/switcher/domain/trigger.rs` which binding holds a
switch open, and `src/input/domain/switch_session.rs` the tap's side of it. The focus order itself is a fact about focus and lives
with the rest of it, in `src/windows/domain/focus_order.rs`. Reaching a window anywhere is
`Reactor::focus_window_anywhere`. Design notes are in `src/switcher/docs/README.md`.
