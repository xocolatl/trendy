//! The parser's diagnostics. Each one says where the fix goes and, when the
//! fix is mechanical, carries it.

use trendy_diag::{Code, Diagnostic, Edit, Span, edit_distance};
use trendy_syntax::SyntaxKind;

use crate::Found;

pub(crate) const OPERATOR_FORM: &str = "an operator definition has the form `operator name{} [];`";
pub(crate) const VARIABLE_FORM: &str = "a variable declaration has the form `var name: type := value;`, where either the type or the value may be left out";

/// Words other languages use to start a definition or declaration, which
/// models and people tend to write from habit: keywords such as `fn` and
/// `let`, and type names that C-family languages put first, such as `int`.
const FOREIGN_KEYWORDS: &[&str] = &[
    "auto",
    "bool",
    "char",
    "const",
    "def",
    "double",
    "fn",
    "fun",
    "func",
    "function",
    "int",
    "let",
    "long",
    "method",
    "proc",
    "procedure",
    "static",
    "string",
    "sub",
    "val",
    "void",
];

/// `expected` should have come right before `found`, at `at`, in a
/// definition of the given `form`. `help` says what to write when it can't be
/// inserted mechanically.
pub(crate) fn missing(
    expected: &[SyntaxKind],
    found: Option<Found>,
    at: Span,
    form: &str,
    help: Option<&str>,
) -> Diagnostic {
    let found_desc = found.map_or_else(|| "end of file".to_string(), |f| f.describe());
    let mut d = Diagnostic::error(
        Code::MissingToken,
        format!("expected {}, found {found_desc}", expected[0].describe()),
        at,
        format!("expected {} here", describe_sequence(expected)),
    )
    .with_note(form);

    if let Some(found) = found.filter(|f| f.span.start != at.start) {
        let label = if matches!(found.kind, SyntaxKind::OperatorKw | SyntaxKind::VarKw) {
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
        None => match help {
            Some(help) => d.with_help(help),
            None => d,
        },
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

/// The boolean literal `text` spells in some other case, if any.
pub(crate) fn boolean_spelling(text: &str) -> Option<&'static str> {
    [SyntaxKind::TrueKw, SyntaxKind::FalseKw]
        .into_iter()
        .filter_map(SyntaxKind::fixed_text)
        .find(|literal| literal.eq_ignore_ascii_case(text))
}

/// `True` or `TRUE` where `true` belongs.
pub(crate) fn miscased_boolean(found: Found) -> Diagnostic {
    let literal = boolean_spelling(found.text).expect("checked by the caller");
    Diagnostic::error(
        Code::MiscasedBoolean,
        format!("`{}` is not a boolean literal", found.text),
        found.span,
        "boolean literals are lowercase",
    )
    .with_note("the boolean literals are `true` and `false`; keywords are case-sensitive")
    .with_suggestion(
        format!("write `{literal}`"),
        vec![Edit {
            span: found.span,
            replacement: literal.to_string(),
        }],
    )
}

/// `=` where `:=` belongs.
pub(crate) fn assignment_with_eq(span: Span) -> Diagnostic {
    Diagnostic::error(
        Code::AssignmentWithEq,
        "expected `:=` for the initial value, found `=`",
        span,
        "`=` is not assignment",
    )
    .with_note("in trendy, as in Tutorial D, `=` means equality; assignment is `:=`")
    .with_suggestion(
        "write `:=`",
        vec![Edit {
            span,
            replacement: ":=".to_string(),
        }],
    )
}

/// Text at `span`, starting with `first`, that isn't part of any definition.
/// `name` and `after_name` are the two tokens after `first`.
pub(crate) fn expected_definition(
    first: Found,
    name: Option<Found>,
    after_name: Option<Found>,
    span: Span,
) -> Diagnostic {
    let d = Diagnostic::error(
        Code::ExpectedDefinition,
        format!("expected a definition, found {}", first.describe()),
        span,
        "not part of any definition",
    )
    .with_note(
        "every top-level item is an operator definition, starting with `operator`, or a variable declaration, starting with `var`",
    );

    // `fn main{…`, `let x = …` or `opertor main…`: a definition with the wrong
    // keyword. What follows the name says which kind of definition it is.
    if first.kind != SyntaxKind::Ident || name.is_none_or(|n| n.kind != SyntaxKind::Ident) {
        return d;
    }
    let keyword = match after_name.map(|f| f.kind) {
        Some(SyntaxKind::LBrace | SyntaxKind::LParen | SyntaxKind::LBracket) => "operator",
        Some(SyntaxKind::Eq | SyntaxKind::ColonEq | SyntaxKind::Colon | SyntaxKind::Semicolon) => {
            "var"
        }
        _ => return d,
    };
    let misspelled = first.text.len() >= 5 && edit_distance(first.text, keyword) <= 2;
    if !(FOREIGN_KEYWORDS.contains(&first.text) || misspelled) {
        return d;
    }
    d.with_suggestion(
        format!("replace `{}` with `{keyword}`", first.text),
        vec![Edit {
            span: first.span,
            replacement: keyword.to_string(),
        }],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

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
