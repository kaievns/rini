# What macOS reports while an application launches

Measured on 2026-10-01, macOS 26.7 (25G229), after Docker Desktop launched without rini taking it on
(`specs/windows.md`, "Taking on an application"). rini hears of a launching application four ways:

- a Carbon launch event;
- KVO on `NSRunningApplication.activationPolicy`;
- KVO on `NSRunningApplication.finishedLaunching`;
- the window server reporting a new window whose owner the reactor has no application for
  (`handle_window_server_appeared` in `src/app/reactor/events/space.rs`). That asks once per window:
  the first report puts the window's info in the catalogue, and a later report for a window-server id
  the catalogue knows returns before it asks about the application.

Becoming frontmost without a thread is looked at too, as a backstop (`domain/app_threads.rs`). The
first three were each checked with a probe process observing a child it started with `posix_spawn`,
which is how `com.docker.backend` starts Docker Desktop and how Launch Services does not. The
window-server report was not probed.

## Carbon reports a launch Launch Services did not make

`kEventAppLaunched` arrived 0.8 s after the spawn, at check-in, for a bundled `LSUIElement` child:
policy Accessory, `finishedLaunching` false. `NSWorkspaceDidLaunchApplicationNotification` never
fired for it.

## Both properties notify across processes

- **`activationPolicy`** changed in the child by `setActivationPolicy:` and by
  `TransformProcessType` each reached the observer within 1 ms, and the value read inside the callback
  was already the new one.
- **`finishedLaunching`** changes when the child enters `-[NSApplication run]`. `finishLaunching`
  called on its own changed nothing, and the process manager logged no `SignalReady` for it.

So for Docker Desktop the Carbon event and both KVO reports would have reached rini. Its first window
was drawn at 10:31:34.208, while it was Accessory and before it turned Regular again at 10:31:34.563,
so that window's one report, if it came, found it not ready. Where the chain broke is inferred, not
seen, because the lines that would show it are debug-level. Two explanations fit and nothing logged
tells them apart: the thread started after the last report gave up on a failed Accessibility call, as
it had no retry, or one KVO report never arrived.

## Reading a launch back out of the unified log

The info-level rini log does not show a launch. macOS's own log does, for days:

```
/usr/bin/log show --style compact --start "<from>" --end "<to>" --predicate \
  'process == "runningboardd" AND eventMessage CONTAINS "Acquiring assertion" AND eventMessage CONTAINS ":<pid>"'
```

`foregroundApp:<pid>` is the application turning Regular, `uielement:<pid>` turning Accessory,
`frontmost:<pid>` becoming frontmost, `AppDrawing` its first window. The process's own
`com.apple.processmanager` lines give `CHECKEDIN ... foreground=`, `launchedByLS=` and `SignalReady`,
which is when it finished launching. `/usr/bin/log`, not `log`: zsh has a builtin of that name.
