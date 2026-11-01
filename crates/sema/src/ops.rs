//! Operators in expressions: what each token denotes, checking operand
//! types, and evaluating and simplifying what can be known when compiling.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use trendy_diag::{Diagnostic, Span, closest};
use trendy_hir as hir;
use trendy_syntax::ast::{self, AstNode, Expr, Literal, LiteralError};
use trendy_syntax::{SyntaxKind, SyntaxToken, literal};

use crate::errors;
use crate::operators::Operators;
use crate::types::{self, Expected, Origin};

/// What an operator token denotes, before its operand type is known. Sema
/// sees only trees without syntax errors, so foreign spellings such as `&&`
/// never reach it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Operator {
    Not,
    /// Prefix `-`.
    Negate,
    /// Prefix `+`, which leaves its operand as it is.
    Plus,
    And,
    Or,
    Xor,
    Implies,
    Add,
    Subtract,
    Multiply,
    /// `/`
    Divide,
    Div,
    Mod,
    /// `||`
    Concatenate,
    Compare(hir::Comparison),
}

/// What an operator's operands must be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Operands {
    /// Always this type.
    Fixed(hir::Scalar),
    /// Of a type the operator works on, all the same: the type comes from
    /// the context or, failing that, from the first operand.
    Inferred,
}

/// An operator with its operand type known.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Resolved {
    /// Prefix `+`, whose result is its operand.
    Identity,
    Unary(hir::UnaryOp),
    Binary(hir::BinaryOp),
    /// `and`, `or` or `xor`, or another that is one of them of its
    /// operands, the left one negated if `not_left`: `a implies b` is `not
    /// a or b`.
    Connective {
        connective: hir::Connective,
        not_left: bool,
    },
}

impl Operator {
    pub(crate) fn of(token: &SyntaxToken) -> Option<Operator> {
        let prefix = token
            .parent()
            .is_some_and(|p| p.kind() == SyntaxKind::UnaryExpr);
        Some(match (token.kind(), prefix) {
            (SyntaxKind::NotKw, _) => Operator::Not,
            (SyntaxKind::Minus, true) => Operator::Negate,
            (SyntaxKind::Plus, true) => Operator::Plus,
            (SyntaxKind::AndKw, _) => Operator::And,
            (SyntaxKind::OrKw, _) => Operator::Or,
            (SyntaxKind::XorKw, _) => Operator::Xor,
            (SyntaxKind::ImpliesKw, _) => Operator::Implies,
            (SyntaxKind::Plus, false) => Operator::Add,
            (SyntaxKind::Minus, false) => Operator::Subtract,
            (SyntaxKind::Star, _) => Operator::Multiply,
            (SyntaxKind::Slash, _) => Operator::Divide,
            (SyntaxKind::DivKw, _) => Operator::Div,
            (SyntaxKind::ModKw, _) => Operator::Mod,
            (SyntaxKind::PipePipe, _) => Operator::Concatenate,
            (SyntaxKind::Eq, _) => Operator::Compare(hir::Comparison::Equal),
            (SyntaxKind::LtGt, _) => Operator::Compare(hir::Comparison::NotEqual),
            (SyntaxKind::Lt, _) => Operator::Compare(hir::Comparison::Less),
            (SyntaxKind::LtEq, _) => Operator::Compare(hir::Comparison::LessOrEqual),
            (SyntaxKind::Gt, _) => Operator::Compare(hir::Comparison::Greater),
            (SyntaxKind::GtEq, _) => Operator::Compare(hir::Comparison::GreaterOrEqual),
            _ => return None,
        })
    }

    /// Whether the operator is a connective of booleans: `and`, `or`,
    /// `xor` or `implies`.
    fn is_connective(self) -> bool {
        matches!(
            self,
            Operator::And | Operator::Or | Operator::Xor | Operator::Implies
        )
    }

    pub(crate) fn is_prefix(self) -> bool {
        matches!(self, Operator::Not | Operator::Negate | Operator::Plus)
    }

    /// What the operands must be. Every operator but the boolean ones takes
    /// its type from the operands, so that an operator applied to a type it
    /// doesn't take, such as `+` to texts, is reported at the operator, with
    /// the one that fits.
    pub(crate) fn operands(self) -> Operands {
        match self {
            Operator::Not | Operator::And | Operator::Or | Operator::Xor | Operator::Implies => {
                Operands::Fixed(hir::Scalar::Boolean)
            }
            Operator::Negate
            | Operator::Plus
            | Operator::Add
            | Operator::Subtract
            | Operator::Multiply
            | Operator::Divide
            | Operator::Div
            | Operator::Mod
            | Operator::Concatenate
            | Operator::Compare(_) => Operands::Inferred,
        }
    }

    /// Whether the result has the operands' type, so that the type expected
    /// of the result is the type of the operands. A comparison's result is a
    /// boolean whatever it compares.
    fn result_is_operand(self) -> bool {
        !matches!(self, Operator::Compare(_))
    }

    /// What a kind must require for its types' values to take the operator:
    /// every type has `=` and `<>`, and no kind requires the others yet.
    pub(crate) fn requirement(self) -> Option<hir::Requirement> {
        match self {
            Operator::Compare(hir::Comparison::Equal | hir::Comparison::NotEqual)
            | Operator::Not
            | Operator::And
            | Operator::Or
            | Operator::Xor
            | Operator::Implies
            | Operator::Negate
            | Operator::Subtract
            | Operator::Divide
            | Operator::Div
            | Operator::Mod
            | Operator::Concatenate => None,
            Operator::Compare(_) => Some(hir::Requirement::Order),
            Operator::Plus | Operator::Add => Some(hir::Requirement::Addition),
            Operator::Multiply => Some(hir::Requirement::Multiplication),
        }
    }

    /// The type of the result, for operands of type `operands`.
    fn result(self, operands: hir::Type) -> hir::Type {
        if self.result_is_operand() {
            operands
        } else {
            hir::Type::Scalar(hir::Scalar::Boolean)
        }
    }

    /// The types the operator takes, as written in an error message.
    pub(crate) fn takes(self) -> &'static str {
        match self {
            Operator::Not | Operator::And | Operator::Or | Operator::Xor | Operator::Implies => {
                "booleans"
            }
            Operator::Negate
            | Operator::Plus
            | Operator::Add
            | Operator::Subtract
            | Operator::Multiply => "integers or approximate numbers",
            Operator::Divide => "approximate numbers",
            Operator::Div | Operator::Mod => "integers",
            Operator::Concatenate => "text",
            Operator::Compare(hir::Comparison::Equal | hir::Comparison::NotEqual) => {
                "two values of the same type"
            }
            Operator::Compare(_) => {
                "booleans, integers, approximate numbers, text, or values of a type with an order"
            }
        }
    }

    /// The operator for operands of type `ty`, or `None` if it doesn't take
    /// them: `/` doesn't take integers, nor `div` and `mod` approximate
    /// numbers, and tuples and relations only have `=` and `<>`.
    pub(crate) fn resolve(self, ty: &hir::Type) -> Option<Resolved> {
        use hir::BinaryOp as B;
        let hir::Type::Scalar(scalar) = *ty else {
            // Tuples have only equality; relations are ordered by inclusion.
            return match (self, ty) {
                (
                    Operator::Compare(
                        comparison @ (hir::Comparison::Equal | hir::Comparison::NotEqual),
                    ),
                    _,
                )
                | (Operator::Compare(comparison), hir::Type::Relation(_)) => Some(
                    Resolved::Binary(B::Compare(comparison, hir::Compared::of(ty))),
                ),
                // A type of the program's own with an order.
                (Operator::Compare(comparison), hir::Type::Defined(defined))
                    if defined.order.is_some() =>
                {
                    Some(Resolved::Binary(B::Compare(
                        comparison,
                        hir::Compared::Defined,
                    )))
                }
                (Operator::Compare(comparison), hir::Type::Parameter(p))
                    if p.any == hir::Any::Relation =>
                {
                    Some(Resolved::Binary(B::Compare(
                        comparison,
                        hir::Compared::of(ty),
                    )))
                }
                // A kind that requires the operator.
                (_, hir::Type::Parameter(p))
                    if self.requirement().is_some_and(|r| p.any.requires(r)) =>
                {
                    let numeric = hir::Numeric::Parameter(p.index);
                    match self {
                        Operator::Compare(comparison) => Some(Resolved::Binary(B::Compare(
                            comparison,
                            hir::Compared::of(ty),
                        ))),
                        Operator::Plus => Some(Resolved::Identity),
                        Operator::Add => Some(Resolved::Binary(B::Add(numeric))),
                        Operator::Multiply => Some(Resolved::Binary(B::Multiply(numeric))),
                        _ => None,
                    }
                }
                _ => None,
            };
        };
        let numeric = hir::Numeric::of(scalar);
        let connective = |connective, not_left| Resolved::Connective {
            connective,
            not_left,
        };
        Some(match (self, numeric) {
            (Operator::Not, _) => Resolved::Unary(hir::UnaryOp::Not),
            (Operator::And, _) => connective(hir::Connective::And, false),
            (Operator::Or, _) => connective(hir::Connective::Or, false),
            (Operator::Xor, _) => connective(hir::Connective::Xor, false),
            (Operator::Implies, _) => connective(hir::Connective::Or, true),
            (Operator::Plus, Some(_)) => Resolved::Identity,
            (Operator::Negate, Some(n)) => Resolved::Unary(hir::UnaryOp::Negate(n)),
            (Operator::Add, Some(n)) => Resolved::Binary(B::Add(n)),
            (Operator::Subtract, Some(n)) => Resolved::Binary(B::Subtract(n)),
            (Operator::Multiply, Some(n)) => Resolved::Binary(B::Multiply(n)),
            (Operator::Divide, Some(hir::Numeric::Approximate)) => Resolved::Binary(B::Divide),
            (Operator::Div, Some(hir::Numeric::Integer)) => Resolved::Binary(B::Div),
            (Operator::Mod, Some(hir::Numeric::Integer)) => Resolved::Binary(B::Mod),
            (Operator::Concatenate, None) if scalar == hir::Scalar::Text => {
                Resolved::Binary(B::Concatenate)
            }
            (Operator::Compare(comparison), _) => {
                Resolved::Binary(B::Compare(comparison, hir::Compared::Scalar(scalar)))
            }
            _ => return None,
        })
    }
}

/// Checks one operand against an expectation, giving its checked form and
/// its type, or `None` after reporting why it has none.
pub(crate) type CheckOperand<'a> = dyn FnMut(&Expr, Option<&Expected>, &mut Vec<Diagnostic>) -> Option<(hir::Expr, hir::Type)>
    + 'a;

/// Check the operands of `op`, written `token`, in an expression whose
/// value is `expected` to have a type. `check` checks one operand against
/// an expectation, giving its checked form and its type.
///
/// Returns the resolved operator, the checked operands and the type of the
/// result, or `None` after reporting why there are none.
pub(crate) fn check_operands(
    op: Operator,
    token: &SyntaxToken,
    operands: Vec<Option<Expr>>,
    expected: Option<&Expected>,
    check: &mut CheckOperand<'_>,
    kinds: &[hir::Kind],
    errors: &mut Vec<Diagnostic>,
) -> Option<(Resolved, Vec<hir::Expr>, hir::Type)> {
    // A context the operator can work in sets the operand type.
    let context = expected.filter(|e| op.result_is_operand() && op.resolve(&e.ty).is_some());
    let mut expectation = match op.operands() {
        Operands::Fixed(scalar) => Some(Expected {
            ty: hir::Type::Scalar(scalar),
            origin: Origin::Operator(token.clone()),
        }),
        Operands::Inferred => context.map(|e| Expected {
            ty: e.ty.clone(),
            origin: e.origin.operand(token),
        }),
    };
    let mut ok = true;
    // Whether the operator was reported as not taking the operands' type.
    let mut wrong_type = false;
    // Without an expected type, the operands valid on their own set it for
    // the rest, as the most specific common supertype of theirs, whatever
    // their order: as of an empty relation, whose heading can't be known
    // on its own (see [`types::Alone`] and [`types::widen`]). Operands of
    // types with no value in common, as two subtypes of a union type, have
    // no value in common either, so one of them is reported, as never
    // equal to the other (IM Prescription 12).
    let mut alone: Vec<Option<types::Alone<(hir::Expr, hir::Type)>>> =
        operands.iter().map(|_| None).collect();
    if expectation.is_none() {
        let mut first = None;
        for (operand, alone) in operands.iter().zip(&mut alone) {
            let Some(operand) = operand else {
                break;
            };
            let mut own_errors = Vec::new();
            let checked = check(operand, None, &mut own_errors);
            let checked = types::Alone::of(checked, own_errors, operand.span());
            if let types::Alone::Valid((_, ty)) = &checked
                && expectation.as_ref().is_none_or(|e| e.ty.overlaps(ty))
            {
                types::widen(&mut expectation, ty.clone(), operand);
                first.get_or_insert(operand);
            }
            *alone = Some(checked);
        }
        // An operator that doesn't take this type is reported once; the
        // other operands are still checked against it, so that an empty
        // relation gets its heading and a mismatch is still reported.
        if let Some(expected) = &expectation
            && op.resolve(&expected.ty).is_none()
        {
            errors.push(errors::wrong_operand_type(
                op,
                token,
                &expected.ty,
                first,
                kinds,
            ));
            ok = false;
            wrong_type = true;
        }
    }
    let mut checked = Vec::new();
    for (operand, alone) in operands.into_iter().zip(alone) {
        let Some(operand) = operand else {
            ok = false;
            continue;
        };
        let before = errors.len();
        let mut against = |errors: &mut Vec<Diagnostic>| {
            check(&operand, expectation.as_ref(), errors).map(|(value, _)| value)
        };
        let result = match alone {
            // An operand that set a type the operator doesn't take.
            Some(types::Alone::Valid(_)) if wrong_type => continue,
            Some(alone) => alone.fit(expectation.as_ref(), operand.span(), errors, against),
            None => against(errors),
        };
        if wrong_type {
            // The operand was checked against a type the operator doesn't
            // take, which is already reported; only its own errors remain.
            let span = operand.span();
            let mut i = before;
            while i < errors.len() {
                if errors[i].code == trendy_diag::Code::TypeMismatch && errors[i].span() == span {
                    errors.remove(i);
                } else {
                    i += 1;
                }
            }
        }
        let Some(value) = result else {
            ok = false;
            continue;
        };
        checked.push(value);
    }
    let Expected { ty, .. } = expectation?;
    if !ok {
        return None;
    }
    let Some(resolved) = op.resolve(&ty) else {
        errors.push(errors::wrong_operand_type(op, token, &ty, None, kinds));
        return None;
    };
    Some((resolved, checked, op.result(ty)))
}

/// A projection, `r{a, b}`, or its complement, `r remove {c}`.
#[derive(Clone, Copy)]
pub(crate) enum Projection<'a> {
    Project(&'a ast::ProjectExpr),
    Remove(&'a ast::PostfixExpr),
}

impl Projection<'_> {
    fn names(self) -> Option<ast::NameList> {
        match self {
            Projection::Project(p) => p.names(),
            Projection::Remove(r) => r.names(),
        }
    }

    /// Where the operator is written: a projection's braces, or `remove`.
    fn at(self) -> Option<Span> {
        match self {
            Projection::Project(p) => Some(p.names()?.span()),
            Projection::Remove(r) => Some(trendy_syntax::span(r.operator()?.text_range())),
        }
    }
}

/// The attributes `projection` of `operand`, which is of type `ty`, removes:
/// those named by `remove`, or those not named by a projection. `None`
/// after reporting why there are none: the operand isn't a tuple or
/// relation, or a name isn't one of its attributes, or appears twice, or
/// `remove` names nothing.
pub(crate) fn removed(
    projection: Projection,
    operand: &Expr,
    ty: &hir::Type,
    errors: &mut Vec<Diagnostic>,
) -> Option<BTreeSet<String>> {
    let (hir::Type::Tuple(heading) | hir::Type::Relation(heading)) = ty else {
        let operator = matches!(projection, Projection::Remove(_)).then_some("remove");
        errors.push(errors::not_projectable(
            operator,
            projection.at()?,
            ty,
            operand,
        ));
        return None;
    };
    let context = errors::attributes_note("operand", ty);
    let listed = listed(
        &projection.names()?,
        heading,
        errors::Noun::Projected,
        &context,
        errors,
    )?;
    if let Projection::Remove(remove) = projection
        && listed.is_empty()
    {
        errors.push(errors::empty_remove(remove, operand));
        return None;
    }
    Some(match projection {
        Projection::Remove(_) => listed,
        Projection::Project(_) => heading
            .attributes()
            .map(|(a, _)| a.to_string())
            .filter(|a| !listed.contains(a))
            .collect(),
    })
}

/// The attributes `list` names, each of which must be one of `heading`'s,
/// named once. `None` after reporting each name that isn't, with
/// `context`, a note on what the attributes are, and a fix to the closest
/// attribute not already named.
pub(crate) fn listed(
    list: &ast::NameList,
    heading: &hir::Heading,
    noun: errors::Noun,
    context: &str,
    errors: &mut Vec<Diagnostic>,
) -> Option<BTreeSet<String>> {
    let named: Vec<(ast::Name, String, Span)> = list
        .names()
        .filter_map(|name| {
            let ident = name.ident()?;
            let span = trendy_syntax::span(ident.text_range());
            Some((name, ident.text().to_string(), span))
        })
        .collect();
    let mut ok = true;
    for (name, text, span) in &named {
        if let Some(&(_, _, first)) = named.iter().find(|(_, n, _)| n == text)
            && first != *span
        {
            errors.push(errors::duplicate_attribute(noun, text, *span, first));
            ok = false;
        } else if heading.get(text).is_none() {
            // The closest attribute not already named is probably the one
            // meant.
            let rename = closest(
                text,
                heading
                    .named()
                    .map(|(a, _)| a)
                    .filter(|a| !named.iter().any(|(_, n, _)| n == a)),
            );
            errors.push(errors::unknown_attribute(
                noun,
                text,
                *span,
                context,
                rename,
                Some(list.removal(name)),
            ));
            ok = false;
        }
    }
    ok.then(|| named.into_iter().map(|(_, n, _)| n).collect())
}

/// The renamings of `postfix`, a `rename` of `operand`, which is of type
/// `ty`: each attribute's name before, mapped to its name after, without
/// those that keep their name. `None` after reporting why there are none:
/// the operand isn't a tuple or relation, or an old name isn't one of its
/// attributes, or is renamed twice, or a new name is taken, or nothing is
/// renamed.
pub(crate) fn renamed(
    postfix: &ast::PostfixExpr,
    operand: &Expr,
    ty: &hir::Type,
    errors: &mut Vec<Diagnostic>,
) -> Option<BTreeMap<String, String>> {
    let token = postfix.operator()?;
    let (hir::Type::Tuple(heading) | hir::Type::Relation(heading)) = ty else {
        let at = trendy_syntax::span(token.text_range());
        errors.push(errors::not_projectable(Some("rename"), at, ty, operand));
        return None;
    };
    let list = postfix.renamings()?;
    // Each renaming: its node, and the names before and after, with spans.
    let name = |n: Option<ast::Name>| {
        let ident = n?.ident()?;
        Some((
            ident.text().to_string(),
            trendy_syntax::span(ident.text_range()),
        ))
    };
    let entries: Vec<(ast::Renaming, NameAt, NameAt)> = list
        .renamings()
        .filter_map(|r| Some((r.clone(), name(r.before())?, name(r.after())?)))
        .collect();
    if list.renamings().next().is_none() {
        errors.push(errors::empty_rename(postfix, operand));
        return None;
    }
    let context = errors::attributes_note("operand", ty);
    let mut ok = true;
    let mut renamings = BTreeMap::new();
    for (renaming, (before, before_span), (after, after_span)) in &entries {
        let first = entries
            .iter()
            .find(|(_, (b, _), _)| b == before)
            .map(|(_, (_, s), _)| *s);
        if let Some(first) = first.filter(|first| first != before_span) {
            errors.push(errors::duplicate_attribute(
                errors::Noun::Projected,
                before,
                *before_span,
                first,
            ));
            ok = false;
        } else if heading.get(before).is_none() {
            if heading.get(after).is_some() {
                // `code as postal_code` for `postal_code as code`: written the other
                // way around.
                errors.push(errors::reversed_renaming(
                    (before, *before_span),
                    (after, *after_span),
                    &context,
                ));
            } else {
                let rename = closest(
                    before,
                    heading
                        .named()
                        .map(|(a, _)| a)
                        .filter(|a| !entries.iter().any(|(_, (b, _), _)| b == a)),
                );
                errors.push(errors::unknown_attribute(
                    errors::Noun::Projected,
                    before,
                    *before_span,
                    &context,
                    rename,
                    Some(list.removal(renaming)),
                ));
            }
            ok = false;
        } else if before != after {
            renamings.insert(before.clone(), after.clone());
        }
    }
    if !ok {
        return None;
    }
    // Every new name must be free: no attribute that keeps its name has it,
    // and no earlier renaming gives it.
    for (i, (_, (before, _), (after, after_span))) in entries.iter().enumerate() {
        if before == after {
            continue;
        }
        let kept = heading.get(after).is_some() && !renamings.contains_key(after);
        let earlier = entries[..i]
            .iter()
            .find(|(_, (b, _), (a, _))| a == after && b != a)
            .map(|(_, _, (_, s))| *s);
        if kept || earlier.is_some() {
            errors.push(errors::attribute_exists(
                after,
                *after_span,
                earlier,
                &context,
            ));
            ok = false;
        }
    }
    ok.then_some(renamings)
}

/// A name as written, and where.
type NameAt = (String, Span);

/// A join: each is Algebra A's AND of two relations, and they differ in
/// what they require of the operands' common attributes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum JoinKind {
    /// `join`, of operands with attributes in common.
    Join,
    /// `times`, of operands with none in common.
    Times,
    /// `compose`, of operands with attributes in common, which the result
    /// doesn't have.
    Compose,
    /// `matching`, the semijoin: the tuples of the left operand that have a
    /// match in the right, on attributes in common.
    Matching,
    /// `not matching`, the antijoin: those that have none.
    NotMatching,
    /// `union`, of relations of the same attributes: the tuples of either.
    Union,
    /// `d_union`: a union whose operands must have no tuple in common.
    DisjointUnion,
    /// `xunion`: the tuples of exactly one operand, as `xor` is of
    /// booleans.
    ExclusiveUnion,
    /// `intersect`, of relations of the same attributes: the tuples of
    /// both, which is their AND.
    Intersect,
    /// `minus`, of relations of the same attributes: the tuples of the left
    /// that aren't in the right, from the left in a list.
    Minus,
    /// `i_minus`: a minus whose right operand's tuples must all be in the
    /// left.
    IncludedMinus,
}

impl JoinKind {
    /// The join written with the operator `tokens`, if they are one.
    fn of(tokens: &[SyntaxToken]) -> Option<JoinKind> {
        let kinds: Vec<SyntaxKind> = tokens.iter().map(SyntaxToken::kind).collect();
        Some(match kinds[..] {
            [SyntaxKind::JoinKw] => JoinKind::Join,
            [SyntaxKind::TimesKw] => JoinKind::Times,
            [SyntaxKind::ComposeKw] => JoinKind::Compose,
            [SyntaxKind::MatchingKw] => JoinKind::Matching,
            [SyntaxKind::NotKw, SyntaxKind::MatchingKw] => JoinKind::NotMatching,
            [SyntaxKind::UnionKw] => JoinKind::Union,
            [SyntaxKind::DUnionKw] => JoinKind::DisjointUnion,
            [SyntaxKind::XUnionKw] => JoinKind::ExclusiveUnion,
            [SyntaxKind::IntersectKw] => JoinKind::Intersect,
            [SyntaxKind::MinusKw] => JoinKind::Minus,
            [SyntaxKind::IMinusKw] => JoinKind::IncludedMinus,
            _ => return None,
        })
    }

    /// Whether it takes relations only, not tuples: all but `join`, `times`
    /// and `compose`.
    fn relations_only(self) -> bool {
        !matches!(self, JoinKind::Join | JoinKind::Times | JoinKind::Compose)
    }

    /// Whether it is a union, whose result has the tuples of every operand.
    pub(crate) fn is_union(self) -> bool {
        matches!(
            self,
            JoinKind::Union | JoinKind::DisjointUnion | JoinKind::ExclusiveUnion
        )
    }

    /// Whether, of a set operator, operands' attributes of types `a` and `b`
    /// fit together (*Type Inheritance and Relational Theory*, IM
    /// Prescription 13): of a union, whose result has the values of both,
    /// if they have a common supertype, which the result's attribute is
    /// of, and otherwise, since the operands' tuples are compared, if some
    /// value is of both (IM Prescription 12).
    pub(crate) fn fits(self, a: &hir::Type, b: &hir::Type) -> bool {
        if self.is_union() {
            a.common_supertype(b).is_some()
        } else {
            a.overlaps(b)
        }
    }

    /// Whether, of a set operator, operands of headings `a` and `b` fit
    /// together: they have the same attributes, and each one's types fit.
    fn headings_fit(self, a: &hir::Heading, b: &hir::Heading) -> bool {
        if self.is_union() {
            a.common_supertype(b).is_some()
        } else {
            a.common_subtype(b).is_some()
        }
    }

    /// Whether it is a set operator, of relations of the same attributes.
    pub(crate) fn is_set(self) -> bool {
        matches!(
            self,
            JoinKind::Union
                | JoinKind::DisjointUnion
                | JoinKind::ExclusiveUnion
                | JoinKind::Intersect
                | JoinKind::Minus
                | JoinKind::IncludedMinus
        )
    }

    /// Whether its list is in brackets, a left fold whose order matters.
    pub(crate) fn is_ordered(self) -> bool {
        matches!(
            self,
            JoinKind::Compose | JoinKind::Minus | JoinKind::IncludedMinus
        )
    }

    /// Whether a list of none has a value: the identity of `join` and
    /// `times`, `relation {} { {} }`.
    fn has_identity(self) -> bool {
        matches!(self, JoinKind::Join | JoinKind::Times)
    }

    /// How the operator is written.
    pub(crate) fn keyword(self) -> &'static str {
        match self {
            JoinKind::Join => "join",
            JoinKind::Times => "times",
            JoinKind::Compose => "compose",
            JoinKind::Matching => "matching",
            JoinKind::NotMatching => "not matching",
            JoinKind::Union => "union",
            JoinKind::DisjointUnion => "d_union",
            JoinKind::ExclusiveUnion => "xunion",
            JoinKind::Intersect => "intersect",
            JoinKind::Minus => "minus",
            JoinKind::IncludedMinus => "i_minus",
        }
    }
}

/// A connective as written: dyadic, `a and b`, or of a list, `and{a, b, c}`.
pub(crate) struct Connecting {
    pub(crate) token: SyntaxToken,
    /// The operands, each of which may be missing from a dyadic one.
    pub(crate) operands: Vec<Option<Expr>>,
}

impl Connecting {
    /// The connective `expr` is, if it is one.
    pub(crate) fn of(expr: &Expr) -> Option<Connecting> {
        let (token, operands) = match expr {
            Expr::Binary(binary) => (binary.operator()?, vec![binary.lhs(), binary.rhs()]),
            Expr::Nary(nary) => (nary.operator()?, nary.operands().map(Some).collect()),
            _ => return None,
        };
        Operator::of(&token)?
            .is_connective()
            .then_some(Connecting { token, operands })
    }
}

/// A join as written: dyadic, `r join s`, or of a list, `join{r, s, t}` or
/// `compose[r, s, t]`.
pub(crate) struct Joining {
    pub(crate) kind: JoinKind,
    /// Where the operator is written: one keyword, or two, as in
    /// `not matching`.
    span: Span,
    pub(crate) operands: Vec<Expr>,
    /// The list form, if it's written that way.
    nary: Option<ast::NaryExpr>,
}

/// How a checked join combines its operands.
pub(crate) enum Combined {
    /// The AND of all of them, in any order: `join` and `times`.
    And,
    /// The tuples of the left operand that have a match in the right, or,
    /// if `negated`, those that have none: `matching` and `not matching`.
    Matching { negated: bool },
    /// From the left: the first operand combined with each other in turn,
    /// as `union`, `d_union`, `xunion`, `minus` and `i_minus` do.
    Combine(hir::Combination),
    /// Compose, from the left: the first operand, then for each other the
    /// AND of the result so far and it, without the attributes they have in
    /// common, given here for each step.
    Composed(Vec<BTreeSet<String>>),
}

impl Joining {
    /// The join `expr` is, if it is one.
    pub(crate) fn of(expr: &Expr) -> Option<Joining> {
        let (tokens, operands, nary) = match expr {
            Expr::Binary(binary) => (
                binary.operators().collect::<Vec<_>>(),
                vec![binary.lhs()?, binary.rhs()?],
                None,
            ),
            Expr::Nary(nary) => (
                vec![nary.operator()?],
                nary.operands().collect(),
                Some(nary.clone()),
            ),
            _ => return None,
        };
        let span = tokens
            .iter()
            .map(|t| trendy_syntax::span(t.text_range()))
            .reduce(|a, b| Span::new(a.start, b.end))?;
        Some(Joining {
            kind: JoinKind::of(&tokens)?,
            span,
            operands,
            nary,
        })
    }

    /// The operator, as written canonically, and where it is written.
    fn operator(&self) -> (&'static str, Span) {
        (self.kind.keyword(), self.span)
    }

    /// How the join combines its operands, each with its type in `types`.
    /// `None` after reporting why there is no result: an operand that isn't
    /// a relation or a tuple, or a tuple among relations, or an attribute of
    /// different types in two operands, or,
    /// for `times`, a common attribute; for `join`, operands that don't all
    /// connect through common attributes; and for `compose`, a step whose
    /// operands have nothing in common, or no operands at all.
    pub(crate) fn combined(
        &self,
        types: &[hir::Type],
        errors: &mut Vec<Diagnostic>,
    ) -> Option<Combined> {
        if !self.kind.has_identity()
            && self.operands.is_empty()
            && let Some(nary) = &self.nary
        {
            errors.push(errors::empty_operands(self.kind, nary));
            return None;
        }
        let operands = self.headings(types, errors)?;
        if self.kind == JoinKind::Compose {
            return self.composed(&operands, errors).map(Combined::Composed);
        }
        if self.kind.is_set() {
            return self.same_headings(&operands, errors);
        }
        let mut ok = true;
        // Each attribute name, with the operands before that have it. A
        // common attribute's value in a tuple of the result is of each of
        // its types, so each two must overlap (IM Prescription 13).
        let mut earlier: BTreeMap<&str, Vec<(Span, &hir::Type)>> = BTreeMap::new();
        let mut shared = BTreeSet::new();
        for &(span, heading) in &operands {
            for (name, ty) in heading.attributes() {
                let before = earlier.entry(name).or_default();
                if !before.is_empty() {
                    shared.insert(name.to_string());
                }
                if let Some(&(at, before_ty)) = before.iter().find(|(_, b)| !b.overlaps(ty)) {
                    errors.push(errors::join_type_conflict(
                        self.operator(),
                        name,
                        (at, before_ty),
                        (span, ty),
                    ));
                    ok = false;
                }
                before.push((span, ty));
            }
        }
        let headings: Vec<&hir::Heading> = operands.iter().map(|&(_, h)| h).collect();
        let groups = groups(&headings);
        let wrong = match self.kind {
            JoinKind::Times => !shared.is_empty(),
            // With a list, `join{}` and `join{r}` have nothing to connect.
            JoinKind::Join | JoinKind::Compose | JoinKind::Matching | JoinKind::NotMatching => {
                groups.iter().any(|&g| g != 0)
            }
            JoinKind::Union
            | JoinKind::DisjointUnion
            | JoinKind::ExclusiveUnion
            | JoinKind::Intersect
            | JoinKind::Minus
            | JoinKind::IncludedMinus => {
                unreachable!("the set operators' headings are checked above")
            }
        };
        if wrong {
            let operands: Vec<errors::Operand> = operands
                .iter()
                .map(|&(span, heading)| errors::Operand {
                    span,
                    heading,
                    composed: false,
                })
                .collect();
            errors.push(errors::common_attributes(
                self.kind,
                self.operator(),
                &operands,
                &shared,
                &groups,
            ));
            ok = false;
        }
        ok.then_some(match self.kind {
            JoinKind::Matching => Combined::Matching { negated: false },
            JoinKind::NotMatching => Combined::Matching { negated: true },
            JoinKind::Join | JoinKind::Times | JoinKind::Compose => Combined::And,
            JoinKind::Union
            | JoinKind::DisjointUnion
            | JoinKind::ExclusiveUnion
            | JoinKind::Intersect
            | JoinKind::Minus
            | JoinKind::IncludedMinus => {
                unreachable!("the set operators' headings are checked above")
            }
        })
    }

    /// The heading of each operand, of type in `types`, with where it is
    /// written, if each is one the operator takes: relations, or, for the
    /// joins, tuples, all of one kind. `None` after reporting each that
    /// isn't.
    fn headings<'t>(
        &self,
        types: &'t [hir::Type],
        errors: &mut Vec<Diagnostic>,
    ) -> Option<Vec<(Span, &'t hir::Heading)>> {
        // Tutorial D's tuple union is a join.
        if self.kind == JoinKind::Union
            && !types.is_empty()
            && types.iter().all(|ty| matches!(ty, hir::Type::Tuple(_)))
        {
            errors.push(errors::tuple_union(self.span));
            return None;
        }
        // Relations join with relations, and tuples with tuples; with
        // both, the tuples are the odd ones out.
        let relations = types.iter().any(|ty| matches!(ty, hir::Type::Relation(_)));
        let mut operands = Vec::new();
        let mut reported_tuple = false;
        for (operand, ty) in self.operands.iter().zip(types) {
            match ty {
                hir::Type::Relation(heading) => operands.push((operand.span(), heading)),
                hir::Type::Tuple(heading) if !relations && !self.kind.relations_only() => {
                    operands.push((operand.span(), heading));
                }
                // One tuple is reported, for operators of relations only.
                hir::Type::Tuple(_) if self.kind.relations_only() => {
                    if !reported_tuple {
                        errors.push(errors::not_joinable(
                            self.kind,
                            self.operator(),
                            ty,
                            operand,
                        ));
                        reported_tuple = true;
                    }
                }
                hir::Type::Tuple(_) => {
                    errors.push(errors::tuple_among_relations(self.kind.keyword(), operand));
                }
                hir::Type::Scalar(_)
                | hir::Type::Defined(_)
                | hir::Type::Dummy(_)
                | hir::Type::Array(_)
                | hir::Type::Parameter(_) => {
                    errors.push(errors::not_joinable(
                        self.kind,
                        self.operator(),
                        ty,
                        operand,
                    ));
                }
            }
        }
        (operands.len() == self.operands.len()).then_some(operands)
    }

    /// How a set operator of `operands`, each with its heading, combines
    /// them, if their headings fit together (see [`JoinKind::fits`]):
    /// each with the first, which the others are combined with, or, of an
    /// intersection, whose tuples are of every operand, each two. Otherwise
    /// reported.
    fn same_headings(
        &self,
        operands: &[(Span, &hir::Heading)],
        errors: &mut Vec<Diagnostic>,
    ) -> Option<Combined> {
        let mut ok = true;
        for (i, (&(_, heading), expr)) in operands.iter().zip(&self.operands).enumerate().skip(1) {
            let before = if self.kind == JoinKind::Intersect {
                i
            } else {
                1
            };
            let unfit = (0..before).find(|&j| !self.kind.headings_fit(operands[j].1, heading));
            if let Some(j) = unfit {
                errors.push(errors::heading_mismatch(
                    self.kind,
                    self.span,
                    (&self.operands[j], operands[j].1),
                    (expr, heading),
                ));
                ok = false;
            }
        }
        ok.then_some(match self.kind {
            JoinKind::Union => Combined::Combine(hir::Combination::Union),
            JoinKind::DisjointUnion => Combined::Combine(hir::Combination::DisjointUnion),
            JoinKind::ExclusiveUnion => Combined::Combine(hir::Combination::ExclusiveUnion),
            JoinKind::Minus => Combined::Combine(hir::Combination::Minus),
            JoinKind::IncludedMinus => Combined::Combine(hir::Combination::IncludedMinus),
            _ => Combined::And,
        })
    }

    /// The attributes each step of a compose of `operands`, each where it's
    /// written and with its heading, removes: those the result so far and
    /// the next operand have in common, which must be some, and of the same
    /// types.
    fn composed(
        &self,
        operands: &[(Span, &hir::Heading)],
        errors: &mut Vec<Diagnostic>,
    ) -> Option<Vec<BTreeSet<String>>> {
        let (&(first_span, first), rest) = operands.split_first()?;
        let (mut so_far, mut so_far_span) = (first.clone(), first_span);
        let mut steps = Vec::new();
        let mut ok = true;
        for &(span, heading) in rest {
            let common = so_far.common(heading);
            for name in &common {
                let (a, b) = (so_far.get(name)?, heading.get(name)?);
                if !a.overlaps(b) {
                    errors.push(errors::join_type_conflict(
                        self.operator(),
                        name,
                        (so_far_span, a),
                        (span, b),
                    ));
                    ok = false;
                }
            }
            if common.is_empty() {
                let operands = [
                    errors::Operand {
                        span: so_far_span,
                        heading: &so_far,
                        composed: so_far_span != first_span,
                    },
                    errors::Operand {
                        span,
                        heading,
                        composed: false,
                    },
                ];
                errors.push(errors::common_attributes(
                    self.kind,
                    self.operator(),
                    &operands,
                    &common,
                    &[0, 1],
                ));
                ok = false;
            }
            so_far = so_far.without(&common).and(&heading.without(&common));
            so_far_span = Span::new(first_span.start, span.end);
            steps.push(common);
        }
        ok.then_some(steps)
    }
}

/// For each heading, which group it is in: those connected to each other
/// through common attributes, numbered from 0 in order of their first
/// heading.
fn groups(headings: &[&hir::Heading]) -> Vec<usize> {
    let mut group: Vec<Option<usize>> = vec![None; headings.len()];
    let mut count = 0;
    for start in 0..headings.len() {
        if group[start].is_some() {
            continue;
        }
        group[start] = Some(count);
        let mut pending = vec![start];
        while let Some(i) = pending.pop() {
            for j in 0..headings.len() {
                if group[j].is_none() && !headings[i].common(headings[j]).is_empty() {
                    group[j] = Some(count);
                    pending.push(j);
                }
            }
        }
        count += 1;
    }
    group.into_iter().map(|g| g.unwrap_or(0)).collect()
}

/// The AND of the relations, or the join of the tuples, `operands`, with
/// the constant ones evaluated together first: AND is associative and
/// commutative, so the order doesn't change the result. The AND of no
/// relations is `relation {} {{}}`, which is AND's identity.
///
/// # Errors
///
/// Returns why constant tuples have no join: an attribute they disagree on.
pub(crate) fn and(operands: Vec<hir::Expr>) -> Result<hir::Expr, Failed> {
    let (constants, others): (Vec<_>, Vec<_>) =
        operands.into_iter().partition(|e| known_value(e).is_some());
    // Of tuples, the type is of all their attributes, as declared, which
    // can be supertypes of the values'.
    let ty = constants
        .iter()
        .map(known_type)
        .reduce(|lhs, rhs| match (lhs, rhs) {
            (hir::Type::Tuple(a), hir::Type::Tuple(b)) => hir::Type::Tuple(a.and(&b)),
            (lhs, _) => lhs,
        });
    let mut constants = constants.iter().filter_map(|e| known_value(e).cloned());
    let constant = match constants.next() {
        Some(first) => Some(
            constants
                .try_fold(first, |lhs, rhs| lhs.and(&rhs))
                .map_err(Failed::Mismatch)?,
        ),
        None => None,
    };
    let mut others = others.into_iter();
    let first = match (constant, ty) {
        (Some(hir::Value::Tuple(tuple)), Some(ty)) => typed(hir::Value::Tuple(tuple), &ty),
        (Some(value), _) => hir::Expr::Constant(value),
        (None, _) => others
            .next()
            .unwrap_or_else(|| hir::Expr::Constant(hir::Value::dee())),
    };
    Ok(others.fold(first, |lhs, rhs| {
        hir::Expr::And(Box::new(lhs), Box::new(rhs))
    }))
}

/// Why an expression of constants has no value.
#[derive(Debug, Clone)]
pub(crate) enum Failed {
    /// An operator on scalars fails.
    Scalar(hir::Failure),
    /// Tuples being joined disagree on a common attribute.
    Mismatch(hir::Mismatch),
    /// A relation whose only tuple is taken has this many, not one.
    NotOne(usize),
    /// A tuple breaks a disjoint union's or an included minus's condition.
    Broken(Box<hir::Broken>),
    /// This built-in, `min` or `max`, is of a relation without tuples.
    NoTuples(hir::ReadOnlyBuiltin),
    /// Two changes to an array overlap: they set one element, or one sets
    /// an element of the other's.
    Overlap,
    /// This built-in, `parse_integer`, `parse_approximate` or
    /// `parse_boolean`, of `text`, which isn't a value of its type, as
    /// `why` says.
    Unparsable {
        builtin: hir::ReadOnlyBuiltin,
        text: String,
        why: hir::Unparsable,
    },
    /// Components that break a constraint of the type of the program's own
    /// a value is selected of.
    Constraint(Box<BrokenConstraint>),
    /// A relation-valued component with two tuples that share the values
    /// of one of its keys, of a value selected of a type of the program's
    /// own.
    Key(Box<BrokenKey>),
    /// A value selected of a type of the program's own that is of two of
    /// its subtypes defined by constraints.
    Overlapping(Box<Overlapping>),
    /// A value treated as one of a type it isn't of.
    NotOfType(Box<(hir::Value, hir::Type)>),
    /// A `fail`, of the error that writes this message.
    Own(String),
}

/// The tuples of `operand` for which `condition`, computed from the tuple at
/// hand, is true: evaluated if the operand is a constant and the condition
/// then is for each of its tuples. A condition that is true whatever the
/// tuple keeps them all.
///
/// # Errors
///
/// Returns why evaluating the condition for a tuple of a constant operand
/// fails.
pub(crate) fn restrict(
    operand: hir::Expr,
    condition: hir::Expr,
    bindings: Bindings,
) -> Result<hir::Expr, TupleFailure> {
    if condition == hir::Expr::Constant(hir::Value::Boolean(true)) {
        return Ok(operand);
    }
    let hir::Expr::Constant(hir::Value::Relation(relation)) = &operand else {
        return Ok(hir::Expr::Restrict(Box::new(operand), Box::new(condition)));
    };
    let mut kept = BTreeSet::new();
    for tuple in &relation.tuples {
        match substitute(condition.clone(), 0, &Substitution::tuple(tuple, bindings)) {
            Ok(hir::Expr::Constant(hir::Value::Boolean(true))) => {
                kept.insert(tuple.clone());
            }
            Ok(hir::Expr::Constant(hir::Value::Boolean(false))) => {}
            Ok(_) => return Ok(hir::Expr::Restrict(Box::new(operand), Box::new(condition))),
            Err(failed) => {
                return Err(TupleFailure {
                    failed,
                    attribute: None,
                    tuple: tuple.clone(),
                });
            }
        }
    }
    Ok(hir::Expr::Constant(hir::Value::Relation(
        hir::RelationValue {
            heading: relation.heading.clone(),
            tuples: kept,
        },
    )))
}

/// The tuples of the AND of `operands` for which `condition`, computed
/// from the tuple at hand, is true, evaluated without making the whole
/// AND, as for every tuple of the whole AND, so that it fails if the
/// condition does for one.
///
/// The conjuncts of the condition that can't fail are computed first (see
/// `hir::Connective::And`), so they restrict as Algebra A defines
/// restriction (TTM, Appendix A): the AND with the relation of the tuples
/// they are true of, which here is finite, as each gives the relation of
/// the values it is true of, among those the operands have for the
/// attributes it reads. The others are computed for the tuples of the AND
/// of the operands and those relations, which are the tuples of the whole
/// AND that every one that can't fail is true of.
///
/// `None` unless every operand is a constant relation, and every conjunct
/// reads the tuple at hand and is known when compiling; otherwise only the
/// whole AND, restricted, says what happens.
pub(crate) fn restricted_and(
    operands: &[hir::Expr],
    condition: &hir::Expr,
    bindings: Bindings,
) -> Option<Result<hir::Expr, TupleFailure>> {
    let relations: Vec<&hir::RelationValue> = operands
        .iter()
        .map(|operand| match operand {
            hir::Expr::Constant(hir::Value::Relation(relation)) => Some(relation),
            _ => None,
        })
        .collect::<Option<_>>()?;
    let names = |relation: &hir::RelationValue| -> BTreeSet<String> {
        relation
            .heading
            .attributes()
            .map(|(name, _)| name.to_string())
            .collect()
    };
    let own: Vec<BTreeSet<String>> = relations.iter().map(|r| names(r)).collect();
    let all: BTreeSet<String> = own.iter().flatten().cloned().collect();
    let (failing, guards): (Vec<&hir::Expr>, Vec<&hir::Expr>) = condition
        .conjuncts()
        .iter()
        .partition(|conjunct| conjunct.can_fail());
    let mut parts: Vec<hir::Value> = Vec::new();
    for guard in guards {
        let read = guard.tuple_read(0);
        let reads = if read.whole {
            all.clone()
        } else {
            read.attributes
        };
        if reads.is_empty() {
            return None;
        }
        let reading: Vec<usize> = (0..relations.len())
            .filter(|&i| !own[i].is_disjoint(&reads))
            .collect();
        // Of each operand that has some of what it reads, that, and what
        // joins the operand to another of them.
        let joins = |name: &String| reading.iter().filter(|&&j| own[j].contains(name)).count() > 1;
        let values: Vec<hir::Value> = reading
            .iter()
            .map(|&i| {
                let dropped = own[i]
                    .iter()
                    .filter(|name| !reads.contains(*name) && !joins(name))
                    .cloned()
                    .collect();
                hir::Value::Relation(relations[i].clone()).without(&dropped)
            })
            .collect();
        // The combinations of the values it reads.
        let values = and_values(values)?;
        let joining = names(relation_of(&values))
            .difference(&reads)
            .cloned()
            .collect();
        let values = values.without(&joining);
        match restrict(hir::Expr::Constant(values), guard.clone(), bindings) {
            Ok(hir::Expr::Constant(allowed)) => parts.push(allowed),
            _ => return None,
        }
    }
    parts.extend(
        relations
            .iter()
            .map(|relation| hir::Value::Relation((*relation).clone())),
    );
    let guarded = hir::Expr::Constant(and_values(parts)?);
    if failing.is_empty() {
        return Some(Ok(guarded));
    }
    let failing = hir::Connective::And.of(failing.into_iter().cloned().collect());
    match restrict(guarded, failing, bindings) {
        Ok(hir::Expr::Constant(restricted)) => Some(Ok(hir::Expr::Constant(restricted))),
        Ok(_) => None,
        Err(failure) => Some(Err(failure)),
    }
}

/// The relation `value` is.
fn relation_of(value: &hir::Value) -> &hir::RelationValue {
    match value {
        hir::Value::Relation(relation) => relation,
        _ => unreachable!("only relations are ANDed here"),
    }
}

/// The AND of the constant relations `parts`, made in an order that keeps
/// each step small: the smallest first, then each next the one with the most
/// attributes in common with the result so far, the smallest among those,
/// so that it restricts the result as much as it can. `None` of no parts,
/// whose AND has no heading to have.
fn and_values(mut parts: Vec<hir::Value>) -> Option<hir::Value> {
    let size = |part: &hir::Value| relation_of(part).tuples.len();
    let first = (0..parts.len()).min_by_key(|&i| size(&parts[i]))?;
    let mut result = parts.swap_remove(first);
    while !parts.is_empty() {
        let so_far = relation_of(&result).heading.clone();
        let next = (0..parts.len())
            .max_by_key(|&i| {
                let common = so_far.common(&relation_of(&parts[i]).heading).len();
                (common, std::cmp::Reverse(size(&parts[i])))
            })
            .expect("a part is left");
        result = result
            .and(&parts.swap_remove(next))
            .expect("relations always have an AND");
    }
    Some(result)
}

/// The relations `lhs` and `rhs`, of the same attributes, combined as
/// `combination` says: evaluated if both are constants.
///
/// # Errors
///
/// Returns a tuple of constants that breaks a disjoint union's or an
/// included minus's condition.
pub(crate) fn combined(
    lhs: hir::Expr,
    rhs: hir::Expr,
    combination: hir::Combination,
) -> Result<hir::Expr, Failed> {
    match (lhs, rhs) {
        (hir::Expr::Constant(lhs), hir::Expr::Constant(rhs)) => lhs
            .combined(&rhs, combination)
            .map(hir::Expr::Constant)
            .map_err(Failed::Broken),
        (lhs, rhs) => Ok(hir::Expr::Combine(
            combination,
            Box::new(lhs),
            Box::new(rhs),
        )),
    }
}

/// The tuples of the relation `lhs` that have a match in `rhs`, or, if
/// `negated`, those that have none: evaluated if both are constants.
pub(crate) fn matching(lhs: hir::Expr, rhs: hir::Expr, negated: bool) -> hir::Expr {
    match (lhs, rhs) {
        (hir::Expr::Constant(lhs), hir::Expr::Constant(rhs)) => {
            hir::Expr::Constant(lhs.matching(&rhs, negated))
        }
        (lhs, rhs) if negated => hir::Expr::Antijoin(Box::new(lhs), Box::new(rhs)),
        (lhs, rhs) => hir::Expr::Semijoin(Box::new(lhs), Box::new(rhs)),
    }
}

/// The only tuple of the relation `operand`: evaluated if it is a constant.
///
/// # Errors
///
/// Returns how many tuples a constant relation has, if not one.
pub(crate) fn only(operand: hir::Expr) -> Result<hir::Expr, Failed> {
    match operand {
        hir::Expr::Constant(hir::Value::Relation(relation)) => {
            let ty = hir::Type::Tuple(relation.heading);
            let mut tuples = relation.tuples.into_iter();
            match (tuples.next(), tuples.len()) {
                (Some(tuple), 0) => Ok(typed(hir::Value::Tuple(tuple), &ty)),
                (None, _) => Err(Failed::NotOne(0)),
                (Some(_), more) => Err(Failed::NotOne(more + 1)),
            }
        }
        operand => Ok(hir::Expr::Only(Box::new(operand))),
    }
}

/// A tuple of these attributes, a constant if they all are, whose
/// attributes are of their types, which can be supertypes of their values'.
pub(crate) fn tuple(attributes: BTreeMap<String, hir::Expr>) -> hir::Expr {
    let constants: Option<BTreeMap<String, hir::Value>> = attributes
        .iter()
        .map(|(name, e)| Some((name.clone(), known_value(e)?.clone())))
        .collect();
    match constants {
        Some(constants) => {
            let heading = attributes
                .iter()
                .map(|(name, e)| (name.clone(), known_type(e)))
                .collect();
            let ty = hir::Type::Tuple(hir::Heading::new(heading));
            typed(hir::Value::Tuple(hir::TupleValue::new(constants)), &ty)
        }
        None => hir::Expr::Tuple(attributes),
    }
}

/// A relation with this heading and these tuples, a constant if they all
/// are; tuples that are equal become one.
pub(crate) fn relation(heading: hir::Heading, tuples: Vec<hir::Expr>) -> hir::Expr {
    let constants: Option<BTreeSet<hir::TupleValue>> = tuples
        .iter()
        .map(|t| match known_value(t) {
            Some(hir::Value::Tuple(t)) => Some(t.clone()),
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

/// An array with elements of type `element`, in order, a constant if they
/// all are.
pub(crate) fn array(element: hir::Type, elements: Vec<hir::Expr>) -> hir::Expr {
    let constants: Option<Vec<hir::Value>> =
        elements.iter().map(|e| known_value(e).cloned()).collect();
    match constants {
        Some(elements) => {
            hir::Expr::Constant(hir::Value::Array(hir::ArrayValue { element, elements }))
        }
        None => hir::Expr::Array(element, elements),
    }
}

/// The array a `load` makes of `relation`, as `loading` says: evaluated
/// if the relation is a constant, and each value its order compares in
/// another possible representation than its type's stored one is known in
/// it.
///
/// # Errors
///
/// Returns why converting a value to the representation its type's order
/// compares fails.
pub(crate) fn load(
    relation: hir::Expr,
    loading: hir::Loading,
    bindings: Bindings,
) -> Result<hir::Expr, Failed> {
    if let hir::Expr::Constant(hir::Value::Relation(constant)) = &relation {
        let compared = constant.tuples.iter().flat_map(|tuple| {
            loading
                .order
                .iter()
                .filter_map(|item| tuple.get(&item.attribute))
        });
        if let Some(converted) = converted(compared, bindings)? {
            let loaded = constant.loaded(&loading, &converted);
            return Ok(hir::Expr::Constant(hir::Value::Array(loaded)));
        }
    }
    Ok(hir::Expr::Load(Box::new(relation), loading))
}

/// The values of types of the program's own among `values`, and among the
/// components their orders compare, whose order compares the components
/// of another possible representation than the stored one, each with those
/// components, which [`hir::compare`] compares it by: `None` if a
/// conversion isn't known when compiling.
///
/// # Errors
///
/// Returns why a conversion fails.
fn converted<'v>(
    values: impl IntoIterator<Item = &'v hir::Value>,
    bindings: Bindings,
) -> Result<Option<hir::Converted>, Failed> {
    let mut converted = hir::Converted::default();
    let mut pending: Vec<hir::Value> = values.into_iter().cloned().collect();
    while let Some(value) = pending.pop() {
        let hir::Value::Defined(value) = value else {
            continue;
        };
        let Some(order) = &value.ty.order else {
            continue;
        };
        let compared = match order.from {
            None => value.components.clone(),
            Some(_) if converted.contains(&value) => continue,
            Some(from) => {
                let args = value
                    .components
                    .attributes()
                    .map(|(c, v)| (c.to_string(), hir::Expr::Constant(v.clone())))
                    .collect();
                let conversion = hir::Call {
                    operator: from,
                    types: Vec::new(),
                    args,
                    updated: BTreeMap::new(),
                };
                let hir::Expr::Constant(hir::Value::Tuple(components)) =
                    call(conversion, bindings)?
                else {
                    return Ok(None);
                };
                converted.insert(value.clone(), components.clone());
                components
            }
        };
        pending.extend(
            order
                .items
                .iter()
                .filter_map(|item| compared.get(&item.attribute).cloned()),
        );
    }
    Ok(Some(converted))
}

/// The relation a `load` makes of `array`, as `unloading` says: evaluated
/// if the array is a constant.
pub(crate) fn unload(array: hir::Expr, unloading: hir::Unloading) -> hir::Expr {
    match array {
        hir::Expr::Constant(hir::Value::Array(array)) => {
            hir::Expr::Constant(hir::Value::Relation(array.unloaded(&unloading)))
        }
        array => hir::Expr::Unload(Box::new(array), unloading),
    }
}

/// The element of `array` at `position`, counting from 1: evaluated if
/// both are constants, and taken straight from a selector whose other
/// elements can't fail.
///
/// # Errors
///
/// Returns why there is no element there: the position is out of range,
/// known now if it is a constant and the array's length is, or the
/// position is below 1, whatever the array.
pub(crate) fn index(array: hir::Expr, position: hir::Expr) -> Result<hir::Expr, hir::Failure> {
    let hir::Expr::Constant(hir::Value::Integer(index)) = position else {
        return Ok(hir::Expr::Index(Box::new(array), Box::new(position)));
    };
    let length = match &array {
        hir::Expr::Constant(hir::Value::Array(array)) => Some(array.elements.len()),
        hir::Expr::Array(_, elements) => Some(elements.len()),
        _ => None,
    };
    let at = usize::try_from(index)
        .ok()
        .and_then(|i| i.checked_sub(1))
        .filter(|at| length.is_none_or(|length| *at < length));
    let Some(at) = at else {
        return Err(hir::Failure::OutOfRange { index, length });
    };
    match array {
        hir::Expr::Constant(hir::Value::Array(mut array)) => {
            Ok(typed(array.elements.swap_remove(at), &array.element))
        }
        hir::Expr::Array(_, mut elements)
            if elements
                .iter()
                .enumerate()
                .all(|(i, e)| i == at || !e.can_fail()) =>
        {
            Ok(elements.swap_remove(at))
        }
        array => Ok(hir::Expr::Index(Box::new(array), Box::new(position))),
    }
}

/// `array` with `changes` made, as [`hir::Expr::WithElements`] makes
/// them: evaluated if the array, each position and each value are
/// constants.
///
/// # Errors
///
/// Returns why it has no value: two changes overlap, known now if their
/// positions are, or a position is out of range, known now if it is a
/// constant and the array's length is, or it is below 1, whatever the
/// array.
pub(crate) fn with_elements(
    array: hir::Expr,
    changes: Vec<hir::ElementChange>,
) -> Result<hir::Expr, Failed> {
    for (i, change) in changes.iter().enumerate() {
        if changes[..i]
            .iter()
            .any(|other| other.overlaps(change) == Some(true))
        {
            return Err(Failed::Overlap);
        }
        for position in &change.path {
            if let hir::Expr::Constant(hir::Value::Integer(index)) = *position
                && index < 1
            {
                return Err(Failed::Scalar(hir::Failure::OutOfRange {
                    index,
                    length: None,
                }));
            }
        }
    }
    let known = |e: &hir::Expr| known_value(e).is_some();
    let hir::Expr::Constant(hir::Value::Array(mut value)) = array else {
        return Ok(hir::Expr::WithElements(Box::new(array), changes));
    };
    if !changes
        .iter()
        .all(|change| change.path.iter().all(known) && known(&change.value))
    {
        let array = hir::Expr::Constant(hir::Value::Array(value));
        return Ok(hir::Expr::WithElements(Box::new(array), changes));
    }
    for change in changes {
        let path: Vec<i64> = change
            .path
            .iter()
            .map(|p| match p {
                hir::Expr::Constant(hir::Value::Integer(index)) => *index,
                _ => unreachable!("sema checks positions are integers"),
            })
            .collect();
        let element = known_value(&change.value).expect("every value is known");
        changed(&mut value, &path, element.clone()).map_err(Failed::Scalar)?;
    }
    Ok(hir::Expr::Constant(hir::Value::Array(value)))
}

/// Give the element of `array` at `path`, positions from the outermost
/// array in, `element`.
///
/// # Errors
///
/// Returns the position that is out of range, if one is.
fn changed(
    array: &mut hir::ArrayValue,
    path: &[i64],
    element: hir::Value,
) -> Result<(), hir::Failure> {
    let (&index, rest) = path.split_first().expect("a path has a position");
    let length = array.elements.len();
    let at = usize::try_from(index)
        .ok()
        .and_then(|i| i.checked_sub(1))
        .filter(|at| *at < length)
        .ok_or(hir::Failure::OutOfRange {
            index,
            length: Some(length),
        })?;
    if rest.is_empty() {
        array.elements[at] = element;
        return Ok(());
    }
    let hir::Value::Array(inner) = &mut array.elements[at] else {
        unreachable!("sema indexes only arrays");
    };
    changed(inner, rest, element)
}

/// `then` if `condition` is true, and else `otherwise`: decided now if the
/// condition is a constant, or if both are the same and the condition
/// can't fail, since then nothing tells them apart.
pub(crate) fn conditional(
    condition: hir::Expr,
    then: hir::Expr,
    otherwise: hir::Expr,
) -> hir::Expr {
    match condition {
        hir::Expr::Constant(hir::Value::Boolean(true)) => then,
        hir::Expr::Constant(hir::Value::Boolean(false)) => otherwise,
        condition if then == otherwise && !condition.can_fail() => then,
        condition => hir::Expr::If(Box::new(condition), Box::new(then), Box::new(otherwise)),
    }
}

/// `operand` with the relation-valued attributes `names` replaced by their
/// attributes: evaluated if the operand is a constant.
pub(crate) fn ungroup(operand: hir::Expr, names: BTreeSet<String>) -> hir::Expr {
    match operand {
        hir::Expr::Constant(value) => hir::Expr::Constant(value.ungrouped(&names)),
        operand => hir::Expr::Ungroup(Box::new(operand), names),
    }
}

/// What a `group` or a `wrap` makes of the attributes each of its
/// gatherings names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Gathering {
    /// A relation-valued attribute, of the tuples that share the rest.
    Group,
    /// A tuple-valued attribute, of each tuple's own.
    Wrap,
}

impl Gathering {
    /// What `postfix` gathers, if it is a `group` or a `wrap`.
    pub(crate) fn of(postfix: &ast::PostfixExpr) -> Option<Gathering> {
        match postfix.operator()?.kind() {
            SyntaxKind::GroupKw => Some(Gathering::Group),
            SyntaxKind::WrapKw => Some(Gathering::Wrap),
            _ => None,
        }
    }

    pub(crate) fn keyword(self) -> &'static str {
        match self {
            Gathering::Group => "group",
            Gathering::Wrap => "wrap",
        }
    }
}

/// What an `ungroup` or an `unwrap` replaces with its attributes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Unpacking {
    /// Relation-valued attributes.
    Ungroup,
    /// Tuple-valued attributes.
    Unwrap,
}

impl Unpacking {
    /// What `postfix` unpacks, if it is an `ungroup` or an `unwrap`.
    pub(crate) fn of(postfix: &ast::PostfixExpr) -> Option<Unpacking> {
        match postfix.operator()?.kind() {
            SyntaxKind::UngroupKw => Some(Unpacking::Ungroup),
            SyntaxKind::UnwrapKw => Some(Unpacking::Unwrap),
            _ => None,
        }
    }

    pub(crate) fn keyword(self) -> &'static str {
        match self {
            Unpacking::Ungroup => "ungroup",
            Unpacking::Unwrap => "unwrap",
        }
    }

    /// The heading of the attribute of type `ty` it unpacks, if it is the
    /// kind it unpacks.
    pub(crate) fn inner(self, ty: &hir::Type) -> Option<&hir::Heading> {
        match (self, ty) {
            (Unpacking::Ungroup, hir::Type::Relation(inner))
            | (Unpacking::Unwrap, hir::Type::Tuple(inner)) => Some(inner),
            _ => None,
        }
    }
}

/// `operand`, a relation with `heading`, grouped: projected on the
/// attributes no grouping gathers, and extended with the image relation of
/// each of its tuples in the operand, projected on each grouping's
/// attributes, as Appendix A defines GROUP. The groupings are simultaneous:
/// each is of the same image.
///
/// # Errors
///
/// Returns why computing a group of a constant operand fails.
pub(crate) fn group(
    operand: hir::Expr,
    heading: &hir::Heading,
    groups: Vec<(String, BTreeSet<String>)>,
    bindings: Bindings,
) -> Result<hir::Expr, TupleFailure> {
    let all: BTreeSet<String> = groups.iter().flat_map(|(_, set)| set.clone()).collect();
    let kept = heading.without(&all);
    // Each group is computed inside the extend's values, one tuple at hand
    // further in than the operand was written.
    let image = image(deeper(operand.clone()), heading, &kept);
    let mut types = BTreeMap::new();
    let mut values = BTreeMap::new();
    for (name, set) in groups {
        // The image has every gathered attribute; each group, its own.
        let others: BTreeSet<String> = all.difference(&set).cloned().collect();
        types.insert(name.clone(), hir::Type::Relation(restricted(heading, &set)));
        values.insert(name, remove(image.clone(), others));
    }
    extend(
        remove(operand, all),
        hir::Heading::new(types),
        values,
        bindings,
    )
}

/// `operand`, a tuple or relation with `heading`, wrapped: each wrapping's
/// attributes replaced by a tuple-valued attribute of them, as Appendix A
/// defines WRAP: an extend, then a remove of what was wrapped. A new name
/// can be one it wraps, which the extend then replaces.
pub(crate) fn wrap(
    operand: hir::Expr,
    heading: &hir::Heading,
    wraps: Vec<(String, BTreeSet<String>)>,
    bindings: Bindings,
) -> hir::Expr {
    let tuple = || hir::Expr::Current(0, heading.clone());
    let mut wrapped = BTreeSet::new();
    let mut types = BTreeMap::new();
    let mut values = BTreeMap::new();
    for (name, set) in wraps {
        let attributes = set
            .iter()
            .map(|a| (a.clone(), extract(tuple(), a.clone())))
            .collect();
        types.insert(name.clone(), hir::Type::Tuple(restricted(heading, &set)));
        values.insert(name, self::tuple(attributes));
        wrapped.extend(set);
    }
    let gone = wrapped
        .difference(&types.keys().cloned().collect())
        .cloned()
        .collect();
    let extended = extend(operand, hir::Heading::new(types), values, bindings)
        .unwrap_or_else(|_| unreachable!("wrapping only extracts, which can't fail"));
    remove(extended, gone)
}

/// `operand`, a tuple or relation with `heading`, with the tuple-valued
/// attributes `names` replaced by their attributes, as Appendix A defines
/// UNWRAP: an extend with each of them, then a remove of what was
/// unwrapped. An attribute can have the name of one it unwraps, which the
/// extend then replaces.
pub(crate) fn unwrap(
    operand: hir::Expr,
    heading: &hir::Heading,
    names: &BTreeSet<String>,
    bindings: Bindings,
) -> hir::Expr {
    let tuple = || hir::Expr::Current(0, heading.clone());
    let mut types = BTreeMap::new();
    let mut values = BTreeMap::new();
    for name in names {
        let Some(hir::Type::Tuple(inner)) = heading.get(name) else {
            unreachable!("sema unwraps only tuple-valued attributes");
        };
        for (attribute, ty) in inner.attributes() {
            types.insert(attribute.to_string(), ty.clone());
            values.insert(
                attribute.to_string(),
                extract(extract(tuple(), name.clone()), attribute.to_string()),
            );
        }
    }
    let gone = names
        .difference(&types.keys().cloned().collect())
        .cloned()
        .collect();
    let extended = extend(operand, hir::Heading::new(types), values, bindings)
        .unwrap_or_else(|_| unreachable!("unwrapping only extracts, which can't fail"));
    remove(extended, gone)
}

/// `heading` with only the attributes in `names`.
fn restricted(heading: &hir::Heading, names: &BTreeSet<String>) -> hir::Heading {
    let others = heading
        .attributes()
        .map(|(a, _)| a.to_string())
        .filter(|a| !names.contains(a))
        .collect();
    heading.without(&others)
}

/// What an `extend`, a `replace` or an `update` does with its attribute
/// assignments.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Assigning {
    /// Adds attributes the operand doesn't have.
    Extend,
    /// Gives attributes the operand has new values of the same types.
    Replace,
    /// Gives attributes a variable has new values of the same types: a
    /// `replace` of the variable, assigned to it.
    Update,
}

impl Assigning {
    /// What `postfix` assigns, if it is an `extend` or a `replace`.
    pub(crate) fn of(postfix: &ast::PostfixExpr) -> Option<Assigning> {
        match postfix.operator()?.kind() {
            SyntaxKind::ExtendKw => Some(Assigning::Extend),
            SyntaxKind::ReplaceKw => Some(Assigning::Replace),
            _ => None,
        }
    }

    /// The keyword.
    pub(crate) fn keyword(self) -> &'static str {
        match self {
            Assigning::Extend => "extend",
            Assigning::Replace => "replace",
            Assigning::Update => "update",
        }
    }
}

/// `expr`, written outside an `Extend`, as seen from inside its values:
/// each tuple at hand it refers to is one `Extend` further out. As for a
/// name that resolved outside, or an operand repeated inside, as `group`
/// repeats its own.
pub(crate) fn deeper(expr: hir::Expr) -> hir::Expr {
    shifted(expr, 0)
}

/// `expr` with each tuple at hand `cutoff` or more `Extend`s out from it
/// one further out; those nearer are inside it, and stay.
fn shifted(expr: hir::Expr, cutoff: usize) -> hir::Expr {
    match expr {
        hir::Expr::Current(depth, heading) if depth >= cutoff => {
            hir::Expr::Current(depth + 1, heading)
        }
        expr => expr.map_children(|child, deeper| shifted(child, cutoff + deeper)),
    }
}

/// The variables, of the program and local, and the parameters, that
/// `expr` reads, each once.
pub(crate) fn targets_read(expr: &hir::Expr) -> Vec<hir::Target> {
    fn walk(expr: &hir::Expr, read: &mut Vec<hir::Target>) {
        let target = match expr {
            hir::Expr::Variable(id) => hir::Target::Variable(*id),
            hir::Expr::Local(id) => hir::Target::Local(*id),
            hir::Expr::Parameter(name) => hir::Target::Parameter(name.clone()),
            _ => return expr.for_each_child(|child, _| walk(child, read)),
        };
        if !read.contains(&target) {
            read.push(target);
        }
    }
    let mut read = Vec::new();
    walk(expr, &mut read);
    read
}

/// Whether `expr` uses the tuple at hand `depth` `Extend`s out from it:
/// one of its attributes, or an image relation of it, which names none.
pub(crate) fn uses_tuple(expr: &hir::Expr, depth: usize) -> bool {
    let read = expr.tuple_read(depth);
    read.whole || !read.attributes.is_empty()
}

/// The image relation of the tuple at hand, with heading `at_hand`, in
/// `relation`, with `heading`: its tuples that match the tuple at hand on
/// their common attributes, without those attributes. That is `relation`
/// joined with the tuple at hand projected on the common attributes, then
/// without them. With none in common, it is all of `relation`.
pub(crate) fn image(
    relation: hir::Expr,
    heading: &hir::Heading,
    at_hand: &hir::Heading,
) -> hir::Expr {
    let common = heading.common(at_hand);
    let others: BTreeSet<String> = at_hand
        .attributes()
        .map(|(name, _)| name.to_string())
        .filter(|name| !common.contains(name))
        .collect();
    let matched = remove(hir::Expr::Current(0, at_hand.clone()), others.clone());
    let matching = self::relation(at_hand.without(&others), vec![matched]);
    let joined = and(vec![relation, matching])
        .unwrap_or_else(|_| unreachable!("relations always have an AND"));
    remove(joined, common)
}

/// Why an `extend` or a `where` of constants has no value: computing
/// `attribute` for `tuple`, or, with none, the condition, fails.
pub(crate) struct TupleFailure {
    pub(crate) failed: Failed,
    pub(crate) attribute: Option<String>,
    pub(crate) tuple: hir::TupleValue,
}

/// `operand` with `attributes`, of the types in `computed`, added or
/// replaced, each computed from the tuple at hand. Evaluated when the operand is a constant
/// and every attribute then is, for each of its tuples.
///
/// # Errors
///
/// Returns why computing an attribute of a constant tuple fails.
pub(crate) fn extend(
    operand: hir::Expr,
    computed: hir::Heading,
    attributes: BTreeMap<String, hir::Expr>,
    bindings: Bindings,
) -> Result<hir::Expr, TupleFailure> {
    let folded = match known_value(&operand) {
        Some(hir::Value::Tuple(tuple)) => {
            let hir::Type::Tuple(heading) = known_type(&operand) else {
                unreachable!("a tuple's type is a tuple type");
            };
            let ty = hir::Type::Tuple(heading.with(&computed));
            return Ok(match extended(tuple, &attributes, bindings)? {
                Some(tuple) => typed(hir::Value::Tuple(tuple), &ty),
                None => hir::Expr::Extend(Box::new(operand), computed, attributes),
            });
        }
        Some(hir::Value::Relation(relation)) => {
            let mut tuples = BTreeSet::new();
            let mut all = true;
            for tuple in &relation.tuples {
                match extended(tuple, &attributes, bindings)? {
                    Some(tuple) => {
                        tuples.insert(tuple);
                    }
                    None => all = false,
                }
            }
            all.then(|| {
                hir::Value::Relation(hir::RelationValue {
                    heading: relation.heading.with(&computed),
                    tuples,
                })
            })
        }
        _ => None,
    };
    Ok(match folded {
        Some(value) => hir::Expr::Constant(value),
        None => hir::Expr::Extend(Box::new(operand), computed, attributes),
    })
}

/// `tuple` with `attributes` added or replaced, each computed with `tuple`
/// as the tuple at hand, if every one is then a constant.
fn extended(
    tuple: &hir::TupleValue,
    attributes: &BTreeMap<String, hir::Expr>,
    bindings: Bindings,
) -> Result<Option<hir::TupleValue>, TupleFailure> {
    let mut computed = BTreeMap::new();
    for (name, value) in attributes {
        match substitute(value.clone(), 0, &Substitution::tuple(tuple, bindings)) {
            Ok(value) if known_value(&value).is_some() => {
                let value = known_value(&value).expect("just checked").clone();
                computed.insert(name.clone(), value);
            }
            Ok(_) => return Ok(None),
            Err(failed) => {
                return Err(TupleFailure {
                    failed,
                    attribute: Some(name.clone()),
                    tuple: tuple.clone(),
                });
            }
        }
    }
    Ok(Some(tuple.with(computed)))
}

/// What evaluating a call substitutes for the leaves of the callee's value,
/// besides its arguments for its parameters.
#[derive(Clone, Copy)]
pub(crate) struct Bindings<'a> {
    /// Every operator, by id, for the built-ins among them, which the
    /// compiler evaluates.
    pub(crate) operators: &'a Operators,
    /// The program's types, for the subtypes defined by constraints that a
    /// value selected may be of.
    pub(crate) namespaces: &'a crate::Namespaces,
    /// The read-only operators' checked values. A call is checked after its
    /// callee, so every operator a call's evaluation reaches is here. None
    /// reads a variable of the program's, so its value depends on its
    /// arguments alone.
    pub(crate) values: &'a HashMap<hir::OperatorId, hir::Expr>,
}

/// What [`substitute`] replaces in an expression, which it then folds
/// again.
struct Substitution<'a> {
    /// The tuple at hand where substituting starts, `Current(0)` there.
    tuple: Option<&'a hir::TupleValue>,
    /// A call's arguments, for its callee's parameters, each known when
    /// compiling, of its parameter's type (see [`known_value`]).
    args: Option<&'a BTreeMap<String, hir::Expr>>,
    /// The values of the callee's local variables known so far, each its
    /// `Let`'s, once the value is known, of the local variable's type.
    locals: &'a [(hir::LocalId, hir::Expr)],
    bindings: Bindings<'a>,
}

/// The value `expr` has, if it is known when compiling: a constant's, or
/// that of one treated as a value of a supertype of its own type.
pub(crate) fn known_value(expr: &hir::Expr) -> Option<&hir::Value> {
    match expr {
        hir::Expr::Constant(value) => Some(value),
        hir::Expr::Treat(_, inner) => known_value(inner),
        _ => None,
    }
}

/// The type of `expr`, which is known when compiling (see
/// [`known_value`]): its value's, or the one it is treated as.
fn known_type(expr: &hir::Expr) -> hir::Type {
    match expr {
        hir::Expr::Treat(ty, _) => (**ty).clone(),
        expr => known_value(expr).expect("a known value").ty(),
    }
}

/// Whether the value of `value` is of type `ty` (IM Prescription 15):
/// evaluated if it is known.
pub(crate) fn is(value: hir::Expr, ty: hir::Type) -> hir::Expr {
    match known_value(&value) {
        Some(known) => hir::Expr::Constant(hir::Value::Boolean(known.is_of(&ty))),
        None => hir::Expr::Is(Box::new(value), Box::new(ty)),
    }
}

/// `value` treated as one of `ty`, a subtype of its type, which fails
/// unless it is one (IM Prescription 14): evaluated if it is known to be
/// one. A known value of another type is treated only when running, which
/// never happens where it is narrowed: the test that narrows it is known
/// to be false then, and decides what it guards without it.
pub(crate) fn treat_as(value: hir::Expr, ty: hir::Type) -> hir::Expr {
    match known_value(&value) {
        Some(known) if known.is_of(&ty) => typed(known.clone(), &ty),
        _ => hir::Expr::TreatAs(Box::new(ty), Box::new(value)),
    }
}

/// `value` treated as one of `ty`, a subtype of its type, as `treat … as
/// …` does: as [`treat_as`], but a known value of another type fails now.
///
/// # Errors
///
/// Returns the known value, and `ty`, if it isn't of `ty`.
pub(crate) fn treated_as(value: hir::Expr, ty: hir::Type) -> Result<hir::Expr, Failed> {
    match known_value(&value) {
        Some(known) if !known.is_of(&ty) => Err(Failed::NotOfType(Box::new((known.clone(), ty)))),
        _ => Ok(treat_as(value, ty)),
    }
}

/// `value`, known when compiling, as an expression of type `ty`, its own
/// type or a supertype of it, as what it is computed from says.
fn typed(value: hir::Value, ty: &hir::Type) -> hir::Expr {
    hir::Expr::Constant(value).treated(ty)
}

impl<'a> Substitution<'a> {
    fn tuple(tuple: &'a hir::TupleValue, bindings: Bindings<'a>) -> Self {
        Substitution {
            tuple: Some(tuple),
            args: None,
            locals: &[],
            bindings,
        }
    }
}

/// A call of the read-only operator `id` with `args`: its value, with the
/// arguments substituted for its parameters and folded, if they are all
/// known when compiling, and otherwise the call itself.
///
/// # Errors
///
/// Returns why evaluating the operator's value for the arguments fails.
pub(crate) fn call(call: hir::Call, bindings: Bindings) -> Result<hir::Expr, Failed> {
    let known: Option<BTreeMap<String, hir::Value>> = call
        .args
        .iter()
        .map(|(name, arg)| Some((name.clone(), known_value(arg)?.clone())))
        .collect();
    let Some(known) = known else {
        return Ok(hir::Expr::Call(call));
    };
    // A specification's call computes the first of its versions, the most
    // specific, whose parameters the arguments' values are of, and
    // otherwise its own value.
    let versions = &bindings.operators.get(call.operator).versions;
    if let Some(version) = versions.iter().find(|version| {
        version
            .params
            .iter()
            .all(|(name, ty)| known[name].is_of(ty))
    }) {
        let result = bindings.operators.get(call.operator).result.clone();
        // Each argument of the version's parameter's type.
        let typed_params = &bindings.operators.get(version.operator).params;
        let args = known
            .iter()
            .map(|(name, value)| (name.clone(), typed(value.clone(), &typed_params[name].ty)))
            .collect();
        let computed = self::call(
            hir::Call {
                operator: version.operator,
                args,
                ..call.clone()
            },
            bindings,
        )?;
        // Of the specification's type, which its versions' are subtypes
        // of; one known only when running is chosen then.
        return Ok(match (result, known_value(&computed)) {
            (Some(result), Some(_)) => computed.treated(&result),
            _ => hir::Expr::Call(call),
        });
    }
    if let Some(builtin) = bindings.operators.get(call.operator).builtin {
        let hir::Builtin::ReadOnly(builtin) = builtin else {
            unreachable!("only read-only operators' calls are values");
        };
        // An argument whose type has a type parameter, such as the empty
        // `relation { v: T } {}`, is known only up to that type, which
        // `to_text` writes: it is evaluated for each list of types instead.
        if call.types.iter().any(|t| t.parameters().next().is_some()) {
            return Ok(hir::Expr::Call(call));
        }
        // `min` and `max` compare the values of `of`.
        let compared = match builtin {
            hir::ReadOnlyBuiltin::Min | hir::ReadOnlyBuiltin::Max => match &known["r"] {
                hir::Value::Relation(r) => r.tuples.iter().filter_map(|t| t.get("of")).collect(),
                other => panic!("`{}` of {other:?}", builtin.name()),
            },
            _ => Vec::new(),
        };
        let Some(converted) = converted(compared, bindings)? else {
            return Ok(hir::Expr::Call(call));
        };
        let value = builtin
            .evaluate(&known, &converted)
            .map_err(|failure| match failure {
                hir::BuiltinFailure::NoTuples => Failed::NoTuples(builtin),
                hir::BuiltinFailure::Failure(failure) => Failed::Scalar(failure),
                hir::BuiltinFailure::Unparsable(why) => match &known["from"] {
                    hir::Value::Text(text) => Failed::Unparsable {
                        builtin,
                        text: text.clone(),
                        why,
                    },
                    other => panic!("`{}` of {other:?}", builtin.name()),
                },
            })?;
        return Ok(hir::Expr::Constant(value));
    }
    // The callee's value for the types its type parameters are given.
    let value = &bindings.values[&call.operator];
    let value = if call.types.is_empty() {
        value.clone()
    } else {
        value.instantiated(&call.types)
    };
    let substitution = Substitution {
        tuple: None,
        args: Some(&call.args),
        locals: &[],
        bindings,
    };
    let evaluated = substitute(value, 0, &substitution)?;
    Ok(match known_value(&evaluated) {
        Some(_) => evaluated,
        // Otherwise the call is made when the program runs.
        None => hir::Expr::Call(call),
    })
}

/// The failure of a `fail` of the error `error`: its message, as the
/// prelude's `message` writes it, or that operator's own failure.
fn own_failure(error: hir::Value, bindings: Bindings) -> Failed {
    let Some(message) = bindings.operators.message() else {
        // The prelude doesn't declare it, which is reported.
        return Failed::Own(error.to_string());
    };
    let call = hir::Call {
        operator: message.id,
        types: Vec::new(),
        args: BTreeMap::from([("error".to_string(), hir::Expr::Constant(error))]),
        updated: BTreeMap::new(),
    };
    match self::call(call, bindings) {
        Ok(hir::Expr::Constant(hir::Value::Text(text))) => Failed::Own(text),
        Ok(other) => unreachable!("an error's message is known with the error: {other:?}"),
        Err(failed) => failed,
    }
}

/// `expr`, every part of which is known when compiling, computed: a
/// constant, unless a `fail` in it is computed, as it is unless a
/// condition leaves it out.
///
/// # Errors
///
/// Returns why it fails.
pub(crate) fn evaluated(expr: hir::Expr, bindings: Bindings) -> Result<hir::Expr, Failed> {
    let substitution = Substitution {
        tuple: None,
        args: None,
        locals: &[],
        bindings,
    };
    substitute(expr, 0, &substitution)
}

/// `array` with `changes` made, substituted as for [`substitute`].
///
/// # Errors
///
/// Returns why a position, a value or the changes fail.
fn substituted_elements(
    array: hir::Expr,
    changes: Vec<hir::ElementChange>,
    depth: usize,
    substitution: &Substitution,
) -> Result<hir::Expr, Failed> {
    let sub = |e: hir::Expr| substitute(e, depth, substitution);
    let changes = changes
        .into_iter()
        .map(|change| {
            Ok(hir::ElementChange {
                path: change.path.into_iter().map(sub).collect::<Result<_, _>>()?,
                value: sub(change.value)?,
            })
        })
        .collect::<Result<_, _>>()?;
    with_elements(sub(array)?, changes)
}

/// The `connective` of `operands`, with what `substitution` knows
/// substituted, as [`substitute`] does. Of the operands of an `and` or an
/// `or`, those that can fail are computed only when those that can't don't
/// decide the result (see [`hir::Connective::guarded`]), and of those that
/// can't, each only if those before it don't: since they can't fail, that
/// only saves the work.
///
/// # Errors
///
/// Returns why an operand fails.
fn substituted_connective(
    connective: hir::Connective,
    operands: Vec<hir::Expr>,
    depth: usize,
    substitution: &Substitution,
) -> Result<hir::Expr, Failed> {
    if let Some(guarded) = connective.guarded(&operands) {
        return substitute(guarded, depth, substitution);
    }
    let guards = !operands.iter().any(hir::Expr::can_fail);
    let mut substituted = Vec::new();
    for operand in operands {
        let operand = substitute(operand, depth, substitution)?;
        if guards
            && let hir::Expr::Constant(hir::Value::Boolean(value)) = operand
            && let Some(result) = connective.decided(value)
        {
            return Ok(hir::Expr::Constant(hir::Value::Boolean(result)));
        }
        substituted.push(operand);
    }
    Ok(connective.of(substituted))
}

/// `if condition then then else otherwise`, with what `substitution`
/// knows substituted, as [`substitute`] does: only the branch taken is
/// computed.
///
/// # Errors
///
/// Returns why the branch taken fails, or the condition does.
fn substituted_if(
    condition: hir::Expr,
    then: hir::Expr,
    otherwise: hir::Expr,
    depth: usize,
    substitution: &Substitution,
) -> Result<hir::Expr, Failed> {
    Ok(match substitute(condition, depth, substitution)? {
        hir::Expr::Constant(hir::Value::Boolean(taken)) => {
            substitute(if taken { then } else { otherwise }, depth, substitution)?
        }
        // Either may be taken when running, or neither: one that fails now
        // stays as it is, to fail only if it is. The whole is no constant,
        // so it isn't the value of what it is in.
        condition => {
            let branch = |e: hir::Expr| substitute(e.clone(), depth, substitution).unwrap_or(e);
            conditional(condition, branch(then), branch(otherwise))
        }
    })
}

/// `expr` with what `substitution` says replaced, and simplified as when
/// it was checked, so that what then depends only on constants is
/// evaluated: the tuple at hand `depth` `Extend`s out, and the parameters
/// of a call's callee. A call whose arguments are then all known is
/// evaluated.
///
/// # Errors
///
/// Returns why an operator now known to fail fails, and its type.
fn substitute(
    expr: hir::Expr,
    depth: usize,
    substitution: &Substitution,
) -> Result<hir::Expr, Failed> {
    let sub = |e: Box<hir::Expr>| substitute(*e, depth, substitution);
    Ok(match expr {
        hir::Expr::Current(d, heading) if d == depth && substitution.tuple.is_some() => {
            let tuple = substitution.tuple.expect("just checked").clone();
            typed(hir::Value::Tuple(tuple), &hir::Type::Tuple(heading))
        }
        hir::Expr::Current(d, heading) if d > depth && substitution.tuple.is_some() => {
            hir::Expr::Current(d - 1, heading)
        }
        hir::Expr::Parameter(name) if substitution.args.is_some() => {
            substitution.args.expect("just checked")[&name].clone()
        }
        hir::Expr::Local(id) => match substitution.locals.iter().find(|(l, _)| *l == id) {
            Some((_, value)) => value.clone(),
            // An update operator's local variable's value is known only
            // when running.
            None => expr,
        },
        hir::Expr::Let(id, value, body) => substituted_let(id, *value, *body, depth, substitution)?,
        // Known, it needs no site.
        hir::Expr::At(site, expr) => hir::Expr::at(site, sub(expr)?),
        // A variable of the program's is known only when running: a
        // read-only operator's value reads none.
        hir::Expr::Current(..)
        | hir::Expr::Constant(_)
        | hir::Expr::Parameter(_)
        | hir::Expr::Variable(_) => expr,
        hir::Expr::Call(called) => {
            let args = called
                .args
                .into_iter()
                .map(|(name, arg)| Ok((name, substitute(arg, depth, substitution)?)))
                .collect::<Result<_, _>>()?;
            call(hir::Call { args, ..called }, substitution.bindings)?
        }
        hir::Expr::Unary(op, operand) => unary(op, sub(operand)?).map_err(Failed::Scalar)?,
        hir::Expr::Binary(op, lhs, rhs) => binary(op, sub(lhs)?, sub(rhs)?, substitution.bindings)?,
        hir::Expr::Connective(connective, operands) => {
            substituted_connective(connective, operands, depth, substitution)?
        }
        hir::Expr::Extract(operand, name) => extract(sub(operand)?, name),
        hir::Expr::Array(element, elements) => array(
            element,
            elements
                .into_iter()
                .map(|e| substitute(e, depth, substitution))
                .collect::<Result<_, _>>()?,
        ),
        hir::Expr::Index(operand, position) => {
            index(sub(operand)?, sub(position)?).map_err(Failed::Scalar)?
        }
        hir::Expr::WithElements(array, changes) => {
            substituted_elements(*array, changes, depth, substitution)?
        }
        hir::Expr::Load(relation, loading) => load(sub(relation)?, loading, substitution.bindings)?,
        hir::Expr::Unload(array, unloading) => unload(sub(array)?, unloading),
        hir::Expr::If(condition, then, otherwise) => {
            substituted_if(*condition, *then, *otherwise, depth, substitution)?
        }
        hir::Expr::Tuple(attributes) => tuple_of(attributes, depth, substitution)?,
        hir::Expr::Relation(heading, tuples) => relation(
            heading,
            tuples
                .into_iter()
                .map(|t| substitute(t, depth, substitution))
                .collect::<Result<_, _>>()?,
        ),
        hir::Expr::Remove(operand, names) => remove(sub(operand)?, names),
        hir::Expr::Rename(operand, renamings) => rename(sub(operand)?, renamings),
        hir::Expr::And(lhs, rhs) => and(vec![sub(lhs)?, sub(rhs)?])?,
        hir::Expr::Ungroup(operand, names) => ungroup(sub(operand)?, names),
        hir::Expr::Only(operand) => only(sub(operand)?)?,
        hir::Expr::Select(defined, components) => {
            select(defined, sub(components)?, substitution.bindings)?
        }
        hir::Expr::Components(value) => components(sub(value)?),
        hir::Expr::Treat(ty, value) => sub(value)?.treated(&ty),
        hir::Expr::TreatAs(ty, value) => treated_as(sub(value)?, *ty)?,
        // Computed, it fails, once its error is known.
        hir::Expr::Fail(error, ty) => {
            let error = sub(error)?;
            match known_value(&error) {
                Some(value) => return Err(own_failure(value.clone(), substitution.bindings)),
                None => hir::Expr::Fail(Box::new(error), ty),
            }
        }
        hir::Expr::Is(value, ty) => is(sub(value)?, *ty),
        hir::Expr::Semijoin(lhs, rhs) => matching(sub(lhs)?, sub(rhs)?, false),
        hir::Expr::Combine(combination, lhs, rhs) => combined(sub(lhs)?, sub(rhs)?, combination)?,
        hir::Expr::Antijoin(lhs, rhs) => matching(sub(lhs)?, sub(rhs)?, true),
        // The condition is one tuple at hand further in.
        hir::Expr::Restrict(operand, condition) => {
            let condition = substitute(*condition, depth + 1, substitution)?;
            restrict(sub(operand)?, condition, substitution.bindings).map_err(|f| f.failed)?
        }
        hir::Expr::Extend(operand, computed, attributes) => {
            // The values are one `Extend` further in.
            let attributes = attributes
                .into_iter()
                .map(|(name, e)| Ok((name, substitute(e, depth + 1, substitution)?)))
                .collect::<Result<_, _>>()?;
            extend(sub(operand)?, computed, attributes, substitution.bindings)
                .map_err(|f| f.failed)?
        }
    })
}

/// `body` with the local variable `id` given `value` in it, substituted as
/// for [`substitute`]: a value that is then known is substituted in the
/// body.
///
/// # Errors
///
/// Returns why the value or the body fails.
fn substituted_let(
    id: hir::LocalId,
    value: hir::Expr,
    body: hir::Expr,
    depth: usize,
    substitution: &Substitution,
) -> Result<hir::Expr, Failed> {
    Ok(match substitute(value, depth, substitution)? {
        value if known_value(&value).is_some() => {
            let mut locals = substitution.locals.to_vec();
            locals.push((id, value));
            let inner = Substitution {
                locals: &locals,
                ..*substitution
            };
            substitute(body, depth, &inner)?
        }
        value => hir::Expr::Let(
            id,
            Box::new(value),
            Box::new(substitute(body, depth, substitution)?),
        ),
    })
}

/// A tuple selector's `attributes`, substituted as for [`substitute`].
fn tuple_of(
    attributes: BTreeMap<String, hir::Expr>,
    depth: usize,
    substitution: &Substitution,
) -> Result<hir::Expr, Failed> {
    Ok(self::tuple(
        attributes
            .into_iter()
            .map(|(name, e)| Ok((name, substitute(e, depth, substitution)?)))
            .collect::<Result<_, _>>()?,
    ))
}

/// The attribute `extract`, written after `operand`, which is of type `ty`,
/// takes the value of, or the component, of a value of a type of the
/// program's own, of its stored representation or one of `others`. `None`
/// after reporting why there is none: the operand isn't a tuple, or hasn't
/// the attribute or component. `ended` is where a statement may assign the
/// operand, a name, which ended what a test showed of its value, if one
/// did.
pub(crate) fn extracted(
    extract: &ast::ExtractExpr,
    (operand, ended): (&Expr, Option<Span>),
    ty: &hir::Type,
    others: &[&hir::Heading],
    namespaces: &crate::Namespaces,
    errors: &mut Vec<Diagnostic>,
) -> Option<String> {
    let ident = extract.attribute()?.ident()?;
    let name = ident.text().to_string();
    let span = trendy_syntax::span(ident.text_range());
    let (heading, noun, context) = match ty {
        hir::Type::Tuple(heading) => (
            heading,
            errors::Noun::Projected,
            errors::attributes_note("tuple", ty),
        ),
        hir::Type::Defined(defined) => (
            &defined.components,
            errors::Noun::Component,
            errors::components_note(defined),
        ),
        _ => {
            errors.push(errors::not_extractable(
                &name,
                span,
                ty,
                (operand, ended),
                namespaces,
            ));
            return None;
        }
    };
    let all = || {
        std::iter::once(heading)
            .chain(others.iter().copied())
            .flat_map(hir::Heading::named)
            .map(|(a, _)| a)
    };
    if !all().any(|a| a == name) {
        let rename = closest(&name, all());
        errors.push(errors::unknown_attribute(
            noun, &name, span, &context, rename, None,
        ));
        return None;
    }
    Some(name)
}

/// The value of the program's own type `defined` whose components are the
/// tuple `components`, which fails unless each of the type's constraints
/// is true of them: evaluated if they are a constant, as each constraint
/// then is.
///
/// # Errors
///
/// Returns the constraint a constant's components break, or why
/// computing one fails.
pub(crate) fn select(
    defined: Box<hir::Defined>,
    components: hir::Expr,
    bindings: Bindings,
) -> Result<hir::Expr, Failed> {
    let Some(hir::Value::Tuple(tuple)) = known_value(&components) else {
        return Ok(hir::Expr::Select(defined, Box::new(components)));
    };
    let tuple = tuple.clone();
    let args: BTreeMap<String, hir::Expr> = defined
        .components
        .attributes()
        .map(|(name, ty)| {
            let value = tuple.get(name).expect("a value has its type's components");
            (name.to_string(), typed(value.clone(), ty))
        })
        .collect();
    let value = hir::DefinedValue {
        ty: defined,
        components: tuple,
    };
    // A relation-valued component's keys first, which constraints can
    // assume.
    if !keys_hold(&value, &args, bindings)? {
        return Ok(hir::Expr::Select(
            value.ty,
            Box::new(hir::Expr::Constant(hir::Value::Tuple(value.components))),
        ));
    }
    let defined = &value.ty;
    // The first constraint that isn't true of them, and its value.
    let mut first = None;
    for constraint in &defined.constraints {
        let checked = call(
            hir::Call {
                operator: constraint.operator,
                types: Vec::new(),
                args: args.clone(),
                updated: BTreeMap::new(),
            },
            bindings,
        )?;
        if checked != hir::Expr::Constant(hir::Value::Boolean(true)) {
            first = Some((constraint.clone(), checked));
            break;
        }
    }
    match first {
        // Of the most specific type whose constraints are true of them,
        // as the type it is selected of.
        None => match most_specific(&value.ty, &args, &value, bindings)? {
            Some(ty) => {
                let selected = hir::Type::Defined(value.ty.clone());
                let value = hir::DefinedValue {
                    ty,
                    components: value.components,
                };
                Ok(typed(hir::Value::Defined(value), &selected))
            }
            None => Ok(hir::Expr::Select(
                value.ty,
                Box::new(hir::Expr::Constant(hir::Value::Tuple(value.components))),
            )),
        },
        Some((constraint, hir::Expr::Constant(hir::Value::Boolean(false)))) => {
            let written = bindings.operators.get(constraint.operator).constrains();
            Err(Failed::Constraint(Box::new(BrokenConstraint {
                value,
                text: constraint.text,
                at: written.map(|(_, c)| c.span()),
            })))
        }
        // Its value is known only when running, and so is whether the
        // components satisfy it.
        Some(_) => Ok(hir::Expr::Select(
            value.ty,
            Box::new(hir::Expr::Constant(hir::Value::Tuple(value.components))),
        )),
    }
}

/// Whether each key of each relation-valued component of `value`, a value
/// of a type of the program's own whose components are `args`, is true of
/// it: of a representation other than the stored one, its `from` computes
/// the component. `false` if one is known only when running.
///
/// # Errors
///
/// Returns the first key broken, with its tuples, or why computing a
/// component fails.
fn keys_hold(
    value: &hir::DefinedValue,
    args: &BTreeMap<String, hir::Expr>,
    bindings: Bindings,
) -> Result<bool, Failed> {
    let defined = &value.ty;
    for (component, keys) in &defined.keys {
        let relation = match defined.representation(component) {
            Some((_, Some(other))) => {
                let converted = call(
                    hir::Call {
                        operator: other.from,
                        types: Vec::new(),
                        args: args.clone(),
                        updated: BTreeMap::new(),
                    },
                    bindings,
                )?;
                match known_value(&converted) {
                    Some(hir::Value::Tuple(own)) => own.get(component).cloned(),
                    _ => None,
                }
            }
            _ => value.components.get(component).cloned(),
        };
        let Some(hir::Value::Relation(relation)) = relation else {
            return Ok(false);
        };
        for key in keys {
            if let Some((shared, [a, b])) = crate::keys::sharing(&relation, key).into_iter().next()
            {
                return Err(Failed::Key(Box::new(BrokenKey {
                    value: value.clone(),
                    component: component.clone(),
                    shared,
                    tuples: [a.clone(), b.clone()],
                    at: crate::keys::written_in(defined, component, key, bindings.operators),
                })));
            }
        }
    }
    Ok(true)
}

/// The most specific type of `value`, a value of `defined` whose
/// components are `args`, of which `defined`'s constraints are true (*Type
/// Inheritance and Relational Theory*, IM Prescription 10): of its
/// subtypes defined by constraints, the one whose specialization
/// constraints are true of them too, its most specific type in turn, or
/// else `defined` itself. `None` if a constraint's value is known only
/// when running.
///
/// # Errors
///
/// Returns two subtypes whose constraints are both true of them, since a
/// value has one most specific type, or why computing a constraint fails.
fn most_specific(
    defined: &hir::Defined,
    args: &BTreeMap<String, hir::Expr>,
    value: &hir::DefinedValue,
    bindings: Bindings,
) -> Result<Option<Box<hir::Defined>>, Failed> {
    let mut found: Option<&hir::Defined> = None;
    for subtype in bindings.namespaces.subtypes(&defined.name) {
        let hir::Type::Defined(subtype) = subtype else {
            unreachable!("a regular type's subtypes are regular types");
        };
        let mut holds = true;
        for constraint in subtype.own_constraints() {
            let checked = call(
                hir::Call {
                    operator: constraint.operator,
                    types: Vec::new(),
                    args: args.clone(),
                    updated: BTreeMap::new(),
                },
                bindings,
            )?;
            match checked {
                hir::Expr::Constant(hir::Value::Boolean(true)) => {}
                hir::Expr::Constant(hir::Value::Boolean(false)) => {
                    holds = false;
                    break;
                }
                _ => return Ok(None),
            }
        }
        if !holds {
            continue;
        }
        if let Some(other) = found {
            let declared = |name: &str| bindings.namespaces.defined(name).map(|(at, _)| at);
            return Err(Failed::Overlapping(Box::new(Overlapping {
                value: value.clone(),
                types: [other.name.clone(), subtype.name.clone()],
                declared: [declared(&other.name), declared(&subtype.name)],
            })));
        }
        found = Some(subtype);
    }
    match found {
        Some(subtype) => most_specific(subtype, args, value, bindings),
        None => Ok(Some(Box::new(defined.clone()))),
    }
}

/// A value selected of a type of the program's own, `value`, that is of
/// two of its subtypes defined by constraints, `types`, each declared
/// where `declared` says, neither a subtype of the other.
#[derive(Debug, Clone)]
pub(crate) struct Overlapping {
    pub(crate) value: hir::DefinedValue,
    pub(crate) types: [String; 2],
    pub(crate) declared: [Option<Span>; 2],
}

/// A value of a type of the program's own whose relation-valued
/// `component` has two tuples, `tuples`, with the same values, `shared`,
/// for one of its keys, written at `at`.
#[derive(Debug, Clone)]
pub(crate) struct BrokenKey {
    pub(crate) value: hir::DefinedValue,
    pub(crate) component: String,
    pub(crate) shared: hir::TupleValue,
    pub(crate) tuples: [hir::TupleValue; 2],
    pub(crate) at: Option<Span>,
}

/// A value of a type of the program's own whose components break one of
/// its constraints: the condition, as it is written, at `at`.
#[derive(Debug, Clone)]
pub(crate) struct BrokenConstraint {
    pub(crate) value: hir::DefinedValue,
    pub(crate) text: String,
    pub(crate) at: Option<Span>,
}

/// The components of `value`, a value of a type of the program's own, as a
/// tuple: evaluated if it is a constant.
pub(crate) fn components(value: hir::Expr) -> hir::Expr {
    match known_value(&value) {
        Some(hir::Value::Defined(known)) => {
            let ty = hir::Type::Tuple(known.ty.components.clone());
            typed(hir::Value::Tuple(known.components.clone()), &ty)
        }
        _ => hir::Expr::Components(Box::new(value)),
    }
}

/// The value of `operand`'s attribute `name`: evaluated if the operand is a
/// constant, and taken straight from a selector, a rename or a remove where
/// nothing that can fail is dropped.
pub(crate) fn extract(operand: hir::Expr, name: String) -> hir::Expr {
    if let Some(hir::Value::Tuple(tuple)) = known_value(&operand) {
        let value = tuple
            .get(&name)
            .cloned()
            .expect("sema extracts only attributes the tuple has");
        let hir::Type::Tuple(heading) = known_type(&operand) else {
            unreachable!("a tuple's type is a tuple type");
        };
        let ty = heading
            .get(&name)
            .expect("the tuple's type has its attributes");
        return typed(value, ty);
    }
    match operand {
        hir::Expr::Tuple(mut attributes)
            if attributes
                .iter()
                .all(|(other, e)| *other == name || !e.can_fail()) =>
        {
            attributes
                .remove(&name)
                .expect("sema extracts only attributes the tuple has")
        }
        // A tuple's removals and renamings only move attributes around.
        hir::Expr::Remove(tuple, _) => extract(*tuple, name),
        hir::Expr::Rename(tuple, renamings) => {
            let before = renamings
                .iter()
                .find(|(_, after)| **after == name)
                .map_or(name.clone(), |(before, _)| before.clone());
            extract(*tuple, before)
        }
        operand => hir::Expr::Extract(Box::new(operand), name),
    }
}

/// Whether `postfix` is a `rename`, rather than a `remove`.
pub(crate) fn is_rename(postfix: &ast::PostfixExpr) -> bool {
    postfix
        .operator()
        .is_some_and(|t| t.kind() == SyntaxKind::RenameKw)
}

/// `operand` with the attributes renamed, evaluated if it's a constant.
pub(crate) fn rename(operand: hir::Expr, renamings: BTreeMap<String, String>) -> hir::Expr {
    if renamings.is_empty() {
        return operand;
    }
    match known_value(&operand) {
        Some(value) => typed(
            value.renamed(&renamings),
            &known_type(&operand).renamed(&renamings),
        ),
        None => hir::Expr::Rename(Box::new(operand), renamings),
    }
}

/// `operand` without the attributes `names`, evaluated if it's a constant,
/// and merged with a removal it already is.
pub(crate) fn remove(operand: hir::Expr, names: BTreeSet<String>) -> hir::Expr {
    if names.is_empty() {
        return operand;
    }
    if let Some(value) = known_value(&operand) {
        return typed(value.without(&names), &known_type(&operand).without(&names));
    }
    match operand {
        hir::Expr::Remove(inner, mut earlier) => {
            earlier.extend(names);
            hir::Expr::Remove(inner, earlier)
        }
        operand => hir::Expr::Remove(Box::new(operand), names),
    }
}

/// The number literal `unary` negates directly, as in `-1` or `-0.5`. Such a
/// literal is a value of its own, so that the smallest integer can be
/// written.
pub(crate) fn negated_literal(unary: &ast::UnaryExpr) -> Option<Literal> {
    if unary.operator()?.kind() != SyntaxKind::Minus {
        return None;
    }
    match unary.operand()? {
        Expr::Literal(literal)
            if literal.token().is_some_and(|t| {
                matches!(
                    t.kind(),
                    SyntaxKind::IntLiteral | SyntaxKind::ApproximateLiteral
                )
            }) =>
        {
            Some(literal)
        }
        _ => None,
    }
}

/// The value of `-literal`, written `expr`, or `None` after reporting why
/// it has none.
pub(crate) fn negative_value(
    literal: &Literal,
    expr: &Expr,
    errors: &mut Vec<Diagnostic>,
) -> Option<hir::Value> {
    let token = literal.token()?;
    match token.kind() {
        SyntaxKind::IntLiteral => {
            literal::check_int(token.text()).ok()?;
            let value = literal::int_value(&format!("-{}", token.text()));
            if value.is_none() {
                errors.push(errors::integer_out_of_range(expr.span()));
            }
            value.map(hir::Value::Integer)
        }
        _ => match literal.value() {
            Ok(ast::LiteralValue::Approximate(r)) => {
                Some(hir::Value::Approximate(hir::Approximate::new(-r)))
            }
            Err(LiteralError::ApproximateTooLarge) => {
                errors.push(errors::approximate_out_of_range(expr.span()));
                None
            }
            _ => None,
        },
    }
}

/// A constant that simplification knows the effect of.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Known {
    /// `0` or `0.0`.
    Zero,
    /// `""`
    Empty,
    One,
    MinusOne,
}

fn known(expr: &hir::Expr) -> Option<Known> {
    let hir::Expr::Constant(value) = expr else {
        return None;
    };
    Some(match value {
        hir::Value::Integer(0) => Known::Zero,
        hir::Value::Integer(1) => Known::One,
        hir::Value::Integer(-1) => Known::MinusOne,
        hir::Value::Text(t) if t.is_empty() => Known::Empty,
        hir::Value::Approximate(r) => match r.get() {
            0.0 => Known::Zero,
            1.0 => Known::One,
            -1.0 => Known::MinusOne,
            _ => return None,
        },
        _ => return None,
    })
}

/// `op operand`, evaluated if the operand is a constant, and otherwise
/// simplified where that can't change the result.
///
/// # Errors
///
/// Returns why the result can't exist.
pub(crate) fn unary(op: hir::UnaryOp, operand: hir::Expr) -> Result<hir::Expr, hir::Failure> {
    if let Some(value) = known_value(&operand) {
        return op.apply(value).map(hir::Expr::Constant);
    }
    Ok(match (op, operand) {
        (hir::UnaryOp::Not, operand) => operand.negated(),
        // `-(-x)` is `x` for an approximate number, but not for an integer:
        // for the smallest one, `-x` fails.
        (
            hir::UnaryOp::Negate(hir::Numeric::Approximate),
            hir::Expr::Unary(hir::UnaryOp::Negate(hir::Numeric::Approximate), inner),
        ) => *inner,
        (op, operand) => hir::Expr::Unary(op, Box::new(operand)),
    })
}

/// An operand of a dyadic operator.
#[derive(Debug, Clone, Copy)]
enum Side {
    Lhs,
    Rhs,
}

/// `lhs op rhs`, evaluated if both are constants, and each value an
/// order compares in another possible representation than its type's
/// stored one is known in it, and otherwise simplified where a constant
/// operand decides the result or leaves the other operand as it is.
/// Expressions have no side effects, so this never changes what a program
/// does. Approximate numbers are finite, with one zero, so these hold for
/// them as for integers: `x * 0.0` is `0.0`, whatever `x` is.
///
/// # Errors
///
/// Returns why the result can't exist, including a division by a constant
/// zero, which fails whatever the dividend is, or why converting a value
/// to the representation its type's order compares fails.
pub(crate) fn binary(
    op: hir::BinaryOp,
    lhs: hir::Expr,
    rhs: hir::Expr,
    bindings: Bindings,
) -> Result<hir::Expr, Failed> {
    if let (Some(a), Some(b)) = (known_value(&lhs), known_value(&rhs)) {
        let compared = match op {
            hir::BinaryOp::Compare(hir::Comparison::Equal | hir::Comparison::NotEqual, _) => {
                Vec::new()
            }
            hir::BinaryOp::Compare(..) => vec![a, b],
            _ => Vec::new(),
        };
        if let Some(converted) = converted(compared, bindings)? {
            return op
                .apply(a, b, &converted)
                .map(hir::Expr::Constant)
                .map_err(Failed::Scalar);
        }
        return Ok(hir::Expr::Binary(op, Box::new(lhs), Box::new(rhs)));
    }
    simplified(op, lhs, rhs).map_err(Failed::Scalar)
}

/// `lhs op rhs`, of which one at most is a constant, simplified as
/// [`binary`] says.
///
/// # Errors
///
/// Returns why the result can't exist: a division by a constant zero.
fn simplified(
    op: hir::BinaryOp,
    lhs: hir::Expr,
    rhs: hir::Expr,
) -> Result<hir::Expr, hir::Failure> {
    use Known::{Empty, MinusOne, One, Zero};
    use hir::BinaryOp::{Add, Compare, Concatenate, Div, Divide, Mod, Multiply, Subtract};
    let negate = |e| {
        let numeric = op
            .operand()
            .and_then(hir::Numeric::of)
            .expect("only arithmetic on constants negates");
        unary(hir::UnaryOp::Negate(numeric), e)
    };
    // `result`, which drops one operand, unless that one can fail.
    let unless_fails = |drops: Side, result: hir::Expr, lhs: hir::Expr, rhs: hir::Expr| {
        let dropped = match drops {
            Side::Lhs => &lhs,
            Side::Rhs => &rhs,
        };
        if dropped.can_fail() {
            hir::Expr::Binary(op, Box::new(lhs), Box::new(rhs))
        } else {
            result
        }
    };
    let constant = |value| hir::Expr::Constant(value);
    Ok(match (op, known(&lhs), known(&rhs)) {
        // A comparison is in normal form, in which a constant boolean
        // leaves the other operand as it is, or negates it.
        (Compare(comparison, compared), _, _) => {
            hir::Expr::compared(comparison, compared, lhs, rhs)
        }
        // The constant leaves the other operand as it is,
        (Add(_), Some(Zero), _) | (Multiply(_), Some(One), _) | (Concatenate, Some(Empty), _) => {
            rhs
        }
        (Add(_) | Subtract(_), _, Some(Zero))
        | (Multiply(_) | Div | Divide, _, Some(One))
        | (Concatenate, _, Some(Empty)) => lhs,
        // negates it,
        (Subtract(_), Some(Zero), _) | (Multiply(_), Some(MinusOne), _) => negate(rhs)?,
        (Multiply(_) | Div | Divide, _, Some(MinusOne)) => negate(lhs)?,
        // or decides the result.
        (Div | Mod | Divide, _, Some(Zero)) => {
            return Err(hir::Failure::DivisionByZero);
        }
        // though every operand is still computed.
        (Multiply(_), Some(Zero), _) => unless_fails(Side::Rhs, lhs.clone(), lhs, rhs),
        (Multiply(_), _, Some(Zero)) => unless_fails(Side::Lhs, rhs.clone(), lhs, rhs),
        (Mod, _, Some(One | MinusOne)) => {
            unless_fails(Side::Lhs, constant(hir::Value::Integer(0)), lhs, rhs)
        }
        _ => hir::Expr::Binary(op, Box::new(lhs), Box::new(rhs)),
    })
}
