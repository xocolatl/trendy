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
pub use source::{File, LineCol, Located, SourceMap, Sources};

/// Byte range into the source text: a file's, or a unit's, its files' one
/// after another (see [`Sources`]).
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

    /// The diagnostic with each of `notes`, of which there may be none, as
    /// an `Option` that is `None`.
    #[must_use]
    pub fn with_notes<N: Into<String>>(mut self, notes: impl IntoIterator<Item = N>) -> Self {
        self.notes.extend(notes.into_iter().map(Into::into));
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

    /// Render for a person reading a terminal, with excerpts of the
    /// `sources` its spans are in.
    #[must_use]
    pub fn render(&self, sources: &Sources) -> String {
        render::human(self, sources)
    }

    /// Render as one line of JSON, for tools and agents.
    #[must_use]
    pub fn to_json(&self, sources: &Sources) -> String {
        json::render(self, sources)
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

/// Render diagnostics in the order given, whose spans are in `sources`.
#[must_use]
pub fn render_all(diagnostics: &[Diagnostic], format: Format, sources: &Sources) -> String {
    match format {
        Format::Human => diagnostics
            .iter()
            .map(|d| render::human(d, sources))
            .collect::<Vec<_>>()
            .join("\n"),
        Format::Json => diagnostics
            .iter()
            .map(|d| json::render(d, sources) + "\n")
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

/// How many edits `written` is from `meant`, if it may be a misspelling
/// of it: at most two, comparing case-insensitively, and fewer than the
/// longer has letters. A name none of whose letters can stay as they are
/// is another name, as `a` is of `n`, and no misspelling of it.
#[must_use]
pub fn misspelling(written: &str, meant: &str) -> Option<usize> {
    let distance = edit_distance(written, meant);
    let letters = |s: &str| s.to_lowercase().chars().count();
    (distance <= 2 && distance < letters(written).max(letters(meant))).then_some(distance)
}

/// The one of `candidates` that `written`, which names nothing, is most
/// likely a misspelling of: the closest of those it may be one of, as
/// [`misspelling`] says, and the first of equals.
#[must_use]
pub fn closest<'c>(
    written: &str,
    candidates: impl IntoIterator<Item = &'c str>,
) -> Option<&'c str> {
    nearest(candidates, |c| misspelling(written, c))
}

/// The one of `missing`, the names something lacks, that `written`, a
/// name it has instead, is most likely meant as: the closest within two
/// edits, comparing case-insensitively, and the first of equals. Unlike
/// [`closest`], it may keep none of the letters, as `y` for `x`: only
/// the names missing are candidates, not every name in scope.
#[must_use]
pub fn renamed<'c>(written: &str, missing: impl IntoIterator<Item = &'c str>) -> Option<&'c str> {
    nearest(missing, |m| {
        Some(edit_distance(written, m)).filter(|d| *d <= 2)
    })
}

/// The one of `candidates` with the least `distance`, of those it gives
/// one, and the first of equals.
fn nearest<'c>(
    candidates: impl IntoIterator<Item = &'c str>,
    distance: impl Fn(&str) -> Option<usize>,
) -> Option<&'c str> {
    candidates
        .into_iter()
        .filter_map(|c| Some((distance(c)?, c)))
        .min_by_key(|(distance, _)| *distance)
        .map(|(_, c)| c)
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

    #[test]
    fn misspellings() {
        assert_eq!(misspelling("rivr", "river"), Some(1));
        assert_eq!(misspelling("hedaer", "header"), Some(2));
        assert_eq!(misspelling("N", "n"), Some(0));
        assert_eq!(misspelling("n", "ns"), Some(1));
        // Every letter changes: another name.
        assert_eq!(misspelling("n", "a"), None);
        assert_eq!(misspelling("ab", "cd"), None);
        assert_eq!(misspelling("main", "operator"), None);
        assert_eq!(
            closest("quantty", ["quality", "quantity"]),
            Some("quantity")
        );
        assert_eq!(closest("cout", ["out", "count"]), Some("out"));
        assert_eq!(closest("n", ["a", "line"]), None);
        assert_eq!(renamed("y", ["x"]), Some("x"));
        assert_eq!(renamed("y", ["wxyz"]), None);
    }
}
