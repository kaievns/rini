//! The value shapes the sections share: niri's tri-state flag, a number checked against its range
//! where it is written, and the string enums.

use std::marker::PhantomData;

use knus::ast::{Literal, TypeName};
use knus::decode::Context;
use knus::errors::DecodeError;
use knus::span::Spanned;
use knus::traits::ErrorSpan;

use crate::displays::platform::cursor_warp::StackedUpperSide;
use crate::input::domain::binding::Decimal;
use crate::input::platform::haptics::HapticPattern;
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
    const INTEGER: bool = false;
}

/// A number, written as an integer or a decimal, refused where it is written when it is out of range.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Number<B>(pub f64, PhantomData<B>);

impl<B> Number<B> {
    pub fn get(self) -> f64 { self.0 }
}

fn range_text<B: Bounds>() -> String {
    let what = if B::INTEGER {
        "a whole number"
    } else {
        "a number"
    };
    let low = if B::ABOVE_MIN { "above" } else { "from" };
    format!("{what} {low} {} to {}", B::MIN, B::MAX)
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
        if B::INTEGER && !matches!(&**value, Literal::Int(_)) {
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

macro_rules! bounds {
    ($($name:ident: $min:expr, $max:expr $(, above = $above:expr)? $(, integer = $integer:expr)?;)*) => {
        $(
            #[derive(Debug, Clone, Copy, PartialEq)]
            pub struct $name;
            impl Bounds for $name {
                const MIN: f64 = $min;
                const MAX: f64 = $max;
                $(const ABOVE_MIN: bool = $above;)?
                $(const INTEGER: bool = $integer;)?
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
    TriggerDistance: 0.01, 1.0;
    Tolerance: 0.0, 100.0;
    Slowdown: 0.0, 100.0;
    Columns: 0.0, 100.0;
    Fingers: 1.0, 5.0, integer = true;
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

/// `haptics`: a pattern, or none.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Haptics(pub Option<HapticPattern>);

kebab_scalar!(Haptics {
    "level-change" => Haptics(Some(HapticPattern::LevelChange)),
    "alignment" => Haptics(Some(HapticPattern::Alignment)),
    "generic" => Haptics(Some(HapticPattern::Generic)),
    "off" => Haptics(None),
});
