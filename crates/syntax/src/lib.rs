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
pub mod literal;

use std::fmt::Write;

use trendy_diag::Span;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u16)]
pub enum SyntaxKind {
    // Trivia: tokens with no grammatical meaning.
    Whitespace,
    /// `/* … */`, which nests: `/* a /* b */ c */` is one comment.
    Comment,

    // Tokens.
    Ident,
    OperatorKw,
    VarKw,
    /// `true`, a boolean literal.
    TrueKw,
    /// `false`, a boolean literal.
    FalseKw,
    TupleKw,
    RelationKw,
    CallKw,
    BuiltinKw,
    NotKw,
    AndKw,
    OrKw,
    XorKw,
    ImpliesKw,
    DivKw,
    ModKw,
    RemoveKw,
    RenameKw,
    JoinKw,
    TimesKw,
    ComposeKw,
    /// `as`, in `old as new`.
    AsKw,
    LBrace,
    RBrace,
    LBracket,
    RBracket,
    /// Not in the grammar yet; lexed so that C-style `()` can be recognized
    /// and fixed.
    LParen,
    RParen,
    Colon,
    Comma,
    /// `.`, in `t.a`.
    Dot,
    ColonEq,
    /// Equality, and recognized where `:=` is meant, with a fix.
    Eq,
    /// `<>`
    LtGt,
    Lt,
    LtEq,
    Gt,
    GtEq,
    Semicolon,
    Plus,
    Minus,
    Star,
    Slash,
    /// `||`, text concatenation.
    PipePipe,
    /// An operator from another language, such as `&&` or `->`: lexed so it
    /// can be recognized and replaced by trendy's keyword.
    ForeignOperator,
    /// Digits, possibly followed by other identifier characters. Malformed
    /// literals keep this kind, so one mistake gives one error.
    IntLiteral,
    /// Digits with a decimal point, an exponent, or both. Malformed literals
    /// keep this kind too.
    RationalLiteral,
    /// `"…"`, including an unterminated one.
    TextLiteral,
    /// A character the lexer does not recognize.
    ErrorToken,

    // Nodes.
    SourceFile,
    /// `operator name{params} [body];`
    Operator,
    /// `builtin operator name{params};`, only in the prelude.
    BuiltinOperator,
    /// `builtin type name;`, only in the prelude. `type` is a contextual
    /// keyword, an identifier elsewhere.
    BuiltinType,
    /// `var name: type := value;`
    VarDecl,
    Name,
    /// `[statement …]`
    Body,
    /// `call name{arguments};`
    CallStmt,
    /// `{name: value, …}`: a call's arguments, a tuple of its parameters.
    Arguments,
    /// An argument without a name, which is reported with a fix.
    PositionalArg,
    /// A name used as an expression, or as the operator in a call.
    NameRef,
    /// `not x`, `-x` or `+x`
    UnaryExpr,
    /// `x and y`, `x + y`, `r join s`, and the other dyadic operators.
    BinaryExpr,
    /// `(x)`
    ParenExpr,
    /// `t.a`: an attribute's value.
    ExtractExpr,
    /// `r{a, b}`: a projection.
    ProjectExpr,
    /// `r remove {a, b}` or `r rename {a as b}`
    PostfixExpr,
    /// `join{r, s, t}` or `times{r, s, t}`: a join of any number of
    /// relations.
    NaryExpr,
    /// `{a, b}`: attribute names, as in a projection.
    NameList,
    /// `{a as b, c as d}`, in a `rename`.
    RenameList,
    /// `a as b`
    Renaming,
    /// `: type`
    TypeAnnotation,
    TypeRef,
    /// `:= value`
    Initializer,
    /// `key { a, b }`, after a variable's type or value. `key` is a
    /// contextual keyword, an identifier elsewhere.
    Key,
    /// `{ name: type, … }`: the attributes of a tuple or relation type, or
    /// of a selector.
    Heading,
    /// `name: type`, in a heading.
    AttributeType,
    /// `tuple { name: type, … }`
    TupleType,
    /// `relation { name: type, … }`
    RelationType,
    /// `tuple [heading] { name: value, … }`; in a relation, `tuple` may be
    /// left out.
    TupleLiteral,
    /// `name: value`, in a tuple literal.
    AttributeValue,
    /// `relation [heading] { tuple, … }`
    RelationLiteral,
    Literal,
    /// Tokens the parser could not fit into the grammar.
    Error,
}

impl SyntaxKind {
    /// Every kind, indexed by its discriminant.
    pub const ALL: [SyntaxKind; 85] = [
        SyntaxKind::Whitespace,
        SyntaxKind::Comment,
        SyntaxKind::Ident,
        SyntaxKind::OperatorKw,
        SyntaxKind::VarKw,
        SyntaxKind::TrueKw,
        SyntaxKind::FalseKw,
        SyntaxKind::TupleKw,
        SyntaxKind::RelationKw,
        SyntaxKind::CallKw,
        SyntaxKind::BuiltinKw,
        SyntaxKind::NotKw,
        SyntaxKind::AndKw,
        SyntaxKind::OrKw,
        SyntaxKind::XorKw,
        SyntaxKind::ImpliesKw,
        SyntaxKind::DivKw,
        SyntaxKind::ModKw,
        SyntaxKind::RemoveKw,
        SyntaxKind::RenameKw,
        SyntaxKind::JoinKw,
        SyntaxKind::TimesKw,
        SyntaxKind::ComposeKw,
        SyntaxKind::AsKw,
        SyntaxKind::LBrace,
        SyntaxKind::RBrace,
        SyntaxKind::LBracket,
        SyntaxKind::RBracket,
        SyntaxKind::LParen,
        SyntaxKind::RParen,
        SyntaxKind::Colon,
        SyntaxKind::Comma,
        SyntaxKind::Dot,
        SyntaxKind::ColonEq,
        SyntaxKind::Eq,
        SyntaxKind::LtGt,
        SyntaxKind::Lt,
        SyntaxKind::LtEq,
        SyntaxKind::Gt,
        SyntaxKind::GtEq,
        SyntaxKind::Semicolon,
        SyntaxKind::Plus,
        SyntaxKind::Minus,
        SyntaxKind::Star,
        SyntaxKind::Slash,
        SyntaxKind::PipePipe,
        SyntaxKind::ForeignOperator,
        SyntaxKind::IntLiteral,
        SyntaxKind::RationalLiteral,
        SyntaxKind::TextLiteral,
        SyntaxKind::ErrorToken,
        SyntaxKind::SourceFile,
        SyntaxKind::Operator,
        SyntaxKind::BuiltinOperator,
        SyntaxKind::BuiltinType,
        SyntaxKind::VarDecl,
        SyntaxKind::Name,
        SyntaxKind::Body,
        SyntaxKind::CallStmt,
        SyntaxKind::Arguments,
        SyntaxKind::PositionalArg,
        SyntaxKind::NameRef,
        SyntaxKind::UnaryExpr,
        SyntaxKind::BinaryExpr,
        SyntaxKind::ParenExpr,
        SyntaxKind::ExtractExpr,
        SyntaxKind::ProjectExpr,
        SyntaxKind::PostfixExpr,
        SyntaxKind::NaryExpr,
        SyntaxKind::NameList,
        SyntaxKind::RenameList,
        SyntaxKind::Renaming,
        SyntaxKind::TypeAnnotation,
        SyntaxKind::TypeRef,
        SyntaxKind::Initializer,
        SyntaxKind::Key,
        SyntaxKind::Heading,
        SyntaxKind::AttributeType,
        SyntaxKind::TupleType,
        SyntaxKind::RelationType,
        SyntaxKind::TupleLiteral,
        SyntaxKind::AttributeValue,
        SyntaxKind::RelationLiteral,
        SyntaxKind::Literal,
        SyntaxKind::Error,
    ];

    #[must_use]
    pub fn is_trivia(self) -> bool {
        matches!(self, SyntaxKind::Whitespace | SyntaxKind::Comment)
    }

    #[must_use]
    pub fn is_keyword(self) -> bool {
        matches!(
            self,
            SyntaxKind::OperatorKw
                | SyntaxKind::VarKw
                | SyntaxKind::TrueKw
                | SyntaxKind::FalseKw
                | SyntaxKind::TupleKw
                | SyntaxKind::RelationKw
                | SyntaxKind::CallKw
                | SyntaxKind::BuiltinKw
                | SyntaxKind::NotKw
                | SyntaxKind::AndKw
                | SyntaxKind::OrKw
                | SyntaxKind::XorKw
                | SyntaxKind::ImpliesKw
                | SyntaxKind::DivKw
                | SyntaxKind::ModKw
                | SyntaxKind::RemoveKw
                | SyntaxKind::RenameKw
                | SyntaxKind::JoinKw
                | SyntaxKind::TimesKw
                | SyntaxKind::ComposeKw
                | SyntaxKind::AsKw
        )
    }

    /// Whether this is a keyword that is an operator, such as `and`.
    #[must_use]
    pub fn is_operator_keyword(self) -> bool {
        matches!(
            self,
            SyntaxKind::NotKw
                | SyntaxKind::AndKw
                | SyntaxKind::OrKw
                | SyntaxKind::XorKw
                | SyntaxKind::ImpliesKw
                | SyntaxKind::DivKw
                | SyntaxKind::ModKw
                | SyntaxKind::RemoveKw
                | SyntaxKind::RenameKw
                | SyntaxKind::JoinKw
                | SyntaxKind::TimesKw
                | SyntaxKind::ComposeKw
        )
    }

    /// Whether this is an operator: an operator keyword, or a symbol such as
    /// `+`.
    #[must_use]
    pub fn is_operator(self) -> bool {
        self.is_operator_keyword()
            || matches!(
                self,
                SyntaxKind::Plus
                    | SyntaxKind::Minus
                    | SyntaxKind::Star
                    | SyntaxKind::Slash
                    | SyntaxKind::PipePipe
            )
            || self.is_comparison()
    }

    /// Whether this is a comparison operator, such as `<`.
    #[must_use]
    pub fn is_comparison(self) -> bool {
        matches!(
            self,
            SyntaxKind::Eq
                | SyntaxKind::LtGt
                | SyntaxKind::Lt
                | SyntaxKind::LtEq
                | SyntaxKind::Gt
                | SyntaxKind::GtEq
        )
    }

    /// Whether this is a join: `join`, `times` or `compose`, which all
    /// match tuples on their common attributes.
    #[must_use]
    pub fn is_join(self) -> bool {
        matches!(
            self,
            SyntaxKind::JoinKw | SyntaxKind::TimesKw | SyntaxKind::ComposeKw
        )
    }

    /// Whether this kind is a literal token: a number, text, or a boolean
    /// keyword.
    #[must_use]
    pub fn is_literal(self) -> bool {
        matches!(
            self,
            SyntaxKind::IntLiteral
                | SyntaxKind::RationalLiteral
                | SyntaxKind::TextLiteral
                | SyntaxKind::TrueKw
                | SyntaxKind::FalseKw
        )
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
            SyntaxKind::VarKw => "var",
            SyntaxKind::TrueKw => "true",
            SyntaxKind::FalseKw => "false",
            SyntaxKind::TupleKw => "tuple",
            SyntaxKind::RelationKw => "relation",
            SyntaxKind::CallKw => "call",
            SyntaxKind::BuiltinKw => "builtin",
            SyntaxKind::NotKw => "not",
            SyntaxKind::AndKw => "and",
            SyntaxKind::OrKw => "or",
            SyntaxKind::XorKw => "xor",
            SyntaxKind::ImpliesKw => "implies",
            SyntaxKind::DivKw => "div",
            SyntaxKind::ModKw => "mod",
            SyntaxKind::RemoveKw => "remove",
            SyntaxKind::RenameKw => "rename",
            SyntaxKind::JoinKw => "join",
            SyntaxKind::TimesKw => "times",
            SyntaxKind::ComposeKw => "compose",
            SyntaxKind::AsKw => "as",
            SyntaxKind::LBrace => "{",
            SyntaxKind::RBrace => "}",
            SyntaxKind::LBracket => "[",
            SyntaxKind::RBracket => "]",
            SyntaxKind::LParen => "(",
            SyntaxKind::RParen => ")",
            SyntaxKind::Colon => ":",
            SyntaxKind::Comma => ",",
            SyntaxKind::Dot => ".",
            SyntaxKind::ColonEq => ":=",
            SyntaxKind::Eq => "=",
            SyntaxKind::LtGt => "<>",
            SyntaxKind::Lt => "<",
            SyntaxKind::LtEq => "<=",
            SyntaxKind::Gt => ">",
            SyntaxKind::GtEq => ">=",
            SyntaxKind::Semicolon => ";",
            SyntaxKind::Plus => "+",
            SyntaxKind::Minus => "-",
            SyntaxKind::Star => "*",
            SyntaxKind::Slash => "/",
            SyntaxKind::PipePipe => "||",
            _ => return None,
        })
    }

    /// How the kind is named in error messages.
    #[must_use]
    pub fn describe(self) -> &'static str {
        match self {
            SyntaxKind::Whitespace => "whitespace",
            SyntaxKind::Comment => "comment",
            SyntaxKind::Ident => "identifier",
            SyntaxKind::OperatorKw => "`operator`",
            SyntaxKind::VarKw => "`var`",
            SyntaxKind::TrueKw => "`true`",
            SyntaxKind::FalseKw => "`false`",
            SyntaxKind::TupleKw => "`tuple`",
            SyntaxKind::RelationKw => "`relation`",
            SyntaxKind::CallKw => "`call`",
            SyntaxKind::BuiltinKw => "`builtin`",
            SyntaxKind::NotKw => "`not`",
            SyntaxKind::AndKw => "`and`",
            SyntaxKind::OrKw => "`or`",
            SyntaxKind::XorKw => "`xor`",
            SyntaxKind::ImpliesKw => "`implies`",
            SyntaxKind::DivKw => "`div`",
            SyntaxKind::ModKw => "`mod`",
            SyntaxKind::RemoveKw => "`remove`",
            SyntaxKind::RenameKw => "`rename`",
            SyntaxKind::JoinKw => "`join`",
            SyntaxKind::TimesKw => "`times`",
            SyntaxKind::ComposeKw => "`compose`",
            SyntaxKind::AsKw => "`as`",
            SyntaxKind::ForeignOperator => "operator",
            SyntaxKind::LBrace => "`{`",
            SyntaxKind::RBrace => "`}`",
            SyntaxKind::LBracket => "`[`",
            SyntaxKind::RBracket => "`]`",
            SyntaxKind::LParen => "`(`",
            SyntaxKind::RParen => "`)`",
            SyntaxKind::Colon => "`:`",
            SyntaxKind::Comma => "`,`",
            SyntaxKind::Dot => "`.`",
            SyntaxKind::ColonEq => "`:=`",
            SyntaxKind::Eq => "`=`",
            SyntaxKind::LtGt => "`<>`",
            SyntaxKind::Lt => "`<`",
            SyntaxKind::LtEq => "`<=`",
            SyntaxKind::Gt => "`>`",
            SyntaxKind::GtEq => "`>=`",
            SyntaxKind::Semicolon => "`;`",
            SyntaxKind::Plus => "`+`",
            SyntaxKind::Minus => "`-`",
            SyntaxKind::Star => "`*`",
            SyntaxKind::Slash => "`/`",
            SyntaxKind::PipePipe => "`||`",
            SyntaxKind::IntLiteral => "integer literal",
            SyntaxKind::RationalLiteral => "rational literal",
            SyntaxKind::TextLiteral => "text literal",
            SyntaxKind::ErrorToken => "unrecognized character",
            SyntaxKind::SourceFile => "source file",
            SyntaxKind::Operator => "operator definition",
            SyntaxKind::BuiltinOperator => "built-in operator declaration",
            SyntaxKind::BuiltinType => "built-in type declaration",
            SyntaxKind::VarDecl => "variable declaration",
            SyntaxKind::Name | SyntaxKind::NameRef => "name",
            SyntaxKind::UnaryExpr
            | SyntaxKind::BinaryExpr
            | SyntaxKind::ParenExpr
            | SyntaxKind::ExtractExpr
            | SyntaxKind::ProjectExpr
            | SyntaxKind::PostfixExpr
            | SyntaxKind::NaryExpr => "expression",
            SyntaxKind::NameList => "attribute names",
            SyntaxKind::RenameList => "renamings",
            SyntaxKind::Renaming => "renaming",
            SyntaxKind::Body => "body",
            SyntaxKind::CallStmt => "call",
            SyntaxKind::Arguments => "arguments",
            SyntaxKind::PositionalArg => "argument",
            SyntaxKind::TypeAnnotation => "type annotation",
            SyntaxKind::TypeRef => "type",
            SyntaxKind::Initializer => "initial value",
            SyntaxKind::Key => "key",
            SyntaxKind::Heading => "heading",
            SyntaxKind::TupleType => "tuple type",
            SyntaxKind::RelationType => "relation type",
            SyntaxKind::TupleLiteral => "tuple value",
            SyntaxKind::RelationLiteral => "relation value",
            SyntaxKind::AttributeType | SyntaxKind::AttributeValue => "attribute",
            SyntaxKind::Literal => "value",
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

/// A position in a [`TreeBuilder`], from [`TreeBuilder::checkpoint`].
#[derive(Debug, Clone, Copy)]
pub struct Checkpoint(rowan::Checkpoint);

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

    /// A position to start a node at later, around what is built after it.
    #[must_use]
    pub fn checkpoint(&self) -> Checkpoint {
        Checkpoint(self.0.checkpoint())
    }

    /// Start a node at `checkpoint`, so that it contains everything built
    /// since then: how a binary expression wraps its left operand.
    pub fn start_node_at(&mut self, checkpoint: Checkpoint, kind: SyntaxKind) {
        self.0
            .start_node_at(checkpoint.0, <Trendy as rowan::Language>::kind_to_raw(kind));
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
