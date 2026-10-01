//! Everything wrong with one config text, as one report.

use std::fmt;

use knus::errors::DecodeError;
use knus::span::Span;
use miette::{Diagnostic, GraphicalReportHandler, GraphicalTheme, LabeledSpan, NamedSource};

/// One thing wrong, with the places it is about.
#[derive(Debug, thiserror::Error, Diagnostic)]
#[error("{message}")]
pub struct Problem {
    message: String,
    #[label(collection)]
    labels: Vec<LabeledSpan>,
    #[help]
    help: Option<String>,
}

impl Problem {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            labels: Vec::new(),
            help: None,
        }
    }

    /// A problem about the text at `span`, labelled.
    pub fn at(span: &Span, message: impl Into<String>, label: impl Into<String>) -> Self {
        Self::new(message).also(span, label)
    }

    /// Another place this problem is about.
    pub fn also(mut self, span: &Span, label: impl Into<String>) -> Self {
        self.labels.push(LabeledSpan::new_with_span(Some(label.into()), *span));
        self
    }

    pub fn help(mut self, help: impl Into<String>) -> Self {
        self.help = Some(help.into());
        self
    }

    /// Where the first label starts, the byte offset it is matched by.
    pub fn offset(&self) -> Option<usize> { self.labels.first().map(|label| label.offset()) }

    /// Any diagnostic carried over as it is: its message, its labels and its help.
    pub fn from_diagnostic(diagnostic: &dyn Diagnostic) -> Self {
        Self {
            message: diagnostic.to_string(),
            labels: diagnostic.labels().map(Iterator::collect).unwrap_or_default(),
            help: diagnostic.help().map(|help| help.to_string()),
        }
    }
}

impl From<DecodeError<Span>> for Problem {
    fn from(error: DecodeError<Span>) -> Self { Self::from_diagnostic(&error) }
}

/// Why a config text could not be used: every problem found, with the text they point into.
#[derive(Debug, thiserror::Error, Diagnostic)]
#[error("rini could not use {name}")]
pub struct ConfigError {
    name: String,
    #[source_code]
    source_code: NamedSource<String>,
    #[related]
    problems: Vec<Problem>,
}

impl ConfigError {
    /// The report on `text`, its problems in the order they are written, any that point nowhere
    /// last.
    pub fn new(name: &str, text: &str, mut problems: Vec<Problem>) -> Self {
        problems.sort_by_key(|problem| problem.offset().unwrap_or(usize::MAX));
        Self {
            name: name.to_owned(),
            source_code: NamedSource::new(name, text.to_owned()),
            problems,
        }
    }

    /// The whole report as text, every problem with its lines, and no colour.
    pub fn render(&self) -> String {
        let mut out = String::new();
        let handler = GraphicalReportHandler::new_themed(GraphicalTheme::unicode_nocolor())
            .with_wrap_lines(false);
        if handler.render_report(&mut out, self).is_err() {
            return self.to_string();
        }
        out
    }
}

/// A config that could not be used, or could not be read at all.
#[derive(Debug)]
pub enum LoadError {
    Unreadable { path: String, error: std::io::Error },
    Invalid(ConfigError),
}

impl LoadError {
    pub fn render(&self) -> String {
        match self {
            LoadError::Unreadable { path, error } => format!("rini could not read {path}: {error}"),
            LoadError::Invalid(error) => error.render(),
        }
    }
}

impl fmt::Display for LoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str(&self.render()) }
}

impl From<ConfigError> for LoadError {
    fn from(error: ConfigError) -> Self { LoadError::Invalid(error) }
}
