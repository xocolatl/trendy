//! The canonical source formatter.
//!
//! The formatter reprints the CST's tokens in order and decides only the
//! whitespace between them. It never adds, drops, or reorders any other
//! token, so it cannot change what a program means, and comments will
//! survive formatting once the language has them. This crate defines the
//! language's layout: whatever it prints is the style.

use trendy_syntax::ast::{AstNode, RelationLiteral, SourceFile};
use trendy_syntax::{SyntaxKind, SyntaxNode, SyntaxToken};

/// Render a file in canonical style.
///
/// Callers should only format trees without syntax errors: an erroneous
/// tree keeps all its tokens, but their layout is not meaningful. The
/// output ends with exactly one newline; an empty file formats as "".
#[must_use]
pub fn format(file: &SourceFile) -> String {
    let mut out = String::new();
    let mut prev: Option<SyntaxToken> = None;
    // Whether the source has a blank line since the previous token.
    let mut blank_line = false;
    let tokens = file
        .syntax()
        .descendants_with_tokens()
        .filter_map(trendy_syntax::SyntaxElement::into_token);

    for token in tokens {
        if token.kind() == SyntaxKind::Whitespace {
            blank_line |= token.text().matches('\n').count() >= 2;
            continue;
        }
        if let Some(prev) = &prev {
            match broken_line(prev, &token) {
                Some(indent) => {
                    out.push('\n');
                    out.push_str(&INDENT.repeat(indent));
                }
                None => out.push_str(separator(prev, &token, blank_line)),
            }
        }
        out.push_str(token.text());
        prev = Some(token);
        blank_line = false;
    }
    if prev.is_some() {
        out.push('\n');
    }
    out
}

const INDENT: &str = "    ";

/// Whether a relation value is laid out one tuple per line: when it has
/// more than one tuple.
fn is_broken(relation: &SyntaxNode) -> bool {
    RelationLiteral::cast(relation.clone()).is_some_and(|r| r.tuples().nth(1).is_some())
}

/// How many broken relation values enclose `relation`.
fn nesting(relation: &SyntaxNode) -> usize {
    relation.ancestors().skip(1).filter(is_broken).count()
}

/// If a line break goes between `prev` and `next`, the indentation level of
/// the new line. Inside a broken relation, each tuple starts its own line,
/// and the closing brace goes on a line of its own.
fn broken_line(prev: &SyntaxToken, next: &SyntaxToken) -> Option<usize> {
    let in_broken = |token: &SyntaxToken| token.parent().filter(is_broken);
    if next.kind() == SyntaxKind::RBrace
        && let Some(relation) = in_broken(next)
    {
        return Some(nesting(&relation));
    }
    match prev.kind() {
        SyntaxKind::LBrace | SyntaxKind::Comma => in_broken(prev).map(|r| nesting(&r) + 1),
        _ => None,
    }
}

/// The whitespace that goes between two adjacent tokens. `blank_line` says
/// whether the source has a blank line between them.
fn separator(prev: &SyntaxToken, next: &SyntaxToken, blank_line: bool) -> &'static str {
    use SyntaxKind::{
        Colon, ColonEq, Comma, Heading, LBrace, LBracket, OperatorKw, RBrace, RelationKw,
        RelationLiteral, Semicolon, TupleKw, TupleLiteral, VarDecl, VarKw,
    };
    if prev.kind() == Semicolon {
        // Between definitions: variable declarations in a row go on adjacent
        // lines unless the source groups them with a blank line, and
        // everything else is separated by a blank line.
        let both_variables = item_kind(prev) == Some(VarDecl) && item_kind(next) == Some(VarDecl);
        return if both_variables && !blank_line {
            "\n"
        } else {
            "\n\n"
        };
    }
    // Braces of a heading, a tuple or a relation, as opposed to an
    // operator's `{}`: `tuple { a: integer }`, `relation { a: integer } {}`.
    let list_brace = |token: &SyntaxToken| {
        token
            .parent()
            .is_some_and(|p| matches!(p.kind(), Heading | TupleLiteral | RelationLiteral))
    };
    match (prev.kind(), next.kind()) {
        (LBrace, RBrace) if list_brace(prev) => "",
        (LBrace, _) if list_brace(prev) => " ",
        (_, RBrace) if list_brace(next) => " ",
        (OperatorKw | VarKw | TupleKw | RelationKw | Colon | ColonEq | Comma, _)
        | (_, ColonEq)
        | (RBrace, LBracket | LBrace) => " ",
        _ => "",
    }
}

/// The kind of the top-level definition that contains `token`.
fn item_kind(token: &SyntaxToken) -> Option<SyntaxKind> {
    token
        .parent_ancestors()
        .find(|node| {
            node.parent()
                .is_some_and(|p| p.kind() == SyntaxKind::SourceFile)
        })
        .map(|node| node.kind())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fmt(source: &str) -> String {
        let parse = trendy_parser::parse(source, trendy_lexer::lex(source));
        assert!(parse.errors.is_empty(), "{:?}", parse.errors);
        format(&parse.tree)
    }

    /// The source's tokens, excluding whitespace.
    fn significant_tokens(source: &str) -> Vec<String> {
        let lexed = trendy_lexer::lex(source);
        lexed
            .tokens
            .iter()
            .filter(|t| t.kind != SyntaxKind::Whitespace)
            .map(|t| source[t.span.start..t.span.end].to_string())
            .collect()
    }

    #[test]
    fn canonical_is_unchanged() {
        assert_eq!(fmt("operator main{} [];\n"), "operator main{} [];\n");
    }

    #[test]
    fn normalizes_whitespace() {
        assert_eq!(
            fmt("  operator\n\tmain {  }\n[\n] ;"),
            "operator main{} [];\n"
        );
    }

    #[test]
    fn separates_operators_with_a_blank_line() {
        assert_eq!(
            fmt("operator a{}[];operator b{}[];"),
            "operator a{} [];\n\noperator b{} [];\n"
        );
    }

    #[test]
    fn variables() {
        assert_eq!(
            fmt("var   i:integer:=0 ;var t :text;var u:=\"x\";\n\n\noperator main{}[];"),
            "var i: integer := 0;\nvar t: text;\nvar u := \"x\";\n\noperator main{} [];\n"
        );
    }

    #[test]
    fn blank_line_between_variables_and_operators() {
        assert_eq!(
            fmt("operator a{}[];var i:=1;operator b{}[];"),
            "operator a{} [];\n\nvar i := 1;\n\noperator b{} [];\n"
        );
    }

    #[test]
    fn blank_lines_group_variables() {
        assert_eq!(
            fmt("var a := 1;\n\n\n\nvar b := 2;\nvar c := 3;\n\noperator main{} [];"),
            "var a := 1;\n\nvar b := 2;\nvar c := 3;\n\noperator main{} [];\n"
        );
    }

    #[test]
    fn tuples() {
        assert_eq!(
            fmt(
                "var x:tuple{a:tuple{b:boolean},c:text};var e:tuple{ };var q:=tuple{x:1.0,y:2.0,};operator main{}[];"
            ),
            "var x: tuple { a: tuple { b: boolean }, c: text };\nvar e: tuple {};\nvar q := tuple { x: 1.0, y: 2.0, };\n\noperator main{} [];\n"
        );
    }

    #[test]
    fn relations() {
        assert_eq!(
            fmt(
                "var r:=relation{a:integer}{{a:1},tuple{a:2}};var e:relation{a:integer}:=relation{};var d:=relation{}{};var t:=tuple{a:integer}{a:1};"
            ),
            "var r := relation { a: integer } {\n    { a: 1 },\n    tuple { a: 2 }\n};\nvar e: relation { a: integer } := relation {};\nvar d := relation {} {};\nvar t := tuple { a: integer } { a: 1 };\n"
        );
    }

    #[test]
    fn relations_with_several_tuples_break_across_lines() {
        assert_eq!(
            fmt("var p:=relation{tuple{n:\"Ada\"},{n:\"Edgar\"},};"),
            "var p := relation {\n    tuple { n: \"Ada\" },\n    { n: \"Edgar\" },\n};\n"
        );
        assert_eq!(
            fmt("var p:=relation{n:text}{{n:\"a\"},{n:\"b\"}};"),
            "var p := relation { n: text } {\n    { n: \"a\" },\n    { n: \"b\" }\n};\n"
        );
        assert_eq!(
            fmt("var q:=relation{{r:relation{{x:1},{x:2}}},{r:relation{}}};"),
            "var q := relation {\n    { r: relation {\n        { x: 1 },\n        { x: 2 }\n    } },\n    { r: relation {} }\n};\n"
        );
    }

    #[test]
    fn empty_file() {
        assert_eq!(fmt("  \n"), "");
    }

    #[test]
    fn idempotent_and_keeps_every_token() {
        for source in [
            "operator main{} [];",
            "operator a { } [ ] ; operator main{}[];",
            "\n\n  operator   x{}\n\n\n[];\n\n",
            "var a:integer:=1;var b:=\"t\\n\";operator main{}[];",
            "var p:tuple{a:tuple{},b:text}:=tuple{a:tuple{},b:\"\",};",
            "var r:=relation{a:integer}{{a:1},tuple{a:2},};",
        ] {
            let once = fmt(source);
            assert_eq!(fmt(&once), once, "not idempotent for {source:?}");
            assert_eq!(significant_tokens(&once), significant_tokens(source));
        }
    }
}
