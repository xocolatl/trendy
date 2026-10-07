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
            if found.kind == SyntaxKind::OperatorKw {
                self.operator();
            } else {
                self.junk(found);
            }
        }
    }

    fn operator(&mut self) {
        self.builder.start_node(SyntaxKind::Operator);
        self.bump();

        if self.at(SyntaxKind::Ident) {
            self.builder.start_node(SyntaxKind::Name);
            self.bump();
            self.builder.finish_node();
        } else {
            self.missing(SyntaxKind::Ident);
        }

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
    /// next `operator` keyword or the end of input. `first` is the next token.
    fn junk(&mut self, first: Found<'a>) {
        let next = self.found_after(1);
        self.builder.start_node(SyntaxKind::Error);
        while self.peek().is_some_and(|k| k != SyntaxKind::OperatorKw) {
            self.bump();
        }
        self.builder.finish_node();
        let end = self.last_end.unwrap_or(first.span.end);
        self.errors.push(errors::expected_definition(
            first,
            next,
            Span::new(first.span.start, end),
        ));
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
                });
            }
        }
    }

    fn flush_missing(&mut self) {
        let Some(missing) = self.missing.take() else {
            return;
        };
        let found = (missing.pos < self.tokens.len()).then(|| self.found_at(missing.pos));
        let at = self.last_end.or(found.map(|f| f.span.start)).unwrap_or(0);
        self.errors
            .push(errors::missing(&missing.expected, found, Span::empty(at)));
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
            "expected an operator definition, found identifier `fn`"
        );
        assert_eq!(errors[0].span(), Span::new(0, 13));
        assert_eq!(fix(source), "operator main{} [];");
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
