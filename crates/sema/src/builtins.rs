//! The built-ins: the declarations of the kinds, types and operators the
//! compiler implements, the prelude's and those of the other units that
//! come with it, checked against it.

use trendy_hir as hir;
use trendy_syntax::ast::{self, BuiltinOperator, BuiltinType, Item, KindDecl, SourceFile};

use std::collections::{BTreeMap, BTreeSet};

use trendy_diag::{Code, Diagnostic, Span};
use trendy_syntax::SyntaxToken;
use trendy_syntax::ast::AstNode;

use crate::operators::{self, Operators, Signature};
use crate::types::{self, TypeParameters};
use crate::{Files, Namespaces, Source};

/// An internal error, `message`, at the name `ident` in a unit that comes
/// with the compiler.
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
/// parameters the declaration's then differ from: of the types of the
/// `library` they name.
fn implemented(
    name: &str,
    params: &BTreeSet<String>,
    library: &hir::Library,
) -> Option<hir::Builtin> {
    let named = || hir::Builtin::ALL.into_iter().filter(|b| b.name() == name);
    named()
        .find(|b| {
            operators::written(&b.params(library))
                .keys()
                .eq(params.iter())
        })
        .or_else(|| named().next())
}

/// The signature of the built-in operator `decl` declares, in a unit that
/// comes with the compiler, checked as `source`, whose id is `id`, with the
/// parameters the compiler implements, or `None` after reporting why it
/// isn't one. It is added to those `declared`, unless it already is.
fn builtin_operator(
    decl: &BuiltinOperator,
    source: Source,
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
    let library = namespaces.library();
    let Some(builtin) = implemented(name, &written, library) else {
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
    if types != implemented || params != builtin.params(library) {
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
                operators::params_text(&operators::written(&builtin.params(library))),
                declared_text(&implemented)
            ),
            span,
            "not the parameters the compiler implements",
        ));
        return None;
    }
    if result != builtin.result(library) {
        let span = written.map_or_else(|| ident_span(&ident), |w| w.span());
        let expected = builtin
            .result(library)
            .map_or_else(|| "none".to_string(), |r| format!("`{r}`"));
        errors.push(Diagnostic::error(
            Code::Internal,
            format!("`{name}` is declared with a result type that differs from its implementation's, {expected}"),
            span,
            "not the result type the compiler implements",
        ));
        return None;
    }
    Some(Signature::builtin(
        id,
        (builtin, ident_span(&ident)),
        library,
        namespaces.unit_at(ident_span(&ident)),
        ast::section_of(decl.syntax()),
        source,
    ))
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
            Item::Constraint(decl) => errors.push(Diagnostic::error(
                Code::Internal,
                "the prelude declares no constraints",
                trendy_syntax::span(decl.syntax().text_range()),
                "a constraint",
            )),
            Item::Database(decl) => errors.push(Diagnostic::error(
                Code::Internal,
                "the prelude declares no databases",
                trendy_syntax::span(decl.syntax().text_range()),
                "a database",
            )),
            Item::Relvar(decl) => errors.push(Diagnostic::error(
                Code::Internal,
                "the prelude declares no databases' relvars",
                decl.span(),
                "a relvar",
            )),
            // A constant is read as a unit's is.
            Item::Builtin(_)
            | Item::Constant(_)
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

/// Give `namespaces` the types of the units that come with the compiler
/// that built-ins' signatures name or literals make, from `files`, as they
/// declare them, once the types are resolved: the prelude's `code_point` and
/// `byte`, whose values the compiler makes with the components
/// [`hir::Library::number_components`]. See [`made_by_the_compiler`].
pub(crate) fn library(files: &Files, namespaces: &mut Namespaces, errors: &mut Vec<Diagnostic>) {
    let code_point = made_by_the_compiler(hir::Library::CODE_POINT, files, namespaces, errors);
    let byte = made_by_the_compiler(hir::Library::BYTE, files, namespaces, errors);
    // A missing `error` is reported with what a failure needs
    // (`failures`), and one of its name stands for it.
    let error = namespaces.prelude_identity(hir::Library::ERROR);
    let error = namespaces
        .dummy_named(&error)
        .cloned()
        .unwrap_or(hir::Dummy {
            name: error,
            supertypes: Vec::new(),
        });
    namespaces.library = Some(hir::Library {
        code_point: Box::new(code_point),
        byte: Box::new(byte),
        error: Box::new(error),
    });
}

/// The prelude's type `name`, whose values the compiler makes with the
/// components [`hir::Library::number_components`], as a type of its own, no
/// subtype. One that isn't is an internal error, at the prelude's end, and so
/// is one missing, which a type of just those components stands for, so that the
/// built-ins are checked against what they name, and nothing else is reported for
/// it.
fn made_by_the_compiler(
    name: &str,
    files: &Files,
    namespaces: &Namespaces,
    errors: &mut Vec<Diagnostic>,
) -> hir::Defined {
    let identity = namespaces.prelude_identity(name);
    let components = hir::Library::number_components();
    let declared = namespaces.defined_named(&identity).cloned();
    let as_made = declared.as_ref().is_some_and(|defined| {
        defined.components == components
            && defined.supertypes.is_empty()
            && defined.specialization.is_none()
    });
    if !as_made {
        errors.push(Diagnostic::error(
            Code::Internal,
            format!(
                "the prelude doesn't declare `type {name} {{ {{ value: integer }}, … }};`, of no supertype, as the compiler makes its values"
            ),
            Span::empty(files.prelude().span().end),
            "missing",
        ));
    }
    declared.unwrap_or_else(|| hir::Defined {
        name: identity,
        unit: trendy_prelude::PRELUDE.name.to_string(),
        components,
        representations: Vec::new(),
        constraints: Vec::new(),
        keys: BTreeMap::new(),
        order: None,
        supertypes: Vec::new(),
        specialized: false,
        specialization: None,
    })
}

/// Add to `list` the signature of each built-in operator the units that
/// come with the compiler declare, in `files`, numbered after those it
/// has, checking that they are exactly those the compiler implements, each
/// declared once, with the right parameters, of types `namespaces`
/// resolves, the prelude's own among them. A declaration that doesn't
/// match the compiler is an internal error, as is one missing, at the
/// prelude's end.
pub(crate) fn operators(
    files: &Files,
    namespaces: &Namespaces,
    list: &mut Vec<Signature>,
    errors: &mut Vec<Diagnostic>,
) {
    // The built-ins declared, rightly or not, so that a wrong
    // declaration isn't missing too.
    let mut declared: Vec<hir::Builtin> = Vec::new();
    for (source, item) in files.items().filter(|(source, _)| source.is_compilers()) {
        if let Item::Builtin(decl) = item {
            let id = hir::OperatorId(list.len());
            list.extend(builtin_operator(
                &decl,
                source,
                id,
                namespaces,
                &mut declared,
                errors,
            ));
        }
    }
    let end = Span::empty(files.prelude().span().end);
    for b in hir::Builtin::ALL
        .into_iter()
        .filter(|b| !declared.contains(b))
    {
        let params = operators::params_text(&operators::written(&b.params(namespaces.library())));
        errors.push(Diagnostic::error(
            Code::Internal,
            format!(
                "no unit that comes with the compiler declares `{}{params}`",
                b.name()
            ),
            end,
            "missing",
        ));
    }
}

/// What the runtime gives the variable `decl`, a `builtin var`, named `ident`,
/// of type `ty`, as the program starts, if it is a unit that comes with the
/// compiler's, or `None`: one elsewhere is reported with the program's
/// operators (see [`crate::operators::signatures`]). One of a name, unit or
/// type the compiler gives none of, or with a value of its own, is an
/// internal error.
pub(crate) fn given(
    decl: &ast::VarDecl,
    ident: &SyntaxToken,
    ty: &hir::Type,
    namespaces: &Namespaces,
    errors: &mut Vec<Diagnostic>,
) -> Option<hir::Given> {
    let unit = namespaces
        .units()
        .name(namespaces.unit_at(ident_span(ident)));
    if !unit.starts_with("trendy.") {
        return None;
    }
    let name = ident.text();
    let Some(given) = hir::Given::ALL
        .into_iter()
        .find(|g| g.name() == name && unit == hir::Given::UNIT)
    else {
        errors.push(internal(
            format!("the runtime gives no variable `{name}` of `{unit}`"),
            ident,
        ));
        return None;
    };
    if decl.initializer().is_some() {
        errors.push(internal(
            format!("`{name}` has the value the runtime gives, and none of its own"),
            ident,
        ));
        return None;
    }
    if *ty != given.ty() {
        errors.push(internal(
            format!("`{name}` is a `{}` as the runtime gives it", given.ty()),
            ident,
        ));
        return None;
    }
    Some(given)
}

/// Report each variable the runtime gives that no unit that comes with the
/// compiler, of `files`, declares, an internal error, at the prelude's end.
pub(crate) fn given_declared(files: &Files, errors: &mut Vec<Diagnostic>) {
    let declared: BTreeSet<String> = files
        .items()
        .filter(|(source, _)| source.is_compilers())
        .filter_map(|(_, item)| match item {
            Item::Variable(decl) if decl.builtin().is_some() => {
                Some(decl.name()?.ident()?.text().to_string())
            }
            _ => None,
        })
        .collect();
    let end = Span::empty(files.prelude().span().end);
    for given in hir::Given::ALL {
        if !declared.contains(given.name()) {
            errors.push(Diagnostic::error(
                Code::Internal,
                format!(
                    "no unit that comes with the compiler declares `builtin var {}: {};`",
                    given.name(),
                    given.ty()
                ),
                end,
                "missing",
            ));
        }
    }
}

/// The identity of each error the language makes, the type a unit that
/// comes with the compiler, in `files`, declares for it, reporting the
/// prelude's `error` and `message` if it doesn't declare them as the
/// compiler needs them, at its end: a program fails with a value of the
/// union type `error`, writing its message, which the specification
/// `message{error: error}: text` gives. So too the errors built-ins give
/// (`hir::LanguageError`), which the runtime makes, each a subtype of
/// `error` that one of those units declares, and the heading
/// `transaction_collection`, what a transaction collects
/// (`hir::transaction_collection`), whose tuples lowering makes.
pub(crate) fn failures(
    files: &Files,
    namespaces: &Namespaces,
    operators: &Operators,
    errors: &mut Vec<Diagnostic>,
) -> BTreeMap<hir::LanguageError, String> {
    let end = Span::empty(files.prelude().span().end);
    let union = &namespaces.library().error;
    if namespaces.dummy_named(&union.name).is_none() {
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
    if namespaces.dummy_named(&union.name).is_some() {
        let collection = hir::transaction_collection(namespaces.library().error_type());
        let identity = namespaces.prelude_identity(hir::Library::TRANSACTION_COLLECTION);
        let declared = namespaces.heading_attributes(&identity);
        if declared != Some(&collection) {
            errors.push(Diagnostic::error(
                Code::Internal,
                "the prelude doesn't declare `heading transaction_collection := { error: error, failed: boolean };`, what a transaction collects",
                end,
                "missing",
            ));
        }
    }
    // The types the units that come with the compiler declare, by name,
    // each with its identity.
    let mut declared: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (source, item) in files.items() {
        if let (true, Item::Type(decl)) = (source.is_compilers(), item)
            && let Some(ident) = decl.name().and_then(|n| n.ident())
        {
            let unit = namespaces.unit_at(ident_span(&ident));
            declared
                .entry(ident.text().to_string())
                .or_default()
                .push(namespaces.type_identity(ident.text(), unit));
        }
    }
    // The errors the built-ins give, each a subtype of `error` with the
    // components the runtime makes it with.
    let mut identities = BTreeMap::new();
    for error in hir::LanguageError::ALL {
        let components = error.components();
        let as_made = match declared.get(error.name()).map(Vec::as_slice) {
            Some([identity]) => namespaces
                .defined_named(identity)
                .filter(|defined| {
                    defined.supertypes == [union.name.clone()]
                        && defined.specialization.is_none()
                        && defined.components == components
                })
                .map(|_| identity.clone()),
            _ => None,
        };
        match as_made {
            Some(identity) => {
                identities.insert(error, identity);
            }
            None => errors.push(Diagnostic::error(
                Code::Internal,
                format!(
                    "no unit that comes with the compiler declares `type {} is error {{ {{ {} }} }};` once, an error a built-in gives",
                    error.name(),
                    components
                        .attributes()
                        .map(|(name, ty)| format!("{name}: {ty}"))
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
                end,
                "missing",
            )),
        }
    }
    identities
}
