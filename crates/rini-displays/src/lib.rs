//! The displays context: screens, native spaces and coordinates, as rini identifies, observes and
//! switches them. Tracks which windows sit on which space; knows nothing about workspaces or
//! layouts. See `docs/architecture.md`.

// Model
pub mod ids;
pub mod screen;
pub mod space_activation;
pub mod topology;

// Events out
pub mod event;

// Adapters
pub mod display_churn;
pub mod space_query;
pub mod space_switch;

// Actors
pub mod cursor_warp;
pub mod spaces;
