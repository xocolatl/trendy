mod errors;
mod sequences;
mod unordered;

use trendy_diag::{Diagnostic, Span};
use trendy_lexer::{Lexed, Token};
use trendy_syntax::ast::{AstNode, SourceFile};
use trendy_syntax::{SyntaxKind, TreeBuilder};

use crate::sequences::bracketed_lists;
use crate::unordered::unordered_operators;

/// The parser's output: a complete tree, even for invalid input, plus every
/// lexer and parser error, ordered by position.
#[derive(Debug, Clone)]
pub struct Parse {
    pub tree: SourceFile,
    pub errors: Vec<Diagnostic>,
}

/// Build the concrete syntax tree for `source` from its tokens.
///
/// Never fails. Text that doesn't fit the grammar is kept in
/// [`SyntaxKind::Error`] nodes, so the tree always reproduces `source`
/// exactly.
#[must_use]
pub fn parse(source: &str, lexed: Lexed) -> Parse {
    let mut parser = Parser {
        source,
        tokens: &lexed.tokens,
        pos: 0,
        last_end: None,
        form: errors::OPERATOR_FORM,
        missing: None,
        builder: TreeBuilder::new(),
        errors: lexed.errors,
    };
    parser.source_file();
    parser.flush_missing();

    let tree = parser.builder.finish();
    let mut errors = parser.errors;
    errors.extend(unordered_operators(tree.syntax()));
    errors.extend(bracketed_lists(tree.syntax()));
    errors.sort_by_key(|d| d.span().start);
    Parse { tree, errors }
}

struct Parser<'a> {
    source: &'a str,
    tokens: &'a [Token],
    /// Index of the next token to add to the tree.
    pos: usize,
    /// Where the last significant token ended. Missing tokens belong here.
    last_end: Option<usize>,
    /// The form of the definition being parsed, for notes on errors in it.
    form: &'static str,
    /// Tokens found missing at `pos`, reported together as one error once
    /// the parser moves on, so one mistake gives one error.
    missing: Option<Missing>,
    builder: TreeBuilder,
    errors: Vec<Diagnostic>,
}

struct Missing {
    /// The token index where they were noticed.
    pos: usize,
    expected: Vec<SyntaxKind>,
    form: &'static str,
    /// Advice for what can't be inserted mechanically, such as a name.
    help: Option<&'static str>,
}

/// How a key is spelled: trendy's `key`, or SQL's `primary key` or
/// `unique`, which are reported with a fix.
#[derive(Clone, Copy, PartialEq, Eq)]
enum KeySpelling {
    Key,
    PrimaryKey,
    Unique,
}

/// The precedence of the comparison operators.
const COMPARISON: u8 = 6;
/// The precedence of `join`, `times` and `compose`.
const JOIN: u8 = 7;

/// A dyadic operator at the next token: its precedence, its first token,
/// and the keyword it is or stands for.
#[derive(Clone, Copy)]
struct BinaryOperator<'a> {
    precedence: u8,
    found: Found<'a>,
    keyword: SyntaxKind,
    /// For SQL's spelling of a join, such as `natural join` or
    /// `left outer join`: how many tokens it is.
    sql: Option<usize>,
}

/// Something the parser found where it expected something else.
#[derive(Clone, Copy)]
struct Found<'a> {
    kind: SyntaxKind,
    span: Span,
    text: &'a str,
}

impl Found<'_> {
    fn describe(&self) -> String {
        match self.kind {
            SyntaxKind::Ident => format!("identifier `{}`", self.text),
            kind => kind.describe().to_string(),
        }
    }
}

impl<'a> Parser<'a> {
    /// The builder opens and closes the root node itself.
    fn source_file(&mut self) {
        while let Some(found) = self.found() {
            match found.kind {
                SyntaxKind::OperatorKw => self.operator(),
                SyntaxKind::BuiltinKw => self.builtin(),
                SyntaxKind::VarKw => self.var_decl(),
                _ => self.junk(found),
            }
        }
    }

    fn operator(&mut self) {
        self.form = errors::OPERATOR_FORM;
        self.start_node(SyntaxKind::Operator);
        self.bump();
        self.name(
            "add a name after `operator`, such as `operator main{} [];`",
            &[SyntaxKind::LBrace, SyntaxKind::LParen],
        );
        self.params();
        self.delimited(
            Some(SyntaxKind::Body),
            "body",
            (SyntaxKind::LBracket, SyntaxKind::RBracket),
            &[
                (SyntaxKind::LBrace, SyntaxKind::RBrace),
                (SyntaxKind::LParen, SyntaxKind::RParen),
            ],
            Self::statements,
        );
        self.expect(SyntaxKind::Semicolon);
        self.builder.finish_node();
    }

    /// `builtin type name;` or `builtin operator name{params};`: a
    /// declaration of something the compiler implements. Only the prelude
    /// has these.
    fn builtin(&mut self) {
        self.form = errors::BUILTIN_FORM;
        let is_type = self
            .found_after(1)
            .is_some_and(|f| f.kind == SyntaxKind::Ident && f.text == "type");
        if is_type {
            self.start_node(SyntaxKind::BuiltinType);
            self.bump();
            self.bump();
            self.name(
                "add the type's name after `builtin type`",
                &[SyntaxKind::Semicolon],
            );
            self.expect(SyntaxKind::Semicolon);
            self.builder.finish_node();
            return;
        }
        self.start_node(SyntaxKind::BuiltinOperator);
        self.bump();
        self.expect(SyntaxKind::OperatorKw);
        self.name(
            "add a name after `operator`",
            &[SyntaxKind::LBrace, SyntaxKind::LParen],
        );
        self.params();
        self.expect(SyntaxKind::Semicolon);
        self.builder.finish_node();
    }

    /// An operator's parameters: a heading, which C-style `()` is accepted
    /// for and reported.
    fn params(&mut self) {
        let outer_form = std::mem::replace(&mut self.form, errors::HEADING_FORM);
        self.delimited(
            Some(SyntaxKind::Heading),
            "parameter list",
            (SyntaxKind::LBrace, SyntaxKind::RBrace),
            &[(SyntaxKind::LParen, SyntaxKind::RParen)],
            |p| {
                p.items(Self::at_attribute, |p| {
                    p.attribute(SyntaxKind::AttributeType, Self::type_expr);
                });
            },
        );
        self.form = outer_form;
    }

    /// Statements up to the end of a body.
    fn statements(&mut self) {
        while let Some(found) = self.found() {
            // A statement can't start with `;`, so one here ends the body:
            // in `[;`, the `]` is missing.
            let ends_body = matches!(
                found.kind,
                SyntaxKind::RBracket
                    | SyntaxKind::RBrace
                    | SyntaxKind::RParen
                    | SyntaxKind::Semicolon
                    | SyntaxKind::OperatorKw
                    | SyntaxKind::VarKw
                    | SyntaxKind::BuiltinKw
            );
            if ends_body {
                break;
            }
            self.statement();
        }
    }

    /// A statement. For now, the only statements are calls.
    fn statement(&mut self) {
        let Some(found) = self.found() else {
            return;
        };
        let call_like = self
            .found_after(1)
            .is_some_and(|f| matches!(f.kind, SyntaxKind::LBrace | SyntaxKind::LParen));
        if found.kind == SyntaxKind::CallKw {
            self.call_stmt();
        } else if found.kind == SyntaxKind::Ident && call_like {
            // `debug_value{v: r};`: a call without `call`.
            self.errors.push(errors::missing_call(found));
            self.call_stmt();
        } else {
            self.missing_with_help(
                SyntaxKind::CallKw,
                "statements are calls, such as `call debug_value{v: 1};`",
            );
            self.start_node(SyntaxKind::Error);
            while let Some(kind) = self.peek() {
                if matches!(
                    kind,
                    SyntaxKind::RBracket | SyntaxKind::OperatorKw | SyntaxKind::VarKw
                ) {
                    break;
                }
                self.bump();
                if kind == SyntaxKind::Semicolon {
                    break;
                }
            }
            self.builder.finish_node();
        }
    }

    /// `call name{arguments};`, with `call` already reported if missing.
    fn call_stmt(&mut self) {
        self.start_node(SyntaxKind::CallStmt);
        if self.at(SyntaxKind::CallKw) {
            self.bump();
        }
        if self.at(SyntaxKind::Ident) {
            self.start_node(SyntaxKind::NameRef);
            self.bump();
            self.builder.finish_node();
        } else if !self.keyword_as_name(
            SyntaxKind::NameRef,
            &[SyntaxKind::LBrace, SyntaxKind::LParen],
        ) {
            self.missing_with_help(SyntaxKind::Ident, "name the operator to call after `call`");
        }
        self.delimited(
            Some(SyntaxKind::Arguments),
            "argument list",
            (SyntaxKind::LBrace, SyntaxKind::RBrace),
            &[(SyntaxKind::LParen, SyntaxKind::RParen)],
            |p| p.items(Self::at_argument, Self::argument),
        );
        self.expect(SyntaxKind::Semicolon);
        self.builder.finish_node();
    }

    fn at_argument(&mut self) -> bool {
        self.peek().is_some_and(|k| {
            k.is_literal()
                || matches!(
                    k,
                    SyntaxKind::Ident
                        | SyntaxKind::TupleKw
                        | SyntaxKind::RelationKw
                        | SyntaxKind::LBrace
                )
        })
    }

    /// `name: value`, or a value without a name, which sema reports.
    fn argument(&mut self) {
        let named = self.at(SyntaxKind::Ident)
            && self.found_after(1).is_some_and(|f| {
                matches!(
                    f.kind,
                    SyntaxKind::Colon | SyntaxKind::Eq | SyntaxKind::ColonEq
                )
            });
        if named {
            self.attribute(SyntaxKind::AttributeValue, Self::expression);
        } else {
            self.start_node(SyntaxKind::PositionalArg);
            self.expression();
            self.builder.finish_node();
        }
    }

    /// `var name: type := value;`, where either the type or the value may be
    /// left out. Sema checks that at least one is there.
    fn var_decl(&mut self) {
        self.form = errors::VARIABLE_FORM;
        self.start_node(SyntaxKind::VarDecl);
        self.bump();
        self.name(
            "add a name after `var`, such as `var count: integer := 0;`",
            &[SyntaxKind::Colon, SyntaxKind::ColonEq, SyntaxKind::Eq],
        );

        let starts_type = |k| {
            matches!(
                k,
                SyntaxKind::Ident | SyntaxKind::TupleKw | SyntaxKind::RelationKw
            )
        };
        if self.at(SyntaxKind::Colon)
            || (self.peek().is_some_and(starts_type) && self.at_key().is_none())
        {
            self.start_node(SyntaxKind::TypeAnnotation);
            if self.at(SyntaxKind::Colon) {
                self.bump();
            } else {
                // `var i integer`: the type is there but the colon isn't.
                self.missing(SyntaxKind::Colon);
            }
            self.type_expr();
            self.builder.finish_node();
        }

        // Keys go after the value, but a variable without one has them right
        // after its type, so a value added later can land after them.
        let before_keys = self.last_end;
        let early_keys = self.keys();
        let initialized = match self.found() {
            Some(found) if matches!(found.kind, SyntaxKind::ColonEq | SyntaxKind::Eq) => {
                self.start_node(SyntaxKind::Initializer);
                let span = self.bump();
                if found.kind == SyntaxKind::Eq {
                    self.errors.push(errors::assignment_with_eq(span));
                }
                self.expression();
                self.builder.finish_node();
                true
            }
            Some(found)
                if found.kind.is_literal()
                    || matches!(found.kind, SyntaxKind::TupleKw | SyntaxKind::RelationKw) =>
            {
                // `var i: integer 0`: the value is there but `:=` isn't.
                self.start_node(SyntaxKind::Initializer);
                self.missing(SyntaxKind::ColonEq);
                self.expression();
                self.builder.finish_node();
                true
            }
            _ => false,
        };
        if initialized
            && let (Some(first), Some(last), Some(before), Some(value_end)) = (
                early_keys.first(),
                early_keys.last(),
                before_keys,
                self.last_end,
            )
        {
            let keys = &self.source[first.start..last.end];
            self.errors.push(errors::key_before_value(
                Span::new(first.start, last.end),
                Span::new(before, last.end),
                keys,
                value_end,
            ));
        }
        self.keys();
        self.expect(SyntaxKind::Semicolon);
        self.builder.finish_node();
    }

    /// A variable's keys, each `key { a, b }`, and their spans.
    fn keys(&mut self) -> Vec<Span> {
        let mut spans = Vec::new();
        while let Some(spelling) = self.at_key() {
            let start = self.found().map_or(0, |f| f.span.start);
            self.key(spelling);
            spans.push(Span::new(start, self.last_end.unwrap_or(start)));
        }
        spans
    }

    /// How the key at the next token is spelled, if one is there: `key`
    /// in any case, or SQL's `primary key` or `unique`, followed by its
    /// attributes in braces, or in SQL's parentheses.
    fn at_key(&mut self) -> Option<KeySpelling> {
        let word = |found: Option<Found>, word: &str| {
            found.is_some_and(|f| f.kind == SyntaxKind::Ident && f.text.eq_ignore_ascii_case(word))
        };
        let list = |found: Option<Found>| {
            found.is_some_and(|f| matches!(f.kind, SyntaxKind::LBrace | SyntaxKind::LParen))
        };
        let next = self.found();
        let after = self.found_after(1);
        if word(next, "key") && list(after) {
            Some(KeySpelling::Key)
        } else if word(next, "primary") && word(after, "key") {
            Some(KeySpelling::PrimaryKey)
        } else if word(next, "unique") && list(after) {
            Some(KeySpelling::Unique)
        } else {
            None
        }
    }

    /// `key { a, b }`, spelled as `at_key` found it.
    fn key(&mut self, spelling: KeySpelling) {
        self.start_node(SyntaxKind::Key);
        let first = self.found().expect("a key starts here");
        let mut span = self.bump();
        match spelling {
            KeySpelling::Key if first.text != "key" => {
                self.errors.push(errors::miscased_keyword(first, "key"));
            }
            KeySpelling::Key => {}
            KeySpelling::PrimaryKey | KeySpelling::Unique => {
                if spelling == KeySpelling::PrimaryKey {
                    span.end = self.bump().end;
                }
                let text = &self.source[span.start..span.end];
                self.errors.push(errors::sql_key(span, text));
            }
        }
        self.names(errors::KEY_FORM);
        self.builder.finish_node();
    }

    /// A definition's name, or `help` on what to write if it's missing.
    /// A name being defined. `follows` are the tokens that can come right
    /// after it, which tell a keyword used as the name from a missing name.
    fn name(&mut self, help: &'static str, follows: &[SyntaxKind]) {
        if self.at(SyntaxKind::Ident) {
            self.start_node(SyntaxKind::Name);
            self.bump();
            self.builder.finish_node();
        } else if !self.keyword_as_name(SyntaxKind::Name, follows) {
            self.missing_with_help(SyntaxKind::Ident, help);
        }
    }

    /// A keyword where a name goes, as in `var remove := 1;`, followed by
    /// one of `follows`: parsed as the `node` it was meant to be, so nothing
    /// else cascades, and reported.
    fn keyword_as_name(&mut self, node: SyntaxKind, follows: &[SyntaxKind]) -> bool {
        let Some(found) = self.found().filter(|f| f.kind.is_keyword()) else {
            return false;
        };
        if !self
            .found_after(1)
            .is_some_and(|next| follows.contains(&next.kind))
        {
            return false;
        }
        self.start_node(node);
        self.bump();
        self.builder.finish_node();
        self.errors.push(errors::keyword_as_name(found));
        true
    }

    /// A named type, a tuple type, or a relation type.
    fn type_expr(&mut self) {
        if self.at_keyword(SyntaxKind::TupleKw) || self.at(SyntaxKind::LBrace) {
            // A bare `{`, as in TypeScript, is a tuple type without `tuple`.
            self.start_node(SyntaxKind::TupleType);
            self.keyword(SyntaxKind::TupleKw);
            self.heading();
            self.builder.finish_node();
        } else if self.at_keyword(SyntaxKind::RelationKw) {
            self.start_node(SyntaxKind::RelationType);
            self.keyword(SyntaxKind::RelationKw);
            self.heading();
            self.builder.finish_node();
        } else if self.at(SyntaxKind::Ident) {
            self.start_node(SyntaxKind::TypeRef);
            self.bump();
            self.builder.finish_node();
        } else {
            self.missing_with_help(
                SyntaxKind::TypeRef,
                "write a type, such as `integer`, `tuple { name: text }` or `relation { name: text }`",
            );
        }
    }

    /// An expression.
    fn expression(&mut self) {
        self.binary(0);
    }

    /// An expression whose dyadic operators bind at least as tightly as
    /// `min`, by precedence climbing. From loosest to tightest: `implies`
    /// (1, grouping to the right), `or` (2), `xor` (3), `and` (4), the
    /// comparisons (6), which don't chain, `join`, `times` and `compose`
    /// (7), `+`, `-` and `||` (8), and `*`, `/`, `div` and `mod` (9), all
    /// grouping to the left. The prefix operators, `not`, `-` and `+`, bind
    /// tightest, so `not a = b` is `(not a) = b`.
    fn binary(&mut self, min: u8) {
        self.peek();
        let start = self.builder.checkpoint();
        self.unary();
        // The right operand of a comparison just parsed: comparisons don't
        // chain, so another one after it is reported, with a fix.
        let mut compared: Option<Span> = None;
        while let Some(operator) = self.binary_operator() {
            let BinaryOperator {
                precedence,
                found,
                keyword,
                sql,
            } = operator;
            if precedence < min {
                break;
            }
            if let Some(middle) = compared.filter(|_| precedence == COMPARISON) {
                let before = self.source[..found.span.start].chars().next_back();
                self.errors.push(errors::chained_comparison(
                    found,
                    &self.source[middle.start..middle.end],
                    before,
                ));
            }
            self.builder.start_node_at(start, SyntaxKind::BinaryExpr);
            match sql {
                Some(tokens) => self.sql_join(found, keyword, tokens),
                None => self.operator_token(found, keyword),
            }
            let rhs_start = self.lookahead().map(|f| f.span.start);
            let right_grouping = keyword == SyntaxKind::ImpliesKw;
            self.binary(if right_grouping {
                precedence
            } else {
                precedence + 1
            });
            compared = match (precedence == COMPARISON, rhs_start, self.last_end) {
                (true, Some(start), Some(end)) if start < end => Some(Span::new(start, end)),
                _ => None,
            };
            if precedence == JOIN {
                self.join_condition();
            }
            self.builder.finish_node();
        }
    }

    /// The dyadic operator at the next token, if any.
    fn binary_operator(&self) -> Option<BinaryOperator<'a>> {
        let found = self.lookahead_nth(0)?;
        if let Some((keyword, tokens)) = self.sql_join_at() {
            return Some(BinaryOperator {
                precedence: JOIN,
                found,
                keyword,
                sql: Some(tokens),
            });
        }
        let keyword = match found.kind {
            SyntaxKind::ForeignOperator => errors::foreign_operator(found.text)?,
            SyntaxKind::Ident => errors::operator_spelling(found.text)?,
            kind => kind,
        };
        let precedence = match keyword {
            SyntaxKind::ImpliesKw => 1,
            SyntaxKind::OrKw => 2,
            SyntaxKind::XorKw => 3,
            SyntaxKind::AndKw => 4,
            kind if kind.is_comparison() => COMPARISON,
            kind if kind.is_join() => JOIN,
            SyntaxKind::Plus | SyntaxKind::Minus | SyntaxKind::PipePipe => 8,
            SyntaxKind::Star | SyntaxKind::Slash | SyntaxKind::DivKw | SyntaxKind::ModKw => 9,
            _ => return None,
        };
        Some(BinaryOperator {
            precedence,
            found,
            keyword,
            sql: None,
        })
    }

    /// SQL's spelling of a join at the next token, in any case: `natural
    /// join` and `inner join`, which are `join`, `cross join`, which is
    /// `times`, and the outer joins, `left`, `right` or `full`, optionally
    /// followed by `outer`, then `join`, which have no equivalent. The
    /// keyword it stands for, and how many tokens it is.
    fn sql_join_at(&self) -> Option<(SyntaxKind, usize)> {
        let word = |n: usize, word: &str| {
            self.lookahead_nth(n)
                .is_some_and(|f| f.text.eq_ignore_ascii_case(word))
        };
        let keyword = if word(0, "cross") {
            SyntaxKind::TimesKw
        } else if ["natural", "inner", "left", "right", "full"]
            .into_iter()
            .any(|w| word(0, w))
        {
            SyntaxKind::JoinKw
        } else {
            return None;
        };
        let outer = ["left", "right", "full"].into_iter().any(|w| word(0, w)) && word(1, "outer");
        let tokens = if outer { 3 } else { 2 };
        word(tokens - 1, "join").then_some((keyword, tokens))
    }

    /// SQL's spelling of a join, whose first token is `first`, `tokens`
    /// long, standing for `keyword`: consumed as the operator, and reported.
    fn sql_join(&mut self, first: Found<'a>, keyword: SyntaxKind, tokens: usize) {
        let mut span = first.span;
        for _ in 0..tokens {
            span.end = self.bump().end;
        }
        let text = &self.source[span.start..span.end];
        self.errors.push(errors::sql_join(span, text, keyword));
    }

    /// SQL's `on condition` or `using (a, b)` after a join's right operand:
    /// consumed, so nothing else cascades, and reported.
    fn join_condition(&mut self) {
        let Some(found) = self
            .lookahead_nth(0)
            .filter(|f| f.kind == SyntaxKind::Ident)
            .filter(|f| f.text.eq_ignore_ascii_case("on") || f.text.eq_ignore_ascii_case("using"))
        else {
            return;
        };
        let start = self.last_end.unwrap_or(found.span.start);
        self.start_node(SyntaxKind::Error);
        self.bump();
        if found.text.eq_ignore_ascii_case("on") {
            self.binary(JOIN + 1);
            while self
                .lookahead_nth(0)
                .is_some_and(|f| f.kind.is_comparison() || f.kind == SyntaxKind::AndKw)
            {
                // `on a.x = b.y and a.z = b.w`: the whole condition.
                self.bump();
                self.binary(JOIN + 1);
            }
        } else if self.at(SyntaxKind::LParen) {
            while !matches!(
                self.peek(),
                None | Some(SyntaxKind::RParen | SyntaxKind::Semicolon)
            ) {
                self.bump();
            }
            if self.at(SyntaxKind::RParen) {
                self.bump();
            }
        }
        self.builder.finish_node();
        let end = self.last_end.unwrap_or(found.span.end);
        self.errors.push(errors::join_condition(
            found,
            Span::new(found.span.start, end),
            Span::new(start, end),
        ));
    }

    /// A prefix operator and its operand, or an operand.
    fn unary(&mut self) {
        let prefix = self.found().and_then(|f| {
            let keyword = match f.kind {
                SyntaxKind::ForeignOperator => errors::foreign_operator(f.text)?,
                SyntaxKind::Ident => errors::operator_spelling(f.text)?,
                kind => kind,
            };
            matches!(
                keyword,
                SyntaxKind::NotKw | SyntaxKind::Minus | SyntaxKind::Plus
            )
            .then_some((f, keyword))
        });
        let Some((found, keyword)) = prefix else {
            self.postfix();
            return;
        };
        self.start_node(SyntaxKind::UnaryExpr);
        self.operator_token(found, keyword);
        self.unary();
        self.builder.finish_node();
    }

    /// `!!r`, Tutorial D's image relation, whose operand is a single one: a
    /// name, a literal or a parenthesized expression. An operator after it
    /// could apply to the operand or to the image, so a postfix operator or
    /// a join there is reported (`unordered_operators`), and parsed as
    /// applying to the image.
    fn image(&mut self) {
        self.start_node(SyntaxKind::ImageExpr);
        self.bump();
        if self.at_operand() {
            self.primary();
        } else {
            let outer_form = std::mem::replace(&mut self.form, errors::IMAGE_FORM);
            self.missing_with_help(
                SyntaxKind::Ident,
                "write the relation after `!!`, as in `!!shipments`",
            );
            self.form = outer_form;
        }
        self.builder.finish_node();
    }

    /// An operand, then postfix operators, which bind tighter than any
    /// other: attribute extraction, `t.a`, projections, `r{a, b}`, `remove`
    /// and `rename`. So `a and b{x}` projects `b`, and `-t.a` negates `t.a`.
    fn postfix(&mut self) {
        self.peek();
        let start = self.builder.checkpoint();
        if self.at(SyntaxKind::BangBang) {
            self.image();
        } else {
            self.primary();
        }
        loop {
            let next = self.lookahead().map(|f| f.kind);
            if next == Some(SyntaxKind::Dot) {
                self.builder.start_node_at(start, SyntaxKind::ExtractExpr);
                self.bump();
                if self.at(SyntaxKind::Ident) {
                    self.start_node(SyntaxKind::Name);
                    self.bump();
                    self.builder.finish_node();
                } else {
                    self.missing_with_help(
                        SyntaxKind::Ident,
                        "write the attribute's name after `.`, as in `t.name`",
                    );
                }
            } else if next == Some(SyntaxKind::LBrace) {
                self.builder.start_node_at(start, SyntaxKind::ProjectExpr);
                self.names(errors::PROJECTION_FORM);
            } else if let Some((found, keyword)) = self.postfix_operator() {
                self.builder.start_node_at(start, SyntaxKind::PostfixExpr);
                self.operator_token(found, keyword);
                match keyword {
                    SyntaxKind::RenameKw => self.rename_list(),
                    SyntaxKind::ExtendKw | SyntaxKind::ReplaceKw => self.assign_list(keyword),
                    SyntaxKind::GroupKw => self.group_list(),
                    _ => self.names(errors::PROJECTION_FORM),
                }
            } else {
                break;
            }
            self.builder.finish_node();
        }
    }

    /// The postfix operator at the next token, if any, and the keyword it is
    /// or stands for.
    fn postfix_operator(&self) -> Option<(Found<'a>, SyntaxKind)> {
        let found = self.lookahead()?;
        let keyword = match found.kind {
            SyntaxKind::Ident => errors::operator_spelling(found.text)?,
            kind => kind,
        };
        matches!(
            keyword,
            SyntaxKind::RemoveKw
                | SyntaxKind::RenameKw
                | SyntaxKind::ExtendKw
                | SyntaxKind::ReplaceKw
                | SyntaxKind::GroupKw
        )
        .then_some((found, keyword))
    }

    /// `{a as b, c as d}`: renamings.
    fn rename_list(&mut self) {
        let outer_form = std::mem::replace(&mut self.form, errors::RENAME_FORM);
        self.delimited(
            Some(SyntaxKind::RenameList),
            "renamings",
            (SyntaxKind::LBrace, SyntaxKind::RBrace),
            &[(SyntaxKind::LParen, SyntaxKind::RParen)],
            |p| p.items(Self::at_attribute, Self::renaming),
        );
        self.form = outer_form;
    }

    /// `a as b`. `AS` is fixed to `as`, and `a: b`, a mapping from other
    /// languages, to `a as b`.
    fn renaming(&mut self) {
        self.start_node(SyntaxKind::Renaming);
        self.start_node(SyntaxKind::Name);
        self.bump();
        self.builder.finish_node();
        match self.found() {
            Some(found) if found.kind == SyntaxKind::AsKw => {
                self.bump();
            }
            Some(found)
                if found.kind == SyntaxKind::Ident && found.text.eq_ignore_ascii_case("as") =>
            {
                self.bump();
                self.errors.push(errors::miscased_keyword(found, "as"));
            }
            Some(found) if found.kind == SyntaxKind::Colon => {
                let neighbors = (
                    self.source[..found.span.start].chars().next_back(),
                    self.source[found.span.end..].chars().next(),
                );
                self.bump();
                self.errors
                    .push(errors::colon_in_renaming(found, neighbors));
            }
            _ => self.missing(SyntaxKind::AsKw),
        }
        if self.at(SyntaxKind::Ident) {
            self.start_node(SyntaxKind::Name);
            self.bump();
            self.builder.finish_node();
        } else {
            self.missing_with_help(SyntaxKind::Ident, "write the new name after `as`");
        }
        self.builder.finish_node();
    }

    /// The delimiters of an `extend`'s, a `replace`'s or a `group`'s list:
    /// braces, since its items apply all at once. Brackets, which would
    /// mean in order, are parsed as they are, and reported with the chain
    /// of the operator that applies them in order (`sequences`).
    fn list_delimiters(&mut self) -> (SyntaxKind, SyntaxKind) {
        if self.at(SyntaxKind::LBracket) {
            (SyntaxKind::LBracket, SyntaxKind::RBracket)
        } else {
            (SyntaxKind::LBrace, SyntaxKind::RBrace)
        }
    }

    /// `{ a := x, b := y }`: attributes and their values, after `keyword`,
    /// `extend` or `replace`.
    fn assign_list(&mut self, keyword: SyntaxKind) {
        let assigning = errors::Assigning::of(keyword);
        let outer_form = std::mem::replace(&mut self.form, assigning.form());
        let delimiters = self.list_delimiters();
        self.delimited(
            Some(SyntaxKind::AssignList),
            assigning.list(),
            delimiters,
            &[(SyntaxKind::LParen, SyntaxKind::RParen)],
            move |p| p.items(Self::at_operand, move |p| p.assign(assigning)),
        );
        self.form = outer_form;
    }

    /// `a := x`. `a: x` and `a = x`, as a tuple and as keyword arguments
    /// are written, are fixed to `a := x`, and `x as a`, as SQL and Tutorial
    /// D's `ADD` write it, to `a := x`.
    fn assign(&mut self, assigning: errors::Assigning) {
        self.start_node(SyntaxKind::Assign);
        let separator = self.found_after(1).map(|f| f.kind);
        let named = self.at(SyntaxKind::Ident)
            && matches!(
                separator,
                Some(SyntaxKind::ColonEq | SyntaxKind::Colon | SyntaxKind::Eq)
            );
        if named {
            self.start_node(SyntaxKind::Name);
            self.bump();
            self.builder.finish_node();
            let found = self.found().expect("a separator is next");
            let neighbors = (
                self.source[..found.span.start].chars().next_back(),
                self.source[found.span.end..].chars().next(),
            );
            let span = self.bump();
            if found.kind != SyntaxKind::ColonEq {
                self.errors.push(errors::assign_separator(
                    assigning, found.kind, span, neighbors,
                ));
            }
            self.expression();
        } else {
            let start = self.found().map_or(0, |f| f.span.start);
            self.expression();
            let value_end = self.last_end.unwrap_or(start);
            match self.found() {
                Some(found)
                    if found.kind == SyntaxKind::AsKw
                        || (found.kind == SyntaxKind::Ident
                            && found.text.eq_ignore_ascii_case("as")) =>
                {
                    self.bump();
                    if let Some(name) = self.found().filter(|f| f.kind == SyntaxKind::Ident) {
                        self.start_node(SyntaxKind::Name);
                        self.bump();
                        self.builder.finish_node();
                        let value = &self.source[start..value_end];
                        self.errors.push(errors::assign_as(
                            assigning,
                            Span::new(start, name.span.end),
                            name.text,
                            value,
                        ));
                    } else {
                        self.missing_with_help(SyntaxKind::Ident, assigning.name_help());
                    }
                }
                _ => {
                    self.errors.push(errors::unnamed_assign(
                        assigning,
                        Span::new(start, value_end),
                    ));
                }
            }
        }
        self.builder.finish_node();
    }

    /// `{ g := {a, b}, h := {c} }`: groupings. SQL's `group by {a}`, which
    /// names the attributes kept rather than gathered, is reported.
    fn group_list(&mut self) {
        let outer_form = std::mem::replace(&mut self.form, errors::GROUP_FORM);
        if let Some(by) = self
            .found()
            .filter(|f| f.kind == SyntaxKind::Ident && f.text.eq_ignore_ascii_case("by"))
        {
            self.start_node(SyntaxKind::Error);
            self.bump();
            if self.at(SyntaxKind::LBrace) {
                self.names(errors::GROUP_FORM);
            }
            self.builder.finish_node();
            let end = self.last_end.unwrap_or(by.span.end);
            self.errors
                .push(errors::group_by(Span::new(by.span.start, end)));
            self.form = outer_form;
            return;
        }
        let delimiters = self.list_delimiters();
        self.delimited(
            Some(SyntaxKind::GroupList),
            "groupings",
            delimiters,
            &[(SyntaxKind::LParen, SyntaxKind::RParen)],
            |p| p.items(Self::at_grouping, Self::grouping),
        );
        self.form = outer_form;
    }

    fn at_grouping(&mut self) -> bool {
        matches!(self.peek(), Some(SyntaxKind::Ident | SyntaxKind::LBrace))
    }

    /// `g := {a, b}`. `g: {a, b}` and `g = {a, b}` are fixed to `:=`, and
    /// Tutorial D's `{a, b} as g` to `g := {a, b}`.
    fn grouping(&mut self) {
        self.start_node(SyntaxKind::Grouping);
        if self.at(SyntaxKind::LBrace) {
            let start = self.found().map_or(0, |f| f.span.start);
            self.names(errors::GROUP_FORM);
            let list_end = self.last_end.unwrap_or(start);
            match self.found() {
                Some(found)
                    if found.kind == SyntaxKind::AsKw
                        || (found.kind == SyntaxKind::Ident
                            && found.text.eq_ignore_ascii_case("as")) =>
                {
                    self.bump();
                    if let Some(name) = self.found().filter(|f| f.kind == SyntaxKind::Ident) {
                        self.start_node(SyntaxKind::Name);
                        self.bump();
                        self.builder.finish_node();
                        let list = &self.source[start..list_end];
                        self.errors.push(errors::grouping_as(
                            Span::new(start, name.span.end),
                            name.text,
                            list,
                        ));
                    } else {
                        self.missing_with_help(
                            SyntaxKind::Ident,
                            "write the new attribute's name before the attributes, as in `residents := {name}`",
                        );
                    }
                }
                _ => self
                    .errors
                    .push(errors::unnamed_grouping(Span::new(start, list_end))),
            }
        } else {
            self.start_node(SyntaxKind::Name);
            self.bump();
            self.builder.finish_node();
            match self.found() {
                Some(found) if found.kind == SyntaxKind::ColonEq => {
                    self.bump();
                }
                Some(found) if matches!(found.kind, SyntaxKind::Colon | SyntaxKind::Eq) => {
                    let neighbors = (
                        self.source[..found.span.start].chars().next_back(),
                        self.source[found.span.end..].chars().next(),
                    );
                    let span = self.bump();
                    self.errors
                        .push(errors::grouping_separator(found.kind, span, neighbors));
                }
                _ => self.missing(SyntaxKind::ColonEq),
            }
            self.names(errors::GROUP_FORM);
        }
        self.builder.finish_node();
    }

    /// `{ a, b }`: attribute names, in a construct of the given `form`.
    fn names(&mut self, form: &'static str) {
        let outer_form = std::mem::replace(&mut self.form, form);
        self.delimited(
            Some(SyntaxKind::NameList),
            "attribute names",
            (SyntaxKind::LBrace, SyntaxKind::RBrace),
            &[(SyntaxKind::LParen, SyntaxKind::RParen)],
            |p| {
                p.items(Self::at_attribute, |p| {
                    p.start_node(SyntaxKind::Name);
                    p.bump();
                    p.builder.finish_node();
                });
            },
        );
        self.form = outer_form;
    }

    /// Consume an operator: `keyword` itself, or another spelling of it,
    /// which is reported with a fix.
    fn operator_token(&mut self, found: Found<'a>, keyword: SyntaxKind) {
        self.bump();
        match found.kind {
            SyntaxKind::ForeignOperator => {
                let neighbors = (
                    self.source[..found.span.start].chars().next_back(),
                    self.source[found.span.end..].chars().next(),
                );
                self.errors
                    .push(errors::foreign_operator_used(found, keyword, neighbors));
            }
            SyntaxKind::Ident => {
                let spelled = keyword.fixed_text().expect("keywords have fixed text");
                self.errors.push(errors::miscased_keyword(found, spelled));
            }
            _ => {}
        }
    }

    /// An operand: a literal, a tuple or relation value, a name, or an
    /// expression in parentheses.
    fn primary(&mut self) {
        if self.at(SyntaxKind::LParen) {
            self.start_node(SyntaxKind::ParenExpr);
            self.bump();
            self.binary(0);
            self.expect(SyntaxKind::RParen);
            self.builder.finish_node();
            return;
        }
        if self.peek().is_some_and(SyntaxKind::is_literal) {
            self.start_node(SyntaxKind::Literal);
            self.bump();
            self.builder.finish_node();
        } else if let Some((found, keyword)) = self.at_nary() {
            self.nary(found, keyword);
        } else if self.at_keyword(SyntaxKind::RelationKw) {
            self.relation_literal();
        } else if self.at_keyword(SyntaxKind::TupleKw) || self.at(SyntaxKind::LBrace) {
            self.tuple_literal(false);
        } else if let Some(found) = self
            .found()
            .filter(|f| f.kind == SyntaxKind::Ident && errors::boolean_spelling(f.text).is_some())
        {
            // `True` or `TRUE`: a boolean literal in the wrong case. It's kept
            // as the literal it was meant to be, so nothing else cascades.
            self.start_node(SyntaxKind::Literal);
            self.bump();
            self.builder.finish_node();
            self.errors.push(errors::miscased_keyword(
                found,
                errors::boolean_spelling(found.text).expect("just checked"),
            ));
        } else if self.at(SyntaxKind::Ident) {
            self.start_node(SyntaxKind::NameRef);
            self.bump();
            self.builder.finish_node();
        } else {
            self.missing_with_help(
                SyntaxKind::Literal,
                "write a value, such as `0`, `\"\"`, `tuple { name: \"x\" }` or `relation { { name: \"x\" } }`",
            );
        }
    }

    /// A join of any number of operands at the next token, if there is one:
    /// `join`, `times` or `compose`, in any case, followed by its operands
    /// in braces or brackets, or in parentheses as a call elsewhere is
    /// written. The token, and the keyword it is or stands for.
    fn at_nary(&mut self) -> Option<(Found<'a>, SyntaxKind)> {
        let found = self.found()?;
        let keyword = match found.kind {
            SyntaxKind::Ident => errors::operator_spelling(found.text)?,
            kind => kind,
        };
        let list = self.found_after(1).is_some_and(|f| {
            matches!(
                f.kind,
                SyntaxKind::LBrace | SyntaxKind::LBracket | SyntaxKind::LParen
            )
        });
        (keyword.is_join() && list).then_some((found, keyword))
    }

    /// `join{r, s, t}`, `times{r, s, t}` or `compose[r, s, t]`: the keyword,
    /// found as `found`, and the operands. Braces enclose what can be in any
    /// order, and brackets what can't, so the others are fixed to them.
    fn nary(&mut self, found: Found<'a>, keyword: SyntaxKind) {
        let outer_form = std::mem::replace(&mut self.form, errors::NARY_FORM);
        self.start_node(SyntaxKind::NaryExpr);
        self.operator_token(found, keyword);
        let braces = (SyntaxKind::LBrace, SyntaxKind::RBrace);
        let brackets = (SyntaxKind::LBracket, SyntaxKind::RBracket);
        let parens = (SyntaxKind::LParen, SyntaxKind::RParen);
        let (delims, mistakes) = if keyword == SyntaxKind::ComposeKw {
            (brackets, [braces, parens])
        } else {
            (braces, [brackets, parens])
        };
        self.delimited(None, "operands", delims, &mistakes, |p| {
            p.items(Self::at_operand, Self::expression);
        });
        self.builder.finish_node();
        self.form = outer_form;
    }

    /// Whether an operand can start at the next token, in a list of them.
    fn at_operand(&mut self) -> bool {
        !matches!(
            self.peek(),
            None | Some(
                SyntaxKind::RBrace
                    | SyntaxKind::RParen
                    | SyntaxKind::RBracket
                    | SyntaxKind::Comma
                    | SyntaxKind::Semicolon
            )
        )
    }

    /// `tuple [heading] { name: value, … }`. In a relation, the `tuple`
    /// keyword may be left out; elsewhere a bare `{` is reported, with a fix.
    fn tuple_literal(&mut self, in_relation: bool) {
        let outer_form = std::mem::replace(&mut self.form, errors::TUPLE_VALUE_FORM);
        self.start_node(SyntaxKind::TupleLiteral);
        // A heading needs the keyword: in a relation, `{ a: 1 } { a: 2 }` is
        // two tuples with a missing comma, not a heading and a body.
        let has_keyword = self.at_keyword(SyntaxKind::TupleKw);
        if has_keyword || !in_relation {
            self.keyword(SyntaxKind::TupleKw);
        }
        if has_keyword && self.at_heading() {
            self.heading();
        }
        self.delimited(
            None,
            "tuple",
            (SyntaxKind::LBrace, SyntaxKind::RBrace),
            &[],
            |p| {
                p.items(Self::at_attribute, |p| {
                    p.attribute(SyntaxKind::AttributeValue, Self::expression);
                });
            },
        );
        self.builder.finish_node();
        self.form = outer_form;
    }

    /// `relation [heading] { tuple, … }`.
    fn relation_literal(&mut self) {
        let outer_form = std::mem::replace(&mut self.form, errors::RELATION_VALUE_FORM);
        self.start_node(SyntaxKind::RelationLiteral);
        self.keyword(SyntaxKind::RelationKw);
        if self.at_heading() {
            self.heading();
        } else if self.at(SyntaxKind::LBrace)
            && self
                .found_after(1)
                .is_some_and(|f| f.kind == SyntaxKind::Ident)
            && self
                .found_after(2)
                .is_some_and(|f| f.kind == SyntaxKind::Colon)
        {
            // `relation { a: integer }`: a heading with no tuples after it.
            self.heading();
            self.missing(SyntaxKind::LBrace);
            self.missing(SyntaxKind::RBrace);
            self.builder.finish_node();
            self.form = outer_form;
            return;
        }
        self.delimited(
            None,
            "relation",
            (SyntaxKind::LBrace, SyntaxKind::RBrace),
            &[],
            |p| {
                p.items(
                    |p| p.at(SyntaxKind::LBrace) || p.at_keyword(SyntaxKind::TupleKw),
                    |p| p.tuple_literal(true),
                );
            },
        );
        self.builder.finish_node();
        self.form = outer_form;
    }

    /// `{ name: type, … }`.
    fn heading(&mut self) {
        let outer_form = std::mem::replace(&mut self.form, errors::HEADING_FORM);
        self.delimited(
            Some(SyntaxKind::Heading),
            "heading",
            (SyntaxKind::LBrace, SyntaxKind::RBrace),
            &[],
            |p| {
                p.items(Self::at_attribute, |p| {
                    p.attribute(SyntaxKind::AttributeType, Self::type_expr);
                });
            },
        );
        self.form = outer_form;
    }

    fn at_attribute(&mut self) -> bool {
        self.at(SyntaxKind::Ident)
    }

    /// Comma-separated items, with an optional trailing comma. `starts_item`
    /// says whether an item starts at the next token, and `item` parses one.
    fn items(&mut self, starts_item: fn(&mut Self) -> bool, item: impl Fn(&mut Self)) {
        while starts_item(self) {
            item(self);
            if self.at(SyntaxKind::Comma) {
                self.bump();
            } else if starts_item(self) {
                // `{ a: 1 b: 2 }`: the next item is there but the comma isn't.
                self.missing(SyntaxKind::Comma);
            } else {
                break;
            }
        }
    }

    /// `name: x`, with `x` parsed by `item`.
    fn attribute(&mut self, attribute: SyntaxKind, item: fn(&mut Self)) {
        self.start_node(attribute);
        self.start_node(SyntaxKind::Name);
        self.bump();
        self.builder.finish_node();
        match self.found() {
            Some(found) if found.kind == SyntaxKind::Colon => {
                self.bump();
            }
            Some(found) if matches!(found.kind, SyntaxKind::Eq | SyntaxKind::ColonEq) => {
                let span = self.bump();
                self.errors
                    .push(errors::wrong_attribute_separator(found.kind, span));
            }
            // `TUPLE { A INTEGER }`, as in Tutorial D: the colon is missing.
            _ => self.missing(SyntaxKind::Colon),
        }
        item(self);
        self.builder.finish_node();
    }

    /// Whether `keyword` is next: the keyword itself, or a mis-cased spelling
    /// of it followed by `{`, as in `TUPLE {`.
    fn at_keyword(&mut self, keyword: SyntaxKind) -> bool {
        let Some(found) = self.found() else {
            return false;
        };
        if found.kind == keyword {
            return true;
        }
        let spelled = keyword.fixed_text().expect("keywords have fixed text");
        found.kind == SyntaxKind::Ident
            && found.text.eq_ignore_ascii_case(spelled)
            && self
                .found_after(1)
                .is_some_and(|f| f.kind == SyntaxKind::LBrace)
    }

    /// Consume `keyword`, reporting a mis-cased spelling of it, or record it
    /// as missing.
    fn keyword(&mut self, keyword: SyntaxKind) {
        let Some(found) = self.found() else {
            self.missing(keyword);
            return;
        };
        if found.kind == keyword {
            self.bump();
        } else if found.kind == SyntaxKind::Ident && self.at_keyword(keyword) {
            self.bump();
            let spelled = keyword.fixed_text().expect("keywords have fixed text");
            self.errors.push(errors::miscased_keyword(found, spelled));
        } else {
            self.missing(keyword);
        }
    }

    /// Whether a selector's heading starts here: a brace group that looks
    /// like a heading, followed by another brace group, the body.
    fn at_heading(&mut self) -> bool {
        self.braces_then_brace() && self.looks_like_heading()
    }

    /// Whether the brace group at the next token starts like a heading: it is
    /// empty, or its first attribute is `name: type` or Tutorial D's
    /// `NAME TYPE`. A value such as `a: 1` doesn't.
    fn looks_like_heading(&mut self) -> bool {
        let kind = |p: &mut Self, n| p.found_after(n).map(|f| f.kind);
        match (kind(self, 1), kind(self, 2), kind(self, 3)) {
            (Some(SyntaxKind::RBrace), _, _)
            | (Some(SyntaxKind::Ident), Some(SyntaxKind::Ident), _) => true,
            (Some(SyntaxKind::Ident), Some(SyntaxKind::Colon), Some(after)) => matches!(
                after,
                SyntaxKind::Ident
                    | SyntaxKind::TupleKw
                    | SyntaxKind::RelationKw
                    | SyntaxKind::LBrace
            ),
            _ => false,
        }
    }

    /// Whether the next token is `{`, and the token after its matching `}`
    /// is another `{`: a heading followed by a body.
    fn braces_then_brace(&mut self) -> bool {
        if !self.at(SyntaxKind::LBrace) {
            return false;
        }
        let mut depth = 0_usize;
        let mut significant = (self.pos..self.tokens.len())
            .map(|i| self.tokens[i].kind)
            .filter(|&k| !is_skipped(k));
        for kind in significant.by_ref() {
            match kind {
                SyntaxKind::LBrace => depth += 1,
                SyntaxKind::RBrace => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                _ => {}
            }
        }
        depth == 0 && significant.next() == Some(SyntaxKind::LBrace)
    }

    /// `open contents close`, as a `node` if given. `contents` parses what's
    /// between. A pair in `mistakes`, such as C-style `()`, is parsed the same
    /// way and reported with a fix replacing both delimiters; `what` names
    /// the construct in that error.
    fn delimited(
        &mut self,
        node: Option<SyntaxKind>,
        what: &'static str,
        delims: (SyntaxKind, SyntaxKind),
        mistakes: &[(SyntaxKind, SyntaxKind)],
        contents: impl FnOnce(&mut Self),
    ) {
        let (open, close) = delims;
        let used = if self.at(open) {
            delims
        } else if let Some(&mistake) = mistakes.iter().find(|(o, _)| self.at(*o)) {
            mistake
        } else {
            self.missing(open);
            self.missing(close);
            return;
        };
        if let Some(node) = node {
            self.start_node(node);
        }
        let open_span = self.bump();
        contents(self);
        let close_span = self.at(used.1).then(|| self.bump());
        if node.is_some() {
            self.builder.finish_node();
        }
        if used != delims {
            self.errors.push(errors::wrong_delimiter(
                what, self.form, delims, used, open_span, close_span,
            ));
        }
        if close_span.is_none() {
            self.missing(close);
        }
    }

    /// Wrap tokens that don't start a definition in an error node, up to the
    /// next definition keyword or the end of input. `first` is the next token.
    fn junk(&mut self, first: Found<'a>) {
        let name = self.found_after(1);
        let after_name = self.found_after(2);
        self.start_node(SyntaxKind::Error);
        while self.peek().is_some_and(|k| {
            !matches!(
                k,
                SyntaxKind::OperatorKw | SyntaxKind::VarKw | SyntaxKind::BuiltinKw
            )
        }) {
            self.bump();
        }
        self.builder.finish_node();
        let end = self.last_end.unwrap_or(first.span.end);
        // `VAR` or `Operator`: a definition keyword in the wrong case.
        let miscased = [SyntaxKind::OperatorKw, SyntaxKind::VarKw]
            .into_iter()
            .filter_map(SyntaxKind::fixed_text)
            .find(|k| first.kind == SyntaxKind::Ident && k.eq_ignore_ascii_case(first.text));
        self.errors.push(match miscased {
            Some(keyword) => errors::miscased_keyword(first, keyword),
            None => errors::expected_definition(
                first,
                name,
                after_name,
                Span::new(first.span.start, end),
            ),
        });
    }

    /// Start a node at the next significant token, so that trivia before it
    /// stays in the enclosing node and every node's span starts at a token.
    fn start_node(&mut self, kind: SyntaxKind) {
        self.peek();
        self.builder.start_node(kind);
    }

    /// The kind of the next significant token, or `None` at end of input.
    ///
    /// Trivia and lexer error tokens before it are added to the tree first.
    /// Lexer errors were already reported, so the grammar skips them.
    fn peek(&mut self) -> Option<SyntaxKind> {
        while let Some(&token) = self.tokens.get(self.pos) {
            if !is_skipped(token.kind) {
                return Some(token.kind);
            }
            self.add_token(token);
        }
        None
    }

    fn at(&mut self, kind: SyntaxKind) -> bool {
        self.peek() == Some(kind)
    }

    /// The next significant token.
    fn found(&mut self) -> Option<Found<'a>> {
        self.peek()?;
        Some(self.found_at(self.pos))
    }

    /// The significant token `n` after the next one, without consuming
    /// anything.
    fn found_after(&mut self, n: usize) -> Option<Found<'a>> {
        self.peek()?;
        let pos = (self.pos + 1..self.tokens.len())
            .filter(|&i| !is_skipped(self.tokens[i].kind))
            .nth(n - 1)?;
        Some(self.found_at(pos))
    }

    /// The next significant token, leaving the trivia before it for whichever
    /// node consumes it: a lookahead that may not take the token.
    fn lookahead(&self) -> Option<Found<'a>> {
        self.lookahead_nth(0)
    }

    /// The significant token `n` after the next one, or the next one for 0,
    /// as a lookahead.
    fn lookahead_nth(&self, n: usize) -> Option<Found<'a>> {
        let pos = (self.pos..self.tokens.len())
            .filter(|&i| !is_skipped(self.tokens[i].kind))
            .nth(n)?;
        Some(self.found_at(pos))
    }

    fn found_at(&self, pos: usize) -> Found<'a> {
        let token = self.tokens[pos];
        Found {
            kind: token.kind,
            span: token.span,
            text: &self.source[token.span.start..token.span.end],
        }
    }

    /// Add the next significant token to the tree and return its span.
    fn bump(&mut self) -> Span {
        self.peek();
        self.flush_missing();
        let token = self.tokens[self.pos];
        self.add_token(token);
        self.last_end = Some(token.span.end);
        token.span
    }

    fn add_token(&mut self, token: Token) {
        let text = &self.source[token.span.start..token.span.end];
        self.builder.token(token.kind, text);
        self.pos += 1;
    }

    fn expect(&mut self, kind: SyntaxKind) {
        if self.at(kind) {
            self.bump();
        } else {
            self.missing(kind);
        }
    }

    /// Record that `kind` should come before the next token.
    fn missing(&mut self, kind: SyntaxKind) {
        self.peek();
        match &mut self.missing {
            Some(missing) if missing.pos == self.pos => missing.expected.push(kind),
            _ => {
                self.flush_missing();
                self.missing = Some(Missing {
                    pos: self.pos,
                    expected: vec![kind],
                    form: self.form,
                    help: None,
                });
            }
        }
    }

    /// Like [`Parser::missing`], for something that can't be inserted
    /// mechanically, with `help` on what to write.
    fn missing_with_help(&mut self, kind: SyntaxKind, help: &'static str) {
        self.missing(kind);
        if let Some(missing) = &mut self.missing {
            missing.help.get_or_insert(help);
        }
    }

    fn flush_missing(&mut self) {
        let Some(missing) = self.missing.take() else {
            return;
        };
        let found = (missing.pos < self.tokens.len()).then(|| self.found_at(missing.pos));
        let at = self.last_end.or(found.map(|f| f.span.start)).unwrap_or(0);
        let neighbors = (
            self.source[..at].chars().next_back(),
            self.source[at..].chars().next(),
        );
        self.errors.push(errors::missing(
            &missing.expected,
            found,
            Span::empty(at),
            neighbors,
            missing.form,
            missing.help,
        ));
    }
}

fn is_skipped(kind: SyntaxKind) -> bool {
    kind.is_trivia() || kind == SyntaxKind::ErrorToken
}

#[cfg(test)]
mod tests {
    use super::*;
    use trendy_diag::Code;
    use trendy_syntax::ast::AstNode;
    use trendy_syntax::outline;

    fn parse_str(source: &str) -> Parse {
        parse(source, trendy_lexer::lex(source))
    }

    fn messages(source: &str) -> Vec<String> {
        parse_str(source)
            .errors
            .into_iter()
            .map(|d| d.message)
            .collect()
    }

    /// Apply every error's first suggestion.
    fn fix(source: &str) -> String {
        let mut edits: Vec<_> = parse_str(source)
            .errors
            .into_iter()
            .filter_map(|d| d.suggestions.into_iter().next())
            .flat_map(|s| s.edits)
            .collect();
        edits.sort_by_key(|e| std::cmp::Reverse(e.span.start));
        let mut fixed = source.to_string();
        for edit in edits {
            fixed.replace_range(edit.span.start..edit.span.end, &edit.replacement);
        }
        fixed
    }

    #[test]
    fn empty_main_tree() {
        let parse = parse_str("operator main{} [];");
        assert!(parse.errors.is_empty());
        assert_eq!(
            outline(parse.tree.syntax()),
            "\
SourceFile
  Operator
    OperatorKw \"operator\"
    Whitespace \" \"
    Name
      Ident \"main\"
    Heading
      LBrace \"{\"
      RBrace \"}\"
    Whitespace \" \"
    Body
      LBracket \"[\"
      RBracket \"]\"
    Semicolon \";\"
"
        );
    }

    #[test]
    fn whitespace_between_operators_belongs_to_the_file() {
        let parse = parse_str("operator a{}[];\n\noperator b{}[];\n");
        let kinds: Vec<_> = parse
            .tree
            .syntax()
            .children_with_tokens()
            .map(|e| e.kind())
            .collect();
        assert_eq!(
            kinds,
            [
                SyntaxKind::Operator,
                SyntaxKind::Whitespace,
                SyntaxKind::Operator,
                SyntaxKind::Whitespace
            ]
        );
    }

    #[test]
    fn missing_token_is_reported_after_the_previous_token() {
        let source = "operator main{} []\n";
        let errors = parse_str(source).errors;
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].code, Code::MissingToken);
        assert_eq!(errors[0].message, "expected `;`, found end of file");
        assert_eq!(errors[0].span(), Span::empty(18));
        assert_eq!(fix(source), "operator main{} [];\n");
    }

    #[test]
    fn missing_tokens_in_a_row_are_one_error() {
        let source = "operator a{} [\noperator main{} [];";
        let errors = parse_str(source).errors;
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].message, "expected `]`, found `operator`");
        assert_eq!(errors[0].primary.message, "expected `];` here");
        assert_eq!(
            errors[0].secondary[0].message,
            "the next definition starts here"
        );
        assert_eq!(errors[0].suggestions[0].message, "insert `];`");
        assert_eq!(fix(source), "operator a{} [];\noperator main{} [];");
    }

    #[test]
    fn missing_parameter_list() {
        let source = "operator main [];";
        assert_eq!(messages(source), ["expected `{`, found `[`"]);
        assert_eq!(fix(source), "operator main{} [];");
    }

    #[test]
    fn missing_name() {
        let parse = parse_str("operator {} [];");
        assert_eq!(parse.errors.len(), 1);
        assert_eq!(parse.errors[0].message, "expected identifier, found `{`");
        assert!(parse.errors[0].suggestions.is_empty());
        assert!(!parse.errors[0].help.is_empty());
        assert!(parse.tree.operators().next().unwrap().name().is_none());
    }

    #[test]
    fn incomplete_definition_at_end_of_file() {
        let errors = parse_str("operator").errors;
        assert_eq!(errors.len(), 1);
        assert_eq!(
            errors[0].primary.message,
            "expected identifier, then `{}[];` here"
        );
    }

    #[test]
    fn c_style_definition() {
        let source = "operator main(){}";
        let errors = parse_str(source).errors;
        let codes: Vec<_> = errors.iter().map(|e| e.code).collect();
        assert_eq!(
            codes,
            [
                Code::WrongDelimiter,
                Code::WrongDelimiter,
                Code::MissingToken
            ]
        );
        assert_eq!(
            errors[0].message,
            "the parameter list must be enclosed in `{}`, not `()`"
        );
        assert_eq!(
            errors[1].message,
            "the body must be enclosed in `[]`, not `{}`"
        );
        assert_eq!(fix(source), "operator main{}[];");
    }

    #[test]
    fn foreign_definition_keyword() {
        let source = "fn main{} [];";
        let errors = parse_str(source).errors;
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].code, Code::ExpectedDefinition);
        assert_eq!(
            errors[0].message,
            "expected a definition, found identifier `fn`"
        );
        assert_eq!(errors[0].span(), Span::new(0, 13));
        assert_eq!(fix(source), "operator main{} [];");
    }

    #[test]
    fn variable_tree() {
        let parse = parse_str(r#"var t: text := "x";"#);
        assert!(parse.errors.is_empty());
        assert_eq!(
            outline(parse.tree.syntax()),
            "\
SourceFile
  VarDecl
    VarKw \"var\"
    Whitespace \" \"
    Name
      Ident \"t\"
    TypeAnnotation
      Colon \":\"
      Whitespace \" \"
      TypeRef
        Ident \"text\"
    Whitespace \" \"
    Initializer
      ColonEq \":=\"
      Whitespace \" \"
      Literal
        TextLiteral \"\\\"x\\\"\"
    Semicolon \";\"
"
        );
    }

    #[test]
    fn keys_tree() {
        let parse = parse_str("var r := s key { a, b } key {};");
        assert!(parse.errors.is_empty());
        let decl = parse.tree.variables().next().unwrap();
        let keys: Vec<Vec<String>> = decl
            .keys()
            .map(|k| {
                k.names()
                    .unwrap()
                    .names()
                    .map(|n| n.ident().unwrap().text().to_string())
                    .collect()
            })
            .collect();
        assert_eq!(keys, [vec!["a", "b"], vec![]]);
        assert!(decl.initializer().is_some());
    }

    #[test]
    fn keys_follow_the_type_or_the_value() {
        for source in [
            "var r: relation { a: integer } key { a };",
            "var r := relation { { a: 1 } } key { a } key { a };",
            "var r: relation { a: integer } := relation {} key { a };",
            "var r := s{a} key { a };",
        ] {
            let parse = parse_str(source);
            assert!(parse.errors.is_empty(), "{source}: {:#?}", parse.errors);
            assert!(
                parse
                    .tree
                    .variables()
                    .next()
                    .unwrap()
                    .keys()
                    .next()
                    .is_some()
            );
        }
    }

    #[test]
    fn key_is_a_name_elsewhere() {
        let parse =
            parse_str("var key := relation { { key: 1 } } key { key };\nvar k := key{key};");
        assert!(parse.errors.is_empty(), "{:#?}", parse.errors);
        let decls: Vec<_> = parse.tree.variables().collect();
        assert_eq!(decls[0].keys().count(), 1);
        assert_eq!(decls[1].keys().count(), 0);
    }

    #[test]
    fn key_before_the_value_is_moved_after_it() {
        let source = "var r: relation { a: integer } key { a } := relation {};";
        assert_eq!(messages(source), ["a key goes after the initial value"]);
        assert_eq!(
            fix(source),
            "var r: relation { a: integer } := relation {} key { a };"
        );
        let codes: Vec<Code> = parse_str("var r key { a } := s;")
            .errors
            .iter()
            .map(|d| d.code)
            .collect();
        assert_eq!(codes, [Code::KeyBeforeValue]);
    }

    #[test]
    fn keys_as_sql_writes_them() {
        let source = "var r := s PRIMARY KEY (a) unique { b } KEY { c };";
        let codes: Vec<Code> = parse_str(source).errors.iter().map(|d| d.code).collect();
        assert_eq!(
            codes,
            [
                Code::KeySpelling,
                Code::WrongDelimiter,
                Code::KeySpelling,
                Code::MiscasedKeyword
            ]
        );
        assert_eq!(fix(source), "var r := s key {a} key { b } key { c };");
    }

    #[test]
    fn variable_parts_are_optional() {
        for source in ["var i: integer;", "var i := 0;", "var i;"] {
            assert!(parse_str(source).errors.is_empty(), "{source}");
        }
        let decl = parse_str("var i := 0;").tree.variables().next().unwrap();
        assert!(decl.type_expr().is_none());
        assert!(decl.initializer().is_some());
    }

    #[test]
    fn variables_and_operators_interleave() {
        let parse = parse_str("var a := 1;\noperator main{} [];\nvar b := 2;");
        assert!(parse.errors.is_empty());
        assert_eq!(parse.tree.items().count(), 3);
    }

    #[test]
    fn eq_for_assignment() {
        let source = "var i: integer = 0;";
        let errors = parse_str(source).errors;
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].code, Code::AssignmentWithEq);
        assert_eq!(errors[0].span(), Span::new(15, 16));
        assert_eq!(fix(source), "var i: integer := 0;");
    }

    #[test]
    fn missing_colon_before_type() {
        let source = "var i integer := 0;";
        assert_eq!(
            messages(source),
            ["expected `:`, found identifier `integer`"]
        );
        assert_eq!(fix(source), "var i: integer := 0;");
    }

    #[test]
    fn missing_assignment_before_value() {
        assert_eq!(fix("var i: integer 0;"), "var i: integer:= 0;");
    }

    #[test]
    fn missing_type_after_colon() {
        let errors = parse_str("var i: := 0;").errors;
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].message, "expected type, found `:=`");
        assert!(errors[0].help[0].contains("such as `integer`"));
        assert!(errors[0].notes[0].starts_with("a variable declaration has the form"));
    }

    #[test]
    fn missing_value_after_assignment() {
        let errors = parse_str("var i :=;").errors;
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].message, "expected value, found `;`");
        assert!(errors[0].help[0].contains("such as `0`"));
    }

    #[test]
    fn foreign_variable_keywords() {
        assert_eq!(fix("let x = 0;"), "var x = 0;");
        assert_eq!(fix("const x: integer := 0;"), "var x: integer := 0;");
        assert_eq!(fix("int x;"), "var x;");
        assert_eq!(fix("int main() {}"), "operator main() {}");
    }

    #[test]
    fn junk_stops_at_var() {
        let parse = parse_str("junk var i := 0;");
        assert_eq!(parse.errors.len(), 1);
        assert_eq!(parse.tree.variables().count(), 1);
    }

    #[test]
    fn approximate_initializer() {
        let parse = parse_str("var r: approximate := 1.5e-3;");
        assert!(parse.errors.is_empty());
        assert!(
            parse
                .tree
                .variables()
                .next()
                .unwrap()
                .initializer()
                .is_some()
        );
    }

    #[test]
    fn boolean_literals() {
        assert!(
            parse_str("var a := true;\nvar b: boolean := false;")
                .errors
                .is_empty()
        );
    }

    #[test]
    fn miscased_booleans() {
        let source = "var a := True;\nvar b := FALSE;";
        let errors = parse_str(source).errors;
        assert_eq!(errors.len(), 2);
        assert_eq!(errors[0].code, Code::MiscasedKeyword);
        assert_eq!(fix(source), "var a := true;\nvar b := false;");
    }

    #[test]
    fn tuple_type_tree() {
        let parse = parse_str("var x: tuple { a: integer, };");
        assert!(parse.errors.is_empty(), "{:?}", parse.errors);
        assert_eq!(
            outline(parse.tree.syntax()),
            "\
SourceFile
  VarDecl
    VarKw \"var\"
    Whitespace \" \"
    Name
      Ident \"x\"
    TypeAnnotation
      Colon \":\"
      Whitespace \" \"
      TupleType
        TupleKw \"tuple\"
        Whitespace \" \"
        Heading
          LBrace \"{\"
          Whitespace \" \"
          AttributeType
            Name
              Ident \"a\"
            Colon \":\"
            Whitespace \" \"
            TypeRef
              Ident \"integer\"
          Comma \",\"
          Whitespace \" \"
          RBrace \"}\"
    Semicolon \";\"
"
        );
    }

    #[test]
    fn tuples_nest_in_types_and_values() {
        for source in [
            "var x: tuple { a: tuple { b: boolean }, c: text };",
            "var q := tuple { x: 1e0, y: tuple { z: \"\" } };",
            "var e: tuple {} := tuple {};",
            "var t: tuple { a: integer } := tuple { a: 1, };",
        ] {
            assert!(parse_str(source).errors.is_empty(), "{source}");
        }
    }

    #[test]
    fn tuple_recovery() {
        let one_fix = |source: &str, fixed: &str| {
            let errors = parse_str(source).errors;
            assert_eq!(errors.len(), 1, "{source}: {errors:#?}");
            assert_eq!(fix(source), fixed, "{source}");
        };
        // Tutorial D: no colon.
        one_fix(
            "var p: tuple { x approximate };",
            "var p: tuple { x: approximate };",
        );
        // Missing comma.
        one_fix(
            "var p := tuple { x: 1 y: 2 };",
            "var p := tuple { x: 1, y: 2 };",
        );
        // `=` as the separator.
        one_fix("var p := tuple { x = 1 };", "var p := tuple { x : 1 };");
        // A JavaScript object literal.
        one_fix("var p := { x: 1 };", "var p :=tuple { x: 1 };");
        one_fix("var p: { x: integer };", "var p:tuple { x: integer };");
        // Missing closing brace.
        one_fix("var p := tuple { x: 1;", "var p := tuple { x: 1};");
    }

    #[test]
    fn miscased_keywords() {
        let codes = |source: &str| -> Vec<Code> {
            parse_str(source).errors.iter().map(|e| e.code).collect()
        };
        assert_eq!(
            codes("var p: TUPLE { x: approximate };"),
            [Code::MiscasedKeyword]
        );
        assert_eq!(fix("var p := Tuple { x: 1 };"), "var p := tuple { x: 1 };");
        assert_eq!(codes("VAR x := 1;"), [Code::MiscasedKeyword]);
        assert_eq!(fix("VAR x := 1;"), "var x := 1;");
        assert_eq!(fix("OPERATOR main{} [];"), "operator main{} [];");
        // `TUPLE` alone is just a (wrong) type name.
        assert!(codes("var p: TUPLE;").is_empty());
    }

    #[test]
    fn relations() {
        for source in [
            "var r: relation { a: boolean, b: integer };",
            "var r := relation { a: integer } { { a: 1 } };",
            "var r := relation { tuple { a: 1 }, { a: 2 }, };",
            "var r := relation {};",
            "var r := relation {} {};",
            "var t := tuple { a: integer } { a: 1 };",
            "var n: tuple { r: relation { x: approximate } };",
            "var n := relation { { r: relation { { x: 1e0 } } } };",
        ] {
            let parse = parse_str(source);
            assert!(parse.errors.is_empty(), "{source}: {:#?}", parse.errors);
        }
    }

    #[test]
    fn heading_or_body() {
        let literal = |source: &str| {
            let decl = parse_str(source).tree.variables().next().unwrap();
            match decl.initializer() {
                Some(trendy_syntax::ast::Expr::Relation(r)) => r,
                other => panic!("{source}: {other:?}"),
            }
        };
        let with_heading = literal("var r := relation { a: integer } { { a: 1 } };");
        assert!(with_heading.heading().is_some());
        assert_eq!(with_heading.tuples().count(), 1);
        let without = literal("var r := relation { { a: 1 }, { a: 2 } };");
        assert!(without.heading().is_none());
        assert_eq!(without.tuples().count(), 2);
    }

    #[test]
    fn relation_recovery() {
        // A heading with no body: the empty body is inserted.
        let source = "var r := relation { a: integer };";
        let errors = parse_str(source).errors;
        assert_eq!(errors.len(), 1, "{errors:#?}");
        assert_eq!(fix(source), "var r := relation { a: integer }{};");
        // Missing comma between tuples, with or without the keyword.
        assert_eq!(
            fix("var r := relation { tuple { a: 1 } tuple { a: 2 } };"),
            "var r := relation { tuple { a: 1 }, tuple { a: 2 } };"
        );
        assert_eq!(
            fix("var r := relation { { a: 1 } { a: 2 } };"),
            "var r := relation { { a: 1 }, { a: 2 } };"
        );
        assert_eq!(
            fix("var r: RELATION { a: integer };"),
            "var r: relation { a: integer };"
        );
    }

    #[test]
    fn nodes_start_at_tokens() {
        for source in [
            "var x: tuple { a: integer } := tuple { a: 1 };",
            "var r := relation { a: integer } { { a: 1 } , tuple { a: 2 } };",
            "operator main ( ) { } ;",
            "var p := { x = 1 y: 2 };",
            "junk  var  i ;",
        ] {
            let tree = parse_str(source).tree;
            for node in tree.syntax().descendants().skip(1) {
                let first = node.first_child_or_token().map(|e| e.kind());
                assert_ne!(
                    first,
                    Some(SyntaxKind::Whitespace),
                    "{source}: {:?} starts with whitespace",
                    node.kind()
                );
            }
        }
    }

    #[test]
    fn calls_and_parameters() {
        for source in [
            "var r: relation { a: boolean, b: integer } := relation {{a:true, b:3}, {b:25, a:false}};\n\nbuiltin operator debug_value{v:any};\n\noperator main{} [\n    call debug_value{v: r};\n];",
            "operator greet{name: text, count: integer} [ call debug_value{v: name}; call debug_value{v: tuple { n: count }}; ];",
            "operator main{} [ call greet{name: \"Ada\", count: 2,}; ];",
            "operator main{} [ call f{}; ];",
        ] {
            let parse = parse_str(source);
            assert!(parse.errors.is_empty(), "{source}: {:#?}", parse.errors);
        }
        let parse = parse_str("operator greet{name: text} [ call debug_value{v: name}; ];");
        let op = parse.tree.operators().next().unwrap();
        assert_eq!(op.params().unwrap().attributes().count(), 1);
        let call = op.body().unwrap().statements().next().unwrap();
        assert_eq!(
            call.callee().unwrap().ident().unwrap().text(),
            "debug_value"
        );
        assert_eq!(call.arguments().unwrap().named().count(), 1);
    }

    #[test]
    fn positional_arguments_parse_for_sema_to_fix() {
        let parse = parse_str("operator main{} [ call debug_value{r}; ];");
        assert!(parse.errors.is_empty(), "{:#?}", parse.errors);
        let call = parse
            .tree
            .operators()
            .next()
            .unwrap()
            .body()
            .unwrap()
            .statements()
            .next()
            .unwrap();
        assert_eq!(call.arguments().unwrap().positional().count(), 1);
    }

    #[test]
    fn builtin_declarations() {
        let parse = parse_str("builtin operator debug_value{v: any};");
        assert!(parse.errors.is_empty(), "{:#?}", parse.errors);
        assert!(matches!(
            parse.tree.items().next(),
            Some(trendy_syntax::ast::Item::Builtin(_))
        ));
    }

    #[test]
    fn call_recovery() {
        let one_fix = |source: &str, fixed: &str| {
            let errors = parse_str(source).errors;
            assert_eq!(errors.len(), 1, "{source}: {errors:#?}");
            assert_eq!(fix(source), fixed, "{source}");
        };
        one_fix(
            "operator main{} [ debug_value{v: 1}; ];",
            "operator main{} [ call debug_value{v: 1}; ];",
        );
        one_fix(
            "operator main{} [ call debug_value(v: 1); ];",
            "operator main{} [ call debug_value{v: 1}; ];",
        );
        one_fix("operator f(x: integer) [];", "operator f{x: integer} [];");
        let codes: Vec<_> = parse_str("operator main() { call f{}; }")
            .errors
            .iter()
            .map(|e| e.code)
            .collect();
        assert_eq!(
            codes,
            [
                Code::WrongDelimiter,
                Code::WrongDelimiter,
                Code::MissingToken
            ]
        );
        assert_eq!(
            fix("operator main() { call f{}; }"),
            "operator main{} [ call f{}; ];"
        );
    }

    #[test]
    fn statements_that_arent_calls() {
        let parse = parse_str("operator main{} [ 42; call f{}; ];");
        assert_eq!(parse.errors.len(), 1, "{:#?}", parse.errors);
        assert_eq!(
            parse.errors[0].message,
            "expected `call`, found integer literal"
        );
        assert_eq!(
            parse
                .tree
                .operators()
                .next()
                .unwrap()
                .body()
                .unwrap()
                .statements()
                .count(),
            1
        );
    }

    #[test]
    fn inserted_keywords_are_separated_from_words() {
        assert_eq!(
            fix("builtin debug_value{v: any};"),
            "builtin operator debug_value{v: any};"
        );
    }

    /// The expression in `var x := <source>;`, as an outline of its
    /// structure: `(a and (b or c))`.
    fn shape(source: &str) -> String {
        fn show(expr: &trendy_syntax::ast::Expr) -> String {
            use trendy_syntax::ast::Expr;
            match expr {
                Expr::Binary(b) => format!(
                    "({} {} {})",
                    show(&b.lhs().unwrap()),
                    b.operator().unwrap().text(),
                    show(&b.rhs().unwrap())
                ),
                Expr::Unary(u) => format!(
                    "({} {})",
                    u.operator().unwrap().text(),
                    show(&u.operand().unwrap())
                ),
                Expr::Paren(p) => show(&p.inner().unwrap()),
                other => other.syntax().to_string().trim().to_string(),
            }
        }
        let source = format!("var x := {source};");
        let parse = parse_str(&source);
        assert!(parse.errors.is_empty(), "{source}: {:#?}", parse.errors);
        show(
            &parse
                .tree
                .variables()
                .next()
                .unwrap()
                .initializer()
                .unwrap(),
        )
    }

    #[test]
    fn precedence() {
        assert_eq!(shape("a and b or c"), "((a and b) or c)");
        assert_eq!(shape("a or b and c"), "(a or (b and c))");
        assert_eq!(shape("a or b xor c"), "(a or (b xor c))");
        assert_eq!(shape("a xor b and c"), "(a xor (b and c))");
        assert_eq!(shape("a and b and c"), "((a and b) and c)");
        assert_eq!(shape("a implies b implies c"), "(a implies (b implies c))");
        assert_eq!(shape("a or b implies c"), "((a or b) implies c)");
        assert_eq!(shape("not a and b"), "((not a) and b)");
        assert_eq!(shape("not not a"), "(not (not a))");
        assert_eq!(shape("a and (b or c)"), "(a and (b or c))");
        assert_eq!(shape("(a)"), "a");
    }

    #[test]
    fn arithmetic_precedence() {
        assert_eq!(shape("a + b * c"), "(a + (b * c))");
        assert_eq!(shape("a * b + c"), "((a * b) + c)");
        assert_eq!(shape("a - b - c"), "((a - b) - c)");
        assert_eq!(shape("a div b mod c"), "((a div b) mod c)");
        assert_eq!(shape("-a * b"), "((- a) * b)");
        assert_eq!(shape("- -a"), "(- (- a))");
        assert_eq!(shape("+a - -1"), "((+ a) - (- 1))");
        assert_eq!(shape("a - (b - c)"), "(a - (b - c))");
        // Prefix operators bind tightest, `not` included.
        assert_eq!(shape("not a + b"), "((not a) + b)");
        assert_eq!(shape("not a and b + c"), "((not a) and (b + c))");
    }

    #[test]
    fn foreign_and_miscased_operators() {
        let source = "var x := a && !b | c -> d;";
        let codes: Vec<_> = parse_str(source).errors.iter().map(|e| e.code).collect();
        assert_eq!(codes, [Code::ForeignOperator; 4]);
        assert_eq!(fix(source), "var x := a and not b or c implies d;");
        assert_eq!(
            fix("var x := a AND NOT b Or c;"),
            "var x := a and not b or c;"
        );
    }

    #[test]
    fn projections() {
        let source = "var x := -a{b, c} remove {c,} and b remove {} and c{};";
        let parse = parse_str(source);
        assert!(parse.errors.is_empty(), "{:#?}", parse.errors);
        let nodes = |kind| {
            parse
                .tree
                .syntax()
                .descendants()
                .filter(|n| n.kind() == kind)
                .map(|n| n.to_string().trim().to_string())
                .collect::<Vec<_>>()
        };
        // Postfix binds tightest: `-` and `and` take the projections.
        assert_eq!(nodes(SyntaxKind::ProjectExpr), ["a{b, c}", "c{}"]);
        assert_eq!(
            nodes(SyntaxKind::PostfixExpr),
            ["a{b, c} remove {c,}", "b remove {}"]
        );
        assert_eq!(nodes(SyntaxKind::NameList), ["{b, c}", "{c,}", "{}", "{}"]);
        assert_eq!(fix("var x := a REMOVE {b};"), "var x := a remove {b};");
        assert_eq!(fix("var x := a remove (b);"), "var x := a remove {b};");
        assert_eq!(messages("var x := a remove;"), ["expected `{`, found `;`"]);
        // `project` is a name again.
        assert!(parse_str("var project := a{b};").errors.is_empty());
    }

    #[test]
    fn keywords_as_names() {
        let source = "var tuple := 1;\noperator remove{} [];\noperator main{} [ call remove{}; ];";
        let errors = parse_str(source).errors;
        let found: Vec<_> = errors.iter().map(|e| (e.code, &e.message[..])).collect();
        assert_eq!(
            found,
            [
                (
                    Code::KeywordAsName,
                    "`tuple` is a keyword, so it can't be a name"
                ),
                (
                    Code::KeywordAsName,
                    "`remove` is a keyword, so it can't be a name"
                ),
                (
                    Code::KeywordAsName,
                    "`remove` is a keyword, so it can't be a name"
                ),
            ],
            "and nothing cascades"
        );
        // A missing name before the next definition stays a missing name.
        assert_eq!(
            parse_str("operator\nvar x := 1;").errors[0].code,
            Code::MissingToken
        );
    }

    #[test]
    fn extractions() {
        let parse = parse_str("var x := -t.a.b + u{c}.c;");
        assert!(parse.errors.is_empty(), "{:#?}", parse.errors);
        let nodes: Vec<_> = parse
            .tree
            .syntax()
            .descendants()
            .filter(|n| n.kind() == SyntaxKind::ExtractExpr)
            .map(|n| n.to_string().trim().to_string())
            .collect();
        assert_eq!(nodes, ["t.a.b", "t.a", "u{c}.c"]);
        assert_eq!(shape("-t.a * 2"), "((- t.a) * 2)");
        assert_eq!(messages("var x := t.;"), ["expected identifier, found `;`"]);
    }

    #[test]
    fn renamings() {
        let source = "var x := a rename {b as c, d as e} rename {c as b};";
        let parse = parse_str(source);
        assert!(parse.errors.is_empty(), "{:#?}", parse.errors);
        let renamings: Vec<_> = parse
            .tree
            .syntax()
            .descendants()
            .filter_map(trendy_syntax::ast::Renaming::cast)
            .map(|r| {
                let name = |n: Option<trendy_syntax::ast::Name>| n.unwrap().syntax().to_string();
                (name(r.before()), name(r.after()))
            })
            .collect();
        assert_eq!(
            renamings,
            [
                ("b".into(), "c".into()),
                ("d".into(), "e".into()),
                ("c".into(), "b".into())
            ]
        );
        // Habits from Tutorial D and from mappings.
        assert_eq!(
            fix("var x := a RENAME ( postal_code AS code );"),
            "var x := a rename { postal_code as code };"
        );
        assert_eq!(
            fix("var x := a rename {postal_code: code};"),
            "var x := a rename {postal_code as code};"
        );
        assert_eq!(
            fix("var x := a rename {postal_code:code};"),
            "var x := a rename {postal_code as code};"
        );
        assert_eq!(
            messages("var x := a rename {postal_code code};"),
            ["expected `as`, found identifier `code`"]
        );
    }

    #[test]
    fn comparisons() {
        assert_eq!(shape("a + 1 < b * 2"), "((a + 1) < (b * 2))");
        assert_eq!(shape("not a = b"), "((not a) = b)");
        assert_eq!(shape("not (a = b)"), "(not (a = b))");
        assert_eq!(shape("a <> b and c >= d"), "((a <> b) and (c >= d))");
        assert_eq!(shape("a = (b = c)"), "(a = (b = c))");
        assert_eq!(
            fix("var x := a == b or c != d;"),
            "var x := a = b or c <> d;"
        );
    }

    #[test]
    fn joins() {
        assert_eq!(shape("a join b join c"), "((a join b) join c)");
        assert_eq!(shape("a times b compose c"), "((a times b) compose c)");
        assert_eq!(shape("a join b = c"), "((a join b) = c)");
        assert_eq!(shape("a join b{x}"), "(a join b{x})");
        assert_eq!(
            shape("a join b rename {x as y}"),
            "(a join b rename {x as y})"
        );
        assert_eq!(shape("a || b join c"), "((a || b) join c)");
        assert_eq!(shape("a join (b join c)"), "(a join (b join c))");
        assert_eq!(fix("var x := a JOIN b;"), "var x := a join b;");
    }

    #[test]
    fn joins_of_lists() {
        assert_eq!(shape("join{a, b, c}"), "join{a, b, c}");
        assert_eq!(shape("join{a, b} join c"), "(join{a, b} join c)");
        assert_eq!(shape("a join times{b, c}"), "(a join times{b, c})");
        assert_eq!(shape("join{}"), "join{}");
        assert_eq!(shape("times{a,}"), "times{a,}");
        assert_eq!(shape("join{a, b}{x}"), "join{a, b}{x}");
        let parse = parse_str("var x := join{a, b join c, (d)};");
        let nary = parse
            .tree
            .syntax()
            .descendants()
            .find_map(trendy_syntax::ast::NaryExpr::cast)
            .unwrap();
        assert_eq!(nary.operator().unwrap().kind(), SyntaxKind::JoinKw);
        assert_eq!(nary.operands().count(), 3);
        // Tutorial D's capitals, and a call's parentheses.
        assert_eq!(fix("var x := JOIN { a, b };"), "var x := join { a, b };");
        assert_eq!(fix("var x := times(a, b);"), "var x := times{a, b};");
        assert_eq!(
            messages("var x := join{a b};"),
            ["expected `,`, found identifier `b`"]
        );
        // Braces for what can be in any order, brackets for what can't.
        assert_eq!(shape("compose[a, b, c]"), "compose[a, b, c]");
        assert_eq!(fix("var x := compose{a, b};"), "var x := compose[a, b];");
        assert_eq!(fix("var x := join[a, b];"), "var x := join{a, b};");
        assert_eq!(fix("var x := compose(a, b);"), "var x := compose[a, b];");
    }

    #[test]
    fn assignments() {
        let source = "var x := r extend { total := price * quantity, label := name || \"!\" };";
        let parse = parse_str(source);
        assert!(parse.errors.is_empty(), "{:#?}", parse.errors);
        let assignments: Vec<_> = parse
            .tree
            .syntax()
            .descendants()
            .filter_map(trendy_syntax::ast::Assign::cast)
            .map(|e| {
                (
                    e.name().unwrap().syntax().to_string(),
                    e.value().unwrap().syntax().to_string(),
                )
            })
            .collect();
        assert_eq!(
            assignments,
            [
                ("total".into(), "price * quantity".into()),
                ("label".into(), "name || \"!\"".into())
            ]
        );
        // Postfix, binding tightest, like `rename`.
        assert_eq!(
            shape("a join b extend { c := 1 }"),
            "(a join b extend { c := 1 })"
        );
        // Tuples' `:`, keyword arguments' `=`, and SQL's and Tutorial D's
        // `AS`.
        assert_eq!(
            fix("var x := r extend { t: a, u = b };"),
            "var x := r extend { t := a, u := b };"
        );
        assert_eq!(
            fix("var x := r extend { a + b as t, c AS u };"),
            "var x := r extend { t := a + b, u := c };"
        );
        assert_eq!(
            fix("var x := r EXTEND (t := a);"),
            "var x := r extend {t := a};"
        );
        assert_eq!(
            messages("var x := r extend { a + b };"),
            ["a new attribute needs a name"]
        );
        assert_eq!(
            fix("var x := r extend { a + b };"),
            "var x := r extend { a + b };",
            "no mechanical fix for a missing name"
        );
        // `replace` takes the same assignments, to attributes that exist.
        assert_eq!(
            shape("a join b replace { c := 1 }"),
            "(a join b replace { c := 1 })"
        );
        assert_eq!(
            fix("var x := r REPLACE (a = 5);"),
            "var x := r replace {a := 5};"
        );
        assert_eq!(
            messages("var x := r replace { 5 };"),
            ["an attribute's new value needs the attribute's name"]
        );
    }

    #[test]
    fn groupings() {
        let parse = parse_str("var x := r group { g := {a, b}, h := {c} };");
        assert!(parse.errors.is_empty(), "{:#?}", parse.errors);
        let groupings: Vec<_> = parse
            .tree
            .syntax()
            .descendants()
            .filter_map(trendy_syntax::ast::Grouping::cast)
            .map(|g| {
                (
                    g.name().unwrap().syntax().to_string(),
                    g.names().unwrap().names().count(),
                )
            })
            .collect();
        assert_eq!(groupings, [("g".into(), 2), ("h".into(), 1)]);
        // A tuple's `:` and keyword arguments' `=`, Tutorial D's `AS`, and
        // capitals.
        assert_eq!(
            fix("var x := r group { g: {a}, h = {b} };"),
            "var x := r group { g := {a}, h := {b} };"
        );
        assert_eq!(
            fix("var x := r GROUP ({a, b} AS g);"),
            "var x := r group {g := {a, b}};"
        );
        // A missing `:=` is inserted; the formatter spaces it.
        assert_eq!(
            fix("var x := r group { g {a} };"),
            "var x := r group { g:= {a} };"
        );
        // SQL's `group by` names the other attributes: no mechanical fix.
        assert_eq!(
            messages("var x := r group by {c};"),
            ["`group` names the attributes to gather, not those to group by"]
        );
        assert_eq!(
            messages("var x := r group { {a} };"),
            ["a grouping needs a name"]
        );
    }

    #[test]
    fn lists_in_brackets() {
        // Brackets ask for an order: the fix is the chain that applies the
        // items in order, not braces, which would apply them all at once.
        for (source, fixed) in [
            (
                "var x := r group [g := {a}, h := {b}];",
                "var x := r group { g := {a} } group { h := {b} };",
            ),
            (
                "var x := r extend [a := 1, b := a];",
                "var x := r extend { a := 1 } extend { b := a };",
            ),
            (
                "var x := r replace [a := b, b := a];",
                "var x := r replace { a := b } replace { b := a };",
            ),
            // One item has no order to keep.
            (
                "var x := r group [g := {a}];",
                "var x := r group { g := {a} };",
            ),
        ] {
            assert_eq!(
                messages(source)[0],
                format!(
                    "`{}` takes its list in braces, applied all at once",
                    source.split_whitespace().nth(4).unwrap()
                ),
                "{source}"
            );
            assert_eq!(fix(source), fixed);
            assert!(parse_str(fixed).errors.is_empty(), "{fixed}");
        }
    }

    #[test]
    fn images() {
        let parse = parse_str("var x := r extend { y := !!s };");
        assert!(parse.errors.is_empty(), "{:#?}", parse.errors);
        let image = parse
            .tree
            .syntax()
            .descendants()
            .find_map(trendy_syntax::ast::ImageExpr::cast)
            .unwrap();
        assert_eq!(image.operand().unwrap().syntax().to_string(), "s");
        // Its operand is a single one; an operator after it that could
        // apply to either the image or the operand needs parentheses.
        for (source, applied) in [
            ("!!r{a}", "a projection"),
            ("!!r.a", "attribute extraction, `.`,"),
            ("!!r rename {a as b}", "`rename`"),
            ("!!r join s", "`join`"),
            ("!!r remove {a} times s", "`remove`"),
        ] {
            let source = format!("var x := t extend {{ y := {source} }};");
            assert_eq!(
                messages(&source),
                [format!(
                    "`!!` and {applied} need parentheses to say which applies first"
                )],
                "{source}"
            );
        }
        let source = "var x := t extend { y := !!r remove {a} times s };";
        let fixes: Vec<_> = parse_str(source).errors[0]
            .suggestions
            .iter()
            .map(|suggestion| {
                let mut fixed = source.to_string();
                for edit in suggestion.edits.iter().rev() {
                    fixed.replace_range(edit.span.start..edit.span.end, &edit.replacement);
                }
                fixed
            })
            .collect();
        // One reading for each place the image can be taken.
        assert_eq!(
            fixes,
            [
                "var x := t extend { y := (!!r) remove {a} times s };",
                "var x := t extend { y := (!!(r remove {a})) times s };",
                "var x := t extend { y := !!(r remove {a} times s) };",
            ]
        );
        for fixed in &fixes {
            assert!(parse_str(fixed).errors.is_empty(), "{fixed}");
        }
        // Parenthesized either way, and with comparisons, which can only
        // apply to the image, it's clear.
        for source in [
            "var x := t extend { y := (!!r){a} };",
            "var x := t extend { y := !!(r{a}) };",
            "var x := t extend { y := !!r = s };",
            "var x := t extend { y := s join !!r };",
        ] {
            assert!(parse_str(source).errors.is_empty(), "{source}");
        }
        // `!!` is one token only with the `!`s together: apart, each is C's
        // `not`.
        assert_eq!(
            fix("var x := r extend { y := ! !s };"),
            "var x := r extend { y := not not s };"
        );
        assert_eq!(
            messages("var x := r extend { y := !! };"),
            ["expected identifier, found `}`"]
        );
    }

    #[test]
    fn joins_as_sql_writes_them() {
        assert_eq!(fix("var x := a natural join b;"), "var x := a join b;");
        assert_eq!(fix("var x := a NATURAL JOIN b;"), "var x := a join b;");
        assert_eq!(fix("var x := a inner join b;"), "var x := a join b;");
        assert_eq!(fix("var x := a cross join b;"), "var x := a times b;");
        assert_eq!(
            fix("var x := a join b using (c, d) join e;"),
            "var x := a join b join e;"
        );
        assert_eq!(
            messages("var x := a left join b;"),
            ["there is no `left join`"]
        );
        assert_eq!(
            messages("var x := a full outer join b;"),
            ["there is no `full outer join`"]
        );
        // The whole condition is consumed, so nothing else cascades, and a
        // join after it is still parsed.
        assert_eq!(
            messages("var x := a join b on a.id = b.a_id and a.y = b.y join c on c.z = b.z;"),
            ["a join has no `on`", "a join has no `on`"]
        );
        // Names that are SQL's words are still names elsewhere.
        assert_eq!(shape("left join right"), "(left join right)");
        assert_eq!(shape("natural join cross"), "(natural join cross)");
    }

    #[test]
    fn comments_go_anywhere_whitespace_does() {
        let source = "/* a */ var /* b */ x /* c */ : /* d */ integer := /* e */ - /* f */ 1 + /* g */ (2) /* h */; /* i */\noperator main{ /* j */ } [ /* k */ call /* l */ f{ /* m */ v: 1 /* n */ } /* o */ ; ] /* p */ ;";
        let parse = parse_str(source);
        assert!(parse.errors.is_empty(), "{:#?}", parse.errors);
        assert_eq!(parse.tree.syntax().to_string(), source, "lossless");
        assert_eq!(
            fix("var x := /* note */ 1 + ;"),
            "var x := /* note */ 1 + ;",
            "no mechanical fix for a missing operand"
        );
        // A missing `;` goes after the code, not after a comment.
        assert_eq!(
            fix("var x := 1 /* one */\nvar y := 2;"),
            "var x := 1; /* one */\nvar y := 2;"
        );
    }

    #[test]
    fn comparisons_dont_chain() {
        let errors = parse_str("var x := a < b + 1 <= c;").errors;
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].code, Code::ChainedComparison);
        assert_eq!(
            fix("var x := a < b + 1 <= c;"),
            "var x := a < b + 1 and b + 1 <= c;"
        );
        assert_eq!(fix("var x := a<b<c;"), "var x := a<b and b <c;");
        assert_eq!(
            fix("var x := a = b = c = d;"),
            "var x := a = b and b = c and c = d;"
        );
    }

    #[test]
    fn foreign_arithmetic_operators() {
        let source = "var x := a/b % c;";
        let errors = parse_str(source).errors;
        let codes: Vec<_> = errors.iter().map(|e| e.code).collect();
        assert_eq!(
            codes,
            [Code::ForeignOperator],
            "`/` is approximate division"
        );
        assert_eq!(
            errors[0].notes[0],
            "the remainder of integer division is the keyword `mod`"
        );
        assert_eq!(shape("a / b * c"), "((a / b) * c)");
        assert_eq!(shape("a || b || c"), "((a || b) || c)");
        assert_eq!(shape("a || b and c"), "((a || b) and c)");
        assert_eq!(fix(source), "var x := a/b mod c;");
        assert_eq!(fix("var x := a DIV b Mod c;"), "var x := a div b mod c;");
    }

    #[test]
    fn incomplete_expressions() {
        assert_eq!(messages("var x := a and;"), ["expected value, found `;`"]);
        assert_eq!(fix("var x := (a or b;"), "var x := (a or b);");
    }

    #[test]
    fn malformed_literal_is_one_error() {
        let errors = parse_str("var i := 1_000;").errors;
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].code, Code::InvalidInteger);
        assert_eq!(parse_str("var t := \"abc;").errors.len(), 1);
    }

    #[test]
    fn misspelled_keyword() {
        assert_eq!(fix("opertor main{} [];"), "operator main{} [];");
    }

    #[test]
    fn unrelated_junk_has_no_fix() {
        let source = "junk ] operator main{} [];";
        let parse = parse_str(source);
        assert_eq!(parse.errors.len(), 1);
        assert!(parse.errors[0].suggestions.is_empty());
        let names: Vec<_> = parse
            .tree
            .operators()
            .map(|o| o.name().unwrap().ident().unwrap().text().to_string())
            .collect();
        assert_eq!(names, ["main"]);
    }

    #[test]
    fn errors_are_in_source_order() {
        assert_eq!(
            messages("operator a{} [ operator # b{}[];"),
            ["expected `]`, found `operator`", "unexpected character `#`",]
        );
    }

    #[test]
    fn lossless_for_any_input() {
        for source in [
            "",
            "operator main{} [];",
            "  operator\n main {\t} [ ] ;  \n",
            "operator main{}\n  [;",
            "}{][;; operator operator x ¿ é {",
            "operator main(){}",
            "void main() { return 0; }",
        ] {
            assert_eq!(parse_str(source).tree.syntax().to_string(), source);
        }
    }
}
