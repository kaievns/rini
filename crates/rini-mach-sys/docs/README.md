# `rini-mach-sys` — raw Mach messaging

`extern "C"` declarations for the Mach calls `rini-ipc` and the SkyLight server-port query
(`src/windows/platform/sub_level.rs`) need, and nothing else. No abstraction: a `-sys` crate. A
call with no precondition is declared `safe` (`mach_task_self`); every other is `unsafe`, and the
crate doc's `# Safety` says what its caller owes. The safe wrapper is `rini-ipc::mach`.

## What it owns

Port allocation and rights (`mach_port_allocate`, `mach_port_insert_right`,
`mach_port_mod_refs`, `mach_port_deallocate`), message send and receive (`mach_msg`,
`mach_msg_destroy`), the MIG special reply port, and the bootstrap calls
(`bootstrap_look_up`, `bootstrap_register`). One thin helper sits beside them:
`mach_get_bs_port` finds a registered service, for the server's startup check for a second
instance (`rini-ipc/src/mach.rs`) and the client's lookup (`rini-ipc/src/client.rs`). The client
imports its constants, message layouts and externs from here. A complex message's descriptors are
not declared: the server reads them from the message's bytes (`payload_of` in `rini-ipc`).

## The shape that matters

**These are declarations, not decisions.** Anything that chooses between them belongs in
`rini-ipc`. The duplicate-detection pass in
[`docs/implementation-audit.md`](../../../docs/implementation-audit.md) flags the
declaration block as near-identical functions; that is the parser mis-framing an
`extern` block, not duplication.

**`bootstrap_register` is what rini uses**, not `bootstrap_check_in`, because launchd
does not own the port. See
[`docs/permissions-and-the-launch-agent.md`](../../../docs/permissions-and-the-launch-agent.md).
