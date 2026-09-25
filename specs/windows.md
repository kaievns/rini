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

## Floating windows and the park corner

- A floating window MUST NOT keep a parked frame as its own position. A floating window has no column
  to be rebuilt from, so its remembered frame is the only record of where it belongs — and that frame is
  written back every time its workspace is arranged. Reading a park as a real position once strands the
  window there for good.
- Whether a FLOATING frame is usable MUST be judged on EITHER axis. The two tests that answer "is this a
  parked tiled column" both require a sliver in both axes, because a column peeking in at the edge of the
  strip shows its full height and is legitimately on screen. A park shows nearly its full height too, so
  both accept it.
- A floating window whose remembered frame is unusable MUST be re-centred on its display, not left where
  it is. There is nothing else to fall back to.

> **Reported 2026-09-25.** "I can't switch to the 1pass window at all... I can see that I'm focused on
> 1pass in the menu bar, but I'm seeing something else on the strip." Fixed.
>
> Measured: rini had activated the right workspace and had 1Password focused, and its frame in both
> rini's model and the window server was (1727, 1089) at 900x1079 on a 1728x1085 display — 1pt of width
> and 28pt of height on screen. Two other floating windows were stranded the same way, System Settings
> at 52pt of height and zoom.us at 44pt.
>
> `ensure_visible_floating` already re-centres a floating window whose frame is hidden. It was asking
> `is_hidden`, a 3pt both-axes test, which called these frames real positions. `is_off_screen`, the 40pt
> both-axes test, calls two of the three real as well. Neither is wrong for tiled columns; both are wrong
> for a float, which is why the floating rule is one-axis and its own.

## rini cannot see the keyboard while another app holds secure input

- When a keyboard event tap receives nothing, rini MUST NOT be assumed to be at fault. macOS bypasses
  every keyboard event tap while any process holds secure input, and rini cannot override it.
- Every keyboard-driven feature MUST have a path that does not go through the tap. The CLI is that path.

> **Reported 2026-09-25.** "When I try to use the app switcher it doesn't show either, in fact it doesn't
> appear or work at all... I can cmd-tab back to this window from 1pass, but ctrl-q does nothing." NOT a
> rini defect.
>
> `ioreg -l -w 0 | grep kCGSSessionSecureInputPID` returned 2109, which was 1Password 7, and kept
> returning it after another application was activated. 1Password 7 takes secure input and does not give
> it back. While it is held, no keyboard event tap in the session receives anything, so every rini
> binding is dead — verified by synthesising ctrl-J and ctrl-Q and seeing nothing reach the reactor,
> while the same synthesised ctrl-Q opened the popup normally before 1Password was activated.
>
> The report's own detail confirms it: cmd-tab still worked. The Dock's switcher is a WindowServer
> symbolic hotkey rather than a tap client, so it is the one keyboard path secure input does not cut.
> Releasing it needs 1Password 7 to quit.

## Windows rini has parked

- A window rini has moved off screen to hide it MUST stay reachable. See the floating rules above for
  the case that was actually reported.

> **Corrected 2026-09-25.** An earlier entry here claimed rini discards persisted windows at startup and
> strands them, citing `rini-cli query windows` returning 2 against 202 entries in `layout.ron`. That
> reading was wrong: `query windows` is scoped to the ACTIVE workspace. Querying every workspace showed
> rini holding all 22 windows, including the two named in the report. The stranding was real and its
> cause was the floating-frame rule above.
