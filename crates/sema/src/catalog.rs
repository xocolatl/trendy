//! The catalog, `trendy.catalog`: the database that describes a program's
//! units, which the compiler fills as it checks them, as a database
//! describes itself (TTM RM Prescription 25), and whose constraints are the
//! rules between units (RM Prescription 23). Each constraint is a condition
//! on the catalog's relvars, computed of the values the program gives them;
//! the tuples that make one false are reported, each where the program
//! says what they say: a `uses` of a unit there isn't, or one through which
//! units use each other.

use std::collections::{BTreeMap, BTreeSet};

use trendy_diag::{Code, Diagnostic, Span};
use trendy_hir as hir;
use trendy_syntax::ast::{self, AstNode, Section, SourceFile};

use crate::body::Context;
use crate::databases::{self, DatabaseConstraint};
use crate::{Files, errors, ops, sections};

/// The relvars the compiler fills, each by its name, with its attributes,
/// each of type `text`, as the catalog declares them.
const RELVARS: [(&str, &[&str]); 5] = [
    // Each unit of the program, by its name.
    ("units", &["unit"]),
    // Each unit a unit's section uses, `interface` or `implementation`.
    ("uses", &["section", "unit", "used"]),
    // Each database, by its name in the program, and the unit declaring it.
    ("databases", &["database", "unit"]),
    // Each relvar or constraint a unit adds to a database.
    ("parts", &["database", "part", "unit"]),
    // Each unit, with itself and each unit whose directory it is under.
    ("within", &["owner", "unit"]),
];

/// A rule between units: a constraint of the catalog's, which sema reports
/// the breaking of.
#[derive(Clone, Copy)]
enum Rule {
    /// `known_units`: a unit uses units of the program. Its breaking tuples
    /// are the units used, `{used}`, that aren't.
    KnownUnits,
    /// `acyclic`: no unit uses itself, through any path. Its breaking
    /// tuples are the units on a cycle, `{unit, used}`, each used by itself.
    Acyclic,
    /// `own_parts`: only a database's own units add to it. Its breaking
    /// tuples are the units that add to one, each with the unit that
    /// declares it, `{unit, owner}`, which it isn't under.
    OwnParts,
}

impl Rule {
    /// The rule the catalog's constraint `name` is.
    fn named(name: &str) -> Option<Rule> {
        match name {
            "known_units" => Some(Rule::KnownUnits),
            "acyclic" => Some(Rule::Acyclic),
            "own_parts" => Some(Rule::OwnParts),
            _ => None,
        }
    }
}

/// The catalog of `files`, filled, with each of its constraints checked of
/// it, `databases` the checked ones, and the tuples that break one
/// reported. A catalog unlike the one the compiler fills, or with a
/// constraint sema can't report, is an internal error.
pub(crate) fn check(
    files: &Files,
    cx: &Context,
    databases: &databases::Checked,
    errors: &mut Vec<Diagnostic>,
) {
    let Some(file) = files.catalog() else {
        return;
    };
    let Some(ident) = file
        .databases()
        .next()
        .and_then(|decl| decl.name())
        .and_then(|name| name.ident())
    else {
        errors.push(internal("the catalog declares no database", file.span()));
        return;
    };
    let span = trendy_syntax::span(ident.text_range());
    let Some(identity) = cx.namespaces.database_named(ident.text(), span) else {
        return;
    };
    let declared = databases.relvars.get(&identity);
    let mut args = BTreeMap::new();
    for (name, attributes) in RELVARS {
        let heading = declared
            .and_then(|relvars| relvars.get(name))
            .cloned()
            .flatten()
            .and_then(|ty| match ty {
                hir::Type::Relation(heading) => Some(heading),
                _ => None,
            })
            .filter(|heading| as_filled(heading, attributes));
        let Some(heading) = heading else {
            errors.push(internal(
                &format!(
                    "the catalog doesn't declare `var {}.{name}: relation {{ {} }};`, which the compiler fills",
                    ident.text(),
                    attributes
                        .iter()
                        .map(|a| format!("{a}: text"))
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
                span,
            ));
            return;
        };
        let tuples = filled(name, files, cx);
        args.insert(
            name.to_string(),
            hir::Expr::Constant(hir::Value::Relation(hir::RelationValue { heading, tuples })),
        );
    }
    if let Some(extra) = declared
        .into_iter()
        .flat_map(BTreeMap::keys)
        .find(|name| RELVARS.iter().all(|(filled, _)| filled != name))
    {
        errors.push(internal(
            &format!("the compiler fills no relvar `{extra}` of the catalog"),
            span,
        ));
    }
    let units: Vec<&str> = files.named().map(|(name, _)| name).collect();
    for constraint in databases
        .constraints
        .iter()
        .filter(|c| c.database == identity)
    {
        let Some(rule) = Rule::named(&constraint.name) else {
            errors.push(internal(
                &format!(
                    "the compiler reports no breaking of the catalog's constraint `{}`",
                    constraint.name
                ),
                span,
            ));
            continue;
        };
        for tuples in broken(constraint, &args, cx) {
            match tuples {
                Ok(tuples) => errors.extend(reported(rule, &tuples, files, &units, cx)),
                Err(why) => errors.push(internal(&why, span)),
            }
        }
    }
}

/// Whether `heading` has `attributes`, each a `text`, and no others.
fn as_filled(heading: &hir::Heading, attributes: &[&str]) -> bool {
    heading.attributes().count() == attributes.len()
        && attributes
            .iter()
            .all(|a| heading.get(a) == Some(&hir::Type::Scalar(hir::Scalar::Text)))
}

/// The tuples of the catalog's relvar `name`, one of [`RELVARS`], for the
/// program of `files`.
fn filled(name: &str, files: &Files, cx: &Context) -> BTreeSet<hir::TupleValue> {
    let text = |t: &str| hir::Value::Text(t.to_string());
    let tuple = |attributes: &[(&str, &str)]| {
        hir::TupleValue::new(
            attributes
                .iter()
                .map(|&(a, v)| (a.to_string(), text(v)))
                .collect(),
        )
    };
    match name {
        "databases" => files
            .named()
            .flat_map(|(unit, file)| {
                declared_databases(file, cx)
                    .into_iter()
                    .map(|database| tuple(&[("database", &database), ("unit", unit)]))
                    .collect::<Vec<_>>()
            })
            .collect(),
        "parts" => files
            .named()
            .flat_map(|(unit, file)| {
                added(file, cx)
                    .into_iter()
                    .map(|(database, part, _)| {
                        tuple(&[("database", &database), ("part", &part), ("unit", unit)])
                    })
                    .collect::<Vec<_>>()
            })
            .collect(),
        "within" => {
            let units: Vec<&str> = files.named().map(|(unit, _)| unit).collect();
            units
                .iter()
                .flat_map(|&unit| {
                    units
                        .iter()
                        .filter(move |&&owner| owner == unit || under(unit, owner))
                        .map(move |&owner| tuple(&[("owner", owner), ("unit", unit)]))
                })
                .collect()
        }
        "units" => files
            .named()
            .map(|(unit, _)| {
                hir::TupleValue::new(BTreeMap::from([("unit".to_string(), text(unit))]))
            })
            .collect(),
        "uses" => files
            .named()
            .flat_map(|(unit, file)| {
                file.uses().flat_map(move |uses| {
                    let section = match uses.section() {
                        Section::Interface => "interface",
                        Section::Implementation => "implementation",
                    };
                    uses.units()
                        .filter_map(|used| {
                            Some(hir::TupleValue::new(BTreeMap::from([
                                ("section".to_string(), text(section)),
                                ("unit".to_string(), text(unit)),
                                ("used".to_string(), text(&used.name()?.text())),
                            ])))
                        })
                        .collect::<Vec<_>>()
                })
            })
            .collect(),
        _ => unreachable!("the compiler fills the relvars it names"),
    }
}

/// Whether the unit `unit` is under the directory of the unit `owner`, as
/// `library.loans`, of `library/loans.trendy`, is under `library`'s.
fn under(unit: &str, owner: &str) -> bool {
    unit.strip_prefix(owner)
        .is_some_and(|rest| rest.starts_with('.'))
}

/// The databases `file` declares, each by its name in the program.
fn declared_databases(file: &SourceFile, cx: &Context) -> Vec<String> {
    file.databases()
        .filter_map(|decl| decl.name()?.ident())
        .map(|ident| {
            let unit = cx
                .namespaces
                .unit_at(trendy_syntax::span(ident.text_range()));
            cx.namespaces.value_identity(ident.text(), unit)
        })
        .collect()
}

/// What `file` adds to databases: each relvar and constraint, with its
/// database's name in the program, its own name, and where it names the
/// database, but for one of a database there isn't, which is reported.
fn added(file: &SourceFile, cx: &Context) -> Vec<(String, String, Span)> {
    let relvars = file
        .relvars()
        .filter_map(|decl| Some((decl.part_of()?, decl.name()?.ident()?)));
    let constraints = file
        .constraints()
        .filter_map(|decl| Some((decl.part_of()?, decl.name()?.ident()?)));
    relvars
        .chain(constraints)
        .filter_map(|(part_of, ident)| {
            let written = part_of.database()?.ident()?;
            let span = trendy_syntax::span(written.text_range());
            let database = cx.namespaces.database_named(written.text(), span)?;
            Some((database, ident.text().to_string(), span))
        })
        .collect()
}

/// The tuples that break each part of `constraint` that doesn't hold of
/// `args`, the catalog's relvars' values, or why computing it failed.
fn broken(
    constraint: &DatabaseConstraint,
    args: &BTreeMap<String, hir::Expr>,
    cx: &Context,
) -> Vec<Result<Vec<hir::TupleValue>, String>> {
    let computed = |expr: &hir::Expr| -> Result<hir::Value, String> {
        ops::evaluated_with(expr.clone(), args, cx.bindings())
            .ok()
            .and_then(|e| ops::known_value(&e).cloned())
            .ok_or_else(|| {
                format!(
                    "the catalog's constraint `{}` can't be computed when compiling",
                    constraint.name
                )
            })
    };
    constraint
        .parts
        .iter()
        .filter_map(|part| match computed(&part.condition) {
            Ok(hir::Value::Boolean(true)) => None,
            Ok(_) => Some(match part.breaking.as_ref().map(computed) {
                Some(Ok(hir::Value::Relation(relation))) => {
                    Ok(relation.tuples.into_iter().collect())
                }
                _ => Err(format!(
                    "the catalog's constraint `{}` doesn't say what breaks it: {}",
                    constraint.name, part.written
                )),
            }),
            Err(why) => Some(Err(why)),
        })
        .collect()
}

/// What `rule`'s breaking `tuples` say, each where the program of `files`
/// says it, whose units are `units`.
fn reported(
    rule: Rule,
    tuples: &[hir::TupleValue],
    files: &Files,
    units: &[&str],
    cx: &Context,
) -> Vec<Diagnostic> {
    let text = |tuple: &hir::TupleValue, attribute: &str| match tuple.get(attribute) {
        Some(hir::Value::Text(t)) => t.clone(),
        other => unreachable!("the catalog's attributes are text, not {other:?}"),
    };
    match rule {
        Rule::KnownUnits => tuples
            .iter()
            .flat_map(|tuple| {
                let used = text(tuple, "used");
                naming(files, None, &used)
                    .into_iter()
                    .map(move |(_, _, name)| errors::unknown_unit(&used, name.span(), units))
                    .collect::<Vec<_>>()
            })
            .collect(),
        Rule::Acyclic => {
            let on_cycles: BTreeSet<String> = tuples.iter().map(|t| text(t, "unit")).collect();
            cycles(files, &on_cycles)
        }
        Rule::OwnParts => tuples
            .iter()
            .flat_map(|tuple| {
                let (unit, owner) = (text(tuple, "unit"), text(tuple, "owner"));
                let file = |name: &str| {
                    files
                        .named()
                        .find(|(u, _)| *u == name)
                        .map(|(_, file)| file)
                };
                // The databases `owner` declares.
                let declared: BTreeSet<String> = file(&owner).map_or_else(BTreeSet::new, |f| {
                    declared_databases(f, cx).into_iter().collect()
                });
                file(&unit)
                    .map(|f| added(f, cx))
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|(database, _, _)| declared.contains(database))
                    .map(|(database, part, at)| {
                        errors::foreign_part(&database, &part, at, &unit, &owner)
                    })
                    .collect::<Vec<_>>()
            })
            .collect(),
    }
}

/// Each place in `files` where a unit, `unit` if given, uses `used`: its
/// file, its `uses`, and the name it writes.
fn naming<'f>(
    files: &'f Files,
    unit: Option<&str>,
    used: &str,
) -> Vec<(&'f SourceFile, ast::UsesDecl, ast::UnitName)> {
    files
        .named()
        .filter(|(name, _)| unit.is_none_or(|unit| unit == *name))
        .flat_map(|(_, file)| {
            file.uses()
                .flat_map(|uses| {
                    uses.units()
                        .filter_map(|u| u.name())
                        .filter(|name| name.text() == used)
                        .map(|name| (file, uses.clone(), name))
                        .collect::<Vec<_>>()
                })
                .collect::<Vec<_>>()
        })
        .collect()
}

/// Each cycle of units in `files` through the units `on_cycles`, once: a
/// path of `uses` from one of them back to itself, found from each in
/// turn that no cycle reported yet passes through.
fn cycles(files: &Files, on_cycles: &BTreeSet<String>) -> Vec<Diagnostic> {
    // Each unit's used units, in name order.
    let mut using: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for (unit, file) in files.named() {
        for uses in file.uses() {
            for name in uses.units().filter_map(|u| u.name()) {
                using
                    .entry(unit.to_string())
                    .or_default()
                    .insert(name.text());
            }
        }
    }
    let mut reported: BTreeSet<String> = BTreeSet::new();
    let mut diagnostics = Vec::new();
    for start in on_cycles {
        if reported.contains(start) {
            continue;
        }
        let Some(cycle) = path_back(&using, start) else {
            continue;
        };
        reported.extend(cycle.iter().cloned());
        // Each `uses` along the cycle: of the next unit, in each unit.
        let steps: Vec<(String, Span)> = cycle
            .iter()
            .zip(cycle.iter().cycle().skip(1))
            .filter_map(|(unit, next)| {
                let (_, _, name) = naming(files, Some(unit), next).into_iter().next()?;
                Some((unit.clone(), name.span()))
            })
            .collect();
        let removal = match cycle.as_slice() {
            [only] => naming(files, Some(only), only)
                .into_iter()
                .next()
                .map(|(file, uses, name)| sections::entry_removal(file, &uses, &name)),
            _ => None,
        };
        diagnostics.push(errors::unit_cycle(&cycle, &steps, removal));
    }
    diagnostics
}

/// The shortest path of `uses`, given each unit's used units, from `start`
/// back to it: its units, `start` first.
fn path_back(using: &BTreeMap<String, BTreeSet<String>>, start: &str) -> Option<Vec<String>> {
    let mut came_from: BTreeMap<&str, &str> = BTreeMap::new();
    let mut frontier = vec![start];
    while !frontier.is_empty() {
        let mut next = Vec::new();
        for unit in frontier {
            for used in using.get(unit).into_iter().flatten() {
                if used == start {
                    let mut path = vec![unit.to_string()];
                    let mut at = unit;
                    while at != start {
                        at = came_from[at];
                        path.push(at.to_string());
                    }
                    path.reverse();
                    return Some(path);
                }
                if !came_from.contains_key(used.as_str()) {
                    came_from.insert(used, unit);
                    next.push(used.as_str());
                }
            }
        }
        frontier = next;
    }
    None
}

/// An internal error: the catalog, at `span`, isn't as the compiler fills
/// and reports it.
fn internal(message: &str, span: Span) -> Diagnostic {
    Diagnostic::error(
        Code::Internal,
        message,
        span,
        "not as the compiler fills it",
    )
}
