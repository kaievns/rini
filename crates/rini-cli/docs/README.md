# `rini-cli` — the client binary

Turns command-line arguments into `rini-ipc` commands and prints what comes back. It
depends on `rini-core` and `rini-ipc` only and cannot link the daemon, which is the
point: anything the CLI can do is in the wire protocol.

## What it owns

Argument parsing (clap), the mapping from subcommands to the wire's `RiniRequest`, the three
parsers (`parse_direction`, `parse_window_server_id`, `parse_window_id`), and the
output formatting for queries.

## The shape that matters

**A mis-mapping is silent.** Near-identical match arms turn subcommands into wire
commands, and nothing at runtime notices if two of them produce the same one.
The tests are therefore mostly about collisions rather than values: no two window
subcommands may map to the same wire command, the two fold directions must differ, and
so must the three resize orientations.

**Window ids are accepted in two forms.** JSON (`{"pid":123,"idx":456}`) and the debug
form (`WindowId { pid: 123, idx: 456 }`), because the second is what appears in logs and
diagnostics output, so it is what a user has to hand.

**Workspace numbers count from 0.** `workspace switch` and `workspace move-window` pass the
index to the wire as given, while the config counts from 1, so `workspace switch 0` is the
config's `focus-workspace 1`.

## Usage

```sh
rini-cli query diagnostics                 # whole-topology dump
rini-cli execute workspace switch 1        # the second workspace: the CLI counts from 0
rini-cli execute window toggle-fold left
```
