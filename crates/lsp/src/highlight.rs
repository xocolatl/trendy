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
    let parent = token.parent().map(|p| p.kind());
    let grandparent = token.parent().and_then(|p| p.parent()).map(|p| p.kind());
    // A heading's owner: an operator's heading holds its parameters.
    let heading_owner = token
        .parent()
        .and_then(|p| p.parent())
        .and_then(|p| p.parent())
        .and_then(|p| p.parent())
        .map(|p| p.kind());
    let (class, definition) = match (token.kind(), parent, grandparent) {
        (SyntaxKind::Comment, _, _) => (Class::Comment, false),
        (kind, _, _) if kind.is_operator() => (Class::ExprOperator, false),
        (kind, _, _) if kind.is_keyword() => (Class::Keyword, false),
        // The contextual keywords: `type`, in `builtin type name;`, `key`,
        // `value` and `from`, `any` and `type`, in `any type`, and `types`.
        (SyntaxKind::Ident, Some(SyntaxKind::BuiltinType), _) if token.text() == "type" => {
            (Class::Keyword, false)
        }
        (
            SyntaxKind::Ident,
            Some(
                SyntaxKind::Key
                | SyntaxKind::FromExpr
                | SyntaxKind::AnyType
                | SyntaxKind::TypeParameters,
            ),
            _,
        ) => (Class::Keyword, false),
        // A type parameter, declared in `types { … }`.
        (SyntaxKind::Ident, Some(SyntaxKind::Name), Some(SyntaxKind::AttributeType))
            if heading_owner == Some(SyntaxKind::TypeParameters) =>
        {
            (Class::Type, true)
        }
        (SyntaxKind::Ident, Some(SyntaxKind::Name), Some(SyntaxKind::BuiltinType)) => {
            (Class::Type, true)
        }
        (SyntaxKind::Ident, Some(SyntaxKind::Name), Some(SyntaxKind::AttributeType))
            if matches!(
                heading_owner,
                Some(SyntaxKind::Operator | SyntaxKind::BuiltinOperator)
            ) =>
        {
            (Class::Parameter, true)
        }
        (
            SyntaxKind::Ident,
            Some(SyntaxKind::NameRef),
            Some(SyntaxKind::CallStmt | SyntaxKind::CallExpr),
        ) => (Class::Operator, false),
        (SyntaxKind::Ident, Some(SyntaxKind::NameRef), _) => {
            if is_parameter(token) {
                (Class::Parameter, false)
            } else {
                (Class::Variable, false)
            }
        }
        (
            SyntaxKind::Ident,
            Some(SyntaxKind::Name),
            Some(SyntaxKind::Operator | SyntaxKind::BuiltinOperator),
        ) => (Class::Operator, true),
        (SyntaxKind::Ident, Some(SyntaxKind::Name), Some(SyntaxKind::VarDecl)) => {
            (Class::Variable, true)
        }
        (
            SyntaxKind::Ident,
            Some(SyntaxKind::Name),
            Some(SyntaxKind::AttributeType | SyntaxKind::Gathering),
        ) => (Class::Attribute, true),
        // A new attribute in an `extend`, or an existing one in a `replace`.
        (SyntaxKind::Ident, Some(SyntaxKind::Name), Some(SyntaxKind::Assign)) => {
            (Class::Attribute, !is_replaced(token))
        }
        (
            SyntaxKind::Ident,
            Some(SyntaxKind::Name),
            Some(
                SyntaxKind::AttributeValue
                | SyntaxKind::NameList
                | SyntaxKind::Renaming
                | SyntaxKind::ExtractExpr,
            ),
        ) => (Class::Attribute, false),
        (SyntaxKind::Ident, Some(SyntaxKind::TypeRef), _) => (Class::Type, false),
        (SyntaxKind::IntLiteral | SyntaxKind::ApproximateLiteral, _, _) => (Class::Number, false),
        (SyntaxKind::TextLiteral, _, _) => (Class::Text, false),
        _ => return None,
    };
    Some(Highlight {
        span: trendy_syntax::span(token.text_range()),
        class,
        definition,
    })
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
