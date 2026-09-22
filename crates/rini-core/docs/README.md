# `rini-core` — identity and file locations

The smallest crate and the one everything depends on. A leaf: it has no behaviour, which
is what keeps it from becoming a hub. If you are tempted to put a decision here, it
belongs in a feature.

## What it owns

| | |
|---|---|
| `ids.rs` | `WindowId { pid, idx }` — rini's own window identity. `WindowServerId` and `SpaceId` are macOS's, declared in `rini-skylight-sys` and re-exported from here so nothing else needs that dependency for a number. `ScreenId` is a `CGDirectDisplayID`. `BTreeExt` |
| `paths.rs` | `data_dir()`, `config_file()`, `restore_file()` — the three places rini reads and writes |

## The shape that matters

**Three identities, three lifetimes.** `WindowId` dies with the process.
`WindowServerId` survives it and is recycled, so the same number can mean a different
window later. `SpaceId` is minted fresh on every display reconnect. Nothing durable may
be keyed by the last two; see
[`src/workspaces/docs/workspaces-and-displays.md`](../../../src/workspaces/docs/workspaces-and-displays.md).

**`rini-ipc` has wire twins.** The types here are internal; the protocol has its own
with `From` impls, so the wire format can change without the core moving.
