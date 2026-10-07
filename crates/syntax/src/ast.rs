//! A typed view of the CST.
//!
//! Each type wraps a node of one kind and gives named access to its parts.
//! Parts are `Option` because a tree with syntax errors can be missing any of
//! them.

use trendy_diag::Span;

use crate::literal;
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
ast_node!(VarDecl);
ast_node!(Name);
ast_node!(TypeRef);
ast_node!(Literal);

/// A top-level definition.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Item {
    Operator(Operator),
    Variable(VarDecl),
}

impl SourceFile {
    /// Every definition, in source order.
    pub fn items(&self) -> impl Iterator<Item = Item> {
        self.0.children().filter_map(|node| {
            Operator::cast(node.clone())
                .map(Item::Operator)
                .or_else(|| VarDecl::cast(node).map(Item::Variable))
        })
    }

    pub fn operators(&self) -> impl Iterator<Item = Operator> {
        self.0.children().filter_map(Operator::cast)
    }

    pub fn variables(&self) -> impl Iterator<Item = VarDecl> {
        self.0.children().filter_map(VarDecl::cast)
    }
}

impl Operator {
    #[must_use]
    pub fn name(&self) -> Option<Name> {
        self.0.children().find_map(Name::cast)
    }
}

impl VarDecl {
    #[must_use]
    pub fn name(&self) -> Option<Name> {
        self.0.children().find_map(Name::cast)
    }

    /// The declared type, from `: type`.
    #[must_use]
    pub fn type_ref(&self) -> Option<TypeRef> {
        self.0
            .children()
            .find(|n| n.kind() == SyntaxKind::TypeAnnotation)?
            .children()
            .find_map(TypeRef::cast)
    }

    /// The initial value, from `:= value`.
    #[must_use]
    pub fn initializer(&self) -> Option<Literal> {
        self.0
            .children()
            .find(|n| n.kind() == SyntaxKind::Initializer)?
            .children()
            .find_map(Literal::cast)
    }
}

impl TypeRef {
    #[must_use]
    pub fn ident(&self) -> Option<SyntaxToken> {
        ident_of(&self.0)
    }
}

/// What a literal stands for.
#[derive(Debug, Clone, PartialEq)]
pub enum LiteralValue {
    Integer(i64),
    Rational(f64),
    Text(String),
}

/// Why a literal has no value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LiteralError {
    /// An integer literal that doesn't fit in 64 bits.
    IntegerTooLarge,
    /// A rational literal too large to represent.
    RationalTooLarge,
    /// A literal the lexer already reported as malformed.
    Malformed,
}

impl Literal {
    #[must_use]
    pub fn token(&self) -> Option<SyntaxToken> {
        self.0
            .children_with_tokens()
            .filter_map(rowan::NodeOrToken::into_token)
            .find(|t| {
                matches!(
                    t.kind(),
                    SyntaxKind::IntLiteral | SyntaxKind::RationalLiteral | SyntaxKind::TextLiteral
                )
            })
    }

    /// # Errors
    ///
    /// Returns why the literal has no value.
    pub fn value(&self) -> Result<LiteralValue, LiteralError> {
        let token = self.token().ok_or(LiteralError::Malformed)?;
        let text = token.text();
        match token.kind() {
            SyntaxKind::IntLiteral => {
                literal::check_int(text).map_err(|_| LiteralError::Malformed)?;
                return literal::int_value(text)
                    .map(LiteralValue::Integer)
                    .ok_or(LiteralError::IntegerTooLarge);
            }
            SyntaxKind::RationalLiteral => {
                literal::check_rational(text).map_err(|_| LiteralError::Malformed)?;
                return literal::rational_value(text)
                    .map(LiteralValue::Rational)
                    .ok_or(LiteralError::RationalTooLarge);
            }
            _ => {}
        }
        let body = text
            .strip_prefix('"')
            .and_then(|t| t.strip_suffix('"'))
            .filter(|_| text.len() >= 2)
            .ok_or(LiteralError::Malformed)?;
        let (value, problems) = literal::decode_text(body);
        if problems.is_empty() {
            Ok(LiteralValue::Text(value))
        } else {
            Err(LiteralError::Malformed)
        }
    }
}

fn ident_of(node: &SyntaxNode) -> Option<SyntaxToken> {
    node.children_with_tokens()
        .filter_map(rowan::NodeOrToken::into_token)
        .find(|t| t.kind() == SyntaxKind::Ident)
}

impl Name {
    #[must_use]
    pub fn ident(&self) -> Option<SyntaxToken> {
        ident_of(&self.0)
    }
}
