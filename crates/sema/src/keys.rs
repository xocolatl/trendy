//! A relation variable's keys (TTM RM Prescription 15), and those of a
//! type's relation-valued components: each a set of the relation's
//! attributes, unique and irreducible, which its value must satisfy.

use std::collections::{BTreeMap, BTreeSet};

use trendy_diag::{Diagnostic, Span};
use trendy_hir as hir;
use trendy_syntax::ast::{self, AstNode, Expr, Key, TypeDecl, VarDecl};

use crate::check::Names;
use crate::types::{self, Expected, Origin};
use crate::{errors, ops};

/// The keys of the variable `decl`, named `name`, of type `ty`, whose
/// initial value is `init`, if it is known when compiling: those written,
/// or the whole heading when none is, and none for a variable that isn't a
/// relation. `None` after reporting a key that isn't valid, or that `init`
/// doesn't satisfy.
pub(crate) fn keys(
    decl: &VarDecl,
    name: &str,
    ty: &hir::Type,
    init: Option<&hir::Value>,
    scope: &mut dyn Names,
    errors: &mut Vec<Diagnostic>,
) -> Option<Vec<hir::Key>> {
    let written: Vec<Key> = decl.keys().collect();
    if written.is_empty() {
        return Some(match ty {
            hir::Type::Relation(heading) => {
                vec![heading.attributes().map(|(a, _)| a.to_string()).collect()]
            }
            _ => Vec::new(),
        });
    }
    let (irreducible, mut ok) = checked(&written, errors::Keyed::Variable(name), ty, errors);
    if !matches!(ty, hir::Type::Relation(_)) {
        return None;
    }
    let relation = init.map(|init| match init {
        hir::Value::Relation(relation) => relation,
        _ => unreachable!("a relation variable's value is a relation"),
    });
    if let (Some(relation), Some(value)) = (relation, decl.initializer()) {
        ok &= satisfied(name, &irreducible, relation, &value, scope, errors);
    }
    ok.then(|| irreducible.into_iter().map(|(_, a)| a).collect())
}

/// The keys `written` of the relation, of type `ty`, that `keyed` names:
/// each that lists attributes it has and is irreducible, with its
/// attributes, and whether every one is. Each that isn't is reported, as
/// are keys of what isn't a relation, which can't have any.
pub(crate) fn checked(
    written: &[Key],
    keyed: errors::Keyed,
    ty: &hir::Type,
    errors: &mut Vec<Diagnostic>,
) -> (Vec<(Key, BTreeSet<String>)>, bool) {
    let hir::Type::Relation(heading) = ty else {
        errors.push(errors::key_not_relation(keyed, ty, written));
        return (Vec::new(), false);
    };
    let name = keyed.name();
    let attributes: Vec<String> = heading
        .attributes()
        .map(|(a, _)| format!("`{a}`"))
        .collect();
    let context = if attributes.is_empty() {
        format!("`{name}` has no attributes, so its only possible key is `{{}}`")
    } else {
        format!(
            "the attributes of `{name}` are {}",
            trendy_diag::conjoin(&attributes)
        )
    };
    let mut keys = Vec::new();
    let mut ok = true;
    for key in written {
        match key
            .names()
            .and_then(|list| ops::listed(&list, heading, errors::Noun::Key, &context, errors))
        {
            Some(attributes) => keys.push((key.clone(), attributes)),
            None => ok = false,
        }
    }

    // A key declared again, or containing another, is redundant; the first
    // of equal keys is the one kept.
    let mut irreducible = Vec::new();
    for (i, (key, attributes)) in keys.iter().enumerate() {
        let smaller = keys.iter().enumerate().find(|&(j, (_, other))| {
            (other == attributes && j < i) || (other.is_subset(attributes) && other != attributes)
        });
        match smaller {
            Some((_, (other, others))) => {
                errors.push(errors::redundant_key(key, attributes, other, others));
                ok = false;
            }
            None => irreducible.push((key.clone(), attributes.clone())),
        }
    }
    (irreducible, ok)
}

/// The keys of the relation-valued components of the type named `ty`,
/// which `decl` declares, each of whose possible representations is in
/// `representations` with its components resolved, by the component's
/// name: those written after each component's type, checked as a
/// variable's are (see [`checked`]). A key written among what the type says
/// of itself is reported, with a fix that moves it after the one component
/// it can be a key of, if there is one.
pub(crate) fn components(
    decl: &TypeDecl,
    ty: &str,
    representations: &[(ast::Representation, &hir::Heading)],
    errors: &mut Vec<Diagnostic>,
) -> BTreeMap<String, Vec<hir::Key>> {
    let mut keys = BTreeMap::new();
    // Each relation-valued component, with its attributes and where its
    // type, and its keys, end.
    let mut relations = Vec::new();
    for (representation, heading) in representations {
        let Some(components) = representation.components() else {
            continue;
        };
        for component in components.attributes() {
            let Some(ident) = component.name().and_then(|n| n.ident()) else {
                continue;
            };
            let name = ident.text().to_string();
            let Some(component_ty) = heading.get(&name) else {
                continue;
            };
            if let hir::Type::Relation(inner) = component_ty {
                let end = trendy_syntax::span(component.syntax().text_range()).end;
                relations.push((name.clone(), inner.clone(), end));
            }
            let written: Vec<Key> = component.keys().collect();
            if written.is_empty() {
                continue;
            }
            let keyed = errors::Keyed::Component {
                ty,
                component: &name,
            };
            let (checked, _) = checked(&written, keyed, component_ty, errors);
            if !checked.is_empty() {
                keys.insert(name, checked.into_iter().map(|(_, a)| a).collect());
            }
        }
    }
    for key in decl.misplaced_keys() {
        let names: Vec<String> = key
            .names()
            .map(|list| {
                list.names()
                    .filter_map(|n| Some(n.ident()?.text().to_string()))
                    .collect()
            })
            .unwrap_or_default();
        let candidates: Vec<(&str, usize)> = relations
            .iter()
            .filter(|(_, inner, _)| names.iter().all(|n| inner.get(n).is_some()))
            .map(|(name, _, end)| (name.as_str(), *end))
            .collect();
        let text = format!("key {{ {} }}", names.join(", "));
        let text = if names.is_empty() {
            "key {}".to_string()
        } else {
            text
        };
        errors.push(errors::misplaced_key(&key, &text, ty, &candidates));
    }
    keys
}

/// The keys written in `decl` that are among `keys`, each with its
/// attributes: those a value assigned to the variable must satisfy, since
/// the key of the whole heading always is.
pub(crate) fn written(decl: &VarDecl, keys: &[hir::Key]) -> Vec<(Key, BTreeSet<String>)> {
    decl.keys()
        .filter_map(|key| {
            let attributes: BTreeSet<String> = key
                .names()?
                .names()
                .filter_map(|n| Some(n.ident()?.text().to_string()))
                .collect();
            keys.contains(&attributes).then_some((key, attributes))
        })
        .collect()
}

/// Whether `relation`, the value of the variable `name`, written `value`,
/// satisfies each of `keys`, after reporting each it violates, at the
/// tuples that share a key's values when `value` is a relation selector.
pub(crate) fn satisfied(
    name: &str,
    keys: &[(Key, BTreeSet<String>)],
    relation: &hir::RelationValue,
    value: &Expr,
    scope: &mut dyn Names,
    errors: &mut Vec<Diagnostic>,
) -> bool {
    let mut ok = true;
    let mut places = None;
    for (key, attributes) in keys {
        for (shared, [earlier, tuple]) in sharing(relation, attributes) {
            let places =
                places.get_or_insert_with(|| tuple_places(value, &relation.heading, scope));
            let place = |t: &hir::TupleValue| places.iter().find(|(v, _)| v == t).map(|&(_, s)| s);
            let (at, first) = match (place(earlier), place(tuple)) {
                (Some(a), Some(b)) => (a.max(b), Some(a.min(b))),
                _ => (value.span(), None),
            };
            errors.push(errors::key_violated(
                errors::Keyed::Variable(name),
                Some(key.span()),
                (&shared, [earlier, tuple]),
                at,
                first,
            ));
            ok = false;
        }
    }
    ok
}

/// The tuples of `relation` that share their values for the key
/// `attributes` with an earlier one, each with those values and the
/// earlier one, the first it shares them with.
pub(crate) fn sharing<'r>(
    relation: &'r hir::RelationValue,
    attributes: &BTreeSet<String>,
) -> Vec<(hir::TupleValue, [&'r hir::TupleValue; 2])> {
    let mut seen: BTreeMap<hir::TupleValue, &hir::TupleValue> = BTreeMap::new();
    let mut shared = Vec::new();
    for tuple in &relation.tuples {
        let value = key_value(tuple, attributes);
        match seen.get(&value) {
            Some(&earlier) => shared.push((value, [earlier, tuple])),
            None => {
                seen.insert(value, tuple);
            }
        }
    }
    shared
}

/// Where the key `attributes` of the component `component` of `defined` is
/// written, found through the type's selectors in `operators`, whose
/// declaration it is in.
pub(crate) fn written_in(
    defined: &hir::Defined,
    component: &str,
    attributes: &BTreeSet<String>,
    operators: &crate::operators::Operators,
) -> Option<Span> {
    let selector = operators.selector(defined, component)?;
    let decl = &operators.get(selector).from_type.as_ref()?.decl;
    decl.representations()
        .filter_map(|r| r.components())
        .flat_map(|components| components.attributes().collect::<Vec<_>>())
        .filter(|c| c.is_named(component))
        .flat_map(|c| c.keys().collect::<Vec<_>>())
        .find(|key| {
            let names: Option<BTreeSet<String>> = key.names().map(|list| {
                list.names()
                    .filter_map(|n| Some(n.ident()?.text().to_string()))
                    .collect()
            });
            names.as_ref() == Some(attributes)
        })
        .map(|key| key.span())
}

/// `tuple`'s value for the key `attributes`: the tuple of just those.
fn key_value(tuple: &hir::TupleValue, attributes: &BTreeSet<String>) -> hir::TupleValue {
    hir::TupleValue::new(
        tuple
            .attributes()
            .filter(|(a, _)| attributes.contains(*a))
            .map(|(a, v)| (a.to_string(), v.clone()))
            .collect(),
    )
}

/// Where each tuple of `value` is written, when it is a relation selector
/// of constants, so that a key violation can point at the tuples. The
/// value was already checked, so each tuple evaluates again without error;
/// this is only done once a key is found violated.
fn tuple_places(
    value: &Expr,
    heading: &hir::Heading,
    scope: &mut dyn Names,
) -> Vec<(hir::TupleValue, Span)> {
    let Expr::Relation(relation) = value else {
        return Vec::new();
    };
    let expected = Expected {
        ty: hir::Type::Tuple(heading.clone()),
        origin: Origin::Value(Expr::Relation(relation.clone())),
    };
    relation
        .elements()
        .filter_map(|element| {
            let (value, _) = types::value(&element, Some(&expected), scope, &mut Vec::new())?;
            match value {
                hir::Value::Tuple(value) => Some((value, element.span())),
                _ => None,
            }
        })
        .collect()
}
