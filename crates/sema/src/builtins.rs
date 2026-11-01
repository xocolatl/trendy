//! The prelude's built-ins: its declarations of the kinds, types and
//! operators the compiler implements, checked against it.

use trendy_hir as hir;
use trendy_syntax::ast::{BuiltinOperator, BuiltinType, Item, KindDecl, SourceFile};

use std::collections::BTreeSet;

use trendy_diag::{Code, Diagnostic, Span};
use trendy_syntax::SyntaxToken;
use trendy_syntax::ast::AstNode;

use crate::Namespaces;
use crate::operators::{self, Operators, Signature};
use crate::types::{self, TypeParameters};

/// An internal error, `message`, at the name `ident` in the prelude.
fn internal(message: String, ident: &SyntaxToken) -> Diagnostic {
    Diagnostic::error(
        Code::Internal,
        message,
        ident_span(ident),
        "not as the compiler implements it",
    )
}

fn ident_span(ident: &SyntaxToken) -> Span {
    trendy_syntax::span(ident.text_range())
}

/// The type parameters `types` that have names, as `types { … }` declares
/// them after the parameters, with a space before it, or nothing if none
/// has one.
fn declared_text(types: &[hir::TypeParameter]) -> String {
    let declared: Vec<String> = types
        .iter()
        .filter_map(|t| Some(format!("{}: {}", t.name.as_ref()?, t.any)))
        .collect();
    if declared.is_empty() {
        String::new()
    } else {
        format!(" types {{ {} }}", declared.join(", "))
    }
}

/// The built-in named `name` whose parameters are named `params`, as a
/// call names their arguments, or else the first named `name`, whose
/// parameters the declaration's then differ from.
fn implemented(name: &str, params: &BTreeSet<String>) -> Option<hir::Builtin> {
    let named = || hir::Builtin::ALL.into_iter().filter(|b| b.name() == name);
    named()
        .find(|b| operators::written(&b.params()).keys().eq(params.iter()))
        .or_else(|| named().next())
}

/// The signature of the built-in operator `decl` declares, whose id is
/// `id`, with the parameters the compiler implements, or `None` after
/// reporting why it isn't one. It is added to those `declared`, unless it
/// already is.
fn builtin_operator(
    decl: &BuiltinOperator,
    id: hir::OperatorId,
    namespaces: &Namespaces,
    declared: &mut Vec<hir::Builtin>,
    errors: &mut Vec<Diagnostic>,
) -> Option<Signature> {
    // A missing name is a syntax error, already reported.
    let ident = decl.name()?.ident()?;
    let name = ident.text();
    let written: BTreeSet<String> = decl
        .params()
        .map(|heading| {
            heading
                .attributes()
                .filter_map(|a| Some(a.name()?.ident()?.text().to_string()))
                .collect()
        })
        .unwrap_or_default();
    let Some(builtin) = implemented(name, &written) else {
        errors.push(internal(
            format!("`{name}` is not a built-in the compiler implements"),
            &ident,
        ));
        return None;
    };
    if declared.contains(&builtin) {
        errors.push(internal(
            format!("`{name}` is declared more than once"),
            &ident,
        ));
        return None;
    }
    declared.push(builtin);
    let (mut types, declared_valid) = decl.type_parameters().map_or_else(
        || (Vec::new(), true),
        |d| operators::declared_types(&d, namespaces, errors),
    );
    let heading = decl.params()?;
    let params = operators::params(&heading, &mut types, namespaces, errors);
    if !declared_valid || !params.unresolved.is_empty() {
        return None;
    }
    let params = params.seen;
    let written = decl.result();
    let result = match &written {
        Some(written) => {
            let mut parameters = TypeParameters::Fixed(&types);
            Some(types::resolve(
                written,
                &mut parameters,
                namespaces,
                errors,
            )?)
        }
        None => None,
    };
    // The type parameters first, which the result type can name.
    let implemented = builtin.types();
    if types != implemented || params != builtin.params() {
        // At `types { … }`, if those it declares are what differ.
        let named = |list: &[hir::TypeParameter]| -> Vec<hir::TypeParameter> {
            list.iter().filter(|t| t.name.is_some()).cloned().collect()
        };
        let span = match decl.type_parameters() {
            Some(declared) if named(&types) != named(&implemented) => declared.span(),
            _ => heading.span(),
        };
        errors.push(Diagnostic::error(
            Code::Internal,
            format!(
                "`{name}` is declared with parameters that differ from its implementation's, `{}{}`",
                operators::params_text(&operators::written(&builtin.params())),
                declared_text(&implemented)
            ),
            span,
            "not the parameters the compiler implements",
        ));
        return None;
    }
    if result != builtin.result() {
        let span = written.map_or_else(|| ident_span(&ident), |w| w.span());
        let expected = builtin
            .result()
            .map_or_else(|| "none".to_string(), |r| format!("`{r}`"));
        errors.push(Diagnostic::error(
            Code::Internal,
            format!("`{name}` is declared with a result type that differs from its implementation's, {expected}"),
            span,
            "not the result type the compiler implements",
        ));
        return None;
    }
    Some(Signature::builtin(id, builtin, types, params))
}

/// The kind `decl` declares, with what the compiler can require of its
/// types, or `None` after reporting why it isn't one: `kinds` are those
/// declared before it.
fn kind_decl(
    decl: &KindDecl,
    kinds: &[(hir::Kind, BTreeSet<hir::Scalar>)],
    errors: &mut Vec<Diagnostic>,
) -> Option<hir::Kind> {
    // A missing name is a syntax error, already reported.
    let ident = decl.name()?.ident()?;
    let name = ident.text();
    if kinds.iter().any(|(kind, _)| kind.name == name) {
        errors.push(internal(
            format!("`{name}` is declared more than once"),
            &ident,
        ));
        return None;
    }
    // `any type` is every type, of no kind.
    if name == "type" {
        errors.push(internal(
            "a kind can't be called `type`".to_string(),
            &ident,
        ));
        return None;
    }
    let mut requires = BTreeSet::new();
    for requirement in decl.requirements() {
        let Some(token) = requirement.token() else {
            continue;
        };
        let requirement = match token.kind() {
            trendy_syntax::SyntaxKind::Lt => hir::Requirement::Order,
            trendy_syntax::SyntaxKind::Plus => hir::Requirement::Addition,
            trendy_syntax::SyntaxKind::Star => hir::Requirement::Multiplication,
            _ => {
                errors.push(internal(
                    format!(
                        "`{}` is not a requirement the compiler implements",
                        token.text()
                    ),
                    &token,
                ));
                continue;
            }
        };
        requires.insert(requirement);
    }
    Some(hir::Kind {
        name: name.to_string(),
        requires,
    })
}

/// Add the built-in type `decl` declares to `types`, and to each of the
/// `kinds` it declares, or report why it isn't one.
fn builtin_type(
    decl: &BuiltinType,
    types: &mut Vec<String>,
    kinds: &mut [(hir::Kind, BTreeSet<hir::Scalar>)],
    errors: &mut Vec<Diagnostic>,
) {
    // A missing name is a syntax error, already reported.
    let Some(ident) = decl.name().and_then(|n| n.ident()) else {
        return;
    };
    let name = ident.text().to_string();
    let Some(scalar) = hir::Scalar::from_name(&name) else {
        errors.push(internal(
            format!("`{name}` is not a type the compiler implements"),
            &ident,
        ));
        return;
    };
    if types.contains(&name) {
        errors.push(internal(
            format!("`{name}` is declared more than once"),
            &ident,
        ));
        return;
    }
    types.push(name.clone());
    let declared = decl.kinds();
    for kind_ident in declared
        .iter()
        .flat_map(trendy_syntax::ast::NameList::names)
        .filter_map(|n| n.ident())
    {
        let Some((kind, members)) = kinds
            .iter_mut()
            .find(|(kind, _)| kind.name == kind_ident.text())
        else {
            errors.push(internal(
                format!("`{}` is not a kind the prelude declares", kind_ident.text()),
                &kind_ident,
            ));
            continue;
        };
        let missing = kind.requires.iter().find(|r| !scalar.has(**r));
        if let Some(missing) = missing {
            errors.push(internal(
                format!(
                    "`{name}` doesn't have `{}`, which `{}` requires",
                    missing.text(),
                    kind.name
                ),
                &kind_ident,
            ));
            continue;
        }
        members.insert(scalar);
    }
}

/// The types the compiler implements: the scalar types.
fn builtin_types() -> impl Iterator<Item = &'static str> {
    hir::Scalar::ALL.into_iter().map(hir::Scalar::name)
}

/// The kinds and built-in types `prelude` declares, checking that the
/// types are exactly those the compiler implements: each kind, with the
/// built-in types of it. The prelude is part of the compiler, so a
/// declaration that doesn't match it is an internal error, at the
/// declaration, as is a variable or a heading, which the prelude doesn't
/// declare. Its built-in operators come once the types they can name are
/// resolved ([`operators`]).
pub(crate) fn kinds(
    prelude: &SourceFile,
    errors: &mut Vec<Diagnostic>,
) -> Vec<(hir::Kind, BTreeSet<hir::Scalar>)> {
    // The kinds first, which types declare and built-ins can name.
    let mut kinds: Vec<(hir::Kind, BTreeSet<hir::Scalar>)> = Vec::new();
    for item in prelude.items() {
        if let Item::Kind(decl) = item
            && let Some(kind) = kind_decl(&decl, &kinds, errors)
        {
            kinds.push((kind, BTreeSet::new()));
        }
    }
    let mut types: Vec<String> = Vec::new();
    for item in prelude.items() {
        match item {
            Item::BuiltinType(decl) => builtin_type(&decl, &mut types, &mut kinds, errors),
            Item::Variable(decl) => errors.push(Diagnostic::error(
                Code::Internal,
                "the prelude declares no variables",
                decl.span(),
                "a variable",
            )),
            Item::Builtin(_)
            | Item::Kind(_)
            | Item::Operator(_)
            | Item::Type(_)
            | Item::Heading(_) => {}
        }
    }
    // What is missing goes at the end.
    let end = Span::empty(prelude.span().end);
    for t in builtin_types().filter(|t| !types.iter().any(|d| d == t)) {
        errors.push(Diagnostic::error(
            Code::Internal,
            format!("the prelude doesn't declare the type `{t}`"),
            end,
            "missing",
        ));
    }
    kinds
}

/// Add to `list` the signature of each built-in operator `prelude`
/// declares, numbered after those it has, checking that they are exactly
/// those the compiler implements, each with the right parameters, of types
/// `namespaces` resolves, the prelude's own among them. A declaration that
/// doesn't match the compiler is an internal error, as is one missing.
pub(crate) fn operators(
    prelude: &SourceFile,
    namespaces: &Namespaces,
    list: &mut Vec<Signature>,
    errors: &mut Vec<Diagnostic>,
) {
    // The built-ins declared, rightly or not, so that a wrong
    // declaration isn't missing too.
    let mut declared: Vec<hir::Builtin> = Vec::new();
    for item in prelude.items() {
        if let Item::Builtin(decl) = item {
            let id = hir::OperatorId(list.len());
            list.extend(builtin_operator(
                &decl,
                id,
                namespaces,
                &mut declared,
                errors,
            ));
        }
    }
    let end = Span::empty(prelude.span().end);
    for b in hir::Builtin::ALL
        .into_iter()
        .filter(|b| !declared.contains(b))
    {
        let params = operators::params_text(&operators::written(&b.params()));
        errors.push(Diagnostic::error(
            Code::Internal,
            format!("the prelude doesn't declare `{}{params}`", b.name()),
            end,
            "missing",
        ));
    }
}

/// Report the prelude's `error` and `message` if it doesn't declare them as
/// the compiler needs them, at its end: a program fails with a value of the
/// union type `error`, writing its message, which the specification
/// `message{error: error}: text` gives. So too the errors built-ins give
/// (`hir::LanguageError`), which the runtime makes, and the heading
/// `transaction_collection`, what a transaction collects
/// (`hir::transaction_collection`), whose tuples lowering makes.
pub(crate) fn failures(
    prelude: &SourceFile,
    namespaces: &Namespaces,
    operators: &Operators,
    errors: &mut Vec<Diagnostic>,
) {
    let end = Span::empty(prelude.span().end);
    if namespaces.dummy("error").is_none() {
        errors.push(Diagnostic::error(
            Code::Internal,
            "the prelude doesn't declare `type error union;`, what a program fails with",
            end,
            "missing",
        ));
    } else if operators.message().is_none() {
        errors.push(Diagnostic::error(
            Code::Internal,
            "the prelude doesn't declare `operator message{error: error}: text;`, which writes an error's message",
            end,
            "missing",
        ));
    }
    // What a transaction collects, whose tuples the compiler makes.
    if let Some(error) = namespaces.dummy("error") {
        let collection = hir::transaction_collection(hir::Type::Dummy(Box::new(error.clone())));
        let declared = namespaces
            .heading("transaction_collection")
            .and_then(|(_, attributes)| attributes);
        if declared != Some(&collection) {
            errors.push(Diagnostic::error(
                Code::Internal,
                "the prelude doesn't declare `heading transaction_collection := { error: error, failed: boolean };`, what a transaction collects",
                end,
                "missing",
            ));
        }
    }
    // The errors the built-ins give, each a subtype of `error` with the
    // components the runtime makes it with.
    for error in hir::LanguageError::ALL {
        let declared = namespaces.defined(error.name()).and_then(|(_, d)| d);
        let components = error.components();
        let as_made = declared.is_some_and(|defined| {
            defined.supertypes == ["error"]
                && defined.specialization.is_none()
                && defined.components == components
        });
        if !as_made {
            errors.push(Diagnostic::error(
                Code::Internal,
                format!(
                    "the prelude doesn't declare `type {} is error {{ {{ {} }} }};`, an error a built-in gives",
                    error.name(),
                    components
                        .attributes()
                        .map(|(name, ty)| format!("{name}: {ty}"))
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
                end,
                "missing",
            ));
        }
    }
}
