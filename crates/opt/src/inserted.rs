//! Constants written where they are read.
//!
//! A constant means its expression: it reads no variable, and what it calls are
//! functions of their arguments, so every read has the same value (TTM RM
//! Prescription 3). So for each read it is the compiler's to choose whether to
//! compute that expression there, where what is around it can need less of it, or
//! to share one computed value. Computing it again at each read can cost far more
//! than it saves, so a constant is written where it is read only if that costs no
//! more than computing it once: if that is its only read, which runs at most once.
//! Others are shared, which lowering computes when a read first needs it.

use std::collections::{BTreeMap, HashMap, HashSet};

use trendy_hir as hir;

/// `program`, with each constant that can be written where it is read written
/// there. What it has no more reads of stays, and nothing computes it.
pub(crate) fn constants_inserted(program: hir::Program) -> hir::Program {
    let sites = Sites::of(&program);
    let inserted: HashSet<hir::ConstantId> = (0..program.constants.len())
        .map(hir::ConstantId)
        .filter(|id| sites.can_be_inserted(*id, &program))
        .collect();
    if inserted.is_empty() {
        return program;
    }
    let inserting = Inserting {
        program: &program,
        inserted: &inserted,
    };
    let operators = program
        .operators
        .iter()
        .map(|operator| operator.clone().map_exprs(&mut |expr| inserting.expr(expr)))
        .collect();
    let constants = program
        .constants
        .iter()
        .map(|constant| hir::Constant {
            value: inserting.expr(constant.value.clone()),
            ..constant.clone()
        })
        .collect();
    hir::Program {
        operators,
        constants,
        ..program
    }
}

/// Where a read of a constant, or a call of an operator, is.
#[derive(Clone, Copy)]
struct Site {
    /// The operator it is in, or nothing for a constant's value, which is
    /// computed at most once.
    within: Option<hir::OperatorId>,
    /// Whether it can run more than once each time what it is in runs: in a
    /// loop, or for each tuple of a relation, or in a constraint, which is checked
    /// at each assignment.
    repeated: bool,
}

/// Each read of each constant, and each call of each operator, that is written
/// in the program. An operator's other callers, such as what a type's constraint
/// or a parameter's is checked by, have none, so it counts as called from more
/// than one place.
#[derive(Default)]
struct Sites {
    reads: HashMap<hir::ConstantId, Vec<Site>>,
    calls: HashMap<hir::OperatorId, Vec<Site>>,
}

impl Sites {
    fn of(program: &hir::Program) -> Sites {
        let mut sites = Sites::default();
        for (i, operator) in program.operators.iter().enumerate() {
            let site = Site {
                within: Some(hir::OperatorId(i)),
                repeated: false,
            };
            match &operator.kind {
                hir::OperatorKind::Update { body, .. } => sites.block(body, site),
                hir::OperatorKind::ReadOnly { value, .. } => sites.expr(value, site),
                hir::OperatorKind::Builtin(_) | hir::OperatorKind::Specification { .. } => {}
            }
        }
        let computed = Site {
            within: None,
            repeated: false,
        };
        for constant in &program.constants {
            sites.expr(&constant.value, computed);
        }
        let checked = Site {
            within: None,
            repeated: true,
        };
        for part in program.constraints.iter().flat_map(|c| &c.parts) {
            sites.expr(&part.condition, checked);
            if let Some(breaking) = &part.breaking {
                sites.expr(breaking, checked);
            }
        }
        sites
    }

    fn expr(&mut self, expr: &hir::Expr, site: Site) {
        match expr {
            hir::Expr::ComputedConstant(id, _) => self.reads.entry(*id).or_default().push(site),
            hir::Expr::Run(block, value) => {
                self.block(block, site);
                self.expr(value, site);
            }
            _ => {
                if let hir::Expr::Call(call) = expr {
                    self.calls.entry(call.operator).or_default().push(site);
                }
                expr.for_each_child(|child, tuples| {
                    let repeated = site.repeated || tuples > 0;
                    self.expr(child, Site { repeated, ..site });
                });
            }
        }
    }

    fn block(&mut self, block: &hir::Block, site: Site) {
        let repeated = Site {
            repeated: true,
            ..site
        };
        for (_, statement) in &block.statements {
            match statement {
                hir::Statement::Call(call) => {
                    self.calls.entry(call.operator).or_default().push(site);
                    for arg in call.args.values() {
                        self.expr(arg, site);
                    }
                }
                hir::Statement::Assign(assignments) => {
                    for assignment in assignments {
                        self.expr(&assignment.value, site);
                    }
                }
                hir::Statement::If(condition, then, otherwise) => {
                    self.expr(condition, site);
                    self.block(then, site);
                    self.block(otherwise, site);
                }
                hir::Statement::Loop(condition, body, _) => {
                    self.expr(condition, repeated);
                    self.block(body, repeated);
                }
                hir::Statement::For(_, array, body) => {
                    self.expr(array, site);
                    self.block(body, repeated);
                }
                hir::Statement::Fail(value) | hir::Statement::Warn(value) => {
                    self.expr(value, site);
                }
                hir::Statement::Transaction(_, body) => self.block(body, site),
            }
        }
    }

    /// Whether the constant `id` can be written where it is read: that is its
    /// only read, which runs at most once. Not one with a key to check, which
    /// takes all of it, nor one with local variables, of value blocks, which what
    /// reads it doesn't have and whose statements nothing around them can reduce.
    fn can_be_inserted(&self, id: hir::ConstantId, program: &hir::Program) -> bool {
        let constant = &program.constants[id.0];
        let [only] = self.reads.get(&id).map_or(&[][..], Vec::as_slice) else {
            return false;
        };
        !has_key_to_check(constant)
            && !has_locals(&constant.value)
            && self.runs_at_most_once(*only, program.entry)
    }

    /// Whether what is at `site` runs at most once in the whole of a run. An
    /// operator does if it is the entry, which nothing calls, or called from one
    /// place, which runs at most once. A cycle of calls has no way in from
    /// anywhere else, or it has more than one place it is called from.
    fn runs_at_most_once(&self, site: Site, entry: hir::OperatorId) -> bool {
        let mut seen = HashSet::new();
        let mut at = site;
        loop {
            if at.repeated {
                return false;
            }
            let Some(operator) = at.within else {
                return true;
            };
            if !seen.insert(operator) {
                return false;
            }
            match self.calls.get(&operator).map_or(&[][..], Vec::as_slice) {
                [] => return operator == entry,
                [only] if operator != entry => at = *only,
                _ => return false,
            }
        }
    }
}

/// Whether `constant` can break a key: every relation satisfies one of all its
/// attributes, which is what it has if it declares none.
fn has_key_to_check(constant: &hir::Constant) -> bool {
    let hir::Type::Relation(heading) = &constant.ty else {
        return false;
    };
    let attributes = heading.attributes().count();
    constant.keys.iter().any(|key| key.len() < attributes)
}

/// Whether `expr` declares or reads a local variable.
fn has_locals(expr: &hir::Expr) -> bool {
    if matches!(
        expr,
        hir::Expr::Local(_) | hir::Expr::Let(..) | hir::Expr::Run(..)
    ) {
        return true;
    }
    let mut found = false;
    expr.for_each_child(|child, _| found |= has_locals(child));
    found
}

/// Writes these constants where they are read.
struct Inserting<'a> {
    program: &'a hir::Program,
    inserted: &'a HashSet<hir::ConstantId>,
}

impl Inserting<'_> {
    /// `expr`, each read in it of a constant to insert replaced by its value,
    /// and each read in that too.
    fn expr(&self, expr: hir::Expr) -> hir::Expr {
        match expr {
            hir::Expr::ComputedConstant(id, _) if self.inserted.contains(&id) => {
                let constant = &self.program.constants[id.0];
                let value = self.expr(constant.value.clone());
                if self.program.type_of(&value, &BTreeMap::new(), &[]) == constant.ty {
                    value
                } else {
                    value.treated(&constant.ty)
                }
            }
            expr => expr.map_children(|child, _| self.expr(child)),
        }
    }
}
