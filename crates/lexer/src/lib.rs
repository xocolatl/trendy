use trendy_diag::{Code, Diagnostic, Edit, Span};
use trendy_syntax::SyntaxKind;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Token {
    pub kind: SyntaxKind,
    pub span: Span,
}

/// The lexer's output: tokens that cover the source exactly, in order, plus
/// an error for each unrecognized character.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lexed {
    pub tokens: Vec<Token>,
    pub errors: Vec<Diagnostic>,
}

/// Split source text into tokens, including whitespace.
///
/// Never fails: an unrecognized character becomes an
/// [`SyntaxKind::ErrorToken`] and an entry in [`Lexed::errors`].
#[must_use]
pub fn lex(source: &str) -> Lexed {
    let mut tokens = Vec::new();
    let mut errors = Vec::new();
    let mut chars = source.char_indices().peekable();

    while let Some((start, c)) = chars.next() {
        let mut end = start + c.len_utf8();
        let mut extend_while = |pred: fn(char) -> bool| {
            while let Some(&(i, c)) = chars.peek() {
                if !pred(c) {
                    break;
                }
                end = i + c.len_utf8();
                chars.next();
            }
        };

        let kind = if c.is_whitespace() {
            extend_while(char::is_whitespace);
            SyntaxKind::Whitespace
        } else if is_ident_start(c) {
            extend_while(is_ident_continue);
            SyntaxKind::keyword(&source[start..end]).unwrap_or(SyntaxKind::Ident)
        } else {
            match c {
                '{' => SyntaxKind::LBrace,
                '}' => SyntaxKind::RBrace,
                '[' => SyntaxKind::LBracket,
                ']' => SyntaxKind::RBracket,
                '(' => SyntaxKind::LParen,
                ')' => SyntaxKind::RParen,
                ';' => SyntaxKind::Semicolon,
                _ => {
                    errors.push(unexpected_character(c, Span::new(start, end)));
                    SyntaxKind::ErrorToken
                }
            }
        };
        tokens.push(Token {
            kind,
            span: Span::new(start, end),
        });
    }

    Lexed { tokens, errors }
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
    Diagnostic::error(
        Code::UnexpectedCharacter,
        format!("unexpected character `{shown}`{code_point}"),
        span,
        "not valid anywhere in trendy source",
    )
    .with_note("identifiers use ASCII letters, digits and `_`; punctuation is `{ } [ ] ( ) ;`")
    .with_suggestion(
        format!("remove `{shown}`"),
        vec![Edit {
            span,
            replacement: String::new(),
        }],
    )
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
