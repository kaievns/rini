//! `binds` and `recent-windows › binds`: the key is the node name, its properties are options, and
//! its one child is the action.

use knus::ast::SpannedNode;
use knus::decode::Context;
use knus::errors::DecodeError;
use knus::span::Span;
use knus::traits::Decode;
use strum::VariantNames;

use crate::input::domain::binding::Action;

#[derive(Debug, Default)]
pub struct BindsPart {
    pub binds: Vec<BindPart>,
}

#[derive(Debug)]
pub struct BindPart {
    pub span: Span,
    /// The node name as written.
    pub key: String,
    pub key_span: Span,
    /// `None` when the action failed to decode, which has been reported already.
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
        ctx: &mut Context<Span>,
    ) -> Result<Self, DecodeError<Span>> {
        for argument in &node.arguments {
            ctx.emit_error(DecodeError::unexpected(
                &argument.literal,
                "argument",
                "`binds` takes no arguments",
            ));
        }
        for name in node.properties.keys() {
            ctx.emit_error(DecodeError::unexpected(
                name,
                "property",
                "`binds` takes no properties",
            ));
        }
        let mut binds = Vec::new();
        for child in node.children() {
            match BindPart::decode_node(child, ctx) {
                Ok(bind) => binds.push(bind),
                Err(error) => ctx.emit_error(error),
            }
        }
        Ok(BindsPart { binds })
    }
}

impl Decode<Span> for BindPart {
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

        let mut bind = BindPart {
            span: *node.span(),
            key: node.node_name.to_string(),
            key_span: *node.node_name.span(),
            action: None,
            repeat,
        };
        let mut children = node.children();
        let Some(child) = children.next() else {
            ctx.emit_error(DecodeError::missing(
                node,
                "expected an action for this key, in braces",
            ));
            return Ok(bind);
        };
        for extra in children {
            ctx.emit_error(DecodeError::unexpected(extra, "node", "a key takes one action"));
        }
        let name = &**child.node_name;
        if !Action::VARIANTS.contains(&name) {
            ctx.emit_error(DecodeError::unexpected(
                &child.node_name,
                super::UNKNOWN_ACTION,
                name,
            ));
            return Ok(bind);
        }
        match Action::decode_node(child, ctx) {
            Ok(action) => bind.action = Some(action),
            Err(error) => ctx.emit_error(error),
        }
        Ok(bind)
    }
}
