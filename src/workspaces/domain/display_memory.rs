//! What rini remembers about the machine, as opposed to what it remembers about a layout.
//!
//! Three records with the same lifetime: which display owns which native space and which display
//! each window belongs to ([`DisplayAffinity`]), where an application's windows belong under a key
//! that survives the application ([`LaunchMemory`]), and the width each application was last given
//! on each display shape ([`AppWidths`]).
//!
//! They are kept apart from the workspace layouts, saved and validated on their own. Sharing the
//! layouts' lifetime, an unreadable layout, or one written by a newer schema, would discard the
//! display memory as well: every window re-homed from scratch on the next display change and every
//! relaunched application landed in a default slot. The memory is about the hardware; the layout is about the windows, and a layout can
//! be thrown away without forgetting which monitor a window lives on.
//!
//! The fields are public on purpose. This is a place to keep the records together, not a facade over
//! them; wrapping every method of each would be another API to keep in step with them.

use serde::{Deserialize, Serialize};

use crate::workspaces::domain::app_widths::AppWidths;
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
    /// The width each application was last given on each display shape. Left out of a file while
    /// empty, and skipped by a rini from before it, which reads the rest of the file as it was.
    #[serde(default, skip_serializing_if = "AppWidths::is_empty")]
    pub app_widths: AppWidths,
}
