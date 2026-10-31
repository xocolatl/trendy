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

/// How tightly a dyadic operator binds: an operator binds tighter than one
/// of a lower precedence, so `a or b and c` is `a or (b and c)`. All group
/// to the left but `implies`, and the comparisons don't chain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Precedence(u8);

impl Precedence {
    /// `where`, the loosest: its condition is all that follows.
    pub const WHERE: Precedence = Precedence(0);
    /// `implies`, which groups to the right.
    pub const IMPLIES: Precedence = Precedence(1);
    pub const OR: Precedence = Precedence(2);
    pub const XOR: Precedence = Precedence(3);
    pub const AND: Precedence = Precedence(4);
    /// `=`, `<>`, `<`, `<=`, `>` and `>=`.
    pub const COMPARISON: Precedence = Precedence(5);
    /// `union`, `d_union` and `xunion`: Algebra A's OR, looser than the AND
    /// of the joins, as `or` is than `and`.
    pub const UNION: Precedence = Precedence(6);
    /// The joins, `join`, `times`, `compose` and the matchings, and
    /// `intersect`, `minus` and `i_minus`, which are AND too.
    pub const JOIN: Precedence = Precedence(7);
    /// `+`, `-` and `||`.
    pub const ADDITION: Precedence = Precedence(8);
    /// `*`, `/`, `div` and `mod`.
    pub const MULTIPLICATION: Precedence = Precedence(9);
    /// The tightest of all.
    pub const TIGHTEST: Precedence = Precedence::MULTIPLICATION;

    /// The next precedence up, which the right operand of an operator of
    /// this one that groups to the left binds at least as tightly as.
    #[must_use]
    pub fn tighter(self) -> Precedence {
        Precedence(self.0 + 1)
    }
}

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
    ArrayKw,
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
    ExtendKw,
    ReplaceKw,
    GroupKw,
    UngroupKw,
    WrapKw,
    UnwrapKw,
    JoinKw,
    TimesKw,
    ComposeKw,
    /// `matching`, in `r matching s` and `r not matching s`.
    MatchingKw,
    UnionKw,
    /// `d_union`, disjoint union.
    DUnionKw,
    /// `xunion`, exclusive union: the tuples of exactly one operand.
    XUnionKw,
    IntersectKw,
    MinusKw,
    /// `i_minus`, included minus.
    IMinusKw,
    /// `where`, in `r where condition`.
    WhereKw,
    /// `as`, in `old as new`.
    AsKw,
    /// `insert`, in `insert r s;`.
    InsertKw,
    /// `d_insert`, disjoint insert: an `insert` that fails if a tuple is
    /// already there.
    DInsertKw,
    /// `delete`, in `delete r where p;`.
    DeleteKw,
    /// `i_delete`, included delete: it fails if a tuple isn't there.
    IDeleteKw,
    /// `update`, in `update r { a := x } where p;`.
    UpdateKw,
    /// `assign`, in `assign { x := y; y := x; };`.
    AssignKw,
    /// `return`, for leaving an operator early.
    ReturnKw,
    /// `if`, in `if condition then value else value`.
    IfKw,
    /// `then`, in an `if`.
    ThenKw,
    /// `else`, in an `if`.
    ElseKw,
    /// `while`, before a loop's condition.
    WhileKw,
    /// `loop`, before a loop's block.
    LoopKw,
    /// `for`, before a `for` loop's variable.
    ForKw,
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
    /// `!!`, in `!!r`: an image relation.
    BangBang,
    /// An operator from another language, such as `&&` or `->`: lexed so it
    /// can be recognized and replaced by trendy's keyword.
    ForeignOperator,
    /// Digits, possibly followed by other identifier characters. Malformed
    /// literals keep this kind, so one mistake gives one error.
    IntLiteral,
    /// Digits with an exponent, and optionally a decimal point. A number
    /// with a point but no exponent, such as `1.5`, is a malformed one, and
    /// malformed literals keep this kind too.
    ApproximateLiteral,
    /// `"…"`, including an unterminated one.
    TextLiteral,
    /// A character the lexer does not recognize.
    ErrorToken,

    // Nodes.
    SourceFile,
    /// `operator name{params} [body];`, or, for a read-only operator,
    /// `operator name{params}: type [value];`.
    Operator,
    /// `builtin operator name{params};`, or, for a read-only one,
    /// `builtin operator name{params}: type;`, only in the prelude.
    BuiltinOperator,
    /// `builtin type name;`, or `builtin type name: { kind, … };` with the
    /// kinds it is of, only in the prelude. `type` is a contextual keyword,
    /// an identifier elsewhere.
    BuiltinType,
    /// `kind name { requirement, … };`: a set of types, each of which
    /// declares it, and what each must have, such as `<`. `kind` is a
    /// contextual keyword, an identifier elsewhere.
    KindDecl,
    /// `heading name := { name: type, … };`: a heading with a name, which
    /// stands for it after `relation` and `tuple`. `heading` is a
    /// contextual keyword, an identifier elsewhere.
    HeadingDecl,
    /// `type name { { name: type, … } };`: a scalar type of the program's
    /// own, with its possible representation. `type` is a contextual
    /// keyword, an identifier elsewhere.
    TypeDecl,
    /// `{ { name: type, … }, constraint … }`: what a type declaration says
    /// of its type, its possible representation and its constraints.
    TypeBody,
    /// `{ name: type, … }`: a possible representation of a type, its
    /// components, and, but for the one its values are stored as, its
    /// conversions to and from that one.
    Representation,
    /// `to { stored := … }` or `from { this := … }`: how a possible
    /// representation converts to the stored one, or from it. `to` and
    /// `from` are contextual keywords, identifiers elsewhere.
    Conversion,
    /// `constraint condition`: a condition every value of a type satisfies.
    /// `constraint` is a contextual keyword, an identifier elsewhere.
    Constraint,
    /// `order [asc c, …]`: the order of a type's values, by their
    /// components, in turn. `order` is a contextual keyword, an identifier
    /// elsewhere.
    TypeOrder,
    /// `{ <, … }`: what a kind's types must have.
    RequirementList,
    /// One thing a kind's types must have: an operator, such as `<`.
    Requirement,
    /// `var name: type := value;`
    VarDecl,
    Name,
    /// `[statement …]`: an operator's body, or a branch of an `if`
    /// statement.
    Body,
    /// `call name{arguments};`
    CallStmt,
    /// `a := x;` or `insert r s;`: an assignment. Several separated by
    /// commas are parsed, so that they can be reported.
    AssignStmt,
    /// `assign { a := x; insert r s; };`: assignments all made at once
    /// (TTM RM Prescription 21).
    AssignBlock,
    /// `return;` or `return value;`: leaving an operator early.
    ReturnStmt,
    /// `if c then [ … ] else [ … ];`: statements run if `c` is true, and
    /// else others, if any. An `else if` is one in another's `else`, with
    /// no `;` of its own.
    IfStmt,
    /// `while c loop [ … ];`: statements run again and again while `c` is
    /// true, checked before each run.
    WhileStmt,
    /// `loop [ … ] while c;`: statements run, then again while `c` is
    /// true, checked after each run.
    LoopStmt,
    /// `for x in a loop [ … ];`: statements run for each element of the
    /// array `a`, in order, with `x` its value. `in` is a contextual
    /// keyword, an identifier elsewhere.
    ForStmt,
    /// `v := x`, in an assignment statement.
    Assignment,
    /// `insert r s` or `d_insert r s`, in an assignment statement.
    Insertion,
    /// `delete r where p` or `i_delete r s`, in an assignment statement.
    Deletion,
    /// `update r { a := x } where p`, in an assignment statement; for a
    /// tuple, without `where`.
    Update,
    /// `load a from r order [asc x, desc y]`, in an assignment statement:
    /// an array given a relation's tuples, or their values, in order.
    /// `load`, `from` and `order` are contextual keywords, identifiers
    /// elsewhere.
    Load,
    /// `[asc x, desc y]`: the order items of a `load`.
    OrderList,
    /// `remove {x}` or `keep {x}`, after a `load`'s order: which of its
    /// tuples' attributes the elements leave out, or keep. `keep` is a
    /// contextual keyword, an identifier elsewhere.
    Keeping,
    /// `as x`, after the array a `load` gives a relation: the attribute
    /// each element becomes.
    ElementName,
    /// `numbered n`, after the array a `load` gives a relation: the
    /// attribute each element's position becomes. `numbered` is a
    /// contextual keyword, an identifier elsewhere.
    Numbering,
    /// `where c`, in a `load`: the tuples it keeps.
    LoadCondition,
    /// `asc x` or `desc x`: an attribute a `load` orders by, and the
    /// direction, a contextual keyword.
    OrderItem,
    /// `{name: value, …}`: a call's arguments, a tuple of its parameters.
    Arguments,
    /// An argument without a name, which is reported with a fix.
    PositionalArg,
    /// A name used as an expression, or as the operator in a call.
    NameRef,
    /// `not x`, `-x` or `+x`
    UnaryExpr,
    /// `x and y`, `x + y`, `r join s`, `r where p`, and the other dyadic
    /// operators.
    BinaryExpr,
    /// `(x)`
    ParenExpr,
    /// `t.a`: an attribute's value.
    ExtractExpr,
    /// `a[i]`: an array's element at position `i`, counting from 1.
    IndexExpr,
    /// `if c then a else b`: `a` if `c` is true, and else `b`.
    IfExpr,
    /// `[ var x := …; x * 2 ]`: statements, then the value they compute,
    /// as a read-only operator's body is.
    ValueBlock,
    /// `r{a, b}`: a projection.
    ProjectExpr,
    /// `r remove {a, b}`, `r rename {a as b}`, `r extend { c := a + b }`,
    /// `r replace { a := a + 1 }`, `r group { g := {a, b} }`,
    /// `r ungroup {g}`, `r wrap { w := {a, b} }` or `r unwrap {w}`
    PostfixExpr,
    /// `join{r, s, t}`, `union{r, s, t}`, `compose[r, s, t]` and the like:
    /// an operator of any number of operands.
    NaryExpr,
    /// `!!r`: the tuples of `r` that match the tuple at hand.
    ImageExpr,
    /// `tuple from r` or `value from r`: the only tuple of a relation, or
    /// the only value of its only tuple. `value` and `from` are contextual
    /// keywords, identifiers elsewhere.
    FromExpr,
    /// `name{arguments}`: a call of a read-only operator, whose value it is.
    /// `name{}` is a projection to the parser, and sema makes it a call when
    /// `name` is an operator.
    CallExpr,
    /// `x.f{a: 1}` or `x.f("a")`: arguments after an attribute's value, as
    /// other languages call a method, which sema reports. `x.f{}` and
    /// `x.f{a}` are projections to the parser.
    MethodCallExpr,
    /// `{a, b}`: attribute names, as in a projection.
    NameList,
    /// `{a as b, c as d}`, in a `rename`.
    RenameList,
    /// `a as b`
    Renaming,
    /// `{ g := {a, b}, h := {c} }`, in a `group` or a `wrap`.
    GatherList,
    /// `g := {a, b}`: a new attribute, relation-valued in a `group` and
    /// tuple-valued in a `wrap`, and the attributes it gathers.
    Gathering,
    /// `{ a := x, b := y }`, in an `extend` or a `replace`.
    AssignList,
    /// `a := x`: an attribute and its value, new in an `extend`, replacing
    /// the old one in a `replace`.
    Assign,
    /// `: type`
    TypeAnnotation,
    TypeRef,
    /// `any type`, `any tuple` or `any relation`: a parameter's type that
    /// can be any type of what it says, which each call gives it. `any`
    /// and `type` are contextual keywords, identifiers elsewhere.
    AnyType,
    /// `types { T: any type, … }`: an operator's type parameters, after
    /// its parameters and result type. `types` is a contextual keyword, an
    /// identifier elsewhere.
    TypeParameters,
    /// `:= value`
    Initializer,
    /// `key { a, b }`, after a variable's type or value. `key` is a
    /// contextual keyword, an identifier elsewhere.
    Key,
    /// `{ name: type, … }`: the attributes of a tuple or relation type, or
    /// of a selector.
    Heading,
    /// A declared heading's name, after `relation` or `tuple`, in place of
    /// the attributes it stands for.
    HeadingRef,
    /// `name: type`, in a heading.
    AttributeType,
    /// `per r`, after a parameter's type: its argument is evaluated for each
    /// tuple of the argument for `r`. `per` is a contextual keyword, an
    /// identifier elsewhere.
    Per,
    /// `var`, `const` or `out` before a parameter's name, saying how it
    /// takes its argument, or before an argument's name, saying how it is
    /// given. `const` and `out` are contextual keywords, identifiers
    /// elsewhere.
    Mode,
    /// `tuple { name: type, … }`
    TupleType,
    /// `relation { name: type, … }`
    RelationType,
    /// `array type`
    ArrayType,
    /// `tuple [heading] { name: value, … }`; in a relation, `tuple` may be
    /// left out.
    TupleLiteral,
    /// `name: value`, in a tuple literal.
    AttributeValue,
    /// `relation [heading] { tuple, … }`
    RelationLiteral,
    /// `array [type] [ element, … ]`
    ArrayLiteral,
    Literal,
    /// Tokens the parser could not fit into the grammar.
    Error,
}

impl SyntaxKind {
    /// Every kind, indexed by its discriminant.
    pub const ALL: [SyntaxKind; 160] = [
        SyntaxKind::Whitespace,
        SyntaxKind::Comment,
        SyntaxKind::Ident,
        SyntaxKind::OperatorKw,
        SyntaxKind::VarKw,
        SyntaxKind::TrueKw,
        SyntaxKind::FalseKw,
        SyntaxKind::TupleKw,
        SyntaxKind::RelationKw,
        SyntaxKind::ArrayKw,
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
        SyntaxKind::ExtendKw,
        SyntaxKind::ReplaceKw,
        SyntaxKind::GroupKw,
        SyntaxKind::UngroupKw,
        SyntaxKind::WrapKw,
        SyntaxKind::UnwrapKw,
        SyntaxKind::JoinKw,
        SyntaxKind::TimesKw,
        SyntaxKind::ComposeKw,
        SyntaxKind::MatchingKw,
        SyntaxKind::UnionKw,
        SyntaxKind::DUnionKw,
        SyntaxKind::XUnionKw,
        SyntaxKind::IntersectKw,
        SyntaxKind::MinusKw,
        SyntaxKind::IMinusKw,
        SyntaxKind::WhereKw,
        SyntaxKind::AsKw,
        SyntaxKind::InsertKw,
        SyntaxKind::DInsertKw,
        SyntaxKind::DeleteKw,
        SyntaxKind::IDeleteKw,
        SyntaxKind::UpdateKw,
        SyntaxKind::AssignKw,
        SyntaxKind::ReturnKw,
        SyntaxKind::IfKw,
        SyntaxKind::ThenKw,
        SyntaxKind::ElseKw,
        SyntaxKind::WhileKw,
        SyntaxKind::LoopKw,
        SyntaxKind::ForKw,
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
        SyntaxKind::BangBang,
        SyntaxKind::ForeignOperator,
        SyntaxKind::IntLiteral,
        SyntaxKind::ApproximateLiteral,
        SyntaxKind::TextLiteral,
        SyntaxKind::ErrorToken,
        SyntaxKind::SourceFile,
        SyntaxKind::Operator,
        SyntaxKind::BuiltinOperator,
        SyntaxKind::BuiltinType,
        SyntaxKind::KindDecl,
        SyntaxKind::HeadingDecl,
        SyntaxKind::TypeDecl,
        SyntaxKind::TypeBody,
        SyntaxKind::Representation,
        SyntaxKind::Conversion,
        SyntaxKind::Constraint,
        SyntaxKind::TypeOrder,
        SyntaxKind::RequirementList,
        SyntaxKind::Requirement,
        SyntaxKind::VarDecl,
        SyntaxKind::Name,
        SyntaxKind::Body,
        SyntaxKind::CallStmt,
        SyntaxKind::AssignStmt,
        SyntaxKind::AssignBlock,
        SyntaxKind::ReturnStmt,
        SyntaxKind::IfStmt,
        SyntaxKind::WhileStmt,
        SyntaxKind::LoopStmt,
        SyntaxKind::ForStmt,
        SyntaxKind::Assignment,
        SyntaxKind::Insertion,
        SyntaxKind::Deletion,
        SyntaxKind::Update,
        SyntaxKind::Load,
        SyntaxKind::OrderList,
        SyntaxKind::Keeping,
        SyntaxKind::ElementName,
        SyntaxKind::Numbering,
        SyntaxKind::LoadCondition,
        SyntaxKind::OrderItem,
        SyntaxKind::Arguments,
        SyntaxKind::PositionalArg,
        SyntaxKind::NameRef,
        SyntaxKind::UnaryExpr,
        SyntaxKind::BinaryExpr,
        SyntaxKind::ParenExpr,
        SyntaxKind::ExtractExpr,
        SyntaxKind::IndexExpr,
        SyntaxKind::IfExpr,
        SyntaxKind::ValueBlock,
        SyntaxKind::ProjectExpr,
        SyntaxKind::PostfixExpr,
        SyntaxKind::NaryExpr,
        SyntaxKind::ImageExpr,
        SyntaxKind::FromExpr,
        SyntaxKind::CallExpr,
        SyntaxKind::MethodCallExpr,
        SyntaxKind::NameList,
        SyntaxKind::RenameList,
        SyntaxKind::Renaming,
        SyntaxKind::GatherList,
        SyntaxKind::Gathering,
        SyntaxKind::AssignList,
        SyntaxKind::Assign,
        SyntaxKind::TypeAnnotation,
        SyntaxKind::TypeRef,
        SyntaxKind::AnyType,
        SyntaxKind::TypeParameters,
        SyntaxKind::Initializer,
        SyntaxKind::Key,
        SyntaxKind::Heading,
        SyntaxKind::HeadingRef,
        SyntaxKind::AttributeType,
        SyntaxKind::Per,
        SyntaxKind::Mode,
        SyntaxKind::TupleType,
        SyntaxKind::RelationType,
        SyntaxKind::ArrayType,
        SyntaxKind::TupleLiteral,
        SyntaxKind::AttributeValue,
        SyntaxKind::RelationLiteral,
        SyntaxKind::ArrayLiteral,
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
                | SyntaxKind::ArrayKw
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
                | SyntaxKind::ExtendKw
                | SyntaxKind::ReplaceKw
                | SyntaxKind::GroupKw
                | SyntaxKind::UngroupKw
                | SyntaxKind::WrapKw
                | SyntaxKind::UnwrapKw
                | SyntaxKind::JoinKw
                | SyntaxKind::TimesKw
                | SyntaxKind::ComposeKw
                | SyntaxKind::MatchingKw
                | SyntaxKind::UnionKw
                | SyntaxKind::DUnionKw
                | SyntaxKind::XUnionKw
                | SyntaxKind::IntersectKw
                | SyntaxKind::MinusKw
                | SyntaxKind::IMinusKw
                | SyntaxKind::WhereKw
                | SyntaxKind::AsKw
                | SyntaxKind::InsertKw
                | SyntaxKind::DInsertKw
                | SyntaxKind::DeleteKw
                | SyntaxKind::IDeleteKw
                | SyntaxKind::UpdateKw
                | SyntaxKind::AssignKw
                | SyntaxKind::ReturnKw
                | SyntaxKind::IfKw
                | SyntaxKind::ThenKw
                | SyntaxKind::ElseKw
                | SyntaxKind::WhileKw
                | SyntaxKind::LoopKw
                | SyntaxKind::ForKw
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
                | SyntaxKind::ExtendKw
                | SyntaxKind::ReplaceKw
                | SyntaxKind::GroupKw
                | SyntaxKind::UngroupKw
                | SyntaxKind::WrapKw
                | SyntaxKind::UnwrapKw
                | SyntaxKind::JoinKw
                | SyntaxKind::TimesKw
                | SyntaxKind::ComposeKw
                | SyntaxKind::MatchingKw
                | SyntaxKind::UnionKw
                | SyntaxKind::DUnionKw
                | SyntaxKind::XUnionKw
                | SyntaxKind::IntersectKw
                | SyntaxKind::MinusKw
                | SyntaxKind::IMinusKw
                | SyntaxKind::WhereKw
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
                    | SyntaxKind::BangBang
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

    /// Whether this is a set operator: `union`, `d_union`, `xunion`,
    /// `intersect`, `minus` or `i_minus`, of relations with the same heading.
    #[must_use]
    pub fn is_set_operator(self) -> bool {
        matches!(
            self,
            SyntaxKind::UnionKw
                | SyntaxKind::DUnionKw
                | SyntaxKind::XUnionKw
                | SyntaxKind::IntersectKw
                | SyntaxKind::MinusKw
                | SyntaxKind::IMinusKw
        )
    }

    /// Whether this is a join: `join`, `times`, `compose` or `matching`,
    /// which all match tuples on their common attributes.
    #[must_use]
    pub fn is_join(self) -> bool {
        matches!(
            self,
            SyntaxKind::JoinKw
                | SyntaxKind::TimesKw
                | SyntaxKind::ComposeKw
                | SyntaxKind::MatchingKw
        )
    }

    /// How tightly this binds as a dyadic operator, if it is one. `-` is
    /// one too, as well as a prefix operator; `not` is only the prefix
    /// one, and `not matching` binds as `matching` does.
    #[must_use]
    pub fn precedence(self) -> Option<Precedence> {
        Some(match self {
            SyntaxKind::WhereKw => Precedence::WHERE,
            SyntaxKind::ImpliesKw => Precedence::IMPLIES,
            SyntaxKind::OrKw => Precedence::OR,
            SyntaxKind::XorKw => Precedence::XOR,
            SyntaxKind::AndKw => Precedence::AND,
            kind if kind.is_comparison() => Precedence::COMPARISON,
            SyntaxKind::UnionKw | SyntaxKind::DUnionKw | SyntaxKind::XUnionKw => Precedence::UNION,
            kind if kind.is_join() => Precedence::JOIN,
            SyntaxKind::IntersectKw | SyntaxKind::MinusKw | SyntaxKind::IMinusKw => {
                Precedence::JOIN
            }
            SyntaxKind::Plus | SyntaxKind::Minus | SyntaxKind::PipePipe => Precedence::ADDITION,
            SyntaxKind::Star | SyntaxKind::Slash | SyntaxKind::DivKw | SyntaxKind::ModKw => {
                Precedence::MULTIPLICATION
            }
            _ => return None,
        })
    }

    /// Whether this keyword starts a shorthand for an assignment: `insert`,
    /// `d_insert`, `delete`, `i_delete` or `update`.
    #[must_use]
    pub fn is_assignment_keyword(self) -> bool {
        matches!(
            self,
            SyntaxKind::InsertKw
                | SyntaxKind::DInsertKw
                | SyntaxKind::DeleteKw
                | SyntaxKind::IDeleteKw
                | SyntaxKind::UpdateKw
        )
    }

    /// Whether this kind is a literal token: a number, text, or a boolean
    /// keyword.
    #[must_use]
    pub fn is_literal(self) -> bool {
        matches!(
            self,
            SyntaxKind::IntLiteral
                | SyntaxKind::ApproximateLiteral
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
            SyntaxKind::ArrayKw => "array",
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
            SyntaxKind::ExtendKw => "extend",
            SyntaxKind::ReplaceKw => "replace",
            SyntaxKind::GroupKw => "group",
            SyntaxKind::UngroupKw => "ungroup",
            SyntaxKind::WrapKw => "wrap",
            SyntaxKind::UnwrapKw => "unwrap",
            SyntaxKind::JoinKw => "join",
            SyntaxKind::TimesKw => "times",
            SyntaxKind::ComposeKw => "compose",
            SyntaxKind::MatchingKw => "matching",
            SyntaxKind::UnionKw => "union",
            SyntaxKind::DUnionKw => "d_union",
            SyntaxKind::XUnionKw => "xunion",
            SyntaxKind::IntersectKw => "intersect",
            SyntaxKind::MinusKw => "minus",
            SyntaxKind::IMinusKw => "i_minus",
            SyntaxKind::WhereKw => "where",
            SyntaxKind::AsKw => "as",
            SyntaxKind::InsertKw => "insert",
            SyntaxKind::DInsertKw => "d_insert",
            SyntaxKind::DeleteKw => "delete",
            SyntaxKind::IDeleteKw => "i_delete",
            SyntaxKind::UpdateKw => "update",
            SyntaxKind::AssignKw => "assign",
            SyntaxKind::ReturnKw => "return",
            SyntaxKind::IfKw => "if",
            SyntaxKind::ThenKw => "then",
            SyntaxKind::ElseKw => "else",
            SyntaxKind::WhileKw => "while",
            SyntaxKind::LoopKw => "loop",
            SyntaxKind::ForKw => "for",
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
            SyntaxKind::BangBang => "!!",
            _ => return None,
        })
    }

    /// How the kind is named in error messages.
    ///
    /// # Panics
    ///
    /// If the kind has no description, which a test rules out.
    #[must_use]
    pub fn describe(self) -> &'static str {
        self.describe_token()
            .or_else(|| self.describe_node())
            .expect("every kind is described")
    }

    /// How a token kind is named in error messages.
    fn describe_token(self) -> Option<&'static str> {
        Some(match self {
            SyntaxKind::Whitespace => "whitespace",
            SyntaxKind::Comment => "comment",
            SyntaxKind::Ident => "identifier",
            SyntaxKind::OperatorKw => "`operator`",
            SyntaxKind::VarKw => "`var`",
            SyntaxKind::TrueKw => "`true`",
            SyntaxKind::FalseKw => "`false`",
            SyntaxKind::TupleKw => "`tuple`",
            SyntaxKind::RelationKw => "`relation`",
            SyntaxKind::ArrayKw => "`array`",
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
            SyntaxKind::ExtendKw => "`extend`",
            SyntaxKind::ReplaceKw => "`replace`",
            SyntaxKind::GroupKw => "`group`",
            SyntaxKind::UngroupKw => "`ungroup`",
            SyntaxKind::WrapKw => "`wrap`",
            SyntaxKind::UnwrapKw => "`unwrap`",
            SyntaxKind::JoinKw => "`join`",
            SyntaxKind::TimesKw => "`times`",
            SyntaxKind::ComposeKw => "`compose`",
            SyntaxKind::MatchingKw => "`matching`",
            SyntaxKind::UnionKw => "`union`",
            SyntaxKind::DUnionKw => "`d_union`",
            SyntaxKind::XUnionKw => "`xunion`",
            SyntaxKind::IntersectKw => "`intersect`",
            SyntaxKind::MinusKw => "`minus`",
            SyntaxKind::IMinusKw => "`i_minus`",
            SyntaxKind::WhereKw => "`where`",
            SyntaxKind::AsKw => "`as`",
            SyntaxKind::InsertKw => "`insert`",
            SyntaxKind::DInsertKw => "`d_insert`",
            SyntaxKind::DeleteKw => "`delete`",
            SyntaxKind::IDeleteKw => "`i_delete`",
            SyntaxKind::UpdateKw => "`update`",
            SyntaxKind::AssignKw => "`assign`",
            SyntaxKind::ReturnKw => "`return`",
            SyntaxKind::IfKw => "`if`",
            SyntaxKind::ThenKw => "`then`",
            SyntaxKind::ElseKw => "`else`",
            SyntaxKind::WhileKw => "`while`",
            SyntaxKind::LoopKw => "`loop`",
            SyntaxKind::ForKw => "`for`",
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
            SyntaxKind::BangBang => "`!!`",
            SyntaxKind::IntLiteral => "integer literal",
            SyntaxKind::ApproximateLiteral => "approximate literal",
            SyntaxKind::TextLiteral => "text literal",
            SyntaxKind::ErrorToken => "unrecognized character",
            _ => return None,
        })
    }

    /// How a node kind is named in error messages.
    fn describe_node(self) -> Option<&'static str> {
        Some(match self {
            SyntaxKind::SourceFile => "source file",
            SyntaxKind::Operator => "operator definition",
            SyntaxKind::BuiltinOperator => "built-in operator declaration",
            SyntaxKind::BuiltinType => "built-in type declaration",
            SyntaxKind::KindDecl => "kind declaration",
            SyntaxKind::HeadingDecl => "heading declaration",
            SyntaxKind::TypeDecl => "type declaration",
            SyntaxKind::TypeBody => "type's representation",
            SyntaxKind::Representation => "possible representation",
            SyntaxKind::Conversion => "conversion",
            SyntaxKind::Constraint => "type constraint",
            SyntaxKind::TypeOrder => "type's order",
            SyntaxKind::RequirementList => "requirements",
            SyntaxKind::Requirement => "requirement",
            SyntaxKind::VarDecl => "variable declaration",
            SyntaxKind::Name | SyntaxKind::NameRef => "name",
            SyntaxKind::UnaryExpr
            | SyntaxKind::BinaryExpr
            | SyntaxKind::ParenExpr
            | SyntaxKind::ExtractExpr
            | SyntaxKind::IndexExpr
            | SyntaxKind::IfExpr
            | SyntaxKind::ValueBlock
            | SyntaxKind::ProjectExpr
            | SyntaxKind::PostfixExpr
            | SyntaxKind::NaryExpr
            | SyntaxKind::ImageExpr
            | SyntaxKind::FromExpr
            | SyntaxKind::CallExpr
            | SyntaxKind::MethodCallExpr => "expression",
            SyntaxKind::NameList => "attribute names",
            SyntaxKind::RenameList => "renamings",
            SyntaxKind::Renaming => "renaming",
            SyntaxKind::GatherList => "gatherings",
            SyntaxKind::Gathering => "gathering",
            SyntaxKind::AssignList => "attribute assignments",
            SyntaxKind::Assign => "attribute assignment",
            SyntaxKind::Body => "body",
            SyntaxKind::CallStmt => "call",
            SyntaxKind::AssignStmt => "assignment statement",
            SyntaxKind::AssignBlock => "`assign` block",
            SyntaxKind::ReturnStmt => "`return` statement",
            SyntaxKind::IfStmt => "`if` statement",
            SyntaxKind::WhileStmt | SyntaxKind::LoopStmt | SyntaxKind::ForStmt => "loop",
            SyntaxKind::Assignment => "assignment",
            SyntaxKind::Insertion => "insertion",
            SyntaxKind::Deletion => "deletion",
            SyntaxKind::Update => "update",
            SyntaxKind::Load => "load",
            SyntaxKind::OrderList => "order",
            SyntaxKind::Keeping => "attributes kept",
            SyntaxKind::ElementName => "element's attribute",
            SyntaxKind::Numbering => "numbering",
            SyntaxKind::LoadCondition => "condition",
            SyntaxKind::OrderItem => "order item",
            SyntaxKind::Arguments => "arguments",
            SyntaxKind::PositionalArg => "argument",
            SyntaxKind::TypeAnnotation => "type annotation",
            SyntaxKind::TypeRef | SyntaxKind::AnyType => "type",
            SyntaxKind::TypeParameters => "type parameters",
            SyntaxKind::Per => "`per`",
            SyntaxKind::Mode => "mode",
            SyntaxKind::Initializer => "initial value",
            SyntaxKind::Key => "key",
            SyntaxKind::Heading | SyntaxKind::HeadingRef => "heading",
            SyntaxKind::TupleType => "tuple type",
            SyntaxKind::RelationType => "relation type",
            SyntaxKind::ArrayType => "array type",
            SyntaxKind::TupleLiteral => "tuple value",
            SyntaxKind::RelationLiteral => "relation value",
            SyntaxKind::ArrayLiteral => "array value",
            SyntaxKind::AttributeType | SyntaxKind::AttributeValue => "attribute",
            SyntaxKind::Literal => "value",
            SyntaxKind::Error => "error",
            _ => return None,
        })
    }
}

/// Whether the braces of the list `node` have spaces inside them, in the
/// canonical layout: a heading's, a tuple's, a relation's, and new
/// attributes', as in `tuple { a: integer }` and `r extend { b := 1 }`, a
/// key's, `key { a, b }`, a built-in type's kinds, and a type's
/// representation, `type t { { a: integer } }`. Parameters,
/// arguments and attribute names are written without, as in
/// `greet{name: text}` and `r{a}`.
#[must_use]
pub fn spaced_braces(node: &SyntaxNode) -> bool {
    match node.kind() {
        SyntaxKind::Heading => node.parent().is_none_or(|owner| {
            !matches!(
                owner.kind(),
                SyntaxKind::Operator | SyntaxKind::BuiltinOperator
            )
        }),
        SyntaxKind::TupleLiteral
        | SyntaxKind::RelationLiteral
        | SyntaxKind::AssignList
        | SyntaxKind::GatherList
        | SyntaxKind::RequirementList
        | SyntaxKind::TypeBody => true,
        SyntaxKind::NameList => node
            .parent()
            .is_some_and(|k| matches!(k.kind(), SyntaxKind::Key | SyntaxKind::BuiltinType)),
        _ => false,
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
    fn every_kind_is_described() {
        for kind in SyntaxKind::ALL {
            assert!(
                kind.describe_token()
                    .or_else(|| kind.describe_node())
                    .is_some(),
                "{kind:?} has no description"
            );
        }
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
