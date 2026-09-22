# `app` — the application layer

What turns the features into rini. Nothing in a feature imports this; the application
knows the features and converts what they emit.

## What it owns

| | |
|---|---|
| `reactor/` | The orchestrator. Holds every feature's store, reduces the events they emit, drives them back |
| `reactor/state.rs` | `RiniState` |
| `reactor/events/` | The reducers, one module per event family |
| `reactor/commands.rs` | What a command MEANS before it is carried out: which window "index 3" is, which display "next" is |
| `reactor/observations.rs` | The other half of the inbox: what the system reported, gathered and handed over settled |
| `reactor/space_affinity.rs` | Which space a window is on — the gathering |
| `reactor/space_resolution.rs` | Which space a window is on — the rules, pure |
| `reactor/query.rs` | Answers from a borrowed `StateView` |
| `reactor/present.rs` | The frame-writing capability: record a destination, number the write |
| `reactor/managers.rs` | The handles: eight manager structs plus the layout manager |
| `reactor/tests/` | The integration tests, one file per subject, over `reactor/tests/fixtures.rs` |
| `config/` | `config.toml`: parsing into each feature's settings, validation that spans features, watching and reload |
| `api/` | The Mach IPC backend bound to the reactor, and the shapes a query answers in |
| `hotkeys/` | The controller: app launches, hotkey registration, config reload. `hotkeys/lower.rs` is the translation from a binding alias to a `reactor::Command`, which is where the tests are |
| `notifications.rs` | The NSWorkspace demultiplexer |
| `boot.rs` | What the flags and the config file say before anything is built: whether to restore, and the config-or-defaults fallback |
| `launch_agent.rs`, `startup.rs`, `logging.rs`, `channels.rs` | The service plist, configured startup commands, tracing, and the span-carrying channel every actor is wired with |

## The shape that matters

**Features talk upward by emitting events.** `windows::event::Event` becomes
`reactor::Event` through a `From` impl. A feature never names `crate::app`, and
[`tests/architecture.rs`](../../../tests/architecture.rs) enforces it.

**The reactor is one actor on one thread.** Everything it holds is single-threaded
state; the actors it talks to have their own run loops and their own threads.

**A broken config must not stop rini starting.** One mistyped binding used to take the
window manager down. It now reports and falls back to the defaults, which is the only
state a user can recover from.

**Decisions leave, orchestration stays.** Pure feature logic has been moved out to each
feature's `domain/` over several passes. What remains in `reactor/mod.rs` reads and
writes several stores per event, which is what the application is for.

**A binding alias is a translation, not a side effect.** `hotkeys/lower.rs` turns a
`WmCmd` into a `reactor::Command`, an `Exec`, a config reload, or a named refusal. It
was ninety match arms inside `handle_event`, each ending in a `send`, so nothing could
check that two aliases did not lower to the same command.

## Where the reactor's tests are

`reactor/tests/` is one module per subject. A failing test names the subject, and the
file you open to change one is the size of that subject rather than of the reactor.

| file | what it covers |
|---|---|
| `displays.rs` | screens arriving and leaving, resolution changes, per-display workspaces, display homes |
| `tiling.rs` | layout passes, strips, columns, folding, resizes, drags, animation |
| `workspaces.rs` | creating and switching workspaces, moving windows, app rules, queries |
| `focus.rs` | focus moves, focus-follows-mouse, raise echoes, cmd-tab, per-app main window |
| `fullscreen.rs` | full width and height on the strip, and macOS taking a window fullscreen off it |
| `spaces.rs` | which space is current, what happens while that is unknown, space membership |
| `lifecycle.rs` | windows and apps appearing, disappearing, dying, and surviving a restart |
| `windows.rs` | which windows rini takes on and which it refuses |
| `fixtures.rs` | the reactors, apps and windows the cases are built on |

`tiling.rs` is not `layout.rs` because a module named `layout` inside `tests` shadows the
`layout` alias the siblings use for `crate::workspaces`.

A rule that can be tested without a reactor does not belong here. `space_resolution`,
`present`, `hotkeys::lower`, `windows::domain::admissible` and `input::domain::pointer`
each left this module and took their tests with them.

## Reading order

`reactor/state.rs` → `reactor/events/` → `reactor/mod.rs`, and `main.rs` for how it is
all assembled.

## Known debt

`reactor/mod.rs` is the largest file in the tree with four functions over 250 lines,
tracked in [`docs/implementation-audit.md`](../../../docs/implementation-audit.md).
