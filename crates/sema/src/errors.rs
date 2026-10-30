//! Sema's diagnostics. Each one says where the fix goes and, when the fix is
//! mechanical, carries it.

use std::collections::BTreeSet;

use trendy_diag::{Code, Diagnostic, Edit, Span, closest, edit_distance, misspelling};
use trendy_hir as hir;
use trendy_syntax::ast::{
    self, AstNode, AttributeValue, Expr, Key, Mode, NaryExpr, OrderList, ProjectExpr, SourceFile,
    TypeExpr, VarDecl,
};
use trendy_syntax::{SyntaxKind, SyntaxToken};

use crate::check::Written;
use crate::globals::{Global, Pending};
use crate::operators::{Chosen, Operators, Signature};
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
    let note = if this.0 == Kind::Variable {
        "variables have a namespace of their own, so no two can have the same name, though a variable can have an operator's"
    } else {
        "headings share a namespace with the types, so no two can have the same name, nor a heading a built-in type's, though a heading can have a variable's or an operator's"
    };
    d.with_note(note)
        .with_help("delete the duplicate definition, or rename one of them")
}

/// The type `name`, declared at `declared`, whose name is that of an
/// operator of kind `kind`, the program's defined at `operator`, or the
/// prelude's: a type's name is its selector's, which no other operator can
/// have.
pub(crate) fn selector_name(
    name: &str,
    declared: Span,
    kind: Kind,
    operator: Option<Span>,
) -> Diagnostic {
    let d = match operator {
        Some(operator) => Diagnostic::error(
            Code::SelectorName,
            format!("operator `{name}` has the name of a type"),
            operator,
            "the type's selector has this name",
        )
        .with_secondary(declared, format!("the type `{name}` is declared here")),
        None => Diagnostic::error(
            Code::SelectorName,
            format!(
                "type `{name}` has the name of {} in the prelude",
                kind.describe()
            ),
            declared,
            "its selector would have the prelude operator's name",
        ),
    };
    d.with_note(format!(
        "a type's selector has its name: `{name}{{…}}` selects a value of the type from its components, so no other operator can have the name"
    ))
    .with_help("rename the operator or the type")
}

/// The operator `name`, defined at `span` with parameters named `params`,
/// which `first`, of that name, already has: built in, the prelude's, or
/// the program's, there.
pub(crate) fn duplicate_operator(
    name: &str,
    span: Span,
    first: (Kind, Option<Span>),
    params: &BTreeSet<String>,
) -> Diagnostic {
    let listed: Vec<String> = params.iter().map(|p| format!("`{p}`")).collect();
    let same = if listed.is_empty() {
        "no parameters".to_string()
    } else {
        format!("parameters named {}", trendy_diag::conjoin(&listed))
    };
    let d = Diagnostic::error(
        Code::DuplicateName,
        format!("`{name}` is defined more than once with {same}"),
        span,
        "redefined here as an operator",
    );
    let d = match first.1 {
        Some(at) => d.with_secondary(
            at,
            format!("`{name}` is first defined here as {}", first.0.describe()),
        ),
        None => d.with_note(format!(
            "`{name}` is {}, declared in the prelude",
            first.0.describe()
        )),
    };
    d.with_note("operators have a namespace of their own, in which operators of one name are told apart by their parameters' names, which each call names its arguments with; so no two have the same name and the same parameters' names, though an operator can have a variable's name")
        .with_help("delete the duplicate definition, or rename it or one of its parameters")
}

/// What a local variable's name must be.
const LOCAL_NAMES: &str = "an operator's names, its parameters' and its local variables', each mean one thing wherever they are in scope, so no two there are the same, and a block's hide no other; they hide the program's variables of their names";

/// The local variable `name`, declared by `decl` in what `within`
/// describes, such as an operator's body, which `first` declares already.
pub(crate) fn local_declared_again(
    name: &str,
    decl: &VarDecl,
    first: &crate::locals::Declaration,
    within: &str,
) -> Diagnostic {
    let named = decl.name().map_or_else(|| decl.span(), |n| n.span());
    let d = Diagnostic::error(
        Code::DuplicateName,
        format!("`{name}` is declared again in {within}"),
        named,
        "declared again here",
    )
    .with_secondary(
        first.name_span(),
        format!("`{name}` is first declared here"),
    )
    .with_note(LOCAL_NAMES);
    // `var total := total + 1;`, as Rust declares a variable again to give
    // it a new value: the first one's assignment was meant, if it is
    // declared before, by a `var`, as a loop's variable can't be assigned.
    let before = matches!(first, crate::locals::Declaration::Var(_))
        && first.name_span().start < decl.span().start;
    match (
        decl.name(),
        decl.type_expr(),
        decl.initializer(),
        decl.keys().next(),
    ) {
        (Some(new), None, Some(_), None) if before => d.with_suggestion(
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

/// A local variable `name`, used at `span`, before `decl` declares it,
/// which hides a variable of the program's, if `hides` says so.
pub(crate) fn local_before_declaration(
    name: &str,
    span: Span,
    decl: &VarDecl,
    hides: bool,
) -> Diagnostic {
    let declared = decl.span();
    let named = decl.name().map_or(declared, |n| n.span());
    // `var n := n + 1;`, as Rust declares a variable from one it hides.
    let own = declared.start <= span.start && span.end <= declared.end;
    let (label, help) = if own && hides {
        (
            "declared here, with a value computed from it",
            format!(
                "to compute it from the program's variable `{name}`, give the local variable a name of its own"
            ),
        )
    } else if own {
        (
            "declared here, with a value computed from it",
            format!("compute `{name}`'s initial value from something else"),
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
    .with_note("a local variable is in scope in all of the block it is declared in, so that its name means one thing there, hiding any variable of the program's, but it has a value only once its declaration gives it one")
    .with_help(help)
}

/// The local variable `name`, used at `span`, outside the block `decl`
/// declares it in, which has ended.
pub(crate) fn local_out_of_scope(
    name: &str,
    span: Span,
    decl: &crate::locals::Declaration,
) -> Diagnostic {
    let named = decl.name_span();
    Diagnostic::error(
        Code::UnknownName,
        format!("local variable `{name}` isn't in scope here"),
        span,
        "used outside its block",
    )
    .with_secondary(named, "declared in a block that ends before here")
    .with_note("a local variable is in scope in the block it is declared in, from its declaration to the block's end")
    .with_help(format!(
        "to use `{name}` here too, declare it before the block, in the one that holds both"
    ))
}

/// A variable `name`, of type `ty`, written `type_expr`, which has no
/// default, without an initial value: a type with a type parameter in it,
/// or with a type of the program's own.
pub(crate) fn no_default_value(name: &str, ty: &hir::Type, type_expr: &TypeExpr) -> Diagnostic {
    let why = if ty.parameters().next().is_some() {
        "each call gives a type parameter its type, so a type with one in it has no default"
    } else {
        "no value of a type of the program's own is any more a start than another, so it has no default, nor has a tuple with one"
    };
    Diagnostic::error(
        Code::UntypedVariable,
        format!("variable `{name}`, of type `{ty}`, has no initial value"),
        type_expr.span(),
        "a type without a default",
    )
    .with_note(format!(
        "without a value, a variable starts at its type's default, but {why}"
    ))
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
                .chain(namespaces.defined_types())
                .chain(declared.iter().copied())
                .collect();
            closest(name, types).map(written)
        });
    if let Some((meant, replacement)) = meant {
        if TYPE_ALIASES.iter().any(|(alias, _)| *alias == lowercase) {
            d = d.with_note(format!("trendy's name for this type is `{meant}`"));
        }
        d = d.with_suggestion(format!("use `{meant}`"), vec![Edit { span, replacement }]);
    }
    d
}

const ANY_ONLY_IN_PARAMETERS: &str = "only a parameter's type can be `any type`, `any tuple`, `any relation` or `any array`: each call gives it the type of its argument, and nothing would give one written anywhere else a type";
const ANY_ELSEWHERE: &str = "to use the type of an argument here, declare a type parameter after the signature, `types { T: any type }`, and write `T` both here and as the parameter's type";
const VARIABLE_TYPES: &str = "a variable's type, and a heading's attributes', are always specific types, such as `integer` or `relation { name: text }`";

/// The heading named `ident`, declared at `declared`, written as a type,
/// as other languages name a type of records: its tuple and relation types
/// are what the fixes write.
pub(crate) fn heading_as_type(ident: &SyntaxToken, declared: Span) -> Diagnostic {
    let name = ident.text();
    let span = trendy_syntax::span(ident.text_range());
    let fix = |word: &str| Edit {
        span: Span::empty(span.start),
        replacement: format!("{word} "),
    };
    Diagnostic::error(
        Code::UnknownType,
        format!("`{name}` is a heading, not a type"),
        span,
        "a heading",
    )
    .with_secondary(declared, format!("`{name}` is declared here"))
    .with_note(format!(
        "a heading is the attributes of tuples and relations alike, so it has two types: `relation {name}`, of relations, and `tuple {name}`, of tuples"
    ))
    .with_suggestion(format!("a relation: `relation {name}`"), vec![fix("relation")])
    .with_suggestion(format!("a tuple: `tuple {name}`"), vec![fix("tuple")])
}

/// `literal`, a tuple selector written with `ident`, the name of a type of
/// the program's own declared at `declared`, as its heading: a value of the
/// type was meant, which its selector, named after it, selects from its
/// components, which the tuple's attributes give.
pub(crate) fn tuple_of_type(
    ident: &SyntaxToken,
    literal: &trendy_syntax::ast::TupleLiteral,
    declared: Span,
) -> Diagnostic {
    let name = ident.text();
    let components: Vec<String> = literal
        .attributes()
        .map(|a| a.syntax().text().to_string())
        .collect();
    Diagnostic::error(
        Code::UnknownType,
        format!("`{name}` is a type, not a heading"),
        trendy_syntax::span(ident.text_range()),
        "a type of the program's own",
    )
    .with_secondary(declared, format!("`{name}` is declared here"))
    .with_note(format!(
        "a value of a type of the program's own is selected by its selector, named after it, from its components, as in `{name}{{…}}`; `tuple h {{ … }}` is a tuple of the heading `h`"
    ))
    .with_suggestion(
        format!("select a value of `{name}`"),
        vec![Edit {
            span: literal.span(),
            replacement: format!("{name}{{{}}}", components.join(", ")),
        }],
    )
}

/// The heading named `ident`, after `relation` or `tuple`, which isn't one.
pub(crate) fn unknown_heading(ident: &SyntaxToken, namespaces: &Namespaces) -> Diagnostic {
    let name = ident.text();
    let span = trendy_syntax::span(ident.text_range());
    let d = Diagnostic::error(
        Code::UnknownType,
        format!("unknown heading `{name}`"),
        span,
        "not a heading",
    )
    .with_note(
        "after `relation` or `tuple` comes a heading: its attributes, as in `relation { name: text }`, or the name of one declared with `heading name := { name: text };`",
    );
    let kind = hir::Scalar::from_name(name)
        .map(|_| Kind::Type)
        .or_else(|| namespaces.defined(name).map(|_| Kind::Defined))
        .or_else(|| namespaces.variable(name).map(|_| Kind::Variable))
        .or_else(|| namespaces.operator(name).map(|(kind, _)| kind));
    if let Some(kind) = kind {
        let d = d.with_note(format!("`{name}` is {}, not a heading", kind.describe()));
        return match kind {
            Kind::Type | Kind::Defined => d.with_help(format!(
                "a tuple or a relation of `{name}` values names an attribute for them, as in `relation {{ value: {name} }}`"
            )),
            _ => d.with_help(
                "write the attributes in braces, or declare a heading and write its name",
            ),
        };
    }
    let mut headings: Vec<&str> = namespaces.headings().collect();
    headings.sort_unstable();
    spelled_like(
        d,
        name,
        span,
        &headings,
        &format!("declare it: `heading {name} := {{ … }};`, or write the attributes in braces"),
    )
}

/// The headings in `cycle`, each naming the next in its attributes' types,
/// and the last the first.
pub(crate) fn cyclic_types(cycle: &[crate::named_types::Cycled]) -> Diagnostic {
    let first = &cycle[0];
    let names: Vec<String> = cycle.iter().map(|c| format!("`{}`", c.name)).collect();
    let message = match names.as_slice() {
        [_] => format!("the {} `{}` contains itself", first.what, first.name),
        _ if cycle.iter().all(|c| c.what == first.what) => format!(
            "the {}s {} contain each other",
            first.what,
            trendy_diag::conjoin(&names)
        ),
        _ => format!("{} contain each other", trendy_diag::conjoin(&names)),
    };
    let d = Diagnostic::error(Code::CyclicTypes, message, first.declared, "declared here")
        .with_note(
            "a heading's attributes and a type's components can be of other headings' and types' types, but not of their own, directly or through others: the type would contain itself (TTM, chapter 6, RM Prescription 6)",
        )
        .with_help(
            "keep a hierarchy as a relation of pairs instead, each with one it contains, as in `heading assembly := { major: text, minor: text };`",
        );
    cycle.iter().enumerate().fold(d, |d, (i, cycled)| {
        let next = &cycle[(i + 1) % cycle.len()];
        let label = if next.name == cycled.name {
            format!("`{}` contains itself here", cycled.name)
        } else {
            format!("`{}` contains `{}` here", cycled.name, next.name)
        };
        d.with_secondary(cycled.via, label)
    })
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
    let mut d = origin.explain(
        Diagnostic::error(
            Code::TypeMismatch,
            format!("mismatched types: expected `{declared}`, found `{found}`"),
            expr.span(),
            format!("this is `{found}`"),
        )
        .with_note("values never convert to another type implicitly"),
    );
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
    } else if let Some(builtin) = conversion(found, declared)
        && (value.is_none() || !parses(builtin))
    {
        // A text known when compiling is one parsing reads, rewritten
        // above as the value's literal, or one it would fail for.
        d = with_conversion(d, "it", builtin, expr);
    }
    // `n * x`, of an integer and an approximate number: the first operand
    // set the type, and as an approximate number it takes the rest.
    if let Origin::Value(first) = origin
        && conversion(declared, found) == Some(hir::ReadOnlyBuiltin::ToApproximate)
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
            None => with_conversion(d, &subject, hir::ReadOnlyBuiltin::ToApproximate, first),
        };
    }
    // Only a written type can be changed to fit the value, and only the
    // value's own: the other operands of an operator whose result has the
    // type, as `"n = "` in `"n = " || n` returned as `text`, wouldn't fit.
    let (operand, origin) = match origin {
        Origin::Operand(_, within) => (true, &**within),
        origin => (false, origin),
    };
    if let (Origin::Loaded(..), Some(fix)) = (origin, removal_fix(declared, found, expr)) {
        return d.with_suggestion(fix.0, fix.1);
    }
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
        | Origin::Component(..)
        | Origin::Variable(..)
        | Origin::Loaded(..)
        | Origin::Element(..)
        | Origin::Heading(_)
        | Origin::Value(_)
        | Origin::Builtin
        | Origin::Columns(_)
        | Origin::Operator(_)
        | Origin::Operand(..) => d,
    }
}

/// The fix for a load of `expr`, a relation of type `found`, into an array
/// of tuples whose relation would be `declared`: if its tuples have the
/// elements' attributes and others, the load keeps only those, after the
/// order.
fn removal_fix(
    declared: &hir::Type,
    found: &hir::Type,
    expr: &Expr,
) -> Option<(String, Vec<Edit>)> {
    let (hir::Type::Relation(want), hir::Type::Relation(have)) = (declared, found) else {
        return None;
    };
    if !want.attributes().all(|(a, ty)| have.get(a) == Some(ty)) {
        return None;
    }
    let load = expr
        .syntax()
        .ancestors()
        .find_map(trendy_syntax::ast::Load::cast)?;
    let wanted: Vec<&str> = want.attributes().map(|(a, _)| a).collect();
    let (help, edit) = keeping_edit(have, &wanted, &load)?;
    Some((help, vec![edit]))
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

/// The prelude's operator that converts a value of type `from` to type
/// `to`, if there is one: `to_text` writes a value of any type as text,
/// `to_approximate` takes integers, and `parse_integer`,
/// `parse_approximate` and `parse_boolean` read values from text, failing
/// for other text.
fn conversion(from: &hir::Type, to: &hir::Type) -> Option<hir::ReadOnlyBuiltin> {
    use hir::{Scalar, Type};
    match (from, to) {
        (Type::Scalar(Scalar::Integer), Type::Scalar(Scalar::Approximate)) => {
            Some(hir::ReadOnlyBuiltin::ToApproximate)
        }
        (Type::Scalar(Scalar::Text), Type::Scalar(Scalar::Integer)) => {
            Some(hir::ReadOnlyBuiltin::ParseInteger)
        }
        (Type::Scalar(Scalar::Text), Type::Scalar(Scalar::Approximate)) => {
            Some(hir::ReadOnlyBuiltin::ParseApproximate)
        }
        (Type::Scalar(Scalar::Text), Type::Scalar(Scalar::Boolean)) => {
            Some(hir::ReadOnlyBuiltin::ParseBoolean)
        }
        (Type::Scalar(_) | Type::Parameter(_), Type::Scalar(Scalar::Text)) => {
            Some(hir::ReadOnlyBuiltin::ToText)
        }
        _ => None,
    }
}

/// Whether the conversion `builtin` reads a value from text, which fails
/// for other text.
fn parses(builtin: hir::ReadOnlyBuiltin) -> bool {
    matches!(
        builtin,
        hir::ReadOnlyBuiltin::ParseInteger
            | hir::ReadOnlyBuiltin::ParseApproximate
            | hir::ReadOnlyBuiltin::ParseBoolean
    )
}

/// `d`, with the fix converting `expr`, which the message calls `subject`,
/// with the built-in `builtin`: a call of it around `expr`.
fn with_conversion(
    d: Diagnostic,
    subject: &str,
    builtin: hir::ReadOnlyBuiltin,
    expr: &Expr,
) -> Diagnostic {
    let name = builtin.name();
    let span = expr.span();
    let verb = if parses(builtin) { "parse" } else { "convert" };
    d.with_suggestion(
        format!(
            "{verb} {subject} with `{name}`: `{name}{{from: {}}}`",
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

/// Whether `kind` requires only an order, which a type of the program's own
/// has when it declares one, so that it is of the kind.
fn orders_only(kind: &hir::Kind) -> bool {
    kind.requires.iter().all(|&r| r == hir::Requirement::Order)
}

/// What a type of `parameter`, `any tuple`, `any relation`, `any array`
/// or of a kind, is, as in "a tuple", and the rule that says so.
fn what_any_is(parameter: &hir::TypeParameter, namespaces: &Namespaces) -> (String, String) {
    match &parameter.any {
        hir::Any::Tuple => (
            "a tuple".to_string(),
            format!("a parameter of type `{parameter}` takes a tuple of any heading"),
        ),
        hir::Any::Relation => (
            "a relation".to_string(),
            format!("a parameter of type `{parameter}` takes a relation of any heading"),
        ),
        hir::Any::Array => (
            "an array".to_string(),
            format!("a parameter of type `{parameter}` takes an array of any elements"),
        ),
        hir::Any::Kind(kind) => {
            let mut members: Vec<String> = namespaces
                .members(kind)
                .iter()
                .map(|m| format!("`{}`", m.name()))
                .collect();
            if orders_only(kind) {
                members.push("types of the program's own with an order".to_string());
            }
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
    }
}

/// The note for a type parameter `parameter` in a parameter's type, such
/// as the `T` of `array T`, that an argument's type has `actual` in place
/// of, which it can't be.
pub(crate) fn unadmitted(
    parameter: &hir::TypeParameter,
    actual: &hir::Type,
    namespaces: &Namespaces,
) -> String {
    let (what, _) = what_any_is(parameter, namespaces);
    format!(
        "`{parameter}` is `{}`, so it is {what}, which `{actual}` isn't",
        parameter.any
    )
}

/// An argument written `arg`, of type `actual`, for a parameter of type
/// `parameter`, `any tuple`, `any relation` or `any array`, written as
/// `origin` says, which it isn't.
pub(crate) fn not_of_kind(
    parameter: &hir::TypeParameter,
    origin: &Origin,
    actual: &hir::Type,
    arg: &Expr,
    namespaces: &Namespaces,
) -> Diagnostic {
    let (expected, note) = what_any_is(parameter, namespaces);
    let mut d = origin.explain(
        Diagnostic::error(
            Code::TypeMismatch,
            format!("mismatched types: expected {expected}, found `{actual}`"),
            arg.span(),
            format!("this is `{actual}`"),
        )
        .with_note(note),
    );
    // A type of the program's own, which an order makes ordered.
    if let (hir::Type::Defined(defined), hir::Any::Kind(kind)) = (actual, &parameter.any)
        && orders_only(kind)
    {
        d = d.with_help(format!(
            "give `{}` an order, `order [asc …]` in its braces, its components in the order they compare",
            defined.name
        ));
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
        // `"yes"` or `"0"`, as `parse_boolean` reads them.
        (hir::Scalar::Boolean, hir::Value::Text(text)) => {
            trendy_text::parse_boolean(text).ok().map(|b| b.to_string())
        }
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
    /// An attribute a load orders by, which must be one of the relation's.
    Ordered,
    /// A component of a value of a type of the program's own, which must
    /// be one of its possible representation's.
    Component,
}

impl Noun {
    /// What a `replace` or an `update` of a value of type `ty` gives new
    /// values: the components of a type of the program's own, or else
    /// attributes.
    pub(crate) fn assigned(ty: &hir::Type) -> Noun {
        match ty {
            hir::Type::Defined(_) => Noun::Component,
            _ => Noun::Attribute,
        }
    }

    fn one(self) -> &'static str {
        match self {
            Noun::Attribute | Noun::Projected | Noun::Key | Noun::Ordered => "attribute",
            Noun::Argument => "argument",
            Noun::Component => "component",
        }
    }

    fn many(self) -> &'static str {
        match self {
            Noun::Attribute | Noun::Projected | Noun::Key | Noun::Ordered => "attributes",
            Noun::Argument => "arguments",
            Noun::Component => "components",
        }
    }
}

/// An attribute or argument named `name`, at `span`, appearing more than
/// once, first at `first`.
pub(crate) fn duplicate_attribute(noun: Noun, name: &str, span: Span, first: Span) -> Diagnostic {
    let set = match noun {
        Noun::Attribute => "a tuple's attributes are a set, so each name can appear only once",
        Noun::Projected | Noun::Key | Noun::Ordered => "each attribute is named once",
        Noun::Argument => "arguments are a tuple of the parameters, so each one is given once",
        Noun::Component => {
            "a possible representation's components are a set, so each name can appear only once"
        }
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
/// attributes or arguments. `context` is a note on what was expected.
///
/// There is no fix: only the program knows what values they should have,
/// and any the compiler chose would make a program that compiles and
/// computes with a value nobody gave.
pub(crate) fn missing_attributes(
    noun: Noun,
    missing: &[&str],
    context: &str,
    at: Span,
) -> Diagnostic {
    let names: Vec<_> = missing.iter().map(|name| format!("`{name}`")).collect();
    let (what, values) = if missing.len() == 1 {
        (noun.one(), "the value it should have")
    } else {
        (noun.many(), "the values they should have")
    };
    Diagnostic::error(
        Code::MissingAttribute,
        format!("missing {what} {}", trendy_diag::conjoin(&names)),
        at,
        format!("no {}", trendy_diag::conjoin(&names)),
    )
    .with_note(context.to_string())
    .with_note(match noun {
        Noun::Attribute | Noun::Projected | Noun::Key | Noun::Ordered => {
            "a tuple value must have every attribute of its type"
        }
        Noun::Argument => "every parameter needs an argument",
        Noun::Component => {
            "a value is selected with every component of its possible representation"
        }
    })
    .with_help(format!(
        "add {} with {values}",
        trendy_diag::conjoin(&names)
    ))
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
        Noun::Key | Noun::Ordered => "not an attribute of the relation",
        Noun::Argument => "not a parameter of this operator",
        Noun::Component => "not a component of the value's type",
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
    .with_note("a kind says what its types must have, `<`, or `+` and `*`, operators only the compiler gives, so only the prelude declares one")
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
    let close = closest(name, words);
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
            let close = closest(
                target,
                params.iter().map(String::as_str).filter(|p| *p != name),
            );
            match close {
                Some(close) => d.with_suggestion(
                    format!("use `{close}`"),
                    vec![Edit {
                        span,
                        replacement: close.to_string(),
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
    origin.explain(
        Diagnostic::error(Code::TypeMismatch, message, arg.span(), format!("this is `{actual}`"))
            .with_note(format!(
                "`{name}` takes a relation of any heading, whose tuples the `per` arguments are evaluated for"
            )),
    )
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
/// looked for ("value" or "variable"), and `candidates` the names in
/// scope.
pub(crate) fn unknown_name(name: &str, span: Span, kind: &str, candidates: &[&str]) -> Diagnostic {
    let d = Diagnostic::error(
        Code::UnknownName,
        format!("unknown {kind} `{name}`"),
        span,
        "not defined",
    );
    spelled_like(
        d,
        name,
        span,
        candidates,
        "declare a variable with this name, or use a parameter",
    )
}

/// A value `name`, at `span`, that refers to nothing, in the condition of
/// a load into a relation without `numbered`: a misspelling of one of
/// `candidates`, the names in scope, if it may be one, and otherwise each
/// element's position, which only `numbered` names.
pub(crate) fn unnumbered_position(name: &str, span: Span, candidates: &[&str]) -> Diagnostic {
    if closest(name, candidates.iter().copied()).is_some() {
        return unknown_name(name, span, "value", candidates);
    }
    Diagnostic::error(
        Code::UnknownName,
        format!("unknown value `{name}`"),
        span,
        "not defined",
    )
    .with_note(format!(
        "a load's `where` sees the attributes of the tuples it makes, and an element's position only as the attribute `numbered {name}` would add, which the relation then has as well"
    ))
    .with_help(format!(
        "for each element's position, write `numbered {name}` before `where`"
    ))
}

/// The value `name`, which refers to nothing, written at `span` as another
/// language writes a constant, such as `PI` or `Math.PI`: the read-only
/// operator `operator`, which has no parameters, and whose call is the
/// value.
pub(crate) fn constant_as_value(name: &str, span: Span, operator: &str) -> Diagnostic {
    let replacement = format!("{operator}{{}}");
    Diagnostic::error(
        Code::UnknownName,
        format!("unknown value `{name}`"),
        span,
        "a constant, as another language writes it",
    )
    .with_note(format!(
        "a constant is a read-only operator without parameters, whose value is its call's, written with braces after its name, as `{replacement}`, and names are case-sensitive"
    ))
    .with_suggestion(
        format!("call `{operator}`"),
        vec![Edit { span, replacement }],
    )
}

/// The value `ARRAY`, or another spelling of `array`, at `span`, which
/// refers to nothing, indexed: an array of one element, as SQL writes it,
/// `ARRAY[1]`.
pub(crate) fn miscased_array(name: &str, span: Span) -> Diagnostic {
    Diagnostic::error(
        Code::UnknownName,
        format!("unknown value `{name}`"),
        span,
        "not defined",
    )
    .with_note("keywords are case-sensitive, and every keyword is lowercase: an array value is written `array [element, …]`")
    .with_suggestion(
        "for an array, write `array`",
        vec![Edit {
            span,
            replacement: "array ".to_string(),
        }],
    )
}

/// The value `name`, Python's `all` or `any`, at `span`, which refers to
/// nothing, projected, as a list of booleans is written: `any{a, b}`,
/// which trendy writes `or{a, b}`.
pub(crate) fn listed_booleans(name: &str, span: Span) -> Diagnostic {
    let keyword = if name == "all" { "and" } else { "or" };
    Diagnostic::error(
        Code::UnknownName,
        format!("unknown value `{name}`"),
        span,
        "not defined",
    )
    .with_note("`and{a, b, c}` is true when every operand is, and `or{a, b, c}` when one is")
    .with_suggestion(
        format!("for a list of booleans, write `{keyword}`"),
        vec![Edit {
            span,
            replacement: keyword.to_string(),
        }],
    )
}

/// An operator `name`, at `span`, that refers to nothing, called with
/// `written` arguments, if known, of which one is an array if `of_array`
/// says so. `operators` are those in scope, each with its parameters'
/// names.
pub(crate) fn unknown_operator(
    name: &str,
    span: Span,
    operators: &[(&str, Vec<&str>)],
    written: Option<Written>,
    of_array: bool,
) -> Diagnostic {
    let d = Diagnostic::error(
        Code::UnknownName,
        format!("unknown operator `{name}`"),
        span,
        "not defined",
    );
    // An operator the prelude hasn't, as SQL's `avg`, which is no
    // misspelling of `min`, nor `log` of `cos`.
    let lowercase = name.to_lowercase();
    if let Some(&(_, note, help)) = ABSENT
        .iter()
        .find(|(names, ..)| names.contains(&&*lowercase))
    {
        return d.with_note(note).with_help(help);
    }
    // Other languages' names for the prelude's operators, such as Python's
    // `prod` and `len`, which no closeness of spelling finds, or finds
    // wrong: `len` is close to `min`.
    let ours = elsewhere(name).map(|(relation, array)| match array {
        Some(array) if of_array => array,
        _ => relation,
    });
    let Some((ours, params)) =
        ours.and_then(|ours| operators.iter().find(|(name, _)| *name == ours))
    else {
        let names: Vec<&str> = operators.iter().map(|(name, _)| *name).collect();
        return spelled_like(
            d,
            name,
            span,
            &names,
            "define the operator, or call one that exists, such as `debug_value`",
        );
    };
    let mut edits = vec![Edit {
        span,
        replacement: (*ours).to_string(),
    }];
    // `len{r: a}` of an array, or Python's `len(a)`: `length`'s one
    // parameter is `a`.
    let named = match (written, params.as_slice()) {
        (Some(written), [param]) => named_argument(written, param),
        _ => None,
    };
    let named = match named {
        Some((edit, param)) => {
            edits.push(edit);
            format!(", whose parameter is `{param}`")
        }
        None => String::new(),
    };
    d.with_suggestion(format!("use `{ours}`, as trendy calls it{named}"), edits)
}

/// Operators other languages have and the prelude hasn't yet, by their
/// names there, lowercase, which closeness of spelling would find wrong,
/// as it finds `min` for `avg` and `cos` for `log`: each with what the
/// prelude has instead, and how to write the value with it.
const ABSENT: &[(&[&str], &str, &str)] = &[
    (
        &["avg", "average", "mean"],
        "a mean is the sum divided by the count, which the prelude's `sum` and `count` give; it has no mean of its own yet, as the mean of integers isn't an integer",
        "write `sum{r: r, of: x} / to_approximate{from: count{r: r}}`, and for integers, `to_approximate{from: sum{r: r, of: x}}` as the dividend",
    ),
    (
        &["tan"],
        MATHEMATICAL,
        "the tangent of `x` is `sin{of: x} / cos{of: x}`",
    ),
    (
        &["atan", "arctan"],
        MATHEMATICAL,
        "the arctangent of `x` is `atan2{y: x, x: 1e0}`",
    ),
    (
        &["hypot"],
        MATHEMATICAL,
        "the hypotenuse of `x` and `y` is `sqrt{of: x * x + y * y}`",
    ),
    (
        &["abs", "fabs"],
        MATHEMATICAL,
        "the absolute value of `x` is `if x < 0e0 then -x else x`, with `0` for an integer",
    ),
    (
        &["pow", "power", "sqr"],
        MATHEMATICAL,
        "a power with a whole exponent is a product, as `x * x` is the square of `x`, and `sqrt{of: x}` is its square root",
    ),
    (
        &[
            "asin", "acos", "arcsin", "arccos", "sinh", "cosh", "tanh", "exp", "log", "ln", "log2",
            "log10", "cbrt", "floor", "ceil", "ceiling", "round", "trunc",
        ],
        MATHEMATICAL,
        "write it as an operator of the program's own, with those",
    ),
];

/// What the prelude has of mathematics.
const MATHEMATICAL: &str = "the prelude's mathematical operators are `+`, `-`, `*`, `/`, `div`, `mod`, `sqrt`, `sin`, `cos`, `atan2` and `pi`, and it has no others yet";

/// The edit naming the one argument of a call, written `written`, `param`,
/// the one parameter of the operator meant, if it has another name or
/// none: a positional argument, or a name written as a projection's
/// attribute, as in `len{a}`. Also `param`, for the fix's message.
fn named_argument<'p>(written: Written, param: &'p str) -> Option<(Edit, &'p str)> {
    let rename = |span| Edit {
        span,
        replacement: param.to_string(),
    };
    let name = |span: Span| Edit {
        span: Span::empty(span.start),
        replacement: format!("{param}: "),
    };
    let edit = match written {
        Written::Arguments(args) => {
            let named: Vec<_> = args.named().collect();
            let positional: Vec<_> = args.positional().collect();
            match (named.as_slice(), positional.as_slice()) {
                ([only], []) => {
                    let written = only.name()?;
                    (written.ident()?.text() != param).then(|| rename(written.span()))?
                }
                ([], [only]) => name(only.span()),
                _ => return None,
            }
        }
        Written::Nothing(list) => {
            let mut names = list.names();
            let only = names.next().filter(|_| names.next().is_none())?;
            name(only.span())
        }
    };
    Some((edit, param))
}

/// `d`, about the name `name`, at `span`, with the fix to the closest of
/// `candidates` in spelling, or else the help `otherwise`.
fn spelled_like(
    d: Diagnostic,
    name: &str,
    span: Span,
    candidates: &[&str],
    otherwise: &str,
) -> Diagnostic {
    match closest(name, candidates.iter().copied()) {
        Some(closest) => d.with_suggestion(
            format!("did you mean `{closest}`?"),
            vec![Edit {
                span,
                replacement: closest.to_string(),
            }],
        ),
        None => d.with_help(otherwise),
    }
}

/// Other languages' names for operators of the prelude, lowercase, with
/// the prelude's, of a relation if it takes one, and of an array if that is
/// another: Python's `len` counts either, as SQL's `CARDINALITY` and Java's
/// `size` do; R's and Kotlin's `readLines` read a file's lines, as
/// Python's and Ruby's `readlines` and .NET's `ReadAllLines` do, and
/// pandas' and R's `read_csv` and Julia's `readcsv` a CSV file's records;
/// and
/// JavaScript's `parseInt` and `parseFloat`, Java's `parseDouble`, and C's
/// `atoi` and `atof`, as Go's `Atoi`, read numbers, and Java's
/// `parseBoolean` and Go's `ParseBool` booleans; and `sine`, `cosine` and
/// NumPy's `arctan2` are `sin`, `cos` and `atan2`.
const ELSEWHERE: &[(&str, &str, Option<&str>)] = &[
    ("len", "count", Some("length")),
    ("size", "count", Some("length")),
    ("cardinality", "count", Some("length")),
    ("prod", "product", None),
    ("readlines", "load_text_file", None),
    ("readalllines", "load_text_file", None),
    ("read_csv", "load_csv", None),
    ("readcsv", "load_csv", None),
    ("parseint", "parse_integer", None),
    ("atoi", "parse_integer", None),
    ("parsefloat", "parse_approximate", None),
    ("parsedouble", "parse_approximate", None),
    ("atof", "parse_approximate", None),
    ("parseboolean", "parse_boolean", None),
    ("parsebool", "parse_boolean", None),
    ("sine", "sin", None),
    ("cosine", "cos", None),
    ("arctan2", "atan2", None),
];

/// The prelude's operators that other languages call `name`, in any case:
/// of a relation, and of an array if that is another, if there are any.
pub(crate) fn elsewhere(name: &str) -> Option<(&'static str, Option<&'static str>)> {
    let lowercase = name.to_lowercase();
    ELSEWHERE
        .iter()
        .find(|(other, ..)| *other == lowercase)
        .map(|&(_, relation, array)| (relation, array))
}

/// Other languages' names for parameters of the prelude's operators, with
/// the operator and the prelude's name: pandas' `read_csv` chooses the
/// columns to read with `usecols`, or names a file's columns with
/// `names`, as `load_csv`'s `columns` does both, and Python's
/// `has_header` says what its `header` does.
const PARAMETERS_ELSEWHERE: &[(hir::Builtin, &str, &str)] = &[
    (LOAD_CSV, "usecols", "columns"),
    (LOAD_CSV, "names", "columns"),
    (LOAD_CSV, "has_header", "header"),
];

const LOAD_CSV: hir::Builtin = hir::Builtin::Update(hir::UpdateBuiltin::LoadCsv);

/// The parameter of the built-in `builtin` that other languages call
/// `name`, if there is one.
pub(crate) fn parameter_elsewhere(builtin: hir::Builtin, name: &str) -> Option<&'static str> {
    PARAMETERS_ELSEWHERE
        .iter()
        .find(|&&(b, other, _)| b == builtin && other == name)
        .map(|&(.., ours)| ours)
}

/// `d`, about the argument written `arg`, of type `actual`, of a call of
/// the built-in `builtin`, with the fix calling the one for its kind of
/// value instead: `count` counts a relation's tuples, and `length` an
/// array's elements.
pub(crate) fn counted_otherwise(
    d: Diagnostic,
    builtin: Option<hir::ReadOnlyBuiltin>,
    actual: &hir::Type,
    arg: &Expr,
) -> Diagnostic {
    let ours = match (builtin, actual) {
        (Some(hir::ReadOnlyBuiltin::Count), hir::Type::Array(_)) => hir::ReadOnlyBuiltin::Length,
        (Some(hir::ReadOnlyBuiltin::Length), hir::Type::Relation(_)) => hir::ReadOnlyBuiltin::Count,
        _ => return d,
    };
    let call = arg
        .syntax()
        .parent()
        .and_then(|p| p.ancestors().find_map(trendy_syntax::ast::CallExpr::cast));
    let params = hir::Builtin::ReadOnly(ours).params();
    let (Some(call), Some(param)) = (call, params.into_keys().next()) else {
        return d;
    };
    let name = ours.name();
    let replacement = format!("{name}{{{param}: {}}}", arg.syntax().text());
    d.with_suggestion(
        format!("count it with `{name}`: `{replacement}`"),
        vec![Edit {
            span: call.span(),
            replacement,
        }],
    )
}

/// The operator `name`, at `span`, used as a value, of `operators`. A
/// read-only operator's call is a value, as `pi{}` is, so it is called,
/// and an update operator is invoked by a `call` statement.
pub(crate) fn operator_as_value(name: &str, span: Span, operators: &Operators) -> Diagnostic {
    let d = Diagnostic::error(
        Code::OperatorAsValue,
        format!("`{name}` is an operator, not a value"),
        span,
        "operators can't be used as values",
    );
    let mut read_only = operators
        .overloads(name)
        .filter(|signature| signature.read_only);
    let Some(first) = read_only.next() else {
        return d.with_help(format!(
            "to invoke it, write a statement: `call {name}{{…}};`"
        ));
    };
    let d = d.with_note(
        "a read-only operator's value is its call's, written with its arguments in braces after its name, even when it has none",
    );
    if first.written.is_empty() || read_only.any(|signature| signature.written.is_empty()) {
        let replacement = format!("{name}{{}}");
        return d.with_suggestion(
            format!("call it: `{replacement}`"),
            vec![Edit { span, replacement }],
        );
    }
    d.with_help(format!(
        "call it with its arguments: the parameters are {}",
        first.text()
    ))
}

/// A call in an expression, of the operator `name`, at `span`, defined at
/// `defined` unless it is built in, which has no value. `empty` says
/// whether the call is `name{}`, without arguments, and `projectable`
/// whether a value of the same name is in scope then, which `(name){}`
/// would project. `out` is what to write instead, if the call is a
/// variable's value, which the operator's `out` parameter can give it.
pub(crate) fn operator_call_without_value(
    name: &str,
    span: Span,
    defined: Option<Span>,
    empty: bool,
    projectable: bool,
    out: Option<OutResult>,
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
    match out {
        Some(OutResult::Fix(help, edit)) => {
            return d
                .with_note("an update operator gives a result through an `out` parameter, which a call names the variable for")
                .with_suggestion(help, vec![edit]);
        }
        Some(OutResult::Help(help)) => {
            return d
                .with_note("an update operator gives a result through an `out` parameter, which a call names the variable for")
                .with_help(help);
        }
        None => {}
    }
    if !projectable {
        return d.with_help(format!(
            "to invoke it, write a statement: `call {name}{{{args}}};`"
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

/// What to write for a call of an update operator with one `out`
/// parameter that is a variable's value, as other languages return one.
pub(crate) enum OutResult {
    /// A `call` statement in the variable's statement's place, with the
    /// variable its `out` argument: the help, which writes it out, and the
    /// edit.
    Fix(String, Edit),
    /// The help, for an initial value, which is computed when compiling.
    Help(String),
}

/// What to write for `expr`, a call, written with the arguments `written`,
/// of the update operator `signature`, which has one `out` parameter, as
/// the value of an assignment statement or a variable's declaration: in an
/// update operator's body, a `call` statement, with the variable its `out`
/// argument, after a declaration of it without a value; and for a variable
/// of the program, which starts with its value when compiling, the same in
/// an operator that runs, such as `main`.
pub(crate) fn out_result(
    signature: &Signature,
    written: Written,
    expr: &Expr,
) -> Option<OutResult> {
    let mut outs = signature
        .written
        .iter()
        .filter_map(|(name, param)| match param {
            crate::operators::Param::Variable(ty, hir::Mode::Out) => Some((name, ty)),
            _ => None,
        });
    let (param, ty) = outs.next()?;
    if outs.next().is_some() {
        return None;
    }
    // The call with the variable as its `out` argument.
    let call = |variable: &str| {
        let (at, before, after) = match written {
            Written::Arguments(args) => args.insertion(),
            Written::Nothing(list) => list.insertion(),
        };
        let start = expr.span().start;
        let mut text = expr.syntax().text().to_string();
        text.insert_str(
            at - start,
            &format!("{before}out {param}: {variable}{after}"),
        );
        format!("call {text}")
    };
    let parent = expr.syntax().parent()?;
    if let Some(trendy_syntax::ast::Clause::Assignment(assignment)) =
        trendy_syntax::ast::Clause::cast(parent.clone())
    {
        // `x := f{…};`, alone in its statement, in an update operator.
        let statement = trendy_syntax::ast::AssignStmt::cast(assignment.syntax().parent()?)?;
        if statement.clauses().count() != 1 || statement.block().is_some() || !callable(&parent) {
            return None;
        }
        let clause = trendy_syntax::ast::Clause::Assignment(assignment);
        let variable = clause.target()?.syntax().text().to_string();
        let replacement = call(&variable);
        return Some(OutResult::Fix(
            format!("call it, with `{variable}` as its `out` argument: `{replacement};`"),
            Edit {
                span: clause.span(),
                replacement,
            },
        ));
    }
    // `var x := f{…};`, declared with its type and no value instead.
    let declaration = VarDecl::cast(parent.parent()?)?;
    let variable = declaration.name()?.ident()?.text().to_string();
    let declared = match declaration.type_expr() {
        Some(written) => written.syntax().text().to_string(),
        None if ty.parameters().next().is_none() => ty.to_string(),
        None => return None,
    };
    let declare = format!("var {variable}: {declared};");
    let statement = format!("{};", call(&variable));
    if declaration.syntax().parent().map(|p| p.kind()) == Some(SyntaxKind::SourceFile) {
        return Some(OutResult::Help(format!(
            "a variable's initial value is computed when compiling, which this call can't be: declare it without one, `{declare}`, and call `{}` in an operator that runs, such as `main`: `{statement}`",
            signature.name
        )));
    }
    if !callable(&parent) {
        return None;
    }
    let indent = indentation(declaration.syntax());
    Some(OutResult::Fix(
        format!(
            "declare `{variable}` without a value, then call it, with `{variable}` as its `out` argument: `{declare} {statement}`"
        ),
        Edit {
            span: Span::new(declaration.span().start, expr.span().end),
            replacement: format!("{declare}\n{indent}{}", call(&variable)),
        },
    ))
}

/// Whether a `call` statement can go where `node` is: in an update
/// operator's body, not a value block's or a read-only operator's.
fn callable(node: &trendy_syntax::SyntaxNode) -> bool {
    node.ancestors()
        .take_while(|a| a.kind() != SyntaxKind::ValueBlock)
        .find_map(trendy_syntax::ast::Operator::cast)
        .is_some_and(|op| op.result().is_none())
}

/// The spaces and tabs before `node` on its line.
fn indentation(node: &trendy_syntax::SyntaxNode) -> String {
    let Some(before) = node.first_token().and_then(|t| t.prev_token()) else {
        return String::new();
    };
    if !before.kind().is_trivia() {
        return String::new();
    }
    let text = before.text();
    text.rfind('\n')
        .map_or_else(String::new, |i| text[i + 1..].to_string())
}

/// The rule that a read-only operator is a function, for notes.
const READ_ONLY_FUNCTION: &str = "a read-only operator is a function of its arguments: its calls with the same arguments have the same value, whenever they are made (TTM, RM Prescription 3), so it reads only its parameters and its own local variables, and a variable of the program's it needs is passed to it, as a `const` parameter";

/// The variable of the program's `name`, of type `ty` if it is known, read
/// at `span` in the read-only operator `operator`, one of `operators`: the
/// fix passes it as a parameter of that name, which each call of the
/// operator in the file gives the variable, but not a call of another of
/// its name.
pub(crate) fn variable_in_read_only(
    name: &str,
    span: Span,
    operator: &Signature,
    operators: &Operators,
    ty: Option<String>,
) -> Diagnostic {
    if let Some((defined, _)) = operator.constrains() {
        return Diagnostic::error(
            Code::VariableInReadOnly,
            format!(
                "`{}`'s constraint can't read the variable `{name}`",
                defined.name
            ),
            span,
            "a variable of the program's",
        )
        .with_note(CONSTRAINT_OF_COMPONENTS)
        .with_help(format!(
            "write the condition with the components and constants, without `{name}`"
        ));
    }
    // A possible representation's `to` or `from`.
    if let Some(from) = &operator.from_type {
        return Diagnostic::error(
            Code::VariableInReadOnly,
            format!(
                "a conversion of `{}` can't read the variable `{name}`",
                from.defined.name
            ),
            span,
            "a variable of the program's",
        )
        .with_note("a conversion computes a representation's components from another's alone, so that a value is the same whichever it is written in")
        .with_help(format!(
            "write it with the components and constants, without `{name}`"
        ));
    }
    let op = &operator.name;
    let d = Diagnostic::error(
        Code::VariableInReadOnly,
        format!("the read-only operator `{op}` can't read the variable `{name}`"),
        span,
        "a variable of the program's",
    )
    .with_note(READ_ONLY_FUNCTION);
    let help = format!(
        "pass `{name}` to `{op}` as a parameter, `{name}: …`, and give it `{name}: {name}` in each call"
    );
    let (Some(decl), Some(ty)) = (&operator.decl, ty) else {
        return d.with_help(help);
    };
    let Some(heading) = decl.params() else {
        return d.with_help(help);
    };
    let (at, before, after) = heading.insertion();
    let mut edits = vec![Edit {
        span: Span::new(at, at),
        replacement: format!("{before}{name}: {ty}{after}"),
    }];
    // Each call, `f{…}`, or `f{}`, which the parser takes for a projection,
    // whose arguments' names choose this operator.
    let file = decl.syntax().ancestors().last().expect("a node has a root");
    let calls_it = |name_ref: Option<trendy_syntax::ast::NameRef>, written: Written| {
        name_ref.and_then(|n| n.ident()).is_some_and(|i| {
            i.text() == op
                && matches!(
                    operators.chosen(op, &crate::check::argument_names(written)),
                    Some(Chosen::One(chosen)) if chosen.id == operator.id
                )
        })
    };
    for node in file.descendants() {
        let insertion = if let Some(call) = trendy_syntax::ast::CallExpr::cast(node.clone()) {
            call.arguments()
                .filter(|args| calls_it(call.callee(), Written::Arguments(args)))
                .map(|args| args.insertion())
        } else if let Some(projection) = ProjectExpr::cast(node) {
            let empty = projection.names().filter(|l| l.names().next().is_none());
            let callee = match projection.operand() {
                Some(Expr::Name(name_ref)) => Some(name_ref),
                _ => None,
            };
            empty
                .filter(|list| calls_it(callee, Written::Nothing(list)))
                .map(|list| list.insertion())
        } else {
            None
        };
        if let Some((at, before, after)) = insertion {
            edits.push(Edit {
                span: Span::new(at, at),
                replacement: format!("{before}{name}: {name}{after}"),
            });
        }
    }
    d.with_suggestion(
        format!("pass `{name}` to `{op}`, as a `const` parameter, which each call gives `{name}`"),
        edits,
    )
}

/// The parameter `name`, at `span`, assigned. `hidden` says whether it
/// hides a variable of the same name.
pub(crate) fn assigned_parameter(name: &str, span: Span, hidden: bool) -> Diagnostic {
    let d = Diagnostic::error(
        Code::NotAssignable,
        format!("the `const` parameter `{name}` can't be assigned"),
        span,
        "a `const` parameter",
    )
    .with_note(MODE_RULE);
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

/// An assignment whose target, at `target`, is the attribute `name` of a
/// tuple, or the component `name` of a value of the type of the program's
/// own `of`, as other languages assign an object's field, `t.name := x`.
/// `fix`, if there is one, is the assignment's span and its rewriting, by
/// `update` of a variable, if the third is true, or else as an assignment
/// of an array's element with the attribute replaced.
pub(crate) fn attribute_assigned(
    target: Span,
    name: &str,
    of: Option<&hir::Defined>,
    fix: Option<(Span, String, bool)>,
) -> Diagnostic {
    let d = match of {
        None => Diagnostic::error(
            Code::AttributeAssigned,
            format!("attribute `{name}` can't be assigned on its own"),
            target,
            "an attribute of a tuple",
        )
        .with_note("a tuple's attributes are changed by `update t { a := x }`, shorthand for `t := t replace { a := x }`; as in Tutorial D, an attribute is no variable to assign"),
        Some(defined) => {
            let t = &defined.name;
            Diagnostic::error(
                Code::AttributeAssigned,
                format!("component `{name}` can't be assigned on its own"),
                target,
                format!("a component of a `{t}`"),
            )
                .with_note("the components of a variable's value are changed by `update v { c := x }`, shorthand for `v := v replace { c := x }`; a component is no variable to assign")
        }
    };
    let (noun, holder) = match of {
        Some(_) => ("component", "variable"),
        None => ("attribute", "tuple"),
    };
    match fix {
        Some((span, replacement, true)) => d.with_suggestion(
            format!("change the {noun} with `update`"),
            vec![Edit { span, replacement }],
        ),
        Some((span, replacement, false)) => d.with_suggestion(
            format!("assign the element with the {noun} changed"),
            vec![Edit { span, replacement }],
        ),
        None => d.with_help(format!(
            "give the {holder} a new value with the {noun} replaced, by `update` or `replace`"
        )),
    }
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

/// An `update`, at `span`, in `statement`, that gives each attribute, or
/// component, as `noun` says, its own value.
pub(crate) fn own_values_updated(
    noun: Noun,
    span: Span,
    statement: &trendy_syntax::ast::AssignStmt,
) -> Diagnostic {
    let d = Diagnostic::error(
        Code::NoChange,
        format!("this `update` gives each {} its own value", noun.one()),
        span,
        "changes nothing",
    )
    .with_note(OWN_VALUE_RULE);
    deleted(d, statement)
}

/// `postfix`, a `replace` of `operand`, that gives each attribute, or
/// component, as `noun` says, its own value.
pub(crate) fn own_values_replaced(
    noun: Noun,
    postfix: &trendy_syntax::ast::PostfixExpr,
    operand: &Expr,
) -> Diagnostic {
    let (span, edit) = dropped(postfix, operand);
    Diagnostic::error(
        Code::NoChange,
        format!("this `replace` gives each {} its own value", noun.one()),
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
const OWN_VALUE_RULE: &str = "a variable, an attribute, a component or an element that isn't given a new value keeps its own, so giving it its own value changes nothing";

/// An `update` with an empty `list`, in `statement`, which would change
/// nothing: of attributes or components, as `noun` says.
pub(crate) fn empty_update(
    noun: Noun,
    list: &trendy_syntax::ast::AssignList,
    statement: &trendy_syntax::ast::AssignStmt,
) -> Diagnostic {
    let many = noun.many();
    let d = Diagnostic::error(
        Code::EmptyDoesNothing,
        "this `update` changes nothing",
        list.span(),
        format!("no {many}"),
    )
    .with_note(format!(
        "`update` takes {many} the variable has, each `name := value`; with none it would change nothing"
    ));
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
const CHANGES_RULE: &str = "the changes in an `assign` block are made at once, each computed from the variables as they were before it: changes to a variable's tuples, attributes, components or elements combine, unless two remove the same tuple, set the same attribute or component, or set the same element or one in it, and `v := …` can't combine with any other change to `v`";

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
/// type: the fix removes it, and the value its body ends with.
pub(crate) fn main_with_result(decl: &trendy_syntax::ast::Operator) -> Diagnostic {
    let value = decl.body().and_then(|body| body.value_removal());
    let edits: Vec<Edit> = decl
        .result_removal()
        .into_iter()
        .chain(value)
        .map(|span| Edit {
            span,
            replacement: String::new(),
        })
        .collect();
    let span = decl.result().map_or_else(|| decl.span(), |t| t.span());
    Diagnostic::error(
        Code::MainSignature,
        "`main` can't have a result type",
        span,
        "nothing receives `main`'s value",
    )
    .with_note("execution starts in `main`, an update operator, which returns no value")
    .with_suggestion("remove the result type, and the value", edits)
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
/// applied to a value of type `ty`, which has no attributes, or, for a
/// `replace` or an `update`, no components either.
pub(crate) fn not_projectable(
    operator: Option<&str>,
    at: Span,
    ty: &hir::Type,
    operand: &Expr,
) -> Diagnostic {
    let (message, label) = match operator {
        Some(operator @ ("replace" | "update")) => (
            format!("`{operator}` can't be applied to {}", plural(ty)),
            format!("`{operator}` takes a tuple, a relation or a value with components"),
        ),
        Some(operator) => (
            format!("`{operator}` can't be applied to {}", plural(ty)),
            format!("`{operator}` takes a tuple or relation"),
        ),
        None => (
            format!("{} can't be projected", plural(ty)),
            "a projection takes a tuple or relation".to_string(),
        ),
    };
    let d = Diagnostic::error(Code::WrongOperandType, message, at, label)
        .with_parameter_note(ty)
        .with_secondary(operand.span(), format!("this is `{ty}`"))
        .with_note(
            "a projection, `r{a, b}`, `r remove {c}`, `r rename {a as b}`, `r extend { d := … }`, `r replace { a := … }`, `r wrap { w := {a} }` and `r unwrap {w}` work on the attributes of a tuple or relation, and `v replace { c := … }` on the components of a value of a type of the program's own",
        );
    match ty {
        hir::Type::Defined(defined) => d.with_note(format!(
            "`{}` is a scalar type, whose values have the components it declares: `v.c` reads one and `replace` gives them new values, but none can be added, removed or renamed",
            defined.name
        )),
        _ => d,
    }
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
    .with_note("there is a tuple at hand only in the values of an `extend` or a `replace`, a `where`'s condition, and an argument for a `per` parameter")
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
        .filter_map(|(a, b)| Some((misspelling(a, b)?, a, b)))
        .min()
        .map(|(_, a, b)| (a, b));
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
        "only a tuple has an attribute's value, and a type's value a component's",
    )
    .with_parameter_note(ty)
    .with_secondary(operand.span(), format!("this is `{ty}`"));
    match ty {
        hir::Type::Relation(_) => d
            .with_note("a relation can have many tuples, so no one value for an attribute")
            .with_help(format!(
                "to keep only the attribute, project on it: `r{{{name}}}`"
            )),
        _ => d.with_note("`t.name` is the value of the attribute `name` of the tuple `t`, or of the component `name` of `t`, a value of a type of the program's own"),
    }
}

/// The error for `if_expr`, of `condition` and `branches`, if it decides
/// nothing: its condition is written `true` or `false`, so it is the
/// branch that takes, or its branches are `true` and `false`, so it is its
/// condition, or the condition negated.
pub(crate) fn pointless_if(
    if_expr: &trendy_syntax::ast::IfExpr,
    condition: &Expr,
    branches: &[Expr; 2],
) -> Option<Diagnostic> {
    let boolean = |e: &Expr| match e {
        Expr::Literal(literal) => match literal.value() {
            Ok(trendy_syntax::ast::LiteralValue::Boolean(b)) => Some(b),
            _ => None,
        },
        _ => None,
    };
    let text = |e: &Expr| e.syntax().text().to_string();
    let note = "an `if` chooses between its branches by its condition, so it decides something only if the condition can be either";
    if let Some(always) = boolean(condition) {
        let taken = &branches[usize::from(!always)];
        let which = if always { "then" } else { "else" };
        return Some(
            Diagnostic::error(
                Code::PointlessCondition,
                format!("the condition is always `{always}`"),
                condition.span(),
                format!("so the `{which}` branch is always taken"),
            )
            .with_note(note)
            .with_suggestion(
                format!("keep the `{which}` branch"),
                vec![Edit {
                    span: if_expr.span(),
                    replacement: text(taken),
                }],
            ),
        );
    }
    let negated = match (boolean(&branches[0]), boolean(&branches[1])) {
        (Some(true), Some(false)) => false,
        (Some(false), Some(true)) => true,
        _ => return None,
    };
    let at = Span::new(branches[0].span().start, branches[1].span().end);
    let (message, label, replacement) = if negated {
        (
            "its condition negated",
            "the opposite of the condition's value",
            negation(condition),
        )
    } else {
        (
            "its condition",
            "the condition's own value",
            text(condition),
        )
    };
    Some(
        Diagnostic::error(
            Code::PointlessCondition,
            format!("this `if` is {message}"),
            at,
            label,
        )
        .with_note(note)
        .with_suggestion(
            format!("write `{replacement}`"),
            vec![Edit {
                span: if_expr.span(),
                replacement,
            }],
        ),
    )
}

/// `not` and `condition`, which needs parentheses unless it is a single
/// operand, as `not` binds tightest.
fn negation(condition: &Expr) -> String {
    let text = condition.syntax().text();
    match condition {
        Expr::Name(_)
        | Expr::Literal(_)
        | Expr::Paren(_)
        | Expr::Call(_)
        | Expr::Extract(_)
        | Expr::Index(_) => format!("not {text}"),
        _ => format!("not ({text})"),
    }
}

/// The error for the `if` statement `stmt`, if it decides nothing: its
/// condition is written `true` or `false`, so only one branch ever runs,
/// or its blocks are empty, so it runs nothing, or nothing either way.
pub(crate) fn pointless_if_statement(stmt: &trendy_syntax::ast::IfStmt) -> Option<Diagnostic> {
    use trendy_syntax::ast::ElseBranch;
    let condition = stmt.condition()?;
    let then = stmt.then_block()?;
    let otherwise = stmt.else_branch();
    // The statements of a block, as written, or nothing.
    let inside = |block: &trendy_syntax::ast::Body| {
        let statements: Vec<_> = block.statements().collect();
        match (statements.first(), statements.last()) {
            (Some(first), Some(last)) => {
                let text = stmt.syntax().text().to_string();
                let start = usize::from(stmt.syntax().text_range().start());
                text[first.span().start - start..last.span().end - start].to_string()
            }
            _ => String::new(),
        }
    };
    let empty = |block: &trendy_syntax::ast::Body| block.statements().next().is_none();
    let note = "an `if` statement chooses which of its blocks runs by its condition, so it does something only if the condition can be either, and each block runs something";
    let replace = |span: Span, replacement: String| vec![Edit { span, replacement }];
    if let Expr::Literal(literal) = &condition
        && let Ok(trendy_syntax::ast::LiteralValue::Boolean(always)) = literal.value()
    {
        let (runs, replacement) = match (always, &otherwise) {
            (true, _) => ("the `then` block always runs", inside(&then)),
            (false, Some(ElseBranch::Block(block))) => {
                ("the `else` block always runs", inside(block))
            }
            (false, Some(ElseBranch::If(chained))) => (
                "the `else if` always decides",
                format!("{};", chained.syntax().text()),
            ),
            (false, None) => ("nothing ever runs", String::new()),
        };
        let fix = if replacement.is_empty() {
            "remove the `if`"
        } else {
            "keep what always runs"
        };
        return Some(
            Diagnostic::error(
                Code::PointlessCondition,
                format!("the condition is always `{always}`"),
                condition.span(),
                format!("so {runs}"),
            )
            .with_note(note)
            .with_suggestion(fix, replace(stmt.span(), replacement)),
        );
    }
    let d = |message: &str, at: Span, label: &str| {
        Diagnostic::error(
            Code::PointlessCondition,
            message.to_string(),
            at,
            label.to_string(),
        )
        .with_note(note)
    };
    match (empty(&then), &otherwise) {
        (true, None) => Some(
            d("this `if` does nothing", stmt.span(), "its block is empty")
                .with_suggestion("remove it", replace(stmt.span(), String::new())),
        ),
        (true, Some(ElseBranch::Block(block))) if empty(block) => Some(
            d(
                "this `if` does nothing",
                stmt.span(),
                "its blocks are empty",
            )
            .with_suggestion("remove it", replace(stmt.span(), String::new())),
        ),
        // `if c then [] else [ … ];`: `if not c then [ … ];`.
        (true, Some(ElseBranch::Block(block))) => Some(
            d("the `then` block is empty", then.span(), "runs nothing").with_suggestion(
                "negate the condition, and run the `else` block's statements after `then`",
                vec![
                    Edit {
                        span: condition.span(),
                        replacement: negation(&condition),
                    },
                    Edit {
                        span: Span::new(then.span().start, block.span().start),
                        replacement: String::new(),
                    },
                ],
            ),
        ),
        (false, Some(ElseBranch::Block(block))) if empty(block) => Some(
            d("the `else` block is empty", block.span(), "runs nothing").with_suggestion(
                "remove it",
                replace(Span::new(then.span().end, block.span().end), String::new()),
            ),
        ),
        _ => None,
    }
}

/// What a loop does, for notes.
const LOOP: &str = "a loop runs its block again and again while its condition is true, so it ends only once its block makes the condition false: no loop leaves early";

/// A loop, at `span`, whose condition, `condition`, is checked as `check`
/// says, and whose block is `body`, if it does nothing or never ends: a
/// condition written `true`, which never ends; one written `false`, which
/// never runs a `while`'s block, and runs a `loop`'s once; or an empty
/// block, which does nothing, or never ends.
pub(crate) fn pointless_loop(
    span: Span,
    condition: &Expr,
    body: &trendy_syntax::ast::Body,
    check: hir::Check,
) -> Option<Diagnostic> {
    let d = |message: &str, at: Span, label: &str| {
        Diagnostic::error(
            Code::PointlessCondition,
            message.to_string(),
            at,
            label.to_string(),
        )
        .with_note(LOOP)
    };
    let written = match condition {
        Expr::Literal(literal) => match literal.value() {
            Ok(trendy_syntax::ast::LiteralValue::Boolean(always)) => Some(always),
            _ => None,
        },
        _ => None,
    };
    let replaced = |replacement: String| vec![Edit { span, replacement }];
    match (written, check) {
        (Some(true), _) => Some(
            d(
                "the condition is always `true`",
                condition.span(),
                "so the loop never ends",
            )
            .with_help("write a condition that its block makes false, as in `while n > 0 loop [ n := n - 1; ];`"),
        ),
        (Some(false), hir::Check::Before) => Some(
            d(
                "the condition is always `false`",
                condition.span(),
                "so the block never runs",
            )
            .with_suggestion("remove the loop", replaced(String::new())),
        ),
        (Some(false), hir::Check::After) => {
            let statements: Vec<_> = body.statements().collect();
            let inside = match (statements.first(), statements.last()) {
                (Some(first), Some(last)) => {
                    let text = body.syntax().text().to_string();
                    let start = body.span().start;
                    text[first.span().start - start..last.span().end - start].to_string()
                }
                _ => String::new(),
            };
            Some(
                d(
                    "the condition is always `false`",
                    condition.span(),
                    "so the block runs once",
                )
                .with_suggestion("keep the block's statements", replaced(inside)),
            )
        }
        (None, _) if body.statements().next().is_none() => Some(
            d("this loop does nothing", body.span(), "its block is empty")
                .with_note("with nothing in its block to make its condition false, it either never runs or never ends")
                .with_help("give the block something to do that makes the condition false"),
        ),
        (None, _) => None,
    }
}

/// A loop's condition, `condition`, known when compiling to be true, from
/// the values it is computed from: the loop never ends.
pub(crate) fn endless_loop(condition: &Expr) -> Diagnostic {
    Diagnostic::error(
        Code::PointlessCondition,
        "the condition is always true",
        condition.span(),
        "known when compiling, so the loop never ends",
    )
    .with_note(LOOP)
    .with_help(
        "write a condition that its block makes false, as in `while n > 0 loop [ n := n - 1; ];`",
    )
}

/// `stmt`, a loop in the read-only operator `name`.
pub(crate) fn loop_in_read_only(name: &str, stmt: &trendy_syntax::ast::Statement) -> Diagnostic {
    Diagnostic::error(
        Code::Return,
        "a read-only operator has no loops",
        loop_start(stmt),
        format!("a loop in `{name}`"),
    )
    .with_note("a read-only operator's body declares and assigns its local variables, then ends with its value: it computes one value, and changes nothing, so a loop would have nothing to do again")
    .with_help("compute the value from the whole relation, as in `sum{r: r, of: x}` or `r extend { … }`, or make the operator an update operator, without a result type, invoked by `call`")
}

/// `stmt`, a loop in a value block.
pub(crate) fn loop_in_block(stmt: &trendy_syntax::ast::Statement) -> Diagnostic {
    Diagnostic::error(
        Code::Return,
        "a value block has no loops",
        loop_start(stmt),
        "a loop in a value block",
    )
    .with_note(BLOCK)
    .with_help(
        "compute the value from the whole relation, as in `sum{r: r, of: x}` or `r extend { … }`",
    )
}

/// Where to point at the loop `stmt`: its first keyword.
fn loop_start(stmt: &trendy_syntax::ast::Statement) -> Span {
    stmt.syntax()
        .first_token()
        .map_or_else(|| stmt.span(), |t| trendy_syntax::span(t.text_range()))
}

/// `index`, `a[i]`, whose operand `a`, written `operand`, has type `ty`,
/// not an array's: only an array's elements have positions. A tuple's
/// attribute named in quotes, as other languages index a dictionary, is
/// its value instead.
pub(crate) fn not_indexable(
    index: &trendy_syntax::ast::IndexExpr,
    ty: &hir::Type,
    operand: &Expr,
) -> Diagnostic {
    let at = index.brackets().unwrap_or_else(|| index.span());
    let d = Diagnostic::error(
        Code::WrongOperandType,
        format!("{} can't be indexed", plural(ty)),
        at,
        "only an array's elements have positions",
    )
    .with_parameter_note(ty)
    .with_secondary(operand.span(), format!("this is `{ty}`"));
    match ty {
        hir::Type::Relation(_) => d.with_note(
            "a relation's tuples have no order, so no positions (TTM RM Proscription 2): a tuple is found by its attribute values, as `tuple from (r where name = \"Amara\")` finds it",
        ),
        hir::Type::Tuple(heading) => {
            let d = d.with_note("`t.name` is the value of the attribute `name` of the tuple `t`");
            let name = index.index().and_then(|position| match position {
                Expr::Literal(literal) => match literal.value() {
                    Ok(trendy_syntax::ast::LiteralValue::Text(name)) => Some(name),
                    _ => None,
                },
                _ => None,
            });
            match name.filter(|name| heading.get(name).is_some()) {
                Some(name) => d.with_suggestion(
                    format!("take the attribute's value: `.{name}`"),
                    vec![Edit {
                        span: at,
                        replacement: format!(".{name}"),
                    }],
                ),
                None => d,
            }
        }
        hir::Type::Parameter(p) if p.any == hir::Any::Array => d.with_help(
            "to index its elements, declare the parameter's type with theirs, a type parameter of its own: `a: array E` with `types { E: any type }`",
        ),
        _ => d,
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
/// after `operand`, which changes nothing. A `replace` is of attributes or
/// components, as `noun` says.
pub(crate) fn empty_assign(
    assigning: crate::ops::Assigning,
    noun: Noun,
    postfix: &trendy_syntax::ast::PostfixExpr,
    operand: &Expr,
) -> Diagnostic {
    let many = noun.many();
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
            ("replaces nothing", &format!("no {many}")),
            &format!(
                "`replace` takes {many} the operand has, each `name := value`; with none it would change nothing"
            ),
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
    let rename = closest(name, heading.named().map(|(a, _)| a));
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

/// A component, `name`, at `span`, that a value of a type of the program's
/// own hasn't in the possible representation a change gives new values,
/// with the components it may be a misspelling of, `candidates`, and those
/// of each of the type's representations.
pub(crate) struct Unknown<'a> {
    pub(crate) name: &'a str,
    pub(crate) span: Span,
    pub(crate) candidates: &'a [String],
    pub(crate) representations: &'a [hir::Heading],
}

/// The component `unknown`, which a `replace` or an `update`, as
/// `assigning` says, of `operand`, a value of the type of the program's own
/// `defined`, gives a new value, but which the type hasn't, or not in the
/// possible representation the others are of.
pub(crate) fn replaced_unknown_component(
    assigning: crate::ops::Assigning,
    unknown: &Unknown,
    defined: &hir::Defined,
    operand: &Expr,
) -> Diagnostic {
    let &Unknown {
        name,
        span,
        candidates,
        representations,
    } = unknown;
    let rename = closest(name, candidates.iter().map(String::as_str));
    let has = if assigning == crate::ops::Assigning::Update {
        "the variable's value has"
    } else {
        "the operand has"
    };
    let components = match representations {
        [_, _, ..] => {
            let selectors: Vec<String> = representations
                .iter()
                .map(|components| selector_text(&defined.name, components))
                .collect();
            format!(
                "`{}`'s components are those of one of its possible representations, as its selectors write them: {}",
                defined.name,
                trendy_diag::disjoin(&selectors)
            )
        }
        _ => components_note(defined),
    };
    let context = format!(
        "`{}` gives components {has} new values of the same types, and {components}",
        assigning.keyword(),
    );
    unknown_attribute(Noun::Component, name, span, &context, rename, None)
        .with_secondary(operand.span(), format!("this is `{}`", defined.name))
}

/// The selector of the type `t` in the possible representation with the
/// components `components`, quoted, with `…` for each argument.
fn selector_text(t: &str, components: &hir::Heading) -> String {
    let written: Vec<String> = components.named().map(|(c, _)| format!("{c}: …")).collect();
    format!("`{t}{{{}}}`", written.join(", "))
}

/// Components of two possible representations of the type of the program's
/// own `defined`, each with where it is written and its representation's
/// components, that a `replace` or an `update` gives new values together.
pub(crate) fn mixed_representations(
    defined: &hir::Defined,
    first: (&str, Span, &hir::Heading),
    second: (&str, Span, &hir::Heading),
) -> Diagnostic {
    let t = &defined.name;
    let (one, one_span, ones) = first;
    let (other, other_span, others) = second;
    let listed = |components: &hir::Heading| selector_text(t, components);
    Diagnostic::error(
        Code::Conversions,
        format!("`{one}` and `{other}` are components of different possible representations of `{t}`"),
        other_span,
        format!("not a component of the representation `{one}` is of"),
    )
    .with_secondary(one_span, format!("a component of {}", listed(ones)))
    .with_note(format!(
        "a `replace` or an `update` gives the components of one representation new values, keeps the others it has, and selects the value in it; `{other}` is a component of {}",
        listed(others)
    ))
    .with_help("give one representation's components new values at a time, as in `v replace { … } replace { … }`")
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
        hir::Type::Defined(_) => d.with_help(
            "a value satisfies a condition or doesn't: the condition itself, with `v.c` for its components, is that boolean",
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

/// What the components of the type of the program's own `defined` are, for
/// a note.
pub(crate) fn components_note(defined: &hir::Defined) -> String {
    let components = named_attributes(&defined.components);
    if components.is_empty() {
        format!("`{}` has no components", defined.name)
    } else {
        format!(
            "`{}`'s components are {}",
            defined.name,
            trendy_diag::conjoin(&components)
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
        crate::ops::Failed::Overlap => overlapping_elements(expr),
        crate::ops::Failed::Unparsable { builtin, text, why } => {
            unparsable(*builtin, text, *why, expr)
        }
        crate::ops::Failed::Constraint(broken) => broken_constraint(broken, expr),
    }
}

/// What a type's constraints are, for notes.
const CONSTRAINT_RULE: &str = "a type is the values its constraints are true of, so selecting a value, by its selector, `replace` or `update`, fails when its components break one (TTM RM Prescription 23)";

/// `expr`, which selects `broken.value`, whose components break a
/// constraint of its type.
fn broken_constraint(broken: &crate::ops::BrokenConstraint, expr: &Expr) -> Diagnostic {
    let t = &broken.value.ty.name;
    let value = hir::Value::Defined(broken.value.clone());
    let message = format!("`{value}` isn't a value of `{t}`");
    let d = match broken.at {
        Some(at) => Diagnostic::error(
            Code::BrokenConstraint,
            message,
            expr.span(),
            format!("its components break a constraint of `{t}`"),
        )
        .with_secondary(at, "false of them"),
        None => Diagnostic::error(
            Code::BrokenConstraint,
            message,
            expr.span(),
            format!("its components break `{t}`'s constraint `{}`", broken.text),
        ),
    };
    d.with_note(CONSTRAINT_RULE)
}

/// `expr`, which takes `builtin`, `parse_integer`, `parse_approximate` or
/// `parse_boolean`, of `text`, which isn't a value of its type, as `why`
/// says.
fn unparsable(
    builtin: hir::ReadOnlyBuiltin,
    text: &str,
    why: hir::Unparsable,
    expr: &Expr,
) -> Diagnostic {
    let name = builtin.name();
    let quoted = hir::Value::Text(text.to_string());
    let (a, rule) = match builtin {
        hir::ReadOnlyBuiltin::ParseInteger => ("an integer", "`parse_integer` reads an integer as trendy writes one: decimal digits, with a `-` before them if it is negative, and nothing else, not even spaces".to_string()),
        hir::ReadOnlyBuiltin::ParseApproximate => ("an approximate number", "`parse_approximate` reads a decimal number: digits, then a `.` and more digits if it has a fraction, then an exponent if it has one, `e` or `E`, an optional sign and digits, all with a `-` before them if it is negative, and nothing else, not even spaces".to_string()),
        hir::ReadOnlyBuiltin::ParseBoolean => {
            let words = |words: &[&str]| {
                let words: Vec<String> = words.iter().map(|w| format!("`{w}`")).collect();
                trendy_diag::disjoin(&words)
            };
            ("a boolean", format!(
                "`parse_boolean` reads {} as true, and {} as false, in any case, and nothing else, not even spaces",
                words(&trendy_text::TRUE_WORDS),
                words(&trendy_text::FALSE_WORDS),
            ))
        }
        other => unreachable!("`{}` reads no text", other.name()),
    };
    match why {
        hir::Unparsable::Malformed => {
            let d = Diagnostic::error(
                Code::Unparsable,
                format!("{quoted} isn't {a}"),
                expr.span(),
                format!("`{name}` of text that isn't {a}"),
            )
            .with_note(rule);
            if builtin == hir::ReadOnlyBuiltin::ParseInteger
                && hir::ReadOnlyBuiltin::ParseApproximate
                    .evaluate(&[("from".to_string(), quoted.clone())].into())
                    .is_ok()
            {
                d.with_help("it is a decimal number with a fraction or an exponent, which `parse_approximate` reads as an approximate number")
            } else {
                d
            }
        }
        hir::Unparsable::OutOfRange => {
            let d = Diagnostic::error(
                Code::Unparsable,
                format!("{quoted} is out of {a}'s range"),
                expr.span(),
                format!("`{name}` of a number out of range"),
            );
            if builtin == hir::ReadOnlyBuiltin::ParseInteger {
                d.with_note(format!(
                    "integers are 64-bit signed, from {} to {}",
                    i64::MIN,
                    i64::MAX
                ))
                .with_help("`parse_approximate` reads it as the approximate number nearest it")
            } else {
                d.with_note(format!(
                    "approximate numbers are finite, up to about {:e} in magnitude; a larger one is an error, not an infinity",
                    f64::MAX
                ))
            }
        }
    }
}

/// `expr`, evaluated when compiling, changes an array twice where the
/// changes overlap: they set the same element, or one sets an element in
/// the other's.
fn overlapping_elements(expr: &Expr) -> Diagnostic {
    Diagnostic::error(
        Code::ConflictingChanges,
        "two changes to an array set the same element",
        expr.span(),
        "evaluating this sets one element twice",
    )
    .with_note(CHANGES_RULE)
    .with_help(
        "give the changes different positions, or make them in two statements, one after the other",
    )
}

/// `expr`, which takes `builtin`, `min` or `max`, of a relation without
/// tuples.
fn no_tuples(builtin: hir::ReadOnlyBuiltin, expr: &Expr) -> Diagnostic {
    let name = builtin.name();
    let extreme = match builtin {
        hir::ReadOnlyBuiltin::Min => "least",
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
/// the first: variables' initial values, or read-only operators that call
/// each other. No cycle has both, as an operator reads no variable.
pub(crate) fn cycle(cycle: &[Pending]) -> Diagnostic {
    let first = &cycle[0];
    // How each is written, and what the one before it does to it.
    let shown = |p: &Pending| match p.global {
        Global::Constraint(_) => format!("`{}`'s constraint", p.name),
        Global::Variable(_) | Global::Operator(_) => format!("`{}`", p.name),
    };
    let names: Vec<String> = cycle.iter().map(shown).collect();
    let d = match first.global {
        Global::Constraint(_) => {
            let ty = &first.name;
            let message = match names.as_slice() {
                [_] => format!("`{ty}`'s constraint selects a value of `{ty}`"),
                [_, rest @ ..] => format!(
                    "`{ty}`'s constraint selects a value of `{ty}` through {}",
                    trendy_diag::conjoin(rest)
                ),
                [] => unreachable!("a cycle has a global in it"),
            };
            Diagnostic::error(Code::Recursion, message, first.defined, "this constraint")
                .with_note(
                    "a type's constraints are checked whenever a value of it is selected, by its selector, `replace` or `update`, so a constraint that selects one, directly or through operators, would check itself without end",
                )
                .with_help("compute the condition from the components without selecting a value of the type")
        }
        Global::Operator(_) => {
            let message = match names.as_slice() {
                [_] => format!("`{}` calls itself", first.name),
                _ => format!("{} call each other", trendy_diag::conjoin(&names)),
            };
            Diagnostic::error(Code::Recursion, message, first.defined, "defined here")
                .with_note(
                    "a read-only operator can't call itself, directly or through others: without a condition to stop on, its call would never end",
                )
                .with_help("compute the value without calling the operator from its own body")
        }
        Global::Variable(_) => {
            let message = match names.as_slice() {
                [_] => format!("the initial value of `{}` depends on itself", first.name),
                _ => format!(
                    "the initial values of {} depend on each other",
                    trendy_diag::conjoin(&names)
                ),
            };
            Diagnostic::error(Code::CyclicInitialValues, message, first.defined, "declared here")
            .with_note(
                "initial values are evaluated when compiling, each after the ones it refers to, so a cycle has no order to evaluate them in",
            )
            .with_help("give one of these variables a value that doesn't refer to the others")
        }
    };
    cycle.iter().enumerate().fold(d, |d, (i, pending)| {
        let next = &cycle[(i + 1) % cycle.len()];
        let (this, that) = (shown(pending), shown(next));
        let label = match (pending.global, next.global) {
            (_, Global::Constraint(_)) if next.global == pending.global => {
                format!(
                    "{this} selects a value of `{}`, which checks it again",
                    next.name
                )
            }
            (_, Global::Constraint(_)) => {
                format!(
                    "{this} selects a value of `{}`, which checks {that}",
                    next.name
                )
            }
            (Global::Variable(_), _) if next.global == pending.global => {
                format!("{this} refers to itself")
            }
            (Global::Variable(_), _) => format!("{this} refers to {that}"),
            _ if next.global == pending.global => format!("{this} calls itself"),
            _ => format!("{this} calls {that}"),
        };
        d.with_secondary(pending.via, label)
    })
}

/// How a type's possible representations relate, for notes.
const REPRESENTATIONS: &str = "a type's values are stored as one of its possible representations, the one without conversions; each other converts to it with `to { stored := … }`, computing the stored components from its own, and from it with `from { own := … }`, computing its own from the stored ones";

/// What a conversion, after `keyword`, `to` or `from`, computes: each of
/// `components`, for notes.
pub(crate) fn conversion_note(keyword: &str, components: &hir::Heading) -> String {
    let names = named_attributes(components);
    let which = if keyword.eq_ignore_ascii_case("to") {
        "the stored representation's components"
    } else {
        "this representation's components"
    };
    format!(
        "`{keyword}` computes each of {which}, {}, once; {REPRESENTATIONS}",
        trendy_diag::conjoin(&names)
    )
}

/// The type `name`, declared at `ident`, every one of whose possible
/// representations has conversions, so that none is stored.
pub(crate) fn no_stored_representation(name: &str, ident: &SyntaxToken) -> Diagnostic {
    Diagnostic::error(
        Code::Conversions,
        format!("no possible representation of `{name}` is stored"),
        trendy_syntax::span(ident.text_range()),
        "each of its representations has conversions",
    )
    .with_note(REPRESENTATIONS)
    .with_help("remove the conversions of the representation to store its values as")
}

/// `representation`, a second possible representation of the type `name`
/// without conversions, after `stored`, the one its values are stored as.
pub(crate) fn second_stored(
    name: &str,
    representation: &trendy_syntax::ast::Representation,
    stored: &trendy_syntax::ast::Representation,
) -> Diagnostic {
    Diagnostic::error(
        Code::Conversions,
        format!("`{name}` has more than one possible representation without conversions"),
        representation.span(),
        "no `to` or `from`",
    )
    .with_secondary(stored.span(), "its values are stored as this one")
    .with_note(REPRESENTATIONS)
    .with_help("convert it to the stored one and from it, after its braces")
}

/// `representation`, other than the stored one, without a conversion
/// `to` the stored one, if `to` is true, or else `from` it.
pub(crate) fn missing_conversion(
    representation: &trendy_syntax::ast::Representation,
    to: bool,
) -> Diagnostic {
    let (missing, help) = if to {
        (
            "`to`",
            "add `to { stored := … }`, computing each stored component from its own",
        )
    } else {
        (
            "`from`",
            "add `from { own := … }`, computing each of its components from the stored ones",
        )
    };
    Diagnostic::error(
        Code::Conversions,
        format!("this possible representation has no conversion {missing}"),
        representation.span(),
        format!("no {missing}"),
    )
    .with_note(REPRESENTATIONS)
    .with_help(help)
}

/// `conversion`, given again after `first`, which goes the same way.
pub(crate) fn repeated_conversion(
    conversion: &trendy_syntax::ast::Conversion,
    first: &trendy_syntax::ast::Conversion,
) -> Diagnostic {
    let keyword = conversion
        .keyword()
        .map_or_else(String::new, |k| k.text().to_string());
    Diagnostic::error(
        Code::DuplicateAttribute,
        format!("`{keyword}` is given more than once"),
        conversion.span(),
        "given again here",
    )
    .with_secondary(first.span(), "first given here")
    .with_note(REPRESENTATIONS)
    .with_suggestion(
        "delete the second",
        vec![Edit {
            span: conversion.removal(),
            replacement: String::new(),
        }],
    )
}

/// `to` and `from`, whose assignments compute what the other's should: the
/// representation's own components, and the stored ones.
pub(crate) fn swapped_conversions(
    to: &trendy_syntax::ast::Conversion,
    from: &trendy_syntax::ast::Conversion,
) -> Diagnostic {
    let span = |c: &trendy_syntax::ast::Conversion| {
        c.keyword()
            .map_or_else(|| c.span(), |k| trendy_syntax::span(k.text_range()))
    };
    Diagnostic::error(
        Code::Conversions,
        "`to` and `from` are the wrong way round",
        span(to),
        "this computes this representation's own components",
    )
    .with_secondary(span(from), "and this the stored ones")
    .with_note(REPRESENTATIONS)
    .with_suggestion(
        "swap `to` and `from`",
        vec![
            Edit {
                span: span(to),
                replacement: "from".to_string(),
            },
            Edit {
                span: span(from),
                replacement: "to".to_string(),
            },
        ],
    )
}

/// The component `component`, at `span`, of a possible representation of
/// the type `name`, which another of them has, at `first`.
pub(crate) fn component_in_two(name: &str, component: &str, span: Span, first: Span) -> Diagnostic {
    Diagnostic::error(
        Code::DuplicateAttribute,
        format!("two possible representations of `{name}` have a component `{component}`"),
        span,
        "a second one",
    )
    .with_secondary(first, "the first")
    .with_note("a component's name says which representation it is of, as `v.c` reads it, so each is in one")
    .with_help("rename one of them")
}

/// Why a type's constraint depends on the components alone, for notes.
const CONSTRAINT_OF_COMPONENTS: &str = "a type's constraint says which values the type has, so it is a condition on a value's components alone: one that read a variable, whose value changes, would make a value of the type one at one time and not at another";

/// `constraint`, of the type `defined`, whose condition is known when
/// compiling to be `holds` whatever the components: true, which says
/// nothing, and is deleted, or false, which no value satisfies.
pub(crate) fn constant_constraint(
    holds: bool,
    constraint: &trendy_syntax::ast::Constraint,
    defined: &hir::Defined,
) -> Diagnostic {
    let span = constraint
        .condition()
        .map_or_else(|| constraint.span(), |c| c.span());
    let t = &defined.name;
    if holds {
        Diagnostic::error(
            Code::ConstantConstraint,
            format!("this constraint is true of every value of `{t}`"),
            span,
            "true whatever the components",
        )
        .with_note("a type's constraints say which of the values its components could make are its own, so one true of them all says nothing")
        .with_suggestion(
            "delete the constraint",
            vec![Edit {
                span: constraint.removal(),
                replacement: String::new(),
            }],
        )
    } else {
        Diagnostic::error(
            Code::ConstantConstraint,
            format!("no value of `{t}` satisfies this constraint"),
            span,
            "false whatever the components",
        )
        .with_note(format!("a type's constraints say which of the values its components could make are its own, so with this one `{t}` would have none, and every selection of one would fail"))
        .with_help("write the condition on the components, which some of their values satisfy")
    }
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

/// `stmt`, a `return` in the update operator `name`: of a value, which
/// it hasn't, or, without one, an early exit, which no operator has yet.
pub(crate) fn return_in_update(name: &str, stmt: &trendy_syntax::ast::ReturnStmt) -> Diagnostic {
    let span = stmt
        .keyword()
        .map_or_else(|| stmt.span(), |t| trendy_syntax::span(t.text_range()));
    if stmt.value().is_none() {
        return Diagnostic::error(
            Code::Return,
            format!("`{name}` can't return early"),
            span,
            "an early exit",
        )
        .with_note(EARLY_EXIT)
        .with_help("put what the rest does in an `if` statement's `else`");
    }
    Diagnostic::error(
        Code::Return,
        format!("`{name}` is an update operator, which returns no value"),
        span,
        "returns a value",
    )
    .with_note(
        "only a read-only operator, which has a result type, has a value, which its body ends with: `operator name{params}: type [value];`",
    )
    .with_help(format!(
        "give `{name}` a result type to make it read-only, or delete the `return`"
    ))
}

/// `stmt`, a `return` in a read-only operator before the end of its body:
/// an early exit, which no operator has yet.
pub(crate) fn early_return(stmt: &trendy_syntax::ast::ReturnStmt) -> Diagnostic {
    let span = stmt
        .keyword()
        .map_or_else(|| stmt.span(), |t| trendy_syntax::span(t.text_range()));
    Diagnostic::error(
        Code::Return,
        "a read-only operator can't return early",
        span,
        "an early exit",
    )
    .with_note(EARLY_EXIT)
    .with_help("choose between values with an `if` expression, and end the body with it")
}

/// Why there are no early exits, for notes.
const EARLY_EXIT: &str = "`return` leaves an operator before the end of its body, which no operator does yet: a body runs to its end, where a read-only operator's value is";

/// A read-only operator's body, for notes.
const BODY_VALUE: &str = "a read-only operator's body declares and assigns its local variables, if it has any, then ends with its value, an expression of its result type, without `;`";

/// What a read-only operator changes, for notes.
const READ_ONLY: &str = "a read-only operator changes nothing but its own local variables (TTM RM Prescription 3): its value is computed from its arguments alone";

/// `stmt`, a `call` in the read-only operator `name`, which would change
/// something.
pub(crate) fn call_in_read_only(name: &str, stmt: &trendy_syntax::ast::CallStmt) -> Diagnostic {
    Diagnostic::error(
        Code::ReadOnlyEffect,
        "a read-only operator can't call an operator",
        stmt.span(),
        format!("a `call` in `{name}`"),
    )
    .with_note(READ_ONLY)
    .with_help(format!(
        "to change something, make `{name}` an update operator, without a result type, invoked by `call`"
    ))
}

/// An `if` statement, `stmt`, in the read-only operator `name`, which
/// chooses between values with an `if` expression.
pub(crate) fn if_in_read_only(name: &str, stmt: &trendy_syntax::ast::IfStmt) -> Diagnostic {
    let at = match (stmt.keyword(), stmt.condition()) {
        (Some(keyword), Some(condition)) => Span::new(
            trendy_syntax::span(keyword.text_range()).start,
            condition.span().end,
        ),
        _ => stmt.span(),
    };
    Diagnostic::error(
        Code::Return,
        "a read-only operator has no `if` statements",
        at,
        format!("an `if` statement in `{name}`"),
    )
    .with_note("a read-only operator's body declares and assigns its local variables and ends with its value: it computes one value, choosing between values with `if` expressions, not statements")
    .with_help("compute the value with an `if` expression, as in `x := if c then a else b;`, or make the operator an update operator, without a result type, invoked by `call`")
}

/// The variable `name`, at `span`, of the program's, assigned in the
/// read-only operator `op`.
pub(crate) fn assigned_in_read_only(name: &str, span: Span, op: &str) -> Diagnostic {
    Diagnostic::error(
        Code::ReadOnlyEffect,
        "a read-only operator can't assign a variable of the program's",
        span,
        format!("a variable of the program's, assigned in `{op}`"),
    )
    .with_note(READ_ONLY)
    .with_help(format!(
        "to compute a value, assign a local variable of `{op}`'s, as in `var new_{name} := …;`; to change `{name}`, make `{op}` an update operator, without a result type, invoked by `call`"
    ))
}

/// The read-only operator `name`, whose `body` doesn't end with a value,
/// and whose result type is `written`.
pub(crate) fn missing_value(
    name: &str,
    body: &trendy_syntax::ast::Body,
    written: &trendy_syntax::ast::TypeExpr,
) -> Diagnostic {
    let at = body.end().map_or_else(|| body.span(), Span::empty);
    Diagnostic::error(
        Code::Return,
        format!("read-only operator `{name}` has no value"),
        at,
        "no value at the end",
    )
    .with_secondary(written.span(), format!("`{name}` has a value of this type"))
    .with_note(BODY_VALUE)
    .with_help("end the body with its value, as in `x * 2`")
}

/// `expr`, a call of `signature` with constant arguments, whose value
/// fails, as `failed` says.
pub(crate) fn call_failure(
    failed: &crate::ops::Failed,
    expr: &Expr,
    signature: &Signature,
) -> Diagnostic {
    // A built-in's value is the compiler's, with nothing to point at, and
    // a selector's is the selection that fails.
    if signature.builtin.is_some() || signature.selects().is_some() {
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
        Code::UnknownEmptyType,
        "the heading of this empty relation is unknown",
        span,
        "no tuples, no heading, and no expected type",
    )
    .with_note("a relation's heading comes from its selector's heading, from its tuples, or from the type expected of it")
    .with_help("write the heading, as in `relation { name: text } {}`, or declare the variable's type, as in `var r: relation { name: text } := relation {};`")
}

/// An array value at `span` with no elements and nothing else giving
/// their type.
pub(crate) fn unknown_array_type(span: Span) -> Diagnostic {
    Diagnostic::error(
        Code::UnknownEmptyType,
        "the type of this empty array's elements is unknown",
        span,
        "no elements, no element type, and no expected type",
    )
    .with_note("an array's elements' type comes from its selector's element type, from its elements, or from the type expected of it")
    .with_help("write the elements' type, as in `array text []`, or declare the variable's type, as in `var a: array text := array [];`")
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

/// An operator written `expr` that has no result because of `failure`.
/// `at` is the operand the failure is reported at, if not the whole: the
/// divisor of a division by zero, or the position of an index out of
/// range.
pub(crate) fn failure(failure: hir::Failure, expr: &Expr, at: Option<&Expr>) -> Diagnostic {
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
            at.map_or(expr.span(), Expr::span),
            "this is zero",
        )
        .with_note("`/`, `div` and `mod` have no result when the divisor is zero"),
        hir::Failure::OutOfRange { index, length } => out_of_range(index, length, expr, at),
        hir::Failure::NegativeRoot(x) => Diagnostic::error(
            Code::NoResult,
            format!("{x} has no square root"),
            expr.span(),
            "the square root of a negative number",
        )
        .with_note("no approximate number's square is negative, so a negative one has no square root; approximate numbers have no NaN for it, nor imaginary numbers"),
    }
}

/// The array `a[i]`, written `expr`, has no element at the position `i`,
/// written `position`, whose value is `index`, as its `length`, if known,
/// shows. Positions count from 1, so other languages' first element,
/// `a[0]`, and elements counted from the end, as `a[-1]`, are fixed.
fn out_of_range(
    index: i64,
    length: Option<usize>,
    expr: &Expr,
    position: Option<&Expr>,
) -> Diagnostic {
    let (message, label) = match length {
        Some(0) => (
            format!("the array is empty, so it has no element at position {index}"),
            "an empty array has no positions".to_string(),
        ),
        Some(1) if index >= 1 => (
            format!("the array has 1 element, so none at position {index}"),
            "its only position is 1".to_string(),
        ),
        Some(length) if index >= 1 => (
            format!("the array has {length} elements, so none at position {index}"),
            format!("positions run from 1 to {length}"),
        ),
        _ => (
            format!("an array has no element at position {index}"),
            "positions count from 1".to_string(),
        ),
    };
    let span = position.map_or(expr.span(), Expr::span);
    let d = Diagnostic::error(Code::IndexOutOfRange, message, span, label).with_note(
        "an array's elements are at positions 1 up to its length, counting from 1 as SQL and Tutorial D do",
    );
    let array = trendy_syntax::ast::IndexExpr::cast(expr.syntax().clone())
        .and_then(|index| index.operand());
    let (Some(array), true) = (array, position.and_then(written_literal).is_some()) else {
        return d;
    };
    let length = format!("length{{a: {}}}", array.syntax().text());
    let (message, replacement) = match index {
        0 => ("for the first element, index it at 1", "1".to_string()),
        -1 => (
            "to count from the end, index from the array's length",
            length,
        ),
        _ if index < 0 => (
            "to count from the end, index from the array's length",
            format!("{length} - {}", -(index + 1)),
        ),
        _ => return d,
    };
    d.with_suggestion(message, vec![Edit { span, replacement }])
}

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
            hir::Any::Array => format!(
                "`{p}` can be any array, so its elements' type isn't known here: only `=` and `<>` apply to its values, and `length`, and passing them on"
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
        hir::Any::Type | hir::Any::Tuple | hir::Any::Relation | hir::Any::Array => &none,
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

/// How a type's values are named in the plural.
fn plural(ty: &hir::Type) -> String {
    match ty {
        hir::Type::Scalar(hir::Scalar::Boolean) => "booleans".to_string(),
        hir::Type::Scalar(hir::Scalar::Integer) => "integers".to_string(),
        hir::Type::Scalar(hir::Scalar::Approximate) => "approximate numbers".to_string(),
        hir::Type::Scalar(hir::Scalar::Text) => "text".to_string(),
        hir::Type::Defined(defined) => format!("values of `{}`", defined.name),
        hir::Type::Tuple(_) => "tuples".to_string(),
        hir::Type::Relation(_) => "relations".to_string(),
        hir::Type::Array(_) => "arrays".to_string(),
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
            (Operator::Compare(_), hir::Type::Array(_)) => d.with_note(
                "arrays can only be compared with `=` and `<>`: they are equal if they have equal elements at each position",
            ),
            (Operator::Compare(_), hir::Type::Defined(defined)) => d
                .with_note(format!(
                    "`{}` has no order, so its values can only be compared with `=` and `<>`, equal when their components are",
                    defined.name
                ))
                .with_help(format!(
                    "give `{}` an order, `order [asc …]` in its braces, its components in the order they compare",
                    defined.name
                )),
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
                    with_conversion(d, &subject, hir::ReadOnlyBuiltin::ToApproximate, first)
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
    // As the program would say when running.
    let message = if empty {
        format!("`{name}` has two tuples, but its key {{}} allows at most one")
    } else {
        let names: Vec<&str> = value.attributes().map(|(a, _)| a).collect();
        format!(
            "two tuples of `{name}` have the same values for its key {{ {} }}",
            names.join(", ")
        )
    };
    let mut d = Diagnostic::error(Code::KeyViolated, message, at, label);
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

const LOAD: &str = "`load a from r` gives an array of tuples the tuples of a relation of their heading, and `load a value from r` an array the values of a relation's only attribute, of their type";

/// The rule for a load into a relation, for notes.
const UNLOAD: &str = "`load r from a` gives a relation a tuple for each element of an array: the element, if it is a tuple, or, after `as x`, a tuple of one attribute, `x`, whose value it is; `numbered n` adds `n`, the element's position, counting from 1, and elements that make equal tuples make one";

/// A load into `name`, of type `ty`, written `target`, which is neither an
/// array nor a relation, of `source`, of type `value`, written as the
/// clause at `clause`. A value of `ty` is assigned instead.
pub(crate) fn load_into_other(
    name: &str,
    ty: &hir::Type,
    target: &Expr,
    value: &hir::Type,
    clause: Span,
    source: &Expr,
) -> Diagnostic {
    let d = Diagnostic::error(
        Code::LoadMismatch,
        format!("`{name}` is neither an array nor a relation, so a load can't fill it"),
        target.span(),
        format!("of type `{ty}`"),
    )
    .with_note(LOAD)
    .with_note(UNLOAD);
    if ty == value {
        d.with_suggestion(
            format!("assign the value to `{name}`"),
            vec![Edit {
                span: clause,
                replacement: format!("{name} := {}", source.syntax().text()),
            }],
        )
    } else {
        d
    }
}

/// `clause`, at `span`, of a load into `name`, which is an array if
/// `array` says so, and a relation if not, though the clause is for a load
/// into the other: the fix deletes `removal`.
pub(crate) fn clause_of_other_load(
    name: &str,
    array: bool,
    clause: &str,
    span: Span,
    removal: Span,
) -> Diagnostic {
    let (is, other, rule) = if array {
        ("an array", "a relation", LOAD)
    } else {
        ("a relation", "an array", UNLOAD)
    };
    let d = Diagnostic::error(
        Code::LoadMismatch,
        format!("`{clause}` is for a load into {other}, but `{name}` is {is}"),
        span,
        format!("not for a load into {is}"),
    )
    .with_note(rule);
    let d = if clause == "order" {
        d.with_note("a relation's tuples have no order, so a load into one has none")
    } else {
        d
    };
    d.with_suggestion(
        format!("remove `{clause}`"),
        vec![Edit {
            span: removal,
            replacement: String::new(),
        }],
    )
}

/// A load into `name`, an array, with no order after its relation, which
/// ends at `at`, with its `where`, if it has one.
pub(crate) fn load_without_order(name: &str, at: usize) -> Diagnostic {
    Diagnostic::error(
        Code::LoadOrder,
        format!("a load into an array needs an order, and `{name}` is one"),
        Span::empty(at),
        "no `order` after the relation",
    )
    .with_note("a relation's tuples have no order, so a load into an array says which to put them in")
    .with_help("write the order after the relation: `order`, then, in brackets, the attributes to order by, each after its direction, `asc` or `desc`, as in `order [desc born, asc name]`")
}

/// A load into `name`, a relation of type `target`, of `source`, of type
/// `ty`, which isn't an array. A relation of `target` is assigned instead,
/// those `where` keeps, if the load has it.
pub(crate) fn unload_of_non_array(
    name: &str,
    target: &hir::Type,
    ty: &hir::Type,
    source: &Expr,
    load: &ast::Load,
) -> Diagnostic {
    let d = Diagnostic::error(
        Code::LoadMismatch,
        format!("a load fills `{name}`, a relation, from an array, but this is `{ty}`"),
        source.span(),
        "not an array",
    )
    .with_note(UNLOAD);
    if ty != target {
        return match ty {
            hir::Type::Relation(_) => d.with_help(format!(
                "a relation is assigned to `{name}` with `:=`, not loaded"
            )),
            _ => d,
        };
    }
    let condition = load
        .condition()
        .and_then(|c| c.condition())
        .map(|c| format!(" where {}", c.syntax().text()))
        .unwrap_or_default();
    d.with_suggestion(
        format!("assign the relation to `{name}`"),
        vec![Edit {
            span: load.span(),
            replacement: format!("{name} := {}{condition}", source.syntax().text()),
        }],
    )
}

/// The edit that makes `load`'s `as x` and `numbered n`, after its array,
/// those of `way`.
fn unloading_edit(load: &ast::Load, way: &hir::Unloading) -> Option<Edit> {
    let start = load.source_end()?;
    let end = [
        load.element_name().map(|n| n.span().end),
        load.numbering().map(|n| n.span().end),
    ]
    .into_iter()
    .flatten()
    .max()
    .unwrap_or(start);
    let named = way.named.iter().map(|x| format!(" as {x}"));
    let numbered = way.numbered.iter().map(|n| format!(" numbered {n}"));
    Some(Edit {
        span: Span::new(start, end),
        replacement: named.chain(numbered).collect(),
    })
}

/// Help that writes a load's `as x` and `numbered n` as `way` says, or
/// `help` if there's no `way`.
fn unloading_fix(
    d: Diagnostic,
    load: &ast::Load,
    way: Option<&hir::Unloading>,
    help: String,
) -> Diagnostic {
    let Some((way, edit)) = way.and_then(|w| Some((w, unloading_edit(load, w)?))) else {
        return d.with_help(help);
    };
    let clauses: Vec<String> = [
        way.named.as_ref().map(|x| format!("`as {x}`")),
        way.numbered.as_ref().map(|n| format!("`numbered {n}`")),
    ]
    .into_iter()
    .flatten()
    .collect();
    let text = if clauses.is_empty() {
        "load the elements as they are".to_string()
    } else {
        format!("load them with {}", clauses.join(" and "))
    };
    d.with_suggestion(text, vec![edit])
}

/// A load into `name`, a relation, of an array of `element`s, which
/// aren't tuples, without `as x`. `way` makes it fit `name`, if it can.
pub(crate) fn unnamed_elements(
    name: &str,
    element: &hir::Type,
    load: &ast::Load,
    way: Option<&hir::Unloading>,
) -> Diagnostic {
    let at = load.source_end().unwrap_or_else(|| load.span().end);
    let d = Diagnostic::error(
        Code::LoadMismatch,
        format!("the array's elements are `{element}`, not tuples, so the load names the attribute each becomes"),
        Span::empty(at),
        "`as` and the attribute's name go here",
    )
    .with_note(UNLOAD);
    unloading_fix(
        d,
        load,
        way,
        format!(
            "write `as` and the attribute's name after the array, as in `load {name} from lines as line`"
        ),
    )
}

/// `numbered`, after `numbering`'s `numbered`, in `load`, the name of an
/// attribute each element's tuple already has. `way` makes it fit the
/// relation loaded, if it can.
pub(crate) fn numbered_attribute_exists(
    numbered: &str,
    numbering: &ast::Numbering,
    load: &ast::Load,
    way: Option<&hir::Unloading>,
) -> Diagnostic {
    let span = numbering
        .name()
        .map_or_else(|| numbering.span(), |n| n.span());
    let d = Diagnostic::error(
        Code::AttributeExists,
        format!(
            "each element's tuple already has `{numbered}`, so the positions need another name"
        ),
        span,
        "an attribute of the elements' tuples",
    )
    .with_note(UNLOAD)
    .with_note("a tuple's attributes are a set, so each name appears once");
    unloading_fix(
        d,
        load,
        way,
        "name the positions' attribute one the tuples haven't".to_string(),
    )
}

/// A load into `name`, a relation with `heading`, of `source`, an array
/// whose elements make tuples of `made`. `way` makes them fit, if it can.
pub(crate) fn unloaded_mismatch(
    name: &str,
    heading: &hir::Heading,
    made: &hir::Heading,
    source: &Expr,
    load: &ast::Load,
    way: Option<&hir::Unloading>,
) -> Diagnostic {
    let tuple = |h: &hir::Heading| {
        hir::Type::Tuple(h.clone())
            .to_string()
            .trim_start_matches("tuple ")
            .to_string()
    };
    let d = Diagnostic::error(
        Code::LoadMismatch,
        format!(
            "the load gives `{name}` tuples of `{}`, but its tuples are `{}`",
            tuple(made),
            tuple(heading)
        ),
        source.span(),
        format!("its elements make tuples of {}", listed(made)),
    )
    .with_note(UNLOAD);
    unloading_fix(
        d,
        load,
        way,
        format!(
            "name the attributes the elements make as `{name}`'s heading does, with `as` and `numbered`"
        ),
    )
}

/// A load of `source`, of type `ty`, into `name`, an array of `element`s,
/// written `load a value from r`, that it can't fill: it isn't a relation
/// of one attribute of their type. `keep` keeps only the attribute of
/// their type, if the relation has one.
pub(crate) fn load_mismatch(
    name: &str,
    element: &hir::Type,
    ty: &hir::Type,
    source: &Expr,
    keep: Option<(String, Edit)>,
) -> Diagnostic {
    let label = match ty {
        hir::Type::Relation(_) => format!("not a relation of one `{element}` attribute"),
        _ => "not a relation".to_string(),
    };
    let d = Diagnostic::error(
        Code::LoadMismatch,
        format!("a load can't fill `{name}`, an `array {element}`, from `{ty}`"),
        source.span(),
        label,
    )
    .with_note(LOAD);
    match keep {
        Some((help, edit)) => d.with_suggestion(help, vec![edit]),
        None => d,
    }
}

/// A load of `source`, of type `ty`, into `name`, an array of `element`s,
/// which aren't tuples, without `value` before `from`, written `from`. The
/// fix writes it, if the relation has one attribute, of their type, or
/// makes the load `keep` only that one as well, if it has others.
pub(crate) fn load_without_value(
    name: &str,
    element: &hir::Type,
    ty: &hir::Type,
    source: &Expr,
    from: Option<&SyntaxToken>,
    keep: Option<(String, Edit)>,
) -> Diagnostic {
    let span = from.map_or_else(|| source.span(), |f| trendy_syntax::span(f.text_range()));
    let d = Diagnostic::error(
        Code::LoadMismatch,
        format!("`{name}` is an `array {element}`, not of tuples, so the load takes values: `load {name} value from …`"),
        span,
        "`value` goes before `from`",
    )
    .with_note(LOAD)
    .with_note("an array of tuples and an array of their values are different types, so the load says which it fills");
    let Some(from) = from else {
        return d;
    };
    let start = trendy_syntax::span(from.text_range()).start;
    let mut edits = vec![Edit {
        span: Span::new(start, start),
        replacement: "value ".to_string(),
    }];
    let fits = matches!(ty, hir::Type::Relation(heading)
        if heading.attributes().map(|(_, ty)| ty).eq([element]));
    if !fits {
        let Some((_, keep)) = keep else {
            return d.with_help(format!(
                "write `value` before `from`, and load a relation of one `{element}` attribute"
            ));
        };
        edits.push(keep);
    }
    d.with_suggestion("load the values, with `value`", edits)
}

/// `value`, before `from`, in a load into `name`, whose elements are the
/// relation's tuples as they are.
pub(crate) fn needless_value(name: &str, value: &SyntaxToken, from: &SyntaxToken) -> Diagnostic {
    let span = trendy_syntax::span(value.text_range());
    Diagnostic::error(
        Code::LoadMismatch,
        format!("`{name}`'s elements are the relation's tuples, so the load doesn't take values"),
        span,
        "not the values of one attribute",
    )
    .with_note(LOAD)
    .with_suggestion(
        "remove `value`",
        vec![Edit {
            span: Span::new(span.start, trendy_syntax::span(from.text_range()).start),
            replacement: String::new(),
        }],
    )
}

/// The rule for leaving attributes out of a load's elements, for notes.
const LOAD_REMOVE: &str = "`remove {x}` after a load's order leaves attributes out of the elements, and `keep {x}` all the others, once the tuples are ordered by them, keeping each element, though some turn out equal; `remove` on the relation leaves them out before, so the order can't name them, and merges tuples that turn out equal";

/// `attribute`, at `span`, which a load's `order` names, though the
/// relation it loads is `postfix`, which removes it, in `list`: the fix
/// moves the `remove` after the order, unless the load has a list of its
/// elements' attributes there already, `keeping`.
pub(crate) fn ordered_after_removal(
    attribute: &str,
    span: Span,
    postfix: &trendy_syntax::ast::PostfixExpr,
    list: &trendy_syntax::ast::NameList,
    order: &OrderList,
    keeping: bool,
) -> Diagnostic {
    let removal = postfix.operand().map_or_else(
        || postfix.span(),
        |o| Span::new(o.span().end, postfix.span().end),
    );
    let d = Diagnostic::error(
        Code::UnknownAttribute,
        format!("`{attribute}` is removed from the relation before the load orders its tuples"),
        span,
        "not an attribute of the relation loaded",
    )
    .with_secondary(removal, "removed here")
    .with_note(LOAD_REMOVE);
    if keeping {
        return d.with_help(format!(
            "leave `{attribute}` out after the order, not from the relation"
        ));
    }
    let end = order.span().end;
    d.with_suggestion(
        "remove it after the order instead",
        vec![
            Edit {
                span: removal,
                replacement: String::new(),
            },
            Edit {
                span: Span::new(end, end),
                replacement: format!(" remove {}", list.syntax().text()),
            },
        ],
    )
}

/// A load of `source` into `name`, an array of tuples of `elements`, which
/// gives them `kept` instead, the tuples' attributes less those it leaves
/// out. `keep` makes it keep the elements' attributes, if the tuples have
/// them.
pub(crate) fn loaded_tuples_mismatch(
    name: &str,
    elements: &hir::Heading,
    kept: &hir::Heading,
    source: &Expr,
    keep: Option<(String, Edit)>,
) -> Diagnostic {
    let d = Diagnostic::error(
        Code::LoadMismatch,
        format!(
            "the load gives `{name}` elements of `tuple {}`, but they are `tuple {}`",
            hir::Type::Tuple(kept.clone())
                .to_string()
                .trim_start_matches("tuple "),
            hir::Type::Tuple(elements.clone())
                .to_string()
                .trim_start_matches("tuple ")
        ),
        source.span(),
        format!("its tuples, less those removed, have {}", listed(kept)),
    )
    .with_note(LOAD)
    .with_note(LOAD_REMOVE);
    match keep {
        Some((help, edit)) => d.with_suggestion(help, vec![edit]),
        None => d,
    }
}

/// The edit that makes `load`, of a relation with `heading`, keep only the
/// attributes `wanted`: `remove` or `keep` after the order, whichever names
/// fewer attributes, `remove` if they are as many, in place of the one it
/// has, if any, or none if it keeps every attribute. Its help, and it, if
/// there is an order to put it after.
pub(crate) fn keeping_edit(
    heading: &hir::Heading,
    wanted: &[&str],
    load: &trendy_syntax::ast::Load,
) -> Option<(String, Edit)> {
    let removed: Vec<&str> = heading
        .named()
        .map(|(a, _)| a)
        .filter(|a| !wanted.contains(a))
        .collect();
    let (help, text) = if removed.is_empty() {
        ("keep every attribute".to_string(), String::new())
    } else if removed.len() <= wanted.len() {
        let names = removed.join(", ");
        (
            format!("leave out `{names}`, after the order"),
            format!(" remove {{{names}}}"),
        )
    } else {
        let names = wanted.join(", ");
        (
            format!("keep only `{names}`, after the order"),
            format!(" keep {{{names}}}"),
        )
    };
    let span = if let Some(keeping) = load.keepings().next() {
        keeping.removal()
    } else {
        let end = load.order()?.span().end;
        Span::new(end, end)
    };
    Some((
        help,
        Edit {
            span,
            replacement: text,
        },
    ))
}

/// A load's order, `order`, with no items, of a relation with `heading`.
/// What an order orders: a load's relation's tuples, by attributes, or
/// the values of the type of the program's own named so, by components.
#[derive(Clone, Copy)]
pub(crate) enum Orders<'a> {
    Load,
    Type(&'a str),
}

impl Orders<'_> {
    fn noun(self) -> Noun {
        match self {
            Orders::Load => Noun::Ordered,
            Orders::Type(_) => Noun::Component,
        }
    }
}

/// The rule for a type's order, for notes.
const TYPE_ORDER: &str = "a type's order compares its values by their components, each of a type with `<`, in turn, until one differs, so it names every component, once, and values that differ are never equal in it";

/// `order`, an order of what `orders` says, with no items, of a relation
/// or type with `heading`.
pub(crate) fn empty_order(order: &OrderList, heading: &hir::Heading, orders: Orders) -> Diagnostic {
    let example = heading
        .attributes()
        .next()
        .map_or_else(|| "name".to_string(), |(name, _)| name.to_string());
    let many = orders.noun().many();
    let d = match orders {
        Orders::Load => Diagnostic::error(
            Code::EmptyList,
            "a load's order needs at least one attribute",
            order.span(),
            "no attributes to order by",
        )
        .with_note("a relation's tuples have no order, so a load says which to put them in"),
        Orders::Type(t) => Diagnostic::error(
            Code::EmptyList,
            format!("the order of `{t}` needs its components"),
            order.span(),
            "no components to order by",
        )
        .with_note(TYPE_ORDER),
    };
    d.with_help(format!(
        "list the {many} to order by, each after `asc` or `desc`, as in `[asc {example}]`"
    ))
}

/// An attribute or a component, as `orders` says, `name`, at `span`, that
/// an order orders by, which the relation or type, with `heading`, hasn't.
pub(crate) fn unknown_order_attribute(
    name: &str,
    span: Span,
    heading: &hir::Heading,
    orders: Orders,
) -> Diagnostic {
    let names: Vec<&str> = heading.attributes().map(|(a, _)| a).collect();
    let rename = closest(name, names.iter().copied());
    let listed: Vec<String> = names.iter().map(|a| format!("`{a}`")).collect();
    let context = match (orders, listed.len()) {
        (Orders::Load, 0) => "the relation has no attributes".to_string(),
        (Orders::Load, _) => format!("the relation's attributes are {}", listed.join(", ")),
        (Orders::Type(t), 0) => format!("`{t}` has no components"),
        (Orders::Type(t), _) => format!("`{t}`'s components are {}", listed.join(", ")),
    };
    unknown_attribute(orders.noun(), name, span, &context, rename, None)
}

/// An attribute or a component, as `orders` says, `name`, at `span`, of
/// type `ty`, which has no order, that an order orders by.
pub(crate) fn unordered_attribute(
    name: &str,
    span: Span,
    ty: &hir::Type,
    orders: Orders,
) -> Diagnostic {
    let (message, note) = match orders {
        Orders::Load => (
            format!("a load can't order by `{name}`, of type `{ty}`"),
            "a load orders by attributes of types with `<`: booleans, integers, approximate numbers and text, and types of the program's own with an order; tuples and arrays have no order, and relations are ordered only as sets, by inclusion",
        ),
        Orders::Type(t) => (
            format!("the order of `{t}` can't compare `{name}`, of type `{ty}`"),
            TYPE_ORDER,
        ),
    };
    let d = Diagnostic::error(
        Code::WrongOperandType,
        message,
        span,
        format!("{} have no total order", plural(ty)),
    )
    .with_parameter_note(ty)
    .with_note(note);
    match ty {
        hir::Type::Defined(defined) => d.with_help(format!(
            "give `{}` an order of its own, `order [asc …]` in its braces",
            defined.name
        )),
        _ => d,
    }
}

/// An attribute or a component, as `orders` says, `name`, at `span`, that
/// an order orders by again, first at `first`: the repeat, deleted with
/// `removal`, orders nothing, since what it would are equal on it.
pub(crate) fn repeated_order_item(
    name: &str,
    span: Span,
    first: Span,
    removal: Span,
    orders: Orders,
) -> Diagnostic {
    let what = match orders {
        Orders::Load => "tuples",
        Orders::Type(_) => "values",
    };
    Diagnostic::error(
        Code::DuplicateAttribute,
        format!("the order names `{name}` more than once"),
        span,
        "named again here",
    )
    .with_secondary(first, format!("`{name}` is first named here"))
    .with_note(format!(
        "an item orders only the {what} equal on the items before it, so a repeat orders nothing"
    ))
    .with_suggestion(
        "remove the repeat",
        vec![Edit {
            span: removal,
            replacement: String::new(),
        }],
    )
}

/// `order`, the order of a type, as `orders` says, which leaves out its
/// components `missing`. There is no fix: where each compares isn't
/// known.
pub(crate) fn unordered_components(
    missing: &[&str],
    order: &OrderList,
    orders: Orders,
) -> Diagnostic {
    let Orders::Type(t) = orders else {
        unreachable!("only a type's order names every component");
    };
    let quoted: Vec<String> = missing.iter().map(|name| format!("`{name}`")).collect();
    let named = trendy_diag::conjoin(&quoted);
    let example = missing[0];
    Diagnostic::error(
        Code::MissingAttribute,
        format!("the order of `{t}` leaves out {named}"),
        order.span(),
        format!("no {named}"),
    )
    .with_note(TYPE_ORDER)
    .with_help(format!(
        "add `asc {example}` or `desc {example}` where it compares, among the others"
    ))
}

/// `again`, a type's second order, after `first`: a type has one order,
/// so it is deleted.
pub(crate) fn second_order(
    t: &str,
    again: &trendy_syntax::ast::TypeOrder,
    first: &trendy_syntax::ast::TypeOrder,
) -> Diagnostic {
    Diagnostic::error(
        Code::DuplicateAttribute,
        format!("the order of `{t}` is given more than once"),
        again.span(),
        "given again here",
    )
    .with_secondary(first.span(), "first given here")
    .with_note("a type has one order, which compares its values")
    .with_suggestion(
        "delete the second order",
        vec![Edit {
            span: again.removal(),
            replacement: String::new(),
        }],
    )
}

/// What a value block is, for notes.
const BLOCK: &str = "a value block declares and assigns its own local variables, then ends with the value they compute; it changes nothing else, as a read-only operator's body doesn't";

/// A value block's local variable `name`, declared at `span`, whose name
/// means something around the block already.
pub(crate) fn block_local_hides(name: &str, span: Span) -> Diagnostic {
    Diagnostic::error(
        Code::DuplicateName,
        format!("`{name}` already names something here"),
        span,
        "a local variable with a name in use",
    )
    .with_note("a name means one thing wherever it is in scope, so a value block's local variables hide no parameter, local variable or attribute of the tuple at hand around them, though they hide the program's variables")
    .with_help(format!(
        "give it a name of its own, as in `var new_{name} := …;`"
    ))
}

/// `stmt`, a `call` in a value block, which would change something.
pub(crate) fn call_in_block(stmt: &trendy_syntax::ast::CallStmt) -> Diagnostic {
    Diagnostic::error(
        Code::ReadOnlyEffect,
        "a value block can't call an operator",
        stmt.span(),
        "a `call` in a value block",
    )
    .with_note(BLOCK)
}

/// `stmt`, an `if` statement in a value block.
pub(crate) fn if_in_block(stmt: &trendy_syntax::ast::IfStmt) -> Diagnostic {
    let at = match (stmt.keyword(), stmt.condition()) {
        (Some(keyword), Some(condition)) => Span::new(
            trendy_syntax::span(keyword.text_range()).start,
            condition.span().end,
        ),
        _ => stmt.span(),
    };
    Diagnostic::error(
        Code::Return,
        "a value block has no `if` statements",
        at,
        "an `if` statement in a value block",
    )
    .with_note(BLOCK)
    .with_help("choose between values with an `if` expression, as in `x := if c then a else b;`, whose branches can be value blocks")
}

/// `stmt`, a `return` in a value block: an early exit from the operator
/// it is in, which no operator has yet.
pub(crate) fn return_in_block(stmt: &trendy_syntax::ast::ReturnStmt) -> Diagnostic {
    let span = stmt
        .keyword()
        .map_or_else(|| stmt.span(), |t| trendy_syntax::span(t.text_range()));
    Diagnostic::error(
        Code::Return,
        "a value block can't return early",
        span,
        "an early exit",
    )
    .with_note(EARLY_EXIT)
    .with_help("end the block with its value, without `return`")
}

/// `name`, at `span`, assigned in a value block, which isn't one of its
/// local variables.
pub(crate) fn assigned_outside_block(name: &str, span: Span) -> Diagnostic {
    Diagnostic::error(
        Code::ReadOnlyEffect,
        format!("a value block can't assign `{name}`"),
        span,
        "not one of the block's local variables",
    )
    .with_note(BLOCK)
    .with_help(format!(
        "compute the new value in the block, and assign it outside: `{name} := [ … ];`"
    ))
}

/// A `for` loop's variable, `name`, at `span`, in the body of the operator
/// `op`, which `first` declares already, in a block around the loop.
pub(crate) fn loop_variable_declared_again(
    name: &str,
    span: Span,
    first: &crate::locals::Declaration,
    op: &str,
) -> Diagnostic {
    Diagnostic::error(
        Code::DuplicateName,
        format!("`{name}` is declared again in `{op}`"),
        span,
        "a loop's variable with a name in use",
    )
    .with_secondary(
        first.name_span(),
        format!("`{name}` is first declared here"),
    )
    .with_note(LOCAL_NAMES)
    .with_help(format!(
        "give the loop's variable a name of its own, as in `for each_{name} in …`"
    ))
}

/// What a `for` loop keeps from changing, for notes.
const FOR_UNCHANGED: &str = "a `for` loop goes through the elements of its array in order, with its variable each one's value, so neither the variable nor the variables its array is computed from change while it runs";

/// `name`, a `for` loop's variable, declared at `declared`, assigned at
/// `span`.
pub(crate) fn assigned_loop_variable(name: &str, span: Span, declared: Span) -> Diagnostic {
    Diagnostic::error(
        Code::NotAssignable,
        format!("`{name}`, a loop's variable, can't be assigned"),
        span,
        "assigned in its loop",
    )
    .with_secondary(
        declared,
        format!("`{name}` takes each element's value in turn"),
    )
    .with_note(FOR_UNCHANGED)
    .with_help(format!(
        "compute the new value in a local variable of its own: `var new_{name} := …;`"
    ))
}

/// `name`, a variable the array of the `for` loop written at `array` is
/// computed from, assigned at `span`, in the loop.
pub(crate) fn assigned_in_loop(name: &str, span: Span, array: Span) -> Diagnostic {
    Diagnostic::error(
        Code::NotAssignable,
        format!("`{name}` can't be assigned in a loop that goes through it"),
        span,
        "assigned in the loop",
    )
    .with_secondary(array, format!("the loop goes through this, computed from `{name}`"))
    .with_note(FOR_UNCHANGED)
    .with_help(format!(
        "assign it after the loop, or go through its positions with a `while` loop, which reads `{name}` anew each time"
    ))
}

/// A call, at `span`, in a `for` loop, of the update operator `callee`,
/// which assigns `name`, directly or through the operators it calls, a
/// variable the array of the loop, written at `array`, is computed from.
pub(crate) fn call_assigns_looped(callee: &str, name: &str, span: Span, array: Span) -> Diagnostic {
    Diagnostic::error(
        Code::NotAssignable,
        format!("`{callee}` assigns `{name}`, which this loop goes through"),
        span,
        format!("assigns `{name}`"),
    )
    .with_secondary(array, format!("the loop goes through this, computed from `{name}`"))
    .with_note(FOR_UNCHANGED)
    .with_help(format!(
        "call `{callee}` after the loop, or go through `{name}`'s positions with a `while` loop, which reads it anew each time"
    ))
}

/// The array a `for` loop goes through, `array`, of type `ty`, which isn't
/// an array's.
pub(crate) fn not_iterable(array: &Expr, ty: &hir::Type) -> Diagnostic {
    let what = match ty {
        hir::Type::Relation(_) => "a relation's tuples".to_string(),
        _ => plural(ty),
    };
    let d = Diagnostic::error(
        Code::WrongOperandType,
        format!("a `for` loop goes through an array's elements, not {what}"),
        array.span(),
        format!("this is `{ty}`"),
    )
    .with_parameter_note(ty);
    match ty {
        hir::Type::Relation(_) => d
            .with_note("a relation's tuples have no order to go through them in (TTM RM Proscription 2), and D has no operators of a tuple at a time (RM Proscription 7)")
            .with_help("load the tuples into an array first, in the order to go through them in: `load a from r order [asc x]; for t in a loop [ … ];`"),
        hir::Type::Scalar(hir::Scalar::Integer) => d.with_help(
            "to count, go through the positions with a `while` loop: `var i := 1; while i <= n loop [ …; i := i + 1; ];`",
        ),
        _ => d,
    }
}

/// A `for` loop, written at `span`, whose block, `body`, is empty: it does
/// nothing.
pub(crate) fn empty_for(span: Span, body: &trendy_syntax::ast::Body) -> Diagnostic {
    Diagnostic::error(
        Code::PointlessCondition,
        "this loop does nothing",
        body.span(),
        "its block is empty",
    )
    .with_note(FOR_UNCHANGED)
    .with_suggestion(
        "remove it",
        vec![Edit {
            span,
            replacement: String::new(),
        }],
    )
}

/// The rule for parameters' modes, for notes.
const MODE_RULE: &str = "a parameter is `const` unless written `var` or `out`: the operator only reads its argument's value; an update operator's `var` parameter takes a variable, which the operator can read and assign, an `out` one a variable it assigns before reading it, and a call marks that argument the same way: `call grow{var r: cities};`";

/// The rule for `out` parameters, for notes.
const OUT_RULE: &str = "an `out` parameter's variable is never read: the operator assigns the parameter on every path, and reads it only once it has; a parameter whose argument's value the operator reads is `var`";

/// The words of `mode`, as written.
fn mode_words(mode: &Mode) -> String {
    let words: Vec<String> = mode.words().map(|w| w.text().to_string()).collect();
    words.join(" ")
}

/// The fix that removes `mode`.
fn mode_removal(mode: &Mode) -> (String, Vec<Edit>) {
    (
        format!("remove `{}`", mode_words(mode)),
        vec![Edit {
            span: mode.removal(),
            replacement: String::new(),
        }],
    )
}

/// `mode`, `var` or `out`, before the name `ident` of the parameter `name`
/// of a read-only operator.
pub(crate) fn updated_in_read_only(name: &str, mode: &Mode, ident: &SyntaxToken) -> Diagnostic {
    let (help, edits) = mode_removal(mode);
    Diagnostic::error(
        Code::MisplacedMode,
        format!(
            "a read-only operator's parameter `{name}` can't be `{}`",
            mode_words(mode)
        ),
        mode.span(),
        "a read-only operator changes nothing but its own local variables",
    )
    .with_secondary(trendy_syntax::span(ident.text_range()), "this parameter")
    .with_note(MODE_RULE)
    .with_note("an operator that changes a variable it is passed is an update operator, without a result type, which `call` invokes")
    .with_suggestion(help, edits)
}

/// `mode`, `var` or `out`, before the name `ident` of the `per` parameter
/// `name`, or, with `per`, of the relation that parameter is evaluated for
/// each tuple of.
pub(crate) fn updated_with_per(
    name: &str,
    per: Option<&str>,
    mode: &Mode,
    ident: &SyntaxToken,
) -> Diagnostic {
    let (help, edits) = mode_removal(mode);
    let words = mode_words(mode);
    let (message, label) = match per {
        Some(per) => (
            format!("`{name}`, which `{per}` is evaluated for each tuple of, can't be `{words}`"),
            "its tuples carry the values of its `per` parameters",
        ),
        None => (
            format!("the `per` parameter `{name}` can't be `{words}`"),
            "its argument is a value for each tuple of its relation",
        ),
    };
    Diagnostic::error(Code::MisplacedMode, message, mode.span(), label)
        .with_secondary(trendy_syntax::span(ident.text_range()), "this parameter")
        .with_note(MODE_RULE)
        .with_note(PER_RULE)
        .with_suggestion(help, edits)
}

/// `mode`, in a heading that isn't an operator's parameters.
pub(crate) fn mode_outside_parameters(mode: &Mode) -> Diagnostic {
    let (help, edits) = mode_removal(mode);
    Diagnostic::error(
        Code::MisplacedMode,
        "only an operator's parameters have a mode",
        mode.span(),
        "an attribute has none",
    )
    .with_note(MODE_RULE)
    .with_suggestion(help, edits)
}

/// The argument `arg`, for the parameter `name` of `signature`, whose
/// mode is `mode`, written with `written`, a different one, or none.
pub(crate) fn argument_mode(
    arg: &AttributeValue,
    name: &str,
    mode: hir::Mode,
    written: Option<&Mode>,
    signature: &Signature,
) -> Diagnostic {
    let span = written.map_or_else(|| arg.span(), AstNode::span);
    let word = mode.word();
    let does = if mode == hir::Mode::Out {
        "assigns"
    } else {
        "updates"
    };
    let (message, label, help, edits) = match (mode, written) {
        (hir::Mode::Const, written) => {
            let written = written.expect("a `const` parameter's argument may be marked `const`");
            let words = mode_words(written);
            let (help, edits) = mode_removal(written);
            let a = if words.starts_with(['a', 'e', 'i', 'o', 'u']) {
                "an"
            } else {
                "a"
            };
            (
                format!(
                    "`{name}` isn't {a} `{words}` parameter, so its argument isn't marked `{words}`"
                ),
                "the operator only reads its value".to_string(),
                help,
                edits,
            )
        }
        (_, None) => {
            let start = arg.span().start;
            (
                format!(
                    "the argument for `{word} {name}` is marked `{word}`, as the variable the call {does}"
                ),
                format!("not marked `{word}`"),
                format!("mark it `{word}`"),
                vec![Edit {
                    span: Span::new(start, start),
                    replacement: format!("{word} "),
                }],
            )
        }
        (_, Some(written)) => {
            let words = mode_words(written);
            (
                format!("the argument for `{word} {name}` is marked `{word}`, not `{words}`"),
                format!("marked `{words}`"),
                format!("mark it `{word}`"),
                vec![Edit {
                    span: written.span(),
                    replacement: word.to_string(),
                }],
            )
        }
    };
    let d = Diagnostic::error(Code::ArgumentMode, message, span, label)
        .with_note(format!("the parameters are {}", signature.text()))
        .with_note(MODE_RULE);
    let d = match signature.param_decl(name) {
        Some(decl) => d.with_secondary(decl.span(), "the parameter"),
        None => d,
    };
    d.with_suggestion(help, edits)
}

/// The argument `written`, for the parameter `name`, whose mode is `mode`,
/// `var` or `out`, which isn't a variable.
pub(crate) fn argument_not_a_variable(name: &str, mode: hir::Mode, written: &Expr) -> Diagnostic {
    let word = mode.word();
    let does = if mode == hir::Mode::Out {
        "assigns"
    } else {
        "updates"
    };
    Diagnostic::error(
        Code::NotAssignable,
        format!("the argument for `{word} {name}` is a variable, which the call {does}"),
        written.span(),
        "not a variable",
    )
    .with_note(MODE_RULE)
    .with_help("declare a variable with this value, and pass the variable: `var v := …;`")
}

/// The variable `name`, at `span`, passed both to `first` and to `second`,
/// each a parameter written with its mode, as `var a`, of one call.
pub(crate) fn passed_twice(
    name: &str,
    first: &str,
    second: &str,
    span: Span,
    earlier: Span,
) -> Diagnostic {
    Diagnostic::error(
        Code::Aliased,
        format!("`{name}` is passed to both `{first}` and `{second}`"),
        span,
        "passed again",
    )
    .with_secondary(earlier, "passed here first")
    .with_note("each `var` or `out` parameter's value is its variable's when the operator ends, so one variable can't be two of them")
    .with_help("pass a different variable to each")
}

/// The variable `name`, at `span`, passed to `param`, a parameter written
/// with its mode, as `var n`, of `callee`, which reads the variable
/// itself, or, if `assigns`, assigns it, in its body or in those of the
/// operators it calls.
pub(crate) fn passed_and_used(
    name: &str,
    param: &str,
    callee: &str,
    assigns: bool,
    span: Span,
) -> Diagnostic {
    let uses = if assigns { "assigns" } else { "reads" };
    let parameter = param.split_once(' ').map_or(param, |(_, p)| p);
    Diagnostic::error(
        Code::Aliased,
        format!("`{name}` is passed to `{param}` of `{callee}`, which {uses} `{name}` itself"),
        span,
        "passed here",
    )
    .with_note(format!("`{callee}` {uses} `{name}` in its body or in an operator it calls, where it would be a second name for what `{parameter}` is"))
    .with_note(MODE_RULE)
    .with_help(format!("pass `{callee}` another variable, or have it use `{parameter}` for `{name}`"))
}

/// The `out` parameter `name`, read at `span` before it is assigned on
/// every path to there. `decl` is where it is declared, in the program's
/// own operators, whose mode the fix makes `var`.
pub(crate) fn read_before_assigned(
    name: &str,
    span: Span,
    decl: Option<&ast::AttributeType>,
) -> Diagnostic {
    let d = Diagnostic::error(
        Code::UnassignedOut,
        format!("the `out` parameter `{name}` is read before it is assigned"),
        span,
        "read here",
    )
    .with_note(OUT_RULE);
    let Some((decl, mode)) = decl.and_then(|d| Some((d, d.mode()?))) else {
        return d;
    };
    d.with_secondary(decl.span(), "declared `out`")
        .with_suggestion(
            "make it `var`, to read the value it is passed",
            vec![Edit {
                span: mode.span(),
                replacement: "var".to_string(),
            }],
        )
}

/// The `out` parameter `name` of `operator`, whose name `ident` is where it
/// is declared, which isn't assigned on every path through the operator.
pub(crate) fn unassigned_out(name: &str, operator: &str, ident: &SyntaxToken) -> Diagnostic {
    Diagnostic::error(
        Code::UnassignedOut,
        format!("the `out` parameter `{name}` isn't assigned on every path through `{operator}`"),
        trendy_syntax::span(ident.text_range()),
        "not always assigned",
    )
    .with_note(OUT_RULE)
    .with_note("an `if` without `else`, a `while` loop and a `for` loop may not run their statements, so what only they assign isn't assigned on every path")
    .with_help(format!("assign `{name}` before `{operator}` ends, whichever way it goes, or make it `var` if `{operator}` may leave its variable as it is"))
}

/// The rule for what `load_csv` reads.
const CSV_FIELDS: &str = "`load_csv` reads each attribute of its array's tuples from the field in the column `columns` gives it: text as it is, and an integer, an approximate number or a boolean as `parse_integer`, `parse_approximate` and `parse_boolean` read them; a value of a type of the program's own is selected from its components, each read from a column of its own";

/// The rule for `load_csv`'s columns.
const CSV_COLUMNS: &str = "`columns` gives each attribute of the tuples the position of the column it is read from, counting from 1, and each a column of its own";

/// The array written `rows`, which `load_csv` loads, whose tuples have
/// the attributes, or values with the components, `unreadable`, by name
/// and type, which it can't read from a field.
pub(crate) fn unreadable_attributes(
    unreadable: &[(String, &hir::Type)],
    rows: &Expr,
) -> Diagnostic {
    let named: Vec<String> = unreadable
        .iter()
        .map(|(name, ty)| format!("`{name}`, of type `{ty}`"))
        .collect();
    Diagnostic::error(
        Code::UnreadableAttribute,
        format!("`load_csv` can't read {}, from a field", trendy_diag::conjoin(&named)),
        rows.span(),
        "loaded with `load_csv`",
    )
    .with_note(CSV_FIELDS)
    .with_help("load the fields of the columns as text, integers, approximate numbers or booleans, and compute the other attributes from them")
}

/// The array written `rows`, which `load_csv` loads, whose tuples' type is
/// `parameter`, a type parameter of the operator the call is in, so their
/// attributes, which name the columns to read, aren't known here.
pub(crate) fn unknown_columns(parameter: &hir::TypeParameter, rows: &Expr) -> Diagnostic {
    Diagnostic::error(
        Code::UnreadableAttribute,
        format!("`load_csv` can't read tuples of `{parameter}`, whose attributes aren't known here"),
        rows.span(),
        "loaded with `load_csv`",
    )
    .with_note(CSV_FIELDS)
    .with_note(format!(
        "`{parameter}` is `{}`, whose attributes each call gives it, but `load_csv` needs them when compiling, to know the types to read their columns as",
        parameter.any
    ))
    .with_help("call `load_csv` where the array's type is written, and pass the array to this operator")
}

/// `attribute`, whose column `load_csv` is given at `at` as `position`,
/// which no column has, as they count from 1.
pub(crate) fn not_a_column(attribute: &str, position: i64, at: Span) -> Diagnostic {
    Diagnostic::error(
        Code::CsvColumns,
        format!("`{attribute}` is read from column {position}, but columns count from 1"),
        at,
        "no column has this position",
    )
    .with_note(CSV_COLUMNS)
}

/// `second`, whose column `load_csv` is given at `at` as `position`, which
/// `first`'s is too, at `first_at`.
pub(crate) fn shared_column(
    first: &str,
    second: &str,
    position: i64,
    first_at: Span,
    at: Span,
) -> Diagnostic {
    Diagnostic::error(
        Code::CsvColumns,
        format!("`{first}` and `{second}` are both read from column {position}"),
        at,
        format!("`{first}`'s column too"),
    )
    .with_secondary(
        first_at,
        format!("`{first}` is read from column {position}"),
    )
    .with_note(CSV_COLUMNS)
    .with_help("give each attribute the column it is in")
}

/// The rule for operators of one name.
const OVERLOADS: &str = "operators of one name are told apart by their parameters' names, which a call names its arguments with, so it calls the one whose parameters they name";

/// A call of `name`, with the arguments `written`, whose names name the
/// parameters of none of its operators, and of `tied` equally nearly.
pub(crate) fn unchosen_operator(name: &str, tied: &[&Signature], written: Written) -> Diagnostic {
    let span = match written {
        Written::Arguments(args) => args.span(),
        Written::Nothing(list) => list.span(),
    };
    let texts: Vec<String> = tied.iter().map(|s| s.text()).collect();
    Diagnostic::error(
        Code::UnchosenOperator,
        format!("these arguments' names choose no operator `{name}`"),
        span,
        format!("not the parameters of any `{name}`"),
    )
    .with_note(OVERLOADS)
    .with_help(format!(
        "name the arguments as one of these has its parameters: {}",
        trendy_diag::disjoin(&texts)
    ))
}
