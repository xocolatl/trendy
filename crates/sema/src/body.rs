//! Operators: their parameters, and the statements and expressions in their
//! bodies.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use trendy_diag::{Diagnostic, Span, edit_distance};
use trendy_hir as hir;
use trendy_syntax::ast::{self, AstNode, AttributeValue, Expr};

use crate::ops::{self, Operator, Resolved};
use crate::types::{self, Expected, Origin, Scope, Selectors};
use crate::{Kind, Values, errors};

/// What a call needs to know about the operator it calls.
pub(crate) struct Signature {
    pub(crate) id: hir::OperatorId,
    /// The definition, for a user operator: where its parameter types are
    /// written, for the labels of mismatches.
    decl: Option<ast::Operator>,
    /// Whether the parameters are valid. Calls to an operator whose
    /// parameters have errors aren't checked, because those errors are
    /// already reported.
    valid: bool,
}

impl Signature {
    pub(crate) fn builtin(id: hir::OperatorId) -> Self {
        Signature {
            id,
            decl: None,
            valid: true,
        }
    }

    pub(crate) fn user(id: hir::OperatorId, decl: ast::Operator, valid: bool) -> Self {
        Signature {
            id,
            decl: Some(decl),
            valid,
        }
    }

    /// Where parameter `name`'s type is written.
    fn param_origin(&self, name: &str) -> Origin {
        let written = self.decl.as_ref().and_then(|decl| {
            decl.params()?
                .attributes()
                .find(|a| {
                    a.name()
                        .and_then(|n| n.ident())
                        .is_some_and(|i| i.text() == name)
                })?
                .type_expr()
        });
        written.map_or(Origin::Builtin, Origin::Type)
    }
}

/// Everything a body can refer to.
pub(crate) struct Context<'a> {
    pub(crate) values: &'a Values,
    pub(crate) operators: &'a [hir::Operator],
    pub(crate) signatures: &'a HashMap<String, Signature>,
    pub(crate) variables: &'a [hir::Variable],
    pub(crate) variable_ids: &'a HashMap<String, hir::VariableId>,
}

/// A user operator's parameters, or `None` after reporting why they are
/// invalid.
pub(crate) fn params(
    heading: &ast::Heading,
    values: &Values,
    errors: &mut Vec<Diagnostic>,
) -> Option<BTreeMap<String, hir::ParamType>> {
    let heading = types::resolve_heading(heading, values, errors)?;
    Some(
        heading
            .attributes()
            .map(|(name, ty)| (name.to_string(), hir::ParamType::Type(ty.clone())))
            .collect(),
    )
}

/// The statements of the user operator `op`, whose id is `within`.
pub(crate) fn body(
    op: &ast::Operator,
    within: hir::OperatorId,
    cx: &Context,
    errors: &mut Vec<Diagnostic>,
) -> Option<Vec<hir::Statement>> {
    let body = op.body()?;
    let mut statements = Vec::new();
    let mut ok = true;
    for stmt in body.statements() {
        match call(&stmt, within, cx, errors) {
            Some(statement) => statements.push(statement),
            None => ok = false,
        }
    }
    ok.then_some(statements)
}

/// `call name{arguments};`.
fn call(
    stmt: &ast::CallStmt,
    within: hir::OperatorId,
    cx: &Context,
    errors: &mut Vec<Diagnostic>,
) -> Option<hir::Statement> {
    let ident = stmt.callee()?.ident()?;
    let name = ident.text();
    let span = trendy_syntax::span(ident.text_range());
    let Some(signature) = cx.signatures.get(name) else {
        let what = if cx.operators[within.0].params.contains_key(name) {
            Some("a parameter")
        } else {
            match cx.values.get(name) {
                Some((Kind::Variable, _)) => Some("a variable"),
                _ => None,
            }
        };
        if let Some(what) = what {
            errors.push(errors::not_an_operator(name, span, what));
        } else {
            let operators: Vec<&str> = cx.signatures.keys().map(String::as_str).collect();
            errors.push(errors::unknown_name(name, span, "operator", &operators));
        }
        return None;
    };
    let args = stmt.arguments()?;
    if !signature.valid {
        return None;
    }
    let args = arguments(&args, signature, name, within, cx, errors)?;
    Some(hir::Statement::Call {
        operator: signature.id,
        args,
    })
}

/// A call's arguments: a tuple of the callee's parameters.
fn arguments(
    args: &ast::Arguments,
    signature: &Signature,
    callee: &str,
    within: hir::OperatorId,
    cx: &Context,
    errors: &mut Vec<Diagnostic>,
) -> Option<BTreeMap<String, hir::Expr>> {
    let params = &cx.operators[signature.id.0].params;
    let signature_text = format!("`{callee}{}`", params_text(params));
    let mut ok = true;

    let named: Vec<(AttributeValue, String, Span)> = args
        .named()
        .filter_map(|a| {
            let ident = a.name()?.ident()?;
            let span = trendy_syntax::span(ident.text_range());
            Some((a, ident.text().to_string(), span))
        })
        .collect();
    let positional: Vec<_> = args.positional().collect();
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
    let mut result = BTreeMap::new();
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
        let expected = match param {
            hir::ParamType::Type(ty) => Some(Expected {
                ty: ty.clone(),
                origin: signature.param_origin(name),
            }),
            hir::ParamType::Any => None,
        };
        match arg
            .value()
            .and_then(|v| expr(&v, expected.as_ref(), within, cx, errors))
        {
            Some(value) => {
                result.insert(name.clone(), value);
            }
            None => ok = false,
        }
    }
    // A lone unnamed argument is reported, with a fix, as that parameter.
    if !missing.is_empty() && positional.is_empty() {
        let missing: Vec<_> = missing
            .into_iter()
            .map(|name| {
                let ty = match &params[name] {
                    hir::ParamType::Type(ty) => Some(ty),
                    hir::ParamType::Any => None,
                };
                (name, ty)
            })
            .collect();
        errors.push(errors::missing_attributes(
            errors::Noun::Argument,
            &missing,
            &format!("the parameters are {signature_text}"),
            args.span(),
            args.insertion(),
        ));
        ok = false;
    }
    ok.then_some(result)
}

/// An expression in a body. Whatever doesn't depend on parameters or
/// variables is evaluated now, to a constant, and the rest simplified.
fn expr(
    expr: &Expr,
    expected: Option<&Expected>,
    within: hir::OperatorId,
    cx: &Context,
    errors: &mut Vec<Diagnostic>,
) -> Option<hir::Expr> {
    match expr {
        Expr::Name(name_ref) => name(name_ref, expr, expected, within, cx, errors),
        Expr::Paren(paren) => self::expr(&paren.inner()?, expected, within, cx, errors),
        Expr::Unary(unary) => {
            if ops::negated_literal(unary).is_some() {
                let mut scope = Literals { values: cx.values };
                return types::value(expr, expected, &mut scope, errors).map(hir::Expr::Constant);
            }
            let token = unary.operator()?;
            let (resolved, operands) =
                operands(&token, vec![unary.operand()], expected, within, cx, errors)?;
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
        Expr::Binary(binary) if ops::JoinKind::of(binary).is_some() => {
            joined(binary, expr, expected, within, cx, errors)
        }
        Expr::Binary(binary) => {
            let token = binary.operator()?;
            let (resolved, operands) = operands(
                &token,
                vec![binary.lhs(), binary.rhs()],
                expected,
                within,
                cx,
                errors,
            )?;
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
            let operand = self::expr(&operand_expr, None, within, cx, errors)?;
            let ty = type_of(&operand, within, cx);
            let name = ops::extracted(extract, &operand_expr, &ty, errors)?;
            let hir::Type::Tuple(heading) = &ty else {
                unreachable!("checked to be a tuple");
            };
            let attribute_ty = heading.get(&name)?.clone();
            if let Some(expected) = expected
                && expected.ty != attribute_ty
            {
                errors.push(errors::type_mismatch(
                    &expected.ty,
                    &expected.origin,
                    &attribute_ty,
                    None,
                    expr,
                ));
                return None;
            }
            Some(ops::extract(operand, name))
        }
        Expr::Project(_) | Expr::Postfix(_) => reshaped(expr, expected, within, cx, errors),
        Expr::Literal(_) => {
            let mut scope = Literals { values: cx.values };
            types::value(expr, expected, &mut scope, errors).map(hir::Expr::Constant)
        }
        Expr::Tuple(tuple) => {
            types::tuple_selector(tuple, expected, &mut Selected { cx, within }, errors)
        }
        Expr::Relation(relation) => {
            types::relation_selector(relation, expected, &mut Selected { cx, within }, errors)
        }
    }
}

/// A projection, `remove` or `rename`, written `expr`, in a body.
fn reshaped(
    expr: &Expr,
    expected: Option<&Expected>,
    within: hir::OperatorId,
    cx: &Context,
    errors: &mut Vec<Diagnostic>,
) -> Option<hir::Expr> {
    let (operand_expr, rename) = match expr {
        Expr::Project(project) => (project.operand()?, None),
        Expr::Postfix(postfix) => (postfix.operand()?, Some(postfix)),
        _ => unreachable!("only projections and postfix operators reshape"),
    };
    let operand = self::expr(&operand_expr, None, within, cx, errors)?;
    let ty = type_of(&operand, within, cx);
    let (result, result_ty) = match (expr, rename) {
        (_, Some(postfix)) if ops::is_rename(postfix) => {
            let renamings = ops::renamed(postfix, &operand_expr, &ty, errors)?;
            let result_ty = ty.renamed(&renamings);
            (ops::rename(operand, renamings), result_ty)
        }
        (Expr::Project(project), _) => {
            let projection = ops::Projection::Project(project);
            let names = ops::removed(projection, &operand_expr, &ty, errors)?;
            let result_ty = ty.without(&names);
            (ops::remove(operand, names), result_ty)
        }
        (_, Some(postfix)) => {
            let projection = ops::Projection::Remove(postfix);
            let names = ops::removed(projection, &operand_expr, &ty, errors)?;
            let result_ty = ty.without(&names);
            (ops::remove(operand, names), result_ty)
        }
        _ => unreachable!("matched above"),
    };
    if let Some(expected) = expected
        && expected.ty != result_ty
    {
        errors.push(errors::type_mismatch(
            &expected.ty,
            &expected.origin,
            &result_ty,
            None,
            expr,
        ));
        return None;
    }
    Some(result)
}

/// A join, `binary`, written `expr`, in a body.
fn joined(
    binary: &ast::BinaryExpr,
    expr: &Expr,
    expected: Option<&Expected>,
    within: hir::OperatorId,
    cx: &Context,
    errors: &mut Vec<Diagnostic>,
) -> Option<hir::Expr> {
    let (join, token) = ops::JoinKind::of(binary)?;
    let (lhs_expr, rhs_expr) = (binary.lhs()?, binary.rhs()?);
    let lhs = self::expr(&lhs_expr, None, within, cx, errors);
    let rhs = self::expr(&rhs_expr, None, within, cx, errors);
    let (lhs, rhs) = (lhs?, rhs?);
    let (lhs_ty, rhs_ty) = (type_of(&lhs, within, cx), type_of(&rhs, within, cx));
    let removed = ops::joined(
        join,
        &token,
        (&lhs_expr, &lhs_ty),
        (&rhs_expr, &rhs_ty),
        errors,
    )?;
    let result_ty = lhs_ty.and(&rhs_ty).without(&removed);
    if let Some(expected) = expected
        && expected.ty != result_ty
    {
        errors.push(errors::type_mismatch(
            &expected.ty,
            &expected.origin,
            &result_ty,
            None,
            expr,
        ));
        return None;
    }
    Some(ops::remove(ops::and(lhs, rhs), removed))
}

/// The operator written `token` and its checked `operands`.
fn operands(
    token: &trendy_syntax::SyntaxToken,
    operands: Vec<Option<Expr>>,
    expected: Option<&Expected>,
    within: hir::OperatorId,
    cx: &Context,
    errors: &mut Vec<Diagnostic>,
) -> Option<(Resolved, Vec<hir::Expr>)> {
    let op = Operator::of(token)?;
    ops::check_operands(
        op,
        token,
        operands,
        expected,
        &mut |operand, expected, errors| {
            let checked = self::expr(operand, expected, within, cx, errors)?;
            let ty = type_of(&checked, within, cx);
            Some((checked, ty))
        },
        errors,
    )
}

/// The type of `expr`, in the body of `within`.
fn type_of(expr: &hir::Expr, within: hir::OperatorId, cx: &Context) -> hir::Type {
    match expr {
        hir::Expr::Constant(value) => value.ty(),
        hir::Expr::Variable(id) => cx.variables[id.0].ty.clone(),
        hir::Expr::Parameter(name) => match &cx.operators[within.0].params[name] {
            hir::ParamType::Type(ty) => ty.clone(),
            hir::ParamType::Any => unreachable!("only built-ins have `any` parameters"),
        },
        hir::Expr::Unary(op, _) => hir::Type::Scalar(op.result()),
        hir::Expr::Binary(op, ..) => hir::Type::Scalar(op.result()),
        hir::Expr::Extract(tuple, name) => match type_of(tuple, within, cx) {
            hir::Type::Tuple(heading) => heading
                .get(name)
                .cloned()
                .expect("sema extracts only attributes the tuple has"),
            other => unreachable!("extracting from {other}"),
        },
        hir::Expr::Tuple(attributes) => hir::Type::Tuple(hir::Heading::new(
            attributes
                .iter()
                .map(|(name, e)| (name.clone(), type_of(e, within, cx)))
                .collect(),
        )),
        hir::Expr::Relation(heading, _) => hir::Type::Relation(heading.clone()),
        hir::Expr::Remove(operand, names) => type_of(operand, within, cx).without(names),
        hir::Expr::Rename(operand, renamings) => type_of(operand, within, cx).renamed(renamings),
        hir::Expr::And(lhs, rhs) => type_of(lhs, within, cx).and(&type_of(rhs, within, cx)),
    }
}

/// A name in a body: a parameter, or else a variable.
fn name(
    name_ref: &ast::NameRef,
    expr: &Expr,
    expected: Option<&Expected>,
    within: hir::OperatorId,
    cx: &Context,
    errors: &mut Vec<Diagnostic>,
) -> Option<hir::Expr> {
    let ident = name_ref.ident()?;
    let name = ident.text();
    let span = trendy_syntax::span(ident.text_range());
    let params = &cx.operators[within.0].params;

    // Parameters hide variables of the same name.
    let (resolved, ty) = if let Some(param) = params.get(name) {
        let hir::ParamType::Type(ty) = param else {
            unreachable!("only built-ins have `any` parameters");
        };
        (hir::Expr::Parameter(name.to_string()), ty.clone())
    } else if let Some(&id) = cx.variable_ids.get(name) {
        (hir::Expr::Variable(id), cx.variables[id.0].ty.clone())
    } else {
        // A variable with errors in its declaration is already reported.
        if cx
            .values
            .get(name)
            .is_none_or(|(kind, _)| kind != Kind::Variable)
        {
            cx.not_a_value(name, span, within, errors);
        }
        return None;
    };
    if let Some(expected) = expected
        && ty != expected.ty
    {
        errors.push(errors::type_mismatch(
            &expected.ty,
            &expected.origin,
            &ty,
            None,
            expr,
        ));
        return None;
    }
    Some(resolved)
}

impl Context<'_> {
    /// Report the name `name`, at `span`, which is neither a parameter of
    /// `within` nor a variable.
    fn not_a_value(
        &self,
        name: &str,
        span: Span,
        within: hir::OperatorId,
        errors: &mut Vec<Diagnostic>,
    ) {
        if let Some((Kind::Operator | Kind::Builtin, _)) = self.values.get(name) {
            errors.push(errors::operator_as_value(name, span));
            return;
        }
        let candidates: Vec<&str> = self.operators[within.0]
            .params
            .keys()
            .map(String::as_str)
            .chain(self.variable_ids.keys().map(String::as_str))
            .collect();
        errors.push(errors::unknown_name(name, span, "value", &candidates));
    }
}

/// Literals in a body, which have no names in them.
struct Literals<'a> {
    values: &'a Values,
}

impl Scope for Literals<'_> {
    fn values(&self) -> &Values {
        self.values
    }

    fn constant(&mut self, _: &ast::NameRef, _: &mut Vec<Diagnostic>) -> Option<hir::Value> {
        unreachable!("a literal has no names")
    }
}

/// Tuple and relation selectors in a body, whose values can be known only
/// when the program runs.
struct Selected<'a, 'b> {
    cx: &'a Context<'b>,
    within: hir::OperatorId,
}

impl Selectors for Selected<'_, '_> {
    type Out = hir::Expr;

    fn values(&self) -> &Values {
        self.cx.values
    }

    fn check(
        &mut self,
        expr: &Expr,
        expected: Option<&Expected>,
        errors: &mut Vec<Diagnostic>,
    ) -> Option<hir::Expr> {
        self::expr(expr, expected, self.within, self.cx, errors)
    }

    fn ty(&self, out: &hir::Expr) -> hir::Type {
        type_of(out, self.within, self.cx)
    }

    fn constant<'o>(&self, out: &'o hir::Expr) -> Option<&'o hir::Value> {
        match out {
            hir::Expr::Constant(value) => Some(value),
            _ => None,
        }
    }

    fn tuple(&self, attributes: BTreeMap<String, hir::Expr>) -> hir::Expr {
        let constants: Option<BTreeMap<String, hir::Value>> = attributes
            .iter()
            .map(|(name, e)| Some((name.clone(), self.constant(e)?.clone())))
            .collect();
        match constants {
            Some(constants) => {
                hir::Expr::Constant(hir::Value::Tuple(hir::TupleValue::new(constants)))
            }
            None => hir::Expr::Tuple(attributes),
        }
    }

    fn relation(&self, heading: hir::Heading, tuples: Vec<hir::Expr>) -> hir::Expr {
        let constants: Option<BTreeSet<hir::TupleValue>> = tuples
            .iter()
            .map(|t| match self.constant(t)? {
                hir::Value::Tuple(t) => Some(t.clone()),
                _ => None,
            })
            .collect();
        match constants {
            Some(tuples) => {
                hir::Expr::Constant(hir::Value::Relation(hir::RelationValue { heading, tuples }))
            }
            None => hir::Expr::Relation(heading, tuples),
        }
    }
}

/// Parameters as written: `{name: text, times: integer}`.
pub(crate) fn params_text(params: &BTreeMap<String, hir::ParamType>) -> String {
    let attributes: Vec<_> = params
        .iter()
        .map(|(name, ty)| format!("{name}: {ty}"))
        .collect();
    format!("{{{}}}", attributes.join(", "))
}
