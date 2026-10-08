//! The parser's diagnostics. Each one says where the fix goes and, when the
//! fix is mechanical, carries it.

use trendy_diag::{Code, Diagnostic, Edit, Span, edit_distance};
use trendy_syntax::SyntaxKind;

use crate::Found;

pub(crate) const OPERATOR_FORM: &str = "an operator definition has the form `operator name{} [];`";
pub(crate) const HEADING_FORM: &str = "a heading lists attributes with their types: `{ name: type, … }`, as in `tuple { name: text }` or `relation { name: text }`";
pub(crate) const RELATION_VALUE_FORM: &str = "a relation value has the form `relation { tuple, … }`, optionally with a heading first: `relation { name: type, … } { … }`";
pub(crate) const TUPLE_VALUE_FORM: &str = "a tuple value has the form `tuple { name: value, … }`";
pub(crate) const BUILTIN_FORM: &str = "built-ins are declared as `builtin type name;` or `builtin operator name{params};`, only in the prelude";
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
/// definition of the given `form`. `neighbors` are the characters before and
/// after `at`. `help` says what to write when it can't be inserted
/// mechanically.
pub(crate) fn missing(
    expected: &[SyntaxKind],
    found: Option<Found>,
    at: Span,
    neighbors: (Option<char>, Option<char>),
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

    let fixed: Option<Vec<&str>> = expected.iter().map(|k| k.fixed_text()).collect();
    match fixed.map(|texts| join_tokens(&texts)) {
        Some(text) => d.with_suggestion(
            format!("insert `{text}`"),
            vec![Edit {
                span: at,
                replacement: spaced(&text, neighbors),
            }],
        ),
        None => match help {
            Some(help) => d.with_help(help),
            None => d,
        },
    }
}

/// Tokens written in a row, with a space between two words so they don't
/// run together: `[`operator`, `{`]` is `operator{`, but two keywords are
/// separated.
fn join_tokens(texts: &[&str]) -> String {
    let mut joined = String::new();
    for text in texts {
        if joined.ends_with(is_word_char) && text.starts_with(is_word_char) {
            joined.push(' ');
        }
        joined.push_str(text);
    }
    joined
}

fn is_word_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
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

/// A `what` (such as "parameter list") enclosed in `wrong` delimiters
/// instead of `right` ones.
pub(crate) fn wrong_delimiter(
    what: &str,
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
            "the {what} must be enclosed in `{right_open}{right_close}`, not `{}{}`",
            text(wrong.0),
            text(wrong.1),
        ),
        open,
        format!("expected `{right_open}`"),
    )
    .with_note(
        "trendy encloses parameter and argument lists in `{}` and bodies in `[]`, unlike C-family languages",
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

/// A keyword written in another case, such as `TRUE` for `true`.
pub(crate) fn miscased_keyword(found: Found, keyword: &str) -> Diagnostic {
    Diagnostic::error(
        Code::MiscasedKeyword,
        format!("`{}` is not a keyword; write `{keyword}`", found.text),
        found.span,
        "keywords are lowercase",
    )
    .with_note("keywords are case-sensitive, and every keyword is lowercase")
    .with_suggestion(
        format!("write `{keyword}`"),
        vec![Edit {
            span: found.span,
            replacement: keyword.to_string(),
        }],
    )
}

/// `=` or `:=` between an attribute's name and its type or value.
pub(crate) fn wrong_attribute_separator(found: SyntaxKind, span: Span) -> Diagnostic {
    let text = found.fixed_text().expect("separators have fixed text");
    Diagnostic::error(
        Code::AttributeSeparator,
        format!("expected `:` after the attribute name, found `{text}`"),
        span,
        "attributes are written `name: value`",
    )
    .with_note("in both tuple types and tuple values, an attribute's name is followed by `:`")
    .with_suggestion(
        "write `:`",
        vec![Edit {
            span,
            replacement: ":".to_string(),
        }],
    )
}

/// The keyword a foreign operator stands for: `&&` for `and`, `!` for
/// `not`, and so on.
pub(crate) fn foreign_operator(text: &str) -> Option<SyntaxKind> {
    Some(match text {
        "&&" | "&" => SyntaxKind::AndKw,
        "|" => SyntaxKind::OrKw,
        "^" => SyntaxKind::XorKw,
        "->" | "=>" => SyntaxKind::ImpliesKw,
        "!" => SyntaxKind::NotKw,
        "%" => SyntaxKind::ModKw,
        _ => return None,
    })
}

/// The operator keyword `text` spells in another case, such as Tutorial D's
/// `AND`, if any.
pub(crate) fn operator_spelling(text: &str) -> Option<SyntaxKind> {
    SyntaxKind::ALL.into_iter().find(|k| {
        k.is_operator_keyword()
            && k.fixed_text()
                .is_some_and(|t| t != text && t.eq_ignore_ascii_case(text))
    })
}

/// A foreign operator, `found`, used for `keyword`. `neighbors` are the
/// characters around it.
pub(crate) fn foreign_operator_used(
    found: Found,
    keyword: SyntaxKind,
    neighbors: (Option<char>, Option<char>),
) -> Diagnostic {
    let spelled = keyword.fixed_text().expect("keywords have fixed text");
    Diagnostic::error(
        Code::ForeignOperator,
        format!("`{}` is not an operator; write `{spelled}`", found.text),
        found.span,
        format!("trendy writes this `{spelled}`"),
    )
    .with_note(match keyword {
        SyntaxKind::ModKw => "the remainder of integer division is the keyword `mod`",
        _ => "the boolean operators are the keywords `not`, `and`, `or`, `xor` and `implies`",
    })
    .with_suggestion(
        format!("write `{spelled}`"),
        vec![Edit {
            span: found.span,
            replacement: spaced(spelled, neighbors),
        }],
    )
}

/// `text`, with a space on either side where it would otherwise run into a
/// word next to it, which would change how it lexes.
fn spaced(text: &str, (before, after): (Option<char>, Option<char>)) -> String {
    let mut spaced = text.to_string();
    if before.is_some_and(is_word_char) && text.starts_with(is_word_char) {
        spaced.insert(0, ' ');
    }
    if after.is_some_and(is_word_char) && text.ends_with(is_word_char) {
        spaced.push(' ');
    }
    spaced
}

/// A call statement, starting with the operator's name `found`, without
/// `call`.
pub(crate) fn missing_call(found: Found) -> Diagnostic {
    Diagnostic::error(
        Code::MissingCall,
        format!("expected `call` before `{}`", found.text),
        found.span,
        "a call statement starts with `call`",
    )
    .with_note(
        "update operators, which don't return a value, are invoked by a `call` statement, as in TTM",
    )
    .with_suggestion(
        "insert `call`",
        vec![Edit {
            span: Span::empty(found.span.start),
            replacement: "call ".to_string(),
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
    let distance = edit_distance(first.text, keyword);
    let misspelled = distance == 0 || (first.text.len() >= 5 && distance <= 2);
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
