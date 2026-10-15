//! Update operators' bodies: their statements, and the expressions in them.

use std::collections::{BTreeMap, HashMap};

use trendy_diag::{Diagnostic, Span};
use trendy_hir as hir;
use trendy_syntax::ast::{self, VarDecl};

use crate::check::{Checker, Lookup, Names, Written};
use crate::operators::Operators;
use crate::{Namespaces, assign, errors, ops};

/// Everything a body can refer to.
pub(crate) struct Context<'a> {
    pub(crate) namespaces: &'a Namespaces,
    pub(crate) operators: &'a Operators,
    /// Each valid read-only operator's value.
    pub(crate) values: &'a HashMap<hir::OperatorId, hir::Expr>,
    /// Each variable, by id: `None` for one with errors, already reported.
    pub(crate) variables: &'a [Option<hir::Variable>],
    pub(crate) variable_ids: &'a HashMap<String, hir::VariableId>,
    /// Each variable's declaration, by name.
    pub(crate) declarations: &'a HashMap<String, VarDecl>,
}

impl Context<'_> {
    /// What evaluating a call in a body substitutes: no variable, since
    /// they change when the program runs.
    pub(crate) fn bindings(&self) -> ops::Bindings<'_> {
        ops::Bindings {
            values: self.values,
            variables: None,
        }
    }
}

/// The statements of the update operator `op`, whose id is `within`.
pub(crate) fn body(
    op: &ast::Operator,
    within: hir::OperatorId,
    cx: &Context,
    errors: &mut Vec<Diagnostic>,
) -> Option<Vec<hir::Statement>> {
    let body = op.body()?;
    let name = &cx.operators.get(within).name;
    // `main` with a result type is reported with its `return`s.
    let returns_reported = name == "main" && op.result().is_some();
    let mut statements = Vec::new();
    let mut ok = true;
    for stmt in body.statements() {
        let statement = match &stmt {
            ast::Statement::Call(stmt) => call(stmt, within, cx, errors),
            ast::Statement::Assign(stmt) => {
                assign::statement(&stmt.clauses().collect::<Vec<_>>(), within, cx, errors)
            }
            ast::Statement::Block(block) => assign::block(block, within, cx, errors),
            ast::Statement::Return(stmt) => {
                if !returns_reported {
                    errors.push(errors::return_in_update(name, stmt));
                }
                None
            }
        };
        match statement {
            Some(statement) => statements.push(statement),
            None => ok = false,
        }
    }
    ok.then_some(statements)
}

/// `call name{arguments};`, of an update operator.
fn call(
    stmt: &ast::CallStmt,
    within: hir::OperatorId,
    cx: &Context,
    errors: &mut Vec<Diagnostic>,
) -> Option<hir::Statement> {
    let ident = stmt.callee()?.ident()?;
    let name = ident.text();
    let span = trendy_syntax::span(ident.text_range());
    let Some(signature) = cx.operators.named(name) else {
        let what = if cx.operators.get(within).params.contains_key(name) {
            Some("a parameter")
        } else {
            cx.namespaces.variable(name).map(|_| "a variable")
        };
        if let Some(what) = what {
            errors.push(errors::not_an_operator(name, span, what));
        } else {
            let operators: Vec<&str> = cx.operators.names().collect();
            errors.push(errors::unknown_name(name, span, "operator", &operators));
        }
        return None;
    };
    if signature.read_only {
        errors.push(errors::call_of_read_only(name, span, signature.defined()));
        return None;
    }
    let args = stmt.arguments()?;
    if !signature.valid {
        return None;
    }
    let mut names = BodyNames { cx, within };
    let args = Checker::new(&mut names).arguments(Written::Arguments(&args), signature, errors)?;
    Some(hir::Statement::Call {
        operator: signature.id,
        args,
    })
}

/// Names in the body of `within`: its parameters, which hide variables of
/// the same name, and the variables.
pub(crate) struct BodyNames<'a, 'b> {
    pub(crate) cx: &'a Context<'b>,
    pub(crate) within: hir::OperatorId,
}

impl BodyNames<'_, '_> {
    fn params(&self) -> &BTreeMap<String, hir::ParamType> {
        &self.cx.operators.get(self.within).params
    }
}

impl Names for BodyNames<'_, '_> {
    fn namespaces(&self) -> &Namespaces {
        self.cx.namespaces
    }

    fn operators(&self) -> &Operators {
        self.cx.operators
    }

    fn resolve(&mut self, name: &str, span: Span, errors: &mut Vec<Diagnostic>) -> Lookup {
        if self.params().contains_key(name) {
            return Lookup::Found(hir::Expr::Parameter(name.to_string()));
        }
        if let Some(&id) = self.cx.variable_ids.get(name) {
            // A variable with errors in its declaration is already reported.
            return match self.cx.variables[id.0] {
                Some(_) => Lookup::Found(hir::Expr::Variable(id)),
                None => Lookup::Reported,
            };
        }
        if self.cx.namespaces.operator(name).is_some() {
            errors.push(errors::operator_as_value(name, span));
            return Lookup::Reported;
        }
        Lookup::Unknown
    }

    fn candidates(&self) -> Vec<String> {
        self.params()
            .keys()
            .chain(self.cx.variable_ids.keys())
            .cloned()
            .collect()
    }

    fn leaf_type(&self, leaf: &hir::Expr) -> hir::Type {
        match leaf {
            hir::Expr::Variable(id) => self.cx.variables[id.0]
                .as_ref()
                .expect("only valid variables are resolved")
                .ty
                .clone(),
            hir::Expr::Parameter(name) => match &self.params()[name] {
                hir::ParamType::Type(ty) => ty.clone(),
                hir::ParamType::Any => unreachable!("only built-ins have `any` parameters"),
            },
            _ => unreachable!("only variables and parameters are leaves"),
        }
    }

    fn checked(&mut self, id: hir::OperatorId, _: Span, _: &mut Vec<Diagnostic>) -> bool {
        // Every read-only operator is checked with the globals.
        self.cx.values.contains_key(&id)
    }

    fn bindings(&self) -> ops::Bindings<'_> {
        self.cx.bindings()
    }
}
