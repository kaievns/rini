//! The displays context: screens, native spaces and coordinates, as rini identifies, observes and
//! switches them. Tracks which windows sit on which space; knows nothing about workspaces or
//! layouts. See `docs/architecture.md`.

// Model
pub mod screen;
pub mod space_activation;
pub mod topology;

// Events out
pub mod event;

// Adapters
pub mod cgs_notify;
pub mod display_churn;
pub mod space_query;
pub mod space_switch;

// Actors
pub mod cursor_warp;
pub mod mission_control;
pub mod window_notify;
pub mod spaces;
