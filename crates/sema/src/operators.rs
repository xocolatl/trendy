//! Operators' signatures: what a call of each is checked against, known
//! before any body is checked, so that anything can call an operator
//! defined after it.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use trendy_diag::{Diagnostic, Span};
use trendy_hir as hir;
use trendy_syntax::ast::{self, AstNode, Item, Said, SourceFile, TypeExpr};

use crate::check::Lookup;
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
    pub(crate) params: BTreeMap<String, hir::Parameter>,
    /// The parameters without a type, whose errors are reported (see
    /// [`Params::unresolved`]).
    pub(crate) unresolved: BTreeSet<String>,
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
    /// What `name` refers to, if it is a parameter the operator sees: the
    /// parameter, or, for one without a type, nothing more to report.
    pub(crate) fn parameter(&self, name: &str) -> Option<Lookup> {
        if self.params.contains_key(name) {
            Some(Lookup::Found(hir::Expr::Parameter(name.to_string())))
        } else if self.unresolved.contains(name) {
            Some(Lookup::Reported)
        } else {
            None
        }
    }

    /// The mode of the parameter `name`, if it is one with a type.
    pub(crate) fn mode(&self, name: &str) -> Option<hir::Mode> {
        self.params.get(name).map(|p| p.mode)
    }

    /// Whether `name` is a `var` or `out` parameter, which the operator can
    /// assign.
    pub(crate) fn updates(&self, name: &str) -> bool {
        self.mode(name).is_some_and(|mode| mode != hir::Mode::Const)
    }

    /// Whether `name` is one of the parameters, with a type or without.
    pub(crate) fn is_parameter(&self, name: &str) -> bool {
        self.params.contains_key(name) || self.unresolved.contains(name)
    }

    /// The parameters' names, with a type or without.
    pub(crate) fn parameter_names(&self) -> impl Iterator<Item = &str> {
        self.params
            .keys()
            .chain(&self.unresolved)
            .map(String::as_str)
    }

    /// Where parameter `name`'s type is written.
    pub(crate) fn param_origin(&self, name: &str) -> Origin {
        let written = self.param_decl(name).and_then(|a| a.type_expr());
        written.map_or(Origin::Builtin, Origin::Type)
    }

    /// Where parameter `name` is written, for the program's own.
    pub(crate) fn param_decl(&self, name: &str) -> Option<ast::AttributeType> {
        self.decl
            .as_ref()?
            .params()?
            .attributes()
            .find(|a| a.is_named(name))
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
    /// Each name's operators, which their parameters' names tell apart.
    by_name: HashMap<String, Vec<hir::OperatorId>>,
}

/// A call's arguments, as they choose which operator of its name it
/// calls: the names of those written `name: value`, and how many have no
/// name.
#[derive(Default)]
pub(crate) struct ArgumentNames {
    pub(crate) named: BTreeSet<String>,
    pub(crate) unnamed: usize,
}

/// Which operator of a name a call's arguments' names choose.
pub(crate) enum Chosen<'a> {
    /// The one whose parameters they name, or, if none, the one whose
    /// parameters they name most nearly.
    One(&'a Signature),
    /// Those whose parameters they name equally nearly, of which none is
    /// nearer.
    Tied(Vec<&'a Signature>),
}

impl Operators {
    pub(crate) fn get(&self, id: hir::OperatorId) -> &Signature {
        &self.list[id.0]
    }

    /// The operators named `name`, in the order they are defined.
    pub(crate) fn overloads(&self, name: &str) -> impl Iterator<Item = &Signature> {
        self.by_name
            .get(name)
            .into_iter()
            .flatten()
            .map(|&id| self.get(id))
    }

    /// Whether an operator is named `name`.
    pub(crate) fn has(&self, name: &str) -> bool {
        self.by_name.contains_key(name)
    }

    /// The operator named `name` that a call whose arguments are named
    /// `args` calls, if one is named so: the only one, or the one whose
    /// parameters `args` names. Otherwise it is the one they name most
    /// nearly, each argument it has no parameter for, and each parameter
    /// they don't name, counting two, but an argument whose name is a
    /// misspelling of a parameter's they don't name, or that has no name,
    /// one, for both.
    pub(crate) fn chosen(&self, name: &str, args: &ArgumentNames) -> Option<Chosen<'_>> {
        let overloads: Vec<&Signature> = self.overloads(name).collect();
        if let [only] = overloads.as_slice() {
            return Some(Chosen::One(only));
        }
        let distance = |signature: &Signature| {
            let mut missing: Vec<&str> = signature
                .written
                .keys()
                .map(String::as_str)
                .filter(|p| !args.named.contains(*p))
                .collect();
            let mut distance = 0;
            for arg in args
                .named
                .iter()
                .filter(|a| !signature.written.contains_key(*a))
            {
                match trendy_diag::renamed(arg, missing.iter().copied()) {
                    Some(meant) => {
                        missing.retain(|m| *m != meant);
                        distance += 1;
                    }
                    None => distance += 2,
                }
            }
            // Naming one without a name, as one of those, fixes both.
            let paired = args.unnamed.min(missing.len());
            distance + paired + 2 * (args.unnamed - paired) + 2 * (missing.len() - paired)
        };
        let nearest = overloads.iter().map(|s| distance(s)).min()?;
        let mut tied: Vec<&Signature> = overloads
            .into_iter()
            .filter(|s| distance(s) == nearest)
            .collect();
        Some(if tied.len() == 1 {
            Chosen::One(tied.remove(0))
        } else {
            Chosen::Tied(tied)
        })
    }

    /// `signature`'s parameters, as a call's error says what they are:
    /// `` `greet{name: text}` ``, and those of the other operators of its
    /// name, which a call's arguments' names choose among.
    pub(crate) fn described(&self, signature: &Signature) -> String {
        let others: Vec<String> = self
            .overloads(&signature.name)
            .filter(|s| s.id != signature.id)
            .map(Signature::text)
            .collect();
        if others.is_empty() {
            signature.text()
        } else {
            format!(
                "{}, or those of another `{}`, {}",
                signature.text(),
                signature.name,
                trendy_diag::disjoin(&others)
            )
        }
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = &Signature> {
        self.list.iter()
    }

    /// Every operator's name, which a misspelled one, or another
    /// language's, may be, with the names of its parameters that take
    /// values, as a call gives their arguments: those a lone argument can
    /// be for, not a `var` or `out` one's variable. A name with several
    /// operators is there for each, in the order they are defined.
    pub(crate) fn names(&self) -> Vec<(&str, Vec<&str>)> {
        self.list
            .iter()
            .map(|signature| {
                let params = signature
                    .written
                    .iter()
                    .filter(|(_, param)| !matches!(param, Param::Variable(..)))
                    .map(|(name, _)| name.as_str())
                    .collect();
                (signature.name.as_str(), params)
            })
            .collect()
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
            unresolved: BTreeSet::new(),
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
                    .map(|h| params(&h, &mut type_parameters, namespaces, errors));
                let params_valid = params.as_ref().is_some_and(|p| p.unresolved.is_empty());
                let result_written = op.result().filter(|_| ident.text() != "main");
                let result = result_written.as_ref().and_then(|t| {
                    let mut fixed = TypeParameters::Fixed(&type_parameters);
                    types::resolve(t, &mut fixed, namespaces, errors)
                });
                let result_valid = result_written.is_none() || result.is_some();
                // The declared ones are each checked once they and the
                // parameters' types are valid.
                let determined = match &declared {
                    Some(declared) if declared_valid && params_valid => {
                        determined(&op, declared, &type_parameters[..declared_count], errors)
                    }
                    _ => true,
                };
                let Params {
                    mut seen,
                    unresolved,
                } = params.unwrap_or_else(|| Params {
                    seen: BTreeMap::new(),
                    unresolved: BTreeSet::new(),
                });
                // A read-only operator updates nothing (TTM RM Prescription
                // 3), so its parameters are `const`, as those reported are
                // taken to be.
                if result_written.is_some()
                    && let Some(heading) = op.params()
                {
                    for attribute in heading.attributes() {
                        let (Some(mode), Some(ident)) =
                            (attribute.mode(), attribute.name().and_then(|n| n.ident()))
                        else {
                            continue;
                        };
                        if let Some(param) = seen.get_mut(ident.text())
                            && param.mode != hir::Mode::Const
                        {
                            errors.push(errors::updated_in_read_only(ident.text(), &mode, &ident));
                            param.mode = hir::Mode::Const;
                        }
                    }
                }
                list.push(Signature {
                    id: hir::OperatorId(list.len()),
                    name: ident.text().to_string(),
                    valid: declared_valid && params_valid && result_valid && determined,
                    types: type_parameters,
                    written: written(&seen),
                    params: seen,
                    unresolved,
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
            Item::Variable(_) | Item::Heading(_) => {}
        }
    }
    let mut by_name: HashMap<String, Vec<hir::OperatorId>> = HashMap::new();
    for signature in &list {
        by_name
            .entry(signature.name.clone())
            .or_default()
            .push(signature.id);
    }
    Operators { list, by_name }
}

/// A parameter, as a call gives its argument.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Param {
    /// An argument of this type.
    Value(hir::Type),
    /// A variable of this type, which the call updates: a `var` or `out`
    /// parameter's argument, as the mode says.
    Variable(hir::Type, hir::Mode),
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

/// An operator's parameters, as [`params`] resolves them.
pub(crate) struct Params {
    /// The parameters as the operator sees them, of valid types.
    pub(crate) seen: BTreeMap<String, hir::Parameter>,
    /// The names of the others, already reported: those whose types are
    /// invalid or written twice, and, if any is, every `per` parameter and
    /// the relation it is in, whose tuples' attributes they are.
    pub(crate) unresolved: BTreeSet<String>,
}

/// An operator's parameters, as it sees them, where a relation's `per`
/// parameters are attributes of its tuples (see [`written`]): `types` are
/// its type parameters, which each `any` written as a parameter's type
/// adds one to. Each `per r` must refer to another parameter, of type
/// `any relation`. Those that are invalid are reported, and their names
/// kept, as a parameter's all the same. Each has the mode written before
/// its name, or is `const`; a `per` one, and the relation it refers to,
/// can only be `const`, as is one reported for being `var` or `out`.
pub(crate) fn params(
    heading: &ast::Heading,
    types: &mut Vec<hir::TypeParameter>,
    namespaces: &Namespaces,
    errors: &mut Vec<Diagnostic>,
) -> Params {
    let Typed { seen, unresolved } = typed_params(heading, types, namespaces, errors);
    // Each `per` parameter, with the relation it refers to.
    let mut per: BTreeMap<String, String> = BTreeMap::new();
    for attribute in heading.attributes() {
        if let (Some(name), Some(relation)) = (
            attribute.name().and_then(|n| n.ident()),
            attribute
                .per()
                .and_then(|p| p.relation())
                .and_then(|r| r.ident()),
        ) {
            per.insert(name.text().to_string(), relation.text().to_string());
        }
    }
    let mut modes: BTreeMap<String, hir::Mode> = BTreeMap::new();
    for attribute in heading.attributes() {
        let (Some(mode), Some(ident)) =
            (attribute.mode(), attribute.name().and_then(|n| n.ident()))
        else {
            continue;
        };
        let said = match mode.says() {
            Said::Const => continue,
            Said::Var => hir::Mode::Var,
            Said::Out => hir::Mode::Out,
        };
        let name = ident.text();
        let relation = per
            .iter()
            .find(|(_, r)| *r == name)
            .map(|(p, _)| p.as_str());
        if per.contains_key(name) || relation.is_some() {
            errors.push(errors::updated_with_per(name, relation, &mode, &ident));
            continue;
        }
        modes.insert(name.to_string(), said);
    }
    let seen = seen
        .into_iter()
        .map(|(name, ty)| {
            let mode = modes.get(&name).copied().unwrap_or(hir::Mode::Const);
            (name, hir::Parameter { ty, mode })
        })
        .collect();
    Params { seen, unresolved }
}

/// An operator's parameters' types, as [`typed_params`] resolves them.
struct Typed {
    seen: BTreeMap<String, hir::Type>,
    unresolved: BTreeSet<String>,
}

/// The types of an operator's parameters, as for [`params`].
fn typed_params(
    heading: &ast::Heading,
    types: &mut Vec<hir::TypeParameter>,
    namespaces: &Namespaces,
    errors: &mut Vec<Diagnostic>,
) -> Typed {
    let types::Attributes {
        resolved,
        mut unresolved,
    } = types::attributes(
        heading,
        &mut TypeParameters::Open(types),
        namespaces,
        errors,
    );
    // Each `per` parameter, with the relation it refers to.
    let mut per: BTreeMap<String, String> = BTreeMap::new();
    let mut ok = unresolved.is_empty();
    for attribute in heading.attributes() {
        let (Some(clause), Some(name)) =
            (attribute.per(), attribute.name().and_then(|n| n.ident()))
        else {
            continue;
        };
        let Some(relation) = clause.relation().and_then(|r| r.ident()) else {
            continue;
        };
        let target = heading.attributes().find(|a| a.is_named(relation.text()));
        let problem = match &target {
            _ if relation.text() == name.text() => Some(errors::PerProblem::Itself),
            None => Some(errors::PerProblem::NotAParameter(
                resolved.keys().chain(&unresolved).cloned().collect(),
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
        // A relation's tuples have its `per` parameters as attributes, so
        // without all of them right, neither it nor they have a type.
        for attribute in heading.attributes() {
            let (Some(clause), Some(name)) =
                (attribute.per(), attribute.name().and_then(|n| n.ident()))
            else {
                continue;
            };
            unresolved.insert(name.text().to_string());
            if let Some(relation) = clause.relation().and_then(|r| r.ident())
                && resolved.contains_key(relation.text())
            {
                unresolved.insert(relation.text().to_string());
            }
        }
        let seen = resolved
            .into_iter()
            .filter(|(name, _)| !unresolved.contains(name))
            .collect();
        return Typed { seen, unresolved };
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
    for (name, ty) in &resolved {
        if per.contains_key(name) {
            continue;
        }
        let ty = match relations.get(name.as_str()) {
            Some(wrapped) => {
                let attributes = per
                    .iter()
                    .filter(|(_, relation)| *relation == name)
                    .map(|(p, _)| (p.clone(), resolved[p].clone()))
                    .chain([(hir::wrapped(name), hir::Type::Parameter(wrapped.clone()))])
                    .collect();
                hir::Type::Relation(hir::Heading::new(attributes))
            }
            None => ty.clone(),
        };
        seen.insert(name.clone(), ty);
    }
    Typed { seen, unresolved }
}

/// The parameters an operator sees, `seen`, as a call gives their
/// arguments: a relation whose tuples have a wrapped attribute (see
/// [`hir::wrapped`]) is one that `per` parameters are evaluated for each
/// tuple of, and its other attributes are those parameters.
pub(crate) fn written(seen: &BTreeMap<String, hir::Parameter>) -> BTreeMap<String, Param> {
    let mut written = BTreeMap::new();
    for (name, hir::Parameter { ty, mode }) in seen {
        if *mode != hir::Mode::Const {
            written.insert(name.clone(), Param::Variable(ty.clone(), *mode));
            continue;
        }
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
/// `any type`, so that its uses aren't reported too. They are in braces,
/// so their order means nothing: they are numbered in their names'
/// order.
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
        if let Some(mode) = attribute.mode() {
            errors.push(errors::mode_outside_parameters(&mode));
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
            index: 0,
            name: Some(name.to_string()),
            any: any.unwrap_or(hir::Any::Type),
        });
    }
    list.sort_by(|a, b| a.name.cmp(&b.name));
    for (index, parameter) in list.iter_mut().enumerate() {
        parameter.index = index;
    }
    (list, ok)
}

/// Whether each of the type parameters `declared` lists, `list`, is in a
/// parameter's type, written `op`'s parameters `params`, so that a call's
/// arguments determine it; otherwise it is reported. One written only
/// once, as a parameter's whole type, is reported too: it is `any`
/// without a name, which the fix writes. One written once inside a
/// parameter's type, as in `array T`, needs its name, as `any` is only a
/// whole type.
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
        // A parameter's whole type, the one place `any …` can be written,
        // as `array any tuple` can't be.
        let whole = |t: &ast::TypeRef| {
            t.syntax()
                .parent()
                .and_then(|attribute| attribute.parent())
                .is_some_and(|list| list == *params.syntax())
        };
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
            [only] if uses == 1 && whole(only) => {
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
            Param::Variable(ty, mode) => format!("{} {name}: {ty}", mode.word()),
            Param::Relation(_) => format!("{name}: any relation"),
            Param::Per { relation, ty } => format!("{name}: {ty} per {relation}"),
        })
        .collect();
    format!("{{{}}}", attributes.join(", "))
}
