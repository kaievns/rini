# What macOS reports while an application launches

Measured on 2026-10-01, macOS 26.7 (25G229), after Docker Desktop launched without rini taking it on
(`specs/windows.md`, "Taking on an application"). rini hears of a launch through three reports — a
Carbon launch event, then KVO on `NSRunningApplication.activationPolicy` and `finishedLaunching` — so
each was checked with a probe process observing a child it started with `posix_spawn`, which is how
`com.docker.backend` starts Docker Desktop and how Launch Services does not.

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

So for Docker Desktop all three reports would have reached rini, and the fault is after the last one.
The thread it started had no retry on a failed Accessibility call; that it failed one is inferred, not
seen, because that line is debug-level.

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
