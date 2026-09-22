//! The reactor's integration tests, by subject.
//!
//! These build a whole `Reactor` and drive it with events, which is what makes them integration
//! tests and why they are here rather than beside a rule. They were one 6,924-line module; a failing
//! test now names a subject, and the file you open to change one is the size of its subject.
//!
//! A rule that can be tested without a reactor should not be here. Several have left over the last
//! few passes — `space_resolution`, `present`, `hotkeys::lower`, `admissible`, `pointer` — and each
//! took its tests with it.

mod fixtures;

mod displays;
mod focus;
mod fullscreen;
mod lifecycle;
mod spaces;
mod tiling;
mod windows;
mod workspaces;
