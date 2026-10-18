//! Operators' local variables (TTM RM Prescription 3): declared in a body,
//! purely local to the operator, and in scope in all of the body, so that a
//! name means one thing there, though one has no value before its
//! declaration.

use trendy_diag::Diagnostic;
use trendy_hir as hir;
use trendy_syntax::ast::{self, AstNode, VarDecl};

use crate::check::{Checker, Names};
use crate::operators::Signature;
use crate::types::{self, Expected, Origin, TypeParameters};
use crate::{Namespaces, errors, keys};

/// The local variables of a body: each one declared in it, in order, and
/// how many of their declarations come before the statement being checked.
#[derive(Default)]
pub(crate) struct Locals {
    declared: Vec<Declared>,
    /// Every local variable, by id: one for each declaration, and, in a
    /// read-only operator, one more for each assignment, which gives its
    /// variable a new value from there on.
    all: Vec<hir::Local>,
    /// How many declarations come before the statement being checked.
    reached: usize,
}

/// A local variable's declaration, by the name it declares.
struct Declared {
    name: String,
    decl: VarDecl,
    /// The local variable its name refers to here, once its declaration is
    /// checked: `None` before then, or after reporting its errors.
    current: Option<hir::LocalId>,
}

/// What a name is among a body's local variables.
pub(crate) enum Local<'a> {
    /// A local variable declared before here, by the id its name refers to.
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
        Some(match declared.current {
            _ if index >= self.reached => Local::Later(&declared.decl),
            Some(id) => Local::Declared(id, &self.all[id.0], &declared.decl),
            None => Local::Invalid,
        })
    }

    /// The names of the local variables declared before here.
    pub(crate) fn names(&self) -> impl Iterator<Item = &str> {
        self.declared[..self.reached]
            .iter()
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
        let declared = self.declared.get_mut(self.reached)?;
        if declared.decl != *decl {
            return None;
        }
        self.reached += 1;
        let id = hir::LocalId(self.all.len());
        self.all.push(local?);
        declared.current = Some(id);
        Some(id)
    }

    /// A new id for the local variable `id`, which an assignment in a
    /// read-only operator gives a new value: its name refers to it from
    /// here on.
    pub(crate) fn reassigned(&mut self, id: hir::LocalId) -> hir::LocalId {
        let new = hir::LocalId(self.all.len());
        self.all.push(self.all[id.0].clone());
        if let Some(declared) = self.declared.iter_mut().find(|d| d.current == Some(id)) {
            declared.current = Some(new);
        }
        new
    }

    /// Every local variable, by id.
    pub(crate) fn finish(self) -> Vec<hir::Local> {
        self.all
    }
}

/// The local variables that `body`, of the operator `op`, declares, and
/// whether each has a name of its own, after reporting each that doesn't:
/// an operator's names, its parameters' and its local variables', each mean
/// one thing in its body, and hide the program's variables.
pub(crate) fn declarations(
    body: &ast::Body,
    op: &Signature,
    errors: &mut Vec<Diagnostic>,
) -> (Locals, bool) {
    let mut locals = Locals::default();
    let mut ok = true;
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
                current: None,
            });
        }
    }
    (locals, ok)
}

/// The local variable `decl` declares in the body of the operator `op`,
/// whose names `names` resolves, and its initial value: the one given, or
/// else its type's default, which a type parameter's type has none of. It
/// is checked as a program variable's declaration is, but with a value
/// known only when running.
pub(crate) fn local(
    decl: &VarDecl,
    op: &Signature,
    namespaces: &Namespaces,
    names: &mut dyn Names,
    errors: &mut Vec<Diagnostic>,
) -> Option<(hir::Local, hir::Expr)> {
    let ident = decl.name()?.ident()?;
    let (name, span) = (ident.text(), trendy_syntax::span(ident.text_range()));
    let declared = decl.type_expr().map(|t| {
        let mut parameters = TypeParameters::Fixed(&op.types);
        (types::resolve(&t, &mut parameters, namespaces, errors), t)
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
