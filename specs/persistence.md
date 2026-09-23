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
- A file whose workspace topology, layouts, floating positions or window records are invalid MUST be
  refused at the load boundary. Admitting one creates windows no later cleanup can identify.
- A refusal MUST say what is wrong and what follows from it, because the user's only other signal is
  that their layout is gone.
- A broken CONFIG file MUST NOT stop rini starting. One mistyped binding used to take the window
  manager down, and with nothing managing windows there is no way to open an editor to fix it. rini
  reports the problem and starts with the built-in defaults.

## Restoring

- Restore is ON by default, so a restart or a redeploy keeps window sizes, workspaces and strip
  positions.
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

## Where it lives

`src/workspaces/engine/persistence/` is the whole of it: `storage.rs` loads and saves, `snapshot.rs` is
the file shape, `restore.rs` the scoping, `matcher.rs` the identity rules.
