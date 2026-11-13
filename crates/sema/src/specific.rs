//! The most specific types a value can have, which dispatch finds a version
//! of an operator for (*Type Inheritance and Relational Theory*, IM
//! Prescriptions 17 and 27).

use std::collections::BTreeMap;
use std::fmt;

use trendy_hir as hir;

use crate::Namespaces;

/// A most specific type a value can have: a type written in trendy, or, of
/// a value made of others, one made of theirs.
#[derive(Clone, PartialEq)]
pub(crate) enum Specific {
    /// A value of the type and of none of its proper subtypes.
    Of(hir::Type),
    /// A tuple whose attributes' values are most specifically of these.
    Tuple(BTreeMap<String, Specific>),
    /// A relation with tuples: each attribute's is the most specific common
    /// supertype of its values' over the tuples.
    Relation(BTreeMap<String, Specific>),
    /// An array with elements: the most specific common supertype of theirs.
    Array(Box<Specific>),
    /// An empty relation or array, of the type: with no tuples or elements
    /// to be of any other type, it is of every relation or array type of its
    /// heading or elements, so of a type no type written in trendy is.
    Empty(hir::Type),
}

impl Specific {
    /// The most specific types a value of `ty` can have: those of its
    /// subtypes, and its own, unless it is a union type, which has no
    /// values of its own; of a tuple, each combination of its attributes';
    /// and of a relation or an array, each combination of the most specific
    /// common supertypes of its attributes' or elements', and the empty
    /// one's, last.
    pub(crate) fn all(ty: &hir::Type, namespaces: &Namespaces) -> Vec<Specific> {
        let under = |name: &str| {
            namespaces
                .subtypes(name)
                .iter()
                .flat_map(|subtype| Specific::all(subtype, namespaces))
                .collect::<Vec<_>>()
        };
        match ty {
            hir::Type::Dummy(dummy) => under(&dummy.name),
            hir::Type::Defined(defined) => {
                let mut all = vec![Specific::Of(ty.clone())];
                all.extend(under(&defined.name));
                all
            }
            hir::Type::Tuple(heading) => combinations(
                heading
                    .attributes()
                    .map(|(name, ty)| (name, Specific::all(ty, namespaces))),
            )
            .into_iter()
            .map(Specific::Tuple)
            .collect(),
            hir::Type::Relation(heading) => {
                let mut all: Vec<Specific> = combinations(
                    heading
                        .attributes()
                        .map(|(name, ty)| (name, closed(Specific::all(ty, namespaces)))),
                )
                .into_iter()
                .map(Specific::Relation)
                .collect();
                all.push(Specific::Empty(ty.clone()));
                all
            }
            hir::Type::Array(element) => {
                let mut all: Vec<Specific> = closed(Specific::all(element, namespaces))
                    .into_iter()
                    .map(|element| Specific::Array(Box::new(element)))
                    .collect();
                all.push(Specific::Empty(ty.clone()));
                all
            }
            _ => vec![Specific::Of(ty.clone())],
        }
    }

    /// Whether a value of this most specific type is of `ty`.
    pub(crate) fn is_of(&self, ty: &hir::Type) -> bool {
        match (self, ty) {
            (Specific::Of(own), _) => own.is_subtype_of(ty),
            (Specific::Tuple(own), hir::Type::Tuple(heading))
            | (Specific::Relation(own), hir::Type::Relation(heading)) => own
                .iter()
                .all(|(name, own)| heading.get(name).is_some_and(|ty| own.is_of(ty))),
            (Specific::Array(own), hir::Type::Array(element)) => own.is_of(element),
            (Specific::Empty(own), _) => matches!(
                (own, ty),
                (hir::Type::Relation(_), hir::Type::Relation(_))
                    | (hir::Type::Array(_), hir::Type::Array(_))
            ),
            _ => false,
        }
    }

    /// What is empty in a value of this most specific type, `"relation"` or
    /// `"array"`, if an empty relation or array is in it.
    pub(crate) fn empty(&self) -> Option<&'static str> {
        match self {
            Specific::Of(_) => None,
            Specific::Tuple(own) | Specific::Relation(own) => {
                own.values().find_map(Specific::empty)
            }
            Specific::Array(own) => own.empty(),
            Specific::Empty(hir::Type::Array(_)) => Some("array"),
            Specific::Empty(_) => Some("relation"),
        }
    }

    /// The most specific type that values of this one and of `other` are
    /// both of, if any: of an empty relation or array, and another, the
    /// other.
    fn common(&self, other: &Specific) -> Option<Specific> {
        match (self, other) {
            (Specific::Of(a), Specific::Of(b)) => a.common_supertype(b).map(Specific::Of),
            (Specific::Tuple(a), Specific::Tuple(b)) => {
                common_attributes(a, b).map(Specific::Tuple)
            }
            (Specific::Relation(a), Specific::Relation(b)) => {
                common_attributes(a, b).map(Specific::Relation)
            }
            (Specific::Array(a), Specific::Array(b)) => a
                .common(b)
                .map(|element| Specific::Array(Box::new(element))),
            (Specific::Empty(_), other) | (other, Specific::Empty(_)) => Some(other.clone()),
            _ => None,
        }
    }
}

/// As trendy writes the type, and an empty relation or array as trendy
/// writes the value: `relation { a: integer } {}`, `array integer []`.
impl fmt::Display for Specific {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let attributes = |f: &mut fmt::Formatter<'_>, own: &BTreeMap<String, Specific>| {
            if own.is_empty() {
                return f.write_str("{}");
            }
            let written: Vec<String> = own
                .iter()
                .map(|(name, ty)| format!("{name}: {ty}"))
                .collect();
            write!(f, "{{ {} }}", written.join(", "))
        };
        match self {
            Specific::Of(ty) => write!(f, "{ty}"),
            Specific::Tuple(own) => {
                f.write_str("tuple ")?;
                attributes(f, own)
            }
            Specific::Relation(own) => {
                f.write_str("relation ")?;
                attributes(f, own)
            }
            Specific::Array(own) => write!(f, "array {own}"),
            Specific::Empty(ty @ hir::Type::Array(_)) => write!(f, "{ty} []"),
            Specific::Empty(ty) => write!(f, "{ty} {{}}"),
        }
    }
}

/// Each combination of one of each name's most specific types in `kinds`.
pub(crate) fn combinations<'a>(
    kinds: impl IntoIterator<Item = (&'a str, Vec<Specific>)>,
) -> Vec<BTreeMap<String, Specific>> {
    let mut combinations = vec![BTreeMap::new()];
    for (name, kinds) in kinds {
        combinations = combinations
            .into_iter()
            .flat_map(|so_far| {
                kinds.iter().map(move |kind| {
                    let mut more = so_far.clone();
                    more.insert(name.to_string(), kind.clone());
                    more
                })
            })
            .collect();
    }
    combinations
}

/// `kinds`, and the most specific common supertype of each set of them: the
/// most specific types the values of a relation's attribute, or an array's
/// elements, can have together.
fn closed(mut kinds: Vec<Specific>) -> Vec<Specific> {
    let mut next = 0;
    while next < kinds.len() {
        for earlier in 0..next {
            if let Some(common) = kinds[next].common(&kinds[earlier])
                && !kinds.contains(&common)
            {
                kinds.push(common);
            }
        }
        next += 1;
    }
    kinds
}

/// The most specific common supertypes of `a`'s and `b`'s attributes, each
/// of the same name, if each has one.
fn common_attributes(
    a: &BTreeMap<String, Specific>,
    b: &BTreeMap<String, Specific>,
) -> Option<BTreeMap<String, Specific>> {
    a.iter()
        .map(|(name, ty)| Some((name.clone(), ty.common(b.get(name)?)?)))
        .collect()
}
