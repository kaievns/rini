# `workspaces` — virtual workspaces and what persists

Rini's own workspaces, layered over macOS spaces: which windows belong to which
workspace, which workspace each display is showing, and everything that has to survive
a restart. Sits above `windows`, `displays` and `layout`.

## What it owns

| | |
|---|---|
| `engine.rs` | `LayoutEngine`: the orchestrator. Holds the workspaces, the per-display layouts, floating state, app rules, display affinity, launch memory and the persistence journal |
| `domain/virtual_workspace.rs` | `WorkspaceStore`, `VirtualWorkspace`: one global workspace list, each display showing one independently |
| `domain/assignment.rs` | `WorkspaceAssignments`: window → workspace and workspace → windows, one index kept both ways so they cannot disagree |
| `domain/display_affinity.rs` | What belongs to a display and survives a replug: home, column width, strip order. Keyed by display UUID |
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

`domain/virtual_workspace.rs` → `domain/assignment.rs` → `engine.rs::handle_command`
→ `engine/persistence/` when you need the file format.

## Detail

- [`workspaces-and-displays.md`](workspaces-and-displays.md) — every rule about what
  belongs to a workspace versus a display, and the failure each answers
- [`launch-memory.md`](launch-memory.md) — how an application's arrangement survives
  the application not running

## Known debt

`LayoutEngine` has thirteen fields and not all of them sound like layout.
`display_affinity` and `launch_memory` are load-bearing in `layout.ron`, so moving them
is a schema change; tracked in [`docs/implementation-audit.md`](../../../docs/implementation-audit.md).
