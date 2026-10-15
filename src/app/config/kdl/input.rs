//! `input` and `cursor`: focus, the pointer, and the modifier `Mod` stands for.

use knus::span::Span;

use super::value::{Flag, Fraction, Number};
use crate::displays::platform::cursor_warp::StackedUpperSide;
use crate::input::domain::key::ModKey;

#[derive(knus::Decode, Debug, Default)]
#[knus(span_type = Span)]
pub struct InputPart {
    #[knus(child, unwrap(argument, str))]
    pub mod_key: Option<ModKey>,
    #[knus(child)]
    pub focus_follows_mouse: Option<Flag>,
    #[knus(child)]
    pub warp_mouse_to_focus: Option<Flag>,
    #[knus(child)]
    pub workspace_auto_back_and_forth: Option<Flag>,
    #[knus(child)]
    pub warp_mouse_across_stacked_outputs: Option<StackedWarpPart>,
    #[knus(children(name = "ignore-app-activation"), unwrap(argument))]
    pub ignore_app_activation: Vec<String>,
}

#[derive(knus::Decode, Debug)]
#[knus(span_type = Span)]
pub struct StackedWarpPart {
    #[knus(argument, default = true)]
    pub on: bool,
    #[knus(property)]
    pub upper_side: Option<StackedUpperSide>,
    #[knus(property)]
    pub lower_top_at: Option<Number<Fraction>>,
}

#[derive(knus::Decode, Debug, Default)]
#[knus(span_type = Span)]
pub struct CursorPart {
    #[knus(child)]
    pub hide_on_focus_change: Option<Flag>,
}
