//! How a look moves a flight: what it composes frame zero and the motion from (`Stage`), what bends a
//! flight already moving (`Pass`), and what a bend gives back (`Retarget`). A look answers with the
//! score's edits (`motion::score::Edit`), which the renderer plays as they are; `slide` is the
//! default look. See "The overlay engine" in `src/animation/docs/animation-smoothness.md`.

pub mod slide;

use objc2_core_foundation::{CGPoint, CGRect};
use rini_core::ids::WindowId;

use super::motion::accents::Accent;
use super::motion::plan::{FlightPlan, PlanDelta, ReflowPlan};
use super::motion::score::Edit;

/// A flight as a look composes it, at frame zero or as its motion starts.
#[derive(Clone, Copy, Debug)]
pub struct Stage<'a> {
    pub plan: &'a FlightPlan,
    /// The windows the flight draws a tile of.
    pub drawn: &'a [WindowId],
    /// How long the motion runs, plain: a look stretches it for an accent that asks it to.
    pub seconds: f64,
    /// What waited for the motion to start: at most one of each kind.
    pub accents: &'a [Accent],
}

/// A later pass, bending a flight that moves.
#[derive(Clone, Copy, Debug)]
pub struct Pass<'a> {
    pub plan: &'a ReflowPlan,
    /// The strip's travel, for a strip movement.
    pub pan: Option<CGPoint>,
    /// The window gaining focus, which wins a tied vote.
    pub focus: Option<WindowId>,
    /// The display in the overlay's space: what a member's destination is judged against.
    pub viewport: CGRect,
    /// The windows the flight draws a tile of once the pass is in.
    pub drawn: &'a [WindowId],
    /// How long what the pass moves runs, plain.
    pub seconds: f64,
    pub accent: Option<Accent>,
}

/// A pass folded into a moving flight: the merged plan, what the merge changed, and the edits that
/// carry it out on the score.
#[derive(Clone, Debug)]
pub struct Retarget {
    pub plan: FlightPlan,
    pub delta: PlanDelta,
    pub edits: Vec<Edit>,
}
