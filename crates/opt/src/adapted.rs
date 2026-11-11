//! What narrows a constant read more than once, written both ways.
//!
//! A read of a constant is as if its expression were written there, so what
//! narrows it, as a restriction of it does, can compute only what it needs of
//! it, rather than all of it, computed once and shared. Which costs less
//! depends on how often each read runs, which only running says, so each is
//! written both ways ([`hir::Expr::AdaptedRead`]), for the program to choose as
//! it runs. But only where what narrows it does compute less of it: otherwise
//! the narrowed form computes all of it, and then, if another read needs all
//! of it, the shared form computes it again.

use std::collections::{BTreeSet, HashMap};

use trendy_hir as hir;

use crate::{and_operands, guard_reads, is_and, names, restrictions_merged, without_site};

/// The type of an expression, where it is.
pub(crate) type TypeOf<'a> = dyn Fn(&hir::Expr) -> hir::Type + 'a;

/// Writes what narrows each of these constants both ways.
pub(crate) struct Adapting<'a> {
    /// The value of each constant to adapt, of the program's or a local one,
    /// which takes the place of a read of it where it is narrowed.
    pub(crate) values: &'a HashMap<hir::Adapted, hir::Expr>,
    /// The type of an expression where the reads are, of those values too.
    pub(crate) ty: &'a TypeOf<'a>,
}

impl Adapting<'_> {
    /// `expr`, with what narrows a constant to adapt written both ways
    /// ([`hir::AdaptedRead`]), the outermost of what narrows it, if it reads the
    /// constant once. The shared form reads the others it reads whole, and the
    /// narrowed one adapts them in turn, so that what reads `k` of them is written
    /// `k + 1` ways, not `2^k`.
    pub(crate) fn expr(&self, expr: hir::Expr) -> hir::Expr {
        if let Some(of) = self.narrowed(&expr)
            && self.reads_of(&expr, of) == 1
        {
            let narrowed = self.expr(self.with_value(expr.clone(), of));
            return hir::Expr::AdaptedRead(Box::new(hir::AdaptedRead {
                of,
                shared: expr,
                narrowed,
            }));
        }
        expr.map_children(|child, _| self.expr(child))
    }

    /// The constant to adapt that `expr` reads, if it is a read of one.
    fn read_of(&self, expr: &hir::Expr) -> Option<hir::Adapted> {
        let of = match expr {
            hir::Expr::ComputedConstant(id, _) => hir::Adapted::Constant(*id),
            hir::Expr::Local(id) => hir::Adapted::Local(*id),
            _ => return None,
        };
        self.values.contains_key(&of).then_some(of)
    }

    /// `expr`, with the value of the constant `of` in place of each read of it.
    fn with_value(&self, expr: hir::Expr, of: hir::Adapted) -> hir::Expr {
        if self.read_of(&expr) == Some(of) {
            return self.values[&of].clone();
        }
        expr.map_children(|child, _| self.with_value(child, of))
    }

    /// How many times `expr` reads the constant `of`.
    fn reads_of(&self, expr: &hir::Expr, of: hir::Adapted) -> usize {
        if self.read_of(expr) == Some(of) {
            return 1;
        }
        let mut reads = 0;
        expr.for_each_child(|child, _| reads += self.reads_of(child, of));
        reads
    }

    /// The constant to adapt that `expr` narrows, if it does: a restriction of
    /// it, or a join, a semijoin or an antijoin of it with another relation,
    /// which computes less of it ([`Adapting::narrows`]), or of what narrows it,
    /// what reads that on the way, as a projection, a renaming or an extension
    /// of it, passing it on.
    fn narrowed(&self, expr: &hir::Expr) -> Option<hir::Adapted> {
        // As it will be, once optimized.
        if let Some(merged) = restrictions_merged(expr) {
            return self.narrowed(&merged);
        }
        let operands: &[&hir::Expr] = match expr {
            hir::Expr::Restrict(operand, _) => &[&**operand],
            hir::Expr::Semijoin(lhs, rhs)
            | hir::Expr::Antijoin(lhs, rhs)
            | hir::Expr::And(lhs, rhs) => &[&**lhs, &**rhs],
            _ => return None,
        };
        operands.iter().enumerate().find_map(|(i, &operand)| {
            if let Some(of) = self.read_of(without_site(operand)) {
                return self.narrows(expr, i == 0, &self.values[&of]).then_some(of);
            }
            self.read_through(operand)
        })
    }

    /// Whether `narrowing`, a restriction, a join, a semijoin or an antijoin whose
    /// operand reads a constant, its left one if `left`, computes less of the
    /// constant's value, `value`, than all of it, in its place, as what comes after
    /// computes only what it needs of it:
    ///
    /// - of a transitive closure, restricted to the tuples whose attribute is equal to
    ///   a value, the paths from that value ([`hir::Expr::looked_up`]), or joined with
    ///   a relation, or matched with one, the paths from the values that has of the
    ///   attributes they share;
    /// - of an AND, restricted by a guard that reads attributes of some of its operands,
    ///   that part of it, restricted as soon as it is joined, before the others are
    ///   ([`crate::Optimizer::planned`]), or joined with another AND, its operands each
    ///   joined with theirs, in the order the plan says.
    ///
    /// A value that reads another constant to adapt is that one's, which the narrowed
    /// form adapts in turn, and a restriction of a restriction is the one it is merged
    /// into ([`crate::restrictions_merged`]). What comes after narrows nothing else:
    /// through what passes a value on, as a renaming or an extension of it, and as an
    /// antijoin needs all of it.
    fn narrows(&self, narrowing: &hir::Expr, left: bool, value: &hir::Expr) -> bool {
        let value = without_site(value);
        // A read of another such constant, whose value the narrowed form adapts in turn.
        if let Some(of) = self.read_of(value) {
            return self.narrows(narrowing, left, &self.values[&of]);
        }
        // A restriction of a restriction, as it will be once optimized.
        if let hir::Expr::Restrict(_, condition) = narrowing
            && let Some(merged) = restrictions_merged(&hir::Expr::Restrict(
                Box::new(value.clone()),
                condition.clone(),
            ))
        {
            let hir::Expr::Restrict(restricted, _) = &merged else {
                unreachable!("restrictions merged are a restriction");
            };
            return self.narrows(&merged, true, restricted);
        }
        let closure = matches!(value, hir::Expr::Tclose(_));
        let shares = |other: &hir::Expr| {
            let (hir::Type::Relation(own), hir::Type::Relation(other)) =
                ((self.ty)(value), (self.ty)(other))
            else {
                return false;
            };
            !names(&own).is_disjoint(&names(&other))
        };
        match narrowing {
            hir::Expr::Restrict(_, condition) if closure => {
                let hir::Type::Relation(heading) = (self.ty)(value) else {
                    return false;
                };
                condition.looked_up(&heading, self.ty).is_some()
            }
            hir::Expr::Restrict(_, condition) if is_and(value) => {
                let mut operands = Vec::new();
                and_operands(value.clone(), &mut operands);
                let headings: Vec<BTreeSet<String>> = operands
                    .iter()
                    .filter_map(|operand| match (self.ty)(operand) {
                        hir::Type::Relation(heading) => Some(names(&heading)),
                        _ => None,
                    })
                    .collect();
                // Read by a guard, and had by the operands but the one at `left_out`.
                let had_without = |reads: &BTreeSet<String>, left_out: usize| {
                    reads.iter().all(|name| {
                        headings
                            .iter()
                            .enumerate()
                            .any(|(i, heading)| i != left_out && heading.contains(name))
                    })
                };
                condition
                    .conjuncts()
                    .iter()
                    .filter_map(guard_reads)
                    .any(|reads| (0..headings.len()).any(|i| had_without(&reads, i)))
            }
            hir::Expr::And(lhs, rhs) if closure => shares(if left { rhs } else { lhs }),
            hir::Expr::And(..) => is_and(value),
            hir::Expr::Semijoin(_, rhs) if closure && left => shares(rhs),
            _ => false,
        }
    }

    /// The constant to adapt that `expr` narrows, passing it on as it is or
    /// narrowed, if it does.
    fn read_through(&self, expr: &hir::Expr) -> Option<hir::Adapted> {
        match expr {
            hir::Expr::Remove(operand, _)
            | hir::Expr::Rename(operand, _)
            | hir::Expr::Extend(operand, ..)
            | hir::Expr::Treat(_, operand)
            | hir::Expr::At(_, operand) => self.read_through(operand),
            _ => self.narrowed(expr),
        }
    }
}
