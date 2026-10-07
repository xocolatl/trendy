//! The canonical source formatter.
//!
//! The formatter reprints the CST's tokens in order and decides only the
//! whitespace between them. It never adds, drops, or reorders any other
//! token, so it cannot change what a program means, and comments will
//! survive formatting once the language has them. This crate defines the
//! language's layout: whatever it prints is the style.

use trendy_syntax::SyntaxKind;
use trendy_syntax::ast::{AstNode, SourceFile};

/// Render a file in canonical style.
///
/// Callers should only format trees without syntax errors: an erroneous
/// tree keeps all its tokens, but their layout is not meaningful. The
/// output ends with exactly one newline; an empty file formats as "".
#[must_use]
pub fn format(file: &SourceFile) -> String {
    let mut out = String::new();
    let mut prev = None;
    let tokens = file
        .syntax()
        .descendants_with_tokens()
        .filter_map(trendy_syntax::SyntaxElement::into_token)
        .filter(|t| t.kind() != SyntaxKind::Whitespace);

    for token in tokens {
        if let Some(prev) = prev {
            out.push_str(separator(prev, token.kind()));
        }
        out.push_str(token.text());
        prev = Some(token.kind());
    }
    if prev.is_some() {
        out.push('\n');
    }
    out
}

/// The whitespace that goes between two adjacent tokens.
fn separator(prev: SyntaxKind, next: SyntaxKind) -> &'static str {
    use SyntaxKind::{LBracket, OperatorKw, RBrace, Semicolon};
    match (prev, next) {
        // Each operator definition ends with `;`; leave a blank line after.
        (Semicolon, _) => "\n\n",
        (OperatorKw, _) | (RBrace, LBracket) => " ",
        _ => "",
    }
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
    fn empty_file() {
        assert_eq!(fmt("  \n"), "");
    }

    #[test]
    fn idempotent_and_keeps_every_token() {
        for source in [
            "operator main{} [];",
            "operator a { } [ ] ; operator main{}[];",
            "\n\n  operator   x{}\n\n\n[];\n\n",
        ] {
            let once = fmt(source);
            assert_eq!(fmt(&once), once, "not idempotent for {source:?}");
            assert_eq!(significant_tokens(&once), significant_tokens(source));
        }
    }
}
