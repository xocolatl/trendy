//! A unit's constraints on its variables (TTM RM Prescription 23): each a
//! condition, named, that holds whenever a statement ends. The variables'
//! initial values are known when compiling, so a constraint they break is
//! reported then, with what breaks it where its form says.

use std::collections::BTreeMap;

use trendy_diag::{Diagnostic, Span};
use trendy_hir as hir;
use trendy_syntax::SyntaxKind;
use trendy_syntax::ast::{self, AstNode, Expr};

use crate::body::Context;
use crate::check::{Checker, Lookup, Names};
use crate::locals::Locals;
use crate::operators::Operators;
use crate::types::{Expected, Origin};
use crate::{Files, Namespaces, errors, ops, sections};

/// Every constraint on the variables of `files`, a database's aside (see
/// [`crate::databases`]), checked: its condition, a boolean, mentions
/// one or more variables, and their initial values satisfy it. The valid
/// ones, after reporting the rest.
pub(crate) fn check(
    files: &Files,
    cx: &Context,
    errors: &mut Vec<Diagnostic>,
) -> Vec<hir::VariableConstraint> {
    files
        .constraints()
        .filter(|decl| decl.part_of().is_none())
        .filter_map(|decl| constraint(&decl, cx, errors))
        .collect()
}

/// The constraint `decl`, unless it has errors, which are reported, or is
/// a duplicate, already reported, whose errors of its own are.
fn constraint(
    decl: &ast::ConstraintDecl,
    cx: &Context,
    errors: &mut Vec<Diagnostic>,
) -> Option<hir::VariableConstraint> {
    let ident = decl.name()?.ident()?;
    let written = decl.condition()?;
    let boolean = Expected {
        ty: hir::Type::Scalar(hir::Scalar::Boolean),
        origin: Origin::Operator(decl.keyword()?),
    };
    let mut names = ConditionNames::new(cx, false);
    Checker::new(&mut names).expr(&written, Some(&boolean), errors)?;
    // Of the initial values, checked again, so that a part that fails is
    // reported where it is, as in an initial value.
    let mut initial = ConditionNames::new(cx, true);
    let holds = match initial.value(&written, errors) {
        Initially::Known(value) => Some(value == hir::Value::Boolean(true)),
        Initially::Unknown => None,
        Initially::Reported => return None,
    };
    if names.mentioned.is_empty() {
        let file = ast::SourceFile::cast(decl.syntax().parent()?)?;
        let removal = sections::removal(&file, decl.syntax());
        errors.push(errors::variable_free_constraint(
            ident.text(),
            &written,
            holds,
            removal,
        ));
        return None;
    }
    if holds == Some(false) {
        let breaches = initial.breaches(&written);
        let mentioned: Vec<&str> = names.mentioned.keys().map(String::as_str).collect();
        errors.push(errors::constraint_broken_initially(
            ident.text(),
            &written,
            breaches,
            &mentioned,
        ));
        return None;
    }
    if !cx.namespaces.is_first_constraint(&ident) {
        return None;
    }
    let parts = parts(&written, &boolean, || ConditionNames::new(cx, false));
    let unit = cx
        .namespaces
        .unit_at(trendy_syntax::span(ident.text_range()));
    Some(hir::VariableConstraint {
        name: cx.namespaces.constraint_identity(ident.text(), unit),
        variables: names.mentioned.values().copied().collect(),
        parts,
        written: written.syntax().text().to_string(),
        site: cx.sites.at(decl.span()),
        checked_at_start: holds.is_none(),
    })
}

/// What a condition on the variables is of their initial values, when compiling.
enum Initially {
    /// Its value.
    Known(hir::Value),
    /// What only the program can compute, as it reads a constant the compiler can't
    /// compute, or runs statements.
    Unknown,
    /// Nothing, as it has errors, already reported, such as a part that fails.
    Reported,
}

/// The names in a constraint's condition, as [`parts`] checks it, which
/// give the local variables of the value blocks in it once they are
/// checked.
pub(crate) trait PartNames: Names {
    /// The local variables of the value blocks in what was checked.
    fn locals(self) -> Vec<hir::Local>;
}

/// The parts of the constraint's condition `written`, already checked as
/// `boolean`, each checked with names `fresh` makes: those that can't fail
/// first, as an `and` computes them, each of a kind in the order it is
/// written.
pub(crate) fn parts<N: PartNames>(
    written: &Expr,
    boolean: &Expected,
    fresh: impl Fn() -> N,
) -> Vec<hir::ConstraintPart> {
    let mut parts: Vec<hir::ConstraintPart> = conjuncts(written)
        .iter()
        .filter_map(|conjunct| part(conjunct, boolean, fresh()))
        .collect();
    parts.sort_by_key(|part| part.condition.can_fail());
    parts
}

/// The part of a constraint's condition `written` is, a conjunct of it,
/// with the tuples that make it false where its form says which, checked
/// with `names`. It is part of a condition already checked, so it has no
/// errors of its own.
fn part<N: PartNames>(
    written: &Expr,
    boolean: &Expected,
    mut names: N,
) -> Option<hir::ConstraintPart> {
    let mut errors = Vec::new();
    let condition = Checker::new(&mut names).expr(written, Some(boolean), &mut errors)?;
    let breaking = match form(written) {
        Form::Within { from, within } => relation_of(&from, &mut names, &mut errors)
            .zip(relation_of(&within, &mut names, &mut errors))
            .and_then(|(from, within)| ops::combined(from, within, hir::Combination::Minus).ok()),
        Form::Empty(relation) => relation_of(&relation, &mut names, &mut errors),
        Form::And(_) | Form::Other => None,
    };
    Some(hir::ConstraintPart {
        condition,
        breaking,
        locals: names.locals(),
        written: written.syntax().text().to_string(),
    })
}

/// `written` checked with `names`, if it is a relation: a form says what breaks a
/// condition only of relations, as `<=` compares other values too, and a unit's own
/// `is_empty` can take them.
fn relation_of<N: Names>(
    written: &Expr,
    names: &mut N,
    errors: &mut Vec<Diagnostic>,
) -> Option<hir::Expr> {
    let mut checker = Checker::new(names);
    let value = checker.expr(written, None, errors)?;
    matches!(checker.ty(&value), hir::Type::Relation(_)).then_some(value)
}

/// The conjuncts of `written`, a condition: the operands of each `and` it
/// is, in parentheses or not, in the order they are written, or itself.
fn conjuncts(written: &Expr) -> Vec<Expr> {
    match form(written) {
        Form::And(operands) => operands.iter().flat_map(conjuncts).collect(),
        _ => vec![unparenthesized(written)],
    }
}

/// `written` without the parentheses around it.
fn unparenthesized(written: &Expr) -> Expr {
    match written {
        Expr::Paren(paren) => paren
            .inner()
            .map_or_else(|| written.clone(), |inner| unparenthesized(&inner)),
        _ => written.clone(),
    }
}

/// What a condition's form says of what makes it false, if what it compares or
/// tests are relations.
enum Form {
    /// `a and b`, or `and{a, b}`: its operands, each of which makes it
    /// false when it is.
    And(Vec<Expr>),
    /// `a <= b`, or `b >= a`: the tuples of `from` that aren't in `within`.
    Within { from: Expr, within: Expr },
    /// `is_empty{r: e}`: the tuples of `e`.
    Empty(Expr),
    /// Anything else, which says nothing of it.
    Other,
}

/// The form of `written`, in parentheses or not.
fn form(written: &Expr) -> Form {
    match unparenthesized(written) {
        expr @ (Expr::Binary(_) | Expr::Nary(_)) => {
            if let Some(connecting) =
                ops::Connecting::of(&expr).filter(|c| c.token.kind() == SyntaxKind::AndKw)
            {
                return Form::And(connecting.operands.into_iter().flatten().collect());
            }
            let Expr::Binary(binary) = &expr else {
                return Form::Other;
            };
            match (binary.operator(), binary.lhs(), binary.rhs()) {
                (Some(token), Some(lhs), Some(rhs)) if token.kind() == SyntaxKind::LtEq => {
                    Form::Within {
                        from: lhs,
                        within: rhs,
                    }
                }
                (Some(token), Some(lhs), Some(rhs)) if token.kind() == SyntaxKind::GtEq => {
                    Form::Within {
                        from: rhs,
                        within: lhs,
                    }
                }
                _ => Form::Other,
            }
        }
        Expr::Call(call) => call
            .callee()
            .and_then(|c| c.ident())
            .filter(|ident| ident.text() == "is_empty")
            .and_then(|_| call.arguments())
            .and_then(|args| args.named().find(|a| a.is_named("r")))
            .and_then(|r| r.value())
            .map_or(Form::Other, Form::Empty),
        _ => Form::Other,
    }
}

/// The names in a constraint's condition: the variables, as variables,
/// whose values it is checked of whenever a statement ends, or else, when
/// it is checked of their `initial` values, each standing for its own,
/// and the local variables of the value blocks in it.
struct ConditionNames<'c, 'a> {
    cx: &'c Context<'a>,
    initial: bool,
    locals: Locals,
    /// The variables it mentions, by the names it writes.
    mentioned: BTreeMap<String, hir::VariableId>,
}

impl<'c, 'a> ConditionNames<'c, 'a> {
    fn new(cx: &'c Context<'a>, initial: bool) -> Self {
        ConditionNames {
            cx,
            initial,
            locals: Locals::default(),
            mentioned: BTreeMap::new(),
        }
    }

    /// What `written`, a condition or a part of one, is of the variables'
    /// initial values, which these names stand for.
    fn value(&mut self, written: &Expr, errors: &mut Vec<Diagnostic>) -> Initially {
        let Some(checked) = Checker::new(self).expr(written, None, errors) else {
            return Initially::Reported;
        };
        match ops::evaluated(checked, self.cx.bindings()) {
            Ok(computed) => ops::known_value(&computed)
                .cloned()
                .map_or(Initially::Unknown, Initially::Known),
            Err(failed) => {
                errors.push(errors::failed(&failed, written));
                Initially::Reported
            }
        }
    }

    /// The value of `written`, a part of a condition whose value is known,
    /// as [`ConditionNames::value`] has it, though errors, which it can't
    /// have, aren't reported.
    fn known(&mut self, written: &Expr) -> Option<hir::Value> {
        match self.value(written, &mut Vec::new()) {
            Initially::Known(value) => Some(value),
            Initially::Unknown | Initially::Reported => None,
        }
    }

    /// What makes `written`, a condition false of the initial values,
    /// false, where its form says, each at the part it is about: the false
    /// operands of an `and`, the tuples of `a` that aren't in `b` for
    /// `a <= b`, and those of `e` for `is_empty{r: e}`. Otherwise, that it
    /// is false.
    fn breaches(&mut self, written: &Expr) -> Vec<(Span, String)> {
        let span = trendy_syntax::span(written.syntax().text_range());
        let false_here = || vec![(span, "this is false".to_string())];
        match form(written) {
            Form::And(operands) => {
                let mut breaches = Vec::new();
                for operand in operands {
                    if self.known(&operand) == Some(hir::Value::Boolean(false)) {
                        breaches.extend(self.breaches(&operand));
                    }
                }
                if breaches.is_empty() {
                    false_here()
                } else {
                    breaches
                }
            }
            Form::Within { from, within } => match (self.known(&from), self.known(&within)) {
                (Some(hir::Value::Relation(from)), Some(hir::Value::Relation(kept))) => {
                    let outside: Vec<&hir::TupleValue> =
                        from.tuples.difference(&kept.tuples).collect();
                    let written_within = within.syntax().text().to_string();
                    vec![(span, errors::tuples_outside(&outside, &written_within))]
                }
                _ => false_here(),
            },
            Form::Empty(relation) => match self.known(&relation) {
                Some(hir::Value::Relation(value)) => {
                    let tuples: Vec<&hir::TupleValue> = value.tuples.iter().collect();
                    let written_relation = relation.syntax().text().to_string();
                    vec![(span, errors::tuples_in(&tuples, &written_relation))]
                }
                _ => false_here(),
            },
            Form::Other => false_here(),
        }
    }
}

impl PartNames for ConditionNames<'_, '_> {
    fn locals(self) -> Vec<hir::Local> {
        self.locals.finish()
    }
}

impl Names for ConditionNames<'_, '_> {
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
        if let Some(id) = self.cx.declared.find(name, span) {
            self.mentioned.insert(name.to_string(), id);
            // A variable with errors in its declaration is already reported.
            return match &self.cx.variables[id.0] {
                // Of its declared type, which can be a supertype of its
                // value's.
                Some(variable) if self.initial => {
                    Lookup::Found(hir::Expr::Constant(variable.init.clone()).treated(&variable.ty))
                }
                Some(_) => Lookup::Found(hir::Expr::Variable(id)),
                None => Lookup::Reported,
            };
        }
        if let Some(id) = self.cx.declared.find_constant(name, span) {
            return self.cx.constant_read(id);
        }
        if self.cx.namespaces.operator(name, span).is_some() {
            errors.push(errors::operator_as_value(name, span, self.cx.operators));
            return Lookup::Reported;
        }
        Lookup::Unknown
    }

    fn constant_of(
        &mut self,
        name: &str,
        unit: hir::FileId,
        span: Span,
        _errors: &mut Vec<Diagnostic>,
    ) -> Lookup {
        self.cx
            .declared
            .constant_in(name, unit, span)
            .map_or(Lookup::Unknown, |id| self.cx.constant_read(id))
    }

    fn candidates(&self, at: Span) -> Vec<String> {
        self.cx.declared.names(at).map(str::to_string).collect()
    }

    fn leaf_type(&self, leaf: &hir::Expr) -> hir::Type {
        match leaf {
            hir::Expr::Variable(id) => self.cx.variables[id.0]
                .as_ref()
                .expect("only valid variables are resolved")
                .ty
                .clone(),
            hir::Expr::Local(id) => self.locals.get(*id).ty.clone(),
            _ => unreachable!("a condition's other names are constants, not {leaf:?}"),
        }
    }

    fn fresh_local(&mut self, local: hir::Local) -> hir::LocalId {
        self.locals.fresh(local)
    }

    fn next_local(&self) -> usize {
        self.locals.next()
    }

    fn hides(&self, _: &str) -> bool {
        false
    }

    fn checked(&mut self, id: hir::OperatorId, _: Span, _: &mut Vec<Diagnostic>) -> bool {
        self.cx.checked(id)
    }

    fn bindings(&self) -> ops::Bindings<'_> {
        self.cx.bindings()
    }

    fn narrowing_ended(&self, _: &str) -> Option<Span> {
        None
    }
}
