//! Assignment statements and `assign` blocks (TTM RM Prescription 21).
//!
//! Each assignment, or shorthand for one, is a change to a variable. The
//! changes of one `assign` block are made at once: every value is computed
//! from the variables as they were before the block, and several changes
//! to one variable are merged, if they don't conflict, into its one new
//! value. So the order of a block's assignments never matters.

use std::collections::BTreeMap;

use trendy_diag::{Diagnostic, Span};
use trendy_hir as hir;
use trendy_syntax::ast::{self, Clause};
use trendy_syntax::{SyntaxKind, SyntaxToken};

use crate::body::{BodyNames, Context};
use crate::check::{Assigned, Checker, Unchanged};
use crate::types::{Expected, Origin};
use crate::{Kind, errors, keys, ops};

/// An assignment statement outside a block, of its one clause; `clauses`
/// has several only in a statement the parser reported.
pub(crate) fn statement(
    clauses: &[Clause],
    within: hir::OperatorId,
    cx: &Context,
    errors: &mut Vec<Diagnostic>,
) -> Option<hir::Statement> {
    assignments(clauses, within, cx, errors)
}

/// An `assign` block: its assignments, made at once.
pub(crate) fn block(
    block: &ast::AssignBlock,
    within: hir::OperatorId,
    cx: &Context,
    errors: &mut Vec<Diagnostic>,
) -> Option<hir::Statement> {
    let clauses: Vec<Clause> = block.statements().flat_map(|stmt| stmt.clauses()).collect();
    if clauses.is_empty() {
        errors.push(errors::empty_assign_block(block));
        return None;
    }
    assignments(&clauses, within, cx, errors)
}

/// `clauses`, each checked into a change, and each variable's changes
/// merged into its new value. A variable whose new value is known when
/// compiling is checked against its keys now.
fn assignments(
    clauses: &[Clause],
    within: hir::OperatorId,
    cx: &Context,
    errors: &mut Vec<Diagnostic>,
) -> Option<hir::Statement> {
    let mut ok = true;
    // Each variable's changes, in the order variables first appear.
    let mut changes: Vec<(hir::VariableId, Vec<(&Clause, Change)>)> = Vec::new();
    for clause in clauses {
        let Some((target, change)) = change(clause, within, cx, errors) else {
            ok = false;
            continue;
        };
        match changes.iter_mut().find(|(t, _)| *t == target) {
            Some((_, list)) => list.push((clause, change)),
            None => changes.push((target, vec![(clause, change)])),
        }
    }
    if !ok {
        return None;
    }
    let mut assignments = Vec::new();
    for (target, list) in changes {
        let variable = &cx.variables[target.0];
        let Some(value) = merged(target, variable, list, within, cx, errors) else {
            ok = false;
            continue;
        };
        assignments.push(hir::Assignment { target, value });
    }
    ok.then_some(hir::Statement::Assign(assignments))
}

/// What one clause does to its variable.
enum Change {
    /// `v := x`: the whole new value, and where it is written.
    Value(hir::Expr, ast::Expr),
    /// An `update` of a tuple: its attributes given new values.
    Attributes(hir::Heading, BTreeMap<String, hir::Expr>),
    /// A change to a relation's tuples.
    Tuples(TupleChange),
}

/// A change to a relation's tuples: those it removes from the old value,
/// and those it adds.
struct TupleChange {
    removed: Option<Removal>,
    added: Option<Addition>,
}

/// The tuples a change removes.
enum Removal {
    /// Those of the old value for which the condition is true: a `delete`'s
    /// or an `update`'s, which, without `where`, is `true`.
    Where(hir::Expr),
    /// Those of a relation, all of which must be in the old value: an
    /// `i_delete`'s.
    Included(hir::Expr),
}

/// The tuples a change adds.
struct Addition {
    tuples: hir::Expr,
    /// Whether none may be there already: a `d_insert`'s.
    disjoint: bool,
}

/// The variable `clause` assigns, and its change, or `None` after
/// reporting why it has none.
fn change(
    clause: &Clause,
    within: hir::OperatorId,
    cx: &Context,
    errors: &mut Vec<Diagnostic>,
) -> Option<(hir::VariableId, Change)> {
    let written = clause.target()?;
    let id = assigned_variable(&written, within, cx, errors)?;
    let variable = &cx.variables[id.0];
    let declaration = &cx.declarations[&variable.name];
    let declared = declaration
        .type_expr()
        .map(Origin::Type)
        .or_else(|| declaration.initializer().map(Origin::Value))?;
    let target = Target {
        id,
        variable,
        written: &written,
        expected: Expected {
            ty: variable.ty.clone(),
            origin: Origin::Variable(variable.name.clone(), Box::new(declared)),
        },
    };
    let mut names = BodyNames { cx, within };
    let mut checker = Checker::new(&mut names);
    let change = match (clause, clause.keyword()) {
        (Clause::Assignment(_), _) => {
            let written = clause.operand()?;
            let value = checker.expr(&written, Some(&target.expected), errors)?;
            if value == target.current() {
                let statement = clause.statement()?;
                errors.push(errors::own_value(&variable.name, clause.span(), &statement));
                return None;
            }
            Change::Value(value, written)
        }
        (Clause::Update(_), Some(keyword)) => {
            update(clause, &keyword, &target, &mut checker, errors)?
        }
        (_, Some(keyword)) => {
            Change::Tuples(shorthand(clause, &keyword, &target, &mut checker, errors)?)
        }
        (_, None) => return None,
    };
    Some((id, change))
}

/// The variable an assignment assigns: its id, its declaration, where it
/// is written, and the type its value must have.
struct Target<'a> {
    id: hir::VariableId,
    variable: &'a hir::Variable,
    written: &'a ast::Expr,
    expected: Expected,
}

impl Target<'_> {
    /// The variable's value before the assignment.
    fn current(&self) -> hir::Expr {
        hir::Expr::Variable(self.id)
    }
}

/// `clause`, an `update` after `keyword`, of `target`: of a tuple, its
/// attributes given new values; of a relation, the tuples for which the
/// condition is true, if there is one, replaced.
fn update(
    clause: &Clause,
    keyword: &SyntaxToken,
    target: &Target,
    checker: &mut Checker,
    errors: &mut Vec<Diagnostic>,
) -> Option<Change> {
    let list = clause.assignments()?;
    let statement = clause.statement()?;
    let assigned = Assigned {
        assigning: ops::Assigning::Update,
        keyword,
        list: &list,
        operand: target.written,
    };
    let unchanged = |why| match why {
        Unchanged::Empty => errors::empty_update(&list, &statement),
        Unchanged::OwnValues => errors::own_values_updated(clause.span(), &statement),
    };
    match (clause.where_keyword(), &target.variable.ty) {
        (None, hir::Type::Tuple(_)) => {
            match checker.assign_attributes(target.current(), &assigned, unchanged, errors)? {
                hir::Expr::Extend(_, computed, values) => {
                    Some(Change::Attributes(computed, values))
                }
                _ => unreachable!("a variable's value is known only when running"),
            }
        }
        (Some(token), hir::Type::Tuple(_)) => {
            let span = trendy_syntax::span(token.text_range());
            errors.push(errors::not_restrictable(
                span,
                &target.variable.ty,
                target.written,
            ));
            None
        }
        (token, ty) => {
            let hir::Type::Relation(heading) = ty else {
                // A scalar has no attributes to update.
                checker.assign_attributes(target.current(), &assigned, unchanged, errors)?;
                return None;
            };
            let condition = match token {
                Some(token) => {
                    checker.condition(heading.clone(), &clause.operand()?, token, errors)?
                }
                None => hir::Expr::Constant(hir::Value::Boolean(true)),
            };
            let selected = restricted(target.current(), condition.clone());
            let replaced = checker.assign_attributes(selected, &assigned, unchanged, errors)?;
            Some(Change::Tuples(TupleChange {
                removed: Some(Removal::Where(condition)),
                added: Some(Addition {
                    tuples: replaced,
                    disjoint: false,
                }),
            }))
        }
    }
}

/// `clause`, a shorthand after `keyword` for changing the tuples of the
/// relation `target`: `insert`, `d_insert`, `delete` or `i_delete`.
fn shorthand(
    clause: &Clause,
    keyword: &SyntaxToken,
    target: &Target,
    checker: &mut Checker,
    errors: &mut Vec<Diagnostic>,
) -> Option<TupleChange> {
    let hir::Type::Relation(heading) = &target.variable.ty else {
        let span = trendy_syntax::span(keyword.text_range());
        errors.push(errors::not_relational(
            keyword.text(),
            span,
            &target.variable.ty,
            target.written,
        ));
        return None;
    };
    let operand = clause.operand()?;
    if keyword.kind() == SyntaxKind::DeleteKw {
        let token = clause.where_keyword()?;
        let condition = checker.condition(heading.clone(), &operand, token, errors)?;
        return Some(TupleChange {
            removed: Some(Removal::Where(condition)),
            added: None,
        });
    }
    let tuples = checker.expr(&operand, Some(&target.expected), errors)?;
    if let hir::Expr::Constant(hir::Value::Relation(relation)) = &tuples
        && relation.tuples.is_empty()
    {
        let statement = clause.statement()?;
        errors.push(errors::no_tuples_given(
            keyword.text(),
            clause.span(),
            &statement,
        ));
        return None;
    }
    Some(match keyword.kind() {
        SyntaxKind::IDeleteKw => TupleChange {
            removed: Some(Removal::Included(tuples)),
            added: None,
        },
        kind => TupleChange {
            removed: None,
            added: Some(Addition {
                tuples,
                disjoint: kind == SyntaxKind::DInsertKw,
            }),
        },
    })
}

/// The new value of the variable `target`, `variable`, from its changes,
/// `list`, in source order, or `None` after reporting a change whose
/// effect is known when compiling, or a conflict between two of them.
fn merged(
    target: hir::VariableId,
    variable: &hir::Variable,
    list: Vec<(&Clause, Change)>,
    within: hir::OperatorId,
    cx: &Context,
    errors: &mut Vec<Diagnostic>,
) -> Option<hir::Expr> {
    if known_effects(target, &variable.name, &list, errors) {
        return None;
    }
    // A whole new value conflicts with any other change.
    let whole = list
        .iter()
        .position(|(_, c)| matches!(c, Change::Value(..)));
    if let (Some(i), true) = (whole, list.len() > 1) {
        // The first other change.
        let other = usize::from(i == 0);
        let (first, second) = (i.min(other), i.max(other));
        errors.push(errors::conflicting_changes(
            &variable.name,
            list[first].0.span(),
            list[second].0.span(),
            &format!(
                "`{} := …` gives `{}` a whole new value",
                variable.name, variable.name
            ),
        ));
        return None;
    }
    let mut list = list.into_iter();
    let (clause, first) = list.next()?;
    match first {
        Change::Value(value, written) => {
            if let hir::Expr::Constant(hir::Value::Relation(relation)) = &value {
                let keys = keys::written(&cx.declarations[&variable.name], &variable.keys);
                let mut names = BodyNames { cx, within };
                if !keys::satisfied(
                    &variable.name,
                    &keys,
                    relation,
                    &written,
                    &mut names,
                    errors,
                ) {
                    return None;
                }
            }
            Some(value)
        }
        Change::Attributes(computed, values) => {
            let mut changes = vec![(clause, computed, values)];
            for (clause, change) in list {
                let Change::Attributes(computed, values) = change else {
                    unreachable!("a tuple variable's changes are updates");
                };
                changes.push((clause, computed, values));
            }
            attributes_merged(target, &variable.name, changes, errors)
        }
        Change::Tuples(change) => {
            let mut changes = vec![(clause, change)];
            for (clause, change) in list {
                let Change::Tuples(change) = change else {
                    unreachable!("a relation variable's changes are to its tuples");
                };
                changes.push((clause, change));
            }
            tuples_merged(target, variable, changes, errors)
        }
    }
}

/// Whether a change in `list`, the changes to the variable `target`,
/// `name`, has an effect known when compiling, on every tuple or none,
/// after reporting each one: it is written as that effect. These are
/// checked with the variable's other changes, on which their errors
/// depend.
fn known_effects(
    target: hir::VariableId,
    name: &str,
    list: &[(&Clause, Change)],
    errors: &mut Vec<Diagnostic>,
) -> bool {
    let alone = list.len() == 1;
    let found: Vec<Diagnostic> = list
        .iter()
        .filter_map(|(clause, change)| known_effect(target, name, clause, change, alone))
        .collect();
    let any = !found.is_empty();
    errors.extend(found);
    any
}

/// The error for `clause`, `change` to the variable `target`, `name`, if
/// its effect is known when compiling: a condition that is, which selects
/// every tuple or none, an `i_delete` of the variable itself, which deletes
/// every tuple, or an insert of it into itself, which changes nothing.
/// `alone` says whether it is the variable's one change. With others, the
/// empty relation's assignment, which a deletion of every tuple is, would
/// conflict with them, and an insert into itself adds back the tuples they
/// remove, which changes something.
fn known_effect(
    target: hir::VariableId,
    name: &str,
    clause: &Clause,
    change: &Change,
    alone: bool,
) -> Option<Diagnostic> {
    let Change::Tuples(TupleChange { removed, added }) = change else {
        return None;
    };
    let keyword = clause.keyword()?;
    let itself = |tuples: &hir::Expr| *tuples == hir::Expr::Variable(target);
    match (removed, added) {
        (Some(Removal::Where(hir::Expr::Constant(hir::Value::Boolean(always)))), _) => {
            // An `update` without `where` is of every tuple, as it should be.
            let token = clause.where_keyword()?;
            Some(match (keyword.kind(), always) {
                (SyntaxKind::DeleteKw, true) => {
                    errors::every_tuple_deleted(keyword.text(), name, clause.span(), alone)
                }
                (_, true) => {
                    let start = trendy_syntax::span(token.text_range()).start;
                    let condition = Span::new(start, clause.operand()?.span().end);
                    errors::every_tuple_updated(condition, clause.condition_removal()?)
                }
                (_, false) => {
                    errors::no_tuple_changed(keyword.text(), clause.span(), &clause.statement()?)
                }
            })
        }
        (Some(Removal::Included(tuples)), None) if itself(tuples) => Some(
            errors::every_tuple_deleted(keyword.text(), name, clause.span(), alone),
        ),
        (None, Some(Addition { tuples, .. })) if alone && itself(tuples) => Some(
            errors::inserted_into_itself(keyword.text(), name, clause.span(), &clause.statement()?),
        ),
        _ => None,
    }
}

/// A tuple variable's new value from its `update`s, each in a clause and
/// giving attributes new values: one `replace` of them all, if no two give
/// the same attribute one.
fn attributes_merged(
    target: hir::VariableId,
    name: &str,
    changes: Vec<(&Clause, hir::Heading, BTreeMap<String, hir::Expr>)>,
    errors: &mut Vec<Diagnostic>,
) -> Option<hir::Expr> {
    let mut types = BTreeMap::new();
    let mut values: BTreeMap<String, (Span, hir::Expr)> = BTreeMap::new();
    for (clause, computed, attributes) in changes {
        for (attribute, value) in attributes {
            if let Some((first, _)) = values.get(&attribute) {
                errors.push(errors::conflicting_changes(
                    name,
                    *first,
                    clause.span(),
                    &format!("both give `{attribute}` a new value"),
                ));
                return None;
            }
            let ty = computed
                .get(&attribute)
                .expect("each value has its attribute's type")
                .clone();
            types.insert(attribute.clone(), ty);
            values.insert(attribute, (clause.span(), value));
        }
    }
    let values = values.into_iter().map(|(a, (_, v))| (a, v)).collect();
    Some(
        ops::extend(
            hir::Expr::Variable(target),
            hir::Heading::new(types),
            values,
        )
        .unwrap_or_else(|_| unreachable!("a variable's value is known only when running")),
    )
}

/// A relation variable's new value from the `changes` to its tuples: its
/// old tuples less every tuple a change removes, and with every tuple one
/// adds. Two changes that remove the same tuple conflict, which is reported
/// now if it is known now, and else stops the program when it runs.
fn tuples_merged(
    target: hir::VariableId,
    variable: &hir::Variable,
    changes: Vec<(&Clause, TupleChange)>,
    errors: &mut Vec<Diagnostic>,
) -> Option<hir::Expr> {
    let current = || hir::Expr::Variable(target);
    let mut wheres = Vec::new();
    let mut included = Vec::new();
    let mut additions = Vec::new();
    for (clause, change) in changes {
        match change.removed {
            Some(Removal::Where(condition)) => wheres.push(condition),
            Some(Removal::Included(tuples)) => included.push((clause, tuples)),
            None => {}
        }
        additions.extend(change.added);
    }
    let kept = match (&wheres[..], included.is_empty()) {
        ([], true) => current(),
        // One condition, and nothing else removed: the others.
        ([condition], true) => restricted(current(), negated(condition.clone())),
        _ => {
            // Only relations named by `i_delete`s are known now, so a
            // conflict found now is between two of those.
            if let Some((first, second, tuple)) = shared(&included) {
                errors.push(errors::conflicting_tuple(
                    &variable.name,
                    first.span(),
                    second.span(),
                    &tuple,
                ));
                return None;
            }
            let strict = !included.is_empty();
            let all = wheres
                .into_iter()
                .map(|condition| restricted(current(), condition))
                .chain(included.into_iter().map(|(_, tuples)| tuples))
                .reduce(|all, tuples| {
                    ops::combined(all, tuples, hir::Combination::DisjointChanges)
                        .unwrap_or_else(|_| unreachable!("tuples shared now are reported above"))
                })
                .expect("several changes remove tuples");
            let combination = if strict {
                hir::Combination::IncludedMinus
            } else {
                hir::Combination::Minus
            };
            combined(current(), all, combination)
        }
    };
    // A `d_insert` is checked against the tuples kept and the other
    // `d_insert`s' only, whatever the order, so they are added first.
    additions.sort_by_key(|addition| !addition.disjoint);
    let mut value = kept;
    for addition in additions {
        let combination = if addition.disjoint {
            hir::Combination::DisjointUnion
        } else {
            hir::Combination::Union
        };
        value = combined(value, addition.tuples, combination);
    }
    Some(value)
}

/// Two of the `i_delete`s in `included`, each in a clause and naming a
/// relation, that are known when compiling and name the same tuple, and
/// that tuple.
fn shared<'a>(
    included: &[(&'a Clause, hir::Expr)],
) -> Option<(&'a Clause, &'a Clause, hir::TupleValue)> {
    let known: Vec<(&Clause, &hir::RelationValue)> = included
        .iter()
        .filter_map(|(clause, tuples)| match tuples {
            hir::Expr::Constant(hir::Value::Relation(relation)) => Some((*clause, relation)),
            _ => None,
        })
        .collect();
    known
        .iter()
        .enumerate()
        .find_map(|(i, (second, relation))| {
            known[..i].iter().find_map(|(first, earlier)| {
                let tuple = relation.tuples.intersection(&earlier.tuples).next()?;
                Some((*first, *second, tuple.clone()))
            })
        })
}

/// The variable `target` names, which an assignment assigns, or `None`
/// after reporting why it can't be assigned: it isn't a variable, or it is
/// a parameter, which is read-only.
fn assigned_variable(
    target: &ast::Expr,
    within: hir::OperatorId,
    cx: &Context,
    errors: &mut Vec<Diagnostic>,
) -> Option<hir::VariableId> {
    let ast::Expr::Name(name) = target else {
        return None;
    };
    let ident = name.ident()?;
    let (name, span) = (ident.text(), trendy_syntax::span(ident.text_range()));
    if cx.operators[within.0].params.contains_key(name) {
        let hidden = cx.variable_ids.contains_key(name);
        errors.push(errors::assigned_parameter(name, span, hidden));
        return None;
    }
    if let Some(&id) = cx.variable_ids.get(name) {
        return Some(id);
    }
    match cx.values.get(name) {
        // A variable with errors in its declaration is already reported.
        Some((Kind::Variable, _)) => {}
        Some((kind @ (Kind::Operator | Kind::Builtin), _)) => {
            errors.push(errors::assigned_operator(name, span, kind));
        }
        None => {
            let variables: Vec<&str> = cx.variable_ids.keys().map(String::as_str).collect();
            errors.push(errors::unknown_name(name, span, "variable", &variables));
        }
    }
    None
}

/// The tuples of the relation `operand` for which `condition` is true.
fn restricted(operand: hir::Expr, condition: hir::Expr) -> hir::Expr {
    ops::restrict(operand, condition)
        .unwrap_or_else(|_| unreachable!("a variable's value is known only when running"))
}

/// `not condition`.
fn negated(condition: hir::Expr) -> hir::Expr {
    ops::unary(hir::UnaryOp::Not, condition).unwrap_or_else(|_| unreachable!("`not` never fails"))
}

/// `lhs` and `rhs`, relations, combined as `combination` says.
fn combined(lhs: hir::Expr, rhs: hir::Expr, combination: hir::Combination) -> hir::Expr {
    ops::combined(lhs, rhs, combination)
        .unwrap_or_else(|_| unreachable!("a variable's value is known only when running"))
}
