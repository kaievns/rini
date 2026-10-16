//! `gestures` and `animations`: dragging one window onto another, how the movements are drawn and in
//! which look, and whether their journal is written and each lift's handover measured.

use knus::span::Span;

use super::value::{Flag, Number, Proportion, Slowdown};
use crate::animation::domain::choreography::Look;

#[derive(knus::Decode, Debug, Default)]
#[knus(span_type = Span)]
pub struct GesturesPart {
    #[knus(child, unwrap(argument))]
    pub drag_swap_overlap: Option<Number<Proportion>>,
}

#[derive(knus::Decode, Debug, Default)]
#[knus(span_type = Span)]
pub struct AnimationsPart {
    #[knus(child)]
    pub off: Option<Flag>,
    #[knus(child, unwrap(argument))]
    pub slowdown: Option<Number<Slowdown>>,
    #[knus(child, unwrap(argument))]
    pub look: Option<Look>,
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
