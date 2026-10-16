//! Operators' signatures: what a call of each is checked against, known
//! before any body is checked, so that anything can call an operator
//! defined after it.

use std::collections::{BTreeMap, HashMap};

use trendy_diag::{Diagnostic, Span};
use trendy_hir as hir;
use trendy_syntax::ast::{self, AstNode, Item, SourceFile, TypeExpr};

use crate::types::{self, Origin, TypeParameters};
use crate::{Namespaces, Prelude, errors};

/// What a call needs to know about the operator it calls.
#[derive(Clone)]
pub(crate) struct Signature {
    pub(crate) id: hir::OperatorId,
    pub(crate) name: String,
    /// The type parameters: those declared with `types`, then one for each
    /// `any` written as a parameter's type.
    pub(crate) types: Vec<hir::TypeParameter>,
    pub(crate) params: BTreeMap<String, hir::Type>,
    /// Whether it is read-only, written with a result type: its call is
    /// then a value, and an update operator's isn't.
    pub(crate) read_only: bool,
    /// A read-only operator's result type, if the one written is valid.
    pub(crate) result: Option<hir::Type>,
    /// The definition, for the program's own: where its parameter and
    /// result types are written, for the labels of mismatches.
    pub(crate) decl: Option<ast::Operator>,
    /// The built-in it is, for the prelude's.
    pub(crate) builtin: Option<hir::Builtin>,
    /// Whether the parameters and the result type are valid. Calls of an
    /// operator with errors in them aren't checked, because those errors
    /// are already reported.
    pub(crate) valid: bool,
}

impl Signature {
    /// Where parameter `name`'s type is written.
    pub(crate) fn param_origin(&self, name: &str) -> Origin {
        let written = self.decl.as_ref().and_then(|decl| {
            decl.params()?
                .attributes()
                .find(|a| {
                    a.name()
                        .and_then(|n| n.ident())
                        .is_some_and(|i| i.text() == name)
                })?
                .type_expr()
        });
        written.map_or(Origin::Builtin, Origin::Type)
    }

    /// Where the operator's name is written in its definition, for the
    /// program's own.
    pub(crate) fn defined(&self) -> Option<Span> {
        let ident = self.decl.as_ref()?.name()?.ident()?;
        Some(trendy_syntax::span(ident.text_range()))
    }

    /// The parameters, as a call's error says what they are:
    /// `` `greet{name: text}` ``.
    pub(crate) fn text(&self) -> String {
        format!("`{}{}`", self.name, params_text(&self.params))
    }
}

/// Every operator, by id: the prelude's, then the program's.
pub(crate) struct Operators {
    list: Vec<Signature>,
    by_name: HashMap<String, hir::OperatorId>,
}

impl Operators {
    pub(crate) fn get(&self, id: hir::OperatorId) -> &Signature {
        &self.list[id.0]
    }

    pub(crate) fn named(&self, name: &str) -> Option<&Signature> {
        self.by_name.get(name).map(|&id| self.get(id))
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = &Signature> {
        self.list.iter()
    }

    /// Every operator's name, which a misspelled one may be.
    pub(crate) fn names(&self) -> impl Iterator<Item = &str> {
        self.by_name.keys().map(String::as_str)
    }
}

/// Every operator's signature: the prelude's, then the program's, each
/// with its parameters and result type checked. Built-ins declared outside
/// the prelude are reported. `main` is checked as an update operator, even
/// with a result type, which is reported when the entry point is.
pub(crate) fn signatures(
    prelude: &Prelude,
    file: &SourceFile,
    namespaces: &Namespaces,
    errors: &mut Vec<Diagnostic>,
) -> Operators {
    let mut list: Vec<Signature> = prelude
        .operators()
        .iter()
        .enumerate()
        .map(|(i, op)| {
            let hir::OperatorKind::Builtin(builtin) = op.kind else {
                unreachable!("the prelude declares only built-ins");
            };
            Signature {
                id: hir::OperatorId(i),
                name: op.name.clone(),
                types: op.types.clone(),
                params: op.params.clone(),
                read_only: false,
                result: None,
                decl: None,
                builtin: Some(builtin),
                valid: true,
            }
        })
        .collect();
    for item in file.items() {
        match item {
            Item::Operator(op) => {
                let Some(ident) = op.name().and_then(|n| n.ident()) else {
                    continue;
                };
                if !namespaces.is_first_operator(&ident) {
                    continue;
                }
                let declared = op.type_parameters();
                let (mut type_parameters, declared_valid) = declared
                    .as_ref()
                    .map_or_else(|| (Vec::new(), true), |d| declared_types(d, errors));
                let declared_count = type_parameters.len();
                let params = op
                    .params()
                    .and_then(|h| params(&h, &mut type_parameters, namespaces, errors));
                let written = op.result().filter(|_| ident.text() != "main");
                let result = written.as_ref().and_then(|t| {
                    let mut fixed = TypeParameters::Fixed(&type_parameters);
                    types::resolve(t, &mut fixed, namespaces, errors)
                });
                let result_valid = written.is_none() || result.is_some();
                // The declared ones are each checked once they and the
                // parameters' types are valid.
                let determined = match (&declared, &params) {
                    (Some(declared), Some(_)) if declared_valid => {
                        determined(&op, declared, &type_parameters[..declared_count], errors)
                    }
                    _ => true,
                };
                list.push(Signature {
                    id: hir::OperatorId(list.len()),
                    name: ident.text().to_string(),
                    valid: declared_valid && params.is_some() && result_valid && determined,
                    types: type_parameters,
                    params: params.unwrap_or_default(),
                    read_only: written.is_some(),
                    result,
                    decl: Some(op),
                    builtin: None,
                });
            }
            Item::Builtin(builtin) => errors.push(errors::builtin_outside_prelude(
                builtin.keyword(),
                builtin.syntax(),
            )),
            Item::BuiltinType(builtin) => errors.push(errors::builtin_outside_prelude(
                builtin.keyword(),
                builtin.syntax(),
            )),
            Item::Variable(_) => {}
        }
    }
    let by_name = list.iter().map(|s| (s.name.clone(), s.id)).collect();
    Operators { list, by_name }
}

/// A user operator's parameters, where `types` are its type parameters,
/// which each `any` written as a parameter's type adds one to, or `None`
/// after reporting why they are invalid.
pub(crate) fn params(
    heading: &ast::Heading,
    types: &mut Vec<hir::TypeParameter>,
    namespaces: &Namespaces,
    errors: &mut Vec<Diagnostic>,
) -> Option<BTreeMap<String, hir::Type>> {
    let heading = types::resolve_heading(
        heading,
        &mut TypeParameters::Open(types),
        namespaces,
        errors,
    )?;
    Some(
        heading
            .attributes()
            .map(|(name, ty)| (name.to_string(), ty.clone()))
            .collect(),
    )
}

/// The type parameters `declared` lists, each `any type`, `any tuple` or
/// `any relation` under a name of its own, and whether they are valid,
/// after reporting why not. One declared as a specific type is listed as
/// `any type`, so that its uses aren't reported too.
fn declared_types(
    declared: &ast::TypeParameters,
    errors: &mut Vec<Diagnostic>,
) -> (Vec<hir::TypeParameter>, bool) {
    let Some(heading) = declared.heading() else {
        return (Vec::new(), false);
    };
    let mut list: Vec<hir::TypeParameter> = Vec::new();
    let mut spans: HashMap<String, Span> = HashMap::new();
    let mut ok = true;
    for attribute in heading.attributes() {
        let Some(ident) = attribute.name().and_then(|n| n.ident()) else {
            continue;
        };
        let (name, span) = (ident.text(), trendy_syntax::span(ident.text_range()));
        if let Some(&first) = spans.get(name) {
            errors.push(errors::duplicate_type_parameter(name, span, first));
            ok = false;
            continue;
        }
        spans.insert(name.to_string(), span);
        if hir::Scalar::from_name(name).is_some() {
            errors.push(errors::type_parameter_named_as_type(name, span));
            ok = false;
            continue;
        }
        let any = match attribute.type_expr() {
            Some(TypeExpr::Any(any)) => types::any_of(&any),
            Some(other) => {
                errors.push(errors::type_parameter_not_any(name, &other));
                None
            }
            None => None,
        };
        ok &= any.is_some();
        list.push(hir::TypeParameter {
            index: list.len(),
            name: Some(name.to_string()),
            any: any.unwrap_or(hir::Any::Type),
        });
    }
    (list, ok)
}

/// Whether each of the type parameters `declared` lists, `list`, is in a
/// parameter's type, written `op`'s parameters `params`, so that a call's
/// arguments determine it; otherwise it is reported. One written only
/// once, as a parameter's type, is reported too: it is `any` without a
/// name, which the fix writes.
fn determined(
    op: &ast::Operator,
    declared: &ast::TypeParameters,
    list: &[hir::TypeParameter],
    errors: &mut Vec<Diagnostic>,
) -> bool {
    let (Some(heading), Some(params)) = (declared.heading(), op.params()) else {
        return true;
    };
    let attributes: Vec<ast::AttributeType> = heading.attributes().collect();
    let mut ok = true;
    for (attribute, parameter) in attributes.iter().zip(list) {
        let name = parameter
            .name
            .as_deref()
            .expect("declared type parameters have names");
        let named = |t: &ast::TypeRef| t.ident().is_some_and(|i| i.text() == name);
        let in_params: Vec<ast::TypeRef> = params.type_refs().filter(named).collect();
        let uses = op.type_refs().filter(named).count();
        // Removing it removes the list, if it is the only one.
        let removal = if attributes.len() == 1 {
            declared.removal()
        } else {
            heading.removal(attribute)
        };
        match in_params.as_slice() {
            [] => {
                errors.push(errors::undetermined_type_parameter(
                    name,
                    attribute.span(),
                    uses,
                    removal,
                ));
                ok = false;
            }
            [only] if uses == 1 => {
                errors.push(errors::type_parameter_named_once(
                    name,
                    parameter.any,
                    only.span(),
                    attribute.span(),
                    removal,
                ));
            }
            _ => {}
        }
    }
    ok
}

/// Parameters as written: `{name: text, times: integer}`.
pub(crate) fn params_text(params: &BTreeMap<String, hir::Type>) -> String {
    let attributes: Vec<_> = params
        .iter()
        .map(|(name, ty)| format!("{name}: {ty}"))
        .collect();
    format!("{{{}}}", attributes.join(", "))
}
