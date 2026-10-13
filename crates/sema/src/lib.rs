mod body;
mod errors;
mod keys;
mod ops;
mod prelude;
mod types;

use std::collections::HashMap;

use trendy_diag::{Diagnostic, Span};
use trendy_hir as hir;
use trendy_syntax::SyntaxToken;
use trendy_syntax::ast::{AstNode, Item, NameRef, SourceFile, VarDecl};

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

    // Variables first: operator bodies refer to them.
    let variables = Globals::new(file, &values).evaluate(file, &mut errors);
    let variable_ids: HashMap<_, _> = variables
        .iter()
        .enumerate()
        .map(|(i, v)| (v.name.clone(), hir::VariableId(i)))
        .collect();

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

/// The program's variables, whose initial values are evaluated when
/// compiling. An initial value can refer to any variable, so each is
/// evaluated when first needed, after the ones it refers to.
struct Globals<'a> {
    values: &'a Values,
    /// The first declaration of each variable.
    decls: HashMap<String, VarDecl>,
    states: HashMap<String, State>,
    /// The variables being evaluated, innermost last: each one's name, its
    /// declaration's name span, and where its initial value refers to the
    /// next.
    stack: Vec<(String, Span, Span)>,
}

enum State {
    Evaluating,
    /// `None` after reporting errors.
    Done(Option<hir::Variable>),
}

impl<'a> Globals<'a> {
    fn new(file: &SourceFile, values: &'a Values) -> Self {
        let decls = file
            .variables()
            .filter_map(|decl| {
                let ident = decl.name()?.ident()?;
                values
                    .is_first(&ident)
                    .then(|| (ident.text().to_string(), decl))
            })
            .collect();
        Globals {
            values,
            decls,
            states: HashMap::new(),
            stack: Vec::new(),
        }
    }

    /// Every valid variable, in source order.
    fn evaluate(mut self, file: &SourceFile, errors: &mut Vec<Diagnostic>) -> Vec<hir::Variable> {
        let mut variables = Vec::new();
        for decl in file.variables() {
            let Some(ident) = decl.name().and_then(|n| n.ident()) else {
                continue;
            };
            if self.values.is_first(&ident) {
                variables.extend(self.variable(ident.text(), errors));
            } else {
                // A duplicate, already reported, but it can have errors of
                // its own.
                variable(&decl, &mut self, errors);
            }
        }
        variables
    }

    /// The first declared variable named `name`, evaluated if it isn't yet.
    fn variable(&mut self, name: &str, errors: &mut Vec<Diagnostic>) -> Option<hir::Variable> {
        match self.states.get(name) {
            Some(State::Done(variable)) => return variable.clone(),
            Some(State::Evaluating) => unreachable!("cycles are caught at the reference"),
            None => {}
        }
        let decl = self.decls.get(name)?.clone();
        let declared = trendy_syntax::span(decl.name()?.ident()?.text_range());
        self.states.insert(name.to_string(), State::Evaluating);
        self.stack.push((name.to_string(), declared, declared));
        let variable = variable(&decl, self, errors);
        self.stack.pop();
        self.states
            .insert(name.to_string(), State::Done(variable.clone()));
        variable
    }
}

impl types::Scope for Globals<'_> {
    fn values(&self) -> &Values {
        self.values
    }

    fn constant(&mut self, name_ref: &NameRef, errors: &mut Vec<Diagnostic>) -> Option<hir::Value> {
        let ident = name_ref.ident()?;
        let name = ident.text();
        let span = trendy_syntax::span(ident.text_range());
        match self.values.get(name) {
            Some((Kind::Variable, _)) => {}
            Some((Kind::Operator | Kind::Builtin, _)) => {
                errors.push(errors::operator_as_value(name, span));
                return None;
            }
            None => {
                let variables: Vec<&str> = self.decls.keys().map(String::as_str).collect();
                errors.push(errors::unknown_name(name, span, "value", &variables));
                return None;
            }
        }
        if let Some(State::Evaluating) = self.states.get(name) {
            let start = self.stack.iter().position(|(n, _, _)| n == name)?;
            let mut cycle = self.stack[start..].to_vec();
            if let Some(last) = cycle.last_mut() {
                last.2 = span;
            }
            errors.push(errors::cyclic_initial_values(&cycle));
            return None;
        }
        if let Some(innermost) = self.stack.last_mut() {
            innermost.2 = span;
        }
        Some(self.variable(name, errors)?.init)
    }
}

/// Check a variable declaration: at least one of its type and initial value
/// must be given, and they must agree.
fn variable(
    decl: &VarDecl,
    globals: &mut Globals,
    errors: &mut Vec<Diagnostic>,
) -> Option<hir::Variable> {
    let values = globals.values;
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
            let value = types::value(&expr, None, globals, errors)?;
            (value.ty(), value)
        }
        (Some((None, _)), Some(expr)) => {
            // The type is invalid, but the value can still have errors of its
            // own.
            types::value(&expr, None, globals, errors);
            return None;
        }
        (Some((Some(ty), type_expr)), Some(expr)) => {
            let expected = types::Expected {
                ty: ty.clone(),
                origin: types::Origin::Type(type_expr),
            };
            let value = types::value(&expr, Some(&expected), globals, errors)?;
            (ty, value)
        }
    };
    let keys = keys::keys(decl, ident.text(), &ty, &init, globals, errors)?;
    values.is_first(&ident).then(|| hir::Variable {
        name: ident.text().to_string(),
        ty,
        init,
        keys,
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
                    keys: Vec::new(),
                },
                hir::Variable {
                    name: "t".to_string(),
                    ty: hir::Type::Scalar(hir::Scalar::Text),
                    init: hir::Value::Text("a\tb".to_string()),
                    keys: Vec::new(),
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

    fn keys_of(source: &str) -> Vec<Vec<hir::Key>> {
        variables(source).into_iter().map(|v| v.keys).collect()
    }

    fn key(names: &[&str]) -> hir::Key {
        names.iter().map(ToString::to_string).collect()
    }

    #[test]
    fn keys() {
        assert_eq!(
            keys_of(
                "var r := relation { { a: 1, b: 2, c: 3 } }\n    key { a, b }\n    key { c, a };\nvar e: relation { a: integer }\n    key {};"
            ),
            [vec![key(&["a", "b"]), key(&["a", "c"])], vec![key(&[])]]
        );
    }

    #[test]
    fn without_a_key_the_whole_heading_is_one() {
        assert_eq!(
            keys_of("var r := relation { { a: 1, b: 2 } };\nvar t := tuple { a: 1 };\nvar n := 1;"),
            [vec![key(&["a", "b"])], vec![], vec![]]
        );
    }

    #[test]
    fn keys_are_relations_attributes() {
        let source = "var r := relation { { name: 1 } } key { nmae } key { name, name };";
        assert_eq!(
            codes(&format!("{source}{MAIN}")),
            [Code::UnknownAttribute, Code::DuplicateAttribute]
        );
        assert_eq!(
            fix(&format!(
                "var r := relation {{ {{ name: 1 }} }} key {{ nmae }};{MAIN}"
            )),
            format!("var r := relation {{ {{ name: 1 }} }} key {{ name }};{MAIN}")
        );
    }

    #[test]
    fn only_relations_have_keys() {
        let source = format!("var t := tuple {{ a: 1 }}\n    key {{ a }}\n    key {{}};{MAIN}");
        assert_eq!(codes(&source), [Code::KeyNotRelation]);
        assert_eq!(fix(&source), format!("var t := tuple {{ a: 1 }};{MAIN}"));
    }

    #[test]
    fn redundant_keys() {
        let declared_twice =
            format!("var r := relation {{ {{ a: 1, b: 2 }} }} key {{ a }} key {{ a }};{MAIN}");
        assert_eq!(messages(&declared_twice), ["key `{ a }` is declared twice"]);
        assert_eq!(
            fix(&declared_twice),
            format!("var r := relation {{ {{ a: 1, b: 2 }} }} key {{ a }};{MAIN}")
        );
        // A key that contains another is reported whichever comes first.
        let contains =
            format!("var r := relation {{ {{ a: 1, b: 2 }} }} key {{ b, a }} key {{ a }};{MAIN}");
        assert_eq!(
            messages(&contains),
            ["key `{ b, a }` contains key `{ a }`, so it isn't a key"]
        );
        assert_eq!(
            fix(&contains),
            format!("var r := relation {{ {{ a: 1, b: 2 }} }} key {{ a }};{MAIN}")
        );
        // Keys can share attributes.
        assert!(analyze_str(&format!(
            "var r := relation {{ {{ a: 1, b: 2, c: 3 }} }} key {{ a, b }} key {{ a, c }} key {{ b, c }};{MAIN}"
        ))
        .is_ok());
    }

    #[test]
    fn values_must_satisfy_their_keys() {
        let source = format!(
            "var r := relation {{\n    {{ a: 1, b: 2 }},\n    {{ a: 2, b: 2 }},\n    {{ a: 1, b: 3 }}\n}}\n    key {{ a }}\n    key {{ b }};{MAIN}"
        );
        let errors = errors(&source);
        assert_eq!(
            errors.iter().map(|e| e.code).collect::<Vec<_>>(),
            [Code::KeyViolated, Code::KeyViolated]
        );
        // Each at the later tuple, with the earlier one and the key.
        let text = |span: Span| &source[span.start..span.end];
        let places: Vec<(&str, Vec<&str>)> = errors
            .iter()
            .map(|e| {
                (
                    text(e.span()),
                    e.secondary.iter().map(|l| text(l.span)).collect(),
                )
            })
            .collect();
        assert_eq!(
            places,
            [
                ("{ a: 2, b: 2 }", vec!["{ a: 1, b: 2 }", "key { b }"]),
                ("{ a: 1, b: 3 }", vec!["{ a: 1, b: 2 }", "key { a }"]),
            ]
        );
    }

    #[test]
    fn computed_values_must_satisfy_their_keys() {
        let source = format!(
            "var r := relation {{ {{ a: 1, b: 2 }}, {{ a: 1, b: 3 }} }};\nvar s := r{{a, b}} key {{ a }};{MAIN}"
        );
        let errors = errors(&source);
        assert_eq!(errors.len(), 1);
        assert_eq!(
            &source[errors[0].span().start..errors[0].span().end],
            "r{a, b}"
        );
        assert_eq!(errors[0].primary.message, "has two tuples with `a: 1`");
    }

    #[test]
    fn the_empty_key_allows_one_tuple() {
        assert!(
            analyze_str(&format!(
                "var r := relation {{ {{ a: 1 }} }} key {{}};{MAIN}"
            ))
            .is_ok()
        );
        assert_eq!(
            codes(&format!(
                "var r := relation {{ {{ a: 1 }}, {{ a: 2 }} }} key {{}};{MAIN}"
            )),
            [Code::KeyViolated]
        );
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
    fn selectors_in_bodies() {
        // Names in a selector are fine; an operator still isn't a value.
        assert_eq!(
            codes(
                "var a := 1;\noperator show{p: integer} [ call debug_value{v: tuple { x: a, y: p, z: main }}; ];\noperator main{} [];"
            ),
            [Code::OperatorAsValue]
        );
        let body = |source: &str| {
            let program = analyze_str(source).unwrap();
            let f = program.operators.iter().find(|o| o.name == "f").unwrap();
            let hir::OperatorKind::User { body } = &f.kind else {
                panic!("f is a user operator");
            };
            body.clone()
        };
        let statements = body(
            "operator f{p: integer} [\n    call debug_value{v: tuple { x: p, y: 1 }};\n    call debug_value{v: relation { { x: p }, { x: 1 } }};\n    call debug_value{v: tuple { x: 1 + 1 }};\n];\noperator main{} [];",
        );
        let p = || hir::Expr::Parameter("p".to_string());
        let one = || hir::Expr::Constant(hir::Value::Integer(1));
        assert_eq!(
            argument(&statements[0]),
            &hir::Expr::Tuple([("x".to_string(), p()), ("y".to_string(), one())].into())
        );
        let hir::Expr::Relation(heading, tuples) = argument(&statements[1]) else {
            panic!("a relation built when running");
        };
        assert_eq!(heading.to_string(), "{ x: integer }");
        assert_eq!(tuples.len(), 2);
        assert!(
            matches!(argument(&statements[2]), hir::Expr::Constant(_)),
            "a selector of constants is folded"
        );
        // Tuples known when compiling must still be distinct; others may
        // turn out equal, and become one.
        assert_eq!(
            codes(
                "operator f{p: integer} [ call debug_value{v: relation { { x: 1 }, { x: p }, { x: 1 } }}; ];\noperator main{} [];"
            ),
            [Code::DuplicateTuple]
        );
        analyze_str("operator f{p: integer, q: integer} [ call debug_value{v: relation { { x: p }, { x: q } }}; ];\noperator main{} [];").unwrap();
        // The checks are the same as for constants.
        assert_eq!(
            codes(
                "operator f{p: integer} [ call debug_value{v: relation { { x: p }, { y: p } }}; ];\noperator main{} [];"
            ),
            [Code::UnknownAttribute]
        );
    }

    fn init(source: &str, name: &str) -> hir::Value {
        variables(source)
            .into_iter()
            .find(|v| v.name == name)
            .unwrap()
            .init
    }

    #[test]
    fn initial_values_are_evaluated() {
        let t = hir::Value::Boolean(true);
        let f = hir::Value::Boolean(false);
        assert_eq!(init("var a := not false;", "a"), t);
        assert_eq!(init("var a := true and false;", "a"), f);
        assert_eq!(init("var a := true xor true;", "a"), f);
        assert_eq!(init("var a := true implies false;", "a"), f);
        assert_eq!(init("var a := false implies false;", "a"), t);
        // `not` binds tightest, then `and`, `xor`, `or`, `implies`.
        assert_eq!(init("var a := not true or true;", "a"), t);
        assert_eq!(init("var a := true or true and false;", "a"), t);
        assert_eq!(init("var a := (true or true) and false;", "a"), f);
        assert_eq!(init("var a := true xor true or true;", "a"), t);
        assert_eq!(init("var a := false implies false implies false;", "a"), t);
        assert_eq!(
            init("var a := (false implies false) implies false;", "a"),
            f
        );
    }

    #[test]
    fn initial_values_refer_to_variables_in_any_order() {
        let vars = variables(
            "var u := t or false;\nvar t: boolean := not s;\nvar s := true;\nvar p := tuple { a: s, r: relation { { b: u } } };",
        );
        let names: Vec<_> = vars.iter().map(|v| v.name.as_str()).collect();
        assert_eq!(names, ["u", "t", "s", "p"], "in source order");
        assert_eq!(vars[0].init, hir::Value::Boolean(false));
        assert_eq!(
            vars[3].ty.to_string(),
            "tuple { a: boolean, r: relation { b: boolean } }"
        );
    }

    #[test]
    fn cyclic_initial_values() {
        let source =
            format!("var a := b;\nvar b := not c;\nvar c := a and true;\nvar d := a;{MAIN}");
        let errors = errors(&source);
        assert_eq!(errors.len(), 1, "one error per cycle: {errors:#?}");
        assert_eq!(errors[0].code, Code::CyclicInitialValues);
        assert_eq!(
            errors[0].message,
            "the initial values of `a`, `b` and `c` depend on each other"
        );
        assert_eq!(errors[0].span(), Span::new(4, 5));
        let labels: Vec<_> = errors[0]
            .secondary
            .iter()
            .map(|l| (&source[l.span.start..l.span.end], &l.message[..]))
            .collect();
        assert_eq!(
            labels,
            [
                ("b", "`a` refers to `b`"),
                ("c", "`b` refers to `c`"),
                ("a", "`c` refers to `a`")
            ]
        );
        let errors = self::errors(&format!("var a := not a;{MAIN}"));
        assert_eq!(
            errors[0].message,
            "the initial value of `a` depends on itself"
        );
        assert_eq!(errors[0].secondary[0].message, "`a` refers to itself");
    }

    #[test]
    fn names_in_initial_values() {
        let source = format!("var count := true;\nvar a := not cout;\nvar b := main;{MAIN}");
        assert_eq!(codes(&source), [Code::UnknownName, Code::OperatorAsValue]);
        assert!(fix(&source).contains("var a := not count;"));
    }

    #[test]
    fn operands_must_be_boolean() {
        let source = format!(
            "var i := 1;\nvar a := not i;\nvar b := true and 0;\nvar c: integer := true or false;{MAIN}"
        );
        let errors = errors(&source);
        assert_eq!(
            errors.iter().map(|e| &e.message[..]).collect::<Vec<_>>(),
            [
                "mismatched types: expected `boolean`, found `integer`",
                "mismatched types: expected `boolean`, found `integer`",
                "mismatched types: expected `integer`, found `boolean`",
            ]
        );
        let origin = &errors[0].secondary[0];
        assert_eq!(&source[origin.span.start..origin.span.end], "not");
        assert_eq!(origin.message, "`not` takes a boolean operand");
        assert_eq!(
            fix(&source),
            format!(
                "var i := 1;\nvar a := not i;\nvar b := true and false;\nvar c: boolean := true or false;{MAIN}"
            ),
            "`0` is rewritten as `false`; nothing to fix on a name"
        );
    }

    fn main_body(source: &str) -> Vec<hir::Statement> {
        let program = analyze_str(source).unwrap();
        let hir::OperatorKind::User { body } = &program.operator(program.entry).kind else {
            panic!("main is a user operator");
        };
        body.clone()
    }

    fn argument(statement: &hir::Statement) -> &hir::Expr {
        let hir::Statement::Call { args, .. } = statement;
        &args["v"]
    }

    #[test]
    fn constant_operands_simplify() {
        let cases = [
            ("true and p", "p"),
            ("p and true", "p"),
            ("false and p", "false"),
            ("p and false", "false"),
            ("false or p", "p"),
            ("p or false", "p"),
            ("true or p", "true"),
            ("p or true", "true"),
            ("false xor p", "p"),
            ("p xor false", "p"),
            ("true xor p", "not p"),
            ("p xor true", "not p"),
            ("true implies p", "p"),
            ("false implies p", "true"),
            ("p implies true", "true"),
            ("p implies false", "not p"),
            ("not not p", "p"),
            ("not (p xor true)", "p"),
            ("p and q", "p and q"),
        ];
        for (written, simplified) in cases {
            let source = |e: &str| {
                format!(
                    "operator f{{p: boolean, q: boolean}} [ call debug_value{{v: {e}}}; ];\noperator main{{}} [];"
                )
            };
            let body = |e: &str| {
                let program = analyze_str(&source(e)).unwrap();
                let f = program.operators.iter().find(|o| o.name == "f").unwrap();
                let hir::OperatorKind::User { body } = &f.kind else {
                    panic!("f is a user operator");
                };
                argument(&body[0]).clone()
            };
            assert_eq!(body(written), body(simplified), "{written}");
        }
    }

    #[test]
    fn integer_initial_values() {
        let int = hir::Value::Integer;
        assert_eq!(init("var a := 1 + 2 * 3;", "a"), int(7));
        assert_eq!(init("var a := (1 + 2) * 3;", "a"), int(9));
        assert_eq!(init("var a := 10 - 3 - 2;", "a"), int(5));
        assert_eq!(init("var a := -7 div 2;", "a"), int(-3));
        assert_eq!(init("var a := -7 mod 2;", "a"), int(-1));
        assert_eq!(init("var a := 7 mod -2;", "a"), int(1));
        assert_eq!(init("var a := - -3 + +4;", "a"), int(7));
        assert_eq!(init("var a := b * 2;\nvar b := 21;", "a"), int(42));
        assert_eq!(init("var a := -9223372036854775808;", "a"), int(i64::MIN));
        assert_eq!(
            init("var a := -1.5;", "a"),
            hir::Value::Rational(hir::Rational::new(-1.5))
        );
        assert_eq!(
            init("var a := -0.0;", "a"),
            hir::Value::Rational(hir::Rational::new(-0.0))
        );
    }

    #[test]
    fn integer_failures_when_compiling() {
        let source = format!(
            "var a := 9223372036854775807 + 1;\nvar b := -(-9223372036854775808);\nvar c := 1 div (2 - 2);\nvar d := -9223372036854775809;{MAIN}"
        );
        let errors = errors(&source);
        let found: Vec<_> = errors
            .iter()
            .map(|e| (e.code, &source[e.span().start..e.span().end]))
            .collect();
        assert_eq!(
            found,
            [
                (Code::Overflow, "9223372036854775807 + 1"),
                (Code::Overflow, "-(-9223372036854775808)"),
                (Code::DivisionByZero, "(2 - 2)"),
                (Code::IntegerOutOfRange, "-9223372036854775809"),
            ]
        );
    }

    #[test]
    fn integer_operand_types() {
        let source = format!(
            "var a := 1 + true;\nvar b := -\"x\";\nvar c: boolean := 1 * 2;\nvar d := not 1;\nvar e := 1.5 + 1;\nvar r: rational := -1;{MAIN}"
        );
        let errors = errors(&source);
        let labels: Vec<_> = errors
            .iter()
            .map(|e| e.secondary.first().map_or("", |l| &l.message[..]))
            .collect();
        assert_eq!(
            labels,
            [
                // The left operand sets the type.
                "expected because of this value",
                // `-` doesn't take text: reported at the operator.
                "",
                "expected because of this type",
                "`not` takes a boolean operand",
                "expected because of this value",
                "expected because of this type",
            ]
        );
        let fixed = fix(&source);
        assert!(fixed.contains("var e := 1.5 + 1.0;"), "{fixed}");
        assert!(
            fixed.ends_with(&format!("var r: rational := -1.0;{MAIN}")),
            "a negative literal is rewritten like any other"
        );
    }

    #[test]
    fn rational_initial_values() {
        let r = |x| hir::Value::Rational(hir::Rational::new(x));
        assert_eq!(init("var a := 1.5 + 2.25 * 2.0;", "a"), r(6.0));
        assert_eq!(init("var a := 1.0 / 4.0 - 0.5;", "a"), r(-0.25));
        assert_eq!(init("var a := -(0.0);", "a"), r(-0.0));
        assert_eq!(init("var a := +2.5;", "a"), r(2.5));
        assert_eq!(init("var a := b / 2.0;\nvar b := 3.0;", "a"), r(1.5));
        // The context sets the operand type, so the literal is what's wrong.
        let source = format!("var a: rational := 1 + 2;{MAIN}");
        assert_eq!(fix(&source), format!("var a: rational := 1.0 + 2.0;{MAIN}"));
    }

    #[test]
    fn rational_failures_when_compiling() {
        let source = format!(
            "var a := 1e300 * 1e300;\nvar b := 1.0 / 0.0;\nvar c := 1.0 / -0.0;\nvar d := -1e308 - 1e308;{MAIN}"
        );
        let errors = errors(&source);
        let found: Vec<_> = errors
            .iter()
            .map(|e| {
                (
                    e.code,
                    &e.message[..],
                    &source[e.span().start..e.span().end],
                )
            })
            .collect();
        assert_eq!(
            found,
            [
                (Code::Overflow, "rational overflow", "1e300 * 1e300"),
                (Code::DivisionByZero, "division by zero", "0.0"),
                (Code::DivisionByZero, "division by zero", "-0.0"),
                (Code::Overflow, "rational overflow", "-1e308 - 1e308"),
            ]
        );
    }

    #[test]
    fn division_operators_by_type() {
        let source = format!(
            "var a := 7 / 2;\nvar b := 7.0 div 2.0;\nvar c := 7.0 mod 2.0;\nvar d: integer := 1/2;{MAIN}"
        );
        let errors = errors(&source);
        let found: Vec<_> = errors
            .iter()
            .map(|e| {
                (
                    e.code,
                    &e.message[..],
                    &source[e.span().start..e.span().end],
                )
            })
            .collect();
        assert_eq!(
            found,
            [
                (
                    Code::WrongOperandType,
                    "`/` can't be applied to integers",
                    "/"
                ),
                (
                    Code::WrongOperandType,
                    "`div` can't be applied to rationals",
                    "div"
                ),
                (
                    Code::WrongOperandType,
                    "`mod` can't be applied to rationals",
                    "mod"
                ),
                (
                    Code::WrongOperandType,
                    "`/` can't be applied to integers",
                    "/"
                ),
            ]
        );
        assert_eq!(
            fix(&source),
            format!(
                "var a := 7 div 2;\nvar b := 7.0 / 2.0;\nvar c := 7.0 mod 2.0;\nvar d: integer := 1 div 2;{MAIN}"
            )
        );
        assert_eq!(
            codes(&format!("var a := 1 + 1.5;\nvar b := true + 1;{MAIN}")),
            [Code::TypeMismatch, Code::WrongOperandType]
        );
    }

    #[test]
    fn rational_simplification() {
        let cases = [
            ("x * 1.0", "x"),
            ("1.0 * x", "x"),
            ("x / 1.0", "x"),
            ("x - 0.0", "x"),
            ("x + -0.0", "x"),
            ("-0.0 + x", "x"),
            ("-0.0 - x", "-x"),
            ("x * -1.0", "-x"),
            ("x / -1.0", "-x"),
            ("- -x", "x"),
            ("+x", "x"),
            // IEEE 754 signed zeros: these aren't the identity.
            ("x + 0.0", "x + 0.0"),
            ("0.0 - x", "0.0 - x"),
            ("x * 0.0", "x * 0.0"),
        ];
        for (written, simplified) in cases {
            let source = |e: &str| {
                format!(
                    "operator f{{x: rational}} [ call debug_value{{v: {e}}}; ];\noperator main{{}} [];"
                )
            };
            let body = |e: &str| {
                let program = analyze_str(&source(e)).unwrap();
                let f = program.operators.iter().find(|o| o.name == "f").unwrap();
                let hir::OperatorKind::User { body } = &f.kind else {
                    panic!("f is a user operator");
                };
                argument(&body[0]).clone()
            };
            assert_eq!(body(written), body(simplified), "{written}");
        }
        let x = || Box::new(hir::Expr::Parameter("x".to_string()));
        let program = analyze_str(
            "operator f{x: rational} [ call debug_value{v: x + 0.0}; ];\noperator main{} [];",
        )
        .unwrap();
        let f = program.operators.iter().find(|o| o.name == "f").unwrap();
        let hir::OperatorKind::User { body } = &f.kind else {
            panic!("f is a user operator");
        };
        assert_eq!(
            argument(&body[0]),
            &hir::Expr::Binary(
                hir::BinaryOp::Add(hir::Numeric::Rational),
                x(),
                Box::new(hir::Expr::Constant(hir::Value::Rational(
                    hir::Rational::new(0.0)
                )))
            )
        );
        assert_eq!(
            codes(
                "operator f{x: rational} [ call debug_value{v: x / 0.0}; ];\noperator main{} [];"
            ),
            [Code::DivisionByZero]
        );
    }

    #[test]
    fn concatenation() {
        let t = |s: &str| hir::Value::Text(s.to_string());
        assert_eq!(init("var a := \"ab\" || \"c\" || \"\";", "a"), t("abc"));
        assert_eq!(
            init("var a := b || \"!\";\nvar b := \"hé\";", "a"),
            t("hé!")
        );
        let source = format!(
            "var a := true || false;\nvar b := \"a\" + \"b\";\nvar c := \"a\" || 1;\nvar d: text := 1 || 2;\nvar e := tuple {{}} || \"\";{MAIN}"
        );
        let errors = errors(&source);
        let found: Vec<_> = errors.iter().map(|e| (e.code, &e.message[..])).collect();
        assert_eq!(
            found,
            [
                (Code::WrongOperandType, "`||` can't be applied to booleans"),
                (Code::WrongOperandType, "`+` can't be applied to text"),
                (
                    Code::TypeMismatch,
                    "mismatched types: expected `text`, found `integer`"
                ),
                (
                    Code::TypeMismatch,
                    "mismatched types: expected `text`, found `integer`"
                ),
                (
                    Code::TypeMismatch,
                    "mismatched types: expected `text`, found `integer`"
                ),
                (Code::WrongOperandType, "`||` can't be applied to tuples"),
            ]
        );
        assert_eq!(
            fix(&source),
            format!(
                "var a := true or false;\nvar b := \"a\" || \"b\";\nvar c := \"a\" || \"1\";\nvar d: text := \"1\" || \"2\";\nvar e := tuple {{}} || \"\";{MAIN}"
            )
        );
    }

    #[test]
    fn concatenation_simplification() {
        let cases = [
            ("t || \"\"", "t"),
            ("\"\" || t", "t"),
            ("\"a\" || \"b\" || t", "\"ab\" || t"),
            ("t || \"a\" || \"b\"", "t || \"a\" || \"b\""),
        ];
        for (written, simplified) in cases {
            let source = |e: &str| {
                format!(
                    "operator f{{t: text}} [ call debug_value{{v: {e}}}; ];\noperator main{{}} [];"
                )
            };
            let body = |e: &str| {
                let program = analyze_str(&source(e)).unwrap();
                let f = program.operators.iter().find(|o| o.name == "f").unwrap();
                let hir::OperatorKind::User { body } = &f.kind else {
                    panic!("f is a user operator");
                };
                argument(&body[0]).clone()
            };
            assert_eq!(body(written), body(simplified), "{written}");
        }
    }

    #[test]
    fn comparison_initial_values() {
        let yes = hir::Value::Boolean(true);
        let no = hir::Value::Boolean(false);
        assert_eq!(init("var a := false < true;", "a"), yes);
        assert_eq!(init("var a := 1 + 1 = 2;", "a"), yes);
        assert_eq!(init("var a := -0.0 < 0.0;", "a"), yes);
        assert_eq!(init("var a := -0.0 = 0.0;", "a"), no);
        assert_eq!(init("var a := \"Z\" < \"a\";", "a"), yes);
        assert_eq!(init("var a := \"é\" > \"z\";", "a"), yes);
        assert_eq!(init("var a := not (1 >= 2);", "a"), yes);
        // `not` binds tightest: these are `(not false) = true`, and so on.
        assert_eq!(init("var a := not false = true;", "a"), yes);
        assert_eq!(init("var a := not true < true;", "a"), yes);
        assert_eq!(init("var a := (1 < 2) = (3 < 4);", "a"), yes);
        assert_eq!(init("var a := 1 <> 1 or 2 <= 1;", "a"), no);
    }

    #[test]
    fn comparison_operand_types() {
        let source = format!(
            "var a := 1 = 1.0;\nvar b: integer := 1 < 2;\nvar c := tuple {{}} < tuple {{}};\nvar d: boolean := 1 = 2;{MAIN}"
        );
        let errors = errors(&source);
        let found: Vec<_> = errors.iter().map(|e| (e.code, &e.message[..])).collect();
        assert_eq!(
            found,
            [
                (
                    Code::TypeMismatch,
                    "mismatched types: expected `integer`, found `rational`"
                ),
                (
                    Code::TypeMismatch,
                    "mismatched types: expected `integer`, found `boolean`"
                ),
                (Code::WrongOperandType, "`<` can't be applied to tuples"),
            ],
            "the expected boolean result doesn't make the operands booleans"
        );
    }

    #[test]
    fn tuple_and_relation_equality() {
        let yes = hir::Value::Boolean(true);
        let no = hir::Value::Boolean(false);
        assert_eq!(
            init(
                "var a := tuple { x: 1, y: \"a\" } = tuple { y: \"a\", x: 1 };",
                "a"
            ),
            yes
        );
        assert_eq!(init("var a := tuple { x: 1 } <> tuple { x: 2 };", "a"), yes);
        assert_eq!(init("var a := tuple {} = tuple {};", "a"), yes);
        assert_eq!(
            init(
                "var a := relation { { x: 1 }, { x: 2 } } = relation { { x: 2 }, { x: 1 } };",
                "a"
            ),
            yes,
            "relations are sets"
        );
        // The right operand's heading comes from the left one.
        assert_eq!(
            init("var a := relation { { x: 1 } } = relation {};", "a"),
            no
        );
        assert_eq!(
            init(
                "var a := tuple { r: relation { { x: -0.0 } } } = tuple { r: relation { { x: 0.0 } } };",
                "a"
            ),
            no,
            "nested values compare as values"
        );
        let source = format!(
            "var a := tuple {{ x: 1 }} = tuple {{ x: 1, y: 2 }};\nvar b := tuple {{ x: 1 }} = relation {{ {{ x: 1 }} }};\nvar c := relation {{ {{ x: 1 }} }} >= relation {{}};{MAIN}"
        );
        let errors = errors(&source);
        let found: Vec<_> = errors.iter().map(|e| (e.code, &e.message[..])).collect();
        assert_eq!(
            found,
            [
                (Code::UnknownAttribute, "unknown attribute `y`"),
                (
                    Code::TypeMismatch,
                    "mismatched types: expected `tuple { x: integer }`, found `relation { x: integer }`"
                ),
                (Code::WrongOperandType, "`>=` can't be applied to relations"),
            ]
        );
        assert!(
            errors[2]
                .notes
                .iter()
                .any(|n| n.contains("only be compared with `=` and `<>`"))
        );
    }

    #[test]
    fn tuple_equality_in_bodies() {
        let body = main_body(
            "var t := tuple { x: 1 };\nvar r := relation { { x: 1 } };\noperator main{} [\n    call debug_value{v: t = tuple { x: 1 }};\n    call debug_value{v: r <> relation {}};\n];",
        );
        let t = || Box::new(hir::Expr::Variable(hir::VariableId(0)));
        let hir::Expr::Binary(op, lhs, _) = argument(&body[0]) else {
            panic!("a comparison");
        };
        assert_eq!(
            *op,
            hir::BinaryOp::Compare(hir::Comparison::Equal, hir::Compared::Tuple)
        );
        assert_eq!(lhs, &t());
        let hir::Expr::Binary(op, ..) = argument(&body[1]) else {
            panic!("a comparison");
        };
        assert_eq!(
            *op,
            hir::BinaryOp::Compare(hir::Comparison::NotEqual, hir::Compared::Relation)
        );
    }

    #[test]
    fn project_and_remove_constants() {
        let vars = variables(
            "var p := relation { { name: \"Ada\", born: 1815 }, { name: \"Ada\", born: 1816 } };\nvar names := p{name};\nvar years := p remove {name};\nvar dee := p{};\nvar dum := relation { a: integer } {}{};\nvar t := tuple { a: 1, b: 2 } remove {b};\nvar same := p{born, name};",
        );
        let value = |name: &str| vars.iter().find(|v| v.name == name).unwrap();
        assert_eq!(value("names").ty.to_string(), "relation { name: text }");
        assert_eq!(
            relation_value(value("names")).tuples.len(),
            1,
            "duplicates dropped"
        );
        assert_eq!(relation_value(value("years")).tuples.len(), 2);
        assert_eq!(value("dee").ty.to_string(), "relation {}");
        assert_eq!(relation_value(value("dee")).tuples.len(), 1, "table_dee");
        assert!(relation_value(value("dum")).tuples.is_empty(), "table_dum");
        assert_eq!(value("t").ty.to_string(), "tuple { a: integer }");
        assert_eq!(value("same").init, value("p").init);
    }

    #[test]
    fn project_and_remove_errors() {
        let source = format!(
            "var t := tuple {{ name: \"Ada\", born: 1815 }};\nvar a := t{{nam}};\nvar b := t remove {{born, born}};\nvar c := t remove {{}};\nvar d := 1{{x}};\nvar e: tuple {{ born: integer }} := t{{name}};{MAIN}"
        );
        let errors = errors(&source);
        let found: Vec<_> = errors.iter().map(|e| (e.code, &e.message[..])).collect();
        assert_eq!(
            found,
            [
                (Code::UnknownAttribute, "unknown attribute `nam`"),
                (
                    Code::DuplicateAttribute,
                    "attribute `born` is given more than once"
                ),
                (Code::EmptyRemove, "`remove {}` removes nothing"),
                (Code::WrongOperandType, "integers can't be projected"),
                (
                    Code::TypeMismatch,
                    "mismatched types: expected `tuple { born: integer }`, found `tuple { name: text }`"
                ),
            ]
        );
        assert_eq!(errors[0].suggestions[0].message, "rename it to `name`");
        assert!(errors[0].notes[0].contains("`born` and `name`"));
        let fixed = fix(&source);
        assert!(fixed.contains("var a := t{name};"), "{fixed}");
        assert!(fixed.contains("var c := t;"), "{fixed}");
    }

    #[test]
    fn project_and_remove_in_bodies() {
        let body = main_body(
            "var t := tuple { a: 1, b: 2, c: 3 };\noperator main{} [\n    call debug_value{v: t remove {a} remove {b}};\n    call debug_value{v: t{a, b, c}};\n];",
        );
        let t = || Box::new(hir::Expr::Variable(hir::VariableId(0)));
        let names = |n: &[&str]| n.iter().map(|s| (*s).to_string()).collect();
        assert_eq!(
            argument(&body[0]),
            &hir::Expr::Remove(t(), names(&["a", "b"])),
            "removals merge"
        );
        assert_eq!(
            argument(&body[1]),
            &*t(),
            "projecting every attribute changes nothing"
        );
    }

    #[test]
    fn extraction() {
        let int = hir::Value::Integer;
        assert_eq!(
            init("var a := tuple { x: 1, y: tuple { z: 2 } }.y.z;", "a"),
            int(2)
        );
        assert_eq!(
            init("var a := t.x + 1;\nvar t := tuple { x: 41 };", "a"),
            int(42)
        );
        assert_eq!(
            init("var a := (tuple { x: 1 } rename {x as y}).y;", "a"),
            int(1)
        );
        let source = format!(
            "var t := tuple {{ postal_code: \"75001\" }};\nvar a := t.postal_cod;\nvar b := relation {{ {{ x: 1 }} }}.x;\nvar c := (1).x;\nvar d: integer := t.postal_code;{MAIN}"
        );
        let errors = errors(&source);
        let found: Vec<_> = errors.iter().map(|e| (e.code, &e.message[..])).collect();
        assert_eq!(
            found,
            [
                (Code::UnknownAttribute, "unknown attribute `postal_cod`"),
                (Code::WrongOperandType, "`.x` can't be applied to relations"),
                (Code::WrongOperandType, "`.x` can't be applied to integers"),
                (
                    Code::TypeMismatch,
                    "mismatched types: expected `integer`, found `text`"
                ),
            ]
        );
        assert_eq!(errors[0].suggestions.len(), 1, "rename it, no removal");
        assert!(fix(&source).contains("var a := t.postal_code;"));
        assert!(errors[1].help[0].contains("r{x}"));
    }

    #[test]
    fn extraction_in_bodies() {
        let body = |e: &str| {
            let program = analyze_str(&format!(
                "operator f{{t: tuple {{ a: integer, b: integer }}, n: integer}} [ call debug_value{{v: {e}}}; ];\noperator main{{}} [];"
            ))
            .unwrap();
            let f = program.operators.iter().find(|o| o.name == "f").unwrap();
            let hir::OperatorKind::User { body } = &f.kind else {
                panic!("f is a user operator");
            };
            argument(&body[0]).clone()
        };
        let t = || Box::new(hir::Expr::Parameter("t".to_string()));
        let n = || hir::Expr::Parameter("n".to_string());
        assert_eq!(body("t.a"), hir::Expr::Extract(t(), "a".to_string()));
        // Taken straight from what the tuple is made of.
        assert_eq!(body("tuple { x: n, y: 1 }.x"), n());
        assert_eq!(body("(t rename {a as z}).z"), body("t.a"));
        assert_eq!(body("(t remove {b}).a"), body("t.a"));
        // Unless that drops something that can fail.
        assert!(matches!(
            body("tuple { x: n, y: n + 1 }.x"),
            hir::Expr::Extract(..)
        ));
    }

    #[test]
    fn rename_constants() {
        let vars = variables(
            "var t := tuple { a: 1, b: \"x\" };\nvar moved := t rename {a as z};\nvar swapped := t rename {a as b, b as a};\nvar same := t rename {a as a};\nvar r := relation { { a: 2, b: 1 }, { a: 1, b: 2 } } rename {a as c};",
        );
        let value = |name: &str| vars.iter().find(|v| v.name == name).unwrap();
        assert_eq!(
            value("moved").ty.to_string(),
            "tuple { b: text, z: integer }"
        );
        assert_eq!(
            value("swapped").ty.to_string(),
            "tuple { a: text, b: integer }"
        );
        assert_eq!(value("same").init, value("t").init);
        assert_eq!(
            value("r").ty.to_string(),
            "relation { b: integer, c: integer }"
        );
    }

    #[test]
    fn rename_errors() {
        let source = format!(
            "var t := tuple {{ postal_code: \"75001\", city: \"Paris\" }};\nvar a := t rename {{postal_cod as code}};\nvar b := t rename {{code as postal_code}};\nvar c := t rename {{postal_code as p, postal_code as q}};\nvar d := t rename {{postal_code as city}};\nvar e := t rename {{postal_code as x, city as x}};\nvar f := t rename {{}};\nvar g := 1 rename {{a as b}};{MAIN}"
        );
        let errors = errors(&source);
        let found: Vec<_> = errors.iter().map(|e| (e.code, &e.message[..])).collect();
        assert_eq!(
            found,
            [
                (Code::UnknownAttribute, "unknown attribute `postal_cod`"),
                (Code::UnknownAttribute, "unknown attribute `code`"),
                (
                    Code::DuplicateAttribute,
                    "attribute `postal_code` is given more than once"
                ),
                (Code::AttributeExists, "attribute `city` already exists"),
                (Code::AttributeExists, "attribute `x` already exists"),
                (Code::EmptyRemove, "`rename {}` renames nothing"),
                (
                    Code::WrongOperandType,
                    "`rename` can't be applied to integers"
                ),
            ]
        );
        assert_eq!(
            errors[1].suggestions[0].message,
            "swap them: `postal_code as code`"
        );
        let fixed = fix(&source);
        assert!(
            fixed.contains("var a := t rename {postal_code as code};"),
            "{fixed}"
        );
        assert!(
            fixed.contains("var b := t rename {postal_code as code};"),
            "{fixed}"
        );
        assert!(fixed.contains("var f := t;"), "{fixed}");
        // A swap isn't a collision: both are renamed at once.
        variables("var t := tuple { a: 1, b: 2 } rename {a as b, b as a};");
    }

    #[test]
    fn rename_in_bodies() {
        let body = main_body(
            "var t := tuple { a: 1, b: 2 };\noperator main{} [\n    call debug_value{v: t rename {a as c, b as b}};\n    call debug_value{v: t rename {a as a}};\n];",
        );
        let t = || Box::new(hir::Expr::Variable(hir::VariableId(0)));
        assert_eq!(
            argument(&body[0]),
            &hir::Expr::Rename(t(), [("a".to_string(), "c".to_string())].into()),
            "renaming to the same name is dropped"
        );
        assert_eq!(argument(&body[1]), &*t());
    }

    #[test]
    fn join_constants() {
        let vars = variables(
            "var cities := relation { { city: \"Kumasi\", country: \"Ghana\" }, { city: \"Osaka\", country: \"Japan\" }, { city: \"Lima\", country: \"Peru\" } };\nvar countries := relation { { country: \"Ghana\", continent: \"Africa\" }, { country: \"Japan\", continent: \"Asia\" } };\nvar sizes := relation { { size: 1 }, { size: 2 } };\nvar placed := cities join countries;\nvar continents := cities compose countries;\nvar pairs := countries{continent} times sizes;\nvar itself := cities join cities;",
        );
        let value = |name: &str| vars.iter().find(|v| v.name == name).unwrap();
        let tuples = |name: &str| {
            let hir::Value::Relation(r) = &value(name).init else {
                panic!("{name} is a relation");
            };
            r.tuples.len()
        };
        assert_eq!(
            value("placed").ty.to_string(),
            "relation { city: text, continent: text, country: text }"
        );
        assert_eq!(tuples("placed"), 2, "Lima has no country to match");
        assert_eq!(
            value("continents").ty.to_string(),
            "relation { city: text, continent: text }"
        );
        assert_eq!(tuples("continents"), 2);
        assert_eq!(
            value("pairs").ty.to_string(),
            "relation { continent: text, size: integer }"
        );
        assert_eq!(tuples("pairs"), 4);
        assert_eq!(value("itself").init, value("cities").init);
    }

    #[test]
    fn join_errors() {
        let source = format!(
            "var cities := relation {{ {{ city: \"Kumasi\", country: \"Ghana\" }} }};\nvar countries := relation {{ {{ county: \"Ghana\", continent: \"Africa\" }} }};\nvar codes := relation {{ {{ country: 288 }} }};\nvar a := cities join countries;\nvar b := cities compose countries;\nvar c := cities times cities;\nvar d := cities join codes;\nvar e := tuple {{ country: \"Ghana\" }} join cities;\nvar f := cities join 1;{MAIN}"
        );
        let errors = errors(&source);
        let found: Vec<_> = errors.iter().map(|e| (e.code, &e.message[..])).collect();
        assert_eq!(
            found,
            [
                (
                    Code::CommonAttributes,
                    "the operands of `join` have no attributes in common"
                ),
                (
                    Code::CommonAttributes,
                    "the operands of `compose` have no attributes in common"
                ),
                (
                    Code::CommonAttributes,
                    "the operands of `times` have `city` and `country` in common"
                ),
                (
                    Code::JoinTypeConflict,
                    "the operands of `join` both have an attribute `country`, of different types"
                ),
                (Code::WrongOperandType, "`join` can't be applied to tuples"),
                (
                    Code::WrongOperandType,
                    "`join` can't be applied to integers"
                ),
            ]
        );
        assert_eq!(
            errors[0].help[0],
            "if `county` is `country`, rename it so they match: `… rename {county as country}`"
        );
    }

    #[test]
    fn joins_of_lists() {
        let vars = variables(
            "var a := relation { { x: 1, y: 2 }, { x: 3, y: 4 } };\nvar b := relation { { y: 2, z: 5 } };\nvar c := relation { { z: 5, w: 6 } };\nvar s := relation { { size: 1 }, { size: 2 } };\nvar all := join{a, b, c};\nvar nested := a join b join c;\nvar any_order := join{c, a, b};\nvar pairs := times{s, c, a{x}};\nvar dee := join{};\nvar also_dee := times{};\nvar alone := join{a};",
        );
        let value = |name: &str| &vars.iter().find(|v| v.name == name).unwrap().init;
        assert_eq!(value("all"), value("nested"));
        assert_eq!(value("all"), value("any_order"));
        assert_eq!(
            value("pairs").ty().to_string(),
            "relation { size: integer, w: integer, x: integer, z: integer }"
        );
        assert_eq!(value("dee"), &hir::Value::dee());
        assert_eq!(value("also_dee"), &hir::Value::dee());
        assert_eq!(value("alone"), value("a"));
    }

    #[test]
    fn join_of_lists_errors() {
        let source = format!(
            "var a := relation {{ {{ x: 1 }} }};\nvar b := relation {{ {{ x: 1, y: 2 }} }};\nvar c := relation {{ {{ z: 1 }} }};\nvar t := relation {{ {{ x: \"1\" }} }};\nvar d := join{{a, b, c}};\nvar e := times{{a, c, b}};\nvar f := join{{a, b, t}};\nvar g := compose{{a, b}};\nvar h := compose{{a, b, c}};\nvar i := join{{a, 1}};{MAIN}"
        );
        let errors = errors(&source);
        let found: Vec<_> = errors.iter().map(|e| (e.code, &e.message[..])).collect();
        assert_eq!(
            found,
            [
                (
                    Code::CommonAttributes,
                    "some operands of `join` have no attributes in common with the others"
                ),
                (
                    Code::CommonAttributes,
                    "the operands of `times` have `x` in common"
                ),
                (
                    Code::JoinTypeConflict,
                    "the operands of `join` both have an attribute `x`, of different types"
                ),
                (
                    Code::NaryCompose,
                    "`compose` takes two operands, not a list"
                ),
                (
                    Code::NaryCompose,
                    "`compose` takes two operands, not a list"
                ),
                (
                    Code::WrongOperandType,
                    "`join` can't be applied to integers"
                ),
            ]
        );
        // Only the operands of `times` that share an attribute are labelled.
        assert_eq!(errors[1].secondary.len(), 2);
        assert!(fix(&source).contains("var g := a compose b;"));
        assert!(errors[4].suggestions.is_empty());
    }

    #[test]
    fn joins_in_bodies() {
        let program = analyze_str(
            "var cities := relation { { city: \"Kumasi\", country: \"Ghana\" } };\noperator f{r: relation { country: text, continent: text }} [\n    call debug_value{v: cities join r};\n    call debug_value{v: cities compose r};\n    call debug_value{v: cities join r join cities};\n];\noperator main{} [];",
        )
        .unwrap();
        let f = program.operators.iter().find(|o| o.name == "f").unwrap();
        let hir::OperatorKind::User { body } = &f.kind else {
            panic!("f is a user operator");
        };
        let cities = || Box::new(hir::Expr::Variable(hir::VariableId(0)));
        let r = || Box::new(hir::Expr::Parameter("r".to_string()));
        assert_eq!(argument(&body[0]), &hir::Expr::And(cities(), r()));
        assert_eq!(
            argument(&body[1]),
            &hir::Expr::Remove(
                Box::new(hir::Expr::And(cities(), r())),
                ["country".to_string()].into()
            )
        );
        assert_eq!(
            argument(&body[2]),
            &hir::Expr::And(Box::new(hir::Expr::And(cities(), r())), cities())
        );
        // In a list, the constant operands are joined together when
        // compiling, and the result joined with the others when running.
        let program = analyze_str(
            "operator f{r: relation { z: integer }} [\n    call debug_value{v: join{relation { { x: 1, y: 2 } }, r, relation { { y: 2, z: 3 } }}};\n    call debug_value{v: join{r}};\n];\noperator main{} [];",
        )
        .unwrap();
        let f = program.operators.iter().find(|o| o.name == "f").unwrap();
        let hir::OperatorKind::User { body } = &f.kind else {
            panic!("f is a user operator");
        };
        let hir::Expr::And(constant, parameter) = argument(&body[0]) else {
            panic!("{:?}", argument(&body[0]));
        };
        let hir::Expr::Constant(constant) = &**constant else {
            panic!("{constant:?}");
        };
        assert_eq!(
            constant.ty().to_string(),
            "relation { x: integer, y: integer, z: integer }"
        );
        assert_eq!(**parameter, *r());
        assert_eq!(argument(&body[1]), &*r());
        let errors = errors(
            "var cities := relation { { city: \"Kumasi\", country: \"Ghana\" } };\noperator f{r: relation { size: integer }} [\n    call debug_value{v: cities join r};\n];\noperator main{} [];",
        );
        assert_eq!(errors[0].code, Code::CommonAttributes, "{errors:#?}");
    }

    #[test]
    fn not_before_a_comparison() {
        let source =
            format!("var n := 1;\nvar a := not n = 0 and true;\nvar b := not n + 1 < 2;{MAIN}");
        let errors = errors(&source);
        assert_eq!(errors.len(), 2, "{errors:#?}");
        assert_eq!(errors[0].code, Code::TypeMismatch);
        assert_eq!(
            errors[0].suggestions[0].message,
            "negate the comparison: `not (n = 0)`"
        );
        assert_eq!(
            fix(&source),
            format!("var n := 1;\nvar a := not (n = 0) and true;\nvar b := not (n + 1 < 2);{MAIN}")
        );
        // In a body too, and not when `not` isn't the comparison's left side.
        let body =
            "operator f{n: integer} [ call debug_value{v: not n = 0}; ];\noperator main{} [];";
        assert_eq!(
            fix(body),
            "operator f{n: integer} [ call debug_value{v: not (n = 0)}; ];\noperator main{} [];"
        );
        let errors = self::errors(&format!("var a := true = not 1;{MAIN}"));
        assert!(
            errors[0]
                .suggestions
                .iter()
                .all(|s| !s.message.starts_with("negate the comparison")),
            "{errors:#?}"
        );
    }

    #[test]
    fn comparison_simplification() {
        let cases = [
            ("p = true", "p"),
            ("true = p", "p"),
            ("p = false", "not p"),
            ("p <> true", "not p"),
            ("false <> p", "p"),
            ("not (n < 1) = false", "n < 1"),
            ("n < m", "n < m"),
        ];
        for (written, simplified) in cases {
            let source = |e: &str| {
                format!(
                    "operator f{{p: boolean, n: integer, m: integer}} [ call debug_value{{v: {e}}}; ];\noperator main{{}} [];"
                )
            };
            let body = |e: &str| {
                let program = analyze_str(&source(e)).unwrap();
                let f = program.operators.iter().find(|o| o.name == "f").unwrap();
                let hir::OperatorKind::User { body } = &f.kind else {
                    panic!("f is a user operator");
                };
                argument(&body[0]).clone()
            };
            assert_eq!(body(written), body(simplified), "{written}");
        }
    }

    #[test]
    fn integer_simplification() {
        let cases = [
            ("n + 0", "n"),
            ("0 + n", "n"),
            ("n - 0", "n"),
            ("0 - n", "-n"),
            ("n * 1", "n"),
            ("1 * n", "n"),
            ("n * 0", "0"),
            ("0 * n", "0"),
            ("n * -1", "-n"),
            ("n div 1", "n"),
            ("n div -1", "-n"),
            ("n mod 1", "0"),
            ("n mod -1", "0"),
            ("+n", "n"),
            // An operand that can fail is kept, so that its failure isn't
            // skipped.
            ("(n + 1) * 0", "(n + 1) * 0"),
            ("(n div m) mod 1", "(n div m) mod 1"),
            ("- -n", "- -n"),
            ("n + m", "n + m"),
        ];
        for (written, simplified) in cases {
            let source = |e: &str| {
                format!(
                    "operator f{{n: integer, m: integer}} [ call debug_value{{v: {e}}}; ];\noperator main{{}} [];"
                )
            };
            let body = |e: &str| {
                let program = analyze_str(&source(e)).unwrap();
                let f = program.operators.iter().find(|o| o.name == "f").unwrap();
                let hir::OperatorKind::User { body } = &f.kind else {
                    panic!("f is a user operator");
                };
                argument(&body[0]).clone()
            };
            assert_eq!(body(written), body(simplified), "{written}");
        }
        let errors = errors(
            "operator f{n: integer} [ call debug_value{v: n mod 0}; ];\noperator main{} [];",
        );
        assert_eq!(errors[0].code, Code::DivisionByZero, "certain to fail");
    }

    #[test]
    fn expressions_in_bodies() {
        let body = main_body(
            "var b := true;\noperator main{} [\n    call debug_value{v: not b};\n    call debug_value{v: b and (true xor true)};\n    call debug_value{v: not (true implies false)};\n];",
        );
        let b = || Box::new(hir::Expr::Variable(hir::VariableId(0)));
        assert_eq!(
            argument(&body[0]),
            &hir::Expr::Unary(hir::UnaryOp::Not, b())
        );
        assert_eq!(
            argument(&body[1]),
            &hir::Expr::Constant(hir::Value::Boolean(false)),
            "`b and false` is false, whatever `b` is"
        );
        assert_eq!(
            argument(&body[2]),
            &hir::Expr::Constant(hir::Value::Boolean(true))
        );
        let errors = errors(
            "operator show{p: integer, q: boolean} [ call debug_value{v: p or q}; call show{p: 1, q: q and p}; ];\noperator main{} [];",
        );
        let codes: Vec<_> = errors.iter().map(|e| e.code).collect();
        assert_eq!(codes, [Code::TypeMismatch, Code::TypeMismatch]);
        assert_eq!(
            errors[0].secondary[0].message,
            "`or` takes boolean operands"
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
