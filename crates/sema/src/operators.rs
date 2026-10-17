//! Operators' signatures: what a call of each is checked against, known
//! before any body is checked, so that anything can call an operator
//! defined after it.

use std::collections::{BTreeMap, HashMap};

use trendy_diag::{Diagnostic, Span};
use trendy_hir as hir;
use trendy_syntax::ast::{self, AstNode, Item, SourceFile, TypeExpr};

use crate::types::{self, Origin, TypeParameters};
use crate::{Namespaces, Prelude, Source, errors};

/// What a call needs to know about the operator it calls.
#[derive(Clone)]
pub(crate) struct Signature {
    pub(crate) id: hir::OperatorId,
    pub(crate) name: String,
    /// The type parameters: those declared with `types`, then one for each
    /// `any` written as a parameter's type.
    pub(crate) types: Vec<hir::TypeParameter>,
    /// The parameters, as a call gives their arguments.
    pub(crate) written: BTreeMap<String, Param>,
    /// The parameters, as the operator sees them: without `per` ones,
    /// which are attributes of their relations' tuples.
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

    /// The relation whose tuples the `per` parameter `name` is an attribute
    /// of, if it is one.
    pub(crate) fn per_relation(&self, name: &str) -> Option<&str> {
        match self.written.get(name)? {
            Param::Per { relation, .. } => Some(relation),
            _ => None,
        }
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
        format!("`{}{}`", self.name, params_text(&self.written))
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

impl Signature {
    /// The signature of the prelude's operator `op`, whose id is `id`. It
    /// points at nothing in the prelude's source, which isn't the
    /// program's.
    pub(crate) fn of_prelude(id: hir::OperatorId, op: &hir::Operator) -> Self {
        let (builtin, result) = match &op.kind {
            hir::OperatorKind::Builtin(builtin) => (Some(*builtin), builtin.result()),
            hir::OperatorKind::ReadOnly { result, .. } => (None, Some(result.clone())),
            hir::OperatorKind::Update { .. } => (None, None),
        };
        Signature {
            id,
            name: op.name.clone(),
            types: op.types.clone(),
            written: written(&op.params),
            params: op.params.clone(),
            read_only: result.is_some(),
            result,
            decl: None,
            builtin,
            valid: true,
        }
    }
}

/// Every operator's signature: the prelude's, then the file's, each with
/// its parameters and result type checked. Built-ins declared outside the
/// prelude are reported. `main` is checked as an update operator, even
/// with a result type, which is reported when the entry point is.
pub(crate) fn signatures(
    prelude: &Prelude,
    file: &SourceFile,
    namespaces: &Namespaces,
    source: Source,
    errors: &mut Vec<Diagnostic>,
) -> Operators {
    let mut list: Vec<Signature> = prelude.signatures().collect();
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
                let (mut type_parameters, declared_valid) = declared.as_ref().map_or_else(
                    || (Vec::new(), true),
                    |d| declared_types(d, namespaces, errors),
                );
                let declared_count = type_parameters.len();
                let params = op
                    .params()
                    .and_then(|h| params(&h, &mut type_parameters, namespaces, errors));
                let result_written = op.result().filter(|_| ident.text() != "main");
                let result = result_written.as_ref().and_then(|t| {
                    let mut fixed = TypeParameters::Fixed(&type_parameters);
                    types::resolve(t, &mut fixed, namespaces, errors)
                });
                let result_valid = result_written.is_none() || result.is_some();
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
                    written: params.as_ref().map(written).unwrap_or_default(),
                    params: params.unwrap_or_default(),
                    read_only: result_written.is_some(),
                    result,
                    decl: Some(op),
                    builtin: None,
                });
            }
            Item::Builtin(_) | Item::BuiltinType(_) | Item::Kind(_)
                if source == Source::Prelude => {}
            Item::Kind(kind) => errors.push(errors::kind_outside_prelude(&kind)),
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

/// A parameter, as a call gives its argument.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Param {
    /// An argument of this type.
    Value(hir::Type),
    /// A relation of any heading, which `per` parameters are evaluated for
    /// each tuple of. The operator sees its tuples with their values as
    /// attributes, beside the tuples' own attributes, wrapped in one more,
    /// of this type parameter, `any tuple` (see [`hir::wrapped`]).
    Relation(hir::TypeParameter),
    /// An argument of type `ty` evaluated for each tuple of the argument
    /// for the parameter `relation`, which the operator sees as an
    /// attribute of that relation's tuples.
    Per { relation: String, ty: hir::Type },
}

/// An operator's parameters, as it sees them, where a relation's `per`
/// parameters are attributes of its tuples (see [`written`]): `types` are
/// its type parameters, which each `any` written as a parameter's type
/// adds one to. `None` after reporting why they are invalid. Each `per r`
/// must refer to another parameter, of type `any relation`.
pub(crate) fn params(
    heading: &ast::Heading,
    types: &mut Vec<hir::TypeParameter>,
    namespaces: &Namespaces,
    errors: &mut Vec<Diagnostic>,
) -> Option<BTreeMap<String, hir::Type>> {
    let resolved = types::resolve_attributes(
        heading,
        &mut TypeParameters::Open(types),
        namespaces,
        errors,
    )?;
    // Each `per` parameter, with the relation it refers to.
    let mut per: BTreeMap<String, String> = BTreeMap::new();
    let mut ok = true;
    for attribute in heading.attributes() {
        let (Some(clause), Some(name)) =
            (attribute.per(), attribute.name().and_then(|n| n.ident()))
        else {
            continue;
        };
        let Some(relation) = clause.relation().and_then(|r| r.ident()) else {
            continue;
        };
        let target = heading.attributes().find(|a| {
            a.name()
                .and_then(|n| n.ident())
                .is_some_and(|i| i.text() == relation.text())
        });
        let problem = match &target {
            _ if relation.text() == name.text() => Some(errors::PerProblem::Itself),
            None => Some(errors::PerProblem::NotAParameter(
                resolved.attributes().map(|(a, _)| a.to_string()).collect(),
            )),
            Some(target) if target.per().is_some() => Some(errors::PerProblem::Per),
            Some(target) => match target.type_expr() {
                Some(TypeExpr::Any(any)) if any_relation(&any) => None,
                Some(written) => Some(errors::PerProblem::NotAnyRelation(written)),
                None => {
                    ok = false;
                    continue;
                }
            },
        };
        if let Some(problem) = problem {
            errors.push(errors::per_relation(name.text(), &relation, problem));
            ok = false;
            continue;
        }
        per.insert(name.text().to_string(), relation.text().to_string());
    }
    if !ok {
        return None;
    }
    // Each relation's own attributes are wrapped as `any tuple`, which its
    // `any relation` becomes.
    let mut relations: BTreeMap<&str, hir::TypeParameter> = BTreeMap::new();
    for relation in per.values() {
        let Some(hir::Type::Parameter(p)) = resolved.get(relation) else {
            unreachable!("`per` refers to an `any relation`");
        };
        types[p.index].any = hir::Any::Tuple;
        relations.insert(relation, types[p.index].clone());
    }
    let mut seen = BTreeMap::new();
    for (name, ty) in resolved.attributes() {
        if per.contains_key(name) {
            continue;
        }
        let ty = match relations.get(name) {
            Some(wrapped) => {
                let attributes = per
                    .iter()
                    .filter(|(_, relation)| *relation == name)
                    .map(|(p, _)| (p.clone(), resolved.get(p).expect("a parameter").clone()))
                    .chain([(hir::wrapped(name), hir::Type::Parameter(wrapped.clone()))])
                    .collect();
                hir::Type::Relation(hir::Heading::new(attributes))
            }
            None => ty.clone(),
        };
        seen.insert(name.to_string(), ty);
    }
    Some(seen)
}

/// The parameters an operator sees, `seen`, as a call gives their
/// arguments: a relation whose tuples have a wrapped attribute (see
/// [`hir::wrapped`]) is one that `per` parameters are evaluated for each
/// tuple of, and its other attributes are those parameters.
pub(crate) fn written(seen: &BTreeMap<String, hir::Type>) -> BTreeMap<String, Param> {
    let mut written = BTreeMap::new();
    for (name, ty) in seen {
        let wrapped = match ty {
            hir::Type::Relation(heading) => heading.get(&hir::wrapped(name)),
            _ => None,
        };
        let (hir::Type::Relation(heading), Some(hir::Type::Parameter(wrapped))) = (ty, wrapped)
        else {
            written.insert(name.clone(), Param::Value(ty.clone()));
            continue;
        };
        written.insert(name.clone(), Param::Relation(wrapped.clone()));
        for (attribute, ty) in heading.attributes().filter(|(a, _)| !hir::is_wrapped(a)) {
            let per = Param::Per {
                relation: name.clone(),
                ty: ty.clone(),
            };
            written.insert(attribute.to_string(), per);
        }
    }
    written
}

/// The type parameters `declared` lists, each `any type`, `any tuple` or
/// `any relation` under a name of its own, and whether they are valid,
/// after reporting why not. One declared as a specific type is listed as
/// `any type`, so that its uses aren't reported too.
pub(crate) fn declared_types(
    declared: &ast::TypeParameters,
    namespaces: &Namespaces,
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
        if let Some(per) = attribute.per() {
            errors.push(errors::per_outside_parameters(&per));
            ok = false;
        }
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
            Some(TypeExpr::Any(any)) => types::any_of(&any, namespaces, errors),
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
                    &parameter.any,
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

/// Whether `any` is `any relation`.
fn any_relation(any: &ast::AnyType) -> bool {
    any.what()
        .is_some_and(|w| w.kind() == trendy_syntax::SyntaxKind::RelationKw)
}

/// Parameters as written: `{name: text, times: integer}`, and
/// `{r: any relation, of: integer per r}`.
pub(crate) fn params_text(params: &BTreeMap<String, Param>) -> String {
    let attributes: Vec<_> = params
        .iter()
        .map(|(name, param)| match param {
            Param::Value(ty) => format!("{name}: {ty}"),
            Param::Relation(_) => format!("{name}: any relation"),
            Param::Per { relation, ty } => format!("{name}: {ty} per {relation}"),
        })
        .collect();
    format!("{{{}}}", attributes.join(", "))
}
