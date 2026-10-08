//! Operators: their parameters, and the statements and expressions in their
//! bodies.

use std::collections::{BTreeMap, HashMap};

use trendy_diag::{Diagnostic, Span, edit_distance};
use trendy_hir as hir;
use trendy_syntax::ast::{self, AstNode, AttributeValue, Expr};

use crate::types::{self, Expected, Origin, Scope};
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
                args.removal(arg),
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
/// variables is evaluated now, to a constant.
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
            let operator = unary.operator()?;
            let operand = unary.operand().and_then(|operand| {
                let expected = Expected::operand(operator);
                self::expr(&operand, Some(&expected), within, cx, errors)
            });
            types::boolean_result(expected, expr, errors)?;
            Some(negation(operand?))
        }
        Expr::Binary(binary) => {
            let operator = binary.operator()?;
            let op = types::bool_op(&operator)?;
            let mut operand = |side: Option<Expr>| {
                let expected = Expected::operand(operator.clone());
                self::expr(&side?, Some(&expected), within, cx, errors)
            };
            let lhs = operand(binary.lhs());
            let rhs = operand(binary.rhs());
            types::boolean_result(expected, expr, errors)?;
            Some(dyadic(op, lhs?, rhs?))
        }
        Expr::Literal(_) | Expr::Tuple(_) | Expr::Relation(_) => {
            let mut scope = Selector { cx, within };
            types::value(expr, expected, &mut scope, errors).map(hir::Expr::Constant)
        }
    }
}

/// The constant `b`, if `expr` is one.
fn constant(expr: &hir::Expr) -> Option<bool> {
    match expr {
        hir::Expr::Constant(hir::Value::Boolean(b)) => Some(*b),
        _ => None,
    }
}

fn boolean(b: bool) -> hir::Expr {
    hir::Expr::Constant(hir::Value::Boolean(b))
}

/// `not operand`, simplified. Expressions have no side effects, so whatever
/// a constant operand decides needn't be computed.
fn negation(operand: hir::Expr) -> hir::Expr {
    match operand {
        hir::Expr::Constant(hir::Value::Boolean(b)) => boolean(!b),
        hir::Expr::Not(inner) => *inner,
        operand => hir::Expr::Not(Box::new(operand)),
    }
}

/// `lhs op rhs`, simplified as for [`negation`].
fn dyadic(op: hir::BoolOp, lhs: hir::Expr, rhs: hir::Expr) -> hir::Expr {
    use hir::BoolOp::{And, Implies, Or, Xor};
    match (op, constant(&lhs), constant(&rhs)) {
        (_, Some(a), Some(b)) => boolean(op.apply(a, b)),
        // The constant leaves the other operand as it is,
        (And | Implies, Some(true), None) | (Or | Xor, Some(false), None) => rhs,
        (And, None, Some(true)) | (Or | Xor, None, Some(false)) => lhs,
        // negates it,
        (Xor, Some(true), None) => negation(rhs),
        (Xor, None, Some(true)) | (Implies, None, Some(false)) => negation(lhs),
        // or decides the result.
        (And, Some(false), None) | (And, None, Some(false)) => boolean(false),
        (Or, Some(true), None)
        | (Or | Implies, None, Some(true))
        | (Implies, Some(false), None) => boolean(true),
        _ => hir::Expr::Binary(op, Box::new(lhs), Box::new(rhs)),
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

/// A tuple or relation value in a body, which is built when compiling, so
/// that its attributes can't refer to parameters or variables.
struct Selector<'a, 'b> {
    cx: &'a Context<'b>,
    within: hir::OperatorId,
}

impl Scope for Selector<'_, '_> {
    fn values(&self) -> &Values {
        self.cx.values
    }

    fn constant(
        &mut self,
        name_ref: &ast::NameRef,
        errors: &mut Vec<Diagnostic>,
    ) -> Option<hir::Value> {
        let ident = name_ref.ident()?;
        let name = ident.text();
        let span = trendy_syntax::span(ident.text_range());
        if self.cx.operators[self.within.0].params.contains_key(name) {
            errors.push(errors::name_in_selector(name, span, "a parameter"));
        } else if let Some((Kind::Variable, _)) = self.cx.values.get(name) {
            errors.push(errors::name_in_selector(name, span, "a variable"));
        } else {
            self.cx.not_a_value(name, span, self.within, errors);
        }
        None
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
