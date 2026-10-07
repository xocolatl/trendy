//! A typed view of the CST.
//!
//! Each type wraps a node of one kind and gives named access to its parts.
//! Parts are `Option` because a tree with syntax errors can be missing any of
//! them.

use trendy_diag::Span;

use crate::{SyntaxKind, SyntaxNode, SyntaxToken, span};

/// A typed wrapper around a node of one particular kind.
pub trait AstNode: Sized {
    fn cast(node: SyntaxNode) -> Option<Self>;
    fn syntax(&self) -> &SyntaxNode;

    fn span(&self) -> Span {
        span(self.syntax().text_range())
    }
}

macro_rules! ast_node {
    ($name:ident) => {
        #[derive(Debug, Clone, PartialEq, Eq, Hash)]
        pub struct $name(pub(crate) SyntaxNode);

        impl AstNode for $name {
            fn cast(node: SyntaxNode) -> Option<Self> {
                (node.kind() == SyntaxKind::$name).then(|| $name(node))
            }

            fn syntax(&self) -> &SyntaxNode {
                &self.0
            }
        }
    };
}

ast_node!(SourceFile);
ast_node!(Operator);
ast_node!(Name);

impl SourceFile {
    pub fn operators(&self) -> impl Iterator<Item = Operator> {
        self.0.children().filter_map(Operator::cast)
    }
}

impl Operator {
    #[must_use]
    pub fn name(&self) -> Option<Name> {
        self.0.children().find_map(Name::cast)
    }
}

impl Name {
    #[must_use]
    pub fn ident(&self) -> Option<SyntaxToken> {
        self.0
            .children_with_tokens()
            .filter_map(rowan::NodeOrToken::into_token)
            .find(|t| t.kind() == SyntaxKind::Ident)
    }
}
