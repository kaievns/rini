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
- Arrangements MUST NOT share a window's records. Rearranging while docked MUST NOT change how the
  laptop looks alone, and the reverse. One record per window cannot express this: the last
  deliberate move won everywhere, so moving a window onto the laptop while undocked silently
  cancelled its place at the desk.
- An application's width MAY carry across arrangements, below the window's own records, as the next
  requirement says. It MUST NOT override a width a window has recorded in this arrangement, so
  rearranging at one desk still changes nothing at another.
- A window new to the session, of an application already running or one opened at a desk not seen
  before, MUST open at the width the user last gave that application on a display of the same
  shape, where it has no width of its own on its display, takes no launch slot and no rule sets its
  width. A display's shape is the points its frame leaves for windows and its physical width to the
  nearest 10mm, so the same panel docked and alone, and one model of monitor at two desks, are one
  shape. A width command and the end of a drag of an edge MUST record the application's width; a
  rule's resize, a width given to a window a rule sized and a focuslab window MUST NOT, since a
  rule's width written as the application's would size its other windows, and neither MUST a
  resize that changes only a window's height. A window the session has
  seen before MUST NOT take the application's width when it comes back: a window the window server
  removes and re-adds keeps its own width, or the default it had, not a sibling's, and so does a
  window back from native fullscreen under another id, though that id was first laid at the
  application's width. Not yet met: an
  existing window moved onto a display it has no width on takes the default there.
- An application's width MUST be kept with the `default-column-width` it overrode, and MUST NOT be
  applied under another one: an edit to the default MUST show on the next new window, not be beaten
  by a width chosen against the old value. One made under another value MUST NOT be read under
  it, and MUST be kept until the application's next write: a default read for a while, as the
  reference config's when the user's fails to parse or a `config set` trial's, MUST NOT cost the
  widths made under the user's. An application MUST keep at most 8 shapes, the most recently set.
- An arrangement is identified by WHICH displays are attached, not how many. An external at home, one
  at the office and a meeting-room projector are three arrangements, and none inherits another's
  layout beyond an application's width as above.
- A display that has never held a window MUST take none when it is attached. Every window stays where
  it is and waits to be told, and the arrangement learns from where the user puts them.
- Width MUST be remembered per (window, display), not per window. Half of 2338pt is comfortable and
  half of 1728pt is cramped; one number cannot serve both. This is what lets a browser be full-width
  when the laptop is alone and half-width on the external.
- Width MUST be remembered as the layout means it — a mode, a bounded ratio, or an unbounded ratio,
  which no display's width bounds hold, as a drag or `Mod+R`'s 100% gives — never in points, because the record is consulted exactly when the
  display size differs. An application's width MUST be held as a fraction of the display rather
  than an offset from `default-column-width`, so a default that differs on another display reads
  it as the same width.
- A remembered width MUST be written so a rini from before its kind existed still reads the file. A
  rini that cannot read the file starts with no homes, widths or launch slots, and its next save
  writes that over the file, so going back a version would forget the machine. An
  application's width a rini cannot read MUST cost that record alone, not the file.
- A width the user set, by a command or by dragging an edge, MUST survive the display sleeping and
  waking, the window server removing and re-adding the window, a drag of the window, and its app's
  relaunch under a rule that sets no width (`specs/workspaces.md`, "Relaunching an application").
  Each is a moment the width is read again, and a width read back as the default undoes the user's
  choice with nothing the user did to explain it. So only a close or the application quitting MAY
  erase a window's widths. A minimize, a window briefly not manageable, one the window server leaves
  out of a snapshot or moves to another space, a drag to another space and a rule that stops
  managing it MUST each keep them, and a window that leaves native fullscreen under another id MUST
  hand them to that id and be laid at them, though that id joins the strip before the old one leaves
  it. A width only the layout holds, as a drag's or a rule's, MUST be recorded before such a
  removal, and a launch slot's width MUST NOT replace one the window has of its own, nor be
  taken by a window the session has seen, which is matched to a slot after its siblings and can
  take one of theirs.
- A rule's width is a window's first. A width the window has of its own on a display MUST beat it
  when the window is laid out again, and the rule MUST NOT size the window again on its app's next
  report, unless the rule's width changed since it sized the window: an edit to a rule MUST show on
  the windows it sizes, once. Not yet met: a rule edited while its window was out of every strip, or
  while rini was not running, is beaten by the window's own width.
- Every erase of a window's remembered width MUST be logged with why and the window's app and
  title, at a level the running service keeps, because a width that comes back at the default is
  otherwise not traceable to what erased it. A removal that erases nothing MUST log below that
  level: a window that stays unmanageable is removed again on every snapshot, and a line per
  snapshot buries the one removal that erased.

> **Reported 2026-11-04, same session.** Asked whether an application's width may carry across
> arrangements, below the per-window record, Kai answered "2. yes". The rule this amends said
> "Arrangements MUST NOT share records", so that rearranging at one desk could not change another.
> That reason holds for a window's own records, which still share nothing. Applied to every record,
> it meant each desk or meeting room not seen before, and every new window of a running
> application, opened at `default-column-width`. Two of the resets of Kai's report below, both
> VS Code on 2026-11-04, fit a new window of the running editor opening at a half, inferred from
> a `MainWindowChanged on unknown window` line for its pid 15s and 27s before each width press.

> **Found 2026-11-04, not reported.** Looking into Kai's report that day ("i keep getting everything
> resetting back to 1/2") found widths lost across a sleep and wake that the service's log could not
> explain: neither the window's removal from the layout nor the erase of its width was logged.

> **Reported 2026-11-04.** "everything defaults to 1/2 of my laptop screen, and even though we have
> the prevous sizes tracking it's buggy as fuck and doesn't work all that well, so i keep getting
> everything resetting back to 1/2 which is annoying". The service's log that day tied 4 resets to a
> wake, 3 by timing alone and 1 measured at both ends (the window laid full as the display turned
> off, pressed back to full 80s after the wake), and 1 to a re-add by the window server 3s before
> the press. Every removal from the
> layout erased the window's widths, and a re-added window read a record a drag never wrote.

> **Found 2026-11-04, not reported.** A window an app rule sized went back to the rule's width on
> every report of its app, a re-add among them: an editor widened to 474pt went back to the rule's
> 234pt. Kai's config sets no rule width.

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
- A screen-parameters change that names a fullscreen space, as waking from sleep with a fullscreen
  video up does, MUST leave the user space's windows and layout as they were for its return.
- A refresh MUST retry a snapshot it cannot forward at most 10 times. While a wake or an unlock is
  still owed a snapshot it MUST start over until one is forwarded, since only a forwarded one
  releases the reactor. A display churn MUST check a topology it has already sampled at most 10
  times, then end unsettled and refresh. A topology that differs from the last sample MUST be
  checked again whatever the attempt, so a churn whose topology changes between every two samples is
  checked until one repeats.
- Each snapshot MUST make one visible-window refresh once every quarantine it releases is lifted,
  not one per quarantine.
- A window event MUST be dropped while a display churn is under way, from the moment the spaces
  actor begins it, including one already queued for the reactor ahead of `DisplayChurnBegin`. macOS
  replaces a window's Accessibility element on an unplug, and handling the old one's destruction
  removes a window that is still there.

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

`src/displays/platform/spaces.rs` is the authority, `src/displays/domain/authority.rs` the
lifecycle holds, churn and refresh it keeps, `src/displays/domain/topology.rs`,
`src/displays/domain/snapshot.rs` and `src/displays/domain/settling.rs` the snapshot rules,
`src/workspaces/domain/display_memory.rs` the durable records. The two caps are
`REFRESH_MAX_RETRIES` and `DISPLAY_STABILIZE_MAX_ATTEMPTS` in `settling.rs`, both 10
(`a_refresh_retries_ten_times`,
`a_churn_is_checked_again_ten_times_and_settles_at_two_identical_samples`);
`a_refresh_asks_again_for_a_snapshot_it_cannot_forward_up_to_its_cap_then_stops` and
`a_churn_that_never_settles_is_checked_up_to_its_cap_then_ends_with_a_refresh` in
`src/displays/platform/spaces/tests.rs` hold the spaces actor to them, and
`wake_quarantine_waits_for_fresh_space_snapshot_before_refresh` and
`session_lock_quarantine_ignores_discovery_and_replays_one_refresh_after_unlock` the one refresh.
`src/displays/platform/display_churn.rs` is the churn record the reactor drops window events by;
`a_window_destroyed_ahead_of_the_churn_s_begin_keeps_its_place` in
`src/app/reactor/tests/displays.rs` and
`a_settled_churn_is_recorded_before_its_begin_and_over_before_its_snapshot` in
`src/displays/platform/spaces/tests.rs` hold it.
`src/displays/platform/mission_control.rs` watches the Dock, and
`a_subscription_the_dock_is_too_busy_to_answer_is_left_for_the_next_ask` there and
`an_application_too_busy_to_answer_is_asked_again_until_it_answers` and
`an_application_that_never_answers_is_given_up_on_at_the_deadline` in
`src/windows/platform/ax/ask_again.rs` hold its asking again. `Removal::widths` in
`src/workspaces/domain/display_affinity.rs` says which removals keep a width, and
`src/app/reactor/tests/widths.rs` holds a width across a wake, a re-add and a drag, and a new
window at its application's width. `src/workspaces/domain/app_widths.rs` keeps the applications'
widths, `APP_WIDTHS_PER_APP` the cap, and `src/workspaces/engine/app_widths.rs` writes and gives
them. Measurements are in
`src/displays/docs/topology.md` and `src/workspaces/docs/workspaces-and-displays.md`.
