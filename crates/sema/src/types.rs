//! Types and values: resolving what the source writes, and checking values
//! against the types expected of them.
//!
//! Checking is bidirectional: an expected type flows down into a value, so
//! that an empty relation can take its heading from context, and each
//! problem is reported at the innermost place it occurs, with a label at
//! where the expectation came from.

use std::collections::{BTreeMap, HashMap};

use trendy_diag::{Diagnostic, Span, edit_distance};
use trendy_hir as hir;
use trendy_syntax::SyntaxToken;
use trendy_syntax::ast::{
    self, AstNode, AttributeValue, Expr, Literal, LiteralError, LiteralValue, RelationLiteral,
    TupleLiteral, TypeExpr,
};

use crate::check::{Checker, Names};
use crate::ops::{Operands, Operator, Resolved};
use crate::{Namespaces, errors};

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
    /// The attribute named, of the operand written here, which a `replace`
    /// gives a new value of the same type.
    Replaced(Expr, String),
    /// The variable named, assigned a value: where its type is set, by its
    /// declared type or else its initial value.
    Variable(String, Box<Origin>),
}

impl Origin {
    /// Where to point a secondary label, if anywhere.
    pub(crate) fn span(&self) -> Option<Span> {
        match self {
            Origin::Type(t) => Some(t.span()),
            Origin::Heading(h) => Some(h.span()),
            Origin::Value(v) | Origin::Replaced(v, _) => Some(v.span()),
            Origin::Builtin => None,
            Origin::Operator(token) => Some(trendy_syntax::span(token.text_range())),
            Origin::Variable(_, declared) => declared.span(),
        }
    }

    pub(crate) fn label(&self) -> String {
        match self {
            Origin::Type(_) | Origin::Heading(_) | Origin::Builtin => {
                "expected because of this type".to_string()
            }
            Origin::Value(_) => "expected because of this value".to_string(),
            Origin::Replaced(_, name) => format!("`{name}` has the expected type here"),
            Origin::Variable(name, declared) => match **declared {
                Origin::Type(_) => format!("`{name}` is declared with this type"),
                _ => format!("`{name}` has this value's type"),
            },
            Origin::Operator(token) if token.kind() == trendy_syntax::SyntaxKind::WhereKw => {
                "`where` takes a boolean condition".to_string()
            }
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
            Origin::Variable(_, declared) => declared.attribute(name),
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
            Origin::Variable(_, declared) => declared.element(),
            Origin::Type(TypeExpr::Relation(r)) => r.heading().map(Origin::Heading),
            Origin::Heading(h) => Some(Origin::Heading(h.clone())),
            Origin::Value(Expr::Relation(r)) => r.elements().next().map(Origin::Value),
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
    namespaces: &Namespaces,
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
                errors.push(errors::unknown_type(&ident, namespaces));
            }
            scalar.map(hir::Type::Scalar)
        }
        TypeExpr::Tuple(tuple) => {
            resolve_heading(&tuple.heading()?, namespaces, errors).map(hir::Type::Tuple)
        }
        TypeExpr::Relation(relation) => {
            resolve_heading(&relation.heading()?, namespaces, errors).map(hir::Type::Relation)
        }
    }
}

pub(crate) fn resolve_heading(
    heading: &ast::Heading,
    namespaces: &Namespaces,
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
            .and_then(|t| resolve(&t, namespaces, errors))
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
/// `expected` if given, where `names` gives what names refer to, all known
/// when compiling. Returns `None` after reporting why there is no valid
/// value (or if the lexer already reported it as malformed).
pub(crate) fn value(
    expr: &Expr,
    expected: Option<&Expected>,
    names: &mut dyn Names,
    errors: &mut Vec<Diagnostic>,
) -> Option<hir::Value> {
    match Checker::new(names).expr(expr, expected, errors)? {
        hir::Expr::Constant(value) => Some(value),
        other => unreachable!("an expression of constants folds to a constant: {other:?}"),
    }
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

/// An error was already reported, so there is no result.
struct Reported;

/// A selector's own heading, which takes precedence over the context:
/// `Ok(None)` if it has none.
fn own_heading(
    heading: Option<ast::Heading>,
    wrap: fn(hir::Heading) -> hir::Type,
    namespaces: &Namespaces,
    errors: &mut Vec<Diagnostic>,
) -> Result<Option<Expected>, Reported> {
    let Some(heading) = heading else {
        return Ok(None);
    };
    let resolved = resolve_heading(&heading, namespaces, errors).ok_or(Reported)?;
    Ok(Some(Expected {
        ty: wrap(resolved),
        origin: Origin::Heading(heading),
    }))
}

/// A tuple selector, checked against `expected` if given.
pub(crate) fn tuple_selector(
    tuple: &TupleLiteral,
    expected: Option<&Expected>,
    checker: &mut Checker,
    errors: &mut Vec<Diagnostic>,
) -> Option<hir::Expr> {
    let own = own_heading(
        tuple.heading(),
        hir::Type::Tuple,
        checker.namespaces(),
        errors,
    )
    .ok()?;
    let here = own.as_ref().or(expected);
    let heading = match here {
        Some(Expected {
            ty: hir::Type::Tuple(heading),
            ..
        }) => Some(heading),
        // The context expects something else: check the tuple on its own,
        // then report the mismatch for the whole of it.
        Some(_) => {
            let out = tuple_selector(tuple, None, checker, errors)?;
            return checker.matched(expected, out, &Expr::Tuple(tuple.clone()), errors);
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
        // have is reported instead of checked.
        let attr_expected = match here.zip(heading) {
            Some((here, heading)) => {
                if let Some(attr_ty) = heading.get(name) {
                    Some(here.attribute(name, attr_ty))
                } else {
                    ok = false;
                    let removal = tuple.removal(attribute);
                    unknown_tuple_attribute(name, *span, here, &mut missing, removal, errors);
                    continue;
                }
            }
            None => None,
        };
        match checker.expr(&attr_expr, attr_expected.as_ref(), errors) {
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
    let out = crate::ops::tuple(attributes);
    match &own {
        // Checked against its own heading; now against the context.
        Some(_) => checker.matched(expected, out, &Expr::Tuple(tuple.clone()), errors),
        None => Some(out),
    }
}

/// Report the attribute `name`, at `span`, which the tuple type expected,
/// `here`, doesn't have. `removal` is the span that removes it. An unknown
/// attribute close to a `missing` one is probably that one, misspelled: the
/// fix renames it, which resolves both problems, so the missing one is no
/// longer reported.
fn unknown_tuple_attribute(
    name: &str,
    span: Span,
    here: &Expected,
    missing: &mut Vec<&str>,
    removal: Span,
    errors: &mut Vec<Diagnostic>,
) {
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
        span,
        &format!("the expected type is `{}`", here.ty),
        renamed,
        Some(removal),
    ));
}

/// A relation selector, checked against `expected` if given. Tuples known
/// when compiling must be distinct; others become one when the program runs
/// if they are equal.
pub(crate) fn relation_selector(
    relation: &RelationLiteral,
    expected: Option<&Expected>,
    checker: &mut Checker,
    errors: &mut Vec<Diagnostic>,
) -> Option<hir::Expr> {
    let own = own_heading(
        relation.heading(),
        hir::Type::Relation,
        checker.namespaces(),
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
            let out = relation_selector(relation, None, checker, errors)?;
            return checker.matched(expected, out, &Expr::Relation(relation.clone()), errors);
        }
        None => None,
    };

    let mut tuples = Vec::new();
    let mut known: BTreeMap<hir::TupleValue, Span> = BTreeMap::new();
    let mut ok = true;
    for tuple in relation.elements() {
        let Some(out) = checker.expr(&tuple, element.as_ref(), errors) else {
            ok = false;
            continue;
        };
        // Without a heading from the selector or the context, the first
        // tuple sets it for the rest.
        if element.is_none() {
            let ty = checker.ty(&out);
            if !matches!(ty, hir::Type::Tuple(_)) {
                errors.push(errors::not_a_tuple(&ty, &tuple));
                ok = false;
                continue;
            }
            element = Some(Expected {
                ty,
                origin: Origin::Value(tuple.clone()),
            });
        }
        if let Some(hir::Value::Tuple(value)) = Checker::constant(&out) {
            if let Some(&first) = known.get(value) {
                errors.push(errors::duplicate_tuple(
                    tuple.span(),
                    first,
                    relation.removal(&tuple),
                ));
                ok = false;
                continue;
            }
            known.insert(value.clone(), tuple.span());
        }
        tuples.push(out);
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
    let out = crate::ops::relation(heading, tuples);
    match &own {
        Some(_) => checker.matched(expected, out, &Expr::Relation(relation.clone()), errors),
        None => Some(out),
    }
}

pub(crate) fn literal_value(literal: &Literal, errors: &mut Vec<Diagnostic>) -> Option<hir::Value> {
    match literal.value() {
        Ok(LiteralValue::Boolean(value)) => Some(hir::Value::Boolean(value)),
        Ok(LiteralValue::Integer(value)) => Some(hir::Value::Integer(value)),
        Ok(LiteralValue::Approximate(value)) => {
            Some(hir::Value::Approximate(hir::Approximate::new(value)))
        }
        Ok(LiteralValue::Text(value)) => Some(hir::Value::Text(value)),
        Err(LiteralError::IntegerTooLarge) => {
            errors.push(errors::integer_out_of_range(literal.span()));
            None
        }
        Err(LiteralError::ApproximateTooLarge) => {
            errors.push(errors::approximate_out_of_range(literal.span()));
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
