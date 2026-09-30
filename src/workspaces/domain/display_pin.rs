//! Which display an app is pinned to, by ROLE rather than by name.
//!
//! "Slack, Outlook and Messages live on the laptop screen" is a rule about the machine's own display,
//! not about a particular monitor. A UUID cannot say it: the external at home and the one at the
//! office have different UUIDs, and a rule naming one is silently inert at the other desk. So a pin
//! names a role and this module resolves it against whatever is attached.
//!
//! A pin is a DEFAULT, not a law. It decides where a window belongs when the arrangement in force has
//! not been told otherwise, and an explicit move writes a home that beats it — which is what "unless I
//! explicitly move it there" means. And a role that nothing fills resolves to nothing, so closing the
//! lid does not strand a window on a display that is not there.

use serde::{Deserialize, Serialize};

use crate::displays::domain::screen::ScreenInfo;

/// The display a rule pins a window to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DisplayRole {
    /// The machine's own screen.
    Internal,
    /// Anything else. With two externals attached it is the leftmost, because a rule that says
    /// "external" on a three-display desk has to mean something and the alternative is nothing.
    External,
}

/// The display filling `role`, or `None` when nothing does.
///
/// `None` is the lid-closed case and it means the pin is INERT: with no built-in attached a window
/// pinned to it goes wherever there is, rather than being held for a display that is not there.
///
/// Screens are taken in the order given, which the caller sorts into physical order, so "external"
/// is stable rather than dependent on the order macOS happens to report.
pub fn display_for_role<'a>(
    role: DisplayRole,
    screens: impl IntoIterator<Item = &'a ScreenInfo>,
) -> Option<&'a str> {
    screens
        .into_iter()
        .filter(|screen| !screen.display_uuid.is_empty())
        .find(|screen| match role {
            DisplayRole::Internal => screen.is_builtin,
            DisplayRole::External => !screen.is_builtin,
        })
        .map(|screen| screen.display_uuid.as_str())
}

#[cfg(test)]
mod tests {
    use objc2_core_foundation::{CGPoint, CGRect, CGSize};
    use rini_core::ids::ScreenId;

    use super::*;

    fn screen(uuid: &str, x: f64, is_builtin: bool) -> ScreenInfo {
        ScreenInfo {
            id: ScreenId::new(1),
            frame: CGRect::new(CGPoint::new(x, 0.0), CGSize::new(1000.0, 1000.0)),
            bounds: CGRect::new(CGPoint::new(x, 0.0), CGSize::new(1000.0, 1000.0)),
            display_uuid: uuid.to_owned(),
            name: None,
            space: None,
            is_builtin,
        }
    }

    #[test]
    fn internal_is_the_machines_own_screen() {
        let screens = [
            screen("studio", 0.0, false),
            screen("builtin", 1000.0, true),
        ];
        assert_eq!(
            display_for_role(DisplayRole::Internal, &screens),
            Some("builtin")
        );
    }

    #[test]
    fn external_is_anything_else() {
        let screens = [
            screen("builtin", 0.0, true),
            screen("studio", 1000.0, false),
        ];
        assert_eq!(display_for_role(DisplayRole::External, &screens), Some("studio"));
    }

    /// The reported escape hatch: "unless the lid is closed and external is the only one available".
    /// A pin nothing can satisfy has to be inert, not a reason to hold a window off screen.
    #[test]
    fn a_role_nothing_fills_resolves_to_nothing() {
        let lid_shut = [screen("studio", 0.0, false)];
        assert_eq!(display_for_role(DisplayRole::Internal, &lid_shut), None);

        let laptop_alone = [screen("builtin", 0.0, true)];
        assert_eq!(display_for_role(DisplayRole::External, &laptop_alone), None);
    }

    /// Whichever external comes first in the order the caller gives, so a three-display desk gets a
    /// stable answer instead of depending on how macOS enumerated the screens.
    #[test]
    fn the_first_external_wins_when_there_are_two() {
        let screens = [
            screen("builtin", 0.0, true),
            screen("left-4k", 1000.0, false),
            screen("right-4k", 2000.0, false),
        ];
        assert_eq!(
            display_for_role(DisplayRole::External, &screens),
            Some("left-4k")
        );
    }

    /// A screen whose UUID macOS has not reported yet cannot be a pin target: homing a window to an
    /// empty string records a display that can never be found again.
    #[test]
    fn a_nameless_screen_is_never_the_answer() {
        let screens = [screen("", 0.0, true), screen("builtin", 1000.0, true)];
        assert_eq!(
            display_for_role(DisplayRole::Internal, &screens),
            Some("builtin")
        );
    }

    #[test]
    fn no_screens_at_all_fills_no_role() {
        assert_eq!(display_for_role(DisplayRole::Internal, &[]), None);
        assert_eq!(display_for_role(DisplayRole::External, &[]), None);
    }
}
