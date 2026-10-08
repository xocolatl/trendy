//! Types and values: resolving what the source writes, and checking that a
//! value has a declared type.

use std::collections::{BTreeMap, HashMap};

use trendy_diag::{Diagnostic, Span, edit_distance};
use trendy_hir as hir;
use trendy_syntax::ast::{
    AstNode, AttributeValue, Expr, Literal, LiteralError, LiteralValue, TupleLiteral, TypeExpr,
};

use crate::{Values, errors};

/// The type a type expression denotes, or `None` after reporting why it
/// denotes none.
pub(crate) fn resolve(
    type_expr: &TypeExpr,
    values: &Values,
    errors: &mut Vec<Diagnostic>,
) -> Option<hir::Type> {
    match type_expr {
        TypeExpr::Named(type_ref) => {
            let ident = type_ref.ident()?;
            let scalar = hir::Scalar::from_name(ident.text());
            if scalar.is_none() {
                errors.push(errors::unknown_type(&ident, values));
            }
            scalar.map(hir::Type::Scalar)
        }
        TypeExpr::Tuple(tuple) => {
            let mut attributes = BTreeMap::new();
            let mut first_spans = HashMap::new();
            let mut ok = true;
            for attribute in tuple.attributes() {
                let Some(ident) = attribute.name().and_then(|n| n.ident()) else {
                    continue;
                };
                let span = trendy_syntax::span(ident.text_range());
                if let Some(&first) = first_spans.get(ident.text()) {
                    errors.push(errors::duplicate_attribute(ident.text(), span, first));
                    ok = false;
                    continue;
                }
                first_spans.insert(ident.text().to_string(), span);
                match attribute
                    .type_expr()
                    .and_then(|t| resolve(&t, values, errors))
                {
                    Some(ty) => {
                        attributes.insert(ident.text().to_string(), ty);
                    }
                    None => ok = false,
                }
            }
            ok.then(|| hir::Type::Tuple(hir::Heading::new(attributes)))
        }
    }
}

/// The value an expression denotes, or `None` after reporting why it has
/// none (or if the lexer already reported it as malformed).
pub(crate) fn value(expr: &Expr, errors: &mut Vec<Diagnostic>) -> Option<hir::Value> {
    match expr {
        Expr::Literal(literal) => literal_value(literal, errors),
        Expr::Tuple(tuple) => {
            let mut attributes = BTreeMap::new();
            let mut ok = true;
            for (attribute, name, span) in named(tuple) {
                if let Some(first) = first_occurrence(tuple, &name, span) {
                    errors.push(errors::duplicate_attribute(&name, span, first));
                    ok = false;
                    continue;
                }
                match attribute.value().and_then(|v| value(&v, errors)) {
                    Some(v) => {
                        attributes.insert(name, v);
                    }
                    None => ok = false,
                }
            }
            ok.then(|| hir::Value::Tuple(hir::TupleValue::new(attributes)))
        }
    }
}

fn literal_value(literal: &Literal, errors: &mut Vec<Diagnostic>) -> Option<hir::Value> {
    match literal.value() {
        Ok(LiteralValue::Boolean(value)) => Some(hir::Value::Boolean(value)),
        Ok(LiteralValue::Integer(value)) => Some(hir::Value::Integer(value)),
        Ok(LiteralValue::Rational(value)) => Some(hir::Value::Rational(hir::Rational::new(value))),
        Ok(LiteralValue::Text(value)) => Some(hir::Value::Text(value)),
        Err(LiteralError::IntegerTooLarge) => {
            errors.push(errors::integer_out_of_range(literal.span()));
            None
        }
        Err(LiteralError::RationalTooLarge) => {
            errors.push(errors::rational_out_of_range(literal.span()));
            None
        }
        Err(LiteralError::Malformed) => None,
    }
}

/// A tuple literal's attributes with their names and name spans, in source
/// order.
fn named(tuple: &TupleLiteral) -> impl Iterator<Item = (AttributeValue, String, Span)> {
    tuple.attributes().filter_map(|attribute| {
        let ident = attribute.name()?.ident()?;
        let span = trendy_syntax::span(ident.text_range());
        Some((attribute, ident.text().to_string(), span))
    })
}

/// The span of an earlier attribute named `name` than the one at `span`.
fn first_occurrence(tuple: &TupleLiteral, name: &str, span: Span) -> Option<Span> {
    named(tuple)
        .map(|(_, n, s)| (n, s))
        .find(|(n, _)| n == name)
        .map(|(_, s)| s)
        .filter(|&s| s != span)
}

/// Check that `value`, written as `expr`, has type `ty`, written as
/// `type_expr`. Problems are reported at the innermost place they occur: a
/// wrong attribute rather than the whole tuple. Returns whether it matches.
pub(crate) fn check(
    ty: &hir::Type,
    type_expr: &TypeExpr,
    value: &hir::Value,
    expr: &Expr,
    errors: &mut Vec<Diagnostic>,
) -> bool {
    if value.ty() == *ty {
        return true;
    }
    let (hir::Type::Tuple(heading), TypeExpr::Tuple(tuple_type), Expr::Tuple(literal)) =
        (ty, type_expr, expr)
    else {
        errors.push(errors::type_mismatch(ty, type_expr, value, expr));
        return false;
    };
    let hir::Value::Tuple(tuple_value) = value else {
        errors.push(errors::type_mismatch(ty, type_expr, value, expr));
        return false;
    };

    let declared: HashMap<String, TypeExpr> = tuple_type
        .attributes()
        .filter_map(|a| Some((a.name()?.ident()?.text().to_string(), a.type_expr()?)))
        .collect();
    let given: Vec<_> = named(literal).collect();
    let given_value = |name: &str| {
        tuple_value
            .attributes()
            .find(|(n, _)| *n == name)
            .map(|(_, v)| v)
    };

    let mut ok = true;
    let mut missing: Vec<&str> = heading
        .attributes()
        .map(|(name, _)| name)
        .filter(|name| !given.iter().any(|(_, n, _)| n == name))
        .collect();
    for (attribute, name, span) in &given {
        let (Some(attr_value), Some(attr_expr)) = (given_value(name), attribute.value()) else {
            continue;
        };
        if let (Some(attr_ty), Some(attr_type_expr)) = (heading.get(name), declared.get(name)) {
            ok &= check(attr_ty, attr_type_expr, attr_value, &attr_expr, errors);
            continue;
        }
        ok = false;
        // An unknown attribute close to a missing one is probably that one,
        // misspelled; the rename resolves both problems.
        let renamed = missing
            .iter()
            .copied()
            .filter(|m| edit_distance(m, name) <= 2)
            .min_by_key(|m| edit_distance(m, name));
        match renamed {
            Some(target) => {
                missing.retain(|m| *m != target);
                errors.push(errors::unknown_attribute(
                    name,
                    *span,
                    ty,
                    Some(target),
                    literal.removal(attribute),
                ));
            }
            None => errors.push(errors::unknown_attribute(
                name,
                *span,
                ty,
                None,
                literal.removal(attribute),
            )),
        }
    }
    if !missing.is_empty() {
        let missing: Vec<_> = missing
            .into_iter()
            .filter_map(|name| Some((name, heading.get(name)?)))
            .collect();
        errors.push(errors::missing_attributes(&missing, ty, literal));
        ok = false;
    }
    ok
}
