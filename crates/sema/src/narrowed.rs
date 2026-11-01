//! Narrowing: in the rest of a condition, a name a test, `x is t`, shows
//! to be of a type refers to its value as one of that type (*Type
//! Inheritance and Relational Theory*, IM Prescriptions 14 and 15). Its
//! value is then treated as one of the type, which would fail if the test
//! were false, so the guard rule computes the test first, wherever it is
//! written (see [`hir::Connective::And`]). A test that is false shows
//! nothing: a program can add subtypes to a union type, so a value that
//! isn't of one of them needn't be of any other in particular.
//!
//! In an update operator's body, an `if` statement's branches, and a
//! `while` loop's block, are narrowed as an `if` expression's branches
//! are, until a statement that may assign the name (see [`Assigns`]).

use std::collections::BTreeMap;

use trendy_diag::{Diagnostic, Span};
use trendy_hir as hir;
use trendy_syntax::SyntaxKind;
use trendy_syntax::ast::{self, AstNode, Expr};

use crate::Namespaces;
use crate::check::{Checker, Lookup, Names};
use crate::operators::Operators;
use crate::ops;
use crate::types;

/// What the tests of a condition, `x is t`, show of the types of the values
/// names refer to, by name, if it is true, or if it is false: the types
/// each is tested to be of.
#[derive(Debug, Clone, Default)]
pub(crate) struct Facts(BTreeMap<String, Vec<hir::Type>>);

impl Facts {
    /// What `condition` shows if it is `truth`: `x is t`, if true, that
    /// `x` is a `t`; `not c` what `c` does if it is the other; an `and`
    /// what each of its operands does, if it is true; an `or` what each
    /// does, if it is false, as `a implies b`, which is `not a or b`, does.
    /// `checker` resolves the types tested.
    pub(crate) fn of(condition: &Expr, truth: bool, checker: &Checker) -> Facts {
        let mut facts = Facts::default();
        facts.gather(condition, truth, checker);
        facts
    }

    fn gather(&mut self, condition: &Expr, truth: bool, checker: &Checker) {
        match condition {
            Expr::Paren(paren) => {
                if let Some(inner) = paren.inner() {
                    self.gather(&inner, truth, checker);
                }
            }
            Expr::Unary(unary)
                if unary
                    .operator()
                    .is_some_and(|t| t.kind() == SyntaxKind::NotKw) =>
            {
                if let Some(operand) = unary.operand() {
                    self.gather(&operand, !truth, checker);
                }
            }
            // That a value isn't of one subtype doesn't show which other it
            // is of, as a program can add more.
            Expr::Is(test) if truth => {
                if let Some((name, ty)) = tested(test, checker) {
                    self.0.entry(name).or_default().push(ty);
                }
            }
            _ => {
                let Some(ops::Connecting { token, operands }) = ops::Connecting::of(condition)
                else {
                    return;
                };
                let boolean = hir::Type::Scalar(hir::Scalar::Boolean);
                let Some(ops::Resolved::Connective {
                    connective,
                    not_left,
                }) = ops::Operator::of(&token).and_then(|op| op.resolve(&boolean))
                else {
                    return;
                };
                for (i, operand) in operands.iter().enumerate() {
                    let Some(operand) = operand else {
                        continue;
                    };
                    // `a implies b`, `not a or b`: if false, `a` is true.
                    let negated = i == 0 && not_left;
                    match (connective, truth) {
                        (hir::Connective::And, true) => self.gather(operand, true, checker),
                        (hir::Connective::Or, false) => self.gather(operand, negated, checker),
                        _ => {}
                    }
                }
            }
        }
    }

    /// These and `other`'s.
    pub(crate) fn and(&self, other: &Facts) -> Facts {
        let mut facts = self.clone();
        for (name, more) in &other.0 {
            facts
                .0
                .entry(name.clone())
                .or_default()
                .extend(more.iter().cloned());
        }
        facts
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The names these show something of.
    pub(crate) fn names(&self) -> impl Iterator<Item = &str> {
        self.0.keys().map(String::as_str)
    }

    /// These, but nothing of `name`, whose value may have changed: whether
    /// they showed something of it.
    pub(crate) fn forget(&mut self, name: &str) -> bool {
        self.0.remove(name).is_some()
    }

    /// These, after an `update` of `name`, whose value is then selected
    /// again of the type it is updated as: of each type they show it to be
    /// of, defined as a subtype by a constraint, which the new value may no
    /// longer be of, the type its components are its supertype's of (see
    /// [`updated_as`]).
    pub(crate) fn updated(&mut self, name: &str) {
        if let Some(types) = self.0.get_mut(name) {
            for ty in types {
                while let hir::Type::Defined(defined) = &*ty
                    && let Some(specialization) = &defined.specialization
                {
                    *ty = hir::Type::Defined(specialization.supertype.clone());
                }
            }
        }
    }

    /// The type that what `name` refers to, a value of type `ty`, is shown
    /// to be of, if it is narrower: the most specific subtype of `ty` it is
    /// tested to be of.
    pub(crate) fn narrowed(&self, name: &str, ty: &hir::Type) -> Option<hir::Type> {
        let mut narrowed = ty.clone();
        for tested in self.0.get(name)? {
            if tested.is_subtype_of(&narrowed) {
                narrowed = tested.clone();
            }
        }
        (narrowed != *ty).then_some(narrowed)
    }

    /// `heading`, with each attribute these show to be of a narrower type
    /// of it.
    pub(crate) fn heading(&self, heading: &hir::Heading) -> hir::Heading {
        hir::Heading::new(
            heading
                .attributes()
                .map(|(name, ty)| {
                    let narrowed = self.narrowed(name, ty);
                    (name.to_string(), narrowed.unwrap_or_else(|| ty.clone()))
                })
                .collect(),
        )
    }
}

/// The name `test` tests, if it tests a name's value, and the type it tests
/// for, if that type is valid, which checking the test reports otherwise.
fn tested(test: &ast::IsExpr, checker: &Checker) -> Option<(String, hir::Type)> {
    let mut operand = test.operand()?;
    while let Expr::Paren(paren) = &operand {
        operand = paren.inner()?;
    }
    let Expr::Name(name) = operand else {
        return None;
    };
    let name = name.ident()?.text().to_string();
    let mut parameters = checker.type_parameters();
    let ty = types::resolve(
        &test.type_expr()?,
        &mut parameters,
        checker.namespaces(),
        &mut Vec::new(),
    )?;
    Some((name, ty))
}

/// `lookup`, what `name` refers to, as `facts` narrow it: a value found,
/// treated as one of the type they show it to be of, if that is narrower
/// than its own, which `names` gives.
pub(crate) fn narrow(facts: &Facts, name: &str, lookup: Lookup, names: &mut dyn Names) -> Lookup {
    match lookup {
        Lookup::Found(value) => {
            let ty = Checker::new(names).ty(&value);
            match facts.narrowed(name, &ty) {
                Some(narrowed) => Lookup::Found(ops::treat_as(value, narrowed)),
                None => Lookup::Found(value),
            }
        }
        other => other,
    }
}

/// The type an `update` of a variable of type `declared`, whose value a
/// test shows to be of `narrowed`, selects its value again of: `narrowed`,
/// or, of a type defined as a subtype by a constraint, whose components
/// are its supertype's, that supertype, as long as it is `declared` or one
/// of its subtypes. A new value of the supertype may be of the subtype
/// again, or not (*Type Inheritance and Relational Theory*, chapter 8,
/// generalization by constraint).
pub(crate) fn updated_as(narrowed: &hir::Type, declared: &hir::Type) -> hir::Type {
    let mut ty = narrowed.clone();
    while let hir::Type::Defined(defined) = &ty
        && let Some(specialization) = &defined.specialization
    {
        let supertype = hir::Type::Defined(specialization.supertype.clone());
        if !supertype.is_subtype_of(declared) {
            break;
        }
        ty = supertype;
    }
    ty
}

/// What a statement may assign, by its syntax, in it and in the
/// statements inside it, which ends what is known of the names' values:
/// each variable an assignment's target names, or a `var` or `out`
/// argument, with where the first is, and the first call, of an update
/// operator, which may assign any of the program's variables.
#[derive(Debug, Default)]
pub(crate) struct Assigns {
    pub(crate) names: BTreeMap<String, Span>,
    pub(crate) call: Option<Span>,
    /// Each variable an `update` changes, whose narrowing it keeps, of the
    /// type it is updated as (see [`Facts::updated`]).
    pub(crate) updated: Vec<String>,
}

impl Assigns {
    pub(crate) fn of(statement: &ast::Statement) -> Assigns {
        let mut assigns = Assigns::default();
        assigns.gather(statement);
        assigns
    }

    fn gather(&mut self, statement: &ast::Statement) {
        match statement {
            ast::Statement::Assign(stmt) => self.clauses(stmt),
            ast::Statement::Block(block) => {
                for stmt in block.statements() {
                    self.clauses(&stmt);
                }
            }
            ast::Statement::Call(stmt) => {
                self.call.get_or_insert(stmt.span());
                let arguments = stmt.arguments();
                let modes = arguments.iter().flat_map(|a| {
                    a.named()
                        .filter_map(|arg| Some((arg.mode()?, arg.value()?)))
                        .collect::<Vec<_>>()
                });
                for (mode, value) in modes {
                    if mode.says() != ast::Said::Const
                        && let ast::Expr::Name(name) = value
                        && let Some(ident) = name.ident()
                    {
                        self.named(ident.text(), stmt.span());
                    }
                }
            }
            ast::Statement::If(stmt) => {
                self.body_of(stmt.then_block());
                match stmt.else_branch() {
                    Some(ast::ElseBranch::Block(body)) => self.body(&body),
                    Some(ast::ElseBranch::If(inner)) => self.gather(&ast::Statement::If(inner)),
                    None => {}
                }
            }
            ast::Statement::While(stmt) => self.body_of(stmt.body()),
            ast::Statement::Loop(stmt) => self.body_of(stmt.body()),
            ast::Statement::For(stmt) => self.body_of(stmt.body()),
            ast::Statement::Transaction(stmt) => {
                self.body_of(stmt.body());
                if let Some(ident) = stmt
                    .collecting()
                    .and_then(|c| c.variable())
                    .and_then(|v| v.ident())
                {
                    self.named(ident.text(), stmt.span());
                }
            }
            ast::Statement::Var(_)
            | ast::Statement::Return(_)
            | ast::Statement::Fail(_)
            | ast::Statement::Warn(_) => {}
        }
    }

    fn body_of(&mut self, body: Option<ast::Body>) {
        if let Some(body) = body {
            self.body(&body);
        }
    }

    fn body(&mut self, body: &ast::Body) {
        for statement in body.statements() {
            self.gather(&statement);
        }
    }

    /// The variables an assignment statement's clauses assign, but those an
    /// `update` changes: it gives new values to the components of the type
    /// it is updated as, so the value is still one of it.
    fn clauses(&mut self, stmt: &ast::AssignStmt) {
        for clause in stmt.clauses() {
            let target = clause.target().and_then(|t| crate::assign::target_path(&t));
            let Some(ident) = target.and_then(|(name, _)| name.ident()) else {
                continue;
            };
            if matches!(clause, ast::Clause::Update(_)) {
                self.updated.push(ident.text().to_string());
            } else {
                self.named(ident.text(), clause.span());
            }
        }
    }

    fn named(&mut self, name: &str, at: Span) {
        self.names.entry(name.to_string()).or_insert(at);
    }

    /// The names it assigns, and, if it calls an update operator, those of
    /// `names` that `is_global` says are the program's variables.
    pub(crate) fn of_names<'n>(
        &self,
        names: impl Iterator<Item = &'n str>,
        is_global: impl Fn(&str) -> bool,
    ) -> BTreeMap<String, Span> {
        let mut assigned = self.names.clone();
        if let Some(call) = self.call {
            for name in names.filter(|n| is_global(n)) {
                assigned.entry(name.to_string()).or_insert(call);
            }
        }
        assigned
    }
}

/// The names in `assigned` that `facts` show something of, which they
/// then don't, each with where it may be assigned, into `ended`.
pub(crate) fn forget(
    facts: &mut Facts,
    assigned: &BTreeMap<String, Span>,
    ended: &mut BTreeMap<String, Span>,
) {
    for (name, at) in assigned {
        if facts.forget(name) {
            ended.insert(name.clone(), *at);
        }
    }
}

/// Names, `outer`'s, of which those `facts` narrow refer to their values as
/// ones of the types the facts show them to be of, each treated as one
/// (`hir::Expr::TreatAs`).
pub(crate) struct Narrowed<'a> {
    pub(crate) outer: &'a mut dyn Names,
    pub(crate) facts: &'a Facts,
}

impl Names for Narrowed<'_> {
    fn namespaces(&self) -> &Namespaces {
        self.outer.namespaces()
    }

    fn site(&self, at: Span) -> hir::Site {
        self.outer.site(at)
    }

    fn resolve(&mut self, name: &str, span: Span, errors: &mut Vec<Diagnostic>) -> Lookup {
        let lookup = self.outer.resolve(name, span, errors);
        narrow(self.facts, name, lookup, &mut *self.outer)
    }

    fn resolve_variable(&mut self, name: &str, span: Span, errors: &mut Vec<Diagnostic>) -> Lookup {
        self.outer.resolve_variable(name, span, errors)
    }

    fn narrowing_ended(&self, name: &str) -> Option<Span> {
        self.outer.narrowing_ended(name)
    }

    fn candidates(&self) -> Vec<String> {
        self.outer.candidates()
    }

    fn leaf_type(&self, leaf: &hir::Expr) -> hir::Type {
        self.outer.leaf_type(leaf)
    }

    fn fresh_local(&mut self, local: hir::Local) -> hir::LocalId {
        self.outer.fresh_local(local)
    }

    fn hides(&self, name: &str) -> bool {
        self.outer.hides(name)
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
}

/// Names, `outer`'s, each referring to a variable as it is declared,
/// whatever a test shows of its value, as a `var` or `out` argument does
/// (see [`Names::resolve_variable`]).
pub(crate) struct Unnarrowed<'a> {
    pub(crate) outer: &'a mut dyn Names,
}

impl Names for Unnarrowed<'_> {
    fn namespaces(&self) -> &Namespaces {
        self.outer.namespaces()
    }

    fn site(&self, at: Span) -> hir::Site {
        self.outer.site(at)
    }

    fn resolve(&mut self, name: &str, span: Span, errors: &mut Vec<Diagnostic>) -> Lookup {
        self.outer.resolve_variable(name, span, errors)
    }

    fn narrowing_ended(&self, name: &str) -> Option<Span> {
        self.outer.narrowing_ended(name)
    }

    fn candidates(&self) -> Vec<String> {
        self.outer.candidates()
    }

    fn leaf_type(&self, leaf: &hir::Expr) -> hir::Type {
        self.outer.leaf_type(leaf)
    }

    fn fresh_local(&mut self, local: hir::Local) -> hir::LocalId {
        self.outer.fresh_local(local)
    }

    fn hides(&self, name: &str) -> bool {
        self.outer.hides(name)
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
}
