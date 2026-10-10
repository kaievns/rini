//! The value shapes the sections share: niri's tri-state flag, a number checked against its range
//! where it is written, and the string enums.

use std::marker::PhantomData;

use knus::ast::{Literal, TypeName};
use knus::decode::Context;
use knus::errors::DecodeError;
use knus::span::Spanned;
use knus::traits::ErrorSpan;

use super::layout::GradientRelativeTo;
use crate::decorations::domain::paint::{Colour, ColourSpace, Hue};
use crate::displays::platform::cursor_warp::StackedUpperSide;
use crate::input::domain::binding::Decimal;
use crate::layout::settings::{CenterFocusedColumn, WindowInsertionPoint};
use crate::windows::domain::rules::RelativeTo;
use crate::workspaces::domain::display_pin::DisplayRole;

/// A flag node: written bare it is on, and `true` or `false` says so explicitly.
#[derive(knus::Decode, Debug, Clone, Copy, PartialEq, Eq)]
pub struct Flag(#[knus(argument, default = true)] pub bool);

/// Where a number may lie. `ABOVE_MIN` excludes the minimum itself.
pub trait Bounds {
    const MIN: f64;
    const MAX: f64;
    const ABOVE_MIN: bool = false;
}

/// A number, written as an integer or a decimal, refused where it is written when it is out of range.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Number<B>(pub f64, PhantomData<B>);

impl<B> Number<B> {
    pub fn get(self) -> f64 { self.0 }
}

fn range_text<B: Bounds>() -> String {
    let low = if B::ABOVE_MIN { "above" } else { "from" };
    format!("a number {low} {} to {}", B::MIN, B::MAX)
}

impl<S: ErrorSpan, B: Bounds> knus::DecodeScalar<S> for Number<B> {
    fn type_check(type_name: &Option<Spanned<TypeName, S>>, ctx: &mut Context<S>) {
        Decimal::type_check(type_name, ctx);
    }

    fn raw_decode(
        value: &Spanned<Literal, S>,
        ctx: &mut Context<S>,
    ) -> Result<Self, DecodeError<S>> {
        let expected = || DecodeError::conversion(value, format!("expected {}", range_text::<B>()));
        if !matches!(&**value, Literal::Int(_) | Literal::Decimal(_)) {
            return Err(expected());
        }
        let Decimal(number) = Decimal::raw_decode(value, ctx)?;
        let low_ok = if B::ABOVE_MIN {
            number > B::MIN
        } else {
            number >= B::MIN
        };
        if !(low_ok && number <= B::MAX) {
            return Err(expected());
        }
        Ok(Number(number, PhantomData))
    }
}

/// A colour as niri writes one, a string any CSS colour form `csscolorparser` reads: a name,
/// `#rgb[a]`, `#rrggbb[aa]`, `rgb()`, `rgba()`, `hsl()`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ColourValue(pub Colour);

impl<S: ErrorSpan> knus::DecodeScalar<S> for ColourValue {
    fn type_check(type_name: &Option<Spanned<TypeName, S>>, ctx: &mut Context<S>) {
        Decimal::type_check(type_name, ctx);
    }

    fn raw_decode(value: &Spanned<Literal, S>, _: &mut Context<S>) -> Result<Self, DecodeError<S>> {
        let Literal::String(text) = &**value else {
            return Err(DecodeError::conversion(value, "expected a colour, such as \"#7fc8ff\""));
        };
        let colour = csscolorparser::parse(text)
            .map_err(|error| DecodeError::conversion(value, format!("expected a colour: {error}")))?;
        let [r, g, b, a] = colour.to_array();
        Ok(ColourValue(Colour { r, g, b, a }))
    }
}

/// A gradient's `in`, as niri writes it: `srgb`, `srgb-linear`, `oklab`, or `oklch` with
/// optionally the way round its hue goes, as `"oklch longer hue"` (`niri:niri-config/src/appearance.rs:725-780`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ColourSpaceValue(pub ColourSpace);

impl<S: ErrorSpan> knus::DecodeScalar<S> for ColourSpaceValue {
    fn type_check(type_name: &Option<Spanned<TypeName, S>>, ctx: &mut Context<S>) {
        Decimal::type_check(type_name, ctx);
    }

    fn raw_decode(value: &Spanned<Literal, S>, _: &mut Context<S>) -> Result<Self, DecodeError<S>> {
        let Literal::String(text) = &**value else {
            return Err(DecodeError::conversion(value, "expected a colour space, such as \"oklch longer hue\""));
        };
        colour_space(text).map(ColourSpaceValue).map_err(|why| DecodeError::conversion(value, why))
    }
}

fn colour_space(text: &str) -> Result<ColourSpace, String> {
    let mut words = text.split_whitespace();
    let space = match words.next() {
        None => return Err("missing color space".to_owned()),
        Some("srgb") => ColourSpace::Srgb,
        Some("srgb-linear") => ColourSpace::SrgbLinear,
        Some("oklab") => ColourSpace::Oklab,
        Some("oklch") => ColourSpace::Oklch(Hue::Shorter),
        Some(other) => return Err(format!("invalid color space {other}; can be srgb, srgb-linear, oklab or oklch")),
    };
    let Some(way) = words.next() else { return Ok(space) };
    if space != ColourSpace::Oklch(Hue::Shorter) {
        return Err("only oklch color space can have hue interpolation".to_owned());
    }
    if words.next() != Some("hue") {
        return Err("interpolation must end with \"hue\", like \"oklch shorter hue\"".to_owned());
    }
    if words.next().is_some() {
        return Err("unexpected text after hue interpolation".to_owned());
    }
    let hue = match way {
        "shorter" => Hue::Shorter,
        "longer" => Hue::Longer,
        "increasing" => Hue::Increasing,
        "decreasing" => Hue::Decreasing,
        other => return Err(format!("invalid hue interpolation {other}; can be shorter, longer, increasing, decreasing")),
    };
    Ok(ColourSpace::Oklch(hue))
}

macro_rules! bounds {
    ($($name:ident: $min:expr, $max:expr $(, above = $above:expr)?;)*) => {
        $(
            #[derive(Debug, Clone, Copy, PartialEq)]
            pub struct $name;
            impl Bounds for $name {
                const MIN: f64 = $min;
                const MAX: f64 = $max;
                $(const ABOVE_MIN: bool = $above;)?
            }
        )*
    };
}

bounds! {
    Pixels: 0.0, 65535.0;
    Coordinate: -65535.0, 65535.0;
    Size: 0.0, 65535.0, above = true;
    Proportion: 0.0, 1.0, above = true;
    Fraction: 0.0, 1.0;
    Slowdown: 0.0, 100.0;
}

/// Decodes a string enum the config spells in kebab-case into a type that lives with its feature.
/// Values listed under `not_yet` are niri's and refused by name.
macro_rules! kebab_scalar {
    ($ty:ty { $($text:literal => $value:expr),* $(,)? } $(not_yet { $($later:literal),* })?) => {
        impl<S: knus::traits::ErrorSpan> knus::DecodeScalar<S> for $ty {
            fn type_check(
                type_name: &Option<knus::span::Spanned<knus::ast::TypeName, S>>,
                ctx: &mut knus::decode::Context<S>,
            ) {
                crate::input::domain::binding::Decimal::type_check(type_name, ctx);
            }

            fn raw_decode(
                value: &knus::span::Spanned<knus::ast::Literal, S>,
                _: &mut knus::decode::Context<S>,
            ) -> Result<Self, knus::errors::DecodeError<S>> {
                let expected = [$(concat!("`", $text, "`")),*].join(", ");
                let knus::ast::Literal::String(text) = &**value else {
                    return Err(knus::errors::DecodeError::conversion(
                        value,
                        format!("expected one of {expected}"),
                    ));
                };
                match &**text {
                    $($text => Ok($value),)*
                    $($($later => Err(knus::errors::DecodeError::conversion(
                        value,
                        format!("`{}` is niri's and not supported yet; expected one of {expected}", $later),
                    )),)*)?
                    other => Err(knus::errors::DecodeError::conversion(
                        value,
                        format!("expected one of {expected}, found `{other}`"),
                    )),
                }
            }
        }
    };
}
kebab_scalar!(StackedUpperSide { "left" => StackedUpperSide::Left, "right" => StackedUpperSide::Right });
kebab_scalar!(CenterFocusedColumn {
    "never" => CenterFocusedColumn::Never,
    "always" => CenterFocusedColumn::Always,
} not_yet { "on-overflow" });
kebab_scalar!(WindowInsertionPoint {
    "next-to-selection" => WindowInsertionPoint::NextToSelection,
    "end-of-tree" => WindowInsertionPoint::EndOfTree,
});
kebab_scalar!(DisplayRole { "internal" => DisplayRole::Internal, "external" => DisplayRole::External });
kebab_scalar!(GradientRelativeTo {
    "window" => GradientRelativeTo::Window,
    "workspace-view" => GradientRelativeTo::WorkspaceView,
});
kebab_scalar!(RelativeTo {
    "top-left" => RelativeTo::TopLeft,
    "top-right" => RelativeTo::TopRight,
    "bottom-left" => RelativeTo::BottomLeft,
    "bottom-right" => RelativeTo::BottomRight,
    "top" => RelativeTo::Top,
    "bottom" => RelativeTo::Bottom,
    "left" => RelativeTo::Left,
    "right" => RelativeTo::Right,
});
