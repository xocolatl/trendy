//! Operators whose precedence relative to each other is left undefined, so
//! that writing one directly after the other is an error rather than a
//! guess. Precedence is a partial order: these pairs could each be read
//! either way, so the reader must say which with parentheses.
//!
//! One is `!!`. Its operand is a relation, and a postfix operator, a join or
//! a set operator after it could apply to that relation as well as to the
//! image: `!!r rename {a as b}` and `!!r join s` both read two ways. The
//! parser builds them as applying to the image, and this check reports
//! them.
//!
//! The other is `union` with a `minus` after it: `r union s minus t` is
//! `r union (s minus t)` in the algebra, where `minus` is an AND and binds
//! like the joins, but `(r union s) minus t` in SQL, which reads set
//! operators from the left.

use trendy_diag::Diagnostic;
use trendy_syntax::ast::{self, AstNode};
use trendy_syntax::{SyntaxElement, SyntaxKind, SyntaxNode, SyntaxToken};

use crate::errors;

/// An error for each `!!` in the tree at `root` that a postfix operator, a
/// join or a set operator follows, and for each `union` whose right operand
/// is a `minus`.
pub(crate) fn unordered_operators(root: &SyntaxNode) -> Vec<Diagnostic> {
    let mut errors = images(root);
    errors.extend(root.descendants().filter_map(union_minus));
    errors
}

/// `node`, if it is `r union s minus t`, parsed as `r union (s minus t)`:
/// the error, with a fix for each reading.
fn union_minus(node: SyntaxNode) -> Option<Diagnostic> {
    let union = ast::BinaryExpr::cast(node)?;
    let operator = union.operator()?;
    if operator.kind() != SyntaxKind::UnionKw {
        return None;
    }
    let (lhs, ast::Expr::Binary(minus)) = (union.lhs()?, union.rhs()?) else {
        return None;
    };
    let minus_token = minus.operator()?;
    if minus_token.kind() != SyntaxKind::MinusKw {
        return None;
    }
    Some(errors::union_minus(
        trendy_syntax::span(minus_token.text_range()),
        lhs.span().start,
        minus.lhs()?.span().end,
        minus.span(),
    ))
}

/// An error for each `!!` in the tree at `root` that a postfix operator, a
/// join or a set operator follows.
fn images(root: &SyntaxNode) -> Vec<Diagnostic> {
    root.descendants()
        .filter_map(ast::ImageExpr::cast)
        .filter_map(|image| {
            let operand = image.operand()?;
            // The chain of postfix operators and joins that follows: the
            // image could be taken before any of them, or after all.
            let mut chain = Vec::new();
            let mut node = image.syntax().clone();
            while let Some((operator, name)) = following(&node) {
                node = node.parent()?;
                let end = trendy_syntax::span(node.text_range()).end;
                chain.push((operator, name, end));
            }
            let (first, name, _) = chain.first()?;
            let steps: Vec<(&str, usize)> = chain
                .iter()
                .map(|(_, name, end)| (name.as_str(), *end))
                .collect();
            Some(errors::unordered_image(
                name,
                trendy_syntax::span(first.text_range()),
                image.span(),
                operand.span().start,
                &steps,
            ))
        })
        .collect()
}

/// The operator applied directly to `node`, a postfix operator or a join of
/// which it is the left operand, and how it is named in an error.
fn following(node: &SyntaxNode) -> Option<(SyntaxToken, String)> {
    let parent = node.parent()?;
    if !applies_to(&parent, node) {
        return None;
    }
    match parent.kind() {
        SyntaxKind::ExtractExpr => {
            let dot = parent
                .children_with_tokens()
                .filter_map(SyntaxElement::into_token)
                .find(|t| t.kind() == SyntaxKind::Dot)?;
            Some((dot, "attribute extraction, `.`,".to_string()))
        }
        SyntaxKind::ProjectExpr => {
            let names = ast::ProjectExpr::cast(parent)?.names()?;
            let brace = names.syntax().first_token()?;
            Some((brace, "a projection".to_string()))
        }
        SyntaxKind::PostfixExpr => {
            let operator = ast::PostfixExpr::cast(parent)?.operator()?;
            let name = format!("`{}`", operator.text());
            Some((operator, name))
        }
        SyntaxKind::BinaryExpr => {
            let operator = ast::BinaryExpr::cast(parent)?.operator()?;
            let name = format!("`{}`", operator.text());
            Some((operator, name))
        }
        _ => None,
    }
}

/// Whether `parent` is a postfix operator applied to `child`, or a join
/// whose left operand is `child`.
fn applies_to(parent: &SyntaxNode, child: &SyntaxNode) -> bool {
    let first = |node: &SyntaxNode| node.children().find_map(ast::Expr::cast);
    let is_first = first(parent).is_some_and(|e| e.syntax() == child);
    match parent.kind() {
        SyntaxKind::ExtractExpr | SyntaxKind::ProjectExpr | SyntaxKind::PostfixExpr => is_first,
        SyntaxKind::BinaryExpr => {
            // The last word, for `not matching`.
            is_first
                && ast::BinaryExpr::cast(parent.clone())
                    .and_then(|b| b.operators().last())
                    .is_some_and(|op| op.kind().is_join() || op.kind().is_set_operator())
        }
        _ => false,
    }
}
