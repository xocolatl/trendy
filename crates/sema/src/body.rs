//! Update operators' bodies: their statements, their local variables, and
//! the expressions in them.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use trendy_diag::{CodeKind, Diagnostic, Span};
use trendy_hir as hir;
use trendy_syntax::SyntaxToken;
use trendy_syntax::ast::{self, AstNode, VarDecl};

use crate::assign::{Assignee, Assigning, Declared};
use crate::check::{self, Checker, Lookup, Names, Representation, Written, argument_names};
use crate::locals::{Declaration, Local, Locals};
use crate::operators::{Chosen, Operators, Signature};
use crate::types::{Expected, Origin};
use crate::{Namespaces, Sites, assign, errors, locals, ops};

/// Everything a body can refer to.
pub(crate) struct Context<'a> {
    /// Where each place in the file is.
    pub(crate) sites: &'a Sites<'a>,
    pub(crate) namespaces: &'a Namespaces,
    pub(crate) operators: &'a Operators,
    /// Each valid read-only operator's value.
    pub(crate) values: &'a HashMap<hir::OperatorId, hir::Expr>,
    /// Each variable, by id: `None` for one with errors, already reported.
    pub(crate) variables: &'a [Option<hir::Variable>],
    pub(crate) variable_ids: &'a HashMap<String, hir::VariableId>,
    /// Each variable's declaration, by name.
    pub(crate) declarations: &'a HashMap<String, VarDecl>,
}

impl Context<'_> {
    /// What evaluating a call in a body substitutes: the read-only
    /// operators' values.
    pub(crate) fn bindings(&self) -> ops::Bindings<'_> {
        ops::Bindings {
            operators: self.operators,
            values: self.values,
        }
    }
}

/// A call, in the body of the update operator `caller`, of `callee`, which
/// gives one of its type parameters, `parameter`, a type built from
/// `caller`'s own, `ty`. If it leads back to `caller`, each time around
/// gives it a larger type.
pub(crate) struct Growing {
    pub(crate) caller: hir::OperatorId,
    pub(crate) callee: hir::OperatorId,
    /// The callee's name, where the call writes it.
    pub(crate) span: Span,
    pub(crate) parameter: hir::TypeParameter,
    pub(crate) ty: hir::Type,
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
/// array is computed from, of the program and local, by name, and where the
/// array is written.
pub(crate) struct Frozen {
    targets: Vec<(hir::Target, String)>,
    array: Span,
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
    let mut statements = Statements {
        cx,
        within,
        locals: Locals::default(),
        deferred,
        frozen: Vec::new(),
        outs: Outs::default(),
    };
    let checked: Vec<ast::Statement> = body.statements().collect();
    let block = statements.block(&checked, errors)?;
    // Every `out` parameter is assigned, whichever way the body goes.
    let signature = cx.operators.get(within);
    let mut ok = true;
    for (name, param) in &signature.params {
        if param.mode != hir::Mode::Out
            || statements.outs.assigned.contains(name)
            || statements.outs.reported.contains(name)
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

/// What is known of an update operator's `out` parameters where the
/// statement being checked is: which are assigned on every path to it, and
/// where it reads and assigns them, which [`Statements::settle`] then
/// reports and counts.
#[derive(Default)]
pub(crate) struct Outs {
    /// Those assigned on every path to the statement.
    assigned: BTreeSet<String>,
    /// Each read of one in the statement, and where.
    reads: Vec<(String, Span)>,
    /// Each the statement assigns, and where it names it: an assignment's
    /// target, which a change such as `insert` reads, or an `out`
    /// argument.
    targets: Vec<(String, Span)>,
    /// Those read before they are assigned, already reported: making one
    /// `var`, as the fix does, makes assigning it on every path needless
    /// too.
    reported: BTreeSet<String>,
}

/// Checks the statements of the update operator `within`, each block's
/// local variables in scope in it.
struct Statements<'a, 'b> {
    cx: &'a Context<'b>,
    within: hir::OperatorId,
    locals: Locals,
    /// What can only be checked once every body is.
    deferred: &'a mut Deferred,
    /// What each `for` loop the statement being checked is in keeps from
    /// changing, outermost first.
    frozen: Vec<Frozen>,
    /// The `out` parameters assigned on every path to the statement being
    /// checked, and its reads of them.
    outs: Outs,
}

impl Statements<'_, '_> {
    fn names(&mut self) -> BodyNames<'_, '_> {
        BodyNames {
            cx: self.cx,
            within: self.within,
            locals: &mut self.locals,
            frozen: &self.frozen,
            outs: &mut self.outs,
        }
    }

    /// Report each read of an `out` parameter in the statement just
    /// checked, `statement`, before it is assigned on every path to there,
    /// then count each it assigns as assigned from then on: if it has
    /// errors, each it names as assigned. A change to one that reads it,
    /// such as `insert`, reads it where it names it.
    fn settle(&mut self, statement: Option<&hir::Statement>, errors: &mut Vec<Diagnostic>) {
        let targets = std::mem::take(&mut self.outs.targets);
        let Some(statement) = statement else {
            self.flush(errors);
            self.outs
                .assigned
                .extend(targets.into_iter().map(|(name, _)| name));
            return;
        };
        let assigned: Vec<&hir::Target> = match statement {
            hir::Statement::Assign(assignments) => {
                for assignment in assignments {
                    let hir::Target::Parameter(name) = &assignment.target else {
                        continue;
                    };
                    if ops::targets_read(&assignment.value).contains(&assignment.target)
                        && !self.outs.reads.iter().any(|(n, _)| n == name)
                        && let Some((_, span)) = targets.iter().find(|(n, _)| n == name)
                    {
                        self.outs.reads.push((name.clone(), *span));
                    }
                }
                assignments.iter().map(|a| &a.target).collect()
            }
            hir::Statement::Call(call) => call.updated.values().collect(),
            _ => Vec::new(),
        };
        self.flush(errors);
        let signature = self.cx.operators.get(self.within);
        for target in assigned {
            if let hir::Target::Parameter(name) = target
                && signature.mode(name) == Some(hir::Mode::Out)
            {
                self.outs.assigned.insert(name.clone());
            }
        }
    }

    /// Report each read of an `out` parameter checked since the last,
    /// before it is assigned on every path to here.
    fn flush(&mut self, errors: &mut Vec<Diagnostic>) {
        let signature = self.cx.operators.get(self.within);
        let mut reported: Vec<Span> = Vec::new();
        for (name, span) in std::mem::take(&mut self.outs.reads) {
            if self.outs.assigned.contains(&name) || reported.contains(&span) {
                continue;
            }
            reported.push(span);
            let decl = signature.param_decl(&name);
            errors.push(errors::read_before_assigned(&name, span, decl.as_ref()));
            self.outs.reported.insert(name);
        }
    }

    /// The block of `statements`, whose local variables are in scope in
    /// all of it, or `None` after reporting its errors.
    fn block(
        &mut self,
        statements: &[ast::Statement],
        errors: &mut Vec<Diagnostic>,
    ) -> Option<hir::Block> {
        let op = self.cx.operators.get(self.within);
        let mut ok = self.locals.enter(statements.iter().cloned(), op, errors);
        let mut checked = Vec::new();
        for stmt in statements {
            match self.sited(stmt, errors) {
                Some(statement) => checked.push(statement),
                None => ok = false,
            }
        }
        let locals = self.locals.exit();
        ok.then_some(hir::Block {
            statements: checked,
            locals,
        })
    }

    /// `stmt`, with its site, where it starts, or `None` after reporting
    /// its errors.
    fn sited(
        &mut self,
        stmt: &ast::Statement,
        errors: &mut Vec<Diagnostic>,
    ) -> Option<(hir::Site, hir::Statement)> {
        let site = self
            .cx
            .sites
            .at(trendy_syntax::span(stmt.syntax().text_range()));
        Some((site, self.statement(stmt, errors)?))
    }

    fn statement(
        &mut self,
        stmt: &ast::Statement,
        errors: &mut Vec<Diagnostic>,
    ) -> Option<hir::Statement> {
        let (cx, within) = (self.cx, self.within);
        let checked = match stmt {
            ast::Statement::Call(stmt) => self.call(stmt, errors),
            ast::Statement::Assign(stmt) => {
                let clauses: Vec<_> = stmt.clauses().collect();
                assign::statement(&clauses, &mut self.names(), errors)
            }
            ast::Statement::Block(block) => assign::block(block, &mut self.names(), errors),
            ast::Statement::Return(stmt) => {
                let name = &cx.operators.get(within).name;
                errors.push(errors::return_in_update(name, stmt));
                None
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
        };
        self.settle(checked.as_ref(), errors);
        checked
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
        let targets = ops::targets_read(&array)
            .into_iter()
            .map(|target| {
                let name = match &target {
                    hir::Target::Variable(id) => self.cx.variables[id.0]
                        .as_ref()
                        .map_or_else(String::new, |v| v.name.clone()),
                    hir::Target::Local(id) => self.locals.get(*id).name.clone(),
                    hir::Target::Parameter(name) => name.clone(),
                };
                (target, name)
            })
            .collect();
        self.frozen.push(Frozen {
            targets,
            array: written.span(),
        });
        let local = hir::Local {
            name,
            ty: *element,
            keys: Vec::new(),
        };
        let op = self.cx.operators.get(self.within);
        let (id, mut ok) =
            self.locals
                .enter_loop(&variable, local, statements.iter().cloned(), op, errors);
        // The block may not run, so what it assigns isn't assigned after.
        let before = self.outs.assigned.clone();
        let mut checked = Vec::new();
        for statement in &statements {
            match self.sited(statement, errors) {
                Some(statement) => checked.push(statement),
                None => ok = false,
            }
        }
        self.outs.assigned = before;
        let locals = self.locals.exit();
        self.frozen.pop();
        let body = hir::Block {
            statements: checked,
            locals,
        };
        ok.then_some(hir::Statement::For(id?, array, body))
    }

    /// The declaration `decl` of a local variable: its initial value's
    /// assignment. Its local variable is declared from then on.
    fn declaration(
        &mut self,
        decl: &VarDecl,
        errors: &mut Vec<Diagnostic>,
    ) -> Option<hir::Statement> {
        let op = self.cx.operators.get(self.within);
        let checked = locals::local(decl, &op.types, &mut self.names(), errors);
        let (local, value) = checked.unzip();
        let id = self.locals.declare(decl, local)?;
        Some(hir::Statement::Assign(vec![hir::Assignment {
            target: hir::Target::Local(id),
            value: value?,
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
        let (cx, within) = (self.cx, self.within);
        let ident = stmt.callee()?.ident()?;
        let name = ident.text();
        let span = trendy_syntax::span(ident.text_range());
        let args = stmt.arguments();
        let names = args
            .as_ref()
            .map(|args| argument_names(Written::Arguments(args)))
            .unwrap_or_default();
        let signature = match cx.operators.chosen(name, &names) {
            Some(Chosen::One(signature)) => signature,
            Some(Chosen::Tied(tied)) => {
                // Without arguments, a syntax error, which is reported.
                if let Some(args) = &args {
                    errors.push(errors::unchosen_operator(
                        name,
                        &tied,
                        Written::Arguments(args),
                    ));
                }
                return None;
            }
            None => {
                let what = if cx.operators.get(within).is_parameter(name) {
                    Some("a parameter")
                } else if self.locals.find(name).is_some() {
                    Some("a local variable")
                } else {
                    cx.namespaces.variable(name).map(|_| "a variable")
                };
                if let Some(what) = what {
                    errors.push(errors::not_an_operator(name, span, what));
                } else {
                    let operators = cx.operators.names();
                    errors.push(errors::unknown_operator(
                        name,
                        span,
                        &operators,
                        args.as_ref().map(Written::Arguments),
                        false,
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
        if signature.builtin == Some(hir::Builtin::Update(hir::UpdateBuiltin::LoadCsv))
            && !self.csv_loadable(&call, signature, &args, errors)
        {
            return None;
        }
        if !self.updated(&call, &args, signature.id, errors) {
            return None;
        }
        // A type parameter passed on as it is never grows.
        for (parameter, ty) in signature.types.iter().zip(&call.types) {
            if !matches!(ty, hir::Type::Parameter(_)) && ty.parameters().next().is_some() {
                self.deferred.growing.push(Growing {
                    caller: within,
                    callee: signature.id,
                    span,
                    parameter: parameter.clone(),
                    ty: ty.clone(),
                });
            }
        }
        let looped: Vec<(hir::VariableId, String, Span)> = self
            .frozen
            .iter()
            .flat_map(|frozen| {
                frozen
                    .targets
                    .iter()
                    .filter_map(|(target, name)| match target {
                        hir::Target::Variable(id) => Some((*id, name.clone(), frozen.array)),
                        hir::Target::Local(_) | hir::Target::Parameter(_) => None,
                    })
            })
            .collect();
        if !looped.is_empty() {
            self.deferred.in_loops.push(LoopCall {
                callee: signature.id,
                span: stmt.span(),
                looped,
            });
        }
        Some(hir::Statement::Call(call))
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
            let passed: Vec<(String, Span)> = self
                .outs
                .reads
                .iter()
                .filter(|(_, s)| within(s))
                .cloned()
                .collect();
            self.outs.reads.retain(|(_, s)| !within(s));
            self.outs.targets.extend(passed);
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
            let mode = self.cx.operators.get(callee).mode(ident.text());
            let param = format!("{} {}", mode.map_or("var", hir::Mode::word), ident.text());
            let name = match target {
                hir::Target::Variable(id) => self.cx.variables[id.0]
                    .as_ref()
                    .map_or_else(String::new, |v| v.name.clone()),
                hir::Target::Local(id) => self.locals.get(*id).name.clone(),
                hir::Target::Parameter(name) => name.clone(),
            };
            let span = value.span();
            if self.names().assigned(&name, span, errors).is_none() {
                ok = false;
                continue;
            }
            if let Some((_, first, earlier)) = passed.iter().find(|(t, ..)| *t == target) {
                errors.push(errors::passed_twice(&name, first, &param, span, *earlier));
                ok = false;
                continue;
            }
            passed.push((target, param.clone(), span));
            if let hir::Target::Variable(variable) = target {
                self.deferred.passed.push(Passed {
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
        let pointless = errors::pointless_loop(span, &written, &body, check);
        let reported = pointless.is_some();
        errors.extend(pointless);
        let boolean = Expected {
            ty: hir::Type::Scalar(hir::Scalar::Boolean),
            origin: Origin::Operator(keyword),
        };
        let condition = Checker::new(&mut self.names()).expr(&written, Some(&boolean), errors);
        if !reported && condition == Some(hir::Expr::Constant(hir::Value::Boolean(true))) {
            errors.push(errors::endless_loop(&written));
        }
        let statements: Vec<ast::Statement> = body.statements().collect();
        let block = match check {
            // The block may not run, so what it assigns isn't assigned
            // after; the condition is computed before it.
            hir::Check::Before => {
                self.flush(errors);
                let before = self.outs.assigned.clone();
                let block = self.block(&statements, errors);
                self.outs.assigned = before;
                block
            }
            // The block runs at least once, before the condition.
            hir::Check::After => {
                let condition_reads = std::mem::take(&mut self.outs.reads);
                let block = self.block(&statements, errors);
                self.outs.reads = condition_reads;
                self.flush(errors);
                block
            }
        };
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
        // unless the condition says which.
        let before = self.outs.assigned.clone();
        let then = self.branch(
            Some(ast::ElseBranch::Block(then_block)),
            taken == Some(false),
            errors,
        );
        let after_then = std::mem::replace(&mut self.outs.assigned, before);
        let otherwise = self.branch(stmt.else_branch(), taken == Some(true), errors);
        let after_otherwise = std::mem::take(&mut self.outs.assigned);
        self.outs.assigned = match taken {
            Some(true) => after_then,
            Some(false) => after_otherwise,
            None => after_then.intersection(&after_otherwise).cloned().collect(),
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
                    .cx
                    .sites
                    .at(trendy_syntax::span(stmt.syntax().text_range()));
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
/// read no variable of the program's.
pub(crate) fn deferred_checks(
    deferred: &Deferred,
    bodies: &HashMap<hir::OperatorId, (Vec<hir::Local>, hir::Block)>,
    operators: &Operators,
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
    growing_recursion(&deferred.growing, bodies, operators, errors);
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
        calls_in(body, &mut called);
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
    each_statement(block, &mut |statement| {
        let targets: Vec<&hir::Target> = match statement {
            hir::Statement::Assign(assignments) => assignments.iter().map(|a| &a.target).collect(),
            hir::Statement::Call(call) => call.updated.values().collect(),
            _ => Vec::new(),
        };
        for target in targets {
            if let hir::Target::Variable(id) = target
                && !assigned.contains(id)
            {
                assigned.push(*id);
            }
        }
    });
}

/// Add each variable of the program that `block`'s calls pass to a `var`
/// parameter, in blocks within it too, to `passed`.
fn passed_in(block: &hir::Block, passed: &mut Vec<hir::VariableId>) {
    each_statement(block, &mut |statement| {
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

/// Call `f` with each of `block`'s statements, and those in blocks within
/// them, each before those within it.
fn each_statement(block: &hir::Block, f: &mut dyn FnMut(&hir::Statement)) {
    for (_, statement) in &block.statements {
        f(statement);
        match statement {
            hir::Statement::If(_, then, otherwise) => {
                each_statement(then, f);
                each_statement(otherwise, f);
            }
            hir::Statement::Loop(_, body, _) | hir::Statement::For(_, _, body) => {
                each_statement(body, f);
            }
            hir::Statement::Call(_) | hir::Statement::Assign(_) => {}
        }
    }
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
        calls_in(body, &mut callees);
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
    each_statement(block, &mut |statement| match statement {
        hir::Statement::Call(call) => call.args.values().for_each(&mut *f),
        hir::Statement::Assign(assignments) => {
            for assignment in assignments {
                f(&assignment.value);
            }
        }
        hir::Statement::If(condition, ..) | hir::Statement::Loop(condition, ..) => f(condition),
        hir::Statement::For(_, array, _) => f(array),
    });
}

/// Report each call in `growing` that leads back to its caller through
/// the calls in the update operators' `bodies`: the operator would need a
/// function for each of the ever larger types the type parameter is given
/// each time around (see [`hir::Operator`]).
fn growing_recursion(
    growing: &[Growing],
    bodies: &HashMap<hir::OperatorId, (Vec<hir::Local>, hir::Block)>,
    operators: &Operators,
    errors: &mut Vec<Diagnostic>,
) {
    let calls = |id: &hir::OperatorId| {
        let mut called = Vec::new();
        if let Some((_, body)) = bodies.get(id) {
            calls_in(body, &mut called);
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

/// Add each operator `block`'s statements call, in blocks within it too, to
/// `called`.
fn calls_in(block: &hir::Block, called: &mut Vec<hir::OperatorId>) {
    each_statement(block, &mut |statement| {
        if let hir::Statement::Call(call) = statement {
            called.push(call.operator);
        }
    });
}

/// Names in the body of `within`: its parameters and its local variables,
/// which hide variables of the same name, and the variables.
pub(crate) struct BodyNames<'a, 'b> {
    pub(crate) cx: &'a Context<'b>,
    pub(crate) within: hir::OperatorId,
    pub(crate) locals: &'a mut Locals,
    /// What the `for` loops the statement being checked is in keep from
    /// changing.
    frozen: &'a [Frozen],
    /// Where the statement being checked reads and assigns the operator's
    /// `out` parameters.
    outs: &'a mut Outs,
}

impl BodyNames<'_, '_> {
    fn signature(&self) -> &Signature {
        self.cx.operators.get(self.within)
    }
}

impl Names for BodyNames<'_, '_> {
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
            if self.signature().mode(name) == Some(hir::Mode::Out) {
                self.outs.reads.push((name.to_string(), span));
            }
            return lookup;
        }
        match self.locals.find(name) {
            Some(Local::Declared(id, ..)) => return Lookup::Found(hir::Expr::Local(id)),
            Some(Local::Later(decl)) => {
                errors.push(errors::local_before_declaration(
                    name,
                    span,
                    decl,
                    self.cx.variable_ids.contains_key(name),
                ));
                return Lookup::Reported;
            }
            Some(Local::Ended(decl)) => {
                errors.push(errors::local_out_of_scope(name, span, decl));
                return Lookup::Reported;
            }
            Some(Local::Invalid) => return Lookup::Reported,
            None => {}
        }
        if let Some(&id) = self.cx.variable_ids.get(name) {
            // A variable with errors in its declaration is already reported.
            return match self.cx.variables[id.0] {
                Some(_) => Lookup::Found(hir::Expr::Variable(id)),
                None => Lookup::Reported,
            };
        }
        if let Some(relation) = self.cx.operators.get(self.within).per_relation(name) {
            errors.push(errors::per_as_value(name, relation, span));
            return Lookup::Reported;
        }
        if self.cx.namespaces.operator(name).is_some() {
            errors.push(errors::operator_as_value(name, span, self.cx.operators));
            return Lookup::Reported;
        }
        Lookup::Unknown
    }

    fn candidates(&self) -> Vec<String> {
        self.signature()
            .parameter_names()
            .chain(self.locals.names())
            .chain(self.cx.variable_ids.keys().map(String::as_str))
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
            hir::Expr::Local(id) => self.locals.get(*id).ty.clone(),
            _ => unreachable!("only variables, parameters and local variables are leaves"),
        }
    }

    fn fresh_local(&mut self, local: hir::Local) -> hir::LocalId {
        self.locals.fresh(local)
    }

    fn hides(&self, name: &str) -> bool {
        self.signature().is_parameter(name)
            || matches!(
                self.locals.find(name),
                Some(Local::Declared(..) | Local::Later(_) | Local::Invalid)
            )
    }

    fn type_parameters(&self) -> Option<&[hir::TypeParameter]> {
        Some(&self.cx.operators.get(self.within).types)
    }

    fn checked(&mut self, id: hir::OperatorId, _: Span, _: &mut Vec<Diagnostic>) -> bool {
        // Every read-only operator is checked with the globals.
        self.cx.values.contains_key(&id)
    }

    fn bindings(&self) -> ops::Bindings<'_> {
        self.cx.bindings()
    }
}

impl Assigning for BodyNames<'_, '_> {
    /// The variable `name`, unless a `for` loop that the assignment is in
    /// goes through an array computed from it.
    fn assigned(
        &mut self,
        name: &str,
        span: Span,
        errors: &mut Vec<Diagnostic>,
    ) -> Option<Assignee> {
        let assignee = self.variable(name, span, errors)?;
        let frozen = self.frozen.iter().find(|frozen| {
            frozen
                .targets
                .iter()
                .any(|(target, _)| *target == assignee.target)
        });
        if let Some(frozen) = frozen {
            errors.push(errors::assigned_in_loop(name, span, frozen.array));
            return None;
        }
        Some(assignee)
    }
}

impl BodyNames<'_, '_> {
    /// The variable `name`, of the program or local, that an assignment at
    /// `span` assigns, or `None` after reporting why it can't be.
    fn variable(
        &mut self,
        name: &str,
        span: Span,
        errors: &mut Vec<Diagnostic>,
    ) -> Option<Assignee> {
        let cx = self.cx;
        let signature = self.signature();
        if signature.updates(name) {
            let ty = signature.params[name].ty.clone();
            let assignee = Assignee::parameter(name, ty, signature.param_origin(name));
            if signature.mode(name) == Some(hir::Mode::Out) {
                self.outs.targets.push((name.to_string(), span));
            }
            return Some(assignee);
        }
        if signature.is_parameter(name) {
            let hidden = cx.variable_ids.contains_key(name);
            errors.push(errors::assigned_parameter(name, span, hidden));
            return None;
        }
        match self.locals.find(name) {
            Some(Local::Declared(id, local, Declaration::Var(declaration))) => {
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
                    cx.variable_ids.contains_key(name),
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
        if let Some(&id) = cx.variable_ids.get(name) {
            // A variable with errors in its declaration is already reported.
            let variable = cx.variables[id.0].as_ref()?;
            return Some(Assignee {
                target: hir::Target::Variable(id),
                current: hir::Expr::Variable(id),
                name: variable.name.clone(),
                ty: variable.ty.clone(),
                keys: variable.keys.clone(),
                declaration: Declared::Variable(cx.declarations[&variable.name].clone()),
            });
        }
        if let Some((kind, _)) = cx.namespaces.operator(name) {
            errors.push(errors::assigned_operator(name, span, kind));
        } else {
            let variables: Vec<&str> = self
                .locals
                .names()
                .chain(cx.variable_ids.keys().map(String::as_str))
                .collect();
            errors.push(errors::unknown_name(name, span, "variable", &variables));
        }
        None
    }
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
/// type known here, text, an integer, an approximate number or a boolean,
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
            Representation::Other(component) => defined.representation(component).map(|(h, _)| h),
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
