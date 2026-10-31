//! The program's globals, evaluated when compiling: the variables' initial
//! values, and the read-only operators' values, which initial values can
//! call. Each is evaluated, or checked, when first needed, after what it
//! refers to, so one referred to while it is itself being evaluated closes a
//! cycle: of initial values, through operators or not, or of read-only
//! operators that call each other.

use std::collections::{BTreeMap, HashMap, HashSet};

use trendy_diag::{Diagnostic, Span};
use trendy_hir as hir;
use trendy_syntax::ast::{self, AstNode, SourceFile, VarDecl};

use crate::assign::{self, Assignee, Assigning};
use crate::check::{Checker, Lookup, Names};
use crate::locals::{self, Declaration, Local, Locals};
use crate::operators::{FromType, Made, Operators, Signature};
use crate::types::{self, Expected, Origin};
use crate::{Namespaces, Prelude, Sites, errors, keys, ops};

/// What the globals evaluate to.
pub(crate) struct Evaluated {
    /// Each variable, by id: `None` for one with errors, already reported.
    pub(crate) variables: Vec<Option<hir::Variable>>,
    /// Each valid read-only operator's value.
    pub(crate) values: HashMap<hir::OperatorId, hir::Expr>,
    /// Each valid read-only operator's local variables, which its value's
    /// `Let`s give values.
    pub(crate) locals: HashMap<hir::OperatorId, Vec<hir::Local>>,
}

/// Evaluates the globals, each when first needed.
pub(crate) struct Globals<'a> {
    /// Where each place in the file is.
    sites: &'a Sites<'a>,
    namespaces: &'a Namespaces,
    operators: &'a Operators,
    /// The first declaration of each variable, by name.
    decls: &'a HashMap<String, VarDecl>,
    /// Each variable's id, by name.
    ids: &'a HashMap<String, hir::VariableId>,
    /// Each variable, by id, once its evaluation has started.
    variables: Vec<Option<State<hir::Variable>>>,
    /// Each read-only operator whose value's checking has started.
    operator_states: HashMap<hir::OperatorId, State<()>>,
    /// Each valid read-only operator's value.
    values: HashMap<hir::OperatorId, hir::Expr>,
    /// Each valid read-only operator's local variables.
    locals: HashMap<hir::OperatorId, Vec<hir::Local>>,
    /// The globals being evaluated, innermost last.
    stack: Vec<Pending>,
    /// Each read-only operator, and each variable of the program's it reads,
    /// already reported.
    read: HashSet<(hir::OperatorId, String)>,
    /// Each cycle already reported, by the globals in it, from the first
    /// reached.
    cycles: HashSet<Vec<Global>>,
    /// The local variables of value blocks in initial values, by id. Most
    /// have values known when compiling, which their names stand for; those
    /// computed for each tuple of a relation, as in an `extend`'s values,
    /// are substituted when each tuple is.
    block_locals: Vec<hir::Local>,
}

/// What a global being evaluated is.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Global {
    /// A variable, whose initial value refers to another.
    Variable(hir::VariableId),
    /// A read-only operator, which a call checks.
    Operator(hir::OperatorId),
    /// A constraint of a type of the program's own, which selecting one of
    /// its values checks.
    Constraint(hir::OperatorId),
}

/// Where a global's evaluation is.
#[derive(Clone)]
enum State<T> {
    /// Under way: a reference to it now closes a cycle.
    Evaluating,
    /// Done: `None` after reporting errors.
    Done(Option<T>),
}

/// A global being evaluated, in a chain of references from the outermost
/// to the innermost.
#[derive(Clone)]
pub(crate) struct Pending {
    pub(crate) global: Global,
    /// Its name, as errors write it: a constraint's type's.
    pub(crate) name: String,
    /// Where its name is defined.
    pub(crate) defined: Span,
    /// Where it refers to the next one, once it does.
    pub(crate) via: Span,
}

impl<'a> Globals<'a> {
    /// The globals of a file, with `prelude`'s read-only operators, which
    /// are checked already.
    pub(crate) fn new(
        prelude: &Prelude,
        namespaces: &'a Namespaces,
        operators: &'a Operators,
        decls: &'a HashMap<String, VarDecl>,
        ids: &'a HashMap<String, hir::VariableId>,
        sites: &'a Sites<'a>,
    ) -> Self {
        let prelude_values = prelude
            .operators()
            .iter()
            .enumerate()
            .filter_map(|(i, op)| match &op.kind {
                hir::OperatorKind::ReadOnly { value, .. } => {
                    Some((hir::OperatorId(i), value.clone()))
                }
                _ => None,
            });
        let values: HashMap<hir::OperatorId, hir::Expr> = prelude_values.collect();
        Globals {
            sites,
            namespaces,
            operators,
            decls,
            ids,
            variables: vec![None; ids.len()],
            operator_states: values
                .keys()
                .map(|&id| (id, State::Done(Some(()))))
                .collect(),
            values,
            locals: HashMap::new(),
            stack: Vec::new(),
            read: HashSet::new(),
            cycles: HashSet::new(),
            block_locals: Vec::new(),
        }
    }

    /// Every variable's initial value, and every read-only operator's
    /// value, whether or not an initial value calls it.
    pub(crate) fn evaluate(mut self, file: &SourceFile, errors: &mut Vec<Diagnostic>) -> Evaluated {
        for decl in file.variables() {
            let Some(ident) = decl.name().and_then(|n| n.ident()) else {
                continue;
            };
            if self.namespaces.is_first_variable(&ident) {
                self.variable(ident.text(), errors);
            } else {
                // A duplicate, already reported, but it can have errors of
                // its own.
                variable(&decl, &mut self, errors);
            }
        }
        let operators = self.operators;
        for signature in operators.iter().filter(|s| s.read_only) {
            if let Some(defined) = signature.defined() {
                self.checked(signature.id, defined, errors);
            }
        }
        Evaluated {
            variables: self
                .variables
                .into_iter()
                .map(|state| match state {
                    Some(State::Done(variable)) => variable,
                    _ => None,
                })
                .collect(),
            values: self.values,
            locals: self.locals,
        }
    }

    /// The first declared variable named `name`, evaluated if it isn't yet.
    fn variable(&mut self, name: &str, errors: &mut Vec<Diagnostic>) -> Option<hir::Variable> {
        let id = *self.ids.get(name)?;
        match &self.variables[id.0] {
            Some(State::Done(variable)) => return variable.clone(),
            Some(State::Evaluating) => unreachable!("cycles are caught at the reference"),
            None => {}
        }
        let decl = self.decls.get(name)?.clone();
        let declared = trendy_syntax::span(decl.name()?.ident()?.text_range());
        self.variables[id.0] = Some(State::Evaluating);
        self.stack.push(Pending {
            global: Global::Variable(id),
            name: name.to_string(),
            defined: declared,
            via: declared,
        });
        let variable = variable(&decl, self, errors);
        self.stack.pop();
        self.variables[id.0] = Some(State::Done(variable.clone()));
        variable
    }

    /// The variable `name`, referred to at `span`, evaluated if it isn't
    /// yet, or `None` after reporting why it has none, such as a cycle its
    /// reference closes.
    fn reach(
        &mut self,
        name: &str,
        span: Span,
        errors: &mut Vec<Diagnostic>,
    ) -> Option<hir::Variable> {
        let id = *self.ids.get(name)?;
        if let Some(State::Evaluating) = self.variables[id.0] {
            let start = self
                .stack
                .iter()
                .position(|p| p.global == Global::Variable(id))?;
            self.cycle(start, span, errors);
            return None;
        }
        if let Some(innermost) = self.stack.last_mut() {
            innermost.via = span;
        }
        self.variable(name, errors)
    }

    /// The type of the variable `name`, as its declaration writes it, or
    /// the named heading of the selector its initial value is, as in
    /// `relation home { … }`, or else as its value has it, if that is
    /// known: its initial value isn't evaluated here, as that could lose
    /// its errors.
    fn written_type(&self, name: &str) -> Option<String> {
        let decl = &self.decls[name];
        if let Some(written) = decl.type_expr() {
            return Some(written.syntax().text().to_string());
        }
        let named = match decl.initializer() {
            Some(ast::Expr::Relation(r)) => r.heading().map(|h| ("relation", h)),
            Some(ast::Expr::Tuple(t)) => t.heading().map(|h| ("tuple", h)),
            _ => None,
        };
        if let Some((kind, ast::HeadingExpr::Named(heading))) = named
            && let Some(ident) = heading.ident()
        {
            return Some(format!("{kind} {}", ident.text()));
        }
        match &self.variables[self.ids[name].0] {
            Some(State::Done(Some(variable))) => Some(variable.ty.to_string()),
            _ => None,
        }
    }

    /// Whether the read-only operator `id`, called at `at`, has a valid
    /// value, its body checked if it isn't yet. Calling one whose body is
    /// being checked closes a cycle: recursion, or a cycle with the initial
    /// values that call it.
    fn checked(&mut self, id: hir::OperatorId, at: Span, errors: &mut Vec<Diagnostic>) -> bool {
        let operators = self.operators;
        let signature = operators.get(id);
        // A call of a specification computes one of its versions, each
        // checked, or its own value, if it has a body.
        if !signature.versions.is_empty() {
            let mut valid = signature.valid;
            for version in &signature.versions {
                valid &= self.checked(version.operator, at, errors);
            }
            let bodiless = signature.decl.as_ref().is_some_and(|d| d.body().is_none());
            return valid && (bodiless || self.checked_own(id, at, errors));
        }
        self.checked_own(id, at, errors)
    }

    /// Whether the read-only operator `id`, called at `at`, has a valid
    /// value of its own, as [`Globals::checked`] says.
    fn checked_own(&mut self, id: hir::OperatorId, at: Span, errors: &mut Vec<Diagnostic>) -> bool {
        let operators = self.operators;
        let signature = operators.get(id);
        // The stored representation's selector's value is the value its
        // arguments are the components of, with no body to check, but its
        // type's constraints, and the conversions computing its keyed
        // components, which it calls: a call of it is one of each of them,
        // so a constraint whose condition selects a value of its type
        // closes a cycle.
        if let Some(defined) = signature.selects().filter(|_| {
            signature
                .from_type
                .as_ref()
                .is_some_and(FromType::selects_stored)
        }) {
            let valid = self
                .namespaces
                .selection_checks(defined)
                .into_iter()
                .all(|operator| self.checked(operator, at, errors));
            if valid {
                self.values
                    .entry(id)
                    .or_insert_with(|| Signature::selection(defined));
                self.locals.entry(id).or_default();
            }
            return valid;
        }
        match self.operator_states.get(&id) {
            Some(State::Done(done)) => return done.is_some(),
            Some(State::Evaluating) => {
                if let Some(start) = self.stack.iter().position(
                    |p| matches!(p.global, Global::Operator(o) | Global::Constraint(o) if o == id),
                ) {
                    self.cycle(start, at, errors);
                }
                return false;
            }
            None => {}
        }
        if let Some(innermost) = self.stack.last_mut() {
            innermost.via = at;
        }
        let defined = signature.defined().unwrap_or(at);
        self.operator_states.insert(id, State::Evaluating);
        let (global, name) = match signature.constrains() {
            Some((ty, _)) => (Global::Constraint(id), ty.name.clone()),
            None => (Global::Operator(id), signature.name.clone()),
        };
        self.stack.push(Pending {
            global,
            name,
            defined,
            via: defined,
        });
        let value = self.value(signature, errors);
        self.stack.pop();
        let valid = value.is_some();
        if let Some((locals, value)) = value {
            self.values.insert(id, value);
            self.locals.insert(id, locals);
        }
        self.operator_states
            .insert(id, State::Done(valid.then_some(())));
        valid
    }

    /// Report the cycle closed by a reference at `at` back to the global
    /// at `start` in the stack, unless another reference closed it already,
    /// as a second call of an operator in it does. A cycle through a type's
    /// constraint is reported from the constraint, wherever it was entered.
    fn cycle(&mut self, start: usize, at: Span, errors: &mut Vec<Diagnostic>) {
        let mut cycle = self.stack[start..].to_vec();
        if let Some(last) = cycle.last_mut() {
            last.via = at;
        }
        if let Some(constraint) = cycle
            .iter()
            .position(|p| matches!(p.global, Global::Constraint(_)))
        {
            cycle.rotate_left(constraint);
        }
        if self.cycles.insert(cycle.iter().map(|p| p.global).collect()) {
            errors.push(errors::cycle(&cycle));
        }
    }

    /// The value of the read-only operator `signature`, and its local
    /// variables: its body declares and assigns those, and ends with its
    /// value, of its result type; it changes nothing else (TTM RM
    /// Prescription 3). Each declaration and assignment is a `Let`
    /// of a local variable of its own, which has the value from there on,
    /// so that the value is one expression, which computes each local
    /// variable's value once. `None` after reporting why it has none.
    fn value(
        &mut self,
        signature: &Signature,
        errors: &mut Vec<Diagnostic>,
    ) -> Option<(Vec<hir::Local>, hir::Expr)> {
        if let Some((defined, constraint)) = signature.constrains() {
            return self.constraint(signature, defined, constraint, errors);
        }
        if let Some(from) = &signature.from_type {
            return self.converted(signature, from, errors);
        }
        let decl = signature.decl.as_ref()?;
        let written = decl.result()?;
        let body = decl.body()?;
        let (mut locals, mut valid) = locals::declarations(&body, signature, errors);
        valid &= signature.valid;
        let mut lets: Vec<(hir::LocalId, hir::Expr)> = Vec::new();
        let statements: Vec<ast::Statement> = body.statements().collect();
        let written_value = body.value();
        for (i, statement) in statements.iter().enumerate() {
            // Where each value it gives a local variable is computed.
            let site = self
                .sites
                .at(trendy_syntax::span(statement.syntax().text_range()));
            let mut names = ReadOnlyNames {
                globals: self,
                within: signature.id,
                locals: &mut locals,
            };
            match statement {
                ast::Statement::Return(stmt) => {
                    // `return;` at the end, with no value, is the body
                    // without one, reported below; the parser makes
                    // `return x;` there the value.
                    let last = i + 1 == statements.len() && written_value.is_none();
                    if !last {
                        errors.push(errors::early_return(stmt));
                    }
                    valid = false;
                }
                ast::Statement::Var(decl) => {
                    let checked = locals::local(decl, &signature.types, &mut names, errors);
                    let (local, value) = checked.unzip();
                    match (locals.declare(decl, local), value) {
                        (Some(id), Some(value)) => lets.push((id, hir::Expr::at(site, value))),
                        _ => valid = false,
                    }
                }
                ast::Statement::Assign(stmt) => {
                    let clauses: Vec<_> = stmt.clauses().collect();
                    let assigned = assign::statement(&clauses, &mut names, errors);
                    valid &= reassigned(assigned, site, &mut locals, &mut lets);
                }
                ast::Statement::Block(block) => {
                    let assigned = assign::block(block, &mut names, errors);
                    valid &= reassigned(assigned, site, &mut locals, &mut lets);
                }
                ast::Statement::Call(stmt) => {
                    errors.push(errors::call_in_read_only(&signature.name, stmt));
                    valid = false;
                }
                ast::Statement::If(stmt) => {
                    errors.push(errors::if_in_read_only(&signature.name, stmt));
                    valid = false;
                }
                ast::Statement::While(_) | ast::Statement::Loop(_) | ast::Statement::For(_) => {
                    errors.push(errors::loop_in_read_only(&signature.name, statement));
                    valid = false;
                }
            }
        }
        let Some(written_value) = written_value else {
            errors.push(errors::missing_value(&signature.name, &body, &written));
            return None;
        };
        let mut names = ReadOnlyNames {
            globals: self,
            within: signature.id,
            locals: &mut locals,
        };
        let expected = signature.result.clone().map(|ty| Expected {
            ty,
            origin: Origin::Type(written.clone()),
        });
        let site = names.site(written_value.span());
        let value = hir::Expr::at(
            site,
            Checker::new(&mut names).expr(&written_value, expected.as_ref(), errors)?,
        );
        valid.then(|| {
            let value = lets.into_iter().rev().fold(value, |body, (id, value)| {
                hir::Expr::Let(id, Box::new(value), Box::new(body))
            });
            (locals.finish(), value)
        })
    }

    /// The value of `signature`, made from a type's possible representation
    /// other than the stored one, as `from` says, with no local variables:
    /// of its selector, the value with the stored components its conversion
    /// to the stored one computes from its own, the parameters, once the
    /// type's constraints are checked; of that conversion, the tuple of the
    /// stored components its `to` computes from its own; and of its
    /// conversion from the stored one, the tuple of its own its `from`
    /// computes from the stored ones. `None` if the representation's
    /// conversions have errors, already reported, or after reporting those
    /// in its `to` or `from`.
    fn converted(
        &mut self,
        signature: &Signature,
        from: &FromType,
        errors: &mut Vec<Diagnostic>,
    ) -> Option<(Vec<hir::Local>, hir::Expr)> {
        if !signature.valid {
            return None;
        }
        let defined = &from.defined;
        let (representation, direction) = match &from.made {
            Made::Selector(_) => return self.selection(signature, defined, errors),
            Made::To(r) => (r, ast::Direction::To),
            Made::From(r) => (r, ast::Direction::From),
            Made::Constraint(_) => unreachable!("a constraint is checked on its own"),
        };
        let conversion = representation
            .conversions()
            .find(|c| c.direction() == Some(direction))?;
        let value = match direction {
            ast::Direction::To => {
                let own = representation.components()?;
                self.conversion(signature, &conversion, &defined.components, &own, errors)?
            }
            ast::Direction::From => {
                let stored = crate::named_types::stored(&from.decl)?.components()?;
                let Some(hir::Type::Tuple(components)) = &signature.result else {
                    unreachable!("a conversion from the stored representation is of a tuple");
                };
                self.conversion(signature, &conversion, components, &stored, errors)?
            }
        };
        Some((Vec::new(), value))
    }

    /// The value of `signature`, the selector of a possible representation of
    /// the type `defined` other than the stored one: the value with the
    /// stored components the representation's conversion to the stored one
    /// computes from its own, the parameters, once that conversion and the
    /// type's constraints are checked. `None` if any has errors, reported
    /// when it is checked.
    fn selection(
        &mut self,
        signature: &Signature,
        defined: &hir::Defined,
        errors: &mut Vec<Diagnostic>,
    ) -> Option<(Vec<hir::Local>, hir::Expr)> {
        let component = signature.params.keys().next()?;
        let (_, other) = defined.representation(component)?;
        let to = other
            .expect("only another representation's selector converts")
            .to;
        let at = signature.defined()?;
        let converts = self.checked(to, at, errors);
        let constrained = self
            .namespaces
            .selection_checks(defined)
            .into_iter()
            .all(|operator| self.checked(operator, at, errors));
        if !converts || !constrained {
            return None;
        }
        let args = signature
            .params
            .keys()
            .map(|c| (c.clone(), hir::Expr::Parameter(c.clone())))
            .collect();
        let components = hir::Expr::Call(hir::Call {
            operator: to,
            types: Vec::new(),
            args,
            updated: BTreeMap::new(),
        });
        Some((
            Vec::new(),
            hir::Expr::Select(Box::new(defined.clone()), Box::new(components)),
        ))
    }

    /// The tuple of `components`, written in `written`, that `conversion`
    /// computes, in `signature`, whose parameters are the components it
    /// computes them from: each once, of its type, computed as a read-only
    /// operator's value is. `None` after reporting why not.
    fn conversion(
        &mut self,
        signature: &Signature,
        conversion: &ast::Conversion,
        components: &hir::Heading,
        written: &ast::Heading,
        errors: &mut Vec<Diagnostic>,
    ) -> Option<hir::Expr> {
        let list = conversion.assignments()?;
        let keyword = conversion
            .keyword()
            .map_or_else(String::new, |k| k.text().to_string());
        let context = errors::conversion_note(&keyword, components);
        let mut locals = Locals::default();
        let mut names = ReadOnlyNames {
            globals: self,
            within: signature.id,
            locals: &mut locals,
        };
        let mut ok = true;
        let mut values: BTreeMap<String, hir::Expr> = BTreeMap::new();
        let mut first: BTreeMap<String, Span> = BTreeMap::new();
        // The components unknown names are fixed to, which aren't missing
        // then.
        let mut renamed: Vec<&str> = Vec::new();
        for assign in list.assignments() {
            let Some(ident) = assign.name().and_then(|n| n.ident()) else {
                ok = false;
                continue;
            };
            let (name, span) = (ident.text(), trendy_syntax::span(ident.text_range()));
            let Some(ty) = components.get(name) else {
                let rename = trendy_diag::closest(name, components.named().map(|(c, _)| c));
                renamed.extend(rename);
                errors.push(errors::unknown_attribute(
                    errors::Noun::Component,
                    name,
                    span,
                    &context,
                    rename,
                    None,
                ));
                ok = false;
                continue;
            };
            if let Some(&earlier) = first.get(name) {
                errors.push(errors::duplicate_attribute(
                    errors::Noun::Component,
                    name,
                    span,
                    earlier,
                ));
                ok = false;
                continue;
            }
            first.insert(name.to_string(), span);
            let origin = written
                .attributes()
                .find(|a| a.is_named(name))
                .and_then(|a| a.type_expr())
                .map_or(Origin::Builtin, Origin::Type);
            let expected = Expected {
                ty: ty.clone(),
                origin,
            };
            match assign
                .value()
                .and_then(|v| Checker::new(&mut names).expr(&v, Some(&expected), errors))
            {
                Some(value) => {
                    values.insert(name.to_string(), value);
                }
                None => ok = false,
            }
        }
        let missing: Vec<&str> = components
            .named()
            .map(|(c, _)| c)
            .filter(|c| !first.contains_key(*c) && !renamed.contains(c))
            .collect();
        if !missing.is_empty() {
            errors.push(errors::missing_attributes(
                errors::Noun::Component,
                &missing,
                &context,
                list.span(),
            ));
            ok = false;
        }
        ok.then(|| ops::tuple(values))
    }

    /// The value of `signature`, an operator that computes `constraint`, of
    /// the type `defined`: its condition, a boolean of the components, its
    /// parameters, checked as a read-only operator's value is, so that it
    /// depends on them alone. `None` after reporting why it has none, or
    /// that it is known when compiling, true or false whatever the
    /// components.
    fn constraint(
        &mut self,
        signature: &Signature,
        defined: &hir::Defined,
        constraint: &ast::Constraint,
        errors: &mut Vec<Diagnostic>,
    ) -> Option<(Vec<hir::Local>, hir::Expr)> {
        let condition = constraint.condition()?;
        let boolean = Expected {
            ty: hir::Type::Scalar(hir::Scalar::Boolean),
            origin: Origin::Operator(constraint.keyword()?),
        };
        let mut locals = Locals::default();
        let mut names = ReadOnlyNames {
            globals: self,
            within: signature.id,
            locals: &mut locals,
        };
        let site = names.site(condition.span());
        let value = Checker::new(&mut names).expr(&condition, Some(&boolean), errors)?;
        if let hir::Expr::Constant(hir::Value::Boolean(holds)) = value {
            errors.push(errors::constant_constraint(holds, constraint, defined));
            return None;
        }
        Some((locals.finish(), hir::Expr::at(site, value)))
    }
}

/// Whether `assigned`, an assignment statement in a read-only operator, of
/// its local variables, is valid, each local variable it assigns then given
/// a new one, which has the value from there on, in `lets`, computed at
/// `site`, the statement's. The values are computed from the local
/// variables before any is assigned, so the statement's assignments are
/// still made at once.
fn reassigned(
    assigned: Option<hir::Statement>,
    site: hir::Site,
    locals: &mut Locals,
    lets: &mut Vec<(hir::LocalId, hir::Expr)>,
) -> bool {
    let Some(hir::Statement::Assign(assignments)) = assigned else {
        return false;
    };
    for assignment in assignments {
        let hir::Target::Local(id) = assignment.target else {
            unreachable!("a read-only operator assigns only its local variables");
        };
        lets.push((locals.reassigned(id), hir::Expr::at(site, assignment.value)));
    }
    true
}

impl Names for Globals<'_> {
    fn namespaces(&self) -> &Namespaces {
        self.namespaces
    }

    fn site(&self, at: Span) -> hir::Site {
        self.sites.at(at)
    }

    fn operators(&self) -> &Operators {
        self.operators
    }

    fn resolve(&mut self, name: &str, span: Span, errors: &mut Vec<Diagnostic>) -> Lookup {
        if self.ids.contains_key(name) {
            return match self.reach(name, span, errors) {
                // Of its declared type, which can be a supertype of its
                // value's.
                Some(variable) => {
                    Lookup::Found(hir::Expr::Constant(variable.init).treated(&variable.ty))
                }
                None => Lookup::Reported,
            };
        }
        if self.namespaces.operator(name).is_some() {
            errors.push(errors::operator_as_value(name, span, self.operators));
            return Lookup::Reported;
        }
        Lookup::Unknown
    }

    fn candidates(&self) -> Vec<String> {
        self.decls.keys().cloned().collect()
    }

    fn leaf_type(&self, leaf: &hir::Expr) -> hir::Type {
        match leaf {
            hir::Expr::Local(id) => self.block_locals[id.0].ty.clone(),
            _ => unreachable!("an initial value's other names are constants, not {leaf:?}"),
        }
    }

    fn fresh_local(&mut self, local: hir::Local) -> hir::LocalId {
        let id = hir::LocalId(self.block_locals.len());
        self.block_locals.push(local);
        id
    }

    fn hides(&self, _: &str) -> bool {
        false
    }

    fn checked(&mut self, id: hir::OperatorId, at: Span, errors: &mut Vec<Diagnostic>) -> bool {
        Globals::checked(self, id, at, errors)
    }

    fn bindings(&self) -> ops::Bindings<'_> {
        ops::Bindings {
            operators: self.operators,
            namespaces: self.namespaces,
            values: &self.values,
        }
    }

    fn narrowing_ended(&self, _: &str) -> Option<Span> {
        None
    }
}

/// Names in the body of the read-only operator `within`: its parameters and
/// its local variables, which hide variables of the same name, and the
/// variables, which it reads when it is called.
struct ReadOnlyNames<'g, 'a, 'l> {
    globals: &'g mut Globals<'a>,
    within: hir::OperatorId,
    locals: &'l mut Locals,
}

impl ReadOnlyNames<'_, '_, '_> {
    fn signature(&self) -> &Signature {
        self.globals.operators.get(self.within)
    }
}

impl Names for ReadOnlyNames<'_, '_, '_> {
    fn namespaces(&self) -> &Namespaces {
        self.globals.namespaces
    }

    fn site(&self, at: Span) -> hir::Site {
        self.globals.sites.at(at)
    }

    fn operators(&self) -> &Operators {
        self.globals.operators
    }

    fn resolve(&mut self, name: &str, span: Span, errors: &mut Vec<Diagnostic>) -> Lookup {
        if let Some(lookup) = self.signature().parameter(name) {
            return lookup;
        }
        match self.locals.find(name) {
            Some(Local::Declared(id, ..)) => return Lookup::Found(hir::Expr::Local(id)),
            Some(Local::Later(decl)) => {
                errors.push(errors::local_before_declaration(
                    name,
                    span,
                    decl,
                    self.globals.ids.contains_key(name),
                ));
                return Lookup::Reported;
            }
            Some(Local::Ended(decl)) => {
                errors.push(errors::local_out_of_scope(name, span, decl));
                return Lookup::Reported;
            }
            Some(Local::Invalid) => return Lookup::Reported,
            None => {}
        }
        // In a constraint, a component of a possible representation other
        // than the stored one, which its conversion computes from the
        // stored components, the parameters.
        let conversion = self.signature().constrains().and_then(|(defined, _)| {
            let (_, other) = defined.representation(name)?;
            Some((other?.from, defined.components.clone()))
        });
        if let Some((conversion, stored)) = conversion {
            if !self.globals.checked(conversion, span, errors) {
                return Lookup::Reported;
            }
            let args = stored
                .attributes()
                .map(|(k, _)| (k.to_string(), hir::Expr::Parameter(k.to_string())))
                .collect();
            let call = hir::Expr::Call(hir::Call {
                operator: conversion,
                types: Vec::new(),
                args,
                updated: BTreeMap::new(),
            });
            return Lookup::Found(hir::Expr::Extract(Box::new(call), name.to_string()));
        }
        // A read-only operator's value depends on its arguments alone, so
        // a variable of the program's is reported, once, with a fix that
        // passes it as a parameter, of its type as written, if it is.
        if self.globals.ids.contains_key(name) {
            if self.globals.read.insert((self.within, name.to_string())) {
                let ty = self.globals.written_type(name);
                errors.push(errors::variable_in_read_only(
                    name,
                    span,
                    self.signature(),
                    self.globals.operators,
                    ty,
                ));
            }
            return Lookup::Reported;
        }
        if let Some(relation) = self.globals.operators.get(self.within).per_relation(name) {
            errors.push(errors::per_as_value(name, relation, span));
            return Lookup::Reported;
        }
        if self.globals.namespaces.operator(name).is_some() {
            errors.push(errors::operator_as_value(
                name,
                span,
                self.globals.operators,
            ));
            return Lookup::Reported;
        }
        Lookup::Unknown
    }

    fn candidates(&self) -> Vec<String> {
        self.signature()
            .parameter_names()
            .chain(self.locals.names())
            .chain(self.globals.ids.keys().map(String::as_str))
            .map(str::to_string)
            .collect()
    }

    fn leaf_type(&self, leaf: &hir::Expr) -> hir::Type {
        match leaf {
            hir::Expr::Parameter(name) => self.signature().params[name].ty.clone(),
            hir::Expr::Local(id) => self.locals.get(*id).ty.clone(),
            hir::Expr::Variable(id) => match &self.globals.variables[id.0] {
                Some(State::Done(Some(variable))) => variable.ty.clone(),
                _ => unreachable!("a variable is evaluated when its name is resolved"),
            },
            _ => unreachable!("only variables, parameters and local variables are leaves"),
        }
    }

    fn fresh_local(&mut self, local: hir::Local) -> hir::LocalId {
        self.locals.fresh(local)
    }

    fn hides(&self, name: &str) -> bool {
        self.signature().is_parameter(name)
            || matches!(
                self.locals.find(name),
                Some(Local::Declared(..) | Local::Later(_) | Local::Invalid)
            )
    }

    fn type_parameters(&self) -> Option<&[hir::TypeParameter]> {
        Some(&self.globals.operators.get(self.within).types)
    }

    fn checked(&mut self, id: hir::OperatorId, at: Span, errors: &mut Vec<Diagnostic>) -> bool {
        self.globals.checked(id, at, errors)
    }

    fn bindings(&self) -> ops::Bindings<'_> {
        ops::Bindings {
            operators: self.globals.operators,
            namespaces: self.globals.namespaces,
            values: &self.globals.values,
        }
    }

    fn narrowing_ended(&self, _: &str) -> Option<Span> {
        None
    }
}

impl Assigning for ReadOnlyNames<'_, '_, '_> {
    fn assigned(
        &mut self,
        name: &str,
        span: Span,
        errors: &mut Vec<Diagnostic>,
    ) -> Option<Assignee> {
        let op = self.globals.operators.get(self.within);
        if op.is_parameter(name) {
            let hidden = self.globals.ids.contains_key(name);
            errors.push(errors::assigned_parameter(name, span, hidden));
            return None;
        }
        match self.locals.find(name) {
            Some(Local::Declared(id, local, Declaration::Var(declaration))) => {
                return Some(Assignee::local(id, local, declaration));
            }
            // A loop's variable is in no read-only operator, which has no
            // loops.
            Some(Local::Declared(_, _, declaration @ Declaration::Loop(_))) => {
                errors.push(errors::assigned_loop_variable(
                    name,
                    span,
                    declaration.name_span(),
                ));
                return None;
            }
            Some(Local::Later(decl)) => {
                errors.push(errors::local_before_declaration(
                    name,
                    span,
                    decl,
                    self.globals.ids.contains_key(name),
                ));
                return None;
            }
            // Its declaration's errors are already reported.
            Some(Local::Ended(decl)) => {
                errors.push(errors::local_out_of_scope(name, span, decl));
                return None;
            }
            Some(Local::Invalid) => return None,
            None => {}
        }
        if self.globals.ids.contains_key(name) {
            errors.push(errors::assigned_in_read_only(name, span, &op.name));
        } else if let Some((kind, _)) = self.globals.namespaces.operator(name) {
            errors.push(errors::assigned_operator(name, span, kind));
        } else {
            let variables: Vec<&str> = self.locals.names().collect();
            errors.push(errors::unknown_name(name, span, "variable", &variables));
        }
        None
    }
}

/// Check a variable declaration: at least one of its type and initial value
/// must be given, and they must agree.
fn variable(
    decl: &VarDecl,
    globals: &mut Globals,
    errors: &mut Vec<Diagnostic>,
) -> Option<hir::Variable> {
    let namespaces = globals.namespaces;
    let ident = decl.name()?.ident()?;
    let declared = decl.type_expr().map(|t| {
        let ty = types::resolve(&t, &mut types::TypeParameters::None, namespaces, errors);
        (ty, t)
    });

    let (ty, init) = match (declared, decl.initializer()) {
        (None, None) => {
            errors.push(errors::untyped_variable(
                ident.text(),
                trendy_syntax::span(ident.text_range()),
            ));
            return None;
        }
        (Some((ty, type_expr)), None) => {
            let ty = ty?;
            let Some(init) = ty.default_value() else {
                errors.push(errors::no_default_value(ident.text(), &ty, &type_expr));
                return None;
            };
            (ty, init)
        }
        // Its type is its value's declared type, which can be a supertype
        // of the value's own.
        (None, Some(expr)) => {
            let (value, ty) = types::value(&expr, None, globals, errors)?;
            (ty, value)
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
            let (value, _) = types::value(&expr, Some(&expected), globals, errors)?;
            (ty, value)
        }
    };
    let keys = keys::keys(decl, ident.text(), &ty, Some(&init), globals, errors)?;
    namespaces.is_first_variable(&ident).then(|| hir::Variable {
        name: ident.text().to_string(),
        ty,
        init,
        keys,
    })
}
