//! The canonical source formatter.
//!
//! The formatter reprints the CST's tokens in order and decides only the
//! whitespace between them. It never adds, drops, or reorders any other
//! token, so it cannot change what a program means, and comments will
//! survive formatting once the language has them. This crate defines the
//! language's layout: whatever it prints is the style.

use trendy_syntax::ast::{AstNode, SourceFile};
use trendy_syntax::{SyntaxKind, SyntaxToken};

/// Render a file in canonical style.
///
/// Callers should only format trees without syntax errors: an erroneous
/// tree keeps all its tokens, but their layout is not meaningful. The
/// output ends with exactly one newline; an empty file formats as "".
#[must_use]
pub fn format(file: &SourceFile) -> String {
    let mut out = String::new();
    let mut prev: Option<SyntaxToken> = None;
    let tokens = file
        .syntax()
        .descendants_with_tokens()
        .filter_map(trendy_syntax::SyntaxElement::into_token)
        .filter(|t| t.kind() != SyntaxKind::Whitespace);

    for token in tokens {
        if let Some(prev) = &prev {
            out.push_str(separator(prev, &token));
        }
        out.push_str(token.text());
        prev = Some(token);
    }
    if prev.is_some() {
        out.push('\n');
    }
    out
}

/// The whitespace that goes between two adjacent tokens.
fn separator(prev: &SyntaxToken, next: &SyntaxToken) -> &'static str {
    use SyntaxKind::{Colon, ColonEq, LBracket, OperatorKw, RBrace, Semicolon, VarDecl, VarKw};
    if prev.kind() == Semicolon {
        // Between definitions: variable declarations in a row stay together,
        // and everything else is separated by a blank line.
        let both_variables = item_kind(prev) == Some(VarDecl) && item_kind(next) == Some(VarDecl);
        return if both_variables { "\n" } else { "\n\n" };
    }
    match (prev.kind(), next.kind()) {
        (OperatorKw | VarKw | Colon | ColonEq, _) | (_, ColonEq) | (RBrace, LBracket) => " ",
        _ => "",
    }
}

/// The kind of the top-level definition that contains `token`.
fn item_kind(token: &SyntaxToken) -> Option<SyntaxKind> {
    token
        .parent_ancestors()
        .find(|node| {
            node.parent()
                .is_some_and(|p| p.kind() == SyntaxKind::SourceFile)
        })
        .map(|node| node.kind())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fmt(source: &str) -> String {
        let parse = trendy_parser::parse(source, trendy_lexer::lex(source));
        assert!(parse.errors.is_empty(), "{:?}", parse.errors);
        format(&parse.tree)
    }

    /// The source's tokens, excluding whitespace.
    fn significant_tokens(source: &str) -> Vec<String> {
        let lexed = trendy_lexer::lex(source);
        lexed
            .tokens
            .iter()
            .filter(|t| t.kind != SyntaxKind::Whitespace)
            .map(|t| source[t.span.start..t.span.end].to_string())
            .collect()
    }

    #[test]
    fn canonical_is_unchanged() {
        assert_eq!(fmt("operator main{} [];\n"), "operator main{} [];\n");
    }

    #[test]
    fn normalizes_whitespace() {
        assert_eq!(
            fmt("  operator\n\tmain {  }\n[\n] ;"),
            "operator main{} [];\n"
        );
    }

    #[test]
    fn separates_operators_with_a_blank_line() {
        assert_eq!(
            fmt("operator a{}[];operator b{}[];"),
            "operator a{} [];\n\noperator b{} [];\n"
        );
    }

    #[test]
    fn variables() {
        assert_eq!(
            fmt("var   i:integer:=0 ;var t :text;var u:=\"x\";\n\n\noperator main{}[];"),
            "var i: integer := 0;\nvar t: text;\nvar u := \"x\";\n\noperator main{} [];\n"
        );
    }

    #[test]
    fn blank_line_between_variables_and_operators() {
        assert_eq!(
            fmt("operator a{}[];var i:=1;operator b{}[];"),
            "operator a{} [];\n\nvar i := 1;\n\noperator b{} [];\n"
        );
    }

    #[test]
    fn empty_file() {
        assert_eq!(fmt("  \n"), "");
    }

    #[test]
    fn idempotent_and_keeps_every_token() {
        for source in [
            "operator main{} [];",
            "operator a { } [ ] ; operator main{}[];",
            "\n\n  operator   x{}\n\n\n[];\n\n",
            "var a:integer:=1;var b:=\"t\\n\";operator main{}[];",
        ] {
            let once = fmt(source);
            assert_eq!(fmt(&once), once, "not idempotent for {source:?}");
            assert_eq!(significant_tokens(&once), significant_tokens(source));
        }
    }
}
