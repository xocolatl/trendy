mod errors;

use std::collections::HashMap;

use trendy_diag::{Diagnostic, Span};
use trendy_hir as hir;
use trendy_syntax::SyntaxToken;
use trendy_syntax::ast::{
    AstNode, Item, Literal, LiteralError, LiteralValue, SourceFile, TypeRef, VarDecl,
};

/// Check a parsed program and translate it to HIR.
///
/// This is the last layer allowed to report errors to the user. Drivers run
/// it only on trees without syntax errors; parts that are missing anyway are
/// skipped, never guessed at.
///
/// # Errors
///
/// Returns every semantic error, in source order.
pub fn analyze(file: &SourceFile) -> Result<hir::Program, Vec<Diagnostic>> {
    let mut errors = Vec::new();
    let values = collect_names(file, &mut errors);

    let mut operators = Vec::new();
    let mut variables = Vec::new();
    let mut entry = None;
    for item in file.items() {
        match item {
            Item::Operator(op) => {
                let Some(ident) = op.name().and_then(|n| n.ident()) else {
                    continue;
                };
                if !values.is_first(&ident) {
                    continue;
                }
                if ident.text() == "main" {
                    entry = Some(hir::OperatorId(operators.len()));
                }
                operators.push(hir::Operator {
                    name: ident.text().to_string(),
                });
            }
            Item::Variable(decl) => {
                if let Some(variable) = variable(&decl, &values, &mut errors) {
                    variables.push(variable);
                }
            }
        }
    }

    if entry.is_none() {
        errors.push(errors::missing_main(file, &values));
    }
    errors.sort_by_key(|d| d.span().start);

    match entry {
        Some(entry) if errors.is_empty() => Ok(hir::Program {
            operators,
            variables,
            entry,
        }),
        _ => Err(errors),
    }
}

/// What a name in the value namespace refers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    Operator,
    Variable,
}

impl Kind {
    pub(crate) fn describe(self) -> &'static str {
        match self {
            Kind::Operator => "an operator",
            Kind::Variable => "a variable",
        }
    }
}

/// The value namespace, shared by operators and variables: each name's first
/// definition. Types have a namespace of their own.
pub(crate) struct Values(HashMap<String, (Kind, Span)>);

impl Values {
    pub(crate) fn get(&self, name: &str) -> Option<(Kind, Span)> {
        self.0.get(name).copied()
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = (&str, Kind, Span)> {
        self.0
            .iter()
            .map(|(name, &(kind, span))| (name.as_str(), kind, span))
    }

    /// Whether `ident` is its name's first definition, not a duplicate.
    fn is_first(&self, ident: &SyntaxToken) -> bool {
        self.get(ident.text())
            .is_some_and(|(_, span)| span == trendy_syntax::span(ident.text_range()))
    }
}

/// Record every top-level name, reporting each one defined more than once.
fn collect_names(file: &SourceFile, errors: &mut Vec<Diagnostic>) -> Values {
    let mut values = HashMap::new();
    for item in file.items() {
        let (kind, name) = match item {
            Item::Operator(op) => (Kind::Operator, op.name()),
            Item::Variable(decl) => (Kind::Variable, decl.name()),
        };
        let Some(ident) = name.and_then(|n| n.ident()) else {
            continue;
        };
        let span = trendy_syntax::span(ident.text_range());
        match values.get(ident.text()) {
            Some(&first) => {
                errors.push(errors::duplicate_name(ident.text(), (kind, span), first));
            }
            None => {
                values.insert(ident.text().to_string(), (kind, span));
            }
        }
    }
    Values(values)
}

/// Check a variable declaration: at least one of its type and initial value
/// must be given, and they must agree.
fn variable(
    decl: &VarDecl,
    values: &Values,
    errors: &mut Vec<Diagnostic>,
) -> Option<hir::Variable> {
    let ident = decl.name()?.ident()?;
    let declared = decl
        .type_ref()
        .map(|t| (resolve_type(&t, values, errors), t));
    let init = decl
        .initializer()
        .map(|lit| (literal_value(&lit, errors), lit));

    let (ty, init) = match (declared, init) {
        (None, None) => {
            errors.push(errors::untyped_variable(
                ident.text(),
                trendy_syntax::span(ident.text_range()),
            ));
            return None;
        }
        (Some((ty, _)), None) => {
            let ty = ty?;
            (ty, ty.default_value())
        }
        (None, Some((value, _))) => {
            let value = value?;
            (value.ty(), value)
        }
        (Some((ty, type_ref)), Some((value, literal))) => {
            let (ty, value) = (ty?, value?);
            if value.ty() != ty {
                errors.push(errors::type_mismatch(ty, &type_ref, &value, &literal));
                return None;
            }
            (ty, value)
        }
    };
    values.is_first(&ident).then(|| hir::Variable {
        name: ident.text().to_string(),
        ty,
        init,
    })
}

/// The built-in type a type reference names, or `None` after reporting an
/// unknown type.
fn resolve_type(
    type_ref: &TypeRef,
    values: &Values,
    errors: &mut Vec<Diagnostic>,
) -> Option<hir::Type> {
    let ident = type_ref.ident()?;
    let ty = hir::Type::from_name(ident.text());
    if ty.is_none() {
        errors.push(errors::unknown_type(&ident, values));
    }
    ty
}

/// A literal's value, or `None` if it has none: either it's too large, which
/// is reported here, or the lexer already reported it as malformed.
fn literal_value(literal: &Literal, errors: &mut Vec<Diagnostic>) -> Option<hir::Value> {
    match literal.value() {
        Ok(LiteralValue::Integer(value)) => Some(hir::Value::Integer(value)),
        Ok(LiteralValue::Rational(value)) => Some(hir::Value::Rational(hir::Rational::new(value))),
        Ok(LiteralValue::Text(value)) => Some(hir::Value::Text(value)),
        Err(LiteralError::IntegerTooLarge) => {
            errors.push(errors::integer_out_of_range(literal.span()));
            None
        }
        Err(LiteralError::RationalTooLarge) => {
            errors.push(errors::rational_out_of_range(literal.span()));
            None
        }
        Err(LiteralError::Malformed) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use trendy_diag::Code;

    fn analyze_str(source: &str) -> Result<hir::Program, Vec<Diagnostic>> {
        let parse = trendy_parser::parse(source, trendy_lexer::lex(source));
        assert!(parse.errors.is_empty(), "{:?}", parse.errors);
        analyze(&parse.tree)
    }

    fn errors(source: &str) -> Vec<Diagnostic> {
        analyze_str(source).unwrap_err()
    }

    fn messages(source: &str) -> Vec<String> {
        errors(source).into_iter().map(|d| d.message).collect()
    }

    /// Apply every error's first suggestion.
    fn fix(source: &str) -> String {
        let mut edits: Vec<_> = errors(source)
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

    const MAIN: &str = "\noperator main{} [];";

    fn variables(source: &str) -> Vec<hir::Variable> {
        analyze_str(&format!("{source}{MAIN}")).unwrap().variables
    }

    #[test]
    fn empty_main() {
        let program = analyze_str("operator main{} [];").unwrap();
        assert_eq!(program.operator(program.entry).name, "main");
    }

    #[test]
    fn entry_is_main_wherever_it_appears() {
        let program = analyze_str("operator a{}[]; operator main{}[];").unwrap();
        assert_eq!(program.entry, hir::OperatorId(1));
    }

    #[test]
    fn variables_with_type_and_value() {
        assert_eq!(
            variables("var i: integer := 7;\nvar t: text := \"a\\tb\";"),
            [
                hir::Variable {
                    name: "i".to_string(),
                    ty: hir::Type::Integer,
                    init: hir::Value::Integer(7),
                },
                hir::Variable {
                    name: "t".to_string(),
                    ty: hir::Type::Text,
                    init: hir::Value::Text("a\tb".to_string()),
                },
            ]
        );
    }

    #[test]
    fn default_values() {
        let vars = variables("var i: integer;\nvar t: text;");
        assert_eq!(vars[0].init, hir::Value::Integer(0));
        assert_eq!(vars[1].init, hir::Value::Text(String::new()));
    }

    #[test]
    fn inferred_types() {
        let vars = variables("var i := 7;\nvar t := \"\";");
        assert_eq!(vars[0].ty, hir::Type::Integer);
        assert_eq!(vars[1].ty, hir::Type::Text);
    }

    #[test]
    fn largest_integer() {
        let vars = variables("var i := 9223372036854775807;");
        assert_eq!(vars[0].init, hir::Value::Integer(i64::MAX));
    }

    #[test]
    fn missing_main() {
        assert_eq!(
            messages("operator start{} [];"),
            ["program has no `main` operator"]
        );
    }

    #[test]
    fn main_must_be_an_operator() {
        let errors = errors("var main := 0;");
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].code, Code::MissingMain);
        assert_eq!(errors[0].span(), Span::new(4, 8));
    }

    #[test]
    fn duplicate_operator() {
        let errors = errors("operator main{}[]; operator main{}[];");
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].code, Code::DuplicateName);
        assert_eq!(errors[0].message, "`main` is defined more than once");
        assert_eq!(errors[0].span(), Span::new(28, 32));
        assert_eq!(errors[0].secondary[0].span, Span::new(9, 13));
    }

    #[test]
    fn variables_and_operators_share_names() {
        let errors = errors("var main := 0;\noperator main{} [];");
        assert_eq!(errors.len(), 2, "{errors:#?}");
        assert_eq!(errors[0].code, Code::MissingMain);
        assert_eq!(errors[1].code, Code::DuplicateName);
        assert_eq!(errors[1].primary.message, "redefined here as an operator");
        assert_eq!(
            errors[1].secondary[0].message,
            "`main` is first defined here as a variable"
        );
    }

    #[test]
    fn variable_may_share_a_name_with_a_type() {
        assert_eq!(variables("var text: text;")[0].name, "text");
    }

    #[test]
    fn missing_main_suggests_adding_one() {
        let errors = errors("operator start{} [];\n");
        let edit = &errors[0].suggestions[0].edits[0];
        assert_eq!(edit.span, Span::empty(21));
        assert_eq!(edit.replacement, "\noperator main{} [];\n");
    }

    #[test]
    fn missing_main_suggests_renaming_near_miss() {
        let errors = errors("operator Main{} [];");
        assert_eq!(errors[0].span(), Span::new(9, 13));
        let suggestion = &errors[0].suggestions[0];
        assert_eq!(suggestion.message, "rename `Main` to `main`");
        assert_eq!(suggestion.edits[0].span, Span::new(9, 13));
    }

    #[test]
    fn unknown_types() {
        let source = format!("var a: int;\nvar b: String;\nvar c: Integer;\nvar d: intger;{MAIN}");
        assert_eq!(
            messages(&source),
            [
                "unknown type `int`",
                "unknown type `String`",
                "unknown type `Integer`",
                "unknown type `intger`"
            ]
        );
        assert_eq!(
            fix(&source),
            format!("var a: integer;\nvar b: text;\nvar c: integer;\nvar d: integer;{MAIN}")
        );
    }

    #[test]
    fn value_used_as_type() {
        let errors = errors(&format!("var x: main;{MAIN}"));
        assert_eq!(errors[0].code, Code::UnknownType);
        assert!(
            errors[0]
                .notes
                .iter()
                .any(|n| n == "`main` is an operator, not a type")
        );
        assert!(errors[0].suggestions.is_empty());
    }

    #[test]
    fn type_mismatches() {
        let source =
            format!("var a: integer := \"5\";\nvar b: text := 5;\nvar c: integer := \"x\";{MAIN}");
        let errors = errors(&source);
        assert_eq!(
            errors.iter().map(|e| &e.message[..]).collect::<Vec<_>>(),
            [
                "mismatched types: expected `integer`, found `text`",
                "mismatched types: expected `text`, found `integer`",
                "mismatched types: expected `integer`, found `text`",
            ]
        );
        assert_eq!(
            errors[0].secondary[0].message,
            "expected because of this type"
        );
        assert_eq!(
            fix(&source),
            format!("var a: integer := 5;\nvar b: text := \"5\";\nvar c: text := \"x\";{MAIN}")
        );
    }

    #[test]
    fn rationals() {
        let vars = variables(
            "var a: rational := 0.0;\nvar b: rational := 0e0;\nvar c := 1.5e-3;\nvar d: rational;",
        );
        let values: Vec<_> = vars.iter().map(|v| (v.ty, v.init.clone())).collect();
        assert_eq!(
            values,
            [
                (
                    hir::Type::Rational,
                    hir::Value::Rational(hir::Rational::new(0.0))
                ),
                (
                    hir::Type::Rational,
                    hir::Value::Rational(hir::Rational::new(0.0))
                ),
                (
                    hir::Type::Rational,
                    hir::Value::Rational(hir::Rational::new(0.0015))
                ),
                (
                    hir::Type::Rational,
                    hir::Value::Rational(hir::Rational::new(0.0))
                ),
            ]
        );
    }

    #[test]
    fn no_implicit_integer_to_rational() {
        let source = format!("var r: rational := 1;{MAIN}");
        let errors = errors(&source);
        assert_eq!(errors[0].code, Code::TypeMismatch);
        assert_eq!(
            errors[0].suggestions[0].message,
            "write it as `rational`: `1.0`"
        );
        assert_eq!(fix(&source), format!("var r: rational := 1.0;{MAIN}"));
    }

    #[test]
    fn rational_mismatch_fixes() {
        let source = format!(
            "var a: integer := 5.0;\nvar b: integer := 2.5;\nvar c: text := 1e3;\nvar d: rational := \"7\";\nvar e: rational := \"0.5\";{MAIN}"
        );
        assert_eq!(
            fix(&source),
            format!(
                "var a: integer := 5;\nvar b: rational := 2.5;\nvar c: text := \"1e3\";\nvar d: rational := 7.0;\nvar e: rational := 0.5;{MAIN}"
            )
        );
    }

    #[test]
    fn rational_type_aliases() {
        let source =
            format!("var a: float;\nvar b: double;\nvar c: Rational;\nvar d: rationl;{MAIN}");
        assert_eq!(
            fix(&source),
            format!("var a: rational;\nvar b: rational;\nvar c: rational;\nvar d: rational;{MAIN}")
        );
    }

    #[test]
    fn rational_out_of_range() {
        let errors = errors(&format!("var r := 1e400;{MAIN}"));
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].code, Code::RationalOutOfRange);
        assert_eq!(errors[0].span(), Span::new(9, 14));
    }

    #[test]
    fn integer_out_of_range() {
        let errors = errors(&format!("var i := 9223372036854775808;{MAIN}"));
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].code, Code::IntegerOutOfRange);
        assert_eq!(errors[0].span(), Span::new(9, 28));
    }

    #[test]
    fn variable_needs_type_or_value() {
        let errors = errors(&format!("var count;{MAIN}"));
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].code, Code::UntypedVariable);
        assert_eq!(errors[0].span(), Span::new(4, 9));
        assert!(errors[0].help[0].contains("`var count: integer;`"));
    }

    #[test]
    fn errors_are_in_source_order() {
        let codes: Vec<_> = errors("var b: bogus;\nvar a;")
            .iter()
            .map(|e| e.code)
            .collect();
        assert_eq!(
            codes,
            [Code::UnknownType, Code::UntypedVariable, Code::MissingMain]
        );
    }
}
