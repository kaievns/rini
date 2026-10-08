//! `layout`, and the `output "UUID"` blocks that override its gaps per display.

use knus::span::Span;

use super::value::{ColourSpaceValue, ColourValue, Flag, Number, Pixels, Proportion};
use crate::layout::settings::{CenterFocusedColumn, Edges, WindowInsertionPoint};

#[derive(knus::Decode, Debug, Default)]
#[knus(span_type = Span)]
pub struct LayoutPart {
    #[knus(child, unwrap(argument))]
    pub gaps: Option<Number<Pixels>>,
    #[knus(child)]
    pub struts: Option<StrutsPart>,
    #[knus(child)]
    pub default_column_width: Option<ProportionPart>,
    #[knus(child)]
    pub min_column_width: Option<ProportionPart>,
    #[knus(child)]
    pub max_column_width: Option<ProportionPart>,
    #[knus(child, unwrap(children))]
    pub preset_column_widths: Option<Vec<PresetPart>>,
    #[knus(child, unwrap(argument))]
    pub center_focused_column: Option<CenterFocusedColumn>,
    #[knus(child, unwrap(argument))]
    pub window_insertion_point: Option<WindowInsertionPoint>,
    #[knus(child)]
    pub focus_ring: Option<BandPart>,
    #[knus(child)]
    pub border: Option<BandPart>,
}

/// niri's `focus-ring` and `border`: `on` or `off`, its width, and its colour or gradient on the
/// window holding focus and on the others.
#[derive(knus::Decode, Debug, Default)]
#[knus(span_type = Span)]
pub struct BandPart {
    #[knus(span)]
    pub span: Span,
    #[knus(child)]
    pub on: Option<Flag>,
    #[knus(child)]
    pub off: Option<Flag>,
    #[knus(child, unwrap(argument))]
    pub width: Option<Number<Pixels>>,
    #[knus(child, unwrap(argument))]
    pub active_color: Option<ColourValue>,
    #[knus(child, unwrap(argument))]
    pub inactive_color: Option<ColourValue>,
    #[knus(child)]
    pub active_gradient: Option<GradientPart>,
    #[knus(child)]
    pub inactive_gradient: Option<GradientPart>,
}

/// niri's `active-gradient from="#80c8ff" to="#bbddff" angle=45 relative-to="window" in="oklch
/// longer hue"`, `angle` 180 and `in` srgb unless written (`niri:niri-config/src/appearance.rs:86-98`).
#[derive(knus::Decode, Debug)]
#[knus(span_type = Span)]
pub struct GradientPart {
    #[knus(span)]
    pub span: Span,
    #[knus(property)]
    pub from: ColourValue,
    #[knus(property)]
    pub to: ColourValue,
    #[knus(property, default = 180)]
    pub angle: i16,
    #[knus(property)]
    pub relative_to: Option<GradientRelativeTo>,
    #[knus(property(name = "in"))]
    pub space: Option<ColourSpaceValue>,
}

/// What a gradient is laid over: its band, or niri's view of the workspace, which is refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GradientRelativeTo {
    Window,
    WorkspaceView,
}

/// niri's `struts`: replaced as a whole, so a side left out is 0.
#[derive(knus::Decode, Debug, Default, Clone, Copy)]
#[knus(span_type = Span)]
pub struct StrutsPart {
    #[knus(child, unwrap(argument))]
    pub left: Option<Number<Pixels>>,
    #[knus(child, unwrap(argument))]
    pub right: Option<Number<Pixels>>,
    #[knus(child, unwrap(argument))]
    pub top: Option<Number<Pixels>>,
    #[knus(child, unwrap(argument))]
    pub bottom: Option<Number<Pixels>>,
}

impl StrutsPart {
    pub fn edges(self) -> Edges {
        let side = |value: Option<Number<Pixels>>| value.map_or(0.0, Number::get);
        Edges {
            top: side(self.top),
            left: side(self.left),
            bottom: side(self.bottom),
            right: side(self.right),
        }
    }
}

/// `{ proportion 0.7; }`, niri's size block, with the one size kind rini has for columns.
#[derive(knus::Decode, Debug)]
#[knus(span_type = Span)]
pub struct ProportionPart {
    #[knus(span)]
    pub span: Span,
    #[knus(child, unwrap(argument))]
    pub proportion: Option<Number<Proportion>>,
}

#[derive(knus::Decode, Debug)]
#[knus(span_type = Span)]
pub enum PresetPart {
    Proportion(#[knus(argument)] Number<Proportion>),
}

#[derive(knus::Decode, Debug)]
#[knus(span_type = Span)]
pub struct OutputPart {
    #[knus(span)]
    pub span: Span,
    /// The display's UUID, as `rini-cli query displays` prints it.
    #[knus(argument)]
    pub uuid: String,
    #[knus(child)]
    pub layout: Option<OutputLayoutPart>,
}

#[derive(knus::Decode, Debug, Default)]
#[knus(span_type = Span)]
pub struct OutputLayoutPart {
    #[knus(child, unwrap(argument))]
    pub gaps: Option<Number<Pixels>>,
    #[knus(child)]
    pub struts: Option<StrutsPart>,
}
