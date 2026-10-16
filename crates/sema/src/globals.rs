//! The program's globals, evaluated when compiling: the variables' initial
//! values, and the read-only operators' values, which initial values can
//! call. Each is evaluated, or checked, when first needed, after what it
//! refers to, so one referred to while it is itself being evaluated closes a
//! cycle: of initial values, through operators or not, or of read-only
//! operators that call each other.

use std::collections::HashMap;

use trendy_diag::{Diagnostic, Span};
use trendy_hir as hir;
use trendy_syntax::ast::{self, SourceFile, VarDecl};

use crate::check::{Checker, Lookup, Names};
use crate::operators::{Operators, Signature};
use crate::types::{self, Expected, Origin};
use crate::{Namespaces, errors, keys, ops};

/// What the globals evaluate to.
pub(crate) struct Evaluated {
    /// Each variable, by id: `None` for one with errors, already reported.
    pub(crate) variables: Vec<Option<hir::Variable>>,
    /// Each valid read-only operator's value.
    pub(crate) values: HashMap<hir::OperatorId, hir::Expr>,
}

/// Evaluates the globals, each when first needed.
pub(crate) struct Globals<'a> {
    namespaces: &'a Namespaces,
    operators: &'a Operators,
    /// The first declaration of each variable, by name.
    decls: &'a HashMap<String, VarDecl>,
    /// Each variable's id, by name.
    ids: &'a HashMap<String, hir::VariableId>,
    /// Each variable, by id, once its evaluation has started.
    variables: Vec<Option<State<hir::Variable>>>,
    /// Each evaluated variable's value, by id, which a call in an initial
    /// value substitutes for it.
    known: Vec<Option<hir::Value>>,
    /// Each read-only operator whose value's checking has started.
    operator_states: HashMap<hir::OperatorId, State<()>>,
    /// Each valid read-only operator's value.
    values: HashMap<hir::OperatorId, hir::Expr>,
    /// The globals being evaluated, innermost last.
    stack: Vec<Pending>,
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
    pub(crate) name: String,
    /// Whether it is a read-only operator, rather than a variable.
    pub(crate) operator: bool,
    /// Where its name is defined.
    pub(crate) defined: Span,
    /// Where it refers to the next one, once it does.
    pub(crate) via: Span,
}

impl<'a> Globals<'a> {
    pub(crate) fn new(
        namespaces: &'a Namespaces,
        operators: &'a Operators,
        decls: &'a HashMap<String, VarDecl>,
        ids: &'a HashMap<String, hir::VariableId>,
    ) -> Self {
        Globals {
            namespaces,
            operators,
            decls,
            ids,
            variables: vec![None; ids.len()],
            known: vec![None; ids.len()],
            operator_states: HashMap::new(),
            values: HashMap::new(),
            stack: Vec::new(),
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
            name: name.to_string(),
            operator: false,
            defined: declared,
            via: declared,
        });
        let variable = variable(&decl, self, errors);
        self.stack.pop();
        self.known[id.0] = variable.as_ref().map(|v| v.init.clone());
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
                .position(|p| !p.operator && p.name == name)?;
            self.cycle(start, span, errors);
            return None;
        }
        if let Some(innermost) = self.stack.last_mut() {
            innermost.via = span;
        }
        self.variable(name, errors)
    }

    /// Whether the read-only operator `id`, called at `at`, has a valid
    /// value, its body checked if it isn't yet. Calling one whose body is
    /// being checked closes a cycle: recursion, or a cycle with the initial
    /// values that call it.
    fn checked(&mut self, id: hir::OperatorId, at: Span, errors: &mut Vec<Diagnostic>) -> bool {
        let operators = self.operators;
        let signature = operators.get(id);
        match self.operator_states.get(&id) {
            Some(State::Done(done)) => return done.is_some(),
            Some(State::Evaluating) => {
                if let Some(start) = self
                    .stack
                    .iter()
                    .position(|p| p.operator && p.name == signature.name)
                {
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
        self.stack.push(Pending {
            name: signature.name.clone(),
            operator: true,
            defined,
            via: defined,
        });
        let value = self.value(signature, errors);
        self.stack.pop();
        let valid = value.is_some();
        if let Some(value) = value {
            self.values.insert(id, value);
        }
        self.operator_states
            .insert(id, State::Done(valid.then_some(())));
        valid
    }

    /// Report the cycle closed by a reference at `at` back to the global
    /// at `start` in the stack.
    fn cycle(&self, start: usize, at: Span, errors: &mut Vec<Diagnostic>) {
        let mut cycle = self.stack[start..].to_vec();
        if let Some(last) = cycle.last_mut() {
            last.via = at;
        }
        errors.push(errors::cycle(&cycle));
    }

    /// The value of the read-only operator `signature`: its body is one
    /// `return`, of a value of its result type, and changes nothing (TTM RM
    /// Prescription 3). `None` after reporting why it has none.
    fn value(&mut self, signature: &Signature, errors: &mut Vec<Diagnostic>) -> Option<hir::Expr> {
        let decl = signature.decl.as_ref()?;
        let written = decl.result()?;
        let body = decl.body()?;
        let mut valid = signature.valid;
        let mut returns = Vec::new();
        for statement in body.statements() {
            match statement {
                ast::Statement::Return(stmt) => returns.push(stmt),
                other => {
                    errors.push(errors::read_only_effect(&signature.name, &other));
                    valid = false;
                }
            }
        }
        let Some((first, extra)) = returns.split_first() else {
            errors.push(errors::missing_return(&signature.name, &body, &written));
            return None;
        };
        for stmt in extra {
            errors.push(errors::extra_return(stmt, first));
            valid = false;
        }
        let expected = signature.result.clone().map(|ty| Expected {
            ty,
            origin: Origin::Type(written),
        });
        let mut names = ReadOnlyNames {
            globals: self,
            within: signature.id,
        };
        let value = Checker::new(&mut names).expr(&first.value()?, expected.as_ref(), errors)?;
        valid.then_some(value)
    }
}

impl Names for Globals<'_> {
    fn namespaces(&self) -> &Namespaces {
        self.namespaces
    }

    fn operators(&self) -> &Operators {
        self.operators
    }

    fn resolve(&mut self, name: &str, span: Span, errors: &mut Vec<Diagnostic>) -> Lookup {
        if self.ids.contains_key(name) {
            return match self.reach(name, span, errors) {
                Some(variable) => Lookup::Found(hir::Expr::Constant(variable.init)),
                None => Lookup::Reported,
            };
        }
        if self.namespaces.operator(name).is_some() {
            errors.push(errors::operator_as_value(name, span));
            return Lookup::Reported;
        }
        Lookup::Unknown
    }

    fn candidates(&self) -> Vec<String> {
        self.decls.keys().cloned().collect()
    }

    fn leaf_type(&self, leaf: &hir::Expr) -> hir::Type {
        unreachable!("an initial value's names are constants, not {leaf:?}")
    }

    fn checked(&mut self, id: hir::OperatorId, at: Span, errors: &mut Vec<Diagnostic>) -> bool {
        Globals::checked(self, id, at, errors)
    }

    fn bindings(&self) -> ops::Bindings<'_> {
        ops::Bindings {
            values: &self.values,
            variables: Some(&self.known),
        }
    }
}

/// Names in the value of the read-only operator `within`: its parameters,
/// which hide variables of the same name, and the variables, which it reads
/// when it is called.
struct ReadOnlyNames<'g, 'a> {
    globals: &'g mut Globals<'a>,
    within: hir::OperatorId,
}

impl ReadOnlyNames<'_, '_> {
    fn params(&self) -> &std::collections::BTreeMap<String, hir::Type> {
        &self.globals.operators.get(self.within).params
    }
}

impl Names for ReadOnlyNames<'_, '_> {
    fn namespaces(&self) -> &Namespaces {
        self.globals.namespaces
    }

    fn operators(&self) -> &Operators {
        self.globals.operators
    }

    fn resolve(&mut self, name: &str, span: Span, errors: &mut Vec<Diagnostic>) -> Lookup {
        if self.params().contains_key(name) {
            return Lookup::Found(hir::Expr::Parameter(name.to_string()));
        }
        if let Some(&id) = self.globals.ids.get(name) {
            return match self.globals.reach(name, span, errors) {
                Some(_) => Lookup::Found(hir::Expr::Variable(id)),
                None => Lookup::Reported,
            };
        }
        if let Some(relation) = self.globals.operators.get(self.within).per_relation(name) {
            errors.push(errors::per_as_value(name, relation, span));
            return Lookup::Reported;
        }
        if self.globals.namespaces.operator(name).is_some() {
            errors.push(errors::operator_as_value(name, span));
            return Lookup::Reported;
        }
        Lookup::Unknown
    }

    fn candidates(&self) -> Vec<String> {
        self.params()
            .keys()
            .chain(self.globals.ids.keys())
            .cloned()
            .collect()
    }

    fn leaf_type(&self, leaf: &hir::Expr) -> hir::Type {
        match leaf {
            hir::Expr::Parameter(name) => self.params()[name].clone(),
            hir::Expr::Variable(id) => match &self.globals.variables[id.0] {
                Some(State::Done(Some(variable))) => variable.ty.clone(),
                _ => unreachable!("a variable is evaluated when its name is resolved"),
            },
            _ => unreachable!("only variables and parameters are leaves"),
        }
    }

    fn type_parameters(&self) -> Option<&[hir::TypeParameter]> {
        Some(&self.globals.operators.get(self.within).types)
    }

    fn checked(&mut self, id: hir::OperatorId, at: Span, errors: &mut Vec<Diagnostic>) -> bool {
        self.globals.checked(id, at, errors)
    }

    fn bindings(&self) -> ops::Bindings<'_> {
        ops::Bindings {
            values: &self.globals.values,
            variables: None,
        }
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
        (Some((ty, _)), None) => {
            let ty = ty?;
            let init = ty.default_value();
            (ty, init)
        }
        (None, Some(expr)) => {
            let value = types::value(&expr, None, globals, errors)?;
            (value.ty(), value)
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
            let value = types::value(&expr, Some(&expected), globals, errors)?;
            (ty, value)
        }
    };
    let keys = keys::keys(decl, ident.text(), &ty, &init, globals, errors)?;
    namespaces.is_first_variable(&ident).then(|| hir::Variable {
        name: ident.text().to_string(),
        ty,
        init,
        keys,
    })
}
