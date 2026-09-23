//! What rini remembers about the machine, as opposed to what it remembers about a layout.
//!
//! Two records with the same lifetime: which display owns which native space and which display each
//! window belongs to ([`DisplayAffinity`]), and where an application's windows belong under a key
//! that survives the application ([`LaunchMemory`]).
//!
//! They used to be two fields of `LayoutEngine`, saved in the same file section as the workspace
//! layouts and validated with them. That made one lifetime out of two: an unreadable layout, or a
//! layout written by a newer schema, discarded the display memory as well, and then every window was
//! re-homed from scratch on the next display change and every relaunched application landed in a
//! default slot. The memory is about the hardware; the layout is about the windows, and a layout can
//! be thrown away without forgetting which monitor a window lives on.
//!
//! The fields are public on purpose. This is a place to keep two records together, not a facade over
//! them; wrapping every method of both would be a third API to keep in step with the other two.

use serde::{Deserialize, Serialize};

use crate::workspaces::domain::display_affinity::DisplayAffinity;
use crate::workspaces::domain::launch_memory::LaunchMemory;

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct DisplayMemory {
    /// Which display owns which space, and which display each window belongs to.
    #[serde(default)]
    pub affinity: DisplayAffinity,
    /// Where each application's windows belong, keyed so it survives the application.
    ///
    /// Defaulted rather than required: a file written before one of these records existed is a file
    /// that remembers less, not a file that cannot be read.
    #[serde(default)]
    pub launch: LaunchMemory,
}
