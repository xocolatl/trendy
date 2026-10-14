//! Operators in expressions: what each token denotes, checking operand
//! types, and evaluating and simplifying what can be known when compiling.

use std::collections::{BTreeMap, BTreeSet};

use trendy_diag::{Diagnostic, Span, edit_distance};
use trendy_hir as hir;
use trendy_syntax::ast::{self, AstNode, Expr, Literal, LiteralError};
use trendy_syntax::{SyntaxKind, SyntaxToken, literal};

use crate::errors;
use crate::types::{Expected, Origin};

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
    Identity(hir::Scalar),
    Unary(hir::UnaryOp),
    Binary(hir::BinaryOp),
}

impl Resolved {
    pub(crate) fn result(self) -> hir::Scalar {
        match self {
            Resolved::Identity(scalar) => scalar,
            Resolved::Unary(op) => op.result(),
            Resolved::Binary(op) => op.result(),
        }
    }
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
            Operator::Compare(_) => "booleans, integers, approximate numbers or text",
        }
    }

    /// The operator for operands of type `ty`, or `None` if it doesn't take
    /// them: `/` doesn't take integers, nor `div` and `mod` approximate
    /// numbers, and tuples and relations only have `=` and `<>`.
    pub(crate) fn resolve(self, ty: &hir::Type) -> Option<Resolved> {
        use hir::BinaryOp as B;
        let hir::Type::Scalar(scalar) = *ty else {
            return match self {
                Operator::Compare(
                    comparison @ (hir::Comparison::Equal | hir::Comparison::NotEqual),
                ) => Some(Resolved::Binary(B::Compare(
                    comparison,
                    hir::Compared::of(ty),
                ))),
                _ => None,
            };
        };
        let numeric = hir::Numeric::of(scalar);
        Some(match (self, numeric) {
            (Operator::Not, _) => Resolved::Unary(hir::UnaryOp::Not),
            (Operator::And, _) => Resolved::Binary(B::And),
            (Operator::Or, _) => Resolved::Binary(B::Or),
            (Operator::Xor, _) => Resolved::Binary(B::Xor),
            (Operator::Implies, _) => Resolved::Binary(B::Implies),
            (Operator::Plus, Some(_)) => Resolved::Identity(scalar),
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
pub(crate) type CheckOperand<'a, T> =
    dyn FnMut(&Expr, Option<&Expected>, &mut Vec<Diagnostic>) -> Option<(T, hir::Type)> + 'a;

/// Check the operands of `op`, written `token`, in an expression whose
/// value is `expected` to have a type. `check` checks one operand against
/// an expectation, giving its checked form and its type.
///
/// Returns the resolved operator and the checked operands, or `None` after
/// reporting why there are none.
pub(crate) fn check_operands<T>(
    op: Operator,
    token: &SyntaxToken,
    operands: Vec<Option<Expr>>,
    expected: Option<&Expected>,
    check: &mut CheckOperand<'_, T>,
    errors: &mut Vec<Diagnostic>,
) -> Option<(Resolved, Vec<T>)> {
    // A context the operator can work in sets the operand type.
    let context = expected.filter(|e| op.result_is_operand() && op.resolve(&e.ty).is_some());
    let mut expectation = match op.operands() {
        Operands::Fixed(scalar) => Some(Expected {
            ty: hir::Type::Scalar(scalar),
            origin: Origin::Operator(token.clone()),
        }),
        Operands::Inferred => context.cloned(),
    };
    let mut checked = Vec::new();
    let mut ok = true;
    // Whether the operator was reported as not taking the operands' type.
    let mut wrong_type = false;
    for operand in operands {
        let Some(operand) = operand else {
            ok = false;
            continue;
        };
        let before = errors.len();
        let result = check(&operand, expectation.as_ref(), errors);
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
        let Some((value, ty)) = result else {
            ok = false;
            continue;
        };
        if expectation.is_none() && ok {
            // The first operand sets the type for the rest, if the operator
            // works on it.
            // An operator that doesn't take this type is reported once; the
            // other operands are still checked against it, so that an empty
            // relation gets its heading and a mismatch is still reported.
            if op.resolve(&ty).is_none() {
                errors.push(errors::wrong_operand_type(op, token, &ty));
                ok = false;
                wrong_type = true;
            }
            expectation = Some(Expected {
                ty,
                origin: Origin::Value(operand),
            });
            if !ok {
                continue;
            }
        }
        checked.push(value);
    }
    let Expected { ty, .. } = expectation?;
    if !ok {
        return None;
    }
    let Some(resolved) = op.resolve(&ty) else {
        errors.push(errors::wrong_operand_type(op, token, &ty));
        return None;
    };
    Some((resolved, checked))
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
            let rename = heading
                .attributes()
                .map(|(a, _)| a)
                .filter(|a| !named.iter().any(|(_, n, _)| n == a))
                .filter(|a| edit_distance(a, text) <= 2)
                .min_by_key(|a| edit_distance(a, text));
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
                let rename = heading
                    .attributes()
                    .map(|(a, _)| a)
                    .filter(|a| !entries.iter().any(|(_, (b, _), _)| b == a))
                    .filter(|a| edit_distance(a, before) <= 2)
                    .min_by_key(|a| edit_distance(a, before));
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
}

impl JoinKind {
    fn of(token: &SyntaxToken) -> Option<JoinKind> {
        Some(match token.kind() {
            SyntaxKind::JoinKw => JoinKind::Join,
            SyntaxKind::TimesKw => JoinKind::Times,
            SyntaxKind::ComposeKw => JoinKind::Compose,
            _ => return None,
        })
    }
}

/// A join as written: dyadic, `r join s`, or of a list, `join{r, s, t}` or
/// `compose[r, s, t]`.
pub(crate) struct Joining {
    pub(crate) kind: JoinKind,
    pub(crate) token: SyntaxToken,
    pub(crate) operands: Vec<Expr>,
    /// The list form, if it's written that way.
    nary: Option<ast::NaryExpr>,
}

/// How a checked join combines its operands.
pub(crate) enum Combined {
    /// The AND of all of them, in any order: `join` and `times`.
    And,
    /// Compose, from the left: the first operand, then for each other the
    /// AND of the result so far and it, without the attributes they have in
    /// common, given here for each step.
    Composed(Vec<BTreeSet<String>>),
}

impl Joining {
    /// The join `expr` is, if it is one.
    pub(crate) fn of(expr: &Expr) -> Option<Joining> {
        let (token, operands, nary) = match expr {
            Expr::Binary(binary) => (binary.operator()?, vec![binary.lhs()?, binary.rhs()?], None),
            Expr::Nary(nary) => (
                nary.operator()?,
                nary.operands().collect(),
                Some(nary.clone()),
            ),
            _ => return None,
        };
        Some(Joining {
            kind: JoinKind::of(&token)?,
            token,
            operands,
            nary,
        })
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
        if self.kind == JoinKind::Compose
            && self.operands.is_empty()
            && let Some(nary) = &self.nary
        {
            errors.push(errors::empty_compose(nary));
            return None;
        }
        // Relations join with relations, and tuples with tuples; with
        // both, the tuples are the odd ones out.
        let relations = types.iter().any(|ty| matches!(ty, hir::Type::Relation(_)));
        let mut operands = Vec::new();
        for (operand, ty) in self.operands.iter().zip(types) {
            match ty {
                hir::Type::Relation(heading) => operands.push((operand.span(), heading)),
                hir::Type::Tuple(heading) if !relations => {
                    operands.push((operand.span(), heading));
                }
                hir::Type::Tuple(_) => {
                    errors.push(errors::tuple_among_relations(&self.token, operand));
                }
                hir::Type::Scalar(_) => errors.push(errors::not_joinable(&self.token, ty, operand)),
            }
        }
        if operands.len() < self.operands.len() {
            return None;
        }
        if self.kind == JoinKind::Compose {
            return self.composed(&operands, errors).map(Combined::Composed);
        }
        let mut ok = true;
        // Each attribute name, with the first operand that has it.
        let mut first: BTreeMap<&str, (Span, &hir::Type)> = BTreeMap::new();
        let mut shared = BTreeSet::new();
        for &(span, heading) in &operands {
            for (name, ty) in heading.attributes() {
                let Some(&(earlier, earlier_ty)) = first.get(name) else {
                    first.insert(name, (span, ty));
                    continue;
                };
                shared.insert(name.to_string());
                if earlier_ty != ty {
                    errors.push(errors::join_type_conflict(
                        &self.token,
                        name,
                        (earlier, earlier_ty),
                        (span, ty),
                    ));
                    ok = false;
                }
            }
        }
        let headings: Vec<&hir::Heading> = operands.iter().map(|&(_, h)| h).collect();
        let groups = groups(&headings);
        let wrong = match self.kind {
            JoinKind::Times => !shared.is_empty(),
            // With a list, `join{}` and `join{r}` have nothing to connect.
            JoinKind::Join | JoinKind::Compose => groups.iter().any(|&g| g != 0),
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
                &self.token,
                &operands,
                &shared,
                &groups,
            ));
            ok = false;
        }
        ok.then_some(Combined::And)
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
                if a != b {
                    errors.push(errors::join_type_conflict(
                        &self.token,
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
                    &self.token,
                    &operands,
                    &common,
                    &[0, 1],
                ));
                ok = false;
            }
            so_far = so_far.and(heading).without(&common);
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
    let (constants, others): (Vec<_>, Vec<_>) = operands
        .into_iter()
        .partition(|e| matches!(e, hir::Expr::Constant(_)));
    let mut constants = constants.into_iter().filter_map(|e| match e {
        hir::Expr::Constant(value) => Some(value),
        _ => None,
    });
    let constant = match constants.next() {
        Some(first) => Some(
            constants
                .try_fold(first, |lhs, rhs| lhs.and(&rhs))
                .map_err(Failed::Mismatch)?,
        ),
        None => None,
    };
    let mut others = others.into_iter();
    let first = match constant {
        Some(value) => hir::Expr::Constant(value),
        None => others
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
    /// An operator whose result is of this type fails.
    Scalar(hir::Failure, hir::Scalar),
    /// Tuples being joined disagree on a common attribute.
    Mismatch(hir::Mismatch),
}

/// A tuple of these attributes, a constant if they all are.
pub(crate) fn tuple(attributes: BTreeMap<String, hir::Expr>) -> hir::Expr {
    let constants: Option<BTreeMap<String, hir::Value>> = attributes
        .iter()
        .map(|(name, e)| match e {
            hir::Expr::Constant(value) => Some((name.clone(), value.clone())),
            _ => None,
        })
        .collect();
    match constants {
        Some(constants) => hir::Expr::Constant(hir::Value::Tuple(hir::TupleValue::new(constants))),
        None => hir::Expr::Tuple(attributes),
    }
}

/// A relation with this heading and these tuples, a constant if they all
/// are; tuples that are equal become one.
pub(crate) fn relation(heading: hir::Heading, tuples: Vec<hir::Expr>) -> hir::Expr {
    let constants: Option<BTreeSet<hir::TupleValue>> = tuples
        .iter()
        .map(|t| match t {
            hir::Expr::Constant(hir::Value::Tuple(t)) => Some(t.clone()),
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
) -> Result<hir::Expr, ExtendFailure> {
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
        types.insert(name.clone(), hir::Type::Relation(only(heading, &set)));
        values.insert(name, remove(image.clone(), others));
    }
    extend(remove(operand, all), hir::Heading::new(types), values)
}

/// `operand`, a tuple or relation with `heading`, wrapped: each wrapping's
/// attributes replaced by a tuple-valued attribute of them, as Appendix A
/// defines WRAP: an extend, then a remove of what was wrapped. A new name
/// can be one it wraps, which the extend then replaces.
pub(crate) fn wrap(
    operand: hir::Expr,
    heading: &hir::Heading,
    wraps: Vec<(String, BTreeSet<String>)>,
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
        types.insert(name.clone(), hir::Type::Tuple(only(heading, &set)));
        values.insert(name, self::tuple(attributes));
        wrapped.extend(set);
    }
    let gone = wrapped
        .difference(&types.keys().cloned().collect())
        .cloned()
        .collect();
    let extended = extend(operand, hir::Heading::new(types), values)
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
    let extended = extend(operand, hir::Heading::new(types), values)
        .unwrap_or_else(|_| unreachable!("unwrapping only extracts, which can't fail"));
    remove(extended, gone)
}

/// `heading` with only the attributes in `names`.
fn only(heading: &hir::Heading, names: &BTreeSet<String>) -> hir::Heading {
    let others = heading
        .attributes()
        .map(|(a, _)| a.to_string())
        .filter(|a| !names.contains(a))
        .collect();
    heading.without(&others)
}

/// What an `extend` or a `replace` does with its attribute assignments.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Assigning {
    /// Adds attributes the operand doesn't have.
    Extend,
    /// Gives attributes the operand has new values of the same types.
    Replace,
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
    let shift = |e: Box<hir::Expr>| Box::new(shifted(*e, cutoff));
    match expr {
        hir::Expr::Current(depth, heading) if depth >= cutoff => {
            hir::Expr::Current(depth + 1, heading)
        }
        hir::Expr::Current(..)
        | hir::Expr::Constant(_)
        | hir::Expr::Variable(_)
        | hir::Expr::Parameter(_) => expr,
        hir::Expr::Unary(op, operand) => hir::Expr::Unary(op, shift(operand)),
        hir::Expr::Binary(op, lhs, rhs) => hir::Expr::Binary(op, shift(lhs), shift(rhs)),
        hir::Expr::Extract(operand, name) => hir::Expr::Extract(shift(operand), name),
        hir::Expr::Tuple(attributes) => hir::Expr::Tuple(
            attributes
                .into_iter()
                .map(|(name, e)| (name, shifted(e, cutoff)))
                .collect(),
        ),
        hir::Expr::Relation(heading, tuples) => hir::Expr::Relation(
            heading,
            tuples.into_iter().map(|t| shifted(t, cutoff)).collect(),
        ),
        hir::Expr::Remove(operand, names) => hir::Expr::Remove(shift(operand), names),
        hir::Expr::Rename(operand, renamings) => hir::Expr::Rename(shift(operand), renamings),
        hir::Expr::And(lhs, rhs) => hir::Expr::And(shift(lhs), shift(rhs)),
        hir::Expr::Ungroup(operand, names) => hir::Expr::Ungroup(shift(operand), names),
        // The values are one `Extend` further in.
        hir::Expr::Extend(operand, computed, attributes) => hir::Expr::Extend(
            shift(operand),
            computed,
            attributes
                .into_iter()
                .map(|(name, e)| (name, shifted(e, cutoff + 1)))
                .collect(),
        ),
    }
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

/// Why an `extend` of constants has no value: computing `attribute` for
/// `tuple` fails.
pub(crate) struct ExtendFailure {
    pub(crate) failed: Failed,
    pub(crate) attribute: String,
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
) -> Result<hir::Expr, ExtendFailure> {
    let folded = match &operand {
        hir::Expr::Constant(hir::Value::Tuple(tuple)) => {
            extended(tuple, &attributes)?.map(hir::Value::Tuple)
        }
        hir::Expr::Constant(hir::Value::Relation(relation)) => {
            let mut tuples = BTreeSet::new();
            let mut all = true;
            for tuple in &relation.tuples {
                match extended(tuple, &attributes)? {
                    Some(tuple) => {
                        tuples.insert(tuple);
                    }
                    None => all = false,
                }
            }
            all.then(|| {
                hir::Value::Relation(hir::RelationValue {
                    heading: relation.heading.and(&computed),
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
) -> Result<Option<hir::TupleValue>, ExtendFailure> {
    let mut computed = BTreeMap::new();
    for (name, value) in attributes {
        match substitute(value.clone(), 0, tuple) {
            Ok(hir::Expr::Constant(value)) => {
                computed.insert(name.clone(), value);
            }
            Ok(_) => return Ok(None),
            Err(failed) => {
                return Err(ExtendFailure {
                    failed,
                    attribute: name.clone(),
                    tuple: tuple.clone(),
                });
            }
        }
    }
    Ok(Some(tuple.with(computed)))
}

/// `expr` with the tuple at hand `depth` `Extend`s out replaced by `tuple`,
/// and simplified as when it was checked, so that what then depends only on
/// constants is evaluated.
///
/// # Errors
///
/// Returns why an operator now known to fail fails, and its type.
fn substitute(expr: hir::Expr, depth: usize, tuple: &hir::TupleValue) -> Result<hir::Expr, Failed> {
    let sub = |e: Box<hir::Expr>| substitute(*e, depth, tuple);
    Ok(match expr {
        hir::Expr::Current(d, _) if d == depth => {
            hir::Expr::Constant(hir::Value::Tuple(tuple.clone()))
        }
        hir::Expr::Current(d, heading) if d > depth => hir::Expr::Current(d - 1, heading),
        hir::Expr::Current(..)
        | hir::Expr::Constant(_)
        | hir::Expr::Variable(_)
        | hir::Expr::Parameter(_) => expr,
        hir::Expr::Unary(op, operand) => {
            unary(op, sub(operand)?).map_err(|f| Failed::Scalar(f, op.result()))?
        }
        hir::Expr::Binary(op, lhs, rhs) => {
            binary(op, sub(lhs)?, sub(rhs)?).map_err(|f| Failed::Scalar(f, op.result()))?
        }
        hir::Expr::Extract(operand, name) => extract(sub(operand)?, name),
        hir::Expr::Tuple(attributes) => tuple_of(attributes, depth, tuple)?,
        hir::Expr::Relation(heading, tuples) => relation(
            heading,
            tuples
                .into_iter()
                .map(|t| substitute(t, depth, tuple))
                .collect::<Result<_, _>>()?,
        ),
        hir::Expr::Remove(operand, names) => remove(sub(operand)?, names),
        hir::Expr::Rename(operand, renamings) => rename(sub(operand)?, renamings),
        hir::Expr::And(lhs, rhs) => and(vec![sub(lhs)?, sub(rhs)?])?,
        hir::Expr::Ungroup(operand, names) => ungroup(sub(operand)?, names),
        hir::Expr::Extend(operand, computed, attributes) => {
            // The values are one `Extend` further in.
            let attributes = attributes
                .into_iter()
                .map(|(name, e)| Ok((name, substitute(e, depth + 1, tuple)?)))
                .collect::<Result<_, _>>()?;
            extend(sub(operand)?, computed, attributes).map_err(|f| f.failed)?
        }
    })
}

/// A tuple selector's `attributes`, substituted as for [`substitute`].
fn tuple_of(
    attributes: BTreeMap<String, hir::Expr>,
    depth: usize,
    tuple: &hir::TupleValue,
) -> Result<hir::Expr, Failed> {
    Ok(self::tuple(
        attributes
            .into_iter()
            .map(|(name, e)| Ok((name, substitute(e, depth, tuple)?)))
            .collect::<Result<_, _>>()?,
    ))
}

/// The attribute `extract`, written after `operand`, which is of type `ty`,
/// takes the value of. `None` after reporting why there is none: the
/// operand isn't a tuple, or hasn't the attribute.
pub(crate) fn extracted(
    extract: &ast::ExtractExpr,
    operand: &Expr,
    ty: &hir::Type,
    errors: &mut Vec<Diagnostic>,
) -> Option<String> {
    let ident = extract.attribute()?.ident()?;
    let name = ident.text().to_string();
    let span = trendy_syntax::span(ident.text_range());
    let hir::Type::Tuple(heading) = ty else {
        errors.push(errors::not_extractable(&name, span, ty, operand));
        return None;
    };
    if heading.get(&name).is_none() {
        let rename = heading
            .attributes()
            .map(|(a, _)| a)
            .filter(|a| edit_distance(a, &name) <= 2)
            .min_by_key(|a| edit_distance(a, &name));
        let context = errors::attributes_note("tuple", ty);
        errors.push(errors::unknown_attribute(
            errors::Noun::Projected,
            &name,
            span,
            &context,
            rename,
            None,
        ));
        return None;
    }
    Some(name)
}

/// The value of `operand`'s attribute `name`: evaluated if the operand is a
/// constant, and taken straight from a selector, a rename or a remove where
/// nothing that can fail is dropped.
pub(crate) fn extract(operand: hir::Expr, name: String) -> hir::Expr {
    match operand {
        hir::Expr::Constant(hir::Value::Tuple(tuple)) => hir::Expr::Constant(
            tuple
                .get(&name)
                .cloned()
                .expect("sema extracts only attributes the tuple has"),
        ),
        hir::Expr::Tuple(mut attributes)
            if attributes
                .iter()
                .all(|(other, e)| *other == name || !can_fail(e)) =>
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
    match operand {
        hir::Expr::Constant(value) => hir::Expr::Constant(value.renamed(&renamings)),
        operand => hir::Expr::Rename(Box::new(operand), renamings),
    }
}

/// `operand` without the attributes `names`, evaluated if it's a constant,
/// and merged with a removal it already is.
pub(crate) fn remove(operand: hir::Expr, names: BTreeSet<String>) -> hir::Expr {
    if names.is_empty() {
        return operand;
    }
    match operand {
        hir::Expr::Constant(value) => hir::Expr::Constant(value.without(&names)),
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

/// Whether evaluating `expr` can fail when the program runs. An operand that
/// can fail is never simplified away, so that every operand is evaluated
/// and its failure never skipped.
fn can_fail(expr: &hir::Expr) -> bool {
    match expr {
        hir::Expr::Constant(_)
        | hir::Expr::Variable(_)
        | hir::Expr::Parameter(_)
        | hir::Expr::Current(..) => false,
        // Negating the smallest integer overflows, and building a relation,
        // removing, renaming or adding attributes, joining or ungrouping it,
        // allocates the result; adding attributes computes them, which can
        // fail too.
        hir::Expr::Unary(hir::UnaryOp::Negate(hir::Numeric::Integer), _)
        | hir::Expr::Remove(..)
        | hir::Expr::Rename(..)
        | hir::Expr::And(..)
        | hir::Expr::Ungroup(..)
        | hir::Expr::Extend(..)
        | hir::Expr::Relation(..) => true,
        hir::Expr::Unary(_, operand) => can_fail(operand),
        hir::Expr::Tuple(attributes) => attributes.values().any(can_fail),
        hir::Expr::Extract(tuple, _) => can_fail(tuple),
        hir::Expr::Binary(op, lhs, rhs) => {
            // Arithmetic and concatenation can fail themselves; the boolean
            // operators and comparisons can't.
            let itself = !matches!(
                op,
                hir::BinaryOp::And
                    | hir::BinaryOp::Or
                    | hir::BinaryOp::Xor
                    | hir::BinaryOp::Implies
                    | hir::BinaryOp::Compare(..)
            );
            itself || can_fail(lhs) || can_fail(rhs)
        }
    }
}

/// A constant that simplification knows the effect of.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Known {
    True,
    False,
    /// `0` or `0.0`.
    Zero,
    /// `-0.0`, which IEEE 754 sign rules make the identity of `+`.
    NegativeZero,
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
        hir::Value::Boolean(true) => Known::True,
        hir::Value::Boolean(false) => Known::False,
        hir::Value::Integer(0) => Known::Zero,
        hir::Value::Integer(1) => Known::One,
        hir::Value::Integer(-1) => Known::MinusOne,
        hir::Value::Text(t) if t.is_empty() => Known::Empty,
        hir::Value::Approximate(r) => match r.get() {
            r if r.to_bits() == 0.0_f64.to_bits() => Known::Zero,
            r if r.to_bits() == (-0.0_f64).to_bits() => Known::NegativeZero,
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
    if let hir::Expr::Constant(value) = &operand {
        return op.apply(value).map(hir::Expr::Constant);
    }
    Ok(match (op, operand) {
        // `-(-x)` is `x` for an approximate number, but not for an integer:
        // for the smallest one, `-x` fails.
        (hir::UnaryOp::Not, hir::Expr::Unary(hir::UnaryOp::Not, inner))
        | (
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

/// `lhs op rhs`, evaluated if both are constants, and otherwise simplified
/// where a constant operand decides the result or leaves the other operand
/// as it is. Expressions have no side effects, so this never changes what a
/// program does. Approximate simplifications follow IEEE 754 exactly, signed
/// zeros included: `x + 0.0` is not `x` when `x` is `-0.0`.
///
/// # Errors
///
/// Returns why the result can't exist, including a division by a constant
/// zero, which fails whatever the dividend is.
pub(crate) fn binary(
    op: hir::BinaryOp,
    lhs: hir::Expr,
    rhs: hir::Expr,
) -> Result<hir::Expr, hir::Failure> {
    use Known::{Empty, False, MinusOne, NegativeZero, One, True, Zero};
    use hir::BinaryOp::{
        Add, And, Compare, Concatenate, Div, Divide, Implies, Mod, Multiply, Or, Subtract, Xor,
    };
    use hir::Comparison::{Equal, NotEqual};
    use hir::Numeric::{Approximate, Integer};
    if let (hir::Expr::Constant(a), hir::Expr::Constant(b)) = (&lhs, &rhs) {
        return op.apply(a, b).map(hir::Expr::Constant);
    }
    let negate = |e| {
        let numeric = hir::Numeric::of(op.operand()).expect("only arithmetic negates");
        unary(hir::UnaryOp::Negate(numeric), e)
    };
    // `result`, which drops one operand, unless that one can fail.
    let unless_fails = |drops: Side, result: hir::Expr, lhs: hir::Expr, rhs: hir::Expr| {
        let dropped = match drops {
            Side::Lhs => &lhs,
            Side::Rhs => &rhs,
        };
        if can_fail(dropped) {
            hir::Expr::Binary(op, Box::new(lhs), Box::new(rhs))
        } else {
            result
        }
    };
    let constant = |value| hir::Expr::Constant(value);
    Ok(match (op, known(&lhs), known(&rhs)) {
        // The constant leaves the other operand as it is,
        (And | Implies | Compare(Equal, _), Some(True), _)
        | (Or | Xor | Compare(NotEqual, _), Some(False), _)
        | (Add(Integer), Some(Zero), _)
        | (Add(Approximate), Some(NegativeZero), _)
        | (Multiply(_), Some(One), _)
        | (Concatenate, Some(Empty), _) => rhs,
        (And | Compare(Equal, _), _, Some(True))
        | (Or | Xor | Compare(NotEqual, _), _, Some(False))
        | (Add(Integer) | Subtract(_), _, Some(Zero))
        | (Add(Approximate), _, Some(NegativeZero))
        | (Multiply(_) | Div | Divide, _, Some(One))
        | (Concatenate, _, Some(Empty)) => lhs,
        // negates it,
        (Xor | Compare(NotEqual, _), Some(True), _) | (Compare(Equal, _), Some(False), _) => {
            unary(hir::UnaryOp::Not, rhs)?
        }
        (Xor | Compare(NotEqual, _), _, Some(True))
        | (Compare(Equal, _) | Implies, _, Some(False)) => unary(hir::UnaryOp::Not, lhs)?,
        (Subtract(Integer), Some(Zero), _)
        | (Subtract(Approximate), Some(NegativeZero), _)
        | (Multiply(_), Some(MinusOne), _) => negate(rhs)?,
        (Multiply(_) | Div | Divide, _, Some(MinusOne)) => negate(lhs)?,
        // or decides the result.
        (Div | Mod | Divide, _, Some(Zero | NegativeZero)) => {
            return Err(hir::Failure::DivisionByZero);
        }
        (And, Some(False), _) | (Multiply(Integer), Some(Zero), _) => {
            unless_fails(Side::Rhs, lhs.clone(), lhs, rhs)
        }
        (And, _, Some(False)) | (Multiply(Integer), _, Some(Zero)) => {
            unless_fails(Side::Lhs, rhs.clone(), lhs, rhs)
        }
        (Mod, _, Some(One | MinusOne)) => {
            unless_fails(Side::Lhs, constant(hir::Value::Integer(0)), lhs, rhs)
        }
        (Or, Some(True), _) | (Implies, Some(False), _) => {
            unless_fails(Side::Rhs, constant(hir::Value::Boolean(true)), lhs, rhs)
        }
        (Or | Implies, _, Some(True)) => {
            unless_fails(Side::Lhs, constant(hir::Value::Boolean(true)), lhs, rhs)
        }
        _ => hir::Expr::Binary(op, Box::new(lhs), Box::new(rhs)),
    })
}
