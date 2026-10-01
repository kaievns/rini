//! `window-rule`, `workspace "name"`, and the top-level startup commands.

use knus::span::Span;

use super::value::{Coordinate, Flag, Number, Size};
use crate::windows::domain::rules::{RegexEq, RelativeTo};
use crate::workspaces::domain::display_pin::DisplayRole;

#[derive(knus::Decode, Debug)]
#[knus(span_type = Span)]
pub struct WindowRulePart {
    #[knus(span)]
    pub span: Span,
    #[knus(children(name = "match"))]
    pub matches: Vec<MatchPart>,
    #[knus(children(name = "exclude"))]
    pub excludes: Vec<MatchPart>,
    #[knus(child)]
    pub open_on_workspace: Option<NamePart>,
    #[knus(child, unwrap(argument))]
    pub open_floating: Option<bool>,
    #[knus(child)]
    pub default_floating_position: Option<FloatingPositionPart>,
    #[knus(child)]
    pub default_column_width: Option<FixedPart>,
    #[knus(child)]
    pub default_window_height: Option<FixedPart>,
    #[knus(child)]
    pub open_focused: Option<BoolPart>,
    #[knus(child, unwrap(argument))]
    pub manage: Option<bool>,
    #[knus(child, unwrap(argument))]
    pub open_on_output: Option<DisplayRole>,
}

#[derive(knus::Decode, Debug)]
#[knus(span_type = Span)]
pub struct MatchPart {
    #[knus(span)]
    pub span: Span,
    #[knus(property, str)]
    pub app_id: Option<RegexEq>,
    #[knus(property, str)]
    pub title: Option<RegexEq>,
    #[knus(property, str)]
    pub app_name: Option<RegexEq>,
    #[knus(property)]
    pub ax_role: Option<String>,
    #[knus(property)]
    pub ax_subrole: Option<String>,
    #[knus(property)]
    pub is_modal: Option<bool>,
}

#[derive(knus::Decode, Debug)]
#[knus(span_type = Span)]
pub struct NamePart {
    #[knus(span)]
    pub span: Span,
    #[knus(argument)]
    pub name: String,
}

#[derive(knus::Decode, Debug)]
#[knus(span_type = Span)]
pub struct BoolPart {
    #[knus(span)]
    pub span: Span,
    #[knus(argument)]
    pub value: bool,
}

#[derive(knus::Decode, Debug)]
#[knus(span_type = Span)]
pub struct FloatingPositionPart {
    #[knus(property)]
    pub x: Number<Coordinate>,
    #[knus(property)]
    pub y: Number<Coordinate>,
    #[knus(property)]
    pub relative_to: Option<RelativeTo>,
}

/// `{ fixed 640; }`, niri's size block, with the one size kind rini has for a rule.
#[derive(knus::Decode, Debug)]
#[knus(span_type = Span)]
pub struct FixedPart {
    #[knus(span)]
    pub span: Span,
    #[knus(child, unwrap(argument))]
    pub fixed: Option<Number<Size>>,
}

#[derive(knus::Decode, Debug)]
#[knus(span_type = Span)]
pub struct WorkspacePart {
    #[knus(span)]
    pub span: Span,
    #[knus(argument)]
    pub name: String,
    #[knus(child)]
    pub focus_at_startup: Option<Flag>,
}

/// `spawn-at-startup "program" "arg"`.
#[derive(knus::Decode, Debug)]
#[knus(span_type = Span)]
pub struct SpawnPart {
    #[knus(span)]
    pub span: Span,
    #[knus(arguments)]
    pub argv: Vec<String>,
}

/// `spawn-sh-at-startup "command line"`.
#[derive(knus::Decode, Debug)]
#[knus(span_type = Span)]
pub struct SpawnShPart {
    #[knus(argument)]
    pub command: String,
}
