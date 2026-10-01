//! `bar` and `recent-windows`, the two things rini draws besides the windows.

use knus::span::Span;

use super::binds::BindsPart;
use super::value::Flag;

#[derive(knus::Decode, Debug, Default)]
#[knus(span_type = Span)]
pub struct BarPart {
    #[knus(child)]
    pub off: Option<Flag>,
}

/// niri's switcher section.
#[derive(knus::Decode, Debug, Default)]
#[knus(span_type = Span)]
pub struct RecentWindowsPart {
    #[knus(child, unwrap(children))]
    pub icons: Option<Vec<IconPart>>,
    #[knus(child)]
    pub binds: Option<BindsPart>,
}

/// `"bundle.id" "~/path.png"`: niri's `environment` shape, the node name is the key.
#[derive(knus::Decode, Debug)]
#[knus(span_type = Span)]
pub struct IconPart {
    #[knus(node_name)]
    pub bundle_id: String,
    #[knus(argument)]
    pub path: String,
}
