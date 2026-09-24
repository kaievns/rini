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
- Width MUST be remembered as the layout means it — a mode or a ratio — never in points, because the
  record is consulted exactly when the display size differs.
- A window's display MUST be written only by intent: an explicit move, a drag, a first sighting, or a
  restore. The forced reassignment that follows an unplug MUST NOT write it, because that record is the
  only thing that brings the window back.
- Moving a window to another display MUST be an intent, so the window stays there across a later
  unplug and replug.
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

## Pinning an app to a display

- A config rule MAY pin matching windows to the `internal` or the `external` display.
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

> **Found 2026-09-23, not reported.** The rule against following a window onto a non-user space was
> written but not enforced: the guard declined, and the next step assigned the window anyway because it
> asked a resolver that knew nothing about user spaces. Reachable whenever the screen locks.

## Where it lives

`src/displays/platform/spaces.rs` is the authority, `src/displays/domain/topology.rs` the snapshot
rules, `src/workspaces/domain/display_memory.rs` the durable records. Measurements are in
`src/displays/docs/topology.md` and `src/workspaces/docs/workspaces-and-displays.md`.
