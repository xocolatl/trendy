//! What a restriction restricts below it.
//!
//! A restriction's condition can restrict what the operation it restricts computes from,
//! where that passes on the attributes it reads as they are, so that less is computed, and
//! what finds tuples, as a lookup, sees it; and a restriction of a restriction is one by
//! both conditions. The optimizer rewrites restrictions so, and what narrows a constant
//! ([`crate::adapted`]) sees them as it will.

use std::collections::{BTreeMap, BTreeSet};

use trendy_hir as hir;

use crate::{TypeOf, at_hand};

/// Rewrites restrictions, given the type of an expression where they are.
pub(crate) struct Pushing<'a> {
    pub(crate) ty: &'a TypeOf<'a>,
}

impl Pushing<'_> {
    /// The heading of `expr`, a relation.
    fn heading(&self, expr: &hir::Expr) -> hir::Heading {
        let hir::Type::Relation(heading) = (self.ty)(expr) else {
            unreachable!("only a relation is restricted");
        };
        heading
    }

    /// `expr`, if it is a restriction of what passes the attributes of each tuple of a
    /// relation on as they are, with the conjuncts of its condition that can restrict that
    /// relation instead ([`Pushing::passes`]) restricting it, so that what computes it
    /// computes less, and what finds its tuples, as a lookup, sees them; the rest restrict
    /// what they did. Where what it restricts is computed, its site, stays where it is.
    pub(crate) fn pushed_down(&self, expr: &hir::Expr) -> Option<hir::Expr> {
        let hir::Expr::Restrict(operand, condition) = expr else {
            return None;
        };
        let (sites, inner) = under_sites(operand);
        let (mut moved, mut kept): (Vec<&hir::Expr>, Vec<&hir::Expr>) = condition
            .conjuncts()
            .iter()
            .partition(|conjunct| self.passes(inner, conjunct));
        // One that can fail, alike for every tuple, goes only with all of the condition: what
        // stays could decide that no tuple computes it.
        if !kept.is_empty() {
            let (cannot, can): (Vec<&hir::Expr>, Vec<&hir::Expr>) =
                moved.into_iter().partition(|conjunct| !conjunct.can_fail());
            moved = cannot;
            kept.extend(can);
        }
        if moved.is_empty() {
            return None;
        }
        let moved = hir::Connective::And.of(moved.into_iter().cloned().collect());
        let below = with_sites(&sites, self.restricted_below(inner, moved));
        Some(if kept.is_empty() {
            below
        } else {
            let kept = hir::Connective::And.of(kept.into_iter().cloned().collect());
            hir::Expr::Restrict(Box::new(below), Box::new(kept))
        })
    }

    /// Whether `conjunct`, of a condition restricting `inner`, can restrict what `inner`
    /// computes from instead, with the same result and failing as it would. What it reads,
    /// `inner` passes on as it is: attributes of a renaming, a projection, an extension
    /// whose values can't fail, which it doesn't read, or an ungrouping, but for those
    /// ungrouped; or all of a tuple, of a union, a symmetric difference or a difference, if
    /// each operand has what it reads of the types the result has, which can be its
    /// subtypes, or of a match or a mismatch, whose result's tuples are its left operand's.
    ///
    /// It skips nothing that can fail there, as every operand is computed. If it can fail
    /// itself, it fails for another tuple first in canonical order, which renaming or
    /// extending tuples changes, and for a tuple `inner` drops, so it doesn't go, unless
    /// what fails in it fails alike for every tuple ([`fails_alike`]): then it goes only
    /// where each tuple of what `inner` computes from goes on, through a renaming, a
    /// projection, an extension or a union. Restricting first computes it for tuples `inner`
    /// drops, as a projection does duplicates, or merges, so then only if it is [`cheap`];
    /// but it is computed once for each tuple of what an ungrouping ungroups, rather than
    /// for each it makes.
    fn passes(&self, inner: &hir::Expr, conjunct: &hir::Expr) -> bool {
        let fails = conjunct.can_fail();
        if fails && !fails_alike(conjunct) {
            return false;
        }
        let read = conjunct.tuple_read(0);
        let cheap = cheap(conjunct);
        // Whether `operand`'s tuples have what it reads, of the types the result's have.
        let reads_alike = |operand: &hir::Expr| {
            let (own, result) = (self.heading(operand), self.heading(inner));
            if read.whole {
                own == result
            } else {
                read.attributes
                    .iter()
                    .all(|name| own.get(name) == result.get(name))
            }
        };
        match inner {
            hir::Expr::Rename(..) => !read.whole,
            hir::Expr::Remove(..) => !read.whole && cheap,
            hir::Expr::Extend(operand, _, attributes) => {
                let own = self.heading(operand);
                let merges = attributes.keys().any(|name| own.get(name).is_some());
                !read.whole
                    && attributes.values().all(|value| !value.can_fail())
                    && attributes
                        .keys()
                        .all(|name| !read.attributes.contains(name))
                    && (cheap || !merges)
            }
            hir::Expr::Ungroup(operand, attributes) => {
                let own = self.heading(operand);
                let ungrouped: BTreeSet<&str> = attributes
                    .iter()
                    .filter_map(|name| match own.get(name) {
                        Some(hir::Type::Relation(group)) => Some(group),
                        _ => None,
                    })
                    .flat_map(|group| group.attributes().map(|(name, _)| name))
                    .collect();
                !fails
                    && !read.whole
                    && read
                        .attributes
                        .iter()
                        .all(|name| !ungrouped.contains(name.as_str()))
            }
            hir::Expr::Combine(hir::Combination::Union, lhs, rhs) => {
                cheap && reads_alike(lhs) && reads_alike(rhs)
            }
            hir::Expr::Combine(
                hir::Combination::ExclusiveUnion | hir::Combination::Minus,
                lhs,
                rhs,
            ) => !fails && cheap && reads_alike(lhs) && reads_alike(rhs),
            hir::Expr::Semijoin(..) | hir::Expr::Antijoin(..) => !fails && cheap,
            _ => false,
        }
    }

    /// `inner`, which [`Pushing::passes`] the conjuncts of `condition`, with what it
    /// computes from restricted by them, renamed to what it renames from.
    fn restricted_below(&self, inner: &hir::Expr, condition: hir::Expr) -> hir::Expr {
        let restricted = |operand: &hir::Expr, condition: hir::Expr| {
            let heading = self.heading(operand);
            Box::new(hir::Expr::Restrict(
                Box::new(operand.clone()),
                Box::new(at_hand(condition, 0, &heading)),
            ))
        };
        match inner {
            hir::Expr::Rename(operand, renaming) => {
                let from: BTreeMap<&str, &str> = renaming
                    .iter()
                    .map(|(old, new)| (new.as_str(), old.as_str()))
                    .collect();
                let condition = renamed_at_hand(condition, 0, &from);
                hir::Expr::Rename(restricted(operand, condition), renaming.clone())
            }
            hir::Expr::Remove(operand, removed) => {
                hir::Expr::Remove(restricted(operand, condition), removed.clone())
            }
            hir::Expr::Extend(operand, heading, attributes) => hir::Expr::Extend(
                restricted(operand, condition),
                heading.clone(),
                attributes.clone(),
            ),
            hir::Expr::Ungroup(operand, ungrouped) => {
                hir::Expr::Ungroup(restricted(operand, condition), ungrouped.clone())
            }
            hir::Expr::Combine(combination, lhs, rhs) => hir::Expr::Combine(
                *combination,
                restricted(lhs, condition.clone()),
                restricted(rhs, condition),
            ),
            hir::Expr::Semijoin(lhs, rhs) => {
                hir::Expr::Semijoin(restricted(lhs, condition), rhs.clone())
            }
            hir::Expr::Antijoin(lhs, rhs) => {
                hir::Expr::Antijoin(restricted(lhs, condition), rhs.clone())
            }
            _ => unreachable!("only what passes a conjunct is restricted below"),
        }
    }
}

/// `r where p and q`, of `expr`, if it is `(r where p) where q` and `p` can't fail, which
/// is the same relation, and fails as it does: every operand is computed, so `r where p` is
/// computed for each tuple of `r`, which a `p` that can fail would fail for where `q` decides
/// the `and` without it; but `q` is computed only for the tuples `p` is true of either way,
/// in the same canonical order, as a restriction's tuples have their operand's heading. What
/// is around `r where p`, as where it is computed, is around `r`, as `p` can't fail.
pub(crate) fn restrictions_merged(expr: &hir::Expr) -> Option<hir::Expr> {
    let hir::Expr::Restrict(operand, outer) = expr else {
        return None;
    };
    let (sites, inner) = under_sites(operand);
    let hir::Expr::Restrict(restricted, condition) = inner else {
        return None;
    };
    if condition.can_fail() {
        return None;
    }
    let restricted = with_sites(&sites, (**restricted).clone());
    let condition = hir::Connective::And.of(vec![(**condition).clone(), (**outer).clone()]);
    Some(hir::Expr::Restrict(
        Box::new(restricted),
        Box::new(condition),
    ))
}

/// The sites `expr` is at, outermost first, and what is computed there.
pub(crate) fn under_sites(expr: &hir::Expr) -> (Vec<hir::Site>, &hir::Expr) {
    let mut sites = Vec::new();
    let mut inner = expr;
    while let hir::Expr::At(site, within) = inner {
        sites.push(*site);
        inner = within;
    }
    (sites, inner)
}

/// `expr`, at `sites`, outermost first.
fn with_sites(sites: &[hir::Site], expr: hir::Expr) -> hir::Expr {
    sites
        .iter()
        .rev()
        .fold(expr, |expr, site| hir::Expr::At(*site, Box::new(expr)))
}

/// Whether `expr`, of a condition on the tuples of a relation, fails alike for every tuple
/// that computes it, if it fails: what in it can fail itself reads none of the tuple's
/// attributes, so it fails the same for any tuple, whichever is first to compute it. What
/// it computes for each tuple of a relation of its own, as an aggregate's argument, can't
/// fail at all.
fn fails_alike(expr: &hir::Expr) -> bool {
    let read = expr.tuple_read(0);
    if read.attributes.is_empty() && !read.whole {
        return true;
    }
    if expr.fails_itself() {
        return false;
    }
    let mut alike = true;
    expr.for_each_child(|child, tuples| {
        alike &= if tuples == 0 {
            fails_alike(child)
        } else {
            !child.can_fail()
        };
    });
    alike
}

/// Whether computing `expr` costs about what keeping a tuple does, whatever the values it
/// reads: it computes scalars by the language's own operators, and tuples of them, from
/// what it reads, with no call, which can cost anything, nor a relation or an array, which
/// can be as large as anything.
fn cheap(expr: &hir::Expr) -> bool {
    let own = match expr {
        hir::Expr::Constant(_)
        | hir::Expr::Variable(_)
        | hir::Expr::Parameter(_)
        | hir::Expr::Local(_)
        | hir::Expr::ComputedConstant(..)
        | hir::Expr::Current(..)
        | hir::Expr::Extract(..)
        | hir::Expr::Unary(..)
        | hir::Expr::Connective(..)
        | hir::Expr::If(..)
        | hir::Expr::Tuple(_)
        | hir::Expr::At(..) => true,
        hir::Expr::Binary(op, ..) => !matches!(
            op,
            hir::BinaryOp::ConcatenateArrays
                | hir::BinaryOp::Compare(
                    _,
                    hir::Compared::Relation
                        | hir::Compared::Array { .. }
                        | hir::Compared::Defined { converts: true }
                        | hir::Compared::Parameter(_)
                )
        ),
        _ => false,
    };
    let mut children = true;
    expr.for_each_child(|child, _| children &= cheap(child));
    own && children
}

/// `expr`, which reads attributes of the tuple at hand `depth` of them out from it, with
/// each read under the name `from` gives it, if it gives one, as a condition on a renaming
/// is on what it renames.
fn renamed_at_hand(expr: hir::Expr, depth: usize, from: &BTreeMap<&str, &str>) -> hir::Expr {
    match expr {
        hir::Expr::Extract(tuple, name) if matches!(*tuple, hir::Expr::Current(d, _) if d == depth) =>
        {
            let name = from
                .get(name.as_str())
                .map_or(name, |old| (*old).to_string());
            hir::Expr::Extract(tuple, name)
        }
        expr => expr.map_children(|child, deeper| renamed_at_hand(child, depth + deeper, from)),
    }
}
