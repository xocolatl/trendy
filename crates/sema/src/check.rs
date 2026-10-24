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

use trendy_diag::{CodeKind, Diagnostic, Span, edit_distance};
use trendy_hir as hir;
use trendy_syntax::SyntaxToken;
use trendy_syntax::ast::{self, AstNode, AttributeValue, Expr};

use crate::operators::{Operators, Param, Signature};
use crate::ops::{self, Operator, Resolved};
use crate::types::{self, Expected};
use crate::{Namespaces, errors};

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

    /// What `name`, written at `span`, refers to.
    fn resolve(&mut self, name: &str, span: Span, errors: &mut Vec<Diagnostic>) -> Lookup;

    /// The names in scope, which a name that refers to nothing may be a
    /// misspelling of.
    fn candidates(&self) -> Vec<String>;

    /// Whether a value named `name` is in scope.
    fn has_value(&self, name: &str) -> bool {
        self.candidates().iter().any(|c| c == name)
    }

    /// The type of `leaf`, a variable or parameter that [`Names::resolve`]
    /// gave.
    fn leaf_type(&self, leaf: &hir::Expr) -> hir::Type;

    /// The id of `local`, a value block's local variable, among those of the
    /// operator whose body this is, or of the initial value being checked.
    fn fresh_local(&mut self, local: hir::Local) -> hir::LocalId;

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

    /// What evaluating a call substitutes, besides its arguments.
    fn bindings(&self) -> ops::Bindings<'_>;
}

/// A call's arguments, as written.
#[derive(Clone, Copy)]
pub(crate) enum Written<'a> {
    /// In braces, `{name: value, …}`.
    Arguments(&'a ast::Arguments),
    /// None, in `f{}`, a projection on no attributes to the parser.
    Nothing(&'a ast::NameList),
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

    /// The value `expr` is, if it is known when compiling.
    pub(crate) fn constant(expr: &hir::Expr) -> Option<&hir::Value> {
        match expr {
            hir::Expr::Constant(value) => Some(value),
            _ => None,
        }
    }

    /// `out`, written `expr`, if it has the expected type; otherwise report
    /// the mismatch.
    pub(crate) fn matched(
        &self,
        expected: Option<&Expected>,
        out: hir::Expr,
        expr: &Expr,
        errors: &mut Vec<Diagnostic>,
    ) -> Option<hir::Expr> {
        let ty = self.ty(&out);
        match expected {
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
                let value = types::literal_value(literal, errors)?;
                self.matched(expected, hir::Expr::Constant(value), expr, errors)
            }
            Expr::Name(name_ref) => self.named(name_ref, expr, expected, errors),
            Expr::Block(block) => crate::block::value(block, &mut *self.names, expected, errors),
            Expr::Paren(paren) => self.expr(&paren.inner()?, expected, errors),
            Expr::Unary(unary) => {
                if let Some(literal) = ops::negated_literal(unary) {
                    let value = ops::negative_value(&literal, expr, errors)?;
                    return self.matched(expected, hir::Expr::Constant(value), expr, errors);
                }
                let token = unary.operator()?;
                let (resolved, operands, result) =
                    self.operands(&token, vec![unary.operand()], expected, errors)?;
                types::result_type(expected, &result, expr, errors)?;
                let [operand] = <[hir::Expr; 1]>::try_from(operands).ok()?;
                match resolved {
                    Resolved::Identity => Some(operand),
                    Resolved::Unary(op) => {
                        types::evaluated(ops::unary(op, operand), expr, None, errors)
                    }
                    Resolved::Binary(_) => None,
                }
            }
            Expr::Binary(binary)
                if binary
                    .operator()
                    .is_some_and(|t| t.kind() == trendy_syntax::SyntaxKind::WhereKw) =>
            {
                self.restricted(binary, expr, expected, errors)
            }
            Expr::Binary(_) | Expr::Nary(_) if ops::Joining::of(expr).is_some() => {
                self.joined(expr, expected, errors)
            }
            Expr::Nary(_) => None,
            Expr::Binary(binary) => {
                let token = binary.operator()?;
                let (resolved, operands, result) =
                    self.operands(&token, vec![binary.lhs(), binary.rhs()], expected, errors)?;
                types::result_type(expected, &result, expr, errors)?;
                let (Resolved::Binary(op), Ok([lhs, rhs])) =
                    (resolved, <[hir::Expr; 2]>::try_from(operands))
                else {
                    return None;
                };
                types::evaluated(
                    ops::binary(op, lhs, rhs),
                    expr,
                    binary.rhs().as_ref(),
                    errors,
                )
            }
            Expr::Extract(extract) => {
                let operand_expr = extract.operand()?;
                let operand = self.expr(&operand_expr, None, errors)?;
                let ty = self.ty(&operand);
                if let Some(error) = self.method_call(extract, &operand_expr, &ty) {
                    errors.push(error);
                    return None;
                }
                let name = ops::extracted(extract, &operand_expr, &ty, errors)?;
                self.matched(expected, ops::extract(operand, name), expr, errors)
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
            Expr::Project(project) if let Some((ident, list)) = self.projected_call(project) => {
                self.projection_call(&ident, &list, expr, expected, errors)
            }
            Expr::Project(_) | Expr::Postfix(_) => self.reshaped(expr, expected, errors),
            Expr::Image(image) => self.image(image, expr, expected, errors),
            Expr::From(from) => self.taken(from, expr, expected, errors),
            Expr::Tuple(tuple) => types::tuple_selector(tuple, expected, self, errors),
            Expr::Relation(relation) => types::relation_selector(relation, expected, self, errors),
            Expr::Array(array) => types::array_selector(array, expected, self, errors),
            Expr::Index(index) => self.indexed(index, expr, expected, errors),
            Expr::If(if_expr) => self.conditional(if_expr, expected, errors),
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
        let [then, otherwise] = self.branches(&branches, taken, expected, errors)?;
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
    /// type: the type expected, or else that of the first valid on its own
    /// (see [`types::Alone`]). The one a condition known when compiling
    /// doesn't take, as `taken` says, is never computed, so its failures
    /// aren't reported, and it has no value if one makes it fail. `None`
    /// after reporting a branch's errors.
    fn branches(
        &mut self,
        written: &[Expr; 2],
        taken: Option<bool>,
        expected: Option<&Expected>,
        errors: &mut Vec<Diagnostic>,
    ) -> Option<[Option<hir::Expr>; 2]> {
        let skipped = |i: usize| taken.is_some_and(|taken| taken != (i == 0));
        let check =
            |this: &mut Self, i: usize, expected: Option<&Expected>, errors: &mut Vec<_>| {
                if !skipped(i) {
                    return this.expr(&written[i], expected, errors);
                }
                let mut own: Vec<Diagnostic> = Vec::new();
                let out = this.expr(&written[i], expected, &mut own);
                errors.extend(own.into_iter().filter(|d| d.code.kind() != CodeKind::Run));
                out
            };
        let mut expectation = expected.cloned();
        let mut alone: [Option<types::Alone<hir::Expr>>; 2] = [None, None];
        if expectation.is_none() {
            for (i, branch) in written.iter().enumerate() {
                let mut own = Vec::new();
                let out = check(self, i, None, &mut own);
                let on_its_own = types::Alone::of(out, own, branch.span());
                let valid = matches!(on_its_own, types::Alone::Valid(_));
                if let types::Alone::Valid(out) = &on_its_own {
                    expectation = Some(Expected {
                        ty: self.ty(out),
                        origin: types::Origin::Value(branch.clone()),
                    });
                }
                alone[i] = Some(on_its_own);
                if valid {
                    break;
                }
            }
        }
        let typed = expectation.is_some();
        let mut values = [None, None];
        let mut ok = true;
        for (i, alone) in alone.into_iter().enumerate() {
            let before = errors.len();
            let out = match alone {
                Some(alone) => alone.finish(typed, written[i].span(), errors, |errors| {
                    check(self, i, expectation.as_ref(), errors)
                }),
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
            Lookup::Unknown => {
                let candidates = self.names.candidates();
                let candidates: Vec<&str> = candidates.iter().map(String::as_str).collect();
                errors.push(errors::unknown_name(name, span, "value", &candidates));
                None
            }
        }
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

    /// The error for `extract`, `x.f`, of `operand`, of type `ty`, which has
    /// no attribute `f`, if an operator is named `f`: a method call, as
    /// object-oriented languages write one, which trendy writes `f{p: x}`.
    fn method_call(
        &self,
        extract: &ast::ExtractExpr,
        operand: &Expr,
        ty: &hir::Type,
    ) -> Option<Diagnostic> {
        let ident = extract.attribute()?.ident()?;
        let name = ident.text();
        if let hir::Type::Tuple(heading) = ty
            && heading.get(name).is_some()
        {
            return None;
        }
        let signature = self.names.operators().named(name)?;
        // `x.f{}`: the empty braces that follow are the call's arguments.
        let braces = extract
            .syntax()
            .parent()
            .and_then(ast::ProjectExpr::cast)
            .filter(|p| p.names().is_some_and(|n| n.names().next().is_none()));
        let whole = braces
            .as_ref()
            .map_or_else(|| extract.span(), AstNode::span);
        let only = (signature.written.len() == 1)
            .then(|| signature.written.keys().next())
            .flatten()
            .filter(|_| braces.is_some());
        Some(errors::method_call(
            name,
            trendy_syntax::span(ident.text_range()),
            operand,
            whole,
            only.map(String::as_str),
            &signature.text(),
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
        let operator = self.names.operators().named(name).is_some();
        let call = if operator {
            list.names().next().is_none() || !self.names.has_value(name)
        } else {
            errors::elsewhere(name).is_some() && !self.names.has_value(name)
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
        self.call(&ident, Written::Arguments(&args), expr, expected, errors)
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
        let known = self.names.operators().named(ident.text()).is_some();
        if known && list.names().next().is_some() {
            self.positional_names(ident, list, errors);
            return None;
        }
        self.call(ident, Written::Nothing(list), expr, expected, errors)
    }

    /// The names in `list`, after the operator `ident` names, written as a
    /// projection's attributes: each a positional argument, which a call
    /// can't have.
    fn positional_names(
        &self,
        ident: &SyntaxToken,
        list: &ast::NameList,
        errors: &mut Vec<Diagnostic>,
    ) {
        let signature = self
            .names
            .operators()
            .named(ident.text())
            .expect("a projected call is of an operator");
        let names: Vec<ast::Name> = list.names().collect();
        let only_param = (signature.written.len() == 1 && names.len() == 1)
            .then(|| signature.written.keys().next())
            .flatten();
        for name in &names {
            errors.push(errors::positional_argument(
                name.span(),
                only_param.map(String::as_str),
                &signature.text(),
            ));
        }
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

    /// A call of the operator `ident` names, with `written` arguments, as
    /// `expr`: a read-only operator's value, evaluated when compiling if
    /// its arguments are known then, and the variables it reads. One whose
    /// arguments are known only for each tuple at hand is evaluated once
    /// it is, when a constant operand is folded.
    fn call(
        &mut self,
        ident: &SyntaxToken,
        written: Written,
        expr: &Expr,
        expected: Option<&Expected>,
        errors: &mut Vec<Diagnostic>,
    ) -> Option<hir::Expr> {
        let name = ident.text();
        let span = trendy_syntax::span(ident.text_range());
        let Some(signature) = self.names.operators().named(name).cloned() else {
            if self.names.has_value(name) {
                errors.push(errors::not_an_operator(name, span, "a value"));
            } else {
                let of_array = self.of_array(written);
                let operators = self.names.operators().names();
                errors.push(errors::unknown_operator(
                    name,
                    span,
                    &operators,
                    Some(written),
                    of_array,
                ));
            }
            return None;
        };
        if !signature.read_only {
            let projectable = matches!(written, Written::Nothing(_)) && self.names.has_value(name);
            errors.push(errors::operator_call_without_value(
                name,
                span,
                signature.defined(),
                matches!(written, Written::Nothing(_)),
                projectable,
            ));
            return None;
        }
        if !signature.valid {
            return None;
        }
        let call = self.arguments(written, &signature, errors)?;
        // A built-in's implementation is the compiler's, never checked.
        if signature.builtin.is_none() && !self.names.checked(signature.id, span, errors) {
            return None;
        }
        let result = match ops::call(call, self.names.bindings()) {
            Ok(result) => result,
            Err(failed) => {
                errors.push(errors::call_failure(&failed, expr, &signature));
                return None;
            }
        };
        self.matched(expected, result, expr, errors)
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
        let params = &signature.written;
        let signature_text = signature.text();
        let mut ok = true;

        let (named, positional): (Vec<AttributeValue>, Vec<ast::PositionalArg>) = match written {
            Written::Arguments(args) => (args.named().collect(), args.positional().collect()),
            Written::Nothing(_) => (Vec::new(), Vec::new()),
        };
        let named: Vec<(AttributeValue, String, Span)> = named
            .into_iter()
            .filter_map(|a| {
                let ident = a.name()?.ident()?;
                let span = trendy_syntax::span(ident.text_range());
                Some((a, ident.text().to_string(), span))
            })
            .collect();
        // With one parameter, a lone unnamed argument can only be that one.
        let only_param = (params.len() == 1 && named.is_empty() && positional.len() == 1)
            .then(|| params.keys().next())
            .flatten();
        for arg in &positional {
            ok = false;
            errors.push(errors::positional_argument(
                arg.span(),
                only_param.map(String::as_str),
                &signature_text,
            ));
        }

        let mut missing: Vec<&str> = params
            .keys()
            .map(String::as_str)
            .filter(|p| !named.iter().any(|(_, n, _)| n == p))
            .collect();
        // The arguments to check, with their parameters.
        let mut pending: Vec<(Expr, &str, &Param)> = Vec::new();
        for (arg, name, span) in &named {
            if let Some(&(_, _, first)) = named.iter().find(|(_, n, _)| n == name)
                && first != *span
            {
                errors.push(errors::duplicate_attribute(
                    errors::Noun::Argument,
                    name,
                    *span,
                    first,
                ));
                ok = false;
                continue;
            }
            let Some(param) = params.get(name) else {
                ok = false;
                let renamed = missing
                    .iter()
                    .copied()
                    .filter(|m| edit_distance(m, name) <= 2)
                    .min_by_key(|m| edit_distance(m, name));
                if let Some(target) = renamed {
                    missing.retain(|m| *m != target);
                }
                let Written::Arguments(args) = written else {
                    unreachable!("only arguments in braces are named");
                };
                errors.push(errors::unknown_attribute(
                    errors::Noun::Argument,
                    name,
                    *span,
                    &format!("the parameters are {signature_text}"),
                    renamed,
                    Some(args.removal(arg)),
                ));
                continue;
            };
            match arg.value() {
                Some(value) => pending.push((value, name, param)),
                None => ok = false,
            }
        }

        let checked = self.typed_arguments(pending, signature, errors);
        // A lone unnamed argument is reported, with a fix, as that parameter.
        if !missing.is_empty() && positional.is_empty() {
            errors.push(missing_arguments(&missing, signature, written));
            ok = false;
        }
        // Every type parameter has a type once every argument is given.
        let (types, args) = checked.filter(|_| ok)?;
        Some(hir::Call {
            operator: signature.id,
            types: types.finish(),
            args,
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
                Param::Value(ty) | Param::Per { ty, .. } if types.know(ty) => Stage::Ready,
                Param::Value(hir::Type::Parameter(_))
                | Param::Per {
                    ty: hir::Type::Parameter(_),
                    ..
                }
                | Param::Relation(_) => Stage::Bare,
                Param::Value(_) | Param::Per { .. } => Stage::Structured,
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
                let mut names = TupleNames {
                    outer: &mut *self.names,
                    heading: heading.clone(),
                };
                Checker::new(&mut names).expr(value, expected.as_ref(), errors)?
            }
            None => self.expr(value, expected.as_ref(), errors)?,
        };
        if expected.is_none() {
            let actual = self.ty(&checked);
            let namespaces = self.names.namespaces();
            if let Err(error) = types.bind(name, ty, &actual, value, namespaces) {
                let constant = Self::constant(&checked);
                let builtin = types.signature.builtin;
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
        let list = postfix.assignments()?;
        let keyword = postfix.operator()?;
        let assigned = Assigned {
            assigning,
            keyword: &keyword,
            list: &list,
            operand: &operand_expr,
        };
        let result = self.assign_attributes(
            operand,
            &assigned,
            |why| match why {
                Unchanged::Empty => errors::empty_assign(assigning, postfix, &operand_expr),
                Unchanged::OwnValues => errors::own_values_replaced(postfix, &operand_expr),
            },
            errors,
        )?;
        self.matched(expected, result, expr, errors)
    }

    /// `operand`, written `assigned.operand`, with the attributes in
    /// `assigned.list` added, or given new values, as `assigned.assigning`
    /// says: each attribute's value is checked with the attributes of the
    /// tuple at hand in scope. A `replace`'s values have the types of the
    /// attributes they replace, and are computed from the old values, all
    /// at once. An attribute given its own value is an error, and
    /// `unchanged` is the error for a list that leaves the operand as it
    /// is.
    pub(crate) fn assign_attributes(
        &mut self,
        operand: hir::Expr,
        assigned: &Assigned,
        unchanged: impl FnOnce(Unchanged) -> Diagnostic,
        errors: &mut Vec<Diagnostic>,
    ) -> Option<hir::Expr> {
        let Assigned {
            assigning,
            keyword,
            list,
            operand: operand_expr,
        } = *assigned;
        let ty = self.ty(&operand);
        let (hir::Type::Tuple(heading) | hir::Type::Relation(heading)) = &ty else {
            let at = trendy_syntax::span(keyword.text_range());
            errors.push(errors::not_projectable(
                Some(assigning.keyword()),
                at,
                &ty,
                operand_expr,
            ));
            return None;
        };
        let assignments: Vec<(ast::Assign, String, Span)> = list
            .assignments()
            .filter_map(|assign| {
                let ident = assign.name()?.ident()?;
                let span = trendy_syntax::span(ident.text_range());
                Some((assign, ident.text().to_string(), span))
            })
            .collect();
        if list.assignments().next().is_none() {
            errors.push(unchanged(Unchanged::Empty));
            return None;
        }
        // When every name is wrong for an `extend` or a `replace`, the
        // other one is likely what was meant.
        let keyword = trendy_syntax::span(keyword.text_range());
        let other = (assigning != ops::Assigning::Update
            && assignments.iter().all(|(_, name, _)| {
                heading.get(name).is_some() == (assigning == ops::Assigning::Extend)
            }))
        .then_some(keyword);
        let mut ok = true;
        let mut values = BTreeMap::new();
        let mut types = BTreeMap::new();
        for (assign, name, span) in &assignments {
            if let Some(&(_, _, first)) = assignments.iter().find(|(_, n, _)| n == name)
                && first != *span
            {
                errors.push(errors::duplicate_attribute(
                    errors::Noun::Attribute,
                    name,
                    *span,
                    first,
                ));
                ok = false;
                continue;
            }
            let (valid, expected) =
                assigned_name(assigning, name, *span, &ty, operand_expr, other, errors);
            ok &= valid;
            let mut names = TupleNames {
                outer: &mut *self.names,
                heading: heading.clone(),
            };
            let mut checker = Checker::new(&mut names);
            match assign
                .value()
                .and_then(|v| checker.expr(&v, expected.as_ref(), errors))
            {
                Some(value) => {
                    types.insert(name.clone(), checker.ty(&value));
                    values.insert(name.clone(), value);
                }
                None => ok = false,
            }
        }
        if !ok {
            return None;
        }
        let own: Vec<&(ast::Assign, String, Span)> = assignments
            .iter()
            .filter(|(_, name, _)| is_own_value(&values[name], name))
            .collect();
        if own.len() == assignments.len() {
            errors.push(unchanged(Unchanged::OwnValues));
            return None;
        }
        if !own.is_empty() {
            for (assign, name, _) in own {
                errors.push(errors::own_attribute_value(name, assign, list));
            }
            return None;
        }
        match ops::extend(operand, hir::Heading::new(types), values, self.bindings()) {
            Ok(result) => Some(result),
            Err(failure) => {
                let value = assignments
                    .iter()
                    .find(|(_, name, _)| Some(name) == failure.attribute.as_ref())
                    .and_then(|(assign, _, _)| assign.value())?;
                errors.push(errors::extension_failure(&failure, &value));
                None
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
            hir::Type::Scalar(_) | hir::Type::Array(_) | hir::Type::Parameter(_) => {
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
        let operand_expr = binary.lhs()?;
        let token = binary.operator()?;
        let condition_expr = binary.rhs()?;
        let written = unparenthesized(&operand_expr);
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
            None => Restricted::Relation(self.expr(&operand_expr, None, errors)?),
        };
        let heading = match &restricted {
            Restricted::Joined { heading, .. } => heading.clone(),
            Restricted::Relation(operand) => match self.ty(operand) {
                hir::Type::Relation(heading) => heading,
                ty => {
                    let span = trendy_syntax::span(token.text_range());
                    errors.push(errors::not_restrictable(span, &ty, &operand_expr));
                    return None;
                }
            },
        };
        let condition = self.condition(heading, &condition_expr, token, errors)?;
        let result = match restricted {
            Restricted::Relation(operand) => ops::restrict(operand, condition, self.bindings()),
            Restricted::Joined { operands, .. } => {
                if let Some(result) = ops::restricted_and(&operands, &condition, self.bindings()) {
                    Ok(result)
                } else {
                    let product = ops::and(operands)
                        .map_err(|failed| errors.push(errors::failed(&failed, &written)))
                        .ok()?;
                    ops::restrict(product, condition, self.bindings())
                }
            }
        };
        match result {
            Ok(result) => self.matched(expected, result, expr, errors),
            Err(failure) => {
                errors.push(errors::extension_failure(&failure, &condition_expr));
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
        let boolean = Expected {
            ty: hir::Type::Scalar(hir::Scalar::Boolean),
            origin: types::Origin::Operator(token),
        };
        let mut names = TupleNames {
            outer: &mut *self.names,
            heading,
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
            if ty != at_hand_ty {
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
        // reported, and operands of different headings are, together. Of
        // operands of one heading, one whose own can't be known, as an empty
        // relation's, is checked again with another's, or else the result's
        // (see [`types::Alone`]); of others, it is unknown.
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
            .filter(|_| joining.kind.one_heading());
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
            &mut |operand, expected, errors| {
                let checked = self.expr(operand, expected, errors)?;
                let ty = self.ty(&checked);
                Some((checked, ty))
            },
            &kinds,
            errors,
        )
    }
}

/// Whether `name`, at `span`, is right for `assigning` in an operand of type
/// `ty`, written `operand`, after reporting why not: new for an `extend`,
/// existing for a `replace`. `other` is the keyword's span, if every name
/// is wrong for it. Also the type a `replace`'s value must have.
fn assigned_name(
    assigning: ops::Assigning,
    name: &str,
    span: Span,
    ty: &hir::Type,
    operand: &Expr,
    other: Option<Span>,
    errors: &mut Vec<Diagnostic>,
) -> (bool, Option<Expected>) {
    let (hir::Type::Tuple(heading) | hir::Type::Relation(heading)) = ty else {
        unreachable!("only tuples and relations have attributes to assign");
    };
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

/// The error for a call, with `written` arguments, of `signature`, which
/// has the parameters `missing` that aren't given one.
fn missing_arguments(missing: &[&str], signature: &Signature, written: Written) -> Diagnostic {
    // A type with a type parameter in it has no default value.
    let missing: Vec<_> = missing
        .iter()
        .map(|&name| {
            let ty = match &signature.written[name] {
                Param::Value(ty) | Param::Per { ty, .. } => Some(ty),
                Param::Relation(_) => None,
            };
            (name, ty.filter(|ty| ty.parameters().next().is_none()))
        })
        .collect();
    let (span, insertion) = match written {
        Written::Arguments(args) => (args.span(), args.insertion()),
        Written::Nothing(list) => (list.span(), list.insertion()),
    };
    let d = errors::missing_attributes(
        errors::Noun::Argument,
        &missing,
        &format!("the parameters are {}", signature.text()),
        span,
        insertion,
    );
    // `sum{r: plots{harvest}}`: the attribute was meant as the argument of
    // the `per` parameter, for each tuple, not as a projection.
    let ([(name, _)], Written::Arguments(args)) = (missing.as_slice(), written) else {
        return d;
    };
    let Param::Per { relation, .. } = &signature.written[*name] else {
        return d;
    };
    let projection = args
        .named()
        .find(|a| {
            a.name()
                .and_then(|n| n.ident())
                .is_some_and(|i| i.text() == relation)
        })
        .and_then(|a| a.value());
    match projection {
        Some(Expr::Project(projection)) => {
            errors::projected_relation(d, name, relation, &projection, insertion)
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
                Some(_) => Unbound::Given(Box::new(self.expected(name, pattern))),
                None => Unbound::Kind(p, self.signature.param_origin(name)),
            },
            _ => Unbound::Given(Box::new(Expected {
                ty: self.instantiate(pattern),
                origin: self.signature.param_origin(name),
            })),
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

/// Why an argument's type gives its parameter's type parameters none.
enum Unbound<'a> {
    /// They have types already, or the type has more than type parameters
    /// in it, so the argument was expected to have this one.
    Given(Box<Expected>),
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
        builtin: Option<hir::Builtin>,
    ) -> Diagnostic {
        match self {
            Unbound::Given(expected) => {
                errors::type_mismatch(&expected.ty, &expected.origin, actual, value, arg)
            }
            Unbound::Kind(parameter, origin) => {
                let d = errors::not_of_kind(parameter, origin, actual, arg, namespaces);
                errors::counted_otherwise(d, builtin, actual, arg)
            }
        }
    }
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
}

impl Names for TupleNames<'_> {
    fn namespaces(&self) -> &Namespaces {
        self.outer.namespaces()
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

    fn candidates(&self) -> Vec<String> {
        self.heading
            .named()
            .map(|(name, _)| name.to_string())
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
        self.heading.get(name).is_some() || self.outer.hides(name)
    }

    fn tuple_at_hand(&self) -> Option<&hir::Heading> {
        Some(&self.heading)
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
