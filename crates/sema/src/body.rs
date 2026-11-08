//! Blocks of statements that are run, an update operator's body, or a
//! read-only operator's or a value block's that decides what runs next:
//! their statements, their local variables, and the expressions in them.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use trendy_diag::{CodeKind, Diagnostic, Span};
use trendy_hir as hir;
use trendy_syntax::SyntaxToken;
use trendy_syntax::ast::{self, AstNode, VarDecl};

use crate::assign::{Assignee, Assigning, Declared};
use crate::check::{
    self, Checker, Lookup, Meaning, Names, Representation, Written, argument_names,
};
use crate::locals::{Declaration, Declaring, Local, Locals};
use crate::narrowed::{self, Facts};
use crate::operators::{Chosen, Operators, Signature};
use crate::types::{Expected, Origin};
use crate::{Namespaces, Sites, assign, errors, locals, ops, scope};

/// Everything a body can refer to.
pub(crate) struct Context<'a> {
    /// The program's units' files.
    pub(crate) files: &'a crate::Files,
    /// Where each place in the file is.
    pub(crate) sites: &'a Sites<'a>,
    pub(crate) namespaces: &'a Namespaces,
    pub(crate) operators: &'a Operators,
    /// Each valid read-only operator's value.
    pub(crate) values: &'a HashMap<hir::OperatorId, hir::Expr>,
    /// The read-only operators that reach a cycle of calls, or are in one.
    pub(crate) recursive: &'a HashSet<hir::OperatorId>,
    /// Each variable, by id: `None` for one with errors, already reported.
    pub(crate) variables: &'a [Option<hir::Variable>],
    /// Each constant, by id: `None` for one with errors, already reported.
    pub(crate) constants: &'a [Option<hir::Constant>],
    /// Each variable's id and declaration, by the name its unit writes.
    pub(crate) declared: &'a scope::Variables,
}

impl Context<'_> {
    /// What reading the constant `id` is, if it has no errors.
    pub(crate) fn constant_read(&self, id: hir::ConstantId) -> Lookup {
        match &self.constants[id.0] {
            Some(constant) => Lookup::Found(crate::globals::read_of(id, constant)),
            None => Lookup::Reported,
        }
    }

    /// What evaluating a call in a body substitutes: the read-only
    /// operators' values.
    pub(crate) fn bindings(&self) -> ops::Bindings<'_> {
        ops::Bindings {
            operators: self.operators,
            namespaces: self.namespaces,
            values: self.values,
            recursive: self.recursive,
        }
    }

    /// Whether the read-only operator `id` has a valid value: every one is
    /// checked with the globals, and a call of a specification computes one
    /// of its versions, or its own value, if it has a body.
    pub(crate) fn checked(&self, id: hir::OperatorId) -> bool {
        let signature = self.operators.get(id);
        let bodiless = signature.decl.as_ref().is_some_and(|d| d.body().is_none());
        let own = if bodiless {
            signature.valid
        } else {
            self.values.contains_key(&id)
        };
        own && signature
            .versions
            .iter()
            .all(|version| self.values.contains_key(&version.operator))
    }
}

/// A call, in the body of the operator `caller`, of `callee`, which gives one
/// of its type parameters, `parameter`, a type built from `caller`'s own, `ty`.
/// If it leads back to `caller`, each time around gives it a larger type.
pub(crate) struct Growing {
    pub(crate) caller: hir::OperatorId,
    pub(crate) callee: hir::OperatorId,
    /// The callee's name, where the call writes it.
    pub(crate) span: Span,
    pub(crate) parameter: hir::TypeParameter,
    pub(crate) ty: hir::Type,
}

impl Growing {
    /// Those of a call in the body of `within` of `called`, its name written
    /// at `span`, which gives its type parameters `types`. A type parameter
    /// passed on as it is never grows.
    pub(crate) fn of(
        within: hir::OperatorId,
        called: &Signature,
        types: &[hir::Type],
        span: Span,
    ) -> Vec<Growing> {
        called
            .types
            .iter()
            .zip(types)
            .filter(|(_, ty)| {
                !matches!(ty, hir::Type::Parameter(_)) && ty.parameters().next().is_some()
            })
            .map(|(parameter, ty)| Growing {
                caller: within,
                callee: called.id,
                span,
                parameter: parameter.clone(),
                ty: ty.clone(),
            })
            .collect()
    }
}

/// What can only be checked once every update operator's body is (see
/// [`deferred_checks`]).
#[derive(Default)]
pub(crate) struct Deferred {
    /// Each call that gives a type parameter a type built from its
    /// caller's own.
    growing: Vec<Growing>,
    /// Each call in a `for` loop.
    in_loops: Vec<LoopCall>,
    /// Each variable of the program's passed to a `var` parameter.
    passed: Vec<Passed>,
    /// Each call statement of an update operator, by the operator it is
    /// in: the operator called, and where its name is written.
    calls: Vec<(hir::OperatorId, hir::OperatorId, Span)>,
    /// Each call statement in a transaction, which mustn't read input or
    /// write output, at any depth of the calls it makes.
    in_transactions: Vec<TransactionCall>,
    /// Each call statement in no transaction of its operator's, as for `calls`:
    /// what it leads to may run with none.
    outside_transactions: Vec<(hir::OperatorId, hir::OperatorId, Span)>,
}

/// A call, its callee's name written at `span`, of the operator `callee`,
/// in a transaction, the innermost one's keyword written at `transaction`.
struct TransactionCall {
    callee: hir::OperatorId,
    span: Span,
    transaction: Span,
}

/// A call, at `span`, that passes the variable of the program's
/// `variable`, `name`, to the `var` parameter `param` of the update
/// operator `callee`, which mustn't read or assign it by its own name.
struct Passed {
    callee: hir::OperatorId,
    variable: hir::VariableId,
    name: String,
    param: String,
    span: Span,
}

/// A call, at `span`, in one or more `for` loops, of the update operator
/// `callee`, which mustn't assign a variable of the program's that a loop's
/// array is computed from.
struct LoopCall {
    callee: hir::OperatorId,
    span: Span,
    /// Each such variable, its name, and where its loop's array is written.
    looped: Vec<(hir::VariableId, String, Span)>,
}

/// What a `for` loop keeps from changing while it runs: the variables its
/// array is computed from, of the program and local, and where the array is
/// written.
pub(crate) struct Frozen {
    targets: Vec<hir::Target>,
    array: Span,
}

/// What is around a block of statements that are run: what the names that
/// aren't its local variables refer to, and what the block can't do there.
pub(crate) trait Around: Names {
    /// What decides whether a local variable of the block can have its name.
    fn declaring(&self) -> Declaring<'_>;

    /// Why `statement` can't be in the block, if it can't.
    fn refused(&self, statement: &ast::Statement) -> Option<Diagnostic>;

    /// Whether `name`, written at `span`, is also a variable of the program's,
    /// which a local variable of that name hides.
    fn is_variable(&self, name: &str, span: Span) -> bool;

    /// The variable `name`, written at `span`, not one of the block's local
    /// variables, `locals`, that an assignment assigns, or `None` after
    /// reporting why it can't be assigned.
    fn assigned_around(
        &mut self,
        name: &str,
        span: Span,
        locals: &[&str],
        errors: &mut Vec<Diagnostic>,
    ) -> Option<Assignee>;

    /// The variable of the program's `written`, qualified by the name of
    /// `unit`, that an assignment assigns, or `None` after reporting why it
    /// can't be assigned.
    fn assigned_in_around(
        &mut self,
        written: &ast::QualifiedName,
        unit: hir::FileId,
        errors: &mut Vec<Diagnostic>,
    ) -> Option<Assignee>;

    /// Whether `name` is an `out` parameter, of an update operator whose
    /// body the block is.
    fn is_out(&self, _name: &str) -> bool {
        false
    }

    /// What a `return`'s value is expected to be: a read-only operator's
    /// result, whose body the block is.
    fn result(&self) -> Option<Expected> {
        None
    }
}

/// What only an update operator's body can do, which is to call one.
struct Update<'a, 'b> {
    cx: &'a Context<'b>,
    within: hir::OperatorId,
    /// What can only be checked once every body is.
    deferred: &'a mut Deferred,
}

impl Update<'_, '_> {
    /// Whether `name` is a variable of the program's that the operator's
    /// body sees, which an operator it calls may assign.
    fn sees_variable(&self, name: &str) -> bool {
        let signature = self.cx.operators.get(self.within);
        let body = signature
            .decl
            .as_ref()
            .map(AstNode::span)
            .expect("an operator with a body is declared with it");
        self.cx.declared.find(name, body).is_some() && !signature.is_parameter(name)
    }
}

/// The statements of the update operator `op`, whose id is `within`, and
/// its local variables. What can only be checked once every body is goes
/// in `deferred`.
pub(crate) fn body(
    op: &ast::Operator,
    within: hir::OperatorId,
    cx: &Context,
    deferred: &mut Deferred,
    errors: &mut Vec<Diagnostic>,
) -> Option<(Vec<hir::Local>, hir::Block)> {
    let body = op.body()?;
    let mut around = UpdateNames { cx, within };
    let update = Update {
        cx,
        within,
        deferred,
    };
    let mut statements = Statements::new(&mut around, Some(update));
    let checked: Vec<ast::Statement> = body.statements().collect();
    let block = statements.block(&checked, errors);
    errors.extend(pointless_exits_at_end(&checked, "return"));
    let block = block?;
    // Every `out` parameter is assigned, whichever way the body goes.
    let signature = cx.operators.get(within);
    let mut ok = true;
    for (name, param) in &signature.params {
        if param.mode != hir::Mode::Out
            || statements
                .unassigned
                .assigned
                .contains(&NoValueYet::Out(name.clone()))
            || statements
                .unassigned
                .reported
                .contains(&NoValueYet::Out(name.clone()))
        {
            continue;
        }
        if let Some(ident) = signature
            .param_decl(name)
            .and_then(|d| d.name())
            .and_then(|n| n.ident())
        {
            errors.push(errors::unassigned_out(name, &signature.name, &ident));
            ok = false;
        }
    }
    ok.then(|| (statements.locals.finish(), block))
}

/// The errors of the `iterate`s that end `statements`, a loop's block, or a
/// branch of an `if` that does: each skips nothing.
fn pointless_iterate(statements: &[ast::Statement]) -> Vec<Diagnostic> {
    pointless_exits_at_end(statements, "iterate")
}

/// The errors of the exits `word`, `iterate` or `return;`, that end
/// `statements`, or a branch of an `if` that does, as nothing would run
/// between each and where it goes.
fn pointless_exits_at_end(statements: &[ast::Statement], word: &str) -> Vec<Diagnostic> {
    let pointless = |keyword: Option<SyntaxToken>, removal: Span| {
        keyword.map(|keyword| {
            errors::pointless_exit(word, trendy_syntax::span(keyword.text_range()), removal)
        })
    };
    match statements.last() {
        Some(ast::Statement::Iterate(stmt)) if word == "iterate" => {
            pointless(stmt.keyword(), stmt.removal())
                .into_iter()
                .collect()
        }
        Some(ast::Statement::Return(stmt)) if word == "return" && stmt.value().is_none() => {
            pointless(stmt.keyword(), stmt.removal())
                .into_iter()
                .collect()
        }
        Some(ast::Statement::If(stmt)) => {
            let then: Vec<ast::Statement> = stmt
                .then_block()
                .map(|body| body.statements().collect())
                .unwrap_or_default();
            let mut found = pointless_exits_at_end(&then, word);
            match stmt.else_branch() {
                Some(ast::ElseBranch::Block(body)) => {
                    let otherwise: Vec<ast::Statement> = body.statements().collect();
                    found.extend(pointless_exits_at_end(&otherwise, word));
                }
                Some(ast::ElseBranch::If(inner)) => {
                    found.extend(pointless_exits_at_end(&[ast::Statement::If(inner)], word));
                }
                None => {}
            }
            found
        }
        _ => Vec::new(),
    }
}

/// The statements of a body or a value block whose statements decide what
/// runs next, `statements`, and its value, `written`, of the type `expected`
/// says, if it says one, which they are in scope for, whose surroundings
/// are `around`: its local variables, and its value as a [`hir::Expr::Run`],
/// or `None` after reporting why it has none. Its local variables' ids
/// follow those of what's around it.
pub(crate) fn run(
    around: &mut dyn Around,
    statements: &[ast::Statement],
    written: Option<&ast::Expr>,
    expected: Option<&Expected>,
    errors: &mut Vec<Diagnostic>,
) -> (Vec<hir::Local>, Option<hir::Expr>) {
    let mut checker = Statements::new(around, None);
    let (block, value) = checker.block_then(statements, errors, |checker, errors| {
        let written = written?;
        let site = checker.around.site(written.span());
        let mut names = checker.names();
        let value = Checker::new(&mut names).expr(written, expected, errors)?;
        Some(hir::Expr::at(site, value))
    });
    // A value after a statement that always leaves is never computed.
    if let (Some(block), Some(written)) = (&block, written)
        && block.always_exits()
    {
        let exit = statements
            .iter()
            .zip(&block.statements)
            .find(|(_, (_, statement))| statement.always_exits())
            .map(|(written, _)| written);
        if let Some(exit) = exit {
            errors.push(errors::unreachable_value(exit, statements.last(), written));
        }
        return (checker.locals.finish(), None);
    }
    let value = block
        .zip(value)
        .map(|(block, value)| hir::Expr::Run(Box::new(block), Box::new(value)));
    (checker.locals.finish(), value)
}

/// What has no value until a statement assigns it: an update operator's
/// `out` parameter, or a local variable declared without a value, whose type
/// has no default.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum NoValueYet {
    Out(String),
    Local(hir::LocalId),
}

/// What is known of what has no value yet where the statement being checked
/// is: which are assigned on every path to it, and where it reads and assigns
/// them, which [`Statements::settle`] then reports and counts.
#[derive(Default)]
pub(crate) struct Unassigned {
    /// The local variables declared without a value, with their declarations.
    locals: BTreeMap<hir::LocalId, VarDecl>,
    /// Those assigned on every path to the statement.
    assigned: BTreeSet<NoValueYet>,
    /// Each read of one in the statement, and where.
    reads: Vec<(NoValueYet, Span)>,
    /// Each the statement assigns, and where it names it: an assignment's
    /// target, which a change such as `insert` reads, or an `out`
    /// argument.
    targets: Vec<(NoValueYet, Span)>,
    /// Those read before they are assigned, already reported: making one
    /// `var`, as the fix does, makes assigning it on every path needless
    /// too.
    reported: BTreeSet<NoValueYet>,
}

impl Unassigned {
    /// What `target` is of what has no value until it is assigned, if it is
    /// any, of those of an operator with the `out` parameters `is_out` says.
    fn of(&self, target: &hir::Target, is_out: impl Fn(&str) -> bool) -> Option<NoValueYet> {
        match target {
            hir::Target::Parameter(name) if is_out(name) => Some(NoValueYet::Out(name.clone())),
            hir::Target::Local(id) if self.locals.contains_key(id) => Some(NoValueYet::Local(*id)),
            _ => None,
        }
    }
}

/// Checks a block of statements, each block's local variables in scope in
/// it, with `around` around them.
struct Statements<'a, 'b> {
    around: &'a mut (dyn Around + 'a),
    /// An update operator's body's, if this is one.
    update: Option<Update<'a, 'b>>,
    locals: Locals,
    /// What each `for` loop the statement being checked is in keeps from
    /// changing, outermost first.
    frozen: Vec<Frozen>,
    /// Where each transaction the statement being checked is in writes its
    /// keyword, outermost first.
    transactions: Vec<Span>,
    /// What has no value yet of what is assigned on every path to the
    /// statement being checked, and its reads of it.
    unassigned: Unassigned,
    /// What the tests of the `if` statements and `while` loops the
    /// statement being checked is in show of the names' values, but of
    /// those a statement before it may have assigned.
    narrowed: Facts,
    /// Where a statement may assign each name it ended what tests showed
    /// of, in the branch or block being checked.
    ended: BTreeMap<String, Span>,
    /// How many loops of these statements the statement being checked is
    /// in, which `leave` and `iterate` are of.
    loops: usize,
}

impl<'a, 'b> Statements<'a, 'b> {
    fn new(around: &'a mut (dyn Around + 'a), update: Option<Update<'a, 'b>>) -> Self {
        let locals = Locals::from(around.next_local());
        Statements {
            around,
            update,
            locals,
            frozen: Vec::new(),
            transactions: Vec::new(),
            unassigned: Unassigned::default(),
            narrowed: Facts::default(),
            ended: BTreeMap::new(),
            loops: 0,
        }
    }
}

impl Statements<'_, '_> {
    fn names(&mut self) -> BodyNames<'_> {
        BodyNames {
            around: &mut *self.around,
            locals: &mut self.locals,
            frozen: &self.frozen,
            unassigned: &mut self.unassigned,
            narrowed: &self.narrowed,
            ended: &self.ended,
        }
    }

    /// What tests show of the names' values in `condition`'s statements if
    /// it is true, and if it is false.
    fn facts(&mut self, condition: &ast::Expr) -> (Facts, Facts) {
        let mut names = self.names();
        let checker = Checker::new(&mut names);
        (
            Facts::of(condition, true, &checker),
            Facts::of(condition, false, &checker),
        )
    }

    /// The names narrowed that `assigns`, of a statement, may assign, each
    /// with where: those it names, and, if it calls an update operator,
    /// the program's variables, which the operator may assign.
    fn assigned(&self, assigns: &narrowed::Assigns) -> BTreeMap<String, Span> {
        // Only an update operator's body calls one.
        let global = |name: &str| {
            self.update
                .as_ref()
                .is_some_and(|update| update.sees_variable(name))
                && self.locals.find(name).is_none()
        };
        assigns.of_names(self.narrowed.names(), global)
    }

    /// Report each read of what has no value yet in the statement just
    /// checked, `statement`, before it is assigned on every path to there,
    /// then count each it assigns as assigned from then on: if it has
    /// errors, each it names as assigned. A change to one that reads it,
    /// such as `insert`, reads it where it names it.
    fn settle(&mut self, statement: Option<&hir::Statement>, errors: &mut Vec<Diagnostic>) {
        let targets = std::mem::take(&mut self.unassigned.targets);
        let Some(statement) = statement else {
            self.flush(errors);
            self.unassigned
                .assigned
                .extend(targets.into_iter().map(|(name, _)| name));
            return;
        };
        let assigned: Vec<&hir::Target> = match statement {
            hir::Statement::Assign(assignments) => {
                for assignment in assignments {
                    let around = &*self.around;
                    let Some(assigned) = self
                        .unassigned
                        .of(&assignment.target, |name| around.is_out(name))
                    else {
                        continue;
                    };
                    if ops::targets_read(&assignment.value).contains(&assignment.target)
                        && !self.unassigned.reads.iter().any(|(n, _)| *n == assigned)
                        && let Some((_, span)) = targets.iter().find(|(n, _)| *n == assigned)
                    {
                        self.unassigned.reads.push((assigned, *span));
                    }
                }
                assignments.iter().map(|a| &a.target).collect()
            }
            hir::Statement::Call(call) => call.updated.values().collect(),
            _ => Vec::new(),
        };
        self.flush(errors);
        for target in assigned {
            let around = &*self.around;
            if let Some(assigned) = self.unassigned.of(target, |name| around.is_out(name)) {
                self.unassigned.assigned.insert(assigned);
            }
        }
    }

    /// Report each read of what has no value yet checked since the last,
    /// before it is assigned on every path to here.
    fn flush(&mut self, errors: &mut Vec<Diagnostic>) {
        let mut reported: Vec<Span> = Vec::new();
        for (read, span) in std::mem::take(&mut self.unassigned.reads) {
            if self.unassigned.assigned.contains(&read) || reported.contains(&span) {
                continue;
            }
            reported.push(span);
            match &read {
                NoValueYet::Out(name) => {
                    // Only an update operator has `out` parameters.
                    let decl = self
                        .update
                        .as_ref()
                        .and_then(|update| update.cx.operators.get(update.within).param_decl(name));
                    errors.push(errors::read_before_assigned(name, span, decl.as_ref()));
                }
                NoValueYet::Local(id) => {
                    if let Some(decl) = self.unassigned.locals.get(id) {
                        errors.push(errors::local_read_before_assigned(decl, span));
                    }
                }
            }
            self.unassigned.reported.insert(read);
        }
    }

    /// The block of `statements`, whose local variables are in scope in
    /// all of it, or `None` after reporting its errors.
    fn block(
        &mut self,
        statements: &[ast::Statement],
        errors: &mut Vec<Diagnostic>,
    ) -> Option<hir::Block> {
        self.block_then(statements, errors, |_, _| ()).0
    }

    /// As [`Statements::block`], and what `then` makes while its local
    /// variables are still in scope.
    fn block_then<T>(
        &mut self,
        statements: &[ast::Statement],
        errors: &mut Vec<Diagnostic>,
        then: impl FnOnce(&mut Self, &mut Vec<Diagnostic>) -> T,
    ) -> (Option<hir::Block>, T) {
        let mut ok = self
            .locals
            .enter(statements.iter().cloned(), self.around.declaring(), errors);
        let checked = self.each_sited(statements, &mut ok, errors);
        let made = then(self, errors);
        let locals = self.locals.exit();
        let block = ok.then_some(hir::Block {
            statements: checked,
            locals,
        });
        (block, made)
    }

    /// Each of `statements`, of a block, with its site, those with errors
    /// left out, after reporting them, which makes `ok` false. Those after
    /// one that always leaves the block never run, which is reported, once.
    fn each_sited(
        &mut self,
        statements: &[ast::Statement],
        ok: &mut bool,
        errors: &mut Vec<Diagnostic>,
    ) -> Vec<(hir::Site, hir::Statement)> {
        let mut checked = Vec::new();
        let (mut exit, mut reported) = (None, false);
        for (at, stmt) in statements.iter().enumerate() {
            if let (Some(exit), Some(last)) = (exit.take(), statements.last()) {
                errors.push(errors::unreachable(exit, stmt, last));
                (*ok, reported) = (false, true);
            }
            match self.sited(stmt, errors) {
                Some((site, statement)) => {
                    if !reported && statement.always_exits() && at + 1 < statements.len() {
                        exit = Some(stmt);
                    }
                    checked.push((site, statement));
                }
                None => *ok = false,
            }
        }
        checked
    }

    /// `stmt`, with its site, where it starts, or `None` after reporting
    /// its errors.
    fn sited(
        &mut self,
        stmt: &ast::Statement,
        errors: &mut Vec<Diagnostic>,
    ) -> Option<(hir::Site, hir::Statement)> {
        let site = self
            .around
            .site(trendy_syntax::span(stmt.syntax().text_range()));
        Some((site, self.statement(stmt, errors)?))
    }

    /// `stmt`, after which nothing is known of the value of a name it may
    /// assign (see [`narrowed::Assigns`]). A loop's statements run again
    /// after one does, and so does its condition, so it is not known in
    /// them either.
    fn statement(
        &mut self,
        stmt: &ast::Statement,
        errors: &mut Vec<Diagnostic>,
    ) -> Option<hir::Statement> {
        let assigns = narrowed::Assigns::of(stmt);
        let assigned = self.assigned(&assigns);
        if matches!(
            stmt,
            ast::Statement::While(_) | ast::Statement::Loop(_) | ast::Statement::For(_)
        ) {
            narrowed::forget(&mut self.narrowed, &assigned, &mut self.ended);
        }
        let checked = self.checked_statement(stmt, errors);
        narrowed::forget(&mut self.narrowed, &assigned, &mut self.ended);
        for name in &assigns.updated {
            self.narrowed.updated(name);
        }
        checked
    }

    fn checked_statement(
        &mut self,
        stmt: &ast::Statement,
        errors: &mut Vec<Diagnostic>,
    ) -> Option<hir::Statement> {
        if let Some(refused) = self.around.refused(stmt) {
            errors.push(refused);
            self.settle(None, errors);
            return None;
        }
        let checked = match stmt {
            ast::Statement::Call(stmt) => self.call(stmt, errors),
            ast::Statement::Assign(stmt) => {
                let clauses: Vec<_> = stmt.clauses().collect();
                assign::statement(&clauses, &mut self.names(), errors)
            }
            ast::Statement::Block(block) => assign::block(block, &mut self.names(), errors),
            ast::Statement::Return(stmt) => self.return_stmt(stmt, errors),
            ast::Statement::Leave(stmt) => {
                self.loop_exit(stmt.keyword(), hir::Statement::Leave, errors)
            }
            ast::Statement::Iterate(stmt) => {
                self.loop_exit(stmt.keyword(), hir::Statement::Iterate, errors)
            }
            ast::Statement::Var(decl) => self.declaration(decl, errors),
            // These settle their parts as they check them.
            ast::Statement::If(stmt) => return self.if_stmt(stmt, errors),
            ast::Statement::While(stmt) => {
                return self.loop_stmt(
                    stmt.keyword(),
                    stmt.condition(),
                    stmt.body(),
                    hir::Check::Before,
                    stmt.span(),
                    errors,
                );
            }
            ast::Statement::Loop(stmt) => {
                return self.loop_stmt(
                    stmt.while_keyword(),
                    stmt.condition(),
                    stmt.body(),
                    hir::Check::After,
                    stmt.span(),
                    errors,
                );
            }
            ast::Statement::For(stmt) => return self.for_stmt(stmt, errors),
            ast::Statement::Transaction(stmt) => return self.transaction_stmt(stmt, errors),
            ast::Statement::Fail(stmt) => {
                self.signal_stmt(errors::Signal::Fail, stmt.keyword(), stmt.error(), errors)
            }
            ast::Statement::Warn(stmt) => {
                self.signal_stmt(errors::Signal::Warn, stmt.keyword(), stmt.error(), errors)
            }
        };
        if checked.is_none() {
            self.count_as_assigned(stmt);
        }
        self.settle(checked.as_ref(), errors);
        checked
    }

    /// Count what `stmt`, which has errors, names to assign as assigned, so that
    /// reading it later isn't an error of its own: as a `var` or `out` argument of a
    /// call that couldn't be checked.
    fn count_as_assigned(&mut self, stmt: &ast::Statement) {
        for name in narrowed::Assigns::of(stmt).names.keys() {
            let assigned = match self.locals.find(name) {
                Some(Local::Declared(id, ..)) => NoValueYet::Local(id),
                _ => NoValueYet::Out(name.clone()),
            };
            self.unassigned.assigned.insert(assigned);
        }
    }

    /// `return;`, or `return e;`, of a read-only operator's result type: an
    /// update operator returns once each of its `out` parameters is assigned,
    /// on every path to there. Where it can't be, `refused` says (see
    /// [`Around::refused`]).
    fn return_stmt(
        &mut self,
        stmt: &ast::ReturnStmt,
        errors: &mut Vec<Diagnostic>,
    ) -> Option<hir::Statement> {
        let Some(written) = stmt.value() else {
            self.returned_assigned(stmt, errors);
            return Some(hir::Statement::Return(None));
        };
        let expected = self.around.result();
        let value = Checker::new(&mut self.names()).expr(&written, expected.as_ref(), errors);
        self.flush(errors);
        Some(hir::Statement::Return(Some(value?)))
    }

    /// Report each `out` parameter of the update operator whose body this is
    /// that isn't assigned on every path to `stmt`, a `return`, which leaves it.
    fn returned_assigned(&mut self, stmt: &ast::ReturnStmt, errors: &mut Vec<Diagnostic>) {
        let Some(update) = &self.update else {
            return;
        };
        let signature = update.cx.operators.get(update.within);
        let span = stmt
            .keyword()
            .map_or_else(|| stmt.span(), |t| trendy_syntax::span(t.text_range()));
        for (name, param) in &signature.params {
            let out = NoValueYet::Out(name.clone());
            if param.mode != hir::Mode::Out
                || self.unassigned.assigned.contains(&out)
                || self.unassigned.reported.contains(&out)
            {
                continue;
            }
            if let Some(ident) = signature
                .param_decl(name)
                .and_then(|d| d.name())
                .and_then(|n| n.ident())
            {
                errors.push(errors::unassigned_at_return(
                    name,
                    &signature.name,
                    span,
                    &ident,
                ));
                self.unassigned.reported.insert(out);
            }
        }
    }

    /// `leave;` or `iterate;`, `exit`, written with `keyword`, of the innermost
    /// loop of these statements, which there must be.
    fn loop_exit(
        &mut self,
        keyword: Option<SyntaxToken>,
        exit: hir::Statement,
        errors: &mut Vec<Diagnostic>,
    ) -> Option<hir::Statement> {
        if self.loops > 0 {
            return Some(exit);
        }
        let keyword = keyword?;
        let word = if matches!(exit, hir::Statement::Leave) {
            "leave"
        } else {
            "iterate"
        };
        let in_block = self.update.is_none() && self.around.result().is_none();
        errors.push(errors::exit_outside_loop(
            word,
            trendy_syntax::span(keyword.text_range()),
            in_block,
        ));
        None
    }

    /// `fail e;` or `warn e;`, as `signal` says, written `keyword` and
    /// `written`: the program fails with the error `e`, when it runs, or
    /// warns with it, and carries on.
    fn signal_stmt(
        &mut self,
        signal: errors::Signal,
        keyword: Option<SyntaxToken>,
        written: Option<ast::Expr>,
        errors: &mut Vec<Diagnostic>,
    ) -> Option<hir::Statement> {
        let (keyword, written) = (keyword?, written?);
        let error = {
            let mut names = self.names();
            Checker::new(&mut names).error(&written, &keyword, signal, errors)
        };
        self.flush(errors);
        let error = error?;
        Some(match signal {
            errors::Signal::Fail => hir::Statement::Fail(error),
            errors::Signal::Warn => hir::Statement::Warn(error),
        })
    }

    /// `for x in a loop [ … ];`: the block run for each element of the
    /// array, with the loop's variable its value. Neither the variable nor
    /// a variable the array is computed from is assigned in the block:
    /// directly, which is reported here, or by a call, once every body is
    /// checked (see [`deferred_checks`]).
    fn for_stmt(
        &mut self,
        stmt: &ast::ForStmt,
        errors: &mut Vec<Diagnostic>,
    ) -> Option<hir::Statement> {
        let (variable, written, body) = (stmt.variable()?, stmt.array()?, stmt.body()?);
        let name = variable.ident()?.text().to_string();
        let statements: Vec<ast::Statement> = body.statements().collect();
        if statements.is_empty() {
            errors.push(errors::empty_for(stmt.span(), &body));
        }
        let checked = {
            let mut names = self.names();
            let mut checker = Checker::new(&mut names);
            checker.expr(&written, None, errors).map(|array| {
                let ty = checker.ty(&array);
                (array, ty)
            })
        };
        self.flush(errors);
        let (array, ty) = checked?;
        let hir::Type::Array(element) = ty else {
            errors.push(errors::not_iterable(&written, &ty));
            return None;
        };
        self.frozen.push(Frozen {
            targets: ops::targets_read(&array),
            array: written.span(),
        });
        let local = hir::Local {
            name,
            ty: *element,
            keys: Vec::new(),
        };
        let (id, mut ok) = self.locals.enter_loop(
            &variable,
            local,
            statements.iter().cloned(),
            self.around.declaring(),
            errors,
        );
        // The block may not run, so what it assigns isn't assigned after.
        let before = self.unassigned.assigned.clone();
        self.loops += 1;
        let checked = self.each_sited(&statements, &mut ok, errors);
        self.loops -= 1;
        self.unassigned.assigned = before;
        let locals = self.locals.exit();
        self.frozen.pop();
        errors.extend(pointless_iterate(&statements));
        let body = hir::Block {
            statements: checked,
            locals,
        };
        ok.then_some(hir::Statement::For(id?, array, body))
    }

    /// `transaction [ … ];` or `transaction collecting e [ … ];`: the
    /// block's statements, undone, all of them, if one fails, and with
    /// `collecting`, the errors they give added to the variable `e`. A
    /// failure it collects undoes what its block assigned, so an `out`
    /// parameter the block assigns isn't assigned after it; one it doesn't
    /// collect goes on, so after it, the block ran.
    fn transaction_stmt(
        &mut self,
        stmt: &ast::TransactionStmt,
        errors: &mut Vec<Diagnostic>,
    ) -> Option<hir::Statement> {
        let collecting = stmt
            .collecting()
            .map(|collecting| self.collecting(stmt, &collecting, errors));
        let statements: Vec<ast::Statement> = stmt
            .body()
            .map(|body| body.statements().collect())
            .unwrap_or_default();
        let mut ok = self
            .locals
            .enter(statements.iter().cloned(), self.around.declaring(), errors);
        let before = self.unassigned.assigned.clone();
        let keyword = stmt.keyword().map_or_else(
            || stmt.span(),
            |keyword| trendy_syntax::span(keyword.text_range()),
        );
        self.transactions.push(keyword);
        let mut checked = Vec::new();
        for statement in &statements {
            match self.sited(statement, errors) {
                Some(statement) => checked.push(statement),
                None => ok = false,
            }
        }
        self.transactions.pop();
        let locals = self.locals.exit();
        if collecting.is_some() {
            self.unassigned.assigned = before;
        }
        ok &= stmt.body().is_some() && collecting.as_ref().is_none_or(Option::is_some);
        let body = hir::Block {
            statements: checked,
            locals,
        };
        ok.then(|| hir::Statement::Transaction(collecting.flatten(), body))
    }

    /// The variable `collecting`, of the transaction `stmt`, names, which
    /// the transaction adds its errors to: one the operator can assign, of
    /// type `relation transaction_collection`, the prelude's heading,
    /// declared before. Adding to it reads it, so an `out` parameter is
    /// assigned before (A0054). `None` after reporting why it isn't one.
    fn collecting(
        &mut self,
        stmt: &ast::TransactionStmt,
        collecting: &ast::Collecting,
        errors: &mut Vec<Diagnostic>,
    ) -> Option<hir::Target> {
        let written = collecting.variable()?;
        let (target, _) = crate::assign::target_path(&written)?;
        let span = written.span();
        let errors_type = hir::Type::Relation(hir::transaction_collection(
            self.around.namespaces().library().error_type(),
        ));
        if let crate::assign::TargetName::Plain(name) = &target {
            let name = name.ident()?;
            let name = name.text();
            let known = self.locals.find(name).is_some() || self.around.has_value(name, span);
            if !known {
                errors.push(errors::undeclared_collecting(
                    name,
                    span,
                    stmt,
                    self.around.namespaces(),
                ));
                return None;
            }
        }
        let assignee = crate::assign::assignee(&target, &mut self.names(), errors)?;
        if assignee.ty != errors_type {
            let name = written.syntax().text().to_string();
            errors.push(errors::collecting_mismatch(&name, span, &assignee.ty));
            return None;
        }
        let around = &*self.around;
        if let Some(read) = self
            .unassigned
            .of(&assignee.target, |name| around.is_out(name))
        {
            self.unassigned.reads.push((read, span));
            self.flush(errors);
        }
        Some(assignee.target)
    }

    /// The declaration `decl` of a local variable: its initial value's
    /// assignment, of none if it has none, for a statement to give it one
    /// before anything reads it. Its local variable is declared from then on.
    fn declaration(
        &mut self,
        decl: &VarDecl,
        errors: &mut Vec<Diagnostic>,
    ) -> Option<hir::Statement> {
        let types = self
            .around
            .type_parameters()
            .map(<[hir::TypeParameter]>::to_vec)
            .unwrap_or_default();
        let checked = locals::assignable(decl, &types, &mut self.names(), errors);
        let (local, value) = checked.unzip();
        let id = self.locals.declare(decl, local)?;
        let Some(value) = value? else {
            self.unassigned.locals.insert(id, decl.clone());
            return Some(hir::Statement::Declare(id));
        };
        Some(hir::Statement::Assign(vec![hir::Assignment {
            target: hir::Target::Local(id),
            value,
        }]))
    }

    /// Whether `call`, of `load_csv`, whose signature is `signature`, with
    /// the arguments `args`, can load its array: whether it can read its
    /// tuples' attributes from fields (see [`csv_readable`]), and
    /// `columns` gives each its own column. If not, it is reported.
    fn csv_loadable(
        &mut self,
        call: &hir::Call,
        signature: &Signature,
        args: &ast::Arguments,
        errors: &mut Vec<Diagnostic>,
    ) -> bool {
        // The types the call gives the tuples, `T`, and the columns, `C`.
        let given = |name: &str| {
            let index = signature
                .types
                .iter()
                .position(|t| t.name.as_deref() == Some(name))
                .expect("`load_csv` has its type parameters");
            &call.types[index]
        };
        let (tuples, given_columns) = (given("T"), given("C"));
        let written = |name: &str| {
            args.named()
                .find(|a| a.is_named(name))
                .and_then(|a| a.value())
        };
        let (Some(rows), Some(columns)) = (written("rows"), written("columns")) else {
            return false;
        };
        // Each value of a type of the program's own is read in the
        // representation whose components the columns name.
        if let hir::Type::Tuple(heading) = tuples
            && !columns_named(heading, &columns, errors)
        {
            return false;
        }
        let read = hir::read_as(tuples, given_columns);
        if !csv_readable(tuples, &read, &rows, errors) {
            return false;
        }
        let positions = positions(&read);
        // Checked again, against the type it must have, which reports how
        // it differs.
        if *given_columns != positions {
            let expected = Expected {
                ty: positions,
                origin: Origin::Columns(rows),
            };
            if Checker::new(&mut self.names())
                .expr(&columns, Some(&expected), errors)
                .is_none()
            {
                return false;
            }
        }
        distinct_columns(&call.args["columns"], &columns, errors)
    }

    /// `call name{arguments};`, of an update operator. One in a `for` loop
    /// is checked once every body is, for what it assigns.
    fn call(
        &mut self,
        stmt: &ast::CallStmt,
        errors: &mut Vec<Diagnostic>,
    ) -> Option<hir::Statement> {
        let update = self
            .update
            .as_ref()
            .expect("only an update operator's body has calls");
        let (cx, within) = (update.cx, update.within);
        let ident = stmt.callee()?.ident()?;
        let name = ident.text();
        let span = trendy_syntax::span(ident.text_range());
        let qualifier = stmt.qualifier();
        let called = cx
            .namespaces
            .written_name(&ident, qualifier.as_ref(), errors)?;
        let args = stmt.arguments();
        let names = args
            .as_ref()
            .map(|args| argument_names(Written::Arguments(args)))
            .unwrap_or_default();
        let signature = match cx.operators.chosen(called, &names) {
            Some(Chosen::One(signature)) => signature,
            Some(Chosen::Tied(tied)) => {
                // Without arguments, a syntax error, which is reported.
                if let Some(args) = &args {
                    errors.push(errors::unchosen_operator(
                        name,
                        &tied,
                        Written::Arguments(args),
                        cx.namespaces.units(),
                    ));
                }
                return None;
            }
            None => {
                if let (Some(unit), Some(qualifier)) = (called.unit, &qualifier) {
                    errors.push(errors::unknown_qualified_operator(
                        &ident,
                        qualifier,
                        unit,
                        cx.namespaces,
                        cx.operators,
                    ));
                    return None;
                }
                let what = if cx.operators.get(within).is_parameter(name) {
                    Some("a parameter")
                } else if self.locals.find(name).is_some() {
                    Some("a local variable")
                } else {
                    cx.namespaces.variable(name, span).map(|_| "a variable")
                };
                if let Some(what) = what {
                    errors.push(errors::not_an_operator(name, span, what));
                } else if let Some(error) =
                    errors::unused_unit(&ident, crate::Kind::Operator, cx.namespaces)
                {
                    errors.push(error);
                } else {
                    errors.push(errors::unknown_operator(
                        &ident,
                        cx.operators,
                        args.as_ref().map(Written::Arguments),
                        false,
                        cx.namespaces,
                    ));
                }
                return None;
            }
        };
        if signature.read_only {
            errors.push(errors::call_of_read_only(name, span, signature.defined()));
            return None;
        }
        let args = args?;
        if !signature.valid {
            return None;
        }
        let call =
            Checker::new(&mut self.names()).arguments(Written::Arguments(&args), signature, errors);
        self.outs_passed(&args, signature);
        let call = call?;
        if matches!(signature.builtin, Some(hir::Builtin::Update(b)) if b.reads_csv())
            && !self.csv_loadable(&call, signature, &args, errors)
        {
            return None;
        }
        if !self.updated(&call, &args, signature.id, errors) {
            return None;
        }
        // What it checks of the arguments known now.
        if let Err(broken) = ops::constraints_hold(&call, cx.bindings()) {
            errors.push(errors::broken_call_constraint(&broken, stmt.span()));
            return None;
        }
        self.defer(&call, signature, span, stmt.span());
        Some(hir::Statement::Call(call))
    }

    /// Record what can only be checked once every body is of `call`, of
    /// `signature`, its name written at `span`, the statement at
    /// `statement`: that it is a call, and in which transaction, if any;
    /// each type parameter it gives a type built from its caller's own;
    /// and the variables of the program the `for` loops it is in keep from
    /// changing.
    fn defer(&mut self, call: &hir::Call, signature: &Signature, span: Span, statement: Span) {
        let update = self
            .update
            .as_mut()
            .expect("only an update operator's body has calls");
        let (cx, within) = (update.cx, update.within);
        update.deferred.calls.push((within, signature.id, span));
        if let Some(&transaction) = self.transactions.last() {
            update.deferred.in_transactions.push(TransactionCall {
                callee: signature.id,
                span,
                transaction,
            });
        } else {
            update
                .deferred
                .outside_transactions
                .push((within, signature.id, span));
        }
        update
            .deferred
            .growing
            .extend(Growing::of(within, signature, &call.types, span));
        let looped: Vec<(hir::VariableId, String, Span)> = self
            .frozen
            .iter()
            .flat_map(|frozen| {
                frozen.targets.iter().filter_map(|target| match target {
                    hir::Target::Variable(id) => {
                        let name = cx.variables[id.0]
                            .as_ref()
                            .map_or_else(String::new, |v| v.name.clone());
                        Some((*id, name, frozen.array))
                    }
                    hir::Target::Local(_) | hir::Target::Parameter(_) => None,
                })
            })
            .collect();
        if !looped.is_empty() {
            update.deferred.in_loops.push(LoopCall {
                callee: signature.id,
                span: statement,
                looped,
            });
        }
    }

    /// Count each argument in `args` for an `out` parameter of `callee`
    /// that is one of this operator's `out` parameters as assigned by the
    /// call, not read.
    fn outs_passed(&mut self, args: &ast::Arguments, callee: &Signature) {
        for arg in args.named() {
            let (Some(param), Some(value)) = (arg.name().and_then(|n| n.ident()), arg.value())
            else {
                continue;
            };
            if callee.mode(param.text()) != Some(hir::Mode::Out) {
                continue;
            }
            let span = value.span();
            let within = |s: &Span| span.start <= s.start && s.end <= span.end;
            let passed: Vec<(NoValueYet, Span)> = self
                .unassigned
                .reads
                .iter()
                .filter(|(_, s)| within(s))
                .cloned()
                .collect();
            self.unassigned.reads.retain(|(_, s)| !within(s));
            self.unassigned.targets.extend(passed);
        }
    }

    /// Whether the variables `call`, of the operator `callee`, with the
    /// arguments `args`, updates, its `var` and `out` parameters'
    /// arguments, can be: each one the body can assign, and a different
    /// one, after reporting why not. Whether the operator called reads or
    /// assigns one that is the program's itself is checked once every body
    /// is.
    fn updated(
        &mut self,
        call: &hir::Call,
        args: &ast::Arguments,
        callee: hir::OperatorId,
        errors: &mut Vec<Diagnostic>,
    ) -> bool {
        let mut ok = true;
        // Each variable passed so far, with its parameter and where.
        let mut passed: Vec<(&hir::Target, String, Span)> = Vec::new();
        for arg in args.named() {
            let (Some(ident), Some(value)) = (arg.name().and_then(|n| n.ident()), arg.value())
            else {
                continue;
            };
            let Some(target) = call.updated.get(ident.text()) else {
                continue;
            };
            let update = self
                .update
                .as_ref()
                .expect("only an update operator's body has calls");
            let cx = update.cx;
            let mode = cx.operators.get(callee).mode(ident.text());
            let param = format!("{} {}", mode.map_or("var", hir::Mode::word), ident.text());
            let span = value.span();
            // A variable of the program's as the call writes it, which a name
            // of another unit's can qualify, and which only a loop can keep
            // from being assigned; a local variable or a parameter by its
            // name, which is never qualified.
            let (name, assignable) = match target {
                hir::Target::Variable(_) => {
                    let written = value.syntax().text().to_string();
                    let unfrozen = self.names().unfrozen(target, &written, span, errors);
                    (written, unfrozen)
                }
                hir::Target::Local(id) => {
                    let name = self.locals.get(*id).name.clone();
                    let assigned = self.names().assigned(&name, span, errors).is_some();
                    (name, assigned)
                }
                hir::Target::Parameter(name) => {
                    let assigned = self.names().assigned(name, span, errors).is_some();
                    (name.clone(), assigned)
                }
            };
            if !assignable {
                ok = false;
                continue;
            }
            if let Some((_, first, earlier)) = passed.iter().find(|(t, ..)| *t == target) {
                errors.push(errors::passed_twice(&name, first, &param, span, *earlier));
                ok = false;
                continue;
            }
            passed.push((target, param.clone(), span));
            if let hir::Target::Variable(variable) = target
                && let Some(update) = &mut self.update
            {
                update.deferred.passed.push(Passed {
                    callee,
                    variable: *variable,
                    name,
                    param,
                    span,
                });
            }
        }
        ok
    }

    /// A loop, written at `span`, whose condition, after `keyword`, is
    /// checked as `check` says: its block run while the condition is
    /// true. One that does nothing, or never ends, as one whose condition
    /// is known to be true when compiling, is reported.
    fn loop_stmt(
        &mut self,
        keyword: Option<SyntaxToken>,
        condition: Option<ast::Expr>,
        body: Option<ast::Body>,
        check: hir::Check,
        span: Span,
        errors: &mut Vec<Diagnostic>,
    ) -> Option<hir::Statement> {
        let (keyword, written, body) = (keyword?, condition?, body?);
        let boolean = Expected {
            ty: hir::Type::Scalar(hir::Scalar::Boolean),
            origin: Origin::Operator(keyword),
        };
        let condition = Checker::new(&mut self.names()).expr(&written, Some(&boolean), errors);
        let statements: Vec<ast::Statement> = body.statements().collect();
        self.loops += 1;
        let block = match check {
            // The block may not run, so what it assigns isn't assigned
            // after; the condition is computed before it, so what it shows
            // if true is so where the block starts.
            hir::Check::Before => {
                self.flush(errors);
                let before = self.unassigned.assigned.clone();
                let (when_true, _) = self.facts(&written);
                let (narrowed, ended) = (self.narrowed.clone(), self.ended.clone());
                self.narrowed = narrowed.and(&when_true);
                let block = self.block(&statements, errors);
                (self.narrowed, self.ended) = (narrowed, ended);
                self.unassigned.assigned = before;
                block
            }
            // The block runs at least once, before the condition, unless it
            // leaves before its end, or goes on to its next run, which can be
            // none.
            hir::Check::After => {
                let condition_reads = std::mem::take(&mut self.unassigned.reads);
                let before = self.unassigned.assigned.clone();
                let block = self.block(&statements, errors);
                if block.as_ref().is_none_or(|block| {
                    let exits = block.loop_exits();
                    exits.leaves || exits.iterates
                }) {
                    self.unassigned.assigned = before;
                }
                self.unassigned.reads = condition_reads;
                self.flush(errors);
                block
            }
        };
        self.loops -= 1;
        let exits = block.as_ref().map(hir::Block::loop_exits);
        let pointless = errors::pointless_loop(span, &written, &body, check, exits);
        let reported = pointless.is_some();
        errors.extend(pointless);
        // A condition always true never ends the loop, which its block must.
        let leaves = exits.is_none_or(|exits| exits.leaves || exits.returns);
        if !reported && !leaves && condition == Some(hir::Expr::Constant(hir::Value::Boolean(true)))
        {
            errors.push(errors::endless_loop(&written));
        }
        errors.extend(pointless_iterate(&statements));
        Some(hir::Statement::Loop(condition?, block?, check))
    }

    /// `if c then [ … ] else [ … ];`: each block checked, but a condition
    /// known when compiling runs only one, so the other's failures aren't
    /// reported.
    fn if_stmt(
        &mut self,
        stmt: &ast::IfStmt,
        errors: &mut Vec<Diagnostic>,
    ) -> Option<hir::Statement> {
        let condition_expr = stmt.condition()?;
        let then_block = stmt.then_block()?;
        if let Some(error) = errors::pointless_if_statement(stmt) {
            errors.push(error);
        }
        let boolean = Expected {
            ty: hir::Type::Scalar(hir::Scalar::Boolean),
            origin: Origin::Operator(stmt.keyword()?),
        };
        let condition =
            Checker::new(&mut self.names()).expr(&condition_expr, Some(&boolean), errors);
        self.flush(errors);
        let taken = match &condition {
            Some(hir::Expr::Constant(hir::Value::Boolean(taken))) => Some(*taken),
            _ => None,
        };
        // What the branch taken assigns is assigned after: what both do,
        // unless the condition says which. Each branch knows what the
        // condition shows of the names' values, as it is true or false.
        let before = self.unassigned.assigned.clone();
        let (when_true, when_false) = self.facts(&condition_expr);
        let (narrowed, ended) = (self.narrowed.clone(), self.ended.clone());
        self.narrowed = narrowed.and(&when_true);
        let then = self.branch(
            Some(ast::ElseBranch::Block(then_block)),
            taken == Some(false),
            errors,
        );
        let after_then = std::mem::replace(&mut self.unassigned.assigned, before);
        (self.narrowed, self.ended) = (narrowed.and(&when_false), ended.clone());
        let otherwise = self.branch(stmt.else_branch(), taken == Some(true), errors);
        (self.narrowed, self.ended) = (narrowed, ended);
        let after_otherwise = std::mem::take(&mut self.unassigned.assigned);
        // A branch that always leaves comes to nothing after the `if`.
        let leaves =
            |block: &Option<hir::Block>| block.as_ref().is_some_and(hir::Block::always_exits);
        self.unassigned.assigned = match (taken, leaves(&then), leaves(&otherwise)) {
            (Some(true), ..) | (None, false, true) => after_then,
            (Some(false), ..) | (None, true, false) => after_otherwise,
            (None, ..) => after_then.intersection(&after_otherwise).cloned().collect(),
        };
        Some(hir::Statement::If(condition?, then?, otherwise?))
    }

    /// An `if` statement's branch, `branch`, as a block, empty if there is
    /// none. One that is `skipped` never runs, so its failures aren't
    /// reported, and it is empty if one would make it fail.
    fn branch(
        &mut self,
        branch: Option<ast::ElseBranch>,
        skipped: bool,
        errors: &mut Vec<Diagnostic>,
    ) -> Option<hir::Block> {
        let mut own = Vec::new();
        let reported_to = if skipped { &mut own } else { &mut *errors };
        let block = match branch {
            None => Some(hir::Block::default()),
            Some(ast::ElseBranch::Block(body)) => {
                let statements: Vec<ast::Statement> = body.statements().collect();
                self.block(&statements, reported_to)
            }
            Some(ast::ElseBranch::If(stmt)) => {
                let site = self
                    .around
                    .site(trendy_syntax::span(stmt.syntax().text_range()));
                self.if_stmt(&stmt, reported_to)
                    .map(|statement| hir::Block {
                        statements: vec![(site, statement)],
                        locals: Vec::new(),
                    })
            }
        };
        if !skipped {
            return block;
        }
        let before = errors.len();
        errors.extend(own.into_iter().filter(|d| d.code.kind() != CodeKind::Run));
        (errors.len() == before).then(|| block.unwrap_or_default())
    }
}

/// Report what can only be checked once every update operator's body is,
/// in `bodies`: each call that leads back to its caller with ever larger
/// types (see [`growing_recursion`]), each call in a `for` loop that
/// assigns a variable of the program's that the loop's array is computed
/// from, in the operator called or in those it calls, and each that passes
/// one to a `var` parameter of an operator that reads or assigns it
/// itself, there or in the update operators it calls: the read-only ones
/// read no variable of the program's. Each call in a transaction that reads
/// input or writes output, and each of `main`'s that reads when a transaction
/// began, with none running.
pub(crate) fn deferred_checks(
    deferred: &Deferred,
    bodies: &HashMap<hir::OperatorId, (Vec<hir::Local>, hir::Block)>,
    evaluated: &crate::globals::Evaluated,
    operators: &Operators,
    declared: &scope::Variables,
    main: Option<hir::OperatorId>,
    errors: &mut Vec<Diagnostic>,
) {
    for passed in &deferred.passed {
        let assigns = assigned_variables(passed.callee, bodies).contains(&passed.variable);
        if assigns || read_variables(passed.callee, bodies).contains(&passed.variable) {
            errors.push(errors::passed_and_used(
                &passed.name,
                &passed.param,
                &operators.get(passed.callee).name,
                assigns,
                passed.span,
            ));
        }
    }
    for (id, at) in &evaluated.read_variables {
        withdraw_constant(*id, *at, declared, bodies, operators, errors);
    }
    let growing = deferred.growing.iter().chain(&evaluated.growing);
    growing_recursion(growing, bodies, &evaluated.values, operators, errors);
    for call in &deferred.in_loops {
        let assigned = assigned_variables(call.callee, bodies);
        if let Some((_, name, array)) = call.looped.iter().find(|(id, ..)| assigned.contains(id)) {
            errors.push(errors::call_assigns_looped(
                &operators.get(call.callee).name,
                name,
                call.span,
                *array,
            ));
        }
    }
    for call in &deferred.in_transactions {
        let does_io = |builtin: hir::UpdateBuiltin| builtin.io().is_some();
        if let Some((steps, builtin)) = steps_to(call.callee, &deferred.calls, operators, &does_io)
            && let Some(io) = builtin.io()
        {
            errors.push(errors::io_in_transaction(
                &operators.get(call.callee).name,
                call.span,
                call.transaction,
                &steps,
                io,
            ));
        }
    }
    // What runs in a transaction can read when it began. What `main` calls in none
    // has none running, however far down it reads it.
    let outside = &deferred.outside_transactions;
    let reads_start =
        |builtin: hir::UpdateBuiltin| builtin == hir::UpdateBuiltin::ReadTransactionStart;
    for &(_, callee, span) in outside.iter().filter(|(caller, ..)| Some(*caller) == main) {
        if let Some((steps, _)) = steps_to(callee, outside, operators, &reads_start) {
            errors.push(errors::outside_any_transaction(
                &operators.get(callee).name,
                span,
                &steps,
            ));
        }
    }
}

/// The calls that lead from the update operator `callee` to a built-in that
/// `wanted` says is wanted, through `calls`, call statements, by the operator
/// each is in, and the versions of a specification called: each step's caller's
/// name, callee's, and where the call is, if it is one, the built-in's last, but
/// those that come with the compiler's own, which are nothing a program can
/// change; none if `callee` is one itself; and the built-in. `None` if it leads
/// to none.
fn steps_to(
    callee: hir::OperatorId,
    calls: &[(hir::OperatorId, hir::OperatorId, Span)],
    operators: &Operators,
    wanted: &dyn Fn(hir::UpdateBuiltin) -> bool,
) -> Option<(Vec<IoStep>, hir::UpdateBuiltin)> {
    // How each operator reached was first reached: from which, and where.
    let mut reached: HashMap<hir::OperatorId, Option<(hir::OperatorId, Option<Span>)>> =
        HashMap::from([(callee, None)]);
    let mut next = std::collections::VecDeque::from([callee]);
    while let Some(id) = next.pop_front() {
        if let Some(hir::Builtin::Update(builtin)) = operators.get(id).builtin
            && wanted(builtin)
        {
            let mut steps = Vec::new();
            let mut at = id;
            while let Some(&Some((from, span))) = reached.get(&at) {
                if operators.get(from).source == crate::Source::Program {
                    steps.push(IoStep {
                        caller: operators.get(from).name.clone(),
                        callee: operators.get(at).name.clone(),
                        span,
                    });
                }
                at = from;
            }
            steps.reverse();
            return Some((steps, builtin));
        }
        let onward = calls
            .iter()
            .filter(|(caller, ..)| *caller == id)
            .map(|&(_, callee, span)| (callee, Some(span)))
            .chain(
                operators
                    .get(id)
                    .versions
                    .iter()
                    .map(|version| (version.operator, None)),
            );
        for (to, span) in onward {
            if let std::collections::hash_map::Entry::Vacant(entry) = reached.entry(to) {
                entry.insert(Some((id, span)));
                next.push_back(to);
            }
        }
    }
    None
}

/// A step from an operator to one it calls, or a version of it it runs,
/// on the way to what a built-in does: their names, and where the call is,
/// if it is one.
pub(crate) struct IoStep {
    pub(crate) caller: String,
    pub(crate) callee: String,
    pub(crate) span: Option<Span>,
}

/// The variables of the program that the update operator `id` assigns, in
/// its body or in those of the operators it calls, in `bodies`.
fn assigned_variables(
    id: hir::OperatorId,
    bodies: &HashMap<hir::OperatorId, (Vec<hir::Local>, hir::Block)>,
) -> Vec<hir::VariableId> {
    let mut assigned = Vec::new();
    let mut seen = vec![id];
    let mut next = vec![id];
    while let Some(id) = next.pop() {
        let Some((_, body)) = bodies.get(&id) else {
            continue;
        };
        assigned_in(body, &mut assigned);
        let mut called = Vec::new();
        called.extend(body.called());
        for callee in called {
            if !seen.contains(&callee) {
                seen.push(callee);
                next.push(callee);
            }
        }
    }
    assigned
}

/// Add each variable of the program that `block`'s statements assign, or
/// pass to a `var` parameter, in blocks within it too, to `assigned`.
fn assigned_in(block: &hir::Block, assigned: &mut Vec<hir::VariableId>) {
    for target in block.assigned() {
        if let hir::Target::Variable(id) = target
            && !assigned.contains(&id)
        {
            assigned.push(id);
        }
    }
}

/// Add each variable of the program that `block`'s calls pass to a `var`
/// parameter, in blocks within it too, to `passed`.
fn passed_in(block: &hir::Block, passed: &mut Vec<hir::VariableId>) {
    block.each_statement(&mut |statement| {
        let hir::Statement::Call(call) = statement else {
            return;
        };
        for target in call.updated.values() {
            if let hir::Target::Variable(id) = target
                && !passed.contains(id)
            {
                passed.push(*id);
            }
        }
    });
}

/// Withdraw from the error for a read-only operator reading the variable `id`
/// at `at`, if it was kept, the fix of declaring it `const`, if something may
/// assign it.
fn withdraw_constant(
    id: hir::VariableId,
    at: Span,
    declared: &scope::Variables,
    bodies: &HashMap<hir::OperatorId, (Vec<hir::Local>, hir::Block)>,
    operators: &Operators,
    errors: &mut [Diagnostic],
) {
    let decl = declared.decl(id);
    let (Some(keyword), Some(name)) = (decl.keyword(), decl.name()) else {
        return;
    };
    if !may_be_assigned(
        id,
        name.syntax().text().to_string().as_str(),
        bodies,
        operators,
    ) {
        return;
    }
    let keyword = trendy_syntax::span(keyword.text_range());
    let reported = errors
        .iter_mut()
        .find(|e| e.code == trendy_diag::Code::VariableInReadOnly && e.primary.span == at);
    if let Some(error) = reported {
        error
            .suggestions
            .retain(|fix| fix.edits.iter().all(|edit| edit.span != keyword));
    }
}

/// Whether something may assign the variable `id`, named `name`: a body that
/// does, directly or by passing it to a `var` or `out` parameter, or a body with
/// errors, whose statements nothing says, in which a word of that name is.
fn may_be_assigned(
    id: hir::VariableId,
    name: &str,
    bodies: &HashMap<hir::OperatorId, (Vec<hir::Local>, hir::Block)>,
    operators: &Operators,
) -> bool {
    let assigned = bodies.values().any(|(_, body)| {
        body.assigned()
            .iter()
            .any(|target| matches!(target, hir::Target::Variable(v) if *v == id))
    });
    assigned
        || operators
            .iter()
            .filter(|signature| !signature.read_only && !bodies.contains_key(&signature.id))
            .filter_map(|signature| signature.decl.as_ref()?.body())
            .any(|body| ast::mentions(body.syntax(), name))
}

/// The variables of the program that the update operator `id` reads, in
/// its body or in those of the update operators it calls, in `bodies`:
/// each a variable's value, or one it passes to a `var` parameter. The
/// read-only operators it calls read none.
fn read_variables(
    id: hir::OperatorId,
    bodies: &HashMap<hir::OperatorId, (Vec<hir::Local>, hir::Block)>,
) -> Vec<hir::VariableId> {
    fn walk(expr: &hir::Expr, read: &mut Vec<hir::VariableId>) {
        if let hir::Expr::Variable(id) = expr
            && !read.contains(id)
        {
            read.push(*id);
        }
        expr.for_each_child(|child, _| walk(child, read));
    }
    let mut read = Vec::new();
    let mut next = vec![id];
    let mut called = vec![id];
    while let Some(id) = next.pop() {
        let Some((_, body)) = bodies.get(&id) else {
            continue;
        };
        exprs_in(body, &mut |expr| walk(expr, &mut read));
        let mut callees = Vec::new();
        callees.extend(body.called());
        for callee in callees {
            if !called.contains(&callee) {
                called.push(callee);
                next.push(callee);
            }
        }
    }
    // A `var` parameter's argument is read too.
    for id in called {
        if let Some((_, body)) = bodies.get(&id) {
            passed_in(body, &mut read);
        }
    }
    read
}

/// Call `f` with each expression `block`'s statements compute, in blocks
/// within it too.
fn exprs_in(block: &hir::Block, f: &mut dyn FnMut(&hir::Expr)) {
    block.each_statement(&mut |statement| match statement {
        hir::Statement::Call(call) => call.args.values().for_each(&mut *f),
        hir::Statement::Assign(assignments) => {
            for assignment in assignments {
                f(&assignment.value);
            }
        }
        hir::Statement::If(condition, ..) | hir::Statement::Loop(condition, ..) => f(condition),
        hir::Statement::For(_, array, _) => f(array),
        hir::Statement::Fail(error)
        | hir::Statement::Warn(error)
        | hir::Statement::Return(Some(error)) => f(error),
        hir::Statement::Declare(_)
        | hir::Statement::Transaction(..)
        | hir::Statement::Return(None)
        | hir::Statement::Leave
        | hir::Statement::Iterate => {}
    });
}

/// Report each call in `growing` that leads back to its caller through
/// the calls in the update operators' `bodies` and the read-only operators'
/// `values`, and from a specification to its versions: the operator would need
/// a function for each of the ever larger types the type parameter is given
/// each time around (see [`hir::Operator`]).
fn growing_recursion<'g>(
    growing: impl Iterator<Item = &'g Growing>,
    bodies: &HashMap<hir::OperatorId, (Vec<hir::Local>, hir::Block)>,
    values: &HashMap<hir::OperatorId, hir::Expr>,
    operators: &Operators,
    errors: &mut Vec<Diagnostic>,
) {
    let calls = |id: &hir::OperatorId| {
        let mut called: Vec<hir::OperatorId> = operators
            .get(*id)
            .versions
            .iter()
            .map(|version| version.operator)
            .collect();
        if let Some((_, body)) = bodies.get(id) {
            called.extend(body.calling());
        }
        if let Some(value) = values.get(id) {
            called.extend(value.calling());
        }
        called
    };
    for call in growing {
        // Whether the callee reaches the caller.
        let mut seen = vec![call.callee];
        let mut next = vec![call.callee];
        while let Some(id) = next.pop() {
            for callee in calls(&id) {
                if !seen.contains(&callee) {
                    seen.push(callee);
                    next.push(callee);
                }
            }
        }
        if seen.contains(&call.caller) {
            errors.push(errors::growing_recursion(
                &operators.get(call.caller).name,
                &operators.get(call.callee).name,
                call.span,
                &call.parameter,
                &call.ty,
            ));
        }
    }
}

/// Names in a block of statements: its local variables, which hide variables
/// of the same name, then those around it.
struct BodyNames<'a> {
    around: &'a mut (dyn Around + 'a),
    locals: &'a mut Locals,
    /// What the `for` loops the statement being checked is in keep from
    /// changing.
    frozen: &'a [Frozen],
    /// Where the statement being checked reads and assigns what has no value
    /// yet.
    unassigned: &'a mut Unassigned,
    /// What tests show of the names' values where the statement is.
    narrowed: &'a Facts,
    /// Where a statement before it may assign each name it ended what
    /// tests showed of.
    ended: &'a BTreeMap<String, Span>,
}

impl BodyNames<'_> {
    /// What the local variable `name`, written at `span`, refers to, if it
    /// is one, before what tests show of its value.
    fn local(&self, name: &str, span: Span, errors: &mut Vec<Diagnostic>) -> Option<Lookup> {
        Some(match self.locals.find(name)? {
            Local::Declared(id, ..) => Lookup::Found(hir::Expr::Local(id)),
            Local::Later(decl) => {
                errors.push(errors::local_before_declaration(
                    name,
                    span,
                    decl,
                    self.around.is_variable(name, span),
                ));
                Lookup::Reported
            }
            Local::Ended(decl) => {
                errors.push(errors::local_out_of_scope(name, span, decl));
                Lookup::Reported
            }
            Local::Invalid => Lookup::Reported,
        })
    }

    /// Note a read at `span` of what `lookup` found, if it has no value until
    /// it is assigned, for [`Statements::settle`] to report.
    fn read(&mut self, lookup: &Lookup, span: Span) {
        let read = match lookup {
            Lookup::Found(hir::Expr::Parameter(parameter)) if self.around.is_out(parameter) => {
                NoValueYet::Out(parameter.clone())
            }
            Lookup::Found(hir::Expr::Local(id)) if self.unassigned.locals.contains_key(id) => {
                NoValueYet::Local(*id)
            }
            _ => return,
        };
        self.unassigned.reads.push((read, span));
    }
}

impl Names for BodyNames<'_> {
    fn namespaces(&self) -> &Namespaces {
        self.around.namespaces()
    }

    fn site(&self, at: Span) -> hir::Site {
        self.around.site(at)
    }

    fn operators(&self) -> &Operators {
        self.around.operators()
    }

    fn resolve(&mut self, name: &str, span: Span, errors: &mut Vec<Diagnostic>) -> Lookup {
        let lookup = self
            .local(name, span, errors)
            .unwrap_or_else(|| self.around.resolve(name, span, errors));
        self.read(&lookup, span);
        let facts = self.narrowed;
        narrowed::narrow(facts, name, lookup, self)
    }

    fn resolve_variable(&mut self, name: &str, span: Span, errors: &mut Vec<Diagnostic>) -> Lookup {
        let lookup = self
            .local(name, span, errors)
            .unwrap_or_else(|| self.around.resolve_variable(name, span, errors));
        self.read(&lookup, span);
        lookup
    }

    fn narrowing_ended(&self, name: &str) -> Option<Span> {
        self.ended
            .get(name)
            .copied()
            .or_else(|| self.around.narrowing_ended(name))
    }

    fn value_in(
        &mut self,
        name: &str,
        unit: hir::FileId,
        span: Span,
        errors: &mut Vec<Diagnostic>,
    ) -> Lookup {
        self.around.value_in(name, unit, span, errors)
    }

    fn candidates(&self, at: Span) -> Vec<String> {
        self.locals
            .names()
            .map(str::to_string)
            .chain(self.around.candidates(at))
            .collect()
    }

    fn leaf_type(&self, leaf: &hir::Expr) -> hir::Type {
        match leaf {
            hir::Expr::Local(id) if self.locals.has(*id) => self.locals.get(*id).ty.clone(),
            _ => self.around.leaf_type(leaf),
        }
    }

    fn fresh_local(&mut self, local: hir::Local) -> hir::LocalId {
        self.locals.fresh(local)
    }

    fn next_local(&self) -> usize {
        self.locals.next()
    }

    fn around(&self, name: &str) -> Option<Meaning> {
        if matches!(
            self.locals.find(name),
            Some(Local::Declared(..) | Local::Later(_) | Local::Invalid)
        ) {
            Some(Meaning::LocalVariable)
        } else {
            self.around.around(name)
        }
    }

    fn tuple_at_hand(&self) -> Option<&hir::Heading> {
        self.around.tuple_at_hand()
    }

    fn unnumbered(&self) -> bool {
        self.around.unnumbered()
    }

    fn type_parameters(&self) -> Option<&[hir::TypeParameter]> {
        self.around.type_parameters()
    }

    fn checked(&mut self, id: hir::OperatorId, at: Span, errors: &mut Vec<Diagnostic>) -> bool {
        self.around.checked(id, at, errors)
    }

    fn calling(&mut self, callee: &Signature, types: &[hir::Type], span: Span) {
        self.around.calling(callee, types, span);
    }

    fn bindings(&self) -> ops::Bindings<'_> {
        self.around.bindings()
    }
}

impl Assigning for BodyNames<'_> {
    /// The variable `name`, unless a `for` loop that the assignment is in
    /// goes through an array computed from it.
    fn assigned(
        &mut self,
        name: &str,
        span: Span,
        errors: &mut Vec<Diagnostic>,
    ) -> Option<Assignee> {
        let assignee = self.variable(name, span, errors)?;
        if !self.unfrozen(&assignee.target, name, span, errors) {
            return None;
        }
        let narrowed = self.narrowed.narrowed(name, &assignee.ty);
        Some(Assignee {
            narrowed,
            ..assignee
        })
    }

    /// The variable `written`, unless a `for` loop that the assignment is in
    /// goes through an array computed from it. A qualified name is never
    /// narrowed.
    fn assigned_in(
        &mut self,
        written: &ast::QualifiedName,
        unit: hir::FileId,
        errors: &mut Vec<Diagnostic>,
    ) -> Option<Assignee> {
        let assignee = self.around.assigned_in_around(written, unit, errors)?;
        let whole = written.syntax().text().to_string();
        self.unfrozen(&assignee.target, &whole, written.span(), errors)
            .then_some(assignee)
    }
}

impl BodyNames<'_> {
    /// Whether `target`, written `name` at `span`, can be assigned where a
    /// `for` loop goes through an array computed from it, which it can't,
    /// after reporting that.
    fn unfrozen(
        &self,
        target: &hir::Target,
        name: &str,
        span: Span,
        errors: &mut Vec<Diagnostic>,
    ) -> bool {
        let frozen = self
            .frozen
            .iter()
            .find(|frozen| frozen.targets.contains(target));
        if let Some(frozen) = frozen {
            errors.push(errors::assigned_in_loop(name, span, frozen.array));
            return false;
        }
        true
    }

    /// The variable `name`, a local variable or else what's around them,
    /// that an assignment at `span` assigns, or `None` after reporting why
    /// it can't be.
    fn variable(
        &mut self,
        name: &str,
        span: Span,
        errors: &mut Vec<Diagnostic>,
    ) -> Option<Assignee> {
        match self.locals.find(name) {
            Some(Local::Declared(_, _, Declaration::Var(declaration)))
                if declaration.is_const() =>
            {
                errors.push(errors::assigned_constant(name, span, declaration));
                return None;
            }
            Some(Local::Declared(id, local, Declaration::Var(declaration))) => {
                if self.unassigned.locals.contains_key(&id) {
                    self.unassigned.targets.push((NoValueYet::Local(id), span));
                }
                return Some(Assignee::local(id, local, declaration));
            }
            Some(Local::Declared(_, _, declaration @ Declaration::Loop(_))) => {
                errors.push(errors::assigned_loop_variable(
                    name,
                    span,
                    declaration.name_span(),
                ));
                return None;
            }
            Some(Local::Later(decl)) => {
                errors.push(errors::local_before_declaration(
                    name,
                    span,
                    decl,
                    self.around.is_variable(name, span),
                ));
                return None;
            }
            Some(Local::Ended(decl)) => {
                errors.push(errors::local_out_of_scope(name, span, decl));
                return None;
            }
            // Its declaration's errors are already reported.
            Some(Local::Invalid) => return None,
            None => {}
        }
        let locals: Vec<&str> = self.locals.names().collect();
        let assignee = self.around.assigned_around(name, span, &locals, errors)?;
        if let hir::Target::Parameter(parameter) = &assignee.target
            && self.around.is_out(parameter)
        {
            self.unassigned
                .targets
                .push((NoValueYet::Out(parameter.clone()), span));
        }
        Some(assignee)
    }
}

/// What is around an update operator's body: its parameters, and the
/// program's variables, which it reads and assigns.
struct UpdateNames<'a, 'b> {
    cx: &'a Context<'b>,
    within: hir::OperatorId,
}

impl UpdateNames<'_, '_> {
    fn signature(&self) -> &Signature {
        self.cx.operators.get(self.within)
    }

    /// Reading the variable of the program's `id`.
    fn variable_read(&self, id: hir::VariableId) -> Lookup {
        // A variable with errors in its declaration is already reported.
        match self.cx.variables[id.0] {
            Some(_) => Lookup::Found(hir::Expr::Variable(id)),
            None => Lookup::Reported,
        }
    }
}

impl Names for UpdateNames<'_, '_> {
    fn namespaces(&self) -> &Namespaces {
        self.cx.namespaces
    }

    fn site(&self, at: Span) -> hir::Site {
        self.cx.sites.at(at)
    }

    fn operators(&self) -> &Operators {
        self.cx.operators
    }

    fn resolve(&mut self, name: &str, span: Span, errors: &mut Vec<Diagnostic>) -> Lookup {
        if let Some(lookup) = self.signature().parameter(name) {
            return lookup;
        }
        if let Some(id) = self.cx.declared.find(name, span) {
            return self.variable_read(id);
        }
        if let Some(id) = self.cx.declared.find_constant(name, span) {
            return self.cx.constant_read(id);
        }
        if let Some(relation) = self.signature().per_relation(name) {
            errors.push(errors::per_as_value(name, relation, span));
            return Lookup::Reported;
        }
        if self.cx.namespaces.operator(name, span).is_some() {
            errors.push(errors::operator_as_value(name, span, self.cx.operators));
            return Lookup::Reported;
        }
        Lookup::Unknown
    }

    fn resolve_variable(&mut self, name: &str, span: Span, errors: &mut Vec<Diagnostic>) -> Lookup {
        if self.signature().parameter(name).is_none()
            && let Some(id) = self.cx.declared.find_constant(name, span)
        {
            errors.push(errors::assigned_constant(
                name,
                span,
                &self.cx.declared.constant(id).first,
            ));
            return Lookup::Reported;
        }
        self.resolve(name, span, errors)
    }

    fn narrowing_ended(&self, _: &str) -> Option<Span> {
        None
    }

    fn value_in(
        &mut self,
        name: &str,
        unit: hir::FileId,
        span: Span,
        _errors: &mut Vec<Diagnostic>,
    ) -> Lookup {
        if let Some(id) = self.cx.declared.variable_in(name, unit, span) {
            return self.variable_read(id);
        }
        self.cx
            .declared
            .constant_in(name, unit, span)
            .map_or(Lookup::Unknown, |id| self.cx.constant_read(id))
    }

    fn candidates(&self, at: Span) -> Vec<String> {
        self.signature()
            .parameter_names()
            .chain(self.cx.declared.names(at))
            .map(str::to_string)
            .collect()
    }

    fn leaf_type(&self, leaf: &hir::Expr) -> hir::Type {
        match leaf {
            hir::Expr::Variable(id) => self.cx.variables[id.0]
                .as_ref()
                .expect("only valid variables are resolved")
                .ty
                .clone(),
            hir::Expr::Parameter(name) => self.signature().params[name].ty.clone(),
            _ => unreachable!("only variables, parameters and local variables are leaves"),
        }
    }

    fn fresh_local(&mut self, _: hir::Local) -> hir::LocalId {
        unreachable!("the local variables of a body are its statements'")
    }

    fn next_local(&self) -> usize {
        0
    }

    fn around(&self, name: &str) -> Option<Meaning> {
        self.signature()
            .is_parameter(name)
            .then_some(Meaning::Parameter)
    }

    fn type_parameters(&self) -> Option<&[hir::TypeParameter]> {
        Some(&self.cx.operators.get(self.within).types)
    }

    fn checked(&mut self, id: hir::OperatorId, _: Span, _: &mut Vec<Diagnostic>) -> bool {
        self.cx.checked(id)
    }

    fn bindings(&self) -> ops::Bindings<'_> {
        self.cx.bindings()
    }
}

impl Around for UpdateNames<'_, '_> {
    fn declaring(&self) -> Declaring<'_> {
        Declaring::Operator(self.signature())
    }

    fn refused(&self, statement: &ast::Statement) -> Option<Diagnostic> {
        let ast::Statement::Return(stmt) = statement else {
            return None;
        };
        stmt.value()
            .map(|_| errors::return_in_update(&self.signature().name, stmt))
    }

    fn is_variable(&self, name: &str, span: Span) -> bool {
        self.cx.declared.find(name, span).is_some()
    }

    fn is_out(&self, name: &str) -> bool {
        self.signature().mode(name) == Some(hir::Mode::Out)
    }

    fn assigned_around(
        &mut self,
        name: &str,
        span: Span,
        locals: &[&str],
        errors: &mut Vec<Diagnostic>,
    ) -> Option<Assignee> {
        let cx = self.cx;
        let signature = self.signature();
        if signature.updates(name) {
            let ty = signature.params[name].ty.clone();
            return Some(Assignee::parameter(name, ty, signature.param_origin(name)));
        }
        if signature.is_parameter(name) {
            let hidden = cx.declared.find(name, span).is_some();
            errors.push(errors::assigned_parameter(name, span, hidden));
            return None;
        }
        if let Some(id) = cx.declared.find(name, span) {
            return program_variable(cx, id, name);
        }
        if let Some(id) = cx.declared.find_constant(name, span) {
            errors.push(errors::assigned_constant(
                name,
                span,
                &cx.declared.constant(id).first,
            ));
        } else if let Some((kind, _)) = cx.namespaces.operator(name, span) {
            errors.push(errors::assigned_operator(name, span, kind));
        } else if let Some(error) = errors::unused_unit_in(
            name,
            span,
            cx.files.tree(cx.namespaces.unit_at(span)),
            crate::Kind::Variable,
            cx.namespaces,
        ) {
            errors.push(error);
        } else {
            let variables: Vec<&str> = locals
                .iter()
                .copied()
                .chain(cx.declared.names(span))
                .collect();
            errors.push(errors::unknown_name(name, span, "variable", &variables));
        }
        None
    }
    fn assigned_in_around(
        &mut self,
        written: &ast::QualifiedName,
        unit: hir::FileId,
        errors: &mut Vec<Diagnostic>,
    ) -> Option<Assignee> {
        let cx = self.cx;
        let id = crate::assign::qualified_variable(written, unit, cx.declared, self, errors)?;
        program_variable(cx, id, &written.syntax().text().to_string())
    }
}

/// The variable of the program's `id`, written `name`, as an assignment
/// assigns it: `None` if its declaration has errors, already reported.
fn program_variable(cx: &Context, id: hir::VariableId, name: &str) -> Option<Assignee> {
    let variable = cx.variables[id.0].as_ref()?;
    Some(Assignee {
        target: hir::Target::Variable(id),
        current: hir::Expr::Variable(id),
        name: name.to_string(),
        ty: variable.ty.clone(),
        keys: variable.keys.clone(),
        declaration: Declared::Variable(cx.declared.decl(id).clone()),
        narrowed: None,
    })
}

/// The type `load_csv`'s `columns` has for values `read` as a record's
/// fields are (see [`hir::read_as`]): a tuple, of each attribute, the
/// position of its column, an integer, or, for a value of a type of the
/// program's own, a tuple of its components' positions, in the
/// representation it is read in.
fn positions(read: &hir::Type) -> hir::Type {
    let heading = match read {
        hir::Type::Tuple(heading) => heading,
        hir::Type::Defined(defined) => &defined.components,
        _ => return hir::Type::Scalar(hir::Scalar::Integer),
    };
    hir::Type::Tuple(hir::Heading::new(
        heading
            .named()
            .map(|(name, ty)| (name.to_string(), positions(ty)))
            .collect(),
    ))
}

/// Whether `load_csv` loads an array, written `rows`, of `tuples`, which it
/// reads as `read`, whose attributes it can read from fields: each of a
/// type known here, text, a character, an integer, an approximate number or a boolean,
/// or a type of the program's own whose components it can read, in the
/// representation it reads them in. If not, it is reported.
fn csv_readable(
    tuples: &hir::Type,
    read: &hir::Type,
    rows: &ast::Expr,
    errors: &mut Vec<Diagnostic>,
) -> bool {
    match tuples {
        hir::Type::Tuple(heading) => {
            let mut unreadable = Vec::new();
            let hir::Type::Tuple(read) = read else {
                unreachable!("tuples are read as tuples");
            };
            unreadable_attributes(heading, read, "", &mut unreadable);
            if unreadable.is_empty() {
                return true;
            }
            errors.push(errors::unreadable_attributes(&unreadable, rows));
        }
        // A type parameter of the operator the call is in, whose tuples'
        // attributes each of its calls gives.
        hir::Type::Parameter(parameter) => {
            errors.push(errors::unknown_columns(parameter, rows));
        }
        other => unreachable!("`load_csv` loads tuples, not `{other}`"),
    }
    false
}

/// The attributes of `heading`, and the components of values of the
/// program's own types in it, in the representation they are read in, as
/// `read` has them, that no field can be read as, each named after `prefix`
/// as `t.c` reads it, into `unreadable`.
fn unreadable_attributes<'h>(
    heading: &'h hir::Heading,
    read: &hir::Heading,
    prefix: &str,
    unreadable: &mut Vec<(String, &'h hir::Type)>,
) {
    for (name, ty) in heading.named() {
        let named = format!("{prefix}{name}");
        match (ty, read.get(name)) {
            (hir::Type::Scalar(_), _) => {}
            (hir::Type::Defined(defined), Some(hir::Type::Tuple(components))) => {
                let first = components.named().next().map(|(c, _)| c);
                let own = first
                    .and_then(|c| defined.representation(c))
                    .map_or(&defined.components, |(own, _)| own);
                unreadable_attributes(own, components, &format!("{named}."), unreadable);
            }
            (hir::Type::Defined(defined), _) => {
                let components = &defined.components;
                unreadable_attributes(components, components, &format!("{named}."), unreadable);
            }
            _ => unreadable.push((named, ty)),
        }
    }
}

/// Whether `written`, the argument for `load_csv`'s `columns`, or a tuple
/// selector in it, of the columns of tuples or a value with `heading`,
/// names, for each value of a type of the program's own there, the
/// components of one of its possible representations, as far as it is
/// written as tuple selectors, after reporting why not: components of two
/// representations (A0059), or a name that is no component of the one the
/// others are of, or of any if none is.
fn columns_named(
    heading: &hir::Heading,
    written: &ast::Expr,
    errors: &mut Vec<Diagnostic>,
) -> bool {
    let ast::Expr::Tuple(tuple) = written else {
        return true;
    };
    let mut ok = true;
    for attribute in tuple.attributes() {
        let name = attribute.name().and_then(|n| n.ident());
        let ty = name.as_ref().and_then(|n| heading.get(n.text()));
        let (Some(hir::Type::Defined(defined)), Some(value)) = (ty, attribute.value()) else {
            continue;
        };
        let ast::Expr::Tuple(components) = &value else {
            continue;
        };
        let written: Vec<(String, Span)> = components
            .attributes()
            .filter_map(|a| {
                let ident = a.name()?.ident()?;
                Some((
                    ident.text().to_string(),
                    trendy_syntax::span(ident.text_range()),
                ))
            })
            .collect();
        let named: Vec<(&str, Span)> = written.iter().map(|(n, s)| (n.as_str(), *s)).collect();
        let rule = errors::OneRepresentation::Read;
        let Some(chosen) = check::representation(defined, &named, rule, errors) else {
            ok = false;
            continue;
        };
        let own = match chosen {
            Representation::Other(at) => Some(&defined.representations[at].components),
            Representation::Stored { .. } => Some(&defined.components),
        };
        let Some(own) = own else {
            continue;
        };
        // A misspelling of a component of the representation, or, if no
        // component of one is named, of any.
        let candidates: Vec<&str> = match chosen {
            Representation::Stored { known: false } => defined
                .possible_representations()
                .flat_map(|h| h.named().map(|(c, _)| c))
                .collect(),
            _ => own.named().map(|(c, _)| c).collect(),
        };
        for &(component, span) in &named {
            if own.get(component).is_none() {
                errors.push(errors::unknown_column_component(
                    component,
                    span,
                    defined,
                    &candidates,
                ));
                ok = false;
            }
        }
        ok &= columns_named(own, &value, errors);
    }
    ok
}

/// Whether `columns`, the argument written `written` for `load_csv`'s
/// `columns`, gives each attribute, and each component of a value of a type
/// of the program's own, a column of its own, counting from 1, if it is
/// known here. If not, it is reported (R0016), as it would be when the
/// program runs.
fn distinct_columns(
    columns: &hir::Expr,
    written: &ast::Expr,
    errors: &mut Vec<Diagnostic>,
) -> bool {
    let Some(hir::Value::Tuple(positions)) = crate::ops::known_value(columns) else {
        return true;
    };
    let mut read = Vec::new();
    columns_read(positions, "", written, &mut read);
    let mut ok = true;
    let mut first: BTreeMap<i64, usize> = BTreeMap::new();
    for (i, (name, position, at)) in read.iter().enumerate() {
        if *position < 1 {
            errors.push(errors::not_a_column(name, *position, *at));
            ok = false;
        } else if let Some(&j) = first.get(position) {
            let (other, _, other_at) = &read[j];
            errors.push(errors::shared_column(
                other, name, *position, *other_at, *at,
            ));
            ok = false;
        } else {
            first.insert(*position, i);
        }
    }
    ok
}

/// Each column `positions` gives, in order: the attribute or component
/// read from it, named after `prefix` as `t.c` reads it, its position, and
/// where that is written in `written`: in a tuple selector, its value, and
/// otherwise the whole argument. A tuple of positions, for a value of a
/// type of the program's own, gives its components'.
fn columns_read(
    positions: &hir::TupleValue,
    prefix: &str,
    written: &ast::Expr,
    read: &mut Vec<(String, i64, Span)>,
) {
    for (name, position) in positions.attributes() {
        let value = match written {
            ast::Expr::Tuple(tuple) => tuple
                .attributes()
                .find(|a| a.is_named(name))
                .and_then(|a| a.value()),
            _ => None,
        };
        let value = value.as_ref().unwrap_or(written);
        let named = format!("{prefix}{name}");
        match position {
            hir::Value::Integer(position) => read.push((named, *position, value.span())),
            hir::Value::Tuple(components) => {
                columns_read(components, &format!("{named}."), value, read);
            }
            other => unreachable!("a column's position is an integer, not `{other}`"),
        }
    }
}
