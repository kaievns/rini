# `app` — the application layer

What turns the features into rini. Nothing in a feature imports this; the application
knows the features and converts what they emit.

## What it owns

| | |
|---|---|
| `reactor/` | The orchestrator. Holds every feature's store, reduces the events they emit, drives them back |
| `reactor/state.rs` | `RiniState` |
| `reactor/events/` | The reducers, one module per event family |
| `reactor/events/app.rs` | Applications launching, terminating, losing their thread, activating, and the windows each one reports |
| `reactor/events/command.rs` | The commands: layout commands, closing a window, a config update, saving before exit, and focusing or moving the mouse to a display |
| `reactor/events/drag.rs` | A mouse-up ending a drag: the swap it deferred carried out, and a window dragged onto another display's space moved to that display's strip in its own workspace |
| `reactor/events/focus.rs` | `resolve`: the space and window a workflow's focus request lands on |
| `reactor/events/outcome.rs` | `EventOutcome`: what a reducer leaves for after its mutation, in order (window-server updates, discoveries, app requests, raises and refreshes), and `absorb`, which merges two |
| `reactor/events/space.rs` | The window server's windows appearing and going, native Mission Control entered and left, and space lifecycle, with the fullscreen departure and last-known user space rules |
| `reactor/events/system.rs` | Menus opening and closing, waking from sleep, a raise completing or timing out, and the WM controller's sender registering |
| `reactor/events/window.rs` | One window's events: created, destroyed, minimized and back, its frame changing (`classify_window_frame_change`), its title, and the mouse moving over it |
| `reactor/events/window_discovery.rs` | An application's window list: which known windows are stale and leave, which are new, and the layout events they make |
| `reactor/commands.rs` | What a command MEANS before it is carried out: which window "index 3" is, which display "next" is |
| `reactor/observations.rs` | The other half of the inbox: what the system reported, gathered and handed over settled |
| `reactor/space_affinity.rs` | Which space a window is on — the gathering |
| `reactor/space_resolution.rs` | Which space a window is on — the rules, pure |
| `reactor/query.rs` | Answers from a borrowed `StateView` |
| `reactor/present.rs` | The frame-writing capability: record a destination, number the write; and `instant_requests`, what a pass that does not fly sends one application |
| `reactor/bar.rs` | The bar's half here: its model built from the stores after every batch and sent only when it changed, and what a click on it asks for |
| `reactor/managers.rs` | The handles: ten manager structs plus the layout manager |
| `reactor/animation.rs` | `AnimationManager`: whether each layout pass flies on the overlay, and placing the real windows when it does not, from `transitions::domain::pass`'s plan |
| `reactor/decorations.rs` | The decorations' half here: what each managed window wears as its window rules have it, who wears the ring, each tiled window's border, and what the rest windows are told after a batch |
| `reactor/diagnostics.rs` | What a query answers about when it was not told (`default_query_space`), and what the diagnostics dump counts as wrong (`orphaned_windows`, `stale_display_homes`) |
| `reactor/testing.rs` | Built only for tests: the reactors, screens and space-state events the reactor's tests start from |
| `reactor/tests/` | The integration tests, one file per subject, over `reactor/tests/fixtures.rs` |
| `config/` | `config.kdl`: `config/kdl/` decodes each section into a part type and `kdl/apply.rs` lays the parts over each feature's settings, with the checks that span sections; then watching and reload. The format's decisions are [`docs/config.md`](../../../docs/config.md) |
| `api/` | The Mach IPC backend bound to the reactor, and the shapes a query answers in |
| `hotkeys/` | The controller: app launches and which apps get a thread (the rule is `windows::domain::app_threads`), hotkey registration, config reload. `hotkeys/lower.rs` is the translation from a bound `Action` to a `reactor::Command`, which is where the tests are |
| `notifications.rs` | The NSWorkspace demultiplexer |
| `power.rs` | Low Power Mode, read at start and kept current from its notification: the input tap and the reactor's flight decision read it |
| `boot.rs` | What the flags and the config file say before anything is built: the command line, `--no-animate` over the config, and the config-or-defaults fallback |
| `launch_agent.rs`, `startup.rs`, `logging.rs`, `channels.rs` | The service plist, configured startup commands, tracing, and the span-carrying channel every actor is wired with |

## The shape that matters

**Features talk upward by emitting events.** `windows::event::Event` becomes
`reactor::Event` through a `From` impl. A feature never names `crate::app`, and
[`tests/architecture.rs`](../../../tests/architecture.rs) enforces it.

**The reactor is one actor on one thread.** Everything it holds is single-threaded
state; the actors it talks to have their own run loops and their own threads.

**A broken config must not stop rini starting.** One mistyped binding used to take the
window manager down. It now reports and falls back to the defaults, which is the only
state a user can recover from. A missing `config.kdl` with a `config.toml` beside it is
reported the same way, since the defaults are not what that file configured. A broken
reload keeps the running config and says why, in the log and with a mark on every bar.

**Decisions leave, orchestration stays.** Pure feature logic has been moved out to each
feature's `domain/` over several passes. What remains in `reactor/mod.rs` reads and
writes several stores per event, which is what the application is for.

**A bound action is a translation, not a side effect.** `hotkeys/lower.rs` turns an
`Action` into a `reactor::Command`, a program to spawn, a config reload, or a named
refusal. It was ninety match arms inside `handle_event`, each ending in a `send`, so
nothing could check that two actions did not lower to the same command; now a table test
names every action's target.

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
| `bar.rs` | what each display's bar is sent after a batch, and what a click on it does |
| `decorations.rs` | what the rest windows are sent after a batch: each window's ring and border, whether it holds focus or asks for attention, and the rules that dress it |
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

`reactor/mod.rs` is the largest file in `src/app/`. It and its functions past the size
limits are held at their sizes in `LONG_FILES` and `LONG_FNS` in `tests/architecture.rs`;
what each of them is, is in [`docs/implementation-audit.md`](../../../docs/implementation-audit.md).
