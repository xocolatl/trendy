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

use trendy_diag::{Diagnostic, Span};
use trendy_hir as hir;
use trendy_syntax::ast::{self, Expr};

use crate::ops::{self, Operator, Resolved};
use crate::types::{self, Expected};
use crate::{Values, errors};

/// What a name refers to.
pub(crate) enum Lookup {
    Found(hir::Expr),
    /// Something that can't be used here, already reported.
    Reported,
    /// Nothing, which the checker reports with the names that are in scope.
    Unknown,
}

/// What names in an expression refer to.
pub(crate) trait Names {
    fn values(&self) -> &Values;

    /// What `name`, written at `span`, refers to.
    fn resolve(&mut self, name: &str, span: Span, errors: &mut Vec<Diagnostic>) -> Lookup;

    /// The names in scope, which a name that refers to nothing may be a
    /// misspelling of.
    fn candidates(&self) -> Vec<String>;

    /// The type of `leaf`, a variable or parameter that [`Names::resolve`]
    /// gave.
    fn leaf_type(&self, leaf: &hir::Expr) -> hir::Type;

    /// The heading of the tuple at hand, `Current(0, …)`, if there is one:
    /// in the values of an `extend` or a `replace`.
    fn tuple_at_hand(&self) -> Option<&hir::Heading> {
        None
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

    pub(crate) fn values(&self) -> &Values {
        self.names.values()
    }

    /// The type of `expr`, which this checker produced.
    pub(crate) fn ty(&self, expr: &hir::Expr) -> hir::Type {
        expr.type_with(&|leaf| self.names.leaf_type(leaf))
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
            Expr::Name(name_ref) => {
                let ident = name_ref.ident()?;
                let (name, span) = (ident.text(), trendy_syntax::span(ident.text_range()));
                match self.names.resolve(name, span, errors) {
                    Lookup::Found(resolved) => self.matched(expected, resolved, expr, errors),
                    Lookup::Reported => None,
                    Lookup::Unknown => {
                        let candidates = self.names.candidates();
                        let candidates: Vec<&str> = candidates.iter().map(String::as_str).collect();
                        errors.push(errors::unknown_name(name, span, "value", &candidates));
                        None
                    }
                }
            }
            Expr::Paren(paren) => self.expr(&paren.inner()?, expected, errors),
            Expr::Unary(unary) => {
                if let Some(literal) = ops::negated_literal(unary) {
                    let value = ops::negative_value(&literal, expr, errors)?;
                    return self.matched(expected, hir::Expr::Constant(value), expr, errors);
                }
                let token = unary.operator()?;
                let (resolved, operands) =
                    self.operands(&token, vec![unary.operand()], expected, errors)?;
                types::result_type(expected, resolved.result(), expr, errors)?;
                let [operand] = <[hir::Expr; 1]>::try_from(operands).ok()?;
                match resolved {
                    Resolved::Identity(_) => Some(operand),
                    Resolved::Unary(op) => {
                        types::evaluated(ops::unary(op, operand), resolved, expr, None, errors)
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
                let (resolved, operands) =
                    self.operands(&token, vec![binary.lhs(), binary.rhs()], expected, errors)?;
                types::result_type(expected, resolved.result(), expr, errors)?;
                let (Resolved::Binary(op), Ok([lhs, rhs])) =
                    (resolved, <[hir::Expr; 2]>::try_from(operands))
                else {
                    return None;
                };
                types::evaluated(
                    ops::binary(op, lhs, rhs),
                    resolved,
                    expr,
                    binary.rhs().as_ref(),
                    errors,
                )
            }
            Expr::Extract(extract) => {
                let operand_expr = extract.operand()?;
                let operand = self.expr(&operand_expr, None, errors)?;
                let ty = self.ty(&operand);
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
            Expr::Project(_) | Expr::Postfix(_) => self.reshaped(expr, expected, errors),
            Expr::Image(image) => self.image(image, expr, expected, errors),
            Expr::From(from) => self.taken(from, expr, expected, errors),
            Expr::Tuple(tuple) => types::tuple_selector(tuple, expected, self, errors),
            Expr::Relation(relation) => types::relation_selector(relation, expected, self, errors),
        }
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

    /// `postfix`, an `extend` or a `replace`, as `assigning` says, written
    /// `expr`: each attribute's value is checked with the attributes of the
    /// tuple at hand in scope. A `replace`'s values have the types of the
    /// attributes they replace, and are computed from the old values, all
    /// at once.
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
        let keyword = postfix.operator()?;
        let (hir::Type::Tuple(heading) | hir::Type::Relation(heading)) = &ty else {
            let at = trendy_syntax::span(keyword.text_range());
            errors.push(errors::not_projectable(
                Some(assigning.keyword()),
                at,
                &ty,
                &operand_expr,
            ));
            return None;
        };
        let list = postfix.assignments()?;
        let assignments: Vec<(ast::Assign, String, Span)> = list
            .assignments()
            .filter_map(|assign| {
                let ident = assign.name()?.ident()?;
                let span = trendy_syntax::span(ident.text_range());
                Some((assign, ident.text().to_string(), span))
            })
            .collect();
        if list.assignments().next().is_none() {
            errors.push(errors::empty_assign(assigning, postfix, &operand_expr));
            return None;
        }
        // When every name is wrong for this operator, the other one is
        // likely what was meant.
        let keyword = trendy_syntax::span(keyword.text_range());
        let other = assignments
            .iter()
            .all(|(_, name, _)| {
                heading.get(name).is_some() == (assigning == ops::Assigning::Extend)
            })
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
                assigned_name(assigning, name, *span, &ty, &operand_expr, other, errors);
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
        match ops::extend(operand, hir::Heading::new(types), values) {
            Ok(result) => self.matched(expected, result, expr, errors),
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
            ops::Gathering::Group => match ops::group(operand, heading, gathered) {
                Ok(result) => result,
                Err(failure) => {
                    errors.push(errors::extension_failure(&failure, &operand_expr));
                    return None;
                }
            },
            ops::Gathering::Wrap => ops::wrap(operand, heading, gathered),
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
            ops::Unpacking::Unwrap => ops::unwrap(operand, heading, &names),
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
            hir::Type::Scalar(_) => {
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
    fn restricted(
        &mut self,
        binary: &ast::BinaryExpr,
        expr: &Expr,
        expected: Option<&Expected>,
        errors: &mut Vec<Diagnostic>,
    ) -> Option<hir::Expr> {
        let operand_expr = binary.lhs()?;
        let operand = self.expr(&operand_expr, None, errors)?;
        let token = binary.operator()?;
        let ty = self.ty(&operand);
        let hir::Type::Relation(heading) = ty else {
            let span = trendy_syntax::span(token.text_range());
            errors.push(errors::not_restrictable(span, &ty, &operand_expr));
            return None;
        };
        let condition_expr = binary.rhs()?;
        let boolean = Expected {
            ty: hir::Type::Scalar(hir::Scalar::Boolean),
            origin: types::Origin::Operator(token),
        };
        let mut names = TupleNames {
            outer: &mut *self.names,
            heading,
        };
        let condition = Checker::new(&mut names).expr(&condition_expr, Some(&boolean), errors)?;
        match ops::restrict(operand, condition) {
            Ok(result) => self.matched(expected, result, expr, errors),
            Err(failure) => {
                errors.push(errors::extension_failure(&failure, &condition_expr));
                None
            }
        }
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
        // Every operand is checked, so that each one's errors are reported.
        let operands: Vec<Option<hir::Expr>> = joining
            .operands
            .iter()
            .map(|operand| self.expr(operand, None, errors))
            .collect();
        let operands: Vec<hir::Expr> = operands.into_iter().collect::<Option<_>>()?;
        let types: Vec<hir::Type> = operands.iter().map(|o| self.ty(o)).collect();
        let combined = joining.combined(&types, errors)?;
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
        match result {
            Ok(result) => self.matched(expected, result, expr, errors),
            Err(failed) => {
                errors.push(errors::failed(&failed, expr));
                None
            }
        }
    }

    /// The operator written `token` and its checked `operands`.
    fn operands(
        &mut self,
        token: &trendy_syntax::SyntaxToken,
        operands: Vec<Option<Expr>>,
        expected: Option<&Expected>,
        errors: &mut Vec<Diagnostic>,
    ) -> Option<(Resolved, Vec<hir::Expr>)> {
        let op = Operator::of(token)?;
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
        (ops::Assigning::Replace, Some(replaced)) => (
            true,
            Some(Expected {
                ty: replaced.clone(),
                origin: types::Origin::Replaced(operand.clone(), name.to_string()),
            }),
        ),
        (ops::Assigning::Replace, None) => {
            errors.push(errors::replaced_unknown(name, span, ty, operand, other));
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

/// Names in the values an `extend` or a `replace` computes: the attributes of the tuple at
/// hand, with `heading`, which hide the names around them, in `outer`.
struct TupleNames<'a> {
    outer: &'a mut dyn Names,
    heading: hir::Heading,
}

impl Names for TupleNames<'_> {
    fn values(&self) -> &Values {
        self.outer.values()
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
            .attributes()
            .map(|(name, _)| name.to_string())
            .chain(self.outer.candidates())
            .collect()
    }

    fn leaf_type(&self, leaf: &hir::Expr) -> hir::Type {
        self.outer.leaf_type(leaf)
    }

    fn tuple_at_hand(&self) -> Option<&hir::Heading> {
        Some(&self.heading)
    }
}
