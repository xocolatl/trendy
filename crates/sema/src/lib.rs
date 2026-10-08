mod body;
mod errors;
mod prelude;
mod types;

use std::collections::HashMap;

use trendy_diag::{Diagnostic, Span};
use trendy_hir as hir;
use trendy_syntax::SyntaxToken;
use trendy_syntax::ast::{AstNode, Item, SourceFile, VarDecl};

pub use prelude::Prelude;

/// Check a parsed program and translate it to HIR. The prelude's built-in
/// operators are in scope.
///
/// This is the last layer allowed to report errors to the user. Drivers run
/// it only on trees without syntax errors; parts that are missing anyway are
/// skipped, never guessed at.
///
/// # Errors
///
/// Returns every semantic error, in source order.
pub fn analyze(prelude: &Prelude, file: &SourceFile) -> Result<hir::Program, Vec<Diagnostic>> {
    let mut errors = Vec::new();
    let values = collect_names(prelude, file, &mut errors);

    // Variables first: their initial values are constants, and operator
    // bodies refer to them.
    let mut variables = Vec::new();
    let mut variable_ids = HashMap::new();
    for decl in file.variables() {
        if let Some(variable) = variable(&decl, &values, &mut errors) {
            variable_ids.insert(variable.name.clone(), hir::VariableId(variables.len()));
            variables.push(variable);
        }
    }

    // Every operator's signature before any body, so that a call can refer
    // to an operator defined after it.
    let mut operators = prelude.operators().to_vec();
    let mut signatures = HashMap::new();
    for (i, op) in operators.iter().enumerate() {
        signatures.insert(
            op.name.clone(),
            body::Signature::builtin(hir::OperatorId(i)),
        );
    }
    let mut user_operators = Vec::new();
    for item in file.items() {
        match item {
            Item::Operator(op) => {
                let Some(ident) = op.name().and_then(|n| n.ident()) else {
                    continue;
                };
                if !values.is_first(&ident) {
                    continue;
                }
                let id = hir::OperatorId(operators.len());
                let params = op
                    .params()
                    .and_then(|h| body::params(&h, &values, &mut errors));
                operators.push(hir::Operator {
                    name: ident.text().to_string(),
                    params: params.clone().unwrap_or_default(),
                    kind: hir::OperatorKind::User { body: Vec::new() },
                });
                signatures.insert(
                    ident.text().to_string(),
                    body::Signature::user(id, op.clone(), params.is_some()),
                );
                user_operators.push((op, id));
            }
            Item::Builtin(builtin) => errors.push(errors::builtin_outside_prelude(
                builtin.keyword(),
                builtin.syntax(),
            )),
            Item::BuiltinType(builtin) => errors.push(errors::builtin_outside_prelude(
                builtin.keyword(),
                builtin.syntax(),
            )),
            Item::Variable(_) => {}
        }
    }

    let context = body::Context {
        values: &values,
        operators: &operators,
        signatures: &signatures,
        variables: &variables,
        variable_ids: &variable_ids,
    };
    let mut bodies = Vec::new();
    for (op, id) in &user_operators {
        if let Some(statements) = body::body(op, *id, &context, &mut errors) {
            bodies.push((*id, statements));
        }
    }
    for (id, statements) in bodies {
        operators[id.0].kind = hir::OperatorKind::User { body: statements };
    }

    let entry = entry(file, &values, &operators, &user_operators, &mut errors);
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

/// The operator named `main`, which must be the program's own and take no
/// parameters.
fn entry(
    file: &SourceFile,
    values: &Values,
    operators: &[hir::Operator],
    user_operators: &[(trendy_syntax::ast::Operator, hir::OperatorId)],
    errors: &mut Vec<Diagnostic>,
) -> Option<hir::OperatorId> {
    let Some((op, id)) = user_operators
        .iter()
        .find(|(_, id)| operators[id.0].name == "main")
    else {
        errors.push(errors::missing_main(file, values));
        return None;
    };
    if let Some(params) = op.params().filter(|h| h.attributes().next().is_some()) {
        errors.push(errors::main_with_params(&params));
        return None;
    }
    Some(*id)
}

/// What a name in the value namespace refers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    Operator,
    Builtin,
    Variable,
}

impl Kind {
    pub(crate) fn describe(self) -> &'static str {
        match self {
            Kind::Operator => "an operator",
            Kind::Builtin => "a built-in operator",
            Kind::Variable => "a variable",
        }
    }
}

/// The value namespace, shared by operators and variables: each name's first
/// definition, and where it is in the program (built-ins are in the prelude,
/// so they have no span). Types have a namespace of their own.
pub(crate) struct Values(HashMap<String, (Kind, Option<Span>)>);

impl Values {
    pub(crate) fn get(&self, name: &str) -> Option<(Kind, Option<Span>)> {
        self.0.get(name).copied()
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = (&str, Kind, Option<Span>)> {
        self.0
            .iter()
            .map(|(name, &(kind, span))| (name.as_str(), kind, span))
    }

    /// Whether `ident` is its name's first definition, not a duplicate.
    fn is_first(&self, ident: &SyntaxToken) -> bool {
        self.get(ident.text())
            .is_some_and(|(_, span)| span == Some(trendy_syntax::span(ident.text_range())))
    }
}

/// Record every top-level name, the prelude's first, reporting each one
/// defined more than once.
fn collect_names(prelude: &Prelude, file: &SourceFile, errors: &mut Vec<Diagnostic>) -> Values {
    let mut values: HashMap<String, (Kind, Option<Span>)> = prelude
        .operators()
        .iter()
        .map(|op| (op.name.clone(), (Kind::Builtin, None)))
        .collect();
    for item in file.items() {
        let (kind, name) = match item {
            Item::Operator(op) => (Kind::Operator, op.name()),
            Item::Variable(decl) => (Kind::Variable, decl.name()),
            // Reported as being outside the prelude.
            Item::Builtin(_) | Item::BuiltinType(_) => continue,
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
                values.insert(ident.text().to_string(), (kind, Some(span)));
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

    fn parse(source: &str) -> SourceFile {
        let parse = trendy_parser::parse(source, trendy_lexer::lex(source));
        assert!(parse.errors.is_empty(), "{:?}", parse.errors);
        parse.tree
    }

    fn prelude() -> Prelude {
        Prelude::load(&parse(trendy_prelude::SOURCE)).unwrap()
    }

    fn analyze_str(source: &str) -> Result<hir::Program, Vec<Diagnostic>> {
        analyze(&prelude(), &parse(source))
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
        assert_eq!(program.operator(program.entry).name, "main");
        assert!(matches!(
            program.operators[0].kind,
            hir::OperatorKind::Builtin(hir::Builtin::DebugValue)
        ));
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
        assert_eq!(errors[0].secondary[0].message, "`x` is first given here");
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

    fn codes(source: &str) -> Vec<Code> {
        errors(source).iter().map(|e| e.code).collect()
    }

    #[test]
    fn the_prelude_loads() {
        let prelude = prelude();
        assert_eq!(prelude.operators()[0].name, "debug_value");
        assert_eq!(
            prelude.operators()[0].params,
            hir::Builtin::DebugValue.params()
        );
    }

    #[test]
    fn calls_with_parameters_and_variables() {
        let program = analyze_str(
            "var r := relation { { a: 1 } };\n\noperator greet{name: text, r: integer} [\n    call debug_value{v: name};\n    call debug_value{v: r};\n];\n\noperator main{} [\n    call debug_value{v: r};\n    call greet{r: 2, name: \"Ada\"};\n];",
        )
        .unwrap();
        let greet = program
            .operators
            .iter()
            .find(|o| o.name == "greet")
            .unwrap();
        let hir::OperatorKind::User { body } = &greet.kind else {
            panic!("greet is a user operator");
        };
        // Inside `greet`, `r` is the parameter, which hides the variable.
        let hir::Statement::Call { args, .. } = &body[1];
        assert_eq!(args["v"], hir::Expr::Parameter("r".to_string()));
        let main = program.operator(program.entry);
        let hir::OperatorKind::User { body } = &main.kind else {
            panic!("main is a user operator");
        };
        let hir::Statement::Call { args, .. } = &body[0];
        assert_eq!(args["v"], hir::Expr::Variable(hir::VariableId(0)));
        let hir::Statement::Call { args, .. } = &body[1];
        assert_eq!(
            args["name"],
            hir::Expr::Constant(hir::Value::Text("Ada".to_string()))
        );
    }

    #[test]
    fn calls_can_precede_definitions() {
        analyze_str("operator main{} [ call later{}; ];\noperator later{} [];").unwrap();
    }

    #[test]
    fn argument_errors() {
        let source = "operator greet{name: text} [];\noperator main{} [\n    call greet{nam: \"Ada\"};\n    call greet{};\n    call greet{name: 1};\n    call greet{\"Ada\"};\n];";
        assert_eq!(
            codes(source),
            [
                Code::UnknownAttribute,
                Code::MissingAttribute,
                Code::TypeMismatch,
                Code::PositionalArgument
            ]
        );
        assert_eq!(
            fix(source),
            "operator greet{name: text} [];\noperator main{} [\n    call greet{name: \"Ada\"};\n    call greet{ name: \"\"};\n    call greet{name: \"1\"};\n    call greet{name: \"Ada\"};\n];"
        );
        let errors = errors(source);
        assert_eq!(errors[1].message, "missing argument `name`");
        let origin = errors[2].secondary[0].span;
        assert_eq!(
            &source[origin.start..origin.end],
            "text",
            "the parameter's type"
        );
    }

    #[test]
    fn debug_value_takes_anything_but_needs_its_argument() {
        analyze_str(
            "operator main{} [ call debug_value{v: 1}; call debug_value{v: relation { { a: 1 } }}; ];",
        )
        .unwrap();
        let errors = errors("operator main{} [ call debug_value{}; ];");
        assert_eq!(errors[0].code, Code::MissingAttribute);
        assert!(errors[0].suggestions.is_empty(), "`any` has no default");
    }

    #[test]
    fn names() {
        let source = "var count := 1;\noperator main{} [\n    call debug_value{v: cout};\n    call debug_valu{v: 1};\n    call debug_value{v: main};\n    call count{};\n];";
        assert_eq!(
            codes(source),
            [
                Code::UnknownName,
                Code::UnknownName,
                Code::OperatorAsValue,
                Code::NotAnOperator
            ]
        );
        assert!(fix(source).contains("call debug_value{v: count};\n    call debug_value{v: 1};"));
    }

    #[test]
    fn builtins_and_any() {
        assert_eq!(
            codes("builtin operator show{v: integer};\noperator main{} [];"),
            [Code::BuiltinOutsidePrelude]
        );
        assert_eq!(
            codes("var x: any := 1;\noperator show{v: any} [];\noperator main{} [];"),
            [Code::AnyOutsideBuiltin, Code::AnyOutsideBuiltin]
        );
        // A variable may be named `any`; it's only reserved as a type.
        analyze_str("var any := 1;\noperator main{} [ call debug_value{v: any}; ];").unwrap();
        let errors = errors("operator debug_value{} [];\noperator main{} [];");
        assert_eq!(errors[0].code, Code::DuplicateName);
        assert!(errors[0].notes[0].contains("declared in the prelude"));
    }

    #[test]
    fn main_takes_no_parameters() {
        let source = "operator main{args: text} [];";
        assert_eq!(codes(source), [Code::MainWithParams]);
        assert_eq!(fix(source), "operator main{} [];");
    }

    #[test]
    fn names_in_constants() {
        assert_eq!(
            codes(
                "var a := 1;\nvar b := a;\noperator main{} [ call debug_value{v: tuple { x: a }}; ];"
            ),
            [Code::NameInConstant, Code::NameInConstant]
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
