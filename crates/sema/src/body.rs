//! Update operators' bodies: their statements, their local variables, and
//! the expressions in them.

use std::collections::{BTreeMap, HashMap};

use trendy_diag::{CodeKind, Diagnostic, Span};
use trendy_hir as hir;
use trendy_syntax::SyntaxToken;
use trendy_syntax::ast::{self, AstNode, VarDecl};

use crate::assign::{Assignee, Assigning};
use crate::check::{Checker, Lookup, Names, Written};
use crate::locals::{Local, Locals};
use crate::operators::Operators;
use crate::types::{Expected, Origin};
use crate::{Namespaces, assign, errors, locals, ops};

/// Everything a body can refer to.
pub(crate) struct Context<'a> {
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
    /// What evaluating a call in a body substitutes: no variable, since
    /// they change when the program runs.
    pub(crate) fn bindings(&self) -> ops::Bindings<'_> {
        ops::Bindings {
            operators: self.operators,
            values: self.values,
            variables: None,
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

/// The statements of the update operator `op`, whose id is `within`, and
/// its local variables. Each call that gives a type parameter a type built
/// from `op`'s own is added to `growing`.
pub(crate) fn body(
    op: &ast::Operator,
    within: hir::OperatorId,
    cx: &Context,
    growing: &mut Vec<Growing>,
    errors: &mut Vec<Diagnostic>,
) -> Option<(Vec<hir::Local>, hir::Block)> {
    let body = op.body()?;
    let mut statements = Statements {
        cx,
        within,
        locals: Locals::default(),
        growing,
    };
    let checked: Vec<ast::Statement> = body.statements().collect();
    let block = statements.block(&checked, errors)?;
    Some((statements.locals.finish(), block))
}

/// Checks the statements of the update operator `within`, each block's
/// local variables in scope in it.
struct Statements<'a, 'b> {
    cx: &'a Context<'b>,
    within: hir::OperatorId,
    locals: Locals,
    /// Each call that gives a type parameter a type built from the
    /// operator's own (see [`Growing`]).
    growing: &'a mut Vec<Growing>,
}

impl Statements<'_, '_> {
    fn names(&mut self) -> BodyNames<'_, '_> {
        BodyNames {
            cx: self.cx,
            within: self.within,
            locals: &mut self.locals,
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
            match self.statement(stmt, errors) {
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

    fn statement(
        &mut self,
        stmt: &ast::Statement,
        errors: &mut Vec<Diagnostic>,
    ) -> Option<hir::Statement> {
        let (cx, within) = (self.cx, self.within);
        match stmt {
            ast::Statement::Call(stmt) => {
                call(stmt, within, cx, &mut self.locals, self.growing, errors)
            }
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
            ast::Statement::Var(decl) => declaration(decl, within, cx, &mut self.locals, errors),
            ast::Statement::If(stmt) => self.if_stmt(stmt, errors),
            ast::Statement::While(stmt) => self.loop_stmt(
                stmt.keyword(),
                stmt.condition(),
                stmt.body(),
                hir::Check::Before,
                stmt.span(),
                errors,
            ),
            ast::Statement::Loop(stmt) => self.loop_stmt(
                stmt.while_keyword(),
                stmt.condition(),
                stmt.body(),
                hir::Check::After,
                stmt.span(),
                errors,
            ),
        }
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
        let block = self.block(&statements, errors);
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
        let taken = match &condition {
            Some(hir::Expr::Constant(hir::Value::Boolean(taken))) => Some(*taken),
            _ => None,
        };
        let then = self.branch(
            Some(ast::ElseBranch::Block(then_block)),
            taken == Some(false),
            errors,
        );
        let otherwise = self.branch(stmt.else_branch(), taken == Some(true), errors);
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
                self.if_stmt(&stmt, reported_to)
                    .map(|statement| hir::Block {
                        statements: vec![statement],
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

/// The declaration `decl` of a local variable in the body of `within`: its
/// initial value's assignment. Its local variable is declared from then on
/// in `locals`.
fn declaration(
    decl: &VarDecl,
    within: hir::OperatorId,
    cx: &Context,
    locals: &mut Locals,
    errors: &mut Vec<Diagnostic>,
) -> Option<hir::Statement> {
    let op = cx.operators.get(within);
    let checked = {
        let mut names = BodyNames { cx, within, locals };
        locals::local(decl, &op.types, &mut names, errors)
    };
    let (local, value) = checked.unzip();
    let id = locals.declare(decl, local)?;
    Some(hir::Statement::Assign(vec![hir::Assignment {
        target: hir::Target::Local(id),
        value: value?,
    }]))
}

/// `call name{arguments};`, of an update operator, in the body of
/// `within`, whose local variables are `locals`.
fn call(
    stmt: &ast::CallStmt,
    within: hir::OperatorId,
    cx: &Context,
    locals: &mut Locals,
    growing: &mut Vec<Growing>,
    errors: &mut Vec<Diagnostic>,
) -> Option<hir::Statement> {
    let ident = stmt.callee()?.ident()?;
    let name = ident.text();
    let span = trendy_syntax::span(ident.text_range());
    let Some(signature) = cx.operators.named(name) else {
        let what = if cx.operators.get(within).params.contains_key(name) {
            Some("a parameter")
        } else if locals.find(name).is_some() {
            Some("a local variable")
        } else {
            cx.namespaces.variable(name).map(|_| "a variable")
        };
        if let Some(what) = what {
            errors.push(errors::not_an_operator(name, span, what));
        } else {
            let operators = cx.operators.names();
            let args = stmt.arguments();
            errors.push(errors::unknown_operator(
                name,
                span,
                &operators,
                args.as_ref().map(Written::Arguments),
                false,
            ));
        }
        return None;
    };
    if signature.read_only {
        errors.push(errors::call_of_read_only(name, span, signature.defined()));
        return None;
    }
    let args = stmt.arguments()?;
    if !signature.valid {
        return None;
    }
    let mut names = BodyNames { cx, within, locals };
    let call = Checker::new(&mut names).arguments(Written::Arguments(&args), signature, errors)?;
    // A type parameter passed on as it is never grows.
    for (parameter, ty) in signature.types.iter().zip(&call.types) {
        if !matches!(ty, hir::Type::Parameter(_)) && ty.parameters().next().is_some() {
            growing.push(Growing {
                caller: within,
                callee: signature.id,
                span,
                parameter: parameter.clone(),
                ty: ty.clone(),
            });
        }
    }
    Some(hir::Statement::Call(call))
}

/// Report each call in `growing` that leads back to its caller through
/// the calls in the update operators' `bodies`: the operator would need a
/// function for each of the ever larger types the type parameter is given
/// each time around (see [`hir::Operator`]).
pub(crate) fn growing_recursion(
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
    for statement in &block.statements {
        match statement {
            hir::Statement::Call(call) => called.push(call.operator),
            hir::Statement::Assign(_) => {}
            hir::Statement::If(_, then, otherwise) => {
                calls_in(then, called);
                calls_in(otherwise, called);
            }
            hir::Statement::Loop(_, body, _) => calls_in(body, called),
        }
    }
}

/// Names in the body of `within`: its parameters and its local variables,
/// which hide variables of the same name, and the variables.
pub(crate) struct BodyNames<'a, 'b> {
    pub(crate) cx: &'a Context<'b>,
    pub(crate) within: hir::OperatorId,
    pub(crate) locals: &'a mut Locals,
}

impl BodyNames<'_, '_> {
    fn params(&self) -> &BTreeMap<String, hir::Type> {
        &self.cx.operators.get(self.within).params
    }
}

impl Names for BodyNames<'_, '_> {
    fn namespaces(&self) -> &Namespaces {
        self.cx.namespaces
    }

    fn operators(&self) -> &Operators {
        self.cx.operators
    }

    fn resolve(&mut self, name: &str, span: Span, errors: &mut Vec<Diagnostic>) -> Lookup {
        if self.params().contains_key(name) {
            return Lookup::Found(hir::Expr::Parameter(name.to_string()));
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
            errors.push(errors::operator_as_value(name, span));
            return Lookup::Reported;
        }
        Lookup::Unknown
    }

    fn candidates(&self) -> Vec<String> {
        self.params()
            .keys()
            .map(String::as_str)
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
            hir::Expr::Parameter(name) => self.params()[name].clone(),
            hir::Expr::Local(id) => self.locals.get(*id).ty.clone(),
            _ => unreachable!("only variables, parameters and local variables are leaves"),
        }
    }

    fn fresh_local(&mut self, local: hir::Local) -> hir::LocalId {
        self.locals.fresh(local)
    }

    fn hides(&self, name: &str) -> bool {
        self.params().contains_key(name)
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
    fn assigned(
        &mut self,
        name: &str,
        span: Span,
        errors: &mut Vec<Diagnostic>,
    ) -> Option<Assignee> {
        let cx = self.cx;
        if self.params().contains_key(name) {
            let hidden = cx.variable_ids.contains_key(name);
            errors.push(errors::assigned_parameter(name, span, hidden));
            return None;
        }
        match self.locals.find(name) {
            Some(Local::Declared(id, local, declaration)) => {
                return Some(Assignee::local(id, local, declaration));
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
                name: variable.name.clone(),
                ty: variable.ty.clone(),
                keys: variable.keys.clone(),
                declaration: cx.declarations[&variable.name].clone(),
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
