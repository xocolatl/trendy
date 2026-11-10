mod assign;
mod block;
mod body;
mod builtins;
mod catalog;
mod check;
mod constraints;
mod databases;
mod errors;
mod globals;
mod keys;
mod locals;
mod named_types;
mod narrowed;
mod operators;
mod ops;
mod scope;
mod sections;
mod types;
mod written_times;

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use trendy_diag::{Diagnostic, Span};
use trendy_hir as hir;
use trendy_syntax::SyntaxToken;
use trendy_syntax::ast::{self, AstNode, HeadingDecl, Item, SourceFile, TypeDecl, VarDecl};

use crate::operators::Operators;

/// Check a parsed program, its units with the units that come with the
/// compiler, the `library`, and translate it to HIR. The prelude's
/// declarations are in scope in every unit, as its own are, so a unit can
/// extend them: a subtype of the prelude's union type, a version of its
/// specification. The library's other units are the program's too, which
/// a unit sees when it uses them, as it does its own. The catalog is filled
/// with what the program's units are and use, and its constraints, the
/// rules between units, are checked of it.
///
/// This is the last layer allowed to report errors to the user. Drivers run
/// it only on trees without syntax errors; parts that are missing anyway are
/// skipped, never guessed at.
///
/// # Errors
///
/// Returns every semantic error, in source order. Their spans are in the
/// units' texts, one after another, then the library's, in the order it
/// has them (see `trendy_diag::Sources`), so the first unit's are those it
/// has alone. A declaration of the library's that doesn't match the
/// compiler is an internal error, as is a constraint of the catalog's that
/// sema can't report.
///
/// # Panics
///
/// If `units` is empty, as a program has main's unit, first, or if the
/// library hasn't the prelude or the catalog.
pub fn analyze(units: &[File], library: &[File]) -> Result<hir::Program, Vec<Diagnostic>> {
    let files = Files::new(units, library);
    let mut errors = Vec::new();
    let checked = units_checked(&files, &mut errors);
    let main = files.main().expect("a program has main's unit");
    let entry = entry(main, &checked.namespaces, &checked.operators, &mut errors);
    errors.sort_by_key(|d| d.span().start);
    match entry {
        Some(entry) if errors.is_empty() => Ok(checked.finish(entry, &files)),
        _ => Err(errors),
    }
}

/// Check the unit first in `units`, with those it uses, and the `library`,
/// without making a program of it: what [`analyze`] reports, but that it
/// needn't have `main` if its interface gives other units something to use.
///
/// Every error, in source order, in the same spans as [`analyze`]'s.
///
/// # Panics
///
/// As for [`analyze`].
#[must_use]
pub fn check_unit(units: &[File], library: &[File]) -> Vec<Diagnostic> {
    let files = Files::new(units, library);
    let mut errors = Vec::new();
    let checked = units_checked(&files, &mut errors);
    let main = files.main().expect("a program has main's unit");
    if main.declares_main() || !main.exports() {
        entry(main, &checked.namespaces, &checked.operators, &mut errors);
    }
    errors.sort_by_key(|d| d.span().start);
    errors
}

/// Where each name written in `units` and `library` refers to is declared, as checking
/// them, as [`check_unit`] does, or, with no `units`, the library alone, as
/// [`check_library`] does, finds it: an operator's, by a call; a type's or a heading's,
/// by its name in a type; a variable's, a constant's, a local variable's or a
/// parameter's, by a value's name; and a unit's, by its name in a `uses`. Each is in the
/// spans their errors are in, in source order, a name once. What has errors is checked
/// as far as it can be, so what refers to nothing is left out. An attribute or a
/// component is left out too: it refers to a heading, which may be written nowhere.
///
/// # Panics
///
/// If the library hasn't the prelude or the catalog.
#[must_use]
pub fn definitions(units: &[File], library: &[File]) -> Vec<Definition> {
    // Only what resolves names: the checks of sections and uses only report errors.
    check(&Files::new(units, library), &mut Vec::new())
        .namespaces
        .definitions()
}

/// A name written in the program, where [`definitions`] says it is declared.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Definition {
    /// The name, as written.
    pub name: Span,
    /// Its declaration's name, or, of a unit, the start of its file.
    pub declaration: Span,
}

/// What [`analyze`] and [`check_unit`] both check: each unit's sections, that
/// each used unit has a use, and all of them together.
fn units_checked(files: &Files, errors: &mut Vec<Diagnostic>) -> Checked {
    for unit in files.sectioned() {
        sections::check(unit, errors);
    }
    files.used_have_a_use(errors);
    let checked = check(files, errors);
    // What the interface mentions several times, reported at the first, as its
    // fix is the same.
    errors.sort_by_key(|d| d.span().start);
    let mut seen: Vec<(String, Vec<trendy_diag::Suggestion>)> = Vec::new();
    errors.retain(|d| {
        if d.code != trendy_diag::Code::InterfaceDeclaration || d.suggestions.is_empty() {
            return true;
        }
        let fix = (d.message.clone(), d.suggestions.clone());
        let first = !seen.contains(&fix);
        seen.push(fix);
        first
    });
    checked
}

/// Check the units that come with the compiler, the `library`, alone, as
/// the units of a program without `main`: that they declare exactly the
/// built-ins the compiler implements, each as it implements it, that what
/// they define in trendy is valid, and that the catalog is as the compiler
/// fills it and reports the breaking of its constraints.
///
/// Every error, in source order: a declaration that doesn't match the
/// compiler is an internal error, at the declaration, since the library is
/// part of the compiler, and an error in what it defines in trendy is
/// reported as a program's would be.
///
/// # Panics
///
/// If the library hasn't the prelude or the catalog.
#[must_use]
pub fn check_library(library: &[File]) -> Vec<Diagnostic> {
    let files = Files::new(&[], library);
    let mut errors = Vec::new();
    for unit in files.sectioned() {
        sections::check(unit, &mut errors);
    }
    check(&files, &mut errors);
    errors.sort_by_key(|d| d.span().start);
    errors
}

/// What a unit is checked as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Source {
    /// One of the program's own, which declares no built-ins.
    Program,
    /// The prelude, which every unit sees, and which declares the kinds and
    /// the built-in types.
    Prelude,
    /// Another unit that comes with the compiler, which a unit sees when it
    /// uses it, as `trendy.files`.
    Library,
}

impl Source {
    /// Whether it comes with the compiler: it can declare built-in
    /// operators, and no fix changes it.
    pub(crate) fn is_compilers(self) -> bool {
        self != Source::Program
    }
}

/// A file of the program, as a driver gives it to sema: its unit's name, its
/// path, which a failure in it writes it by, and its tree, parsed without
/// errors.
#[derive(Clone, Copy)]
pub struct File<'a> {
    pub name: &'a str,
    pub path: &'a str,
    pub tree: &'a SourceFile,
}

/// The files checked together: the program's units', main's first, then
/// the library's, in the order the driver gives them, so that a span is an
/// offset into their texts one after another. The prelude's declarations
/// are visited first, so a name defined in both is the program's duplicate
/// of the prelude's.
pub(crate) struct Files {
    /// Each unit's tree, by its [`hir::FileId`], with what it is checked as.
    units: Vec<(Source, SourceFile)>,
    /// The prelude's unit.
    prelude: hir::FileId,
    /// The catalog's unit.
    catalog: hir::FileId,
    /// Each file's path, by its [`hir::FileId`].
    paths: Vec<String>,
    /// Each file's unit's name, and where its text ends in the program's,
    /// by its [`hir::FileId`].
    names: scope::Units,
    /// The declarations in units' implementations of types their
    /// interfaces declare, which complete those (see
    /// [`ast::TypeDecl::implementation`]), each found once, by where the
    /// interface's starts in the program's text.
    parts: HashMap<usize, TypeDecl>,
    /// Where each of `parts` starts: they are no items of their own.
    part_starts: HashSet<usize>,
}

impl Files {
    /// The files of `units` and of the `library`, as the trees of one
    /// program, the units' first, so that a span is an offset into their
    /// texts one after another.
    ///
    /// # Panics
    ///
    /// If the library hasn't the prelude or the catalog.
    fn new(units: &[File], library: &[File]) -> Files {
        let all: Vec<&File> = units.iter().chain(library).collect();
        let trees: Vec<SourceFile> = all.iter().map(|file| file.tree.clone()).collect();
        let files: Vec<SourceFile> = ast::Program::new(&trees).files().collect();
        let find = |unit: trendy_prelude::Unit| {
            let at = library
                .iter()
                .position(|file| file.name == unit.name)
                .unwrap_or_else(|| panic!("the library has `{}`", unit.name));
            hir::FileId(units.len() + at)
        };
        let (prelude, catalog) = (find(trendy_prelude::PRELUDE), find(trendy_prelude::CATALOG));
        let unit_files: Vec<(&str, &SourceFile)> = all
            .iter()
            .zip(&files)
            .map(|(file, tree)| (file.name, tree))
            .collect();
        // The library alone, as it is checked, has nothing to be used by.
        let roots = if units.is_empty() {
            0..all.len()
        } else {
            0..units.len()
        };
        let roots = roots.map(hir::FileId).chain([prelude, catalog]);
        let names = scope::Units::new(&unit_files, prelude, roots);
        let units: Vec<(Source, SourceFile)> = files
            .into_iter()
            .enumerate()
            .map(|(i, tree)| {
                let source = if i < units.len() {
                    Source::Program
                } else if hir::FileId(i) == prelude {
                    Source::Prelude
                } else {
                    Source::Library
                };
                (source, tree)
            })
            .collect();
        let parts: HashMap<usize, TypeDecl> = units
            .iter()
            .flat_map(|(_, tree)| tree.types())
            .filter_map(|decl| Some((decl.span().start, decl.implementation()?)))
            .collect();
        let part_starts = parts.values().map(|part| part.span().start).collect();
        Files {
            units,
            prelude,
            catalog,
            paths: all.iter().map(|file| file.path.to_string()).collect(),
            names,
            parts,
            part_starts,
        }
    }

    /// Main's unit, the first, unless only the library is checked.
    fn main(&self) -> Option<&SourceFile> {
        self.units
            .first()
            .filter(|(source, _)| *source == Source::Program)
            .map(|(_, tree)| tree)
    }

    /// Report each unit of the program's but main's, whose own use is its
    /// [`entry`]'s to say, with neither `main` nor anything in its interface:
    /// nothing can run it, and nothing can use it (A0002).
    fn used_have_a_use(&self, errors: &mut Vec<Diagnostic>) {
        let program: Vec<&SourceFile> = self
            .units
            .iter()
            .take_while(|(source, _)| *source == Source::Program)
            .map(|(_, tree)| tree)
            .collect();
        for (i, unit) in program.iter().enumerate().skip(1) {
            if unit.declares_main() || unit.exports() {
                continue;
            }
            let name = self.names.name(hir::FileId(i));
            let used_at = program
                .iter()
                .flat_map(|other| other.uses())
                .flat_map(|uses| uses.units().collect::<Vec<_>>())
                .find(|used| used.name().is_some_and(|n| n.text() == name))
                .map(|used| used.span());
            errors.push(errors::unit_of_no_use(name, unit, used_at));
        }
    }

    /// Each unit with sections, whose rules are checked: every one but
    /// the prelude, which every unit sees whole.
    fn sectioned(&self) -> impl Iterator<Item = &SourceFile> {
        self.units
            .iter()
            .filter(|(source, _)| *source != Source::Prelude)
            .map(|(_, tree)| tree)
    }

    /// The file of `unit`.
    pub(crate) fn tree(&self, unit: hir::FileId) -> &SourceFile {
        &self.units[unit.0].1
    }

    /// The catalog's unit.
    pub(crate) fn catalog(&self) -> &SourceFile {
        &self.units[self.catalog.0].1
    }

    /// Each unit's file, with its unit's name, in the program's order.
    pub(crate) fn named(&self) -> impl Iterator<Item = (&str, &SourceFile)> {
        self.units
            .iter()
            .enumerate()
            .map(|(i, (_, file))| (self.names.name(hir::FileId(i)), file))
    }

    /// The prelude.
    pub(crate) fn prelude(&self) -> &SourceFile {
        &self.units[self.prelude.0].1
    }

    /// The file of the unit `name`, one that comes with the compiler, as every
    /// program has them.
    pub(crate) fn unit_named(&self, name: &str) -> &SourceFile {
        self.named()
            .find(|(unit, _)| *unit == name)
            .map(|(_, file)| file)
            .expect("every program has the units that come with the compiler")
    }

    /// Each file, with what it is checked as, the prelude first, then the
    /// others in the program's order.
    pub(crate) fn each(&self) -> impl Iterator<Item = (Source, &SourceFile)> {
        let prelude = &self.units[self.prelude.0];
        std::iter::once(prelude)
            .chain(
                self.units
                    .iter()
                    .filter(|(source, _)| *source != Source::Prelude),
            )
            .map(|(source, file)| (*source, file))
    }

    /// Every definition, each with the file it is in, the prelude's first,
    /// each file's in source order: but for a type's declaration in its
    /// unit's implementation, which completes the interface's, its part
    /// (see [`ast::TypeDecl::implementation`]).
    pub(crate) fn items(&self) -> impl Iterator<Item = (Source, Item)> {
        self.each().flat_map(|(source, file)| {
            file.items()
                .filter(|item| !matches!(item, Item::Type(decl) if self.is_part(decl)))
                .map(move |item| (source, item))
        })
    }

    pub(crate) fn headings(&self) -> impl Iterator<Item = HeadingDecl> {
        self.each().flat_map(|(_, file)| file.headings())
    }

    /// Whether `decl` is a type's declaration in its unit's implementation
    /// that completes the interface's.
    fn is_part(&self, decl: &TypeDecl) -> bool {
        !self.part_starts.is_empty() && self.part_starts.contains(&decl.span().start)
    }

    /// Every type's declaration, but for its part in its unit's
    /// implementation, as [`Files::items`] gives them.
    pub(crate) fn types(&self) -> impl Iterator<Item = TypeDecl> {
        self.each()
            .flat_map(|(_, file)| file.types())
            .filter(|decl| !self.is_part(decl))
    }

    pub(crate) fn variables(&self) -> impl Iterator<Item = VarDecl> {
        self.each().flat_map(|(_, file)| file.variables())
    }

    pub(crate) fn constants(&self) -> impl Iterator<Item = VarDecl> {
        self.each().flat_map(|(_, file)| file.constants())
    }

    pub(crate) fn constraints(&self) -> impl Iterator<Item = ast::ConstraintDecl> {
        self.each().flat_map(|(_, file)| file.constraints())
    }

    pub(crate) fn relvars(&self) -> impl Iterator<Item = VarDecl> {
        self.each().flat_map(|(_, file)| file.relvars())
    }
}

/// Where each place in the files is: the sites of what runs there.
pub(crate) struct Sites<'a> {
    sources: trendy_diag::Sources<'a>,
}

impl<'a> Sites<'a> {
    /// The sites in the files whose paths and texts are `files`, each file
    /// at its [`hir::FileId`], in the program's order.
    fn new(files: &'a [(&'a str, String)]) -> Self {
        Sites {
            sources: trendy_diag::Sources::new(
                files.iter().map(|(path, text)| (*path, text.as_str())),
            ),
        }
    }

    /// The site of what starts at `span`.
    pub(crate) fn at(&self, span: Span) -> hir::Site {
        let located = self.sources.locate(span);
        let position = located.file.map.line_col(located.span.start);
        hir::Site {
            file: hir::FileId(located.index),
            line: position.line,
            column: position.col,
        }
    }
}

/// Files, checked, with every error reported.
pub(crate) struct Checked {
    namespaces: Namespaces,
    operators: Operators,
    evaluated: globals::Evaluated,
    /// Each valid update operator's local variables and statements.
    bodies: HashMap<hir::OperatorId, (Vec<hir::Local>, hir::Block)>,
    /// Each valid constraint on the variables.
    constraints: Vec<hir::VariableConstraint>,
    /// The identity of each error the language makes, declared as the
    /// compiler makes it.
    language_errors: BTreeMap<hir::LanguageError, String>,
}

/// Check `files`, as one unit.
pub(crate) fn check(files: &Files, errors: &mut Vec<Diagnostic>) -> Checked {
    // The kinds and built-in types first, as the compiler implements them,
    // which only the prelude declares.
    let kinds = builtins::kinds(files.prelude(), errors);
    let mut namespaces = collect_names(&kinds, files, errors);
    namespaces.found_units(files);
    // Each file's path and text, in the program's order, the units' first.
    let texts: Vec<(&str, String)> = files
        .units
        .iter()
        .zip(&files.paths)
        .map(|((_, file), path)| (path.as_str(), file.syntax().text().to_string()))
        .collect();
    let sites = Sites::new(&texts);

    // The headings' attributes and the types' components first, which any
    // type can name, a built-in's parameters too, and their constraints,
    // whose operators come first.
    named_types::resolve(files, &mut namespaces, hir::OperatorId(0), errors);
    builtins::library(files, &mut namespaces, errors);
    builtins::given_declared(files, errors);

    // Every operator's signature first, so that anything can refer to an
    // operator defined after it.
    let operators = operators::signatures(files, &namespaces, errors);
    let language_errors = builtins::failures(files, &namespaces, &operators, errors);

    // Then the globals: the variables' initial values, and the read-only
    // operators' values, which initial values can call and which read
    // variables, each when first needed.
    let declared = variables(files, &namespaces);
    let evaluated =
        globals::Globals::new(&namespaces, &operators, &declared, &sites).evaluate(files, errors);

    // Then the constraints on the variables, which their initial values
    // satisfy, and the update operators' bodies, which refer to both.
    let context = body::Context {
        files,
        sites: &sites,
        namespaces: &namespaces,
        operators: &operators,
        values: &evaluated.values,
        recursive: &evaluated.recursive,
        variables: &evaluated.variables,
        constants: &evaluated.constants,
        declared: &declared,
    };
    let constraints = constraints::check(files, &context, errors);
    let databases = databases::check(files, &context, errors);
    // The catalog's constraints, the rules between units, checked of the
    // program's units.
    catalog::check(files, &context, &databases, errors);
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
    let main = files
        .main()
        .and_then(|file| main_signature(file, &namespaces, &operators))
        .map(|signature| signature.id);
    body::deferred_checks(
        &deferred, &bodies, &evaluated, &operators, &declared, main, errors,
    );
    Checked {
        namespaces,
        operators,
        evaluated,
        bodies,
        constraints,
        language_errors,
    }
}

impl Checked {
    /// The program of `files`, without errors, which starts in `entry`.
    pub(crate) fn finish(mut self, entry: hir::OperatorId, files: &Files) -> hir::Program {
        let message = self
            .operators
            .message()
            .expect("a prelude without errors declares `message`")
            .id;
        let bindings = ops::Bindings {
            operators: &self.operators,
            namespaces: &self.namespaces,
            values: &self.evaluated.values,
            recursive: &self.evaluated.recursive,
        };
        let can_fail: Vec<bool> = self
            .operators
            .iter()
            .map(|signature| bindings.can_fail(signature.id))
            .collect();
        let operators = self
            .operators
            .iter()
            .map(|signature| {
                let bodiless = signature.bodiless();
                let kind = match (signature.builtin, &signature.result) {
                    (Some(builtin), _) => hir::OperatorKind::Builtin(builtin),
                    // Its versions compute it.
                    (None, result) if bodiless => hir::OperatorKind::Specification {
                        result: result.clone(),
                    },
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
                    name: self.operators.identity(signature.id, &self.namespaces),
                    types: signature.types.clone(),
                    params: signature.params.clone(),
                    kind,
                    versions: signature.versions.clone(),
                    constraints: signature.constraints.clone(),
                    can_fail: can_fail[signature.id.0],
                }
            })
            .collect();
        let variables = self
            .evaluated
            .variables
            .into_iter()
            .map(|v| v.expect("a program without errors has every variable"))
            .collect();
        let constants = self
            .evaluated
            .constants
            .into_iter()
            .map(|c| c.expect("a program without errors has every constant"))
            .collect();
        let library = self.namespaces.library().clone();
        hir::Program {
            operators,
            variables,
            constants,
            files: files.paths.clone(),
            constraints: self.constraints,
            subtypes: self.namespaces.subtypes,
            language_errors: self.language_errors,
            library,
            entry,
            message,
        }
    }
}

/// The operator named `main` in the unit of `file`, the one without parameters, if
/// there are several.
fn main_signature<'o>(
    file: &SourceFile,
    namespaces: &Namespaces,
    operators: &'o Operators,
) -> Option<&'o operators::Signature> {
    // The main unit's own, not another's it sees.
    let mains: Vec<&operators::Signature> = operators
        .declared_in("main", namespaces.unit_at(file.span()))
        .collect();
    mains
        .iter()
        .find(|s| s.written.is_empty())
        .or_else(|| mains.first())
        .copied()
}

/// The operator named `main`, which must be the program's own, an update
/// operator taking no parameters, or only the errors it gives, `out
/// errors: relation transaction_collection`, or `None` after reporting why there
/// is none the program can start in.
fn entry(
    file: &SourceFile,
    namespaces: &Namespaces,
    operators: &Operators,
    errors: &mut Vec<Diagnostic>,
) -> Option<hir::OperatorId> {
    let main = main_signature(file, namespaces, operators);
    let Some((signature, decl)) = main.and_then(|s| Some((s, s.decl.as_ref()?))) else {
        errors.push(errors::missing_main(file, namespaces));
        return None;
    };
    let mut valid = true;
    if let Some(params) = decl.params() {
        for param in params.attributes() {
            let error = if param.is_named("errors") {
                errors::main_errors(&param, &signature.params["errors"], namespaces)
            } else {
                Some(errors::main_with_params(&params, &param, namespaces))
            };
            if let Some(error) = error {
                errors.push(error);
                valid = false;
            }
        }
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
    Variable,
    /// A constant, which shares the variables' namespace.
    Constant,
    /// A database, which is a variable.
    Database,
    /// A database's relvar.
    Relvar,
    Constraint,
    Heading,
    /// A built-in type.
    Type,
    /// A type of the program's own.
    Defined,
    /// A type's selector, an operator with the type's name.
    Selector,
}

impl Kind {
    pub(crate) fn describe(self) -> &'static str {
        match self {
            Kind::Operator => "an operator",
            Kind::Builtin => "a built-in operator",
            Kind::Variable => "a variable",
            Kind::Constant => "a constant",
            Kind::Database => "a database",
            Kind::Relvar => "a database's relvar",
            Kind::Constraint => "a constraint",
            Kind::Heading => "a heading",
            Kind::Type => "a built-in type",
            Kind::Defined => "a type",
            Kind::Selector => "a type's selector",
        }
    }
}

/// The top-level names, the prelude's and the program's units', each in
/// its namespace: each declaration, with the unit it is in, and where it
/// is. Operators and variables have a namespace each, so a variable can
/// have an operator's name; types and headings have a third, and
/// constraints a fourth. Operators of one name are told apart by their
/// parameters' names, which every call writes, so a name has an operator
/// for each set of them. A name written in a unit means the declaration it
/// sees of it (see [`scope`]), and each declaration is called in the
/// program, and so in the HIR, by its identity: its name, or, if it is
/// ambiguous, its name qualified by its unit's.
pub(crate) struct Namespaces {
    /// The program's units, which each declaration is in one of.
    units: scope::Units,
    /// Each type's declaration in its unit's implementation, which
    /// completes its interface's, by where the interface's starts (see
    /// [`ast::TypeDecl::implementation`]).
    parts: HashMap<usize, TypeDecl>,
    /// User and built-in operators: each name's, with their parameters'
    /// names, in the order they are defined.
    operators: scope::Declared<OperatorName>,
    /// Where each variable is declared.
    variables: scope::Declared<Span>,
    /// Where each constant is declared, first: it shares the variables'
    /// namespace.
    constants: scope::Declared<Span>,
    /// Where each constraint is declared: no name refers to one, so they
    /// have a namespace of their own.
    constraints: scope::Declared<Span>,
    /// Where each database is declared: a database is a variable, so it
    /// shares the variables' namespace.
    databases: scope::Declared<Span>,
    /// Where each heading is declared.
    headings: scope::Declared<Span>,
    /// Where the prelude declares each built-in type, whose name no
    /// heading or other type can have.
    builtin_types: HashMap<String, Span>,
    /// Each heading's attributes, by its identity, unless they have errors,
    /// which are reported: [`named_types::resolve`] fills it in.
    attributes: HashMap<String, hir::Heading>,
    /// Where each of the program's own types is declared.
    types: scope::Declared<Span>,
    /// Each of the program's types, by its identity, with its components,
    /// unless they have errors, which are reported:
    /// [`named_types::resolve`] fills it in.
    defined: HashMap<String, hir::Defined>,
    /// Each of the program's types' possible representations but the one
    /// its values are stored as, in order, whose components have no
    /// errors, by the type's identity: [`named_types::resolve`] fills it in
    /// too.
    converted: HashMap<String, Vec<Converted>>,
    /// Each of the program's union types, all of them dummy types, by its
    /// identity, unless its declaration has errors:
    /// [`named_types::resolve`] fills it in.
    dummies: HashMap<String, hir::Dummy>,
    /// Each type's immediate subtypes, by its identity, a union type's and
    /// a regular type's, in the order they are declared, but those with
    /// errors.
    subtypes: BTreeMap<String, Vec<hir::Type>>,
    /// The kinds, which only the prelude declares, by name, each with the
    /// built-in types of it.
    kinds: HashMap<String, (hir::Kind, BTreeSet<hir::Scalar>)>,
    /// The types of the units that come with the compiler that built-ins'
    /// signatures name, once the types are resolved:
    /// [`builtins::library`] fills it in.
    library: Option<hir::Library>,
    /// Where each name checking has found refers to is declared, as it finds them, for
    /// [`definitions`]: noted wherever a name is resolved, which reaches these through
    /// every checker, so they are noted through a shared reference.
    found: std::cell::RefCell<Vec<Definition>>,
}

/// An operator's definition, as its name's namespace has it.
struct OperatorName {
    kind: Kind,
    /// Where it is defined: its name.
    span: Span,
    /// Its parameters' names, as a call gives their arguments.
    params: BTreeSet<String>,
}

/// A possible representation of a type of the program's own other than
/// the one its values are stored as, which they are converted to and from.
#[derive(Clone)]
pub(crate) struct Converted {
    /// Its declaration: its components, with its own constraints, and its
    /// conversions, which a type its unit's interface declares has in its
    /// declaration in the implementation.
    pub(crate) decl: ast::Possrep,
    /// Its components.
    pub(crate) components: hir::Heading,
    /// Whether it has each conversion once, the right way round, so that
    /// its selector and its conversion from the stored representation are
    /// checked: otherwise that is reported already.
    pub(crate) convertible: bool,
}

impl Namespaces {
    /// Names in the units `units`, with the prelude's `kinds` only, for its
    /// own declarations.
    pub(crate) fn with_kinds(
        units: scope::Units,
        parts: HashMap<usize, TypeDecl>,
        kinds: &[(hir::Kind, BTreeSet<hir::Scalar>)],
    ) -> Self {
        Namespaces {
            units,
            parts,
            operators: scope::Declared::default(),
            variables: scope::Declared::default(),
            constants: scope::Declared::default(),
            constraints: scope::Declared::default(),
            databases: scope::Declared::default(),
            headings: scope::Declared::default(),
            builtin_types: HashMap::new(),
            attributes: HashMap::new(),
            types: scope::Declared::default(),
            defined: HashMap::new(),
            converted: HashMap::new(),
            dummies: HashMap::new(),
            subtypes: BTreeMap::new(),
            kinds: kinds
                .iter()
                .map(|(kind, members)| (kind.name.clone(), (kind.clone(), members.clone())))
                .collect(),
            library: None,
            found: std::cell::RefCell::default(),
        }
    }

    /// Note that the name written at `name` refers to what is declared at `declaration`.
    pub(crate) fn found(&self, name: Span, declaration: Span) {
        self.found
            .borrow_mut()
            .push(Definition { name, declaration });
    }

    /// Note where each unit that a `uses` of `files` names is: the start of its file.
    fn found_units(&self, files: &Files) {
        for (_, file) in &files.units {
            for used in file
                .uses()
                .flat_map(|decl| decl.units().collect::<Vec<_>>())
            {
                let Some(name) = used.name() else {
                    continue;
                };
                if let Some(unit) = self.units.id(&name.text()) {
                    let start = files.tree(unit).span().start;
                    let written = trendy_syntax::span(name.syntax().text_range());
                    self.found(written, Span { start, end: start });
                }
            }
        }
    }

    /// What [`Namespaces::found`] noted, in source order, each name once, as first found:
    /// what is checked more than once is found again.
    fn definitions(&self) -> Vec<Definition> {
        let mut found = self.found.take();
        found.sort_by_key(|definition| definition.name.start);
        found.dedup_by_key(|definition| definition.name);
        found
    }

    /// Where the prelude declares the built-in type `name` names, if it names one.
    pub(crate) fn builtin_type_declared(&self, name: WrittenName) -> Option<Span> {
        self.builtin_type(name)?;
        self.builtin_types.get(name.name).copied()
    }

    /// The types of the units that come with the compiler that built-ins'
    /// signatures name.
    ///
    /// # Panics
    ///
    /// Before the types are resolved, when [`builtins::library`] gives
    /// them.
    pub(crate) fn library(&self) -> &hir::Library {
        self.library
            .as_ref()
            .expect("the library's types are resolved before the operators")
    }

    /// The declaration in its unit's implementation of the type `decl`
    /// declares in the interface, if it has one.
    pub(crate) fn implementation(&self, decl: &TypeDecl) -> Option<&TypeDecl> {
        self.parts.get(&decl.span().start)
    }

    /// The possible representations of the type `decl` declares, each with
    /// where its conversions are (see [`ast::TypeDecl::possreps`]).
    pub(crate) fn possreps(&self, decl: &TypeDecl) -> Vec<ast::Possrep> {
        decl.possreps_with(self.implementation(decl))
    }

    /// The possible representation of the type `decl` declares that its
    /// values are stored as: the one without conversions, which the others
    /// convert to and from.
    pub(crate) fn stored_possrep(&self, decl: &TypeDecl) -> Option<ast::Possrep> {
        self.possreps(decl)
            .into_iter()
            .find(ast::Possrep::is_stored)
    }

    /// Where the components of the possible representation of the type
    /// `decl` declares that its values are stored as are written.
    pub(crate) fn stored(&self, decl: &TypeDecl) -> Option<ast::Representation> {
        self.stored_possrep(decl).map(|p| p.written().clone())
    }

    /// The constraints of the type `decl` declares, which say which values
    /// it has (see [`ast::TypeDecl::constraints`]).
    pub(crate) fn type_constraints(&self, decl: &TypeDecl) -> Vec<ast::Constraint> {
        decl.constraints_of(&self.possreps(decl)).collect()
    }

    /// The program's units.
    pub(crate) fn units(&self) -> &scope::Units {
        &self.units
    }

    /// The unit `at` is in, whose declarations a name written there sees.
    pub(crate) fn unit_at(&self, at: Span) -> hir::FileId {
        self.units.at(at)
    }

    /// The section of its unit `at` is in, which says what a name written there
    /// sees.
    pub(crate) fn section_at(&self, at: Span) -> ast::Section {
        self.viewer(at).section
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
    /// arguments, which can be a type parameter of the caller's. A built-in
    /// type is of the kinds it declares, and a type of the program's own of
    /// those whose requirements it has, as an order.
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
            (hir::Any::Kind(kind), hir::Type::Defined(_) | hir::Type::Array(_)) => {
                kind.requires.iter().all(|&requirement| ty.has(requirement))
            }
            (_, hir::Type::Parameter(p)) => p.any == *any,
            _ => false,
        }
    }

    /// Who sees what is written at `at`: its unit, and its section.
    fn viewer(&self, at: Span) -> scope::Viewer {
        self.units.viewer(at)
    }

    /// The first operator named `name` that what is written at `at` sees:
    /// whether it is built in, and where it is defined.
    pub(crate) fn operator(&self, name: &str, at: Span) -> Option<(Kind, Span)> {
        let (_, first) = self
            .operators
            .each_visible(name, self.viewer(at), &self.units)
            .next()?;
        Some((first.kind, first.span))
    }

    /// The first operator named `name` whose parameters are named
    /// `params` that what is written at `at` sees: whether it is built in,
    /// and where it is defined.
    pub(crate) fn operator_with(
        &self,
        name: &str,
        params: &BTreeSet<String>,
        at: Span,
    ) -> Option<(Kind, Span)> {
        let (_, first) = self
            .operators
            .each_visible(name, self.viewer(at), &self.units)
            .find(|(_, d)| d.params == *params)?;
        Some((first.kind, first.span))
    }

    /// What `name`, written at `at`, means in `namespace`, of the
    /// declarations seen there: what kind of thing it is, and where it is
    /// declared.
    fn seen(&self, name: &str, at: Span, namespace: Namespace) -> scope::Seen<(Kind, Span)> {
        let viewer = self.viewer(at);
        let of = |table: &scope::Declared<Span>, kind: Kind| {
            table
                .candidates(name, viewer, &self.units)
                .map(move |&span| (kind, span))
        };
        match namespace {
            Namespace::Value => of(&self.variables, Kind::Variable)
                .merged(of(&self.constants, Kind::Constant))
                .merged(of(&self.databases, Kind::Database))
                .resolved(),
            Namespace::Type => of(&self.types, Kind::Defined)
                .merged(of(&self.headings, Kind::Heading))
                .resolved(),
        }
    }

    /// What `name` means in the namespace of types and headings: what its
    /// unit sees of its name, or, qualified, the declaration of the unit
    /// its qualifier names, which hides nothing.
    fn seen_type(&self, name: WrittenName) -> scope::Seen<(Kind, Span)> {
        let Some(unit) = name.unit else {
            return self.seen(name.name, name.at, Namespace::Type);
        };
        let viewer = self.viewer(name.at);
        let found = self
            .types
            .seen_in(name.name, unit, viewer, &self.units)
            .map(|&span| (Kind::Defined, span))
            .or_else(|| {
                self.headings
                    .seen_in(name.name, unit, viewer, &self.units)
                    .map(|&span| (Kind::Heading, span))
            });
        match found {
            Some(found) => scope::Seen::One(unit, found),
            None => scope::Seen::Nothing,
        }
    }

    /// What `name` means, if it is a declaration of `kind`'s: its unit, and
    /// where it is declared.
    fn seen_type_as(&self, name: WrittenName, kind: Kind) -> Option<(hir::FileId, Span)> {
        match self.seen_type(name).one()? {
            (unit, (found, span)) if found == kind => Some((unit, span)),
            _ => None,
        }
    }

    /// The name `ident`, qualified by `qualifier` or not, as it is looked
    /// up, or `None` once its qualifier is reported as naming no unit its
    /// unit sees (see [`Namespaces::qualified_unit`]).
    pub(crate) fn written_name<'t>(
        &self,
        ident: &'t SyntaxToken,
        qualifier: Option<&ast::Qualifier>,
        errors: &mut Vec<Diagnostic>,
    ) -> Option<WrittenName<'t>> {
        let unit = match qualifier {
            Some(qualifier) => Some(self.qualified_unit(qualifier, errors)?),
            None => None,
        };
        Some(WrittenName {
            name: ident.text(),
            at: trendy_syntax::span(ident.text_range()),
            unit,
        })
    }

    /// The name `ident`, qualified by `qualifier` or not, as
    /// [`Namespaces::written_name`] finds it, for a lookup that reports
    /// nothing, as the name's qualifier is reported where what it names is
    /// resolved: `None` if the qualifier names no unit its unit sees.
    pub(crate) fn written_name_unreported<'t>(
        &self,
        ident: &'t SyntaxToken,
        qualifier: Option<&ast::Qualifier>,
    ) -> Option<WrittenName<'t>> {
        let unit = match qualifier {
            Some(qualifier) => {
                let viewer = self.viewer(qualifier.span());
                match self.units.qualified(viewer, &qualifier.unit()?.text()) {
                    scope::Qualified::Seen(unit)
                    | scope::Qualified::Own(unit)
                    | scope::Qualified::Aliased(unit, _) => Some(unit),
                    scope::Qualified::Unused(_) | scope::Qualified::Unknown => return None,
                }
            }
            None => None,
        };
        Some(WrittenName {
            name: ident.text(),
            at: trendy_syntax::span(ident.text_range()),
            unit,
        })
    }

    /// The unit `qualifier` names, where it is written: one its unit
    /// sees, or, after reporting that, its own, whose names are never
    /// qualified, or one its section uses by another name (A0075); or `None`
    /// after reporting that it names a unit its section doesn't use (A0075)
    /// or no unit (A0015).
    pub(crate) fn qualified_unit(
        &self,
        qualifier: &ast::Qualifier,
        errors: &mut Vec<Diagnostic>,
    ) -> Option<hir::FileId> {
        let written = qualifier.unit()?;
        let viewer = self.viewer(qualifier.span());
        match self.units.qualified(viewer, &written.text()) {
            scope::Qualified::Seen(unit) => Some(unit),
            scope::Qualified::Own(unit) => {
                errors.push(errors::own_qualifier(qualifier));
                Some(unit)
            }
            scope::Qualified::Aliased(unit, by) => {
                errors.push(errors::aliased_qualifier(&written, &by));
                Some(unit)
            }
            scope::Qualified::Unused(unit) => {
                errors.push(errors::unused_qualifier(
                    &written,
                    self.units.name(unit),
                    viewer.section,
                ));
                None
            }
            scope::Qualified::Unknown => {
                errors.push(errors::unknown_qualifier(
                    &written,
                    &self.units.qualifiers(viewer),
                ));
                None
            }
        }
    }

    /// The names of the types and headings the unit `unit` declares that
    /// what is written at `at` sees, qualifying them by that unit.
    pub(crate) fn type_names_in(&self, unit: hir::FileId, at: Span) -> Vec<&str> {
        let viewer = self.viewer(at);
        let mut names: Vec<&str> = self
            .types
            .names_in(unit, viewer, &self.units)
            .chain(self.headings.names_in(unit, viewer, &self.units))
            .collect();
        names.sort_unstable();
        names
    }

    /// Whether the unit `unit` declares a type or a heading `name` where
    /// what is written at `at` doesn't see it: in its implementation.
    pub(crate) fn hides_type(&self, unit: hir::FileId, name: &str, at: Span) -> bool {
        let viewer = self.viewer(at);
        self.types.hidden_in(name, unit, viewer, &self.units)
            || self.headings.hidden_in(name, unit, viewer, &self.units)
    }

    /// What `name`, written at `at`, means in `namespace`, if it is a
    /// declaration of `kind`'s: its unit, and where it is declared.
    fn seen_as(
        &self,
        name: &str,
        at: Span,
        namespace: Namespace,
        kind: Kind,
    ) -> Option<(hir::FileId, Span)> {
        match self.seen(name, at, namespace).one()? {
            (unit, (found, span)) if found == kind => Some((unit, span)),
            _ => None,
        }
    }

    /// The declarations of `name` that what is written at `at` sees in
    /// `namespace` from several units, if it does, a clash: each with its
    /// unit's name, and where it is declared.
    pub(crate) fn clash(
        &self,
        name: &str,
        at: Span,
        namespace: Namespace,
    ) -> Option<Vec<(hir::FileId, Span)>> {
        match self.seen(name, at, namespace) {
            scope::Seen::Clash(declared) => Some(
                declared
                    .into_iter()
                    .map(|(unit, (_, span))| (unit, span))
                    .collect(),
            ),
            scope::Seen::Nothing | scope::Seen::One(..) => None,
        }
    }

    /// The name a qualifier written at `at` names `unit` by: the one its
    /// section uses it by, or else its own.
    pub(crate) fn qualifier_for(&self, unit: hir::FileId, at: Span) -> &str {
        self.units.used_as(self.viewer(at), unit)
    }

    /// The unit whose interface declares the type or heading `name`, which
    /// an interface, at `at`, doesn't see, but its implementation does,
    /// through a unit only the implementation uses.
    pub(crate) fn used_by_implementation(&self, name: &str, at: Span) -> Option<&str> {
        let viewer = self.viewer(at);
        if viewer.section != ast::Section::Interface
            || !matches!(self.seen(name, at, Namespace::Type), scope::Seen::Nothing)
        {
            return None;
        }
        let implementation = scope::Viewer {
            section: ast::Section::Implementation,
            ..viewer
        };
        let (unit, _) = self
            .types
            .candidates(name, implementation, &self.units)
            .merged(self.headings.candidates(name, implementation, &self.units))
            .resolved()
            .one()?;
        (unit != viewer.unit && unit != self.units.prelude()).then(|| self.units.name(unit))
    }

    /// The names of the units whose interfaces declare `name` as a thing
    /// of `kind`'s, a type, a heading, an operator, or a value, a variable or
    /// a constant, which what is written at `at` doesn't see, but could use,
    /// by their names; and the section it is written in, whose `uses` would
    /// gain one.
    pub(crate) fn exporters(&self, name: &str, at: Span, kind: Kind) -> (Vec<&str>, ast::Section) {
        let viewer = self.viewer(at);
        let units = match kind {
            Kind::Defined => self.types.unseen_exporters(name, viewer, &self.units),
            Kind::Heading => self.headings.unseen_exporters(name, viewer, &self.units),
            Kind::Variable | Kind::Constant => {
                let mut units = self.variables.unseen_exporters(name, viewer, &self.units);
                units.extend(self.constants.unseen_exporters(name, viewer, &self.units));
                units
            }
            _ => self.operators.unseen_exporters(name, viewer, &self.units),
        };
        let mut names: Vec<&str> = units.into_iter().map(|u| self.units.name(u)).collect();
        names.sort_unstable();
        (names, viewer.section)
    }

    /// Where the variable named `name` that what is written at `at` sees is
    /// declared.
    pub(crate) fn variable(&self, name: &str, at: Span) -> Option<Span> {
        self.seen_as(name, at, Namespace::Value, Kind::Variable)
            .map(|(_, span)| span)
    }

    /// Where the constant named `name` that what is written at `at` sees is
    /// declared.
    pub(crate) fn constant(&self, name: &str, at: Span) -> Option<Span> {
        self.seen_as(name, at, Namespace::Value, Kind::Constant)
            .map(|(_, span)| span)
    }

    /// Whether `unit` declares a constant named `name` that what is written
    /// at `at` sees.
    pub(crate) fn has_constant_in(&self, name: &str, unit: hir::FileId, at: Span) -> bool {
        self.constants
            .seen_in(name, unit, self.viewer(at), &self.units)
            .is_some()
    }

    /// The names of the variables and the constants the unit `unit` declares
    /// that what is written at `at` sees, qualifying them by that unit.
    pub(crate) fn value_names_in(&self, unit: hir::FileId, at: Span) -> Vec<&str> {
        let viewer = self.viewer(at);
        let mut names: Vec<&str> = self
            .variables
            .names_in(unit, viewer, &self.units)
            .chain(self.constants.names_in(unit, viewer, &self.units))
            .collect();
        names.sort_unstable();
        names
    }

    /// What a variable, a constant or a database named `name`, declared in
    /// `unit`, is called in the program: they share a namespace.
    pub(crate) fn value_identity(&self, name: &str, unit: hir::FileId) -> String {
        let declaring = self
            .variables
            .declaring(name)
            .map(|(u, _)| u)
            .chain(self.constants.declaring(name).map(|(u, _)| u))
            .chain(self.databases.declaring(name).map(|(u, _)| u));
        let ambiguous = self.units.ambiguous(declaring, unit);
        self.units.identity(name, unit, ambiguous)
    }

    /// Where the database named `name` that what is written at `at` sees is
    /// declared.
    pub(crate) fn database(&self, name: &str, at: Span) -> Option<Span> {
        self.seen_as(name, at, Namespace::Value, Kind::Database)
            .map(|(_, span)| span)
    }

    /// The identity of the database named `name` that what is written at
    /// `at` sees.
    pub(crate) fn database_named(&self, name: &str, at: Span) -> Option<String> {
        let (unit, _) = self.seen_as(name, at, Namespace::Value, Kind::Database)?;
        Some(self.value_identity(name, unit))
    }

    /// The names of the databases what is written at `at` sees.
    pub(crate) fn database_names(&self, at: Span) -> impl Iterator<Item = &str> {
        self.databases.names_visible(self.viewer(at), &self.units)
    }

    /// Whether `ident` is the first declaration of its name in `table`, in
    /// its unit, not a duplicate.
    fn is_first_in(&self, table: &scope::Declared<Span>, ident: &SyntaxToken) -> bool {
        let span = trendy_syntax::span(ident.text_range());
        table.first_in(ident.text(), self.unit_at(span)) == Some(&span)
    }

    /// Whether `ident` is its constraint name's first declaration in its
    /// unit, not a duplicate.
    pub(crate) fn is_first_constraint(&self, ident: &SyntaxToken) -> bool {
        self.is_first_in(&self.constraints, ident)
    }

    /// What the constraint named `name`, declared in `unit`, is called in
    /// the program.
    pub(crate) fn constraint_identity(&self, name: &str, unit: hir::FileId) -> String {
        let declaring = self.constraints.declaring(name).map(|(u, _)| u);
        let ambiguous = self.units.ambiguous(declaring, unit);
        self.units.identity(name, unit, ambiguous)
    }

    /// The heading `name` names: where it is declared, and its attributes,
    /// unless they have errors.
    pub(crate) fn heading(&self, name: WrittenName) -> Option<(Span, Option<&hir::Heading>)> {
        let (unit, span) = self.seen_type_as(name, Kind::Heading)?;
        Some((
            span,
            self.attributes.get(&self.type_identity(name.name, unit)),
        ))
    }

    /// The attributes of the heading whose identity is `identity`, as the
    /// HIR calls it, unless they have errors.
    pub(crate) fn heading_attributes(&self, identity: &str) -> Option<&hir::Heading> {
        self.attributes.get(identity)
    }

    /// The names of the headings what is written at `at` sees.
    pub(crate) fn headings(&self, at: Span) -> impl Iterator<Item = &str> {
        self.headings.names_visible(self.viewer(at), &self.units)
    }

    /// Whether `ident` is its heading name's first declaration in its
    /// unit, not a duplicate.
    pub(crate) fn is_first_heading(&self, ident: &SyntaxToken) -> bool {
        self.is_first_in(&self.headings, ident)
    }

    /// What a type or a heading named `name`, declared in `unit`, is called
    /// in the program: they share a namespace.
    pub(crate) fn type_identity(&self, name: &str, unit: hir::FileId) -> String {
        let declaring = self
            .types
            .declaring(name)
            .map(|(u, _)| u)
            .chain(self.headings.declaring(name).map(|(u, _)| u));
        let ambiguous = self.units.ambiguous(declaring, unit);
        self.units.identity(name, unit, ambiguous)
    }

    /// What the type or heading the prelude declares as `name` is called in
    /// the program: the compiler's code finds the prelude's own so, though
    /// another unit declares one of its name.
    pub(crate) fn prelude_identity(&self, name: &str) -> String {
        self.type_identity(name, self.units.prelude())
    }

    /// What the type or heading the unit `unit`, one that comes with the
    /// compiler, declares as `name` is called in the program, as for
    /// [`Namespaces::prelude_identity`].
    pub(crate) fn library_identity(&self, unit: &str, name: &str) -> String {
        let id = self
            .units
            .id(unit)
            .expect("every program has the units that come with the compiler");
        self.type_identity(name, id)
    }

    /// The prelude's heading `name`, as a fix writes it at `at`: plain, if
    /// it means the prelude's there, and otherwise qualified, as the unit
    /// there hides it by its own, or sees another unit's too.
    pub(crate) fn preludes_heading_written(&self, name: &str, at: Span) -> String {
        if self.heading_named(WrittenName::plain(name, at)) == Some(self.prelude_identity(name)) {
            name.to_string()
        } else {
            format!("{}::{name}", self.qualifier_for(self.units.prelude(), at))
        }
    }

    /// The identity of the heading `name` names.
    pub(crate) fn heading_named(&self, name: WrittenName) -> Option<String> {
        let (unit, _) = self.seen_type_as(name, Kind::Heading)?;
        Some(self.type_identity(name.name, unit))
    }

    /// The identity of the program's own type `name` names.
    pub(crate) fn type_named(&self, name: WrittenName) -> Option<String> {
        let (unit, _) = self.seen_type_as(name, Kind::Defined)?;
        Some(self.type_identity(name.name, unit))
    }

    /// Where the heading or the program's own type whose identity is
    /// `identity` is declared: its first declaration in its unit.
    pub(crate) fn type_declared(&self, identity: &str) -> Option<Span> {
        let written = identity.rsplit("::").next().unwrap_or(identity);
        self.types
            .declaring(written)
            .chain(self.headings.declaring(written))
            .find(|&(unit, _)| self.type_identity(written, unit) == identity)
            .map(|(_, &span)| span)
    }

    /// The built-in type `name` names: the prelude's, plain, as no unit
    /// can declare one of its name, or qualified by the prelude.
    pub(crate) fn builtin_type(&self, name: WrittenName) -> Option<hir::Scalar> {
        if name.unit.is_some_and(|unit| unit != self.units.prelude()) {
            return None;
        }
        hir::Scalar::from_name(name.name)
    }

    /// The program's own type `name` names: where it is declared, and the
    /// type, unless its components have errors.
    pub(crate) fn defined(&self, name: WrittenName) -> Option<(Span, Option<&hir::Defined>)> {
        let (unit, span) = self.seen_type_as(name, Kind::Defined)?;
        Some((span, self.defined.get(&self.type_identity(name.name, unit))))
    }

    /// The program's own type the name `ident` declares, unless its
    /// components have errors: its declaration's, whatever else its unit
    /// sees of that name.
    pub(crate) fn declared_type(&self, ident: &SyntaxToken) -> Option<&hir::Defined> {
        let unit = self.unit_at(trendy_syntax::span(ident.text_range()));
        self.defined.get(&self.type_identity(ident.text(), unit))
    }

    /// Whether the operators of the name `ident` declares, a type's, in its
    /// unit, are its selectors, as no other operator of the unit has its
    /// name.
    pub(crate) fn selects(&self, ident: &SyntaxToken) -> bool {
        let unit = self.unit_at(trendy_syntax::span(ident.text_range()));
        self.operators
            .first_in(ident.text(), unit)
            .is_some_and(|d| d.kind == Kind::Selector)
    }

    /// The program's own type whose identity is `identity`, as the HIR
    /// calls it, unless its components have errors.
    pub(crate) fn defined_named(&self, identity: &str) -> Option<&hir::Defined> {
        self.defined.get(identity)
    }

    /// The program's own union type `name` names, a dummy type, unless it
    /// isn't one, or its declaration has errors.
    pub(crate) fn dummy(&self, name: WrittenName) -> Option<&hir::Dummy> {
        let (unit, _) = self.seen_type_as(name, Kind::Defined)?;
        self.dummies.get(&self.type_identity(name.name, unit))
    }

    /// The union type whose identity is `identity`, as the HIR calls it, a
    /// dummy type, unless its declaration has errors: the prelude's
    /// `error`, among them.
    pub(crate) fn dummy_named(&self, identity: &str) -> Option<&hir::Dummy> {
        self.dummies.get(identity)
    }

    /// The immediate subtypes of the union type whose identity is
    /// `identity`, in the order they are declared, but those with errors.
    pub(crate) fn subtypes(&self, identity: &str) -> &[hir::Type] {
        self.subtypes.get(identity).map_or(&[], Vec::as_slice)
    }

    /// The read-only operators selecting a value of `defined` calls, to
    /// check its components and find its most specific type: its own
    /// checks (see [`hir::Defined::checks`]), and the specialization
    /// constraints of the subtypes defined by constraints that it may be
    /// of, and theirs in turn.
    pub(crate) fn selection_checks(&self, defined: &hir::Defined) -> Vec<hir::OperatorId> {
        let mut checks: Vec<hir::OperatorId> = defined.checks().collect();
        let mut pending: Vec<&str> = vec![&defined.name];
        while let Some(name) = pending.pop() {
            for subtype in self.subtypes(name) {
                if let hir::Type::Defined(subtype) = subtype {
                    checks.extend(subtype.own_constraints().iter().map(|c| c.operator));
                    pending.push(&subtype.name);
                }
            }
        }
        checks
    }

    /// The possible representations of the program's own type whose
    /// identity is `identity` but the one its values are stored as, whose
    /// components have no errors.
    pub(crate) fn converted(&self, identity: &str) -> &[Converted] {
        self.converted.get(identity).map_or(&[], Vec::as_slice)
    }

    /// The names of the program's own types what is written at `at` sees.
    pub(crate) fn defined_types(&self, at: Span) -> impl Iterator<Item = &str> {
        self.types.names_visible(self.viewer(at), &self.units)
    }

    /// Whether `ident` is its type name's first declaration in its unit,
    /// not a duplicate.
    pub(crate) fn is_first_type(&self, ident: &SyntaxToken) -> bool {
        self.is_first_in(&self.types, ident)
    }

    /// The operators what is written at `at` sees: each one's name, whether
    /// it is built in, and where it is defined.
    pub(crate) fn operators(&self, at: Span) -> impl Iterator<Item = (&str, Kind, Span)> {
        self.operators
            .each_named_visible(self.viewer(at), &self.units)
            .map(|(name, d)| (name, d.kind, d.span))
    }

    /// Whether `ident` is its variable name's first declaration in its
    /// unit, not a duplicate.
    pub(crate) fn is_first_variable(&self, ident: &SyntaxToken) -> bool {
        self.is_first_in(&self.variables, ident)
    }

    /// Whether `ident` is its constant name's first declaration in its unit,
    /// not a duplicate, or the implementation's part of one.
    pub(crate) fn is_first_constant(&self, ident: &SyntaxToken) -> bool {
        self.is_first_in(&self.constants, ident)
    }

    /// The first declaration in `unit` of `name` in the namespace a
    /// declaration of `kind`'s is in, or else the built-in type of that
    /// name, whose duplicate a declaration of `unit`'s of that name is:
    /// what kind of thing it is, and where it is declared.
    fn declared_before(&self, name: &str, unit: hir::FileId, kind: Kind) -> Option<(Kind, Span)> {
        let here = |table: &scope::Declared<Span>, kind: Kind| {
            table.each_in(name, unit).next().map(|&span| (kind, span))
        };
        match kind {
            Kind::Variable | Kind::Constant | Kind::Database => {
                here(&self.variables, Kind::Variable)
                    .or_else(|| here(&self.constants, Kind::Constant))
                    .or_else(|| here(&self.databases, Kind::Database))
            }
            Kind::Constraint => here(&self.constraints, Kind::Constraint),
            _ => here(&self.headings, Kind::Heading)
                .or_else(|| here(&self.types, Kind::Defined))
                .or_else(|| self.builtin_types.get(name).map(|&s| (Kind::Type, s))),
        }
    }
}

/// A name a unit writes, of a type, a heading or an operator: where, and,
/// if it is qualified, the unit its qualifier names, as
/// [`Namespaces::written_name`] finds it.
#[derive(Clone, Copy)]
pub(crate) struct WrittenName<'a> {
    pub(crate) name: &'a str,
    pub(crate) at: Span,
    pub(crate) unit: Option<hir::FileId>,
}

impl<'a> WrittenName<'a> {
    /// `name`, written at `at` unqualified.
    pub(crate) fn plain(name: &'a str, at: Span) -> Self {
        WrittenName {
            name,
            at,
            unit: None,
        }
    }
}

/// A namespace that more than one table of declarations shares.
#[derive(Clone, Copy)]
pub(crate) enum Namespace {
    /// The values': variables and databases.
    Value,
    /// The types': the program's own types and headings.
    Type,
}

/// Record every top-level name in its namespace, the prelude's first,
/// reporting each one defined more than once in them.
fn collect_names(
    kinds: &[(hir::Kind, BTreeSet<hir::Scalar>)],
    files: &Files,
    errors: &mut Vec<Diagnostic>,
) -> Namespaces {
    let mut namespaces = Namespaces::with_kinds(files.names.clone(), files.parts.clone(), kinds);
    let mut constants: HashMap<(hir::FileId, String), VarDecl> = HashMap::new();
    for (source, item) in files.items() {
        let prelude = source == Source::Prelude;
        let (kind, name, params) = match item {
            Item::Operator(ref op) => (Kind::Operator, op.name(), Some(op.params())),
            Item::Builtin(ref decl) if source.is_compilers() => {
                (Kind::Builtin, decl.name(), Some(decl.params()))
            }
            Item::BuiltinType(ref decl) if prelude => (Kind::Type, decl.name(), None),
            Item::Variable(ref decl) => (Kind::Variable, decl.name(), None),
            Item::Constant(ref decl) => (Kind::Constant, decl.name(), None),
            Item::Database(ref decl) => (Kind::Database, decl.name(), None),
            Item::Constraint(ref decl) if decl.part_of().is_none() => {
                (Kind::Constraint, decl.name(), None)
            }
            Item::Heading(ref decl) => (Kind::Heading, decl.name(), None),
            Item::Type(ref decl) => (Kind::Defined, decl.name(), None),
            // The prelude's kinds are checked against the compiler, and
            // built-ins outside the units that come with it are reported,
            // as are built-in types outside the prelude; a database's
            // relvars and constraints are named within it
            // (`databases::check`).
            Item::Builtin(_)
            | Item::BuiltinType(_)
            | Item::Kind(_)
            | Item::Relvar(_)
            | Item::Constraint(_) => continue,
        };
        let Some(ident) = name.and_then(|n| n.ident()) else {
            continue;
        };
        let (name, span) = (ident.text(), trendy_syntax::span(ident.text_range()));
        let unit = namespaces.unit_at(span);
        let section = ast::section_of(item.syntax());
        // The implementation's value of what its interface declares, which is
        // its other part.
        if let Item::Constant(decl) = &item {
            let first = (unit, name.to_string());
            if let Some(first) = constants.get(&first)
                && completes_constant(first, decl)
            {
                continue;
            }
            constants
                .entry((unit, name.to_string()))
                .or_insert_with(|| decl.clone());
        }
        if let Some(params) = params {
            let params = param_names(params.as_ref());
            let first = namespaces
                .operators
                .each_in(name, unit)
                .find(|d| d.params == params)
                .map(|d| (d.kind, d.span));
            match first {
                // A built-in has no versions, so this is never called
                // (`operators::signatures`).
                Some(first) if first.0 == Kind::Builtin || kind == Kind::Builtin => {
                    errors.push(errors::duplicate_operator(name, span, first, &params));
                }
                // A version, or a duplicate, which its parameters' types
                // tell, once they are resolved (`operators::signatures`).
                Some(_) => {}
                None => {
                    namespaces.operators.insert(
                        name,
                        unit,
                        section,
                        OperatorName { kind, span, params },
                    );
                }
            }
            continue;
        }
        // A duplicate of a declaration of its unit's: one of another unit's
        // of that name, the prelude's among them, is hidden by it, or
        // clashes with another unit's where a unit that sees both writes
        // it.
        if let Some(first) = namespaces.declared_before(name, unit, kind) {
            let completed = match &item {
                Item::Variable(decl) if section == ast::Section::Implementation => files
                    .variables()
                    .find(|v| {
                        v.name().map(|n| n.span()) == Some(first.1)
                            && ast::section_of(v.syntax()) == ast::Section::Interface
                    })
                    .map(|first| errors::variable_completed(&first, decl)),
                _ => None,
            };
            errors.push(
                completed.unwrap_or_else(|| errors::duplicate_name(name, (kind, span), first)),
            );
            continue;
        }
        match kind {
            Kind::Variable => namespaces.variables.insert(name, unit, section, span),
            Kind::Constant => namespaces.constants.insert(name, unit, section, span),
            Kind::Database => namespaces.databases.insert(name, unit, section, span),
            Kind::Constraint => namespaces.constraints.insert(name, unit, section, span),
            Kind::Defined => namespaces.types.insert(name, unit, section, span),
            Kind::Type => {
                namespaces.builtin_types.insert(name.to_string(), span);
            }
            _ => namespaces.headings.insert(name, unit, section, span),
        }
    }
    selectors(files, &mut namespaces, errors);
    namespaces
}

/// The names of the parameters `params` declares, as a call gives their
/// arguments, or none if it has none.
pub(crate) fn param_names(params: Option<&ast::Heading>) -> BTreeSet<String> {
    params
        .map(|heading| {
            heading
                .attributes()
                .filter_map(|a| Some(a.name()?.ident()?.text().to_string()))
                .collect()
        })
        .unwrap_or_default()
}

/// Record each type's selector, an operator named after it, with its
/// components as its parameters, in `namespaces`: no other operator of its
/// unit can have a type's name, so one that does is reported, and gives
/// way to the selector, as a duplicate does to the first definition.
/// Another unit's operator of its name, the prelude's among them, is
/// hidden by it where it is seen, or overloaded, as any operator of
/// another unit's is.
fn selectors(files: &Files, namespaces: &mut Namespaces, errors: &mut Vec<Diagnostic>) {
    for decl in files.types() {
        let Some(ident) = decl.name().and_then(|n| n.ident()) else {
            continue;
        };
        if !namespaces.is_first_type(&ident) {
            continue;
        }
        let (name, span) = (ident.text(), trendy_syntax::span(ident.text_range()));
        let unit = namespaces.unit_at(span);
        let section = ast::section_of(decl.syntax());
        let others: Vec<Span> = namespaces
            .operators
            .each_in(name, unit)
            .map(|other| other.span)
            .collect();
        for &at in &others {
            errors.push(errors::selector_name(name, span, at));
        }
        namespaces.operators.remove_in(name, unit);
        // A selector for each possible representation.
        for representation in decl.representations() {
            let params = representation
                .components()
                .map(|components| {
                    components
                        .attributes()
                        .filter_map(|a| Some(a.name()?.ident()?.text().to_string()))
                        .collect()
                })
                .unwrap_or_default();
            namespaces.operators.insert(
                name,
                unit,
                section,
                OperatorName {
                    kind: Kind::Selector,
                    span,
                    params,
                },
            );
        }
    }
}

/// The program's variables, each by its first declaration in its unit.
fn variables(files: &Files, namespaces: &Namespaces) -> scope::Variables {
    let first = files.variables().filter(|decl| {
        decl.name()
            .and_then(|n| n.ident())
            .is_some_and(|ident| namespaces.is_first_variable(&ident))
    });
    scope::Variables::new(first, files.constants(), namespaces)
}

/// Whether `decl` gives the value of the constant `first` declares without one,
/// in its unit's interface: the rest of it, in the implementation, so that the
/// interface doesn't say how it is computed.
pub(crate) fn completes_constant(first: &VarDecl, decl: &VarDecl) -> bool {
    first.initializer().is_none()
        && ast::section_of(first.syntax()) == ast::Section::Interface
        && decl.initializer().is_some()
        && ast::section_of(decl.syntax()) == ast::Section::Implementation
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

    thread_local! {
        /// The units that come with the compiler, as
        /// `trendy_prelude::UNITS` lists them, parsed once for each
        /// thread's tests.
        static LIBRARY: Vec<SourceFile> = trendy_prelude::UNITS
            .iter()
            .map(|unit| parse(unit.source))
            .collect();
    }

    /// `f` of the units that come with the compiler, as
    /// `trendy_prelude::UNITS` lists them, with `replaced`, if given, the
    /// tree of the unit it names, in its place.
    fn with_library<R>(
        replaced: Option<(trendy_prelude::Unit, &SourceFile)>,
        f: impl FnOnce(&[File]) -> R,
    ) -> R {
        LIBRARY.with(|trees| {
            let files: Vec<File> = trendy_prelude::UNITS
                .iter()
                .zip(trees)
                .map(|(unit, tree)| File {
                    name: unit.name,
                    path: unit.path,
                    tree: match replaced {
                        Some((other, replacement)) if other == *unit => replacement,
                        _ => tree,
                    },
                })
                .collect();
            f(&files)
        })
    }

    fn analyze_str(source: &str) -> Result<hir::Program, Vec<Diagnostic>> {
        analyze_units(&[("program", source)])
    }

    /// `program` analyzed, as the one unit of a program, with `prelude` as
    /// the prelude.
    fn analyze_with(
        program: &SourceFile,
        prelude: &SourceFile,
    ) -> Result<hir::Program, Vec<Diagnostic>> {
        let unit = File {
            name: "program",
            path: "program.trendy",
            tree: program,
        };
        with_library(Some((trendy_prelude::PRELUDE, prelude)), |library| {
            analyze(&[unit], library)
        })
    }

    /// The program of `units`, each a unit's name and text, main's first,
    /// each unit in a file named after it.
    fn analyze_units(units: &[(&str, &str)]) -> Result<hir::Program, Vec<Diagnostic>> {
        let trees: Vec<SourceFile> = units.iter().map(|&(_, source)| parse(source)).collect();
        let paths: Vec<String> = units
            .iter()
            .map(|(name, _)| format!("{name}.trendy"))
            .collect();
        let files: Vec<File> = units
            .iter()
            .zip(&trees)
            .zip(&paths)
            .map(|((&(name, _), tree), path)| File { name, path, tree })
            .collect();
        with_library(None, |library| analyze(&files, library))
    }

    /// What each name written in the first of `units`, each a unit's name and text, refers
    /// to, as [`definitions`] says: the name's text, and its declaration's, or of a unit's
    /// file, the first line of it.
    fn definitions_in(units: &[(&str, &str)]) -> Vec<(String, String)> {
        let trees: Vec<SourceFile> = units.iter().map(|&(_, source)| parse(source)).collect();
        let paths: Vec<String> = units
            .iter()
            .map(|(name, _)| format!("{name}.trendy"))
            .collect();
        let files: Vec<File> = units
            .iter()
            .zip(&trees)
            .zip(&paths)
            .map(|((&(name, _), tree), path)| File { name, path, tree })
            .collect();
        let found = with_library(None, |library| definitions(&files, library));
        let text: String = units
            .iter()
            .map(|(_, source)| *source)
            .chain(trendy_prelude::UNITS.iter().map(|unit| unit.source))
            .collect();
        found
            .iter()
            .filter(|d| d.name.end <= units[0].1.len())
            .map(|d| {
                let declaration = if d.declaration.start == d.declaration.end {
                    text[d.declaration.start..]
                        .lines()
                        .next()
                        .unwrap_or_default()
                } else {
                    &text[d.declaration.start..d.declaration.end]
                };
                (
                    text[d.name.start..d.name.end].to_string(),
                    declaration.to_string(),
                )
            })
            .collect()
    }

    /// The errors of the units that come with the compiler, checked alone,
    /// with `tree` as `unit`'s.
    fn checked_with(unit: trendy_prelude::Unit, tree: &SourceFile) -> Vec<Diagnostic> {
        with_library(Some((unit, tree)), check_library)
    }

    /// The errors of the units that come with the compiler, checked alone,
    /// with `prelude` as the prelude.
    fn checked_prelude(prelude: &SourceFile) -> Vec<Diagnostic> {
        checked_with(trendy_prelude::PRELUDE, prelude)
    }

    /// Where the prelude's text starts in a program whose only unit is
    /// `source`: after it and the other units that come with the compiler.
    fn prelude_start(source: &str) -> usize {
        let before = trendy_prelude::UNITS
            .iter()
            .take_while(|unit| unit.name != trendy_prelude::PRELUDE.name)
            .map(|unit| unit.source.len());
        source.len() + before.sum::<usize>()
    }

    /// The errors of the program `source`, each where its fix goes, in it,
    /// not in the prelude, which is part of the compiler.
    fn errors(source: &str) -> Vec<Diagnostic> {
        let errors = analyze_str(source).unwrap_err();
        for d in &errors {
            let edits = d.suggestions.iter().flat_map(|s| &s.edits);
            for span in std::iter::once(d.primary.span).chain(edits.map(|e| e.span)) {
                assert!(span.end <= source.len(), "{source}: {d:#?}");
            }
        }
        errors
    }

    /// The prelude's text at `span`, in the program of `source` and the
    /// units that come with the compiler.
    fn in_prelude(source: &str, span: Span) -> &'static str {
        let before = prelude_start(source);
        let start = span.start.checked_sub(before).expect("in the prelude");
        &trendy_prelude::PRELUDE.source[start..span.end - before]
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

    /// The variables of `program`, but those the runtime gives, which only
    /// `trendy.process` declares.
    fn own_variables(program: &hir::Program) -> impl Iterator<Item = &hir::Variable> {
        program.variables.iter().filter(|v| v.given.is_none())
    }

    /// The variables `source` declares.
    fn variables(source: &str) -> Vec<hir::Variable> {
        let program = analyze_str(&format!("{source}{MAIN}")).unwrap();
        own_variables(&program).cloned().collect()
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
        // The built-ins come after the types' constraints, the prelude's
        // first.
        let first = program
            .operators
            .iter()
            .find(|op| matches!(op.kind, hir::OperatorKind::Builtin(_)));
        assert!(matches!(
            first.map(|op| &op.kind),
            Some(hir::OperatorKind::Builtin(hir::Builtin::Update(
                hir::UpdateBuiltin::DebugValue
            )))
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
                    given: None,
                    keys: Vec::new(),
                },
                hir::Variable {
                    name: "t".to_string(),
                    ty: hir::Type::Scalar(hir::Scalar::Text),
                    init: hir::Value::Text("a\tb".to_string()),
                    given: None,
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
        // order.
        let errors = errors(&format!(
            "operator a{{x: integer, y: integer}} [];\noperator a{{y: text, x: text}} [];{MAIN}"
        ));
        assert_eq!(errors.len(), 1, "{errors:#?}");
        assert_eq!(errors[0].code, Code::DuplicateName);
        assert_eq!(
            errors[0].message,
            "`a` is defined more than once with parameters named `x` and `y`"
        );
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
        let source = "var rate := 2;\noperator set{} [ rate := 3; ];\noperator scaled{n: integer}: integer [ n * rate ];\noperator scaled{n: integer, by: integer}: integer [ n * by ];\noperator main{} [\n    call debug_value{v: scaled{n: 1}};\n    call debug_value{v: scaled{n: 1, by: 3}};\n];";
        assert_eq!(
            fix(source),
            "var rate := 2;\noperator set{} [ rate := 3; ];\noperator scaled{n: integer, rate: integer}: integer [ n * rate ];\noperator scaled{n: integer, by: integer}: integer [ n * by ];\noperator main{} [\n    call debug_value{v: scaled{n: 1, rate: rate}};\n    call debug_value{v: scaled{n: 1, by: 3}};\n];"
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
        assert!(matches!(unsited(value.clone()), hir::Expr::Call(..)));
    }

    #[test]
    fn leave_and_iterate_can_name_a_loop_they_are_in() {
        let update = |body: &str| format!("operator f{{n: integer}} [\n{body}\n];{MAIN}");
        // Of a loop around the innermost, which an always true condition of an
        // inner loop can then end; loops side by side can share a name.
        for body in [
            "for x in array [1] loop as rows [ for y in array [2] loop [ if y = n then [ leave rows; ]; ]; ];",
            "loop as retry [ while true loop [ iterate retry; ]; ] while n > 0;",
            "for x in array [1] loop as each [ leave each; ]; for y in array [2] loop as each [ if y = n then [ iterate each; ]; call debug_value{v: y}; ];",
        ] {
            let source = update(body);
            assert!(
                analyze_str(&source).is_ok(),
                "{body}: {:#?}",
                analyze_str(&source).err()
            );
        }
        // A name no loop around has, fixed to one it misspells.
        let source = update("for x in array [1] loop as rows [ leave rowz; ];");
        assert_eq!(codes(&source), [Code::Exit]);
        assert_eq!(
            fix(&source),
            update("for x in array [1] loop as rows [ leave rows; ];")
        );
        // A value block's statements are in no loop of theirs.
        assert_eq!(
            codes(&update(
                "for x in array [1] loop as rows [ var v := [ leave rows; 1 ]; leave rows; ];"
            )),
            [Code::Exit]
        );
        // A loop in another can't share its name.
        assert_eq!(
            codes(&update(
                "for x in array [1] loop as rows [ for y in array [1] loop as rows [ leave rows; ]; leave rows; ];"
            )),
            [Code::DuplicateName]
        );
        // A name nothing names changes nothing, which the fix removes.
        let source = update("for x in array [1] loop as rows [ leave; ];");
        assert_eq!(codes(&source), [Code::Exit]);
        assert_eq!(fix(&source), update("for x in array [1] loop [ leave; ];"));
    }

    #[test]
    fn leave_and_iterate_are_of_the_innermost_loop() {
        let update = |body: &str| format!("operator f{{n: integer}} [\n{body}\n];{MAIN}");
        // In a loop, of each kind, through `if`s and transactions.
        for body in [
            "var i := 0; while i < n loop [ i := i + 1; if i = 3 then [ leave; ]; call debug_value{v: i}; ];",
            "for x in array [1, 2] loop [ transaction [ if x = n then [ iterate; ]; call debug_value{v: x}; ]; ];",
            "var i := 0; loop [ i := i + 1; if i > n then [ leave; ]; ] while true;",
            "while true loop [ if n > 0 then [ return; ]; leave; ];",
        ] {
            let source = update(body);
            assert!(
                analyze_str(&source).is_ok(),
                "{body}: {:#?}",
                analyze_str(&source).err()
            );
        }
        // In none, which a value block's statements aren't in, though the
        // block is.
        assert_eq!(codes(&update("leave;")), [Code::Exit]);
        let source = update("for x in array [1] loop [ var v := [ iterate; 1 ]; ];");
        let found = errors(&source);
        assert_eq!(found[0].code, Code::Exit);
        assert!(found[0].notes[1].contains("value block"), "{found:#?}");
        // One that skips nothing, at its block's end, in an `if` there too.
        for (body, fixed) in [
            (
                "for x in array [1] loop [ call debug_value{v: x}; iterate; ];",
                "for x in array [1] loop [ call debug_value{v: x}; ];",
            ),
            (
                "for x in array [1] loop [ if x = 1 then [ call debug_value{v: x}; iterate; ]; ];",
                "for x in array [1] loop [ if x = 1 then [ call debug_value{v: x}; ]; ];",
            ),
        ] {
            assert_eq!(codes(&update(body)), [Code::Exit], "{body}");
            assert_eq!(fix(&update(body)), update(fixed), "{body}");
        }
        // What follows one never runs, which the fix deletes.
        let body =
            "for x in array [1] loop [ leave; call debug_value{v: x}; call debug_value{v: n}; ];";
        assert_eq!(codes(&update(body)), [Code::Unreachable]);
        assert_eq!(
            fix(&update(body)),
            update("for x in array [1] loop [ leave; ];")
        );
        // An always true condition with no way out, but `return`, never ends.
        assert_eq!(
            codes(&update(
                "while true loop [ for x in array [1] loop [ leave; ]; ];"
            )),
            [Code::PointlessCondition]
        );
        // A block run once that leaves needs its loop, and keeps it.
        assert!(
            analyze_str(&update(
                "loop [ if n = 1 then [ leave; ]; call debug_value{v: n}; ] while false;"
            ))
            .is_ok()
        );
        // An `out` parameter is assigned on every path to a `return`, and
        // what a branch that returns doesn't assign is assigned after it.
        let out =
            |body: &str| format!("operator g{{n: integer, out r: integer}} [ {body} ];{MAIN}");
        assert_eq!(
            codes(&out("if n > 0 then [ return; ]; r := 1;")),
            [Code::Unassigned]
        );
        assert!(analyze_str(&out("if n > 0 then [ r := 0; return; ]; r := 1;")).is_ok());
        assert!(
            analyze_str(&out(
                "if n > 0 then [ return; ] else [ r := 1; ]; r := r + 1;"
            ))
            .is_err()
        );
        assert!(
            analyze_str(&out(
                "if n > 0 then [ r := 0; return; ] else [ r := 1; ]; r := r + 1;"
            ))
            .is_ok()
        );
        // A loop run at least once assigns nothing for sure, if it can leave
        // or go on to its next run first.
        let declared = |body: &str| {
            format!(
                "type positive {{ {{ p: integer, constraint p > 0 }} }};\n{}",
                update(&format!(
                    "var v: positive; {body} call debug_value{{v: v}};"
                ))
            )
        };
        let assigned = "loop [ v := positive{p: 1}; ] while n > 5;";
        assert!(
            analyze_str(&declared(assigned)).is_ok(),
            "{:#?}",
            analyze_str(&declared(assigned)).err()
        );
        for body in [
            "loop [ if n > 0 then [ leave; ]; v := positive{p: 1}; ] while n > 5;",
            "loop [ if n > 0 then [ iterate; ]; v := positive{p: 1}; ] while n > 5;",
        ] {
            assert_eq!(codes(&declared(body)), [Code::Unassigned], "{body}");
        }
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
        // An early return gives its value, wherever it is, and its body still
        // ends with one.
        let early = "operator f{x: integer}: integer [ if x > 0 then [ return x; ]; 1 ];";
        assert!(analyze_str(&format!("{early}{MAIN}")).is_ok());
        assert_eq!(
            messages(&format!(
                "operator f{{x: integer}}: integer [ if x > 0 then [ return; ]; 1 ];{MAIN}"
            )),
            ["a `return` in the read-only operator `f` gives its value"]
        );
        assert_eq!(
            codes(&format!(
                "operator f{{x: integer}}: integer [ if x > 0 then [ return \"x\"; ]; 1 ];{MAIN}"
            )),
            [Code::TypeMismatch]
        );
        // An update operator's has no value, and one at its body's end skips
        // nothing.
        assert_eq!(
            codes("operator f{} [ return 1; ];\noperator main{} [];"),
            [Code::Return]
        );
        assert!(analyze_str("operator f{n: integer} [ if n > 0 then [ return; ]; call debug_value{v: n}; ];\noperator main{} [];").is_ok());
        let source = "operator f{} [ call debug_value{v: 1}; return; ];\noperator main{} [];";
        assert_eq!(codes(source), [Code::Exit]);
        assert_eq!(
            fix(source),
            "operator f{} [ call debug_value{v: 1}; ];\noperator main{} [];"
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
        // So is a name the rest of a value follows.
        for (written, fixed) in [
            ("double{n + 1}", "double{x: n + 1}"),
            (
                "count{poets where born < 1500}",
                "count{r: poets where born < 1500}",
            ),
            ("count{poets{name}}", "count{r: poets{name}}"),
        ] {
            let source = program(&format!("call debug_value{{v: {written}}};"));
            assert_eq!(codes(&source), [Code::PositionalArgument], "{written}");
            assert!(fix(&source).contains(fixed), "{written}");
        }
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
             operator bump{} [ vat := 17; dishes := relation {}; ];\n\
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
             operator bump{} [ vat := 17; dishes := relation {}; ];\n\
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

    /// Whether a call of each operator can fail, whatever its arguments are,
    /// is worked out from what it does: a built-in by its own rule, and one of
    /// the program's from its value, its versions', the constraints on its
    /// parameters, and whether it calls itself.
    #[test]
    fn calls_can_fail_as_their_operators_can() {
        let program = analyze_str(
            "type pair { { a: integer, b: integer } };
type even_pair is pair { constraint a mod 2 = 0 };
operator first{p: pair}: integer [ p.a ];
operator half{n: integer}: integer [ n div 2 ];
operator within{n: integer, size: integer}: boolean [ n >= 1 and n <= size ];
operator shown{n: integer}: text [ to_text{from: n} ];
operator positive{n: integer, constraint n > 0}: integer [ n ];
operator flips{b: boolean}: boolean [ if b then not flips{b: b} else false ];
operator side{p: pair}: integer [ 0 ];
operator side{p: even_pair}: integer [ p.b ];
operator other_side{p: pair}: integer [ 0 ];
operator other_side{p: even_pair}: integer [ p.b div 2 ];
operator main{} [];",
        )
        .unwrap();
        // The operator itself, not an interface's declaration of it, which
        // nothing calls.
        let can_fail = |name: &str| {
            let declared = |o: &hir::Operator| {
                matches!(o.kind, hir::OperatorKind::Specification { .. }) && o.versions.is_empty()
            };
            program
                .operators
                .iter()
                .find(|o| o.name == name && !declared(o))
                .unwrap_or_else(|| panic!("no `{name}`"))
                .can_fail
        };
        for name in [
            "count",
            "length",
            "is_empty",
            "to_approximate",
            "sin",
            "atan2",
        ] {
            assert!(!can_fail(name), "`{name}` can't fail");
        }
        for name in ["min", "sum", "to_text", "sqrt", "parse_integer", "some"] {
            assert!(can_fail(name), "`{name}` can fail");
        }
        for name in ["first", "within", "side"] {
            assert!(!can_fail(name), "`{name}` can't fail");
        }
        // Its value divides, makes a text, checks its parameter, calls itself,
        // or has a version that divides.
        for name in ["half", "shown", "positive", "flips", "other_side"] {
            assert!(can_fail(name), "`{name}` can fail");
        }
    }

    #[test]
    fn read_only_operators_can_call_themselves() {
        for source in [
            "operator f{}: integer [ g{} ];\noperator g{}: integer [ f{} ];\noperator main{} [];",
            "operator f{x: integer}: integer [ f{x: x} ];\noperator main{} [];",
            "operator f{x: integer}: integer [ if x = 0 then 0 else f{x: x - 1} ];\noperator main{} [];",
        ] {
            assert!(analyze_str(source).is_ok(), "{:#?}", errors(source));
        }
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
            vars[0].ty.default_value().as_ref(),
            Some(&vars[0].init),
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
        // Of set operators, from another operand, or else the
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
    fn a_types_relation_valued_components_have_keys() {
        let program = analyze_str(&format!(
            "type outline {{
    {{ vertices: relation {{ v_no: integer, x: integer }} key {{ v_no }} key {{ x }} }},
}};
var square := outline{{vertices: relation {{ {{ v_no: 1, x: 0 }}, {{ v_no: 2, x: 1 }} }}}};
var extracted := square.vertices union relation {{ {{ v_no: 1, x: 5 }} }};{MAIN}"
        ))
        .unwrap();
        let hir::Type::Defined(outline) = &program.variables[0].ty else {
            panic!("{:?}", program.variables[0].ty);
        };
        let key = |names: &[&str]| -> hir::Key { names.iter().map(ToString::to_string).collect() };
        assert_eq!(
            outline.keys,
            [("vertices".to_string(), vec![key(&["v_no"]), key(&["x"])])].into()
        );
        // An extracted value has no key: only what declares one does.
        assert_eq!(program.variables[1].keys, [key(&["v_no", "x"])]);
        // A key is checked as a variable's is.
        for (component, message) in [
            (
                "{ n: integer key { n } }",
                "the component `n` isn't a relation, so it can't have a key",
            ),
            (
                "{ vs: relation { a: integer, b: integer } key { a } key { a, b } }",
                "key `{ a, b }` contains key `{ a }`, so it isn't a key",
            ),
            (
                "{ vs: relation { a: integer } key { c } }",
                "unknown attribute `c`",
            ),
        ] {
            let source = format!("type t {{ {component} }};{MAIN}");
            assert_eq!(messages(&source), [message], "{source}");
        }
        // A key among what a type says of itself is moved after the one
        // component it can be a key of.
        let misplaced = format!(
            "type outline {{\n    {{ vertices: relation {{ v_no: integer }} }},\n    key {{ v_no }},\n}};{MAIN}"
        );
        assert_eq!(codes(&misplaced), [Code::MisplacedKey]);
        assert_eq!(
            fix(&misplaced),
            format!(
                "type outline {{\n    {{ vertices: relation {{ v_no: integer }} key {{ v_no }} }},\n}};{MAIN}"
            )
        );
        let inside = format!(
            "type outline {{ {{ vertices: relation {{ v_no: integer }}, key {{ v_no }} }} }};{MAIN}"
        );
        assert_eq!(
            fix(&inside),
            format!(
                "type outline {{ {{ vertices: relation {{ v_no: integer }} key {{ v_no }} }} }};{MAIN}"
            )
        );
        let several = errors(&format!(
            "type t {{ {{ a: relation {{ v: integer }}, b: relation {{ v: integer }} }}, key {{ v }} }};{MAIN}"
        ));
        assert_eq!(
            several[0].help,
            ["write it after the type of the component it is a rule of, `a` or `b`"]
        );
        assert!(several[0].suggestions.is_empty());
    }

    #[test]
    fn selecting_a_value_checks_its_components_keys() {
        // When compiling, as a constraint is, naming the two tuples.
        let found = errors(&format!(
            "type outline {{ {{ vertices: relation {{ v_no: integer, x: integer }} key {{ v_no }} }} }};
var bad := outline{{vertices: relation {{ {{ v_no: 1, x: 0 }}, {{ v_no: 1, x: 5 }} }}}};{MAIN}"
        ));
        assert_eq!(found.len(), 1, "{found:#?}");
        assert_eq!(found[0].code, Code::KeyViolated);
        assert_eq!(
            found[0].message,
            "two tuples of an `outline`'s `vertices` have the same values for its key { v_no }"
        );
        assert_eq!(
            found[0].primary.message,
            "its `vertices` has two tuples with `v_no: 1`"
        );
        assert!(
            found[0].notes.contains(
                &"the two tuples are `tuple { v_no: 1, x: 0 }` and `tuple { v_no: 1, x: 5 }`"
                    .to_string()
            )
        );
        // By `update`, and of another representation's component, which its
        // conversion computes.
        let updated = codes(&format!(
            "type outline {{ {{ vertices: relation {{ v_no: integer, x: integer }} key {{ v_no }} }} }};
var o := outline{{vertices: relation {{ {{ v_no: 1, x: 0 }} }}}};
var bad := [ var p := o; update p {{ vertices := vertices union relation {{ {{ v_no: 1, x: 5 }} }} }}; p ];{MAIN}"
        ));
        assert_eq!(updated, [Code::KeyViolated]);
        let converted = errors(&format!(
            "type seg {{
    {{ len: integer }},
    {{ ends: relation {{ n: integer, x: integer }} key {{ x }} }}
        to {{ len := max{{r: ends, of: x}} - min{{r: ends, of: x}} }}
        from {{ ends := relation {{ {{ n: 1, x: 0 }}, {{ n: 2, x: len }} }} }},
}};
var one := seg{{len: 3}};
var zero := seg{{len: 0}};{MAIN}"
        ));
        assert_eq!(converted.len(), 1, "{converted:#?}");
        assert_eq!(
            converted[0].message,
            "two tuples of a `seg`'s `ends` have the same values for its key { x }"
        );
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
        // Naming the key, as the program would when running.
        assert_eq!(
            errors[0].message,
            "two tuples of `r` have the same values for its key { b }"
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
        let two = errors(&format!(
            "var r := relation {{ {{ a: 1 }}, {{ a: 2 }} }} key {{}};{MAIN}"
        ));
        assert_eq!(two[0].code, Code::KeyViolated);
        assert_eq!(
            two[0].message,
            "`r` has two tuples, but its key {} allows at most one"
        );
    }

    #[test]
    fn the_prelude_is_valid() {
        assert_eq!(
            checked_prelude(&parse(trendy_prelude::PRELUDE.source)),
            Vec::new()
        );
        // Its built-ins are a program's operators.
        let program = analyze_str("operator main{} [];").unwrap();
        let debug_value = program
            .operators
            .iter()
            .find(|op| op.name == "debug_value")
            .unwrap();
        assert_eq!(
            debug_value.params,
            hir::Builtin::Update(hir::UpdateBuiltin::DebugValue).params(&program.library)
        );
    }

    #[test]
    fn a_program_extends_the_prelude() {
        // Every unit sees its interface, but not its implementation.
        let hidden = parse(&format!(
            "{}operator twice{{n: integer}}: integer [ n * 2 ];\n",
            trendy_prelude::PRELUDE.source
        ));
        assert_eq!(checked_prelude(&hidden), Vec::new());
        let calls = parse("var a := twice{n: 2};\noperator main{} [];");
        let unseen = analyze_with(&calls, &hidden).unwrap_err();
        assert_eq!(unseen.len(), 1, "{unseen:#?}");
        assert_eq!(unseen[0].code, Code::UnknownName);
        // Its types and the specification in its interface, and the
        // versions in its implementation.
        let prelude = trendy_prelude::PRELUDE.source.replace(
            "\nimplementation\n",
            "\ntype shape union;\ntype square is shape { { side: integer } };\ntype disc is shape { { radius: integer } };\noperator area{s: shape}: integer;\n\nimplementation\n\noperator area{s: square}: integer [ s.side * s.side ];\noperator area{s: disc}: integer [ 3 * s.radius * s.radius ];\n",
        );
        let prelude = parse(&prelude);
        assert_eq!(checked_prelude(&prelude), Vec::new());
        let strip = "type strip is shape { { length: integer, width: integer } };\n";
        let version = "operator area{s: strip}: integer [ s.length * s.width ];\n";
        let program = format!(
            "{strip}{version}var a := area{{s: strip{{length: 2, width: 3}}}};\nvar b := area{{s: square{{side: 2}}}};\noperator main{{}} [];"
        );
        let program = analyze_with(&parse(&program), &prelude).unwrap();
        let values: Vec<_> = own_variables(&program).map(|v| v.init.clone()).collect();
        assert_eq!(values, [hir::Value::Integer(6), hir::Value::Integer(4)]);
        // Without its version, the program's type is reported, at it.
        let source = format!("{strip}operator main{{}} [];");
        let errors = analyze_with(&parse(&source), &prelude).unwrap_err();
        assert_eq!(errors.len(), 1, "{errors:#?}");
        assert_eq!(errors[0].code, Code::Versions);
        let at = |span: Span| &source[span.start..span.end];
        assert_eq!(at(errors[0].primary.span), "strip");
        let declared = errors[0].secondary[0].span;
        let prelude = prelude.syntax().text().to_string();
        let before = prelude_start(&source);
        assert_eq!(
            &prelude[declared.start - before..declared.end - before],
            "area"
        );
    }

    /// A program's own error, and an operator that fails with it.
    const NO_CONVERSION: &str = "type no_conversion is error { { unit: text } };\noperator message{error: no_conversion}: text [ \"no conversion from \" || error.unit ];\noperator grams{unit: text}: approximate [ if unit = \"kg\" then 1000e0 else fail no_conversion{unit: unit} ];\n";

    #[test]
    fn a_fail_fails_when_it_is_computed() {
        // Not computed, as a branch not taken.
        let vars = variables(&format!("{NO_CONVERSION}var kg := grams{{unit: \"kg\"}};"));
        assert_eq!(
            vars[0].init,
            hir::Value::Approximate(hir::Approximate::new(1000.0))
        );
        // Computed when compiling: reported then, with its message.
        let errors = errors(&format!(
            "{NO_CONVERSION}var pinch := grams{{unit: \"pinch\"}};{MAIN}"
        ));
        assert_eq!(errors.len(), 1, "{errors:#?}");
        assert_eq!(errors[0].code, Code::Failed);
        assert_eq!(errors[0].message, "no conversion from pinch");
        assert!(errors[0].notes[1].contains("computing `grams`'s value"));
        let errors = self::errors(&format!(
            "var direct: integer := fail failure{{message: \"direct\"}};{MAIN}"
        ));
        assert_eq!(errors[0].code, Code::Failed);
        assert_eq!(errors[0].message, "direct");
        // Its message as the program would write it, so that it can't
        // change the terminal, or show what it doesn't say.
        let errors = self::errors(&format!(
            "var direct: integer := fail failure{{message: \"a\\u{{1B}}[2J\\u{{202E}}b\"}};{MAIN}"
        ));
        assert_eq!(errors[0].message, "a\\u{1B}[2J\\u{202E}b");
        // When running, as a statement always is.
        analyze_str(&format!(
            "{NO_CONVERSION}operator weigh{{unit: text}} [ call debug_value{{v: grams{{unit: unit}}}}; ];\noperator main{{}} [ call weigh{{unit: \"pinch\"}}; fail failure{{message: \"done\"}}; ];"
        ))
        .unwrap();
        let body = main_body("operator main{} [ fail failure{message: \"done\"}; ];");
        assert!(matches!(body[0].1, hir::Statement::Fail(_)), "{body:#?}");
    }

    #[test]
    fn a_fail_has_the_type_expected_where_it_is() {
        // An `if`'s other branch's.
        let vars = variables(
            "var n := 3;\nvar a := if n > 0 then 1 else fail failure{message: \"no\"};\nvar b := if n < 0 then fail failure{message: \"no\"} else \"yes\";",
        );
        assert_eq!(vars[1].init, hir::Value::Integer(1));
        assert_eq!(vars[2].init, hir::Value::Text("yes".to_string()));
        // None.
        for source in [
            "var a := fail failure{message: \"no\"};",
            "operator f{x: boolean}: integer [ var y := if x then fail failure{message: \"a\"} else fail failure{message: \"b\"}; 1 ];",
        ] {
            let codes = codes(&format!("{source}{MAIN}"));
            assert!(
                !codes.is_empty() && codes.iter().all(|c| *c == Code::UnknownEmptyType),
                "{source}: {codes:?}"
            );
        }
    }

    #[test]
    fn a_fail_fails_with_an_error() {
        // A text is the prelude's `failure`'s message.
        let source = "operator main{} [ fail \"empty\"; ];";
        assert_eq!(codes(source), [Code::TypeMismatch]);
        assert_eq!(
            fix(source),
            "operator main{} [ fail failure{message: \"empty\"}; ];"
        );
        let errors = errors("operator main{} [ fail 7; ];");
        assert_eq!(errors[0].code, Code::TypeMismatch);
        assert!(errors[0].suggestions.is_empty());
        assert!(errors[0].help[0].contains("failure{message:"));
        // One in a value block runs when the program does, and the block's value
        // after it is never computed.
        assert_eq!(
            codes(&format!(
                "var a: integer := [ var n := 1; if n > 0 then [ fail failure{{message: \"no\"}}; ]; n ];{MAIN}"
            )),
            [Code::InitialValueWhenRunning]
        );
        assert_eq!(
            codes(&format!(
                "var a: integer := [ fail failure{{message: \"no\"}}; 1 ];{MAIN}"
            )),
            [Code::Unreachable]
        );
    }

    #[test]
    fn a_warn_warns_with_an_error_in_an_update_operator() {
        let body = main_body(
            "operator main{} [ transaction [ warn failure{message: \"low\"}; ]; warn failure{message: \"low\"}; ];",
        );
        let [
            (_, hir::Statement::Transaction(None, block)),
            (_, hir::Statement::Warn(_)),
        ] = &body[..]
        else {
            panic!("{body:#?}");
        };
        assert!(matches!(
            block.statements[..],
            [(_, hir::Statement::Warn(_))]
        ));
        // An error, as a `fail`'s is, and a text the prelude's `failure`'s
        // message.
        let source = "operator main{} [ warn \"low\"; ];";
        let errors = errors(source);
        assert_eq!(errors[0].code, Code::TypeMismatch);
        assert!(errors[0].message.starts_with("a program warns"));
        assert!(errors[0].notes[0].starts_with("`warn e`"));
        assert_eq!(
            fix(source),
            "operator main{} [ warn failure{message: \"low\"}; ];"
        );
        let errors = self::errors("operator main{} [ warn 7; ];");
        assert!(errors[0].suggestions.is_empty());
        assert!(errors[0].help[0].contains("`warn failure{message:"));
        // Not in a read-only operator, nor a value block, which compute a
        // value.
        for source in [
            "operator f{}: integer [ warn failure{message: \"no\"}; 1 ];",
            "var a: integer := [ warn failure{message: \"no\"}; 1 ];",
        ] {
            let errors = self::errors(&format!("{source}{MAIN}"));
            assert_eq!(errors.len(), 1, "{source}: {errors:#?}");
            assert_eq!(errors[0].code, Code::Return, "{source}");
            assert!(errors[0].message.ends_with("can't warn"), "{source}");
        }
    }

    #[test]
    fn transactions_add_their_errors_to_a_variable() {
        // `collecting` names a variable declared before, a relation of
        // errors.
        let source = "operator main{} [
    var found: relation transaction_collection;
    transaction collecting found [
        call debug_value{v: 1};
    ];
    call debug_value{v: found};
];";
        let program = analyze_str(source).unwrap();
        let hir::OperatorKind::Update { locals, body } = &program.operator(program.entry).kind
        else {
            panic!("main is an update operator");
        };
        let [
            _,
            (_, hir::Statement::Transaction(Some(hir::Target::Local(found)), block)),
            _,
        ] = &body.statements[..]
        else {
            panic!("{:#?}", body.statements);
        };
        assert_eq!(block.statements.len(), 1);
        assert_eq!(locals[found.0].name, "found");
        // One not declared, which the fix declares, or of another type; a
        // transaction where nothing changes.
        let undeclared = "operator main{} [\n    transaction collecting found [];\n];";
        assert_eq!(codes(undeclared), [Code::UnknownName]);
        assert_eq!(
            fix(undeclared),
            "operator main{} [\n    var found: relation transaction_collection;\n    transaction collecting found [];\n];"
        );
        for (source, code) in [
            (
                "operator main{} [ var n := 1; transaction collecting n []; ];",
                Code::TypeMismatch,
            ),
            (
                "operator main{} [ var e: relation { error: error }; transaction collecting e []; ];",
                Code::TypeMismatch,
            ),
            (
                "var a: integer := [ transaction [ var b := 1; ]; 1 ];\noperator main{} [];",
                Code::InitialValueWhenRunning,
            ),
        ] {
            assert_eq!(codes(source), [code], "{source}");
        }
        // A failure a transaction collects undoes what its block assigned,
        // an `out` parameter too; one it doesn't goes on. Adding to an
        // `out` parameter reads it.
        let out = |transaction: &str| {
            format!(
                "operator f{{out n: integer}} [ var e: relation transaction_collection; {transaction} [ n := 1; ]; ];\noperator main{{}} [ var n := 0; call f{{out n: n}}; ];"
            )
        };
        assert_eq!(codes(&out("transaction collecting e")), [Code::Unassigned]);
        assert!(analyze_str(&out("transaction")).is_ok());
        let read = "operator f{out e: relation transaction_collection} [ transaction collecting e []; ];\noperator main{} [ var e: relation transaction_collection; call f{out e: e}; ];";
        assert_eq!(codes(read), [Code::Unassigned]);
    }

    /// A program with the units of dates and UUIDs, that says what `main` does.
    fn timed(operators: &str, main: &str) -> String {
        format!(
            "uses {{ trendy.time, trendy.uuid }};
{operators}
operator main{{}} [ var at: timestamp; var id: uuid_v7; var four: uuid_v4; {main} ];
"
        )
    }

    #[test]
    fn the_clock_is_read_outside_a_transaction() {
        let stamp = "operator stamp{out at: timestamp} [ call now{out at}; ];";
        assert!(analyze_str(&timed(stamp, "call now{out at}; call stamp{out at};")).is_ok());
        for (main, message) in [
            (
                "transaction [ call now{out at}; ];",
                "`now` reads the clock, in a transaction",
            ),
            (
                "transaction [ call stamp{out at}; ];",
                "`stamp` reads the clock, in a transaction, as `stamp` calls `now`",
            ),
            (
                "transaction [ transaction [ call now{out at}; ]; ];",
                "`now` reads the clock, in a transaction",
            ),
            (
                "transaction [ if at.date.days = 0 then [ call now{out at}; ]; ];",
                "`now` reads the clock, in a transaction",
            ),
            // Without an instant, a version 7 UUID's bits read it.
            (
                "transaction [ call generate{out v7: id}; ];",
                "`generate` reads the clock, in a transaction",
            ),
        ] {
            let found = errors(&timed(stamp, &format!("call now{{out at}}; {main}")));
            assert_eq!(found.len(), 1, "{main}: {found:#?}");
            assert_eq!(found[0].code, Code::IoInTransaction, "{main}");
            assert_eq!(found[0].message, message, "{main}");
            assert!(
                found[0].help[0].contains("`call transaction_start{out at};`"),
                "{main}"
            );
        }
        // A read-only operator reads nothing, a transaction of its own or not.
        let read_only = "operator f{n: integer}: integer [ var at := 1; transaction [ at := n; ]; call now{out at}; at ];";
        assert!(codes(&timed(read_only, "")).contains(&Code::ReadOnlyEffect));
    }

    #[test]
    fn a_transactions_start_is_read_in_a_transaction() {
        let stamp = "operator stamp{out at: timestamp} [ call transaction_start{out at}; ];
operator through{out at: timestamp} [ call stamp{out at}; ];
operator begun{out at: timestamp} [ transaction [ call stamp{out at}; ]; ];
operator unused{out at: timestamp} [ call stamp{out at}; ];";
        for good in [
            "",
            "transaction [ call transaction_start{out at}; ];",
            "transaction [ call stamp{out at}; ];",
            "transaction [ call through{out at}; ];",
            "transaction [ transaction [ call through{out at}; ]; ];",
            "transaction [ if at.date.days = 0 then [ call stamp{out at}; ]; ];",
            // What begins one itself can be called with none running.
            "call begun{out at};",
            "transaction [ call begun{out at}; ];",
        ] {
            let source = timed(stamp, &format!("call now{{out at}}; {good}"));
            assert!(analyze_str(&source).is_ok(), "{good}");
        }
        for (main, message, fixes) in [
            (
                "call transaction_start{out at};",
                "`transaction_start` reads when a transaction began, and this call is in none",
                true,
            ),
            (
                "if at.date.days = 0 then [ call transaction_start{out at}; ];",
                "`transaction_start` reads when a transaction began, and this call is in none",
                true,
            ),
            (
                "call stamp{out at};",
                "`stamp` reads when a transaction began, as `stamp` calls `transaction_start`, and this call is in none",
                false,
            ),
            (
                "call through{out at};",
                "`through` reads when a transaction began, as `stamp` calls `transaction_start`, and this call is in none",
                false,
            ),
            // In a transaction too, what is called in none is reported.
            (
                "transaction [ call through{out at}; ]; call through{out at};",
                "`through` reads when a transaction began, as `stamp` calls `transaction_start`, and this call is in none",
                false,
            ),
        ] {
            let source = timed(stamp, &format!("call now{{out at}}; {main}"));
            let found = errors(&source);
            assert_eq!(found.len(), 1, "{main}: {found:#?}");
            assert_eq!(found[0].code, Code::OutsideAnyTransaction, "{main}");
            assert_eq!(found[0].message, message, "{main}");
            if fixes {
                let fixed = fix(&source);
                assert!(fixed.contains("call now{out at}; "), "{fixed}");
                assert!(analyze_str(&fixed).is_ok(), "{fixed}");
            } else {
                assert!(found[0].suggestions.is_empty(), "{main}");
                assert!(found[0].help[0].contains("`transaction [ … ];`"), "{main}");
            }
        }
        // Through a specification's version, whichever it is that reads it.
        let versions = "type shape union;
type circle is shape { { r: integer } };
type square is shape { { side: integer } };
operator stamp{s: shape, out at: timestamp};
operator stamp{s: circle, out at: timestamp} [ call transaction_start{out at}; ];
operator stamp{s: square, out at: timestamp} [ at := timestamp{date: date{days: s.side}, time: time{nanos_of_day: 0}}; ];";
        let call = "call stamp{s: circle{r: 1}, out at};";
        let outside = timed(versions, &format!("call now{{out at}}; {call}"));
        assert_eq!(codes(&outside), [Code::OutsideAnyTransaction]);
        let inside = timed(
            versions,
            &format!("call now{{out at}}; transaction [ {call} ];"),
        );
        assert!(analyze_str(&inside).is_ok());
        // A read-only operator can't read it.
        let read_only =
            "operator f{n: integer}: integer [ var at := n; call transaction_start{out at}; at ];";
        assert!(codes(&timed(read_only, "")).contains(&Code::ReadOnlyEffect));
    }

    /// Which version a new UUID is the call says, whatever the variable is of.
    #[test]
    fn a_new_uuid_is_of_the_version_the_call_names() {
        let main = |call: &str| {
            format!("uses {{ trendy.uuid }};\noperator main{{}} [ var id: uuid; {call} ];")
        };
        for call in ["call generate{out v4: id};", "call generate{out v7: id};"] {
            assert!(analyze_str(&main(call)).is_ok(), "{call}");
        }
        // The one that named no version is fixed to the first trendy had.
        let source = main("call generate{out uuid: id};");
        assert_eq!(codes(&source), [Code::UnknownAttribute]);
        let fixed = fix(&source);
        assert_eq!(fixed, main("call generate{out v4: id};"));
    }

    #[test]
    fn a_uuid_is_generated_in_a_transaction_from_what_it_is_given() {
        let good = "transaction [
            var started: timestamp;
            call transaction_start{out at: started};
            call generate{out v4: four};
            call generate{at: started, out v7: id};
        ];";
        assert!(analyze_str(&timed("", good)).is_ok());
        // 48 bits of milliseconds reach so far, from 1970.
        for days in ["-1", "3_257_812"] {
            let main = format!(
                "call generate{{at: timestamp{{date: date{{days: {days}}}, time: time{{nanos_of_day: 0}}}}, out v7: id}};"
            );
            assert_eq!(
                codes(&timed("", &main)),
                [Code::BrokenCallConstraint],
                "{days}"
            );
        }
    }

    #[test]
    fn other_languages_ways_of_reading_the_clock_are_explained() {
        for written in [
            "var a := current_timestamp;",
            "var a := CURRENT_TIMESTAMP;",
            "var a := current_date;",
            "var a := getdate{};",
            "var a := clock_timestamp{};",
        ] {
            let found = errors(&format!("uses {{ trendy.time }};\n{written}{MAIN}"));
            assert_eq!(found[0].code, Code::UnknownName, "{written}");
            assert!(
                found[0].help[0].contains("`var at: timestamp; call now{out at};`"),
                "{written}"
            );
        }
        let found = errors(&format!("var a := transaction_timestamp;{MAIN}"));
        assert!(
            found[0].help[0].contains("call transaction_start{out at};"),
            "{found:#?}"
        );
        // Written as a value, and with the unit not used, each says what to do.
        let found = errors("uses { trendy.time };\noperator main{} [ var a := now{}; ];");
        assert_eq!(found[0].code, Code::OperatorAsValue, "{found:#?}");
        let found = errors("operator main{} [ var a := 1; call now{out at: a}; ];");
        assert_eq!(found[0].code, Code::UnknownName, "{found:#?}");
        assert!(found[0].notes[0].contains("`trendy.time`"), "{found:#?}");
        // A mistyped call is one error, whatever is read after it.
        let found = errors(
            "uses { trendy.time };\noperator main{} [ var a: timestamp; call now{out a}; call debug_value{v: a}; ];",
        );
        assert_eq!(found.len(), 1, "{found:#?}");
        assert_eq!(found[0].code, Code::PositionalArgument, "{found:#?}");
    }

    #[test]
    fn transactions_read_and_write_nothing() {
        let program = |main: &str| {
            format!(
                "uses {{ trendy.files }};

operator read{{out lines: array text}} [ call load_text_file{{path: file_path{{text: \"notes.txt\"}}, out lines: lines}}; ];
operator twice{{out lines: array text}} [ call read{{out lines: lines}}; ];
operator main{{}} [ var lines: array text; {main} ];"
            )
        };
        // Writing is the same, and says so, with what to do instead.
        let writes = errors(
            "uses { trendy.files };

operator write{} [ call save_text_file{path: file_path{text: \"notes.txt\"}, lines: array [\"Nile\"]}; ];
operator main{} [
    transaction [ call save_text_file{path: file_path{text: \"notes.txt\"}, lines: array [\"Nile\"]}; ];
    transaction [ call write{}; ];
];",
        );
        assert_eq!(writes.len(), 2, "{writes:#?}");
        assert!(writes.iter().all(|e| e.code == Code::IoInTransaction));
        let said: Vec<&str> = writes.iter().map(|e| e.message.as_str()).collect();
        assert!(
            said.contains(&"`save_text_file` writes a file, in a transaction"),
            "{said:?}"
        );
        assert!(
            said.contains(
                &"`write` writes a file, in a transaction, as `write` calls `save_text_file`"
            ),
            "{said:?}"
        );
        // At any depth of the calls, the chain labeled, each step where it
        // calls the next.
        let errors = errors(&program("transaction [ call twice{out lines: lines}; ];"));
        assert_eq!(errors.len(), 1, "{errors:#?}");
        assert_eq!(errors[0].code, Code::IoInTransaction);
        assert_eq!(
            errors[0].message,
            "`twice` reads a file, in a transaction, as `read` calls `load_text_file`"
        );
        let labels: Vec<&str> = std::iter::once(&errors[0].primary)
            .chain(&errors[0].secondary)
            .map(|l| l.message.as_str())
            .collect();
        assert_eq!(
            labels,
            [
                "leads to reading a file",
                "in this transaction",
                "`twice` calls `read` here",
                "`read` calls `load_text_file` here"
            ]
        );
        // In a transaction within one, or directly, too; but debug output,
        // and a read outside any, are allowed.
        for (main, failing) in [
            (
                "transaction [ transaction [ call load_text_file{path: file_path{text: \"a\"}, out lines: lines}; ]; ];",
                true,
            ),
            (
                "transaction [ call debug_value{v: 1}; call debug_table{r: relation { { n: 1 } }}; ];",
                false,
            ),
            (
                "call twice{out lines: lines}; transaction [ call debug_value{v: lines}; ];",
                false,
            ),
        ] {
            let source = program(main);
            if failing {
                assert_eq!(codes(&source), [Code::IoInTransaction], "{main}");
            } else {
                assert!(analyze_str(&source).is_ok(), "{main}");
            }
        }
    }

    #[test]
    fn the_library_has_error_and_its_messages() {
        let vars = variables(
            "uses { trendy.files };\n\nvar a := message{error: failure{message: \"why\"}};\nvar b := message{error: not_one_tuple{count: 0}};\nvar c := message{error: not_one_tuple{count: 2}};\nvar d := message{error: index_out_of_range{position: 0, length: 3}};\nvar e := message{error: index_out_of_range{position: 4, length: 0}};\nvar f := message{error: index_out_of_range{position: 4, length: 1}};\nvar g := message{error: index_out_of_range{position: 4, length: 3}};\nvar h := message{error: division_by_zero{operation: \"7 div 0\"}};\nvar i := message{error: overflow{operation: \"9223372036854775807 + 1\"}};\nvar j := message{error: unreadable_field{path: \"p.csv\", line: 1, column: 3, attribute: \"at.latitude\", type: \"approximate\", text: \"north\", out_of_range: false}};\nvar k := message{error: missing_column{path: \"p.csv\", attribute: \"km\", column: 4, columns: 3}};\nvar l := message{error: unreadable_field{path: \"p.csv\", line: 2, column: 1, attribute: \"open\", type: \"boolean\", text: \"maybe\", out_of_range: false}};\nvar m := message{error: component_key_violated_in_file{path: \"p.csv\", line: 2, attribute: \"s\", type: \"span\", component: \"days\", key: \"\", first: \"a\", second: \"b\"}};",
        );
        let messages: Vec<String> = vars.iter().map(|v| v.init.to_string()).collect();
        assert_eq!(
            messages,
            [
                "\"why\"",
                "\"the relation has no tuples, but exactly one is taken\"",
                "\"the relation has 2 tuples, but exactly one is taken\"",
                "\"an array has no element at position 0: positions count from 1\"",
                "\"the array is empty, so it has no element at position 4\"",
                "\"the array has 1 element, so none at position 4\"",
                "\"the array has 3 elements, so none at position 4\"",
                "\"division by zero: 7 div 0 has no result\"",
                "\"overflow: 9223372036854775807 + 1 is out of its type's range\"",
                "\"\\\"north\\\", in column 3 on line 1 of \\\"p.csv\\\", isn't an approximate number, the type of `at.latitude`; if line 1 is a header, `header: true` skips it\"",
                "\"`km` is read from column 4, but \\\"p.csv\\\" has 3 columns\"",
                "\"\\\"maybe\\\", in column 1 on line 2 of \\\"p.csv\\\", isn't a boolean, the type of `open`\"",
                "\"line 2 of \\\"p.csv\\\" gives `s` a value of `span` in which its `days` has two tuples, but its key {} allows at most one: a and b\"",
            ]
        );
        // The compiler's contract with it: the errors the runtime makes,
        // each with its components.
        let source = trendy_prelude::FILES.source.replace(
            "type not_utf8 is error { { path: text, line: integer } };",
            "type not_utf8 is error { { path: text, line: approximate } };",
        );
        assert_ne!(source, trendy_prelude::FILES.source);
        let errors = checked_with(trendy_prelude::FILES, &parse(&source));
        assert!(
            errors.iter().any(|d| d.code == Code::Internal
                && d.message
                    .contains("`type not_utf8 is error { { line: integer, path: text } };`")),
            "{errors:#?}"
        );
        let source = trendy_prelude::PRELUDE.source.replace(
            "heading transaction_collection := { error: error, failed: boolean };",
            "heading transaction_collection := { error: error, failed: integer };",
        );
        assert_ne!(source, trendy_prelude::PRELUDE.source);
        let errors = checked_prelude(&parse(&source));
        assert!(
            errors.iter().any(|d| d.code == Code::Internal
                && d.message.contains("`heading transaction_collection")),
            "{errors:#?}"
        );
        let source = trendy_prelude::PRELUDE
            .source
            .replace("type error union;", "type error_ union;");
        let errors = checked_prelude(&parse(&source));
        assert!(
            errors
                .iter()
                .any(|d| d.code == Code::Internal && d.message.contains("`type error union;`")),
            "{errors:#?}"
        );
    }

    #[test]
    fn a_text_is_its_code_points() {
        // In order, one for each Unicode scalar value, which is no
        // character a reader sees, and back.
        let vars = variables(
            "var p := to_code_points{from: \"hé🌍\"};\nvar t := from_code_points{from: p};\nvar n := length{a: to_code_points{from: \"e\\u{301}\"}};\nvar none := to_code_points{from: \"\"};\nvar omega := to_code_points{from: \"Ω\"}[1].value;\nvar empty := from_code_points{from: array code_point []};",
        );
        let values: Vec<String> = vars.iter().map(|v| v.init.to_string()).collect();
        assert_eq!(
            values,
            [
                "array [code_point{value: 104}, code_point{value: 233}, code_point{value: 127757}]",
                "\"hé🌍\"",
                "2",
                "array code_point []",
                "937",
                "\"\"",
            ]
        );
        // A code point is a Unicode scalar value: from 0 to 10FFFF hex, but
        // not the surrogates.
        let selected = |n: i64| format!("var c := code_point{{value: {n}}};{MAIN}");
        for n in [0, 55295, 57344, 1_114_111] {
            assert!(analyze_str(&selected(n)).is_ok(), "{n}");
        }
        for n in [-1, 55296, 57343, 1_114_112] {
            assert_eq!(codes(&selected(n)), [Code::BrokenConstraint], "{n}");
        }
        // Each made as selecting it would make it: of a subtype a
        // constraint defines, if it is true of it.
        let vars = variables(
            "type ascii is code_point { constraint value < 128 };\nvar a := to_code_points{from: \"aé\"};\nvar b := a[1] is ascii;\nvar c := a[2] is ascii;",
        );
        let values: Vec<String> = vars.iter().map(|v| v.init.to_string()).collect();
        assert_eq!(values[1..], ["true", "false"]);
    }

    #[test]
    fn the_prelude_declares_code_point_as_the_compiler_makes_it() {
        // Of other components than the compiler makes its values with, in
        // one representation, which has no conversions to give.
        let prelude = trendy_prelude::PRELUDE.source;
        let declaration = |from: usize| {
            let start = from + prelude[from..].find("type code_point {").unwrap();
            let end = start + prelude[start..].find("\n};\n").unwrap() + "\n};\n".len();
            (start, end)
        };
        let (start, end) = declaration(0);
        let (part, part_end) = declaration(end);
        let source = format!(
            "{}type code_point {{ {{ value: integer, plane: integer }} }};\n{}{}",
            &prelude[..start],
            &prelude[end..part],
            &prelude[part_end..]
        );
        // `trendy.uuid` calls its other representation's selector, which is gone.
        let errors = checked_prelude(&parse(&source));
        let errors: Vec<&Diagnostic> = errors.iter().filter(|e| e.code == Code::Internal).collect();
        assert_eq!(errors.len(), 1, "{errors:#?}");
        assert!(
            errors[0]
                .message
                .contains("`type code_point { { value: integer }, … };`"),
            "{errors:#?}"
        );
    }

    #[test]
    fn bytes_are_values_of_the_preludes_byte() {
        let vars = variables(
            "var a := x'7F';\nvar b := x\"de_AD\";\nvar c := x\"\";\nvar d := a.value;\nvar e := b[2];\nvar f := byte{value: 127} = a;\nvar g := to_text{from: a};\nvar h := to_text{from: b};\nvar i := array [a] = x\"7f\";",
        );
        let values: Vec<String> = vars.iter().map(|v| v.init.to_string()).collect();
        assert_eq!(
            values,
            [
                "x'7f'",
                "x\"dead\"",
                "x\"\"",
                "127",
                "x'ad'",
                "true",
                r#""x'7f'""#,
                r#""x\"dead\"""#,
                "true",
            ]
        );
        assert_eq!(vars[0].ty.to_string(), "byte");
        assert_eq!(vars[1].ty.to_string(), "array byte");
        // Out of its range, it isn't one.
        assert_eq!(
            codes(&format!("var a := byte{{value: 256}};{MAIN}")),
            [Code::BrokenConstraint]
        );
        assert_eq!(
            codes(&format!("var a := byte{{value: -1}};{MAIN}")),
            [Code::BrokenConstraint]
        );
    }

    #[test]
    fn a_literal_is_of_the_most_specific_type_a_byte_is_of() {
        let vars = variables(
            "type ascii is byte { constraint value < 128 };\nvar a := x'61' is ascii;\nvar b := x'ff' is ascii;\nvar c := x\"61ff\"[1] is ascii;\nvar d := x\"61ff\"[2] is ascii;\nvar e := (not x'ff') is ascii;\nvar f := (x'ff' and x'7f') is ascii;",
        );
        let values: Vec<String> = vars.iter().map(|v| v.init.to_string()).collect();
        assert_eq!(values, ["true", "false", "true", "false", "true", "true"]);
        // An array of them is still ordered, by their numbers, whichever each is
        // of, as a unit that compares arrays of bytes needn't know of a subtype.
        let vars = variables(
            "type ascii is byte { constraint value < 128 };\nvar a := x\"61\" < x\"ff\";\nvar b := x\"ff\" < x\"61\";\nvar c := x\"61\" < x\"62\";\nvar d := relation { { b: x'ff' }, { b: x'61' } };",
        );
        let values: Vec<String> = vars.iter().map(|v| v.init.to_string()).collect();
        assert_eq!(
            values,
            [
                "true",
                "false",
                "true",
                "relation { b: byte } { { b: x'61' }, { b: x'ff' } }"
            ]
        );
        // In their numbers' order, whichever each is of, whatever it is called.
        let vars = variables(
            "type tiny is byte { constraint value < 16 };\nvar a := relation { { b: x'ff' }, { b: x'01' } };\nvar b := x\"01\" < x\"ff\";",
        );
        assert_eq!(
            vars[0].init.to_string(),
            "relation { b: byte } { { b: x'01' }, { b: x'ff' } }"
        );
        assert_eq!(vars[1].init.to_string(), "true");
        // Written as a byte, and an array of them as bytes, whichever each is
        // of, as without the subtype; an array of the subtype's is of another
        // type, written as one.
        let vars = variables(
            "type ascii is byte { constraint value < 128 };\nvar a := to_text{from: x'61'};\nvar b := to_text{from: x\"61ff\"};\nvar c := to_text{from: array [treat x'61' as ascii]};",
        );
        let values: Vec<String> = vars.iter().map(|v| v.init.to_string()).collect();
        assert_eq!(
            values,
            [r#""x'61'""#, r#""x\"61ff\"""#, r#""array [x'61']""#]
        );
    }

    #[test]
    fn a_programs_own_byte_is_not_the_preludes() {
        let vars = variables(
            "type byte { { value: integer } };\nvar mine := byte{value: 7};\nvar theirs := x'07';\nvar n := mine.value;\nvar written := to_text{from: mine};",
        );
        assert_eq!(vars[0].ty.to_string(), "program::byte");
        assert_eq!(vars[1].ty.to_string(), "trendy.prelude::byte");
        assert_eq!(vars[1].init.to_string(), "x'07'");
        assert_eq!(vars[2].init.to_string(), "7");
        assert_eq!(vars[3].init.to_string(), r#""program::byte{value: 7}""#,);
        // Its operators are its own, as it has none.
        let errors = errors(
            "type byte { { value: integer } };\nvar mine := byte{value: 7};\nvar a := mine and mine;\n\noperator main{} [];",
        );
        assert_eq!(
            errors.iter().map(|e| e.code).collect::<Vec<_>>(),
            [Code::TypeMismatch, Code::TypeMismatch],
            "{errors:#?}"
        );
    }

    #[test]
    fn bit_operators_of_bytes_give_bytes() {
        let vars = variables(
            "var a := x'f0' and x'3c';\nvar b := x'f0' or x'0f';\nvar c := x'ff' xor x'0f';\nvar d := not x'0f';\nvar e := not x'00';\nvar f := not x'ff';\nvar g := and{x'ff', x'3c', x'0f'};\nvar h := or{x'01', x'02', x'04'};\nvar i := xor{x'01', x'01', x'04'};\nvar j: byte := and{};\nvar k: byte := or{};\nvar l: byte := xor{};\nvar m := (x'ff' and x'f0') = x'f0';",
        );
        let values: Vec<String> = vars.iter().map(|v| v.init.to_string()).collect();
        assert_eq!(
            values,
            [
                "x'30'", "x'ff'", "x'f0'", "x'f0'", "x'ff'", "x'00'", "x'0c'", "x'07'", "x'04'",
                "x'ff'", "x'00'", "x'00'", "true",
            ]
        );
        assert!(vars.iter().take(12).all(|v| v.ty.to_string() == "byte"));
    }

    #[test]
    fn bytes_have_no_arithmetic() {
        for (source, operator) in [
            ("x'ff' + x'01'", "+"),
            ("x'ff' + 1", "+"),
            ("-x'ff'", "-"),
            ("x'ff' * x'02'", "*"),
            ("x'ff' shl 1", "shl"),
            ("x'ff' shr 1", "shr"),
        ] {
            let errors = errors(&format!("var a := {source};{MAIN}"));
            assert_eq!(
                errors[0].code,
                Code::WrongOperandType,
                "{source}: {errors:#?}"
            );
            assert!(
                errors[0].message.contains(&format!("`{operator}`")),
                "{errors:#?}"
            );
            assert!(
                errors[0]
                    .notes
                    .iter()
                    .any(|n| n.contains("`b.value` is its number")),
                "{source}: {errors:#?}"
            );
        }
        // Nor have a type's values bits, but a byte's.
        let of_a_type =
            |value: &str| format!("type t {{ {{ n: integer }} }};\nvar a := {value};{MAIN}");
        assert_eq!(codes(&of_a_type("not t{n: 1}")), [Code::WrongOperandType]);
        assert_eq!(
            codes(&of_a_type("t{n: 1} and t{n: 2}")),
            [Code::TypeMismatch, Code::TypeMismatch]
        );
        // An array of bytes has no bits of its own.
        assert_eq!(
            codes(&format!("var a := x\"ab\" and x\"cd\";{MAIN}")),
            [Code::TypeMismatch, Code::TypeMismatch]
        );
    }

    #[test]
    fn other_languages_ways_with_bytes_are_fixed() {
        for (written, fixed) in [
            ("var a: byte := 0x7f;", "var a: byte := x'7f';"),
            ("var a: byte := 127;", "var a: byte := x'7f';"),
            ("var a: byte := 0;", "var a: byte := x'00';"),
            ("var a: byte := 'a';", "var a: byte := x'61';"),
            ("var a: byte := x\"7f\";", "var a: byte := x'7f';"),
            (
                "var a: array byte := x'7f';",
                "var a: array byte := x\"7f\";",
            ),
            ("var a := x'f0' and 0x0f;", "var a := x'f0' and x'0f';"),
            ("var a := 15 and x'f0';", "var a := x'0f' and x'f0';"),
            ("var a := x'61' = 'a';", "var a := x'61' = x'61';"),
            ("var a: u8 := x'01';", "var a: byte := x'01';"),
            ("var a: uint8 := x'01';", "var a: byte := x'01';"),
            ("var a: octet := x'01';", "var a: byte := x'01';"),
            ("var a: BLOB := x\"01\";", "var a: array byte := x\"01\";"),
            ("var a: bytea := x\"01\";", "var a: array byte := x\"01\";"),
            (
                "var a: VARBINARY := x\"01\";",
                "var a: array byte := x\"01\";",
            ),
            ("var a: binary := x\"01\";", "var a: array byte := x\"01\";"),
            ("var a: bytes := x\"01\";", "var a: array byte := x\"01\";"),
        ] {
            let source = format!("{written}{MAIN}");
            assert_eq!(fix(&source), format!("{fixed}{MAIN}"), "{written}");
        }
        // No literal of a byte says what these do: the type is what changes.
        for written in [
            "var a: byte := 300;",
            "var a: byte := -1;",
            "var a: byte := x\"7f7f\";",
        ] {
            let source = format!("{written}{MAIN}");
            let errors = errors(&source);
            assert_eq!(errors[0].code, Code::TypeMismatch, "{written}: {errors:#?}");
            assert!(!fix(&source).contains("x'"), "{written}");
        }
        // Grouped as its comparison, as of integers.
        assert_eq!(
            fix(&format!("var a := x'f0' and x'f0' = x'f0';{MAIN}")),
            format!("var a := (x'f0' and x'f0') = x'f0';{MAIN}")
        );
        let errors = errors(&format!("var a := x'f0' and x'f0' = x'f0';{MAIN}"));
        assert_eq!(errors[0].code, Code::BitGrouping);
        assert!(errors[0].message.contains("`and` of a byte and a boolean"));
        let errors = self::errors(&format!("var a := not x'f0' = x'0f';{MAIN}"));
        assert_eq!(errors[0].code, Code::BitGrouping);
        assert!(errors[0].message.contains("the complement of a byte"));
    }

    #[test]
    fn a_uuids_bits_say_its_version() {
        let test = |bytes: &str, ty: &str| {
            let vars = variables(&format!(
                "uses {{ trendy.uuid }};\nvar a := uuid{{bytes: x\"{bytes}\"}} is {ty};"
            ));
            vars[0].init.to_string()
        };
        for (bytes, ty, expected) in [
            ("00000000_0000_4000_8000_000000000000", "uuid_v4", "true"),
            ("00000000_0000_4fff_bfff_000000000000", "uuid_v4", "true"),
            ("00000000_0000_7000_8000_000000000000", "uuid_v7", "true"),
            ("00000000_0000_7fff_bfff_000000000000", "uuid_v7", "true"),
            // Another version.
            ("00000000_0000_1000_8000_000000000000", "uuid_v4", "false"),
            ("00000000_0000_7000_8000_000000000000", "uuid_v4", "false"),
            ("00000000_0000_4000_8000_000000000000", "uuid_v7", "false"),
            // Another variant.
            ("00000000_0000_4000_0000_000000000000", "uuid_v4", "false"),
            ("00000000_0000_4000_c000_000000000000", "uuid_v4", "false"),
            ("00000000_0000_7000_4000_000000000000", "uuid_v7", "false"),
            ("00000000_0000_7000_c000_000000000000", "uuid_v7", "false"),
        ] {
            assert_eq!(test(bytes, ty), expected, "{bytes} is {ty}");
        }
    }

    #[test]
    fn a_uuids_text_has_what_belongs_at_each_place() {
        let right = "00112233-4455-6677-8899-aabbccddeeff";
        let is_text = |text: &str| {
            let vars = variables(&format!(
                "uses {{ trendy.uuid }};\nvar a := is_uuid_text{{text: \"{text}\"}};"
            ));
            vars[0].init.to_string()
        };
        assert_eq!(is_text(right), "true");
        assert_eq!(is_text(&format!("{right}0")), "false");
        assert_eq!(is_text(&right[1..]), "false");
        assert_eq!(is_text(""), "false");
        for i in 0..right.len() {
            // A digit where a dash goes, and where a digit goes something else.
            let mut wrong = right.to_string();
            let replacement = if &right[i..=i] == "-" { "0" } else { "g" };
            wrong.replace_range(i..=i, replacement);
            assert_eq!(is_text(&wrong), "false", "{wrong}");
        }
        // Each digit each place can have.
        for digit in "0123456789abcdef".chars() {
            let every = right.replace(|c: char| c != '-', &digit.to_string());
            assert_eq!(is_text(&every), "true", "{every}");
        }
        assert_eq!(is_text(&right.to_uppercase()), "false");
    }

    #[test]
    fn a_value_block_in_a_conversion_has_its_locals() {
        // Only known when the program runs, so not folded away.
        let program = "type t {\n    { n: integer },\n    {\n        s: text,\n    }\n        to { n := [ var p := to_code_points{from: s}; length{a: p} ] }\n        from { s := [ var written := to_text{from: n}; written ] },\n};\n\noperator main{} [\n    var word := \"abc\";\n    call debug_value{v: t{s: word || \"d\"}.s};\n];";
        let program = analyze_str(program).unwrap();
        let locals = program
            .operators
            .iter()
            .filter_map(|operator| match &operator.kind {
                hir::OperatorKind::ReadOnly { locals, .. } if operator.name.starts_with("t ") => {
                    Some(locals.len())
                }
                _ => None,
            });
        assert_eq!(locals.collect::<Vec<_>>(), [1, 1]);
    }

    #[test]
    fn a_subtype_has_its_supertypes_representations() {
        let vars = variables(
            "type p {\n    { a: integer },\n    { b: integer } to { a := b + 1 } from { b := a - 1 },\n};\ntype c is p { constraint a > 10 };\nvar v := treat p{a: 20} as c;\nvar n := v.b;",
        );
        assert_eq!(vars[1].init.to_string(), "19");
    }

    #[test]
    fn other_languages_ways_with_characters_are_fixed() {
        // Python's `len` of a text counts its code points.
        let length = |of: &str| format!("var word := \"Zürich\";\nvar n := {of};{MAIN}");
        assert_eq!(
            fix(&length("length{a: word}")),
            length("length{a: to_code_points{from: word}}")
        );
        assert_eq!(
            fix(&length("count{r: word}")),
            length("length{a: to_code_points{from: word}}")
        );
        assert_eq!(
            fix(&fix(&length("len{word}"))),
            length("length{a: to_code_points{from: word}}")
        );
        // Rust's `chars`, Java's `codePoints` and JavaScript's
        // `fromCodePoint`, with their arguments named.
        let call = |written: &str| format!("var word := \"Zürich\";\nvar p := {written};{MAIN}");
        assert_eq!(
            fix(&call("chars{word}")),
            call("to_code_points{from: word}")
        );
        assert_eq!(
            fix(&call("codePoints{word}")),
            call("to_code_points{from: word}")
        );
        assert_eq!(
            fix(&call("fromCodePoint{to_code_points{from: word}}")),
            call("from_code_points{from: to_code_points{from: word}}")
        );
        // A character's number, and the character of a number, as Python's
        // `ord` and `chr` give them, which are a code point's, however the
        // argument is written.
        let one = |written: &str| format!("var letter := 'ü';\nvar p := {written};{MAIN}");
        for (written, fixed) in [
            ("ord{letter}", "code_point{character: letter}.value"),
            ("ord{c: '('}", "code_point{character: '('}.value"),
            ("chr{233}", "code_point{value: 233}.character"),
            ("fromCharCode{233}", "code_point{value: 233}.character"),
        ] {
            let errors = errors(&one(written));
            assert_eq!(errors.len(), 1, "{errors:#?}");
            assert_eq!(errors[0].code, Code::UnknownName);
            assert!(errors[0].notes[0].contains("`code_point`"), "{errors:#?}");
            assert_eq!(fix(&one(written)), one(fixed), "{written}");
            assert!(analyze_str(&one(fixed)).is_ok(), "{fixed}");
        }
        // Of a position in a text, or several numbers, how to write it.
        for (written, help) in [
            (
                "charCodeAt{s: word, i: 1}",
                "`to_code_points{from: t}[i].value`",
            ),
            ("fromCharCode{72, 105}", "`code_point{value: n}.character`"),
        ] {
            let errors = errors(&call(written));
            assert!(errors[0].suggestions.is_empty(), "{errors:#?}");
            assert!(errors[0].help[0].contains(help), "{errors:#?}");
        }
    }

    #[test]
    fn a_representations_own_constraints_are_checked_as_its_components_are_given() {
        // Which of its components represent a value: checked of those
        // given, before its `to` converts them, which would make one.
        let tens = "type tens {\n    { n: integer },\n    { big: integer, constraint big mod 10 = 0 }\n        to { n := big div 10 }\n        from { big := n * 10 },\n};\n";
        let values = |source: &str| -> Vec<String> {
            variables(&format!("{tens}{source}"))
                .iter()
                .map(|v| v.init.to_string())
                .collect()
        };
        assert_eq!(
            values("var a := tens{big: 20};\nvar b := tens{big: 20}.big;"),
            ["tens{n: 2}", "20"]
        );
        let broken = errors(&format!("{tens}var a := tens{{big: 15}};{MAIN}"));
        assert_eq!(broken.len(), 1, "{broken:#?}");
        assert_eq!(broken[0].code, Code::BrokenConstraint);
        assert_eq!(broken[0].message, "`tens{big: 15}` isn't a value of `tens`");
        assert_eq!(broken[0].secondary[0].message, "false of them");
        // So does a `replace` or an `update` in it, which selects in it.
        let replaced = errors(&format!(
            "{tens}var a := tens{{n: 2}} replace {{ big := 15 }};{MAIN}"
        ));
        assert_eq!(replaced[0].code, Code::BrokenConstraint, "{replaced:#?}");
        assert_eq!(
            replaced[0].message,
            "`tens{big: 15}` isn't a value of `tens`"
        );
        // The stored representation's components are its own, so no other
        // representation's constraint is checked of them.
        assert_eq!(values("var a := tens{n: 3}.big;"), ["30"]);
        // It names its own components alone, with a fix moving it into the
        // type's braces, where it names any representation's.
        let other = tens.replace(
            "constraint big mod 10 = 0",
            "constraint big mod 10 = 0 and n > 0",
        );
        let source = format!("{other}operator main{{}} [];");
        assert_eq!(codes(&source), [Code::OtherRepresentation]);
        assert_eq!(
            fix(&source),
            format!(
                "{}operator main{{}} [];",
                tens.replace(
                    "{ big: integer, constraint big mod 10 = 0 }",
                    "{ big: integer }"
                )
                .replace(
                    "from { big := n * 10 },",
                    "from { big := n * 10 }, constraint big mod 10 = 0 and n > 0,"
                )
            )
        );
        // One that selects a value in its representation would check
        // itself again.
        let cycle = tens.replace(
            "constraint big mod 10 = 0",
            "constraint tens{big: big}.n >= 0",
        );
        let errors = errors(&format!("{cycle}operator main{{}} [];"));
        assert_eq!(errors.len(), 1, "{errors:#?}");
        assert_eq!(errors[0].code, Code::ConstraintCycle);
        assert_eq!(
            errors[0].message,
            "`tens{big: …}`'s constraint selects a value in its representation through `tens`"
        );
    }

    #[test]
    fn a_type_the_interface_declares_is_converted_in_the_implementation() {
        // Its users see its representations, its constraints and its
        // order; the implementation declares it again, with each
        // representation's components and the conversions.
        let interface = "interface\n\ntype duration {\n    { seconds: integer },\n    { hours: integer, minutes: integer, constraint minutes >= 0 and minutes < 60 },\n    constraint seconds >= 0,\n};\n\nimplementation\n\n";
        let part = "type duration {\n    { hours: integer, minutes: integer }\n        to { seconds := hours * 3600 + minutes * 60 }\n        from { hours := seconds div 3600, minutes := seconds mod 3600 div 60 },\n};\n\n";
        let program = |rest: &str| format!("{interface}{part}{rest}operator main{{}} [];");
        let analyzed = analyze_str(&program(
            "var a := duration{hours: 1, minutes: 30}.seconds;\nvar b := duration{seconds: 5400}.minutes;\n",
        ))
        .unwrap();
        let values: Vec<String> = own_variables(&analyzed)
            .map(|v| v.init.to_string())
            .collect();
        assert_eq!(values, ["5400", "30"]);
        // Its representation's constraint, and the type's, checked as ever.
        assert_eq!(
            codes(&program("var a := duration{hours: 1, minutes: 90};\n")),
            [Code::BrokenConstraint]
        );
        assert_eq!(
            codes(&program("var a := duration{seconds: -1};\n")),
            [Code::BrokenConstraint]
        );
        // Without the implementation's, nothing says which representation
        // is stored, nor how the others convert.
        let alone = format!("{interface}operator main{{}} [];");
        assert_eq!(codes(&alone), [Code::Conversions]);
        assert!(
            errors(&alone)[0]
                .message
                .starts_with("the interface declares `duration` with several"),
            "{alone}"
        );
        // Conversions in the interface are moved to the implementation, to a
        // declaration of the type there, or to a new one.
        let converted = interface.replace(
            "{ hours: integer, minutes: integer, constraint minutes >= 0 and minutes < 60 },",
            "{ hours: integer, minutes: integer, constraint minutes >= 0 and minutes < 60 }\n        to { seconds := hours * 3600 + minutes * 60 }\n        from { hours := seconds div 3600, minutes := seconds mod 3600 div 60 },",
        );
        let source = format!("{converted}operator main{{}} [];");
        assert_eq!(codes(&source), [Code::InterfaceDeclaration]);
        let fixed = fix(&source);
        assert!(analyze_str(&fixed).is_ok(), "{fixed}");
        assert!(
            fixed.contains("implementation\n\noperator main{} [];\ntype duration {"),
            "{fixed}"
        );
        // A constraint or an order in the implementation's is moved to the
        // interface, where its users see it.
        let constrained = part.replace(
            "from { hours := seconds div 3600, minutes := seconds mod 3600 div 60 },",
            "from { hours := seconds div 3600, minutes := seconds mod 3600 div 60 },\n    constraint seconds < 86400,",
        );
        let source = format!("{interface}{constrained}operator main{{}} [];");
        assert_eq!(codes(&source), [Code::InterfaceDeclaration]);
        let fixed = fix(&source);
        assert!(analyze_str(&fixed).is_ok(), "{fixed}");
        assert!(
            fixed.contains("constraint seconds >= 0, constraint seconds < 86400,"),
            "{fixed}"
        );
        // Its representations' components, as the interface writes them,
        // each of them.
        let retyped = part.replace(
            "{ hours: integer, minutes: integer }",
            "{ hours: integer, minutes: text }",
        );
        let source = format!("{interface}{retyped}operator main{{}} [];");
        assert_eq!(codes(&source), [Code::DefinitionMismatch]);
        assert_eq!(fix(&source), program(""));
        // One it doesn't declare, which leaves two without conversions.
        let other = part.replace("{ hours: integer, minutes: integer }", "{ days: integer }");
        let errors = errors(&format!("{interface}{other}operator main{{}} [];"));
        let messages: Vec<&str> = errors.iter().map(|e| e.message.as_str()).collect();
        assert_eq!(
            messages,
            [
                "`duration` has more than one possible representation without conversions",
                "the interface declares no representation of `duration` with these components, `duration{days: …}`",
            ],
            "{errors:#?}"
        );
        assert!(
            errors[0].help.iter().any(|h| h.contains(
                "give the other `to` and `from`, in the implementation's declaration of `duration`"
            )),
            "{errors:#?}"
        );
    }

    #[test]
    fn an_implementation_repeats_only_the_representations_it_extends() {
        let interface = "interface\n\ntype t {\n    { a: integer },\n    { b: integer },\n    { c: integer },\n};\n\nimplementation\n\n";
        let b = "{ b: integer } to { a := b } from { b := a }";
        let c = "{ c: integer } to { a := c } from { c := a }";
        let of = |reps: &str| format!("{interface}type t {{ {reps} }};\noperator main{{}} [];");
        // The one left out is the one its values are stored as.
        let vars = variables(&format!(
            "{interface}type t {{ {b}, {c} }};\nvar one := t{{b: 1}};\nvar a := one.a;\nvar same := t{{c: 1}} = t{{a: 1}};"
        ));
        assert_eq!(vars[0].init.to_string(), "t{a: 1}");
        assert_eq!(vars[1].init.to_string(), "1");
        assert_eq!(vars[2].init.to_string(), "true");
        // Repeating it is allowed, and it is still checked against the interface.
        let repeated = of(&format!("{{ a: integer }}, {b}, {c}"));
        assert!(analyze_str(&repeated).is_ok());
        let retyped = of(&format!("{{ a: text }}, {b}, {c}"));
        assert_eq!(codes(&retyped), [Code::DefinitionMismatch]);
        assert_eq!(fix(&retyped), repeated);
        // Two left out, or none, can't say which it is.
        assert_eq!(codes(&of(b)), [Code::Conversions]);
        assert_eq!(
            codes(&of(&format!(
                "{{ a: integer }} to {{ b := a }} from {{ a := b }}, {b}, {c}"
            ))),
            [Code::Conversions]
        );
        // Conversions in the interface go after what its declaration has.
        let source = "interface\n\ntype t {\n    { a: integer },\n    { b: integer } to { a := b } from { b := a },\n    { c: integer },\n};\n\nimplementation\n\ntype t {\n    { c: integer } to { a := c } from { c := a },\n};\n\noperator main{} [];";
        assert_eq!(codes(source), [Code::InterfaceDeclaration]);
        let fixed = fix(source);
        assert!(analyze_str(&fixed).is_ok(), "{fixed}");
        assert!(fixed.contains("    { b: integer },\n"), "{fixed}");
        assert!(
            fixed.contains("{ c: integer } to { a := c } from { c := a },\n    { b: integer } to { a := b } from { b := a },"),
            "{fixed}"
        );
        // Conversions on both sides are for the author to choose between.
        let source = "interface\n\ntype t {\n    { a: integer },\n    { b: integer } to { a := b } from { b := a },\n};\n\nimplementation\n\ntype t {\n    { b: integer } to { a := b + 1 } from { b := a - 1 },\n};\n\noperator main{} [];";
        let found = errors(source);
        assert_eq!(found[0].code, Code::InterfaceDeclaration, "{found:#?}");
        assert!(found[0].suggestions.is_empty(), "{found:#?}");
        // With a declaration of none, it gets one, of those they convert.
        let source = "interface\n\ntype t {\n    { a: integer },\n    { b: integer } to { a := b } from { b := a },\n};\n\nimplementation\n\noperator main{} [];";
        let fixed = fix(source);
        assert!(analyze_str(&fixed).is_ok(), "{fixed}");
        assert!(
            fixed.contains("type t {\n    { b: integer } to { a := b } from { b := a },\n};"),
            "{fixed}"
        );
    }

    #[test]
    fn a_subtypes_implementation_repeats_only_what_it_extends() {
        // Neither its own constraint, nor even that it is a subtype.
        let program = |part: &str| {
            format!(
                "interface\n\ntype e {{ {{ a: approximate, b: approximate }} }};\n\ntype circle is e {{\n    constraint a = b,\n    {{ r: approximate }},\n}};\n\nimplementation\n\n{part} {{\n    {{ r: approximate }} to {{ a := r, b := r }} from {{ r := a }},\n}};\n\nvar c := e{{a: 2e0, b: 2e0}} is circle;\nvar r := circle{{r: 3e0}}.r;\n"
            )
        };
        for part in ["type circle", "type circle is e"] {
            let vars = variables(&program(part));
            assert_eq!(vars[0].init.to_string(), "true", "{part}");
            assert_eq!(vars[1].init.to_string(), "3e0", "{part}");
        }
    }

    #[test]
    fn the_stored_representations_own_constraints_are_the_types() {
        // Its components are every value's, so they hold of every value,
        // whatever representation it is selected in.
        let temperature = "type temperature {\n    { kelvin: approximate, constraint kelvin >= 0e0 },\n    { celsius: approximate }\n        to { kelvin := celsius + 273.15e0 }\n        from { celsius := kelvin - 273.15e0 },\n};\n";
        assert_eq!(
            codes(&format!(
                "{temperature}var t := temperature{{celsius: -300e0}};{MAIN}"
            )),
            [Code::BrokenConstraint]
        );
        assert_eq!(
            codes(&format!(
                "{temperature}var t := temperature{{kelvin: -1e0}};{MAIN}"
            )),
            [Code::BrokenConstraint]
        );
        // They name its own components alone, as another's do, so that
        // nothing in them says which representation is stored.
        let other = temperature.replace("kelvin >= 0e0", "celsius >= -273.15e0");
        let source = format!("{other}operator main{{}} [];");
        assert_eq!(codes(&source), [Code::OtherRepresentation]);
        assert_eq!(
            fix(&source),
            format!(
                "{}operator main{{}} [];",
                other
                    .replace(", constraint celsius >= -273.15e0 }", " }")
                    .replace(
                        "{ kelvin: approximate },",
                        "{ kelvin: approximate }, constraint celsius >= -273.15e0,"
                    )
            )
        );
    }

    #[test]
    fn a_code_point_is_a_number_or_its_character() {
        // A selection of literals is a literal, and its character that of the
        // code point alone.
        let values: Vec<String> = variables(
            "var open := code_point{character: '('};\nvar closed := code_point{value: 41}.character;\nvar same := code_point{character: '('} = code_point{value: 40};",
        )
        .iter()
        .map(|v| v.init.to_string())
        .collect();
        assert_eq!(values, ["code_point{value: 40}", "')'", "true"]);
        // Of a character of one code point alone.
        for written in ["'🇸🇪'", "'e\\u{301}'"] {
            let source = format!("var c := code_point{{character: {written}}};{MAIN}");
            assert_eq!(codes(&source), [Code::BrokenConstraint], "{written}");
        }
        // As text, the fix writes it as one.
        let source = format!("var c := code_point{{character: \"(\"}};{MAIN}");
        assert_eq!(codes(&source), [Code::TypeMismatch]);
        assert_eq!(
            fix(&source),
            format!("var c := code_point{{character: '('}};{MAIN}")
        );
    }

    #[test]
    fn a_character_is_a_value_of_its_own() {
        let value = |written: &str| {
            let source = format!("var v := {written};{MAIN}");
            let program = analyze_str(&source).unwrap_or_else(|e| panic!("{written}: {e:#?}"));
            program.variables[0].init.to_string()
        };
        for (written, known) in [
            ("'a'", "'a'"),
            ("'🇸🇪'", "'🇸🇪'"),
            ("'\\''", "'\\''"),
            ("'\"'", "'\"'"),
            ("'\\r\\n'", "'\\r\\n'"),
            ("'\\u{41}'", "'A'"),
            // Equal only if the same code points, and ordered by them.
            ("'a' = 'a'", "true"),
            ("'é' = 'e\\u{301}'", "false"),
            ("'a' < 'b'", "true"),
            ("'Z' < 'a'", "true"),
            ("min{r: relation { { c: 'b' }, { c: 'a' } }, of: c}", "'a'"),
            // Its conversions.
            ("from_character{from: '🇸🇪'}", "\"🇸🇪\""),
            ("to_text{from: 'a'}", "\"'a'\""),
            ("to_text{from: '\\''}", "\"'\\\\''\""),
            ("parse_character{from: \"🇸🇪\"}", "'🇸🇪'"),
            ("to_characters{from: \"🇸🇪a\"}", "array ['🇸🇪', 'a']"),
            ("to_characters{from: \"\"}", "array character []"),
            ("from_characters{from: array ['🇸🇪', 'a']}", "\"🇸🇪a\""),
            // A flag's halves are one, written together.
            ("from_characters{from: array ['🇸', '🇪']}", "\"🇸🇪\""),
            (
                "length{a: to_characters{from: from_characters{from: array ['🇸', '🇪']}}}",
                "1",
            ),
        ] {
            assert_eq!(value(written), known, "{written}");
        }
        // Of a type of the program's own too, which an order can be of.
        let source = format!(
            "type grade {{ {{ letter: character }}, constraint letter >= 'A' and letter <= 'F', order [asc letter] }};\nvar a := grade{{letter: 'B'}} < grade{{letter: 'C'}};{MAIN}"
        );
        assert_eq!(
            analyze_str(&source).unwrap().variables[0].init.to_string(),
            "true"
        );
        // Text that isn't one fails, as other text to read does, known when compiling.
        for written in ["ab", ""] {
            let source = format!("var c := parse_character{{from: \"{written}\"}};{MAIN}");
            let found = errors(&source);
            assert_eq!(found[0].code, Code::Unparsable, "{found:#?}");
            assert!(
                found[0].message.contains("isn't one character"),
                "{found:#?}"
            );
        }
    }

    #[test]
    fn arrays_concatenate_and_are_ordered_by_their_elements() {
        let value = |written: &str| {
            let source = format!("var v := {written};{MAIN}");
            let program = analyze_str(&source).unwrap_or_else(|e| panic!("{written}: {e:#?}"));
            program.variables[0].init.to_string()
        };
        for (written, known) in [
            ("array [1] || array [2, 3]", "array [1, 2, 3]"),
            ("array integer [] || array [1]", "array [1]"),
            ("array [1] || array integer []", "array [1]"),
            ("array [1, 2] < array [1, 3]", "true"),
            ("array [1, 3] < array [1, 2]", "false"),
            ("array [1] < array [1, 0]", "true"),
            ("array integer [] < array [0]", "true"),
            ("array [1] <= array [1]", "true"),
            ("array [2] > array [1, 9]", "true"),
            ("array [\"a\"] < array [\"b\"]", "true"),
            ("array [array [1]] < array [array [2]]", "true"),
            (
                "max{r: relation { { a: array [1] }, { a: array [2] } }, of: a}",
                "array [2]",
            ),
        ] {
            assert_eq!(value(written), known, "{written}");
        }
        // Of a type whose order is its stored components, ascending.
        let source = format!(
            "type n {{ {{ v: integer }}, order [asc v] }};\nvar a := array [n{{v: 1}}] < array [n{{v: 2}}];{MAIN}"
        );
        assert_eq!(
            analyze_str(&source).unwrap().variables[0].init.to_string(),
            "true"
        );
        // Not of tuples, which have no order, nor one the other way.
        for (types, a, b) in [
            ("", "array [tuple { x: 1 }]", "array [tuple { x: 2 }]"),
            (
                "type d { { v: integer }, order [desc v] };\n",
                "array [d{v: 1}]",
                "array [d{v: 2}]",
            ),
        ] {
            let source = format!("{types}var v := {a} < {b};{MAIN}");
            assert_eq!(codes(&source), [Code::WrongOperandType], "{a}");
            let source = format!("{types}var v := {a} = {b};{MAIN}");
            assert!(analyze_str(&source).is_ok(), "{a}");
        }
        // Of one type.
        let source = format!("var v := array [1] || array [\"a\"];{MAIN}");
        assert_eq!(codes(&source), [Code::TypeMismatch]);
        let source = format!("var v := array [1] || 2;{MAIN}");
        assert_eq!(codes(&source), [Code::TypeMismatch]);
    }

    #[test]
    fn a_known_failure_has_the_code_of_its_kind() {
        let failing = |error: &str, declared: &str| {
            let source = format!(
                "{declared}operator f{{}}: integer [ fail {error} ];\nvar v := f{{}};\n{MAIN}"
            );
            errors(&source)[0].code
        };
        // The language's, written as a program can write them.
        assert_eq!(
            failing("division_by_zero{operation: \"1 div 0\"}", ""),
            Code::DivisionByZero
        );
        assert_eq!(failing("failure{message: \"no\"}", ""), Code::Failed);
        // A program's own of the same name is its own, the language's named by its unit.
        let own = "type unparsable is error { { reason: text } };\noperator message{error: unparsable}: text [ error.reason ];\n";
        assert_eq!(failing("unparsable{reason: \"no\"}", own), Code::Failed);
        assert_eq!(
            failing(
                "trendy.prelude::unparsable{text: \"a\", type: \"integer\", out_of_range: false}",
                own
            ),
            Code::Unparsable
        );
    }

    #[test]
    fn a_character_is_no_text() {
        let var = |declared: &str| format!("var v{declared};{MAIN}");
        // What is written in the other quotes is written in its type's.
        for (written, fixed) in [
            (": text := 'a'", ": text := \"a\""),
            (": text := '\"'", ": text := \"\\\"\""),
            (": character := \"a\"", ": character := 'a'"),
            (": character := \"'\"", ": character := '\\''"),
            (": character := \"🇸🇪\"", ": character := '🇸🇪'"),
        ] {
            assert_eq!(codes(&var(written)), [Code::TypeMismatch], "{written}");
            assert_eq!(fix(&var(written)), var(fixed), "{written}");
            assert!(analyze_str(&var(fixed)).is_ok(), "{fixed}");
        }
        assert_eq!(fix(&var(" := 'a' = \"a\"")), var(" := 'a' = 'a'"),);
        // Not one character, nothing can make it one: the type's the fix.
        let found = errors(&var(": character := \"ab\""));
        assert_eq!(found[0].suggestions.len(), 1, "{found:#?}");
        assert_eq!(found[0].suggestions[0].edits[0].replacement, "text");
        // Converted by an operator when it isn't known.
        let body = |statement: &str| {
            format!("operator f{{c: character, t: text}} [ {statement} ];\n{MAIN}")
        };
        assert_eq!(
            fix(&body("var u: text := c;")),
            body("var u: text := from_character{from: c};")
        );
        assert_eq!(
            fix(&body("var u: character := t;")),
            body("var u: character := parse_character{from: t};")
        );
        assert_eq!(
            fix(&body("var u := c || t;")),
            body("var u := from_character{from: c} || t;")
        );
        assert_eq!(
            fix(&body("var u := t || c;")),
            body("var u := t || from_character{from: c};")
        );
        // It shows no representation: no components, and no selector.
        assert_eq!(codes(&body("var u := c.text;")), [Code::WrongOperandType]);
        for written in ["character{from: t}", "Character{from: t}", "char{from: t}"] {
            let source = body(&format!("var u := {written};"));
            assert_eq!(
                fix(&source),
                body("var u := parse_character{from: t};"),
                "{written}"
            );
        }
        // It has no default, as no character is any more a start than another.
        for declared in [": character", ": tuple { c: character }"] {
            assert_eq!(codes(&var(declared)), [Code::UntypedVariable], "{declared}");
        }
        for declared in [": array character", ": relation { c: character }"] {
            assert!(analyze_str(&var(declared)).is_ok(), "{declared}");
        }
        let source = "operator main{} [ var c: character; ];";
        assert!(analyze_str(source).is_ok());
        let source = "operator main{} [ var c: character; call debug_value{v: c}; ];";
        assert_eq!(codes(source), [Code::Unassigned]);
    }

    #[test]
    fn other_languages_names_for_a_character() {
        // Most languages' `char` is one, and SQL's and Tutorial D's a string.
        for (written, fixed) in [
            ("char", "character"),
            ("Char", "character"),
            ("Character", "character"),
            ("CHAR", "text"),
            ("CHARACTER", "text"),
        ] {
            let source = format!("var v: {written} := 'a';\n{MAIN}");
            assert_eq!(codes(&source)[0], Code::UnknownType, "{written}");
            assert_eq!(
                fix(&source),
                format!("var v: {fixed} := 'a';\n{MAIN}"),
                "{written}"
            );
        }
    }

    #[test]
    fn a_unit_reads_files_with_trendy_files() {
        // Its operators and errors are its interface's, which a unit sees
        // once it uses it, as the fix does.
        let source = "operator main{} [\n    var lines: array text;\n    call load_text_file{path: file_path{text: \"rivers.txt\"}, out lines: lines};\n];\n";
        let unused = errors(source);
        assert_eq!(unused.len(), 1, "{unused:#?}");
        assert_eq!(unused[0].code, Code::UnknownName);
        assert_eq!(
            unused[0].notes[0],
            "`load_text_file` is declared by `trendy.files`, which this unit doesn't use"
        );
        let fixed = fix(source);
        assert_eq!(fixed, format!("uses {{ trendy.files }};\n\n{source}"));
        assert!(analyze_str(&fixed).is_ok(), "{fixed}");
        let typed = "var e: relation { error: error } := relation { { error: not_utf8{path: \"a\", line: 1} } };\noperator main{} [];";
        assert_eq!(codes(typed), [Code::UnknownName]);
        // A unit can declare a type of one of its names, which a unit that
        // doesn't use it means, called by its own name, as what nothing uses has
        // no say in that, and the error the language makes by its unit's.
        let own = "type malformed_csv { { why: text } };\nvar m := malformed_csv{why: \"no header\"};\noperator main{} [];";
        let program = analyze_str(own).unwrap();
        assert_eq!(
            program.language_errors[&hir::LanguageError::MalformedCsv],
            "trendy.files::malformed_csv"
        );
        assert_eq!(program.variables[0].ty.to_string(), "malformed_csv");
        let made = program.language_error(hir::LanguageError::MalformedCsv);
        assert_eq!(made.name, "trendy.files::malformed_csv");
    }

    #[test]
    fn the_library_declares_type_parameters_in_any_order() {
        // In braces, as a program's are, so the built-ins' are the same.
        let declared = "types { C: any tuple, T: any tuple }";
        assert!(trendy_prelude::FILES.source.contains(declared));
        let source = trendy_prelude::FILES.source.replacen(
            declared,
            "types { T: any tuple, C: any tuple }",
            1,
        );
        assert_eq!(
            checked_with(trendy_prelude::FILES, &parse(&source)),
            Vec::new()
        );
    }

    #[test]
    fn the_prelude_declares_built_ins_as_the_compiler_implements_them() {
        let wrong = |from: &str, to: &str| {
            assert!(trendy_prelude::PRELUDE.source.contains(from), "{from}");
            let source = trendy_prelude::PRELUDE.source.replacen(from, to, 1);
            let errors = checked_prelude(&parse(&source));
            assert_eq!(errors.len(), 1, "{errors:#?}");
            assert_eq!(errors[0].code, Code::Internal);
            let span = errors[0].primary.span;
            let start = prelude_start("");
            (
                errors[0].message.clone(),
                source[span.start - start..span.end - start].to_string(),
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
        let (_, hir::Statement::Call(call)) = &body[1] else {
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
    fn what_runs_says_where_it_is() {
        let program = analyze_str(
            "operator half{n: integer, by: integer}: integer [\n    var d := n div by;\n    d + 1\n];\n\noperator main{} [\n    call debug_value{v: half{n: 4, by: 2}};\n];\n",
        )
        .unwrap();
        let site = |line, column| hir::Site {
            file: hir::FileId(0),
            line,
            column,
        };
        // Each statement, where it starts.
        let (_, main) = update_body(&program, "main");
        assert_eq!(main[0].0, site(7, 5));
        // A value block's values that can fail, each where its statement
        // starts, or the value, where it does.
        let half = program.operators.iter().find(|o| o.name == "half").unwrap();
        let hir::OperatorKind::ReadOnly { value, .. } = &half.kind else {
            panic!("`half` is read-only");
        };
        let hir::Expr::Let(_, d, rest) = value else {
            panic!("{value:?}");
        };
        assert!(
            matches!(**d, hir::Expr::At(at, _) if at == site(2, 5)),
            "{d:?}"
        );
        assert!(
            matches!(**rest, hir::Expr::At(at, _) if at == site(3, 5)),
            "{rest:?}"
        );
        // The prelude's are the prelude's: its implementation's, which its
        // interface's declaration completes.
        let some = program
            .operators
            .iter()
            .find(|o| o.name == "some" && matches!(o.kind, hir::OperatorKind::ReadOnly { .. }))
            .unwrap();
        let hir::OperatorKind::ReadOnly { value, .. } = &some.kind else {
            panic!("`some` is read-only");
        };
        let hir::Expr::At(at, _) = value else {
            panic!("{value:?}");
        };
        assert_eq!(program.files[at.file.0], "<prelude>");
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
            [Code::BuiltinInProgram]
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
        // A built-in's name, with its parameters' names or others, can be
        // a program's, which hides it, or overloads it.
        analyze_str("operator debug_value{v: integer} [];\noperator main{} [];").unwrap();
        analyze_str("operator debug_value{} [];\noperator main{} [];").unwrap();
    }

    #[test]
    fn databases_are_declared_with_their_relvars_and_constraints() {
        const KITCHEN: &str = "database kitchen;\nvar stock := 5;\nvar kitchen.pantry: relation { item: text, grams: integer } key { item };\nvar kitchen.recipes: relation { recipe: text, item: text } key { recipe, item };\n";
        // In the interface or not, its relvars named alone in its
        // constraints, and a variable of the program's can have a relvar's
        // name.
        let valid = format!(
            "interface\n\n{}constraint kitchen.known := recipes{{item}} <= pantry{{item}};\n\nimplementation\n\nvar stock := 5;\nvar pantry := 1;{MAIN}",
            KITCHEN.replace("var stock := 5;\n", "")
        );
        assert!(
            analyze_str(&valid).is_ok(),
            "{:#?}",
            analyze_str(&valid).err()
        );
        for (added, codes) in [
            (
                "var kitchen.seeded: relation { a: integer } := relation { { a: 1 } };",
                &[Code::DatabasePart][..],
            ),
            ("var kitchen.count: integer;", &[Code::DatabasePart]),
            ("var kitchen.loose;", &[Code::DatabasePart]),
            (
                "var kitchn.more: relation { a: integer };",
                &[Code::UnknownName],
            ),
            (
                "var kitchen.pantry: relation { a: integer };",
                &[Code::DuplicateName],
            ),
            ("var kitchen := 1;", &[Code::DuplicateName]),
            ("database stock;", &[Code::DuplicateName]),
            (
                "constraint kitchen.c := is_empty{r: kitchen.pantry};",
                &[Code::DatabasePart],
            ),
            ("constraint kitchen.c := stock > 0;", &[Code::DatabasePart]),
            (
                "constraint kitchen.c := 1 < 2;",
                &[Code::ConstantConstraint],
            ),
            ("constraint kitchen.c := pantry;", &[Code::TypeMismatch]),
            (
                "constraint kitchen.c := count{r: pantry} > 0;\nconstraint kitchen.c := count{r: pantry} > 1;",
                &[Code::DuplicateName],
            ),
            (
                "operator f{} [ call debug_value{v: kitchen}; ];",
                &[Code::DatabasePart],
            ),
        ] {
            let found = errors(&format!("{KITCHEN}{added}{MAIN}"));
            let found: Vec<Code> = found.iter().map(|e| e.code).collect();
            assert_eq!(found, codes, "{added}");
        }
        // The fixes that are certain: an initial value removed.
        assert_eq!(
            fix(&format!(
                "{KITCHEN}var kitchen.seeded: relation {{ a: integer }} := relation {{ {{ a: 1 }} }};{MAIN}"
            )),
            format!("{KITCHEN}var kitchen.seeded: relation {{ a: integer }};{MAIN}")
        );
        // A constraint of one database can share another's name, and a
        // unit's own.
        let shared = format!(
            "{KITCHEN}database cellar;\nvar cellar.wines: relation {{ wine: text }} key {{ wine }};\nconstraint kitchen.some := count{{r: pantry}} >= 0;\nconstraint cellar.some := count{{r: wines}} >= 0;\nconstraint some := stock >= 0;{MAIN}"
        );
        assert!(
            analyze_str(&shared).is_ok(),
            "{:#?}",
            analyze_str(&shared).err()
        );
    }

    #[test]
    fn constraints_hold_of_the_initial_values() {
        const DECLARED: &str = "var cities := relation {\n    { city: \"Nyeri\", country: \"Kenya\" },\n    { city: \"Cair Paravel\", country: \"Narnia\" },\n}\nkey { city };\nvar countries := relation { { country: \"Kenya\" } } key { country };\nvar stock := 5;\n";
        // A valid one is the program's, of the variables as variables.
        let program =
            analyze_str(&format!("{DECLARED}constraint stocked := stock > 0;{MAIN}")).unwrap();
        assert_eq!(program.constraints.len(), 1);
        let constraint = &program.constraints[0];
        assert_eq!(constraint.name, "stocked");
        assert_eq!(constraint.written, "stock > 0");
        let stock = program
            .variables
            .iter()
            .position(|v| v.name == "stock")
            .unwrap();
        assert_eq!(constraint.variables, [hir::VariableId(stock)].into());
        // Its parts, an `and`'s operands, those that can't fail first, each
        // with the relation of the tuples that make it false where its form
        // says which.
        let program = analyze_str(&format!(
            "{DECLARED}constraint c := (10 div stock > 1 and is_empty{{r: cities where city = \"Lima\"}}) and stock > 0;{MAIN}"
        ))
        .unwrap();
        let parts: Vec<(&str, bool)> = program.constraints[0]
            .parts
            .iter()
            .map(|p| (p.written.as_str(), p.breaking.is_some()))
            .collect();
        assert_eq!(
            parts,
            [
                ("stock > 0", false),
                ("10 div stock > 1", false),
                ("is_empty{r: cities where city = \"Lima\"}", true),
            ]
        );
        // One the initial values break, with what breaks it where its form
        // says: the tuples outside the right of `<=`, a false operand of
        // `and`, and the tuples of `is_empty`'s relation.
        for (constraint, breaches) in [
            (
                "cities{country} <= countries",
                &["`tuple { country: \"Narnia\" }` isn't in `countries`"][..],
            ),
            (
                "countries >= cities{country}",
                &["`tuple { country: \"Narnia\" }` isn't in `countries`"],
            ),
            (
                "stock > 0 and is_empty{r: cities where country = \"Narnia\"}",
                &[
                    "`cities where country = \"Narnia\"` has `tuple { city: \"Cair Paravel\", country: \"Narnia\" }`",
                ],
            ),
            ("stock mod 2 = 0", &["this is false"]),
        ] {
            let source = format!("{DECLARED}constraint known := {constraint};{MAIN}");
            let broken = errors(&source);
            assert_eq!(broken.len(), 1, "{constraint}: {broken:#?}");
            let d = &broken[0];
            assert_eq!(d.code, Code::ConstraintViolated, "{constraint}");
            assert_eq!(d.message, "the initial values break the constraint `known`");
            let labels: Vec<&str> = std::iter::once(&d.primary)
                .chain(&d.secondary)
                .map(|l| l.message.as_str())
                .filter(|l| *l != "false of the initial values")
                .collect();
            assert_eq!(labels, breaches, "{constraint}");
        }
        // One that mentions no variable says nothing, or can never hold.
        let source = format!("{DECLARED}constraint always := 1 < 2;\n{MAIN}");
        assert_eq!(fix(&source), format!("{DECLARED}operator main{{}} [];"));
        for condition in ["1 < 2", "2 < 1"] {
            let constant = errors(&format!("{DECLARED}constraint c := {condition};{MAIN}"));
            assert_eq!(constant.len(), 1, "{condition}: {constant:#?}");
            assert_eq!(constant[0].code, Code::ConstantConstraint, "{condition}");
        }
        // A part of it that fails is reported where it is.
        let source = format!("{DECLARED}constraint c := 10 div (stock - 5) > 0;{MAIN}");
        let failed = errors(&source);
        assert_eq!(failed.len(), 1, "{failed:#?}");
        let at = failed[0].primary.span;
        assert_eq!(&source[at.start..at.end], "(stock - 5)");
        // A boolean, with names of one namespace each.
        for (constraint, code) in [
            ("c := stock", Code::TypeMismatch),
            ("c := stocks > 0", Code::UnknownName),
            (
                "c := stock > 0;\nconstraint c := stock < 9",
                Code::DuplicateName,
            ),
        ] {
            let found = errors(&format!("{DECLARED}constraint {constraint};{MAIN}"));
            let codes: Vec<Code> = found.iter().map(|e| e.code).collect();
            assert_eq!(codes, [code], "{constraint}: {found:#?}");
        }
        // A variable's name too, as nothing names a constraint as a value.
        assert!(analyze_str(&format!("{DECLARED}constraint cities := stock > 0;{MAIN}")).is_ok());
        // In the interface, though its variables are the implementation's.
        let source = "interface\n\nconstraint stocked := stock > 0;\n\nimplementation\n\nvar stock := 5;\n\noperator main{} [];";
        assert_eq!(
            fix(source),
            "interface\n\nimplementation\n\nvar stock := 5;\n\noperator main{} [];\nconstraint stocked := stock > 0;\n"
        );
    }

    #[test]
    fn tclose_closes_a_relation_of_two_attributes_of_one_type() {
        // Folded when the relation is known: every path, cycles too.
        let vars = variables(
            "var closed := relation { { a: 1, b: 2 }, { a: 2, b: 3 }, { a: 3, b: 2 } } tclose;",
        );
        let pairs = |pairs: &[(i64, i64)]| {
            hir::Value::Relation(hir::RelationValue {
                heading: hir::Heading::new(
                    [
                        ("a".to_string(), hir::Type::Scalar(hir::Scalar::Integer)),
                        ("b".to_string(), hir::Type::Scalar(hir::Scalar::Integer)),
                    ]
                    .into(),
                ),
                tuples: pairs
                    .iter()
                    .map(|&(a, b)| {
                        hir::TupleValue::new(
                            [
                                ("a".to_string(), hir::Value::Integer(a)),
                                ("b".to_string(), hir::Value::Integer(b)),
                            ]
                            .into(),
                        )
                    })
                    .collect(),
            })
        };
        assert_eq!(
            vars[0].init,
            pairs(&[(1, 2), (1, 3), (2, 2), (2, 3), (3, 2), (3, 3)])
        );
        // Not two attributes of one type, nor a relation: what to close is
        // the program's to say.
        for (source, code) in [
            (
                "var x := relation { { a: 1, b: 2, c: 3 } } tclose;",
                Code::TcloseOperand,
            ),
            (
                "var x := relation { { a: 1, b: \"two\" } } tclose;",
                Code::TcloseOperand,
            ),
            (
                "var x := tuple { a: 1, b: 2 } tclose;",
                Code::WrongOperandType,
            ),
        ] {
            let errors = errors(&format!("{source}{MAIN}"));
            assert_eq!(errors.len(), 1, "{source}: {errors:#?}");
            assert_eq!(errors[0].code, code, "{source}");
            assert!(errors[0].suggestions.is_empty(), "{source}");
        }
        let errors = errors(&format!(
            "var r := relation {{ {{ a: 1, b: 2, c: \"x\" }} }};\nvar x := (r where a > 0) tclose;{MAIN}"
        ));
        assert!(
            errors[0].help[0].contains("`(r where a > 0){a, b} tclose`"),
            "{errors:#?}"
        );
    }

    #[test]
    fn a_units_interface_declares_what_its_implementation_defines() {
        // An interface operator's heading, and its definition repeating
        // it, are one operator, called by its name.
        let source = "interface\n\nheading lake := { name: text, depth: integer };\n\noperator deepest{lakes: relation lake}: relation lake;\n\noperator clear{var lakes: relation lake};\n\nimplementation\n\noperator deepest{lakes: relation lake}: relation lake [\n    lakes where depth = max{r: lakes, of: depth}\n];\n\noperator clear{var lakes: relation lake} [\n    lakes := relation {};\n];\n\noperator main{} [\n    var lakes := relation { { name: \"Baikal\", depth: 1642 } };\n    call debug_value{v: deepest{lakes: lakes}};\n    call clear{var lakes: lakes};\n];\n";
        assert!(analyze_str(source).is_ok(), "{:#?}", errors(source));
        // What the interface can't have, or must, each with its fix.
        let main = "\noperator main{} [];\n";
        for (source, fixed, code) in [
            (
                format!(
                    "interface\n\nvar total := 0;\n\nimplementation\n\nconstraint counted := total >= 0;\n{main}"
                ),
                format!(
                    "interface\n\nvar total := 0;\n\nconstraint counted := total >= 0;\n\nimplementation\n{main}"
                ),
                Code::InterfaceDeclaration,
            ),
            (
                format!(
                    "interface\n\nvar total: integer;\n\nimplementation\n\nvar total := 0;\n{main}"
                ),
                format!("interface\n\nvar total: integer := 0;\n\nimplementation\n{main}"),
                Code::DuplicateName,
            ),
            (
                format!(
                    "interface\n\noperator double{{n: integer}}: integer [\n    n * 2\n];\n\nimplementation\n{main}"
                ),
                format!(
                    "interface\n\noperator double{{n: integer}}: integer;\n\nimplementation\n{main}\noperator double{{n: integer}}: integer [\n    n * 2\n];\n"
                ),
                Code::InterfaceDeclaration,
            ),
            (
                format!(
                    "interface\n\noperator double{{n: metres}}: integer;\n\nimplementation\n\n/* A depth. */\ntype metres {{ {{ m: integer }} }};\n\noperator double{{n: metres}}: integer [\n    n.m * 2\n];\n{main}"
                ),
                format!(
                    "interface\n\noperator double{{n: metres}}: integer;\n\n/* A depth. */\ntype metres {{ {{ m: integer }} }};\n\nimplementation\n\noperator double{{n: metres}}: integer [\n    n.m * 2\n];\n{main}"
                ),
                Code::InterfaceDeclaration,
            ),
            (
                format!(
                    "interface\n\ntype running_low is error {{ {{ sacks: integer }} }};\n\noperator message{{error: running_low}}: text [\n    \"low\"\n];\n\nimplementation\n{main}"
                ),
                format!(
                    "interface\n\ntype running_low is error {{ {{ sacks: integer }} }};\n\nimplementation\n{main}\noperator message{{error: running_low}}: text [\n    \"low\"\n];\n"
                ),
                Code::InterfaceDeclaration,
            ),
            (
                format!("interface\n\noperator clear{{n: integer}};\n\nimplementation\n{main}"),
                format!(
                    "interface\n\noperator clear{{n: integer}};\n\nimplementation\n{main}\noperator clear{{n: integer}} [];\n"
                ),
                Code::Versions,
            ),
            (
                format!(
                    "interface\n\noperator double{{n: integer}}: integer;\n\nimplementation\n\noperator double{{n: integer}}: approximate [\n    2e0\n];\n{main}"
                ),
                format!(
                    "interface\n\noperator double{{n: integer}}: integer;\n\nimplementation\n\noperator double{{n: integer}}: integer [\n    2e0\n];\n{main}"
                ),
                Code::DefinitionMismatch,
            ),
        ] {
            assert_eq!(codes(&source), [code], "{source}");
            assert_eq!(fix(&source), fixed, "{source}");
        }
        // A read-only operator's missing body is the program's to give.
        let errors = errors(&format!(
            "interface\n\noperator double{{n: integer}}: integer;\n\nimplementation\n{main}"
        ));
        assert_eq!(errors[0].code, Code::Versions);
        assert!(errors[0].suggestions.is_empty());
        assert!(errors[0].help[0].contains("operator double{n: integer}: integer [ … ];"));
    }

    #[test]
    fn the_runtime_gives_trendy_process_its_variables_as_the_program_starts() {
        // Read and assigned as any other unit's, from `main` on.
        let source = "uses { trendy.process };\n\noperator main{} [\n    call debug_value{v: arguments};\n    insert environment relation { { name: \"LANG\", value: \"sw_KE\" } };\n];";
        let program = analyze_str(source).unwrap();
        let given: Vec<Option<hir::Given>> = program
            .variables
            .iter()
            .filter_map(|v| v.given)
            .map(Some)
            .collect();
        assert_eq!(
            given,
            [Some(hir::Given::Arguments), Some(hir::Given::Environment)]
        );
        // Its constraint, which only the program can check of what it is
        // given, is checked as it starts, and is no program's that doesn't use
        // the unit.
        assert!(program.constraints.iter().any(|c| c.checked_at_start));
        let unused = analyze_str("operator main{} [];").unwrap();
        assert!(unused.constraints.is_empty(), "{:#?}", unused.constraints);
        // Not when compiling.
        let source = "uses { trendy.process };\n\nvar first := arguments;\n\noperator main{} [];";
        assert_eq!(codes(source), [Code::InitialValueWhenRunning]);
        // A program declares none.
        let source = "builtin var mine: integer;\n\noperator main{} [];";
        let found = errors(source);
        assert_eq!(found[0].code, Code::BuiltinInProgram, "{found:#?}");
        assert!(found[0].help[0].contains("remove `builtin`"), "{found:#?}");
        // The unit's declarations are the compiler's: one it gives no value,
        // or of another type, or with a value of its own, is an internal error,
        // and so is one missing.
        let process = trendy_prelude::PROCESS.source;
        for (from, to) in [
            (
                "builtin var arguments: array text;",
                "builtin var arguments: array text;\nbuiltin var flags: array text;",
            ),
            (
                "builtin var arguments: array text;",
                "builtin var arguments: text;",
            ),
            (
                "builtin var arguments: array text;",
                "builtin var arguments: array text := array [];",
            ),
            (
                "builtin var arguments: array text;",
                "var arguments: array text;",
            ),
        ] {
            assert!(process.contains(from));
            let source = process.replacen(from, to, 1);
            let found = checked_with(trendy_prelude::PROCESS, &parse(&source));
            assert!(
                found.iter().any(|e| e.code == Code::Internal),
                "{to}: {found:#?}"
            );
        }
    }

    #[test]
    fn main_takes_its_errors() {
        // The arguments are `trendy.process`'s, by any name another language
        // gives them: the fix removes the parameter, and uses the unit.
        for written in [
            "arguments: array text",
            "var arguments: array text",
            "args: text",
            "argv: array text",
            "ARGV: array text",
            "params: integer",
        ] {
            let source = format!("operator main{{{written}}} [];");
            assert_eq!(codes(&source), [Code::MainSignature], "{written}");
            assert_eq!(
                fix(&source),
                "uses { trendy.process };\n\noperator main{} [];",
                "{written}"
            );
        }
        // Where the unit is used already, its `arguments` are what the
        // parameter was.
        let source = "uses { trendy.process };\n\noperator main{arguments: array text} [\n    call debug_value{v: arguments};\n];";
        let fixed = fix(source);
        assert_eq!(
            fixed,
            "uses { trendy.process };\n\noperator main{} [\n    call debug_value{v: arguments};\n];"
        );
        assert!(analyze_str(&fixed).is_ok(), "{fixed}");
        // Another parameter is removed.
        let source = "operator main{depth: integer} [];";
        assert_eq!(codes(source), [Code::MainSignature]);
        assert_eq!(fix(source), "operator main{} [];");
        // Read elsewhere, the arguments and the environment say where they are.
        for (name, rule) in [
            ("argv", "variable `arguments`"),
            ("args", "variable `arguments`"),
            ("argc", "variable `arguments`"),
            ("ARGV", "variable `arguments`"),
            ("environ", "variable `environment`"),
            ("env", "variable `environment`"),
            ("ENV", "variable `environment`"),
        ] {
            let found = errors(&format!(
                "operator main{{}} [ call debug_value{{v: {name}}}; ];"
            ));
            assert_eq!(found[0].code, Code::UnknownName, "{name}");
            assert!(found[0].notes[0].contains(rule), "{name}: {found:#?}");
        }
        // So do C's `getenv` and `setenv`.
        for source in [
            "operator main{} [ call debug_value{v: getenv{name: \"HOME\"}}; ];",
            "operator main{} [ call setenv{name: \"HOME\", value: \"/\"}; ];",
        ] {
            let found = errors(source);
            assert_eq!(found[0].code, Code::UnknownName, "{source}");
            assert!(
                found[0].notes[0].contains("variable `environment`"),
                "{source}: {found:#?}"
            );
        }
        // Without the unit, they are offered with it.
        let found = errors("operator main{} [ call debug_value{v: environment}; ];");
        assert_eq!(found[0].code, Code::UnknownName);
        assert_eq!(
            fix("operator main{} [ call debug_value{v: environment}; ];"),
            "uses { trendy.process };\n\noperator main{} [ call debug_value{v: environment}; ];"
        );
        let given = "operator main{out errors: relation transaction_collection} [ errors := relation {}; ];";
        assert!(analyze_str(given).is_ok());
        // An `out` parameter, of a `relation transaction_collection`,
        // which the fixes make it; assigned, as any `out` parameter is.
        for (source, fixed) in [
            (
                "operator main{errors: relation transaction_collection} [];",
                "operator main{out errors: relation transaction_collection} [];",
            ),
            (
                "operator main{var errors: relation transaction_collection} [];",
                "operator main{out errors: relation transaction_collection} [];",
            ),
            (
                "operator main{out errors: relation { error: error }} [ errors := relation {}; ];",
                "operator main{out errors: relation transaction_collection} [ errors := relation {}; ];",
            ),
        ] {
            assert_eq!(codes(source), [Code::MainSignature], "{source}");
            assert_eq!(fix(source), fixed, "{source}");
        }
        assert_eq!(
            codes("operator main{out errors: relation transaction_collection} [];"),
            [Code::Unassigned]
        );
        let errors = errors("operator main{errors: text, more: integer} [];");
        assert_eq!(errors[0].code, Code::MainSignature);
        assert!(errors[0].notes[0].contains("out errors: relation transaction_collection"));
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
    fn operands_are_booleans_or_integers() {
        let source = format!(
            "var a := not \"x\";\nvar b := true and 0;\nvar c: integer := true or false;\noperator d{{t: text}} [ call debug_value{{v: t and t}}; ];{MAIN}"
        );
        let errors = errors(&source);
        assert_eq!(
            errors.iter().map(|e| &e.message[..]).collect::<Vec<_>>(),
            [
                "`not` can't be applied to text",
                "mismatched types: expected `boolean`, found `integer`",
                // Booleans agree, so the result is reported, not each.
                "mismatched types: expected `integer`, found `boolean`",
                "mismatched types: expected `boolean`, found `text`",
                "mismatched types: expected `boolean`, found `text`",
            ]
        );
        assert_eq!(
            errors[0].primary.message,
            "`not` takes booleans, or integers or bytes, bit by bit"
        );
        // Operands of no type `and` takes are booleans.
        let origin = &errors[3].secondary[0];
        assert_eq!(&source[origin.span.start..origin.span.end], "and");
        assert_eq!(
            origin.message,
            "`and` takes booleans, or integers or bytes, bit by bit"
        );
        let fixed = fix(&source);
        assert!(
            fixed.contains(
                "var b := true and false;\nvar c: boolean := true or false;\noperator d{t: text} [ call debug_value{v: parse_boolean{from: t} and parse_boolean{from: t}}; ];"
            ),
            "`0` is rewritten as `false`: {fixed}"
        );
    }

    #[test]
    fn bits_of_integers() {
        let values = |source: &str| -> Vec<hir::Value> {
            let program = analyze_str(source).unwrap();
            own_variables(&program).map(|v| v.init.clone()).collect()
        };
        assert_eq!(
            values(&format!(
                "var a := 0xF0 and 0x3C;\nvar b := 0xF0 or 0x0F;\nvar c := xor{{1, 2, 3}};\nvar d := not 0;\nvar e := 1 shl 3 + 1;\nvar f := -9 shr 1;\nvar g := 1 shl -1;\nvar h: integer := and{{}};\nvar i := 0b1010 and not 0b10;{MAIN}"
            )),
            [0x30, 0xFF, 0, -1, 9, -5, 0, -1, 8].map(hir::Value::Integer)
        );
        let errors = errors(&format!("var a := 1 shl 63;{MAIN}"));
        assert_eq!(errors[0].code, Code::Overflow, "{errors:#?}");
        // Integers that agree are reported as the result.
        let errors = self::errors(&format!(
            "var flags := 3;\nvar r := relation {{ {{ a: 1 }} }} where flags and a;{MAIN}"
        ));
        assert_eq!(errors.len(), 1, "{errors:#?}");
        assert_eq!(
            errors[0].message,
            "mismatched types: expected `boolean`, found `integer`"
        );
        assert_eq!(errors[0].primary.message, "this is `integer`");
    }

    #[test]
    fn bits_compared() {
        let source = format!(
            "var flags := 6;\nvar a := flags and 4 = 4;\nvar b := 4 = flags and 4;\nvar c := flags xor 1 shl 2 <> 0 or true;{MAIN}"
        );
        let errors = errors(&source);
        let codes: Vec<_> = errors.iter().map(|e| e.code).collect();
        assert_eq!(codes, [Code::BitGrouping; 3], "{errors:#?}");
        assert_eq!(
            errors[0].message,
            "`flags and 4 = 4` is `flags and (4 = 4)`, `and` of an integer and a boolean"
        );
        assert_eq!(
            errors[0].suggestions[0].message,
            "compare the bits: `(flags and 4) = 4`"
        );
        assert_eq!(
            fix(&source),
            format!(
                "var flags := 6;\nvar a := (flags and 4) = 4;\nvar b := 4 = (flags and 4);\nvar c := (flags xor 1 shl 2) <> 0 or true;{MAIN}"
            )
        );
        // Not when the comparison's operand next to the integer isn't one.
        let errors = self::errors(&format!(
            "var flags := 6;\nvar a := flags and \"x\" = \"y\";{MAIN}"
        ));
        assert_eq!(errors[0].code, Code::TypeMismatch, "{errors:#?}");
    }

    /// `expr` without the sites in it, which only
    /// [`what_runs_says_where_it_is`] is about.
    fn unsited(expr: hir::Expr) -> hir::Expr {
        match expr {
            hir::Expr::At(_, expr) => unsited(*expr),
            expr => expr.map_children(|e, _| unsited(e)),
        }
    }

    fn main_body(source: &str) -> Vec<(hir::Site, hir::Statement)> {
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
    fn argument(statement: &(hir::Site, hir::Statement)) -> &hir::Expr {
        let (_, hir::Statement::Call(call)) = statement else {
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
            "var a := 1 + true;\nvar b := -\"x\";\nvar c: boolean := 1 * 2;\nvar d := not 1.5e0;\nvar e := 1.5e0 + 1;\nvar r: approximate := -1;{MAIN}"
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
                // `not` doesn't take approximate numbers.
                "",
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
    fn square_roots_and_trigonometry_when_compiling() {
        let r = |x| hir::Value::Approximate(hir::Approximate::new(x));
        assert_eq!(init("var a := pi;", "a"), r(std::f64::consts::PI));
        assert_eq!(
            init("var a := sqrt{of: 3e0 * 3e0 + 4e0 * 4e0};", "a"),
            r(5.0)
        );
        assert_eq!(init("var a := sqrt{of: -0e0};", "a"), r(0.0));
        assert_eq!(init("var a := cos{of: pi};", "a"), r(-1.0));
        assert_eq!(init("var a := sin{of: pi / 2e0};", "a"), r(1.0));
        assert_eq!(
            init("var a := atan2{y: 1e0, x: -1e0};", "a"),
            r(3.0 * std::f64::consts::FRAC_PI_4)
        );
        // The origin's angle is 0, as the positive x axis's is.
        assert_eq!(init("var a := atan2{y: 0e0, x: 0e0};", "a"), r(0.0));
        // A negative number has no square root.
        let source = format!("var a := sqrt{{of: 1e0 - 5e0}};{MAIN}");
        let errors = errors(&source);
        assert_eq!(errors.len(), 1, "{errors:#?}");
        assert_eq!(errors[0].code, Code::NoResult);
        assert_eq!(errors[0].message, "-4e0 has no square root");
        assert_eq!(
            &source[errors[0].span().start..errors[0].span().end],
            "sqrt{of: 1e0 - 5e0}"
        );
    }

    #[test]
    fn constants_are_called() {
        // With the braces of a call, and as other languages write it: in
        // another case, as C does, and with its library.
        for written in [
            "pi{}", "PI", "Pi", "M_PI", "math.pi", "Math.PI", "math.Pi", "math::pi", "Math::PI",
        ] {
            let source = format!("var a := 2e0 * {written};{MAIN}");
            let errors = errors(&source);
            assert_eq!(errors.len(), 1, "{errors:#?}");
            let fixed = fix(&source);
            assert_eq!(fixed, format!("var a := 2e0 * pi;{MAIN}"), "{written}");
            assert!(analyze_str(&fixed).is_ok(), "{fixed}");
        }
        // With its unit's name, the same.
        let source = format!("var a := 2e0 * trendy.prelude::pi{{}};{MAIN}");
        assert_eq!(
            fix(&source),
            format!("var a := 2e0 * trendy.prelude::pi;{MAIN}")
        );
        assert!(analyze_str(&format!("var a := 2e0 * trendy.prelude::pi;{MAIN}")).is_ok());
        // Not an operator with parameters, whose arguments the fix can't
        // know, nor a library's other members.
        for written in ["sqrt", "SQRT", "math.sqrt", "math.e"] {
            let source = format!("var a := {written};{MAIN}");
            let errors = errors(&source);
            assert!(errors[0].suggestions.is_empty(), "{written}: {errors:#?}");
        }
        let source = format!("var a := sqrt;{MAIN}");
        assert_eq!(
            errors(&source)[0].help,
            ["call it with its arguments: the parameters are `sqrt{of: approximate}`"]
        );
        // An update operator is called by a statement.
        let source = format!("var a := main;{MAIN}");
        assert_eq!(
            errors(&source)[0].help,
            ["to invoke it, write a statement: `call main{…};`"]
        );
    }

    #[test]
    fn other_languages_mathematics() {
        // Names of the prelude's operators.
        for (written, ours) in [("sine", "sin"), ("cosine", "cos"), ("arctan2", "atan2")] {
            let args = if ours == "atan2" {
                "y: 1e0, x: 1e0"
            } else {
                "of: 1e0"
            };
            let source = format!("var a := {written}{{{args}}};{MAIN}");
            let fixed = fix(&source);
            assert_eq!(
                fixed,
                format!("var a := {ours}{{{args}}};{MAIN}"),
                "{written}"
            );
            assert!(analyze_str(&fixed).is_ok(), "{fixed}");
        }
        // Operators the prelude hasn't, which are no misspellings of those
        // it has, as `log` is close to `cos`, `asin` to `sin` and `atan` to
        // `atan2`, and Pascal's `sqr` squares where BASIC's is a root.
        for (written, help) in [
            ("tan", "sin{of: x} / cos{of: x}"),
            ("atan", "atan2{y: x, x: 1e0}"),
            ("abs", "if x < 0e0 then -x else x"),
            ("sqr", "`x * x` is the square"),
            ("log", "an operator of the program's own"),
            ("asin", "an operator of the program's own"),
        ] {
            let source = format!("var a := {written}{{of: 1e0}};{MAIN}");
            let errors = errors(&source);
            assert_eq!(errors[0].code, Code::UnknownName, "{written}");
            assert!(errors[0].suggestions.is_empty(), "{written}: {errors:#?}");
            assert!(errors[0].help[0].contains(help), "{written}: {errors:#?}");
        }
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
    fn only_a_scalar_operand_is_told_values_never_convert() {
        let conversion = "values never convert to another type implicitly";
        // A tuple, a relation or an array is told what it has instead.
        for (operation, rule) in [
            ("tuple { x: 1 } < tuple { x: 2 }", "tuples have no order"),
            (
                "array [tuple { x: 1 }] < array [tuple { x: 2 }]",
                "an array is ordered only if its elements are",
            ),
            (
                "array [1] + array [2]",
                "`+` applies to scalar values, such as an array's elements, `a[i]`",
            ),
            ("-array [1]", "`-` applies to scalar values"),
            (
                "tuple { x: 1 } * tuple { x: 1 }",
                "such as a tuple's attributes, `t.a`",
            ),
            (
                "relation { { x: 1 } } + relation { { x: 1 } }",
                "such as a relation's attributes in `where` or `extend`",
            ),
            // Other languages convert these to what the operator takes.
            ("1 || \" people\"", conversion),
            ("true + 1", conversion),
            ("1 / 2", conversion),
        ] {
            let found = errors(&format!("var a := {operation};{MAIN}"));
            assert_eq!(found.len(), 1, "{operation}: {found:#?}");
            assert_eq!(found[0].code, Code::WrongOperandType, "{operation}");
            let notes = &found[0].notes;
            assert!(
                notes.iter().any(|n| n.contains(rule)),
                "{operation}: {notes:#?}"
            );
            assert_eq!(
                notes.iter().any(|n| n == conversion),
                rule == conversion,
                "{operation}: {notes:#?}"
            );
        }
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
            "var a := false and 1 div 0 = 1;\nvar b := 1 div 0 = 1 and false;\nvar c := true or 1 div 0 = 1;\nvar d := false implies 1 div 0 = 1;\nvar e := 1 div 0 = 1 implies true;\nvar g := and{1 div 0 = 1, true, false};\nvar h := or{1 div 0 = 1, 1 div 0 = 2, true};\nvar points := relation { { x: 0, y: 5 }, { x: 2, y: 6 } };\nvar steep := points where y div x > 1 and x <> 0;\nvar masked := points where y div x > 1 and (x and 0xFF) <> 0;\nvar decided := points where y div x > 1 and x + 1 > 1;\nvar expected := relation { { x: 2, y: 6 } };",
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
        // A false operand decides it wherever it is written, and the bits of an
        // integer can't fail, so a comparison of them is computed first.
        assert_eq!(value("steep"), value("expected"));
        assert_eq!(value("masked"), value("expected"));
        // One that can fail, and is false, decides it too.
        assert_eq!(value("decided"), value("expected"));
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
        // One known only when running decides it there: the `and` stays, of both.
        let program = analyze_str(
            "operator f{x: integer, y: integer}: boolean [ y div x > 1 and x <> 0 ];\noperator main{} [];",
        )
        .unwrap();
        let f = program.operators.iter().find(|o| o.name == "f").unwrap();
        let hir::OperatorKind::ReadOnly { value, .. } = &f.kind else {
            panic!("a read-only operator");
        };
        let value = unsited(value.clone());
        assert!(
            matches!(&value, hir::Expr::Connective(hir::Connective::And, operands) if operands.len() == 2),
            "{value:?}"
        );
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
            unsited(value.clone())
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
        let source = format!(
            "var t := \"Kraków\";\nvar a := not t = \"Lagos\" and true;\nvar b := not t || \"!\" < \"x\";{MAIN}"
        );
        let errors = errors(&source);
        assert_eq!(errors.len(), 2, "{errors:#?}");
        assert_eq!(errors[0].code, Code::WrongOperandType);
        assert_eq!(
            errors[0].suggestions[0].message,
            "negate the comparison: `not (t = \"Lagos\")`"
        );
        assert_eq!(
            fix(&source),
            format!(
                "var t := \"Kraków\";\nvar a := not (t = \"Lagos\") and true;\nvar b := not (t || \"!\" < \"x\");{MAIN}"
            )
        );
        // In a body too, and not when `not` isn't the comparison's left side.
        let body =
            "operator f{t: text} [ call debug_value{v: not t = \"Lagos\"}; ];\noperator main{} [];";
        assert_eq!(
            fix(body),
            "operator f{t: text} [ call debug_value{v: not (t = \"Lagos\")}; ];\noperator main{} [];"
        );
        let errors = self::errors(&format!("var a := true = not \"x\";{MAIN}"));
        assert!(
            errors[0]
                .suggestions
                .iter()
                .all(|s| !s.message.starts_with("negate the comparison")),
            "{errors:#?}"
        );
        // Of an integer, either could be meant.
        let source = format!(
            "var n := 1;\nvar a := not n = 0;\nvar b := not n + 1 < 2;\nvar c := (not n) = 0;\nvar d := 0 = not n;{MAIN}"
        );
        let errors = self::errors(&source);
        assert_eq!(
            errors.iter().map(|e| e.code).collect::<Vec<_>>(),
            [Code::BitGrouping; 2],
            "{errors:#?}"
        );
        assert_eq!(
            errors[0].message,
            "`not n = 0` compares the complement of an integer"
        );
        assert_eq!(
            errors[1].help,
            [
                "write `not (n + 1 < 2)` to negate the comparison, or `(not n + 1) < 2` to compare the complement"
            ]
        );
        assert!(errors.iter().all(|e| e.suggestions.is_empty()));
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
        // Of an integer and a boolean, the boolean sets the type.
        assert_eq!(
            errors[0].message,
            "mismatched types: expected `boolean`, found `integer`"
        );
        assert_eq!(
            errors[0].secondary[0].message,
            "expected because of this value"
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
            let [(_, hir::Statement::Assign(assignments))] = &body[..] else {
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
        let [_, (_, hir::Statement::Assign(assignments))] = &body[..] else {
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
        let [_, (_, hir::Statement::Assign(assignments))] = &body[..] else {
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
        let [(_, hir::Statement::Assign(assignments))] = &body[..] else {
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
        let [(_, hir::Statement::Assign(assignments))] = &body[..] else {
            panic!("{body:?}");
        };
        assert_eq!(assignments.len(), 2);
        let source = "var t := tuple { a: 1, b: 2 };\noperator main{} [ assign { update t { a := b }; update t { b := a }; }; ];";
        let body = main_body(source);
        let [(_, hir::Statement::Assign(assignments))] = &body[..] else {
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
            let [(_, hir::Statement::Assign(assignments))] = &body[..] else {
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
        let [(_, hir::Statement::Assign(assignments))] = &body.statements[..] else {
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
            .filter_map(|(_, s)| match s {
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
        let (_, hir::Statement::Call(call)) = &body.statements[0] else {
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

    /// A variable whose type has no default has no value until a statement gives it
    /// one: reading it before then would read whatever is there, with nothing to say so.
    #[test]
    fn a_variable_without_a_default_is_assigned_before_it_is_read() {
        const GIVE: &str = "p := pair{label: \"a\", n: 1};";
        const PAIR: &str = "type pair { { label: text, n: integer } };
operator give{out p: pair} [ p := pair{label: \"a\", n: 1}; ];
operator poke{var p: pair} [ update p { n := 3 }; ];
";
        let body = |statements: &str| {
            format!(
                "{PAIR}operator main{{}} [\n    var c := 1;\n    var p: pair;\n    {statements}\n];\n"
            )
        };
        for good in [
            "",
            "call give{out p}; call debug_value{v: p};",
            "p := pair{label: \"a\", n: 1}; call debug_value{v: p.n};",
            "if c = 1 then [ call give{out p}; ] else [ p := pair{label: \"b\", n: 2}; ]; call debug_value{v: p};",
            "if c = 1 then [ call give{out p}; call debug_value{v: p}; ];",
            "loop [ call give{out p}; c := c + 1; ] while c < 3; call debug_value{v: p};",
            "transaction [ call give{out p}; ]; call debug_value{v: p};",
            // Assigned twice, and passed on once it has a value.
            "call give{out p}; call poke{var p}; call give{out p}; call debug_value{v: p};",
            // Assigned from itself, once it has a value.
            "call give{out p}; p := pair{label: p.label, n: p.n + 1};",
            "call give{out p}; update p { n := 3 };",
        ] {
            let source = body(good);
            assert!(analyze_str(&source).is_ok(), "{good}");
        }
        for (bad, at) in [
            ("call debug_value{v: p};", "p}"),
            ("call debug_value{v: p.n};", "p.n"),
            (
                "const m := [ const a := 1; p.n + a ]; call debug_value{v: m};",
                "p.n",
            ),
            (
                "const r := relation { { k: 1 } } where k = p.n; call debug_value{v: r};",
                "p.n",
            ),
            ("p := pair{label: \"a\", n: p.n};", "p.n"),
            ("update p { n := 3 };", "p {"),
            ("call poke{var p};", "p}"),
            (
                "if c = 1 then [ call give{out p}; ]; call debug_value{v: p};",
                "p};",
            ),
            (
                "if c = 1 then [ call give{out p}; ] else [ c := 2; ]; call debug_value{v: p};",
                "p};",
            ),
            (
                "while c < 3 loop [ call give{out p}; c := c + 1; ]; call debug_value{v: p};",
                "p};",
            ),
            (
                "for x in array [1] loop [ call give{out p}; ]; call debug_value{v: p};",
                "p};",
            ),
        ] {
            let source = body(bad);
            let found = errors(&source);
            assert_eq!(
                found
                    .iter()
                    .map(|e| (e.code, e.message.as_str()))
                    .collect::<Vec<_>>(),
                [(
                    Code::Unassigned,
                    "the variable `p` is read before it is assigned"
                )],
                "{bad}"
            );
            let last = source.rfind(at).unwrap();
            let first = source.find(bad).unwrap() + bad.find(at).unwrap();
            assert!(
                (first..=last).contains(&found[0].span().start),
                "{bad}: {:?}",
                found[0].span()
            );
            assert_eq!(
                found[0].secondary[0].message, "declared without a value",
                "{bad}"
            );
            assert!(found[0].help[0].contains("assign `p`"), "{bad}");
        }
        // A transaction that collects its errors may undo what it assigned.
        let collected = body(&format!(
            "var e: relation transaction_collection; transaction collecting e [ {GIVE} ]; call debug_value{{v: p}};"
        ));
        assert_eq!(codes(&collected), [Code::Unassigned]);
        // A type with a default starts at it, and can be read at once.
        let integer = "operator main{} [ var n: integer; call debug_value{v: n}; ];";
        assert!(analyze_str(integer).is_ok());
        // Of a type parameter's too, an update operator's.
        let generic = "operator f{v: T, out r: T} types { T: any type } [ var w: T; w := v; r := w; ];\noperator main{} [];";
        assert!(analyze_str(generic).is_ok(), "{generic}");
        let generic = "operator f{v: T, out r: T} types { T: any type } [ var w: T; r := w; ];\noperator main{} [];";
        assert_eq!(codes(generic), [Code::Unassigned]);
        // A variable of the program's has nothing to go by.
        let global = format!("{PAIR}var p: pair;{MAIN}");
        assert_eq!(codes(&global), [Code::UntypedVariable]);
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
                    Code::Unassigned,
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
        let values: Vec<_> = own_variables(&program).map(|v| v.init.clone()).collect();
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
        let (_, hir::Statement::Call(call)) = &body[0] else {
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
        // Their errors point at their declarations in the prelude, which no
        // fix changes, as it is part of the compiler.
        let source = format!("{PLOTS}var x := some{{r: plots, of: 1}};{MAIN}");
        let errors = errors(&source);
        assert_eq!(errors.len(), 1, "{errors:#?}");
        assert_eq!(errors[0].code, Code::TypeMismatch);
        assert_eq!(in_prelude(&source, errors[0].secondary[0].span), "boolean");
        assert_eq!(errors[0].suggestions.len(), 1, "{errors:#?}");
    }

    #[test]
    fn a_units_own_names_hide_the_preludes() {
        // An operator of a prelude operator's name and parameters' names,
        // built in or written in trendy, is the unit's own, which its
        // calls call, and each is named by its unit in the program.
        let units = |main: &str| {
            analyze_units(&[
                ("program", main),
                (
                    "tally",
                    "interface\n\noperator tallied{r: any relation}: integer;\n\nimplementation\n\noperator tallied{r: any relation}: integer [ count{r: r} ];",
                ),
            ])
        };
        let program = units("uses { tally };\n\noperator count{r: any relation}: integer [ 0 ];\noperator some{r: any relation, of: boolean per r}: boolean [ true ];\nvar r := relation { { x: 1 }, { x: 2 } };\nvar mine := count{r: r};\nvar theirs := tallied{r: r};\nvar any := some{r: r, of: x > 5};\n\noperator main{} [];").unwrap();
        let values: Vec<String> = own_variables(&program)
            .map(|v| v.init.to_string())
            .collect();
        assert_eq!(values[1..], ["0", "2", "true"]);
        let names: BTreeSet<&str> = program
            .operators
            .iter()
            .map(|o| o.name.as_str())
            .filter(|n| n.ends_with("count") || n.ends_with("some"))
            .collect();
        assert_eq!(
            names,
            BTreeSet::from([
                "program::count",
                "program::some",
                "trendy.prelude::count",
                "trendy.prelude::some"
            ])
        );
        // A type of a prelude operator's name, whose selector overloads it.
        let program = units("type count { { n: integer } };\nvar c := count{n: 1};\nvar n := count{r: relation { { x: 1 } }};\n\noperator main{} [];").unwrap();
        let values: Vec<String> = own_variables(&program)
            .map(|v| v.init.to_string())
            .collect();
        assert_eq!(values, ["count{n: 1}", "1"]);
        // A type of a prelude type's name, which the prelude's operators
        // still take and give.
        let program = units("type code_point { { n: integer } };\nvar mine := code_point{n: 1};\nvar theirs := to_code_points{from: \"a\"}[1].value;\n\noperator main{} [];").unwrap();
        assert_eq!(program.variables[0].ty.to_string(), "program::code_point");
        assert_eq!(program.variables[1].init.to_string(), "97");
        // Its other representation's selector is still the prelude's, under
        // its name for it.
        let program = units("type code_point { { n: integer } };\nvar theirs := trendy.prelude::code_point{character: 'a'}.value;\n\noperator main{} [];").unwrap();
        assert_eq!(program.variables[0].init.to_string(), "97");
        // The prelude's union type `error` and its heading
        // `transaction_collection`, which the language's failures and
        // transactions are of, whatever a unit declares of their names.
        let shadowing = "type error { { code: integer } };\nheading transaction_collection := { n: integer };\nvar e := error{code: 1};\nvar m := message{error: failure{message: \"no\"}};\n";
        let program = units(&format!("{shadowing}operator main{{}} [];")).unwrap();
        assert_eq!(program.variables[0].ty.to_string(), "program::error");
        assert_eq!(program.variables[1].init.to_string(), "\"no\"");
        assert_eq!(program.library.error.name, "trendy.prelude::error");
        let failing = units(&format!(
            "{shadowing}operator main{{}} [\n    var c: relation {{ error: integer }};\n    transaction collecting d [\n        fail failure{{message: \"no\"}};\n    ];\n];"
        ))
        .unwrap_err();
        assert_eq!(failing.len(), 1, "{failing:#?}");
        assert_eq!(failing[0].code, Code::UnknownName);
        // Its fix declares `d` of the prelude's heading, which the unit
        // hides by its own.
        assert_eq!(
            failing[0].suggestions[0].edits[0].replacement,
            "var d: relation trendy.prelude::transaction_collection;\n    "
        );
        // Another unit's and the prelude's, both other units', clash where
        // the name is written.
        let errors = analyze_units(&[
            ("program", "uses { tally };\n\nvar n := count{r: relation { { x: 1 } }};\n\noperator main{} [];"),
            ("tally", "interface\n\noperator count{r: any relation}: integer;\n\nimplementation\n\noperator count{r: any relation}: integer [ 0 ];"),
        ])
        .unwrap_err();
        assert_eq!(
            errors.iter().map(|e| e.code).collect::<Vec<_>>(),
            [Code::NameClash],
            "{errors:#?}"
        );
        assert_eq!(
            errors[0].message,
            "`count{r}` is declared by the prelude, which every unit sees, and by `tally`, which this unit uses"
        );
        assert_eq!(errors[0].secondary.len(), 2, "{errors:#?}");
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
            [Code::BuiltinInProgram]
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
    fn other_languages_ways_with_a_series_are_fixed() {
        let in_main = |call: &str| {
            format!(
                "operator f{{low: integer, high: integer}} [ call debug_value{{v: {call}}}; ];{MAIN}"
            )
        };
        for (written, ours) in [
            // PostgreSQL's, whose arguments' order says which each is, in
            // any case, whether they look like values or a projection's
            // attributes, or are named already.
            (
                "generate_series{1, 9, 2}",
                "series{from: 1, to: 9, step: 2}",
            ),
            ("GENERATE_SERIES{1, 9}", "series{from: 1, to: 9}"),
            ("generate_series{low, high}", "series{from: low, to: high}"),
            ("generate_series{from: 1, to: 9}", "series{from: 1, to: 9}"),
            // Its parameters' names, and R's for the step, which chooses
            // the operator with one.
            ("series{start: 1, stop: 9}", "series{from: 1, to: 9}"),
            (
                "series{start: 1, stop: 9, step: 2}",
                "series{from: 1, to: 9, step: 2}",
            ),
            (
                "series{from: 1, to: 9, by: 2}",
                "series{from: 1, to: 9, step: 2}",
            ),
        ] {
            let fixed = fix(&in_main(written));
            assert_eq!(fixed, in_main(ours), "{written}");
            assert!(analyze_str(&fixed).is_ok(), "{fixed}");
        }
        // Nothing says which of its own a call's unnamed arguments are, nor
        // how many of another operator's there are.
        assert!(
            errors(&in_main("series{1, 9}"))
                .iter()
                .all(|d| d.suggestions.is_empty())
        );
        assert_eq!(
            fix(&in_main("generate_series{1, 2, 3, 4}")),
            in_main("series{1, 2, 3, 4}")
        );
        // Python's leaves its end out, so that what to write depends on the
        // values, and it isn't a misspelling of anything.
        let range = errors(&in_main("range{1, 9}"));
        assert_eq!(range.len(), 1, "{range:#?}");
        assert_eq!(range[0].code, Code::UnknownName);
        assert!(range[0].suggestions.is_empty(), "{range:#?}");
        assert!(
            range[0]
                .help
                .iter()
                .any(|h| h.contains("series{from: a, to: b - 1}")),
            "{range:#?}"
        );
        // A program's `series` is its own, with parameters of its own.
        let own = "operator series{a: integer, step: integer}: integer [ a + step ];\n";
        let fixed = fix(&format!("{own}var x := series{{a: 1, by: 2}};{MAIN}"));
        assert!(!fixed.contains("step: 2"), "{fixed}");
    }

    #[test]
    fn a_name_alone_is_its_own_value() {
        let area = "operator area{width: integer, height: integer}: integer [ width * height ];\noperator area{side: integer}: integer [ side * side ];\n";
        for body in [
            // Arguments, in any order, with others, choosing the operator.
            "area{width, height}",
            "area{height, width}",
            "area{width, height: 2}",
            "area{width: 2, height}",
            "area{side: width}",
            // Tuples, with or without their keyword.
            "count{r: relation { { width, height } }}",
            "tuple { width, height }.width",
            "length{a: array [{ width, height: 1 }]}",
        ] {
            let source = format!(
                "{area}operator f{{width: integer, height: integer}}: integer [ {body} ];{MAIN}"
            );
            assert!(
                analyze_str(&source).is_ok(),
                "{body}: {:#?}",
                errors(&source)
            );
        }
        // What it names is any value in scope: a local variable, an attribute
        // of the tuple at hand.
        let source = format!(
            "{area}operator f{{}}: integer [ var side := 2; area{{side}} ];\nvar r := relation {{ {{ side: 1 }} }} extend {{ a := area{{side}} }};{MAIN}"
        );
        assert!(analyze_str(&source).is_ok(), "{:#?}", errors(&source));
        // In an update call, with its mode.
        let source = "operator g{a: integer, out b: integer, var c: integer} [ b := a; c := a; ];\noperator main{} [ var a := 1; var b := 0; var c := 0; call g{a, out b, var c}; ];";
        assert!(analyze_str(source).is_ok(), "{:#?}", errors(source));
        // Its mode is still needed, and the fix adds it.
        assert_eq!(
            fix(
                "operator g{out b: integer} [ b := 1; ];\noperator main{} [ var b := 0; call g{b}; ];"
            ),
            "operator g{out b: integer} [ b := 1; ];\noperator main{} [ var b := 0; call g{out b}; ];"
        );
        // It means what is written out: the same in the program.
        let written = |args: &str| {
            let source = format!(
                "operator f{{a: integer}}: integer [ a ];\nvar a := 1;\nvar x := f{{{args}}};"
            );
            variables(&source)[1].init.clone()
        };
        assert_eq!(written("a"), written("a: a"));
        // A value of the name has to exist.
        let source = format!("operator f{{a: integer}}: integer [ a ];\nvar x := f{{a}};{MAIN}");
        let unknown = errors(&source);
        assert_eq!(unknown[0].code, Code::UnknownName);
        assert!(
            unknown[0].notes.iter().any(|n| n.contains("its own value")),
            "{unknown:#?}"
        );
        let source = format!("var x := tuple {{ a }};{MAIN}");
        assert_eq!(codes(&source), [Code::UnknownName]);
        // Twice is twice.
        let source = format!("var a := 1;\nvar x := tuple {{ a, a: 2 }};{MAIN}");
        assert_eq!(codes(&source), [Code::DuplicateAttribute]);
    }

    #[test]
    fn a_name_alone_that_is_no_parameter() {
        let in_f = |call: &str| {
            format!(
                "operator f{{w: integer, h: integer, side: integer}}: integer [ {call} ];{MAIN}"
            )
        };
        let area = "operator area{width: integer, height: integer}: integer [ width * height ];\n";
        // One left over can only be for the one parameter left.
        assert_eq!(
            fix(&format!("{area}{}", in_f("area{width: 1, h}"))),
            format!("{area}{}", in_f("area{width: 1, height: h}"))
        );
        assert_eq!(fix(&in_f("count{w}")), in_f("count{r: w}"));
        // Several, nothing says which is for which.
        let source = format!("{area}{}", in_f("area{w, h}"));
        let nameless = errors(&source);
        assert_eq!(nameless.len(), 2, "{nameless:#?}");
        assert!(nameless.iter().all(|d| d.code == Code::PositionalArgument));
        assert!(
            nameless.iter().all(|d| d.suggestions.is_empty()),
            "{nameless:#?}"
        );
        // No value of the name, it may be a misspelling of the parameter's.
        let source = format!(
            "{area}operator f{{width: integer, height: integer}}: integer [ area{{width, heigth}} ];{MAIN}"
        );
        assert_eq!(
            fix(&source),
            format!(
                "{area}operator f{{width: integer, height: integer}}: integer [ area{{width, height}} ];{MAIN}"
            )
        );
        // Another language's name for one keeps its value.
        assert_eq!(
            fix(&in_f("count{r: series{start: w, stop: h}}")),
            in_f("count{r: series{from: w, to: h}}")
        );
        let source = "operator f{start: integer, stop: integer}: integer [ count{r: series{start, stop}} ];\noperator main{} [];";
        assert_eq!(
            fix(source),
            "operator f{start: integer, stop: integer}: integer [ count{r: series{from: start, to: stop}} ];\noperator main{} [];"
        );
        // A program's own operator has no other language's names.
        let source = "operator g{from: integer}: integer [ from ];\noperator f{start: integer, other: integer}: integer [ g{start, other} ];\noperator main{} [];";
        assert!(
            errors(source).iter().all(|d| d.suggestions.is_empty()),
            "{:#?}",
            errors(source)
        );
    }

    #[test]
    fn names_alone_that_could_be_a_projection_or_a_call() {
        let both = "var t := relation { { y: 1, r: 2 } };\noperator t{y: integer, r: integer}: integer [ y ];\n";
        // In an update operator, as a read-only one reads no variable.
        let in_f = |call: &str| {
            format!(
                "{both}operator f{{y: integer, r: integer}} [ call debug_value{{v: {call}}}; ];{MAIN}"
            )
        };
        // Both fit, so nothing says which.
        let source = in_f("t{y, r}");
        assert_eq!(codes(&source), [Code::ProjectionOrCall]);
        let ambiguous = &errors(&source)[0];
        assert!(ambiguous.suggestions.is_empty(), "{ambiguous:#?}");
        assert_eq!(ambiguous.help.len(), 2);
        // Each way of saying which is fine.
        for call in ["t{y: y, r: r}", "(t){y, r}"] {
            let source = in_f(call);
            assert!(
                analyze_str(&source).is_ok(),
                "{call}: {:#?}",
                errors(&source)
            );
        }
        // Not all its parameters: a projection, of what it can't be a call of.
        let source = format!("{both}var x := t{{y}};{MAIN}");
        assert!(analyze_str(&source).is_ok(), "{:#?}", errors(&source));
        // No values of those names: a projection.
        let source = format!("{both}var x := t{{y, r}};{MAIN}");
        assert!(analyze_str(&source).is_ok(), "{:#?}", errors(&source));
        // No value of its name: a call, the prelude's operators too.
        let source = "var r := relation { { x: 1 } };\nvar c := count{r};\noperator main{} [];";
        assert!(analyze_str(source).is_ok(), "{:#?}", errors(source));
        // A program's value hides the prelude's name, where it can.
        let source = "var series := relation { { from: 1, other: 2 } };\nvar x := series{from};\noperator main{} [];";
        assert!(analyze_str(source).is_ok(), "{:#?}", errors(source));
        // A value that can't be projected so: the fix calls the operator.
        let source = "operator f{r: relation { x: integer }}: integer [ var count := 1; count{r} ];\noperator main{} [];";
        assert_eq!(codes(source), [Code::WrongOperandType]);
        assert_eq!(
            fix(source),
            "operator f{r: relation { x: integer }}: integer [ var count := 1; count{r: r} ];\noperator main{} [];"
        );
        let source = "var g := relation { { x: 1 } };\noperator g{y: integer}: integer [ y ];\noperator f{y: integer}: integer [ count{r: g{y}} ];\noperator main{} [];";
        assert_eq!(
            fix(source),
            "var g := relation { { x: 1 } };\noperator g{y: integer}: integer [ y ];\noperator f{y: integer}: integer [ count{r: g{y: y}} ];\noperator main{} [];"
        );
    }

    #[test]
    fn a_constraint_in_an_operators_braces_is_checked_of_what_is_known() {
        let half =
            "operator half{n: integer, m: integer, constraint n >= 0}: integer [ n div 2 + m ];\n";
        for (written, breaks) in [
            ("var x := half{n: 2, m: 0};", false),
            ("var x := half{n: -1, m: 0};", true),
            // Through a call whose value is computed.
            (
                "operator twice{n: integer}: integer [ half{n: n, m: 0} * 2 ];\nvar x := twice{n: -1};",
                true,
            ),
            // A parameter's isn't known, but whatever it reads is.
            (
                "operator g{m: integer}: integer [ half{n: 2, m: m} ];",
                false,
            ),
            (
                "operator g{m: integer}: integer [ half{n: -1, m: m} ];",
                true,
            ),
            (
                "operator g{n: integer}: integer [ half{n: n, m: 0} ];",
                false,
            ),
        ] {
            let source = format!("{half}{written}{MAIN}");
            if breaks {
                assert_eq!(codes(&source), [Code::BrokenCallConstraint], "{written}");
            } else {
                assert!(
                    analyze_str(&source).is_ok(),
                    "{written}: {:#?}",
                    errors(&source)
                );
            }
        }
        // It says what breaks what, and where each is.
        let source = format!("{half}var x := half{{n: -1, m: 0}};{MAIN}");
        let broken = &errors(&source)[0];
        assert_eq!(broken.message, "`half{n: -1}` can't be called");
        assert_eq!(broken.secondary.len(), 1);
        // So does an update operator's.
        let down = "operator down{n: integer, constraint n > 0} [ call debug_value{v: n}; ];\n";
        for (args, code) in [("n: 1", None), ("n: 0", Some(Code::BrokenCallConstraint))] {
            let source = format!("{down}operator main{{}} [ call down{{{args}}}; ];");
            match code {
                Some(code) => assert_eq!(codes(&source), [code]),
                None => assert!(analyze_str(&source).is_ok(), "{:#?}", errors(&source)),
            }
        }
        // `var` parameters can be read, as the variable is when the call
        // starts, which isn't known when compiling.
        let source = "operator f{var a: integer, constraint a > 0} [ a := 1; ];\noperator main{} [ var x := 0; call f{var a: x}; ];";
        assert!(analyze_str(source).is_ok(), "{:#?}", errors(source));
        // Generic, and in any order in the braces.
        let source = format!(
            "operator f{{constraint length{{a: a}} > 0, a: array E}}: E types {{ E: any type }} [ a[1] ];\nvar x := f{{a: array [1]}};{MAIN}"
        );
        assert!(analyze_str(&source).is_ok(), "{:#?}", errors(&source));
    }

    #[test]
    fn a_constraint_in_an_operators_braces_says_what_arguments_it_takes() {
        for (source, code) in [
            // It reads a parameter that has a value when the call is made.
            (
                "operator f{n: integer, constraint 1 > 0}: integer [ n ];",
                Code::CallConstraint,
            ),
            (
                "operator f{} [];\noperator g{constraint 1 > 0} [];",
                Code::CallConstraint,
            ),
            (
                "operator f{n: integer, out m: integer, constraint m > n} [ m := n; ];",
                Code::CallConstraint,
            ),
            (
                "operator f{r: any relation, of: boolean per r, constraint of}: boolean [ true ];",
                Code::CallConstraint,
            ),
            // And nothing of the program's.
            (
                "var z := 1;\noperator f{n: integer, constraint n > z}: integer [ n ];",
                Code::VariableInReadOnly,
            ),
            // A boolean condition, that depends on them.
            (
                "operator f{n: integer, constraint n}: integer [ n ];",
                Code::TypeMismatch,
            ),
            (
                "operator f{n: integer, constraint false and n > 0}: integer [ n ];",
                Code::CallConstraint,
            ),
            // Not calling its own operator, even through another.
            (
                "operator f{n: integer, constraint f{n: n} > 0}: integer [ n ];",
                Code::ConstraintCycle,
            ),
            (
                "operator f{n: integer, constraint g{n: n} > 0}: integer [ n ];\noperator g{n: integer}: integer [ f{n: n} ];",
                Code::ConstraintCycle,
            ),
            // A version has none of its own.
            (
                "type t union;\ntype a is t { { x: integer } };\ntype b is t { { y: integer } };\noperator f{v: t}: integer;\noperator f{v: a, constraint v.x > 0}: integer [ v.x ];\noperator f{v: b}: integer [ v.y ];",
                Code::CallConstraint,
            ),
        ] {
            let source = format!("{source}{MAIN}");
            assert_eq!(codes(&source), [code], "{source}");
        }
        // A misspelled parameter is one, not a condition that reads none.
        assert_eq!(
            fix(&format!(
                "operator f{{step: integer, constraint stp <> 0}}: integer [ step ];{MAIN}"
            )),
            format!("operator f{{step: integer, constraint step <> 0}}: integer [ step ];{MAIN}")
        );
        // One that is always true is removed.
        assert_eq!(
            fix(&format!(
                "operator f{{n: integer, constraint true or n > 0}}: integer [ n ];{MAIN}"
            )),
            format!("operator f{{n: integer}}: integer [ n ];{MAIN}")
        );
    }

    #[test]
    fn a_constraint_goes_in_the_declaration_its_callers_see() {
        // The fix moves it there, or removes it if it says the same.
        let unit = |declared: &str, defined: &str| {
            format!(
                "interface\n\noperator f{{{declared}}}: integer;\n\nimplementation\n\noperator f{{{defined}}}: integer [ n ];\n\noperator main{{}} [];\n"
            )
        };
        let source = unit("n: integer", "n: integer, constraint n > 0");
        assert_eq!(codes(&source), [Code::CallConstraint]);
        let fixed = fix(&source);
        assert_eq!(fixed, unit("n: integer, constraint n > 0", "n: integer"));
        assert!(analyze_str(&fixed).is_ok(), "{fixed}");
        let source = unit(
            "n: integer, constraint n > 0",
            "n: integer, constraint n > 0",
        );
        assert_eq!(
            fix(&source),
            unit("n: integer, constraint n > 0", "n: integer")
        );
        // Where it is, it is checked.
        let source = format!(
            "{}var x := f{{n: 0}};\n",
            unit("n: integer, constraint n > 0", "n: integer")
        );
        assert_eq!(codes(&source), [Code::BrokenCallConstraint]);
    }

    #[test]
    fn a_series_is_the_preludes_to_call() {
        // It has a loop, so an initial value can't wait for it.
        assert_eq!(
            codes(&format!("var digits := series{{from: 0, to: 9}};{MAIN}")),
            [Code::InitialValueWhenRunning]
        );
        // Its parts are the implementation's own: not seen, and no clash.
        assert_eq!(
            codes(&format!("var x := past{{at: 1, to: 2, step: 1}};{MAIN}")),
            [Code::UnknownName]
        );
        let own = format!(
            "operator past{{n: integer}}: integer [ n ];\noperator last_step{{n: integer}}: integer [ n ];\nvar x := past{{n: 1}} + last_step{{n: 1}};\noperator f{{}}: relation {{ value: integer }} [ series{{from: 1, to: 2}} ];{MAIN}"
        );
        assert!(analyze_str(&own).is_ok(), "{:#?}", errors(&own));
    }

    #[test]
    fn other_languages_names_for_writing_are_fixed() {
        // R's and Kotlin's, Python's, .NET's, Node's and Go's, and the names
        // that follow from `load_`'s being `read_` elsewhere, fixed to
        // `save_text_file`, from `trendy.files`, which the unit then uses.
        for written in [
            "writeLines",
            "writelines",
            "WriteAllLines",
            "writeFile",
            "WriteFile",
            "write_text_file",
            "write_file",
            "write_lines",
            "save_file",
        ] {
            let source = format!(
                "operator main{{}} [\n    call {written}{{path: \"a.txt\", lines: array [\"Nile\"]}};\n];\n"
            );
            // Then the path, a text, is selected as one.
            let fixed = fix(&fix(&source));
            assert_eq!(
                fixed,
                "uses { trendy.files };\n\noperator main{} [\n    call save_text_file{path: file_path{text: \"a.txt\"}, lines: array [\"Nile\"]};\n];\n",
                "{written}"
            );
            assert!(analyze_str(&fixed).is_ok(), "{fixed}");
        }
    }

    #[test]
    fn a_path_is_a_file_path() {
        // Selected from its text, which isn't empty and has no U+0000, as no
        // system's path can be or have.
        let files = "uses { trendy.files };\n";
        assert!(
            analyze_str(&format!(
                "{files}var p := file_path{{text: \"rivers.txt\"}};{MAIN}"
            ))
            .is_ok()
        );
        for text in ["", "a\\u{0}b.txt"] {
            let source = format!("{files}var p := file_path{{text: \"{text}\"}};{MAIN}");
            assert_eq!(codes(&source), [Code::BrokenConstraint], "{text:?}");
        }
        // A text where one goes is selected, a name alone keeping its name, as
        // the conversion and the array of one do.
        let source = format!(
            "{files}\noperator read{{path: text}} [\n    var lines: array text;\n    call load_text_file{{path, out lines}};\n    call load_text_file{{path: \"rivers.txt\", out lines}};\n];\n{MAIN}"
        );
        let fixed = fix(&source);
        assert!(
            fixed.contains("call load_text_file{path: file_path{text: path}, out lines};"),
            "{fixed}"
        );
        assert!(
            fixed
                .contains("call load_text_file{path: file_path{text: \"rivers.txt\"}, out lines};"),
            "{fixed}"
        );
        assert!(analyze_str(&fixed).is_ok(), "{fixed}");
        let source = "uses { trendy.streams };\n\noperator say{line: integer, lines: text} [\n    call write{line};\n    call write{lines};\n];\n\noperator main{} [];\n";
        let fixed = fix(source);
        assert!(
            fixed.contains("call write{line: to_text{from: line}};"),
            "{fixed}"
        );
        assert!(
            fixed.contains("call write{lines: array [lines]};"),
            "{fixed}"
        );
        assert!(analyze_str(&fixed).is_ok(), "{fixed}");
    }

    #[test]
    fn other_languages_reads_of_a_line_say_how_to_read_one() {
        // Python's, Ruby's and C's, Pascal's, Node's and Kotlin's, Rust's, C's and
        // Java's, which return the line, or take a prompt, explained.
        for written in [
            "input",
            "gets",
            "readln",
            "readLine",
            "read_line",
            "getline",
            "nextLine",
        ] {
            let found = errors(&format!(
                "operator main{{}} [\n    var name := {written}{{\"Name? \"}};\n];\n"
            ));
            assert_eq!(found.len(), 1, "{written}: {found:#?}");
            assert_eq!(found[0].code, Code::UnknownName, "{written}");
            assert!(
                found[0]
                    .help
                    .iter()
                    .any(|help| help.contains("call read{out line: name};")),
                "{written}: {found:#?}"
            );
        }
        // A transaction reads no stream.
        let found = errors(
            "uses { trendy.streams };\n\noperator main{} [\n    var name: text;\n    transaction [ call read{out line: name}; ];\n];\n",
        );
        let said: Vec<&str> = found.iter().map(|e| e.message.as_str()).collect();
        assert_eq!(said, ["`read` reads a stream, in a transaction"]);
    }

    #[test]
    fn other_languages_names_for_printing_are_fixed() {
        // Python's, Java's and Rust's, Ruby's and C's, Pascal's and the shell's,
        // in any case, fixed to `write` of a line, from `trendy.streams`, which
        // the unit then uses.
        for written in ["print", "println", "PrintLn", "puts", "writeln", "echo"] {
            let source = format!("operator main{{}} [\n    call {written}{{\"Habari\"}};\n];\n");
            let fixed = fix(&source);
            assert_eq!(
                fixed,
                "uses { trendy.streams };\n\noperator main{} [\n    call write{line: \"Habari\"};\n];\n",
                "{written}"
            );
            assert!(analyze_str(&fixed).is_ok(), "{fixed}");
        }
        // Rust's to standard error, which the fix names too.
        for (written, param) in [("eprintln", "line"), ("eprint", "text")] {
            let source = format!("operator main{{}} [\n    call {written}{{\"oops\"}};\n];\n");
            let fixed = fix(&source);
            assert_eq!(
                fixed,
                format!(
                    "uses {{ trendy.streams }};\n\noperator main{{}} [\n    call write{{{param}: \"oops\", to: standard_error}};\n];\n"
                ),
                "{written}"
            );
            assert!(analyze_str(&fixed).is_ok(), "{fixed}");
        }
        // C's, Ruby's and Python's names for the streams themselves.
        for (written, ours) in [("stderr", "standard_error"), ("STDOUT", "standard_output")] {
            let source = format!(
                "uses {{ trendy.streams }};\n\noperator main{{}} [\n    call write{{line: \"oops\", to: {written}}};\n];\n"
            );
            let fixed = fix(&source);
            assert_eq!(fixed, source.replace(written, ours), "{written}");
            assert!(analyze_str(&fixed).is_ok(), "{fixed}");
        }
    }

    #[test]
    fn a_write_is_to_a_stream_outside_the_program() {
        // A transaction writes to none, whichever, as a file is one too.
        let found = errors(
            "uses { trendy.streams, trendy.files };\n\noperator main{} [\n    transaction [ call write{line: \"Habari\"}; ];\n    transaction [ call write{line: \"x\", to: file{path: file_path{text: \"log.txt\"}}}; ];\n];\n",
        );
        let said: Vec<&str> = found.iter().map(|e| e.message.as_str()).collect();
        assert_eq!(said, ["`write` writes to a stream, in a transaction"; 2]);
        assert!(found.iter().all(|e| e.code == Code::IoInTransaction));
        // Standard input can't be written to, as is known when compiling.
        assert_eq!(
            codes(
                "uses { trendy.streams };\n\noperator main{} [\n    call write{line: \"x\", to: standard_input};\n];\n"
            ),
            [Code::BrokenCallConstraint]
        );
        // But any other stream can, by a value of `stream`.
        assert!(
            analyze_str(
                "uses { trendy.streams, trendy.files };\n\noperator say{to: stream} [ call write{line: \"x\", to}; ];\noperator main{} [\n    call say{to: standard_error};\n    call say{to: file{path: file_path{text: \"log.txt\"}}};\n];\n"
            )
            .is_ok()
        );
    }

    #[test]
    fn one_value_where_an_array_goes_is_made_an_array_of_one() {
        let source = "uses { trendy.files };\n\noperator main{} [\n    call save_text_file{path: file_path{text: \"a.txt\"}, lines: \"Nile\"};\n];\n";
        let fixed = fix(source);
        assert_eq!(
            fixed,
            source.replace("lines: \"Nile\"", "lines: array [\"Nile\"]")
        );
        assert!(analyze_str(&fixed).is_ok(), "{fixed}");
    }

    #[test]
    fn a_relation_where_an_array_goes_says_how_to_load_one() {
        // The elements' type says which load: of values, or of tuples.
        for (element, load) in [
            ("text", "`load a value from r order"),
            ("tuple { line: text }", "`load a from r order"),
        ] {
            let source = format!(
                "operator f{{a: array {element}}} [ call debug_value{{v: length{{a}}}}; ];
operator main{{}} [
    const r := relation {{ {{ line: \"Nile\" }} }};
    call f{{a: r}};
];
"
            );
            let errors = errors(&source);
            assert_eq!(errors.len(), 1, "{errors:#?}");
            assert_eq!(errors[0].code, Code::TypeMismatch);
            assert!(
                errors[0].help.iter().any(|help| help.contains(load)),
                "{errors:#?}"
            );
        }
    }

    #[test]
    fn other_languages_names_for_reading_are_fixed() {
        // A file's lines: R's and Kotlin's, Python's and Ruby's, and .NET's,
        // which they return, fixed to `load_text_file` with its argument
        // named, from `trendy.files`, which the unit then uses, then to a
        // call with the variable its `out` argument.
        for written in ["readLines", "readlines", "ReadAllLines"] {
            let source =
                format!("operator main{{}} [\n    var lines := {written}{{\"rivers.txt\"}};\n];\n");
            let fixed = fix(&source);
            assert_eq!(
                fixed,
                "uses { trendy.files };\n\noperator main{} [\n    var lines := load_text_file{path: \"rivers.txt\"};\n];\n",
                "{written}"
            );
            // Then the path, a text, is selected as one.
            let fixed = fix(&fix(&fixed));
            assert_eq!(
                fixed,
                "uses { trendy.files };\n\noperator main{} [\n    var lines: array text;\n    call load_text_file{path: file_path{text: \"rivers.txt\"}, out lines: lines};\n];\n"
            );
            assert!(analyze_str(&fixed).is_ok(), "{fixed}");
        }
        // A unit that uses it already only gets the name.
        let source = "uses { trendy.files };\n\noperator main{} [\n    var lines: array text;\n    call readLines{path: \"rivers.txt\", out lines: lines};\n];\n";
        assert_eq!(fix(source), source.replace("readLines", "load_text_file"));
        // A file's bytes: .NET's and Java's, Python's, Kotlin's and Ruby's,
        // and the name `load_`'s being `read_` elsewhere gives it.
        for written in [
            "ReadAllBytes",
            "readAllBytes",
            "read_bytes",
            "readBytes",
            "binread",
            "read_binary_file",
        ] {
            let source =
                format!("operator main{{}} [\n    var data := {written}{{\"kente.png\"}};\n];\n");
            let fixed = fix(&fix(&fix(&source)));
            assert_eq!(
                fixed,
                "uses { trendy.files };\n\noperator main{} [\n    var data: array byte;\n    call load_binary_file{path: file_path{text: \"kente.png\"}, out bytes: data};\n];\n",
                "{written}"
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
                "uses { trendy.files };\n\noperator main{} [\n    var rows: array tuple { city: text };\n    call load_csv{\"cities.csv\", out rows: rows};\n];\n",
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
        let source = "uses { trendy.files };\n\noperator main{} [\n    var rivers: array text;\n    call load_text_file{path: file_path{text: \"rivers.txt\"}, out lines: rivers};\n];\n";
        let body = main_body(source);
        let (_, hir::Statement::Call(call)) = &body[1] else {
            panic!("{body:#?}");
        };
        assert_eq!(call.updated["lines"], hir::Target::Local(hir::LocalId(0)));
        // As a value, as other languages return one, it is fixed to a call
        // with the variable its `out` argument.
        for (written, fixed) in [
            (
                "var rivers := load_text_file{path: file_path{text: \"rivers.txt\"}};",
                "var rivers: array text;\n    call load_text_file{path: file_path{text: \"rivers.txt\"}, out lines: rivers};",
            ),
            (
                "var rivers: array text;\n    rivers := load_text_file{path: file_path{text: \"rivers.txt\"}};",
                "var rivers: array text;\n    call load_text_file{path: file_path{text: \"rivers.txt\"}, out lines: rivers};",
            ),
        ] {
            let source =
                format!("uses {{ trendy.files }};\n\noperator main{{}} [\n    {written}\n];\n");
            assert_eq!(codes(&source), [Code::OperatorAsValue], "{written}");
            assert_eq!(
                fix(&source),
                format!("uses {{ trendy.files }};\n\noperator main{{}} [\n    {fixed}\n];\n"),
                "{written}"
            );
        }
        // Not where no statement can call it: an initial value, which is
        // computed when compiling, or a read-only operator's value.
        for source in [
            format!(
                "uses {{ trendy.files }};\n\nvar lines := load_text_file{{path: file_path{{text: \"rivers.txt\"}}}};{MAIN}"
            ),
            format!(
                "uses {{ trendy.files }};\n\noperator rivers{{}}: array text [ load_text_file{{path: file_path{{text: \"rivers.txt\"}}}} ];{MAIN}"
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
                "uses {{ trendy.files }};\n\noperator main{{}} [\n    var rows: {declared};\n    call load_csv{{path: file_path{{text: \"cities.csv\"}}, header: true, columns: tuple {{ city: 1 }}, out rows: rows}};\n];\n"
            )
        };
        // Of text, integers, approximate numbers and booleans, read when
        // the program runs, into the variable for `rows`, of the tuples'
        // type.
        let source = call("array tuple { city: text }");
        let body = main_body(&source);
        let (_, hir::Statement::Call(call_of)) = &body[1] else {
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
        let generic = "uses { trendy.files };\n\noperator read{out rows: array T} types { T: any tuple } [\n    call load_csv{path: file_path{text: \"cities.csv\"}, header: true, columns: tuple { city: 1 }, out rows: rows};\n];\noperator main{} [];";
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
                "uses {{ trendy.files }};\n\noperator main{{}} [\n    var rows: array tuple {{ city: text, people: integer }};\n    call load_csv{{path: file_path{{text: \"cities.csv\"}}, header: false, columns: {columns}, out rows: rows}};\n];\n"
            )
        };
        analyze_str(&call("tuple { people: 3, city: 1 }")).unwrap();
        // A position for each attribute, and no other, which the array
        // that sets them is pointed at for.
        let missing = errors(&call("tuple { city: 1 }"));
        assert_eq!(missing[0].code, Code::MissingAttribute);
        assert_eq!(
            missing[0].secondary[0].message,
            "`columns` gives each attribute of these tuples a column, or the columns of its components, in one of its possible representations"
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
        let source = "uses { trendy.files };\n\noperator main{} [\n    var rows: array tuple { city: text };\n    call load_csv{path: file_path{text: \"cities.csv\"}, has_header: true, usecols: tuple { city: 1 }, out rows: rows};\n];\n";
        assert_eq!(
            fix(source),
            "uses { trendy.files };\n\noperator main{} [\n    var rows: array tuple { city: text };\n    call load_csv{path: file_path{text: \"cities.csv\"}, header: true, columns: tuple { city: 1 }, out rows: rows};\n];\n"
        );
    }

    #[test]
    fn a_programs_own_types_are_ordered_by_their_order() {
        let declared =
            "type day { { year: integer, month: integer }, order [asc year, desc month] };
type entry { { on: day, title: text }, order [asc on, asc title] };
";
        let var = |value: &str| format!("{declared}var v := {value};");
        let value = |source: &str| variables(source)[0].init.to_string();
        // Compared by the order, its components in turn, a descending one
        // the other way, and a value inside by its own type's.
        assert_eq!(
            value(&var(
                "day{year: 2026, month: 3} < day{year: 2026, month: 2}"
            )),
            "true"
        );
        assert_eq!(
            value(&var(
                "day{year: 2025, month: 1} >= day{year: 2026, month: 12}"
            )),
            "false"
        );
        assert_eq!(
            value(&var(
                "max{r: relation { { e: entry{on: day{year: 2026, month: 1}, title: \"b\"} }, { e: entry{on: day{year: 2026, month: 2}, title: \"a\"} } }, of: e}"
            )),
            "entry{on: day{month: 1, year: 2026}, title: \"b\"}"
        );
        // Of `any ordered`, so a generic operator's `<` takes its values.
        assert_eq!(
            value(&format!(
                "{declared}operator least{{a: T, b: T}}: T types {{ T: any ordered }} [ if a < b then a else b ];\nvar v := least{{a: day{{year: 2026, month: 1}}, b: day{{year: 2026, month: 5}}}};"
            )),
            "day{month: 5, year: 2026}"
        );
        // Without an order, none of that, with help to give it one.
        let unordered = "type spot { { x: integer } };\nvar s := spot{x: 1};\n";
        for (source, code) in [
            (
                format!("{unordered}var v := s < s;{MAIN}"),
                Code::WrongOperandType,
            ),
            (
                format!("{unordered}var v := min{{r: relation {{ {{ s: s }} }}, of: s}};{MAIN}"),
                Code::TypeMismatch,
            ),
        ] {
            let errors = errors(&source);
            assert_eq!(errors[0].code, code, "{source}");
            assert!(
                errors[0]
                    .help
                    .contains(&"give `spot` an order, `order [asc …]` in its braces, its components in the order they compare".to_string()),
                "{errors:#?}"
            );
        }
    }

    #[test]
    fn a_programs_own_types_order_errors() {
        let declare = |order: &str| {
            format!("type day {{ {{ year: integer, month: integer }}, {order} }};{MAIN}")
        };
        // Every component, once, each of an ordered type, and one order.
        assert_eq!(
            messages(&declare("order [asc year]")),
            ["the order of `day` leaves out `month`"]
        );
        let source = declare("order [asc yaer, asc month]");
        assert_eq!(messages(&source), ["unknown component `yaer`"]);
        assert_eq!(fix(&source), declare("order [asc year, asc month]"));
        let source = declare("order [asc year, asc month, desc year]");
        assert_eq!(codes(&source), [Code::DuplicateAttribute]);
        assert_eq!(fix(&source), declare("order [asc year, asc month]"));
        let source = declare("order [asc year, asc month], order [desc year, desc month]");
        assert_eq!(
            messages(&source),
            ["the order of `day` is given more than once"]
        );
        assert_eq!(fix(&source), declare("order [asc year, asc month]"));
        assert_eq!(codes(&declare("order []")), [Code::EmptyList]);
        assert_eq!(
            messages(&format!(
                "type spot {{ {{ x: integer }} }};\ntype day {{ {{ at: spot }}, order [asc at] }};{MAIN}"
            )),
            ["the order of `day` can't compare `at`, of type `spot`"]
        );
    }

    /// A pair of integers, stored as they are, and ordered by their total,
    /// then the first, which the stored order wouldn't give.
    const PAIR: &str = "type pair {
    { a: integer, b: integer },
    { total: integer, first: integer }
        to { a := first, b := total - first }
        from { total := a + b, first := a },
    order [asc total, asc first],
};
";

    #[test]
    fn a_programs_own_types_are_ordered_in_any_representation() {
        let value = |value: &str| {
            let source = format!("{PAIR}var v := {value};");
            variables(&source)[0].init.to_string()
        };
        // Compared by the components the order names, of the representation
        // it names, which the conversion computes.
        assert_eq!(value("pair{a: 1, b: 9} > pair{a: 5, b: 0}"), "true");
        assert_eq!(value("pair{a: 1, b: 3} < pair{a: 2, b: 2}"), "true");
        let pairs = "relation { { p: pair{a: 5, b: 0} }, { p: pair{a: 1, b: 9} }, { p: pair{a: 0, b: 4} } }";
        assert_eq!(
            value(&format!("min{{r: {pairs}, of: p}}")),
            "pair{a: 0, b: 4}"
        );
        assert_eq!(
            value(&format!("max{{r: {pairs}, of: p}}")),
            "pair{a: 1, b: 9}"
        );
        assert_eq!(
            value(&format!(
                "[ var ps: array pair; load ps value from {pairs} order [desc p]; ps ]"
            )),
            "array [pair{a: 1, b: 9}, pair{a: 5, b: 0}, pair{a: 0, b: 4}]"
        );
        // Of `any ordered`, and inside a value of another type, by it.
        let source = format!(
            "{PAIR}type entry {{ {{ at: pair }}, order [desc at] }};\noperator least{{a: T, b: T}}: T types {{ T: any ordered }} [ if a < b then a else b ];\nvar v := least{{a: entry{{at: pair{{a: 1, b: 9}}}}, b: entry{{at: pair{{a: 5, b: 0}}}}}};"
        );
        assert_eq!(
            variables(&source)[0].init.to_string(),
            "entry{at: pair{a: 1, b: 9}}"
        );
        // A conversion that fails when compiling fails the comparison.
        assert_eq!(
            codes(&format!(
                "{PAIR}var v := pair{{a: 9223372036854775807, b: 1}} < pair{{a: 0, b: 0}};{MAIN}"
            )),
            [Code::Overflow]
        );
    }

    #[test]
    fn a_programs_own_types_are_ordered_in_one_representation() {
        let declare = |order: &str| {
            format!(
                "type pair {{
    {{ a: integer, b: integer }},
    {{ total: integer, first: integer }}
        to {{ a := first, b := total - first }}
        from {{ total := a + b, first := a }},
    {order},
}};{MAIN}"
            )
        };
        assert_eq!(
            messages(&declare("order [asc total, asc a]")),
            ["`total` and `a` are components of different possible representations of `pair`"]
        );
        assert_eq!(
            messages(&declare("order [asc total]")),
            ["the order of `pair` leaves out `first`"]
        );
        // A misspelling is of the representation the others are of, or of
        // any.
        let source = declare("order [asc totl, asc first]");
        assert_eq!(messages(&source), ["unknown component `totl`"]);
        assert_eq!(fix(&source), declare("order [asc total, asc first]"));
        let source = declare("order [asc totl]");
        assert_eq!(fix(&source), declare("order [asc total]"));
        assert!(
            errors(&source)[0].notes.iter().any(|note| note.contains(
                "an order compares the components of one possible representation of `pair`, as its selectors write them: `pair{a: …, b: …}` or `pair{first: …, total: …}`"
            )),
            "{:#?}",
            errors(&source)
        );
    }

    const TEMPERATURE: &str = "type temperature {
    { kelvin: approximate },
    { celsius: approximate }
        to { kelvin := celsius + 273.15e0 }
        from { celsius := kelvin - 273.15e0 },
    constraint kelvin >= 0e0,
    constraint celsius < 1000e0,
};
";

    #[test]
    fn a_programs_own_types_have_possible_representations() {
        let var = |value: &str| {
            let source = format!("{TEMPERATURE}var v := {value};");
            variables(&source)[0].init.to_string()
        };
        // Selected in any representation, stored in one, and read in any.
        assert_eq!(
            var("temperature{celsius: 100e0}"),
            "temperature{kelvin: 3.7315e2}"
        );
        assert_eq!(var("temperature{kelvin: 373.15e0}.celsius"), "1e2");
        assert_eq!(
            var("temperature{celsius: 0e0} = temperature{kelvin: 273.15e0}"),
            "true"
        );
        // A constraint names any component, through the conversion.
        let broken = errors(&format!(
            "{TEMPERATURE}var v := temperature{{celsius: 2000e0}};{MAIN}"
        ));
        assert_eq!(broken[0].code, Code::BrokenConstraint);
        assert_eq!(
            broken[0].primary.message,
            "its components break a constraint of `temperature`"
        );
        // Known only when running, a component of another representation
        // is its conversion's, of the value's stored components, computed
        // once.
        let program = analyze_str(&format!(
            "{TEMPERATURE}operator c{{t: temperature}}: approximate [ t.celsius ];{MAIN}"
        ))
        .unwrap();
        let c = program.operators.iter().find(|op| op.name == "c").unwrap();
        let hir::OperatorKind::ReadOnly { value, .. } = &c.kind else {
            panic!("a read-only operator");
        };
        let hir::Expr::At(_, value) = value else {
            panic!("the body's value, where it is: {value:?}");
        };
        let hir::Expr::Extract(inner, name) = &**value else {
            panic!("a component: {value:?}");
        };
        assert_eq!(name, "celsius");
        assert!(
            matches!(&**inner, hir::Expr::Extract(extended, _) if matches!(**extended, hir::Expr::Extend(..))),
            "{inner:?}"
        );
    }

    #[test]
    fn a_programs_own_types_change_in_any_representation() {
        let var = |value: &str| {
            let source =
                format!("{TEMPERATURE}var t := temperature{{celsius: 20e0}};\nvar v := {value};");
            variables(&source)[1].init.to_string()
        };
        // In another representation, selected in it from its components,
        // the others as they were in it.
        assert_eq!(
            var("t replace { celsius := celsius + 5e0 }"),
            "temperature{kelvin: 2.9815e2}"
        );
        // Each representation's components are in scope, the others
        // computed by their conversions.
        assert_eq!(
            var("t replace { kelvin := celsius }"),
            "temperature{kelvin: 2e1}"
        );
        assert_eq!(
            var("t replace { celsius := kelvin - 293.15e0 + 1e0 }"),
            "temperature{kelvin: 2.7415e2}"
        );
        // So `update` does, a whole new value.
        assert_eq!(
            var("[ var u := t; update u { celsius := 0e0 }; u ]"),
            "temperature{kelvin: 2.7315e2}"
        );
        // An outer tuple's attributes are in scope too.
        assert_eq!(
            var(
                "tuple from (relation { { at: t, by: 2e0 } } replace { at := at replace { celsius := celsius * by } })"
            ),
            "tuple { at: temperature{kelvin: 3.1315e2}, by: 2e0 }"
        );
        // Selected, the value satisfies the type's constraints, of any
        // representation's components.
        let source = format!(
            "{TEMPERATURE}var t := temperature{{celsius: 20e0}};\nvar v := t replace {{ celsius := 1000e0 }};{MAIN}"
        );
        let errors = errors(&source);
        assert_eq!(errors.len(), 1, "{errors:#?}");
        assert_eq!(errors[0].code, Code::BrokenConstraint);
        assert_eq!(
            &source[errors[0].span().start..errors[0].span().end],
            "t replace { celsius := 1000e0 }"
        );
    }

    #[test]
    fn a_programs_own_types_change_one_representation_at_a_time() {
        let program = |change: &str| {
            format!(
                "{TEMPERATURE}operator main{{}} [\n    var t := temperature{{celsius: 20e0}};\n    {change}\n    call debug_value{{v: t}};\n];\n"
            )
        };
        // Components of two representations, which no selector takes
        // together.
        let source = program("t := t replace { celsius := 1e0, kelvin := 2e0 };");
        let mixed = errors(&source);
        assert_eq!(mixed.len(), 1, "{mixed:#?}");
        assert_eq!(mixed[0].code, Code::Conversions);
        assert_eq!(
            mixed[0].message,
            "`celsius` and `kelvin` are components of different possible representations of `temperature`"
        );
        assert_eq!(
            &source[mixed[0].span().start..mixed[0].span().end],
            "kelvin"
        );
        assert!(mixed[0].suggestions.is_empty(), "{mixed:#?}");
        // A misspelling is one of a representation's components, of the
        // one the others are of, if they name one.
        let source = program("update t { celsus := 1e0 };");
        assert_eq!(fix(&source), program("update t { celsius := 1e0 };"));
        let source = program("update t { celsius := 1e0, kelvn := 2e0 };");
        let unknown = errors(&source);
        assert_eq!(unknown[0].code, Code::UnknownAttribute);
        assert!(unknown[0].suggestions.is_empty(), "{unknown:#?}");
        assert!(
            unknown[0].notes[0].contains("`temperature{kelvin: …}` or `temperature{celsius: …}`"),
            "{unknown:#?}"
        );
        // Updates of two representations' components conflict in an
        // `assign` block, since no selector takes them together.
        let source =
            program("assign { update t { celsius := 1e0 }; update t { kelvin := 300e0 }; };");
        assert_eq!(
            messages(&source),
            [
                "two changes to `t` conflict: they update components of different possible representations of `temperature`"
            ]
        );
        // Of one, any, they combine, as they do of the stored one, each
        // computed from the value as it was, unless two give one component
        // a value.
        let value = |change: &str| {
            let source = format!(
                "{PAIR}var v := [\n    var p := pair{{a: 1, b: 2}};\n    {change}\n    p\n];"
            );
            variables(&source)[0].init.to_string()
        };
        assert_eq!(
            value("assign { update p { total := 10 }; update p { first := total }; };"),
            "pair{a: 3, b: 7}"
        );
        assert_eq!(
            value("assign { update p { a := 5 }; update p { b := a }; };"),
            "pair{a: 5, b: 1}"
        );
        let source = format!(
            "{PAIR}operator main{{}} [\n    var p := pair{{a: 1, b: 2}};\n    assign {{ update p {{ total := 10 }}; update p {{ total := 4, first := 1 }}; }};\n];\n"
        );
        assert_eq!(
            messages(&source),
            ["two changes to `p` conflict: both give `total` a new value"]
        );
        // As other languages assign an object's field, fixed to `update`.
        let source = program("t.celsius := 25e0;");
        assert_eq!(fix(&source), program("update t { celsius := 25e0 };"));
        assert!(analyze_str(&fix(&source)).is_ok());
    }

    #[test]
    fn a_programs_own_types_representation_errors() {
        let declare = |others: &str| {
            format!("type temperature {{\n    {{ kelvin: approximate }},\n    {others},\n}};{MAIN}")
        };
        // One stored, the others converting both ways, once each.
        assert_eq!(
            messages(&declare("{ celsius: approximate }")),
            ["`temperature` has more than one possible representation without conversions"]
        );
        assert_eq!(
            messages(&declare(
                "{ celsius: approximate } to { kelvin := celsius + 273.15e0 }"
            )),
            ["this possible representation has no conversion `from`"]
        );
        assert_eq!(
            messages(&format!(
                "type t {{ {{ a: integer }} to {{ a := 1 }} from {{ a := 1 }} }};{MAIN}"
            )),
            ["no possible representation of `t` is stored"]
        );
        let source = declare(
            "{ celsius: approximate } to { kelvin := celsius } to { kelvin := 1e0 } from { celsius := kelvin }",
        );
        assert_eq!(codes(&source), [Code::DuplicateAttribute]);
        assert_eq!(
            fix(&source),
            declare("{ celsius: approximate } to { kelvin := celsius } from { celsius := kelvin }")
        );
        // The wrong way round, swapped.
        let source =
            declare("{ celsius: approximate } to { celsius := kelvin } from { kelvin := celsius }");
        assert_eq!(
            messages(&source),
            ["`to` and `from` are the wrong way round"]
        );
        assert_eq!(
            fix(&source),
            declare("{ celsius: approximate } from { celsius := kelvin } to { kelvin := celsius }")
        );
        // A component in one representation.
        assert_eq!(
            codes(&declare(
                "{ kelvin: integer } to { kelvin := 1e0 } from { kelvin := 1 }"
            )),
            [Code::DuplicateAttribute]
        );
        // Each conversion computes its components, once each, of their
        // types, from the others alone.
        let source =
            declare("{ celsius: approximate } to { kelvn := celsius } from { celsius := kelvin }");
        assert_eq!(messages(&source), ["unknown component `kelvn`"]);
        assert_eq!(
            fix(&source),
            declare("{ celsius: approximate } to { kelvin := celsius } from { celsius := kelvin }")
        );
        assert_eq!(
            codes(&declare(
                "{ celsius: approximate } to { kelvin := 1 } from { celsius := kelvin }"
            )),
            [Code::TypeMismatch]
        );
        assert_eq!(
            messages(&format!(
                "var limit := 1e0;\n{}",
                declare(
                    "{ celsius: approximate } to { kelvin := celsius } from { celsius := limit }"
                )
            )),
            ["a conversion of `temperature` can't read the variable `limit`"]
        );
    }

    #[test]
    fn a_csv_file_is_read_into_values_of_a_programs_own_types() {
        let call = |declared: &str, columns: &str| {
            format!(
                "uses {{ trendy.files }};\n\ntype position {{ {{ latitude: approximate, longitude: approximate }} }};\ntype area {{ {{ name: text, parts: relation {{ part: text }} }} }};\n\noperator main{{}} [\n    var rows: array tuple {{ {declared} }};\n    call load_csv{{path: file_path{{text: \"places.csv\"}}, header: true, columns: {columns}, out rows: rows}};\n];\n"
            )
        };
        // A value's components are read from columns of their own, given
        // as a tuple of them, nested for a value inside.
        analyze_str(&call(
            "name: text, at: position",
            "tuple { name: 1, at: tuple { latitude: 2, longitude: 3 } }",
        ))
        .unwrap();
        // Not a column for the value as a whole.
        let whole = errors(&call(
            "name: text, at: position",
            "tuple { name: 1, at: 2 }",
        ));
        assert_eq!(whole[0].code, Code::TypeMismatch);
        assert_eq!(
            whole[0].message,
            "mismatched types: expected `tuple { latitude: integer, longitude: integer }`, found `integer`"
        );
        // A component is named as `t.c` reads it, in a column of its own,
        // and one no field writes is unreadable.
        assert_eq!(
            messages(&call(
                "name: text, at: position",
                "tuple { name: 2, at: tuple { latitude: 2, longitude: 0 } }",
            )),
            [
                "`at.latitude` and `name` are both read from column 2",
                "`at.longitude` is read from column 0, but columns count from 1",
            ]
        );
        assert_eq!(
            messages(&call("a: area", "tuple { a: tuple { name: 1, parts: 2 } }")),
            ["`load_csv` can't read `a.parts`, of type `relation { part: text }`, from a field"]
        );
    }

    #[test]
    fn a_csv_file_is_read_in_any_representation() {
        let call = |columns: &str| {
            format!(
                "uses {{ trendy.files }};\n\n{TEMPERATURE}type bag {{\n    {{ items: relation {{ item: text }} }},\n    {{ item: text }}\n        to {{ items := relation {{ {{ item: item }} }} }}\n        from {{ item := value from items }},\n}};\n\noperator main{{}} [\n    var rows: array tuple {{ city: text, mean: temperature, contents: bag }};\n    call load_csv{{path: file_path{{text: \"climate.csv\"}}, header: true, columns: {columns}, out rows: rows}};\n];\n"
            )
        };
        // A value's components are those of any of its possible
        // representations, which it is converted from, even one whose
        // stored components no field writes.
        analyze_str(&call(
            "tuple { city: 1, mean: tuple { celsius: 2 }, contents: tuple { item: 3 } }",
        ))
        .unwrap();
        analyze_str(&call(
            "tuple { city: 1, mean: tuple { kelvin: 2 }, contents: tuple { item: 3 } }",
        ))
        .unwrap();
        assert_eq!(
            messages(&call(
                "tuple { city: 1, mean: tuple { kelvin: 2 }, contents: tuple { items: 3 } }"
            )),
            [
                "`load_csv` can't read `contents.items`, of type `relation { item: text }`, from a field"
            ]
        );
        // Those of one representation.
        let mixed = errors(&call(
            "tuple { city: 1, mean: tuple { celsius: 2, kelvin: 4 }, contents: tuple { item: 3 } }",
        ));
        assert_eq!(mixed.len(), 1, "{mixed:#?}");
        assert_eq!(mixed[0].code, Code::Conversions);
        assert_eq!(
            mixed[0].message,
            "`celsius` and `kelvin` are components of different possible representations of `temperature`"
        );
        assert!(mixed[0].suggestions.is_empty(), "{mixed:#?}");
        // A misspelling is one of a representation's components, of the
        // one the others are of, if they name one.
        let source =
            call("tuple { city: 1, mean: tuple { celsus: 2 }, contents: tuple { item: 3 } }");
        assert_eq!(codes(&source), [Code::UnknownAttribute]);
        assert_eq!(
            fix(&source),
            call("tuple { city: 1, mean: tuple { celsius: 2 }, contents: tuple { item: 3 } }")
        );
        let unknown = errors(&call(
            "tuple { city: 1, mean: tuple { fahrenheit: 2 }, contents: tuple { item: 3 } }",
        ));
        assert_eq!(unknown[0].code, Code::UnknownAttribute);
        assert!(unknown[0].suggestions.is_empty(), "{unknown:#?}");
        assert!(
            unknown[0].notes[0].contains("`temperature{kelvin: …}` or `temperature{celsius: …}`"),
            "{unknown:#?}"
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
    ) -> (&'p [hir::Local], &'p [(hir::Site, hir::Statement)]) {
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
            .filter_map(|(_, s)| match s {
                hir::Statement::Assign(a) => Some(a.iter().map(|a| a.target.clone()).collect()),
                hir::Statement::Declare(_)
                | hir::Statement::Call(_)
                | hir::Statement::If(..)
                | hir::Statement::Loop(..)
                | hir::Statement::For(..)
                | hir::Statement::Fail(_)
                | hir::Statement::Warn(_)
                | hir::Statement::Transaction(..)
                | hir::Statement::Return(_)
                | hir::Statement::Leave(_)
                | hir::Statement::Iterate(_) => None,
            })
            .collect();
        let local = |i| vec![hir::Target::Local(hir::LocalId(i))];
        assert_eq!(targets, [local(0), local(1), local(1)]);
        let (_, hir::Statement::Assign(declared)) = &body[1] else {
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
        let (_, hir::Statement::Call(call)) = &body[3] else {
            panic!("a call");
        };
        assert_eq!(call.args["r"], hir::Expr::Local(hir::LocalId(1)));
        let (_, main) = update_body(&program, "main");
        let (_, hir::Statement::Call(call)) = &main[0] else {
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
        // A type parameter has no default, nor has a tuple with one, so where
        // nothing decides what runs, and it could as well have its value now, it does.
        let generic = errors(&format!(
            "operator f{{v: T}}: T types {{ T: any type }} [ var w: T; v ];{MAIN}"
        ));
        assert_eq!(generic[0].code, Code::UntypedVariable);
        assert_eq!(
            generic[0].message,
            "variable `w`, of type `T`, has no initial value"
        );
        let tuple = errors(&format!(
            "operator f{{v: T}}: T types {{ T: any type }} [ var w: tuple {{ v: T }}; v ];{MAIN}"
        ));
        assert_eq!(tuple[0].code, Code::UntypedVariable);
        // But a relation or an array with one starts empty, as any does.
        for ty in [
            "array T",
            "relation { v: T }",
            "array relation { v: T, n: integer }",
            "relation { r: relation { v: T } }",
        ] {
            let program = format!(
                "operator f{{v: T}} types {{ T: any type }} [ var w: {ty}; call debug_value{{v: w}}; ];{MAIN}"
            );
            assert!(analyze_str(&program).is_ok(), "{program}");
        }
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
        let value = unsited(value.clone());
        assert!(
            matches!(&value, hir::Expr::Let(_, _, body) if matches!(**body, hir::Expr::Let(..)))
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
        // Its local variables are out of scope after it.
        assert_eq!(codes("[ var a := 1; a ] + a"), [Code::UnknownName]);
    }

    #[test]
    fn a_value_block_runs_statements_on_its_own_local_variables() {
        let in_f = |block: &str| {
            format!(
                "var total := 0;\noperator f{{n: integer}}: integer [ var m := 1; {block} ];{MAIN}"
            )
        };
        for block in [
            "[ var a := n; while a > 0 loop [ a := a - 1; ]; a ]",
            "[ var a := n; loop [ a := a + 1; ] while a < 3; a ]",
            "[ var s := 0; for x in array [n, m] loop [ s := s + x; ]; s ]",
            "[ var a := 0; if n > m then [ a := 1; ] else [ a := 2; ]; a ]",
            "[ if n < 0 then [ fail failure{message: \"no\"}; ]; n ]",
            "[ var q := 0; var e: relation transaction_collection; transaction collecting e [ q := 1 div n; ]; q ]",
            // A program's variable it hides, and blocks side by side.
            "[ var total := n; if n > 0 then [ total := 1; ]; total ]",
            "[ var a := 0; if n > 0 then [ var b := 1; a := b; ] else [ var b := 2; a := b; ]; a ]",
            // Each in the other's statements.
            "[ var a := 0; while a < 2 loop [ a := a + [ var i := 1; if n > 0 then [ i := 2; ]; i ]; ]; a ]",
        ] {
            let source = in_f(block);
            assert!(
                analyze_str(&source).is_ok(),
                "{block}: {:#?}",
                errors(&source)
            );
        }
        // Each of these is the same as in a block without them.
        let with_a_loop = |statement: &str| {
            in_f(&format!(
                "[ var a := 1; while a < 3 loop [ {statement} a := a + 1; ]; a ]"
            ))
        };
        assert_eq!(
            errors(&with_a_loop("m := a;"))[0].message,
            "a value block can't assign `m`"
        );
        for (statement, code) in [
            ("n := a;", Code::ReadOnlyEffect),
            ("total := a;", Code::ReadOnlyEffect),
            ("call debug_value{v: a};", Code::ReadOnlyEffect),
            ("warn failure{message: \"no\"};", Code::Return),
            ("return a;", Code::Return),
            // A local variable hides nothing around the block.
            ("var n := 1;", Code::DuplicateName),
            ("var m := 1;", Code::DuplicateName),
            ("var a := 1;", Code::DuplicateName),
        ] {
            assert_eq!(codes(&with_a_loop(statement)), [code], "{statement}");
        }
        for (block, code) in [
            // What's around it is read as it is outside the block.
            (
                "[ var a := total; if a > 0 then [ a := 1; ]; a ]",
                Code::VariableInReadOnly,
            ),
            // Its local variables are out of scope after it, and in the
            // branches they aren't declared in.
            (
                "[ var a := 1; if a > 0 then [ a := 2; ]; a ] + a",
                Code::UnknownName,
            ),
            ("[ if n > 0 then [ var b := 1; ]; b ]", Code::UnknownName),
            (
                "[ if n > 0 then [ b := 1; ]; var b := 0; b ]",
                Code::LocalBeforeDeclaration,
            ),
            // Its value has the type expected.
            (
                "[ var t := \"a\"; if n > 0 then [ t := \"b\"; ]; t ]",
                Code::TypeMismatch,
            ),
            // A loop's array and variable stay put.
            (
                "[ var x := array [1]; for y in x loop [ x := array [2]; ]; n ]",
                Code::NotAssignable,
            ),
            (
                "[ for y in array [1] loop [ y := 2; ]; n ]",
                Code::NotAssignable,
            ),
        ] {
            assert_eq!(codes(&in_f(block)), [code], "{block}");
        }
        // In an update operator, it reads what the operator reads, and an
        // `out` parameter only once it is assigned.
        assert!(analyze_str(&format!(
            "var total := 0;\noperator f{{n: integer}} [ total := [ var a := total; if n > 0 then [ a := n; ]; a ]; ];{MAIN}"
        ))
        .is_ok());
        assert_eq!(
            codes(&format!(
                "operator f{{out y: integer}} [ y := [ var a := y; if a > 0 then [ a := 1; ]; a ]; ];{MAIN}"
            )),
            [Code::Unassigned]
        );
    }

    #[test]
    fn loops_run_while_their_condition_is_true() {
        let body = main_body(
            "operator main{} [ var n := 3; while n > 0 loop [ var m := n; n := m - 1; ]; loop [ n := n + 1; ] while n < 2; ];",
        );
        let [
            _,
            (_, hir::Statement::Loop(_, before, hir::Check::Before)),
            (_, hir::Statement::Loop(_, after, hir::Check::After)),
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
        // One in an initial value runs when the program does.
        assert_eq!(
            codes(&format!(
                "var x := [ var i := 0; while i < 3 loop [ i := i + 1; ]; i ];{MAIN}"
            )),
            [Code::InitialValueWhenRunning]
        );
    }

    #[test]
    fn for_loops_go_through_an_array() {
        let body = main_body(
            "operator main{} [ var total := 0; for n in array [3, 1, 2] loop [ var doubled := n * 2; total := total + doubled; ]; ];",
        );
        let [_, (_, hir::Statement::For(variable, array, block))] = &body[..] else {
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
    }

    #[test]
    fn a_read_only_operator_runs_statements_on_what_it_is_given() {
        // Each that an update operator has, on its parameters and local
        // variables.
        for body in [
            "var i := n; while i > 0 loop [ i := i - 1; ]; i",
            "var i := n; loop [ i := i + 1; ] while i < 3; i",
            "var s := 0; for x in array [n, 1] loop [ s := s + x; ]; s",
            "var x := 0; if n > 0 then [ x := 1; ] else [ x := 2; ]; x",
            "if n < 0 then [ fail failure{message: \"no\"}; ]; n",
            "var q := 0; var e: relation transaction_collection; transaction collecting e [ q := 1 div n; ]; q",
            "var q := 0; transaction [ q := n; ]; q",
            "var r := relation { { a: n } }; if n > 0 then [ insert r relation { { a: 0 } }; delete r where a = n; ]; count{r: r}",
        ] {
            let source = format!("operator f{{n: integer}}: integer [ {body} ];{MAIN}");
            assert!(
                analyze_str(&source).is_ok(),
                "{body}: {:#?}",
                errors(&source)
            );
        }
        // They are run, and a call of it isn't computed when compiling,
        // where one without them is.
        let program = analyze_str(
            "operator looped{n: integer}: integer [ var i := n; while i > 0 loop [ i := i - 1; ]; i ];\noperator plain{n: integer}: integer [ var i := n; i := i - 1; i ];\noperator main{} [ call debug_value{v: looped{n: 2}}; call debug_value{v: plain{n: 2}}; ];",
        )
        .unwrap();
        let runs = |name: &str| {
            let op = program.operators.iter().find(|o| o.name == name).unwrap();
            let hir::OperatorKind::ReadOnly { value, .. } = &op.kind else {
                panic!("read-only");
            };
            matches!(value, hir::Expr::Run(..))
        };
        assert_eq!((runs("looped"), runs("plain")), (true, false));
        let body = main_body(
            "operator looped{n: integer}: integer [ var i := n; while i > 0 loop [ i := i - 1; ]; i ];\noperator plain{n: integer}: integer [ var i := n; i := i - 1; i ];\noperator main{} [ call debug_value{v: looped{n: 2}}; call debug_value{v: plain{n: 2}}; ];",
        );
        assert!(matches!(argument(&body[0]), hir::Expr::Call(_)));
        assert_eq!(
            argument(&body[1]),
            &hir::Expr::Constant(hir::Value::Integer(1))
        );
    }

    #[test]
    fn a_read_only_operators_statements_touch_nothing_else() {
        let in_loop = |statement: &str| {
            format!(
                "var total := 0;\noperator set{{}} [ total := 1; ];\noperator f{{n: integer}}: integer [ var i := n; while i > 0 loop [ {statement} i := i - 1; ]; i ];{MAIN}"
            )
        };
        // The program's variables, read, with the fix that passes it in.
        let read = in_loop("i := i + total;");
        assert_eq!(codes(&read), [Code::VariableInReadOnly]);
        assert!(
            fix(&read).contains("operator f{n: integer, total: integer}: integer"),
            "{}",
            fix(&read)
        );
        // Or assigned.
        assert_eq!(codes(&in_loop("total := 1;")), [Code::ReadOnlyEffect]);
        // A call of an update operator, which may change anything.
        assert_eq!(
            codes(&in_loop("call debug_value{v: i};")),
            [Code::ReadOnlyEffect]
        );
        // A warning, which would be written.
        assert_eq!(
            codes(&in_loop("warn failure{message: \"no\"};")),
            [Code::Return]
        );
        // Its parameters are given to it, as an update operator's `const`
        // ones are.
        assert_eq!(codes(&in_loop("n := 1;")), [Code::NotAssignable]);
        // It returns from the loop, and then nothing after the `return` runs.
        assert_eq!(codes(&in_loop("return i;")), [Code::Unreachable]);
        assert!(analyze_str(&in_loop("if i = 2 then [ return i; ];")).is_ok());
        // And it ends with its value.
        let source = format!(
            "operator f{{n: integer}}: integer [ while n > 0 loop [ var i := 1; ]; ];{MAIN}"
        );
        assert_eq!(codes(&source), [Code::Return]);
        // A loop's value is the value's type, as any is.
        let source = format!(
            "operator f{{n: integer}}: integer [ var t := \"a\"; if n > 0 then [ t := \"b\"; ]; t ];{MAIN}"
        );
        assert_eq!(codes(&source), [Code::TypeMismatch]);
    }

    #[test]
    fn a_constant_of_the_programs_is_known_when_it_can_be() {
        // As if its value were written: a variable's initial value can read it,
        // in any order, and so can a read-only operator's and a constraint.
        let program = analyze_str(
            "var a := b + 1;\nconst b := c * 2;\nconst c: integer := 3;\noperator f{}: integer [ b ];\nvar d := f{};\ntype t { { n: integer }, constraint n < b };\nvar e := t{n: 5};\noperator main{} [];",
        )
        .unwrap();
        let init = |i: usize| program.variables[i].init.clone();
        assert_eq!(init(0), hir::Value::Integer(7));
        assert_eq!(init(1), hir::Value::Integer(6));
        assert!(init(2).to_string().contains('5'), "{}", init(2));
        // Broken as it is computed, which is when compiling.
        assert_eq!(
            codes("const a := 1 div 0;\noperator main{} [];"),
            [Code::DivisionByZero]
        );
        assert_eq!(
            codes(
                "const r := relation { { a: 1, b: 1 }, { a: 1, b: 2 } } key { a };\noperator main{} [];"
            ),
            [Code::KeyViolated]
        );
        // Of the type it says, as a variable's value is.
        assert_eq!(
            codes("const a: text := 1;\noperator main{} [];"),
            [Code::TypeMismatch]
        );
        // A name it shares with variables, and databases, but not operators.
        assert_eq!(
            codes("const a := 1;\nvar a := 2;\noperator main{} [];"),
            [Code::DuplicateName]
        );
        assert_eq!(
            codes("const a := 1;\nconst a := 2;\noperator main{} [];"),
            [Code::DuplicateName]
        );
        analyze_str(
            "const a := 1;\noperator a{}: integer [ 2 ];\nvar b := a + a{};\noperator main{} [];",
        )
        .unwrap();
    }

    #[test]
    fn a_constant_the_compiler_cant_compute_is_left_to_the_program() {
        let looped = "const digits := series{from: 0, to: 9};\n";
        // Anything can read it but a variable's initial value, which is known
        // when compiling.
        analyze_str(&format!(
            "{looped}const biggest := max{{r: digits, of: value}};\noperator f{{}}: integer [ biggest ];\noperator main{{}} [ call debug_value{{v: f{{}}}}; ];"
        ))
        .unwrap();
        assert_eq!(
            codes(&format!("{looped}var a := digits;\noperator main{{}} [];")),
            [Code::InitialValueWhenRunning]
        );
        // Through a recursion, a value block's statements and an operator.
        for value in [
            "const a := down{n: 2};",
            "const a := [ var i := 0; while i < 2 loop [ i := i + 1; ]; i ];",
            "const a := through{};",
        ] {
            let source = format!(
                "operator down{{n: integer}}: integer [ if n = 0 then 0 else down{{n: n - 1}} ];\noperator through{{}}: integer [ down{{n: 1}} ];\n{value}\noperator main{{}} [ call debug_value{{v: a}}; ];"
            );
            analyze_str(&source).unwrap();
        }
    }

    #[test]
    fn a_constant_reads_no_variable_and_nothing_assigns_it() {
        let found = errors("var v := 1;\nconst c := v + 1;\noperator main{} [];");
        assert_eq!(found.len(), 1, "{found:#?}");
        assert_eq!(found[0].code, Code::VariableInReadOnly);
        assert_eq!(found[0].suggestions.len(), 1, "{found:#?}");
        for body in [
            "c := 2;",
            "insert r relation { { n: 2 } };",
            "delete r where n = 1;",
            "call inc{var n: c};",
        ] {
            let source = format!(
                "const c := 1;\nconst r := relation {{ {{ n: 1 }} }};\noperator inc{{var n: integer}} [ n := n + 1; ];\noperator main{{}} [ {body} ];"
            );
            let found = errors(&source);
            assert_eq!(codes(&source), [Code::NotAssignable], "{body}: {found:#?}");
            assert_eq!(found[0].suggestions.len(), 1, "{body}: {found:#?}");
        }
        // In a read-only operator, whose fix leaves it to read a variable.
        assert_eq!(
            codes("const c := 1;\noperator f{}: integer [ c := 2; c ];\noperator main{} [];"),
            [Code::NotAssignable]
        );
        // What an interface declares can't be a variable, so nothing fixes it.
        let found = errors("operator main{} [ pi := 3e0; ];");
        assert_eq!(found[0].code, Code::NotAssignable, "{found:#?}");
        assert!(found[0].suggestions.is_empty(), "{found:#?}");
    }

    #[test]
    fn a_constant_cycle_is_found_however_it_is_reached() {
        let c = "const c := f{};\n";
        let f = "operator f{}: integer [ c + 1 ];\n";
        for source in [
            format!("{c}{f}{MAIN}"),
            format!("{f}{c}{MAIN}"),
            format!("const c := d;\nconst d := c;\n{MAIN}"),
            format!("const c := c;\n{MAIN}"),
            // Done with `b` by the time the constant refers to it, from a variable
            // first, and through a recursion.
            format!(
                "var v: integer := a{{n: 1}};\noperator a{{n: integer}}: integer [ b{{n: n}} + k ];\noperator b{{n: integer}}: integer [ if n = 0 then 0 else a{{n: n - 1}} ];\nconst k := b{{n: 0}};\n{MAIN}"
            ),
        ] {
            let found = errors(&source);
            assert!(
                found.iter().any(|e| e.code == Code::CyclicInitialValues),
                "{source}: {found:#?}"
            );
            assert_eq!(
                found.len(),
                found
                    .iter()
                    .filter(|e| e.code == Code::CyclicInitialValues)
                    .count()
                    + found
                        .iter()
                        .filter(|e| e.code == Code::InitialValueWhenRunning)
                        .count(),
                "{source}: {found:#?}"
            );
        }
    }

    #[test]
    fn a_constant_is_not_called() {
        for (source, code) in [
            (
                "const a := 1;\nvar b := a{};\noperator main{} [];",
                Code::NotAnOperator,
            ),
            ("var b := pi{};\noperator main{} [];", Code::NotAnOperator),
            (
                "const a := 1;\nvar b := a{x: 1};\noperator main{} [];",
                Code::NotAnOperator,
            ),
        ] {
            assert_eq!(codes(source), [code], "{source}");
        }
        // Its braces go, if it has nothing in them.
        let found = errors("var b := pi{};\noperator main{} [];");
        assert_eq!(found[0].suggestions.len(), 1, "{found:#?}");
        let found = errors("const a := 1;\nvar b := a{x: 1};\noperator main{} [];");
        assert!(found[0].suggestions.is_empty(), "{found:#?}");
        // Other languages' ways of writing the prelude's.
        for written in ["PI", "M_PI", "math.pi", "Math.PI"] {
            let source = format!("var b := {written};\noperator main{{}} [];");
            let found = errors(&source);
            assert_eq!(
                found[0].suggestions[0].edits[0].replacement, "pi",
                "{found:#?}"
            );
        }
        // A read-only operator without parameters is still called.
        let found = errors("operator tau{}: integer [ 6 ];\nvar b := TAU;\noperator main{} [];");
        assert_eq!(
            found[0].suggestions[0].edits[0].replacement, "tau{}",
            "{found:#?}"
        );
    }

    #[test]
    fn a_variable_nothing_assigns_can_be_a_constant() {
        let reads = "operator f{}: integer [ v ];\noperator main{} [ call debug_value{v: f{}}; ];";
        let found = errors(&format!("var v := 1;\n{reads}"));
        assert_eq!(found[0].code, Code::VariableInReadOnly, "{found:#?}");
        assert_eq!(
            found[0].suggestions[0].edits[0].replacement, "const",
            "{found:#?}"
        );
        // One that something assigns is passed, and one with no value has nothing
        // to be a constant of.
        for declared in [
            "var v := 1;\noperator g{} [ v := 2; ];",
            "var v := 1;\noperator g{var n: integer} [ n := 2; ];\noperator h{} [ call g{var n: v}; ];",
            "var v: integer;",
        ] {
            let found = errors(&format!("{declared}\n{reads}"));
            assert_eq!(found[0].code, Code::VariableInReadOnly, "{found:#?}");
            assert!(
                found[0]
                    .suggestions
                    .iter()
                    .all(|s| s.edits[0].replacement != "const"),
                "{declared}: {found:#?}"
            );
        }
    }

    #[test]
    fn a_body_with_errors_may_assign_a_variable() {
        // Nothing says what a body with errors assigns, and a word of the name in
        // it may be its target. What an argument is for names nothing.
        let reads = "operator f{}: integer [ v ];\noperator main{} [ call debug_value{v: f{}}; ];";
        let is_offered = |declared: &str| {
            errors(&format!("{declared}\n{reads}"))
                .iter()
                .find(|e| e.code == Code::VariableInReadOnly)
                .expect("reported")
                .suggestions
                .iter()
                .any(|s| s.edits[0].replacement == "const")
        };
        assert!(!is_offered("var v := 1;\noperator g{} [ v := nope; ];"));
        assert!(is_offered("var v := 1;\noperator g{} [ var w := nope; ];"));
    }

    #[test]
    fn a_units_constant_and_operator_can_share_a_name() {
        let unit = "interface\n\nconst table := relation { { a: 1, b: 2 } };\nconst limit := 3;\n\
             operator table{a: integer, b: integer}: integer;\noperator table{}: integer;\n\
             operator limit{x: integer}: integer;\n\nimplementation\n\nconst hidden := 4;\n\
             operator table{a: integer, b: integer}: integer [ a + b ];\n\
             operator table{}: integer [ hidden ];\noperator limit{x: integer}: integer [ x ];\n";
        let analyzed = |declared: &str| {
            analyze_units(&[
                (
                    "program",
                    &format!("uses {{ m }};\n{declared}\noperator main{{}} [];"),
                ),
                ("m", unit),
            ])
        };
        let value = |written: &str| {
            let program = analyzed(&format!("var a := 1;\nvar v := {written};")).unwrap();
            program.variables[1].init.to_string()
        };
        // The constant, its projection, and before `{}` the operator.
        assert_eq!(value("m::limit"), "3");
        assert_eq!(value("m::table{b}"), "relation { b: integer } { { b: 2 } }");
        assert_eq!(value("m::table{}"), "4");
        assert_eq!(value("m::table{a: 1, b: 2}"), "3");
        // With each name a value, it could be either (A0079), as it can be
        // unqualified.
        let found = analyzed("var a := 1;\nvar b := 2;\nvar v := m::table{a, b};").unwrap_err();
        assert_eq!(found[0].code, Code::ProjectionOrCall, "{found:#?}");
        assert_eq!(
            found[0].help,
            [
                "to call the operator, write `m::table{a: a, b: b}`",
                "to project the value, write `(m::table){a, b}`",
            ]
        );
        // A value that can't be projected: the fix calls the operator.
        let source = "var x := 1;\nvar v := m::limit{x};";
        let found = analyzed(source).unwrap_err();
        assert_eq!(found[0].code, Code::NotAnOperator, "{found:#?}");
        assert_eq!(
            found[0].suggestions[0].message,
            "call the operator `m::limit`, which has these parameters"
        );
        assert_eq!(found[0].suggestions[0].edits[0].replacement, "x: ");
        // Its own operator of that name is no operator of that unit's.
        let source =
            "operator limit{a: integer}: integer [ a ];\nvar a := 1;\nvar v := m::limit{a};";
        let found = analyzed(source).unwrap_err();
        assert_eq!(found[0].code, Code::NotAnOperator, "{found:#?}");
        assert!(found[0].suggestions.is_empty(), "{found:#?}");
        // What its implementation keeps is no other unit's to read.
        let found = analyzed("var v := m::hidden;").unwrap_err();
        assert_eq!(found[0].code, Code::UnknownName, "{found:#?}");
        assert_eq!(found[0].message, "unknown value `m::hidden`");
    }

    #[test]
    fn a_qualified_name_is_a_constant_or_a_call() {
        // A constant, read wherever a value is, known when compiling.
        for source in [
            "var a := 2e0 * trendy.prelude::pi;",
            "const a := trendy.prelude::pi;",
            "operator f{}: approximate [ trendy.prelude::pi ];",
            "type t { { x: approximate }, constraint x < trendy.prelude::pi };",
            "var r: relation { x: approximate };\nconstraint small := is_empty{r: r where x > trendy.prelude::pi};",
            "database d;\nvar d.r: relation { x: approximate } key { x };\nconstraint d.small := is_empty{r: r where x > trendy.prelude::pi};",
            "operator g{} [ call debug_value{v: trendy.prelude::pi}; ];",
            "operator g{} [ var a := [ var b := trendy.prelude::pi; b ]; call debug_value{v: a}; ];",
            "operator g{r: relation { x: approximate }} [ call debug_table{r: r extend { y := x * trendy.prelude::pi } where y > trendy.prelude::pi}; ];",
            "operator g{a: approximate} [ if a > trendy.prelude::pi then [ call debug_value{v: trendy.prelude::pi}; ]; ];",
        ] {
            let source = format!("{source}\n{MAIN}");
            assert!(
                analyze_str(&source).is_ok(),
                "{source}: {:#?}",
                errors(&source)
            );
        }
        // A misspelling, fixed from what the unit declares.
        assert_eq!(
            fix(&format!("var a := trendy.prelude::pie;{MAIN}")),
            format!("var a := trendy.prelude::pi;{MAIN}")
        );
        // A library no one wrote, as `math.pi`.
        assert_eq!(
            fix(&format!("var a := nowhere::pi;{MAIN}")),
            format!("var a := pi;{MAIN}")
        );
        // A unit no one wrote, of another name, as any qualifier.
        assert_eq!(
            codes(&format!("var a := nowhere::nothing;{MAIN}")),
            [Code::UnknownName]
        );
        // An operator, called, whose name alone is reported, with the fix if it
        // has no parameters.
        let unit = "operator tau{}: integer [ 6 ];\n";
        assert_eq!(
            codes(&format!("var a := trendy.prelude::count;{MAIN}")),
            [Code::OperatorAsValue]
        );
        assert_eq!(
            errors(&format!("var a := trendy.prelude::count;{MAIN}"))[0].help,
            ["call it with its arguments: the parameters are `count{r: any relation}`"]
        );
        assert_eq!(
            errors(&format!(
                "{unit}var a := trendy.prelude::debug_value;{MAIN}"
            ))[0]
                .help,
            ["to invoke it, write a statement: `call trendy.prelude::debug_value{…};`"]
        );
        // Called, and with arguments, a constant is reported.
        for (written, fixed) in [
            ("trendy.prelude::pi{}", Some("trendy.prelude::pi")),
            ("trendy.prelude::pi{a: 1}", None),
            ("trendy.prelude::pi{a}", None),
        ] {
            let source = format!("var a := {written};{MAIN}");
            assert_eq!(codes(&source), [Code::NotAnOperator], "{written}");
            let found = errors(&source);
            match fixed {
                Some(fixed) => assert_eq!(fix(&source), format!("var a := {fixed};{MAIN}")),
                None => assert!(found[0].suggestions.is_empty(), "{found:#?}"),
            }
        }
        // Never assigned, in any form, nor passed to be.
        let inc = "operator inc{var n: approximate} [ n := n + 1e0; ];\n";
        for body in [
            "trendy.prelude::pi := 3e0;",
            "trendy.prelude::pi[1] := 3e0;",
            "assign { trendy.prelude::pi := 3e0; };",
            "insert trendy.prelude::pi relation { { n: 1 } };",
            "delete trendy.prelude::pi where n = 1;",
            "call inc{var n: trendy.prelude::pi};",
        ] {
            let source = format!("{inc}operator main{{}} [ {body} ];");
            assert_eq!(codes(&source), [Code::NotAssignable], "{body}");
        }
    }

    #[test]
    fn a_constraints_form_says_what_breaks_it_only_of_relations() {
        let breaking = |condition: &str, operators: &str| {
            let source = format!(
                "var v := relation {{ {{ x: 1 }} }};\nvar n := 1;\nvar t := \"a\";\n{operators}constraint c := {condition};\n{MAIN}"
            );
            let program = analyze_str(&source).unwrap();
            program.constraints[0].parts[0].breaking.is_some()
        };
        assert!(breaking("v <= v", ""));
        assert!(breaking("v >= v", ""));
        assert!(breaking("is_empty{r: v where x > 1}", ""));
        for condition in ["count{r: v} <= 3", "3 >= count{r: v}", "t <= \"b\""] {
            assert!(!breaking(condition, ""), "{condition}");
        }
        let own = "operator is_empty{r: integer}: boolean [ r = 1 ];\n";
        assert!(!breaking("is_empty{r: n}", own));
    }

    #[test]
    fn a_constraint_the_compiler_cant_compute_is_for_the_program_to_check() {
        let doubling = "operator doubling{n: integer}: integer [ var i := n; while i < 8 loop [ i := i * 2; ]; i ];\n";
        let three = "const three := max{r: series{from: 1, to: 3}, of: value};\n";
        let at_start = |source: String| {
            let source = format!("var v := 1;\n{source}\n{MAIN}");
            analyze_str(&source).unwrap().constraints[0].checked_at_start
        };
        assert!(!at_start("constraint c := v = 1;".to_string()));
        assert!(at_start(format!("{three}constraint c := v <= three;")));
        assert!(at_start(format!(
            "{doubling}constraint c := v <= doubling{{n: 1}};"
        )));
        // What the initial values are known to break is still an error.
        let source = format!("var v := 1;\n{doubling}constraint c := v > 5;\n{MAIN}");
        assert_eq!(codes(&source), [Code::ConstraintViolated]);
        let source = format!(
            "var v := 1;\n{doubling}constraint c := v > 5 and v <= doubling{{n: 1}};\n{MAIN}"
        );
        assert_eq!(codes(&source), [Code::ConstraintViolated]);
        // And one that mentions no variable, which it can't say is true.
        let source = format!("{three}constraint c := three > 0;\n{MAIN}");
        let found = errors(&source);
        assert_eq!(found[0].code, Code::ConstantConstraint, "{found:#?}");
        assert_eq!(
            found[0].primary.message,
            "the same whatever the variables are"
        );
        assert!(found[0].suggestions.is_empty(), "{found:#?}");
    }

    #[test]
    fn a_local_constant_is_never_assigned() {
        let update = "operator inc{var n: integer} [ n := n + 1; ];\n";
        let fill = "operator fill{out n: integer} [ n := 1; ];\n";
        for body in [
            "const c := 1; c := 2;",
            "const c := array [1]; c[1] := 2;",
            "const c := relation { { n: 1 } }; insert c relation { { n: 2 } };",
            "const c := relation { { n: 1 } }; delete c where n = 1;",
            "const c := relation { { n: 1 } }; update c { n := 2 } where n = 1;",
            "const c := 1; assign { c := 2; };",
            "const c := 1; call inc{var n: c};",
            "const c := 1; call fill{out n: c};",
            "const c: array integer := array [1]; load c value from relation { { n: 1 } } order [asc n] keep {n};",
            "var v := [ const c := 1; c := 2; c ];",
            "const c := relation { { n: 1 } }; if count{r: c} > 0 then [ c := relation {}; ];",
        ] {
            let source = format!("{update}{fill}operator main{{}} [ {body} ];");
            let found = errors(&source);
            assert_eq!(codes(&source), [Code::NotAssignable], "{body}: {found:#?}");
            assert_eq!(found[0].suggestions.len(), 1, "{body}: {found:#?}");
        }
        // In a read-only operator, a body's and one with statements.
        for source in [
            "operator f{}: integer [ const c := 1; c := 2; c ];\noperator main{} [];",
            "operator f{}: integer [ const c := 1; var r := c; if r > 0 then [ c := 2; ]; r ];\noperator main{} [];",
        ] {
            assert_eq!(codes(source), [Code::NotAssignable], "{source}");
        }
        // It reads as any variable does, with a type, keys and a value that isn't
        // known when compiling, in each kind of block.
        analyze_str(
            "operator f{n: integer}: integer [ const twice: integer := n * 2; twice + 1 ];\noperator g{r: relation { n: integer }} [ const k := r key { n }; if count{r: k} > 0 then [ const m := max{r: k, of: n}; call debug_value{v: m}; ]; ];\noperator main{} [ const v := [ const w := 1; w + 1 ]; call debug_value{v}; ];",
        )
        .unwrap();
    }

    #[test]
    fn a_local_constant_has_its_value_from_where_it_is_declared() {
        for declaration in ["const c: integer;", "const c;"] {
            let source = format!("operator main{{}} [ {declaration} ];");
            let found = errors(&source);
            assert_eq!(codes(&source), [Code::ConstantWithoutValue], "{found:#?}");
            // Nothing can say what its value was to be.
            assert!(found[0].suggestions.is_empty(), "{found:#?}");
        }
        // Declared again, which can't be fixed to an assignment of a constant.
        let source = "operator main{} [ const c := 1; const c := 2; ];";
        let found = errors(source);
        assert_eq!(codes(source), [Code::DuplicateName]);
        assert!(found[0].suggestions.is_empty(), "{found:#?}");
        let source = "operator main{} [ var c := 1; const c := 2; ];";
        let found = errors(source);
        assert_eq!(found[0].suggestions.len(), 1, "{found:#?}");
    }

    #[test]
    fn a_call_that_reaches_a_cycle_is_never_computed_when_compiling() {
        let countdown = "operator countdown{n: integer}: integer [ if n = 0 then 0 else countdown{n: n - 1} ];\n";
        for value in [
            "countdown{n: 3}",
            "countdown{n: 3} + 1",
            // Through one that isn't in the cycle.
            "reaching{n: 3}",
            // Through the versions of a specification, one of which is.
            "walk{s: ahead{n: 3}}",
            // Through a type's constraint, as a selection checks it.
            "fine{n: 3}.n",
        ] {
            let source = format!(
                "{countdown}operator reaching{{n: integer}}: integer [ countdown{{n: n}} ];\n\
                 type fine {{ {{ n: integer }}, constraint countdown{{n: n}} = 0 }};\n\
                 type s union;\ntype ahead is s {{ {{ n: integer }} }};\ntype back is s {{ {{ m: integer }} }};\n\
                 operator walk{{s: s}}: integer;\n\
                 operator walk{{s: ahead}}: integer [ walk{{s: back{{m: s.n}}}} ];\n\
                 operator walk{{s: back}}: integer [ s.m ];\n\
                 var v := {value};{MAIN}"
            );
            assert_eq!(
                codes(&source),
                [Code::InitialValueWhenRunning],
                "{value}: {:#?}",
                errors(&source)
            );
        }
        // Where it is left for the program to make, it is fine.
        let source =
            format!("{countdown}operator main{{}} [ call debug_value{{v: countdown{{n: 3}}}}; ];");
        assert!(analyze_str(&source).is_ok(), "{:#?}", errors(&source));
    }

    #[test]
    fn a_cycle_through_a_conversion_or_a_constraint_checks_itself() {
        for source in [
            // A type's constraint that selects its own type, through an
            // operator that could otherwise stop it.
            format!(
                "type t {{ {{ n: integer }}, constraint fine{{n: n}} }};\noperator fine{{n: integer}}: boolean [ if n = 0 then true else t{{n: n - 1}}.n = 0 ];{MAIN}"
            ),
            // A conversion that selects in the representation it converts from.
            format!(
                "type t {{ {{ n: integer }}, {{ m: integer }} to {{ n := back{{m: m}} }} from {{ m := n }} }};\noperator back{{m: integer}}: integer [ if m = 0 then 0 else t{{m: m - 1}}.n ];{MAIN}"
            ),
            // A constraint in an operator's braces, through another.
            format!(
                "operator f{{n: integer, constraint g{{n: n}} > 0}}: integer [ n ];\noperator g{{n: integer}}: integer [ if n = 0 then 1 else f{{n: n - 1}} ];{MAIN}"
            ),
        ] {
            assert_eq!(
                codes(&source),
                [Code::ConstraintCycle],
                "{:#?}",
                errors(&source)
            );
        }
    }

    #[test]
    fn a_cycle_is_found_past_an_operator_already_checked() {
        // Whichever comes first in the program, the cycle through the constraint
        // is found.
        let a = "operator a{n: integer}: integer [ b{n: n} + t{n: n}.n ];\n";
        let b = "operator b{n: integer}: integer [ if n = 0 then 0 else a{n: n - 1} ];\n";
        let t = "type t { { n: integer }, constraint b{n: n} = 0 };\n";
        for source in [
            format!("{a}{b}{t}{MAIN}"),
            format!("{b}{a}{t}{MAIN}"),
            format!("{t}{a}{b}{MAIN}"),
        ] {
            assert_eq!(
                codes(&source),
                [Code::ConstraintCycle],
                "{:#?}",
                errors(&source)
            );
        }
        // An initial value, checked first, starts at `a`, and `b` is done by the
        // time the constraint refers to it.
        let source = format!("var v: integer := a{{n: 1}};\n{a}{b}{t}{MAIN}");
        assert!(
            codes(&source).contains(&Code::ConstraintCycle),
            "{:#?}",
            errors(&source)
        );
    }

    #[test]
    fn writing_an_error_doesnt_lead_back_to_its_fail() {
        let error = "type refused is error { { code: integer } };\n";
        let message =
            "operator message{error: refused}: text [ to_text{from: checked{n: error.code}} ];\n";
        for checked in [
            // A `fail` as a value.
            "operator checked{n: integer}: integer [ if n < 0 then fail refused{code: n} else n ];\n",
            // A `fail` statement.
            "operator checked{n: integer}: integer [ if n < 0 then [ fail refused{code: n}; ]; n ];\n",
            // Through another, done by the time the `fail` refers to `message`.
            "operator checked{n: integer}: integer [ other{n: n} + (if n < 0 then fail refused{code: n} else 0) ];\noperator other{n: integer}: integer [ if n = 0 then 0 else checked{n: n - 1} ];\n",
        ] {
            for source in [
                format!("{error}{message}{checked}{MAIN}"),
                format!("{error}{checked}{message}{MAIN}"),
            ] {
                let found = errors(&source);
                assert_eq!(codes(&source), [Code::ConstraintCycle], "{found:#?}");
                assert!(found[0].message.contains("a `fail` in"), "{found:#?}");
            }
        }
        // A `message` that calls an operator with no `fail` can recur, and a
        // `fail` that nothing `message` calls reaches is fine.
        let source = format!(
            "{error}operator message{{error: refused}}: text [ to_text{{from: down{{n: error.code}}}} ];\noperator down{{n: integer}}: integer [ if n = 0 then 0 else down{{n: n - 1}} ];\noperator checked{{n: integer}}: integer [ if n < 0 then fail refused{{code: n}} else n ];{MAIN}"
        );
        assert!(analyze_str(&source).is_ok(), "{:#?}", errors(&source));
    }

    #[test]
    fn a_read_only_operator_that_grows_its_types_is_reported() {
        for source in [
            format!("operator nest{{a: any type}}: integer [ nest{{a: tuple {{ x: a }}}} ];{MAIN}"),
            // Through another, and in a value block's statements.
            format!(
                "operator nest{{a: any type}}: integer [ other{{a: tuple {{ x: a }}}} ];\noperator other{{a: any type}}: integer [ var n := 0; if n = 0 then [ n := nest{{a: a}}; ]; n ];{MAIN}"
            ),
        ] {
            assert_eq!(
                codes(&source),
                [Code::GrowingRecursion],
                "{:#?}",
                errors(&source)
            );
        }
        // Passed on as it is, it never grows.
        let source = format!(
            "operator same{{a: T, n: integer}}: T\n    types {{ T: any type }}\n[ if n = 0 then a else same{{a: a, n: n - 1}} ];{MAIN}"
        );
        assert!(analyze_str(&source).is_ok(), "{:#?}", errors(&source));
    }

    #[test]
    fn a_read_only_operator_whose_statements_are_run_can_recur() {
        for source in [
            // In a value block.
            format!(
                "operator f{{n: integer}}: integer [ [ var r := 0; if n > 0 then [ r := f{{n: n - 1}}; ]; r ] ];{MAIN}"
            ),
            // Itself.
            format!(
                "operator f{{n: integer}}: integer [ var r := 0; if n > 0 then [ r := f{{n: n - 1}}; ]; r ];{MAIN}"
            ),
            // Through an operator computed when it can be, each way around.
            format!(
                "operator g{{n: integer}}: integer [ f{{n: n}} ];\noperator f{{n: integer}}: integer [ var r := 0; if n > 0 then [ r := g{{n: n - 1}}; ]; r ];{MAIN}"
            ),
        ] {
            assert!(analyze_str(&source).is_ok(), "{:#?}", errors(&source));
        }
        // Through a type's constraint, which selecting a value checks.
        let source = format!(
            "type t {{ {{ n: integer }}, constraint fine{{n: n}} }};\noperator fine{{n: integer}}: boolean [ var ok := true; if n > 0 then [ ok := t{{n: 0}}.n = 0; ]; ok ];{MAIN}"
        );
        assert_eq!(codes(&source), [Code::ConstraintCycle]);
        // Two that call neither themselves nor each other are fine.
        let source = format!(
            "operator a{{n: integer}}: integer [ var r := n; if n > 0 then [ r := b{{n: n}}; ]; r ];\noperator b{{n: integer}}: integer [ var r := n; if n > 1 then [ r := 1; ]; r ];{MAIN}"
        );
        assert!(analyze_str(&source).is_ok(), "{:#?}", errors(&source));
    }

    #[test]
    fn an_initial_value_cant_wait_for_statements_to_run() {
        let looped = "operator looped{n: integer}: integer [ var i := n; while i > 0 loop [ i := i - 1; ]; i ];\n";
        for value in [
            "looped{n: 2}",
            // Through one computed when it can be.
            "plain{n: 2}",
            "plain{n: 2} + 1",
            // Or its own statements.
            "[ var i := 2; while i > 0 loop [ i := i - 1; ]; i ]",
        ] {
            let source = format!(
                "{looped}operator plain{{n: integer}}: integer [ looped{{n: n}} ];\nvar x := {value};{MAIN}"
            );
            let found = errors(&source);
            assert_eq!(
                codes(&source),
                [Code::InitialValueWhenRunning],
                "{found:#?}"
            );
            assert_eq!(&source[found[0].span().start..found[0].span().end], value);
        }
        // With a type, too, and a selection whose constraint runs them.
        let source = format!("{looped}var x: integer := looped{{n: 2}};{MAIN}");
        assert_eq!(codes(&source), [Code::InitialValueWhenRunning]);
        let source = format!(
            "operator fine{{n: integer}}: boolean [ var ok := true; if n > 0 then [ ok := false; ]; ok ];\ntype t {{ {{ n: integer }}, constraint fine{{n: n}} }};\nvar x := t{{n: 0}};{MAIN}"
        );
        assert_eq!(codes(&source), [Code::InitialValueWhenRunning]);
        // A read-only operator's value, and an update operator's, can wait.
        let source = format!(
            "{looped}operator plain{{n: integer}}: integer [ looped{{n: n}} ];\nvar x: integer;\noperator main{{}} [ x := plain{{n: 2}}; ];"
        );
        assert!(analyze_str(&source).is_ok(), "{:#?}", errors(&source));
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
        // Nothing runs after a `return`.
        let after = read_only("return n; var later := 1;");
        assert_eq!(after[0].code, Code::Unreachable);
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
            .flat_map(|(_, statement)| match statement {
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
        let (_, hir::Statement::Assign(assignments)) = &body.statements[1] else {
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
                .flat_map(|(_, statement)| match statement {
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
        let (_, hir::Statement::Assign(assignments)) = &body.statements[1] else {
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
            unsited(value.clone())
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
            ("len{r where x > 1}", "count{r: r where x > 1}"),
            ("size{x: a}", "length{a: a}"),
            ("CARDINALITY{a: a}", "length{a: a}"),
            ("count{r: a}", "length{a: a}"),
            ("length{a: r}", "count{r: r}"),
            ("ARRAY[1]", "array [1]"),
            ("a.length{}", "length{a: a}"),
            ("a.length", "length{a: a}"),
            ("a.length + 1", "length{a: a} + 1"),
        ] {
            let source = format!("{a}var x := {written};{MAIN}");
            let fixed_source = format!("{a}var x := {fixed};{MAIN}");
            assert_eq!(fix(&source), fixed_source, "{written}");
            assert!(analyze_str(&fixed_source).is_ok(), "{fixed}");
        }
    }

    #[test]
    fn a_method_call_is_fixed_only_when_its_parameter_is_known() {
        let shows = "operator show{v: integer}: text [ to_text{from: v} ];\noperator show{n: integer}: text [ to_text{from: n} ];\nvar seven := 7;\n";
        for written in ["seven.show", "seven.show{}"] {
            let source = format!("{shows}var x := {written};{MAIN}");
            let diagnostics = errors(&source);
            assert_eq!(
                diagnostics.iter().map(|d| d.code).collect::<Vec<_>>(),
                [Code::WrongOperandType],
                "{written}"
            );
            assert!(diagnostics[0].suggestions.is_empty(), "{written}");
            assert!(!diagnostics[0].help.is_empty(), "{written}");
        }
    }

    #[test]
    fn a_method_calls_other_arguments_are_kept_in_its_fix() {
        let scaled =
            "operator scaled{n: integer, rate: integer}: integer [ n * rate ];\nvar seven := 7;\n";
        let source = format!("{scaled}var x := seven.scaled{{rate: 2}};{MAIN}");
        assert_eq!(codes(&source), [Code::WrongOperandType]);
        let fixed = format!("{scaled}var x := scaled{{n: seven, rate: 2}};{MAIN}");
        assert_eq!(fix(&source), fixed);
        assert!(analyze_str(&fixed).is_ok());
        // An argument without a name, or one the operator hasn't, leaves
        // which parameter the value is for unknown.
        for written in ["seven.scaled{2}", "seven.scaled{by: 2}"] {
            let source = format!("{scaled}var x := {written};{MAIN}");
            let diagnostics = errors(&source);
            assert_eq!(
                diagnostics.iter().map(|d| d.code).collect::<Vec<_>>(),
                [Code::WrongOperandType],
                "{written}"
            );
            assert!(diagnostics[0].suggestions.is_empty(), "{written}");
        }
        // An attribute's or a component's value is called as nothing.
        let values =
            "type point { {x: integer} };\nvar t := tuple { a: 1 };\nvar p := point{x: 1};\n";
        for written in ["t.a{x: 1}", "p.x{y: 1}"] {
            assert_eq!(
                codes(&format!("{values}var x := {written};{MAIN}")),
                [Code::NotAnOperator],
                "{written}"
            );
        }
        // A component named as an operator is still a component.
        analyze_str(&format!(
            "{values}operator x{{v: integer}}: integer [ v ];\nvar n := p.x;{MAIN}"
        ))
        .unwrap();
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
        let (_, hir::Statement::If(_, then, otherwise)) = &body[0] else {
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
    fn union_types_and_their_subtypes() {
        let program = analyze_str(&format!(
            "type figure union;\ntype shape union is figure;\ntype disc is shape {{ {{ r: approximate }} }};\ntype square is shape {{ {{ side: approximate }} }};\ntype blob is figure {{ {{ size: integer }} }};{MAIN}"
        ))
        .unwrap();
        // Each union type's immediate subtypes, in the order they are
        // declared, each with its supertypes, the immediate one first.
        let subtypes = |name: &str| -> Vec<String> {
            program.subtypes[name]
                .iter()
                .map(ToString::to_string)
                .collect()
        };
        assert_eq!(subtypes("figure"), ["shape", "blob"]);
        assert_eq!(subtypes("shape"), ["disc", "square"]);
        let hir::Type::Defined(disc) = &program.subtypes["shape"][0] else {
            panic!("a type with a representation");
        };
        assert_eq!(disc.supertypes, ["shape", "figure"]);
        let hir::Type::Dummy(shape) = &program.subtypes["figure"][0] else {
            panic!("a union type");
        };
        assert_eq!(shape.supertypes, ["figure"]);
        assert_eq!(program.subtypes["figure"][0].root(), Some("figure"));
        let subtype = |name: &str| format!("type {name} is figure {{ {{ r: approximate }} }};\n");
        let two = format!("{}{}", subtype("a"), subtype("b"));
        for (source, code, message) in [
            (
                format!("type figure union {{ {{ r: approximate }} }};\n{two}"),
                Code::UnionType,
                "the union type `figure` has no possible representation of its own",
            ),
            (
                "type ellipse { { a: approximate } };\ntype circle is ellipse { constraint a > 0e0, { r: approximate } };\n".to_string(),
                Code::Specialization,
                "`circle`'s possible representation has no conversions",
            ),
            (
                "type ellipse { { a: approximate } };\ntype circle is ellipse { constraint a > 0e0, order [asc a] };\n".to_string(),
                Code::Specialization,
                "`circle` can't have an order of its own",
            ),
            (
                "type ellipse { { a: approximate } };\ntype round union is ellipse;\ntype disc is round { { r: approximate } };\ntype ring is round { { r: approximate, w: approximate } };\n".to_string(),
                Code::UnionType,
                "the union type `round` can't be a subtype of `ellipse`, which isn't a union type",
            ),
            (
                "type t is integer { { a: integer } };\n".to_string(),
                Code::UnionType,
                "`integer` is a built-in type, so no type is a subtype of it",
            ),
            (
                format!("type figure union;\n{two}type t is figur {{ {{ a: integer }} }};\n"),
                Code::UnknownType,
                "unknown type `figur`",
            ),
            (
                format!("type figure union;\n{}", subtype("disc")),
                Code::UnionType,
                "the union type `figure` has only one subtype, `disc`",
            ),
            (
                "type figure union;\n".to_string(),
                Code::UnionType,
                "the union type `figure` has no subtypes",
            ),
            (
                format!("type figure union is shape;\ntype shape union is figure;\n{two}"),
                Code::CyclicTypes,
                "the union types `figure` and `shape` are subtypes of each other",
            ),
            (
                format!("type figure union;\n{}type cluster is figure {{ {{ parts: relation {{ part: figure }} }} }};\n", subtype("a")),
                Code::CyclicTypes,
                "the types `figure` and `cluster` contain each other",
            ),
            (
                format!("type figure union;\n{two}var f: figure;\n"),
                Code::UntypedVariable,
                "variable `f`, of type `figure`, has no initial value",
            ),
        ] {
            let found = errors(&format!("{source}{MAIN}"));
            let found: Vec<(Code, &str)> = found.iter().map(|e| (e.code, e.message.as_str())).collect();
            assert_eq!(found, [(code, message)], "{source}");
        }
        // A cycle through a union type says where it contains a subtype.
        let cycle = errors(&format!(
            "type figure union;\n{}type cluster is figure {{ {{ parts: relation {{ part: figure }} }} }};{MAIN}",
            subtype("a")
        ));
        assert!(
            cycle[0]
                .secondary
                .iter()
                .any(|label| label.message == "`cluster` is one of `figure`'s subtypes here"),
            "{cycle:#?}"
        );
    }

    /// A union type, `figure`, and its subtypes, `disc` and `square`, as
    /// tests of substitution declare them, with a value of each.
    const FIGURES: &str = "type figure union;
type disc is figure { { r: approximate } };
type square is figure { { side: approximate } };
var round := disc{r: 1e0};
var box := square{side: 2e0};
";

    #[test]
    fn a_subtypes_value_is_a_value_of_its_union_type() {
        let variables = variables(&format!(
            "{FIGURES}var f: figure := round;
var shapes := relation {{ {{ s: round }}, {{ s: box }} }};
var both := array [round, box];
var either := if count{{r: shapes}} > 5 then round else box;
var same := f = round;
var also := round = f;
var wrapped := tuple {{ s: f }};
operator first{{}}: figure [ disc{{r: 3e0}} ];
var made := first{{}};
"
        ));
        let figure = || {
            hir::Type::Dummy(Box::new(hir::Dummy {
                name: "figure".to_string(),
                supertypes: Vec::new(),
            }))
        };
        let s = |ty: hir::Type| hir::Heading::new([("s".to_string(), ty)].into());
        let types: BTreeMap<&str, &hir::Type> =
            variables.iter().map(|v| (v.name.as_str(), &v.ty)).collect();
        // Each has its declared type, the most specific common supertype
        // of what meets in it, whatever its value's own type.
        assert_eq!(types["f"], &figure());
        assert_eq!(types["shapes"], &hir::Type::Relation(s(figure())));
        assert_eq!(types["both"], &hir::Type::Array(Box::new(figure())));
        assert_eq!(types["either"], &figure());
        assert_eq!(types["wrapped"], &hir::Type::Tuple(s(figure())));
        assert_eq!(types["made"], &figure());
        let value = |name: &str| &variables.iter().find(|v| v.name == name).unwrap().init;
        assert_eq!(value("f"), value("round"));
        assert_eq!(value("same"), &hir::Value::Boolean(true));
        assert_eq!(value("also"), &hir::Value::Boolean(true));
        // A value known when compiling is no more specific than its
        // declared type says: none of these is valid, though each value
        // is a disc.
        for invalid in [
            "var x := f.r;",
            "var x: disc := f;",
            "var x := made.r;",
            "var x := wrapped.s.r;",
            "var x := both[1].r;",
            "var x := (tuple from (shapes where s = round)).s.r;",
            "var x := [ var y: figure := round; y.r ];",
        ] {
            let source = format!(
                "{FIGURES}var f: figure := round;\nvar made := first{{}};\noperator first{{}}: figure [ disc{{r: 3e0}} ];\nvar wrapped := tuple {{ s: f }};\nvar both := array [round, box];\nvar shapes := relation {{ {{ s: round }}, {{ s: box }} }};\n{invalid}{MAIN}"
            );
            let found = errors(&source);
            assert_eq!(found.len(), 1, "{invalid}: {found:#?}");
            assert!(
                matches!(found[0].code, Code::WrongOperandType | Code::TypeMismatch),
                "{invalid}: {found:#?}"
            );
        }
    }

    #[test]
    fn a_test_narrows_a_name_in_the_rest_of_its_condition() {
        // Each of these reads a component only a subtype has, which is
        // valid because a test shows the value is of it there, whichever
        // order they are written in.
        for (body, value) in [
            ("f is disc and f.r > 0e0", "true"),
            ("f.r > 0e0 and f is disc", "true"),
            ("not (f is disc) or f.r > 0e0", "true"),
            ("f is disc implies f.r > 0e0", "true"),
            ("f is square implies f.side > 0e0", "true"),
            (
                "if f is disc then f.r > 0e0 else f is square and f.side > 0e0",
                "true",
            ),
            ("if not (f is disc) then false else f.r > 0e0", "true"),
            (
                "count{r: relation { { s: f } } where s is disc and s.r > 0e0} = 1",
                "true",
            ),
        ] {
            let source = format!(
                "{FIGURES}operator test{{f: figure}}: boolean [ {body} ];\nvar result := test{{f: round}};{MAIN}"
            );
            let variables = analyze_str(&source)
                .unwrap_or_else(|e| panic!("{body}: {e:#?}"))
                .variables;
            let result = variables.iter().find(|v| v.name == "result").unwrap();
            assert_eq!(result.init.to_string(), value, "{body}");
        }
        // `where` narrows its result's heading too.
        let variables = variables(&format!(
            "{FIGURES}var shapes := relation {{ {{ s: round }}, {{ s: box }} }};\nvar discs := shapes where s is disc;\nvar radii := discs extend {{ radius := s.r }};"
        ));
        let discs = variables.iter().find(|v| v.name == "discs").unwrap();
        assert_eq!(discs.ty.to_string(), "relation { s: disc }");
        // Without a test, it isn't; where a test shows it is of another
        // subtype, it is one of that; and where a test is false, it isn't
        // known to be of any other, as a program can add more.
        for (body, code) in [
            ("f.r > 0e0", Code::WrongOperandType),
            ("f is square and f.r > 0e0", Code::UnknownAttribute),
            (
                "if f is disc then f.side > 0e0 else true",
                Code::UnknownAttribute,
            ),
            ("f is square or f.r > 0e0", Code::WrongOperandType),
            ("not (f is square) and f.r > 0e0", Code::WrongOperandType),
            (
                "if f is square then true else f.r > 0e0",
                Code::WrongOperandType,
            ),
        ] {
            let found = errors(&format!(
                "{FIGURES}operator test{{f: figure}}: boolean [ {body} ];{MAIN}"
            ));
            assert_eq!(found.len(), 1, "{body}: {found:#?}");
            assert_eq!(found[0].code, code, "{body}: {found:#?}");
        }
        // Help with the test to write first.
        let found = errors(&format!(
            "{FIGURES}operator test{{f: figure}}: boolean [ f.r > 0e0 ];{MAIN}"
        ));
        assert_eq!(
            found[0].help,
            [
                "`r` is a component of `disc`: test `f` first, as in `f is disc and f.r …`, and in the rest of the condition it is one"
            ]
        );
    }

    #[test]
    fn a_call_computes_the_version_for_its_arguments_types() {
        let variables = variables(&format!(
            "{FIGURES}operator sides{{f: figure}}: integer;
operator sides{{f: disc}}: integer [ 0 ];
operator sides{{f: square}}: integer [ 4 ];
operator name{{f: figure}}: text [ \"a figure\" ];
operator name{{f: square}}: text [ \"a square\" ];
var f: figure := round;
var round_sides := sides{{f: f}};
var box_sides := sides{{f: box}};
var round_name := name{{f: round}};
var box_name := name{{f: box}};"
        ));
        let value = |name: &str| {
            variables
                .iter()
                .find(|v| v.name == name)
                .unwrap()
                .init
                .to_string()
        };
        assert_eq!(value("round_sides"), "0");
        assert_eq!(value("box_sides"), "4");
        // The specification's own body, where no version is for them.
        assert_eq!(value("round_name"), "\"a figure\"");
        assert_eq!(value("box_name"), "\"a square\"");
        // The HIR lists the versions, most specific first.
        let program = analyze_str(&format!(
            "{FIGURES}operator sides{{f: figure}}: integer;
operator sides{{f: disc}}: integer [ 0 ];
operator sides{{f: square}}: integer [ 4 ];{MAIN}"
        ))
        .unwrap();
        let spec = program
            .operators
            .iter()
            .find(|o| o.name == "sides" && !o.versions.is_empty())
            .unwrap();
        assert!(matches!(spec.kind, hir::OperatorKind::Specification { .. }));
        assert_eq!(spec.versions.len(), 2);
    }

    #[test]
    fn versions_are_like_their_specification() {
        let found = |defined: &str| -> Vec<(Code, String)> {
            errors(&format!("{FIGURES}{defined}{MAIN}"))
                .into_iter()
                .map(|e| (e.code, e.message))
                .collect()
        };
        for (defined, code, message) in [
            (
                "operator sides{f: figure}: integer;\noperator sides{f: disc}: integer [ 0 ];",
                Code::Versions,
                "`square` has no version of `sides`",
            ),
            (
                "operator sides{f: figure}: integer;\noperator sides{f: disc}: text [ \"0\" ];\noperator sides{f: square}: integer [ 4 ];",
                Code::Versions,
                "a version of `sides` is of `integer`, or a subtype of it, not `text`",
            ),
            (
                "operator sides{f: figure}: integer;\noperator sides{f: disc} [];\noperator sides{f: square}: integer [ 4 ];",
                Code::Versions,
                "a version of `sides` is read-only exactly if `sides` is",
            ),
            (
                "operator sides{f: disc}: integer [ 0 ];\noperator sides{f: square}: integer [ 4 ];",
                Code::Versions,
                "`sides` is defined twice with the same parameters, and no specification of both",
            ),
            (
                "operator sides{f: figure}: integer [ 1 ];\noperator sides{f: figure}: integer [ 2 ];",
                Code::DuplicateName,
                "`sides` is defined more than once with parameters named `f`",
            ),
            (
                "operator sides{f: figure}: integer;",
                Code::Versions,
                "`sides` has no body and no versions, so nothing computes it",
            ),
            (
                "operator grow{var f: figure};\noperator grow{var f: disc} [];\noperator grow{var f: square} [];",
                Code::Versions,
                "the `var` or `out` parameter `f` has a version's type of its own",
            ),
            (
                "operator pair{f: figure, g: figure}: integer;\noperator pair{f: disc, g: figure}: integer [ 1 ];\noperator pair{f: figure, g: disc}: integer [ 2 ];\noperator pair{f: square, g: square}: integer [ 3 ];",
                Code::Versions,
                "two versions of `pair` are for `pair{f: disc, g: disc}`, neither more specific than the other",
            ),
        ] {
            let found = found(defined);
            assert!(
                found.contains(&(code, message.to_string())),
                "{defined}: {found:#?}"
            );
        }
        // The fix for a specification's missing version names it.
        let missing = errors(&format!(
            "{FIGURES}operator sides{{f: figure}}: integer;\noperator sides{{f: disc}}: integer [ 0 ];{MAIN}"
        ));
        assert_eq!(
            missing[0].help,
            ["define its version: `operator sides{f: square}: integer [ … ];`"]
        );
        // And the one for versions without a specification declares it.
        let unspecified = errors(&format!(
            "{FIGURES}operator sides{{f: disc}}: integer [ 0 ];\noperator sides{{f: square}}: integer [ 4 ];{MAIN}"
        ));
        assert_eq!(
            unspecified[0].help,
            [
                "declare their specification, of their parameters' union types, `operator sides{f: figure}: integer;`, which they are versions of"
            ]
        );
    }

    #[test]
    fn tests_and_treats_decide_something() {
        // A test or a treat folds, of a value known when compiling.
        let variables = variables(&format!(
            "{FIGURES}var f: figure := round;\nvar yes := f is disc;\nvar no := f is square;\nvar r := (treat f as disc).r;"
        ));
        let value = |name: &str| {
            variables
                .iter()
                .find(|v| v.name == name)
                .unwrap()
                .init
                .to_string()
        };
        assert_eq!(
            (value("yes"), value("no"), value("r")),
            ("true".into(), "false".into(), "1e0".into())
        );
        // One always true, or never, or of a tuple's type, decides nothing.
        for (test, message) in [
            (
                "round is figure",
                "the test is always true: `round` is always a value of `figure`",
            ),
            (
                "round is disc",
                "the test is always true: `round` is always a value of `disc`",
            ),
            (
                "round is square",
                "the test is never true: no value of `disc` is a value of `square`",
            ),
            (
                "1 is disc",
                "the test is never true: no value of `integer` is a value of `disc`",
            ),
            (
                "treat round as figure",
                "the treat always succeeds: `round` is always a value of `figure`",
            ),
            (
                "treat round as square",
                "the treat never succeeds: no value of `disc` is a value of `square`",
            ),
            (
                "tuple { s: f } is tuple { s: disc }",
                "`is` tests a scalar value, not `tuple { s: f }`, of type `tuple { s: figure }`",
            ),
        ] {
            let found = errors(&format!(
                "{FIGURES}var f: figure := round;\nvar x := {test};{MAIN}"
            ));
            let found: Vec<(Code, &str)> =
                found.iter().map(|e| (e.code, e.message.as_str())).collect();
            assert_eq!(found, [(Code::TypeTest, message)], "{test}");
        }
        // A treat of a value known not to be of its type fails now.
        let found = errors(&format!(
            "{FIGURES}var f: figure := box;\nvar x := treat f as disc;{MAIN}"
        ));
        assert_eq!(found.len(), 1, "{found:#?}");
        assert_eq!(found[0].code, Code::NotOfType);
        assert_eq!(
            found[0].message,
            "`square{side: 2e0}` isn't a value of `disc`"
        );
        // Tutorial D's operators, fixed.
        let source = format!(
            "{FIGURES}var f: figure := round;\nvar x := IS_DISC{{f}};\nvar y := TREAT_AS_DISC{{f}};{MAIN}"
        );
        assert_eq!(
            fix(&source),
            format!(
                "{FIGURES}var f: figure := round;\nvar x := f is disc;\nvar y := (treat f as disc);{MAIN}"
            )
        );
    }

    #[test]
    fn disjoint_subtypes_are_never_equal_and_variables_are_exact() {
        // No value is both a disc and a square.
        let found = errors(&format!("{FIGURES}var x := round = box;{MAIN}"));
        assert_eq!(found.len(), 1, "{found:#?}");
        assert_eq!(found[0].code, Code::TypeMismatch);
        assert!(
            found[0].notes.contains(
                &"`disc` and `square` are both `figure`, but no value is of both: a union type's subtypes are disjoint"
                    .to_string()
            ),
            "{found:#?}"
        );
        // A value of the union type can be of another subtype.
        let found = errors(&format!(
            "{FIGURES}var f: figure := round;\nvar x: disc := f;{MAIN}"
        ));
        assert!(
            found[0].notes.contains(
                &"a value of `figure` is a value of one of its subtypes, which needn't be `disc`"
                    .to_string()
            ),
            "{found:#?}"
        );
        // An `out` parameter's can be of a supertype, as the operator gives it a
        // value of its type and reads nothing; but not of a subtype, nor of another
        // type, and a `var` parameter's can't be either.
        let make = "operator make{out d: disc} [ d := round; ];\n";
        for good in [
            "var f: figure; call make{out d: f};",
            "var d: disc; call make{out d};",
        ] {
            let source = format!("{FIGURES}{make}operator main{{}} [ {good} ];");
            assert!(analyze_str(&source).is_ok(), "{good}");
        }
        let poke = "operator poke{var d: disc} [ d := round; ];\n";
        for (bad, body) in [
            (
                "a `var` one's",
                format!(
                    "{poke}operator main{{}} [ var f: figure := round; call poke{{var d: f}}; ];"
                ),
            ),
            (
                "another type's",
                format!("{make}operator main{{}} [ var n := 1; call make{{out d: n}}; ];"),
            ),
            (
                "a subtype's",
                "operator any{out f: figure} [ f := box; ];\noperator main{} [ var d := round; call any{out f: d}; ];".to_string(),
            ),
        ] {
            let found = codes(&format!("{FIGURES}{body}"));
            assert_eq!(found, [Code::TypeMismatch], "{bad}");
        }
        // A `var` parameter's argument is of exactly its type, which the
        // operator can assign any value of.
        let found = errors(&format!(
            "{FIGURES}operator grow{{var f: figure}} [ f := box; ];\noperator main{{}} [ var d := round; call grow{{var f: d}}; ];"
        ));
        assert_eq!(found.len(), 1, "{found:#?}");
        assert_eq!(found[0].code, Code::TypeMismatch);
        assert!(
            found[0]
                .notes
                .iter()
                .any(|n| n.contains("IM Prescription 19")),
            "{found:#?}"
        );
    }

    #[test]
    fn relational_operators_combine_attributes_of_one_hierarchy() {
        // A union's attribute is of the most specific common supertype, an
        // intersection's and a join's of the least specific common
        // subtype, and a difference's and a semijoin's of the left
        // operand's (IM Prescription 13).
        let relations = "var discs := relation { { s: round, n: 1 } };
var squares := relation { { s: box, n: 2 } };
var figures := discs union squares;
";
        let variables = variables(&format!(
            "{FIGURES}{relations}var either := xunion{{discs, squares}};
var met := figures intersect discs;
var joined := figures join discs{{s}};
var left := figures minus discs;
var right := discs minus figures;
var matched := discs matching figures{{s}};
var images := figures extend {{ same := !!(discs rename {{n as m}}) }};
"
        ));
        let types: BTreeMap<&str, &hir::Type> =
            variables.iter().map(|v| (v.name.as_str(), &v.ty)).collect();
        let figure = hir::Type::Dummy(Box::new(hir::Dummy {
            name: "figure".to_string(),
            supertypes: Vec::new(),
        }));
        let disc = types["round"].clone();
        let of = |ty: &hir::Type| {
            hir::Type::Relation(hir::Heading::new(
                [
                    ("s".to_string(), ty.clone()),
                    ("n".to_string(), hir::Type::Scalar(hir::Scalar::Integer)),
                ]
                .into(),
            ))
        };
        for (name, ty) in [
            ("figures", of(&figure)),
            ("either", of(&figure)),
            ("met", of(&disc)),
            ("joined", of(&disc)),
            ("left", of(&figure)),
            ("right", of(&disc)),
            ("matched", of(&disc)),
        ] {
            assert_eq!(types[name], &ty, "{name}");
        }
        let value = |name: &str| &variables.iter().find(|v| v.name == name).unwrap().init;
        let hir::Value::Relation(figures) = value("figures") else {
            panic!("{:?}", value("figures"));
        };
        assert_eq!(figures.tuples.len(), 2);
        let hir::Value::Relation(met) = value("met") else {
            panic!("{:?}", value("met"));
        };
        let hir::Value::Relation(discs) = value("discs") else {
            panic!("{:?}", value("discs"));
        };
        assert_eq!(met.tuples, discs.tuples);
        // Values of disjoint types never match, nor are equal, so a join or
        // a comparison of them would decide nothing.
        let disjoint = |note: &String| {
            note.ends_with(
                "are both `figure`, but no value is of both: a union type's subtypes are disjoint",
            )
        };
        for (written, code) in [
            ("discs join squares", Code::JoinTypeConflict),
            ("discs not matching squares", Code::JoinTypeConflict),
            ("discs compose squares{s}", Code::JoinTypeConflict),
            (
                "discs extend { other := !!squares }",
                Code::JoinTypeConflict,
            ),
            ("discs intersect squares", Code::HeadingMismatch),
            ("discs minus squares", Code::HeadingMismatch),
            ("intersect{figures, discs, squares}", Code::HeadingMismatch),
        ] {
            let found = errors(&format!("{FIGURES}{relations}var x := {written};{MAIN}"));
            assert_eq!(found.len(), 1, "{written}: {found:#?}");
            assert_eq!(found[0].code, code, "{written}: {found:#?}");
            assert!(found[0].notes.iter().any(disjoint), "{written}: {found:#?}");
        }
        // A union's attributes need a common supertype.
        assert_eq!(
            codes(&format!(
                "{FIGURES}{relations}var x := discs union relation {{ {{ s: 1, n: 1 }} }};{MAIN}"
            )),
            [Code::HeadingMismatch]
        );
        // `i_delete` takes tuples of overlapping types, as `i_minus` does,
        // but `insert` only those of the variable's types or subtypes.
        let changed = |statement: &str| {
            format!(
                "{FIGURES}operator change{{var d: relation {{s: disc, n: integer}}, f: relation {{s: figure, n: integer}}}} [ {statement} ];{MAIN}"
            )
        };
        assert!(analyze_str(&changed("i_delete d f matching d;")).is_ok());
        assert_eq!(codes(&changed("insert d f;")), [Code::TypeMismatch]);
    }

    #[test]
    fn statements_narrow_names_until_one_may_assign_them() {
        let types = "type figure union;\ntype disc is figure { { r: approximate } };\ntype square is figure { { side: approximate } };\nvar g: figure := disc{r: 1e0};\noperator touch{} [ g := square{side: 1e0}; ];\noperator grow{var f: figure} [ f := square{side: 2e0}; ];\n";
        // An `if` statement's branches, as an `if`'s, a `while` loop's
        // block, and an `update`, which keeps the value of the type; a
        // `var` argument passes the variable as declared.
        let valid = format!(
            "{types}operator ok{{var f: figure, n: integer}} [
    if f is disc then [
        call debug_value{{v: f.r}};
        update f {{ r := 2e0 * r }};
        call debug_value{{v: f.r}};
        call grow{{var f: f}};
    ] else if not (f is square) then [
        call debug_value{{v: 1}};
    ] else [
        call debug_value{{v: f.side}};
    ];
    var i := 0;
    while f is disc and i < n loop [
        call debug_value{{v: f.r}};
        i := i + 1;
    ];
];{MAIN}"
        );
        assert!(analyze_str(&valid).is_ok(), "{:#?}", errors(&valid));
        // A statement that may assign a name ends it: an assignment, a
        // `var` argument, any call, of a variable of the program's, one in
        // a branch inside, and one later in a loop.
        for (body, assigned) in [
            (
                "f := square{side: 1e0};\n        call debug_value{v: f.r};",
                "f := square{side: 1e0}",
            ),
            (
                "call grow{var f: f};\n        call debug_value{v: f.r};",
                "call grow{var f: f};",
            ),
            (
                "if c then [ f := square{side: 1e0}; ];\n        call debug_value{v: f.r};",
                "f := square{side: 1e0}",
            ),
            (
                "while c loop [ call debug_value{v: f.r}; f := square{side: 1e0}; ];",
                "f := square{side: 1e0}",
            ),
        ] {
            let source = format!(
                "{types}operator bad{{var f: figure, c: boolean}} [\n    if f is disc then [\n        {body}\n    ];\n];{MAIN}"
            );
            let found = errors(&source);
            assert_eq!(found.len(), 1, "{source}: {found:#?}");
            assert_eq!(found[0].code, Code::WrongOperandType, "{source}");
            let label = &found[0]
                .secondary
                .iter()
                .find(|l| l.message == "`f` may be assigned here");
            let at = label.unwrap_or_else(|| panic!("{source}: {found:#?}")).span;
            assert_eq!(&source[at.start..at.end], assigned, "{source}");
        }
        let called = format!(
            "{types}operator bad{{}} [ if g is disc then [ call touch{{}}; call debug_value{{v: g.r}}; ]; ];{MAIN}"
        );
        assert_eq!(codes(&called), [Code::WrongOperandType]);
    }

    #[test]
    fn a_subtype_defined_by_constraints_is_its_supertypes_values_it_is_true_of() {
        let types = "type ellipse { { a: approximate, b: approximate }, constraint a >= b };\ntype circle is ellipse { constraint a = b };\n";
        let variables = variables(&format!(
            "{types}var round := ellipse{{a: 2e0, b: 2e0}};
var oval := ellipse{{a: 3e0, b: 2e0}};
var widened := [ var e := round; update e {{ a := 3e0 }}; e ];
var narrowed := [ var e := oval; update e {{ a := 2e0 }}; e ];
var tests := tuple {{ round: round is circle, oval: oval is circle, widened: widened is circle, narrowed: narrowed is circle }};
"
        ));
        let value = |name: &str| &variables.iter().find(|v| v.name == name).unwrap().init;
        assert_eq!(
            value("tests").to_string(),
            "tuple { narrowed: true, oval: false, round: true, widened: false }"
        );
        // Declared an ellipse, of most specific type circle, written as the
        // ellipse it is.
        let ty = |name: &str| {
            variables
                .iter()
                .find(|v| v.name == name)
                .unwrap()
                .ty
                .to_string()
        };
        assert_eq!(ty("round"), "ellipse");
        assert_eq!(value("round").to_string(), "ellipse{a: 2e0, b: 2e0}");
        let hir::Value::Defined(round) = value("round") else {
            panic!("{:?}", value("round"));
        };
        assert_eq!(round.ty.name, "circle");
        // A circle variable holds circles only: an update that makes its
        // value just an ellipse breaks its constraint.
        let broken = errors(&format!(
            "{types}var c := [ var c: circle := treat ellipse{{a: 1e0, b: 1e0}} as circle; update c {{ a := 2e0 }}; c ];{MAIN}"
        ));
        assert_eq!(broken[0].code, Code::BrokenConstraint, "{broken:#?}");
        // A value of two subtypes of one type is an error.
        let overlapping = errors(&format!(
            "{types}type small is ellipse {{ constraint a < 1e0 }};\nvar dot := ellipse{{a: 5e-1, b: 5e-1}};{MAIN}"
        ));
        assert_eq!(overlapping[0].code, Code::OverlappingSubtypes);
        assert_eq!(
            overlapping[0].message,
            "`ellipse{a: 5e-1, b: 5e-1}` is both a `circle` and a `small`, but a value has one most specific type"
        );
        // It needs a constraint.
        assert_eq!(
            codes(&format!(
                "type ellipse {{ {{ a: approximate }} }};\ntype circle is ellipse {{ {{ r: approximate }} }};{MAIN}"
            )),
            [Code::Specialization]
        );
        // An update of a name narrowed to a circle changes it as an
        // ellipse, after which it is an ellipse.
        let updated = format!(
            "type figure union;\ntype ellipse is figure {{ {{ a: approximate, b: approximate }}, constraint a >= b }};\ntype circle is ellipse {{ constraint a = b }};\ntype square is figure {{ {{ side: approximate }} }};\noperator widen{{var f: figure}} [ if f is circle then [ update f {{ a := 2e0 * a }}; call debug_value{{v: f.b}}; ]; ];{MAIN}"
        );
        assert!(analyze_str(&updated).is_ok(), "{:#?}", errors(&updated));
    }

    #[test]
    fn a_subtypes_own_representation_converts_to_its_supertypes_components() {
        let circle = |to: &str, from: &str| {
            format!(
                "type ellipse {{ {{ a: approximate, b: approximate, ctr: integer }}, constraint a >= b }};
type circle is ellipse {{
    constraint a = b,
    {{ r: approximate, ctr: integer }} to {{ {to} }} from {{ {from} }},
}};
"
            )
        };
        let ttm = circle("a := r, b := r, ctr := ctr", "r := a, ctr := ctr");
        // Its selector, its component, an update of it, and the component
        // it carries unchanged, which is its supertype's.
        let variables = variables(&format!(
            "{ttm}var c := circle{{r: 2e0, ctr: 1}};
var r := c.r;
var bigger := [ var d := c; update d {{ r := 3e0 }}; d ];
var moved := [ var d := c; update d {{ ctr := 2 }}; d ];
var read := tuple {{ r: r, bigger: bigger.r, ctr: moved.ctr, a: moved.a }};
"
        ));
        let value = |name: &str| &variables.iter().find(|v| v.name == name).unwrap().init;
        assert_eq!(
            value("read").to_string(),
            "tuple { a: 2e0, bigger: 3e0, ctr: 2, r: 2e0 }"
        );
        // A selection whose components aren't of a circle fails.
        let broken = errors(&format!(
            "{}var c := circle{{r: 2e0, ctr: 1}};{MAIN}",
            circle("a := r, b := r / 2e0, ctr := ctr", "r := a, ctr := ctr")
        ));
        assert_eq!(broken[0].code, Code::BrokenConstraint, "{broken:#?}");
        // Each of its representations converts, and one that doesn't carry
        // its supertype's component unchanged can't have it.
        let unconverted = errors(&format!(
            "type ellipse {{ {{ a: approximate, b: approximate }} }};\ntype circle is ellipse {{ constraint a = b, {{ r: approximate }} }};{MAIN}"
        ));
        assert_eq!(
            unconverted
                .iter()
                .map(|e| e.message.as_str())
                .collect::<Vec<_>>(),
            ["`circle`'s possible representation has no conversions"]
        );
        let changed = errors(&format!(
            "{}{MAIN}",
            circle("a := r, b := r, ctr := ctr + 1", "r := a, ctr := ctr - 1")
        ));
        assert_eq!(changed[0].code, Code::DuplicateAttribute, "{changed:#?}");
        assert!(
            changed[0]
                .notes
                .iter()
                .any(|n| n.contains("carry it unchanged"))
        );
    }

    #[test]
    fn a_dummy_type_has_no_selector_order_or_components() {
        // A dummy type has no selector; a fix selects the one subtype whose
        // representation has the components named.
        let types = "type figure union;\ntype disc is figure { { r: approximate } };\ntype square is figure { { side: approximate } };\n";
        let selected = errors(&format!("{types}var f := figure{{r: 1e0}};{MAIN}"));
        assert_eq!(selected[0].code, Code::DummySelected, "{selected:#?}");
        assert_eq!(
            selected[0].message,
            "the union type `figure` has no selector"
        );
        assert_eq!(
            fix(&format!("{types}var f := figure{{r: 1e0}};{MAIN}")),
            format!("{types}var f := disc{{r: 1e0}};{MAIN}")
        );
        let unfit = errors(&format!("{types}var f := figure{{x: 1e0}};{MAIN}"));
        assert_eq!(
            unfit[0].help,
            ["select a value of one of its subtypes, with `disc{…}` or `square{…}`"]
        );
        // Nor an order, nor components.
        for (body, note) in [
            (
                "x < y",
                "`figure` is a union type, which has no order: its values, of its subtypes' types, can only be compared with `=` and `<>`",
            ),
            (
                "x.r > 0e0",
                "`figure` is a union type, with no components of its own: its values are its subtypes', each with the components of its own type",
            ),
        ] {
            let found = errors(&format!(
                "{types}operator f{{x: figure, y: figure}}: boolean [ {body} ];{MAIN}"
            ));
            assert_eq!(found.len(), 1, "{found:#?}");
            assert_eq!(found[0].code, Code::WrongOperandType);
            assert_eq!(found[0].notes[0], note, "{body}");
        }
    }

    #[test]
    fn a_programs_own_types() {
        // Declared in any order, nested, and in a heading; selected by the
        // type's name, a component read with a dot, and compared, all
        // folded.
        let source = "var here := place{name: \"Lima\", at: position{latitude: -12.05e0, longitude: -77.04e0}};
var latitude := here.at.latitude;
var same := here.at = position{latitude: -12.05e0, longitude: -77.04e0};
var cities: relation city := relation { { name: \"Lima\", at: here.at } };
heading city := { name: text, at: position };
type place { { name: text, at: position } };
type position { { latitude: approximate, longitude: approximate } };
";
        let approximate = || hir::Type::Scalar(hir::Scalar::Approximate);
        let position = hir::Defined {
            name: "position".to_string(),
            unit: "program".to_string(),
            components: hir::Heading::new(
                [
                    ("latitude".to_string(), approximate()),
                    ("longitude".to_string(), approximate()),
                ]
                .into(),
            ),
            representations: Vec::new(),
            constraints: Vec::new(),
            keys: BTreeMap::new(),
            order: None,
            supertypes: Vec::new(),
            specialized: false,
            specialization: None,
        };
        let place = hir::Defined {
            name: "place".to_string(),
            unit: "program".to_string(),
            components: hir::Heading::new(
                [
                    (
                        "at".to_string(),
                        hir::Type::Defined(Box::new(position.clone())),
                    ),
                    ("name".to_string(), hir::Type::Scalar(hir::Scalar::Text)),
                ]
                .into(),
            ),
            representations: Vec::new(),
            constraints: Vec::new(),
            keys: BTreeMap::new(),
            order: None,
            supertypes: Vec::new(),
            specialized: false,
            specialization: None,
        };
        let vars = variables(source);
        assert_eq!(vars[0].ty, hir::Type::Defined(Box::new(place)));
        assert_eq!(
            vars[0].init.to_string(),
            "place{at: position{latitude: -1.205e1, longitude: -7.704e1}, name: \"Lima\"}"
        );
        assert_eq!(vars[1].init.to_string(), "-1.205e1");
        assert_eq!(vars[2].init, hir::Value::Boolean(true));
        assert_eq!(
            vars[3].ty,
            hir::Type::Relation(hir::Heading::new(
                [
                    (
                        "at".to_string(),
                        hir::Type::Defined(Box::new(position.clone()))
                    ),
                    ("name".to_string(), hir::Type::Scalar(hir::Scalar::Text)),
                ]
                .into()
            ))
        );
        // The selector is a read-only operator named after the type, which
        // a call with values known only when running calls.
        let program = analyze_str(&format!(
            "{source}operator north{{p: position}}: position [ position{{latitude: p.latitude + 1e0, longitude: p.longitude}} ];{MAIN}"
        ))
        .unwrap();
        let selector = program
            .operators
            .iter()
            .find(|op| op.name == "position")
            .unwrap();
        let hir::OperatorKind::ReadOnly { result, value, .. } = &selector.kind else {
            panic!("a read-only operator");
        };
        assert_eq!(*result, hir::Type::Defined(Box::new(position.clone())));
        assert!(matches!(value, hir::Expr::Select(defined, _) if **defined == position));
        assert_eq!(
            selector.params.keys().collect::<Vec<_>>(),
            ["latitude", "longitude"]
        );
    }

    #[test]
    fn a_programs_own_types_errors() {
        let celsius = "type celsius { { degrees: approximate } };\n";
        // A component that isn't one, fixed to the closest.
        let source =
            format!("{celsius}var t := celsius{{degrees: 1e0}};\nvar d := t.degres;{MAIN}");
        assert_eq!(messages(&source), ["unknown component `degres`"]);
        assert_eq!(
            fix(&source),
            format!("{celsius}var t := celsius{{degrees: 1e0}};\nvar d := t.degrees;{MAIN}")
        );
        // A missing component is a missing argument, a lone one without a
        // name named, and no default.
        assert_eq!(
            codes(&format!("{celsius}var t := celsius{{}};{MAIN}")),
            [Code::MissingAttribute]
        );
        assert_eq!(
            fix(&format!("{celsius}var t := celsius{{1e0}};{MAIN}")),
            format!("{celsius}var t := celsius{{degrees: 1e0}};{MAIN}")
        );
        assert_eq!(
            codes(&format!("{celsius}var t: celsius;{MAIN}")),
            [Code::UntypedVariable]
        );
        assert_eq!(
            codes(&format!("{celsius}var t: tuple {{ c: celsius }};{MAIN}")),
            [Code::UntypedVariable]
        );
        // No order, and no join.
        assert_eq!(
            codes(&format!(
                "{celsius}operator f{{a: celsius, b: celsius}}: boolean [ a < b ];{MAIN}"
            )),
            [Code::WrongOperandType]
        );
        // A tuple of its name, fixed to its selector.
        let source = format!("{celsius}var t := tuple celsius {{ degrees: 1e0 }};{MAIN}");
        assert_eq!(messages(&source), ["`celsius` is a type, not a heading"]);
        assert_eq!(
            fix(&source),
            format!("{celsius}var t := celsius{{degrees: 1e0}};{MAIN}")
        );
        // Its name is its selector's: another operator of its unit can't
        // have it, which then gives way.
        let source = format!(
            "{celsius}operator celsius{{f: approximate}}: celsius [ celsius{{degrees: f}} ];{MAIN}"
        );
        assert_eq!(codes(&source), [Code::SelectorName]);
        // Defined once, and not as a heading.
        assert_eq!(
            codes(&format!(
                "{celsius}{celsius}heading celsius := {{ a: integer }};{MAIN}"
            )),
            [Code::DuplicateName, Code::DuplicateName]
        );
        // Not containing itself, directly or through a heading.
        assert_eq!(
            messages(&format!("type route {{ {{ rest: route }} }};{MAIN}")),
            ["the type `route` contains itself"]
        );
        assert_eq!(
            messages(&format!(
                "heading leg := {{ to: route }};\ntype route {{ {{ legs: relation leg }} }};{MAIN}"
            )),
            ["`leg` and `route` contain each other"]
        );
    }

    #[test]
    fn a_programs_own_types_components_are_updated() {
        // `update` gives components new values, with the components in
        // scope, and keeps the others; two of different components in a
        // block merge. Folded in value blocks.
        let source = "type position { { latitude: approximate, longitude: approximate } };
var moved := [
    var p := position{latitude: 1e0, longitude: 2e0};
    update p { latitude := latitude + longitude };
    p
];
var swapped := [
    var p := position{latitude: 1e0, longitude: 2e0};
    assign {
        update p { latitude := longitude };
        update p { longitude := latitude };
    };
    p
];
";
        let vars = variables(source);
        assert_eq!(
            vars[0].init.to_string(),
            "position{latitude: 3e0, longitude: 2e0}"
        );
        assert_eq!(
            vars[1].init.to_string(),
            "position{latitude: 2e0, longitude: 1e0}"
        );
        // A variable's, known only when running, is selected from its
        // components, updated.
        let program = analyze_str(&format!(
            "{source}operator north{{var p: position}} [ update p {{ latitude := latitude + 1e0 }}; ];{MAIN}"
        ))
        .unwrap();
        let north = program
            .operators
            .iter()
            .find(|op| op.name == "north")
            .unwrap();
        let hir::OperatorKind::Update { body, .. } = &north.kind else {
            panic!("an update operator");
        };
        let [(_, hir::Statement::Assign(assignments))] = &body.statements[..] else {
            panic!("one assignment: {:?}", body.statements);
        };
        assert!(matches!(
            &assignments[0].value,
            hir::Expr::Select(_, components) if matches!(**components, hir::Expr::Extend(..))
        ));
    }

    #[test]
    fn a_programs_own_types_update_errors() {
        let declared = "type position { { latitude: approximate, longitude: approximate } };
var here := position{latitude: -1.29e0, longitude: 36.82e0};
var stop := tuple { name: \"Nairobi\", at: here };
var stops := array [here];
";
        let main = |body: &str| format!("{declared}operator main{{}} [\n    {body}\n];\n");
        // A component that isn't one, fixed to the closest.
        let source = main("update here { lattitude := 0e0 };");
        assert_eq!(messages(&source), ["unknown component `lattitude`"]);
        assert_eq!(fix(&source), main("update here { latitude := 0e0 };"));
        // A component keeps its type, and is given once.
        assert_eq!(
            codes(&main("update here { latitude := \"south\" };")),
            [Code::TypeMismatch]
        );
        assert_eq!(
            codes(&main("update here { latitude := 0e0, latitude := 1e0 };")),
            [Code::DuplicateAttribute]
        );
        // One that changes nothing, or has a condition.
        assert_eq!(
            messages(&main("update here { latitude := latitude };")),
            ["this `update` gives each component its own value"]
        );
        assert_eq!(codes(&main("update here {};")), [Code::EmptyDoesNothing]);
        assert_eq!(
            codes(&main(
                "update here { latitude := 0e0 } where longitude > 0e0;"
            )),
            [Code::WrongOperandType]
        );
        // Two in a block that give one component a value conflict.
        assert_eq!(
            codes(&main(
                "assign {\n        update here { latitude := 0e0 };\n        update here { latitude := 1e0 };\n    };"
            )),
            [Code::ConflictingChanges]
        );
        // A component assigned on its own, as other languages assign a
        // field, fixed to an `update` of the variable, or an assignment of
        // the element, with a `replace` for a value inside.
        let source = main("here.latitude := 0e0;");
        assert_eq!(
            messages(&source),
            ["component `latitude` can't be assigned on its own"]
        );
        assert_eq!(fix(&source), main("update here { latitude := 0e0 };"));
        assert_eq!(
            fix(&main("stop.at.latitude := 0e0;")),
            main("update stop { at := at replace { latitude := 0e0 } };")
        );
        assert_eq!(
            fix(&main("stops[1].latitude := 0e0;")),
            main("stops[1] := stops[1] replace { latitude := 0e0 };")
        );
        // None when the value names a component, which `update` or
        // `replace` brings into scope.
        let source = main("here.latitude := longitude;");
        assert_eq!(fix(&source), source);
        let source = main("stop.at.latitude := longitude;");
        assert_eq!(fix(&source), source);
    }

    #[test]
    fn a_programs_own_types_components_are_replaced() {
        // `replace` is the value with components given new values, each
        // computed with the components in scope, all at once, and the
        // others kept: of a value nested in another, or in a relation's
        // tuples, too.
        let source = "type position { { latitude: approximate, longitude: approximate } };
type journey { { origin: position, destination: position } };
var here := position{latitude: 1e0, longitude: 2e0};
var flight := journey{origin: here, destination: position{latitude: 3e0, longitude: 4e0}};
var equator := here replace { latitude := 0e0 };
var back := flight replace { origin := destination, destination := origin };
var east := flight replace { destination := destination replace { longitude := longitude + 1e0 } };
var landmarks := relation { { name: \"Petra\", at: here } } replace { at := at replace { latitude := -latitude } };
";
        let vars = variables(source);
        assert_eq!(
            vars[2].init.to_string(),
            "position{latitude: 0e0, longitude: 2e0}"
        );
        assert_eq!(
            vars[3].init.to_string(),
            "journey{destination: position{latitude: 1e0, longitude: 2e0}, origin: position{latitude: 3e0, longitude: 4e0}}"
        );
        assert_eq!(
            vars[4].init.to_string(),
            "journey{destination: position{latitude: 3e0, longitude: 5e0}, origin: position{latitude: 1e0, longitude: 2e0}}"
        );
        assert_eq!(
            vars[5].init.to_string(),
            "relation { at: position, name: text } { { at: position{latitude: -1e0, longitude: 2e0}, name: \"Petra\" } }"
        );
        // A value known only when running is selected from its components,
        // replaced.
        let program = analyze_str(&format!(
            "{source}operator north{{p: position}}: position [ p replace {{ latitude := latitude + 1e0 }} ];{MAIN}"
        ))
        .unwrap();
        let north = program
            .operators
            .iter()
            .find(|op| op.name == "north")
            .unwrap();
        let hir::OperatorKind::ReadOnly { value, .. } = &north.kind else {
            panic!("a read-only operator");
        };
        let hir::Expr::At(_, value) = value else {
            panic!("the body's value, where it is: {value:?}");
        };
        assert!(matches!(
            &**value,
            hir::Expr::Select(_, components) if matches!(**components, hir::Expr::Extend(..))
        ));
    }

    #[test]
    fn a_programs_own_types_replace_errors() {
        let declared = "type position { { latitude: approximate, longitude: approximate } };
var here := position{latitude: -1.29e0, longitude: 36.82e0};
";
        let var = |value: &str| format!("{declared}var v := {value};{MAIN}");
        // A component that isn't one, fixed to the closest, with no `extend`
        // to suggest.
        let source = var("here replace { lattitude := 0e0 }");
        assert_eq!(messages(&source), ["unknown component `lattitude`"]);
        assert_eq!(fix(&source), var("here replace { latitude := 0e0 }"));
        // A component keeps its type, and is given once.
        assert_eq!(
            codes(&var("here replace { latitude := \"south\" }")),
            [Code::TypeMismatch]
        );
        assert_eq!(
            codes(&var("here replace { latitude := 0e0, latitude := 1e0 }")),
            [Code::DuplicateAttribute]
        );
        // One that changes nothing, fixed by dropping it.
        let source = var("here replace { latitude := latitude }");
        assert_eq!(
            messages(&source),
            ["this `replace` gives each component its own value"]
        );
        assert_eq!(fix(&source), var("here"));
        assert_eq!(fix(&var("here replace {}")), var("here"));
        // The type's components are the ones it declares: none are added,
        // removed or renamed.
        for changed in [
            "here extend { altitude := 0e0 }",
            "here remove {latitude}",
            "here rename {latitude as lat}",
            "here{latitude}",
        ] {
            assert_eq!(codes(&var(changed)), [Code::WrongOperandType]);
        }
        // A value without components has none to replace.
        assert_eq!(
            messages(&var("1 replace { a := 2 }")),
            ["`replace` can't be applied to integers"]
        );
    }

    #[test]
    fn a_programs_own_types_constraints() {
        let declared = "type percentage { { value: integer }, constraint value >= 0, constraint value <= 100 };\n";
        let var = |value: &str| format!("{declared}var v := {value};{MAIN}");
        // A value whose components satisfy them is selected when compiling,
        // by the selector or `replace`.
        assert_eq!(
            variables(&format!(
                "{declared}var v := percentage{{value: 20}} replace {{ value := value * 2 }};"
            ))[0]
                .init
                .to_string(),
            "percentage{value: 40}"
        );
        // One that breaks one is reported where it is selected, by the
        // selector, `replace` or `update`, with the first it breaks.
        let source = var("percentage{value: 120}");
        assert_eq!(
            messages(&source),
            ["`percentage{value: 120}` isn't a value of `percentage`"]
        );
        let broken = errors(&source);
        assert_eq!(
            broken[0].primary.message,
            "its components break a constraint of `percentage`"
        );
        assert_eq!(broken[0].secondary[0].message, "false of them");
        for value in [
            "percentage{value: 20} replace { value := -1 }",
            "[\n    var p := percentage{value: 1};\n    update p { value := 101 };\n    p\n]",
        ] {
            assert_eq!(codes(&var(value)), [Code::BrokenConstraint], "{value}");
        }
        // Known only when running, the selector's value checks each, in
        // order, by an operator of the components for each.
        let program = analyze_str(&format!(
            "{declared}operator half{{p: percentage}}: percentage [ p replace {{ value := value div 2 }} ];{MAIN}"
        ))
        .unwrap();
        let selector = program
            .operators
            .iter()
            .find(|op| op.name == "percentage")
            .unwrap();
        let hir::OperatorKind::ReadOnly { value, .. } = &selector.kind else {
            panic!("a read-only operator");
        };
        let hir::Expr::Select(defined, _) = value else {
            panic!("a selection: {value:?}");
        };
        let texts: Vec<&str> = defined
            .constraints
            .iter()
            .map(|c| c.text.as_str())
            .collect();
        assert_eq!(texts, ["value >= 0", "value <= 100"]);
        let checks = &program.operators[defined.constraints[0].operator.0];
        assert!(matches!(
            &checks.kind,
            hir::OperatorKind::ReadOnly {
                result: hir::Type::Scalar(hir::Scalar::Boolean),
                ..
            }
        ));
        assert_eq!(checks.params.keys().collect::<Vec<_>>(), ["value"]);
    }

    #[test]
    fn a_programs_own_types_constraint_errors() {
        let declare = |constraint: &str| {
            format!("type percentage {{ {{ value: integer }}, constraint {constraint} }};{MAIN}")
        };
        // A boolean condition on the components alone.
        assert_eq!(codes(&declare("value + 1")), [Code::TypeMismatch]);
        assert_eq!(
            messages(&format!("var limit := 100;\n{}", declare("value <= limit"))),
            ["`percentage`'s constraint can't read the variable `limit`"]
        );
        // One known when compiling: true, deleted, or false.
        let source = format!(
            "type percentage {{ {{ value: integer }}, constraint value >= 0, constraint 1 = 1 }};{MAIN}"
        );
        assert_eq!(
            messages(&source),
            ["this constraint is true of every value of `percentage`"]
        );
        assert_eq!(
            fix(&source),
            format!("type percentage {{ {{ value: integer }}, constraint value >= 0 }};{MAIN}")
        );
        assert_eq!(
            messages(&declare("1 > 2")),
            ["no value of `percentage` satisfies this constraint"]
        );
        // One that selects a value of its type, directly or through an
        // operator, closes a cycle, reported once.
        assert_eq!(
            messages(&declare("percentage{value: value} <> percentage{value: 0}")),
            ["`percentage`'s constraint selects a value of `percentage`"]
        );
        assert_eq!(
            messages(&format!(
                "operator positive{{n: integer}}: boolean [ percentage{{value: n}} <> percentage{{value: 0}} ];\n{}",
                declare("positive{n: value}")
            )),
            ["`percentage`'s constraint selects a value of `percentage` through `positive`"]
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

    /// A unit that declares one of each kind of name, the same as another's.
    const LAKES: &str = "var count := 1;\nheading place := { name: text };\ntype metres { { m: integer } };\nvar depth := metres{m: 3};\nconstraint positive := count > 0;\noperator twice{n: integer}: integer [ n * 2 ];\noperator bump{} [ count := twice{n: count}; ];";

    /// A unit's start that gives other units something to use, as a used
    /// unit must, for one that has nothing else in its interface.
    const EXPORTS: &str = "interface\n\nheading exported := { a: integer };\n\nimplementation\n\n";

    /// The errors of the program `source`, with the units `a` and `b`, and
    /// `source` with their fixes in it applied.
    fn with_units_a_and_b(source: &str) -> (Vec<Code>, String) {
        let errors =
            analyze_units(&[("program", source), ("a", EXPORTS), ("b", EXPORTS)]).unwrap_err();
        let mut edits: Vec<_> = errors
            .iter()
            .filter_map(|d| d.suggestions.first())
            .flat_map(|s| s.edits.clone())
            .collect();
        edits.sort_by_key(|e| std::cmp::Reverse(e.span.start));
        let mut fixed = source.to_string();
        for edit in edits {
            fixed.replace_range(edit.span.start..edit.span.end, &edit.replacement);
        }
        (errors.iter().map(|e| e.code).collect(), fixed)
    }

    #[test]
    fn a_sections_uses_comes_first_once() {
        let codes = |source| with_units_a_and_b(source).0;
        let fix = |source| with_units_a_and_b(source).1;
        // After a declaration: moved before the section's first one.
        let source = "/* Greets. */\noperator f{} [];\nuses { a };\noperator main{} [];";
        assert_eq!(codes(source), [Code::UsesPlacement]);
        assert_eq!(
            fix(source),
            "uses { a };\n\n/* Greets. */\noperator f{} [];\noperator main{} [];"
        );
        let source = "interface\n\noperator f{};\n\nuses { a };\n\nimplementation\n\nuses { b };\n\noperator f{} [];\n\noperator main{} [];";
        assert_eq!(codes(source), [Code::UsesPlacement]);
        assert_eq!(
            fix(source),
            "interface\n\nuses { a };\n\noperator f{};\n\nimplementation\n\nuses { b };\n\noperator f{} [];\n\noperator main{} [];"
        );
        // A second in a section: its units, but those the first names, added
        // to the first's.
        let source = "uses { a };\nuses { b, a };\noperator main{} [];";
        assert_eq!(codes(source), [Code::UsesPlacement]);
        assert_eq!(fix(source), "uses { a, b };\noperator main{} [];");
        // One in each section.
        let source =
            "interface\n\nuses { a };\n\nimplementation\n\nuses { b };\n\noperator main{} [];";
        assert!(analyze_units(&[("program", source), ("a", EXPORTS), ("b", EXPORTS)]).is_ok());
    }

    /// A unit whose interface declares variables, a constraint on them and a
    /// constant, and whose implementation keeps a variable of its own.
    const COUNTS: &str = "interface\n\nvar total := 0;\n\nvar found: relation transaction_collection;\n\nconstraint counted := total >= 0;\n\nconst limit := 3;\n\nimplementation\n\nvar kept := 0;";

    /// The errors of the program `source`, with the unit `counts`, and
    /// `source` with their first fixes in it applied.
    fn with_counts(source: &str) -> (Vec<Code>, String) {
        let Err(errors) = analyze_units(&[("program", source), ("counts", COUNTS)]) else {
            return (Vec::new(), source.to_string());
        };
        let mut edits: Vec<_> = errors
            .iter()
            .filter_map(|d| d.suggestions.first())
            .flat_map(|s| s.edits.clone())
            .collect();
        edits.sort_by_key(|e| std::cmp::Reverse(e.span.start));
        let mut fixed = source.to_string();
        for edit in edits {
            assert!(edit.span.end <= source.len(), "{source}: {errors:#?}");
            fixed.replace_range(edit.span.start..edit.span.end, &edit.replacement);
        }
        (errors.iter().map(|e| e.code).collect(), fixed)
    }

    #[test]
    fn a_unit_reads_and_assigns_the_variables_another_units_interface_declares() {
        let main = |body: &str| {
            format!(
                "uses {{ counts }};\n\noperator take{{out n: integer}} [ n := 5; ];\n\noperator main{{}} [\n{body}\n];"
            )
        };
        // By its name or qualified: read, assigned, passed and collected into,
        // its unit's constraint checked as any other's.
        let source = main(
            "    total := total + 1;\n    counts::total := counts::total + 1;\n    call take{out n: total};\n    call take{out n: counts::total};\n    transaction collecting counts::found [\n        total := -1;\n    ];\n    call debug_value{v: found};",
        );
        let program = analyze_units(&[("program", &source), ("counts", COUNTS)]).unwrap();
        assert_eq!(program.constraints[0].name, "counted");
        // A unit's own variable of the name hides it, which is then qualified,
        // and each is passed as itself.
        let source = main("    call take{out n: total};\n    call take{out n: counts::total};")
            .replace("uses { counts };", "uses { counts };\n\nvar total := 1;");
        assert!(analyze_units(&[("program", &source), ("counts", COUNTS)]).is_ok());
        // Not what its implementation keeps, nor its constant, nor in a value
        // block.
        for (body, code) in [
            ("    kept := 1;", Code::UnknownName),
            ("    counts::kept := 1;", Code::UnknownName),
            ("    counts::limit := 4;", Code::NotAssignable),
            (
                "    var a := [ counts::total := 1; 2 ];",
                Code::ReadOnlyEffect,
            ),
        ] {
            assert_eq!(with_counts(&main(body)).0, [code], "{body}");
        }
        // A read-only operator is given it, qualified or not, as a parameter,
        // which each call gives it as written.
        for read in ["total", "counts::total"] {
            let source = format!(
                "uses {{ counts }};\n\noperator twice{{}}: integer [ {read} * 2 ];\n\noperator main{{}} [\n    call debug_value{{v: twice{{}}}};\n];"
            );
            let (codes, fixed) = with_counts(&source);
            assert_eq!(codes, [Code::VariableInReadOnly], "{source}");
            assert_eq!(
                fixed,
                format!(
                    "uses {{ counts }};\n\noperator twice{{total: integer}}: integer [ total * 2 ];\n\noperator main{{}} [\n    call debug_value{{v: twice{{total: {read}}}}};\n];"
                )
            );
        }
        // Nor does a read-only operator assign it.
        let source = "uses { counts };\n\noperator f{}: integer [\n    counts::total := 1;\n    1\n];\n\noperator main{} [];";
        assert_eq!(with_counts(source).0, [Code::ReadOnlyEffect]);
        // What holds of it is its own unit's to say, and no database's.
        let source = main("").replace(
            "uses { counts };",
            "uses { counts };\n\nconstraint mine := counts::total < 9;",
        );
        assert_eq!(with_counts(&source).0, [Code::ForeignConstraint]);
        let source = main("").replace(
            "uses { counts };",
            "uses { counts };\n\ndatabase kitchen;\nvar kitchen.pantry: relation { item: text } key { item };\nconstraint kitchen.c := count{r: pantry} < counts::total;",
        );
        assert_eq!(with_counts(&source).0, [Code::DatabasePart]);
    }

    #[test]
    fn passing_a_variable_another_unit_has_one_of_the_name_of_passes_it() {
        // Each unit's `count`, the program's called `program::count`, given to
        // an `out` parameter by the name its unit writes.
        let other = "interface\n\noperator touch{};\n\nimplementation\n\nvar count := 0;\n\noperator touch{} [ count := count + 1; ];";
        let main = "uses { other };\n\nvar count := 0;\n\noperator take{out n: integer} [ n := 5; ];\n\noperator main{} [\n    call take{out n: count};\n    call touch{};\n];";
        let program = analyze_units(&[("program", main), ("other", other)]);
        assert!(program.is_ok(), "{:#?}", program.err());
    }

    /// A unit that declares a type, a heading and operators in its
    /// interface, and an operator its implementation keeps.
    const DEPTHS: &str = "interface\n\ntype metres { { m: integer } };\n\nheading lake := { name: text, depth: metres };\n\noperator deepest{lakes: relation lake}: relation lake;\n\nimplementation\n\noperator deepest{lakes: relation lake}: relation lake [\n    lakes where depth.m = max{r: lakes, of: depth.m}\n];\n\noperator secret{}: integer [ 1 ];";

    #[test]
    fn a_unit_sees_the_interfaces_of_the_units_it_uses() {
        let main = "uses { lakes.depths };\n\nvar l := relation lake { { name: \"Titicaca\", depth: metres{m: 281} } };\nvar d := deepest{lakes: l};\n\noperator main{} [];";
        let program = analyze_units(&[("program", main), ("lakes.depths", DEPTHS)]).unwrap();
        assert_eq!(
            program.variables[1].ty.to_string(),
            "relation { depth: metres, name: text }"
        );
        // Not what its implementation keeps.
        let errors = analyze_units(&[
            (
                "program",
                "uses { lakes.depths };\n\nvar s := secret{};\n\noperator main{} [];",
            ),
            ("lakes.depths", DEPTHS),
        ])
        .unwrap_err();
        assert_eq!(errors.len(), 1, "{errors:#?}");
        assert!(errors[0].message.contains("`secret`"), "{errors:#?}");
        // Nor anything, without the `uses`.
        let errors = analyze_units(&[
            ("program", "var m := metres{m: 1};\n\noperator main{} [];"),
            ("lakes.depths", DEPTHS),
        ])
        .unwrap_err();
        assert_eq!(errors[0].code, Code::UnknownName, "{errors:#?}");
        // What a unit uses is its own: the units that use it don't see it.
        let between = "interface\n\nuses { lakes.depths };\n\nheading named := { name: text };";
        let errors = analyze_units(&[
            (
                "program",
                "uses { between };\n\nvar m := metres{m: 1};\n\noperator main{} [];",
            ),
            ("between", between),
            ("lakes.depths", DEPTHS),
        ])
        .unwrap_err();
        assert_eq!(errors.len(), 1, "{errors:#?}");
        assert_eq!(errors[0].code, Code::UnknownName);
    }

    /// A unit that declares a union type, a specification of it, and a
    /// version for its own subtype.
    /// `main` analyzed with the units `lakes.depths` and `seas.depths`, each
    /// declaring a type `metres`.
    fn with_depths(main: &str) -> Result<hir::Program, Vec<Diagnostic>> {
        analyze_units(&[
            ("program", main),
            ("lakes.depths", DEPTHS),
            (
                "seas.depths",
                "interface\n\ntype metres { { m: integer } };",
            ),
        ])
    }

    #[test]
    fn a_type_or_heading_is_qualified_by_its_units_name() {
        // A clash, settled where the name is written, a heading's too, and
        // a type whose unit hides it by its own, or the prelude's built-in
        // types, as the program calls them.
        let program = with_depths("uses { lakes.depths, seas.depths };\n\ntype metres { { feet: integer } };\n\noperator deep{m: lakes.depths::metres, s: seas.depths::metres, f: metres, n: trendy.prelude::integer}: boolean [ m.m > s.m ];\n\noperator deepest{l: relation lakes.depths::lake}: integer [ count{r: l} ];\n\noperator main{} [];").unwrap();
        let deep = program.operators.iter().find(|o| o.name == "deep").unwrap();
        let types: Vec<String> = ["m", "s", "f", "n"]
            .iter()
            .map(|p| deep.params[*p].ty.to_string())
            .collect();
        assert_eq!(
            types,
            [
                "lakes.depths::metres",
                "seas.depths::metres",
                "program::metres",
                "integer"
            ]
        );
        // A unit by the name its `uses` gives it, which replaces its own
        // there, or by its own, if its section uses it.
        let aliased = "uses { lakes.depths as depths };\n\noperator deep{m: depths::metres}: boolean [ m.m > 1 ];\n\noperator main{} [];";
        assert!(
            with_depths(aliased).is_ok(),
            "{:#?}",
            with_depths(aliased).err()
        );
        // The prelude's, though the unit hides it by its own.
        let collected = "heading transaction_collection := { n: integer };\n\noperator main{out errors: relation trendy.prelude::transaction_collection} [\n    errors := relation trendy.prelude::transaction_collection {};\n];";
        assert!(
            with_depths(collected).is_ok(),
            "{:#?}",
            with_depths(collected).err()
        );
    }

    #[test]
    fn an_operator_is_called_qualified_by_its_units_name() {
        // A unit's own `count`, which hides the prelude's, calls it; a
        // selector of one of two types of a name; and an update operator.
        let tally = "interface\n\noperator note{n: integer};\n\nimplementation\n\noperator note{n: integer} [\n    call debug_value{v: n};\n];";
        let main = "uses { lakes.depths, seas.depths, tally };\n\noperator count{r: any relation}: integer [ trendy.prelude::count{r: r} * 10 ];\n\nvar n := count{r: relation { { x: 1 }, { x: 2 } }};\nvar m := seas.depths::metres{m: 281};\n\noperator main{} [\n    call tally::note{n: n};\n];";
        let units = |main: &str| {
            analyze_units(&[
                ("program", main),
                ("lakes.depths", DEPTHS),
                (
                    "seas.depths",
                    "interface\n\ntype metres { { m: integer } };",
                ),
                ("tally", tally),
            ])
        };
        let program = units(main).unwrap();
        let values: Vec<String> = own_variables(&program)
            .map(|v| v.init.to_string())
            .collect();
        assert_eq!(values, ["20", "seas.depths::metres{m: 281}"]);
        // A clash's help writes each operator qualified.
        let errors = units("uses { lakes.depths, seas.depths };\n\nvar m := metres{m: 281};\n\noperator main{} [];").unwrap_err();
        assert_eq!(errors[0].code, Code::NameClash);
        assert!(
            errors[0].help[0].starts_with(
                "qualify it by the unit meant, `lakes.depths::metres{…}` or `seas.depths::metres{…}`"
            ),
            "{:#?}",
            errors[0].help
        );
        // One the unit doesn't declare is fixed to one it misspells, in an
        // expression or a `call`.
        for (written, fixed) in [
            (
                "var n := trendy.prelude::cont{r: relation { { x: 1 } }};\n\noperator main{} [];",
                "count",
            ),
            ("operator main{} [\n    call tally::nte{n: 1};\n];", "note"),
        ] {
            let errors = units(&format!("uses {{ tally }};\n\n{written}")).unwrap_err();
            assert_eq!(errors.len(), 1, "{errors:#?}");
            assert_eq!(errors[0].code, Code::UnknownName);
            assert_eq!(errors[0].suggestions[0].edits[0].replacement, fixed);
        }
    }

    #[test]
    fn a_qualifier_names_a_unit_the_section_uses() {
        let fixed = |main: &str| -> (Vec<Code>, String) {
            let errors = with_depths(main).unwrap_err();
            let codes = errors.iter().map(|e| e.code).collect();
            let mut edits: Vec<trendy_diag::Edit> = errors
                .iter()
                .filter_map(|d| d.suggestions.first())
                .flat_map(|s| s.edits.clone())
                .collect();
            edits.sort_by_key(|e| std::cmp::Reverse(e.span.start));
            let mut fixed = main.to_string();
            for edit in edits {
                fixed.replace_range(edit.span.start..edit.span.end, &edit.replacement);
            }
            (codes, fixed)
        };
        let operator =
            |t: &str| format!("operator deep{{m: {t}}}: integer [ 1 ];\n\noperator main{{}} [];");
        for (main, codes, after) in [
            // Its full name, where its `uses` gives it another.
            (
                format!(
                    "uses {{ lakes.depths as depths }};\n\n{}",
                    operator("lakes.depths::metres")
                ),
                vec![Code::Qualifier],
                format!(
                    "uses {{ lakes.depths as depths }};\n\n{}",
                    operator("depths::metres")
                ),
            ),
            // A unit its section doesn't use.
            (
                operator("lakes.depths::metres"),
                vec![Code::Qualifier],
                format!(
                    "uses {{ lakes.depths }};\n\n{}",
                    operator("lakes.depths::metres")
                ),
            ),
            // Its own, whose names it never qualifies.
            (
                format!(
                    "type metres {{ {{ m: integer }} }};\n{}",
                    operator("program::metres")
                ),
                vec![Code::Qualifier],
                format!(
                    "type metres {{ {{ m: integer }} }};\n{}",
                    operator("metres")
                ),
            ),
            // A misspelling of a unit, and of what it declares.
            (
                format!(
                    "uses {{ lakes.depths }};\n\n{}",
                    operator("lake.depths::metres")
                ),
                vec![Code::UnknownName],
                format!(
                    "uses {{ lakes.depths }};\n\n{}",
                    operator("lakes.depths::metres")
                ),
            ),
            (
                format!(
                    "uses {{ lakes.depths }};\n\n{}",
                    operator("lakes.depths::meters")
                ),
                vec![Code::UnknownType],
                format!(
                    "uses {{ lakes.depths }};\n\n{}",
                    operator("lakes.depths::metres")
                ),
            ),
        ] {
            assert_eq!(fixed(&main), (codes, after), "{main}");
        }
        // What its implementation declares, which other units don't see.
        let errors = with_depths(&format!(
            "uses {{ lakes.depths }};\n\n{}",
            operator("lakes.depths::secret")
        ))
        .unwrap_err();
        assert_eq!(errors[0].message, "unknown type `lakes.depths::secret`");
    }

    const FIGURES_UNIT: &str = "interface\n\ntype figure union;\n\ntype square is figure { { side: approximate } };\n\noperator area{f: figure}: approximate;\n\nimplementation\n\noperator area{f: square}: approximate [ f.side * f.side ];";

    /// A unit that uses `figures`, with a subtype of its own, and its version.
    const DISCS_UNIT: &str = "interface\n\nuses { figures };\n\ntype disc is figure { { r: approximate } };\n\nimplementation\n\noperator area{f: disc}: approximate [ 3e0 * f.r * f.r ];";

    #[test]
    fn a_unit_gives_versions_of_a_specification_it_sees() {
        let main = "uses { figures, discs };\n\nvar a := area{f: disc{r: 2e0}};\nvar b := area{f: square{side: 2e0}};\n\noperator main{} [];";
        let program = analyze_units(&[
            ("program", main),
            ("figures", FIGURES_UNIT),
            ("discs", DISCS_UNIT),
        ])
        .unwrap();
        let values: Vec<String> = own_variables(&program)
            .map(|v| v.init.to_string())
            .collect();
        assert_eq!(values, ["1.2e1", "4e0"]);
        // One family, named as its specification.
        let areas: Vec<&str> = program
            .operators
            .iter()
            .filter(|o| o.name.ends_with("area"))
            .map(|o| o.name.as_str())
            .collect();
        assert_eq!(areas, ["area", "area", "area"]);
        // Each subtype has a version, whichever unit gives it.
        let errors = analyze_units(&[
            ("program", "uses { figures, discs };\n\noperator main{} [];"),
            ("figures", FIGURES_UNIT),
            (
                "discs",
                "interface\n\nuses { figures };\n\ntype disc is figure { { r: approximate } };",
            ),
        ])
        .unwrap_err();
        assert_eq!(
            errors.iter().map(|e| e.code).collect::<Vec<_>>(),
            [Code::Versions],
            "{errors:#?}"
        );
        // And only one: as units are in no order, each of several is
        // reported, naming the others.
        let errors = analyze_units(&[
            ("program", "uses { figures, discs };\n\noperator area{f: disc}: approximate [ 0e0 ];\n\noperator main{} [];"),
            ("figures", FIGURES_UNIT),
            ("discs", DISCS_UNIT),
        ])
        .unwrap_err();
        assert_eq!(
            errors.iter().map(|e| e.code).collect::<Vec<_>>(),
            [Code::Versions, Code::Versions],
            "{errors:#?}"
        );
        for (error, other) in errors.iter().zip(["discs", "program"]) {
            assert_eq!(
                error.message,
                "`area{f: disc}` has a version in each of `discs` and `program`"
            );
            assert_eq!(error.secondary.len(), 1);
            assert_eq!(error.secondary[0].message, format!("another, in `{other}`"));
        }
        // Whatever order the program's units are given in.
        let reordered = analyze_units(&[
            ("program", "uses { figures, discs };\n\noperator area{f: disc}: approximate [ 0e0 ];\n\noperator main{} [];"),
            ("discs", DISCS_UNIT),
            ("figures", FIGURES_UNIT),
        ])
        .unwrap_err();
        let messages = |errors: &[Diagnostic]| -> Vec<(String, Vec<String>)> {
            errors
                .iter()
                .map(|e| {
                    let secondary = e.secondary.iter().map(|l| l.message.clone()).collect();
                    (e.message.clone(), secondary)
                })
                .collect()
        };
        assert_eq!(messages(&reordered), messages(&errors));
        // An operator of a name and parameters' names it sees, but no
        // version of it, is the unit's own, which hides the other there,
        // and each is named by its unit in the program.
        let program = analyze_units(&[
            ("program", "uses { figures };\n\noperator area{f: integer}: approximate [ 0e0 ];\nvar a := area{f: 1};\n\noperator main{} [];"),
            ("figures", FIGURES_UNIT),
            ("discs", DISCS_UNIT),
        ])
        .unwrap();
        assert_eq!(program.variables[0].init.to_string(), "0e0");
        let areas: BTreeSet<&str> = program
            .operators
            .iter()
            .filter(|o| o.name.ends_with("area"))
            .map(|o| o.name.as_str())
            .collect();
        assert_eq!(areas, BTreeSet::from(["figures::area", "program::area"]));
    }

    #[test]
    fn a_name_of_a_unit_not_used_gets_its_uses() {
        // The errors of `main`, with `units`, and `main` with each one's
        // first fix in it.
        let fixed = |main: &str, units: &[(&str, &str)]| {
            let mut all = vec![("program", main)];
            all.extend_from_slice(units);
            let errors = analyze_units(&all).unwrap_err();
            let mut edits: Vec<_> = errors
                .iter()
                .filter_map(|d| d.suggestions.first())
                .flat_map(|s| s.edits.clone())
                .filter(|e| e.span.end <= main.len())
                .collect();
            edits.sort_by_key(|e| std::cmp::Reverse(e.span.start));
            edits.dedup();
            let mut text = main.to_string();
            for edit in edits {
                text.replace_range(edit.span.start..edit.span.end, &edit.replacement);
            }
            (errors, text)
        };
        // A type, a heading and an operator, each the interface of one unit
        // declares: the fix uses it, before the first declaration.
        let main = "/* Lakes. */\nvar n: metres;\n\noperator main{} [];";
        let (errors, text) = fixed(main, &[("lakes.depths", DEPTHS)]);
        assert_eq!(errors[0].message, "unknown type `metres`");
        assert!(errors[0].notes[0].contains("`lakes.depths`"), "{errors:#?}");
        assert_eq!(
            text,
            "uses { lakes.depths };\n\n/* Lakes. */\nvar n: metres;\n\noperator main{} [];"
        );
        let main = "uses { figures };\n\nvar l: relation lake;\nvar d := deepest{lakes: l};\n\noperator main{} [];";
        let (errors, text) = fixed(
            main,
            &[
                ("lakes.depths", DEPTHS),
                ("figures", FIGURES_UNIT),
                ("discs", DISCS_UNIT),
            ],
        );
        assert_eq!(errors.len(), 2, "{errors:#?}");
        assert_eq!(
            text,
            "uses { figures, lakes.depths };\n\nvar l: relation lake;\nvar d := deepest{lakes: l};\n\noperator main{} [];"
        );
        // A call without arguments, `f{}`, which no operator it sees has.
        let rivers = "interface\n\noperator longest{}: text;\n\nimplementation\n\noperator longest{}: text [ \"Nile\" ];";
        let (errors, text) = fixed(
            "operator main{} [\n    call debug_value{v: longest{}};\n];",
            &[("lakes.rivers", rivers)],
        );
        assert_eq!(
            errors[0].message, "unknown operator `longest`",
            "{errors:#?}"
        );
        assert_eq!(
            text,
            "uses { lakes.rivers };\n\noperator main{} [\n    call debug_value{v: longest{}};\n];"
        );
        // In the interface, its `uses`, after its header.
        let main = "interface\n\noperator count_of{lakes: relation lake}: integer;\n\nimplementation\n\noperator count_of{lakes: relation lake}: integer [ count{r: lakes} ];\n\noperator main{} [];";
        let (_, text) = fixed(main, &[("lakes.depths", DEPTHS)]);
        assert!(
            text.starts_with("interface\n\nuses { lakes.depths };\n\noperator count_of"),
            "{text}"
        );
        // Of several units, help naming them.
        let seas = "interface\n\ntype metres { { m: integer } };";
        let (errors, text) = fixed(
            "var n: metres;\n\noperator main{} [];",
            &[("lakes.depths", DEPTHS), ("seas.depths", seas)],
        );
        assert!(errors[0].suggestions.is_empty(), "{errors:#?}");
        assert!(
            errors[0].notes[0].contains("`lakes.depths` and `seas.depths`"),
            "{errors:#?}"
        );
        assert_eq!(text, "var n: metres;\n\noperator main{} [];");
        // None that uses this unit, as no unit uses itself.
        let (errors, _) = fixed(
            "var n: metres;\n\noperator main{} [];",
            &[(
                "lakes.depths",
                &DEPTHS.replace(
                    "implementation\n\n",
                    "implementation\n\nuses { program };\n\n",
                ),
            )],
        );
        assert!(
            errors.iter().all(|e| e.suggestions.is_empty()),
            "{errors:#?}"
        );
    }

    #[test]
    fn an_interface_sees_its_own_uses() {
        // The implementation sees both sections' `uses`, the interface only
        // its own: the fix uses the unit in the interface instead.
        let source = "interface\n\noperator count_of{lakes: relation lake}: integer;\n\nimplementation\n\nuses { lakes.depths };\n\noperator count_of{lakes: relation lake}: integer [ count{r: lakes} ];\n\noperator main{} [];";
        let errors = analyze_units(&[("program", source), ("lakes.depths", DEPTHS)]).unwrap_err();
        assert_eq!(errors.len(), 1, "{errors:#?}");
        assert_eq!(errors[0].code, Code::InterfaceDeclaration);
        assert_eq!(
            errors[0].message,
            "`lake` comes from `lakes.depths`, which only the implementation uses"
        );
        let mut edits = errors[0].suggestions[0].edits.clone();
        edits.sort_by_key(|e| std::cmp::Reverse(e.span.start));
        let mut fixed = source.to_string();
        for edit in edits {
            fixed.replace_range(edit.span.start..edit.span.end, &edit.replacement);
        }
        assert_eq!(
            fixed,
            "interface\n\nuses { lakes.depths };\n\noperator count_of{lakes: relation lake}: integer;\n\nimplementation\n\noperator count_of{lakes: relation lake}: integer [ count{r: lakes} ];\n\noperator main{} [];"
        );
        assert!(analyze_units(&[("program", &fixed), ("lakes.depths", DEPTHS)]).is_ok());
    }

    #[test]
    fn a_name_two_units_declare_clashes_where_it_is_written() {
        let seas = "interface\n\ntype metres { { m: integer } };\n\noperator deepest{lakes: relation { name: text }}: integer;\n\nimplementation\n\noperator deepest{lakes: relation { name: text }}: integer [ 0 ];";
        let units = |main: &str| {
            analyze_units(&[
                ("program", main),
                ("lakes.depths", DEPTHS),
                ("seas.depths", seas),
            ])
        };
        // Unwritten, no clash.
        assert!(units("uses { lakes.depths, seas.depths };\n\noperator main{} [];").is_ok());
        // Written, a clash, at each place it is.
        let errors = units("uses { lakes.depths, seas.depths };\n\nvar m := metres{m: 1};\nvar n: metres;\n\noperator main{} [];").unwrap_err();
        assert_eq!(errors.len(), 2, "{errors:#?}");
        assert!(
            errors.iter().all(|e| e.code == Code::NameClash),
            "{errors:#?}"
        );
        assert_eq!(
            errors[0].message,
            "`metres` is declared by `lakes.depths` and `seas.depths`, which this unit uses"
        );
        assert_eq!(errors[0].secondary.len(), 2);
        // Qualified by the unit meant, a type is that unit's.
        assert!(
            errors[1].help[0].starts_with(
                "qualify it by the unit meant, `lakes.depths::metres` or `seas.depths::metres`"
            ),
            "{:#?}",
            errors[1].help
        );
        // Whatever order they are used, or given, in.
        let reordered = analyze_units(&[
            ("program", "uses { seas.depths, lakes.depths };\n\nvar m := metres{m: 1};\nvar n: metres;\n\noperator main{} [];"),
            ("seas.depths", seas),
            ("lakes.depths", DEPTHS),
        ])
        .unwrap_err();
        assert_eq!(reordered[0].message, errors[0].message);
        let at = |e: &Diagnostic| {
            e.secondary
                .iter()
                .map(|l| l.message.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(at(&reordered[0]), at(&errors[0]));
        // The unit's own hides a used unit's, as a unit never qualifies
        // its own names.
        let program = units("uses { seas.depths };\n\ntype metres { { m: integer, deep: boolean } };\nvar m := metres{m: 1, deep: false};\nvar n: metres := m;\n\noperator main{} [];").unwrap();
        assert_eq!(program.variables[1].ty.to_string(), "program::metres");
        // Operators of one name and the same parameters' names clash;
        // those of others are overloads.
        let errors = units("uses { lakes.depths, seas.depths };\n\noperator main{} [ call debug_value{v: deepest{lakes: relation { { name: \"Tana\" } }}}; ];").unwrap_err();
        assert_eq!(errors[0].code, Code::NameClash, "{errors:#?}");
        assert_eq!(
            errors[0].message,
            "`deepest{lakes}` is declared by `lakes.depths` and `seas.depths`, which this unit uses"
        );
        assert!(units("uses { seas.depths };\n\noperator deepest{seas: integer}: integer [ seas ];\nvar d := deepest{seas: 1};\n\noperator main{} [];").is_ok());
        // Each declaration has its own name in the program.
        let program =
            units("uses { lakes.depths };\n\nvar m := metres{m: 1};\n\noperator main{} [];")
                .unwrap();
        assert_eq!(program.variables[0].ty.to_string(), "lakes.depths::metres");
    }

    /// The errors of the first of `units`, checked as a unit, not a program.
    fn checked_unit(units: &[(&str, &str)]) -> Vec<Diagnostic> {
        with_library(None, |library| {
            let parsed: Vec<(String, SourceFile)> = units
                .iter()
                .map(|(name, source)| ((*name).to_string(), parse(source)))
                .collect();
            let files: Vec<File> = parsed
                .iter()
                .map(|(name, tree)| File {
                    name,
                    path: name,
                    tree,
                })
                .collect();
            check_unit(&files, library)
        })
    }

    /// The value of `expression`, of `trendy.time`, folded.
    fn time_value(expression: &str) -> String {
        let vars = variables(&format!("uses {{ trendy.time }};\nvar a := {expression};"));
        vars[0].init.to_string()
    }

    #[test]
    fn dates_count_days_from_1970() {
        // As Python's `datetime` has them.
        for (year, month, day, days) in [
            (1970, 1, 1, 0),
            (1969, 12, 31, -1),
            (2000, 2, 29, 11_016),
            (1900, 3, 1, -25_508),
            (1600, 2, 29, -135_081),
            (1, 1, 1, -719_162),
            (9999, 12, 31, 2_932_896),
            (2024, 3, 20, 19_802),
        ] {
            let selected = format!("date{{year: {year}, month: {month}, day: {day}}}");
            assert_eq!(time_value(&format!("{selected}.days")), days.to_string());
            let back = format!("date{{days: {days}}}");
            assert_eq!(time_value(&format!("{back}.year")), year.to_string());
            assert_eq!(time_value(&format!("{back}.month")), month.to_string());
            assert_eq!(time_value(&format!("{back}.day")), day.to_string());
        }
        // Year 0 is 1 BC, a leap year, and before it, the years go on down.
        assert_eq!(time_value("date{year: 0, month: 2, day: 29}.year"), "0");
        assert_eq!(time_value("date{year: -4, month: 2, day: 29}.day"), "29");
        for broken in [
            "date{year: 2023, month: 2, day: 29}",
            "date{year: 1900, month: 2, day: 29}",
            "date{year: 2024, month: 13, day: 1}",
            "date{year: 2024, month: 4, day: 31}",
            "date{year: 2024, month: 1, day: 0}",
            "date{days: 10_000_000_000_001}",
            "date{days: -10_000_000_000_001}",
            "date{year: 30_000_000_001, month: 1, day: 1}",
            "date{year: 30_000_000_000, month: 1, day: 1}",
            // Far enough out to overflow, were it not for the bound.
            "date{year: 1_000_000_000_000_000_000, month: 1, day: 1}",
            "time{nanos_of_day: -1}",
            "time{nanos_of_day: 86_400_000_000_000}",
            "time{hour: 24, minute: 0, second: 0, nanosecond: 0}",
            "time{hour: 0, minute: 60, second: 0, nanosecond: 0}",
            "time{hour: 23, minute: 59, second: 60, nanosecond: 0}",
            // Not only at the day's end, where it would run past it anyway.
            "time{hour: 12, minute: 0, second: 60, nanosecond: 0}",
            "offset_timestamp{year: 2024, month: 3, day: 5, hour: 12, minute: 0, second: 60, nanosecond: 0, utc_offset: 0}",
            "offset_timestamp{year: 2024, month: 3, day: 5, hour: 12, minute: 60, second: 0, nanosecond: 0, utc_offset: 0}",
            "offset_timestamp{year: 2024, month: 3, day: 5, hour: 24, minute: 0, second: 0, nanosecond: 0, utc_offset: 0}",
            "offset_timestamp{year: 2024, month: 2, day: 30, hour: 0, minute: 0, second: 0, nanosecond: 0, utc_offset: 0}",
            "offset_timestamp{year: 2024, month: 13, day: 1, hour: 0, minute: 0, second: 0, nanosecond: 0, utc_offset: 0}",
            "offset_timestamp{year: 2024, month: 3, day: 5, hour: 0, minute: 0, second: 0, nanosecond: 1_000_000_000, utc_offset: 0}",
            "time{hour: 0, minute: 0, second: 0, nanosecond: 1_000_000_000}",
        ] {
            let source = format!("uses {{ trendy.time }};\nvar a := {broken};{MAIN}");
            assert_eq!(codes(&source), [Code::BrokenConstraint], "{broken}");
        }
        // The date first, whatever the times, then the time.
        let at = |days: i64, nanos: i64| {
            format!("timestamp{{date: date{{days: {days}}}, time: time{{nanos_of_day: {nanos}}}}}")
        };
        assert_eq!(time_value(&format!("{} < {}", at(1, 5), at(2, 0))), "true");
        assert_eq!(time_value(&format!("{} < {}", at(1, 0), at(1, 5))), "true");
        // Each end of the range is a date.
        assert_eq!(
            time_value("date{days: 10_000_000_000_000}.year"),
            "27379072039"
        );
        assert_eq!(
            time_value("date{days: -10_000_000_000_000}.year"),
            "-27379068100"
        );
    }

    #[test]
    fn a_time_is_the_nanoseconds_of_its_day() {
        assert_eq!(
            time_value("time{hour: 0, minute: 0, second: 0, nanosecond: 0}.nanos_of_day"),
            "0"
        );
        assert_eq!(
            time_value(
                "time{hour: 23, minute: 59, second: 59, nanosecond: 999_999_999}.nanos_of_day"
            ),
            "86399999999999"
        );
        for (read, value) in [
            ("hour", 13),
            ("minute", 7),
            ("second", 9),
            ("nanosecond", 5),
        ] {
            assert_eq!(
                time_value(&format!("time{{nanos_of_day: 47_229_000_000_005}}.{read}")),
                value.to_string()
            );
        }
    }

    #[test]
    fn an_offset_is_part_of_the_value_and_moves_the_clock() {
        let at = |date: &str, time: &str| format!("timestamp{{date: {date}, time: {time}}}");
        let noon = at(
            "date{year: 2024, month: 3, day: 5}",
            "time{hour: 12, minute: 0, second: 0, nanosecond: 7}",
        );
        let offset =
            |seconds: i64| format!("offset_timestamp{{timestamp: {noon}, utc_offset: {seconds}}}");
        // Ahead of UTC, the clock shows later, and behind, earlier.
        assert_eq!(time_value(&format!("{}.hour", offset(3_600))), "13");
        assert_eq!(time_value(&format!("{}.hour", offset(-3_600))), "11");
        assert_eq!(time_value(&format!("{}.minute", offset(20_700))), "45");
        assert_eq!(time_value(&format!("{}.nanosecond", offset(20_700))), "7");
        assert_eq!(
            time_value(&format!("{}.utc_offset", offset(20_700))),
            "20700"
        );
        // Past midnight, each way, onto the next month.
        let last = at(
            "date{year: 2024, month: 2, day: 29}",
            "time{hour: 23, minute: 30, second: 0, nanosecond: 0}",
        );
        let first = at(
            "date{year: 2024, month: 3, day: 1}",
            "time{hour: 0, minute: 30, second: 0, nanosecond: 0}",
        );
        let later = format!("offset_timestamp{{timestamp: {last}, utc_offset: 3_600}}");
        assert_eq!(time_value(&format!("{later}.month")), "3");
        assert_eq!(time_value(&format!("{later}.day")), "1");
        assert_eq!(time_value(&format!("{later}.hour")), "0");
        let earlier = format!("offset_timestamp{{timestamp: {first}, utc_offset: -3_600}}");
        assert_eq!(time_value(&format!("{earlier}.month")), "2");
        assert_eq!(time_value(&format!("{earlier}.day")), "29");
        assert_eq!(time_value(&format!("{earlier}.hour")), "23");
        // The same instant at two offsets: two values, and one instant.
        assert_eq!(
            time_value(&format!("{} = {}", offset(0), offset(3_600))),
            "false"
        );
        assert_eq!(
            time_value(&format!(
                "{}.timestamp = {}.timestamp",
                offset(0),
                offset(3_600)
            )),
            "true"
        );
        assert_eq!(
            time_value(&format!("{} < {}", offset(0), offset(3_600))),
            "true"
        );
        // The instant first, whatever the offsets.
        let later_by_a_nanosecond = at(
            "date{year: 2024, month: 3, day: 5}",
            "time{hour: 12, minute: 0, second: 0, nanosecond: 8}",
        );
        assert_eq!(
            time_value(&format!(
                "offset_timestamp{{timestamp: {later_by_a_nanosecond}, utc_offset: 0}} > {}",
                offset(3_600)
            )),
            "true"
        );
        // Selected by the clock, it is the same value.
        let written = "offset_timestamp{year: 2024, month: 3, day: 5, hour: 17, minute: 45, second: 0, nanosecond: 7, utc_offset: 20_700}";
        assert_eq!(
            time_value(&format!("{written} = {}", offset(20_700))),
            "true"
        );
        // Past the last date, read, and selected again, it is what it was.
        let end = at(
            "date{days: 10_000_000_000_000}",
            "time{nanos_of_day: 86_399_999_999_999}",
        );
        let past = format!("offset_timestamp{{timestamp: {end}, utc_offset: 86_399}}");
        assert_eq!(time_value(&format!("{past}.day")), "21");
        let again = format!(
            "offset_timestamp{{year: {past}.year, month: {past}.month, day: {past}.day, hour: {past}.hour, minute: {past}.minute, second: {past}.second, nanosecond: {past}.nanosecond, utc_offset: {past}.utc_offset}} = {past}"
        );
        assert_eq!(time_value(&again), "true");
        for broken in [86_400, -86_400] {
            let source = format!(
                "uses {{ trendy.time }};\nvar a := {};{MAIN}",
                offset(broken)
            );
            assert_eq!(codes(&source), [Code::BrokenConstraint], "{broken}");
        }
    }

    #[test]
    fn a_component_carried_unchanged_is_in_both_representations() {
        let shared = |to: &str, from: &str| {
            format!(
                "type t {{\n    {{ a: integer, b: integer }},\n    {{ c: integer, b: integer }}\n        to {{ a := c, {to} }}\n        from {{ c := a, {from} }},\n}};\n"
            )
        };
        let carried = shared("b := b", "b := b");
        let vars = variables(&format!(
            "{carried}var one := t{{c: 1, b: 2}};\nvar a := one.a;\nvar b := one.b;\nvar same := one = t{{a: 1, b: 2}};"
        ));
        assert_eq!(vars[1].init.to_string(), "1");
        assert_eq!(vars[2].init.to_string(), "2");
        assert_eq!(vars[3].init.to_string(), "true");
        // Anything else makes it two components of one name.
        for (to, from) in [("b := b + 1", "b := b"), ("b := b", "b := b - 1")] {
            let source = format!("{}operator main{{}} [];", shared(to, from));
            let errors = errors(&source);
            assert_eq!(errors[0].code, Code::DuplicateAttribute, "{errors:#?}");
            assert!(errors[0].help[0].contains("`b := b`"), "{errors:#?}");
        }
        // Of two that aren't the stored one, there is nothing to carry.
        let source = "type t {\n    { a: integer },\n    { b: integer } to { a := b } from { b := a },\n    { b: integer, c: integer } to { a := b + c } from { b := a, c := 0 },\n};\noperator main{} [];";
        let errors = errors(source);
        assert_eq!(errors[0].code, Code::DuplicateAttribute, "{errors:#?}");
        assert_eq!(errors[0].help[0], "rename one of them");
    }

    #[test]
    fn other_languages_ways_with_dates_and_times_are_fixed() {
        let start = "uses { trendy.time };\n";
        let fixed = |written: &str| fix(&format!("{start}{written}{MAIN}"));
        for (written, right) in [
            ("var a: datetime := 1;", "var a: timestamp := 1;"),
            ("var a: DateTime := 1;", "var a: timestamp := 1;"),
            ("var a: Instant := 1;", "var a: timestamp := 1;"),
            ("var a: timestamptz := 1;", "var a: timestamp := 1;"),
            (
                "var a: datetimeoffset := 1;",
                "var a: offset_timestamp := 1;",
            ),
            (
                "var a: OffsetDateTime := 1;",
                "var a: offset_timestamp := 1;",
            ),
            ("var a: interval := 1;", "var a: duration := 1;"),
            ("var a: INTERVAL := 1;", "var a: duration := 1;"),
            ("var a: timedelta := 1;", "var a: duration := 1;"),
            ("var a: TimeSpan := 1;", "var a: duration := 1;"),
            (
                "var a: duration := \"PT1H30M\";",
                "var a: duration := duration{seconds: 5400, subsec_nanos: 0};",
            ),
            (
                "var a: duration := \"-PT0.5S\";",
                "var a: duration := duration{seconds: -1, subsec_nanos: 500000000};",
            ),
            (
                "var a: date := \"2024-03-05\";",
                "var a: date := date{year: 2024, month: 3, day: 5};",
            ),
            (
                "var a: time := \"13:07:09.5\";",
                "var a: time := time{hour: 13, minute: 7, second: 9, nanosecond: 500000000};",
            ),
            (
                "var a: timestamp := \"2024-03-05T13:07:09Z\";",
                "var a: timestamp := timestamp{date: date{year: 2024, month: 3, day: 5}, time: time{hour: 13, minute: 7, second: 9, nanosecond: 0}};",
            ),
            (
                "var a: timestamp := \"2024-03-05 13:07:09\";",
                "var a: timestamp := timestamp{date: date{year: 2024, month: 3, day: 5}, time: time{hour: 13, minute: 7, second: 9, nanosecond: 0}};",
            ),
            (
                "var a: offset_timestamp := \"2024-03-05T18:52:09+05:45\";",
                "var a: offset_timestamp := offset_timestamp{year: 2024, month: 3, day: 5, hour: 18, minute: 52, second: 9, nanosecond: 0, utc_offset: 20700};",
            ),
            (
                "var a: offset_timestamp := \"2024-03-05 13:07:09-05:00\";",
                "var a: offset_timestamp := offset_timestamp{year: 2024, month: 3, day: 5, hour: 13, minute: 7, second: 9, nanosecond: 0, utc_offset: -18000};",
            ),
        ] {
            assert_eq!(fixed(written), format!("{start}{right}{MAIN}"), "{written}");
        }
        // Each fix compiles, if what it writes is a value.
        let good = format!("{start}var a: date := \"2024-03-05\";{MAIN}");
        assert!(analyze_str(&fix(&good)).is_ok(), "{}", fix(&good));
        // A date that isn't one is the selector's to report, once it is written.
        let bad = fix(&format!("{start}var a: date := \"2024-02-30\";{MAIN}"));
        assert_eq!(codes(&bad), [Code::BrokenConstraint]);
        // Not of this form, and of no other type, there is only the other way.
        for written in [
            "var a: date := \"March 5\";",
            "var a: date := \"2024-3-5\";",
            "var a: integer := \"2024-03-05\";",
            "var a: timestamp := \"2024-03-05\";",
            "var a: duration := \"P1M\";",
            "var a: duration := \"5 minutes\";",
        ] {
            let source = format!("{start}{written}{MAIN}");
            assert!(
                fix(&source).contains(": text :=") || !fix(&source).contains("year:"),
                "{written}"
            );
        }
        // A program's own `date` isn't `trendy.time`'s.
        let own = "type date { { n: integer } };\nvar a: date := \"2024-03-05\";\n";
        assert!(!fix(&format!("{own}{MAIN}")).contains("year:"));
        // Arithmetic is of a component.
        let errors = errors(&format!("{start}var a := date{{days: 0}} + 1;{MAIN}"));
        assert_eq!(errors[0].code, Code::WrongOperandType);
        assert!(
            errors[0].help[0].contains("`date{days: x.days + 1}`"),
            "{errors:#?}"
        );
        // Of a type of several components, which to compute with isn't known.
        let source = format!(
            "{start}var a := timestamp{{date: date{{days: 0}}, time: time{{nanos_of_day: 0}}}} - 1;{MAIN}"
        );
        let found = self::errors(&source);
        assert_eq!(found[0].code, Code::WrongOperandType);
        assert!(found[0].help[0].contains("`after{t, by:"), "{found:#?}");
        assert!(found[0].suggestions.is_empty(), "{found:#?}");
    }

    /// Instants and durations, as other languages add and subtract them.
    const INSTANTS: &str = "uses { trendy.time };
const a := timestamp{date: date{days: 1}, time: time{nanos_of_day: 0}};
const b := timestamp{date: date{days: 0}, time: time{nanos_of_day: 0}};
const d := seconds{n: 5};
";

    #[test]
    fn arithmetic_on_instants_is_fixed_to_what_it_means() {
        for (written, right, seconds) in [
            (
                "var x := a - b;",
                "var x := elapsed{from: b, to: a};",
                Some(86_400),
            ),
            (
                "var x := b - a;",
                "var x := elapsed{from: a, to: b};",
                Some(-86_400),
            ),
            ("var x := a + d;", "var x := after{t: a, by: d};", None),
            ("var x := d + a;", "var x := after{t: a, by: d};", None),
            ("var x := a - d;", "var x := before{t: a, by: d};", None),
            (
                "var x := after{t: a, by: d} - before{t: b, by: d};",
                "var x := elapsed{from: before{t: b, by: d}, to: after{t: a, by: d}};",
                Some(86_410),
            ),
        ] {
            let fixed = fix(&format!("{INSTANTS}{written}{MAIN}"));
            assert_eq!(fixed, format!("{INSTANTS}{right}{MAIN}"), "{written}");
            // One error, with no other way, for one fix that compiles.
            let source = format!("{INSTANTS}{written}{MAIN}");
            let found = errors(&source);
            assert_eq!(found.len(), 1, "{written}: {found:#?}");
            assert!(found[0].help.is_empty(), "{found:#?}");
            assert!(analyze_str(&fixed).is_ok(), "{fixed}");
            if let Some(seconds) = seconds {
                let vars = variables(&format!("{INSTANTS}{right}\nvar y := x.seconds;"));
                assert_eq!(vars.last().unwrap().init.to_string(), seconds.to_string());
            }
        }
        // What has no such meaning says what there is.
        for (written, help) in [
            ("var x := d + d;", "no arithmetic yet"),
            ("var x := d * 2;", "no arithmetic yet"),
            ("var x := a + 1;", "`after{t, by:"),
            ("var x := a * 2;", "`after{t, by:"),
        ] {
            let found = errors(&format!("{INSTANTS}{written}{MAIN}"));
            assert_eq!(found[0].code, Code::WrongOperandType, "{written}");
            assert!(found[0].help[0].contains(help), "{written}: {found:#?}");
            assert!(found[0].suggestions.is_empty(), "{written}: {found:#?}");
        }
        // A program's own types of those names are its own.
        let own = "type timestamp { { a: integer, b: integer } };
var one := timestamp{a: 1, b: 2};
var two := timestamp{a: 3, b: 4};
var x := one - two;";
        let found = errors(&format!("{own}{MAIN}"));
        assert_eq!(found[0].code, Code::WrongOperandType, "{found:#?}");
        assert!(found[0].help.is_empty(), "{found:#?}");
        assert!(found[0].suggestions.is_empty(), "{found:#?}");
    }

    #[test]
    fn what_nothing_uses_has_no_say_in_a_programs_names() {
        let own = "type duration { { n: integer } };
operator days{n: integer}: integer [ n ];
var a := duration{n: days{n: 1}};
";
        let names = |source: String| {
            let program = analyze_str(&source).unwrap();
            let days: Vec<String> = program
                .operators
                .iter()
                .map(|operator| operator.name.clone())
                .filter(|name| name.ends_with("days"))
                .collect();
            (program.variables[0].ty.to_string(), days)
        };
        let (ty, days) = names(format!("{own}{MAIN}"));
        assert_eq!(ty, "duration");
        assert!(days.contains(&"days".to_string()), "{days:?}");
        // Its own give way, so that each still has one name of its own.
        assert!(days.contains(&"trendy.time::days".to_string()), "{days:?}");
        // Used, each is as ambiguous as any other unit's, and a used unit's uses count.
        let (ty, days) = names(format!("uses {{ trendy.time }};\n{own}{MAIN}"));
        assert_eq!(ty, "program::duration");
        assert!(days.contains(&"program::days".to_string()), "{days:?}");
        assert!(days.contains(&"trendy.time::days".to_string()), "{days:?}");
    }

    #[test]
    fn months_and_years_are_no_durations() {
        for name in ["months", "years", "Months"] {
            let source = format!("uses {{ trendy.time }};\nvar a := {name}{{n: 1}};{MAIN}");
            let found = errors(&source);
            assert_eq!(found[0].code, Code::UnknownName, "{found:#?}");
            assert!(found[0].help[0].contains("`add_months"), "{found:#?}");
        }
    }

    /// The seconds and nanoseconds of the duration `expression`.
    fn duration_parts(expression: &str) -> (String, String) {
        (
            time_value(&format!("{expression}.seconds")),
            time_value(&format!("{expression}.subsec_nanos")),
        )
    }

    #[test]
    fn a_duration_is_exact_and_has_one_form() {
        for (expression, seconds, nanos) in [
            ("nanoseconds{n: 1}", 0_i64, 1),
            ("nanoseconds{n: -1}", -1, 999_999_999),
            ("nanoseconds{n: 1_500_000_000}", 1, 500_000_000),
            ("microseconds{n: -1}", -1, 999_999_000),
            ("microseconds{n: 2_000_001}", 2, 1_000),
            ("milliseconds{n: -1_500}", -2, 500_000_000),
            ("milliseconds{n: 1}", 0, 1_000_000),
            ("seconds{n: -5}", -5, 0),
            ("minutes{n: 2}", 120, 0),
            ("hours{n: -1}", -3_600, 0),
            ("days{n: 2}", 172_800, 0),
            // The smallest of them, which counting down from it would overflow.
            (
                "nanoseconds{n: -9_223_372_036_854_775_807 - 1}",
                -9_223_372_037,
                145_224_192,
            ),
        ] {
            assert_eq!(
                duration_parts(expression),
                (seconds.to_string(), nanos.to_string()),
                "{expression}"
            );
        }
        // However it is written, it is the same value (RM Prescription 8).
        for (a, b, equal) in [
            ("days{n: 1}", "hours{n: 24}", true),
            ("hours{n: 1}", "minutes{n: 60}", true),
            ("minutes{n: 1}", "seconds{n: 60}", true),
            ("seconds{n: 1}", "milliseconds{n: 1_000}", true),
            ("milliseconds{n: 1}", "microseconds{n: 1_000}", true),
            ("microseconds{n: -1}", "nanoseconds{n: -1_000}", true),
            ("seconds{n: 1}", "nanoseconds{n: 1_000_000_001}", false),
            ("seconds{n: 1}", "seconds{n: -1}", false),
        ] {
            assert_eq!(time_value(&format!("{a} = {b}")), equal.to_string(), "{a}");
        }
        // Ordered by how long it is, as it is stored, negative ones too.
        for (a, b) in [
            ("seconds{n: -2}", "seconds{n: -1}"),
            ("seconds{n: -1}", "milliseconds{n: -1}"),
            ("milliseconds{n: -2}", "milliseconds{n: -1}"),
            ("nanoseconds{n: -1}", "nanoseconds{n: 0}"),
            ("nanoseconds{n: 999_999_999}", "seconds{n: 1}"),
            ("seconds{n: 1}", "nanoseconds{n: 1_000_000_001}"),
        ] {
            assert_eq!(time_value(&format!("{a} < {b}")), "true", "{a} < {b}");
            assert_eq!(time_value(&format!("{b} < {a}")), "false", "{b} < {a}");
            assert_eq!(
                time_value(&format!("array [{a}] < array [{b}]")),
                "true",
                "arrays of {a} and {b}"
            );
        }
        // Its limits, and each unit's, which are the same.
        for good in [
            "seconds{n: 2_000_000_000_000_000_000}",
            "seconds{n: -2_000_000_000_000_000_000}",
            "minutes{n: 33_333_333_333_333_333}",
            "hours{n: -555_555_555_555_555}",
            "days{n: 23_148_148_148_148}",
        ] {
            let source = format!("uses {{ trendy.time }};\nvar a := {good};{MAIN}");
            assert!(analyze_str(&source).is_ok(), "{good}");
        }
        for broken in [
            "seconds{n: 2_000_000_000_000_000_001}",
            "seconds{n: -2_000_000_000_000_000_001}",
            "minutes{n: 33_333_333_333_333_334}",
            "hours{n: -555_555_555_555_556}",
            "days{n: 23_148_148_148_149}",
        ] {
            let source = format!("uses {{ trendy.time }};\nvar a := {broken};{MAIN}");
            assert_eq!(codes(&source), [Code::BrokenCallConstraint], "{broken}");
        }
        for broken in [
            "duration{seconds: 0, subsec_nanos: 1_000_000_000}",
            "duration{seconds: 0, subsec_nanos: -1}",
            "duration{seconds: 2_000_000_000_000_000_001, subsec_nanos: 0}",
        ] {
            let source = format!("uses {{ trendy.time }};\nvar a := {broken};{MAIN}");
            assert_eq!(codes(&source), [Code::BrokenConstraint], "{broken}");
        }
    }

    /// The instant `nanos` into the `days`th day after 1970-01-01.
    fn instant(days: i64, nanos: i64) -> String {
        format!("timestamp{{date: date{{days: {days}}}, time: time{{nanos_of_day: {nanos}}}}}")
    }

    #[test]
    fn the_time_between_two_instants_is_elapsed() {
        const LAST_NANO: i64 = 86_399_999_999_999;
        const END: i64 = 10_000_000_000_000;
        for ((from_day, from_nanos), (to_day, to_nanos), seconds, nanos) in [
            ((0, 0), (0, 0), 0_i64, 0),
            ((0, 0), (0, 1), 0, 1),
            ((0, 1), (0, 0), -1, 999_999_999),
            ((0, LAST_NANO), (1, 0), 0, 1),
            ((1, 0), (0, LAST_NANO), -1, 999_999_999),
            ((0, 0), (1, 0), 86_400, 0),
            ((-1, 0), (1, 0), 172_800, 0),
            ((1, 0), (-1, 0), -172_800, 0),
            // Between the ends, which is why this can't fail.
            (
                (-END, 0),
                (END, LAST_NANO),
                1_728_000_000_000_086_399,
                999_999_999,
            ),
            ((END, LAST_NANO), (-END, 0), -1_728_000_000_000_086_400, 1),
        ] {
            let call = format!(
                "elapsed{{from: {}, to: {}}}",
                instant(from_day, from_nanos),
                instant(to_day, to_nanos)
            );
            assert_eq!(
                duration_parts(&call),
                (seconds.to_string(), nanos.to_string()),
                "{call}"
            );
        }
        // Going by what is between two instants gets from one to the other, either way.
        let instants = [
            (0, 0),
            (0, 1),
            (0, LAST_NANO),
            (1, 0),
            (-1, 0),
            (-1, LAST_NANO),
            (19_802, 11_186_000_000_000),
            (-719_162, 5),
            (-END, 0),
            (END, LAST_NANO),
        ];
        for &(a_day, a_nanos) in &instants {
            for &(b_day, b_nanos) in &instants {
                let (a, b) = (instant(a_day, a_nanos), instant(b_day, b_nanos));
                let between = format!("elapsed{{from: {a}, to: {b}}}");
                for check in [
                    format!("after{{t: {a}, by: {between}}} = {b}"),
                    format!("before{{t: {b}, by: {between}}} = {a}"),
                ] {
                    assert_eq!(time_value(&check), "true", "{check}");
                }
            }
        }
        // Past either end there is no such instant.
        for call in [
            format!(
                "after{{t: {}, by: nanoseconds{{n: 1}}}}",
                instant(END, LAST_NANO)
            ),
            format!("before{{t: {}, by: nanoseconds{{n: 1}}}}", instant(-END, 0)),
            format!("after{{t: {}, by: nanoseconds{{n: -1}}}}", instant(-END, 0)),
            format!("before{{t: {}, by: days{{n: -1}}}}", instant(END, 0)),
        ] {
            let source = format!("uses {{ trendy.time }};\nvar a := {call};{MAIN}");
            assert_eq!(codes(&source), [Code::BrokenConstraint], "{call}");
        }
    }

    #[test]
    fn a_month_on_is_the_same_day_or_a_broken_constraint() {
        let on = |year: i64, month: i64, day: i64| {
            format!("date{{year: {year}, month: {month}, day: {day}}}")
        };
        for ((year, month, day), months, right) in [
            ((2024, 1, 15), 1, (2024, 2, 15)),
            ((2024, 1, 15), 0, (2024, 1, 15)),
            ((2024, 12, 31), 1, (2025, 1, 31)),
            ((2024, 1, 31), 2, (2024, 3, 31)),
            ((2024, 3, 15), -3, (2023, 12, 15)),
            ((2024, 1, 15), -25, (2021, 12, 15)),
            ((2024, 1, 15), 24, (2026, 1, 15)),
            ((2024, 2, 29), 48, (2028, 2, 29)),
            ((2023, 1, 29), 13, (2024, 2, 29)),
            ((0, 1, 15), -1, (-1, 12, 15)),
            ((-1, 12, 15), 1, (0, 1, 15)),
        ] {
            let call = format!(
                "add_months{{d: {}, months: {months}}}",
                on(year, month, day)
            );
            assert_eq!(
                time_value(&format!("{call} = {}", on(right.0, right.1, right.2))),
                "true",
                "{call}"
            );
            let check = format!(
                "has_day_after_months{{d: {}, months: {months}}}",
                on(year, month, day)
            );
            assert_eq!(time_value(&check), "true", "{check}");
        }
        for ((year, month, day), months) in [
            ((2024, 1, 31), 1_i64),
            ((2024, 1, 30), 1),
            ((2023, 1, 29), 1),
            ((2024, 1, 31), 3),
            ((2024, 3, 31), -1),
            ((2024, 2, 29), 12),
            ((2024, 2, 29), -12),
            // Beyond a date's range, and where counting them could overflow.
            ((2024, 1, 15), 9_223_372_036_854_775_807),
            ((2024, 1, 15), -9_223_372_036_854_775_807),
            ((2024, 1, 15), 720_000_000_000),
        ] {
            let check = format!(
                "has_day_after_months{{d: {}, months: {months}}}",
                on(year, month, day)
            );
            assert_eq!(time_value(&check), "false", "{check}");
            let source = format!(
                "uses {{ trendy.time }};\nvar a := add_months{{d: {}, months: {months}}};{MAIN}",
                on(year, month, day)
            );
            assert_eq!(codes(&source), [Code::BrokenCallConstraint], "{source}");
        }
        // At the ends of a date's range, a month further is no date.
        for (days, months, is) in [
            (10_000_000_000_000_i64, 1, false),
            (10_000_000_000_000, -1, true),
            (-10_000_000_000_000, -1, false),
            (-10_000_000_000_000, 1, true),
        ] {
            let check =
                format!("has_day_after_months{{d: date{{days: {days}}}, months: {months}}}");
            assert_eq!(time_value(&check), is.to_string(), "{check}");
        }
    }

    /// The program of `interface` and `implementation`, with a `main`.
    fn sectioned(interface: &str, implementation: &str) -> String {
        format!(
            "interface\n\n{interface}\n\nimplementation\n\n{implementation}\n\noperator main{{}} [];\n"
        )
    }

    #[test]
    fn an_interface_sees_only_interfaces() {
        let mention = "the interface mentions";
        let is_small = "operator is_small{a: integer}: boolean [\n    a < 5\n];";
        // Each thing an interface can write, of what its implementation declares.
        for (interface, implementation) in [
            (
                "type t { { a: integer, constraint is_small{a} } };",
                is_small,
            ),
            (
                "type t { { a: integer, constraint is_small{a: a} } };",
                is_small,
            ),
            ("const shown := is_small{a: 1};", is_small),
            (
                "operator f{a: integer, constraint is_small{a: a}};",
                &format!("{is_small}\n\noperator f{{a: integer}} [];"),
            ),
            (
                "type t { { a: integer, constraint a < low } };",
                "const low := 3;",
            ),
            ("const shown := low + 1;", "const low := 3;"),
            (
                "operator f{a: secret};",
                "type secret { { s: integer } };\n\noperator f{a: secret} [];",
            ),
            (
                "type t { { a: secret } };",
                "type secret { { s: integer } };",
            ),
            (
                "heading h := { a: secret };",
                "type secret { { s: integer } };",
            ),
            (
                "operator f{a: relation h};",
                "heading h := { x: integer };\n\noperator f{a: relation h} [];",
            ),
            // A type named in a constraint, not only in a signature.
            (
                "type t { { a: integer, constraint is_empty{r: relation h {}} } };",
                "heading h := { x: integer };",
            ),
        ] {
            let source = sectioned(interface, implementation);
            let found = errors(&source);
            assert_eq!(
                found[0].code,
                Code::InterfaceDeclaration,
                "{interface}: {found:#?}"
            );
            assert!(
                found[0].message.starts_with(mention),
                "{interface}: {found:#?}"
            );
            let fixed = fix(&source);
            let after = analyze_str(&fixed);
            assert!(
                after
                    .as_ref()
                    .err()
                    .is_none_or(|e| e.iter().all(|d| !d.message.starts_with(mention))),
                "{fixed}\n{after:#?}"
            );
        }
        // The implementation sees both.
        let both = sectioned(
            "type t { { a: integer } };\n\noperator f{}: t;",
            "operator g{}: t [\n    f{}\n];\n\noperator f{}: t [\n    t{a: 1}\n];",
        );
        assert!(analyze_str(&both).is_ok(), "{both}");
        // An operator's body stays where it is, its comment goes with its heading.
        let commented = sectioned(
            "type t { { a: integer, constraint is_small{a: a} } };",
            "/* Whether it is small. */\noperator is_small{a: integer}: boolean [\n    a < 5\n];",
        );
        let fixed = fix(&commented);
        assert!(analyze_str(&fixed).is_ok(), "{fixed}");
        let (interface, implementation) = fixed.split_once("implementation").unwrap();
        assert!(
            interface
                .contains("/* Whether it is small. */\noperator is_small{a: integer}: boolean;"),
            "{fixed}"
        );
        assert!(implementation.contains("a < 5"), "{fixed}");
        assert!(!implementation.contains("/* Whether"), "{fixed}");
        // Declared, it isn't Tutorial D's `IS_T`, whatever its name looks like.
        let named_like_a_test = sectioned(
            "type percentage { { n: integer, constraint is_percentage{n} } };",
            "operator is_percentage{n: integer}: boolean [\n    n >= 0 and n <= 100\n];",
        );
        assert_eq!(codes(&named_like_a_test), [Code::InterfaceDeclaration]);
        assert!(analyze_str(&fix(&named_like_a_test)).is_ok());
        // Mentioned twice, reported once, as the fix is one.
        let twice = sectioned(
            "type t { { a: integer, constraint a < low and a > -low } };",
            "const low := 3;",
        );
        assert_eq!(codes(&twice), [Code::InterfaceDeclaration]);
        // Of several overloads, the one meant isn't certain.
        let overloads = sectioned(
            "type t { { a: integer, constraint is_small{a: a} } };",
            "operator is_small{a: integer}: boolean [\n    a < 5\n];\n\noperator is_small{b: integer}: boolean [\n    b < 6\n];",
        );
        let found = errors(&overloads);
        assert!(found[0].suggestions.is_empty(), "{found:#?}");
        assert!(found[0].help[0].contains("by its heading"), "{found:#?}");
        // A variable stays in the implementation, if what reads it reads no
        // variable anyway.
        let variable = sectioned(
            "type t { { a: integer, constraint a < kept } };",
            "var kept := 3;",
        );
        let found = errors(&variable);
        assert_eq!(found[0].code, Code::InterfaceDeclaration, "{found:#?}");
        assert!(found[0].suggestions.is_empty(), "{found:#?}");
        // Otherwise it moves to the interface, but a constraint on it alone
        // moves to the implementation.
        for (interface, implementation, fixed) in [
            (
                "var total := kept;",
                "var kept := 3;",
                "var total := kept;\n\nvar kept := 3;",
            ),
            (
                "constraint low := kept < 5;",
                "var kept := 3;",
                "implementation\n\nvar kept := 3;\n\noperator main{} [];\n\nconstraint low := kept < 5;",
            ),
            (
                "var total := 0;\n\nconstraint low := kept > total;",
                "var kept := 3;",
                "var total := 0;\n\nconstraint low := kept > total;\n\nvar kept := 3;",
            ),
        ] {
            let source = sectioned(interface, implementation);
            assert_eq!(codes(&source), [Code::InterfaceDeclaration], "{source}");
            let fixed_source = fix(&source);
            assert!(analyze_str(&fixed_source).is_ok(), "{fixed_source}");
            assert!(fixed_source.contains(fixed), "{fixed_source}");
        }
        // What only the implementation declares hides nothing from the interface:
        // there, the name is the prelude's, and in the implementation, its own.
        let own = sectioned(
            "type t { { a: array integer, constraint length{a: a} > 0 } };",
            "operator length{of: integer}: integer [\n    of\n];\n\nvar one := length{of: 1};",
        );
        let program = analyze_str(&own);
        assert!(program.is_ok(), "{own}\n{program:#?}");
        // Another unit sees the interface, not the implementation.
        let unit = sectioned(
            "operator f{}: integer;",
            "operator g{}: integer [\n    1\n];\n\noperator f{}: integer [\n    g{}\n];",
        );
        let found = analyze_units(&[
            (
                "program",
                "uses { lib };\n\nvar a := g{};\n\noperator main{} [];",
            ),
            ("lib", &unit),
        ])
        .unwrap_err();
        assert_eq!(found[0].code, Code::UnknownName, "{found:#?}");
    }

    #[test]
    fn days_in_month_is_the_rule_a_date_is_checked_by() {
        for (year, month, days) in [
            (2024, 2, 29),
            (2023, 2, 28),
            (2000, 2, 29),
            (1900, 2, 28),
            (2024, 1, 31),
            (2024, 4, 30),
            (2024, 12, 31),
        ] {
            assert_eq!(
                time_value(&format!("days_in_month{{year: {year}, month: {month}}}")),
                days.to_string()
            );
        }
        let source = format!(
            "uses {{ trendy.time }};\nvar a := days_in_month{{year: 2024, month: 13}};{MAIN}"
        );
        assert_eq!(codes(&source), [Code::BrokenCallConstraint]);
    }

    #[test]
    fn an_attribute_compared_with_itself_is_reported() {
        let cities = "var cities := relation { { n: \"Lagos\", q: 1 } };\n";
        let main = "operator main{} [];";
        // Whichever comparison, always true or always false, wherever it is.
        for (condition, always) in [
            ("n = n", "true"),
            ("n <= n", "true"),
            ("n >= n", "true"),
            ("n <> n", "false"),
            ("n < n", "false"),
            ("n > n", "false"),
            ("(n) = n", "true"),
            ("q = 1 and n = n", "true"),
            ("n = n or q = 1", "true"),
            ("not (n = n)", "true"),
            // What the compiler can tell is the attribute, written another way.
            ("n = n || \"\"", "true"),
            ("q = q + 0", "true"),
        ] {
            let source = format!("{cities}var a := cities where {condition};\n{main}");
            let found = errors(&source);
            assert_eq!(found.len(), 1, "{condition}: {found:#?}");
            assert_eq!(found[0].code, Code::ComparedWithItself, "{condition}");
            assert_eq!(
                found[0].primary.message,
                format!("always {always}, of every tuple"),
                "{condition}"
            );
            assert!(found[0].help[0].contains("another attribute"), "{found:#?}");
        }
        let source = format!("{cities}var a := cities extend {{ same := q = q }};\n{main}");
        assert_eq!(codes(&source), [Code::ComparedWithItself]);
        // In `update` and `delete`, and a nested attribute.
        let body = |statement: &str| format!("{cities}operator main{{}} [ {statement} ];");
        assert_eq!(
            codes(&body("delete cities where q = q;")),
            [Code::ComparedWithItself]
        );
        assert_eq!(
            codes(&body("update cities { q := 1 } where q = q;")),
            [Code::ComparedWithItself]
        );
        let nested = "var r := relation { { p: tuple { x: 1 } } };\nvar a := r where p.x = p.x;\n";
        assert_eq!(
            codes(&format!("{nested}{main}")),
            [Code::ComparedWithItself]
        );
        // Of a component.
        let component = "type c { { re: integer } };\nvar r := relation { { p: c{re: 1} } };\nvar a := r where p.re = p.re;\n";
        let found = errors(&format!("{component}{main}"));
        assert_eq!(found[0].code, Code::ComparedWithItself, "{found:#?}");
        assert!(found[0].message.contains("the attribute `p`"), "{found:#?}");
        // Different attributes, and what isn't an attribute, are as they were.
        for fine in [
            format!("{cities}var a := cities where n = n || \"x\";\n{main}"),
            format!("{cities}var a := cities where q = 1;\n{main}"),
            format!("var r := relation {{ {{ a: 1, b: 1 }} }};\nvar s := r where a = b;\n{main}"),
            format!(
                "var r := relation {{ {{ a: 1, b: 2 }} }};\nvar x := 1;\nvar same := x = x;\n{main}"
            ),
            format!("operator f{{t: tuple {{ a: integer }}}}: boolean [ (t.a = t.a) ];\n{main}"),
            format!(
                "{cities}operator f{{m: text}}: relation {{ n: text, q: integer }} [ relation {{ {{ n: m, q: 1 }} }} where n = m ];\n{main}"
            ),
        ] {
            let program = analyze_str(&fine);
            assert!(program.is_ok(), "{fine}\n{program:#?}");
        }
    }

    #[test]
    fn an_attribute_compared_with_itself_says_what_it_hides() {
        let main = "operator main{} [];";
        // What it says it hides, and where it says that is declared: the text
        // just before that, if the program says.
        let says = |source: &str, what: &str, declared_after: Option<&str>| {
            let found = errors(source);
            assert_eq!(found[0].code, Code::ComparedWithItself, "{found:#?}");
            let help = &found[0].help[0];
            assert!(help.contains(what), "{what}: {help}");
            let starts: Vec<_> = found[0].secondary.iter().map(|l| l.span.start).collect();
            let expected: Vec<_> = declared_after
                .iter()
                .map(|before| source.find(before).unwrap() + before.len())
                .collect();
            assert_eq!(starts, expected, "{what}: {source}");
        };
        let relation = "relation { n: text, q: integer }";
        let parameter = "the parameter `n`";
        says(
            &format!("operator f{{r: {relation}, n: text}}: {relation} [ r where n = n ];\n{main}"),
            parameter,
            Some(&format!("{relation}, ")),
        );
        says(
            &format!(
                "operator f{{r: {relation}, n: text}} [ call debug_table{{r: r where n = n}}; ];\n{main}"
            ),
            parameter,
            Some(&format!("{relation}, ")),
        );
        let local = "the local variable `n`";
        says(
            &format!(
                "operator f{{r: {relation}}} [ var n := \"a\"; call debug_table{{r: r where n = n}}; ];\n{main}"
            ),
            local,
            Some("[ var "),
        );
        says(
            &format!(
                "operator f{{r: {relation}}}: {relation} [ var n := \"a\"; r where n = n ];\n{main}"
            ),
            local,
            Some("[ var "),
        );
        says(
            &format!(
                "operator f{{r: {relation}, a: array text}} [ for n in a loop [ call debug_table{{r: r where n = n}}; ]; ];\n{main}"
            ),
            local,
            Some("[ for "),
        );
        // In a block inside the one that declares it.
        says(
            &format!(
                "operator f{{r: {relation}, b: boolean}} [ var n := \"a\"; if b then [ call debug_table{{r: r where n = n}}; ]; ];\n{main}"
            ),
            local,
            Some("[ var "),
        );
        let cities = "var cities := relation { { n: \"Lagos\", q: 1 } };\n";
        says(
            &format!("const n := \"a\";\n{cities}var a := cities where n = n;\n{main}"),
            "the constant `n`",
            Some("const "),
        );
        says(
            &format!(
                "var n := \"a\";\n{cities}operator main{{}} [ call debug_table{{r: cities where n = n}}; ];"
            ),
            "the variable `n`",
            Some("var "),
        );
        // What the tuple further out hides, of a name the one at hand doesn't have.
        says(
            &format!(
                "operator f{{r: {relation}, s: relation {{ m: text }}, n: text}}: relation {{ n: text, q: integer, c: integer }} [ r extend {{ c := count{{r: s where n = n}} }} ];\n{main}"
            ),
            parameter,
            Some("{ m: text }, "),
        );
        // An attribute further out, of a tuple around this one, which isn't declared
        // in any one place.
        says(
            &format!(
                "var r := relation {{ {{ n: 1 }} }};\nvar s := relation {{ {{ n: 1 }} }};\nvar a := r extend {{ c := count{{r: s where n = n}} }};\n{main}"
            ),
            "the attribute further out `n`",
            None,
        );
    }

    /// That each program's only error has a fix that gives the other, which has none.
    fn fixes_to(cases: &[(String, String)]) {
        for (source, expected) in cases {
            assert_eq!(fix(source), *expected);
            let program = analyze_str(expected);
            assert!(program.is_ok(), "{expected}\n{program:#?}");
        }
    }

    #[test]
    fn an_attribute_compared_with_itself_is_fixed_by_a_constant_before_the_statement() {
        let main = "operator main{} [];";
        let r = "relation { n: text, q: integer }";
        fixes_to(&[
            // On the line it is written on, or on a line of its own.
            (
                format!("operator f{{r: {r}, n: text}}: {r} [ r where n = n ];\n{main}"),
                format!(
                    "operator f{{r: {r}, n: text}}: {r} [ const n_ := n; r where n = n_ ];\n{main}"
                ),
            ),
            (
                format!("operator f{{r: {r}, n: text}}: {r} [r where n <> n];\n{main}"),
                format!(
                    "operator f{{r: {r}, n: text}}: {r} [const n_ := n; r where n <> n_];\n{main}"
                ),
            ),
            (
                format!(
                    "operator f{{r: {r}, n: text}} [\n    call debug_table{{r: r where n = n}};\n];\n{main}"
                ),
                format!(
                    "operator f{{r: {r}, n: text}} [\n    const n_ := n;\n    call debug_table{{r: r where n = n_}};\n];\n{main}"
                ),
            ),
            // In the innermost block, with what a loop has changed by then.
            (
                format!(
                    "operator f{{r: {r}, n: text, b: boolean}} [\n    if b then [\n        call debug_table{{r: r where n = n}};\n    ];\n];\n{main}"
                ),
                format!(
                    "operator f{{r: {r}, n: text, b: boolean}} [\n    if b then [\n        const n_ := n;\n        call debug_table{{r: r where n = n_}};\n    ];\n];\n{main}"
                ),
            ),
            (
                format!(
                    "operator f{{r: {r}}} [\n    var n := \"a\";\n    while n <> \"aaa\" loop [\n        n := n || \"a\";\n        call debug_table{{r: r where n = n}};\n    ];\n];\n{main}"
                ),
                format!(
                    "operator f{{r: {r}}} [\n    var n := \"a\";\n    while n <> \"aaa\" loop [\n        n := n || \"a\";\n        const n_ := n;\n        call debug_table{{r: r where n = n_}};\n    ];\n];\n{main}"
                ),
            ),
            (
                format!(
                    "operator f{{r: {r}, a: array text}} [ for n in a loop [ call debug_table{{r: r where n = n}}; ]; ];\n{main}"
                ),
                format!(
                    "operator f{{r: {r}, a: array text}} [ for n in a loop [ const n_ := n; call debug_table{{r: r where n = n_}}; ]; ];\n{main}"
                ),
            ),
            // Before the statement, of what is inside no tuple's scope: a block in a
            // `where` would read the attribute, and an `assign` has only assignments.
            (
                format!(
                    "operator f{{r: {r}, n: text}}: {r} [ r where [ var a := 1; n = n and q = a ] ];\n{main}"
                ),
                format!(
                    "operator f{{r: {r}, n: text}}: {r} [ const n_ := n; r where [ var a := 1; n = n_ and q = a ] ];\n{main}"
                ),
            ),
            (
                format!(
                    "operator f{{r: {r}, n: text}} [\n    var x := r;\n    var y := 0;\n    assign {{ x := r where n = n; y := 1; }};\n];\n{main}"
                ),
                format!(
                    "operator f{{r: {r}, n: text}} [\n    var x := r;\n    var y := 0;\n    const n_ := n;\n    assign {{ x := r where n = n_; y := 1; }};\n];\n{main}"
                ),
            ),
        ]);
    }

    #[test]
    fn an_attribute_compared_with_itself_is_fixed_by_a_constant_reading_what_it_hides() {
        let main = "operator main{} [];";
        let r = "relation { n: text, q: integer }";
        let cities = "var cities := relation { { n: \"Lagos\", q: 1 } };\n";
        fixes_to(&[
            // A local variable of a value block, in that block, wherever that is.
            (
                format!(
                    "operator f{{r: {r}}}: integer [ var c := [ var n := \"a\"; count{{r: r where n = n}} ]; c ];\n{main}"
                ),
                format!(
                    "operator f{{r: {r}}}: integer [ var c := [ var n := \"a\"; const n_ := n; count{{r: r where n = n_}} ]; c ];\n{main}"
                ),
            ),
            // The program's: in an operator, or among its definitions, for a constant.
            (
                format!(
                    "const n := \"a\";\n{cities}operator main{{}} [\n    call debug_table{{r: cities where n = n}};\n];"
                ),
                format!(
                    "const n := \"a\";\n{cities}operator main{{}} [\n    const n_ := n;\n    call debug_table{{r: cities where n = n_}};\n];"
                ),
            ),
            (
                format!("const n := \"a\";\n{cities}var a := cities where n = n;\n{main}"),
                format!(
                    "const n := \"a\";\n{cities}const n_ := n;\nvar a := cities where n = n_;\n{main}"
                ),
            ),
            (
                format!(
                    "var n := \"a\";\n{cities}operator main{{}} [\n    call debug_table{{r: cities where n = n}};\n];"
                ),
                format!(
                    "var n := \"a\";\n{cities}operator main{{}} [\n    const n_ := n;\n    call debug_table{{r: cities where n = n_}};\n];"
                ),
            ),
            // What it is written as: only the name changes.
            (
                format!("operator f{{r: {r}, n: text}}: {r} [ r where n = (n) ];\n{main}"),
                format!(
                    "operator f{{r: {r}, n: text}}: {r} [ const n_ := n; r where n = (n_) ];\n{main}"
                ),
            ),
            (
                format!(
                    "operator f{{r: relation {{ p: tuple {{ x: integer }} }}, p: tuple {{ x: integer }}}}: relation {{ p: tuple {{ x: integer }} }} [ r where p.x = p.x ];\n{main}"
                ),
                format!(
                    "operator f{{r: relation {{ p: tuple {{ x: integer }} }}, p: tuple {{ x: integer }}}}: relation {{ p: tuple {{ x: integer }} }} [ const p_ := p; r where p.x = p_.x ];\n{main}"
                ),
            ),
            // By a name nothing in the program has.
            (
                format!("operator f{{r: {r}, n: text, n_: text}}: {r} [ r where n = n ];\n{main}"),
                format!(
                    "operator f{{r: {r}, n: text, n_: text}}: {r} [ const n_2 := n; r where n = n_2 ];\n{main}"
                ),
            ),
        ]);
        // Not even a name of the program's, which it would hide from what reads that.
        let source = format!(
            "const n_ := \"x\";\noperator f{{r: {r}, n: text}}: {r} [ r where n = n and n_ = \"x\" ];\n{main}"
        );
        assert!(fix(&source).contains("const n_2 := n;"), "{}", fix(&source));
        // Nor that the tuple has, where its heading is written in another unit.
        let found = analyze_units(&[
            ("program", "uses { lib };\noperator f{r: relation row, n: text}: relation row [ r where n = n ];\noperator main{} [];"),
            ("lib", "interface\nheading row := { n: text, n_: text };"),
        ])
        .unwrap_err();
        assert_eq!(found[0].code, Code::ComparedWithItself, "{found:#?}");
        assert!(
            found[0].suggestions[0]
                .message
                .contains("`const n_2 := n;`"),
            "{found:#?}"
        );
    }

    #[test]
    fn an_attribute_compared_with_itself_has_help_where_a_fix_would_be_a_guess() {
        let main = "operator main{} [];";
        let r = "relation { n: text, q: integer }";
        let cities = "var cities := relation { { n: \"Lagos\", q: 1 } };\n";
        let help = |source: &str| {
            let found = errors(source);
            assert_eq!(
                found[0].code,
                Code::ComparedWithItself,
                "{source}: {found:#?}"
            );
            assert!(found[0].suggestions.is_empty(), "{source}: {found:#?}");
            found[0].help[0].clone()
        };
        // Which side reads it, of an order, or of what the compiler made one side from.
        for condition in ["q < q", "q <= q", "q > q", "q >= q"] {
            let source =
                format!("operator f{{r: {r}, q: integer}}: {r} [ r where {condition} ];\n{main}");
            assert!(
                help(&source).contains("`const q_ := q;`"),
                "{}",
                help(&source)
            );
        }
        for (condition, parameter) in [("n = n || \"\"", "n: text"), ("q = q + 0", "q: integer")] {
            let source =
                format!("operator f{{r: {r}, {parameter}}}: {r} [ r where {condition} ];\n{main}");
            assert!(
                help(&source).contains("into a constant"),
                "{}",
                help(&source)
            );
        }
        // A loop's condition, computed again; what a constant can't read, before it
        // is declared or where it can't read a variable; and an attribute, which
        // no statement can read.
        for source in [
            format!(
                "operator f{{r: {r}, n: text}} [\n    while count{{r: r where n = n}} > 0 loop [ ];\n];\n{main}"
            ),
            format!(
                "var n := \"a\";\n{cities}operator main{{}} [\n    while count{{r: cities where n = n}} > 0 loop [ ];\n];"
            ),
            format!(
                "operator f{{r: {r}}} [\n    call debug_table{{r: r where n = n}};\n    var n := \"a\";\n];\n{main}"
            ),
            format!("var n := \"a\";\noperator f{{r: {r}}}: {r} [ r where n = n ];\n{main}"),
            format!("var n := \"a\";\n{cities}var a := cities where n = n;\n{main}"),
            format!(
                "var r := relation {{ {{ n: 1 }} }};\nvar s := relation {{ {{ n: 1 }} }};\nvar a := r extend {{ c := count{{r: s where n = n}} }};\n{main}"
            ),
        ] {
            assert!(
                help(&source).contains("rename it"),
                "{source}: {}",
                help(&source)
            );
        }
    }

    #[test]
    fn a_unit_has_main_or_an_interface_or_both() {
        let codes = |units: &[(&str, &str)]| -> Vec<Code> {
            checked_unit(units).iter().map(|e| e.code).collect()
        };
        let exports =
            "interface\n\noperator f{}: integer;\n\nimplementation\n\noperator f{}: integer [1];";
        assert_eq!(codes(&[("unit", exports)]), []);
        assert_eq!(codes(&[("unit", "operator main{} [];")]), []);
        let both = format!("{exports}\n\noperator main{{}} [];");
        assert_eq!(codes(&[("unit", &both)]), []);
        // Neither, which is no more of a use for its having sections, or for
        // what they use.
        for neither in [
            "",
            "operator f{} [];",
            "interface\n\nimplementation\n\noperator f{} [];",
            "interface\n\nuses { trendy.files };\n\nimplementation\n\noperator f{} [];",
        ] {
            assert_eq!(
                codes(&[("unit", neither)]),
                [Code::MissingMain],
                "{neither}"
            );
        }
        // `main` of any use is still a program's, whatever else it has.
        let wrong = format!("{exports}\n\noperator main{{a: integer}} [];");
        assert_eq!(codes(&[("unit", &wrong)]), [Code::MainSignature]);
        // A `Main` beside an interface is an operator like any other.
        let named = "interface\n\noperator Main{};\n\nimplementation\n\noperator Main{} [];";
        assert_eq!(codes(&[("unit", named)]), []);
    }

    #[test]
    fn a_used_unit_has_a_use_too() {
        let main = "uses { helper };\n\noperator main{} [];";
        let helper = "operator f{}: integer [1];";
        let errors = analyze_units(&[("program", main), ("helper", helper)]).unwrap_err();
        assert_eq!(errors.len(), 1, "{errors:#?}");
        assert_eq!(errors[0].code, Code::MissingMain);
        assert_eq!(
            errors[0].message,
            "the unit `helper` has neither `main` nor an interface, so nothing can run or use it"
        );
        // Where `interface` would go, in it, not at the end of the unit before.
        assert_eq!(errors[0].primary.span.start, main.len());
        assert_eq!(errors[0].primary.span.end, main.len() + "operator".len());
        assert_eq!(
            &main[errors[0].secondary[0].span.start..errors[0].secondary[0].span.end],
            "helper"
        );
        // What it should export isn't certain, so there is no fix.
        assert!(errors[0].suggestions.is_empty(), "{errors:#?}");
        assert!(errors[0].help[0].contains("`interface`"), "{errors:#?}");
        // Used through another, the same.
        let used = "interface\n\nuses { helper };\n\nheading h := { a: integer };";
        let errors = analyze_units(&[
            ("program", "uses { used };\n\noperator main{} [];"),
            ("used", used),
            ("helper", helper),
        ])
        .unwrap_err();
        assert_eq!(errors.len(), 1, "{errors:#?}");
        assert_eq!(errors[0].code, Code::MissingMain);
        // One with `main` can be run on its own, so it is of use.
        let tool = "operator main{} [];";
        assert!(analyze_units(&[("program", main), ("helper", tool)]).is_ok());
        // But a program starts in its own `main`, not one in a unit it uses.
        let errors = analyze_units(&[
            ("program", "uses { helper };\n\noperator f{} [];"),
            ("helper", tool),
        ])
        .unwrap_err();
        assert_eq!(errors[0].code, Code::MissingMain, "{errors:#?}");
    }

    #[test]
    fn a_program_needs_main_and_a_unit_says_what_it_is_for() {
        // With an interface, adding an empty `main` wouldn't help.
        let library = "interface\n\noperator f{};\n\nimplementation\n\noperator f{} [];";
        let errors = errors(library);
        assert_eq!(errors.len(), 1, "{errors:#?}");
        assert_eq!(errors[0].code, Code::MissingMain);
        assert_eq!(
            errors[0].message,
            "this unit has an interface but no `main` operator, so it can't be run"
        );
        assert_eq!(errors[0].primary.span, Span::new(0, "interface".len()));
        assert!(errors[0].suggestions.is_empty(), "{errors:#?}");
        assert!(
            errors[0].help[0].contains("`trendyc --check`"),
            "{errors:#?}"
        );
        // But a near miss is still fixed.
        let named = "interface\n\noperator Main{};\n\nimplementation\n\noperator Main{} [];";
        assert!(fix(named).contains("operator main{};"), "{}", fix(named));
        // Neither gets an empty `main`, and is told of the other way.
        let errors = self::errors("operator f{} [];");
        assert_eq!(errors[0].message, "program has no `main` operator");
        assert!(errors[0].help[0].contains("`interface`"), "{errors:#?}");
        assert!(
            fix("operator f{} [];").ends_with("operator main{} [];\n"),
            "{}",
            fix("operator f{} [];")
        );
    }

    #[test]
    fn what_only_adds_to_a_database_is_fixed_to_its_interface() {
        let library = "interface\n\ndatabase library;\n\nvar library.books: relation { isbn: text } key { isbn };";
        let part = "/* What is on loan. */\nuses { library };\n\nvar library.loans: relation { isbn: text } key { isbn };\n\nconstraint library.lent := loans{isbn} <= books{isbn};";
        let main = "uses { library };\n\noperator main{} [];";
        let errors = analyze_units(&[
            ("program", main),
            ("library", library),
            ("library.loans", part),
        ])
        .unwrap_err();
        assert_eq!(errors.len(), 1, "{errors:#?}");
        assert_eq!(errors[0].code, Code::MissingMain);
        let start = main.len() + library.len();
        let edit = &errors[0].suggestions[0].edits[0];
        assert_eq!(
            edit.span,
            Span::empty(start + "/* What is on loan. */\n".len())
        );
        let fixed = format!(
            "/* What is on loan. */\n{}{}",
            edit.replacement,
            &part["/* What is on loan. */\n".len()..]
        );
        assert!(
            analyze_units(&[
                ("program", main),
                ("library", library),
                ("library.loans", &fixed),
            ])
            .is_ok(),
            "{fixed}"
        );
        // Checked alone, or built, it gets the same fix rather than an empty `main`.
        let errors = checked_unit(&[("loans", part), ("library", library)]);
        let inserted = errors
            .iter()
            .filter(|e| e.code == Code::MissingMain)
            .flat_map(|e| &e.suggestions)
            .flat_map(|s| &s.edits)
            .map(|e| e.replacement.as_str())
            .collect::<Vec<_>>();
        assert_eq!(inserted, ["interface\n\n"]);
        // Anything that isn't shared in any case is for its author to sort.
        let found = analyze_units(&[
            ("program", "uses { helper };\n\noperator main{} [];"),
            (
                "helper",
                "heading h := { a: integer };\nconstraint c := 1 = 1;",
            ),
        ])
        .unwrap_err();
        let of_no_use = found.iter().find(|e| e.code == Code::MissingMain).unwrap();
        assert!(of_no_use.suggestions.is_empty(), "{found:#?}");
        let errors = analyze_units(&[
            ("program", "uses { helper };\n\noperator main{} [];"),
            ("helper", "heading h := { a: integer };\noperator f{} [];"),
        ])
        .unwrap_err();
        assert!(errors[0].suggestions.is_empty(), "{errors:#?}");
    }

    #[test]
    fn a_unit_uses_units_of_the_program() {
        // The catalog's `known_units`, broken by a unit there isn't, at each
        // `uses` of it, with the file it would be.
        let errors = analyze_units(&[
            ("program", "uses { lakes.depth };\n\noperator main{} [];"),
            (
                "lakes.depths",
                "interface\n\nuses { recipes };\n\nheading depth := { m: integer };",
            ),
        ])
        .unwrap_err();
        let messages: Vec<&str> = errors.iter().map(|e| e.message.as_str()).collect();
        assert_eq!(
            messages,
            ["unknown unit `lakes.depth`", "unknown unit `recipes`"]
        );
        assert!(errors.iter().all(|e| e.code == Code::UnknownName));
        // A misspelling of one is fixed to it.
        assert_eq!(
            errors[0].suggestions[0].edits[0].replacement,
            "lakes.depths"
        );
        assert!(
            errors[1].notes[0].contains("`recipes.trendy`"),
            "{:#?}",
            errors[1]
        );
        // Those that come with the compiler are the program's.
        assert!(
            analyze_units(&[("program", "uses { trendy.catalog };\n\noperator main{} [];")])
                .is_ok()
        );
    }

    #[test]
    fn the_catalog_is_as_the_compiler_fills_and_reports_it() {
        let with_catalog = |catalog: &str| {
            let (program, catalog) = (parse("operator main{} [];"), parse(catalog));
            let unit = File {
                name: "program",
                path: "program.trendy",
                tree: &program,
            };
            with_library(Some((trendy_prelude::CATALOG, &catalog)), |library| {
                analyze(&[unit], library).unwrap_err()
            })
        };
        let catalog = trendy_prelude::CATALOG.source;
        // A constraint the compiler doesn't report the breaking of.
        let errors = with_catalog(&format!(
            "{catalog}\nconstraint catalog.few := count{{r: units}} < 3;"
        ));
        assert_eq!(errors.len(), 1, "{errors:#?}");
        assert_eq!(errors[0].code, Code::Internal);
        // A relvar it doesn't fill, or one it does, unlike its own.
        let errors = with_catalog(&catalog.replace("var catalog.uses", "var catalog.used"));
        assert!(
            errors.iter().any(|e| e.code == Code::Internal),
            "{errors:#?}"
        );
    }

    #[test]
    fn only_a_databases_own_units_add_to_it() {
        let library = "interface\n\ndatabase library;\n\nvar library.books: relation { isbn: text, title: text } key { isbn };";
        let loans = "interface\n\nuses { library };\n\nvar library.loans: relation { isbn: text, member: integer } key { isbn };\n\nconstraint library.lent_books := loans{isbn} <= books{isbn};";
        let program = analyze_units(&[
            ("program", "uses { library };\n\noperator main{} [];"),
            ("library", library),
            ("library.loans", loans),
            (
                "library.loans.late",
                "interface\n\nuses { library };\n\nvar library.late: relation { isbn: text };",
            ),
        ]);
        assert!(program.is_ok(), "{program:#?}");
        // Elsewhere, each relvar and constraint is reported where it names
        // the database.
        let main = "uses { library };\n\nvar library.reviews: relation { isbn: text, stars: integer };\n\nconstraint library.reviewed := reviews{isbn} <= books{isbn};\n\noperator main{} [];";
        let errors = analyze_units(&[("program", main), ("library", library)]).unwrap_err();
        assert_eq!(errors.len(), 2, "{errors:#?}");
        assert!(
            errors.iter().all(|e| e.code == Code::DatabasePart),
            "{errors:#?}"
        );
        assert_eq!(
            errors[0].message,
            "only the database `library`'s own units add to it, not `program`"
        );
        let at = |e: &Diagnostic| &main[e.primary.span.start..e.primary.span.end];
        assert_eq!(
            errors.iter().map(at).collect::<Vec<_>>(),
            ["library", "library"]
        );
        assert!(errors[0].help[0].contains("`library/`"), "{errors:#?}");
        // A unit whose name only starts like the database's unit's is no
        // unit of its.
        let errors = analyze_units(&[
            ("program", "uses { library };\n\noperator main{} [];"),
            ("library", library),
            (
                "libraryx",
                "interface\n\nuses { library };\n\nvar library.x: relation { a: integer };",
            ),
        ])
        .unwrap_err();
        assert_eq!(
            errors.iter().map(|e| e.code).collect::<Vec<_>>(),
            [Code::DatabasePart]
        );
    }

    #[test]
    fn no_unit_uses_itself() {
        // The catalog's `acyclic`, broken by the units on a cycle, reported
        // once for each cycle, at the first unit's `uses`.
        let errors = analyze_units(&[
            ("program", "uses { a };\n\noperator main{} [];"),
            (
                "a",
                "interface\n\nuses { b };\n\nheading a := { x: integer };",
            ),
            (
                "b",
                "interface\n\nheading b := { x: integer };\n\nimplementation\n\nuses { a };",
            ),
        ])
        .unwrap_err();
        assert_eq!(errors.len(), 1, "{errors:#?}");
        assert_eq!(errors[0].code, Code::UnitCycle);
        assert_eq!(errors[0].message, "the units `a` and `b` use each other");
        assert_eq!(errors[0].secondary.len(), 1);
        assert!(errors[0].suggestions.is_empty());
        let errors = analyze_units(&[
            ("program", "uses { a };\n\noperator main{} [];"),
            (
                "a",
                "interface\n\nuses { b };\n\nheading a := { x: integer };",
            ),
            (
                "b",
                "interface\n\nuses { c };\n\nheading b := { x: integer };",
            ),
            (
                "c",
                "interface\n\nuses { a };\n\nheading c := { x: integer };",
            ),
        ])
        .unwrap_err();
        assert_eq!(
            errors
                .iter()
                .map(|e| e.message.as_str())
                .collect::<Vec<_>>(),
            ["the units `a`, `b` and `c` use each other in a cycle"]
        );
        // A unit of its own: the fix removes it from its `uses`.
        let source = "uses { program, a };\n\noperator main{} [];";
        let errors = analyze_units(&[("program", source), ("a", EXPORTS)]).unwrap_err();
        assert_eq!(errors.len(), 1, "{errors:#?}");
        assert_eq!(errors[0].message, "the unit `program` uses itself");
        let edit = &errors[0].suggestions[0].edits[0];
        let mut fixed = source.to_string();
        fixed.replace_range(edit.span.start..edit.span.end, &edit.replacement);
        assert_eq!(fixed, "uses { a };\n\noperator main{} [];");
    }

    #[test]
    fn units_see_their_own_names_and_the_preludes() {
        // Another unit's names are unknown, and not suggested.
        let helper = (
            "helper",
            "interface\n\nheading h := { a: integer };\n\nimplementation\n\noperator greet{} [];\nvar depth := 1;",
        );
        let errors = analyze_units(&[("program", "operator main{} [ call greet{}; ];"), helper])
            .unwrap_err();
        assert_eq!(
            errors.iter().map(|e| e.code).collect::<Vec<_>>(),
            [Code::UnknownName]
        );
        let errors =
            analyze_units(&[("program", "operator main{} [ call gret{}; ];"), helper]).unwrap_err();
        assert!(!format!("{errors:?}").contains("greet"), "{errors:#?}");
        let errors = analyze_units(&[
            (
                "program",
                "operator main{} [ call debug_value{v: depth}; ];",
            ),
            helper,
        ])
        .unwrap_err();
        assert_eq!(
            errors.iter().map(|e| e.code).collect::<Vec<_>>(),
            [Code::UnknownName]
        );
        // The prelude's are every unit's.
        let program = analyze_units(&[
            (
                "program",
                "operator main{} [ call debug_value{v: sqrt{of: 4e0}}; ];",
            ),
            (
                "helper",
                &format!("{EXPORTS}operator f{{}} [ call debug_value{{v: sqrt{{of: 9e0}}}}; ];"),
            ),
        ]);
        assert!(program.is_ok(), "{program:?}");
    }

    #[test]
    fn names_in_several_units_are_qualified() {
        // Each unit's own, though another declares the same names: no
        // duplicates, and each qualified by its unit in the program.
        let main = format!("{LAKES}\noperator main{{}} [ call bump{{}}; ];");
        let lakes = format!("{EXPORTS}{LAKES}");
        let program = analyze_units(&[("program", &main), ("lakes", &lakes)]).unwrap();
        let variables: Vec<&str> = own_variables(&program).map(|v| v.name.as_str()).collect();
        assert_eq!(
            variables,
            [
                "program::count",
                "program::depth",
                "lakes::count",
                "lakes::depth"
            ]
        );
        let constraints: Vec<&str> = program
            .constraints
            .iter()
            .map(|c| c.name.as_str())
            .collect();
        assert_eq!(constraints, ["program::positive", "lakes::positive"]);
        let named = |name: &str| {
            program
                .operators
                .iter()
                .filter(|o| o.name.ends_with(&format!("::{name}")) || o.name == name)
                .map(|o| o.name.as_str())
                .collect::<Vec<_>>()
        };
        assert_eq!(named("twice"), ["program::twice", "lakes::twice"]);
        assert_eq!(named("bump"), ["program::bump", "lakes::bump"]);
        // A selector is named as its type is.
        assert_eq!(named("metres"), ["program::metres", "lakes::metres"]);
        assert_eq!(program.variables[1].ty.to_string(), "program::metres");
        assert_eq!(program.variables[3].ty.to_string(), "lakes::metres");
        // A name in one unit is plain.
        assert_eq!(program.operator(program.entry).name, "main");
    }

    #[test]
    fn overloads_and_the_preludes_names_stay_plain() {
        // Operators of one name whose parameters' names differ are no
        // clash, in one unit or several.
        let program = analyze_units(&[
            (
                "program",
                "operator f{x: integer} [];\noperator main{} [ call f{x: 1}; ];",
            ),
            (
                "helper",
                "interface\n\nheading h := { a: integer };\n\nimplementation\n\noperator f{y: integer} [];\noperator g{} [ call f{y: 2}; ];",
            ),
        ])
        .unwrap();
        let fs: Vec<&str> = program
            .operators
            .iter()
            .filter(|o| o.name == "f" || o.name.ends_with("::f"))
            .map(|o| o.name.as_str())
            .collect();
        assert_eq!(fs, ["f", "f"]);
        // A unit's version of the prelude's specification is in its family,
        // under its name.
        let failure = "type no_lake is error { { lake: text } };\noperator message{error: no_lake}: text [ \"no lake \" || error.lake ];";
        let program = analyze_units(&[
            ("program", &format!("{failure}\noperator main{{}} [];")),
            ("lakes", &format!("{EXPORTS}{failure}")),
        ])
        .unwrap();
        let message = program.operator(program.message);
        let versions: Vec<String> = message
            .versions
            .iter()
            .flat_map(|v| v.params.values().map(ToString::to_string))
            .filter(|ty| ty.ends_with("no_lake"))
            .collect();
        assert_eq!(versions, ["program::no_lake", "lakes::no_lake"]);
        assert!(
            message
                .versions
                .iter()
                .all(|v| program.operator(v.operator).name == "message"),
        );
    }

    #[test]
    fn each_name_refers_to_its_declaration() {
        let depths = "interface\n\noperator below{m: integer}: integer;\n\nimplementation\n\noperator below{m: integer}: integer [ m - 1 ];\n";
        let main = "uses { depths };

heading lake := { name: text, metres: integer };
type label { { text: text } };
var lakes: relation lake := relation {};
const deepest := 1600;

operator described{l: tuple lake}: label [
    label{text: l.name}
];

operator main{} [
    var listed := count{r: lakes};
    var more := listed + deepest + below{m: listed};
    call debug_value{v: more};
    call debug_value{v: described{l: tuple { name: \"Baikal\", metres: 1642 }}};
    call debug_value{v: count{r: lakes where name = \"Baikal\"}};
];
";
        let found = definitions_in(&[("main", main), ("depths", depths)]);
        let in_main: Vec<(&str, &str)> = found
            .iter()
            .map(|(name, declared)| (name.as_str(), declared.as_str()))
            .collect();
        for expected in [
            // A unit, by its file.
            ("depths", "interface"),
            // Types and headings, the prelude's too.
            ("text", "text"),
            ("integer", "integer"),
            ("lake", "lake"),
            ("label", "label"),
            // Operators, the prelude's and another unit's, and a selector.
            ("count", "count"),
            ("below", "below"),
            ("debug_value", "debug_value"),
            // Values: a parameter, a variable, a constant and local variables.
            ("l", "l"),
            ("lakes", "lakes"),
            ("deepest", "deepest"),
            ("listed", "listed"),
            ("more", "more"),
        ] {
            assert!(in_main.contains(&expected), "{expected:?} in {in_main:#?}");
        }
        // An attribute of the tuple at hand has no declaration to go to.
        assert!(
            !in_main.iter().any(|(name, _)| *name == "name"),
            "{in_main:#?}"
        );
    }
}
