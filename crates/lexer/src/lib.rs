use trendy_diag::{Code, Diagnostic, Edit, Span};
use trendy_syntax::SyntaxKind;
use trendy_syntax::literal::{self, IntProblem, RationalProblem, TextProblem};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Token {
    pub kind: SyntaxKind,
    pub span: Span,
}

/// The lexer's output: tokens that cover the source exactly, in order, plus
/// an error for each unrecognized character and malformed literal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lexed {
    pub tokens: Vec<Token>,
    pub errors: Vec<Diagnostic>,
}

/// Split source text into tokens, including whitespace.
///
/// Never fails. An unrecognized character becomes an
/// [`SyntaxKind::ErrorToken`], and a malformed literal keeps the kind it was
/// meant to have, so the parser doesn't report the same mistake again. Both
/// add an entry to [`Lexed::errors`].
#[must_use]
pub fn lex(source: &str) -> Lexed {
    let mut tokens = Vec::new();
    let mut errors = Vec::new();
    let mut start = 0;

    while let Some(c) = source[start..].chars().next() {
        let after = |pred: &dyn Fn(char) -> bool| scan_while(source, start + c.len_utf8(), pred);
        let (kind, end) = if c.is_whitespace() {
            (SyntaxKind::Whitespace, after(&char::is_whitespace))
        } else if is_ident_start(c) {
            let end = after(&is_ident_continue);
            let kind = SyntaxKind::keyword(&source[start..end]).unwrap_or(SyntaxKind::Ident);
            (kind, end)
        } else if c.is_ascii_digit() || is_leading_point(&source[start..]) {
            let end = number_end(source, start);
            let text = &source[start..end];
            let span = Span::new(start, end);
            if literal::is_rational(text) {
                if let Err(problem) = literal::check_rational(text) {
                    errors.push(invalid_rational(&problem, text, span));
                }
                (SyntaxKind::RationalLiteral, end)
            } else {
                if let Err(problem) = literal::check_int(text) {
                    errors.push(invalid_integer(&problem, text, span));
                }
                (SyntaxKind::IntLiteral, end)
            }
        } else if c == '"' {
            (
                SyntaxKind::TextLiteral,
                text_literal(source, start, &mut errors),
            )
        } else if source[start..].starts_with(":=") {
            (SyntaxKind::ColonEq, start + 2)
        } else if let Some(len) = foreign_operator(&source[start..]) {
            (SyntaxKind::ForeignOperator, start + len)
        } else {
            let kind = match c {
                '{' => SyntaxKind::LBrace,
                '}' => SyntaxKind::RBrace,
                '[' => SyntaxKind::LBracket,
                ']' => SyntaxKind::RBracket,
                '(' => SyntaxKind::LParen,
                ')' => SyntaxKind::RParen,
                ':' => SyntaxKind::Colon,
                ',' => SyntaxKind::Comma,
                '=' => SyntaxKind::Eq,
                ';' => SyntaxKind::Semicolon,
                '+' => SyntaxKind::Plus,
                '-' => SyntaxKind::Minus,
                '*' => SyntaxKind::Star,
                '/' => SyntaxKind::Slash,
                _ => {
                    let span = Span::new(start, start + c.len_utf8());
                    errors.push(unexpected_character(c, span));
                    SyntaxKind::ErrorToken
                }
            };
            (kind, start + c.len_utf8())
        };
        tokens.push(Token {
            kind,
            span: Span::new(start, end),
        });
        start = end;
    }

    Lexed { tokens, errors }
}

/// Operators from other languages, which the parser replaces with trendy's
/// keywords: `&&` for `and`, `->` for `implies`, `%` for `mod`, and so on.
/// Longer ones come first, so that `&&` isn't two `&`.
pub const FOREIGN_OPERATORS: [&str; 9] = ["&&", "||", "->", "=>", "!", "&", "|", "^", "%"];

/// The length of the foreign operator `text` starts with, if any.
fn foreign_operator(text: &str) -> Option<usize> {
    FOREIGN_OPERATORS
        .iter()
        .find(|op| text.starts_with(**op))
        .map(|op| op.len())
}

/// Whether `text` starts with a decimal point followed by a digit, like
/// `.5`: a malformed rational literal, reported with its fix.
fn is_leading_point(text: &str) -> bool {
    let mut chars = text.chars();
    chars.next() == Some('.') && chars.next().is_some_and(|c| c.is_ascii_digit())
}

/// Where the number token starting at `start` ends. It takes identifier
/// characters and decimal points, so malformed literals stay one token, and a
/// sign directly after an exponent marker.
fn number_end(source: &str, start: usize) -> usize {
    let mut prev = None;
    for (i, c) in source[start..].char_indices() {
        let exponent_sign = matches!(c, '+' | '-')
            && prev.is_some_and(|p| literal::EXPONENT_MARKERS.contains(&p))
            && literal::is_rational(&source[start..start + i]);
        if !(is_ident_continue(c) || c == '.' || exponent_sign) {
            return start + i;
        }
        prev = Some(c);
    }
    source.len()
}

/// The offset of the first character at or after `from` that fails `pred`.
fn scan_while(source: &str, from: usize, pred: &dyn Fn(char) -> bool) -> usize {
    source[from..]
        .find(|c| !pred(c))
        .map_or(source.len(), |i| from + i)
}

/// Scan the text literal starting with the `"` at `start`, report its
/// problems, and return where it ends.
fn text_literal(source: &str, start: usize, errors: &mut Vec<Diagnostic>) -> usize {
    // The body's escapes are checked even when the literal is unterminated,
    // so that every mistake in it is reported at once.
    let mut check_escapes = |body_end: usize| {
        let body = &source[start + 1..body_end];
        for problem in literal::decode_text(body).1 {
            errors.push(invalid_escape(&problem, body, start + 1));
        }
    };
    let mut escaped = false;
    for (i, c) in source[start + 1..].char_indices() {
        let at = start + 1 + i;
        match c {
            '\n' | '\r' => break,
            '"' if !escaped => {
                check_escapes(at);
                return at + 1;
            }
            _ => escaped = !escaped && c == '\\',
        }
    }

    // Unterminated: the literal runs to the end of the line. If the line ends
    // with `;`, the literal most likely ends just before it, so the `;` is
    // left to end the statement and the fix inserts the quote there.
    let line_end = scan_while(source, start + 1, &|c| c != '\n' && c != '\r');
    let line = source[start + 1..line_end].trim_end();
    let end = match line.strip_suffix(';') {
        Some(before) => start + 1 + before.trim_end().len(),
        None => start + 1 + line.len(),
    };
    check_escapes(end);
    errors.push(unterminated_text(Span::new(start, end)));
    end
}

fn unexpected_character(c: char, span: Span) -> Diagnostic {
    // Invisible characters are unreadable between backticks, so non-ASCII
    // characters also get their code point, and control characters an escape.
    let shown = if c.is_control() {
        c.escape_unicode().to_string()
    } else {
        c.to_string()
    };
    let code_point = if c.is_ascii() {
        String::new()
    } else {
        format!(" (U+{:04X})", u32::from(c))
    };
    let punctuation: Vec<_> = SyntaxKind::ALL
        .into_iter()
        .filter_map(|k| k.fixed_text().filter(|_| k.is_punctuation()))
        .collect();
    Diagnostic::error(
        Code::UnexpectedCharacter,
        format!("unexpected character `{shown}`{code_point}"),
        span,
        "not valid anywhere in trendy source",
    )
    .with_note(format!(
        "identifiers use ASCII letters, digits and `_`; text is written in `\"`; punctuation is `{}`",
        punctuation.join(" ")
    ))
    .with_suggestion(
        format!("remove `{shown}`"),
        vec![Edit {
            span,
            replacement: String::new(),
        }],
    )
}

fn unterminated_text(span: Span) -> Diagnostic {
    Diagnostic::error(
        Code::UnterminatedText,
        "unterminated text literal",
        span,
        "this text literal has no closing `\"`",
    )
    .with_note("a text literal must end on the line it starts; write `\\n` for a line break")
    .with_suggestion(
        "insert `\"`",
        vec![Edit {
            span: Span::empty(span.end),
            replacement: "\"".to_string(),
        }],
    )
}

/// `problem` is in `body`, which starts at `offset` in the source.
fn invalid_escape(problem: &TextProblem, body: &str, offset: usize) -> Diagnostic {
    let note = format!("the valid escapes are {}", literal::escape_list());
    match problem {
        TextProblem::UnknownEscape { range, found } => {
            let span = Span::new(offset + range.start, offset + range.end);
            let d = Diagnostic::error(
                Code::InvalidEscape,
                format!("unknown escape `\\{found}`"),
                span,
                "not an escape sequence",
            )
            .with_note(note);
            // `\'` is a quote in many languages; anything else is most often a
            // backslash meant literally, as in a Windows path.
            let (message, replacement) = if *found == '\'' {
                ("write `'` without a backslash".to_string(), "'".to_string())
            } else {
                (
                    "write `\\\\` for a literal backslash".to_string(),
                    format!("\\\\{found}"),
                )
            };
            d.with_suggestion(message, vec![Edit { span, replacement }])
        }
        TextProblem::InvalidUnicode { range, reason } => Diagnostic::error(
            Code::InvalidEscape,
            format!("invalid Unicode escape `{}`", &body[range.clone()]),
            Span::new(offset + range.start, offset + range.end),
            *reason,
        )
        .with_note(
            "a Unicode escape is `\\u{…}` with 1 to 6 hexadecimal digits, such as `\\u{1F600}`",
        )
        .with_note(note),
    }
}

fn invalid_rational(problem: &RationalProblem, text: &str, span: Span) -> Diagnostic {
    let reason = match problem {
        RationalProblem::Fixable { reason, .. } | RationalProblem::Invalid { reason } => reason,
    };
    let d = Diagnostic::error(
        Code::InvalidRational,
        format!("invalid rational literal `{text}`"),
        span,
        format!("this literal {reason}"),
    )
    .with_note(
        "rational literals have digits on both sides of the decimal point, an exponent, or both, such as `0.5`, `5.0` or `1e10`",
    );
    match problem {
        RationalProblem::Fixable { fixed, .. } => d.with_suggestion(
            format!("write `{fixed}`"),
            vec![Edit {
                span,
                replacement: fixed.clone(),
            }],
        ),
        RationalProblem::Invalid { .. } => d,
    }
}

fn invalid_integer(problem: &IntProblem, text: &str, span: Span) -> Diagnostic {
    let d = Diagnostic::error(
        Code::InvalidInteger,
        format!("invalid integer literal `{text}`"),
        span,
        "not a decimal integer",
    )
    .with_note("integer literals are decimal digits only, such as `42`");
    let replace =
        |message: String, replacement: String| (message, vec![Edit { span, replacement }]);
    let fix = match problem {
        IntProblem::Underscores { digits } => Some(replace(
            "remove the underscores".to_string(),
            digits.clone(),
        )),
        IntProblem::Radix {
            name,
            value: Some(value),
        } => Some(replace(
            format!("write the {name} literal in decimal: `{value}`"),
            value.to_string(),
        )),
        IntProblem::Radix { value: None, .. } | IntProblem::Invalid => None,
    };
    match fix {
        Some((message, edits)) => d.with_suggestion(message, edits),
        None => d,
    }
}

fn is_ident_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_'
}

fn is_ident_continue(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

#[cfg(test)]
mod tests {
    use super::*;
    use SyntaxKind::*;

    fn kinds(source: &str) -> Vec<SyntaxKind> {
        lex(source).tokens.into_iter().map(|t| t.kind).collect()
    }

    #[test]
    fn empty_main() {
        assert_eq!(
            kinds("operator main{} [];"),
            [
                OperatorKw, Whitespace, Ident, LBrace, RBrace, Whitespace, LBracket, RBracket,
                Semicolon
            ]
        );
    }

    #[test]
    fn whitespace_runs_are_one_token() {
        assert_eq!(kinds(" \n\t a"), [Whitespace, Ident]);
    }

    #[test]
    fn keyword_prefix_is_identifier() {
        assert_eq!(kinds("operators"), [Ident]);
    }

    #[test]
    fn unexpected_character_is_an_error_token() {
        let lexed = lex("main#?");
        assert_eq!(
            lexed.tokens.iter().map(|t| t.kind).collect::<Vec<_>>(),
            [Ident, ErrorToken, ErrorToken]
        );
        assert_eq!(lexed.errors.len(), 2);
        let error = &lexed.errors[0];
        assert_eq!(error.code, Code::UnexpectedCharacter);
        assert_eq!(error.span(), Span::new(4, 5));
        assert_eq!(error.message, "unexpected character `#`");
        assert_eq!(error.suggestions[0].edits[0].replacement, "");
    }

    #[test]
    fn invisible_characters_are_escaped() {
        assert_eq!(
            lex("\u{200b}").errors[0].message,
            "unexpected character `\u{200b}` (U+200B)"
        );
        assert_eq!(lex("\0").errors[0].message, "unexpected character `\\u{0}`");
    }

    /// Apply every error's first suggestion.
    fn fix(source: &str) -> String {
        let mut edits: Vec<_> = lex(source)
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

    fn codes(source: &str) -> Vec<Code> {
        lex(source).errors.iter().map(|e| e.code).collect()
    }

    #[test]
    fn variable_declaration() {
        assert_eq!(
            kinds(r#"var t: text := "x";"#),
            [
                VarKw,
                Whitespace,
                Ident,
                Colon,
                Whitespace,
                Ident,
                Whitespace,
                ColonEq,
                Whitespace,
                TextLiteral,
                Semicolon
            ]
        );
        assert_eq!(kinds("i=0"), [Ident, Eq, IntLiteral]);
        assert_eq!(kinds(": ="), [Colon, Whitespace, Eq]);
    }

    #[test]
    fn text_literals() {
        assert_eq!(
            kinds(r#""" "a\"b" "\\""#),
            [
                TextLiteral,
                Whitespace,
                TextLiteral,
                Whitespace,
                TextLiteral
            ]
        );
        assert!(lex(r#""tab\tquote\"unicode\u{1F600}""#).errors.is_empty());
    }

    #[test]
    fn unterminated_text_ends_at_the_line() {
        let source = "var t := \"abc\nvar u := 1;";
        let lexed = lex(source);
        assert_eq!(codes(source), [Code::UnterminatedText]);
        assert_eq!(lexed.tokens[6].kind, TextLiteral);
        assert_eq!(lexed.tokens[6].span, Span::new(9, 13));
        assert_eq!(fix(source), "var t := \"abc\"\nvar u := 1;");
    }

    #[test]
    fn unterminated_text_leaves_the_final_semicolon() {
        let source = "var t := \"abc ;  \nx";
        let lexed = lex(source);
        assert_eq!(lexed.tokens[6].span, Span::new(9, 13));
        assert_eq!(lexed.tokens[8].kind, Semicolon);
        assert_eq!(fix(source), "var t := \"abc\" ;  \nx");
        assert_eq!(lex("\"a;b").tokens[0].span, Span::new(0, 4));
    }

    #[test]
    fn unterminated_text_reports_its_escapes_too() {
        let source = "var p := \"C:\\data;";
        assert_eq!(codes(source), [Code::InvalidEscape, Code::UnterminatedText]);
        assert_eq!(fix(source), "var p := \"C:\\\\data\";");
    }

    #[test]
    fn escaped_quote_does_not_terminate() {
        assert_eq!(codes(r#""abc\""#), [Code::UnterminatedText]);
    }

    #[test]
    fn invalid_escapes() {
        let source = r#""C:\data" "it\'s" "\u{D800}""#;
        let errors = lex(source).errors;
        assert_eq!(
            errors.iter().map(|e| &e.message[..]).collect::<Vec<_>>(),
            [
                r"unknown escape `\d`",
                r"unknown escape `\'`",
                r"invalid Unicode escape `\u{D800}`",
            ]
        );
        assert_eq!(errors[0].span(), Span::new(3, 5));
        assert_eq!(errors[1].span(), Span::new(13, 15));
        assert_eq!(errors[2].span(), Span::new(19, 27));
        assert_eq!(errors[2].primary.message, "surrogates are not characters");
        assert!(errors[2].suggestions.is_empty());
        assert_eq!(fix(source), r#""C:\\data" "it's" "\u{D800}""#);
    }

    #[test]
    fn backslash_meant_literally() {
        assert_eq!(fix(r#""C:\data""#), r#""C:\\data""#);
    }

    #[test]
    fn integer_literals() {
        assert_eq!(
            kinds("0 42 007"),
            [IntLiteral, Whitespace, IntLiteral, Whitespace, IntLiteral]
        );
        assert!(
            lex("9223372036854775808").errors.is_empty(),
            "range is sema's job"
        );
    }

    #[test]
    fn malformed_integers_keep_their_kind() {
        assert_eq!(kinds("1_000"), [IntLiteral]);
        assert_eq!(codes("1_000 0x1F 12ab"), [Code::InvalidInteger; 3]);
        assert_eq!(fix("1_000 0x1F 0b101 12ab"), "1000 31 5 12ab");
    }

    #[test]
    fn rational_literals() {
        assert_eq!(
            kinds("0.0 0e0 1.5E-3 2e+2"),
            [
                RationalLiteral,
                Whitespace,
                RationalLiteral,
                Whitespace,
                RationalLiteral,
                Whitespace,
                RationalLiteral
            ]
        );
        assert!(lex("0.0 0e0 1.5E-3 2e+2 17.5").errors.is_empty());
        assert_eq!(kinds("1e5;"), [RationalLiteral, Semicolon]);
        assert_eq!(kinds("0x1E+5"), [IntLiteral, Plus, IntLiteral]);
    }

    #[test]
    fn malformed_rationals_keep_their_kind() {
        assert_eq!(kinds(".5"), [RationalLiteral]);
        assert_eq!(kinds("5."), [RationalLiteral]);
        assert_eq!(codes("5. .5 1_000.5 1e"), [Code::InvalidRational; 4]);
        assert_eq!(fix("5. .5 1_000.5 1e"), "5.0 0.5 1000.5 1e");
        assert_eq!(
            lex("1e").errors[0].primary.message,
            "this literal has an exponent with no digits"
        );
    }

    #[test]
    fn a_lone_point_is_still_unexpected() {
        assert_eq!(codes("."), [Code::UnexpectedCharacter]);
    }

    #[test]
    fn tuple_tokens() {
        assert_eq!(
            kinds("tuple { a: 1, }"),
            [
                TupleKw, Whitespace, LBrace, Whitespace, Ident, Colon, Whitespace, IntLiteral,
                Comma, Whitespace, RBrace
            ]
        );
    }

    #[test]
    fn foreign_operators() {
        assert_eq!(
            kinds("a&&b||!c->d=>e"),
            [
                Ident,
                ForeignOperator,
                Ident,
                ForeignOperator,
                ForeignOperator,
                Ident,
                ForeignOperator,
                Ident,
                ForeignOperator,
                Ident
            ]
        );
        assert!(lex("a && b").errors.is_empty(), "the parser reports them");
        assert_eq!(
            kinds("x = 1"),
            [Ident, Whitespace, Eq, Whitespace, IntLiteral]
        );
    }

    #[test]
    fn boolean_keywords() {
        assert_eq!(
            kinds("not and or xor implies"),
            [
                NotKw, Whitespace, AndKw, Whitespace, OrKw, Whitespace, XorKw, Whitespace,
                ImpliesKw
            ]
        );
    }

    #[test]
    fn arithmetic() {
        assert_eq!(
            kinds("-a+1*2-3e-2 div b mod c"),
            [
                Minus,
                Ident,
                Plus,
                IntLiteral,
                Star,
                IntLiteral,
                Minus,
                RationalLiteral,
                Whitespace,
                DivKw,
                Whitespace,
                Ident,
                Whitespace,
                ModKw,
                Whitespace,
                Ident
            ]
        );
        assert_eq!(
            kinds("a/b%c"),
            [Ident, Slash, Ident, ForeignOperator, Ident]
        );
    }

    #[test]
    fn parentheses() {
        assert_eq!(kinds("()"), [LParen, RParen]);
    }

    #[test]
    fn tokens_cover_the_source() {
        let source = "operator é main{\n}[] ;?";
        let lexed = lex(source);
        let mut pos = 0;
        for token in &lexed.tokens {
            assert_eq!(token.span.start, pos);
            pos = token.span.end;
        }
        assert_eq!(pos, source.len());
    }
}
