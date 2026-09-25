# Which windows rini manages

## Admission

A window Accessibility reports is turned away before anything else knows it exists when:

- it has no visible peer in the window server and is not minimized — an id the server does not report
  back means the window is not on screen;
- its Accessibility role is not a window role;
- an application rule says to ignore it;
- for the applications that need it, its title element cannot be read. This is a per-application
  allowlist and a known heuristic, not a general rule.

Accessibility reports a window BEFORE the window server has it, so "no server id yet" MUST be treated
as visible rather than absent. The reverse — an id the server does not report — means not on screen.

## Identity

- A window has two identities: rini's own, which dies with the process, and the window server's, which
  survives and is RECYCLED.
- Tracking a recycled server id for a new window MUST detach it from the window that had it. Leaving
  both pointing at it makes one window answer for two.
- Giving a window a new server id MUST release the one it had.

## Liveness

- A failed Accessibility request means the window is GONE only when the element itself is invalid.
- "Could not complete" MUST NOT retire a window. An application slow to answer is the normal case
  during launch and under load, and retiring on it deletes live windows.
- A window absent from `kAXWindowsAttribute` MUST NOT be retired: that attribute is space-filtered and
  cannot say whether a window still exists globally.
- A record of a window MUST NOT be forgotten while it still holds anything — a pending frame write, an
  observed space, a rule decision.

> **Found 2026-09-23, not reported.** The prune check tested four of a record's nine fields. A record
> holding only a pending frame write was pruned and the write forgotten.

## Writing a frame

- A frame write is size, then position, then size AGAIN. AppKit clamps a size against the window's
  current position, so a window near a screen edge cannot grow until it has moved; the first size is
  what lets the move succeed for a window that must grow and shift at once. Neither size may be
  dropped.
- Enhanced User Interface MUST be suppressed for the duration of a burst of writes, not per write.
  AppKit re-lays-out a window while it is on, which fights the write.
- One thread per application. An Accessibility call blocks on the owning application, so a hung
  application MUST NOT hang rini.

## Where it lives

`src/windows/domain/admissible.rs` is admission, `src/windows/domain/catalogue.rs` the records,
`src/windows/domain/ax_events.rs` the liveness rules, `src/windows/platform/app_actor.rs` the per-app
thread.

## Windows rini has parked

- A window rini has moved off screen to hide it MUST stay in rini's model for as long as it exists. A
  parked window that rini forgets is stranded: it is off screen, no strip holds it, no workspace can
  bring it back, and the switcher cannot offer it because it has no workspace assignment.

> **Reported 2026-09-25.** "When I switch to the 1password window it doesn't go up, I can't see it
> behind the kirocrew window." Diagnosed, NOT fixed. The observation: `rini-cli query windows` listed 10
> windows, and the 1Password and Kiro Crew windows were not among them — while `CGWindowListCopyWindowInfo`
> showed both alive at (1727, 1085) and (1727, 1089), which is rini's own off-screen park corner. So both
> had been parked by rini and then dropped from its model, leaving them permanently invisible and
> unreachable. rini did see both APPLICATIONS, so this is window registration rather than Accessibility
> being refused.
>
> The suspected path is `src/app/reactor/events/space.rs`, where a window-server disappearance with
> `ordered_in == Some(false)` is promoted to an immediate `WindowDestroyed`. A window parked off the
> display is a plausible trigger for exactly that report. Not confirmed: the running build logs at WARN
> and the decision is at DEBUG, so reproducing it needs the log level raised first.
