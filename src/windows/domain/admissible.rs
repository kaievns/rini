//! Which Accessibility windows rini will register at all.
//!
//! Distinct from `rules`, which is the user's config, and from `state::is_manageable`, which is about
//! a window rini has already taken on. This is the door: four reasons to turn a window away, applied
//! before anything else knows it exists.
//!
//! Lifted out of `platform::app_actor::register_window`, where they were four early returns in a
//! ninety-line function against a live `AXUIElement` — so the strings below, which is what they
//! mostly are, could not be exercised at all.

/// What is known about a candidate window before rini decides whether to take it.
#[derive(Debug, Clone, Copy, Default)]
pub struct Candidate<'a> {
    /// The window server has a visible window for this one. An AX window without one is not on
    /// screen, whatever AX says.
    pub has_visible_peer: bool,
    pub is_minimized: bool,
    pub bundle_id: Option<&'a str>,
    pub path: Option<&'a str>,
    pub ax_role: Option<&'a str>,
}

/// Why a window was turned away.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rejected {
    /// No visible window-server peer, and not merely minimized.
    NoVisiblePeer,
    /// A widget. They report as windows and are not ones a user arranges.
    Widget,
    /// An app extension bundle (`.appex`), same reasoning.
    AppExtension,
    /// A popover or a menu. AX calls them windows; they are chrome.
    NonStandardRole,
}

/// `None` when rini will register the window.
pub fn rejection(candidate: &Candidate<'_>) -> Option<Rejected> {
    // Minimized is the exception rather than an oversight: a minimized window has no visible peer by
    // definition, and dropping it would lose the window while it sits in the Dock.
    if !candidate.has_visible_peer && !candidate.is_minimized {
        return Some(Rejected::NoVisiblePeer);
    }
    if candidate.bundle_id.is_some_and(is_widget_bundle) {
        return Some(Rejected::Widget);
    }
    if candidate.path.is_some_and(is_app_extension_path) {
        return Some(Rejected::AppExtension);
    }
    if matches!(candidate.ax_role, Some("AXPopover") | Some("AXMenu")) {
        return Some(Rejected::NonStandardRole);
    }
    None
}

/// Matches both `com.example.widget` and `com.example.widget.something`, case-insensitively.
///
/// Not a `contains("widget")`: an application legitimately named "Widgets" would be turned away.
fn is_widget_bundle(bundle_id: &str) -> bool {
    let lower = bundle_id.to_ascii_lowercase();
    lower.ends_with(".widget") || lower.contains(".widget.")
}

fn is_app_extension_path(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    lower.contains(".appex/") || lower.ends_with(".appex")
}

/// Whether the window server has a visible window for this AX one.
///
/// An AX window with no window-server id at all counts as visible: rini has nothing to check it
/// against, and refusing every window whose id has not arrived yet would drop windows during the
/// gap between Accessibility reporting one and the server catching up. With an id, the server's
/// info is the answer.
pub fn has_visible_peer(window_server_id_known: bool, server_reported: bool) -> bool {
    !window_server_id_known || server_reported
}

/// Applications whose AX tree lies about a window being standard unless it has a title element.
///
/// A heuristic, and known to be one: `app_actor` carries a TODO about replacing it with something
/// modelled on AeroSpace's AX dumps. Named here so the list is one place rather than a condition
/// inside a ninety-line function.
pub fn needs_title_element_to_be_standard(bundle_id: Option<&str>) -> bool {
    matches!(
        bundle_id,
        Some("com.googlecode.iterm2") | Some("com.apple.TextInputUI.xpc.CursorUIViewService")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok() -> Candidate<'static> {
        Candidate {
            has_visible_peer: true,
            ..Default::default()
        }
    }

    #[test]
    fn a_window_with_a_visible_peer_and_nothing_odd_is_admitted() {
        assert_eq!(rejection(&ok()), None);
    }

    #[test]
    fn a_window_the_window_server_cannot_see_is_turned_away() {
        let c = Candidate {
            has_visible_peer: false,
            ..Default::default()
        };
        assert_eq!(rejection(&c), Some(Rejected::NoVisiblePeer));
    }

    /// The exception that keeps a minimized window from being forgotten while it sits in the Dock.
    #[test]
    fn a_minimized_window_is_admitted_without_a_visible_peer() {
        let c = Candidate {
            has_visible_peer: false,
            is_minimized: true,
            ..Default::default()
        };
        assert_eq!(rejection(&c), None);
    }

    #[test]
    fn a_widget_bundle_is_turned_away_in_both_of_its_forms() {
        for id in [
            "com.example.widget",
            "com.example.widget.extra",
            "COM.EXAMPLE.WIDGET",
        ] {
            let c = Candidate { bundle_id: Some(id), ..ok() };
            assert_eq!(rejection(&c), Some(Rejected::Widget), "{id}");
        }
    }

    /// The rule is suffix-or-dotted-infix, not a substring search, so an application whose name
    /// merely contains the word is still admitted.
    #[test]
    fn an_application_named_after_widgets_is_not_a_widget() {
        for id in [
            "com.example.widgetstudio",
            "com.widgets.app",
            "com.example.WidgetMaker",
        ] {
            let c = Candidate { bundle_id: Some(id), ..ok() };
            assert_eq!(rejection(&c), None, "{id}");
        }
    }

    #[test]
    fn an_app_extension_path_is_turned_away_in_both_of_its_forms() {
        for path in [
            "/Applications/Thing.app/PlugIns/Helper.appex",
            "/Applications/Thing.app/PlugIns/Helper.appex/Contents/MacOS/Helper",
            "/Applications/Thing.app/PlugIns/HELPER.APPEX",
        ] {
            let c = Candidate { path: Some(path), ..ok() };
            assert_eq!(rejection(&c), Some(Rejected::AppExtension), "{path}");
        }
    }

    #[test]
    fn an_ordinary_path_is_admitted() {
        let c = Candidate {
            path: Some("/Applications/Ghostty.app"),
            ..ok()
        };
        assert_eq!(rejection(&c), None);
    }

    #[test]
    fn popovers_and_menus_are_chrome_rather_than_windows() {
        for role in ["AXPopover", "AXMenu"] {
            let c = Candidate { ax_role: Some(role), ..ok() };
            assert_eq!(rejection(&c), Some(Rejected::NonStandardRole), "{role}");
        }
        let c = Candidate {
            ax_role: Some("AXWindow"),
            ..ok()
        };
        assert_eq!(rejection(&c), None);
    }

    /// The order matters for the LOG more than the outcome: a window with two problems should be
    /// reported by the first one checked, so the message does not change when a later rule shifts.
    #[test]
    fn the_first_reason_is_the_one_reported() {
        let c = Candidate {
            has_visible_peer: false,
            bundle_id: Some("com.example.widget"),
            ax_role: Some("AXMenu"),
            ..Default::default()
        };
        assert_eq!(rejection(&c), Some(Rejected::NoVisiblePeer));
    }

    /// The asymmetry worth pinning: no id means "assume visible", because the id may simply not
    /// have arrived yet. An id the server does not know about means the window is not on screen.
    #[test]
    fn a_window_with_no_server_id_yet_counts_as_visible() {
        assert!(
            has_visible_peer(false, false),
            "no id yet, so nothing to contradict AX"
        );
        assert!(has_visible_peer(false, true));
        assert!(has_visible_peer(true, true));
        assert!(
            !has_visible_peer(true, false),
            "an id the server does not report is not on screen"
        );
    }

    #[test]
    fn only_the_two_known_liars_need_a_title_element() {
        assert!(needs_title_element_to_be_standard(Some("com.googlecode.iterm2")));
        assert!(needs_title_element_to_be_standard(Some(
            "com.apple.TextInputUI.xpc.CursorUIViewService"
        )));
        assert!(!needs_title_element_to_be_standard(Some(
            "com.mitchellh.ghostty"
        )));
        assert!(!needs_title_element_to_be_standard(None));
    }
}
