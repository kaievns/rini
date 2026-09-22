# `rini-ipc` — the wire language and its transport

What rini and its clients say to each other, and the Mach port they say it over. Shared
by the daemon and `rini-cli`, which is what keeps the language honest: the CLI cannot
link the daemon, so anything it needs has to be in the protocol.

## What it owns

| | |
|---|---|
| `protocol/commands.rs` | `LayoutCommand`, `Command`: every action rini can be asked to take. `serde(rename_all = "snake_case")` is also what turns a config binding string into a command |
| `protocol/queries.rs` | The shapes a query answers in: `ContainerTreeNode`, the diagnostics census, window and workspace views |
| `protocol/layout.rs` | `Direction`, `ResizeOrientation`, `LayoutKind` |
| `mach.rs` | The Mach service: registering `git.kaievns.rini` and carrying a request/response pair |
| `client.rs` | The client half, used by `rini-cli` and anything else that asks |
| `subscriptions.rs` | Event subscription, so a status bar can subscribe rather than poll |
| `cli_exec.rs` | Running a subscriber's command with the event in its environment |

## The shape that matters

**The command enum is the config vocabulary.** A binding string is a serde
deserialization of `LayoutCommand`, so deleting a variant deletes a config keyword, and
a config naming a removed command fails to parse — which costs every other setting in
the file. Deleting a command means updating `rini.default.toml` in the same change.

**`MachServices` is deliberately not enabled** in the launch agent. Letting launchd own
the port would need `bootstrap_check_in` instead of `bootstrap_register`, and nothing
needs it. See [`docs/permissions-and-the-launch-agent.md`](../../../docs/permissions-and-the-launch-agent.md).

**Signals fire on startup, workspace switches and workspace window changes**, so a
status bar subscribes instead of polling.
