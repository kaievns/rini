# Workspaces, displays, and what belongs to which

The rules below are the ones the code enforces. Each was arrived at by getting
it wrong first; the failure is recorded so the rule is not relaxed by accident.

## One workspace list, shared by every display

`WorkspaceStore.workspace_order` is one list. Each display records which
workspace it is *showing* (`active_workspace_per_space`) and switches
independently, but "coding" is one object owning one strip per display.

Before: `HashMap<SpaceId, Vec<VirtualWorkspaceId>>`, four workspaces created
per display. "coding" on the built-in and "coding" on the external were
unrelated objects sharing a name and an index. Moving a window between displays
had to guess a counterpart by ordinal, and an unplug scattered windows into
whichever workspace shared an index, which is what made a display appear to
hold two strips at once. Ordinal is now the position in `workspace_order`;
index 2 is the same workspace everywhere.

Consequences that follow from this and are tested in `virtual_workspace.rs`:

- **A window keeps its workspace when it changes display**
  (`move_window_to_space`). The workspace is the window's identity; only an
  explicit move-to-workspace command changes it. The old ordinal translation
  was also conditional on the target display having no assignments, so a window
  dragged to a display already in use, or a Chrome tab torn off into a new
  window, silently landed on that display's active workspace and "vanished".
- **A new window lands on the workspace its own display is showing**, not the
  first one and not another display's.
- **`remap_space` is a rename.** macOS mints a new space id on every display
  reconnect. This used to migrate workspace objects, deleting whatever had been
  auto-created on the new id and dropping the assignments that referenced it;
  that is why a dock/undock cycle lost windows. Now only the per-display
  "showing" entry and per-display focus move. `WorkspaceAssignments::remap_space`
  merges rather than replaces: macOS has usually already placed windows on the
  new id before rini sees it, and `insert` dropped them out of their workspace.
- **Assignment is one index, kept both ways** (`assignment.rs`): window →
  workspace and workspace → windows are one lookup each and cannot disagree.
  Before, each `VirtualWorkspace` owned a set of member windows, and a window
  could sit in two sets after sleep/wake or a same-space workspace move, which
  leaked into queries and layout recovery. The index lives beside the window
  catalogue (`crate::windows::domain::catalogue`) in `WindowStore`; the operations that
  touch both, such as removing a window or rekeying its identity, go through
  that facade so neither half is updated without the other.
- **Last focus is per (workspace, display)** (`VirtualWorkspace.last_focused`),
  because a workspace has a strip per display.
- **Focus memory is not cleared when focus leaves the workspace.** That runs on
  every ordinary focus change to another display or workspace; clearing it is
  why switching back always landed on the first column. It is cleaned up when
  the window is closed or forgotten.

## Display affinity: width and home belong to the display

`DisplayAffinity` (`display_affinity.rs`) is keyed by display UUID because
macOS mints a fresh space id on every reconnect (one monitor observed as 479,
484, 487, 516, 552, 1138 in a session).

- **Column width is per (window, display).** Half of 2338pt is a sensible
  default and half of 1728pt is cramped; one width cannot serve both. It lives
  here, not in the layout tree, because a tree is per workspace: with the width
  inferred from how many columns a workspace held, a full-size window moved
  from workspace 1 to 2 to 3 went half-size on 2 (other windows) and full on 3
  (empty). A fresh column starts at the display default, so the recorded width
  has to be re-applied when the window enters a tree; recording it in the
  affinity alone changed nothing.
- **Width comes from the layout, not only from commands.** The affinity map was
  once written only by explicit width commands, so a window whose width came
  from the layout never appeared in it and relaunched at the default.
- **A home is written once, on first sighting or an explicit move.** The forced
  reassignment after an unplug must not write it: an unplug evacuates windows
  onto the remaining display, and recording that as home destroys the record
  that brings them back on replug. A home is only written if the space's
  display was known at the time, so a window seen before that mapping existed
  has none.
- **A home is written by an intent, never by an observation.** Four things write it:
  a drag to another display (`events::drag`), an explicit move command
  (`events::command`), a first sighting (`note_window_display_home`), and a restore.
  All four are moments where something asked for the window to be somewhere.

  The settled-topology pass (`sync_display_affinity`) only records the strip order and
  homes windows that have none yet. It used to re-home any window it merely SAW on a
  display, which is circular: the pass observes the result of rini's own layout, and
  that layout already follows from the home. So every reason a window was temporarily
  elsewhere — parked, evacuated on an unplug, laid out on a neighbour — became its new
  address, and it never came back on a replug. That is the "windows teleport between
  displays" report. Windows homed to a DETACHED display were already spared as the
  evacuation case; the attached case was the one that moved.

  Written-once-never-revised was the original behaviour and it went stale the moment
  the user rearranged anything, which is why the observation pass was added. The fix is
  not to observe harder: it is that a drag is an intent and writes the home itself.

- **Affinity for closed windows is dropped on every settled topology**
  (`forget_affinity_for_dead_windows`). It was only cleared on the `WindowRemoved` path,
  not `WindowRemovedPreserveFloating`, which the display-change path uses.
  Measured: the external's affinity list held three closed windows while all
  fourteen live windows were homed to the built-in; repatriation reported
  `homed=[3 windows] to_move=[]` and the external came back empty every time.
- **A native space belongs to one display.** `set_display_space` evicts any
  other display claiming the space; without that, two displays both appear to
  own it and the affinity pass moves windows between them forever.

## Restore must not strand windows

- **Saved slots for a detached display are released, keeping only the display
  they belonged to** (`storage.rs`). Restoring them put windows at that
  display's coordinates (measured x=-1680) with nothing there, and nothing
  migrated them back; one dock/undock produced a layout only fixable by
  deleting `layout.ron`. The released windows lay out fresh at default width;
  their affinity survives, so replugging repatriates them.
- **Restoring one display's strip must not overwrite the others.** A workspace's
  `layout_system` holds a strip per display; replacing the whole
  `VirtualWorkspace` object (safe when a workspace had one display) wiped every
  other display's strip. Only this display's focused window and strip are taken
  from the snapshot; name and mode are current-session metadata.
