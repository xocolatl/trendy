//! Value blocks: `[ var x := …; x * 2 ]`, statements, then the value they
//! compute, as an expression. A block is a read-only operator's body in
//! place, Tutorial D's `WITH … : exp` (TTM chapter 5): its local variables
//! are its own, in scope in all of it, and hide no name around it but a
//! variable of the program's; it changes nothing else. A local variable
//! whose value is known when compiling is that value wherever it is used,
//! and each other one is a `Let`, as in a read-only operator's body.

use trendy_diag::{Diagnostic, Span};
use trendy_hir as hir;
use trendy_syntax::ast::{self, VarDecl};

use crate::assign::{self, Assignee, Assigning};
use crate::check::{Checker, Lookup, Names};
use crate::types::Expected;
use crate::{Namespaces, Operators, errors, locals, ops};

/// The value of `block`, whose names `outer` resolves, of the type
/// `expected` says, if it says one, or `None` after reporting why it has
/// none.
pub(crate) fn value(
    block: &ast::ValueBlock,
    outer: &mut dyn Names,
    expected: Option<&Expected>,
    errors: &mut Vec<Diagnostic>,
) -> Option<hir::Expr> {
    let statements: Vec<ast::Statement> = block.statements().collect();
    let (mut names, mut ok) = BlockNames::new(outer, &statements, errors);
    let types = names
        .type_parameters()
        .map(<[hir::TypeParameter]>::to_vec)
        .unwrap_or_default();
    let mut lets = Vec::new();
    for statement in &statements {
        // Where each value it gives a local variable is computed.
        let site = names.site(trendy_syntax::span(statement.syntax().text_range()));
        match statement {
            ast::Statement::Var(decl) => {
                let checked = locals::local(decl, &types, &mut names, errors);
                ok &= names.declare(decl, checked, site, &mut lets);
            }
            ast::Statement::Assign(stmt) => {
                let clauses: Vec<_> = stmt.clauses().collect();
                let assigned = assign::statement(&clauses, &mut names, errors);
                ok &= names.reassigned(assigned, site, &mut lets);
            }
            ast::Statement::Block(block) => {
                let assigned = assign::block(block, &mut names, errors);
                ok &= names.reassigned(assigned, site, &mut lets);
            }
            ast::Statement::Call(stmt) => {
                errors.push(errors::call_in_block(stmt));
                ok = false;
            }
            ast::Statement::If(stmt) => {
                errors.push(errors::if_in_block(stmt));
                ok = false;
            }
            ast::Statement::While(_) | ast::Statement::Loop(_) | ast::Statement::For(_) => {
                errors.push(errors::loop_in_block(statement));
                ok = false;
            }
            ast::Statement::Transaction(stmt) => {
                errors.push(errors::transaction_in_block(stmt));
                ok = false;
            }
            ast::Statement::Return(stmt) => {
                errors.push(errors::return_in_block(stmt));
                ok = false;
            }
            ast::Statement::Fail(stmt) => {
                errors.push(errors::fail_statement_for_value(stmt, "a value block"));
                ok = false;
            }
        }
    }
    let written = block.value()?;
    let site = names.site(written.span());
    let value = hir::Expr::at(
        site,
        Checker::new(&mut names).expr(&written, expected, errors)?,
    );
    ok.then(|| {
        lets.into_iter().rev().fold(value, |body, (id, value)| {
            hir::Expr::Let(id, Box::new(value), Box::new(body))
        })
    })
}

/// Names in a value block: its own local variables, then the names around
/// it, in `outer`.
struct BlockNames<'a> {
    outer: &'a mut dyn Names,
    /// Its local variables, in the order they are declared.
    scope: Vec<BlockLocal>,
    /// How many of them are declared before the statement being checked.
    reached: usize,
}

/// A value block's local variable, by the name it declares.
struct BlockLocal {
    name: String,
    decl: VarDecl,
    /// Once its declaration is checked, without errors: its id, which an
    /// assignment of a value known only when running gives a new one, the
    /// variable, and its value, if that is known when compiling, which its
    /// name then stands for.
    current: Option<(hir::LocalId, hir::Local, Option<hir::Value>)>,
}

impl<'a> BlockNames<'a> {
    /// The names in a block of `statements`, within `outer`, and whether
    /// each of its local variables has a name of its own, after reporting
    /// each that doesn't.
    fn new(
        outer: &'a mut dyn Names,
        statements: &[ast::Statement],
        errors: &mut Vec<Diagnostic>,
    ) -> (Self, bool) {
        let mut scope: Vec<BlockLocal> = Vec::new();
        let mut ok = true;
        for statement in statements {
            let ast::Statement::Var(decl) = statement else {
                continue;
            };
            let Some(ident) = decl.name().and_then(|n| n.ident()) else {
                continue;
            };
            let (name, span) = (ident.text(), trendy_syntax::span(ident.text_range()));
            if let Some(first) = scope.iter().find(|local| local.name == name) {
                errors.push(errors::local_declared_again(
                    name,
                    decl,
                    &locals::Declaration::Var(first.decl.clone()),
                    "a value block",
                ));
                ok = false;
            } else if outer.hides(name) {
                errors.push(errors::block_local_hides(name, span));
                ok = false;
            } else {
                scope.push(BlockLocal {
                    name: name.to_string(),
                    decl: decl.clone(),
                    current: None,
                });
            }
        }
        let names = BlockNames {
            outer,
            scope,
            reached: 0,
        };
        (names, ok)
    }

    /// Record the local variable `decl` declares, `checked` with its value,
    /// or `None` after its errors were reported: a value known only when
    /// running is added to `lets`, computed at `site`. Whether it is valid.
    fn declare(
        &mut self,
        decl: &VarDecl,
        checked: Option<(hir::Local, hir::Expr)>,
        site: hir::Site,
        lets: &mut Vec<(hir::LocalId, hir::Expr)>,
    ) -> bool {
        // A declaration that isn't in the scope declares a name again, or
        // one that hides another, which is reported already.
        let Some(declared) = self
            .scope
            .get_mut(self.reached)
            .filter(|declared| declared.decl == *decl)
        else {
            return false;
        };
        self.reached += 1;
        let Some((local, value)) = checked else {
            return false;
        };
        let id = self.outer.fresh_local(local.clone());
        let known = crate::ops::known_value(&value).cloned();
        if known.is_none() {
            lets.push((id, hir::Expr::at(site, value)));
        }
        declared.current = Some((id, local, known));
        true
    }

    /// Give each local variable that `assigned`, an assignment statement in
    /// the block, at `site`, assigns its new value: one known only when
    /// running is added to `lets`, under a new id, computed there. Whether
    /// the statement is valid.
    fn reassigned(
        &mut self,
        assigned: Option<hir::Statement>,
        site: hir::Site,
        lets: &mut Vec<(hir::LocalId, hir::Expr)>,
    ) -> bool {
        let Some(hir::Statement::Assign(assignments)) = assigned else {
            return false;
        };
        for assignment in assignments {
            let hir::Target::Local(id) = assignment.target else {
                unreachable!("a value block assigns only its own local variables");
            };
            let declared = self
                .scope
                .iter_mut()
                .find(|declared| declared.current.as_ref().is_some_and(|(c, ..)| *c == id))
                .expect("a value block assigns only its own local variables");
            let (_, local, _) = declared.current.take().expect("just found");
            let known = crate::ops::known_value(&assignment.value).cloned();
            declared.current = Some(if known.is_some() {
                (id, local, known)
            } else {
                let new = self.outer.fresh_local(local.clone());
                lets.push((new, hir::Expr::at(site, assignment.value)));
                (new, local, None)
            });
        }
        true
    }

    /// The local variable `name` of the block's, if it has one: its place
    /// in the scope, and its declaration.
    fn own(&self, name: &str) -> Option<(usize, &BlockLocal)> {
        self.scope
            .iter()
            .enumerate()
            .find(|(_, local)| local.name == name)
    }

    /// The names of the block's local variables declared before here.
    fn declared(&self) -> impl Iterator<Item = &str> {
        self.scope[..self.reached]
            .iter()
            .filter(|local| local.current.is_some())
            .map(|local| local.name.as_str())
    }
}

impl Names for BlockNames<'_> {
    fn namespaces(&self) -> &Namespaces {
        self.outer.namespaces()
    }

    fn site(&self, at: Span) -> hir::Site {
        self.outer.site(at)
    }

    fn resolve(&mut self, name: &str, span: Span, errors: &mut Vec<Diagnostic>) -> Lookup {
        let Some((index, local)) = self.own(name) else {
            return self.outer.resolve(name, span, errors);
        };
        if index >= self.reached {
            errors.push(errors::local_before_declaration(
                name,
                span,
                &local.decl,
                false,
            ));
            return Lookup::Reported;
        }
        match &local.current {
            // Of its declared type, which can be a supertype of its
            // value's.
            Some((_, local, Some(value))) => {
                Lookup::Found(hir::Expr::Constant(value.clone()).treated(&local.ty))
            }
            Some((id, _, None)) => Lookup::Found(hir::Expr::Local(*id)),
            // Its declaration's errors are already reported.
            None => Lookup::Reported,
        }
    }

    fn candidates(&self) -> Vec<String> {
        self.declared()
            .map(str::to_string)
            .chain(self.outer.candidates())
            .collect()
    }

    fn leaf_type(&self, leaf: &hir::Expr) -> hir::Type {
        self.outer.leaf_type(leaf)
    }

    fn fresh_local(&mut self, local: hir::Local) -> hir::LocalId {
        self.outer.fresh_local(local)
    }

    fn hides(&self, name: &str) -> bool {
        self.own(name).is_some() || self.outer.hides(name)
    }

    fn tuple_at_hand(&self) -> Option<&hir::Heading> {
        self.outer.tuple_at_hand()
    }

    fn unnumbered(&self) -> bool {
        self.outer.unnumbered()
    }

    fn type_parameters(&self) -> Option<&[hir::TypeParameter]> {
        self.outer.type_parameters()
    }

    fn operators(&self) -> &Operators {
        self.outer.operators()
    }

    fn checked(&mut self, id: hir::OperatorId, at: Span, errors: &mut Vec<Diagnostic>) -> bool {
        self.outer.checked(id, at, errors)
    }

    fn bindings(&self) -> ops::Bindings<'_> {
        self.outer.bindings()
    }

    fn narrowing_ended(&self, name: &str) -> Option<Span> {
        if self.own(name).is_some() {
            return None;
        }
        self.outer.narrowing_ended(name)
    }
}

impl Assigning for BlockNames<'_> {
    fn assigned(
        &mut self,
        name: &str,
        span: Span,
        errors: &mut Vec<Diagnostic>,
    ) -> Option<Assignee> {
        if let Some((index, local)) = self.own(name) {
            if index >= self.reached {
                errors.push(errors::local_before_declaration(
                    name,
                    span,
                    &local.decl,
                    false,
                ));
                return None;
            }
            let (id, variable, known) = local.current.as_ref()?;
            let assignee = Assignee::local(*id, variable, &local.decl);
            return Some(match known {
                // No `Let` binds it: its name stands for its value.
                Some(value) => Assignee {
                    current: hir::Expr::Constant(value.clone()).treated(&variable.ty),
                    ..assignee
                },
                None => assignee,
            });
        }
        if self.outer.has_value(name) {
            errors.push(errors::assigned_outside_block(name, span));
        } else {
            let variables: Vec<&str> = self.declared().collect();
            errors.push(errors::unknown_name(name, span, "variable", &variables));
        }
        None
    }
}
