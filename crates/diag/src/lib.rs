//! Source spans and diagnostics.
//!
//! Most trendy code is expected to be written by language models, and their
//! main feedback loop is reading diagnostics. So every diagnostic must say
//! exactly where the problem is and, whenever the fix is mechanical, carry it
//! as a [`Suggestion`]: concrete edits that a tool can apply without
//! interpretation. Every diagnostic has a stable [`Code`], and
//! `trendyc --explain` prints a page about it with a failing and a corrected
//! example.

mod code;
mod json;
mod render;
mod source;

pub use code::{Code, CodeKind};
pub use source::{LineCol, SourceMap};

/// Byte range into the source text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Span {
    pub start: usize,
    pub end: usize,
}

impl Span {
    #[must_use]
    pub fn new(start: usize, end: usize) -> Self {
        Span { start, end }
    }

    /// An empty span at `offset`, for "something is missing here".
    #[must_use]
    pub fn empty(offset: usize) -> Self {
        Span::new(offset, offset)
    }
}

/// A span with an explanation of what is there, or what should be.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Label {
    pub span: Span,
    pub message: String,
}

/// One text replacement. An empty span inserts; an empty replacement deletes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edit {
    pub span: Span,
    pub replacement: String,
}

/// A fix that can be applied mechanically. The edits don't overlap, and
/// applying all of them resolves the diagnostic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Suggestion {
    /// What the fix does, as an imperative: "insert `;`".
    pub message: String,
    pub edits: Vec<Edit>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub code: Code,
    pub message: String,
    /// Where the problem is. For something missing, an empty span where it
    /// belongs.
    pub primary: Label,
    /// Other places that explain the problem, such as an earlier definition.
    pub secondary: Vec<Label>,
    /// Background facts about the language that explain the error.
    pub notes: Vec<String>,
    /// Advice that is not a mechanical fix.
    pub help: Vec<String>,
    pub suggestions: Vec<Suggestion>,
}

impl Diagnostic {
    /// An error whose primary label is `label` at `span`.
    #[must_use]
    pub fn error(
        code: Code,
        message: impl Into<String>,
        span: Span,
        label: impl Into<String>,
    ) -> Self {
        Diagnostic {
            code,
            message: message.into(),
            primary: Label {
                span,
                message: label.into(),
            },
            secondary: Vec::new(),
            notes: Vec::new(),
            help: Vec::new(),
            suggestions: Vec::new(),
        }
    }

    #[must_use]
    pub fn with_secondary(mut self, span: Span, message: impl Into<String>) -> Self {
        self.secondary.push(Label {
            span,
            message: message.into(),
        });
        self
    }

    #[must_use]
    pub fn with_note(mut self, note: impl Into<String>) -> Self {
        self.notes.push(note.into());
        self
    }

    #[must_use]
    pub fn with_help(mut self, help: impl Into<String>) -> Self {
        self.help.push(help.into());
        self
    }

    #[must_use]
    pub fn with_suggestion(mut self, message: impl Into<String>, edits: Vec<Edit>) -> Self {
        self.suggestions.push(Suggestion {
            message: message.into(),
            edits,
        });
        self
    }

    /// Where the problem is.
    #[must_use]
    pub fn span(&self) -> Span {
        self.primary.span
    }

    /// Render for a person reading a terminal, with source excerpts.
    #[must_use]
    pub fn render(&self, path: &str, source: &str) -> String {
        render::human(self, path, &SourceMap::new(source))
    }

    /// Render as one line of JSON, for tools and agents.
    #[must_use]
    pub fn to_json(&self, path: &str, source: &str) -> String {
        json::render(self, path, &SourceMap::new(source))
    }
}

/// How drivers print diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// Readable text with source excerpts, separated by blank lines.
    Human,
    /// One JSON object per line.
    Json,
}

impl Format {
    /// Parse the value of an `--error-format` flag.
    #[must_use]
    pub fn from_flag(value: &str) -> Option<Self> {
        match value {
            "human" => Some(Format::Human),
            "json" => Some(Format::Json),
            _ => None,
        }
    }
}

/// Render diagnostics in the order given.
#[must_use]
pub fn render_all(diagnostics: &[Diagnostic], format: Format, path: &str, source: &str) -> String {
    let map = SourceMap::new(source);
    match format {
        Format::Human => diagnostics
            .iter()
            .map(|d| render::human(d, path, &map))
            .collect::<Vec<_>>()
            .join("\n"),
        Format::Json => diagnostics
            .iter()
            .map(|d| json::render(d, path, &map) + "\n")
            .collect(),
    }
}

/// Join items for prose: `a`, `a and b`, or `a, b and c`.
#[must_use]
pub fn conjoin(items: &[String]) -> String {
    joined(items, "and")
}

/// Join alternatives for prose: `a`, `a or b`, or `a, b or c`.
#[must_use]
pub fn disjoin(items: &[String]) -> String {
    joined(items, "or")
}

fn joined(items: &[String], word: &str) -> String {
    match items {
        [] => String::new(),
        [only] => only.clone(),
        [init @ .., last] => format!("{} {word} {last}", init.join(", ")),
    }
}

/// Levenshtein distance, comparing case-insensitively. Used to suggest the
/// name that a misspelled one was probably meant to be.
#[must_use]
pub fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.to_lowercase().chars().collect();
    let b: Vec<char> = b.to_lowercase().chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut diagonal = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let substitute = diagonal + usize::from(ca != cb);
            diagonal = row[j + 1];
            row[j + 1] = substitute.min(row[j] + 1).min(row[j + 1] + 1);
        }
    }
    row[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edit_distances() {
        assert_eq!(edit_distance("operator", "operator"), 0);
        assert_eq!(edit_distance("Operator", "operator"), 0);
        assert_eq!(edit_distance("opertor", "operator"), 1);
        assert_eq!(edit_distance("oprator", "operator"), 1);
        assert_eq!(edit_distance("operater", "operator"), 1);
        assert_eq!(edit_distance("main", "operator"), 7);
    }
}
