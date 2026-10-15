//! When the spaces actor asks again: how often a refresh retries, which display reconfigurations
//! start a churn, and when a churn has settled, at two identical topology samples and a quiet
//! window server (`src/displays/docs/topology.md`).
use rini_core::ids::{DisplayUuid, SpaceId};
use rini_skylight_sys::DisplayReconfigFlags;

use crate::displays::domain::screen::ScreenInfo;
use crate::displays::domain::snapshot::SpaceKinds;
use crate::displays::domain::topology::snapshot_spaces_are_coherent;

/// How many times a refresh asks again for a snapshot it can forward.
pub const REFRESH_MAX_RETRIES: u8 = 10;
/// How many times a churn's check asks again before the churn ends unsettled.
pub const DISPLAY_STABILIZE_MAX_ATTEMPTS: u8 = 10;
/// How many identical topology samples in a row settle a churn.
pub const DISPLAY_STABLE_REQUIRED_HITS: u8 = 2;

/// The attempt a refresh retries at, or `None` once it has retried `REFRESH_MAX_RETRIES` times.
pub fn refresh_retry(attempt: u8) -> Option<u8> {
    (attempt < REFRESH_MAX_RETRIES).then(|| attempt + 1)
}

/// Whether a display reconfiguration starts a churn.
///
/// A native space switch can emit a reconfiguration on a single display, and that is handled as a
/// space change. A display added, removed, moved, mirrored or switched on or off, a new main display,
/// a new mode and a new desktop shape all leave the active spaces unsettled for a while.
pub fn begins_display_churn(flags: DisplayReconfigFlags) -> bool {
    flags.intersects(
        DisplayReconfigFlags::ADD
            | DisplayReconfigFlags::REMOVE
            | DisplayReconfigFlags::MOVED
            | DisplayReconfigFlags::SET_MAIN
            | DisplayReconfigFlags::SET_MODE
            | DisplayReconfigFlags::ENABLED
            | DisplayReconfigFlags::DISABLED
            | DisplayReconfigFlags::MIRROR
            | DisplayReconfigFlags::UNMIRROR
            | DisplayReconfigFlags::DESKTOP_SHAPE_CHANGED,
    )
}

type Fingerprint = Vec<(DisplayUuid, u64, u64, u64, u64, Option<u64>)>;

/// The topology samples a churn has taken: the last one, and how many times in a row it was taken.
#[derive(Debug, Default)]
pub struct Stability(Option<(Fingerprint, u8)>);

impl Stability {
    pub fn reset(&mut self) { self.0 = None; }

    /// How many times in a row `screens` has now been sampled, or `None` for a new topology.
    fn sample(&mut self, screens: &[ScreenInfo]) -> Option<u8> {
        let fingerprint: Fingerprint = screens
            .iter()
            .map(|d| {
                let (origin, size) = (d.frame.origin, d.frame.size);
                let space = d.space.map(|space| space.get());
                let bits = (origin.x.to_bits(), origin.y.to_bits());
                (
                    d.display_uuid.clone(),
                    bits.0,
                    bits.1,
                    size.width.to_bits(),
                    size.height.to_bits(),
                    space,
                )
            })
            .collect();
        match &mut self.0 {
            Some((last, hits)) if *last == fingerprint => {
                *hits = hits.saturating_add(1);
                Some(*hits)
            }
            _ => {
                self.0 = Some((fingerprint, 1));
                None
            }
        }
    }
}

/// What a churn's check does next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Settle {
    /// Check again, as this attempt.
    Again(u8),
    /// End the churn unsettled, and refresh.
    GiveUp,
    /// The topology has settled: forward it and end the churn.
    Forward,
}

/// The step after check `attempt` sampled `screens`.
///
/// A new topology is always checked again, whatever the attempt. One sampled
/// `DISPLAY_STABLE_REQUIRED_HITS` times in a row is forwarded once its spaces are coherent and the
/// window server is `quiet`; an incoherent one starts the count over. Anything short of that checks
/// again until `DISPLAY_STABILIZE_MAX_ATTEMPTS`, then gives up.
pub fn settle(
    stability: &mut Stability,
    screens: &[ScreenInfo],
    kinds: SpaceKinds,
    quiet: impl FnOnce() -> bool,
    attempt: u8,
) -> Settle {
    let again = if attempt < DISPLAY_STABILIZE_MAX_ATTEMPTS {
        Settle::Again(attempt + 1)
    } else {
        Settle::GiveUp
    };
    if screens.is_empty() {
        return again;
    }
    let Some(hits) = stability.sample(screens) else {
        return Settle::Again(attempt.saturating_add(1));
    };
    if hits < DISPLAY_STABLE_REQUIRED_HITS {
        return again;
    }
    let spaces: Vec<Option<SpaceId>> = screens.iter().map(|screen| screen.space).collect();
    if !snapshot_spaces_are_coherent(&spaces, |space| kinds.is_fullscreen(space)) {
        stability.reset();
        return again;
    }
    if !quiet() {
        return again;
    }
    Settle::Forward
}

#[cfg(test)]
mod tests;
