//! The headings and types declared with names: each one resolved once,
//! after those it names, which can be declared in any order. One that
//! contains itself, directly or through others, is reported.

use std::collections::{HashMap, HashSet};

use trendy_diag::{Diagnostic, Span};
use trendy_hir as hir;
use trendy_syntax::ast::{self, AstNode, HeadingDecl, HeadingRef, SourceFile, TypeDecl, TypeRef};

use crate::errors;
use crate::types::{self, TypeParameters};
use crate::{Converted, Namespaces};

/// A heading's or a type's declaration, and the declared headings and
/// types it names, each with where.
struct Declared {
    decl: Decl,
    names: Vec<(String, Span)>,
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
    /// possible representations' components.
    fn headings(&self) -> Vec<ast::Heading> {
        match self {
            Decl::Heading(decl) => decl.heading().into_iter().collect(),
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

/// Whether each of the possible representations of the type `decl`
/// declares, named `name`, in order, but the stored one, can be converted
/// to and from the stored one, after reporting why one can't: exactly one
/// is stored, without conversions, and each other has `to` and `from`,
/// once each, the right way round; and no component is in two.
fn convertible(decl: &TypeDecl, name: &str, errors: &mut Vec<Diagnostic>) -> Vec<bool> {
    let representations: Vec<ast::Representation> = decl.representations().collect();
    let stored = representations
        .iter()
        .position(|r| r.conversions().next().is_none());
    let Some(stored) = stored else {
        if let Some(ident) = decl.name().and_then(|n| n.ident()) {
            errors.push(errors::no_stored_representation(name, &ident));
        }
        return vec![false; representations.len()];
    };
    let stored_names = component_names(&representations[stored]);
    let mut seen: Vec<(String, Span)> = stored_names.clone();
    representations
        .iter()
        .enumerate()
        .map(|(i, representation)| {
            if i == stored {
                return false;
            }
            let conversions: Vec<ast::Conversion> = representation.conversions().collect();
            let mut ok = true;
            if conversions.is_empty() {
                errors.push(errors::second_stored(
                    name,
                    representation,
                    &representations[stored],
                ));
                ok = false;
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
                    let own = component_names(representation);
                    let has =
                        |list: &[(String, Span)], name: &str| list.iter().any(|(n, _)| n == name);
                    let (to_names, from_names) = (assigned_names(to), assigned_names(from));
                    // Only when no name is both representations'.
                    let swapped = !to_names.is_empty()
                        && !from_names.is_empty()
                        && to_names
                            .iter()
                            .all(|n| has(&own, n) && !has(&stored_names, n))
                        && from_names
                            .iter()
                            .all(|n| has(&stored_names, n) && !has(&own, n));
                    if swapped {
                        errors.push(errors::swapped_conversions(to, from));
                        ok = false;
                    }
                    for (component, span) in own {
                        if let Some((_, first)) = seen.iter().find(|(n, _)| *n == component) {
                            errors.push(errors::component_in_two(name, &component, span, *first));
                            ok = false;
                        } else {
                            seen.push((component, span));
                        }
                    }
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
/// declared, and where it names the next one, which the last names the
/// first of.
pub(crate) struct Cycled {
    pub(crate) name: String,
    pub(crate) what: &'static str,
    pub(crate) declared: Span,
    pub(crate) via: Span,
}

/// Resolve the attributes of each heading `file` declares, and the
/// components and constraints of each type, into `namespaces`, reporting
/// each cycle among them, and every error in their attributes. Each
/// constraint is an operator, numbered from `first_constraint` in the order
/// the types resolve, which [`crate::operators::signatures`] makes.
pub(crate) fn resolve(
    file: &SourceFile,
    namespaces: &mut Namespaces,
    first_constraint: hir::OperatorId,
    errors: &mut Vec<Diagnostic>,
) {
    let headings = file.headings().filter_map(|decl| {
        let ident = decl.name()?.ident()?;
        namespaces
            .is_first_heading(&ident)
            .then(|| (ident.text().to_string(), Decl::Heading(decl)))
    });
    let types = file.types().filter_map(|decl| {
        let ident = decl.name()?.ident()?;
        namespaces
            .is_first_type(&ident)
            .then(|| (ident.text().to_string(), Decl::Type(decl)))
    });
    let declared: Vec<(String, Declared)> = headings
        .chain(types)
        .map(|(name, decl)| {
            let names = decl
                .headings()
                .iter()
                .flat_map(|h| named(h, namespaces))
                .collect();
            (name, Declared { decl, names })
        })
        .collect();
    // Which representations convert, reported once for each type.
    let convertible: HashMap<&str, Vec<bool>> = declared
        .iter()
        .filter_map(|(name, declared)| match &declared.decl {
            Decl::Type(decl) => Some((name.as_str(), convertible(decl, name, errors))),
            Decl::Heading(_) => None,
        })
        .collect();
    let by_name: HashMap<&str, &Declared> = declared.iter().map(|(n, d)| (n.as_str(), d)).collect();

    // Each after those it names, and the cycles found on the way.
    let mut order = Search::default();
    for (name, _) in &declared {
        order.visit(name, &by_name, namespaces, errors);
    }
    // Each constraint's operator, numbered in the order the types resolve.
    let mut next = first_constraint.0;
    for name in &order.finished {
        let declared = by_name[name.as_str()];
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
                let mut constraints = Vec::new();
                for constraint in decl.constraints() {
                    let text = constraint
                        .condition()
                        .map_or_else(String::new, |c| c.syntax().text().to_string());
                    constraints.push(hir::Constraint {
                        operator: hir::OperatorId(next),
                        text,
                    });
                    next += 1;
                }
                let order = type_order(decl, name, &resolved, errors);
                let defined = hir::Defined {
                    name: name.clone(),
                    components: resolved,
                    constraints,
                    order,
                };
                namespaces.defined.insert(name.clone(), defined);
                // The others, with the types their components name
                // resolved by now.
                let converted: Vec<Converted> = decl
                    .representations()
                    .zip(&convertible[name.as_str()])
                    .filter(|(r, _)| r.conversions().next().is_some())
                    .filter_map(|(representation, &convertible)| {
                        let heading = representation.components()?;
                        let components = types::resolve_heading(
                            &heading,
                            &mut TypeParameters::None,
                            namespaces,
                            errors,
                        )?;
                        Some(Converted {
                            decl: representation,
                            components,
                            convertible,
                        })
                    })
                    .collect();
                namespaces.converted.insert(name.clone(), converted);
            }
        }
    }
}

/// The order `decl` gives its type, named `name`, with `components`, if it
/// gives it one, or `None` after reporting its errors: a type has one.
fn type_order(
    decl: &TypeDecl,
    name: &str,
    components: &hir::Heading,
    errors: &mut Vec<Diagnostic>,
) -> Option<Vec<hir::OrderItem>> {
    let mut orders = decl.orders();
    let first = orders.next()?;
    for again in orders {
        errors.push(errors::second_order(name, &again, &first));
    }
    let list = first.list()?;
    crate::assign::order_items(&list, components, errors::Orders::Type(name), errors)
}

/// The declared headings, after `relation` or `tuple`, and types that
/// `heading`'s attributes name, each with where.
fn named(heading: &ast::Heading, namespaces: &Namespaces) -> Vec<(String, Span)> {
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
            Some((
                ident.text().to_string(),
                trendy_syntax::span(node.text_range()),
            ))
        })
        .collect()
}

/// A depth-first search of the headings and types, through the ones each
/// names.
#[derive(Default)]
struct Search {
    /// The ones being visited, each with where it names the next.
    path: Vec<(String, Option<Span>)>,
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
                .map(|(name, via)| Cycled {
                    name: name.clone(),
                    what: by_name[name.as_str()].decl.what(),
                    declared: namespaces
                        .heading(name)
                        .map(|(span, _)| span)
                        .or_else(|| namespaces.defined(name).map(|(span, _)| span))
                        .expect("a declared heading or type"),
                    via: via.expect("one in the path names the next"),
                })
                .collect();
            self.cyclic.extend(cycle.iter().map(|c| c.name.clone()));
            errors.push(errors::cyclic_types(&cycle));
            return;
        }
        self.path.push((name.to_string(), None));
        for (next, via) in &by_name[name].names {
            self.path.last_mut().expect("just pushed").1 = Some(*via);
            self.visit(next, by_name, namespaces, errors);
        }
        self.path.pop();
        self.finished.push(name.to_string());
    }
}
