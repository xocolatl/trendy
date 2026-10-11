//! The VS Code extension's TextMate grammar is generated from the lexer, so
//! it can't drift from the language. This test regenerates it and fails if
//! the committed file differs. To update the file after changing tokens:
//!
//! ```text
//! UPDATE_TEXTMATE=1 cargo test -p trendy_lsp --test textmate
//! ```
//!
//! The grammar only gives instant, approximate coloring while the language
//! server starts. The server's semantic tokens, computed from the real CST,
//! take over from there.

use std::path::PathBuf;

use serde_json::{Value, json};
use trendy_syntax::SyntaxKind;

fn grammar_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../editors/vscode/syntaxes/trendy.tmLanguage.json")
}

/// The TextMate scope for each punctuation token.
fn punctuation_scope(kind: SyntaxKind) -> &'static str {
    match kind {
        SyntaxKind::LBrace | SyntaxKind::RBrace => "punctuation.section.braces.trendy",
        SyntaxKind::LBracket | SyntaxKind::RBracket => "punctuation.section.brackets.trendy",
        SyntaxKind::LParen | SyntaxKind::RParen => "punctuation.section.parens.trendy",
        SyntaxKind::Colon => "punctuation.separator.type.trendy",
        SyntaxKind::Comma => "punctuation.separator.comma.trendy",
        SyntaxKind::Dot => "punctuation.accessor.trendy",
        SyntaxKind::ColonEq => "keyword.operator.assignment.trendy",
        SyntaxKind::Eq
        | SyntaxKind::LtGt
        | SyntaxKind::Lt
        | SyntaxKind::LtEq
        | SyntaxKind::Gt
        | SyntaxKind::GtEq => "keyword.operator.comparison.trendy",
        SyntaxKind::Semicolon => "punctuation.terminator.trendy",
        SyntaxKind::Plus | SyntaxKind::Minus | SyntaxKind::Star | SyntaxKind::Slash => {
            "keyword.operator.arithmetic.trendy"
        }
        SyntaxKind::PipePipe => "keyword.operator.concatenation.trendy",
        other => panic!("choose a TextMate scope for the new punctuation {other:?}"),
    }
}

/// A set of characters, built in increasing order.
#[derive(Default)]
struct CharClass(Vec<(char, char)>);

impl CharClass {
    fn add(&mut self, c: char) {
        self.add_range(c, c);
    }

    /// Add `start..=end`, which must come after everything already added.
    fn add_range(&mut self, start: char, end: char) {
        match self.0.last_mut() {
            Some((_, last)) if u32::from(*last) + 1 == u32::from(start) => *last = end,
            _ => self.0.push((start, end)),
        }
    }

    /// Append `other`, whose characters all come after this class's.
    fn extend(&mut self, other: &CharClass) {
        for &(start, end) in &other.0 {
            self.add_range(start, end);
        }
    }

    /// As a regex character class.
    fn pattern(&self) -> String {
        let one = |c: char| match c {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '_' => c.to_string(),
            _ => format!("\\x{{{:X}}}", u32::from(c)),
        };
        let body: String = self
            .0
            .iter()
            .map(|&(start, end)| match u32::from(end) - u32::from(start) {
                0 => one(start),
                1 => one(start) + &one(end),
                _ => format!("{}-{}", one(start), one(end)),
            })
            .collect();
        format!("[{body}]")
    }
}

/// Word-like token syntax, found by asking the lexer about every character.
struct Probed {
    identifier: String,
    number: String,
}

fn probe() -> Probed {
    // Every character is probed independently, so the work is split across
    // threads by code point range and the results joined in order.
    let threads = std::thread::available_parallelism().map_or(1, usize::from);
    let chunk = (u32::from(char::MAX) + 1).div_ceil(u32::try_from(threads).unwrap());
    let parts: Vec<[CharClass; 4]> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..threads)
            .map(|t| {
                let start = chunk * u32::try_from(t).unwrap();
                let end = (start + chunk).min(u32::from(char::MAX) + 1);
                scope.spawn(move || probe_range(start..end))
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    let mut classes: [CharClass; 4] = Default::default();
    for part in &parts {
        for (class, piece) in classes.iter_mut().zip(part) {
            class.extend(piece);
        }
    }
    let [ident_start, ident_continue, number_start, number_continue] = classes;
    // A sign continues a number only directly after an exponent marker.
    let markers: String = trendy_syntax::literal::EXPONENT_MARKERS.iter().collect();
    Probed {
        identifier: format!("{}{}*", ident_start.pattern(), ident_continue.pattern()),
        number: format!(
            "{}(?:[{markers}][+\\-]|{})*",
            number_start.pattern(),
            number_continue.pattern()
        ),
    }
}

/// For the code points in `range`: identifier start, identifier
/// continuation, number start, and number continuation characters.
fn probe_range(range: std::ops::Range<u32>) -> [CharClass; 4] {
    let single = |s: &str, want: &dyn Fn(SyntaxKind) -> bool| {
        let tokens = trendy_lexer::lex(s).tokens;
        tokens.len() == 1 && want(tokens[0].kind)
    };
    let is_word = |kind: SyntaxKind| kind == SyntaxKind::Ident || kind.is_keyword();
    let is_number =
        |kind: SyntaxKind| matches!(kind, SyntaxKind::IntLiteral | SyntaxKind::RationalLiteral);
    let mut classes: [CharClass; 4] = Default::default();
    for c in range.filter_map(char::from_u32) {
        let alone = c.to_string();
        if single(&alone, &is_word) && trendy_lexer::lex(&alone).errors.is_empty() {
            classes[0].add(c);
        }
        if single(&format!("a{c}"), &is_word) {
            classes[1].add(c);
        }
        if single(&alone, &is_number) {
            classes[2].add(c);
        }
        // Valid or not, a character that continues a number is colored as
        // part of it; the diagnostic says what's wrong.
        if single(&format!("1{c}"), &is_number) {
            classes[3].add(c);
        }
    }
    classes
}

fn regex_escape(text: &str) -> String {
    text.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c.to_string()
            } else {
                format!("\\{c}")
            }
        })
        .collect()
}

/// One pattern per punctuation scope. Longer tokens are tried first, so
/// that `:=` isn't taken for `:` followed by `=`.
fn punctuation_patterns() -> Vec<Value> {
    let mut punctuation: Vec<(&str, Vec<&str>)> = Vec::new();
    for kind in SyntaxKind::ALL.into_iter().filter(|k| k.is_punctuation()) {
        let text = kind.fixed_text().unwrap();
        let scope = punctuation_scope(kind);
        match punctuation.iter_mut().find(|(s, _)| *s == scope) {
            Some((_, texts)) => texts.push(text),
            None => punctuation.push((scope, vec![text])),
        }
    }
    let longest = |texts: &[&str]| texts.iter().map(|t| t.len()).max().unwrap();
    punctuation.sort_by_key(|(_, texts)| std::cmp::Reverse(longest(texts)));
    punctuation
        .iter()
        .map(|(scope, texts)| {
            let mut texts = texts.clone();
            texts.sort_by_key(|t| std::cmp::Reverse(t.len()));
            let alternatives: Vec<_> = texts.iter().map(|t| regex_escape(t)).collect();
            json!({ "match": alternatives.join("|"), "name": scope })
        })
        .collect()
}

/// Text literals: like the lexer, an unterminated literal ends at the end of
/// the line.
/// `/* … */`. Comments nest, so the rule includes itself.
fn comment_rule() -> Value {
    json!({
        "begin": "/\\*",
        "end": "\\*/",
        "name": "comment.block.trendy",
        "patterns": [{ "include": "#comment" }],
    })
}

fn text_rule() -> Value {
    let simple_escapes: String = trendy_syntax::literal::SIMPLE_ESCAPES
        .iter()
        .map(|(c, _)| regex_escape(&c.to_string()))
        .collect();
    json!({
        "begin": "\"",
        "end": "\"|$",
        "name": "string.quoted.double.trendy",
        "patterns": [
            {
                // The `\u{…}` form is defined in trendy_syntax::literal.
                "match": format!("\\\\(?:[{simple_escapes}]|u\\{{[0-9A-Fa-f]{{1,6}}\\}})"),
                "name": "constant.character.escape.trendy",
            },
            { "match": "\\\\.", "name": "invalid.illegal.escape.trendy" },
        ],
    })
}

fn generate(probed: &Probed) -> Value {
    let kinds = SyntaxKind::ALL;
    let keyword = |kind: SyntaxKind| regex_escape(kind.fixed_text().unwrap());
    // Literal keywords (`true`, `false`) are colored as constants.
    let (literal_keywords, keywords): (Vec<_>, Vec<_>) = kinds
        .into_iter()
        .filter(|k| k.is_keyword())
        .partition(|k| k.is_literal());
    // Operator keywords (`not`, `div`, …) are colored as operators.
    let (operator_keywords, keywords): (Vec<_>, Vec<_>) =
        keywords.into_iter().partition(|k| k.is_operator_keyword());
    let keywords: Vec<_> = keywords.into_iter().map(keyword).collect();
    let operator_keywords: Vec<_> = operator_keywords.into_iter().map(keyword).collect();
    let all_keywords: Vec<_> = kinds
        .into_iter()
        .filter(|k| k.is_keyword())
        .map(keyword)
        .collect();
    let literal_keywords: Vec<_> = literal_keywords.into_iter().map(keyword).collect();
    let identifier = &probed.identifier;

    let punctuation_patterns = punctuation_patterns();

    json!({
        "$schema": "https://raw.githubusercontent.com/martinring/tmlanguage/master/tmlanguage.json",
        "name": "trendy",
        "scopeName": "source.trendy",
        "comment": "Generated from the lexer by crates/lsp/tests/textmate.rs. Do not edit: run `UPDATE_TEXTMATE=1 cargo test -p trendy_lsp --test textmate`.",
        "patterns": [
            { "include": "#comment" },
            { "include": "#operator-definition" },
            { "include": "#builtin-type" },
            { "include": "#call" },
            { "include": "#variable-declaration" },
            { "include": "#keywords" },
            { "include": "#operator-word" },
            { "include": "#boolean" },
            { "include": "#number" },
            { "include": "#text" },
            { "include": "#punctuation" },
        ],
        "repository": {
            "operator-definition": {
                "match": format!("\\b({})\\s+({identifier})", keyword(SyntaxKind::OperatorKw)),
                "captures": {
                    "1": { "name": "keyword.other.trendy" },
                    "2": { "name": "entity.name.function.trendy" },
                },
            },
            // The type name is only captured when it isn't a keyword, such as
            // the `tuple` of a tuple type.
            "variable-declaration": {
                "match": format!(
                    "\\b({})\\s+({identifier})(?:\\s*({})\\s*(?!(?:{})\\b)({identifier}))?",
                    keyword(SyntaxKind::VarKw),
                    regex_escape(SyntaxKind::Colon.fixed_text().unwrap()),
                    all_keywords.join("|"),
                ),
                "captures": {
                    "1": { "name": "keyword.other.trendy" },
                    "2": { "name": "variable.other.definition.trendy" },
                    "3": { "name": punctuation_scope(SyntaxKind::Colon) },
                    "4": { "name": "entity.name.type.trendy" },
                },
            },
            // `type` is a keyword only here, so it gets a rule of its own.
            "builtin-type": {
                "match": format!(
                    "\\b({})\\s+(type)\\s+({identifier})",
                    keyword(SyntaxKind::BuiltinKw)
                ),
                "captures": {
                    "1": { "name": "keyword.other.trendy" },
                    "2": { "name": "keyword.other.trendy" },
                    "3": { "name": "entity.name.type.trendy" },
                },
            },
            "call": {
                "match": format!("\\b({})\\s+({identifier})", keyword(SyntaxKind::CallKw)),
                "captures": {
                    "1": { "name": "keyword.other.trendy" },
                    "2": { "name": "entity.name.function.trendy" },
                },
            },
            "comment": comment_rule(),
            "keywords": {
                "match": format!("\\b(?:{})\\b", keywords.join("|")),
                "name": "keyword.other.trendy",
            },
            "operator-word": {
                "match": format!("\\b(?:{})\\b", operator_keywords.join("|")),
                "name": "keyword.operator.word.trendy",
            },
            "boolean": {
                "match": format!("\\b(?:{})\\b", literal_keywords.join("|")),
                "name": "constant.language.boolean.trendy",
            },
            "number": {
                "match": format!("\\b{}", probed.number),
                "name": "constant.numeric.trendy",
            },
            "text": text_rule(),
            "punctuation": { "patterns": punctuation_patterns },
        },
    })
}

#[test]
fn grammar_is_generated_from_the_lexer() {
    let probed = probe();
    assert_eq!(probed.identifier, "[A-Z_a-z][0-9A-Z_a-z]*");
    assert_eq!(probed.number, "[0-9](?:[eE][+\\-]|[\\x{2E}0-9A-Z_a-z])*");

    let expected = generate(&probed);
    let path = grammar_path();
    if std::env::var_os("UPDATE_TEXTMATE").is_some() {
        let text = serde_json::to_string_pretty(&expected).unwrap() + "\n";
        std::fs::write(&path, text).unwrap();
        return;
    }
    let actual: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert!(
        actual == expected,
        "{} is out of date; run `UPDATE_TEXTMATE=1 cargo test -p trendy_lsp --test textmate`",
        path.display()
    );
}
