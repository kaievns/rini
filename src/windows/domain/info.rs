//! What rini knows about an app or a window, however it learned it.
//!
//! These are the shapes the Accessibility and window-server adapters fill in and the rest of rini
//! reads. The reads themselves live in `crate::windows::platform`; the conversions from
//! `NSRunningApplication` and `AXUIElement` stay with them.

use std::path::PathBuf;

use objc2_core_foundation::{CGRect, CGSize};
use rini_core::ids::{WindowServerId, pid_t};

#[derive(Debug, Clone)]
pub struct AppInfo {
    pub bundle_id: Option<String>,
    pub localized_name: Option<String>,
}

#[derive(Debug, Clone)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each is an attribute Accessibility reports on its own"
)]
pub struct WindowInfo {
    pub is_standard: bool,
    pub is_root: bool,
    pub is_minimized: bool,
    pub is_resizable: bool,
    pub title: String,
    pub frame: CGRect,
    pub min_size: Option<CGSize>,
    pub max_size: Option<CGSize>,
    pub sys_id: Option<WindowServerId>,
    pub bundle_id: Option<String>,
    pub path: Option<PathBuf>,
    pub ax_role: Option<String>,
    pub ax_subrole: Option<String>,
    /// `AXModal`: the window blocks its app until dismissed. Electron and Zoom report such
    /// dialogs as `AXStandardWindow`, so the subrole alone does not tell them from app windows.
    pub is_modal: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct WindowServerInfo {
    pub id: WindowServerId,
    pub pid: pid_t,
    pub layer: i32,
    pub frame: CGRect,
    pub min_frame: CGSize,
    pub max_frame: CGSize,
}

/// The one radius a window rounds at from the four corners the window server reports: the largest,
/// since Core Animation takes one radius a layer. `None` when no corner is rounded, which is how the
/// server answers for a window it has no radius for.
pub fn reported_radius(corners: &[f64]) -> Option<f64> {
    corners
        .iter()
        .copied()
        .filter(|corner| corner.is_finite() && *corner > 0.0)
        .reduce(f64::max)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reported_radius_is_the_largest_corner_above_zero() {
        assert_eq!(reported_radius(&[16.0; 4]), Some(16.0));
        assert_eq!(reported_radius(&[8.0, 8.0, 0.0, 0.0]), Some(8.0));
        assert_eq!(
            reported_radius(&[8.0, 16.0, 0.0, 12.0]),
            Some(16.0),
            "unequal corners: the largest"
        );
        assert_eq!(reported_radius(&[f64::NAN, 12.0, 0.0, 0.0]), Some(12.0));
        assert_eq!(reported_radius(&[0.0; 4]), None);
        assert_eq!(reported_radius(&[]), None);
        assert_eq!(reported_radius(&[f64::NAN; 4]), None);
    }
}
