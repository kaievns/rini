//! The file format: KDL v1, decoded with knus into one part type per section, niri's shape, every
//! field optional, and then laid over a `Config` by `apply`. Only this module knows the format.

pub mod apply;
pub mod bar;
pub mod binds;
pub mod error;
pub mod gestures;
pub mod input;
pub mod layout;
pub mod suggest;
pub mod value;
pub mod window_rule;

use knus::ast::SpannedNode;
use knus::errors::DecodeError;
use knus::span::Span;
use knus::traits::Decode;

use self::bar::{BarPart, ConfigNotificationPart, RecentWindowsPart};
use self::binds::BindsPart;
use self::error::{ConfigError, Problem};
use self::gestures::{AnimationsPart, GesturesPart};
use self::input::{CursorPart, InputPart};
use self::layout::{LayoutPart, OutputPart};
use self::value::Flag;
use self::window_rule::{SpawnPart, SpawnShPart, WindowRulePart, WorkspacePart};
use crate::app::startup::StartupCommand;

/// The `kind` a bind's decoder reports an action name rini does not have under. Its message is the
/// name alone, so the report can say what was probably meant.
///
/// knus reports a node it does not know where the node starts: as an unexpected node in a struct,
/// or as a failed conversion in an enum. The schema walk reports the same nodes with a suggestion,
/// so knus's report of a node the walk has reported is dropped.
pub const UNKNOWN_ACTION: &str = "action";

/// One config text, decoded. Every section is optional, and a singleton written twice is an error.
#[derive(Debug, Default)]
pub struct ConfigPart {
    pub input: Option<InputPart>,
    pub cursor: Option<CursorPart>,
    pub layout: Option<LayoutPart>,
    pub animations: Option<AnimationsPart>,
    pub gestures: Option<GesturesPart>,
    pub recent_windows: Option<RecentWindowsPart>,
    pub bar: Option<BarPart>,
    pub config_notification: Option<ConfigNotificationPart>,
    pub binds: Option<BindsPart>,
    pub activate_spaces_by_default: Option<Flag>,
    pub reapply_window_rules_on_title_change: Option<Flag>,
    /// The repeatable nodes, each in file order.
    pub outputs: Vec<OutputPart>,
    pub workspaces: Vec<WorkspacePart>,
    pub window_rules: Vec<WindowRulePart>,
    pub startup: Vec<StartupCommand>,
}

/// Decodes `text`, reporting every problem in it at once. `name` is what the report calls it.
pub fn parse(name: &str, text: &str) -> Result<ConfigPart, ConfigError> {
    let document = knus::parse_ast::<Span>(name, text).map_err(|error| {
        let problems = miette::Diagnostic::related(&error)
            .map(|related| related.map(Problem::from_diagnostic).collect())
            .unwrap_or_default();
        ConfigError::new(name, text, problems)
    })?;

    let unknown = suggest::unknown_nodes(&document);
    let mut decoding = Decoding::default();
    let mut part = ConfigPart::default();
    for node in &document.nodes {
        let d = &mut decoding;
        match &**node.node_name {
            "input" => d.once(&mut part.input, node),
            "cursor" => d.once(&mut part.cursor, node),
            "layout" => d.once(&mut part.layout, node),
            "animations" => d.once(&mut part.animations, node),
            "gestures" => d.once(&mut part.gestures, node),
            "recent-windows" => d.once(&mut part.recent_windows, node),
            "bar" => d.once(&mut part.bar, node),
            "config-notification" => d.once(&mut part.config_notification, node),
            "binds" => d.once(&mut part.binds, node),
            "activate-spaces-by-default" => d.once(&mut part.activate_spaces_by_default, node),
            "reapply-window-rules-on-title-change" => {
                d.once(&mut part.reapply_window_rules_on_title_change, node)
            }
            "output" => part.outputs.extend(d.decode(node)),
            "workspace" => part.workspaces.extend(d.decode(node)),
            "window-rule" => part.window_rules.extend(d.decode(node)),
            "spawn-at-startup" => {
                if let Some(SpawnPart { span, argv }) = d.decode(node) {
                    if argv.is_empty() {
                        d.problems.push(Problem::at(
                            &span,
                            "`spawn-at-startup` needs a program",
                            "nothing to run",
                        ));
                    } else {
                        part.startup.push(StartupCommand::Spawn(argv));
                    }
                }
            }
            "spawn-sh-at-startup" => {
                if let Some(SpawnShPart { command }) = d.decode(node) {
                    part.startup.push(StartupCommand::SpawnSh(command));
                }
            }
            _ => {}
        }
    }

    let reported: Vec<usize> = unknown.iter().map(|(offset, _)| *offset).collect();
    let mut problems: Vec<Problem> = unknown.into_iter().map(|(_, problem)| problem).collect();
    problems.extend(decoding.problems);
    problems.extend(decoding.errors.into_iter().filter_map(|error| match error {
        DecodeError::Unexpected { span, kind: "node", .. }
        | DecodeError::Conversion { span, .. }
            if reported.contains(&span.0) =>
        {
            None
        }
        DecodeError::Unexpected {
            span,
            kind: UNKNOWN_ACTION,
            message,
        } => Some(suggest::unknown_action(&message, &span)),
        other => Some(Problem::from(other)),
    }));
    problems.sort_by_key(|problem| problem.offset().unwrap_or(usize::MAX));
    if problems.is_empty() {
        Ok(part)
    } else {
        Err(ConfigError::new(name, text, problems))
    }
}

#[derive(Default)]
struct Decoding<'a> {
    errors: Vec<DecodeError<Span>>,
    problems: Vec<Problem>,
    first: Vec<(&'a str, Span)>,
}

impl<'a> Decoding<'a> {
    fn decode<T: Decode<Span>>(&mut self, node: &SpannedNode<Span>) -> Option<T> {
        knus::decode::node::<T, Span>(node)
            .map_err(|errors| self.errors.extend(errors))
            .ok()
    }

    /// A section that may be written once.
    fn once<T: Decode<Span>>(&mut self, slot: &mut Option<T>, node: &'a SpannedNode<Span>) {
        let name = &**node.node_name;
        let here = *node.node_name.span();
        if let Some((_, first)) = self.first.iter().find(|(seen, _)| *seen == name) {
            self.problems.push(
                Problem::at(&here, format!("`{name}` is written twice"), "written again here")
                    .also(first, "first written here")
                    .help("a section is written once; put everything in the first one"),
            );
            return;
        }
        self.first.push((name, here));
        *slot = self.decode(node);
    }
}
