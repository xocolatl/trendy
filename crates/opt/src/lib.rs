//! The optimizer: from HIR to HIR, a program that means the same and costs
//! less to run.
//!
//! Sema computes, when compiling, whatever it can, and leaves the rest for
//! the program to compute when it runs, which is what this rewrites. A
//! rewrite never changes what a program means, which includes the failures
//! it can have: every operand is still computed, and an operand that can
//! fail only where it would be (see `language.md`'s "Evaluation").

use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet};

use trendy_hir as hir;

/// `program`, optimized.
#[must_use]
pub fn optimized(program: hir::Program) -> hir::Program {
    let operators = program
        .operators
        .iter()
        .map(|operator| optimized_operator(&program, operator))
        .collect();
    hir::Program {
        operators,
        ..program
    }
}

/// `operator`, of `program`, optimized.
fn optimized_operator(program: &hir::Program, operator: &hir::Operator) -> hir::Operator {
    let kind = match &operator.kind {
        hir::OperatorKind::Update { locals, body } => {
            let optimizer = Optimizer {
                program,
                params: &operator.params,
                locals,
            };
            hir::OperatorKind::Update {
                locals: locals.clone(),
                body: optimizer.block(body.clone()),
            }
        }
        hir::OperatorKind::ReadOnly {
            result,
            locals,
            value,
        } => {
            let optimizer = Optimizer {
                program,
                params: &operator.params,
                locals,
            };
            hir::OperatorKind::ReadOnly {
                result: result.clone(),
                locals: locals.clone(),
                value: optimizer.expr(value.clone()),
            }
        }
        hir::OperatorKind::Builtin(builtin) => hir::OperatorKind::Builtin(*builtin),
        hir::OperatorKind::Specification { result } => hir::OperatorKind::Specification {
            result: result.clone(),
        },
    };
    hir::Operator {
        kind,
        ..operator.clone()
    }
}

/// Optimizes the expressions of an operator, with these parameters and
/// local variables.
struct Optimizer<'a> {
    program: &'a hir::Program,
    params: &'a BTreeMap<String, hir::Parameter>,
    locals: &'a [hir::Local],
}

impl Optimizer<'_> {
    fn block(&self, block: hir::Block) -> hir::Block {
        hir::Block {
            statements: block
                .statements
                .into_iter()
                .map(|(site, s)| (site, self.statement(s)))
                .collect(),
            ..block
        }
    }

    fn statement(&self, statement: hir::Statement) -> hir::Statement {
        match statement {
            hir::Statement::Call(call) => hir::Statement::Call(hir::Call {
                args: call
                    .args
                    .into_iter()
                    .map(|(name, arg)| (name, self.expr(arg)))
                    .collect(),
                ..call
            }),
            hir::Statement::Assign(assignments) => hir::Statement::Assign(
                assignments
                    .into_iter()
                    .map(|a| hir::Assignment {
                        value: self.expr(a.value),
                        ..a
                    })
                    .collect(),
            ),
            hir::Statement::If(condition, then, otherwise) => hir::Statement::If(
                self.expr(condition),
                self.block(then),
                self.block(otherwise),
            ),
            hir::Statement::Loop(condition, body, check) => {
                hir::Statement::Loop(self.expr(condition), self.block(body), check)
            }
            hir::Statement::For(local, array, body) => {
                hir::Statement::For(local, self.expr(array), self.block(body))
            }
            hir::Statement::Fail(error) => hir::Statement::Fail(self.expr(error)),
            hir::Statement::Warn(error) => hir::Statement::Warn(self.expr(error)),
            hir::Statement::Transaction(collecting, body) => {
                hir::Statement::Transaction(collecting, self.block(body))
            }
        }
    }

    /// `expr`, and every expression in it, optimized: an AND of relations,
    /// restricted or not, is [`Optimizer::planned`].
    fn expr(&self, expr: hir::Expr) -> hir::Expr {
        match expr {
            hir::Expr::Restrict(operand, condition) if matches!(*operand, hir::Expr::And(..)) => {
                let condition = self.expr(*condition);
                self.planned(*operand, condition.conjuncts().to_vec())
            }
            hir::Expr::And(..) if matches!(self.ty(&expr), hir::Type::Relation(_)) => {
                self.planned(expr, Vec::new())
            }
            expr => expr.map_children(|child, _| self.expr(child)),
        }
    }

    fn ty(&self, expr: &hir::Expr) -> hir::Type {
        self.program.type_of(expr, self.params, self.locals)
    }

    /// `and`, an AND of relations, restricted to the tuples every one of
    /// `conjuncts` is true of, computed as soon as it can be, and its
    /// operands joined in an order that keeps what it makes small, as
    /// [`order`] says.
    ///
    /// Each conjunct that can't fail, and reads the tuple at hand an
    /// attribute at a time, restricts the AND as soon as it has every
    /// attribute the conjunct reads, one that reads none the first operand:
    /// that only drops tuples it would drop at the end, and it can't fail
    /// on the others. The rest restrict the whole AND, as they would have,
    /// since one that can fail is computed only for a tuple of it that
    /// every one that can't is true of (see [`hir::Connective::And`]).
    fn planned(&self, and: hir::Expr, conjuncts: Vec<hir::Expr>) -> hir::Expr {
        let mut operands = Vec::new();
        and_operands(and, &mut operands);
        let operands: Vec<Joined> = operands
            .into_iter()
            .map(|operand| {
                let expr = self.expr(operand);
                let hir::Type::Relation(heading) = self.ty(&expr) else {
                    unreachable!("an AND of relations is of relations");
                };
                Joined { expr, heading }
            })
            .collect();
        let (mut guards, mut rest) = (Vec::new(), Vec::new());
        for conjunct in conjuncts {
            let read = conjunct.tuple_read(0);
            if conjunct.can_fail() || read.whole {
                rest.push(conjunct);
            } else {
                guards.push(Guard {
                    expr: conjunct,
                    reads: read.attributes,
                });
            }
        }
        let names: Vec<BTreeSet<String>> = operands
            .iter()
            .map(|operand| names(&operand.heading))
            .collect();
        let reads: Vec<&BTreeSet<String>> = guards.iter().map(|guard| &guard.reads).collect();
        let parts = order(&names, &reads);
        let mut operands: Vec<Option<Joined>> = operands.into_iter().map(Some).collect();
        let mut whole = None;
        for part in parts {
            let mut joined = None;
            for i in part {
                let operand = operands[i].take().expect("each operand is in one part");
                joined = Some(and_guarded(joined, operand, &mut guards));
            }
            let joined = joined.expect("each part has an operand");
            whole = Some(and_guarded(whole, joined, &mut guards));
        }
        let whole = whole.expect("an AND has operands");
        if rest.is_empty() {
            whole.expr
        } else {
            hir::Expr::Restrict(
                Box::new(whole.expr),
                Box::new(hir::Connective::And.of(rest)),
            )
        }
    }
}

/// An expression whose value is a relation, with that relation's heading.
struct Joined {
    expr: hir::Expr,
    heading: hir::Heading,
}

/// A conjunct of a restriction's condition that can restrict part of the
/// AND it restricts: one that can't fail, and reads the tuple at hand an
/// attribute at a time.
struct Guard {
    expr: hir::Expr,
    /// The attributes of the tuple at hand it reads.
    reads: BTreeSet<String>,
}

/// The operands of `expr`, of an AND, and of each AND among them: of a
/// chain of ANDs, however it is grouped, as AND is associative.
fn and_operands(expr: hir::Expr, operands: &mut Vec<hir::Expr>) {
    match expr {
        hir::Expr::And(lhs, rhs) => {
            and_operands(*lhs, operands);
            and_operands(*rhs, operands);
        }
        operand => operands.push(operand),
    }
}

/// The names of the attributes of `heading`.
fn names(heading: &hir::Heading) -> BTreeSet<String> {
    heading
        .attributes()
        .map(|(name, _)| name.to_string())
        .collect()
}

/// The order to join the operands of an AND in, given the names of each
/// one's attributes, `names`, and of those each guard of its restriction
/// reads, `reads`: the operands in parts, each the order to join them in.
///
/// Two operands are related when they have an attribute in common, or a
/// guard reads attributes of both. A part is the operands related to each
/// other, directly or through others, so that the parts are only ever
/// multiplied, last, when nothing more can restrict them, in the order
/// their first operands are written. In a part, the first operand is the
/// one the most guards read only attributes of, and each next the one
/// related to those so far, with the most attributes in common with them,
/// then the most guards it lets be computed. Of operands otherwise equal,
/// the first written comes first.
fn order(names: &[BTreeSet<String>], reads: &[&BTreeSet<String>]) -> Vec<Vec<usize>> {
    let related = |i: usize, k: usize| {
        !names[i].is_disjoint(&names[k])
            || reads
                .iter()
                .any(|read| !read.is_disjoint(&names[i]) && !read.is_disjoint(&names[k]))
    };
    let mut placed = vec![false; names.len()];
    let mut parts = Vec::new();
    for first in 0..names.len() {
        if placed[first] {
            continue;
        }
        placed[first] = true;
        let mut part = vec![first];
        let mut i = 0;
        while i < part.len() {
            let at = part[i];
            for (k, placed) in placed.iter_mut().enumerate() {
                if !*placed && related(at, k) {
                    *placed = true;
                    part.push(k);
                }
            }
            i += 1;
        }
        let mut ordered: Vec<usize> = Vec::new();
        let mut joined = BTreeSet::new();
        while !part.is_empty() {
            let lets = |i: usize| {
                reads
                    .iter()
                    .filter(|read| {
                        !read.is_subset(&joined)
                            && read
                                .iter()
                                .all(|n| joined.contains(n) || names[i].contains(n))
                    })
                    .count()
            };
            let next = (0..part.len())
                .max_by_key(|&j| {
                    let i = part[j];
                    (
                        ordered.iter().any(|&k| related(i, k)),
                        names[i].intersection(&joined).count(),
                        lets(i),
                        Reverse(i),
                    )
                })
                .expect("an operand is left");
            let i = part.remove(next);
            joined.extend(names[i].iter().cloned());
            ordered.push(i);
        }
        parts.push(ordered);
    }
    parts
}

/// The AND of `so_far`, if anything, and `next`, restricted by each of
/// `guards` that reads only attributes it has, each taken out of `guards`.
fn and_guarded(so_far: Option<Joined>, next: Joined, guards: &mut Vec<Guard>) -> Joined {
    let (expr, heading) = match so_far {
        None => (next.expr, next.heading),
        Some(so_far) => (
            hir::Expr::And(Box::new(so_far.expr), Box::new(next.expr)),
            so_far.heading.and(&next.heading),
        ),
    };
    let (now, later): (Vec<Guard>, Vec<Guard>) = std::mem::take(guards)
        .into_iter()
        .partition(|guard| guard.reads.iter().all(|n| heading.get(n).is_some()));
    *guards = later;
    if now.is_empty() {
        return Joined { expr, heading };
    }
    let condition = hir::Connective::And.of(now
        .into_iter()
        .map(|guard| at_hand(guard.expr, 0, &heading))
        .collect());
    Joined {
        expr: hir::Expr::Restrict(Box::new(expr), Box::new(condition)),
        heading,
    }
}

/// `expr`, which reads only attributes of `heading` of the tuple at hand
/// `depth` of them out from it, with that tuple one of `heading`, as the
/// tuple at hand of a restriction of part of an AND is.
fn at_hand(expr: hir::Expr, depth: usize, heading: &hir::Heading) -> hir::Expr {
    match expr {
        hir::Expr::Current(d, _) if d == depth => hir::Expr::Current(d, heading.clone()),
        expr => expr.map_children(|child, deeper| at_hand(child, depth + deeper, heading)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The relation `call debug_table{r: …};`, the only statement of the
    /// update operator `f` in `source`, writes, once optimized, in
    /// shorthand: each restriction's condition in trendy, and the rest as
    /// its ANDs, of parameters. Each restriction's tuple at hand must have
    /// the heading of what it restricts.
    fn planned(source: &str) -> String {
        let prelude = trendy_prelude::SOURCE;
        let prelude = trendy_parser::parse(prelude, trendy_lexer::lex(prelude));
        let parse = trendy_parser::parse(source, trendy_lexer::lex(source));
        assert!(parse.errors.is_empty(), "{:?}", parse.errors);
        let program =
            trendy_sema::analyze(&parse.tree, &prelude.tree).expect("the program is valid");
        let program = optimized(program);
        let operator = program
            .operators
            .iter()
            .find(|operator| operator.name == "f")
            .expect("`f` is defined");
        let hir::OperatorKind::Update { locals, body } = &operator.kind else {
            panic!("`f` is an update operator");
        };
        let [(_, hir::Statement::Call(call))] = &body.statements[..] else {
            panic!("`f` makes one call");
        };
        let optimizer = Optimizer {
            program: &program,
            params: &operator.params,
            locals,
        };
        shorthand(&optimizer, &call.args["r"])
    }

    fn shorthand(optimizer: &Optimizer, expr: &hir::Expr) -> String {
        match expr {
            hir::Expr::Parameter(name) => name.clone(),
            hir::Expr::And(lhs, rhs) => format!(
                "({} and {})",
                shorthand(optimizer, lhs),
                shorthand(optimizer, rhs)
            ),
            hir::Expr::Restrict(operand, condition) => {
                let hir::Type::Relation(heading) = optimizer.ty(operand) else {
                    panic!("a restriction is of a relation");
                };
                assert_at_hand(condition, 0, &heading);
                format!(
                    "({} where {})",
                    shorthand(optimizer, operand),
                    written(condition)
                )
            }
            hir::Expr::Extend(operand, _, attributes) => {
                let values: Vec<String> = attributes
                    .iter()
                    .map(|(name, value)| format!("{name} := {}", shorthand(optimizer, value)))
                    .collect();
                format!(
                    "({} extend {{ {} }})",
                    shorthand(optimizer, operand),
                    values.join(", ")
                )
            }
            hir::Expr::Call(call) => {
                let name = &optimizer.program.operator(call.operator).name;
                let args: Vec<String> = call
                    .args
                    .iter()
                    .map(|(param, arg)| format!("{param}: {}", shorthand(optimizer, arg)))
                    .collect();
                format!("{name}{{{}}}", args.join(", "))
            }
            other => panic!("no shorthand for {other:?}"),
        }
    }

    /// `condition`, of a restriction, in trendy.
    fn written(condition: &hir::Expr) -> String {
        match condition {
            hir::Expr::Extract(tuple, name) if matches!(**tuple, hir::Expr::Current(..)) => {
                name.clone()
            }
            hir::Expr::Parameter(name) => name.clone(),
            hir::Expr::Constant(hir::Value::Integer(n)) => n.to_string(),
            hir::Expr::Connective(hir::Connective::And, operands) => operands
                .iter()
                .map(written)
                .collect::<Vec<_>>()
                .join(" and "),
            hir::Expr::Binary(op, lhs, rhs) => {
                let op = match op {
                    hir::BinaryOp::Div => "div",
                    hir::BinaryOp::Compare(hir::Comparison::Equal, _) => "=",
                    hir::BinaryOp::Compare(hir::Comparison::Greater, _) => ">",
                    hir::BinaryOp::Compare(hir::Comparison::Less, _) => "<",
                    other => panic!("no shorthand for {other:?}"),
                };
                format!("{} {op} {}", written(lhs), written(rhs))
            }
            other => panic!("no shorthand for {other:?}"),
        }
    }

    /// Asserts that each tuple at hand `depth` out from `expr` has `heading`.
    fn assert_at_hand(expr: &hir::Expr, depth: usize, heading: &hir::Heading) {
        match expr {
            hir::Expr::Current(d, at_hand) if *d == depth => assert_eq!(at_hand, heading),
            _ => expr.for_each_child(|child, deeper| {
                assert_at_hand(child, depth + deeper, heading);
            }),
        }
    }

    const PARAMS: &str = "a: relation { x: integer }, \
        b: relation { y: integer }, \
        c: relation { z: integer }, \
        d: relation { x: integer, y: integer }, \
        e: relation { w: integer }, \
        p: boolean";

    fn f(r: &str) -> String {
        format!("operator main{{}} [];\noperator f{{{PARAMS}}} [ call debug_table{{r: {r}}}; ];")
    }

    #[test]
    fn a_condition_restricts_the_operands_it_reads_as_soon_as_they_are_joined() {
        assert_eq!(
            planned(&f("times{a, b, c} where x = z and y > 1")),
            "(((a and c) where x = z) and (b where y > 1))"
        );
        assert_eq!(
            planned(&f("times{b, a, c} where p")),
            "(((b where p) and a) and c)"
        );
    }

    #[test]
    fn what_can_fail_restricts_the_whole_and() {
        assert_eq!(
            planned(&f("times{a, b} where 1 div x = 1 and y > 1")),
            "((a and (b where y > 1)) where 1 div x = 1)"
        );
    }

    #[test]
    fn operands_join_through_common_attributes_before_any_product() {
        assert_eq!(planned(&f("join{a, b, d}")), "((a and d) and b)");
        assert_eq!(
            planned(&f("times{a, b} join d where y > 1")),
            "(((b where y > 1) and d) and a)"
        );
    }

    #[test]
    fn a_tuple_at_hand_further_out_is_left_as_it_is() {
        assert_eq!(
            planned(&f(
                "e extend { n := count{r: times{a, b} where x > w} } where n > 0"
            )),
            "((e extend { n := count{r: ((a where x > w) and b)} }) where n > 0)"
        );
    }
}
