//! The prelude: the declarations of the built-in operators, checked against
//! the operators the compiler implements, and the operators it defines in
//! trendy, checked as a program's are.

use trendy_hir as hir;
use trendy_syntax::ast::{BuiltinOperator, BuiltinType, Item, KindDecl, SourceFile};

use std::collections::{BTreeMap, BTreeSet};

use trendy_diag::{Code, Diagnostic, Span};
use trendy_syntax::SyntaxToken;
use trendy_syntax::ast::AstNode;

use crate::operators::{self, Param, Signature};
use crate::types::{self, TypeParameters};
use crate::{Namespaces, Source};

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

/// `params`, as calls give arguments for parameters of those types.
fn value_params(params: &BTreeMap<String, hir::Type>) -> BTreeMap<String, Param> {
    params
        .iter()
        .map(|(name, ty)| (name.clone(), Param::Value(ty.clone())))
        .collect()
}

/// The built-in operator `decl` declares, with the parameters the
/// compiler implements, or `None` after reporting why it isn't one. Its
/// name is added to those `declared`, unless it already is.
fn builtin_operator(
    decl: &BuiltinOperator,
    namespaces: &Namespaces,
    declared: &mut Vec<String>,
    errors: &mut Vec<Diagnostic>,
) -> Option<hir::Operator> {
    // A missing name is a syntax error, already reported.
    let ident = decl.name()?.ident()?;
    let name = ident.text();
    let Some(builtin) = hir::Builtin::from_name(name) else {
        errors.push(internal(
            format!("`{name}` is not a built-in the compiler implements"),
            &ident,
        ));
        return None;
    };
    if declared.iter().any(|d| d == name) {
        errors.push(internal(
            format!("`{name}` is declared more than once"),
            &ident,
        ));
        return None;
    }
    declared.push(name.to_string());
    let heading = decl.params()?;
    let mut types = Vec::new();
    let params = operators::params(&heading, &mut types, namespaces, errors)?.seen;
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
    if types != builtin.types() || params != builtin.params() {
        errors.push(Diagnostic::error(
            Code::Internal,
            format!(
                "`{name}` is declared with parameters that differ from its implementation's, `{}`",
                operators::params_text(&value_params(&builtin.params()))
            ),
            heading.span(),
            "not the parameters the compiler implements",
        ));
        return None;
    }
    Some(hir::Operator {
        name: name.to_string(),
        types,
        params,
        kind: hir::OperatorKind::Builtin(builtin),
    })
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
        match token.kind() {
            trendy_syntax::SyntaxKind::Lt => {
                requires.insert(hir::Requirement::Order);
            }
            _ => errors.push(internal(
                format!(
                    "`{}` is not a requirement the compiler implements",
                    token.text()
                ),
                &token,
            )),
        }
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
        // Every built-in type has an order: `false` before `true`, numbers
        // numerically, and text by code point.
        let missing = kind.requires.iter().find(|r| match r {
            hir::Requirement::Order => false,
        });
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

/// The loaded prelude: its operators, the built-ins first, then those it
/// defines in trendy, which every program has, checked already.
#[derive(Debug, Clone)]
pub struct Prelude {
    operators: Vec<hir::Operator>,
    /// Each one's parameters, as calls give their arguments, which HIR's
    /// lack for `per` parameters.
    written: Vec<BTreeMap<String, Param>>,
    /// The kinds, each with the built-in types that declare it.
    kinds: Vec<(hir::Kind, BTreeSet<hir::Scalar>)>,
}

impl Prelude {
    /// Load the prelude, checking that it declares exactly the built-in
    /// operators the compiler implements, each with the right parameters,
    /// and the operators it defines in trendy, which only it and the
    /// built-ins are in scope for.
    ///
    /// # Errors
    ///
    /// Returns every way the prelude is wrong: a declaration that doesn't
    /// match the compiler is an internal error, since the prelude is part
    /// of the compiler, at the declaration, and an error in an operator it
    /// defines is reported as a program's would be.
    pub fn load(file: &SourceFile) -> Result<Prelude, Vec<Diagnostic>> {
        let mut errors = Vec::new();
        // The kinds first, which types declare and built-ins can name.
        let mut kinds: Vec<(hir::Kind, BTreeSet<hir::Scalar>)> = Vec::new();
        for item in file.items() {
            if let Item::Kind(decl) = item
                && let Some(kind) = kind_decl(&decl, &kinds, &mut errors)
            {
                kinds.push((kind, BTreeSet::new()));
            }
        }
        let mut types: Vec<String> = Vec::new();
        for item in file.items() {
            if let Item::BuiltinType(decl) = item {
                builtin_type(&decl, &mut types, &mut kinds, &mut errors);
            }
        }
        let namespaces = Namespaces::with_kinds(&kinds);
        let mut operators: Vec<hir::Operator> = Vec::new();
        // The built-ins declared, rightly or not, so that a wrong
        // declaration isn't missing too.
        let mut declared: Vec<String> = Vec::new();
        for item in file.items() {
            match item {
                Item::Builtin(decl) => {
                    let op = builtin_operator(&decl, &namespaces, &mut declared, &mut errors);
                    operators.extend(op);
                }
                // Checked above, or once the built-ins are known.
                Item::BuiltinType(_) | Item::Kind(_) | Item::Operator(_) => {}
                Item::Variable(decl) => errors.push(Diagnostic::error(
                    Code::Internal,
                    "the prelude declares no variables",
                    decl.span(),
                    "a variable",
                )),
            }
        }
        // What is missing goes at the end.
        let end = Span::empty(file.span().end);
        let missing_operators = hir::Builtin::ALL
            .into_iter()
            .map(hir::Builtin::name)
            .filter(|b| !declared.iter().any(|d| d == b))
            .map(|b| format!("the prelude doesn't declare `{b}`"));
        let missing_types = builtin_types()
            .filter(|t| !types.iter().any(|d| d == t))
            .map(|t| format!("the prelude doesn't declare the type `{t}`"));
        for message in missing_operators.chain(missing_types) {
            errors.push(Diagnostic::error(Code::Internal, message, end, "missing"));
        }
        let written = operators
            .iter()
            .map(|op| value_params(&op.params))
            .collect();
        let builtins = Prelude {
            operators,
            written,
            kinds,
        };

        let checked = crate::check(&builtins, file, Source::Prelude, &mut errors);
        if !errors.is_empty() {
            errors.sort_by_key(|d| d.span().start);
            return Err(errors);
        }
        let written = checked.signatures().map(|s| s.written.clone()).collect();
        let (operators, _) = checked.finish(&builtins);
        Ok(Prelude {
            operators,
            written,
            kinds: builtins.kinds,
        })
    }

    /// The kinds, each with the built-in types that declare it.
    pub(crate) fn kinds(&self) -> &[(hir::Kind, BTreeSet<hir::Scalar>)] {
        &self.kinds
    }

    /// The operators, the built-ins first, each in the order the prelude
    /// has them.
    pub(crate) fn operators(&self) -> &[hir::Operator] {
        &self.operators
    }

    /// The operators' signatures, by id.
    pub(crate) fn signatures(&self) -> impl Iterator<Item = Signature> {
        self.operators
            .iter()
            .zip(&self.written)
            .enumerate()
            .map(|(i, (op, written))| {
                Signature::of_prelude(hir::OperatorId(i), op, written.clone())
            })
    }
}
