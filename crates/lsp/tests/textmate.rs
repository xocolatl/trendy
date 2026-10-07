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
        SyntaxKind::Semicolon => "punctuation.terminator.trendy",
        other => panic!("choose a TextMate scope for the new punctuation {other:?}"),
    }
}

/// Every character for which `accepts` holds, as a regex character class.
fn char_class(accepts: impl Fn(char) -> bool) -> String {
    let mut ranges: Vec<(char, char)> = Vec::new();
    for c in (0..=u32::from(char::MAX)).filter_map(char::from_u32) {
        if !accepts(c) {
            continue;
        }
        match ranges.last_mut() {
            Some((_, end)) if u32::from(*end) + 1 == u32::from(c) => *end = c,
            _ => ranges.push((c, c)),
        }
    }
    let one = |c: char| match c {
        'a'..='z' | 'A'..='Z' | '0'..='9' | '_' => c.to_string(),
        _ => format!("\\x{{{:X}}}", u32::from(c)),
    };
    let body: String = ranges
        .iter()
        .map(|&(start, end)| match u32::from(end) - u32::from(start) {
            0 => one(start),
            1 => one(start) + &one(end),
            _ => format!("{}-{}", one(start), one(end)),
        })
        .collect();
    format!("[{body}]")
}

/// The lexer's identifier syntax, found by asking the lexer about every
/// character.
fn identifier_pattern() -> String {
    let single = |s: &str| {
        let lexed = trendy_lexer::lex(s);
        lexed.tokens.len() == 1 && lexed.errors.is_empty()
    };
    let is_word = |kind: SyntaxKind| kind == SyntaxKind::Ident || kind.is_keyword();
    let start = char_class(|c| {
        let s = c.to_string();
        single(&s) && is_word(trendy_lexer::lex(&s).tokens[0].kind)
    });
    let continuation = char_class(|c| single(&format!("a{c}")));
    format!("{start}{continuation}*")
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

fn generate() -> Value {
    let kinds = SyntaxKind::ALL;
    let keywords: Vec<_> = kinds
        .iter()
        .filter(|k| k.is_keyword())
        .map(|k| regex_escape(k.fixed_text().unwrap()))
        .collect();
    let identifier = identifier_pattern();
    let definition_keyword = regex_escape(SyntaxKind::OperatorKw.fixed_text().unwrap());

    let mut punctuation: Vec<(&str, String)> = Vec::new();
    for kind in kinds.iter().filter(|k| k.is_punctuation()) {
        let text = regex_escape(kind.fixed_text().unwrap());
        let scope = punctuation_scope(*kind);
        match punctuation.iter_mut().find(|(s, _)| *s == scope) {
            Some((_, chars)) => chars.push_str(&text),
            None => punctuation.push((scope, text)),
        }
    }

    json!({
        "$schema": "https://raw.githubusercontent.com/martinring/tmlanguage/master/tmlanguage.json",
        "name": "trendy",
        "scopeName": "source.trendy",
        "comment": "Generated from the lexer by crates/lsp/tests/textmate.rs. Do not edit: run `UPDATE_TEXTMATE=1 cargo test -p trendy_lsp --test textmate`.",
        "patterns": [
            { "include": "#definition" },
            { "include": "#keywords" },
            { "include": "#punctuation" },
        ],
        "repository": {
            "definition": {
                "match": format!("\\b({definition_keyword})\\s+({identifier})"),
                "captures": {
                    "1": { "name": "keyword.other.trendy" },
                    "2": { "name": "entity.name.function.trendy" },
                },
            },
            "keywords": {
                "match": format!("\\b(?:{})\\b", keywords.join("|")),
                "name": "keyword.other.trendy",
            },
            "punctuation": {
                "patterns": punctuation
                    .iter()
                    .map(|(scope, chars)| json!({ "match": format!("[{chars}]"), "name": scope }))
                    .collect::<Vec<_>>(),
            },
        },
    })
}

#[test]
fn grammar_is_generated_from_the_lexer() {
    let expected = generate();
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

#[test]
fn identifier_pattern_matches_the_lexer() {
    assert_eq!(identifier_pattern(), "[A-Z_a-z][0-9A-Z_a-z]*");
}
