# Remembering where an application's windows belong

A window relaunched after a reboot arrived as a default-width column in whatever
workspace happened to be active. Everything needed to do better was already recorded
and already persisted — it was just keyed by something that does not survive the
application.

## What was already there

`DisplayAffinity` records, per window:

- `window_home`: the display UUID the window belongs to, written on first sighting and
  on an explicit move, never by the forced reassignment that follows an unplug.
- `window_width`: the `ColumnWidth` it last had **on each display**, stored as a ratio,
  an unbounded ratio (`Unbounded`, which no width bound holds, as a drag gives) or the
  `FullWidth` mode rather than in points, so it survives a resolution change. The file holds
  an unbounded ratio as an `Offset`, with the window listed beside it in `dragged_width` (a
  launch slot's `width_dragged`), keys a rini from before them skips: an unknown variant would
  have failed the whole file, and the start that follows forgets every home, width and slot.

`WorkspaceAssignments` (`src/workspaces/domain/assignment.rs`) records which workspace
each window is assigned to. All three are written to `~/.rini/layout.ron` on save.

All three are keyed by `WindowId { pid, idx }`. A pid is gone the moment the
application quits, so every one of those records is dead weight on the next boot:

```
Loaded persisted layout for startup restore, saved_windows=14, restore_candidates=0
```

## The durable key

`LaunchMemory` holds the same three facts under a key that outlives the process:

```
app_id  ->  topology  ->  [ Slot { title, display_uuid, workspace_index, width } ]
```

- **`app_id`** is the bundle identifier. It is the only application-level name that is
  stable across launches; the pid and the window server id are not, and the executable
  path is not always available.
- **`topology`** is the set of display UUIDs currently connected, as the `SetupId` display
  affinity keys its setups by (`src/workspaces/domain/display_setup.rs`): sorted, each UUID once, and none not
  reported yet. A two-display key is the two UUIDs joined by `+`. This is what
  makes "50% on the external, 100% on the built-in" expressible: the same application
  gets a different answer depending on what is plugged in, and the answer for a
  topology is only consulted when that topology is present again. The precedent is
  `workspace_layouts`, which already keys its configurations by display size.
- **`workspace_index`** rather than `VirtualWorkspaceId`. The id is a slotmap key that
  does persist, but the index is what the user means by "workspace 2" and it survives a
  layout file being restored onto a different set of workspaces.

## Matching a new window to a slot

An application with four windows needs four slots, or every window it restores lands in
one workspace at one size. Slots are matched in order:

1. **By title.** Exact match against the remembered title. Meaningful for the
   applications this matters for: a terminal's title is its working directory, an
   editor's is its project, a notes app's is its vault.
2. **By ordinal.** The Nth window of the application to appear takes the Nth unclaimed
   slot. Covers applications whose titles are page titles and change every session.
3. **Not at all.** A window beyond the remembered slots gets its application's width on
   this display shape ("An application's width"), or today's defaults.

A slot is claimed once, so two windows cannot both take it.

A focuslab window, a Ghostty window `tools/focuslab/lab.sh` titled `lab-` and one ASCII
letter (`is_focuslab_window`), leaves no slot and takes none, and the lookup counts it as no sibling,
since it left no slot to claim. The slots a file already holds for one are dropped as it loads.
On 2026-11-04 `~/.rini/layout.ron` held 18 of them among Ghostty's 47 slots, under 3 of its 5
arrangements, each after the terminals' own, so a terminal opened past its siblings' slots
would take a lab window's by ordinal: its workspace and its width. A longer title, `lab-notes`
or `lab-AB`, can be a terminal's own and keeps its slot. A test reads `lab.sh` for the title
and its default letters, so retitling the lab windows there fails it.

## Precedence

Explicit configuration wins over anything learned, one property at a time:

1. A `window-rule` from the config file that applies to the window, for what it sets: a workspace
   (`open-on-workspace`) and a width (`default-column-width`). A display pin does not override the
   slot's home here: the slot's home is one rini inferred, which `set_pinned_home` corrects
   wherever homes are decided (`workspaces-and-displays.md`).
2. A remembered slot for this topology, for the rest (`SlotOverrides`). Its width is written only
   at the window's first sighting in the session (`SlotTaking`), and only where the window has
   none of its own on that display. A window the window server took away and gives back has no
   workspace and no home, so it looks up a slot too and is matched after its siblings, to a slot
   that may be another window's: it takes that slot's workspace, as it did before widths were
   kept across a removal, and keeps its own width or the default it had
   (`a_window_given_back_takes_no_width_from_a_launch_slot`). Not met: it can land on that
   sibling's workspace, since a removal forgets the workspace; taking no slot at all would send
   every window back from a wake to the active workspace instead.
3. For a window new to the session that took no slot and whose rules set no width, the width
   its application was last given on a display of this shape ("An application's width").
4. Today's default: active workspace, display under the cursor, configured column width.

A window a rule floats (`open-floating true`) or leaves unmanaged takes nothing from a slot, and
opens where today's default puts it (`SlotOverrides::of` gives `None`). Taken, the slot sent every
floated modal and System Settings window to the slot's workspace, out of view. It is not logged:
every modal would log it at `info`.

A rule once turned the slot off whole, so a rule that only pinned a display cost the app its
remembered width at every relaunch: on 2026-11-04 Outlook's slots in `~/.rini/layout.ron` held a
full width under 5 of its 5 arrangements, written and never read, behind Kai's
`open-on-output "internal"` rule for Slack, Outlook and Messages.

A rule's width beats the slot's even where the user widened the window since the rule sized it:
the slot keeps no record of the rule's value it was written under, so telling a user's width from
a rule's edited since is not built (`specs/workspaces.md`, "Relaunching an application"). This
order departs on purpose from "Precedence, in KWin's words" in the window sizing research, not
yet on `main` (branch `sizing/research`), which puts the slot above a rule's default and lets a
rule's width win only once its value changed. That order needs the rule's value stored with the slot, a file format
change, and this list is rewritten to it when that record lands.

## An application's width

A slot holds one window under one arrangement, so a new window of a running application, a
window past its siblings' slots, and every application at a desk not seen before opened at
`default-column-width` (Kai's report of 2026-11-04, `specs/displays.md`). `AppWidths`
(`src/workspaces/domain/app_widths.rs`) keeps, per bundle id, the width the user last gave the
application on each `DisplayShape`: the points the display's frame leaves for windows and its
physical width to the nearest 10mm, from `CGDisplayScreenSize`. The key names no arrangement and
no display UUID, so the same panel docked and alone, and one model of monitor at two desks, share
it. Kai allowed that on 2026-11-04 ("2. yes"); a window's own records still share nothing.

- **Written** by a width command (the preset cycle, resize-by, the full-width toggle and a title
  bar's zoom, through `remember_given_width`) and a drag's end (`remember_dragged_width`). A
  width back at the default forgets the shape's width, so the next window opens at the default.
  Resize-by writes only when the column's width changed: a height key, or `Smart` in a stack,
  changes a row's height, and writing the column's default would forget the app's width.
  Not by a rule's resize, nor by a width given to a window a rule sized (`last_rule_size` holds a
  width): a rule's width is the rule's, and written as the app's it would size the app's other
  windows. Not by a focuslab window. Not before a removal or a move to another workspace, which
  record a width but give none.
- **Read** for a window the session has not seen (`first_sighting`), with no workspace yet, that
  took no slot and whose rules set no width, as it first joins a strip on the display it was
  first seen on (`take_app_width`, `take_arrival_width`), unless it has a width of its own there.
  A window the window server takes away and gives back has been seen, so it keeps its own width
  or the default it had rather than a sibling's since.
- **Held apart from the window's record** until it arrives, so a width of its own there wins at
  that arrival (`a_window_s_own_width_beats_its_app_s_at_the_same_arrival`).
- **Not for a window's new id.** A window back from native fullscreen under a new id is one the
  session has seen. When the old id is removed as `OwnedByAnotherId`, the new id counts as seen
  and drops any app width still pending (`hand_over_sighting`). Laid at its app's width already,
  it goes back to the default unless its old id handed it a width there, which is laid instead.
- **Absolute.** `AppWidth` is a fraction of the display, not an offset from
  `default-column-width` as `ColumnWidth::Offset` is, so a different default on another display
  reads it as the same width. The window records and the slots are still offsets.
- **With the default it overrode** (`Overrode::DefaultColumnWidth`). A width made under another
  value is not read, so an edited default shows on new windows rather than being beaten by a width
  chosen against the old one. It is kept until the application's next write, which drops it. It
  was once dropped as the config was read and at start, so a default read for a while, the
  reference config's 0.7 when Kai's 0.5 failed to parse or a `config set` trial, erased every
  application's width for good, inferred from the code and not seen (`an_app_s_width_is_read_again_once_its_default_column_width_is_back`).
- **Capped** at `APP_WIDTHS_PER_APP`, 8 shapes per application, the most recently set first.
- **In the file** as `display_memory.app_widths`, absent while empty, so a file with none is
  written back as it was. A rini from before it skips the key and reads the rest
  (`a_file_with_app_widths_is_read_whole_by_the_rini_before_them`).
- **Readable past additions.** Each record is a struct of optional fields with no enum in it
  (`StoredRecord`): a width is `(fraction:Some(0.66667))`, `(fraction:..,unbounded:true)` or
  `(full:true)`, and its default `(default_column_width:Some(0.5))`. A field a later rini adds is
  skipped. A record left without a field this rini reads, as a width held only in points or one
  made under a rule rather than the default, is dropped alone, and the file loads with every
  other record, home and slot (`a_file_from_a_later_rini_loads_every_app_width_it_can_read_and_everything_else`).
  A later kind MUST leave those fields empty rather than fill them with a value this rini would
  misread. The dropped records are not written back by this rini.

Not built: a rule's width per display, an application's width carried as points from a display
of another shape, and a default per class of application, which the window sizing research names
Model B. `Overrode` is where a rule's value would be stored for a rule to rank against, in a
field of its own beside `default_column_width`. Not met
either: an existing window moved onto a display it has no width on takes the default there, not
its application's.

## Recording

The memory is a projection of live state, computed when the layout is saved rather than
maintained by hooks on every move and resize. One write path instead of five, and no new
work in the hot paths that place windows. The autosave runs within 5s of any layout
change, so the file is current when the machine goes down.

An EMPTY projection is ignored rather than written. Producing no slots for an
application means its windows could not be read, not that it should be forgotten, and
forgetting is the one outcome nothing can recover from — holding the arrangement while
the application is not running is the entire point. Measured live: TextEdit was
recorded in workspace 1, then vanished from the file, because rini had stopped tracking
its window while the process was still alive and the projection had produced nothing.

The same rule applies per FIELD, which it did not at first, and that is what kept
sending full-width windows back at half size. A width has three states, not two, and
`ProjectedWidth` is the type that says so:

| state | meaning | what `remember` stores |
|---|---|---|
| `Known(Some(w))` | the layout holding the window says `w` | `w` |
| `Known(None)` | the layout holds the window at the display default | nothing, so Mod+F can be undone across a relaunch |
| `Unknown` | no layout holding the window could be consulted | whatever is already remembered |

Collapsing `Unknown` into `Known(None)` is a data-destroying bug rather than a missed
optimisation: the autosave runs every few seconds, so ONE projection that could not read
the layout overwrote `FullWidth` with nothing, permanently. The window then relaunched at
the default width and no record survived to say otherwise, which is why pressing Mod+F
again was the only fix and why the same report came back three times.
`resolve_width` inherits by title first, then by position, matching `slot_for_window`.

Reading the width consults EVERY layout configuration of the workspace, not only the
active display size (`width_from_layouts` over `configurations_for`). A workspace holds a
strip per display size, so a window made full width on the external is recorded in that
size's layout, and while the built-in is showing, the active layout knows nothing about it.

Every way the lookup can give up is logged, as a `NoLaunchSlot` in the `gave_up` field:
`launch_slot_for_new_window` returned `None` for four different reasons in silence, so each
time a width went missing the reason had to be reconstructed from the layout file. They are
`info`, the lowest level the service keeps (`RUST_LOG=error,warn,info` in the launch agent):
at `debug`, `grep -c "remembered\|relaunched"` over the service's log on 2026-11-04 found 0.
A missing topology is a `warn`, because nothing remembered can be found at all until it
arrives. Two give-ups happen outside the lookup and are logged the same way: window rules that set
both a workspace and a width for a window whose application left slots (`RulesSetAll`, all the
slot would give), and a slot whose workspace the window cannot go to (`WorkspaceRefused`).

Two lookups also have to be right, and both were wrong first time:

- The window's space comes from `workspace_info_for_window_any`, not
  `space_with_window`. The latter searches each space's ACTIVE workspace tree, so a
  window parked in a workspace nobody was looking at was invisible to it.
- Its display is `window_home`, falling back to the display its space is on. A home is
  written once, on first sighting, and only if the space's display was known by then, so
  a window that appeared before that mapping existed has none — forever. Seen live on a
  window rini tracked and had assigned to a workspace.

## Applying, and where the first sighting actually happens

Not `WindowAdded`. A launching application's windows are first seen while the app rules
are applied, from `WindowsOnScreenUpdated`, and **the window is not in the window store
yet at that point**. The first version read the bundle id and title from the store
there, found nothing, and returned before it had even consulted the memory — the whole
feature was silently dead, with no log line to say so. Identity comes from the caller,
which has both as parameters.

### The record is not what decides the width

Three attempts at the width, and the lookup was right in all three. The log said so:

```
matched a remembered slot, idx=9789, app_id="com.apple.TextEdit", slot=0, of=1
placing a relaunched window where it was, idx=9789, workspace=1, width=Some(FullWidth)
```

The workspace was applied and the width was not, because the width was being written to the
`DisplayAffinity` record and nothing was telling the LAYOUT. `apply_remembered_column_width`
exists for exactly that and was wired only to the paths that MOVE a window between
workspaces or displays.

Wiring it into `add_window_to_layout` was still wrong, and the absence of a log line is what
proved it: a launching application's windows never go through that function. They arrive in
bulk through `sync_tiled_windows_for_app` -> `set_windows_for_app`, driven by the same
app-rule discovery as the workspace. The width is applied there, to the difference between
the desired and current membership, so an existing window's width is never re-applied over
something the user has just changed.

Two things follow from this that are easy to get wrong again:

- A width is read from the window's own layout, not from the affinity record. That record
  holds a width only where something recorded one (a width command, a drag's end, a rule's
  resize, a removal; `workspaces-and-displays.md`), and when it was written by width commands
  alone the file read `window_width:{}` while every window on screen had a width.
- `is_window_full_width` and the tree's `is_fullscreen_within_gaps` are the same flag under
  two names. `ColumnWidth::FullWidth` is that flag, not a ratio.

Verified live, end to end. TextEdit moved to workspace 1, recorded, quit, and relaunched
while workspace 3 was active:

```
placing a relaunched window where it was, idx=8784, workspace=1
TextEdit: ws 1        active workspace before the relaunch: 3
```

Not yet verified: the two-topology case, which needs the external display physically
plugged and unplugged. The keying and the per-topology answers are unit tested; what has
not been observed on real hardware is a second topology being recorded and chosen.
