# What survives a restart

## The two lifetimes

A saved file holds two records with different lifetimes, and they MUST be readable independently:

- **The layout** — which windows are in which columns, in which workspaces, at what widths.
- **The machine's memory** — which display owns which space, which display each window belongs to,
  and where each application's windows go when it comes back.

A layout that cannot be trusted MUST NOT cost the machine's memory. Re-homing every window from
scratch and landing every relaunched application in a default slot is a worse outcome than laying out
the strip fresh.

> **Found 2026-09-23, not reported.** The two shared a file section and a single validation, so a layout
> from a newer schema — or one that failed validation — discarded the display memory with it.

## Refusing a file

- A file written by a NEWER schema version MUST be refused rather than half-read.
- A file written by an OLDER schema version MUST be refused too, layout and all, and the refusal MUST
  say which version it is. rini reads only the current shape, so an older file read as one would
  come back without the window records its own shape held.

> **Reported 2026-10-15.** Kai's decision in the dead-code sweep: delete every reader of files older
> than schema 6. Kai's `~/.rini/layout.ron` was already at 6, and the readers for 0 to 3 could not
> run on any real file, which the layout-system tag check or RON refused first.
- A file whose workspace topology, layouts, floating positions or window records are invalid MUST be
  refused at the load boundary. Admitting one creates windows no later cleanup can identify.
- A refusal MUST say what is wrong and what follows from it, because the user's only other signal is
  that their layout is gone.
- A broken CONFIG file (`config.kdl`) MUST NOT stop rini starting. One mistyped binding used to take
  the window manager down, and with nothing managing windows there is no way to open an editor to fix
  it. rini reports the problem and starts with the built-in defaults. `specs/config.md` has the rest of
  what a config owes.

## Restoring

- Restore is ON by default, so a restart or a redeploy keeps window sizes, workspaces and strip
  positions. Not yet met: the scroll offset is not saved, so a column as wide as the viewport comes
  back at its width but not where it stood.
- A saved identity MUST NOT be matched to the wrong live window. Both identities are unreliable —
  rini's own dies with the process, the window server's is recycled — so a match needs evidence
  specific to the window: its title and size within a known application.
- A scoped restore MUST NOT consume a live window from outside its scope.
- Restoring MUST NOT strand a window at coordinates no attached display covers.

## Saving

- A save MUST NOT write a value it could not read. "Could not read" and "is not set" are different
  answers, and writing the second for the first erases what was remembered.
- A save MUST NOT write an origin hint that has no corresponding saved layout: a stale observation is
  worse than none, because it makes a portable file look unambiguous.
- The autosave and the quit save MUST forget what is kept under a native space that no attached
  display shows, that no display is remembered on, that is not the saved active space, and that no
  window is on: its layouts, which workspace it showed and the focus remembered on it. macOS mints a
  new space id on every reconnect and never reissues one, so such a space is one nothing reaches
  again. A space a display is remembered on MUST stay, windows or none, because the next start moves
  its layouts and the workspace it showed onto the display's new id; a space a window is on MUST stay,
  because that window can come back to it. Nothing is forgotten before the startup restore has moved
  the saved spaces onto this session's ids, since until then every one of them looks dead.

> **Reported 2026-10-15.** The dead-code sweep found Kai's `~/.rini/layout.ron` holding 68 workspace
> layouts over 17 native space ids, only 3 of them mapped to a display. Kai's decision: fix it. On
> the copy taken that day, the rule forgets 14 of 18 space ids, all of them empty, and keeps the 4
> displays' spaces; the file goes from 56069 to 38532 bytes.

## Where it lives

`src/workspaces/engine/persistence/` is the whole of it: `storage.rs` loads and saves, `snapshot.rs` is
the file shape, `restore.rs` the scoping, `matcher.rs` the identity rules. Which native spaces a save
forgets is decided in `src/workspaces/domain/workspaces.rs`.
