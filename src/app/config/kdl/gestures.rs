//! `gestures` and `animations`: the two trackpad swipes, dragging one window onto another, how the
//! movements are drawn, and whether their journal is written and each lift's handover measured.

use knus::span::Span;

use super::value::{
    Columns, Fingers, Flag, Haptics, Number, Proportion, Slowdown, Tolerance, TriggerDistance,
};

#[derive(knus::Decode, Debug, Default)]
#[knus(span_type = Span)]
pub struct GesturesPart {
    #[knus(child)]
    pub swipe_workspace_switch: Option<SwipeWorkspaceSwitchPart>,
    #[knus(child)]
    pub swipe_view_scroll: Option<SwipeViewScrollPart>,
    #[knus(child)]
    pub pass_through: Option<Flag>,
    #[knus(child)]
    pub visit_empty_workspaces: Option<Flag>,
    #[knus(child, unwrap(argument))]
    pub drag_swap_overlap: Option<Number<Proportion>>,
}

#[derive(knus::Decode, Debug, Default)]
#[knus(span_type = Span)]
pub struct SwipeWorkspaceSwitchPart {
    #[knus(span)]
    pub span: Span,
    #[knus(child)]
    pub on: Option<Flag>,
    #[knus(child)]
    pub off: Option<Flag>,
    #[knus(child, unwrap(argument))]
    pub fingers: Option<Number<Fingers>>,
    #[knus(child, unwrap(argument))]
    pub trigger_distance: Option<Number<TriggerDistance>>,
    #[knus(child, unwrap(argument))]
    pub vertical_tolerance: Option<Number<Tolerance>>,
    #[knus(child)]
    pub invert: Option<Flag>,
    #[knus(child, unwrap(argument))]
    pub haptics: Option<Haptics>,
}

#[derive(knus::Decode, Debug, Default)]
#[knus(span_type = Span)]
pub struct SwipeViewScrollPart {
    #[knus(span)]
    pub span: Span,
    #[knus(child)]
    pub on: Option<Flag>,
    #[knus(child)]
    pub off: Option<Flag>,
    #[knus(child, unwrap(argument))]
    pub fingers: Option<Number<Fingers>>,
    #[knus(child, unwrap(argument))]
    pub trigger_distance: Option<Number<TriggerDistance>>,
    #[knus(child, unwrap(argument))]
    pub vertical_tolerance: Option<Number<Tolerance>>,
    #[knus(child)]
    pub invert: Option<Flag>,
    #[knus(child)]
    pub switch_workspace_on_overscroll: Option<OverscrollPart>,
}

#[derive(knus::Decode, Debug)]
#[knus(span_type = Span)]
pub struct OverscrollPart {
    #[knus(argument, default = true)]
    pub on: bool,
    /// Columns past the end of the strip.
    #[knus(property)]
    pub threshold: Option<Number<Columns>>,
}

#[derive(knus::Decode, Debug, Default)]
#[knus(span_type = Span)]
pub struct AnimationsPart {
    #[knus(child)]
    pub off: Option<Flag>,
    #[knus(child, unwrap(argument))]
    pub slowdown: Option<Number<Slowdown>>,
    #[knus(child)]
    pub journal: Option<JournalPart>,
}

/// `animations › journal`: the record of every transition, written once the node is written and not
/// `false`, and each lift measured with `measure-handover`.
#[derive(knus::Decode, Debug)]
#[knus(span_type = Span)]
pub struct JournalPart {
    #[knus(argument, default = true)]
    pub on: bool,
    #[knus(child)]
    pub measure_handover: Option<Flag>,
}
