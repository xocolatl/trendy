//! A program's databases (TTM RM Prescription 16): each a variable that
//! outlives the program, whose value is its relvars together (Appendix D's
//! dbvar, a tuplevar), which its units add, each relvar and constraint
//! named after it. A database's constraint (RM Prescription 23) is a
//! condition on its relvars alone, which are in scope by name in it, as an
//! operator's parameters are in its body: a boolean function of them.

use std::collections::{BTreeMap, BTreeSet};

use trendy_diag::{Diagnostic, Span};
use trendy_hir as hir;
use trendy_syntax::ast::{self, AstNode, VarDecl};

use crate::body::Context;
use crate::check::{Checker, Lookup, Names};
use crate::constraints::{self, PartNames};
use crate::locals::Locals;
use crate::operators::Operators;
use crate::types::{self, Expected, Origin};
use crate::{Files, Kind, Namespace, Namespaces, errors, keys, ops, sections};

/// Every database's relvars and constraints, checked.
pub(crate) struct Checked {
    /// Each database's relvars, by its identity, each by its name, with
    /// its type, unless its declaration has errors.
    pub(crate) relvars: BTreeMap<String, BTreeMap<String, Option<hir::Type>>>,
    /// Each valid constraint.
    pub(crate) constraints: Vec<DatabaseConstraint>,
}

/// A database's constraint, checked: a condition on the database's
/// relvars, which are its parameters, in parts (see
/// [`constraints::parts`]).
pub(crate) struct DatabaseConstraint {
    /// The database's identity.
    pub(crate) database: String,
    /// Its name in the database.
    pub(crate) name: String,
    pub(crate) parts: Vec<hir::ConstraintPart>,
}

/// Check every database's relvars and constraints in `files`, reporting
/// what is wrong with them.
pub(crate) fn check(files: &Files, cx: &Context, errors: &mut Vec<Diagnostic>) -> Checked {
    // Each database's relvars, by name, the first declaration of each, by
    // the database's identity.
    let mut relvars: BTreeMap<String, BTreeMap<String, Relvar>> = BTreeMap::new();
    for decl in files.relvars() {
        let Some((database, name, relvar)) = self::relvar(&decl, cx, errors) else {
            continue;
        };
        let of = relvars.entry(database.identity).or_default();
        match of.get(&name) {
            Some(first) => errors.push(errors::duplicate_name(
                &name,
                (Kind::Relvar, relvar.span),
                (Kind::Relvar, first.span),
            )),
            None => {
                of.insert(name, relvar);
            }
        }
    }
    // Each database's constraints, by name, where each is first declared,
    // by the database's identity.
    let mut declared: BTreeMap<String, BTreeMap<String, Span>> = BTreeMap::new();
    let constraints = files
        .constraints()
        .filter(|d| d.part_of().is_some())
        .filter_map(|decl| constraint(&decl, &relvars, &mut declared, cx, errors))
        .collect();
    Checked {
        relvars: relvars
            .into_iter()
            .map(|(database, of)| {
                let types = of
                    .into_iter()
                    .map(|(name, relvar)| (name, relvar.ty))
                    .collect();
                (database, types)
            })
            .collect(),
        constraints,
    }
}

/// A database a declaration adds to.
struct Database {
    /// Its name, as the declaration writes it.
    name: String,
    /// What it is called in the program.
    identity: String,
}

/// A database's relvar, as declared.
struct Relvar {
    /// Where its name is.
    span: Span,
    /// Its type, a relation type, unless its declaration has errors, which
    /// are reported.
    ty: Option<hir::Type>,
}

/// The database the relvar `decl` adds to it, its name, and the relvar, if
/// the database is one there is.
fn relvar(
    decl: &VarDecl,
    cx: &Context,
    errors: &mut Vec<Diagnostic>,
) -> Option<(Database, String, Relvar)> {
    let database = known_database(&decl.part_of()?, cx, errors)?;
    let ident = decl.name()?.ident()?;
    let ty = relvar_type(decl, &database.name, ident.text(), cx, errors);
    let relvar = Relvar {
        span: trendy_syntax::span(ident.text_range()),
        ty,
    };
    Some((database, ident.text().to_string(), relvar))
}

/// The type of the relvar `decl`, named `name`, of `database`: written,
/// a relation type, with its keys, and without an initial value, as its
/// value is the database's. `None` after reporting why it has none.
fn relvar_type(
    decl: &VarDecl,
    database: &str,
    name: &str,
    cx: &Context,
    errors: &mut Vec<Diagnostic>,
) -> Option<hir::Type> {
    let initialized = decl.initializer().is_some();
    if initialized {
        errors.push(errors::relvar_initialized(decl, database, name));
    }
    let Some(written) = decl.type_expr() else {
        let span = decl.name().map_or_else(|| decl.span(), |n| n.span());
        errors.push(errors::untyped_relvar(database, name, span));
        return None;
    };
    let ty = types::resolve(
        &written,
        &mut types::TypeParameters::None,
        cx.namespaces,
        errors,
    )?;
    if !matches!(ty, hir::Type::Relation(_)) {
        errors.push(errors::relvar_not_relation(database, name, &written, &ty));
        return None;
    }
    let empty = BTreeMap::new();
    let mut names = DatabaseNames::new(cx, database, &empty);
    keys::keys(
        decl,
        &format!("{database}.{name}"),
        &ty,
        None,
        &mut names,
        errors,
    )?;
    (!initialized).then_some(ty)
}

/// The database `part_of` names, if it is one there is, after reporting
/// that it isn't.
fn known_database(
    part_of: &ast::PartOf,
    cx: &Context,
    errors: &mut Vec<Diagnostic>,
) -> Option<Database> {
    let ident = part_of.database()?.ident()?;
    let name = ident.text();
    let span = trendy_syntax::span(ident.text_range());
    let Some(identity) = cx.namespaces.database_named(name, span) else {
        let candidates: Vec<&str> = cx.namespaces.database_names(span).collect();
        errors.push(
            errors::name_clash(name, span, Namespace::Value, cx.namespaces)
                .unwrap_or_else(|| errors::unknown_name(name, span, "database", &candidates)),
        );
        return None;
    };
    Some(Database {
        name: name.to_string(),
        identity,
    })
}

/// Check the constraint `decl` of a database, whose relvars are
/// `relvars`, by database: named once in it, among those `declared` in
/// it, a boolean, mentioning its relvars alone, one or more. The
/// constraint, unless it has errors, or is a duplicate, already reported,
/// whose errors of its own are.
fn constraint(
    decl: &ast::ConstraintDecl,
    relvars: &BTreeMap<String, BTreeMap<String, Relvar>>,
    declared: &mut BTreeMap<String, BTreeMap<String, Span>>,
    cx: &Context,
    errors: &mut Vec<Diagnostic>,
) -> Option<DatabaseConstraint> {
    let database = known_database(&decl.part_of()?, cx, errors)?;
    let ident = decl.name()?.ident()?;
    let span = trendy_syntax::span(ident.text_range());
    let names_of = declared.entry(database.identity.clone()).or_default();
    let first = if let Some(&first) = names_of.get(ident.text()) {
        errors.push(errors::duplicate_name(
            ident.text(),
            (Kind::Constraint, span),
            (Kind::Constraint, first),
        ));
        false
    } else {
        names_of.insert(ident.text().to_string(), span);
        true
    };
    let written = decl.condition()?;
    let empty = BTreeMap::new();
    let of = relvars.get(&database.identity).unwrap_or(&empty);
    let mut names = DatabaseNames::new(cx, &database.name, of);
    let boolean = Expected {
        ty: hir::Type::Scalar(hir::Scalar::Boolean),
        origin: Origin::Operator(decl.keyword()?),
    };
    let condition = Checker::new(&mut names).expr(&written, Some(&boolean), errors)?;
    if names.mentioned.is_empty() {
        // Known when compiling, as it mentions nothing that changes.
        let holds = ops::evaluated(condition, cx.bindings())
            .ok()
            .and_then(|computed| ops::known_value(&computed).cloned())
            == Some(hir::Value::Boolean(true));
        let file = ast::SourceFile::cast(decl.syntax().parent()?)?;
        let removal = sections::removal(&file, decl.syntax());
        errors.push(errors::variable_free_constraint(
            ident.text(),
            &written,
            holds,
            removal,
        ));
        return None;
    }
    first.then(|| DatabaseConstraint {
        database: database.identity,
        name: ident.text().to_string(),
        parts: constraints::parts(&written, &boolean, || {
            DatabaseNames::new(cx, &database.name, of)
        }),
    })
}

/// The names in a database's definition: its relvars, by name, each as a
/// parameter of the condition, and the local variables of the value blocks
/// in it. The program's variables aren't the database's, and the
/// database's own name isn't needed there.
struct DatabaseNames<'c, 'a> {
    cx: &'c Context<'a>,
    database: &'c str,
    relvars: &'c BTreeMap<String, Relvar>,
    locals: Locals,
    /// The relvars it mentions, by name.
    mentioned: BTreeSet<String>,
}

impl<'c, 'a> DatabaseNames<'c, 'a> {
    fn new(cx: &'c Context<'a>, database: &'c str, relvars: &'c BTreeMap<String, Relvar>) -> Self {
        DatabaseNames {
            cx,
            database,
            relvars,
            locals: Locals::default(),
            mentioned: BTreeSet::new(),
        }
    }
}

impl PartNames for DatabaseNames<'_, '_> {
    fn locals(self) -> Vec<hir::Local> {
        self.locals.finish()
    }
}

impl Names for DatabaseNames<'_, '_> {
    fn namespaces(&self) -> &Namespaces {
        self.cx.namespaces
    }

    fn site(&self, at: Span) -> hir::Site {
        self.cx.sites.at(at)
    }

    fn operators(&self) -> &Operators {
        self.cx.operators
    }

    fn resolve(&mut self, name: &str, span: Span, errors: &mut Vec<Diagnostic>) -> Lookup {
        if let Some(relvar) = self.relvars.get(name) {
            self.mentioned.insert(name.to_string());
            // A relvar with errors in its declaration is already reported.
            return match relvar.ty {
                Some(_) => Lookup::Found(hir::Expr::Parameter(name.to_string())),
                None => Lookup::Reported,
            };
        }
        if name == self.database {
            errors.push(errors::database_in_its_definition(name, span));
            return Lookup::Reported;
        }
        if self.cx.declared.find(name, span).is_some() {
            errors.push(errors::variable_in_database_constraint(
                name,
                span,
                self.database,
            ));
            return Lookup::Reported;
        }
        if self.cx.namespaces.operator(name, span).is_some() {
            errors.push(errors::operator_as_value(name, span, self.cx.operators));
            return Lookup::Reported;
        }
        Lookup::Unknown
    }

    fn candidates(&self, _: Span) -> Vec<String> {
        self.relvars.keys().cloned().collect()
    }

    fn leaf_type(&self, leaf: &hir::Expr) -> hir::Type {
        match leaf {
            hir::Expr::Parameter(name) => self.relvars[name]
                .ty
                .clone()
                .expect("only valid relvars are resolved"),
            hir::Expr::Local(id) => self.locals.get(*id).ty.clone(),
            _ => unreachable!("a database's condition's other names are constants, not {leaf:?}"),
        }
    }

    fn fresh_local(&mut self, local: hir::Local) -> hir::LocalId {
        self.locals.fresh(local)
    }

    fn hides(&self, _: &str) -> bool {
        false
    }

    fn checked(&mut self, id: hir::OperatorId, _: Span, _: &mut Vec<Diagnostic>) -> bool {
        self.cx.checked(id)
    }

    fn bindings(&self) -> ops::Bindings<'_> {
        self.cx.bindings()
    }

    fn narrowing_ended(&self, _: &str) -> Option<Span> {
        None
    }
}
