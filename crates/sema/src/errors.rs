//! Sema's diagnostics. Each one says where the fix goes and, when the fix is
//! mechanical, carries it.

use trendy_diag::{Code, Diagnostic, Edit, Span, edit_distance};
use trendy_hir as hir;
use trendy_syntax::SyntaxToken;
use trendy_syntax::ast::{AstNode, Expr, SourceFile, TupleLiteral, TypeExpr};

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

/// `name` is defined at `this` but was already defined at `first`.
pub(crate) fn duplicate_name(name: &str, this: (Kind, Span), first: (Kind, Span)) -> Diagnostic {
    Diagnostic::error(
        Code::DuplicateName,
        format!("`{name}` is defined more than once"),
        this.1,
        format!("redefined here as {}", this.0.describe()),
    )
    .with_secondary(
        first.1,
        format!("`{name}` is first defined here as {}", first.0.describe()),
    )
    .with_note("variables and operators share one namespace, so every name must be unique")
    .with_help("delete the duplicate definition, or rename one of them")
}

pub(crate) fn missing_main(file: &SourceFile, values: &Values) -> Diagnostic {
    const MESSAGE: &str = "program has no `main` operator";
    const ENTRY_NOTE: &str = "execution starts in the operator named `main`";

    if let Some((Kind::Variable, span)) = values.get("main") {
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
        .min_by_key(|&(_, _, span)| span.start);
    if let Some((name, _, span)) = near_miss {
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

/// A value written as `expr` whose type isn't `declared`, written as
/// `type_expr`.
pub(crate) fn type_mismatch(
    declared: &hir::Type,
    type_expr: &TypeExpr,
    value: &hir::Value,
    expr: &Expr,
) -> Diagnostic {
    let found = value.ty();
    let mut d = Diagnostic::error(
        Code::TypeMismatch,
        format!("mismatched types: expected `{declared}`, found `{found}`"),
        expr.span(),
        format!("this is `{found}`"),
    )
    .with_secondary(type_expr.span(), "expected because of this type")
    .with_note("values never convert to another type implicitly");

    // Rewriting the literal is the likelier fix when it is mechanical, so it
    // comes first; changing the declared type always works.
    if let (hir::Type::Scalar(scalar), Expr::Literal(literal)) = (declared, expr) {
        let written = literal.token().map(|t| t.text().to_string());
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
    d.with_suggestion(
        format!("change the type to `{found}`"),
        vec![Edit {
            span: type_expr.span(),
            replacement: found.to_string(),
        }],
    )
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
        hir::Value::Integer(n) if *n >= 0 => n.to_string(),
        hir::Value::Rational(r) => {
            let r = r.get();
            // Debug formatting always includes a point or an exponent.
            (r.is_finite() && r.is_sign_positive()).then(|| format!("{r:?}"))?
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
        hir::Value::Integer(_) => return None,
    })
}

/// An attribute named `name`, at `span`, appearing more than once, first at
/// `first`.
pub(crate) fn duplicate_attribute(name: &str, span: Span, first: Span) -> Diagnostic {
    Diagnostic::error(
        Code::DuplicateAttribute,
        format!("attribute `{name}` is defined more than once"),
        span,
        "redefined here",
    )
    .with_secondary(first, format!("`{name}` is first defined here"))
    .with_note("a tuple's attributes are a set, so each name can appear only once")
    .with_help("rename or remove the duplicate")
}

/// A tuple value, written as `literal`, without the `missing` attributes of
/// its declared type `ty`.
pub(crate) fn missing_attributes(
    missing: &[(&str, &hir::Type)],
    ty: &hir::Type,
    literal: &TupleLiteral,
) -> Diagnostic {
    let names: Vec<_> = missing
        .iter()
        .map(|(name, _)| format!("`{name}`"))
        .collect();
    let noun = if missing.len() == 1 {
        "attribute"
    } else {
        "attributes"
    };
    let d = Diagnostic::error(
        Code::MissingAttribute,
        format!("missing {noun} {}", trendy_diag::conjoin(&names)),
        literal.span(),
        format!("this value has no {}", trendy_diag::conjoin(&names)),
    )
    .with_note(format!("the declared type is `{ty}`"))
    .with_note("a tuple value must have every attribute of its type");

    let attributes = missing
        .iter()
        .map(|(name, ty)| Some(format!("{name}: {}", source_text(&ty.default_value())?)))
        .collect::<Option<Vec<_>>>();
    let Some(attributes) = attributes else {
        return d;
    };
    let (at, before, after) = literal.insertion();
    d.with_suggestion(
        format!("add {} with default values", trendy_diag::conjoin(&names)),
        vec![Edit {
            span: Span::empty(at),
            replacement: format!("{before}{}{after}", attributes.join(", ")),
        }],
    )
}

/// An attribute `name`, at `span`, that the declared type `ty` doesn't
/// have. `rename` is the missing attribute it was probably meant to be, and
/// `removal` the span that removes it.
pub(crate) fn unknown_attribute(
    name: &str,
    span: Span,
    ty: &hir::Type,
    rename: Option<&str>,
    removal: Span,
) -> Diagnostic {
    let mut d = Diagnostic::error(
        Code::UnknownAttribute,
        format!("unknown attribute `{name}`"),
        span,
        "not an attribute of the declared type",
    )
    .with_note(format!("the declared type is `{ty}`"));
    if let Some(target) = rename {
        d = d.with_suggestion(
            format!("rename it to `{target}`"),
            vec![Edit {
                span,
                replacement: target.to_string(),
            }],
        );
    }
    d.with_suggestion(
        format!("remove `{name}`"),
        vec![Edit {
            span: removal,
            replacement: String::new(),
        }],
    )
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
        "integers are 64-bit signed: the largest is {}",
        i64::MAX
    ))
    .with_note("there are no negative literals yet, because there is no unary minus")
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
