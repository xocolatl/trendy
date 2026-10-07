//! The parser's diagnostics. Each one says where the fix goes and, when the
//! fix is mechanical, carries it.

use trendy_diag::{Code, Diagnostic, Edit, Span};
use trendy_syntax::SyntaxKind;

use crate::Found;

const DEFINITION_FORM: &str = "an operator definition has the form `operator name{} [];`";

/// Keywords other languages use to start a definition, which models and
/// people tend to write from habit.
const FOREIGN_DEFINITION_KEYWORDS: &[&str] = &[
    "def",
    "fn",
    "fun",
    "func",
    "function",
    "int",
    "method",
    "proc",
    "procedure",
    "sub",
    "void",
];

/// `expected` should have come right before `found`, at `at`.
pub(crate) fn missing(expected: &[SyntaxKind], found: Option<Found>, at: Span) -> Diagnostic {
    let found_desc = found.map_or_else(|| "end of file".to_string(), |f| f.describe());
    let mut d = Diagnostic::error(
        Code::MissingToken,
        format!("expected {}, found {found_desc}", expected[0].describe()),
        at,
        format!("expected {} here", describe_sequence(expected)),
    )
    .with_note(DEFINITION_FORM);

    if let Some(found) = found.filter(|f| f.span.start != at.start) {
        let label = if found.kind == SyntaxKind::OperatorKw {
            "the next definition starts here".to_string()
        } else {
            format!("found {found_desc}")
        };
        d = d.with_secondary(found.span, label);
    }

    let fixed: Option<String> = expected.iter().map(|k| k.fixed_text()).collect();
    match fixed {
        Some(text) => d.with_suggestion(
            format!("insert `{text}`"),
            vec![Edit {
                span: at,
                replacement: text,
            }],
        ),
        None if expected.contains(&SyntaxKind::Ident) => {
            d.with_help("add a name after `operator`, such as `operator main{} [];`")
        }
        None => d,
    }
}

/// Describe a sequence of expected tokens, merging adjacent fixed-text
/// tokens: `[Ident, LBrace, RBrace]` is "identifier, then `{}`".
fn describe_sequence(kinds: &[SyntaxKind]) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut run = String::new();
    for kind in kinds {
        if let Some(text) = kind.fixed_text() {
            run.push_str(text);
        } else {
            if !run.is_empty() {
                parts.push(format!("`{}`", std::mem::take(&mut run)));
            }
            parts.push(kind.describe().to_string());
        }
    }
    if !run.is_empty() {
        parts.push(format!("`{run}`"));
    }
    parts.join(", then ")
}

/// A `node` enclosed in `wrong` delimiters instead of `right` ones.
pub(crate) fn wrong_delimiter(
    node: SyntaxKind,
    right: (SyntaxKind, SyntaxKind),
    wrong: (SyntaxKind, SyntaxKind),
    open: Span,
    close: Option<Span>,
) -> Diagnostic {
    let text = |k: SyntaxKind| k.fixed_text().expect("delimiters have fixed text");
    let (right_open, right_close) = (text(right.0), text(right.1));

    let mut edits = vec![Edit {
        span: open,
        replacement: right_open.to_string(),
    }];
    let mut d = Diagnostic::error(
        Code::WrongDelimiter,
        format!(
            "the {} must be enclosed in `{right_open}{right_close}`, not `{}{}`",
            node.describe(),
            text(wrong.0),
            text(wrong.1),
        ),
        open,
        format!("expected `{right_open}`"),
    )
    .with_note(
        "trendy encloses parameter lists in `{}` and bodies in `[]`, unlike C-family languages",
    );
    if let Some(close) = close {
        d = d.with_secondary(close, format!("expected `{right_close}`"));
        edits.push(Edit {
            span: close,
            replacement: right_close.to_string(),
        });
    }
    d.with_suggestion(format!("use `{right_open}{right_close}`"), edits)
}

/// Text at `span`, starting with `first`, that isn't part of any definition.
pub(crate) fn expected_definition(first: Found, next: Option<Found>, span: Span) -> Diagnostic {
    let d = Diagnostic::error(
        Code::ExpectedDefinition,
        format!(
            "expected an operator definition, found {}",
            first.describe()
        ),
        span,
        "not part of any definition",
    )
    .with_note("every top-level item is an operator definition, starting with `operator`");

    // `fn main…` or `opertor main…`: a definition with the wrong keyword.
    let names_something = next.is_some_and(|n| n.kind == SyntaxKind::Ident);
    let looks_like_keyword = first.kind == SyntaxKind::Ident
        && (FOREIGN_DEFINITION_KEYWORDS.contains(&first.text)
            || (first.text.len() >= 5 && edit_distance(first.text, "operator") <= 2));
    if !(names_something && looks_like_keyword) {
        return d;
    }
    d.with_suggestion(
        format!("replace `{}` with `operator`", first.text),
        vec![Edit {
            span: first.span,
            replacement: "operator".to_string(),
        }],
    )
}

/// Levenshtein distance, comparing case-insensitively.
fn edit_distance(a: &str, b: &str) -> usize {
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

    #[test]
    fn sequence_descriptions() {
        use SyntaxKind::*;
        assert_eq!(describe_sequence(&[Semicolon]), "`;`");
        assert_eq!(describe_sequence(&[RBracket, Semicolon]), "`];`");
        assert_eq!(
            describe_sequence(&[Ident, LBrace, RBrace]),
            "identifier, then `{}`"
        );
    }
}
