//! The canonical source formatter.
//!
//! The formatter reprints the CST's tokens and decides the whitespace between
//! them. It never adds or drops a token, so comments will survive formatting
//! once the language has them. The only tokens it moves are the attributes of
//! a relation's tuples, which it writes in the order of the relation's
//! heading; attributes are unordered, so that never changes what a program
//! means. This crate defines the language's layout: whatever it prints is the
//! style.

mod order;

use trendy_syntax::ast::{AstNode, Body, RelationLiteral, SourceFile};
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
    for token in order::tokens(file) {
        if token.kind() == SyntaxKind::Whitespace {
            blank_line |= token.text().matches('\n').count() >= 2;
            continue;
        }
        if let Some(prev) = &prev {
            if let Some(indent) = broken_line(prev, &token) {
                out.push('\n');
                out.push_str(&INDENT.repeat(indent));
            } else {
                let separator = separator(prev, &token, blank_line);
                // Never let two tokens run together into others: the
                // formatter must not change what the program means.
                if separator.is_empty() && would_merge(prev, &token) {
                    out.push(' ');
                } else {
                    out.push_str(separator);
                }
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

/// Whether a node is laid out one item per line, indented: a body with
/// statements, or a relation value with more than one tuple.
fn is_block(node: &SyntaxNode) -> bool {
    match node.kind() {
        SyntaxKind::Body => {
            Body::cast(node.clone()).is_some_and(|b| b.statements().next().is_some())
        }
        SyntaxKind::RelationLiteral => {
            RelationLiteral::cast(node.clone()).is_some_and(|r| r.tuples().nth(1).is_some())
        }
        _ => false,
    }
}

/// How many blocks enclose `node`, not counting itself: the indentation
/// level of its own lines' start.
fn level(node: &SyntaxNode) -> usize {
    node.ancestors().skip(1).filter(is_block).count()
}

/// If a line break goes between `prev` and `next`, the indentation level of
/// the new line. In a block, each item starts its own line, one level in,
/// and the closing delimiter goes on a line of its own.
fn broken_line(prev: &SyntaxToken, next: &SyntaxToken) -> Option<usize> {
    let block = |token: &SyntaxToken| token.parent().filter(is_block);
    if matches!(next.kind(), SyntaxKind::RBrace | SyntaxKind::RBracket)
        && let Some(block) = block(next)
    {
        return Some(level(&block));
    }
    match prev.kind() {
        SyntaxKind::LBrace | SyntaxKind::LBracket | SyntaxKind::Comma => {
            block(prev).map(|b| level(&b) + 1)
        }
        // A statement's `;`, inside a body.
        SyntaxKind::Semicolon => {
            let body = prev.parent()?.parent().filter(is_block)?;
            Some(level(&body) + 1)
        }
        _ => None,
    }
}

/// The whitespace that goes between two adjacent tokens. `blank_line` says
/// whether the source has a blank line between them.
fn separator(prev: &SyntaxToken, next: &SyntaxToken, blank_line: bool) -> &'static str {
    use SyntaxKind::{
        BuiltinKw, BuiltinType, CallKw, Colon, ColonEq, Comma, Heading, Ident, LBrace, LBracket,
        Minus, NotKw, OperatorKw, Plus, RBrace, RelationKw, RelationLiteral, Semicolon, TupleKw,
        TupleLiteral, VarDecl, VarKw,
    };
    if prev.kind() == Semicolon {
        // Between definitions: variable declarations in a row go on adjacent
        // lines unless the source groups them with a blank line, and
        // everything else is separated by a blank line.
        let both_variables = item_kind(prev) == Some(VarDecl) && item_kind(next) == Some(VarDecl);
        let both_builtin_types =
            item_kind(prev) == Some(BuiltinType) && item_kind(next) == Some(BuiltinType);
        return if (both_variables || both_builtin_types) && !blank_line {
            "\n"
        } else {
            "\n\n"
        };
    }
    // Braces with spaces inside: a heading, a tuple or a relation, as in
    // `tuple { a: integer }`. Parameters and arguments are written without,
    // as in `greet{name: text}`, and so is an operator's empty `{}`.
    let list_brace = |token: &SyntaxToken| {
        token.parent().is_some_and(|p| match p.kind() {
            Heading => p.parent().is_none_or(|owner| {
                !matches!(
                    owner.kind(),
                    SyntaxKind::Operator | SyntaxKind::BuiltinOperator
                )
            }),
            TupleLiteral | RelationLiteral => true,
            _ => false,
        })
    };
    // The contextual keyword in `builtin type name;`.
    let type_keyword = prev.kind() == Ident
        && prev.parent().is_some_and(|p| p.kind() == BuiltinType)
        && prev.text() == "type";
    // A dyadic operator token, such as the `-` in `a - b`, as opposed to the
    // prefix one in `-b`.
    let dyadic = |token: &SyntaxToken| {
        token.kind().is_operator()
            && token
                .parent()
                .is_some_and(|p| p.kind() == SyntaxKind::BinaryExpr)
    };
    let sign = |token: &SyntaxToken| matches!(token.kind(), Plus | Minus);
    match (prev.kind(), next.kind()) {
        (LBrace, RBrace) if list_brace(prev) => "",
        (LBrace, _) if list_brace(prev) => " ",
        (_, RBrace) if list_brace(next) => " ",
        _ if type_keyword => " ",
        _ if dyadic(prev) || dyadic(next) => " ",
        // `- -a`, not `--a`.
        _ if sign(prev) && sign(next) => " ",
        (
            OperatorKw | VarKw | TupleKw | RelationKw | CallKw | BuiltinKw | Colon | ColonEq
            | Comma | NotKw,
            _,
        )
        | (_, ColonEq)
        | (RBrace, LBracket | LBrace) => " ",
        _ => "",
    }
}

/// Whether `prev` and `next`, written with nothing between them, would lex
/// as some other tokens, as `call` and `f` would run together into `callf`.
fn would_merge(prev: &SyntaxToken, next: &SyntaxToken) -> bool {
    let joined = format!("{}{}", prev.text(), next.text());
    let tokens = trendy_lexer::lex(&joined).tokens;
    !(tokens.len() == 2 && tokens[0].kind == prev.kind() && tokens[1].kind == next.kind())
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
    fn operators_and_calls() {
        assert_eq!(
            fmt(
                "operator greet { name : text } [ call debug_value { v : name } ; call debug_value{v:1};];operator main{}[call greet{name:\"Ada\"};];"
            ),
            "operator greet{name: text} [\n    call debug_value{v: name};\n    call debug_value{v: 1};\n];\n\noperator main{} [\n    call greet{name: \"Ada\"};\n];\n"
        );
    }

    #[test]
    fn relations_in_calls_indent_inside_the_body() {
        assert_eq!(
            fmt("operator main{}[call debug_value{v:relation{{a:1},{a:2}}};];"),
            "operator main{} [\n    call debug_value{v: relation {\n        { a: 1 },\n        { a: 2 }\n    }};\n];\n"
        );
    }

    #[test]
    fn the_prelude_is_canonical() {
        assert_eq!(fmt(trendy_prelude_source()), trendy_prelude_source());
    }

    fn trendy_prelude_source() -> &'static str {
        include_str!("../../prelude/src/prelude.trendy")
    }

    #[test]
    fn tokens_never_run_together() {
        // A parse with errors still has its tokens; the formatter must keep
        // them apart even where no rule puts a space.
        let source = "var x: =1;";
        let parse = trendy_parser::parse(source, trendy_lexer::lex(source));
        let out = format(&parse.tree);
        assert_eq!(significant_tokens(&out), significant_tokens(source));
    }

    #[test]
    fn relation_tuples_follow_the_declared_heading() {
        assert_eq!(
            fmt(
                "var r: relation { a: boolean, b: integer } := relation {{a:true, b:3}, {b:25, a:false}};"
            ),
            "var r: relation { a: boolean, b: integer } := relation {\n    { a: true, b: 3 },\n    { a: false, b: 25 }\n};\n"
        );
    }

    #[test]
    fn heading_order_is_as_written_not_alphabetical() {
        assert_eq!(
            fmt("var r: relation { b: integer, a: boolean } := relation { { a: true, b: 3 } };"),
            "var r: relation { b: integer, a: boolean } := relation { { b: 3, a: true } };\n"
        );
    }

    #[test]
    fn the_selector_heading_wins() {
        assert_eq!(
            fmt(
                "var r := relation { y: integer, x: integer } { { x: 1, y: 2 }, tuple { x: 3, y: 4, } };"
            ),
            "var r := relation { y: integer, x: integer } {\n    { y: 2, x: 1 },\n    tuple { y: 4, x: 3, }\n};\n"
        );
    }

    #[test]
    fn without_a_heading_the_first_tuple_sets_the_order() {
        assert_eq!(
            fmt("var r := relation { { n: 1, m: 2 }, { m: 3, n: 4 } };"),
            "var r := relation {\n    { n: 1, m: 2 },\n    { n: 4, m: 3 }\n};\n"
        );
    }

    #[test]
    fn nested_relations_follow_their_attribute_types() {
        assert_eq!(
            fmt(
                "var t: tuple { r: relation { b: integer, a: integer } } := tuple { r: relation { { a: 1, b: 2 } } };"
            ),
            "var t: tuple { r: relation { b: integer, a: integer } } := tuple { r: relation { { b: 2, a: 1 } } };\n"
        );
        assert_eq!(
            fmt(
                "operator f{r: relation { b: integer, a: integer }} [];operator main{}[call f{r: relation { { a: 1, b: 2 } }};];"
            ),
            "operator f{r: relation { b: integer, a: integer }} [];\n\noperator main{} [\n    call f{r: relation { { b: 2, a: 1 } }};\n];\n"
        );
    }

    #[test]
    fn boolean_operators() {
        assert_eq!(
            fmt("var a:=not(true)and false or  true xor\ntrue implies not false;"),
            "var a := not (true) and false or true xor true implies not false;\n"
        );
        assert_eq!(
            fmt("operator main{}[call debug_value{v:( a or b )};];"),
            "operator main{} [\n    call debug_value{v: (a or b)};\n];\n"
        );
    }

    #[test]
    fn arithmetic_operators() {
        assert_eq!(
            fmt("var a:=-1+2*-x div(3-+4)mod  5;"),
            "var a := -1 + 2 * -x div (3 - +4) mod 5;\n"
        );
        assert_eq!(fmt("var a := - -1;"), "var a := - -1;\n");
        assert_eq!(fmt("var a := 1.0/-2.0;"), "var a := 1.0 / -2.0;\n");
        assert_eq!(fmt("var a := not-1;"), "var a := not -1;\n");
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
            "var r:relation{a:boolean,b:integer}:=relation{{b:1,a:true},{a:false,b:2},};",
            "builtin type any;builtin operator f{v:any};operator main{}[call f{v:1};call f{v:2};];",
        ] {
            let once = fmt(source);
            assert_eq!(fmt(&once), once, "not idempotent for {source:?}");
            // Attributes may move, but no token is added or dropped.
            let sorted = |mut tokens: Vec<String>| {
                tokens.sort();
                tokens
            };
            assert_eq!(
                sorted(significant_tokens(&once)),
                sorted(significant_tokens(source))
            );
        }
    }
}
