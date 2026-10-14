//! A relation variable's keys (TTM RM Prescription 15): each a set of its
//! attributes, unique and irreducible, which its value must satisfy.

use std::collections::{BTreeMap, BTreeSet};

use trendy_diag::{Diagnostic, Span};
use trendy_hir as hir;
use trendy_syntax::ast::{Expr, Key, VarDecl};

use crate::check::Names;
use crate::types::{self, Expected, Origin};
use crate::{errors, ops};

/// The keys of the variable `decl`, named `name`, of type `ty`, whose
/// initial value is `init`: those written, or the whole heading when none
/// is, and none for a variable that isn't a relation. `None` after
/// reporting a key that isn't valid, or that `init` doesn't satisfy.
pub(crate) fn keys(
    decl: &VarDecl,
    name: &str,
    ty: &hir::Type,
    init: &hir::Value,
    scope: &mut dyn Names,
    errors: &mut Vec<Diagnostic>,
) -> Option<Vec<hir::Key>> {
    let written: Vec<Key> = decl.keys().collect();
    let hir::Type::Relation(heading) = ty else {
        if written.is_empty() {
            return Some(Vec::new());
        }
        errors.push(errors::key_not_relation(name, ty, &written));
        return None;
    };
    if written.is_empty() {
        return Some(vec![
            heading.attributes().map(|(a, _)| a.to_string()).collect(),
        ]);
    }

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
            Some(attributes) => keys.push((key, attributes)),
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
            None => irreducible.push((key, attributes)),
        }
    }

    let hir::Value::Relation(relation) = init else {
        unreachable!("a relation variable's value is a relation");
    };
    let mut places = None;
    for (key, attributes) in irreducible {
        let mut seen: BTreeMap<hir::TupleValue, &hir::TupleValue> = BTreeMap::new();
        for tuple in &relation.tuples {
            let value = key_value(tuple, attributes);
            let Some(first) = seen.get(&value) else {
                seen.insert(value, tuple);
                continue;
            };
            let places = places.get_or_insert_with(|| tuple_places(decl, heading, scope));
            let place = |t: &hir::TupleValue| places.iter().find(|(v, _)| v == t).map(|&(_, s)| s);
            let (at, first) = match (place(first), place(tuple)) {
                (Some(a), Some(b)) => (a.max(b), Some(a.min(b))),
                _ => (decl.initializer()?.span(), None),
            };
            errors.push(errors::key_violated(name, key, &value, at, first));
            ok = false;
        }
    }

    ok.then(|| keys.into_iter().map(|(_, attributes)| attributes).collect())
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

/// Where each tuple of `decl`'s initial value is written, when it is a
/// relation selector, so that a key violation can point at the tuples. The
/// value was already checked, so each tuple evaluates again without error;
/// this is only done once a key is found violated.
fn tuple_places(
    decl: &VarDecl,
    heading: &hir::Heading,
    scope: &mut dyn Names,
) -> Vec<(hir::TupleValue, Span)> {
    let Some(Expr::Relation(relation)) = decl.initializer() else {
        return Vec::new();
    };
    let expected = Expected {
        ty: hir::Type::Tuple(heading.clone()),
        origin: Origin::Value(Expr::Relation(relation.clone())),
    };
    relation
        .elements()
        .filter_map(|element| {
            let value = types::value(&element, Some(&expected), scope, &mut Vec::new())?;
            match value {
                hir::Value::Tuple(value) => Some((value, element.span())),
                _ => None,
            }
        })
        .collect()
}
