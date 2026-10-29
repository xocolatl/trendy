//! Types and values: resolving what the source writes, and checking values
//! against the types expected of them.
//!
//! Checking is bidirectional: an expected type flows down into a value, so
//! that an empty relation can take its heading from context, and each
//! problem is reported at the innermost place it occurs, with a label at
//! where the expectation came from.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use trendy_diag::{Diagnostic, Span, renamed};
use trendy_hir as hir;
use trendy_syntax::SyntaxToken;
use trendy_syntax::ast::{
    self, ArrayLiteral, AstNode, AttributeValue, Expr, Literal, LiteralError, LiteralValue,
    RelationLiteral, TupleLiteral, TypeExpr,
};

use crate::check::{Checker, Names};
use crate::ops::{Operands, Operator};
use crate::{Namespaces, errors};

/// Where an expected type comes from: for the secondary label of a
/// mismatch, and to find where each attribute's own expectation is written.
#[derive(Debug, Clone)]
pub(crate) enum Origin {
    /// A written type: a declaration's, or an attribute's in a heading.
    Type(TypeExpr),
    /// A selector's heading, as in `relation { a: integer } { … }`, which
    /// sets the attributes of a tuple, or of a relation's tuples, or one
    /// after `relation` or `tuple` in a type. A declared heading's name
    /// leads to its attributes' types, where it is declared.
    Heading(ast::HeadingExpr),
    /// Another value that set the type, such as the tuple of a relation
    /// selector that gives the others theirs.
    Value(Expr),
    /// A built-in operator's parameter, declared in the prelude: there is no
    /// place in the program to point at.
    Builtin,
    /// An operator such as `and` or `+`, for its operands.
    Operator(SyntaxToken),
    /// An operand of the operator written here, which gives a value of its
    /// operands' type: the type expected of its result because of the
    /// origin within, which is never itself an operand's.
    Operand(SyntaxToken, Box<Origin>),
    /// The attribute named, of the operand written here, which a `replace`
    /// gives a new value of the same type.
    Replaced(Expr, String),
    /// The variable named, assigned a value: where its type is set, by its
    /// declared type or else its initial value.
    Variable(String, Box<Origin>),
    /// The array variable named, of tuples, which a load fills from a
    /// relation of their heading: where the variable's type is set.
    Loaded(String, Box<Origin>),
    /// The elements of the array written as the text, which an element
    /// assignment, as in `a[i] := x`, gives a value: where their type is
    /// set.
    Element(String, Box<Origin>),
    /// The type parameter named, of an operator called, given its type by
    /// the argument written here.
    TypeParameter(String, Expr),
}

impl Origin {
    /// The secondary labels on a mismatch: where the expectation comes from,
    /// if anywhere, and the operator that passed it to an operand.
    pub(crate) fn labels(&self) -> Vec<(Span, String)> {
        let Origin::Operand(token, within) = self else {
            return self
                .span()
                .map(|span| (span, self.label()))
                .into_iter()
                .collect();
        };
        let mut labels = within.labels();
        labels.push((
            trendy_syntax::span(token.text_range()),
            format!("the result of `{}` has its operands' type", token.text()),
        ));
        labels
    }

    /// The expectation of the result of the operator written `token`, as
    /// the expectation of each of its operands.
    pub(crate) fn operand(&self, token: &SyntaxToken) -> Origin {
        // Only the innermost operator is labeled: it explains the rest.
        let within = match self {
            Origin::Operand(_, within) => within.clone(),
            origin => Box::new(origin.clone()),
        };
        Origin::Operand(token.clone(), within)
    }

    /// Where to point a secondary label, if anywhere.
    fn span(&self) -> Option<Span> {
        match self {
            Origin::Type(t) => Some(t.span()),
            Origin::Heading(h) => Some(h.span()),
            Origin::Value(v) | Origin::Replaced(v, _) | Origin::TypeParameter(_, v) => {
                Some(v.span())
            }
            Origin::Builtin => None,
            Origin::Operator(token) => Some(trendy_syntax::span(token.text_range())),
            Origin::Variable(_, within)
            | Origin::Loaded(_, within)
            | Origin::Element(_, within)
            | Origin::Operand(_, within) => within.span(),
        }
    }

    fn label(&self) -> String {
        match self {
            Origin::Type(_) | Origin::Heading(_) | Origin::Builtin => {
                "expected because of this type".to_string()
            }
            Origin::Value(v)
                if v.syntax()
                    .parent()
                    .is_some_and(|p| p.kind() == trendy_syntax::SyntaxKind::IfExpr) =>
            {
                "an `if`'s branches have one type, this one's".to_string()
            }
            Origin::Value(_) => "expected because of this value".to_string(),
            Origin::TypeParameter(name, _) => {
                format!("`{name}` is given this argument's type")
            }
            Origin::Replaced(_, name) => format!("`{name}` has the expected type here"),
            Origin::Variable(name, declared) => match **declared {
                Origin::Type(_) => format!("`{name}` is declared with this type"),
                _ => format!("`{name}` has this value's type"),
            },
            Origin::Loaded(name, _) => {
                format!("a load into `{name}` takes a relation of its elements' heading")
            }
            Origin::Element(array, within) => match **within {
                Origin::Type(_) => format!("`{array}`'s elements are declared with this type"),
                _ => format!("`{array}`'s elements have this value's type"),
            },
            Origin::Operand(_, within) => within.label(),
            Origin::Operator(token) if token.kind() == trendy_syntax::SyntaxKind::WhereKw => {
                "`where` takes a boolean condition".to_string()
            }
            Origin::Operator(token) if token.kind() == trendy_syntax::SyntaxKind::LBracket => {
                "an array's positions are integers, counting from 1".to_string()
            }
            Origin::Operator(token)
                if token.parent().is_some_and(|p| {
                    matches!(
                        p.kind(),
                        trendy_syntax::SyntaxKind::WhileStmt | trendy_syntax::SyntaxKind::LoopStmt
                    )
                }) =>
            {
                "`while` takes a boolean condition".to_string()
            }
            // An `if`'s keyword, or another language's `elif` for one.
            Origin::Operator(token)
                if token.parent().is_some_and(|p| {
                    matches!(
                        p.kind(),
                        trendy_syntax::SyntaxKind::IfExpr | trendy_syntax::SyntaxKind::IfStmt
                    )
                }) =>
            {
                "`if` takes a boolean condition".to_string()
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

    /// The expectation for the elements of the array written `array`,
    /// whose own this is: where their type is written, if anywhere, and
    /// else the array's.
    pub(crate) fn elements(&self, array: &str) -> Origin {
        let within = self.array_element().unwrap_or_else(|| self.clone());
        Origin::Element(array.to_string(), Box::new(within))
    }

    /// Where the expectation for attribute `name` is written.
    fn attribute(&self, name: &str) -> Option<Origin> {
        match self {
            Origin::Variable(_, declared) | Origin::Element(_, declared) => {
                declared.attribute(name)
            }
            Origin::Loaded(_, declared) => declared.array_element()?.attribute(name),
            Origin::Type(TypeExpr::Tuple(t)) => Origin::Heading(t.heading()?).attribute(name),
            Origin::Heading(h) => h
                .attributes_written()?
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

    /// Where the expectation for an array's elements is written.
    fn array_element(&self) -> Option<Origin> {
        match self {
            Origin::Variable(_, declared) | Origin::Element(_, declared) => {
                declared.array_element()
            }
            Origin::Type(TypeExpr::Array(array)) => array.element().map(Origin::Type),
            Origin::Value(Expr::Array(array)) => array.elements().next().map(Origin::Value),
            _ => None,
        }
    }

    /// Where the expectation for a relation's tuples is written.
    fn element(&self) -> Option<Origin> {
        match self {
            Origin::Variable(_, declared) | Origin::Element(_, declared) => declared.element(),
            Origin::Loaded(_, declared) => declared.array_element(),
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
    result: &hir::Type,
    expr: &Expr,
    errors: &mut Vec<Diagnostic>,
) -> Option<()> {
    match expected {
        Some(e) if e.ty != *result => {
            errors.push(errors::type_mismatch(&e.ty, &e.origin, result, None, expr));
            None
        }
        _ => Some(()),
    }
}

/// The type parameters a type can name, besides the built-in types, by
/// where it is written.
pub(crate) enum TypeParameters<'a> {
    /// None: in a variable's type, or a heading's declaration, which are
    /// always of specific types.
    None,
    /// The operator's own: in its result type, or in its body.
    Fixed(&'a [hir::TypeParameter]),
    /// The operator's own, in a parameter's type, where each `any type`,
    /// `any tuple` or `any relation` adds one.
    Open(&'a mut Vec<hir::TypeParameter>),
}

impl TypeParameters<'_> {
    /// The type parameters there are so far.
    pub(crate) fn list(&self) -> &[hir::TypeParameter] {
        match self {
            TypeParameters::None => &[],
            TypeParameters::Fixed(list) => list,
            TypeParameters::Open(list) => list,
        }
    }

    /// The type parameter declared with the name `name`.
    fn named(&self, name: &str) -> Option<&hir::TypeParameter> {
        self.list().iter().find(|p| p.name.as_deref() == Some(name))
    }
}

/// What `any` says a type parameter can be, from the word with it: `type`,
/// `tuple`, `relation`, or a kind, or `None` after reporting that it names
/// none.
pub(crate) fn any_of(
    any: &ast::AnyType,
    namespaces: &Namespaces,
    errors: &mut Vec<Diagnostic>,
) -> Option<hir::Any> {
    let what = any.what()?;
    Some(match what.kind() {
        trendy_syntax::SyntaxKind::TupleKw => hir::Any::Tuple,
        trendy_syntax::SyntaxKind::RelationKw => hir::Any::Relation,
        trendy_syntax::SyntaxKind::ArrayKw => hir::Any::Array,
        _ if what.text() == "type" => hir::Any::Type,
        _ => {
            let Some(kind) = namespaces.kind(what.text()) else {
                errors.push(errors::unknown_kind(&what, namespaces));
                return None;
            };
            hir::Any::Kind(kind.clone())
        }
    })
}

/// The type a type expression denotes, where `parameters` are the type
/// parameters it can name, or `None` after reporting why it denotes none.
pub(crate) fn resolve(
    type_expr: &TypeExpr,
    parameters: &mut TypeParameters,
    namespaces: &Namespaces,
    errors: &mut Vec<Diagnostic>,
) -> Option<hir::Type> {
    match type_expr {
        TypeExpr::Named(type_ref) => {
            let ident = type_ref.ident()?;
            if let Some(parameter) = parameters.named(ident.text()) {
                return Some(hir::Type::Parameter(parameter.clone()));
            }
            let scalar = hir::Scalar::from_name(ident.text());
            if scalar.is_none() {
                errors.push(match namespaces.heading(ident.text()) {
                    Some((declared, _)) => errors::heading_as_type(&ident, declared),
                    None => errors::unknown_type(&ident, namespaces, parameters),
                });
            }
            scalar.map(hir::Type::Scalar)
        }
        TypeExpr::Tuple(tuple) => {
            resolve_heading_expr(&tuple.heading()?, parameters, namespaces, errors)
                .map(hir::Type::Tuple)
        }
        TypeExpr::Relation(relation) => {
            resolve_heading_expr(&relation.heading()?, parameters, namespaces, errors)
                .map(hir::Type::Relation)
        }
        TypeExpr::Array(array) => {
            let element = resolve(&array.element()?, parameters, namespaces, errors)?;
            Some(hir::Type::Array(Box::new(element)))
        }
        TypeExpr::Any(written) => {
            let any = any_of(written, namespaces, errors)?;
            let TypeParameters::Open(list) = parameters else {
                errors.push(errors::any_outside_parameter(
                    &any,
                    written,
                    matches!(parameters, TypeParameters::None),
                ));
                return None;
            };
            let parameter = hir::TypeParameter {
                index: list.len(),
                name: None,
                any,
            };
            list.push(parameter.clone());
            Some(hir::Type::Parameter(parameter))
        }
    }
}

/// The heading `written` after `relation` or `tuple` denotes: its own
/// attributes, or those of the heading it names, as for [`resolve`], or
/// `None` after reporting why it denotes none. A declared heading with
/// errors was reported where it is declared.
pub(crate) fn resolve_heading_expr(
    written: &ast::HeadingExpr,
    parameters: &mut TypeParameters,
    namespaces: &Namespaces,
    errors: &mut Vec<Diagnostic>,
) -> Option<hir::Heading> {
    match written {
        ast::HeadingExpr::Written(heading) => {
            resolve_heading(heading, parameters, namespaces, errors)
        }
        ast::HeadingExpr::Named(named) => {
            let ident = named.ident()?;
            let Some((_, attributes)) = namespaces.heading(ident.text()) else {
                errors.push(errors::unknown_heading(&ident, namespaces));
                return None;
            };
            attributes.cloned()
        }
    }
}

/// The heading `heading` denotes, as for [`resolve`], or `None` after
/// reporting why it denotes none. Only an operator's parameters, which
/// [`crate::operators::params`] resolves, can have `per` or a mode.
pub(crate) fn resolve_heading(
    heading: &ast::Heading,
    parameters: &mut TypeParameters,
    namespaces: &Namespaces,
    errors: &mut Vec<Diagnostic>,
) -> Option<hir::Heading> {
    let mut ok = true;
    for per in heading.attributes().filter_map(|a| a.per()) {
        errors.push(errors::per_outside_parameters(&per));
        ok = false;
    }
    for mode in heading.attributes().filter_map(|a| a.mode()) {
        errors.push(errors::mode_outside_parameters(&mode));
        ok = false;
    }
    let resolved = resolve_attributes(heading, parameters, namespaces, errors)?;
    ok.then_some(resolved)
}

/// The attributes of `heading`, with their types, as for
/// [`resolve_heading`], leaving any `per` to the caller. `None` after
/// reporting why one is invalid.
pub(crate) fn resolve_attributes(
    heading: &ast::Heading,
    parameters: &mut TypeParameters,
    namespaces: &Namespaces,
    errors: &mut Vec<Diagnostic>,
) -> Option<hir::Heading> {
    let Attributes {
        resolved,
        unresolved,
    } = attributes(heading, parameters, namespaces, errors);
    unresolved.is_empty().then(|| hir::Heading::new(resolved))
}

/// A heading's attributes, as [`attributes`] resolves them.
pub(crate) struct Attributes {
    /// Those whose types are valid.
    pub(crate) resolved: BTreeMap<String, hir::Type>,
    /// The names of the others, already reported: those whose types are
    /// invalid, and those written twice.
    pub(crate) unresolved: BTreeSet<String>,
}

/// The attributes of `heading`, each with its type, or among those
/// reported as invalid.
pub(crate) fn attributes(
    heading: &ast::Heading,
    parameters: &mut TypeParameters,
    namespaces: &Namespaces,
    errors: &mut Vec<Diagnostic>,
) -> Attributes {
    let mut resolved = BTreeMap::new();
    let mut unresolved = BTreeSet::new();
    let mut first_spans = HashMap::new();
    for attribute in heading.attributes() {
        let Some(ident) = attribute.name().and_then(|n| n.ident()) else {
            continue;
        };
        let name = ident.text().to_string();
        let span = trendy_syntax::span(ident.text_range());
        if let Some(&first) = first_spans.get(&name) {
            errors.push(errors::duplicate_attribute(
                errors::Noun::Attribute,
                &name,
                span,
                first,
            ));
            // Which of the two types it has is for the fix to decide.
            resolved.remove(&name);
            unresolved.insert(name);
            continue;
        }
        first_spans.insert(name.clone(), span);
        match attribute
            .type_expr()
            .and_then(|t| resolve(&t, parameters, namespaces, errors))
        {
            Some(ty) => {
                resolved.insert(name, ty);
            }
            None => {
                unresolved.insert(name);
            }
        }
    }
    Attributes {
        resolved,
        unresolved,
    }
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
        // Nothing in one reads input (`Checker::call`).
        other => unreachable!("an expression of constants folds to a constant: {other:?}"),
    }
}

/// An operator's result, written `expr`, or `None` after reporting why there
/// is none. `divisor` is the right operand, where a division by zero is
/// reported.
pub(crate) fn evaluated<T>(
    result: Result<T, hir::Failure>,
    expr: &Expr,
    divisor: Option<&Expr>,
    errors: &mut Vec<Diagnostic>,
) -> Option<T> {
    result
        .map_err(|failure| {
            errors.push(errors::failure(failure, expr, divisor));
        })
        .ok()
}

/// An error was already reported, so there is no result.
struct Reported;

/// A selector's own heading, which takes precedence over the context:
/// `Ok(None)` if it has none.
fn own_heading(
    heading: Option<ast::HeadingExpr>,
    wrap: fn(hir::Heading) -> hir::Type,
    checker: &Checker,
    errors: &mut Vec<Diagnostic>,
) -> Result<Option<Expected>, Reported> {
    let Some(heading) = heading else {
        return Ok(None);
    };
    let mut parameters = checker.type_parameters();
    let resolved = resolve_heading_expr(&heading, &mut parameters, checker.namespaces(), errors)
        .ok_or(Reported)?;
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
    let own = own_heading(tuple.heading(), hir::Type::Tuple, checker, errors).ok()?;
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
    if let (Some(here), false) = (here, missing.is_empty()) {
        errors.push(errors::missing_attributes(
            errors::Noun::Attribute,
            &missing,
            &format!("the expected type is `{}`", here.ty),
            tuple.span(),
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
    let renamed = renamed(name, missing.iter().copied());
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
    let own = own_heading(relation.heading(), hir::Type::Relation, checker, errors).ok()?;
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

    let elements: Vec<Expr> = relation.elements().collect();
    let mut ok = true;
    // Without a heading from the selector or the context, the first tuple
    // that is valid on its own sets it for the rest, wherever it is, as the
    // tuples have no order (see [`Alone`]).
    let mut alone: Vec<Option<Alone<hir::Expr>>> = vec![None; elements.len()];
    if element.is_none() {
        for (tuple, alone) in elements.iter().zip(&mut alone) {
            let mut own_errors = Vec::new();
            let out = checker.expr(tuple, None, &mut own_errors);
            let on_its_own = Alone::of(out, own_errors, tuple.span());
            if let Alone::Valid(out) = &on_its_own {
                let ty = checker.ty(out);
                if !matches!(ty, hir::Type::Tuple(_)) {
                    errors.push(errors::not_a_tuple(&ty, tuple));
                    *alone = Some(Alone::Failed(Vec::new()));
                    continue;
                }
                element = Some(Expected {
                    ty,
                    origin: Origin::Value(tuple.clone()),
                });
                *alone = Some(on_its_own);
                break;
            }
            *alone = Some(on_its_own);
        }
    }
    let mut tuples = Vec::new();
    let mut known: BTreeMap<hir::TupleValue, Span> = BTreeMap::new();
    for (tuple, alone) in elements.iter().zip(alone) {
        let out = match alone {
            Some(alone) => alone.finish(element.is_some(), tuple.span(), errors, |errors| {
                checker.expr(tuple, element.as_ref(), errors)
            }),
            None => checker.expr(tuple, element.as_ref(), errors),
        };
        let Some(out) = out else {
            ok = false;
            continue;
        };
        if let Some(hir::Value::Tuple(value)) = Checker::constant(&out) {
            if let Some(&first) = known.get(value) {
                errors.push(errors::duplicate_tuple(
                    tuple.span(),
                    first,
                    relation.removal(tuple),
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

/// A value checked on its own, without an expected type, to find one among
/// several that gives the others theirs, as a relation selector's tuples
/// and an operator's operands do, whose order doesn't decide which.
#[derive(Clone)]
pub(crate) enum Alone<T> {
    Valid(T),
    /// Valid but for what another's type would give it: the type of an
    /// empty relation in it, its heading. It is checked again with that
    /// type, if another gives one, and else has these errors.
    Waiting(Vec<Diagnostic>),
    /// Invalid on its own, with these errors, those of what another's type
    /// would give it among them, which are left out if another gives one.
    Failed(Vec<Diagnostic>),
}

impl<T> Alone<T> {
    /// What checking the value written at `span` on its own came to: `out`,
    /// with `errors`.
    pub(crate) fn of(out: Option<T>, errors: Vec<Diagnostic>, span: Span) -> Self {
        match out {
            Some(out) if errors.is_empty() => Alone::Valid(out),
            _ if !errors.is_empty() && errors.iter().all(|d| given(d, span)) => {
                Alone::Waiting(errors)
            }
            _ => Alone::Failed(errors),
        }
    }

    /// The value written at `span`, once another's type is known, if
    /// `typed`, which `check` checks it against again if it is waiting,
    /// after reporting its errors that remain.
    pub(crate) fn finish(
        self,
        typed: bool,
        span: Span,
        errors: &mut Vec<Diagnostic>,
        check: impl FnOnce(&mut Vec<Diagnostic>) -> Option<T>,
    ) -> Option<T> {
        match self {
            Alone::Valid(out) => Some(out),
            Alone::Waiting(_) if typed => check(errors),
            Alone::Waiting(own) => {
                errors.extend(own);
                None
            }
            Alone::Failed(own) => {
                errors.extend(own.into_iter().filter(|d| !(typed && given(d, span))));
                None
            }
        }
    }
}

/// Whether `error`, of a value written at `span`, is one that another
/// value's type gives what it lacks: an empty relation's unknown heading,
/// in the value. Errors elsewhere, as in an operator it calls, checked
/// then, aren't.
fn given(error: &Diagnostic, span: Span) -> bool {
    let at = error.span();
    error.code == trendy_diag::Code::UnknownEmptyType
        && span.start <= at.start
        && at.end <= span.end
}

/// An array selector, checked against `expected` if given. Its elements'
/// type comes from the selector, as in `array integer []`, the type
/// expected of it, or an element that is valid on its own, whichever it is
/// (see [`Alone`]).
pub(crate) fn array_selector(
    array: &ArrayLiteral,
    expected: Option<&Expected>,
    checker: &mut Checker,
    errors: &mut Vec<Diagnostic>,
) -> Option<hir::Expr> {
    let own = match array.element_type() {
        Some(written) => {
            let mut parameters = checker.type_parameters();
            let ty = resolve(&written, &mut parameters, checker.namespaces(), errors)?;
            Some(Expected {
                ty: hir::Type::Array(Box::new(ty)),
                origin: Origin::Type(written),
            })
        }
        None => None,
    };
    let here = own.as_ref().or(expected);
    let mut element = match here {
        Some(Expected {
            ty: hir::Type::Array(element),
            origin,
        }) => Some(Expected {
            ty: (**element).clone(),
            origin: origin.array_element().unwrap_or_else(|| origin.clone()),
        }),
        Some(_) => {
            let out = array_selector(array, None, checker, errors)?;
            return checker.matched(expected, out, &Expr::Array(array.clone()), errors);
        }
        None => None,
    };
    let elements: Vec<Expr> = array.elements().collect();
    let mut alone: Vec<Option<Alone<hir::Expr>>> = vec![None; elements.len()];
    if element.is_none() {
        for (written, alone) in elements.iter().zip(&mut alone) {
            let mut own_errors = Vec::new();
            let out = checker.expr(written, None, &mut own_errors);
            let on_its_own = Alone::of(out, own_errors, written.span());
            if let Alone::Valid(out) = &on_its_own {
                element = Some(Expected {
                    ty: checker.ty(out),
                    origin: Origin::Value(written.clone()),
                });
                *alone = Some(on_its_own);
                break;
            }
            *alone = Some(on_its_own);
        }
    }
    let mut ok = true;
    let mut values = Vec::new();
    for (written, alone) in elements.iter().zip(alone) {
        let out = match alone {
            Some(alone) => alone.finish(element.is_some(), written.span(), errors, |errors| {
                checker.expr(written, element.as_ref(), errors)
            }),
            None => checker.expr(written, element.as_ref(), errors),
        };
        match out {
            Some(out) => values.push(out),
            None => ok = false,
        }
    }
    // An element with errors was already reported; it may be why there is
    // no type, so that isn't reported again.
    if !ok {
        return None;
    }
    let Some(Expected { ty: element, .. }) = element else {
        errors.push(errors::unknown_array_type(array.span()));
        return None;
    };
    let out = crate::ops::array(element, values);
    match &own {
        Some(_) => checker.matched(expected, out, &Expr::Array(array.clone()), errors),
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
