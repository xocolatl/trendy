//! The program's globals, evaluated when compiling: the variables' initial
//! values, and the read-only operators' values, which initial values can
//! call. Each is evaluated, or checked, when first needed, after what it
//! refers to, so one referred to while it is itself being evaluated closes a
//! cycle: of initial values, through operators or not, or of read-only
//! operators that call each other.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};

use trendy_diag::{Diagnostic, Span};
use trendy_hir as hir;
use trendy_syntax::ast::{self, AstNode, VarDecl};

use crate::assign::{self, Assignee, Assigning};
use crate::body::{Around, Growing};
use crate::check::{Checker, Lookup, Meaning, Names};
use crate::locals::{self, Declaration, Declaring, Local, Locals};
use crate::operators::{CallConstraint, FromType, Made, Operators, Signature};
use crate::types::{self, Expected, Origin};
use crate::{Files, Namespaces, Sites, errors, keys, ops, scope};

/// What the globals evaluate to.
pub(crate) struct Evaluated {
    /// Each variable, by id: `None` for one with errors, already reported.
    pub(crate) variables: Vec<Option<hir::Variable>>,
    /// Each valid read-only operator's value.
    pub(crate) values: HashMap<hir::OperatorId, hir::Expr>,
    /// Each valid read-only operator's local variables, which its value's
    /// `Let`s give values.
    pub(crate) locals: HashMap<hir::OperatorId, Vec<hir::Local>>,
    /// The read-only operators that reach a cycle of calls, or are in one
    /// (see [`ops::Bindings`]).
    pub(crate) recursive: HashSet<hir::OperatorId>,
    /// Each call in a read-only operator's body that gives a type parameter a
    /// type built from its caller's own.
    pub(crate) growing: Vec<Growing>,
    /// Each constant, by id: `None` for one with errors, already reported.
    pub(crate) constants: Vec<Option<hir::Constant>>,
    /// Each variable of the program's a read-only operator reads, and where,
    /// whose error has the fix of making it a constant, unless something assigns
    /// it, as the update operators' bodies, checked after, say.
    pub(crate) read_variables: Vec<(hir::VariableId, Span)>,
}

/// Evaluates the globals, each when first needed.
pub(crate) struct Globals<'a> {
    /// Where each place in the file is.
    sites: &'a Sites<'a>,
    namespaces: &'a Namespaces,
    operators: &'a Operators,
    /// Each variable's id and declaration, by the name its unit writes.
    declared: &'a scope::Variables,
    /// Each variable, by id, once its evaluation has started.
    variables: Vec<Option<State<hir::Variable>>>,
    /// Each constant, by id, once its evaluation has started.
    constants: Vec<Option<State<hir::Constant>>>,
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
    /// Each of those, as [`Evaluated::read_variables`] has it.
    read_variables: Vec<(hir::VariableId, Span)>,
    /// Each cycle already reported, by the globals in it, from the first
    /// reached.
    cycles: HashSet<Vec<Global>>,
    /// The local variables of value blocks in initial values, by id. Most
    /// have values known when compiling, which their names stand for; those
    /// computed for each tuple of a relation, as in an `extend`'s values,
    /// are substituted when each tuple is.
    block_locals: Vec<hir::Local>,
    /// The read-only operators that reach a cycle of calls, or are in one,
    /// as far as they have been checked (see [`ops::Bindings`]).
    recursive: HashSet<hir::OperatorId>,
    /// Each call in a read-only operator's body that gives a type parameter
    /// a type built from its caller's own.
    growing: Vec<Growing>,
    /// Each reference an operator, a constraint or a constant being checked has
    /// made.
    references: Vec<Reference>,
    /// Each operator, constraint or constant whose checking has started, as a
    /// cycle shows it.
    pendings: HashMap<Referred, Pending>,
}

/// What a reference is from, or to.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Referred {
    Operator(hir::OperatorId),
    Constant(hir::ConstantId),
}

/// A reference, written at `at`, that checking `from` makes to `to`, whether or
/// not it has been checked yet.
#[derive(Clone, Copy)]
struct Reference {
    from: Referred,
    to: Referred,
    at: Span,
}

impl Global {
    /// What a reference from it is from, if one can lead back to it: a
    /// variable's initial value is what nothing else refers to.
    fn referred(self) -> Option<Referred> {
        match self {
            Global::Operator(id) | Global::Constraint(id) | Global::CallConstraint(id) => {
                Some(Referred::Operator(id))
            }
            Global::Constant(id) => Some(Referred::Constant(id)),
            Global::Variable(_) => None,
        }
    }
}

/// What a global being evaluated is.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Global {
    /// A variable, whose initial value refers to another.
    Variable(hir::VariableId),
    /// A read-only operator, which a call checks.
    Operator(hir::OperatorId),
    /// A constant, which reading it checks.
    Constant(hir::ConstantId),
    /// A constraint of a type of the program's own, which selecting one of
    /// its values checks.
    Constraint(hir::OperatorId),
    /// A constraint in an operator's braces, which a call of it checks.
    CallConstraint(hir::OperatorId),
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
    /// Of a possible representation's own constraint, the representation,
    /// as its selector is written, `t{c: …}`.
    pub(crate) representation: Option<String>,
    /// Where its name is defined.
    pub(crate) defined: Span,
    /// Where it refers to the next one, once it does.
    pub(crate) via: Span,
    /// Whether it does by a `fail`, whose error the next one writes.
    pub(crate) failing: bool,
}

impl<'a> Globals<'a> {
    /// The globals of the files checked.
    pub(crate) fn new(
        namespaces: &'a Namespaces,
        operators: &'a Operators,
        declared: &'a scope::Variables,
        sites: &'a Sites<'a>,
    ) -> Self {
        Globals {
            sites,
            namespaces,
            operators,
            declared,
            variables: vec![None; declared.len()],
            constants: vec![None; declared.constants()],
            operator_states: HashMap::new(),
            values: HashMap::new(),
            locals: HashMap::new(),
            stack: Vec::new(),
            read: HashSet::new(),
            read_variables: Vec::new(),
            cycles: HashSet::new(),
            block_locals: Vec::new(),
            recursive: HashSet::new(),
            growing: Vec::new(),
            references: Vec::new(),
            pendings: HashMap::new(),
        }
    }

    /// Every variable's initial value, and every read-only operator's
    /// value, whether or not an initial value calls it.
    pub(crate) fn evaluate(mut self, files: &Files, errors: &mut Vec<Diagnostic>) -> Evaluated {
        for decl in files.variables() {
            let Some(ident) = decl.name().and_then(|n| n.ident()) else {
                continue;
            };
            let at = trendy_syntax::span(ident.text_range());
            if let Some(id) = self.declared.find(ident.text(), at)
                && self.namespaces.is_first_variable(&ident)
            {
                self.variable(id, errors);
            } else {
                // A duplicate, already reported, but it can have errors of
                // its own.
                variable(&decl, &mut self, errors);
            }
        }
        for constant in 0..self.constants.len() {
            self.constant(hir::ConstantId(constant), errors);
        }
        let operators = self.operators;
        for signature in operators.iter().filter(|s| s.read_only) {
            if let Some(defined) = signature.defined() {
                self.checked(signature.id, defined, errors);
            }
        }
        self.report_cycles_through_what_was_checked(errors);
        // What their value blocks declare, as they all number theirs together.
        let locals = self.block_locals.clone();
        let constants = std::mem::take(&mut self.constants)
            .into_iter()
            .map(|state| match state {
                Some(State::Done(constant)) => constant.map(|constant| hir::Constant {
                    locals: locals.clone(),
                    ..constant
                }),
                _ => None,
            })
            .collect();
        Evaluated {
            constants,
            read_variables: std::mem::take(&mut self.read_variables),
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
            recursive: self.recursive,
            growing: self.growing,
        }
    }

    /// The variable `id`, evaluated if it isn't yet.
    fn variable(
        &mut self,
        id: hir::VariableId,
        errors: &mut Vec<Diagnostic>,
    ) -> Option<hir::Variable> {
        match &self.variables[id.0] {
            Some(State::Done(variable)) => return variable.clone(),
            Some(State::Evaluating) => unreachable!("cycles are caught at the reference"),
            None => {}
        }
        let decl = self.declared.decl(id).clone();
        let ident = decl.name()?.ident()?;
        let declared = trendy_syntax::span(ident.text_range());
        self.variables[id.0] = Some(State::Evaluating);
        self.stack.push(Pending {
            global: Global::Variable(id),
            name: ident.text().to_string(),
            representation: None,
            defined: declared,
            via: declared,
            failing: false,
        });
        let variable = variable(&decl, self, errors);
        self.stack.pop();
        self.variables[id.0] = Some(State::Done(variable.clone()));
        variable
    }

    /// The constant `id`, evaluated if it isn't yet.
    fn constant(
        &mut self,
        id: hir::ConstantId,
        errors: &mut Vec<Diagnostic>,
    ) -> Option<hir::Constant> {
        match &self.constants[id.0] {
            Some(State::Done(constant)) => return constant.clone(),
            Some(State::Evaluating) => unreachable!("cycles are caught at the reference"),
            None => {}
        }
        let declared = self.declared.constant(id).clone();
        let ident = declared.first.name()?.ident()?;
        let defined = trendy_syntax::span(ident.text_range());
        self.constants[id.0] = Some(State::Evaluating);
        let pending = Pending {
            global: Global::Constant(id),
            name: ident.text().to_string(),
            representation: None,
            defined,
            via: defined,
            failing: false,
        };
        self.pendings
            .insert(Referred::Constant(id), pending.clone());
        self.stack.push(pending);
        let constant = constant(&declared, self, errors);
        self.stack.pop();
        self.constants[id.0] = Some(State::Done(constant.clone()));
        constant
    }

    /// What reading the constant `id` at `span` is, evaluated if it isn't yet,
    /// or that it is reported, as a cycle its reference closes is.
    fn constant_read(
        &mut self,
        id: hir::ConstantId,
        span: Span,
        errors: &mut Vec<Diagnostic>,
    ) -> Lookup {
        if let Some(from) = self
            .stack
            .last()
            .and_then(|pending| pending.global.referred())
        {
            self.references.push(Reference {
                from,
                to: Referred::Constant(id),
                at: span,
            });
        }
        if let Some(State::Evaluating) = self.constants[id.0] {
            let start = self
                .stack
                .iter()
                .position(|p| p.global == Global::Constant(id));
            if let Some(start) = start {
                self.cycle(start, span, errors);
            }
            return Lookup::Reported;
        }
        if let Some(innermost) = self.stack.last_mut() {
            innermost.via = span;
        }
        match self.constant(id, errors) {
            Some(constant) => Lookup::Found(read_of(id, &constant)),
            None => Lookup::Reported,
        }
    }

    /// The variable `id`, referred to at `span`, evaluated if it isn't
    /// yet, or `None` after reporting why it has none, such as a cycle its
    /// reference closes.
    fn reach(
        &mut self,
        id: hir::VariableId,
        span: Span,
        errors: &mut Vec<Diagnostic>,
    ) -> Option<hir::Variable> {
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
        self.variable(id, errors)
    }

    /// The type of the variable `id`, as its declaration writes it, or
    /// the named heading of the selector its initial value is, as in
    /// `relation home { … }`, or else as its value has it, if that is
    /// known: its initial value isn't evaluated here, as that could lose
    /// its errors.
    fn written_type(&self, id: hir::VariableId) -> Option<String> {
        let decl = self.declared.decl(id);
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
        match &self.variables[id.0] {
            Some(State::Done(Some(variable))) => Some(variable.ty.to_string()),
            _ => None,
        }
    }

    /// Whether the read-only operator `id`, called at `at`, has a valid
    /// value, its body checked if it isn't yet. Calling one whose body is
    /// being checked closes a cycle: recursion, whose calls are made when the
    /// program runs, or, through what a type's declaration or an operator's
    /// constraint makes, what nothing can stop (A0036).
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
            let valid = valid && (bodiless || self.checked_own(id, at, errors));
            if signature
                .versions
                .iter()
                .any(|version| self.recursive.contains(&version.operator))
            {
                self.recursive.insert(id);
            }
            return valid;
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
        if let Some(from) = self
            .stack
            .last()
            .and_then(|pending| pending.global.referred())
        {
            self.references.push(Reference {
                from,
                to: Referred::Operator(id),
                at,
            });
        }
        match self.operator_states.get(&id) {
            Some(State::Done(done)) => return done.is_some(),
            Some(State::Evaluating) => return self.closes_a_cycle(id, at, errors),
            None => {}
        }
        if let Some(innermost) = self.stack.last_mut() {
            innermost.via = at;
        }
        let defined = signature.defined().unwrap_or(at);
        self.operator_states.insert(id, State::Evaluating);
        let (global, name) = match (signature.constrains(), &signature.constrains_call) {
            (Some((ty, _)), _) => (Global::Constraint(id), ty.name.clone()),
            (None, Some(call)) => (
                Global::CallConstraint(id),
                self.operators.get(call.of).name.clone(),
            ),
            (None, None) => (Global::Operator(id), signature.name.clone()),
        };
        // A representation's own constraint, as its selector is written.
        let representation = signature
            .constrains()
            .filter(|_| signature.constrained_representation().is_some())
            .map(|(ty, _)| {
                let components: Vec<String> =
                    signature.params.keys().map(|c| format!("{c}: …")).collect();
                format!("{}{{{}}}", ty.name, components.join(", "))
            });
        let pending = Pending {
            global,
            name,
            representation,
            defined,
            via: defined,
            failing: false,
        };
        self.pendings
            .insert(Referred::Operator(id), pending.clone());
        self.stack.push(pending);
        let value = self.value(signature, errors);
        self.stack.pop();
        let valid = value.is_some();
        if let Some((locals, value)) = value {
            // What calls one that reaches a cycle reaches it too. Every other
            // operator it calls is checked, and reaches none, so a call's
            // evaluation never meets a cycle.
            if value
                .calling()
                .iter()
                .any(|callee| self.recursive.contains(callee))
            {
                self.recursive.insert(id);
            }
            self.values.insert(id, value);
            self.locals.insert(id, locals);
        }
        self.operator_states
            .insert(id, State::Done(valid.then_some(())));
        valid
    }

    /// Whether a reference at `at` to the read-only operator `id`, whose body is
    /// being checked, is valid: recursion, as each operator in the cycle it
    /// closes runs when the program does, and a call of it is left for then, or
    /// not, as the cycle has what nothing can stop, which it reports.
    fn closes_a_cycle(
        &mut self,
        id: hir::OperatorId,
        at: Span,
        errors: &mut Vec<Diagnostic>,
    ) -> bool {
        let Some(start) = self.stack.iter().position(|p| {
            matches!(
                p.global,
                Global::Operator(o) | Global::Constraint(o) | Global::CallConstraint(o)
                    if o == id
            )
        }) else {
            return false;
        };
        let operators_alone = self.stack[start..].iter().all(|p| match p.global {
            Global::Operator(o) => self.operators.get(o).from_type.is_none(),
            Global::Constraint(_)
            | Global::CallConstraint(_)
            | Global::Variable(_)
            | Global::Constant(_) => false,
        });
        if !operators_alone {
            self.cycle(start, at, errors);
            return false;
        }
        for pending in &self.stack[start..] {
            if let Global::Operator(operator) = pending.global {
                self.recursive.insert(operator);
            }
        }
        true
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
        self.report_cycle(cycle, errors);
    }

    /// Report each cycle that is more than operators calling each other that
    /// checking them, each as it is first needed, can't see: one operator of a
    /// cycle can be checked before another reference to it closes a second one,
    /// through a constraint, and a `fail` always refers to `message`, whose
    /// calls then lead back to it, which writing the error would run again
    /// without end (see [`Expr::fails`]). Only one of each is reported, as one
    /// more cycle would show once it is fixed.
    ///
    /// [`Expr::fails`]: hir::Expr::fails
    fn report_cycles_through_what_was_checked(&mut self, errors: &mut Vec<Diagnostic>) {
        let references = std::mem::take(&mut self.references);
        let operators = self.operators.iter().count();
        let node = |referred: Referred| match referred {
            Referred::Operator(id) => id.0,
            Referred::Constant(id) => operators + id.0,
        };
        let count = references
            .iter()
            .map(|reference| node(reference.from).max(node(reference.to)) + 1)
            .max()
            .unwrap_or(0);
        let mut edges = vec![Vec::new(); count];
        for reference in &references {
            edges[node(reference.from)].push(node(reference.to));
        }
        let cycles = hir::cycles_of(&edges);
        // Those already reported, whose errors say enough.
        let mut reported: HashSet<usize> = self
            .cycles
            .iter()
            .flatten()
            .filter_map(|global| {
                let referred = global.referred()?;
                cycles.get(node(referred)).copied().flatten()
            })
            .collect();
        let writes_errors: Vec<hir::OperatorId> =
            self.operators.message().map_or_else(Vec::new, |message| {
                message.versions.iter().map(|v| v.operator).collect()
            });
        for reference in &references {
            let Some(cycle) = cycles[node(reference.from)] else {
                continue;
            };
            if cycles[node(reference.to)] != Some(cycle) || reported.contains(&cycle) {
                continue;
            }
            let failing = match (reference.from, reference.to) {
                (Referred::Operator(from), Referred::Operator(to)) => {
                    writes_errors.contains(&to)
                        && self.values.get(&from).is_some_and(hir::Expr::fails)
                }
                _ => false,
            };
            let from = &self.pendings[&reference.from];
            let checks_itself = match from.global {
                Global::Operator(id) => self.operators.get(id).from_type.is_some(),
                _ => true,
            };
            if !failing && !checks_itself {
                continue;
            }
            reported.insert(cycle);
            let leading = references_between(&references, reference.to, reference.from, |to| {
                cycles[node(to)] == Some(cycle)
            });
            let mut shown = vec![Pending {
                via: reference.at,
                failing,
                ..from.clone()
            }];
            shown.extend(leading.into_iter().map(|next| Pending {
                via: next.at,
                ..self.pendings[&next.from].clone()
            }));
            self.report_cycle(shown, errors);
        }
    }

    /// Report the `cycle`, unless it is reported already: see
    /// [`Globals::cycle`].
    fn report_cycle(&mut self, mut cycle: Vec<Pending>, errors: &mut Vec<Diagnostic>) {
        let first = cycle
            .iter()
            .position(|p| matches!(p.global, Global::Constraint(_) | Global::CallConstraint(_)))
            .or_else(|| {
                cycle
                    .iter()
                    .position(|p| matches!(p.global, Global::Constant(_)))
            });
        if let Some(first) = first {
            cycle.rotate_left(first);
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
    /// variable's value once. A body whose statements decide what runs next
    /// has them run, as its value (see [`hir::Expr::Run`]). `None` after
    /// reporting why it has none.
    fn value(
        &mut self,
        signature: &Signature,
        errors: &mut Vec<Diagnostic>,
    ) -> Option<(Vec<hir::Local>, hir::Expr)> {
        if let Some((defined, constraint)) = signature.constrains() {
            return self.constraint(signature, defined, constraint, errors);
        }
        if let Some(call) = &signature.constrains_call {
            return self.call_constraint(signature, call, errors);
        }
        if let Some(from) = &signature.from_type {
            return self.converted(signature, from, errors);
        }
        let decl = signature.decl.as_ref()?;
        let written = decl.result()?;
        let body = decl.body()?;
        if body.decides_what_runs() {
            return self.run(signature, &body, &written, errors);
        }
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
                ast::Statement::Fail(_)
                | ast::Statement::If(_)
                | ast::Statement::While(_)
                | ast::Statement::Loop(_)
                | ast::Statement::For(_)
                | ast::Statement::Transaction(_) => {
                    unreachable!("a body with them has its statements run")
                }
                ast::Statement::Warn(stmt) => {
                    let within = format!("the read-only operator `{}`", signature.name);
                    errors.push(errors::warn_for_value(stmt, &within));
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

    /// The value of the read-only operator `signature`, whose `body`, of the
    /// result type `written`, has statements that decide what runs next,
    /// and its local variables. They read and assign only its parameters and
    /// local variables (see [`ReadOnlyNames`]), and it isn't computed when
    /// compiling. `None` after reporting why it has none.
    fn run(
        &mut self,
        signature: &Signature,
        body: &ast::Body,
        written: &ast::TypeExpr,
        errors: &mut Vec<Diagnostic>,
    ) -> Option<(Vec<hir::Local>, hir::Expr)> {
        let written_value = body.value();
        if written_value.is_none() {
            errors.push(errors::missing_value(&signature.name, body, written));
        }
        let expected = signature.result.clone().map(|ty| Expected {
            ty,
            origin: Origin::Type(written.clone()),
        });
        let statements: Vec<ast::Statement> = body.statements().collect();
        let mut locals = Locals::default();
        let mut names = ReadOnlyNames {
            globals: self,
            within: signature.id,
            locals: &mut locals,
        };
        let (locals, value) = crate::body::run(
            &mut names,
            &statements,
            written_value.as_ref(),
            expected.as_ref(),
            errors,
        );
        value
            .filter(|_| signature.valid)
            .map(|value| (locals, value))
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
            Made::Constraint { .. } => unreachable!("a constraint is checked on its own"),
        };
        let conversion = representation
            .conversions()
            .find(|c| c.direction() == Some(direction))?;
        // Each assigns components, whose types are written where they are
        // declared.
        match direction {
            ast::Direction::To => {
                let stored = self.stored_heading(defined);
                let written = stored.as_ref();
                self.conversion(signature, &conversion, &defined.components, written, errors)
            }
            ast::Direction::From => {
                let own = representation.components();
                let Some(hir::Type::Tuple(components)) = &signature.result else {
                    unreachable!("a conversion from the stored representation is of a tuple");
                };
                self.conversion(signature, &conversion, components, own.as_ref(), errors)
            }
        }
    }

    /// Where the stored components of `defined` are declared: in the stored
    /// representation of the type its values are stored as, which a
    /// subtype defined by constraints doesn't declare, found through that
    /// type's selector.
    fn stored_heading(&self, defined: &hir::Defined) -> Option<ast::Heading> {
        let stored = defined.stored_as();
        let (component, _) = stored.components.attributes().next()?;
        let selector = self.operators.selector(stored, component)?;
        let from = self.operators.get(selector).from_type.as_ref()?;
        from.stored.as_ref()?.components()
    }

    /// The value of `signature`, the selector of a possible representation of
    /// the type `defined` other than the stored one: the value of its
    /// components, the parameters, selected in it (see
    /// [`hir::Expr::SelectIn`]), once its own constraints, its conversion
    /// to the stored one and the type's constraints are checked. `None` if
    /// any has errors, reported when it is checked.
    fn selection(
        &mut self,
        signature: &Signature,
        defined: &hir::Defined,
        errors: &mut Vec<Diagnostic>,
    ) -> Option<(Vec<hir::Local>, hir::Expr)> {
        // Its representation, whose components are its parameters.
        let (position, representation) = defined
            .representations
            .iter()
            .enumerate()
            .find(|(_, r)| signature.takes(&r.components))
            .expect("only another representation's selector converts");
        let at = signature.defined()?;
        let mut checks = vec![representation.to];
        checks.extend(representation.constraints.iter().map(|c| c.operator));
        checks.extend(self.namespaces.selection_checks(defined));
        if !checks
            .into_iter()
            .all(|operator| self.checked(operator, at, errors))
        {
            return None;
        }
        let components = signature
            .params
            .keys()
            .map(|c| (c.clone(), hir::Expr::Parameter(c.clone())))
            .collect();
        let selected = hir::Expr::SelectIn(
            Box::new(defined.clone()),
            position,
            Box::new(hir::Expr::Tuple(components)),
        );
        Some((Vec::new(), selected))
    }

    /// The tuple of `components`, written in `written`, that `conversion`
    /// computes, in `signature`, whose parameters are the components it
    /// computes them from: each once, of its type, computed as a read-only
    /// operator's value is, and the local variables of the value blocks that
    /// do. `None` after reporting why not.
    fn conversion(
        &mut self,
        signature: &Signature,
        conversion: &ast::Conversion,
        components: &hir::Heading,
        written: Option<&ast::Heading>,
        errors: &mut Vec<Diagnostic>,
    ) -> Option<(Vec<hir::Local>, hir::Expr)> {
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
                .and_then(|w| w.attributes().find(|a| a.is_named(name)))
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
        ok.then(|| (locals.finish(), ops::tuple(values)))
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

    /// The value of the read-only operator `signature`, the constraint
    /// `call` in another operator's braces: its condition, of the parameters
    /// it reads, and its local variables.
    fn call_constraint(
        &mut self,
        signature: &Signature,
        call: &CallConstraint,
        errors: &mut Vec<Diagnostic>,
    ) -> Option<(Vec<hir::Local>, hir::Expr)> {
        let constraint = &call.constraint;
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
        let operator = &self.operators.get(call.of).name;
        if signature.params.is_empty() && signature.unresolved.is_empty() {
            errors.push(errors::constraint_reads_no_parameter(constraint, operator));
            return None;
        }
        if let hir::Expr::Constant(hir::Value::Boolean(holds)) = value {
            errors.push(errors::constant_call_constraint(
                holds, constraint, operator,
            ));
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

/// The references, the fewest, that lead from `from` to `to`, by those that
/// `within` says of: none if it is `from`.
fn references_between(
    references: &[Reference],
    from: Referred,
    to: Referred,
    within: impl Fn(Referred) -> bool,
) -> Vec<Reference> {
    if from == to {
        return Vec::new();
    }
    let mut came_by: HashMap<Referred, Reference> = HashMap::new();
    let mut next = VecDeque::from([from]);
    while let Some(at) = next.pop_front() {
        for reference in references.iter().filter(|r| r.from == at && within(r.to)) {
            if reference.to != from && !came_by.contains_key(&reference.to) {
                came_by.insert(reference.to, *reference);
                next.push_back(reference.to);
            }
        }
    }
    let mut path = Vec::new();
    let mut at = to;
    while let Some(reference) = came_by.get(&at) {
        path.push(*reference);
        at = reference.from;
    }
    path.reverse();
    path
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
        if let Some(constant) = self.declared.find_constant(name, span) {
            return self.constant_read(constant, span, errors);
        }
        if let Some(id) = self.declared.find(name, span) {
            // What a constant is computed from never changes.
            if let Some(Pending {
                global: Global::Constant(_),
                name: constant,
                ..
            }) = self.stack.last()
            {
                errors.push(errors::variable_in_constant(
                    name,
                    span,
                    constant,
                    self.declared.decl(id),
                ));
                return Lookup::Reported;
            }
            return match self.reach(id, span, errors) {
                // Of its declared type, which can be a supertype of its
                // value's.
                Some(variable) => {
                    Lookup::Found(hir::Expr::Constant(variable.init).treated(&variable.ty))
                }
                None => Lookup::Reported,
            };
        }
        if self.namespaces.operator(name, span).is_some() {
            errors.push(errors::operator_as_value(name, span, self.operators));
            return Lookup::Reported;
        }
        Lookup::Unknown
    }

    fn constant_of(
        &mut self,
        name: &str,
        unit: hir::FileId,
        span: Span,
        errors: &mut Vec<Diagnostic>,
    ) -> Lookup {
        self.declared
            .constant_in(name, unit, span)
            .map_or(Lookup::Unknown, |id| self.constant_read(id, span, errors))
    }

    fn candidates(&self, at: Span) -> Vec<String> {
        self.declared.names(at).map(str::to_string).collect()
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

    fn next_local(&self) -> usize {
        self.block_locals.len()
    }

    fn around(&self, _: &str) -> Option<Meaning> {
        None
    }

    fn checked(&mut self, id: hir::OperatorId, at: Span, errors: &mut Vec<Diagnostic>) -> bool {
        Globals::checked(self, id, at, errors)
    }

    fn bindings(&self) -> ops::Bindings<'_> {
        ops::Bindings {
            operators: self.operators,
            namespaces: self.namespaces,
            values: &self.values,
            recursive: &self.recursive,
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
                    self.globals.declared.find(name, span).is_some(),
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
        // In a constraint in an operator's braces, a parameter that has no
        // one value a call can be checked with.
        if let Some(call) = &self.signature().constrains_call {
            let operator = self.globals.operators.get(call.of);
            if operator.mode(name) == Some(hir::Mode::Out) || operator.per_relation(name).is_some()
            {
                errors.push(errors::constraint_reads_no_value(name, span, operator));
                return Lookup::Reported;
            }
        }
        // In a representation's own constraint, a component of another,
        // which its components, as given, don't have: the stored one's,
        // though of every value, read as the others' do, so that they don't
        // say which it is.
        if let Some((defined, constraint)) = self.signature().constrains()
            && let Some(own) = constraint
                .syntax()
                .ancestors()
                .find_map(ast::Representation::cast)
            && defined.representation(name).is_some()
        {
            errors.push(errors::other_representations_component(
                name, span, defined, constraint, &own,
            ));
            return Lookup::Reported;
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
        if let Some(id) = self.globals.declared.find(name, span) {
            if self.globals.read.insert((self.within, name.to_string())) {
                let ty = self.globals.written_type(id);
                let mut error = errors::variable_in_read_only(
                    name,
                    span,
                    self.signature(),
                    self.globals.operators,
                    ty,
                );
                // Ahead of passing it, and withdrawn if something assigns it.
                error.suggestions.splice(
                    0..0,
                    errors::declared_constant(self.globals.declared.decl(id)),
                );
                errors.push(error);
                self.globals.read_variables.push((id, span));
            }
            return Lookup::Reported;
        }
        if let Some(constant) = self.globals.declared.find_constant(name, span) {
            return self.globals.constant_read(constant, span, errors);
        }
        if let Some(relation) = self.globals.operators.get(self.within).per_relation(name) {
            errors.push(errors::per_as_value(name, relation, span));
            return Lookup::Reported;
        }
        if self.globals.namespaces.operator(name, span).is_some() {
            errors.push(errors::operator_as_value(
                name,
                span,
                self.globals.operators,
            ));
            return Lookup::Reported;
        }
        Lookup::Unknown
    }

    fn constant_of(
        &mut self,
        name: &str,
        unit: hir::FileId,
        span: Span,
        errors: &mut Vec<Diagnostic>,
    ) -> Lookup {
        self.globals
            .declared
            .constant_in(name, unit, span)
            .map_or(Lookup::Unknown, |id| {
                self.globals.constant_read(id, span, errors)
            })
    }

    fn candidates(&self, at: Span) -> Vec<String> {
        // A constraint can read any of its operator's parameters.
        let parameters = match &self.signature().constrains_call {
            Some(call) => self.globals.operators.get(call.of),
            None => self.signature(),
        };
        parameters
            .parameter_names()
            .chain(self.locals.names())
            .chain(self.globals.declared.names(at))
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

    fn next_local(&self) -> usize {
        self.locals.next()
    }

    fn around(&self, name: &str) -> Option<Meaning> {
        if self.signature().is_parameter(name) {
            Some(Meaning::Parameter)
        } else if matches!(
            self.locals.find(name),
            Some(Local::Declared(..) | Local::Later(_) | Local::Invalid)
        ) {
            Some(Meaning::LocalVariable)
        } else {
            None
        }
    }

    fn type_parameters(&self) -> Option<&[hir::TypeParameter]> {
        Some(&self.globals.operators.get(self.within).types)
    }

    fn checked(&mut self, id: hir::OperatorId, at: Span, errors: &mut Vec<Diagnostic>) -> bool {
        self.globals.checked(id, at, errors)
    }

    fn calling(&mut self, callee: &Signature, types: &[hir::Type], span: Span) {
        let growing = Growing::of(self.within, callee, types, span);
        self.globals.growing.extend(growing);
    }

    fn bindings(&self) -> ops::Bindings<'_> {
        ops::Bindings {
            operators: self.globals.operators,
            namespaces: self.globals.namespaces,
            values: &self.globals.values,
            recursive: &self.globals.recursive,
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
        match self.locals.find(name) {
            Some(Local::Declared(_, _, Declaration::Var(declaration)))
                if declaration.is_const() =>
            {
                errors.push(errors::assigned_constant(name, span, declaration));
                return None;
            }
            Some(Local::Declared(id, local, Declaration::Var(declaration))) => {
                return Some(Assignee::local(id, local, declaration));
            }
            // A loop's variable is a loop's, whose statements are run.
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
                    self.globals.declared.find(name, span).is_some(),
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
        let names: Vec<String> = self.locals.names().map(str::to_string).collect();
        let names: Vec<&str> = names.iter().map(String::as_str).collect();
        self.assigned_around(name, span, &names, errors)
    }
}

impl Around for ReadOnlyNames<'_, '_, '_> {
    fn declaring(&self) -> Declaring<'_> {
        Declaring::Operator(self.signature())
    }

    fn refused(&self, statement: &ast::Statement) -> Option<Diagnostic> {
        let signature = self.signature();
        match statement {
            ast::Statement::Call(stmt) => Some(errors::call_in_read_only(&signature.name, stmt)),
            ast::Statement::Warn(stmt) => {
                let within = format!("the read-only operator `{}`", signature.name);
                Some(errors::warn_for_value(stmt, &within))
            }
            // `return;` at the end, with no value, is the body without one,
            // reported then; the parser makes `return x;` there the value.
            ast::Statement::Return(stmt) => {
                let body = signature.decl.as_ref().and_then(ast::Operator::body);
                let last = body.as_ref().is_some_and(|body| {
                    body.value().is_none()
                        && body
                            .statements()
                            .last()
                            .is_some_and(|last| last.syntax() == statement.syntax())
                });
                (!last).then(|| errors::early_return(stmt))
            }
            _ => None,
        }
    }

    fn is_variable(&self, name: &str, span: Span) -> bool {
        self.globals.declared.find(name, span).is_some()
    }

    fn assigned_around(
        &mut self,
        name: &str,
        span: Span,
        locals: &[&str],
        errors: &mut Vec<Diagnostic>,
    ) -> Option<Assignee> {
        let op = self.globals.operators.get(self.within);
        if op.is_parameter(name) {
            let hidden = self.globals.declared.find(name, span).is_some();
            errors.push(errors::assigned_parameter(name, span, hidden));
        } else if self.globals.declared.find(name, span).is_some() {
            errors.push(errors::assigned_in_read_only(name, span, &op.name));
        } else if let Some(id) = self.globals.declared.find_constant(name, span) {
            errors.push(errors::assigned_constant(
                name,
                span,
                &self.globals.declared.constant(id).first,
            ));
        } else if let Some((kind, _)) = self.globals.namespaces.operator(name, span) {
            errors.push(errors::assigned_operator(name, span, kind));
        } else {
            errors.push(errors::unknown_name(name, span, "variable", locals));
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
    let id = globals
        .declared
        .find(ident.text(), trendy_syntax::span(ident.text_range()))
        .filter(|_| namespaces.is_first_variable(&ident))?;
    Some(hir::Variable {
        name: globals.declared.identity(id).to_string(),
        ty,
        init,
        keys,
    })
}

/// The constant `declared`, its declaration, its value computed when compiling
/// if it can be: and otherwise, as nothing but statements or calls that may not
/// stop are left of it, as an expression, which the program computes first. It is
/// of the type its interface says, or its own says, or else its value's.
fn constant(
    declared: &scope::ConstantDecl,
    globals: &mut Globals,
    errors: &mut Vec<Diagnostic>,
) -> Option<hir::Constant> {
    let ident = declared.first.name()?.ident()?;
    let (name, span) = (ident.text(), trendy_syntax::span(ident.text_range()));
    let valued = declared.completed.as_ref().unwrap_or(&declared.first);
    let Some(written) = valued.initializer() else {
        let interface = ast::section_of(valued.syntax()) == ast::Section::Interface;
        errors.push(errors::constant_without_value(name, span, interface));
        return None;
    };
    if declared.completed.is_some() && declared.first.type_expr().is_none() {
        errors.push(errors::interface_constant_without_type(name, span));
        return None;
    }
    let namespaces = globals.namespaces;
    let type_expr = declared.first.type_expr().or_else(|| valued.type_expr());
    let declared_type = match &type_expr {
        Some(written) => {
            let mut parameters = types::TypeParameters::None;
            Some((
                types::resolve(written, &mut parameters, namespaces, errors),
                written.clone(),
            ))
        }
        None => None,
    };
    if let (Some((Some(ty), _)), Some(own)) = (&declared_type, valued.type_expr())
        && declared.completed.is_some()
    {
        let mut parameters = types::TypeParameters::None;
        if let Some(own_type) = types::resolve(&own, &mut parameters, namespaces, errors)
            && own_type != *ty
        {
            errors.push(errors::constant_type_differs(name, &own, ty));
            return None;
        }
    }
    let at = globals.site(trendy_syntax::span(valued.syntax().text_range()));
    let mut checker = Checker::new(&mut *globals);
    let (ty, value) = match declared_type {
        None => {
            let value = checker.expr(&written, None, errors)?;
            (checker.ty(&value), value)
        }
        Some((None, _)) => {
            // The type is invalid, but the value can still have errors of its
            // own.
            checker.expr(&written, None, errors);
            return None;
        }
        Some((Some(ty), type_expr)) => {
            let expected = types::Expected {
                ty: ty.clone(),
                origin: types::Origin::Type(type_expr),
            };
            let value = checker.expr(&written, Some(&expected), errors)?;
            (ty, value)
        }
    };
    let value = if ops::known_value(&value).is_some() {
        value
    } else {
        match ops::evaluated(value, globals.bindings()) {
            Ok(value) => value,
            Err(failed) => {
                errors.push(errors::failed(&failed, &written));
                return None;
            }
        }
    };
    let known = ops::known_value(&value).cloned();
    let keys = keys::keys(valued, name, &ty, known.as_ref(), globals, errors)?;
    let value = match known {
        Some(known) => hir::Expr::Constant(known),
        None => hir::Expr::at(at, value),
    };
    Some(hir::Constant {
        name: declared.identity.clone(),
        ty,
        keys,
        locals: Vec::new(),
        value,
    })
}

/// What reading the constant `id`, `constant`, is: its value, if that is known
/// when compiling, and otherwise what the program computes first.
pub(crate) fn read_of(id: hir::ConstantId, constant: &hir::Constant) -> hir::Expr {
    if let hir::Expr::Constant(value) = &constant.value {
        return hir::Expr::Constant(value.clone()).treated(&constant.ty);
    }
    hir::Expr::ComputedConstant(id, Box::new(constant.ty.clone()))
}
