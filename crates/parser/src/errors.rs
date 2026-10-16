//! The parser's diagnostics. Each one says where the fix goes and, when the
//! fix is mechanical, carries it.

use trendy_diag::{Code, Diagnostic, Edit, Span, edit_distance};
use trendy_syntax::SyntaxKind;

use crate::Found;

pub(crate) const OPERATOR_FORM: &str = "an operator definition has the form `operator name{} [];`, or, for a read-only operator, `operator name{}: type [return value;];`";
pub(crate) const HEADING_FORM: &str = "a heading lists attributes with their types: `{ name: type, … }`, as in `tuple { name: text }` or `relation { name: text }`";
pub(crate) const RELATION_VALUE_FORM: &str = "a relation value has the form `relation { tuple, … }`, optionally with a heading first: `relation { name: type, … } { … }`";
pub(crate) const PROJECTION_FORM: &str =
    "a projection and `remove` take attribute names: `r{a, b}`, `r remove {a, b}`";
pub(crate) const NARY_FORM: &str = "`{}` encloses what can be in any order, and `[]` what can't: `join{r, s, t}` and `times{r, s, t}` can be in any order, while `compose[r, s, t]` is `(r compose s) compose t`";
pub(crate) const EXTEND_FORM: &str = "`extend` takes new attributes, each `name := value`, where the value is computed from the tuple's own attributes: `r extend { total := price * quantity }`";
pub(crate) const REPLACE_FORM: &str = "`replace` takes attributes the operand has, each `name := value`, where the new value is computed from the tuple's own attributes: `r replace { price := price * 2 }`";
pub(crate) const IMAGE_FORM: &str = "in an `extend`'s or a `replace`'s values, `!!r` is the tuples of `r` that match the tuple at hand on their common attributes, without those attributes; `r` is a name, or an expression in parentheses";
pub(crate) const GROUP_FORM: &str = "`group` gathers attributes into new relation-valued attributes, each `name := {attributes}`: `r group { residents := {name} }`";
pub(crate) const WRAP_FORM: &str = "`wrap` gathers attributes into new tuple-valued attributes, each `name := {attributes}`: `r wrap { address := {street, city} }`";
pub(crate) const UNGROUP_FORM: &str = "`ungroup` takes the relation-valued attributes to replace with their attributes: `r ungroup {members}`";
pub(crate) const UNWRAP_FORM: &str = "`unwrap` takes the tuple-valued attributes to replace with their attributes: `r unwrap {address}`";
pub(crate) const KEY_FORM: &str = "a key lists attributes after a relation variable's value, or after its type if it has no value: `var r := relation { … } key { a, b };`";
pub(crate) const RENAME_FORM: &str =
    "`rename` takes renamings, each `old as new`: `r rename {postal_code as code}`";
pub(crate) const TUPLE_VALUE_FORM: &str = "a tuple value has the form `tuple { name: value, … }`";
pub(crate) const BUILTIN_FORM: &str = "built-ins are declared as `builtin type name;`, `builtin operator name{params};` or `builtin operator name{params}: type;`, only in the prelude";
pub(crate) const ASSIGNMENT_FORM: &str = "an assignment statement is `name := value;`, or a shorthand for one: `insert r s;`, `d_insert r s;`, `delete r where p;`, `i_delete r s;` or `update r { a := x } where p;`";
pub(crate) const ASSIGN_BLOCK_FORM: &str =
    "an `assign` block makes assignments all at once: `assign { x := y; y := x; };`";
pub(crate) const UPDATE_FORM: &str = "`update` takes attributes the variable has, each `name := value`, then for a relation an optional `where` and condition: `update r { price := price * 2 } where id = 7;`";
pub(crate) const TYPES_FORM: &str = "an operator's type parameters follow its parameters and result type, each `name: any type`, `any tuple` or `any relation`: `operator pair{a: T, b: T}: relation { v: T } types { T: any type } [ … ];`";
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
            Code::SqlRelational,
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
        Code::SqlRelational,
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
        Code::SqlRelational,
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

/// What a list of attribute assignments does: `extend` adds attributes,
/// and `replace` gives existing ones new values, as does `update`, to a
/// variable's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Assigning {
    Extend,
    Replace,
    Update,
}

impl Assigning {
    /// What `keyword`'s assignments do.
    pub(crate) fn of(keyword: SyntaxKind) -> Assigning {
        match keyword {
            SyntaxKind::ExtendKw => Assigning::Extend,
            SyntaxKind::ReplaceKw => Assigning::Replace,
            SyntaxKind::UpdateKw => Assigning::Update,
            _ => unreachable!("only `extend`, `replace` and `update` assign attributes"),
        }
    }

    pub(crate) fn form(self) -> &'static str {
        match self {
            Assigning::Extend => EXTEND_FORM,
            Assigning::Replace => REPLACE_FORM,
            Assigning::Update => UPDATE_FORM,
        }
    }

    /// What the list is called when it is missing.
    pub(crate) fn list(self) -> &'static str {
        match self {
            Assigning::Extend => "new attributes",
            Assigning::Replace | Assigning::Update => "attributes and their new values",
        }
    }

    /// The attribute each assignment names.
    fn attribute(self) -> &'static str {
        match self {
            Assigning::Extend => "new attribute",
            Assigning::Replace | Assigning::Update => "attribute",
        }
    }

    /// Help for a value with no name before it.
    pub(crate) fn name_help(self) -> &'static str {
        match self {
            Assigning::Extend => {
                "write the new attribute's name and `:=` before the value, as in `total := a + b`"
            }
            Assigning::Replace | Assigning::Update => {
                "write the name of the attribute it replaces and `:=` before the value, as in `price := price * 2`"
            }
        }
    }
}

/// `:` or `=`, `found` at `span`, between an attribute's name and its value
/// in an `extend` or a `replace`, as `assigning` says, where `:=` goes.
/// `neighbors` are the characters around it.
pub(crate) fn assign_separator(
    assigning: Assigning,
    found: SyntaxKind,
    span: Span,
    neighbors: (Option<char>, Option<char>),
) -> Diagnostic {
    let text = found.fixed_text().expect("separators have fixed text");
    let attribute = assigning.attribute();
    let note = match assigning {
        Assigning::Extend => {
            "in `extend`, each new attribute is defined as a value computed from the tuple, with `:=`, as a variable is; `:` pairs a name with a given value in a tuple"
        }
        Assigning::Replace => {
            "in `replace`, each attribute is given a new value computed from the tuple, with `:=`, as a variable is; `:` pairs a name with a given value in a tuple, and `=` compares"
        }
        Assigning::Update => {
            "in `update`, each attribute is given a new value computed from the tuple, with `:=`, as a variable is; `:` pairs a name with a given value in a tuple, and `=` compares"
        }
    };
    Diagnostic::error(
        Code::AttributeSeparator,
        format!("expected `:=` after the {attribute}'s name, found `{text}`"),
        span,
        format!("{attribute}s are written `name := value` here"),
    )
    .with_note(note)
    .with_suggestion(
        "write `:=`",
        vec![Edit {
            span,
            replacement: spaced_operator(":=", neighbors),
        }],
    )
}

/// A value, at `span`, in an `extend` or a `replace`, as `assigning` says,
/// with no attribute name.
pub(crate) fn unnamed_assign(assigning: Assigning, span: Span) -> Diagnostic {
    let message = match assigning {
        Assigning::Extend => "a new attribute needs a name",
        Assigning::Replace | Assigning::Update => {
            "an attribute's new value needs the attribute's name"
        }
    };
    Diagnostic::error(Code::MissingToken, message, span, "this value has no name")
        .with_note(assigning.form())
        .with_help(assigning.name_help())
}

/// `value as name`, at `span`, in an `extend` or a `replace`, as
/// `assigning` says, as SQL writes a computed column and Tutorial D's
/// `EXTEND … ADD` a new attribute.
pub(crate) fn assign_as(assigning: Assigning, span: Span, name: &str, value: &str) -> Diagnostic {
    Diagnostic::error(
        Code::AttributeSeparator,
        format!(
            "an {} is written `{name} := …`, not `… as {name}`",
            match assigning {
                Assigning::Extend => "new attribute",
                Assigning::Replace | Assigning::Update => "attribute's new value",
            }
        ),
        span,
        "the name goes first",
    )
    .with_note(assigning.form())
    .with_suggestion(
        format!("write `{name} := …`"),
        vec![Edit {
            span,
            replacement: format!("{name} := {value}"),
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

/// The operator `name`, at `span`, directly after the image relation at
/// `image`, whose operand starts at `operand`, which could apply to the
/// image or to its operand. `steps` are the operators that follow, in
/// order, each with where the expression it makes ends: the image can be
/// taken before any of them, or after all, and each is a suggestion.
pub(crate) fn unordered_image(
    name: &str,
    span: Span,
    image: Span,
    operand: usize,
    steps: &[(&str, usize)],
) -> Diagnostic {
    let insert = |at: usize, text: &str| Edit {
        span: Span::new(at, at),
        replacement: text.to_string(),
    };
    let mut d = Diagnostic::error(
        Code::UnorderedOperators,
        format!("`!!` and {name} need parentheses to say which applies first"),
        span,
        "this could apply to the image, or to the relation it is of",
    )
    .with_note("`!!` takes a single operand: a name, or an expression in parentheses")
    .with_note("an operator after it could apply to either, so neither is assumed: postfix operators and joins there need parentheses");
    d = d.with_suggestion(
        "take the image first: `(!!r) …`",
        vec![insert(image.start, "("), insert(image.end, ")")],
    );
    for (k, &(name, end)) in steps.iter().enumerate() {
        d = if k + 1 == steps.len() {
            d.with_suggestion(
                "take the image last: `!!(r …)`",
                vec![insert(operand, "("), insert(end, ")")],
            )
        } else {
            d.with_suggestion(
                format!("take the image after {name}: `(!!(r …)) …`"),
                vec![
                    insert(image.start, "("),
                    insert(operand, "("),
                    insert(end, "))"),
                ],
            )
        };
    }
    d
}

/// What a list of gatherings makes: `group` new relation-valued
/// attributes, and `wrap` new tuple-valued ones.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Gathering {
    Group,
    Wrap,
}

impl Gathering {
    /// What `keyword`'s gatherings make.
    pub(crate) fn of(keyword: SyntaxKind) -> Gathering {
        match keyword {
            SyntaxKind::GroupKw => Gathering::Group,
            SyntaxKind::WrapKw => Gathering::Wrap,
            _ => unreachable!("only `group` and `wrap` gather attributes"),
        }
    }

    fn keyword(self) -> &'static str {
        match self {
            Gathering::Group => "group",
            Gathering::Wrap => "wrap",
        }
    }

    pub(crate) fn form(self) -> &'static str {
        match self {
            Gathering::Group => GROUP_FORM,
            Gathering::Wrap => WRAP_FORM,
        }
    }

    /// What the list is called when it is missing.
    pub(crate) fn list(self) -> &'static str {
        match self {
            Gathering::Group => "groupings",
            Gathering::Wrap => "wrappings",
        }
    }

    /// One of the list's items.
    fn item(self) -> &'static str {
        match self {
            Gathering::Group => "grouping",
            Gathering::Wrap => "wrapping",
        }
    }

    /// Help for attributes with no name before them.
    pub(crate) fn name_help(self) -> &'static str {
        match self {
            Gathering::Group => {
                "write the new attribute's name and `:=` before the attributes, as in `residents := {name}`"
            }
            Gathering::Wrap => {
                "write the new attribute's name and `:=` before the attributes, as in `address := {street, city}`"
            }
        }
    }
}

/// `:` or `=`, `found` at `span`, between a gathering's name and its
/// attributes, where `:=` goes. `neighbors` are the characters around it.
pub(crate) fn gathering_separator(
    gathering: Gathering,
    found: SyntaxKind,
    span: Span,
    neighbors: (Option<char>, Option<char>),
) -> Diagnostic {
    let text = found.fixed_text().expect("separators have fixed text");
    Diagnostic::error(
        Code::AttributeSeparator,
        format!("expected `:=` after the new attribute's name, found `{text}`"),
        span,
        format!("{}s are written `name := {{attributes}}`", gathering.item()),
    )
    .with_note(format!(
        "in `{}`, each new attribute is defined by the attributes it gathers, with `:=`, as an `extend` defines one; `:` pairs a name with a given value in a tuple",
        gathering.keyword()
    ))
    .with_suggestion(
        "write `:=`",
        vec![Edit {
            span,
            replacement: spaced_operator(":=", neighbors),
        }],
    )
}

/// `{…} as name`, at `span`, in a `group` or a `wrap`, as Tutorial D writes
/// one, where `list` is the attributes as written.
pub(crate) fn gathering_as(gathering: Gathering, span: Span, name: &str, list: &str) -> Diagnostic {
    Diagnostic::error(
        Code::AttributeSeparator,
        format!(
            "a {} is written `{name} := {list}`, not `{list} as {name}`",
            gathering.item()
        ),
        span,
        "the name goes first",
    )
    .with_note(gathering.form())
    .with_note("`as` only renames; a new attribute is defined with `:=`, its name first")
    .with_suggestion(
        format!("write `{name} := {list}`"),
        vec![Edit {
            span,
            replacement: format!("{name} := {list}"),
        }],
    )
}

/// Attributes, at `span`, in a `group` or a `wrap`, with no name for the
/// new attribute.
pub(crate) fn unnamed_gathering(gathering: Gathering, span: Span) -> Diagnostic {
    Diagnostic::error(
        Code::MissingToken,
        format!("a {} needs a name", gathering.item()),
        span,
        "these attributes have no name to be gathered into",
    )
    .with_note(gathering.form())
    .with_help(gathering.name_help())
}

/// SQL's `by {…}`, at `span`, after `group`, which names the attributes
/// kept rather than those gathered.
pub(crate) fn group_by(span: Span) -> Diagnostic {
    Diagnostic::error(
        Code::SqlRelational,
        "`group` names the attributes to gather, not those to group by",
        span,
        "SQL's `GROUP BY` names the others",
    )
    .with_note(GROUP_FORM)
    .with_note("the attributes not gathered are the ones each group shares, as SQL's `GROUP BY` lists")
    .with_help("list the other attributes, and name the new attribute: `r group by {city}` is `r group { members := {…every attribute but city…} }`")
}

/// The list of `operator`, `extend`, `replace`, `group` or `wrap`, at
/// `list`, in
/// brackets opening at `open`, with `items` as written. Its items apply all
/// at once, in braces; in order, they are a chain of the operator, which is
/// the fix, since brackets ask for an order.
pub(crate) fn bracketed_list(
    operator: &str,
    open: Span,
    list: Span,
    items: &[String],
) -> Diagnostic {
    let replacement = if items.is_empty() {
        "{}".to_string()
    } else {
        items
            .iter()
            .map(|item| format!("{{ {item} }}"))
            .collect::<Vec<_>>()
            .join(&format!(" {operator} "))
    };
    let d = Diagnostic::error(
        Code::WrongDelimiter,
        format!("`{operator}` takes its list in braces, applied all at once"),
        open,
        "brackets would mean in order",
    )
    .with_note(format!(
        "braces enclose what can be in any order, and brackets what can't; the items of one `{operator}` apply all at once, so to apply them in order, chain `{operator}`s"
    ));
    if items.len() > 1 {
        d.with_suggestion(
            format!("apply them in order: `r {operator} {{ … }} {operator} {{ … }}`"),
            vec![Edit {
                span: list,
                replacement,
            }],
        )
    } else {
        d.with_suggestion(
            "use `{}`",
            vec![Edit {
                span: list,
                replacement,
            }],
        )
    }
}

/// What a list of attributes to unpack holds: relation-valued ones, for
/// `ungroup`, and tuple-valued ones, for `unwrap`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Unpacking {
    Ungroup,
    Unwrap,
}

impl Unpacking {
    /// What `keyword` unpacks.
    pub(crate) fn of(keyword: SyntaxKind) -> Unpacking {
        match keyword {
            SyntaxKind::UngroupKw => Unpacking::Ungroup,
            SyntaxKind::UnwrapKw => Unpacking::Unwrap,
            _ => unreachable!("only `ungroup` and `unwrap` unpack attributes"),
        }
    }

    fn keyword(self) -> &'static str {
        match self {
            Unpacking::Ungroup => "ungroup",
            Unpacking::Unwrap => "unwrap",
        }
    }

    pub(crate) fn form(self) -> &'static str {
        match self {
            Unpacking::Ungroup => UNGROUP_FORM,
            Unpacking::Unwrap => UNWRAP_FORM,
        }
    }
}

/// A bare name, `found`, after `ungroup` or `unwrap`, where a list in
/// braces goes.
pub(crate) fn bare_unpacked(unpacking: Unpacking, found: Found) -> Diagnostic {
    let keyword = unpacking.keyword();
    Diagnostic::error(
        Code::WrongDelimiter,
        format!("`{keyword}` takes its attributes in braces: `{{{}}}`", found.text),
        found.span,
        "not in braces",
    )
    .with_note(unpacking.form())
    .with_note(format!("like `remove`, `{keyword}` takes a list of attribute names, in braces since their order doesn't matter"))
    .with_suggestion(
        "put it in braces",
        vec![Edit {
            span: found.span,
            replacement: format!("{{{}}}", found.text),
        }],
    )
}

/// The dyadic operator another language's name `text`, in any case, stands
/// for, and how trendy spells it: the literature's `semijoin` for
/// `matching`, `semiminus` and `antijoin` for `not matching`, and SQL's
/// `except` for `minus`.
pub(crate) fn operator_name(text: &str) -> Option<(SyntaxKind, &'static str)> {
    let lower = text.to_ascii_lowercase();
    Some(match lower.as_str() {
        "semijoin" => (SyntaxKind::MatchingKw, "matching"),
        "semiminus" | "antijoin" => (SyntaxKind::MatchingKw, "not matching"),
        "except" => (SyntaxKind::MinusKw, "minus"),
        _ => return None,
    })
}

/// `found`, another language's name for the operator trendy writes
/// `spelled`.
pub(crate) fn renamed_operator(found: Found, spelled: &str) -> Diagnostic {
    let (code, note) = if spelled == "minus" {
        (
            Code::SqlRelational,
            "`r minus s` is the tuples of `r` that aren't in `s`, of the same heading: SQL's `except`",
        )
    } else {
        (
            Code::ForeignOperator,
            "`r matching s` is the tuples of `r` that have a match in `s` on their common attributes, and `r not matching s` those that have none: the semijoin and the antijoin",
        )
    };
    Diagnostic::error(
        code,
        format!("`{}` is written `{spelled}`", found.text),
        found.span,
        format!("trendy writes this `{spelled}`"),
    )
    .with_note(note)
    .with_suggestion(
        format!("write `{spelled}`"),
        vec![Edit {
            span: found.span,
            replacement: spelled.to_string(),
        }],
    )
}

/// `r union s minus t`, whose `minus` is at `span`: `r` starts at `start`,
/// `s` ends at `s_end`, and `s minus t` is at `minus`. The fixes are each
/// reading, the minus first or last.
pub(crate) fn union_minus(
    (union, minus_word): (&str, &str),
    span: Span,
    start: usize,
    s_end: usize,
    minus: Span,
) -> Diagnostic {
    let insert = |at: usize, text: &str| Edit {
        span: Span::new(at, at),
        replacement: text.to_string(),
    };
    Diagnostic::error(
        Code::UnorderedOperators,
        format!("`{union}` and `{minus_word}` need parentheses to say which applies first"),
        span,
        "this could apply to the union, or to its right operand only",
    )
    .with_note(format!("`{minus_word}` binds like the joins, as an AND, so `r {union} s {minus_word} t` would be `r {union} (s {minus_word} t)`; SQL reads set operators from the left, as `(r {union} s) {minus_word} t`, so neither is assumed"))
    .with_suggestion(
        format!("remove from the union: `(r {union} s) {minus_word} t`"),
        vec![insert(start, "("), insert(s_end, ")")],
    )
    .with_suggestion(
        format!("remove from the right operand: `r {union} (s {minus_word} t)`"),
        vec![insert(minus.start, "("), insert(minus.end, ")")],
    )
}

/// SQL's `all` or `distinct`, `found`, after a set operator, which `span`
/// removes with the space before it.
pub(crate) fn set_quantifier(found: Found, span: Span) -> Diagnostic {
    Diagnostic::error(
        Code::SqlRelational,
        format!("there is no `{}` here", found.text.to_ascii_lowercase()),
        found.span,
        "SQL's set quantifier",
    )
    .with_note("a relation is a set, so it has no duplicates to keep or remove: `union`, `intersect` and `minus` give sets")
    .with_suggestion(
        format!("remove `{}`", found.text),
        vec![Edit {
            span,
            replacement: String::new(),
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

/// A read-only operator's result type, after `found`, another language's
/// `->` or `returns`, which `span` covers with the whitespace before it.
pub(crate) fn result_spelling(found: Found, span: Span) -> Diagnostic {
    Diagnostic::error(
        Code::ResultSpelling,
        format!("a result type is written after `:`, not `{}`", found.text),
        found.span,
        "another language's spelling",
    )
    .with_note("a read-only operator has the form `operator name{params}: type [return value;];`, with the result type written as a variable's is")
    .with_suggestion(
        format!("replace `{}` with `:`", found.text),
        vec![Edit {
            span,
            replacement: ":".to_string(),
        }],
    )
}

/// Type parameters in angle brackets after an operator's name, `<T, U>`,
/// at `span`, with their names if they are only names.
pub(crate) struct Angled {
    pub(crate) span: Span,
    pub(crate) names: Option<Vec<String>>,
}

/// `angled`, written after an operator's name: the fix declares its names
/// with `types` at `at`, the end of the signature, unless the operator
/// already has a `types` list, which is `declared`.
pub(crate) fn angled_type_parameters(angled: &Angled, declared: bool, at: usize) -> Diagnostic {
    let d = Diagnostic::error(
        Code::TypeParameterSpelling,
        "type parameters are declared with `types`, after the signature, not in `<…>`",
        angled.span,
        "another language's syntax",
    )
    .with_note(TYPES_FORM)
    .with_note(
        "each call gives the type parameters types from its arguments, so a call never names them",
    );
    let Some(names) = &angled.names else {
        return d.with_help(
            "remove the `<…>` and declare each type parameter after the result type, as `types { T: any type }`",
        );
    };
    let mut edits = vec![Edit {
        span: angled.span,
        replacement: String::new(),
    }];
    if !declared {
        let list: Vec<String> = names.iter().map(|n| format!("{n}: any type")).collect();
        edits.push(Edit {
            span: Span::new(at, at),
            replacement: format!(" types {{ {} }}", list.join(", ")),
        });
    }
    d.with_suggestion("declare them with `types`", edits)
}

/// `keyword any`, at `span`, where `keyword` is `tuple` or `relation`:
/// written the other way around.
pub(crate) fn any_after(keyword: Found, span: Span) -> Diagnostic {
    let fixed = format!("any {}", keyword.text);
    Diagnostic::error(
        Code::TypeParameterSpelling,
        format!("write `{fixed}`, not `{} any`", keyword.text),
        span,
        "the words are the other way around",
    )
    .with_note("a parameter that takes any tuple, or any relation, has the type `any tuple` or `any relation`, as one that takes any value has `any type`")
    .with_suggestion(
        format!("write `{fixed}`"),
        vec![Edit {
            span,
            replacement: fixed,
        }],
    )
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

/// `=` where `:=` belongs, `context` saying where, as in "in an
/// assignment".
pub(crate) fn assignment_with_eq(span: Span, context: &str) -> Diagnostic {
    Diagnostic::error(
        Code::AssignmentWithEq,
        format!("expected `:=` {context}, found `=`"),
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

/// Assignments `clauses`, separated by `commas`, in the statement at
/// `span`, as TTM writes a multiple assignment; `in_block` says whether
/// the statement is in an `assign` block already.
pub(crate) fn comma_assignments(
    span: Span,
    commas: &[Span],
    clauses: &[&str],
    in_block: bool,
) -> Diagnostic {
    let d = Diagnostic::error(
        Code::CommaAssignment,
        "assignments are separated by `;`, not `,`",
        commas[0],
        "a comma between assignments",
    );
    if in_block {
        return d
            .with_note("in an `assign` block, each assignment ends with `;`, as statements do")
            .with_suggestion(
                "end each assignment with `;`",
                commas
                    .iter()
                    .map(|&span| Edit {
                        span,
                        replacement: ";".to_string(),
                    })
                    .collect(),
            );
    }
    let statements: Vec<String> = clauses.iter().map(|c| format!("{c};")).collect();
    d.with_note(
        "assignments made all at once, each computed from the variables as they were, go in an `assign` block: `assign { x := y; y := x; };`",
    )
    .with_suggestion(
        "make them at once, in an `assign` block",
        vec![Edit {
            span,
            replacement: format!("assign {{ {} }};", statements.join(" ")),
        }],
    )
}

/// SQL's `found`, `into` or `from`, after `keyword`, `insert` or `delete`
/// and their kin, where `span` deletes it.
pub(crate) fn sql_statement_word(found: Found, span: Span, keyword: SyntaxKind) -> Diagnostic {
    let spelled = keyword.fixed_text().expect("keywords have fixed text");
    let form = if matches!(keyword, SyntaxKind::DeleteKw | SyntaxKind::IDeleteKw) {
        "delete r where p"
    } else {
        "insert r s"
    };
    let word = found.text.to_ascii_lowercase();
    Diagnostic::error(
        Code::SqlRelational,
        format!("`{spelled}` takes no `{word}`"),
        found.span,
        "SQL's spelling",
    )
    .with_note(format!(
        "the variable's name follows `{spelled}` directly, as in `{form}`"
    ))
    .with_suggestion(
        format!("delete `{}`", found.text),
        vec![Edit {
            span,
            replacement: String::new(),
        }],
    )
}

/// SQL's `set`, at `set`, before an `update`'s new values, which end at
/// `end`.
pub(crate) fn sql_set(set: Span, end: usize) -> Diagnostic {
    Diagnostic::error(
        Code::SqlRelational,
        "an `update`'s new values are written in braces, not after `set`",
        set,
        "SQL's spelling",
    )
    .with_note(UPDATE_FORM)
    .with_suggestion(
        "write the new values in braces",
        vec![
            Edit {
                span: set,
                replacement: "{".to_string(),
            },
            Edit {
                span: Span::empty(end),
                replacement: " }".to_string(),
            },
        ],
    )
}

/// A `delete` at `span`, of the variable `target`, with no `where`: before
/// `relation`, its text and whether it needs parentheses as an operand, if
/// there is one.
pub(crate) fn delete_without_where(
    span: Span,
    target: Option<&str>,
    relation: Option<(&str, bool)>,
) -> Diagnostic {
    let d = Diagnostic::error(
        Code::DeleteWithoutWhere,
        "`delete` needs `where` and a condition",
        span,
        "no condition",
    );
    let Some(target) = target else {
        return d.with_note(ASSIGNMENT_FORM);
    };
    match relation {
        None => d
            .with_note(
                "`delete r where p` deletes the tuples for which `p` is true, so that every tuple isn't deleted by accident; to delete them all, assign the empty relation",
            )
            .with_suggestion(
                "assign the empty relation",
                vec![Edit {
                    span,
                    replacement: format!("{target} := relation {{}}"),
                }],
            ),
        Some((relation, needs_parentheses)) => {
            let relation = if needs_parentheses {
                format!("({relation})")
            } else {
                relation.to_string()
            };
            d.with_note(
                "`delete r where p` deletes the tuples for which `p` is true; to delete the tuples of a relation, assign the difference, or write `i_delete r s`, which also checks that each is there",
            )
            .with_suggestion(
                "assign the difference",
                vec![Edit {
                    span,
                    replacement: format!("{target} := {target} minus {relation}"),
                }],
            )
        }
    }
}

/// An `update`'s new values, between the parentheses `open` and `close`,
/// after its condition, which starts with `where` at `where_start` and
/// ends at `condition_end`, as Tutorial D writes `UPDATE R WHERE p ( … )`.
pub(crate) fn values_after_where(
    where_start: usize,
    condition_end: usize,
    (open, close): (Span, Span),
    source: &str,
) -> Diagnostic {
    let values = source[open.end..close.start].trim();
    Diagnostic::error(
        Code::UpdateOrder,
        "an `update`'s new values go before `where`",
        Span::new(open.start, close.end),
        "after the condition",
    )
    .with_note(UPDATE_FORM)
    .with_suggestion(
        "move them before `where`, in braces",
        vec![
            Edit {
                span: Span::empty(where_start),
                replacement: format!("{{ {values} }} "),
            },
            Edit {
                span: Span::new(condition_end, close.end),
                replacement: String::new(),
            },
        ],
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
