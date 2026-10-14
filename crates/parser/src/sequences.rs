//! Lists in brackets where braces go: after `extend`, `replace` and
//! `group`. Braces enclose what can be in any order and brackets what
//! can't, and the items of one of these apply all at once, so their order
//! doesn't matter. Brackets would ask for them in order, which is what
//! chaining the operator does: `r group [x := {a}, y := {b}]` is
//! `r group { x := {a} } group { y := {b} }`, not the same as
//! `r group { x := {a}, y := {b} }`. So the fix is the chain, which keeps
//! the order asked for, rather than braces, which would drop it.

use trendy_diag::Diagnostic;
use trendy_syntax::ast::{self, AstNode};
use trendy_syntax::{SyntaxKind, SyntaxNode};

use crate::errors;

/// An error for each list of an `extend`, a `replace` or a `group` in the
/// tree at `root` that is in brackets.
pub(crate) fn bracketed_lists(root: &SyntaxNode) -> Vec<Diagnostic> {
    root.descendants()
        .filter_map(ast::PostfixExpr::cast)
        .filter_map(|postfix| {
            let keyword = postfix.operator()?;
            let list = postfix
                .syntax()
                .children()
                .find(|n| matches!(n.kind(), SyntaxKind::AssignList | SyntaxKind::GatherList))?;
            let open = list
                .first_token()
                .filter(|t| t.kind() == SyntaxKind::LBracket)?;
            let items: Vec<String> = list
                .children()
                .filter(|n| matches!(n.kind(), SyntaxKind::Assign | SyntaxKind::Gathering))
                .map(|n| n.text().to_string())
                .collect();
            let operator = keyword.kind().fixed_text()?;
            Some(errors::bracketed_list(
                operator,
                trendy_syntax::span(open.text_range()),
                trendy_syntax::span(list.text_range()),
                &items,
            ))
        })
        .collect()
}
