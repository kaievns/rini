//! The overview: every workspace of one display at once, zoomed out, while its key is held.
//!
//! Pure: the scene a display's workspaces are drawn as, over `rini-core`'s ids and rini-geometry's
//! spaces, and what changed between two of them. It names no other feature and has no `platform/`,
//! because it touches nothing outside itself. See `src/overview/docs/README.md` and
//! `specs/overview.md`.

pub mod domain;
