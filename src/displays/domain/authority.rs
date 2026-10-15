//! What the spaces actor holds between notifications, as three machines: the lifecycle holds of
//! sleep and the login window, the display churn, and the screen refresh. Each changes only through
//! its `next`.
use rini_core::ids::SpaceId;
use rini_skylight_sys::{DisplayReconfigFlags, WindowServerId};
use rustc_hash::FxHashMap as HashMap;

use crate::displays::domain::settling::Stability;

/// What holds the actor's snapshots back while the native picture is part-formed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hold {
    Sleep,
    /// The login window owns the displays.
    Session,
}

/// Which holds are on.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum Held {
    #[default]
    Neither,
    One(Hold),
    Both,
}

impl Held {
    fn with(self, hold: Hold) -> Self {
        match self {
            Self::Neither => Self::One(hold),
            Self::One(on) if on == hold => self,
            Self::One(_) | Self::Both => Self::Both,
        }
    }

    fn without(self, hold: Hold) -> Option<Hold> {
        match self {
            Self::Neither => None,
            Self::One(on) => (on != hold).then_some(on),
            Self::Both => Some(match hold {
                Hold::Sleep => Hold::Session,
                Hold::Session => Hold::Sleep,
            }),
        }
    }
}

impl From<Option<Hold>> for Held {
    fn from(hold: Option<Hold>) -> Self { hold.map_or(Self::Neither, Self::One) }
}

/// The lifecycle holds, and whether the reactor's own lifecycle quarantine is owed a release.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lifecycle {
    /// The holds on, with no release owed: nothing forwarded releases the reactor.
    Holding(Held),
    /// A hold came off: the next snapshot forwarded releases the reactor. The other hold can still
    /// be on.
    Releasing(Option<Hold>),
}

impl Default for Lifecycle {
    fn default() -> Self { Self::Holding(Held::Neither) }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LifecycleEdge {
    On(Hold),
    Off(Hold),
    /// A snapshot was forwarded.
    Forwarded,
}

impl Lifecycle {
    pub fn next(self, edge: LifecycleEdge) -> Self {
        match (self, edge) {
            (Self::Holding(held), LifecycleEdge::On(hold)) => Self::Holding(held.with(hold)),
            (Self::Releasing(rest), LifecycleEdge::On(hold)) => {
                Self::Holding(Held::from(rest).with(hold))
            }
            (Self::Holding(held), LifecycleEdge::Off(hold)) => Self::Releasing(held.without(hold)),
            (Self::Releasing(rest), LifecycleEdge::Off(hold)) => {
                Self::Releasing(rest.filter(|on| *on != hold))
            }
            (Self::Holding(held), LifecycleEdge::Forwarded) => Self::Holding(held),
            (Self::Releasing(rest), LifecycleEdge::Forwarded) => Self::Holding(Held::from(rest)),
        }
    }

    pub fn holds(self) -> bool {
        !matches!(self, Self::Holding(Held::Neither) | Self::Releasing(None))
    }

    /// Whether the next snapshot forwarded releases the reactor's lifecycle quarantine.
    pub fn releases(self) -> bool { matches!(self, Self::Releasing(_)) }
}

/// A display reconfiguration under way.
#[derive(Debug)]
pub struct ChurnRun {
    /// Every reconfiguration it has heard.
    pub flags: DisplayReconfigFlags,
    pub stability: Stability,
    /// Which active space each window was on when it began.
    pub windows_before: HashMap<WindowServerId, SpaceId>,
}

/// The display churn, and the epoch a stabilization check carries. The epoch moves on at every
/// reconfiguration and at the end, so a check scheduled before either is stale.
#[derive(Debug, Default)]
pub struct Churn {
    epoch: u64,
    run: Option<ChurnRun>,
}

pub enum ChurnEdge<'a> {
    /// A reconfiguration that begins a churn, heard with the windows forwarded last.
    Reconfigured(DisplayReconfigFlags, &'a HashMap<WindowServerId, SpaceId>),
    /// The churn a check at this epoch belongs to ends.
    Ended(u64),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChurnStep {
    Began,
    /// A churn under way heard another reconfiguration: its samples start over.
    Extended,
    Ended,
    /// The check is from an epoch since moved on from, or no churn is under way.
    Stale,
}

impl Churn {
    pub fn next(&mut self, edge: ChurnEdge) -> ChurnStep {
        match edge {
            ChurnEdge::Reconfigured(flags, windows) => {
                self.epoch = self.epoch.wrapping_add(1);
                match &mut self.run {
                    Some(run) => {
                        run.flags |= flags;
                        run.stability.reset();
                        ChurnStep::Extended
                    }
                    None => {
                        self.run = Some(ChurnRun {
                            flags,
                            stability: Stability::default(),
                            windows_before: windows.clone(),
                        });
                        ChurnStep::Began
                    }
                }
            }
            ChurnEdge::Ended(epoch) => {
                if self.run_at(epoch).is_none() {
                    return ChurnStep::Stale;
                }
                self.run = None;
                self.epoch = self.epoch.wrapping_add(1);
                ChurnStep::Ended
            }
        }
    }

    /// The churn a check at `epoch` belongs to, if it is still under way.
    pub fn run_at(&mut self, epoch: u64) -> Option<&mut ChurnRun> {
        self.run.as_mut().filter(|_| self.epoch == epoch)
    }

    pub fn is_active(&self) -> bool { self.run.is_some() }

    pub fn epoch(&self) -> u64 { self.epoch }
}

/// Whether a screen refresh is scheduled.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum Pending {
    #[default]
    Idle,
    Scheduled,
}

/// Whether a space switch has been heard that no snapshot forwarded since has shown.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum SpaceSwitch {
    #[default]
    Shown,
    Unconfirmed,
}

/// The screen refresh. Its two parts move independently: a switch heard mid-churn waits with no
/// refresh scheduled, and a refresh scheduled for any reason sees the switch first.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Refresh {
    pub pending: Pending,
    pub space_switch: SpaceSwitch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefreshEdge {
    Scheduled,
    /// The scheduled refresh ran out: dropped, given up, or forwarded.
    Ended,
    SpaceSwitched,
    Forwarded,
}

impl Refresh {
    pub fn next(self, edge: RefreshEdge) -> Self {
        match edge {
            RefreshEdge::Scheduled => Self {
                pending: Pending::Scheduled,
                ..self
            },
            RefreshEdge::Ended => Self { pending: Pending::Idle, ..self },
            RefreshEdge::SpaceSwitched => Self {
                space_switch: SpaceSwitch::Unconfirmed,
                ..self
            },
            RefreshEdge::Forwarded => Self {
                space_switch: SpaceSwitch::Shown,
                ..self
            },
        }
    }

    pub fn is_pending(self) -> bool { self.pending == Pending::Scheduled }

    pub fn awaits_space_switch(self) -> bool { self.space_switch == SpaceSwitch::Unconfirmed }
}

#[cfg(test)]
mod tests;
