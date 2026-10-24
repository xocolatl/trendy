mod errors;
mod sequences;
mod unordered;

use std::ops::RangeInclusive;

use trendy_diag::{Diagnostic, Span};
use trendy_lexer::{Lexed, Token};
use trendy_syntax::ast::{AstNode, SourceFile};
use trendy_syntax::{Precedence, SyntaxKind, TreeBuilder};

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
        value_end: None,
        missing: None,
        paren_calls: true,
        previous_item: None,
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
    /// Where the last expression parsed ended: a statement that ends there
    /// ends with a value, which an operator could continue.
    value_end: Option<usize>,
    /// Tokens found missing at `pos`, reported together as one error once
    /// the parser moves on, so one mistake gives one error.
    missing: Option<Missing>,
    /// Whether a name followed by `(` is a call, as other languages write
    /// one: everywhere but in Tutorial D's `UPDATE R WHERE p ( … )`, whose
    /// condition the parenthesized new values follow.
    paren_calls: bool,
    /// Where the top-level definition before the one being parsed ends, if
    /// there is one: where a definition written inside this one goes.
    previous_item: Option<usize>,
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

/// What a statement starting at the next token is, by its first tokens:
/// trendy's statements, and other languages' that are reported.
#[derive(Clone, Copy, PartialEq, Eq)]
enum StatementStart {
    If,
    While,
    Loop,
    For,
    /// A heading's declaration, which goes at the top level.
    Heading,
    /// `break;` or `LEAVE name;`, which trendy has none of.
    Exit,
    Call,
    Var,
    /// `let x = 1;`, as Rust and JavaScript declare a variable.
    ForeignVar,
    AssignBlock,
    Assignment,
    Return,
    /// `debug_value{v: r};`, a call without `call`, or, where a value
    /// goes, a read-only operator's call.
    BareCall,
}

/// A dyadic operator at the next token: its precedence, its first token,
/// and the keyword it is or stands for.
#[derive(Clone, Copy)]
struct BinaryOperator<'a> {
    precedence: Precedence,
    found: Found<'a>,
    keyword: SyntaxKind,
    words: Words,
}

/// How a dyadic operator is written.
#[derive(Clone, Copy)]
enum Words {
    /// As one token: the keyword, or another spelling of it.
    One,
    /// `not matching`, two keywords.
    NotMatching,
    /// SQL's spelling of a join, such as `natural join` or
    /// `left outer join`, this many tokens long.
    Sql(usize),
    /// Another language's name for it, such as the literature's `semijoin`
    /// for `matching` or SQL's `except` for `minus`, fixed to `spelled`.
    Renamed { spelled: &'static str },
}

/// Something the parser found where it expected something else.
#[derive(Clone, Copy)]
struct Found<'a> {
    kind: SyntaxKind,
    span: Span,
    text: &'a str,
}

/// The direction an order item's word, in any case, is written: `asc` for
/// `asc` and `ascending`, `desc` for `desc` and `descending`.
fn direction(word: &str) -> Option<&'static str> {
    ["asc", "desc"].into_iter().find(|spelled| {
        word.eq_ignore_ascii_case(spelled) || word.eq_ignore_ascii_case(&format!("{spelled}ending"))
    })
}

/// Whether a value can start with a token of this kind, other than a name:
/// a literal, a selector, a parenthesized expression, a prefix operator or
/// an image relation.
fn starts_value(kind: SyntaxKind) -> bool {
    kind.is_literal()
        || matches!(
            kind,
            SyntaxKind::TupleKw
                | SyntaxKind::RelationKw
                | SyntaxKind::LBrace
                | SyntaxKind::LParen
                | SyntaxKind::Minus
                | SyntaxKind::Plus
                | SyntaxKind::NotKw
                | SyntaxKind::BangBang
        )
}

impl Found<'_> {
    /// Whether it is `keyword`, or the same word in another case.
    fn spells(&self, keyword: SyntaxKind) -> bool {
        self.kind == keyword
            || (self.kind == SyntaxKind::Ident
                && keyword
                    .fixed_text()
                    .is_some_and(|text| self.text.eq_ignore_ascii_case(text)))
    }

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
            self.previous_item = self.last_end;
            match found.kind {
                SyntaxKind::OperatorKw => self.operator(),
                SyntaxKind::BuiltinKw => self.builtin(),
                SyntaxKind::VarKw => self.var_decl(),
                SyntaxKind::Ident if self.at_kind() => self.kind_decl(),
                SyntaxKind::Ident if self.at_heading_decl() => self.heading_decl(),
                _ => self.junk(found),
            }
        }
    }

    /// Whether a kind declaration starts here: `kind`, a contextual keyword,
    /// before a name and `{`.
    fn at_kind(&mut self) -> bool {
        self.found().is_some_and(|f| f.text == "kind")
            && self
                .found_after(1)
                .is_some_and(|f| f.kind == SyntaxKind::Ident)
            && self
                .found_after(2)
                .is_some_and(|f| f.kind == SyntaxKind::LBrace)
    }

    /// `kind name { requirement, … };`: a set of types, each of which
    /// declares it, and what each must have.
    fn kind_decl(&mut self) {
        self.form = errors::KIND_FORM;
        self.start_node(SyntaxKind::KindDecl);
        self.bump();
        self.name("add the kind's name after `kind`", &[SyntaxKind::LBrace]);
        self.delimited(
            Some(SyntaxKind::RequirementList),
            "requirements",
            (SyntaxKind::LBrace, SyntaxKind::RBrace),
            &[],
            |p| {
                p.items(Self::at_requirement, |p| {
                    p.start_node(SyntaxKind::Requirement);
                    p.bump();
                    p.builder.finish_node();
                });
            },
        );
        self.expect(SyntaxKind::Semicolon);
        self.builder.finish_node();
    }

    /// Whether a heading declaration starts here: `heading`, a contextual
    /// keyword, in any case, before a name, or another language's keyword
    /// for a type of records before a name, then `{`, or `=` or `:=` and
    /// `{`, as in TypeScript's `type Home = {…}` and `interface Home {…}`.
    fn at_heading_decl(&mut self) -> bool {
        let Some(first) = self.found().filter(|f| f.kind == SyntaxKind::Ident) else {
            return false;
        };
        if self
            .found_after(1)
            .is_none_or(|f| f.kind != SyntaxKind::Ident)
        {
            return false;
        }
        if first.text.eq_ignore_ascii_case("heading") {
            return true;
        }
        let kind = |p: &mut Self, n| p.found_after(n).map(|f| f.kind);
        errors::record_keyword(first.text).is_some()
            && match kind(self, 2) {
                Some(SyntaxKind::LBrace) => true,
                Some(SyntaxKind::Eq | SyntaxKind::ColonEq) => {
                    kind(self, 3) == Some(SyntaxKind::LBrace)
                }
                _ => false,
            }
    }

    /// `heading name := { name: type, … };`: a heading with a name, which
    /// stands for it after `relation` and `tuple`. Another language's
    /// declaration of a type of records, such as TypeScript's
    /// `type Home = {…};` or Rust's `struct Home {…}`, is parsed as one,
    /// and fixed to one in a single edit. `=` for `:=`, and `relation` or
    /// `tuple` before the attributes, are reported, with fixes.
    fn heading_decl(&mut self) {
        let outer_form = std::mem::replace(&mut self.form, errors::HEADING_DECL_FORM);
        self.start_node(SyntaxKind::HeadingDecl);
        let keyword = self.found().expect("at a heading declaration");
        self.bump();
        let mut foreign = (!keyword.text.eq_ignore_ascii_case("heading"))
            .then(|| errors::ForeignHeading::new(keyword));
        if foreign.is_none() && keyword.text != "heading" {
            self.errors
                .push(errors::miscased_keyword(keyword, "heading"));
        }
        self.name(
            "add the heading's name after `heading`, as in `heading home := { city: text };`",
            &[SyntaxKind::ColonEq, SyntaxKind::Eq, SyntaxKind::LBrace],
        );
        let at = self.last_end.unwrap_or(keyword.span.end);
        match (self.peek(), &mut foreign) {
            (Some(SyntaxKind::ColonEq), _) => {
                self.bump();
            }
            (Some(SyntaxKind::Eq), Some(foreign)) => foreign.eq = Some(self.bump()),
            (Some(SyntaxKind::Eq), None) => {
                let span = self.bump();
                self.errors
                    .push(errors::assignment_with_eq(span, "to declare a heading"));
            }
            (_, Some(foreign)) => foreign.colon_eq = Some(at),
            (_, None) => self.missing(SyntaxKind::ColonEq),
        }
        if let Some(found) = self
            .found()
            .filter(|f| matches!(f.kind, SyntaxKind::TupleKw | SyntaxKind::RelationKw))
        {
            // `heading home := relation { … }`: `relation` and `tuple` go
            // where it is used.
            let next = self.found_after(1).map_or(found.span.end, |f| f.span.start);
            self.bump();
            self.errors.push(errors::heading_as_type(
                found,
                Span::new(found.span.start, next),
            ));
        }
        self.heading();
        match foreign {
            Some(mut foreign) => {
                if self.at(SyntaxKind::Semicolon) {
                    self.bump();
                } else {
                    foreign.semicolon = Some(self.last_end.unwrap_or(at));
                }
                self.errors.push(errors::foreign_heading(&foreign));
            }
            None => self.expect(SyntaxKind::Semicolon),
        }
        self.builder.finish_node();
        self.form = outer_form;
    }

    /// Whether a requirement starts here: an operator, or a name.
    fn at_requirement(&mut self) -> bool {
        self.peek()
            .is_some_and(|k| k == SyntaxKind::Ident || k.is_operator())
    }

    fn operator(&mut self) {
        self.form = errors::OPERATOR_FORM;
        self.start_node(SyntaxKind::Operator);
        self.bump();
        self.name(
            "add a name after `operator`, such as `operator main{} [];`",
            &[SyntaxKind::LBrace, SyntaxKind::LParen],
        );
        let angled = self.angled_type_parameters();
        self.params();
        let valued = self.result_type();
        let declared = self.type_parameters();
        if let Some(angled) = angled {
            let at = self.last_end.unwrap_or(angled.span.end);
            self.errors
                .push(errors::angled_type_parameters(&angled, declared, at));
        }
        self.delimited(
            Some(SyntaxKind::Body),
            "body",
            (SyntaxKind::LBracket, SyntaxKind::RBracket),
            &[
                (SyntaxKind::LBrace, SyntaxKind::RBrace),
                (SyntaxKind::LParen, SyntaxKind::RParen),
            ],
            |p| {
                if valued {
                    p.valued_statements();
                } else {
                    p.statements();
                }
            },
        );
        self.expect(SyntaxKind::Semicolon);
        self.builder.finish_node();
    }

    /// `builtin type name;` or `builtin operator name{params};`, with a
    /// result type for a read-only one and type parameters for a generic
    /// one: a declaration of something the compiler implements. Only the
    /// prelude has these.
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
                &[SyntaxKind::Semicolon, SyntaxKind::Colon],
            );
            // The kinds it is of.
            if self.at(SyntaxKind::Colon) {
                self.bump();
                self.names(errors::BUILTIN_FORM);
            }
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
        self.result_type();
        self.type_parameters();
        self.expect(SyntaxKind::Semicolon);
        self.builder.finish_node();
    }

    /// A read-only operator's result type, `: type`, after its parameters,
    /// if it has one, which it returns. Other languages' spellings,
    /// `-> type` and Tutorial D's `RETURNS type`, are parsed as it and
    /// fixed.
    fn result_type(&mut self) -> bool {
        let Some(found) = self.found() else {
            return false;
        };
        let starts_type = self.found_after(1).is_some_and(|f| {
            matches!(
                f.kind,
                SyntaxKind::Ident
                    | SyntaxKind::TupleKw
                    | SyntaxKind::RelationKw
                    | SyntaxKind::LBrace
            )
        });
        let spelled_otherwise = match found.kind {
            SyntaxKind::Colon => false,
            SyntaxKind::ForeignOperator if found.text == "->" && starts_type => true,
            SyntaxKind::Ident if found.text.eq_ignore_ascii_case("returns") && starts_type => true,
            _ => return false,
        };
        let before = self.last_end.unwrap_or(found.span.start);
        self.start_node(SyntaxKind::TypeAnnotation);
        self.bump();
        if spelled_otherwise {
            self.errors.push(errors::result_spelling(
                found,
                Span::new(before, found.span.end),
            ));
        }
        self.type_expr();
        self.builder.finish_node();
        true
    }

    /// An operator's type parameters, `types { T: any type, … }`, after its
    /// parameters and result type, if it has them, which it returns.
    /// `types` is a contextual keyword, recognized there before `{`, in any
    /// case, which is reported.
    fn type_parameters(&mut self) -> bool {
        let Some(found) = self
            .found()
            .filter(|f| f.kind == SyntaxKind::Ident && f.text.eq_ignore_ascii_case("types"))
        else {
            return false;
        };
        if self
            .found_after(1)
            .is_none_or(|f| f.kind != SyntaxKind::LBrace)
        {
            return false;
        }
        self.start_node(SyntaxKind::TypeParameters);
        self.bump();
        if found.text != "types" {
            self.errors.push(errors::miscased_keyword(found, "types"));
        }
        let outer_form = std::mem::replace(&mut self.form, errors::TYPES_FORM);
        self.delimited(
            Some(SyntaxKind::Heading),
            "type parameters",
            (SyntaxKind::LBrace, SyntaxKind::RBrace),
            &[],
            |p| {
                p.items(Self::at_attribute, |p| {
                    p.attribute(SyntaxKind::AttributeType, Self::attribute_type);
                });
            },
        );
        self.form = outer_form;
        self.builder.finish_node();
        true
    }

    /// Type parameters in angle brackets after an operator's name, as
    /// Rust, TypeScript, Java and C++ write them, `pair<T>{…}`: kept in an
    /// error node, to be reported once the signature is parsed, when where
    /// `types { … }` goes is known.
    fn angled_type_parameters(&mut self) -> Option<errors::Angled> {
        let open = self.found().filter(|f| f.kind == SyntaxKind::Lt)?;
        self.start_node(SyntaxKind::Error);
        self.bump();
        // Up to the matching `>`, or what starts the parameters or ends
        // the definition, if it is missing.
        let mut inside: Vec<Found> = Vec::new();
        let mut depth = 1;
        let mut end = open.span.end;
        while let Some(found) = self.found() {
            let ends = matches!(
                found.kind,
                SyntaxKind::LBrace
                    | SyntaxKind::LBracket
                    | SyntaxKind::LParen
                    | SyntaxKind::Semicolon
                    | SyntaxKind::OperatorKw
                    | SyntaxKind::VarKw
            );
            if ends {
                break;
            }
            match found.kind {
                SyntaxKind::Lt => depth += 1,
                SyntaxKind::Gt => depth -= 1,
                _ => {}
            }
            end = self.bump().end;
            if depth == 0 {
                break;
            }
            inside.push(found);
        }
        self.builder.finish_node();
        // Names alone, separated by commas, which `types` can declare as
        // they are; anything more, such as a bound, has no fix.
        let names: Option<Vec<String>> = (depth == 0)
            .then(|| {
                inside
                    .chunks(2)
                    .map(|pair| match pair {
                        [name]
                        | [
                            name,
                            Found {
                                kind: SyntaxKind::Comma,
                                ..
                            },
                        ] if name.kind == SyntaxKind::Ident => Some(name.text.to_string()),
                        _ => None,
                    })
                    .collect::<Option<Vec<_>>>()
            })
            .flatten()
            .filter(|names| !names.is_empty());
        Some(errors::Angled {
            span: Span::new(open.span.start, end),
            names,
        })
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
                    p.attribute(SyntaxKind::AttributeType, Self::attribute_type);
                });
            },
        );
        self.form = outer_form;
    }

    /// Statements up to the end of a body.
    fn statements(&mut self) {
        self.statements_to(false);
    }

    /// Statements up to the end of a body, or, if `end_word`, up to an
    /// `end`, in any case, which ends Pascal's `begin … end`.
    fn statements_to(&mut self, end_word: bool) {
        while let Some(found) = self.found() {
            if end_word && found.kind == SyntaxKind::Ident && found.text.eq_ignore_ascii_case("end")
            {
                break;
            }
            if self.ends_statements(&found) {
                break;
            }
            self.statement();
        }
    }

    /// Whether `found`, the next token, ends a body's statements. A
    /// statement can't start with `;`, so one here ends the body: in `[;`,
    /// the `]` is missing.
    fn ends_statements(&self, found: &Found) -> bool {
        matches!(
            found.kind,
            SyntaxKind::RBracket
                | SyntaxKind::RBrace
                | SyntaxKind::RParen
                | SyntaxKind::Semicolon
                | SyntaxKind::OperatorKw
                | SyntaxKind::BuiltinKw
        ) || self.at_unclosed_declaration(found)
    }

    /// A read-only operator's body, or a value block's contents:
    /// statements, then the value, an expression without `;`. A `;` after
    /// the value is reported, with a fix deleting it, and so is a value
    /// before other statements. Whether there is a value.
    fn valued_statements(&mut self) -> bool {
        let mut valued = false;
        while let Some(found) = self.found() {
            if self.ends_statements(&found) {
                break;
            }
            if found.spells(SyntaxKind::ReturnKw) && self.returns_value_last() {
                // `return x;`, the value written as other languages do.
                self.start_node(SyntaxKind::Error);
                self.bump();
                self.builder.finish_node();
                let value = self.found().map_or(found.span.end, |f| f.span.start);
                self.expression();
                let semicolon = self.found().filter(|f| f.kind == SyntaxKind::Semicolon);
                if semicolon.is_some() {
                    self.start_node(SyntaxKind::Error);
                    self.bump();
                    self.builder.finish_node();
                }
                self.errors.push(errors::returned_value(
                    Span::new(found.span.start, value),
                    semicolon.map(|s| s.span),
                ));
                valued = true;
                break;
            }
            if !self.at_value() {
                self.statement();
                continue;
            }
            self.expression();
            valued = true;
            let value = Span::new(found.span.start, self.last_end.unwrap_or(found.span.end));
            let Some(semicolon) = self.found().filter(|f| f.kind == SyntaxKind::Semicolon) else {
                break;
            };
            self.bump();
            let last = self.found().is_none_or(|next| self.ends_statements(&next));
            if last {
                self.errors.push(errors::value_semicolon(semicolon.span));
                break;
            }
            self.errors.push(errors::value_before_statements(value));
            valued = false;
        }
        valued
    }

    /// Whether a value block starts here: a `[` before a statement, which
    /// a `var` starts, or a `;` ends before the `]`. Anything else in
    /// brackets is an array without its keyword, as other languages write
    /// one. Braces before a `var`, as Rust and C write a block, can't be a
    /// tuple, so they are one too, reported.
    fn at_value_block(&mut self) -> bool {
        let declares = self.found_after(1).is_some_and(|f| {
            f.kind == SyntaxKind::VarKw
                || (f.kind == SyntaxKind::Ident && errors::foreign_keyword(f.text))
        }) && self
            .found_after(2)
            .is_some_and(|f| f.kind == SyntaxKind::Ident);
        if self.at(SyntaxKind::LBrace) {
            return declares;
        }
        if !self.at(SyntaxKind::LBracket) {
            return false;
        }
        if declares {
            return true;
        }
        let mut depth = 0_usize;
        for token in &self.tokens[self.pos + 1..] {
            match token.kind {
                SyntaxKind::LBrace | SyntaxKind::LBracket | SyntaxKind::LParen => depth += 1,
                SyntaxKind::RBrace | SyntaxKind::RBracket | SyntaxKind::RParen if depth == 0 => {
                    return false;
                }
                SyntaxKind::RBrace | SyntaxKind::RBracket | SyntaxKind::RParen => depth -= 1,
                SyntaxKind::Semicolon if depth == 0 => return true,
                _ => {}
            }
        }
        false
    }

    /// `[ var x := …; x * 2 ]`: statements, then the value they compute,
    /// as in a read-only operator's body. One without a value is reported,
    /// and so are braces, fixed to brackets.
    fn value_block(&mut self) {
        let outer_form = std::mem::replace(&mut self.form, errors::VALUE_BLOCK_FORM);
        self.delimited(
            Some(SyntaxKind::ValueBlock),
            "value block",
            (SyntaxKind::LBracket, SyntaxKind::RBracket),
            &[(SyntaxKind::LBrace, SyntaxKind::RBrace)],
            |p| {
                if !p.valued_statements() {
                    let at = p.last_end.unwrap_or(0);
                    p.errors.push(errors::block_without_value(Span::empty(at)));
                }
            },
        );
        self.form = outer_form;
    }

    /// Whether a read-only body's value starts here, rather than a
    /// statement: an operand that doesn't start one.
    fn at_value(&mut self) -> bool {
        let Some(found) = self.found() else {
            return false;
        };
        // An `if` that runs to the end is the value, an `if` expression,
        // and one that a `;` ends a statement.
        if self.at_if() {
            return self.runs_to_end();
        }
        // `a = b` is a comparison that ends the body, and otherwise
        // assignment's `=` in place of `:=`, which is reported.
        let compared = found.kind == SyntaxKind::Ident
            && self
                .found_after(1)
                .is_some_and(|f| f.kind == SyntaxKind::Eq)
            && self.runs_to_end();
        // A name before `{` is a read-only operator's call, where a value
        // goes.
        self.at_operand()
            && (compared
                || matches!(
                    self.statement_start(),
                    None | Some(StatementStart::BareCall)
                ))
    }

    /// Whether the `return` here has a value and ends the body it is in:
    /// whether the first `;` outside any delimiters, if any, is the last
    /// thing before the `]`.
    fn returns_value_last(&self) -> bool {
        let significant = |i: usize| {
            (i..self.tokens.len())
                .find(|&j| !is_skipped(self.tokens[j].kind))
                .map(|j| self.tokens[j].kind)
        };
        let closes = |kind: Option<SyntaxKind>| {
            matches!(
                kind,
                None | Some(SyntaxKind::RBracket | SyntaxKind::RBrace | SyntaxKind::RParen)
            )
        };
        let after = significant(self.pos + 1);
        if after == Some(SyntaxKind::Semicolon) || closes(after) {
            return false;
        }
        let mut depth = 0_usize;
        for i in self.pos + 1..self.tokens.len() {
            match self.tokens[i].kind {
                SyntaxKind::LBrace | SyntaxKind::LBracket | SyntaxKind::LParen => depth += 1,
                SyntaxKind::RBrace | SyntaxKind::RBracket | SyntaxKind::RParen if depth == 0 => {
                    return true;
                }
                SyntaxKind::RBrace | SyntaxKind::RBracket | SyntaxKind::RParen => depth -= 1,
                SyntaxKind::Semicolon if depth == 0 => return closes(significant(i + 1)),
                _ => {}
            }
        }
        true
    }

    /// Whether what starts here runs to the end of the body it is in, with
    /// no `;` before the `]`, outside any delimiters it opens.
    fn runs_to_end(&self) -> bool {
        let mut depth = 0_usize;
        for token in &self.tokens[self.pos..] {
            match token.kind {
                SyntaxKind::LBrace | SyntaxKind::LBracket | SyntaxKind::LParen => depth += 1,
                SyntaxKind::RBrace | SyntaxKind::RBracket | SyntaxKind::RParen if depth == 0 => {
                    return true;
                }
                SyntaxKind::RBrace | SyntaxKind::RBracket | SyntaxKind::RParen => depth -= 1,
                SyntaxKind::Semicolon if depth == 0 => return false,
                _ => {}
            }
        }
        false
    }

    /// Whether `found`, the next token, is a `var` at the start of an
    /// unindented line, in a body that no `]` closes before the next
    /// definition: the declaration of a program's variable, before which
    /// the body's `];` is missing. A local variable's declaration is in a
    /// body that a `]` closes, in a valid program.
    fn at_unclosed_declaration(&self, found: &Found) -> bool {
        let unindented = self.source[..found.span.start]
            .rsplit('\n')
            .next()
            .is_some_and(str::is_empty);
        if found.kind != SyntaxKind::VarKw || !unindented {
            return false;
        }
        let mut depth = 0usize;
        for token in &self.tokens[self.pos..] {
            match token.kind {
                SyntaxKind::LBracket | SyntaxKind::LBrace | SyntaxKind::LParen => depth += 1,
                SyntaxKind::RBracket if depth == 0 => return false,
                SyntaxKind::RBracket | SyntaxKind::RBrace | SyntaxKind::RParen => {
                    depth = depth.saturating_sub(1);
                }
                SyntaxKind::OperatorKw | SyntaxKind::BuiltinKw if depth == 0 => return true,
                _ => {}
            }
        }
        true
    }

    /// What statement starts at the next token, if one does.
    fn statement_start(&mut self) -> Option<StatementStart> {
        let found = self.found()?;
        let call_like = self
            .found_after(1)
            .is_some_and(|f| matches!(f.kind, SyntaxKind::LBrace | SyntaxKind::LParen));
        // `break;` and `LEAVE name;`: names, before what no statement
        // starting with a name has after it.
        let exit = found.kind == SyntaxKind::Ident
            && ["break", "continue", "leave"]
                .iter()
                .any(|word| found.text.eq_ignore_ascii_case(word))
            && self
                .found_after(1)
                .is_some_and(|f| matches!(f.kind, SyntaxKind::Semicolon | SyntaxKind::Ident));
        Some(if self.at_if() {
            StatementStart::If
        } else if found.spells(SyntaxKind::WhileKw) {
            StatementStart::While
        } else if self.at_loop() {
            StatementStart::Loop
        } else if found.spells(SyntaxKind::ForKw)
            || (found.kind == SyntaxKind::Ident && found.text.eq_ignore_ascii_case("foreach"))
        {
            StatementStart::For
        } else if found.text == "heading" && self.at_heading_decl() {
            StatementStart::Heading
        } else if exit {
            StatementStart::Exit
        } else if self.at_call_keyword() {
            StatementStart::Call
        } else if found.kind == SyntaxKind::VarKw {
            StatementStart::Var
        } else if self.at_foreign_declaration() {
            StatementStart::ForeignVar
        } else if found.kind == SyntaxKind::AssignKw {
            StatementStart::AssignBlock
        } else if self.at_assignment() {
            StatementStart::Assignment
        } else if found.spells(SyntaxKind::ReturnKw) {
            StatementStart::Return
        } else if found.kind == SyntaxKind::Ident && call_like {
            StatementStart::BareCall
        } else {
            return None;
        })
    }

    /// A statement: a call, an assignment statement, a local variable's
    /// declaration, an `if`, a loop, or a `return`, for leaving early.
    /// Anything else is reported, and an expression, as C and JavaScript
    /// allow one, is parsed first, so that an error in it is reported
    /// instead.
    fn statement(&mut self) {
        let Some(found) = self.found() else {
            return;
        };
        match self.statement_start() {
            Some(StatementStart::If) => self.if_stmt(),
            Some(StatementStart::While) => self.while_stmt(),
            Some(StatementStart::Loop) => self.loop_stmt(),
            Some(StatementStart::For) => self.for_stmt(),
            Some(StatementStart::Heading) => {
                // A heading declared in a body, as a local type: parsed as
                // one, and moved before the definition it is in.
                let before = self.last_end.unwrap_or(found.span.start);
                self.heading_decl();
                let end = self.last_end.unwrap_or(found.span.end);
                self.errors.push(errors::nested_heading(
                    found.span,
                    &self.source[found.span.start..end],
                    Span::new(before, end),
                    self.previous_item,
                ));
            }
            Some(StatementStart::Exit) => {
                self.errors.push(errors::loop_exit(found));
                self.skip_statement(&STATEMENT_STOPS);
            }
            Some(StatementStart::Call) => self.call_stmt(),
            Some(StatementStart::Var) => self.var_decl(),
            Some(StatementStart::ForeignVar) => {
                self.errors.push(errors::foreign_declaration(found));
                self.var_decl();
            }
            Some(StatementStart::AssignBlock) => self.assign_block(),
            Some(StatementStart::Assignment) => self.assign_stmt(false),
            Some(StatementStart::Return) => self.return_stmt(),
            Some(StatementStart::BareCall) => {
                self.errors.push(errors::missing_call(found));
                self.call_stmt();
            }
            None if self.at_expression() => {
                self.flush_missing();
                self.start_node(SyntaxKind::Error);
                let before = self.errors.len();
                self.expression();
                self.flush_missing();
                // One that isn't an expression after all, such as `x += 1`,
                // has its own error.
                if self.errors.len() == before {
                    let end = self.last_end.unwrap_or(found.span.end);
                    self.errors.push(errors::expression_statement(Span::new(
                        found.span.start,
                        end,
                    )));
                }
                if self.at(SyntaxKind::Semicolon) {
                    self.bump();
                } else {
                    self.skip_statement(&STATEMENT_STOPS);
                }
                self.builder.finish_node();
            }
            None => {
                self.flush_missing();
                let at = self.last_end.unwrap_or(found.span.start);
                self.errors
                    .push(errors::expected_statement(found, Span::empty(at)));
                self.skip_statement(&STATEMENT_STOPS);
            }
        }
    }

    /// The `;` that ends a statement. Before an expression that can't
    /// start another statement, as the `2` in `a := 1 2;`, a `;` alone
    /// doesn't fix it, and after a value an operator may be what's
    /// missing, so nothing is inserted: the rest of the statement is
    /// skipped, with one error.
    fn end_statement(&mut self) {
        if self.at(SyntaxKind::Semicolon) {
            self.bump();
            return;
        }
        match self.found() {
            Some(found) if self.at_expression() && self.statement_start().is_none() => {
                self.flush_missing();
                let at = self.last_end.unwrap_or(found.span.start);
                let after_value = self.value_end.is_some() && self.value_end == self.last_end;
                self.errors.push(errors::operand_after_statement(
                    found,
                    Span::empty(at),
                    self.form,
                    after_value,
                ));
                self.skip_statement(&STATEMENT_STOPS);
            }
            _ => self.missing(SyntaxKind::Semicolon),
        }
    }

    /// Whether a declaration starts here with another language's keyword,
    /// as in `let x = 1;` or `int x = 1;`: the word, a name, and what follows
    /// a variable's name.
    fn at_foreign_declaration(&mut self) -> bool {
        self.found()
            .is_some_and(|f| f.kind == SyntaxKind::Ident && errors::foreign_keyword(f.text))
            && self
                .found_after(1)
                .is_some_and(|f| f.kind == SyntaxKind::Ident)
            && self.found_after(2).is_some_and(|f| {
                matches!(
                    f.kind,
                    SyntaxKind::Eq
                        | SyntaxKind::ColonEq
                        | SyntaxKind::Colon
                        | SyntaxKind::Semicolon
                )
            })
    }

    /// Whether an assignment, or a shorthand for one, starts here: a
    /// target, then `:=`, or `=`, which is reported. A keyword before it is
    /// one too, reported as a name that can't be one.
    fn at_assignment(&mut self) -> bool {
        let Some(found) = self.found() else {
            return false;
        };
        let after = self.after_target_path(1);
        let assigned = (found.kind == SyntaxKind::Ident || found.kind.is_keyword())
            && self
                .found_after(after)
                .is_some_and(|f| matches!(f.kind, SyntaxKind::ColonEq | SyntaxKind::Eq));
        assigned || self.at_clause_keyword().is_some() || self.at_load()
    }

    /// The offset, as [`Parser::found_after`] counts, of the first token
    /// from offset `n` on that isn't part of a target's path after its
    /// variable: positions, `[…]`, and attributes, `.name`.
    fn after_target_path(&mut self, mut n: usize) -> usize {
        loop {
            match self.found_after(n).map(|f| f.kind) {
                Some(SyntaxKind::LBracket) => {
                    let mut depth = 0usize;
                    loop {
                        match self.found_after(n).map(|f| f.kind) {
                            Some(SyntaxKind::LBracket) => depth += 1,
                            Some(SyntaxKind::RBracket) => depth -= 1,
                            Some(SyntaxKind::Semicolon) | None => return n,
                            _ => {}
                        }
                        n += 1;
                        if depth == 0 {
                            break;
                        }
                    }
                }
                Some(SyntaxKind::Dot)
                    if self
                        .found_after(n + 1)
                        .is_some_and(|f| f.kind == SyntaxKind::Ident) =>
                {
                    n += 2;
                }
                _ => return n,
            }
        }
    }

    /// Whether a `load` starts here: `load`, a contextual keyword, in any
    /// case, before the array's name.
    fn at_load(&mut self) -> bool {
        self.at_word("load")
            && self
                .found_after(1)
                .is_some_and(|f| f.kind == SyntaxKind::Ident)
    }

    /// Wrap the tokens of a statement that isn't one in an error node, up
    /// to its `;`, or up to one of `stops` outside any delimiters it opens.
    fn skip_statement(&mut self, stops: &[SyntaxKind]) {
        self.start_node(SyntaxKind::Error);
        let mut depth = 0usize;
        while let Some(kind) = self.peek() {
            if depth == 0 && stops.contains(&kind) {
                break;
            }
            self.bump();
            match kind {
                SyntaxKind::LBrace | SyntaxKind::LBracket | SyntaxKind::LParen => depth += 1,
                SyntaxKind::RBrace | SyntaxKind::RBracket | SyntaxKind::RParen => {
                    depth = depth.saturating_sub(1);
                }
                SyntaxKind::Semicolon if depth == 0 => break,
                _ => {}
            }
        }
        self.builder.finish_node();
    }

    /// Whether a `loop … while` starts here: `loop`, or C's `do` or Pascal's
    /// and Lua's `repeat`, in any case, before a block.
    fn at_loop(&mut self) -> bool {
        let Some(found) = self.found() else {
            return false;
        };
        let foreign = ["do", "repeat"]
            .iter()
            .any(|word| found.text.eq_ignore_ascii_case(word));
        let block = self.found_after(1).is_some_and(|f| {
            matches!(f.kind, SyntaxKind::LBracket | SyntaxKind::LBrace)
                || (f.kind == SyntaxKind::Ident && f.text.eq_ignore_ascii_case("begin"))
        });
        found.kind == SyntaxKind::LoopKw
            || (found.kind == SyntaxKind::Ident
                && block
                && (foreign || found.spells(SyntaxKind::LoopKw)))
    }

    /// `while condition loop [ … ];`: the block run while the condition is
    /// true, checked before each run. Pascal's and Lua's `do`, a missing
    /// `loop`, Tutorial D's `WHILE c; … END WHILE`, Python's `:`, and the
    /// block's habits, as an `if` statement's (see [`Parser::branch`]), are
    /// reported, with fixes where they are mechanical.
    fn while_stmt(&mut self) {
        let outer_form = std::mem::replace(&mut self.form, errors::LOOP_FORM);
        self.start_node(SyntaxKind::WhileStmt);
        self.any_case(SyntaxKind::WhileKw);
        self.expression();
        let mut unbracketed = Vec::new();
        let last_unbracketed = match self.found() {
            Some(found) if found.kind == SyntaxKind::Semicolon && self.at_tutorial_while() => {
                self.tutorial_while(found);
                false
            }
            _ => self.loop_block(
                &mut unbracketed,
                "`while c:` is written `while c loop [ … ];`",
            ),
        };
        self.end_loop(&unbracketed, last_unbracketed);
        self.builder.finish_node();
        self.form = outer_form;
    }

    /// A `while` or `for` loop's `loop` and block, after its condition or
    /// array. Pascal's and Lua's `do`, a missing `loop`, Python's `:`, its
    /// spelling `python`, and the block's habits, as an `if` statement's
    /// (see [`Parser::branch`]), are reported. Whether the block is a
    /// statement without brackets, each one's span added to `unbracketed`.
    fn loop_block(&mut self, unbracketed: &mut Vec<Span>, python: &'static str) -> bool {
        match self.found() {
            Some(found) if found.spells(SyntaxKind::LoopKw) => {
                self.any_case(SyntaxKind::LoopKw);
                self.branch(unbracketed)
            }
            Some(found)
                if found.kind == SyntaxKind::Ident && found.text.eq_ignore_ascii_case("do") =>
            {
                self.bump();
                self.errors.push(errors::loop_spelling(found));
                self.branch(unbracketed)
            }
            // Python's block is what its lines' indentation says, so only
            // the first statement is taken for it, reported once.
            Some(found) if found.kind == SyntaxKind::Colon => {
                self.bump();
                self.errors.push(errors::python_loop(found, python));
                self.branch(&mut Vec::new())
            }
            Some(found) if matches!(found.kind, SyntaxKind::LBracket | SyntaxKind::LBrace) => {
                let before = self.last_end.unwrap_or(found.span.start);
                self.errors.push(errors::missing_loop(before, found.span));
                self.branch(unbracketed)
            }
            _ => {
                self.missing_with_help(
                    SyntaxKind::LoopKw,
                    "write `loop` and the block, as in `while c loop [ … ];` or `for x in a loop [ … ];`",
                );
                false
            }
        }
    }

    /// The end of a `while` or `for` loop: each block in `unbracketed`
    /// reported, and the `;`, which a last block without brackets, if
    /// `last_unbracketed`, ended with its statement's.
    fn end_loop(&mut self, unbracketed: &[Span], last_unbracketed: bool) {
        let shared = last_unbracketed && !self.at(SyntaxKind::Semicolon);
        for &span in unbracketed {
            self.errors
                .push(errors::unbracketed_branch(span, shared, "a loop's block"));
        }
        if !shared {
            self.expect(SyntaxKind::Semicolon);
        }
    }

    /// `for x in a loop [ … ];`: the block run for each element of the
    /// array `a`, in order, with `x` its value. `in` is a contextual
    /// keyword. C#'s and PHP's `foreach`, JavaScript's `of`, C's
    /// `for (…; …; …)`, and the habits of a `while` loop's block (see
    /// [`Parser::loop_block`]), are reported, with fixes where they are
    /// mechanical.
    fn for_stmt(&mut self) {
        let outer_form = std::mem::replace(&mut self.form, errors::FOR_FORM);
        self.start_node(SyntaxKind::ForStmt);
        let found = self.found().expect("at `for`");
        if found.spells(SyntaxKind::ForKw) {
            self.any_case(SyntaxKind::ForKw);
        } else {
            self.bump();
            self.errors.push(errors::foreach(found));
        }
        if self.at_c_for() {
            let header = self.found().map_or(found.span, |f| f.span);
            self.skip_statement(&STATEMENT_STOPS);
            self.errors
                .push(errors::c_for(Span::new(found.span.start, header.end)));
            self.builder.finish_node();
            self.form = outer_form;
            return;
        }
        if self.at(SyntaxKind::Ident) {
            self.start_node(SyntaxKind::Name);
            self.bump();
            self.builder.finish_node();
        } else {
            self.missing_with_help(
                SyntaxKind::Ident,
                "name the variable that takes each element's value, as in `for name in names loop [ … ];`",
            );
        }
        if self.at_word("in") {
            self.contextual_keyword("in");
        } else if let Some(of) = self.found_word("of") {
            self.bump();
            self.errors.push(errors::for_of(of));
        } else {
            self.missing_with_help(
                SyntaxKind::Ident,
                "write `in` and the array after the variable, as in `for name in names loop [ … ];`",
            );
        }
        if self.at_operand() && !self.at(SyntaxKind::LoopKw) {
            self.expression();
        } else {
            self.missing_with_help(
                SyntaxKind::Ident,
                "write the array to go through after `in`, as in `for name in names loop [ … ];`",
            );
        }
        let mut unbracketed = Vec::new();
        let last_unbracketed = self.loop_block(
            &mut unbracketed,
            "`for x in a:` is written `for x in a loop [ … ];`",
        );
        self.end_loop(&unbracketed, last_unbracketed);
        self.builder.finish_node();
        self.form = outer_form;
    }

    /// Whether C's `for (…; …; …)` is next: parentheses with a `;` right
    /// inside them.
    fn at_c_for(&self) -> bool {
        let mut depth = 0_usize;
        for token in &self.tokens[self.pos..] {
            match token.kind {
                kind if is_skipped(kind) => {}
                SyntaxKind::LParen if depth == 0 => depth = 1,
                _ if depth == 0 => return false,
                SyntaxKind::LBrace | SyntaxKind::LBracket | SyntaxKind::LParen => depth += 1,
                SyntaxKind::RBrace | SyntaxKind::RBracket | SyntaxKind::RParen => {
                    depth -= 1;
                    if depth == 0 {
                        return false;
                    }
                }
                SyntaxKind::Semicolon if depth == 1 => return true,
                _ => {}
            }
        }
        false
    }

    /// Whether Tutorial D's `WHILE c; … END WHILE` goes on here, at the `;`
    /// after the condition: whether `END WHILE` comes before the body ends.
    fn at_tutorial_while(&self) -> bool {
        let mut depth = 0_usize;
        for i in self.pos + 1..self.tokens.len() {
            match self.tokens[i].kind {
                SyntaxKind::LBrace | SyntaxKind::LBracket | SyntaxKind::LParen => depth += 1,
                SyntaxKind::RBrace | SyntaxKind::RBracket | SyntaxKind::RParen if depth == 0 => {
                    return false;
                }
                SyntaxKind::RBrace | SyntaxKind::RBracket | SyntaxKind::RParen => depth -= 1,
                SyntaxKind::Ident
                    if depth == 0 && self.found_at(i).text.eq_ignore_ascii_case("end") =>
                {
                    return (i + 1..self.tokens.len())
                        .find(|&j| !is_skipped(self.tokens[j].kind))
                        .is_some_and(|j| self.found_at(j).spells(SyntaxKind::WhileKw));
                }
                _ => {}
            }
        }
        false
    }

    /// Tutorial D's `WHILE c; … END WHILE`, at the `;`, `semicolon`, after
    /// the condition: the statements up to `END WHILE` as the block, and
    /// the fix writing them in brackets after `loop`.
    fn tutorial_while(&mut self, semicolon: Found) {
        self.start_node(SyntaxKind::Error);
        self.bump();
        self.builder.finish_node();
        self.start_node(SyntaxKind::Body);
        self.statements_to(true);
        self.builder.finish_node();
        self.start_node(SyntaxKind::Error);
        let start = self.bump().start;
        let end = self.bump().end;
        self.builder.finish_node();
        self.errors.push(errors::tutorial_while(
            semicolon.span,
            Span::new(start, end),
        ));
    }

    /// `loop [ … ] while condition;`: the block run, then again while the
    /// condition is true, checked after each run. C's `do`, Pascal's and
    /// Lua's `repeat … until c`, and the block's habits, as an `if`
    /// statement's (see [`Parser::branch`]), are reported, with fixes.
    fn loop_stmt(&mut self) {
        let outer_form = std::mem::replace(&mut self.form, errors::LOOP_FORM);
        self.start_node(SyntaxKind::LoopStmt);
        let found = self.found().expect("at a loop");
        if found.spells(SyntaxKind::LoopKw) {
            self.any_case(SyntaxKind::LoopKw);
        } else {
            self.bump();
            self.errors.push(errors::loop_spelling(found));
        }
        let mut unbracketed = Vec::new();
        self.branch(&mut unbracketed);
        for &span in &unbracketed {
            self.errors
                .push(errors::unbracketed_branch(span, false, "a loop's block"));
        }
        match self.found() {
            Some(found) if found.spells(SyntaxKind::WhileKw) => {
                self.any_case(SyntaxKind::WhileKw);
                self.expression();
            }
            Some(found)
                if found.kind == SyntaxKind::Ident && found.text.eq_ignore_ascii_case("until") =>
            {
                self.bump();
                let start = self.found().map_or(found.span.end, |f| f.span.start);
                self.expression();
                let end = self.last_end.unwrap_or(start);
                self.errors
                    .push(errors::loop_until(found, Span::new(start, end)));
            }
            _ => self.missing_with_help(
                SyntaxKind::WhileKw,
                "end the loop with `while` and its condition, as in `loop [ … ] while c;`",
            ),
        }
        self.expect(SyntaxKind::Semicolon);
        self.builder.finish_node();
        self.form = outer_form;
    }

    /// `if condition then [ … ] else [ … ];`: statements run if the
    /// condition is true, and else others, if any, with `else if` chaining
    /// another condition. Each branch is a block in brackets, even of one
    /// statement: a statement without them, Pascal's `begin … end`, and C's
    /// `{ … }`, are reported, with fixes, as are the spellings of other
    /// languages an `if` expression's are (see [`Parser::if_expression`]).
    fn if_stmt(&mut self) {
        let outer_form = std::mem::replace(&mut self.form, errors::IF_STMT_FORM);
        self.start_node(SyntaxKind::IfStmt);
        self.any_case(SyntaxKind::IfKw);
        let mut unbracketed = Vec::new();
        let last_unbracketed = self.if_stmt_branches(&mut unbracketed);
        self.end_if();
        // A last branch without brackets ends with its statement's `;`,
        // which ended the `if` as well.
        let shared = last_unbracketed && !self.at(SyntaxKind::Semicolon);
        for (i, &span) in unbracketed.iter().enumerate() {
            let ends_if = shared && i + 1 == unbracketed.len();
            self.errors.push(errors::unbracketed_branch(
                span,
                ends_if,
                "an `if` statement's branch",
            ));
        }
        if !shared {
            self.expect(SyntaxKind::Semicolon);
        }
        self.builder.finish_node();
        self.form = outer_form;
    }

    /// What follows an `if` statement's keyword: the condition and its
    /// branches. Whether the last branch was a statement without brackets,
    /// each one's span added to `unbracketed`.
    fn if_stmt_branches(&mut self, unbracketed: &mut Vec<Span>) -> bool {
        self.expression();
        self.any_case(SyntaxKind::ThenKw);
        let mut last = self.branch(unbracketed);
        if let Some(found) = self.at_else_if_spelling() {
            self.start_node(SyntaxKind::IfStmt);
            self.bump();
            self.errors.push(errors::else_if_spelling(found));
            last = self.if_stmt_branches(unbracketed);
            self.builder.finish_node();
        } else if self.found().is_some_and(|f| f.spells(SyntaxKind::ElseKw)) {
            self.any_case(SyntaxKind::ElseKw);
            if self.at_if() {
                self.start_node(SyntaxKind::IfStmt);
                self.any_case(SyntaxKind::IfKw);
                last = self.if_stmt_branches(unbracketed);
                self.builder.finish_node();
            } else {
                last = self.branch(unbracketed);
            }
        }
        last
    }

    /// Python's `elif`, Perl's `elsif` or another's `elseif`, in any case,
    /// for `else if`, if it is next.
    fn at_else_if_spelling(&mut self) -> Option<Found<'a>> {
        self.found().filter(|found| {
            found.kind == SyntaxKind::Ident
                && ["elif", "elsif", "elseif"]
                    .iter()
                    .any(|w| found.text.eq_ignore_ascii_case(w))
        })
    }

    /// A branch of an `if` statement: a block in brackets, with C's `{ … }`
    /// fixed as an operator's body's is. Pascal's `begin … end` is one too,
    /// reported; so is a statement without brackets, whose span is added
    /// to `unbracketed` for the `if` to report. Whether it was one.
    fn branch(&mut self, unbracketed: &mut Vec<Span>) -> bool {
        let Some(found) = self.found() else {
            self.missing(SyntaxKind::LBracket);
            return false;
        };
        match found.kind {
            SyntaxKind::LBracket | SyntaxKind::LBrace => {
                self.delimited(
                    Some(SyntaxKind::Body),
                    "block",
                    (SyntaxKind::LBracket, SyntaxKind::RBracket),
                    &[(SyntaxKind::LBrace, SyntaxKind::RBrace)],
                    Self::statements,
                );
                false
            }
            SyntaxKind::Ident if found.text.eq_ignore_ascii_case("begin") => {
                self.begin_end(found);
                false
            }
            // No statement starts with these: the block is missing.
            SyntaxKind::ElseKw | SyntaxKind::Semicolon | SyntaxKind::RBracket => {
                self.missing(SyntaxKind::LBracket);
                self.missing(SyntaxKind::RBracket);
                false
            }
            _ => {
                self.start_node(SyntaxKind::Body);
                self.statement();
                self.builder.finish_node();
                let end = self.last_end.unwrap_or(found.span.end);
                unbracketed.push(Span::new(found.span.start, end));
                true
            }
        }
    }

    /// Pascal's `begin … end`, or Tutorial D's `BEGIN; … END`, starting at
    /// `begin`, parsed as a block in brackets, and reported, with the fix
    /// writing it as one. The `;` that ends Tutorial D's block, as a
    /// statement, before an `else` or `END IF`, goes with it.
    fn begin_end(&mut self, begin: Found) {
        self.start_node(SyntaxKind::Body);
        let mut opening = self.bump();
        if self.at(SyntaxKind::Semicolon) {
            opening.end = self.bump().end;
        }
        self.statements_to(true);
        let end_if = self
            .lookahead_nth(1)
            .is_some_and(|f| f.spells(SyntaxKind::IfKw));
        let closing = match self.found() {
            Some(found)
                if found.kind == SyntaxKind::Ident
                    && found.text.eq_ignore_ascii_case("end")
                    && !end_if =>
            {
                let mut closing = self.bump();
                let after = self.lookahead_nth(1);
                let continues = after.is_some_and(|f| {
                    f.spells(SyntaxKind::ElseKw)
                        || (f.kind == SyntaxKind::Ident && f.text.eq_ignore_ascii_case("end"))
                });
                if self.at(SyntaxKind::Semicolon) && continues {
                    closing.end = self.bump().end;
                }
                Some(closing)
            }
            _ => {
                self.missing(SyntaxKind::RBracket);
                None
            }
        };
        self.builder.finish_node();
        self.errors.push(errors::begin_end(begin, opening, closing));
    }

    /// `return;` or `return value;`, with `return` in any case, as Tutorial
    /// D's `RETURN`, which is fixed: an early exit, which sema rejects until
    /// there are early exits.
    fn return_stmt(&mut self) {
        self.start_node(SyntaxKind::ReturnStmt);
        let found = self.found().expect("at `return`");
        self.bump();
        if found.kind != SyntaxKind::ReturnKw {
            self.errors.push(errors::miscased_keyword(found, "return"));
        }
        // An update operator's has none, which sema reports either way.
        if self.at_operand() {
            self.expression();
        }
        self.end_statement();
        self.builder.finish_node();
    }

    /// Whether `call` is next, or Tutorial D's `CALL` in another case, which
    /// the operator's name follows.
    fn at_call_keyword(&mut self) -> bool {
        self.found().is_some_and(|f| {
            f.kind == SyntaxKind::CallKw
                || (f.spells(SyntaxKind::CallKw)
                    && self
                        .found_after(1)
                        .is_some_and(|n| n.kind == SyntaxKind::Ident))
        })
    }

    /// `call name{arguments};`, with `call` already reported if missing.
    fn call_stmt(&mut self) {
        self.start_node(SyntaxKind::CallStmt);
        if self.at_call_keyword() {
            self.any_case(SyntaxKind::CallKw);
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
        self.end_statement();
        self.builder.finish_node();
    }

    /// `x := y;`, or a shorthand for an assignment. Several separated by
    /// commas, as TTM writes a multiple assignment, are reported, with a
    /// fix writing them in an `assign` block, or, `in_block`, ending each
    /// with `;`.
    fn assign_stmt(&mut self, in_block: bool) {
        let outer_form = std::mem::replace(&mut self.form, errors::ASSIGNMENT_FORM);
        self.start_node(SyntaxKind::AssignStmt);
        let start = self.found().map_or(0, |f| f.span.start);
        let mut clauses = Vec::new();
        let mut commas = Vec::new();
        loop {
            let from = self.found().map_or(start, |f| f.span.start);
            self.clause();
            clauses.push(Span::new(from, self.last_end.unwrap_or(from)));
            if !self.at(SyntaxKind::Comma) {
                break;
            }
            commas.push(self.bump());
        }
        self.end_statement();
        self.builder.finish_node();
        self.form = outer_form;
        if !commas.is_empty() {
            let end = self.last_end.unwrap_or(start);
            let clauses: Vec<&str> = clauses
                .iter()
                .map(|c| &self.source[c.start..c.end])
                .collect();
            self.errors.push(errors::comma_assignments(
                Span::new(start, end),
                &commas,
                &clauses,
                in_block,
            ));
        }
    }

    /// `assign { x := y; y := x; };`: assignments made at once. Only
    /// assignments go in it; anything else is reported and skipped.
    fn assign_block(&mut self) {
        let outer_form = std::mem::replace(&mut self.form, errors::ASSIGN_BLOCK_FORM);
        self.start_node(SyntaxKind::AssignBlock);
        self.bump();
        if self.at(SyntaxKind::LBrace) {
            self.bump();
            while let Some(kind) = self.peek() {
                if matches!(
                    kind,
                    SyntaxKind::RBrace
                        | SyntaxKind::RBracket
                        | SyntaxKind::OperatorKw
                        | SyntaxKind::VarKw
                        | SyntaxKind::BuiltinKw
                ) {
                    break;
                }
                if self.at_assignment() {
                    self.assign_stmt(true);
                } else {
                    self.missing_with_help(
                        SyntaxKind::Ident,
                        "an `assign` block holds only assignments, such as `x := 1;`; anything else goes before or after it",
                    );
                    self.skip_statement(&[
                        SyntaxKind::RBrace,
                        SyntaxKind::RBracket,
                        SyntaxKind::OperatorKw,
                        SyntaxKind::VarKw,
                    ]);
                }
            }
            self.expect(SyntaxKind::RBrace);
        } else {
            self.missing(SyntaxKind::LBrace);
        }
        self.expect(SyntaxKind::Semicolon);
        self.builder.finish_node();
        self.form = outer_form;
    }

    /// The keyword a shorthand for an assignment starts with, if one is
    /// next: the keyword, or a mis-cased spelling of it followed by a name,
    /// as in Tutorial D's `INSERT R …`.
    fn at_clause_keyword(&mut self) -> Option<SyntaxKind> {
        let found = self.found()?;
        if found.kind.is_assignment_keyword() {
            return Some(found.kind);
        }
        let before_name = self
            .found_after(1)
            .is_some_and(|f| f.kind == SyntaxKind::Ident);
        if found.kind != SyntaxKind::Ident || !before_name {
            return None;
        }
        SyntaxKind::ALL
            .into_iter()
            .filter(|k| k.is_assignment_keyword())
            .find(|k| {
                k.fixed_text()
                    .is_some_and(|text| text.eq_ignore_ascii_case(found.text))
            })
    }

    /// One assignment: `v := x`, or a shorthand for one.
    fn clause(&mut self) {
        if self.at_load() {
            self.start_node(SyntaxKind::Load);
            self.load();
            self.builder.finish_node();
            return;
        }
        match self.at_clause_keyword() {
            Some(keyword @ (SyntaxKind::InsertKw | SyntaxKind::DInsertKw)) => {
                self.start_node(SyntaxKind::Insertion);
                self.clause_keyword(keyword);
                let into = self.sql_word("into", keyword);
                self.errors.extend(into);
                self.target();
                self.expression();
            }
            Some(keyword @ (SyntaxKind::DeleteKw | SyntaxKind::IDeleteKw)) => {
                self.start_node(SyntaxKind::Deletion);
                let start = self.clause_keyword(keyword);
                let from = self.sql_word("from", keyword);
                let target = self.target();
                // Without a condition, the fix rewrites the whole deletion,
                // `from` included.
                let conditioned = keyword == SyntaxKind::IDeleteKw || self.at(SyntaxKind::WhereKw);
                if conditioned {
                    self.errors.extend(from);
                }
                if keyword == SyntaxKind::IDeleteKw {
                    self.expression();
                } else {
                    self.condition(start, target);
                }
            }
            Some(keyword) => {
                self.start_node(SyntaxKind::Update);
                self.clause_keyword(keyword);
                self.target();
                // Tutorial D's order has the new values after the condition.
                let values_first = !self.at(SyntaxKind::WhereKw);
                if self.at_sql_set() {
                    self.sql_set();
                } else if values_first {
                    self.assign_list(keyword);
                }
                if self.at(SyntaxKind::WhereKw) {
                    let where_span = self.bump();
                    let outer = std::mem::replace(&mut self.paren_calls, values_first);
                    self.binary(Precedence::WHERE.tighter());
                    self.paren_calls = outer;
                    if !values_first {
                        self.values_after_where(where_span);
                    }
                }
            }
            None => {
                self.peek();
                let start = self.builder.checkpoint();
                let target = self.found().map_or(0, |f| f.span.start);
                self.assignment_target();
                let mut with_eq = None;
                let assign = match self.found() {
                    Some(found) if found.kind == SyntaxKind::Eq => {
                        let span = self.bump();
                        with_eq = Some(errors::assignment_with_eq(span, "in an assignment"));
                        Some(span)
                    }
                    Some(found) if found.kind == SyntaxKind::ColonEq => Some(self.bump()),
                    _ => {
                        self.missing(SyntaxKind::ColonEq);
                        None
                    }
                };
                self.expression();
                // `a := r order [asc x]`: an ordering as a value, which
                // only a `load` makes.
                let order = self.found_word("order").filter(|_| self.at_order());
                if let (Some(assign), Some(order)) = (assign, order) {
                    self.builder.start_node_at(start, SyntaxKind::Load);
                    self.errors
                        .push(errors::assigned_order(assign, target, order.span));
                    let outer_form = std::mem::replace(&mut self.form, errors::LOAD_FORM);
                    self.load_order();
                    self.form = outer_form;
                } else {
                    self.builder.start_node_at(start, SyntaxKind::Assignment);
                    self.errors.extend(with_eq);
                }
            }
        }
        self.builder.finish_node();
    }

    /// `load a from r order [asc x, desc y]`, after its node starts: the
    /// array `a` given the tuples of the relation `r`, or the values of its
    /// only attribute, in order. `load`, `from` and `order` are contextual
    /// keywords.
    fn load(&mut self) {
        let outer_form = std::mem::replace(&mut self.form, errors::LOAD_FORM);
        self.contextual_keyword("load");
        self.target();
        if self.at_word("from") {
            self.contextual_keyword("from");
        } else {
            let at = self.last_end.unwrap_or(0);
            let operand = self.at_operand() && !self.at_order();
            let before = self.found().filter(|_| operand).map(|f| f.span.start);
            self.errors
                .push(errors::load_without_from(Span::empty(at), before));
        }
        if self.at_operand() && !self.at_order() {
            // A whole expression, which ends before `order`: a load is a
            // statement, so, unlike a `tuple from`'s relation, nothing
            // after it could be outside it.
            self.expression();
        } else {
            self.missing_with_help(
                SyntaxKind::Ident,
                "write the relation to load after `from`, as in `load a from r order [asc x]`",
            );
        }
        self.load_order();
        self.form = outer_form;
    }

    /// A load's order, after its relation: `order [asc x, desc y]`. SQL's
    /// `order by`, items without brackets, Tutorial D's `ORDER ( … )`, and
    /// items' habits (see [`Parser::order_item`]), are reported with fixes.
    fn load_order(&mut self) {
        let Some(order) = self.found_word("order") else {
            let at = self.last_end.unwrap_or(0);
            self.errors
                .push(errors::load_without_order(Span::empty(at)));
            return;
        };
        self.contextual_keyword("order");
        let by = self.found_word("by");
        if by.is_some() {
            self.bump();
        }
        let next = self.found();
        let first = next.map_or(order.span.end, |f| f.span.start);
        if next.is_some_and(|f| {
            matches!(
                f.kind,
                SyntaxKind::LBracket | SyntaxKind::LParen | SyntaxKind::LBrace
            )
        }) {
            if let Some(by) = by {
                let gap = Span::new(by.span.start, first);
                self.errors.push(errors::order_by(by, gap, None));
            }
            self.delimited(
                Some(SyntaxKind::OrderList),
                "order",
                (SyntaxKind::LBracket, SyntaxKind::RBracket),
                &[
                    (SyntaxKind::LParen, SyntaxKind::RParen),
                    (SyntaxKind::LBrace, SyntaxKind::RBrace),
                ],
                |p| p.items(Self::at_attribute, Self::order_item),
            );
        } else if self.at(SyntaxKind::Ident) {
            // `order by name desc`, as SQL writes it: the items without
            // brackets.
            self.start_node(SyntaxKind::OrderList);
            self.items(Self::at_attribute, Self::order_item);
            self.builder.finish_node();
            let close = self.last_end.unwrap_or(first);
            self.errors.push(match by {
                Some(by) => errors::order_by(by, Span::new(by.span.start, first), Some(close)),
                None => errors::unbracketed_order(Span::new(order.span.end, first), close),
            });
        } else {
            self.missing_with_help(
                SyntaxKind::LBracket,
                "list the attributes to order by in brackets, each after `asc` or `desc`, as in `order [asc name]`",
            );
        }
    }

    /// An item of a load's order: `asc x` or `desc x`. A direction after
    /// the attribute, as SQL writes it, or spelled out, as in LINQ's
    /// `descending`, an item without one, and SQL's `nulls first` and
    /// `nulls last`, are reported with fixes.
    fn order_item(&mut self) {
        self.start_node(SyntaxKind::OrderItem);
        let first = self.found().expect("at an attribute");
        let prefix = direction(first.text).filter(|_| {
            self.found_after(1)
                .is_some_and(|f| f.kind == SyntaxKind::Ident)
        });
        if let Some(spelled) = prefix {
            self.bump();
            if first.text.eq_ignore_ascii_case(spelled) {
                if first.text != spelled {
                    self.errors.push(errors::miscased_keyword(first, spelled));
                }
            } else {
                self.errors.push(errors::long_direction(first, spelled));
            }
            self.order_attribute();
        } else {
            let attribute = self.order_attribute();
            let after = self
                .found()
                .filter(|f| f.kind == SyntaxKind::Ident)
                .and_then(|f| Some((f, direction(f.text)?)));
            match after {
                Some((found, spelled)) => {
                    self.bump();
                    self.errors.push(errors::misplaced_direction(
                        found,
                        spelled,
                        first,
                        Span::new(attribute.end, found.span.end),
                    ));
                }
                None => self.errors.push(errors::missing_direction(attribute)),
            }
        }
        // SQL's `nulls first` or `nulls last`.
        let nulls = self.found_word("nulls").filter(|_| {
            self.found_after(1).is_some_and(|f| {
                f.kind == SyntaxKind::Ident
                    && (f.text.eq_ignore_ascii_case("first") || f.text.eq_ignore_ascii_case("last"))
            })
        });
        if let Some(nulls) = nulls {
            let before = self.last_end.unwrap_or(nulls.span.start);
            self.start_node(SyntaxKind::Error);
            self.bump();
            self.bump();
            self.builder.finish_node();
            let end = self.last_end.unwrap_or(nulls.span.end);
            self.errors.push(errors::order_nulls(
                Span::new(nulls.span.start, end),
                Span::new(before, end),
            ));
        }
        self.builder.finish_node();
    }

    /// The attribute of an order item, which is next, and its span.
    fn order_attribute(&mut self) -> Span {
        if !self.at(SyntaxKind::Ident) {
            self.missing_with_help(
                SyntaxKind::Ident,
                "name the attribute to order by after its direction, as in `asc name`",
            );
            return Span::empty(self.last_end.unwrap_or(0));
        }
        self.start_node(SyntaxKind::Name);
        let span = self.bump();
        self.builder.finish_node();
        span
    }

    /// Whether a load's order is next: `order`, in any case, before its
    /// items, or SQL's `by`. Elsewhere, as where a load's relation goes,
    /// `order` is a name.
    fn at_order(&mut self) -> bool {
        self.at_word("order")
            && self.found_after(1).is_some_and(|f| {
                matches!(
                    f.kind,
                    SyntaxKind::LBracket | SyntaxKind::LParen | SyntaxKind::LBrace
                ) || (f.kind == SyntaxKind::Ident && f.text.eq_ignore_ascii_case("by"))
            })
    }

    /// Whether the contextual keyword `word` is next, in any case.
    fn at_word(&mut self, word: &str) -> bool {
        self.found_word(word).is_some()
    }

    /// The contextual keyword `word`, in any case, if it is next.
    fn found_word(&mut self, word: &str) -> Option<Found<'a>> {
        self.found()
            .filter(|f| f.kind == SyntaxKind::Ident && f.text.eq_ignore_ascii_case(word))
    }

    /// Consume the contextual keyword `word`, which is next in some case,
    /// reporting any case but lowercase.
    fn contextual_keyword(&mut self, word: &str) {
        let found = self.found().expect("the word is next");
        self.bump();
        if found.text != word {
            self.errors.push(errors::miscased_keyword(found, word));
        }
    }

    /// Consume `keyword`, which [`Parser::at_clause_keyword`] found,
    /// reporting a mis-cased spelling of it. Returns where it starts.
    fn clause_keyword(&mut self, keyword: SyntaxKind) -> usize {
        let found = self.found().expect("the keyword is next");
        self.bump();
        if found.kind != keyword {
            let spelled = keyword.fixed_text().expect("keywords have fixed text");
            self.errors.push(errors::miscased_keyword(found, spelled));
        }
        found.span.start
    }

    /// SQL's `word`, `into` or `from`, after `keyword`, before the
    /// variable's name: consumed, and its error, with a fix deleting it,
    /// returned.
    fn sql_word(&mut self, word: &str, keyword: SyntaxKind) -> Option<Diagnostic> {
        let found = self.found()?;
        let before_name = self
            .found_after(1)
            .is_some_and(|f| f.kind == SyntaxKind::Ident);
        if found.kind == SyntaxKind::Ident && found.text.eq_ignore_ascii_case(word) && before_name {
            self.bump();
            let next = self.found().map_or(found.span.end, |f| f.span.start);
            return Some(errors::sql_statement_word(
                found,
                Span::new(found.span.start, next),
                keyword,
            ));
        }
        None
    }

    /// What an assignment, `target := value`, assigns: a variable, or an
    /// element of one, as in `grid[i][j]`, its positions each an
    /// `IndexExpr` around what is before it. TTM's pseudovariables nest
    /// (RM Prescription 5). An attribute, as in `t.name`, an `ExtractExpr`,
    /// is parsed for sema to report: a tuple's attributes are changed by
    /// `update`.
    fn assignment_target(&mut self) {
        self.peek();
        let start = self.builder.checkpoint();
        self.target();
        loop {
            match self.lookahead().map(|f| f.kind) {
                Some(SyntaxKind::LBracket) => {
                    self.builder.start_node_at(start, SyntaxKind::IndexExpr);
                    self.bump();
                    self.expression();
                    self.expect(SyntaxKind::RBracket);
                }
                Some(SyntaxKind::Dot) => {
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
                }
                _ => break,
            }
            self.builder.finish_node();
        }
    }

    /// The variable an assignment assigns, and its name, if it is there.
    fn target(&mut self) -> Option<&'a str> {
        if self.at(SyntaxKind::Ident) {
            self.start_node(SyntaxKind::NameRef);
            let span = self.bump();
            self.builder.finish_node();
            return Some(&self.source[span.start..span.end]);
        }
        if !self.keyword_as_name(SyntaxKind::NameRef, &[SyntaxKind::ColonEq, SyntaxKind::Eq]) {
            self.missing_with_help(SyntaxKind::Ident, "name the variable to assign");
        }
        None
    }

    /// A `delete`'s `where` and condition, after the variable. Without
    /// them, the deletion is reported, with a fix writing what it would
    /// mean in Tutorial D: the assignment of the empty relation, or, before
    /// a relation, of the variable minus it. `start` is where the deletion
    /// starts, and `target` the variable's name.
    fn condition(&mut self, start: usize, target: Option<&str>) {
        if self.at(SyntaxKind::WhereKw) {
            self.bump();
            self.binary(Precedence::WHERE.tighter());
            return;
        }
        let target_end = self.last_end.unwrap_or(start);
        let relation = self.at_operand().then(|| {
            let from = self.found().map_or(target_end, |f| f.span.start);
            self.expression();
            let to = self.last_end.unwrap_or(from);
            (
                &self.source[from..to],
                self.has_operator(Span::new(from, to)),
            )
        });
        let end = self.last_end.unwrap_or(target_end);
        self.errors.push(errors::delete_without_where(
            Span::new(start, end),
            target,
            relation,
        ));
    }

    /// Whether the expression at `span` has an operator outside any
    /// delimiters, so that it needs parentheses to be an operand.
    fn has_operator(&self, span: Span) -> bool {
        let mut depth = 0usize;
        self.tokens
            .iter()
            .filter(|t| t.span.start >= span.start && t.span.end <= span.end)
            .any(|t| {
                match t.kind {
                    SyntaxKind::LBrace | SyntaxKind::LBracket | SyntaxKind::LParen => depth += 1,
                    SyntaxKind::RBrace | SyntaxKind::RBracket | SyntaxKind::RParen => {
                        depth = depth.saturating_sub(1);
                    }
                    _ => {}
                }
                depth == 0 && t.kind.is_operator()
            })
    }

    /// Whether SQL's `set` is next, in an `update`: `set` before an
    /// attribute's name and its new value.
    fn at_sql_set(&mut self) -> bool {
        self.found()
            .is_some_and(|f| f.kind == SyntaxKind::Ident && f.text.eq_ignore_ascii_case("set"))
            && self
                .found_after(1)
                .is_some_and(|f| f.kind == SyntaxKind::Ident)
    }

    /// SQL's `set a = x, b = y`, in an `update`: parsed as the attributes
    /// it means, and reported with a fix writing them in braces.
    fn sql_set(&mut self) {
        let outer_form = std::mem::replace(&mut self.form, errors::UPDATE_FORM);
        self.start_node(SyntaxKind::AssignList);
        let set = self.bump();
        loop {
            self.assign(errors::Assigning::Update, Precedence::WHERE.tighter());
            // A comma before another `name =`, as SQL writes them,
            // continues the list; any other ends the assignment, as the one
            // before `n := …` does.
            let continues = self.at(SyntaxKind::Comma)
                && self
                    .found_after(1)
                    .is_some_and(|f| f.kind == SyntaxKind::Ident)
                && self
                    .found_after(2)
                    .is_some_and(|f| f.kind == SyntaxKind::Eq);
            if !continues {
                break;
            }
            self.bump();
        }
        self.builder.finish_node();
        self.form = outer_form;
        let end = self.last_end.unwrap_or(set.end);
        self.errors.push(errors::sql_set(set, end));
    }

    /// Tutorial D's order, `update r where p ( a := x )`: the new values,
    /// after the condition, are parsed and reported, with a fix moving them
    /// before `where`, at `where_span`, in braces.
    fn values_after_where(&mut self, where_span: Span) {
        if !self.at(SyntaxKind::LParen) {
            self.missing_with_help(
                SyntaxKind::LBrace,
                "write the new values before `where`, as in `update r { a := x } where p;`",
            );
            return;
        }
        let condition_end = self.last_end.unwrap_or_default();
        self.start_node(SyntaxKind::AssignList);
        let open = self.bump();
        self.items(Self::at_operand, |p| {
            p.assign(errors::Assigning::Update, Precedence::WHERE);
        });
        let close = self.at(SyntaxKind::RParen).then(|| self.bump());
        self.builder.finish_node();
        let Some(close) = close else {
            self.missing(SyntaxKind::RParen);
            return;
        };
        self.errors.push(errors::values_after_where(
            where_span.start,
            condition_end,
            (open, close),
            self.source,
        ));
    }

    fn at_argument(&mut self) -> bool {
        self.peek()
            .is_some_and(|k| k == SyntaxKind::Ident || starts_value(k))
    }

    /// Whether a call expression starts here: a name followed by its
    /// arguments, which have names, `f{a: x}`, or start with a value,
    /// `f{1}`, which sema reports, or are in parentheses, as other
    /// languages write a call. `f{}` and `f{a}` are projections to the
    /// parser, and sema makes `f{}` a call when `f` is an operator.
    fn at_call(&mut self) -> bool {
        let named = |f: Option<Found>| f.is_some_and(|f| f.kind == SyntaxKind::Ident);
        if !named(self.found()) {
            return false;
        }
        match self.found_after(1).map(|f| f.kind) {
            Some(SyntaxKind::LParen) => self.paren_calls,
            Some(SyntaxKind::LBrace) => match self.found_after(2) {
                Some(f) if f.kind == SyntaxKind::Ident => self.found_after(3).is_some_and(|f| {
                    matches!(
                        f.kind,
                        SyntaxKind::Colon | SyntaxKind::Eq | SyntaxKind::ColonEq
                    )
                }),
                Some(f) => starts_value(f.kind),
                None => false,
            },
            _ => false,
        }
    }

    /// `name{arguments}`, at [`Parser::at_call`]: a call of a read-only
    /// operator. Arguments in parentheses are reported, with a fix.
    fn call_expr(&mut self) {
        self.start_node(SyntaxKind::CallExpr);
        self.start_node(SyntaxKind::NameRef);
        self.bump();
        self.builder.finish_node();
        self.delimited(
            Some(SyntaxKind::Arguments),
            "argument list",
            (SyntaxKind::LBrace, SyntaxKind::RBrace),
            &[(SyntaxKind::LParen, SyntaxKind::RParen)],
            |p| p.items(Self::at_argument, Self::argument),
        );
        self.builder.finish_node();
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
        let outer_form = std::mem::replace(&mut self.form, errors::VARIABLE_FORM);
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
                    self.errors
                        .push(errors::assignment_with_eq(span, "for the initial value"));
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
        self.end_statement();
        self.builder.finish_node();
        self.form = outer_form;
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

    /// An attribute's type, in a heading, and for a parameter, `per r`
    /// after it, if it is evaluated for each tuple of the relation `r`.
    /// `per` is a contextual keyword, recognized there before a name, in
    /// any case, which is reported; sema reports it anywhere but in an
    /// operator's parameters.
    fn attribute_type(&mut self) {
        self.type_expr();
        // Looking ahead leaves the whitespace after the type to whatever
        // comes next, unless it is `per`.
        let Some(found) = self
            .lookahead()
            .filter(|f| f.kind == SyntaxKind::Ident && f.text.eq_ignore_ascii_case("per"))
        else {
            return;
        };
        if self
            .lookahead_nth(1)
            .is_none_or(|f| f.kind != SyntaxKind::Ident)
        {
            return;
        }
        self.start_node(SyntaxKind::Per);
        self.bump();
        if found.text != "per" {
            self.errors.push(errors::miscased_keyword(found, "per"));
        }
        self.start_node(SyntaxKind::NameRef);
        self.bump();
        self.builder.finish_node();
        self.builder.finish_node();
    }

    /// A named type, a tuple type, a relation type, or `any type`,
    /// `any tuple`, `any relation` or `any` and a kind, whose words are
    /// contextual keywords, recognized together. `relation any` and `tuple any` are parsed as
    /// what they mean and fixed.
    fn type_expr(&mut self) {
        let word = |found: Option<Found>, word: &str| {
            found.is_some_and(|f| f.kind == SyntaxKind::Ident && f.text == word)
        };
        let next = self.found();
        let after = self.found_after(1);
        if word(next, "any")
            && after.is_some_and(|f| {
                matches!(
                    f.kind,
                    SyntaxKind::Ident
                        | SyntaxKind::TupleKw
                        | SyntaxKind::RelationKw
                        | SyntaxKind::ArrayKw
                )
            })
        {
            self.start_node(SyntaxKind::AnyType);
            self.bump();
            self.bump();
            self.builder.finish_node();
        } else if let Some(keyword) = next.filter(|f| {
            matches!(
                f.kind,
                SyntaxKind::TupleKw | SyntaxKind::RelationKw | SyntaxKind::ArrayKw
            ) && word(after, "any")
        }) {
            self.start_node(SyntaxKind::AnyType);
            let start = self.bump().start;
            let end = self.bump().end;
            self.builder.finish_node();
            self.errors
                .push(errors::any_after(keyword, Span::new(start, end)));
        } else if self.at_keyword(SyntaxKind::TupleKw) || self.at(SyntaxKind::LBrace) {
            // A bare `{`, as in TypeScript, is a tuple type without `tuple`.
            self.start_node(SyntaxKind::TupleType);
            self.keyword(SyntaxKind::TupleKw);
            self.heading_expr();
            self.builder.finish_node();
        } else if self.at_keyword(SyntaxKind::RelationKw) {
            self.start_node(SyntaxKind::RelationType);
            self.keyword(SyntaxKind::RelationKw);
            self.heading_expr();
            self.builder.finish_node();
        } else if self.at(SyntaxKind::ArrayKw) {
            self.start_node(SyntaxKind::ArrayType);
            self.bump();
            self.type_expr();
            self.builder.finish_node();
        } else if self.at(SyntaxKind::Ident) {
            self.start_node(SyntaxKind::TypeRef);
            self.bump();
            self.builder.finish_node();
        } else {
            self.missing_with_help(
                SyntaxKind::TypeRef,
                "write a type, such as `integer`, `tuple { name: text }`, `relation { name: text }` or `array integer`",
            );
        }
    }

    /// An expression.
    /// A whole expression: an `if`, or operators down to `where`. Other
    /// languages' conditionals after one, Python's `a if c else b` and C's
    /// `c ? a : b`, are reported, with fixes.
    fn expression(&mut self) {
        self.peek();
        let start = self.builder.checkpoint();
        let from = self.lookahead().map(|f| f.span.start);
        if self.at_if() {
            self.if_expression();
        } else {
            self.binary(Precedence::WHERE);
        }
        if let Some(from) = from {
            self.foreign_conditional(start, from);
        }
    }

    /// An expression whose operators bind at least as tightly as `min`: a
    /// whole one, an `if` too, at 0, and else one that an `if`'s `else`
    /// branch would run past the end of, unless parenthesized.
    fn value(&mut self, min: Precedence) {
        if min == Precedence::WHERE {
            self.expression();
        } else {
            self.binary(min);
        }
    }

    /// Whether an `if` starts here: the keyword, or Tutorial D's `IF` in
    /// another case, which `then`, in any case, follows before the end of
    /// the expression.
    fn at_if(&mut self) -> bool {
        let Some(found) = self.found() else {
            return false;
        };
        if found.kind == SyntaxKind::IfKw {
            return true;
        }
        if !found.spells(SyntaxKind::IfKw) {
            return false;
        }
        let mut depth = 0_usize;
        for i in self.pos + 1..self.tokens.len() {
            let token = self.tokens[i];
            match token.kind {
                SyntaxKind::LBrace | SyntaxKind::LBracket | SyntaxKind::LParen => depth += 1,
                SyntaxKind::RBrace | SyntaxKind::RBracket | SyntaxKind::RParen if depth == 0 => {
                    return false;
                }
                SyntaxKind::RBrace | SyntaxKind::RBracket | SyntaxKind::RParen => depth -= 1,
                SyntaxKind::Semicolon | SyntaxKind::Comma if depth == 0 => return false,
                _ if depth == 0 && self.found_at(i).spells(SyntaxKind::ThenKw) => return true,
                _ => {}
            }
        }
        false
    }

    /// `if condition then value else value`, each part a whole expression,
    /// so the `else` branch takes all that follows. Every `if` has an
    /// `else`, since it has a value either way. Its keywords in capitals,
    /// Python's `elif` and its kin for `else if`, and Tutorial D's `END IF`
    /// after the last value, are reported, with fixes.
    fn if_expression(&mut self) {
        let outer_form = std::mem::replace(&mut self.form, errors::IF_FORM);
        self.start_node(SyntaxKind::IfExpr);
        self.any_case(SyntaxKind::IfKw);
        self.if_branches();
        self.builder.finish_node();
        self.end_if();
        self.form = outer_form;
    }

    /// What follows an `if`'s keyword: the condition, and its branches.
    fn if_branches(&mut self) {
        self.expression();
        self.any_case(SyntaxKind::ThenKw);
        self.expression();
        if let Some(found) = self.at_else_if_spelling() {
            // `elif c then …`: an `else if`, whose `if` it is too.
            self.start_node(SyntaxKind::IfExpr);
            self.bump();
            self.errors.push(errors::else_if_spelling(found));
            self.if_branches();
            self.builder.finish_node();
            return;
        }
        match self.found() {
            Some(found) if found.spells(SyntaxKind::ElseKw) => {
                self.any_case(SyntaxKind::ElseKw);
                self.expression();
            }
            // `else` can be inserted, but not the value after it.
            _ => {
                self.missing(SyntaxKind::ElseKw);
                self.missing_with_help(
                    SyntaxKind::Literal,
                    "an `if` has a value either way: write `else` and the value when the condition is false",
                );
            }
        }
    }

    /// Tutorial D's `END IF` after an `if`'s last value, in any case:
    /// consumed, so nothing else cascades, and reported, with the fix that
    /// deletes it.
    fn end_if(&mut self) {
        let end = self
            .lookahead_nth(0)
            .is_some_and(|f| f.kind == SyntaxKind::Ident && f.text.eq_ignore_ascii_case("end"));
        if !end
            || !self
                .lookahead_nth(1)
                .is_some_and(|f| f.spells(SyntaxKind::IfKw))
        {
            return;
        }
        let start = self.last_end;
        self.start_node(SyntaxKind::Error);
        let end = self.bump().start;
        let span = Span::new(end, self.bump().end);
        self.builder.finish_node();
        self.errors.push(errors::end_if(
            span,
            Span::new(start.unwrap_or(span.start), span.end),
        ));
    }

    /// Python's `a if c else b`, or C's `c ? a : b`, after the expression
    /// that started at `start`, from `from`, if there is one: parsed, each
    /// chained one too, as in `a if c else b if d else e`, and reported,
    /// with the fix to `if c then a else b`.
    fn foreign_conditional(&mut self, start: trendy_syntax::Checkpoint, from: usize) {
        let Some(found) = self.lookahead() else {
            return;
        };
        let style = match found.kind {
            SyntaxKind::IfKw => errors::Conditional::Python,
            SyntaxKind::ForeignOperator if found.text == "?" => errors::Conditional::C,
            _ => return,
        };
        let (opener, separator) = match style {
            errors::Conditional::Python => (SyntaxKind::IfKw, SyntaxKind::ElseKw),
            errors::Conditional::C => (SyntaxKind::ForeignOperator, SyntaxKind::Colon),
        };
        let Some(first_end) = self.last_end else {
            return;
        };
        self.builder.start_node_at(start, SyntaxKind::Error);
        // Each part's text, in the order written: a value or a condition
        // first, as the style says, then alternately the other, and the
        // last value.
        let mut parts = vec![self.source[from..first_end].to_string()];
        loop {
            self.bump();
            parts.push(self.part());
            if !self.at(separator) {
                self.missing(separator);
                self.builder.finish_node();
                return;
            }
            self.bump();
            parts.push(self.part());
            if !self.lookahead().is_some_and(|f| {
                f.kind == opener && (opener != SyntaxKind::ForeignOperator || f.text == "?")
            }) {
                break;
            }
        }
        self.builder.finish_node();
        let span = Span::new(from, self.last_end.unwrap_or(first_end));
        self.errors
            .push(errors::foreign_conditional(style, span, &parts, found.span));
    }

    /// A part of another language's conditional, as text: operators down
    /// to `where`.
    fn part(&mut self) -> String {
        let from = self.lookahead().map_or(0, |f| f.span.start);
        self.binary(Precedence::WHERE);
        let end = self.last_end.unwrap_or(from).max(from);
        self.source[from..end].to_string()
    }

    /// An expression whose dyadic operators bind at least as tightly as
    /// `min`, by precedence climbing (see [`Precedence`]). The prefix
    /// operators, `not`, `-` and `+`, bind tightest, so `not a = b` is
    /// `(not a) = b`.
    fn binary(&mut self, min: Precedence) {
        self.binary_within(min..=Precedence::TIGHTEST);
    }

    /// An expression whose dyadic operators' precedences are in
    /// `precedences`: it ends before an operator that binds more loosely,
    /// as every expression does, or more tightly, as the relation of a
    /// `tuple from` does before arithmetic.
    fn binary_within(&mut self, precedences: RangeInclusive<Precedence>) {
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
                words,
            } = operator;
            if !precedences.contains(&precedence) {
                break;
            }
            if let Some(middle) = compared.filter(|_| precedence == Precedence::COMPARISON) {
                let before = self.source[..found.span.start].chars().next_back();
                self.errors.push(errors::chained_comparison(
                    found,
                    &self.source[middle.start..middle.end],
                    before,
                ));
            }
            self.builder.start_node_at(start, SyntaxKind::BinaryExpr);
            match words {
                Words::One => self.operator_token(found, keyword),
                Words::NotMatching => {
                    self.operator_token(found, SyntaxKind::NotKw);
                    let matching = self.found().expect("`matching` follows `not`");
                    self.operator_token(matching, SyntaxKind::MatchingKw);
                }
                Words::Sql(tokens) => self.sql_join(found, keyword, tokens),
                Words::Renamed { spelled } => {
                    self.bump();
                    self.errors.push(errors::renamed_operator(found, spelled));
                }
            }
            if keyword.is_set_operator() {
                self.set_quantifier();
            }
            let rhs_start = self.lookahead().map(|f| f.span.start);
            let right_grouping = keyword == SyntaxKind::ImpliesKw;
            let right = if right_grouping {
                precedence
            } else {
                precedence.tighter()
            };
            self.binary_within(right..=*precedences.end());
            compared = match (
                precedence == Precedence::COMPARISON,
                rhs_start,
                self.last_end,
            ) {
                (true, Some(start), Some(end)) if start < end => Some(Span::new(start, end)),
                _ => None,
            };
            if precedence == Precedence::JOIN {
                self.join_condition();
            }
            self.builder.finish_node();
        }
        self.value_end = self.last_end;
    }

    /// The dyadic operator at the next token, if any.
    fn binary_operator(&self) -> Option<BinaryOperator<'a>> {
        let found = self.lookahead_nth(0)?;
        if let Some((keyword, tokens)) = self.sql_join_at() {
            return Some(BinaryOperator {
                precedence: Precedence::JOIN,
                found,
                keyword,
                words: Words::Sql(tokens),
            });
        }
        if found.kind == SyntaxKind::Ident
            && let Some((keyword, spelled)) = errors::operator_name(found.text)
        {
            return Some(BinaryOperator {
                precedence: keyword.precedence()?,
                found,
                keyword,
                words: Words::Renamed { spelled },
            });
        }
        let keyword = match found.kind {
            SyntaxKind::ForeignOperator => errors::foreign_operator(found.text)?,
            SyntaxKind::Ident => errors::operator_spelling(found.text)?,
            kind => kind,
        };
        // `not matching`: `not` is otherwise a prefix operator only.
        let matching = |f: Found| f.spells(SyntaxKind::MatchingKw);
        if keyword == SyntaxKind::NotKw && self.lookahead_nth(1).is_some_and(matching) {
            return Some(BinaryOperator {
                precedence: Precedence::JOIN,
                found,
                keyword: SyntaxKind::MatchingKw,
                words: Words::NotMatching,
            });
        }
        Some(BinaryOperator {
            precedence: keyword.precedence()?,
            found,
            keyword,
            words: Words::One,
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

    /// SQL's `all` or `distinct` after a set operator, as in `union all`:
    /// consumed, so the right operand still parses, and reported. A relation
    /// has no duplicates, so neither changes anything.
    fn set_quantifier(&mut self) {
        let Some(found) = self.lookahead_nth(0).filter(|f| {
            f.kind == SyntaxKind::Ident
                && (f.text.eq_ignore_ascii_case("all") || f.text.eq_ignore_ascii_case("distinct"))
        }) else {
            return;
        };
        let start = self.last_end.unwrap_or(found.span.start);
        self.start_node(SyntaxKind::Error);
        self.bump();
        self.builder.finish_node();
        self.errors.push(errors::set_quantifier(
            found,
            Span::new(start, found.span.end),
        ));
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
            self.binary(Precedence::JOIN.tighter());
            while self
                .lookahead_nth(0)
                .is_some_and(|f| f.kind.is_comparison() || f.kind == SyntaxKind::AndKw)
            {
                // `on a.x = b.y and a.z = b.w`: the whole condition.
                self.bump();
                self.binary(Precedence::JOIN.tighter());
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
        if let Some(taken) = self.at_taken() {
            self.taken(taken);
            return;
        }
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

    /// What a `from` expression starting here takes: `tuple from` or
    /// `value from`, in any case. `value` and `from` are contextual
    /// keywords: a name followed by another can't be anything else.
    fn at_taken(&mut self) -> Option<trendy_syntax::ast::Taken> {
        let first = self.found()?;
        let from = self.found_after(1)?;
        if from.kind != SyntaxKind::Ident || !from.text.eq_ignore_ascii_case("from") {
            return None;
        }
        if first.spells(SyntaxKind::TupleKw) {
            Some(trendy_syntax::ast::Taken::Tuple)
        } else if first.kind == SyntaxKind::Ident && first.text.eq_ignore_ascii_case("value") {
            Some(trendy_syntax::ast::Taken::Value)
        } else {
            None
        }
    }

    /// `tuple from r` or `value from r`, a prefix operator, so postfix
    /// operators bind tighter: `value from r{a}` is of `r{a}`. Capitals, as
    /// in Tutorial D's `TUPLE FROM R`, are fixed.
    fn taken(&mut self, taken: trendy_syntax::ast::Taken) {
        self.start_node(SyntaxKind::FromExpr);
        let word = match taken {
            trendy_syntax::ast::Taken::Tuple => "tuple",
            trendy_syntax::ast::Taken::Value => "value",
        };
        for expected in [word, "from"] {
            let found = self.found().expect("at `tuple from` or `value from`");
            self.bump();
            if found.text != expected {
                self.errors.push(errors::miscased_keyword(found, expected));
            }
        }
        if self.at_operand() {
            self.relation_expression();
        } else {
            self.missing_with_help(
                SyntaxKind::Ident,
                "write the relation after `from`, as in `tuple from r`",
            );
        }
        self.builder.finish_node();
    }

    /// A relation's expression, as Tutorial D's `TUPLE FROM` takes: its
    /// relational operators, from the unions to the joins, and a `where`
    /// with its condition. So `tuple from r where p` is of `r where p`,
    /// while `tuple from r = t` compares the tuple, as a comparison binds
    /// more loosely, and `value from r{a} + 1` adds to the value, as no
    /// relation has arithmetic (TTM, Appendix I's `<relation exp>`).
    fn relation_expression(&mut self) {
        self.peek();
        let start = self.builder.checkpoint();
        self.binary_within(Precedence::UNION..=Precedence::JOIN);
        if let Some(operator) = self
            .binary_operator()
            .filter(|o| o.keyword == SyntaxKind::WhereKw)
        {
            self.builder.start_node_at(start, SyntaxKind::BinaryExpr);
            self.operator_token(operator.found, operator.keyword);
            self.binary(Precedence::WHERE.tighter());
            self.builder.finish_node();
        }
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
        // Whether the value so far is an attribute's, `x.f`, which a method
        // call, as other languages write one, follows with `()`.
        let mut extracted = false;
        loop {
            let next = self.lookahead().map(|f| f.kind);
            let method_call = extracted && next == Some(SyntaxKind::LParen);
            extracted = next == Some(SyntaxKind::Dot);
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
            } else if next == Some(SyntaxKind::LBracket) {
                // A `[` after a value is always an element's position.
                self.builder.start_node_at(start, SyntaxKind::IndexExpr);
                self.bump();
                self.expression();
                self.expect(SyntaxKind::RBracket);
            } else if (next == Some(SyntaxKind::LBrace) && !self.at_braced_statements())
                || method_call
            {
                // `x.f()` is reported as `x.f{}` is, once its `()` is fixed.
                self.builder.start_node_at(start, SyntaxKind::ProjectExpr);
                let what = if method_call {
                    "arguments"
                } else {
                    "attribute names"
                };
                self.listed_names(what, errors::PROJECTION_FORM);
            } else if let Some((found, keyword)) = self.postfix_operator() {
                self.builder.start_node_at(start, SyntaxKind::PostfixExpr);
                self.operator_token(found, keyword);
                match keyword {
                    SyntaxKind::RenameKw => self.rename_list(),
                    SyntaxKind::ExtendKw | SyntaxKind::ReplaceKw => self.assign_list(keyword),
                    SyntaxKind::GroupKw | SyntaxKind::WrapKw => self.gather_list(keyword),
                    SyntaxKind::UngroupKw | SyntaxKind::UnwrapKw => self.unpacked(keyword),
                    _ => self.names(errors::PROJECTION_FORM),
                }
            } else {
                break;
            }
            self.builder.finish_node();
        }
    }

    /// Whether the `{` next starts statements, as C writes a block, in
    /// `while (c) { … }`: a `;` or `:=` is right inside it, which a
    /// projection's names never have.
    fn at_braced_statements(&self) -> bool {
        let mut depth = 0_usize;
        for token in &self.tokens[self.pos..] {
            match token.kind {
                SyntaxKind::LBrace | SyntaxKind::LBracket | SyntaxKind::LParen => depth += 1,
                SyntaxKind::RBrace | SyntaxKind::RBracket | SyntaxKind::RParen => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        return false;
                    }
                }
                SyntaxKind::Semicolon | SyntaxKind::ColonEq if depth == 1 => return true,
                _ => {}
            }
        }
        false
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
                | SyntaxKind::UngroupKw
                | SyntaxKind::WrapKw
                | SyntaxKind::UnwrapKw
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
            Some(found) if found.kind == SyntaxKind::Colon => {
                let neighbors = (
                    self.source[..found.span.start].chars().next_back(),
                    self.source[found.span.end..].chars().next(),
                );
                self.bump();
                self.errors
                    .push(errors::colon_in_renaming(found, neighbors));
            }
            _ => self.any_case(SyntaxKind::AsKw),
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
        // An `update` isn't an operator that could be chained, so brackets
        // are only a wrong delimiter there.
        let (delimiters, mistakes): (_, &[_]) = if assigning == errors::Assigning::Update {
            (
                (SyntaxKind::LBrace, SyntaxKind::RBrace),
                &[
                    (SyntaxKind::LParen, SyntaxKind::RParen),
                    (SyntaxKind::LBracket, SyntaxKind::RBracket),
                ],
            )
        } else {
            (
                self.list_delimiters(),
                &[(SyntaxKind::LParen, SyntaxKind::RParen)],
            )
        };
        self.delimited(
            Some(SyntaxKind::AssignList),
            assigning.list(),
            delimiters,
            mistakes,
            move |p| {
                p.items(Self::at_operand, move |p| {
                    p.assign(assigning, Precedence::WHERE);
                });
            },
        );
        self.form = outer_form;
    }

    /// `a := x`. `a: x` and `a = x`, as a tuple and as keyword arguments
    /// are written, are fixed to `a := x`, and `x as a`, as SQL and Tutorial
    /// D's `ADD` write it, to `a := x`. The value's operators bind at least
    /// as tightly as `min`: after SQL's `set`, a `where` ends it.
    fn assign(&mut self, assigning: errors::Assigning, min: Precedence) {
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
            self.value(min);
        } else {
            let start = self.found().map_or(0, |f| f.span.start);
            self.value(min);
            let value_end = self.last_end.unwrap_or(start);
            match self.found() {
                Some(found) if found.spells(SyntaxKind::AsKw) => {
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

    /// `{ g := {a, b}, h := {c} }`: gatherings, after `keyword`, `group` or
    /// `wrap`. SQL's `group by {a}`, which names the attributes kept rather
    /// than gathered, is reported.
    fn gather_list(&mut self, keyword: SyntaxKind) {
        let gathering = errors::Gathering::of(keyword);
        let outer_form = std::mem::replace(&mut self.form, gathering.form());
        if let Some(by) = self.found().filter(|f| {
            gathering == errors::Gathering::Group
                && f.kind == SyntaxKind::Ident
                && f.text.eq_ignore_ascii_case("by")
        }) {
            self.start_node(SyntaxKind::Error);
            self.bump();
            if self.at(SyntaxKind::LBrace) {
                self.names(gathering.form());
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
            Some(SyntaxKind::GatherList),
            gathering.list(),
            delimiters,
            &[(SyntaxKind::LParen, SyntaxKind::RParen)],
            move |p| p.items(Self::at_gathering, move |p| p.gathering(gathering)),
        );
        self.form = outer_form;
    }

    fn at_gathering(&mut self) -> bool {
        matches!(self.peek(), Some(SyntaxKind::Ident | SyntaxKind::LBrace))
    }

    /// `g := {a, b}`. `g: {a, b}` and `g = {a, b}` are fixed to `:=`, and
    /// Tutorial D's `{a, b} as g` to `g := {a, b}`.
    fn gathering(&mut self, gathering: errors::Gathering) {
        self.start_node(SyntaxKind::Gathering);
        if self.at(SyntaxKind::LBrace) {
            let start = self.found().map_or(0, |f| f.span.start);
            self.names(gathering.form());
            let list_end = self.last_end.unwrap_or(start);
            match self.found() {
                Some(found) if found.spells(SyntaxKind::AsKw) => {
                    self.bump();
                    if let Some(name) = self.found().filter(|f| f.kind == SyntaxKind::Ident) {
                        self.start_node(SyntaxKind::Name);
                        self.bump();
                        self.builder.finish_node();
                        let list = &self.source[start..list_end];
                        self.errors.push(errors::gathering_as(
                            gathering,
                            Span::new(start, name.span.end),
                            name.text,
                            list,
                        ));
                    } else {
                        self.missing_with_help(SyntaxKind::Ident, gathering.name_help());
                    }
                }
                _ => self.errors.push(errors::unnamed_gathering(
                    gathering,
                    Span::new(start, list_end),
                )),
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
                    self.errors.push(errors::gathering_separator(
                        gathering, found.kind, span, neighbors,
                    ));
                }
                _ => self.missing(SyntaxKind::ColonEq),
            }
            self.names(gathering.form());
        }
        self.builder.finish_node();
    }

    /// `{g, h}`: the attributes to unpack, after `keyword`, `ungroup` or
    /// `unwrap`. A bare name, as in `ungroup g`, is put in braces.
    fn unpacked(&mut self, keyword: SyntaxKind) {
        let unpacking = errors::Unpacking::of(keyword);
        if let Some(found) = self.found().filter(|f| f.kind == SyntaxKind::Ident) {
            self.start_node(SyntaxKind::NameList);
            self.start_node(SyntaxKind::Name);
            self.bump();
            self.builder.finish_node();
            self.builder.finish_node();
            self.errors.push(errors::bare_unpacked(unpacking, found));
            return;
        }
        self.names(unpacking.form());
    }

    /// `{ a, b }`: attribute names, in a construct of the given `form`.
    fn names(&mut self, form: &'static str) {
        self.listed_names("attribute names", form);
    }

    /// [`Parser::names`], called `what` where they are reported in `()`.
    fn listed_names(&mut self, what: &'static str, form: &'static str) {
        let outer_form = std::mem::replace(&mut self.form, form);
        self.delimited(
            Some(SyntaxKind::NameList),
            what,
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
    /// Whether an expression starts at the next token: an operand that
    /// [`Parser::primary`] takes, or a prefix operator before one.
    fn at_expression(&mut self) -> bool {
        let Some(found) = self.found() else {
            return false;
        };
        let prefix = match found.kind {
            SyntaxKind::ForeignOperator => errors::foreign_operator(found.text),
            kind => Some(kind),
        };
        found.kind.is_literal()
            || matches!(
                found.kind,
                SyntaxKind::Ident
                    | SyntaxKind::LParen
                    | SyntaxKind::LBracket
                    | SyntaxKind::LBrace
                    | SyntaxKind::TupleKw
                    | SyntaxKind::RelationKw
                    | SyntaxKind::ArrayKw
            )
            || matches!(
                prefix,
                Some(SyntaxKind::NotKw | SyntaxKind::Minus | SyntaxKind::Plus)
            )
            || self.at_if()
            || self.at_nary().is_some()
    }

    fn primary(&mut self) {
        if self.at(SyntaxKind::LParen) {
            self.start_node(SyntaxKind::ParenExpr);
            self.bump();
            self.expression();
            self.expect(SyntaxKind::RParen);
            self.builder.finish_node();
            return;
        }
        if self.at_if() {
            // An operand: an `if` takes all that follows, so it needs
            // parentheses to say where it ends.
            let from = self.lookahead().map_or(0, |f| f.span.start);
            self.if_expression();
            let end = self.last_end.unwrap_or(from);
            self.errors
                .push(errors::unparenthesized_if(Span::new(from, end)));
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
        } else if self.at_value_block() {
            self.value_block();
        } else if self.at(SyntaxKind::ArrayKw)
            || self.at(SyntaxKind::LBracket)
            || self.at_miscased_array()
        {
            self.array_literal();
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
        } else if self.at_call() {
            self.call_expr();
        } else if self.at(SyntaxKind::Ident) {
            self.start_node(SyntaxKind::NameRef);
            self.bump();
            self.builder.finish_node();
        } else {
            self.missing_with_help(
                SyntaxKind::Literal,
                "write a value, such as `0`, `\"\"`, `tuple { name: \"x\" }`, `relation { { name: \"x\" } }` or `array [1, 2]`",
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
        // `matching` isn't commutative, so it has no list.
        let nary = matches!(
            keyword,
            SyntaxKind::JoinKw
                | SyntaxKind::TimesKw
                | SyntaxKind::ComposeKw
                | SyntaxKind::UnionKw
                | SyntaxKind::DUnionKw
                | SyntaxKind::XUnionKw
                | SyntaxKind::IntersectKw
                | SyntaxKind::MinusKw
                | SyntaxKind::IMinusKw
        );
        (nary && list).then_some((found, keyword))
    }

    /// `join{r, s, t}`, `times{…}`, `union{…}`, `intersect{…}`,
    /// `compose[r, s, t]` or `minus[…]`: the keyword, found as `found`, and
    /// the operands. Braces enclose what can be in any order, and brackets
    /// what can't, the left folds, so the others are fixed to them.
    fn nary(&mut self, found: Found<'a>, keyword: SyntaxKind) {
        let outer_form = std::mem::replace(&mut self.form, errors::NARY_FORM);
        self.start_node(SyntaxKind::NaryExpr);
        self.operator_token(found, keyword);
        let braces = (SyntaxKind::LBrace, SyntaxKind::RBrace);
        let brackets = (SyntaxKind::LBracket, SyntaxKind::RBracket);
        let parens = (SyntaxKind::LParen, SyntaxKind::RParen);
        let ordered = matches!(
            keyword,
            SyntaxKind::ComposeKw | SyntaxKind::MinusKw | SyntaxKind::IMinusKw
        );
        let (delims, mistakes) = if ordered {
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

    /// `tuple [heading] { name: value, … }`, its heading written out or a
    /// declared one's name. In a relation, the `tuple` keyword may be left
    /// out; elsewhere a bare `{` is reported, with a fix.
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
        } else if has_keyword && self.at(SyntaxKind::Ident) {
            self.heading_ref();
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

    /// `array [type] [ element, … ]`, its elements' type written only for
    /// an empty one with no other. A bare `[`, as other languages write an
    /// array, and SQL's `ARRAY[`, are reported, with fixes.
    fn array_literal(&mut self) {
        let outer_form = std::mem::replace(&mut self.form, errors::ARRAY_VALUE_FORM);
        self.start_node(SyntaxKind::ArrayLiteral);
        match self.found() {
            // `ARRAY[1, 2]`, as SQL writes it.
            Some(found) if found.kind == SyntaxKind::Ident => {
                self.bump();
                self.errors.push(errors::miscased_keyword(found, "array"));
            }
            _ => self.keyword(SyntaxKind::ArrayKw),
        }
        if self.peek().is_some_and(|k| k != SyntaxKind::LBracket) {
            self.type_expr();
        }
        self.delimited(
            None,
            "array",
            (SyntaxKind::LBracket, SyntaxKind::RBracket),
            &[],
            |p| {
                // A tuple selector, its `tuple` keyword optional here, as in
                // a relation, or any expression.
                p.items(Self::at_operand, |p| {
                    if p.at(SyntaxKind::LBrace) {
                        p.tuple_literal(true);
                    } else {
                        p.expression();
                    }
                });
            },
        );
        self.builder.finish_node();
        self.form = outer_form;
    }

    /// `relation [heading] { tuple, … }`, its heading written out or a
    /// declared one's name.
    fn relation_literal(&mut self) {
        let outer_form = std::mem::replace(&mut self.form, errors::RELATION_VALUE_FORM);
        self.start_node(SyntaxKind::RelationLiteral);
        self.keyword(SyntaxKind::RelationKw);
        if self.at_heading() {
            self.heading();
        } else if self.at(SyntaxKind::Ident) {
            self.heading_ref();
            if !self.at(SyntaxKind::LBrace) {
                // `relation home;`: a declared heading's type, as a value.
                let at = self.last_end.unwrap_or(0);
                let found = self.found();
                self.errors.push(errors::relation_without_tuples(at, found));
                self.builder.finish_node();
                self.form = outer_form;
                return;
            }
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
                // A tuple selector, its `tuple` keyword optional here, or
                // any expression of a tuple, such as a variable.
                p.items(Self::at_operand, |p| {
                    let selector = p.at(SyntaxKind::LBrace)
                        || (p.at_keyword(SyntaxKind::TupleKw) && p.at_taken().is_none());
                    if selector {
                        p.tuple_literal(true);
                    } else {
                        p.expression();
                    }
                });
            },
        );
        self.builder.finish_node();
        self.form = outer_form;
    }

    /// A heading after `relation` or `tuple` in a type: a declared one's
    /// name, or its attributes written out.
    fn heading_expr(&mut self) {
        if self.at(SyntaxKind::Ident) {
            self.heading_ref();
        } else {
            self.heading();
        }
    }

    /// A declared heading's name.
    fn heading_ref(&mut self) {
        self.start_node(SyntaxKind::HeadingRef);
        self.bump();
        self.builder.finish_node();
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
                    p.attribute(SyntaxKind::AttributeType, Self::attribute_type);
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
        found.kind == keyword
            || (found.spells(keyword)
                && self
                    .found_after(1)
                    .is_some_and(|f| f.kind == SyntaxKind::LBrace))
    }

    /// Consume `keyword`, or the same word in another case, which is
    /// reported, wherever it is, or record it as missing.
    fn any_case(&mut self, keyword: SyntaxKind) {
        match self.found() {
            Some(found) if found.kind == keyword => {
                self.bump();
            }
            Some(found) if found.spells(keyword) => {
                self.bump();
                let spelled = keyword.fixed_text().expect("keywords have fixed text");
                self.errors.push(errors::miscased_keyword(found, spelled));
            }
            _ => self.missing(keyword),
        }
    }

    /// Consume `keyword`, a selector's, reporting a mis-cased spelling of
    /// it before the selector's `{`, or record it as missing.
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

    /// Whether an array selector starts here with its keyword in another
    /// case, as SQL writes `ARRAY[1, 2]`: a word spelled `array`, then
    /// brackets that no index could be, empty or with several elements. One
    /// element, as in `ARRAY[1]`, is a value of that name indexed, to the
    /// parser.
    fn at_miscased_array(&mut self) -> bool {
        if !self
            .found()
            .is_some_and(|f| f.kind == SyntaxKind::Ident && f.text.eq_ignore_ascii_case("array"))
        {
            return false;
        }
        let mut significant = (self.pos..self.tokens.len())
            .map(|i| self.tokens[i].kind)
            .filter(|&k| !is_skipped(k))
            .skip(1);
        if significant.next() != Some(SyntaxKind::LBracket) {
            return false;
        }
        let mut depth = 1_usize;
        for (i, kind) in significant.enumerate() {
            match kind {
                SyntaxKind::LBracket | SyntaxKind::LBrace | SyntaxKind::LParen => depth += 1,
                SyntaxKind::RBracket | SyntaxKind::RBrace | SyntaxKind::RParen => {
                    depth -= 1;
                    if depth == 0 {
                        return i == 0;
                    }
                }
                SyntaxKind::Comma if depth == 1 => return true,
                _ => {}
            }
        }
        false
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
        }) && !self.at_heading_decl()
        {
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

/// Where a statement being skipped ends, before its `;`: at its body's
/// `]`, or at the keyword of the next definition or declaration.
const STATEMENT_STOPS: [SyntaxKind; 3] = [
    SyntaxKind::RBracket,
    SyntaxKind::OperatorKw,
    SyntaxKind::VarKw,
];

fn is_skipped(kind: SyntaxKind) -> bool {
    kind.is_trivia() || kind == SyntaxKind::ErrorToken
}

#[cfg(test)]
mod tests {
    use super::*;
    use trendy_diag::Code;
    use trendy_syntax::ast::{AstNode, Clause, Expr, Statement, TypeExpr};
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
        assert_eq!(fix("var i: integer 0;"), "var i: integer := 0;");
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
        // In a body, the declaration is parsed as what it means.
        assert_eq!(
            fix("operator f{} [ let x = 0; const y: integer = x; ];"),
            "operator f{} [ var x := 0; var y: integer := x; ];"
        );
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
        one_fix("var p := { x: 1 };", "var p := tuple { x: 1 };");
        one_fix("var p: { x: integer };", "var p: tuple { x: integer };");
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
        // Tutorial D's `CALL`, not an operator named `CALL` without one.
        assert_eq!(
            fix("operator main{} [ CALL debug_value{v: 1}; ];"),
            "operator main{} [ call debug_value{v: 1}; ];"
        );
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
        assert_eq!(with_heading.elements().count(), 1);
        let without = literal("var r := relation { { a: 1 }, { a: 2 } };");
        assert!(without.heading().is_none());
        assert_eq!(without.elements().count(), 2);
        // Any expression of a tuple, such as a variable, is an element.
        let parse = parse_str("var r := relation { t, { a: 1 }, (u) };");
        assert!(parse.errors.is_empty(), "{:#?}", parse.errors);
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
    fn arrays() {
        for source in [
            "var a: array integer := array [1, 2, 3];",
            "var e := array text [];",
            "var n: array array integer := array [array [1], array integer []];",
            "var t := array [{ a: 1 }, tuple { a: 2 }];",
            "var r: relation { months: array text };",
            "var x := a[1][2] + a[length{a: a}];",
            "operator f{a: any array, b: array T}: T types { T: any type } [ b[1] ];",
        ] {
            let parse = parse_str(source);
            assert!(parse.errors.is_empty(), "{source}: {:#?}", parse.errors);
        }
        let initializer = |source: &str| {
            let decl = parse_str(source).tree.variables().next().unwrap();
            decl.initializer().unwrap()
        };
        let Expr::Array(typed) = initializer("var e := array text [];") else {
            panic!("an array value");
        };
        assert!(typed.element_type().is_some());
        assert_eq!(typed.elements().count(), 0);
        let Expr::Array(listed) = initializer("var a := array [1, 2];") else {
            panic!("an array value");
        };
        assert!(listed.element_type().is_none());
        assert_eq!(listed.elements().count(), 2);
        // An index binds to what it follows, as `.` does.
        let Expr::Binary(sum) = initializer("var x := -a[1] + a.b[2];") else {
            panic!("a sum");
        };
        assert!(matches!(sum.lhs(), Some(Expr::Unary(_))));
        let Some(Expr::Index(index)) = sum.rhs() else {
            panic!("an index");
        };
        assert!(matches!(index.operand(), Some(Expr::Extract(_))));
        assert!(matches!(index.index(), Some(Expr::Literal(_))));
    }

    #[test]
    fn array_recovery() {
        let one_fix = |source: &str, fixed: &str| {
            let errors = parse_str(source).errors;
            assert_eq!(errors.len(), 1, "{source}: {errors:#?}");
            assert_eq!(fix(source), fixed, "{source}");
        };
        // Python's and JavaScript's lists, and SQL's arrays.
        one_fix("var a := [1, 2];", "var a := array [1, 2];");
        one_fix("var a := [];", "var a := array [];");
        one_fix("var a := ARRAY[1, 2];", "var a := array[1, 2];");
        one_fix("var a := Array[];", "var a := array[];");
        // One element could be a value of that name indexed.
        assert!(parse_str("var a := ARRAY[1];").errors.is_empty());
        one_fix("var a: array any;", "var a: any array;");
        // A method call's parentheses, fixed to braces, which sema then
        // fixes to a call.
        let errors = parse_str("var n := a.length();").errors;
        assert_eq!(errors.len(), 1, "{errors:#?}");
        assert_eq!(errors[0].code, Code::WrongDelimiter);
        assert!(errors[0].message.contains("arguments"), "{errors:#?}");
        assert_eq!(fix("var n := a.length();"), "var n := a.length{};");
        // A missing `]`.
        one_fix("var x := a[1;", "var x := a[1];");
    }

    #[test]
    fn ifs() {
        let initializer = |source: &str| {
            let parse = parse_str(source);
            assert!(parse.errors.is_empty(), "{source}: {:#?}", parse.errors);
            parse
                .tree
                .variables()
                .next()
                .unwrap()
                .initializer()
                .unwrap()
        };
        let Expr::If(chain) = initializer("var x := if a then 1 else if b then 2 else 3;") else {
            panic!("an `if`");
        };
        assert!(matches!(chain.condition(), Some(Expr::Name(_))));
        assert!(matches!(chain.then_branch(), Some(Expr::Literal(_))));
        assert!(matches!(chain.else_branch(), Some(Expr::If(_))));
        // The `else` branch takes all that follows, `where` too.
        let Expr::If(sum) = initializer("var x := if a then 1 else b + 1;") else {
            panic!("an `if`");
        };
        assert!(matches!(sum.else_branch(), Some(Expr::Binary(_))));
        let Expr::If(restricted) = initializer("var x := if a then r else s where p;") else {
            panic!("an `if`");
        };
        assert!(matches!(restricted.else_branch(), Some(Expr::Binary(_))));
        // Every `if` has an `else`, so one in a `then` branch is complete.
        let Expr::If(nested) = initializer("var x := if a then if b then 1 else 2 else 3;") else {
            panic!("an `if`");
        };
        assert!(matches!(nested.then_branch(), Some(Expr::If(_))));
        assert!(matches!(nested.else_branch(), Some(Expr::Literal(_))));
        // Parenthesized, as an operand.
        let Expr::Binary(added) = initializer("var x := (if a then 1 else 2) + 3;") else {
            panic!("a sum");
        };
        assert!(matches!(added.lhs(), Some(Expr::Paren(_))));
        for source in [
            "var x := f{v: if a then 1 else 2};",
            "var x := tuple { a: if b then 1 else 2 };",
            "var x := array [if a then 1 else 2];",
            "var x := r extend { y := if a > 0 then a else -a };",
            "operator f{n: integer}: integer [ if n < 0 then -n else n ];",
        ] {
            let parse = parse_str(source);
            assert!(parse.errors.is_empty(), "{source}: {:#?}", parse.errors);
        }
    }

    #[test]
    fn if_recovery() {
        let one_fix = |source: &str, fixed: &str| {
            let errors = parse_str(source).errors;
            assert_eq!(errors.len(), 1, "{source}: {errors:#?}");
            assert_eq!(fix(source), fixed, "{source}");
        };
        // As an operand, parenthesized where its `else` branch ends.
        one_fix(
            "var x := 1 + if a then 2 else 3 + 4;",
            "var x := 1 + (if a then 2 else 3 + 4);",
        );
        // Python's, C's and Tutorial D's.
        one_fix(
            "var x := \"one\" if n = 1 else \"many\";",
            "var x := if n = 1 then \"one\" else \"many\";",
        );
        one_fix(
            "var x := 1 if a else 2 if b else 3;",
            "var x := if a then 1 else if b then 2 else 3;",
        );
        one_fix(
            "var x := n = 1 ? \"one\" : n = 2 ? \"two\" : \"many\";",
            "var x := if n = 1 then \"one\" else if n = 2 then \"two\" else \"many\";",
        );
        one_fix(
            "var x := (a ? 1 : 2) + 3;",
            "var x := (if a then 1 else 2) + 3;",
        );
        one_fix(
            "var x := if a then 1 elif b then 2 else 3;",
            "var x := if a then 1 else if b then 2 else 3;",
        );
        one_fix(
            "var x := if a then 1 else 2 END IF;",
            "var x := if a then 1 else 2;",
        );
        assert_eq!(
            fix("var x := IF a THEN 1 ELSE 2 END IF;"),
            "var x := if a then 1 else 2;"
        );
        // A name `IF` without `then` after it is a name.
        assert!(parse_str("var x := IF;").errors.is_empty());
        // Without `else`, there is no value to insert.
        let errors = parse_str("var x := if a then 1;").errors;
        assert_eq!(errors.len(), 1, "{errors:#?}");
        assert_eq!(errors[0].code, Code::MissingToken);
        assert!(errors[0].suggestions.is_empty(), "{errors:#?}");
        one_fix("var x := if a 1 else 2;", "var x := if a then 1 else 2;");
    }

    #[test]
    fn if_statements() {
        let statement = |body: &str| {
            let source = format!("operator f{{}} [ {body} ];");
            let parse = parse_str(&source);
            assert!(parse.errors.is_empty(), "{body}: {:#?}", parse.errors);
            let op = parse.tree.operators().next().unwrap();
            let statements: Vec<_> = op.body().unwrap().statements().collect();
            assert_eq!(statements.len(), 1, "{body}");
            match &statements[0] {
                Statement::If(stmt) => stmt.clone(),
                other => panic!("{body}: {other:?}"),
            }
        };
        let plain = statement("if a then [ call f{}; call g{}; ];");
        assert!(matches!(plain.condition(), Some(Expr::Name(_))));
        assert_eq!(plain.then_block().unwrap().statements().count(), 2);
        assert!(plain.else_branch().is_none());
        let chain =
            statement("if a then [ call f{}; ] else if b then [ var x := 1; ] else [ x := 2; ];");
        let Some(trendy_syntax::ast::ElseBranch::If(chained)) = chain.else_branch() else {
            panic!("an `else if`");
        };
        assert!(matches!(
            chained.else_branch(),
            Some(trendy_syntax::ast::ElseBranch::Block(_))
        ));
        // A block can be empty, and hold another `if`.
        statement("if a then [] else [ if b then [ call f{}; ]; ];");
    }

    #[test]
    fn if_statement_recovery() {
        let body = |statements: &str| format!("operator f{{}} [ {statements} ];");
        let one_fix = |source: &str, fixed: &str| {
            let errors = parse_str(&body(source)).errors;
            assert_eq!(errors.len(), 1, "{source}: {errors:#?}");
            assert_eq!(fix(&body(source)), body(fixed), "{source}");
        };
        // Without brackets, the last statement's `;` ends the `if` too.
        assert_eq!(
            fix(&body("if a then call f{}; else call g{};")),
            body("if a then [call f{};] else [call g{};];")
        );
        one_fix("if a then call f{};;", "if a then [call f{};];");
        one_fix("if a then begin call f{}; end;", "if a then [ call f{}; ];");
        one_fix("if a then { call f{}; };", "if a then [ call f{}; ];");
        one_fix(
            "if a then [ call f{}; ] elif b then [ call g{}; ];",
            "if a then [ call f{}; ] else if b then [ call g{}; ];",
        );
        one_fix(
            "if a then [ call f{}; ] end if;",
            "if a then [ call f{}; ];",
        );
        one_fix("if a then [ call f{}; ]", "if a then [ call f{}; ];");
        // `[` is inserted as an index's would be, which the formatter spaces.
        one_fix(
            "if a then else [ call g{}; ];",
            "if a then[] else [ call g{}; ];",
        );
        // Tutorial D's, its keywords in capitals each fixed too.
        assert_eq!(
            fix(&body(
                "IF a THEN BEGIN; CALL f{}; END; ELSE BEGIN; CALL g{}; END; END IF;"
            )),
            body("if a then [ call f{}; ] else [ call g{}; ];")
        );
    }

    #[test]
    fn read_only_bodies_end_with_their_value() {
        let value = |body: &str| {
            let source = format!("operator f{{}}: integer [ {body} ];");
            let parse = parse_str(&source);
            assert!(parse.errors.is_empty(), "{body}: {:#?}", parse.errors);
            let body = parse.tree.operators().next().unwrap().body().unwrap();
            (body.statements().count(), body.value())
        };
        assert!(matches!(value("x * 2"), (0, Some(Expr::Binary(_)))));
        assert!(matches!(value("var y := 2; y"), (1, Some(Expr::Name(_)))));
        // An `if` without blocks is an expression, the value, and a
        // comparison is no assignment.
        assert!(matches!(
            value("if c then 1 else 2"),
            (0, Some(Expr::If(_)))
        ));
        assert!(matches!(value("a = b"), (0, Some(Expr::Binary(_)))));
        // A call is the value, not a `call` without its keyword.
        assert!(matches!(value("f{a: 1}"), (0, Some(Expr::Call(_)))));
        // An `if` statement isn't one, which sema reports.
        assert!(matches!(value("if c then [ x := 1; ]; x"), (1, Some(_))));
    }

    #[test]
    fn value_blocks() {
        let initial = |value: &str| {
            let source = format!("var x := {value};");
            let parse = parse_str(&source);
            (
                parse.tree.variables().next().unwrap().initializer(),
                parse.errors,
            )
        };
        let (block, errors) = initial("[ var a := 1; a := a + 1; a * 2 ]");
        assert!(errors.is_empty(), "{errors:#?}");
        let Some(Expr::Block(block)) = block else {
            panic!("{block:?}");
        };
        assert_eq!(block.statements().count(), 2);
        assert!(matches!(block.value(), Some(Expr::Binary(_))));
        // Without a statement, brackets are an array's, as other languages
        // write one.
        for list in ["[1, 2]", "[x]", "[]"] {
            let (array, errors) = initial(list);
            assert!(matches!(array, Some(Expr::Array(_))), "{list}");
            assert_eq!(errors[0].code, Code::MissingToken, "{list}");
        }
        assert_eq!(
            initial("[ var a := 1; ]").1[0].message,
            "a value block ends with its value"
        );
        // Braces, as Rust and C write a block, and Rust's `let`.
        assert_eq!(
            fix("var x := { let a = 1; a * 2 };"),
            "var x := [ var a := 1; a * 2 ];"
        );
        // An `if` whose branches are value blocks, which runs to the end, is
        // a body's value; with a `;`, it is a statement.
        let source = "operator f{c: boolean}: integer [ if c then [ var a := 1; a ] else [ var b := 2; b ] ];";
        let parse = parse_str(source);
        assert!(parse.errors.is_empty(), "{:#?}", parse.errors);
        let body = parse.tree.operators().next().unwrap().body().unwrap();
        assert!(matches!(body.value(), Some(Expr::If(_))));
    }

    #[test]
    fn body_value_recovery() {
        let body = |statements: &str| format!("operator f{{}}: integer [ {statements} ];");
        // Other languages' `return`, in any case, and a `;` after the value.
        assert_eq!(fix(&body("return x * 2;")), body("x * 2"));
        assert_eq!(fix(&body("var y := 2; RETURN y;")), body("var y := 2; y"));
        assert_eq!(fix(&body("var y := 2; y;")), body("var y := 2; y"));
        assert_eq!(
            messages(&body("x; var y := 2; y")),
            ["only the last item is the value"]
        );
        // `return` before the end, an early exit, parses: sema reports it.
        assert!(messages(&body("return x; var y := 2; y")).is_empty());
    }

    #[test]
    fn loads() {
        let load = |body: &str| {
            let source = format!("operator f{{}} [ {body} ];");
            let parse = parse_str(&source);
            assert!(parse.errors.is_empty(), "{body}: {:#?}", parse.errors);
            let op = parse.tree.operators().next().unwrap();
            let statements: Vec<_> = op.body().unwrap().statements().collect();
            let [Statement::Assign(stmt)] = &statements[..] else {
                panic!("{body}: {statements:?}");
            };
            let clauses: Vec<_> = stmt.clauses().collect();
            let [Clause::Load(load)] = &clauses[..] else {
                panic!("{body}: {clauses:?}");
            };
            (clauses[0].clone(), load.clone())
        };
        let (clause, plain) = load("load a from r where x > 1 order [asc x, desc y];");
        assert_eq!(clause.target().unwrap().syntax().text(), "a");
        assert_eq!(clause.operand().unwrap().syntax().text(), "r where x > 1");
        let items: Vec<_> = plain
            .order()
            .unwrap()
            .items()
            .map(|item| {
                let name = item.attribute().unwrap().ident().unwrap();
                (name.text().to_string(), item.descending())
            })
            .collect();
        assert_eq!(items, [("x".to_string(), false), ("y".to_string(), true)]);
        // The words are contextual keywords: names elsewhere, even in a load,
        // as an attribute ordered by.
        load("load desc from order order [asc desc];");
        let parse = parse_str("operator f{load: integer} [ load := load + 1; ];");
        assert!(parse.errors.is_empty(), "{:#?}", parse.errors);
    }

    #[test]
    fn load_recovery() {
        let body = |statements: &str| format!("operator f{{}} [ {statements} ];");
        // SQL's order, its keywords in capitals, its directions after the
        // attributes, and its nulls; an item without a direction.
        assert_eq!(
            fix(&body("load a from r ORDER BY x DESC nulls last, y;")),
            body("load a from r order [desc x, asc y];")
        );
        assert_eq!(
            fix(&body("load a from r order by [asc x];")),
            body("load a from r order [asc x];")
        );
        assert_eq!(
            fix(&body("load a from r order asc x;")),
            body("load a from r order [asc x];")
        );
        // Tutorial D's, without `from`; LINQ's direction, spelled out.
        assert_eq!(
            fix(&body("LOAD a r ORDER (descending x);")),
            body("load a from r order [desc x];")
        );
        assert_eq!(
            fix(&body("load a from r order {x ascending};")),
            body("load a from r order [asc x];")
        );
        // An ordering as a value, which only a load makes.
        assert_eq!(
            fix(&body("a := r order [asc x];")),
            body("load a from r order [asc x];")
        );
        assert_eq!(messages(&body("load a from r;")), ["a load needs an order"]);
    }

    #[test]
    fn loops() {
        let statement = |body: &str| {
            let source = format!("operator f{{}} [ {body} ];");
            let parse = parse_str(&source);
            assert!(parse.errors.is_empty(), "{body}: {:#?}", parse.errors);
            let op = parse.tree.operators().next().unwrap();
            let statements: Vec<_> = op.body().unwrap().statements().collect();
            assert_eq!(statements.len(), 1, "{body}");
            statements[0].clone()
        };
        let Statement::While(before) = statement("while n > 0 loop [ n := n - 1; call f{}; ];")
        else {
            panic!("a `while`");
        };
        assert!(matches!(before.condition(), Some(Expr::Binary(_))));
        assert_eq!(before.body().unwrap().statements().count(), 2);
        let Statement::Loop(after) = statement("loop [ n := n + 1; ] while n < 3;") else {
            panic!("a `loop`");
        };
        assert!(matches!(after.condition(), Some(Expr::Binary(_))));
        assert_eq!(after.body().unwrap().statements().count(), 1);
        // `break` and `continue` are names, where a statement can start
        // with one.
        let parse = parse_str(
            "operator f{continue: integer} [ var break := continue; break := break + 1; ];",
        );
        assert!(parse.errors.is_empty(), "{:#?}", parse.errors);
    }

    #[test]
    fn for_loops() {
        let source = "operator f{} [ for name in names{x} loop [ call g{}; call h{}; ]; ];";
        let parse = parse_str(source);
        assert!(parse.errors.is_empty(), "{:#?}", parse.errors);
        let op = parse.tree.operators().next().unwrap();
        let statements: Vec<_> = op.body().unwrap().statements().collect();
        let [Statement::For(stmt)] = &statements[..] else {
            panic!("{statements:?}");
        };
        assert_eq!(stmt.variable().unwrap().ident().unwrap().text(), "name");
        assert!(matches!(stmt.array(), Some(Expr::Project(_))));
        assert_eq!(stmt.body().unwrap().statements().count(), 2);
        // `in` is a name elsewhere.
        let parse = parse_str("operator f{in: integer} [ var x := in; ];");
        assert!(parse.errors.is_empty(), "{:#?}", parse.errors);
    }

    #[test]
    fn for_loop_recovery() {
        let body = |statements: &str| format!("operator f{{}} [ {statements} ];");
        // C#'s `foreach`, JavaScript's `of`, a block in braces without
        // `loop`, Lua's `do`.
        assert_eq!(
            fix(&body("foreach x of a { call f{}; };")),
            body("for x in a loop [ call f{}; ];")
        );
        assert_eq!(
            fix(&body("for x in a do call f{};")),
            body("for x in a loop [call f{};];")
        );
        // C's counting loop, and Python's, without fixes.
        assert_eq!(
            messages(&body("for (i = 0; i < n; i := i + 1) { call f{}; };")),
            ["C's `for (…; …; …)` is written as a `while` loop"]
        );
        assert_eq!(
            messages(&body("for x in a: call f{};")),
            ["`for x in a:` is written `for x in a loop [ … ];`"]
        );
    }

    #[test]
    fn loop_recovery() {
        let body = |statements: &str| format!("operator f{{}} [ {statements} ];");
        // C's, without `loop`, its block in braces.
        assert_eq!(
            fix(&body("while (n > 0) { n := n - 1; };")),
            body("while (n > 0) loop [ n := n - 1; ];")
        );
        // Pascal's and Lua's `do`, a statement without brackets.
        assert_eq!(
            fix(&body("while n > 0 do n := n - 1;")),
            body("while n > 0 loop [n := n - 1;];")
        );
        // C's `do … while`, and Pascal's `repeat … until`.
        assert_eq!(
            fix(&body("do { n := n + 1; } while (n < 3);")),
            body("loop [ n := n + 1; ] while (n < 3);")
        );
        assert_eq!(
            fix(&body("repeat [ n := n + 1; ] until n = 3;")),
            body("loop [ n := n + 1; ] while not (n = 3);")
        );
        // Tutorial D's, in capitals.
        assert_eq!(
            fix(&body("WHILE n < 5; n := n + 1; END WHILE;")),
            body("while n < 5 loop [ n := n + 1; ];")
        );
        // No early exits.
        assert_eq!(messages(&body("break;")), ["trendy has no `break`"]);
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
        let Some(Statement::Call(call)) = op.body().unwrap().statements().next() else {
            panic!("a call");
        };
        assert_eq!(
            call.callee().unwrap().ident().unwrap().text(),
            "debug_value"
        );
        assert_eq!(call.arguments().unwrap().named().count(), 1);
    }

    #[test]
    fn read_only_operators_and_calls() {
        let source =
            "operator double{x: integer}: integer [ x * 2 ];\nvar four := double{x: 2} + 0;";
        let parse = parse_str(source);
        assert!(parse.errors.is_empty(), "{:#?}", parse.errors);
        let op = parse.tree.operators().next().unwrap();
        assert!(matches!(op.result(), Some(TypeExpr::Named(_))));
        assert!(op.body().unwrap().statements().next().is_none());
        assert!(matches!(op.body().unwrap().value(), Some(Expr::Binary(_))));
        let initial = |source: &str| {
            let parse = parse_str(&format!("var x := {source};"));
            assert!(parse.errors.is_empty(), "{source}: {:#?}", parse.errors);
            parse
                .tree
                .variables()
                .next()
                .unwrap()
                .initializer()
                .unwrap()
        };
        assert!(matches!(initial("f{a: 1}"), Expr::Call(_)));
        // A value first is a positional argument, which sema reports.
        assert!(matches!(initial("f{1}"), Expr::Call(_)));
        // Postfix operators apply to the call.
        let Expr::Project(project) = initial("g{b: 2}{c}") else {
            panic!("a projection");
        };
        assert!(matches!(project.operand(), Some(Expr::Call(_))));
        // `f{}` and `f{a}` are projections to the parser.
        assert!(matches!(initial("f{}"), Expr::Project(_)));
        assert!(matches!(initial("f{a}"), Expr::Project(_)));
        // Other languages' spellings are fixed.
        assert_eq!(
            fix("operator f{x: integer} -> integer [ RETURN x; ];"),
            "operator f{x: integer}: integer [ x ];"
        );
        assert_eq!(
            fix("operator f{x: integer} returns integer [ x ];"),
            "operator f{x: integer}: integer [ x ];"
        );
        assert_eq!(fix("var x := f(a: 1);"), "var x := f{a: 1};");
        // Tutorial D's `update`, whose values follow the condition in
        // parentheses, isn't a call.
        assert_eq!(
            fix("operator main{} [ update r where c (a := 1); ];"),
            "operator main{} [ update r { a := 1 } where c; ];"
        );
        // Without a value, `return` parses, as an update operator's would:
        // sema reports it.
        assert!(messages("operator f{}: integer [ return; ];").is_empty());
    }

    #[test]
    fn type_parameters() {
        let source = "operator pair{a: T, b: any tuple, r: any relation}: relation { v: T } types { T: any type } [ relation {} ];";
        let parse = parse_str(source);
        assert!(parse.errors.is_empty(), "{:#?}", parse.errors);
        let op = parse.tree.operators().next().unwrap();
        let types = op.type_parameters().unwrap();
        assert_eq!(types.keyword().unwrap().text(), "types");
        let declared: Vec<_> = types.heading().unwrap().attributes().collect();
        assert_eq!(declared.len(), 1);
        let Some(TypeExpr::Any(any)) = declared[0].type_expr() else {
            panic!("`any type`");
        };
        assert_eq!(any.what().unwrap().text(), "type");
        let params: Vec<_> = op.params().unwrap().attributes().collect();
        assert!(matches!(params[0].type_expr(), Some(TypeExpr::Named(_))));
        let Some(TypeExpr::Any(any)) = params[2].type_expr() else {
            panic!("`any relation`");
        };
        assert_eq!(any.what().unwrap().kind(), SyntaxKind::RelationKw);
        // `types` and `any` are words elsewhere.
        let parse = parse_str("var types := 1; var any := types;");
        assert!(parse.errors.is_empty(), "{:#?}", parse.errors);
        // A bare `any` is a type's name to the parser, which sema reports.
        let parse = parse_str("operator f{v: any} [];");
        assert!(parse.errors.is_empty(), "{:#?}", parse.errors);

        // Other languages' spellings are fixed.
        assert_eq!(
            fix("operator same<T, U>{a: T, b: U}: boolean [ true ];"),
            "operator same{a: T, b: U}: boolean types { T: any type, U: any type } [ true ];"
        );
        assert_eq!(
            fix("operator same<T>{a: T, b: T} types { T: any type } [];"),
            "operator same{a: T, b: T} types { T: any type } [];"
        );
        assert_eq!(
            fix("operator f{r: relation any, t: tuple any} [];"),
            "operator f{r: any relation, t: any tuple} [];"
        );
        assert_eq!(
            fix("operator f{v: T} TYPES { T: any type } [];"),
            "operator f{v: T} types { T: any type } [];"
        );
        // A bound has no fix, but is reported once.
        let errors = parse_str("operator max<T: Ord>{a: T}: T [ a ];").errors;
        assert_eq!(errors.len(), 1, "{errors:#?}");
        assert_eq!(errors[0].code, Code::TypeParameterSpelling);
        assert!(errors[0].suggestions.is_empty());
        let errors = parse_str("operator max<T extends Comparable<T>>{a: T}: T [ a ];").errors;
        assert_eq!(errors.len(), 1, "{errors:#?}");
    }

    #[test]
    fn kinds() {
        let parse = parse_str(
            "kind ordered { < };\nbuiltin type text: { ordered };\noperator f{a: any ordered} [];",
        );
        assert!(parse.errors.is_empty(), "{:#?}", parse.errors);
        let items: Vec<_> = parse.tree.items().collect();
        let trendy_syntax::ast::Item::Kind(kind) = &items[0] else {
            panic!("a kind");
        };
        assert_eq!(kind.name().unwrap().ident().unwrap().text(), "ordered");
        let requirements: Vec<_> = kind.requirements().filter_map(|r| r.token()).collect();
        assert_eq!(requirements[0].kind(), SyntaxKind::Lt);
        let trendy_syntax::ast::Item::BuiltinType(text) = &items[1] else {
            panic!("a built-in type");
        };
        assert_eq!(text.kinds().unwrap().names().count(), 1);
        // `kind` is a word elsewhere.
        let parse = parse_str("var kind := 1; var r := relation { { kind: kind } };");
        assert!(parse.errors.is_empty(), "{:#?}", parse.errors);
    }

    #[test]
    fn per_parameters() {
        let parse = parse_str("operator f{r: any relation, of: T per r} types { T: any type } [];");
        assert!(parse.errors.is_empty(), "{:#?}", parse.errors);
        let op = parse.tree.operators().next().unwrap();
        let params: Vec<_> = op.params().unwrap().attributes().collect();
        assert!(params[0].per().is_none());
        let per = params[1].per().unwrap();
        assert_eq!(per.relation().unwrap().ident().unwrap().text(), "r");
        assert!(matches!(params[1].type_expr(), Some(TypeExpr::Named(_))));
        // `per` is a word elsewhere.
        let parse = parse_str("var per := 1; var r := relation { { per: per } };");
        assert!(parse.errors.is_empty(), "{:#?}", parse.errors);
        assert_eq!(
            fix("operator f{r: any relation, of: integer PER r} [];"),
            "operator f{r: any relation, of: integer per r} [];"
        );
    }

    #[test]
    fn positional_arguments_parse_for_sema_to_fix() {
        let parse = parse_str("operator main{} [ call debug_value{r}; ];");
        assert!(parse.errors.is_empty(), "{:#?}", parse.errors);
        let statement = parse
            .tree
            .operators()
            .next()
            .unwrap()
            .body()
            .unwrap()
            .statements()
            .next();
        let Some(Statement::Call(call)) = statement else {
            panic!("a call");
        };
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
        // With type parameters, after the result type.
        let parse = parse_str(
            "builtin operator max{r: any relation, of: T per r}: T types { T: any ordered };",
        );
        assert!(parse.errors.is_empty(), "{:#?}", parse.errors);
        let Some(trendy_syntax::ast::Item::Builtin(max)) = parse.tree.items().next() else {
            panic!("a built-in");
        };
        assert!(max.result().is_some());
        assert_eq!(
            max.type_parameters()
                .and_then(|t| t.heading())
                .map(|h| h.attributes().count()),
            Some(1)
        );
    }

    #[test]
    fn heading_declarations() {
        let parse = parse_str("heading home := { city: text, rooms: integer };");
        assert!(parse.errors.is_empty(), "{:#?}", parse.errors);
        let Some(trendy_syntax::ast::Item::Heading(home)) = parse.tree.items().next() else {
            panic!("a heading's declaration");
        };
        assert_eq!(home.name().and_then(|n| n.ident()).unwrap().text(), "home");
        assert_eq!(home.heading().unwrap().attributes().count(), 2);
        // `heading` is a name elsewhere, such as a ship's.
        assert!(
            parse_str("var heading := relation { { heading: 90 } };")
                .errors
                .is_empty()
        );
        // Its name stands for its attributes after `relation` and `tuple`,
        // in types and in selectors.
        for source in [
            "var r: relation home := relation home {};",
            "var r: relation home key { city };",
            "var t := tuple home { city: \"Lima\", rooms: 3 };",
            "var a: array tuple home := array tuple home [];",
            "var r := relation { tuple home { city: \"Lima\", rooms: 3 } };",
            "operator only{r: relation home}: tuple home [ tuple from r ];",
        ] {
            let parse = parse_str(source);
            assert!(parse.errors.is_empty(), "{source}: {:#?}", parse.errors);
            assert!(
                parse
                    .tree
                    .syntax()
                    .descendants()
                    .any(|n| n.kind() == SyntaxKind::HeadingRef),
                "{source}"
            );
        }
        // A name finds its declaration in the file.
        let parse =
            parse_str("var r: relation home := relation {};\nheading home := { city: text };");
        let named = parse
            .tree
            .syntax()
            .descendants()
            .find_map(trendy_syntax::ast::HeadingRef::cast)
            .unwrap();
        assert_eq!(named.declaration(), parse.tree.heading("home"));
        assert!(named.declaration().is_some());
    }

    #[test]
    fn heading_habits() {
        let fixed = "heading home := { city: text };";
        for source in [
            "heading home = { city: text };",
            "heading home { city: text };",
            "heading home := relation { city: text };",
            "heading home := tuple { city: text };",
            "Heading home := { city: text };",
        ] {
            assert_eq!(fix(source), fixed, "{source}");
        }
        // Other languages' types of records, fixed in one edit.
        for (source, fixed) in [
            (
                "type Home = { city: text };",
                "heading Home := { city: text };",
            ),
            (
                "interface Home { city: text }",
                "heading Home := { city: text };",
            ),
            (
                "struct Home { city: text }\nvar x := 1;",
                "heading Home := { city: text };\nvar x := 1;",
            ),
        ] {
            let parse = parse_str(source);
            assert_eq!(parse.errors.len(), 1, "{source}: {:#?}", parse.errors);
            assert_eq!(parse.errors[0].code, Code::ExpectedDefinition);
            assert_eq!(fix(source), fixed);
        }
        let errors = parse_str("heading home := relation { city: text };").errors;
        assert_eq!(errors[0].code, Code::HeadingAsType);
        // Junk stops before a heading's declaration.
        let parse = parse_str("junk heading home := { city: text };");
        assert_eq!(parse.errors.len(), 1);
        assert_eq!(parse.tree.headings().count(), 1);
        // One in a body moves before its operator, after the one before.
        assert_eq!(
            fix(
                "var x := 1;\n\noperator main{} [\n    heading home := { city: text };\n    var r: relation home;\n];"
            ),
            "var x := 1;\n\nheading home := { city: text };\n\noperator main{} [\n    var r: relation home;\n];"
        );
        assert_eq!(
            fix("operator main{} [\n    heading home := { city: text };\n];"),
            "heading home := { city: text };\n\noperator main{} [\n];"
        );
        // A heading's name as a relation value: the empty relation.
        assert_eq!(fix("var r := relation home;"), "var r := relation home {};");
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
        let error = &parse.errors[0];
        assert_eq!(error.code, Code::ExpressionStatement);
        assert_eq!(error.message, "an expression is not a statement");
        assert_eq!(error.primary.span, Span::new(18, 20));
        // Whether to assign it or write it is a decision, so no fix.
        assert!(error.suggestions.is_empty());
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
    fn what_cant_start_a_statement() {
        let parse = parse_str("operator main{} [ , x; call f{}; ];");
        assert_eq!(parse.errors.len(), 1, "{:#?}", parse.errors);
        assert_eq!(parse.errors[0].message, "expected a statement, found `,`");
        assert!(parse.errors[0].suggestions.is_empty());
        // Not an expression after all: its own error, alone.
        let errors = parse_str("var x := 0;\noperator main{} [ x += 1; ];").errors;
        assert_eq!(errors.len(), 1, "{errors:#?}");
        assert_eq!(errors[0].message, "expected value, found `=`");
        // In a read-only body, a statement is one too.
        let errors = parse_str("operator f{}: integer [ break; 1 ];").errors;
        assert_eq!(errors.len(), 1, "{errors:#?}");
        assert_eq!(errors[0].code, Code::LoopSpelling);
    }

    #[test]
    fn an_operand_after_a_statement_inserts_nothing() {
        // `1 + 2`, `12` or `1`: a `;` alone wouldn't do, so none is
        // inserted, and `2` is skipped, so it is one error.
        let source = "operator main{} [ a := 1 2; call f{}; ];";
        let parse = parse_str(source);
        assert_eq!(parse.errors.len(), 1, "{:#?}", parse.errors);
        let error = &parse.errors[0];
        assert_eq!(error.code, Code::MissingToken);
        assert_eq!(error.message, "expected `;`, found integer literal");
        assert_eq!(error.primary.span, Span::empty(24));
        assert_eq!(error.primary.message, "expected `;` or an operator here");
        assert!(error.suggestions.is_empty());
        let body = parse.tree.operators().next().unwrap().body().unwrap();
        assert_eq!(body.statements().count(), 2);
        // After a call, no operator could go there.
        let errors = parse_str("operator main{} [ call f{} 2; ];").errors;
        assert_eq!(errors.len(), 1, "{errors:#?}");
        assert_eq!(errors[0].primary.message, "expected `;` here");
        // Before another statement, the `;` was forgotten.
        assert_eq!(
            fix("operator main{} [\n    a := 1\n    b := 2;\n];"),
            "operator main{} [\n    a := 1;\n    b := 2;\n];"
        );
    }

    #[test]
    fn local_variables_are_declared_in_bodies() {
        let parse = parse_str(
            "operator f{} [ var n: integer := 1; var r := relation {} key {}; n := 2; ];",
        );
        assert!(parse.errors.is_empty(), "{:#?}", parse.errors);
        let body = parse.tree.operators().next().unwrap().body().unwrap();
        let declared: Vec<String> = body
            .statements()
            .filter_map(|s| match s {
                Statement::Var(decl) => Some(decl.name()?.syntax().text().to_string()),
                _ => None,
            })
            .collect();
        assert_eq!(declared, ["n", "r"]);
        assert_eq!(body.statements().count(), 3);
        // Unindented, in a body no `]` closes before the next definition, a
        // `var` is the program's, before which the `];` is missing.
        let source = "operator f{} [\n    call g{};\nvar y := 1;\noperator main{} [];";
        let parse = parse_str(source);
        assert_eq!(parse.errors.len(), 1, "{:#?}", parse.errors);
        assert_eq!(parse.errors[0].message, "expected `]`, found `var`");
        assert_eq!(parse.tree.variables().count(), 1);
        // Closed, it is the operator's, however it is indented.
        let parse = parse_str("operator f{} [\nvar y := 1;\n];\noperator main{} [];");
        assert!(parse.errors.is_empty(), "{:#?}", parse.errors);
        assert_eq!(parse.tree.variables().count(), 0);
    }

    /// The assignments in `main`'s body, `body`, each as its kind and the
    /// text of its target, keyword and operand, those in an `assign` block
    /// marked so.
    fn clauses(body: &str) -> Vec<String> {
        let source = format!("operator main{{}} [ {body} ];");
        let parse = parse_str(&source);
        assert!(parse.errors.is_empty(), "{body}: {:#?}", parse.errors);
        let text = |expr: Option<trendy_syntax::ast::Expr>| {
            expr.map_or_else(|| "-".to_string(), |e| e.syntax().text().to_string())
        };
        let describe = |clause: trendy_syntax::ast::Clause| {
            format!(
                "{:?} {} {} {}",
                clause.syntax().kind(),
                text(clause.target()),
                clause
                    .keyword()
                    .map_or("-".to_string(), |k| k.text().to_string()),
                text(clause.operand()),
            )
        };
        let body = parse.tree.operators().next().unwrap().body().unwrap();
        body.statements()
            .flat_map(|statement| match statement {
                Statement::Assign(stmt) => stmt.clauses().map(describe).collect(),
                Statement::Block(block) => block
                    .statements()
                    .flat_map(|stmt| stmt.clauses())
                    .map(|clause| format!("assign: {}", describe(clause)))
                    .collect(),
                Statement::Call(_)
                | Statement::Return(_)
                | Statement::Var(_)
                | Statement::If(_)
                | Statement::While(_)
                | Statement::Loop(_)
                | Statement::For(_) => Vec::new(),
            })
            .collect()
    }

    #[test]
    fn assignment_statements() {
        assert_eq!(clauses("n := n + 1;"), ["Assignment n - n + 1"]);
        assert_eq!(
            clauses("insert r s union t; d_insert r s;"),
            ["Insertion r insert s union t", "Insertion r d_insert s"]
        );
        // The condition is all that follows `where`.
        assert_eq!(
            clauses("delete r where a = 1 and b; i_delete r s;"),
            ["Deletion r delete a = 1 and b", "Deletion r i_delete s"]
        );
        assert_eq!(
            clauses("update r { a := a + 1 } where b = 2;"),
            ["Update r update b = 2"]
        );
        assert_eq!(clauses("update t { a := 1 };"), ["Update t update -"]);
        // An element, at any depth, and an attribute, which sema reports.
        assert_eq!(
            clauses("a[i + 1] := 2; g[1][j] := x; t.name := y; a[1].b := z;"),
            [
                "Assignment a[i + 1] - 2",
                "Assignment g[1][j] - x",
                "Assignment t.name - y",
                "Assignment a[1].b - z"
            ]
        );
        let main = |body: &str| format!("operator main{{}} [ {body} ];");
        assert_eq!(fix(&main("a[1] = 2;")), main("a[1] := 2;"));
    }

    #[test]
    fn assign_blocks() {
        assert_eq!(
            clauses("assign { a := b; b := a; }; n := 1;"),
            [
                "assign: Assignment a - b",
                "assign: Assignment b - a",
                "Assignment n - 1"
            ]
        );
        assert_eq!(
            clauses("assign { update r { a := 1 }; delete r where b; };"),
            ["assign: Update r update -", "assign: Deletion r delete b"]
        );
        assert_eq!(clauses("assign {};"), Vec::<String>::new());
        // Only assignments go in a block.
        let main = |body: &str| format!("operator main{{}} [ {body} ];");
        assert_eq!(
            messages(&main("assign { call f{}; a := 1; };")),
            ["expected identifier, found `call`"]
        );
        assert_eq!(
            messages(&main("assign { assign { a := 1; }; };")),
            ["expected identifier, found `assign`"]
        );
    }

    #[test]
    fn assignment_mistakes() {
        let main = |body: &str| format!("operator main{{}} [ {body} ];");
        // C's `=`, and Tutorial D's capitals.
        assert_eq!(fix(&main("n = n + 1;")), main("n := n + 1;"));
        assert_eq!(
            messages(&main("n = 1;")),
            ["expected `:=` in an assignment, found `=`"]
        );
        assert_eq!(fix(&main("INSERT r s;")), main("insert r s;"));
        // TTM's multiple assignment, with commas.
        assert_eq!(
            fix(&main("a := b, b := a;")),
            main("assign { a := b; b := a; };")
        );
        assert_eq!(
            fix(&main("assign { a := b, b := a; };")),
            main("assign { a := b; b := a; };")
        );
        // SQL's words.
        assert_eq!(
            fix(&main("insert into r s; delete from r where a;")),
            main("insert r s; delete r where a;")
        );
        assert_eq!(
            fix(&main("update r set a = 1, b = a where c;")),
            main("update r { a := 1, b := a } where c;")
        );
        assert_eq!(
            fix(&main("assign { update r set a = 1; n := 2; };")),
            main("assign { update r { a := 1 }; n := 2; };")
        );
        // Tutorial D's order.
        assert_eq!(
            fix(&main("update r where c (a := 1);")),
            main("update r { a := 1 } where c;")
        );
        assert_eq!(fix(&main("update r [a := 1];")), main("update r {a := 1};"));
        // A `delete` needs a condition.
        assert_eq!(
            messages(&main("delete r;")),
            ["`delete` needs `where` and a condition"]
        );
        assert_eq!(fix(&main("delete r;")), main("r := relation {};"));
        assert_eq!(fix(&main("delete from r;")), main("r := relation {};"));
        assert_eq!(fix(&main("delete r s;")), main("r := r minus s;"));
        assert_eq!(
            fix(&main("delete r s union t;")),
            main("r := r minus (s union t);")
        );
        // A keyword can't be assigned.
        assert_eq!(
            messages(&main("join := 1;")),
            ["`join` is a keyword, so it can't be a name"]
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
                Expr::From(f) => format!(
                    "({} from {})",
                    match f.taken() {
                        trendy_syntax::ast::Taken::Tuple => "tuple",
                        trendy_syntax::ast::Taken::Value => "value",
                    },
                    show(&f.operand().unwrap())
                ),
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
    fn set_operators() {
        // `union` binds more loosely than the joins, `intersect` and `minus`
        // like them.
        assert_eq!(shape("r union s join t"), "(r union (s join t))");
        assert_eq!(shape("r intersect s join t"), "((r intersect s) join t)");
        assert_eq!(shape("r minus s union t"), "((r minus s) union t)");
        // Lists: braces for the commutative ones, brackets for `minus`.
        for source in [
            "var x := union{r, s, t};",
            "var x := intersect{r, s};",
            "var x := minus[r, s, t];",
        ] {
            assert!(parse_str(source).errors.is_empty(), "{source}");
        }
        assert_eq!(fix("var x := minus{r, s};"), "var x := minus[r, s];");
        assert_eq!(fix("var x := union[r, s];"), "var x := union{r, s};");
        // SQL's spellings.
        assert_eq!(fix("var x := r EXCEPT s;"), "var x := r minus s;");
        assert_eq!(fix("var x := r union all s;"), "var x := r union s;");
        assert_eq!(fix("var x := r UNION s;"), "var x := r union s;");
        // `r union s minus t` reads two ways.
        let source = "var x := r union s minus t;";
        assert_eq!(
            messages(source),
            ["`union` and `minus` need parentheses to say which applies first"]
        );
        assert_eq!(fix(source), "var x := (r union s) minus t;");
        // `xunion` binds and lists as `union` does.
        assert_eq!(shape("r xunion s join t"), "(r xunion (s join t))");
        assert!(parse_str("var x := xunion{r, s, t};").errors.is_empty());
        assert_eq!(fix("var x := xunion[r, s];"), "var x := xunion{r, s};");
        assert_eq!(
            messages("var x := r xunion s minus t;"),
            ["`xunion` and `minus` need parentheses to say which applies first"]
        );
        // The checked ones bind and list as their plain forms do.
        assert_eq!(shape("r d_union s join t"), "(r d_union (s join t))");
        assert_eq!(shape("r i_minus s join t"), "((r i_minus s) join t)");
        assert_eq!(fix("var x := i_minus{r, s};"), "var x := i_minus[r, s];");
        assert!(parse_str("var x := d_union{r, s, t};").errors.is_empty());
        assert_eq!(fix("var x := r D_UNION s;"), "var x := r d_union s;");
        assert_eq!(
            messages("var x := r d_union s i_minus t;"),
            ["`d_union` and `i_minus` need parentheses to say which applies first"]
        );
        // `!!` before a set operator, too.
        assert_eq!(
            messages("var x := t extend { y := !!r union s };"),
            ["`!!` and `union` need parentheses to say which applies first"]
        );
    }

    #[test]
    fn matchings() {
        // With the joins' precedence; `not matching` is one operator.
        assert_eq!(shape("r matching s join t"), "((r matching s) join t)");
        let parse = parse_str("var x := r not matching s where a;");
        assert!(parse.errors.is_empty(), "{:#?}", parse.errors);
        // Tutorial D's capitals, and the literature's names.
        assert_eq!(
            fix("var x := r NOT MATCHING s;"),
            "var x := r not matching s;"
        );
        assert_eq!(fix("var x := r semijoin s;"), "var x := r matching s;");
        assert_eq!(fix("var x := r SemiMinus s;"), "var x := r not matching s;");
        assert_eq!(fix("var x := r antijoin s;"), "var x := r not matching s;");
        // No list form: `matching` isn't commutative.
        assert!(!parse_str("var x := matching{r, s};").errors.is_empty());
        // A prefix `not` is still a prefix.
        assert!(parse_str("var x := not a;").errors.is_empty());
    }

    #[test]
    fn restrictions() {
        // The loosest operator: the condition is all that follows, and a
        // join before it is restricted.
        assert_eq!(
            shape("r join s where a = 1 and b or c"),
            "((r join s) where (((a = 1) and b) or c))"
        );
        assert_eq!(shape("r where a where b"), "((r where a) where b)");
        assert_eq!(fix("var x := r WHERE a = 1;"), "var x := r where a = 1;");
    }

    #[test]
    fn taken_relations() {
        // A relation's expression: its relational operators, and a `where`
        // with all of its condition.
        assert_eq!(
            shape("tuple from r union s join t where a = b + 1 and c"),
            "(tuple from ((r union (s join t)) where ((a = (b + 1)) and c)))"
        );
        // Not a comparison after it, or arithmetic, which no relation has.
        assert_eq!(shape("tuple from r = t"), "((tuple from r) = t)");
        assert_eq!(shape("value from r{a} + 1"), "((value from r{a}) + 1)");
        assert_eq!(
            shape("value from r{a} * 2 + value from s{a}"),
            "(((value from r{a}) * 2) + (value from s{a}))"
        );
        assert_eq!(
            shape("value from r join s{a} || \"!\""),
            "((value from (r join s{a})) || \"!\")"
        );
        assert_eq!(
            shape("a = value from (r where b = 1){c} - 1"),
            "(a = ((value from (r where b = 1){c}) - 1))"
        );
    }

    #[test]
    fn taken() {
        for source in [
            "var x := tuple from r;",
            "var x := value from r{a};",
            "var x := relation { tuple from r, { a: 1 } };",
            // `value` and `from` are names elsewhere.
            "var value := relation { { from: 1 } };",
        ] {
            assert!(parse_str(source).errors.is_empty(), "{source}");
        }
        // A prefix operator: postfix operators bind tighter.
        let parse = parse_str("var x := value from r{a};");
        let from = parse
            .tree
            .syntax()
            .descendants()
            .find_map(trendy_syntax::ast::FromExpr::cast)
            .unwrap();
        assert_eq!(from.operand().unwrap().syntax().to_string(), "r{a}");
        assert_eq!(from.taken(), trendy_syntax::ast::Taken::Value);
        // Tutorial D's capitals.
        assert_eq!(fix("var x := TUPLE FROM r;"), "var x := tuple from r;");
        assert_eq!(fix("var x := Value From r;"), "var x := value from r;");
    }

    #[test]
    fn groupings() {
        let parse = parse_str("var x := r group { g := {a, b}, h := {c} };");
        assert!(parse.errors.is_empty(), "{:#?}", parse.errors);
        let groupings: Vec<_> = parse
            .tree
            .syntax()
            .descendants()
            .filter_map(trendy_syntax::ast::Gathering::cast)
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
        // A missing `:=` is inserted, spaced.
        assert_eq!(
            fix("var x := r group { g {a} };"),
            "var x := r group { g := {a} };"
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
    fn ungroupings() {
        let parse = parse_str("var x := r ungroup {g, h};");
        assert!(parse.errors.is_empty(), "{:#?}", parse.errors);
        // Tutorial D's parentheses and capitals, and a bare name.
        assert_eq!(fix("var x := r UNGROUP (g);"), "var x := r ungroup {g};");
        assert_eq!(fix("var x := r ungroup g;"), "var x := r ungroup {g};");
        assert_eq!(
            messages("var x := r ungroup g;"),
            ["`ungroup` takes its attributes in braces: `{g}`"]
        );
    }

    #[test]
    fn wrappings() {
        let parse = parse_str("var x := r wrap { w := {a, b} } unwrap {w};");
        assert!(parse.errors.is_empty(), "{:#?}", parse.errors);
        // Tutorial D's `AS`, parentheses and capitals, and a bare name.
        assert_eq!(
            fix("var x := r WRAP ({a, b} AS w);"),
            "var x := r wrap {w := {a, b}};"
        );
        assert_eq!(fix("var x := r UNWRAP (w);"), "var x := r unwrap {w};");
        assert_eq!(fix("var x := r unwrap w;"), "var x := r unwrap {w};");
        assert_eq!(
            messages("var x := r wrap { w: {a} };"),
            ["expected `:=` after the new attribute's name, found `:`"]
        );
        assert_eq!(
            messages("var x := r wrap { {a} };"),
            ["a wrapping needs a name"]
        );
        // SQL's `by` belongs to `group` only.
        assert_eq!(
            messages("var x := r wrap by {a};")[0],
            "expected `{`, found identifier `by`"
        );
        assert_eq!(
            fix("var x := r wrap [v := {a}, w := {v, b}];"),
            "var x := r wrap { v := {a} } wrap { w := {v, b} };"
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
