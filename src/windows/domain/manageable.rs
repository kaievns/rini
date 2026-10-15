//! Whether rini manages a window, and whether one the app no longer lists is gone, decided over
//! what Accessibility and the window server said about it.
//!
//! Every fact arrives as a value. A window-server fact is `None` when the window server gave no
//! answer, and an unanswered fact is no evidence either way: only an answer turns a window away.

use crate::windows::domain::info::{WindowInfo, WindowServerInfo};
use crate::windows::domain::stack::{NORMAL_WINDOW_LEVEL, WindowLevel};

/// What the manageability verdict is decided on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Facts {
    pub minimized: bool,
    /// Accessibility calls it a standard window.
    pub ax_standard: bool,
    /// Accessibility calls it a top-level window, one with no parent.
    pub ax_root: bool,
    pub server: ServerFacts,
}

/// What the window server says of a window, each fact `None` where it gave no answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ServerFacts {
    /// Its layer, as the catalogue last recorded it.
    pub layer: Option<i32>,
    /// It is on more than one space: on every space, as a sticky window is.
    pub sticky: Option<bool>,
    pub level: Option<WindowLevel>,
}

impl Facts {
    /// `info`'s Accessibility facts beside the window server's `server`.
    pub fn of(info: &WindowInfo, server: ServerFacts) -> Self {
        Self {
            minimized: info.is_minimized,
            ax_standard: info.is_standard,
            ax_root: info.is_root,
            server,
        }
    }
}

/// Whether rini manages a window: one that is not minimized, sits on layer 0, is on one space, at
/// the normal level, and that Accessibility calls a standard top-level window.
pub fn manageable(facts: Facts) -> bool {
    let ServerFacts { layer, sticky, level } = facts.server;
    !facts.minimized
        && layer.is_none_or(|layer| layer == 0)
        && sticky != Some(true)
        && level.is_none_or(|level| level == NORMAL_WINDOW_LEVEL)
        && facts.ax_standard
        && facts.ax_root
}

/// Whether the window server's picture of a window the app no longer lists says it is gone: not a
/// suitable window, off layer 0, too small to be real, or ordered out. A failed query (`None`) is
/// not evidence; only an explicit negative observation retires a window.
pub fn looks_gone(
    info: &WindowServerInfo,
    suitable: Option<bool>,
    ordered_in: Option<bool>,
) -> bool {
    const MIN_REAL_WINDOW_DIMENSION: f64 = 2.0;
    let too_small = info.frame.size.width.abs() < MIN_REAL_WINDOW_DIMENSION
        || info.frame.size.height.abs() < MIN_REAL_WINDOW_DIMENSION;
    suitable == Some(false) || info.layer != 0 || too_small || ordered_in == Some(false)
}

#[cfg(test)]
mod tests {
    use objc2_core_foundation::{CGPoint, CGRect, CGSize};
    use rini_core::ids::WindowServerId;

    use super::*;

    /// A standard top-level window on layer 0, one space and the normal level.
    fn managed() -> Facts {
        Facts {
            minimized: false,
            ax_standard: true,
            ax_root: true,
            server: ServerFacts {
                layer: Some(0),
                sticky: Some(false),
                level: Some(NORMAL_WINDOW_LEVEL),
            },
        }
    }

    #[test]
    fn a_standard_top_level_window_on_layer_0_one_space_and_the_normal_level_is_managed() {
        assert!(manageable(managed()));
    }

    #[test]
    fn a_minimized_window_is_not_managed() {
        assert!(!manageable(Facts { minimized: true, ..managed() }));
    }

    #[test]
    fn a_window_off_layer_0_is_not_managed() {
        let server = ServerFacts {
            layer: Some(3),
            ..managed().server
        };
        assert!(!manageable(Facts { server, ..managed() }));
    }

    #[test]
    fn a_sticky_window_is_not_managed() {
        let server = ServerFacts {
            sticky: Some(true),
            ..managed().server
        };
        assert!(!manageable(Facts { server, ..managed() }));
    }

    #[test]
    fn a_window_above_or_below_the_normal_level_is_not_managed() {
        for level in [3, -1] {
            let server = ServerFacts {
                level: Some(level),
                ..managed().server
            };
            assert!(!manageable(Facts { server, ..managed() }), "level {level}");
        }
    }

    #[test]
    fn a_window_accessibility_does_not_call_standard_is_not_managed() {
        assert!(!manageable(Facts {
            ax_standard: false,
            ..managed()
        }));
    }

    #[test]
    fn a_window_with_a_parent_is_not_managed() {
        assert!(!manageable(Facts { ax_root: false, ..managed() }));
    }

    #[test]
    fn a_window_server_that_answers_nothing_turns_no_window_away() {
        assert!(manageable(Facts {
            server: ServerFacts::default(),
            ..managed()
        }));
        assert!(
            !manageable(Facts {
                ax_standard: false,
                server: ServerFacts::default(),
                ..managed()
            }),
            "but Accessibility's answers still count"
        );
    }

    fn info(layer: i32, w: f64, h: f64) -> WindowServerInfo {
        WindowServerInfo {
            id: WindowServerId::new(1),
            pid: 1,
            layer,
            frame: CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(w, h)),
            min_frame: CGSize::new(0.0, 0.0),
            max_frame: CGSize::new(0.0, 0.0),
        }
    }

    #[test]
    fn a_window_is_gone_only_on_an_explicit_negative_observation() {
        let real = info(0, 800.0, 600.0);
        assert!(
            !looks_gone(&real, None, None),
            "unknown suitability and order are not evidence"
        );
        assert!(!looks_gone(&real, Some(true), Some(true)));
        assert!(looks_gone(&real, Some(false), None));
        assert!(looks_gone(&real, None, Some(false)));
        assert!(looks_gone(&info(1, 800.0, 600.0), None, None), "off layer 0");
        assert!(
            looks_gone(&info(0, 1.0, 600.0), None, None),
            "a sliver is not a window"
        );
    }
}
