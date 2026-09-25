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

- A window rini has moved off screen to hide it MUST stay reachable across a restart. A parked window
  rini drops is stranded: it is off screen, no strip holds it, no workspace brings it back, and the
  switcher cannot offer it because it has no workspace assignment.
- Startup MUST NOT silently discard a persisted window it cannot match. The saved layout is the only
  record of where a parked window was, so discarding the record while the window is still off screen
  makes the window unrecoverable by any means rini offers.

> **Reported 2026-09-25.** "When I switch to the 1password window it doesn't go up, I can't see it
> behind the kirocrew window... the popup doesn't show either." Diagnosed, NOT fixed.
>
> What was measured, 60 seconds after a `service restart` with 26 windows alive on the display:
>
> | Source | Windows |
> |---|---|
> | `CGWindowListCopyWindowInfo` | 26 |
> | `~/.rini/layout.ron` | 202 entries |
> | `rini-cli query windows` | 2 |
>
> The two rini held were the only two NOT parked. Every window at the off-screen park corner —
> (1727, 1085) on this display — was absent from the model, including the 1Password and Kiro Crew
> windows named in the report. rini saw both APPLICATIONS, so this is window registration and not
> Accessibility being refused.
>
> The mechanism is named by rini's own log: `workspaces::engine`, "Ignored unmatched persisted windows
> after application discovery". Startup matches persisted windows against what it discovers and drops
> what does not match, and a window parked off the display is exactly what fails to be discovered. So
> the count of 10 seen earlier in the same session was an accumulation over a long uptime, not the
> result of startup adoption.
>
> This makes the switcher look broken after every restart for a reason that is not in the switcher: it
> can only offer what rini knows, and after a restart rini knows the visible workspace.
