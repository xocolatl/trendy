//! Sema's diagnostics. Each one says where the fix goes and, when the fix is
//! mechanical, carries it.

use std::collections::BTreeSet;

use trendy_diag::{Code, Diagnostic, Edit, Span, edit_distance};
use trendy_hir as hir;
use trendy_syntax::SyntaxToken;
use trendy_syntax::ast::{
    AstNode, Expr, Key, NaryExpr, ProjectExpr, SourceFile, TypeExpr, VarDecl,
};

use crate::globals::Pending;
use crate::operators::Signature;
use crate::types::{Origin, TypeParameters};
use crate::{Kind, Namespaces};

/// Type names from other languages, and the built-in type each one means.
const TYPE_ALIASES: &[(&str, hir::Scalar)] = &[
    ("bigint", hir::Scalar::Integer),
    ("bit", hir::Scalar::Boolean),
    ("bool", hir::Scalar::Boolean),
    ("char", hir::Scalar::Text),
    ("decimal", hir::Scalar::Approximate),
    ("double", hir::Scalar::Approximate),
    ("f32", hir::Scalar::Approximate),
    ("f64", hir::Scalar::Approximate),
    ("float", hir::Scalar::Approximate),
    ("float64", hir::Scalar::Approximate),
    ("i64", hir::Scalar::Integer),
    ("int", hir::Scalar::Integer),
    ("int64", hir::Scalar::Integer),
    ("long", hir::Scalar::Integer),
    ("number", hir::Scalar::Approximate),
    ("numeric", hir::Scalar::Approximate),
    ("real", hir::Scalar::Approximate),
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
    let note = match this.0 {
        Kind::Variable => {
            "variables have a namespace of their own, so no two can have the same name, though a variable can have an operator's"
        }
        _ => {
            "operators have a namespace of their own, so no two can have the same name, though an operator can have a variable's"
        }
    };
    d.with_note(note)
        .with_help("delete the duplicate definition, or rename one of them")
}

/// What a local variable's name must be.
const LOCAL_NAMES: &str = "an operator's names, its parameters' and its local variables', each mean one thing in all of its body, so no two are the same; they hide the program's variables of their names";

/// The local variable `name`, declared in the body of the operator `op` by
/// `decl`, which `first` declares already.
pub(crate) fn local_declared_again(
    name: &str,
    decl: &VarDecl,
    first: &VarDecl,
    op: &str,
) -> Diagnostic {
    let named = |decl: &VarDecl| decl.name().map_or_else(|| decl.span(), |n| n.span());
    let d = Diagnostic::error(
        Code::DuplicateName,
        format!("`{name}` is declared again in `{op}`"),
        named(decl),
        "declared again here",
    )
    .with_secondary(named(first), format!("`{name}` is first declared here"))
    .with_note(LOCAL_NAMES);
    // `var total := total + 1;`, as Rust declares a variable again to give
    // it a new value: the first one's assignment was meant.
    match (
        decl.name(),
        decl.type_expr(),
        decl.initializer(),
        decl.keys().next(),
    ) {
        (Some(new), None, Some(_), None) => d.with_suggestion(
            format!("assign the first one instead: `{name} := …;`"),
            vec![Edit {
                span: Span::new(decl.span().start, new.span().start),
                replacement: String::new(),
            }],
        ),
        _ => d.with_help(format!(
            "give the second a name of its own, or assign the first: `{name} := …;`"
        )),
    }
}

/// A local variable `name`, declared at `span` in the body of the operator
/// `op`, whose parameter written at `parameter` has its name.
pub(crate) fn local_named_as_parameter(
    name: &str,
    span: Span,
    parameter: Option<Span>,
    op: &str,
) -> Diagnostic {
    let d = Diagnostic::error(
        Code::DuplicateName,
        format!("`{name}` is a parameter of `{op}` already"),
        span,
        "a local variable with a parameter's name",
    );
    let d = match parameter {
        Some(parameter) => d.with_secondary(parameter, format!("the parameter `{name}`")),
        None => d,
    };
    d.with_note(LOCAL_NAMES).with_help(format!(
        "a parameter keeps its value while `{op}` runs, so a value computed from it needs a name of its own, as in `var new_{name} := …;`"
    ))
}

/// A local variable `name`, used at `span`, before `decl` declares it.
pub(crate) fn local_before_declaration(name: &str, span: Span, decl: &VarDecl) -> Diagnostic {
    let declared = decl.span();
    let named = decl.name().map_or(declared, |n| n.span());
    // `var n := n + 1;`, as Rust declares a variable from one it hides.
    let own = declared.start <= span.start && span.end <= declared.end;
    let (label, help) = if own {
        (
            "declared here, with a value computed from it",
            format!(
                "to compute it from a variable of the program named `{name}`, give the local variable a name of its own"
            ),
        )
    } else {
        (
            "declared here, after it is used",
            format!("declare `{name}` before it is used, or give it a name of its own"),
        )
    };
    Diagnostic::error(
        Code::LocalBeforeDeclaration,
        format!("local variable `{name}` is used before its declaration"),
        span,
        "used here",
    )
    .with_secondary(named, label)
    .with_note("a local variable is in scope in all of its operator's body, so that its name means one thing there, hiding any variable of the program's, but it has a value only once its declaration gives it one")
    .with_help(help)
}

/// A variable `name`, of type `ty`, written `type_expr`, which has a type
/// parameter in it, without an initial value.
pub(crate) fn no_default_value(name: &str, ty: &hir::Type, type_expr: &TypeExpr) -> Diagnostic {
    Diagnostic::error(
        Code::UntypedVariable,
        format!("variable `{name}`, of type `{ty}`, has no initial value"),
        type_expr.span(),
        "a type without a default",
    )
    .with_note("without a value, a variable starts at its type's default, but each call gives a type parameter its type, so a type with one in it has no default")
    .with_help(format!("give it a value: `var {name}: {ty} := …;`"))
}

pub(crate) fn missing_main(file: &SourceFile, namespaces: &Namespaces) -> Diagnostic {
    const MESSAGE: &str = "program has no `main` operator";
    const ENTRY_NOTE: &str = "execution starts in the operator named `main`";

    if let Some(span) = namespaces.variable("main") {
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
    let near_miss = namespaces
        .operators()
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
/// Names other languages give a type that values of any type have:
/// TypeScript's, Go's and Python's `any`, and the polymorphic pseudo-types
/// of Postgres.
const ANY_ALIASES: &[&str] = &["any", "anycompatible", "anyelement", "anynonarray"];

/// The type named `ident`, which isn't one, written where `parameters` are
/// the type parameters it could name.
pub(crate) fn unknown_type(
    ident: &SyntaxToken,
    namespaces: &Namespaces,
    parameters: &TypeParameters,
) -> Diagnostic {
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
    let declared: Vec<&str> = parameters
        .list()
        .iter()
        .filter_map(|p| p.name.as_deref())
        .collect();
    if !declared.is_empty() {
        let listed: Vec<String> = declared.iter().map(|p| format!("`{p}`")).collect();
        d = d.with_note(format!(
            "the operator's type parameters are {}",
            trendy_diag::conjoin(&listed)
        ));
    }
    let kind = namespaces
        .variable(name)
        .map(|_| Kind::Variable)
        .or_else(|| namespaces.operator(name).map(|(kind, _)| kind));
    if let Some(kind) = kind {
        return d.with_note(format!("`{name}` is {}, not a type", kind.describe()));
    }

    let lowercase = name.to_lowercase();
    if ANY_ALIASES.contains(&lowercase.as_str()) {
        return match parameters {
            TypeParameters::Open(_) => d
                .with_note("a parameter that takes a value of any type has the type `any type`, and each call gives it the type of its argument")
                .with_suggestion(
                    "write `any type`",
                    vec![Edit {
                        span,
                        replacement: "any type".to_string(),
                    }],
                ),
            TypeParameters::Fixed(_) => d.with_note(ANY_ONLY_IN_PARAMETERS).with_help(ANY_ELSEWHERE),
            TypeParameters::None => d.with_note(ANY_ONLY_IN_PARAMETERS).with_note(VARIABLE_TYPES),
        };
    }
    let written = |n: &str| (n.to_string(), n.to_string());
    let meant = TYPE_ALIASES
        .iter()
        .find(|(alias, _)| *alias == lowercase)
        .map(|&(_, ty)| written(ty.name()))
        .or_else(|| {
            let types: Vec<&str> = hir::Scalar::ALL
                .iter()
                .map(|t| t.name())
                .chain(declared.iter().copied())
                .collect();
            types
                .into_iter()
                .filter(|t| edit_distance(t, name) <= 2)
                .min_by_key(|t| edit_distance(t, name))
                .map(written)
        });
    if let Some((meant, replacement)) = meant {
        if TYPE_ALIASES.iter().any(|(alias, _)| *alias == lowercase) {
            d = d.with_note(format!("trendy's name for this type is `{meant}`"));
        }
        d = d.with_suggestion(format!("use `{meant}`"), vec![Edit { span, replacement }]);
    }
    d
}

const ANY_ONLY_IN_PARAMETERS: &str = "only a parameter's type can be `any type`, `any tuple` or `any relation`: each call gives it the type of its argument, and nothing would give one written anywhere else a type";
const ANY_ELSEWHERE: &str = "to use the type of an argument here, declare a type parameter after the signature, `types { T: any type }`, and write `T` both here and as the parameter's type";
const VARIABLE_TYPES: &str =
    "a variable's type is always a specific type, such as `integer` or `relation { name: text }`";

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
    for (span, label) in origin.labels() {
        d = d.with_secondary(span, label);
    }
    let unnamed = |ty: &hir::Type| ty.parameters().any(|p| p.name.is_none());
    if unnamed(declared) || unnamed(found) {
        d = d.with_note("each `any type`, `any tuple` or `any relation` written as a parameter's type is a type of its own; to make parameters' types the same, declare one type parameter after the signature, `types { T: any type }`, and write `T` in each");
    }
    // `x + 1` of a `T`, as generic code in other languages can write it.
    if let hir::Type::Parameter(p @ hir::TypeParameter { name: Some(_), .. }) = declared
        && !matches!(found, hir::Type::Parameter(_))
    {
        d = d.with_note(format!(
            "`{p}` is whatever type each call gives it, so a value of a specific type, such as `{found}`, is never a `{p}`: values of `{p}` come from the arguments"
        ));
    }

    // `not n = 0`: `not` binds tightest, so this negates `n`; the comparison
    // was meant.
    if let Some((message, edits)) = negated_comparison(origin, expr) {
        d = d.with_suggestion(message, edits);
    }
    // Rewriting the literal is the likelier fix when it is mechanical, so it
    // comes first, and then converting the value; changing the declared type
    // comes last.
    let rewritten = match (declared, written_literal(expr), value) {
        (hir::Type::Scalar(scalar), Some(written), Some(value)) => {
            rewrite_literal(*scalar, value, &written)
        }
        _ => None,
    };
    if let Some(replacement) = rewritten {
        d = d.with_suggestion(
            format!("write it as `{declared}`: `{replacement}`"),
            vec![Edit {
                span: expr.span(),
                replacement,
            }],
        );
    } else if let Some(builtin) = conversion(found, declared) {
        d = with_conversion(d, "it", builtin, expr);
    }
    // `n * x`, of an integer and an approximate number: the first operand
    // set the type, and as an approximate number it takes the rest.
    if let Origin::Value(first) = origin
        && conversion(declared, found) == Some(hir::Builtin::ToApproximate)
    {
        let subject = format!("`{}`", first.syntax().text());
        d = match written_literal(first) {
            Some(written) => d.with_suggestion(
                format!("write {subject} as `{found}`: `{written}e0`"),
                vec![Edit {
                    span: first.span(),
                    replacement: format!("{written}e0"),
                }],
            ),
            None => with_conversion(d, &subject, hir::Builtin::ToApproximate, first),
        };
    }
    // Only a written type can be changed to fit the value, and only the
    // value's own: the other operands of an operator whose result has the
    // type, as `"n = "` in `"n = " || n` returned as `text`, wouldn't fit.
    let (operand, origin) = match origin {
        Origin::Operand(_, within) => (true, &**within),
        origin => (false, origin),
    };
    match origin {
        Origin::Type(type_expr) if !operand => d.with_suggestion(
            format!("change the type to `{found}`"),
            vec![Edit {
                span: type_expr.span(),
                replacement: found.to_string(),
            }],
        ),
        Origin::Replaced(_, name) => d.with_note(format!(
            "`replace` keeps each attribute's type; to change `{name}`'s, compute it under another name with `extend`, then remove the old one and rename the new: `… extend {{ new_{name} := … }} remove {{{name}}} rename {{new_{name} as {name}}}`"
        )),
        // A variable's type is fixed by its declaration: assign a value of
        // that type.
        Origin::TypeParameter(name, _) => d.with_note(format!(
            "the operator's parameters of type `{name}` all take arguments of one type"
        )),
        Origin::Type(_)
        | Origin::Variable(..)
        | Origin::Heading(_)
        | Origin::Value(_)
        | Origin::Builtin
        | Origin::Operator(_)
        | Origin::Operand(..) => d,
    }
}

/// The literal `expr` is, as written, if it is one, or a negative one such
/// as `-1`.
fn written_literal(expr: &Expr) -> Option<String> {
    match expr {
        Expr::Literal(literal) => literal.token().map(|t| t.text().to_string()),
        Expr::Unary(unary) => crate::ops::negated_literal(unary)
            .and_then(|literal| literal.token())
            .map(|t| format!("-{}", t.text())),
        _ => None,
    }
}

/// The prelude's operator that converts every value of type `from` to type
/// `to`, if there is one: `to_text` writes a value of any type as text, and
/// `to_approximate` takes integers.
fn conversion(from: &hir::Type, to: &hir::Type) -> Option<hir::Builtin> {
    use hir::{Scalar, Type};
    match (from, to) {
        (Type::Scalar(Scalar::Integer), Type::Scalar(Scalar::Approximate)) => {
            Some(hir::Builtin::ToApproximate)
        }
        (Type::Scalar(_) | Type::Parameter(_), Type::Scalar(Scalar::Text)) => {
            Some(hir::Builtin::ToText)
        }
        _ => None,
    }
}

/// `d`, with the fix converting `expr`, which the message calls `subject`,
/// with the built-in `builtin`: a call of it around `expr`.
fn with_conversion(d: Diagnostic, subject: &str, builtin: hir::Builtin, expr: &Expr) -> Diagnostic {
    let name = builtin.name();
    let span = expr.span();
    d.with_suggestion(
        format!(
            "convert {subject} with `{name}`: `{name}{{from: {}}}`",
            expr.syntax().text()
        ),
        vec![
            Edit {
                span: Span::empty(span.start),
                replacement: format!("{name}{{from: "),
            },
            Edit {
                span: Span::empty(span.end),
                replacement: "}".to_string(),
            },
        ],
    )
}

/// An argument written `arg`, of type `actual`, for a parameter of type
/// `parameter`, `any tuple` or `any relation`, written as `origin` says,
/// which it isn't.
pub(crate) fn not_of_kind(
    parameter: &hir::TypeParameter,
    origin: &Origin,
    actual: &hir::Type,
    arg: &Expr,
    namespaces: &Namespaces,
) -> Diagnostic {
    let (expected, note) = match &parameter.any {
        hir::Any::Tuple => (
            "a tuple".to_string(),
            format!("a parameter of type `{parameter}` takes a tuple of any heading"),
        ),
        hir::Any::Relation => (
            "a relation".to_string(),
            format!("a parameter of type `{parameter}` takes a relation of any heading"),
        ),
        hir::Any::Kind(kind) => {
            let members: Vec<String> = namespaces
                .members(kind)
                .iter()
                .map(|m| format!("`{}`", m.name()))
                .collect();
            (
                format!("a type of kind `{}`", kind.name),
                format!(
                    "the types of kind `{}` are those that declare it: {}",
                    kind.name,
                    trendy_diag::conjoin(&members)
                ),
            )
        }
        hir::Any::Type => unreachable!("every type is `any type`"),
    };
    let mut d = Diagnostic::error(
        Code::TypeMismatch,
        format!("mismatched types: expected {expected}, found `{actual}`"),
        arg.span(),
        format!("this is `{actual}`"),
    )
    .with_note(note);
    for (span, label) in origin.labels() {
        d = d.with_secondary(span, label);
    }
    // A type parameter of the operator the call is in, which can be types
    // that aren't of the kind: it can be declared as the kind instead.
    if let (hir::Type::Parameter(p), hir::Any::Kind(kind)) = (actual, &parameter.any) {
        let wanted = format!("any {}", kind.name);
        d = d.with_note(format!(
            "`{actual}` is `{}`, which can be a type that doesn't declare `{}`",
            p.any, kind.name
        ));
        d = d.with_help(match &p.name {
            Some(name) => format!("declare `{name}` as `{wanted}`, whose types all declare it"),
            None => {
                format!("write `{wanted}` for the parameter's type, whose types all declare it")
            }
        });
    }
    d
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
/// when there is an obvious one: `1` as an approximate number is `1e0`, and
/// `"5"` as an integer is `5`.
fn rewrite_literal(ty: hir::Scalar, value: &hir::Value, written: &str) -> Option<String> {
    use trendy_syntax::literal;
    match (ty, value) {
        (
            hir::Scalar::Text,
            hir::Value::Boolean(_) | hir::Value::Integer(_) | hir::Value::Approximate(_),
        ) => Some(format!("\"{written}\"")),
        // `0` and `1` are the usual spellings of false and true elsewhere.
        (hir::Scalar::Boolean, hir::Value::Integer(n @ (0 | 1))) => Some((*n == 1).to_string()),
        (hir::Scalar::Boolean, hir::Value::Text(text)) => ["true", "false"]
            .contains(&text.as_str())
            .then(|| text.clone()),
        (hir::Scalar::Approximate, hir::Value::Integer(_)) => Some(format!("{written}e0")),
        (hir::Scalar::Integer, hir::Value::Approximate(r)) => {
            // Only exact integers within range; `{:.0}` prints them in full.
            let r = r.get();
            let exact = r.fract() == 0.0 && r.abs() < 2_f64.powi(63);
            exact.then(|| format!("{r:.0}"))
        }
        (hir::Scalar::Integer, hir::Value::Text(text)) => {
            (!text.is_empty() && literal::check_int(text).is_ok()).then(|| text.clone())
        }
        (hir::Scalar::Approximate, hir::Value::Text(text)) => {
            if literal::is_approximate(text) {
                match literal::check_approximate(text) {
                    Ok(()) => Some(text.clone()),
                    Err(literal::ApproximateProblem::Fixable { fixed, .. }) => Some(fixed),
                    Err(literal::ApproximateProblem::Invalid { .. }) => None,
                }
            } else if !text.is_empty() && literal::check_int(text).is_ok() {
                Some(format!("{text}e0"))
            } else {
                None
            }
        }
        _ => None,
    }
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
    /// An attribute named in a key, which must be one of the relation's.
    Key,
}

impl Noun {
    fn one(self) -> &'static str {
        match self {
            Noun::Attribute | Noun::Projected | Noun::Key => "attribute",
            Noun::Argument => "argument",
        }
    }

    fn many(self) -> &'static str {
        match self {
            Noun::Attribute | Noun::Projected | Noun::Key => "attributes",
            Noun::Argument => "arguments",
        }
    }
}

/// An attribute or argument named `name`, at `span`, appearing more than
/// once, first at `first`.
pub(crate) fn duplicate_attribute(noun: Noun, name: &str, span: Span, first: Span) -> Diagnostic {
    let set = match noun {
        Noun::Attribute => "a tuple's attributes are a set, so each name can appear only once",
        Noun::Projected | Noun::Key => "each attribute is named once",
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
        Noun::Attribute | Noun::Projected | Noun::Key => {
            "a tuple value must have every attribute of its type"
        }
        Noun::Argument => "every parameter needs an argument",
    });

    // Defaults make the fix; an `any` parameter has none.
    let items = missing
        .iter()
        .map(|(name, ty)| Some(format!("{name}: {}", (*ty)?.default_value())))
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

/// `d`, the error for a call without an argument for the `per` parameter
/// `name`, with the argument for `relation` written as `projection`: if
/// that is on one attribute, which was meant as the argument, a fix that
/// gives it as that, inserted as `insertion` says, and `relation` all of
/// the projection's operand.
pub(crate) fn projected_relation(
    d: Diagnostic,
    name: &str,
    relation: &str,
    projection: &ProjectExpr,
    insertion: (usize, &str, &str),
) -> Diagnostic {
    let (Some(operand), Some(names)) = (projection.operand(), projection.names()) else {
        return d;
    };
    let names: Vec<_> = names.names().filter_map(|n| n.ident()).collect();
    let [attribute] = names.as_slice() else {
        return d;
    };
    let (attribute, operand_text) = (attribute.text(), operand.syntax().text().to_string());
    let (offset, before, after) = insertion;
    d.with_note(format!(
        "`{}` has one tuple for each value of `{attribute}`, so tuples with equal values would count once; `{name}` is computed for each tuple of `{relation}`, so every one counts",
        projection.syntax().text()
    ))
    .with_suggestion(
        format!("give `{relation}` all of `{operand_text}`, and `{name}` the attribute: `{name}: {attribute}`"),
        vec![
            Edit {
                span: projection.span(),
                replacement: operand_text,
            },
            Edit {
                span: Span::empty(offset),
                replacement: format!("{before}{name}: {attribute}{after}"),
            },
        ],
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
        Noun::Key => "not an attribute of the relation",
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
/// A kind, declared by `decl` in a program, which only the prelude
/// declares: its types are the ones that declare it, and a program can't
/// declare a type.
pub(crate) fn kind_outside_prelude(decl: &trendy_syntax::ast::KindDecl) -> Diagnostic {
    let span = decl
        .keyword()
        .map_or_else(|| decl.span(), |k| trendy_syntax::span(k.text_range()));
    Diagnostic::error(
        Code::BuiltinOutsidePrelude,
        "kinds can only be declared in the prelude",
        span,
        "not allowed in a program",
    )
    .with_note("a kind's types are those that declare it, as the prelude's built-in types do, and a program can't declare a type, so a kind of its own would have none")
    .with_help("name one of the prelude's kinds after `any`, as in `any ordered`")
}

/// The kind `what`, written after `any`, which isn't one, where
/// `namespaces` has the kinds there are.
pub(crate) fn unknown_kind(what: &SyntaxToken, namespaces: &Namespaces) -> Diagnostic {
    let name = what.text();
    let span = trendy_syntax::span(what.text_range());
    let kinds = namespaces.kinds();
    let listed: Vec<String> = kinds.iter().map(|k| format!("`{}`", k.name)).collect();
    let d = Diagnostic::error(
        Code::UnknownType,
        format!("unknown kind `{name}`"),
        span,
        "not a kind",
    )
    .with_note(format!(
        "`any` is followed by `type`, `tuple`, `relation`, or a kind, the prelude's {}",
        trendy_diag::conjoin(&listed)
    ));
    let words: Vec<&str> = ["type"]
        .into_iter()
        .chain(kinds.iter().map(|k| k.name.as_str()))
        .collect();
    let close = words
        .into_iter()
        .filter(|w| edit_distance(w, name) <= 2)
        .min_by_key(|w| edit_distance(w, name));
    match close {
        Some(close) => d.with_suggestion(
            format!("use `{close}`"),
            vec![Edit {
                span,
                replacement: close.to_string(),
            }],
        ),
        None => d,
    }
}

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

/// The type parameter `name`, declared at `span`, already declared at
/// `first`.
pub(crate) fn duplicate_type_parameter(name: &str, span: Span, first: Span) -> Diagnostic {
    Diagnostic::error(
        Code::DuplicateName,
        format!("type parameter `{name}` is declared more than once"),
        span,
        "declared again here",
    )
    .with_secondary(first, format!("`{name}` is first declared here"))
    .with_note("each type parameter stands for one type, so each has a name of its own")
    .with_help("delete the duplicate, or rename one of them")
}

/// A type parameter declared at `span` with the name `name` of a built-in
/// type, which it would hide.
pub(crate) fn type_parameter_named_as_type(name: &str, span: Span) -> Diagnostic {
    Diagnostic::error(
        Code::DuplicateName,
        format!("`{name}` is already a type"),
        span,
        "a built-in type's name",
    )
    .with_note("a type parameter can't have a type's name, which it would hide in the operator")
    .with_help("rename the type parameter, such as to `T`")
}

/// The type parameter `name` declared as `written`, a specific type rather
/// than `any …`.
pub(crate) fn type_parameter_not_any(name: &str, written: &TypeExpr) -> Diagnostic {
    Diagnostic::error(
        Code::TypeParameterNotAny,
        format!("type parameter `{name}` must be `any type`, `any tuple` or `any relation`"),
        written.span(),
        "a specific type",
    )
    .with_note("a type parameter stands for any type of what it says, which each call gives it from its arguments")
    .with_help(format!(
        "declare `{name}: any type`, or write the specific type where `{name}` is used"
    ))
}

/// The type parameter `name`, declared at `span`, which no parameter's
/// type has, so no call can give it a type. It is used `uses` times
/// elsewhere; with none, `removal` removes it.
pub(crate) fn undetermined_type_parameter(
    name: &str,
    span: Span,
    uses: usize,
    removal: Span,
) -> Diagnostic {
    let rule = "each call gives a type parameter a type from the argument for each parameter whose type has it, so it must be in some parameter's type";
    if uses == 0 {
        return Diagnostic::error(
            Code::UndeterminedTypeParameter,
            format!("type parameter `{name}` is never used"),
            span,
            "declared here",
        )
        .with_note(rule)
        .with_suggestion(
            format!("remove `{name}`"),
            vec![Edit {
                span: removal,
                replacement: String::new(),
            }],
        );
    }
    Diagnostic::error(
        Code::UndeterminedTypeParameter,
        format!("no parameter's type has `{name}`, so a call can't give it a type"),
        span,
        "not in any parameter's type",
    )
    .with_note(rule)
    .with_note("an operator's result type is what its arguments' types make it, so a type parameter only in the result, or in the body, would have no type")
    .with_help(format!(
        "write `{name}` in the type of the parameter whose argument has that type, or use a specific type instead"
    ))
}

/// The type parameter `name`, declared at `declared` as `any`, and written
/// only once, at `only`, a parameter's type. `removal` removes the
/// declaration.
pub(crate) fn type_parameter_named_once(
    name: &str,
    any: &hir::Any,
    only: Span,
    declared: Span,
    removal: Span,
) -> Diagnostic {
    Diagnostic::error(
        Code::TypeParameterNamedOnce,
        format!("type parameter `{name}` is used only once, so it needs no name: write `{any}`"),
        only,
        format!("the only use of `{name}`"),
    )
    .with_secondary(declared, format!("`{name}` is declared here"))
    .with_note("a type parameter's name says that several types are the same type; one used once is written as what it can be, in its place")
    .with_suggestion(
        format!("write `{any}` here, and remove `{name}`"),
        vec![
            Edit {
                span: only,
                replacement: any.to_string(),
            },
            Edit {
                span: removal,
                replacement: String::new(),
            },
        ],
    )
}

/// A call, in the operator `within`, of `called`, written at `span`, which
/// gives `called`'s type parameter `parameter` the type `ty`, built from
/// `within`'s own, and leads back to `within`.
pub(crate) fn growing_recursion(
    within: &str,
    called: &str,
    span: Span,
    parameter: &hir::TypeParameter,
    ty: &hir::Type,
) -> Diagnostic {
    let message = if within == called {
        format!("`{within}` calls itself with a larger type each time")
    } else {
        format!("this call of `{called}` leads back to `{within}` with a larger type each time")
    };
    Diagnostic::error(
        Code::GrowingRecursion,
        message,
        span,
        format!("gives `{parameter}` the type `{ty}`"),
    )
    .with_note("an operator is compiled once for each list of types its type parameters are given, and calls that lead back to it with a type built from its own would give it ever larger ones, which no program can hold")
    .with_help("pass the type parameters on as they are, or give the call specific types")
}

/// `per`, written `per`, in a heading other than an operator's parameters.
pub(crate) fn per_outside_parameters(per: &trendy_syntax::ast::Per) -> Diagnostic {
    Diagnostic::error(
        Code::PerRelation,
        "`per` is only for an operator's parameters",
        per.span(),
        "not after a parameter's type",
    )
    .with_note(PER_RULE)
    .with_suggestion(
        "remove it",
        vec![Edit {
            span: per.removal(),
            replacement: String::new(),
        }],
    )
}

const PER_RULE: &str = "a parameter `p: type per r` takes an argument evaluated for each tuple of the argument for `r`, with that tuple's attributes in scope, as in `where`; inside the operator, `p` is an attribute of `r`'s tuples";

/// Why `per` can't refer to the parameter it names.
pub(crate) enum PerProblem {
    /// It names the parameter it follows.
    Itself,
    /// It names no parameter; these are the operator's.
    NotAParameter(Vec<String>),
    /// It names a `per` parameter, which isn't a relation of its own.
    Per,
    /// It names a parameter of the type written here, not `any relation`.
    NotAnyRelation(TypeExpr),
}

/// `per relation`, after the parameter `name`'s type, which can't refer
/// to it, for `problem`.
pub(crate) fn per_relation(name: &str, relation: &SyntaxToken, problem: PerProblem) -> Diagnostic {
    let span = trendy_syntax::span(relation.text_range());
    let target = relation.text();
    let message = match &problem {
        PerProblem::Itself => format!("`{name}` can't be evaluated per its own tuples"),
        PerProblem::NotAParameter(_) => format!("`{target}` is not a parameter"),
        PerProblem::Per => format!("`{target}` is evaluated per tuple itself, so it has none"),
        PerProblem::NotAnyRelation(_) => {
            format!("`per` refers to a parameter of type `any relation`, but `{target}` isn't one")
        }
    };
    let d = Diagnostic::error(Code::PerRelation, message, span, "after `per`").with_note(PER_RULE);
    match problem {
        PerProblem::Itself | PerProblem::Per => {
            d.with_help("name a parameter of type `any relation` after `per`")
        }
        PerProblem::NotAParameter(params) => {
            let close = params
                .iter()
                .filter(|p| *p != name && edit_distance(p, target) <= 2)
                .min_by_key(|p| edit_distance(p, target));
            match close {
                Some(close) => d.with_suggestion(
                    format!("use `{close}`"),
                    vec![Edit {
                        span,
                        replacement: close.clone(),
                    }],
                ),
                None => d.with_help("name a parameter of type `any relation` after `per`"),
            }
        }
        PerProblem::NotAnyRelation(written) => d
            .with_secondary(written.span(), format!("`{target}` is declared with this type"))
            .with_note("the operator sees the relation's tuples with the `per` parameters as attributes, and their own attributes wrapped, so that they stay distinct but can't clash, as only `any relation` can be")
            .with_help(format!(
                "declare `{target}: any relation`, or, for a relation of a known heading, compute from its attributes in the operator, with `where` or `extend`"
            )),
    }
}

/// The argument written `arg`, of type `actual`, for the parameter `name`,
/// written as `origin` says, which `per` parameters refer to, which isn't
/// a relation whose heading is known.
pub(crate) fn per_relation_argument(
    name: &str,
    origin: &Origin,
    actual: &hir::Type,
    arg: &Expr,
) -> Diagnostic {
    let unknown = matches!(actual, hir::Type::Parameter(p) if p.any == hir::Any::Relation);
    let message = if unknown {
        format!(
            "the attributes of `{actual}` aren't known here, so no `per` argument can be evaluated for its tuples"
        )
    } else {
        format!("mismatched types: expected a relation, found `{actual}`")
    };
    let mut d = Diagnostic::error(Code::TypeMismatch, message, arg.span(), format!("this is `{actual}`"))
        .with_note(format!(
            "`{name}` takes a relation of any heading, whose tuples the `per` arguments are evaluated for"
        ));
    for (span, label) in origin.labels() {
        d = d.with_secondary(span, label);
    }
    d
}

/// The note on a `per` argument that is the same for every tuple: the
/// argument for `name`, of the parameter `relation`'s tuples.
fn per_note(name: &str, relation: &str) -> String {
    format!(
        "`{name}` is a `per {relation}` parameter: its argument is computed for each tuple of `{relation}`, with the tuple's attributes in scope by name, as in `where`; one that doesn't use them is the same for every tuple, so only how many there are would matter"
    )
}

/// The argument `value` for the `per` parameter `name`, of the parameter
/// `relation`'s tuples of `heading`, if it names one of their attributes
/// in quotes, as SQL writes a column's name: a text, the same for every
/// tuple, which the fix makes the attribute.
pub(crate) fn quoted_attribute(
    value: &Expr,
    name: &str,
    relation: &str,
    heading: &hir::Heading,
) -> Option<Diagnostic> {
    let Expr::Literal(literal) = value else {
        return None;
    };
    let Ok(trendy_syntax::ast::LiteralValue::Text(attribute)) = literal.value() else {
        return None;
    };
    heading.named().find(|(a, _)| *a == attribute)?;
    Some(
        Diagnostic::error(
            Code::ConstantPer,
            format!("the argument for `{name}` is the same for every tuple of `{relation}`"),
            value.span(),
            format!("a text, not the attribute `{attribute}`"),
        )
        .with_note(per_note(name, relation))
        .with_note("double quotes make a text; SQL writes a column's name in them, but an attribute is named bare")
        .with_suggestion(
            format!("name the attribute: `{attribute}`"),
            vec![Edit {
                span: value.span(),
                replacement: attribute,
            }],
        ),
    )
}

/// The argument `value` for the `per` parameter `name`, which doesn't use
/// the attributes of the parameter `relation`'s tuples, of `heading`, so
/// is the same for every one.
pub(crate) fn constant_per(
    name: &str,
    relation: &str,
    value: &Expr,
    heading: &hir::Heading,
) -> Diagnostic {
    let attributes: Vec<String> = heading.named().map(|(a, _)| format!("`{a}`")).collect();
    let help = if attributes.is_empty() {
        format!(
            "`{relation}`'s tuples have no attributes, so no `per {relation}` argument can use them"
        )
    } else {
        format!(
            "compute it from the attributes of `{relation}`'s tuples: {}",
            trendy_diag::conjoin(&attributes)
        )
    };
    Diagnostic::error(
        Code::ConstantPer,
        format!("the argument for `{name}` is the same for every tuple of `{relation}`"),
        value.span(),
        "this doesn't depend on the tuple",
    )
    .with_note(per_note(name, relation))
    .with_help(help)
}

/// The `per` parameter `name`, of the relation `relation`, written at
/// `span` as a value, outside that relation's tuples.
pub(crate) fn per_as_value(name: &str, relation: &str, span: Span) -> Diagnostic {
    Diagnostic::error(
        Code::PerAsValue,
        format!("`{name}` is an attribute of `{relation}`'s tuples, not a value"),
        span,
        format!("not in `{relation}`'s tuples"),
    )
    .with_note(format!(
        "`{name}` is a `per {relation}` parameter: its argument is evaluated for each tuple of `{relation}`, and inside the operator it is an attribute of `{relation}`'s tuples"
    ))
    .with_help(format!(
        "name it where `{relation}`'s attributes are in scope: in `{relation} where …`, `{relation} extend {{ … }}`, `{relation}{{{name}}}`, or a `per {relation}` argument"
    ))
}

/// `any`, written `written`, somewhere other than a parameter's type: in a
/// variable's, if `in_variable`, or in an operator's result type or body.
pub(crate) fn any_outside_parameter(
    any: &hir::Any,
    written: &trendy_syntax::ast::AnyType,
    in_variable: bool,
) -> Diagnostic {
    let d = Diagnostic::error(
        Code::AnyOutsideParameter,
        format!("`{any}` can only be the type of an operator's parameter"),
        written.span(),
        "not a parameter's type",
    )
    .with_note(ANY_ONLY_IN_PARAMETERS);
    if in_variable {
        d.with_note(VARIABLE_TYPES)
    } else {
        d.with_help(ANY_ELSEWHERE)
    }
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
    // `avg{…}`, as SQL writes a mean, which is no misspelling of `min`.
    let lowercase = name.to_lowercase();
    if kind == "operator" && ["avg", "average", "mean"].contains(&&*lowercase) {
        return d
            .with_note("a mean is the sum divided by the count, which the prelude's `sum` and `count` give; it has no mean of its own yet, as the mean of integers isn't an integer")
            .with_help("write `sum{r: r, of: x} / to_approximate{from: count{r: r}}`, and for integers, `to_approximate{from: sum{r: r, of: x}}` as the dividend");
    }
    // Other languages' names for the prelude's operators, such as Python's
    // `prod` and `len`, which no closeness of spelling finds, or finds
    // wrong: `len` is close to `min`.
    let elsewhere = ELSEWHERE
        .iter()
        .find(|(other, _)| kind == "operator" && *other == lowercase)
        .filter(|(_, ours)| candidates.contains(ours));
    if let Some((_, ours)) = elsewhere {
        return d.with_suggestion(
            format!("use `{ours}`, as trendy calls it"),
            vec![Edit {
                span,
                replacement: (*ours).to_string(),
            }],
        );
    }
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

/// Other languages' names for operators of the prelude, lowercase, with
/// the prelude's.
const ELSEWHERE: &[(&str, &str)] = &[("len", "count"), ("length", "count"), ("prod", "product")];

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

/// A call in an expression, of the operator `name`, at `span`, defined at
/// `defined` unless it is built in, which has no value. `empty` says
/// whether the call is `name{}`, without arguments, and `projectable`
/// whether a value of the same name is in scope then, which `(name){}`
/// would project.
pub(crate) fn operator_call_without_value(
    name: &str,
    span: Span,
    defined: Option<Span>,
    empty: bool,
    projectable: bool,
) -> Diagnostic {
    let args = if empty { "" } else { "…" };
    let d = Diagnostic::error(
        Code::OperatorAsValue,
        format!("`{name}{{{args}}}` calls the operator `{name}`, which has no value"),
        span,
        "an update operator",
    );
    let d = match defined {
        Some(defined) => {
            d.with_secondary(defined, format!("the operator `{name}` is defined here"))
        }
        None => d.with_note(format!(
            "`{name}` is a built-in operator, declared in the prelude"
        )),
    };
    let d = d.with_note(
        "a name right before `{}` is the operator when there is one, as it is after `call`; an update operator returns no value, so it is invoked only by a `call` statement, not in an expression",
    );
    if !projectable {
        return d.with_help(format!(
            "to invoke it, write a statement: `call {name}{{}};`"
        ));
    }
    d.with_suggestion(
        format!("parenthesize `{name}` to project the value of that name instead"),
        vec![
            Edit {
                span: Span::empty(span.start),
                replacement: "(".to_string(),
            },
            Edit {
                span: Span::empty(span.end),
                replacement: ")".to_string(),
            },
        ],
    )
}

/// The parameter `name`, at `span`, assigned. `hidden` says whether it
/// hides a variable of the same name.
pub(crate) fn assigned_parameter(name: &str, span: Span, hidden: bool) -> Diagnostic {
    let d = Diagnostic::error(
        Code::NotAssignable,
        format!("parameter `{name}` can't be assigned"),
        span,
        "a parameter",
    )
    .with_note("parameters are read-only: an operator can't change the arguments it is passed");
    if hidden {
        d.with_help(format!(
            "the parameter hides the variable `{name}`; to assign the variable, rename the parameter"
        ))
    } else {
        d.with_help(format!(
            "to keep a changed value, declare a local variable for it, and assign that: `var new_{name} := {name};`"
        ))
    }
}

/// The operator `name`, at `span`, assigned, `kind` saying which kind.
pub(crate) fn assigned_operator(name: &str, span: Span, kind: Kind) -> Diagnostic {
    Diagnostic::error(
        Code::NotAssignable,
        format!("`{name}` is {}, not a variable", kind.describe()),
        span,
        "can't be assigned",
    )
    .with_note("only variables, declared with `var` at the top level, are assigned")
}

/// A `delete` or an `i_delete`, after `keyword`, of the variable `name`,
/// at `span`, that deletes every tuple: a `delete` whose condition is
/// always true, or an `i_delete` of the variable itself. `alone` says
/// whether it is the variable's only change in its statement, which the
/// assignment of the empty relation can then replace.
pub(crate) fn every_tuple_deleted(
    keyword: &str,
    name: &str,
    span: Span,
    alone: bool,
) -> Diagnostic {
    let (message, form) = if keyword == "i_delete" {
        (
            format!("`{name}` is deleted from itself"),
            "`i_delete r s` deletes the tuples of `s` from `r`",
        )
    } else {
        (
            "this `delete`'s condition is always true".to_string(),
            "`delete r where p` deletes the tuples for which `p` is true",
        )
    };
    let d = Diagnostic::error(
        Code::EveryTuple,
        message,
        span,
        format!("deletes every tuple of `{name}`"),
    )
    .with_note(format!(
        "{form}; to delete every tuple, assign the empty relation, `r := relation {{}}`, which says so"
    ));
    if alone {
        d.with_suggestion(
            "assign the empty relation",
            vec![Edit {
                span,
                replacement: format!("{name} := relation {{}}"),
            }],
        )
    } else {
        d.with_help(format!(
            "`{name} := relation {{}}` can't combine with this block's other changes to `{name}`; replace them all with one assignment of its whole new value, `{name} := …`"
        ))
    }
}

/// An `update`'s condition, at `span`, that is always true, so that it
/// updates every tuple, as it would without it; `removal` deletes it.
pub(crate) fn every_tuple_updated(span: Span, removal: Span) -> Diagnostic {
    Diagnostic::error(
        Code::EveryTuple,
        "this `update`'s condition is always true",
        span,
        "selects every tuple",
    )
    .with_note(
        "`update r { … } where p` updates the tuples for which `p` is true, and `update r { … }` every tuple",
    )
    .with_suggestion(
        "delete the condition",
        vec![Edit {
            span: removal,
            replacement: String::new(),
        }],
    )
}

/// A `delete` or an `update`, after `keyword`, at `span`, in `statement`,
/// whose condition is always false, so that it changes nothing.
pub(crate) fn no_tuple_changed(
    keyword: &str,
    span: Span,
    statement: &trendy_syntax::ast::AssignStmt,
) -> Diagnostic {
    let form = if keyword == "delete" {
        "delete r where p"
    } else {
        "update r { … } where p"
    };
    let d = Diagnostic::error(
        Code::NoChange,
        format!("this `{keyword}`'s condition is always false"),
        span,
        format!("{keyword}s nothing"),
    )
    .with_note(format!(
        "`{form}` {keyword}s the tuples for which `p` is true, so with a condition that is always false, it changes nothing"
    ));
    deleted(d, statement)
}

/// `d` with the fix deleting `statement`, and its `assign` block if
/// nothing else is in it.
fn deleted(d: Diagnostic, statement: &trendy_syntax::ast::AssignStmt) -> Diagnostic {
    let (what, span) = match statement.block() {
        Some(block) if block.statements().count() == 1 => ("the `assign` block", block.removal()),
        _ => ("the statement", statement.removal()),
    };
    d.with_suggestion(
        format!("delete {what}"),
        vec![Edit {
            span,
            replacement: String::new(),
        }],
    )
}

/// An assignment, at `span`, in `statement`, of the variable `name`'s own
/// value to it.
pub(crate) fn own_value(
    name: &str,
    span: Span,
    statement: &trendy_syntax::ast::AssignStmt,
) -> Diagnostic {
    let d = Diagnostic::error(
        Code::NoChange,
        format!("`{name}` is given its own value"),
        span,
        "changes nothing",
    )
    .with_note(OWN_VALUE_RULE);
    deleted(d, statement)
}

/// An `insert`, a `d_insert` or an `i_delete`, after `keyword`, at `span`,
/// in `statement`, of the empty relation.
pub(crate) fn no_tuples_given(
    keyword: &str,
    span: Span,
    statement: &trendy_syntax::ast::AssignStmt,
) -> Diagnostic {
    let (does, form) = if keyword == "i_delete" {
        ("removes", "removes the tuples of `s` from `r`")
    } else {
        ("adds", "adds the tuples of `s` to `r`")
    };
    let d = Diagnostic::error(
        Code::NoChange,
        format!("this `{keyword}` {does} no tuples"),
        span,
        "changes nothing",
    )
    .with_note(format!(
        "`{keyword} r s` {form}, and `s` is the empty relation"
    ));
    deleted(d, statement)
}

/// An `insert` or a `d_insert`, after `keyword`, of the variable `name`,
/// at `span`, in `statement`, into itself.
pub(crate) fn inserted_into_itself(
    keyword: &str,
    name: &str,
    span: Span,
    statement: &trendy_syntax::ast::AssignStmt,
) -> Diagnostic {
    let (label, note) = if keyword == "d_insert" {
        (
            "fails, or changes nothing",
            "`d_insert r s` adds the tuples of `s` to `r`, and fails if one is there already, so with `r` itself it fails if `r` has a tuple, and otherwise changes nothing",
        )
    } else {
        (
            "changes nothing",
            "`insert r s` adds the tuples of `s` to `r`, and those of `r` are there already",
        )
    };
    let d = Diagnostic::error(
        Code::NoChange,
        format!("`{name}` is inserted into itself"),
        span,
        label,
    )
    .with_note(note);
    deleted(d, statement)
}

/// An `update`, at `span`, in `statement`, that gives each attribute its
/// own value.
pub(crate) fn own_values_updated(
    span: Span,
    statement: &trendy_syntax::ast::AssignStmt,
) -> Diagnostic {
    let d = Diagnostic::error(
        Code::NoChange,
        "this `update` gives each attribute its own value",
        span,
        "changes nothing",
    )
    .with_note(OWN_VALUE_RULE);
    deleted(d, statement)
}

/// `postfix`, a `replace` of `operand`, that gives each attribute its own
/// value.
pub(crate) fn own_values_replaced(
    postfix: &trendy_syntax::ast::PostfixExpr,
    operand: &Expr,
) -> Diagnostic {
    let (span, edit) = dropped(postfix, operand);
    Diagnostic::error(
        Code::NoChange,
        "this `replace` gives each attribute its own value",
        span,
        "leaves the operand as it is",
    )
    .with_note(OWN_VALUE_RULE)
    .with_suggestion("drop the `replace { … }`", vec![edit])
}

/// `assign`, in `list`, which gives the attribute `name` its own value.
pub(crate) fn own_attribute_value(
    name: &str,
    assign: &trendy_syntax::ast::Assign,
    list: &trendy_syntax::ast::AssignList,
) -> Diagnostic {
    Diagnostic::error(
        Code::NoChange,
        format!("`{name}` is given its own value"),
        assign.span(),
        "changes nothing",
    )
    .with_note(OWN_VALUE_RULE)
    .with_suggestion(
        "delete the assignment",
        vec![Edit {
            span: list.removal(assign),
            replacement: String::new(),
        }],
    )
}

/// Why giving something its own value is an error, for notes.
const OWN_VALUE_RULE: &str = "a variable or an attribute that isn't given a new value keeps its own, so giving it its own value changes nothing";

/// An `update` with an empty `list`, in `statement`, which would change
/// nothing.
pub(crate) fn empty_update(
    list: &trendy_syntax::ast::AssignList,
    statement: &trendy_syntax::ast::AssignStmt,
) -> Diagnostic {
    let d = Diagnostic::error(
        Code::EmptyDoesNothing,
        "this `update` changes nothing",
        list.span(),
        "no attributes",
    )
    .with_note(
        "`update` takes attributes the variable has, each `name := value`; with none it would change nothing",
    );
    deleted(d, statement)
}

/// An `assign` block with no assignments.
pub(crate) fn empty_assign_block(block: &trendy_syntax::ast::AssignBlock) -> Diagnostic {
    Diagnostic::error(
        Code::EmptyDoesNothing,
        "this `assign` block assigns nothing",
        block.span(),
        "no assignments",
    )
    .with_note(
        "an `assign` block makes its assignments all at once; with none it would change nothing",
    )
    .with_suggestion(
        "delete it",
        vec![Edit {
            span: block.removal(),
            replacement: String::new(),
        }],
    )
}

/// Two changes to the variable `name` in one `assign` block, at `first`
/// and `second`, that conflict, as `why` says.
pub(crate) fn conflicting_changes(name: &str, first: Span, second: Span, why: &str) -> Diagnostic {
    Diagnostic::error(
        Code::ConflictingChanges,
        format!("two changes to `{name}` conflict: {why}"),
        second,
        "conflicts with the other change",
    )
    .with_secondary(first, "the other change")
    .with_note(CHANGES_RULE)
    .with_help(
        "make one of the changes do both, or make them in two statements, one after the other",
    )
}

/// Two `i_delete`s of the variable `name` in one `assign` block, at `first`
/// and `second`, that both remove `tuple`.
pub(crate) fn conflicting_tuple(
    name: &str,
    first: Span,
    second: Span,
    tuple: &hir::TupleValue,
) -> Diagnostic {
    let tuple = format!("`tuple {tuple}`");
    Diagnostic::error(
        Code::ConflictingChanges,
        format!("two changes to `{name}` remove the same tuple"),
        second,
        format!("removes {tuple}"),
    )
    .with_secondary(first, format!("also removes {tuple}"))
    .with_note(CHANGES_RULE)
    .with_help("remove the tuple in only one of the changes")
}

/// How an `assign` block's changes to one variable combine, for notes.
const CHANGES_RULE: &str = "the changes in an `assign` block are made at once, each computed from the variables as they were before it: changes to a variable's tuples or attributes combine, unless two remove the same tuple or set the same attribute, and `v := …` can't combine with any other change to `v`";

/// `call name{…}`, at `span`, where `name` is `what`, not an operator.
pub(crate) fn not_an_operator(name: &str, span: Span, what: &str) -> Diagnostic {
    Diagnostic::error(
        Code::NotAnOperator,
        format!("`{name}` is {what}, not an operator"),
        span,
        "can't be called",
    )
    .with_note(
        "only operators are called: `call` invokes an update operator, such as `debug_value`, and a read-only operator's call is a value in an expression",
    )
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

/// `operator main{…}: type`, the definition `decl`, which has no result
/// type: the fix removes it, and the `return`s with it.
pub(crate) fn main_with_result(decl: &trendy_syntax::ast::Operator) -> Diagnostic {
    let mut edits: Vec<Edit> = decl
        .result_removal()
        .into_iter()
        .map(|span| Edit {
            span,
            replacement: String::new(),
        })
        .collect();
    for statement in decl
        .body()
        .iter()
        .flat_map(trendy_syntax::ast::Body::statements)
    {
        if let trendy_syntax::ast::Statement::Return(stmt) = statement {
            edits.push(Edit {
                span: stmt.removal(),
                replacement: String::new(),
            });
        }
    }
    let span = decl.result().map_or_else(|| decl.span(), |t| t.span());
    Diagnostic::error(
        Code::MainSignature,
        "`main` can't have a result type",
        span,
        "nothing receives `main`'s value",
    )
    .with_note("execution starts in `main`, an update operator, which returns no value")
    .with_suggestion("remove the result type, and the `return`s", edits)
}

/// `operator main{…}`, whose parameters `params` must be empty.
pub(crate) fn main_with_params(params: &trendy_syntax::ast::Heading) -> Diagnostic {
    Diagnostic::error(
        Code::MainSignature,
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
        .with_parameter_note(ty)
        .with_secondary(operand.span(), format!("this is `{ty}`"))
        .with_note(
            "a projection, `r{a, b}`, `r remove {c}`, `r rename {a as b}`, `r extend { d := … }`, `r replace { a := … }`, `r wrap { w := {a} }` and `r unwrap {w}` work on the attributes of a tuple or relation",
        )
}

/// What every join does with the operands' common attributes, for notes.
const JOIN_RULE: &str = "`join` matches the tuples of its operands on all the attributes they have in common, by name; `times` is for operands with none in common, `compose` is `join` without the common attributes, and `matching` and `not matching` keep the left operand's tuples that have a match, or none";

const IMAGE_RULE: &str = "`!!r` is the image relation of the tuple at hand in `r`: the tuples of `r` that match it on their common attributes, by name, without those attributes";

/// The attributes of `heading`, each in backticks, as a list writes them:
/// a wrapped one, which no source can name, as `…`, last.
fn named_attributes(heading: &hir::Heading) -> Vec<String> {
    let mut names: Vec<String> = heading.named().map(|(a, _)| format!("`{a}`")).collect();
    if heading.attributes().any(|(a, _)| hir::is_wrapped(a)) {
        names.push("…".to_string());
    }
    names
}

/// The attributes of `heading`, for a label or note.
fn listed(heading: &hir::Heading) -> String {
    let names = named_attributes(heading);
    if names.is_empty() {
        "no attributes".to_string()
    } else {
        trendy_diag::conjoin(&names)
    }
}

/// `!!`, written `image`, where there is no tuple at hand.
pub(crate) fn image_outside(image: &trendy_syntax::ast::ImageExpr) -> Diagnostic {
    let span = image
        .operator()
        .map_or_else(|| image.span(), |k| trendy_syntax::span(k.text_range()));
    Diagnostic::error(
        Code::ImageOutsideTuple,
        "`!!` has no tuple at hand here",
        span,
        "no tuple to match",
    )
    .with_note(IMAGE_RULE)
    .with_note("there is a tuple at hand only in the values of an `extend` or a `replace`")
    .with_help("compute it for each tuple of another relation: `groups extend { members := !!r }`")
}

/// `!!`, at `span`, applied to `operand`, of type `ty`, which isn't a
/// relation.
pub(crate) fn image_not_relation(span: Span, ty: &hir::Type, operand: &Expr) -> Diagnostic {
    Diagnostic::error(
        Code::WrongOperandType,
        format!("`!!` can't be applied to {}", plural(ty)),
        span,
        "`!!` takes a relation",
    )
    .with_parameter_note(ty)
    .with_secondary(operand.span(), format!("this is `{ty}`"))
    .with_note(IMAGE_RULE)
}

/// `!!`, at `span`, applied to `operand`, a relation with `heading`,
/// which has no attribute in common with the tuple at hand, which has
/// `at_hand`.
pub(crate) fn image_unrelated(
    span: Span,
    operand: &Expr,
    heading: &hir::Heading,
    at_hand: &hir::Heading,
) -> Diagnostic {
    let d = Diagnostic::error(
        Code::CommonAttributes,
        "`!!` needs an attribute in common with the tuple at hand",
        span,
        "nothing to match on",
    )
    .with_secondary(operand.span(), format!("this has {}", listed(heading)))
    .with_note(format!("the tuple at hand has {}", listed(at_hand)))
    .with_note(IMAGE_RULE);
    // An attribute named a little differently on each side is likely the
    // one to match on.
    let close = heading
        .named()
        .flat_map(|(a, _)| at_hand.named().map(move |(b, _)| (a, b)))
        .filter(|(a, b)| edit_distance(a, b) <= 2)
        .min_by_key(|(a, b)| (edit_distance(a, b), *a, *b));
    match close {
        Some((a, b)) => d.with_suggestion(
            format!("rename `{a}` to `{b}`, to match on it"),
            vec![Edit {
                span: operand.span(),
                replacement: format!("({} rename {{{a} as {b}}})", operand.syntax().text()),
            }],
        ),
        None => d.with_help(
            "if an attribute means the same in both but is named differently, rename it: `!!(r rename {a as b})`",
        ),
    }
}

/// `!!`, at `span`, applied to `operand`, whose attribute `name` has
/// type `ty`, which is `at_hand` in the tuple at hand.
pub(crate) fn image_type_conflict(
    span: Span,
    operand: &Expr,
    name: &str,
    ty: &hir::Type,
    at_hand: &hir::Type,
) -> Diagnostic {
    Diagnostic::error(
        Code::JoinTypeConflict,
        format!(
            "the relation and the tuple at hand both have an attribute `{name}`, of different types"
        ),
        span,
        format!("`!!` matches tuples on `{name}`"),
    )
    .with_secondary(operand.span(), format!("`{name}` is `{ty}` here"))
    .with_note(format!("in the tuple at hand, `{name}` is `{at_hand}`"))
    .with_note(IMAGE_RULE)
    .with_note("matched attributes must have the same type, since values never convert to another type implicitly")
    .with_help(format!(
        "if the two `{name}`s mean different things, rename one: `!!(r rename {{{name} as other_{name}}})`"
    ))
}

/// The element `expr` of a relation selector, of type `found`, which isn't
/// a tuple.
pub(crate) fn not_a_tuple(found: &hir::Type, expr: &Expr) -> Diagnostic {
    Diagnostic::error(
        Code::TypeMismatch,
        format!("mismatched types: expected a tuple, found `{found}`"),
        expr.span(),
        format!("this is `{found}`"),
    )
    .with_note(
        "a relation's elements are tuples: tuple selectors, `{ a: 1 }`, or expressions of tuples",
    )
}

/// The join `join`, `operator` written at `span`, applied to `operand`, of
/// type `ty`, which it doesn't take: a scalar, or, for `matching` and
/// `not matching`, a tuple.
pub(crate) fn not_joinable(
    join: crate::ops::JoinKind,
    (operator, span): (&str, Span),
    ty: &hir::Type,
    operand: &Expr,
) -> Diagnostic {
    let (takes, note) = match join {
        crate::ops::JoinKind::Matching | crate::ops::JoinKind::NotMatching => (
            "relations",
            "`matching` and `not matching` keep the tuples of a relation that have a match in another relation, or none; of tuples, compare them with `=`",
        ),
        kind if kind.is_set() => (
            "relations of one heading",
            "`union`, `d_union`, `xunion`, `intersect`, `minus` and `i_minus` combine the tuples of relations; Tutorial D's tuple union, of the attributes of tuples, is `join`",
        ),
        _ => (
            "relations, or tuples",
            "`join`, `times` and `compose` combine the tuples of relations, or the attributes of tuples",
        ),
    };
    Diagnostic::error(
        Code::WrongOperandType,
        format!("`{operator}` can't be applied to {}", plural(ty)),
        span,
        format!("`{operator}` takes {takes}"),
    )
    .with_parameter_note(ty)
    .with_secondary(operand.span(), format!("this is `{ty}`"))
    .with_note(note)
}

/// `union`, at `span`, of tuples: Tutorial D's tuple union, which is `join`.
pub(crate) fn tuple_union(span: Span) -> Diagnostic {
    Diagnostic::error(
        Code::WrongOperandType,
        "`union` can't be applied to tuples",
        span,
        "`union` takes relations of one heading",
    )
    .with_note("Tutorial D's tuple union, a tuple of both tuples' attributes, is `join` in trendy; `union` is of the tuples of relations")
    .with_suggestion(
        "write `join`",
        vec![Edit {
            span,
            replacement: "join".to_string(),
        }],
    )
}

/// The tuple `operand` in the join `operator` of relations.
pub(crate) fn tuple_among_relations(operator: &str, operand: &Expr) -> Diagnostic {
    let written = operand.syntax().text().to_string();
    Diagnostic::error(
        Code::WrongOperandType,
        format!("`{operator}` can't combine a tuple with relations"),
        operand.span(),
        "this is a tuple",
    )
    .with_note(format!(
        "`{operator}` takes relations, or tuples, but not both; a relation of one tuple joins like the tuple"
    ))
    .with_suggestion(
        "make it a relation of one tuple",
        vec![Edit {
            span: operand.span(),
            replacement: format!("relation {{ {written} }}"),
        }],
    )
}

/// The join `operator`, written at `span`, of operands written at `lhs` and
/// `rhs`, whose common attribute `name` has type `lhs_ty` in one and
/// `rhs_ty` in the other.
pub(crate) fn join_type_conflict(
    (operator, span): (&str, Span),
    name: &str,
    (lhs, lhs_ty): (Span, &hir::Type),
    (rhs, rhs_ty): (Span, &hir::Type),
) -> Diagnostic {
    Diagnostic::error(
        Code::JoinTypeConflict,
        format!(
            "the operands of `{operator}` both have an attribute `{name}`, of different types"
        ),
        span,
        format!("`{operator}` matches tuples on `{name}`"),
    )
    .with_secondary(lhs, format!("`{name}` is `{lhs_ty}` here"))
    .with_secondary(rhs, format!("and `{rhs_ty}` here"))
    .with_note(JOIN_RULE)
    .with_note("matched attributes must have the same type, since values never convert to another type implicitly")
    .with_help(format!(
        "if the two `{name}`s mean different things, rename one: `… rename {{{name} as other_{name}}}`"
    ))
}

/// An operand of a join, for its errors: where it is written, and its
/// heading. For a step of `compose[…]`, the operands before it, composed.
pub(crate) struct Operand<'a> {
    pub(crate) span: Span,
    pub(crate) heading: &'a hir::Heading,
    pub(crate) composed: bool,
}

/// The join `join`, `operator` written at `span`, of `operands`, whose
/// common attributes are wrong for the operator. For `times`, `shared`
/// are the attributes more than one operand has. For `join` and `compose`,
/// `groups` gives each operand's group, the operands connected to each other
/// through common attributes, and there is more than one.
pub(crate) fn common_attributes(
    join: crate::ops::JoinKind,
    (operator, span): (&str, Span),
    operands: &[Operand],
    shared: &BTreeSet<String>,
    groups: &[usize],
) -> Diagnostic {
    use crate::ops::JoinKind;
    let attributes = |operand: &Operand| {
        let names: Vec<String> = operand
            .heading
            .attributes()
            .map(|(a, _)| format!("`{a}`"))
            .collect();
        let this = if operand.composed {
            "composed, these have"
        } else {
            "this has"
        };
        if names.is_empty() {
            format!("{this} no attributes")
        } else {
            format!("{this} {}", trendy_diag::conjoin(&names))
        }
    };
    let labelled = |d: Diagnostic, shown: &dyn Fn(&hir::Heading) -> bool| {
        operands
            .iter()
            .filter(|operand| shown(operand.heading))
            .fold(d, |d, operand| {
                d.with_secondary(operand.span, attributes(operand))
            })
    };
    if join == JoinKind::Times {
        let names: Vec<String> = shared.iter().map(|a| format!("`{a}`")).collect();
        let first = shared
            .first()
            .expect("times is reported with common attributes");
        let d = Diagnostic::error(
            Code::CommonAttributes,
            format!(
                "the operands of `{operator}` have {} in common",
                trendy_diag::conjoin(&names)
            ),
            span,
            format!("`{operator}` takes operands with no attributes in common"),
        );
        // Only the operands that share something, in a long list.
        return labelled(d, &|heading| {
            operands.len() == 2 || shared.iter().any(|a| heading.get(a).is_some())
        })
        .with_note(JOIN_RULE)
        .with_help(format!(
            "to match tuples on them, write `join`; if they mean different things, rename one side's: `… rename {{{first} as other_{first}}}`"
        ));
    }
    // An attribute of one group that's a near miss for one of another's was
    // probably meant to match it. Short names are all near each other, so
    // the difference must be small for their length too.
    let near_miss = operands
        .iter()
        .zip(groups)
        .flat_map(|(Operand { heading: a, .. }, g)| {
            operands
                .iter()
                .zip(groups)
                .filter(move |(_, h)| *h != g)
                .flat_map(move |(Operand { heading: b, .. }, _)| {
                    a.named()
                        .flat_map(move |(x, _)| b.named().map(move |(y, _)| (x, y)))
                })
        })
        .filter(|(a, b)| {
            let distance = edit_distance(a, b);
            distance <= 2 && distance * 3 <= a.len().min(b.len())
        })
        .min_by_key(|(a, b)| (edit_distance(a, b), *a, *b));
    let message = if operands.len() == 2 {
        format!("the operands of `{operator}` have no attributes in common")
    } else {
        format!("some operands of `{operator}` have no attributes in common with the others")
    };
    let d = Diagnostic::error(
        Code::CommonAttributes,
        message,
        span,
        format!("`{operator}` matches tuples on their common attributes"),
    );
    let d = labelled(d, &|_| true).with_note(JOIN_RULE);
    let d = if operands.len() == 2 {
        d
    } else {
        d.with_note(format!(
            "each operand of `{operator}{{…}}` must have an attribute in common with another, so that they all connect, and none is paired with every tuple of the rest by mistake"
        ))
    };
    let d = match near_miss {
        Some((a, b)) => d.with_help(format!(
            "if `{b}` is `{a}`, rename it so they match: `… rename {{{b} as {a}}}`"
        )),
        None => d,
    };
    if matches!(join, JoinKind::Matching | JoinKind::NotMatching) {
        return d;
    }
    d.with_help(if operands.len() == 2 {
        "to pair every tuple of one with every tuple of the other, write `times`"
    } else {
        "to pair every tuple of some operands with every tuple of others, join each group, and combine the groups with `times`"
    })
}

/// A list of the operator `join`, written in the brackets it takes: in
/// brackets for the left folds, and braces for the others.
fn list_of(join: crate::ops::JoinKind) -> String {
    let operator = join.keyword();
    if join.is_ordered() {
        format!("{operator}[r, s, t]")
    } else {
        format!("{operator}{{r, s, t}}")
    }
}

/// `expr`, written so that a postfix operator applies to all of it: in
/// parentheses if it is a prefix or dyadic operator's, which a postfix one
/// would bind tighter than.
fn postfix_operand(expr: &Expr) -> String {
    let written = expr.syntax().text().to_string();
    match expr {
        Expr::Unary(_) | Expr::Image(_) | Expr::From(_) | Expr::Binary(_) => format!("({written})"),
        _ => written,
    }
}

/// The set operator `operator`, written at `span`, of operands whose
/// headings differ: the first, written at `first.0` with heading `first.1`,
/// and another, at `other.0` with `other.1`. When one attribute of each is
/// named differently, with the same type, the fix renames the other's.
pub(crate) fn heading_mismatch(
    (operator, span): (&str, Span),
    first: (&Expr, &hir::Heading),
    other: (&Expr, &hir::Heading),
) -> Diagnostic {
    let only = |a: &hir::Heading, b: &hir::Heading| -> Vec<(String, hir::Type)> {
        a.attributes()
            .filter(|(name, ty)| b.get(name) != Some(ty))
            .map(|(name, ty)| (name.to_string(), ty.clone()))
            .collect()
    };
    let (first_only, other_only) = (only(first.1, other.1), only(other.1, first.1));
    let listed = |attributes: &[(String, hir::Type)]| {
        let names: Vec<String> = attributes
            .iter()
            .map(|(name, ty)| format!("`{name}: {ty}`"))
            .collect();
        if names.is_empty() {
            "nothing else".to_string()
        } else {
            trendy_diag::conjoin(&names)
        }
    };
    let d = Diagnostic::error(
        Code::HeadingMismatch,
        format!("the operands of `{operator}` have different headings"),
        span,
        format!("`{operator}` takes relations of one heading"),
    )
    .with_secondary(first.0.span(), format!("this has {}", listed(&first_only)))
    .with_secondary(other.0.span(), format!("and this {}", listed(&other_only)))
    .with_note(format!(
        "`{operator}` combines tuples, so they must have the same attributes, of the same types; rename or project an operand to match"
    ));
    match (&first_only[..], &other_only[..]) {
        ([(want, want_ty)], [(have, have_ty)]) if want_ty == have_ty && want != have => d
            .with_suggestion(
                format!("rename `{have}` to `{want}`"),
                vec![Edit {
                    span: other.0.span(),
                    replacement: format!(
                        "{} rename {{{have} as {want}}}",
                        postfix_operand(other.0)
                    ),
                }],
            ),
        _ => d,
    }
}

/// `nary`, a list of the operator `join` with nothing in it, which has no
/// value, since the operator has no identity.
pub(crate) fn empty_operands(join: crate::ops::JoinKind, nary: &NaryExpr) -> Diagnostic {
    use crate::ops::JoinKind;
    let operator = join.keyword();
    let (written, why) = match join {
        JoinKind::Minus => (
            "minus[]",
            "`minus[r, s, t]` removes from `r`, so it needs an `r`",
        ),
        JoinKind::Union => (
            "union{}",
            "a union of nothing would have no heading to be a relation of",
        ),
        JoinKind::DisjointUnion => (
            "d_union{}",
            "a union of nothing would have no heading to be a relation of",
        ),
        JoinKind::ExclusiveUnion => (
            "xunion{}",
            "a union of nothing would have no heading to be a relation of",
        ),
        JoinKind::IncludedMinus => (
            "i_minus[]",
            "`i_minus[r, s, t]` removes from `r`, so it needs an `r`",
        ),
        JoinKind::Intersect => (
            "intersect{}",
            "`intersect{}` would be every tuple of a heading that isn't known, and that is usually infinitely many",
        ),
        _ => (
            "compose[]",
            "no relation composes with everything to give it back, since compose needs a common attribute and removes it",
        ),
    };
    Diagnostic::error(
        Code::EmptyList,
        format!("`{written}` has no value"),
        nary.span(),
        format!("`{operator}` needs at least one operand"),
    )
    .with_note(format!(
        "`join{{}}` and `times{{}}` are `relation {{}} {{ {{}} }}`, which joins with anything to give it back; {why}"
    ))
    .with_help(format!("write the relations, as in `{}`", list_of(join)))
}

/// `.name`, at `at`, applied to a value of type `ty`, which isn't a tuple.
/// The operator `name`, at `at`, called as a method of `operand`, as
/// `operand.name{}`, written over `whole`. With a lone parameter `only`,
/// the fix makes `operand` its argument.
pub(crate) fn method_call(
    name: &str,
    at: Span,
    operand: &Expr,
    whole: Span,
    only: Option<&str>,
    params: &str,
) -> Diagnostic {
    let d = Diagnostic::error(
        Code::WrongOperandType,
        format!("`{name}` is an operator, not an attribute, so it isn't called as a method"),
        at,
        "called as a method",
    )
    .with_note(format!(
        "an operator is called by its name, with each argument named in braces after it, and none before it as a method's receiver: the parameters are {params}"
    ));
    match only {
        Some(param) => d.with_suggestion(
            format!("call `{name}` with it as `{param}`"),
            vec![Edit {
                span: whole,
                replacement: format!("{name}{{{param}: {}}}", operand.syntax().text()),
            }],
        ),
        None => d.with_help(format!(
            "call it as `{name}{{…}}`, with this value as one of its arguments"
        )),
    }
}

pub(crate) fn not_extractable(name: &str, at: Span, ty: &hir::Type, operand: &Expr) -> Diagnostic {
    let d = Diagnostic::error(
        Code::WrongOperandType,
        format!("`.{name}` can't be applied to {}", plural(ty)),
        at,
        "only a tuple has an attribute's value",
    )
    .with_parameter_note(ty)
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
    empty_list(
        postfix,
        operand,
        ("renames nothing", "no renamings"),
        "`rename` takes renamings, each `old as new`; with none it would change nothing",
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

/// `extend {}` or `replace {}`, as `assigning` says, written `postfix`
/// after `operand`, which changes nothing.
pub(crate) fn empty_assign(
    assigning: crate::ops::Assigning,
    postfix: &trendy_syntax::ast::PostfixExpr,
    operand: &Expr,
) -> Diagnostic {
    match assigning {
        crate::ops::Assigning::Extend => empty_list(
            postfix,
            operand,
            ("adds nothing", "no new attributes"),
            "`extend` takes new attributes, each `name := value`; with none it would change nothing",
        ),
        crate::ops::Assigning::Replace => empty_list(
            postfix,
            operand,
            ("replaces nothing", "no attributes"),
            "`replace` takes attributes the operand has, each `name := value`; with none it would change nothing",
        ),
        crate::ops::Assigning::Update => unreachable!("an `update` is a statement"),
    }
}

/// The new attribute `name`, at `span`, which `operand`, of type `ty`,
/// already has. `replace` is the `extend` keyword's span, if every
/// attribute the `extend` names exists, so that `replace` is likely meant.
pub(crate) fn extension_exists(
    name: &str,
    span: Span,
    ty: &hir::Type,
    operand: &Expr,
    replace: Option<Span>,
) -> Diagnostic {
    let d = Diagnostic::error(
        Code::AttributeExists,
        format!("attribute `{name}` already exists"),
        span,
        "this name is taken",
    )
    .with_secondary(operand.span(), format!("this is `{ty}`"))
    .with_note("`extend` adds attributes; attributes are a set, so a new one needs a name the operand doesn't have");
    match replace {
        Some(keyword) => d.with_suggestion(
            format!("to give `{name}` a new value of the same type, write `replace`"),
            vec![Edit {
                span: keyword,
                replacement: "replace".to_string(),
            }],
        ),
        None => d.with_help(format!(
            "to give `{name}` a new value of the same type, use `replace` for it: `… replace {{ {name} := … }}`; to add an attribute, choose a free name"
        )),
    }
}

/// The attribute `name`, at `span`, which a `replace` gives a new value but
/// `operand`, of type `ty`, hasn't. `extend` is the `replace` keyword's
/// span, if no attribute the `replace` names exists, so that `extend` is
/// likely meant.
pub(crate) fn replaced_unknown(
    assigning: crate::ops::Assigning,
    name: &str,
    span: Span,
    ty: &hir::Type,
    operand: &Expr,
    extend: Option<Span>,
) -> Diagnostic {
    let (hir::Type::Tuple(heading) | hir::Type::Relation(heading)) = ty else {
        unreachable!("only tuples and relations have attributes to replace");
    };
    let rename = heading
        .named()
        .map(|(a, _)| a)
        .filter(|a| edit_distance(a, name) <= 2)
        .min_by_key(|a| edit_distance(a, name));
    let rule = if assigning == crate::ops::Assigning::Update {
        "`update` gives attributes the variable has new values of the same types; a variable's type never changes, so it adds none"
    } else {
        "`replace` gives attributes the operand has new values of the same types; it adds none"
    };
    let d = unknown_attribute(Noun::Projected, name, span, rule, rename, None)
        .with_secondary(operand.span(), format!("this is `{ty}`"));
    if assigning == crate::ops::Assigning::Update {
        return d;
    }
    match extend {
        Some(keyword) => d.with_suggestion(
            format!("to add `{name}` as a new attribute, write `extend`"),
            vec![Edit {
                span: keyword,
                replacement: "extend".to_string(),
            }],
        ),
        None => d.with_help(format!(
            "to add `{name}` as a new attribute, use `extend` for it: `… extend {{ {name} := … }}`"
        )),
    }
}

/// `where`, at `span`, applied to `operand`, of type `ty`, which isn't a
/// relation.
pub(crate) fn not_restrictable(span: Span, ty: &hir::Type, operand: &Expr) -> Diagnostic {
    let d = Diagnostic::error(
        Code::WrongOperandType,
        format!("`where` can't be applied to {}", plural(ty)),
        span,
        "`where` takes a relation",
    )
    .with_parameter_note(ty)
    .with_secondary(operand.span(), format!("this is `{ty}`"))
    .with_note("`r where p` is the tuples of the relation `r` for which the condition `p` is true");
    match ty {
        hir::Type::Tuple(_) => d.with_help(
            "a tuple satisfies a condition or doesn't: the condition itself, with `t.a` for its attributes, is that boolean",
        ),
        _ => d,
    }
}

/// `operator`, `group`, `ungroup`, `tuple from` or `value from`, at `span`,
/// applied to `operand`, of type `ty`, which isn't a relation.
pub(crate) fn not_relational(
    operator: &str,
    span: Span,
    ty: &hir::Type,
    operand: &Expr,
) -> Diagnostic {
    let note = match operator {
        "group" => {
            "`group` gathers attributes of the tuples that share the others into a relation; a tuple has no other tuples to gather with"
        }
        "ungroup" => {
            "`ungroup` replaces each tuple of a relation with one for each tuple of its relation-valued attributes; a tuple's result would be a relation"
        }
        "insert" | "d_insert" | "delete" | "i_delete" => {
            "`insert`, `d_insert`, `delete` and `i_delete` add and remove the tuples of a relation variable; another variable is given a new value with `:=`, and a tuple variable new attribute values with `update`"
        }
        _ => "`tuple from` and `value from` take the only tuple of a relation",
    };
    Diagnostic::error(
        Code::WrongOperandType,
        format!("`{operator}` can't be applied to {}", plural(ty)),
        span,
        format!("`{operator}` takes a relation"),
    )
    .with_parameter_note(ty)
    .with_secondary(operand.span(), format!("this is `{ty}`"))
    .with_note(note)
}

/// A note on the attributes of `whose`, "operand" or "tuple", of type
/// `ty`, a tuple or relation type: what they are, or that there are none.
pub(crate) fn attributes_note(whose: &str, ty: &hir::Type) -> String {
    let (hir::Type::Tuple(heading) | hir::Type::Relation(heading)) = ty else {
        unreachable!("only tuples and relations have attributes");
    };
    let attributes = named_attributes(heading);
    if attributes.is_empty() {
        format!("the {whose}, of type `{ty}`, has no attributes")
    } else {
        format!(
            "the {whose}'s attributes are {}",
            trendy_diag::conjoin(&attributes)
        )
    }
}

/// The attribute `name`, at `span`, named in an `ungroup` or an `unwrap`,
/// as `unpacking` says, of type `ty`, which isn't the kind it unpacks.
pub(crate) fn not_unpackable(
    unpacking: crate::ops::Unpacking,
    name: &str,
    span: Span,
    ty: &hir::Type,
) -> Diagnostic {
    use crate::ops::Unpacking;
    let (done, kind, note) = match unpacking {
        Unpacking::Ungroup => (
            "ungrouped",
            "relation-valued",
            "`ungroup` replaces a relation-valued attribute with the attributes of its relation's tuples",
        ),
        Unpacking::Unwrap => (
            "unwrapped",
            "tuple-valued",
            "`unwrap` replaces a tuple-valued attribute with its tuple's attributes",
        ),
    };
    let d = Diagnostic::error(
        Code::WrongOperandType,
        format!("`{name}` can't be {done}: it is `{ty}`"),
        span,
        format!("not {kind}"),
    )
    .with_note(note);
    match (unpacking, ty) {
        (Unpacking::Ungroup, hir::Type::Tuple(_)) => d.with_help(format!(
            "a tuple-valued attribute is unpacked with `unwrap`: `… unwrap {{{name}}}`"
        )),
        (Unpacking::Unwrap, hir::Type::Relation(_)) => d.with_help(format!(
            "a relation-valued attribute is unpacked with `ungroup`: `… ungroup {{{name}}}`"
        )),
        _ => d,
    }
}

/// The attribute `attribute` of the attribute `unpacked`, written at
/// `span`, which the result of an `ungroup` or an `unwrap`, as `unpacking`
/// says, already has: kept, if `other` is `None`, or from `other`.
pub(crate) fn unpack_clash(
    unpacking: crate::ops::Unpacking,
    attribute: &str,
    span: Span,
    unpacked: &str,
    other: Option<&str>,
) -> Diagnostic {
    let keyword = unpacking.keyword();
    let (doing, its) = match unpacking {
        crate::ops::Unpacking::Ungroup => ("ungrouping", "its tuples have"),
        crate::ops::Unpacking::Unwrap => ("unwrapping", "it has"),
    };
    let from = match other {
        Some(other) => format!("`{other}` has it too"),
        None => "the result keeps an attribute of that name".to_string(),
    };
    Diagnostic::error(
        Code::AttributeExists,
        format!("{doing} `{unpacked}` would give two attributes `{attribute}`"),
        span,
        format!("{its} `{attribute}`, and {from}"),
    )
    .with_note("attributes are a set, so each name can appear only once in the result")
    .with_help(match other {
        None => format!(
            "rename the kept one first: `… rename {{{attribute} as other_{attribute}}} {keyword} {{{unpacked}}}`"
        ),
        Some(other) => format!(
            "{keyword} one, rename what it gave, then {keyword} the other: `… {keyword} {{{other}}} rename {{{attribute} as {other}_{attribute}}} {keyword} {{{unpacked}}}`"
        ),
    })
}

/// `group {}` or `wrap {}`, as `gathering` says, written `postfix` after
/// `operand`, which gathers nothing.
pub(crate) fn empty_gathering(
    gathering: crate::ops::Gathering,
    postfix: &trendy_syntax::ast::PostfixExpr,
    operand: &Expr,
) -> Diagnostic {
    let (label, note) = match gathering {
        crate::ops::Gathering::Group => (
            "no groupings",
            "`group` takes groupings, each `name := {attributes}`; with none it would change nothing",
        ),
        crate::ops::Gathering::Wrap => (
            "no wrappings",
            "`wrap` takes wrappings, each `name := {attributes}`; with none it would change nothing",
        ),
    };
    empty_list(postfix, operand, ("gathers nothing", label), note)
}

/// `ungroup {}` or `unwrap {}`, as `unpacking` says, written `postfix`
/// after `operand`, which changes nothing.
pub(crate) fn empty_unpacking(
    unpacking: crate::ops::Unpacking,
    postfix: &trendy_syntax::ast::PostfixExpr,
    operand: &Expr,
) -> Diagnostic {
    let (does, note) = match unpacking {
        crate::ops::Unpacking::Ungroup => (
            "ungroups nothing",
            "`ungroup` takes the relation-valued attributes to replace with their attributes; with none it would change nothing",
        ),
        crate::ops::Unpacking::Unwrap => (
            "unwraps nothing",
            "`unwrap` takes the tuple-valued attributes to replace with their attributes; with none it would change nothing",
        ),
    };
    empty_list(postfix, operand, (does, "no attributes named"), note)
}

/// The attribute `name`, at `span`, which gathering `gathering` gathers,
/// and gathering `first.0` already does, at `first.1`, in a `group` or a
/// `wrap`, as `kind` says.
pub(crate) fn gathered_twice(
    kind: crate::ops::Gathering,
    name: &str,
    span: Span,
    first: (&str, Span),
    gathering: &str,
) -> Diagnostic {
    let note = match kind {
        crate::ops::Gathering::Group => {
            "each attribute goes into one grouping; the attributes no grouping names are the ones each group shares"
        }
        crate::ops::Gathering::Wrap => "each attribute goes into one wrapping",
    };
    Diagnostic::error(
        Code::DuplicateAttribute,
        format!(
            "attribute `{name}` is gathered into both `{}` and `{gathering}`",
            first.0
        ),
        span,
        format!("gathered into `{gathering}` here"),
    )
    .with_secondary(first.1, format!("and into `{}` here", first.0))
    .with_note(note)
    .with_help(format!("gather `{name}` into only one of them"))
}

/// The gathering `name`, at `span`, of a `group` or a `wrap`, as
/// `gathering` says, whose new attribute's name is one that `operand`, of
/// type `ty`, keeps.
pub(crate) fn gathering_exists(
    gathering: crate::ops::Gathering,
    name: &str,
    span: Span,
    ty: &hir::Type,
    operand: &Expr,
) -> Diagnostic {
    let keyword = gathering.keyword();
    Diagnostic::error(
        Code::AttributeExists,
        format!("attribute `{name}` already exists"),
        span,
        "the result keeps an attribute of this name",
    )
    .with_secondary(operand.span(), format!("this is `{ty}`"))
    .with_note(format!("`{keyword}` keeps the attributes it doesn't gather, so a new attribute needs a name other than theirs; a name it gathers is free"))
    .with_help(format!("choose another name, or rename `{name}` first"))
}

/// Computing a new attribute, or a condition, written `value`, fails for a
/// tuple of a constant operand, as `failure` says.
pub(crate) fn extension_failure(failure: &crate::ops::TupleFailure, value: &Expr) -> Diagnostic {
    let tuple = format!("`tuple {}`", failure.tuple);
    let computing = match &failure.attribute {
        Some(attribute) => format!("computing `{attribute}`"),
        None => "evaluating the condition".to_string(),
    };
    failed(&failure.failed, value)
        .with_note(format!("{computing} for {tuple}, which the operand has"))
}

/// Why `expr`, of constants, has no value, as `failed` says.
pub(crate) fn failed(failed: &crate::ops::Failed, expr: &Expr) -> Diagnostic {
    match failed {
        crate::ops::Failed::Scalar(failure) => self::failure(*failure, expr, None),
        crate::ops::Failed::Mismatch(mismatch) => tuple_mismatch(mismatch, expr),
        crate::ops::Failed::NotOne(count) => not_one_tuple(*count, expr),
        crate::ops::Failed::Broken(broken) => broken_combination(broken, expr),
        crate::ops::Failed::NoTuples(builtin) => no_tuples(*builtin, expr),
    }
}

/// `expr`, which takes `builtin`, `min` or `max`, of a relation without
/// tuples.
fn no_tuples(builtin: hir::Builtin, expr: &Expr) -> Diagnostic {
    let name = builtin.name();
    let extreme = match builtin {
        hir::Builtin::Min => "least",
        _ => "greatest",
    };
    Diagnostic::error(
        Code::NoMinOrMax,
        format!("the relation has no tuples, so it has no {name}"),
        expr.span(),
        format!("`{name}` of a relation with no tuples"),
    )
    .with_note(format!("`{name}{{r: r, of: x}}` is the {extreme} value of `x` for a tuple of `r`, so `r` must have one; unlike `count`, which is 0, it has no value for no tuples"))
    .with_help(format!("take the {name} only of a relation with tuples; for each group's, as in `r extend {{ m := {name}{{r: !!s, of: x}} }}`, extend `r matching s` instead, which keeps the groups with tuples in `s`"))
}

/// `expr`, a disjoint union or an included minus of constants, which a
/// tuple breaks, as `broken` says.
fn broken_combination(broken: &hir::Broken, expr: &Expr) -> Diagnostic {
    let tuple = format!("`tuple {}`", broken.tuple);
    match broken.combination {
        hir::Combination::IncludedMinus => Diagnostic::error(
            Code::MissingTuple,
            "`i_minus` removes a tuple that isn't there",
            expr.span(),
            format!("the right operand has {tuple}, which the left doesn't"),
        )
        .with_note("`r i_minus s` is `r minus s` where every tuple of `s` is in `r`: it fails if one isn't, which `minus` would ignore")
        .with_help("if removing what isn't there is fine, write `minus`"),
        _ => Diagnostic::error(
            Code::SharedTuple,
            "the operands of `d_union` have a tuple in common",
            expr.span(),
            format!("both have {tuple}"),
        )
        .with_note("`r d_union s` is `r union s` where the operands have no tuple in common: it fails if they do, which `union` would allow")
        .with_help("if tuples in common are fine, write `union`"),
    }
}

/// `tuple from` or `value from`, written `expr`, of a relation with
/// `count` tuples, not one.
fn not_one_tuple(count: usize, expr: &Expr) -> Diagnostic {
    let has = if count == 0 {
        "no tuples".to_string()
    } else {
        format!("{count} tuples")
    };
    Diagnostic::error(
        Code::NotOneTuple,
        format!("the relation has {has}, but exactly one is taken"),
        expr.span(),
        format!("this relation has {has}"),
    )
    .with_note("`tuple from r` is the only tuple of `r`, and `value from r` the only value of it, so `r` must have exactly one")
    .with_help("restrict the relation to the one tuple meant first, as in `tuple from r where key = …`")
}

/// `tuple from` or `value from`, as `taken` says, at `span`, written
/// `expr`, of `operand`, a tuple with `heading` rather than a relation. The
/// fix is the tuple itself, or its only attribute's value.
pub(crate) fn taken_from_tuple(
    taken: trendy_syntax::ast::Taken,
    span: Span,
    expr: &Expr,
    operand: &Expr,
    heading: &hir::Heading,
) -> Diagnostic {
    use trendy_syntax::ast::Taken;
    let operator = match taken {
        Taken::Tuple => "tuple from",
        Taken::Value => "value from",
    };
    let d = Diagnostic::error(
        Code::WrongOperandType,
        format!("`{operator}` can't be applied to tuples"),
        span,
        format!("`{operator}` takes a relation"),
    )
    .with_secondary(operand.span(), "this is already a tuple")
    .with_note("`tuple from r` and `value from r` take the only tuple out of a relation; a tuple's attributes are taken with `.`, as in `t.a`");
    let written = operand.syntax().text().to_string();
    let mut names = heading.attributes().map(|(a, _)| a);
    match (taken, names.next(), names.next()) {
        (Taken::Tuple, ..) => d.with_suggestion(
            "use the tuple itself",
            vec![Edit {
                span: expr.span(),
                replacement: written,
            }],
        ),
        (Taken::Value, Some(name), None) => {
            let base = postfix_operand(operand);
            d.with_suggestion(
                format!("take its attribute `{name}`"),
                vec![Edit {
                    span: expr.span(),
                    replacement: format!("{base}.{name}"),
                }],
            )
        }
        (Taken::Value, ..) => d.with_help("take the attribute meant with `.`, as in `t.a`"),
    }
}

/// `value from`, at `span`, of `operand`, a relation with `heading`, which
/// hasn't exactly one attribute.
pub(crate) fn not_one_attribute(span: Span, operand: &Expr, heading: &hir::Heading) -> Diagnostic {
    let names: Vec<&str> = heading.attributes().map(|(a, _)| a).collect();
    let has = if names.is_empty() {
        "no attributes".to_string()
    } else {
        let listed: Vec<String> = names.iter().map(|a| format!("`{a}`")).collect();
        trendy_diag::conjoin(&listed)
    };
    let d = Diagnostic::error(
        Code::WrongOperandType,
        "`value from` takes a relation of one attribute",
        span,
        "no single value to take",
    )
    .with_secondary(operand.span(), format!("this has {has}"))
    .with_note("`value from r` is the value of the only attribute of the only tuple of `r`; `tuple from r` takes the whole tuple");
    // Projecting on one attribute is the fix, and which is a choice, so
    // each is offered. A prefix operand is parenthesized first, since a
    // projection would bind tighter.
    let base = postfix_operand(operand);
    names.iter().fold(d, |d, name| {
        d.with_suggestion(
            format!("take `{name}`"),
            vec![Edit {
                span: operand.span(),
                replacement: format!("{base}{{{name}}}"),
            }],
        )
    })
}

/// The join `expr` of tuples that disagree on an attribute, as `mismatch`
/// says.
fn tuple_mismatch(mismatch: &hir::Mismatch, expr: &Expr) -> Diagnostic {
    let value = |v: &hir::Value| format!("`{v}`");
    let name = &mismatch.attribute;
    Diagnostic::error(
        Code::TupleMismatch,
        format!("the tuples disagree on `{name}`, so they have no join"),
        expr.span(),
        format!(
            "`{name}` is {} in one and {} in the other",
            value(&mismatch.lhs),
            value(&mismatch.rhs)
        ),
    )
    .with_note("tuples join when they agree on every attribute they have in common; a join of relations would just have no tuple, but there is no tuple to be here")
    .with_help(format!(
        "if the two `{name}`s mean different things, rename one: `… rename {{{name} as other_{name}}}`; to keep only matching tuples, join relations of them instead"
    ))
}

/// `remove {}`, written `postfix` after `operand`, which removes nothing.
pub(crate) fn empty_remove(
    postfix: &trendy_syntax::ast::PostfixExpr,
    operand: &Expr,
) -> Diagnostic {
    empty_list(
        postfix,
        operand,
        ("removes nothing", "no attributes named"),
        "`remove` takes the attributes to drop; with none it would change nothing",
    )
    .with_help("to keep no attributes at all, project on none: `r{}`")
}

/// The postfix operator `postfix`, written after `operand` with an empty
/// list, which would leave the operand as it is: what it does, `does.0`,
/// and the label, `does.1`, with `note` on what its list takes. The fix
/// drops it.
fn empty_list(
    postfix: &trendy_syntax::ast::PostfixExpr,
    operand: &Expr,
    does: (&str, &str),
    note: &str,
) -> Diagnostic {
    let spelled = postfix
        .operator()
        .and_then(|t| t.kind().fixed_text())
        .unwrap_or("this");
    let (span, edit) = dropped(postfix, operand);
    Diagnostic::error(
        Code::EmptyDoesNothing,
        format!("`{spelled} {{}}` {}", does.0),
        span,
        does.1,
    )
    .with_note(note.to_string())
    .with_suggestion(format!("drop the `{spelled} {{}}`"), vec![edit])
}

/// `postfix`, an operator applied to `operand`, from its keyword, and the
/// edit that drops it, leaving the operand.
fn dropped(postfix: &trendy_syntax::ast::PostfixExpr, operand: &Expr) -> (Span, Edit) {
    let end = postfix.span().end;
    let start = postfix.operator().map_or(operand.span().end, |t| {
        trendy_syntax::span(t.text_range()).start
    });
    let edit = Edit {
        span: Span::new(operand.span().end, end),
        replacement: String::new(),
    };
    (Span::new(start, end), edit)
}

/// The globals in `cycle`, each referring to the next and the last to
/// the first: an initial value's cycle, through operators or not, or, if
/// it has no variable, read-only operators that call each other.
pub(crate) fn cycle(cycle: &[Pending]) -> Diagnostic {
    // Start at a variable, if there is one, whose initial value has no
    // order to be evaluated in.
    let start = cycle.iter().position(|p| !p.operator).unwrap_or(0);
    let cycle: Vec<&Pending> = cycle[start..].iter().chain(&cycle[..start]).collect();
    let first = cycle[0];
    let names = |operators: bool| -> Vec<String> {
        cycle
            .iter()
            .filter(|p| p.operator == operators)
            .map(|p| format!("`{}`", p.name))
            .collect()
    };
    let d = if first.operator {
        let names = names(true);
        let message = match names.as_slice() {
            [_] => format!("`{}` calls itself", first.name),
            _ => format!("{} call each other", trendy_diag::conjoin(&names)),
        };
        Diagnostic::error(Code::Recursion, message, first.defined, "defined here")
            .with_note(
                "a read-only operator can't call itself, directly or through others: without a condition to stop on, its call would never end",
            )
            .with_help("compute the value without calling the operator from its own body")
    } else {
        let names = names(false);
        let message = match names.as_slice() {
            [_] => format!("the initial value of `{}` depends on itself", first.name),
            _ => format!(
                "the initial values of {} depend on each other",
                trendy_diag::conjoin(&names)
            ),
        };
        Diagnostic::error(Code::CyclicInitialValues, message, first.defined, "declared here")
            .with_note(
                "initial values are evaluated when compiling, each after the ones it refers to, and the read-only operators it calls with the variables they read, so a cycle has no order to evaluate them in",
            )
            .with_help("give one of these variables a value that doesn't refer to the others")
    };
    cycle.iter().enumerate().fold(d, |d, (i, pending)| {
        let next = cycle[(i + 1) % cycle.len()];
        let verb = match (pending.operator, next.operator) {
            (_, true) => "calls",
            (true, false) => "reads",
            (false, false) => "refers to",
        };
        let label = if next.name == pending.name && next.operator == pending.operator {
            format!("`{}` {verb} itself", pending.name)
        } else {
            format!("`{}` {verb} `{}`", pending.name, next.name)
        };
        d.with_secondary(pending.via, label)
    })
}

/// A `call` of the read-only operator `name`, at `span`, defined at
/// `defined`, whose value would be lost.
pub(crate) fn call_of_read_only(name: &str, span: Span, defined: Option<Span>) -> Diagnostic {
    let d = Diagnostic::error(
        Code::CallOfReadOnly,
        format!("`{name}` is a read-only operator, so `call` would lose its value"),
        span,
        "a read-only operator",
    );
    let d = match defined {
        Some(defined) => d.with_secondary(defined, "defined here, with a result type"),
        None => d,
    };
    d.with_note(format!(
        "`call` invokes an update operator, which returns nothing; a read-only operator's call is a value, used in an expression, as in `call debug_value{{v: {name}{{…}}}};`"
    ))
}

/// `stmt`, a `return` in the update operator `name`.
pub(crate) fn return_in_update(name: &str, stmt: &trendy_syntax::ast::ReturnStmt) -> Diagnostic {
    let span = stmt
        .keyword()
        .map_or_else(|| stmt.span(), |t| trendy_syntax::span(t.text_range()));
    Diagnostic::error(
        Code::Return,
        format!("`{name}` is an update operator, which returns no value"),
        span,
        "returns a value",
    )
    .with_note(
        "only a read-only operator, which has a result type, returns a value: `operator name{params}: type [return value;];`",
    )
    .with_help(format!(
        "give `{name}` a result type to make it read-only, or delete the `return`"
    ))
}

/// `stmt`, a statement other than `return` in the read-only operator
/// `name`: a call, or an assignment, which would change something.
pub(crate) fn read_only_effect(name: &str, stmt: &trendy_syntax::ast::Statement) -> Diagnostic {
    let change = format!(
        "to change something, make `{name}` an update operator, without a result type, invoked by `call`"
    );
    let (message, label, help) = match stmt {
        trendy_syntax::ast::Statement::Call(_) => (
            "a read-only operator can't call an operator",
            format!("a `call` in `{name}`"),
            change,
        ),
        trendy_syntax::ast::Statement::Var(_) => (
            "a read-only operator can't declare a variable",
            format!("a `var` in `{name}`"),
            "compute the value in the `return`".to_string(),
        ),
        _ => (
            "a read-only operator can't assign a variable",
            format!("an assignment in `{name}`"),
            change,
        ),
    };
    Diagnostic::error(Code::ReadOnlyEffect, message, stmt.span(), label)
        .with_note(
            "a read-only operator changes nothing (TTM RM Prescription 3): its body is one `return`, of a value computed from its parameters and the variables it reads",
        )
        .with_help(help)
}

/// The read-only operator `name`, whose `body` has no `return`, and whose
/// result type is `written`.
pub(crate) fn missing_return(
    name: &str,
    body: &trendy_syntax::ast::Body,
    written: &trendy_syntax::ast::TypeExpr,
) -> Diagnostic {
    let at = body.end().map_or_else(|| body.span(), Span::empty);
    Diagnostic::error(
        Code::Return,
        format!("read-only operator `{name}` has no `return`"),
        at,
        "no `return`",
    )
    .with_secondary(
        written.span(),
        format!("`{name}` returns a value of this type"),
    )
    .with_note(RETURN_RULE)
    .with_help("return its value: `return …;`")
}

/// `stmt`, a `return` after `first` in a read-only operator.
pub(crate) fn extra_return(
    stmt: &trendy_syntax::ast::ReturnStmt,
    first: &trendy_syntax::ast::ReturnStmt,
) -> Diagnostic {
    Diagnostic::error(
        Code::Return,
        "a read-only operator returns once",
        stmt.span(),
        "another `return`",
    )
    .with_secondary(first.span(), "the first, which gives its value")
    .with_note(RETURN_RULE)
    .with_suggestion(
        "delete it",
        vec![Edit {
            span: stmt.removal(),
            replacement: String::new(),
        }],
    )
}

/// A read-only operator's body, for notes.
const RETURN_RULE: &str = "a read-only operator's body is one statement, `return value;`, with a value of its result type";

/// `expr`, a call of `signature` with constant arguments, whose value
/// fails, as `failed` says.
pub(crate) fn call_failure(
    failed: &crate::ops::Failed,
    expr: &Expr,
    signature: &Signature,
) -> Diagnostic {
    // A built-in's value is the compiler's, with nothing to point at.
    if signature.builtin.is_some() {
        return self::failed(failed, expr);
    }
    let d = self::failed(failed, expr).with_note(format!(
        "computing `{}`'s value for these arguments",
        signature.name
    ));
    let returned = signature
        .decl
        .as_ref()
        .and_then(trendy_syntax::ast::Operator::body)
        .and_then(|body| {
            body.statements().find_map(|s| match s {
                trendy_syntax::ast::Statement::Return(stmt) => stmt.value(),
                _ => None,
            })
        });
    match returned {
        Some(value) => d.with_secondary(value.span(), format!("`{}`'s value", signature.name)),
        None => d,
    }
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

pub(crate) fn approximate_out_of_range(span: Span) -> Diagnostic {
    Diagnostic::error(
        Code::ApproximateOutOfRange,
        "approximate literal is too large",
        span,
        "too large for a 64-bit floating-point number",
    )
    .with_note(format!(
        "`approximate` is a 64-bit IEEE 754 binary floating-point number: the largest is about {:e}",
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

/// An operator written `expr` that has no result because of `failure`. `divisor` is its right operand, where a
/// division by zero is reported.
pub(crate) fn failure(failure: hir::Failure, expr: &Expr, divisor: Option<&Expr>) -> Diagnostic {
    match failure {
        hir::Failure::Overflow(hir::Scalar::Approximate) => Diagnostic::error(
            Code::Overflow,
            "approximate overflow",
            expr.span(),
            "the result is too large for an approximate number",
        )
        .with_note(format!(
            "approximate numbers are finite, up to about {:e} in magnitude; a larger result is an error, not an infinity",
            f64::MAX
        )),
        hir::Failure::Overflow(_) => Diagnostic::error(
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
        hir::Failure::DivisionByZero => Diagnostic::error(
            Code::DivisionByZero,
            "division by zero",
            divisor.map_or(expr.span(), Expr::span),
            "this is zero",
        )
        .with_note("`/`, `div` and `mod` have no result when the divisor is zero"),
    }
}

/// How a type's values are named in the plural.
/// A note on a type parameter's values: only what applies to any type of
/// what it can be applies to them.
trait ParameterNote {
    /// The diagnostic, with the note if `ty` is a type parameter.
    fn with_parameter_note(self, ty: &hir::Type) -> Self;
}

impl ParameterNote for Diagnostic {
    fn with_parameter_note(self, ty: &hir::Type) -> Self {
        let hir::Type::Parameter(p) = ty else {
            return self;
        };
        self.with_note(match &p.any {
            hir::Any::Kind(kind) => {
                let required: Vec<String> = kind
                    .requires
                    .iter()
                    .map(|r| format!("`{}`", r.text()))
                    .collect();
                format!(
                    "`{p}` can be any type of kind `{}`, so only what every one has applies to its values: `=` and `<>`, what `{}` requires, {}, and passing them on",
                    kind.name,
                    kind.name,
                    trendy_diag::conjoin(&required)
                )
            }
            hir::Any::Type => format!(
                "`{p}` can be any type, so only what applies to every type applies to its values: `=` and `<>`, and passing them on"
            ),
            hir::Any::Tuple => format!(
                "`{p}` can be any tuple, so its attributes aren't known here: only `=` and `<>` apply to its values, and passing them on"
            ),
            hir::Any::Relation => format!(
                "`{p}` can be any relation, so its attributes aren't known here: only `=` and `<>` apply to its values, and `<=`, `<`, `>=` and `>` as sets, and passing them on"
            ),
        })
    }
}

/// `d`, with how to apply `op`, written `operator`, to values of the type
/// parameter `p`: as a type of one of `kinds` that requires it, and what
/// `p`'s own kind requires.
fn kind_help(
    d: Diagnostic,
    op: crate::ops::Operator,
    p: &hir::TypeParameter,
    operator: &str,
    kinds: &[hir::Kind],
) -> Diagnostic {
    use crate::ops::Operator;
    let Some(needed) = op.requirement() else {
        return d;
    };
    let none = std::collections::BTreeSet::new();
    let has = match &p.any {
        hir::Any::Kind(kind) => &kind.requires,
        hir::Any::Type | hir::Any::Tuple | hir::Any::Relation => &none,
    };
    let any = |fits: &dyn Fn(&hir::Kind) -> bool| {
        let names: Vec<String> = kinds
            .iter()
            .filter(|k| k.requires.contains(&needed) && fits(k))
            .map(|k| format!("`any {}`", k.name))
            .collect();
        (!names.is_empty()).then(|| trendy_diag::disjoin(&names))
    };
    let values = p.name.as_ref().map_or_else(
        || "these values".to_string(),
        |name| format!("values of `{name}`"),
    );
    let doing = match op {
        Operator::Compare(_) => format!("compare {values} with `{operator}`"),
        Operator::Add => format!("add {values} with `{operator}`"),
        Operator::Multiply => format!("multiply {values} with `{operator}`"),
        _ => format!("apply `{operator}` to {values}"),
    };
    let declare = |what: &str| match &p.name {
        Some(name) => format!("declare `{name}` as {what}"),
        None => format!("write {what} for the parameter's type"),
    };
    if let Some(what) = any(&|k| has.is_subset(&k.requires)) {
        return d.with_help(format!(
            "to {doing}, {}, whose types all have it",
            declare(&what)
        ));
    }
    // No kind requires both: the other one, if nothing needs what `p`'s
    // kind requires.
    match (&p.any, any(&|_| true)) {
        (hir::Any::Kind(kind), Some(what)) => {
            let required: Vec<String> = has.iter().map(|r| format!("`{}`", r.text())).collect();
            d.with_help(format!(
                "to {doing}, {} instead, if nothing else needs what `{}` requires, {}: no kind requires `{}` as well",
                declare(&what),
                kind.name,
                trendy_diag::conjoin(&required),
                needed.text()
            ))
        }
        _ => d,
    }
}

fn plural(ty: &hir::Type) -> String {
    match ty {
        hir::Type::Scalar(hir::Scalar::Boolean) => "booleans".to_string(),
        hir::Type::Scalar(hir::Scalar::Integer) => "integers".to_string(),
        hir::Type::Scalar(hir::Scalar::Approximate) => "approximate numbers".to_string(),
        hir::Type::Scalar(hir::Scalar::Text) => "text".to_string(),
        hir::Type::Tuple(_) => "tuples".to_string(),
        hir::Type::Relation(_) => "relations".to_string(),
        hir::Type::Parameter(p) => format!("values of `{p}`"),
    }
}

/// The operator `op`, written `token`, applied to operands of type `ty`,
/// which it doesn't take, as `first` is, if it is known: the first operand,
/// which set the type.
pub(crate) fn wrong_operand_type(
    op: crate::ops::Operator,
    token: &SyntaxToken,
    ty: &hir::Type,
    first: Option<&Expr>,
    kinds: &[hir::Kind],
) -> Diagnostic {
    use crate::ops::Operator;
    let span = trendy_syntax::span(token.text_range());
    let d = Diagnostic::error(
        Code::WrongOperandType,
        format!("`{}` can't be applied to {}", token.text(), plural(ty)),
        span,
        format!("`{}` takes {}", token.text(), op.takes()),
    )
    .with_parameter_note(ty)
    .with_note("values never convert to another type implicitly");
    // `n || " people"`: the text was meant, not `||` of numbers or `T`s.
    let to_text = conversion(ty, &hir::Type::Scalar(hir::Scalar::Text));
    if let (Operator::Concatenate, Some(builtin), Some(first)) = (op, to_text, first)
        && *ty != hir::Type::Scalar(hir::Scalar::Boolean)
    {
        let subject = format!("`{}`", first.syntax().text());
        return with_conversion(d, &subject, builtin, first);
    }
    let hir::Type::Scalar(scalar) = *ty else {
        return match (op, ty) {
            (_, hir::Type::Parameter(p)) => kind_help(d, op, p, token.text(), kinds),
            (Operator::Compare(_), _) => d.with_note(
                "tuples have no order: they can only be compared with `=` and `<>`; relations can also be compared as sets, with `<=` and `<` for subset and proper subset",
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
        // Of integers, `div` was likelier meant; of an integer by an
        // approximate number, the integer's conversion.
        (Operator::Divide, hir::Scalar::Integer) => {
            let d = replace(
                d.with_note(
                    "`/` divides approximate numbers; integer division is `div`, which truncates toward zero",
                ),
                "div",
                "use integer division: `div`",
            );
            match first {
                Some(first) => {
                    let subject = format!("`{}`", first.syntax().text());
                    with_conversion(d, &subject, hir::Builtin::ToApproximate, first)
                }
                None => d,
            }
        }
        (Operator::Div, hir::Scalar::Approximate) => replace(
            d.with_note("`div` is integer division; approximate numbers divide with `/`"),
            "/",
            "use approximate division: `/`",
        ),
        (Operator::Mod, hir::Scalar::Approximate) => d.with_note(
            "`mod` is the remainder of integer division; approximate numbers have no remainder operator",
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

/// A key's attributes, in the order written: `{ a, b }`, or `{}`.
fn key_text(key: &Key) -> String {
    let names: Vec<String> = key.names().map_or_else(Vec::new, |list| {
        list.names()
            .filter_map(|name| Some(name.ident()?.text().to_string()))
            .collect()
    });
    if names.is_empty() {
        "{}".to_string()
    } else {
        format!("{{ {} }}", names.join(", "))
    }
}

/// `keys`, on the variable `name` of type `ty`, which isn't a relation.
pub(crate) fn key_not_relation(name: &str, ty: &hir::Type, keys: &[Key]) -> Diagnostic {
    let d = Diagnostic::error(
        Code::KeyNotRelation,
        format!("`{name}` isn't a relation, so it can't have a key"),
        keys[0].span(),
        format!("a key on a variable of type `{ty}`"),
    )
    .with_note(
        "a key says that no two tuples of a relation have the same values for its attributes, so only a relation variable has keys",
    );
    let removals = keys
        .iter()
        .map(|key| Edit {
            span: key.removal(),
            replacement: String::new(),
        })
        .collect();
    let what = if keys.len() == 1 {
        "the key"
    } else {
        "the keys"
    };
    d.with_suggestion(format!("remove {what}"), removals)
}

/// `key`, with the attributes `attributes`, which is redundant because of
/// `other`, with the attributes `others`: the same, or a subset.
pub(crate) fn redundant_key(
    key: &Key,
    attributes: &BTreeSet<String>,
    other: &Key,
    others: &BTreeSet<String>,
) -> Diagnostic {
    let (text, other_text) = (key_text(key), key_text(other));
    let d = if attributes == others {
        Diagnostic::error(
            Code::RedundantKey,
            format!("key `{text}` is declared twice"),
            key.span(),
            "declared again here",
        )
        .with_secondary(other.span(), "first declared here")
        .with_note("each key is declared once")
    } else {
        Diagnostic::error(
            Code::RedundantKey,
            format!("key `{text}` contains key `{other_text}`, so it isn't a key"),
            key.span(),
            format!("contains `{other_text}`"),
        )
        .with_secondary(other.span(), "already a key")
        .with_note(format!(
            "a key is irreducible: no smaller set of its attributes tells the tuples apart, but `{other_text}` already does"
        ))
    };
    d.with_suggestion(
        format!("remove key `{text}`"),
        vec![Edit {
            span: key.removal(),
            replacement: String::new(),
        }],
    )
}

/// Two tuples of the variable `name` with the same `value` for `key`:
/// one at `at`, and the other at `first` if it has a place of its own.
pub(crate) fn key_violated(
    name: &str,
    key: &Key,
    value: &hir::TupleValue,
    at: Span,
    first: Option<Span>,
) -> Diagnostic {
    let attributes: Vec<String> = value
        .attributes()
        .map(|(a, v)| format!("{a}: {v}"))
        .collect();
    let empty = attributes.is_empty();
    let shared = format!("`{}`", attributes.join(", "));
    let (label, first_label) = match (empty, first.is_some()) {
        (true, true) => ("a second tuple".to_string(), "a first tuple".to_string()),
        (true, false) => ("more than one tuple".to_string(), String::new()),
        (false, true) => (
            format!("has {shared} too"),
            format!("first tuple with {shared}"),
        ),
        (false, false) => (format!("has two tuples with {shared}"), String::new()),
    };
    let mut d = Diagnostic::error(
        Code::KeyViolated,
        format!("two tuples of `{name}` have the same value for one of its keys"),
        at,
        label,
    );
    if let Some(first) = first {
        d = d.with_secondary(first, first_label);
    }
    d.with_secondary(key.span(), "the key")
        .with_note(if empty {
            "`key {}` allows at most one tuple, since every tuple has the same values for no attributes"
        } else {
            "no two tuples can have the same values for a key's attributes"
        })
        .with_help("correct or remove one of these tuples, or declare the key that was meant")
}
