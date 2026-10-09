//! Types and values: resolving what the source writes, and checking values
//! against the types expected of them.
//!
//! Checking is bidirectional: an expected type flows down into a value, so
//! that an empty relation can take its heading from context, and each
//! problem is reported at the innermost place it occurs, with a label at
//! where the expectation came from.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use trendy_diag::{Diagnostic, Span, edit_distance};
use trendy_hir as hir;
use trendy_syntax::SyntaxToken;
use trendy_syntax::ast::{
    self, AstNode, AttributeValue, Expr, Literal, LiteralError, LiteralValue, RelationLiteral,
    TupleLiteral, TypeExpr,
};

use crate::ops::{self, Operands, Operator, Resolved};
use crate::{Values, errors};

/// Where an expected type comes from: for the secondary label of a
/// mismatch, and to find where each attribute's own expectation is written.
#[derive(Debug, Clone)]
pub(crate) enum Origin {
    /// A written type: a declaration's, or an attribute's in a heading.
    Type(TypeExpr),
    /// A selector's heading, as in `relation { a: integer } { … }`, which
    /// sets the attributes of a tuple, or of a relation's tuples.
    Heading(ast::Heading),
    /// An earlier value that set the type, such as a relation's first tuple.
    Value(Expr),
    /// A built-in operator's parameter, declared in the prelude: there is no
    /// place in the program to point at.
    Builtin,
    /// An operator such as `and` or `+`, for its operands.
    Operator(SyntaxToken),
}

impl Origin {
    /// Where to point a secondary label, if anywhere.
    pub(crate) fn span(&self) -> Option<Span> {
        match self {
            Origin::Type(t) => Some(t.span()),
            Origin::Heading(h) => Some(h.span()),
            Origin::Value(v) => Some(v.span()),
            Origin::Builtin => None,
            Origin::Operator(token) => Some(trendy_syntax::span(token.text_range())),
        }
    }

    pub(crate) fn label(&self) -> String {
        match self {
            Origin::Type(_) | Origin::Heading(_) | Origin::Builtin => {
                "expected because of this type".to_string()
            }
            Origin::Value(_) => "expected because of this value".to_string(),
            // Only operators with a fixed operand type are origins.
            Origin::Operator(token) => match Operator::of(token).map(|op| (op, op.operands())) {
                Some((op, Operands::Fixed(scalar))) if op.is_prefix() => {
                    let ty = scalar.name();
                    let article = if ty.starts_with(['a', 'e', 'i', 'o', 'u']) {
                        "an"
                    } else {
                        "a"
                    };
                    format!("`{}` takes {article} {ty} operand", token.text())
                }
                Some((_, Operands::Fixed(scalar))) => {
                    format!("`{}` takes {} operands", token.text(), scalar.name())
                }
                _ => "expected because of this operator".to_string(),
            },
        }
    }

    /// Where the expectation for attribute `name` is written.
    fn attribute(&self, name: &str) -> Option<Origin> {
        match self {
            Origin::Type(TypeExpr::Tuple(t)) => Origin::Heading(t.heading()?).attribute(name),
            Origin::Heading(h) => h
                .attributes()
                .find(|a| {
                    a.name()
                        .and_then(|n| n.ident())
                        .is_some_and(|i| i.text() == name)
                })?
                .type_expr()
                .map(Origin::Type),
            Origin::Value(Expr::Tuple(t)) => t
                .attributes()
                .find(|a| {
                    a.name()
                        .and_then(|n| n.ident())
                        .is_some_and(|i| i.text() == name)
                })?
                .value()
                .map(Origin::Value),
            _ => None,
        }
    }

    /// Where the expectation for a relation's tuples is written.
    fn element(&self) -> Option<Origin> {
        match self {
            Origin::Type(TypeExpr::Relation(r)) => r.heading().map(Origin::Heading),
            Origin::Heading(h) => Some(Origin::Heading(h.clone())),
            Origin::Value(Expr::Relation(r)) => {
                r.tuples().next().map(|t| Origin::Value(Expr::Tuple(t)))
            }
            _ => None,
        }
    }
}

/// A type that a value must have, and where that requirement comes from.
#[derive(Debug, Clone)]
pub(crate) struct Expected {
    pub(crate) ty: hir::Type,
    pub(crate) origin: Origin,
}

impl Expected {
    /// The expectation for attribute `name`, of type `ty`, inside this one.
    fn attribute(&self, name: &str, ty: &hir::Type) -> Expected {
        Expected {
            ty: ty.clone(),
            origin: self
                .origin
                .attribute(name)
                .unwrap_or_else(|| self.origin.clone()),
        }
    }
}

/// What names in a value can refer to, where its value must be known when
/// compiling.
pub(crate) trait Scope {
    fn values(&self) -> &Values;

    /// The value `name` refers to, or `None` after reporting why it has none
    /// that is known when compiling.
    fn constant(&mut self, name: &ast::NameRef, errors: &mut Vec<Diagnostic>)
    -> Option<hir::Value>;
}

/// Check that an operator's result, of type `result` and written `expr`,
/// is what the context expects; `None` after reporting that it isn't.
pub(crate) fn result_type(
    expected: Option<&Expected>,
    result: hir::Scalar,
    expr: &Expr,
    errors: &mut Vec<Diagnostic>,
) -> Option<()> {
    let result = hir::Type::Scalar(result);
    match expected {
        Some(e) if e.ty != result => {
            errors.push(errors::type_mismatch(&e.ty, &e.origin, &result, None, expr));
            None
        }
        _ => Some(()),
    }
}

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
            if ident.text() == "any" {
                errors.push(errors::any_outside_builtin(trendy_syntax::span(
                    ident.text_range(),
                )));
                return None;
            }
            let scalar = hir::Scalar::from_name(ident.text());
            if scalar.is_none() {
                errors.push(errors::unknown_type(&ident, values));
            }
            scalar.map(hir::Type::Scalar)
        }
        TypeExpr::Tuple(tuple) => {
            resolve_heading(&tuple.heading()?, values, errors).map(hir::Type::Tuple)
        }
        TypeExpr::Relation(relation) => {
            resolve_heading(&relation.heading()?, values, errors).map(hir::Type::Relation)
        }
    }
}

pub(crate) fn resolve_heading(
    heading: &ast::Heading,
    values: &Values,
    errors: &mut Vec<Diagnostic>,
) -> Option<hir::Heading> {
    let mut attributes = BTreeMap::new();
    let mut first_spans = HashMap::new();
    let mut ok = true;
    for attribute in heading.attributes() {
        let Some(ident) = attribute.name().and_then(|n| n.ident()) else {
            continue;
        };
        let span = trendy_syntax::span(ident.text_range());
        if let Some(&first) = first_spans.get(ident.text()) {
            errors.push(errors::duplicate_attribute(
                errors::Noun::Attribute,
                ident.text(),
                span,
                first,
            ));
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
    ok.then(|| hir::Heading::new(attributes))
}

/// The value `expr` denotes, evaluated when compiling and checked against
/// `expected` if given. Returns `None` after reporting why there is no valid
/// value (or if the lexer already reported it as malformed).
pub(crate) fn value(
    expr: &Expr,
    expected: Option<&Expected>,
    scope: &mut dyn Scope,
    errors: &mut Vec<Diagnostic>,
) -> Option<hir::Value> {
    match expr {
        Expr::Literal(literal) => {
            let value = literal_value(literal, errors)?;
            matches(expected, value, expr, errors)
        }
        Expr::Tuple(tuple) => tuple_value(tuple, expected, scope, errors),
        Expr::Relation(relation) => relation_value(relation, expected, scope, errors),
        Expr::Name(name) => {
            let value = scope.constant(name, errors)?;
            matches(expected, value, expr, errors)
        }
        Expr::Paren(paren) => value(&paren.inner()?, expected, scope, errors),
        Expr::Project(_) | Expr::Postfix(_) => {
            let projection = match expr {
                Expr::Project(p) => ops::Projection::Project(p),
                Expr::Postfix(r) => ops::Projection::Remove(r),
                _ => unreachable!("matched above"),
            };
            let operand = projection.operand()?;
            let value = value(&operand, None, scope, errors)?;
            let names = ops::removed(projection, &operand, &value.ty(), errors)?;
            let result = if names.is_empty() {
                value
            } else {
                value.without(&names)
            };
            matches(expected, result, expr, errors)
        }
        Expr::Unary(unary) => {
            if let Some(literal) = ops::negated_literal(unary) {
                let value = ops::negative_value(&literal, expr, errors)?;
                return matches(expected, value, expr, errors);
            }
            let token = unary.operator()?;
            let (resolved, operands) =
                operands(&token, vec![unary.operand()], expected, scope, errors)?;
            result_type(expected, resolved.result(), expr, errors)?;
            let [operand] = <[hir::Value; 1]>::try_from(operands).ok()?;
            match resolved {
                Resolved::Identity(_) => Some(operand),
                Resolved::Unary(op) => evaluated(op.apply(&operand), resolved, expr, None, errors),
                Resolved::Binary(_) => None,
            }
        }
        Expr::Binary(binary) => {
            let token = binary.operator()?;
            let (resolved, operands) = operands(
                &token,
                vec![binary.lhs(), binary.rhs()],
                expected,
                scope,
                errors,
            )?;
            result_type(expected, resolved.result(), expr, errors)?;
            let (Resolved::Binary(op), [lhs, rhs]) =
                (resolved, <[hir::Value; 2]>::try_from(operands).ok()?)
            else {
                return None;
            };
            evaluated(
                op.apply(&lhs, &rhs),
                resolved,
                expr,
                binary.rhs().as_ref(),
                errors,
            )
        }
    }
}

/// The operator written `token` and the values of its `operands`.
fn operands(
    token: &SyntaxToken,
    operands: Vec<Option<Expr>>,
    expected: Option<&Expected>,
    scope: &mut dyn Scope,
    errors: &mut Vec<Diagnostic>,
) -> Option<(Resolved, Vec<hir::Value>)> {
    let op = Operator::of(token)?;
    ops::check_operands(
        op,
        token,
        operands,
        expected,
        &mut |operand, expected, errors| {
            let value = value(operand, expected, scope, errors)?;
            let ty = value.ty();
            Some((value, ty))
        },
        errors,
    )
}

/// An operator's result, written `expr`, or `None` after reporting why there
/// is none. `divisor` is the right operand, where a division by zero is
/// reported.
pub(crate) fn evaluated<T>(
    result: Result<T, hir::Failure>,
    resolved: Resolved,
    expr: &Expr,
    divisor: Option<&Expr>,
    errors: &mut Vec<Diagnostic>,
) -> Option<T> {
    result
        .map_err(|failure| {
            errors.push(errors::failure(failure, resolved.result(), expr, divisor));
        })
        .ok()
}

/// `value`, if it has the expected type; otherwise report the mismatch.
fn matches(
    expected: Option<&Expected>,
    value: hir::Value,
    expr: &Expr,
    errors: &mut Vec<Diagnostic>,
) -> Option<hir::Value> {
    match expected {
        Some(e) if value.ty() != e.ty => {
            errors.push(errors::type_mismatch(
                &e.ty,
                &e.origin,
                &value.ty(),
                Some(&value),
                expr,
            ));
            None
        }
        _ => Some(value),
    }
}

/// An error was already reported, so there is no result.
struct Reported;

/// A selector's own heading, which takes precedence over the context:
/// `Ok(None)` if it has none.
fn own_heading(
    heading: Option<ast::Heading>,
    wrap: fn(hir::Heading) -> hir::Type,
    values: &Values,
    errors: &mut Vec<Diagnostic>,
) -> Result<Option<Expected>, Reported> {
    let Some(heading) = heading else {
        return Ok(None);
    };
    let resolved = resolve_heading(&heading, values, errors).ok_or(Reported)?;
    Ok(Some(Expected {
        ty: wrap(resolved),
        origin: Origin::Heading(heading),
    }))
}

fn tuple_value(
    tuple: &TupleLiteral,
    expected: Option<&Expected>,
    scope: &mut dyn Scope,
    errors: &mut Vec<Diagnostic>,
) -> Option<hir::Value> {
    let own = own_heading(tuple.heading(), hir::Type::Tuple, scope.values(), errors).ok()?;
    let here = own.as_ref().or(expected);
    let heading = match here {
        Some(Expected {
            ty: hir::Type::Tuple(heading),
            ..
        }) => Some(heading),
        // The context expects something else: evaluate the tuple on its own,
        // then report the mismatch for the whole of it.
        Some(_) => {
            let value = tuple_value(tuple, None, scope, errors)?;
            return matches(expected, value, &Expr::Tuple(tuple.clone()), errors);
        }
        None => None,
    };

    let given: Vec<_> = named(tuple).collect();
    let mut attributes = BTreeMap::new();
    let mut ok = true;
    let mut missing: Vec<&str> = heading.map_or_else(Vec::new, |h| {
        h.attributes()
            .map(|(name, _)| name)
            .filter(|name| !given.iter().any(|(_, n, _)| n == name))
            .collect()
    });
    for (attribute, name, span) in &given {
        if let Some(first) = first_occurrence(&given, name, *span) {
            errors.push(errors::duplicate_attribute(
                errors::Noun::Attribute,
                name,
                *span,
                first,
            ));
            ok = false;
            continue;
        }
        let Some(attr_expr) = attribute.value() else {
            continue;
        };
        // Against the heading, if there is one; an attribute it doesn't
        // have is reported instead of evaluated.
        let attr_expected = match here.zip(heading) {
            Some((here, heading)) => {
                if let Some(attr_ty) = heading.get(name) {
                    Some(here.attribute(name, attr_ty))
                } else {
                    ok = false;
                    // An unknown attribute close to a missing one is probably
                    // that one, misspelled; the rename resolves both problems.
                    let renamed = missing
                        .iter()
                        .copied()
                        .filter(|m| edit_distance(m, name) <= 2)
                        .min_by_key(|m| edit_distance(m, name));
                    if let Some(target) = renamed {
                        missing.retain(|m| *m != target);
                    }
                    errors.push(errors::unknown_attribute(
                        errors::Noun::Attribute,
                        name,
                        *span,
                        &format!("the expected type is `{}`", here.ty),
                        renamed,
                        tuple.removal(attribute),
                    ));
                    continue;
                }
            }
            None => None,
        };
        match value(&attr_expr, attr_expected.as_ref(), scope, errors) {
            Some(v) => {
                attributes.insert(name.clone(), v);
            }
            None => ok = false,
        }
    }
    if let (Some(here), Some(heading), false) = (here, heading, missing.is_empty()) {
        let missing: Vec<_> = missing
            .into_iter()
            .filter_map(|name| Some((name, Some(heading.get(name)?))))
            .collect();
        errors.push(errors::missing_attributes(
            errors::Noun::Attribute,
            &missing,
            &format!("the expected type is `{}`", here.ty),
            tuple.span(),
            tuple.insertion(),
        ));
        ok = false;
    }
    if !ok {
        return None;
    }
    let value = hir::Value::Tuple(hir::TupleValue::new(attributes));
    match &own {
        // Checked against its own heading; now against the context.
        Some(_) => matches(expected, value, &Expr::Tuple(tuple.clone()), errors),
        None => Some(value),
    }
}

fn relation_value(
    relation: &RelationLiteral,
    expected: Option<&Expected>,
    scope: &mut dyn Scope,
    errors: &mut Vec<Diagnostic>,
) -> Option<hir::Value> {
    let own = own_heading(
        relation.heading(),
        hir::Type::Relation,
        scope.values(),
        errors,
    )
    .ok()?;
    let here = own.as_ref().or(expected);
    // What each tuple must be: the tuple type of the relation's heading.
    let mut element = match here {
        Some(Expected {
            ty: hir::Type::Relation(heading),
            origin,
        }) => Some(Expected {
            ty: hir::Type::Tuple(heading.clone()),
            origin: origin.element().unwrap_or_else(|| origin.clone()),
        }),
        Some(_) => {
            let value = relation_value(relation, None, scope, errors)?;
            return matches(expected, value, &Expr::Relation(relation.clone()), errors);
        }
        None => None,
    };

    let mut tuples: BTreeMap<hir::TupleValue, Span> = BTreeMap::new();
    let mut ok = true;
    for tuple in relation.tuples() {
        let Some(hir::Value::Tuple(value)) = tuple_value(&tuple, element.as_ref(), scope, errors)
        else {
            ok = false;
            continue;
        };
        // Without a heading from the selector or the context, the first
        // tuple sets it for the rest.
        if element.is_none() {
            element = Some(Expected {
                ty: hir::Value::Tuple(value.clone()).ty(),
                origin: Origin::Value(Expr::Tuple(tuple.clone())),
            });
        }
        if let Some(&first) = tuples.get(&value) {
            errors.push(errors::duplicate_tuple(
                tuple.span(),
                first,
                relation.removal(&tuple),
            ));
            ok = false;
            continue;
        }
        tuples.insert(value, tuple.span());
    }

    // A tuple with errors was already reported; it may be why there is no
    // heading, so that isn't reported again.
    if !ok {
        return None;
    }
    let Some(Expected {
        ty: hir::Type::Tuple(heading),
        ..
    }) = element
    else {
        errors.push(errors::unknown_relation_heading(relation.span()));
        return None;
    };
    let value = hir::Value::Relation(hir::RelationValue {
        heading,
        tuples: tuples.into_keys().collect::<BTreeSet<_>>(),
    });
    match &own {
        Some(_) => matches(expected, value, &Expr::Relation(relation.clone()), errors),
        None => Some(value),
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

/// The span of an attribute named `name` before the one at `span`.
fn first_occurrence(
    given: &[(AttributeValue, String, Span)],
    name: &str,
    span: Span,
) -> Option<Span> {
    given
        .iter()
        .find(|(_, n, _)| n == name)
        .map(|&(_, _, s)| s)
        .filter(|&s| s != span)
}
