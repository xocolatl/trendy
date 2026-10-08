mod errors;
mod types;

use std::collections::HashMap;

use trendy_diag::{Diagnostic, Span};
use trendy_hir as hir;
use trendy_syntax::SyntaxToken;
use trendy_syntax::ast::{Item, SourceFile, VarDecl};

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
        .type_expr()
        .map(|t| (types::resolve(&t, values, errors), t));

    let (ty, init) = match (declared, decl.initializer()) {
        (None, None) => {
            errors.push(errors::untyped_variable(
                ident.text(),
                trendy_syntax::span(ident.text_range()),
            ));
            return None;
        }
        (Some((ty, _)), None) => {
            let ty = ty?;
            let init = ty.default_value();
            (ty, init)
        }
        (None, Some(expr)) => {
            let value = types::value(&expr, None, values, errors)?;
            (value.ty(), value)
        }
        (Some((None, _)), Some(expr)) => {
            // The type is invalid, but the value can still have errors of its
            // own.
            types::value(&expr, None, values, errors);
            return None;
        }
        (Some((Some(ty), type_expr)), Some(expr)) => {
            let expected = types::Expected {
                ty: ty.clone(),
                origin: types::Origin::Type(type_expr),
            };
            let value = types::value(&expr, Some(&expected), values, errors)?;
            (ty, value)
        }
    };
    values.is_first(&ident).then(|| hir::Variable {
        name: ident.text().to_string(),
        ty,
        init,
    })
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
                    ty: hir::Type::Scalar(hir::Scalar::Integer),
                    init: hir::Value::Integer(7),
                },
                hir::Variable {
                    name: "t".to_string(),
                    ty: hir::Type::Scalar(hir::Scalar::Text),
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
        assert_eq!(vars[0].ty, hir::Type::Scalar(hir::Scalar::Integer));
        assert_eq!(vars[1].ty, hir::Type::Scalar(hir::Scalar::Text));
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
        let values: Vec<_> = vars
            .iter()
            .map(|v| (v.ty.clone(), v.init.clone()))
            .collect();
        assert_eq!(
            values,
            [
                (
                    hir::Type::Scalar(hir::Scalar::Rational),
                    hir::Value::Rational(hir::Rational::new(0.0))
                ),
                (
                    hir::Type::Scalar(hir::Scalar::Rational),
                    hir::Value::Rational(hir::Rational::new(0.0))
                ),
                (
                    hir::Type::Scalar(hir::Scalar::Rational),
                    hir::Value::Rational(hir::Rational::new(0.0015))
                ),
                (
                    hir::Type::Scalar(hir::Scalar::Rational),
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
    fn booleans() {
        let vars = variables("var a := true;\nvar b: boolean := false;\nvar c: boolean;");
        let values: Vec<_> = vars
            .iter()
            .map(|v| (v.ty.clone(), v.init.clone()))
            .collect();
        assert_eq!(
            values,
            [
                (
                    hir::Type::Scalar(hir::Scalar::Boolean),
                    hir::Value::Boolean(true)
                ),
                (
                    hir::Type::Scalar(hir::Scalar::Boolean),
                    hir::Value::Boolean(false)
                ),
                (
                    hir::Type::Scalar(hir::Scalar::Boolean),
                    hir::Value::Boolean(false)
                ),
            ]
        );
    }

    #[test]
    fn boolean_mismatch_fixes() {
        let source = format!(
            "var a: boolean := 1;\nvar b: boolean := \"false\";\nvar c: text := true;\nvar d: bool;\nvar e: boolean := 2;{MAIN}"
        );
        assert_eq!(
            fix(&source),
            format!(
                "var a: boolean := true;\nvar b: boolean := false;\nvar c: text := \"true\";\nvar d: boolean;\nvar e: integer := 2;{MAIN}"
            )
        );
    }

    fn scalar(s: hir::Scalar) -> hir::Type {
        hir::Type::Scalar(s)
    }

    fn tuple_type(attributes: &[(&str, hir::Type)]) -> hir::Type {
        hir::Type::Tuple(hir::Heading::new(
            attributes
                .iter()
                .map(|(n, t)| ((*n).to_string(), t.clone()))
                .collect(),
        ))
    }

    #[test]
    fn nested_tuple_default() {
        let vars = variables("var x: tuple { a: tuple { b: boolean }, c: text };");
        let b = tuple_type(&[("b", scalar(hir::Scalar::Boolean))]);
        assert_eq!(
            vars[0].ty,
            tuple_type(&[("a", b), ("c", scalar(hir::Scalar::Text))])
        );
        assert_eq!(
            vars[0].ty.default_value(),
            vars[0].init,
            "starts at its type's default"
        );
        assert_eq!(
            vars[0].ty.to_string(),
            "tuple { a: tuple { b: boolean }, c: text }"
        );
    }

    #[test]
    fn tuple_inference_and_unordered_attributes() {
        let vars = variables(
            "var q := tuple { y: 2.0, x: 1.0 };\nvar p: tuple { a: integer, b: text } := tuple { b: \"x\", a: 1, };\nvar e: tuple {};\nvar f := tuple {};",
        );
        assert_eq!(vars[0].ty.to_string(), "tuple { x: rational, y: rational }");
        assert_eq!(vars[1].ty.to_string(), "tuple { a: integer, b: text }");
        assert_eq!(vars[2].ty, tuple_type(&[]));
        assert_eq!(vars[3].ty, tuple_type(&[]));
    }

    #[test]
    fn mismatch_is_reported_at_the_attribute() {
        let source = "var p: tuple { a: tuple { b: boolean } } := tuple { a: tuple { b: 1 } };"
            .to_string()
            + MAIN;
        let errors = errors(&source);
        assert_eq!(errors.len(), 1, "{errors:#?}");
        assert_eq!(errors[0].code, Code::TypeMismatch);
        assert_eq!(&source[errors[0].span().start..errors[0].span().end], "1");
        let secondary = errors[0].secondary[0].span;
        assert_eq!(&source[secondary.start..secondary.end], "boolean");
        assert_eq!(
            fix(&source),
            "var p: tuple { a: tuple { b: boolean } } := tuple { a: tuple { b: true } };"
                .to_string()
                + MAIN
        );
    }

    #[test]
    fn tuple_against_scalar() {
        let errors = errors(&format!("var p: tuple {{ a: integer }} := 5;{MAIN}"));
        assert_eq!(
            errors[0].message,
            "mismatched types: expected `tuple { a: integer }`, found `integer`"
        );
        assert_eq!(
            errors[0].suggestions[0].message,
            "change the type to `integer`"
        );
    }

    #[test]
    fn missing_attributes() {
        let source = format!(
            "var p: tuple {{ x: rational, y: rational, z: text }} := tuple {{ x: 1.0 }};{MAIN}"
        );
        let errors = errors(&source);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].code, Code::MissingAttribute);
        assert_eq!(errors[0].message, "missing attributes `y` and `z`");
        assert_eq!(
            fix(&source),
            format!(
                "var p: tuple {{ x: rational, y: rational, z: text }} := tuple {{ x: 1.0, y: 0.0, z: \"\" }};{MAIN}"
            )
        );
        assert_eq!(
            fix(&format!(
                "var p: tuple {{ x: integer, y: integer }} := tuple {{ x: 1, }};{MAIN}"
            )),
            format!("var p: tuple {{ x: integer, y: integer }} := tuple {{ x: 1, y: 0, }};{MAIN}")
        );
        assert_eq!(
            fix(&format!(
                "var p: tuple {{ x: integer }} := tuple {{}};{MAIN}"
            )),
            format!("var p: tuple {{ x: integer }} := tuple {{ x: 0}};{MAIN}")
        );
    }

    #[test]
    fn unknown_attributes() {
        let source = format!(
            "var p: tuple {{ x: rational, y: rational }} := tuple {{ x: 1.0, why: 2.0 }};{MAIN}"
        );
        let errors = errors(&source);
        assert_eq!(
            errors.len(),
            1,
            "a rename resolves the missing attribute too"
        );
        assert_eq!(errors[0].code, Code::UnknownAttribute);
        assert_eq!(errors[0].suggestions[0].message, "rename it to `y`");
        assert_eq!(
            fix(&source),
            format!(
                "var p: tuple {{ x: rational, y: rational }} := tuple {{ x: 1.0, y: 2.0 }};{MAIN}"
            )
        );
        assert_eq!(
            fix(&format!(
                "var p: tuple {{ x: integer }} := tuple {{ x: 1, zzz: 2 }};{MAIN}"
            )),
            format!("var p: tuple {{ x: integer }} := tuple {{ x: 1 }};{MAIN}")
        );
        assert_eq!(
            fix(&format!(
                "var p: tuple {{ x: integer }} := tuple {{ zzz: 2, x: 1 }};{MAIN}"
            )),
            format!("var p: tuple {{ x: integer }} := tuple {{ x: 1 }};{MAIN}")
        );
    }

    #[test]
    fn duplicate_attributes() {
        let errors = errors(&format!(
            "var p: tuple {{ x: integer, x: text }};\nvar q := tuple {{ y: 1, y: 2 }};{MAIN}"
        ));
        let codes: Vec<_> = errors.iter().map(|e| e.code).collect();
        assert_eq!(codes, [Code::DuplicateAttribute, Code::DuplicateAttribute]);
        assert_eq!(errors[0].secondary[0].message, "`x` is first defined here");
    }

    #[test]
    fn unknown_type_inside_a_tuple() {
        let source = format!("var p: tuple {{ a: int }};{MAIN}");
        assert_eq!(
            fix(&source),
            format!("var p: tuple {{ a: integer }};{MAIN}")
        );
    }

    fn relation_value(var: &hir::Variable) -> &hir::RelationValue {
        match &var.init {
            hir::Value::Relation(r) => r,
            other => panic!("not a relation: {other:?}"),
        }
    }

    #[test]
    fn relation_default_is_empty() {
        let vars = variables("var r: relation { a: boolean, b: integer };");
        assert_eq!(
            vars[0].ty.to_string(),
            "relation { a: boolean, b: integer }"
        );
        assert!(relation_value(&vars[0]).tuples.is_empty());
    }

    #[test]
    fn relation_selectors() {
        let vars = variables(
            "var a := relation { a: integer } { { a: 1 } };\nvar b := relation { tuple { a: 1 }, { a: 2 }, };\nvar c: relation { a: integer } := relation {};\nvar d := relation {} {};\nvar e := tuple { a: integer } { a: 1 };",
        );
        assert_eq!(vars[0].ty.to_string(), "relation { a: integer }");
        assert_eq!(relation_value(&vars[0]).tuples.len(), 1);
        assert_eq!(vars[1].ty, vars[0].ty);
        assert_eq!(relation_value(&vars[1]).tuples.len(), 2);
        assert_eq!(vars[2].ty, vars[0].ty);
        assert!(relation_value(&vars[2]).tuples.is_empty());
        assert_eq!(vars[3].ty.to_string(), "relation {}");
        assert_eq!(vars[4].ty.to_string(), "tuple { a: integer }");
    }

    #[test]
    fn empty_relation_needs_a_heading() {
        let errors = errors(&format!("var r := relation {{}};{MAIN}"));
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].code, Code::UnknownRelationHeading);
    }

    #[test]
    fn duplicate_tuples() {
        let source = format!("var r := relation {{ {{ a: 1, b: 2 }}, {{ b: 2, a: 1 }} }};{MAIN}");
        let errors = errors(&source);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].code, Code::DuplicateTuple);
        assert_eq!(
            fix(&source),
            format!("var r := relation {{ {{ a: 1, b: 2 }} }};{MAIN}")
        );
    }

    #[test]
    fn inferred_heading_mismatch() {
        let source = format!("var r := relation {{ {{ a: 1 }}, {{ a: \"x\" }} }};{MAIN}");
        let errors = errors(&source);
        assert_eq!(errors.len(), 1, "{errors:#?}");
        assert_eq!(errors[0].code, Code::TypeMismatch);
        assert_eq!(
            &source[errors[0].span().start..errors[0].span().end],
            "\"x\""
        );
        let origin = &errors[0].secondary[0];
        assert_eq!(&source[origin.span.start..origin.span.end], "1");
        assert_eq!(origin.message, "expected because of this value");
        assert!(
            errors[0]
                .suggestions
                .iter()
                .all(|s| !s.message.starts_with("change the type")),
            "no written type to change"
        );
    }

    #[test]
    fn selector_heading_checks_its_tuples() {
        let source = format!("var t := tuple {{ a: integer }} {{ a: \"x\" }};{MAIN}");
        let errors = errors(&source);
        assert_eq!(errors.len(), 1, "{errors:#?}");
        let origin = errors[0].secondary[0].span;
        assert_eq!(&source[origin.start..origin.end], "integer");
        assert_eq!(
            fix(&source),
            format!("var t := tuple {{ a: text }} {{ a: \"x\" }};{MAIN}")
        );
    }

    #[test]
    fn selector_heading_against_declared_type() {
        let errors = errors(&format!(
            "var r: relation {{ a: integer }} := relation {{ a: text }} {{}};{MAIN}"
        ));
        assert_eq!(errors.len(), 1);
        assert_eq!(
            errors[0].message,
            "mismatched types: expected `relation { a: integer }`, found `relation { a: text }`"
        );
    }

    #[test]
    fn missing_attribute_in_a_relation_tuple() {
        let source = format!(
            "var r: relation {{ a: integer, b: text }} := relation {{ {{ a: 1 }} }};{MAIN}"
        );
        assert_eq!(
            fix(&source),
            format!(
                "var r: relation {{ a: integer, b: text }} := relation {{ {{ a: 1, b: \"\" }} }};{MAIN}"
            )
        );
    }

    #[test]
    fn nested_relations() {
        let vars = variables(
            "var n: tuple { r: relation { x: rational } };\nvar m := tuple { r: relation { { x: 1.0 } } };\nvar rr := relation { { inner: relation { { x: 1 } } } };",
        );
        assert_eq!(vars[0].ty, vars[1].ty);
        assert_eq!(
            vars[2].ty.to_string(),
            "relation { inner: relation { x: integer } }"
        );
    }

    #[test]
    fn relation_valued_attributes_compare_as_sets() {
        // The inner relations list the same tuples in another order, so the
        // outer tuples are equal: a duplicate.
        let source = format!(
            "var r := relation {{\n    {{ k: 1, inner: relation {{ {{ x: 1 }}, {{ x: 2 }} }} }},\n    {{ k: 1, inner: relation {{ {{ x: 2 }}, {{ x: 1 }} }} }},\n}};{MAIN}"
        );
        let errors = errors(&source);
        assert_eq!(errors.len(), 1, "{errors:#?}");
        assert_eq!(errors[0].code, Code::DuplicateTuple);

        // Different inner relations make different outer tuples.
        let vars = variables(
            "var r := relation {\n    { k: 1, inner: relation { { x: 1 } } },\n    { k: 1, inner: relation { { x: 2 } } },\n};",
        );
        assert_eq!(relation_value(&vars[0]).tuples.len(), 2);
    }

    #[test]
    fn errors_inside_a_relation_valued_attribute() {
        let source = format!(
            "var r: relation {{ inner: relation {{ x: integer }} }} := relation {{ {{ inner: relation {{ {{ x: \"a\" }} }} }} }};{MAIN}"
        );
        let found = errors(&source);
        assert_eq!(found.len(), 1, "{found:#?}");
        assert_eq!(found[0].code, Code::TypeMismatch);
        let at = found[0].span();
        assert_eq!(&source[at.start..at.end], "\"a\"");
        let origin = found[0].secondary[0].span;
        assert_eq!(
            &source[origin.start..origin.end],
            "integer",
            "the inner heading's type"
        );

        // An inner relation's tuples must agree with each other too.
        let source = format!(
            "var r := relation {{ {{ inner: relation {{ {{ x: 1 }}, {{ y: 2 }} }} }} }};{MAIN}"
        );
        // One error: `y` is one edit from the missing `x`, so the fix renames
        // it; and the outer relation, whose only tuple is invalid, doesn't
        // also report an unknown heading.
        let found = errors(&source);
        assert_eq!(found.len(), 1, "{found:#?}");
        assert_eq!(found[0].code, Code::UnknownAttribute);
        assert_eq!(found[0].suggestions[0].message, "rename it to `x`");
    }

    #[test]
    fn empty_inner_relation_takes_its_heading_from_the_first_tuple() {
        let vars = variables(
            "var teams := relation {\n    { members: relation { { name: \"Ada\" } } },\n    { members: relation {} },\n};",
        );
        assert_eq!(
            vars[0].ty.to_string(),
            "relation { members: relation { name: text } }"
        );
        assert_eq!(relation_value(&vars[0]).tuples.len(), 2);
    }

    #[test]
    fn missing_relation_attribute_gets_an_empty_relation() {
        let source = format!(
            "var t: tuple {{ a: integer, r: relation {{ x: integer }} }} := tuple {{ a: 1 }};{MAIN}"
        );
        assert_eq!(
            fix(&source),
            format!(
                "var t: tuple {{ a: integer, r: relation {{ x: integer }} }} := tuple {{ a: 1, r: relation {{ x: integer }} {{}} }};{MAIN}"
            )
        );
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
