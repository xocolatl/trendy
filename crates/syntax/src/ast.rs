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
ast_node!(AssignStmt);
ast_node!(AssignBlock);
ast_node!(Assignment);
ast_node!(Insertion);
ast_node!(Deletion);
ast_node!(Update);
ast_node!(Arguments);
ast_node!(PositionalArg);
ast_node!(NameRef);
ast_node!(UnaryExpr);
ast_node!(BinaryExpr);
ast_node!(ParenExpr);
ast_node!(ExtractExpr);
ast_node!(ProjectExpr);
ast_node!(PostfixExpr);
ast_node!(NaryExpr);
ast_node!(ImageExpr);
ast_node!(FromExpr);
ast_node!(NameList);
ast_node!(RenameList);
ast_node!(Renaming);
ast_node!(GatherList);
ast_node!(Gathering);
ast_node!(AssignList);
ast_node!(Assign);
ast_node!(VarDecl);
ast_node!(Key);
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
    pub fn statements(&self) -> impl Iterator<Item = Statement> {
        self.0.children().filter_map(|node| {
            CallStmt::cast(node.clone())
                .map(Statement::Call)
                .or_else(|| AssignStmt::cast(node.clone()).map(Statement::Assign))
                .or_else(|| AssignBlock::cast(node).map(Statement::Block))
        })
    }
}

/// A statement in an operator's body.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Statement {
    Call(CallStmt),
    Assign(AssignStmt),
    Block(AssignBlock),
}

impl AssignStmt {
    /// The assignments, in source order: one, or, in a statement the
    /// parser reported, several separated by commas.
    pub fn clauses(&self) -> impl Iterator<Item = Clause> + use<> {
        self.0.children().filter_map(Clause::cast)
    }

    /// The `assign` block the statement is in, if it is in one.
    #[must_use]
    pub fn block(&self) -> Option<AssignBlock> {
        AssignBlock::cast(self.0.parent()?)
    }

    /// The span to delete to remove the statement, with the whitespace
    /// directly before it.
    #[must_use]
    pub fn removal(&self) -> Span {
        statement_removal(&self.0)
    }
}

impl AssignBlock {
    /// The `assign` keyword.
    #[must_use]
    pub fn keyword(&self) -> Option<SyntaxToken> {
        significant_tokens(&self.0).next()
    }

    /// The assignment statements in the block, in source order.
    pub fn statements(&self) -> impl Iterator<Item = AssignStmt> + use<> {
        self.0.children().filter_map(AssignStmt::cast)
    }

    /// The span to delete to remove the block, with the whitespace directly
    /// before it.
    #[must_use]
    pub fn removal(&self) -> Span {
        statement_removal(&self.0)
    }
}

/// The span to delete to remove the statement `node`, with the whitespace
/// directly before it. A comment before it stays, since it may be about
/// what follows.
fn statement_removal(node: &SyntaxNode) -> Span {
    let own = span(node.text_range());
    let start = node
        .prev_sibling_or_token()
        .filter(|e| e.kind() == SyntaxKind::Whitespace)
        .map_or(own.start, |e| span(e.text_range()).start);
    Span::new(start, own.end)
}

/// One assignment in an assignment statement: `v := x`, or a shorthand for
/// one.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Clause {
    /// `v := x`
    Assignment(Assignment),
    /// `insert r s` or `d_insert r s`
    Insertion(Insertion),
    /// `delete r where p` or `i_delete r s`
    Deletion(Deletion),
    /// `update r { a := x } where p`
    Update(Update),
}

impl Clause {
    #[must_use]
    pub fn cast(node: SyntaxNode) -> Option<Self> {
        Assignment::cast(node.clone())
            .map(Clause::Assignment)
            .or_else(|| Insertion::cast(node.clone()).map(Clause::Insertion))
            .or_else(|| Deletion::cast(node.clone()).map(Clause::Deletion))
            .or_else(|| Update::cast(node).map(Clause::Update))
    }

    #[must_use]
    pub fn syntax(&self) -> &SyntaxNode {
        match self {
            Clause::Assignment(c) => c.syntax(),
            Clause::Insertion(c) => c.syntax(),
            Clause::Deletion(c) => c.syntax(),
            Clause::Update(c) => c.syntax(),
        }
    }

    #[must_use]
    pub fn span(&self) -> Span {
        span(self.syntax().text_range())
    }

    /// The variable assigned: the first expression.
    #[must_use]
    pub fn target(&self) -> Option<Expr> {
        self.syntax().children().find_map(Expr::cast)
    }

    /// The keyword it starts with, for a shorthand: `insert`, `d_insert`,
    /// `delete`, `i_delete` or `update`.
    #[must_use]
    pub fn keyword(&self) -> Option<SyntaxToken> {
        significant_tokens(self.syntax()).find(|t| t.kind().is_assignment_keyword())
    }

    /// The expression after the target: the value assigned, the relation
    /// inserted or deleted, or the condition of a `delete` or an `update`.
    #[must_use]
    pub fn operand(&self) -> Option<Expr> {
        self.syntax().children().filter_map(Expr::cast).nth(1)
    }

    /// The `where` before a condition, if there is one.
    #[must_use]
    pub fn where_keyword(&self) -> Option<SyntaxToken> {
        significant_tokens(self.syntax()).find(|t| t.kind() == SyntaxKind::WhereKw)
    }

    /// The span to delete to remove the `where` and the condition, with the
    /// whitespace before them, up to the end of what comes before.
    #[must_use]
    pub fn condition_removal(&self) -> Option<Span> {
        let keyword = self.where_keyword()?;
        let end = self.operand()?.span().end;
        Some(Span::new(preceded_from(&keyword.into()), end))
    }

    /// The statement the assignment is in.
    #[must_use]
    pub fn statement(&self) -> Option<AssignStmt> {
        AssignStmt::cast(self.syntax().parent()?)
    }

    /// An `update`'s attributes and their new values.
    #[must_use]
    pub fn assignments(&self) -> Option<AssignList> {
        self.syntax().children().find_map(AssignList::cast)
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
    /// For `not matching`, its first word.
    #[must_use]
    pub fn operator(&self) -> Option<SyntaxToken> {
        significant_tokens(&self.0).next()
    }

    /// The operator's words: one, or two for `not matching`.
    pub fn operators(&self) -> impl Iterator<Item = SyntaxToken> {
        significant_tokens(&self.0)
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

impl ExtractExpr {
    #[must_use]
    pub fn operand(&self) -> Option<Expr> {
        self.0.children().find_map(Expr::cast)
    }

    /// The attribute's name, after the `.`.
    #[must_use]
    pub fn attribute(&self) -> Option<Name> {
        self.0.children().find_map(Name::cast)
    }
}

impl ProjectExpr {
    #[must_use]
    pub fn operand(&self) -> Option<Expr> {
        self.0.children().find_map(Expr::cast)
    }

    #[must_use]
    pub fn names(&self) -> Option<NameList> {
        self.0.children().find_map(NameList::cast)
    }
}

impl PostfixExpr {
    /// The operator: `remove`, `rename` or `extend`.
    #[must_use]
    pub fn operator(&self) -> Option<SyntaxToken> {
        significant_tokens(&self.0).next()
    }

    #[must_use]
    pub fn operand(&self) -> Option<Expr> {
        self.0.children().find_map(Expr::cast)
    }

    #[must_use]
    pub fn names(&self) -> Option<NameList> {
        self.0.children().find_map(NameList::cast)
    }

    #[must_use]
    pub fn renamings(&self) -> Option<RenameList> {
        self.0.children().find_map(RenameList::cast)
    }

    /// The gatherings of a `group` or a `wrap`.
    #[must_use]
    pub fn gatherings(&self) -> Option<GatherList> {
        self.0.children().find_map(GatherList::cast)
    }

    /// The attribute assignments of an `extend` or a `replace`.
    #[must_use]
    pub fn assignments(&self) -> Option<AssignList> {
        self.0.children().find_map(AssignList::cast)
    }
}

impl GatherList {
    pub fn gatherings(&self) -> impl Iterator<Item = Gathering> {
        self.0.children().filter_map(Gathering::cast)
    }
}

impl Gathering {
    /// The new attribute's name.
    #[must_use]
    pub fn name(&self) -> Option<Name> {
        self.0.children().find_map(Name::cast)
    }

    /// The attributes it gathers.
    #[must_use]
    pub fn names(&self) -> Option<NameList> {
        self.0.children().find_map(NameList::cast)
    }
}

impl AssignList {
    pub fn assignments(&self) -> impl Iterator<Item = Assign> {
        self.0.children().filter_map(Assign::cast)
    }

    /// The span to delete to remove `assign` from the list, with a comma
    /// that separates it from a neighbor.
    #[must_use]
    pub fn removal(&self, assign: &Assign) -> Span {
        list_removal(assign.syntax())
    }
}

impl Assign {
    /// The attribute's name.
    #[must_use]
    pub fn name(&self) -> Option<Name> {
        self.0.children().find_map(Name::cast)
    }

    /// Its value.
    #[must_use]
    pub fn value(&self) -> Option<Expr> {
        self.0.children().find_map(Expr::cast)
    }
}

impl NaryExpr {
    /// The operator: `join`, `times` or `compose`.
    #[must_use]
    pub fn operator(&self) -> Option<SyntaxToken> {
        significant_tokens(&self.0).next()
    }

    pub fn operands(&self) -> impl Iterator<Item = Expr> {
        self.0.children().filter_map(Expr::cast)
    }
}

impl ImageExpr {
    /// The operator, `!!`.
    #[must_use]
    pub fn operator(&self) -> Option<SyntaxToken> {
        significant_tokens(&self.0).next()
    }

    /// The relation whose tuples are matched.
    #[must_use]
    pub fn operand(&self) -> Option<Expr> {
        self.0.children().find_map(Expr::cast)
    }
}

/// What a `from` expression takes from a relation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Taken {
    /// `tuple from r`: its only tuple.
    Tuple,
    /// `value from r`: the only attribute's value of its only tuple.
    Value,
}

impl FromExpr {
    /// What it takes: a tuple, after `tuple`, or a value, after `value`.
    #[must_use]
    pub fn taken(&self) -> Taken {
        match significant_tokens(&self.0).next().map(|t| t.kind()) {
            Some(SyntaxKind::TupleKw) => Taken::Tuple,
            _ => Taken::Value,
        }
    }

    /// The words before the operand: `tuple from` or `value from`.
    pub fn keywords(&self) -> impl Iterator<Item = SyntaxToken> {
        significant_tokens(&self.0)
    }

    /// The relation it takes from.
    #[must_use]
    pub fn operand(&self) -> Option<Expr> {
        self.0.children().find_map(Expr::cast)
    }
}

impl RenameList {
    pub fn renamings(&self) -> impl Iterator<Item = Renaming> {
        self.0.children().filter_map(Renaming::cast)
    }

    /// The span to delete to remove `renaming` from the list, with a comma
    /// that separates it from a neighbor.
    #[must_use]
    pub fn removal(&self, renaming: &Renaming) -> Span {
        list_removal(renaming.syntax())
    }
}

impl Renaming {
    /// The attribute's name before renaming: the one before `as`.
    #[must_use]
    pub fn before(&self) -> Option<Name> {
        self.0.children().find_map(Name::cast)
    }

    /// Its name after renaming: the one after `as`.
    #[must_use]
    pub fn after(&self) -> Option<Name> {
        self.0.children().filter_map(Name::cast).nth(1)
    }
}

impl NameList {
    pub fn names(&self) -> impl Iterator<Item = Name> {
        self.0.children().filter_map(Name::cast)
    }

    /// The span to delete to remove `name` from the list, with a comma that
    /// separates it from a neighbor.
    #[must_use]
    pub fn removal(&self, name: &Name) -> Span {
        list_removal(name.syntax())
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

    /// The keys, `key { a, b }`, in source order.
    pub fn keys(&self) -> impl Iterator<Item = Key> {
        self.0.children().filter_map(Key::cast)
    }
}

impl Key {
    /// The words before the names: `key`, or another language's spelling,
    /// such as SQL's `primary key` or `unique`, which the parser reports.
    pub fn keywords(&self) -> impl Iterator<Item = SyntaxToken> {
        significant_tokens(&self.0).filter(|t| t.kind() == SyntaxKind::Ident)
    }

    /// The attributes, `{ a, b }`.
    #[must_use]
    pub fn names(&self) -> Option<NameList> {
        self.0.children().find_map(NameList::cast)
    }

    /// The span to delete to remove the key, with the whitespace before it,
    /// up to the end of what comes before.
    #[must_use]
    pub fn removal(&self) -> Span {
        Span::new(preceded_from(&self.0.clone().into()), self.span().end)
    }
}

/// Where the trivia before `element` starts: the end of the sibling before
/// it that isn't trivia, or, if there is none, its own start.
fn preceded_from(element: &SyntaxElement) -> usize {
    let own = span(element.text_range()).start;
    std::iter::successors(
        element.prev_sibling_or_token(),
        SyntaxElement::prev_sibling_or_token,
    )
    .find(|e| !e.kind().is_trivia())
    .map_or(own, |e| span(e.text_range()).end)
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
    Extract(ExtractExpr),
    Project(ProjectExpr),
    Postfix(PostfixExpr),
    Nary(NaryExpr),
    Image(ImageExpr),
    From(FromExpr),
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
            .or_else(|| ParenExpr::cast(node.clone()).map(Expr::Paren))
            .or_else(|| ExtractExpr::cast(node.clone()).map(Expr::Extract))
            .or_else(|| ProjectExpr::cast(node.clone()).map(Expr::Project))
            .or_else(|| PostfixExpr::cast(node.clone()).map(Expr::Postfix))
            .or_else(|| NaryExpr::cast(node.clone()).map(Expr::Nary))
            .or_else(|| ImageExpr::cast(node.clone()).map(Expr::Image))
            .or_else(|| FromExpr::cast(node).map(Expr::From))
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
            Expr::Extract(e) => e.syntax(),
            Expr::Project(e) => e.syntax(),
            Expr::Postfix(e) => e.syntax(),
            Expr::Nary(e) => e.syntax(),
            Expr::Image(e) => e.syntax(),
            Expr::From(e) => e.syntax(),
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

    /// The tuples: tuple selectors, or any expressions of tuples.
    pub fn elements(&self) -> impl Iterator<Item = Expr> {
        self.0.children().filter_map(Expr::cast)
    }

    /// The span to delete to remove `element`, including the comma that
    /// separates it from a neighbor.
    #[must_use]
    pub fn removal(&self, element: &Expr) -> Span {
        list_removal(element.syntax())
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
    Approximate(f64),
    Text(String),
}

/// Why a literal has no value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LiteralError {
    /// An integer literal that doesn't fit in 64 bits.
    IntegerTooLarge,
    /// An approximate literal too large to represent.
    ApproximateTooLarge,
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
            SyntaxKind::ApproximateLiteral => {
                literal::check_approximate(text).map_err(|_| LiteralError::Malformed)?;
                return literal::approximate_value(text)
                    .map(LiteralValue::Approximate)
                    .ok_or(LiteralError::ApproximateTooLarge);
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
