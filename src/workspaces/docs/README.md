# `workspaces` — virtual workspaces and what persists

Rini's own workspaces, layered over macOS spaces: which windows belong to which
workspace, which workspace each display is showing, and everything that has to survive
a restart. Sits above `windows`, `displays` and `layout`.

## What it owns

| | |
|---|---|
| `engine.rs` | `LayoutEngine`: the orchestrator. Holds the workspaces, the per-display layouts, floating state, app rules and the persistence journal. Display affinity and launch memory left for `domain/display_memory.rs`, owned by the reactor |
| `engine/commands/` | One module per family of `LayoutCommand`: `focus`, `arrange`, `resize`, `strip`, `floating`. `commands.rs` holds `resolve_target`, which is the space, active workspace and active layout every arm needs |
| `domain/virtual_workspace.rs` | `WorkspaceStore`, `VirtualWorkspace`: one global workspace list, each display showing one independently |
| `domain/assignment.rs` | `WorkspaceAssignments`: window → workspace and workspace → windows, one index kept both ways so they cannot disagree |
| `domain/display_affinity.rs` | What belongs to a display and survives a replug: home, column width, strip order. Keyed by display UUID |
| `domain/workspace_focus.rs` | Who takes focus when a workspace becomes active, and where a cycle step lands |
| `domain/launch_memory.rs` | Where an application's windows belong, under a key that outlives the process |
| `domain/floating.rs`, `floating_position_store.rs` | Which windows float and where they sit |
| `domain/hidden_window_placement.rs` | Where off-workspace windows park |
| `domain/workspaces.rs` | `WorkspaceLayouts`: a layout per workspace per display SIZE |
| `engine/persistence/` | `layout.ron`: save, load, restore, and matching a saved window to a live one |

## The shape that matters

**One workspace list, shared by every display.** "coding" is one object with one strip
per display. Before, each display had its own four workspaces that shared a name and
an index, and an unplug scattered windows into whichever workspace shared an ordinal.

**A window's display is durable, not inferred.** Never guess from coordinates: parked
windows sit off screen, macOS refuses to keep a window fully outside every display, so
parked coordinates land on a neighbour and trusting them walks every window onto one
display.

**Nothing durable is keyed by anything that dies.** Space ids die on reconnect,
`WindowId` dies with the process. Display UUID and bundle id are the keys that survive.

**A width that could not be READ is not a width of zero.** `ProjectedWidth` exists
because collapsing those two erased remembered widths on the next autosave.

## Reading order

`domain/virtual_workspace.rs` → `domain/assignment.rs` → `engine.rs::handle_command`, which
resolves a target and hands off to `engine/commands/`
→ `engine/persistence/` when you need the file format.

## Detail

- [`workspaces-and-displays.md`](workspaces-and-displays.md) — every rule about what
  belongs to a workspace versus a display, and the failure each answers
- [`launch-memory.md`](launch-memory.md) — how an application's arrangement survives
  the application not running

## What is layout and what is the machine

`domain/display_affinity.rs` and `domain/launch_memory.rs` describe the MACHINE, not a
layout: which monitor owns which space, which monitor a window belongs to, where an
application's windows go when it comes back. They are one `DisplayMemory`
(`domain/display_memory.rs`), owned by `app::reactor::RiniState` beside the window store
and passed into engine methods the same way.

They used to be two `LayoutEngine` fields written into the same `layout.ron` section as the
layouts, behind the same validation. That made one lifetime out of two: a layout that could
not be trusted discarded the display memory with it, and then every window was re-homed
from scratch on the next display change. `LoadedLayout` now reads the memory before the
layout is validated, so a refusal costs only the strip.
