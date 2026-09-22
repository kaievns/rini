# `rini-skylight-sys` — raw SkyLight and CGS declarations

The private window-server API, declared. Reverse engineered by yabai and other projects;
rini uses it without disabling SIP and without a scripting addition.

## What it owns

The `SLS*` and `CGS*` function declarations, and the plain value types that come with
them: `SpaceId` (a `CGSSpaceID`), `WindowServerId` (a `CGWindowID`),
`DisplayReconfigFlags`, `CGSEventType`, `KnownCGSEvent`.

## The shape that matters

**The value types are vocabulary, the functions are API.** `SpaceId` is a number the
domain has to carry; `SLSSpaceGetType` is a call it must not make. That distinction is
what `SKYLIGHT_VALUE_TYPES` in [`tests/architecture.rs`](../../../tests/architecture.rs)
encodes, so a pure module may name the first and not the second.

`rini-core` re-exports `SpaceId` and `WindowServerId`, so nothing takes a dependency on
this crate just to hold a number.

## Measured behaviour

Levels, capture options and notification semantics are recorded in
[`src/animation/docs/capture-overlay-research.md`](../../../src/animation/docs/capture-overlay-research.md).
Private API behaviour is measured rather than documented upstream, so a claim here
without a measurement behind it should be distrusted.
