//! Operators: their parameters, and the statements and expressions in their
//! bodies.

use std::collections::{BTreeMap, HashMap};

use trendy_diag::{Diagnostic, Span, edit_distance};
use trendy_hir as hir;
use trendy_syntax::ast::{self, AstNode, AttributeValue, VarDecl};

use crate::check::{Checker, Lookup, Names};
use crate::types::{self, Expected, Origin};
use crate::{Kind, Values, assign, errors};

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
    /// Each variable's declaration, by name.
    pub(crate) declarations: &'a HashMap<String, VarDecl>,
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
        let statement = match &stmt {
            ast::Statement::Call(stmt) => call(stmt, within, cx, errors),
            ast::Statement::Assign(stmt) => {
                assign::statement(&stmt.clauses().collect::<Vec<_>>(), within, cx, errors)
            }
            ast::Statement::Block(block) => assign::block(block, within, cx, errors),
        };
        match statement {
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
            .and_then(|v| checked(&v, expected.as_ref(), within, cx, errors))
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

/// `expr`, in the body of `within`, checked against `expected` if given.
fn checked(
    expr: &ast::Expr,
    expected: Option<&Expected>,
    within: hir::OperatorId,
    cx: &Context,
    errors: &mut Vec<Diagnostic>,
) -> Option<hir::Expr> {
    Checker::new(&mut BodyNames { cx, within }).expr(expr, expected, errors)
}

/// Names in the body of `within`: its parameters, which hide variables of
/// the same name, and the variables.
pub(crate) struct BodyNames<'a, 'b> {
    pub(crate) cx: &'a Context<'b>,
    pub(crate) within: hir::OperatorId,
}

impl Names for BodyNames<'_, '_> {
    fn values(&self) -> &Values {
        self.cx.values
    }

    fn resolve(&mut self, name: &str, span: Span, errors: &mut Vec<Diagnostic>) -> Lookup {
        if self.cx.operators[self.within.0].params.contains_key(name) {
            return Lookup::Found(hir::Expr::Parameter(name.to_string()));
        }
        if let Some(&id) = self.cx.variable_ids.get(name) {
            return Lookup::Found(hir::Expr::Variable(id));
        }
        match self.cx.values.get(name) {
            // A variable with errors in its declaration is already reported.
            Some((Kind::Variable, _)) => Lookup::Reported,
            Some((Kind::Operator | Kind::Builtin, _)) => {
                errors.push(errors::operator_as_value(name, span));
                Lookup::Reported
            }
            None => Lookup::Unknown,
        }
    }

    fn candidates(&self) -> Vec<String> {
        self.cx.operators[self.within.0]
            .params
            .keys()
            .chain(self.cx.variable_ids.keys())
            .cloned()
            .collect()
    }

    fn leaf_type(&self, leaf: &hir::Expr) -> hir::Type {
        match leaf {
            hir::Expr::Variable(id) => self.cx.variables[id.0].ty.clone(),
            hir::Expr::Parameter(name) => match &self.cx.operators[self.within.0].params[name] {
                hir::ParamType::Type(ty) => ty.clone(),
                hir::ParamType::Any => unreachable!("only built-ins have `any` parameters"),
            },
            _ => unreachable!("only variables and parameters are leaves"),
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
