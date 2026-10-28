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
ast_node!(KindDecl);
ast_node!(HeadingDecl);
ast_node!(RequirementList);
ast_node!(Requirement);
ast_node!(Body);
ast_node!(CallStmt);
ast_node!(ReturnStmt);
ast_node!(IfStmt);
ast_node!(WhileStmt);
ast_node!(LoopStmt);
ast_node!(ForStmt);
ast_node!(AssignStmt);
ast_node!(AssignBlock);
ast_node!(Assignment);
ast_node!(Insertion);
ast_node!(Deletion);
ast_node!(Update);
ast_node!(Load);
ast_node!(ValueBlock);
ast_node!(OrderList);
ast_node!(OrderItem);
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
ast_node!(CallExpr);
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
ast_node!(AnyType);
ast_node!(TypeParameters);
ast_node!(Literal);
ast_node!(Heading);
ast_node!(HeadingRef);
ast_node!(AttributeType);
ast_node!(Per);
ast_node!(Mode);
ast_node!(TupleType);
ast_node!(RelationType);
ast_node!(ArrayType);
ast_node!(ArrayLiteral);
ast_node!(IndexExpr);
ast_node!(IfExpr);
ast_node!(TupleLiteral);
ast_node!(AttributeValue);
ast_node!(RelationLiteral);

/// A top-level definition.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Item {
    Operator(Operator),
    Builtin(BuiltinOperator),
    BuiltinType(BuiltinType),
    Kind(KindDecl),
    Heading(HeadingDecl),
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
                .or_else(|| KindDecl::cast(node.clone()).map(Item::Kind))
                .or_else(|| HeadingDecl::cast(node.clone()).map(Item::Heading))
                .or_else(|| VarDecl::cast(node).map(Item::Variable))
        })
    }

    pub fn headings(&self) -> impl Iterator<Item = HeadingDecl> {
        self.0.children().filter_map(HeadingDecl::cast)
    }

    /// The heading named `name`'s declaration: the first with its name, as
    /// a second is an error.
    #[must_use]
    pub fn heading(&self, name: &str) -> Option<HeadingDecl> {
        self.headings().find(|decl| {
            decl.name()
                .and_then(|n| n.ident())
                .is_some_and(|i| i.text() == name)
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

    /// The result type, from `: type`: a read-only operator's.
    #[must_use]
    pub fn result(&self) -> Option<TypeExpr> {
        self.0
            .children()
            .find(|n| n.kind() == SyntaxKind::TypeAnnotation)?
            .children()
            .find_map(TypeExpr::cast)
    }

    /// The type parameters, `types { T: any type }`.
    #[must_use]
    pub fn type_parameters(&self) -> Option<TypeParameters> {
        self.0.children().find_map(TypeParameters::cast)
    }

    /// Every type written by name in it, nested ones too, in source order,
    /// but in its type parameters, which declare them.
    pub fn type_refs(&self) -> impl Iterator<Item = TypeRef> + use<> {
        self.0.descendants().filter_map(TypeRef::cast).filter(|t| {
            !t.0.ancestors()
                .any(|a| a.kind() == SyntaxKind::TypeParameters)
        })
    }

    /// The span to delete to remove the result type, `: type`, with the
    /// whitespace before it.
    #[must_use]
    pub fn result_removal(&self) -> Option<Span> {
        let annotation = self
            .0
            .children()
            .find(|n| n.kind() == SyntaxKind::TypeAnnotation)?;
        Some(Span::new(
            preceded_from(&annotation.clone().into()),
            span(annotation.text_range()).end,
        ))
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

    /// The result type, from `: type`: a read-only built-in's.
    #[must_use]
    pub fn result(&self) -> Option<TypeExpr> {
        self.0
            .children()
            .find(|n| n.kind() == SyntaxKind::TypeAnnotation)?
            .children()
            .find_map(TypeExpr::cast)
    }

    /// The type parameters, `types { T: any ordered }`.
    #[must_use]
    pub fn type_parameters(&self) -> Option<TypeParameters> {
        self.0.children().find_map(TypeParameters::cast)
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

    /// The kinds it is of, `{ ordered, … }`.
    #[must_use]
    pub fn kinds(&self) -> Option<NameList> {
        self.0.children().find_map(NameList::cast)
    }

    /// The `builtin` keyword.
    #[must_use]
    pub fn keyword(&self) -> Option<SyntaxToken> {
        significant_tokens(&self.0).find(|t| t.kind() == SyntaxKind::BuiltinKw)
    }
}

impl HeadingDecl {
    /// The word `heading`, or another language's for a type, which the
    /// parser reports.
    #[must_use]
    pub fn keyword(&self) -> Option<SyntaxToken> {
        significant_tokens(&self.0).next()
    }

    #[must_use]
    pub fn name(&self) -> Option<Name> {
        self.0.children().find_map(Name::cast)
    }

    /// The attributes it names.
    #[must_use]
    pub fn heading(&self) -> Option<Heading> {
        self.0.children().find_map(Heading::cast)
    }
}

impl KindDecl {
    /// The word `kind`.
    #[must_use]
    pub fn keyword(&self) -> Option<SyntaxToken> {
        significant_tokens(&self.0).next()
    }

    #[must_use]
    pub fn name(&self) -> Option<Name> {
        self.0.children().find_map(Name::cast)
    }

    /// What its types must have.
    pub fn requirements(&self) -> impl Iterator<Item = Requirement> + use<> {
        self.0
            .children()
            .filter_map(RequirementList::cast)
            .flat_map(|list| {
                list.0
                    .children()
                    .filter_map(Requirement::cast)
                    .collect::<Vec<_>>()
            })
    }
}

impl Requirement {
    /// The token it is: an operator, such as `<`.
    #[must_use]
    pub fn token(&self) -> Option<SyntaxToken> {
        significant_tokens(&self.0).next()
    }
}

impl Body {
    /// A read-only operator's value, the expression its body ends with.
    #[must_use]
    pub fn value(&self) -> Option<Expr> {
        self.0.children().find_map(Expr::cast)
    }

    /// The span to delete to remove the value, with the whitespace before
    /// it, as for a statement.
    #[must_use]
    pub fn value_removal(&self) -> Option<Span> {
        self.value().map(|value| statement_removal(value.syntax()))
    }

    pub fn statements(&self) -> impl Iterator<Item = Statement> {
        statements_of(&self.0)
    }

    /// Where something missing from the end of the body goes: just before
    /// its `]`.
    #[must_use]
    pub fn end(&self) -> Option<usize> {
        significant_tokens(&self.0)
            .filter(|t| t.kind() == SyntaxKind::RBracket)
            .last()
            .map(|t| span(t.text_range()).start)
    }
}

/// A statement in an operator's body.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Statement {
    Call(CallStmt),
    Assign(AssignStmt),
    Block(AssignBlock),
    Return(ReturnStmt),
    If(IfStmt),
    While(WhileStmt),
    Loop(LoopStmt),
    For(ForStmt),
    /// A local variable's declaration, as a program variable's is written.
    Var(VarDecl),
}

/// The statements among `node`'s children.
fn statements_of(node: &SyntaxNode) -> impl Iterator<Item = Statement> + use<> {
    node.children().filter_map(|node| {
        CallStmt::cast(node.clone())
            .map(Statement::Call)
            .or_else(|| AssignStmt::cast(node.clone()).map(Statement::Assign))
            .or_else(|| AssignBlock::cast(node.clone()).map(Statement::Block))
            .or_else(|| ReturnStmt::cast(node.clone()).map(Statement::Return))
            .or_else(|| IfStmt::cast(node.clone()).map(Statement::If))
            .or_else(|| WhileStmt::cast(node.clone()).map(Statement::While))
            .or_else(|| LoopStmt::cast(node.clone()).map(Statement::Loop))
            .or_else(|| ForStmt::cast(node.clone()).map(Statement::For))
            .or_else(|| VarDecl::cast(node).map(Statement::Var))
    })
}

impl ValueBlock {
    pub fn statements(&self) -> impl Iterator<Item = Statement> {
        statements_of(&self.0)
    }

    /// The value, the expression the block ends with.
    #[must_use]
    pub fn value(&self) -> Option<Expr> {
        self.0.children().find_map(Expr::cast)
    }
}

impl Statement {
    #[must_use]
    pub fn syntax(&self) -> &SyntaxNode {
        match self {
            Statement::Call(s) => s.syntax(),
            Statement::Assign(s) => s.syntax(),
            Statement::Block(s) => s.syntax(),
            Statement::Return(s) => s.syntax(),
            Statement::If(s) => s.syntax(),
            Statement::While(s) => s.syntax(),
            Statement::Loop(s) => s.syntax(),
            Statement::For(s) => s.syntax(),
            Statement::Var(s) => s.syntax(),
        }
    }

    #[must_use]
    pub fn span(&self) -> Span {
        span(self.syntax().text_range())
    }
}

/// What follows an `if` statement's `else`: a block, or, for `else if`,
/// another `if`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ElseBranch {
    Block(Body),
    If(IfStmt),
}

impl WhileStmt {
    /// The `while`.
    #[must_use]
    pub fn keyword(&self) -> Option<SyntaxToken> {
        significant_tokens(&self.0).next()
    }

    /// The condition, between `while` and `loop`.
    #[must_use]
    pub fn condition(&self) -> Option<Expr> {
        self.0.children().find_map(Expr::cast)
    }

    /// The statements run while the condition is true.
    #[must_use]
    pub fn body(&self) -> Option<Body> {
        self.0.children().find_map(Body::cast)
    }
}

impl LoopStmt {
    /// The `loop`, or the word another language writes for it, which is
    /// reported: C's `do`.
    #[must_use]
    pub fn keyword(&self) -> Option<SyntaxToken> {
        significant_tokens(&self.0).next()
    }

    /// The statements run once, then again while the condition is true.
    #[must_use]
    pub fn body(&self) -> Option<Body> {
        self.0.children().find_map(Body::cast)
    }

    /// The `while` after the block.
    #[must_use]
    pub fn while_keyword(&self) -> Option<SyntaxToken> {
        significant_tokens(&self.0).find(|t| t.kind() == SyntaxKind::WhileKw)
    }

    /// The condition, after `while`.
    #[must_use]
    pub fn condition(&self) -> Option<Expr> {
        self.0.children().find_map(Expr::cast)
    }
}

impl ForStmt {
    /// The `for`, or the word another language writes for it, which is
    /// reported: C#'s `foreach`.
    #[must_use]
    pub fn keyword(&self) -> Option<SyntaxToken> {
        significant_tokens(&self.0).next()
    }

    /// The variable, which takes each element's value in turn.
    #[must_use]
    pub fn variable(&self) -> Option<Name> {
        self.0.children().find_map(Name::cast)
    }

    /// The array whose elements the loop goes through, after `in`.
    #[must_use]
    pub fn array(&self) -> Option<Expr> {
        self.0.children().find_map(Expr::cast)
    }

    /// The statements run for each element.
    #[must_use]
    pub fn body(&self) -> Option<Body> {
        self.0.children().find_map(Body::cast)
    }
}

impl IfStmt {
    /// The `if`, or the word another language writes for it, which is
    /// reported: Python's `elif` in an `else`.
    #[must_use]
    pub fn keyword(&self) -> Option<SyntaxToken> {
        significant_tokens(&self.0).next()
    }

    /// The condition, between `if` and `then`.
    #[must_use]
    pub fn condition(&self) -> Option<Expr> {
        self.0.children().find_map(Expr::cast)
    }

    /// The statements run if the condition is true.
    #[must_use]
    pub fn then_block(&self) -> Option<Body> {
        self.0.children().find_map(Body::cast)
    }

    /// What is run if the condition is false, if anything: what follows
    /// the block run if it is true.
    #[must_use]
    pub fn else_branch(&self) -> Option<ElseBranch> {
        self.0
            .children()
            .filter_map(|node| {
                Body::cast(node.clone())
                    .map(ElseBranch::Block)
                    .or_else(|| IfStmt::cast(node).map(ElseBranch::If))
            })
            .nth(1)
    }
}

impl ReturnStmt {
    /// The `return` keyword.
    #[must_use]
    pub fn keyword(&self) -> Option<SyntaxToken> {
        significant_tokens(&self.0).next()
    }

    /// The value returned.
    #[must_use]
    pub fn value(&self) -> Option<Expr> {
        self.0.children().find_map(Expr::cast)
    }

    /// The span to delete to remove the statement, as for
    /// [`AssignBlock::removal`].
    #[must_use]
    pub fn removal(&self) -> Span {
        statement_removal(&self.0)
    }
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
    /// `load a from r order [asc x]`
    Load(Load),
}

impl Clause {
    #[must_use]
    pub fn cast(node: SyntaxNode) -> Option<Self> {
        Assignment::cast(node.clone())
            .map(Clause::Assignment)
            .or_else(|| Insertion::cast(node.clone()).map(Clause::Insertion))
            .or_else(|| Deletion::cast(node.clone()).map(Clause::Deletion))
            .or_else(|| Update::cast(node.clone()).map(Clause::Update))
            .or_else(|| Load::cast(node).map(Clause::Load))
    }

    #[must_use]
    pub fn syntax(&self) -> &SyntaxNode {
        match self {
            Clause::Assignment(c) => c.syntax(),
            Clause::Insertion(c) => c.syntax(),
            Clause::Deletion(c) => c.syntax(),
            Clause::Update(c) => c.syntax(),
            Clause::Load(c) => c.syntax(),
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
    /// inserted, deleted or loaded, or the condition of a `delete` or an
    /// `update`.
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

impl Load {
    /// The order items, in brackets.
    #[must_use]
    pub fn order(&self) -> Option<OrderList> {
        self.0.children().find_map(OrderList::cast)
    }
}

impl OrderList {
    pub fn items(&self) -> impl Iterator<Item = OrderItem> {
        self.0.children().filter_map(OrderItem::cast)
    }

    /// The span to delete to remove `item` from the list, with a comma that
    /// separates it from a neighbor.
    #[must_use]
    pub fn removal(&self, item: &OrderItem) -> Span {
        list_removal(item.syntax())
    }
}

impl OrderItem {
    /// `asc` or `desc`.
    #[must_use]
    pub fn direction(&self) -> Option<SyntaxToken> {
        self.0
            .children_with_tokens()
            .filter_map(SyntaxElement::into_token)
            .find(|t| t.kind() == SyntaxKind::Ident)
    }

    /// Whether the direction is `desc`.
    #[must_use]
    pub fn descending(&self) -> bool {
        self.direction().is_some_and(|t| t.text() == "desc")
    }

    /// The attribute ordered by.
    #[must_use]
    pub fn attribute(&self) -> Option<Name> {
        self.0.children().find_map(Name::cast)
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

impl CallExpr {
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

impl IndexExpr {
    /// The array, before the brackets.
    #[must_use]
    pub fn operand(&self) -> Option<Expr> {
        self.0.children().find_map(Expr::cast)
    }

    /// The position, in the brackets.
    #[must_use]
    pub fn index(&self) -> Option<Expr> {
        self.0.children().filter_map(Expr::cast).nth(1)
    }

    /// The `[` that opens the brackets.
    #[must_use]
    pub fn l_bracket(&self) -> Option<SyntaxToken> {
        significant_tokens(&self.0).find(|t| t.kind() == SyntaxKind::LBracket)
    }

    /// The brackets and the position in them, from the `[` to the end.
    #[must_use]
    pub fn brackets(&self) -> Option<Span> {
        let open = self.l_bracket()?;
        Some(Span::new(span(open.text_range()).start, self.span().end))
    }
}

impl IfExpr {
    /// The `if`, or the word another language writes for it, which is
    /// reported: Python's `elif` in an `else` branch.
    #[must_use]
    pub fn keyword(&self) -> Option<SyntaxToken> {
        significant_tokens(&self.0).next()
    }

    /// The condition, between `if` and `then`.
    #[must_use]
    pub fn condition(&self) -> Option<Expr> {
        self.0.children().find_map(Expr::cast)
    }

    /// The value if the condition is true, between `then` and `else`.
    #[must_use]
    pub fn then_branch(&self) -> Option<Expr> {
        self.0.children().filter_map(Expr::cast).nth(1)
    }

    /// The value if the condition is false, after `else`.
    #[must_use]
    pub fn else_branch(&self) -> Option<Expr> {
        self.0.children().filter_map(Expr::cast).nth(2)
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

    /// Where a new item goes, as for [`TupleLiteral::insertion`].
    #[must_use]
    pub fn insertion(&self) -> (usize, &'static str, &'static str) {
        list_insertion(&self.0, self.names().last().map(|n| n.0))
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

/// A type as written: a name, a tuple type, a relation type, an array type,
/// or what a type parameter can be.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum TypeExpr {
    Named(TypeRef),
    Tuple(TupleType),
    Relation(RelationType),
    Array(ArrayType),
    Any(AnyType),
}

impl TypeExpr {
    #[must_use]
    pub fn cast(node: SyntaxNode) -> Option<Self> {
        TypeRef::cast(node.clone())
            .map(TypeExpr::Named)
            .or_else(|| TupleType::cast(node.clone()).map(TypeExpr::Tuple))
            .or_else(|| RelationType::cast(node.clone()).map(TypeExpr::Relation))
            .or_else(|| ArrayType::cast(node.clone()).map(TypeExpr::Array))
            .or_else(|| AnyType::cast(node).map(TypeExpr::Any))
    }

    #[must_use]
    pub fn syntax(&self) -> &SyntaxNode {
        match self {
            TypeExpr::Named(t) => t.syntax(),
            TypeExpr::Tuple(t) => t.syntax(),
            TypeExpr::Relation(t) => t.syntax(),
            TypeExpr::Array(t) => t.syntax(),
            TypeExpr::Any(t) => t.syntax(),
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
    Array(ArrayLiteral),
    Name(NameRef),
    Unary(UnaryExpr),
    Binary(BinaryExpr),
    Paren(ParenExpr),
    Extract(ExtractExpr),
    Index(IndexExpr),
    If(IfExpr),
    Project(ProjectExpr),
    Postfix(PostfixExpr),
    Nary(NaryExpr),
    Image(ImageExpr),
    From(FromExpr),
    Call(CallExpr),
    Block(ValueBlock),
}

impl Expr {
    #[must_use]
    pub fn cast(node: SyntaxNode) -> Option<Self> {
        Literal::cast(node.clone())
            .map(Expr::Literal)
            .or_else(|| TupleLiteral::cast(node.clone()).map(Expr::Tuple))
            .or_else(|| RelationLiteral::cast(node.clone()).map(Expr::Relation))
            .or_else(|| ArrayLiteral::cast(node.clone()).map(Expr::Array))
            .or_else(|| NameRef::cast(node.clone()).map(Expr::Name))
            .or_else(|| UnaryExpr::cast(node.clone()).map(Expr::Unary))
            .or_else(|| BinaryExpr::cast(node.clone()).map(Expr::Binary))
            .or_else(|| ParenExpr::cast(node.clone()).map(Expr::Paren))
            .or_else(|| ExtractExpr::cast(node.clone()).map(Expr::Extract))
            .or_else(|| IndexExpr::cast(node.clone()).map(Expr::Index))
            .or_else(|| IfExpr::cast(node.clone()).map(Expr::If))
            .or_else(|| ProjectExpr::cast(node.clone()).map(Expr::Project))
            .or_else(|| PostfixExpr::cast(node.clone()).map(Expr::Postfix))
            .or_else(|| NaryExpr::cast(node.clone()).map(Expr::Nary))
            .or_else(|| ImageExpr::cast(node.clone()).map(Expr::Image))
            .or_else(|| FromExpr::cast(node.clone()).map(Expr::From))
            .or_else(|| CallExpr::cast(node.clone()).map(Expr::Call))
            .or_else(|| ValueBlock::cast(node).map(Expr::Block))
    }

    #[must_use]
    pub fn syntax(&self) -> &SyntaxNode {
        match self {
            Expr::Literal(e) => e.syntax(),
            Expr::Tuple(e) => e.syntax(),
            Expr::Relation(e) => e.syntax(),
            Expr::Array(e) => e.syntax(),
            Expr::Name(e) => e.syntax(),
            Expr::Unary(e) => e.syntax(),
            Expr::Binary(e) => e.syntax(),
            Expr::Paren(e) => e.syntax(),
            Expr::Extract(e) => e.syntax(),
            Expr::Index(e) => e.syntax(),
            Expr::If(e) => e.syntax(),
            Expr::Project(e) => e.syntax(),
            Expr::Postfix(e) => e.syntax(),
            Expr::Nary(e) => e.syntax(),
            Expr::Image(e) => e.syntax(),
            Expr::From(e) => e.syntax(),
            Expr::Call(e) => e.syntax(),
            Expr::Block(e) => e.syntax(),
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

    /// The span to delete to remove `attribute`, with its comma.
    #[must_use]
    pub fn removal(&self, attribute: &AttributeType) -> Span {
        list_removal(attribute.syntax())
    }

    /// Every type written by name in it, nested ones too, in source order.
    pub fn type_refs(&self) -> impl Iterator<Item = TypeRef> + use<> {
        self.0.descendants().filter_map(TypeRef::cast)
    }
}

impl TupleType {
    #[must_use]
    pub fn heading(&self) -> Option<HeadingExpr> {
        self.0.children().find_map(HeadingExpr::cast)
    }
}

impl RelationType {
    #[must_use]
    pub fn heading(&self) -> Option<HeadingExpr> {
        self.0.children().find_map(HeadingExpr::cast)
    }
}

/// A heading as written after `relation` or `tuple`: its attributes, or the
/// name of a declared heading.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum HeadingExpr {
    Written(Heading),
    Named(HeadingRef),
}

impl HeadingExpr {
    #[must_use]
    pub fn cast(node: SyntaxNode) -> Option<Self> {
        Heading::cast(node.clone())
            .map(HeadingExpr::Written)
            .or_else(|| HeadingRef::cast(node).map(HeadingExpr::Named))
    }

    #[must_use]
    pub fn syntax(&self) -> &SyntaxNode {
        match self {
            HeadingExpr::Written(h) => h.syntax(),
            HeadingExpr::Named(h) => h.syntax(),
        }
    }

    #[must_use]
    pub fn span(&self) -> Span {
        span(self.syntax().text_range())
    }

    /// Where its attributes are written: here, or in the declaration of
    /// the heading it names.
    #[must_use]
    pub fn attributes_written(&self) -> Option<Heading> {
        match self {
            HeadingExpr::Written(heading) => Some(heading.clone()),
            HeadingExpr::Named(named) => named.declaration()?.heading(),
        }
    }
}

impl HeadingRef {
    #[must_use]
    pub fn ident(&self) -> Option<SyntaxToken> {
        ident_of(&self.0)
    }

    /// The declaration of the heading it names, in its file.
    #[must_use]
    pub fn declaration(&self) -> Option<HeadingDecl> {
        let file = SourceFile::cast(self.0.ancestors().last()?)?;
        file.heading(self.ident()?.text())
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

    /// `per r`, after a parameter's type.
    #[must_use]
    pub fn per(&self) -> Option<Per> {
        self.0.children().find_map(Per::cast)
    }

    /// `var` or `const`, before a parameter's name, or another language's
    /// mode, before the name or the type, which the parser reports.
    #[must_use]
    pub fn mode(&self) -> Option<Mode> {
        self.0.children().find_map(Mode::cast)
    }
}

impl Mode {
    /// The word, `var` or `const`, or another language's, such as `ref`.
    /// Ada's `in out` is two.
    pub fn words(&self) -> impl Iterator<Item = SyntaxToken> {
        significant_tokens(&self.0)
    }

    /// Whether it says `var`, as written or as another language writes it,
    /// as every mode but `const` and Ada's `in` does.
    #[must_use]
    pub fn is_var(&self) -> bool {
        let words: Vec<String> = self.words().map(|w| w.text().to_lowercase()).collect();
        !matches!(words.as_slice(), [word] if word == "const" || word == "in")
    }

    /// The span to delete to remove it, with the whitespace after it, up
    /// to the name it is before.
    #[must_use]
    pub fn removal(&self) -> Span {
        let end = std::iter::successors(
            self.0.next_sibling_or_token(),
            SyntaxElement::next_sibling_or_token,
        )
        .find(|e| !e.kind().is_trivia())
        .map_or(self.span().end, |e| span(e.text_range()).start);
        Span::new(self.span().start, end)
    }
}

impl Per {
    /// The word `per`.
    #[must_use]
    pub fn keyword(&self) -> Option<SyntaxToken> {
        significant_tokens(&self.0).next()
    }

    /// The parameter named after `per`, whose argument's tuples the
    /// argument is evaluated for.
    #[must_use]
    pub fn relation(&self) -> Option<NameRef> {
        self.0.children().find_map(NameRef::cast)
    }

    /// The span to delete to remove it, with the whitespace before.
    #[must_use]
    pub fn removal(&self) -> Span {
        Span::new(preceded_from(&self.0.clone().into()), self.span().end)
    }
}

impl TupleLiteral {
    /// The heading written before the attributes, as in
    /// `tuple { a: integer } { a: 1 }` or `tuple home { … }`.
    #[must_use]
    pub fn heading(&self) -> Option<HeadingExpr> {
        self.0.children().find_map(HeadingExpr::cast)
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

impl ArrayType {
    /// The elements' type, after `array`.
    #[must_use]
    pub fn element(&self) -> Option<TypeExpr> {
        self.0.children().find_map(TypeExpr::cast)
    }
}

impl ArrayLiteral {
    /// The `array` keyword, which a bare `[` lacks.
    #[must_use]
    pub fn keyword(&self) -> Option<SyntaxToken> {
        significant_tokens(&self.0).find(|t| t.kind() == SyntaxKind::ArrayKw)
    }

    /// The elements' type, written before them, as in `array integer []`.
    #[must_use]
    pub fn element_type(&self) -> Option<TypeExpr> {
        self.0.children().find_map(TypeExpr::cast)
    }

    /// The elements, in order.
    pub fn elements(&self) -> impl Iterator<Item = Expr> {
        self.0.children().filter_map(Expr::cast)
    }
}

impl RelationLiteral {
    /// The heading written before the tuples, as in
    /// `relation { a: integer } {}` or `relation home {}`.
    #[must_use]
    pub fn heading(&self) -> Option<HeadingExpr> {
        self.0.children().find_map(HeadingExpr::cast)
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

    /// `var` or `const`, before an argument's name, or another language's
    /// mode, before the name or the value, which the parser reports.
    #[must_use]
    pub fn mode(&self) -> Option<Mode> {
        self.0.children().find_map(Mode::cast)
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

impl AnyType {
    /// The word `any`.
    #[must_use]
    pub fn any(&self) -> Option<SyntaxToken> {
        significant_tokens(&self.0).find(|t| t.kind() == SyntaxKind::Ident && t.text() == "any")
    }

    /// The word for what it can be: `type`, `tuple` or `relation`, which
    /// follows `any`, or, as the parser reports, comes before it.
    #[must_use]
    pub fn what(&self) -> Option<SyntaxToken> {
        let any = self.any()?;
        significant_tokens(&self.0).find(|t| *t != any)
    }
}

impl TypeParameters {
    /// The word `types`, or how it is cased otherwise.
    #[must_use]
    pub fn keyword(&self) -> Option<SyntaxToken> {
        significant_tokens(&self.0).next()
    }

    /// The type parameters, `{ T: any type }`: a heading.
    #[must_use]
    pub fn heading(&self) -> Option<Heading> {
        self.0.children().find_map(Heading::cast)
    }

    /// The span to delete to remove them, with the whitespace before.
    #[must_use]
    pub fn removal(&self) -> Span {
        Span::new(preceded_from(&self.0.clone().into()), self.span().end)
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
