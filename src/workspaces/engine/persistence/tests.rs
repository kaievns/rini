//! The persistence tests, by subject.
//!
//! They were one 3,302-line module. Every one of them builds a real `LayoutEngine`, saves it, and
//! loads it back, which is what makes them worth having and what makes them long; a failing test now
//! names the subject rather than just the file.

use super::*;

fn test_engine() -> LayoutEngine {
    LayoutEngine::new(&VirtualWorkspaceSettings::default(), &LayoutSettings::default())
}

mod announcements;
mod launch_memory;
mod matching;
mod restore_scope;
mod save_load;
mod schema;
mod startup;
