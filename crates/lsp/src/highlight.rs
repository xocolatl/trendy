//! Semantic highlighting: what each token is, according to the CST.
//!
//! Classification uses the same tree the compiler parses, so highlighting
//! can't disagree with the compiler. It works on trees with syntax errors
//! too. Tokens with nothing to say (punctuation, whitespace, errors) are left
//! to the editor's grammar.

use trendy_diag::Span;
use trendy_syntax::ast::{AstNode, SourceFile};
use trendy_syntax::{SyntaxKind, SyntaxToken};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Class {
    Keyword,
    /// An operator's name.
    Operator,
    /// A variable's name.
    Variable,
    Type,
    Number,
    Text,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Highlight {
    pub span: Span,
    pub class: Class,
    /// Whether this is where the thing is defined, rather than a use of it.
    pub definition: bool,
}

/// Highlights for every classified token, in source order.
pub fn highlights(file: &SourceFile) -> Vec<Highlight> {
    file.syntax()
        .descendants_with_tokens()
        .filter_map(trendy_syntax::SyntaxElement::into_token)
        .filter_map(|token| classify(&token))
        .collect()
}

fn classify(token: &SyntaxToken) -> Option<Highlight> {
    let parent = token.parent().map(|p| p.kind());
    let grandparent = token.parent().and_then(|p| p.parent()).map(|p| p.kind());
    let (class, definition) = match (token.kind(), parent, grandparent) {
        (kind, _, _) if kind.is_keyword() => (Class::Keyword, false),
        (SyntaxKind::Ident, Some(SyntaxKind::Name), Some(SyntaxKind::Operator)) => {
            (Class::Operator, true)
        }
        (SyntaxKind::Ident, Some(SyntaxKind::Name), Some(SyntaxKind::VarDecl)) => {
            (Class::Variable, true)
        }
        (SyntaxKind::Ident, Some(SyntaxKind::TypeRef), _) => (Class::Type, false),
        (SyntaxKind::IntLiteral | SyntaxKind::RationalLiteral, _, _) => (Class::Number, false),
        (SyntaxKind::TextLiteral, _, _) => (Class::Text, false),
        _ => return None,
    };
    Some(Highlight {
        span: trendy_syntax::span(token.text_range()),
        class,
        definition,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn highlight(source: &str) -> Vec<(&str, Class, bool)> {
        let parse = trendy_parser::parse(source, trendy_lexer::lex(source));
        highlights(&parse.tree)
            .into_iter()
            .map(|h| (&source[h.span.start..h.span.end], h.class, h.definition))
            .collect()
    }

    #[test]
    fn definition() {
        assert_eq!(
            highlight("operator main{} [];"),
            [
                ("operator", Class::Keyword, false),
                ("main", Class::Operator, true),
            ]
        );
    }

    #[test]
    fn variables() {
        assert_eq!(
            highlight("var t: text := \"x\";\nvar i := 0;\nvar r := 1e5;"),
            [
                ("var", Class::Keyword, false),
                ("t", Class::Variable, true),
                ("text", Class::Type, false),
                ("\"x\"", Class::Text, false),
                ("var", Class::Keyword, false),
                ("i", Class::Variable, true),
                ("0", Class::Number, false),
                ("var", Class::Keyword, false),
                ("r", Class::Variable, true),
                ("1e5", Class::Number, false),
            ]
        );
    }

    #[test]
    fn works_on_broken_files() {
        assert_eq!(
            highlight("junk operator a{ operator b"),
            [
                ("operator", Class::Keyword, false),
                ("a", Class::Operator, true),
                ("operator", Class::Keyword, false),
                ("b", Class::Operator, true),
            ]
        );
    }
}
