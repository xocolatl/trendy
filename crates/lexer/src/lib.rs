use std::fmt::Write;

use trendy_diag::{Code, Diagnostic, Edit, Span};
use trendy_syntax::literal::{self, ApproximateProblem, IntProblem, TextProblem};
use trendy_syntax::{SyntaxKind, is_ident_continue, is_ident_start};

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
        } else if let Some(quote) = bytes_quote(&source[start..]) {
            let kind = if quote == '"' {
                SyntaxKind::BytesLiteral
            } else {
                SyntaxKind::ByteLiteral
            };
            (kind, bytes_literal(source, start, quote, &mut errors))
        } else if is_ident_start(c) {
            let end = after(&is_ident_continue);
            let kind = SyntaxKind::keyword(&source[start..end]).unwrap_or(SyntaxKind::Ident);
            (kind, end)
        } else if c.is_ascii_digit() || is_leading_point(&source[start..]) {
            let end = number_end(source, start);
            let text = &source[start..end];
            let span = Span::new(start, end);
            if literal::is_approximate(text) {
                if let Err(problem) = literal::check_approximate(text) {
                    errors.push(invalid_approximate(&problem, text, span));
                }
                (SyntaxKind::ApproximateLiteral, end)
            } else {
                if let Err(problem) = literal::check_int(text) {
                    errors.push(invalid_integer(&problem, text, span));
                }
                (SyntaxKind::IntLiteral, end)
            }
        } else if source[start..].starts_with("/*") {
            (SyntaxKind::Comment, comment(source, start, &mut errors))
        } else if source[start..].starts_with("*/") && !source[start..].starts_with("*/*") {
            // `*/` can't be code, since `/` never starts an operand; but in
            // `a*/* c */b` it is `*` and then a comment.
            errors.push(stray_comment_end(Span::new(start, start + 2)));
            (SyntaxKind::ErrorToken, start + 2)
        } else if let Some(end) = include(&source[start..]) {
            errors.push(foreign_include(source, Span::new(start, start + end)));
            (SyntaxKind::ErrorToken, start + end)
        } else if c == '"' {
            (
                SyntaxKind::TextLiteral,
                text_literal(source, start, &mut errors),
            )
        } else if c == '\'' {
            (
                SyntaxKind::CharacterLiteral,
                character_literal(source, start, &mut errors),
            )
        } else if source[start..].starts_with(":=") {
            (SyntaxKind::ColonEq, start + 2)
        } else if let Some((kind, len)) = two_character_token(&source[start..]) {
            (kind, start + len)
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
                '.' => SyntaxKind::Dot,
                '=' => SyntaxKind::Eq,
                '<' => SyntaxKind::Lt,
                '>' => SyntaxKind::Gt,
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

/// C's `#include` and Objective-C's `#import`, the directives that use
/// another file, with the word after `#`.
const INCLUDES: [(&str, &str); 2] = [("#include", "C's"), ("#import", "Objective-C's")];

/// Where the directive `text` starts with ends, if it is one of
/// [`INCLUDES`]: at the end of its line.
fn include(text: &str) -> Option<usize> {
    let (word, _) = INCLUDES.iter().find(|(word, _)| {
        text.starts_with(word)
            && !text[word.len()..]
                .chars()
                .next()
                .is_some_and(is_ident_continue)
    })?;
    Some(text.find('\n').unwrap_or(text.len()).max(word.len()))
}

/// C's or Objective-C's directive at `span`, which uses another file. The
/// fix writes the `uses` of the unit its path names, if it names one.
fn foreign_include(source: &str, span: Span) -> Diagnostic {
    let text = source[span.start..span.end].trim_end();
    let (word, language) = INCLUDES
        .iter()
        .find(|(word, _)| text.starts_with(word))
        .expect("an include directive");
    let path = text[word.len()..].trim();
    let unit = path
        .strip_prefix('"')
        .and_then(|p| p.strip_suffix('"'))
        .or_else(|| path.strip_prefix('<').and_then(|p| p.strip_suffix('>')))
        .and_then(trendy_syntax::unit::named_by);
    let span = Span::new(span.start, span.start + text.len());
    let d = Diagnostic::error(
        Code::UsesForm,
        format!("a unit uses another with `uses {{ … }};`, not {language} `{word}`"),
        span,
        "another language's directive",
    )
    .with_note(trendy_syntax::unit::USES_RULE);
    match unit {
        Some(unit) => d.with_suggestion(
            format!("write `uses {{ {unit} }};`"),
            vec![Edit {
                span,
                replacement: format!("uses {{ {unit} }};"),
            }],
        ),
        None => d.with_help("name the unit by its path from the directory of `main`'s file, its directories and its file's stem between `.`s, as in `uses { lakes.depths };`"),
    }
}

/// Operators from other languages, which the parser replaces with trendy's
/// keywords: `&&` for `and`, `->` for `implies`, `%` for `mod`, and so on,
/// and C's `c ? a : b` with an `if`. Longer ones come first, so that `&&`
/// isn't two `&`.
pub const FOREIGN_OPERATORS: [&str; 15] = [
    ">>>", "<<", ">>", "&&", "->", "=>", "==", "!=", "!", "&", "|", "^", "~", "%", "?",
];

/// The two-character punctuation `text` starts with, if any. It's tried
/// before foreign operators, so that `||` isn't two `|`; none of these
/// starts like a foreign operator.
fn two_character_token(text: &str) -> Option<(SyntaxKind, usize)> {
    let kind = match text.get(..2)? {
        "||" => SyntaxKind::PipePipe,
        "::" => SyntaxKind::ColonColon,
        // Tutorial D's image relation; a lone `!` is a foreign `not`.
        "!!" => SyntaxKind::BangBang,
        "<>" => SyntaxKind::LtGt,
        "<=" => SyntaxKind::LtEq,
        ">=" => SyntaxKind::GtEq,
        _ => return None,
    };
    Some((kind, 2))
}

/// The length of the foreign operator `text` starts with, if any.
fn foreign_operator(text: &str) -> Option<usize> {
    FOREIGN_OPERATORS
        .iter()
        .find(|op| text.starts_with(**op))
        .map(|op| op.len())
}

/// Whether `text` starts with a decimal point followed by a digit, like
/// `.5`: a malformed approximate literal, reported with its fix.
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
            && literal::is_approximate(&source[start..start + i]);
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

/// The end of the character literal starting at `start`. A `'''` is a quote without
/// its backslash, and a literal that doesn't end on its line runs to the line's end,
/// or to just before a `;` there, as a text literal does. What isn't one character
/// is reported, keeping the kind, so that nothing else cascades.
fn character_literal(source: &str, start: usize, errors: &mut Vec<Diagnostic>) -> usize {
    if source[start..].starts_with("'''") {
        errors.push(quote_without_backslash(Span::new(start, start + 3)));
        return start + 3;
    }
    let line_end = scan_while(source, start + 1, &|c| c != '\n' && c != '\r');
    let mut escaped = false;
    let mut closing = None;
    for (i, c) in source[start + 1..line_end].char_indices() {
        if c == '\'' && !escaped {
            closing = Some(start + 1 + i);
            break;
        }
        escaped = !escaped && c == '\\';
    }
    let end = closing.unwrap_or_else(|| {
        let line = source[start + 1..line_end].trim_end();
        let before = line.strip_suffix(';').map_or(line, str::trim_end);
        start + 1 + before.len()
    });
    let body = &source[start + 1..end];
    let (value, problems) = literal::decode_character(body);
    for problem in &problems {
        errors.push(invalid_escape(problem, body, start + 1));
    }
    let one = literal::is_one_character(&value);
    let literal_end = end + usize::from(closing.is_some());
    let span = Span::new(start, literal_end);
    if closing.is_none() {
        errors.push(unterminated_character(span, body, one));
    } else if !one && problems.is_empty() {
        errors.push(not_one_character(span, body, &value));
    }
    literal_end
}

/// The quote `text` starts a literal of bytes with, after its prefix: `x`, or
/// the `X` of SQL or the `b` or `B` of Python, Rust and SQL, which are
/// reported. Only a literal puts a quote straight after a name, so that a
/// variable `x` is still one.
fn bytes_quote(text: &str) -> Option<char> {
    let mut chars = text.chars();
    let prefix = chars.next()?;
    let quote = chars.next()?;
    (matches!(prefix, 'x' | 'X' | 'b' | 'B') && matches!(quote, '\'' | '"')).then_some(quote)
}

/// The end of the literal of bytes starting at `start`, with a prefix and then
/// `quote`. One that doesn't end on its line runs to the line's end, or to just
/// before a `;` there, as a text literal does. What is wrong with it is reported,
/// keeping its kind, so that nothing else cascades.
fn bytes_literal(source: &str, start: usize, quote: char, errors: &mut Vec<Diagnostic>) -> usize {
    let body_start = start + 2;
    let line_end = scan_while(source, body_start, &|c| c != '\n' && c != '\r');
    let mut escaped = false;
    let mut closing = None;
    for (i, c) in source[body_start..line_end].char_indices() {
        if c == quote && !escaped {
            closing = Some(body_start + i);
            break;
        }
        escaped = !escaped && c == '\\';
    }
    let end = closing.unwrap_or_else(|| {
        let line = source[body_start..line_end].trim_end();
        let before = line.strip_suffix(';').map_or(line, str::trim_end);
        body_start + before.len()
    });
    let literal_end = end + usize::from(closing.is_some());
    let span = Span::new(start, literal_end);
    let body = &source[body_start..end];
    let prefix = &source[start..=start];
    if closing.is_none() {
        errors.push(unterminated_bytes(span, prefix, quote, body));
    } else if matches!(prefix, "b" | "B") {
        errors.push(byte_string(span, quote, body));
    } else if let Some(problem) = hex_problem(span, prefix, quote, body) {
        errors.push(problem);
    }
    literal_end
}

/// What is wrong with the literal of bytes at `span`, with the prefix `prefix`, `x`
/// or `X`, and the body `body` between its `quote`s, if anything is.
fn hex_problem(span: Span, prefix: &str, quote: char, body: &str) -> Option<Diagnostic> {
    let bytes = match literal::hex_bytes(body) {
        Ok(bytes) => bytes,
        Err(problem) => return Some(invalid_hex(span, quote, &problem)),
    };
    let one_byte = bytes.len() == 1;
    if prefix == "x" && (quote == '"' || one_byte) {
        return None;
    }
    let replacement = if quote == '\'' && one_byte {
        format!("x'{body}'")
    } else {
        format!("x\"{body}\"")
    };
    Some(if prefix == "x" {
        not_one_byte(span, bytes.len(), replacement)
    } else {
        uppercase_bytes_prefix(span, replacement)
    })
}

/// The literal of bytes at `span`, in `quote`s, with the digits `problem` says
/// are wrong.
fn invalid_hex(span: Span, quote: char, problem: &literal::HexProblem) -> Diagnostic {
    match problem {
        literal::HexProblem::Digit { digit, at } => {
            let at = span.start + 2 + at;
            Diagnostic::error(
                Code::ByteLiteral,
                format!("`{digit}` isn't a hexadecimal digit"),
                Span::new(at, at + digit.len_utf8()),
                "not one of `0` to `9` or `a` to `f`",
            )
            .with_note(BYTE_LITERAL)
        }
        literal::HexProblem::Underscores { fixed } => Diagnostic::error(
            Code::ByteLiteral,
            "an underscore in a literal of bytes goes between two digits",
            span,
            "an underscore that separates nothing",
        )
        .with_note(BYTE_LITERAL)
        .with_suggestion(
            "remove it",
            vec![Edit {
                span,
                replacement: format!("x{quote}{fixed}{quote}"),
            }],
        ),
        literal::HexProblem::Odd { digits } => Diagnostic::error(
            Code::ByteLiteral,
            "a byte is two hexadecimal digits",
            span,
            match digits {
                1 => "1 digit".to_string(),
                n => format!("{n} digits, an odd number"),
            },
        )
        .with_note(BYTE_LITERAL)
        .with_help("write each byte's two digits: `x'07'` is the byte 7"),
    }
}

/// The byte literal at `span`, of `bytes` bytes: other languages' `x'…'`
/// holds any number, which `replacement` does here.
fn not_one_byte(span: Span, bytes: usize, replacement: String) -> Diagnostic {
    let label = match bytes {
        0 => "no byte".to_string(),
        n => format!("{n} bytes"),
    };
    Diagnostic::error(
        Code::ByteLiteral,
        "a byte literal in single quotes is one byte",
        span,
        label,
    )
    .with_note(BYTE_LITERAL)
    .with_suggestion(
        format!("for an array of bytes, write `{replacement}`"),
        vec![Edit { span, replacement }],
    )
}

/// SQL's `X'7F'`, at `span`, which `replacement` writes here.
fn uppercase_bytes_prefix(span: Span, replacement: String) -> Diagnostic {
    Diagnostic::error(
        Code::ByteLiteral,
        "a literal of bytes starts with a lowercase `x`",
        span,
        "SQL's spelling",
    )
    .with_note(BYTE_LITERAL)
    .with_suggestion(
        format!("write `{replacement}`"),
        vec![Edit { span, replacement }],
    )
}

/// Python's, Rust's or SQL's `b"abc"`, at `span`, of the body `body` between its
/// `quote`s, written in hexadecimal if that is certainly what it means: of ASCII
/// characters, with no escapes to decode, and not only `0` and `1` between single
/// quotes, which are bits in SQL.
fn byte_string(span: Span, quote: char, body: &str) -> Diagnostic {
    let d = Diagnostic::error(
        Code::ByteLiteral,
        "bytes are written in hexadecimal",
        span,
        "Python's, Rust's or SQL's spelling",
    )
    .with_note(BYTE_LITERAL);
    let bits = quote == '\'' && body.len() > 1 && body.chars().all(|c| matches!(c, '0' | '1'));
    if !body.is_ascii() || body.contains('\\') || bits {
        return d.with_help("write the bytes it means in hexadecimal: `b\"ab\"` is `x\"6162\"`");
    }
    let digits = body.bytes().fold(String::new(), |mut digits, byte| {
        write!(digits, "{byte:02x}").expect("a string takes what is written to it");
        digits
    });
    let replacement = if quote == '\'' && body.len() == 1 {
        format!("x'{digits}'")
    } else {
        format!("x\"{digits}\"")
    };
    d.with_suggestion(
        format!("write `{replacement}`"),
        vec![Edit { span, replacement }],
    )
}

/// The literal of bytes at `span`, with the prefix `prefix` and the body
/// `body`, which has no closing `quote`: fixed by adding it, if it is otherwise
/// right.
fn unterminated_bytes(span: Span, prefix: &str, quote: char, body: &str) -> Diagnostic {
    let d = Diagnostic::error(
        Code::ByteLiteral,
        "unterminated literal of bytes",
        span,
        format!("this literal has no closing `{quote}`"),
    )
    .with_note("a literal of bytes must end on the line it starts")
    .with_note(BYTE_LITERAL);
    let closes = prefix == "x"
        && literal::hex_bytes(body).is_ok_and(|bytes| quote == '"' || bytes.len() == 1);
    if !closes {
        return d;
    }
    d.with_suggestion(
        format!("insert `{quote}`"),
        vec![Edit {
            span: Span::empty(span.end),
            replacement: quote.to_string(),
        }],
    )
}

/// `'''` at `span`.
fn quote_without_backslash(span: Span) -> Diagnostic {
    Diagnostic::error(
        Code::CharacterLiteral,
        "a single quote in a character literal needs a backslash",
        span,
        "these quotes hold nothing, and start another",
    )
    .with_note(CHARACTER_LITERAL)
    .with_suggestion(
        r"write `'\''`",
        vec![Edit {
            span,
            replacement: r"'\''".to_string(),
        }],
    )
}

/// The character literal at `span`, with the body `body`, which holds `value`: not
/// one character, so most likely a text another language writes in single quotes.
fn not_one_character(span: Span, body: &str, value: &str) -> Diagnostic {
    let label = match literal::characters(value).count() {
        0 => "no character is empty".to_string(),
        n => format!("{n} characters"),
    };
    let text = literal::text_literal_of_character_body(body);
    Diagnostic::error(
        Code::CharacterLiteral,
        "a character literal is one character",
        span,
        label,
    )
    .with_note(CHARACTER_LITERAL)
    .with_suggestion(
        format!("for text, write `{text}`"),
        vec![Edit {
            span,
            replacement: text,
        }],
    )
}

/// The character literal at `span`, with the body `body`, which has no closing `'`:
/// fixed by adding it, if it holds one character, and otherwise to a text literal.
fn unterminated_character(span: Span, body: &str, one: bool) -> Diagnostic {
    let d = Diagnostic::error(
        Code::CharacterLiteral,
        "unterminated character literal",
        span,
        "this character literal has no closing `'`",
    )
    .with_note("a character literal must end on the line it starts")
    .with_note(CHARACTER_LITERAL);
    if one {
        return d.with_suggestion(
            "insert `'`",
            vec![Edit {
                span: Span::empty(span.end),
                replacement: "'".to_string(),
            }],
        );
    }
    let text = literal::text_literal_of_character_body(body);
    d.with_suggestion(
        format!("for text, write `{text}`"),
        vec![Edit {
            span,
            replacement: text,
        }],
    )
}

/// What a character literal is.
const CHARACTER_LITERAL: &str = r"a character literal is one character between single quotes, what a reader sees as one, as `'a'` or `'🇸🇪'`; text is written between double quotes, and a single quote in a character literal is `\'`";

const BYTE_LITERAL: &str = r#"a byte is two hexadecimal digits between `x'` and `'`, as `x'7f'`, and an array of bytes is pairs of them between `x"` and `"`, as `x"dead_beef"`, an underscore between any two digits"#;

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
        "identifiers use ASCII letters, digits and `_`; text is written in `\"`, and a character in `'`; punctuation is `{}`",
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

/// Scan the comment starting with the `/*` at `start`, report it if it is
/// unterminated, and return where it ends. Comments nest, so each `/*`
/// needs its own `*/`.
fn comment(source: &str, start: usize, errors: &mut Vec<Diagnostic>) -> usize {
    // Where each comment still open starts, outermost first.
    let mut open = vec![start];
    let mut at = start + 2;
    while at < source.len() {
        let rest = &source[at..];
        if rest.starts_with("/*") {
            open.push(at);
            at += 2;
        } else if rest.starts_with("*/") {
            open.pop();
            at += 2;
            if open.is_empty() {
                return at;
            }
        } else {
            at += rest.chars().next().map_or(1, char::len_utf8);
        }
    }
    errors.push(unterminated_comment(&open, source.len()));
    source.len()
}

/// A comment whose openers at `open`, outermost first, have no `*/` before
/// the end of the source, at `end`.
fn unterminated_comment(open: &[usize], end: usize) -> Diagnostic {
    let mut d = Diagnostic::error(
        Code::UnterminatedComment,
        "unterminated comment",
        Span::new(open[0], open[0] + 2),
        "this comment has no closing `*/`",
    )
    .with_note("comments nest: each `/*` inside a comment needs its own `*/`");
    for &nested in &open[1..] {
        d = d.with_secondary(
            Span::new(nested, nested + 2),
            "a nested comment, also unclosed",
        );
    }
    d.with_suggestion(
        "close it at the end of the file",
        vec![Edit {
            span: Span::empty(end),
            replacement: "*/".repeat(open.len()),
        }],
    )
}

/// A `*/` at `span`, outside any comment.
fn stray_comment_end(span: Span) -> Diagnostic {
    Diagnostic::error(
        Code::StrayCommentEnd,
        "`*/` outside a comment",
        span,
        "no comment to close",
    )
    .with_note("comments are written `/* … */`, and nest")
    .with_suggestion(
        "remove it",
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

fn invalid_approximate(problem: &ApproximateProblem, text: &str, span: Span) -> Diagnostic {
    let reason = match problem {
        ApproximateProblem::Fixable { reason, .. } | ApproximateProblem::Invalid { reason } => {
            reason
        }
    };
    let d = Diagnostic::error(
        Code::InvalidApproximate,
        format!("invalid approximate literal `{text}`"),
        span,
        format!("this literal {reason}"),
    )
    .with_note(
        "an approximate literal has an exponent, and digits on both sides of a decimal point if it has one, such as `1e0`, `2.5e0` or `1e-10`, with `_` between any two digits, as `1_000.5e0`; a decimal number without an exponent, such as `2.5`, is reserved for exact numbers",
    );
    match problem {
        ApproximateProblem::Fixable { fixed, .. } => d.with_suggestion(
            format!("write `{fixed}`"),
            vec![Edit {
                span,
                replacement: fixed.clone(),
            }],
        ),
        ApproximateProblem::Invalid { .. } => d,
    }
}

/// What an integer literal is, for the notes of the errors about one.
const INTEGER_RULE: &str = "an integer literal is decimal digits, as `42`, or `0x`, `0o` or `0b` and hexadecimal, octal or binary ones, as `0xFF`, `0o755` or `0b1010`, with `_` between any two digits, as `1_000_000`; a decimal one has no leading zero";

fn invalid_integer(problem: &IntProblem, text: &str, span: Span) -> Diagnostic {
    let replace = |fixed: &str| {
        vec![Edit {
            span,
            replacement: fixed.to_string(),
        }]
    };
    match problem {
        IntProblem::Uppercase { fixed } => Diagnostic::error(
            Code::InvalidInteger,
            format!("`{text}`'s prefix is written in lower case"),
            span,
            "an upper-case prefix",
        )
        .with_note(INTEGER_RULE)
        .with_suggestion(format!("write `{fixed}`"), replace(fixed)),
        IntProblem::Underscores { fixed } => Diagnostic::error(
            Code::InvalidInteger,
            format!("`{text}` has an underscore that isn't between two digits"),
            span,
            "an underscore out of place",
        )
        .with_note(INTEGER_RULE)
        .with_suggestion(format!("write `{fixed}`"), replace(fixed)),
        IntProblem::LeadingZero { digits } => Diagnostic::error(
            Code::InvalidInteger,
            format!("`{text}` has a leading zero"),
            span,
            "C reads this as an octal number",
        )
        .with_note(INTEGER_RULE)
        .with_help(format!(
            "write `{digits}` for the decimal number, or `0o{digits}` for the octal one, as C reads it"
        )),
        IntProblem::Digit { digit, at, radix } => Diagnostic::error(
            Code::InvalidInteger,
            format!("`{text}` has `{digit}`, which isn't a {radix} digit"),
            Span::new(span.start + at, span.start + at + digit.len_utf8()),
            format!("not a {radix} digit"),
        )
        .with_note(INTEGER_RULE),
        IntProblem::NoDigits => Diagnostic::error(
            Code::InvalidInteger,
            format!("`{text}` has no digits"),
            span,
            "a prefix with no digits after it",
        )
        .with_note(INTEGER_RULE),
    }
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

    /// `source` with the first fix of its first error made.
    fn fixed(source: &str) -> String {
        let lexed = lex(source);
        let mut fixed = source.to_string();
        let edits = &lexed.errors[0].suggestions[0].edits;
        for edit in edits.iter().rev() {
            fixed.replace_range(edit.span.start..edit.span.end, &edit.replacement);
        }
        fixed
    }

    #[test]
    fn a_character_literal_is_one_character() {
        for one in [
            "'a'",
            "'é'",
            "'e\u{301}'",
            "'🇸🇪'",
            "'👨\u{200D}👩\u{200D}👧\u{200D}👦'",
            r"'\''",
            r"'\\'",
            "'\"'",
            r"'\n'",
            r"'\r\n'",
            r"'\u{1F44D}'",
            r"'\u{1F1F8}\u{1F1EA}'",
        ] {
            let lexed = lex(one);
            assert_eq!(lexed.errors.len(), 0, "{one}: {:#?}", lexed.errors);
            assert_eq!(kinds(one), [CharacterLiteral], "{one}");
        }
        // It ends where it says, whatever follows.
        assert_eq!(
            kinds("'a' 'b'"),
            [CharacterLiteral, Whitespace, CharacterLiteral]
        );
        assert_eq!(kinds("'a';"), [CharacterLiteral, Semicolon]);
    }

    #[test]
    fn what_isnt_one_character_is_fixed_to_what_it_is() {
        // Text, as other languages write it, and the quote without its backslash.
        for (written, meant, label) in [
            ("'abc'", r#""abc""#, "3 characters"),
            ("''", r#""""#, "no character is empty"),
            ("'🇸🇪🇫🇮'", r#""🇸🇪🇫🇮""#, "2 characters"),
            (r"'it\'s'", r#""it's""#, "4 characters"),
            (r#"'a"b'"#, r#""a\"b""#, "3 characters"),
            (r"'a\nb'", r#""a\nb""#, "3 characters"),
            (
                "'''",
                r"'\''",
                "these quotes hold nothing, and start another",
            ),
        ] {
            let lexed = lex(written);
            assert_eq!(lexed.errors.len(), 1, "{written}: {:#?}", lexed.errors);
            assert_eq!(lexed.errors[0].code, Code::CharacterLiteral, "{written}");
            assert_eq!(lexed.errors[0].primary.message, label, "{written}");
            assert_eq!(fixed(written), meant, "{written}");
            assert_eq!(kinds(written), [CharacterLiteral], "{written}");
            assert!(lex(&fixed(written)).errors.is_empty(), "{written}");
        }
        // Without its end, which goes before a `;`, as text's does.
        for (written, meant) in [
            ("var a := 'x;\n", "var a := 'x';\n"),
            ("var a := 'x\n", "var a := 'x'\n"),
            ("'", "\"\""),
            ("var a := 'abc;\n", "var a := \"abc\";\n"),
        ] {
            let lexed = lex(written);
            assert_eq!(lexed.errors.len(), 1, "{written}: {:#?}", lexed.errors);
            assert_eq!(lexed.errors[0].code, Code::CharacterLiteral, "{written}");
            assert_eq!(fixed(written), meant, "{written}");
        }
        // A bad escape is that, and only that.
        let lexed = lex(r"'\q'");
        assert_eq!(lexed.errors.len(), 1, "{:#?}", lexed.errors);
        assert_eq!(lexed.errors[0].code, Code::InvalidEscape);
        // A text's own quote needs no backslash, as a character's does.
        assert_eq!(lex(r#""\'""#).errors[0].code, Code::InvalidEscape);
        assert!(lex(r#""'""#).errors.is_empty());
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
    fn includes_are_uses() {
        let lexed = lex("#include \"lakes/depths.h\"\nvar x := 1;");
        assert_eq!(lexed.tokens[0].kind, ErrorToken);
        assert_eq!(lexed.errors.len(), 1, "{:#?}", lexed.errors);
        let error = &lexed.errors[0];
        assert_eq!(error.code, Code::UsesForm);
        assert_eq!(
            error.suggestions[0].edits[0].replacement,
            "uses { lakes.depths };"
        );
        assert_eq!(error.suggestions[0].edits[0].span, Span::new(0, 25));
        let lexed = lex("#import <Foundation/Foundation.h>");
        assert_eq!(
            lexed.errors[0].suggestions[0].edits[0].replacement,
            "uses { Foundation.Foundation };"
        );
        let lexed = lex("#include <../x.h>");
        assert!(lexed.errors[0].suggestions.is_empty());
        // Not a directive: `#` alone.
        assert_eq!(lex("#includes").errors[0].code, Code::UnexpectedCharacter);
    }

    #[test]
    fn unexpected_character_is_an_error_token() {
        let lexed = lex("main#@");
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
    fn integers_in_four_radixes() {
        assert!(lex("42 1_000 0xFF_ff 0o755 0b1010").errors.is_empty());
        assert_eq!(kinds("0xFF"), [IntLiteral]);
        // Malformed ones keep their kind, and are fixed where that's
        // certain.
        assert_eq!(kinds("1__000"), [IntLiteral]);
        assert_eq!(codes("1__000 0X1F 0755 12ab"), [Code::InvalidInteger; 4]);
        assert_eq!(fix("1__000 0X1F 1_ 0755 12ab"), "1_000 0x1F 1 0755 12ab");
    }

    #[test]
    fn approximate_literals() {
        assert_eq!(
            kinds("1e0 0e0 1.5E-3 2e+2"),
            [
                ApproximateLiteral,
                Whitespace,
                ApproximateLiteral,
                Whitespace,
                ApproximateLiteral,
                Whitespace,
                ApproximateLiteral
            ]
        );
        assert!(lex("1e0 0e0 1.5E-3 2e+2 17.5e0").errors.is_empty());
        assert_eq!(kinds("1e5;"), [ApproximateLiteral, Semicolon]);
        assert_eq!(kinds("0x1E+5"), [IntLiteral, Plus, IntLiteral]);
    }

    #[test]
    fn malformed_approximates_keep_their_kind() {
        assert_eq!(kinds(".5"), [ApproximateLiteral]);
        assert_eq!(kinds("5."), [ApproximateLiteral]);
        assert_eq!(codes("5. .5 1_000.5 1e"), [Code::InvalidApproximate; 4]);
        assert_eq!(fix("5. .5 1_000.5 1e"), "5e0 0.5e0 1_000.5e0 1e");
        // A decimal number needs an exponent to be approximate.
        assert_eq!(codes("1.5 1.0"), [Code::InvalidApproximate; 2]);
        assert_eq!(fix("1.5 1.0"), "1.5e0 1e0");
        assert_eq!(
            lex("1e").errors[0].primary.message,
            "this literal has an exponent with no digits"
        );
    }

    #[test]
    fn a_lone_point_is_a_dot() {
        assert_eq!(kinds("."), [Dot]);
        assert!(codes(".").is_empty(), "the parser decides");
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
            kinds("a&&b|!c->d=>e||f"),
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
                Ident,
                PipePipe,
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
    fn image_relations() {
        // Two `!`s together are an image relation; apart, each is C's `not`.
        assert_eq!(kinds("!!r"), [BangBang, Ident]);
        assert_eq!(
            kinds("! !r"),
            [ForeignOperator, Whitespace, ForeignOperator, Ident]
        );
        assert_eq!(kinds("!="), [ForeignOperator]);
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
                ApproximateLiteral,
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
    fn comparisons() {
        assert_eq!(
            kinds("a=b<>c<d<=e>f>=g"),
            [
                Ident, Eq, Ident, LtGt, Ident, Lt, Ident, LtEq, Ident, Gt, Ident, GtEq, Ident
            ]
        );
        assert_eq!(
            kinds("a==b!=c=>d"),
            [
                Ident,
                ForeignOperator,
                Ident,
                ForeignOperator,
                Ident,
                ForeignOperator,
                Ident
            ]
        );
    }

    #[test]
    fn dots() {
        assert_eq!(kinds("t.a.b"), [Ident, Dot, Ident, Dot, Ident]);
        assert_eq!(kinds("(t).a"), [LParen, Ident, RParen, Dot, Ident]);
        // A leading point is still a malformed approximate literal.
        assert_eq!(kinds(".5"), [ApproximateLiteral]);
    }

    #[test]
    fn comments() {
        assert_eq!(
            kinds("a/* x */+/**/b"),
            [Ident, Comment, Plus, Comment, Ident]
        );
        // Nested: one comment.
        let source = "a /* x /* y */ z */ b";
        assert_eq!(
            kinds(source),
            [Ident, Whitespace, Comment, Whitespace, Ident]
        );
        assert_eq!(lex(source).tokens[2].span, Span::new(2, 19));
        assert!(lex("/* é\n */").errors.is_empty());
        assert_eq!(
            kinds("a / b"),
            [Ident, Whitespace, Slash, Whitespace, Ident]
        );
        assert_eq!(kinds("2*/* c */3"), [IntLiteral, Star, Comment, IntLiteral]);
    }

    #[test]
    fn unterminated_comments() {
        let source = "var a := 1; /* x /* y */";
        let errors = lex(source).errors;
        assert_eq!(codes(source), [Code::UnterminatedComment]);
        assert_eq!(errors[0].span(), Span::new(12, 14));
        assert!(errors[0].secondary.is_empty(), "the nested one is closed");
        assert_eq!(fix(source), "var a := 1; /* x /* y */*/");
        let errors = lex("/* a /* b").errors;
        assert_eq!(errors[0].secondary[0].span, Span::new(5, 7));
        assert_eq!(fix("/* a /* b"), "/* a /* b*/*/");
    }

    #[test]
    fn stray_comment_end() {
        assert_eq!(codes("var a := 1; */"), [Code::StrayCommentEnd]);
        assert_eq!(fix("var a := 1 */;"), "var a := 1 ;");
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

    /// The fix of the only error `written`, a literal of bytes, has, applied.
    fn bytes_fixed(written: &str) -> String {
        let lexed = lex(written);
        assert_eq!(lexed.errors.len(), 1, "{written}: {:#?}", lexed.errors);
        assert_eq!(lexed.errors[0].code, Code::ByteLiteral, "{written}");
        let suggestion = &lexed.errors[0].suggestions[0];
        let [edit] = suggestion.edits.as_slice() else {
            panic!("{written}: {suggestion:#?}");
        };
        let mut fixed = written.to_string();
        fixed.replace_range(edit.span.start..edit.span.end, &edit.replacement);
        fixed
    }

    #[test]
    fn literals_of_bytes() {
        for byte in ["x'7f'", "x'7F'", "x'7_f'"] {
            assert_eq!(kinds(byte), [ByteLiteral], "{byte}");
            assert!(lex(byte).errors.is_empty(), "{byte}");
        }
        for bytes in ["x\"\"", "x\"dead\"", "x\"DE_AD\"", "x\"7f\""] {
            assert_eq!(kinds(bytes), [BytesLiteral], "{bytes}");
            assert!(lex(bytes).errors.is_empty(), "{bytes}");
        }
        // Only a quote straight after it makes `x` a literal's.
        assert_eq!(kinds("x"), [Ident]);
        assert_eq!(kinds("xor"), [XorKw]);
        assert_eq!(kinds("x 'a'"), [Ident, Whitespace, CharacterLiteral]);
        assert_eq!(kinds("x \"a\""), [Ident, Whitespace, TextLiteral]);
        assert_eq!(kinds("ax'a'"), [Ident, CharacterLiteral]);
        assert_eq!(kinds("x'7f';"), [ByteLiteral, Semicolon]);
    }

    #[test]
    fn literals_of_bytes_are_fixed_when_what_they_mean_is_certain() {
        for (written, right) in [
            // SQL's, of any number of bytes.
            ("x'dead'", "x\"dead\""),
            ("x''", "x\"\""),
            ("X'7F'", "x'7F'"),
            ("X'DEADBEEF'", "x\"DEADBEEF\""),
            ("X\"dead\"", "x\"dead\""),
            // Python's and Rust's.
            ("b\"abc\"", "x\"616263\""),
            ("b'a'", "x'61'"),
            ("b'abc'", "x\"616263\""),
            ("B\"a\"", "x\"61\""),
            ("b\"\"", "x\"\""),
            ("b''", "x\"\""),
            // Stray underscores.
            ("x\"_ab\"", "x\"ab\""),
            ("x'7f_'", "x'7f'"),
            ("x\"a__b\"", "x\"a_b\""),
            // An end left out.
            ("x\"dead", "x\"dead\""),
            ("x\"dead;", "x\"dead\";"),
            ("x'7f;", "x'7f';"),
        ] {
            assert_eq!(bytes_fixed(written), right, "{written}");
        }
        for (written, message) in [
            // The digit left out could be any.
            ("x'7'", "a byte is two hexadecimal digits"),
            ("x\"abc\"", "a byte is two hexadecimal digits"),
            // Not a digit, and only it is pointed at.
            ("x\"zz\"", "`z` isn't a hexadecimal digit"),
            ("x'g0'", "`g` isn't a hexadecimal digit"),
            // Not ASCII, an escape to decode, or SQL's bits.
            ("b\"caf\u{e9}\"", "bytes are written in hexadecimal"),
            ("b\"a\\n\"", "bytes are written in hexadecimal"),
            ("b'0101'", "bytes are written in hexadecimal"),
            // An end left out, of what closing it won't fix.
            ("x'dead", "unterminated literal of bytes"),
            ("x\"abc", "unterminated literal of bytes"),
            ("X'7f", "unterminated literal of bytes"),
        ] {
            let lexed = lex(written);
            assert_eq!(lexed.errors.len(), 1, "{written}: {:#?}", lexed.errors);
            assert_eq!(lexed.errors[0].code, Code::ByteLiteral, "{written}");
            assert_eq!(lexed.errors[0].message, message, "{written}");
            assert!(lexed.errors[0].suggestions.is_empty(), "{written}");
        }
        // A quote after a backslash is still inside it.
        let lexed = lex("b\"a\\\"b\";");
        assert_eq!(kinds("b\"a\\\"b\";"), [BytesLiteral, Semicolon]);
        assert_eq!(lexed.errors.len(), 1, "{:#?}", lexed.errors);
        let at = lex("x\"azz\"").errors[0].primary.span;
        assert_eq!((at.start, at.end), (3, 4));
        // What is wrong is told apart from how many there are.
        assert_eq!(lex("x'dead'").errors[0].primary.message, "2 bytes");
        assert_eq!(lex("x''").errors[0].primary.message, "no byte");
        assert_eq!(lex("x'7'").errors[0].primary.message, "1 digit");
        assert_eq!(
            lex("x\"abc\"").errors[0].primary.message,
            "3 digits, an odd number"
        );
    }
}
