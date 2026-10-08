//! A typed view of the CST.
//!
//! Each type wraps a node of one kind and gives named access to its parts.
//! Parts are `Option` because a tree with syntax errors can be missing any of
//! them.

use trendy_diag::Span;

use crate::literal;
use crate::{SyntaxElement, SyntaxKind, SyntaxNode, SyntaxToken, span};

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
ast_node!(TupleType);
ast_node!(AttributeType);
ast_node!(TupleLiteral);
ast_node!(AttributeValue);

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
    pub fn type_expr(&self) -> Option<TypeExpr> {
        self.0
            .children()
            .find(|n| n.kind() == SyntaxKind::TypeAnnotation)?
            .children()
            .find_map(TypeExpr::cast)
    }

    /// The initial value, from `:= value`.
    #[must_use]
    pub fn initializer(&self) -> Option<Expr> {
        self.0
            .children()
            .find(|n| n.kind() == SyntaxKind::Initializer)?
            .children()
            .find_map(Expr::cast)
    }
}

/// A type as written: a name, or a tuple type.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum TypeExpr {
    Named(TypeRef),
    Tuple(TupleType),
}

impl TypeExpr {
    #[must_use]
    pub fn cast(node: SyntaxNode) -> Option<Self> {
        TypeRef::cast(node.clone())
            .map(TypeExpr::Named)
            .or_else(|| TupleType::cast(node).map(TypeExpr::Tuple))
    }

    #[must_use]
    pub fn syntax(&self) -> &SyntaxNode {
        match self {
            TypeExpr::Named(t) => t.syntax(),
            TypeExpr::Tuple(t) => t.syntax(),
        }
    }

    #[must_use]
    pub fn span(&self) -> Span {
        span(self.syntax().text_range())
    }
}

/// An expression. For now: a literal, or a tuple value.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Expr {
    Literal(Literal),
    Tuple(TupleLiteral),
}

impl Expr {
    #[must_use]
    pub fn cast(node: SyntaxNode) -> Option<Self> {
        Literal::cast(node.clone())
            .map(Expr::Literal)
            .or_else(|| TupleLiteral::cast(node).map(Expr::Tuple))
    }

    #[must_use]
    pub fn syntax(&self) -> &SyntaxNode {
        match self {
            Expr::Literal(e) => e.syntax(),
            Expr::Tuple(e) => e.syntax(),
        }
    }

    #[must_use]
    pub fn span(&self) -> Span {
        span(self.syntax().text_range())
    }
}

impl TupleType {
    pub fn attributes(&self) -> impl Iterator<Item = AttributeType> {
        self.0.children().filter_map(AttributeType::cast)
    }
}

impl AttributeType {
    #[must_use]
    pub fn name(&self) -> Option<Name> {
        self.0.children().find_map(Name::cast)
    }

    #[must_use]
    pub fn type_expr(&self) -> Option<TypeExpr> {
        self.0.children().find_map(TypeExpr::cast)
    }
}

impl TupleLiteral {
    pub fn attributes(&self) -> impl Iterator<Item = AttributeValue> {
        self.0.children().filter_map(AttributeValue::cast)
    }

    /// Where a new attribute goes, as an offset and the text to put around
    /// `name: value`: after the last attribute, keeping any trailing comma
    /// trailing.
    #[must_use]
    pub fn insertion(&self) -> (usize, &'static str, &'static str) {
        let Some(last) = self.attributes().last() else {
            let after_brace = significant_tokens(&self.0)
                .find(|t| t.kind() == SyntaxKind::LBrace)
                .map_or_else(|| self.span().end, |t| span(t.text_range()).end);
            return (after_brace, " ", "");
        };
        match next_significant(last.syntax()) {
            Some(comma) if comma.kind() == SyntaxKind::Comma => {
                (span(comma.text_range()).end, " ", ",")
            }
            _ => (last.span().end, ", ", ""),
        }
    }

    /// The span to delete to remove `attribute`, including the comma that
    /// separates it from a neighbor.
    #[must_use]
    pub fn removal(&self, attribute: &AttributeValue) -> Span {
        let own = attribute.span();
        if let Some(comma) =
            next_significant(attribute.syntax()).filter(|t| t.kind() == SyntaxKind::Comma)
        {
            // Up to the next attribute, so its spacing stays as it was.
            let end = comma
                .next_token()
                .filter(|t| t.kind().is_trivia())
                .map_or_else(
                    || span(comma.text_range()).end,
                    |ws| span(ws.text_range()).end,
                );
            return Span::new(own.start, end);
        }
        if let Some(comma) =
            previous_significant(attribute.syntax()).filter(|t| t.kind() == SyntaxKind::Comma)
        {
            return Span::new(span(comma.text_range()).start, own.end);
        }
        own
    }
}

fn significant_tokens(node: &SyntaxNode) -> impl Iterator<Item = SyntaxToken> {
    node.children_with_tokens()
        .filter_map(rowan::NodeOrToken::into_token)
        .filter(|t| !t.kind().is_trivia())
}

/// The next sibling token of `node` that isn't trivia.
fn next_significant(node: &SyntaxNode) -> Option<SyntaxToken> {
    std::iter::successors(
        node.next_sibling_or_token(),
        SyntaxElement::next_sibling_or_token,
    )
    .find(|e| !e.kind().is_trivia())?
    .into_token()
}

/// The previous sibling token of `node` that isn't trivia.
fn previous_significant(node: &SyntaxNode) -> Option<SyntaxToken> {
    std::iter::successors(
        node.prev_sibling_or_token(),
        SyntaxElement::prev_sibling_or_token,
    )
    .find(|e| !e.kind().is_trivia())?
    .into_token()
}

impl AttributeValue {
    #[must_use]
    pub fn name(&self) -> Option<Name> {
        self.0.children().find_map(Name::cast)
    }

    #[must_use]
    pub fn value(&self) -> Option<Expr> {
        self.0.children().find_map(Expr::cast)
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
    Boolean(bool),
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
            .find(|t| t.kind().is_literal())
    }

    /// # Errors
    ///
    /// Returns why the literal has no value.
    pub fn value(&self) -> Result<LiteralValue, LiteralError> {
        let token = self.token().ok_or(LiteralError::Malformed)?;
        let text = token.text();
        match token.kind() {
            SyntaxKind::TrueKw => return Ok(LiteralValue::Boolean(true)),
            SyntaxKind::FalseKw => return Ok(LiteralValue::Boolean(false)),
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
