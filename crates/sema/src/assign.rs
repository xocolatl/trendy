//! Assignment statements and `assign` blocks (TTM RM Prescription 21).
//!
//! Each assignment, or shorthand for one, is a change to a variable, of the
//! program or local to the operator. The
//! changes of one `assign` block are made at once: every value is computed
//! from the variables as they were before the block, and several changes
//! to one variable are merged, if they don't conflict, into its one new
//! value. So the order of a block's assignments never matters.

use std::collections::BTreeMap;

use trendy_diag::{Diagnostic, Span};
use trendy_hir as hir;
use trendy_syntax::ast::{self, AstNode, Clause, VarDecl};
use trendy_syntax::{SyntaxKind, SyntaxToken};

use crate::check::{Assigned, Checker, Names, Unchanged};
use crate::types::{self, Expected, Origin};
use crate::{errors, keys, ops};

/// What names in an operator's body refer to, which an assignment there
/// can assign.
pub(crate) trait Assigning: Names {
    /// The variable `name`, written at `span`, that an assignment assigns,
    /// of the program or local, or `None` after reporting why it can't be
    /// assigned.
    fn assigned(
        &mut self,
        name: &str,
        span: Span,
        errors: &mut Vec<Diagnostic>,
    ) -> Option<Assignee>;
}

/// A variable an assignment assigns, of the program or local to the
/// operator, or a `var` parameter, as it is declared.
pub(crate) struct Assignee {
    pub(crate) target: hir::Target,
    /// Its value before the assignment: the variable itself, or, for a
    /// value block's local variable whose value is known when compiling,
    /// that value, which no `Let` binds.
    pub(crate) current: hir::Expr,
    pub(crate) name: String,
    pub(crate) ty: hir::Type,
    pub(crate) keys: Vec<hir::Key>,
    pub(crate) declaration: Declared,
}

/// Where an assignee is declared.
#[derive(Clone)]
pub(crate) enum Declared {
    /// A variable, of the program or local, by this declaration.
    Variable(VarDecl),
    /// A `var` parameter, whose type is set here.
    Parameter(Origin),
}

impl Assignee {
    /// The local variable `id`, `local`, which `declaration` declares.
    pub(crate) fn local(id: hir::LocalId, local: &hir::Local, declaration: &VarDecl) -> Self {
        Assignee {
            target: hir::Target::Local(id),
            current: hir::Expr::Local(id),
            name: local.name.clone(),
            ty: local.ty.clone(),
            keys: local.keys.clone(),
            declaration: Declared::Variable(declaration.clone()),
        }
    }

    /// The `var` parameter `name`, of type `ty`, which is set at `origin`.
    pub(crate) fn parameter(name: &str, ty: hir::Type, origin: Origin) -> Self {
        Assignee {
            target: hir::Target::Parameter(name.to_string()),
            current: hir::Expr::Parameter(name.to_string()),
            name: name.to_string(),
            ty,
            keys: Vec::new(),
            declaration: Declared::Parameter(origin),
        }
    }

    /// Where its type is set: its declared type, or else its initial value,
    /// or a parameter's type.
    fn declared(&self) -> Option<Origin> {
        match &self.declaration {
            Declared::Variable(declaration) => declaration
                .type_expr()
                .map(Origin::Type)
                .or_else(|| declaration.initializer().map(Origin::Value)),
            Declared::Parameter(origin) => Some(origin.clone()),
        }
    }
}

/// An assignment statement outside a block, of its one clause, whose names
/// `names` resolves; `clauses` has several only in a statement the parser
/// reported.
pub(crate) fn statement(
    clauses: &[Clause],
    names: &mut dyn Assigning,
    errors: &mut Vec<Diagnostic>,
) -> Option<hir::Statement> {
    assignments(clauses, names, errors)
}

/// An `assign` block: its assignments, made at once.
pub(crate) fn block(
    block: &ast::AssignBlock,
    names: &mut dyn Assigning,
    errors: &mut Vec<Diagnostic>,
) -> Option<hir::Statement> {
    let clauses: Vec<Clause> = block.statements().flat_map(|stmt| stmt.clauses()).collect();
    if clauses.is_empty() {
        errors.push(errors::empty_assign_block(block));
        return None;
    }
    assignments(&clauses, names, errors)
}

/// `clauses`, each checked into a change, and each variable's changes
/// merged into its new value. A variable whose new value is known when
/// compiling is checked against its keys now.
fn assignments(
    clauses: &[Clause],
    names: &mut dyn Assigning,
    errors: &mut Vec<Diagnostic>,
) -> Option<hir::Statement> {
    let mut ok = true;
    // Each variable's changes, in the order variables first appear.
    let mut changes: Vec<(Assignee, Vec<(&Clause, Change)>)> = Vec::new();
    for clause in clauses {
        let Some((assignee, change)) = change(clause, names, errors) else {
            ok = false;
            continue;
        };
        match changes
            .iter_mut()
            .find(|(a, _)| a.target == assignee.target)
        {
            Some((_, list)) => list.push((clause, change)),
            None => changes.push((assignee, vec![(clause, change)])),
        }
    }
    if !ok {
        return None;
    }
    let mut assignments = Vec::new();
    for (assignee, list) in changes {
        let Some(value) = merged(&assignee, list, names, errors) else {
            ok = false;
            continue;
        };
        assignments.push(hir::Assignment {
            target: assignee.target.clone(),
            value,
        });
    }
    ok.then_some(hir::Statement::Assign(assignments))
}

/// What one clause does to its variable.
enum Change {
    /// `v := x`: the whole new value, and where it is written.
    Value(hir::Expr, ast::Expr),
    /// `a[i] := x`, of an array: its element at a path given a new value.
    Element(ElementAt),
    /// An `update` of a tuple: its attributes given new values.
    Attributes(hir::Heading, BTreeMap<String, hir::Expr>),
    /// A change to a relation's tuples.
    Tuples(TupleChange),
}

/// An element an assignment gives a new value: the change, and where its
/// target is written.
struct ElementAt {
    change: hir::ElementChange,
    written: ast::Expr,
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
    names: &mut dyn Assigning,
    errors: &mut Vec<Diagnostic>,
) -> Option<(Assignee, Change)> {
    let written = clause.target()?;
    let Some((name, positions)) = target_path(&written) else {
        if let Some(error) = attribute_assigned(clause, &written, names, errors) {
            errors.push(error);
        }
        return None;
    };
    let ident = name.ident()?;
    let span = trendy_syntax::span(ident.text_range());
    let assignee = names.assigned(ident.text(), span, errors)?;
    let declared = assignee.declared()?;
    let target = Target {
        expected: Expected {
            ty: assignee.ty.clone(),
            origin: Origin::Variable(assignee.name.clone(), Box::new(declared)),
        },
        assignee,
        written: &written,
    };
    let mut checker = Checker::new(names);
    let change = match (clause, clause.keyword()) {
        (Clause::Load(load), _) => loaded(clause, load, &target, &mut checker, errors)?,
        (Clause::Assignment(_), _) if !positions.is_empty() => {
            Change::Element(element(clause, &target, &positions, &mut checker, errors)?)
        }
        (Clause::Assignment(_), _) => {
            let written = clause.operand()?;
            let value = checker.expr(&written, Some(&target.expected), errors)?;
            if value == target.assignee.target.read() {
                let statement = clause.statement()?;
                errors.push(errors::own_value(
                    &target.assignee.name,
                    clause.span(),
                    &statement,
                ));
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
    Some((target.assignee, change))
}

/// The variable that `written`, an assignment's target, names, and the
/// brackets of each position after it, from the variable's array in, if
/// the target is a variable, or an element of one, as in `grid[i][j]`.
fn target_path(written: &ast::Expr) -> Option<(ast::NameRef, Vec<ast::IndexExpr>)> {
    let mut positions = Vec::new();
    let mut at = written.clone();
    loop {
        match at {
            ast::Expr::Name(name) => {
                positions.reverse();
                return Some((name, positions));
            }
            ast::Expr::Index(index) => {
                at = index.operand()?;
                positions.push(index);
            }
            _ => return None,
        }
    }
}

/// `clause`, an assignment of an element of `target`, an array variable,
/// at `positions`, each array's brackets, from the variable's in: a
/// pseudovariable, as TTM calls one (RM Prescription 5), shorthand for an
/// assignment of the whole array. Its path and value, or `None` after
/// reporting why it has none.
fn element(
    clause: &Clause,
    target: &Target,
    positions: &[ast::IndexExpr],
    checker: &mut Checker,
    errors: &mut Vec<Diagnostic>,
) -> Option<ElementAt> {
    // The array at each position's level, as it is, which shows a position
    // out of range now if its value is known, and as its name reads it,
    // which shows a value that is the element's own.
    let mut array = target.assignee.current.clone();
    let mut itself = target.assignee.target.read();
    let mut ty = target.assignee.ty.clone();
    let mut origin = target.expected.origin.clone();
    let mut path = Vec::new();
    for index in positions {
        let operand = index.operand()?;
        let hir::Type::Array(element) = ty else {
            errors.push(errors::not_indexable(index, &ty, &operand));
            return None;
        };
        let integer = Expected {
            ty: hir::Type::Scalar(hir::Scalar::Integer),
            origin: Origin::Operator(index.l_bracket()?),
        };
        let written_position = index.index()?;
        let position = checker.expr(&written_position, Some(&integer), errors)?;
        let written = ast::Expr::Index(index.clone());
        array = types::evaluated(
            ops::index(array, position.clone()),
            &written,
            Some(&written_position),
            errors,
        )?;
        itself = hir::Expr::Index(Box::new(itself), Box::new(position.clone()));
        path.push(position);
        origin = origin.elements(&operand.syntax().text().to_string());
        ty = *element;
    }
    let value = checker.expr(&clause.operand()?, Some(&Expected { ty, origin }), errors)?;
    if value == itself {
        let statement = clause.statement()?;
        let text = target.written.syntax().text().to_string();
        errors.push(errors::own_value(&text, clause.span(), &statement));
        return None;
    }
    Some(ElementAt {
        change: hir::ElementChange { path, value },
        written: target.written.clone(),
    })
}

/// The error for `clause`, whose target, `written`, is an attribute, as
/// other languages assign an object's field, `t.name := x`: a tuple's
/// attributes are changed by `update`, as Tutorial D has no attribute
/// pseudovariables (TTM chapter 3). It has a fix when the attributes end
/// the target, after a variable or an element of one, and the value names
/// none of the attributes that the fix brings into scope.
fn attribute_assigned(
    clause: &Clause,
    written: &ast::Expr,
    names: &mut dyn Assigning,
    errors: &mut Vec<Diagnostic>,
) -> Option<Diagnostic> {
    // The attributes at the end, outermost first, and what they are of.
    let mut attributes = Vec::new();
    let mut base = written.clone();
    while let ast::Expr::Extract(extract) = &base {
        let name = extract.attribute()?.ident()?.text().to_string();
        attributes.push(name);
        base = extract.operand()?;
    }
    attributes.reverse();
    let fix = (|| {
        let (name, positions) = target_path(&base)?;
        let ident = name.ident()?;
        let span = trendy_syntax::span(ident.text_range());
        let mut ty = names.assigned(ident.text(), span, errors)?.ty;
        for _ in &positions {
            let hir::Type::Array(element) = ty else {
                return None;
            };
            ty = *element;
        }
        // Each attribute's tuple, whose attributes are in scope where the
        // fix gives the attribute its value.
        let mut opened = Vec::new();
        for attribute in &attributes {
            let hir::Type::Tuple(heading) = ty else {
                return None;
            };
            ty = heading.get(attribute)?.clone();
            opened.push(heading);
        }
        let value = clause.operand()?;
        let captured = value
            .syntax()
            .descendants()
            .filter_map(ast::NameRef::cast)
            .filter_map(|name| name.ident())
            .any(|ident| opened.iter().any(|h| h.get(ident.text()).is_some()));
        if captured {
            return None;
        }
        // `a := a replace { b := x }`, inside out.
        let mut nested = value.syntax().text().to_string();
        for (i, attribute) in attributes.iter().enumerate().skip(1).rev() {
            nested = format!(
                "{} replace {{ {attribute} := {nested} }}",
                attributes[i - 1]
            );
        }
        let first = &attributes[0];
        let base = base.syntax().text().to_string();
        let replacement = if positions.is_empty() {
            format!("update {base} {{ {first} := {nested} }}")
        } else {
            format!("{base} := {base} replace {{ {first} := {nested} }}")
        };
        Some((clause.span(), replacement, positions.is_empty()))
    })();
    let attribute = attributes.last()?;
    Some(errors::attribute_assigned(written.span(), attribute, fix))
}

/// `load`, the clause `clause`, of `target`: the array of the tuples of
/// its relation, or of the values of the relation's only attribute, in the
/// order of its items. Which is up to the array's elements: tuples, or
/// other values.
fn loaded(
    clause: &Clause,
    load: &ast::Load,
    target: &Target,
    checker: &mut Checker,
    errors: &mut Vec<Diagnostic>,
) -> Option<Change> {
    let written = clause.operand()?;
    let order = load.order()?;
    let name = &target.assignee.name;
    let hir::Type::Array(element) = &target.assignee.ty else {
        let relation = checker.expr(&written, None, errors)?;
        errors.push(errors::load_into_non_array(
            name,
            &target.assignee.ty,
            target.written,
            &checker.ty(&relation),
            clause.span(),
            &written,
        ));
        return None;
    };
    let (expected, loaded) = match &**element {
        hir::Type::Tuple(heading) => (
            Some(Expected {
                ty: hir::Type::Relation(heading.clone()),
                origin: Origin::Loaded(name.clone(), Box::new(target.expected.origin.clone())),
            }),
            hir::Loaded::Tuples,
        ),
        _ => (None, hir::Loaded::Values),
    };
    let relation = checker.expr(&written, expected.as_ref(), errors)?;
    let ty = checker.ty(&relation);
    let heading = match &ty {
        hir::Type::Relation(heading)
            if loaded == hir::Loaded::Tuples
                || heading.attributes().map(|(_, ty)| ty).eq([&**element]) =>
        {
            heading
        }
        _ => {
            errors.push(errors::load_mismatch(
                name,
                element,
                &ty,
                &written,
                projected(&ty, element, &order).as_deref(),
            ));
            return None;
        }
    };
    let items = order_items(&order, heading, errors)?;
    Some(Change::Value(ops::load(relation, items, loaded), written))
}

/// The attribute of `ty`, a relation, of type `element`, that every item of
/// `order` names, if it is the only one of that type: what to project the
/// relation onto to load its values.
fn projected(ty: &hir::Type, element: &hir::Type, order: &ast::OrderList) -> Option<String> {
    let hir::Type::Relation(heading) = ty else {
        return None;
    };
    let mut of_type = heading.attributes().filter(|(_, ty)| *ty == element);
    let (name, _) = of_type.next()?;
    let only = of_type.next().is_none();
    let named = order.items().all(|item| {
        item.attribute()
            .and_then(|a| a.ident())
            .is_some_and(|i| i.text() == name)
    });
    (only && named).then(|| name.to_string())
}

/// The items of a load's `order`, each a different attribute of `heading`,
/// of a type with an order, at least one, or `None` after reporting why
/// they aren't.
fn order_items(
    order: &ast::OrderList,
    heading: &hir::Heading,
    errors: &mut Vec<Diagnostic>,
) -> Option<Vec<hir::OrderItem>> {
    let items: Vec<ast::OrderItem> = order.items().collect();
    if items.is_empty() {
        errors.push(errors::empty_order(order, heading));
        return None;
    }
    let mut ok = true;
    let mut ordered: Vec<(hir::OrderItem, Span)> = Vec::new();
    for item in &items {
        let Some(ident) = item.attribute().and_then(|a| a.ident()) else {
            ok = false;
            continue;
        };
        let name = ident.text();
        let span = trendy_syntax::span(ident.text_range());
        let Some(ty) = heading.get(name) else {
            errors.push(errors::unknown_order_attribute(name, span, heading));
            ok = false;
            continue;
        };
        if !ty.has(hir::Requirement::Order) {
            errors.push(errors::unordered_attribute(name, span, ty));
            ok = false;
            continue;
        }
        if let Some((_, first)) = ordered.iter().find(|(o, _)| o.attribute == name) {
            errors.push(errors::repeated_order_item(
                name,
                span,
                *first,
                order.removal(item),
            ));
            ok = false;
            continue;
        }
        let item = hir::OrderItem {
            attribute: name.to_string(),
            descending: item.descending(),
        };
        ordered.push((item, span));
    }
    ok.then(|| ordered.into_iter().map(|(item, _)| item).collect())
}

/// The variable an assignment assigns, where it is written, and the type
/// its value must have.
struct Target<'w> {
    assignee: Assignee,
    written: &'w ast::Expr,
    expected: Expected,
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
    match (clause.where_keyword(), &target.assignee.ty) {
        // Of the variable itself, whose value isn't known until it runs,
        // so that the attributes' values come back: the changes are
        // merged, then applied to its value (`attributes_merged`).
        (None, hir::Type::Tuple(_)) => {
            match checker.assign_attributes(
                target.assignee.target.read(),
                &assigned,
                unchanged,
                errors,
            )? {
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
                &target.assignee.ty,
                target.written,
            ));
            None
        }
        (token, ty) => {
            let hir::Type::Relation(heading) = ty else {
                // A scalar has no attributes to update.
                checker.assign_attributes(
                    target.assignee.current.clone(),
                    &assigned,
                    unchanged,
                    errors,
                )?;
                return None;
            };
            let condition = match token {
                Some(token) => {
                    checker.condition(heading.clone(), &clause.operand()?, token, errors)?
                }
                None => hir::Expr::Constant(hir::Value::Boolean(true)),
            };
            let selected = restricted(
                target.assignee.current.clone(),
                condition.clone(),
                checker.bindings(),
            );
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
    let hir::Type::Relation(heading) = &target.assignee.ty else {
        let span = trendy_syntax::span(keyword.text_range());
        errors.push(errors::not_relational(
            keyword.text(),
            span,
            &target.assignee.ty,
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

/// The new value of the variable `assignee` from its changes, `list`, in
/// source order, or `None` after reporting a change whose effect is known
/// when compiling, or a conflict between two of them.
fn merged(
    assignee: &Assignee,
    list: Vec<(&Clause, Change)>,
    names: &mut dyn Assigning,
    errors: &mut Vec<Diagnostic>,
) -> Option<hir::Expr> {
    let name = assignee.name.as_str();
    if known_effects(&assignee.current, name, &list, errors) {
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
        let written = match list[i].0 {
            Clause::Load(_) => format!("`load {name} from …`"),
            _ => format!("`{name} := …`"),
        };
        errors.push(errors::conflicting_changes(
            name,
            list[first].0.span(),
            list[second].0.span(),
            &format!("{written} gives `{name}` a whole new value"),
        ));
        return None;
    }
    let mut list = list.into_iter();
    let (clause, first) = list.next()?;
    match first {
        Change::Value(value, written) => {
            if let (
                hir::Expr::Constant(hir::Value::Relation(relation)),
                Declared::Variable(declaration),
            ) = (&value, &assignee.declaration)
            {
                let keys = keys::written(declaration, &assignee.keys);
                if !keys::satisfied(name, &keys, relation, &written, names, errors) {
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
            attributes_merged(
                assignee.current.clone(),
                name,
                changes,
                names.bindings(),
                errors,
            )
        }
        Change::Tuples(change) => {
            let mut changes = vec![(clause, change)];
            for (clause, change) in list {
                let Change::Tuples(change) = change else {
                    unreachable!("a relation variable's changes are to its tuples");
                };
                changes.push((clause, change));
            }
            tuples_merged(&assignee.current, name, changes, names.bindings(), errors)
        }
        Change::Element(change) => {
            let mut changes = vec![(clause, change)];
            for (clause, change) in list {
                let Change::Element(change) = change else {
                    unreachable!("an array variable's changes are to its elements");
                };
                changes.push((clause, change));
            }
            elements_merged(assignee.current.clone(), name, changes, errors)
        }
    }
}

/// The new value of the array variable `name`, whose value is `current`,
/// from the `changes` to its elements, if no two are known to overlap now:
/// to set the same element, or one an element in the other's. One known
/// only when running stops the program then.
fn elements_merged(
    current: hir::Expr,
    name: &str,
    changes: Vec<(&Clause, ElementAt)>,
    errors: &mut Vec<Diagnostic>,
) -> Option<hir::Expr> {
    for (i, (clause, at)) in changes.iter().enumerate() {
        let Some((first, earlier)) = changes[..i]
            .iter()
            .find(|(_, earlier)| earlier.change.overlaps(&at.change) == Some(true))
        else {
            continue;
        };
        let text = |at: &ElementAt| at.written.syntax().text().to_string();
        let why = match earlier.change.path.len().cmp(&at.change.path.len()) {
            std::cmp::Ordering::Equal => format!("both set `{}`", text(at)),
            std::cmp::Ordering::Less => format!("`{}` is in `{}`", text(at), text(earlier)),
            std::cmp::Ordering::Greater => format!("`{}` is in `{}`", text(earlier), text(at)),
        };
        errors.push(errors::conflicting_changes(
            name,
            first.span(),
            clause.span(),
            &why,
        ));
        return None;
    }
    let written = changes[0].1.written.clone();
    let changes = changes.into_iter().map(|(_, at)| at.change).collect();
    types::evaluated(
        ops::with_elements(current, changes).map_err(|failed| match failed {
            ops::Failed::Scalar(failure) => failure,
            _ => unreachable!("overlapping changes are reported above"),
        }),
        &written,
        None,
        errors,
    )
}

/// Whether a change in `list`, the changes to the variable `name`, whose
/// value before them is `current`, has an effect known when compiling, on
/// every tuple or none, after reporting each one: it is written as that
/// effect. These are checked with the variable's other changes, on which
/// their errors depend.
fn known_effects(
    current: &hir::Expr,
    name: &str,
    list: &[(&Clause, Change)],
    errors: &mut Vec<Diagnostic>,
) -> bool {
    let alone = list.len() == 1;
    let found: Vec<Diagnostic> = list
        .iter()
        .filter_map(|(clause, change)| known_effect(current, name, clause, change, alone))
        .collect();
    let any = !found.is_empty();
    errors.extend(found);
    any
}

/// The error for `clause`, `change` to the variable `name`, whose value
/// before it is `current`, if its effect is known when compiling: a condition that is, which selects
/// every tuple or none, an `i_delete` of the variable itself, which deletes
/// every tuple, or an insert of it into itself, which changes nothing.
/// `alone` says whether it is the variable's one change. With others, the
/// empty relation's assignment, which a deletion of every tuple is, would
/// conflict with them, and an insert into itself adds back the tuples they
/// remove, which changes something.
fn known_effect(
    current: &hir::Expr,
    name: &str,
    clause: &Clause,
    change: &Change,
    alone: bool,
) -> Option<Diagnostic> {
    let Change::Tuples(TupleChange { removed, added }) = change else {
        return None;
    };
    let keyword = clause.keyword()?;
    let itself = |tuples: &hir::Expr| tuples == current;
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

/// The new value of the tuple variable `name`, whose value is `current`,
/// from its `update`s, each in a clause and giving attributes new values:
/// one `replace` of them all, if no two give the same attribute one.
fn attributes_merged(
    current: hir::Expr,
    name: &str,
    changes: Vec<(&Clause, hir::Heading, BTreeMap<String, hir::Expr>)>,
    bindings: ops::Bindings,
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
        ops::extend(current, hir::Heading::new(types), values, bindings)
            .unwrap_or_else(|_| unreachable!("a variable's value is known only when running")),
    )
}

/// The new value of the relation variable `name`, whose value is `old`,
/// from the `changes` to its tuples: its old tuples less every tuple a
/// change removes, and with every tuple one adds. Two changes that remove
/// the same tuple conflict, which is reported now if it is known now, and
/// else stops the program when it runs.
fn tuples_merged(
    old: &hir::Expr,
    name: &str,
    changes: Vec<(&Clause, TupleChange)>,
    bindings: ops::Bindings,
    errors: &mut Vec<Diagnostic>,
) -> Option<hir::Expr> {
    let current = || old.clone();
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
        ([condition], true) => restricted(current(), condition.clone().negated(), bindings),
        _ => {
            // Only relations named by `i_delete`s are known now, so a
            // conflict found now is between two of those.
            if let Some((first, second, tuple)) = shared(&included) {
                errors.push(errors::conflicting_tuple(
                    name,
                    first.span(),
                    second.span(),
                    &tuple,
                ));
                return None;
            }
            let strict = !included.is_empty();
            let all = wheres
                .into_iter()
                .map(|condition| restricted(current(), condition, bindings))
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

/// The tuples of the relation `operand` for which `condition` is true.
fn restricted(operand: hir::Expr, condition: hir::Expr, bindings: ops::Bindings) -> hir::Expr {
    ops::restrict(operand, condition, bindings)
        .unwrap_or_else(|_| unreachable!("a variable's value is known only when running"))
}

/// `lhs` and `rhs`, relations, combined as `combination` says.
fn combined(lhs: hir::Expr, rhs: hir::Expr, combination: hir::Combination) -> hir::Expr {
    ops::combined(lhs, rhs, combination)
        .unwrap_or_else(|_| unreachable!("a variable's value is known only when running"))
}
