//! Operators in expressions: what each token denotes, and evaluating and
//! simplifying what can be known when compiling.

use trendy_diag::Diagnostic;
use trendy_hir as hir;
use trendy_syntax::ast::{self, Expr, Literal, LiteralError};
use trendy_syntax::{SyntaxKind, SyntaxToken, literal};

use crate::errors;

/// What an operator token denotes. Sema sees only trees without syntax
/// errors, so foreign spellings such as `&&` never reach it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Operator {
    /// Prefix `+`, which leaves its integer operand as it is.
    Plus,
    Unary(hir::UnaryOp),
    Binary(hir::BinaryOp),
}

impl Operator {
    pub(crate) fn of(token: &SyntaxToken) -> Option<Operator> {
        let prefix = token
            .parent()
            .is_some_and(|p| p.kind() == SyntaxKind::UnaryExpr);
        Some(match (token.kind(), prefix) {
            (SyntaxKind::NotKw, _) => Operator::Unary(hir::UnaryOp::Not),
            (SyntaxKind::Minus, true) => Operator::Unary(hir::UnaryOp::Negate),
            (SyntaxKind::Plus, true) => Operator::Plus,
            (SyntaxKind::AndKw, _) => Operator::Binary(hir::BinaryOp::And),
            (SyntaxKind::OrKw, _) => Operator::Binary(hir::BinaryOp::Or),
            (SyntaxKind::XorKw, _) => Operator::Binary(hir::BinaryOp::Xor),
            (SyntaxKind::ImpliesKw, _) => Operator::Binary(hir::BinaryOp::Implies),
            (SyntaxKind::Plus, false) => Operator::Binary(hir::BinaryOp::Add),
            (SyntaxKind::Minus, false) => Operator::Binary(hir::BinaryOp::Subtract),
            (SyntaxKind::Star, _) => Operator::Binary(hir::BinaryOp::Multiply),
            (SyntaxKind::DivKw, _) => Operator::Binary(hir::BinaryOp::Div),
            (SyntaxKind::ModKw, _) => Operator::Binary(hir::BinaryOp::Mod),
            _ => return None,
        })
    }

    /// The type of each operand.
    pub(crate) fn operand(self) -> hir::Scalar {
        match self {
            Operator::Plus => hir::Scalar::Integer,
            Operator::Unary(op) => op.operand(),
            Operator::Binary(op) => op.operand(),
        }
    }

    /// The type of the result.
    pub(crate) fn result(self) -> hir::Scalar {
        match self {
            Operator::Plus => hir::Scalar::Integer,
            Operator::Unary(op) => op.result(),
            Operator::Binary(op) => op.result(),
        }
    }

    pub(crate) fn is_prefix(self) -> bool {
        !matches!(self, Operator::Binary(_))
    }
}

/// The number literal `unary` negates directly, as in `-1` or `-0.5`. Such a
/// literal is a value of its own, so that the smallest integer can be
/// written, and negative rationals.
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
        hir::Expr::Unary(hir::UnaryOp::Not, operand) => can_fail(operand),
        hir::Expr::Unary(hir::UnaryOp::Negate, _) => true,
        hir::Expr::Binary(op, lhs, rhs) => {
            op.operand() == hir::Scalar::Integer || can_fail(lhs) || can_fail(rhs)
        }
    }
}

fn constant(expr: &hir::Expr) -> Option<&hir::Value> {
    match expr {
        hir::Expr::Constant(value) => Some(value),
        _ => None,
    }
}

fn boolean(b: bool) -> hir::Expr {
    hir::Expr::Constant(hir::Value::Boolean(b))
}

fn integer(n: i64) -> hir::Expr {
    hir::Expr::Constant(hir::Value::Integer(n))
}

/// `op operand`, evaluated if the operand is a constant, and otherwise
/// simplified where that can't change the result.
///
/// # Errors
///
/// Returns why the result can't exist.
pub(crate) fn unary(op: hir::UnaryOp, operand: hir::Expr) -> Result<hir::Expr, hir::Failure> {
    if let Some(value) = constant(&operand) {
        return op.apply(value).map(hir::Expr::Constant);
    }
    Ok(match (op, operand) {
        (hir::UnaryOp::Not, hir::Expr::Unary(hir::UnaryOp::Not, inner)) => *inner,
        // `-(-x)` is not `x`: for the smallest integer, `-x` fails.
        (op, operand) => hir::Expr::Unary(op, Box::new(operand)),
    })
}

/// `lhs op rhs`, evaluated if both are constants, and otherwise simplified
/// where a constant operand decides the result or leaves the other operand
/// as it is. Expressions have no side effects, so this never changes what a
/// program does.
///
/// # Errors
///
/// Returns why the result can't exist, including a division by the constant
/// zero, which fails whatever the dividend is.
pub(crate) fn binary(
    op: hir::BinaryOp,
    lhs: hir::Expr,
    rhs: hir::Expr,
) -> Result<hir::Expr, hir::Failure> {
    use hir::BinaryOp::{Add, And, Div, Implies, Mod, Multiply, Or, Subtract, Xor};
    use hir::Value::{Boolean, Integer};
    if let (Some(a), Some(b)) = (constant(&lhs), constant(&rhs)) {
        return op.apply(a, b).map(hir::Expr::Constant);
    }
    let negate = |e| hir::Expr::Unary(hir::UnaryOp::Negate, Box::new(e));
    // `result`, which drops `dropped`, unless `dropped` can fail.
    let unless_fails = |dropped: &hir::Expr, result: hir::Expr, otherwise: hir::Expr| {
        if can_fail(dropped) { otherwise } else { result }
    };
    let both = |lhs, rhs| hir::Expr::Binary(op, Box::new(lhs), Box::new(rhs));
    Ok(
        match (op, constant(&lhs).cloned(), constant(&rhs).cloned()) {
            // The constant leaves the other operand as it is,
            (And | Implies, Some(Boolean(true)), None)
            | (Or | Xor, Some(Boolean(false)), None)
            | (Add, Some(Integer(0)), None)
            | (Multiply, Some(Integer(1)), None) => rhs,
            (And, None, Some(Boolean(true)))
            | (Or | Xor, None, Some(Boolean(false)))
            | (Add | Subtract, None, Some(Integer(0)))
            | (Multiply | Div, None, Some(Integer(1))) => lhs,
            // negates it,
            (Xor, Some(Boolean(true)), None) => unary(hir::UnaryOp::Not, rhs)?,
            (Xor, None, Some(Boolean(true))) | (Implies, None, Some(Boolean(false))) => {
                unary(hir::UnaryOp::Not, lhs)?
            }
            (Subtract, Some(Integer(0)), None) | (Multiply, Some(Integer(-1)), None) => negate(rhs),
            (Multiply | Div, None, Some(Integer(-1))) => negate(lhs),
            // or decides the result.
            (Div | Mod, None, Some(Integer(0))) => return Err(hir::Failure::DivisionByZero),
            (And, Some(Boolean(false)), None) | (Multiply, Some(Integer(0)), None) => {
                unless_fails(&rhs, lhs.clone(), both(lhs.clone(), rhs.clone()))
            }
            (And, None, Some(Boolean(false)))
            | (Multiply, None, Some(Integer(0)))
            | (Mod, None, Some(Integer(1 | -1))) => {
                let result = match op {
                    Mod => integer(0),
                    _ => rhs.clone(),
                };
                unless_fails(&lhs, result, both(lhs.clone(), rhs.clone()))
            }
            (Or, Some(Boolean(true)), None) | (Implies, Some(Boolean(false)), None) => {
                unless_fails(&rhs, boolean(true), both(lhs.clone(), rhs.clone()))
            }
            (Or | Implies, None, Some(Boolean(true))) => {
                unless_fails(&lhs, boolean(true), both(lhs.clone(), rhs.clone()))
            }
            _ => both(lhs, rhs),
        },
    )
}
