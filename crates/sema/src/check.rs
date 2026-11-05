//! Checking expressions: types, operators and names, into `hir::Expr`.
//!
//! One checker serves initial values and operator bodies alike. What a name
//! refers to is all that differs between them ([`Names`]): in an initial
//! value, a variable's value, evaluated when compiling; in a body, a
//! parameter or a variable, known only when running. Whatever doesn't
//! depend on what is known only when running is evaluated now, to a
//! constant, and the rest simplified, so an initial value always checks to
//! a constant.

use std::collections::{BTreeMap, BTreeSet};

use trendy_diag::{CodeKind, Diagnostic, Span};
use trendy_hir as hir;
use trendy_syntax::ast::{self, AstNode, AttributeValue, Expr, Said};
use trendy_syntax::{SyntaxKind, SyntaxToken};

use crate::narrowed::{self, Facts};
use crate::operators::{ArgumentNames, Chosen, Operators, Param, Signature};
use crate::ops::{self, Operator, Resolved};
use crate::types::{self, Expected};
use crate::{Kind, Namespace, Namespaces, errors};

/// What a name refers to.
pub(crate) enum Lookup {
    Found(hir::Expr),
    /// Something that can't be used here, already reported.
    Reported,
    /// Nothing, which the checker reports with the names that are in scope.
    Unknown,
}

/// What a `where` restricts: a relation, or the operands of a join or a
/// product of relations, kept apart, so that the condition restricts them
/// as they are joined (see [`ops::restricted_and`]).
enum Restricted {
    Relation(hir::Expr),
    Joined {
        operands: Vec<hir::Expr>,
        heading: hir::Heading,
    },
}

/// `operands`, of the join written `expr`, combined as `combined` says.
/// `None` after reporting why constants have no result.
fn combine(
    operands: Vec<hir::Expr>,
    combined: ops::Combined,
    expr: &Expr,
    errors: &mut Vec<Diagnostic>,
) -> Option<hir::Expr> {
    let result = match combined {
        ops::Combined::And => ops::and(operands),
        ops::Combined::Combine(combination) => {
            let mut operands = operands.into_iter();
            let first = operands.next()?;
            operands.try_fold(first, |result, operand| {
                ops::combined(result, operand, combination)
            })
        }
        ops::Combined::Matching { negated } => {
            let mut operands = operands.into_iter();
            let (lhs, rhs) = (operands.next()?, operands.next()?);
            Ok(ops::matching(lhs, rhs, negated))
        }
        ops::Combined::Composed(steps) => {
            let mut operands = operands.into_iter();
            let first = operands.next()?;
            operands
                .zip(steps)
                .try_fold(first, |result, (operand, common)| {
                    Ok(ops::remove(ops::and(vec![result, operand])?, common))
                })
        }
    };
    result
        .map_err(|failed| errors.push(errors::failed(&failed, expr)))
        .ok()
}

/// `expr` without the parentheses around it, if it has any.
fn unparenthesized(expr: &Expr) -> Expr {
    match expr {
        Expr::Paren(paren) => paren
            .inner()
            .map_or_else(|| expr.clone(), |e| unparenthesized(&e)),
        _ => expr.clone(),
    }
}

/// What names in an expression refer to.
pub(crate) trait Names {
    /// The program's top-level names.
    fn namespaces(&self) -> &Namespaces;

    /// The site of what starts at `at`, in the file being checked.
    fn site(&self, at: Span) -> hir::Site;

    /// What `name`, written at `span`, refers to.
    fn resolve(&mut self, name: &str, span: Span, errors: &mut Vec<Diagnostic>) -> Lookup;

    /// What `name`, written at `span`, refers to as a variable, which a
    /// `var` or `out` argument passes: as it is declared, whatever a test
    /// shows of its value, since the operator can give it any value of its
    /// type (see [`crate::narrowed`]).
    fn resolve_variable(&mut self, name: &str, span: Span, errors: &mut Vec<Diagnostic>) -> Lookup {
        self.resolve(name, span, errors)
    }

    /// What reading the constant named `name` that `unit` declares is, written
    /// at `span`: nothing, if what is written there doesn't see one.
    fn constant_of(
        &mut self,
        name: &str,
        unit: hir::FileId,
        span: Span,
        errors: &mut Vec<Diagnostic>,
    ) -> Lookup;

    /// The names in scope at `at`, which a name written there that refers
    /// to nothing may be a misspelling of.
    fn candidates(&self, at: Span) -> Vec<String>;

    /// Whether a value named `name` is in scope at `at`.
    fn has_value(&self, name: &str, at: Span) -> bool {
        self.candidates(at).iter().any(|c| c == name)
    }

    /// The type of `leaf`, a variable or parameter that [`Names::resolve`]
    /// gave.
    fn leaf_type(&self, leaf: &hir::Expr) -> hir::Type;

    /// The id of `local`, a value block's local variable, among those of the
    /// operator whose body this is, or of the initial value being checked.
    fn fresh_local(&mut self, local: hir::Local) -> hir::LocalId;

    /// The id [`Names::fresh_local`] would give next, past the local
    /// variables so far: those of a value block with statements that are
    /// run follow it, and join these when it ends.
    fn next_local(&self) -> usize;

    /// Whether a local variable named `name`, declared here, would hide a
    /// name that means something here: a parameter, a local variable or an
    /// attribute of the tuple at hand. It may hide a variable of the
    /// program's.
    fn hides(&self, name: &str) -> bool;

    /// The heading of the tuple at hand, `Current(0, …)`, if there is one:
    /// in the values of an `extend` or a `replace`.
    fn tuple_at_hand(&self) -> Option<&hir::Heading> {
        None
    }

    /// Whether the tuple at hand is one a load into a relation without
    /// `numbered` makes of an array's element, whose position a name that
    /// refers to nothing may be meant as.
    fn unnumbered(&self) -> bool {
        false
    }

    /// The type parameters of the operator whose body this is, which a
    /// type written in it can name: `None` outside one, in an initial
    /// value.
    fn type_parameters(&self) -> Option<&[hir::TypeParameter]> {
        None
    }

    /// Every operator, by id and by name.
    fn operators(&self) -> &Operators;

    /// Whether the read-only operator `id`, called at `at`, has a valid
    /// value. Its body is checked first if it isn't yet, which can close a
    /// cycle, reported then.
    fn checked(&mut self, id: hir::OperatorId, at: Span, errors: &mut Vec<Diagnostic>) -> bool;

    /// A call of the operator `callee`, its name written at `span`, gives its
    /// type parameters `types`, in an operator whose calls can lead back to
    /// itself: a read-only one's, which [`body::Growing`] records.
    fn calling(&mut self, _callee: &Signature, _types: &[hir::Type], _span: Span) {}

    /// What evaluating a call substitutes, besides its arguments.
    fn bindings(&self) -> ops::Bindings<'_>;

    /// Where a statement may assign `name`, if that ended what a test
    /// showed of its value, in the branch or block being checked, so that
    /// an error that needs it narrowed can say why it isn't.
    fn narrowing_ended(&self, name: &str) -> Option<Span>;
}

/// A call's arguments, as written.
#[derive(Clone, Copy)]
pub(crate) enum Written<'a> {
    /// In braces, `{name: value, …}`.
    Arguments(&'a ast::Arguments),
    /// Names alone, each its own value, `f{a, b}`, or none, `f{}`: a
    /// projection to the parser.
    Nothing(&'a ast::NameList),
}

impl Written<'_> {
    /// Where the arguments are written, in the unit whose operators the
    /// call sees.
    pub(crate) fn span(self) -> Span {
        match self {
            Written::Arguments(arguments) => arguments.span(),
            Written::Nothing(list) => list.span(),
        }
    }
}

/// Checks expressions whose names `names` resolves.
pub(crate) struct Checker<'a> {
    names: &'a mut dyn Names,
}

impl<'a> Checker<'a> {
    pub(crate) fn new(names: &'a mut dyn Names) -> Self {
        Checker { names }
    }

    pub(crate) fn namespaces(&self) -> &Namespaces {
        self.names.namespaces()
    }

    /// The type parameters a type written here can name.
    pub(crate) fn type_parameters(&self) -> types::TypeParameters<'_> {
        match self.names.type_parameters() {
            Some(list) => types::TypeParameters::Fixed(list),
            None => types::TypeParameters::None,
        }
    }

    /// The type of `expr`, which this checker produced.
    pub(crate) fn ty(&self, expr: &hir::Expr) -> hir::Type {
        expr.type_with(&|leaf| match leaf {
            hir::Expr::Call(call) => self
                .names
                .operators()
                .get(call.operator)
                .result
                .as_ref()
                .expect("only valid read-only operators are called")
                .instantiated(&call.types),
            _ => self.names.leaf_type(leaf),
        })
    }

    /// What evaluating a call substitutes, as for [`Names::bindings`].
    pub(crate) fn bindings(&self) -> ops::Bindings<'_> {
        self.names.bindings()
    }

    /// Whether each conversion that comparing values of type `ty` calls,
    /// for an order of a type of the program's own that compares the
    /// components of another possible representation than the stored one,
    /// its own or a component's, is checked, so that comparisons of its
    /// values fold, after checking it, at `at`, if it isn't yet: `false`
    /// after reporting that it has errors, or that it compares values of
    /// its own type, which needs it again.
    pub(crate) fn orders_checked(
        &mut self,
        ty: &hir::Type,
        at: Span,
        errors: &mut Vec<Diagnostic>,
    ) -> bool {
        let hir::Type::Defined(defined) = ty else {
            return true;
        };
        let Some((order, components)) = defined.ordered_by() else {
            return true;
        };
        if let Some(from) = order.from
            && !self.names.checked(from, at, errors)
        {
            return false;
        }
        order.items.iter().all(|item| {
            let component = components
                .get(&item.attribute)
                .expect("an order names its representation's components");
            self.orders_checked(component, at, errors)
        })
    }

    /// The value `expr` is, if it is known when compiling.
    pub(crate) fn constant(expr: &hir::Expr) -> Option<&hir::Value> {
        ops::known_value(expr)
    }

    /// `out`, written `expr`, if it has the expected type, or a subtype of
    /// it, as which it is then treated (*Type Inheritance and Relational
    /// Theory*, IM Prescription 16); otherwise report the mismatch.
    pub(crate) fn matched(
        &self,
        expected: Option<&Expected>,
        out: hir::Expr,
        expr: &Expr,
        errors: &mut Vec<Diagnostic>,
    ) -> Option<hir::Expr> {
        let ty = self.ty(&out);
        match expected {
            Some(e) if ty != e.ty && ty.is_subtype_of(&e.ty) => Some(out.treated(&e.ty)),
            Some(e) if ty != e.ty => {
                errors.push(errors::type_mismatch(
                    &e.ty,
                    &e.origin,
                    &ty,
                    Self::constant(&out),
                    expr,
                ));
                None
            }
            _ => Some(out),
        }
    }

    /// `expr`, checked against `expected` if given, or `None` after
    /// reporting why it has no valid value.
    pub(crate) fn expr(
        &mut self,
        expr: &Expr,
        expected: Option<&Expected>,
        errors: &mut Vec<Diagnostic>,
    ) -> Option<hir::Expr> {
        match expr {
            Expr::Literal(literal) => {
                let value = types::literal_value(literal, expr, self, errors)?;
                self.matched(expected, value, expr, errors)
            }
            Expr::Name(name_ref) => self.named(name_ref, expr, expected, errors),
            Expr::Qualified(written) => self.qualified(written, expr, expected, errors),
            Expr::Is(test) => self.type_test(test, expr, expected, errors),
            Expr::Treat(treat) => self.treated(treat, expr, expected, errors),
            Expr::Fail(fail) => self.failure(fail, expr, expected, errors),
            Expr::Block(block) => crate::block::value(block, &mut *self.names, expected, errors),
            Expr::Paren(paren) => self.expr(&paren.inner()?, expected, errors),
            Expr::Unary(unary) => self.prefixed(unary, expr, expected, errors),
            Expr::Binary(binary)
                if binary
                    .operator()
                    .is_some_and(|t| t.kind() == SyntaxKind::WhereKw) =>
            {
                self.restricted(binary, expr, expected, errors)
            }
            Expr::Binary(_) | Expr::Nary(_) if ops::Connecting::of(expr).is_some() => {
                self.connective(expr, expected, errors)
            }
            Expr::Binary(_) | Expr::Nary(_) if ops::Joining::of(expr).is_some() => {
                self.joined(expr, expected, errors)
            }
            Expr::Nary(_) => None,
            Expr::Binary(binary) => self.infixed(binary, expr, expected, errors),
            Expr::Extract(extract) => self.extracted(extract, expr, expected, errors),
            Expr::Postfix(postfix)
                if postfix
                    .operator()
                    .is_some_and(|t| t.kind() == SyntaxKind::TcloseKw) =>
            {
                self.closed(postfix, expr, expected, errors)
            }
            Expr::Postfix(postfix) if let Some(gathering) = ops::Gathering::of(postfix) => {
                self.gathered(postfix, gathering, expr, expected, errors)
            }
            Expr::Postfix(postfix) if let Some(unpacking) = ops::Unpacking::of(postfix) => {
                self.unpacked(postfix, unpacking, expr, expected, errors)
            }
            Expr::Postfix(postfix) if let Some(assigning) = ops::Assigning::of(postfix) => {
                self.assigned(postfix, assigning, expr, expected, errors)
            }
            Expr::Call(call) => self.written_call(call, expr, expected, errors),
            Expr::MethodCall(call) => self.method_called(call, errors),
            Expr::Project(project) if let Some((ident, list)) = self.projected_call(project) => {
                self.projection_call(&ident, &list, expr, expected, errors)
            }
            Expr::Project(project) if let Some((written, list)) = self.qualified_call(project) => {
                let ident = written.ident()?;
                let name =
                    self.namespaces()
                        .written_name(&ident, written.qualifier().as_ref(), errors)?;
                self.call(
                    &ident,
                    name,
                    Written::Nothing(&list),
                    expr,
                    expected,
                    errors,
                )
            }
            Expr::Project(project) => self.projection(project, expr, expected, errors),
            Expr::Postfix(_) => self.reshaped(expr, expected, errors),
            Expr::Image(image) => self.image(image, expr, expected, errors),
            Expr::From(from) => self.taken(from, expr, expected, errors),
            Expr::Tuple(tuple) => types::tuple_selector(tuple, expected, self, errors),
            Expr::Relation(relation) => types::relation_selector(relation, expected, self, errors),
            Expr::Array(array) => types::array_selector(array, expected, self, errors),
            Expr::Index(index) => self.indexed(index, expr, expected, errors),
            Expr::If(if_expr) => self.conditional(if_expr, expected, errors),
        }
    }

    /// `unary`, written `expr`, an operator before its operand: a negative
    /// literal, or `not`, `-` or `+` of an operand. A `not` of an integer
    /// that begins a comparison is an error, as other languages read it
    /// differently (A0076).
    fn prefixed(
        &mut self,
        unary: &ast::UnaryExpr,
        expr: &Expr,
        expected: Option<&Expected>,
        errors: &mut Vec<Diagnostic>,
    ) -> Option<hir::Expr> {
        if let Some(literal) = ops::negated_literal(unary) {
            let value = ops::negative_value(&literal, expr, errors)?;
            return self.matched(expected, hir::Expr::Constant(value), expr, errors);
        }
        let token = unary.operator()?;
        let (resolved, operands, result) =
            self.operands(&token, vec![unary.operand()], expected, errors)?;
        types::result_type(expected, &result, expr, errors)?;
        let [operand] = <[hir::Expr; 1]>::try_from(operands).ok()?;
        if matches!(
            resolved,
            Resolved::Unary(hir::UnaryOp::Complement) | Resolved::NotByte
        ) && let Some(comparison) = errors::begun_comparison(&token)
        {
            errors.push(errors::complement_compared(
                &token,
                &unary.operand()?,
                &comparison,
                &result,
            ));
            return None;
        }
        match resolved {
            Resolved::Identity => Some(operand),
            Resolved::Unary(op) => types::evaluated(ops::unary(op, operand), expr, None, errors),
            Resolved::NotByte => self.not_byte(operand, expr, errors),
            Resolved::Binary(_) | Resolved::Bytes(_) | Resolved::Connective { .. } => None,
        }
    }

    /// `binary`, written `expr`, an operator between two scalar operands,
    /// or a comparison: neither a connective, a join nor a restriction,
    /// which have their own checks.
    fn infixed(
        &mut self,
        binary: &ast::BinaryExpr,
        expr: &Expr,
        expected: Option<&Expected>,
        errors: &mut Vec<Diagnostic>,
    ) -> Option<hir::Expr> {
        let token = binary.operator()?;
        let (resolved, operands, result) =
            self.operands(&token, vec![binary.lhs(), binary.rhs()], expected, errors)?;
        types::result_type(expected, &result, expr, errors)?;
        let Ok([lhs, rhs]) = <[hir::Expr; 2]>::try_from(operands) else {
            return None;
        };
        let Resolved::Binary(op) = resolved else {
            unreachable!("a connective, which `Checker::connective` checks");
        };
        if let hir::BinaryOp::Compare(..) = op
            && !self.orders_checked(
                &self.ty(&lhs),
                trendy_syntax::span(token.text_range()),
                errors,
            )
        {
            return None;
        }
        match ops::binary(op, lhs, rhs, self.bindings()) {
            Ok(result) => Some(result),
            Err(ops::Failed::Scalar(failure)) => {
                errors.push(errors::failure(failure, expr, binary.rhs().as_ref()));
                None
            }
            Err(failed) => {
                errors.push(errors::failed(&failed, expr));
                None
            }
        }
    }

    /// `if_expr`: its `then` branch's value if its condition is true, and
    /// else its `else` branch's, of one type. Only the branch taken is
    /// computed, so a condition known when compiling leaves the other's
    /// failures unreported.
    fn conditional(
        &mut self,
        if_expr: &ast::IfExpr,
        expected: Option<&Expected>,
        errors: &mut Vec<Diagnostic>,
    ) -> Option<hir::Expr> {
        let condition_expr = if_expr.condition()?;
        let branches = [if_expr.then_branch()?, if_expr.else_branch()?];
        if let Some(error) = errors::pointless_if(if_expr, &condition_expr, &branches) {
            errors.push(error);
        }
        let boolean = Expected {
            ty: hir::Type::Scalar(hir::Scalar::Boolean),
            origin: types::Origin::Operator(if_expr.keyword()?),
        };
        let condition = self.expr(&condition_expr, Some(&boolean), errors);
        let taken = match &condition {
            Some(hir::Expr::Constant(hir::Value::Boolean(taken))) => Some(*taken),
            _ => None,
        };
        // What the condition's tests show where each branch is taken.
        let shown = [
            Facts::of(&condition_expr, true, self),
            Facts::of(&condition_expr, false, self),
        ];
        let [then, otherwise] = self.branches(&branches, &shown, taken, expected, errors)?;
        match (taken, then, otherwise) {
            (Some(true), then, _) => then,
            (Some(false), _, otherwise) => otherwise,
            (None, Some(then), Some(otherwise)) => {
                Some(ops::conditional(condition?, then, otherwise))
            }
            (None, ..) => None,
        }
    }

    /// An `if`'s branches, `then` and `else`, written `written`, of one
    /// type: the type expected, or else the most specific common supertype
    /// of those valid on their own (see [`types::widen`]), each with the
    /// names narrowed that `shown` says the condition's tests show where
    /// it is taken. The one a condition known when compiling doesn't take,
    /// as `taken` says, is never computed, so its failures aren't
    /// reported, and it has no value if one makes it fail. `None` after
    /// reporting a branch's errors.
    fn branches(
        &mut self,
        written: &[Expr; 2],
        shown: &[Facts; 2],
        taken: Option<bool>,
        expected: Option<&Expected>,
        errors: &mut Vec<Diagnostic>,
    ) -> Option<[Option<hir::Expr>; 2]> {
        let skipped = |i: usize| taken.is_some_and(|taken| taken != (i == 0));
        let check =
            |this: &mut Self, i: usize, expected: Option<&Expected>, errors: &mut Vec<_>| {
                if !skipped(i) {
                    return this.narrowed(&written[i], &shown[i], expected, errors);
                }
                let mut own: Vec<Diagnostic> = Vec::new();
                let out = this.narrowed(&written[i], &shown[i], expected, &mut own);
                errors.extend(own.into_iter().filter(|d| d.code.kind() != CodeKind::Run));
                out
            };
        let mut expectation = expected.cloned();
        let mut alone: [Option<types::Alone<(hir::Expr, hir::Type)>>; 2] = [None, None];
        if expectation.is_none() {
            for (i, branch) in written.iter().enumerate() {
                let mut own = Vec::new();
                let out = check(self, i, None, &mut own).map(|out| {
                    let ty = self.ty(&out);
                    (out, ty)
                });
                let on_its_own = types::Alone::of(out, own, branch.span());
                if let types::Alone::Valid((_, ty)) = &on_its_own {
                    types::widen(&mut expectation, ty.clone(), branch);
                }
                alone[i] = Some(on_its_own);
            }
        }
        let mut values = [None, None];
        let mut ok = true;
        for (i, alone) in alone.into_iter().enumerate() {
            let before = errors.len();
            let out = match alone {
                Some(alone) => {
                    alone.fit(expectation.as_ref(), written[i].span(), errors, |errors| {
                        check(self, i, expectation.as_ref(), errors)
                    })
                }
                None => check(self, i, expectation.as_ref(), errors),
            };
            // A branch not taken that would fail is no error.
            ok &= out.is_some() || (skipped(i) && errors.len() == before);
            values[i] = out;
        }
        ok.then_some(values)
    }

    /// The value `name_ref` names, written `expr`.
    fn named(
        &mut self,
        name_ref: &ast::NameRef,
        expr: &Expr,
        expected: Option<&Expected>,
        errors: &mut Vec<Diagnostic>,
    ) -> Option<hir::Expr> {
        let ident = name_ref.ident()?;
        let (name, span) = (ident.text(), trendy_syntax::span(ident.text_range()));
        match self.names.resolve(name, span, errors) {
            Lookup::Found(resolved) => self.matched(expected, resolved, expr, errors),
            Lookup::Reported => None,
            // `ARRAY[1]`, as SQL writes an array of one element.
            Lookup::Unknown if name.eq_ignore_ascii_case("array") && indexed(name_ref) => {
                errors.push(errors::miscased_array(name, span));
                None
            }
            // `any{a, b}`: Python's `any` or `all` with a list's braces.
            Lookup::Unknown if matches!(name, "all" | "any") && projected(name_ref) => {
                errors.push(errors::listed_booleans(name, span));
                None
            }
            Lookup::Unknown => {
                let namespaces = self.names.namespaces();
                if let Some(clash) = errors::name_clash(name, span, Namespace::Value, namespaces) {
                    errors.push(clash);
                    return None;
                }
                if self.names.namespaces().database(name, span).is_some() {
                    errors.push(errors::database_as_value(name, span));
                    return None;
                }
                if let Some((meant, written)) = self.constant_elsewhere(name_ref, name, span) {
                    errors.push(errors::constant_as_value(name, written, &meant));
                    return None;
                }
                let candidates = self.names.candidates(span);
                let candidates: Vec<&str> = candidates.iter().map(String::as_str).collect();
                errors.push(if self.names.unnumbered() {
                    errors::unnumbered_position(name, span, &candidates)
                } else if name_ref.syntax().kind() == trendy_syntax::SyntaxKind::Name {
                    errors::unknown_name_alone(name, span, &candidates)
                } else {
                    errors::unknown_name(name, span, "value", &candidates)
                });
                None
            }
        }
    }

    /// `written`, as `expr`: a constant the unit it names declares, the one
    /// kind of value a unit's interface can give another.
    fn qualified(
        &mut self,
        written: &ast::QualifiedName,
        expr: &Expr,
        expected: Option<&Expected>,
        errors: &mut Vec<Diagnostic>,
    ) -> Option<hir::Expr> {
        let ident = written.ident()?;
        let qualifier = written.qualifier()?;
        let span = trendy_syntax::span(ident.text_range());
        // `math::pi`, as `math.pi`, of a library no one wrote.
        if self
            .namespaces()
            .written_name_unreported(&ident, Some(&qualifier))
            .is_none()
            && let Some(meant) = self.constant_called_otherwise(ident.text(), span)
        {
            let whole = written.syntax().text().to_string();
            errors.push(errors::constant_as_value(&whole, written.span(), &meant));
            return None;
        }
        let name = self
            .namespaces()
            .written_name(&ident, Some(&qualifier), errors)?;
        let unit = name.unit?;
        match self.names.constant_of(ident.text(), unit, span, errors) {
            Lookup::Found(value) => self.matched(expected, value, expr, errors),
            Lookup::Reported => None,
            Lookup::Unknown => {
                errors.push(errors::unknown_qualified_value(
                    written,
                    &ident,
                    unit,
                    self.names.namespaces(),
                    self.names.operators(),
                ));
                None
            }
        }
    }

    /// What `name_ref`, which names no value, is written for as another language
    /// writes a constant, as `pi` is written `PI`, in another case, C's `M_PI`,
    /// or `math.pi` and `Math.PI`, with the library it is in: the constant, or
    /// the call of the read-only operator without parameters, of its name, and
    /// the span of what is written.
    fn constant_elsewhere(
        &self,
        name_ref: &ast::NameRef,
        name: &str,
        span: Span,
    ) -> Option<(String, Span)> {
        let member = name_ref
            .syntax()
            .parent()
            .and_then(ast::ExtractExpr::cast)
            .filter(|_| matches!(name, "math" | "Math"))
            .and_then(|extract| Some((extract.attribute()?.ident()?, extract.span())));
        let (constant, written) = match &member {
            Some((attribute, whole)) => (attribute.text(), *whole),
            None => (name, span),
        };
        Some((self.constant_called_otherwise(constant, span)?, written))
    }

    /// What is written for `name`, which another language would call a
    /// constant, seen from `span`: the constant, or the call of the read-only
    /// operator without parameters, of its name in lowercase, without C's `M_`.
    fn constant_called_otherwise(&self, name: &str, span: Span) -> Option<String> {
        let lowered = name.strip_prefix("M_").unwrap_or(name).to_ascii_lowercase();
        if self.names.namespaces().constant(&lowered, span).is_some() {
            return Some(lowered);
        }
        let operator = self
            .names
            .operators()
            .overloads(&lowered, span)
            .any(|signature| signature.read_only && signature.written.is_empty());
        operator.then(|| format!("{lowered}{{}}"))
    }

    /// `index`, `a[i]`, written `expr`: the element of the array `a` at the
    /// position `i`, counting from 1.
    fn indexed(
        &mut self,
        index: &ast::IndexExpr,
        expr: &Expr,
        expected: Option<&Expected>,
        errors: &mut Vec<Diagnostic>,
    ) -> Option<hir::Expr> {
        let operand_expr = index.operand()?;
        let position_expr = index.index()?;
        let operand = self.expr(&operand_expr, None, errors);
        let ty = operand.as_ref().map(|operand| self.ty(operand));
        let is_array = matches!(ty, Some(hir::Type::Array(_)));
        if let Some(ty) = ty.as_ref().filter(|_| !is_array) {
            errors.push(errors::not_indexable(index, ty, &operand_expr));
        }
        // Not an array's, the position has no type to expect, but can
        // still have errors of its own.
        let position = if let Some(token) = index.l_bracket().filter(|_| is_array) {
            let integer = Expected {
                ty: hir::Type::Scalar(hir::Scalar::Integer),
                origin: types::Origin::Operator(token),
            };
            self.expr(&position_expr, Some(&integer), errors)
        } else {
            self.expr(&position_expr, None, errors);
            None
        };
        let element = types::evaluated(
            ops::index(operand?, position?),
            expr,
            Some(&position_expr),
            errors,
        )?;
        self.matched(expected, element, expr, errors)
    }

    /// The error for `extract`, `x.f`, of `operand`, of type `ty`, whose
    /// other representations' components are `others`, if `x` has no
    /// attribute or component `f` and an operator is named `f`: a method
    /// call, as object-oriented languages write one, which trendy writes
    /// `f{p: x}`.
    fn method_call(
        &self,
        extract: &ast::ExtractExpr,
        operand: &Expr,
        ty: &hir::Type,
        others: &[&hir::Heading],
    ) -> Option<Diagnostic> {
        let ident = extract.attribute()?.ident()?;
        let name = ident.text();
        let has = match ty {
            hir::Type::Tuple(heading) => heading.get(name).is_some(),
            hir::Type::Defined(defined) => {
                defined.components.get(name).is_some()
                    || others.iter().any(|c| c.get(name).is_some())
            }
            _ => false,
        };
        if has {
            return None;
        }
        let operators = self.names.operators();
        let at = trendy_syntax::span(ident.text_range());
        let first = operators.overloads(name, at).next()?;
        // What follows `x.f`: the call's other arguments, in braces, or none.
        // Names in braces, `x.f{a}`, might be attributes' or values, so
        // which parameter `x` is for isn't known.
        let parent = extract.syntax().parent();
        let call = parent.clone().and_then(ast::MethodCallExpr::cast);
        let projection = parent.and_then(ast::ProjectExpr::cast);
        let whole = match (&call, &projection) {
            (Some(call), _) => call.span(),
            (None, Some(projection)) => projection.span(),
            (None, None) => extract.span(),
        };
        let written: Option<Vec<ast::AttributeValue>> = match (&call, &projection) {
            (Some(call), _) => call
                .arguments()
                .filter(|args| args.positional().next().is_none())
                .map(|args| args.named().collect()),
            (None, Some(projection)) => projection
                .names()
                .filter(|names| names.names().next().is_none())
                .map(|_| Vec::new()),
            (None, None) => Some(Vec::new()),
        };
        // The overloads that the arguments written leave one parameter
        // of: `x` is for it if just one does.
        let fitting: Vec<&Signature> = written.as_ref().map_or_else(Vec::new, |written| {
            operators
                .overloads(name, at)
                .filter(|s| {
                    s.written.len() == written.len() + 1
                        && written.iter().all(|a| {
                            a.name()
                                .and_then(|n| n.ident())
                                .is_some_and(|i| s.written.contains_key(i.text()))
                        })
                })
                .collect()
        });
        let only = match (fitting.as_slice(), &written) {
            ([signature], Some(written)) => signature
                .written
                .keys()
                .find(|p| !written.iter().any(|a| a.is_named(p)))
                .map(|param| (param.as_str(), written.as_slice())),
            _ => None,
        };
        let signature = fitting.first().copied().unwrap_or(first);
        Some(errors::method_call(
            name,
            at,
            operand,
            whole,
            only,
            &operators.described(signature, at),
        ))
    }

    /// A projection, `remove` or `rename`, written `expr`.
    fn reshaped(
        &mut self,
        expr: &Expr,
        expected: Option<&Expected>,
        errors: &mut Vec<Diagnostic>,
    ) -> Option<hir::Expr> {
        let (operand_expr, postfix) = match expr {
            Expr::Project(project) => (project.operand()?, None),
            Expr::Postfix(postfix) => (postfix.operand()?, Some(postfix)),
            _ => unreachable!("only projections and postfix operators reshape"),
        };
        let operand = self.expr(&operand_expr, None, errors)?;
        let ty = self.ty(&operand);
        let result = match (expr, postfix) {
            (_, Some(postfix)) if ops::is_rename(postfix) => {
                let renamings = ops::renamed(postfix, &operand_expr, &ty, errors)?;
                ops::rename(operand, renamings)
            }
            (Expr::Project(project), _) => {
                // `pi{}`, a constant called as if it were an operator.
                if !matches!(ty, hir::Type::Tuple(_) | hir::Type::Relation(_))
                    && let Some(list) = project.names()
                    && let Some((written, at)) = self.constant_written(&operand_expr)
                {
                    let list = Written::Nothing(&list);
                    errors.push(errors::constant_called(&written, at, list));
                    return None;
                }
                let projection = ops::Projection::Project(project);
                let names = ops::removed(projection, &operand_expr, &ty, errors)?;
                ops::remove(operand, names)
            }
            (_, Some(postfix)) => {
                let projection = ops::Projection::Remove(postfix);
                let names = ops::removed(projection, &operand_expr, &ty, errors)?;
                ops::remove(operand, names)
            }
            _ => unreachable!("matched above"),
        };
        self.matched(expected, result, expr, errors)
    }

    /// How `operand` names a constant, if it does, and where the constant's
    /// name is.
    fn constant_written(&self, operand: &Expr) -> Option<(String, Span)> {
        let (ident, written) = match operand {
            Expr::Name(name_ref) => {
                let ident = name_ref.ident()?;
                let at = trendy_syntax::span(ident.text_range());
                let constant = !self.names.hides(ident.text())
                    && self.namespaces().constant(ident.text(), at).is_some();
                if !constant {
                    return None;
                }
                let written = ident.text().to_string();
                (ident, written)
            }
            // Only a constant has a value, written so.
            Expr::Qualified(written) => (written.ident()?, written.syntax().text().to_string()),
            _ => return None,
        };
        Some((written, trendy_syntax::span(ident.text_range())))
    }

    /// The operand and list of `project`, if it is a call of an operator of
    /// another unit's written as a projection: if the operand is a qualified
    /// name, which is a value only if that unit has a constant of that name,
    /// and not before `{}` if it has an operator of that name too. One whose
    /// qualifier names no unit is reported as a call would be.
    fn qualified_call(
        &self,
        project: &ast::ProjectExpr,
    ) -> Option<(ast::QualifiedName, ast::NameList)> {
        let Expr::Qualified(written) = project.operand()? else {
            return None;
        };
        let list = project.names()?;
        let ident = written.ident()?;
        let qualifier = written.qualifier();
        let name = self
            .namespaces()
            .written_name_unreported(&ident, qualifier.as_ref());
        let constant = name.is_some_and(|name| {
            name.unit
                .is_some_and(|unit| self.namespaces().has_constant_in(name.name, unit, name.at))
        });
        // As before `{}` a name that is an operator's is a call, a value of its
        // name or not.
        let operator = name.is_some_and(|name| !self.names.operators().called(name).is_empty());
        (!constant || (operator && list.names().next().is_none())).then_some((written, list))
    }

    /// The operator's name and the list, if `project` is a call written as
    /// a projection: of a name right before `{}` that is an operator's, as
    /// it is after `call`, or of an operator's name before attribute names
    /// when no value has it, which can then only be meant as a call, as is
    /// another language's name for one of the prelude's, as in Python's
    /// `len{a}`.
    fn projected_call(&self, project: &ast::ProjectExpr) -> Option<(SyntaxToken, ast::NameList)> {
        let Expr::Name(name_ref) = project.operand()? else {
            return None;
        };
        let ident = name_ref.ident()?;
        let list = project.names()?;
        let name = ident.text();
        let at = trendy_syntax::span(ident.text_range());
        let operator = self.names.operators().has(name, at);
        let call = if operator {
            list.names().next().is_none() || !self.names.has_value(name, at)
        } else {
            // Another language's operator, or one of a unit this one could
            // use, which its error names.
            let foreign = errors::elsewhere(name).is_some()
                || errors::absent(name).is_some()
                || errors::tutorial_d_tested(name, at, self.namespaces()).is_some()
                || !self
                    .namespaces()
                    .exporters(name, at, Kind::Operator)
                    .0
                    .is_empty();
            foreign && !self.names.has_value(name, at)
        };
        call.then_some((ident, list))
    }

    /// `call`, written `expr`, with its arguments in braces.
    fn written_call(
        &mut self,
        call: &ast::CallExpr,
        expr: &Expr,
        expected: Option<&Expected>,
        errors: &mut Vec<Diagnostic>,
    ) -> Option<hir::Expr> {
        let ident = call.callee()?.ident()?;
        let args = call.arguments()?;
        let name = self
            .namespaces()
            .written_name(&ident, call.qualifier().as_ref(), errors)?;
        self.call(
            &ident,
            name,
            Written::Arguments(&args),
            expr,
            expected,
            errors,
        )
    }

    /// A call of the operator `ident` names, written `expr` as a projection
    /// on `list`: without arguments if it is empty, and otherwise with the
    /// names in it as positional arguments, which are reported, as is an
    /// operator no one defined.
    fn projection_call(
        &mut self,
        ident: &SyntaxToken,
        list: &ast::NameList,
        expr: &Expr,
        expected: Option<&Expected>,
        errors: &mut Vec<Diagnostic>,
    ) -> Option<hir::Expr> {
        let name = crate::WrittenName::plain(ident.text(), trendy_syntax::span(ident.text_range()));
        self.call(ident, name, Written::Nothing(list), expr, expected, errors)
    }

    /// `project`, written `expr`, a projection, whose operand has a value.
    /// Its names alone could also be the arguments of an operator of the
    /// operand's name, if it has exactly those parameters, and each has a
    /// value here: then, if the projection fits too, nothing says which is
    /// meant (A0079), and if it doesn't, the fix writes the call out.
    fn projection(
        &mut self,
        project: &ast::ProjectExpr,
        expr: &Expr,
        expected: Option<&Expected>,
        errors: &mut Vec<Diagnostic>,
    ) -> Option<hir::Expr> {
        let Some((operand, list)) = self.could_be_a_call(project) else {
            return self.reshaped(expr, expected, errors);
        };
        let mut own = Vec::new();
        let projected = self.reshaped(expr, expected, &mut own);
        if own.is_empty() {
            errors.push(errors::projection_or_call(&operand, &list, expr));
            return None;
        }
        let first = own.remove(0);
        errors.push(errors::as_a_call(first, &operand, &list));
        errors.append(&mut own);
        projected
    }

    /// `project`'s operand as it is written, a name, qualified or not, and its
    /// list, if they could be read as a call: an operator of that name has
    /// exactly those parameters, and each of them names a value.
    fn could_be_a_call(&self, project: &ast::ProjectExpr) -> Option<(String, ast::NameList)> {
        let operand = project.operand()?;
        let (ident, qualifier) = match &operand {
            Expr::Name(name_ref) => (name_ref.ident()?, None),
            Expr::Qualified(written) => (written.ident()?, written.qualifier()),
            _ => return None,
        };
        let list = project.names()?;
        let called = self
            .namespaces()
            .written_name_unreported(&ident, qualifier.as_ref())?;
        let names: Vec<SyntaxToken> = list.names().filter_map(|n| n.ident()).collect();
        let parameters: BTreeSet<&str> = names.iter().map(SyntaxToken::text).collect();
        (!names.is_empty()
            && self.names.operators().takes_exactly(called, &parameters)
            && parameters
                .iter()
                .all(|name| self.names.has_value(name, called.at)))
        .then(|| (operand.syntax().text().to_string(), list))
    }

    /// Whether an argument of `written`, checked on its own, is an array,
    /// as an unknown operator's argument is, to tell which of the prelude's
    /// operators another language's name means. Its errors, if any, are
    /// left for when the call names an operator.
    fn of_array(&mut self, written: Written) -> bool {
        let checked: Vec<hir::Expr> = match written {
            Written::Arguments(args) => args
                .named()
                .filter_map(|a| a.value())
                .chain(args.positional().filter_map(|a| a.value()))
                .filter_map(|value| self.expr(&value, None, &mut Vec::new()))
                .collect(),
            // Names written as a projection's attributes, as in `len{a}`.
            Written::Nothing(list) => list
                .names()
                .filter_map(|n| n.ident())
                .filter_map(|ident| {
                    let span = trendy_syntax::span(ident.text_range());
                    match self.names.resolve(ident.text(), span, &mut Vec::new()) {
                        Lookup::Found(value) => Some(value),
                        Lookup::Reported | Lookup::Unknown => None,
                    }
                })
                .collect(),
        };
        checked
            .iter()
            .any(|value| matches!(self.ty(value), hir::Type::Array(_)))
    }

    /// A call of the operator `ident` names, written `called`, qualified or
    /// not, with `written` arguments, as `expr`: a read-only operator's
    /// value, evaluated when compiling if its arguments are known then.
    /// One whose arguments are known only for each tuple at hand is
    /// evaluated once it is, when a constant operand is folded.
    fn call(
        &mut self,
        ident: &SyntaxToken,
        called: crate::WrittenName,
        written: Written,
        expr: &Expr,
        expected: Option<&Expected>,
        errors: &mut Vec<Diagnostic>,
    ) -> Option<hir::Expr> {
        let name = ident.text();
        let span = trendy_syntax::span(ident.text_range());
        let signature = match self
            .names
            .operators()
            .chosen(called, &argument_names(written))
        {
            Some(Chosen::One(signature)) => signature.clone(),
            Some(Chosen::Tied(tied)) => {
                let units = self.names.namespaces().units();
                errors.push(errors::unchosen_operator(name, &tied, written, units));
                return None;
            }
            None => {
                match (called.unit, qualifier_of(expr)) {
                    (Some(unit), Some(_))
                        if self.namespaces().has_constant_in(name, unit, span) =>
                    {
                        let whole = format!("{}{name}", qualifier_text(expr));
                        errors.push(errors::constant_called(&whole, span, written));
                    }
                    (Some(unit), Some(qualifier)) => {
                        errors.push(errors::unknown_qualified_operator(
                            ident,
                            &qualifier,
                            unit,
                            self.names.namespaces(),
                            self.names.operators(),
                        ));
                    }
                    _ => self.unknown_operator(ident, written, errors),
                }
                return None;
            }
        };
        if !signature.read_only {
            let projectable =
                matches!(written, Written::Nothing(_)) && self.names.has_value(name, span);
            errors.push(errors::operator_call_without_value(
                name,
                span,
                signature.defined(),
                matches!(written, Written::Nothing(list) if list.names().next().is_none()),
                projectable,
                errors::out_result(&signature, written, expr),
            ));
            return None;
        }
        if !signature.valid {
            return None;
        }
        let call = self.arguments(written, &signature, errors)?;
        self.names.calling(&signature, &call.types, span);
        // A built-in's implementation is the compiler's, never checked, but
        // one that makes values of a type the library defines makes each as
        // selecting it would, which checks what that checks first.
        let checks = match signature.builtin {
            // And what it checks of its arguments, which a call is one of
            // too, so a constraint that calls its operator closes a cycle.
            None => std::iter::once(signature.id)
                .chain(signature.constraints.iter().map(|c| c.operator))
                .collect(),
            Some(builtin) => builtin
                .makes(self.namespaces().library())
                .map(|made| self.namespaces().selection_checks(made))
                .unwrap_or_default(),
        };
        if !checks
            .into_iter()
            .all(|operator| self.names.checked(operator, span, errors))
        {
            return None;
        }
        // Its value may compare the values of a type it is given.
        if !call
            .types
            .iter()
            .all(|ty| self.orders_checked(ty, span, errors))
        {
            return None;
        }
        let bindings = self.names.bindings();
        let result = match ops::call(call, bindings) {
            Ok(result) => result,
            Err(failed) => {
                errors.push(errors::call_failure(&failed, expr, &signature));
                return None;
            }
        };
        self.matched(expected, result, expr, errors)
    }

    /// The error for a call of the operator `ident` names, with `written`
    /// arguments, which no operator is named: a value's name, one of a
    /// unit the section could use, or an operator's misspelled, or another
    /// language's.
    fn unknown_operator(
        &mut self,
        ident: &SyntaxToken,
        written: Written,
        errors: &mut Vec<Diagnostic>,
    ) {
        let (name, span) = (ident.text(), trendy_syntax::span(ident.text_range()));
        if self.names.namespaces().constant(name, span).is_some() {
            errors.push(errors::constant_called(name, span, written));
        } else if self.names.has_value(name, span) {
            errors.push(errors::not_an_operator(name, span, "a value"));
        } else if let Some(error) = errors::tutorial_d_test(name, span, written, self.namespaces())
        {
            errors.push(error);
        } else if let Some(dummy) = self
            .names
            .namespaces()
            .dummy(crate::WrittenName::plain(name, span))
        {
            // The names of the arguments, if each has one.
            let named = match written {
                Written::Arguments(arguments) if arguments.positional().next().is_none() => {
                    arguments
                        .named()
                        .map(|a| Some(a.name()?.ident()?.text().to_string()))
                        .collect()
                }
                Written::Arguments(_) => None,
                Written::Nothing(_) => Some(BTreeSet::new()),
            };
            let namespaces = self.names.namespaces();
            errors.push(errors::dummy_selected(
                dummy,
                span,
                namespaces,
                named.as_ref(),
            ));
        } else if let Some(error) =
            errors::unused_unit(ident, Kind::Operator, self.names.namespaces())
        {
            errors.push(error);
        } else {
            let of_array = self.of_array(written);
            errors.push(errors::unknown_operator(
                ident,
                self.names.operators(),
                Some(written),
                of_array,
                self.names.namespaces(),
            ));
        }
    }

    /// Each of the arguments `named`, of `written`, given to the parameter of
    /// `signature`, which are `signature_text`, it says it is for, reporting
    /// those no parameter has, but names alone that nothing says the parameter
    /// of, as they are reported with any unnamed ones.
    fn assigned_to_parameters<'g, 's>(
        &self,
        named: &'g [Given],
        written: Written,
        signature: &'s Signature,
        signature_text: &str,
        errors: &mut Vec<Diagnostic>,
    ) -> ArgumentsGiven<'g, 's> {
        let mut ok = true;
        let mut missing: Vec<&str> = signature
            .written
            .keys()
            .map(String::as_str)
            .filter(|p| !named.iter().any(|given| given.name == *p))
            .collect();
        // The arguments to check, with their parameters.
        let mut pending: Vec<(Expr, &str, &Param)> = Vec::new();
        // Names alone that are no parameter's, which have no name.
        let mut nameless: Vec<&Given> = Vec::new();
        for given in named {
            let (name, span) = (given.name.as_str(), given.span);
            if let Some(first) = named.iter().find(|other| other.name == name)
                && first.span != span
            {
                errors.push(errors::duplicate_attribute(
                    errors::Noun::Argument,
                    name,
                    span,
                    first.span,
                ));
                ok = false;
                continue;
            }
            let Some(param) = signature.written.get(name) else {
                ok = false;
                if given.alone {
                    // Its value stays, for another language's name for one.
                    // No value of the name, it may be a misspelling of one.
                    // Otherwise, nothing says which it is for, but what's
                    // left when it is the only one.
                    let elsewhere = signature.named_elsewhere(name, missing.iter().copied());
                    let misspelled = (!self.names.has_value(name, span))
                        .then(|| trendy_diag::closest(name, missing.iter().copied()))
                        .flatten();
                    if let Some(target) = elsewhere.or(misspelled) {
                        missing.retain(|m| *m != target);
                    }
                    match (elsewhere, misspelled) {
                        (Some(target), _) => errors.push(errors::name_alone_for(
                            name,
                            span,
                            Some(target),
                            signature_text,
                        )),
                        (None, Some(target)) => errors.push(errors::misspelled_name_alone(
                            name,
                            span,
                            target,
                            signature_text,
                        )),
                        (None, None) => nameless.push(given),
                    }
                    continue;
                }
                // Another language's name for one, or a misspelling.
                let renamed = signature.meant(name, missing.iter().copied());
                if let Some(target) = renamed {
                    missing.retain(|m| *m != target);
                }
                let (Some(attribute), Written::Arguments(args)) = (&given.attribute, written)
                else {
                    unreachable!("only arguments in braces are written out");
                };
                errors.push(errors::unknown_attribute(
                    errors::Noun::Argument,
                    name,
                    span,
                    &format!("the parameters are {signature_text}"),
                    renamed,
                    Some(args.removal(attribute)),
                ));
                continue;
            };
            if let Some(attribute) = &given.attribute {
                ok &= marked(attribute, name, param, signature, errors);
            }
            match &given.value {
                Some(value) => pending.push((value.clone(), name, param)),
                None => ok = false,
            }
        }
        ArgumentsGiven {
            pending,
            nameless,
            missing,
            valid: ok,
        }
    }

    /// A call of the operator `signature`, with `written` arguments for its
    /// parameters: each named, each parameter given one, and each of its
    /// parameter's type, once each type parameter in that type is given
    /// the type of an argument, the same for every argument that gives it
    /// one (see [`TypeArguments`]).
    pub(crate) fn arguments(
        &mut self,
        written: Written,
        signature: &Signature,
        errors: &mut Vec<Diagnostic>,
    ) -> Option<hir::Call> {
        let signature_text = self.names.operators().described(signature, written.span());
        let mut ok = true;

        let (named, positional): (Vec<Given>, Vec<ast::PositionalArg>) = match written {
            Written::Arguments(args) => (
                args.named().filter_map(Given::written).collect(),
                args.positional().collect(),
            ),
            Written::Nothing(list) => (
                list.names()
                    .filter_map(|name| Given::alone(&name))
                    .collect(),
                Vec::new(),
            ),
        };
        let ArgumentsGiven {
            pending,
            nameless,
            missing,
            valid,
        } = self.assigned_to_parameters(&named, written, signature, &signature_text, errors);
        ok &= valid;

        let variables: BTreeMap<&str, (Expr, hir::Mode)> = pending
            .iter()
            .filter_map(|(value, name, p)| match p {
                Param::Variable(_, mode) => Some((*name, (value.clone(), *mode))),
                _ => None,
            })
            .collect();
        let checked = self.typed_arguments(pending, signature, errors);
        // A lone unnamed argument can only be for the one parameter no
        // other argument names.
        let unnamed = positional.len() + nameless.len();
        let only_param = match missing.as_slice() {
            [param] if unnamed == 1 => Some(*param),
            _ => None,
        };
        for arg in &positional {
            ok = false;
            errors.push(errors::positional_argument(
                arg.span(),
                only_param,
                &signature_text,
            ));
        }
        for given in &nameless {
            errors.push(errors::name_alone_for(
                &given.name,
                given.span,
                only_param,
                &signature_text,
            ));
        }
        // Unnamed arguments are for parameters without them, so those
        // aren't missing.
        if !missing.is_empty() && unnamed == 0 {
            errors.push(missing_arguments(
                &missing,
                signature,
                &signature_text,
                written,
            ));
            ok = false;
        }
        // Every type parameter has a type once every argument is given.
        let (types, mut args) = checked?;
        let updated = updated(&variables, &mut args, errors)?;
        ok.then(|| hir::Call {
            operator: signature.id,
            types: types.finish(),
            args,
            updated,
        })
    }

    /// The arguments `pending`, each written with the name of its
    /// parameter, of the operator `signature`, each checked once the type
    /// parameters in its parameter's type are given types, and a `per`
    /// one once its relation is, for each of its tuples; and those types.
    /// The arguments are those the operator sees: each relation's carries
    /// its `per` arguments' values as attributes of its tuples. `None`
    /// after reporting why some argument is invalid.
    fn typed_arguments<'s>(
        &mut self,
        mut pending: Vec<(Expr, &str, &'s Param)>,
        signature: &'s Signature,
        errors: &mut Vec<Diagnostic>,
    ) -> Option<(TypeArguments<'s>, BTreeMap<String, hir::Expr>)> {
        let mut types = TypeArguments::new(signature);
        let mut args = BTreeMap::new();
        // Each relation's argument, as written, and its tuples' heading,
        // and each `per` argument, as written and checked.
        let mut relations: BTreeMap<String, (Expr, hir::Heading)> = BTreeMap::new();
        let mut per: BTreeMap<String, (Expr, hir::Expr)> = BTreeMap::new();
        let mut ok = true;
        loop {
            let stage = |param: &Param| match param {
                Param::Per { relation, .. } if !relations.contains_key(relation) => Stage::Waiting,
                Param::Value(ty) | Param::Variable(ty, _) | Param::Per { ty, .. }
                    if types.know(ty) =>
                {
                    Stage::Ready
                }
                Param::Value(hir::Type::Parameter(_))
                | Param::Variable(hir::Type::Parameter(_), _)
                | Param::Per {
                    ty: hir::Type::Parameter(_),
                    ..
                }
                | Param::Relation(_) => Stage::Bare,
                Param::Value(_) | Param::Variable(..) | Param::Per { .. } => Stage::Structured,
            };
            // Those whose parameters' types are known are checked against
            // them. An unknown type parameter expects nothing of its
            // argument, which is checked on its own and gives it a type, so
            // those come next; others then expect nothing either.
            let Some(now) = [Stage::Ready, Stage::Bare, Stage::Structured]
                .into_iter()
                .find(|now| pending.iter().any(|(_, _, p)| stage(p) == *now))
            else {
                // Only `per` arguments of relations with errors are left.
                break;
            };
            let (checked_now, rest): (Vec<_>, Vec<_>) =
                pending.into_iter().partition(|(_, _, p)| stage(p) == now);
            pending = rest;
            for (value, name, param) in checked_now {
                let checked = match param {
                    Param::Value(ty) => {
                        self.typed_argument(&value, name, ty, None, &mut types, errors)
                    }
                    // A variable of exactly the parameter's type, which the
                    // operator can give any value of it (*Type Inheritance
                    // and Relational Theory*, IM Prescription 19), as it is
                    // declared.
                    Param::Variable(ty, _) => {
                        let mut variables = crate::narrowed::Unnarrowed {
                            outer: &mut *self.names,
                        };
                        let checked = Checker::new(&mut variables)
                            .typed_argument(&value, name, ty, None, &mut types, errors);
                        match checked {
                            Some(hir::Expr::Treat(declared, variable)) => {
                                let actual = self.ty(&variable);
                                errors.push(errors::variable_subtype(
                                    &declared,
                                    &actual,
                                    &signature.param_origin(name),
                                    &value,
                                ));
                                None
                            }
                            checked => checked,
                        }
                    }
                    Param::Per { relation, ty } => {
                        let heading = &relations[relation].1;
                        let checked = self.typed_argument(
                            &value,
                            name,
                            ty,
                            Some((relation, heading)),
                            &mut types,
                            errors,
                        );
                        ok &= checked.is_some();
                        if let Some(checked) = checked {
                            per.insert(name.to_string(), (value, checked));
                        }
                        continue;
                    }
                    Param::Relation(wrapped) => {
                        let checked = self.expr(&value, None, errors);
                        let Some((checked, heading)) = checked.and_then(|checked| {
                            self.relation_argument(checked, &value, name, signature, errors)
                        }) else {
                            ok = false;
                            continue;
                        };
                        types.give(wrapped, hir::Type::Tuple(heading.clone()), &value);
                        relations.insert(name.to_string(), (value, heading));
                        Some(checked)
                    }
                };
                match checked {
                    Some(checked) => {
                        args.insert(name.to_string(), checked);
                    }
                    None => ok = false,
                }
            }
        }
        ok &= pending.is_empty();
        ok &= self.relations_seen(relations, &per, &mut args, signature, errors);
        ok.then_some((types, args))
    }

    /// `checked`, the argument written `value` for the parameter `name` of
    /// the operator `signature`, which `per` parameters refer to, with the
    /// heading of its tuples, or `None` after reporting why it isn't a
    /// relation of a heading known here.
    fn relation_argument(
        &self,
        checked: hir::Expr,
        value: &Expr,
        name: &str,
        signature: &Signature,
        errors: &mut Vec<Diagnostic>,
    ) -> Option<(hir::Expr, hir::Heading)> {
        let actual = self.ty(&checked);
        let hir::Type::Relation(heading) = actual else {
            errors.push(errors::per_relation_argument(
                name,
                &signature.param_origin(name),
                &actual,
                value,
            ));
            return None;
        };
        Some((checked, heading))
    }

    /// Replace each relation's argument in `args`, written and of tuples
    /// of the heading in `relations`, with the relation the operator
    /// `signature` sees, carrying the values of its `per` arguments, in
    /// `per`, as attributes of its tuples (see [`Checker::relation_seen`]).
    /// Whether every one has its `per` arguments, and none fails for a
    /// tuple, after reporting it.
    fn relations_seen(
        &self,
        relations: BTreeMap<String, (Expr, hir::Heading)>,
        per: &BTreeMap<String, (Expr, hir::Expr)>,
        args: &mut BTreeMap<String, hir::Expr>,
        signature: &Signature,
        errors: &mut Vec<Diagnostic>,
    ) -> bool {
        let mut ok = true;
        for (relation, (written, heading)) in relations {
            let Some(arg) = args.remove(&relation) else {
                continue;
            };
            let names: Vec<&str> = signature
                .written
                .iter()
                .filter(|(_, p)| matches!(p, Param::Per { relation: r, .. } if *r == relation))
                .map(|(name, _)| name.as_str())
                .collect();
            if !names.iter().all(|n| per.contains_key(*n)) {
                ok = false;
                continue;
            }
            match self.relation_seen(&relation, arg, &heading, &names, per) {
                Ok(seen) => {
                    args.insert(relation, seen);
                }
                Err(failure) => {
                    let at = failure
                        .attribute
                        .as_ref()
                        .and_then(|a| per.get(a))
                        .map_or(&written, |(written, _)| written);
                    errors.push(errors::extension_failure(&failure, at));
                    ok = false;
                }
            }
        }
        ok
    }

    /// The argument `value` for the parameter `name`, of type `ty`, as the
    /// type parameters in it are known so far in `types`, or, if they
    /// aren't, giving them their types, or `None` after reporting why it
    /// is invalid. A `per` argument, of the parameter `relation`, has the
    /// attributes of the tuples of its relation's argument, `heading`, in
    /// scope, and must use them: otherwise it is the same for every tuple.
    fn typed_argument(
        &mut self,
        value: &Expr,
        name: &str,
        ty: &hir::Type,
        per: Option<(&str, &hir::Heading)>,
        types: &mut TypeArguments,
        errors: &mut Vec<Diagnostic>,
    ) -> Option<hir::Expr> {
        let expected = types.know(ty).then(|| types.expected(name, ty));
        let checked = match per {
            Some((relation, heading)) => {
                // An attribute named in quotes, whatever the type expected.
                if let Some(error) = errors::quoted_attribute(value, name, relation, heading) {
                    errors.push(error);
                    return None;
                }
                let mut names = TupleNames::new(&mut *self.names, heading.clone());
                Checker::new(&mut names).expr(value, expected.as_ref(), errors)?
            }
            None => self.expr(value, expected.as_ref(), errors)?,
        };
        if expected.is_none() {
            let actual = self.ty(&checked);
            let namespaces = self.names.namespaces();
            if let Err(error) = types.bind(name, ty, &actual, value, namespaces) {
                let constant = Self::constant(&checked);
                let builtin = types.signature.builtin.and_then(hir::Builtin::read_only);
                errors.push(error.diagnostic(&actual, constant, value, namespaces, builtin));
                return None;
            }
        }
        if let Some((relation, heading)) = per
            && !ops::uses_tuple(&checked, 0)
        {
            errors.push(errors::constant_per(name, relation, value, heading));
            return None;
        }
        Some(checked)
    }

    /// The relation the operator sees for the parameter `relation`, whose
    /// argument is `arg`, of tuples of `heading`: its tuples with the
    /// values of the `per` parameters `names`, from `per`, as attributes,
    /// beside their own attributes, wrapped in one more. Those named as a
    /// `per` parameter are replaced, and the wrapped one may have the name
    /// of one the argument's tuples wrap themselves.
    ///
    /// # Errors
    ///
    /// Returns why a `per` argument fails for a tuple of a constant `arg`.
    fn relation_seen(
        &self,
        relation: &str,
        arg: hir::Expr,
        heading: &hir::Heading,
        names: &[&str],
        per: &BTreeMap<String, (Expr, hir::Expr)>,
    ) -> Result<hir::Expr, ops::TupleFailure> {
        let wrapped = hir::wrapped(relation);
        let at_hand = hir::Expr::Current(0, heading.clone());
        let own: BTreeMap<String, hir::Expr> = heading
            .attributes()
            .map(|(name, _)| {
                let value = ops::extract(at_hand.clone(), name.to_string());
                (name.to_string(), value)
            })
            .collect();
        let mut computed = BTreeMap::from([(wrapped.clone(), hir::Type::Tuple(heading.clone()))]);
        let mut attributes = BTreeMap::from([(wrapped.clone(), ops::tuple(own))]);
        for name in names {
            let checked = per[*name].1.clone();
            computed.insert((*name).to_string(), self.ty(&checked));
            attributes.insert((*name).to_string(), checked);
        }
        let dropped = heading
            .attributes()
            .map(|(name, _)| name.to_string())
            .filter(|name| !computed.contains_key(name))
            .collect();
        let extended = ops::extend(
            arg,
            hir::Heading::new(computed),
            attributes,
            self.bindings(),
        )?;
        Ok(ops::remove(extended, dropped))
    }

    /// `postfix`, `r tclose`, written `expr`: the transitive closure of
    /// `r`, a relation of two attributes of one type (TTM's TCLOSE). `None`
    /// after reporting why it isn't one.
    fn closed(
        &mut self,
        postfix: &ast::PostfixExpr,
        expr: &Expr,
        expected: Option<&Expected>,
        errors: &mut Vec<Diagnostic>,
    ) -> Option<hir::Expr> {
        let (operand_expr, keyword) = (postfix.operand()?, postfix.operator()?);
        let operand = self.expr(&operand_expr, None, errors)?;
        let ty = self.ty(&operand);
        let hir::Type::Relation(heading) = &ty else {
            let at = trendy_syntax::span(keyword.text_range());
            errors.push(errors::not_relational("tclose", at, &ty, &operand_expr));
            return None;
        };
        let types: Vec<&hir::Type> = heading.attributes().map(|(_, ty)| ty).collect();
        if !matches!(types[..], [a, b] if a == b) {
            errors.push(errors::tclose_operand(&keyword, &operand_expr, heading));
            return None;
        }
        self.matched(expected, ops::tclose(operand), expr, errors)
    }

    /// `postfix`, an `extend` or a `replace`, as `assigning` says, written
    /// `expr`.
    fn assigned(
        &mut self,
        postfix: &ast::PostfixExpr,
        assigning: ops::Assigning,
        expr: &Expr,
        expected: Option<&Expected>,
        errors: &mut Vec<Diagnostic>,
    ) -> Option<hir::Expr> {
        let operand_expr = postfix.operand()?;
        let operand = self.expr(&operand_expr, None, errors)?;
        let ty = self.ty(&operand);
        let noun = errors::Noun::assigned(&ty);
        let list = postfix.assignments()?;
        let keyword = postfix.operator()?;
        let assigned = Assigned {
            assigning,
            keyword: &keyword,
            list: &list,
            operand: &operand_expr,
        };
        let replaced = self.assign_attributes(
            operand.clone(),
            &assigned,
            |why| match why {
                Unchanged::Empty => errors::empty_assign(assigning, noun, postfix, &operand_expr),
                Unchanged::OwnValues => errors::own_values_replaced(noun, postfix, &operand_expr),
            },
            errors,
        )?;
        // A type's value is selected from its stored components, replaced,
        // or in another representation, from the operand's.
        let result = match (replaced, &ty) {
            (Replacement::Attributes(components), hir::Type::Defined(defined)) => {
                self.selected(defined, components, expr, errors)?
            }
            (Replacement::Representation(replaced), hir::Type::Defined(defined)) => {
                self.selected_in(defined, operand, replaced, expr, errors)?
            }
            (Replacement::Attributes(result), _) => result,
            (Replacement::Representation(_), _) => {
                unreachable!("only a type's value is selected in another representation")
            }
        };
        self.matched(expected, result, expr, errors)
    }

    /// `extract`, written `expr`: the value of a tuple's attribute, or of a
    /// component of a value of a type of the program's own, extracted from
    /// its components, or, of another representation than the stored one,
    /// from those its conversion computes from them.
    fn extracted(
        &mut self,
        extract: &ast::ExtractExpr,
        expr: &Expr,
        expected: Option<&Expected>,
        errors: &mut Vec<Diagnostic>,
    ) -> Option<hir::Expr> {
        let (operand, ty, name) = self.extraction(extract, errors)?;
        let value = match &ty {
            hir::Type::Defined(defined) if defined.components.get(&name).is_none() => {
                self.converted_component(defined, operand, &name, expr, errors)?
            }
            hir::Type::Defined(_) => ops::extract(ops::components(operand), name),
            _ => ops::extract(operand, name),
        };
        self.matched(expected, value, expr, errors)
    }

    /// `extract`, `x.f`: the value `x`, its type, and `f`, if `x` has an
    /// attribute or a component of that name, and otherwise reported, as a
    /// method call if an operator is named `f`.
    fn extraction(
        &mut self,
        extract: &ast::ExtractExpr,
        errors: &mut Vec<Diagnostic>,
    ) -> Option<(hir::Expr, hir::Type, String)> {
        let operand_expr = extract.operand()?;
        let operand = self.expr(&operand_expr, None, errors)?;
        let ty = self.ty(&operand);
        let others = Self::other_components(&ty);
        let others: Vec<&hir::Heading> = others.iter().collect();
        if let Some(error) = self.method_call(extract, &operand_expr, &ty, &others) {
            errors.push(error);
            return None;
        }
        // Where narrowing a name ended, for the error that needs it.
        let ended = match unparenthesized(&operand_expr) {
            Expr::Name(name) => name
                .ident()
                .and_then(|ident| self.names.narrowing_ended(ident.text())),
            _ => None,
        };
        let namespaces = self.namespaces();
        let name = ops::extracted(
            extract,
            (&operand_expr, ended),
            &ty,
            &others,
            namespaces,
            errors,
        )?;
        Some((operand, ty, name))
    }

    /// `call`, `x.f{a: 1}`: arguments after `x.f`, as object-oriented
    /// languages call a method. The extraction reports a method call if
    /// `f` is an operator, and an attribute's or a component's value takes
    /// no arguments.
    fn method_called(
        &mut self,
        call: &ast::MethodCallExpr,
        errors: &mut Vec<Diagnostic>,
    ) -> Option<hir::Expr> {
        let method = call.method()?;
        let (_, ty, _) = self.extraction(&method, errors)?;
        let what = match ty {
            hir::Type::Defined(_) => "a component's value",
            _ => "an attribute's value",
        };
        errors.push(errors::not_an_operator(
            &method.syntax().text().to_string(),
            method.span(),
            what,
        ));
        None
    }

    /// The components of the possible representations of `ty`, if it is a
    /// type of the program's own, but the stored one: of a subtype, its
    /// supertype's too, as its values are its supertype's.
    fn other_components(ty: &hir::Type) -> Vec<hir::Heading> {
        match ty {
            hir::Type::Defined(defined) => defined
                .representations
                .iter()
                .map(|r| r.components.clone())
                .collect(),
            _ => Vec::new(),
        }
    }

    /// The component `name` of `value`, written `expr`, of the type of the
    /// program's own `defined`, of a possible representation other than the
    /// stored one: computed by its conversion from the stored components.
    /// `None` after reporting that the conversion fails, or if it has
    /// errors, already reported.
    pub(crate) fn converted_component(
        &mut self,
        defined: &hir::Defined,
        value: hir::Expr,
        name: &str,
        expr: &Expr,
        errors: &mut Vec<Diagnostic>,
    ) -> Option<hir::Expr> {
        let components = ops::components(value);
        let (_, other) = defined.representation(name)?;
        let (converted, _) = self.converted(defined, components, other?, expr, errors)?;
        Some(ops::extract(converted, name.to_string()))
    }

    /// The components, in the possible representation `other` of the type
    /// of the program's own `defined`, of the value whose stored components
    /// are `components`, written `expr`: a tuple of them, computed by its
    /// conversion from the stored ones, and its heading. `None` after
    /// reporting that the conversion fails, or if it has errors, already
    /// reported.
    fn converted(
        &mut self,
        defined: &hir::Defined,
        components: hir::Expr,
        other: &hir::Representation,
        expr: &Expr,
        errors: &mut Vec<Diagnostic>,
    ) -> Option<(hir::Expr, hir::Heading)> {
        let (conversion, own) = (other.from, other.components.clone());
        let result = hir::Type::Tuple(own.clone());
        let converted = self.applied(
            conversion,
            result,
            components,
            &defined.components,
            expr,
            errors,
        )?;
        Some((converted, own))
    }

    /// The value of the type of the program's own `defined` selected in the
    /// representation `replaced` is of, from its components, which a
    /// `replace` or an `update` replaced, computed from the stored
    /// components of `value`. `None` after reporting that it fails, written
    /// `expr`, as a selection breaking a constraint does.
    pub(crate) fn selected_in(
        &mut self,
        defined: &hir::Defined,
        value: hir::Expr,
        replaced: Replaced,
        expr: &Expr,
        errors: &mut Vec<Diagnostic>,
    ) -> Option<hir::Expr> {
        let ty = hir::Type::Defined(Box::new(defined.clone()));
        let Replaced {
            selector,
            components,
            tuple,
        } = replaced;
        // From the stored components at hand,
        let selected = self.applied(selector, ty.clone(), tuple, &components, expr, errors)?;
        // which are `value`'s.
        self.computed_from(ops::components(value), ty, selected, expr, errors)
    }

    /// The value, of type `result`, of a call of `operator` with each
    /// attribute of `tuple`, of `heading`, as the argument of the parameter
    /// of its name: computed in an `Extend` of `tuple`, so that `tuple` is
    /// computed once. `None` after reporting that the call, written `expr`,
    /// fails, or if the operator has errors, already reported.
    fn applied(
        &mut self,
        operator: hir::OperatorId,
        result: hir::Type,
        tuple: hir::Expr,
        heading: &hir::Heading,
        expr: &Expr,
        errors: &mut Vec<Diagnostic>,
    ) -> Option<hir::Expr> {
        if !self.names.checked(operator, expr.span(), errors) {
            return None;
        }
        let call = called_with(operator, heading);
        self.computed_from(tuple, result, call, expr, errors)
    }

    /// The value, of type `result`, that `computed` computes from `tuple`
    /// at hand, its `Current(0)`: in an `Extend` of `tuple`, so that `tuple`
    /// is computed once. `None` after reporting that it fails, written
    /// `expr`.
    fn computed_from(
        &self,
        tuple: hir::Expr,
        result: hir::Type,
        computed: hir::Expr,
        expr: &Expr,
        errors: &mut Vec<Diagnostic>,
    ) -> Option<hir::Expr> {
        let wrapped = hir::wrapped("as");
        let extension = hir::Heading::new([(wrapped.clone(), result)].into());
        let values = [(wrapped.clone(), computed)].into();
        match ops::extend(tuple, extension, values, self.bindings()) {
            Ok(extended) => Some(ops::extract(extended, wrapped)),
            Err(failure) => {
                errors.push(errors::failed(&failure.failed, expr));
                None
            }
        }
    }

    /// The value of the type of the program's own `defined` whose
    /// components are `components`, selected by `expr`, which fails unless
    /// each of its relation-valued components' keys, and each of the
    /// type's constraints, is true of them: evaluated if they are known
    /// when compiling. Each constraint, and each conversion that computes
    /// a keyed component, is checked first if it isn't yet. `None` after
    /// reporting a key or a constraint they break, or one with errors.
    pub(crate) fn selected(
        &mut self,
        defined: &hir::Defined,
        components: hir::Expr,
        expr: &Expr,
        errors: &mut Vec<Diagnostic>,
    ) -> Option<hir::Expr> {
        let at = expr.span();
        let checks = self.namespaces().selection_checks(defined);
        if !checks
            .into_iter()
            .all(|operator| self.names.checked(operator, at, errors))
        {
            return None;
        }
        match ops::select(Box::new(defined.clone()), components, self.bindings()) {
            Ok(value) => Some(value),
            Err(failed) => {
                errors.push(errors::failed(&failed, expr));
                None
            }
        }
    }

    /// `operand`, written `assigned.operand`, with the attributes in
    /// `assigned.list` added, or given new values, as `assigned.assigning`
    /// says: each attribute's value is checked with the attributes of the
    /// tuple at hand in scope. A `replace`'s values have the types of the
    /// attributes they replace, and are computed from the old values, all
    /// at once. A `replace` or an `update` of a value of a type of the
    /// program's own gives its components new values in the same way, those
    /// of one of its possible representations (TTM chapter 5's scalar
    /// update): the result is the tuple of them, which the value is
    /// selected from, of another than the stored one computed from the
    /// stored components at hand, which [`Checker::selected_in`] computes
    /// from the operand's. An attribute
    /// given its own value is an error, and `unchanged` is the error for a
    /// list that leaves the operand as it is.
    pub(crate) fn assign_attributes(
        &mut self,
        operand: hir::Expr,
        assigned: &Assigned,
        unchanged: impl FnOnce(Unchanged) -> Diagnostic,
        errors: &mut Vec<Diagnostic>,
    ) -> Option<Replacement> {
        let Assigned {
            assigning,
            keyword,
            list,
            operand: operand_expr,
        } = *assigned;
        let ty = self.ty(&operand);
        let assignments: Vec<(ast::Assign, String, Span)> = list
            .assignments()
            .filter_map(|assign| {
                let ident = assign.name()?.ident()?;
                let span = trendy_syntax::span(ident.text_range());
                Some((assign, ident.text().to_string(), span))
            })
            .collect();
        let (heading, operand, changed) =
            self.changed(&ty, operand, &assignments, assigned, errors)?;
        let noun = errors::Noun::assigned(&ty);
        if list.assignments().next().is_none() {
            errors.push(unchanged(Unchanged::Empty));
            return None;
        }
        // When every name is wrong for an `extend` or a `replace` of a
        // tuple or relation, the other one is likely what was meant.
        let keyword = trendy_syntax::span(keyword.text_range());
        let other = (assigning != ops::Assigning::Update
            && matches!(changed, Changed::Attributes)
            && assignments.iter().all(|(_, name, _)| {
                heading.get(name).is_some() == (assigning == ops::Assigning::Extend)
            }))
        .then_some(keyword);
        let (representations, candidates) = components_known(&changed, &heading);
        let mut ok = true;
        let mut values = BTreeMap::new();
        let mut types = BTreeMap::new();
        for (assign, name, span) in &assignments {
            if let Some(&(_, _, first)) = assignments.iter().find(|(_, n, _)| n == name)
                && first != *span
            {
                errors.push(errors::duplicate_attribute(noun, name, *span, first));
                ok = false;
                continue;
            }
            let assigned = Named {
                assigning,
                name,
                span: *span,
                candidates: &candidates,
                representations: &representations,
            };
            let (valid, expected) =
                assigned_name(&assigned, &ty, &heading, operand_expr, other, errors);
            ok &= valid;
            let written = assign.value();
            match self.assigned_value(&changed, &heading, written, expected.as_ref(), errors) {
                Some((value, ty)) => {
                    types.insert(name.clone(), ty);
                    values.insert(name.clone(), value);
                }
                None => ok = false,
            }
        }
        if !ok || !changes_something(&assignments, &values, list, unchanged, errors) {
            return None;
        }
        let replaced = match ops::extend(operand, hir::Heading::new(types), values, self.bindings())
        {
            Ok(result) => result,
            Err(failure) => {
                let value = assignments
                    .iter()
                    .find(|(_, name, _)| Some(name) == failure.attribute.as_ref())
                    .and_then(|(assign, _, _)| assign.value())?;
                errors.push(errors::extension_failure(&failure, &value));
                return None;
            }
        };
        match changed {
            Changed::Attributes | Changed::Stored(..) => Some(Replacement::Attributes(replaced)),
            Changed::Other(_, selector) => Some(Replacement::Representation(Replaced {
                selector,
                components: heading,
                tuple: replaced,
            })),
        }
    }

    /// What `assignments`, of `assigned`, give new values in `operand`, of
    /// type `ty`: its attributes, or the components of one of its possible
    /// representations, if it is a value of a type of the program's own,
    /// with their heading and the tuple of them. Those of another than the
    /// stored one are computed from the stored components at hand, which
    /// the caller gives (`selected_in`). `None` after reporting why there
    /// are none to give values.
    fn changed<'t>(
        &mut self,
        ty: &'t hir::Type,
        operand: hir::Expr,
        assignments: &[(ast::Assign, String, Span)],
        assigned: &Assigned,
        errors: &mut Vec<Diagnostic>,
    ) -> Option<(hir::Heading, hir::Expr, Changed<'t>)> {
        let assigning = assigned.assigning;
        match (ty, assigning) {
            (hir::Type::Tuple(heading) | hir::Type::Relation(heading), _) => {
                Some((heading.clone(), operand, Changed::Attributes))
            }
            (hir::Type::Defined(defined), ops::Assigning::Replace | ops::Assigning::Update) => {
                let named: Vec<(&str, Span)> = assignments
                    .iter()
                    .map(|(_, name, span)| (name.as_str(), *span))
                    .collect();
                let rule = errors::OneRepresentation::Changed;
                match representation(defined, &named, rule, errors)? {
                    Representation::Stored { known } => Some((
                        defined.components.clone(),
                        ops::components(operand),
                        Changed::Stored(defined, known),
                    )),
                    Representation::Other(at) => {
                        let other = &defined.representations[at];
                        let selector = self
                            .names
                            .operators()
                            .selector_of(defined, &other.components)?;
                        let at_hand = hir::Expr::Current(0, defined.components.clone());
                        let (converted, own) =
                            self.converted(defined, at_hand, other, assigned.operand, errors)?;
                        Some((own, converted, Changed::Other(defined, selector)))
                    }
                }
            }
            _ => {
                let at = trendy_syntax::span(assigned.keyword.text_range());
                errors.push(errors::not_projectable(
                    Some(assigning.keyword()),
                    at,
                    ty,
                    assigned.operand,
                ));
                None
            }
        }
    }

    /// The value `written`, of an assignment in a list that gives new
    /// values to what `changed` says, of `heading`, checked as `expected`
    /// says, and its type. In scope: a tuple's attributes, or a type's
    /// value's components, of any representation, and first those of the
    /// one the list gives new values, if it isn't the stored one.
    fn assigned_value(
        &mut self,
        changed: &Changed,
        heading: &hir::Heading,
        written: Option<Expr>,
        expected: Option<&Expected>,
        errors: &mut Vec<Diagnostic>,
    ) -> Option<(hir::Expr, hir::Type)> {
        match changed {
            Changed::Attributes => {
                let mut names = TupleNames::new(&mut *self.names, heading.clone());
                checked_value(&mut names, written, expected, errors)
            }
            Changed::Stored(defined, _) => {
                let mut names = ComponentNames::new(&mut *self.names, (*defined).clone());
                checked_value(&mut names, written, expected, errors)
            }
            Changed::Other(defined, _) => {
                let mut components = ComponentNames::new(&mut *self.names, (*defined).clone());
                let mut names = TupleNames::new(&mut components, heading.clone());
                checked_value(&mut names, written, expected, errors)
            }
        }
    }

    /// `postfix`, a `group` or a `wrap`, as `gathering` says, written
    /// `expr`: each gathering's attributes made into a new attribute. A
    /// `group` makes relation-valued ones, of the tuples that share the
    /// attributes no grouping gathers, and takes a relation; a `wrap`
    /// makes tuple-valued ones, of each tuple's own, and takes a tuple or a
    /// relation. Several gatherings are simultaneous.
    fn gathered(
        &mut self,
        postfix: &ast::PostfixExpr,
        gathering: ops::Gathering,
        expr: &Expr,
        expected: Option<&Expected>,
        errors: &mut Vec<Diagnostic>,
    ) -> Option<hir::Expr> {
        let operand_expr = postfix.operand()?;
        let operand = self.expr(&operand_expr, None, errors)?;
        let ty = self.ty(&operand);
        let keyword = trendy_syntax::span(postfix.operator()?.text_range());
        let heading = match (gathering, &ty) {
            (ops::Gathering::Group, hir::Type::Relation(heading))
            | (ops::Gathering::Wrap, hir::Type::Tuple(heading) | hir::Type::Relation(heading)) => {
                heading
            }
            (ops::Gathering::Group, hir::Type::Tuple(_)) => {
                errors.push(errors::not_relational("group", keyword, &ty, &operand_expr));
                return None;
            }
            _ => {
                let operator = Some(gathering.keyword());
                errors.push(errors::not_projectable(
                    operator,
                    keyword,
                    &ty,
                    &operand_expr,
                ));
                return None;
            }
        };
        let list = postfix.gatherings()?;
        if list.gatherings().next().is_none() {
            errors.push(errors::empty_gathering(gathering, postfix, &operand_expr));
            return None;
        }
        let gathered = gatherings(gathering, &list, &ty, &operand_expr, errors)?;
        let result = match gathering {
            ops::Gathering::Group => {
                match ops::group(operand, heading, gathered, self.bindings()) {
                    Ok(result) => result,
                    Err(failure) => {
                        errors.push(errors::extension_failure(&failure, &operand_expr));
                        return None;
                    }
                }
            }
            ops::Gathering::Wrap => ops::wrap(operand, heading, gathered, self.bindings()),
        };
        self.matched(expected, result, expr, errors)
    }

    /// `postfix`, an `ungroup` or an `unwrap`, as `unpacking` says, written
    /// `expr`: the named attributes replaced by their own attributes. An
    /// `ungroup` takes a relation and relation-valued attributes, and gives
    /// each tuple one for each combination of a tuple of each; an `unwrap`
    /// takes a tuple or a relation and tuple-valued attributes.
    fn unpacked(
        &mut self,
        postfix: &ast::PostfixExpr,
        unpacking: ops::Unpacking,
        expr: &Expr,
        expected: Option<&Expected>,
        errors: &mut Vec<Diagnostic>,
    ) -> Option<hir::Expr> {
        let operand_expr = postfix.operand()?;
        let operand = self.expr(&operand_expr, None, errors)?;
        let ty = self.ty(&operand);
        let keyword = trendy_syntax::span(postfix.operator()?.text_range());
        let heading = match (unpacking, &ty) {
            (ops::Unpacking::Ungroup, hir::Type::Relation(heading))
            | (ops::Unpacking::Unwrap, hir::Type::Tuple(heading) | hir::Type::Relation(heading)) => {
                heading
            }
            (ops::Unpacking::Ungroup, hir::Type::Tuple(_)) => {
                errors.push(errors::not_relational(
                    "ungroup",
                    keyword,
                    &ty,
                    &operand_expr,
                ));
                return None;
            }
            _ => {
                let operator = Some(unpacking.keyword());
                errors.push(errors::not_projectable(
                    operator,
                    keyword,
                    &ty,
                    &operand_expr,
                ));
                return None;
            }
        };
        let list = postfix.names()?;
        if list.names().next().is_none() {
            errors.push(errors::empty_unpacking(unpacking, postfix, &operand_expr));
            return None;
        }
        let names = unpackings(unpacking, &list, &ty, errors)?;
        let result = match unpacking {
            ops::Unpacking::Ungroup => ops::ungroup(operand, names),
            ops::Unpacking::Unwrap => ops::unwrap(operand, heading, &names, self.bindings()),
        };
        self.matched(expected, result, expr, errors)
    }

    /// `from`, a `tuple from` or a `value from`, written `expr`: the only
    /// tuple of its operand, a relation, or the value of that tuple's only
    /// attribute.
    fn taken(
        &mut self,
        from: &ast::FromExpr,
        expr: &Expr,
        expected: Option<&Expected>,
        errors: &mut Vec<Diagnostic>,
    ) -> Option<hir::Expr> {
        let operand_expr = from.operand()?;
        let operand = self.expr(&operand_expr, None, errors)?;
        let ty = self.ty(&operand);
        let keywords = from.keywords().map(|t| trendy_syntax::span(t.text_range()));
        let span = keywords.reduce(|a, b| Span::new(a.start, b.end))?;
        let taken = from.taken();
        let operator = match taken {
            ast::Taken::Tuple => "tuple from",
            ast::Taken::Value => "value from",
        };
        let heading = match &ty {
            hir::Type::Relation(heading) => heading,
            hir::Type::Tuple(heading) => {
                errors.push(errors::taken_from_tuple(
                    taken,
                    span,
                    expr,
                    &operand_expr,
                    heading,
                ));
                return None;
            }
            hir::Type::Scalar(_)
            | hir::Type::Defined(_)
            | hir::Type::Dummy(_)
            | hir::Type::Array(_)
            | hir::Type::Parameter(_) => {
                errors.push(errors::not_relational(operator, span, &ty, &operand_expr));
                return None;
            }
        };
        let attribute = match taken {
            ast::Taken::Tuple => None,
            ast::Taken::Value => {
                let mut names = heading.attributes().map(|(a, _)| a.to_string());
                let (Some(name), None) = (names.next(), names.next()) else {
                    errors.push(errors::not_one_attribute(span, &operand_expr, heading));
                    return None;
                };
                Some(name)
            }
        };
        let tuple = match ops::only(operand) {
            Ok(tuple) => tuple,
            Err(failed) => {
                errors.push(errors::failed(&failed, expr));
                return None;
            }
        };
        let result = match attribute {
            Some(name) => ops::extract(tuple, name),
            None => tuple,
        };
        self.matched(expected, result, expr, errors)
    }

    /// An `and`, an `or`, an `xor` or an `implies`, written `expr`, of two
    /// operands, or, of a list such as `and{a, b, c}`, any number: of
    /// booleans, or, but for `implies`, of integers, bit by bit, as the
    /// operands, checked on their own, and the context say (see
    /// [`ops::connected`]), each with its own errors.
    fn connective(
        &mut self,
        expr: &Expr,
        expected: Option<&Expected>,
        errors: &mut Vec<Diagnostic>,
    ) -> Option<hir::Expr> {
        let ops::Connecting { token, operands } = ops::Connecting::of(expr)?;
        let op = Operator::of(&token)?;
        let Some(Resolved::Connective {
            connective,
            not_left,
        }) = op.resolve(&hir::Type::Scalar(hir::Scalar::Boolean))
        else {
            unreachable!("`and`, `or`, `xor` and `implies` are connectives of booleans");
        };
        // What the others' tests show where each operand is computed: of
        // an `and`'s, if they are true, and of an `or`'s, if they are false,
        // as the guard rule computes the others only then (see
        // [`narrowed::Facts`]). An integer has no tests.
        let shown: Vec<Facts> = operands
            .iter()
            .enumerate()
            .map(|(i, operand)| {
                let truth = match connective {
                    hir::Connective::And => true,
                    hir::Connective::Or => i == 0 && not_left,
                    hir::Connective::Xor => return Facts::default(),
                };
                operand
                    .as_ref()
                    .map_or_else(Facts::default, |o| Facts::of(o, truth, self))
            })
            .collect();
        let facts: Vec<Facts> = (0..operands.len())
            .map(|i| {
                shown
                    .iter()
                    .enumerate()
                    .filter(|(j, _)| *j != i)
                    .fold(Facts::default(), |all, (_, facts)| all.and(facts))
            })
            .collect();
        let own = ops::OnTheirOwn::of(&operands, &mut self.narrowing(&facts));
        if let Some(error) = self.misgrouped(expr, op, &own, &facts) {
            errors.push(error);
            return None;
        }
        let expectation = ops::connected(op, &token, &operands, &own, expected);
        let checked = ops::fitted(
            operands,
            own.checked,
            Some(&expectation),
            false,
            &mut self.narrowing(&facts),
        );
        match op.resolve(&expectation.ty)? {
            Resolved::Connective {
                connective,
                not_left,
            } => connected_booleans(connective, not_left, checked, expr, expected, errors),
            resolved @ (Resolved::Binary(hir::BinaryOp::Bitwise(_)) | Resolved::Bytes(_)) => {
                let mut operands = Vec::new();
                for (out, own) in checked {
                    errors.extend(own);
                    operands.push(out);
                }
                let operands: Vec<hir::Expr> = operands.into_iter().collect::<Option<_>>()?;
                types::result_type(expected, &expectation.ty, expr, errors)?;
                let mut operands = operands.into_iter();
                let Some(first) = operands.next() else {
                    return self.bits_of_none(resolved, expr, errors);
                };
                operands.try_fold(first, |lhs, rhs| match resolved {
                    Resolved::Bytes(bitwise) => self.bytes_bits(bitwise, lhs, rhs, expr, errors),
                    Resolved::Binary(op) => {
                        let bits = ops::binary(op, lhs, rhs, self.bindings());
                        reported(bits, expr, errors)
                    }
                    _ => unreachable!("bits are of integers or of bytes"),
                })
            }
            Resolved::Identity | Resolved::NotByte | Resolved::Unary(_) | Resolved::Binary(_) => {
                unreachable!("a connective is of booleans, of integers or of bytes")
            }
        }
    }

    /// The connective `resolved`, written `expr`, of integers or bytes bit by
    /// bit, of no operands: all its bits set, or none.
    fn bits_of_none(
        &mut self,
        resolved: Resolved,
        expr: &Expr,
        errors: &mut Vec<Diagnostic>,
    ) -> Option<hir::Expr> {
        match resolved {
            Resolved::Bytes(bitwise) => {
                let number = hir::Expr::Constant(hir::Value::Integer(bitwise.identity() & 0xFF));
                self.byte_of_number(number, expr, errors)
            }
            Resolved::Binary(hir::BinaryOp::Bitwise(bitwise)) => {
                Some(hir::Expr::Constant(hir::Value::Integer(bitwise.identity())))
            }
            _ => unreachable!("bits are of integers or of bytes"),
        }
    }

    /// The byte of the number `number`, of a byte's range, written `expr`.
    fn byte_of_number(
        &mut self,
        number: hir::Expr,
        expr: &Expr,
        errors: &mut Vec<Diagnostic>,
    ) -> Option<hir::Expr> {
        let byte = self.namespaces().library().byte.clone();
        let components = ops::tuple([("value".to_string(), number)].into());
        self.selected(&byte, components, expr, errors)
    }

    /// The value of the literal `byte`, written `expr`.
    pub(crate) fn byte_literal(
        &mut self,
        byte: u8,
        expr: &Expr,
        errors: &mut Vec<Diagnostic>,
    ) -> Option<hir::Expr> {
        let number = hir::Expr::Constant(hir::Value::Integer(i64::from(byte)));
        self.byte_of_number(number, expr, errors)
    }

    /// The value of the literal of `bytes`, written `expr`: each selected
    /// once, however many times it is written.
    pub(crate) fn bytes_literal(
        &mut self,
        bytes: &[u8],
        expr: &Expr,
        errors: &mut Vec<Diagnostic>,
    ) -> Option<hir::Expr> {
        let mut selected: BTreeMap<u8, hir::Expr> = BTreeMap::new();
        let mut elements = Vec::with_capacity(bytes.len());
        for byte in bytes {
            if !selected.contains_key(byte) {
                let element = self.byte_literal(*byte, expr, errors)?;
                selected.insert(*byte, element);
            }
            elements.push(selected[byte].clone());
        }
        let byte = self.namespaces().library().byte.clone();
        Some(ops::array(hir::Type::Defined(byte), elements))
    }

    /// `lhs op rhs` of bytes, written `expr`: of their numbers, whose eight bits
    /// are all the two have, so that the result is a byte too.
    fn bytes_bits(
        &mut self,
        op: hir::Bitwise,
        lhs: hir::Expr,
        rhs: hir::Expr,
        expr: &Expr,
        errors: &mut Vec<Diagnostic>,
    ) -> Option<hir::Expr> {
        let number = ops::binary(
            hir::BinaryOp::Bitwise(op),
            ops::number_of_byte(lhs),
            ops::number_of_byte(rhs),
            self.bindings(),
        );
        let number = reported(number, expr, errors)?;
        self.byte_of_number(number, expr, errors)
    }

    /// `not` of the byte `operand`, written `expr`: the complement of its
    /// number, of its eight bits.
    fn not_byte(
        &mut self,
        operand: hir::Expr,
        expr: &Expr,
        errors: &mut Vec<Diagnostic>,
    ) -> Option<hir::Expr> {
        let complement = ops::unary(hir::UnaryOp::Complement, ops::number_of_byte(operand));
        let complement = types::evaluated(complement, expr, None, errors)?;
        let number = ops::binary(
            hir::BinaryOp::Bitwise(hir::Bitwise::And),
            complement,
            hir::Expr::Constant(hir::Value::Integer(0xFF)),
            self.bindings(),
        );
        let number = reported(number, expr, errors)?;
        self.byte_of_number(number, expr, errors)
    }

    /// Checks an operand of a connective, the one at an index, narrowed by
    /// what `facts` at that index show, giving its checked form and its
    /// type.
    fn narrowing<'c>(
        &'c mut self,
        facts: &'c [Facts],
    ) -> impl FnMut(
        usize,
        &Expr,
        Option<&Expected>,
        &mut Vec<Diagnostic>,
    ) -> Option<(hir::Expr, hir::Type)>
    + 'c {
        move |i, operand, expected, errors| {
            let checked = self.narrowed(operand, &facts[i], expected, errors)?;
            let ty = self.ty(&checked);
            Some((checked, ty))
        }
    }

    /// The error for a connective `op` of two operands, written `expr`,
    /// one an integer and the other a comparison of one, as `own` has
    /// them, checked on their own, which `facts` narrow: `x and 1 = 1` is
    /// `x and (1 = 1)`, as a comparison binds tighter, where the comparison
    /// of bits, `(x and 1) = 1`, was surely meant.
    fn misgrouped(
        &mut self,
        expr: &Expr,
        op: Operator,
        own: &ops::OnTheirOwn,
        facts: &[Facts],
    ) -> Option<Diagnostic> {
        let Expr::Binary(binary) = expr else {
            return None;
        };
        if op == Operator::Implies {
            return None;
        }
        let boolean = hir::Type::Scalar(hir::Scalar::Boolean);
        let ty = |i: usize| match &own.checked[i] {
            Some(types::Alone::Valid((_, ty))) => Some(ty),
            _ => None,
        };
        let bits = |i: usize| ty(i).filter(|ty| ops::has_bits(ty));
        let comparison = |operand: Option<Expr>| match operand? {
            Expr::Binary(b) if b.operator()?.kind().is_comparison() => Some(b),
            _ => None,
        };
        // The comparison's operand next to the other, which is an integer.
        let (i, comparison, inner, of) = if ty(1) == Some(&boolean)
            && let Some(of) = bits(0)
        {
            let comparison = comparison(binary.rhs())?;
            let lhs = comparison.lhs()?;
            (1, comparison, lhs, of)
        } else if ty(0) == Some(&boolean)
            && let Some(of) = bits(1)
        {
            let comparison = comparison(binary.lhs())?;
            let rhs = comparison.rhs()?;
            (0, comparison, rhs, of)
        } else {
            return None;
        };
        let mut scratch = Vec::new();
        let inner_ty = self
            .narrowed(&inner, &facts[i], None, &mut scratch)
            .map(|checked| self.ty(&checked));
        (inner_ty.as_ref() == Some(of) && scratch.is_empty())
            .then(|| errors::misgrouped_bits(binary, &comparison, &inner, of))
    }

    /// `test`, `x is t`, written `expr`: whether the value of `x` is of the
    /// type `t` (IM Prescription 15). `None` after reporting why it isn't
    /// valid: its type is invalid, or its operand's, or the test is always
    /// true or never.
    fn type_test(
        &mut self,
        test: &ast::IsExpr,
        expr: &Expr,
        expected: Option<&Expected>,
        errors: &mut Vec<Diagnostic>,
    ) -> Option<hir::Expr> {
        let operand_expr = test.operand()?;
        let operand = self.expr(&operand_expr, None, errors);
        let written = test.type_expr()?;
        let mut parameters = self.type_parameters();
        let ty = types::resolve(&written, &mut parameters, self.namespaces(), errors)?;
        let operand = operand?;
        let actual = self.ty(&operand);
        if let Some(error) =
            errors::type_test(errors::Testing::Is, &actual, &ty, &operand_expr, &written)
        {
            errors.push(error);
            return None;
        }
        self.matched(expected, ops::is(operand, ty), expr, errors)
    }

    /// `fail`, `fail e`, written `expr`: the program fails with the error
    /// `e`, so it has no value, and the type expected where it is, as one
    /// branch of an `if` takes the other's, which gives it one as it gives
    /// an empty relation its heading (A0011). It fails only if it is
    /// computed: folding computes it (see [`ops::evaluated`]).
    fn failure(
        &mut self,
        fail: &ast::FailExpr,
        expr: &Expr,
        expected: Option<&Expected>,
        errors: &mut Vec<Diagnostic>,
    ) -> Option<hir::Expr> {
        let error = self.error(
            &fail.error()?,
            &fail.keyword()?,
            errors::Signal::Fail,
            errors,
        );
        let Some(expected) = expected else {
            errors.push(errors::untyped_failure(expr.span()));
            return None;
        };
        Some(hir::Expr::Fail(
            Box::new(error?),
            Box::new(expected.ty.clone()),
        ))
    }

    /// The error `written`, which the `fail` or `warn` written `keyword`,
    /// as `signal` says, fails or warns with: a value of a subtype of the
    /// prelude's union type `error`, as a value of it. `None` after
    /// reporting why it isn't one.
    pub(crate) fn error(
        &mut self,
        written: &Expr,
        keyword: &SyntaxToken,
        signal: errors::Signal,
        errors: &mut Vec<Diagnostic>,
    ) -> Option<hir::Expr> {
        let error = self.expr(written, None, errors)?;
        // The prelude declares it, or that is reported.
        let ty = self.namespaces().library().error_type();
        let found = self.ty(&error);
        if !found.is_subtype_of(&ty) {
            errors.push(errors::not_an_error(written, &found, keyword, signal));
            return None;
        }
        // Computing it, as folding does, computes its message, as a call
        // of `message` would.
        let message = self.names.operators().message()?.id;
        let at = trendy_syntax::span(keyword.text_range());
        self.names
            .checked(message, at, errors)
            .then(|| error.treated(&ty))
    }

    /// `treat`, `treat x as t`, written `expr`: the value of `x` as one of
    /// the type `t`, a subtype of its type, which fails unless it is one
    /// (IM Prescription 14). `None` after reporting why it isn't valid: its
    /// type is invalid, or its operand's, or it always succeeds or never,
    /// or the value is known and isn't one.
    fn treated(
        &mut self,
        treat: &ast::TreatExpr,
        expr: &Expr,
        expected: Option<&Expected>,
        errors: &mut Vec<Diagnostic>,
    ) -> Option<hir::Expr> {
        let operand_expr = treat.operand()?;
        let operand = self.expr(&operand_expr, None, errors);
        let written = treat.type_expr()?;
        let mut parameters = self.type_parameters();
        let ty = types::resolve(&written, &mut parameters, self.namespaces(), errors)?;
        let operand = operand?;
        let actual = self.ty(&operand);
        if let Some(error) = errors::type_test(
            errors::Testing::Treat,
            &actual,
            &ty,
            &operand_expr,
            &written,
        ) {
            errors.push(error);
            return None;
        }
        match ops::treated_as(operand, ty) {
            Ok(treated) => self.matched(expected, treated, expr, errors),
            Err(failed) => {
                errors.push(errors::failed(&failed, expr));
                None
            }
        }
    }

    /// `expr`, checked as `expected` says, with the names `facts` narrow
    /// narrowed, if they narrow any (see [`narrowed::Narrowed`]).
    pub(crate) fn narrowed(
        &mut self,
        expr: &Expr,
        facts: &Facts,
        expected: Option<&Expected>,
        errors: &mut Vec<Diagnostic>,
    ) -> Option<hir::Expr> {
        if facts.is_empty() {
            return self.expr(expr, expected, errors);
        }
        let mut names = narrowed::Narrowed {
            outer: &mut *self.names,
            facts,
        };
        Checker::new(&mut names).expr(expr, expected, errors)
    }

    /// `binary`, a `where`, written `expr`: the tuples of its operand, a
    /// relation, for which its condition, a boolean checked with the
    /// attributes of the tuple at hand in scope, is true.
    ///
    /// A join or a product of relations is restricted as it is made
    /// (`ops::restricted_and`), so that what the condition rules out is
    /// never made when it needn't be.
    fn restricted(
        &mut self,
        binary: &ast::BinaryExpr,
        expr: &Expr,
        expected: Option<&Expected>,
        errors: &mut Vec<Diagnostic>,
    ) -> Option<hir::Expr> {
        let (operand, token, condition) = (binary.lhs()?, binary.operator()?, binary.rhs()?);
        self.restriction(&operand, token, &condition, expr, expected, errors)
    }

    /// The tuples of `operand_expr`, a relation, for which `condition_expr`,
    /// after `token`, `where`, is true, as for [`Checker::restricted`]: a
    /// `where`, written `expr`, or a load's.
    pub(crate) fn restriction(
        &mut self,
        operand_expr: &Expr,
        token: trendy_syntax::SyntaxToken,
        condition_expr: &Expr,
        expr: &Expr,
        expected: Option<&Expected>,
        errors: &mut Vec<Diagnostic>,
    ) -> Option<hir::Expr> {
        let written = unparenthesized(operand_expr);
        let restricted = match ops::Joining::of(&written) {
            Some(joining) => {
                let (operands, combined) = self.join_operands(&joining, None, errors)?;
                let headings: Option<Vec<hir::Heading>> = operands
                    .iter()
                    .map(|o| match self.ty(o) {
                        hir::Type::Relation(heading) => Some(heading),
                        _ => None,
                    })
                    .collect();
                match (combined, headings) {
                    (ops::Combined::And, Some(headings)) => Restricted::Joined {
                        heading: headings
                            .iter()
                            .fold(hir::Heading::default(), |so_far, h| so_far.and(h)),
                        operands,
                    },
                    (combined, _) => {
                        Restricted::Relation(combine(operands, combined, &written, errors)?)
                    }
                }
            }
            None => Restricted::Relation(self.expr(operand_expr, None, errors)?),
        };
        let heading = match &restricted {
            Restricted::Joined { heading, .. } => heading.clone(),
            Restricted::Relation(operand) => match self.ty(operand) {
                hir::Type::Relation(heading) => heading,
                ty => {
                    let span = trendy_syntax::span(token.text_range());
                    errors.push(errors::not_restrictable(span, &ty, operand_expr));
                    return None;
                }
            },
        };
        let condition = self.condition(heading.clone(), condition_expr, token, errors)?;
        let result = match restricted {
            Restricted::Relation(operand) => ops::restrict(operand, condition, self.bindings()),
            Restricted::Joined { operands, .. } => {
                if let Some(result) = ops::restricted_and(&operands, &condition, self.bindings()) {
                    result
                } else {
                    let product = ops::and(operands)
                        .map_err(|failed| errors.push(errors::failed(&failed, &written)))
                        .ok()?;
                    ops::restrict(product, condition, self.bindings())
                }
            }
        };
        match result {
            // Each tuple's attributes are of the types its tests show them
            // to be of, which the result's heading says.
            Ok(result) => {
                let shown = Facts::of(condition_expr, true, self).heading(&heading);
                let result = if shown == heading {
                    result
                } else {
                    result.treated(&hir::Type::Relation(shown))
                };
                self.matched(expected, result, expr, errors)
            }
            Err(failure) => {
                errors.push(errors::extension_failure(&failure, condition_expr));
                None
            }
        }
    }

    /// `condition`, after `token`, `where`, checked as a boolean with the
    /// attributes of the tuple at hand, of a relation with `heading`, in
    /// scope.
    pub(crate) fn condition(
        &mut self,
        heading: hir::Heading,
        condition: &Expr,
        token: trendy_syntax::SyntaxToken,
        errors: &mut Vec<Diagnostic>,
    ) -> Option<hir::Expr> {
        let names = TupleNames::new(&mut *self.names, heading);
        Self::boolean(names, condition, token, errors)
    }

    /// `condition`, after `token`, the `where` of a load into a relation,
    /// checked as [`Checker::condition`] checks one, of the tuples of
    /// `heading` it makes of an array's elements, which `unnumbered` says
    /// whether it leaves without their positions.
    pub(crate) fn unloaded_condition(
        &mut self,
        heading: hir::Heading,
        unnumbered: bool,
        condition: &Expr,
        token: trendy_syntax::SyntaxToken,
        errors: &mut Vec<Diagnostic>,
    ) -> Option<hir::Expr> {
        let names = TupleNames {
            outer: &mut *self.names,
            heading,
            unnumbered,
        };
        Self::boolean(names, condition, token, errors)
    }

    /// `condition`, after `token`, checked as a boolean with the names of
    /// `names`.
    fn boolean(
        mut names: TupleNames,
        condition: &Expr,
        token: trendy_syntax::SyntaxToken,
        errors: &mut Vec<Diagnostic>,
    ) -> Option<hir::Expr> {
        let boolean = Expected {
            ty: hir::Type::Scalar(hir::Scalar::Boolean),
            origin: types::Origin::Operator(token),
        };
        Checker::new(&mut names).expr(condition, Some(&boolean), errors)
    }

    /// `image`, written `expr`: the image relation of the tuple at hand in
    /// the operand, which is the operand joined with the tuple at hand
    /// projected on their common attributes, then without them.
    fn image(
        &mut self,
        image: &ast::ImageExpr,
        expr: &Expr,
        expected: Option<&Expected>,
        errors: &mut Vec<Diagnostic>,
    ) -> Option<hir::Expr> {
        let operand_expr = image.operand()?;
        let operand = self.expr(&operand_expr, None, errors);
        let Some(at_hand) = self.names.tuple_at_hand().cloned() else {
            errors.push(errors::image_outside(image));
            return None;
        };
        let operand = operand?;
        let span = trendy_syntax::span(image.operator()?.text_range());
        let hir::Type::Relation(heading) = self.ty(&operand) else {
            let ty = self.ty(&operand);
            errors.push(errors::image_not_relation(span, &ty, &operand_expr));
            return None;
        };
        let common = heading.common(&at_hand);
        if common.is_empty() {
            errors.push(errors::image_unrelated(
                span,
                &operand_expr,
                &heading,
                &at_hand,
            ));
            return None;
        }
        let mut ok = true;
        for name in &common {
            let (ty, at_hand_ty) = (heading.get(name)?, at_hand.get(name)?);
            if !ty.overlaps(at_hand_ty) {
                errors.push(errors::image_type_conflict(
                    span,
                    &operand_expr,
                    name,
                    ty,
                    at_hand_ty,
                ));
                ok = false;
            }
        }
        if !ok {
            return None;
        }
        let result = ops::image(operand, &heading, &at_hand);
        self.matched(expected, result, expr, errors)
    }

    /// `expr`, a join.
    fn joined(
        &mut self,
        expr: &Expr,
        expected: Option<&Expected>,
        errors: &mut Vec<Diagnostic>,
    ) -> Option<hir::Expr> {
        let joining = ops::Joining::of(expr)?;
        let (operands, combined) = self.join_operands(&joining, expected, errors)?;
        let result = combine(operands, combined, expr, errors)?;
        self.matched(expected, result, expr, errors)
    }

    /// The operands of `joining`, checked, and how the join combines them.
    fn join_operands(
        &mut self,
        joining: &ops::Joining,
        expected: Option<&Expected>,
        errors: &mut Vec<Diagnostic>,
    ) -> Option<(Vec<hir::Expr>, ops::Combined)> {
        // Every operand is checked on its own, so that each one's errors are
        // reported, and operands of different headings are, together. Of a
        // set operator's operands, of the same attributes, one whose own
        // can't be known, as an empty relation's, is checked again with
        // another's, or else the result's (see [`types::Alone`]); of
        // others, it is unknown.
        let alone: Vec<types::Alone<hir::Expr>> = joining
            .operands
            .iter()
            .map(|operand| {
                let mut own_errors = Vec::new();
                let out = self.expr(operand, None, &mut own_errors);
                types::Alone::of(out, own_errors, operand.span())
            })
            .collect();
        let given = alone
            .iter()
            .zip(&joining.operands)
            .find_map(|(alone, operand)| match alone {
                types::Alone::Valid(out) => Some(Expected {
                    ty: self.ty(out),
                    origin: types::Origin::Value(operand.clone()),
                }),
                _ => None,
            })
            .or_else(|| {
                expected
                    .filter(|e| matches!(e.ty, hir::Type::Relation(_)))
                    .cloned()
            })
            .filter(|_| joining.kind.is_set());
        let operands: Vec<Option<hir::Expr>> = alone
            .into_iter()
            .zip(&joining.operands)
            .map(|(alone, operand)| {
                alone.finish(given.is_some(), operand.span(), errors, |errors| {
                    self.expr(operand, given.as_ref(), errors)
                })
            })
            .collect();
        let operands: Vec<hir::Expr> = operands.into_iter().collect::<Option<_>>()?;
        let types: Vec<hir::Type> = operands.iter().map(|o| self.ty(o)).collect();
        let combined = joining.combined(&types, errors)?;
        Some((operands, combined))
    }

    /// The operator written `token`, its checked `operands` and the type of
    /// its result.
    fn operands(
        &mut self,
        token: &trendy_syntax::SyntaxToken,
        operands: Vec<Option<Expr>>,
        expected: Option<&Expected>,
        errors: &mut Vec<Diagnostic>,
    ) -> Option<(Resolved, Vec<hir::Expr>, hir::Type)> {
        let op = Operator::of(token)?;
        // For the help on an operator a type parameter's values don't have.
        let kinds: Vec<hir::Kind> = self.namespaces().kinds().into_iter().cloned().collect();
        ops::check_operands(
            op,
            token,
            operands,
            expected,
            &mut |_, operand, expected, errors| {
                let checked = self.expr(operand, expected, errors)?;
                let ty = self.ty(&checked);
                Some((checked, ty))
            },
            &kinds,
            errors,
        )
    }
}

/// `result`, of what is written `expr`, after reporting why not, if computing it
/// fails.
fn reported(
    result: Result<hir::Expr, ops::Failed>,
    expr: &Expr,
    errors: &mut Vec<Diagnostic>,
) -> Option<hir::Expr> {
    result
        .map_err(|failed| {
            errors.push(match failed {
                ops::Failed::Scalar(failure) => errors::failure(failure, expr, None),
                failed => errors::failed(&failed, expr),
            });
        })
        .ok()
}

/// The connective `connective` of booleans, written `expr`, of the
/// operands `checked`, each with its own errors, the left one negated
/// if `not_left`. Where an operand is known when compiling and decides
/// the result, the others aren't computed (see `hir::Connective::And`),
/// so their failures aren't reported, as for an `if`'s branch not
/// taken; otherwise one known to fail is an error, since it fails
/// whenever it is computed.
fn connected_booleans(
    connective: hir::Connective,
    not_left: bool,
    checked: Vec<(Option<hir::Expr>, Vec<Diagnostic>)>,
    expr: &Expr,
    expected: Option<&Expected>,
    errors: &mut Vec<Diagnostic>,
) -> Option<hir::Expr> {
    // The result, if a constant operand decides it, and that operand:
    // the others then aren't computed.
    let decided = checked
        .iter()
        .enumerate()
        .find_map(|(i, (out, _))| match out {
            Some(hir::Expr::Constant(hir::Value::Boolean(value))) => {
                let negated = i == 0 && not_left;
                Some((connective.decided(*value != negated)?, i))
            }
            _ => None,
        });
    let mut ok = true;
    let mut operands = Vec::new();
    for (i, (out, own)) in checked.into_iter().enumerate() {
        if decided.is_some_and(|(_, decider)| decider != i) {
            let before = errors.len();
            errors.extend(own.into_iter().filter(|d| d.code.kind() != CodeKind::Run));
            ok &= out.is_some() || errors.len() == before;
        } else {
            ok &= out.is_some();
            errors.extend(own);
        }
        operands.push(out);
    }
    if !ok {
        return None;
    }
    types::result_type(
        expected,
        &hir::Type::Scalar(hir::Scalar::Boolean),
        expr,
        errors,
    )?;
    if let Some((result, _)) = decided {
        return Some(hir::Expr::Constant(hir::Value::Boolean(result)));
    }
    let operands = operands
        .into_iter()
        .enumerate()
        .map(|(i, operand)| operand.map(|o| if i == 0 && not_left { o.negated() } else { o }))
        .collect::<Option<_>>()?;
    Some(connective.of(operands))
}

/// Whether `assignments`, of `list`, with their values, `values`, give
/// something a new value, after reporting each that gives an attribute its
/// own: all of them at once, as `unchanged` says, or else one by one.
fn changes_something(
    assignments: &[(ast::Assign, String, Span)],
    values: &BTreeMap<String, hir::Expr>,
    list: &ast::AssignList,
    unchanged: impl FnOnce(Unchanged) -> Diagnostic,
    errors: &mut Vec<Diagnostic>,
) -> bool {
    let own: Vec<&(ast::Assign, String, Span)> = assignments
        .iter()
        .filter(|(_, name, _)| is_own_value(&values[name], name))
        .collect();
    if own.len() == assignments.len() {
        errors.push(unchanged(Unchanged::OwnValues));
        return false;
    }
    for (assign, name, _) in &own {
        errors.push(errors::own_attribute_value(name, assign, list));
    }
    own.is_empty()
}

/// An attribute or a component, `name`, at `span`, that a list of
/// `assigning` gives a value, and, if it is a component, the names it may
/// be a misspelling of, and the components of each possible
/// representation of its value's type.
struct Named<'a> {
    assigning: ops::Assigning,
    name: &'a str,
    span: Span,
    candidates: &'a [String],
    representations: &'a [hir::Heading],
}

/// Whether `name`, at `span`, is right for `assigning` in an operand of type
/// `ty`, written `operand`, after reporting why not: new for an `extend`,
/// existing for a `replace` or an `update`, and, of a value of a type of
/// the program's own, a component of `heading`, the representation the
/// list gives new values. `other` is the keyword's span, if every name is
/// wrong for it. Also the type a `replace`'s or an `update`'s value must
/// have.
fn assigned_name(
    named: &Named,
    ty: &hir::Type,
    heading: &hir::Heading,
    operand: &Expr,
    other: Option<Span>,
    errors: &mut Vec<Diagnostic>,
) -> (bool, Option<Expected>) {
    let &Named {
        assigning,
        name,
        span,
        candidates,
        representations,
    } = named;
    if let hir::Type::Defined(defined) = ty {
        let Some(component) = heading.get(name) else {
            let unknown = errors::Unknown {
                name,
                span,
                candidates,
                representations,
            };
            errors.push(errors::replaced_unknown_component(
                assigning, &unknown, defined, operand,
            ));
            return (false, None);
        };
        return (
            true,
            Some(Expected {
                ty: component.clone(),
                origin: types::Origin::Component(operand.clone(), name.to_string()),
            }),
        );
    }
    match (assigning, heading.get(name)) {
        (ops::Assigning::Extend, None) => (true, None),
        (ops::Assigning::Extend, Some(_)) => {
            errors.push(errors::extension_exists(name, span, ty, operand, other));
            (false, None)
        }
        (ops::Assigning::Replace | ops::Assigning::Update, Some(replaced)) => (
            true,
            Some(Expected {
                ty: replaced.clone(),
                origin: types::Origin::Replaced(operand.clone(), name.to_string()),
            }),
        ),
        (ops::Assigning::Replace | ops::Assigning::Update, None) => {
            errors.push(errors::replaced_unknown(
                assigning, name, span, ty, operand, other,
            ));
            (false, None)
        }
    }
}

/// What a list of assignments gives new values: a tuple's or a relation's
/// attributes, or the components of a type's value, `defined`, in its
/// stored possible representation, whether the list names one of them or
/// none, or in another, whose selector is `selector`.
enum Changed<'t> {
    Attributes,
    Stored(&'t hir::Defined, bool),
    Other(&'t hir::Defined, hir::OperatorId),
}

/// The components of each possible representation of a type's value that
/// `changed` says a list gives new values, for errors, and the names a
/// component's misspelling may be one of: the representation's it gives new
/// values, `heading`, if the list names one, or else any.
fn components_known(changed: &Changed, heading: &hir::Heading) -> (Vec<hir::Heading>, Vec<String>) {
    let representations: Vec<hir::Heading> = match changed {
        Changed::Attributes => Vec::new(),
        Changed::Stored(defined, _) | Changed::Other(defined, _) => {
            defined.possible_representations().cloned().collect()
        }
    };
    let candidates = match changed {
        Changed::Stored(_, false) => representations
            .iter()
            .flat_map(|h| h.named().map(|(c, _)| c.to_string()))
            .collect(),
        _ => heading.named().map(|(c, _)| c.to_string()).collect(),
    };
    (representations, candidates)
}

/// The possible representation of a type whose components something names:
/// the stored one, and whether a component of it is named, or another, by
/// its position among the type's others.
pub(crate) enum Representation {
    Stored { known: bool },
    Other(usize),
}

/// The possible representation of the type `defined` whose components
/// `named`, each with where it is written, are: the first, the stored one
/// first, with all those it has, or the stored one if it has none. `None`
/// after reporting components of two representations, which `rule` says
/// must be one's.
pub(crate) fn representation(
    defined: &hir::Defined,
    named: &[(&str, Span)],
    rule: errors::OneRepresentation,
    errors: &mut Vec<Diagnostic>,
) -> Option<Representation> {
    let representations: Vec<&hir::Heading> = defined.possible_representations().collect();
    let known: Vec<(&str, Span)> = named
        .iter()
        .copied()
        .filter(|(name, _)| representations.iter().any(|h| h.get(name).is_some()))
        .collect();
    let all = |heading: &hir::Heading| known.iter().all(|(name, _)| heading.get(name).is_some());
    match representations.iter().position(|h| all(h)) {
        Some(0) => Some(Representation::Stored {
            known: !known.is_empty(),
        }),
        Some(at) => Some(Representation::Other(at - 1)),
        None => {
            // The first named, of its first representation, and one that
            // isn't of it.
            let (one, one_span) = known[0];
            let at = representations
                .iter()
                .position(|h| h.get(one).is_some())
                .expect("a known component is of a representation");
            let (other, other_span) = *known
                .iter()
                .find(|(name, _)| representations[at].get(name).is_none())
                .expect("no representation has them all");
            let other_at = representations
                .iter()
                .position(|h| h.get(other).is_some())
                .expect("a known component is of a representation");
            errors.push(errors::mixed_representations(
                defined,
                (one, one_span, representations[at]),
                (other, other_span, representations[other_at]),
                rule,
            ));
            None
        }
    }
}

/// What a `replace`, an `update` or an `extend` makes of its operand.
pub(crate) enum Replacement {
    /// Its attributes, or the stored components of a value of a type of
    /// the program's own, which the value is selected from, with those the
    /// list assigns added or replaced.
    Attributes(hir::Expr),
    /// The components of a value of a type of the program's own in another
    /// possible representation than the stored one, with those the list
    /// assigns replaced, which the value is selected from in it.
    Representation(Replaced),
}

/// The components of a value of a type of the program's own in the
/// possible representation with `components`, other than the stored one,
/// whose selector is `selector`, with some replaced: `tuple`, an `Extend`
/// of the tuple of them its conversion computes from the stored components
/// at hand, `Current(0)`, which [`Checker::selected_in`] computes from a
/// value's.
pub(crate) struct Replaced {
    pub(crate) selector: hir::OperatorId,
    pub(crate) components: hir::Heading,
    pub(crate) tuple: hir::Expr,
}

/// The gatherings of a `group` or a `wrap`, as `gathering` says, in
/// `list`, of an operand of type `ty`, written `operand`: each new
/// attribute's name and the attributes it gathers. `None` after reporting
/// each that is wrong: an unknown attribute, a name given twice, an
/// attribute gathered twice, or a new name that the result keeps.
fn gatherings(
    gathering: ops::Gathering,
    list: &ast::GatherList,
    ty: &hir::Type,
    operand: &Expr,
    errors: &mut Vec<Diagnostic>,
) -> Option<Vec<(String, BTreeSet<String>)>> {
    let (hir::Type::Tuple(heading) | hir::Type::Relation(heading)) = ty else {
        unreachable!("only tuples and relations have attributes to gather");
    };
    let context = errors::attributes_note("operand", ty);
    let mut ok = true;
    // Each attribute gathered, with the gathering that gathers it and
    // where.
    let mut gathered: BTreeMap<String, (String, Span)> = BTreeMap::new();
    let mut gathered_items: Vec<(String, Span, BTreeSet<String>)> = Vec::new();
    for item in list.gatherings() {
        let Some(ident) = item.name().and_then(|n| n.ident()) else {
            ok = false;
            continue;
        };
        let (name, span) = (
            ident.text().to_string(),
            trendy_syntax::span(ident.text_range()),
        );
        if let Some((_, first, _)) = gathered_items.iter().find(|(n, _, _)| *n == name) {
            errors.push(errors::duplicate_attribute(
                errors::Noun::Attribute,
                &name,
                span,
                *first,
            ));
            ok = false;
            continue;
        }
        let Some(names) = item.names() else {
            ok = false;
            continue;
        };
        let Some(set) = ops::listed(&names, heading, errors::Noun::Projected, &context, errors)
        else {
            ok = false;
            continue;
        };
        for attribute in names.names() {
            let Some(ident) = attribute.ident() else {
                continue;
            };
            let at = trendy_syntax::span(ident.text_range());
            match gathered.get(ident.text()) {
                Some((other, first)) if *other != name => {
                    errors.push(errors::gathered_twice(
                        gathering,
                        ident.text(),
                        at,
                        (other, *first),
                        &name,
                    ));
                    ok = false;
                }
                Some(_) => {}
                None => {
                    gathered.insert(ident.text().to_string(), (name.clone(), at));
                }
            }
        }
        gathered_items.push((name, span, set));
    }
    let all: BTreeSet<String> = gathered.keys().cloned().collect();
    let kept = heading.without(&all);
    for (name, span, _) in &gathered_items {
        if kept.get(name).is_some() {
            errors.push(errors::gathering_exists(
                gathering, name, *span, ty, operand,
            ));
            ok = false;
        }
    }
    ok.then(|| {
        gathered_items
            .into_iter()
            .map(|(name, _, set)| (name, set))
            .collect()
    })
}

/// The attributes an `ungroup` or an `unwrap`, as `unpacking` says, names in
/// `list`, of an operand of type `ty`. `None` after reporting each that is
/// wrong: unknown, named twice, not the kind it unpacks, or with an
/// attribute that the result would have twice.
fn unpackings(
    unpacking: ops::Unpacking,
    list: &ast::NameList,
    ty: &hir::Type,
    errors: &mut Vec<Diagnostic>,
) -> Option<BTreeSet<String>> {
    let (hir::Type::Tuple(heading) | hir::Type::Relation(heading)) = ty else {
        unreachable!("only tuples and relations have attributes to unpack");
    };
    let context = errors::attributes_note("operand", ty);
    let names = ops::listed(list, heading, errors::Noun::Projected, &context, errors)?;
    let mut ok = true;
    // Where each attribute of the result comes from: kept, or which
    // unpacked attribute.
    let mut placed: BTreeMap<String, Option<String>> = heading
        .without(&names)
        .attributes()
        .map(|(a, _)| (a.to_string(), None))
        .collect();
    for name in list.names() {
        let Some(ident) = name.ident() else {
            continue;
        };
        let span = trendy_syntax::span(ident.text_range());
        let unpacked = ident.text();
        let attribute_ty = heading.get(unpacked)?;
        let Some(inner) = unpacking.inner(attribute_ty) else {
            errors.push(errors::not_unpackable(
                unpacking,
                unpacked,
                span,
                attribute_ty,
            ));
            ok = false;
            continue;
        };
        for (attribute, _) in inner.attributes() {
            if let Some(from) = placed.get(attribute) {
                errors.push(errors::unpack_clash(
                    unpacking,
                    attribute,
                    span,
                    unpacked,
                    from.as_deref(),
                ));
                ok = false;
            } else {
                placed.insert(attribute.to_string(), Some(unpacked.to_string()));
            }
        }
    }
    ok.then_some(names)
}

/// An `extend`, a `replace` or an `update`, as `assigning` says, after
/// `keyword`, of `list`, applied to what is written `operand`.
#[derive(Clone, Copy)]
pub(crate) struct Assigned<'a> {
    pub(crate) assigning: ops::Assigning,
    pub(crate) keyword: &'a trendy_syntax::SyntaxToken,
    pub(crate) list: &'a ast::AssignList,
    pub(crate) operand: &'a Expr,
}

/// The qualifier of the name `expr` calls, a call, or a projection to the
/// parser, if it has one.
fn qualifier_of(expr: &Expr) -> Option<ast::Qualifier> {
    match expr {
        Expr::Call(call) => call.qualifier(),
        Expr::Project(project) => match project.operand()? {
            Expr::Qualified(written) => written.qualifier(),
            _ => None,
        },
        _ => None,
    }
}

/// The qualifier of the name `expr` calls, as it is written.
fn qualifier_text(expr: &Expr) -> String {
    qualifier_of(expr).map_or_else(String::new, |q| q.syntax().text().to_string())
}

/// Whether `arg`, the argument for the parameter `name`, `param`, of
/// `signature`, is marked as the parameter is: `var` for a `var` one,
/// `out` for an `out` one, and `const` or nothing for another; or `false`,
/// after reporting it.
fn marked(
    arg: &AttributeValue,
    name: &str,
    param: &Param,
    signature: &Signature,
    errors: &mut Vec<Diagnostic>,
) -> bool {
    let mode = match param {
        Param::Variable(_, mode) => *mode,
        Param::Value(_) | Param::Relation(_) | Param::Per { .. } => hir::Mode::Const,
    };
    let written = arg.mode();
    let said = match written.as_ref().map(ast::Mode::says) {
        None | Some(Said::Const) => hir::Mode::Const,
        Some(Said::Var) => hir::Mode::Var,
        Some(Said::Out) => hir::Mode::Out,
    };
    if said == mode {
        return true;
    }
    errors.push(errors::argument_mode(
        arg,
        name,
        mode,
        written.as_ref(),
        signature,
    ));
    false
}

/// The variables a call updates: the arguments in `args`, checked, for its
/// `var` and `out` parameters, written, with their modes, as `variables`
/// has them, which are taken out of `args`. `None` after reporting each
/// that isn't a variable.
fn updated(
    variables: &BTreeMap<&str, (Expr, hir::Mode)>,
    args: &mut BTreeMap<String, hir::Expr>,
    errors: &mut Vec<Diagnostic>,
) -> Option<BTreeMap<String, hir::Target>> {
    let mut updated = BTreeMap::new();
    let mut ok = true;
    for (name, (written, mode)) in variables {
        let Some(arg) = args.remove(*name) else {
            continue;
        };
        let target = match arg {
            hir::Expr::Variable(id) => hir::Target::Variable(id),
            hir::Expr::Local(id) => hir::Target::Local(id),
            hir::Expr::Parameter(parameter) => hir::Target::Parameter(parameter),
            _ => {
                errors.push(errors::argument_not_a_variable(name, *mode, written));
                ok = false;
                continue;
            }
        };
        updated.insert((*name).to_string(), target);
    }
    ok.then_some(updated)
}

/// The error for a call, with `written` arguments, of `signature`, which
/// has the parameters `missing` that aren't given one, and whose
/// parameters, with those of the other operators of its name, are
/// `described`.
fn missing_arguments(
    missing: &[&str],
    signature: &Signature,
    described: &str,
    written: Written,
) -> Diagnostic {
    let span = match written {
        Written::Arguments(args) => args.span(),
        Written::Nothing(list) => list.span(),
    };
    let d = errors::missing_attributes(
        errors::Noun::Argument,
        missing,
        &format!("the parameters are {described}"),
        span,
    );
    // `sum{r: plots{harvest}}`: the attribute was meant as the argument of
    // the `per` parameter, for each tuple, not as a projection.
    let ([name], Written::Arguments(args)) = (missing, written) else {
        return d;
    };
    let Param::Per { relation, .. } = &signature.written[*name] else {
        return d;
    };
    let projection = args
        .named()
        .find(|a| a.is_named(relation))
        .and_then(|a| a.value());
    match projection {
        Some(Expr::Project(projection)) => {
            errors::projected_relation(d, name, relation, &projection, args.insertion())
        }
        _ => d,
    }
}

/// The types a call gives its operator's type parameters, as its arguments
/// are checked, each from the argument it is the type of, or is in the
/// type of.
///
/// An argument is checked against its parameter's type once every type
/// parameter in that type has a type. One whose parameter's type is a type
/// parameter that has none yet is checked on its own, and gives it its
/// type; then, if none is ready, the rest are checked on their own, and
/// their types give their type parameters theirs. So which argument gives
/// a type parameter its type never depends on the order the arguments are
/// written in, which has no meaning, as for a tuple's attributes.
struct TypeArguments<'s> {
    signature: &'s Signature,
    types: Vec<Option<(hir::Type, Expr)>>,
}

impl<'s> TypeArguments<'s> {
    fn new(signature: &'s Signature) -> Self {
        TypeArguments {
            signature,
            types: vec![None; signature.types.len()],
        }
    }

    /// Whether every type parameter in `ty` has a type.
    fn know(&self, ty: &hir::Type) -> bool {
        ty.parameters().all(|p| self.types[p.index].is_some())
    }

    /// `ty`, a parameter's type, with each type parameter that has a type
    /// replaced by it.
    fn instantiate(&self, ty: &hir::Type) -> hir::Type {
        let types: Vec<hir::Type> = self
            .types
            .iter()
            .zip(&self.signature.types)
            .map(|(given, parameter)| match given {
                Some((ty, _)) => ty.clone(),
                None => hir::Type::Parameter(parameter.clone()),
            })
            .collect();
        ty.instantiated(&types)
    }

    /// What the argument for the parameter `name`, of type `ty`, every
    /// type parameter in which has a type, is expected to be: of that type,
    /// because of the argument that gave the type parameter its type, if
    /// `ty` is one, or else because of the type written.
    fn expected(&self, name: &str, ty: &hir::Type) -> Expected {
        let origin = match ty {
            hir::Type::Parameter(p) => {
                let (_, from) = self.types[p.index]
                    .as_ref()
                    .expect("only a known type is expected");
                types::Origin::TypeParameter(p.to_string(), from.clone())
            }
            _ => self.signature.param_origin(name),
        };
        Expected {
            ty: self.instantiate(ty),
            origin,
        }
    }

    /// Give each type parameter in `pattern`, the type of the parameter
    /// `name`, the type that makes it `actual`, the type of its argument,
    /// written `arg`: all of them, or, if no types do, none, and why.
    fn bind<'a>(
        &mut self,
        name: &'a str,
        pattern: &'a hir::Type,
        actual: &hir::Type,
        arg: &Expr,
        namespaces: &Namespaces,
    ) -> Result<(), Unbound<'a>> {
        let mut trial = self.types.clone();
        if bind(&mut trial, pattern, actual, arg, namespaces) {
            self.types = trial;
            return Ok(());
        }
        Err(match pattern {
            hir::Type::Parameter(p) => match &self.types[p.index] {
                Some(_) => Unbound::Given(Box::new(self.expected(name, pattern)), None),
                None => Unbound::Kind(p, self.signature.param_origin(name)),
            },
            _ => Unbound::Given(
                Box::new(Expected {
                    ty: self.instantiate(pattern),
                    origin: self.signature.param_origin(name),
                }),
                unadmitted(&self.types, pattern, actual, namespaces),
            ),
        })
    }

    /// Give the type parameter `parameter` the type `ty`, from the argument
    /// written `arg`: a relation's own tuple type, which it has none yet.
    fn give(&mut self, parameter: &hir::TypeParameter, ty: hir::Type, arg: &Expr) {
        self.types[parameter.index] = Some((ty, arg.clone()));
    }

    /// The type each type parameter is given, by index, once every
    /// argument is given and valid: each has one, since each is in a
    /// parameter's type.
    fn finish(self) -> Vec<hir::Type> {
        self.types
            .into_iter()
            .map(|t| t.expect("every type parameter is in a parameter's type").0)
            .collect()
    }
}

/// Give the type parameters in `pattern` the types in `types` that make it
/// `actual`, from the argument `arg`, where `namespaces` says which types
/// are of each kind: whether some do.
fn bind(
    types: &mut [Option<(hir::Type, Expr)>],
    pattern: &hir::Type,
    actual: &hir::Type,
    arg: &Expr,
    namespaces: &Namespaces,
) -> bool {
    match (pattern, actual) {
        (hir::Type::Parameter(p), _) => match &types[p.index] {
            Some((given, _)) => given == actual,
            None if namespaces.admits(&p.any, actual) => {
                types[p.index] = Some((actual.clone(), arg.clone()));
                true
            }
            None => false,
        },
        (hir::Type::Tuple(pattern), hir::Type::Tuple(actual))
        | (hir::Type::Relation(pattern), hir::Type::Relation(actual)) => {
            pattern.attributes().count() == actual.attributes().count()
                && pattern.attributes().all(|(name, ty)| {
                    actual
                        .get(name)
                        .is_some_and(|other| bind(types, ty, other, arg, namespaces))
                })
        }
        (hir::Type::Array(pattern), hir::Type::Array(actual)) => {
            bind(types, pattern, actual, arg, namespaces)
        }
        _ => pattern == actual,
    }
}

/// The type parameter in `pattern`, without a type in `types` yet, that
/// the part of `actual` in its place can't be, as `text` can't be the `T`
/// of `array T` if `T` is `any tuple`, and that part, if there is one.
fn unadmitted<'a>(
    types: &[Option<(hir::Type, Expr)>],
    pattern: &'a hir::Type,
    actual: &hir::Type,
    namespaces: &Namespaces,
) -> Option<(&'a hir::TypeParameter, hir::Type)> {
    match (pattern, actual) {
        (hir::Type::Parameter(p), _) => (types[p.index].is_none()
            && !namespaces.admits(&p.any, actual))
        .then(|| (p, actual.clone())),
        (hir::Type::Tuple(pattern), hir::Type::Tuple(actual))
        | (hir::Type::Relation(pattern), hir::Type::Relation(actual)) => pattern
            .attributes()
            .find_map(|(name, ty)| unadmitted(types, ty, actual.get(name)?, namespaces)),
        (hir::Type::Array(pattern), hir::Type::Array(actual)) => {
            unadmitted(types, pattern, actual, namespaces)
        }
        _ => None,
    }
}

/// Why an argument's type gives its parameter's type parameters none.
enum Unbound<'a> {
    /// They have types already, or the type has more than type parameters
    /// in it, so the argument was expected to have this one; and the type
    /// parameter in it that the argument's type has a type in place of
    /// that it can't be, if that is why.
    Given(Box<Expected>, Option<(&'a hir::TypeParameter, hir::Type)>),
    /// The parameter's type is the type parameter, which can't be the
    /// argument's type: `any tuple` or `any relation` of another type.
    Kind(&'a hir::TypeParameter, types::Origin),
}

impl Unbound<'_> {
    /// The error for an argument, written `arg`, of type `actual`, and
    /// with the value `value` if it is a constant, of a call of the
    /// built-in `builtin`, if it is one.
    fn diagnostic(
        &self,
        actual: &hir::Type,
        value: Option<&hir::Value>,
        arg: &Expr,
        namespaces: &Namespaces,
        builtin: Option<hir::ReadOnlyBuiltin>,
    ) -> Diagnostic {
        match self {
            Unbound::Given(expected, unadmitted) => {
                let d = errors::type_mismatch(&expected.ty, &expected.origin, actual, value, arg);
                match unadmitted {
                    Some((parameter, ty)) => {
                        d.with_note(errors::unadmitted(parameter, ty, namespaces))
                    }
                    None => d,
                }
            }
            Unbound::Kind(parameter, origin) => {
                let d = errors::not_of_kind(parameter, origin, actual, arg, namespaces);
                errors::counted_otherwise(d, builtin, actual, arg, namespaces)
            }
        }
    }
}

/// Whether `name_ref` is the operand of a projection, as in `r{a, b}`.
fn projected(name_ref: &ast::NameRef) -> bool {
    name_ref
        .syntax()
        .parent()
        .and_then(ast::ProjectExpr::cast)
        .and_then(|project| project.operand())
        .is_some_and(|operand| operand.syntax() == name_ref.syntax())
}

/// Whether `name_ref` is the operand of an index, as in `a[1]`.
fn indexed(name_ref: &ast::NameRef) -> bool {
    name_ref
        .syntax()
        .parent()
        .and_then(ast::IndexExpr::cast)
        .and_then(|index| index.operand())
        .is_some_and(|operand| operand.syntax() == name_ref.syntax())
}

/// When an argument is checked, as [`TypeArguments`] says.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Stage {
    /// Against its parameter's type, which is known.
    Ready,
    /// On its own, its parameter's type an unknown type parameter, which
    /// expects nothing of it, or a relation for `per` parameters.
    Bare,
    /// On its own, its parameter's type made of unknown type parameters.
    Structured,
    /// Once its relation is checked, for a `per` argument.
    Waiting,
}

/// Why an `extend`'s, a `replace`'s or an `update`'s list leaves its
/// operand as it is.
#[derive(Clone, Copy)]
pub(crate) enum Unchanged {
    /// It has no attributes.
    Empty,
    /// It gives each attribute its own value.
    OwnValues,
}

/// Whether `value`, computed for the attribute `name`, is that attribute
/// of the tuple at hand, unchanged.
fn is_own_value(value: &hir::Expr, name: &str) -> bool {
    matches!(
        value,
        hir::Expr::Extract(tuple, attribute)
            if attribute == name && matches!(**tuple, hir::Expr::Current(0, _))
    )
}

/// Names in the values an `extend` or a `replace` computes: the attributes of the tuple at
/// hand, with `heading`, which hide the names around them, in `outer`.
struct TupleNames<'a> {
    outer: &'a mut dyn Names,
    heading: hir::Heading,
    /// Whether a load into a relation without `numbered` makes the tuple
    /// at hand of an array's element.
    unnumbered: bool,
}

impl<'a> TupleNames<'a> {
    /// The names of a tuple with `heading`, which no load makes, in
    /// `outer`.
    fn new(outer: &'a mut dyn Names, heading: hir::Heading) -> Self {
        TupleNames {
            outer,
            heading,
            unnumbered: false,
        }
    }
}

impl Names for TupleNames<'_> {
    fn namespaces(&self) -> &Namespaces {
        self.outer.namespaces()
    }

    fn site(&self, at: Span) -> hir::Site {
        self.outer.site(at)
    }

    fn resolve(&mut self, name: &str, span: Span, errors: &mut Vec<Diagnostic>) -> Lookup {
        if self.heading.get(name).is_some() {
            let tuple = hir::Expr::Current(0, self.heading.clone());
            return Lookup::Found(hir::Expr::Extract(Box::new(tuple), name.to_string()));
        }
        match self.outer.resolve(name, span, errors) {
            Lookup::Found(expr) => Lookup::Found(ops::deeper(expr)),
            other => other,
        }
    }

    fn constant_of(
        &mut self,
        name: &str,
        unit: hir::FileId,
        span: Span,
        errors: &mut Vec<Diagnostic>,
    ) -> Lookup {
        self.outer.constant_of(name, unit, span, errors)
    }

    fn candidates(&self, at: Span) -> Vec<String> {
        self.heading
            .named()
            .map(|(name, _)| name.to_string())
            .chain(self.outer.candidates(at))
            .collect()
    }

    fn leaf_type(&self, leaf: &hir::Expr) -> hir::Type {
        self.outer.leaf_type(leaf)
    }

    fn fresh_local(&mut self, local: hir::Local) -> hir::LocalId {
        self.outer.fresh_local(local)
    }

    fn next_local(&self) -> usize {
        self.outer.next_local()
    }

    fn hides(&self, name: &str) -> bool {
        self.heading.get(name).is_some() || self.outer.hides(name)
    }

    fn tuple_at_hand(&self) -> Option<&hir::Heading> {
        Some(&self.heading)
    }

    fn unnumbered(&self) -> bool {
        self.unnumbered
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

    fn calling(&mut self, callee: &Signature, types: &[hir::Type], span: Span) {
        self.outer.calling(callee, types, span);
    }

    fn bindings(&self) -> ops::Bindings<'_> {
        self.outer.bindings()
    }

    fn narrowing_ended(&self, name: &str) -> Option<Span> {
        if self.heading.get(name).is_some() {
            return None;
        }
        self.outer.narrowing_ended(name)
    }
}

/// The value `written`, if there is one, checked with `names` in scope, as
/// `expected` says, and its type.
fn checked_value(
    names: &mut dyn Names,
    written: Option<Expr>,
    expected: Option<&Expected>,
    errors: &mut Vec<Diagnostic>,
) -> Option<(hir::Expr, hir::Type)> {
    let mut checker = Checker::new(names);
    let value = checker.expr(&written?, expected, errors)?;
    let ty = checker.ty(&value);
    Some((value, ty))
}

/// A call of `operator` with each attribute of the tuple at hand, of
/// `heading`, as the argument of the parameter of its name.
fn called_with(operator: hir::OperatorId, heading: &hir::Heading) -> hir::Expr {
    let at_hand = hir::Expr::Current(0, heading.clone());
    let args = heading
        .attributes()
        .map(|(k, _)| {
            let arg = hir::Expr::Extract(Box::new(at_hand.clone()), k.to_string());
            (k.to_string(), arg)
        })
        .collect();
    hir::Expr::Call(hir::Call {
        operator,
        types: Vec::new(),
        args,
        updated: BTreeMap::new(),
    })
}

/// The names of the components of a value of the program's own type
/// `defined` at hand, `Current(0, …)` its stored components, in `outer`:
/// the components of each of its possible representations, as in a
/// constraint, those of another than the stored one computed by its
/// conversion from the stored ones where each is named.
struct ComponentNames<'a> {
    outer: &'a mut dyn Names,
    defined: hir::Defined,
}

impl<'a> ComponentNames<'a> {
    fn new(outer: &'a mut dyn Names, defined: hir::Defined) -> Self {
        ComponentNames { outer, defined }
    }
}

impl Names for ComponentNames<'_> {
    fn namespaces(&self) -> &Namespaces {
        self.outer.namespaces()
    }

    fn site(&self, at: Span) -> hir::Site {
        self.outer.site(at)
    }

    fn resolve(&mut self, name: &str, span: Span, errors: &mut Vec<Diagnostic>) -> Lookup {
        let stored = &self.defined.components;
        if stored.get(name).is_some() {
            let tuple = hir::Expr::Current(0, stored.clone());
            return Lookup::Found(hir::Expr::Extract(Box::new(tuple), name.to_string()));
        }
        let conversion = self
            .defined
            .representation(name)
            .and_then(|(_, other)| other);
        if let Some(conversion) = conversion.map(|r| r.from) {
            if !self.outer.checked(conversion, span, errors) {
                return Lookup::Reported;
            }
            let converted = called_with(conversion, &self.defined.components);
            return Lookup::Found(hir::Expr::Extract(Box::new(converted), name.to_string()));
        }
        match self.outer.resolve(name, span, errors) {
            Lookup::Found(expr) => Lookup::Found(ops::deeper(expr)),
            other => other,
        }
    }

    fn constant_of(
        &mut self,
        name: &str,
        unit: hir::FileId,
        span: Span,
        errors: &mut Vec<Diagnostic>,
    ) -> Lookup {
        self.outer.constant_of(name, unit, span, errors)
    }

    fn candidates(&self, at: Span) -> Vec<String> {
        self.defined
            .possible_representations()
            .flat_map(|h| h.named().map(|(c, _)| c.to_string()))
            .chain(self.outer.candidates(at))
            .collect()
    }

    fn leaf_type(&self, leaf: &hir::Expr) -> hir::Type {
        self.outer.leaf_type(leaf)
    }

    fn fresh_local(&mut self, local: hir::Local) -> hir::LocalId {
        self.outer.fresh_local(local)
    }

    fn next_local(&self) -> usize {
        self.outer.next_local()
    }

    fn hides(&self, name: &str) -> bool {
        self.defined
            .possible_representations()
            .any(|h| h.get(name).is_some())
            || self.outer.hides(name)
    }

    fn tuple_at_hand(&self) -> Option<&hir::Heading> {
        Some(&self.defined.components)
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

    fn calling(&mut self, callee: &Signature, types: &[hir::Type], span: Span) {
        self.outer.calling(callee, types, span);
    }

    fn bindings(&self) -> ops::Bindings<'_> {
        self.outer.bindings()
    }

    fn narrowing_ended(&self, name: &str) -> Option<Span> {
        let component = self
            .defined
            .possible_representations()
            .any(|h| h.get(name).is_some());
        if component {
            return None;
        }
        self.outer.narrowing_ended(name)
    }
}

/// The names of the arguments `written`, which choose which operator of
/// its name a call calls.
pub(crate) fn argument_names(written: Written) -> ArgumentNames {
    let name_of = |a: &AttributeValue| Some(a.name()?.ident()?.text().to_string());
    match written {
        Written::Arguments(args) => ArgumentNames {
            named: args
                .named()
                .filter(|a| !a.is_shorthand())
                .filter_map(|a| name_of(&a))
                .collect(),
            alone: args
                .named()
                .filter(AttributeValue::is_shorthand)
                .filter_map(|a| name_of(&a))
                .collect(),
            unnamed: args.positional().count(),
        },
        // Names written as a projection's attributes, as in `double{n}`.
        Written::Nothing(list) => ArgumentNames {
            named: BTreeSet::new(),
            alone: list
                .names()
                .filter_map(|n| Some(n.ident()?.text().to_string()))
                .collect(),
            unnamed: 0,
        },
    }
}

/// What a call's arguments that say which parameter they are for are given
/// to (see [`Checker::assigned_to_parameters`]).
struct ArgumentsGiven<'g, 's> {
    /// The arguments to check, with their parameters.
    pending: Vec<(Expr, &'g str, &'s Param)>,
    /// Names alone that are no parameter's, which have no name.
    nameless: Vec<&'g Given>,
    /// The parameters none is for.
    missing: Vec<&'s str>,
    /// Whether nothing was reported.
    valid: bool,
}

/// An argument of a call that says which parameter it is for: `name: value`,
/// or a name alone, which is its own value.
struct Given {
    name: String,
    span: Span,
    value: Option<Expr>,
    /// As written in an argument list, which a call the parser took for a
    /// projection has none of.
    attribute: Option<AttributeValue>,
    /// Whether it is a name alone.
    alone: bool,
}

impl Given {
    /// The argument written `attribute`, if it has a name.
    fn written(attribute: AttributeValue) -> Option<Given> {
        let ident = attribute.name()?.ident()?;
        Some(Given {
            name: ident.text().to_string(),
            span: trendy_syntax::span(ident.text_range()),
            value: attribute.value(),
            alone: attribute.is_shorthand(),
            attribute: Some(attribute),
        })
    }

    /// The argument written `name`, in a list the parser took for a
    /// projection's.
    fn alone(name: &ast::Name) -> Option<Given> {
        let ident = name.ident()?;
        Some(Given {
            name: ident.text().to_string(),
            span: trendy_syntax::span(ident.text_range()),
            value: Some(name.value()),
            attribute: None,
            alone: true,
        })
    }
}
