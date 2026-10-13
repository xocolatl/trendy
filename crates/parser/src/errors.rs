//! The parser's diagnostics. Each one says where the fix goes and, when the
//! fix is mechanical, carries it.

use trendy_diag::{Code, Diagnostic, Edit, Span, edit_distance};
use trendy_syntax::SyntaxKind;

use crate::Found;

pub(crate) const OPERATOR_FORM: &str = "an operator definition has the form `operator name{} [];`";
pub(crate) const HEADING_FORM: &str = "a heading lists attributes with their types: `{ name: type, … }`, as in `tuple { name: text }` or `relation { name: text }`";
pub(crate) const RELATION_VALUE_FORM: &str = "a relation value has the form `relation { tuple, … }`, optionally with a heading first: `relation { name: type, … } { … }`";
pub(crate) const PROJECTION_FORM: &str =
    "a projection and `remove` take attribute names: `r{a, b}`, `r remove {a, b}`";
pub(crate) const NARY_FORM: &str =
    "a join of any number of relations lists them in braces: `join{r, s, t}` or `times{r, s, t}`";
pub(crate) const KEY_FORM: &str = "a key lists attributes after a relation variable's value, or after its type if it has no value: `var r := relation { … } key { a, b };`";
pub(crate) const RENAME_FORM: &str =
    "`rename` takes renamings, each `old as new`: `r rename {postal_code as code}`";
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
/// instead of `right` ones, in a construct of the given `form`.
pub(crate) fn wrong_delimiter(
    what: &str,
    form: &str,
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
    .with_note(form);
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

/// A key spelled `text`, at `span`, as SQL spells one: `primary key` or
/// `unique`.
pub(crate) fn sql_key(span: Span, text: &str) -> Diagnostic {
    Diagnostic::error(
        Code::KeySpelling,
        format!("a key is written `key`, not `{text}`"),
        span,
        "SQL's spelling",
    )
    .with_note(
        "every key is a candidate key, and none is primary: a relation variable can have several, each written `key { … }`",
    )
    .with_suggestion(
        "write `key`",
        vec![Edit {
            span,
            replacement: "key".to_string(),
        }],
    )
}

/// What every join does with the operands' common attributes, for notes.
const JOIN_RULE: &str = "`join` matches the tuples of its operands on all the attributes they have in common, by name; `times` is for operands with none in common, and `compose` is `join` without the common attributes";

/// SQL's spelling of a join, `text`, at `span`, which stands for `keyword`:
/// `natural join` and `inner join` are `join`, and `cross join` is `times`.
/// An outer join has no equivalent.
pub(crate) fn sql_join(span: Span, text: &str, keyword: SyntaxKind) -> Diagnostic {
    let words: Vec<String> = text
        .split_whitespace()
        .map(str::to_ascii_lowercase)
        .collect();
    if ["left", "right", "full"].contains(&words[0].as_str()) {
        return Diagnostic::error(
            Code::SqlJoin,
            format!("there is no `{}`", words.join(" ")),
            span,
            "SQL's outer join",
        )
        .with_note(
            "an outer join fills the missing side with nulls, and trendy has no nulls: every attribute of every tuple has a value",
        )
        .with_help(
            "join the tuples that match with `join`, and keep the others apart, as a relation of their own",
        );
    }
    let spelled = keyword.fixed_text().expect("keywords have fixed text");
    let d = Diagnostic::error(
        Code::SqlJoin,
        format!("a join is written `{spelled}`, not `{text}`"),
        span,
        "SQL's spelling",
    );
    let d = if keyword == SyntaxKind::TimesKw {
        d.with_note(
            "`times` pairs every tuple of one operand with every tuple of the other; the operands must have no attributes in common",
        )
    } else {
        d.with_note(JOIN_RULE)
    };
    d.with_suggestion(
        format!("write `{spelled}`"),
        vec![Edit {
            span,
            replacement: spelled.to_string(),
        }],
    )
}

/// SQL's `on` or `using` clause, starting with `found`, at `clause`, after a
/// join. `removal` deletes it with the whitespace before it.
pub(crate) fn join_condition(found: Found, clause: Span, removal: Span) -> Diagnostic {
    let word = found.text.to_ascii_lowercase();
    let d = Diagnostic::error(
        Code::SqlJoin,
        format!("a join has no `{word}`"),
        clause,
        "SQL's join condition",
    )
    .with_note(JOIN_RULE);
    if word == "using" {
        d.with_suggestion(
            "delete it: the common attributes are matched already",
            vec![Edit {
                span: removal,
                replacement: String::new(),
            }],
        )
    } else {
        d.with_help(
            "rename attributes so that the ones to match have the same name, and the others different names, as in `r join s rename {r_id as id}`",
        )
    }
}

/// Keys, at `keys` and written `text`, before a variable's initial value,
/// which ends at `value_end`. `removal` deletes them with the whitespace
/// before them.
pub(crate) fn key_before_value(
    keys: Span,
    removal: Span,
    text: &str,
    value_end: usize,
) -> Diagnostic {
    Diagnostic::error(
        Code::KeyBeforeValue,
        "a key goes after the initial value",
        keys,
        "before the value",
    )
    .with_note(KEY_FORM)
    .with_suggestion(
        "move it after the value",
        vec![
            Edit {
                span: removal,
                replacement: String::new(),
            },
            Edit {
                span: Span::empty(value_end),
                replacement: format!(" {text}"),
            },
        ],
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

/// The keyword `found`, used as a name.
pub(crate) fn keyword_as_name(found: Found) -> Diagnostic {
    let keywords: Vec<&str> = SyntaxKind::ALL
        .into_iter()
        .filter(|k| k.is_keyword())
        .filter_map(SyntaxKind::fixed_text)
        .collect();
    Diagnostic::error(
        Code::KeywordAsName,
        format!("`{}` is a keyword, so it can't be a name", found.text),
        found.span,
        "a keyword",
    )
    .with_note(format!("the keywords are `{}`", keywords.join("`, `")))
    .with_help(format!(
        "choose another name, such as `{0}_` or `my_{0}`, and use it wherever this one is used",
        found.text
    ))
}

/// `old: new`, `found` being the `:`, in a renaming. `neighbors` are the
/// characters around it.
pub(crate) fn colon_in_renaming(
    found: Found,
    neighbors: (Option<char>, Option<char>),
) -> Diagnostic {
    Diagnostic::error(
        Code::RenameSeparator,
        "a renaming is written `old as new`",
        found.span,
        "write `as` here",
    )
    .with_note("`rename` takes renamings, each `old as new`, so the direction is always clear")
    .with_suggestion(
        "write `as`, reading this as `old: new`",
        vec![Edit {
            span: found.span,
            replacement: spaced_operator("as", neighbors),
        }],
    )
}

/// `text`, with a space on either side, unless whitespace is already there.
fn spaced_operator(text: &str, (before, after): (Option<char>, Option<char>)) -> String {
    let mut spaced = text.to_string();
    if !before.is_some_and(char::is_whitespace) {
        spaced.insert(0, ' ');
    }
    if !after.is_some_and(char::is_whitespace) {
        spaced.push(' ');
    }
    spaced
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
        "==" => SyntaxKind::Eq,
        "!=" => SyntaxKind::LtGt,
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
        SyntaxKind::Eq | SyntaxKind::LtGt => {
            "equality is `=`, and inequality `<>`; assignment is `:=`"
        }
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

/// A comparison, `found`, right after another one whose right operand is
/// written `middle`. `before` is the character before `found`.
pub(crate) fn chained_comparison(found: Found, middle: &str, before: Option<char>) -> Diagnostic {
    let space = if before.is_some_and(char::is_whitespace) {
        ""
    } else {
        " "
    };
    Diagnostic::error(
        Code::ChainedComparison,
        "comparisons can't be chained",
        found.span,
        "a second comparison",
    )
    .with_note("`a < b < c` would compare the boolean `a < b` with `c`, which is almost never meant, so comparisons don't chain")
    .with_suggestion(
        format!("compare `{middle}` twice: `… and {middle} {} …`", found.text),
        vec![Edit {
            span: trendy_diag::Span::empty(found.span.start),
            replacement: format!("{space}and {middle} "),
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
