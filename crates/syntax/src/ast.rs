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
ast_node!(BuiltinOperator);
ast_node!(BuiltinType);
ast_node!(Body);
ast_node!(CallStmt);
ast_node!(Arguments);
ast_node!(PositionalArg);
ast_node!(NameRef);
ast_node!(UnaryExpr);
ast_node!(BinaryExpr);
ast_node!(ParenExpr);
ast_node!(VarDecl);
ast_node!(Name);
ast_node!(TypeRef);
ast_node!(Literal);
ast_node!(Heading);
ast_node!(AttributeType);
ast_node!(TupleType);
ast_node!(RelationType);
ast_node!(TupleLiteral);
ast_node!(AttributeValue);
ast_node!(RelationLiteral);

/// A top-level definition.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Item {
    Operator(Operator),
    Builtin(BuiltinOperator),
    BuiltinType(BuiltinType),
    Variable(VarDecl),
}

impl SourceFile {
    /// Every definition, in source order.
    pub fn items(&self) -> impl Iterator<Item = Item> {
        self.0.children().filter_map(|node| {
            Operator::cast(node.clone())
                .map(Item::Operator)
                .or_else(|| BuiltinOperator::cast(node.clone()).map(Item::Builtin))
                .or_else(|| BuiltinType::cast(node.clone()).map(Item::BuiltinType))
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

    /// The parameters: a heading.
    #[must_use]
    pub fn params(&self) -> Option<Heading> {
        self.0.children().find_map(Heading::cast)
    }

    #[must_use]
    pub fn body(&self) -> Option<Body> {
        self.0.children().find_map(Body::cast)
    }
}

impl BuiltinOperator {
    #[must_use]
    pub fn name(&self) -> Option<Name> {
        self.0.children().find_map(Name::cast)
    }

    /// The parameters: a heading.
    #[must_use]
    pub fn params(&self) -> Option<Heading> {
        self.0.children().find_map(Heading::cast)
    }

    /// The `builtin` keyword.
    #[must_use]
    pub fn keyword(&self) -> Option<SyntaxToken> {
        significant_tokens(&self.0).find(|t| t.kind() == SyntaxKind::BuiltinKw)
    }
}

impl BuiltinType {
    #[must_use]
    pub fn name(&self) -> Option<Name> {
        self.0.children().find_map(Name::cast)
    }

    /// The `builtin` keyword.
    #[must_use]
    pub fn keyword(&self) -> Option<SyntaxToken> {
        significant_tokens(&self.0).find(|t| t.kind() == SyntaxKind::BuiltinKw)
    }
}

impl Body {
    pub fn statements(&self) -> impl Iterator<Item = CallStmt> {
        self.0.children().filter_map(CallStmt::cast)
    }
}

impl CallStmt {
    /// The operator called.
    #[must_use]
    pub fn callee(&self) -> Option<NameRef> {
        self.0.children().find_map(NameRef::cast)
    }

    #[must_use]
    pub fn arguments(&self) -> Option<Arguments> {
        self.0.children().find_map(Arguments::cast)
    }
}

impl Arguments {
    /// The named arguments, `name: value`.
    pub fn named(&self) -> impl Iterator<Item = AttributeValue> {
        self.0.children().filter_map(AttributeValue::cast)
    }

    pub fn positional(&self) -> impl Iterator<Item = PositionalArg> {
        self.0.children().filter_map(PositionalArg::cast)
    }

    /// Where a new argument goes, as for [`TupleLiteral::insertion`].
    #[must_use]
    pub fn insertion(&self) -> (usize, &'static str, &'static str) {
        list_insertion(&self.0, self.named().last().map(|a| a.0))
    }

    /// The span to delete to remove `argument`, with its comma.
    #[must_use]
    pub fn removal(&self, argument: &AttributeValue) -> Span {
        list_removal(argument.syntax())
    }
}

impl PositionalArg {
    #[must_use]
    pub fn value(&self) -> Option<Expr> {
        self.0.children().find_map(Expr::cast)
    }
}

impl UnaryExpr {
    /// The operator: `not`, or a foreign spelling of it such as `!`.
    #[must_use]
    pub fn operator(&self) -> Option<SyntaxToken> {
        significant_tokens(&self.0).next()
    }

    #[must_use]
    pub fn operand(&self) -> Option<Expr> {
        self.0.children().find_map(Expr::cast)
    }
}

impl BinaryExpr {
    /// The operator: a keyword such as `and`, or a foreign spelling of one.
    #[must_use]
    pub fn operator(&self) -> Option<SyntaxToken> {
        significant_tokens(&self.0).next()
    }

    #[must_use]
    pub fn lhs(&self) -> Option<Expr> {
        self.0.children().find_map(Expr::cast)
    }

    #[must_use]
    pub fn rhs(&self) -> Option<Expr> {
        self.0.children().filter_map(Expr::cast).nth(1)
    }
}

impl ParenExpr {
    #[must_use]
    pub fn inner(&self) -> Option<Expr> {
        self.0.children().find_map(Expr::cast)
    }
}

impl NameRef {
    #[must_use]
    pub fn ident(&self) -> Option<SyntaxToken> {
        ident_of(&self.0)
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

/// A type as written: a name, a tuple type, or a relation type.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum TypeExpr {
    Named(TypeRef),
    Tuple(TupleType),
    Relation(RelationType),
}

impl TypeExpr {
    #[must_use]
    pub fn cast(node: SyntaxNode) -> Option<Self> {
        TypeRef::cast(node.clone())
            .map(TypeExpr::Named)
            .or_else(|| TupleType::cast(node.clone()).map(TypeExpr::Tuple))
            .or_else(|| RelationType::cast(node).map(TypeExpr::Relation))
    }

    #[must_use]
    pub fn syntax(&self) -> &SyntaxNode {
        match self {
            TypeExpr::Named(t) => t.syntax(),
            TypeExpr::Tuple(t) => t.syntax(),
            TypeExpr::Relation(t) => t.syntax(),
        }
    }

    #[must_use]
    pub fn span(&self) -> Span {
        span(self.syntax().text_range())
    }
}

/// An expression.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Expr {
    Literal(Literal),
    Tuple(TupleLiteral),
    Relation(RelationLiteral),
    Name(NameRef),
    Unary(UnaryExpr),
    Binary(BinaryExpr),
    Paren(ParenExpr),
}

impl Expr {
    #[must_use]
    pub fn cast(node: SyntaxNode) -> Option<Self> {
        Literal::cast(node.clone())
            .map(Expr::Literal)
            .or_else(|| TupleLiteral::cast(node.clone()).map(Expr::Tuple))
            .or_else(|| RelationLiteral::cast(node.clone()).map(Expr::Relation))
            .or_else(|| NameRef::cast(node.clone()).map(Expr::Name))
            .or_else(|| UnaryExpr::cast(node.clone()).map(Expr::Unary))
            .or_else(|| BinaryExpr::cast(node.clone()).map(Expr::Binary))
            .or_else(|| ParenExpr::cast(node).map(Expr::Paren))
    }

    #[must_use]
    pub fn syntax(&self) -> &SyntaxNode {
        match self {
            Expr::Literal(e) => e.syntax(),
            Expr::Tuple(e) => e.syntax(),
            Expr::Relation(e) => e.syntax(),
            Expr::Name(e) => e.syntax(),
            Expr::Unary(e) => e.syntax(),
            Expr::Binary(e) => e.syntax(),
            Expr::Paren(e) => e.syntax(),
        }
    }

    #[must_use]
    pub fn span(&self) -> Span {
        span(self.syntax().text_range())
    }
}

impl Heading {
    pub fn attributes(&self) -> impl Iterator<Item = AttributeType> {
        self.0.children().filter_map(AttributeType::cast)
    }
}

impl TupleType {
    #[must_use]
    pub fn heading(&self) -> Option<Heading> {
        self.0.children().find_map(Heading::cast)
    }
}

impl RelationType {
    #[must_use]
    pub fn heading(&self) -> Option<Heading> {
        self.0.children().find_map(Heading::cast)
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
    /// The heading written before the attributes, as in
    /// `tuple { a: integer } { a: 1 }`.
    #[must_use]
    pub fn heading(&self) -> Option<Heading> {
        self.0.children().find_map(Heading::cast)
    }

    pub fn attributes(&self) -> impl Iterator<Item = AttributeValue> {
        self.0.children().filter_map(AttributeValue::cast)
    }

    /// Where a new attribute goes, as an offset and the text to put around
    /// `name: value`: after the last attribute, keeping any trailing comma
    /// trailing.
    #[must_use]
    pub fn insertion(&self) -> (usize, &'static str, &'static str) {
        list_insertion(&self.0, self.attributes().last().map(|a| a.0))
    }

    /// The span to delete to remove `attribute`, including the comma that
    /// separates it from a neighbor.
    #[must_use]
    pub fn removal(&self, attribute: &AttributeValue) -> Span {
        list_removal(attribute.syntax())
    }
}

impl RelationLiteral {
    /// The heading written before the tuples, as in
    /// `relation { a: integer } {}`.
    #[must_use]
    pub fn heading(&self) -> Option<Heading> {
        self.0.children().find_map(Heading::cast)
    }

    pub fn tuples(&self) -> impl Iterator<Item = TupleLiteral> {
        self.0.children().filter_map(TupleLiteral::cast)
    }

    /// The span to delete to remove `tuple`, including the comma that
    /// separates it from a neighbor.
    #[must_use]
    pub fn removal(&self, tuple: &TupleLiteral) -> Span {
        list_removal(tuple.syntax())
    }
}

/// Where a new element goes in the braced list `container`, whose last
/// element is `last_item`: an offset and the text to put around it. After the
/// last element, keeping any trailing comma trailing; or just inside the `{`.
fn list_insertion(
    container: &SyntaxNode,
    last_item: Option<SyntaxNode>,
) -> (usize, &'static str, &'static str) {
    let Some(last_item) = last_item else {
        // The container's own `{` is a token child; a heading's is inside it.
        let after_brace = significant_tokens(container)
            .find(|t| t.kind() == SyntaxKind::LBrace)
            .map_or_else(
                || span(container.text_range()).end,
                |t| span(t.text_range()).end,
            );
        return (after_brace, " ", "");
    };
    match next_significant(&last_item) {
        Some(comma) if comma.kind() == SyntaxKind::Comma => {
            (span(comma.text_range()).end, " ", ",")
        }
        _ => (span(last_item.text_range()).end, ", ", ""),
    }
}

/// The span to delete to remove a comma-separated list element, including
/// the comma that separates it from a neighbor.
fn list_removal(item: &SyntaxNode) -> Span {
    let own = span(item.text_range());
    if let Some(comma) = next_significant(item).filter(|t| t.kind() == SyntaxKind::Comma) {
        // Up to the next element, so its spacing stays as it was.
        let end = comma
            .next_token()
            .filter(|t| t.kind() == SyntaxKind::Whitespace)
            .map_or_else(
                || span(comma.text_range()).end,
                |ws| span(ws.text_range()).end,
            );
        return Span::new(own.start, end);
    }
    if let Some(comma) = previous_significant(item).filter(|t| t.kind() == SyntaxKind::Comma) {
        return Span::new(span(comma.text_range()).start, own.end);
    }
    own
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
