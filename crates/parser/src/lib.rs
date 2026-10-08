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
                SyntaxKind::VarKw => self.var_decl(),
                _ => self.junk(found),
            }
        }
    }

    fn operator(&mut self) {
        self.form = errors::OPERATOR_FORM;
        self.builder.start_node(SyntaxKind::Operator);
        self.bump();
        self.name("add a name after `operator`, such as `operator main{} [];`");

        self.delimited(
            SyntaxKind::ParamList,
            (SyntaxKind::LBrace, SyntaxKind::RBrace),
            &[(SyntaxKind::LParen, SyntaxKind::RParen)],
        );
        self.delimited(
            SyntaxKind::Body,
            (SyntaxKind::LBracket, SyntaxKind::RBracket),
            &[
                (SyntaxKind::LBrace, SyntaxKind::RBrace),
                (SyntaxKind::LParen, SyntaxKind::RParen),
            ],
        );
        self.expect(SyntaxKind::Semicolon);
        self.builder.finish_node();
    }

    /// `var name: type := value;`, where either the type or the value may be
    /// left out. Sema checks that at least one is there.
    fn var_decl(&mut self) {
        self.form = errors::VARIABLE_FORM;
        self.builder.start_node(SyntaxKind::VarDecl);
        self.bump();
        self.name("add a name after `var`, such as `var count: integer := 0;`");

        let starts_type = |k| matches!(k, SyntaxKind::Ident | SyntaxKind::TupleKw);
        if self.at(SyntaxKind::Colon) || self.peek().is_some_and(starts_type) {
            self.builder.start_node(SyntaxKind::TypeAnnotation);
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
                self.builder.start_node(SyntaxKind::Initializer);
                let span = self.bump();
                if found.kind == SyntaxKind::Eq {
                    self.errors.push(errors::assignment_with_eq(span));
                }
                self.expression();
                self.builder.finish_node();
            }
            Some(found) if found.kind.is_literal() || found.kind == SyntaxKind::TupleKw => {
                // `var i: integer 0`: the value is there but `:=` isn't.
                self.builder.start_node(SyntaxKind::Initializer);
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
    fn name(&mut self, help: &'static str) {
        if self.at(SyntaxKind::Ident) {
            self.builder.start_node(SyntaxKind::Name);
            self.bump();
            self.builder.finish_node();
        } else {
            self.missing_with_help(SyntaxKind::Ident, help);
        }
    }

    /// A named type, or a tuple type.
    fn type_expr(&mut self) {
        if self.at_tuple() {
            self.tuple(
                SyntaxKind::TupleType,
                SyntaxKind::AttributeType,
                Self::type_expr,
            );
        } else if self.at(SyntaxKind::Ident) {
            self.builder.start_node(SyntaxKind::TypeRef);
            self.bump();
            self.builder.finish_node();
        } else {
            self.missing_with_help(
                SyntaxKind::TypeRef,
                "write a type, such as `integer`, `text` or `tuple { name: text }`",
            );
        }
    }

    /// A literal, or a tuple value.
    fn expression(&mut self) {
        if self.peek().is_some_and(SyntaxKind::is_literal) {
            self.builder.start_node(SyntaxKind::Literal);
            self.bump();
            self.builder.finish_node();
        } else if self.at_tuple() {
            self.tuple(
                SyntaxKind::TupleLiteral,
                SyntaxKind::AttributeValue,
                Self::expression,
            );
        } else if let Some(found) = self
            .found()
            .filter(|f| f.kind == SyntaxKind::Ident && errors::boolean_spelling(f.text).is_some())
        {
            // `True` or `TRUE`: a boolean literal in the wrong case. It's kept
            // as the literal it was meant to be, so nothing else cascades.
            self.builder.start_node(SyntaxKind::Literal);
            self.bump();
            self.builder.finish_node();
            self.errors.push(errors::miscased_keyword(
                found,
                errors::boolean_spelling(found.text).expect("just checked"),
            ));
        } else {
            self.missing_with_help(
                SyntaxKind::Literal,
                "write a value, such as `0`, `\"\"` or `tuple { name: \"x\" }`",
            );
        }
    }

    /// Whether a tuple type or value starts here: `tuple {`, a mis-cased
    /// `TUPLE {`, or a bare `{`, as in a JSON or JavaScript object.
    fn at_tuple(&mut self) -> bool {
        let Some(found) = self.found() else {
            return false;
        };
        let brace_next = self
            .found_after(1)
            .is_some_and(|f| f.kind == SyntaxKind::LBrace);
        match found.kind {
            SyntaxKind::TupleKw | SyntaxKind::LBrace => true,
            SyntaxKind::Ident => brace_next && found.text.eq_ignore_ascii_case("tuple"),
            _ => false,
        }
    }

    /// `tuple { item, … }`, where an item is `name: x` and `x` is parsed by
    /// `item`. `node` and `attribute` are the node kinds for the whole and
    /// for each item. A trailing comma is allowed.
    fn tuple(&mut self, node: SyntaxKind, attribute: SyntaxKind, item: fn(&mut Self)) {
        let outer_form = std::mem::replace(
            &mut self.form,
            if node == SyntaxKind::TupleType {
                errors::TUPLE_TYPE_FORM
            } else {
                errors::TUPLE_VALUE_FORM
            },
        );
        self.builder.start_node(node);
        let found = self.found().expect("at_tuple saw a token");
        match found.kind {
            SyntaxKind::TupleKw => {
                self.bump();
            }
            SyntaxKind::Ident => {
                // `TUPLE` or `Tuple`.
                self.bump();
                self.errors.push(errors::miscased_keyword(found, "tuple"));
            }
            _ => self.missing(SyntaxKind::TupleKw),
        }

        if self.at(SyntaxKind::LBrace) {
            self.bump();
            while self.at(SyntaxKind::Ident) {
                self.attribute(attribute, item);
                match self.peek() {
                    Some(SyntaxKind::Comma) => {
                        self.bump();
                    }
                    // `tuple { a: 1 b: 2 }`: the next attribute is there but
                    // the comma isn't.
                    Some(SyntaxKind::Ident) => self.missing(SyntaxKind::Comma),
                    _ => break,
                }
            }
            self.expect(SyntaxKind::RBrace);
        } else {
            self.missing(SyntaxKind::LBrace);
            self.missing(SyntaxKind::RBrace);
        }
        self.builder.finish_node();
        self.form = outer_form;
    }

    /// `name: x`, with `x` parsed by `item`.
    fn attribute(&mut self, attribute: SyntaxKind, item: fn(&mut Self)) {
        self.builder.start_node(attribute);
        self.builder.start_node(SyntaxKind::Name);
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

    /// A `node` made of a `delims` pair with nothing between.
    ///
    /// A pair in `mistakes` is parsed as if it were `delims` and reported
    /// with a fix. Only pairs that can't be the start of what comes next are
    /// listed, so that a missing node isn't mistaken for a misspelled one.
    fn delimited(
        &mut self,
        node: SyntaxKind,
        delims: (SyntaxKind, SyntaxKind),
        mistakes: &[(SyntaxKind, SyntaxKind)],
    ) {
        let (open, close) = delims;
        if self.at(open) {
            self.builder.start_node(node);
            self.bump();
            self.expect(close);
            self.builder.finish_node();
            return;
        }

        let Some(&(wrong_open, wrong_close)) = mistakes.iter().find(|(o, _)| self.at(*o)) else {
            self.missing(open);
            self.missing(close);
            return;
        };
        self.builder.start_node(node);
        let open_span = self.bump();
        let close_span = if self.at(wrong_close) {
            Some(self.bump())
        } else {
            None
        };
        self.builder.finish_node();
        self.errors.push(errors::wrong_delimiter(
            node,
            delims,
            (wrong_open, wrong_close),
            open_span,
            close_span,
        ));
        if close_span.is_none() {
            self.missing(close);
        }
    }

    /// Wrap tokens that don't start a definition in an error node, up to the
    /// next definition keyword or the end of input. `first` is the next token.
    fn junk(&mut self, first: Found<'a>) {
        let name = self.found_after(1);
        let after_name = self.found_after(2);
        self.builder.start_node(SyntaxKind::Error);
        while self
            .peek()
            .is_some_and(|k| !matches!(k, SyntaxKind::OperatorKw | SyntaxKind::VarKw))
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
        self.errors.push(errors::missing(
            &missing.expected,
            found,
            Span::empty(at),
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
    ParamList
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
