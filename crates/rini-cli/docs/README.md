# `rini-cli` — the client binary

Turns command-line arguments into `rini-ipc` commands and prints what comes back. It
depends on `rini-core` and `rini-ipc` only and cannot link the daemon, which is the
point: anything the CLI can do is in the wire protocol.

## What it owns

Argument parsing (clap), the mapping from subcommands to `LayoutCommand`, the three
parsers (`parse_direction`, `parse_window_server_id`, `parse_window_id`), and the
output formatting for queries.

## The shape that matters

**A mis-mapping is silent.** Thirty-three near-identical match arms turn subcommands
into wire commands, and nothing at runtime notices if two of them produce the same one.
The tests are therefore mostly about collisions rather than values: no two window
subcommands may map to the same wire command, the two fold directions must differ, and
so must the three resize orientations.

**Window ids are accepted in two forms.** JSON (`{"pid":123,"idx":456}`) and the debug
form (`WindowId { pid: 123, idx: 456 }`), because the second is what appears in logs and
diagnostics output, so it is what a user has to hand.

## Usage

```sh
rini-cli query diagnostics                 # whole-topology dump
rini-cli execute workspace switch 2
rini-cli execute window toggle-fold left
```
