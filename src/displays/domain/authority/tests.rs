use super::*;

const SLEEP: Hold = Hold::Sleep;
const SESSION: Hold = Hold::Session;

fn on(hold: Hold) -> LifecycleEdge { LifecycleEdge::On(hold) }

fn off(hold: Hold) -> LifecycleEdge { LifecycleEdge::Off(hold) }

#[test]
fn a_lifecycle_moves_through_its_holds_and_owes_one_release_after_a_hold_comes_off() {
    use Held::{Both, Neither, One};
    use Lifecycle::{Holding, Releasing};
    let forwarded = LifecycleEdge::Forwarded;
    let rows = [
        (Holding(Neither), on(SLEEP), Holding(One(SLEEP))),
        (Holding(One(SLEEP)), on(SLEEP), Holding(One(SLEEP))),
        (Holding(One(SESSION)), on(SLEEP), Holding(Both)),
        (Holding(Both), on(SESSION), Holding(Both)),
        (Releasing(None), on(SESSION), Holding(One(SESSION))),
        (Releasing(Some(SESSION)), on(SLEEP), Holding(Both)),
        (Releasing(Some(SLEEP)), on(SLEEP), Holding(One(SLEEP))),
        (Holding(One(SLEEP)), off(SLEEP), Releasing(None)),
        (Holding(Neither), off(SLEEP), Releasing(None)),
        (Holding(One(SESSION)), off(SLEEP), Releasing(Some(SESSION))),
        (Holding(Both), off(SLEEP), Releasing(Some(SESSION))),
        (Holding(Both), off(SESSION), Releasing(Some(SLEEP))),
        (Releasing(Some(SESSION)), off(SESSION), Releasing(None)),
        (Releasing(Some(SESSION)), off(SLEEP), Releasing(Some(SESSION))),
        (Releasing(None), off(SLEEP), Releasing(None)),
        (Releasing(None), forwarded, Holding(Neither)),
        (Releasing(Some(SESSION)), forwarded, Holding(One(SESSION))),
        (Holding(Both), forwarded, Holding(Both)),
        (Holding(Neither), forwarded, Holding(Neither)),
    ];
    for (from, edge, to) in rows {
        assert_eq!(from.next(edge), to, "{from:?} on {edge:?}");
    }
}

#[test]
fn a_lifecycle_holds_while_any_hold_is_on_and_releases_only_once_one_came_off() {
    use Held::{Both, Neither, One};
    use Lifecycle::{Holding, Releasing};
    let rows = [
        (Holding(Neither), false, false),
        (Holding(One(SLEEP)), true, false),
        (Holding(One(SESSION)), true, false),
        (Holding(Both), true, false),
        (Releasing(None), false, true),
        (Releasing(Some(SLEEP)), true, true),
        (Releasing(Some(SESSION)), true, true),
    ];
    for (lifecycle, holds, releases) in rows {
        assert_eq!(
            (lifecycle.holds(), lifecycle.releases()),
            (holds, releases),
            "{lifecycle:?}"
        );
    }
}

/// The three flags the actor kept before: each edge set or cleared them as below. Every sequence of
/// five edges answers the same through the machine as through the flags.
#[test]
fn a_lifecycle_answers_as_the_sleeping_session_and_release_flags_did_over_every_five_edges() {
    #[derive(Clone, Copy, Default)]
    struct Flags {
        sleeping: bool,
        session_inactive: bool,
        release: bool,
    }
    fn apply(flags: Flags, edge: LifecycleEdge) -> Flags {
        match edge {
            LifecycleEdge::On(Hold::Sleep) => Flags {
                sleeping: true,
                release: false,
                ..flags
            },
            LifecycleEdge::On(Hold::Session) => Flags {
                session_inactive: true,
                release: false,
                ..flags
            },
            LifecycleEdge::Off(Hold::Sleep) => Flags {
                sleeping: false,
                release: true,
                ..flags
            },
            LifecycleEdge::Off(Hold::Session) => Flags {
                session_inactive: false,
                release: true,
                ..flags
            },
            LifecycleEdge::Forwarded => Flags { release: false, ..flags },
        }
    }
    let edges = [
        on(SLEEP),
        on(SESSION),
        off(SLEEP),
        off(SESSION),
        LifecycleEdge::Forwarded,
    ];
    let mut walks = vec![(Lifecycle::default(), Flags::default())];
    for _ in 0..5 {
        walks = walks
            .into_iter()
            .flat_map(|(lifecycle, flags)| {
                edges.iter().map(move |&edge| (lifecycle.next(edge), apply(flags, edge)))
            })
            .collect();
        for (lifecycle, flags) in &walks {
            assert_eq!(
                (lifecycle.holds(), lifecycle.releases()),
                (flags.sleeping || flags.session_inactive, flags.release),
                "{lifecycle:?}"
            );
        }
    }
}

fn windows(pairs: &[(u32, u64)]) -> HashMap<WindowServerId, SpaceId> {
    pairs
        .iter()
        .map(|&(wsid, space)| (WindowServerId::new(wsid), SpaceId::new(space)))
        .collect()
}

/// A churn at `epoch`, under way when `flags` is `Some`.
fn churn(epoch: u64, flags: Option<DisplayReconfigFlags>) -> Churn {
    Churn {
        epoch,
        run: flags.map(|flags| ChurnRun {
            flags,
            stability: Stability::default(),
            windows_before: windows(&[(1, 10)]),
        }),
    }
}

#[test]
fn a_churn_begins_extends_and_ends_moving_its_epoch_on_each_time() {
    use DisplayReconfigFlags as F;
    let now = windows(&[(2, 20)]);
    let reconfigured = |flags| ChurnEdge::Reconfigured(flags, &now);
    let rows = [
        (
            churn(4, None),
            reconfigured(F::ADD),
            ChurnStep::Began,
            Some(F::ADD),
            5,
        ),
        (
            churn(4, Some(F::ADD)),
            reconfigured(F::MOVED),
            ChurnStep::Extended,
            Some(F::ADD | F::MOVED),
            5,
        ),
        (
            churn(4, Some(F::ADD)),
            ChurnEdge::Ended(4),
            ChurnStep::Ended,
            None,
            5,
        ),
        (
            churn(4, Some(F::ADD)),
            ChurnEdge::Ended(3),
            ChurnStep::Stale,
            Some(F::ADD),
            4,
        ),
        (churn(4, None), ChurnEdge::Ended(4), ChurnStep::Stale, None, 4),
        (
            churn(u64::MAX, None),
            reconfigured(F::ADD),
            ChurnStep::Began,
            Some(F::ADD),
            0,
        ),
    ];
    for (mut churn, edge, step, flags, epoch) in rows {
        assert_eq!(churn.next(edge), step);
        assert_eq!(churn.run.as_ref().map(|run| run.flags), flags, "{step:?}");
        assert_eq!(churn.epoch(), epoch, "{step:?}");
        assert_eq!(churn.is_active(), flags.is_some(), "{step:?}");
    }
}

/// The windows before a churn are the ones forwarded when it began: a reconfiguration mid-churn
/// keeps them.
#[test]
fn a_churn_keeps_the_windows_it_began_with() {
    let mut churn = Churn::default();
    let first = windows(&[(1, 10)]);
    churn.next(ChurnEdge::Reconfigured(DisplayReconfigFlags::ADD, &first));
    churn.next(ChurnEdge::Reconfigured(
        DisplayReconfigFlags::MOVED,
        &windows(&[(2, 20)]),
    ));
    let epoch = churn.epoch();

    let run = churn.run_at(epoch).expect("a churn under way at its own epoch");
    assert_eq!(run.windows_before, first);
    assert!(
        churn.run_at(epoch - 1).is_none(),
        "a check from before the last reconfiguration"
    );
}

#[test]
fn a_refresh_schedules_ends_and_waits_for_a_space_switch_on_its_own() {
    let idle = Refresh::default();
    let pending = Refresh {
        pending: Pending::Scheduled,
        ..idle
    };
    let switched = Refresh {
        space_switch: SpaceSwitch::Unconfirmed,
        ..idle
    };
    let both = Refresh {
        pending: Pending::Scheduled,
        space_switch: SpaceSwitch::Unconfirmed,
    };
    let rows = [
        (idle, RefreshEdge::Scheduled, pending),
        (switched, RefreshEdge::Scheduled, both),
        (pending, RefreshEdge::Ended, idle),
        (both, RefreshEdge::Ended, switched),
        (idle, RefreshEdge::SpaceSwitched, switched),
        (pending, RefreshEdge::SpaceSwitched, both),
        (both, RefreshEdge::Forwarded, pending),
        (switched, RefreshEdge::Forwarded, idle),
        (idle, RefreshEdge::Forwarded, idle),
    ];
    for (from, edge, to) in rows {
        assert_eq!(from.next(edge), to, "{from:?} on {edge:?}");
    }
    assert!(both.is_pending() && both.awaits_space_switch());
    assert!(!idle.is_pending() && !idle.awaits_space_switch());
}
