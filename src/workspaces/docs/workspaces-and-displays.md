# Workspaces, displays, and what belongs to which

The rules below are the ones the code enforces. Each was arrived at by getting
it wrong first; the failure is recorded so the rule is not relaxed by accident.

## One workspace list, shared by every display

`WorkspaceStore.workspace_order` is one list. Each display records which
workspace it is *showing* and the one it showed before (`active_workspace_per_space`,
a `ShownWorkspace` each) and switches independently, but "coding" is one object
owning one strip per display.

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
  from the layout never appeared in it and relaunched at the default. A width
  command, a drag's end (`events::drag`, `remember_dragged_width`) and a rule's
  resize (`apply_app_rule_resize`, below) each write it now, and so does every removal a
  window can come back from, before the layout lets go of the column: a re-added
  window reads the record, never the layout it left, so a width only the layout
  held (a drag, an app sizing its own window, a restored layout) came back at the
  default. Each writes only a width the column has: a column at the default, or a
  window in no strip, writes nothing and erases nothing.
- **A window's own width beats a rule's.** A rule's size is a window's first
  (`AppRuleSize`), and an app reports its windows on every refresh, which once
  asked for the rule's size each time: a window the user widened went back to the
  rule's width on its app's next report, and a re-added one had its record
  overwritten with the rule's. A rule now asks a tiled window for each value once
  (`AppRuleEffects::tiled_resize`, against the catalogue's `last_rule_size`, which a
  removal clears). Where the window has a width of its own on the display, the rule's
  height is taken and its width is not, and the record stands; the rule's width is
  written only where the window has none, or where the value changed since the rule
  last sized the window (`AppRuleResize::edited`), and a rule never erases a record.
  A rule edited while its window was out of every strip, or while rini was not
  running, finds no size it gave, and is beaten by the window's own width: telling
  that apart needs the rule's value kept with the record, which is not built.
- **A window's own width beats a launch slot's.** A window that comes back with
  no workspace looks up a launch slot, and the slot it takes may be another
  window's, so its width is written only at a window's first sighting, where
  the window has none (`SlotTaking`, `set_window_width_if_absent`).
- **A window new to the session with no width of its own takes its app's.** A
  width command and a drag's end also write the width as the application's on
  the display's shape (`DisplayShape`), which carries across arrangements, and
  a window the session has not seen, that no launch slot or rule sizes, takes it
  as it first joins a strip (`launch-memory.md`, "An application's width"). A
  window seen before, re-added by the window server, does not.
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
  The pass reaches a window homed in the arrangement in force, and any window this
  session has seen with a record in any arrangement, as a removal that keeps widths
  leaves one with no home. A window from the file this session has not seen yet is
  left alone: after a restart a live window is not in the store until its app
  reports it, and a pass then would erase the width a minimize kept.
- **At startup, a window's records go with it unless it can still be found**
  (`forget_windows_gone_since_the_save`, `Erase::GoneSinceTheSave`). A window id is
  the process id and the window server id, both reused after a reboot, so a width
  left under one by a minimized window would size whichever window takes that id.
  A restore candidate keeps its records for the rekey, and a window the window
  server still has for that process keeps them under its id, as one minimized
  across a restart of rini does.
- **Every erase of a width record is logged at `info` with why** (`log_erased_widths`,
  its `Erase`), and with the window's app and title as its persistence fingerprint last
  recorded them, read before the fingerprint is forgotten: the window's removal from
  every layout with its `Removal`, its app quitting, the sweep above, a width command
  putting the column back at the default, or the first arrangement named being one
  already known, which keeps its own records rather than adopting those written before
  any was named (`Unadopted`; only a file that names arrangements and none in force gets
  there). The service's log on 2026-11-04 could not say which path erased a width lost
  across a sleep, because none of them was logged.
- **Only a close erases a window's widths** (`Removal::widths`). `WindowRemoved`
  carries the `Removal`, and every removal forgets the window's workspace, floating
  state, home and place in the strip (`forget_place`). Its widths it keeps, for a
  window that still exists and can come back: a minimize, a drag to another native
  space, a window turning unmanageable on a refresh of the window server's facts or on
  its app reporting it again (one variant each), a rule that no longer manages it, a
  window left out of an authoritative snapshot, and the two removals in
  `reactor/display_homes.rs` of a window found not `EffectivelyManageable`. An id
  superseded on leaving native fullscreen hands its widths to the id that owns the
  window now, where that id has none (`hand_widths`), and logs any it drops. That id
  is laid out before the old one is removed, at the default or its app's width, so the
  removal lays its column at the width handed over (`apply_remembered_width_where_laid`):
  the record alone left it at the default until the next time it arrived in a strip.
  With none handed over, a column laid at the app's width goes back to the default
  (`hand_over_sighting`, `launch-memory.md`). Those two
  removals in `reactor/display_homes.rs` log the window's role, title and state, at
  `info` only when the window still had an assignment, a floating place, a home or a
  strip place: one that stays unmanageable is removed again on every authoritative
  snapshot, and logs those at `debug`. Every removal erased every width until
  2026-11-04, and a window briefly not manageable as the display turned off, or left
  out of one snapshot, came back at the default: reproduced by
  `a_full_width_window_keeps_its_width_across_the_display_sleeping_and_waking` and
  `a_window_the_window_server_removes_and_re_adds_keeps_its_width`, the shape of the
  service's log that day (a width laid full as the display turned off, the user's
  full-width press 80s after the wake; a re-add with no workspace 3s before another).
  Which removal ran that day is not verified: none was logged then.
- **A pin beats an observation and loses to a choice.** A window rule naming
  `open-on-output "internal"` decides where a window belongs when the arrangement has
  not been told otherwise. That needs three levels, not two, so `Setup` records
  which homes the USER chose (`home_by_intent`, written by `set_window_home`)
  separately from the ones inferred from where a window was seen
  (`set_window_home_if_absent`). `set_pinned_home` overrides the second and
  leaves the first alone. Without the distinction the pin either lapsed — an app
  opening on the external kept it, because a home was already present — or
  overrode the user on every settled topology.

  The pin is re-applied on every settled topology rather than recorded, so it is
  never mistaken for a move, and a role nothing fills writes nothing at all.
- **A native space belongs to one display.** `set_display_space` evicts any
  other display claiming the space; without that, two displays both appear to
  own it and the affinity pass moves windows between them forever.
- **Records are per ARRANGEMENT, not per display.** Keying by one display cannot
  express a window that sits on the SAME display in two arrangements and wants a
  different width in each — a terminal full-width on the laptop alone, half when
  the editor comes back from the external to share the screen. Nor can it express
  two arrangements disagreeing about where a window belongs: with one home per
  window the last deliberate move won everywhere, so moving a terminal onto the
  laptop while undocked cancelled its place at the desk and redocking left it
  behind. `display_setup::SetupId` is the sorted set of attached display UUIDs,
  and each arrangement owns its own homes, strip orders and widths.

  Identity, not count. The external at home, the one at the office and a
  meeting-room projector are three arrangements. A display that has never held a
  window takes none when attached — not by a guard, but because the arrangement
  it forms holds no records naming it, so repatriation finds nothing to move.

  `DisplayAffinity` holds the arrangements and remembers which is in force, so
  everything above it asks the same questions as before and gets answers scoped
  to the current one. Three operations deliberately reach across ALL of them:
  forgetting a dead window, forgetting an app, and rekeying a relaunch. A
  `WindowId` that has died can never match again, and the arrangement holding the
  stale record may not be attached for months.

## Restore must not strand windows

- **Saved slots for a detached display are released, keeping only the display
  they belonged to** (`storage.rs`). Restoring them put windows at that
  display's coordinates (measured x=-1680) with nothing there, and nothing
  migrated them back; one dock/undock produced a layout only fixable by
  deleting `layout.ron`. The released windows lay out fresh at default width;
  their affinity survives, so replugging repatriates them.
