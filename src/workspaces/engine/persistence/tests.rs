//! The persistence tests, by subject.
//!
//! They were one 3,302-line module. Every one of them builds a real `LayoutEngine`, saves it, and
//! loads it back, which is what makes them worth having and what makes them long; a failing test now
//! names the subject rather than just the file.

use super::*;

fn test_engine() -> LayoutEngine {
    LayoutEngine::new(&VirtualWorkspaceSettings::default(), &LayoutSettings::default())
}

/// A path for a layout file in a directory of its own, which goes, file and all, when the returned
/// `TempDir` drops at the end of the test, passed or failed.
fn layout_file() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let path = dir.path().join("layout.ron");
    (dir, path)
}

mod announcements;
mod launch_memory;
mod matching;
mod save_load;
mod schema;
mod startup;
