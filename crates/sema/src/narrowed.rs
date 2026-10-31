//! Narrowing: in the rest of a condition, a name a test, `x is t`, shows
//! to be of a type refers to its value as one of that type (*Type
//! Inheritance and Relational Theory*, IM Prescriptions 14 and 15). Its
//! value is then treated as one of the type, which would fail if the test
//! were false, so the guard rule computes the test first, wherever it is
//! written (see [`hir::Connective::And`]). A test that is false shows
//! nothing: a program can add subtypes to a union type, so a value that
//! isn't of one of them needn't be of any other in particular.

use std::collections::BTreeMap;

use trendy_diag::{Diagnostic, Span};
use trendy_hir as hir;
use trendy_syntax::SyntaxKind;
use trendy_syntax::ast::{self, Expr};

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
        match self.outer.resolve(name, span, errors) {
            Lookup::Found(value) => {
                let ty = Checker::new(&mut *self.outer).ty(&value);
                match self.facts.narrowed(name, &ty) {
                    Some(narrowed) => Lookup::Found(ops::treat_as(value, narrowed)),
                    None => Lookup::Found(value),
                }
            }
            other => other,
        }
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
