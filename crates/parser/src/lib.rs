mod errors;

use trendy_diag::{Diagnostic, Span};
use trendy_lexer::{Lexed, Token};
use trendy_syntax::ast::SourceFile;
use trendy_syntax::{SyntaxKind, TreeBuilder};

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

    let mut errors = parser.errors;
    errors.sort_by_key(|d| d.span().start);
    Parse {
        tree: parser.builder.finish(),
        errors,
    }
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

/// The precedence of the comparison operators.
const COMPARISON: u8 = 6;

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
        if self.at(SyntaxKind::Colon) || self.peek().is_some_and(starts_type) {
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

        match self.found() {
            Some(found) if matches!(found.kind, SyntaxKind::ColonEq | SyntaxKind::Eq) => {
                self.start_node(SyntaxKind::Initializer);
                let span = self.bump();
                if found.kind == SyntaxKind::Eq {
                    self.errors.push(errors::assignment_with_eq(span));
                }
                self.expression();
                self.builder.finish_node();
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
            }
            _ => {}
        }
        self.expect(SyntaxKind::Semicolon);
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
    /// comparisons (6), which don't chain, `+`, `-` and `||` (7), and `*`,
    /// `/`, `div` and `mod` (8), all grouping to the left. The prefix
    /// operators, `not`, `-` and `+`, bind tightest, so `not a = b` is
    /// `(not a) = b`.
    fn binary(&mut self, min: u8) {
        self.peek();
        let start = self.builder.checkpoint();
        self.unary();
        // The right operand of a comparison just parsed: comparisons don't
        // chain, so another one after it is reported, with a fix.
        let mut compared: Option<Span> = None;
        while let Some((precedence, found, keyword)) = self.binary_operator() {
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
            self.operator_token(found, keyword);
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
            self.builder.finish_node();
        }
    }

    /// The dyadic operator at the next token, if any: its precedence, the
    /// token, and the keyword it is or stands for.
    fn binary_operator(&self) -> Option<(u8, Found<'a>, SyntaxKind)> {
        let found = self.lookahead()?;
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
            SyntaxKind::Plus | SyntaxKind::Minus | SyntaxKind::PipePipe => 7,
            SyntaxKind::Star | SyntaxKind::Slash | SyntaxKind::DivKw | SyntaxKind::ModKw => 8,
            _ => return None,
        };
        Some((precedence, found, keyword))
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

    /// An operand, then postfix operators, which bind tighter than any
    /// other: projections, `r{a, b}`, and `remove`, `r remove {c}`. So
    /// `a and b{x}` projects `b`.
    fn postfix(&mut self) {
        self.peek();
        let start = self.builder.checkpoint();
        self.primary();
        loop {
            if self
                .lookahead()
                .is_some_and(|f| f.kind == SyntaxKind::LBrace)
            {
                self.builder.start_node_at(start, SyntaxKind::ProjectExpr);
                self.name_list();
            } else if let Some((found, keyword)) = self.postfix_operator() {
                self.builder.start_node_at(start, SyntaxKind::PostfixExpr);
                self.operator_token(found, keyword);
                self.name_list();
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
        (keyword == SyntaxKind::RemoveKw).then_some((found, keyword))
    }

    /// `{ a, b }`: attribute names.
    fn name_list(&mut self) {
        let outer_form = std::mem::replace(&mut self.form, errors::PROJECTION_FORM);
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
    fn items(&mut self, starts_item: fn(&mut Self) -> bool, item: fn(&mut Self)) {
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
        contents: fn(&mut Self),
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
                what, delims, used, open_span, close_span,
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
        let pos = (self.pos..self.tokens.len()).find(|&i| !is_skipped(self.tokens[i].kind))?;
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
    fn rational_initializer() {
        let parse = parse_str("var r: rational := 1.5e-3;");
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
            "var q := tuple { x: 1.0, y: tuple { z: \"\" } };",
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
            "var p: tuple { x rational };",
            "var p: tuple { x: rational };",
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
            codes("var p: TUPLE { x: rational };"),
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
            "var n: tuple { r: relation { x: rational } };",
            "var n := relation { { r: relation { { x: 1.0 } } } };",
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
            "operator greet{name: text, times: integer} [ call debug_value{v: name}; call debug_value{v: tuple { n: times }}; ];",
            "operator main{} [ call greet{name: \"Ada\", times: 2,}; ];",
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
        assert_eq!(codes, [Code::ForeignOperator], "`/` is rational division");
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
