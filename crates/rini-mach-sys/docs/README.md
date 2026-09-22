# `rini-mach-sys` — raw Mach messaging

`extern "C"` declarations for the Mach calls `rini-ipc` needs, and nothing else. No
safety, no abstraction: a `-sys` crate. The safe wrapper is `rini-ipc::mach`.

## What it owns

Port allocation and rights (`mach_port_allocate`, `mach_port_insert_right`,
`mach_port_mod_refs`, `mach_port_deallocate`), message send and receive (`mach_msg`,
`mach_msg_destroy`), the MIG special reply port, and the bootstrap calls
(`bootstrap_look_up`, `bootstrap_check_in`, `bootstrap_register`).

## The shape that matters

**These are declarations, not decisions.** Anything that chooses between them belongs in
`rini-ipc`. The duplicate-detection pass in
[`docs/implementation-audit.md`](../../../docs/implementation-audit.md) flags the
declaration block as near-identical functions; that is the parser mis-framing an
`extern` block, not duplication.

**`bootstrap_register` is what rini uses**, not `bootstrap_check_in`, because launchd
does not own the port. See
[`docs/permissions-and-the-launch-agent.md`](../../../docs/permissions-and-the-launch-agent.md).
