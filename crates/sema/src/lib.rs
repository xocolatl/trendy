mod assign;
mod body;
mod check;
mod errors;
mod globals;
mod keys;
mod operators;
mod ops;
mod prelude;
mod types;

use std::collections::HashMap;

use trendy_diag::{Diagnostic, Span};
use trendy_hir as hir;
use trendy_syntax::SyntaxToken;
use trendy_syntax::ast::{Item, SourceFile, VarDecl};

pub use prelude::Prelude;

use crate::operators::Operators;

/// Check a parsed program and translate it to HIR. The prelude's
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
    let checked = check(prelude, file, Source::Program, &mut errors);
    let entry = entry(file, &checked.namespaces, &checked.operators, &mut errors);
    errors.sort_by_key(|d| d.span().start);
    match entry {
        Some(entry) if errors.is_empty() => {
            let (operators, variables) = checked.finish(prelude);
            Ok(hir::Program {
                operators,
                variables,
                entry,
            })
        }
        _ => Err(errors),
    }
}

/// What a file is checked as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Source {
    /// A program, which declares no built-ins.
    Program,
    /// The prelude, whose built-ins the prelude it is checked with already
    /// has: its operators written in trendy are checked.
    Prelude,
}

/// A file, checked, with every error reported.
pub(crate) struct Checked {
    namespaces: Namespaces,
    operators: Operators,
    evaluated: globals::Evaluated,
    bodies: HashMap<hir::OperatorId, Vec<hir::Statement>>,
}

/// Check `file`, as `source` says, with `prelude`'s operators in scope.
pub(crate) fn check(
    prelude: &Prelude,
    file: &SourceFile,
    source: Source,
    errors: &mut Vec<Diagnostic>,
) -> Checked {
    let namespaces = collect_names(prelude, file, errors);

    // Every operator's signature first, so that anything can refer to an
    // operator defined after it.
    let operators = operators::signatures(prelude, file, &namespaces, source, errors);

    // Then the globals: the variables' initial values, and the read-only
    // operators' values, which initial values can call and which read
    // variables, each when first needed.
    let declarations = declarations(file, &namespaces);
    let variable_ids = variable_ids(file, &namespaces);
    let evaluated = globals::Globals::new(
        prelude,
        &namespaces,
        &operators,
        &declarations,
        &variable_ids,
    )
    .evaluate(file, errors);

    // Then the update operators' bodies, which refer to both.
    let context = body::Context {
        namespaces: &namespaces,
        operators: &operators,
        values: &evaluated.values,
        variables: &evaluated.variables,
        variable_ids: &variable_ids,
        declarations: &declarations,
    };
    let mut bodies = HashMap::new();
    let mut growing = Vec::new();
    for signature in operators.iter().filter(|s| !s.read_only) {
        if let Some(decl) = &signature.decl
            && let Some(statements) = body::body(decl, signature.id, &context, &mut growing, errors)
        {
            bodies.insert(signature.id, statements);
        }
    }
    body::growing_recursion(&growing, &bodies, &operators, errors);
    Checked {
        namespaces,
        operators,
        evaluated,
        bodies,
    }
}

impl Checked {
    /// The signatures of the file's operators, after the prelude's.
    pub(crate) fn signatures(&self) -> impl Iterator<Item = &operators::Signature> {
        self.operators.iter()
    }

    /// The HIR operators, `prelude`'s as it has them, then the file's, with
    /// their bodies, and the variables, of a file without errors.
    pub(crate) fn finish(mut self, prelude: &Prelude) -> (Vec<hir::Operator>, Vec<hir::Variable>) {
        let operators = self
            .operators
            .iter()
            .map(|signature| {
                if let Some(op) = prelude.operators().get(signature.id.0) {
                    return op.clone();
                }
                let kind = match (signature.builtin, &signature.result) {
                    (Some(builtin), _) => hir::OperatorKind::Builtin(builtin),
                    (None, Some(result)) => hir::OperatorKind::ReadOnly {
                        result: result.clone(),
                        value: self.evaluated.values[&signature.id].clone(),
                    },
                    (None, None) => hir::OperatorKind::Update {
                        body: self
                            .bodies
                            .remove(&signature.id)
                            .expect("every update operator without errors has a body"),
                    },
                };
                hir::Operator {
                    name: signature.name.clone(),
                    types: signature.types.clone(),
                    params: signature.params.clone(),
                    kind,
                }
            })
            .collect();
        let variables = self
            .evaluated
            .variables
            .into_iter()
            .map(|v| v.expect("a program without errors has every variable"))
            .collect();
        (operators, variables)
    }
}

/// The operator named `main`, which must be the program's own, an update
/// operator taking no parameters.
fn entry(
    file: &SourceFile,
    namespaces: &Namespaces,
    operators: &Operators,
    errors: &mut Vec<Diagnostic>,
) -> Option<hir::OperatorId> {
    let Some((signature, decl)) = operators
        .named("main")
        .and_then(|s| Some((s, s.decl.as_ref()?)))
    else {
        errors.push(errors::missing_main(file, namespaces));
        return None;
    };
    let mut valid = true;
    if let Some(params) = decl.params().filter(|h| h.attributes().next().is_some()) {
        errors.push(errors::main_with_params(&params));
        valid = false;
    }
    if decl.result().is_some() {
        errors.push(errors::main_with_result(decl));
        valid = false;
    }
    valid.then_some(signature.id)
}

/// What a top-level name refers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    Operator,
    Builtin,
    /// An operator the prelude defines in trendy.
    Prelude,
    Variable,
}

impl Kind {
    pub(crate) fn describe(self) -> &'static str {
        match self {
            Kind::Operator | Kind::Prelude => "an operator",
            Kind::Builtin => "a built-in operator",
            Kind::Variable => "a variable",
        }
    }
}

/// The program's top-level names, each in its namespace: its first
/// definition, and where it is (built-ins are in the prelude, so they have
/// no span). Operators and variables have a namespace each, so a variable
/// can have an operator's name; types have a third.
#[derive(Default)]
pub(crate) struct Namespaces {
    /// User and built-in operators.
    operators: HashMap<String, (Kind, Option<Span>)>,
    variables: HashMap<String, Span>,
}

impl Namespaces {
    /// The operator named `name`: whether it is built in, and where it is
    /// defined.
    pub(crate) fn operator(&self, name: &str) -> Option<(Kind, Option<Span>)> {
        self.operators.get(name).copied()
    }

    /// Where the variable named `name` is declared.
    pub(crate) fn variable(&self, name: &str) -> Option<Span> {
        self.variables.get(name).copied()
    }

    pub(crate) fn operators(&self) -> impl Iterator<Item = (&str, Kind, Option<Span>)> {
        self.operators
            .iter()
            .map(|(name, &(kind, span))| (name.as_str(), kind, span))
    }

    /// Whether `ident` is its operator name's first definition, not a
    /// duplicate.
    pub(crate) fn is_first_operator(&self, ident: &SyntaxToken) -> bool {
        self.operator(ident.text())
            .is_some_and(|(_, span)| span == Some(trendy_syntax::span(ident.text_range())))
    }

    /// Whether `ident` is its variable name's first declaration, not a
    /// duplicate.
    pub(crate) fn is_first_variable(&self, ident: &SyntaxToken) -> bool {
        self.variable(ident.text()) == Some(trendy_syntax::span(ident.text_range()))
    }
}

/// Record every top-level name in its namespace, the prelude's first,
/// reporting each one defined more than once in it.
fn collect_names(prelude: &Prelude, file: &SourceFile, errors: &mut Vec<Diagnostic>) -> Namespaces {
    let mut namespaces = Namespaces {
        operators: prelude
            .operators()
            .iter()
            .map(|op| {
                let kind = match op.kind {
                    hir::OperatorKind::Builtin(_) => Kind::Builtin,
                    _ => Kind::Prelude,
                };
                (op.name.clone(), (kind, None))
            })
            .collect(),
        variables: HashMap::new(),
    };
    for item in file.items() {
        let (kind, name) = match item {
            Item::Operator(op) => (Kind::Operator, op.name()),
            Item::Variable(decl) => (Kind::Variable, decl.name()),
            // Reported as being outside the prelude, or the prelude's own,
            // already in it.
            Item::Builtin(_) | Item::BuiltinType(_) => continue,
        };
        let Some(ident) = name.and_then(|n| n.ident()) else {
            continue;
        };
        let (name, span) = (ident.text(), trendy_syntax::span(ident.text_range()));
        let first = match kind {
            Kind::Variable => namespaces.variable(name).map(|s| (kind, Some(s))),
            _ => namespaces.operator(name),
        };
        match (first, kind) {
            (Some(first), _) => errors.push(errors::duplicate_name(name, (kind, span), first)),
            (None, Kind::Variable) => {
                namespaces.variables.insert(name.to_string(), span);
            }
            (None, _) => {
                namespaces
                    .operators
                    .insert(name.to_string(), (kind, Some(span)));
            }
        }
    }
    namespaces
}

/// The first declaration of each variable, by name.
fn declarations(file: &SourceFile, namespaces: &Namespaces) -> HashMap<String, VarDecl> {
    file.variables()
        .filter_map(|decl| {
            let ident = decl.name()?.ident()?;
            namespaces
                .is_first_variable(&ident)
                .then(|| (ident.text().to_string(), decl))
        })
        .collect()
}

/// Each variable's id, by name: its first declaration's place among them,
/// in source order.
fn variable_ids(file: &SourceFile, namespaces: &Namespaces) -> HashMap<String, hir::VariableId> {
    file.variables()
        .filter_map(|decl| {
            let ident = decl.name()?.ident()?;
            namespaces
                .is_first_variable(&ident)
                .then(|| ident.text().to_string())
        })
        .enumerate()
        .map(|(i, name)| (name, hir::VariableId(i)))
        .collect()
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
    fn variables_and_operators_have_separate_namespaces() {
        // A variable and an operator can have the same name, built in or
        // not: a value is the variable, and `call` the operator.
        let program = analyze_str("var main := 0;\noperator main{} [];").unwrap();
        assert_eq!(program.variables[0].name, "main");
        analyze_str(
            "var greet := \"Hello\";\nvar debug_value := 1;\noperator greet{name: text} [ call debug_value{v: greet || name}; ];\noperator main{} [ call greet{name: \"Amara\"}; call debug_value{v: debug_value}; ];",
        )
        .unwrap();
        // So can a parameter.
        analyze_str(
            "operator f{debug_value: integer} [ call debug_value{v: debug_value}; ];\noperator main{} [];",
        )
        .unwrap();
        // But not two variables, or two operators.
        let variables = errors("var a := 0;\nvar a := 1;\noperator main{} [];");
        assert_eq!(variables[0].code, Code::DuplicateName);
        assert!(variables[0].notes[0].starts_with("variables have a namespace of their own"));
        let operators = errors("operator a{} [];\noperator a{} [];\noperator main{} [];");
        assert!(operators[0].notes[0].starts_with("operators have a namespace of their own"));
    }

    /// Read-only operators for the tests below.
    const READ_ONLY: &str = "var rate := 3;\nvar poets := relation { { name: \"Ada\", born: 1815 }, { name: \"Rumi\", born: 1207 } };\noperator double{x: integer}: integer [ return x * 2; ];\noperator quadruple{x: integer}: integer [ return double{x: double{x: x}}; ];\noperator scaled{x: integer}: integer [ return x * rate; ];\noperator born_before{year: integer}: relation { name: text, born: integer } [ return poets where born < year; ];";

    #[test]
    fn read_only_operators_are_evaluated_when_compiling() {
        let program = analyze_str(&format!(
            "{READ_ONLY}\nvar a := quadruple{{x: 2}};\nvar b := scaled{{x: 2}};\nvar c := born_before{{year: 1500}}{{name}};\nvar d := poets extend {{ twice := double{{x: born}} }};{MAIN}"
        ))
        .unwrap();
        let value = |name: &str| {
            program
                .variables
                .iter()
                .find(|v| v.name == name)
                .unwrap()
                .init
                .clone()
        };
        assert_eq!(value("a"), hir::Value::Integer(8));
        // A variable an operator reads has its initial value then.
        assert_eq!(value("b"), hir::Value::Integer(6));
        let hir::Value::Relation(c) = value("c") else {
            panic!("a relation");
        };
        assert_eq!(c.tuples.len(), 1);
        // A call whose argument is the tuple at hand's is evaluated for each.
        let hir::Value::Relation(d) = value("d") else {
            panic!("a relation");
        };
        assert!(
            d.tuples
                .iter()
                .any(|t| t.get("twice") == Some(&hir::Value::Integer(3630)))
        );
        // So is one in a body, with arguments known when compiling.
        let body = main_body(&format!(
            "{READ_ONLY}\noperator main{{}} [ call debug_value{{v: double{{x: 21}}}}; ];"
        ));
        assert_eq!(
            argument(&body[0]),
            &hir::Expr::Constant(hir::Value::Integer(42))
        );
        // A read-only operator's value is in HIR, with calls of others left
        // where their arguments are its parameters.
        let quadruple = program
            .operators
            .iter()
            .find(|o| o.name == "quadruple")
            .unwrap();
        let hir::OperatorKind::ReadOnly { result, value } = &quadruple.kind else {
            panic!("a read-only operator");
        };
        assert_eq!(result, &hir::Type::Scalar(hir::Scalar::Integer));
        assert!(matches!(value, hir::Expr::Call(..)));
    }

    #[test]
    fn read_only_operators_change_nothing_and_return_once() {
        let codes_of = |body: &str| {
            codes(&format!(
                "var n := 0;\noperator f{{x: integer}}: integer [ {body} ];{MAIN}"
            ))
        };
        assert_eq!(
            codes_of("call debug_value{v: x}; n := x; return x;"),
            [Code::ReadOnlyEffect, Code::ReadOnlyEffect]
        );
        assert_eq!(codes_of(""), [Code::Return]);
        let source =
            "operator f{x: integer}: integer [ return x; return 1; ];\noperator main{} [];";
        assert_eq!(codes(source), [Code::Return]);
        assert_eq!(
            fix(source),
            "operator f{x: integer}: integer [ return x; ];\noperator main{} [];"
        );
        // A `return` in an update operator.
        assert_eq!(
            codes("operator f{} [ return 1; ];\noperator main{} [];"),
            [Code::Return]
        );
        // A `return` of the wrong type, reported at the value.
        let errors = errors("operator f{}: integer [ return \"one\"; ];\noperator main{} [];");
        assert_eq!(errors[0].code, Code::TypeMismatch);
        // `main` has no result type: the fix removes it with its `return`s.
        let source = "operator main{}: integer [ call debug_value{v: 1}; return 0; ];";
        assert_eq!(codes(source), [Code::MainSignature]);
        assert_eq!(fix(source), "operator main{} [ call debug_value{v: 1}; ];");
    }

    #[test]
    fn calls_of_read_only_and_update_operators() {
        let program = |body: &str| {
            format!(
                "{READ_ONLY}\nvar n := 1;\noperator note{{}} [];\noperator main{{}} [ {body} ];"
            )
        };
        // A read-only operator's call is a value, which `call` would lose.
        assert_eq!(
            codes(&program("call double{x: 1};")),
            [Code::CallOfReadOnly]
        );
        // An update operator's has none.
        assert_eq!(
            codes(&program("call debug_value{v: note{}};")),
            [Code::OperatorAsValue]
        );
        assert_eq!(
            codes(&program("call debug_value{v: debug_value{v: 1}};")),
            [Code::OperatorAsValue]
        );
        // Calls are checked as `call`'s are.
        assert_eq!(
            codes(&program("call debug_value{v: double{y: 1}};")),
            [Code::UnknownAttribute]
        );
        assert_eq!(
            codes(&program("call debug_value{v: double{}};")),
            [Code::MissingAttribute]
        );
        assert_eq!(
            codes(&program("call debug_value{v: n{x: 1}};")),
            [Code::NotAnOperator]
        );
        assert_eq!(
            codes(&program("call debug_value{v: triple{x: 1}};")),
            [Code::UnknownName]
        );
        // A name before attribute names that only an operator has is a call
        // with positional arguments.
        let source = program("call debug_value{v: double{n}};");
        assert_eq!(codes(&source), [Code::PositionalArgument]);
        assert!(fix(&source).contains("double{x: n}"));
        // A call whose arguments, or a variable its operator reads, are
        // known only when the program runs is made then.
        for value in ["double{x: n}", "scaled{x: 1}"] {
            let body = main_body(&program(&format!("call debug_value{{v: {value}}};")));
            assert!(matches!(argument(&body[0]), hir::Expr::Call(..)), "{value}");
        }
        // A call whose value fails is an error at the call.
        let errors = errors(&format!(
            "{READ_ONLY}\nvar big := double{{x: 9223372036854775807}};{MAIN}"
        ));
        assert_eq!(errors[0].code, Code::Overflow);
        assert!(errors[0].notes.iter().any(|n| n.contains("`double`")));
    }

    #[test]
    fn read_only_operators_dont_call_themselves() {
        let mutual = errors(
            "operator f{}: integer [ return g{}; ];\noperator g{}: integer [ return f{}; ];\noperator main{} [];",
        );
        assert_eq!(mutual.len(), 1, "{mutual:#?}");
        assert_eq!(mutual[0].code, Code::Recursion);
        assert_eq!(mutual[0].message, "`f` and `g` call each other");
        assert_eq!(
            messages("operator f{x: integer}: integer [ return f{x: x}; ];\noperator main{} [];"),
            ["`f` calls itself"]
        );
        // A cycle through an operator's value is an initial values' cycle.
        let cycle = errors(&format!(
            "var a := f{{}};\noperator f{{}}: integer [ return a + 1; ];{MAIN}"
        ));
        assert_eq!(cycle.len(), 1, "{cycle:#?}");
        assert_eq!(cycle[0].code, Code::CyclicInitialValues);
        assert_eq!(
            cycle[0].message,
            "the initial value of `a` depends on itself"
        );
        let labels: Vec<&str> = cycle[0]
            .secondary
            .iter()
            .map(|l| l.message.as_str())
            .collect();
        assert_eq!(labels, ["`a` calls `f`", "`f` reads `a`"]);
    }

    #[test]
    fn a_name_before_empty_braces_is_the_operator() {
        // `poets{}` calls the operator, as `call poets{};` does, which, as an
        // update operator, has no value; projecting the variable needs
        // parentheses.
        let program = |value: &str| {
            format!(
                "var poets := relation {{ {{ name: \"Ada\" }} }};\noperator poets{{}} [];\noperator main{{}} [ call debug_value{{v: {value}}}; ];"
            )
        };
        let called = errors(&program("poets{}"));
        assert_eq!(called.len(), 1, "{called:#?}");
        assert_eq!(called[0].code, Code::OperatorAsValue);
        assert_eq!(
            called[0].message,
            "`poets{}` calls the operator `poets`, which has no value"
        );
        assert_eq!(fix(&program("poets{}")), program("(poets){}"));
        analyze_str(&program("(poets){}")).unwrap();
        // A projection on attributes can't be a call, so it is the value's.
        analyze_str(&program("poets{name}")).unwrap();
        // So it is in an initial value, and for a built-in operator.
        let source = format!(
            "var debug_value := relation {{ {{ a: 1 }} }};\nvar r := debug_value{{}};{MAIN}"
        );
        assert_eq!(codes(&source), [Code::OperatorAsValue]);
        assert!(fix(&source).contains("var r := (debug_value){};"));
        // Without a value of its name, there's nothing to project instead.
        let alone = errors(&format!("var r := main{{}};{MAIN}"));
        assert_eq!(
            alone[0].message,
            "`main{}` calls the operator `main`, which has no value"
        );
        assert!(alone[0].suggestions.is_empty());
        // Without an operator, it is the projection it looks like.
        analyze_str(&format!(
            "var poets := relation {{ {{ name: \"Ada\" }} }};\nvar none := poets{{}};{MAIN}"
        ))
        .unwrap();
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
    fn approximate_numbers() {
        let vars = variables(
            "var a: approximate := 0e0;\nvar b: approximate := 0e0;\nvar c := 1.5e-3;\nvar d: approximate;",
        );
        let values: Vec<_> = vars
            .iter()
            .map(|v| (v.ty.clone(), v.init.clone()))
            .collect();
        assert_eq!(
            values,
            [
                (
                    hir::Type::Scalar(hir::Scalar::Approximate),
                    hir::Value::Approximate(hir::Approximate::new(0.0))
                ),
                (
                    hir::Type::Scalar(hir::Scalar::Approximate),
                    hir::Value::Approximate(hir::Approximate::new(0.0))
                ),
                (
                    hir::Type::Scalar(hir::Scalar::Approximate),
                    hir::Value::Approximate(hir::Approximate::new(0.0015))
                ),
                (
                    hir::Type::Scalar(hir::Scalar::Approximate),
                    hir::Value::Approximate(hir::Approximate::new(0.0))
                ),
            ]
        );
    }

    #[test]
    fn no_implicit_integer_to_approximate() {
        let source = format!("var r: approximate := 1;{MAIN}");
        let errors = errors(&source);
        assert_eq!(errors[0].code, Code::TypeMismatch);
        assert_eq!(
            errors[0].suggestions[0].message,
            "write it as `approximate`: `1e0`"
        );
        assert_eq!(fix(&source), format!("var r: approximate := 1e0;{MAIN}"));
    }

    #[test]
    fn approximate_mismatch_fixes() {
        let source = format!(
            "var a: integer := 5e0;\nvar b: integer := 2.5e0;\nvar c: text := 1e3;\nvar d: approximate := \"7\";\nvar e: approximate := \"0.5\";{MAIN}"
        );
        assert_eq!(
            fix(&source),
            format!(
                "var a: integer := 5;\nvar b: approximate := 2.5e0;\nvar c: text := \"1e3\";\nvar d: approximate := 7e0;\nvar e: approximate := 0.5e0;{MAIN}"
            )
        );
    }

    #[test]
    fn approximate_type_aliases() {
        let source =
            format!("var a: float;\nvar b: double;\nvar c: Approximate;\nvar d: aproximate;{MAIN}");
        assert_eq!(
            fix(&source),
            format!(
                "var a: approximate;\nvar b: approximate;\nvar c: approximate;\nvar d: approximate;{MAIN}"
            )
        );
    }

    #[test]
    fn approximate_out_of_range() {
        let errors = errors(&format!("var r := 1e400;{MAIN}"));
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].code, Code::ApproximateOutOfRange);
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
            "var q := tuple { y: 2e0, x: 1e0 };\nvar p: tuple { a: integer, b: text } := tuple { b: \"x\", a: 1, };\nvar e: tuple {};\nvar f := tuple {};",
        );
        assert_eq!(
            vars[0].ty.to_string(),
            "tuple { x: approximate, y: approximate }"
        );
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
            "var p: tuple {{ x: approximate, y: approximate, z: text }} := tuple {{ x: 1e0 }};{MAIN}"
        );
        let errors = errors(&source);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].code, Code::MissingAttribute);
        assert_eq!(errors[0].message, "missing attributes `y` and `z`");
        assert_eq!(
            fix(&source),
            format!(
                "var p: tuple {{ x: approximate, y: approximate, z: text }} := tuple {{ x: 1e0, y: 0e0, z: \"\" }};{MAIN}"
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
            "var p: tuple {{ x: approximate, y: approximate }} := tuple {{ x: 1e0, why: 2e0 }};{MAIN}"
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
                "var p: tuple {{ x: approximate, y: approximate }} := tuple {{ x: 1e0, y: 2e0 }};{MAIN}"
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
            "var n: tuple { r: relation { x: approximate } };\nvar m := tuple { r: relation { { x: 1e0 } } };\nvar rr := relation { { inner: relation { { x: 1 } } } };",
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
        let hir::OperatorKind::Update { body } = &greet.kind else {
            panic!("greet is a user operator");
        };
        // Inside `greet`, `r` is the parameter, which hides the variable.
        assert_eq!(*argument(&body[1]), hir::Expr::Parameter("r".to_string()));
        let main = program.operator(program.entry);
        let hir::OperatorKind::Update { body } = &main.kind else {
            panic!("main is a user operator");
        };
        assert_eq!(*argument(&body[0]), hir::Expr::Variable(hir::VariableId(0)));
        let hir::Statement::Call(call) = &body[1] else {
            panic!("a call");
        };
        assert_eq!(
            call.args["name"],
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
        // `any type` is only a parameter's.
        let source = "var x: any type := 1;\noperator f{v: integer}: any type [ return v; ];\noperator main{} [];";
        assert_eq!(
            codes(source),
            [Code::AnyOutsideParameter, Code::AnyOutsideParameter]
        );
        // A bare `any`, as TypeScript and Go write it, is fixed to it in a
        // parameter's type, as are PostgreSQL's polymorphic types.
        let source =
            "var x: any := 1;\noperator show{v: any, w: anyelement} [];\noperator main{} [];";
        assert_eq!(
            codes(source),
            [Code::UnknownType, Code::UnknownType, Code::UnknownType]
        );
        assert_eq!(
            fix(source),
            "var x: any := 1;\noperator show{v: any type, w: any type} [];\noperator main{} [];"
        );
        // A variable may be named `any`, or `types`.
        analyze_str(
            "var any := 1;\nvar types := any;\noperator main{} [ call debug_value{v: types}; ];",
        )
        .unwrap();
        let errors = errors("operator debug_value{} [];\noperator main{} [];");
        assert_eq!(errors[0].code, Code::DuplicateName);
        assert!(errors[0].notes[0].contains("declared in the prelude"));
    }

    #[test]
    fn main_takes_no_parameters() {
        let source = "operator main{args: text} [];";
        assert_eq!(codes(source), [Code::MainSignature]);
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
            let hir::OperatorKind::Update { body } = &f.kind else {
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
        let hir::OperatorKind::Update { body } = &program.operator(program.entry).kind else {
            panic!("main is a user operator");
        };
        body.clone()
    }

    fn argument(statement: &hir::Statement) -> &hir::Expr {
        let hir::Statement::Call(call) = statement else {
            panic!("a call");
        };
        &call.args["v"]
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
                let hir::OperatorKind::Update { body } = &f.kind else {
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
            init("var a := -1.5e0;", "a"),
            hir::Value::Approximate(hir::Approximate::new(-1.5))
        );
        assert_eq!(
            init("var a := -0e0;", "a"),
            hir::Value::Approximate(hir::Approximate::new(-0.0))
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
            "var a := 1 + true;\nvar b := -\"x\";\nvar c: boolean := 1 * 2;\nvar d := not 1;\nvar e := 1.5e0 + 1;\nvar r: approximate := -1;{MAIN}"
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
        assert!(fixed.contains("var e := 1.5e0 + 1e0;"), "{fixed}");
        assert!(
            fixed.ends_with(&format!("var r: approximate := -1e0;{MAIN}")),
            "a negative literal is rewritten like any other"
        );
    }

    #[test]
    fn approximate_initial_values() {
        let r = |x| hir::Value::Approximate(hir::Approximate::new(x));
        assert_eq!(init("var a := 1.5e0 + 2.25e0 * 2e0;", "a"), r(6.0));
        assert_eq!(init("var a := 1e0 / 4e0 - 0.5e0;", "a"), r(-0.25));
        assert_eq!(init("var a := -(0e0);", "a"), r(-0.0));
        assert_eq!(init("var a := +2.5e0;", "a"), r(2.5));
        assert_eq!(init("var a := b / 2e0;\nvar b := 3e0;", "a"), r(1.5));
        // The context sets the operand type, so the literal is what's wrong.
        let source = format!("var a: approximate := 1 + 2;{MAIN}");
        assert_eq!(
            fix(&source),
            format!("var a: approximate := 1e0 + 2e0;{MAIN}")
        );
    }

    #[test]
    fn approximate_failures_when_compiling() {
        let source = format!(
            "var a := 1e300 * 1e300;\nvar b := 1e0 / 0e0;\nvar c := 1e0 / -0e0;\nvar d := -1e308 - 1e308;{MAIN}"
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
                (Code::Overflow, "approximate overflow", "1e300 * 1e300"),
                (Code::DivisionByZero, "division by zero", "0e0"),
                (Code::DivisionByZero, "division by zero", "-0e0"),
                (Code::Overflow, "approximate overflow", "-1e308 - 1e308"),
            ]
        );
    }

    #[test]
    fn division_operators_by_type() {
        let source = format!(
            "var a := 7 / 2;\nvar b := 7e0 div 2e0;\nvar c := 7e0 mod 2e0;\nvar d: integer := 1/2;{MAIN}"
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
                    "`div` can't be applied to approximate numbers",
                    "div"
                ),
                (
                    Code::WrongOperandType,
                    "`mod` can't be applied to approximate numbers",
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
                "var a := 7 div 2;\nvar b := 7e0 / 2e0;\nvar c := 7e0 mod 2e0;\nvar d: integer := 1 div 2;{MAIN}"
            )
        );
        assert_eq!(
            codes(&format!("var a := 1 + 1.5e0;\nvar b := true + 1;{MAIN}")),
            [Code::TypeMismatch, Code::WrongOperandType]
        );
    }

    #[test]
    fn approximate_simplification() {
        let cases = [
            ("x * 1e0", "x"),
            ("1e0 * x", "x"),
            ("x / 1e0", "x"),
            ("x - 0e0", "x"),
            ("x + -0e0", "x"),
            ("-0e0 + x", "x"),
            ("-0e0 - x", "-x"),
            ("x * -1e0", "-x"),
            ("x / -1e0", "-x"),
            ("- -x", "x"),
            ("+x", "x"),
            // IEEE 754 signed zeros: these aren't the identity.
            ("x + 0e0", "x + 0e0"),
            ("0e0 - x", "0e0 - x"),
            ("x * 0e0", "x * 0e0"),
        ];
        for (written, simplified) in cases {
            let source = |e: &str| {
                format!(
                    "operator f{{x: approximate}} [ call debug_value{{v: {e}}}; ];\noperator main{{}} [];"
                )
            };
            let body = |e: &str| {
                let program = analyze_str(&source(e)).unwrap();
                let f = program.operators.iter().find(|o| o.name == "f").unwrap();
                let hir::OperatorKind::Update { body } = &f.kind else {
                    panic!("f is a user operator");
                };
                argument(&body[0]).clone()
            };
            assert_eq!(body(written), body(simplified), "{written}");
        }
        let x = || Box::new(hir::Expr::Parameter("x".to_string()));
        let program = analyze_str(
            "operator f{x: approximate} [ call debug_value{v: x + 0e0}; ];\noperator main{} [];",
        )
        .unwrap();
        let f = program.operators.iter().find(|o| o.name == "f").unwrap();
        let hir::OperatorKind::Update { body } = &f.kind else {
            panic!("f is a user operator");
        };
        assert_eq!(
            argument(&body[0]),
            &hir::Expr::Binary(
                hir::BinaryOp::Add(hir::Numeric::Approximate),
                x(),
                Box::new(hir::Expr::Constant(hir::Value::Approximate(
                    hir::Approximate::new(0.0)
                )))
            )
        );
        assert_eq!(
            codes(
                "operator f{x: approximate} [ call debug_value{v: x / 0e0}; ];\noperator main{} [];"
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
                let hir::OperatorKind::Update { body } = &f.kind else {
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
        assert_eq!(init("var a := -0e0 < 0e0;", "a"), yes);
        assert_eq!(init("var a := -0e0 = 0e0;", "a"), no);
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
            "var a := 1 = 1e0;\nvar b: integer := 1 < 2;\nvar c := tuple {{}} < tuple {{}};\nvar d: boolean := 1 = 2;{MAIN}"
        );
        let errors = errors(&source);
        let found: Vec<_> = errors.iter().map(|e| (e.code, &e.message[..])).collect();
        assert_eq!(
            found,
            [
                (
                    Code::TypeMismatch,
                    "mismatched types: expected `integer`, found `approximate`"
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
                "var a := tuple { r: relation { { x: -0e0 } } } = tuple { r: relation { { x: 0e0 } } };",
                "a"
            ),
            no,
            "nested values compare as values"
        );
        let source = format!(
            "var a := tuple {{ x: 1 }} = tuple {{ x: 1, y: 2 }};\nvar b := tuple {{ x: 1 }} = relation {{ {{ x: 1 }} }};\nvar c := tuple {{ x: 1 }} >= tuple {{ x: 2 }};{MAIN}"
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
                (Code::WrongOperandType, "`>=` can't be applied to tuples"),
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
                (Code::EmptyDoesNothing, "`remove {}` removes nothing"),
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
            let hir::OperatorKind::Update { body } = &f.kind else {
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
                (Code::EmptyDoesNothing, "`rename {}` renames nothing"),
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
                (
                    Code::WrongOperandType,
                    "`join` can't combine a tuple with relations"
                ),
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
            "var a := relation {{ {{ x: 1 }} }};\nvar b := relation {{ {{ x: 1, y: 2 }} }};\nvar c := relation {{ {{ z: 1 }} }};\nvar t := relation {{ {{ x: \"1\" }} }};\nvar d := join{{a, b, c}};\nvar e := times{{a, c, b}};\nvar f := join{{a, b, t}};\nvar i := join{{a, 1}};{MAIN}"
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
                    Code::WrongOperandType,
                    "`join` can't be applied to integers"
                ),
            ]
        );
        // Only the operands of `times` that share an attribute are labelled.
        assert_eq!(errors[1].secondary.len(), 2);
    }

    #[test]
    fn compose_of_lists() {
        let vars = variables(
            "var a := relation { { x: 1, y: 2 }, { x: 3, y: 4 } };\nvar b := relation { { y: 2, z: 5 }, { y: 4, z: 6 } };\nvar c := relation { { z: 5, w: 7 } };\nvar listed := compose[a, b, c];\nvar nested := (a compose b) compose c;\nvar alone := compose[a];",
        );
        let value = |name: &str| &vars.iter().find(|v| v.name == name).unwrap().init;
        assert_eq!(value("listed"), value("nested"));
        assert_eq!(
            value("listed").ty().to_string(),
            "relation { w: integer, x: integer }"
        );
        assert_eq!(value("alone"), value("a"));

        // Each step composes what comes before with the next operand, so
        // `y`, removed by the first step, can't match the third operand.
        let source = format!(
            "var a := relation {{ {{ x: 1, y: 2 }} }};\nvar b := relation {{ {{ y: 2, z: 3 }} }};\nvar d := relation {{ {{ y: 2, w: 4 }} }};\nvar t := relation {{ {{ z: \"3\" }} }};\nvar e := compose[a, b, d];\nvar f := compose[a, b, t];\nvar g := compose[];{MAIN}"
        );
        let errors = errors(&source);
        let found: Vec<_> = errors.iter().map(|e| (e.code, &e.message[..])).collect();
        assert_eq!(
            found,
            [
                (
                    Code::CommonAttributes,
                    "the operands of `compose` have no attributes in common"
                ),
                (
                    Code::JoinTypeConflict,
                    "the operands of `compose` both have an attribute `z`, of different types"
                ),
                (Code::EmptyList, "`compose[]` has no value"),
            ]
        );
        let labels: Vec<_> = errors[0]
            .secondary
            .iter()
            .map(|l| (&source[l.span.start..l.span.end], &l.message[..]))
            .collect();
        assert_eq!(
            labels,
            [
                ("a, b", "composed, these have `x` and `z`"),
                ("d", "this has `w` and `y`")
            ]
        );
    }

    #[test]
    fn extend_constants() {
        let vars = variables(
            "var price := 100;\nvar items := relation { { name: \"tea\", price: 3, quantity: 4 }, { name: \"rice\", price: 2, quantity: 10 } };\nvar none := relation { price: integer } {};\nvar totals := items extend { total := price * quantity };\nvar point := tuple { x: 1, y: 2 } extend { sum := x + y, outside := price };\nvar empty := none extend { double := price * 2 };\nvar nested := items{name} extend { others := items{name} extend { mine := name } };",
        );
        let value = |name: &str| &vars.iter().find(|v| v.name == name).unwrap().init;
        // The attribute `price` hides the variable.
        assert_eq!(
            value("totals"),
            &variables(
                "var t := relation { { name: \"tea\", price: 3, quantity: 4, total: 12 }, { name: \"rice\", price: 2, quantity: 10, total: 20 } };"
            )[0]
            .init
        );
        assert_eq!(
            value("point"),
            &variables("var p := tuple { outside: 100, sum: 3, x: 1, y: 2 };")[0].init
        );
        // An empty relation still has the new attribute.
        assert_eq!(
            value("empty").ty().to_string(),
            "relation { double: integer, price: integer }"
        );
        // Inside a nested `extend`, `name` is the inner tuple's.
        let hir::Value::Relation(nested) = value("nested") else {
            panic!("a relation");
        };
        for tuple in &nested.tuples {
            let Some(hir::Value::Relation(others)) = tuple.get("others") else {
                panic!("a relation-valued attribute");
            };
            assert!(others.tuples.iter().all(|t| t.get("mine") == t.get("name")));
        }
    }

    #[test]
    fn extend_errors() {
        let source = format!(
            "var r := relation {{ {{ quantity: 1 }} }};\nvar a := r extend {{ total := quantty * 2 }};\nvar b := r extend {{ quantity := 2 }};\nvar c := r extend {{ d := 1, d := 2 }};\nvar e := r extend {{}};\nvar f := 1 extend {{ g := 2 }};\nvar h := relation {{ {{ n: 9223372036854775807 }} }} extend {{ m := n + 1 }};{MAIN}"
        );
        let errors = errors(&source);
        let found: Vec<_> = errors.iter().map(|e| (e.code, &e.message[..])).collect();
        assert_eq!(
            found,
            [
                (Code::UnknownName, "unknown value `quantty`"),
                (Code::AttributeExists, "attribute `quantity` already exists"),
                (
                    Code::DuplicateAttribute,
                    "attribute `d` is given more than once"
                ),
                (Code::EmptyDoesNothing, "`extend {}` adds nothing"),
                (
                    Code::WrongOperandType,
                    "`extend` can't be applied to integers"
                ),
                (Code::Overflow, "integer overflow"),
            ]
        );
        // The misspelled attribute is suggested, though not a variable.
        let fixed = fix(&source);
        assert!(fixed.contains("total := quantity * 2"), "{fixed}");
        assert!(fixed.contains("var e := r;"), "{fixed}");
        // Every attribute it names exists, so `replace` is likely meant.
        assert!(fixed.contains("r replace { quantity := 2 }"), "{fixed}");
        assert_eq!(
            errors[5].notes[1],
            "computing `m` for `tuple { n: 9223372036854775807 }`, which the operand has"
        );
    }

    #[test]
    fn replace_constants() {
        let vars = variables(
            "var items := relation { { name: \"tea\", price: 3 }, { name: \"rice\", price: 2 } };\nvar doubled := items replace { price := price * 2 };\nvar point := tuple { x: 1, y: 2 } replace { x := y, y := x };\nvar same := items replace { price := 1 };\nvar none := relation { price: integer } {} replace { price := price + 1 };",
        );
        let value = |name: &str| &vars.iter().find(|v| v.name == name).unwrap().init;
        assert_eq!(
            value("doubled"),
            &variables(
                "var t := relation { { name: \"tea\", price: 6 }, { name: \"rice\", price: 4 } };"
            )[0]
            .init
        );
        // Every value is computed from the old tuple, all at once.
        assert_eq!(
            value("point"),
            &variables("var p := tuple { x: 2, y: 1 };")[0].init
        );
        // Tuples that become equal are one tuple.
        let hir::Value::Relation(same) = value("same") else {
            panic!("a relation");
        };
        assert_eq!(same.tuples.len(), 2, "the names still differ");
        assert_eq!(
            value("none").ty().to_string(),
            "relation { price: integer }"
        );
        let vars =
            variables("var r := relation { { a: 1, b: 1 }, { a: 2, b: 1 } } replace { a := b };");
        let hir::Value::Relation(r) = &vars[0].init else {
            panic!("a relation");
        };
        assert_eq!(r.tuples.len(), 1);
    }

    #[test]
    fn image_constants() {
        let vars = variables(
            "var people := relation { { name: \"Ada\", city: \"London\" }, { name: \"Mary\", city: \"London\" }, { name: \"Edgar\", city: \"Portland\" } };\nvar cities := relation { { city: \"London\" }, { city: \"Nyeri\" } };\nvar residents := cities extend { residents := !!people };\nvar selves := people extend { me := !!people };\nvar pairs := people extend { others := !!(people rename {name as other}) };",
        );
        let value = |name: &str| &vars.iter().find(|v| v.name == name).unwrap().init;
        // Nyeri has no residents, so its image is empty.
        assert_eq!(
            value("residents"),
            &variables(
                "var r := relation { { city: \"London\", residents: relation { { name: \"Ada\" }, { name: \"Mary\" } } }, { city: \"Nyeri\", residents: relation { name: text } {} } };"
            )[0]
            .init
        );
        // Matching on every attribute leaves the tuple itself, without them:
        // the image is `relation {} { {} }`.
        let hir::Value::Relation(selves) = value("selves") else {
            panic!("a relation");
        };
        assert!(
            selves
                .tuples
                .iter()
                .all(|t| t.get("me") == Some(&hir::Value::dee()))
        );
        assert_eq!(
            value("pairs").ty().to_string(),
            "relation { city: text, name: text, others: relation { other: text } }"
        );
    }

    #[test]
    fn group_constants() {
        let vars = variables(
            "var r := relation { { k: 1, a: \"x\", b: true }, { k: 1, a: \"y\", b: false }, { k: 2, a: \"x\", b: true } };\nvar one := r group { g := {a, b} };\nvar two := r group { xs := {a}, ys := {b} };\nvar all := r group { g := {k, a, b} };\nvar none := r{k} group { g := {} };\nvar same := r group { a := {a} };\nvar empty := relation { k: integer, a: text } {} group { g := {a} };",
        );
        let value = |name: &str| &vars.iter().find(|v| v.name == name).unwrap().init;
        let expected = |source: &str| variables(source).remove(0).init;
        assert_eq!(
            value("one"),
            &expected(
                "var e := relation { { k: 1, g: relation { { a: \"x\", b: true }, { a: \"y\", b: false } } }, { k: 2, g: relation { { a: \"x\", b: true } } } };"
            )
        );
        // Simultaneous: each grouping is of the same matching tuples.
        assert_eq!(
            value("two"),
            &expected(
                "var e := relation { { k: 1, xs: relation { { a: \"x\" }, { a: \"y\" } }, ys: relation { { b: true }, { b: false } } }, { k: 2, xs: relation { { a: \"x\" } }, ys: relation { { b: true } } } };"
            )
        );
        // Every attribute: one tuple, of the whole relation.
        let hir::Value::Relation(all) = value("all") else {
            panic!("a relation");
        };
        assert_eq!(all.tuples.len(), 1);
        // No attribute: each tuple with `relation { {} }`.
        let hir::Value::Relation(none) = value("none") else {
            panic!("a relation");
        };
        assert!(
            none.tuples
                .iter()
                .all(|t| t.get("g") == Some(&hir::Value::dee()))
        );
        // A name it gathers is free.
        assert_eq!(
            value("same").ty().to_string(),
            "relation { a: relation { a: text }, b: boolean, k: integer }"
        );
        assert_eq!(
            value("empty").ty().to_string(),
            "relation { g: relation { a: text }, k: integer }"
        );
    }

    #[test]
    fn group_errors() {
        let source = format!(
            "var r := relation {{ {{ k: 1, a: \"x\", b: true }} }};\nvar c := r group {{ g := {{aa}} }};\nvar d := r group {{ g := {{a}}, h := {{a, b}} }};\nvar e := r group {{ g := {{a}}, g := {{b}} }};\nvar f := r group {{ k := {{a}} }};\nvar h := r group {{}};\nvar i := tuple {{ a: 1 }} group {{ g := {{a}} }};{MAIN}"
        );
        let errors = errors(&source);
        let found: Vec<_> = errors.iter().map(|e| (e.code, &e.message[..])).collect();
        assert_eq!(
            found,
            [
                (Code::UnknownAttribute, "unknown attribute `aa`"),
                (
                    Code::DuplicateAttribute,
                    "attribute `a` is gathered into both `g` and `h`"
                ),
                (
                    Code::DuplicateAttribute,
                    "attribute `g` is given more than once"
                ),
                (Code::AttributeExists, "attribute `k` already exists"),
                (Code::EmptyDoesNothing, "`group {}` gathers nothing"),
                (Code::WrongOperandType, "`group` can't be applied to tuples"),
            ]
        );
        let fixed = fix(&source);
        assert!(fixed.contains("r group { g := {a} }"), "{fixed}");
        assert!(fixed.contains("var h := r;"), "{fixed}");
    }

    #[test]
    fn ungroup_constants() {
        let vars = variables(
            "var r := relation { { k: 1, g: relation { { a: 1 }, { a: 2 } }, h: relation { { b: true }, { b: false } } }, { k: 2, g: relation { a: integer } {}, h: relation { { b: true } } } };\nvar one := r ungroup {g};\nvar both := r ungroup {g, h};\nvar dup := relation { { k: 1, g: relation { { a: 1 } } }, { k: 1, g: relation { { a: 1 }, { a: 2 } } } } ungroup {g};\nvar flat := relation { { k: 1, a: 1 }, { k: 1, a: 2 }, { k: 2, a: 1 } };\nvar back := flat group { g := {a} } ungroup {g};",
        );
        let value = |name: &str| &vars.iter().find(|v| v.name == name).unwrap().init;
        let tuples = |name: &str| match value(name) {
            hir::Value::Relation(r) => r.tuples.len(),
            other => panic!("a relation: {other:?}"),
        };
        // `k: 2` has an empty `g`, so it has no tuples.
        assert_eq!(tuples("one"), 2);
        assert_eq!(
            value("one").ty().to_string(),
            "relation { a: integer, h: relation { b: boolean }, k: integer }"
        );
        // Every combination of a tuple of each.
        assert_eq!(tuples("both"), 4);
        // Tuples that turn out equal are one.
        assert_eq!(tuples("dup"), 2);
        // Grouping then ungrouping gives the relation back.
        assert_eq!(value("back"), value("flat"));
    }

    #[test]
    fn ungroup_errors() {
        let source = format!(
            "var r := relation {{ {{ k: 1, a: 2, g: relation {{ {{ a: 1 }} }}, h: relation {{ {{ b: 1 }} }}, i: relation {{ {{ b: 2 }} }}, t: tuple {{ c: 1 }} }} }};\nvar b := r ungroup {{gg}};\nvar c := r ungroup {{k}};\nvar d := r ungroup {{t}};\nvar e := r ungroup {{g}};\nvar f := r ungroup {{h, i}};\nvar j := r ungroup {{}};\nvar l := tuple {{ g: relation {{ {{ a: 1 }} }} }} ungroup {{g}};{MAIN}"
        );
        let errors = errors(&source);
        let found: Vec<_> = errors.iter().map(|e| (e.code, &e.message[..])).collect();
        assert_eq!(
            found,
            [
                (Code::UnknownAttribute, "unknown attribute `gg`"),
                (
                    Code::WrongOperandType,
                    "`k` can't be ungrouped: it is `integer`"
                ),
                (
                    Code::WrongOperandType,
                    "`t` can't be ungrouped: it is `tuple { c: integer }`"
                ),
                (
                    Code::AttributeExists,
                    "ungrouping `g` would give two attributes `a`"
                ),
                (
                    Code::AttributeExists,
                    "ungrouping `i` would give two attributes `b`"
                ),
                (Code::EmptyDoesNothing, "`ungroup {}` ungroups nothing"),
                (
                    Code::WrongOperandType,
                    "`ungroup` can't be applied to tuples"
                ),
            ]
        );
        assert_eq!(
            errors[2].help,
            ["a tuple-valued attribute is unpacked with `unwrap`: `… unwrap {t}`"]
        );
        assert_eq!(
            errors[4].help,
            [
                "ungroup one, rename what it gave, then ungroup the other: `… ungroup {h} rename {b as h_b} ungroup {i}`"
            ]
        );
        let fixed = fix(&source);
        assert!(fixed.contains("r ungroup {g}"), "{fixed}");
        assert!(fixed.contains("var j := r;"), "{fixed}");
    }

    #[test]
    fn wrap_constants() {
        let vars = variables(
            "var t := tuple { a: 1, b: 2, c: 3 };\nvar one := t wrap { w := {a, b} };\nvar same := t wrap { a := {a, b} };\nvar two := t wrap { v := {a}, w := {b} };\nvar back := t wrap { w := {a, b} } unwrap {w};\nvar inner := tuple { w: tuple { w: 1, x: 2 } } unwrap {w};\nvar r := relation { { a: 1, b: 2 }, { a: 3, b: 4 } };\nvar rs := r wrap { w := {a} };\nvar rback := r wrap { w := {a} } unwrap {w};",
        );
        let value = |name: &str| &vars.iter().find(|v| v.name == name).unwrap().init;
        let expected = |source: &str| variables(source).remove(0).init;
        assert_eq!(
            value("one"),
            &expected("var e := tuple { c: 3, w: tuple { a: 1, b: 2 } };")
        );
        // A new name can be one it wraps.
        assert_eq!(
            value("same"),
            &expected("var e := tuple { a: tuple { a: 1, b: 2 }, c: 3 };")
        );
        assert_eq!(
            value("two"),
            &expected("var e := tuple { c: 3, v: tuple { a: 1 }, w: tuple { b: 2 } };")
        );
        // Unwrapping undoes wrapping, and an attribute can have the name of
        // the one it came from.
        assert_eq!(value("back"), value("t"));
        assert_eq!(value("inner"), &expected("var e := tuple { w: 1, x: 2 };"));
        assert_eq!(
            value("rs").ty().to_string(),
            "relation { b: integer, w: tuple { a: integer } }"
        );
        assert_eq!(value("rback"), value("r"));
    }

    #[test]
    fn wrap_errors() {
        let source = format!(
            "var t := tuple {{ a: 1, b: 2, w: tuple {{ a: 3 }}, g: relation {{ {{ c: 1 }} }} }};\nvar c := t wrap {{ v := {{a}}, x := {{a, b}} }};\nvar d := t wrap {{ b := {{a}} }};\nvar e := t wrap {{}};\nvar f := 1 wrap {{ v := {{a}} }};\nvar h := t unwrap {{w}};\nvar i := t unwrap {{g}};\nvar j := t unwrap {{a}};\nvar k := t unwrap {{}};{MAIN}"
        );
        let errors = errors(&source);
        let found: Vec<_> = errors.iter().map(|e| (e.code, &e.message[..])).collect();
        assert_eq!(
            found,
            [
                (
                    Code::DuplicateAttribute,
                    "attribute `a` is gathered into both `v` and `x`"
                ),
                (Code::AttributeExists, "attribute `b` already exists"),
                (Code::EmptyDoesNothing, "`wrap {}` gathers nothing"),
                (
                    Code::WrongOperandType,
                    "`wrap` can't be applied to integers"
                ),
                (
                    Code::AttributeExists,
                    "unwrapping `w` would give two attributes `a`"
                ),
                (
                    Code::WrongOperandType,
                    "`g` can't be unwrapped: it is `relation { c: integer }`"
                ),
                (
                    Code::WrongOperandType,
                    "`a` can't be unwrapped: it is `integer`"
                ),
                (Code::EmptyDoesNothing, "`unwrap {}` unwraps nothing"),
            ]
        );
        assert_eq!(
            errors[5].help,
            ["a relation-valued attribute is unpacked with `ungroup`: `… ungroup {g}`"]
        );
        assert_eq!(
            errors[4].help,
            ["rename the kept one first: `… rename {a as other_a} unwrap {w}`"]
        );
    }

    #[test]
    fn group_in_bodies() {
        let program = analyze_str(
            "operator f{r: relation { k: integer, a: text }} [\n    call debug_value{v: r group { g := {a} }};\n];\noperator main{} [];",
        )
        .unwrap();
        let f = program.operators.iter().find(|o| o.name == "f").unwrap();
        let hir::OperatorKind::Update { body } = &f.kind else {
            panic!("f is a user operator");
        };
        // An extend of the kept attributes, each with its image in `r`.
        let hir::Expr::Extend(operand, computed, values) = argument(&body[0]) else {
            panic!("an extend: {:?}", argument(&body[0]));
        };
        assert!(matches!(**operand, hir::Expr::Remove(..)));
        assert_eq!(computed.to_string(), "{ g: relation { a: text } }");
        assert!(matches!(values["g"], hir::Expr::Remove(..)));
    }

    #[test]
    fn image_errors() {
        let source = format!(
            "var r := relation {{ {{ a: 1, b: \"x\" }} }};\nvar s := relation {{ {{ c: 1 }} }};\nvar t := relation {{ {{ a: \"y\" }} }};\nvar u := !!r;\nvar v := r extend {{ w := !!s }};\nvar x := r extend {{ w := !!t }};\nvar y := r extend {{ w := !!1 }};\nvar z := r extend {{ w := !!q }};{MAIN}"
        );
        let errors = errors(&source);
        let found: Vec<_> = errors.iter().map(|e| (e.code, &e.message[..])).collect();
        assert_eq!(
            found,
            [
                (Code::ImageOutsideTuple, "`!!` has no tuple at hand here"),
                (
                    Code::CommonAttributes,
                    "`!!` needs an attribute in common with the tuple at hand"
                ),
                (
                    Code::JoinTypeConflict,
                    "the relation and the tuple at hand both have an attribute `a`, of different types"
                ),
                (Code::WrongOperandType, "`!!` can't be applied to integers"),
                (Code::UnknownName, "unknown value `q`"),
            ]
        );
        assert_eq!(errors[1].notes[0], "the tuple at hand has `a` and `b`");
        let fixed = fix(
            "var r := relation { { country: \"GH\" } };\nvar s := relation { { county: \"GH\", city: \"Kumasi\" } };\nvar t := r extend { cities := !!s };\n\noperator main{} [];",
        );
        assert!(
            fixed.contains("!!(s rename {county as country})"),
            "{fixed}"
        );
    }

    #[test]
    fn replace_errors() {
        let source = format!(
            "var r := relation {{ {{ price: 1, name: \"tea\" }} }};\nvar a := r replace {{ prise := 2 }};\nvar b := r replace {{ total := price * 2 }};\nvar c := r replace {{ price := 2, total := 3 }};\nvar d := r replace {{ price := 2e0 }};\nvar e := r replace {{ price := 1, price := 2 }};\nvar f := r replace {{}};\nvar g := 1 replace {{ h := 2 }};{MAIN}"
        );
        let errors = errors(&source);
        let found: Vec<_> = errors.iter().map(|e| (e.code, &e.message[..])).collect();
        assert_eq!(
            found,
            [
                (Code::UnknownAttribute, "unknown attribute `prise`"),
                (Code::UnknownAttribute, "unknown attribute `total`"),
                (Code::UnknownAttribute, "unknown attribute `total`"),
                (
                    Code::TypeMismatch,
                    "mismatched types: expected `integer`, found `approximate`"
                ),
                (
                    Code::DuplicateAttribute,
                    "attribute `price` is given more than once"
                ),
                (Code::EmptyDoesNothing, "`replace {}` replaces nothing"),
                (
                    Code::WrongOperandType,
                    "`replace` can't be applied to integers"
                ),
            ]
        );
        let fixed = fix(&source);
        // A misspelling is fixed; when no attribute named exists, `extend`
        // is likely meant; with some that do, there's no mechanical fix.
        assert!(fixed.contains("r replace { price := 2 }"), "{fixed}");
        assert!(fixed.contains("r extend { total := price * 2 }"), "{fixed}");
        assert!(
            fixed.contains("r replace { price := 2, total := 3 }"),
            "{fixed}"
        );
        assert!(fixed.contains("var f := r;"), "{fixed}");
        assert_eq!(
            errors[3].notes[1],
            "`replace` keeps each attribute's type; to change `price`'s, compute it under another name with `extend`, then remove the old one and rename the new: `… extend { new_price := … } remove {price} rename {new_price as price}`"
        );
    }

    #[test]
    fn extend_in_bodies() {
        let program = analyze_str(
            "var factor := 3;\noperator f{r: relation { n: integer }, t: tuple { n: integer }} [\n    call debug_value{v: r extend { m := n * factor }};\n    call debug_value{v: t extend { m := n }};\n    call debug_value{v: relation { { n: 2 } } extend { m := n * 2 }};\n];\noperator main{} [];",
        )
        .unwrap();
        let f = program.operators.iter().find(|o| o.name == "f").unwrap();
        let hir::OperatorKind::Update { body } = &f.kind else {
            panic!("f is a user operator");
        };
        let heading =
            hir::Heading::new([("n".to_string(), hir::Type::Scalar(hir::Scalar::Integer))].into());
        let n = || {
            Box::new(hir::Expr::Extract(
                Box::new(hir::Expr::Current(0, heading.clone())),
                "n".to_string(),
            ))
        };
        let added =
            hir::Heading::new([("m".to_string(), hir::Type::Scalar(hir::Scalar::Integer))].into());
        assert_eq!(
            argument(&body[0]),
            &hir::Expr::Extend(
                Box::new(hir::Expr::Parameter("r".to_string())),
                added.clone(),
                [(
                    "m".to_string(),
                    hir::Expr::Binary(
                        hir::BinaryOp::Multiply(hir::Numeric::Integer),
                        n(),
                        Box::new(hir::Expr::Variable(hir::VariableId(0)))
                    )
                )]
                .into()
            )
        );
        assert_eq!(
            argument(&body[1]),
            &hir::Expr::Extend(
                Box::new(hir::Expr::Parameter("t".to_string())),
                added,
                [("m".to_string(), *n())].into()
            )
        );
        // A constant operand is extended when compiling.
        assert!(matches!(argument(&body[2]), hir::Expr::Constant(_)));
    }

    #[test]
    fn tuple_joins() {
        let vars = variables(
            "var ada := tuple { name: \"Ada\", born: 1815 };\nvar place := tuple { name: \"Ada\", city: \"London\" };\nvar joined := ada join place;\nvar composed := ada compose place;\nvar timed := ada times tuple { year: 2024 };\nvar listed := join{ada, place, tuple { city: \"London\", country: \"UK\" }};",
        );
        let value = |name: &str| &vars.iter().find(|v| v.name == name).unwrap().init;
        let expected = |source: &str| variables(source).remove(0).init;
        assert_eq!(
            value("joined"),
            &expected("var e := tuple { born: 1815, city: \"London\", name: \"Ada\" };")
        );
        assert_eq!(
            value("composed"),
            &expected("var e := tuple { born: 1815, city: \"London\" };")
        );
        assert_eq!(
            value("timed").ty().to_string(),
            "tuple { born: integer, name: text, year: integer }"
        );
        assert_eq!(
            value("listed").ty().to_string(),
            "tuple { born: integer, city: text, country: text, name: text }"
        );
    }

    #[test]
    fn tuple_join_errors() {
        let source = format!(
            "var ada := tuple {{ name: \"Ada\", born: 1815 }};\nvar place := tuple {{ name: \"Ada Lovelace\", city: \"London\" }};\nvar a := ada join place;\nvar b := ada compose place;\nvar r := relation {{ {{ name: \"Ada\" }} }};\nvar c := r join ada;\nvar d := ada times place;\nvar e := ada join 1;\nvar f := ada extend {{ g := place join ada }};{MAIN}"
        );
        let errors = errors(&source);
        let found: Vec<_> = errors.iter().map(|e| (e.code, &e.message[..])).collect();
        assert_eq!(
            found,
            [
                (
                    Code::TupleMismatch,
                    "the tuples disagree on `name`, so they have no join"
                ),
                (
                    Code::TupleMismatch,
                    "the tuples disagree on `name`, so they have no join"
                ),
                (
                    Code::WrongOperandType,
                    "`join` can't combine a tuple with relations"
                ),
                (
                    Code::CommonAttributes,
                    "the operands of `times` have `name` in common"
                ),
                (
                    Code::WrongOperandType,
                    "`join` can't be applied to integers"
                ),
                (
                    Code::TupleMismatch,
                    "the tuples disagree on `name`, so they have no join"
                ),
            ]
        );
        assert_eq!(
            errors[0].primary.message,
            "`name` is `\"Ada\"` in one and `\"Ada Lovelace\"` in the other"
        );
        // The fix makes a relation of the tuple, which joins.
        let fixed = fix(&source);
        assert!(fixed.contains("r join relation { ada }"), "{fixed}");
        assert!(
            self::errors(&fixed)
                .iter()
                .all(|e| e.code != Code::WrongOperandType || !e.message.contains("combine")),
            "{fixed}"
        );
    }

    #[test]
    fn set_constants() {
        let vars = variables(
            "var a := relation { { n: 1 }, { n: 2 } };\nvar b := relation { { n: 2 }, { n: 3 } };\nvar c := relation { { n: 3 } };\nvar u := a union b;\nvar i := a intersect b;\nvar m := a minus b;\nvar ul := union{a, b, c};\nvar ml := minus[union{a, b}, c, relation { { n: 1 } }];",
        );
        let value = |name: &str| &vars.iter().find(|v| v.name == name).unwrap().init;
        let ns = |name: &str| match value(name) {
            hir::Value::Relation(r) => r
                .tuples
                .iter()
                .map(|t| t.get("n").cloned().unwrap())
                .collect::<Vec<_>>(),
            other => panic!("a relation: {other:?}"),
        };
        let ints = |ns: &[i64]| {
            ns.iter()
                .map(|&n| hir::Value::Integer(n))
                .collect::<Vec<_>>()
        };
        assert_eq!(ns("u"), ints(&[1, 2, 3]));
        assert_eq!(ns("i"), ints(&[2]));
        assert_eq!(ns("m"), ints(&[1]));
        assert_eq!(ns("ul"), ints(&[1, 2, 3]));
        assert_eq!(ns("ml"), ints(&[2]));
        // The tuples of exactly one operand, or of an odd number of a list.
        let vars = variables(
            "var a := relation { { n: 1 }, { n: 2 } };\nvar b := relation { { n: 2 }, { n: 3 } };\nvar x := a xunion b;\nvar xl := xunion{a, b, relation { { n: 3 }, { n: 4 } }};",
        );
        let xs = |name: &str| match &vars.iter().find(|v| v.name == name).unwrap().init {
            hir::Value::Relation(r) => r
                .tuples
                .iter()
                .map(|t| t.get("n").cloned().unwrap())
                .collect::<Vec<_>>(),
            other => panic!("a relation: {other:?}"),
        };
        assert_eq!(xs("x"), [hir::Value::Integer(1), hir::Value::Integer(3)]);
        assert_eq!(xs("xl"), [hir::Value::Integer(1), hir::Value::Integer(4)]);
    }

    #[test]
    fn checked_set_operators() {
        let vars = variables(
            "var a := relation { { n: 1 }, { n: 2 } };\nvar u := a d_union relation { { n: 3 } };\nvar m := a i_minus relation { { n: 1 } };\nvar ul := d_union{a, relation { { n: 3 } }, relation { { n: 4 } }};",
        );
        let value = |name: &str| &vars.iter().find(|v| v.name == name).unwrap().init;
        let count = |name: &str| match value(name) {
            hir::Value::Relation(r) => r.tuples.len(),
            other => panic!("a relation: {other:?}"),
        };
        assert_eq!(count("u"), 3);
        assert_eq!(count("m"), 1);
        assert_eq!(count("ul"), 4);
        let found: Vec<_> = errors(&format!(
            "var a := relation {{ {{ n: 1 }} }};\nvar b := a d_union a;\nvar c := a i_minus relation {{ {{ n: 2 }} }};\nvar d := i_minus[];{MAIN}"
        ))
        .into_iter()
        .map(|e| (e.code, e.message))
        .collect();
        assert_eq!(
            found,
            [
                (
                    Code::SharedTuple,
                    "the operands of `d_union` have a tuple in common".to_string()
                ),
                (
                    Code::MissingTuple,
                    "`i_minus` removes a tuple that isn't there".to_string()
                ),
                (Code::EmptyList, "`i_minus[]` has no value".to_string()),
            ]
        );
    }

    #[test]
    fn subset_comparisons() {
        let vars = variables(
            "var a := relation { { n: 1 } };\nvar b := relation { { n: 1 }, { n: 2 } };\nvar c := tuple { le: a <= b, lt: a < b, ge: a >= b, gt: a > b, same_le: a <= a, same_lt: a < a, back: b <= a };",
        );
        assert_eq!(
            vars[2].init,
            variables(
                "var e := tuple { back: false, ge: false, gt: false, le: true, lt: true, same_le: true, same_lt: false };"
            )[0]
            .init
        );
        let found: Vec<_> = errors(&format!(
            "var a := tuple {{ x: 1 }} < tuple {{ x: 2 }};{MAIN}"
        ))
        .into_iter()
        .map(|e| (e.code, e.message))
        .collect();
        assert_eq!(
            found,
            [(
                Code::WrongOperandType,
                "`<` can't be applied to tuples".to_string()
            )]
        );
    }

    #[test]
    fn set_errors() {
        let source = format!(
            "var people := relation {{ {{ name: \"Ada\", city: \"London\" }} }};\nvar more := relation {{ {{ name: \"Wangari\", town: \"Nyeri\" }} }};\nvar a := people union more;\nvar b := union{{}};\nvar c := tuple {{ x: 1 }} union tuple {{ y: 1 }};\nvar d := people intersect relation {{ {{ name: 1, city: \"x\" }} }};{MAIN}"
        );
        let errors = errors(&source);
        let found: Vec<_> = errors.iter().map(|e| (e.code, &e.message[..])).collect();
        assert_eq!(
            found,
            [
                (
                    Code::HeadingMismatch,
                    "the operands of `union` have different headings"
                ),
                (Code::EmptyList, "`union{}` has no value"),
                (Code::WrongOperandType, "`union` can't be applied to tuples"),
                (
                    Code::HeadingMismatch,
                    "the operands of `intersect` have different headings"
                ),
            ]
        );
        let fixed = fix(&source);
        assert!(
            fixed.contains("people union more rename {town as city}"),
            "{fixed}"
        );
        assert!(
            fixed.contains("tuple { x: 1 } join tuple { y: 1 }"),
            "{fixed}"
        );
    }

    #[test]
    fn matching_constants() {
        let vars = variables(
            "var people := relation { { name: \"Ada\", city: \"London\" }, { name: \"Wangari\", city: \"Nyeri\" } };\nvar kenyan := relation { { city: \"Nyeri\", country: \"Kenya\" } };\nvar a := people matching kenyan;\nvar b := people not matching kenyan;\nvar c := (people not matching kenyan) remove {city};",
        );
        let value = |name: &str| &vars.iter().find(|v| v.name == name).unwrap().init;
        let expected = |source: &str| variables(source).remove(0).init;
        assert_eq!(
            value("a"),
            &expected("var e := relation { { name: \"Wangari\", city: \"Nyeri\" } };")
        );
        assert_eq!(
            value("b"),
            &expected("var e := relation { { name: \"Ada\", city: \"London\" } };")
        );
        assert_eq!(
            value("c"),
            &expected("var e := relation { { name: \"Ada\" } };")
        );
    }

    #[test]
    fn matching_errors() {
        let source = format!(
            "var a := relation {{ {{ x: 1 }} }};\nvar b := a matching relation {{ {{ y: 1 }} }};\nvar c := tuple {{ x: 1 }} matching a;\nvar d := a not matching relation {{ {{ x: \"one\" }} }};{MAIN}"
        );
        let errors = errors(&source);
        let found: Vec<_> = errors.iter().map(|e| (e.code, &e.message[..])).collect();
        assert_eq!(
            found,
            [
                (
                    Code::CommonAttributes,
                    "the operands of `matching` have no attributes in common"
                ),
                (
                    Code::WrongOperandType,
                    "`matching` can't be applied to tuples"
                ),
                (
                    Code::JoinTypeConflict,
                    "the operands of `not matching` both have an attribute `x`, of different types"
                ),
            ]
        );
        // `times` is no fix for a semijoin.
        assert!(errors[0].help.iter().all(|h| !h.contains("times")));
    }

    #[test]
    fn restrict_constants() {
        let vars = variables(
            "var people := 1;\nvar cities := relation { { city: \"Paris\", people: 2100000 }, { city: \"Lyon\", people: 520000 } };\nvar big := cities where people > 1000000;\nvar all := cities where people > 0 or true;\nvar none := cities where false;\nvar paris := tuple from cities where city = \"Paris\";\nvar matched := cities where !!cities = relation { {} };",
        );
        let value = |name: &str| &vars.iter().find(|v| v.name == name).unwrap().init;
        let tuples = |name: &str| match value(name) {
            hir::Value::Relation(r) => r.tuples.len(),
            other => panic!("a relation: {other:?}"),
        };
        // The attribute `people` hides the variable.
        assert_eq!(tuples("big"), 1);
        assert_eq!(tuples("all"), 2);
        assert_eq!(tuples("none"), 0);
        assert_eq!(
            value("paris"),
            &variables("var e := tuple { city: \"Paris\", people: 2100000 };")[0].init
        );
        // Image relations work in a condition.
        assert_eq!(tuples("matched"), 2);
    }

    #[test]
    fn restrict_errors() {
        let source = format!(
            "var cities := relation {{ {{ city: \"Paris\", people: 2100000 }} }};\nvar a := cities where people;\nvar b := cities where people * 9000000000000 > 0;\nvar c := tuple {{ x: 1 }} where x = 1;\nvar d := cities where citty = \"Paris\";{MAIN}"
        );
        let errors = errors(&source);
        let found: Vec<_> = errors.iter().map(|e| (e.code, &e.message[..])).collect();
        assert_eq!(
            found,
            [
                (
                    Code::TypeMismatch,
                    "mismatched types: expected `boolean`, found `integer`"
                ),
                (Code::Overflow, "integer overflow"),
                (Code::WrongOperandType, "`where` can't be applied to tuples"),
                (Code::UnknownName, "unknown value `citty`"),
            ]
        );
        assert_eq!(
            errors[1].notes[1],
            "evaluating the condition for `tuple { city: \"Paris\", people: 2100000 }`, which the operand has"
        );
        assert!(fix(&source).contains("cities where city = \"Paris\""));
    }

    #[test]
    fn restrict_in_bodies() {
        let program = analyze_str(
            "operator f{r: relation { n: integer }} [\n    call debug_value{v: r where n > 1};\n    call debug_value{v: r where true};\n];\noperator main{} [];",
        )
        .unwrap();
        let f = program.operators.iter().find(|o| o.name == "f").unwrap();
        let hir::OperatorKind::Update { body } = &f.kind else {
            panic!("f is a user operator");
        };
        assert!(matches!(argument(&body[0]), hir::Expr::Restrict(..)));
        // A condition true of every tuple keeps them all.
        assert_eq!(argument(&body[1]), &hir::Expr::Parameter("r".to_string()));
    }

    #[test]
    fn taken_constants() {
        let vars = variables(
            "var people := relation { { name: \"Ada\", born: 1815 }, { name: \"Edgar\", born: 1923 } };\nvar ada := tuple from (people join relation { { name: \"Ada\" } });\nvar born := value from (people join relation { { name: \"Ada\" } }){born};\nvar value := value from relation { { from: 1 } };",
        );
        let value = |name: &str| &vars.iter().find(|v| v.name == name).unwrap().init;
        let expected = |source: &str| variables(source).remove(0).init;
        assert_eq!(
            value("ada"),
            &expected("var e := tuple { born: 1815, name: \"Ada\" };")
        );
        assert_eq!(value("born"), &hir::Value::Integer(1815));
        // `value` and `from` are names elsewhere.
        assert_eq!(value("value"), &hir::Value::Integer(1));
    }

    #[test]
    fn taken_errors() {
        let source = format!(
            "var people := relation {{ {{ name: \"Ada\", born: 1815 }}, {{ name: \"Edgar\", born: 1923 }} }};\nvar empty := relation {{ name: text }} {{}};\nvar a := tuple from empty;\nvar b := tuple from people;\nvar c := value from people;\nvar d := tuple from tuple {{ x: 1 }};{MAIN}"
        );
        let errors = errors(&source);
        let found: Vec<_> = errors.iter().map(|e| (e.code, &e.message[..])).collect();
        assert_eq!(
            found,
            [
                (
                    Code::NotOneTuple,
                    "the relation has no tuples, but exactly one is taken"
                ),
                (
                    Code::NotOneTuple,
                    "the relation has 2 tuples, but exactly one is taken"
                ),
                (
                    Code::WrongOperandType,
                    "`value from` takes a relation of one attribute"
                ),
                (
                    Code::WrongOperandType,
                    "`tuple from` can't be applied to tuples"
                ),
            ]
        );
        // Each attribute is offered, projected on.
        let offered: Vec<_> = errors[2]
            .suggestions
            .iter()
            .map(|s| s.edits[0].replacement.as_str())
            .collect();
        assert_eq!(offered, ["people{born}", "people{name}"]);
        // From a tuple: the tuple itself, or its only attribute's value.
        let fixed = fix(&format!(
            "var t := tuple {{ x: 1 }};\nvar a := tuple from t;\nvar b := value from t;{MAIN}"
        ));
        assert!(fixed.contains("var a := t;"), "{fixed}");
        assert!(fixed.contains("var b := t.x;"), "{fixed}");
    }

    #[test]
    fn relations_of_tuple_expressions() {
        let vars = variables(
            "var ada := tuple { name: \"Ada\", born: 1815 };\nvar people := relation { ada, { name: \"Edgar\", born: 1923 } };",
        );
        let hir::Value::Relation(people) = &vars[1].init else {
            panic!("a relation");
        };
        assert_eq!(people.tuples.len(), 2);
        let found: Vec<_> = errors(&format!(
            "var a := tuple {{ x: 1 }};\nvar r := relation {{ 1 }};\nvar s := relation {{ a, tuple {{ y: 2 }} }};\nvar t := relation {{ a, a }};{MAIN}"
        ))
        .into_iter()
        .map(|e| (e.code, e.message))
        .collect();
        assert_eq!(
            found,
            [
                (
                    Code::TypeMismatch,
                    "mismatched types: expected a tuple, found `integer`".to_string()
                ),
                (Code::UnknownAttribute, "unknown attribute `y`".to_string()),
                (
                    Code::DuplicateTuple,
                    "duplicate tuple in a relation value".to_string()
                ),
            ]
        );
    }

    #[test]
    fn joins_in_bodies() {
        let program = analyze_str(
            "var cities := relation { { city: \"Kumasi\", country: \"Ghana\" } };\noperator f{r: relation { country: text, continent: text }} [\n    call debug_value{v: cities join r};\n    call debug_value{v: cities compose r};\n    call debug_value{v: cities join r join cities};\n];\noperator main{} [];",
        )
        .unwrap();
        let f = program.operators.iter().find(|o| o.name == "f").unwrap();
        let hir::OperatorKind::Update { body } = &f.kind else {
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
        let hir::OperatorKind::Update { body } = &f.kind else {
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
                let hir::OperatorKind::Update { body } = &f.kind else {
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
                let hir::OperatorKind::Update { body } = &f.kind else {
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

    /// A program with `variables`, whose `main` is `body`.
    fn assigning(variables: &str, body: &str) -> String {
        format!("{variables}\noperator main{{}} [ {body} ];")
    }

    const POETS: &str = "var poets := relation { { name: \"Ada\", born: 1815 } } key { name };\nvar address := tuple { city: \"Lima\" };\nvar n := 0;";

    #[test]
    fn assignments_expand_their_shorthands() {
        let poets = || Box::new(hir::Expr::Variable(hir::VariableId(0)));
        let assigned = |body: &str| {
            let body = main_body(&assigning(POETS, body));
            let [hir::Statement::Assign(assignments)] = &body[..] else {
                panic!("{body:?}");
            };
            assignments
                .iter()
                .map(|a| (a.target, a.value.clone()))
                .collect::<Vec<_>>()
        };
        let relation = |name: &str| {
            let tuple = hir::TupleValue::new(
                [
                    ("born".to_string(), hir::Value::Integer(1)),
                    ("name".to_string(), hir::Value::Text(name.to_string())),
                ]
                .into(),
            );
            let heading = hir::Heading::new(
                [
                    ("born".to_string(), hir::Type::Scalar(hir::Scalar::Integer)),
                    ("name".to_string(), hir::Type::Scalar(hir::Scalar::Text)),
                ]
                .into(),
            );
            Box::new(hir::Expr::Constant(hir::Value::Relation(
                hir::RelationValue {
                    heading,
                    tuples: [tuple].into(),
                },
            )))
        };
        let target = hir::VariableId(0);
        assert_eq!(
            assigned(r#"insert poets relation { { name: "Rumi", born: 1 } };"#),
            [(
                target,
                hir::Expr::Combine(hir::Combination::Union, poets(), relation("Rumi"))
            )]
        );
        // Merged in a block: what is removed first, then what is added.
        assert_eq!(
            assigned(
                r#"assign { d_insert poets relation { { name: "Rumi", born: 1 } }; i_delete poets relation { { name: "Ada", born: 1 } }; };"#
            ),
            [(
                target,
                hir::Expr::Combine(
                    hir::Combination::DisjointUnion,
                    Box::new(hir::Expr::Combine(
                        hir::Combination::IncludedMinus,
                        poets(),
                        relation("Ada")
                    )),
                    relation("Rumi")
                )
            )]
        );
        // The tuples for which the condition is false are kept.
        let [(_, hir::Expr::Restrict(operand, condition))] =
            &assigned("delete poets where born > 1900;")[..]
        else {
            panic!("a restriction");
        };
        assert_eq!(*operand, poets());
        assert!(matches!(
            **condition,
            hir::Expr::Unary(hir::UnaryOp::Not, _)
        ));
        // Those for which it is true are replaced, in union with the others.
        let [(_, hir::Expr::Combine(hir::Combination::Union, kept, changed))] =
            &assigned("update poets { born := born + 1 } where name = \"Ada\";")[..]
        else {
            panic!("a union");
        };
        assert!(matches!(**kept, hir::Expr::Restrict(..)));
        assert!(matches!(**changed, hir::Expr::Extend(..)));
        // A tuple's update is a replace of it.
        let [(target, hir::Expr::Extend(operand, ..))] =
            &assigned("update address { city := city || \"!\" };")[..]
        else {
            panic!("an extension");
        };
        assert_eq!(*target, hir::VariableId(1));
        assert_eq!(**operand, hir::Expr::Variable(hir::VariableId(1)));
    }

    #[test]
    fn a_blocks_changes_to_a_variable_are_merged() {
        let main = |body: &str| main_body(&assigning(POETS, body));
        // Two updates: the old value less the tuples either selects, which
        // must be different, with the tuples each makes.
        let body = main(
            "assign { update poets { born := 1 } where name = \"Ada\"; update poets { born := 2 } where born > 1900; };",
        );
        let [hir::Statement::Assign(assignments)] = &body[..] else {
            panic!("{body:?}");
        };
        let [hir::Assignment { value, .. }] = &assignments[..] else {
            panic!("one assignment: {assignments:?}");
        };
        let hir::Expr::Combine(hir::Combination::Union, inner, second) = value else {
            panic!("{value:?}");
        };
        let hir::Expr::Combine(hir::Combination::Union, kept, first) = &**inner else {
            panic!("{inner:?}");
        };
        assert!(matches!(**first, hir::Expr::Extend(..)));
        assert!(matches!(**second, hir::Expr::Extend(..)));
        let hir::Expr::Combine(hir::Combination::Minus, _, removed) = &**kept else {
            panic!("{kept:?}");
        };
        assert!(matches!(
            **removed,
            hir::Expr::Combine(hir::Combination::DisjointChanges, ..)
        ));
        // A tuple's updates of different attributes are one.
        let body = main("assign { update address { city := \"Cusco\" }; n := n + 1; };");
        let [hir::Statement::Assign(assignments)] = &body[..] else {
            panic!("{body:?}");
        };
        assert_eq!(assignments.len(), 2);
        let source = "var t := tuple { a: 1, b: 2 };\noperator main{} [ assign { update t { a := b }; update t { b := a }; }; ];";
        let body = main_body(source);
        let [hir::Statement::Assign(assignments)] = &body[..] else {
            panic!("{body:?}");
        };
        let [
            hir::Assignment {
                value: hir::Expr::Extend(_, _, values),
                ..
            },
        ] = &assignments[..]
        else {
            panic!("one extension: {assignments:?}");
        };
        assert_eq!(values.len(), 2, "both attributes, each from the old tuple");
        // A `d_insert` is checked against the tuples kept, not those an
        // `update` makes, before or after it.
        for block in [
            "assign { update poets { born := 1 }; d_insert poets relation { { name: \"Rumi\", born: n } }; };",
            "assign { d_insert poets relation { { name: \"Rumi\", born: n } }; update poets { born := 1 }; };",
        ] {
            let body = main(block);
            let [hir::Statement::Assign(assignments)] = &body[..] else {
                panic!("{body:?}");
            };
            let [hir::Assignment { value, .. }] = &assignments[..] else {
                panic!("one assignment: {assignments:?}");
            };
            let hir::Expr::Combine(hir::Combination::Union, inserted, updated) = value else {
                panic!("{value:?}");
            };
            assert!(matches!(
                **inserted,
                hir::Expr::Combine(hir::Combination::DisjointUnion, ..)
            ));
            assert!(matches!(**updated, hir::Expr::Extend(..)), "{block}");
        }
    }

    #[test]
    fn conflicting_changes_known_when_compiling() {
        let conflict = |body: &str| codes(&assigning(POETS, body));
        assert_eq!(
            conflict("assign { n := 1; n := 2; };"),
            [Code::ConflictingChanges]
        );
        assert_eq!(
            conflict("assign { poets := relation {}; delete poets where born > 1; };"),
            [Code::ConflictingChanges]
        );
        assert_eq!(
            conflict(
                "assign { update address { city := \"a\" }; update address { city := \"b\" }; };"
            ),
            [Code::ConflictingChanges]
        );
        let errors = errors(&assigning(
            POETS,
            r#"assign { i_delete poets relation { { name: "Ada", born: 1815 } }; i_delete poets relation { { name: "Ada", born: 1815 } }; };"#,
        ));
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].code, Code::ConflictingChanges);
        assert_eq!(
            errors[0].message,
            "two changes to `poets` remove the same tuple"
        );
        assert_eq!(
            codes(&assigning(POETS, "assign {};")),
            [Code::EmptyDoesNothing]
        );
    }

    #[test]
    fn only_variables_are_assigned() {
        let source = "var n := 0;\noperator f{n: integer, p: integer} [ p := 1; n := 2; ];\noperator main{} [ main := 1; m := 2; ];";
        let errors = errors(source);
        let codes: Vec<_> = errors.iter().map(|e| e.code).collect();
        assert_eq!(
            codes,
            [
                Code::NotAssignable,
                Code::NotAssignable,
                Code::NotAssignable,
                Code::UnknownName
            ]
        );
        assert_eq!(errors[0].message, "parameter `p` can't be assigned");
        // The parameter `n` hides the variable.
        assert!(
            errors[1].help[0].contains("rename the parameter"),
            "{:?}",
            errors[1].help
        );
        assert_eq!(errors[2].message, "`main` is an operator, not a variable");
    }

    #[test]
    fn assigned_values_have_the_variables_type() {
        let inferred = errors(&assigning(POETS, "n := \"one\";"));
        assert_eq!(inferred.len(), 1);
        assert_eq!(inferred[0].code, Code::TypeMismatch);
        assert_eq!(
            inferred[0].secondary[0].message,
            "`n` has this value's type"
        );
        assert!(
            inferred[0].suggestions.is_empty(),
            "the variable's type stays"
        );
        let declared = errors(&assigning("var m: integer;", "m := true;"));
        assert_eq!(declared[0].code, Code::TypeMismatch);
        assert_eq!(
            declared[0].secondary[0].message,
            "`m` is declared with this type"
        );
        // A tuple's attributes are checked against the variable's.
        assert_eq!(
            codes(&assigning(POETS, "address := tuple { town: \"Lima\" };")),
            [Code::MissingAttribute, Code::UnknownAttribute]
        );
    }

    #[test]
    fn shorthands_need_the_right_variables() {
        assert_eq!(
            codes(&assigning(
                POETS,
                "insert address tuple { city: \"Lima\" };"
            )),
            [Code::WrongOperandType]
        );
        assert_eq!(
            codes(&assigning(
                POETS,
                "update address { city := \"Quito\" } where city = \"Lima\";"
            )),
            [Code::WrongOperandType]
        );
        assert_eq!(
            codes(&assigning(POETS, "update poets { died := 1852 };")),
            [Code::UnknownAttribute]
        );
        assert_eq!(
            codes(&assigning(POETS, "update poets {};")),
            [Code::EmptyDoesNothing]
        );
        assert_eq!(
            codes(&assigning(POETS, "delete poets where born;")),
            [Code::TypeMismatch]
        );
    }

    /// The program with `POETS` whose `main` is `body`, which has one
    /// error, with `code`, with its one fix applied, which leaves none.
    fn fixed(code: Code, body: &str) -> String {
        let source = assigning(POETS, body);
        let errors = errors(&source);
        let [error] = &errors[..] else {
            panic!("one error: {errors:#?}");
        };
        assert_eq!(error.code, code, "{body}");
        let [edit] = &error.suggestions[0].edits[..] else {
            panic!("one edit: {:?}", error.suggestions);
        };
        let mut fixed = source.clone();
        fixed.replace_range(edit.span.start..edit.span.end, &edit.replacement);
        assert!(analyze_str(&fixed).is_ok(), "{fixed}");
        fixed
    }

    #[test]
    fn a_change_of_every_tuple_says_so() {
        let fixed = |body: &str| fixed(Code::EveryTuple, body);
        // Each condition is `true` once folded.
        for condition in ["true", "1 = 1", "born > 1 or true"] {
            assert_eq!(
                fixed(&format!("delete poets where {condition};")),
                assigning(POETS, "poets := relation {};")
            );
            assert_eq!(
                fixed(&format!("update poets {{ born := 1 }} where {condition};")),
                assigning(POETS, "update poets { born := 1 };")
            );
        }
        assert_eq!(
            fixed("i_delete poets poets;"),
            assigning(POETS, "poets := relation {};")
        );
        let tuple_dependent =
            "delete poets where born > 1; update poets { born := 1 } where born < 1;";
        assert!(analyze_str(&assigning(POETS, tuple_dependent)).is_ok());
        // The empty relation replaces a block's one change to the variable.
        for deletion in ["delete poets where true", "i_delete poets poets"] {
            assert_eq!(
                fixed(&format!("assign {{ {deletion}; n := 1; }};")),
                assigning(POETS, "assign { poets := relation {}; n := 1; };")
            );
            // With others, it would conflict with them.
            let combined = errors(&assigning(
                POETS,
                &format!(
                    r#"assign {{ {deletion}; insert poets relation {{ {{ name: "Rumi", born: 1207 }} }}; }};"#
                ),
            ));
            assert_eq!(combined.len(), 1);
            assert_eq!(combined[0].code, Code::EveryTuple);
            assert!(combined[0].suggestions.is_empty());
            assert!(
                combined[0].help[0].contains("`poets := …`"),
                "{:?}",
                combined[0].help
            );
        }
    }

    #[test]
    fn assignments_that_change_nothing_go() {
        let fixed = |body: &str| fixed(Code::NoChange, body);
        // A variable's own value, once folded.
        for body in ["n := n;", "n := (n);", "n := n + 0;"] {
            assert_eq!(
                fixed(&format!("n := 1; {body}")),
                assigning(POETS, "n := 1;")
            );
        }
        // No tuples, inserted or deleted.
        for keyword in ["insert", "d_insert", "i_delete"] {
            assert_eq!(
                fixed(&format!("n := 1; {keyword} poets relation {{}};")),
                assigning(POETS, "n := 1;")
            );
        }
        // A condition that is `false` once folded selects no tuple.
        for condition in ["false", "not true", "born > 1 and false"] {
            for change in ["delete poets", "update poets { born := 1 }"] {
                assert_eq!(
                    fixed(&format!("n := 1; {change} where {condition};")),
                    assigning(POETS, "n := 1;")
                );
            }
        }
        // From a block, and with it if nothing else is in it.
        assert_eq!(
            fixed("assign { delete poets where false; n := 1; };"),
            assigning(POETS, "assign { n := 1; };")
        );
        assert_eq!(
            fixed("n := 1; assign { delete poets where false; };"),
            assigning(POETS, "n := 1;")
        );
        // Of the variable into itself, as its one change.
        for keyword in ["insert", "d_insert"] {
            assert_eq!(
                fixed(&format!("n := 1; {keyword} poets poets;")),
                assigning(POETS, "n := 1;")
            );
            assert_eq!(
                fixed(&format!("assign {{ {keyword} poets poets; n := 2; }};")),
                assigning(POETS, "assign { n := 2; };")
            );
        }
        // With changes that remove tuples, it adds them back.
        let back = "assign { update poets { born := born + 1 }; insert poets poets; };";
        assert!(analyze_str(&assigning(POETS, back)).is_ok());
        // Each attribute's own value, of a relation, with or without a
        // condition, or of a tuple.
        for body in [
            "update poets { born := born };",
            "update poets { born := born, name := name } where born > 1;",
            "update address { city := city };",
        ] {
            assert_eq!(
                fixed(&format!("n := 1; {body}")),
                assigning(POETS, "n := 1;")
            );
        }
        // With a block of nothing else.
        assert_eq!(
            fixed("n := 1; assign { n := n; };"),
            assigning(POETS, "n := 1;")
        );
        // Only one attribute's: that assignment goes.
        assert_eq!(
            fixed("update poets { name := name, born := born + 1 };"),
            assigning(POETS, "update poets { born := born + 1 };")
        );
        assert_eq!(
            fixed("update poets { born := born + 1, name := name };"),
            assigning(POETS, "update poets { born := born + 1 };")
        );
        // In a `replace`, too.
        let source =
            "var r := relation { { a: 1, b: 2 } } replace { a := a };\noperator main{} [];";
        let errors = errors(source);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].code, Code::NoChange);
        let edit = &errors[0].suggestions[0].edits[0];
        assert_eq!(
            &source[edit.span.start..edit.span.end],
            " replace { a := a }"
        );
        assert_eq!(
            codes(
                "var r := relation { { a: 1, b: 2 } } replace { a := a, b := 3 };\noperator main{} [];"
            ),
            [Code::NoChange]
        );
        // What changes something is fine.
        let changes = r#"n := n + 1; insert poets relation { { name: "Rumi", born: 1207 } }; update address { city := city || "!" };"#;
        assert!(analyze_str(&assigning(POETS, changes)).is_ok());
    }

    #[test]
    fn keys_of_values_known_when_compiling_are_checked() {
        let body =
            r#"poets := relation { { name: "Ada", born: 1815 }, { name: "Ada", born: 1816 } };"#;
        let errors = errors(&assigning(POETS, body));
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].code, Code::KeyViolated);
        // Pointing at the second tuple, with the first as a secondary.
        let source = assigning(POETS, body);
        assert_eq!(
            &source[errors[0].span().start..errors[0].span().end],
            r#"{ name: "Ada", born: 1816 }"#
        );
        // So it is in a block.
        let block = format!("assign {{ {body} n := 1; }};");
        assert_eq!(codes(&assigning(POETS, &block)), [Code::KeyViolated]);
    }

    /// What `source` has at `span`.
    fn at(source: &str, span: Span) -> &str {
        &source[span.start..span.end]
    }

    const SAME: &str =
        "operator same{a: T, b: T}: boolean types { T: any type } [ return a = b; ];\n";

    #[test]
    fn generic_operators_are_called_with_their_arguments_types() {
        // Evaluated when compiling, for each type.
        let program = analyze_str(&format!(
            "{SAME}var t := same{{a: 1, b: 1}};\nvar f := same{{b: \"Lhasa\", a: \"Lima\"}};\nvar r := same{{a: relation {{ {{ x: 1 }} }}, b: relation {{}}}};{MAIN}"
        ))
        .unwrap();
        let values: Vec<_> = program.variables.iter().map(|v| v.init.clone()).collect();
        assert_eq!(
            values,
            [
                hir::Value::Boolean(true),
                hir::Value::Boolean(false),
                hir::Value::Boolean(false)
            ]
        );
        // When running, the call says which types.
        let body = main_body(&format!(
            "{SAME}var n := 1;\noperator main{{}} [ call debug_value{{v: same{{a: n, b: 2}}}}; ];"
        ));
        let hir::Expr::Call(call) = argument(&body[0]) else {
            panic!("a call");
        };
        assert_eq!(call.types, [hir::Type::Scalar(hir::Scalar::Integer)]);
        // So does a call of `debug_value`, whose `any type` has no name.
        let hir::Statement::Call(call) = &body[0] else {
            panic!("a call");
        };
        assert_eq!(call.types, [hir::Type::Scalar(hir::Scalar::Boolean)]);
    }

    #[test]
    fn a_type_parameter_is_one_type_for_every_argument() {
        let source = format!("{SAME}var x := same{{a: 1, b: \"Lima\"}};{MAIN}");
        let errors = errors(&source);
        assert_eq!(errors.len(), 1, "{errors:#?}");
        assert_eq!(errors[0].code, Code::TypeMismatch);
        assert_eq!(at(&source, errors[0].span()), "\"Lima\"");
        let given = &errors[0].secondary[0];
        assert_eq!(at(&source, given.span), "1");
        assert_eq!(given.message, "`T` is given this argument's type");
        // `any relation` takes only relations, and `any tuple` only tuples.
        let source = "operator f{r: any relation, t: any tuple} [];\noperator main{} [ call f{r: 1, t: relation { { a: 1 } }}; ];";
        let messages = messages(source);
        assert_eq!(
            messages,
            [
                "mismatched types: expected a relation, found `integer`",
                "mismatched types: expected a tuple, found `relation { a: integer }`"
            ]
        );
        // A structured type takes its type parameters' types from its
        // argument's, and gives them to others.
        let only = "operator only{r: relation { v: T }}: T types { T: any type } [ return value from r; ];\n";
        let vars = variables(&format!(
            "{only}var a := only{{r: relation {{ {{ v: 5 }} }}}};\nvar b := only{{r: relation {{ {{ v: \"Kyiv\" }} }}}};"
        ));
        assert_eq!(vars[0].init, hir::Value::Integer(5));
        assert_eq!(vars[1].init, hir::Value::Text("Kyiv".to_string()));
        let pick = "operator pick{a: T, r: relation { v: T }}: boolean types { T: any type } [ return r = relation {}; ];\n";
        // `relation {}` is expected to have `T`'s heading, once `a` gives
        // `T` its type, whichever is written first.
        for args in ["a: 1, r: relation {}", "r: relation {}, a: 1"] {
            let vars = variables(&format!("{pick}var p := pick{{{args}}};"));
            assert_eq!(vars[0].init, hir::Value::Boolean(true));
        }
        assert_eq!(
            codes(&format!(
                "{pick}var p := pick{{a: 1, r: relation {{ {{ v: \"x\" }} }}}};{MAIN}"
            )),
            [Code::TypeMismatch]
        );
    }

    #[test]
    fn generic_bodies_are_checked_for_every_type() {
        // Only `=` and `<>` apply to any type.
        let source = "operator f{a: T, b: T}: boolean types { T: any type } [ return a < b; ];\noperator main{} [];";
        assert_eq!(codes(source), [Code::WrongOperandType]);
        assert_eq!(messages(source), ["`<` can't be applied to values of `T`"]);
        let source =
            "operator f{a: T}: T types { T: any type } [ return a + a; ];\noperator main{} [];";
        assert_eq!(codes(source), [Code::WrongOperandType]);
        // Two type parameters are two types.
        let source = "operator f{a: T, b: U, c: T, d: U}: boolean types { T: any type, U: any type } [ return a = b; ];\noperator main{} [];";
        assert_eq!(codes(source), [Code::TypeMismatch]);
        // So are two written `any type`, which one declared can make one.
        let source =
            "operator f{a: any type, b: any type}: boolean [ return a = b; ];\noperator main{} [];";
        let errors = errors(source);
        assert_eq!(errors[0].code, Code::TypeMismatch);
        assert!(
            errors[0]
                .notes
                .iter()
                .any(|n| n.contains("types { T: any type }")),
            "{errors:#?}"
        );
        // Relations are ordered by inclusion, so `any relation` is.
        analyze_str(
            "operator sub{r: R, s: R}: boolean types { R: any relation } [ return r <= s; ];\nvar yes := sub{r: relation { { a: 1 } }, s: relation { { a: 1 }, { a: 2 } }};\noperator main{} [];",
        )
        .unwrap();
        // A type parameter can be written in the body.
        let program = analyze_str(
            "operator none{a: T}: relation { v: T } types { T: any type } [ return relation { v: T } {}; ];\nvar e := none{a: \"Accra\"};\noperator main{} [];",
        )
        .unwrap();
        assert_eq!(program.variables[0].ty.to_string(), "relation { v: text }");
    }

    #[test]
    fn type_parameters_are_declared_once_and_determined() {
        // Never used, or only where no argument gives it a type.
        let source = "operator f{a: integer} types { T: any type } [];\noperator main{} [];";
        assert_eq!(codes(source), [Code::UndeterminedTypeParameter]);
        assert_eq!(
            fix(source),
            "operator f{a: integer} [];\noperator main{} [];"
        );
        let source =
            "operator f{a: T, b: T} types { T: any type, U: any type } [];\noperator main{} [];";
        assert_eq!(
            fix(source),
            "operator f{a: T, b: T} types { T: any type } [];\noperator main{} [];"
        );
        let source = "operator f{a: integer}: relation { v: T } types { T: any type } [ return relation {}; ];\noperator main{} [];";
        assert_eq!(codes(source), [Code::UndeterminedTypeParameter]);
        assert!(errors(source)[0].suggestions.is_empty());
        // Used once, it needs no name.
        let source =
            "operator f{r: R, n: integer} types { R: any relation } [];\noperator main{} [];";
        assert_eq!(codes(source), [Code::TypeParameterNamedOnce]);
        assert_eq!(
            fix(source),
            "operator f{r: any relation, n: integer} [];\noperator main{} [];"
        );
        // Twice, or in the result or the body, it does.
        analyze_str(
            "operator f{a: T}: T types { T: any type } [ return a; ];\noperator main{} [];",
        )
        .unwrap();
        analyze_str(
            "operator f{a: T} types { T: any type } [ call debug_value{v: relation { a: T } {}}; ];\noperator main{} [];",
        )
        .unwrap();
        // It is `any …`, under a name of its own.
        let source = "operator f{a: T, b: T} types { T: integer } [];\noperator main{} [];";
        assert_eq!(codes(source), [Code::TypeParameterNotAny]);
        let source =
            "operator f{a: text, b: text} types { text: any type } [];\noperator main{} [];";
        assert_eq!(codes(source), [Code::DuplicateName]);
        let source =
            "operator f{a: T, b: T} types { T: any type, T: any tuple } [];\noperator main{} [];";
        assert_eq!(codes(source), [Code::DuplicateName]);
        // A misspelled one is found.
        let source = "operator f{a: Key, b: Kye} types { Key: any type } [];\noperator main{} [];";
        assert_eq!(
            fix(source),
            "operator f{a: Key, b: Key} types { Key: any type } [];\noperator main{} [];"
        );
        // Variables have specific types.
        assert_eq!(
            codes(
                "operator f{a: T, b: T} types { T: any type } [];\nvar x: T;\noperator main{} [];"
            ),
            [Code::UnknownType]
        );
    }

    #[test]
    fn recursion_with_larger_types_is_reported() {
        let source = "operator f{a: T, b: T} types { T: any type } [ call f{a: tuple { x: a }, b: tuple { x: b }}; ];\noperator main{} [ call f{a: 1, b: 2}; ];";
        let errors = errors(source);
        assert_eq!(errors.len(), 1, "{errors:#?}");
        assert_eq!(errors[0].code, Code::GrowingRecursion);
        assert_eq!(
            errors[0].primary.message,
            "gives `T` the type `tuple { x: T }`"
        );
        // Through another operator too.
        let source = "operator f{a: any type} [ call g{b: relation { tuple { x: a } }}; ];\noperator g{b: any type} [ call f{a: b}; ];\noperator main{} [];";
        assert_eq!(codes(source), [Code::GrowingRecursion]);
        // Passing the type on as it is is fine, and so is a larger type
        // that doesn't lead back.
        analyze_str(
            "operator f{a: any type} [ call f{a: a}; call g{b: tuple { x: a }}; ];\noperator g{b: any type} [];\noperator main{} [];",
        )
        .unwrap();
    }

    const PLOTS: &str = "var plots := relation {\n    { plot: 1, region: \"Nyeri\", of: 1200 },\n    { plot: 2, region: \"Nyeri\", of: 950 },\n    { plot: 3, region: \"Mendoza\", of: 220 },\n}\nkey { plot };\n";

    const VALUES: &str = "operator values{r: any relation, of: T per r}: relation { v: T } types { T: any type } [ return r{of} rename {of as v}; ];\n";

    #[test]
    fn per_arguments_are_evaluated_for_each_tuple() {
        let vars = variables(&format!(
            "{PLOTS}{VALUES}var regions := values{{r: plots, of: region}};\nvar wet := values{{r: plots, of: of > 1000}};"
        ));
        // The operator's `of` is its own, not the plots' attribute of that
        // name, which is in scope in the argument.
        assert_eq!(
            text(&vars[1].init),
            r#"relation { v: text } { { v: "Mendoza" }, { v: "Nyeri" } }"#
        );
        assert_eq!(
            text(&vars[2].init),
            "relation { v: boolean } { { v: false }, { v: true } }"
        );
        // The operator sees the relation's tuples with `of`, and their own
        // attributes wrapped, which it can't name.
        let program = analyze_str(&format!("{VALUES}{MAIN}")).unwrap();
        let values = program
            .operators
            .iter()
            .find(|o| o.name == "values")
            .unwrap();
        assert_eq!(values.params.len(), 1);
        assert_eq!(values.params["r"].to_string(), "relation { of: T, … }");
        // A relation passed on keeps its tuples, and its `per` attribute.
        let again = "operator again{r: any relation, of: T per r}: relation { v: T } types { T: any type } [ return values{r: r, of: of}; ];\n";
        let vars = variables(&format!(
            "{PLOTS}{VALUES}{again}var regions := again{{r: plots, of: region}};"
        ));
        assert_eq!(
            text(&vars[1].init),
            r#"relation { v: text } { { v: "Mendoza" }, { v: "Nyeri" } }"#
        );
    }

    /// `value` as source text.
    fn text(value: &hir::Value) -> String {
        errors::source_text(value).unwrap()
    }

    #[test]
    fn per_errors() {
        // `per` names an `any relation` parameter of the operator.
        let source = "operator f{r: any relation, a: integer per s, b: integer per b, c: integer per a, d: relation {}, e: integer per d} [];\noperator main{} [];";
        assert_eq!(
            codes(source),
            [
                Code::PerRelation,
                Code::PerRelation,
                Code::PerRelation,
                Code::PerRelation
            ]
        );
        assert_eq!(
            codes("var t: tuple { a: integer per a };\noperator main{} [];"),
            [Code::PerRelation]
        );
        // Inside the operator, a `per` parameter is an attribute.
        let source = "operator f{r: any relation, of: integer per r}: integer [ return of; ];\noperator main{} [];";
        assert_eq!(codes(source), [Code::PerAsValue]);
        // Attributes are named bare.
        let source = format!(
            "{PLOTS}operator total{{r: any relation, of: integer per r}} [];\noperator main{{}} [ call total{{r: plots, of: \"of\"}}; ];"
        );
        assert_eq!(codes(&source), [Code::TypeMismatch]);
        assert!(fix(&source).contains("call total{r: plots, of: of};"));
        // The relation is a relation.
        let source = format!("{VALUES}var v := values{{r: 1, of: 2}};{MAIN}");
        assert_eq!(
            messages(&source),
            ["mismatched types: expected a relation, found `integer`"]
        );
        // A `per` argument is checked for each tuple.
        let source =
            format!("{PLOTS}{VALUES}var v := values{{r: plots, of: 1 div (of - 950)}};{MAIN}");
        assert_eq!(codes(&source), [Code::DivisionByZero]);
        // The wrapped attribute is never suggested.
        let source = format!(
            "{VALUES}operator f{{r: any relation, of: integer per r}}: relation {{ of: integer }} [ return r{{x}}; ];{MAIN}"
        );
        let errors = errors(&source);
        assert_eq!(errors[0].code, Code::UnknownAttribute);
        assert!(
            errors[0]
                .suggestions
                .iter()
                .all(|s| !s.message.contains('%')),
            "{errors:#?}"
        );
    }

    #[test]
    fn the_prelude_defines_some_and_every_in_trendy() {
        let vars = variables(&format!(
            "{PLOTS}var dry := some{{r: plots, of: of < 500}};\nvar wet := every{{r: plots, of: of > 500}};\nvar none := some{{r: plots where false, of: true}};\nvar all := every{{r: plots where false, of: false}};"
        ));
        let values: Vec<_> = vars[1..].iter().map(|v| v.init.clone()).collect();
        assert_eq!(
            values,
            [
                hir::Value::Boolean(true),
                hir::Value::Boolean(false),
                hir::Value::Boolean(false),
                hir::Value::Boolean(true)
            ]
        );
        // Their errors point at nothing in the prelude, which isn't the
        // program's source.
        let source = format!("{PLOTS}var x := some{{r: plots, of: 1}};{MAIN}");
        let errors = errors(&source);
        assert_eq!(errors.len(), 1, "{errors:#?}");
        assert_eq!(errors[0].code, Code::TypeMismatch);
        assert!(errors[0].secondary.is_empty());
        // Their names are taken, as the built-ins' are.
        let taken = self::errors("operator some{} [];\noperator main{} [];");
        assert_eq!(taken[0].code, Code::DuplicateName);
        assert!(taken[0].notes[0].contains("declared in the prelude"));
    }
}
