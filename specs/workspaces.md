# Workspaces

Workspaces are stacked VERTICALLY. Moving up and down moves through the stack; moving left and right
moves along the strip within one workspace.

## Identity and order

- The workspace ORDER is global: index 2 is the same workspace on every display.
- The ACTIVE workspace is per space. A space rini has never seen lists the same workspaces and has none
  of them active.
- A workspace's configured name comes from the config's `workspace "name"` nodes and MUST survive a
  restore. A restore reads a saved layout, not a saved set of names.
- The workspaces are the config's `workspace` nodes, in their order, and rini MUST NOT add a
  workspace the config does not declare. A reload that declares one more adds it; nothing else
  does. A workspace added at run time has no name the config knows and no place in its order, and
  niri has no action that adds one.
- Workspace names MUST compare ignoring case wherever a name is looked up: two `workspace` nodes
  that differ only in case are refused as one declared twice, and a bind, a window rule or a command
  naming `web` reaches the workspace declared `Web`.

> **Reported 2026-10-15.** Kai's decision in the dead-code sweep: delete `create-workspace`. It
> appended an unnamed "Workspace N" after the configured list, and neither Kai's config nor the
> shipped binds used it.

## What belongs to a workspace

- Every managed window belongs to exactly one workspace on one space.
- A workspace holding windows whose display is no longer attached MUST be reported as orphaned rather
  than silently kept: its windows are unreachable from any strip.
- A window a space owns but its layout tree does not hold MUST be reported as orphaned. Such a window
  is reachable by cmd-tab and unreachable by scrolling, which is what "a second invisible strip" looks
  like from the user's side. Floating windows are excluded, because being outside the tree is what
  floating means.

## Moving between workspaces

- Switching to the workspace already shown MUST do nothing, as in niri by default;
  `focus-workspace-previous` is how to go back. niri's `input › workspace-auto-back-and-forth` MUST
  be refused with the reason.

> **Reported 2026-10-15.** Kai's decision: `workspace-auto-back-and-forth` goes. It is niri's option,
> dropped anyway: neither Kai's config nor the default turned it on.

- The default binds MUST move a window to another workspace without following it. The user asked to
  send the window away, not to go with it.
- The config's move actions MUST follow the window unless told `focus=false`, because niri's do and a
  niri config has to mean the same thing here. The shipped binds say `focus=false`.
- The CLI MUST NOT follow unless asked, with `--follow`.
- A move acts on the space the window is actually on, not the space the command named.
- A move's `next` and `prev` are steps through the stack, not names. Neither wraps, and a step past
  the end bounces. On the wire only the exact lowercase strings `"next"` and `"prev"` are steps,
  read before any name. Any other spelling is a name, compared ignoring case, so an IPC client
  reaches a workspace called "next" as `"Next"`. Binds name a workspace by its place in the config
  and window rules by its name, so a workspace called "next" is reachable from both.

## Relaunching an application

- Where an application's windows belong MUST survive the application quitting, keyed so it does not
  depend on a process id: by bundle identifier and display topology.
- A relaunched window MUST return to its remembered workspace AND its remembered width.

## Where it lives

`src/workspaces/domain/virtual_workspace.rs` is the store, `src/workspaces/domain/selector.rs` which
workspace a selector names and how names compare, `src/workspaces/domain/launch_memory.rs` the
relaunch records, `src/workspaces/engine/launch.rs` what is recorded in them and the slot a relaunched
window takes, `src/workspaces/engine/rules.rs` and `src/workspaces/engine/discovery.rs` where it takes
it, `src/workspaces/engine/affinity.rs` the width it arrives at, and `src/workspaces/engine.rs` the
orchestration. Measurements are in
`src/workspaces/docs/launch-memory.md` and `src/workspaces/docs/workspaces-and-displays.md`.
