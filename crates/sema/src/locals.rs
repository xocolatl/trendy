//! Operators' local variables (TTM RM Prescription 3): declared in a
//! block, purely local to the operator, and in scope in all of the block,
//! so that a name means one thing there, though one has no value before
//! its declaration.

use trendy_diag::Diagnostic;
use trendy_hir as hir;
use trendy_syntax::ast::{self, AstNode, VarDecl};

use crate::check::{Checker, Names};
use crate::operators::Signature;
use crate::types::{self, Expected, Origin, TypeParameters};
use crate::{errors, keys};

/// The local variables of a body: those of each block the statement being
/// checked is in, and of the blocks that have ended.
#[derive(Default)]
pub(crate) struct Locals {
    /// The blocks the statement being checked is in, outermost first.
    scopes: Vec<Scope>,
    /// The declarations of the blocks that have ended, whose names are out
    /// of scope.
    ended: Vec<Declared>,
    /// Every local variable, by id: one for each declaration, and, in a
    /// read-only operator, one more for each assignment, which gives its
    /// variable a new value from there on.
    all: Vec<hir::Local>,
}

/// A block's local variables: each declared in it, in order, and how many
/// of their declarations come before the statement being checked.
#[derive(Default)]
struct Scope {
    declared: Vec<Declared>,
    reached: usize,
}

/// What declares a local variable.
#[derive(Clone)]
pub(crate) enum Declaration {
    /// `var name …;`.
    Var(VarDecl),
    /// A `for` loop, of whose array's elements its variable, `name`, takes
    /// each in turn.
    Loop(ast::Name),
}

impl Declaration {
    /// Where the variable's name is written.
    pub(crate) fn name_span(&self) -> trendy_diag::Span {
        match self {
            Declaration::Var(decl) => decl.name().map_or_else(|| decl.span(), |n| n.span()),
            Declaration::Loop(name) => name.span(),
        }
    }
}

/// A local variable's declaration, by the name it declares.
struct Declared {
    name: String,
    decl: Declaration,
    /// The local variable its name refers to here, once its declaration is
    /// checked: `None` before then, or after reporting its errors.
    current: Option<hir::LocalId>,
}

/// What a name is among a body's local variables.
pub(crate) enum Local<'a> {
    /// A local variable declared before here, by the id its name refers to.
    Declared(hir::LocalId, &'a hir::Local, &'a Declaration),
    /// One declared after here, or here, which has no value yet.
    Later(&'a VarDecl),
    /// One declared in a block that has ended, out of scope here.
    Ended(&'a Declaration),
    /// One whose declaration has errors, already reported.
    Invalid,
}

impl Locals {
    /// The local variable named `name`, if there is one.
    pub(crate) fn find(&self, name: &str) -> Option<Local<'_>> {
        for scope in self.scopes.iter().rev() {
            let found = scope
                .declared
                .iter()
                .enumerate()
                .find(|(_, declared)| declared.name == name);
            if let Some((index, declared)) = found {
                return Some(match (&declared.decl, declared.current) {
                    (Declaration::Var(decl), _) if index >= scope.reached => Local::Later(decl),
                    (_, Some(id)) => Local::Declared(id, &self.all[id.0], &declared.decl),
                    (_, None) => Local::Invalid,
                });
            }
        }
        self.ended
            .iter()
            .rev()
            .find(|declared| declared.name == name)
            .map(|declared| Local::Ended(&declared.decl))
    }

    /// The names of the local variables in scope and declared before here.
    pub(crate) fn names(&self) -> impl Iterator<Item = &str> {
        self.scopes
            .iter()
            .flat_map(|scope| &scope.declared[..scope.reached])
            .filter(|declared| declared.current.is_some())
            .map(|declared| declared.name.as_str())
    }

    /// The local variable `id`.
    pub(crate) fn get(&self, id: hir::LocalId) -> &hir::Local {
        &self.all[id.0]
    }

    /// The id of `local`, the local variable `decl` declares, now checked,
    /// which its name refers to from here on, or `None` if it has errors,
    /// or `decl` declares one again, both already reported.
    pub(crate) fn declare(
        &mut self,
        decl: &VarDecl,
        local: Option<hir::Local>,
    ) -> Option<hir::LocalId> {
        let scope = self.scopes.last_mut()?;
        let declared = scope.declared.get_mut(scope.reached)?;
        if !matches!(&declared.decl, Declaration::Var(declaring) if declaring == decl) {
            return None;
        }
        scope.reached += 1;
        let id = hir::LocalId(self.all.len());
        self.all.push(local?);
        declared.current = Some(id);
        Some(id)
    }

    /// The id of `local`, a value block's local variable, which its block
    /// keeps track of itself.
    pub(crate) fn fresh(&mut self, local: hir::Local) -> hir::LocalId {
        let id = hir::LocalId(self.all.len());
        self.all.push(local);
        id
    }

    /// A new id for the local variable `id`, which an assignment in a
    /// read-only operator gives a new value: its name refers to it from
    /// here on.
    pub(crate) fn reassigned(&mut self, id: hir::LocalId) -> hir::LocalId {
        let new = hir::LocalId(self.all.len());
        self.all.push(self.all[id.0].clone());
        let declared = self
            .scopes
            .iter_mut()
            .flat_map(|scope| &mut scope.declared)
            .find(|d| d.current == Some(id));
        if let Some(declared) = declared {
            declared.current = Some(new);
        }
        new
    }

    /// Enter a block, of `statements`, of the operator `op`, whose local
    /// variables are in scope in all of it. Whether each has a name of its
    /// own, after reporting each that doesn't: an operator's names, its
    /// parameters' and its local variables', each mean one thing wherever
    /// they are in scope, so a block's hide no other, though they hide the
    /// program's variables.
    pub(crate) fn enter(
        &mut self,
        statements: impl Iterator<Item = ast::Statement>,
        op: &Signature,
        errors: &mut Vec<Diagnostic>,
    ) -> bool {
        self.enter_scope(Scope::default(), statements, op, errors)
    }

    /// Enter the block of a `for` loop, of `statements`, of the operator
    /// `op`, whose variable is `name`, declared by the loop: `local`, in
    /// scope in all of the block. Its id, if its name is its own, as for
    /// [`Locals::enter`], and whether each of the block's local variables
    /// has one.
    pub(crate) fn enter_loop(
        &mut self,
        name: &ast::Name,
        local: hir::Local,
        statements: impl Iterator<Item = ast::Statement>,
        op: &Signature,
        errors: &mut Vec<Diagnostic>,
    ) -> (Option<hir::LocalId>, bool) {
        let mut scope = Scope::default();
        let span = name.span();
        let first = self
            .scopes
            .iter()
            .flat_map(|scope| &scope.declared)
            .find(|other| other.name == local.name);
        let id = if let Some(first) = first {
            errors.push(errors::loop_variable_declared_again(
                &local.name,
                span,
                &first.decl,
                &op.name,
            ));
            None
        } else if op.is_parameter(&local.name) {
            errors.push(errors::local_named_as_parameter(
                &local.name,
                span,
                None,
                &op.name,
            ));
            None
        } else {
            let id = hir::LocalId(self.all.len());
            scope.declared.push(Declared {
                name: local.name.clone(),
                decl: Declaration::Loop(name.clone()),
                current: Some(id),
            });
            scope.reached = 1;
            self.all.push(local);
            Some(id)
        };
        let ok = self.enter_scope(scope, statements, op, errors);
        (id, ok && id.is_some())
    }

    /// Enter a block, `scope` holding any local variable it declares
    /// before its statements, of `statements`, as for [`Locals::enter`].
    fn enter_scope(
        &mut self,
        mut scope: Scope,
        statements: impl Iterator<Item = ast::Statement>,
        op: &Signature,
        errors: &mut Vec<Diagnostic>,
    ) -> bool {
        let mut ok = true;
        for stmt in statements {
            let ast::Statement::Var(decl) = stmt else {
                continue;
            };
            let Some(ident) = decl.name().and_then(|n| n.ident()) else {
                continue;
            };
            let (name, span) = (ident.text(), trendy_syntax::span(ident.text_range()));
            let first = scope
                .declared
                .iter()
                .chain(self.scopes.iter().flat_map(|scope| &scope.declared))
                .find(|other| other.name == name);
            if let Some(first) = first {
                errors.push(errors::local_declared_again(
                    name,
                    &decl,
                    &first.decl,
                    &format!("`{}`", op.name),
                ));
                ok = false;
            } else if op.is_parameter(name) {
                let parameter =
                    op.decl
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
                scope.declared.push(Declared {
                    name: name.to_string(),
                    decl: Declaration::Var(decl),
                    current: None,
                });
            }
        }
        self.scopes.push(scope);
        ok
    }

    /// Leave the innermost block: the local variables declared in it,
    /// whose names are out of scope from here on.
    pub(crate) fn exit(&mut self) -> Vec<hir::LocalId> {
        let scope = self.scopes.pop().expect("a block was entered");
        let ids = scope
            .declared
            .iter()
            .filter_map(|declared| declared.current)
            .collect();
        self.ended.extend(scope.declared);
        ids
    }

    /// Every local variable, by id.
    pub(crate) fn finish(self) -> Vec<hir::Local> {
        self.all
    }
}

/// The local variables of `body`, the body of the operator `op`, its block
/// entered, and whether each has a name of its own (see
/// [`Locals::enter`]).
pub(crate) fn declarations(
    body: &ast::Body,
    op: &Signature,
    errors: &mut Vec<Diagnostic>,
) -> (Locals, bool) {
    let mut locals = Locals::default();
    let ok = locals.enter(body.statements(), op, errors);
    (locals, ok)
}

/// The local variable `decl` declares in the body of an operator with the
/// type parameters `types`, or in a value block, whose names `names`
/// resolves, and its initial value: the one given, or else its type's
/// default, which a type parameter's type has none of. It is checked as a
/// program variable's declaration is, but with a value known only when
/// running.
pub(crate) fn local(
    decl: &VarDecl,
    types: &[hir::TypeParameter],
    names: &mut dyn Names,
    errors: &mut Vec<Diagnostic>,
) -> Option<(hir::Local, hir::Expr)> {
    let ident = decl.name()?.ident()?;
    let (name, span) = (ident.text(), trendy_syntax::span(ident.text_range()));
    let declared = decl.type_expr().map(|t| {
        let mut parameters = TypeParameters::Fixed(types);
        (
            types::resolve(&t, &mut parameters, names.namespaces(), errors),
            t,
        )
    });
    let mut checker = Checker::new(&mut *names);
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
    let keys = keys::keys(decl, name, &ty, known, names, errors)?;
    Some((
        hir::Local {
            name: name.to_string(),
            ty,
            keys,
        },
        value,
    ))
}
