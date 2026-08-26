# Multi-display focus

## macOS picks the window on cmd-tab, and it picks the wrong one

cmd-tab activates an APP. macOS then chooses which of that app's windows to
focus, and its choice is not rini's. Measured with Ghostty holding one window on
each display, the built-in one parked for a workspace that was not showing:

```
3:53:42.63  MainWindowChanged -> 9607    external window, where the user was
3:53:44.93  cmd-tab to Slack             built-in switches to Slack's workspace,
                                         which parks Ghostty 11333 off screen
3:53:47.04  MainWindowChanged -> 11333   cmd-tab back: macOS names the PARKED window
3:53:47.06  WindowServerFocusChanged(11333, SpaceId(1))
3:53:47.06  Auto-switching to workspace 1 for activated app (pid: 954)
```

The last line is rini following its own rule: focus landed on a window parked off
screen, so switch that display's workspace to reveal it. The rule exists for
cmd-`, where the user cycles into another workspace deliberately and the switch is
the only thing that makes the keystroke visible.

Applied to cmd-tab it is wrong three times over. It moves a display the user did
not ask to move, it animates that move, and it lands them in a window they were
not in. The window they were in was on the other display, visible, and needed
nothing.

So rini keeps its own record of which window of each app it last saw focused, and
on an activation it prefers that over macOS's pick. The redirect is deliberately
one-sided: it applies only when the pick is parked and the remembered window is
visible, so it can never CAUSE a workspace switch, only avoid one.

### Telling an activation apart from window cycling

cmd-` must keep working as it does, which means the rule has to fire on app
activation only. No timer is involved:

- `ApplicationGloballyActivated(pid)` snapshots what the app had focused before,
  but only on a real activation edge. A duplicate arrives while the app is already
  frontmost, and re-snapshotting there would capture the window the activation
  just focused.
- The next `WindowServerFocusChanged` for that pid consumes the snapshot.
- cmd-` is rini's own `CycleAppWindows` command, and it raises the window itself.
  The app is already frontmost, so there is no activation edge and no snapshot.
- A raise rini asked for arrives as a quiet activation, which drops the pending
  snapshot. Redirecting behind rini's own raise would undo it.
- `RaiseEcho` sits earlier in the same handler and returns a raise's own focus
  reports before the redirect is considered at all, so only the window the raise
  meant to focus reaches this. See "The offset is honest, and it still moved eight
  times per press" in `docs/capture-overlay-research.md`.

The pure decision is `activation_focus_target` in
`src/actor/reactor/main_window.rs`, so the four cases are tested without replaying
an activation sequence.

## Related

`docs/capture-overlay-research.md`, "There is one overlay, so it follows the space
being animated". The same cmd-tab sequence also animated the wrong display, for an
unrelated reason.
