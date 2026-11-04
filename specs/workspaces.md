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

## Up and down a column and on through the stack

- **`focus-window-or-workspace-up` and `-down` MUST move focus up or down the focused window's
  column, and where the column ends that way, holds one window, or the workspace holds none, MUST
  switch to the workspace above or below exactly as `focus-workspace-up` and `-down` do**: the same
  window focused, the same slide under every look, and at the end of the stack the same bounce. One
  key then walks a column and the stack, as niri's do.
- `move-window-up-or-to-workspace-up` and `move-window-down-or-to-workspace-down` MUST move the
  focused window up or down its column, and where the column ends MUST move it to the workspace
  that way with focus following, as `move-window-to-workspace-up` and `-down` do with `focus=true`.
- With a floating window focused, the focus forms MUST act as their press within the column does,
  which takes focus back to the strip, and the move forms MUST move the floating window to the
  workspace that way with focus following: a floating window is in no column, so a move within one
  would move the strip's selection, not the window focused, and could carry it to another display.
  A press of either MUST NOT carry a window to another display.
- They MUST hold wherever a bind runs, the overview included, since it runs the real actions. The
  names are niri's, so a niri config's binds of them work as written. `focus-workspace-up` and
  `-down`, the moves to a workspace and the display forms, `focus-window-or-monitor-up` among them,
  MUST stay as they are.

> **Reported 2026-10-19.** "i'll need to change the keybindings/navigation logic. and that is
> globally, not just for the overview. instead of having the dedicated p/; buttons to switch between
> workspaces, i want to use the i/k buttons that we use for moving up/down in the columns, the new
> logic will basically combine them, when i'm reaching the top of a column (or there is just one
> window) the next "i" will move to the upper workspace, "k" for down works similarly. you can keep
> the p/; around too, i primarily want to change the meta-i/k behaviour." Kai's `Mod` is Alt, and
> his `Mod+I` and `Mod+K` were `focus-window-or-monitor-up` and `-down`, which cross to the display
> above or below instead. niri's four actions had been refused as niri's alone. The shipped binds
> keep `Mod+J` and `Mod+K` on the display forms and write the workspace forms beside them.
>
> **Found 2026-10-19, not reported.** Review found that a move with a floating window focused took
> the column's move, which works on the strip's selection and not the floating window: at its
> column's end on the lower of two stacked displays it carried that tiled window to the display
> above. The move forms now move the floating window to the workspace instead.

## Relaunching an application

- Where an application's windows belong MUST survive the application quitting, keyed so it does not
  depend on a process id: by bundle identifier and display topology.
- A relaunched window MUST return to its remembered workspace AND its remembered width.
- A window rule MUST override a remembered place only for what it sets. A rule that pins a display
  or names a workspace MUST leave the window its remembered width, and one that names no workspace
  its remembered workspace. A rule that floats a window or stops managing it MUST take nothing
  from a remembered place: the window opens where it would with none remembered, on the workspace
  on screen. A modal or a settings window has no column to keep, and the place its app left can be
  on another workspace, where it would open out of view. A rule's width MUST win over a remembered
  width written before the rule set that value, as a rule's width is a window's first
  (`specs/displays.md`). A width the user gave the window since, while the rule's value stayed the
  same, MUST survive the relaunch. Not yet met: the rule's width wins at a relaunch, changed or
  not, so a width the user gave a window a rule sizes is lost at its app's relaunch: telling the
  two apart needs the rule's value kept with the place.

> **Reported 2026-11-04.** "everything defaults to 1/2 of my laptop screen, and even though we
> have the prevous sizes tracking it's buggy as fuck and doesn't work all that well, so i keep
> getting everything resetting back to 1/2 which is annoying". Any rule turned the remembered
> place off for its app: Kai's `open-on-output "internal"` rule for Slack, Outlook and Messages
> sets no width, and Outlook's places held a full width under 5 of its 5 arrangements, written and
> never read. Slack reset 2 minutes after a cold start on 2026-11-03 (inferred to be this).

> **Found 2026-11-04, not reported.** Review found that taking the place under every rule sent a
> floated window to its place's workspace, with nothing switching the view to it. Kai's rules
> float every modal, Zoom, 1Password, System Settings and Finder's dialogs, and System Settings'
> places sit on workspace 2 ("comms") in `~/.rini/layout.ron`. A floated window now takes no place.
- A window a screen test opens MUST NOT leave a remembered place or take one, and a place one left
  in a file MUST be dropped as the file is read. A test's windows are not the user's, and matched
  by order, the place a test window left hands its workspace and width to the user's next window
  of that application.

> **Found 2026-11-04, not reported.** Kai's `~/.rini/layout.ron` held 18 places for focuslab's
> `lab-A` to `lab-D` among Ghostty's 47, under 3 of its 5 arrangements.

- A window that takes no remembered place MUST say why in the log, at a level the running service
  keeps, because a window that comes back at the default width is otherwise not explained by
  anything the service writes. A window a rule floats or leaves unmanaged is the exception: it
  takes no place by design and MUST NOT be logged, since every modal would log a line at that
  level (`src/workspaces/docs/launch-memory.md`, "Precedence").

> **Found 2026-11-04, not reported.** Looking into Kai's report that day ("everything defaults to
> 1/2 of my laptop screen, and even though we have the prevous sizes tracking it's buggy as fuck
> and doesn't work all that well") found every way the lookup gives up logged at `debug`, below
> the `info` the service keeps: the service's log for the day held no line about a remembered
> place.

## Where it lives

`src/workspaces/domain/virtual_workspace.rs` is the store, `src/workspaces/domain/selector.rs` which
workspace a selector names and how names compare, `src/workspaces/domain/launch_memory.rs` the
relaunch records, `src/workspaces/engine/launch.rs` what is recorded in them and the slot a relaunched
window takes, `src/workspaces/engine/rules.rs` and `src/workspaces/engine/discovery.rs` where it takes
it, `src/workspaces/engine/affinity.rs` the width it arrives at, `src/workspaces/engine/commands/workspace.rs`
the switches and moves to a workspace, `src/workspaces/engine/commands/cross.rs` and
`src/workspaces/domain/command.rs` a press on through the stack, and `src/workspaces/engine.rs` the
orchestration. Measurements are in `src/workspaces/docs/launch-memory.md` and
`src/workspaces/docs/workspaces-and-displays.md`.
