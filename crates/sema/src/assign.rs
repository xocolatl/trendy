//! Assignment statements and `assign` blocks (TTM RM Prescription 21).
//!
//! Each assignment, or shorthand for one, is a change to a variable, of the
//! program or local to the operator. The
//! changes of one `assign` block are made at once: every value is computed
//! from the variables as they were before the block, and several changes
//! to one variable are merged, if they don't conflict, into its one new
//! value. So the order of a block's assignments never matters.

use std::collections::{BTreeMap, BTreeSet};

use trendy_diag::{Diagnostic, Edit, Span};
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

/// `load`, the clause `clause`, of `target`: of a relation into an array
/// (see [`into_array`]), or of an array into a relation (see
/// [`into_relation`]), as the variable's type says.
fn loaded(
    clause: &Clause,
    load: &ast::Load,
    target: &Target,
    checker: &mut Checker,
    errors: &mut Vec<Diagnostic>,
) -> Option<Change> {
    let written = clause.operand()?;
    match &target.assignee.ty {
        hir::Type::Array(element) => into_array(load, target, element, &written, checker, errors),
        hir::Type::Relation(heading) => {
            into_relation(load, target, heading, &written, checker, errors)
        }
        ty => {
            let value = checker.expr(&written, None, errors)?;
            errors.push(errors::load_into_other(
                &target.assignee.name,
                ty,
                target.written,
                &checker.ty(&value),
                clause.span(),
                &written,
            ));
            None
        }
    }
}

/// `load`, of `written`, into `target`, an array of `element`s: the array
/// of the tuples of the relation, those `where` keeps, or, written `load a
/// value from r`, of the values of its only attribute, in the order of its
/// items, each without the attributes its `remove`, after the order,
/// names, or those its `keep` doesn't. The array's elements must be what
/// it says: tuples of the relation's heading, less those, or values of the
/// one attribute's type, which can be tuples too.
fn into_array(
    load: &ast::Load,
    target: &Target,
    element: &hir::Type,
    written: &ast::Expr,
    checker: &mut Checker,
    errors: &mut Vec<Diagnostic>,
) -> Option<Change> {
    let name = &target.assignee.name;
    if !without_other_clauses(load, name, true, errors) {
        return None;
    }
    let Some(order) = load.order() else {
        let at = load
            .condition()
            .map_or_else(|| load.source_end(), |c| Some(c.span().end))?;
        errors.push(errors::load_without_order(name, at));
        return None;
    };
    let keeping = load.keepings().next();
    if let Some(error) = ordered_after_removal(written, &order, keeping.is_some()) {
        errors.push(error);
        return None;
    }
    let (expected, loaded) = match (load.value(), element) {
        // With nothing removed, the relation's heading is the elements',
        // which an empty relation can take from them.
        (None, hir::Type::Tuple(heading)) if keeping.is_none() => (
            Some(Expected {
                ty: hir::Type::Relation(heading.clone()),
                origin: Origin::Loaded(name.clone(), Box::new(target.expected.origin.clone())),
            }),
            hir::Loaded::Tuples,
        ),
        (None, hir::Type::Tuple(_)) => (None, hir::Loaded::Tuples),
        (None, _) => {
            // Values are taken only when the load says so.
            let relation = checker.expr(written, None, errors)?;
            let ty = checker.ty(&relation);
            let keep = values_kept(&ty, element, load);
            errors.push(errors::load_without_value(
                name,
                element,
                &ty,
                written,
                load.from().as_ref(),
                keep,
            ));
            return None;
        }
        (Some(_), _) => (None, hir::Loaded::Values),
    };
    // The tuples `where` keeps, before they are ordered.
    let relation = match load
        .condition()
        .and_then(|c| Some((c.keyword()?, c.condition()?)))
    {
        Some((token, condition)) => checker.restriction(
            written,
            token,
            &condition,
            written,
            expected.as_ref(),
            errors,
        )?,
        None => checker.expr(written, expected.as_ref(), errors)?,
    };
    let ty = checker.ty(&relation);
    let hir::Type::Relation(heading) = &ty else {
        errors.push(errors::load_mismatch(name, element, &ty, written, None));
        return None;
    };
    let removed = match &keeping {
        Some(keeping) => removed(keeping, heading, &ty, errors)?,
        None => BTreeSet::new(),
    };
    let filling = Filling {
        name,
        element,
        load,
        written,
    };
    if !filling.fits(loaded, heading, &removed, errors) {
        return None;
    }
    let order = order_items(&order, heading, errors)?;
    let loading = hir::Loading {
        order,
        removed,
        loaded,
    };
    Some(Change::Value(ops::load(relation, loading), written.clone()))
}

/// `load`, of `written`, into `target`, a relation with `heading`: the
/// relation of the elements of the array, each a tuple, its own if it is
/// one, with the attribute `as x` names, if it does, whose value it is,
/// and `numbered n`, if it is there, whose value is its position, counting
/// from 1, and those that turn out equal one, and of them those `where`
/// keeps. The tuples must be of the relation's heading.
fn into_relation(
    load: &ast::Load,
    target: &Target,
    heading: &hir::Heading,
    written: &ast::Expr,
    checker: &mut Checker,
    errors: &mut Vec<Diagnostic>,
) -> Option<Change> {
    let name = &target.assignee.name;
    let array = checker.expr(written, None, errors)?;
    let ty = checker.ty(&array);
    let hir::Type::Array(element) = &ty else {
        errors.push(errors::unload_of_non_array(
            name,
            &target.assignee.ty,
            &ty,
            written,
            load,
        ));
        return None;
    };
    if !without_other_clauses(load, name, false, errors) {
        return None;
    }
    let unloading = hir::Unloading {
        named: load
            .element_name()
            .and_then(|n| n.name()?.ident())
            .map(|i| i.text().to_string()),
        numbered: load
            .numbering()
            .and_then(|n| n.name()?.ident())
            .map(|i| i.text().to_string()),
    };
    let fits = || fitting(element, heading, &unloading);
    // An element that isn't a tuple is the value of an attribute it names.
    if unloading.named.is_none() && !matches!(**element, hir::Type::Tuple(_)) {
        errors.push(errors::unnamed_elements(
            name,
            element,
            load,
            fits().as_ref(),
        ));
        return None;
    }
    // The positions' attribute is one the elements don't make.
    let elements = hir::Unloading {
        numbered: None,
        ..unloading.clone()
    }
    .heading(element);
    if let (Some(numbered), Some(numbering)) = (&unloading.numbered, load.numbering())
        && elements.get(numbered).is_some()
    {
        errors.push(errors::numbered_attribute_exists(
            numbered,
            &numbering,
            load,
            fits().as_ref(),
        ));
        return None;
    }
    let made = unloading.heading(element);
    if made != *heading {
        errors.push(errors::unloaded_mismatch(
            name,
            heading,
            &made,
            written,
            load,
            fits().as_ref(),
        ));
        return None;
    }
    let unnumbered = unloading.numbered.is_none();
    let relation = ops::unload(array, unloading);
    // The tuples `where` keeps, of those the elements make.
    let Some((token, written_condition)) = load
        .condition()
        .and_then(|c| Some((c.keyword()?, c.condition()?)))
    else {
        return Some(Change::Value(relation, written.clone()));
    };
    let condition =
        checker.unloaded_condition(made, unnumbered, &written_condition, token, errors)?;
    match ops::restrict(relation, condition, checker.bindings()) {
        Ok(relation) => Some(Change::Value(relation, written.clone())),
        Err(failure) => {
            errors.push(errors::extension_failure(&failure, &written_condition));
            None
        }
    }
}

/// Whether `load`, into `name`, an array if `array` says so and a relation
/// if not, has none of the clauses of a load into the other: `as x` and
/// `numbered n` are for a load into a relation, and `value` and an order
/// for a load into an array. Each it has is reported, with a fix that
/// removes it.
fn without_other_clauses(
    load: &ast::Load,
    name: &str,
    array: bool,
    errors: &mut Vec<Diagnostic>,
) -> bool {
    let mut other: Vec<(&str, Span, Span)> = Vec::new();
    if array {
        if let Some(named) = load.element_name() {
            other.push(("as", named.span(), named.removal()));
        }
        if let Some(numbering) = load.numbering() {
            other.push(("numbered", numbering.span(), numbering.removal()));
        }
    } else {
        if let (Some(value), Some(from)) = (load.value(), load.from()) {
            let span = trendy_syntax::span(value.text_range());
            let removal = Span::new(span.start, trendy_syntax::span(from.text_range()).start);
            other.push(("value", span, removal));
        }
        if let (Some(keyword), Some(order), Some(removal)) =
            (load.order_keyword(), load.order(), load.order_removal())
        {
            let start = trendy_syntax::span(keyword.text_range()).start;
            other.push(("order", Span::new(start, order.span().end), removal));
        }
    }
    for &(clause, span, removal) in &other {
        errors.push(errors::clause_of_other_load(
            name, array, clause, span, removal,
        ));
    }
    other.is_empty()
}

/// The clauses, `as x` and `numbered n`, that would make a load of an array
/// of `element`s give tuples of `heading`, if one way does: the one that
/// keeps a name `written` has, if there are several.
fn fitting(
    element: &hir::Type,
    heading: &hir::Heading,
    written: &hir::Unloading,
) -> Option<hir::Unloading> {
    let integer = hir::Type::Scalar(hir::Scalar::Integer);
    // The heading less the positions' attribute, if it has one, which is
    // the elements', or an attribute of the elements' type.
    let numbers = std::iter::once(None).chain(
        heading
            .named()
            .filter(|(_, ty)| **ty == integer)
            .map(|(a, _)| Some(a.to_string())),
    );
    let ways: Vec<hir::Unloading> = numbers
        .filter_map(|numbered| {
            let rest = heading.without(&numbered.iter().cloned().collect());
            let named = match element {
                hir::Type::Tuple(elements) if *elements == rest => None,
                _ => {
                    let mut attributes = rest.named();
                    let (named, ty) = attributes.next()?;
                    if attributes.next().is_some() || ty != element {
                        return None;
                    }
                    Some(named.to_string())
                }
            };
            Some(hir::Unloading { named, numbered })
        })
        .collect();
    let keeps = |way: &hir::Unloading| {
        (written.named.is_some() && way.named == written.named)
            || (written.numbered.is_some() && way.numbered == written.numbered)
    };
    if let [way] = ways.as_slice() {
        return Some(way.clone());
    }
    let mut kept = ways.iter().filter(|w| keeps(w));
    let way = kept.next()?;
    kept.next().is_none().then(|| way.clone())
}

/// The attributes of `heading`, of the relation of type `ty` a load takes,
/// that its elements leave out, as `keeping` says: those it names after
/// `remove`, or the others than those it names after `keep`. `None` after
/// reporting one that isn't the relation's, or is named twice.
fn removed(
    keeping: &ast::Keeping,
    heading: &hir::Heading,
    ty: &hir::Type,
    errors: &mut Vec<Diagnostic>,
) -> Option<BTreeSet<String>> {
    let context = errors::attributes_note("relation", ty);
    let named = ops::listed(
        &keeping.names()?,
        heading,
        errors::Noun::Projected,
        &context,
        errors,
    )?;
    if !keeping.keeps() {
        return Some(named);
    }
    Some(
        heading
            .attributes()
            .map(|(a, _)| a.to_string())
            .filter(|a| !named.contains(a))
            .collect(),
    )
}

/// The array variable `name` that `load` fills, of `element`s, from the
/// relation `written`.
struct Filling<'a> {
    name: &'a str,
    element: &'a hir::Type,
    load: &'a ast::Load,
    written: &'a ast::Expr,
}

impl Filling<'_> {
    /// Whether the elements are what the load gives them of the tuples of
    /// a relation with `heading`, `loaded`, without the attributes
    /// `removed`, after reporting why not, with a fix that keeps the
    /// attributes they need, if the tuples have them.
    fn fits(
        &self,
        loaded: hir::Loaded,
        heading: &hir::Heading,
        removed: &BTreeSet<String>,
        errors: &mut Vec<Diagnostic>,
    ) -> bool {
        let (name, element, load) = (self.name, self.element, self.load);
        let kept = heading.without(removed);
        match (loaded, element) {
            (hir::Loaded::Tuples, hir::Type::Tuple(elements)) if kept != *elements => {
                let has_them = elements
                    .attributes()
                    .all(|(a, ty)| heading.get(a) == Some(ty));
                let wanted: Vec<&str> = elements.attributes().map(|(a, _)| a).collect();
                let keep = has_them
                    .then(|| errors::keeping_edit(heading, &wanted, load))
                    .flatten();
                errors.push(errors::loaded_tuples_mismatch(
                    name,
                    elements,
                    &kept,
                    self.written,
                    keep,
                ));
                false
            }
            (hir::Loaded::Values, element)
                if !kept.attributes().map(|(_, ty)| ty).eq([element]) =>
            {
                // Its tuples are the elements, as they are.
                if let (hir::Type::Tuple(elements), Some(value), Some(from)) =
                    (element, load.value(), load.from())
                    && kept == *elements
                {
                    errors.push(errors::needless_value(name, &value, &from));
                    return false;
                }
                let ty = hir::Type::Relation(heading.clone());
                let keep = values_kept(&ty, element, load);
                let given = hir::Type::Relation(kept);
                errors.push(errors::load_mismatch(
                    name,
                    element,
                    &given,
                    self.written,
                    keep,
                ));
                false
            }
            _ => true,
        }
    }
}

/// The edit that makes `load`, of a relation of type `ty`, keep only its
/// attribute of type `element`, as a load of values from it needs, if it
/// has one, and only one.
fn values_kept(ty: &hir::Type, element: &hir::Type, load: &ast::Load) -> Option<(String, Edit)> {
    let hir::Type::Relation(heading) = ty else {
        return None;
    };
    let mut of_type = heading.attributes().filter(|(_, ty)| *ty == element);
    let (name, _) = of_type.next()?;
    if of_type.next().is_some() {
        return None;
    }
    errors::keeping_edit(heading, &[name], load)
}

/// The error for a load of `written`, a relation `r remove {…}`, whose
/// `order` names an attribute that the `remove` leaves out, which the
/// order then can't see; `remove` after the order, unless the load has a
/// list of its elements' attributes there already, `keeping`, leaves it
/// out of the elements after ordering by it.
fn ordered_after_removal(
    written: &ast::Expr,
    order: &ast::OrderList,
    keeping: bool,
) -> Option<Diagnostic> {
    let ast::Expr::Postfix(postfix) = written else {
        return None;
    };
    if postfix.operator()?.kind() != SyntaxKind::RemoveKw {
        return None;
    }
    let list = postfix.names()?;
    let removed: Vec<String> = list
        .names()
        .filter_map(|n| Some(n.ident()?.text().to_string()))
        .collect();
    let (attribute, span) = order.items().find_map(|item| {
        let ident = item.attribute()?.ident()?;
        removed.iter().any(|r| r == ident.text()).then(|| {
            (
                ident.text().to_string(),
                trendy_syntax::span(ident.text_range()),
            )
        })
    })?;
    Some(errors::ordered_after_removal(
        &attribute, span, postfix, &list, order, keeping,
    ))
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
