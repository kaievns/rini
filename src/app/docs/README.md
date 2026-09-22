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
| `reactor/managers.rs` | The handles: eight manager structs plus the layout manager |
| `config/` | `config.toml`: parsing into each feature's settings, validation that spans features, watching and reload |
| `api/` | The Mach IPC backend bound to the reactor, and the shapes a query answers in |
| `hotkeys.rs` | Lowers a `WmCmd` alias to a `Command` |
| `notifications.rs` | The NSWorkspace demultiplexer |
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

## Reading order

`reactor/state.rs` → `reactor/events/` → `reactor/mod.rs`, and `main.rs` for how it is
all assembled.

## Known debt

`reactor/mod.rs` is the largest file in the tree with four functions over 250 lines, and
its tests live in one large integration module rather than beside the rules. Both are
tracked in [`docs/implementation-audit.md`](../../../docs/implementation-audit.md).
