# Workspaces

Workspaces are stacked VERTICALLY. Moving up and down moves through the stack; moving left and right
moves along the strip within one workspace.

## Identity and order

- The workspace ORDER is global: index 2 is the same workspace on every display.
- The ACTIVE workspace is per space. A space rini has never seen lists the same workspaces and has none
  of them active.
- A workspace's configured name comes from the config and MUST survive a restore. A restore reads a
  saved layout, not a saved set of names.

## What belongs to a workspace

- Every managed window belongs to exactly one workspace on one space.
- A workspace holding windows whose display is no longer attached MUST be reported as orphaned rather
  than silently kept: its windows are unreachable from any strip.
- A window a space owns but its layout tree does not hold MUST be reported as orphaned. Such a window
  is reachable by cmd-tab and unreachable by scrolling, which is what "a second invisible strip" looks
  like from the user's side. Floating windows are excluded, because being outside the tree is what
  floating means.

## Moving between workspaces

- Moving a window to another workspace MUST NOT follow it. The user asked to send the window away, not
  to go with it. Following is available to the CLI, which asks explicitly.
- A move acts on the space the window is actually on, not the space the command named.
- `next` and `prev` are matched as workspace NAMES before the name lookup, so a workspace actually
  named "next" cannot be selected by name. This is a known limitation, not an accident.

## Relaunching an application

- Where an application's windows belong MUST survive the application quitting, keyed so it does not
  depend on a process id: by bundle identifier and display topology.
- A relaunched window MUST return to its remembered workspace AND its remembered width.

## Where it lives

`src/workspaces/domain/virtual_workspace.rs` is the store, `src/workspaces/domain/launch_memory.rs` the
relaunch records, `src/workspaces/engine.rs` the orchestration. Measurements are in
`src/workspaces/docs/launch-memory.md` and `src/workspaces/docs/workspaces-and-displays.md`.
