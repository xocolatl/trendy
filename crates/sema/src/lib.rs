mod assign;
mod block;
mod body;
mod check;
mod errors;
mod globals;
mod headings;
mod keys;
mod locals;
mod operators;
mod ops;
mod prelude;
mod types;

use std::collections::{BTreeSet, HashMap};

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
    /// Each valid update operator's local variables and statements.
    bodies: HashMap<hir::OperatorId, (Vec<hir::Local>, hir::Block)>,
}

/// Check `file`, as `source` says, with `prelude`'s operators in scope.
pub(crate) fn check(
    prelude: &Prelude,
    file: &SourceFile,
    source: Source,
    errors: &mut Vec<Diagnostic>,
) -> Checked {
    let mut namespaces = collect_names(prelude, file, errors);

    // The headings' attributes first, which any type can name.
    headings::resolve(file, &mut namespaces, errors);

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
    let mut deferred = body::Deferred::default();
    for signature in operators.iter().filter(|s| !s.read_only) {
        if let Some(decl) = &signature.decl
            && let Some(statements) =
                body::body(decl, signature.id, &context, &mut deferred, errors)
        {
            bodies.insert(signature.id, statements);
        }
    }
    body::deferred_checks(&deferred, &bodies, &operators, errors);
    Checked {
        namespaces,
        operators,
        evaluated,
        bodies,
    }
}

impl Checked {
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
                        locals: self
                            .evaluated
                            .locals
                            .remove(&signature.id)
                            .expect("every read-only operator without errors has a value"),
                        value: self.evaluated.values[&signature.id].clone(),
                    },
                    (None, None) => {
                        let (locals, body) = self
                            .bodies
                            .remove(&signature.id)
                            .expect("every update operator without errors has a body");
                        hir::OperatorKind::Update { locals, body }
                    }
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
/// operator taking no parameters: the one without parameters, if there are
/// several.
fn entry(
    file: &SourceFile,
    namespaces: &Namespaces,
    operators: &Operators,
    errors: &mut Vec<Diagnostic>,
) -> Option<hir::OperatorId> {
    let mains: Vec<&operators::Signature> = operators.overloads("main").collect();
    let main = mains
        .iter()
        .find(|s| s.written.is_empty())
        .or_else(|| mains.first());
    let Some((signature, decl)) = main.and_then(|s| Some((s, s.decl.as_ref()?))) else {
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
    Heading,
    /// A built-in type.
    Type,
}

impl Kind {
    pub(crate) fn describe(self) -> &'static str {
        match self {
            Kind::Operator | Kind::Prelude => "an operator",
            Kind::Builtin => "a built-in operator",
            Kind::Variable => "a variable",
            Kind::Heading => "a heading",
            Kind::Type => "a built-in type",
        }
    }
}

/// The program's top-level names, each in its namespace: its first
/// definition, and where it is (built-ins are in the prelude, so they have
/// no span). Operators and variables have a namespace each, so a variable
/// can have an operator's name; types and headings have a third. Operators
/// of one name are told apart by their parameters' names, which every
/// call writes, so a name has an operator for each set of them.
#[derive(Default)]
pub(crate) struct Namespaces {
    /// User and built-in operators: each name's, with their parameters'
    /// names, in the order they are defined.
    operators: HashMap<String, Vec<OperatorName>>,
    variables: HashMap<String, Span>,
    /// Where each heading is declared.
    headings: HashMap<String, Span>,
    /// Each heading's attributes, unless they have errors, which are
    /// reported: [`headings::resolve`] fills it in.
    attributes: HashMap<String, hir::Heading>,
    /// The kinds, which only the prelude declares, by name, each with the
    /// built-in types of it.
    kinds: HashMap<String, (hir::Kind, BTreeSet<hir::Scalar>)>,
}

/// An operator's definition, as its name's namespace has it.
struct OperatorName {
    kind: Kind,
    /// Where it is defined, unless the prelude defines it.
    span: Option<Span>,
    /// Its parameters' names, as a call gives their arguments.
    params: BTreeSet<String>,
}

impl Namespaces {
    /// Names with the prelude's `kinds` only, for its own declarations.
    pub(crate) fn with_kinds(kinds: &[(hir::Kind, BTreeSet<hir::Scalar>)]) -> Self {
        Namespaces {
            kinds: kinds
                .iter()
                .map(|(kind, members)| (kind.name.clone(), (kind.clone(), members.clone())))
                .collect(),
            ..Namespaces::default()
        }
    }

    /// The kind named `name`.
    pub(crate) fn kind(&self, name: &str) -> Option<&hir::Kind> {
        self.kinds.get(name).map(|(kind, _)| kind)
    }

    /// Every kind, sorted by name.
    pub(crate) fn kinds(&self) -> Vec<&hir::Kind> {
        let mut kinds: Vec<&hir::Kind> = self.kinds.values().map(|(kind, _)| kind).collect();
        kinds.sort_by(|a, b| a.name.cmp(&b.name));
        kinds
    }

    /// The types of `kind`, sorted.
    pub(crate) fn members(&self, kind: &hir::Kind) -> Vec<hir::Scalar> {
        self.kinds
            .get(&kind.name)
            .map(|(_, members)| members.iter().copied().collect())
            .unwrap_or_default()
    }

    /// Whether a type parameter of `any` can be `ty`: a type of a call's
    /// arguments, which can be a type parameter of the caller's.
    pub(crate) fn admits(&self, any: &hir::Any, ty: &hir::Type) -> bool {
        match (any, ty) {
            (hir::Any::Type, _)
            | (hir::Any::Tuple, hir::Type::Tuple(_))
            | (hir::Any::Relation, hir::Type::Relation(_))
            | (hir::Any::Array, hir::Type::Array(_)) => true,
            (hir::Any::Kind(kind), hir::Type::Scalar(scalar)) => self
                .kinds
                .get(&kind.name)
                .is_some_and(|(_, members)| members.contains(scalar)),
            (_, hir::Type::Parameter(p)) => p.any == *any,
            _ => false,
        }
    }

    /// The first operator named `name`: whether it is built in, and where
    /// it is defined.
    pub(crate) fn operator(&self, name: &str) -> Option<(Kind, Option<Span>)> {
        let first = self.operators.get(name)?.first()?;
        Some((first.kind, first.span))
    }

    /// Where the variable named `name` is declared.
    pub(crate) fn variable(&self, name: &str) -> Option<Span> {
        self.variables.get(name).copied()
    }

    /// The heading named `name`: where it is declared, and its attributes,
    /// unless they have errors.
    pub(crate) fn heading(&self, name: &str) -> Option<(Span, Option<&hir::Heading>)> {
        let span = *self.headings.get(name)?;
        Some((span, self.attributes.get(name)))
    }

    /// The headings' names.
    pub(crate) fn headings(&self) -> impl Iterator<Item = &str> {
        self.headings.keys().map(String::as_str)
    }

    /// Whether `ident` is its heading name's first declaration, not a
    /// duplicate.
    pub(crate) fn is_first_heading(&self, ident: &SyntaxToken) -> bool {
        self.headings.get(ident.text()) == Some(&trendy_syntax::span(ident.text_range()))
    }

    pub(crate) fn operators(&self) -> impl Iterator<Item = (&str, Kind, Option<Span>)> {
        self.operators
            .iter()
            .flat_map(|(name, defined)| defined.iter().map(|d| (name.as_str(), d.kind, d.span)))
    }

    /// Whether `ident` names the first definition of an operator of its
    /// name and parameters' names, not a duplicate.
    pub(crate) fn is_first_operator(&self, ident: &SyntaxToken) -> bool {
        let span = Some(trendy_syntax::span(ident.text_range()));
        self.operators
            .get(ident.text())
            .is_some_and(|defined| defined.iter().any(|d| d.span == span))
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
        kinds: Namespaces::with_kinds(prelude.kinds()).kinds,
        ..Namespaces::default()
    };
    for op in prelude.operators() {
        let kind = match op.kind {
            hir::OperatorKind::Builtin(_) => Kind::Builtin,
            _ => Kind::Prelude,
        };
        let params = operators::written(&op.params).into_keys().collect();
        namespaces
            .operators
            .entry(op.name.clone())
            .or_default()
            .push(OperatorName {
                kind,
                span: None,
                params,
            });
    }
    for item in file.items() {
        let (kind, name) = match item {
            Item::Operator(ref op) => (Kind::Operator, op.name()),
            Item::Variable(ref decl) => (Kind::Variable, decl.name()),
            Item::Heading(ref decl) => (Kind::Heading, decl.name()),
            // Reported as being outside the prelude, or the prelude's own,
            // already in it.
            Item::Builtin(_) | Item::BuiltinType(_) | Item::Kind(_) => continue,
        };
        let Some(ident) = name.and_then(|n| n.ident()) else {
            continue;
        };
        let (name, span) = (ident.text(), trendy_syntax::span(ident.text_range()));
        if let Item::Operator(op) = item {
            let params: BTreeSet<String> = op
                .params()
                .map(|heading| {
                    heading
                        .attributes()
                        .filter_map(|a| Some(a.name()?.ident()?.text().to_string()))
                        .collect()
                })
                .unwrap_or_default();
            let defined = namespaces.operators.entry(name.to_string()).or_default();
            match defined.iter().find(|d| d.params == params) {
                Some(first) => errors.push(errors::duplicate_operator(
                    name,
                    span,
                    (first.kind, first.span),
                    &params,
                )),
                None => defined.push(OperatorName {
                    kind,
                    span: Some(span),
                    params,
                }),
            }
            continue;
        }
        let first = match kind {
            Kind::Variable => namespaces.variable(name).map(|s| (kind, Some(s))),
            _ => namespaces
                .heading(name)
                .map(|(s, _)| (kind, Some(s)))
                .or_else(|| hir::Scalar::from_name(name).map(|_| (Kind::Type, None))),
        };
        match (first, kind) {
            (Some(first), _) => errors.push(errors::duplicate_name(name, (kind, span), first)),
            (None, Kind::Variable) => {
                namespaces.variables.insert(name.to_string(), span);
            }
            (None, _) => {
                namespaces.headings.insert(name.to_string(), span);
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
    use std::collections::BTreeMap;
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
            hir::OperatorKind::Builtin(hir::Builtin::Update(hir::UpdateBuiltin::DebugValue))
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
        assert_eq!(
            errors[0].message,
            "`main` is defined more than once with no parameters"
        );
        assert_eq!(errors[0].span(), Span::new(28, 32));
        assert_eq!(errors[0].secondary[0].span, Span::new(9, 13));
    }

    /// Operators of one name, which their parameters' names tell apart.
    const AREAS: &str = "operator area{width: integer, height: integer}: integer [ width * height ];\noperator area{side: integer}: integer [ side * side ];\noperator greeting{name: text, title: text}: text [ title || \" \" || name ];\noperator greeting{name: text, nickname: text}: text [ nickname ];\n";

    #[test]
    fn a_calls_arguments_names_choose_the_operator_of_its_name() {
        let vars = variables(&format!(
            "{AREAS}var a := area{{width: 3, height: 4}};\nvar b := area{{side: 3}};\nvar c := greeting{{name: \"Ada\", nickname: \"Countess\"}};"
        ));
        let values: Vec<&hir::Value> = vars.iter().map(|v| &v.init).collect();
        assert_eq!(
            values,
            [
                &hir::Value::Integer(12),
                &hir::Value::Integer(9),
                &hir::Value::Text("Countess".to_string())
            ]
        );
        // Update operators too, and a name of the prelude's, as `count` of
        // an array beside `count` of a relation.
        analyze_str(
            "operator show{v: integer} [ call debug_value{v: v}; ];\noperator show{v: integer, label: text} [ call debug_value{v: label}; ];\noperator count{a: any array}: integer [ length{a: a} ];\noperator main{} [ call show{v: 1}; call show{v: count{a: array [1]}, label: \"one\"}; call debug_value{v: count{r: relation { { x: 1 } }}}; ];",
        )
        .unwrap();
        // Not two of one name with the same parameters' names, in any
        // order, nor the prelude's.
        for (source, message) in [
            (
                "operator a{x: integer, y: integer} [];\noperator a{y: text, x: text} [];",
                "`a` is defined more than once with parameters named `x` and `y`",
            ),
            (
                "operator count{r: any relation}: integer [ 0 ];",
                "`count` is defined more than once with parameters named `r`",
            ),
        ] {
            let errors = errors(&format!("{source}{MAIN}"));
            assert_eq!(errors.len(), 1, "{source}: {errors:#?}");
            assert_eq!(errors[0].code, Code::DuplicateName, "{source}");
            assert_eq!(errors[0].message, message);
        }
        // `main` is the one without parameters.
        analyze_str("operator main{x: integer} [];\noperator main{} [];").unwrap();
    }

    #[test]
    fn arguments_naming_no_operator_are_checked_against_the_nearest() {
        let call = |call: &str| {
            format!("{AREAS}operator main{{}} [\n    call debug_value{{v: {call}}};\n];")
        };
        // The one whose parameters they name most nearly, whose missing and
        // unknown arguments are reported, and whose error names the others.
        let missing = errors(&call("area{width: 3}"));
        assert_eq!(missing[0].code, Code::MissingAttribute);
        assert!(
            missing[0].notes[0].ends_with("or those of another `area`, `area{side: integer}`"),
            "{:?}",
            missing[0].notes
        );
        assert_eq!(fix(&call("area{sid: 3}")), call("area{side: 3}"));
        // An argument without a name is for a parameter without one.
        assert_eq!(fix(&call("area{3}")), call("area{side: 3}"));
        assert_eq!(
            fix(&call("area{width: 3, 4}")),
            call("area{width: 3, height: 4}")
        );
        let two = errors(&call("area{3, 4}"));
        assert_eq!(codes(&call("area{3, 4}")), [Code::PositionalArgument; 2]);
        assert!(
            two[0].notes[0].ends_with("the parameters are `area{height: integer, width: integer}`, or those of another `area`, `area{side: integer}`"),
            "{:?}",
            two[0].notes
        );
        // Equally near ones leave the choice to the program.
        let tied = errors(&call("greeting{name: \"Ada\"}"));
        assert_eq!(tied.len(), 1, "{tied:#?}");
        assert_eq!(tied[0].code, Code::UnchosenOperator);
        assert!(tied[0].suggestions.is_empty());
        assert_eq!(
            tied[0].help,
            [
                "name the arguments as one of these has its parameters: `greeting{name: text, title: text}` or `greeting{name: text, nickname: text}`"
            ]
        );
        // So does a statement's.
        let source = "operator show{v: integer, label: text} [];\noperator show{v: integer, unit: text} [];\noperator main{} [ call show{v: 1}; ];";
        assert_eq!(codes(source), [Code::UnchosenOperator]);
    }

    #[test]
    fn a_variable_is_passed_only_to_its_reader() {
        // A read-only operator reading a variable gets it as a parameter,
        // which only the calls choosing it give it.
        let source = "var rate := 2;\noperator scaled{n: integer}: integer [ n * rate ];\noperator scaled{n: integer, by: integer}: integer [ n * by ];\noperator main{} [\n    call debug_value{v: scaled{n: 1}};\n    call debug_value{v: scaled{n: 1, by: 3}};\n];";
        assert_eq!(
            fix(source),
            "var rate := 2;\noperator scaled{n: integer, rate: integer}: integer [ n * rate ];\noperator scaled{n: integer, by: integer}: integer [ n * by ];\noperator main{} [\n    call debug_value{v: scaled{n: 1, rate: rate}};\n    call debug_value{v: scaled{n: 1, by: 3}};\n];"
        );
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
        assert_eq!(
            operators[0].message,
            "`a` is defined more than once with no parameters"
        );
    }

    /// Read-only operators for the tests below.
    const READ_ONLY: &str = "var rate := 3;\nvar poets := relation { { name: \"Ada\", born: 1815 }, { name: \"Rumi\", born: 1207 } };\noperator double{x: integer}: integer [ x * 2 ];\noperator quadruple{x: integer}: integer [ double{x: double{x: x}} ];\noperator scaled{x: integer, rate: integer}: integer [ x * rate ];\noperator born_before{year: integer, poets: relation { name: text, born: integer }}: relation { name: text, born: integer } [ poets where born < year ];";

    #[test]
    fn read_only_operators_are_evaluated_when_compiling() {
        let program = analyze_str(&format!(
            "{READ_ONLY}\nvar a := quadruple{{x: 2}};\nvar b := scaled{{x: 2, rate: rate}};\nvar c := born_before{{year: 1500, poets: poets}}{{name}};\nvar d := poets extend {{ twice := double{{x: born}} }};{MAIN}"
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
        // A variable passed to an operator has its initial value then.
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
        let hir::OperatorKind::ReadOnly { result, value, .. } = &quadruple.kind else {
            panic!("a read-only operator");
        };
        assert_eq!(result, &hir::Type::Scalar(hir::Scalar::Integer));
        assert!(matches!(value, hir::Expr::Call(..)));
    }

    #[test]
    fn read_only_operators_change_nothing_and_end_with_their_value() {
        let codes_of = |body: &str| {
            codes(&format!(
                "var n := 0;\noperator f{{x: integer}}: integer [ {body} ];{MAIN}"
            ))
        };
        assert_eq!(
            codes_of("call debug_value{v: x}; n := x; x"),
            [Code::ReadOnlyEffect, Code::ReadOnlyEffect]
        );
        assert_eq!(codes_of(""), [Code::Return]);
        assert_eq!(codes_of("var y := x;"), [Code::Return]);
        // There are no early exits yet.
        let early = errors("operator f{x: integer}: integer [ return x; 1 ];\noperator main{} [];");
        assert_eq!(early[0].code, Code::Return);
        assert_eq!(early[0].message, "a read-only operator can't return early");
        assert!(early[0].suggestions.is_empty());
        // A `return` in an update operator, of a value or not.
        assert_eq!(
            codes("operator f{} [ return 1; ];\noperator main{} [];"),
            [Code::Return]
        );
        assert_eq!(
            messages("operator f{} [ return; ];\noperator main{} [];"),
            ["`f` can't return early"]
        );
        // A value of the wrong type, reported at the value.
        let errors = errors("operator f{}: integer [ \"one\" ];\noperator main{} [];");
        assert_eq!(errors[0].code, Code::TypeMismatch);
        // `main` has no result type: the fix removes it with its value.
        let source = "operator main{}: integer [ call debug_value{v: 1}; 0 ];";
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
        // A call whose arguments are known only when the program runs, as a
        // variable of the program's is, is made then.
        for value in ["double{x: n}", "scaled{x: 1, rate: rate}"] {
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
    fn read_only_operators_read_no_variables() {
        // A read-only operator's value depends on its arguments alone: a
        // variable it reads is reported, once, and the fix passes it as a
        // parameter, which each call gives the variable, in an initial
        // value, a body or another read-only operator, whose read of it is
        // then reported in turn.
        let source = "var vat := 16;\nvar dishes := relation { { price: 1200 } };\n\
             operator taxed{price: integer}: integer [ price + price * vat div 100 + vat ];\n\
             operator dearest{}: integer [ max{r: dishes, of: taxed{price: price}} ];\n\
             var folded := taxed{price: 100};\n\
             operator main{} [ call debug_value{v: taxed{price: 100}}; ];";
        let found = errors(source);
        assert_eq!(
            found.iter().map(|e| e.message.as_str()).collect::<Vec<_>>(),
            [
                "the read-only operator `taxed` can't read the variable `vat`",
                "the read-only operator `dearest` can't read the variable `dishes`",
            ]
        );
        let once = fix(source);
        assert_eq!(
            once,
            "var vat := 16;\nvar dishes := relation { { price: 1200 } };\n\
             operator taxed{price: integer, vat: integer}: integer [ price + price * vat div 100 + vat ];\n\
             operator dearest{dishes: relation { price: integer }}: integer [ max{r: dishes, of: taxed{price: price, vat: vat}} ];\n\
             var folded := taxed{price: 100, vat: vat};\n\
             operator main{} [ call debug_value{v: taxed{price: 100, vat: vat}}; ];"
        );
        let twice = fix(&once);
        assert!(
            twice.contains("operator dearest{dishes: relation { price: integer }, vat: integer}"),
            "{twice}"
        );
        let program = analyze_str(&twice).unwrap();
        assert_eq!(program.variables[2].init, hir::Value::Integer(132));
    }

    #[test]
    fn read_only_operators_dont_call_themselves() {
        let mutual = errors(
            "operator f{}: integer [ g{} ];\noperator g{}: integer [ f{} ];\noperator main{} [];",
        );
        assert_eq!(mutual.len(), 1, "{mutual:#?}");
        assert_eq!(mutual[0].code, Code::Recursion);
        assert_eq!(mutual[0].message, "`f` and `g` call each other");
        assert_eq!(
            messages("operator f{x: integer}: integer [ f{x: x} ];\noperator main{} [];"),
            ["`f` calls itself"]
        );
        // No cycle goes through an operator's value, which reads no
        // variable of the program's.
        assert_eq!(
            codes(&format!(
                "var a := f{{}};\noperator f{{}}: integer [ a + 1 ];{MAIN}"
            )),
            [Code::VariableInReadOnly]
        );
    }

    #[test]
    fn a_name_before_empty_braces_is_the_operator() {
        // `poets{}` calls the operator, as `call poets{};` does, which, as an
        // update operator, has no value; projecting the variable needs
        // parentheses.
        let program = |value: &str| {
            format!(
                "var poets := relation {{ {{ name: \"Ada\" }} }};\noperator poets{{}} [];\noperator main{{}} [ call debug_table{{r: {value}}}; ];"
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
    fn a_parameter_without_a_type_is_a_parameter_all_the_same() {
        // Its uses aren't reported again, and the others' are checked.
        let source = "operator show{x: intger, label: text} [\n    call debug_value{v: x};\n    call debug_value{v: label + 1};\n    call debug_value{v: nothing};\n];\n\noperator double{n: integr}: integer [\n    n * 2\n];\n\noperator main{} [];";
        assert_eq!(
            messages(source),
            [
                "unknown type `intger`",
                "`+` can't be applied to text",
                "unknown value `nothing`",
                "unknown type `integr`",
            ]
        );
        // Nor are a parameter's written twice, or, with a `per` that's
        // wrong, those of its relation.
        assert_eq!(
            codes(
                "operator f{a: integer, a: text} [\n    call debug_value{v: a};\n];\n\noperator total{r: integer, of: integer per r}: integer [\n    sum{r: r, of: of}\n];\n\noperator main{} [];"
            ),
            [Code::DuplicateAttribute, Code::PerRelation]
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
    fn conversion_fixes() {
        // Without a literal to rewrite, the value converts: the one of the
        // wrong type, or the first operand, which set the type.
        let show = |values: &[&str]| {
            let calls: Vec<String> = values
                .iter()
                .map(|v| format!("    call debug_value{{v: {v}}};"))
                .collect();
            format!(
                "operator show{{n: integer, x: approximate, t: text, v: any type}} [\n{}\n];\noperator label{{n: integer}}: text [ n ];{MAIN}",
                calls.join("\n")
            )
        };
        let source = show(&[
            "\"n = \" || n",
            "n || \"!\"",
            "v || \"!\"",
            "x * n",
            "n * x",
            "1 * x",
            "n * 2 = x",
            "n + t",
            "x * t",
            "t and true",
        ]);
        let fixed = show(&[
            "\"n = \" || to_text{from: n}",
            "to_text{from: n} || \"!\"",
            "to_text{from: v} || \"!\"",
            "x * to_approximate{from: n}",
            "to_approximate{from: n} * x",
            "1e0 * x",
            "to_approximate{from: n * 2} = x",
            "n + parse_integer{from: t}",
            "x * parse_approximate{from: t}",
            "parse_boolean{from: t} and true",
        ])
        .replace("[ n ]", "[ to_text{from: n} ]");
        assert_eq!(fix(&source), fixed);
        assert!(analyze_str(&fixed).is_ok(), "{fixed}");
        // `/` of integers is likelier `div`, but of an integer by an
        // approximate number, the integer converts.
        let divided = errors(&show(&["n / x"]));
        let titles: Vec<&str> = divided[0]
            .suggestions
            .iter()
            .map(|s| s.message.as_str())
            .collect();
        assert_eq!(
            titles,
            [
                "use integer division: `div`",
                "convert `n` with `to_approximate`: `to_approximate{from: n}`"
            ]
        );
        // Booleans, tuples and relations don't convert to text: `or` was
        // meant, or an attribute's value.
        let source = format!("var t := tuple {{ a: \"x\" }};\nvar s: text := t;{MAIN}");
        assert!(
            errors(&source)[0]
                .suggestions
                .iter()
                .all(|s| !s.message.contains("to_text"))
        );
    }

    #[test]
    fn an_operand_is_expected_to_have_the_type_of_the_result() {
        // The type isn't the operand's own to change: `"n = "` wouldn't fit
        // `integer`, nor would `x`.
        let source = format!(
            "operator label{{n: integer}}: text [ \"n = \" || n ];\noperator scale{{x: approximate, n: integer}}: approximate [ x * n ];{MAIN}"
        );
        let errors = errors(&source);
        assert_eq!(errors.len(), 2, "{errors:#?}");
        for (error, (ty, operator)) in errors.iter().zip([("text", "||"), ("approximate", "*")]) {
            let labels: Vec<(&str, &str)> = error
                .secondary
                .iter()
                .map(|l| (&source[l.span.start..l.span.end], l.message.as_str()))
                .collect();
            assert_eq!(
                labels,
                [
                    (ty, "expected because of this type"),
                    (
                        operator,
                        &*format!("the result of `{operator}` has its operands' type")
                    )
                ]
            );
            assert!(
                error
                    .suggestions
                    .iter()
                    .all(|s| !s.message.starts_with("change the type")),
                "{error:#?}"
            );
        }
        let fixed = source
            .replace("|| n", "|| to_text{from: n}")
            .replace("* n", "* to_approximate{from: n}");
        assert_eq!(fix(&source), fixed);
        assert!(analyze_str(&fixed).is_ok(), "{fixed}");
    }

    #[test]
    fn conversions_are_evaluated_when_compiling() {
        let vars = variables(
            "var a := to_text{from: -0e0};\nvar b := to_text{from: tuple { city: \"Lima\\n\", n: 1 }};\nvar c := to_text{from: relation { a: integer } {}};\nvar d := to_approximate{from: 9007199254740993};\noperator empty{v: T}: text\n    types { T: any type }\n[\n    to_text{from: relation { v: T } {}}\n];\nvar e := empty{v: 1};",
        );
        let text = |i: usize| match &vars[i].init {
            hir::Value::Text(text) => text.clone(),
            other => panic!("{other:?}"),
        };
        assert_eq!(text(0), "0e0", "there is one zero");
        assert_eq!(text(1), "tuple { city: \"Lima\\n\", n: 1 }");
        assert_eq!(text(2), "relation { a: integer } {}");
        assert_eq!(
            vars[3].init,
            hir::Value::Approximate(hir::Approximate::new(9_007_199_254_740_992.0))
        );
        // Of the type the call gives `T`, not of `T`.
        assert_eq!(text(4), "relation { v: integer } {}");
    }

    #[test]
    fn numbers_are_parsed_when_compiling() {
        let vars = variables(
            "var a := parse_integer{from: \"-42\"};\nvar b := parse_approximate{from: \"2.5\"};\nvar c := parse_approximate{from: \"-0e0\"};\noperator read{t: text}: integer [\n    parse_integer{from: t} + 1\n];\nvar d := read{t: \"007\"};",
        );
        assert_eq!(vars[0].init, hir::Value::Integer(-42));
        assert_eq!(
            vars[1].init,
            hir::Value::Approximate(hir::Approximate::new(2.5))
        );
        let hir::Value::Approximate(zero) = vars[2].init else {
            panic!("{:?}", vars[2].init);
        };
        assert_eq!(zero.to_bits(), 0, "there is one zero");
        assert_eq!(vars[3].init, hir::Value::Integer(8));
    }

    #[test]
    fn booleans_are_parsed_when_compiling() {
        let vars = variables(
            "var a := parse_boolean{from: \"Yes\"};\nvar b := parse_boolean{from: \"0\"};\nvar c := parse_boolean{from: to_text{from: true}};",
        );
        let values: Vec<&hir::Value> = vars.iter().map(|v| &v.init).collect();
        assert_eq!(
            values,
            [
                &hir::Value::Boolean(true),
                &hir::Value::Boolean(false),
                &hir::Value::Boolean(true)
            ]
        );
    }

    #[test]
    fn text_that_isnt_a_number_fails_when_compiling() {
        let failure = |init: &str| {
            let errors = errors(&format!(
                "operator read{{t: text}}: integer [\n    parse_integer{{from: t}}\n];\nvar v := {init};{MAIN}"
            ));
            assert_eq!(errors.len(), 1, "{init}: {errors:#?}");
            assert_eq!(errors[0].code, Code::Unparsable, "{init}");
            (errors[0].message.clone(), errors[0].help.clone())
        };
        let (message, help) = failure("parse_integer{from: \"4.5\"}");
        assert_eq!(message, "\"4.5\" isn't an integer");
        assert!(help[0].contains("`parse_approximate`"), "{help:?}");
        // Nothing reads it as another number.
        let (message, help) = failure("parse_integer{from: \"4,5\"}");
        assert_eq!(message, "\"4,5\" isn't an integer");
        assert!(help.is_empty(), "{help:?}");
        let (message, _) = failure("parse_integer{from: \"9223372036854775808\"}");
        assert_eq!(
            message,
            "\"9223372036854775808\" is out of an integer's range"
        );
        let (message, _) = failure("parse_approximate{from: \" 2.5\"}");
        assert_eq!(message, "\" 2.5\" isn't an approximate number");
        let (message, _) = failure("parse_approximate{from: \"1e400\"}");
        assert_eq!(message, "\"1e400\" is out of an approximate number's range");
        // A boolean's words, and nothing around them.
        let errors = errors(&format!("var v := parse_boolean{{from: \"yes \"}};{MAIN}"));
        assert_eq!(errors.len(), 1, "{errors:#?}");
        assert_eq!(errors[0].message, "\"yes \" isn't a boolean");
        assert_eq!(
            errors[0].notes,
            [
                "`parse_boolean` reads `true`, `t`, `yes`, `y` or `1` as true, and `false`, `f`, `no`, `n` or `0` as false, in any case, and nothing else, not even spaces"
            ]
        );
        // Through an operator, and for a tuple.
        let (message, _) = failure("read{t: \"x\"}");
        assert_eq!(message, "\"x\" isn't an integer");
        let (message, _) = failure(
            "relation { { t: \"1\" }, { t: \"one\" } } extend { n := parse_integer{from: t} }",
        );
        assert_eq!(message, "\"one\" isn't an integer");
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
            "var a: boolean := 1;\nvar b: boolean := \"false\";\nvar c: text := true;\nvar d: bool;\nvar e: boolean := 2;\nvar f: boolean := \"Yes\";\nvar g: boolean := \"n\";\nvar h: boolean := \"on\";{MAIN}"
        );
        // Text that `parse_boolean` reads is its literal, and other text
        // is still text.
        assert_eq!(
            fix(&source),
            format!(
                "var a: boolean := true;\nvar b: boolean := false;\nvar c: text := \"true\";\nvar d: boolean;\nvar e: integer := 2;\nvar f: boolean := true;\nvar g: boolean := false;\nvar h: text := \"on\";{MAIN}"
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
        // No value the compiler could choose is the one the program means.
        assert!(errors[0].suggestions.is_empty());
        assert_eq!(
            errors[0].help,
            ["add `y` and `z` with the values they should have"]
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
        assert_eq!(errors[0].code, Code::UnknownEmptyType);
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
    fn any_tuple_gives_a_relation_selector_its_heading() {
        // The tuples have no order, so the one with an empty relation in it
        // takes its heading from another, whichever is first.
        let typed = "{ r: relation { { b: 1 } } }";
        let empty = "{ r: relation {} }";
        let first = variables(&format!("var a := relation {{ {typed}, {empty} }};"));
        let second = variables(&format!("var a := relation {{ {empty}, {typed} }};"));
        assert_eq!(first[0].init, second[0].init);
        assert_eq!(
            second[0].init.to_string(),
            "relation { r: relation { b: integer } } { { r: relation { b: integer } {} }, { r: relation { b: integer } { { b: 1 } } } }"
        );
        // However deeply it is nested.
        let deep = variables(
            "var a := relation { { r: relation { { s: relation {} } } }, { r: relation { { s: relation { { c: 1 } } } } } };",
        );
        assert_eq!(
            deep[0].ty.to_string(),
            "relation { r: relation { s: relation { c: integer } } }"
        );
        // With no tuple whose type is known on its own, each is reported.
        let source = format!("var a := relation {{ {empty}, {empty} }};{MAIN}");
        assert_eq!(
            codes(&source),
            [Code::UnknownEmptyType, Code::UnknownEmptyType]
        );
        // Another error in a tuple is reported, not taken for one that
        // needs a heading.
        let source = format!(
            "var a := relation {{ {{ r: relation {{}}, n: 1 + true }}, {{ r: relation {{ {{ b: 1 }} }}, n: 2 }} }};{MAIN}"
        );
        assert_eq!(codes(&source), [Code::TypeMismatch]);
    }

    #[test]
    fn an_operand_gives_an_empty_relation_its_heading() {
        let r = "var r := relation { { b: 1 } };\n";
        let heading = |value: &str| variables(&format!("{r}var v := {value};"))[1].ty.clone();
        // Of operators of one heading, from another operand, or else the
        // type expected, whichever order they're written in.
        for (first, second) in [
            ("relation {} union r", "r union relation {}"),
            ("union{relation {}, r}", "union{r, relation {}}"),
            ("relation {} = r", "r = relation {}"),
            ("minus[relation {}, r]", "r minus relation {}"),
        ] {
            assert_eq!(heading(first), heading(second), "{first}");
        }
        let vars =
            variables("var v: relation { b: integer } := relation {} intersect relation {};");
        assert_eq!(vars[0].init.to_string(), "relation { b: integer } {}");
        // Not of a join, whose operands' headings differ.
        assert_eq!(
            codes(&format!("{r}var v := relation {{}} join r;{MAIN}")),
            [Code::UnknownEmptyType]
        );
        // Operands of different headings are still reported together.
        let source = format!("{r}var s := relation {{ {{ c: 1 }} }};\nvar v := r union s;{MAIN}");
        assert_eq!(codes(&source), [Code::HeadingMismatch]);
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
        let errors = errors(&source);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].message, "missing attribute `b`");
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
            hir::Builtin::Update(hir::UpdateBuiltin::DebugValue).params()
        );
    }

    #[test]
    fn the_prelude_declares_type_parameters_in_any_order() {
        // In braces, as a program's are, so the built-ins' are the same.
        let declared = "types { C: any tuple, T: any tuple }";
        assert!(trendy_prelude::SOURCE.contains(declared));
        let source =
            trendy_prelude::SOURCE.replacen(declared, "types { T: any tuple, C: any tuple }", 1);
        Prelude::load(&parse(&source)).unwrap();
    }

    #[test]
    fn the_prelude_declares_built_ins_as_the_compiler_implements_them() {
        let wrong = |from: &str, to: &str| {
            assert!(trendy_prelude::SOURCE.contains(from), "{from}");
            let source = trendy_prelude::SOURCE.replacen(from, to, 1);
            let errors = Prelude::load(&parse(&source)).unwrap_err();
            assert_eq!(errors.len(), 1, "{errors:#?}");
            assert_eq!(errors[0].code, Code::Internal);
            let span = errors[0].primary.span;
            (
                errors[0].message.clone(),
                source[span.start..span.end].to_string(),
            )
        };
        // A type parameter of another kind.
        let (message, at) = wrong(
            "max{r: any relation, of: T per r}: T\n    types { T: any ordered }",
            "max{r: any relation, of: T per r}: T\n    types { T: any type }",
        );
        assert_eq!(
            message,
            "`max` is declared with parameters that differ from its implementation's, `{of: T per r, r: any relation} types { T: any ordered }`"
        );
        assert_eq!(at, "types { T: any type }");
        // A parameter that isn't `per`.
        let (_, at) = wrong(
            "min{r: any relation, of: T per r}: T",
            "min{r: any relation, of: T}: T",
        );
        assert_eq!(at, "{r: any relation, of: T}");
        // A kind requiring what the compiler doesn't implement, or a type
        // declaring a kind whose requirement it doesn't have.
        let (message, at) = wrong("kind numeric { +, * };", "kind numeric { +, *, - };");
        assert_eq!(message, "`-` is not a requirement the compiler implements");
        assert_eq!(at, "-");
        let (message, at) = wrong(
            "builtin type text: { ordered };",
            "builtin type text: { ordered, numeric };",
        );
        assert_eq!(message, "`text` doesn't have `+`, which `numeric` requires");
        assert_eq!(at, "numeric");
    }

    #[test]
    fn calls_with_parameters_and_variables() {
        let program = analyze_str(
            "var r := relation { { a: 1 } };\n\noperator greet{name: text, r: integer} [\n    call debug_value{v: name};\n    call debug_value{v: r};\n];\n\noperator main{} [\n    call debug_table{r: r};\n    call greet{r: 2, name: \"Ada\"};\n];",
        )
        .unwrap();
        let greet = program
            .operators
            .iter()
            .find(|o| o.name == "greet")
            .unwrap();
        let hir::OperatorKind::Update {
            body: hir::Block {
                statements: body, ..
            },
            ..
        } = &greet.kind
        else {
            panic!("greet is a user operator");
        };
        // Inside `greet`, `r` is the parameter, which hides the variable.
        assert_eq!(*argument(&body[1]), hir::Expr::Parameter("r".to_string()));
        let main = program.operator(program.entry);
        let hir::OperatorKind::Update {
            body: hir::Block {
                statements: body, ..
            },
            ..
        } = &main.kind
        else {
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
            "operator greet{name: text} [];\noperator main{} [\n    call greet{name: \"Ada\"};\n    call greet{};\n    call greet{name: \"1\"};\n    call greet{name: \"Ada\"};\n];"
        );
        let errors = errors(source);
        assert_eq!(errors[1].message, "missing argument `name`");
        assert_eq!(errors[1].help, ["add `name` with the value it should have"]);
        let origin = errors[2].secondary[0].span;
        assert_eq!(
            &source[origin.start..origin.end],
            "text",
            "the parameter's type"
        );
        // A lone unnamed argument is for the one parameter no other names.
        let source = "operator greet{name: text, title: text} [];\noperator main{} [ call greet{name: \"Ada\", \"Countess\"}; ];";
        assert_eq!(
            fix(source),
            "operator greet{name: text, title: text} [];\noperator main{} [ call greet{name: \"Ada\", title: \"Countess\"}; ];"
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
    }

    #[test]
    fn names() {
        let source = "var tally := 1;\noperator main{} [\n    call debug_value{v: taly};\n    call debug_valu{v: 1};\n    call debug_value{v: main};\n    call tally{};\n];";
        assert_eq!(
            codes(source),
            [
                Code::UnknownName,
                Code::UnknownName,
                Code::OperatorAsValue,
                Code::NotAnOperator
            ]
        );
        assert!(fix(source).contains("call debug_value{v: tally};\n    call debug_value{v: 1};"));
    }

    #[test]
    fn braces_order_neither_type_parameters_nor_parameters() {
        // However they are written, the operator is the same, its type
        // parameters numbered in their names' order, those its parameters
        // declare first, then each `any`, in its parameter's name's order.
        let pair = |written: &str| {
            let program = analyze_str(&format!("{written}{MAIN}")).unwrap();
            let pair = program.operators.iter().find(|o| o.name == "pair").unwrap();
            (pair.types.clone(), pair.params.clone())
        };
        let ours = pair(
            "operator pair{a: T, b: U, c: array T, d: array U, e: any type, f: any tuple} types { T: any type, U: any type } [];",
        );
        assert_eq!(
            pair(
                "operator pair{f: any tuple, e: any type, d: array U, c: array T, b: U, a: T} types { U: any type, T: any type } [];"
            ),
            ours
        );
        let names: Vec<_> = ours
            .0
            .iter()
            .map(|t| (t.index, t.name.clone(), t.any.clone()))
            .collect();
        assert_eq!(
            names,
            [
                (0, Some("T".to_string()), hir::Any::Type),
                (1, Some("U".to_string()), hir::Any::Type),
                (2, None, hir::Any::Type),
                (3, None, hir::Any::Tuple),
            ]
        );
    }

    #[test]
    fn builtins_and_any() {
        assert_eq!(
            codes("builtin operator show{v: integer};\noperator main{} [];"),
            [Code::BuiltinOutsidePrelude]
        );
        // `any type` is only a parameter's.
        let source =
            "var x: any type := 1;\noperator f{v: integer}: any type [ v ];\noperator main{} [];";
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
        // A built-in's name and parameters' names are taken, though not
        // its name with others.
        let errors = errors("operator debug_value{v: integer} [];\noperator main{} [];");
        assert_eq!(errors[0].code, Code::DuplicateName);
        assert!(errors[0].notes[0].contains("declared in the prelude"));
        analyze_str("operator debug_value{} [];\noperator main{} [];").unwrap();
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
            let hir::OperatorKind::Update {
                body: hir::Block {
                    statements: body, ..
                },
                ..
            } = &f.kind
            else {
                panic!("f is a user operator");
            };
            body.clone()
        };
        let statements = body(
            "operator f{p: integer} [\n    call debug_value{v: tuple { x: p, y: 1 }};\n    call debug_table{r: relation { { x: p }, { x: 1 } }};\n    call debug_value{v: tuple { x: 1 + 1 }};\n];\noperator main{} [];",
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
                "operator f{p: integer} [ call debug_table{r: relation { { x: 1 }, { x: p }, { x: 1 } }}; ];\noperator main{} [];"
            ),
            [Code::DuplicateTuple]
        );
        analyze_str("operator f{p: integer, q: integer} [ call debug_table{r: relation { { x: p }, { x: q } }}; ];\noperator main{} [];").unwrap();
        // The checks are the same as for constants.
        assert_eq!(
            codes(
                "operator f{p: integer} [ call debug_table{r: relation { { x: p }, { y: p } }}; ];\noperator main{} [];"
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
        let hir::OperatorKind::Update {
            body: hir::Block {
                statements: body, ..
            },
            ..
        } = &program.operator(program.entry).kind
        else {
            panic!("main is a user operator");
        };
        body.clone()
    }

    /// The only argument of a call, such as `debug_value`'s or
    /// `debug_table`'s.
    fn argument(statement: &hir::Statement) -> &hir::Expr {
        let hir::Statement::Call(call) = statement else {
            panic!("a call");
        };
        let [argument] = call.args.values().collect::<Vec<_>>()[..] else {
            panic!("one argument");
        };
        argument
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
                let hir::OperatorKind::Update {
                    body:
                        hir::Block {
                            statements: body, ..
                        },
                    ..
                } = &f.kind
                else {
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
            hir::Value::Approximate(hir::Approximate::new(0.0))
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
        assert_eq!(init("var a := -(0e0);", "a"), r(0.0));
        assert_eq!(init("var a := -2e0 * 0e0;", "a"), r(0.0));
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
            ("x + 0e0", "x"),
            ("0e0 + x", "x"),
            ("x - 0e0", "x"),
            ("0e0 - x", "-x"),
            ("x * 0e0", "0e0"),
            ("0e0 * x", "0e0"),
            ("x + -0e0", "x"),
            ("x * -1e0", "-x"),
            ("x / -1e0", "-x"),
            ("- -x", "x"),
            ("+x", "x"),
            // An operand that can fail is kept, so that its failure isn't
            // skipped; and `x` may be zero, which fails.
            ("x * 1e300 * 0e0", "x * 1e300 * 0e0"),
            ("0e0 / x", "0e0 / x"),
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
                let hir::OperatorKind::Update {
                    body:
                        hir::Block {
                            statements: body, ..
                        },
                    ..
                } = &f.kind
                else {
                    panic!("f is a user operator");
                };
                argument(&body[0]).clone()
            };
            assert_eq!(body(written), body(simplified), "{written}");
        }
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
                let hir::OperatorKind::Update {
                    body:
                        hir::Block {
                            statements: body, ..
                        },
                    ..
                } = &f.kind
                else {
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
        assert_eq!(init("var a := -0e0 = 0e0;", "a"), yes);
        assert_eq!(init("var a := -0e0 < 0e0;", "a"), no);
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
            yes,
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
            let hir::OperatorKind::Update {
                body: hir::Block {
                    statements: body, ..
                },
                ..
            } = &f.kind
            else {
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
            "operator f{r: relation { k: integer, a: text }} [\n    call debug_table{r: r group { g := {a} }};\n];\noperator main{} [];",
        )
        .unwrap();
        let f = program.operators.iter().find(|o| o.name == "f").unwrap();
        let hir::OperatorKind::Update {
            body: hir::Block {
                statements: body, ..
            },
            ..
        } = &f.kind
        else {
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
            "var factor := 3;\noperator f{r: relation { n: integer }, t: tuple { n: integer }} [\n    call debug_table{r: r extend { m := n * factor }};\n    call debug_value{v: t extend { m := n }};\n    call debug_table{r: relation { { n: 2 } } extend { m := n * 2 }};\n];\noperator main{} [];",
        )
        .unwrap();
        let f = program.operators.iter().find(|o| o.name == "f").unwrap();
        let hir::OperatorKind::Update {
            body: hir::Block {
                statements: body, ..
            },
            ..
        } = &f.kind
        else {
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
    fn a_product_is_restricted_as_it_is_made() {
        let vars = variables(
            "var r := relation { { a: 1 }, { a: 2 }, { a: 3 } };\nvar s := relation { { b: 1 }, { b: 2 } };\nvar t := relation { { c: 1, d: 7 }, { c: 2, d: 7 } };\nvar u := relation { { d: 7 }, { d: 8 } };\nvar product := times{r, s, t} where a = b and c < 2 and a + c = 2;\nvar unread := times{r, s, t} where a = b;\nvar joined := ((t join u) times r) where d = 7 and a = c;\nvar expected := relation { { a: 1, b: 1, c: 1, d: 7 } };\nvar expected_joined := relation { { a: 1, c: 1, d: 7 }, { a: 2, c: 2, d: 7 } };",
        );
        let value = |name: &str| &vars.iter().find(|v| v.name == name).unwrap().init;
        assert_eq!(value("product"), value("expected"));
        // An operand the condition reads nothing of is in the result whole.
        let hir::Value::Relation(unread) = value("unread") else {
            panic!("a relation");
        };
        assert_eq!(unread.tuples.len(), 4);
        assert_eq!(value("joined"), value("expected_joined"));
    }

    #[test]
    fn a_product_restricted_as_it_is_made_fails_as_the_whole_would() {
        // `b = 1`, which can't fail, holds for `a: 0`, so the division is
        // made.
        let codes = codes(&format!(
            "var r := relation {{ {{ a: 1 }}, {{ a: 0 }} }};\nvar s := relation {{ {{ b: 1 }} }};\nvar x := times{{r, s}} where b = 1 and 10 div a = 1;{MAIN}"
        ));
        assert_eq!(codes, [Code::DivisionByZero]);
        // `b = 2` doesn't, so it isn't; and of no tuples, nothing is
        // divided.
        let vars = variables(
            "var r := relation { { a: 1 }, { a: 0 } };\nvar s := relation { { b: 1 } };\nvar none := relation { b: integer } {};\nvar x := times{r, s} where b = 2 and 10 div a = 1;\nvar y := times{r, none} where 10 div a = 1;",
        );
        for restricted in &vars[3..] {
            let hir::Value::Relation(restricted) = &restricted.init else {
                panic!("a relation");
            };
            assert!(restricted.tuples.is_empty());
            assert_eq!(restricted.heading.attributes().count(), 2);
        }
    }

    #[test]
    fn guards_decide_which_operands_are_computed() {
        // A constant that decides the result: the other isn't computed.
        let vars = variables(
            "var a := false and 1 div 0 = 1;\nvar b := 1 div 0 = 1 and false;\nvar c := true or 1 div 0 = 1;\nvar d := false implies 1 div 0 = 1;\nvar e := 1 div 0 = 1 implies true;\nvar g := and{1 div 0 = 1, true, false};\nvar h := or{1 div 0 = 1, 1 div 0 = 2, true};\nvar points := relation { { x: 0, y: 5 }, { x: 2, y: 6 } };\nvar steep := points where y div x > 1 and x <> 0;\nvar expected := relation { { x: 2, y: 6 } };",
        );
        let value = |name: &str| &vars.iter().find(|v| v.name == name).unwrap().init;
        for (name, result) in [
            ("a", false),
            ("b", false),
            ("c", true),
            ("d", true),
            ("e", true),
            ("g", false),
            ("h", true),
        ] {
            assert_eq!(value(name), &hir::Value::Boolean(result), "{name}");
        }
        // A guard after what it guards guards it too.
        assert_eq!(value("steep"), value("expected"));
        // One that doesn't decide it leaves the division to be made, and
        // one known only when running leaves one known to fail an error.
        for source in [
            "var f := true and 1 div 0 = 1;{MAIN}".to_string(),
            "var f := xor{false, 1 div 0 = 1};{MAIN}".to_string(),
            "operator f{x: integer}: boolean [ x <> 0 and 1 div 0 = 1 ];{MAIN}".to_string(),
        ] {
            assert_eq!(
                codes(&source.replace("{MAIN}", MAIN)),
                [Code::DivisionByZero]
            );
        }
        // Guards known only when running are guards there.
        let program = analyze_str(
            "operator f{x: integer, y: integer}: boolean [ y div x > 1 and x <> 0 ];\noperator main{} [];",
        )
        .unwrap();
        let f = program.operators.iter().find(|o| o.name == "f").unwrap();
        let hir::OperatorKind::ReadOnly { value, .. } = &f.kind else {
            panic!("a read-only operator");
        };
        let hir::Expr::Connective(connective, operands) = value else {
            panic!("{value:?}");
        };
        assert!(matches!(
            connective.guarded(operands),
            Some(hir::Expr::If(..))
        ));
    }

    #[test]
    fn connectives_are_kept_in_a_normal_form() {
        // The value of a read-only operator of parameters, written `value`.
        let value = |value: &str| {
            let program = analyze_str(&format!(
                "operator f{{a: boolean, b: boolean, c: boolean, x: integer, y: integer, r: relation {{ n: integer }}, s: relation {{ n: integer }}}}: boolean [ {value} ];\noperator main{{}} [];"
            ))
            .unwrap();
            let f = program.operators.iter().find(|o| o.name == "f").unwrap();
            let hir::OperatorKind::ReadOnly { value, .. } = &f.kind else {
                panic!("a read-only operator");
            };
            value.clone()
        };
        for (written, normal) in [
            // A chain is one connective, however grouped,
            ("a and (b and c)", "(a and b) and c"),
            ("a or (b or c) or a", "a or b or c"),
            // `not` goes in as far as it can,
            ("not (a and x < y)", "not a or x >= y"),
            ("not (a or b and c)", "not a and (not b or not c)"),
            ("not not a", "a"),
            ("not (r = s)", "r <> s"),
            ("a implies b", "not a or b"),
            // an operand is written once, and a constant that doesn't
            // decide the result is dropped,
            ("a and b and a", "a and b"),
            ("a and true", "a"),
            ("a or false", "a"),
            // and of an `xor`, two of one that can't fail cancel, and a
            // true operand negates the rest. Of booleans, `<>` is an `xor`,
            // and `=` its negation, which is `=` only of two operands.
            ("a xor b xor a", "b"),
            ("true xor a xor b", "not (a xor b)"),
            ("a xor not b", "not (a xor b)"),
            ("not (a xor b) xor c", "not (a xor b xor c)"),
            ("a <> b", "a xor b"),
            ("not (a = b)", "a xor b"),
            ("not (a xor b)", "a = b"),
            ("not (a <> b)", "a = b"),
            ("a = not b", "a xor b"),
            ("a = true", "a"),
            ("a = false", "not a"),
            ("(a = b) xor c", "not (a xor b xor c)"),
            ("(a xor b) = c", "not (a xor b xor c)"),
            // A list is the chain of its operands, and of none, its
            // operator's identity.
            ("and{a, b, c}", "a and b and c"),
            ("or{a, or{b, c}, a}", "a or b or c"),
            ("xor{a, b, a}", "b"),
            ("and{a}", "a"),
            ("and{}", "true"),
            ("or{}", "false"),
            ("xor{}", "false"),
            ("not and{a, x < y}", "not a or x >= y"),
        ] {
            assert_eq!(value(written), value(normal), "{written}");
        }
        assert!(matches!(
            value("not (a xor b)"),
            hir::Expr::Binary(
                hir::BinaryOp::Compare(hir::Comparison::Equal, hir::Compared::Scalar(_)),
                ..
            )
        ));
        let hir::Expr::Connective(hir::Connective::Or, operands) = value("a or (b or c)") else {
            panic!("an `or`");
        };
        assert_eq!(operands.len(), 3);
        // Two of one that can fail are both computed.
        let hir::Expr::Connective(hir::Connective::Xor, operands) =
            value("x div y = 1 xor x div y = 1")
        else {
            panic!("an `xor`");
        };
        assert_eq!(operands.len(), 2);
        // Neither of two relations need include the other.
        assert!(matches!(
            value("not (r <= s)"),
            hir::Expr::Unary(hir::UnaryOp::Not, _)
        ));
    }

    #[test]
    fn restrict_in_bodies() {
        let program = analyze_str(
            "operator f{r: relation { n: integer }} [\n    call debug_table{r: r where n > 1};\n    call debug_table{r: r where true};\n];\noperator main{} [];",
        )
        .unwrap();
        let f = program.operators.iter().find(|o| o.name == "f").unwrap();
        let hir::OperatorKind::Update {
            body: hir::Block {
                statements: body, ..
            },
            ..
        } = &f.kind
        else {
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
            "var cities := relation { { city: \"Kumasi\", country: \"Ghana\" } };\noperator f{r: relation { country: text, continent: text }} [\n    call debug_table{r: cities join r};\n    call debug_table{r: cities compose r};\n    call debug_table{r: cities join r join cities};\n];\noperator main{} [];",
        )
        .unwrap();
        let f = program.operators.iter().find(|o| o.name == "f").unwrap();
        let hir::OperatorKind::Update {
            body: hir::Block {
                statements: body, ..
            },
            ..
        } = &f.kind
        else {
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
            "operator f{r: relation { z: integer }} [\n    call debug_table{r: join{relation { { x: 1, y: 2 } }, r, relation { { y: 2, z: 3 } }}};\n    call debug_table{r: join{r}};\n];\noperator main{} [];",
        )
        .unwrap();
        let f = program.operators.iter().find(|o| o.name == "f").unwrap();
        let hir::OperatorKind::Update {
            body: hir::Block {
                statements: body, ..
            },
            ..
        } = &f.kind
        else {
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
            "var cities := relation { { city: \"Kumasi\", country: \"Ghana\" } };\noperator f{r: relation { size: integer }} [\n    call debug_table{r: cities join r};\n];\noperator main{} [];",
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
                let hir::OperatorKind::Update {
                    body:
                        hir::Block {
                            statements: body, ..
                        },
                    ..
                } = &f.kind
                else {
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
                let hir::OperatorKind::Update {
                    body:
                        hir::Block {
                            statements: body, ..
                        },
                    ..
                } = &f.kind
                else {
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
                .map(|a| (a.target.clone(), a.value.clone()))
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
        let target = || hir::Target::Variable(hir::VariableId(0));
        assert_eq!(
            assigned(r#"insert poets relation { { name: "Rumi", born: 1 } };"#),
            [(
                target(),
                hir::Expr::Combine(hir::Combination::Union, poets(), relation("Rumi"))
            )]
        );
        // Merged in a block: what is removed first, then what is added.
        assert_eq!(
            assigned(
                r#"assign { d_insert poets relation { { name: "Rumi", born: 1 } }; i_delete poets relation { { name: "Ada", born: 1 } }; };"#
            ),
            [(
                target(),
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
        // The tuples for which the condition is false are kept: `born <=
        // 1900`, the negation of `born > 1900`.
        let [(_, hir::Expr::Restrict(operand, condition))] =
            &assigned("delete poets where born > 1900;")[..]
        else {
            panic!("a restriction");
        };
        assert_eq!(*operand, poets());
        assert!(matches!(
            **condition,
            hir::Expr::Binary(hir::BinaryOp::Compare(hir::Comparison::LessOrEqual, _), ..)
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
        assert_eq!(*target, hir::Target::Variable(hir::VariableId(1)));
        assert_eq!(**operand, hir::Expr::Variable(hir::VariableId(1)));
    }

    #[test]
    fn an_element_is_assigned_as_its_array() {
        let source = |body: &str| {
            format!(
                "var a := array [1, 2, 3];\nvar g := array [array [1], array [2]];\noperator main{{}} [ var i := 1; {body} ];"
            )
        };
        // `a[i] := x` assigns `a` the array with the element changed.
        let body = main_body(&source("a[i] := 5;"));
        let [_, hir::Statement::Assign(assignments)] = &body[..] else {
            panic!("{body:?}");
        };
        let [
            hir::Assignment {
                target: hir::Target::Variable(id),
                value: hir::Expr::WithElements(array, changes),
            },
        ] = &assignments[..]
        else {
            panic!("{assignments:?}");
        };
        assert_eq!(**array, hir::Expr::Variable(*id));
        let [change] = &changes[..] else {
            panic!("{changes:?}");
        };
        assert_eq!(change.value, hir::Expr::Constant(hir::Value::Integer(5)));
        // Changes to an array's elements, at any depth, are one.
        let body = main_body(&source("assign { g[i][1] := 3; g[2] := array [4]; };"));
        let [_, hir::Statement::Assign(assignments)] = &body[..] else {
            panic!("{body:?}");
        };
        let [
            hir::Assignment {
                value: hir::Expr::WithElements(_, changes),
                ..
            },
        ] = &assignments[..]
        else {
            panic!("{assignments:?}");
        };
        let depths: Vec<usize> = changes.iter().map(|c| c.path.len()).collect();
        assert_eq!(depths, [2, 1]);
        // Known when compiling, the new value is too.
        assert_eq!(
            variables("var b := [ var c := array [1, 2]; c[2] := 20; c ];")[0]
                .init
                .to_string(),
            "array [1, 20]"
        );
        // Changes known to overlap now are reported now, and others stop
        // the program if they do.
        let codes = |body: &str| codes(&source(body));
        for overlapping in [
            "assign { a[1] := 1; a[1] := 2; };",
            "assign { a[i] := 1; a[i] := 2; };",
            "assign { g[1] := array [0]; g[1][1] := 2; };",
            "assign { a := array [0]; a[1] := 2; };",
        ] {
            assert_eq!(
                codes(overlapping),
                [Code::ConflictingChanges],
                "{overlapping}"
            );
        }
        let apart = source("assign { a[i] := 1; a[2] := 2; g[1][1] := 3; g[2][1] := 4; };");
        assert!(analyze_str(&apart).is_ok(), "{:#?}", errors(&apart));
        // As are those of a call evaluated when compiling.
        let evaluated = "operator f{i: integer, j: integer}: array integer [ var c := array [1, 2]; assign { c[i] := 5; c[j] := 6; }; c ];\nvar x := f{i: 1, j: 1};";
        assert_eq!(
            errors(&format!("{evaluated}{MAIN}"))
                .iter()
                .map(|e| e.code)
                .collect::<Vec<_>>(),
            [Code::ConflictingChanges]
        );
        // A position below 1, a value that is the element's own, of another
        // type, or an element of what isn't an array.
        assert_eq!(codes("a[0] := 1;"), [Code::IndexOutOfRange]);
        assert_eq!(codes("a[i] := a[i];"), [Code::NoChange]);
        assert_eq!(codes("a[1] := \"one\";"), [Code::TypeMismatch]);
        assert_eq!(codes("i[1] := 1;"), [Code::WrongOperandType]);
        assert_eq!(
            fix(&source("a[0] := 1;")),
            source("a[1] := 1;"),
            "positions count from 1"
        );
    }

    #[test]
    fn attributes_are_changed_by_update() {
        let source = |body: &str| {
            format!(
                "var t := tuple {{ name: \"Ada\", born: 1815 }};\nvar people := array [{{ name: \"Ada\", born: 1815 }}];\noperator main{{}} [ var name := \"Kenji\"; {body} ];"
            )
        };
        assert_eq!(
            fix(&source("t.name := \"Ada Lovelace\";")),
            source("update t { name := \"Ada Lovelace\" };")
        );
        assert_eq!(
            fix(&source("people[1].born := 1816;")),
            source("people[1] := people[1] replace { born := 1816 };")
        );
        // Inside the braces, `name` would be the attribute.
        let [error] = &errors(&source("t.name := name;"))[..] else {
            panic!("one error");
        };
        assert_eq!(error.code, Code::AttributeAssigned);
        assert!(error.suggestions.is_empty());
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
        assert_eq!(
            errors[0].message,
            "the `const` parameter `p` can't be assigned"
        );
        // The parameter `n` hides the variable.
        assert!(
            errors[1].help[0].contains("rename the parameter"),
            "{:?}",
            errors[1].help
        );
        assert_eq!(errors[2].message, "`main` is an operator, not a variable");
    }

    #[test]
    fn var_parameters_update_their_arguments_variables() {
        let program = analyze_str(
            "var count := 1;\n\
             operator double{var n: integer, const by: integer} [ n := n * by; ];\n\
             operator twice{var n: integer} [ call double{var n: n, by: 2}; call double{var n: n, by: 2}; ];\n\
             operator main{} [ var local := 2; call double{var n: count, const by: 3}; call twice{var n: local}; ];",
        )
        .unwrap();
        let double = program
            .operators
            .iter()
            .find(|o| o.name == "double")
            .unwrap();
        assert_eq!(double.params["n"].mode, hir::Mode::Var);
        assert_eq!(double.params["by"].mode, hir::Mode::Const);
        // The body assigns the parameter itself.
        let hir::OperatorKind::Update { body, .. } = &double.kind else {
            panic!("an update operator");
        };
        let [hir::Statement::Assign(assignments)] = &body.statements[..] else {
            panic!("{body:?}");
        };
        assert_eq!(
            assignments[0].target,
            hir::Target::Parameter("n".to_string())
        );
        // A call gives each `var` parameter a variable, which it updates:
        // the program's, a local one, or a `var` parameter passed on.
        let hir::OperatorKind::Update { body, .. } = &program.operator(program.entry).kind else {
            panic!("main is an update operator");
        };
        let updated: Vec<(BTreeMap<String, hir::Target>, Vec<String>)> = body
            .statements
            .iter()
            .filter_map(|s| match s {
                hir::Statement::Call(call) => {
                    Some((call.updated.clone(), call.args.keys().cloned().collect()))
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            updated,
            [
                (
                    [("n".to_string(), hir::Target::Variable(hir::VariableId(0)))].into(),
                    vec!["by".to_string()]
                ),
                (
                    [("n".to_string(), hir::Target::Local(hir::LocalId(0)))].into(),
                    Vec::new()
                ),
            ]
        );
        let twice = program
            .operators
            .iter()
            .find(|o| o.name == "twice")
            .unwrap();
        let hir::OperatorKind::Update { body, .. } = &twice.kind else {
            panic!("an update operator");
        };
        let hir::Statement::Call(call) = &body.statements[0] else {
            panic!("{body:?}");
        };
        assert_eq!(call.updated["n"], hir::Target::Parameter("n".to_string()));
    }

    #[test]
    fn modes_only_where_a_parameter_can_have_one() {
        for (source, message, fixed) in [
            (
                "operator doubled{var n: integer}: integer [ n * 2 ];",
                "a read-only operator's parameter `n` can't be `var`",
                "operator doubled{n: integer}: integer [ n * 2 ];",
            ),
            (
                "operator f{var r: any relation, of: integer per r} [];",
                "`r`, which `of` is evaluated for each tuple of, can't be `var`",
                "operator f{r: any relation, of: integer per r} [];",
            ),
            (
                "operator f{r: any relation, var of: integer per r} [];",
                "the `per` parameter `of` can't be `var`",
                "operator f{r: any relation, of: integer per r} [];",
            ),
            (
                "operator halved{out n: integer}: integer [ 1 ];",
                "a read-only operator's parameter `n` can't be `out`",
                "operator halved{n: integer}: integer [ 1 ];",
            ),
            (
                "operator f{r: any relation, out of: integer per r} [];",
                "the `per` parameter `of` can't be `out`",
                "operator f{r: any relation, of: integer per r} [];",
            ),
            (
                "var t: tuple { var a: integer };",
                "only an operator's parameters have a mode",
                "var t: tuple { a: integer };",
            ),
            (
                "operator f{n: T} types { var T: any type } [];",
                "only an operator's parameters have a mode",
                "operator f{n: T} types { T: any type } [];",
            ),
        ] {
            let source = format!("{source}{MAIN}");
            assert_eq!(messages(&source), [message], "{source}");
            assert_eq!(fix(&source), format!("{fixed}{MAIN}"), "{source}");
        }
        // `const` is a parameter's mode anyway, and can be written.
        assert!(
            analyze_str(&format!(
                "operator half{{const n: integer}}: integer [ n div 2 ];{MAIN}"
            ))
            .is_ok()
        );
    }

    #[test]
    fn arguments_are_marked_as_their_parameters_are() {
        const OPERATORS: &str = "var count := 1;\n\
             operator double{var n: integer} [ n := n * 2; ];\n\
             operator show{n: integer} [ call debug_value{v: n}; ];\n\
             operator set{out n: integer} [ n := 1; ];\n";
        for (call, message, fixed) in [
            (
                "call set{n: count};",
                "the argument for `out n` is marked `out`, as the variable the call assigns",
                "call set{out n: count};",
            ),
            (
                "call set{var n: count};",
                "the argument for `out n` is marked `out`, not `var`",
                "call set{out n: count};",
            ),
            (
                "call double{out n: count};",
                "the argument for `var n` is marked `var`, not `out`",
                "call double{var n: count};",
            ),
            (
                "call show{out n: count};",
                "`n` isn't an `out` parameter, so its argument isn't marked `out`",
                "call show{n: count};",
            ),
            (
                "call double{n: count};",
                "the argument for `var n` is marked `var`, as the variable the call updates",
                "call double{var n: count};",
            ),
            (
                "call double{const n: count};",
                "the argument for `var n` is marked `var`, not `const`",
                "call double{var n: count};",
            ),
            (
                "call show{var n: count};",
                "`n` isn't a `var` parameter, so its argument isn't marked `var`",
                "call show{n: count};",
            ),
        ] {
            let source = format!("{OPERATORS}operator main{{}} [ {call} ];");
            assert_eq!(messages(&source), [message], "{source}");
            assert_eq!(
                fix(&source),
                format!("{OPERATORS}operator main{{}} [ {fixed} ];")
            );
        }
        // So are a read-only operator's, whose parameters are `const`.
        assert!(analyze_str(&format!("{OPERATORS}operator half{{n: integer}}: integer [ n div 2 ];\noperator main{{}} [ call show{{const n: half{{const n: 4}}}}; ];")).is_ok());
        assert_eq!(
            messages(&format!(
                "{OPERATORS}operator half{{n: integer}}: integer [ n div 2 ];\nvar h := half{{var n: count}};{MAIN}"
            )),
            ["`n` isn't a `var` parameter, so its argument isn't marked `var`"]
        );
    }

    #[test]
    fn var_arguments_are_variables_the_body_can_assign() {
        const OPERATORS: &str = "var count := 1;\n\
             var numbers := array [1, 2];\n\
             operator double{var n: integer} [ n := n * 2; ];\n";
        for (body, code, message) in [
            (
                "call double{var n: count + 1};",
                Code::NotAssignable,
                "the argument for `var n` is a variable, which the call updates",
            ),
            (
                "call double{var n: 2};",
                Code::NotAssignable,
                "the argument for `var n` is a variable, which the call updates",
            ),
            (
                "call double{var n: numbers[1]};",
                Code::NotAssignable,
                "the argument for `var n` is a variable, which the call updates",
            ),
            (
                "for x in numbers loop [ call double{var n: x}; ];",
                Code::NotAssignable,
                "`x`, a loop's variable, can't be assigned",
            ),
            (
                "call double{var n: \"one\"};",
                Code::TypeMismatch,
                "mismatched types: expected `integer`, found `text`",
            ),
        ] {
            let source = format!("{OPERATORS}operator main{{}} [ {body} ];");
            let errors = errors(&source);
            assert_eq!(
                errors
                    .iter()
                    .map(|e| (e.code, e.message.as_str()))
                    .collect::<Vec<_>>(),
                [(code, message)],
                "{source}"
            );
        }
        // A `const` parameter is passed on as a value, not as a variable.
        assert_eq!(
            messages(&format!(
                "{OPERATORS}operator f{{p: integer}} [ call double{{var n: p}}; ];{MAIN}"
            )),
            ["the `const` parameter `p` can't be assigned"]
        );
        // Nor is a variable that a `for` loop's array is computed from.
        assert_eq!(
            messages(&format!(
                "{OPERATORS}operator g{{}} [ for x in numbers loop [ call double{{var n: count}}; call grow{{var a: numbers}}; ]; ];\noperator grow{{var a: array integer}} [];{MAIN}"
            )),
            ["`numbers` can't be assigned in a loop that goes through it"]
        );
    }

    #[test]
    fn a_var_argument_has_no_other_name_in_the_call() {
        const OPERATORS: &str = "var count := 1;\n\
             var other := 2;\n\
             operator add{var a: integer, var b: integer} [ a := a + b; ];\n\
             operator both{var a: integer, out b: integer} [ b := a; ];\n\
             operator reads{var n: integer} [ n := count + 1; ];\n\
             operator assigns{var n: integer} [ count := 3; n := 1; ];\n\
             operator through{var n: integer} [ call reads{var n: n}; ];\n\
             operator plus{var a: integer, by: integer} [ a := a + by; ];\n";
        for (call, message) in [
            (
                "call add{var a: count, var b: count};",
                "`count` is passed to both `var a` and `var b`",
            ),
            (
                "call both{var a: count, out b: count};",
                "`count` is passed to both `var a` and `out b`",
            ),
            (
                "call reads{var n: count};",
                "`count` is passed to `var n` of `reads`, which reads `count` itself",
            ),
            (
                "call assigns{var n: count};",
                "`count` is passed to `var n` of `assigns`, which assigns `count` itself",
            ),
            (
                "call through{var n: count};",
                "`count` is passed to `var n` of `through`, which reads `count` itself",
            ),
        ] {
            let source = format!("{OPERATORS}operator main{{}} [ {call} ];");
            let errors = errors(&source);
            assert_eq!(
                errors
                    .iter()
                    .map(|e| (e.code, e.message.as_str()))
                    .collect::<Vec<_>>(),
                [(Code::Aliased, message)],
                "{source}"
            );
        }
        // Other variables are fine, and so is reading the variable in
        // another argument, whose value is computed before the call.
        let fine = format!(
            "{OPERATORS}operator main{{}} [ call add{{var a: count, var b: other}}; call reads{{var n: other}}; call plus{{var a: count, by: count}}; ];"
        );
        assert!(analyze_str(&fine).is_ok(), "{:?}", messages(&fine));
    }

    #[test]
    fn out_parameters_are_assigned_before_they_are_read() {
        let body = |body: &str| {
            format!(
                "var g := 0;\noperator f{{x: integer, out n: integer}} [ {body} ];\noperator bump{{var n: integer}} [ n := n + 1; ];\noperator set{{out n: integer}} [ n := 1; ];{MAIN}"
            )
        };
        // Assigned first, then read; on every path, as `if`'s branches
        // both assign it, or a `loop … while`'s block, which runs once.
        for ok in [
            "n := 42; var y := n; n := y + 1;",
            "if x > 0 then [ n := 1; ] else [ n := 2; ]; call debug_value{v: n};",
            "if x > 0 then [ n := 1; ] else if x < 0 then [ n := 2; ] else [ n := 3; ];",
            "loop [ n := 1; ] while n < 0;",
            "call set{out n: n}; call bump{var n: n};",
            "n := 1; n := n + 1;",
        ] {
            assert!(
                analyze_str(&body(ok)).is_ok(),
                "{ok}: {:?}",
                messages(&body(ok))
            );
        }
        // Read before it is assigned, as the value of a variable, by a
        // change that reads it, by a `var` argument, in a condition, or in
        // an `assign` block, whose values are computed before any is
        // assigned.
        for (read, at) in [
            ("var y := n; n := y;", "n;"),
            ("n := n + 1;", "n + 1"),
            ("call bump{var n: n};", "n}"),
            ("if n > 0 then [ n := 1; ] else [ n := 2; ];", "n > 0"),
            (
                "while x > 0 loop [ n := 1; ]; call debug_value{v: n};",
                "n}",
            ),
            ("assign { n := 1; g := n; };", "n; }"),
        ] {
            let source = body(read);
            let errors = errors(&source);
            assert_eq!(
                errors
                    .iter()
                    .map(|e| (e.code, e.message.as_str()))
                    .collect::<Vec<_>>(),
                [(
                    Code::UnassignedOut,
                    "the `out` parameter `n` is read before it is assigned"
                )],
                "{read}"
            );
            let span = errors[0].span();
            assert_eq!(
                span.start,
                source.find(read).unwrap() + read.find(at).unwrap(),
                "{read}"
            );
            // The fix makes it `var`, whose argument's value can be read.
            assert!(
                fix(&source).contains("operator f{x: integer, var n: integer}"),
                "{read}"
            );
        }
        // Not assigned on every path.
        for path in [
            "if x > 0 then [ n := 1; ];",
            "while x > 0 loop [ n := 1; ];",
            "for i in array [1] loop [ n := i; ];",
            "call debug_value{v: x};",
        ] {
            assert_eq!(
                messages(&body(path)),
                ["the `out` parameter `n` isn't assigned on every path through `f`"],
                "{path}"
            );
        }
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

    const SAME: &str = "operator same{a: T, b: T}: boolean types { T: any type } [ a = b ];\n";

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
        let only =
            "operator only{r: relation { v: T }}: T types { T: any type } [ value from r ];\n";
        let vars = variables(&format!(
            "{only}var a := only{{r: relation {{ {{ v: 5 }} }}}};\nvar b := only{{r: relation {{ {{ v: \"Kyiv\" }} }}}};"
        ));
        assert_eq!(vars[0].init, hir::Value::Integer(5));
        assert_eq!(vars[1].init, hir::Value::Text("Kyiv".to_string()));
        let pick = "operator pick{a: T, r: relation { v: T }}: boolean types { T: any type } [ r = relation {} ];\n";
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
        let source =
            "operator f{a: T, b: T}: boolean types { T: any type } [ a < b ];\noperator main{} [];";
        assert_eq!(codes(source), [Code::WrongOperandType]);
        assert_eq!(messages(source), ["`<` can't be applied to values of `T`"]);
        let source = "operator f{a: T}: T types { T: any type } [ a + a ];\noperator main{} [];";
        assert_eq!(codes(source), [Code::WrongOperandType]);
        // Two type parameters are two types.
        let source = "operator f{a: T, b: U, c: T, d: U}: boolean types { T: any type, U: any type } [ a = b ];\noperator main{} [];";
        assert_eq!(codes(source), [Code::TypeMismatch]);
        // So are two written `any type`, which one declared can make one.
        let source =
            "operator f{a: any type, b: any type}: boolean [ a = b ];\noperator main{} [];";
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
            "operator sub{r: R, s: R}: boolean types { R: any relation } [ r <= s ];\nvar yes := sub{r: relation { { a: 1 } }, s: relation { { a: 1 }, { a: 2 } }};\noperator main{} [];",
        )
        .unwrap();
        // A type parameter can be written in the body.
        let program = analyze_str(
            "operator none{a: T}: relation { v: T } types { T: any type } [ relation { v: T } {} ];\nvar e := none{a: \"Accra\"};\noperator main{} [];",
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
        let source = "operator f{a: integer}: relation { v: T } types { T: any type } [ relation {} ];\noperator main{} [];";
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
        // Twice, or in the result or the body, it does, and so does one
        // inside a parameter's type, where `any …` can't be written.
        analyze_str("operator f{a: T}: T types { T: any type } [ a ];\noperator main{} [];")
            .unwrap();
        analyze_str("operator f{a: array T} types { T: any tuple } [];\noperator main{} [];")
            .unwrap();
        analyze_str(
            "operator f{a: T} types { T: any type } [ call debug_table{r: relation { a: T } {}}; ];\noperator main{} [];",
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

    const VALUES: &str = "operator values{r: any relation, of: T per r}: relation { v: T } types { T: any type } [ r{of} rename {of as v} ];\n";

    #[test]
    fn per_arguments_are_evaluated_for_each_tuple() {
        let vars = variables(&format!(
            "{PLOTS}{VALUES}var regions := values{{r: plots, of: region}};\nvar wet := values{{r: plots, of: of > 1000}};"
        ));
        // The operator's `of` is its own, not the plots' attribute of that
        // name, which is in scope in the argument.
        assert_eq!(
            vars[1].init.to_string(),
            r#"relation { v: text } { { v: "Mendoza" }, { v: "Nyeri" } }"#
        );
        assert_eq!(
            vars[2].init.to_string(),
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
        assert_eq!(values.params["r"].ty.to_string(), "relation { of: T, … }");
        // A relation passed on keeps its tuples, and its `per` attribute.
        let again = "operator again{r: any relation, of: T per r}: relation { v: T } types { T: any type } [ values{r: r, of: of} ];\n";
        let vars = variables(&format!(
            "{PLOTS}{VALUES}{again}var regions := again{{r: plots, of: region}};"
        ));
        assert_eq!(
            vars[1].init.to_string(),
            r#"relation { v: text } { { v: "Mendoza" }, { v: "Nyeri" } }"#
        );
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
        let source =
            "operator f{r: any relation, of: integer per r}: integer [ of ];\noperator main{} [];";
        assert_eq!(codes(source), [Code::PerAsValue]);
        // Attributes are named bare, whatever the type: in quotes, one is a
        // text, the same for every tuple.
        for ty in ["integer", "text"] {
            let source = format!(
                "{PLOTS}operator total{{r: any relation, of: {ty} per r}} [];\noperator main{{}} [ call total{{r: plots, of: \"of\"}}; ];"
            );
            assert_eq!(codes(&source), [Code::ConstantPer], "{ty}");
            assert!(fix(&source).contains("call total{r: plots, of: of};"));
        }
        // An argument must use the tuple: one that doesn't, because it is a
        // constant, a parameter or an outer tuple's attribute, or folds to a
        // constant, is the same for every tuple.
        for of in ["true", "n", "region = \"Nyeri\"", "false and of > 0"] {
            let source = format!(
                "{PLOTS}operator total{{r: any relation, of: boolean per r}}: boolean [ every{{r: r, of: of}} ];\nvar regions := relation {{ {{ region: \"Nyeri\", n: true }} }};\nvar x := regions extend {{ t := total{{r: !!plots, of: {of}}} }};{MAIN}"
            );
            let errors = errors(&source);
            assert_eq!(errors[0].code, Code::ConstantPer, "{of}: {errors:#?}");
            assert_eq!(
                errors[0].help,
                ["compute it from the attributes of `r`'s tuples: `of` and `plot`"],
                "{of}"
            );
        }
        // An image relation uses the tuple, though it names no attribute.
        analyze_str(&format!(
            "{PLOTS}var regions := relation {{ {{ region: \"Nyeri\" }} }};\nvar x := some{{r: regions, of: count{{r: !!plots}} > 1}};{MAIN}"
        ))
        .unwrap();
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
            "{VALUES}operator f{{r: any relation, of: integer per r}}: relation {{ of: integer }} [ r{{x}} ];{MAIN}"
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
            "{PLOTS}var dry := some{{r: plots, of: of < 500}};\nvar wet := every{{r: plots, of: of > 500}};\nvar none := some{{r: plots where false, of: of > 0}};\nvar all := every{{r: plots where false, of: of > 0}};"
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
        // Their names with their parameters' names are taken, as the
        // built-ins' are.
        let taken = self::errors(
            "operator some{r: any relation, of: boolean per r}: boolean [ false ];\noperator main{} [];",
        );
        assert_eq!(taken[0].code, Code::DuplicateName);
        assert!(taken[0].notes[0].contains("declared in the prelude"));
    }

    #[test]
    fn count_is_a_read_only_built_in() {
        let vars = variables(&format!(
            "{PLOTS}var n := count{{r: plots}};\nvar none := count{{r: plots where false}};"
        ));
        assert_eq!(vars[1].init, hir::Value::Integer(3));
        assert_eq!(vars[2].init, hir::Value::Integer(0));
        // Made when running where the relation is known only then.
        let body = main_body(&format!(
            "{PLOTS}operator main{{}} [ call debug_value{{v: count{{r: plots}}}}; ];"
        ));
        let hir::Expr::Call(call) = argument(&body[0]) else {
            panic!("a call");
        };
        assert_eq!(call.args.len(), 1);
        // `call` would lose its value.
        assert_eq!(
            codes(&format!(
                "{PLOTS}operator main{{}} [ call count{{r: plots}}; ];"
            )),
            [Code::CallOfReadOnly]
        );
        // Other languages' spellings are fixed: a positional argument, and
        // a method call. `count(plots)`'s parentheses are the parser's.
        for written in ["count{plots}", "plots.count{}"] {
            let source = format!("{PLOTS}var n := {written};{MAIN}");
            assert!(
                fix(&source).contains("var n := count{r: plots};"),
                "{written}: {}",
                fix(&source)
            );
        }
        // An attribute named as an operator is still an attribute.
        analyze_str("var t := tuple { count: 1 };\nvar c := t.count;\noperator main{} [];")
            .unwrap();
    }

    #[test]
    fn a_kind_says_what_its_type_parameters_can_be() {
        let below = "operator below{r: any relation, of: T per r, limit: T}: relation { v: T } types { T: any ordered } [ (r where of < limit){of} rename {of as v} ];\n";
        let vars = variables(&format!(
            "{PLOTS}{below}var dry := below{{r: plots, of: of, limit: 1000}};\nvar early := below{{r: plots, of: region, limit: \"N\"}};"
        ));
        assert_eq!(
            vars[1].init.to_string(),
            "relation { v: integer } { { v: 220 }, { v: 950 } }"
        );
        assert_eq!(
            vars[2].init.to_string(),
            r#"relation { v: text } { { v: "Mendoza" } }"#
        );
        // Only types of the kind are arguments.
        let source = format!(
            "{below}var x := below{{r: relation {{ {{ a: tuple {{}} }} }}, of: a, limit: tuple {{}}}};{MAIN}"
        );
        let mismatched = errors(&source);
        assert_eq!(mismatched[0].code, Code::TypeMismatch);
        assert_eq!(
            mismatched[0].message,
            "mismatched types: expected a type of kind `ordered`, found `tuple {}`"
        );
        // `<` is for a kind that requires it, and the error says which.
        let source = "operator less{a: T, b: T}: boolean types { T: any type } [ a < b ];\noperator main{} [];";
        let unordered = errors(source);
        assert_eq!(unordered[0].code, Code::WrongOperandType);
        assert!(
            unordered[0].help[0].contains("declare `T` as `any ordered`"),
            "{unordered:#?}"
        );
        // A kind named wrong, or declared in a program.
        let source = "operator f{a: any ordred} [];\noperator main{} [];";
        assert_eq!(codes(source), [Code::UnknownType]);
        assert_eq!(
            fix(source),
            "operator f{a: any ordered} [];\noperator main{} [];"
        );
        assert_eq!(
            codes("kind mine { < };\noperator main{} [];"),
            [Code::BuiltinOutsidePrelude]
        );
    }

    #[test]
    fn min_and_max_are_by_the_order_of_any_ordered_type() {
        let vars = variables(&format!(
            "{PLOTS}var driest := min{{r: plots, of: of}};\nvar last := max{{r: plots, of: region}};\nvar wettest := max{{r: plots, of: of * -1}};"
        ));
        assert_eq!(vars[1].init, hir::Value::Integer(220));
        assert_eq!(vars[2].init, hir::Value::Text("Nyeri".to_string()));
        assert_eq!(vars[3].init, hir::Value::Integer(-220));
        // Of no tuples, neither has a value: an error when compiling, if the
        // relation is known then.
        let source = format!("{PLOTS}var none := max{{r: plots where false, of: of}};{MAIN}");
        let empty = errors(&source);
        assert_eq!(empty[0].code, Code::NoMinOrMax);
        assert_eq!(
            empty[0].message,
            "the relation has no tuples, so it has no max"
        );
        assert!(
            empty[0].notes.iter().all(|n| !n.contains("computing")),
            "{empty:#?}"
        );
        // For each tuple at hand, as of a group's image relation.
        let source = format!(
            "{PLOTS}var regions := relation {{ {{ region: \"Nyeri\" }}, {{ region: \"Puno\" }} }};\nvar lows := regions extend {{ low := min{{r: !!plots, of: of}} }};{MAIN}"
        );
        let empty = errors(&source);
        assert_eq!(empty[0].code, Code::NoMinOrMax);
        assert!(empty[0].notes[1].contains("\"Puno\""), "{empty:#?}");
        // Only for types with an order, which a generic operator's type
        // parameter is told how to have.
        assert_eq!(
            codes(&format!(
                "{PLOTS}var t := max{{r: plots, of: tuple {{}}}};{MAIN}"
            )),
            [Code::TypeMismatch]
        );
        let source = "operator top{r: any relation, of: T per r}: T types { T: any type } [ max{r: r, of: of} ];\noperator main{} [];";
        let unordered = errors(source);
        assert_eq!(unordered[0].code, Code::TypeMismatch);
        assert_eq!(
            unordered[0].help,
            ["declare `T` as `any ordered`, whose types all declare it"]
        );
        let source = "operator top{r: any relation, of: T per r}: T types { T: any ordered } [ max{r: r, of: of} ];\n";
        let vars = variables(&format!(
            "{PLOTS}{source}var top := top{{r: plots, of: plot}};"
        ));
        assert_eq!(vars[1].init, hir::Value::Integer(3));
    }

    #[test]
    fn sum_is_exact_then_rounded_once() {
        let vars = variables(&format!(
            "{PLOTS}var rain := sum{{r: plots, of: of}};\nvar none := sum{{r: plots where false, of: of}};\nvar halves := sum{{r: plots, of: to_approximate{{from: of}} * 5e-1}};\nvar nothing := sum{{r: plots where false, of: 1e-1 * to_approximate{{from: plot}}}};"
        ));
        assert_eq!(vars[1].init, hir::Value::Integer(2370));
        // Of no tuples, 0 of the type.
        assert_eq!(vars[2].init, hir::Value::Integer(0));
        assert_eq!(
            vars[3].init,
            hir::Value::Approximate(hir::Approximate::new(1185.0))
        );
        assert_eq!(
            vars[4].init,
            hir::Value::Approximate(hir::Approximate::new(0.0))
        );
        // Ten tenths, which adding one at a time makes 0.9999999999999999.
        let tenths: Vec<String> = (1..=10).map(|n| format!("{{ n: {n}, x: 1e-1 }}")).collect();
        let vars = variables(&format!(
            "var tenths := relation {{ {} }};\nvar s := sum{{r: tenths, of: x}};",
            tenths.join(", ")
        ));
        assert_eq!(
            vars[1].init,
            hir::Value::Approximate(hir::Approximate::new(1.0))
        );
        // Out of range only if the exact sum is, whatever the order.
        let big = "var big := relation { { n: 9223372036854775807 }, { n: 1 }, { n: -1 } };\n";
        let vars = variables(&format!("{big}var s := sum{{r: big, of: n}};"));
        assert_eq!(vars[1].init, hir::Value::Integer(i64::MAX));
        let source = format!("{big}var s := sum{{r: big where n > 0, of: n}};{MAIN}");
        assert_eq!(codes(&source), [Code::Overflow]);
        // Only of numbers.
        let source = format!("{PLOTS}var s := sum{{r: plots, of: region}};{MAIN}");
        assert_eq!(
            errors(&source)[0].message,
            "mismatched types: expected a type of kind `numeric`, found `text`"
        );
    }

    #[test]
    fn product_is_exact_then_rounded_once() {
        let growth = "var growth := relation { { year: 2021, factor: 1.05e0, units: 3 }, { year: 2022, factor: 1.05e0, units: 4 }, { year: 2023, factor: 9.8e-1, units: -5 } };\n";
        let vars = variables(&format!(
            "{growth}var units := product{{r: growth, of: units}};\nvar none := product{{r: growth where false, of: units}};\nvar factor := product{{r: growth, of: factor}};\nvar nothing := product{{r: growth where false, of: factor}};\nvar zero := product{{r: growth, of: year - 2022}};"
        ));
        assert_eq!(vars[1].init, hir::Value::Integer(-60));
        // Of no tuples, 1 of the type.
        assert_eq!(vars[2].init, hir::Value::Integer(1));
        // The nearest to the exact product; multiplied in turn, in either
        // order, the three are 1.08045.
        assert_eq!(
            vars[3].init,
            hir::Value::Approximate(hir::Approximate::new(1.080_450_000_000_000_1))
        );
        assert_eq!(
            vars[4].init,
            hir::Value::Approximate(hir::Approximate::new(1.0))
        );
        assert_eq!(vars[5].init, hir::Value::Integer(0));
        // Out of range only if the exact product is, whatever the order:
        // 2^63 isn't an integer, but -2^63 is.
        let big = "var big := relation { { n: 4611686018427387904 }, { n: 2 }, { n: -1 } };\n";
        let vars = variables(&format!("{big}var p := product{{r: big, of: n}};"));
        assert_eq!(vars[1].init, hir::Value::Integer(i64::MIN));
        let source = format!("{big}var p := product{{r: big where n > 0, of: n}};{MAIN}");
        assert_eq!(codes(&source), [Code::Overflow]);
    }

    #[test]
    fn other_languages_names_for_aggregates_are_fixed() {
        // Python's, which no closeness of spelling finds, or finds wrong.
        for (written, ours) in [("prod", "product"), ("len", "count"), ("LEN", "count")] {
            let of = if ours == "count" { "" } else { ", of: of" };
            let source = format!("{PLOTS}var x := {written}{{r: plots{of}}};{MAIN}");
            let fixed = fix(&source);
            assert_eq!(
                fixed,
                format!("{PLOTS}var x := {ours}{{r: plots{of}}};{MAIN}"),
                "{written}"
            );
            assert!(analyze_str(&fixed).is_ok(), "{fixed}");
        }
    }

    #[test]
    fn other_languages_names_for_reading_are_fixed() {
        // A file's lines: R's and Kotlin's, Python's and Ruby's, and .NET's,
        // which they return, fixed to `load_text_file` with its argument
        // named, then to a call with the variable its `out` argument.
        for written in ["readLines", "readlines", "ReadAllLines"] {
            let source =
                format!("operator main{{}} [\n    var lines := {written}{{\"rivers.txt\"}};\n];\n");
            let fixed = fix(&source);
            assert_eq!(
                fixed,
                "operator main{} [\n    var lines := load_text_file{path: \"rivers.txt\"};\n];\n",
                "{written}"
            );
            let fixed = fix(&fixed);
            assert_eq!(
                fixed,
                "operator main{} [\n    var lines: array text;\n    call load_text_file{path: \"rivers.txt\", out lines: lines};\n];\n"
            );
            assert!(analyze_str(&fixed).is_ok(), "{fixed}");
        }
        // A CSV file's records: pandas' and R's, and Julia's, whose
        // arguments the program then names, with the columns and whether
        // there is a header.
        for written in ["read_csv", "readcsv"] {
            let source = format!(
                "operator main{{}} [\n    var rows: array tuple {{ city: text }};\n    rows := {written}{{\"cities.csv\"}};\n];\n"
            );
            let fixed = fix(&fix(&source));
            assert_eq!(
                fixed,
                "operator main{} [\n    var rows: array tuple { city: text };\n    call load_csv{\"cities.csv\", out rows: rows};\n];\n",
                "{written}"
            );
            assert_eq!(codes(&fixed), [Code::PositionalArgument]);
        }
        // Numbers: JavaScript's, C's and Go's, and Java's; and booleans:
        // Java's and Go's.
        for (written, arg, ours) in [
            ("parseInt", "\"42\"", "parse_integer{from: "),
            ("atoi", "\"42\"", "parse_integer{from: "),
            ("Atoi", "\"42\"", "parse_integer{from: "),
            ("parseFloat", "\"2.5\"", "parse_approximate{from: "),
            ("atof", "\"2.5\"", "parse_approximate{from: "),
            ("parseDouble", "\"2.5\"", "parse_approximate{from: "),
            ("parseBoolean", "\"yes\"", "parse_boolean{from: "),
            ("ParseBool", "\"yes\"", "parse_boolean{from: "),
        ] {
            let source = format!(
                "operator main{{}} [\n    call debug_value{{v: {written}{{{arg}}}}};\n];\n"
            );
            let fixed = fix(&source);
            assert_eq!(
                fixed,
                source.replace(&format!("{written}{{"), ours),
                "{written}"
            );
            assert!(analyze_str(&fixed).is_ok(), "{fixed}");
        }
    }

    #[test]
    fn a_file_is_read_into_an_out_variable() {
        // A call, which reads the file when the program runs, into the
        // variable for `lines`.
        let source = "operator main{} [\n    var rivers: array text;\n    call load_text_file{path: \"rivers.txt\", out lines: rivers};\n];\n";
        let body = main_body(source);
        let hir::Statement::Call(call) = &body[1] else {
            panic!("{body:#?}");
        };
        assert_eq!(call.updated["lines"], hir::Target::Local(hir::LocalId(0)));
        // As a value, as other languages return one, it is fixed to a call
        // with the variable its `out` argument.
        for (written, fixed) in [
            (
                "var rivers := load_text_file{path: \"rivers.txt\"};",
                "var rivers: array text;\n    call load_text_file{path: \"rivers.txt\", out lines: rivers};",
            ),
            (
                "var rivers: array text;\n    rivers := load_text_file{path: \"rivers.txt\"};",
                "var rivers: array text;\n    call load_text_file{path: \"rivers.txt\", out lines: rivers};",
            ),
        ] {
            let source = format!("operator main{{}} [\n    {written}\n];\n");
            assert_eq!(codes(&source), [Code::OperatorAsValue], "{written}");
            assert_eq!(
                fix(&source),
                format!("operator main{{}} [\n    {fixed}\n];\n"),
                "{written}"
            );
        }
        // Not where no statement can call it: an initial value, which is
        // computed when compiling, or a read-only operator's value.
        for source in [
            format!("var lines := load_text_file{{path: \"rivers.txt\"}};{MAIN}"),
            format!(
                "operator rivers{{}}: array text [ load_text_file{{path: \"rivers.txt\"}} ];{MAIN}"
            ),
        ] {
            assert_eq!(codes(&source), [Code::OperatorAsValue], "{source}");
            assert_eq!(fix(&source), source);
        }
    }

    #[test]
    fn a_csv_file_is_read_into_tuples_of_types_it_can_read() {
        let call = |declared: &str| {
            format!(
                "operator main{{}} [\n    var rows: {declared};\n    call load_csv{{path: \"cities.csv\", header: true, columns: tuple {{ city: 1 }}, out rows: rows}};\n];\n"
            )
        };
        // Of text, integers, approximate numbers and booleans, read when
        // the program runs, into the variable for `rows`, of the tuples'
        // type.
        let source = call("array tuple { city: text }");
        let body = main_body(&source);
        let hir::Statement::Call(call_of) = &body[1] else {
            panic!("{body:#?}");
        };
        assert_eq!(call_of.updated["rows"], hir::Target::Local(hir::LocalId(0)));
        // Not of other types, which no field writes.
        let unreadable = errors(&call(
            "array tuple { city: text, districts: relation { district: text }, centre: tuple { x: integer } }",
        ));
        // Reported once, not with `columns` too.
        assert_eq!(unreadable.len(), 1, "{unreadable:#?}");
        assert_eq!(unreadable[0].code, Code::UnreadableAttribute);
        assert_eq!(
            unreadable[0].message,
            "`load_csv` can't read `centre`, of type `tuple { x: integer }` and `districts`, of type `relation { district: text }`, from a field"
        );
        // Nor of a type parameter, whose attributes, the columns, aren't
        // known here.
        let generic = "operator read{out rows: array T} types { T: any tuple } [\n    call load_csv{path: \"cities.csv\", header: true, columns: tuple { city: 1 }, out rows: rows};\n];\noperator main{} [];";
        assert_eq!(codes(generic), [Code::UnreadableAttribute]);
        // Tuples, which an array of other values hasn't.
        let texts = errors(&call("array text"));
        assert_eq!(texts[0].code, Code::TypeMismatch);
        assert!(
            texts[0]
                .notes
                .contains(&"`T` is `any tuple`, so it is a tuple, which `text` isn't".to_string()),
            "{:?}",
            texts[0].notes
        );
    }

    #[test]
    fn a_csv_files_columns_are_given_each_attribute() {
        let call = |columns: &str| {
            format!(
                "operator main{{}} [\n    var rows: array tuple {{ city: text, people: integer }};\n    call load_csv{{path: \"cities.csv\", header: false, columns: {columns}, out rows: rows}};\n];\n"
            )
        };
        analyze_str(&call("tuple { people: 3, city: 1 }")).unwrap();
        // A position for each attribute, and no other, which the array
        // that sets them is pointed at for.
        let missing = errors(&call("tuple { city: 1 }"));
        assert_eq!(missing[0].code, Code::MissingAttribute);
        assert_eq!(
            missing[0].secondary[0].message,
            "`columns` gives a column for each attribute of these tuples"
        );
        assert_eq!(
            fix(&call("tuple { city: 1, peopel: 2 }")),
            call("tuple { city: 1, people: 2 }")
        );
        assert_eq!(
            fix(&call("tuple { city: 1, people: 2, country: 3 }")),
            call("tuple { city: 1, people: 2 }")
        );
        assert_eq!(
            codes(&call("tuple { city: 1, people: 2e0 }")),
            [Code::TypeMismatch]
        );
        // Each a column, counting from 1, of its own, if known when
        // compiling, as it would be when running.
        let zero = errors(&call("tuple { city: 0, people: 2 }"));
        assert_eq!(zero[0].code, Code::CsvColumns);
        assert_eq!(
            zero[0].message,
            "`city` is read from column 0, but columns count from 1"
        );
        let shared = errors(&call("tuple { city: 2, people: 2 }"));
        assert_eq!(shared[0].code, Code::CsvColumns);
        assert_eq!(
            shared[0].message,
            "`city` and `people` are both read from column 2"
        );
        // Other languages' names for the parameters.
        let source = "operator main{} [\n    var rows: array tuple { city: text };\n    call load_csv{path: \"cities.csv\", has_header: true, usecols: tuple { city: 1 }, out rows: rows};\n];\n";
        assert_eq!(
            fix(source),
            "operator main{} [\n    var rows: array tuple { city: text };\n    call load_csv{path: \"cities.csv\", header: true, columns: tuple { city: 1 }, out rows: rows};\n];\n"
        );
    }

    #[test]
    fn a_projection_for_a_per_parameter_is_fixed_to_its_attribute() {
        // Projecting would merge equal values, which `of` counts each of.
        let source = format!("{PLOTS}var s := sum{{r: plots{{of}}}};{MAIN}");
        let missing = errors(&source);
        assert_eq!(missing[0].code, Code::MissingAttribute);
        assert!(
            missing[0]
                .notes
                .iter()
                .any(|n| n.starts_with("`plots{of}` has one tuple for each value of `of`")),
            "{missing:#?}"
        );
        assert_eq!(
            fix(&source),
            format!("{PLOTS}var s := sum{{r: plots, of: of}};{MAIN}")
        );
        // Not of several attributes, which aren't one argument.
        let source = format!("{PLOTS}var s := sum{{r: plots{{of, plot}}}};{MAIN}");
        assert!(errors(&source)[0].suggestions.is_empty(), "{source}");
    }

    #[test]
    fn a_mean_is_a_sum_divided_by_a_count() {
        // As SQL writes it, and not `min`, which `mean` is close to.
        for name in ["avg", "AVG", "mean", "average"] {
            let source = format!("{PLOTS}var m := {name}{{r: plots, of: of}};{MAIN}");
            let errors = errors(&source);
            assert_eq!(errors[0].code, Code::UnknownName, "{name}");
            assert!(errors[0].suggestions.is_empty(), "{errors:#?}");
            assert!(
                errors[0].help[0].contains("sum{r: r, of: x} / to_approximate{from: count{r: r}}"),
                "{errors:#?}"
            );
        }
    }

    #[test]
    fn values_of_any_numeric_are_added_and_multiplied() {
        let generic = "operator twice{x: T}: T types { T: any numeric } [ x + +x ];\noperator area{w: T, h: T}: T types { T: any numeric } [ w * h ];\n";
        let vars = variables(&format!(
            "{generic}var a := twice{{x: 21}};\nvar b := area{{w: 2.5e0, h: 4e0}};"
        ));
        assert_eq!(vars[0].init, hir::Value::Integer(42));
        assert_eq!(
            vars[1].init,
            hir::Value::Approximate(hir::Approximate::new(10.0))
        );
        // As the type a call gives, which can overflow.
        let source = format!("{generic}var big := twice{{x: 9223372036854775807}};{MAIN}");
        assert_eq!(codes(&source), [Code::Overflow]);
        // Only types of the kind are arguments.
        let source = format!("{generic}var t := twice{{x: \"Lagos\"}};{MAIN}");
        assert_eq!(
            errors(&source)[0].message,
            "mismatched types: expected a type of kind `numeric`, found `text`"
        );
        // Only what the kind requires applies, and the help names the kind
        // that would allow the rest.
        let body = |any: &str, body: &str| {
            errors(&format!(
                "operator f{{x: T, y: T}}: T types {{ T: any {any} }} [ {body} ];{MAIN}"
            ))
        };
        let subtracted = body("numeric", "x - y");
        assert_eq!(subtracted[0].code, Code::WrongOperandType);
        assert!(subtracted[0].help.is_empty(), "{subtracted:#?}");
        assert!(
            subtracted[0].notes[0].contains("what `numeric` requires, `+` and `*`"),
            "{subtracted:#?}"
        );
        assert_eq!(
            body("type", "x + y")[0].help,
            [
                "to add values of `T` with `+`, declare `T` as `any numeric`, whose types all have it"
            ]
        );
        // No kind requires both, so switching loses what one requires.
        let ordered = errors(&format!(
            "operator f{{x: T, y: T}}: boolean types {{ T: any numeric }} [ x < y ];{MAIN}"
        ));
        assert_eq!(
            ordered[0].help,
            [
                "to compare values of `T` with `<`, declare `T` as `any ordered` instead, if nothing else needs what `numeric` requires, `+` and `*`: no kind requires `<` as well"
            ]
        );
        // A literal is never a `T`, as generic code elsewhere can write it.
        let literal = body("numeric", "x + 1");
        assert_eq!(literal[0].code, Code::TypeMismatch);
        assert!(
            literal[0]
                .notes
                .iter()
                .any(|n| n.starts_with("`T` is whatever type each call gives it")),
            "{literal:#?}"
        );
    }

    /// The local variables and statements of the update operator `name`.
    fn update_body<'p>(
        program: &'p hir::Program,
        name: &str,
    ) -> (&'p [hir::Local], &'p [hir::Statement]) {
        let op = program.operators.iter().find(|o| o.name == name).unwrap();
        let hir::OperatorKind::Update { locals, body } = &op.kind else {
            panic!("{name} is an update operator");
        };
        (locals, &body.statements)
    }

    #[test]
    fn local_variables_are_their_operators_own() {
        let program = analyze_str(
            "var x := 1;\noperator f{n: integer} [\n    var x := n * 2;\n    var seen: relation { v: integer } key { v };\n    insert seen relation { { v: x } };\n    call debug_table{r: seen};\n];\noperator main{} [ call f{n: x}; ];",
        )
        .unwrap();
        let (locals, body) = update_body(&program, "f");
        let declared: Vec<(&str, String)> = locals
            .iter()
            .map(|l| (l.name.as_str(), l.ty.to_string()))
            .collect();
        assert_eq!(
            declared,
            [
                ("x", "integer".to_string()),
                ("seen", "relation { v: integer }".to_string())
            ]
        );
        assert_eq!(locals[1].keys, [["v".to_string()].into()]);
        // A declaration assigns the initial value, or the type's default.
        let targets: Vec<Vec<hir::Target>> = body
            .iter()
            .filter_map(|s| match s {
                hir::Statement::Assign(a) => Some(a.iter().map(|a| a.target.clone()).collect()),
                hir::Statement::Call(_)
                | hir::Statement::If(..)
                | hir::Statement::Loop(..)
                | hir::Statement::For(..) => None,
            })
            .collect();
        let local = |i| vec![hir::Target::Local(hir::LocalId(i))];
        assert_eq!(targets, [local(0), local(1), local(1)]);
        let hir::Statement::Assign(declared) = &body[1] else {
            panic!("a declaration");
        };
        assert_eq!(
            declared[0].value,
            hir::Expr::Constant(hir::Value::Relation(hir::RelationValue {
                heading: hir::Heading::new(
                    [("v".to_string(), hir::Type::Scalar(hir::Scalar::Integer))].into()
                ),
                tuples: std::collections::BTreeSet::new(),
            }))
        );
        // `x` is the local variable in `f`, hiding the program's.
        let hir::Statement::Call(call) = &body[3] else {
            panic!("a call");
        };
        assert_eq!(call.args["r"], hir::Expr::Local(hir::LocalId(1)));
        let (_, main) = update_body(&program, "main");
        let hir::Statement::Call(call) = &main[0] else {
            panic!("a call");
        };
        assert_eq!(call.args["n"], hir::Expr::Variable(hir::VariableId(0)));
    }

    #[test]
    fn a_local_variable_has_a_name_of_its_own() {
        let errors_in = |body: &str| {
            errors(&format!(
                "var total := 0;\noperator f{{p: integer}} [ {body} ];{MAIN}"
            ))
        };
        let parameter = errors_in("var p := 1;");
        assert_eq!(parameter[0].code, Code::DuplicateName);
        assert_eq!(parameter[0].message, "`p` is a parameter of `f` already");
        // Declared again, as Rust gives a variable a new value: assigned.
        let source = format!(
            "operator f{{}} [ var n := 1; var n := n + 1; call debug_value{{v: n}}; ];{MAIN}"
        );
        let again = errors(&source);
        assert_eq!(again[0].message, "`n` is declared again in `f`");
        let fixed = fix(&source);
        assert_eq!(
            fixed,
            format!("operator f{{}} [ var n := 1; n := n + 1; call debug_value{{v: n}}; ];{MAIN}")
        );
        assert!(analyze_str(&fixed).is_ok(), "{fixed}");
        // In scope in all of the body, but with a value from its declaration.
        let before = errors_in("total := total + 1; var total := 5;");
        assert_eq!(before[0].code, Code::LocalBeforeDeclaration);
        assert_eq!(
            before[0].message,
            "local variable `total` is used before its declaration"
        );
        let own = errors(&format!(
            "var n := 1;\noperator f{{}} [ var n := n + 1; ];{MAIN}"
        ));
        assert_eq!(own[0].code, Code::LocalBeforeDeclaration);
        assert!(
            own[0].help[0].contains("give the local variable a name of its own"),
            "{own:#?}"
        );
        // A type parameter's type has no default.
        let generic = errors(&format!(
            "operator f{{v: T}} types {{ T: any type }} [ var w: T; ];{MAIN}"
        ));
        assert_eq!(generic[0].code, Code::UntypedVariable);
        assert_eq!(
            generic[0].message,
            "variable `w`, of type `T`, has no initial value"
        );
    }

    #[test]
    fn a_read_only_operators_local_variables_are_lets() {
        let program = analyze_str(&format!(
            "var vat := 16;\noperator taxed{{price: integer, vat: integer}}: integer [\n    var tax := price * vat div 100;\n    var total := price + tax;\n    total := total + 1;\n    total\n];\nvar folded := taxed{{price: 1000, vat: vat}};{MAIN}"
        ))
        .unwrap();
        let op = program
            .operators
            .iter()
            .find(|o| o.name == "taxed")
            .unwrap();
        let hir::OperatorKind::ReadOnly { locals, value, .. } = &op.kind else {
            panic!("taxed is read-only");
        };
        // The assignment gives `total` a local variable of its own.
        let named: Vec<&str> = locals.iter().map(|l| l.name.as_str()).collect();
        assert_eq!(named, ["tax", "total", "total"]);
        let mut lets = Vec::new();
        let mut rest = value;
        while let hir::Expr::Let(id, _, body) = rest {
            lets.push(id.0);
            rest = body;
        }
        assert_eq!(lets, [0, 1, 2]);
        assert_eq!(*rest, hir::Expr::Local(hir::LocalId(2)));
        // Called with constants, evaluated when compiling.
        assert_eq!(program.variables[1].init, hir::Value::Integer(1161));
        // Each value is computed once: doubling a relation six times is six
        // `Let`s, not 64 copies of it.
        let doubled = "operator doubled{r: relation { a: integer }}: relation { a: integer } [ var a := r union r; var b := a union a; var c := b union b; var d := c union c; var e := d union d; e union e ];\n";
        let program = analyze_str(&format!("{doubled}{MAIN}")).unwrap();
        let op = program
            .operators
            .iter()
            .find(|o| o.name == "doubled")
            .unwrap();
        let hir::OperatorKind::ReadOnly { value, .. } = &op.kind else {
            panic!("doubled is read-only");
        };
        assert_eq!(
            format!("{value:?}").matches("Parameter").count(),
            2,
            "{value:?}"
        );
        // Swapped at once, in an `assign` block.
        let swapped = "operator swapped{a: integer, b: integer}: tuple { a: integer, b: integer } [ var x := a; var y := b; assign { x := y; y := x; }; tuple { a: x, b: y } ];\nvar t := swapped{a: 1, b: 2};";
        assert_eq!(
            variables(swapped)[0].init.to_string(),
            "tuple { a: 2, b: 1 }"
        );
    }

    #[test]
    fn value_blocks_compute_a_value_in_steps() {
        // Known when compiling, the value is too.
        let vars = variables(
            "var people := relation { { name: \"Ada\", born: 1815 }, { name: \"Alan\", born: 1912 } };\nvar span := [ var first := min{r: people, of: born}; var last := max{r: people, of: born}; last - first ];\nvar step := [ var n := 1; n := n + 1; assign { n := n * 10; }; n ];",
        );
        let values: Vec<String> = vars.iter().skip(1).map(|v| v.init.to_string()).collect();
        assert_eq!(values, ["97", "20"]);
        // A local variable whose value is known has no `Let`, so a change
        // to it starts from that value, whether the change is known too or
        // only when running.
        let vars = variables(
            "var known := [ var t := tuple { a: 1, b: 2 }; update t { a := 7 }; t ];\noperator later{n: integer}: tuple { a: integer, b: integer } [ [ var t := tuple { a: 1, b: 2 }; update t { a := n }; t ] ];\nvar called := later{n: 5};",
        );
        let values: Vec<String> = vars.iter().map(|v| v.init.to_string()).collect();
        assert_eq!(values, ["tuple { a: 7, b: 2 }", "tuple { a: 5, b: 2 }"]);
        // Known only when running, each local variable is a `Let`.
        let program = analyze_str(&format!(
            "operator f{{n: integer}}: integer [ [ var d := n * 2; d := d + 1; d * 10 ] ];{MAIN}"
        ))
        .unwrap();
        let f = program.operators.iter().find(|o| o.name == "f").unwrap();
        let hir::OperatorKind::ReadOnly { value, locals, .. } = &f.kind else {
            panic!("f is read-only");
        };
        assert!(
            matches!(value, hir::Expr::Let(_, _, body) if matches!(**body, hir::Expr::Let(..)))
        );
        assert_eq!(locals.len(), 2);
        // In an `if`'s branches, and in an `extend`'s values, with the
        // tuple's attributes in scope.
        let source = format!(
            "operator g{{c: boolean}}: integer [ if c then [ var a := 1; a ] else [ var b := 2; b ] ];\nvar e := relation {{ {{ x: 1 }} }} extend {{ y := [ var z := x + 1; z * z ] }};{MAIN}"
        );
        assert!(analyze_str(&source).is_ok(), "{:#?}", errors(&source));
        assert_eq!(
            variables("var e := relation { { x: 1 }, { x: 3 } } extend { y := [ var z := x + 1; z * z ] };")[0]
                .init
                .to_string(),
            "relation { x: integer, y: integer } { { x: 1, y: 4 }, { x: 3, y: 16 } }"
        );
    }

    #[test]
    fn value_blocks_change_nothing_else() {
        let in_f = |block: &str| {
            errors(&format!(
                "var total := 0;\noperator f{{n: integer}}: integer [ var m := 1; {block} ];{MAIN}"
            ))
        };
        let codes = |block: &str| in_f(block).iter().map(|e| e.code).collect::<Vec<_>>();
        // A local variable of its own hides nothing but a program's
        // variable.
        assert_eq!(codes("[ var n := 1; n ]"), [Code::DuplicateName]);
        assert_eq!(codes("[ var m := 1; m ]"), [Code::DuplicateName]);
        assert_eq!(
            codes("[ var a := 1; var a := 2; a ]"),
            [Code::DuplicateName]
        );
        let hiding = format!(
            "var total := 0;\noperator f{{n: integer}}: integer [ [ var total := n; total ] ];{MAIN}"
        );
        assert!(analyze_str(&hiding).is_ok());
        // It changes nothing outside.
        assert_eq!(
            in_f("[ var a := 1; m := a; a ]")[0].message,
            "a value block can't assign `m`"
        );
        assert_eq!(
            codes("[ var a := 1; total := a; a ]"),
            [Code::ReadOnlyEffect]
        );
        assert_eq!(
            codes("[ var a := 1; call debug_value{v: a}; a ]"),
            [Code::ReadOnlyEffect]
        );
        assert_eq!(
            codes("[ var a := 1; if a > 0 then [ a := 2; ]; a ]"),
            [Code::Return]
        );
        // Its local variables are out of scope after it.
        assert_eq!(codes("[ var a := 1; a ] + a"), [Code::UnknownName]);
    }

    #[test]
    fn loops_run_while_their_condition_is_true() {
        let body = main_body(
            "operator main{} [ var n := 3; while n > 0 loop [ var m := n; n := m - 1; ]; loop [ n := n + 1; ] while n < 2; ];",
        );
        let [
            _,
            hir::Statement::Loop(_, before, hir::Check::Before),
            hir::Statement::Loop(_, after, hir::Check::After),
        ] = &body[..]
        else {
            panic!("{body:?}");
        };
        // Each run's local variables are the block's own.
        assert_eq!(before.locals.len(), 1);
        assert!(after.locals.is_empty());
        let codes_of = |statements: &str| {
            codes(&format!(
                "operator f{{n: integer}} [ var i := n; {statements} ];{MAIN}"
            ))
        };
        // A condition that decides nothing, or a block that does nothing.
        for (statements, message) in [
            (
                "while true loop [ i := i - 1; ];",
                "the condition is always `true`",
            ),
            (
                "while false loop [ i := i - 1; ];",
                "the condition is always `false`",
            ),
            (
                "loop [ i := i - 1; ] while false;",
                "the condition is always `false`",
            ),
            ("while i > 0 loop [];", "this loop does nothing"),
            (
                "while 1 = 1 loop [ i := i + 1; ];",
                "the condition is always true",
            ),
        ] {
            let errors = errors(&format!(
                "operator f{{n: integer}} [ var i := n; {statements} ];{MAIN}"
            ));
            assert_eq!(errors[0].code, Code::PointlessCondition, "{statements}");
            assert_eq!(errors[0].message, message, "{statements}");
        }
        assert_eq!(codes_of("while i loop [ i := 0; ];"), [Code::TypeMismatch]);
        // A loop's block runs again, so a fix keeps it, or removes it.
        assert_eq!(
            fix(&format!(
                "operator f{{}} [ loop [ call g{{}}; ] while false; ];\noperator g{{}} [];{MAIN}"
            )),
            format!("operator f{{}} [ call g{{}}; ];\noperator g{{}} [];{MAIN}")
        );
        // Only in an update operator.
        assert_eq!(
            codes(&format!(
                "operator f{{n: integer}}: integer [ var i := n; while i > 0 loop [ i := i - 1; ]; i ];{MAIN}"
            )),
            [Code::Return]
        );
        assert_eq!(
            codes(&format!(
                "var x := [ var i := 0; while i < 3 loop [ i := i + 1; ]; i ];{MAIN}"
            )),
            [Code::Return]
        );
    }

    #[test]
    fn for_loops_go_through_an_array() {
        let body = main_body(
            "operator main{} [ var total := 0; for n in array [3, 1, 2] loop [ var doubled := n * 2; total := total + doubled; ]; ];",
        );
        let [_, hir::Statement::For(variable, array, block)] = &body[..] else {
            panic!("{body:?}");
        };
        assert!(matches!(array, hir::Expr::Constant(hir::Value::Array(_))));
        // The loop's variable, and the block's own, are the block's.
        assert_eq!(block.locals.len(), 2);
        assert!(block.locals.contains(variable));
        let source = |statements: &str| {
            format!(
                "var queue := array [\"Amara\", \"Kofi\"];\nvar rooms := relation {{ {{ room: 1 }} }};\noperator reset{{}} [ queue := array [\"Ama\"]; ];\noperator indirectly{{}} [ call reset{{}}; ];\noperator f{{}} [ {statements} ];{MAIN}"
            )
        };
        let message = |statements: &str| {
            let errors = errors(&source(statements));
            assert_eq!(errors.len(), 1, "{statements}: {errors:#?}");
            (errors[0].code, errors[0].message.clone())
        };
        // Neither its variable nor its array changes while it runs, even by
        // a call, of an operator that assigns one through another.
        assert_eq!(
            message("for name in queue loop [ name := \"x\"; ];"),
            (
                Code::NotAssignable,
                "`name`, a loop's variable, can't be assigned".to_string()
            )
        );
        assert_eq!(
            message("for name in queue loop [ if name = \"Kofi\" then [ queue := array []; ]; ];"),
            (
                Code::NotAssignable,
                "`queue` can't be assigned in a loop that goes through it".to_string()
            )
        );
        assert_eq!(
            message("for name in queue loop [ call indirectly{}; ];"),
            (
                Code::NotAssignable,
                "`indirectly` assigns `queue`, which this loop goes through".to_string()
            )
        );
        // But other variables do, and after the loop, it does too.
        let fine = source(
            "var seen := 0; for name in queue loop [ seen := seen + 1; call debug_value{v: name}; ]; call reset{};",
        );
        assert!(analyze_str(&fine).is_ok(), "{:#?}", errors(&fine));
        // Only arrays, and only in an update operator.
        assert_eq!(
            message("for r in rooms loop [ call reset{}; ];").0,
            Code::WrongOperandType
        );
        assert_eq!(
            message("var x := 0; for x in queue loop [ call debug_value{v: x}; ];").0,
            Code::DuplicateName
        );
        assert_eq!(
            message("for x in queue loop [];").0,
            Code::PointlessCondition
        );
        assert_eq!(
            codes(&format!(
                "operator g{{}}: integer [ var n := 0; for x in array [1] loop [ n := n + x; ]; n ];{MAIN}"
            )),
            [Code::Return]
        );
    }

    #[test]
    fn a_read_only_operator_changes_only_its_local_variables() {
        let read_only = |body: &str| {
            errors(&format!(
                "var total := 0;\noperator f{{n: integer}}: integer [ {body} ];{MAIN}"
            ))
        };
        let assigned = read_only("total := total + n; total");
        assert_eq!(assigned[0].code, Code::ReadOnlyEffect);
        assert_eq!(
            assigned[0].message,
            "a read-only operator can't assign a variable of the program's"
        );
        let called = read_only("var k := n; call debug_value{v: k}; k");
        assert_eq!(
            called[0].message,
            "a read-only operator can't call an operator"
        );
        // Its value is last.
        let after = read_only("return n; var later := 1;");
        assert_eq!(after[0].code, Code::Return);
        assert_eq!(after[0].message, "a read-only operator can't return early");
        // A local variable can't be computed from itself.
        let own = read_only("var m := m + 1; n");
        assert_eq!(own[0].code, Code::LocalBeforeDeclaration);
        assert_eq!(
            own[0].help,
            ["compute `m`'s initial value from something else"]
        );
    }

    #[test]
    fn a_local_relation_is_checked_against_its_keys_when_known() {
        let source = format!(
            "operator f{{}} [ var r := relation {{ {{ k: 1, v: 1 }}, {{ k: 1, v: 2 }} }} key {{ k }}; ];{MAIN}"
        );
        assert_eq!(codes(&source), [Code::KeyViolated]);
    }

    #[test]
    fn arrays_are_evaluated_when_compiling() {
        let vars = variables(
            "var a := array [1200, 950, 220];\nvar second := a[2];\nvar n := length{a: a};\nvar same := a = array [1200, 950, 220];\nvar reordered := a = array [950, 1200, 220];\nvar nested := array [array [1, 2], array [3]][1][2];",
        );
        let values: Vec<String> = vars.iter().map(|v| v.init.to_string()).collect();
        assert_eq!(
            values,
            ["array [1200, 950, 220]", "950", "3", "true", "false", "2"]
        );
        assert_eq!(
            vars[0].ty,
            hir::Type::Array(Box::new(hir::Type::Scalar(hir::Scalar::Integer)))
        );
        // The default is the empty array, written with its elements' type.
        assert_eq!(
            variables("var none: array text;")[0].init.to_string(),
            "array text []"
        );
    }

    const PEOPLE: &str = "var people := relation { { name: \"Ada\", born: 1815 }, { name: \"Edgar\", born: 1923 }, { name: \"Alan\", born: 1912 } };";

    /// The values assigned in `main`'s body, `body`, after declaring
    /// `PEOPLE`, written as literals; `None` for one known only when
    /// running.
    fn loaded(body: &str) -> Vec<Option<String>> {
        main_body(&assigning(PEOPLE, body))
            .iter()
            .flat_map(|statement| match statement {
                hir::Statement::Assign(assignments) => assignments.clone(),
                other => panic!("{other:?}"),
            })
            .map(|a| match a.value {
                hir::Expr::Constant(value) => Some(value.to_string()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn loads_order_a_relation_into_an_array() {
        // Known when compiling, the array is too: of the values of a
        // relation of one attribute, or of the tuples.
        let people = r#"relation { { name: "Ada", born: 1815 }, { name: "Edgar", born: 1923 }, { name: "Alan", born: 1912 } }"#;
        assert_eq!(
            loaded(&format!(
                "var names: array text; load names value from {people}{{name}} order [desc name]; var by_birth: array tuple {{ name: text, born: integer }}; load by_birth from {people} where born > 1900 order [asc born];"
            )),
            [
                Some("array text []".to_string()),
                Some(r#"array ["Edgar", "Alan", "Ada"]"#.to_string()),
                Some("array tuple { born: integer, name: text } []".to_string()),
                Some(
                    r#"array [tuple { born: 1912, name: "Alan" }, tuple { born: 1923, name: "Edgar" }]"#
                        .to_string()
                ),
            ]
        );
        // An empty relation takes its heading from the array's elements.
        assert_eq!(
            loaded(
                "var none: array tuple { a: integer }; load none from relation {} order [asc a];"
            ),
            [
                Some("array tuple { a: integer } []".to_string()),
                Some("array tuple { a: integer } []".to_string()),
            ]
        );
        // Known only when running.
        let program = analyze_str(&format!(
            "operator f{{r: relation {{ v: integer }}}} [ var a: array integer; load a value from r order [desc v]; ];{MAIN}"
        ))
        .unwrap();
        let f = program.operators.iter().find(|o| o.name == "f").unwrap();
        let hir::OperatorKind::Update { body, .. } = &f.kind else {
            panic!("f is an update operator");
        };
        let hir::Statement::Assign(assignments) = &body.statements[1] else {
            panic!("{:?}", body.statements);
        };
        assert_eq!(
            assignments[0].value,
            hir::Expr::Load(
                Box::new(hir::Expr::Parameter("r".to_string())),
                hir::Loading {
                    order: vec![hir::OrderItem {
                        attribute: "v".to_string(),
                        descending: true,
                    }],
                    removed: BTreeSet::new(),
                    loaded: hir::Loaded::Values,
                },
            )
        );
    }

    #[test]
    fn loads_remove_attributes_after_ordering() {
        // Ordered by `born`, then without it: equal elements are each
        // there, while a projection would merge them.
        let twins = "var twins := relation { { name: \"Ada\", born: 1815 }, { name: \"Ada\", born: 1912 }, { name: \"Alan\", born: 1900 } };";
        let loaded = |body: &str| {
            main_body(&assigning(twins, body))
                .iter()
                .flat_map(|statement| match statement {
                    hir::Statement::Assign(assignments) => assignments.clone(),
                    other => panic!("{other:?}"),
                })
                .filter_map(|a| match a.value {
                    hir::Expr::Constant(value @ hir::Value::Array(_)) => Some(value.to_string()),
                    _ => None,
                })
                .collect::<Vec<_>>()
        };
        // Known when compiling, the relation is written out.
        let people = r#"relation { { name: "Ada", born: 1815 }, { name: "Ada", born: 1912 }, { name: "Alan", born: 1900 } }"#;
        assert_eq!(
            loaded(&format!(
                "var names: array text; load names value from {people} order [asc born] remove {{born}}; var named: array tuple {{ name: text }}; load named from {people} order [desc born] remove {{born}}; var merged: array text; load merged value from {people}{{name}} order [asc name];"
            )),
            [
                "array text []",
                r#"array ["Ada", "Alan", "Ada"]"#,
                "array tuple { name: text } []",
                r#"array [tuple { name: "Ada" }, tuple { name: "Alan" }, tuple { name: "Ada" }]"#,
                "array text []",
                r#"array ["Ada", "Alan"]"#,
            ]
        );
        let source = |load: &str| {
            assigning(
                twins,
                &format!("var names: array text; var named: array tuple {{ name: text }}; {load}"),
            )
        };
        // Ordered by an attribute the relation's `remove` left out: the fix
        // removes it after the order instead.
        let removed_first = source("load names value from twins remove {born} order [asc born];");
        assert_eq!(codes(&removed_first), [Code::UnknownAttribute]);
        assert_eq!(
            fix(&removed_first),
            source("load names value from twins order [asc born] remove {born};")
        );
        // Tuples with more attributes than the elements: the fix removes
        // the others after the order.
        let more = source("load named from twins order [asc born];");
        assert_eq!(codes(&more), [Code::TypeMismatch]);
        assert_eq!(
            fix(&more),
            source("load named from twins order [asc born] remove {born};")
        );
        // Removing the wrong ones, or one the relation hasn't.
        assert_eq!(
            codes(&source(
                "load named from twins order [asc born] remove {name};"
            )),
            [Code::LoadMismatch]
        );
        assert_eq!(
            codes(&source(
                "load names value from twins order [asc born] remove {bron};"
            )),
            [Code::UnknownAttribute]
        );
        assert_eq!(
            codes(&source(
                "load names value from twins order [asc born] remove {born, born};"
            )),
            [Code::DuplicateAttribute]
        );
        // The values of the one attribute left.
        assert_eq!(
            codes(&source("load names value from twins order [asc born];")),
            [Code::LoadMismatch]
        );
    }

    #[test]
    fn loads_keep_attributes_after_ordering() {
        // `keep` names those kept, and leaves out the others; a fix writes
        // whichever of the two lists is shorter.
        let cities = "var cities := relation { { city: \"Lagos\", country: \"Nigeria\", people: 15 }, { city: \"Abuja\", country: \"Nigeria\", people: 4 } };";
        let source = |load: &str| {
            assigning(
                cities,
                &format!(
                    "var names: array text; var placed: array tuple {{ city: text, country: text }}; {load}"
                ),
            )
        };
        assert!(
            analyze_str(&source(
                "load names value from cities order [desc people] keep {city}; load placed from cities order [asc people] keep {city, country};"
            ))
            .is_ok()
        );
        let fixed = |load: &str, fixed: &str| {
            assert_eq!(fix(&source(load)), source(fixed), "{load}");
        };
        fixed(
            "load placed from cities order [asc people];",
            "load placed from cities order [asc people] remove {people};",
        );
        fixed(
            "load placed from cities order [asc people] keep {city};",
            "load placed from cities order [asc people] remove {people};",
        );
        assert_eq!(
            codes(&source("load names value from cities order [asc people];")),
            [Code::LoadMismatch]
        );
        let narrowed = assigning(
            cities,
            "var named: array tuple { city: text }; load named from cities order [asc people];",
        );
        assert_eq!(
            fix(&narrowed),
            assigning(
                cities,
                "var named: array tuple { city: text }; load named from cities order [asc people] keep {city};"
            )
        );
    }

    #[test]
    fn loads_are_assignments() {
        // Of a read-only operator's local variable, and in an `assign`
        // block, which can't change the array otherwise as well.
        let source = format!(
            "operator first{{r: relation {{ v: integer }}}}: integer [ var a: array integer; load a value from r order [asc v]; a[1] ];\noperator f{{}} [ var a: array integer; var n := 0; assign {{ load a value from relation {{ {{ v: 2 }}, {{ v: 1 }} }} order [asc v]; n := 1; }}; ];{MAIN}"
        );
        assert!(analyze_str(&source).is_ok(), "{:#?}", errors(&source));
        let source = assigning(
            PEOPLE,
            "var a: array text; assign { load a value from people{name} order [asc name]; a := array [\"Ada\"]; };",
        );
        assert_eq!(
            messages(&source),
            ["two changes to `a` conflict: `load a from …` gives `a` a whole new value"]
        );
    }

    #[test]
    fn a_load_orders_by_attributes_with_an_order() {
        let source = |any: &str| {
            format!(
                "operator f{{r: relation {{ v: T }}}} types {{ T: {any} }} [ var a: array T := array []; load a value from r order [asc v]; ];{MAIN}"
            )
        };
        assert!(analyze_str(&source("any ordered")).is_ok());
        assert_eq!(codes(&source("any type")), [Code::WrongOperandType]);
        let source = assigning(
            "var teams := relation { { team: \"core\", members: relation { { name: \"Ada\" } } } };",
            "var a: array tuple { team: text, members: relation { name: text } }; load a from teams order [asc members];",
        );
        assert_eq!(codes(&source), [Code::WrongOperandType]);
    }

    #[test]
    fn load_errors() {
        let declared = "var names: array text; var n := 0; var others: relation { name: text, born: integer }; var everyone: array tuple { name: text, born: integer };";
        let source = |load: &str| assigning(PEOPLE, &format!("{declared} {load}"));
        let fixed = |load: &str, fixed: &str| {
            assert_eq!(fix(&source(load)), source(fixed), "{load}");
        };
        // Into an array of values, with `value`, of the one attribute left:
        // the fix leaves the others out, after the order, which keeps
        // equal values each, as a projection wouldn't.
        fixed(
            "load names value from people order [asc name];",
            "load names value from people order [asc name] remove {born};",
        );
        fixed(
            "load names value from people where born > 1900 order [asc name];",
            "load names value from people where born > 1900 order [asc name] remove {born};",
        );
        // Ordered by another attribute, which is then left out.
        fixed(
            "load names value from people order [asc born];",
            "load names value from people order [asc born] remove {born};",
        );
        // Without `value`, the tuples, which don't fit: the fix writes it,
        // and leaves the others out if there are any.
        fixed(
            "load names from people{name} order [asc name];",
            "load names value from people{name} order [asc name];",
        );
        fixed(
            "load names from people order [asc name];",
            "load names value from people order [asc name] remove {born};",
        );
        // An array of tuples takes the relation's tuples as they are.
        fixed(
            "load everyone value from people order [asc name];",
            "load everyone from people order [asc name];",
        );
        // With `value`, it takes the values of a relation of one
        // tuple-valued attribute: they fit, but a tuple has no order, so
        // only the order is wrong.
        assert_eq!(
            codes(&source(
                "load everyone value from people wrap { p := {name, born} } order [asc p];"
            )),
            [Code::WrongOperandType]
        );
        assert_eq!(
            codes(&source("load n from people order [asc name];")),
            [Code::LoadMismatch]
        );
        // A relation is assigned.
        fixed(
            "load others from people order [asc name];",
            "others := people;",
        );
        // Into an array of tuples, the relation of their heading.
        assert_eq!(
            codes(&source("load everyone from people{name} order [asc name];")),
            [Code::TypeMismatch]
        );
        assert_eq!(
            codes(&source("load everyone from people order [];")),
            [Code::EmptyList]
        );
        fixed(
            "load everyone from people order [asc nam];",
            "load everyone from people order [asc name];",
        );
        fixed(
            "load everyone from people order [desc born, asc name, asc born];",
            "load everyone from people order [desc born, asc name];",
        );
    }

    #[test]
    fn loads_make_a_relation_of_an_array() {
        // Known when compiling, the relation is too: a tuple for each
        // element, the value of the attribute `as` names, or the element
        // itself, with its position if `numbered` names an attribute for
        // it, and of them those `where` keeps, which sees both. Elements
        // that make equal tuples make one.
        let texts = r#"array ["name", "Ada", "", "Ada"]"#;
        let pairs = "array [tuple { a: 1 }, tuple { a: 1 }]";
        assert_eq!(
            loaded(&format!(
                "var lines: relation {{ line: text, n: integer }}; load lines from {texts} as line numbered n where n > 1 and line <> \"\"; var names: relation {{ name: text }}; load names from {texts} as name; var rows: relation {{ a: integer, at: integer }}; load rows from {pairs} numbered at; var nested: relation {{ t: tuple {{ a: integer }} }}; load nested from {pairs} as t;"
            ))
            .into_iter()
            .skip(1)
            .step_by(2)
            .collect::<Vec<_>>(),
            [
                Some(
                    r#"relation { line: text, n: integer } { { line: "Ada", n: 2 }, { line: "Ada", n: 4 } }"#
                        .to_string()
                ),
                Some(
                    r#"relation { name: text } { { name: "" }, { name: "Ada" }, { name: "name" } }"#
                        .to_string()
                ),
                Some(
                    "relation { a: integer, at: integer } { { a: 1, at: 1 }, { a: 1, at: 2 } }"
                        .to_string()
                ),
                Some("relation { t: tuple { a: integer } } { { t: tuple { a: 1 } } }".to_string()),
            ]
        );
        // Known only when running: the restriction of the relation of the
        // array's elements.
        let program = analyze_str(&format!(
            "operator f{{a: array text}} [ var r: relation {{ x: text, n: integer }}; load r from a as x numbered n where n > 1; ];{MAIN}"
        ))
        .unwrap();
        let f = program.operators.iter().find(|o| o.name == "f").unwrap();
        let hir::OperatorKind::Update { body, .. } = &f.kind else {
            panic!("f is an update operator");
        };
        let hir::Statement::Assign(assignments) = &body.statements[1] else {
            panic!("{:?}", body.statements);
        };
        let hir::Expr::Restrict(relation, _) = &assignments[0].value else {
            panic!("{:?}", assignments[0].value);
        };
        assert_eq!(
            **relation,
            hir::Expr::Unload(
                Box::new(hir::Expr::Parameter("a".to_string())),
                hir::Unloading {
                    named: Some("x".to_string()),
                    numbered: Some("n".to_string()),
                },
            )
        );
    }

    #[test]
    fn unload_errors() {
        let declared = "var texts := array [\"Ada\", \"Alan\"]; var pairs := array [tuple { name: \"Ada\", n: 1 }]; var names: relation { name: text }; var lines: relation { name: text, n: integer }; var everyone: array tuple { name: text, born: integer };";
        let source = |load: &str| assigning(PEOPLE, &format!("{declared} {load}"));
        let fixed = |load: &str, fixed: &str| {
            assert_eq!(fix(&source(load)), source(fixed), "{load}");
        };
        // An array of values names the attribute each element is the value
        // of, and the one for its position, as the relation's heading has
        // them.
        fixed("load names from texts;", "load names from texts as name;");
        fixed(
            "load lines from texts;",
            "load lines from texts as name numbered n;",
        );
        fixed(
            "load lines from texts as line where n > 1;",
            "load lines from texts as name numbered n where n > 1;",
        );
        // An array of tuples gives their own, unless `as` names one
        // attribute for each, which no way of loading fits here.
        let unfitting = source("load names from pairs as name;");
        assert_eq!(codes(&unfitting), [Code::LoadMismatch]);
        assert_eq!(fix(&unfitting), unfitting);
        fixed(
            "load lines from pairs numbered n;",
            "load lines from pairs;",
        );
        assert_eq!(
            codes(&source("load lines from pairs numbered n;")),
            [Code::AttributeExists]
        );
        // `value` and an order are for a load into an array, and `as` and
        // `numbered` for a load into a relation.
        fixed(
            "load names value from texts as name;",
            "load names from texts as name;",
        );
        fixed(
            "load names from texts as name order [asc name];",
            "load names from texts as name;",
        );
        fixed(
            "load everyone from people as p numbered n order [asc name];",
            "load everyone from people order [asc name];",
        );
        // A relation is assigned, with the condition, if there is one.
        fixed(
            "load names from people{name} where name <> \"Ada\";",
            "names := people{name} where name <> \"Ada\";",
        );
        // A name `where` doesn't know, in a load without `numbered`, may
        // be meant as each element's position, unless it may be a
        // misspelling: `n` isn't one of `a`, whose every letter differs.
        let unnumbered = assigning(
            PEOPLE,
            &format!("{declared} var a := texts; load names from a as name where n > 1;"),
        );
        let errors = errors(&unnumbered);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].code, Code::UnknownName);
        assert!(errors[0].suggestions.is_empty());
        assert_eq!(
            errors[0].help,
            ["for each element's position, write `numbered n` before `where`"]
        );
        fixed(
            "load names from texts as name where nme <> \"Ada\";",
            "load names from texts as name where name <> \"Ada\";",
        );
        // An array is loaded with an order.
        assert_eq!(
            codes(&source("load everyone from people where born > 1900;")),
            [Code::LoadOrder]
        );
        assert_eq!(
            codes(&source("var n := 0; load n from texts;")),
            [Code::LoadMismatch]
        );
    }

    #[test]
    fn an_index_of_a_selector_takes_its_element() {
        // Unless another element can fail, which it must still be computed
        // to find out.
        let program = analyze_str(&format!(
            "operator kept{{x: integer}}: integer [ array [x, 2][2] ];\noperator failing{{x: integer}}: integer [ array [x + 1, 2][2] ];{MAIN}"
        ))
        .unwrap();
        let value = |name: &str| {
            let op = program.operators.iter().find(|o| o.name == name).unwrap();
            let hir::OperatorKind::ReadOnly { value, .. } = &op.kind else {
                panic!("{name} is read-only");
            };
            value.clone()
        };
        assert_eq!(value("kept"), hir::Expr::Constant(hir::Value::Integer(2)));
        assert!(matches!(value("failing"), hir::Expr::Index(..)));
    }

    #[test]
    fn an_empty_arrays_type_comes_from_context() {
        for (source, written) in [
            ("var a: array text := array [];", "array text []"),
            ("var a := array integer [];", "array integer []"),
            (
                "var a := array [array [], array [1]];",
                "array [array integer [], array [1]]",
            ),
            (
                "var r := relation { { m: array [] }, { m: array [\"Jun\"] } };",
                "relation { m: array text } { { m: array text [] }, { m: array [\"Jun\"] } }",
            ),
            (
                "var t: tuple { a: array integer } := tuple { a: array [] };",
                "tuple { a: array integer [] }",
            ),
            ("var same := array [] = array [1];", "false"),
        ] {
            assert_eq!(variables(source)[0].init.to_string(), written, "{source}");
        }
        for source in ["var a := array [];", "var a := array [array []];"] {
            assert_eq!(
                codes(&format!("{source}{MAIN}")),
                [Code::UnknownEmptyType],
                "{source}"
            );
        }
        // Elements of different types.
        assert_eq!(
            codes(&format!("var a := array [1, \"x\"];{MAIN}")),
            [Code::TypeMismatch]
        );
    }

    #[test]
    fn positions_out_of_range_are_reported_when_known() {
        let a = "var a := array [1200, 950, 220];\n";
        for position in ["0", "4", "-1"] {
            let source = format!("{a}var x := a[{position}];{MAIN}");
            assert_eq!(codes(&source), [Code::IndexOutOfRange], "{position}");
        }
        // Below 1, whatever the array.
        let source = format!("operator f{{a: array integer}}: integer [ a[0] ];{MAIN}");
        assert_eq!(codes(&source), [Code::IndexOutOfRange]);
        // Other languages' first and last elements.
        for (written, fixed) in [
            ("a[0]", "a[1]"),
            ("a[-1]", "a[length{a: a}]"),
            ("a[-3]", "a[length{a: a} - 2]"),
        ] {
            let source = format!("{a}var x := {written};{MAIN}");
            let fixed_source = format!("{a}var x := {fixed};{MAIN}");
            assert_eq!(fix(&source), fixed_source, "{written}");
            assert!(analyze_str(&fixed_source).is_ok(), "{fixed}");
        }
        let messages = messages(&format!(
            "{a}var x := a[4];\nvar y := array [1][2];\nvar z := array integer [][1];{MAIN}"
        ));
        assert_eq!(
            messages,
            [
                "the array has 3 elements, so none at position 4",
                "the array has 1 element, so none at position 2",
                "the array is empty, so it has no element at position 1",
            ]
        );
    }

    #[test]
    fn only_arrays_are_indexed() {
        let t = "var t := tuple { name: \"Amara\", city: \"Lagos\" };\n";
        let source = format!("{t}var x := t[\"name\"];{MAIN}");
        assert_eq!(codes(&source), [Code::WrongOperandType]);
        assert_eq!(fix(&source), format!("{t}var x := t.name;{MAIN}"));
        for source in [
            "var r := relation { { a: 1 } };\nvar x := r[1];",
            "var x := 5[1];",
            "operator f{a: any array}: integer [ a[1] ];",
        ] {
            assert_eq!(
                codes(&format!("{source}{MAIN}")),
                [Code::WrongOperandType],
                "{source}"
            );
        }
        // A position is an integer.
        assert_eq!(
            codes(&format!("var x := array [1][\"1\"];{MAIN}")),
            [Code::TypeMismatch]
        );
        // Arrays have no order.
        assert_eq!(
            codes(&format!("var x := array [1] < array [2];{MAIN}")),
            [Code::WrongOperandType]
        );
        assert_eq!(
            codes(&format!("var x := value from array [1];{MAIN}")),
            [Code::WrongOperandType]
        );
    }

    #[test]
    fn an_arrays_elements_can_have_a_type_parameter() {
        let first = "operator first{a: array E}: E types { E: any type } [ a[1] ];\n";
        let vars = variables(&format!(
            "{first}var n := first{{a: array [7, 8]}};\nvar t := first{{a: array [\"Kofi\"]}};"
        ));
        assert_eq!(vars[0].init, hir::Value::Integer(7));
        assert_eq!(vars[1].init, hir::Value::Text("Kofi".to_string()));
        let same =
            "operator same{a: array E, b: array E}: boolean types { E: any type } [ a = b ];\n";
        assert_eq!(
            codes(&format!(
                "{same}var x := same{{a: array [1], b: array [\"1\"]}};{MAIN}"
            )),
            [Code::TypeMismatch]
        );
    }

    #[test]
    fn other_languages_habits_with_arrays_are_fixed() {
        let a = "var a := array [1200, 950, 220];\nvar r := relation { { x: 1 } };\n";
        for (written, fixed) in [
            ("len{r: a}", "length{a: a}"),
            ("len{a}", "length{a: a}"),
            ("len{r}", "count{r: r}"),
            ("size{x: a}", "length{a: a}"),
            ("CARDINALITY{a: a}", "length{a: a}"),
            ("count{r: a}", "length{a: a}"),
            ("length{a: r}", "count{r: r}"),
            ("ARRAY[1]", "array [1]"),
            ("a.length{}", "length{a: a}"),
        ] {
            let source = format!("{a}var x := {written};{MAIN}");
            let fixed_source = format!("{a}var x := {fixed};{MAIN}");
            assert_eq!(fix(&source), fixed_source, "{written}");
            assert!(analyze_str(&fixed_source).is_ok(), "{fixed}");
        }
    }

    #[test]
    fn pythons_all_and_any_are_fixed() {
        // With a list's braces; in parentheses, the parser fixes them.
        for (written, fixed) in [("all{t, f}", "and{t, f}"), ("any{t, f}", "or{t, f}")] {
            let source = format!("var t := true;\nvar f := false;\nvar x := {written};{MAIN}");
            let fixed_source = source.replace(written, fixed);
            assert_eq!(fix(&source), fixed_source, "{written}");
            assert!(analyze_str(&fixed_source).is_ok(), "{fixed}");
        }
        // A relation of that name is projected as any other.
        assert!(
            analyze_str(&format!(
                "var all := relation {{ {{ t: 1 }} }};\nvar x := all{{t}};{MAIN}"
            ))
            .is_ok()
        );
    }

    #[test]
    fn an_if_computes_only_the_branch_taken() {
        let vars = variables(
            "var none := relation { n: integer } {};\nvar mean := if count{r: none} = 0 then 0 else sum{r: none, of: n} div count{r: none};\nvar label := if count{r: none} = 1 then \"one\" else if count{r: none} = 0 then \"none\" else \"many\";",
        );
        assert_eq!(vars[1].init, hir::Value::Integer(0));
        assert_eq!(vars[2].init, hir::Value::Text("none".to_string()));
        // A call evaluated when compiling takes only one branch too.
        let safe =
            "operator safe{a: integer, b: integer}: integer [ if b = 0 then 0 else a div b ];\n";
        assert_eq!(
            variables(&format!("{safe}var x := safe{{a: 7, b: 0}};"))[0].init,
            hir::Value::Integer(0)
        );
        // Unknown when compiling, a branch that fails whenever it is taken
        // is an error.
        let source =
            format!("operator f{{n: integer}}: integer [ if n > 0 then 1 else 1 div 0 ];{MAIN}");
        assert_eq!(codes(&source), [Code::DivisionByZero]);
    }

    #[test]
    fn an_ifs_branches_have_one_type() {
        // An empty relation takes its heading from the other branch, in
        // either order.
        for source in [
            "operator f{n: integer}: boolean [ (if n > 0 then relation {} else relation { { a: 1 } }) = relation {} ];",
            "operator f{n: integer}: boolean [ (if n > 0 then relation { { a: 1 } } else relation {}) = relation {} ];",
            "operator f{n: integer}: relation { a: integer } [ if n > 0 then relation {} else relation {} ];",
        ] {
            assert!(analyze_str(&format!("{source}{MAIN}")).is_ok(), "{source}");
        }
        for (source, code) in [
            ("var x := if 1 = 1 then 1 else \"one\";", Code::TypeMismatch),
            ("var x := if 1 then 1 else 2;", Code::TypeMismatch),
            (
                "var x := if 1 = 1 then relation {} else relation {};",
                Code::UnknownEmptyType,
            ),
        ] {
            assert!(
                codes(&format!("{source}{MAIN}")).contains(&code),
                "{source}"
            );
        }
        // Both alike, it is the one, if the condition can't fail.
        let program = analyze_str(&format!(
            "operator same{{n: integer}}: integer [ if n > 0 then n else n ];{MAIN}"
        ))
        .unwrap();
        let op = program.operators.iter().find(|o| o.name == "same").unwrap();
        let hir::OperatorKind::ReadOnly { value, .. } = &op.kind else {
            panic!("same is read-only");
        };
        assert_eq!(*value, hir::Expr::Parameter("n".to_string()));
    }

    #[test]
    fn an_if_that_decides_nothing_is_fixed() {
        let f = |body: &str| format!("operator f{{n: integer}}: boolean [ {body} ];{MAIN}");
        for (written, fixed) in [
            ("if true then n > 0 else n < 0", "n > 0"),
            ("if false then n > 0 else n < 0", "n < 0"),
            ("if n > 0 then true else false", "n > 0"),
            ("if n > 0 then false else true", "not (n > 0)"),
            ("if (n > 0) then false else true", "not (n > 0)"),
        ] {
            assert_eq!(codes(&f(written)), [Code::PointlessCondition], "{written}");
            assert_eq!(fix(&f(written)), f(fixed), "{written}");
            assert!(analyze_str(&f(fixed)).is_ok(), "{fixed}");
        }
        // Known only from a variable's value, it decides.
        assert!(
            analyze_str(&format!(
                "var on := true;\nvar x := if on then 1 else 2;{MAIN}"
            ))
            .is_ok()
        );
    }

    #[test]
    fn a_blocks_local_variables_are_in_scope_in_it_alone() {
        let f = |body: &str| format!("operator f{{n: integer}} [ {body} ];{MAIN}");
        // Side by side, each block can have one of a name.
        let program = analyze_str(&f(
            "if n > 0 then [ var note := \"positive\"; call debug_value{v: note}; ] else [ var note := 1; call debug_value{v: note}; ];",
        ))
        .unwrap();
        let (locals, body) = update_body(&program, "f");
        assert_eq!(locals.len(), 2);
        let hir::Statement::If(_, then, otherwise) = &body[0] else {
            panic!("an `if`");
        };
        assert_eq!(then.locals, [hir::LocalId(0)]);
        assert_eq!(otherwise.locals, [hir::LocalId(1)]);
        for (body, code) in [
            (
                "if n > 0 then [ var note := 1; ]; call debug_value{v: note};",
                Code::UnknownName,
            ),
            (
                "if n > 0 then [ var note := 1; ]; note := 2;",
                Code::UnknownName,
            ),
            (
                "var note := 0; if n > 0 then [ var note := 1; ];",
                Code::DuplicateName,
            ),
            (
                "if n > 0 then [ var note := 1; ]; var note := 2;",
                Code::DuplicateName,
            ),
            ("if n > 0 then [ var n := 1; ];", Code::DuplicateName),
            (
                "if n > 0 then [ call debug_value{v: note}; var note := 1; ];",
                Code::LocalBeforeDeclaration,
            ),
        ] {
            assert!(
                codes(&f(body)).contains(&code),
                "{body}: {:?}",
                codes(&f(body))
            );
        }
        // Declared again in a block within, the first is assigned, if it
        // comes first.
        assert_eq!(
            fix(&f("var note := 0; if n > 0 then [ var note := 1; ];")),
            f("var note := 0; if n > 0 then [ note := 1; ];")
        );
    }

    #[test]
    fn an_if_statement_runs_one_block() {
        let f = |body: &str| format!("operator f{{n: integer}} [ {body} ];{MAIN}");
        // Known when compiling, the block it doesn't run has no failures.
        for body in [
            "if 1 = 2 then [ call debug_value{v: 1 div 0}; ];",
            "if 1 = 1 then [ call debug_value{v: n}; ] else [ call debug_value{v: 1 div 0}; ];",
            "if 1 = 1 then [ call debug_value{v: n}; ] else if 1 div 0 = 1 then [ call debug_value{v: n}; ];",
        ] {
            assert!(analyze_str(&f(body)).is_ok(), "{body}");
        }
        assert_eq!(
            codes(&f("if n > 0 then [ call debug_value{v: 1 div 0}; ];")),
            [Code::DivisionByZero]
        );
        assert_eq!(
            codes(&f("if n then [ call debug_value{v: n}; ];")),
            [Code::TypeMismatch]
        );
        // A read-only operator chooses with an `if` expression.
        let source = format!(
            "operator g{{n: integer}}: integer [ var x := 0; if n > 0 then [ x := 1; ]; x ];{MAIN}"
        );
        assert_eq!(codes(&source), [Code::Return]);
    }

    #[test]
    fn an_if_statement_that_decides_nothing_is_fixed() {
        let f = |body: &str| format!("operator f{{n: integer}} [ {body} ];{MAIN}");
        let show = "call debug_value{v: n};";
        for (written, fixed) in [
            (format!("if true then [ {show} ];"), show.to_string()),
            (format!("if false then [ {show} ];"), String::new()),
            (
                format!("if false then [ {show} ] else [ {show} call debug_value{{v: 1}}; ];"),
                format!("{show} call debug_value{{v: 1}};"),
            ),
            ("if n > 0 then [];".to_string(), String::new()),
            ("if n > 0 then [] else [];".to_string(), String::new()),
            (
                format!("if n > 0 then [] else [ {show} ];"),
                format!("if not (n > 0) then [ {show} ];"),
            ),
            (
                format!("if n > 0 then [ {show} ] else [];"),
                format!("if n > 0 then [ {show} ];"),
            ),
        ] {
            assert_eq!(codes(&f(&written)), [Code::PointlessCondition], "{written}");
            assert_eq!(fix(&f(&written)), f(&fixed), "{written}");
            assert!(analyze_str(&f(&fixed)).is_ok(), "{fixed}");
        }
    }

    #[test]
    fn named_headings() {
        // A heading's name stands for its attributes, after `relation` and
        // `tuple`, in types and selectors, declared in any order, and nested.
        let source = "var r: relation home := relation home { { city: \"Lima\", rooms: 3 } };
var t: tuple home := tuple from r;
var s := relation street { { homes: r } };
heading street := { homes: relation home };
heading home := { city: text, rooms: integer };
";
        let home = hir::Heading::new(
            [
                ("city".to_string(), hir::Type::Scalar(hir::Scalar::Text)),
                ("rooms".to_string(), hir::Type::Scalar(hir::Scalar::Integer)),
            ]
            .into(),
        );
        let types: Vec<hir::Type> = variables(source).into_iter().map(|v| v.ty).collect();
        assert_eq!(
            types,
            [
                hir::Type::Relation(home.clone()),
                hir::Type::Tuple(home.clone()),
                hir::Type::Relation(hir::Heading::new(
                    [("homes".to_string(), hir::Type::Relation(home))].into()
                )),
            ]
        );
        // The same type as its attributes written out.
        assert!(
            analyze_str(&format!(
                "heading home := {{ city: text }};\nvar r: relation {{ city: text }} := relation home {{}};{MAIN}"
            ))
            .is_ok()
        );
    }

    #[test]
    fn named_heading_errors() {
        let home = "heading home := { city: text, rooms: integer };\n";
        // Its name alone isn't a type: a relation's or a tuple's is.
        let source = format!("{home}operator show{{h: home}} [];{MAIN}");
        assert_eq!(messages(&source)[0], "`home` is a heading, not a type");
        assert_eq!(
            fix(&source),
            format!("{home}operator show{{h: relation home}} [];{MAIN}")
        );
        // An unknown name, close to one, or a type's.
        let source = format!("{home}var r: relation hom;{MAIN}");
        assert_eq!(fix(&source), format!("{home}var r: relation home;{MAIN}"));
        assert_eq!(
            messages(&format!("var r: relation integer;{MAIN}")),
            ["unknown heading `integer`"]
        );
        // Defined once, and not as a type.
        assert_eq!(
            codes(&format!(
                "{home}{home}heading text := {{ a: integer }};{MAIN}"
            )),
            [Code::DuplicateName, Code::DuplicateName]
        );
        // Not containing itself, directly or through others; a heading
        // that names one of a cycle isn't reported too.
        assert_eq!(
            messages(&format!(
                "heading part := {{ parts: relation part }};\nheading a := {{ b: tuple b }};\nheading b := {{ a: relation a }};\nheading c := {{ a: tuple a }};{MAIN}"
            )),
            [
                "the heading `part` contains itself",
                "the headings `a` and `b` contain each other",
            ]
        );
        // Its attributes are of specific types.
        assert_eq!(
            codes(&format!("heading h := {{ a: any type }};{MAIN}")),
            [Code::AnyOutsideParameter]
        );
        // A value against it is checked against its declaration's types.
        let mismatch = errors(&format!(
            "{home}var r: relation home := relation {{ {{ city: \"Lima\", rooms: \"3\" }} }};{MAIN}"
        ));
        assert_eq!(mismatch[0].code, Code::TypeMismatch);
    }
}
