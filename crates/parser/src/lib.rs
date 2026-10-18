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
        paren_calls: true,
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
    /// Whether a name followed by `(` is a call, as other languages write
    /// one: everywhere but in Tutorial D's `UPDATE R WHERE p ( … )`, whose
    /// condition the parenthesized new values follow.
    paren_calls: bool,
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

/// The precedence of `where`, the loosest operator: its condition is a
/// whole boolean expression.
const WHERE: u8 = 0;
/// The precedence of the comparison operators.
const COMPARISON: u8 = 6;
/// The precedence of `union`, Algebra A's OR, looser than the AND of the
/// joins, as `or` is than `and`.
const UNION: u8 = 7;
/// The precedence of the joins, `join`, `times`, `compose` and the
/// matchings, and of `intersect` and `minus`, which are AND too.
const JOIN: u8 = 8;

/// A dyadic operator at the next token: its precedence, its first token,
/// and the keyword it is or stands for.
#[derive(Clone, Copy)]
struct BinaryOperator<'a> {
    precedence: u8,
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
                SyntaxKind::Ident if self.at_kind() => self.kind_decl(),
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
        self.result_type();
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
            Self::statements,
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
    /// if it has one. Other languages' spellings, `-> type` and Tutorial
    /// D's `RETURNS type`, are parsed as it and fixed.
    fn result_type(&mut self) {
        let Some(found) = self.found() else {
            return;
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
            _ => return,
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
                    | SyntaxKind::BuiltinKw
            );
            if ends_body || self.at_unclosed_declaration(&found) {
                break;
            }
            self.statement();
        }
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

    /// A statement: a call, an assignment statement, a local variable's
    /// declaration, or a `return`.
    fn statement(&mut self) {
        let Some(found) = self.found() else {
            return;
        };
        let call_like = self
            .found_after(1)
            .is_some_and(|f| matches!(f.kind, SyntaxKind::LBrace | SyntaxKind::LParen));
        if found.kind == SyntaxKind::CallKw {
            self.call_stmt();
        } else if found.kind == SyntaxKind::VarKw {
            self.var_decl();
        } else if found.kind == SyntaxKind::AssignKw {
            self.assign_block();
        } else if self.at_assignment() {
            self.assign_stmt(false);
        } else if found.kind == SyntaxKind::ReturnKw
            || (found.kind == SyntaxKind::Ident && found.text.eq_ignore_ascii_case("return"))
        {
            self.return_stmt();
        } else if found.kind == SyntaxKind::Ident && call_like {
            // `debug_value{v: r};`: a call without `call`.
            self.errors.push(errors::missing_call(found));
            self.call_stmt();
        } else {
            self.missing_with_help(
                SyntaxKind::CallKw,
                "statements are calls, such as `call debug_value{v: 1};`, assignments, such as `n := n + 1;`, local variables, such as `var n := 0;`, and, in a read-only operator, `return value;`",
            );
            self.skip_statement(&[
                SyntaxKind::RBracket,
                SyntaxKind::OperatorKw,
                SyntaxKind::VarKw,
            ]);
        }
    }

    /// Whether an assignment, or a shorthand for one, starts here. A
    /// keyword before `:=` is one too, reported as a name that can't be one.
    fn at_assignment(&mut self) -> bool {
        let Some(found) = self.found() else {
            return false;
        };
        let assigned = (found.kind == SyntaxKind::Ident || found.kind.is_keyword())
            && self
                .found_after(1)
                .is_some_and(|f| matches!(f.kind, SyntaxKind::ColonEq | SyntaxKind::Eq));
        assigned || self.at_clause_keyword().is_some()
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

    /// `return value;`, with `return` in any case, as Tutorial D's
    /// `RETURN`, which is fixed.
    fn return_stmt(&mut self) {
        self.start_node(SyntaxKind::ReturnStmt);
        let found = self.found().expect("at `return`");
        self.bump();
        if found.kind != SyntaxKind::ReturnKw {
            self.errors.push(errors::miscased_keyword(found, "return"));
        }
        if self.at_operand() {
            self.expression();
        } else {
            self.missing_with_help(
                SyntaxKind::Literal,
                "write the value to return after `return`, as in `return x + 1;`",
            );
        }
        self.expect(SyntaxKind::Semicolon);
        self.builder.finish_node();
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
        self.expect(SyntaxKind::Semicolon);
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
                    self.binary(WHERE + 1);
                    self.paren_calls = outer;
                    if !values_first {
                        self.values_after_where(where_span);
                    }
                }
            }
            None => {
                self.start_node(SyntaxKind::Assignment);
                self.target();
                match self.found() {
                    Some(found) if found.kind == SyntaxKind::Eq => {
                        let span = self.bump();
                        self.errors
                            .push(errors::assignment_with_eq(span, "in an assignment"));
                    }
                    _ => self.expect(SyntaxKind::ColonEq),
                }
                self.expression();
            }
        }
        self.builder.finish_node();
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
            self.binary(WHERE + 1);
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
            self.assign(errors::Assigning::Update, WHERE + 1);
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
        self.items(Self::at_operand, |p| p.assign(errors::Assigning::Update, 0));
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
        self.expect(SyntaxKind::Semicolon);
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
            self.heading();
            self.builder.finish_node();
        } else if self.at_keyword(SyntaxKind::RelationKw) {
            self.start_node(SyntaxKind::RelationType);
            self.keyword(SyntaxKind::RelationKw);
            self.heading();
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
    fn expression(&mut self) {
        self.binary(0);
    }

    /// An expression whose dyadic operators bind at least as tightly as
    /// `min`, by precedence climbing. From loosest to tightest: `where` (0),
    /// whose condition is all that follows, `implies` (1, grouping to the
    /// right), `or` (2), `xor` (3), `and` (4), the comparisons (6), which
    /// don't chain, `union` (7), the joins, `intersect` and `minus` (8),
    /// `+`, `-` and `||` (9), and `*`, `/`, `div` and `mod` (10), all
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
                words,
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
                words: Words::Sql(tokens),
            });
        }
        if found.kind == SyntaxKind::Ident
            && let Some((keyword, spelled)) = errors::operator_name(found.text)
        {
            return Some(BinaryOperator {
                precedence: if keyword == SyntaxKind::UnionKw {
                    UNION
                } else {
                    JOIN
                },
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
        let matching = |f: Found| {
            f.kind == SyntaxKind::MatchingKw
                || (f.kind == SyntaxKind::Ident && f.text.eq_ignore_ascii_case("matching"))
        };
        if keyword == SyntaxKind::NotKw && self.lookahead_nth(1).is_some_and(matching) {
            return Some(BinaryOperator {
                precedence: JOIN,
                found,
                keyword: SyntaxKind::MatchingKw,
                words: Words::NotMatching,
            });
        }
        let precedence = match keyword {
            SyntaxKind::WhereKw => WHERE,
            SyntaxKind::ImpliesKw => 1,
            SyntaxKind::OrKw => 2,
            SyntaxKind::XorKw => 3,
            SyntaxKind::AndKw => 4,
            kind if kind.is_comparison() => COMPARISON,
            SyntaxKind::UnionKw | SyntaxKind::DUnionKw | SyntaxKind::XUnionKw => UNION,
            kind if kind.is_join() => JOIN,
            SyntaxKind::IntersectKw | SyntaxKind::MinusKw | SyntaxKind::IMinusKw => JOIN,
            SyntaxKind::Plus | SyntaxKind::Minus | SyntaxKind::PipePipe => 9,
            SyntaxKind::Star | SyntaxKind::Slash | SyntaxKind::DivKw | SyntaxKind::ModKw => 10,
            _ => return None,
        };
        Some(BinaryOperator {
            precedence,
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
        match first.kind {
            SyntaxKind::TupleKw => Some(trendy_syntax::ast::Taken::Tuple),
            SyntaxKind::Ident if first.text.eq_ignore_ascii_case("tuple") => {
                Some(trendy_syntax::ast::Taken::Tuple)
            }
            SyntaxKind::Ident if first.text.eq_ignore_ascii_case("value") => {
                Some(trendy_syntax::ast::Taken::Value)
            }
            _ => None,
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
    /// operators down to the joins, and a `where` with its condition. So
    /// `tuple from r where p` is of `r where p`, while
    /// `(tuple from r) = t` compares, as a comparison binds more loosely.
    fn relation_expression(&mut self) {
        self.peek();
        let start = self.builder.checkpoint();
        self.binary(UNION);
        if let Some(operator) = self
            .binary_operator()
            .filter(|o| o.keyword == SyntaxKind::WhereKw)
        {
            self.builder.start_node_at(start, SyntaxKind::BinaryExpr);
            self.operator_token(operator.found, operator.keyword);
            self.binary(WHERE + 1);
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
            } else if next == Some(SyntaxKind::LBrace) || method_call {
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
            move |p| p.items(Self::at_operand, move |p| p.assign(assigning, 0)),
        );
        self.form = outer_form;
    }

    /// `a := x`. `a: x` and `a = x`, as a tuple and as keyword arguments
    /// are written, are fixed to `a := x`, and `x as a`, as SQL and Tutorial
    /// D's `ADD` write it, to `a := x`. The value's operators bind at least
    /// as tightly as `min`: after SQL's `set`, a `where` ends it.
    fn assign(&mut self, assigning: errors::Assigning, min: u8) {
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
            self.binary(min);
        } else {
            let start = self.found().map_or(0, |f| f.span.start);
            self.binary(min);
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
    use trendy_syntax::ast::{AstNode, Expr, Statement, TypeExpr};
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
            "operator f{a: any array, b: array T}: T types { T: any type } [ return b[1]; ];",
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
        let source = "operator double{x: integer}: integer [ return x * 2; ];\nvar four := double{x: 2} + 0;";
        let parse = parse_str(source);
        assert!(parse.errors.is_empty(), "{:#?}", parse.errors);
        let op = parse.tree.operators().next().unwrap();
        assert!(matches!(op.result(), Some(TypeExpr::Named(_))));
        let Some(Statement::Return(stmt)) = op.body().unwrap().statements().next() else {
            panic!("a return");
        };
        assert!(stmt.value().is_some());
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
            "operator f{x: integer}: integer [ return x; ];"
        );
        assert_eq!(
            fix("operator f{x: integer} returns integer [ return x; ];"),
            "operator f{x: integer}: integer [ return x; ];"
        );
        assert_eq!(fix("var x := f(a: 1);"), "var x := f{a: 1};");
        // Tutorial D's `update`, whose values follow the condition in
        // parentheses, isn't a call.
        assert_eq!(
            fix("operator main{} [ update r where c (a := 1); ];"),
            "operator main{} [ update r { a := 1 } where c; ];"
        );
        assert_eq!(
            messages("operator f{}: integer [ return; ];"),
            ["expected value, found `;`"]
        );
    }

    #[test]
    fn type_parameters() {
        let source = "operator pair{a: T, b: any tuple, r: any relation}: relation { v: T } types { T: any type } [ return relation {}; ];";
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
            fix("operator same<T, U>{a: T, b: U}: boolean [ return true; ];"),
            "operator same{a: T, b: U}: boolean types { T: any type, U: any type } [ return true; ];"
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
        let errors = parse_str("operator max<T: Ord>{a: T}: T [ return a; ];").errors;
        assert_eq!(errors.len(), 1, "{errors:#?}");
        assert_eq!(errors[0].code, Code::TypeParameterSpelling);
        assert!(errors[0].suggestions.is_empty());
        let errors =
            parse_str("operator max<T extends Comparable<T>>{a: T}: T [ return a; ];").errors;
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
                Statement::Call(_) | Statement::Return(_) | Statement::Var(_) => Vec::new(),
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
        // `tuple from` takes a relation's expression, `where` included,
        // but not a comparison after it.
        let from = |source: &str| {
            parse_str(source)
                .tree
                .syntax()
                .descendants()
                .find_map(trendy_syntax::ast::FromExpr::cast)
                .unwrap()
                .operand()
                .unwrap()
                .syntax()
                .to_string()
        };
        assert_eq!(
            from("var x := tuple from r join s where a = 1 and b;"),
            "r join s where a = 1 and b"
        );
        assert_eq!(from("var x := tuple from r = t;"), "r");
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
