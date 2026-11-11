//! The optimizer: from HIR to HIR, a program that means the same and costs
//! less to run.
//!
//! Sema computes, when compiling, whatever it can, and leaves the rest for
//! the program to compute when it runs, operators, constants and the conditions
//! of constraints on variables, which is what this rewrites. A
//! rewrite never changes what a program means, which includes the failures
//! it can have: every operand is still computed, and an operand that can
//! fail only where it would be (see `language.md`'s "Evaluation"). A constant,
//! the program's or a local one, is the exception: it is computed when something
//! first needs it, so its failure happens there (see `language.md`'s "Constants").

use std::cell::RefCell;
use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet, HashMap};

use trendy_hir as hir;

mod adapted;
mod inserted;
mod pushed;

use adapted::Adapting;
use pushed::{Pushing, restrictions_merged};

/// The type of an expression, where it is.
pub(crate) type TypeOf<'a> = dyn Fn(&hir::Expr) -> hir::Type + 'a;

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
            let params = BTreeMap::new();
            let optimizer = Optimizer::new(program, &params, &part.locals);
            let condition = optimizer.expr(part.condition.clone());
            let breaking = part
                .breaking
                .as_ref()
                .map(|breaking| optimizer.expr(breaking.clone()));
            hir::ConstraintPart {
                condition,
                breaking,
                locals: optimizer.with_deferred(part.locals.clone()),
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
    let params = BTreeMap::new();
    let optimizer = Optimizer::new(program, &params, &constant.locals);
    let value = optimizer.expr(constant.value.clone());
    hir::Constant {
        value,
        locals: optimizer.with_deferred(constant.locals.clone()),
        ..constant.clone()
    }
}

/// `operator`, of `program`, optimized.
fn optimized_operator(program: &hir::Program, operator: &hir::Operator) -> hir::Operator {
    let optimizer = Optimizer::new(program, &operator.params, operator.locals());
    let kind = match operator.kind.clone() {
        hir::OperatorKind::Update { locals, body } => {
            let body = optimizer.values_written_where_read(body, None).0;
            let body = body.map_exprs(&mut |expr| optimizer.bindings_inlined(expr));
            hir::OperatorKind::Update {
                locals,
                body: optimizer.locals_adapted(body, None).0,
            }
        }
        kind => kind,
    };
    let rewritten = hir::Operator {
        kind,
        ..operator.clone()
    }
    .map_exprs(&mut |expr| optimizer.expr(expr));
    let kind = match rewritten.kind {
        hir::OperatorKind::Update { locals, body } => hir::OperatorKind::Update {
            locals: optimizer.with_deferred(locals),
            body,
        },
        hir::OperatorKind::ReadOnly {
            result,
            locals,
            value,
        } => hir::OperatorKind::ReadOnly {
            result,
            locals: optimizer.with_deferred(locals),
            value,
        },
        kind => kind,
    };
    hir::Operator { kind, ..rewritten }
}

/// Optimizes the expressions of an operator, with these parameters and
/// local variables.
struct Optimizer<'a> {
    program: &'a hir::Program,
    params: &'a BTreeMap<String, hir::Parameter>,
    locals: &'a [hir::Local],
    /// The local constants, of `locals`, made computed when a read first needs them
    /// ([`Optimizer::locals_adapted`]).
    deferred: RefCell<BTreeSet<hir::LocalId>>,
}

impl<'a> Optimizer<'a> {
    fn new(
        program: &'a hir::Program,
        params: &'a BTreeMap<String, hir::Parameter>,
        locals: &'a [hir::Local],
    ) -> Self {
        Optimizer {
            program,
            params,
            locals,
            deferred: RefCell::new(BTreeSet::new()),
        }
    }

    /// `locals`, each made computed when a read first needs it so marked
    /// ([`hir::Local::deferred`]).
    fn with_deferred(&self, locals: Vec<hir::Local>) -> Vec<hir::Local> {
        let deferred = self.deferred.borrow();
        locals
            .into_iter()
            .enumerate()
            .map(|(i, local)| hir::Local {
                deferred: deferred.contains(&hir::LocalId(i)),
                ..local
            })
            .collect()
    }

    /// `expr`, and every expression in it, optimized: an AND of relations,
    /// restricted or not, is [`Optimizer::planned`], and a binding's value is written where
    /// it is read if it can be ([`Optimizer::moves`]), so that the plan sees all of it.
    fn expr(&self, expr: hir::Expr) -> hir::Expr {
        if let Some(merged) = restrictions_merged(&expr) {
            return self.expr(merged);
        }
        let pushing = Pushing {
            ty: &|expr| self.ty(expr),
        };
        if let Some(pushed) = pushing.pushed_down(&expr) {
            return self.expr(pushed);
        }
        match expr {
            // What a binding's value reads can't change in its body: a read-only operator's
            // variables are each bound once, and a value block assigns only its own (A0034).
            hir::Expr::Let(id, value, body)
                if self.moves(id, &value) && read_at_most_once(id, &[&*body]) =>
            {
                let value = self.moved(id, &value, None);
                self.expr(with_local_as(*body, id, &value))
            }
            // A constant read more than once, which what is around it can't change: what
            // narrows it is written both ways, and it is computed when a read first needs it.
            hir::Expr::Let(id, value, body)
                if self.locals[id.0].constant && self.moves(id, &value) =>
            {
                let values =
                    HashMap::from([(hir::Adapted::Local(id), self.moved(id, &value, None))]);
                let body = Adapting {
                    values: &values,
                    ty: &|expr| self.ty(expr),
                }
                .expr(*body);
                let mut adapted = BTreeSet::new();
                adapted_locals(&body, &mut adapted);
                if adapted.contains(&id) {
                    self.deferred.borrow_mut().insert(id);
                }
                hir::Expr::Let(id, value, Box::new(body)).map_children(|child, _| self.expr(child))
            }
            hir::Expr::Run(block, value) => {
                let (block, value) = self.values_written_where_read(*block, Some(*value));
                let block = block.map_exprs(&mut |expr| self.bindings_inlined(expr));
                let value = value.map(|value| self.bindings_inlined(value));
                let (block, value) = self.locals_adapted(block, value);
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
    /// variable whose value can be written where it is read gone, and its value written
    /// there: see [`Optimizer::named_value`].
    fn values_written_where_read(
        &self,
        mut block: hir::Block,
        mut end: Option<hir::Expr>,
    ) -> (hir::Block, Option<hir::Expr>) {
        let mut at = 0;
        while at < block.statements.len() {
            if let Some((id, value)) = self.named_value(&block, end.as_ref(), at) {
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
                    .map_blocks(&mut |inner| self.values_written_where_read(inner, None).0);
                (site, statement)
            })
            .collect();
        (block, end)
    }

    /// `expr`, with each binding of a `Let` written where it is read, as
    /// [`Optimizer::expr`] writes it, if that is its only read, which runs at most once:
    /// done before what narrows a local constant is written both ways, so that a value
    /// written in two forms has its bindings in place in each, rather than copied into
    /// each, and adapted again in each, once it is.
    fn bindings_inlined(&self, expr: hir::Expr) -> hir::Expr {
        match expr {
            hir::Expr::Let(id, value, body)
                if self.moves(id, &value) && read_at_most_once(id, &[&*body]) =>
            {
                let value = self.moved(id, &value, None);
                self.bindings_inlined(with_local_as(*body, id, &value))
            }
            expr => expr.map_children(|child, _| self.bindings_inlined(child)),
        }
    }

    /// `block`, and the blocks in it, and `end`, its value if it has one, with what narrows
    /// each local constant read more than once written both ways ([`Adapting`]), for the
    /// program to choose as it runs, if its value can be written where it is read and what
    /// that reads doesn't change while `block` runs, as for [`Optimizer::named_value`]. Each
    /// such constant is noted as one computed when a read first needs it
    /// ([`hir::Local::deferred`]).
    fn locals_adapted(
        &self,
        block: hir::Block,
        end: Option<hir::Expr>,
    ) -> (hir::Block, Option<hir::Expr>) {
        let mut values = HashMap::new();
        for (site, statement) in &block.statements {
            let hir::Statement::Assign(assignments) = statement else {
                continue;
            };
            let [
                hir::Assignment {
                    target: hir::Target::Local(id),
                    value,
                },
            ] = &assignments[..]
            else {
                continue;
            };
            if block.locals.contains(id)
                && block.times_assigned(*id) == 1
                && self.locals[id.0].constant
                && self.moves(*id, value)
                && self.stays_while(value, &block)
            {
                values.insert(
                    hir::Adapted::Local(*id),
                    self.moved(*id, value, Some(*site)),
                );
            }
        }
        let (mut block, end) = if values.is_empty() {
            (block, end)
        } else {
            let adapting = Adapting {
                values: &values,
                ty: &|expr| self.ty(expr),
            };
            (
                block.map_exprs(&mut |expr| adapting.expr(expr)),
                end.map(|end| adapting.expr(end)),
            )
        };
        {
            let mut deferred = self.deferred.borrow_mut();
            block.for_each_expr(&mut |expr| adapted_locals(expr, &mut deferred));
            if let Some(end) = &end {
                adapted_locals(end, &mut deferred);
            }
        }
        block.statements = block
            .statements
            .into_iter()
            .map(|(site, statement)| {
                (
                    site,
                    statement.map_blocks(&mut |inner| self.locals_adapted(inner, None).0),
                )
            })
            .collect();
        (block, end)
    }

    /// The local variable the statement of `block` at `at` declares, and its value, at the
    /// statement's site, if that is all the statement does and its value can be written
    /// wherever `block`, or `end`, reads it: it is assigned nowhere else, its value
    /// [`Optimizer::moves`], what that reads doesn't change while `block` runs, and it is
    /// read no more than once, by what runs no more than once each time `block` does.
    fn named_value(
        &self,
        block: &hir::Block,
        end: Option<&hir::Expr>,
        at: usize,
    ) -> Option<(hir::LocalId, hir::Expr)> {
        let (site, hir::Statement::Assign(assignments)) = &block.statements[at] else {
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
        if !block.locals.contains(id)
            || block.times_assigned(*id) != 1
            || !self.moves(*id, value)
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
        (!in_a_loop && read_at_most_once(*id, &exprs))
            .then(|| (*id, self.moved(*id, value, Some(*site))))
    }

    /// Whether the value of the local variable `id`, `value`, can be computed where it is
    /// read rather than where it is assigned: if `id` is a constant, which is computed when
    /// the compiler chooses, so that a failure in it happens where it is read, or if
    /// `value` is a rename of what is only read, which computes nothing. Not if it has a
    /// key to check, as checking takes all of it, nor statements, which nothing around it
    /// can reduce.
    fn moves(&self, id: hir::LocalId, value: &hir::Expr) -> bool {
        let local = &self.locals[id.0];
        (local.constant || is_rename_of_a_read(without_site(value)))
            && !has_key_to_check(local)
            && !runs(value)
    }

    /// `value`, of the local variable `id`, as it is written where `id` is read: of the
    /// type `id` is declared with, and computed at `site`, where it is assigned, if it isn't
    /// already, so that a failure in it says so. A rename of what is only read has no site,
    /// so that what reads it sees what it renames.
    fn moved(&self, id: hir::LocalId, value: &hir::Expr, site: Option<hir::Site>) -> hir::Expr {
        let value = if is_rename_of_a_read(without_site(value)) {
            without_site(value).clone()
        } else {
            site.map_or_else(|| value.clone(), |site| hir::Expr::at(site, value.clone()))
        };
        let declared = &self.locals[id.0].ty;
        if self.ty(&value) == *declared {
            value
        } else {
            value.treated(declared)
        }
    }

    /// Whether what `value` reads has the same value for as long as `block` runs: nothing
    /// in it assigns a local variable it reads, unless it declares it there, before reading
    /// it, nor a parameter, nor a variable of the program's, which an operator it calls
    /// could assign as well.
    fn stays_while(&self, value: &hir::Expr, block: &hir::Block) -> bool {
        let assigned = block.assigned();
        let mut calls = false;
        block.each_statement(&mut |statement| {
            if let hir::Statement::Call(call) = statement {
                calls |= !matches!(
                    self.program.operator(call.operator).kind,
                    hir::OperatorKind::Builtin(_)
                );
            }
        });
        let stays = |read: &hir::Expr| match read {
            hir::Expr::Local(id) => {
                block.times_assigned(*id) <= usize::from(block.locals.contains(id))
            }
            hir::Expr::Parameter(name) => !assigned.contains(&hir::Target::Parameter(name.clone())),
            hir::Expr::Variable(id) => !calls && !assigned.contains(&hir::Target::Variable(*id)),
            _ => true,
        };
        every_read(value, &stays)
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
            match guard_reads(&conjunct) {
                Some(reads) => guards.push(Guard {
                    expr: conjunct,
                    reads,
                }),
                None => rest.push(conjunct),
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

/// The attributes of the tuple at hand that `conjunct`, of a restriction's condition, reads,
/// if it is a guard ([`Guard`]): it can't fail, and reads the tuple an attribute at a time.
fn guard_reads(conjunct: &hir::Expr) -> Option<BTreeSet<String>> {
    let read = conjunct.tuple_read(0);
    (!conjunct.can_fail() && !read.whole).then_some(read.attributes)
}

/// Whether `expr` is a rename of what is only read, with nothing to compute, and the same
/// wherever in a chain of bindings it is read.
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

/// `expr`, apart from where it is computed, which only says so of what can fail.
fn without_site(expr: &hir::Expr) -> &hir::Expr {
    match expr {
        hir::Expr::At(_, inner) => without_site(inner),
        expr => expr,
    }
}

/// Whether `exprs` read the local variable `id` no more than once, and not again for each
/// tuple of something, or by statements that can run again: what is read more often than
/// that is better computed once than at each read.
fn read_at_most_once(id: hir::LocalId, exprs: &[&hir::Expr]) -> bool {
    let (mut reads, mut again) = (0, false);
    for expr in exprs {
        count_reads(id, expr, false, &mut reads, &mut again);
    }
    reads <= 1 && !again
}

/// Add to `reads` how many times `expr` reads the local variable `id`, and to `again`
/// whether one of them is computed again, as `repeated` says `expr` is. Of the two forms of
/// an adapted read, one is computed, so it reads it as often as the one that reads it more.
fn count_reads(
    id: hir::LocalId,
    expr: &hir::Expr,
    repeated: bool,
    reads: &mut usize,
    again: &mut bool,
) {
    if let hir::Expr::Local(read) = expr {
        if *read == id {
            *reads += 1;
            *again |= repeated;
        }
        return;
    }
    if let hir::Expr::AdaptedRead(read) = expr {
        let (mut shared, mut narrowed) = (0, 0);
        count_reads(id, &read.shared, repeated, &mut shared, again);
        count_reads(id, &read.narrowed, repeated, &mut narrowed, again);
        *reads += shared.max(narrowed);
        return;
    }
    expr.for_each_child(|child, tuples| {
        let repeated = repeated || tuples > 0 || expr.computes_again(child);
        count_reads(id, child, repeated, reads, again);
    });
}

/// Whether `local`, a relation, can break a key: every relation satisfies one of all its
/// attributes, which is what it has if it declares none.
fn has_key_to_check(local: &hir::Local) -> bool {
    let hir::Type::Relation(heading) = &local.ty else {
        return false;
    };
    let attributes = heading.attributes().count();
    local.keys.iter().any(|key| key.len() < attributes)
}

/// Whether `expr` has statements in it, which decide what runs next.
fn runs(expr: &hir::Expr) -> bool {
    if matches!(expr, hir::Expr::Run(..)) {
        return true;
    }
    let mut found = false;
    expr.for_each_child(|child, _| found |= runs(child));
    found
}

/// Whether `stays` is true of every variable, parameter and local variable `expr` reads.
fn every_read(expr: &hir::Expr, stays: &dyn Fn(&hir::Expr) -> bool) -> bool {
    match expr {
        hir::Expr::Local(_) | hir::Expr::Parameter(_) | hir::Expr::Variable(_) => stays(expr),
        _ => {
            let mut all = true;
            expr.for_each_child(|child, _| all &= every_read(child, stays));
            all
        }
    }
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
        | hir::Statement::Leave(_)
        | hir::Statement::Iterate(_) => {}
    }
    for block in statement.blocks() {
        for (_, inner) in &block.statements {
            collect_exprs(inner, exprs);
        }
    }
}

/// `expr`, with `value` where it reads the local variable `id`.
fn with_local_as(expr: hir::Expr, id: hir::LocalId, value: &hir::Expr) -> hir::Expr {
    match expr {
        hir::Expr::Local(read) if read == id => value.clone(),
        expr => expr.map_children(|child, _| with_local_as(child, id, value)),
    }
}

/// Add to `deferred` each local constant an adapted read in `expr` narrows.
fn adapted_locals(expr: &hir::Expr, deferred: &mut BTreeSet<hir::LocalId>) {
    if let hir::Expr::AdaptedRead(read) = expr
        && let hir::Adapted::Local(id) = read.of
    {
        deferred.insert(id);
    }
    expr.for_each_child(|child, _| adapted_locals(child, deferred));
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
        let optimizer = Optimizer::new(&program, &operator.params, locals);
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
        let params = BTreeMap::new();
        let optimizer = Optimizer::new(&program, &params, &constant.locals);
        shorthand(&optimizer, &constant.value)
    }

    fn shorthand(optimizer: &Optimizer, expr: &hir::Expr) -> String {
        match expr {
            hir::Expr::Parameter(name) => name.clone(),
            hir::Expr::Variable(id) => optimizer.program.variables[id.0].name.clone(),
            hir::Expr::ComputedConstant(id, _) => optimizer.program.constants[id.0].name.clone(),
            hir::Expr::At(_, inner) => shorthand(optimizer, inner),
            // As it reads the constant: the narrowed form, with its value in its place, is
            // planned alike.
            hir::Expr::AdaptedRead(read) => shorthand(optimizer, &read.shared),
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
                    written(optimizer, condition)
                )
            }
            hir::Expr::Extend(operand, _, attributes) => {
                let values: Vec<String> = attributes
                    .iter()
                    .map(|(name, value)| format!("{name} := {}", written(optimizer, value)))
                    .collect();
                format!(
                    "({} extend {{ {} }})",
                    shorthand(optimizer, operand),
                    values.join(", ")
                )
            }
            hir::Expr::Call(call) => called(optimizer, call, &|arg| shorthand(optimizer, arg)),
            hir::Expr::Rename(operand, renaming) => {
                let renamed: Vec<String> = renaming
                    .iter()
                    .map(|(old, new)| format!("{old} as {new}"))
                    .collect();
                format!(
                    "({} rename {{{}}})",
                    shorthand(optimizer, operand),
                    renamed.join(", ")
                )
            }
            hir::Expr::Remove(operand, names) | hir::Expr::Ungroup(operand, names) => {
                let word = if matches!(expr, hir::Expr::Remove(..)) {
                    "remove"
                } else {
                    "ungroup"
                };
                let names: Vec<&str> = names.iter().map(String::as_str).collect();
                format!(
                    "({} {word} {{{}}})",
                    shorthand(optimizer, operand),
                    names.join(", ")
                )
            }
            hir::Expr::Combine(combination, lhs, rhs) => {
                let word = match combination {
                    hir::Combination::Union => "union",
                    hir::Combination::DisjointUnion => "d_union",
                    hir::Combination::ExclusiveUnion => "xunion",
                    hir::Combination::Minus => "minus",
                    hir::Combination::IncludedMinus => "i_minus",
                    hir::Combination::DisjointChanges => "changes",
                };
                format!(
                    "({} {word} {})",
                    shorthand(optimizer, lhs),
                    shorthand(optimizer, rhs)
                )
            }
            hir::Expr::Semijoin(lhs, rhs) | hir::Expr::Antijoin(lhs, rhs) => {
                let word = if matches!(expr, hir::Expr::Semijoin(..)) {
                    "matching"
                } else {
                    "not matching"
                };
                format!(
                    "({} {word} {})",
                    shorthand(optimizer, lhs),
                    shorthand(optimizer, rhs)
                )
            }
            other => panic!("no shorthand for {other:?}"),
        }
    }

    /// `call`, in trendy, each argument as `arg` writes it.
    fn called(
        optimizer: &Optimizer,
        call: &hir::Call,
        arg: &dyn Fn(&hir::Expr) -> String,
    ) -> String {
        let name = &optimizer.program.operator(call.operator).name;
        let args: Vec<String> = call
            .args
            .iter()
            .map(|(param, value)| format!("{param}: {}", arg(value)))
            .collect();
        format!("{name}{{{}}}", args.join(", "))
    }

    /// `condition`, of a restriction, or a value an extension computes, in trendy, and a
    /// relation in it as [`shorthand`] writes it.
    fn written(optimizer: &Optimizer, condition: &hir::Expr) -> String {
        match condition {
            hir::Expr::Extract(tuple, name) if matches!(**tuple, hir::Expr::Current(..)) => {
                name.clone()
            }
            hir::Expr::Parameter(name) => name.clone(),
            hir::Expr::Constant(hir::Value::Integer(n)) => n.to_string(),
            hir::Expr::Connective(hir::Connective::And, operands) => operands
                .iter()
                .map(|operand| written(optimizer, operand))
                .collect::<Vec<_>>()
                .join(" and "),
            hir::Expr::Call(call) => called(optimizer, call, &|arg| written(optimizer, arg)),
            hir::Expr::Binary(op, lhs, rhs) => {
                let op = match op {
                    hir::BinaryOp::Add(_) => "+",
                    hir::BinaryOp::Div => "div",
                    hir::BinaryOp::Compare(hir::Comparison::Equal, _) => "=",
                    hir::BinaryOp::Compare(hir::Comparison::Greater, _) => ">",
                    hir::BinaryOp::Compare(hir::Comparison::Less, _) => "<",
                    other => panic!("no shorthand for {other:?}"),
                };
                format!(
                    "{} {op} {}",
                    written(optimizer, lhs),
                    written(optimizer, rhs)
                )
            }
            relation => shorthand(optimizer, relation),
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
        groups: relation { z: integer, inner: relation { x: integer } }, \
        size: integer, \
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
        let params = BTreeMap::new();
        let optimizer = Optimizer::new(&program, &params, &part.locals);
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

    /// The constant each read of what narrows it in `main` adapts, of the constants of
    /// [`narrowed_constants`], once optimized, as [`adapted_reads`] says.
    fn narrowing_adapts(body: &str) -> Vec<String> {
        let constants = narrowed_constants();
        adapted_reads(&format!("{constants}operator main{{}} [ {body} ];"))
    }

    /// Constants of closures, joins and what passes them on, read in the cases of
    /// [`narrowing_adapts`].
    fn narrowed_constants() -> String {
        format!(
            "const a := {SERIES} rename {{value as x}};\n\
             const b := {SERIES} rename {{value as y}};\n\
             const c := ({SERIES} rename {{value as x}} extend {{ y := x + 1 }}) tclose;\n\
             const p := times{{a, b}};\n\
             const e := {SERIES} rename {{value as x}} extend {{ z := x * 2 }};\n\
             const g := c where y > x;\n\
             const h := c where x div y > 0;\n\
             const cr := c rename {{x as w}};\n\
             const cu := c union (c where y > x);\n\
             const k := ({SERIES} extend {{ same := value }}) key {{ same }};\n"
        )
    }

    #[test]
    fn what_narrows_a_constant_read_more_than_once_is_written_both_ways() {
        let xs = format!("({SERIES} rename {{value as x}})");
        let zs = format!("({SERIES} rename {{value as z}})");
        for (body, adapted) in [
            // A closure restricted to the paths from a value, but not what reads it whole,
            // nor what restricts it otherwise, which follows all of them.
            (
                "call debug_table{r: c where x = 1}; call debug_table{r: c where x = 2}; \
                 call debug_value{v: count{r: c}};"
                    .to_string(),
                &["c", "c"][..],
            ),
            (
                "call debug_table{r: c where x > 1}; call debug_table{r: c where x > 2};"
                    .to_string(),
                &[],
            ),
            // A join restricted by what reads one of its operands, before the other is
            // joined, but not by what reads both.
            (
                "call debug_table{r: p where x = 1}; call debug_table{r: p where y = 2};"
                    .to_string(),
                &["p", "p"],
            ),
            (
                "call debug_table{r: p where x = y}; call debug_table{r: p where x < y};"
                    .to_string(),
                &[],
            ),
            // A closure joined or matched with a relation that has an attribute of it, as
            // its paths are followed from that relation's values, but not one that has none,
            // nor what isn't matched.
            (
                format!(
                    "call debug_table{{r: c join {xs}}}; call debug_table{{r: c matching {xs}}};"
                ),
                &["c", "c"],
            ),
            (
                format!(
                    "call debug_table{{r: times{{c, {zs}}}}}; \
                     call debug_table{{r: {xs} matching c}}; \
                     call debug_table{{r: c not matching {xs}}};"
                ),
                &[],
            ),
            // A join joined with another, whose operands are joined with its own.
            (
                format!(
                    "call debug_table{{r: p join {xs}}}; call debug_table{{r: times{{p, {zs}}}}};"
                ),
                &["p", "p"],
            ),
            // Not what an extension computes, which computes all of it.
            (
                "call debug_table{r: e where z = 2}; call debug_table{r: e where z = 4};"
                    .to_string(),
                &[],
            ),
            // Not with a key to check, which is of all of it.
            (
                "call debug_table{r: k where value = 1}; call debug_table{r: k where value = 2};"
                    .to_string(),
                &[],
            ),
        ] {
            assert_eq!(narrowing_adapts(&body), adapted, "{body}");
        }
    }

    #[test]
    fn what_narrows_a_constant_is_judged_as_it_will_be_once_optimized() {
        for (body, adapted) in [
            // A restriction of a restriction, as one by both conditions, if the inner one
            // can't fail: of a read, or of a constant's value.
            (
                "call debug_table{r: (c where y > x) where x = 1}; \
                 call debug_table{r: (c where y > x) where x = 2};"
                    .to_string(),
                &["c", "c"][..],
            ),
            // `g`'s value reads `c`, which its narrowed form adapts in turn.
            (
                "call debug_table{r: g where x = 1}; call debug_table{r: g where x = 2};"
                    .to_string(),
                &["g", "c", "g", "c"],
            ),
            (
                "call debug_table{r: h where x = 1}; call debug_table{r: h where x = 2};"
                    .to_string(),
                &[],
            ),
            // What reads what narrows it, passing it on, and what it passes on, as the
            // condition will restrict what that computes from.
            (
                "call debug_table{r: (c where x = 1) rename {x as w}}; \
                 call debug_table{r: (c where x = 2){y}};"
                    .to_string(),
                &["c", "c"],
            ),
            (
                "call debug_table{r: (c rename {x as w}) where w = 1}; \
                 call debug_table{r: (c extend {w := x}) where x = 2}; \
                 call debug_table{r: (p extend {w := x}) where x = 1}; \
                 call debug_table{r: (p extend {w := x}) where y = 1};"
                    .to_string(),
                &["c", "c", "p", "p"],
            ),
            // Into both operands of a union, and through what a constant's value passes on,
            // its narrowed form adapting the constant that reads in turn.
            (
                "call debug_table{r: (c union c) where x = 1};".to_string(),
                &["c", "c"],
            ),
            (
                "call debug_table{r: cr where w = 1}; call debug_table{r: cr where w = 2};"
                    .to_string(),
                &["cr", "c", "cr", "c"],
            ),
            (
                "call debug_table{r: cu where x = 1}; call debug_table{r: cu where x = 2};"
                    .to_string(),
                &["cu", "c", "c", "cu", "c", "c"],
            ),
            // Not where the condition can't go, below an extension whose values can fail.
            (
                "call debug_table{r: (c extend {w := x + 1}) where x = 2}; \
                 call debug_table{r: (c extend {w := x + 1}) where x = 3};"
                    .to_string(),
                &[],
            ),
        ] {
            assert_eq!(narrowing_adapts(&body), adapted, "{body}");
        }
    }

    /// The constant each read in `main` of `source` adapts, once optimized, in order,
    /// those in a narrowed form too, each of which reads its value, not it.
    fn adapted_reads(source: &str) -> Vec<String> {
        let program = program(source);
        let main = program.operator(program.entry);
        let hir::OperatorKind::Update { body, .. } = &main.kind else {
            panic!("`main` is an update operator");
        };
        let mut names = Vec::new();
        body.for_each_expr(&mut |expr| adapted_in(expr, &program, &mut names));
        names
    }

    /// The constant each read in `expr` adapts, as [`adapted_reads`] says.
    fn adapted_in(expr: &hir::Expr, program: &hir::Program, names: &mut Vec<String>) {
        if let hir::Expr::AdaptedRead(read) = expr
            && let hir::Adapted::Constant(id) = read.of
        {
            let name = &program.constants[id.0].name;
            let mut narrowed = Vec::new();
            reads(&read.narrowed, program, &mut narrowed);
            assert!(
                !narrowed.contains(name),
                "{name} is read where it is narrowed"
            );
            names.push(name.clone());
        }
        expr.for_each_child(|child, _| adapted_in(child, program, names));
    }

    /// The program of `main` doing nothing and `f`, of [`PARAMS`], with `body`.
    fn f_body(body: &str) -> String {
        format!("operator main{{}} [];\noperator f{{{PARAMS}}} [ {body} ];")
    }

    /// The local variables of the operator `f` in `source` that are computed where they are
    /// declared, once optimized, rather than written where they are read.
    fn declared_and_computed(source: &str) -> Vec<String> {
        let program = program(source);
        let f = program
            .operators
            .iter()
            .find(|operator| operator.name == "f")
            .expect("`f` is defined");
        let name = |id: hir::LocalId| f.locals()[id.0].name.clone();
        let mut computed = Vec::new();
        match &f.kind {
            hir::OperatorKind::Update { body, .. } => body.each_statement(&mut |statement| {
                for target in statement.assigns() {
                    if let hir::Target::Local(id) = target {
                        computed.push(name(*id));
                    }
                }
            }),
            hir::OperatorKind::ReadOnly { value, .. } => bound(value, &name, &mut computed),
            hir::OperatorKind::Builtin(_) | hir::OperatorKind::Specification { .. } => {}
        }
        computed.sort();
        computed.dedup();
        computed
    }

    /// The name of each local variable `expr` binds a value to, as `name` gives it.
    fn bound(expr: &hir::Expr, name: &dyn Fn(hir::LocalId) -> String, names: &mut Vec<String>) {
        if let hir::Expr::Let(id, ..) = expr {
            names.push(name(*id));
        }
        expr.for_each_child(|child, _| bound(child, name, names));
    }

    #[test]
    fn a_local_constants_only_read_is_its_value() {
        // As if written where it is read, so that the plan sees all of it: one join of
        // three, of a chain of constants too, restricted where it can be.
        for (named, written) in [
            (
                "const t := a join d; call debug_table{r: t join b};",
                "(a join d) join b",
            ),
            (
                "const t := a join d; const u := t join b; call debug_table{r: u where y = 1};",
                "((a join d) join b) where y = 1",
            ),
            (
                "const t := a join d; call debug_table{r: t where x = 1};",
                "(a join d) where x = 1",
            ),
        ] {
            assert_eq!(planned(&f_body(named)), planned(&f(written)), "{named}");
        }
    }

    #[test]
    fn what_narrows_a_local_constant_read_more_than_once_is_written_both_ways() {
        let reads = "call debug_table{r: t where x = 1}; call debug_table{r: t where x = 2};";
        for (declared, adapted) in [
            ("const t := a join d;", &["t", "t"][..]),
            (
                "const t := a join d; call debug_value{v: count{r: t}};",
                &["t", "t"],
            ),
            // Not a variable, nor a constant with a key, which is of all of it, nor one whose
            // value reads what changes before a read.
            ("var t := a join d;", &[]),
            ("const t := (a join d) key { x };", &[]),
            ("var n := 1; const t := a where x = n; n := 2;", &[]),
        ] {
            let source = f_body(&format!("{declared} {reads}"));
            let program = program(&source);
            let f = program
                .operators
                .iter()
                .find(|operator| operator.name == "f")
                .expect("`f` is defined");
            let hir::OperatorKind::Update { locals, body } = &f.kind else {
                panic!("`f` is an update operator");
            };
            let mut names = Vec::new();
            body.for_each_expr(&mut |expr| adapted_local_reads(expr, locals, &mut names));
            assert_eq!(names, adapted, "{source}");
            let deferred: Vec<&str> = locals
                .iter()
                .filter(|local| local.deferred)
                .map(|local| local.name.as_str())
                .collect();
            assert_eq!(deferred, &adapted[..adapted.len().min(1)], "{source}");
        }
    }

    #[test]
    fn what_narrows_a_local_constant_of_a_value_is_written_both_ways() {
        let reads = "count{r: t where x = 1} + count{r: t where x = 2}";
        for (body, adapted) in [
            // Bound by the value, or declared by a statement of its value block.
            (format!("const t := a join d; {reads}"), &["t", "t"][..]),
            (
                format!(
                    "const t := a join d; var n := 0; \
                     if n = 0 then [ n := count{{r: t where x = 3}}; ]; n + {reads}"
                ),
                &["t", "t", "t"],
            ),
            // Not a variable.
            (format!("var t := a join d; {reads}"), &[]),
        ] {
            let source =
                format!("operator main{{}} [];\noperator g{{{PARAMS}}}: integer [ {body} ];");
            let program = program(&source);
            let g = program
                .operators
                .iter()
                .find(|operator| operator.name == "g")
                .expect("`g` is defined");
            let hir::OperatorKind::ReadOnly { locals, value, .. } = &g.kind else {
                panic!("`g` is a read-only operator");
            };
            let mut names = Vec::new();
            adapted_local_reads(value, locals, &mut names);
            assert_eq!(names, adapted, "{source}");
            let deferred: Vec<&str> = locals
                .iter()
                .filter(|local| local.deferred)
                .map(|local| local.name.as_str())
                .collect();
            assert_eq!(deferred, &adapted[..adapted.len().min(1)], "{source}");
        }
    }

    /// The name of the local constant each adapted read in `expr` narrows, of `locals`.
    fn adapted_local_reads(expr: &hir::Expr, locals: &[hir::Local], names: &mut Vec<String>) {
        if let hir::Expr::AdaptedRead(read) = expr
            && let hir::Adapted::Local(id) = read.of
        {
            assert!(
                !read.narrowed.reads_local(id),
                "it is read where it is narrowed"
            );
            names.push(locals[id.0].name.clone());
        }
        expr.for_each_child(|child, _| adapted_local_reads(child, locals, names));
    }

    #[test]
    fn a_local_variables_value_is_written_where_it_is_read_if_nothing_can_tell() {
        // A constant, which may fail where it is read; a variable that only renames what
        // is read, which computes nothing; one read only in a branch, or by a read-only
        // operator; one read nowhere, which nothing computes.
        let overflows = "a extend { q := 9223372036854775807 + x }";
        for source in [
            f_body(&format!(
                "const t := {overflows}; call debug_table{{r: t}};"
            )),
            f_body("var t := a rename {x as w}; call debug_table{r: t join e};"),
            f_body("const t := a join d; if p then [ call debug_table{r: t}; ];"),
            f_body("const t := a join d;"),
            format!(
                "operator main{{}} [];\noperator f{{{PARAMS}}}: integer [ const t := {overflows}; count{{r: t}} ];"
            ),
        ] {
            assert_eq!(
                declared_and_computed(&source),
                Vec::<String>::new(),
                "{source}"
            );
        }
        // Read more than once, in a loop, or for each tuple; after what it reads changes,
        // a variable of the program's by an operator it calls too; with a key to check, or
        // statements; or a variable that computes something, where it is declared.
        let changed = "var g: relation { x: integer };\noperator h{} [ g := relation {}; ];\n";
        for source in [
            f_body("const t := a join d; call debug_table{r: t}; call debug_table{r: t join b};"),
            f_body(
                "const t := a join d; var n := 0; while n < 2 loop [ call debug_table{r: t}; n := n + 1; ];",
            ),
            f_body("const t := a join d; call debug_table{r: b where y = count{r: t}};"),
            f_body("var v := a; const t := v join d; v := a where x = 1; call debug_table{r: t};"),
            format!(
                "{changed}{}",
                f_body("const t := g join d; g := relation {}; call debug_table{r: t};")
            ),
            format!(
                "{changed}{}",
                f_body("const t := g join d; call h{}; call debug_table{r: t};")
            ),
            f_body(
                "const t: relation { x: integer, y: integer } := a join d key { x }; call debug_table{r: t};",
            ),
            f_body(
                "const t := [ var i := 0; while i < 1 loop [ i := i + 1; ]; a ]; call debug_table{r: t};",
            ),
            f_body(&format!("var t := {overflows}; call debug_table{{r: t}};")),
            f_body("var t := a join d; call debug_table{r: t};"),
            format!(
                "operator main{{}} [];\noperator f{{{PARAMS}}}: integer [ var t := {overflows}; count{{r: t}} ];"
            ),
        ] {
            assert!(
                declared_and_computed(&source).contains(&"t".to_string()),
                "{source}"
            );
        }
    }

    #[test]
    fn what_fails_alike_for_every_tuple_restricts_what_keeps_each_tuple() {
        // What can fail only where it reads no attribute fails alike for every tuple, so it
        // goes below what keeps each tuple, with all of the condition, but not where what
        // stays could decide it isn't computed, nor below what drops tuples.
        for (restriction, pushed) in [
            (
                "(d rename {x as w}) where w = size + 1",
                "((d where x = size + 1) rename {x as w})",
            ),
            (
                "d{x} where x = size + 1",
                "((d where x = size + 1) remove {y})",
            ),
            (
                "(d extend {w := x}) where y = size + 1",
                "((d where y = size + 1) extend { w := x })",
            ),
            (
                "(d union d) where x = size + 1",
                "((d where x = size + 1) union (d where x = size + 1))",
            ),
            (
                "(d extend {w := x}) where w = 1 and y = size + 1",
                "((d extend { w := x }) where w = 1 and y = size + 1)",
            ),
            (
                "(groups ungroup {inner}) where z = size + 1",
                "((groups ungroup {inner}) where z = size + 1)",
            ),
            (
                "(d minus d) where x = size + 1",
                "((d minus d) where x = size + 1)",
            ),
            (
                "(d matching a) where y = size + 1",
                "((d matching a) where y = size + 1)",
            ),
        ] {
            assert_eq!(planned(&f(restriction)), pushed, "{restriction}");
        }
    }

    #[test]
    fn a_condition_restricts_what_passes_the_attributes_it_reads_on() {
        for (restriction, pushed) in [
            // Through a renaming, renamed back, a projection, an extension it doesn't read
            // and an ungrouping of what it doesn't read, and on down as far as it can go.
            (
                "(d rename {x as w}) where w = 1 and y > 2",
                "((d where x = 1 and y > 2) rename {x as w})",
            ),
            ("d{x} where x = 1", "((d where x = 1) remove {y})"),
            (
                "(d extend {w := x}) where y = 1",
                "((d where y = 1) extend { w := x })",
            ),
            (
                "(groups ungroup {inner}) where z = 1",
                "((groups where z = 1) ungroup {inner})",
            ),
            (
                "((d rename {x as w}) union (d rename {x as w})) where w = 1",
                "(((d where x = 1) rename {x as w}) union ((d where x = 1) rename {x as w}))",
            ),
            // Into both operands of a union, a symmetric difference and a difference, and the
            // left one of a match or a mismatch.
            (
                "(d xunion d) where x = 1",
                "((d where x = 1) xunion (d where x = 1))",
            ),
            (
                "(d minus d) where x = 1",
                "((d where x = 1) minus (d where x = 1))",
            ),
            ("(d matching a) where y = 1", "((d where y = 1) matching a)"),
            (
                "(d not matching a) where y = 1",
                "((d where y = 1) not matching a)",
            ),
            // Not what can fail, which could fail for another tuple first, and the rest of a
            // condition goes on where it can.
            (
                "(d rename {x as w}) where w = 1 and 1 div y = 1",
                "(((d where x = 1) rename {x as w}) where 1 div y = 1)",
            ),
            // Not what an extension adds, nor below one whose values can fail.
            (
                "(d extend {w := x}) where w = 1",
                "((d extend { w := x }) where w = 1)",
            ),
            (
                "(d extend {w := x + 1}) where y = 1",
                "((d extend { w := x + 1 }) where y = 1)",
            ),
            // Not what is ungrouped, nor into what must not overlap or must be included.
            (
                "(groups ungroup {inner}) where x = 1",
                "((groups ungroup {inner}) where x = 1)",
            ),
            ("(d d_union d) where x = 1", "((d d_union d) where x = 1)"),
            ("(d i_minus d) where x = 1", "((d i_minus d) where x = 1)"),
        ] {
            assert_eq!(planned(&f(restriction)), pushed, "{restriction}");
        }
        // A call, below what keeps every tuple, but not below what would have dropped or
        // merged some, as it would be computed for them too.
        let big = "operator big{n: integer}: boolean [ n > 1 ];\n";
        for (restriction, pushed) in [
            (
                "(d rename {x as w}) where big{n: w}",
                "((d where big{n: x}) rename {x as w})",
            ),
            (
                "(d extend {w := x}) where big{n: y}",
                "((d where big{n: y}) extend { w := x })",
            ),
            (
                "(d matching a) where big{n: y}",
                "((d matching a) where big{n: y})",
            ),
            ("d{x} where big{n: x}", "((d remove {y}) where big{n: x})"),
            (
                "(d replace {x := y}) where big{n: y}",
                "((d extend { x := y }) where big{n: y})",
            ),
            (
                "(d replace {x := y}) where y = 1",
                "((d where y = 1) extend { x := y })",
            ),
        ] {
            assert_eq!(
                planned(&format!("{big}{}", f(restriction))),
                pushed,
                "{restriction}"
            );
        }
        // Into the operands of a union whose types differ, only what reads what they have
        // of the result's types.
        let source = |condition: &str| {
            format!(
                "type shape union;\n\
                 type square is shape {{ {{ side: integer }} }};\n\
                 type circle is shape {{ {{ radius: integer }} }};\n\
                 operator main{{}} [];\n\
                 operator f{{a: relation {{ s: square, k: integer }}, \
                 b: relation {{ s: shape, k: integer }}, t: shape}} [ \
                 call debug_table{{r: (a union b) where {condition}}}; ];"
            )
        };
        assert_eq!(
            planned(&source("k = 1")),
            "((a where k = 1) union (b where k = 1))"
        );
        assert_eq!(planned(&source("s = t")), "((a union b) where s = t)");
    }

    #[test]
    fn a_restriction_of_a_restriction_is_one_by_both_conditions() {
        // Of an AND, planned as one, so that each part restricts its operands as soon as it
        // can, and of more than two.
        assert_eq!(
            planned(&f("(times{a, b} where x = 1) where y > 1")),
            planned(&f("times{a, b} where x = 1 and y > 1"))
        );
        assert_eq!(
            planned(&f("((d where x = 1) where y > 1) where x = y")),
            "(d where x = 1 and y > 1 and x = y)"
        );
        // Not if the inner condition can fail, as it is computed for every tuple, though
        // the outer one would decide the `and` without it; but the outer one can.
        assert_eq!(
            planned(&f("(d where 1 div x = 1) where y > 1")),
            "((d where 1 div x = 1) where y > 1)"
        );
        assert_eq!(
            planned(&f("(d where y > 1) where 1 div x = 1")),
            "(d where y > 1 and 1 div x = 1)"
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
