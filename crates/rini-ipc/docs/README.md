# `rini-ipc` — the wire language and its transport

What rini and its clients say to each other, and the Mach port they say it over. Shared
by the daemon and `rini-cli`, which is what keeps the language honest: the CLI cannot
link the daemon, so anything it needs has to be in the protocol.

## What it owns

| | |
|---|---|
| `protocol/commands.rs` | `LayoutCommand`, `Command`: every action rini can be asked to take, and `ConfigCommand`: reload the running config, or apply a KDL fragment over it |
| `protocol/queries.rs` | The shapes a query answers in: `ContainerTreeNode`, the diagnostics census, window and workspace views |
| `protocol/layout.rs` | `Direction`, `ResizeOrientation`, `LayoutKind` |
| `mach.rs` | The Mach service: registering `git.kaievns.rini`, finding a request's payload in the message the kernel delivered (`payload_of`), and answering it on the reply port it came with |
| `client.rs` | The client half, used by `rini-cli` and anything else that asks |

## The shape that matters

**The command enum is not the config vocabulary.** A binding names an
`input::domain::binding::Action`, in niri's words, and the application lowers it to a
command here. The wire enum can be renamed or reshaped without breaking anyone's config;
what has to change with it is the lowering, `src/app/controller/lower.rs`, whose table test
names every action's command.

**A command's name is the wire's.** `LayoutCommand` and `ReactorCommand` give their variant's name
as a `&'static str` (strum's `IntoStaticStr`, in serde's snake_case), which the transitions journal
names a press by. `strum_names_are_the_wire_names` holds every variant's to what serde reads.

**`MachServices` is deliberately not enabled** in the launch agent. Letting launchd own
the port would need `bootstrap_check_in` instead of `bootstrap_register`, and nothing
needs it. See [`docs/permissions-and-the-launch-agent.md`](../../../docs/permissions-and-the-launch-agent.md).

**A request is inline, or out of line in a complex message's first descriptor.** `rini-cli` sends
it inline. `payload_of` reads either from the message's bytes, at the offsets of the descriptor as
`<mach/message.h>` packs it for a 64-bit task, and `the_kernel_s_out_of_line_and_inline_messages_parse_to_the_bytes_sent`
holds those offsets to what the kernel delivers. A first descriptor of another type, or a message
shorter than its header and descriptor need, is refused and gets no reply; a zero-size one is an
empty request, which gets none either.

**Nothing is pushed.** A request is answered on the reply port it came with, and rini sends
nothing unasked: a client that follows rini polls a query.
