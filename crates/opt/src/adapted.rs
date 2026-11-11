//! What narrows a constant read more than once, written both ways.
//!
//! A read of a constant is as if its expression were written there, so what
//! narrows it, as a restriction of it does, can compute only what it needs of
//! it, rather than all of it, computed once and shared. Which costs less
//! depends on how often each read runs, which only running says, so each is
//! written both ways ([`hir::Expr::AdaptedRead`]), for the program to choose as
//! it runs.

use std::collections::HashMap;

use trendy_hir as hir;

/// Writes what narrows each of these constants both ways.
pub(crate) struct Adapting<'a> {
    /// The value of each constant to adapt, of the program's or a local one,
    /// which takes the place of a read of it where it is narrowed.
    pub(crate) values: &'a HashMap<hir::Adapted, hir::Expr>,
}

impl Adapting<'_> {
    /// `expr`, with what narrows a constant to adapt, but `shared`, written both
    /// ways ([`hir::AdaptedRead`]), the outermost of what narrows it, if it reads
    /// the constant once.
    pub(crate) fn expr(&self, expr: hir::Expr, shared: Option<hir::Adapted>) -> hir::Expr {
        if let Some(of) = self.narrowed(&expr).filter(|of| Some(*of) != shared)
            && self.reads_of(&expr, of) == 1
        {
            let narrowed = self.expr(self.with_value(expr.clone(), of), None);
            let shared = expr.map_children(|child, _| self.expr(child, Some(of)));
            return hir::Expr::AdaptedRead(Box::new(hir::AdaptedRead {
                of,
                shared,
                narrowed,
            }));
        }
        expr.map_children(|child, _| self.expr(child, shared))
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
    /// what reads it on the way, as a projection, a renaming or an extension
    /// of it, passing it on.
    fn narrowed(&self, expr: &hir::Expr) -> Option<hir::Adapted> {
        let operands: &[&hir::Expr] = match expr {
            hir::Expr::Restrict(operand, _) => &[&**operand],
            hir::Expr::Semijoin(lhs, rhs)
            | hir::Expr::Antijoin(lhs, rhs)
            | hir::Expr::And(lhs, rhs) => &[&**lhs, &**rhs],
            _ => return None,
        };
        operands
            .iter()
            .find_map(|operand| self.read_through(operand))
    }

    /// The constant to adapt that `expr` reads, passing it on as it is or
    /// narrowed, if it does.
    fn read_through(&self, expr: &hir::Expr) -> Option<hir::Adapted> {
        if let Some(of) = self.read_of(expr) {
            return Some(of);
        }
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
