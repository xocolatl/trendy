//! Sema's diagnostics. Each one says where the fix goes and, when the fix is
//! mechanical, carries it.

use trendy_diag::{Code, Diagnostic, Edit, Span, edit_distance};
use trendy_hir as hir;
use trendy_syntax::SyntaxToken;
use trendy_syntax::ast::{AstNode, Expr, SourceFile};

use crate::types::Origin;
use crate::{Kind, Values};

/// Type names from other languages, and the built-in type each one means.
const TYPE_ALIASES: &[(&str, hir::Scalar)] = &[
    ("bigint", hir::Scalar::Integer),
    ("bit", hir::Scalar::Boolean),
    ("bool", hir::Scalar::Boolean),
    ("char", hir::Scalar::Text),
    ("decimal", hir::Scalar::Rational),
    ("double", hir::Scalar::Rational),
    ("f32", hir::Scalar::Rational),
    ("f64", hir::Scalar::Rational),
    ("float", hir::Scalar::Rational),
    ("float64", hir::Scalar::Rational),
    ("i64", hir::Scalar::Integer),
    ("int", hir::Scalar::Integer),
    ("int64", hir::Scalar::Integer),
    ("long", hir::Scalar::Integer),
    ("number", hir::Scalar::Rational),
    ("numeric", hir::Scalar::Rational),
    ("real", hir::Scalar::Rational),
    ("str", hir::Scalar::Text),
    ("string", hir::Scalar::Text),
    ("varchar", hir::Scalar::Text),
];

/// `name` is defined at `this` but was already defined at `first`, which
/// has no span for a built-in.
pub(crate) fn duplicate_name(
    name: &str,
    this: (Kind, Span),
    first: (Kind, Option<Span>),
) -> Diagnostic {
    let d = Diagnostic::error(
        Code::DuplicateName,
        format!("`{name}` is defined more than once"),
        this.1,
        format!("redefined here as {}", this.0.describe()),
    );
    let d = match first.1 {
        Some(span) => d.with_secondary(
            span,
            format!("`{name}` is first defined here as {}", first.0.describe()),
        ),
        None => d.with_note(format!(
            "`{name}` is {}, declared in the prelude",
            first.0.describe()
        )),
    };
    d.with_note("variables and operators share one namespace, so every name must be unique")
        .with_help("delete the duplicate definition, or rename one of them")
}

pub(crate) fn missing_main(file: &SourceFile, values: &Values) -> Diagnostic {
    const MESSAGE: &str = "program has no `main` operator";
    const ENTRY_NOTE: &str = "execution starts in the operator named `main`";

    if let Some((Kind::Variable, Some(span))) = values.get("main") {
        return Diagnostic::error(
            Code::MissingMain,
            MESSAGE,
            span,
            "`main` is a variable, but the entry point must be an operator",
        )
        .with_note(ENTRY_NOTE)
        .with_help("rename the variable, and define `operator main{} [];`");
    }

    // `Main` or `MAIN` is almost certainly meant to be the entry point, so the
    // fix goes there.
    let near_miss = values
        .iter()
        .filter(|&(name, kind, _)| kind == Kind::Operator && name.eq_ignore_ascii_case("main"))
        .filter_map(|(name, _, span)| Some((name, span?)))
        .min_by_key(|&(_, span)| span.start);
    if let Some((name, span)) = near_miss {
        return Diagnostic::error(
            Code::MissingMain,
            MESSAGE,
            span,
            format!("`{name}` is not `main`: names are case-sensitive"),
        )
        .with_note(ENTRY_NOTE)
        .with_suggestion(
            format!("rename `{name}` to `main`"),
            vec![Edit {
                span,
                replacement: "main".to_string(),
            }],
        );
    }

    let end = file.span().end;
    let text = file.syntax().to_string();
    let separator = match () {
        () if text.is_empty() => "",
        () if text.ends_with('\n') => "\n",
        () => "\n\n",
    };
    Diagnostic::error(
        Code::MissingMain,
        MESSAGE,
        Span::empty(end),
        "expected an operator named `main` in this file",
    )
    .with_note(ENTRY_NOTE)
    .with_suggestion(
        "add an entry point: `operator main{} [];`",
        vec![Edit {
            span: Span::empty(end),
            replacement: format!("{separator}operator main{{}} [];\n"),
        }],
    )
}

/// The type name `ident` doesn't name a type.
pub(crate) fn unknown_type(ident: &SyntaxToken, values: &Values) -> Diagnostic {
    let name = ident.text();
    let span = trendy_syntax::span(ident.text_range());
    let builtins: Vec<_> = hir::Scalar::ALL
        .iter()
        .map(|t| format!("`{}`", t.name()))
        .collect();
    let mut d = Diagnostic::error(
        Code::UnknownType,
        format!("unknown type `{name}`"),
        span,
        "not a type",
    )
    .with_note(format!(
        "the built-in types are {}",
        trendy_diag::conjoin(&builtins)
    ));
    if let Some((kind, _)) = values.get(name) {
        return d.with_note(format!("`{name}` is {}, not a type", kind.describe()));
    }

    let lowercase = name.to_lowercase();
    let meant = TYPE_ALIASES
        .iter()
        .find(|(alias, _)| *alias == lowercase)
        .map(|&(_, ty)| ty)
        .or_else(|| {
            hir::Scalar::ALL
                .into_iter()
                .filter(|t| edit_distance(t.name(), name) <= 2)
                .min_by_key(|t| edit_distance(t.name(), name))
        });
    if let Some(ty) = meant {
        if TYPE_ALIASES.iter().any(|(alias, _)| *alias == lowercase) {
            d = d.with_note(format!("trendy's name for this type is `{}`", ty.name()));
        }
        d = d.with_suggestion(
            format!("use `{}`", ty.name()),
            vec![Edit {
                span,
                replacement: ty.name().to_string(),
            }],
        );
    }
    d
}

/// A value written as `expr`, of type `found`, where `declared` is expected
/// because of `origin`. `value` is the value, if it is a constant, for fixes
/// that rewrite the literal.
pub(crate) fn type_mismatch(
    declared: &hir::Type,
    origin: &Origin,
    found: &hir::Type,
    value: Option<&hir::Value>,
    expr: &Expr,
) -> Diagnostic {
    let mut d = Diagnostic::error(
        Code::TypeMismatch,
        format!("mismatched types: expected `{declared}`, found `{found}`"),
        expr.span(),
        format!("this is `{found}`"),
    )
    .with_note("values never convert to another type implicitly");
    if let Some(span) = origin.span() {
        d = d.with_secondary(span, origin.label());
    }

    // `not n = 0`: `not` binds tightest, so this negates `n`; the comparison
    // was meant.
    if let Some((message, edits)) = negated_comparison(origin, expr) {
        d = d.with_suggestion(message, edits);
    }
    // Rewriting the literal is the likelier fix when it is mechanical, so it
    // comes first; changing the declared type always works.
    // A literal, or a negative one such as `-1`.
    let written = match expr {
        Expr::Literal(literal) => literal.token().map(|t| t.text().to_string()),
        Expr::Unary(unary) => crate::ops::negated_literal(unary)
            .and_then(|literal| literal.token())
            .map(|t| format!("-{}", t.text())),
        _ => None,
    };
    if let (hir::Type::Scalar(scalar), Some(written), Some(value)) = (declared, written, value) {
        let written = Some(written);
        if let Some(replacement) = written.and_then(|w| rewrite_literal(*scalar, value, &w)) {
            d = d.with_suggestion(
                format!("write it as `{declared}`: `{replacement}`"),
                vec![Edit {
                    span: expr.span(),
                    replacement,
                }],
            );
        }
    }
    // Only a written type can be changed to fit the value.
    match origin {
        Origin::Type(type_expr) => d.with_suggestion(
            format!("change the type to `{found}`"),
            vec![Edit {
                span: type_expr.span(),
                replacement: found.to_string(),
            }],
        ),
        Origin::Heading(_) | Origin::Value(_) | Origin::Builtin | Origin::Operator(_) => d,
    }
}

/// The fix for `not` applied to a non-boolean `operand` that starts the left
/// side of a comparison, as in `not n = 0`: negate the comparison instead,
/// `not (n = 0)`, as SQL and Python read it.
fn negated_comparison(origin: &Origin, operand: &Expr) -> Option<(String, Vec<Edit>)> {
    let Origin::Operator(token) = origin else {
        return None;
    };
    if token.kind() != trendy_syntax::SyntaxKind::NotKw {
        return None;
    }
    // Up from `not`, through operators that bind tighter than a comparison,
    // as long as `not` stays leftmost: `not n + 1 < 2` is
    // `((not n) + 1) < 2`.
    let mut node = token.parent()?;
    let comparison = loop {
        let parent = trendy_syntax::ast::BinaryExpr::cast(node.parent()?)?;
        if parent.lhs().is_none_or(|lhs| *lhs.syntax() != node) {
            return None;
        }
        let kind = parent.operator()?.kind();
        if kind.is_comparison() {
            break parent;
        }
        let tighter = matches!(
            kind,
            trendy_syntax::SyntaxKind::Plus
                | trendy_syntax::SyntaxKind::Minus
                | trendy_syntax::SyntaxKind::Star
                | trendy_syntax::SyntaxKind::Slash
                | trendy_syntax::SyntaxKind::DivKw
                | trendy_syntax::SyntaxKind::ModKw
                | trendy_syntax::SyntaxKind::PipePipe
        );
        if !tighter {
            return None;
        }
        node = parent.syntax().clone();
    };
    let start = operand.span().start;
    let end = comparison.span().end;
    let whole = comparison.syntax().text().to_string();
    let compared = &whole[start - comparison.span().start..];
    Some((
        format!("negate the comparison: `not ({compared})`"),
        vec![
            Edit {
                span: Span::empty(start),
                replacement: "(".to_string(),
            },
            Edit {
                span: Span::empty(end),
                replacement: ")".to_string(),
            },
        ],
    ))
}

/// A literal of type `ty` for the same value as `value`, written `written`,
/// when there is an obvious one: `1` as a rational is `1.0`, and `"5"` as an
/// integer is `5`.
fn rewrite_literal(ty: hir::Scalar, value: &hir::Value, written: &str) -> Option<String> {
    use trendy_syntax::literal;
    match (ty, value) {
        (
            hir::Scalar::Text,
            hir::Value::Boolean(_) | hir::Value::Integer(_) | hir::Value::Rational(_),
        ) => Some(format!("\"{written}\"")),
        // `0` and `1` are the usual spellings of false and true elsewhere.
        (hir::Scalar::Boolean, hir::Value::Integer(n @ (0 | 1))) => Some((*n == 1).to_string()),
        (hir::Scalar::Boolean, hir::Value::Text(text)) => ["true", "false"]
            .contains(&text.as_str())
            .then(|| text.clone()),
        (hir::Scalar::Rational, hir::Value::Integer(_)) => Some(format!("{written}.0")),
        (hir::Scalar::Integer, hir::Value::Rational(r)) => {
            // Only exact integers within range; `{:.0}` prints them in full.
            let r = r.get();
            let exact = r.fract() == 0.0 && r.abs() < 2_f64.powi(63);
            exact.then(|| format!("{r:.0}"))
        }
        (hir::Scalar::Integer, hir::Value::Text(text)) => {
            (!text.is_empty() && literal::check_int(text).is_ok()).then(|| text.clone())
        }
        (hir::Scalar::Rational, hir::Value::Text(text)) => {
            if literal::is_rational(text) && literal::check_rational(text).is_ok() {
                Some(text.clone())
            } else if !text.is_empty() && literal::check_int(text).is_ok() {
                Some(format!("{text}.0"))
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Source text for `value`, if it can be written as a literal.
fn source_text(value: &hir::Value) -> Option<String> {
    Some(match value {
        hir::Value::Boolean(b) => b.to_string(),
        hir::Value::Integer(n) => n.to_string(),
        hir::Value::Rational(r) => {
            let r = r.get();
            // Debug formatting always includes a point or an exponent.
            r.is_finite().then(|| format!("{r:?}"))?
        }
        hir::Value::Text(text) => format!("\"{}\"", trendy_syntax::literal::escape_text(text)),
        hir::Value::Tuple(tuple) => {
            let attributes = tuple
                .attributes()
                .map(|(name, v)| Some(format!("{name}: {}", source_text(v)?)))
                .collect::<Option<Vec<_>>>()?;
            if attributes.is_empty() {
                "tuple {}".to_string()
            } else {
                format!("tuple {{ {} }}", attributes.join(", "))
            }
        }
        // With its heading first, so that even an empty relation is
        // self-describing.
        hir::Value::Relation(relation) => {
            let tuples = relation
                .tuples
                .iter()
                .map(|t| {
                    source_text(&hir::Value::Tuple(t.clone()))
                        .map(|text| text.trim_start_matches("tuple ").to_string())
                })
                .collect::<Option<Vec<_>>>()?;
            if tuples.is_empty() {
                format!("relation {} {{}}", relation.heading)
            } else {
                format!("relation {} {{ {} }}", relation.heading, tuples.join(", "))
            }
        }
    })
}

/// What a list of `name: value` items is: a tuple's attributes, or a call's
/// arguments. They follow the same rules, and errors say which they are.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Noun {
    Attribute,
    Argument,
    /// An attribute named in a projection or `remove`, which must be one of
    /// its operand's.
    Projected,
}

impl Noun {
    fn one(self) -> &'static str {
        match self {
            Noun::Attribute | Noun::Projected => "attribute",
            Noun::Argument => "argument",
        }
    }

    fn many(self) -> &'static str {
        match self {
            Noun::Attribute | Noun::Projected => "attributes",
            Noun::Argument => "arguments",
        }
    }
}

/// An attribute or argument named `name`, at `span`, appearing more than
/// once, first at `first`.
pub(crate) fn duplicate_attribute(noun: Noun, name: &str, span: Span, first: Span) -> Diagnostic {
    let set = match noun {
        Noun::Attribute => "a tuple's attributes are a set, so each name can appear only once",
        Noun::Projected => "each attribute is named once",
        Noun::Argument => "arguments are a tuple of the parameters, so each one is given once",
    };
    Diagnostic::error(
        Code::DuplicateAttribute,
        format!("{} `{name}` is given more than once", noun.one()),
        span,
        "given again here",
    )
    .with_secondary(first, format!("`{name}` is first given here"))
    .with_note(set)
    .with_help("rename or remove the duplicate")
}

/// A tuple value or argument list, at `at`, without the `missing`
/// attributes or arguments, each with its type (`None` for `any`). `context`
/// is a note on what was expected; `insertion` is where new items go.
pub(crate) fn missing_attributes(
    noun: Noun,
    missing: &[(&str, Option<&hir::Type>)],
    context: &str,
    at: Span,
    insertion: (usize, &str, &str),
) -> Diagnostic {
    let names: Vec<_> = missing
        .iter()
        .map(|(name, _)| format!("`{name}`"))
        .collect();
    let what = if missing.len() == 1 {
        noun.one()
    } else {
        noun.many()
    };
    let d = Diagnostic::error(
        Code::MissingAttribute,
        format!("missing {what} {}", trendy_diag::conjoin(&names)),
        at,
        format!("no {}", trendy_diag::conjoin(&names)),
    )
    .with_note(context.to_string())
    .with_note(match noun {
        Noun::Attribute | Noun::Projected => "a tuple value must have every attribute of its type",
        Noun::Argument => "every parameter needs an argument",
    });

    // Defaults make the fix; an `any` parameter has none.
    let items = missing
        .iter()
        .map(|(name, ty)| Some(format!("{name}: {}", source_text(&(*ty)?.default_value())?)))
        .collect::<Option<Vec<_>>>();
    let Some(items) = items else {
        return d.with_help(format!("give {}", trendy_diag::conjoin(&names)));
    };
    let (offset, before, after) = insertion;
    d.with_suggestion(
        format!("add {} with default values", trendy_diag::conjoin(&names)),
        vec![Edit {
            span: Span::empty(offset),
            replacement: format!("{before}{}{after}", items.join(", ")),
        }],
    )
}

/// An attribute or argument `name`, at `span`, that isn't expected.
/// `context` is a note on what was expected; `rename` is the missing item it
/// was probably meant to be, and `removal` the span that removes it, if
/// removing it is a fix.
pub(crate) fn unknown_attribute(
    noun: Noun,
    name: &str,
    span: Span,
    context: &str,
    rename: Option<&str>,
    removal: Option<Span>,
) -> Diagnostic {
    let label = match noun {
        Noun::Attribute => "not an attribute of the expected type",
        Noun::Projected => "not an attribute of the operand",
        Noun::Argument => "not a parameter of this operator",
    };
    let mut d = Diagnostic::error(
        Code::UnknownAttribute,
        format!("unknown {} `{name}`", noun.one()),
        span,
        label,
    )
    .with_note(context.to_string());
    if let Some(target) = rename {
        d = d.with_suggestion(
            format!("rename it to `{target}`"),
            vec![Edit {
                span,
                replacement: target.to_string(),
            }],
        );
    }
    match removal {
        Some(removal) => d.with_suggestion(
            format!("remove `{name}`"),
            vec![Edit {
                span: removal,
                replacement: String::new(),
            }],
        ),
        None => d,
    }
}

/// `builtin …`, the declaration `decl` with `keyword`, outside the prelude.
pub(crate) fn builtin_outside_prelude(
    keyword: Option<SyntaxToken>,
    decl: &trendy_syntax::SyntaxNode,
) -> Diagnostic {
    let span = keyword.map_or_else(
        || trendy_syntax::span(decl.text_range()),
        |k| trendy_syntax::span(k.text_range()),
    );
    Diagnostic::error(
        Code::BuiltinOutsidePrelude,
        "built-ins can only be declared in the prelude",
        span,
        "not allowed in a program",
    )
    .with_note("the prelude declares the types and operators the compiler implements, such as `integer` and `debug_value`; they are always in scope")
    .with_help("to define an operator, remove `builtin` and give it a body: `operator name{…} [ … ];`")
}

/// The reserved type name `any`, at `span`, used outside a built-in
/// operator's parameters.
pub(crate) fn any_outside_builtin(span: Span) -> Diagnostic {
    Diagnostic::error(
        Code::AnyOutsideBuiltin,
        "`any` is only for the parameters of built-in operators",
        span,
        "not a type that values or variables can have",
    )
    .with_note("`any` lets a built-in operator such as `debug_value` take a value of every type")
    .with_help("use a specific type, such as `integer`, `text` or `tuple { name: text }`")
}

/// A name `name`, at `span`, that refers to nothing. `kind` is what was
/// looked for ("value" or "operator"), and `candidates` the names in scope.
pub(crate) fn unknown_name(name: &str, span: Span, kind: &str, candidates: &[&str]) -> Diagnostic {
    let d = Diagnostic::error(
        Code::UnknownName,
        format!("unknown {kind} `{name}`"),
        span,
        "not defined",
    );
    let closest = candidates
        .iter()
        .filter(|c| edit_distance(c, name) <= 2)
        .min_by_key(|c| edit_distance(c, name));
    match closest {
        Some(closest) => d.with_suggestion(
            format!("did you mean `{closest}`?"),
            vec![Edit {
                span,
                replacement: (*closest).to_string(),
            }],
        ),
        None => d.with_help(match kind {
            "operator" => "define the operator, or call one that exists, such as `debug_value`",
            _ => "declare a variable with this name, or use a parameter",
        }),
    }
}

/// The operator `name`, at `span`, used as a value.
pub(crate) fn operator_as_value(name: &str, span: Span) -> Diagnostic {
    Diagnostic::error(
        Code::OperatorAsValue,
        format!("`{name}` is an operator, not a value"),
        span,
        "operators can't be used as values",
    )
    .with_help(format!(
        "to invoke it, write a statement: `call {name}{{…}};`"
    ))
}

/// `call name{…}`, at `span`, where `name` is `what`, not an operator.
pub(crate) fn not_an_operator(name: &str, span: Span, what: &str) -> Diagnostic {
    Diagnostic::error(
        Code::NotAnOperator,
        format!("`{name}` is {what}, not an operator"),
        span,
        "can't be called",
    )
    .with_note("`call` invokes an update operator, such as `debug_value`")
}

/// An argument without a name, at `span`, in a call to `signature`.
/// `only_param` is the name it must have, when that is certain.
pub(crate) fn positional_argument(
    span: Span,
    only_param: Option<&str>,
    signature: &str,
) -> Diagnostic {
    let d = Diagnostic::error(
        Code::PositionalArgument,
        "arguments must be named",
        span,
        "no parameter name",
    )
    .with_note(format!(
        "arguments are a tuple of the operator's parameters, so each is written `name: value`; the parameters are {signature}"
    ));
    match only_param {
        Some(param) => d.with_suggestion(
            format!("name it `{param}`"),
            vec![Edit {
                span: Span::empty(span.start),
                replacement: format!("{param}: "),
            }],
        ),
        None => d.with_help("write each argument as `name: value`"),
    }
}

/// `operator main{…}`, whose parameters `params` must be empty.
pub(crate) fn main_with_params(params: &trendy_syntax::ast::Heading) -> Diagnostic {
    Diagnostic::error(
        Code::MainWithParams,
        "`main` can't take parameters",
        params.span(),
        "nothing passes arguments to `main`",
    )
    .with_note("execution starts in `main`, with no arguments")
    .with_suggestion(
        "remove the parameters",
        vec![Edit {
            span: params.span(),
            replacement: "{}".to_string(),
        }],
    )
}

/// A projection, or the `operator` such as `remove`, written at `at`,
/// applied to a value of type `ty`, which isn't a tuple or relation.
pub(crate) fn not_projectable(
    operator: Option<&str>,
    at: Span,
    ty: &hir::Type,
    operand: &Expr,
) -> Diagnostic {
    let (message, label) = match operator {
        Some(operator) => (
            format!("`{operator}` can't be applied to {}", plural(ty)),
            format!("`{operator}` takes a tuple or relation"),
        ),
        None => (
            format!("{} can't be projected", plural(ty)),
            "a projection takes a tuple or relation".to_string(),
        ),
    };
    Diagnostic::error(Code::WrongOperandType, message, at, label)
        .with_secondary(operand.span(), format!("this is `{ty}`"))
        .with_note(
            "a projection, `r{a, b}`, `r remove {c}` and `r rename {a as b}` work on the attributes of a tuple or relation",
        )
}

/// `.name`, at `at`, applied to a value of type `ty`, which isn't a tuple.
pub(crate) fn not_extractable(name: &str, at: Span, ty: &hir::Type, operand: &Expr) -> Diagnostic {
    let d = Diagnostic::error(
        Code::WrongOperandType,
        format!("`.{name}` can't be applied to {}", plural(ty)),
        at,
        "only a tuple has an attribute's value",
    )
    .with_secondary(operand.span(), format!("this is `{ty}`"));
    match ty {
        hir::Type::Relation(_) => d
            .with_note("a relation can have many tuples, so no one value for an attribute")
            .with_help(format!(
                "to keep only the attribute, project on it: `r{{{name}}}`"
            )),
        _ => d.with_note("`t.name` is the value of the attribute `name` of the tuple `t`"),
    }
}

/// `rename {}`, written `postfix` after `operand`, which renames nothing.
pub(crate) fn empty_rename(
    postfix: &trendy_syntax::ast::PostfixExpr,
    operand: &Expr,
) -> Diagnostic {
    let end = postfix.span().end;
    let keyword = postfix.operator().map_or(operand.span().end, |t| {
        trendy_syntax::span(t.text_range()).start
    });
    Diagnostic::error(
        Code::EmptyRemove,
        "`rename {}` renames nothing",
        Span::new(keyword, end),
        "no renamings",
    )
    .with_note("`rename` takes renamings, each `old as new`; with none it would change nothing")
    .with_suggestion(
        "drop the `rename {}`",
        vec![Edit {
            span: Span::new(operand.span().end, end),
            replacement: String::new(),
        }],
    )
}

/// The renaming `before as after`, the wrong way around: `before` isn't an
/// attribute, but `after` is. `context` names the attributes.
pub(crate) fn reversed_renaming(
    (before, before_span): (&str, Span),
    (after, after_span): (&str, Span),
    context: &str,
) -> Diagnostic {
    Diagnostic::error(
        Code::UnknownAttribute,
        format!("unknown attribute `{before}`"),
        before_span,
        "not an attribute of the operand",
    )
    .with_secondary(after_span, format!("but `{after}` is"))
    .with_note(context.to_string())
    .with_note("a renaming is `old as new`: the attribute's name now comes first")
    .with_suggestion(
        format!("swap them: `{after} as {before}`"),
        vec![
            Edit {
                span: before_span,
                replacement: after.to_string(),
            },
            Edit {
                span: after_span,
                replacement: before.to_string(),
            },
        ],
    )
}

/// A renaming to `name`, at `span`, which is taken: by an attribute that
/// keeps its name, or by an earlier renaming's new name, at `earlier`.
pub(crate) fn attribute_exists(
    name: &str,
    span: Span,
    earlier: Option<Span>,
    context: &str,
) -> Diagnostic {
    let d = Diagnostic::error(
        Code::AttributeExists,
        format!("attribute `{name}` already exists"),
        span,
        "this name is taken",
    );
    let d = match earlier {
        Some(earlier) => d.with_secondary(earlier, format!("`{name}` is the new name here too")),
        None => d.with_note(context.to_string()),
    };
    d.with_note(
        "attributes are a set, so names are unique; a new name must be free, unless the attribute that has it is renamed too",
    )
    .with_help("choose a free name, or rename the other attribute in the same `rename`")
}

/// `remove {}`, written `postfix` after `operand`, which removes nothing.
pub(crate) fn empty_remove(
    postfix: &trendy_syntax::ast::PostfixExpr,
    operand: &Expr,
) -> Diagnostic {
    let end = postfix.span().end;
    let keyword = postfix.operator().map_or(operand.span().end, |t| {
        trendy_syntax::span(t.text_range()).start
    });
    Diagnostic::error(
        Code::EmptyRemove,
        "`remove {}` removes nothing",
        Span::new(keyword, end),
        "no attributes named",
    )
    .with_note("`remove` takes the attributes to drop; with none it would change nothing")
    .with_help("to keep no attributes at all, project on none: `r{}`")
    .with_suggestion(
        "drop the `remove {}`",
        vec![Edit {
            span: Span::new(operand.span().end, end),
            replacement: String::new(),
        }],
    )
}

/// The variables in `cycle`, each with its declaration's name span and the
/// span where its initial value refers to the next one, the last referring
/// to the first.
pub(crate) fn cyclic_initial_values(cycle: &[(String, Span, Span)]) -> Diagnostic {
    let (first, declared, _) = &cycle[0];
    let names: Vec<String> = cycle.iter().map(|(n, _, _)| format!("`{n}`")).collect();
    let message = match names.as_slice() {
        [_] => format!("the initial value of `{first}` depends on itself"),
        _ => format!(
            "the initial values of {} depend on each other",
            trendy_diag::conjoin(&names)
        ),
    };
    let mut d = Diagnostic::error(
        Code::CyclicInitialValues,
        message,
        *declared,
        "declared here",
    );
    for (i, (name, _, via)) in cycle.iter().enumerate() {
        let next = &cycle[(i + 1) % cycle.len()].0;
        let label = if next == name {
            format!("`{name}` refers to itself")
        } else {
            format!("`{name}` refers to `{next}`")
        };
        d = d.with_secondary(*via, label);
    }
    d.with_note(
        "initial values are evaluated when compiling, each after the ones it refers to, so a cycle has no order to evaluate them in",
    )
    .with_help("give one of these variables a value that doesn't refer to the others")
}

/// A tuple at `span` that the relation already has, first at `first`.
/// `removal` is the span that removes it.
pub(crate) fn duplicate_tuple(span: Span, first: Span, removal: Span) -> Diagnostic {
    Diagnostic::error(
        Code::DuplicateTuple,
        "duplicate tuple in a relation value",
        span,
        "this tuple is already in the relation",
    )
    .with_secondary(first, "first listed here")
    .with_note("a relation is a set of tuples, so each tuple can appear only once")
    .with_help("if a different tuple was meant, change this one; otherwise remove it")
    .with_suggestion(
        "remove the duplicate",
        vec![Edit {
            span: removal,
            replacement: String::new(),
        }],
    )
}

/// A relation value at `span` with no tuples and nothing else giving its
/// heading.
pub(crate) fn unknown_relation_heading(span: Span) -> Diagnostic {
    Diagnostic::error(
        Code::UnknownRelationHeading,
        "the heading of this empty relation is unknown",
        span,
        "no tuples, no heading, and no expected type",
    )
    .with_note("a relation's heading comes from its selector's heading, from its tuples, or from the type expected of it")
    .with_help("write the heading, as in `relation { name: text } {}`, or declare the variable's type, as in `var r: relation { name: text } := relation {};`")
}

pub(crate) fn rational_out_of_range(span: Span) -> Diagnostic {
    Diagnostic::error(
        Code::RationalOutOfRange,
        "rational literal is too large",
        span,
        "too large for a 64-bit floating-point number",
    )
    .with_note(format!(
        "`rational` is a 64-bit IEEE 754 binary floating-point number: the largest is about {:e}",
        f64::MAX
    ))
}

pub(crate) fn integer_out_of_range(span: Span) -> Diagnostic {
    Diagnostic::error(
        Code::IntegerOutOfRange,
        "integer literal is too large",
        span,
        "doesn't fit in a 64-bit integer",
    )
    .with_note(format!(
        "integers are 64-bit signed, from {} to {}",
        i64::MIN,
        i64::MAX
    ))
}

/// An operator written `expr`, whose result would be a `result`, that has
/// no result because of `failure`. `divisor` is its right operand, where a
/// division by zero is reported.
pub(crate) fn failure(
    failure: hir::Failure,
    result: hir::Scalar,
    expr: &Expr,
    divisor: Option<&Expr>,
) -> Diagnostic {
    match (failure, result) {
        (hir::Failure::Overflow, hir::Scalar::Rational) => Diagnostic::error(
            Code::Overflow,
            "rational overflow",
            expr.span(),
            "the result is too large for a rational",
        )
        .with_note(format!(
            "rationals are finite, up to about {:e} in magnitude; a larger result is an error, not an infinity",
            f64::MAX
        )),
        (hir::Failure::Overflow, _) => Diagnostic::error(
            Code::Overflow,
            "integer overflow",
            expr.span(),
            "the result doesn't fit in a 64-bit integer",
        )
        .with_note(format!(
            "integers are 64-bit signed, from {} to {}; a result outside that range is an error, not wrapped around",
            i64::MIN,
            i64::MAX
        )),
        (hir::Failure::DivisionByZero, _) => Diagnostic::error(
            Code::DivisionByZero,
            "division by zero",
            divisor.map_or(expr.span(), Expr::span),
            "this is zero",
        )
        .with_note("`/`, `div` and `mod` have no result when the divisor is zero"),
    }
}

/// How a type's values are named in the plural.
fn plural(ty: &hir::Type) -> &'static str {
    match ty {
        hir::Type::Scalar(hir::Scalar::Boolean) => "booleans",
        hir::Type::Scalar(hir::Scalar::Integer) => "integers",
        hir::Type::Scalar(hir::Scalar::Rational) => "rationals",
        hir::Type::Scalar(hir::Scalar::Text) => "text",
        hir::Type::Tuple(_) => "tuples",
        hir::Type::Relation(_) => "relations",
    }
}

/// The operator `op`, written `token`, applied to operands of type `ty`,
/// which it doesn't take.
pub(crate) fn wrong_operand_type(
    op: crate::ops::Operator,
    token: &SyntaxToken,
    ty: &hir::Type,
) -> Diagnostic {
    use crate::ops::Operator;
    let span = trendy_syntax::span(token.text_range());
    let d = Diagnostic::error(
        Code::WrongOperandType,
        format!("`{}` can't be applied to {}", token.text(), plural(ty)),
        span,
        format!("`{}` takes {}", token.text(), op.takes()),
    )
    .with_note("values never convert to another type implicitly");
    let hir::Type::Scalar(scalar) = *ty else {
        return match op {
            Operator::Compare(_) => d.with_note(
                "tuples and relations have no order; they can only be compared with `=` and `<>`",
            ),
            _ => d,
        };
    };
    let replace = |d: Diagnostic, with: &str, message: &str| {
        // A keyword needs spaces where the operator had none, as in `a/b`.
        let word = with.starts_with(|c: char| c.is_ascii_alphabetic());
        let tight = |t: Option<SyntaxToken>| t.is_some_and(|t| !t.kind().is_trivia());
        let mut replacement = with.to_string();
        if word && tight(token.prev_token()) {
            replacement.insert(0, ' ');
        }
        if word && tight(token.next_token()) {
            replacement.push(' ');
        }
        d.with_suggestion(message, vec![Edit { span, replacement }])
    };
    match (op, scalar) {
        (Operator::Divide, hir::Scalar::Integer) => replace(
            d.with_note(
                "`/` divides rationals; integer division is `div`, which truncates toward zero",
            ),
            "div",
            "use integer division: `div`",
        ),
        (Operator::Div, hir::Scalar::Rational) => replace(
            d.with_note("`div` is integer division; rationals divide with `/`"),
            "/",
            "use rational division: `/`",
        ),
        (Operator::Mod, hir::Scalar::Rational) => d.with_note(
            "`mod` is the remainder of integer division; rationals have no remainder operator",
        ),
        (Operator::Concatenate, hir::Scalar::Boolean) => replace(
            d.with_note("`||` concatenates text; the boolean operator is `or`"),
            "or",
            "use `or`",
        ),
        (Operator::Add, hir::Scalar::Text) => replace(
            d.with_note("`+` adds numbers; text is concatenated with `||`"),
            "||",
            "concatenate with `||`",
        ),
        _ => d,
    }
}

/// A declaration of `name`, at `span`, with neither a type nor a value.
pub(crate) fn untyped_variable(name: &str, span: Span) -> Diagnostic {
    Diagnostic::error(
        Code::UntypedVariable,
        format!("variable `{name}` has neither a type nor an initial value"),
        span,
        "needs a type, a value, or both",
    )
    .with_note("without a value, a variable starts at its type's default; without a type, it takes its value's type")
    .with_help(format!(
        "give a type, as in `var {name}: integer;`, or a value, as in `var {name} := 0;`"
    ))
}
