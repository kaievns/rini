//! The reactor's per-window record: what it last learned about a window and whether it manages it.
use objc2_core_foundation::CGRect;

use crate::windows::domain::info::WindowInfo;

#[derive(Debug)]
pub struct WindowState {
    pub info: WindowInfo,
    /// The last known frame of the window. Always includes the last write.
    ///
    /// This value only updates monotonically with respect to writes; in other
    /// words, we only accept reads when we know they come after the last write.
    pub frame_monotonic: CGRect,
    pub is_manageable: bool,
    pub ignore_app_rule: bool,
}

impl From<WindowInfo> for WindowState {
    fn from(info: WindowInfo) -> WindowState {
        WindowState {
            frame_monotonic: info.frame,
            info,
            is_manageable: false,
            ignore_app_rule: false,
        }
    }
}

impl WindowState {
    pub fn is_effectively_manageable(&self) -> bool {
        self.is_manageable && !self.ignore_app_rule
    }

    pub fn matches_filter(&self, filter: WindowFilter) -> bool {
        match filter {
            WindowFilter::Manageable => self.is_manageable,
            WindowFilter::EffectivelyManageable => self.is_effectively_manageable(),
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub enum WindowFilter {
    Manageable,
    EffectivelyManageable,
}
