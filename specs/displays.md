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
