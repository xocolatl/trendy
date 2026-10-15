//! Operators' signatures: what a call of each is checked against, known
//! before any body is checked, so that anything can call an operator
//! defined after it.

use std::collections::{BTreeMap, HashMap};

use trendy_diag::{Diagnostic, Span};
use trendy_hir as hir;
use trendy_syntax::ast::{self, AstNode, Item, SourceFile};

use crate::types::{self, Origin};
use crate::{Namespaces, Prelude, errors};

/// What a call needs to know about the operator it calls.
#[derive(Clone)]
pub(crate) struct Signature {
    pub(crate) id: hir::OperatorId,
    pub(crate) name: String,
    pub(crate) params: BTreeMap<String, hir::ParamType>,
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
                let params = op.params().and_then(|h| params(&h, namespaces, errors));
                let written = op.result().filter(|_| ident.text() != "main");
                let result = written
                    .as_ref()
                    .and_then(|t| types::resolve(t, namespaces, errors));
                let result_valid = written.is_none() || result.is_some();
                list.push(Signature {
                    id: hir::OperatorId(list.len()),
                    name: ident.text().to_string(),
                    valid: params.is_some() && result_valid,
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

/// A user operator's parameters, or `None` after reporting why they are
/// invalid.
fn params(
    heading: &ast::Heading,
    namespaces: &Namespaces,
    errors: &mut Vec<Diagnostic>,
) -> Option<BTreeMap<String, hir::ParamType>> {
    let heading = types::resolve_heading(heading, namespaces, errors)?;
    Some(
        heading
            .attributes()
            .map(|(name, ty)| (name.to_string(), hir::ParamType::Type(ty.clone())))
            .collect(),
    )
}

/// Parameters as written: `{name: text, times: integer}`.
pub(crate) fn params_text(params: &BTreeMap<String, hir::ParamType>) -> String {
    let attributes: Vec<_> = params
        .iter()
        .map(|(name, ty)| format!("{name}: {ty}"))
        .collect();
    format!("{{{}}}", attributes.join(", "))
}
