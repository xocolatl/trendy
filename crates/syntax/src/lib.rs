//! The concrete syntax tree (CST).
//!
//! The tree is lossless: it holds every byte of the source, including
//! whitespace and text that failed to parse, so printing it reproduces the
//! input exactly. That is what the formatter and editor tooling build on.
//!
//! The tree is built on `rowan`, and no other crate may use `rowan`
//! directly. The [`ast`] module gives a typed view of the tree for later
//! layers.

pub mod ast;

use std::fmt::Write;

use trendy_diag::Span;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u16)]
pub enum SyntaxKind {
    // Trivia: tokens with no grammatical meaning.
    Whitespace,

    // Tokens.
    Ident,
    OperatorKw,
    LBrace,
    RBrace,
    LBracket,
    RBracket,
    LParen,
    RParen,
    Semicolon,
    /// A character the lexer does not recognize.
    ErrorToken,

    // Nodes.
    SourceFile,
    /// `operator name{params} [body];`
    Operator,
    Name,
    /// `{…}`
    ParamList,
    /// `[…]`
    Body,
    /// Tokens the parser could not fit into the grammar.
    Error,
}

impl SyntaxKind {
    /// Every kind, indexed by its discriminant.
    pub const ALL: [SyntaxKind; 17] = [
        SyntaxKind::Whitespace,
        SyntaxKind::Ident,
        SyntaxKind::OperatorKw,
        SyntaxKind::LBrace,
        SyntaxKind::RBrace,
        SyntaxKind::LBracket,
        SyntaxKind::RBracket,
        SyntaxKind::LParen,
        SyntaxKind::RParen,
        SyntaxKind::Semicolon,
        SyntaxKind::ErrorToken,
        SyntaxKind::SourceFile,
        SyntaxKind::Operator,
        SyntaxKind::Name,
        SyntaxKind::ParamList,
        SyntaxKind::Body,
        SyntaxKind::Error,
    ];

    #[must_use]
    pub fn is_trivia(self) -> bool {
        self == SyntaxKind::Whitespace
    }

    #[must_use]
    pub fn is_keyword(self) -> bool {
        matches!(self, SyntaxKind::OperatorKw)
    }

    /// The keyword spelled `text`, if it is one.
    #[must_use]
    pub fn keyword(text: &str) -> Option<SyntaxKind> {
        SyntaxKind::ALL
            .into_iter()
            .find(|k| k.is_keyword() && k.fixed_text() == Some(text))
    }

    /// Whether this is a punctuation token: fixed text that isn't a keyword.
    #[must_use]
    pub fn is_punctuation(self) -> bool {
        !self.is_keyword() && self.fixed_text().is_some()
    }

    /// The exact text of a token kind that is always spelled the same way.
    #[must_use]
    pub fn fixed_text(self) -> Option<&'static str> {
        Some(match self {
            SyntaxKind::OperatorKw => "operator",
            SyntaxKind::LBrace => "{",
            SyntaxKind::RBrace => "}",
            SyntaxKind::LBracket => "[",
            SyntaxKind::RBracket => "]",
            SyntaxKind::LParen => "(",
            SyntaxKind::RParen => ")",
            SyntaxKind::Semicolon => ";",
            _ => return None,
        })
    }

    /// How the kind is named in error messages.
    #[must_use]
    pub fn describe(self) -> &'static str {
        match self {
            SyntaxKind::Whitespace => "whitespace",
            SyntaxKind::Ident => "identifier",
            SyntaxKind::OperatorKw => "`operator`",
            SyntaxKind::LBrace => "`{`",
            SyntaxKind::RBrace => "`}`",
            SyntaxKind::LBracket => "`[`",
            SyntaxKind::RBracket => "`]`",
            SyntaxKind::LParen => "`(`",
            SyntaxKind::RParen => "`)`",
            SyntaxKind::Semicolon => "`;`",
            SyntaxKind::ErrorToken => "unrecognized character",
            SyntaxKind::SourceFile => "source file",
            SyntaxKind::Operator => "operator definition",
            SyntaxKind::Name => "name",
            SyntaxKind::ParamList => "parameter list",
            SyntaxKind::Body => "body",
            SyntaxKind::Error => "error",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Trendy {}

impl rowan::Language for Trendy {
    type Kind = SyntaxKind;

    fn kind_from_raw(raw: rowan::SyntaxKind) -> SyntaxKind {
        SyntaxKind::ALL[usize::from(raw.0)]
    }

    fn kind_to_raw(kind: SyntaxKind) -> rowan::SyntaxKind {
        rowan::SyntaxKind(kind as u16)
    }
}

pub type SyntaxNode = rowan::SyntaxNode<Trendy>;
pub type SyntaxToken = rowan::SyntaxToken<Trendy>;
pub type SyntaxElement = rowan::SyntaxElement<Trendy>;

/// The byte range a node or token covers in the source.
#[must_use]
pub fn span(range: rowan::TextRange) -> Span {
    Span::new(range.start().into(), range.end().into())
}

/// An indented outline of a tree, one node or token per line, as
/// `Kind` for nodes and `Kind "text"` for tokens. For tests and debugging.
#[must_use]
pub fn outline(node: &SyntaxNode) -> String {
    let mut out = String::new();
    for event in node.preorder_with_tokens() {
        let rowan::WalkEvent::Enter(element) = event else {
            continue;
        };
        let depth = element.parent().map_or(0, |p| p.ancestors().count());
        out.push_str(&"  ".repeat(depth));
        // Writing to a `String` cannot fail.
        let _ = match element {
            rowan::NodeOrToken::Node(n) => writeln!(out, "{:?}", n.kind()),
            rowan::NodeOrToken::Token(t) => writeln!(out, "{:?} {:?}", t.kind(), t.text()),
        };
    }
    out
}

/// Builds a source file's tree bottom-up, in source order. Used by the
/// parser.
///
/// The root [`SyntaxKind::SourceFile`] node is opened by [`TreeBuilder::new`]
/// and closed by [`TreeBuilder::finish`], so every tree is a source file.
pub struct TreeBuilder(rowan::GreenNodeBuilder<'static>);

impl TreeBuilder {
    #[must_use]
    pub fn new() -> Self {
        let mut builder = TreeBuilder(rowan::GreenNodeBuilder::new());
        builder.start_node(SyntaxKind::SourceFile);
        builder
    }

    pub fn start_node(&mut self, kind: SyntaxKind) {
        self.0
            .start_node(<Trendy as rowan::Language>::kind_to_raw(kind));
    }

    pub fn token(&mut self, kind: SyntaxKind, text: &str) {
        self.0
            .token(<Trendy as rowan::Language>::kind_to_raw(kind), text);
    }

    pub fn finish_node(&mut self) {
        self.0.finish_node();
    }

    /// The finished tree. Every node started since [`TreeBuilder::new`] must
    /// have been finished.
    #[must_use]
    pub fn finish(mut self) -> ast::SourceFile {
        self.0.finish_node();
        ast::SourceFile(SyntaxNode::new_root(self.0.finish()))
    }
}

impl Default for TreeBuilder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keywords_have_fixed_text() {
        for kind in SyntaxKind::ALL.into_iter().filter(|k| k.is_keyword()) {
            let text = kind.fixed_text().unwrap();
            assert_eq!(SyntaxKind::keyword(text), Some(kind));
        }
        assert_eq!(SyntaxKind::keyword("main"), None);
    }

    #[test]
    fn kinds_round_trip_through_rowan() {
        for (i, &kind) in SyntaxKind::ALL.iter().enumerate() {
            assert_eq!(kind as usize, i, "ALL is out of order at {kind:?}");
            let raw = <Trendy as rowan::Language>::kind_to_raw(kind);
            assert_eq!(<Trendy as rowan::Language>::kind_from_raw(raw), kind);
        }
    }
}
