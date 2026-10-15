# More than one screen

## Requirements of the machine

- "Displays have separate Spaces" MUST be enabled. rini refuses to start without it and says so. With
  it off, macOS gives every display one shared space, so a per-display strip has nowhere to live and
  every space query answers about the wrong screen. This is a hard requirement, not a degraded mode.

## Identity

- macOS mints a NEW space id every time a display is reconnected. One monitor was observed as 479, 484,
  487, 516, 552 and 1138 in a single session. Nothing durable may be keyed by space id.
- A display's UUID is its durable identity. Everything that must survive an unplug is keyed by UUID.
- A display record MUST NOT be pruned when the display goes away. That record is precisely the memory
  needed to put windows back when it returns, and it costs nothing to keep one entry per display ever
  seen.

## Unplugging and replugging

- Before anything reacts to a display's absence, the windows that were on it MUST be recorded against
  it. That is the only moment the truth is available: once macOS has moved them to the remaining
  display, nothing can say which of them had been where.
- On replug, the windows recorded against that display MUST come back to it, keeping their workspace.
- A saved layout belonging to a display that is not attached MUST NOT be restored at that display's
  coordinates. Its windows lay out fresh on a live display and return when the display does.
- A window's display is written by INTENT — an explicit move, or first sighting — and never by the
  forced reassignment that follows a display change. Recording an observation would make a window
  evacuated during an unplug permanently belong to the display it was evacuated to.

> **Reported 2026-09-22 (as part of the layout audit).** Restoring windows at an unplugged display's
> coordinates was measured at x=-1680 with no display there, stranding them with nothing to migrate
> them back; a single dock/undock cycle then produced a layout only fixable by deleting the layout file.

## One work context, arranged differently per display

A workspace is a work context — coding, comms, research — and it spans every attached display. The same
context is arranged differently depending on what is plugged in, and rini remembers each arrangement.

- A window MUST remember, per ARRANGEMENT: which display it belongs to, its column width there, and
  its place in that display's strip order. An arrangement is the SET of attached displays, named by
  their UUIDs, so plugging a monitor in spreads the workspace and unplugging it gathers the windows
  back, each to a place remembered for that arrangement.
- Arrangements MUST NOT share records. Rearranging while docked MUST NOT change how the laptop looks
  alone, and the reverse. One record per window cannot express this: the last deliberate move won
  everywhere, so moving a window onto the laptop while undocked silently cancelled its place at the
  desk.
- An arrangement is identified by WHICH displays are attached, not how many. An external at home, one
  at the office and a meeting-room projector are three arrangements, and none inherits another's
  layout.
- A display that has never held a window MUST take none when it is attached. Every window stays where
  it is and waits to be told, and the arrangement learns from where the user puts them.
- Width MUST be remembered per (window, display), not per window. Half of 2338pt is comfortable and
  half of 1728pt is cramped; one number cannot serve both. This is what lets a browser be full-width
  when the laptop is alone and half-width on the external.
- Width MUST be remembered as the layout means it — a mode, a bounded ratio, or an unbounded ratio,
  which no display's width bounds hold, as a drag or `Mod+R`'s 100% gives — never in points, because the record is consulted exactly when the
  display size differs.
- A remembered width MUST be written so a rini from before its kind existed still reads the file. A
  rini that cannot read the file starts with no homes, widths or launch slots, and its next save
  writes that over the file, so going back a version would forget the machine.
- A window's display MUST be written only by intent: an explicit move, a drag, a first sighting, or a
  restore. The forced reassignment that follows an unplug MUST NOT write it, because that record is the
  only thing that brings the window back.
- Moving a window to another display MUST be an intent, so the window stays there across a later
  unplug and replug.
- A window moved to another display, by the user or by its return on replug, MUST land centred on
  that display and wholly on it. One larger than the display MUST keep its size and put its top-left
  corner on the display's, so its title bar stays on screen rather than above or left of it.
- `next` MUST cycle and wrap, so a single key moves a window back and forth between two displays. A
  direction MUST NOT wrap: right from the rightmost display names nothing.

> **Reported 2026-09-24.** "I will have my windows organised by workspaces coding/comms/research.
> Plugging a monitor in will let me spread a workspace between monitors and organise windows
> differently, say editor on one screen, terminal on the other, or have a browser full-width when there
> is one monitor but then have it 1/2 width on the external... when I add/remove monitor the windows
> should remember where they were in each configuration and regroup/resize accordingly."
>
> **Reported 2026-09-24, same session.** "I need that behaviour in the missing case... it needs to be
> keyed to a set of displays not per display, because configuration depends on a set: internal
> only/internal+external/external only (with internal lid closed). Moreover you need to track those
> display IDs too. My external display at home is different from one at the office. Or say I plug in a
> TV/projector in a meeting, my setup should not assume that it can just move everything to it."
> Records moved from one key per display to one per arrangement. The case that could not be expressed
> before — a window on the SAME display in two arrangements wanting a different width in each — is the
> one this answers.

> **Found 2026-10-15, not reported.** The explicit move and the return on replug placed a window with
> two copies of one rule that disagreed on a window larger than the display: the move kept its
> bottom-right corner on the display, which put its title bar above and left of it, and the return kept
> its top-left. Both now keep the top-left.

## Pinning an app to a display

- A `window-rule` MAY pin the windows it applies to to the `internal` or the `external` display, with
  `open-on-output "internal"` or `open-on-output "external"`.
- A pin names a ROLE, never a display. The external at home and the one at the office have different
  UUIDs, so a rule naming one would be silently inert at the other desk.
- A pin is a DEFAULT, not a law, and the precedence MUST be: an explicit move by the user, then the
  pin, then where the window happens to be.
  - It MUST correct a home rini merely inferred, which is what makes an app that OPENS on the wrong
    screen end up on the right one.
  - It MUST NOT override a home the user chose. Choosing is per arrangement, so moving a pinned window
    across while docked MUST NOT disable the pin when the laptop is alone.
- A pin whose role nothing fills MUST be inert. Pinned to the internal display with the lid shut, the
  window lives on whatever is attached rather than being held off the only screen there is.

> **Reported 2026-09-24.** "I will also need overrides in the config to say if I want specific
> apps/windows to be pinned to internal or external display consistently no matter what. Like for
> example Slack, Outlook, Messages should never leave my internal display unless I explicitly move it
> there or the lid is closed and external is the only one available."

> **Reported 2026-09-24.** "Can you add a ctrl-M key binding that will move a window between monitors? I
> might need left/right down the road, but for now I just really want to cycle because I only have two
> monitors so one button is enough." The `next` selector it needs had never worked: `DisplaySelector` is
> an untagged enum ending in `Uuid(String)`, which accepts any string, so `selector = "next"` parsed as
> a display whose UUID was literally "next" and the command silently moved nothing.

## What a display change must not do

- A snapshot naming no screens MUST NOT be treated as authoritative. An empty screen list is what macOS
  reports mid-reconfiguration, and believing it evacuates every window.
- One user space MUST NOT appear on two screens at once. macOS reports exactly that mid-transition, and
  committing it assigns one space's windows to two displays.
- Only user spaces count. Fullscreen and login spaces are transient native state and MUST be nulled out
  before anything reads them.
- A window MUST NOT be followed onto a login or fullscreen space. Assigning one there strands it
  somewhere the user cannot reach and no layout pass will touch.
- Asking the window server again MUST stop. A refresh MUST retry a snapshot it cannot forward at
  most `REFRESH_MAX_RETRIES` times, and MAY start over only while a wake or an unlock is still owed
  a snapshot, since only a forwarded one releases the reactor. A display churn MUST check a topology
  it has already sampled at most `DISPLAY_STABILIZE_MAX_ATTEMPTS` times, then end unsettled and
  refresh.
- Each snapshot MUST make one visible-window refresh once every quarantine it releases is lifted,
  not one per quarantine.

> **Found 2026-10-16, not reported.** The first snapshot after a wake or an unlock released two
> quarantines and refreshed once for each, so every application listed its windows twice. Tests never
> saw it: their snapshots claimed to release no display churn, which no production snapshot does.

> **Found 2026-09-23, not reported.** The rule against following a window onto a non-user space was
> written but not enforced: the guard declined, and the next step assigned the window anyway because it
> asked a resolver that knew nothing about user spaces. Reachable whenever the screen locks.

## Native Spaces

- rini MUST offer no action, wire command or CLI subcommand that switches the native macOS Space;
  macOS's own keys do that, and rini follows the Space shown. Kai, deleting `switch-space-left` and
  `-right` on 2026-10-15: "generally happy to delete", then "delete".
- Which native Spaces rini manages MUST be decided by `activate-spaces-by-default` and
  `toggle-space-activated` alone, with no command-line flag that limits rini to the Space it started
  on or starts every Space inactive. Kai, deleting `--one` and `--default-disable` on 2026-10-15:
  "delete".

## Mission Control

- rini MUST hear Mission Control open and close for the whole session, even when the Dock is too
  busy to answer its Accessibility subscriptions when rini starts, as it can be at login. A
  subscription the Dock does not answer MUST be asked again, every 20ms doubling to 1s for up to
  60s, and a Dock that never answers MUST be named in the log.

> **Found 2026-10-15 in review, not reported.** A Dock busy at rini's start was asked once more,
> 10ms later, and never again after that, so Mission Control could go unseen for the session.

## Where it lives

`src/displays/platform/spaces.rs` is the authority, `src/displays/domain/topology.rs`,
`src/displays/domain/snapshot.rs` and `src/displays/domain/settling.rs` the snapshot rules,
`src/workspaces/domain/display_memory.rs` the durable records.
`a_refresh_asks_again_for_a_snapshot_it_cannot_forward_up_to_its_cap_then_stops` and
`a_churn_that_never_settles_is_checked_up_to_its_cap_then_ends_with_a_refresh` in
`src/displays/platform/spaces/tests.rs` hold the caps, and
`wake_quarantine_waits_for_fresh_space_snapshot_before_refresh` and
`session_lock_quarantine_ignores_discovery_and_replays_one_refresh_after_unlock` the one refresh.
`src/displays/platform/mission_control.rs` watches the Dock, and
`a_subscription_the_dock_is_too_busy_to_answer_is_left_for_the_next_ask` there and
`an_application_too_busy_to_answer_is_asked_again_until_it_answers` and
`an_application_that_never_answers_is_given_up_on_at_the_deadline` in
`src/windows/platform/ax/ask_again.rs` hold its asking again. Measurements are in
`src/displays/docs/topology.md` and `src/workspaces/docs/workspaces-and-displays.md`.
