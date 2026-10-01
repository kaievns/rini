//! `binds` and `recent-windows › binds`: the key is the node name, its properties are options, and
//! its one child is the action.

use knus::ast::SpannedNode;
use knus::decode::Context;
use knus::errors::DecodeError;
use knus::span::Span;
use knus::traits::Decode;
use strum::VariantNames;

use crate::input::domain::binding::Action;

/// A bind block. knus keeps no value of a node with an error anywhere in it, so the block carries
/// its binds' errors itself, and one bad bind leaves the others for the checks across binds.
#[derive(Debug, Default)]
pub struct BindsPart {
    pub binds: Vec<BindPart>,
    pub errors: Vec<DecodeError<Span>>,
}

#[derive(Debug)]
pub struct BindPart {
    pub span: Span,
    /// The node name as written.
    pub key: String,
    pub key_span: Span,
    /// `None` when the action failed to decode, which is in the block's errors.
    pub action: Option<Action>,
    pub repeat: bool,
}

/// Bind properties niri has and rini refuses, with why.
const REFUSED_PROPERTIES: [(&str, &str); 4] = [
    ("cooldown-ms", "`cooldown-ms` is not supported yet"),
    (
        "hotkey-overlay-title",
        "rini has no hotkey overlay, so `hotkey-overlay-title` has nothing to title",
    ),
    (
        "allow-when-locked",
        "rini sees no keys at the lock screen, so `allow-when-locked` cannot be honoured",
    ),
    (
        "allow-inhibiting",
        "`allow-inhibiting` is Wayland's shortcut-inhibit protocol, which macOS does not have",
    ),
];

impl Decode<Span> for BindsPart {
    fn decode_node(
        node: &SpannedNode<Span>,
        _: &mut Context<Span>,
    ) -> Result<Self, DecodeError<Span>> {
        let mut errors = Vec::new();
        for argument in &node.arguments {
            errors.push(DecodeError::unexpected(
                &argument.literal,
                "argument",
                "`binds` takes no arguments",
            ));
        }
        for name in node.properties.keys() {
            errors.push(DecodeError::unexpected(
                name,
                "property",
                "`binds` takes no properties",
            ));
        }
        let binds = node.children().map(|child| BindPart::decode(child, &mut errors)).collect();
        Ok(BindsPart { binds, errors })
    }
}

impl BindPart {
    /// One bind, as much of it as decodes; what does not is added to `errors`.
    fn decode(node: &SpannedNode<Span>, errors: &mut Vec<DecodeError<Span>>) -> BindPart {
        let repeat = match knus::decode::node::<KeyOptions, Span>(node) {
            Ok(KeyOptions { repeat }) => repeat,
            Err(found) => {
                errors.extend(found);
                false
            }
        };
        let mut bind = BindPart {
            span: *node.span(),
            key: node.node_name.to_string(),
            key_span: *node.node_name.span(),
            action: None,
            repeat,
        };
        let mut children = node.children();
        let Some(child) = children.next() else {
            errors.push(DecodeError::missing(
                node,
                "expected an action for this key, in braces",
            ));
            return bind;
        };
        for extra in children {
            errors.push(DecodeError::unexpected(extra, "node", "a key takes one action"));
        }
        let name = &**child.node_name;
        if !Action::VARIANTS.contains(&name) {
            errors.push(DecodeError::unexpected(
                &child.node_name,
                super::UNKNOWN_ACTION,
                name,
            ));
            return bind;
        }
        match knus::decode::node::<Action, Span>(child) {
            Ok(action) => bind.action = Some(action),
            Err(found) => errors.extend(found),
        }
        bind
    }
}

/// What a key's node says besides its action: no type name, no arguments, and `repeat=`.
struct KeyOptions {
    repeat: bool,
}

impl Decode<Span> for KeyOptions {
    fn decode_node(
        node: &SpannedNode<Span>,
        ctx: &mut Context<Span>,
    ) -> Result<Self, DecodeError<Span>> {
        if let Some(type_name) = &node.type_name {
            ctx.emit_error(DecodeError::unexpected(
                type_name,
                "type name",
                "no type name expected for a key",
            ));
        }
        for argument in &node.arguments {
            ctx.emit_error(DecodeError::unexpected(
                &argument.literal,
                "argument",
                "a key takes no arguments; its action goes in braces: `Mod+J { focus-column-left; }`",
            ));
        }
        let mut repeat = false;
        for (name, value) in &node.properties {
            match &***name {
                "repeat" => repeat = knus::DecodeScalar::decode(value, ctx)?,
                other => {
                    let message = REFUSED_PROPERTIES
                        .iter()
                        .find(|(refused, _)| *refused == other)
                        .map(|(_, why)| why.to_string())
                        .unwrap_or_else(|| {
                            format!("unexpected property `{other}`; a key takes `repeat=`")
                        });
                    ctx.emit_error(DecodeError::unexpected(name, "property", message));
                }
            }
        }
        Ok(KeyOptions { repeat })
    }
}
