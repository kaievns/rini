//! Which native space a window belongs to, as a decision rather than a set of store reads.
//!
//! `space_affinity` had six precedence orders over the same four sources, each spelled out inline
//! against live stores, so none of them could be exercised without a reactor and the names — `best`,
//! `assigned`, `authoritative`, `discovery`, `reported`, `geometry` — were the only statement of how
//! they differed. The reads stay there; the orders are here, over plain `Option<SpaceId>`.

use rini_core::ids::SpaceId;

/// Every answer available about one window, gathered before any of them is preferred.
///
/// `None` means the source had nothing to say, which is different from a source saying a window is
/// nowhere: only `native_fullscreen` means that, and it overrules the rest.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(crate) struct Candidates {
    /// What the window server reports, already arbitrated against a move rini has in flight.
    pub reported: Option<SpaceId>,
    /// The workspace assignment, but only while the window is parked in a workspace nobody is
    /// showing. A parked window's geometry is a lie, so this outranks it.
    pub parked_assignment: Option<SpaceId>,
    /// The workspace assignment, whatever the window's state.
    pub assignment: Option<SpaceId>,
    /// The space the window's frame lands on.
    pub geometry: Option<SpaceId>,
    /// Whether `geometry`'s space is one of the spaces currently showing.
    pub geometry_is_active: bool,
    /// macOS has the window in its own fullscreen space. Rini claims no space for it at all.
    pub native_fullscreen: bool,
}

impl Candidates {
    fn active_geometry(&self) -> Option<SpaceId> {
        self.geometry.filter(|_| self.geometry_is_active)
    }
}

/// The strongest claim, for deciding where a window IS.
///
/// Not a precedence, which is why it has to be written out: when the window server and a parked
/// assignment disagree, the window server wins, because a disagreement means something moved the
/// window since it was parked. When only the parked assignment answers, it wins over nothing.
/// Falling back to the plain assignment last is what keeps a window rini has never seen on screen
/// attached to the workspace it was put in.
///
/// A window macOS has taken fullscreen has no space rini claims, under this rule as under the
/// others. It is on a space of its own and is not managed while it is there, so answering with the
/// space it used to be assigned to is answering about a window that is not there.
pub(crate) fn authoritative(c: &Candidates) -> Option<SpaceId> {
    if c.native_fullscreen {
        return None;
    }
    if let Some(parked) = c.parked_assignment {
        return match c.reported {
            Some(reported) if reported != parked => Some(reported),
            _ => Some(parked),
        };
    }
    c.reported.or(c.assignment)
}

/// Where to PUT a window, given a frame and possibly a window-server id.
///
/// Geometry is the last resort rather than the first: a parked window's frame is off screen on
/// purpose, so believing it walks windows onto whichever display the park happens to overlap. That
/// feedback loop is recorded in `src/workspaces/docs/workspaces-and-displays.md`.
pub(crate) fn placement(c: &Candidates) -> Option<SpaceId> {
    if c.native_fullscreen {
        return None;
    }
    c.reported.or(c.parked_assignment).or(c.geometry)
}

/// Placement without consulting the window server, for callers that have only a frame.
pub(crate) fn geometry_only(c: &Candidates) -> Option<SpaceId> {
    if c.native_fullscreen {
        return None;
    }
    c.parked_assignment.or(c.geometry)
}

/// The best answer available, for a window that is already tracked.
pub(crate) fn best(c: &Candidates) -> Option<SpaceId> {
    authoritative(c).or_else(|| placement(c))
}

/// Where a newly discovered window belongs.
///
/// Prefers a SHOWING space over `best`'s fallbacks: a window rini is seeing for the first time is
/// almost always in front of the user, so geometry that lands on a visible space beats an
/// assignment inherited from a saved layout.
pub(crate) fn discovery(c: &Candidates) -> Option<SpaceId> {
    if c.native_fullscreen {
        return None;
    }
    authoritative(c).or_else(|| c.active_geometry()).or_else(|| best(c))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn space(n: u64) -> Option<SpaceId> {
        Some(SpaceId::new(n))
    }

    /// The one answer that is not a precedence. A parked window whose server-reported space differs
    /// has been moved by something else since it was parked, and that move is the newer fact.
    #[test]
    fn a_parked_window_reported_somewhere_else_is_where_the_server_says() {
        let c = Candidates {
            reported: space(2),
            parked_assignment: space(1),
            ..Default::default()
        };
        assert_eq!(authoritative(&c), space(2));
    }

    #[test]
    fn a_parked_window_the_server_cannot_place_stays_where_it_was_parked() {
        let c = Candidates {
            reported: None,
            parked_assignment: space(1),
            ..Default::default()
        };
        assert_eq!(authoritative(&c), space(1));
    }

    #[test]
    fn an_unparked_window_prefers_the_server_then_its_assignment() {
        let c = Candidates {
            reported: space(3),
            assignment: space(9),
            ..Default::default()
        };
        assert_eq!(authoritative(&c), space(3));
        let c = Candidates {
            reported: None,
            assignment: space(9),
            ..Default::default()
        };
        assert_eq!(
            authoritative(&c),
            space(9),
            "a window never seen on screen keeps its workspace"
        );
    }

    // Geometry is deliberately absent from `authoritative`: a parked frame is off screen on purpose.
    #[test]
    fn authoritative_never_falls_back_to_geometry() {
        let c = Candidates {
            geometry: space(5),
            geometry_is_active: true,
            ..Default::default()
        };
        assert_eq!(authoritative(&c), None);
    }

    #[test]
    fn placement_uses_geometry_only_when_nothing_better_answers() {
        let c = Candidates {
            geometry: space(5),
            ..Default::default()
        };
        assert_eq!(placement(&c), space(5));
        let c = Candidates {
            parked_assignment: space(1),
            geometry: space(5),
            ..Default::default()
        };
        assert_eq!(
            placement(&c),
            space(1),
            "a park outranks the frame it was parked at"
        );
        let c = Candidates {
            reported: space(2),
            parked_assignment: space(1),
            geometry: space(5),
            ..Default::default()
        };
        assert_eq!(placement(&c), space(2));
    }

    #[test]
    fn geometry_only_skips_the_window_server_answer() {
        let c = Candidates {
            reported: space(2),
            geometry: space(5),
            ..Default::default()
        };
        assert_eq!(geometry_only(&c), space(5));
        assert_eq!(
            placement(&c),
            space(2),
            "which is the only difference between the two"
        );
    }

    /// A window in front of the user beats an assignment restored from a file.
    #[test]
    fn discovery_prefers_a_showing_space_over_an_inherited_assignment() {
        let c = Candidates {
            assignment: space(7),
            geometry: space(5),
            geometry_is_active: true,
            ..Default::default()
        };
        assert_eq!(
            discovery(&c),
            space(7),
            "an assignment IS authoritative, so it still wins"
        );

        let c = Candidates {
            geometry: space(5),
            geometry_is_active: true,
            ..Default::default()
        };
        assert_eq!(discovery(&c), space(5));
    }

    #[test]
    fn discovery_ignores_geometry_that_lands_on_a_space_nobody_is_showing() {
        let c = Candidates {
            geometry: space(5),
            geometry_is_active: false,
            ..Default::default()
        };
        assert_eq!(
            discovery(&c),
            space(5),
            "it still falls through to best, which allows it"
        );
    }

    /// Every rule refuses a window macOS has taken fullscreen. It is on its own space and rini does
    /// not manage it while it is there, so there is no space to answer with — not even the one it was
    /// assigned to before, which is the answer three of these used to give.
    #[test]
    fn a_native_fullscreen_window_belongs_to_no_space_under_any_rule() {
        let c = Candidates {
            native_fullscreen: true,
            reported: space(2),
            parked_assignment: space(1),
            assignment: space(9),
            geometry: space(5),
            geometry_is_active: true,
        };
        assert_eq!(authoritative(&c), None);
        assert_eq!(placement(&c), None);
        assert_eq!(geometry_only(&c), None);
        assert_eq!(best(&c), None);
        assert_eq!(discovery(&c), None);
    }

    #[test]
    fn nothing_known_is_nowhere() {
        let c = Candidates::default();
        for got in [
            authoritative(&c),
            placement(&c),
            geometry_only(&c),
            best(&c),
            discovery(&c),
        ] {
            assert_eq!(got, None);
        }
    }
}
