# `workspaces` — virtual workspaces and what persists

Rini's own workspaces, layered over macOS spaces: which windows belong to which
workspace, which workspace each display is showing, and everything that has to survive
a restart. Sits above `windows`, `displays` and `layout`.

## What it owns

| | |
|---|---|
| `engine.rs` | `LayoutEngine`: the orchestrator. Holds the workspaces, the per-display layouts, floating state, app rules and the persistence journal, and answers each `LayoutEvent` (`handle_event`) and `EngineCommand` (`handle_command`, the one entry point for every command), which workspace a display shows and who takes focus there. Display affinity and launch memory left for `domain/display_memory.rs`, owned by the reactor; each of its other jobs is one file under `engine/` |
| `engine/discovery.rs` | What an application reports of its windows on a space (`AppWindows`): each admitted under its rules (`admit_window`, an `Admitted`), whether its constraints change a layout (`constraints_change_layout`), and the strip's membership reconciled with the report (`sync_tiled_windows_for_app`); a window added or removed one at a time |
| `engine/rules.rs` | A window's app rules: the decision, the workspace and layer it gives the window, a relaunched window's remembered slot taken first when no rule matches, and the size a tiled rule asks for |
| `engine/navigation.rs` | Focus moving along a strip, one column or row (`FocusStep::Toward`) or to an end of it (`FocusStep::ToEnd`), out of the floating layer into it, and to the display above or below, where a strip ends: `move_focus_internal`, which crosses with `focus_adjacent_space` and stops with `focus_strip_end` |
| `engine/affinity.rs` | What belongs to a display, as the engine writes it into `DisplayMemory`: each window's home, set on intent and never by the settled-topology pass, the strip order a replug rebuilds, the windows to bring back, and the width each window has on each display, remembered on a deliberate resize and applied when it arrives in a strip |
| `engine/launch.rs` | Launch memory's projection, every live window's slot recorded under the attached displays before each save (`remember_launch_slots`), and its lookup, the slot a relaunched window takes (`launch_slot_for_new_window`) |
| `engine/frames.rs` | The frames a layout pass writes for one space: the strip's columns, a column off the strip parked in the corner on its side, the floating layer kept where it is usable and centred where not, maximized floating windows over the tiling area, and every window of a workspace not showing parked bottom-right; `calculate_layout_for_workspace` for the reactor's passes spends what the strip held, and `preview_layout_for_workspace` for a query leaves it |
| `engine/moves.rs` | A window moved to another workspace, to another display's space, and between the strip and the floating layer: all three take it out of its layer (`detach`) and put it into another (`attach`), and the two moves to a workspace go through `take_window` and `place_window`, which also reassign its workspace and put it back where it was when the destination cannot be assigned |
| `engine/commands/` | One dispatcher per family of `EngineCommand`, each matching only its own enum: `focus`, `arrange`, `resize`, `strip`, `floating`, `workspace`. `commands.rs` holds `resolve_target`, the space, active workspace and active layout that `focus`, `arrange`, `resize` and `strip` need, as do `ToggleFocusFloating` and a tiled window's `Maximize`; the `workspace` family, `ToggleWindowFloating` and a floating window's `Maximize` act before any target is resolved |
| `domain/command.rs` | `EngineCommand`: the wire's flat `LayoutCommand` sorted into one enum per family (`FocusCommand`, `ArrangeCommand`, `ResizeCommand`, `StripCommand`, `FloatingCommand`, `WorkspaceCommand`) by one exhaustive `From`, and which workspace commands switch the workspace shown (`switches_workspace`) |
| `domain/virtual_workspace.rs` | `WorkspaceStore`, `VirtualWorkspace`: one global workspace list, each display showing one independently |
| `domain/assignment.rs` | `WorkspaceAssignments`: window → workspace and workspace → windows, one index kept both ways so they cannot disagree |
| `domain/display_affinity.rs` | What belongs to a display and survives a replug: home, column width, strip order. Keyed by display UUID |
| `domain/selector.rs` | Which workspace a `WorkspaceSelector` names (`resolve_workspace`), for a move and for an app rule alike, names compared ignoring case (`same_workspace_name`) as the config compares them, and the end of the stack a `next`/`prev` step runs into (`workspace_stack_direction`) |
| `domain/workspace_focus.rs` | Who takes focus when a workspace becomes active, and where a cycle step lands |
| `domain/app_rules.rs` | What a window's app rule does to workspace policy as one `AppRuleEffects` (its workspace, floating as a `RuleFloating` read against the last rule, position, size, focus), and what the layout does once a window's rules are re-evaluated (`BeforeRules` -> `AfterRules`) |
| `domain/display_pin.rs` | `DisplayRole`: an app pinned to a display by its role, such as the built-in screen, resolved against whatever is attached (`display_for_role`), a default an explicit move beats |
| `domain/display_setup.rs` | `Setup` per `SetupId`, the set of attached display UUIDs: each window's display, the strip order per display and the width per window and display, never shared between arrangements |
| `domain/window_store.rs` | `WindowStore`: the window catalogue with each window's workspace assignment, the operations that touch both kept here so the two cannot disagree |
| `domain/launch_memory.rs` | Where an application's windows belong, under a key that outlives the process |
| `domain/floating.rs`, `domain/floating_position_store.rs` | Which windows float and where they sit |
| `domain/hidden_window_placement.rs` | Where off-workspace windows park |
| `domain/workspaces.rs` | `WorkspaceLayouts`: a layout per workspace per display SIZE |
| `engine/persistence/` | `layout.ron`: save, load, restore, and matching a saved window to a live one |
| `settings.rs` | `VirtualWorkspaceSettings`: which workspaces there are, which one shows at launch, and the window rules that route windows into them |

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

`domain/virtual_workspace.rs` → `domain/assignment.rs` → `domain/command.rs` →
`engine.rs::handle_command`, which hands each family to its dispatcher in `engine/commands/`
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
