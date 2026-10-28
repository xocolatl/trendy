//! Semantic highlighting: what each token is, according to the CST.
//!
//! Classification uses the same tree the compiler parses, so highlighting
//! can't disagree with the compiler. It works on trees with syntax errors
//! too. Tokens with nothing to say (punctuation, whitespace, errors) are left
//! to the editor's grammar.

use trendy_diag::Span;
use trendy_syntax::ast::{AstNode, SourceFile};
use trendy_syntax::{SyntaxKind, SyntaxToken};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Class {
    Keyword,
    /// An operator in an expression, such as `and` or `+`.
    ExprOperator,
    /// An operator's name.
    Operator,
    /// A variable's name.
    Variable,
    /// A tuple attribute's name.
    Attribute,
    /// An operator's parameter.
    Parameter,
    Type,
    Number,
    Text,
    Comment,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Highlight {
    pub span: Span,
    pub class: Class,
    /// Whether this is where the thing is defined, rather than a use of it.
    pub definition: bool,
}

/// Highlights for every classified token, in source order.
pub fn highlights(file: &SourceFile) -> Vec<Highlight> {
    file.syntax()
        .descendants_with_tokens()
        .filter_map(trendy_syntax::SyntaxElement::into_token)
        .filter_map(|token| classify(&token))
        .collect()
}

fn classify(token: &SyntaxToken) -> Option<Highlight> {
    let (class, definition) = match token.kind() {
        SyntaxKind::Comment => (Class::Comment, false),
        kind if kind.is_operator() => (Class::ExprOperator, false),
        kind if kind.is_keyword() => (Class::Keyword, false),
        SyntaxKind::IntLiteral | SyntaxKind::ApproximateLiteral => (Class::Number, false),
        SyntaxKind::TextLiteral => (Class::Text, false),
        SyntaxKind::Ident => word(token)?,
        _ => return None,
    };
    Some(Highlight {
        span: trendy_syntax::span(token.text_range()),
        class,
        definition,
    })
}

/// What the identifier `token` is, and whether it is where that is
/// defined: a contextual keyword, or the name of a type, a kind, or a
/// value.
fn word(token: &SyntaxToken) -> Option<(Class, bool)> {
    // The kind of the node `n` levels above the token, the parent first.
    let above = |n: usize| token.parent_ancestors().nth(n).map(|p| p.kind());
    let (parent, grandparent) = (above(0), above(1));
    // The contextual keywords: `type`, in `builtin type name;`, `key`,
    // `value` and `from`, `any` and `type`, in `any type`, `types`, `per`,
    // a mode's `const` and `out`, `kind`, a load's `load`, `from`, `order`, `asc` and
    // `desc`, a `for` loop's `in`, and `heading`.
    let keyword = match parent {
        Some(SyntaxKind::BuiltinType) => token.text() == "type",
        Some(SyntaxKind::AnyType) => matches!(token.text(), "any" | "type"),
        Some(
            SyntaxKind::Key
            | SyntaxKind::FromExpr
            | SyntaxKind::TypeParameters
            | SyntaxKind::Per
            | SyntaxKind::Mode
            | SyntaxKind::KindDecl
            | SyntaxKind::Load
            | SyntaxKind::OrderItem
            | SyntaxKind::ForStmt
            | SyntaxKind::HeadingDecl,
        ) => true,
        _ => false,
    };
    if keyword {
        return Some((Class::Keyword, false));
    }
    if let Some(named) = type_name(parent, grandparent, above(2), above(3)) {
        return Some(named);
    }
    Some(match (parent, grandparent) {
        (Some(SyntaxKind::Name), Some(SyntaxKind::AttributeType))
            if matches!(
                above(3),
                Some(SyntaxKind::Operator | SyntaxKind::BuiltinOperator)
            ) =>
        {
            (Class::Parameter, true)
        }
        (Some(SyntaxKind::NameRef), Some(SyntaxKind::CallStmt | SyntaxKind::CallExpr)) => {
            (Class::Operator, false)
        }
        (Some(SyntaxKind::NameRef), _) if is_parameter(token) => (Class::Parameter, false),
        (Some(SyntaxKind::NameRef), _) => (Class::Variable, false),
        (Some(SyntaxKind::Name), Some(SyntaxKind::Operator | SyntaxKind::BuiltinOperator)) => {
            (Class::Operator, true)
        }
        (Some(SyntaxKind::Name), Some(SyntaxKind::VarDecl | SyntaxKind::ForStmt)) => {
            (Class::Variable, true)
        }
        (Some(SyntaxKind::Name), Some(SyntaxKind::AttributeType | SyntaxKind::Gathering)) => {
            (Class::Attribute, true)
        }
        // A new attribute in an `extend`, or an existing one in a `replace`.
        (Some(SyntaxKind::Name), Some(SyntaxKind::Assign)) => {
            (Class::Attribute, !is_replaced(token))
        }
        (
            Some(SyntaxKind::Name),
            Some(
                SyntaxKind::AttributeValue
                | SyntaxKind::NameList
                | SyntaxKind::Renaming
                | SyntaxKind::ExtractExpr
                | SyntaxKind::OrderItem,
            ),
        ) => (Class::Attribute, false),
        _ => return None,
    })
}

/// Whether an identifier, below nodes of the kinds `parent`, `grandparent`,
/// and the two above, `third` and `fourth`, is the name of a type or a
/// kind, and whether it is where that is defined.
fn type_name(
    parent: Option<SyntaxKind>,
    grandparent: Option<SyntaxKind>,
    third: Option<SyntaxKind>,
    fourth: Option<SyntaxKind>,
) -> Option<(Class, bool)> {
    let definition = match (parent?, grandparent) {
        // A type or a heading, named, or a kind, after `any`, as in
        // `any ordered`.
        (SyntaxKind::TypeRef | SyntaxKind::HeadingRef | SyntaxKind::AnyType, _) => false,
        // A built-in type, a kind, or a heading, declared.
        (
            SyntaxKind::Name,
            Some(SyntaxKind::BuiltinType | SyntaxKind::KindDecl | SyntaxKind::HeadingDecl),
        ) => true,
        // A kind, among a built-in type's.
        (SyntaxKind::Name, Some(SyntaxKind::NameList))
            if third == Some(SyntaxKind::BuiltinType) =>
        {
            false
        }
        // A type parameter, declared in `types { … }`.
        (SyntaxKind::Name, Some(SyntaxKind::AttributeType))
            if fourth == Some(SyntaxKind::TypeParameters) =>
        {
            true
        }
        _ => return None,
    };
    Some((Class::Type, definition))
}

/// Whether the name `token` refers to a parameter of the operator it is in.
/// Parameters hide variables of the same name, so this decides it.
fn is_parameter(token: &SyntaxToken) -> bool {
    token
        .parent_ancestors()
        .find_map(trendy_syntax::ast::Operator::cast)
        .and_then(|op| op.params())
        .is_some_and(|params| {
            params.attributes().any(|a| {
                a.name()
                    .and_then(|n| n.ident())
                    .is_some_and(|i| i.text() == token.text())
            })
        })
}

/// Whether the name `token`, in an attribute assignment, names an existing
/// attribute that a `replace` or an `update` gives a new value, rather
/// than defining a new one in an `extend`.
fn is_replaced(token: &SyntaxToken) -> bool {
    // The name, the assignment, its list, and what the list belongs to.
    let Some(owner) = token.parent_ancestors().nth(3) else {
        return false;
    };
    match trendy_syntax::ast::PostfixExpr::cast(owner.clone()) {
        Some(postfix) => postfix
            .operator()
            .is_some_and(|keyword| keyword.kind() == SyntaxKind::ReplaceKw),
        None => owner.kind() == SyntaxKind::Update,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn highlight(source: &str) -> Vec<(&str, Class, bool)> {
        let parse = trendy_parser::parse(source, trendy_lexer::lex(source));
        highlights(&parse.tree)
            .into_iter()
            .map(|h| (&source[h.span.start..h.span.end], h.class, h.definition))
            .collect()
    }

    #[test]
    fn boolean_operators() {
        assert_eq!(
            highlight("var a := not b and (c implies true);"),
            [
                ("var", Class::Keyword, false),
                ("a", Class::Variable, true),
                ("not", Class::ExprOperator, false),
                ("b", Class::Variable, false),
                ("and", Class::ExprOperator, false),
                ("c", Class::Variable, false),
                ("implies", Class::ExprOperator, false),
                ("true", Class::Keyword, false),
            ]
        );
    }

    #[test]
    fn arithmetic_operators() {
        assert_eq!(
            highlight("var a := -1 * b mod 2;"),
            [
                ("var", Class::Keyword, false),
                ("a", Class::Variable, true),
                ("-", Class::ExprOperator, false),
                ("1", Class::Number, false),
                ("*", Class::ExprOperator, false),
                ("b", Class::Variable, false),
                ("mod", Class::ExprOperator, false),
                ("2", Class::Number, false),
            ]
        );
    }

    #[test]
    fn projections() {
        assert_eq!(
            highlight("var a := b{c};"),
            [
                ("var", Class::Keyword, false),
                ("a", Class::Variable, true),
                ("b", Class::Variable, false),
                ("c", Class::Attribute, false),
            ]
        );
    }

    #[test]
    fn extractions() {
        assert_eq!(
            highlight("var a := b.c;"),
            [
                ("var", Class::Keyword, false),
                ("a", Class::Variable, true),
                ("b", Class::Variable, false),
                ("c", Class::Attribute, false),
            ]
        );
    }

    #[test]
    fn renamings() {
        assert_eq!(
            highlight("var a := b rename {c as d};"),
            [
                ("var", Class::Keyword, false),
                ("a", Class::Variable, true),
                ("b", Class::Variable, false),
                ("rename", Class::ExprOperator, false),
                ("c", Class::Attribute, false),
                ("as", Class::Keyword, false),
                ("d", Class::Attribute, false),
            ]
        );
    }

    #[test]
    fn joins() {
        assert_eq!(
            highlight("var a := b join c times d compose e;"),
            [
                ("var", Class::Keyword, false),
                ("a", Class::Variable, true),
                ("b", Class::Variable, false),
                ("join", Class::ExprOperator, false),
                ("c", Class::Variable, false),
                ("times", Class::ExprOperator, false),
                ("d", Class::Variable, false),
                ("compose", Class::ExprOperator, false),
                ("e", Class::Variable, false),
            ]
        );
    }

    #[test]
    fn assignments() {
        assert_eq!(
            highlight("var a := b extend { c := d };"),
            [
                ("var", Class::Keyword, false),
                ("a", Class::Variable, true),
                ("b", Class::Variable, false),
                ("extend", Class::ExprOperator, false),
                ("c", Class::Attribute, true),
                ("d", Class::Variable, false),
            ]
        );
        assert_eq!(
            highlight("var a := b replace { c := d };"),
            [
                ("var", Class::Keyword, false),
                ("a", Class::Variable, true),
                ("b", Class::Variable, false),
                ("replace", Class::ExprOperator, false),
                ("c", Class::Attribute, false),
                ("d", Class::Variable, false),
            ]
        );
    }

    #[test]
    fn local_variables() {
        assert_eq!(
            highlight("operator f{} [ var n: integer := 1; n := n + 1; ];"),
            [
                ("operator", Class::Keyword, false),
                ("f", Class::Operator, true),
                ("var", Class::Keyword, false),
                ("n", Class::Variable, true),
                ("integer", Class::Type, false),
                ("1", Class::Number, false),
                ("n", Class::Variable, false),
                ("n", Class::Variable, false),
                ("+", Class::ExprOperator, false),
                ("1", Class::Number, false),
            ]
        );
    }

    #[test]
    fn assignment_statements() {
        assert_eq!(
            highlight("operator f{p: integer} [ n := p; update r { a := b } where c; ];"),
            [
                ("operator", Class::Keyword, false),
                ("f", Class::Operator, true),
                ("p", Class::Parameter, true),
                ("integer", Class::Type, false),
                ("n", Class::Variable, false),
                ("p", Class::Parameter, false),
                ("update", Class::Keyword, false),
                ("r", Class::Variable, false),
                ("a", Class::Attribute, false),
                ("b", Class::Variable, false),
                ("where", Class::ExprOperator, false),
                ("c", Class::Variable, false),
            ]
        );
        assert_eq!(
            highlight("operator f{} [ assign { insert r s; delete r where a; }; ];")
                .into_iter()
                .filter(|(_, class, _)| *class == Class::Keyword)
                .map(|(text, _, _)| text)
                .collect::<Vec<_>>(),
            ["operator", "assign", "insert", "delete"]
        );
    }

    #[test]
    fn groupings() {
        assert_eq!(
            highlight("var a := b group { c := {d} };"),
            [
                ("var", Class::Keyword, false),
                ("a", Class::Variable, true),
                ("b", Class::Variable, false),
                ("group", Class::ExprOperator, false),
                ("c", Class::Attribute, true),
                ("d", Class::Attribute, false),
            ]
        );
    }

    #[test]
    fn taken() {
        assert_eq!(
            highlight("var value := value from from;"),
            [
                ("var", Class::Keyword, false),
                ("value", Class::Variable, true),
                ("value", Class::Keyword, false),
                ("from", Class::Keyword, false),
                ("from", Class::Variable, false),
            ]
        );
    }

    #[test]
    fn images() {
        assert_eq!(
            highlight("var a := b extend { c := !!d };"),
            [
                ("var", Class::Keyword, false),
                ("a", Class::Variable, true),
                ("b", Class::Variable, false),
                ("extend", Class::ExprOperator, false),
                ("c", Class::Attribute, true),
                ("!!", Class::ExprOperator, false),
                ("d", Class::Variable, false),
            ]
        );
    }

    #[test]
    fn joins_of_lists() {
        assert_eq!(
            highlight("var a := join{b, c};"),
            [
                ("var", Class::Keyword, false),
                ("a", Class::Variable, true),
                ("join", Class::ExprOperator, false),
                ("b", Class::Variable, false),
                ("c", Class::Variable, false),
            ]
        );
    }

    #[test]
    fn comments() {
        assert_eq!(
            highlight("/* a /* b */ c */ var x := 1; /* d */"),
            [
                ("/* a /* b */ c */", Class::Comment, false),
                ("var", Class::Keyword, false),
                ("x", Class::Variable, true),
                ("1", Class::Number, false),
                ("/* d */", Class::Comment, false),
            ]
        );
    }

    #[test]
    fn definition() {
        assert_eq!(
            highlight("operator main{} [];"),
            [
                ("operator", Class::Keyword, false),
                ("main", Class::Operator, true),
            ]
        );
    }

    #[test]
    fn variables() {
        assert_eq!(
            highlight("var t: text := \"x\";\nvar i := 0;\nvar r := 1e5;"),
            [
                ("var", Class::Keyword, false),
                ("t", Class::Variable, true),
                ("text", Class::Type, false),
                ("\"x\"", Class::Text, false),
                ("var", Class::Keyword, false),
                ("i", Class::Variable, true),
                ("0", Class::Number, false),
                ("var", Class::Keyword, false),
                ("r", Class::Variable, true),
                ("1e5", Class::Number, false),
            ]
        );
    }

    #[test]
    fn boolean_literals_are_keywords() {
        assert_eq!(
            highlight("var b := true;"),
            [
                ("var", Class::Keyword, false),
                ("b", Class::Variable, true),
                ("true", Class::Keyword, false),
            ]
        );
    }

    #[test]
    fn tuples() {
        assert_eq!(
            highlight("var p: tuple { x: approximate } := tuple { x: 1e0 };"),
            [
                ("var", Class::Keyword, false),
                ("p", Class::Variable, true),
                ("tuple", Class::Keyword, false),
                ("x", Class::Attribute, true),
                ("approximate", Class::Type, false),
                ("tuple", Class::Keyword, false),
                ("x", Class::Attribute, false),
                ("1e0", Class::Number, false),
            ]
        );
    }

    #[test]
    fn operators_parameters_and_calls() {
        assert_eq!(
            highlight("var x := 1;\noperator show{x: integer, y: text} [ call debug_value{v: x}; call debug_value{v: y}; ];\noperator main{} [ call show{x: x, y: \"a\"}; ];")
                .into_iter()
                .filter(|(_, class, _)| *class != Class::Keyword)
                .collect::<Vec<_>>(),
            [
                ("x", Class::Variable, true),
                ("1", Class::Number, false),
                ("show", Class::Operator, true),
                ("x", Class::Parameter, true),
                ("integer", Class::Type, false),
                ("y", Class::Parameter, true),
                ("text", Class::Type, false),
                ("debug_value", Class::Operator, false),
                ("v", Class::Attribute, false),
                ("x", Class::Parameter, false),
                ("debug_value", Class::Operator, false),
                ("v", Class::Attribute, false),
                ("y", Class::Parameter, false),
                ("main", Class::Operator, true),
                ("show", Class::Operator, false),
                ("x", Class::Attribute, false),
                ("x", Class::Variable, false),
                ("y", Class::Attribute, false),
                ("\"a\"", Class::Text, false),
            ]
        );
    }

    #[test]
    fn builtins() {
        assert_eq!(
            highlight("builtin type text;\nbuiltin operator debug_value{v: any type};"),
            [
                ("builtin", Class::Keyword, false),
                ("type", Class::Keyword, false),
                ("text", Class::Type, true),
                ("builtin", Class::Keyword, false),
                ("operator", Class::Keyword, false),
                ("debug_value", Class::Operator, true),
                ("v", Class::Parameter, true),
                ("any", Class::Keyword, false),
                ("type", Class::Keyword, false),
            ]
        );
    }

    #[test]
    fn headings() {
        assert_eq!(
            highlight("heading home := { city: text };\nvar r: relation home := relation home {};"),
            [
                ("heading", Class::Keyword, false),
                ("home", Class::Type, true),
                ("city", Class::Attribute, true),
                ("text", Class::Type, false),
                ("var", Class::Keyword, false),
                ("r", Class::Variable, true),
                ("relation", Class::Keyword, false),
                ("home", Class::Type, false),
                ("relation", Class::Keyword, false),
                ("home", Class::Type, false),
            ]
        );
    }

    #[test]
    fn type_parameters() {
        assert_eq!(
            highlight("operator f{a: T, r: any relation}: T types { T: any type } [];"),
            [
                ("operator", Class::Keyword, false),
                ("f", Class::Operator, true),
                ("a", Class::Parameter, true),
                ("T", Class::Type, false),
                ("r", Class::Parameter, true),
                ("any", Class::Keyword, false),
                ("relation", Class::Keyword, false),
                ("T", Class::Type, false),
                ("types", Class::Keyword, false),
                ("T", Class::Type, true),
                ("any", Class::Keyword, false),
                ("type", Class::Keyword, false),
            ]
        );
    }

    #[test]
    fn per() {
        assert_eq!(
            highlight("operator f{r: any relation, of: integer per r} [];"),
            [
                ("operator", Class::Keyword, false),
                ("f", Class::Operator, true),
                ("r", Class::Parameter, true),
                ("any", Class::Keyword, false),
                ("relation", Class::Keyword, false),
                ("of", Class::Parameter, true),
                ("integer", Class::Type, false),
                ("per", Class::Keyword, false),
                ("r", Class::Parameter, false),
            ]
        );
    }

    #[test]
    fn modes() {
        assert_eq!(
            highlight(
                "operator f{const n: integer, out r: integer} [ call f{const n: 1, out r: r}; ];"
            ),
            [
                ("operator", Class::Keyword, false),
                ("f", Class::Operator, true),
                ("const", Class::Keyword, false),
                ("n", Class::Parameter, true),
                ("integer", Class::Type, false),
                ("out", Class::Keyword, false),
                ("r", Class::Parameter, true),
                ("integer", Class::Type, false),
                ("call", Class::Keyword, false),
                ("f", Class::Operator, false),
                ("const", Class::Keyword, false),
                ("n", Class::Attribute, false),
                ("1", Class::Number, false),
                ("out", Class::Keyword, false),
                ("r", Class::Attribute, false),
                ("r", Class::Parameter, false),
            ]
        );
    }

    #[test]
    fn loads() {
        assert_eq!(
            highlight(
                "operator f{r: relation { name: text }} [ var a: array text; load a value from r order [desc name]; ];"
            ),
            [
                ("operator", Class::Keyword, false),
                ("f", Class::Operator, true),
                ("r", Class::Parameter, true),
                ("relation", Class::Keyword, false),
                ("name", Class::Attribute, true),
                ("text", Class::Type, false),
                ("var", Class::Keyword, false),
                ("a", Class::Variable, true),
                ("array", Class::Keyword, false),
                ("text", Class::Type, false),
                ("load", Class::Keyword, false),
                ("a", Class::Variable, false),
                ("value", Class::Keyword, false),
                ("from", Class::Keyword, false),
                ("r", Class::Parameter, false),
                ("order", Class::Keyword, false),
                ("desc", Class::Keyword, false),
                ("name", Class::Attribute, false),
            ]
        );
    }

    #[test]
    fn kinds() {
        assert_eq!(
            highlight(
                "kind ordered { < };\nbuiltin type text: { ordered };\noperator f{a: any ordered} [];"
            ),
            [
                ("kind", Class::Keyword, false),
                ("ordered", Class::Type, true),
                ("<", Class::ExprOperator, false),
                ("builtin", Class::Keyword, false),
                ("type", Class::Keyword, false),
                ("text", Class::Type, true),
                ("ordered", Class::Type, false),
                ("operator", Class::Keyword, false),
                ("f", Class::Operator, true),
                ("a", Class::Parameter, true),
                ("any", Class::Keyword, false),
                ("ordered", Class::Type, false),
            ]
        );
    }

    #[test]
    fn keys() {
        assert_eq!(
            highlight("var key := relation { { key: 1 } }\n    key { key };"),
            [
                ("var", Class::Keyword, false),
                ("key", Class::Variable, true),
                ("relation", Class::Keyword, false),
                ("key", Class::Attribute, false),
                ("1", Class::Number, false),
                ("key", Class::Keyword, false),
                ("key", Class::Attribute, false),
            ]
        );
    }

    #[test]
    fn works_on_broken_files() {
        assert_eq!(
            highlight("junk operator a{ operator b"),
            [
                ("operator", Class::Keyword, false),
                ("a", Class::Operator, true),
                ("operator", Class::Keyword, false),
                ("b", Class::Operator, true),
            ]
        );
    }
}
