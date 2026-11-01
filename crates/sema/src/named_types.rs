//! The headings and types declared with names: each one resolved once,
//! after those it names, which can be declared in any order. One that
//! contains itself, directly or through others, is reported, and so is a
//! union type that contains itself through its subtypes, or is a subtype
//! of itself.

use std::collections::{HashMap, HashSet};

use trendy_diag::{Diagnostic, Span};
use trendy_hir as hir;
use trendy_syntax::ast::{self, AstNode, HeadingDecl, HeadingRef, TypeDecl, TypeRef};

use crate::check::{self, Representation};
use crate::errors;
use crate::types::{self, TypeParameters};
use crate::{Converted, Files, Namespaces};

/// A heading's or a type's declaration, and the declared headings and
/// types it contains.
struct Declared {
    decl: Decl,
    names: Vec<Contained>,
}

/// A heading or a type that another names: its name, where, and how.
struct Contained {
    name: String,
    at: Span,
    how: Named,
}

/// How a heading or a type names another, which then resolves first.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Named {
    /// As the type of one of its attributes or components.
    Holds,
    /// A union type, as one of its subtypes, whose values are among its
    /// own.
    Subtype,
    /// A subtype defined by a constraint, as its supertype, whose
    /// components its values have.
    Specializes,
}

/// A declaration with a name in the types' namespace.
enum Decl {
    Heading(HeadingDecl),
    Type(TypeDecl),
}

impl Decl {
    /// What it declares, for errors.
    fn what(&self) -> &'static str {
        match self {
            Decl::Heading(_) => "heading",
            Decl::Type(_) => "type",
        }
    }

    /// The attributes it declares, or its type's components, as its values
    /// are stored.
    fn heading(&self) -> Option<ast::Heading> {
        match self {
            Decl::Heading(decl) => decl.heading(),
            Decl::Type(decl) => stored(decl)?.components(),
        }
    }

    /// Every heading in it: a heading's attributes, or each of a type's
    /// possible representations' components, which a union type has none
    /// of, as its errors say.
    fn headings(&self) -> Vec<ast::Heading> {
        match self {
            Decl::Heading(decl) => decl.heading().into_iter().collect(),
            Decl::Type(decl) if decl.union_keyword().is_some() => Vec::new(),
            Decl::Type(decl) => decl
                .representations()
                .filter_map(|r| r.components())
                .collect(),
        }
    }
}

/// The possible representation of the type `decl` declares that its values
/// are stored as: the first without conversions, which the others convert
/// to and from.
pub(crate) fn stored(decl: &TypeDecl) -> Option<ast::Representation> {
    decl.representations()
        .find(|r| r.conversions().next().is_none())
}

/// The names of `representation`'s components, as written.
fn component_names(representation: &ast::Representation) -> Vec<(String, Span)> {
    representation
        .components()
        .map(|heading| {
            heading
                .attributes()
                .filter_map(|a| a.name()?.ident())
                .map(|i| (i.text().to_string(), trendy_syntax::span(i.text_range())))
                .collect()
        })
        .unwrap_or_default()
}

/// Whether `conversion` gives the component `name` the value of the
/// component of the same name, `name := name`.
fn carries(conversion: &ast::Conversion, name: &str) -> bool {
    conversion.assignments().is_some_and(|list| {
        list.assignments().any(|assign| {
            let assigned = assign.name().and_then(|n| n.ident());
            let value = match assign.value() {
                Some(ast::Expr::Name(value)) => value.ident(),
                _ => None,
            };
            assigned.is_some_and(|a| a.text() == name) && value.is_some_and(|v| v.text() == name)
        })
    })
}

/// The names `conversion` assigns.
fn assigned_names(conversion: &ast::Conversion) -> Vec<String> {
    conversion
        .assignments()
        .map(|list| {
            list.assignments()
                .filter_map(|a| Some(a.name()?.ident()?.text().to_string()))
                .collect()
        })
        .unwrap_or_default()
}

/// The names of the components a type's representations convert to and
/// from, and, of a subtype defined by constraints, what it inherits.
struct Names<'n> {
    stored: &'n [(String, Span)],
    inherited: Option<&'n Inherited<'n>>,
}

/// Whether `representation`, of the type `name`, with the conversions `to`
/// and `from`, converts the right way round, and has components of its
/// own, none of those `seen` so far, after reporting why not: a subtype's
/// representation can have its supertype's component, the same one, if it
/// carries it unchanged both ways, as TTM's circle's `ctr` is its
/// ellipse's.
fn both_ways(
    name: &str,
    representation: &ast::Representation,
    (to, from): (&ast::Conversion, &ast::Conversion),
    names: &Names,
    seen: &mut Vec<(String, Span)>,
    errors: &mut Vec<Diagnostic>,
) -> bool {
    let mut ok = true;
    let own = component_names(representation);
    let has = |list: &[(String, Span)], name: &str| list.iter().any(|(n, _)| n == name);
    let (to_names, from_names) = (assigned_names(to), assigned_names(from));
    // Only when no name is both representations'.
    let swapped = !to_names.is_empty()
        && !from_names.is_empty()
        && to_names
            .iter()
            .all(|n| has(&own, n) && !has(names.stored, n))
        && from_names
            .iter()
            .all(|n| has(names.stored, n) && !has(&own, n));
    if swapped {
        errors.push(errors::swapped_conversions(to, from));
        ok = false;
    }
    for (component, span) in own {
        let inherited = names.inherited.filter(|i| has(&i.taken, &component));
        if inherited.is_some() && carries(to, &component) && carries(from, &component) {
            continue;
        }
        if let Some((_, first)) = seen.iter().find(|(n, _)| *n == component) {
            errors.push(errors::component_in_two(
                name,
                &component,
                (span, *first),
                inherited.map(|i| i.supertype),
            ));
            ok = false;
        } else {
            seen.push((component, span));
        }
    }
    ok
}

/// The stored components a subtype defined by constraints converts its
/// own representations to and from, its supertype's, and the components of
/// all its supertypes' representations, which none of its own may have,
/// each with where it is written; and the supertype, by name.
struct Inherited<'s> {
    stored: Vec<(String, Span)>,
    taken: Vec<(String, Span)>,
    supertype: &'s str,
}

/// Whether each of the possible representations of the type `decl`
/// declares, named `name`, in order, but the stored one, can be converted
/// to and from the stored one, after reporting why one can't: exactly one
/// is stored, without conversions, or, of a subtype defined by constraints,
/// `inherited`, none, and each other has `to` and `from`, once each, the
/// right way round; and no component is in two.
fn convertible(
    decl: &TypeDecl,
    name: &str,
    inherited: Option<&Inherited>,
    errors: &mut Vec<Diagnostic>,
) -> Vec<bool> {
    let representations: Vec<ast::Representation> = decl.representations().collect();
    let stored = representations
        .iter()
        .position(|r| r.conversions().next().is_none());
    let (stored_names, mut seen) = match (inherited, stored) {
        (Some(inherited), _) => (inherited.stored.clone(), inherited.taken.clone()),
        (None, Some(stored)) => {
            let names = component_names(&representations[stored]);
            (names.clone(), names)
        }
        (None, None) => {
            if let Some(ident) = decl.name().and_then(|n| n.ident()) {
                errors.push(errors::no_stored_representation(name, &ident));
            }
            return vec![false; representations.len()];
        }
    };
    representations
        .iter()
        .enumerate()
        .map(|(i, representation)| {
            if inherited.is_none() && Some(i) == stored {
                return false;
            }
            let conversions: Vec<ast::Conversion> = representation.conversions().collect();
            let mut ok = true;
            match (inherited, stored) {
                (Some(inherited), _) if conversions.is_empty() => {
                    errors.push(errors::specialized_stored(
                        name,
                        representation,
                        inherited.supertype,
                    ));
                    ok = false;
                }
                (None, Some(stored)) if conversions.is_empty() => {
                    errors.push(errors::second_stored(
                        name,
                        representation,
                        &representations[stored],
                    ));
                    ok = false;
                }
                _ => {}
            }
            let mut to = None;
            let mut from = None;
            for conversion in &conversions {
                let first = match conversion.direction() {
                    Some(ast::Direction::To) => &mut to,
                    Some(ast::Direction::From) => &mut from,
                    None => continue,
                };
                match first {
                    Some(first) => errors.push(errors::repeated_conversion(conversion, first)),
                    None => *first = Some(conversion.clone()),
                }
            }
            match (&to, &from) {
                (Some(to), Some(from)) => {
                    let names = Names {
                        stored: &stored_names,
                        inherited,
                    };
                    ok &= both_ways(name, representation, (to, from), &names, &mut seen, errors);
                }
                _ if conversions.is_empty() => {}
                _ => {
                    errors.push(errors::missing_conversion(representation, to.is_none()));
                    ok = false;
                }
            }
            ok
        })
        .collect()
}

/// A heading or a type in a cycle: its name, what it is, where it is
/// declared, and where and how it names the next one, which the last names
/// the first of.
pub(crate) struct Cycled {
    pub(crate) name: String,
    pub(crate) what: &'static str,
    pub(crate) declared: Span,
    pub(crate) via: Span,
    pub(crate) how: Named,
}

/// The type each type is a subtype of, by name: the one it names after
/// `is`, and where it names it, if that is a union type, or a regular type,
/// which it is then defined as a subtype of by a constraint, and neither is
/// in a cycle of types that are subtypes of each other, which are left
/// out.
struct Supertypes {
    immediate: HashMap<String, (String, Span)>,
    /// Those whose immediate supertype is a regular type.
    constrained: HashSet<String>,
    cyclic: HashSet<String>,
}

impl Supertypes {
    /// What `name` names as its supertype or subtypes, as it is resolved
    /// after them: of a union type, its immediate subtypes, as it contains
    /// them, and of a subtype defined by a constraint, its supertype, whose
    /// components it has.
    fn named<'s>(&'s self, name: &'s str) -> impl Iterator<Item = Contained> + 's {
        let subtypes = self
            .immediate
            .iter()
            .filter(move |(subtype, (supertype, _))| {
                supertype == name && !self.constrained.contains(*subtype)
            })
            .map(|(subtype, (_, at))| Contained {
                name: subtype.clone(),
                at: *at,
                how: Named::Subtype,
            });
        let supertype = self
            .immediate
            .get(name)
            .filter(|_| self.constrained.contains(name))
            .map(|(supertype, at)| Contained {
                name: supertype.clone(),
                at: *at,
                how: Named::Specializes,
            });
        subtypes.chain(supertype)
    }

    /// Whether a type is defined as a subtype of `name` by a constraint.
    fn specialized(&self, name: &str) -> bool {
        self.immediate.iter().any(|(subtype, (supertype, _))| {
            supertype == name && self.constrained.contains(subtype)
        })
    }

    /// The regular type `name` is defined as a subtype of by a constraint,
    /// if it is one.
    fn specializes(&self, name: &str) -> Option<&str> {
        self.immediate
            .get(name)
            .filter(|_| self.constrained.contains(name))
            .map(|(supertype, _)| supertype.as_str())
    }

    /// The types `name` is a subtype of, its immediate supertype first, and
    /// so on up.
    fn of(&self, name: &str) -> Vec<String> {
        let mut chain = Vec::new();
        let mut at = name;
        while let Some((supertype, _)) = self.immediate.get(at) {
            chain.push(supertype.clone());
            at = supertype;
        }
        chain
    }
}

/// The supertypes of the types `declared`, by name, after reporting each
/// that is neither a union type nor a regular type of the program's, each
/// union type with representations or with fewer than two subtypes, each
/// subtype of a regular type that isn't defined by a constraint alone, and
/// each cycle of types that are subtypes of each other, which are left
/// out.
fn supertypes(
    declared: &[(String, Declared)],
    namespaces: &Namespaces,
    errors: &mut Vec<Diagnostic>,
) -> Supertypes {
    let union_decl = |name: &str| {
        declared.iter().find_map(|(n, d)| match &d.decl {
            Decl::Type(decl) if n == name && decl.union_keyword().is_some() => Some(decl),
            _ => None,
        })
    };
    let mut immediate: HashMap<String, (String, Span)> = HashMap::new();
    let mut constrained = HashSet::new();
    for (name, declared) in declared {
        let Decl::Type(decl) = &declared.decl else {
            continue;
        };
        if let (Some(_), Some(body)) = (decl.union_keyword(), decl.body()) {
            errors.push(errors::union_with_body(name, &body));
        }
        let Some(ident) = decl
            .supertype()
            .and_then(|s| s.type_ref())
            .and_then(|r| r.ident())
        else {
            continue;
        };
        let supertype = ident.text();
        let at = trendy_syntax::span(ident.text_range());
        if union_decl(supertype).is_some() {
            immediate.insert(name.clone(), (supertype.to_string(), at));
        } else if namespaces.defined(supertype).is_some() {
            // A subtype of a regular type, defined by its constraints: a
            // union type has no components for them to constrain.
            // One with other errors, reported, is a subtype all the same if
            // it has a constraint, so that its constraints read the
            // supertype's components.
            if decl.union_keyword().is_some() {
                errors.push(errors::union_under_regular(name, &ident));
            } else if specialized(decl, name, supertype, errors)
                || decl.constraints().next().is_some()
            {
                immediate.insert(name.clone(), (supertype.to_string(), at));
                constrained.insert(name.clone());
            }
        } else if let Some((declared, _)) = namespaces.heading(supertype) {
            errors.push(errors::heading_as_type(&ident, declared));
        } else if hir::Scalar::from_name(supertype).is_some() {
            errors.push(errors::built_in_supertype(&ident));
        } else {
            errors.push(errors::unknown_type(
                &ident,
                namespaces,
                &TypeParameters::None,
            ));
        }
    }
    // Union types that are subtypes of each other, each cycle reported once
    // and left out.
    let mut cyclic: HashSet<String> = HashSet::new();
    for (name, _) in declared {
        let mut path: Vec<&str> = Vec::new();
        let mut at = name.as_str();
        while let Some((supertype, _)) = immediate.get(at) {
            if cyclic.contains(at) {
                break;
            }
            path.push(at);
            if let Some(start) = path.iter().position(|p| p == supertype) {
                let cycle: Vec<Cycled> = path[start..]
                    .iter()
                    .map(|&n| Cycled {
                        name: n.to_string(),
                        what: "type",
                        declared: namespaces.defined(n).expect("a declared type").0,
                        via: immediate[n].1,
                        how: Named::Holds,
                    })
                    .collect();
                errors.push(errors::cyclic_supertypes(&cycle));
                cyclic.extend(cycle.into_iter().map(|c| c.name));
                break;
            }
            at = supertype;
        }
    }
    immediate.retain(|name, _| !cyclic.contains(name));
    constrained.retain(|name| !cyclic.contains(name));
    let supertypes = Supertypes {
        immediate,
        constrained,
        cyclic,
    };
    lone_subtypes(declared, &supertypes, errors);
    supertypes
}

/// Report each union type among `declared` with fewer than two subtypes,
/// of `supertypes`: one alone would have all its values, and none none.
fn lone_subtypes(
    declared: &[(String, Declared)],
    supertypes: &Supertypes,
    errors: &mut Vec<Diagnostic>,
) {
    for (name, union) in declared {
        let Decl::Type(decl) = &union.decl else {
            continue;
        };
        if decl.union_keyword().is_none() || supertypes.cyclic.contains(name) {
            continue;
        }
        let subtypes: Vec<(String, Span)> = declared
            .iter()
            .filter_map(|(n, _)| {
                let (supertype, at) = supertypes.immediate.get(n)?;
                (supertype == name).then(|| (n.clone(), *at))
            })
            .collect();
        if subtypes.len() < 2
            && let Some(ident) = decl.name().and_then(|n| n.ident())
        {
            errors.push(errors::lone_subtype(&ident, &subtypes));
        }
    }
}

/// What the subtype `name` defined by constraints, if it is one, of those
/// `declared`, inherits from its supertypes, as `supertypes` say, for its
/// representations' conversions (see [`Inherited`]).
fn inherited<'s>(
    name: &str,
    declared: &[(String, Declared)],
    supertypes: &'s Supertypes,
) -> Option<Inherited<'s>> {
    let supertype = supertypes.specializes(name)?;
    let decl = |n: &str| {
        declared
            .iter()
            .find_map(|(d, declared)| match &declared.decl {
                Decl::Type(decl) if d == n => Some(decl.clone()),
                _ => None,
            })
    };
    let mut taken = Vec::new();
    let mut at = supertype;
    loop {
        let decl = decl(at)?;
        for representation in decl.representations() {
            taken.extend(component_names(&representation));
        }
        if let Some(next) = supertypes.specializes(at) {
            at = next;
        } else {
            let stored = stored(&decl)
                .map(|r| component_names(&r))
                .unwrap_or_default();
            return Some(Inherited {
                stored,
                taken,
                supertype,
            });
        }
    }
}

/// Whether `decl`, which declares the type `name` a subtype of the regular
/// type `supertype`, defines it by constraints: at least one, which says
/// which of the supertype's values are its own, and no order of its own,
/// since its values are the supertype's, ordered as they are; otherwise
/// reported.
fn specialized(decl: &TypeDecl, name: &str, supertype: &str, errors: &mut Vec<Diagnostic>) -> bool {
    let mut ok = true;
    if decl.constraints().next().is_none() {
        errors.push(errors::unconstrained_subtype(decl, name, supertype));
        ok = false;
    }
    for order in decl.orders() {
        errors.push(errors::specialized_order(&order, name, supertype));
        ok = false;
    }
    ok
}

/// Resolve the attributes of each heading `files` declare, and the
/// components, possible representations and constraints of each type, into
/// `namespaces`, reporting each cycle among them, and every error in their
/// attributes. Each constraint, and each representation's conversion to the
/// stored one, is an operator, numbered from `first` in the order the types
/// resolve, a type's constraints first, which
/// [`crate::operators::signatures`] makes.
pub(crate) fn resolve(
    files: &Files,
    namespaces: &mut Namespaces,
    first: hir::OperatorId,
    errors: &mut Vec<Diagnostic>,
) {
    let mut declared = declarations(files, namespaces);
    let supertypes = supertypes(&declared, namespaces, errors);
    // Which representations convert, reported once for each type but a
    // union type, whose representations are an error: of a subtype defined
    // by a constraint, to and from its supertype's stored components.
    let convertible: HashMap<String, Vec<bool>> = declared
        .iter()
        .filter_map(|(name, declared_type)| match &declared_type.decl {
            Decl::Type(decl) if decl.union_keyword().is_none() => {
                let inherited = inherited(name, &declared, &supertypes);
                Some((
                    name.clone(),
                    convertible(decl, name, inherited.as_ref(), errors),
                ))
            }
            _ => None,
        })
        .collect();
    // A union type contains its subtypes, as their values are its own, and
    // a subtype defined by a constraint has its supertype's components.
    for (name, declared) in &mut declared {
        declared.names.extend(supertypes.named(name));
    }
    let by_name: HashMap<&str, &Declared> = declared.iter().map(|(n, d)| (n.as_str(), d)).collect();

    // Each after those it names, and the cycles found on the way.
    let mut order = Search::default();
    for (name, _) in &declared {
        order.visit(name, &by_name, namespaces, errors);
    }
    // Each constraint's and conversion's operator, numbered in the order
    // the types resolve.
    let mut next = first.0;
    for name in &order.finished {
        let declared = by_name[name.as_str()];
        if let Decl::Type(decl) = &declared.decl
            && decl.union_keyword().is_some()
        {
            // One in a cycle, of either kind, is left out.
            if !order.cyclic.contains(name) && !supertypes.cyclic.contains(name) {
                let dummy = hir::Dummy {
                    name: name.clone(),
                    supertypes: supertypes.of(name),
                };
                namespaces.dummies.insert(name.clone(), dummy);
            }
            continue;
        }
        // A subtype defined by a constraint has its supertype's components,
        // resolved first.
        if let (Decl::Type(decl), Some(supertype)) = (&declared.decl, supertypes.specializes(name))
        {
            if let Some(supertype) = namespaces.defined.get(supertype).cloned()
                && !order.cyclic.contains(name)
            {
                let placed = Placed {
                    name,
                    supertypes: supertypes.of(name),
                    specialized: supertypes.specialized(name),
                };
                let convertible = &convertible[name.as_str()];
                specialization(
                    decl,
                    placed,
                    supertype,
                    convertible,
                    &mut next,
                    namespaces,
                    errors,
                );
            }
            continue;
        }
        let Some(heading) = declared.decl.heading() else {
            continue;
        };
        // One in a cycle has no attributes, but its others' errors are
        // reported all the same.
        let resolved =
            types::resolve_heading(&heading, &mut TypeParameters::None, namespaces, errors)
                .filter(|_| !order.cyclic.contains(name));
        let Some(resolved) = resolved else {
            continue;
        };
        match &declared.decl {
            Decl::Heading(_) => {
                namespaces.attributes.insert(name.clone(), resolved);
            }
            Decl::Type(decl) => {
                let convertible = &convertible[name.as_str()];
                let resolved = Resolved {
                    placed: Placed {
                        name,
                        supertypes: supertypes.of(name),
                        specialized: supertypes.specialized(name),
                    },
                    components: resolved,
                    convertible,
                };
                defined(decl, resolved, &mut next, namespaces, errors);
            }
        }
    }
    unions(&declared, &supertypes, namespaces);
}

/// The headings and types `files` declare, each the first of its name, by
/// name, with what it names.
fn declarations(files: &Files, namespaces: &Namespaces) -> Vec<(String, Declared)> {
    let headings = files.headings().filter_map(|decl| {
        let ident = decl.name()?.ident()?;
        namespaces
            .is_first_heading(&ident)
            .then(|| (ident.text().to_string(), Decl::Heading(decl)))
    });
    let types = files.types().filter_map(|decl| {
        let ident = decl.name()?.ident()?;
        namespaces
            .is_first_type(&ident)
            .then(|| (ident.text().to_string(), Decl::Type(decl)))
    });
    headings
        .chain(types)
        .map(|(name, decl)| {
            let names = decl
                .headings()
                .iter()
                .flat_map(|h| named(h, namespaces))
                .collect();
            (name, Declared { decl, names })
        })
        .collect()
}

/// Each union type's subtypes, of those `declared`, into `namespaces`, in
/// the order they are declared, but those with errors.
fn unions(declared: &[(String, Declared)], supertypes: &Supertypes, namespaces: &mut Namespaces) {
    for (name, _) in declared {
        let Some((supertype, _)) = supertypes.immediate.get(name) else {
            continue;
        };
        let subtype = match namespaces.defined.get(name) {
            Some(defined) => hir::Type::Defined(Box::new(defined.clone())),
            None => match namespaces.dummies.get(name) {
                Some(dummy) => hir::Type::Dummy(Box::new(dummy.clone())),
                None => continue,
            },
        };
        namespaces
            .subtypes
            .entry(supertype.clone())
            .or_default()
            .push(subtype);
    }
}

/// A type of the program's own, by name, with its place in its hierarchy:
/// its supertypes, and whether a type is defined as a subtype of it by a
/// constraint.
struct Placed<'r> {
    name: &'r str,
    supertypes: Vec<String>,
    specialized: bool,
}

/// A type whose stored components are resolved, `components`, and, of its
/// possible representations, whether each converts.
struct Resolved<'r> {
    placed: Placed<'r>,
    components: hir::Heading,
    convertible: &'r [bool],
}

/// The type `decl` declares, `placed`, a subtype of the regular type
/// `supertype` defined by its constraints (*Type Inheritance and
/// Relational Theory*, IM Prescription 10), into `namespaces`: its values
/// are the supertype's of which its constraints are true, so its
/// components, representations, keys and order are the supertype's, and
/// its constraints the supertype's, then its own, each an operator
/// numbered from `next` on.
fn specialization(
    decl: &TypeDecl,
    placed: Placed,
    supertype: hir::Defined,
    convertible: &[bool],
    next: &mut usize,
    namespaces: &mut Namespaces,
    errors: &mut Vec<Diagnostic>,
) {
    let mut numbered = || {
        let id = hir::OperatorId(*next);
        *next += 1;
        id
    };
    let mut constraints = supertype.constraints.clone();
    constraints.extend(decl.constraints().map(|constraint| {
        hir::Constraint {
            operator: numbered(),
            text: constraint
                .condition()
                .map_or_else(String::new, |c| c.syntax().text().to_string()),
        }
    }));
    let converted = converted(decl, convertible, namespaces, errors);
    let mut representations = supertype.representations.clone();
    representations.extend(converted.iter().map(|c| hir::Representation {
        components: c.components.clone(),
        to: numbered(),
        from: numbered(),
    }));
    let defined = hir::Defined {
        name: placed.name.to_string(),
        components: supertype.components.clone(),
        representations,
        constraints,
        keys: supertype.keys.clone(),
        order: supertype.order.clone(),
        supertypes: placed.supertypes,
        specialized: placed.specialized,
        specialization: Some(hir::Specialization {
            supertype: Box::new(supertype),
        }),
    };
    namespaces.defined.insert(placed.name.to_string(), defined);
    namespaces
        .converted
        .insert(placed.name.to_string(), converted);
}

/// The possible representations `decl` declares with conversions, each
/// with its components resolved, and whether `convertible` says it
/// converts, by its position; those whose components have errors are left
/// out.
fn converted(
    decl: &TypeDecl,
    convertible: &[bool],
    namespaces: &Namespaces,
    errors: &mut Vec<Diagnostic>,
) -> Vec<Converted> {
    decl.representations()
        .zip(convertible)
        .filter(|(r, _)| r.conversions().next().is_some())
        .filter_map(|(representation, &convertible)| {
            let heading = representation.components()?;
            let components =
                types::resolve_heading(&heading, &mut TypeParameters::None, namespaces, errors)?;
            Some(Converted {
                decl: representation,
                components,
                convertible,
            })
        })
        .collect()
}

/// The type `decl` declares, `resolved`, into `namespaces`: its
/// representations but the stored one, with the types their components
/// name resolved by now, its constraints and its order. Each constraint,
/// then each representation's conversions to the stored one and from it,
/// is an operator numbered from `next` on.
fn defined(
    decl: &TypeDecl,
    resolved: Resolved,
    next: &mut usize,
    namespaces: &mut Namespaces,
    errors: &mut Vec<Diagnostic>,
) {
    let Resolved {
        placed:
            Placed {
                name,
                supertypes,
                specialized,
            },
        components,
        convertible,
    } = resolved;
    let mut numbered = || {
        let id = hir::OperatorId(*next);
        *next += 1;
        id
    };
    let constraints = decl
        .constraints()
        .map(|constraint| hir::Constraint {
            operator: numbered(),
            text: constraint
                .condition()
                .map_or_else(String::new, |c| c.syntax().text().to_string()),
        })
        .collect();
    let converted = converted(decl, convertible, namespaces, errors);
    let representations = converted
        .iter()
        .map(|c| hir::Representation {
            components: c.components.clone(),
            to: numbered(),
            from: numbered(),
        })
        .collect();
    // Each representation, with its components resolved: the stored one's
    // are the type's.
    let resolved: Vec<(ast::Representation, &hir::Heading)> = stored(decl)
        .map(|r| (r, &components))
        .into_iter()
        .chain(converted.iter().map(|c| (c.decl.clone(), &c.components)))
        .collect();
    let keys = crate::keys::components(decl, name, &resolved, errors);
    let mut defined = hir::Defined {
        name: name.to_string(),
        components,
        representations,
        constraints,
        keys,
        order: None,
        supertypes,
        specialized,
        specialization: None,
    };
    defined.order = type_order(decl, &defined, errors);
    namespaces.defined.insert(name.to_string(), defined);
    namespaces.converted.insert(name.to_string(), converted);
}

/// The order `decl` gives its type, `defined`, if it gives it one, or
/// `None` after reporting its errors: a type has one, which compares the
/// components of the possible representation those it names are of.
fn type_order(
    decl: &TypeDecl,
    defined: &hir::Defined,
    errors: &mut Vec<Diagnostic>,
) -> Option<hir::Order> {
    let mut orders = decl.orders();
    let first = orders.next()?;
    for again in orders {
        errors.push(errors::second_order(&defined.name, &again, &first));
    }
    let list = first.list()?;
    let idents: Vec<_> = list
        .items()
        .filter_map(|item| item.attribute()?.ident())
        .collect();
    let named: Vec<(&str, Span)> = idents
        .iter()
        .map(|ident| (ident.text(), trendy_syntax::span(ident.text_range())))
        .collect();
    let rule = errors::OneRepresentation::Ordered;
    let (components, from) = match check::representation(defined, &named, rule, errors)? {
        Representation::Stored { .. } => (&defined.components, None),
        Representation::Other(at) => {
            let other = &defined.representations[at];
            (&other.components, Some(other.from))
        }
    };
    let orders = errors::Orders::Type(defined);
    let items = crate::assign::order_items(&list, components, orders, errors)?;
    Some(hir::Order { from, items })
}

/// The declared headings, after `relation` or `tuple`, and types that
/// `heading`'s attributes name, each with where.
fn named(heading: &ast::Heading, namespaces: &Namespaces) -> Vec<Contained> {
    heading
        .syntax()
        .descendants()
        .filter_map(|node| {
            let ident = HeadingRef::cast(node.clone())
                .and_then(|r| r.ident())
                .filter(|i| namespaces.heading(i.text()).is_some())
                .or_else(|| {
                    TypeRef::cast(node.clone())
                        .and_then(|r| r.ident())
                        .filter(|i| namespaces.defined(i.text()).is_some())
                })?;
            Some(Contained {
                name: ident.text().to_string(),
                at: trendy_syntax::span(node.text_range()),
                how: Named::Holds,
            })
        })
        .collect()
}

/// A depth-first search of the headings and types, through the ones each
/// names.
#[derive(Default)]
struct Search {
    /// The ones being visited, each with where and how it names the next.
    path: Vec<(String, Option<(Span, Named)>)>,
    /// The ones visited, each after those it names.
    finished: Vec<String>,
    /// The ones in a cycle.
    cyclic: HashSet<String>,
}

impl Search {
    fn visit(
        &mut self,
        name: &str,
        by_name: &HashMap<&str, &Declared>,
        namespaces: &Namespaces,
        errors: &mut Vec<Diagnostic>,
    ) {
        if self.finished.iter().any(|f| f == name) {
            return;
        }
        if let Some(start) = self.path.iter().position(|(n, _)| n == name) {
            let cycle: Vec<Cycled> = self.path[start..]
                .iter()
                .map(|(name, via)| {
                    let (via, how) = via.expect("one in the path names the next");
                    Cycled {
                        name: name.clone(),
                        what: by_name[name.as_str()].decl.what(),
                        declared: namespaces
                            .heading(name)
                            .map(|(span, _)| span)
                            .or_else(|| namespaces.defined(name).map(|(span, _)| span))
                            .expect("a declared heading or type"),
                        via,
                        how,
                    }
                })
                .collect();
            self.cyclic.extend(cycle.iter().map(|c| c.name.clone()));
            errors.push(errors::cyclic_types(&cycle));
            return;
        }
        self.path.push((name.to_string(), None));
        for next in &by_name[name].names {
            self.path.last_mut().expect("just pushed").1 = Some((next.at, next.how));
            self.visit(&next.name, by_name, namespaces, errors);
        }
        self.path.pop();
        self.finished.push(name.to_string());
    }
}
