# `rini-geometry` — rect arithmetic

Extension traits over `CGRect`, `CGPoint` and `CGSize`, plus the handful of predicates
every layout decision needs. Pure, and the only crate that tests without linking AppKit:
`cargo test -p rini-geometry` builds plain Rust.

## What it owns

`CGRectExt` (`mid`, `intersection`, `area`, `contains`, `contains_rect`), `Round`,
`SameAs` and `IsWithin` for float comparison, `centered_in`, `is_off_screen` and
`park_entry_frame`.

## The shape that matters

**`is_off_screen` has a threshold, and it is measured.** 40pt of overlap. Applications
clamp a park past it — Kiro reports 41pt, Finder 52pt — which is why a park is judged
from both the requested frame and the server's. See "Parking" in
[`src/layout/docs/strip.md`](../../../src/layout/docs/strip.md).

**Float comparison is explicit.** `SameAs` and `IsWithin` exist so no layout test
compares `f64` with `==` and no production code rounds by accident.
