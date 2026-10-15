# Which windows rini manages

## Taking on an application

An application is taken on when it gets its Accessibility thread. No window of an application without
one reaches the strip.

- An application MUST be taken on once it is Regular and has finished launching, in whichever order
  it got there, however often its activation policy changed on the way, and whether or not Launch
  Services launched it.
- One thread per application. A launch reported twice MUST NOT start a second thread. An
  Accessibility call blocks on the owning application, so a hung application MUST NOT hang rini.
- An application slow to answer Accessibility while it launches MUST be asked again, not given up on
  at the first failure.
- An application that becomes frontmost without a thread MUST be looked at again then. Its launch, its
  policy change, its finished launch and each of its windows are reported once, and one report that
  fails or goes missing MUST NOT leave the application unmanaged for as long as it runs.
- A thread that gives up MUST say so in the info-level log.
- An application's thread MUST end when the application quits, whether rini hears of the quit or finds
  it at a request that fails because the application has gone, and everything the thread holds MUST go
  with it: its observer, its Accessibility elements and its record of the application's windows. A raise
  queued or under way for that application MUST NOT keep the thread.

> **Reported 2026-10-01.** "also the docker window is not in the strip for some reason". Fixed.
>
> Measured: Docker Desktop (pid 21114, `com.electron.dockerdesktop`) is started by
> `com.docker.backend`, not by Launch Services (`launchedByLS=0`). The unified log has its activation
> policy changing twice in its first two seconds: Regular at check-in (10:31:32.657), Accessory at
> 10:31:32.972, finished launching at 10:31:33.180, first window drawn at 10:31:34.208, Regular again at
> 10:31:34.563. It became frontmost at 10:31:34.063, before it was Regular again, and then at
> 10:32:19, 10:47:13, 11:58:42 and 11:59:11. At 12:00 rini had
> no thread for it: `rini-cli query applications` listed 14 applications without it, a sample of rini
> showed no `com.electron.dockerdesktop(21114)` thread, and Accessibility answered for its window in
> 41 ms. The info-level log never named the pid.
>
> The Carbon launch and both property reports do fire for an application launched this way; the probes
> are in `src/windows/docs/launch-notifications.md`. Its first window was drawn while it was Accessory,
> so that window's one report, if it came, found it not ready. What no step had was a second chance.
> The thread started after the last report gave up on its first failed Accessibility subscription —
> every application but JetBrains' and Emacs got no retry — and logged that at debug level only. That it
> failed one is inferred, not seen, since that line is debug-level; a KVO report that never arrived
> fits the same logs. Kiro Crew, also Electron, failed an Accessibility read with "could not complete"
> after 1 s at 08:01:55 the same day, as it drew its first window.

> **Reported 2026-10-06.** "fix that runaway thread and stale cache cleaning". Fixed.
>
> Measured: rini (pid 96921), up 16h, still ran threads for two applications that had quit, pids 3096
> (zoom) and 9726. Read from the code: the thread's task ran its requests and its raises side by side,
> and only the requests ended at a quit. The raises waited on a channel whose sender the thread itself
> held, so they never ended, and the thread runs until its task does. A raise waiting for a gone
> application to activate also asked it every 10ms whether it was frontmost, until the raise was
> cancelled.

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

What admission decides about a window MUST hold at every refresh of the application's windows, not
only when the window is first seen: a window the title-element rule makes non-standard stays
non-standard, and one the window server gives a parent stays off the root. A refresh reports each
window once (`a_refresh_reports_a_known_window_as_registration_decided`,
`a_refresh_reports_a_window_it_takes_on_once`).

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
- A record of a window MUST NOT be forgotten while it still holds anything: its Accessibility state,
  a window-server id or a rule decision. The record of a window-server id MUST NOT be forgotten while
  it holds anything: the window it belongs to, a visibility, an observation, a space, what the server
  reported of it, a recent sighting or a native-fullscreen record still waiting on a window.
- A window that leaves MUST take its pictures with it, once, whichever way it leaves: closed, closed
  while minimised or unmanaged, found gone when its application's windows are looked at again, left
  behind by its application quitting, or found again under a new identity for the same window-server
  window. An application that quits MUST take its windows' records too, native-fullscreen ones
  included, whether bound to a window or still waiting on one, and whether or not rini ever watched
  it: macOS hands the window-server ids they are keyed by to other windows. A window whose
  Accessibility destruction is reported during a refresh quarantine MUST keep its pictures: its
  element can be replaced while the window lives.

> **Found 2026-09-23, not reported.** The prune check tested four of a record's nine fields. A record
> holding only one of the other five was pruned and what it held forgotten.

> **Reported 2026-10-06.** "fix that runaway thread and stale cache cleaning". Fixed.
>
> Read from the code: only two ways a window left forgot its pictures, an Accessibility destruction of
> a window still held and the window server's report of a close, and the second skipped a minimised or
> unmanaged window. An application quitting, a window found gone when its application's windows were
> looked at again, and a new identity for the same window dropped the window and kept its pictures
> until rini restarted; a quitting application also kept its windows' records. The log of the running
> build showed "Application terminated without notification" 3 times across its runs. How often the
> other ways ran is not logged.

> **Found 2026-10-15, not reported.** That fix dropped a quitting application's windows one at a
> time, and dropping one window keeps its native-fullscreen record on purpose, so those records, and
> any still waiting on a window, outlived the application.

## Writing a frame

- A same-size move writes the position alone, and falls back to size, position, size when the size it
  reads back has changed. When the window server already has the window less than 0.5pt off the
  desired width and the desired height, the write MUST NOT send a size. A size write costs the app
  2-60ms even when the size does not change, against 0.4-3ms for a position write. The size is read
  from the window server, not the app, so the check asks nothing of the app.
- When the frame read back after that position write is 0.5pt or more off the desired size in
  either axis, size, position, size MUST follow, and the frame reported MUST be the one read after
  it. Nothing else puts the size back if the app changed it on the move.
- Any other frame write is size, then position, then size AGAIN, and so is one whose window-server
  size cannot be read. AppKit clamps a size against the window's current position, so a window near
  a screen edge cannot grow until it has moved; the first size is what lets the move succeed for a
  window that must grow and shift at once. Neither size may be dropped from such a write.
- Enhanced User Interface MUST be suppressed for the duration of a burst of writes, not per write.
  AppKit re-lays-out a window while it is on, which fights the write.

> **Found 2026-10-03, not reported.** Measured in the handover prototype's research run on
> 2026-10-03: an Accessibility size write cost the app 2-60ms even when the size did not change, a
> position write 0.4-3ms. The prototype wrote the position alone for a move that kept the size.
>
> The read-back fallback was added in review, not measured. It guards a move the app resizes on the
> way, a window constrained to a smaller display for one, which a position written alone would leave
> at the wrong size.

## Where it lives

`src/windows/domain/app_threads.rs` is taking an application on, `src/windows/domain/admissible.rs`
admission, `src/windows/domain/catalogue.rs` the records, `src/windows/domain/ax_events.rs` the
liveness rules, `src/windows/domain/frame_write.rs` which writes a frame takes,
`src/windows/platform/app_actor.rs` the per-app thread, and `src/app/hotkeys/mod.rs` the controller
that starts it. An application too busy to answer a subscription while it launches is asked again
by the thread's launch loop, every 20ms doubling to 1s for up to 60s, on the schedule in
`src/windows/platform/ax/ask_again.rs`; `an_application_too_busy_launching_to_answer_is_asked_again`
and `an_application_that_never_answers_is_given_up_on_once_patience_runs_out` in `app_actor.rs` hold
it. A window whose own subscription it is too busy to answer is left out and taken on at the next
AXWindows refresh, held by `a_window_too_busy_to_watch_is_taken_on_at_the_next_refresh`. The
Accessibility observer itself asks nothing again. A window leaving rini's records goes through
`src/app/reactor/events/window.rs`, which forgets its pictures, except at a new identity, whose old
one `src/app/reactor/events/window_discovery.rs` forgets.

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
