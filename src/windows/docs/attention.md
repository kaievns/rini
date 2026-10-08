# Attention: which applications ask for it

An application asks for attention with `-[NSApplication requestUserAttention:]`: its Dock icon
bounces until it is activated or cancels the ask. macOS asks per application, never per window, so
rini draws every window of an asking application urgent but the one holding focus
(`specs/decorations.md`, "Urgent").

## What is observable (P9, 2026-10-06)

A windowless helper with a Dock icon bounced while a second process watched, Kai idle 333s
(`docs/data/2026-10-08-attention/p9-bounce.jsonl`, the `observed` and `end` lines of the run):

| source | during the bounce | after the cancel |
|---|---|---|
| `lsappinfo info -long`, `LSWantsAttention` | absent → `true` | `false` |
| `lsappinfo`, `ChangeCount` | 528 → 531 | 534 |
| the Dock item's `AXFrame` | moved (the bounce) | |
| Dock windows | one appeared | |
| SkyLight, `NSWorkspace` and distributed notifications | none | none |

## How rini reads it (P14's read half, 2026-10-08)

The attention reader (`platform/attention.rs`) asks LaunchServices, as `lsappinfo` does: once a
second, `_LSCopyApplicationArrayInFrontToBackOrder` for every running application's ASN, then
`_LSCopyApplicationInformation` for each one's `pid` and `LSWantsAttention`. Both are private and
resolved by `dlsym`, so rini still launches, and reads no attention, where either is missing.

- **The call reads what `lsappinfo` reads.** Over the 12 regular applications running, absent
  where `lsappinfo` printed `[ NULL ]` and `false` (a `CFBoolean`) for the two that had bounced
  earlier, through both the one-key and the two-key call
  (`docs/data/2026-10-08-attention/read.swift`, `read.txt`). `true` was not read: that needs a
  bounce, which runs only while Kai is away.
- **A sweep costs 3.2ms**, p50 over 41 sweeps 250ms apart, p90 4.0ms, max 4.5ms, for 12
  applications, on a thread without AppKit (`cost.swift`, `cost.txt` beside it). Once a second
  that is 0.3% of one core. Sweeps run back to back cost 0.4-0.8ms (`read.txt`); that
  LaunchServices caches across close calls is inferred.
- **The array call's second argument changed nothing**: 0, 1, 2, 3, 4, 8, 2^31 and 2^32-1 each
  listed the same 12 applications (`read.txt`).
- **No notification is read.** `lsappinfo`'s `ChangeCount` moves with the ask, and LaunchServices
  exports `_LSScheduleNotificationOnQueueWithBlock` and `_LSModifyNotification`, but whether a
  notification fires on the ask, and with which code, needs a bounce. Until that runs the reader
  polls, which is what P14 said to do if no notification is found.

## What follows from it

- An ask shows up to a second late, the poll's period.
- Activating the application ends its ask, so rini does not wait for the poll: no window of the
  application holding focus is drawn urgent (`decorations::domain::style::Rings::urgent`).
