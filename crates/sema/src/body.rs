//! Update operators' bodies: their statements, their local variables, and
//! the expressions in them.

use std::collections::{BTreeMap, HashMap};

use trendy_diag::{Diagnostic, Span};
use trendy_hir as hir;
use trendy_syntax::ast::{self, AstNode, VarDecl};

use crate::check::{Checker, Lookup, Names, Written};
use crate::operators::Operators;
use crate::types::{self, Expected, Origin, TypeParameters};
use crate::{Namespaces, assign, errors, keys, ops};

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

/// The local variables of a body: each one declared in it, in order, and
/// how many of their declarations come before the statement being checked.
/// A local variable is in scope in its whole body, so that a name means
/// one thing in all of it, but it has no value before its declaration.
#[derive(Default)]
pub(crate) struct Locals {
    declared: Vec<Declared>,
    /// How many declarations come before the statement being checked.
    reached: usize,
}

/// A local variable's declaration, by the name it declares.
struct Declared {
    name: String,
    decl: VarDecl,
    /// The local variable, once its declaration is checked: `None` before
    /// then, or after reporting its errors.
    local: Option<hir::Local>,
}

/// What a name is among a body's local variables.
pub(crate) enum Local<'a> {
    /// A local variable declared before here.
    Declared(hir::LocalId, &'a hir::Local, &'a VarDecl),
    /// One declared after here, or here, which has no value yet.
    Later(&'a VarDecl),
    /// One whose declaration has errors, already reported.
    Invalid,
}

impl Locals {
    /// The local variable named `name`, if there is one.
    pub(crate) fn find(&self, name: &str) -> Option<Local<'_>> {
        let (index, declared) = self
            .declared
            .iter()
            .enumerate()
            .find(|(_, declared)| declared.name == name)?;
        Some(match &declared.local {
            _ if index >= self.reached => Local::Later(&declared.decl),
            Some(local) => Local::Declared(hir::LocalId(index), local, &declared.decl),
            None => Local::Invalid,
        })
    }

    /// The names of the local variables declared before here.
    pub(crate) fn names(&self) -> impl Iterator<Item = &str> {
        self.declared[..self.reached]
            .iter()
            .filter(|declared| declared.local.is_some())
            .map(|declared| declared.name.as_str())
    }
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
) -> Option<(Vec<hir::Local>, Vec<hir::Statement>)> {
    let body = op.body()?;
    let name = &cx.operators.get(within).name;
    // `main` with a result type is reported with its `return`s.
    let returns_reported = name == "main" && op.result().is_some();
    let (mut locals, mut ok) = declarations(&body, within, cx, errors);
    let mut statements = Vec::new();
    for stmt in body.statements() {
        let statement = match &stmt {
            ast::Statement::Call(stmt) => call(stmt, within, cx, &locals, growing, errors),
            ast::Statement::Assign(stmt) => assign::statement(
                &stmt.clauses().collect::<Vec<_>>(),
                within,
                cx,
                &locals,
                errors,
            ),
            ast::Statement::Block(block) => assign::block(block, within, cx, &locals, errors),
            ast::Statement::Return(stmt) => {
                if !returns_reported {
                    errors.push(errors::return_in_update(name, stmt));
                }
                None
            }
            ast::Statement::Var(decl) => declaration(decl, within, cx, &mut locals, errors),
        };
        match statement {
            Some(statement) => statements.push(statement),
            None => ok = false,
        }
    }
    let locals: Option<Vec<hir::Local>> = locals.declared.into_iter().map(|d| d.local).collect();
    ok.then_some((locals?, statements))
}

/// The local variables `body` declares, and whether each has a name of its
/// own, after reporting each that doesn't: an operator's names, its
/// parameters' and its local variables', each mean one thing in its body,
/// and hide the program's variables.
fn declarations(
    body: &ast::Body,
    within: hir::OperatorId,
    cx: &Context,
    errors: &mut Vec<Diagnostic>,
) -> (Locals, bool) {
    let mut locals = Locals::default();
    let mut ok = true;
    let op = cx.operators.get(within);
    for stmt in body.statements() {
        let ast::Statement::Var(decl) = stmt else {
            continue;
        };
        let Some(ident) = decl.name().and_then(|n| n.ident()) else {
            continue;
        };
        let (name, span) = (ident.text(), trendy_syntax::span(ident.text_range()));
        let first = locals.declared.iter().find(|other| other.name == name);
        if let Some(first) = first {
            errors.push(errors::local_declared_again(
                name,
                &decl,
                &first.decl,
                &op.name,
            ));
            ok = false;
        } else if op.params.contains_key(name) {
            let parameter = op
                .decl
                .as_ref()
                .and_then(ast::Operator::params)
                .and_then(|params| {
                    params.attributes().find_map(|a| {
                        let name_node = a.name()?;
                        (name_node.ident()?.text() == name).then(|| name_node.span())
                    })
                });
            errors.push(errors::local_named_as_parameter(
                name, span, parameter, &op.name,
            ));
            ok = false;
        } else {
            locals.declared.push(Declared {
                name: name.to_string(),
                decl,
                local: None,
            });
        }
    }
    (locals, ok)
}

/// The declaration `decl` of a local variable in the body of `within`: its
/// initial value's assignment, once checked as a program variable's
/// declaration is, but with a value known only when running. Its local
/// variable is declared from then on in `locals`.
fn declaration(
    decl: &VarDecl,
    within: hir::OperatorId,
    cx: &Context,
    locals: &mut Locals,
    errors: &mut Vec<Diagnostic>,
) -> Option<hir::Statement> {
    let index = locals.reached;
    let declares = locals
        .declared
        .get(index)
        .is_some_and(|declared| declared.decl == *decl);
    let checked = local(decl, within, cx, locals, errors);
    if !declares {
        // Declared again, already reported, and checked only for errors
        // of its own.
        return None;
    }
    let (local, value) = checked.unzip();
    locals.declared[index].local = local;
    locals.reached += 1;
    Some(hir::Statement::Assign(vec![hir::Assignment {
        target: hir::Target::Local(hir::LocalId(index)),
        value: value?,
    }]))
}

/// The local variable `decl` declares in the body of `within`, where
/// `locals` are the local variables, and its initial value: the one given,
/// or else its type's default, which a type parameter's type has none of.
fn local(
    decl: &VarDecl,
    within: hir::OperatorId,
    cx: &Context,
    locals: &Locals,
    errors: &mut Vec<Diagnostic>,
) -> Option<(hir::Local, hir::Expr)> {
    let ident = decl.name()?.ident()?;
    let (name, span) = (ident.text(), trendy_syntax::span(ident.text_range()));
    let op = cx.operators.get(within);
    let declared = decl.type_expr().map(|t| {
        let mut parameters = TypeParameters::Fixed(&op.types);
        (
            types::resolve(&t, &mut parameters, cx.namespaces, errors),
            t,
        )
    });
    let mut names = BodyNames { cx, within, locals };
    let mut checker = Checker::new(&mut names);
    let (ty, value) = match (declared, decl.initializer()) {
        (None, None) => {
            errors.push(errors::untyped_variable(name, span));
            return None;
        }
        (Some((ty, type_expr)), None) => {
            let ty = ty?;
            if ty.parameters().next().is_some() {
                errors.push(errors::no_default_value(name, &ty, &type_expr));
                return None;
            }
            let value = hir::Expr::Constant(ty.default_value());
            (ty, value)
        }
        (None, Some(written)) => {
            let value = checker.expr(&written, None, errors)?;
            (checker.ty(&value), value)
        }
        (Some((None, _)), Some(written)) => {
            // The type is invalid, but the value can still have errors of
            // its own.
            checker.expr(&written, None, errors);
            return None;
        }
        (Some((Some(ty), type_expr)), Some(written)) => {
            let expected = Expected {
                ty: ty.clone(),
                origin: Origin::Type(type_expr),
            };
            let value = checker.expr(&written, Some(&expected), errors)?;
            (ty, value)
        }
    };
    // A relation known when compiling is checked against the keys now.
    let known = match &value {
        hir::Expr::Constant(value) => Some(value),
        _ => None,
    };
    let keys = keys::keys(decl, name, &ty, known, &mut names, errors)?;
    Some((
        hir::Local {
            name: name.to_string(),
            ty,
            keys,
        },
        value,
    ))
}

/// `call name{arguments};`, of an update operator, in the body of
/// `within`, whose local variables are `locals`.
fn call(
    stmt: &ast::CallStmt,
    within: hir::OperatorId,
    cx: &Context,
    locals: &Locals,
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
            let operators: Vec<&str> = cx.operators.names().collect();
            errors.push(errors::unknown_name(name, span, "operator", &operators));
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
    bodies: &HashMap<hir::OperatorId, (Vec<hir::Local>, Vec<hir::Statement>)>,
    operators: &Operators,
    errors: &mut Vec<Diagnostic>,
) {
    let calls = |id: &hir::OperatorId| {
        bodies
            .get(id)
            .into_iter()
            .flat_map(|(_, body)| body)
            .filter_map(|s| match s {
                hir::Statement::Call(call) => Some(call.operator),
                hir::Statement::Assign(_) => None,
            })
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

/// Names in the body of `within`: its parameters and its local variables,
/// which hide variables of the same name, and the variables.
pub(crate) struct BodyNames<'a, 'b> {
    pub(crate) cx: &'a Context<'b>,
    pub(crate) within: hir::OperatorId,
    pub(crate) locals: &'a Locals,
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
                errors.push(errors::local_before_declaration(name, span, decl));
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
            hir::Expr::Local(id) => self.locals.declared[id.0]
                .local
                .as_ref()
                .expect("only valid local variables are resolved")
                .ty
                .clone(),
            _ => unreachable!("only variables, parameters and local variables are leaves"),
        }
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
