# Remembering where an application's windows belong

A window relaunched after a reboot arrived as a default-width column in whatever
workspace happened to be active. Everything needed to do better was already recorded
and already persisted — it was just keyed by something that does not survive the
application.

## What was already there

`DisplayAffinity` records, per window:

- `window_home`: the display UUID the window belongs to, written on first sighting and
  on an explicit move, never by the forced reassignment that follows an unplug.
- `window_width`: the `ColumnWidth` it last had **on each display**, stored as a ratio
  or the `FullWidth` mode rather than in points, so it survives a resolution change.

`VirtualWorkspaceManager` records which workspace each window is assigned to. All three
are written to `~/.rini/layout.ron` on save.

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
- **`topology`** is the set of display UUIDs currently connected, sorted. This is what
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
3. **Not at all.** A window beyond the remembered slots gets today's defaults.

A slot is claimed once, so two windows cannot both take it.

## Precedence

Explicit configuration wins over anything learned:

1. An `[[windows]]` rule from the config file.
2. A remembered slot for this topology.
3. Today's default: active workspace, display under the cursor, configured column width.

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

Verified live, end to end. TextEdit moved to workspace 1, recorded, quit, and relaunched
while workspace 3 was active:

```
placing a relaunched window where it was, idx=8784, workspace=1
TextEdit: ws 1        active workspace before the relaunch: 3
```

Not yet verified: the two-topology case, which needs the external display physically
plugged and unplugged. The keying and the per-topology answers are unit tested; what has
not been observed on real hardware is a second topology being recorded and chosen.
