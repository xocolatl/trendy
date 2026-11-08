//! The optimizer: from HIR to HIR, a program that means the same and costs
//! less to run.
//!
//! Sema computes, when compiling, whatever it can, and leaves the rest for
//! the program to compute when it runs, operators, constants and the conditions
//! of constraints on variables, which is what this rewrites. A
//! rewrite never changes what a program means, which includes the failures
//! it can have: every operand is still computed, and an operand that can
//! fail only where it would be (see `language.md`'s "Evaluation").

use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet};

use trendy_hir as hir;

mod inserted;

/// `program`, optimized.
#[must_use]
pub fn optimized(program: hir::Program) -> hir::Program {
    let program = inserted::constants_inserted(program);
    let operators = program
        .operators
        .iter()
        .map(|operator| optimized_operator(&program, operator))
        .collect();
    let constants = program
        .constants
        .iter()
        .map(|constant| optimized_constant(&program, constant))
        .collect();
    let constraints = program
        .constraints
        .iter()
        .map(|constraint| optimized_constraint(&program, constraint))
        .collect();
    hir::Program {
        operators,
        constants,
        constraints,
        ..program
    }
}

/// `constraint`, of `program`, optimized: what each of its parts computes, which
/// the program does at each assignment to one of its variables. Each part says the
/// same of the same tuples, and its text is as it is written.
fn optimized_constraint(
    program: &hir::Program,
    constraint: &hir::VariableConstraint,
) -> hir::VariableConstraint {
    let parts = constraint
        .parts
        .iter()
        .map(|part| {
            let optimizer = Optimizer {
                program,
                params: &BTreeMap::new(),
                locals: &part.locals,
            };
            hir::ConstraintPart {
                condition: optimizer.expr(part.condition.clone()),
                breaking: part
                    .breaking
                    .as_ref()
                    .map(|breaking| optimizer.expr(breaking.clone())),
                ..part.clone()
            }
        })
        .collect();
    hir::VariableConstraint {
        parts,
        ..constraint.clone()
    }
}

/// `constant`, of `program`, optimized: a value with no parameters, which the
/// program computes if the compiler can't.
fn optimized_constant(program: &hir::Program, constant: &hir::Constant) -> hir::Constant {
    let optimizer = Optimizer {
        program,
        params: &BTreeMap::new(),
        locals: &constant.locals,
    };
    hir::Constant {
        value: optimizer.expr(constant.value.clone()),
        ..constant.clone()
    }
}

/// `operator`, of `program`, optimized.
fn optimized_operator(program: &hir::Program, operator: &hir::Operator) -> hir::Operator {
    let optimizer = Optimizer {
        program,
        params: &operator.params,
        locals: operator.locals(),
    };
    let kind = match operator.kind.clone() {
        hir::OperatorKind::Update { locals, body } => hir::OperatorKind::Update {
            locals,
            body: optimizer.renames_written_where_read(body, None).0,
        },
        kind => kind,
    };
    hir::Operator {
        kind,
        ..operator.clone()
    }
    .map_exprs(&mut |expr| optimizer.expr(expr))
}

/// Optimizes the expressions of an operator, with these parameters and
/// local variables.
struct Optimizer<'a> {
    program: &'a hir::Program,
    params: &'a BTreeMap<String, hir::Parameter>,
    locals: &'a [hir::Local],
}

impl Optimizer<'_> {
    /// `expr`, and every expression in it, optimized: an AND of relations,
    /// restricted or not, is [`Optimizer::planned`].
    fn expr(&self, expr: hir::Expr) -> hir::Expr {
        match expr {
            hir::Expr::Let(id, value, body)
                if is_rename_of_a_read(without_site(&value))
                    && self.read_once_through_a_rename(id, &[&*body]) =>
            {
                self.expr(with_local_as(*body, id, without_site(&value)))
            }
            hir::Expr::Run(block, value) => {
                let (block, value) = self.renames_written_where_read(*block, Some(*value));
                let value = value.expect("it was given one");
                hir::Expr::Run(Box::new(block), Box::new(value))
                    .map_children(|child, _| self.expr(child))
            }
            hir::Expr::Restrict(operand, condition) if is_and(&operand) => {
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

    /// `block`, and the blocks in it, and `end`, its value if it has one, with each local
    /// variable that only names a rename gone, and the rename written where it is read, as
    /// for a binding: see [`Optimizer::named_rename`].
    fn renames_written_where_read(
        &self,
        mut block: hir::Block,
        mut end: Option<hir::Expr>,
    ) -> (hir::Block, Option<hir::Expr>) {
        let mut at = 0;
        while at < block.statements.len() {
            if let Some((id, value)) = self.named_rename(&block, end.as_ref(), at) {
                block.statements.remove(at);
                block.locals.retain(|local| *local != id);
                block = block.map_exprs(&mut |expr| with_local_as(expr, id, &value));
                end = end.map(|end| with_local_as(end, id, &value));
            } else {
                at += 1;
            }
        }
        block.statements = block
            .statements
            .into_iter()
            .map(|(site, statement)| {
                let statement = statement
                    .map_blocks(&mut |inner| self.renames_written_where_read(inner, None).0);
                (site, statement)
            })
            .collect();
        (block, end)
    }

    /// The local variable the statement of `block` at `at` declares, and its value, if that
    /// is all the statement does and it is as good to read the value wherever `block`, or
    /// `end`, reads the variable: assigned nowhere else, a rename of what is only read, which
    /// doesn't change while `block` runs, and read only through a rename.
    fn named_rename(
        &self,
        block: &hir::Block,
        end: Option<&hir::Expr>,
        at: usize,
    ) -> Option<(hir::LocalId, hir::Expr)> {
        let (_, hir::Statement::Assign(assignments)) = &block.statements[at] else {
            return None;
        };
        let [
            hir::Assignment {
                target: hir::Target::Local(id),
                value,
            },
        ] = &assignments[..]
        else {
            return None;
        };
        let value = without_site(value);
        if !is_rename_of_a_read(value)
            || !block.locals.contains(id)
            || block.times_assigned(*id) != 1
            || !self.stays_while(value, block)
        {
            return None;
        }
        let mut exprs: Vec<&hir::Expr> = Vec::new();
        for (_, statement) in &block.statements {
            collect_exprs(statement, &mut exprs);
        }
        exprs.extend(end);
        let mut in_a_loop = false;
        block.each_statement(&mut |statement| {
            if matches!(
                statement,
                hir::Statement::Loop(..) | hir::Statement::For(..)
            ) {
                statement.for_each_expr(&mut |expr| in_a_loop |= expr.reads_local(*id));
            }
        });
        (!in_a_loop && self.read_once_through_a_rename(*id, &exprs)).then(|| (*id, value.clone()))
    }

    /// Whether `exprs` read the binding or local variable `id` no more than once, not again for
    /// each tuple of something, and only through a rename. What is read more often than that is
    /// better sorted once than searched each time.
    fn read_once_through_a_rename(&self, id: hir::LocalId, exprs: &[&hir::Expr]) -> bool {
        let mut reads = 0;
        exprs
            .iter()
            .all(|expr| self.reads_only_through_renames(id, expr, false, &mut reads))
            && reads <= 1
    }

    /// Whether what the rename `renamed` reads has the same value for as long as `block`
    /// runs: nothing in it assigns it, unless it declares it there, before reading it.
    fn stays_while(&self, renamed: &hir::Expr, block: &hir::Block) -> bool {
        let mut read = renamed;
        while let hir::Expr::Rename(inner, _) = read {
            read = inner;
        }
        match read {
            hir::Expr::Parameter(name) => self
                .params
                .get(name)
                .is_some_and(|parameter| parameter.mode == hir::Mode::Const),
            hir::Expr::Local(id) => {
                block.times_assigned(*id) <= usize::from(block.locals.contains(id))
            }
            _ => true,
        }
    }

    /// Whether everything in `expr` that reads the binding `id` of a rename can read what
    /// is renamed instead, through the rename, so that none of its tuples need moving: a
    /// join, a semijoin, a projection, another rename, `count`, `sum` and `product`, and
    /// `is_empty`, as lowering says. Then it isn't computed, for all those to read.
    fn reads_only_through_renames(
        &self,
        id: hir::LocalId,
        expr: &hir::Expr,
        repeated: bool,
        reads: &mut usize,
    ) -> bool {
        use hir::ReadOnlyBuiltin::{Count, Product, Sum};
        match expr {
            hir::Expr::Local(read) => *read != id,
            hir::Expr::Let(bound, ..) if *bound == id => false,
            _ => {
                let through = |operand: &hir::Expr| match expr {
                    hir::Expr::And(lhs, rhs)
                    | hir::Expr::Semijoin(lhs, rhs)
                    | hir::Expr::Antijoin(lhs, rhs) => {
                        core::ptr::eq(operand, lhs.as_ref()) || core::ptr::eq(operand, rhs.as_ref())
                    }
                    hir::Expr::Remove(renamed, _) | hir::Expr::Rename(renamed, _) => {
                        core::ptr::eq(operand, renamed.as_ref())
                    }
                    hir::Expr::Call(call) => {
                        let callee = self.program.operator(call.operator);
                        let counts = matches!(
                            callee.kind,
                            hir::OperatorKind::Builtin(hir::Builtin::ReadOnly(
                                Count | Sum | Product
                            ))
                        ) || callee.name == "is_empty";
                        counts && call.args.values().any(|arg| core::ptr::eq(operand, arg))
                    }
                    _ => false,
                };
                let mut only = true;
                expr.for_each_child(|child, _| {
                    let is_read = matches!(child, hir::Expr::Local(read) if *read == id);
                    *reads += usize::from(is_read);
                    let again = repeated || expr.computes_again(child);
                    only = only
                        && ((is_read && !again && through(child))
                            || self.reads_only_through_renames(id, child, again, reads));
                });
                only
            }
        }
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

/// Whether `expr` is a rename of what is only read, with nothing to compute or to fail,
/// and the same wherever in a chain of bindings it is read.
fn is_rename_of_a_read(expr: &hir::Expr) -> bool {
    let mut operand = expr;
    while let hir::Expr::Rename(inner, _) = operand {
        operand = inner;
    }
    matches!(expr, hir::Expr::Rename(..))
        && matches!(
            operand,
            hir::Expr::Parameter(_)
                | hir::Expr::Local(_)
                | hir::Expr::ComputedConstant(..)
                | hir::Expr::Constant(_)
        )
}

/// Each expression of `statement`, and of the blocks in it, into `exprs`.
fn collect_exprs<'e>(statement: &'e hir::Statement, exprs: &mut Vec<&'e hir::Expr>) {
    match statement {
        hir::Statement::Call(call) => exprs.extend(call.args.values()),
        hir::Statement::Assign(assignments) => {
            exprs.extend(assignments.iter().map(|assignment| &assignment.value));
        }
        hir::Statement::If(condition, ..)
        | hir::Statement::Loop(condition, ..)
        | hir::Statement::For(_, condition, _)
        | hir::Statement::Fail(condition)
        | hir::Statement::Warn(condition)
        | hir::Statement::Return(Some(condition)) => exprs.push(condition),
        hir::Statement::Declare(_)
        | hir::Statement::Transaction(..)
        | hir::Statement::Return(None)
        | hir::Statement::Leave
        | hir::Statement::Iterate => {}
    }
    for block in statement.blocks() {
        for (_, inner) in &block.statements {
            collect_exprs(inner, exprs);
        }
    }
}

/// `expr`, apart from where it is computed, which only says so of what can fail.
fn without_site(expr: &hir::Expr) -> &hir::Expr {
    match expr {
        hir::Expr::At(_, inner) => without_site(inner),
        expr => expr,
    }
}

/// `expr`, with `value` where it reads the local variable `id`.
fn with_local_as(expr: hir::Expr, id: hir::LocalId, value: &hir::Expr) -> hir::Expr {
    match expr {
        hir::Expr::Local(read) if read == id => value.clone(),
        expr => expr.map_children(|child, _| with_local_as(child, id, value)),
    }
}

/// Whether `expr` is an AND, at a site of its own or not, as a value written
/// elsewhere is.
fn is_and(expr: &hir::Expr) -> bool {
    match expr {
        hir::Expr::And(..) => true,
        hir::Expr::At(_, inner) => is_and(inner),
        _ => false,
    }
}

/// The operands of `expr`, of an AND, and of each AND among them: of a
/// chain of ANDs, however it is grouped, as AND is associative. Those of an AND
/// at a site of its own are each computed there, where a failure in it says it
/// is.
fn and_operands(expr: hir::Expr, operands: &mut Vec<hir::Expr>) {
    match expr {
        hir::Expr::At(site, inner) if is_and(&inner) => {
            let mut own = Vec::new();
            and_operands(*inner, &mut own);
            operands.extend(
                own.into_iter()
                    .map(|operand| hir::Expr::At(site, Box::new(operand))),
            );
        }
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

/// The AND of `so_far`, if anything, and `next`: `next` restricted first by each of
/// `guards` that reads only attributes it has, and then the AND by each that now can be,
/// each taken out of `guards`. So only a guard across operands comes between joins, which
/// are otherwise a chain of them, whose order can be chosen as the program runs.
fn and_guarded(so_far: Option<Joined>, next: Joined, guards: &mut Vec<Guard>) -> Joined {
    let next = guarded(next, guards);
    let Some(so_far) = so_far else {
        return next;
    };
    let heading = so_far.heading.and(&next.heading);
    guarded(
        Joined {
            expr: hir::Expr::And(Box::new(so_far.expr), Box::new(next.expr)),
            heading,
        },
        guards,
    )
}

/// `joined`, restricted by each of `guards` that reads only attributes it has, each taken
/// out of `guards`.
fn guarded(joined: Joined, guards: &mut Vec<Guard>) -> Joined {
    let Joined { expr, heading } = joined;
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

    /// The valid program `source`, optimized.
    fn program(source: &str) -> hir::Program {
        let parse = trendy_parser::parse(source, trendy_lexer::lex(source));
        assert!(parse.errors.is_empty(), "{:?}", parse.errors);
        let unit = trendy_sema::File {
            name: "program",
            path: "program.trendy",
            tree: &parse.tree,
        };
        let library = trendy_load::library();
        let library: Vec<trendy_sema::File> = library
            .iter()
            .map(|unit| trendy_sema::File {
                name: &unit.name,
                path: &unit.path,
                tree: &unit.parse.tree,
            })
            .collect();
        let program = trendy_sema::analyze(&[unit], &library).expect("the program is valid");
        optimized(program)
    }

    /// The relation `call debug_table{r: …};`, the only statement of the
    /// update operator `f` in `source`, writes, once optimized, in
    /// shorthand: each restriction's condition in trendy, and the rest as
    /// its ANDs, of parameters. Each restriction's tuple at hand must have
    /// the heading of what it restricts.
    fn planned(source: &str) -> String {
        let program = program(source);
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

    /// The value of the constant `name` in `source`, once optimized, as
    /// [`planned`] writes it.
    fn planned_constant(source: &str, name: &str) -> String {
        let program = program(source);
        let constant = program
            .constants
            .iter()
            .find(|constant| constant.name == name)
            .expect("the constant is declared");
        let optimizer = Optimizer {
            program: &program,
            params: &BTreeMap::new(),
            locals: &constant.locals,
        };
        shorthand(&optimizer, &constant.value)
    }

    fn shorthand(optimizer: &Optimizer, expr: &hir::Expr) -> String {
        match expr {
            hir::Expr::Parameter(name) => name.clone(),
            hir::Expr::Variable(id) => optimizer.program.variables[id.0].name.clone(),
            hir::Expr::ComputedConstant(id, _) => optimizer.program.constants[id.0].name.clone(),
            hir::Expr::At(_, inner) => shorthand(optimizer, inner),
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

    /// How the first constraint's first part's relation, of an `is_empty`, is planned
    /// in its condition, and in what it says breaks it.
    fn planned_constraint(source: &str) -> (String, String) {
        let program = program(source);
        let part = &program.constraints[0].parts[0];
        let optimizer = Optimizer {
            program: &program,
            params: &BTreeMap::new(),
            locals: &part.locals,
        };
        let hir::Expr::Call(call) = &part.condition else {
            panic!("the condition is `is_empty`: {:?}", part.condition);
        };
        let breaking = part.breaking.as_ref().expect("`is_empty` says which");
        (
            shorthand(&optimizer, &call.args["r"]),
            shorthand(&optimizer, breaking),
        )
    }

    #[test]
    fn a_constraints_condition_is_planned_as_an_operators_is() {
        let vars = "var regions: relation { region: integer, size: integer };\nvar customers: relation { customer: integer, region: integer };\nvar orders: relation { customer: integer, amount: integer };\n";
        let main = "operator main{} [];";
        // What it says is broken is planned too, by the same rule.
        let (condition, breaking) = planned_constraint(&format!(
            "{vars}constraint c := is_empty{{r: (orders join customers join regions) where amount < 0 and size = 1}};\n{main}"
        ));
        assert_eq!(
            condition,
            "(((orders where amount < 0) and customers) and (regions where size = 1))"
        );
        assert_eq!(breaking, condition);
    }

    #[test]
    fn a_constants_value_is_planned_as_an_operators_is() {
        let source = "const a := series{from: 1, to: 3} rename {value as x};\n\
             const b := series{from: 1, to: 3} rename {value as y};\n\
             const few := times{a, b} where x = 1 and y = 2;\n\
             operator main{} [ call debug_table{r: few}; call debug_table{r: a}; call debug_table{r: b}; ];";
        assert_eq!(
            planned_constant(source, "few"),
            "((a where x = 1) and (b where y = 2))"
        );
    }

    /// The name of each constant of `source` an operator still reads.
    fn shared(source: &str) -> Vec<String> {
        let program = program(source);
        let mut read = Vec::new();
        let mut note = |expr: &hir::Expr| reads(expr, &program, &mut read);
        for operator in &program.operators {
            match &operator.kind {
                hir::OperatorKind::Update { body, .. } => body.for_each_expr(&mut note),
                hir::OperatorKind::ReadOnly { value, .. } => note(value),
                hir::OperatorKind::Builtin(_) | hir::OperatorKind::Specification { .. } => {}
            }
        }
        read.sort();
        read.dedup();
        read
    }

    /// The name of each constant `expr` reads, and those they read.
    fn reads(expr: &hir::Expr, program: &hir::Program, read: &mut Vec<String>) {
        if let hir::Expr::ComputedConstant(id, _) = expr {
            let constant = &program.constants[id.0];
            read.push(constant.name.clone());
            reads(&constant.value, program, read);
        }
        expr.for_each_child(|child, _| reads(child, program, read));
    }

    const SERIES: &str = "series{from: 1, to: 3}";

    #[test]
    fn a_constants_only_read_that_runs_once_is_its_value() {
        let two = format!(
            "const a := {SERIES} rename {{value as x}};\n\
             const b := {SERIES} rename {{value as y}};\n"
        );
        // Its only read, in `main`, in a branch, or in what `main` calls once;
        // a constant it reads, however many read that one.
        for source in [
            format!("{two}operator main{{}} [ call debug_table{{r: a}}; ];"),
            format!(
                "{two}operator main{{}} [ var n := 0; if n > 0 then [ call debug_table{{r: a}}; ]; ];"
            ),
            format!(
                "{two}operator f{{}} [ call debug_table{{r: a}}; ];\noperator main{{}} [ call f{{}}; ];"
            ),
            format!(
                "{two}operator f{{}}: integer [ count{{r: a}} ];\noperator main{{}} [ call debug_value{{v: f{{}}}}; ];"
            ),
            format!(
                "const b := {SERIES};\nconst a := b;\noperator main{{}} [ call debug_table{{r: a}}; ];"
            ),
        ] {
            assert_eq!(shared(&source), Vec::<String>::new(), "{source}");
        }
        // Its value seen by what reads it: the closure restricted to one value.
        let source = format!(
            "const a := ({SERIES} rename {{value as x}}) extend {{ y := x }};\n\
             const r := a tclose;\n\
             operator main{{}} [ call debug_table{{r: r where x = 1}}; ];"
        );
        let program = program(&source);
        let main = program.operator(program.entry);
        let hir::OperatorKind::Update { body, .. } = &main.kind else {
            panic!("`main` is an update operator");
        };
        let mut closures = 0;
        body.for_each_expr(&mut |expr| closures += restricted_closures(expr));
        assert_eq!(closures, 1);
    }

    /// How many restrictions of a closure are in `expr`.
    fn restricted_closures(expr: &hir::Expr) -> usize {
        let mut found = 0;
        if let hir::Expr::Restrict(operand, _) = expr {
            let mut closure = &**operand;
            while let hir::Expr::At(_, inner) = closure {
                closure = inner;
            }
            found = usize::from(matches!(closure, hir::Expr::Tclose(_)));
        }
        expr.for_each_child(|child, _| found += restricted_closures(child));
        found
    }

    #[test]
    fn a_plan_sees_what_a_constant_inserted_is() {
        // The `a` and `b` of `f` stay read, so that they are shared, by `main`.
        let source = format!(
            "const a := {SERIES} rename {{value as x}};\n\
             const b := {SERIES} rename {{value as y}};\n\
             const pairs := times{{a, b}};\n\
             operator f{{}} [ call debug_table{{r: pairs where x = 1 and y = 2}}; ];\n\
             operator main{{}} [ call f{{}}; call debug_table{{r: a}}; call debug_table{{r: b}}; ];"
        );
        assert_eq!(planned(&source), "((a where x = 1) and (b where y = 2))");
    }

    #[test]
    fn a_constant_read_more_than_once_is_shared() {
        let a = format!("const a := {SERIES} rename {{value as x}};\n");
        let table = "call debug_table{r: a};";
        for body in [
            // Two reads, in one statement or two.
            format!("operator main{{}} [ {table} {table} ];"),
            "operator main{} [ call debug_table{r: a union a}; ];".to_string(),
            // In a loop, or its condition.
            format!("operator main{{}} [ var n := 0; while n < 3 loop [ {table} n := n + 1; ]; ];"),
            "operator main{} [ var n := 0; while n < count{r: a} loop [ n := n + 1; ]; ];".to_string(),
            format!("operator main{{}} [ for x in array [1, 2] loop [ {table} ]; ];"),
            // For each tuple.
            format!(
                "operator main{{}} [ call debug_table{{r: ({SERIES} rename {{value as y}}) where y = count{{r: a}}}}; ];"
            ),
            format!(
                "operator main{{}} [ call debug_table{{r: ({SERIES} rename {{value as y}}) extend {{ n := count{{r: a}} }}}}; ];"
            ),
            // In what is called more than once, or from a loop, or in a cycle.
            format!("operator f{{}} [ {table} ];\noperator main{{}} [ call f{{}}; call f{{}}; ];"),
            format!(
                "operator f{{}} [ {table} ];\noperator main{{}} [ var n := 0; while n < 3 loop [ call f{{}}; n := n + 1; ]; ];"
            ),
            "operator f{n: integer}: integer [ if n = 0 then count{r: a} else f{n: n - 1} ];\noperator main{} [ call debug_value{v: f{n: 3}}; ];".to_string(),
            // In what is called for each value of a type, and beside a constraint's,
            // which is checked at each assignment.
            "type t { { n: integer }, constraint n <= count{r: a} };\noperator main{} [ var m := 1; call debug_value{v: t{n: m}}; ];".to_string(),
            format!(
                "var v: relation {{ x: integer }};\nconstraint c := is_empty{{r: v where x > count{{r: a}}}};\noperator main{{}} [ v := relation {{}}; {table} ];"
            ),
        ] {
            let source = format!("{a}{body}");
            assert_eq!(shared(&source), ["a"], "{source}");
        }
        // With a key to check, or local variables, whatever reads it.
        let table = "operator main{} [ call debug_table{r: a}; ];";
        let source =
            format!("const a := ({SERIES} extend {{ same := 1 }}) key {{ same }};\n{table}");
        assert_eq!(shared(&source), ["a"]);
        let source = format!(
            "const a := [ var n := 0; while n < 3 loop [ n := n + 1; ]; {SERIES} where value = n ];\n{table}"
        );
        assert_eq!(shared(&source), ["a"]);
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
