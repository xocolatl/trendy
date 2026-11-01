//! Operators' signatures: what a call of each is checked against, known
//! before any body is checked, so that anything can call an operator
//! defined after it.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use trendy_diag::{Diagnostic, Span};
use trendy_hir as hir;
use trendy_syntax::ast::{self, AstNode, Item, Said, TypeExpr};

use crate::check::Lookup;
use crate::types::{self, Origin, TypeParameters};
use crate::{Files, Kind, Namespaces, Source, errors};

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
    /// The file it is in. The prelude's declarations are labeled, but no
    /// fix changes them, as the prelude is part of the compiler.
    pub(crate) source: Source,
    /// The type of the program's own it is made from, if it is a
    /// selector, a constraint or a conversion of it.
    pub(crate) from_type: Option<FromType>,
    /// Whether the parameters and the result type are valid. Calls of an
    /// operator with errors in them aren't checked, because those errors
    /// are already reported.
    pub(crate) valid: bool,
    /// What it is among the operators of its name and its parameters'
    /// names.
    pub(crate) family: Family,
    /// A specification's versions, each before those it is more specific
    /// than (see [`hir::Operator::versions`]).
    pub(crate) versions: Vec<hir::Version>,
}

/// What an operator is among those of its name and its parameters' names
/// (*Type Inheritance and Relational Theory*, IM Prescription 17).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Family {
    /// The only one, or the specification of the others, whose parameters'
    /// types are supertypes of theirs.
    Own,
    /// A version of the specification with this id, which calls of it
    /// compute for arguments of its parameters' types.
    Version(hir::OperatorId),
    /// One with the same parameters as another of which it is no version,
    /// which is reported.
    Duplicate,
}

/// An operator made from the declaration of a type of the program's own,
/// `defined`, declared by `decl`, as `made` says.
#[derive(Clone)]
pub(crate) struct FromType {
    pub(crate) defined: hir::Defined,
    /// The type's declaration, where the components are written.
    pub(crate) decl: ast::TypeDecl,
    pub(crate) made: Made,
}

/// What an operator made from a type's declaration is.
#[derive(Clone)]
pub(crate) enum Made {
    /// The selector of a possible representation, named after the type,
    /// whose parameters are the representation's components: its value is
    /// the value of the type with those components, of the stored
    /// representation, or with those its `to` computes from them, of
    /// another, if the type's constraints are true of them.
    Selector(ast::Representation),
    /// A constraint, true of the stored components, its parameters, if they
    /// are those of a value of the type.
    Constraint(ast::Constraint),
    /// A possible representation's conversion to the stored one, whose
    /// parameters are its components: its value is the tuple of the stored
    /// components, which its `to` computes from them, whether or not they
    /// satisfy the type's constraints. No call can name it.
    To(ast::Representation),
    /// A possible representation's conversion from the stored one, whose
    /// parameters are the stored components: its value is the tuple of the
    /// representation's own, which its `from` computes from them. No call
    /// can name it.
    From(ast::Representation),
}

impl FromType {
    /// The type, if the operator is a selector.
    pub(crate) fn selects(&self) -> Option<&hir::Defined> {
        matches!(self.made, Made::Selector(_)).then_some(&self.defined)
    }

    /// Whether the operator is the selector of the representation the
    /// type's values are stored as, whose value is selected from its
    /// arguments with no conversion.
    pub(crate) fn selects_stored(&self) -> bool {
        matches!(&self.made, Made::Selector(r) if r.conversions().next().is_none())
    }

    /// The representation whose components are the operator's parameters:
    /// a selector's or a conversion to the stored one's own, or else the
    /// stored one.
    fn parameters(&self) -> Option<ast::Representation> {
        match &self.made {
            Made::Selector(representation) | Made::To(representation) => {
                Some(representation.clone())
            }
            Made::Constraint(_) | Made::From(_) => crate::named_types::stored(&self.decl),
        }
    }
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
        match self.param_decl(name).and_then(|a| a.type_expr()) {
            Some(written) if self.source == Source::Prelude => Origin::Prelude(written),
            Some(written) => Origin::Type(written),
            None => Origin::Builtin,
        }
    }

    /// Where parameter `name` is written, for the program's own: one made
    /// from a type's declaration has a representation's components.
    pub(crate) fn param_decl(&self, name: &str) -> Option<ast::AttributeType> {
        let params = match &self.from_type {
            Some(from) => from.parameters()?.components()?,
            None => self.decl.as_ref()?.params()?,
        };
        params.attributes().find(|a| a.is_named(name))
    }

    /// Whether its parameters are exactly `components`, as the selector's
    /// of their representation are.
    pub(crate) fn takes(&self, components: &hir::Heading) -> bool {
        self.params.len() == components.attributes().count()
            && components
                .attributes()
                .all(|(c, _)| self.params.contains_key(c))
    }

    /// The type of the program's own it is a selector of.
    pub(crate) fn selects(&self) -> Option<&hir::Defined> {
        self.from_type.as_ref()?.selects()
    }

    /// The type of the program's own it is a constraint of, and the
    /// constraint.
    pub(crate) fn constrains(&self) -> Option<(&hir::Defined, &ast::Constraint)> {
        let from = self.from_type.as_ref()?;
        match &from.made {
            Made::Constraint(constraint) => Some((&from.defined, constraint)),
            Made::Selector(_) | Made::To(_) | Made::From(_) => None,
        }
    }

    /// The type of the program's own and the possible representation it
    /// converts to, from the stored one, if it is a conversion.
    pub(crate) fn converts(&self) -> Option<(&hir::Defined, &ast::Representation)> {
        let from = self.from_type.as_ref()?;
        match &from.made {
            Made::From(representation) => Some((&from.defined, representation)),
            Made::Selector(_) | Made::Constraint(_) | Made::To(_) => None,
        }
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
    /// program's own: a constraint's `constraint`, and a representation's
    /// selector, if it isn't the stored one's, or conversion, its `to` or
    /// `from`.
    pub(crate) fn defined(&self) -> Option<Span> {
        let conversion = |r: &ast::Representation, d: ast::Direction| {
            r.conversions()
                .find(|c| c.direction() == Some(d))
                .and_then(|c| c.keyword())
        };
        let ident = match &self.from_type {
            Some(from) => match &from.made {
                Made::Constraint(constraint) => constraint.keyword()?,
                Made::Selector(_) if from.selects_stored() => from.decl.name()?.ident()?,
                Made::Selector(r) | Made::To(r) => conversion(r, ast::Direction::To)?,
                Made::From(r) => conversion(r, ast::Direction::From)?,
            },
            None => self.decl.as_ref()?.name()?.ident()?,
        };
        Some(trendy_syntax::span(ident.text_range()))
    }

    /// The value of a selector, of the type `defined`: the value whose
    /// components are its parameters, if they satisfy its constraints.
    pub(crate) fn selection(defined: &hir::Defined) -> hir::Expr {
        let components = defined
            .components
            .attributes()
            .map(|(name, _)| (name.to_string(), hir::Expr::Parameter(name.to_string())))
            .collect();
        let defined = Box::new(defined.clone());
        hir::Expr::Select(defined, Box::new(hir::Expr::Tuple(components)))
    }

    /// The parameters, as a call's error says what they are:
    /// `` `greet{name: text}` ``.
    pub(crate) fn text(&self) -> String {
        format!("`{}{}`", self.name, params_text(&self.written))
    }
}

/// Every operator, by id: the prelude's, then the constraints of the
/// program's types, then the program's operators and selectors.
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
    /// The operators of `list`, by id, each named in the order they are
    /// defined, but constraints and conversions, which no call can name.
    fn new(list: Vec<Signature>) -> Self {
        let mut by_name: HashMap<String, Vec<hir::OperatorId>> = HashMap::new();
        // A version is called through its specification, and a duplicate
        // not at all.
        let named = |s: &&Signature| {
            s.constrains().is_none() && s.converts().is_none() && s.family == Family::Own
        };
        for signature in list.iter().filter(named) {
            by_name
                .entry(signature.name.clone())
                .or_default()
                .push(signature.id);
        }
        Operators { list, by_name }
    }

    pub(crate) fn get(&self, id: hir::OperatorId) -> &Signature {
        &self.list[id.0]
    }

    /// The selector of the type of the program's own `defined` in the
    /// possible representation with the component `component`, if the
    /// type has selectors.
    pub(crate) fn selector(
        &self,
        defined: &hir::Defined,
        component: &str,
    ) -> Option<hir::OperatorId> {
        self.overloads(&defined.name)
            .find(|signature| {
                signature
                    .selects()
                    .is_some_and(|ty| ty.name == defined.name)
                    && signature.params.contains_key(component)
            })
            .map(|signature| signature.id)
    }

    /// The selector of the type of the program's own `defined` in the
    /// possible representation with exactly the components `components`, if
    /// the type has selectors.
    pub(crate) fn selector_of(
        &self,
        defined: &hir::Defined,
        components: &hir::Heading,
    ) -> Option<hir::OperatorId> {
        self.overloads(&defined.name)
            .find(|signature| {
                signature
                    .selects()
                    .is_some_and(|ty| ty.name == defined.name)
                    && signature.takes(components)
            })
            .map(|signature| signature.id)
    }

    /// The operators named `name`, in the order they are defined.
    pub(crate) fn overloads(&self, name: &str) -> impl Iterator<Item = &Signature> {
        self.by_name
            .get(name)
            .into_iter()
            .flatten()
            .map(|&id| self.get(id))
    }

    /// The prelude's `message{error: error}: text`, a specification of its
    /// union type `error`, which writes an error's message, if the prelude
    /// declares it so.
    pub(crate) fn message(&self) -> Option<&Signature> {
        self.overloads("message").find(|s| {
            let error = s.params.get("error").map(|p| &p.ty);
            s.source == Source::Prelude
                && s.written.keys().eq(["error"])
                && matches!(error, Some(hir::Type::Dummy(d)) if d.name == "error")
                && s.result == Some(hir::Type::Scalar(hir::Scalar::Text))
                && s.decl.as_ref().is_some_and(|d| d.body().is_none())
        })
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
    /// The signature of `builtin`, whose id is `id`, with the type
    /// parameters and parameters its declaration has, which are the
    /// compiler's. Its declaration isn't an operator's, so it points at
    /// none (see [`Signature::param_origin`]).
    pub(crate) fn builtin(
        id: hir::OperatorId,
        builtin: hir::Builtin,
        types: Vec<hir::TypeParameter>,
        params: BTreeMap<String, hir::Parameter>,
    ) -> Self {
        let result = builtin.result();
        Signature {
            id,
            name: builtin.name().to_string(),
            types,
            written: written(&params),
            params,
            unresolved: BTreeSet::new(),
            read_only: result.is_some(),
            result,
            decl: None,
            builtin: Some(builtin),
            source: Source::Prelude,
            from_type: None,
            family: Family::Own,
            versions: Vec::new(),
            valid: true,
        }
    }
}

/// Every operator's signature: those of the types' constraints and
/// conversions, then the built-ins', which can name the prelude's types,
/// then each file's operators and selectors, the prelude's first, each
/// with its parameters and result type checked.
/// Built-ins declared outside the prelude are reported. `main` is checked
/// as an update operator, even with a result type, which is reported when
/// the entry point is.
pub(crate) fn signatures(
    files: &Files,
    namespaces: &Namespaces,
    errors: &mut Vec<Diagnostic>,
) -> Operators {
    let mut list = Vec::new();
    for signature in numbered(files, namespaces) {
        assert_eq!(
            signature.id.0,
            list.len(),
            "constraints and conversions are numbered first, in order"
        );
        list.push(signature);
    }
    crate::builtins::operators(files.prelude(), namespaces, &mut list, errors);
    for (source, item) in files.items() {
        match item {
            Item::Operator(op) => {
                let id = hir::OperatorId(list.len());
                list.extend(operator(op, id, source, namespaces, errors));
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
            Item::Type(decl) => {
                for signature in from_type(&decl, source, namespaces) {
                    list.push(Signature {
                        id: hir::OperatorId(list.len()),
                        ..signature
                    });
                }
            }
            Item::Variable(_) | Item::Heading(_) => {}
        }
    }
    families(&mut list, namespaces, errors);
    Operators::new(list)
}

/// The operators in `list` written in trendy, each by its place in it, in
/// sets of one name and the same parameters' names. A built-in's
/// duplicate is in none, as it is one already.
fn sets(list: &[Signature]) -> Vec<Vec<usize>> {
    let mut sets: BTreeMap<(String, Vec<String>), Vec<usize>> = BTreeMap::new();
    for (i, signature) in list.iter().enumerate() {
        if signature.decl.is_some()
            && signature.from_type.is_none()
            && signature.family == Family::Own
        {
            let names = signature.params.keys().cloned().collect();
            sets.entry((signature.name.clone(), names))
                .or_default()
                .push(i);
        }
    }
    sets.into_values().collect()
}

/// Each set of the operators in `list` written in trendy, the prelude's and
/// the program's, of one name and the same parameters' names, checked as a
/// specification and its versions (*Type Inheritance and Relational
/// Theory*, IM Prescription 17): the one whose parameters' types are
/// supertypes of all the others', and the others, each with parameters of
/// subtypes of those, which a call of it computes for arguments of them.
/// One with nothing to be a version of, or another with the same types, is
/// a duplicate; a version unlike its specification is reported, as is an
/// operator without a body that has no versions, or a combination of its
/// union types' subtypes that none is for, or two are, neither more
/// specific than the other.
fn families(list: &mut [Signature], namespaces: &Namespaces, errors: &mut Vec<Diagnostic>) {
    for members in sets(list) {
        let covers = |spec: &Signature, other: &Signature| {
            other.params.iter().all(|(name, p)| {
                spec.params
                    .get(name)
                    .is_some_and(|s| p.ty.is_subtype_of(&s.ty))
            })
        };
        let same = |a: &Signature, b: &Signature| covers(a, b) && covers(b, a);
        let spec = members
            .iter()
            .copied()
            .find(|&i| members.iter().all(|&j| covers(&list[i], &list[j])));
        let Some(spec) = spec else {
            // None is a specification of the others: versions of one not
            // declared, if the parameters' types have common supertypes,
            // and otherwise each after the first is a duplicate of it.
            let specified = specification_of(&members, list);
            for &later in &members[1..] {
                match &specified {
                    Some(specified) => {
                        errors.push(errors::unspecified_versions(
                            &list[later],
                            &list[members[0]],
                            specified,
                        ));
                        list[later].family = Family::Duplicate;
                        list[later].valid = false;
                    }
                    None => duplicate(list, members[0], later, errors),
                }
            }
            continue;
        };
        let mut versions: Vec<usize> = Vec::new();
        for &member in &members {
            if member == spec {
                continue;
            }
            if let Some(&earlier) = std::iter::once(&spec)
                .chain(&versions)
                .find(|&&e| same(&list[e], &list[member]))
            {
                duplicate(list, earlier, member, errors);
                continue;
            }
            if let Some(error) = errors::unlike_version(&list[member], &list[spec]) {
                errors.push(error);
                list[member].valid = false;
            }
            list[member].family = Family::Version(list[spec].id);
            versions.push(member);
        }
        // Each before those it is more specific than: its parameters' types
        // are as deep in their hierarchies or deeper, and one is deeper.
        let depth = |s: &Signature| -> usize {
            s.params
                .values()
                .map(|p| match &p.ty {
                    hir::Type::Defined(d) => d.supertypes.len() + 1,
                    hir::Type::Dummy(d) => d.supertypes.len(),
                    _ => 0,
                })
                .sum()
        };
        versions.sort_by_key(|&v| std::cmp::Reverse(depth(&list[v])));
        let made: Vec<hir::Version> = versions
            .iter()
            .map(|&v| hir::Version {
                operator: list[v].id,
                params: list[v]
                    .params
                    .iter()
                    .filter(|(name, p)| list[spec].params[*name].ty != p.ty)
                    .map(|(name, p)| (name.clone(), p.ty.clone()))
                    .collect(),
            })
            .collect();
        list[spec].versions = made;
        if list[spec].versions.is_empty() {
            continue;
        }
        if let Some(error) = uncovered(&list[spec], list, namespaces) {
            errors.push(error);
            list[spec].valid = false;
        }
    }
    // An operator without a body and without versions computes nothing.
    for signature in list.iter_mut() {
        let bodiless = signature.decl.as_ref().is_some_and(|d| d.body().is_none());
        if bodiless && signature.versions.is_empty() && signature.family == Family::Own {
            if let Some(decl) = &signature.decl {
                errors.push(errors::bodiless_operator(decl));
            }
            signature.valid = false;
        }
    }
}

/// The specification `members`, of `list`, would be versions of, written
/// as a definition: each parameter of the most specific common supertype
/// of theirs, if they have one, and the result too, if they have results;
/// `None` if a parameter's types have none, or they are all the same.
fn specification_of(members: &[usize], list: &[Signature]) -> Option<String> {
    let first = &list[members[0]];
    let mut written = Vec::new();
    let mut wider = false;
    for (name, param) in &first.params {
        let mut common = param.ty.clone();
        for &member in &members[1..] {
            common = common.common_supertype(&list[member].params.get(name)?.ty)?;
        }
        wider |= members.iter().any(|&m| list[m].params[name].ty != common);
        written.push(format!("{name}: {common}"));
    }
    if !wider {
        return None;
    }
    let result = match &first.result {
        Some(result) => {
            let mut common = result.clone();
            for &member in &members[1..] {
                common = common.common_supertype(list[member].result.as_ref()?)?;
            }
            format!(": {common}")
        }
        None => String::new(),
    };
    Some(format!(
        "operator {}{{{}}}{result};",
        first.name,
        written.join(", ")
    ))
}

/// Report `later`, of `list`, as a duplicate of `first`, which it has the
/// same name, parameters' names and types as.
fn duplicate(list: &mut [Signature], first: usize, later: usize, errors: &mut Vec<Diagnostic>) {
    let ident = list[later]
        .decl
        .as_ref()
        .and_then(ast::Operator::name)
        .and_then(|n| n.ident());
    let at = list[first]
        .decl
        .as_ref()
        .and_then(ast::Operator::name)
        .and_then(|n| n.ident());
    if let (Some(ident), Some(at)) = (ident, at) {
        let params: BTreeSet<String> = list[later].written.keys().cloned().collect();
        errors.push(errors::duplicate_operator(
            ident.text(),
            trendy_syntax::span(ident.text_range()),
            (crate::Kind::Operator, trendy_syntax::span(at.text_range())),
            &params,
        ));
    }
    list[later].family = Family::Duplicate;
    list[later].valid = false;
}

/// The error for the specification `spec`, of `list`, if a combination of
/// the types with components under its versions' parameters' union types
/// has no version for it and it has no body, or two, neither more specific
/// than the other.
fn uncovered(spec: &Signature, list: &[Signature], namespaces: &Namespaces) -> Option<Diagnostic> {
    // The parameters some version is narrower for, and the types each can
    // be of.
    let dispatched: BTreeSet<&String> =
        spec.versions.iter().flat_map(|v| v.params.keys()).collect();
    let mut combinations: Vec<BTreeMap<String, hir::Type>> = vec![BTreeMap::new()];
    for name in &dispatched {
        let kinds = leaves(&spec.params[*name].ty, namespaces);
        combinations = combinations
            .into_iter()
            .flat_map(|so_far| {
                kinds.iter().map(move |kind| {
                    let mut more = so_far.clone();
                    more.insert((*name).clone(), kind.clone());
                    more
                })
            })
            .collect();
    }
    let bodiless = spec.decl.as_ref().is_some_and(|d| d.body().is_none());
    let fits = |version: &hir::Version, combination: &BTreeMap<String, hir::Type>| {
        version
            .params
            .iter()
            .all(|(name, ty)| combination[name].is_subtype_of(ty))
    };
    let narrower = |a: &hir::Version, b: &hir::Version| {
        b.params.iter().all(|(name, ty)| {
            a.params
                .get(name)
                .map_or(&spec.params[name].ty, |t| t)
                .is_subtype_of(ty)
        })
    };
    for combination in &combinations {
        let fitting: Vec<&hir::Version> = spec
            .versions
            .iter()
            .filter(|v| fits(v, combination))
            .collect();
        if fitting.is_empty() {
            if bodiless {
                return Some(errors::missing_version(spec, combination, namespaces));
            }
            continue;
        }
        let narrowest = fitting
            .iter()
            .find(|a| fitting.iter().all(|b| narrower(a, b)));
        if narrowest.is_none() {
            let [first, second, ..] = fitting.as_slice() else {
                unreachable!("two fit, neither narrower");
            };
            let name = |v: &hir::Version| list[v.operator.0].decl.as_ref();
            return Some(errors::ambiguous_versions(
                spec,
                combination,
                (name(first), name(second)),
            ));
        }
    }
    None
}

/// The types with components a value of `ty` can be of: those under it, if
/// it is a union type, and otherwise itself.
fn leaves(ty: &hir::Type, namespaces: &Namespaces) -> Vec<hir::Type> {
    match ty {
        hir::Type::Dummy(dummy) => namespaces
            .subtypes(&dummy.name)
            .iter()
            .flat_map(|subtype| leaves(subtype, namespaces))
            .collect(),
        other => vec![other.clone()],
    }
}

/// The signature of `op`, the program's operator, whose id is `id`, with
/// its parameters and result type checked, unless it isn't its name's
/// first declaration, which is reported already.
fn operator(
    op: ast::Operator,
    id: hir::OperatorId,
    source: Source,
    namespaces: &Namespaces,
    errors: &mut Vec<Diagnostic>,
) -> Option<Signature> {
    let ident = op.name().and_then(|n| n.ident())?;
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
    // A built-in has no versions, so one with a built-in's name and its
    // parameters' names, reported with the names, is never called.
    let names = crate::param_names(op.params().as_ref());
    let duplicate = namespaces
        .operator_with(ident.text(), &names)
        .is_some_and(|(kind, _)| kind == Kind::Builtin);
    Some(Signature {
        id,
        name: ident.text().to_string(),
        valid: declared_valid && params_valid && result_valid && determined && !duplicate,
        types: type_parameters,
        written: written(&seen),
        params: seen,
        unresolved,
        read_only: result_written.is_some(),
        result,
        decl: Some(op),
        builtin: None,
        source,
        from_type: None,
        family: if duplicate {
            Family::Duplicate
        } else {
            Family::Own
        },
        versions: Vec::new(),
    })
}

/// The signatures of the constraints of the types `files` declare, and of
/// their possible representations' conversions to the stored ones and
/// from them, in the order of the ids [`crate::named_types::resolve`] gave
/// them: read-only operators, each named for its type and its place among
/// them, or the representation, a name no source can write or call. A
/// constraint's parameters are the stored components, and its value a
/// boolean; a conversion's parameters are the components of the
/// representation it converts from, and its value the tuple of those of
/// the one it converts to.
fn numbered(files: &Files, namespaces: &Namespaces) -> Vec<Signature> {
    let mut signatures = Vec::new();
    for (source, item) in files.items() {
        let Item::Type(decl) = item else {
            continue;
        };
        let Some(ident) = decl.name().and_then(|n| n.ident()) else {
            continue;
        };
        let Some((_, Some(defined))) = namespaces.defined(ident.text()) else {
            continue;
        };
        if !namespaces.is_first_type(&ident) {
            continue;
        }
        let from_type = |made| FromType {
            defined: defined.clone(),
            decl: decl.clone(),
            made,
        };
        let params = component_parameters(&defined.components);
        for (i, (constraint, written)) in defined
            .own_constraints()
            .iter()
            .zip(decl.constraints())
            .enumerate()
        {
            signatures.push(Signature {
                id: constraint.operator,
                name: format!("{} constraint {}", defined.name, i + 1),
                types: Vec::new(),
                written: self::written(&params),
                params: params.clone(),
                unresolved: BTreeSet::new(),
                read_only: true,
                result: Some(hir::Type::Scalar(hir::Scalar::Boolean)),
                decl: None,
                builtin: None,
                source,
                from_type: Some(from_type(Made::Constraint(written))),
                family: Family::Own,
                versions: Vec::new(),
                valid: true,
            });
        }
        let converted = namespaces.converted(&defined.name);
        for (representation, converted) in defined.own_representations().iter().zip(converted) {
            let own = component_parameters(&converted.components);
            let names: Vec<&str> = converted.components.named().map(|(c, _)| c).collect();
            let conversion =
                |id, name, params: BTreeMap<String, hir::Parameter>, result, made| Signature {
                    id,
                    name,
                    types: Vec::new(),
                    written: self::written(&params),
                    params,
                    unresolved: BTreeSet::new(),
                    read_only: true,
                    result: Some(hir::Type::Tuple(result)),
                    decl: None,
                    builtin: None,
                    source,
                    from_type: Some(from_type(made)),
                    family: Family::Own,
                    versions: Vec::new(),
                    valid: converted.convertible,
                };
            signatures.push(conversion(
                representation.to,
                format!("{} from {{{}}}", defined.name, names.join(", ")),
                own,
                defined.components.clone(),
                Made::To(converted.decl.clone()),
            ));
            signatures.push(conversion(
                representation.from,
                format!("{} as {{{}}}", defined.name, names.join(", ")),
                params.clone(),
                converted.components.clone(),
                Made::From(converted.decl.clone()),
            ));
        }
    }
    signatures.sort_by_key(|signature| signature.id);
    signatures
}

/// The parameters of an operator of `components`, a possible
/// representation's: each a constant of its component's type.
fn component_parameters(components: &hir::Heading) -> BTreeMap<String, hir::Parameter> {
    components
        .attributes()
        .map(|(name, ty)| (name.to_string(), hir::Parameter::constant(ty.clone())))
        .collect()
}

/// The selectors of the type `decl` declares, each with no id yet: one for
/// each possible representation, named after the type, whose parameters
/// are its components, unless its name is another operator's, which is
/// reported. None for a type whose stored components have errors, and
/// those of a representation with errors in its conversions, which are
/// reported, are invalid.
fn from_type(decl: &ast::TypeDecl, source: Source, namespaces: &Namespaces) -> Vec<Signature> {
    let Some(ident) = decl.name().and_then(|n| n.ident()) else {
        return Vec::new();
    };
    let name = ident.text();
    let Some((_, Some(defined))) = namespaces.defined(name) else {
        return Vec::new();
    };
    if !namespaces.is_first_type(&ident) {
        return Vec::new();
    }
    let selects = matches!(namespaces.operator(name), Some((Kind::Selector, _)));
    let made = |made: Made, params: BTreeMap<String, hir::Parameter>, result, valid| Signature {
        id: hir::OperatorId(0),
        name: name.to_string(),
        types: Vec::new(),
        written: written(&params),
        params,
        unresolved: BTreeSet::new(),
        read_only: true,
        result: Some(result),
        decl: None,
        builtin: None,
        source,
        from_type: Some(FromType {
            defined: defined.clone(),
            decl: decl.clone(),
            made,
        }),
        valid,
        family: Family::Own,
        versions: Vec::new(),
    };
    let mut signatures = Vec::new();
    if let (true, Some(stored)) = (selects, crate::named_types::stored(decl)) {
        let selector = made(
            Made::Selector(stored),
            component_parameters(&defined.components),
            hir::Type::Defined(Box::new(defined.clone())),
            true,
        );
        signatures.push(selector);
    }
    for converted in namespaces.converted(name) {
        let own = component_parameters(&converted.components);
        if selects {
            let selector = made(
                Made::Selector(converted.decl.clone()),
                own.clone(),
                hir::Type::Defined(Box::new(defined.clone())),
                converted.convertible,
            );
            signatures.push(selector);
        }
    }
    signatures
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
