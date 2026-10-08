//! Operators in expressions: what each token denotes, checking operand
//! types, and evaluating and simplifying what can be known when compiling.

use trendy_diag::Diagnostic;
use trendy_hir as hir;
use trendy_syntax::ast::{self, Expr, Literal, LiteralError};
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
}

/// What an operator's operands must be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Operands {
    /// Always this type.
    Fixed(hir::Scalar),
    /// Integers or rationals, both the same: the type comes from the context
    /// or, failing that, from the first operand.
    Numeric,
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
            _ => return None,
        })
    }

    pub(crate) fn is_prefix(self) -> bool {
        matches!(self, Operator::Not | Operator::Negate | Operator::Plus)
    }

    /// What the operands must be. `/`, `div` and `mod` take their type like
    /// the other arithmetic operators, so that applying one to the wrong
    /// numeric type is reported at the operator, with the one that fits.
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
            | Operator::Mod => Operands::Numeric,
        }
    }

    /// The operator for operands of type `scalar`, or `None` if it doesn't
    /// take them: `/` doesn't take integers, nor `div` and `mod` rationals.
    pub(crate) fn resolve(self, scalar: hir::Scalar) -> Option<Resolved> {
        use hir::BinaryOp as B;
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
            (Operator::Divide, Some(hir::Numeric::Rational)) => Resolved::Binary(B::Divide),
            (Operator::Div, Some(hir::Numeric::Integer)) => Resolved::Binary(B::Div),
            (Operator::Mod, Some(hir::Numeric::Integer)) => Resolved::Binary(B::Mod),
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
    let numeric_context =
        expected.filter(|e| matches!(e.ty, hir::Type::Scalar(s) if hir::Numeric::of(s).is_some()));
    let mut expectation = match op.operands() {
        Operands::Fixed(scalar) => Some(Expected {
            ty: hir::Type::Scalar(scalar),
            origin: Origin::Operator(token.clone()),
        }),
        Operands::Numeric => numeric_context.cloned(),
    };
    let mut checked = Vec::new();
    let mut ok = true;
    for operand in operands {
        let Some(operand) = operand else {
            ok = false;
            continue;
        };
        let Some((value, ty)) = check(&operand, expectation.as_ref(), errors) else {
            ok = false;
            continue;
        };
        if expectation.is_none() {
            // The first operand sets the type for the rest.
            if matches!(ty, hir::Type::Scalar(s) if hir::Numeric::of(s).is_some()) {
                expectation = Some(Expected {
                    ty,
                    origin: Origin::Value(operand),
                });
            } else {
                errors.push(errors::not_numeric(token, &ty, &operand));
                ok = false;
                continue;
            }
        }
        checked.push(value);
    }
    let Some(Expected {
        ty: hir::Type::Scalar(scalar),
        ..
    }) = expectation
    else {
        return None;
    };
    if !ok {
        return None;
    }
    let Some(resolved) = op.resolve(scalar) else {
        errors.push(errors::wrong_operand_type(op, token, scalar));
        return None;
    };
    Some((resolved, checked))
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
                    SyntaxKind::IntLiteral | SyntaxKind::RationalLiteral
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
            Ok(ast::LiteralValue::Rational(r)) => {
                Some(hir::Value::Rational(hir::Rational::new(-r)))
            }
            Err(LiteralError::RationalTooLarge) => {
                errors.push(errors::rational_out_of_range(expr.span()));
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
        hir::Expr::Constant(_) | hir::Expr::Variable(_) | hir::Expr::Parameter(_) => false,
        hir::Expr::Unary(hir::UnaryOp::Negate(hir::Numeric::Integer), _) => true,
        hir::Expr::Unary(_, operand) => can_fail(operand),
        hir::Expr::Binary(op, lhs, rhs) => {
            op.operand() != hir::Scalar::Boolean || can_fail(lhs) || can_fail(rhs)
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
        hir::Value::Rational(r) => match r.get() {
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
        // `-(-x)` is `x` for a rational, but not for an integer: for the
        // smallest one, `-x` fails.
        (hir::UnaryOp::Not, hir::Expr::Unary(hir::UnaryOp::Not, inner))
        | (
            hir::UnaryOp::Negate(hir::Numeric::Rational),
            hir::Expr::Unary(hir::UnaryOp::Negate(hir::Numeric::Rational), inner),
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
/// program does. Rational simplifications follow IEEE 754 exactly, signed
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
    use Known::{False, MinusOne, NegativeZero, One, True, Zero};
    use hir::BinaryOp::{Add, And, Div, Divide, Implies, Mod, Multiply, Or, Subtract, Xor};
    use hir::Numeric::{Integer, Rational};
    if let (hir::Expr::Constant(a), hir::Expr::Constant(b)) = (&lhs, &rhs) {
        return op.apply(a, b).map(hir::Expr::Constant);
    }
    let numeric = hir::Numeric::of(op.operand());
    let negate = |e| {
        let numeric = numeric.expect("only arithmetic negates");
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
        (And | Implies, Some(True), _)
        | (Or | Xor, Some(False), _)
        | (Add(Integer), Some(Zero), _)
        | (Add(Rational), Some(NegativeZero), _)
        | (Multiply(_), Some(One), _) => rhs,
        (And, _, Some(True))
        | (Or | Xor, _, Some(False))
        | (Add(Integer) | Subtract(_), _, Some(Zero))
        | (Add(Rational), _, Some(NegativeZero))
        | (Multiply(_) | Div | Divide, _, Some(One)) => lhs,
        // negates it,
        (Xor, Some(True), _) => unary(hir::UnaryOp::Not, rhs)?,
        (Xor, _, Some(True)) | (Implies, _, Some(False)) => unary(hir::UnaryOp::Not, lhs)?,
        (Subtract(Integer), Some(Zero), _)
        | (Subtract(Rational), Some(NegativeZero), _)
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
