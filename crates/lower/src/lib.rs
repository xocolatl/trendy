//! Lowering: HIR to LIR. It chooses how each value is represented in memory,
//! turns statements into instructions, and builds the type descriptors that
//! runtime functions taking `any` receive.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use trendy_hir as hir;
use trendy_lir as lir;

/// Translate HIR to LIR. Infallible: sema has already rejected bad programs.
#[must_use]
pub fn lower(program: &hir::Program) -> lir::Program {
    let mut cx = Lowering::new(&program.files, &program.subtypes, integer_bounds(program));
    cx.recursion = program.recursion();
    cx.reading_transaction_start = program.reading_transaction_start();

    let mut variable_data = Vec::new();
    for variable in &program.variables {
        let fields = vec![cx.value_field(&variable.init)];
        variable_data.push(cx.push(lir::Data {
            kind: lir::DataKind::Variable {
                name: variable.name.clone(),
            },
            writable: true,
            fields,
        }));
    }

    // Each operator becomes a function once lowered code calls it,
    // starting with the entry point, and once for each list of types its
    // type parameters are given: an operator that is only called when
    // compiling, which sema evaluates, has none. Built-ins are runtime
    // calls.
    let (main, _) = function(program, &variable_data, program.entry, &[], &mut cx);
    let entry = entry(program, &variable_data, main, &mut cx);
    lir::Program {
        functions: cx.functions.into_values().collect(),
        data: cx.data,
        entry,
    }
}

/// The function the executable starts in: it gives the runtime the table
/// of the errors the language makes ([`Lowering::failures`]), gives each
/// variable the runtime gives its value that the program uses that value
/// ([`FunctionBuilder::given_variables`]), checks the constraints the compiler couldn't,
/// calls `main`, and writes the errors it gives, if it takes `out errors`, and
/// fails if one failed ([`MainErrors`]); if `main` fails, it writes the calls
/// the failure passed through (its trace), after its error, then fails. An error waiting, with
/// nothing to take it, a `fail`'s or the language's, is written first: its
/// message, as `message` gives it, with its kind's code, and where it
/// failed. If `message` fails with an error of its own, that one is
/// written instead; if it fails otherwise, its failure has written its
/// error. Either way the trace written is the failure's that is.
fn entry(
    program: &hir::Program,
    variable_data: &[lir::DataId],
    main: lir::FunctionId,
    cx: &mut Lowering,
) -> lir::FunctionId {
    let id = cx.number();
    let mut b = FunctionBuilder::new(
        program,
        "__entry".to_string(),
        BTreeMap::new(),
        variable_data,
    );
    let failures = lir::Place::whole(lir::Base::Data(cx.failures(program)));
    b.insts.push(lir::Inst::CallRuntime {
        function: lir::RuntimeFn::Failures,
        args: vec![failures],
    });
    let (handle, done) = (b.new_block(), b.new_block());
    let waiting = cx.waiting(program);
    let written = Written::new(&mut b, waiting, cx);
    let params = &program.operator(program.entry).params;
    let args = params_layout(params, &cx.families).map(|layout| b.slot(layout));
    let field = |name: &str| {
        let at = params.keys().position(|param| param == name)?;
        let mut field = args.clone()?;
        field.path.push(at);
        Some(field)
    };
    let main_errors = field("errors").map(|at| MainErrors::new(&mut b, at, cx));
    // What main, and all it reaches, uses, which is lowered by now.
    let used: BTreeSet<lir::DataId> = cx
        .functions
        .values()
        .flat_map(lir::Function::data_used)
        .collect();
    b.catching(handle, |b| {
        b.given_variables(&used, cx);
        b.constraints_checked_at_start(cx);
        b.insts.push(lir::Inst::Call {
            function: main,
            args: args.clone(),
            result: None,
            can_fail: true,
        });
        // What it wrote to standard output and is still held, whose failure is
        // as any of main's, in a scope for its reason.
        b.scoped(|b| {
            b.insts.push(lir::Inst::CallRuntime {
                function: lir::RuntimeFn::OutputEnds,
                args: Vec::new(),
            });
            b.scoped = true;
        });
        if let Some(main_errors) = &main_errors {
            main_errors.write(b, cx);
        }
    });
    if let Some(main_errors) = &main_errors {
        main_errors.release(&mut b, cx);
        let (failing, returning) = (b.new_block(), b.new_block());
        b.end_block(
            lir::Terminator::Branch {
                condition: main_errors.failed.clone(),
                then: failing,
                otherwise: returning,
            },
            failing,
        );
        b.end_block(lir::Terminator::Failed, returning);
    }
    b.end_block(lir::Terminator::Return, handle);
    b.catches.push(handle);
    written.write(&mut b, handle, done, cx);
    b.catches.pop();
    b.end_block(lir::Terminator::Jump(done), done);

    b.insts.push(lir::Inst::CallRuntime {
        function: lir::RuntimeFn::WriteTrace,
        args: Vec::new(),
    });
    written.release(&mut b, cx);
    if let Some(main_errors) = &main_errors {
        main_errors.release(&mut b, cx);
    }
    let after = b.new_block();
    b.end_block(lir::Terminator::Failed, after);
    let lowered = b.finish(None, cx);
    cx.functions.insert(id.0, lowered);
    id
}

/// The errors `main` gives, if it takes `out errors`, a `relation
/// transaction_collection`, which the entry writes when it returns: the
/// warnings, then the failures, each with its message and its kind's code,
/// and no site, as an error holds none, then fails if one failed.
struct MainErrors {
    /// The relation, which `main` assigns: null until it does.
    errors: lir::Place,
    /// Its type.
    ty: hir::Type,
    /// Whether one failed, a byte, 0 until a failure is written.
    failed: lir::Place,
}

impl MainErrors {
    /// Its slots, which the entry block of `b` sets as it starts: no errors, and
    /// none failed; and their address at `arg`, of `main`'s arguments.
    fn new(b: &mut FunctionBuilder, arg: lir::Place, cx: &mut Lowering) -> Self {
        let main = b.program.operator(b.program.entry);
        let ty = main.params["errors"].ty.clone();
        let layout = cx.layout(&ty);
        let errors = b.slot(layout.clone());
        let failed = b.slot(lir::Layout::U8);
        for (dst, src, layout) in [
            (errors.clone(), zero_constant(&layout, cx), layout),
            (failed.clone(), u8_constant(0, cx), lir::Layout::U8),
        ] {
            b.insts.push(lir::Inst::Copy { dst, src, layout });
        }
        b.insts.push(lir::Inst::Address {
            dst: arg,
            src: errors.clone(),
        });
        MainErrors { errors, ty, failed }
    }

    /// Write the errors, with `b` building the code: the warnings, then
    /// the failures, noting that one failed.
    fn write(&self, b: &mut FunctionBuilder, cx: &mut Lowering) {
        let hir::Type::Relation(heading) = &self.ty else {
            unreachable!("`main`'s errors are a relation");
        };
        let tuple = cx.layout(&hir::Type::Tuple(heading.clone()));
        for failures in [false, true] {
            b.each_tuple(&self.errors, tuple.clone(), cx, |b, at_hand, _, cx| {
                let field = |i| lir::Place {
                    path: [at_hand.path.clone(), vec![i]].concat(),
                    ..at_hand.clone()
                };
                // `error`, then `failed`, in name order.
                let (error, failed) = (field(0), field(1));
                let (written, next) = (b.new_block(), b.new_block());
                let (then, otherwise) = if failures {
                    (written, next)
                } else {
                    (next, written)
                };
                b.end_block(
                    lir::Terminator::Branch {
                        condition: failed.clone(),
                        then,
                        otherwise,
                    },
                    written,
                );
                b.scoped(|b| {
                    let message = b.message_of(&error, cx);
                    b.insts.push(lir::Inst::CallRuntime {
                        function: lir::RuntimeFn::Report,
                        args: vec![message, error, failed.clone()],
                    });
                });
                if failures {
                    b.insts.push(lir::Inst::Copy {
                        dst: self.failed.clone(),
                        src: u8_constant(1, cx),
                        layout: lir::Layout::U8,
                    });
                }
                b.end_block(lir::Terminator::Jump(next), next);
            });
        }
    }

    /// Give up the references the errors hold, with `b` building the code.
    fn release(&self, b: &mut FunctionBuilder, cx: &mut Lowering) {
        let descriptor = lir::Place::whole(lir::Base::Data(cx.descriptor(&self.ty)));
        b.insts.push(lir::Inst::CallRuntime {
            function: lir::RuntimeFn::Release,
            args: vec![self.errors.clone(), descriptor],
        });
    }
}

/// What the entry writes an error waiting with: where it waits, the scope
/// `main` is called in, which none of the message's statement's outlast,
/// and the error being written, taken from where it waits, so that one a
/// failing `message` leaves there is told apart.
struct Written {
    waiting: Waiting,
    outer: lir::Place,
    held: lir::Place,
    layout: lir::Layout,
    zero: lir::Place,
}

impl Written {
    /// Its slots, which the entry block of `b` sets as it starts: the
    /// scope it is called in, and no error held.
    fn new(b: &mut FunctionBuilder, waiting: Waiting, cx: &mut Lowering) -> Self {
        let outer = b.slot(lir::Layout::Address);
        b.insts.push(lir::Inst::CallRuntime {
            function: lir::RuntimeFn::Innermost,
            args: vec![outer.clone()],
        });
        let layout = cx.layout(&waiting.ty);
        let held = b.slot(layout.clone());
        let zero = zero_constant(&layout, cx);
        b.insts.push(lir::Inst::Copy {
            dst: held.clone(),
            src: zero.clone(),
            layout: layout.clone(),
        });
        Written {
            waiting,
            outer,
            held,
            layout,
            zero,
        }
    }

    /// At `handle`, where `main`'s failures go, with `b` building it:
    /// write the error waiting, if there is one, and go to `done`, which
    /// writes the trace. Each failure on the way goes to `handle` again.
    fn write(
        &self,
        b: &mut FunctionBuilder,
        handle: lir::BlockId,
        done: lir::BlockId,
        cx: &mut Lowering,
    ) {
        b.insts.push(lir::Inst::CallRuntime {
            function: lir::RuntimeFn::EndSince,
            args: vec![self.outer.clone()],
        });
        let error = self.waiting.error();
        let waits = b.is_of(&error, &self.waiting.ty, cx);
        let take = b.new_block();
        b.end_block(
            lir::Terminator::Branch {
                condition: waits,
                then: take,
                otherwise: done,
            },
            take,
        );
        self.release(b, cx);
        for (dst, src) in [
            (self.held.clone(), error.clone()),
            (error, self.zero.clone()),
        ] {
            b.insts.push(lir::Inst::Copy {
                dst,
                src,
                layout: self.layout.clone(),
            });
        }
        // The calls the error's failure passed through, which a failing
        // `message`'s replace.
        let traced = b.slot(lir::Layout::Usize);
        b.insts.push(lir::Inst::CallRuntime {
            function: lir::RuntimeFn::Traced,
            args: vec![traced.clone()],
        });
        let unwritten = b.new_block();
        b.scoped(|this| {
            let mut message = None;
            this.catching(unwritten, |this| {
                message = Some(this.message_of(&self.held, cx));
            });
            let message = message.expect("the message is computed");
            this.insts.push(lir::Inst::RestoreSite {
                src: self.waiting.site(),
            });
            let next = this.new_block();
            this.fail(
                lir::Failure::Own {
                    message,
                    error: self.held.clone(),
                },
                next,
            );
        });
        b.end_block(lir::Terminator::Jump(done), unwritten);
        b.insts.push(lir::Inst::CallRuntime {
            function: lir::RuntimeFn::TraceSince,
            args: vec![traced],
        });
        let next = b.new_block();
        b.end_block(lir::Terminator::Jump(handle), next);
    }

    /// Give up the references the error held has, with `b` building the
    /// code.
    fn release(&self, b: &mut FunctionBuilder, cx: &mut Lowering) {
        let descriptor = lir::Place::whole(lir::Base::Data(cx.descriptor(&self.waiting.ty)));
        b.insts.push(lir::Inst::CallRuntime {
            function: lir::RuntimeFn::Release,
            args: vec![self.held.clone(), descriptor],
        });
    }
}

/// The warnings of the innermost transaction with `collecting` running,
/// which it collects as it ends: the program's own variables
/// `__warnings`, a `relation transaction_collection`, and `__collecting`,
/// a boolean, whether one is running, named in the system's namespace, as
/// [`Waiting`]'s are. A transaction with `collecting` puts aside the
/// warnings of those around it as it begins, and takes its own back as it
/// ends; one without leaves them, so that its warnings go to the one
/// around it.
#[derive(Debug, Clone)]
struct Collection {
    /// `__warnings`' data object.
    warnings: lir::DataId,
    /// `__collecting`'s.
    collecting: lir::DataId,
    /// `relation transaction_collection`.
    ty: hir::Type,
    /// The prelude's type `error`.
    error: hir::Type,
}

/// What a transaction puts aside as it begins: the warnings collected so
/// far, with their references, and whether a transaction with
/// `collecting` was running.
#[derive(Clone)]
struct Aside {
    warnings: lir::Place,
    collecting: lir::Place,
}

impl Collection {
    /// The warnings' place.
    fn warnings(&self) -> lir::Place {
        lir::Place {
            base: lir::Base::Data(self.warnings),
            path: vec![0],
        }
    }

    /// The place of whether a transaction with `collecting` is running.
    fn collecting(&self) -> lir::Place {
        lir::Place {
            base: lir::Base::Data(self.collecting),
            path: vec![0],
        }
    }

    /// Put aside the warnings collected so far, and whether a transaction
    /// with `collecting` is running, as one begins, with `b` building it:
    /// the warnings go, with their references, to a slot of their own, and
    /// none are collected yet, as one is running.
    fn put_aside(&self, b: &mut FunctionBuilder, cx: &mut Lowering) -> Aside {
        let layout = cx.layout(&self.ty);
        let aside = Aside {
            warnings: b.slot(layout.clone()),
            collecting: b.slot(lir::Layout::U8),
        };
        let none = self.none(b, cx);
        let one = u8_constant(1, cx);
        for (dst, src, layout) in [
            (aside.warnings.clone(), self.warnings(), layout.clone()),
            (self.warnings(), none, layout),
            (aside.collecting.clone(), self.collecting(), lir::Layout::U8),
            (self.collecting(), one, lir::Layout::U8),
        ] {
            b.insts.push(lir::Inst::Copy { dst, src, layout });
        }
        aside
    }

    /// Take back what `aside` put aside, as the transaction ends, with `b`
    /// building it: the warnings its block collected, in a slot, with
    /// their references.
    fn take_back(&self, b: &mut FunctionBuilder, aside: &Aside, cx: &mut Lowering) -> lir::Place {
        let layout = cx.layout(&self.ty);
        let own = b.slot(layout.clone());
        for (dst, src, layout) in [
            (own.clone(), self.warnings(), layout.clone()),
            (self.warnings(), aside.warnings.clone(), layout),
            (self.collecting(), aside.collecting.clone(), lir::Layout::U8),
        ] {
            b.insts.push(lir::Inst::Copy { dst, src, layout });
        }
        own
    }

    /// No warnings, a constant.
    fn none(&self, b: &mut FunctionBuilder, cx: &mut Lowering) -> lir::Place {
        let hir::Type::Relation(heading) = &self.ty else {
            unreachable!("a transaction's collection is a relation");
        };
        let none = hir::Value::Relation(hir::RelationValue {
            heading: heading.clone(),
            tuples: BTreeSet::new(),
        });
        b.place(&hir::Expr::Constant(none), cx)
    }

    /// The relation of one tuple of a transaction's collection, of the
    /// error at `error` and `failed`, in a slot, in memory the statement
    /// owns, with `b` building it.
    fn one(
        &self,
        b: &mut FunctionBuilder,
        error: &lir::Place,
        failed: bool,
        cx: &mut Lowering,
    ) -> lir::Place {
        let hir::Type::Relation(heading) = &self.ty else {
            unreachable!("a transaction's collection is a relation");
        };
        let tuple = b.slot(cx.layout(&hir::Type::Tuple(heading.clone())));
        let (mut attribute, mut flag) = (tuple.clone(), tuple.clone());
        attribute.path.push(0);
        flag.path.push(1);
        b.insts.push(lir::Inst::Copy {
            dst: attribute,
            src: error.clone(),
            layout: cx.layout(&self.error),
        });
        b.insts.push(lir::Inst::Copy {
            dst: flag,
            src: u8_constant(u8::from(failed), cx),
            layout: lir::Layout::U8,
        });
        let one = b.slot(cx.layout(&self.ty));
        let descriptor = lir::Place::whole(lir::Base::Data(cx.descriptor(&self.ty)));
        let count = usize_constant(1, cx);
        b.insts.push(lir::Inst::CallRuntime {
            function: lir::RuntimeFn::Relation,
            args: vec![one.clone(), tuple, count, descriptor],
        });
        b.scoped = true;
        one
    }
}

/// Where a failure's error waits for what takes it, a transaction or the
/// entry, which writes it: the program's own variable `__failure`, of the
/// prelude's type `error`, null when no error waits, and `__failure_site`,
/// the site it failed at. A `fail` puts its error there, and the runtime
/// the language's ([`Lowering::failures`]). Their names are in the
/// system's namespace, which no program can name.
#[derive(Debug, Clone)]
struct Waiting {
    /// `__failure`'s data object.
    error: lir::DataId,
    /// `__failure_site`'s.
    site: lir::DataId,
    /// The prelude's type `error`.
    ty: hir::Type,
}

impl Waiting {
    /// The error's place.
    fn error(&self) -> lir::Place {
        lir::Place {
            base: lir::Base::Data(self.error),
            path: vec![0],
        }
    }

    /// The site's place.
    fn site(&self) -> lir::Place {
        lir::Place {
            base: lir::Base::Data(self.site),
            path: vec![0],
        }
    }
}

/// The function of the operator `id` instantiated with `types`, lowered
/// when first called, and whether a call of it makes a text, relation or
/// array that the calling statement owns, as a read-only operator's can.
///
/// An update operator's function runs each statement in a scope of its
/// own. A read-only operator's computes its value, then stores it where the
/// caller says, as a `result`; it begins no scope, so what it makes is the
/// calling statement's. Whether one makes something is known once its function
/// is lowered, before the calling statement ends, unless it calls itself: one
/// in a cycle of calls, which is lowered while it is being lowered, always
/// makes what its result points to, as the call it ends in may have.
fn function(
    program: &hir::Program,
    variable_data: &[lir::DataId],
    id: hir::OperatorId,
    types: &[hir::Type],
    cx: &mut Lowering,
) -> (lir::FunctionId, bool) {
    let key = (id, types.to_vec());
    if let Some(&lowered) = cx.functions_of.get(&key) {
        return lowered;
    }
    let function_id = cx.number();
    let op = program.operator(id);
    let hir::Operator {
        name, params, kind, ..
    } = if types.is_empty() {
        op.clone()
    } else {
        op.instantiated(types)
    };
    // Instances of one operator are told apart by their parameters' types,
    // and operators of one name by their parameters' names.
    let overloaded = program.operators.iter().filter(|o| o.name == name).count() > 1;
    let name = if types.is_empty() && !overloaded {
        name
    } else {
        format!("{name}{}", params_text(&params))
    };
    // Found before its body is lowered, so that a call of itself finds it.
    let cycle = cx.recursion.cycle(id);
    let hands_out = cycle.is_some()
        && matches!(&kind, hir::OperatorKind::ReadOnly { result, .. } if cx.points(result));
    cx.functions_of
        .insert(key.clone(), (function_id, hands_out));
    let mut builder = FunctionBuilder::new(program, name, params, variable_data);
    if let Some(cycle) = cycle {
        builder.in_cycle(cycle, cx);
    }
    let (lowered, makes) = match kind {
        hir::OperatorKind::Update { locals, body } => {
            builder.declare(locals, cx);
            builder.returning(body.returns(), None, |builder| {
                builder.tail_block(&body, cx);
            });
            builder.give_up_arguments(cx);
            (builder.finish(None, cx), false)
        }
        hir::OperatorKind::ReadOnly {
            result,
            locals,
            value,
        } => builder.valued(locals, &value, &result, cx),
        hir::OperatorKind::Builtin(_) => unreachable!("built-ins are runtime calls"),
        hir::OperatorKind::Specification { .. } => {
            unreachable!("a specification's versions are called instead")
        }
    };
    // One that can't fail has no use for its sites, so a call of it needs
    // nothing for a failure, nor to go back to its own site.
    let lowered = if lowered.can_fail() {
        lowered
    } else {
        cx.unfailing.insert(function_id.0);
        lowered.without_sites()
    };
    cx.functions.insert(function_id.0, lowered);
    cx.functions_of.insert(key, (function_id, makes));
    (function_id, makes)
}

/// The place of the value in the data object `data`, which is the struct of its
/// fields, of which the value is the only one.
fn in_data(data: lir::DataId) -> lir::Place {
    lir::Place {
        base: lir::Base::Data(data),
        path: vec![0],
    }
}

/// What lowered code reads of a constant the compiler can't compute.
#[derive(Clone, Copy)]
struct ComputedConstant {
    /// Where it is kept: its value, zero until it is computed, and whether it
    /// has been.
    data: lir::DataId,
    /// The function that computes it.
    function: lir::FunctionId,
}

impl ComputedConstant {
    /// Where it says whether it has been computed.
    fn computed(self) -> lir::Place {
        lir::Place {
            base: lir::Base::Data(self.data),
            path: vec![1],
        }
    }
}

/// The constant `id`, read by lowered code, whose function is lowered here, so
/// after those of the constants it reads.
fn computed_constant(
    program: &hir::Program,
    variable_data: &[lir::DataId],
    id: hir::ConstantId,
    cx: &mut Lowering,
) -> ComputedConstant {
    if let Some(&computed) = cx.constants.get(&id) {
        return computed;
    }
    let constant = &program.constants[id.0];
    let layout = cx.layout(&constant.ty);
    let data = cx.push(lir::Data {
        kind: lir::DataKind::Variable {
            name: constant.name.clone(),
        },
        writable: true,
        fields: vec![zero_field(&layout), lir::Field::U8(0)],
    });
    let computed = ComputedConstant {
        data,
        function: cx.number(),
    };
    cx.constants.insert(id, computed);
    let name = format!("constant {}", constant.name);
    let mut builder = FunctionBuilder::new(program, name, BTreeMap::new(), variable_data);
    builder.bind(constant.locals.clone());
    builder.scoped(|this| {
        let src = this.place(&constant.value, cx);
        this.named_keys_of(&constant.name, &constant.keys, &constant.ty, &src, None, cx);
        this.assign_place(in_data(data), src, &constant.ty, cx);
        let one = u8_constant(1, cx);
        this.insts.push(lir::Inst::Copy {
            dst: computed.computed(),
            src: one,
            layout: lir::Layout::U8,
        });
    });
    let lowered = builder.finish(None, cx);
    cx.functions.insert(computed.function.0, lowered);
    computed
}

/// A part of a constraint, which lowered code checks with a function of
/// its own: the part's condition, a boolean, or, if `tuples`, the
/// relation of the tuples that make it false, as `debug_value` writes it.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct Checked {
    /// The constraint, by its place in [`hir::Program::constraints`].
    constraint: usize,
    /// The part, by its place in [`hir::VariableConstraint::parts`].
    part: usize,
    tuples: bool,
}

/// The function computing `checked`, as [`function`] makes an operator's,
/// with no parameters: it reads the program's variables.
fn constraint_function(
    program: &hir::Program,
    variable_data: &[lir::DataId],
    checked: Checked,
    cx: &mut Lowering,
) -> (lir::FunctionId, bool) {
    if let Some(&lowered) = cx.constraint_functions.get(&checked) {
        return lowered;
    }
    let function_id = cx.number();
    let constraint = &program.constraints[checked.constraint];
    let part = &constraint.parts[checked.part];
    // Named, as a failure's trace writes it, by its constraint, its part,
    // if it has several, and what it computes.
    let which = if constraint.parts.len() > 1 {
        format!(", part {}", checked.part + 1)
    } else {
        String::new()
    };
    let tuples = if checked.tuples { ", its tuples" } else { "" };
    let name = format!("constraint {}{which}{tuples}", constraint.name);
    let builder = FunctionBuilder::new(program, name, BTreeMap::new(), variable_data);
    let (lowered, makes) = match (&part.breaking, checked.tuples) {
        (Some(breaking), true) => builder.written(part.locals.clone(), breaking, cx),
        (None, true) => unreachable!("only a part whose form says which tuples break it has them"),
        (_, false) => builder.valued(
            part.locals.clone(),
            &part.condition,
            &hir::Type::Scalar(hir::Scalar::Boolean),
            cx,
        ),
    };
    cx.functions.insert(function_id.0, lowered);
    cx.constraint_functions
        .insert(checked, (function_id, makes));
    (function_id, makes)
}

/// Parameters as written, `{name: text, times: integer}`, with their
/// types in full, so that different types are written differently.
fn params_text(params: &BTreeMap<String, hir::Parameter>) -> String {
    format!(
        "{{{}}}",
        attributes_text(params.iter().map(|(name, p)| (name, &p.ty)))
    )
}

/// Attributes, `name: type, …`, with their types in full: a wrapped
/// attribute too, which a type's source text leaves out.
fn attributes_text<'t>(attributes: impl Iterator<Item = (&'t String, &'t hir::Type)>) -> String {
    let written: Vec<String> = attributes
        .map(|(name, ty)| format!("{name}: {}", type_text(ty)))
        .collect();
    written.join(", ")
}

/// A type's text in full, as for [`attributes_text`].
fn type_text(ty: &hir::Type) -> String {
    let heading = |h: &hir::Heading| {
        let attributes: Vec<(String, hir::Type)> = h
            .attributes()
            .map(|(name, ty)| (name.to_string(), ty.clone()))
            .collect();
        attributes_text(attributes.iter().map(|(name, ty)| (name, ty)))
    };
    match ty {
        hir::Type::Tuple(h) => format!("tuple {{ {} }}", heading(h)),
        hir::Type::Relation(h) => format!("relation {{ {} }}", heading(h)),
        hir::Type::Array(element) => format!("array {}", type_text(element)),
        _ => ty.to_string(),
    }
}

/// The program-wide part of lowering: the data objects, and the functions.
struct Lowering {
    /// The operators a call of which reads when a transaction began, so that a
    /// transaction that calls one has to read the clock as it begins.
    reading_transaction_start: BTreeSet<hir::OperatorId>,
    /// How values are laid out.
    families: Families,
    data: Vec<lir::Data>,
    /// Each type's descriptor, built once.
    descriptors: HashMap<hir::Type, lir::DataId>,
    /// Each sort order other than by name some relation is sorted by, by its [`SortId`],
    /// and each one's id.
    sort_orders: Vec<Vec<String>>,
    sort_ids: HashMap<Vec<String>, SortId>,
    /// The descriptor of each relation type sorted by each such order, built once.
    sorted_descriptors: HashMap<(hir::Type, SortId), lir::DataId>,
    /// Each list of positions a descriptor's sort order has, made once, so that one sort
    /// order is one data object, whatever its attributes are named
    /// ([`lir::Arrangement::Sorted`]).
    sort_positions: HashMap<Vec<u64>, lir::DataId>,
    /// An empty constant, for addresses that are never read.
    empty: Option<lir::DataId>,
    /// The functions lowered, by id, which counts up from 0: one is added
    /// once its body is lowered, after those it calls.
    functions: BTreeMap<usize, lir::Function>,
    /// The function of each operator and list of types lowered code calls
    /// it with, and whether a call of it makes a text, relation or array.
    functions_of: HashMap<(hir::OperatorId, Vec<hir::Type>), (lir::FunctionId, bool)>,
    /// The functions lowered, by id, that can't fail, and so set no site: a
    /// call of one needs nothing for a failure (see [`lir::Function::can_fail`]).
    unfailing: BTreeSet<usize>,
    /// The function of each constraint's part that lowered code checks,
    /// and whether a call of it makes a text, relation or array.
    constraint_functions: HashMap<Checked, (lir::FunctionId, bool)>,
    /// Each of the program's files' path, by its [`hir::FileId`], as a
    /// failure writes it.
    files: Vec<String>,
    /// Each site's data object, made once.
    sites: HashMap<hir::Site, lir::DataId>,
    /// Each file's path, as a text, made once.
    paths: HashMap<hir::FileId, lir::Field>,
    /// Which operators call themselves, directly or through others.
    recursion: hir::Recursion,
    /// Each constant lowered code reads.
    constants: HashMap<hir::ConstantId, ComputedConstant>,
    /// Where the arguments of a tail call of each function wait, as the
    /// frame they were made in goes, until the function copies them.
    tail_arguments: HashMap<lir::FunctionId, lir::DataId>,
    /// How many functions are numbered.
    numbered: usize,
    /// Where a failure's error waits, once a `fail` or the entry is
    /// lowered.
    waiting: Option<Waiting>,
    /// The warnings of the innermost transaction running, once a
    /// transaction or a `warn` is lowered.
    collection: Option<Collection>,
}

impl Lowering {
    /// For the program whose files' paths are `files`, with the types'
    /// immediate subtypes `subtypes`.
    fn new(
        files: &[String],
        subtypes: &BTreeMap<String, Vec<hir::Type>>,
        bounds: HashMap<hir::OperatorId, IntegerBounds>,
    ) -> Self {
        Lowering {
            reading_transaction_start: BTreeSet::new(),
            families: Families::new(subtypes, bounds),
            data: Vec::new(),
            descriptors: HashMap::new(),
            sort_orders: Vec::new(),
            sort_ids: HashMap::new(),
            sorted_descriptors: HashMap::new(),
            sort_positions: HashMap::new(),
            empty: None,
            functions: BTreeMap::new(),
            functions_of: HashMap::new(),
            unfailing: BTreeSet::new(),
            constraint_functions: HashMap::new(),
            files: files.to_vec(),
            sites: HashMap::new(),
            paths: HashMap::new(),
            recursion: hir::Recursion::default(),
            constants: HashMap::new(),
            tail_arguments: HashMap::new(),
            numbered: 0,
            waiting: None,
            collection: None,
        }
    }

    /// Where the warnings of the innermost transaction running are, made
    /// when first needed: none, and none running.
    fn collection(&mut self, program: &hir::Program) -> Collection {
        if let Some(collection) = &self.collection {
            return collection.clone();
        }
        let error = program.operator(program.message).params["error"].ty.clone();
        let heading = hir::transaction_collection(error.clone());
        let none = hir::Value::Relation(hir::RelationValue {
            heading: heading.clone(),
            tuples: BTreeSet::new(),
        });
        let field = self.value_field(&none);
        let mut variable = |name: &str, field| {
            self.push(lir::Data {
                kind: lir::DataKind::Variable {
                    name: name.to_string(),
                },
                writable: true,
                fields: vec![field],
            })
        };
        let warnings = variable("__warnings", field);
        let collecting = variable("__collecting", lir::Field::U8(0));
        let collection = Collection {
            warnings,
            collecting,
            ty: hir::Type::Relation(heading),
            error,
        };
        self.collection = Some(collection.clone());
        collection
    }

    /// The next function's id: they count up from 0.
    fn number(&mut self) -> lir::FunctionId {
        self.numbered += 1;
        lir::FunctionId(self.numbered - 1)
    }

    /// Where a `fail`'s error waits, made when first needed, null.
    fn waiting(&mut self, program: &hir::Program) -> Waiting {
        if let Some(waiting) = &self.waiting {
            return waiting.clone();
        }
        let ty = program.operator(program.message).params["error"].ty.clone();
        let field = zero_field(&self.layout(&ty));
        let mut variable = |name: &str, field| {
            self.push(lir::Data {
                kind: lir::DataKind::Variable {
                    name: name.to_string(),
                },
                writable: true,
                fields: vec![field],
            })
        };
        let error = variable("__failure", field);
        let site = variable("__failure_site", lir::Field::Usize(0));
        let waiting = Waiting { error, site, ty };
        self.waiting = Some(waiting.clone());
        waiting
    }

    /// The table of the errors the language makes, which the entry gives
    /// the runtime, laid out as its `Failures`: the addresses of where a
    /// failure's error waits and its site ([`Waiting`]), the descriptor of
    /// the prelude's type `error`, and of an array of each kind's, in
    /// [`hir::LanguageError::ALL`]'s order, with their count. A kind, the
    /// runtime's `Kind`, is its type's name, its components' descriptor,
    /// and its code, as texts.
    fn failures(&mut self, program: &hir::Program) -> lir::DataId {
        let waiting = self.waiting(program);
        let mut kinds = Vec::new();
        for error in hir::LanguageError::ALL {
            let name = self.value_field(&hir::Value::Text(error.name().to_string()));
            let components = self.components_descriptor(program.language_error(error));
            let code = self.value_field(&hir::Value::Text(error.code().id().to_string()));
            kinds.extend([name, lir::Field::Address(components), code]);
        }
        let kinds = self.constant(kinds);
        let error = self.descriptor(&waiting.ty);
        self.constant(vec![
            lir::Field::Address(waiting.error),
            lir::Field::Address(waiting.site),
            lir::Field::Address(error),
            lir::Field::Address(kinds),
            lir::Field::Usize(len(hir::LanguageError::ALL.len())),
        ])
    }

    /// The layout of a value of type `ty`.
    fn layout(&self, ty: &hir::Type) -> lir::Layout {
        self.families.layout(ty)
    }

    /// Whether a value of type `ty` points to memory (see
    /// [`Families::points`]).
    fn points(&self, ty: &hir::Type) -> bool {
        self.families.points(ty)
    }

    /// The data object of `site`, laid out as `trendy_runtime::Site`: the
    /// path of its file, as a text, then its line and column.
    fn site(&mut self, site: hir::Site) -> lir::DataId {
        if let Some(&id) = self.sites.get(&site) {
            return id;
        }
        let path = self.path_of(site.file);
        let id = self.constant(vec![
            path,
            lir::Field::Usize(len(site.line)),
            lir::Field::Usize(len(site.column)),
        ]);
        self.sites.insert(site, id);
        id
    }

    /// The path of `file`, as a text.
    fn path_of(&mut self, file: hir::FileId) -> lir::Field {
        if let Some(path) = self.paths.get(&file) {
            return path.clone();
        }
        let text = self.files[file.0].clone();
        let path = self.value_field(&hir::Value::Text(text));
        self.paths.insert(file, path.clone());
        path
    }

    /// Where the arguments of a tail call of `function`, laid out as `layout`,
    /// wait, made when first needed. One place does for all its calls, as they
    /// are there only from before the caller gives up its frame until the
    /// function starts, and nothing else runs in that time.
    fn tail_arguments(&mut self, function: lir::FunctionId, layout: &lir::Layout) -> lir::Place {
        let data = if let Some(&data) = self.tail_arguments.get(&function) {
            data
        } else {
            let data = self.push(lir::Data {
                kind: lir::DataKind::Constant,
                writable: true,
                fields: vec![zero_field(layout)],
            });
            self.tail_arguments.insert(function, data);
            data
        };
        lir::Place {
            base: lir::Base::Data(data),
            path: vec![0],
        }
    }

    fn push(&mut self, data: lir::Data) -> lir::DataId {
        self.data.push(data);
        lir::DataId(self.data.len() - 1)
    }

    fn constant(&mut self, fields: Vec<lir::Field>) -> lir::DataId {
        self.push(lir::Data {
            kind: lir::DataKind::Constant,
            writable: false,
            fields,
        })
    }

    /// The address of a new constant buffer of `contents`, after a header
    /// that says it is never released, so that sharing it costs nothing.
    fn buffer(&mut self, contents: lir::Field) -> lir::Field {
        let buffer = self.constant(vec![lir::Field::immortal_header(), contents]);
        lir::Field::FieldAddress(buffer, 1)
    }

    /// The field representing `value`, adding any constants it points to.
    fn value_field(&mut self, value: &hir::Value) -> lir::Field {
        match value {
            hir::Value::Boolean(b) => lir::Field::U8(u8::from(*b)),
            hir::Value::Integer(n) => lir::Field::I64(*n),
            hir::Value::Approximate(r) => lir::Field::F64Bits(r.to_bits()),
            hir::Value::Text(text) | hir::Value::Character(text) => {
                let bytes = self.buffer(lir::Field::Bytes(text.as_bytes().to_vec()));
                lir::Field::Struct(vec![bytes, lir::Field::Usize(len(text.len()))])
            }
            // A value of a type in a hierarchy: its own type's descriptor's
            // address, then its components, in its type's variant of the
            // room for them.
            hir::Value::Defined(value) if hir::Type::Defined(value.ty.clone()).root().is_some() => {
                let own = self.components_descriptor(&value.ty);
                let room = self.layout(&hir::Type::Defined(value.ty.clone()));
                let lir::Layout::Struct(fields) = room else {
                    unreachable!("a value in a hierarchy is laid out as a struct");
                };
                let components = self.value_field(&hir::Value::Tuple(value.components.clone()));
                lir::Field::Struct(vec![
                    lir::Field::Address(own),
                    lir::Field::Variant(fields[1].clone(), Box::new(components)),
                ])
            }
            // Its integers its constraints let it store narrower, so.
            hir::Value::Defined(value) if let Some(stored) = self.families.stored(&value.ty) => {
                let fields = stored
                    .attributes()
                    .zip(value.components.attributes())
                    .map(|((_, ty), (_, component))| match (ty, component) {
                        (hir::Type::Scalar(hir::Scalar::Narrow(width)), hir::Value::Integer(n)) => {
                            narrow_field(*width, *n)
                        }
                        _ => self.value_field(component),
                    })
                    .collect();
                lir::Field::Struct(fields)
            }
            // Attributes come in name order, the heading's order, and a
            // value of any other type of the program's own is laid out as
            // its components.
            hir::Value::Tuple(tuple)
            | hir::Value::Defined(hir::DefinedValue {
                components: tuple, ..
            }) => lir::Field::Struct(
                tuple
                    .attributes()
                    .map(|(_, value)| self.value_field(value))
                    .collect(),
            ),
            hir::Value::Relation(relation) => self.relation_field(relation, Order::Canonical),
            hir::Value::Array(array) => {
                let elements = array.elements.iter().map(|e| self.value_field(e)).collect();
                let elements = self.buffer(lir::Field::Struct(elements));
                lir::Field::Struct(vec![elements, lir::Field::Usize(len(array.elements.len()))])
            }
        }
    }

    /// The value `relation`, as [`Lowering::value_field`] lays it out, with its tuples in a
    /// read-only array, sorted in `order`, a sorted one: in canonical order, as `hir::Value`
    /// orders them, or by the attributes of another, each as `hir::Value` orders its values,
    /// as the runtime does.
    fn relation_field(&mut self, relation: &hir::RelationValue, order: Order) -> lir::Field {
        let mut tuples: Vec<&hir::TupleValue> = relation.tuples.iter().collect();
        if let Order::SortedBy(_) = order {
            let names = self.sort_names(order, &relation.heading);
            tuples.sort_by(|a, b| {
                names
                    .iter()
                    .map(|name| a.get(name).cmp(&b.get(name)))
                    .find(|ordering| ordering.is_ne())
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
        }
        let elements = tuples
            .into_iter()
            .map(|t| self.value_field(&hir::Value::Tuple(t.clone())))
            .collect();
        let tuples = self.buffer(lir::Field::Struct(elements));
        lir::Field::Struct(vec![tuples, lir::Field::Usize(len(relation.tuples.len()))])
    }

    /// The descriptor of `ty`, laid out as `trendy_runtime::TypeDesc`:
    /// `{ kind: u8, points: u8, integers, size, attributes: *Attr, count,
    /// sorted_by: *usize, element: *TypeDesc, name: *u8, name_len, written: *u8,
    /// written_len }`. A relation's tuples are sorted by its attributes as
    /// listed, by name, so `sorted_by` is null, as it is for every type but a relation
    /// sorted otherwise ([`Lowering::described`]).
    fn descriptor(&mut self, ty: &hir::Type) -> lir::DataId {
        if let Some(&id) = self.descriptors.get(ty) {
            return id;
        }
        if ty.root().is_some() {
            return self.hierarchy_descriptor(ty);
        }
        let stored;
        let (kind, heading, element) = match ty {
            hir::Type::Scalar(hir::Scalar::Boolean) => (KIND_BOOLEAN, None, None),
            hir::Type::Scalar(hir::Scalar::Narrow(width)) => {
                let kind = match width {
                    hir::Width::U8 => KIND_U8,
                    hir::Width::U16 => KIND_U16,
                    hir::Width::U32 => KIND_U32,
                };
                (kind, None, None)
            }
            hir::Type::Defined(defined) => {
                stored = self.families.stored(defined);
                let heading = stored.as_ref().unwrap_or(&defined.components);
                (KIND_DEFINED, Some(heading), None)
            }
            hir::Type::Scalar(hir::Scalar::Integer) => (KIND_INTEGER, None, None),
            hir::Type::Scalar(hir::Scalar::Approximate) => (KIND_APPROXIMATE, None, None),
            hir::Type::Scalar(hir::Scalar::Text) => (KIND_TEXT, None, None),
            hir::Type::Scalar(hir::Scalar::Character) => (KIND_CHARACTER, None, None),
            hir::Type::Tuple(heading) => (KIND_TUPLE, Some(heading), None),
            hir::Type::Relation(heading) => (
                KIND_RELATION,
                Some(heading),
                Some(hir::Type::Tuple(heading.clone())),
            ),
            hir::Type::Array(element) => (KIND_ARRAY, None, Some((**element).clone())),
            hir::Type::Dummy(_) => unreachable!("a dummy type is in a hierarchy"),
            hir::Type::Parameter(_) => unreachable!("functions are of instantiated operators"),
        };
        let (attributes, count) = match heading {
            Some(heading) => (self.attributes(heading), heading.attributes().count()),
            None => (self.empty(), 0),
        };
        // A relation's tuples' type, or an array's elements', or null.
        let element = element.map_or(lir::Field::Usize(0), |element| {
            lir::Field::Address(self.descriptor(&element))
        });
        // A type of the program's own's name, and the name its values are
        // written with, or none.
        let (name, written) = match ty {
            hir::Type::Defined(defined) => (defined.name.as_str(), defined.written()),
            _ => ("", ""),
        };
        let (name, written) = (self.name(name), self.name(written));
        let points = self.points(ty);
        let integers = match ty {
            hir::Type::Relation(heading) => {
                integers(&hir::Type::Tuple(heading.clone()), &self.families)
            }
            _ => integers(ty, &self.families),
        };
        let id = self.constant(vec![
            lir::Field::U8(kind),
            lir::Field::U8(u8::from(points)),
            lir::Field::Usize(len(integers)),
            lir::Field::Size(self.layout(ty)),
            lir::Field::Address(attributes),
            lir::Field::Usize(len(count)),
            lir::Field::Usize(0),
            element,
            name.0,
            name.1,
            written.0,
            written.1,
        ]);
        self.descriptors.insert(ty.clone(), id);
        id
    }

    /// The descriptor of `ty`, a type in a hierarchy, whose values are each
    /// the address of its own type's descriptor (see
    /// [`Lowering::components_descriptor`]) and then its components: a
    /// `KIND_UNION` one, with one attribute, with no name or type, at where
    /// the components are.
    fn hierarchy_descriptor(&mut self, ty: &hir::Type) -> lir::DataId {
        let layout = self.layout(ty);
        let empty = self.empty();
        let components = self.constant(vec![lir::Field::Struct(vec![
            lir::Field::Address(empty),
            lir::Field::Usize(0),
            lir::Field::Offset(layout.clone(), vec![1]),
            lir::Field::Usize(0),
        ])]);
        let name = self.name(&ty.to_string());
        let written = self.name("");
        let points = self.points(ty);
        let id = self.constant(vec![
            lir::Field::U8(KIND_UNION),
            lir::Field::U8(u8::from(points)),
            lir::Field::Usize(0),
            lir::Field::Size(layout),
            lir::Field::Address(components),
            lir::Field::Usize(1),
            lir::Field::Usize(0),
            lir::Field::Usize(0),
            name.0,
            name.1,
            written.0,
            written.1,
        ]);
        self.descriptors.insert(ty.clone(), id);
        id
    }

    /// The order of a relation's tuples sorted by the attributes `names`, every one of
    /// them, most significant first: canonical, if that is by name.
    fn sorted_by(&mut self, names: Vec<String>) -> Order {
        if names.is_sorted() {
            return Order::Canonical;
        }
        if let Some(&id) = self.sort_ids.get(&names) {
            return Order::SortedBy(id);
        }
        let id = SortId(self.sort_orders.len());
        self.sort_orders.push(names.clone());
        self.sort_ids.insert(names, id);
        Order::SortedBy(id)
    }

    /// The names of the attributes a relation of `heading` in `order`, which is sorted,
    /// is sorted by, most significant first.
    fn sort_names(&self, order: Order, heading: &hir::Heading) -> Vec<String> {
        match order {
            Order::SortedBy(id) => self.sort_orders[id.0].clone(),
            Order::Canonical => heading.attributes().map(|(a, _)| a.to_string()).collect(),
            Order::Loose | Order::Repeated => {
                unreachable!("only a sorted relation has a sort order")
            }
        }
    }

    /// The descriptor of a relation of type `ty` whose tuples are in `order`: its type's,
    /// unless they are sorted otherwise than by name, which its sort order then says. The
    /// layout is the type's, by name, so its tuples' integers, if that is all they are, are
    /// compared through the sort order, not as a run of them.
    fn described(&mut self, ty: &hir::Type, order: Order) -> lir::DataId {
        let Order::SortedBy(id) = order else {
            return self.descriptor(ty);
        };
        if let Some(&described) = self.sorted_descriptors.get(&(ty.clone(), id)) {
            return described;
        }
        let hir::Type::Relation(heading) = ty else {
            unreachable!("only a relation is sorted");
        };
        let positions: Vec<u64> = self.sort_orders[id.0]
            .iter()
            .map(|name| {
                let position = heading
                    .attributes()
                    .position(|(a, _)| a == name)
                    .expect("a sort order names the relation's attributes");
                len(position)
            })
            .collect();
        let positions = if let Some(&made) = self.sort_positions.get(&positions) {
            made
        } else {
            let fields = positions.iter().map(|&p| lir::Field::Usize(p)).collect();
            let made = self.constant(fields);
            self.sort_positions.insert(positions, made);
            made
        };
        let by_name = self.descriptor(ty);
        let mut fields = self.data[by_name.0].fields.clone();
        fields[DESCRIPTOR_INTEGERS] = lir::Field::Usize(0);
        fields[lir::SORTED_BY_FIELD] = lir::Field::Address(positions);
        let described = self.constant(fields);
        self.sorted_descriptors.insert((ty.clone(), id), described);
        described
    }

    /// The order of what `view` reads, of what it reads through its renames in `order`,
    /// which is sorted: sorted by the same attributes, as they are read.
    fn renamed_order(&mut self, order: Order, view: &View) -> Order {
        let hir::Type::Relation(heading) = &view.ty else {
            unreachable!("only a relation is sorted");
        };
        let read_as: BTreeMap<&str, &str> = view
            .shape
            .names
            .iter()
            .map(|(read, own)| (own.as_str(), read.as_str()))
            .collect();
        let names = self
            .sort_names(order, heading)
            .iter()
            .map(|own| read_as[own.as_str()].to_string())
            .collect();
        self.sorted_by(names)
    }

    /// The order of what `view` reads, of what it reads through its renames in `order`:
    /// sorted by the same attributes, as they are read, if it is sorted.
    fn read_order(&mut self, order: Order, view: &View) -> Order {
        match order {
            Order::Canonical | Order::SortedBy(_) => self.renamed_order(order, view),
            Order::Loose | Order::Repeated => order,
        }
    }

    /// The order a join of what `lhs` and `rhs` read, in `lhs_order` and `rhs_order`, is
    /// made in where a sorted one will do: by its common attributes, which the runtime
    /// merges it on, in the order it compares them, then the others of each operand, in
    /// its order, as each run of tuples that agree on them pairs one of the left's, in its
    /// order, with each of the right's, in its (see `trendy_runtime::Matches`). With none in
    /// common, that is the left's order, then the right's, as a product is written. One
    /// that reads a much larger operand as it is, unsorted, or of a loose operand, sorts its
    /// tuples into it, which costs no more than into canonical order. Of a loose operand,
    /// whose runs are in no particular order, it is canonical order.
    fn joined_order(
        &mut self,
        lhs: &View,
        rhs: &View,
        lhs_order: Order,
        rhs_order: Order,
    ) -> Order {
        if [lhs_order, rhs_order]
            .iter()
            .any(|order| matches!(order, Order::Loose | Order::Repeated))
        {
            return Order::Canonical;
        }
        let read = |this: &mut Lowering, order: Order, view: &View| {
            let read_order = this.read_order(order, view);
            let hir::Type::Relation(heading) = &view.named else {
                unreachable!("only a relation is sorted");
            };
            this.sort_names(read_order, heading)
        };
        let common: Vec<String> = lhs
            .shape
            .names
            .keys()
            .filter(|name| rhs.shape.names.contains_key(*name))
            .cloned()
            .collect();
        let mut names = common.clone();
        for name in read(self, lhs_order, lhs)
            .into_iter()
            .chain(read(self, rhs_order, rhs))
        {
            if !names.contains(&name) {
                names.push(name);
            }
        }
        self.sorted_by(names)
    }

    /// The order of a relation of `heading` sorted by `leading` first, then by its other
    /// attributes, by name.
    fn leading_order(&mut self, leading: &[String], heading: &hir::Heading) -> Order {
        let mut names = leading.to_vec();
        for (name, _) in heading.attributes() {
            if !leading.iter().any(|first| first == name) {
                names.push(name.to_string());
            }
        }
        self.sorted_by(names)
    }

    /// Whether a relation of `heading` in `order` is sorted by `leading` first.
    fn leads_with(&self, order: Order, heading: &hir::Heading, leading: &[String]) -> bool {
        matches!(order, Order::Canonical | Order::SortedBy(_))
            && self.sort_names(order, heading).starts_with(leading)
    }

    /// The order of a relation of `heading` in `order`, a sorted one, extended with
    /// `added`, attributes it hasn't: sorted by the same attributes, then those it adds, by
    /// name, as an extension's tuples are made in its operand's order, all different.
    fn extended_order(
        &mut self,
        order: Order,
        heading: &hir::Heading,
        added: &BTreeMap<String, hir::Expr>,
    ) -> Order {
        let mut names = self.sort_names(order, heading);
        names.extend(added.keys().cloned());
        self.sorted_by(names)
    }

    /// The fields of a descriptor that hold `name`: the address of its
    /// UTF-8 bytes, and their length.
    fn name(&mut self, name: &str) -> (lir::Field, lir::Field) {
        let bytes = if name.is_empty() {
            self.empty()
        } else {
            self.constant(vec![lir::Field::Bytes(name.as_bytes().to_vec())])
        };
        (
            lir::Field::Address(bytes),
            lir::Field::Usize(len(name.len())),
        )
    }

    /// The descriptor of the components of a value of `defined`, as its
    /// selector writes them: a `KIND_DEFINED` one, as of a value of the
    /// type outside a hierarchy, which is laid out as its components. In a
    /// hierarchy, a value starts with this one's address, as its own type.
    fn components_descriptor(&mut self, defined: &hir::Defined) -> lir::DataId {
        // With no constraints to store them narrower by, as they are given.
        let alone = hir::Defined {
            supertypes: Vec::new(),
            specialized: false,
            constraints: Vec::new(),
            ..defined.clone()
        };
        self.descriptor(&hir::Type::Defined(Box::new(alone)))
    }

    /// The descriptor of the components of `representation`, a possible
    /// representation of `defined`, given to its selector, as if they were
    /// a value's stored ones, so that a failure writes them as the
    /// selector writes them, `t{c: …}`.
    fn representation_descriptor(
        &mut self,
        defined: &hir::Defined,
        representation: &hir::Representation,
    ) -> lir::DataId {
        let given = hir::Defined {
            name: defined.written().to_string(),
            unit: defined.unit.clone(),
            components: representation.components.clone(),
            representations: Vec::new(),
            constraints: Vec::new(),
            keys: BTreeMap::new(),
            order: None,
            supertypes: Vec::new(),
            specialized: false,
            specialization: None,
        };
        self.descriptor(&hir::Type::Defined(Box::new(given)))
    }

    /// A heading's attributes, laid out as an array of `trendy_runtime::Attr`:
    /// `{ name: *u8, name_len, offset, desc: *TypeDesc }`, in name order. A
    /// wrapped attribute, which the source can't name, has none.
    fn attributes(&mut self, heading: &hir::Heading) -> lir::DataId {
        let tuple = self.layout(&hir::Type::Tuple(heading.clone()));
        let entries = heading
            .attributes()
            .enumerate()
            .map(|(i, (name, ty))| {
                let name = if hir::is_wrapped(name) { "" } else { name };
                let name_bytes = self.constant(vec![lir::Field::Bytes(name.as_bytes().to_vec())]);
                let desc = self.descriptor(ty);
                lir::Field::Struct(vec![
                    lir::Field::Address(name_bytes),
                    lir::Field::Usize(len(name.len())),
                    lir::Field::Offset(tuple.clone(), vec![i]),
                    lir::Field::Address(desc),
                ])
            })
            .collect();
        self.constant(entries)
    }

    fn empty(&mut self) -> lir::DataId {
        if let Some(id) = self.empty {
            return id;
        }
        let id = self.constant(vec![lir::Field::Bytes(Vec::new())]);
        self.empty = Some(id);
        id
    }
}

// The `trendy_runtime::KIND_` constants.
const KIND_BOOLEAN: u8 = 0;
const KIND_INTEGER: u8 = 1;
const KIND_APPROXIMATE: u8 = 2;
const KIND_TEXT: u8 = 3;
const KIND_TUPLE: u8 = 4;
const KIND_RELATION: u8 = 5;
const KIND_ARRAY: u8 = 6;
const KIND_DEFINED: u8 = 7;
const KIND_UNION: u8 = 8;
const KIND_CHARACTER: u8 = 9;
const KIND_U8: u8 = 10;
const KIND_U16: u8 = 11;
const KIND_U32: u8 = 12;

/// Where a descriptor has `trendy_runtime::TypeDesc::integers` among its fields, as
/// [`Lowering::descriptor`] lays it out.
const DESCRIPTOR_INTEGERS: usize = 2;

/// The part of lowering for one function.
struct FunctionBuilder<'a> {
    program: &'a hir::Program,
    /// The function's name, unique in the program.
    name: String,
    /// The parameters of the operator it is, with their types, which have
    /// no type parameters in them.
    params: BTreeMap<String, hir::Parameter>,
    /// The slot with the address of each `var` or `out` parameter's
    /// variable, which the parameter struct holds, copied there on entry.
    pointers: BTreeMap<String, lir::SlotId>,
    variable_data: &'a [lir::DataId],
    /// The local variables of the operator it is, with their types, which
    /// have no type parameters in them.
    locals: Vec<hir::Local>,
    /// Where each local variable is, by id: in a read-only operator, once
    /// its `Let` is lowered.
    local_places: Vec<Option<LocalPlace>>,
    slots: Vec<lir::Layout>,
    /// The blocks, those not ended yet placeholders.
    blocks: Vec<lir::Block>,
    /// The block being built, whose instructions are `insts`.
    current: lir::BlockId,
    insts: Vec<lir::Inst>,
    /// Whether the current statement needs a scope (see
    /// [`lir::RuntimeFn::Begin`]): whether it calls a runtime function that
    /// makes a text, relation or array, which the statement holds until it
    /// ends, assigns a value that points to one, or passes one to an update
    /// operator. Only a variable's references outlive the statement.
    scoped: bool,
    /// The slot of the statements' scopes, one at a time.
    scope: Option<lir::Place>,
    /// Whether the function begins scopes, a statement's or a loop's,
    /// which its failure exit ends.
    begins: bool,
    /// Each tuple at hand, innermost last: what `Current` refers to,
    /// inside an `Extend`'s values and a `Restrict`'s condition.
    tuples: Vec<AtHand>,
    /// The site the code built so far leaves the program at: the
    /// statement's being lowered, or a value's in it, if any.
    site: Option<hir::Site>,
    /// Where the site the function is entered at is saved, once a call
    /// changes it while the function has none of its own, or its failure
    /// exit records it.
    entered: Option<lir::Place>,
    /// The blocks that take the failures of the code being built,
    /// innermost last; with none, they go to the function's failure exit.
    catches: Vec<lir::BlockId>,
    /// For each block, the block that took its failures as it was built,
    /// or `None` for the failure exit.
    handlers: Vec<Option<lir::BlockId>>,
    /// The function's failure exit, made when a block that can fail first
    /// sends its failures there.
    exit: Option<lir::BlockId>,
    /// The slot of each binding of a `Let` whose value points to a text,
    /// relation or array, which holds references to them from the `Let` on,
    /// until its last use, and of each relation a loop looks up in and its
    /// index, which hold them from the first tuple that needs them until
    /// the loop ends (see [`FunctionBuilder::looked_up`]), with its type:
    /// null before and after, from the function's entry on, so that its
    /// failure exit releases each.
    lets: Vec<(lir::Place, hir::Type)>,
    /// The cycle of calls the function is in, if it is in one: see
    /// [`FunctionBuilder::in_cycle`].
    cycle: Option<usize>,
    /// The slot it copies its arguments to, if it is, and has parameters.
    arguments: Option<lir::Place>,
    /// The blocks that end in a tail call, which end the scopes it has begun.
    tail_blocks: Vec<lir::BlockId>,
    /// What an early exit, `return`, `leave` or `iterate`, goes out of from
    /// where the code is, innermost last.
    exits: Vec<Exit>,
    /// The slot of the innermost scope as the function starts, once what ends
    /// the scopes it has begun needs it.
    outer_scope: Option<lir::Place>,
    /// The `Let`s' bindings that the value being computed reads for the last
    /// time, as an operand (see [`FunctionBuilder::operand_read`]).
    last_read: Vec<hir::LocalId>,
    /// The order of each of the `Let`s' bindings, and of the local variables, of relations
    /// whose tuples aren't in canonical order, as nothing that reads them depends on it: in
    /// no particular order (see [`FunctionBuilder::loose_place`]), or sorted otherwise
    /// (see [`FunctionBuilder::sorted_place`]).
    local_orders: HashMap<hir::LocalId, Order>,
    /// Each local constant computed when a read first needs it ([`hir::Local::deferred`]),
    /// as its declaration last lowered left it.
    deferred: HashMap<hir::LocalId, Deferred>,
    /// The blocks' local variables that can be so, when they are assigned, with what all
    /// that reads each wants: any order, or a sorted one (see
    /// [`FunctionBuilder::note_loose_locals`]).
    local_wants: HashMap<hir::LocalId, Want>,
    /// The attributes what reads each of those would have it sorted by first, as
    /// [`ReadsOf`] gathers them: what sorts it anyway sorts it so, and one looked up again
    /// is put in that order.
    local_leading: HashMap<hir::LocalId, Vec<String>>,
}

/// What [`FunctionBuilder::read_want`] gathers of the reads of a local variable or a
/// binding, as it goes through them.
#[derive(Default)]
struct ReadsOf {
    /// How many joins and semijoins search it: what is searched more than once is better
    /// sorted once.
    searches: usize,
    /// The attributes what reads it would have it sorted by first, if all that would agree:
    /// a restriction's equated ones, which its runs are found by, and a join's or a
    /// semijoin's common ones, which it is merged on.
    leading: Option<Vec<String>>,
    /// Whether two would have it sorted by different ones first, so that none is.
    disagree: bool,
    /// Whether a restriction finds its equated attributes in it again for each tuple of
    /// something, or time round a loop, which sorting it once by them pays for.
    looked_up_again: bool,
    /// Whether what reads it wants it in canonical order more than once: searched again, or
    /// for each tuple of something, or time round a loop, which would sort it each time if
    /// it were kept in another order.
    canonical_again: bool,
}

impl ReadsOf {
    /// Note that a reader would have it sorted by `leading` first.
    fn prefer(&mut self, leading: Vec<String>) {
        if leading.is_empty() {
            return;
        }
        match &self.leading {
            None if !self.disagree => self.leading = Some(leading),
            Some(known) if *known == leading => {}
            _ => {
                self.leading = None;
                self.disagree = true;
            }
        }
    }
}

/// What is around a read of a relation, as [`FunctionBuilder::read_want`] goes down.
#[derive(Clone, Copy)]
struct Around {
    /// What order what it is part of has to come out in.
    want: Want,
    /// Whether it is part of what a join or a semijoin searches, by order.
    searched: bool,
    /// Whether it is read again for each tuple of something, or time round a loop.
    repeated: bool,
    /// Whether it is part of a join of several, whose operands are joined in the order its
    /// plan says as the program runs ([`lir::RuntimeFn::Join`]), so that what it would have
    /// them sorted by first isn't known when compiling.
    planned: bool,
}

impl Around {
    /// What is around a value that its reader wants in canonical order.
    fn wanted_in_order() -> Around {
        Around {
            want: Want::Canonical,
            searched: false,
            repeated: false,
            planned: false,
        }
    }
}

/// How a relation's tuples are ordered, or are to be.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Order {
    /// In canonical order, so that equal relations have equal arrays.
    Canonical,
    /// Sorted by the attributes of a sort order other than by name, in turn, as only a
    /// descriptor that says so can describe it ([`Lowering::described`]).
    SortedBy(SortId),
    /// In no particular order, only all different: what nothing depends the order of needn't
    /// be sorted (see [`FunctionBuilder::loose_place`]).
    Loose,
    /// In no particular order, with some perhaps the same: for what removes those itself,
    /// as a projection does.
    Repeated,
}

/// What the reader of a relation asks of its order.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Want {
    /// Canonical order, so that equal relations have equal arrays.
    Canonical,
    /// Sorted in whatever order is less work, which may not be by name, and which the
    /// reader is told ([`FunctionBuilder::sorted_place`]).
    Sorted,
    /// In any order, all different: what nothing depends the order of needn't be sorted
    /// (see [`FunctionBuilder::loose_place`]).
    Any,
    /// In any order, with some perhaps the same: for what removes those itself, as a
    /// projection does.
    Repeated,
}

impl Want {
    /// The stronger of this and `other`: what is wanted by readers that want either. Any
    /// order is the weakest, then a sorted one, then canonical order.
    fn stronger(self, other: Want) -> Want {
        let strength = |want: Want| match want {
            Want::Any | Want::Repeated => 0,
            Want::Sorted => 1,
            Want::Canonical => 2,
        };
        if strength(other) > strength(self) {
            other
        } else {
            self
        }
    }

    /// The order that what can only give canonical order or leave its tuples as they
    /// come gives what is wanted so: canonical order is a sorted one.
    fn as_comes(self) -> Order {
        match self {
            Want::Canonical | Want::Sorted => Order::Canonical,
            Want::Any => Order::Loose,
            Want::Repeated => Order::Repeated,
        }
    }
}

/// What is known of the order of a relation's tuples as they are made, as what makes them
/// is told: by a reshape or an extension.
#[derive(Clone, Copy, Debug)]
enum Made {
    /// Wanted in this order, which they are sorted into if they don't come so, or in none,
    /// if it is [`Order::Loose`].
    Wanted(Order),
    /// In this order, a sorted one, which they come in, as they do in what they are made
    /// from, so they needn't be sorted or checked: by a reshape, a rename's of a relation
    /// sorted by the same attributes ([`lir::ORDERED_RESULT`]), and by an extension, its
    /// operand's order, then the attributes it adds ([`lir::Inst::SortedAs`]).
    Kept(Order),
}

/// A sort order other than by name: the names of the attributes a relation's tuples are
/// sorted by, most significant first, each of them once, among
/// [`Lowering::sort_orders`].
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
struct SortId(usize);

/// How a relation is read through a rename, which needs none of its tuples moved, as what
/// reads it reads attributes by where they are: its heading, and the name each attribute
/// is read by and has there.
#[derive(Clone)]
struct Shape {
    physical: hir::Heading,
    /// By the name it is read by, in name order, as the result has them.
    names: BTreeMap<String, String>,
}

impl Shape {
    /// A relation or a tuple of `heading`, read as it is.
    fn plain(heading: &hir::Heading) -> Shape {
        Shape {
            physical: heading.clone(),
            names: heading
                .attributes()
                .map(|(name, _)| (name.to_string(), name.to_string()))
                .collect(),
        }
    }

    /// Where the attribute read as `name` is among the heading's, if it has one.
    fn position(&self, name: &str) -> Option<usize> {
        let own = self.names.get(name)?;
        self.physical.attributes().position(|(a, _)| a == own)
    }

    /// Where each attribute is among the heading's, in the order they are read by name.
    fn positions(&self) -> Vec<usize> {
        self.names
            .keys()
            .map(|name| {
                self.position(name)
                    .expect("a rename reads only attributes the operand has")
            })
            .collect()
    }

    /// Whether what is read has the layout and the order of what it is read from: each
    /// attribute comes where it did, whatever it is called.
    fn is_as_laid_out(&self) -> bool {
        self.positions().into_iter().eq(0..self.names.len())
    }
}

/// `inner`, of type `ty`, read as `named` through the renames around it, as `shape` says.
#[derive(Clone)]
struct View<'e> {
    inner: &'e hir::Expr,
    ty: hir::Type,
    named: hir::Type,
    shape: Shape,
}

/// What an early exit goes out of, which it ends as it would end itself.
#[derive(Clone)]
enum Exit {
    /// A loop, whose next run `iterate` goes to, its condition checked
    /// first, and whose end `leave` goes to.
    Loop {
        next: lir::BlockId,
        after: lir::BlockId,
    },
    /// A block, whose local variables give up their references as it ends.
    Block(Vec<hir::LocalId>),
    /// The runtime's scope a `for` loop's array is made in, which ends after
    /// the loop.
    Scope(lir::Place),
    /// A transaction, which commits as its block ends.
    Transaction(Box<Commit>),
    /// What an operator's body comes to as it ends, where a `return` goes,
    /// with where the body's value goes, and its type, if it has one.
    Return {
        landing: lir::BlockId,
        result: Option<(lir::Place, hir::Type)>,
    },
}

/// What a transaction does as its block ends, and it commits: it reads the
/// clock no more, if it did, gives up the values it kept, and, with
/// `collecting`, takes back the warnings it put aside, adding its own to the
/// variable. `catches` is how many blocks took failures around it.
#[derive(Clone)]
struct Commit {
    clocked: bool,
    kept: Vec<Kept>,
    collecting: Option<(hir::Target, Aside)>,
    collection: Collection,
    catches: usize,
}

/// A variable a transaction keeps as it begins: where it is, the slot its
/// value is kept in, and its type.
#[derive(Clone)]
struct Kept {
    variable: lir::Place,
    value: lir::Place,
    ty: hir::Type,
}

impl Kept {
    /// Keep the variable's value, with references of its own, with `b`
    /// building the code.
    fn keep(&self, b: &mut FunctionBuilder, cx: &mut Lowering) {
        b.insts.push(lir::Inst::Copy {
            dst: self.value.clone(),
            src: self.variable.clone(),
            layout: cx.layout(&self.ty),
        });
        if cx.points(&self.ty) {
            b.insts.push(lir::Inst::CallRuntime {
                function: lir::RuntimeFn::Retain,
                args: vec![self.value.clone(), self.descriptor(cx)],
            });
        }
    }

    /// Give up the value kept, as the transaction commits.
    fn give_up(&self, b: &mut FunctionBuilder, cx: &mut Lowering) {
        if cx.points(&self.ty) {
            b.insts.push(lir::Inst::CallRuntime {
                function: lir::RuntimeFn::Release,
                args: vec![self.value.clone(), self.descriptor(cx)],
            });
        }
    }

    /// Give the variable its kept value back, as the transaction rolls
    /// back: its references go to the variable, which gives up its own.
    fn restore(&self, b: &mut FunctionBuilder, cx: &mut Lowering) {
        if cx.points(&self.ty) {
            b.insts.push(lir::Inst::CallRuntime {
                function: lir::RuntimeFn::Release,
                args: vec![self.variable.clone(), self.descriptor(cx)],
            });
        }
        b.insts.push(lir::Inst::Copy {
            dst: self.variable.clone(),
            src: self.value.clone(),
            layout: cx.layout(&self.ty),
        });
    }

    /// Its type's descriptor, as a place.
    fn descriptor(&self, cx: &mut Lowering) -> lir::Place {
        lir::Place::whole(lir::Base::Data(cx.descriptor(&self.ty)))
    }
}

/// Where a local variable is: its value's place, and whether the variable
/// holds references to the buffers its value points to, as a program's
/// variable does, which the end of its block gives up: an update
/// operator's variable does, while a `Let`'s, or a loop's, is the place of
/// a value something else holds.
#[derive(Debug, Clone)]
struct LocalPlace {
    value: lir::Place,
    holds: bool,
}

/// A local constant computed when a read first needs it ([`hir::Local::deferred`]), as its
/// declaration leaves it each time it runs.
struct Deferred {
    /// Its value, as a read of it computes it.
    value: hir::Expr,
    /// Whether it has been computed since its declaration ran.
    computed: lir::Place,
    /// A bit of an integer for each read that narrows it ([`hir::Expr::AdaptedRead`]), by
    /// the order they are lowered in, the last for all those past it, as nothing but
    /// what is computed depends on it: whether the read has run since its declaration did.
    ran: lir::Place,
    /// How many of those reads are lowered.
    reads: u32,
}

/// What says whether a read that narrows a constant has run ([`FunctionBuilder::read_ran`]).
enum Ran {
    /// A static of the read's own, of a constant of the program's.
    Static(lir::Place),
    /// The read's bit, `bit`, of `reads`, of a local constant ([`Deferred::ran`]).
    Bit { reads: lir::Place, bit: lir::Place },
}

/// A restriction's loop, as [`FunctionBuilder::restrict`] lowers it: of the tuples of the
/// relation at `src`, of `heading`, which come as `order` says, each that it keeps is
/// written, laid out as `tuple`, to `dst`, at `kept`, which counts them.
#[derive(Clone, Copy)]
struct Restricted<'a> {
    src: &'a lir::Place,
    heading: &'a hir::Heading,
    order: Order,
    dst: &'a lir::Place,
    kept: &'a lir::Place,
    tuple: &'a lir::Layout,
}

/// The slots what puts failures aside keeps what it knows in, as it runs: the loop of
/// [`FunctionBuilder::each_at_hand_failing_first`], or the steps of
/// [`FunctionBuilder::steps_put_aside`].
struct FirstFailing {
    /// Which is the first to fail, among those so far, or `none`: a tuple's index, or a
    /// step's.
    first: lir::Place,
    /// `usize::MAX`: no tuple or step.
    none: lir::Place,
    /// The scope innermost as it begins, if what it computes begins scopes of its own,
    /// which a failure's end with.
    scope: Option<lir::Place>,
    /// How many calls the trace has as it begins, and the site, if what it computes makes
    /// calls, which leave a failure's calls in the trace and the site at their own.
    traced: Option<(lir::Place, lir::Place)>,
}

/// The steps of [`FunctionBuilder::steps_put_aside`], once they are lowered.
struct Steps {
    failing: FirstFailing,
    /// Where each is computed again, from a copy of its own, to fail as it did.
    again: Vec<lir::BlockId>,
}

/// What a restriction keeps of how the operands that can fail of its condition, an `and`
/// or an `or`, have decided it, so as to compute them in the order likeliest to decide it
/// soonest ([`FunctionBuilder::ranked_connective`]): kept for the whole run of the program,
/// as each time the restriction runs learns from the last.
struct Ranking {
    /// How many operands it ranks.
    count: usize,
    /// A variable of the program's own of `4 * count + 2` usizes: as `trendy_rt_rank` orders
    /// them, the operand computed in each place, in turn, then for each operand, as written,
    /// how many times it was computed, how many times it decided the condition, and what it
    /// costs; then how many tuples the condition has been computed for, and at how many it
    /// is ranked next.
    state: lir::DataId,
    /// The order, as this run of the restriction computes its operands in: one slot of
    /// `count` usizes, copied from `state` as it begins, and once it ranks them again. A run
    /// that an operand begins, of the same restriction, can rank them again too, which
    /// mustn't change the order of a tuple some operands of which are computed already.
    order: lir::Place,
}

impl Ranking {
    /// Its state's field `at`.
    fn field(&self, at: usize) -> lir::Place {
        lir::Place {
            base: lir::Base::Data(self.state),
            path: vec![at],
        }
    }
}

/// What the loop of [`FunctionBuilder::each_at_hand_failing_first`] gives the handler of a
/// tuple's failure.
struct Looped {
    /// Where the loop goes on after a tuple's work, with the next.
    next: lir::BlockId,
    /// The tuple at hand, which a slot points to.
    at_hand: lir::Place,
    /// Its index.
    index: lir::Place,
    /// The bindings its work makes, among [`FunctionBuilder::lets`], which a failure gives up.
    bound: std::ops::Range<usize>,
}

/// A tuple at hand, which `Current` refers to: where it is, and, if it is
/// each tuple of a relation in turn, the loop's lookups.
struct AtHand {
    place: lir::Place,
    lookups: Option<Lookups>,
}

impl AtHand {
    /// The tuple at `place`, the only one at hand: there is no loop.
    fn one(place: lir::Place) -> AtHand {
        AtHand {
            place,
            lookups: None,
        }
    }
}

/// The relations a loop over a relation's tuples looks up in (see
/// [`FunctionBuilder::looked_up`]).
#[derive(Default)]
struct Lookups {
    /// The local variables bound in the loop, as lowering reaches them,
    /// which can differ for each tuple: a relation looked up in reads none
    /// of them.
    varying: Vec<hir::LocalId>,
    made: Vec<Lookup>,
}

/// A relation, `operand`, of type `ty`, that a loop looks up in on the
/// attributes `on`: the slot that keeps it, once the first tuple that
/// needs it computes it, and the slot of its index on them.
struct Lookup {
    operand: hir::Expr,
    on: Vec<String>,
    ty: hir::Type,
    kept: lir::Place,
    index: lir::Place,
}

/// How many integers a value of type `ty` is, one after another in the
/// order they compare, if it is nothing else, and otherwise 0: an integer
/// is one, and a tuple or a value of a type outside a hierarchy is its
/// attributes', in name order, as they are laid out and compared.
fn integers(ty: &hir::Type, families: &Families) -> usize {
    match ty {
        hir::Type::Scalar(hir::Scalar::Integer) => 1,
        hir::Type::Defined(defined) if families.stored(defined).is_some() => 0,
        hir::Type::Tuple(_) | hir::Type::Defined(_) if ty.root().is_none() => {
            let fields = ty.fields().expect("tuples and types' values have fields");
            let each: Vec<usize> = fields
                .attributes()
                .map(|(_, ty)| integers(ty, families))
                .collect();
            if each.contains(&0) {
                0
            } else {
                each.iter().sum()
            }
        }
        _ => 0,
    }
}

/// The type of a relation's index (see [`lir::RuntimeFn::Index`]): an
/// array of the positions of its tuples.
fn index_type() -> hir::Type {
    hir::Type::Array(Box::new(hir::Type::Scalar(hir::Scalar::Integer)))
}

/// A change to an array's element, computed: the slot of each position,
/// from the outermost array in, and of the value.
struct Computed {
    positions: Vec<lir::Place>,
    value: lir::Place,
}

/// Whether `expr` is only read from where it is, not computed: a
/// constant, a variable, a parameter, a local variable, the tuple at
/// hand, or an attribute of one of those.
fn is_read(expr: &hir::Expr) -> bool {
    match expr {
        hir::Expr::Constant(_)
        | hir::Expr::Variable(_)
        | hir::Expr::ComputedConstant(..)
        | hir::Expr::Parameter(_)
        | hir::Expr::Local(_)
        | hir::Expr::Current(..) => true,
        hir::Expr::Extract(tuple, _) => is_read(tuple),
        _ => false,
    }
}

/// The type of the elements `depth` arrays into one of type `ty`.
fn element_at(ty: &hir::Type, depth: usize) -> hir::Type {
    (0..depth).fold(ty.clone(), |ty, _| match ty {
        hir::Type::Array(element) => *element,
        _ => unreachable!("sema indexes only arrays"),
    })
}

/// A block not built yet.
fn placeholder() -> lir::Block {
    lir::Block {
        insts: Vec::new(),
        terminator: lir::Terminator::Return,
        failure: None,
    }
}

impl<'a> FunctionBuilder<'a> {
    /// The function `name`, of an operator with the parameters `params`,
    /// with an entry block that copies the address of each `var` or `out`
    /// parameter's variable to a slot of its own.
    fn new(
        program: &'a hir::Program,
        name: String,
        params: BTreeMap<String, hir::Parameter>,
        variable_data: &'a [lir::DataId],
    ) -> Self {
        let mut builder = FunctionBuilder {
            program,
            name,
            params,
            pointers: BTreeMap::new(),
            variable_data,
            locals: Vec::new(),
            local_places: Vec::new(),
            slots: Vec::new(),
            blocks: vec![placeholder()],
            current: lir::BlockId(0),
            insts: Vec::new(),
            scoped: false,
            scope: None,
            begins: false,
            tuples: Vec::new(),
            site: None,
            entered: None,
            catches: Vec::new(),
            handlers: vec![None],
            exit: None,
            lets: Vec::new(),
            cycle: None,
            arguments: None,
            tail_blocks: Vec::new(),
            exits: Vec::new(),
            outer_scope: None,
            last_read: Vec::new(),
            local_orders: HashMap::new(),
            deferred: HashMap::new(),
            local_wants: HashMap::new(),
            local_leading: HashMap::new(),
        };
        let variables: Vec<(usize, String)> = builder
            .params
            .iter()
            .enumerate()
            .filter(|(_, (_, p))| p.mode != hir::Mode::Const)
            .map(|(i, (name, _))| (i, name.clone()))
            .collect();
        for (index, name) in variables {
            let slot = builder.slot(lir::Layout::Address);
            builder.insts.push(lir::Inst::Copy {
                dst: slot.clone(),
                src: lir::Place {
                    base: lir::Base::Params,
                    path: vec![index],
                },
                layout: lir::Layout::Address,
            });
            let lir::Base::Slot(id) = slot.base else {
                unreachable!("a slot's place is the whole slot");
            };
            builder.pointers.insert(name, id);
        }
        builder
    }

    /// Make the function one in the cycle of calls `cycle`, whose calls can go
    /// on without end. Its caller gives it a reference to what each argument
    /// points to, which it holds in a slot of its own, as a `Let`'s binding does
    /// (see `lets`), rather than leaving them with the calling statement: a
    /// tail call can then give them up, however long the calls go on, and it
    /// reads nothing of a frame that has gone. And it fails if the stack is
    /// used up (R0025), rather than running off its end.
    fn in_cycle(&mut self, cycle: usize, cx: &mut Lowering) {
        self.cycle = Some(cycle);
        if let Some(layout) = params_layout(&self.params, &cx.families) {
            let arguments = self.slot(layout.clone());
            self.insts.push(lir::Inst::Copy {
                dst: arguments.clone(),
                src: lir::Place::whole(lir::Base::Params),
                layout,
            });
            for (place, ty) in held_arguments(&self.params, &arguments, cx) {
                self.lets.push((place, ty));
            }
            self.arguments = Some(arguments);
        }
        self.insts.push(lir::Inst::CheckStack);
    }

    /// Give up the arguments the function holds, if it is in a cycle of calls.
    fn give_up_arguments(&mut self, cx: &mut Lowering) {
        if let Some(arguments) = self.arguments.clone() {
            for (place, ty) in held_arguments(&self.params, &arguments, cx) {
                self.release_place(&place, &ty, cx);
            }
        }
    }

    /// The type of `expr`, in the function.
    fn type_of(&self, expr: &hir::Expr) -> hir::Type {
        self.program.type_of(expr, &self.params, &self.locals)
    }

    /// Slots for the local variables `locals`. One that points to texts,
    /// relations or arrays starts with null pointers, as it is again at the
    /// end of its block, so that its declaration, the first assignment, has
    /// no references to give up.
    fn declare(&mut self, locals: Vec<hir::Local>, cx: &mut Lowering) {
        for local in locals {
            let layout = cx.layout(&local.ty);
            let value = self.slot(layout.clone());
            let holds = cx.points(&local.ty);
            if holds {
                self.insts.push(lir::Inst::Copy {
                    dst: value.clone(),
                    src: zero_constant(&layout, cx),
                    layout,
                });
            }
            self.locals.push(local);
            self.local_places.push(Some(LocalPlace { value, holds }));
        }
    }

    /// The local variables `locals` of a read-only operator, each placed
    /// where its `Let` puts its value, as each binding is lowered
    /// ([`FunctionBuilder::let_chain`]).
    fn bind(&mut self, locals: Vec<hir::Local>) {
        self.local_places = vec![None; locals.len()];
        self.locals = locals;
    }

    /// The function whose result is `value`, of the type `result`, with the
    /// local variables `locals`, which its `Let`s give values, as a
    /// read-only operator's is: and whether a call of it makes a text,
    /// relation or array. What it makes on the way is its call's, in a
    /// scope of the call's own, which hands its result out to its caller's
    /// (see [`FunctionBuilder::nested_scope`]).
    fn valued(
        mut self,
        locals: Vec<hir::Local>,
        value: &hir::Expr,
        result: &hir::Type,
        cx: &mut Lowering,
    ) -> (lir::Function, bool) {
        self.bind(locals);
        let layout = cx.layout(result);
        self.nested_scope(cx, |this, cx| {
            this.returned(value, result, cx);
            // Whatever it reads, a call in a cycle always hands it out.
            this.scoped |= this.cycle.is_some() && cx.points(result);
            vec![(lir::Place::whole(lir::Base::Result), result.clone())]
        });
        self.give_up_arguments(cx);
        let makes = self.scoped && cx.points(result);
        (self.finish(Some(layout), cx), makes)
    }

    /// The function whose result is `value` written as `debug_value`
    /// writes it, a text, with the local variables `locals`, as for
    /// [`FunctionBuilder::valued`]: a call of it makes the text.
    fn written(
        mut self,
        locals: Vec<hir::Local>,
        value: &hir::Expr,
        cx: &mut Lowering,
    ) -> (lir::Function, bool) {
        self.bind(locals);
        let ty = self.type_of(value);
        self.nested_scope(cx, |this, cx| {
            let src = this.place(value, cx);
            let descriptor = lir::Place::whole(lir::Base::Data(cx.descriptor(&ty)));
            let dst = lir::Place::whole(lir::Base::Result);
            this.insts.push(lir::Inst::CallRuntime {
                function: lir::RuntimeFn::ToText,
                args: vec![dst.clone(), src, descriptor],
            });
            this.scoped = true;
            vec![(dst, hir::Type::Scalar(hir::Scalar::Text))]
        });
        let layout = scalar_layout(hir::Scalar::Text);
        (self.finish(Some(layout), cx), true)
    }

    /// `expr`, a read-only operator's value or a part of it in a tail position,
    /// of the type `ty`, stored as the function's result: unless a tail call
    /// ends the function first, which then leaves it to the function called.
    fn returned(&mut self, expr: &hir::Expr, ty: &hir::Type, cx: &mut Lowering) {
        let result = lir::Place::whole(lir::Base::Result);
        if !self.has_tail_call(expr, cx) {
            let src = self.place(expr, cx);
            self.insts.push(lir::Inst::Copy {
                dst: result,
                src,
                layout: cx.layout(ty),
            });
            return;
        }
        match expr {
            hir::Expr::At(site, inner) => {
                let outer = self.site;
                self.at(Some(*site), cx);
                self.returned(inner, ty, cx);
                if outer.is_some() {
                    self.at(outer, cx);
                }
            }
            hir::Expr::Treat(_, inner) => self.returned(inner, ty, cx),
            hir::Expr::Let(..) => {
                let (end, alive) = self.let_bindings(expr, Want::Canonical, cx);
                self.reading_last(end, alive.iter().map(|(id, ..)| *id), |this| {
                    this.returned(end, ty, cx);
                });
                self.keep_past_bindings(&result, ty, alive, cx);
            }
            hir::Expr::Run(block, value) => {
                self.note_loose_locals(block, Some(value));
                self.running(block, value, &result, ty, cx, |this, cx| {
                    this.returned(value, ty, cx);
                });
            }
            hir::Expr::If(condition, then, otherwise) => {
                let condition = self.place(condition, cx);
                let (first, second, after) = (self.new_block(), self.new_block(), self.new_block());
                self.end_block(
                    lir::Terminator::Branch {
                        condition,
                        then: first,
                        otherwise: second,
                    },
                    first,
                );
                let outer = self.site;
                for (branch, next) in [then, otherwise].into_iter().zip([second, after]) {
                    self.returned(branch, ty, cx);
                    self.site = outer;
                    self.end_block(lir::Terminator::Jump(after), next);
                }
            }
            hir::Expr::Call(call) => self.tail_call(call, cx),
            _ => unreachable!("only those are tail positions"),
        }
    }

    /// Whether `expr`, in a tail position, has a tail call in one.
    fn has_tail_call(&self, expr: &hir::Expr, cx: &Lowering) -> bool {
        match expr {
            hir::Expr::At(_, inner)
            | hir::Expr::Treat(_, inner)
            | hir::Expr::Run(_, inner)
            | hir::Expr::Let(_, _, inner) => self.has_tail_call(inner, cx),
            hir::Expr::If(_, then, otherwise) => {
                self.has_tail_call(then, cx) || self.has_tail_call(otherwise, cx)
            }
            hir::Expr::Call(call) => self.is_tail_call(call, cx),
            _ => false,
        }
    }

    /// Whether `call`, in a tail position, is a tail call: it closes a cycle of
    /// calls, and leaves its caller nothing to do after it returns.
    fn is_tail_call(&self, call: &hir::Call, cx: &Lowering) -> bool {
        self.cycle.is_some()
            && self.cycle == cx.recursion.cycle(call.operator)
            // A local variable's address is in the frame it would give up, and
            // sema rejects a variable of the program's in a call that closes a
            // cycle, as the operator would use it by its own name too (A0053).
            && call
                .updated
                .values()
                .all(|target| matches!(target, hir::Target::Parameter(_)))
    }

    /// `call`, a tail call: its arguments, checked against the constraints in
    /// the braces of the operator it calls, then a jump to the first of its
    /// versions that they fit, in place of the function.
    fn tail_call(&mut self, call: &hir::Call, cx: &mut Lowering) {
        let params = params_of(self.program.operator(call.operator), &call.types);
        let args = self.arguments(&params, call, cx);
        self.call_constraints_checked(call, args.as_ref(), &params, cx);
        self.for_version(call, args, cx, |this, operator, args, cx| {
            this.tail_call_of(operator, &call.types, args, cx);
        });
    }

    /// End the block with a jump to the function of the operator `operator`,
    /// instantiated with `types`, in place of this one: it gets its arguments,
    /// at `args`, with its own references, and its result goes where this one's
    /// would. All this function holds is given up first, which ends what the
    /// arguments alone don't hold. What follows is never reached.
    fn tail_call_of(
        &mut self,
        operator: hir::OperatorId,
        types: &[hir::Type],
        args: Option<lir::Place>,
        cx: &mut Lowering,
    ) {
        let (function, _) = function(self.program, self.variable_data, operator, types, cx);
        let params = params_of(self.program.operator(operator), types);
        let waiting = args.map(|args| {
            for (place, ty) in held_arguments(&params, &args, cx) {
                let descriptor = lir::Place::whole(lir::Base::Data(cx.descriptor(&ty)));
                self.insts.push(lir::Inst::CallRuntime {
                    function: lir::RuntimeFn::Retain,
                    args: vec![place, descriptor],
                });
            }
            let layout = params_layout(&params, &cx.families).expect("it has arguments");
            let waiting = cx.tail_arguments(function, &layout);
            self.insts.push(lir::Inst::Copy {
                dst: waiting.clone(),
                src: args,
                layout,
            });
            waiting
        });
        self.release_held(cx);
        self.tail_blocks.push(self.current);
        let next = self.new_block();
        self.end_block(
            lir::Terminator::TailCall {
                function,
                args: waiting,
            },
            next,
        );
    }

    /// The statements of `block`, the last of them in a tail position, as an
    /// update operator's last statement is, and of a branch of an `if` that is
    /// one, then the release of the references its local variables hold.
    fn tail_block(&mut self, block: &hir::Block, cx: &mut Lowering) {
        self.note_loose_locals(block, None);
        self.exits.push(Exit::Block(block.locals.clone()));
        if let Some(((site, last), statements)) = block.statements.split_last() {
            let last_mentioned = last_mentioned(block, None, &self.locals);
            for (at, (site, statement)) in statements.iter().enumerate() {
                self.at(Some(*site), cx);
                self.statement(statement, cx);
                self.release_locals_last_mentioned_at(at, &last_mentioned, cx);
            }
            self.at(Some(*site), cx);
            match last {
                hir::Statement::Call(call) if self.is_tail_call(call, cx) => {
                    self.scoped(|this| this.tail_call(call, cx));
                }
                hir::Statement::If(condition, then, otherwise) => {
                    self.if_statement(condition, [then, otherwise], Self::tail_block, cx);
                }
                statement => self.statement(statement, cx),
            }
        }
        self.exits.pop();
        self.release_locals_of(block, cx);
    }

    /// The statements of `block`, then the release of the references its
    /// local variables hold: every statement that could point into what
    /// they point to has ended, and holds its own. Releasing makes their
    /// pointers null, so that one assigned again, when the block runs
    /// again, has none to give up.
    fn block(&mut self, block: &hir::Block, cx: &mut Lowering) {
        self.note_loose_locals(block, None);
        self.exits.push(Exit::Block(block.locals.clone()));
        self.statements_of(block, None, cx);
        self.exits.pop();
        self.release_locals_of(block, cx);
    }

    /// The statements of `block`, each followed by the release of the references held by
    /// the local variables that nothing after it mentions, in the block or in `end`, what
    /// is computed from them when it has run, so that a relation lasts no longer than its
    /// last use, as a binding's does (see [`FunctionBuilder::let_chain`]).
    fn statements_of(&mut self, block: &hir::Block, end: Option<&hir::Expr>, cx: &mut Lowering) {
        let last_mentioned = last_mentioned(block, end, &self.locals);
        for (at, (site, statement)) in block.statements.iter().enumerate() {
            self.at(Some(*site), cx);
            self.statement(statement, cx);
            self.release_locals_last_mentioned_at(at, &last_mentioned, cx);
        }
    }

    /// The release of the references held by the local variables of `last_mentioned` that
    /// the statement at `at` is the last to mention.
    fn release_locals_last_mentioned_at(
        &mut self,
        at: usize,
        last_mentioned: &[(hir::LocalId, usize)],
        cx: &mut Lowering,
    ) {
        for (id, _) in last_mentioned.iter().filter(|(_, last)| *last == at) {
            self.release_local(*id, cx);
        }
    }

    /// The release of the references `block`'s local variables hold.
    fn release_locals_of(&mut self, block: &hir::Block, cx: &mut Lowering) {
        for id in &block.locals {
            self.release_local(*id, cx);
        }
    }

    /// Set the site the program is at to `site`, if there is one, so that
    /// a failure from here on writes it.
    fn at(&mut self, site: Option<hir::Site>, cx: &mut Lowering) {
        self.site = site;
        if let Some(site) = site {
            self.insts.push(lir::Inst::Site(cx.site(site)));
        }
    }

    /// The site the code is at again after a call, whose callee leaves the
    /// program at its own: the function's own, if it has set one, or else
    /// the one it was entered at, saved as it starts.
    fn after_call(&mut self, cx: &mut Lowering) {
        if self.site.is_some() {
            self.at(self.site, cx);
            return;
        }
        let entered = self.entered();
        self.insts.push(lir::Inst::RestoreSite { src: entered });
    }

    /// Where the site the function is entered at is saved, as it starts.
    fn entered(&mut self) -> lir::Place {
        if let Some(entered) = &self.entered {
            return entered.clone();
        }
        let slot = self.slot(lir::Layout::Address);
        self.entered = Some(slot.clone());
        slot
    }

    /// Where `expr`, computed at `site`, is: the site set while it is
    /// computed, then the one before, if there is one, again.
    fn place_at(&mut self, site: hir::Site, expr: &hir::Expr, cx: &mut Lowering) -> lir::Place {
        let outer = self.site;
        self.at(Some(site), cx);
        let place = self.place(expr, cx);
        if outer.is_some() {
            self.at(outer, cx);
        }
        place
    }

    /// Have each loop over a relation's tuples that the code being lowered
    /// is in count the local variables `bound` as differing for each tuple
    /// (see [`Lookups`]).
    fn bound_in_loops(&mut self, bound: &[hir::LocalId]) {
        for at_hand in &mut self.tuples {
            if let Some(lookups) = &mut at_hand.lookups {
                lookups.varying.extend(bound);
            }
        }
    }

    /// The value of a body or a value block whose statements decide what
    /// runs next: its `block`'s statements, then `value`, which reads the
    /// local variables they assign, computed in a scope of its own, handed
    /// out to the one it is in whatever it is, as it may be one of them,
    /// which gives it up after. Those are placed here, unless the function
    /// has placed them, holding references as an update operator's do, null
    /// from the function's entry on, so that its failure exit and a
    /// transaction's rollback release each, as a `Let`'s binding (see
    /// `lets`). Its statements have scopes of their own, which the
    /// statement this is in may be using.
    fn run(&mut self, block: &hir::Block, value: &hir::Expr, cx: &mut Lowering) -> lir::Place {
        self.note_loose_locals(block, Some(value));
        let ty = self.type_of(value);
        let result = self.slot(cx.layout(&ty));
        self.running(block, value, &result, &ty, cx, |this, cx| {
            let src = this.place(value, cx);
            this.insts.push(lir::Inst::Copy {
                dst: result.clone(),
                src,
                layout: cx.layout(&ty),
            });
        });
        result
    }

    /// `block`'s statements then what `value` adds, which stores the value of
    /// `end`, what they lead to, at `result`, of the type `ty`, as
    /// [`FunctionBuilder::run`] says.
    fn running(
        &mut self,
        block: &hir::Block,
        end: &hir::Expr,
        result: &lir::Place,
        ty: &hir::Type,
        cx: &mut Lowering,
        value: impl FnOnce(&mut Self, &mut Lowering),
    ) {
        let declared = block.declared();
        self.bound_in_loops(&declared);
        let mut loops = Vec::new();
        block.each_statement(&mut |statement| {
            if let hir::Statement::For(variable, ..) = statement {
                loops.push(*variable);
            }
        });
        for id in declared.into_iter().filter(|id| !loops.contains(id)) {
            let ty = self.locals[id.0].ty.clone();
            let holds = cx.points(&ty);
            let place = if let Some(place) = &self.local_places[id.0] {
                place.value.clone()
            } else {
                let value = self.slot(cx.layout(&ty));
                self.local_places[id.0] = Some(LocalPlace {
                    value: value.clone(),
                    holds,
                });
                value
            };
            if holds {
                self.lets.push((place, ty));
            }
        }
        let (outer_scope, outer_site) = (self.scope.take(), self.site);
        self.nested_scope(cx, |this, cx| {
            // An operator's body, whose value `return` can give too.
            this.returning(block.returns(), Some((result, ty)), |this| {
                this.statements_of(block, Some(end), cx);
                value(this, cx);
            });
            this.scoped |= cx.points(ty);
            vec![(result.clone(), ty.clone())]
        });
        self.scope = outer_scope;
        self.release_locals_of(block, cx);
        self.site = outer_site;
        self.after_call(cx);
    }

    /// The value of `expr`, a chain of `Let`s, each binding a local
    /// variable for the rest, the sites they are at among them, and the
    /// expression the chain ends with: a read-only operator's body, or a
    /// value block's. A binding's value is computed in a scope of its own
    /// and copied to a slot of its own, which holds references to it
    /// (`let_bound`), so that the binding frees it once nothing after it in
    /// the chain reads it: the value of the next binding that does, or else
    /// the end's, which may point into it, and which the scope it is in
    /// then keeps. So a relation a body declares lasts no longer than its
    /// last use, as a statement's values do.
    fn let_chain(
        &mut self,
        expr: &hir::Expr,
        want: Want,
        cx: &mut Lowering,
    ) -> (lir::Place, Order) {
        let outer = self.site;
        let (end, alive) = self.let_bindings(expr, want, cx);
        let bound = alive.iter().map(|(id, ..)| *id);
        let (mut value, order) = self.reading_last(end, bound, |this| match want {
            Want::Any => this.loose_place(end, cx),
            Want::Sorted => this.sorted_place(end, cx),
            Want::Canonical | Want::Repeated => (this.place(end, cx), Order::Canonical),
        });
        if !alive.is_empty() {
            // The end's value may point into the bindings still alive, so
            // it is copied out of them first.
            let ty = self.type_of(end);
            let layout = cx.layout(&ty);
            let copy = self.slot(layout.clone());
            self.insts.push(lir::Inst::Copy {
                dst: copy.clone(),
                src: value,
                layout,
            });
            self.keep_past_bindings(&copy, &ty, alive, cx);
            value = copy;
        }
        if outer.is_some() && self.site != outer {
            self.at(outer, cx);
        }
        (value, order)
    }

    /// Have the scope keep the value at `value`, of the type `ty`, which may
    /// point into the bindings `alive`, the end of a chain of `Let`s, then
    /// release them.
    fn keep_past_bindings(
        &mut self,
        value: &lir::Place,
        ty: &hir::Type,
        alive: Vec<(hir::LocalId, lir::Place, hir::Type)>,
        cx: &mut Lowering,
    ) {
        if alive.is_empty() {
            return;
        }
        if cx.points(ty) {
            let descriptor = lir::Place::whole(lir::Base::Data(cx.descriptor(ty)));
            self.insts.push(lir::Inst::CallRuntime {
                function: lir::RuntimeFn::Keep,
                args: vec![value.clone(), descriptor],
            });
            self.scoped = true;
        }
        for (_, slot, ty) in alive {
            self.release_place(&slot, &ty, cx);
        }
    }

    /// What `lower` adds, which computes `value`, whichever of the bindings
    /// `bound` have no reads after it being given up by the operation that reads
    /// them there, if that is all that does (see
    /// [`FunctionBuilder::operand_read`]).
    fn reading_last<T>(
        &mut self,
        value: &hir::Expr,
        bound: impl Iterator<Item = hir::LocalId>,
        lower: impl FnOnce(&mut Self) -> T,
    ) -> T {
        let outer = self.last_read.len();
        let bound: Vec<hir::LocalId> = bound
            .filter(|id| value.reads_last_as_operand(*id) && !self.read_by_deferred(value, *id))
            .collect();
        self.last_read.extend(bound);
        let lowered = lower(self);
        self.last_read.truncate(outer);
        lowered
    }

    /// What computes each local constant computed when a read first needs it
    /// ([`hir::Local::deferred`]), of the chain of `Let`s `expr` or around it, which such a
    /// read reads.
    fn deferred_values(&self, expr: &hir::Expr) -> Vec<(hir::LocalId, hir::Expr)> {
        let mut deferred: Vec<(hir::LocalId, hir::Expr)> = self
            .deferred
            .iter()
            .map(|(id, deferred)| (*id, deferred.value.clone()))
            .collect();
        let mut at = expr;
        loop {
            match at {
                hir::Expr::At(_, inner) if matches!(**inner, hir::Expr::Let(..)) => at = inner,
                hir::Expr::Let(id, value, body) => {
                    if self.locals[id.0].deferred {
                        deferred.push((*id, (**value).clone()));
                    }
                    at = body;
                }
                _ => return deferred,
            }
        }
    }

    /// Whether what computes a local constant `expr` reads, computed when a read first
    /// needs it ([`hir::Local::deferred`]), reads the local variable `id`, as that read may
    /// compute it there.
    fn read_by_deferred(&self, expr: &hir::Expr, id: hir::LocalId) -> bool {
        self.deferred.iter().any(|(constant, deferred)| {
            deferred.value.reads_local(id) && expr.reads_local(*constant)
        })
    }

    /// The binding of the local constant `id`, of `value`, computed when a read first
    /// needs it ([`hir::Local::deferred`]), as [`FunctionBuilder::defer`] declares one in a
    /// block: null from the function's entry on, as a binding that points to anything is,
    /// and assigned as `want` says, as its readers would have it, by the read that computes
    /// it.
    fn deferred_binding(
        &mut self,
        id: hir::LocalId,
        value: &hir::Expr,
        want: Want,
        cx: &mut Lowering,
    ) -> (lir::Place, hir::Type) {
        let ty = self.locals[id.0].ty.clone();
        let slot = self.slot(cx.layout(&ty));
        self.lets.push((slot.clone(), ty.clone()));
        self.local_places[id.0] = Some(LocalPlace {
            value: slot.clone(),
            holds: false,
        });
        if want != Want::Canonical {
            self.local_wants.insert(id, want);
        }
        self.defer(id, value, cx);
        (slot, ty)
    }

    /// The operand `operand`, of type `ty`, at `place`, which the relational
    /// operation just lowered has read, given up if nothing else can read it, so
    /// that it doesn't last while the statement goes on: a value made here, which
    /// only the operation reads, and a `Let`'s binding that this is the last read
    /// of. The result holds references of its own to what the two share. A
    /// variable's or a parameter's value is held by what holds it, which goes on
    /// reading it, and a tuple's attributes are shared with what is computed from
    /// it, by no reference.
    fn operand_read(
        &mut self,
        operand: &hir::Expr,
        place: &lir::Place,
        ty: &hir::Type,
        cx: &mut Lowering,
    ) {
        if !matches!(ty, hir::Type::Relation(_) | hir::Type::Array(_)) {
            return;
        }
        match operand {
            hir::Expr::At(_, inner) | hir::Expr::Treat(_, inner) => {
                self.operand_read(inner, place, ty, cx);
            }
            // The binding's own reference: `place` may be a copy, put in order to be read.
            hir::Expr::Local(id) if self.last_read.contains(id) => {
                let (own, _) = self.local_place(*id);
                self.release_place(&own, ty, cx);
            }
            hir::Expr::Local(_)
            | hir::Expr::Variable(_)
            | hir::Expr::ComputedConstant(..)
            | hir::Expr::Parameter(_)
            | hir::Expr::Constant(_)
            | hir::Expr::Current(..) => {}
            // A rename that keeps the order is a view of what it renames: a binding, if this
            // is where one is, which goes on holding it.
            _ if self.is_a_bindings(place) => {}
            _ => {
                let descriptor = lir::Place::whole(lir::Base::Data(cx.descriptor(ty)));
                self.insts.push(lir::Inst::CallRuntime {
                    function: lir::RuntimeFn::DropOperand,
                    args: vec![place.clone(), descriptor],
                });
            }
        }
    }

    /// Whether `place` is where a local variable is.
    fn is_a_bindings(&self, place: &lir::Place) -> bool {
        self.local_places
            .iter()
            .flatten()
            .any(|local| local.value == *place)
    }

    /// The bindings of `expr`, a chain of `Let`s, in order, each given up once
    /// nothing after it in the chain reads it: the end of the chain, which is
    /// not lowered, and those it may read, with their slots and types.
    fn let_bindings<'e>(
        &mut self,
        expr: &'e hir::Expr,
        want: Want,
        cx: &mut Lowering,
    ) -> (&'e hir::Expr, Vec<(hir::LocalId, lir::Place, hir::Type)>) {
        // The chain's sites and bindings, in order, then its end.
        enum Part<'e> {
            Site(hir::Site),
            Let(hir::LocalId, &'e hir::Expr),
        }
        let mut parts = Vec::new();
        let mut end = expr;
        loop {
            match end {
                hir::Expr::At(site, inner) if matches!(**inner, hir::Expr::Let(..)) => {
                    parts.push(Part::Site(*site));
                    end = inner;
                }
                hir::Expr::Let(id, value, body) => {
                    parts.push(Part::Let(*id, value));
                    end = body;
                }
                _ => break,
            }
        }
        let deferred = self.deferred_values(expr);
        // Whether the binding of `id` is read after the part at `k`: by a
        // later binding's value, before one binds it again, or by the end, or by
        // what computes a binding computed when a read first needs it.
        let read_after = |k: usize, id: hir::LocalId| {
            for part in &parts[k + 1..] {
                if let Part::Let(bound, value) = part {
                    if reads_through_deferred(value, id, &deferred) {
                        return true;
                    }
                    if *bound == id {
                        return false;
                    }
                }
            }
            reads_through_deferred(end, id, &deferred)
        };
        // What each binding is wanted in by all that reads it: the last first, as what one is
        // wanted in says what those it is computed from are.
        let mut wants = vec![Want::Canonical; parts.len()];
        for k in (0..parts.len()).rev() {
            let Part::Let(id, _) = &parts[k] else {
                continue;
            };
            if !self.is_relation_of_no_key_to_check(*id) {
                continue;
            }
            let mut reads = ReadsOf::default();
            let mut read_as = |expr: &hir::Expr, want: Want| {
                let around = Around {
                    want,
                    ..Around::wanted_in_order()
                };
                self.read_want(*id, expr, around, &mut reads)
            };
            let mut wanted = Want::Any;
            for (part, &later) in parts[k + 1..].iter().zip(&wants[k + 1..]) {
                if let Part::Let(bound, value) = part {
                    wanted = if bound == id {
                        Want::Canonical
                    } else {
                        wanted.stronger(read_as(value, later))
                    };
                }
            }
            // What the end is computed as: canonical order, unless wanted otherwise.
            let end_want = if want == Want::Repeated {
                Want::Canonical
            } else {
                want
            };
            let wanted = wanted.stronger(read_as(end, end_want));
            wants[k] = self.leading_want(*id, wanted, reads);
        }
        let mut alive: Vec<(hir::LocalId, lir::Place, hir::Type)> = Vec::new();
        for (k, part) in parts.iter().enumerate() {
            match part {
                Part::Site(site) => self.at(Some(*site), cx),
                Part::Let(id, value) => {
                    let ending: Vec<hir::LocalId> = alive
                        .iter()
                        .map(|(bound, ..)| *bound)
                        .filter(|bound| !read_after(k, *bound))
                        .collect();
                    let (slot, ty) = self.reading_last(value, ending.into_iter(), |this| {
                        this.let_bound(*id, value, wants[k], cx)
                    });
                    if cx.points(&ty) {
                        alive.push((*id, slot, ty));
                    }
                    let (dead, live): (Vec<_>, Vec<_>) = alive
                        .into_iter()
                        .partition(|(bound, ..)| !read_after(k, *bound));
                    for (_, slot, ty) in dead {
                        self.release_place(&slot, &ty, cx);
                    }
                    alive = live;
                }
            }
        }
        (end, alive)
    }

    /// Bind the local variable `id` to `value`: computed in a scope of its
    /// own, nested in the one it is in, and copied to a slot of the
    /// binding's own, which takes references to what it points to, if it
    /// points to anything, as the scope ends, so that only the binding
    /// holds what the computation made for it. A value that points to
    /// nothing stays where it is computed, unless that can be given up before it
    /// is read (see [`FunctionBuilder::can_be_given_up`]), and is then copied. The
    /// binding's place, and its type, are given; one that points to anything is
    /// null from the function's entry on, until then.
    fn let_bound(
        &mut self,
        id: hir::LocalId,
        value: &hir::Expr,
        want: Want,
        cx: &mut Lowering,
    ) -> (lir::Place, hir::Type) {
        self.bound_in_loops(&[id]);
        let ty = self.locals[id.0].ty.clone();
        if self.locals[id.0].deferred {
            return self.deferred_binding(id, value, want, cx);
        }
        let points = cx.points(&ty);
        if !points {
            let mut place = None;
            self.nested_scope(cx, |this, cx| {
                let src = this.place(value, cx);
                this.check_keys(&hir::Target::Local(id), &src, cx);
                let goes = this.can_be_given_up(&src);
                place = Some(if goes {
                    let copy = this.slot(cx.layout(&ty));
                    this.insts.push(lir::Inst::Copy {
                        dst: copy.clone(),
                        src,
                        layout: cx.layout(&ty),
                    });
                    copy
                } else {
                    src
                });
                Vec::new()
            });
            let place = place.expect("the value is placed");
            self.local_places[id.0] = Some(LocalPlace {
                value: place.clone(),
                holds: false,
            });
            return (place, ty);
        }
        let layout = cx.layout(&ty);
        let slot = self.slot(layout.clone());
        self.lets.push((slot.clone(), ty.clone()));
        self.nested_scope(cx, |this, cx| {
            let (src, order) = this.given_local(id, value, want, cx);
            if order != Order::Canonical {
                this.local_orders.insert(id, order);
            }
            this.check_keys(&hir::Target::Local(id), &src, cx);
            this.insts.push(lir::Inst::Copy {
                dst: slot.clone(),
                src,
                layout,
            });
            let descriptor = lir::Place::whole(lir::Base::Data(cx.descriptor(&ty)));
            this.insts.push(lir::Inst::CallRuntime {
                function: lir::RuntimeFn::Retain,
                args: vec![slot.clone(), descriptor],
            });
            Vec::new()
        });
        self.local_places[id.0] = Some(LocalPlace {
            value: slot.clone(),
            holds: false,
        });
        (slot, ty)
    }

    /// Whether what is at `place` can stop being there while the function goes on:
    /// in a buffer, which a release can free, or part of a slot that holds references,
    /// as `t.a` is of the binding `t`, whose release makes all of it zero. No other slot
    /// is released, so what is read from one stays what it is.
    fn can_be_given_up(&self, place: &lir::Place) -> bool {
        match place.base {
            lir::Base::Indirect { .. } => true,
            lir::Base::Slot(_) => self.lets.iter().any(|(slot, _)| slot.base == place.base),
            lir::Base::Data(_) | lir::Base::Params | lir::Base::Result => false,
        }
    }

    /// Give up the references the value at `place`, of type `ty`, holds,
    /// making all of it zero (see [`lir::RuntimeFn::Release`]), so that what is
    /// read later must have been copied out first.
    fn release_place(&mut self, place: &lir::Place, ty: &hir::Type, cx: &mut Lowering) {
        let descriptor = lir::Place::whole(lir::Base::Data(cx.descriptor(ty)));
        self.insts.push(lir::Inst::CallRuntime {
            function: lir::RuntimeFn::Release,
            args: vec![place.clone(), descriptor],
        });
    }

    /// The function, its last block ended with a return, with a result
    /// laid out as `result`, if it has one: each block that can fail sends
    /// its failures to the block that took them as it was built, or else
    /// to the function's failure exit, made if one does.
    fn finish(mut self, result: Option<lir::Layout>, cx: &mut Lowering) -> lir::Function {
        self.end_block(lir::Terminator::Return, lir::BlockId(0));
        for i in 0..self.blocks.len() {
            if self.blocks[i].can_fail() {
                let target = match self.handlers[i] {
                    Some(handler) => handler,
                    None => *self.exit.get_or_insert_with(|| {
                        self.blocks.push(placeholder());
                        self.handlers.push(None);
                        lir::BlockId(self.blocks.len() - 1)
                    }),
                };
                self.blocks[i].failure = Some(target);
            }
        }
        if let Some(exit) = self.exit {
            self.failure_exit(exit, cx);
        }
        if self.begins {
            for block in std::mem::take(&mut self.tail_blocks) {
                let saved = self.outer_scope();
                self.blocks[block.0].insts.push(lir::Inst::CallRuntime {
                    function: lir::RuntimeFn::EndSince,
                    args: vec![saved],
                });
            }
        }
        if let Some(outer_scope) = self.outer_scope.take() {
            let innermost = lir::Inst::CallRuntime {
                function: lir::RuntimeFn::Innermost,
                args: vec![outer_scope],
            };
            self.blocks[0].insts.insert(0, innermost);
        }
        if let Some(entered) = self.entered.take() {
            let save = lir::Inst::SaveSite { dst: entered };
            self.blocks[0].insts.insert(0, save);
        }
        // Each `Let`'s binding that holds references starts null, as the
        // function is entered, once nothing else is inserted before.
        for (slot, ty) in &self.lets {
            let layout = cx.layout(ty);
            let zero = zero_constant(&layout, cx);
            self.blocks[0].insts.insert(
                0,
                lir::Inst::Copy {
                    dst: slot.clone(),
                    src: zero,
                    layout,
                },
            );
        }
        lir::Function {
            name: self.name,
            recursive: self.cycle.is_some(),
            params: params_layout(&self.params, &cx.families),
            result,
            slots: self.slots,
            blocks: self.blocks,
        }
    }

    /// The function's failure exit, a block of its own, which gives up
    /// what the function holds, as the end of its statements and blocks
    /// would, then returns, failing: it ends the scopes begun since the
    /// function was entered, the innermost then, saved as it starts, and
    /// then releases its local variables' references, and its `Let`s'
    /// bindings'. A local variable holds none before its declaration or
    /// after its block, nor a binding before its `Let` or after its last
    /// use, so one released there gives up only what it holds. Before it returns, it
    /// records in the failure's trace that the failure passed through the
    /// call of it, at the site it was entered at.
    fn failure_exit(&mut self, exit: lir::BlockId, cx: &mut Lowering) {
        self.current = exit;
        if self.begins {
            let saved = self.outer_scope();
            self.insts.push(lir::Inst::CallRuntime {
                function: lir::RuntimeFn::EndSince,
                args: vec![saved],
            });
        }
        self.release_held(cx);
        let (entered, name) = (self.entered(), self.name.clone());
        let name = self.text(&name, cx);
        self.insts.push(lir::Inst::CallRuntime {
            function: lir::RuntimeFn::Trace,
            args: vec![entered, name],
        });
        self.end_block(lir::Terminator::Failed, exit);
    }

    /// The slot of the innermost scope as the function starts.
    fn outer_scope(&mut self) -> lir::Place {
        if let Some(saved) = &self.outer_scope {
            return saved.clone();
        }
        let saved = self.slot(lir::Layout::Address);
        self.outer_scope = Some(saved.clone());
        saved
    }

    /// Give up what the function holds, its local variables' references, and
    /// its `Let`s' bindings', as it leaves, and whatever it was doing: a local
    /// variable holds none before its declaration or after its block, nor a
    /// binding before its `Let` or after its last use, so one released gives up
    /// only what it holds.
    fn release_held(&mut self, cx: &mut Lowering) {
        let held: Vec<(lir::Place, hir::Type)> = self
            .local_places
            .iter()
            .zip(&self.locals)
            .filter_map(|(place, local)| {
                let place = place.as_ref().filter(|place| place.holds)?;
                Some((place.value.clone(), local.ty.clone()))
            })
            .chain(self.lets.iter().cloned())
            .collect();
        for (value, ty) in held {
            self.release_place(&value, &ty, cx);
        }
    }

    /// A new block, to be built later.
    fn new_block(&mut self) -> lir::BlockId {
        self.blocks.push(placeholder());
        self.handlers.push(None);
        lir::BlockId(self.blocks.len() - 1)
    }

    /// End the current block with `terminator`, and go on building `next`.
    /// Its failures go to the innermost block that takes them, which
    /// [`FunctionBuilder::finish`] makes its failure target.
    fn end_block(&mut self, terminator: lir::Terminator, next: lir::BlockId) {
        self.blocks[self.current.0] = lir::Block {
            insts: std::mem::take(&mut self.insts),
            terminator,
            failure: None,
        };
        self.handlers[self.current.0] = self.catches.last().copied();
        self.current = next;
    }

    /// What `lower` adds, its failures going to `handler`: in blocks of
    /// its own, since a block's failures go where they did as it ended.
    fn catching(&mut self, handler: lir::BlockId, lower: impl FnOnce(&mut Self)) {
        let start = self.new_block();
        self.end_block(lir::Terminator::Jump(start), start);
        self.catches.push(handler);
        lower(self);
        let next = self.new_block();
        self.end_block(lir::Terminator::Jump(next), next);
        self.catches.pop();
    }

    /// End the current block by stopping the program because of
    /// `failure`, and go on building `next`.
    fn fail(&mut self, failure: lir::Failure, next: lir::BlockId) {
        self.end_block(lir::Terminator::Fail(Box::new(failure)), next);
    }

    /// End the current block, where a check found a value broken, as
    /// `broken` says, and go on in `next`, where it isn't.
    fn broken(&mut self, broken: Broken, next: lir::BlockId) {
        match broken {
            Broken::Fail(failure) => self.end_block(lir::Terminator::Fail(failure), next),
            Broken::Skip { function, args, to } => {
                self.insts.push(lir::Inst::CallRuntime { function, args });
                self.end_block(lir::Terminator::Jump(to), next);
            }
        }
    }

    /// The slot of the statements' scopes, made when one first needs it.
    fn scope_slot(&mut self) -> lir::Place {
        if let Some(scope) = &self.scope {
            return scope.clone();
        }
        let scope = self.new_scope();
        self.scope = Some(scope.clone());
        scope
    }

    /// A slot for a scope, which the function begins.
    fn new_scope(&mut self) -> lir::Place {
        self.begins = true;
        self.slot(lir::Layout::scope())
    }

    /// Where the next instruction goes: the current block, and its index
    /// there.
    fn position(&self) -> (lir::BlockId, usize) {
        (self.current, self.insts.len())
    }

    /// Insert `inst` at `at`, a position taken earlier.
    fn insert(&mut self, at: (lir::BlockId, usize), inst: lir::Inst) {
        let (block, index) = at;
        let insts = if block == self.current {
            &mut self.insts
        } else {
            &mut self.blocks[block.0].insts
        };
        insts.insert(index, inst);
    }

    /// The statement, in a scope of its own if it needs one: an `if`'s
    /// statements each in their own, and its condition in another.
    fn statement(&mut self, statement: &hir::Statement, cx: &mut Lowering) {
        let assigns = statement.assigns();
        match statement {
            hir::Statement::Declare(id) => {
                let (place, _) = self.local_place(*id);
                self.insts.push(lir::Inst::NoValueYet(place));
            }
            hir::Statement::Call(call) => self.scoped(|this| {
                this.call(call, cx);
                this.constraints_checked(&assigns, cx);
            }),
            hir::Statement::Assign(assignments) => {
                if let [
                    hir::Assignment {
                        target: hir::Target::Local(id),
                        value,
                    },
                ] = &assignments[..]
                    && self.locals[id.0].deferred
                {
                    self.defer(*id, value, cx);
                    return;
                }
                self.scoped(|this| {
                    this.assign(assignments, cx);
                    this.constraints_checked(&assigns, cx);
                });
            }
            hir::Statement::If(condition, then, otherwise) => {
                self.if_statement(condition, [then, otherwise], Self::block, cx);
            }
            hir::Statement::Loop(condition, body, check) => {
                self.loop_statement(condition, body, *check, cx);
            }
            hir::Statement::For(variable, array, body) => {
                self.for_statement(*variable, array, body, cx);
            }
            hir::Statement::Fail(error) => self.scoped(|this| this.fail_with(error, cx)),
            hir::Statement::Warn(error) => self.warn(error, cx),
            hir::Statement::Transaction(collecting, body) => {
                self.transaction(collecting.as_ref(), body, cx);
                self.scoped(|this| this.constraints_checked(&assigns, cx));
            }
            hir::Statement::Return(value) => self.early_return(value.as_ref(), cx),
            hir::Statement::Leave(out) => self.loop_exit(false, *out, cx),
            hir::Statement::Iterate(out) => self.loop_exit(true, *out, cx),
        }
    }

    /// Check the constraints on the program's variables `assigned`, the
    /// targets a statement assigned as it ends, mentions one or more of:
    /// each part of each, in turn, failing with the first that is false,
    /// and the tuples that make it false where its form says which.
    fn constraints_checked(&mut self, assigned: &[&hir::Target], cx: &mut Lowering) {
        let variables: BTreeSet<hir::VariableId> = assigned
            .iter()
            .filter_map(|target| match target {
                hir::Target::Variable(id) => Some(*id),
                hir::Target::Local(_) | hir::Target::Parameter(_) => None,
            })
            .collect();
        let program = self.program;
        for (index, constraint) in program.constraints.iter().enumerate() {
            if !constraint.variables.is_disjoint(&variables) {
                self.variable_constraint_checked(index, cx);
            }
        }
    }

    /// Give each variable the runtime gives its value, as the program starts,
    /// that value, if `used`, the data the program's functions use, has it, so
    /// that one the program never reads or assigns can't stop it: the
    /// arguments, or the environment's array, which becomes its relation. Each
    /// is made in a scope of its own, which gives the variable its own
    /// reference.
    fn given_variables(&mut self, used: &BTreeSet<lir::DataId>, cx: &mut Lowering) {
        let program = self.program;
        for (i, variable) in program.variables.iter().enumerate() {
            let Some(given) = variable.given else {
                continue;
            };
            if !used.contains(&self.variable_data[i]) {
                continue;
            }
            let at = in_data(self.variable_data[i]);
            let ty = variable.ty.clone();
            let descriptor = lir::Place::whole(lir::Base::Data(cx.descriptor(&ty)));
            self.scoped(|b| {
                match given {
                    hir::Given::Arguments => b.insts.push(lir::Inst::CallRuntime {
                        function: lir::RuntimeFn::Arguments,
                        args: vec![at.clone(), descriptor.clone()],
                    }),
                    hir::Given::Environment => {
                        let hir::Type::Relation(heading) = &ty else {
                            unreachable!("sema gives the environment its relation type");
                        };
                        let array_ty =
                            hir::Type::Array(Box::new(hir::Type::Tuple(heading.clone())));
                        let array = b.slot(cx.layout(&array_ty));
                        let array_descriptor =
                            lir::Place::whole(lir::Base::Data(cx.descriptor(&array_ty)));
                        b.insts.push(lir::Inst::CallRuntime {
                            function: lir::RuntimeFn::Environment,
                            args: vec![array.clone(), array_descriptor],
                        });
                        let unloading = hir::Unloading {
                            named: None,
                            numbered: None,
                        };
                        let relation = b.unloaded(&array, &array_ty, &ty, &unloading, cx);
                        b.insts.push(lir::Inst::Copy {
                            dst: at.clone(),
                            src: relation,
                            layout: cx.layout(&ty),
                        });
                    }
                }
                b.insts.push(lir::Inst::CallRuntime {
                    function: lir::RuntimeFn::Retain,
                    args: vec![at.clone(), descriptor.clone()],
                });
                b.scoped = true;
            });
        }
    }

    /// Check the constraints the compiler can't check of the variables' initial
    /// values, each at its own site, leaving the program at the one it is at for
    /// `main`.
    fn constraints_checked_at_start(&mut self, cx: &mut Lowering) {
        let program = self.program;
        for (index, constraint) in program.constraints.iter().enumerate() {
            if constraint.checked_at_start {
                self.at(Some(constraint.site), cx);
                self.scoped(|this| this.variable_constraint_checked(index, cx));
                self.site = None;
                self.after_call(cx);
            }
        }
    }

    /// Check each part of the constraint `index`, in turn: those that can't fail first,
    /// each reporting that it is broken if it is, then those that can, as steps
    /// ([`FunctionBuilder::steps_put_aside`]), as one that is false decides that the
    /// constraint is broken even if another fails (see [`hir::Connective::And`]). If none
    /// is, the first to fail is computed again, and fails as it did.
    fn variable_constraint_checked(&mut self, index: usize, cx: &mut Lowering) {
        let constraint = &self.program.constraints[index];
        let (failing, safe): (Vec<usize>, Vec<usize>) = (0..constraint.parts.len())
            .partition(|&part| constraint.parts[part].condition.can_fail());
        for part in safe {
            let holds = self.part_holds(index, part, cx);
            let (kept, broken) = (self.new_block(), self.new_block());
            self.end_block(
                lir::Terminator::Branch {
                    condition: holds,
                    then: kept,
                    otherwise: broken,
                },
                broken,
            );
            self.part_broken(index, part, kept, cx);
        }
        if failing.is_empty() {
            return;
        }
        let broken: Vec<lir::BlockId> = failing.iter().map(|_| self.new_block()).collect();
        let steps = self.steps_failing_again(
            failing.len(),
            true,
            cx,
            |this, i, cx| this.part_holds(index, failing[i], cx),
            |this, i, holds, _| {
                let kept = this.new_block();
                this.end_block(
                    lir::Terminator::Branch {
                        condition: holds,
                        then: kept,
                        otherwise: broken[i],
                    },
                    kept,
                );
            },
        );
        let failed = self.steps_failed(&steps.failing);
        let (again, held) = (self.new_block(), self.new_block());
        self.end_block(
            lir::Terminator::Branch {
                condition: failed,
                then: again,
                otherwise: held,
            },
            again,
        );
        self.fail_again(&steps, cx);
        for (i, &part) in failing.iter().enumerate() {
            self.end_block(lir::Terminator::Jump(held), broken[i]);
            let unreached = self.new_block();
            self.part_broken(index, part, unreached, cx);
        }
        self.end_block(lir::Terminator::Jump(held), held);
    }

    /// Whether the part `part` of the constraint `index` holds, in a new slot.
    fn part_holds(&mut self, index: usize, part: usize, cx: &mut Lowering) -> lir::Place {
        let checked = Checked {
            constraint: index,
            part,
            tuples: false,
        };
        let holds = self.slot(scalar_layout(hir::Scalar::Boolean));
        self.call_constraint(checked, holds.clone(), cx);
        holds
    }

    /// Fail where the part `part` of the constraint `index` is broken, saying so, with the
    /// tuples that break it if its form says which, and go on building `next`.
    fn part_broken(&mut self, index: usize, part: usize, next: lir::BlockId, cx: &mut Lowering) {
        let constraint = &self.program.constraints[index];
        let written = &constraint.parts[part];
        let name = self.text(&constraint.name, cx);
        let condition = self.text(&written.written, cx);
        let tuples = written.breaking.is_some().then(|| {
            let tuples = self.slot(scalar_layout(hir::Scalar::Text));
            let checked = Checked {
                constraint: index,
                part,
                tuples: true,
            };
            self.call_constraint(checked, tuples.clone(), cx);
            tuples
        });
        let failure = lir::Failure::ConstraintViolated {
            constraint: name,
            condition,
            tuples,
        };
        self.fail(failure, next);
    }

    /// Call the function of `checked`, its value into `result`.
    fn call_constraint(&mut self, checked: Checked, result: lir::Place, cx: &mut Lowering) {
        let (function, makes) = constraint_function(self.program, self.variable_data, checked, cx);
        self.scoped |= makes;
        self.insts.push(lir::Inst::Call {
            function,
            args: None,
            result: Some(result),
            can_fail: true,
        });
        self.after_call(cx);
    }

    /// A transaction of `body`, with `collecting`, the variable it adds its
    /// errors to, if it has one. As it begins, each variable the block can
    /// assign is kept, with references of its own ([`Kept`]), and, with
    /// `collecting`, the warnings collected so far, those of the
    /// transactions around it, put aside ([`Collection`]); then the block
    /// runs, its failures going to the transaction's rollback, with no
    /// warnings yet. As it ends, what was kept is given up, and its
    /// block's warnings are added to `collecting`. Without, they are those
    /// of the transaction around it, and the block's go to it as they're
    /// given.
    ///
    /// The rollback ends the scopes the block began, releases its local
    /// variables, and its statements' `Let`s' bindings, and gives each
    /// variable kept its value back, with its
    /// references, which can't fail: nothing reads what the block left
    /// there, as every statement of it has ended. Its block's warnings
    /// survive. Then, with `collecting`, it adds them to `collecting`, and
    /// takes a `fail`'s error waiting, which it adds too, forgets the calls
    /// its failure passed through, and goes on after the transaction.
    /// Without, or for running out of memory, which has no error, the
    /// failure goes on, to the transaction around it, or the function's
    /// failure exit.
    fn transaction(
        &mut self,
        collecting: Option<&hir::Target>,
        body: &hir::Block,
        cx: &mut Lowering,
    ) {
        let kept = self.kept(body, cx);
        let collection = cx.collection(self.program);
        let (outer, traced) = (
            self.slot(lir::Layout::Address),
            self.slot(lir::Layout::Usize),
        );
        self.insts.push(lir::Inst::CallRuntime {
            function: lir::RuntimeFn::Innermost,
            args: vec![outer.clone()],
        });
        self.insts.push(lir::Inst::CallRuntime {
            function: lir::RuntimeFn::Traced,
            args: vec![traced.clone()],
        });
        let collecting = collecting.map(|target| (target, collection.put_aside(self, cx)));
        for kept in &kept {
            kept.keep(self, cx);
        }
        // What it calls reads when it began, which its statements can't read themselves.
        let clocked = body.calls_any_of(&cx.reading_transaction_start);
        let clock = |this: &mut Self, function| {
            if clocked {
                this.insts.push(lir::Inst::CallRuntime {
                    function,
                    args: Vec::new(),
                });
            }
        };
        clock(self, lir::RuntimeFn::TransactionBegins);
        let (rollback, after) = (self.new_block(), self.new_block());
        let lets = self.lets.len();
        let commit = Commit {
            clocked,
            kept: kept.clone(),
            collecting: collecting
                .as_ref()
                .map(|(target, aside)| ((*target).clone(), aside.clone())),
            collection: collection.clone(),
            catches: self.catches.len(),
        };
        self.exits.push(Exit::Transaction(Box::new(commit.clone())));
        self.catching(rollback, |this| this.block(body, cx));
        self.exits.pop();
        self.committed(&commit, cx);
        self.end_block(lir::Terminator::Jump(after), rollback);

        clock(self, lir::RuntimeFn::TransactionEnds);
        self.insts.push(lir::Inst::CallRuntime {
            function: lir::RuntimeFn::EndSince,
            args: vec![outer],
        });
        for id in body.declared() {
            self.release_local(id, cx);
        }
        // The bindings of the block's statements' `Let`s, those of the
        // statement that failed holding their values.
        for i in lets..self.lets.len() {
            let (slot, ty) = self.lets[i].clone();
            self.release_place(&slot, &ty, cx);
        }
        for kept in &kept {
            kept.restore(self, cx);
        }
        let Some((target, aside)) = collecting else {
            self.end_block(lir::Terminator::Raise, after);
            return;
        };
        let own = collection.take_back(self, &aside, cx);
        let waiting = cx.waiting(self.program);
        let error = waiting.error();
        let waits = self.is_of(&error, &waiting.ty, cx);
        let (take, raise) = (self.new_block(), self.new_block());
        self.end_block(
            lir::Terminator::Branch {
                condition: waits,
                then: take,
                otherwise: raise,
            },
            take,
        );
        self.insts.push(lir::Inst::CallRuntime {
            function: lir::RuntimeFn::Untrace,
            args: vec![traced],
        });
        self.collect(&collection, target, own.clone(), Some(&error), cx);
        let descriptor = lir::Place::whole(lir::Base::Data(cx.descriptor(&waiting.ty)));
        self.insts.push(lir::Inst::CallRuntime {
            function: lir::RuntimeFn::Release,
            args: vec![error, descriptor],
        });
        self.end_block(lir::Terminator::Jump(after), raise);
        self.collect(&collection, target, own, None, cx);
        self.end_block(lir::Terminator::Raise, after);
    }

    /// What `lower` adds, an operator's body, which a `return` in it, if
    /// `returns`, leaves, to come to what the body comes to as it ends, with
    /// its value, at the place `result` says, of its type, if it has one.
    fn returning(
        &mut self,
        returns: bool,
        result: Option<(&lir::Place, &hir::Type)>,
        lower: impl FnOnce(&mut Self),
    ) {
        if !returns {
            lower(self);
            return;
        }
        let landing = self.new_block();
        self.exits.push(Exit::Return {
            landing,
            result: result.map(|(held, ty)| (held.clone(), ty.clone())),
        });
        lower(self);
        self.exits.pop();
        self.end_block(lir::Terminator::Jump(landing), landing);
    }

    /// What a transaction does as its block ends, as `commit` says, which its
    /// block's end and an early exit from it both do.
    fn committed(&mut self, commit: &Commit, cx: &mut Lowering) {
        if commit.clocked {
            self.insts.push(lir::Inst::CallRuntime {
                function: lir::RuntimeFn::TransactionEnds,
                args: Vec::new(),
            });
        }
        for kept in &commit.kept {
            kept.give_up(self, cx);
        }
        if let Some((target, aside)) = &commit.collecting {
            let own = commit.collection.take_back(self, aside, cx);
            self.collect(&commit.collection, target, own, None, cx);
        }
    }

    /// End what the exits from the one at `depth` on are, the innermost
    /// first, as each would end: a block's local variables give up their
    /// references, a scope ends, after handing `value`, with its type, to the
    /// one around it, if a `return` is giving it, and a transaction commits,
    /// its failures no longer its own. Then go to `to`, the code after this
    /// being unreachable, as an exit is the last its block runs.
    fn exit_to(
        &mut self,
        depth: usize,
        value: Option<(&lir::Place, &hir::Type)>,
        to: lir::BlockId,
        cx: &mut Lowering,
    ) {
        let catches = self.catches.clone();
        let exited: Vec<Exit> = self.exits[depth..].iter().rev().cloned().collect();
        for exit in exited {
            match exit {
                Exit::Block(locals) => {
                    for id in locals {
                        self.release_local(id, cx);
                    }
                }
                Exit::Scope(scope) => {
                    if let Some((value, ty)) = value.filter(|(_, ty)| cx.points(ty)) {
                        let descriptor = lir::Place::whole(lir::Base::Data(cx.descriptor(ty)));
                        self.insts.push(lir::Inst::CallRuntime {
                            function: lir::RuntimeFn::HandOut,
                            args: vec![value.clone(), descriptor],
                        });
                    }
                    self.insts.push(lir::Inst::CallRuntime {
                        function: lir::RuntimeFn::End,
                        args: vec![scope],
                    });
                }
                Exit::Transaction(commit) => {
                    // Its commit fails as the code around it does.
                    let committing = self.new_block();
                    self.end_block(lir::Terminator::Jump(committing), committing);
                    self.catches.truncate(commit.catches);
                    self.committed(&commit, cx);
                }
                Exit::Loop { .. } | Exit::Return { .. } => {}
            }
        }
        let unreachable = self.new_block();
        self.end_block(lir::Terminator::Jump(to), unreachable);
        self.catches = catches;
    }

    /// `return;`, or `return value;`: the value given to the function's result,
    /// as the body's own would be, unless a tail call ends the function first,
    /// then what is between it and the body's end ended, and on to what the body
    /// comes to as it ends. A transaction it leaves commits first, so the call
    /// of a value is never a tail call out of one.
    fn early_return(&mut self, value: Option<&hir::Expr>, cx: &mut Lowering) {
        let depth = self
            .exits
            .iter()
            .rposition(|exit| matches!(exit, Exit::Return { .. }))
            .expect("sema returns only from an operator's body");
        let Exit::Return { landing, result } = self.exits[depth].clone() else {
            unreachable!("found as one");
        };
        let given = match (value, &result) {
            (Some(value), Some((held, ty))) => {
                // In a tail position, if the body's value is the function's.
                let commits = self.exits[depth..]
                    .iter()
                    .any(|exit| matches!(exit, Exit::Transaction(_)));
                if !commits && *held == lir::Place::whole(lir::Base::Result) {
                    self.returned(value, ty, cx);
                } else {
                    let src = self.place(value, cx);
                    self.insts.push(lir::Inst::Copy {
                        dst: held.clone(),
                        src,
                        layout: cx.layout(ty),
                    });
                }
                self.scoped |= cx.points(ty);
                Some((held, ty))
            }
            _ => None,
        };
        self.exit_to(depth + 1, given, landing, cx);
    }

    /// `leave` or, if `next`, `iterate`, of the loop `out` loops out from the
    /// innermost, 0 for that: what is between it and that loop's block's end
    /// ended, the loops within it too, then on to after the loop, or its next
    /// run.
    fn loop_exit(&mut self, next: bool, out: usize, cx: &mut Lowering) {
        let depth = self
            .exits
            .iter()
            .enumerate()
            .rev()
            .filter(|(_, exit)| matches!(exit, Exit::Loop { .. }))
            .nth(out)
            .map(|(depth, _)| depth)
            .expect("sema leaves only loops it is in");
        let Exit::Loop { next: again, after } = self.exits[depth].clone() else {
            unreachable!("found as one");
        };
        self.exit_to(depth + 1, None, if next { again } else { after }, cx);
    }

    /// `call`, of `trendy.streams`' `put` or `append`: the runtime function that
    /// writes what it is given, the call's arguments, in the order it takes them,
    /// and the descriptor of its bytes, if it writes bytes.
    fn written_out(&mut self, builtin: hir::UpdateBuiltin, call: &hir::Call, cx: &mut Lowering) {
        let (function, named): (lir::RuntimeFn, &[&str]) = match builtin {
            hir::UpdateBuiltin::PutText => (lir::RuntimeFn::PutText, &["text", "error"]),
            hir::UpdateBuiltin::PutBytes => (lir::RuntimeFn::PutBytes, &["bytes", "error"]),
            hir::UpdateBuiltin::AppendText => (lir::RuntimeFn::AppendText, &["path", "text"]),
            hir::UpdateBuiltin::AppendBytes => (lir::RuntimeFn::AppendBytes, &["path", "bytes"]),
            hir::UpdateBuiltin::AppendLines => (lir::RuntimeFn::AppendLines, &["path", "lines"]),
            _ => unreachable!("only `put` and `append` write out"),
        };
        let mut args: Vec<lir::Place> = named
            .iter()
            .map(|name| self.place(&call.args[*name], cx))
            .collect();
        if let Some(bytes) = call.args.get("bytes") {
            let ty = self.type_of(bytes);
            args.push(lir::Place::whole(lir::Base::Data(cx.descriptor(&ty))));
        }
        self.insts.push(lir::Inst::CallRuntime { function, args });
        // For why the stream can't be written, and a file's path, if it can't.
        self.scoped = true;
    }

    /// What `call`, of `trendy.streams`' `read`, reads of standard input, by the
    /// runtime function for `builtin`, given to its `out` parameter, `line`,
    /// `lines` or `bytes`, which the runtime makes as [`made_by_runtime`] says,
    /// given its descriptor if it is an array, and the errors, if `call` names
    /// `out errors`, given to that.
    fn read_in(&mut self, builtin: hir::UpdateBuiltin, call: &hir::Call, cx: &mut Lowering) {
        let (function, read) = match builtin {
            hir::UpdateBuiltin::ReadLine => (lir::RuntimeFn::ReadLine, "line"),
            hir::UpdateBuiltin::ReadLineWithErrors => (lir::RuntimeFn::ReadLineWithErrors, "line"),
            hir::UpdateBuiltin::ReadLines => (lir::RuntimeFn::ReadLines, "lines"),
            hir::UpdateBuiltin::ReadLinesWithErrors => {
                (lir::RuntimeFn::ReadLinesWithErrors, "lines")
            }
            hir::UpdateBuiltin::ReadBytes => (lir::RuntimeFn::ReadBytes, "bytes"),
            hir::UpdateBuiltin::ReadBytesWithErrors => {
                (lir::RuntimeFn::ReadBytesWithErrors, "bytes")
            }
            _ => unreachable!("only `read` reads standard input"),
        };
        let target = &call.updated[read];
        let ty = self.assigned(target).0.clone();
        let made = made_by_runtime(&ty);
        let dst = self.slot(cx.layout(&made));
        let errors = call.updated.contains_key("errors").then(|| self.errors(cx));
        let mut args = vec![dst.clone()];
        args.extend(errors.as_ref().map(|errors| errors.given.clone()));
        if matches!(made, hir::Type::Array(_)) {
            args.push(lir::Place::whole(lir::Base::Data(cx.descriptor(&made))));
        }
        self.insts.push(lir::Inst::CallRuntime { function, args });
        self.scoped = true;
        let value = self.made_of_components(dst, &ty, cx);
        self.store(target, value, cx);
        if let Some(errors) = errors {
            self.store(&call.updated["errors"], errors.value, cx);
        }
    }

    /// The random bytes `call` asks for, given to its `out` parameter, which
    /// the runtime makes as [`made_by_runtime`] says.
    fn random_bytes(&mut self, call: &hir::Call, cx: &mut Lowering) {
        let target = &call.updated["bytes"];
        let ty = self.assigned(target).0.clone();
        let made = made_by_runtime(&ty);
        let bytes = self.slot(cx.layout(&made));
        let count = self.place(&call.args["count"], cx);
        let descriptor = lir::Place::whole(lir::Base::Data(cx.descriptor(&made)));
        self.insts.push(lir::Inst::CallRuntime {
            function: lir::RuntimeFn::RandomBytes,
            args: vec![bytes.clone(), count, descriptor],
        });
        self.scoped = true;
        let bytes = self.made_of_components(bytes, &ty, cx);
        self.store(target, bytes, cx);
    }

    /// What `function` reads of the clock, given to the `out` parameters of `call`.
    fn clock_read(&mut self, function: lir::RuntimeFn, call: &hir::Call, cx: &mut Lowering) {
        let (seconds, nanos) = (self.slot(lir::Layout::I64), self.slot(lir::Layout::I64));
        self.insts.push(lir::Inst::CallRuntime {
            function,
            args: vec![seconds.clone(), nanos.clone()],
        });
        self.store(&call.updated["seconds"], seconds, cx);
        self.store(&call.updated["nanos"], nanos, cx);
    }

    /// Add `own`, a transaction's block's warnings, which it holds, and
    /// the error at `failed`, if the transaction takes one, to
    /// `collecting`, in a scope of their own, then give up `own`.
    fn collect(
        &mut self,
        collection: &Collection,
        collecting: &hir::Target,
        own: lir::Place,
        failed: Option<&lir::Place>,
        cx: &mut Lowering,
    ) {
        self.scoped(|this| {
            let mut added = own.clone();
            if let Some(error) = failed {
                let one = collection.one(this, error, true, cx);
                added = this.united(added, one, &collection.ty, cx);
            }
            let current = this.holder(collecting, cx);
            let combined = this.united(current, added, &collection.ty, cx);
            this.check_keys(collecting, &combined, cx);
            this.store(collecting, combined, cx);
        });
        let descriptor = lir::Place::whole(lir::Base::Data(cx.descriptor(&collection.ty)));
        self.insts.push(lir::Inst::CallRuntime {
            function: lir::RuntimeFn::Release,
            args: vec![own, descriptor],
        });
    }

    /// The union of the relations at `lhs` and `rhs`, of type `ty`, in a
    /// new slot, in memory the statement owns.
    fn united(
        &mut self,
        lhs: lir::Place,
        rhs: lir::Place,
        ty: &hir::Type,
        cx: &mut Lowering,
    ) -> lir::Place {
        let descriptor = lir::Place::whole(lir::Base::Data(cx.descriptor(ty)));
        let union = usize_constant(lir::Combination::Union.code(), cx);
        let dst = self.slot(cx.layout(ty));
        self.insts.push(lir::Inst::CallRuntime {
            function: lir::RuntimeFn::Combine,
            args: vec![dst.clone(), lhs, rhs, descriptor, union],
        });
        self.scoped = true;
        dst
    }

    /// `warn e;`: the error `e` added, `failed` false, to the warnings of
    /// the innermost transaction with `collecting` running, if one is
    /// ([`Collection`]), or else its message written, as `message` gives
    /// it (R0022).
    fn warn(&mut self, error: &hir::Expr, cx: &mut Lowering) {
        let collection = cx.collection(self.program);
        let (add, write, after) = (self.new_block(), self.new_block(), self.new_block());
        self.end_block(
            lir::Terminator::Branch {
                condition: collection.collecting(),
                then: add,
                otherwise: write,
            },
            add,
        );
        self.scoped(|this| {
            let value = this.place(error, cx);
            let one = collection.one(this, &value, false, cx);
            let combined = this.united(collection.warnings(), one, &collection.ty, cx);
            this.assign_place(collection.warnings(), combined, &collection.ty, cx);
        });
        self.end_block(lir::Terminator::Jump(after), write);
        self.scoped(|this| {
            let value = this.place(error, cx);
            let message = this.message_of(&value, cx);
            this.insts.push(lir::Inst::CallRuntime {
                function: lir::RuntimeFn::Warned,
                args: vec![message],
            });
        });
        self.end_block(lir::Terminator::Jump(after), after);
    }

    /// The message of the error at `error`, a value of the prelude's type
    /// `error`, as `message` gives it, in a slot, in memory the calling
    /// statement owns.
    fn message_of(&mut self, error: &lir::Place, cx: &mut Lowering) -> lir::Place {
        let callee = self.program.operator(self.program.message);
        let ty = callee.params["error"].ty.clone();
        let params = params_layout(&callee.params, &cx.families)
            .expect("`message` has a parameter, the error");
        let args = self.slot(params);
        let mut arg = args.clone();
        arg.path.push(0);
        self.insts.push(lir::Inst::Copy {
            dst: arg,
            src: error.clone(),
            layout: cx.layout(&ty),
        });
        let message = self.slot(cx.layout(&hir::Type::Scalar(hir::Scalar::Text)));
        let call = self.program.call_of(self.program.message, BTreeMap::new());
        self.invoke(&call, Some(args), Some(&message), cx);
        message
    }

    /// What a transaction of `body` keeps as it begins: each variable the
    /// block can assign, and a slot for its value. These are the program's
    /// variables it assigns, or the operators it calls do, at any depth,
    /// and the operator's own local variables, declared outside the block,
    /// and `var` and `out` parameters, it assigns or passes on to be.
    fn kept(&mut self, body: &hir::Block, cx: &mut Lowering) -> Vec<Kept> {
        let declared = body.declared();
        let mut targets: Vec<hir::Target> = self
            .program
            .variables_assigned(body)
            .into_iter()
            .map(hir::Target::Variable)
            .collect();
        for target in body.assigned() {
            let ours = match &target {
                hir::Target::Variable(_) => false,
                hir::Target::Local(id) => !declared.contains(id),
                hir::Target::Parameter(_) => true,
            };
            if ours && !targets.contains(&target) {
                targets.push(target);
            }
        }
        targets
            .into_iter()
            .map(|target| {
                let ty = self.assigned(&target).0.clone();
                let variable = self.holder(&target, cx);
                let value = self.slot(cx.layout(&ty));
                Kept {
                    variable,
                    value,
                    ty,
                }
            })
            .collect()
    }

    /// Give up the references the local variable `id` holds, if it holds
    /// any, and make its pointers null, as its block's end does.
    fn release_local(&mut self, id: hir::LocalId, cx: &mut Lowering) {
        let Some(place) = self.local_places[id.0].as_ref().filter(|p| p.holds) else {
            return;
        };
        let value = place.value.clone();
        let descriptor = lir::Place::whole(lir::Base::Data(cx.descriptor(&self.locals[id.0].ty)));
        self.insts.push(lir::Inst::CallRuntime {
            function: lir::RuntimeFn::Release,
            args: vec![value, descriptor],
        });
    }

    /// Fail with the error `error`, which waits, with the site it fails
    /// at, for what takes it ([`Waiting`]): a transaction, or else the
    /// entry, which writes its message (R0021). The error takes its own
    /// references, before any an error waiting already has are given up:
    /// one a failing `message` makes may be made of the one it writes.
    /// What comes after is never reached.
    fn fail_with(&mut self, error: &hir::Expr, cx: &mut Lowering) {
        let waiting = cx.waiting(self.program);
        let value = self.place(error, cx);
        let descriptor = lir::Place::whole(lir::Base::Data(cx.descriptor(&waiting.ty)));
        let layout = cx.layout(&waiting.ty);
        let held = self.slot(layout.clone());
        self.insts.push(lir::Inst::Copy {
            dst: held.clone(),
            src: value,
            layout: layout.clone(),
        });
        self.insts.push(lir::Inst::CallRuntime {
            function: lir::RuntimeFn::Retain,
            args: vec![held.clone(), descriptor.clone()],
        });
        self.insts.push(lir::Inst::CallRuntime {
            function: lir::RuntimeFn::Release,
            args: vec![waiting.error(), descriptor],
        });
        self.insts.push(lir::Inst::Copy {
            dst: waiting.error(),
            src: held,
            layout,
        });
        self.insts.push(lir::Inst::SaveSite {
            dst: waiting.site(),
        });
        let next = self.new_block();
        self.end_block(lir::Terminator::Raise, next);
    }

    /// A `for` loop of `body` over `array`, with the local variable
    /// `variable` the element at hand: the array computed once, then a
    /// loop over its elements, as over a relation's tuples, the variable
    /// each one's own place, so no copy. Sema rules out assigning what the
    /// array is computed from in the loop, so a variable's value stays put;
    /// an array the expression makes is kept, by a scope of the loop's own,
    /// until the loop ends. Its slot is its own, as the statements in the
    /// block each begin theirs in the one they share.
    fn for_statement(
        &mut self,
        variable: hir::LocalId,
        array: &hir::Expr,
        body: &hir::Block,
        cx: &mut Lowering,
    ) {
        let hir::Type::Array(element) = self.type_of(array) else {
            unreachable!("sema loops only over arrays");
        };
        let start = self.position();
        let src = self.place(array, cx);
        // The scope the array is made in, if it makes one, which an exit from
        // the loop's block past the loop ends.
        let scope = std::mem::take(&mut self.scoped).then(|| {
            let scope = self.new_scope();
            self.insert(
                start,
                lir::Inst::CallRuntime {
                    function: lir::RuntimeFn::Begin,
                    args: vec![scope.clone()],
                },
            );
            self.exits.push(Exit::Scope(scope.clone()));
            scope
        });
        let after = self.new_block();
        self.each_tuple(&src, cx.layout(&element), cx, |this, at_hand, _, cx| {
            this.local_places[variable.0] = Some(LocalPlace {
                value: at_hand,
                holds: false,
            });
            let next = this.new_block();
            this.exits.push(Exit::Loop { next, after });
            this.block(body, cx);
            this.exits.pop();
            this.end_block(lir::Terminator::Jump(next), next);
        });
        self.end_block(lir::Terminator::Jump(after), after);
        if let Some(scope) = scope {
            self.exits.pop();
            self.insts.push(lir::Inst::CallRuntime {
                function: lir::RuntimeFn::End,
                args: vec![scope],
            });
        }
    }

    /// A loop of `body`, run while `condition` is true, checked as `check`
    /// says: a block that computes the condition, in a scope of its own,
    /// each time, and branches to the body's block, which jumps back to
    /// it, or on to one after the loop. A condition known to be false when
    /// compiling runs a `loop`'s block once, and a `while`'s never, and one
    /// known to be true runs it until it leaves, which sema says it can.
    fn loop_statement(
        &mut self,
        condition: &hir::Expr,
        body: &hir::Block,
        check: hir::Check,
        cx: &mut Lowering,
    ) {
        if let hir::Expr::Constant(hir::Value::Boolean(runs)) = condition {
            if !runs && check == hir::Check::Before {
                return;
            }
            let (run, after) = (self.new_block(), self.new_block());
            self.end_block(lir::Terminator::Jump(run), run);
            let next = if *runs { run } else { after };
            self.exits.push(Exit::Loop { next, after });
            self.block(body, cx);
            self.exits.pop();
            self.end_block(lir::Terminator::Jump(next), after);
            return;
        }
        let (test, run, after) = (self.new_block(), self.new_block(), self.new_block());
        let first = match check {
            hir::Check::Before => test,
            hir::Check::After => run,
        };
        self.end_block(lir::Terminator::Jump(first), test);
        // The condition is the loop's, though its body's statements ran
        // before it.
        self.at(self.site, cx);
        // Copied, so that nothing its scope owns is read once it ends.
        let decided = self.slot(lir::Layout::U8);
        self.scoped(|this| {
            let src = this.place(condition, cx);
            this.insts.push(lir::Inst::Copy {
                dst: decided.clone(),
                src,
                layout: lir::Layout::U8,
            });
        });
        self.end_block(
            lir::Terminator::Branch {
                condition: decided,
                then: run,
                otherwise: after,
            },
            run,
        );
        self.exits.push(Exit::Loop { next: test, after });
        self.block(body, cx);
        self.exits.pop();
        self.end_block(lir::Terminator::Jump(test), after);
    }

    /// `if condition then [ … ] else [ … ];`, of the blocks `branches`:
    /// only the one taken, if the condition is a constant, or else a branch
    /// on it, computed in a scope of its own, which ends before either
    /// block runs, to a block of each, then on to one after both.
    fn if_statement(
        &mut self,
        condition: &hir::Expr,
        branches: [&hir::Block; 2],
        block: fn(&mut Self, &hir::Block, &mut Lowering),
        cx: &mut Lowering,
    ) {
        if let hir::Expr::Constant(hir::Value::Boolean(taken)) = condition {
            block(self, branches[usize::from(!taken)], cx);
            return;
        }
        // Copied, so that nothing its scope owns is read once it ends.
        let decided = self.slot(lir::Layout::U8);
        self.scoped(|this| {
            let src = this.place(condition, cx);
            this.insts.push(lir::Inst::Copy {
                dst: decided.clone(),
                src,
                layout: lir::Layout::U8,
            });
        });
        let (then, otherwise, after) = (self.new_block(), self.new_block(), self.new_block());
        self.end_block(
            lir::Terminator::Branch {
                condition: decided,
                then,
                otherwise,
            },
            then,
        );
        for (branch, next) in branches.into_iter().zip([otherwise, after]) {
            block(self, branch, cx);
            self.end_block(lir::Terminator::Jump(after), next);
        }
    }

    /// What `lower` adds, the work of one tuple of a relational operation
    /// or of one call of a read-only operator, in a scope of its own, nested
    /// in the one it is in, if it makes a text, relation or array, as a
    /// relational database's executor resets its per-tuple memory: the scope ends after
    /// the work, freeing what it made, but for the values `lower` gives,
    /// each with its type, which outlive it, and which the scope it is in
    /// is handed first ([`lir::RuntimeFn::HandOut`]). That one needs a
    /// scope then too. So a relational operation's memory is its operands'
    /// and its result's, not its work's, however many tuples it has.
    fn nested_scope(
        &mut self,
        cx: &mut Lowering,
        lower: impl FnOnce(&mut Self, &mut Lowering) -> Vec<(lir::Place, hir::Type)>,
    ) {
        let outer = std::mem::take(&mut self.scoped);
        let start = self.position();
        let kept = lower(self, cx);
        let makes = std::mem::take(&mut self.scoped);
        if makes {
            let scope = self.new_scope();
            self.insert(
                start,
                lir::Inst::CallRuntime {
                    function: lir::RuntimeFn::Begin,
                    args: vec![scope.clone()],
                },
            );
            for (value, ty) in &kept {
                if !cx.points(ty) {
                    continue;
                }
                let descriptor = lir::Place::whole(lir::Base::Data(cx.descriptor(ty)));
                self.insts.push(lir::Inst::CallRuntime {
                    function: lir::RuntimeFn::HandOut,
                    args: vec![value.clone(), descriptor],
                });
            }
            self.insts.push(lir::Inst::CallRuntime {
                function: lir::RuntimeFn::End,
                args: vec![scope],
            });
        }
        self.scoped = outer || makes;
    }

    /// What `lower` adds, in a scope of its own if it needs one, which
    /// begins before its first instruction and ends after its last.
    fn scoped(&mut self, lower: impl FnOnce(&mut Self)) {
        let start = self.position();
        lower(self);
        if std::mem::take(&mut self.scoped) {
            let scope = self.scope_slot();
            self.insert(
                start,
                lir::Inst::CallRuntime {
                    function: lir::RuntimeFn::Begin,
                    args: vec![scope.clone()],
                },
            );
            self.insts.push(lir::Inst::CallRuntime {
                function: lir::RuntimeFn::End,
                args: vec![scope],
            });
        }
    }

    fn call(&mut self, call: &hir::Call, cx: &mut Lowering) {
        let callee = self.program.operator(call.operator);
        match &callee.kind {
            hir::OperatorKind::Builtin(hir::Builtin::Update(builtin)) => {
                self.update_builtin(*builtin, call, cx);
            }
            hir::OperatorKind::Update { .. } | hir::OperatorKind::Specification { .. } => {
                let params = params_of(callee, &call.types);
                let args = self.arguments(&params, call, cx);
                self.call_constraints_checked(call, args.as_ref(), &params, cx);
                // An assignment the operator makes must not free what an
                // argument points to, though it can replace the value of
                // the only variable holding it. One in a cycle of calls holds
                // its own.
                if cx.recursion.cycle(call.operator).is_none()
                    && let Some(args) = &args
                {
                    for (arg, ty) in held_arguments(&params, args, cx) {
                        let descriptor = cx.descriptor(&ty);
                        self.insts.push(lir::Inst::CallRuntime {
                            function: lir::RuntimeFn::Keep,
                            args: vec![arg, lir::Place::whole(lir::Base::Data(descriptor))],
                        });
                        self.scoped = true;
                    }
                }
                self.invoke(call, args, None, cx);
                // Each variable it updates is assigned its parameter's
                // value as it ends, which its keys are checked against.
                for target in call.updated.values() {
                    let holder = self.holder(target, cx);
                    self.check_keys(target, &holder, cx);
                }
            }
            hir::OperatorKind::ReadOnly { .. }
            | hir::OperatorKind::Builtin(hir::Builtin::ReadOnly(_)) => {
                unreachable!("`call` invokes only update operators")
            }
        }
    }

    /// `call`, of the update operator the compiler implements, `builtin`.
    fn update_builtin(&mut self, builtin: hir::UpdateBuiltin, call: &hir::Call, cx: &mut Lowering) {
        match builtin {
            hir::UpdateBuiltin::DebugValue => {
                self.write(&call.args["v"], lir::RuntimeFn::DebugValue, cx);
            }
            hir::UpdateBuiltin::DebugTable => {
                self.write(&call.args["r"], lir::RuntimeFn::DebugTable, cx);
            }
            hir::UpdateBuiltin::LoadTextFile => {
                self.file_loaded(lir::RuntimeFn::LoadTextFile, "lines", call, cx);
            }
            hir::UpdateBuiltin::LoadTextFileWithErrors => {
                let function = lir::RuntimeFn::LoadTextFileWithErrors;
                self.file_loaded_with_errors(function, "lines", call, cx);
            }
            hir::UpdateBuiltin::LoadBinaryFile => {
                self.file_loaded(lir::RuntimeFn::LoadBinaryFile, "bytes", call, cx);
            }
            hir::UpdateBuiltin::LoadBinaryFileWithErrors => {
                let function = lir::RuntimeFn::LoadBinaryFileWithErrors;
                self.file_loaded_with_errors(function, "bytes", call, cx);
            }
            hir::UpdateBuiltin::ReadClock => self.clock_read(lir::RuntimeFn::ReadClock, call, cx),
            hir::UpdateBuiltin::ReadTransactionStart => {
                self.clock_read(lir::RuntimeFn::ReadTransactionStart, call, cx);
            }
            hir::UpdateBuiltin::RandomBytes => self.random_bytes(call, cx),
            hir::UpdateBuiltin::PutText
            | hir::UpdateBuiltin::PutBytes
            | hir::UpdateBuiltin::AppendText
            | hir::UpdateBuiltin::AppendBytes
            | hir::UpdateBuiltin::AppendLines => self.written_out(builtin, call, cx),
            hir::UpdateBuiltin::ReadLine
            | hir::UpdateBuiltin::ReadLineWithErrors
            | hir::UpdateBuiltin::ReadLines
            | hir::UpdateBuiltin::ReadLinesWithErrors
            | hir::UpdateBuiltin::ReadBytes
            | hir::UpdateBuiltin::ReadBytesWithErrors => self.read_in(builtin, call, cx),
            hir::UpdateBuiltin::SaveTextFile => {
                let args = vec![
                    self.path_text(call, cx),
                    self.place(&call.args["lines"], cx),
                ];
                self.insts.push(lir::Inst::CallRuntime {
                    function: lir::RuntimeFn::SaveTextFile,
                    args,
                });
                self.scoped = true;
            }
            hir::UpdateBuiltin::SaveTextFileWithErrors => {
                let errors = self.errors(cx);
                let args = vec![
                    self.path_text(call, cx),
                    self.place(&call.args["lines"], cx),
                    errors.given.clone(),
                ];
                self.insts.push(lir::Inst::CallRuntime {
                    function: lir::RuntimeFn::SaveTextFileWithErrors,
                    args,
                });
                self.scoped = true;
                self.store(&call.updated["errors"], errors.value, cx);
            }
            hir::UpdateBuiltin::LoadCsv | hir::UpdateBuiltin::LoadCsvWithErrors => {
                // Whether the file has a header, and each attribute's
                // column, given their tuple's type's descriptor, and
                // where the line each record starts on goes.
                let header = self.place(&call.args["header"], cx);
                let columns = &call.args["columns"];
                let columns_ty = self.type_of(columns);
                let columns = self.columns(columns, &columns_ty, cx);
                let lines = self.slot(lir::Layout::Struct(vec![
                    lir::Layout::Address,
                    lir::Layout::Usize,
                ]));
                let mut given = vec![header, columns, lines.clone()];
                // And, with `errors`, where they go, and their types.
                let errors =
                    (builtin == hir::UpdateBuiltin::LoadCsvWithErrors).then(|| self.errors(cx));
                let function = match &errors {
                    Some(errors) => {
                        given.push(errors.given.clone());
                        lir::RuntimeFn::LoadCsvWithErrors
                    }
                    None => lir::RuntimeFn::LoadCsv,
                };
                let target = &call.updated["rows"];
                let ty = self.assigned(target).0.clone();
                // Read as the columns say, then converted, each value
                // of a type of the program's own read in another
                // representation than the stored one.
                let hir::Type::Array(record) = &ty else {
                    unreachable!("`load_csv` loads an array");
                };
                let read = hir::Type::Array(Box::new(hir::read_as(record, &columns_ty)));
                let (records, path) = self.loaded(function, call, &read, given, cx);
                let file = InFile { path, lines };
                let errors_given = errors.as_ref().map(|errors| &errors.given);
                let rows = self.records_checked(&records, &read, &ty, &file, errors_given, cx);
                self.store(target, rows, cx);
                if let Some(errors) = errors {
                    self.store(&call.updated["errors"], errors.value, cx);
                }
            }
        }
    }

    /// Where a load's errors go, `relation { error: error }`, as the
    /// runtime's `Errors`, which its runtime function is given: the
    /// relation of them, and its descriptor's address. Their types are in
    /// the table the entry gives the runtime ([`Lowering::failures`]).
    fn errors(&mut self, cx: &mut Lowering) -> Errors {
        let ty = self.program.library.errors_type();
        let given = self.slot(lir::Layout::Struct(vec![
            cx.layout(&ty),
            lir::Layout::Address,
        ]));
        let descriptor = lir::Place::whole(lir::Base::Data(cx.descriptor(&ty)));
        let field = |i| lir::Place {
            path: [given.path.clone(), vec![i]].concat(),
            ..given.clone()
        };
        self.insts.push(lir::Inst::Address {
            dst: field(1),
            src: descriptor,
        });
        Errors {
            value: field(0),
            given,
        }
    }

    /// `load_csv`'s `columns`, written `columns`, of type `ty`, as the
    /// runtime's `Columns`: the tuple's address, and its type's
    /// descriptor's.
    fn columns(&mut self, columns: &hir::Expr, ty: &hir::Type, cx: &mut Lowering) -> lir::Place {
        let tuple = self.place(columns, cx);
        let descriptor = lir::Place::whole(lir::Base::Data(cx.descriptor(ty)));
        let given = self.slot(lir::Layout::Struct(vec![
            lir::Layout::Address,
            lir::Layout::Address,
        ]));
        for (i, src) in [tuple, descriptor].into_iter().enumerate() {
            let dst = lir::Place {
                path: [given.path.clone(), vec![i]].concat(),
                ..given.clone()
            };
            self.insts.push(lir::Inst::Address { dst, src });
        }
        given
    }

    /// What the runtime's `function` reads of the file at `call`'s `path`, an
    /// array, which it makes as [`made_by_runtime`] says, given to the `out`
    /// parameter `read`, `lines` or `bytes`.
    fn file_loaded(
        &mut self,
        function: lir::RuntimeFn,
        read: &str,
        call: &hir::Call,
        cx: &mut Lowering,
    ) {
        let target = &call.updated[read];
        let ty = self.assigned(target).0.clone();
        let (made, _) = self.loaded(function, call, &made_by_runtime(&ty), Vec::new(), cx);
        let array = self.made_of_components(made, &ty, cx);
        self.store(target, array, cx);
    }

    /// As [`FunctionBuilder::file_loaded`], with what it can't read of the
    /// file given to the `out` parameter `errors`.
    fn file_loaded_with_errors(
        &mut self,
        function: lir::RuntimeFn,
        read: &str,
        call: &hir::Call,
        cx: &mut Lowering,
    ) {
        let target = &call.updated[read];
        let ty = self.assigned(target).0.clone();
        let errors = self.errors(cx);
        let given = vec![errors.given.clone()];
        let (made, _) = self.loaded(function, call, &made_by_runtime(&ty), given, cx);
        let array = self.made_of_components(made, &ty, cx);
        self.store(target, array, cx);
        self.store(&call.updated["errors"], errors.value, cx);
    }

    /// The text of the `file_path` that `call` names its `path`, which the
    /// runtime takes, where compiled code reads it, by its own type, in a
    /// hierarchy or out of one.
    fn path_text(&mut self, call: &hir::Call, cx: &mut Lowering) -> lir::Place {
        let path = hir::Expr::Components(Box::new(call.args["path"].clone()));
        self.place(&hir::Expr::Extract(Box::new(path), "text".to_string()), cx)
    }

    /// `call`, of `load_text_file` or `load_csv`: what the runtime function
    /// `function` reads from the file at the path, as the arguments `given`
    /// after it say, into an array the statement holds, of type `ty`, given
    /// its descriptor. The array, and the path.
    fn loaded(
        &mut self,
        function: lir::RuntimeFn,
        call: &hir::Call,
        ty: &hir::Type,
        given: Vec<lir::Place>,
        cx: &mut Lowering,
    ) -> (lir::Place, lir::Place) {
        let path = self.path_text(call, cx);
        let descriptor = lir::Place::whole(lir::Base::Data(cx.descriptor(ty)));
        let array = self.slot(cx.layout(ty));
        let args = [array.clone(), path.clone()]
            .into_iter()
            .chain(given)
            .chain([descriptor])
            .collect();
        self.insts.push(lir::Inst::CallRuntime { function, args });
        self.scoped = true;
        (array, path)
    }

    /// The array of type `ty` of the records of `records`, an array of type
    /// `read` that `load_csv` read from `file`: each value of a type of the
    /// program's own in them selected from its components, read in one of
    /// its possible representations (see [`hir::read_as`]), and each
    /// checked against its type's constraints, as selecting it would, inner
    /// values first. The program stops at the first broken, writing the
    /// line, unless the call gives `errors`, the runtime's `Errors`: then a
    /// record whose value breaks a constraint is skipped, with its error
    /// there. The array is `records` itself, if nothing in them is
    /// converted or skipped, and else a new one the statement holds.
    fn records_checked(
        &mut self,
        records: &lir::Place,
        read: &hir::Type,
        ty: &hir::Type,
        file: &InFile,
        errors: Option<&lir::Place>,
        cx: &mut Lowering,
    ) -> lir::Place {
        let (hir::Type::Array(read_record), hir::Type::Array(record)) = (read, ty) else {
            unreachable!("`load_csv` loads an array");
        };
        if read == ty {
            return records.clone();
        }
        let errors = errors.filter(|_| skippable(record, &cx.families));
        let mut count = records.clone();
        count.path.push(1);
        let rows = self.allocated(ty, count, cx);
        // With errors, where the next record kept goes, after those kept.
        let kept = errors.map(|_| {
            let kept = self.slot(lir::Layout::Usize);
            let zero = usize_constant(0, cx);
            self.insts.push(lir::Inst::Copy {
                dst: kept.clone(),
                src: zero,
                layout: lir::Layout::Usize,
            });
            kept
        });
        self.each_tuple(
            records,
            cx.layout(read_record),
            cx,
            |this, read, index, cx| {
                let row = this.element(&rows, kept.as_ref().unwrap_or(index), cx.layout(record));
                // A record skipped goes on after the record.
                let after = this.new_block();
                let at = Record {
                    file,
                    index: index.clone(),
                    skip: errors.map(|errors| Skip {
                        errors: errors.clone(),
                        to: after,
                    }),
                };
                let value = Reading {
                    dst: row,
                    src: read,
                    name: String::new(),
                    at: &at,
                };
                this.record_value(&value, record, read_record, cx);
                if let Some(kept) = &kept {
                    this.insts.push(lir::Inst::Increment {
                        place: kept.clone(),
                    });
                }
                this.end_block(lir::Terminator::Jump(after), after);
            },
        );
        if let Some(kept) = kept {
            let mut count = rows.clone();
            count.path.push(1);
            self.insts.push(lir::Inst::Copy {
                dst: count,
                src: kept,
                layout: lir::Layout::Usize,
            });
        }
        self.adopt(&rows, ty, cx);
        rows
    }

    /// The value of type `ty` that `value` says where to write, which
    /// `load_csv` read as `read`: written there, converted if `read` is
    /// another type, and checked, with each value of a type of the program's
    /// own in it, inner first (see [`FunctionBuilder::records_checked`]). A
    /// value of a type of the
    /// program's own is read as a tuple of the components of the
    /// representation the columns named, each read in turn, and, of another
    /// than the stored one, converted by its conversion to it.
    fn record_value(
        &mut self,
        value: &Reading,
        ty: &hir::Type,
        read: &hir::Type,
        cx: &mut Lowering,
    ) {
        if ty == read {
            self.insts.push(lir::Inst::Copy {
                dst: value.dst.clone(),
                src: value.src.clone(),
                layout: cx.layout(ty),
            });
            return;
        }
        let hir::Type::Tuple(fields) = read else {
            unreachable!("only a tuple is read as a value of another type");
        };
        let defined = match ty {
            hir::Type::Tuple(heading) => {
                self.attributes_read(value, heading, fields, cx);
                return;
            }
            hir::Type::Defined(defined) => defined,
            _ => unreachable!("only tuples and types' values are read as tuples"),
        };
        // Where its stored components go: where it goes, or, in a
        // hierarchy, a place of their own, which it is then made of.
        let in_hierarchy = ty.root().is_some();
        let narrowed = cx.families.stored(defined);
        let target = if in_hierarchy || narrowed.is_some() {
            let stored = hir::Type::Tuple(defined.components.clone());
            Reading {
                dst: self.slot(cx.layout(&stored)),
                ..value.clone()
            }
        } else {
            value.clone()
        };
        let names: Vec<&str> = fields.named().map(|(c, _)| c).collect();
        match defined.representation_with(&names) {
            Some((own, None)) => self.attributes_read(&target, own, fields, cx),
            Some((own, Some(other))) => {
                let components = Reading {
                    dst: self.slot(cx.layout(&hir::Type::Tuple(own.clone()))),
                    ..value.clone()
                };
                self.attributes_read(&components, own, fields, cx);
                // Its representation's own constraints first, of them as
                // read.
                if !other.constraints.is_empty() {
                    let checking = self.read_checking(&value.name, value.at, cx);
                    self.represented(defined, other, &components.dst, &checking, cx);
                }
                let stored = self.converted(other.to, own, components.dst, cx);
                self.insts.push(lir::Inst::Copy {
                    dst: target.dst.clone(),
                    src: stored,
                    layout: cx.layout(&hir::Type::Tuple(defined.components.clone())),
                });
            }
            None => unreachable!("a type's value is read in one of its representations"),
        }
        if defined.is_constrained() || in_hierarchy {
            let checking = self.read_checking(&value.name, value.at, cx);
            if defined.is_constrained() {
                self.constrained(defined, &target.dst, &checking, cx);
            }
            if in_hierarchy {
                self.made_in_hierarchy(defined, &target.dst, &value.dst, &checking, cx);
            }
        }
        if let Some(stored) = &narrowed {
            self.stored_copied(stored, &target.dst, &value.dst, false, cx);
        }
    }

    /// The value at `dst` of `defined`, a type in a hierarchy, made of the
    /// components at `components`: its own type's descriptor's address,
    /// then them.
    fn made_in_hierarchy(
        &mut self,
        defined: &hir::Defined,
        components: &lir::Place,
        dst: &lir::Place,
        checking: &Checking,
        cx: &mut Lowering,
    ) {
        let mut at = dst.clone();
        at.path.extend(cx.families.components_path(defined));
        self.insts.push(lir::Inst::Copy {
            dst: at,
            src: components.clone(),
            layout: cx.layout(&hir::Type::Tuple(defined.components.clone())),
        });
        self.tag_most_specific(defined, components, dst, checking, cx);
    }

    /// Write at `dst`, a value in a hierarchy, the address of the descriptor
    /// of the most specific type of the value of `defined` whose components
    /// are at `components`, of which `defined`'s constraints are true (*Type
    /// Inheritance and Relational Theory*, IM Prescription 10): of its
    /// subtypes defined by constraints, the one whose specialization
    /// constraints are true of them too, its most specific type in turn, or
    /// else `defined` itself. A value of two of them stops the program,
    /// writing both (R0020): a value has one most specific type; one read
    /// from a CSV file whose call gives errors is skipped, with its error.
    fn tag_most_specific(
        &mut self,
        defined: &hir::Defined,
        components: &lir::Place,
        dst: &lir::Place,
        checking: &Checking,
        cx: &mut Lowering,
    ) {
        let subtypes = cx.families.constrained(defined).to_vec();
        let holds: Vec<lir::Place> = subtypes
            .iter()
            .map(|subtype| self.all_hold(subtype, components, cx))
            .collect();
        for (i, first) in subtypes.iter().enumerate() {
            for (j, second) in subtypes.iter().enumerate().skip(i + 1) {
                let (both, next) = (self.new_block(), self.new_block());
                self.end_block(
                    lir::Terminator::Branch {
                        condition: holds[i].clone(),
                        then: both,
                        otherwise: next,
                    },
                    both,
                );
                let failed = self.new_block();
                self.end_block(
                    lir::Terminator::Branch {
                        condition: holds[j].clone(),
                        then: failed,
                        otherwise: next,
                    },
                    failed,
                );
                let value = components.clone();
                let descriptor =
                    lir::Place::whole(lir::Base::Data(cx.components_descriptor(defined)));
                let (first, second) = (self.text(&first.name, cx), self.text(&second.name, cx));
                let broken = match checking {
                    Checking::Read {
                        record,
                        skip: Some(skip),
                    } => Broken::Skip {
                        function: lir::RuntimeFn::OverlappingSubtypesError,
                        args: vec![
                            skip.errors.clone(),
                            value,
                            descriptor,
                            first,
                            second,
                            record.clone(),
                        ],
                        to: skip.to,
                    },
                    Checking::Selected | Checking::Read { .. } => {
                        Broken::Fail(Box::new(lir::Failure::OverlappingSubtypes {
                            value,
                            descriptor,
                            first,
                            second,
                        }))
                    }
                };
                self.broken(broken, next);
            }
        }
        let after = self.new_block();
        for (subtype, holds) in subtypes.iter().zip(holds) {
            let (of_it, not_of_it) = (self.new_block(), self.new_block());
            self.end_block(
                lir::Terminator::Branch {
                    condition: holds,
                    then: of_it,
                    otherwise: not_of_it,
                },
                of_it,
            );
            self.tag_most_specific(subtype, components, dst, checking, cx);
            self.end_block(lir::Terminator::Jump(after), not_of_it);
        }
        let own = lir::Place::whole(lir::Base::Data(cx.components_descriptor(defined)));
        let mut tag = dst.clone();
        tag.path.push(0);
        self.insts.push(lir::Inst::Address { dst: tag, src: own });
        self.end_block(lir::Terminator::Jump(after), after);
    }

    /// Whether the specialization constraints of `subtype`, a type defined
    /// by constraints, are all true of the components at `components`, in
    /// a new [`lir::Layout::U8`]: each computed until one is false.
    fn all_hold(
        &mut self,
        subtype: &hir::Defined,
        components: &lir::Place,
        cx: &mut Lowering,
    ) -> lir::Place {
        let result = self.slot(lir::Layout::U8);
        self.insts.push(lir::Inst::Copy {
            dst: result.clone(),
            src: u8_constant(0, cx),
            layout: lir::Layout::U8,
        });
        let done = self.new_block();
        for constraint in subtype.own_constraints() {
            let holds = self.holds(constraint, &subtype.components, components, cx);
            let next = self.new_block();
            self.end_block(
                lir::Terminator::Branch {
                    condition: holds,
                    then: next,
                    otherwise: done,
                },
                next,
            );
        }
        self.insts.push(lir::Inst::Copy {
            dst: result.clone(),
            src: u8_constant(1, cx),
            layout: lir::Layout::U8,
        });
        self.end_block(lir::Terminator::Jump(done), done);
        result
    }

    /// Whether `constraint` is true of the components at `value`, laid out
    /// as the tuple of them, `components`, a value's or a representation's:
    /// a call of its operator with the components as its arguments, in a
    /// new place.
    fn holds(
        &mut self,
        constraint: &hir::Constraint,
        components: &hir::Heading,
        value: &lir::Place,
        cx: &mut Lowering,
    ) -> lir::Place {
        let at_hand = hir::Expr::Current(0, components.clone());
        let args: BTreeMap<String, hir::Expr> = components
            .attributes()
            .map(|(name, _)| {
                let arg = hir::Expr::Extract(Box::new(at_hand.clone()), name.to_string());
                (name.to_string(), arg)
            })
            .collect();
        let call = hir::Expr::Call(self.program.call_of(constraint.operator, args));
        self.tuples.push(AtHand::one(value.clone()));
        let holds = self.place(&call, cx);
        self.tuples.pop();
        holds
    }

    /// The tuple the conversion `operator`, of a type of the program's own,
    /// computes from the components `heading` of a value at `value`, its
    /// arguments, by name, in a new place.
    fn converted(
        &mut self,
        operator: hir::OperatorId,
        heading: &hir::Heading,
        value: lir::Place,
        cx: &mut Lowering,
    ) -> lir::Place {
        let at_hand = hir::Expr::Current(0, heading.clone());
        let args = heading
            .attributes()
            .map(|(c, _)| {
                let arg = hir::Expr::Extract(Box::new(at_hand.clone()), c.to_string());
                (c.to_string(), arg)
            })
            .collect();
        let conversion = hir::Expr::Call(self.program.call_of(operator, args));
        self.tuples.push(AtHand::one(value));
        let converted = self.place(&conversion, cx);
        self.tuples.pop();
        converted
    }

    /// Each attribute of a tuple of `heading`, or component of a value of a
    /// type of the program's own, in the value `value` says where to write,
    /// read as `read`, a heading with the same names (see
    /// [`FunctionBuilder::record_value`]).
    fn attributes_read(
        &mut self,
        value: &Reading,
        heading: &hir::Heading,
        read: &hir::Heading,
        cx: &mut Lowering,
    ) {
        let attributes = heading.attributes().zip(read.attributes());
        for (i, ((attribute, ty), (_, read))) in attributes.enumerate() {
            let mut inner = Reading {
                name: dotted(&value.name, attribute),
                ..value.clone()
            };
            inner.dst.path.push(i);
            inner.src.path.push(i);
            self.record_value(&inner, ty, read, cx);
        }
    }

    /// How a value named `name`, in a record of `at`, is checked against
    /// its type's components' keys and constraints, and the subtypes it
    /// may be of: as one read from the record, which a place laid out as
    /// [`lir::Layout::record`] describes, made here, which stops the program
    /// at the first broken, writing the record's line, or, if its call gives
    /// errors, skips the record, with its error.
    fn read_checking(&mut self, name: &str, at: &Record, cx: &mut Lowering) -> Checking {
        // Laid out as `lir::Layout::record`.
        let record = self.slot(lir::Layout::record());
        let attribute = self.text(name, cx);
        let line = self.element(&at.file.lines, &at.index, lir::Layout::Usize);
        let text = scalar_layout(hir::Scalar::Text);
        for (i, (src, layout)) in [
            (attribute, text.clone()),
            (at.file.path.clone(), text),
            (line, lir::Layout::Usize),
        ]
        .into_iter()
        .enumerate()
        {
            let mut dst = record.clone();
            dst.path.push(i);
            self.insts.push(lir::Inst::Copy { dst, src, layout });
        }
        Checking::Read {
            record,
            skip: at.skip.clone(),
        }
    }

    /// A call of the runtime function `function`, which writes `value`,
    /// given its address and its type's descriptor.
    fn write(&mut self, value: &hir::Expr, function: lir::RuntimeFn, cx: &mut Lowering) {
        let ty = self.type_of(value);
        let value = self.place(value, cx);
        let descriptor = lir::Place::whole(lir::Base::Data(cx.descriptor(&ty)));
        self.insts.push(lir::Inst::CallRuntime {
            function,
            args: vec![value, descriptor],
        });
    }

    /// The arguments of `call`, for `params`: a tuple, built in a stack
    /// slot in parameter name order, whose address is passed, if there are
    /// any. Each `const` parameter's is the argument's value, and each other
    /// one's the address of its variable.
    fn arguments(
        &mut self,
        params: &BTreeMap<String, hir::Parameter>,
        call: &hir::Call,
        cx: &mut Lowering,
    ) -> Option<lir::Place> {
        let slot = self.slot(params_layout(params, &cx.families)?);
        for (i, (name, param)) in params.iter().enumerate() {
            let mut dst = slot.clone();
            dst.path.push(i);
            match param.mode {
                hir::Mode::Const => {
                    let src = self.place(&call.args[name], cx);
                    self.insts.push(lir::Inst::Copy {
                        dst,
                        src,
                        layout: cx.layout(&param.ty),
                    });
                }
                hir::Mode::Var | hir::Mode::Out => {
                    let src = self.holder(&call.updated[name], cx);
                    self.insts.push(lir::Inst::Address { dst, src });
                }
            }
        }
        Some(slot)
    }

    /// The value of `call`, of a read-only operator, computed into a new
    /// stack slot. What the operator makes is the statement's.
    fn read_only_call(&mut self, call: &hir::Call, cx: &mut Lowering) -> lir::Place {
        let callee = self.program.operator(call.operator);
        match callee.kind {
            hir::OperatorKind::Builtin(hir::Builtin::ReadOnly(builtin)) => {
                return self.builtin_value(builtin, call, cx);
            }
            hir::OperatorKind::Builtin(hir::Builtin::Update(_)) => {
                unreachable!("only read-only operators are called in expressions")
            }
            hir::OperatorKind::ReadOnly { .. }
            | hir::OperatorKind::Update { .. }
            | hir::OperatorKind::Specification { .. } => {}
        }
        if let Some(asked) = self.asks_if_empty(call) {
            let inner = self.view_of(asked).inner;
            let (mut count, _) = self.loose_place(inner, cx);
            count.path.push(1);
            let dst = self.slot(lir::Layout::U8);
            let zero = usize_constant(0, cx);
            self.insts.push(lir::Inst::Binary {
                dst: dst.clone(),
                op: lir::BinaryOp::Compare(lir::Comparison::Equal, lir::Operand::Usize),
                lhs: count,
                rhs: zero,
            });
            return dst;
        }
        let params = params_of(callee, &call.types);
        let args = self.arguments(&params, call, cx);
        self.call_constraints_checked(call, args.as_ref(), &params, cx);
        let dst = self.slot(cx.layout(&self.program.result(call)));
        self.invoke(call, args, Some(&dst), cx);
        dst
    }

    /// Stop the program, where `call` is, unless each constraint in the
    /// braces of the operator it calls is true of its arguments, at `args`,
    /// for `params`: a call of the read-only operator of the parameters it
    /// reads, which it is given as a tuple, as the error shows them.
    fn call_constraints_checked(
        &mut self,
        call: &hir::Call,
        args: Option<&lir::Place>,
        params: &BTreeMap<String, hir::Parameter>,
        cx: &mut Lowering,
    ) {
        let callee = self.program.operator(call.operator);
        for constraint in &callee.constraints {
            let reads = params_of(self.program.operator(constraint.operator), &call.types);
            let layout =
                params_layout(&reads, &cx.families).expect("a constraint reads a parameter");
            let given = self.slot(layout);
            for (i, (name, param)) in reads.iter().enumerate() {
                let mut dst = given.clone();
                dst.path.push(i);
                // A `var` parameter's is its variable's value as the call
                // starts, as the operator sees it first.
                let src = if params[name].mode == hir::Mode::Const {
                    let index = params
                        .keys()
                        .position(|p| p == name)
                        .expect("a constraint's parameters are its operator's");
                    let mut arg = args.expect("the operator has parameters").clone();
                    arg.path.push(index);
                    arg
                } else {
                    self.holder(&call.updated[name], cx)
                };
                self.insts.push(lir::Inst::Copy {
                    dst,
                    src,
                    layout: cx.layout(&param.ty),
                });
            }
            let holds = self.slot(lir::Layout::U8);
            self.call_function(
                constraint.operator,
                &call.types,
                Some(given.clone()),
                Some(holds.clone()),
                cx,
            );
            let (kept, failed) = (self.new_block(), self.new_block());
            self.end_block(
                lir::Terminator::Branch {
                    condition: holds,
                    then: kept,
                    otherwise: failed,
                },
                failed,
            );
            let heading = hir::Type::Tuple(hir::Heading::new(
                reads
                    .iter()
                    .map(|(name, p)| (name.clone(), p.ty.clone()))
                    .collect(),
            ));
            let descriptor = lir::Place::whole(lir::Base::Data(cx.descriptor(&heading)));
            let operator = self.text(&callee.name, cx);
            let written = self.text(&constraint.text, cx);
            self.broken(
                Broken::Fail(Box::new(lir::Failure::BrokenCallConstraint {
                    value: given,
                    descriptor,
                    operator,
                    constraint: written,
                })),
                kept,
            );
        }
    }

    /// Call the operator `call` names, with the argument struct `args`,
    /// its value, if it is read-only, into `result`.
    fn invoke(
        &mut self,
        call: &hir::Call,
        args: Option<lir::Place>,
        result: Option<&lir::Place>,
        cx: &mut Lowering,
    ) {
        self.for_version(call, args, cx, |this, operator, args, cx| {
            this.call_function(operator, &call.types, args, result.cloned(), cx);
        });
    }

    /// What `lower` adds for the operator that `call` runs, given the argument
    /// struct `args`: of its versions, the first whose parameters the arguments'
    /// values are of, and otherwise itself. A specification without a body has a
    /// version for whatever the arguments' values are of, so the last is run,
    /// untested, if none before it is.
    fn for_version(
        &mut self,
        call: &hir::Call,
        args: Option<lir::Place>,
        cx: &mut Lowering,
        lower: impl Fn(&mut Self, hir::OperatorId, Option<lir::Place>, &mut Lowering),
    ) {
        let callee = self.program.operator(call.operator);
        if callee.versions.is_empty() {
            lower(self, call.operator, args, cx);
            return;
        }
        let params = params_of(callee, &call.types);
        let bodiless = matches!(callee.kind, hir::OperatorKind::Specification { .. });
        let done = self.new_block();
        for (i, version) in callee.versions.iter().enumerate() {
            let next = self.new_block();
            if !(bodiless && i + 1 == callee.versions.len()) {
                for (name, ty) in &version.params {
                    let index = params
                        .keys()
                        .position(|p| p == name)
                        .expect("a version's parameters are its specification's");
                    let mut arg = args.clone().expect("a version has parameters");
                    arg.path.push(index);
                    let fits = self.is_of(&arg, ty, cx);
                    let tested = self.new_block();
                    self.end_block(
                        lir::Terminator::Branch {
                            condition: fits,
                            then: tested,
                            otherwise: next,
                        },
                        tested,
                    );
                }
            }
            lower(self, version.operator, args.clone(), cx);
            self.end_block(lir::Terminator::Jump(done), next);
        }
        if !bodiless {
            lower(self, call.operator, args, cx);
        }
        self.end_block(lir::Terminator::Jump(done), done);
    }

    /// Call the function of the operator `operator`, instantiated with
    /// `types`, with the argument struct `args`, its value into `result`.
    fn call_function(
        &mut self,
        operator: hir::OperatorId,
        types: &[hir::Type],
        args: Option<lir::Place>,
        result: Option<lir::Place>,
        cx: &mut Lowering,
    ) {
        let (function, makes) = function(self.program, self.variable_data, operator, types, cx);
        self.scoped |= makes;
        // It holds its arguments itself (see `in_cycle`).
        if cx.recursion.cycle(operator).is_some()
            && let Some(args) = &args
        {
            let params = params_of(self.program.operator(operator), types);
            for (arg, ty) in held_arguments(&params, args, cx) {
                let descriptor = lir::Place::whole(lir::Base::Data(cx.descriptor(&ty)));
                self.insts.push(lir::Inst::CallRuntime {
                    function: lir::RuntimeFn::Retain,
                    args: vec![arg, descriptor],
                });
            }
        }
        // One being lowered, in a cycle of calls, may fail.
        let can_fail = !cx.unfailing.contains(&function.0);
        self.insts.push(lir::Inst::Call {
            function,
            args,
            result,
            can_fail,
        });
        // The callee leaves the program at its own site.
        if can_fail {
            self.after_call(cx);
        }
    }

    /// The value of `call`, of the read-only built-in `builtin`, computed
    /// into a new stack slot.
    fn builtin_value(
        &mut self,
        builtin: hir::ReadOnlyBuiltin,
        call: &hir::Call,
        cx: &mut Lowering,
    ) -> lir::Place {
        match builtin {
            // The relation's or array's count, which it keeps after its
            // tuples or elements.
            hir::ReadOnlyBuiltin::Count | hir::ReadOnlyBuiltin::Length => {
                let operand = call.args.values().next().expect("one argument");
                let mut count = if builtin == hir::ReadOnlyBuiltin::Count {
                    let inner = self.view_of(operand).inner;
                    self.loose_place(inner, cx).0
                } else {
                    self.place(operand, cx)
                };
                count.path.push(1);
                let dst = self.slot(lir::Layout::I64);
                self.insts.push(lir::Inst::Unary {
                    dst: dst.clone(),
                    op: lir::UnaryOp::Integer,
                    src: count,
                });
                dst
            }
            hir::ReadOnlyBuiltin::Min => {
                let r = &call.args["r"];
                self.extreme(r, hir::Comparison::Less, lir::Failure::NoMin, cx)
            }
            hir::ReadOnlyBuiltin::Max => {
                let r = &call.args["r"];
                self.extreme(r, hir::Comparison::Greater, lir::Failure::NoMax, cx)
            }
            hir::ReadOnlyBuiltin::Sum => self.numbers(lir::RuntimeFn::Sum, call, cx),
            hir::ReadOnlyBuiltin::Product => self.numbers(lir::RuntimeFn::Product, call, cx),
            hir::ReadOnlyBuiltin::ToText => {
                let from = &call.args["from"];
                let ty = self.type_of(from);
                let value = self.place(from, cx);
                let descriptor = lir::Place::whole(lir::Base::Data(cx.descriptor(&ty)));
                let dst = self.slot(scalar_layout(hir::Scalar::Text));
                self.insts.push(lir::Inst::CallRuntime {
                    function: lir::RuntimeFn::ToText,
                    args: vec![dst.clone(), value, descriptor],
                });
                self.scoped = true;
                dst
            }
            hir::ReadOnlyBuiltin::ToApproximate => {
                let src = self.place(&call.args["from"], cx);
                let dst = self.slot(lir::Layout::F64Bits);
                self.insts.push(lir::Inst::Unary {
                    dst: dst.clone(),
                    op: lir::UnaryOp::Approximate,
                    src,
                });
                dst
            }
            hir::ReadOnlyBuiltin::ParseInteger => {
                self.parsed(lir::RuntimeFn::ParseInteger, call, cx)
            }
            hir::ReadOnlyBuiltin::ParseApproximate => {
                self.parsed(lir::RuntimeFn::ParseApproximate, call, cx)
            }
            hir::ReadOnlyBuiltin::ParseBoolean => {
                self.parsed(lir::RuntimeFn::ParseBoolean, call, cx)
            }
            hir::ReadOnlyBuiltin::ToCodePoints => self.code_points(call, cx),
            hir::ReadOnlyBuiltin::FromCodePoints => self.text_of_code_points(call, cx),
            hir::ReadOnlyBuiltin::ToCharacters => self.characters(call, cx),
            hir::ReadOnlyBuiltin::FromCharacters => self.text_of_characters(call, cx),
            hir::ReadOnlyBuiltin::Sqrt => {
                self.mathematical(lir::RuntimeFn::Sqrt, call, &["of"], cx)
            }
            hir::ReadOnlyBuiltin::Sin => self.mathematical(lir::RuntimeFn::Sin, call, &["of"], cx),
            hir::ReadOnlyBuiltin::Cos => self.mathematical(lir::RuntimeFn::Cos, call, &["of"], cx),
            hir::ReadOnlyBuiltin::Atan2 => {
                self.mathematical(lir::RuntimeFn::Atan2, call, &["y", "x"], cx)
            }
        }
    }

    /// The value of `call`, of `to_code_points`: the array of its text's
    /// code points, as `hir::ReadOnlyBuiltin::evaluate` gives it, which the
    /// runtime makes as [`made_by_runtime`] says.
    fn code_points(&mut self, call: &hir::Call, cx: &mut Lowering) -> lir::Place {
        let ty = self.program.result(call);
        let made = made_by_runtime(&ty);
        let text = self.place(&call.args["from"], cx);
        let descriptor = lir::Place::whole(lir::Base::Data(cx.descriptor(&made)));
        let points = self.slot(cx.layout(&made));
        self.insts.push(lir::Inst::CallRuntime {
            function: lir::RuntimeFn::ToCodePoints,
            args: vec![points.clone(), text, descriptor],
        });
        self.scoped = true;
        self.made_of_components(points, &ty, cx)
    }

    /// The array of type `ty` of what a runtime function made at `made`, of
    /// the type [`made_by_runtime`] gives for it: `made` itself, or, for one
    /// of a type in a hierarchy, made of their components, a new array the
    /// statement holds, each made as selecting it would make it, of its most
    /// specific type.
    fn made_of_components(
        &mut self,
        made: lir::Place,
        ty: &hir::Type,
        cx: &mut Lowering,
    ) -> lir::Place {
        let hir::Type::Array(element) = ty else {
            return made;
        };
        let hir::Type::Defined(defined) = &**element else {
            return made;
        };
        if element.root().is_none() {
            return made;
        }
        let components = hir::Type::Tuple(defined.components.clone());
        let mut count = made.clone();
        count.path.push(1);
        let values = self.allocated(ty, count, cx);
        let layout = cx.layout(element);
        self.each_tuple(
            &made,
            cx.layout(&components),
            cx,
            |this, value, index, cx| {
                let dst = this.element(&values, index, layout);
                this.made_in_hierarchy(defined, &value, &dst, &Checking::Selected, cx);
            },
        );
        values
    }

    /// The value of `call`, of `from_code_points`: the text of its array's
    /// code points, as `hir::ReadOnlyBuiltin::evaluate` gives it, which the
    /// runtime reads of their stored components. An array of a type in a
    /// hierarchy has them copied to a new array of them first.
    fn text_of_code_points(&mut self, call: &hir::Call, cx: &mut Lowering) -> lir::Place {
        let from = &call.args["from"];
        let ty = self.type_of(from);
        let hir::Type::Array(element) = &ty else {
            unreachable!("`from_code_points` takes an array");
        };
        let hir::Type::Defined(code_point) = &**element else {
            unreachable!("`from_code_points` takes code points");
        };
        let components = hir::Type::Tuple(code_point.components.clone());
        let stored = if element.root().is_none() {
            ty.clone()
        } else {
            hir::Type::Array(Box::new(components.clone()))
        };
        let array = self.place(from, cx);
        let points = if element.root().is_none() {
            array
        } else {
            let mut count = array.clone();
            count.path.push(1);
            let points = self.allocated(&stored, count, cx);
            let layout = cx.layout(&components);
            let path = cx.families.components_path(code_point);
            self.each_tuple(&array, cx.layout(element), cx, |this, point, index, _| {
                let dst = this.element(&points, index, layout.clone());
                let mut src = point;
                src.path.extend(path);
                this.insts.push(lir::Inst::Copy { dst, src, layout });
            });
            points
        };
        let descriptor = lir::Place::whole(lir::Base::Data(cx.descriptor(&stored)));
        let dst = self.slot(scalar_layout(hir::Scalar::Text));
        self.insts.push(lir::Inst::CallRuntime {
            function: lir::RuntimeFn::FromCodePoints,
            args: vec![dst.clone(), points, descriptor],
        });
        self.scoped = true;
        dst
    }

    /// The value of `call`, of `to_characters`: the array of its text's characters,
    /// as `hir::ReadOnlyBuiltin::evaluate` gives it.
    fn characters(&mut self, call: &hir::Call, cx: &mut Lowering) -> lir::Place {
        let ty = self.program.result(call);
        let text = self.place(&call.args["from"], cx);
        let descriptor = lir::Place::whole(lir::Base::Data(cx.descriptor(&ty)));
        let characters = self.slot(cx.layout(&ty));
        self.insts.push(lir::Inst::CallRuntime {
            function: lir::RuntimeFn::ToCharacters,
            args: vec![characters.clone(), text, descriptor],
        });
        self.scoped = true;
        characters
    }

    /// The value of `call`, of `from_characters`: the text of its array's characters,
    /// as `hir::ReadOnlyBuiltin::evaluate` gives it.
    fn text_of_characters(&mut self, call: &hir::Call, cx: &mut Lowering) -> lir::Place {
        let array = self.place(&call.args["from"], cx);
        let dst = self.slot(scalar_layout(hir::Scalar::Text));
        self.insts.push(lir::Inst::CallRuntime {
            function: lir::RuntimeFn::FromCharacters,
            args: vec![dst.clone(), array],
        });
        self.scoped = true;
        dst
    }

    /// The value of `call`, of `sqrt`, `sin`, `cos` or `atan2`, an
    /// approximate number computed into a new stack slot by the runtime
    /// function `function` from the arguments for `operands`, in order, as
    /// `hir::ReadOnlyBuiltin::evaluate` computes it.
    fn mathematical(
        &mut self,
        function: lir::RuntimeFn,
        call: &hir::Call,
        operands: &[&str],
        cx: &mut Lowering,
    ) -> lir::Place {
        let dst = self.slot(lir::Layout::F64Bits);
        let mut args = vec![dst.clone()];
        for operand in operands {
            args.push(self.place(&call.args[*operand], cx));
        }
        self.insts.push(lir::Inst::CallRuntime { function, args });
        dst
    }

    /// The value of `call`, of `parse_integer`, `parse_approximate` or
    /// `parse_boolean`, computed into a new stack slot by the runtime
    /// function `function`, which reads its text as
    /// `hir::ReadOnlyBuiltin::evaluate` does.
    fn parsed(
        &mut self,
        function: lir::RuntimeFn,
        call: &hir::Call,
        cx: &mut Lowering,
    ) -> lir::Place {
        let text = self.place(&call.args["from"], cx);
        let dst = self.slot(cx.layout(&self.program.result(call)));
        self.insts.push(lir::Inst::CallRuntime {
            function,
            args: vec![dst.clone(), text],
        });
        dst
    }

    /// The value of `call`, of `sum` or `product`, computed into a new
    /// stack slot by the runtime function `function`, which computes it as
    /// `hir::ReadOnlyBuiltin::evaluate` does, from the relation, its descriptor and
    /// the position of `of` among its attributes.
    fn numbers(
        &mut self,
        function: lir::RuntimeFn,
        call: &hir::Call,
        cx: &mut Lowering,
    ) -> lir::Place {
        let r = self.view_of(&call.args["r"]);
        let hir::Type::Relation(heading) = &r.named else {
            unreachable!("`sum` and `product` take relations");
        };
        let of = heading.get("of").expect("`of` is an attribute");
        let index = r.shape.position("of").expect("`of` is an attribute");
        let dst = self.slot(cx.layout(of));
        let (src, _) = self.loose_place(r.inner, cx);
        let descriptor = lir::Place::whole(lir::Base::Data(cx.descriptor(&r.ty)));
        let index = usize_constant(len(index), cx);
        self.insts.push(lir::Inst::CallRuntime {
            function,
            args: vec![dst.clone(), src, descriptor, index],
        });
        dst
    }

    /// The value of the attribute `of` of the relation `r`'s tuples that
    /// is `comparison` (`<` for the least, `>` for the greatest) every
    /// other, computed into a new stack slot: the first tuple's, replaced
    /// by each tuple's that is `comparison` it. The program stops with
    /// `failure` if the relation has no tuples.
    fn extreme(
        &mut self,
        r: &hir::Expr,
        comparison: hir::Comparison,
        failure: lir::Failure,
        cx: &mut Lowering,
    ) -> lir::Place {
        let ty = self.type_of(r);
        let hir::Type::Relation(heading) = &ty else {
            unreachable!("`min` and `max` take relations");
        };
        let of_ty = heading.get("of").expect("`of` is an attribute").clone();
        let of = attribute_names(&ty)
            .position(|a| a == "of")
            .expect("`of` is an attribute");
        let tuple = cx.layout(&hir::Type::Tuple(heading.clone()));
        let value = cx.layout(&of_ty);
        // Two different values can tie in a type's own order, and the first is the one.
        let src = if of_ty.orders_as_stored() {
            self.loose_place(r, cx).0
        } else {
            self.place(r, cx)
        };
        let mut count = src.clone();
        count.path.push(1);
        let zero = usize_constant(0, cx);
        let empty = self.slot(lir::Layout::U8);
        self.insts.push(lir::Inst::Binary {
            dst: empty.clone(),
            op: lir::BinaryOp::Compare(lir::Comparison::Equal, lir::Operand::Usize),
            lhs: count,
            rhs: zero.clone(),
        });
        let (none, some) = (self.new_block(), self.new_block());
        self.end_block(
            lir::Terminator::Branch {
                condition: empty,
                then: none,
                otherwise: some,
            },
            none,
        );
        self.fail(failure, some);
        let mut first = self.element(&src, &zero, tuple.clone());
        first.path.push(of);
        let best = self.slot(value.clone());
        self.insts.push(lir::Inst::Copy {
            dst: best.clone(),
            src: first,
            layout: value.clone(),
        });
        let compare = hir::BinaryOp::Compare(comparison, hir::Compared::of(&of_ty));
        self.each_tuple(&src, tuple, cx, |this, mut at_hand, _, cx| {
            at_hand.path.push(of);
            let better = this.binary(compare, &of_ty, at_hand.clone(), best.clone(), cx);
            let (replace, next) = (this.new_block(), this.new_block());
            this.end_block(
                lir::Terminator::Branch {
                    condition: better,
                    then: replace,
                    otherwise: next,
                },
                replace,
            );
            this.insts.push(lir::Inst::Copy {
                dst: best.clone(),
                src: at_hand,
                layout: value,
            });
            this.end_block(lir::Terminator::Jump(next), next);
        });
        best
    }

    /// Assignments made at once, each to a different variable. Each value
    /// is computed and copied into a slot of its own, since it can be in a
    /// variable that another assignment changes; an array's that is the
    /// array with elements changed, as `a[i] := x` assigns `a`, has each
    /// change's positions and value computed instead, to make it in place.
    /// Then each variable's value is checked against its keys, and each
    /// array's changes against each other, and only then assigned: whole
    /// values first, which take a reference to what they point to, so that
    /// a change in place copies any buffer they hold.
    fn assign(&mut self, assignments: &[hir::Assignment], cx: &mut Lowering) {
        // An `insert` or a `delete` alone: with others, they read the
        // variable as it was.
        if let [assignment] = assignments {
            if let Some((tuples, combination)) = self.combined_with_itself(assignment) {
                self.changed_by_tuples(&assignment.target, tuples, combination, cx);
                return;
            }
            if let Some(condition) = self.restricted_itself(assignment) {
                self.delete_unless(&assignment.target, condition, cx);
                return;
            }
        }
        let mut values: Vec<(hir::Target, lir::Place)> = Vec::new();
        let mut changed: Vec<(hir::Target, Vec<Computed>)> = Vec::new();
        for assignment in assignments {
            if let hir::Expr::WithElements(array, changes) = &assignment.value
                && **array == assignment.target.read()
            {
                let computed: Vec<Computed> =
                    changes.iter().map(|c| self.computed(c, cx)).collect();
                self.check_overlaps(changes, &computed, cx);
                changed.push((assignment.target.clone(), computed));
                continue;
            }
            let value = self.assigned_value(assignment, cx);
            let ty = cx.layout(self.assigned(&assignment.target).0);
            let slot = self.slot(ty.clone());
            self.insts.push(lir::Inst::Copy {
                dst: slot.clone(),
                src: value,
                layout: ty,
            });
            values.push((assignment.target.clone(), slot));
        }
        for (target, value) in &values {
            self.check_keys(target, value, cx);
        }
        if changed.iter().map(|(_, c)| c.len()).sum::<usize>() > 1 {
            for (target, computed) in &changed {
                let ty = self.assigned(target).0.clone();
                self.keep_values(&ty, computed, cx);
            }
        }
        for (target, value) in values {
            self.store(&target, value, cx);
        }
        for (target, computed) in changed {
            let ty = self.assigned(&target).0.clone();
            let holder = self.holder(&target, cx);
            for change in computed {
                self.changed_at(holder.clone(), &ty, change, cx);
            }
        }
    }

    /// Where the value of `assignment` is: with its tuples in no particular order if it
    /// is to a local variable that nothing reads otherwise, as it then says.
    fn assigned_value(&mut self, assignment: &hir::Assignment, cx: &mut Lowering) -> lir::Place {
        match &assignment.target {
            hir::Target::Local(id) if let Some(&want) = self.local_wants.get(id) => {
                let (place, order) = self.given_local(*id, &assignment.value, want, cx);
                if order != Order::Canonical {
                    self.local_orders.insert(*id, order);
                }
                place
            }
            _ => self.place(&assignment.value, cx),
        }
    }

    /// Note which of `block`'s local variables can be left out of canonical order, and what
    /// all that reads each wants instead: a relation with no key to check, assigned only
    /// where it is declared, that the block, and `end`, its value if it has one, read only
    /// in ways that want any order, or a sorted one (see [`FunctionBuilder::read_want`]).
    /// The last are first, as what one is wanted in says what those it is computed from
    /// are.
    fn note_loose_locals(&mut self, block: &hir::Block, end: Option<&hir::Expr>) {
        for id in block.locals.iter().rev() {
            if !self.is_relation_of_no_key_to_check(*id) || block.times_assigned(*id) != 1 {
                continue;
            }
            let mut reads = ReadsOf::default();
            let mut want = self.block_read_want(*id, block, false, &mut reads);
            if let Some(end) = end {
                let read = self.read_want(*id, end, Around::wanted_in_order(), &mut reads);
                want = want.stronger(read);
            }
            self.note_local(*id, want, reads);
        }
    }

    /// The tuples `assignment` adds to its variable or takes from it, as
    /// what kind of combination, if that is all it does: its value is the
    /// variable's, of the same type as the tuples are, combined with them.
    fn combined_with_itself<'v>(
        &self,
        assignment: &'v hir::Assignment,
    ) -> Option<(&'v hir::Expr, hir::Combination)> {
        let hir::Expr::Combine(
            combination @ (hir::Combination::Union
            | hir::Combination::DisjointUnion
            | hir::Combination::Minus
            | hir::Combination::IncludedMinus),
            variable,
            tuples,
        ) = &assignment.value
        else {
            return None;
        };
        let ty = self.assigned(&assignment.target).0;
        (**variable == assignment.target.read()
            && self.type_of(variable) == *ty
            && self.type_of(tuples) == *ty)
            .then_some((&**tuples, *combination))
    }

    /// What `assignment` keeps of its variable, if that is all it does: its
    /// value is the variable's, of the type it is, restricted by this
    /// condition, which the tuple at hand is what it is true of.
    fn restricted_itself<'v>(&self, assignment: &'v hir::Assignment) -> Option<&'v hir::Expr> {
        let hir::Expr::Restrict(variable, condition) = &assignment.value else {
            return None;
        };
        (**variable == assignment.target.read()
            && self.type_of(variable) == *self.assigned(&assignment.target).0)
            .then_some(&**condition)
    }

    /// Add `tuples` to the relation variable `target`, or take them from it,
    /// in its own buffer if nothing else holds it (see
    /// [`lir::RuntimeFn::Insert`] and [`lir::RuntimeFn::Remove`]). After adding
    /// them, its keys are checked of what was added alone, as they held for
    /// the others, and a failure leaves it as it is, as that stops the
    /// program, or a transaction, which restores it. Taking them can't break a
    /// key.
    fn changed_by_tuples(
        &mut self,
        target: &hir::Target,
        tuples: &hir::Expr,
        combination: hir::Combination,
        cx: &mut Lowering,
    ) {
        let tuples = self.place(tuples, cx);
        let ty = self.assigned(target).0.clone();
        let holder = self.holder(target, cx);
        let descriptor = lir::Place::whole(lir::Base::Data(cx.descriptor(&ty)));
        let (function, code) = match combination {
            hir::Combination::Union => (lir::RuntimeFn::Insert, lir::Combination::Union),
            hir::Combination::DisjointUnion => {
                (lir::RuntimeFn::Insert, lir::Combination::DisjointUnion)
            }
            hir::Combination::Minus => (lir::RuntimeFn::Remove, lir::Combination::Minus),
            hir::Combination::IncludedMinus => {
                (lir::RuntimeFn::Remove, lir::Combination::IncludedMinus)
            }
            hir::Combination::ExclusiveUnion | hir::Combination::DisjointChanges => {
                unreachable!("a variable changes in place by adding or taking tuples")
            }
        };
        let code = usize_constant(code.code(), cx);
        self.insts.push(lir::Inst::CallRuntime {
            function,
            args: vec![holder.clone(), tuples.clone(), descriptor, code],
        });
        self.scoped = true;
        if function == lir::RuntimeFn::Insert {
            self.check_keys_of(target, &holder, Some(&tuples), cx);
        }
    }

    /// Take from the relation variable `target` the tuples `kept` is false
    /// of, in its own buffer if nothing else holds it. It is found what goes
    /// before anything does, so a condition that fails, or reads the variable,
    /// finds it as it was. What goes is what the opposite of `kept` is true of,
    /// so a run of it can be found rather than gone through (see
    /// [`FunctionBuilder::equal_runs`]), and if that is all it says, it goes
    /// with no condition computed. Taking tuples can't break a key.
    fn delete_unless(&mut self, target: &hir::Target, kept: &hir::Expr, cx: &mut Lowering) {
        let ty = self.assigned(target).0.clone();
        let hir::Type::Relation(heading) = &ty else {
            unreachable!("sema restricts only relations");
        };
        let tuple = cx.layout(&hir::Type::Tuple(heading.clone()));
        let holder = self.holder(target, cx);
        let gone = kept.clone().negated();
        let (run, rest) =
            self.equal_runs(holder.clone(), &ty, Order::Canonical, heading, &gone, cx);
        let descriptor = lir::Place::whole(lir::Base::Data(cx.descriptor(&ty)));
        self.scoped = true;
        if rest == hir::Expr::Constant(hir::Value::Boolean(true)) {
            self.insts.push(lir::Inst::CallRuntime {
                function: lir::RuntimeFn::RemoveRun,
                args: vec![holder, run, descriptor],
            });
            return;
        }
        let mut count = run.clone();
        count.path.push(1);
        let marks_ty = hir::Type::Array(Box::new(hir::Type::Scalar(hir::Scalar::Boolean)));
        let marks = self.allocated(&marks_ty, count, cx);
        let marked = u8_constant(1, cx);
        self.each_at_hand(&run, tuple, cx, |this, _, index, cx| {
            // The condition, computed in a scope of its own for each tuple,
            // which nothing it made outlives.
            let goes = this.slot(lir::Layout::U8);
            this.nested_scope(cx, |this, cx| {
                let computed = this.place(&rest, cx);
                this.insts.push(lir::Inst::Copy {
                    dst: goes.clone(),
                    src: computed,
                    layout: lir::Layout::U8,
                });
                Vec::new()
            });
            let (mark, next) = (this.new_block(), this.new_block());
            this.end_block(
                lir::Terminator::Branch {
                    condition: goes,
                    then: mark,
                    otherwise: next,
                },
                mark,
            );
            let to = this.element(&marks, index, lir::Layout::U8);
            this.insts.push(lir::Inst::Copy {
                dst: to,
                src: marked,
                layout: lir::Layout::U8,
            });
            this.end_block(lir::Terminator::Jump(next), next);
        });
        self.insts.push(lir::Inst::CallRuntime {
            function: lir::RuntimeFn::RemoveMarked,
            args: vec![holder, run, marks, descriptor],
        });
    }

    /// The type and keys of the variable `target`, of the program or local,
    /// or of a `var` or `out` parameter, which has none: its variable's are checked
    /// where it is passed.
    fn assigned(&self, target: &hir::Target) -> (&hir::Type, &[hir::Key]) {
        match target {
            hir::Target::Variable(id) => {
                let variable = self.program.variable(*id);
                (&variable.ty, &variable.keys)
            }
            hir::Target::Local(id) => {
                let local = &self.locals[id.0];
                (&local.ty, &local.keys)
            }
            hir::Target::Parameter(name) => (&self.params[name].ty, &[]),
        }
    }

    /// Stop the program if the relation at `value`, about to be assigned to
    /// the variable `target`, has two tuples with the same values for one
    /// of its keys, writing the variable, the key and the tuples. A key of
    /// the whole heading always holds.
    fn check_keys(&mut self, target: &hir::Target, value: &lir::Place, cx: &mut Lowering) {
        self.check_keys_of(target, value, None, cx);
    }

    /// As [`FunctionBuilder::check_keys`], of what the tuples `added` alone
    /// could have broken, if it says.
    fn check_keys_of(
        &mut self,
        target: &hir::Target,
        value: &lir::Place,
        added: Option<&lir::Place>,
        cx: &mut Lowering,
    ) {
        let (ty, keys) = self.assigned(target);
        let (ty, keys) = (ty.clone(), keys.to_vec());
        let variable = self.target_name(target).to_string();
        self.named_keys_of(&variable, &keys, &ty, value, added, cx);
    }

    /// Stop the program if the relation at `value`, of the type `ty`, what is
    /// called `variable` has as it is assigned, breaks one of `keys`: as for
    /// [`FunctionBuilder::check_keys_of`].
    fn named_keys_of(
        &mut self,
        variable: &str,
        keys: &[hir::Key],
        ty: &hir::Type,
        value: &lir::Place,
        added: Option<&lir::Place>,
        cx: &mut Lowering,
    ) {
        if !matches!(ty, hir::Type::Relation(_)) {
            return;
        }
        let ty = ty.clone();
        for key in keys {
            self.key_checked(value, added, &ty, key, cx, |this, failed, cx| {
                Broken::Fail(Box::new(lir::Failure::Key(lir::KeyFailure::Variable {
                    value: failed.value,
                    descriptor: failed.descriptor,
                    key: failed.key,
                    pair: failed.pair,
                    variable: this.text(variable, cx),
                })))
            });
        }
    }

    /// The name of the variable `target`, of the program or local, or of a
    /// `var` or `out` parameter.
    fn target_name<'t>(&'t self, target: &'t hir::Target) -> &'t str {
        match target {
            hir::Target::Variable(id) => &self.program.variable(*id).name,
            hir::Target::Local(id) => &self.locals[id.0].name,
            hir::Target::Parameter(name) => name,
        }
    }

    /// Where the variable `target`'s value is, of the program or local, or
    /// a `var` or `out` parameter's variable.
    fn holder(&self, target: &hir::Target, cx: &Lowering) -> lir::Place {
        match target {
            hir::Target::Variable(id) => lir::Place {
                base: lir::Base::Data(self.variable_data[id.0]),
                path: vec![0],
            },
            hir::Target::Local(id) => self.local_places[id.0]
                .as_ref()
                .expect("an update operator's local variables are placed first")
                .value
                .clone(),
            hir::Target::Parameter(name) => self.pointee(name, cx),
        }
    }

    /// The variable of the `var` or `out` parameter `name`, through its address.
    fn pointee(&self, name: &str, cx: &Lowering) -> lir::Place {
        lir::Place::whole(lir::Base::Indirect {
            slot: self.pointers[name],
            pointee: cx.layout(&self.params[name].ty),
        })
    }

    /// Assign the value at `value` to the variable `target`, of the
    /// program or local: copied, and, if it points to texts, relations or
    /// arrays, with a reference taken to each, which the variable holds.
    fn store(&mut self, target: &hir::Target, value: lir::Place, cx: &mut Lowering) {
        let ty = self.assigned(target).0.clone();
        let dst = self.holder(target, cx);
        self.assign_place(dst, value, &ty, cx);
    }

    /// Assign the value at `value`, of type `ty`, to the variable at `dst`,
    /// which takes references to what it points to.
    fn assign_place(
        &mut self,
        dst: lir::Place,
        value: lir::Place,
        ty: &hir::Type,
        cx: &mut Lowering,
    ) {
        if cx.points(ty) {
            let descriptor = lir::Place::whole(lir::Base::Data(cx.descriptor(ty)));
            self.insts.push(lir::Inst::CallRuntime {
                function: lir::RuntimeFn::Assign,
                args: vec![dst, value, descriptor],
            });
            self.scoped = true;
        } else {
            self.insts.push(lir::Inst::Copy {
                dst,
                src: value,
                layout: cx.layout(ty),
            });
        }
    }

    /// Where the constant `id` is, computed first if this read is the first to
    /// need it. A failure in that goes where any call's does.
    fn constant_read(&mut self, id: hir::ConstantId, cx: &mut Lowering) -> lir::Place {
        let constant = computed_constant(self.program, self.variable_data, id, cx);
        let (compute, computed) = (self.new_block(), self.new_block());
        self.end_block(
            lir::Terminator::Branch {
                condition: constant.computed(),
                then: computed,
                otherwise: compute,
            },
            compute,
        );
        self.insts.push(lir::Inst::Call {
            function: constant.function,
            args: None,
            result: None,
            can_fail: true,
        });
        self.after_call(cx);
        self.end_block(lir::Terminator::Jump(computed), computed);
        in_data(constant.data)
    }

    /// The value of `expr`, `read`, as the program chooses as it runs (see
    /// [`hir::Expr::AdaptedRead`]): the shared form, once the constant is
    /// computed, and otherwise the narrowed one, which costs less than the whole
    /// unless the read runs again, as one in a loop does. So the first time it
    /// runs, it computes its narrowed form, and after that its shared one, which
    /// computes the constant whole if nothing has. A failure in that is one the
    /// narrowed form has too, as narrowing never leaves out what can fail. Its
    /// tuples are in canonical order, unless its reader `want`s them in any, as
    /// either form gives them, all different, and how they come.
    fn adapted_read(
        &mut self,
        expr: &hir::Expr,
        read: &hir::AdaptedRead,
        want: Want,
        cx: &mut Lowering,
    ) -> (lir::Place, Order) {
        let layout = cx.layout(&self.type_of(expr));
        let dst = self.slot(layout.clone());
        let [shared, uncomputed, first, narrowed, after] =
            std::array::from_fn(|_| self.new_block());
        let computed = match read.of {
            hir::Adapted::Constant(id) => {
                computed_constant(self.program, self.variable_data, id, cx).computed()
            }
            hir::Adapted::Local(id) => self.deferred[&id].computed.clone(),
        };
        self.end_block(
            lir::Terminator::Branch {
                condition: computed,
                then: shared,
                otherwise: uncomputed,
            },
            uncomputed,
        );
        let (ran, mark) = self.read_ran(read.of, cx);
        self.end_block(
            lir::Terminator::Branch {
                condition: ran,
                then: shared,
                otherwise: first,
            },
            first,
        );
        self.mark_ran(mark, cx);
        self.end_block(lir::Terminator::Jump(narrowed), shared);
        let ty = self.type_of(expr);
        let loose = want == Want::Any;
        for (form, next) in [(&read.shared, narrowed), (&read.narrowed, after)] {
            let src = if loose {
                let (src, order) = self.loose_place(form, cx);
                if order == Order::Repeated {
                    self.canonical(src, order, &ty, cx)
                } else {
                    src
                }
            } else {
                self.place(form, cx)
            };
            self.insts.push(lir::Inst::Copy {
                dst: dst.clone(),
                src,
                layout: layout.clone(),
            });
            self.end_block(lir::Terminator::Jump(after), next);
        }
        let order = if loose {
            Order::Loose
        } else {
            Order::Canonical
        };
        (dst, order)
    }

    /// Whether a read that narrows `of` ([`FunctionBuilder::adapted_read`]) has run,
    /// and what says so, for [`FunctionBuilder::mark_ran`]: of a constant of the
    /// program's, a static of its own, for the whole run, and of a local one, its bit
    /// of what the constant's declaration resets ([`Deferred::ran`]).
    fn read_ran(&mut self, of: hir::Adapted, cx: &mut Lowering) -> (lir::Place, Ran) {
        match of {
            hir::Adapted::Constant(_) => {
                let name = format!("__read_{}", cx.data.len());
                let ran = in_data(cx.push(lir::Data {
                    kind: lir::DataKind::Variable { name },
                    writable: true,
                    fields: vec![lir::Field::U8(0)],
                }));
                (ran.clone(), Ran::Static(ran))
            }
            hir::Adapted::Local(id) => {
                let deferred = self
                    .deferred
                    .get_mut(&id)
                    .expect("a local constant is read after its declaration");
                let bit = 1_i64 << deferred.reads.min(62);
                deferred.reads += 1;
                let reads = deferred.ran.clone();
                let bit = self.place(&hir::Expr::Constant(hir::Value::Integer(bit)), cx);
                let (own, ran) = (self.slot(lir::Layout::I64), self.slot(lir::Layout::U8));
                self.insts.push(lir::Inst::Binary {
                    dst: own.clone(),
                    op: lir::BinaryOp::And(lir::Bits::Integer),
                    lhs: reads.clone(),
                    rhs: bit.clone(),
                });
                let none = self.place(&hir::Expr::Constant(hir::Value::Integer(0)), cx);
                self.insts.push(lir::Inst::Binary {
                    dst: ran.clone(),
                    op: lir::BinaryOp::Compare(
                        lir::Comparison::NotEqual,
                        lir::Operand::Numeric(lir::Numeric::Integer),
                    ),
                    lhs: own,
                    rhs: none,
                });
                (ran, Ran::Bit { reads, bit })
            }
        }
    }

    /// Record that the read `ran` says of has run.
    fn mark_ran(&mut self, ran: Ran, cx: &mut Lowering) {
        match ran {
            Ran::Static(ran) => {
                let one = u8_constant(1, cx);
                self.insts.push(lir::Inst::Copy {
                    dst: ran,
                    src: one,
                    layout: lir::Layout::U8,
                });
            }
            Ran::Bit { reads, bit } => self.insts.push(lir::Inst::Binary {
                dst: reads.clone(),
                op: lir::BinaryOp::Or(lir::Bits::Integer),
                lhs: reads,
                rhs: bit,
            }),
        }
    }

    /// Where the parameter `name` is, among the arguments the function is given,
    /// of one that isn't a pointer to what it is.
    fn parameter_place(&self, name: &str) -> lir::Place {
        let index = self
            .params
            .keys()
            .position(|p| p == name)
            .expect("sema resolved the parameter");
        let mut parameter = self
            .arguments
            .clone()
            .unwrap_or_else(|| lir::Place::whole(lir::Base::Params));
        parameter.path.push(index);
        parameter
    }

    /// The declaration of the local constant `id`, of `value`, computed when a read first
    /// needs it ([`hir::Local::deferred`]): nothing has computed it since, nor run a read of
    /// it that narrows it. It has no value until a read computes it, and holds nothing, as
    /// what it held as its declaration last ran was given up after the block's last
    /// statement to mention it, or as the block was left, and it is null, as it is from the
    /// function's entry on, so that what assigns it gives up nothing.
    fn defer(&mut self, id: hir::LocalId, value: &hir::Expr, cx: &mut Lowering) {
        let (slot, _) = self.local_place(id);
        self.insts.push(lir::Inst::NoValueYet(slot));
        let (computed, ran) = (self.slot(lir::Layout::U8), self.slot(lir::Layout::I64));
        let none = u8_constant(0, cx);
        self.insts.push(lir::Inst::Copy {
            dst: computed.clone(),
            src: none,
            layout: lir::Layout::U8,
        });
        let none = self.place(&hir::Expr::Constant(hir::Value::Integer(0)), cx);
        self.insts.push(lir::Inst::Copy {
            dst: ran.clone(),
            src: none,
            layout: lir::Layout::I64,
        });
        self.deferred.insert(
            id,
            Deferred {
                value: value.clone(),
                computed,
                ran,
                reads: 0,
            },
        );
    }

    /// Where the local variable `id` is, and how its tuples are ordered, as
    /// [`FunctionBuilder::local_place`] says, once it has its value: one computed when a
    /// read first needs it is computed here if nothing has since its declaration ran,
    /// assigned as its declaration would.
    fn read_local(&mut self, id: hir::LocalId, cx: &mut Lowering) -> (lir::Place, Order) {
        if let Some(deferred) = self.deferred.get(&id) {
            let (computed, value) = (deferred.computed.clone(), deferred.value.clone());
            let (compute, after) = (self.new_block(), self.new_block());
            self.end_block(
                lir::Terminator::Branch {
                    condition: computed.clone(),
                    then: after,
                    otherwise: compute,
                },
                compute,
            );
            self.assign(
                &[hir::Assignment {
                    target: hir::Target::Local(id),
                    value,
                }],
                cx,
            );
            let one = u8_constant(1, cx);
            self.insts.push(lir::Inst::Copy {
                dst: computed,
                src: one,
                layout: lir::Layout::U8,
            });
            self.end_block(lir::Terminator::Jump(after), after);
        }
        self.local_place(id)
    }

    /// Where the local variable `id` is, and how its tuples are ordered, if it is a
    /// relation.
    fn local_place(&self, id: hir::LocalId) -> (lir::Place, Order) {
        let place = self.local_places[id.0]
            .as_ref()
            .expect("a local variable is placed before it is used")
            .value
            .clone();
        let order = self
            .local_orders
            .get(&id)
            .copied()
            .unwrap_or(Order::Canonical);
        (place, order)
    }

    /// The relation at `place`, of type `ty`, as `order` says, in canonical order: where it
    /// is if it already is, and otherwise in a new slot, as what is there may be read again
    /// as it is.
    fn canonical(
        &mut self,
        place: lir::Place,
        order: Order,
        ty: &hir::Type,
        cx: &mut Lowering,
    ) -> lir::Place {
        self.in_order(place, order, Order::Canonical, ty, cx)
    }

    /// The relation at `place`, of type `ty`, as `order` says, in `wanted`, a sorted order:
    /// where it is if it already is, and otherwise in a new slot, as what is there may be
    /// read again as it is. A loose one is put in order, and one sorted otherwise is
    /// sorted again, which is a choice of order rather than something left unsorted.
    fn in_order(
        &mut self,
        place: lir::Place,
        order: Order,
        wanted: Order,
        ty: &hir::Type,
        cx: &mut Lowering,
    ) -> lir::Place {
        if order == wanted {
            return place;
        }
        let layout = cx.layout(ty);
        let dst = self.slot(layout.clone());
        self.insts.push(lir::Inst::Copy {
            dst: dst.clone(),
            src: place,
            layout,
        });
        let function = match order {
            Order::Loose | Order::Repeated => lir::RuntimeFn::PutInOrder,
            Order::Canonical | Order::SortedBy(_) => lir::RuntimeFn::Reorder,
        };
        let descriptor = lir::Place::whole(lir::Base::Data(cx.described(ty, wanted)));
        self.insts.push(lir::Inst::CallRuntime {
            function,
            args: vec![dst.clone(), descriptor],
        });
        self.scoped = true;
        dst
    }

    /// Where the constant relation `value`, of type `ty`, is, with its tuples in `order`, a
    /// sorted one, as what reads it wants: in the program's data, sorted so as it is
    /// compiled, at no cost when it runs, and, if not by name, in a slot that says so, for
    /// [`lir::Function::loose_read`], which knows only slots.
    fn constant_in(
        &mut self,
        value: &hir::RelationValue,
        ty: &hir::Type,
        order: Order,
        cx: &mut Lowering,
    ) -> lir::Place {
        let field = cx.relation_field(value, order);
        let data = in_data(cx.constant(vec![field]));
        if order == Order::Canonical {
            return data;
        }
        let layout = cx.layout(ty);
        let slot = self.slot(layout.clone());
        self.insts.push(lir::Inst::Copy {
            dst: slot.clone(),
            src: data,
            layout,
        });
        let descriptor = lir::Place::whole(lir::Base::Data(cx.described(ty, order)));
        self.insts.push(lir::Inst::SortedAs {
            relation: slot.clone(),
            descriptor,
        });
        slot
    }

    /// The order the constant relation `value` is given in to what would have it sorted by
    /// `leading` first: that, as it costs nothing, if there are any, and otherwise canonical
    /// order.
    fn constant_order(value: &hir::RelationValue, leading: &[String], cx: &mut Lowering) -> Order {
        if leading.is_empty() {
            Order::Canonical
        } else {
            cx.leading_order(leading, &value.heading)
        }
    }

    /// The attributes of a relation of `heading` that the conjuncts of the restriction
    /// `condition` each equate to a value, if that is all they say, each of a different
    /// one: those equated to values that can't fail first, then those equated to values
    /// that can, each by name, in which order [`FunctionBuilder::equal_runs`] finds the run
    /// of each, leaving nothing of the condition to compute for each tuple.
    fn only_equated_attributes(
        &self,
        heading: &hir::Heading,
        condition: &hir::Expr,
    ) -> Option<Vec<String>> {
        let (mut safe, mut failing) = (BTreeSet::new(), BTreeSet::new());
        for conjunct in condition.conjuncts() {
            let (name, value) = heading
                .attributes()
                .find_map(|(name, ty)| Some((name, self.equated(conjunct, name, ty)?)))?;
            let fresh = !safe.contains(name) && !failing.contains(name);
            let into = if value.can_fail() {
                &mut failing
            } else {
                &mut safe
            };
            if !fresh || !into.insert(name.to_string()) {
                return None;
            }
        }
        Some(safe.into_iter().chain(failing).collect())
    }

    /// What a join or a semijoin is given of its operand, what `view` reads, joined with what
    /// `other` reads: a constant written sorted by the attributes they have in common,
    /// which the runtime merges them on, as it costs nothing, and anything else as `want`
    /// asks.
    fn joined_operand(
        &mut self,
        view: &View,
        other: &View,
        want: Want,
        cx: &mut Lowering,
    ) -> (lir::Place, Order) {
        let Some(value) = constant_relation(view.inner) else {
            return self.given(view.inner, want, cx);
        };
        let order = Self::constant_order(value, &common_owns(view, other), cx);
        (self.constant_in(value, &view.ty, order, cx), order)
    }

    /// The order [`FunctionBuilder::joined_operand`] would give what `view` reads in, without
    /// computing it, as a sorted order will do.
    fn joined_operand_order(&self, view: &View, other: &View, cx: &mut Lowering) -> Order {
        match constant_relation(view.inner) {
            Some(value) => Self::constant_order(value, &common_owns(view, other), cx),
            None => self.sorted_order(view.inner, cx),
        }
    }

    /// The attributes of a relation of `heading` that the restriction `condition` says are
    /// each equal to a value that can't fail, which its runs are found by, by name.
    fn equated_attributes(&self, heading: &hir::Heading, condition: &hir::Expr) -> Vec<String> {
        let conjuncts = condition.conjuncts();
        heading
            .attributes()
            .filter(|(name, ty)| {
                conjuncts.iter().any(|conjunct| {
                    self.equated(conjunct, name, ty)
                        .is_some_and(|value| !value.can_fail())
                })
            })
            .map(|(name, _)| name.to_string())
            .collect()
    }

    /// The order [`FunctionBuilder::sorted_place`] would give the relation `expr` in,
    /// without computing it.
    fn sorted_order(&self, expr: &hir::Expr, cx: &mut Lowering) -> Order {
        match expr {
            hir::Expr::At(_, inner) => self.sorted_order(inner, cx),
            hir::Expr::Local(id) => match self.local_orders.get(id) {
                Some(&order @ Order::SortedBy(_)) => order,
                _ => Order::Canonical,
            },
            hir::Expr::Rename(..) => {
                let view = self.view_of(expr);
                let order = self.sorted_order(view.inner, cx);
                cx.renamed_order(order, &view)
            }
            hir::Expr::Combine(_, lhs, rhs) => self.shared_order(lhs, rhs, cx),
            hir::Expr::Restrict(operand, condition) => {
                match (constant_relation(operand), self.type_of(operand)) {
                    (Some(value), hir::Type::Relation(heading)) => {
                        let only = condition
                            .can_fail()
                            .then(|| self.only_equated_attributes(&heading, condition))
                            .flatten();
                        let leading =
                            only.unwrap_or_else(|| self.equated_attributes(&heading, condition));
                        Self::constant_order(value, &leading, cx)
                    }
                    _ => self.sorted_order(operand, cx),
                }
            }
            hir::Expr::Extend(operand, _, attributes) => {
                let hir::Type::Relation(heading) = self.type_of(operand) else {
                    return Order::Canonical;
                };
                if attributes.keys().any(|name| heading.get(name).is_some()) {
                    return Order::Canonical;
                }
                let order = self.sorted_order(operand, cx);
                cx.extended_order(order, &heading, attributes)
            }
            hir::Expr::And(lhs, rhs)
                if matches!(self.type_of(lhs), hir::Type::Relation(_))
                    && self.lookup_in(lhs, rhs).is_none()
                    && self.lookup_in(rhs, lhs).is_none()
                    && several_joined(lhs, rhs).is_none()
                    && !joins_a_closure(lhs, rhs) =>
            {
                let (left, right) = (self.view_as_it_is(lhs), self.view_as_it_is(rhs));
                let lhs_order = self.joined_operand_order(&left, &right, cx);
                let rhs_order = self.joined_operand_order(&right, &left, cx);
                cx.joined_order(&left, &right, lhs_order, rhs_order)
            }
            hir::Expr::Semijoin(lhs, rhs) | hir::Expr::Antijoin(lhs, rhs) => {
                let (left, right) = (self.view_as_it_is(lhs), self.view_as_it_is(rhs));
                let order = self.joined_operand_order(&left, &right, cx);
                cx.read_order(order, &left)
            }
            _ => Order::Canonical,
        }
    }

    /// The order the relations `lhs` and `rhs`, of the same attributes, are merged in,
    /// where any will do: the one [`FunctionBuilder::sorted_place`] would give both in, if
    /// it is the same, and otherwise canonical order.
    fn shared_order(&self, lhs: &hir::Expr, rhs: &hir::Expr, cx: &mut Lowering) -> Order {
        // A constant is written in whatever order the other comes in.
        match (constant_relation(lhs), constant_relation(rhs)) {
            (Some(_), None) => return self.sorted_order(rhs, cx),
            (None, Some(_)) => return self.sorted_order(lhs, cx),
            _ => {}
        }
        let order = self.sorted_order(lhs, cx);
        if order == self.sorted_order(rhs, cx) {
            order
        } else {
            Order::Canonical
        }
    }

    /// Where `value`, the value of the local variable or binding `id`, is, computed as `want`
    /// asks, and its order: sorted by what its readers would have it sorted by first, if it
    /// is sorted anyway, as a projection that may make two tuples equal is, and if it is
    /// wanted sorted, as one looked up again is ([`FunctionBuilder::leading_want`]).
    fn given_local(
        &mut self,
        id: hir::LocalId,
        value: &hir::Expr,
        want: Want,
        cx: &mut Lowering,
    ) -> (lir::Place, Order) {
        let Some(leading) = self.local_leading.get(&id).cloned() else {
            return self.given(value, want, cx);
        };
        let (place, order) = self.preferring(value, want, &leading, cx);
        let hir::Type::Relation(heading) = self.locals[id.0].ty.clone() else {
            return (place, order);
        };
        if want != Want::Sorted || cx.leads_with(order, &heading, &leading) {
            return (place, order);
        }
        let wanted = cx.leading_order(&leading, &heading);
        let ty = hir::Type::Relation(heading);
        (self.in_order(place, order, wanted, &ty, cx), wanted)
    }

    /// Where the relation `expr` is, computed as `want` asks, and its order: a projection
    /// that is sorted anyway sorted by `leading` first.
    fn preferring(
        &mut self,
        expr: &hir::Expr,
        want: Want,
        leading: &[String],
        cx: &mut Lowering,
    ) -> (lir::Place, Order) {
        match expr {
            hir::Expr::At(site, inner) => {
                let outer = self.site;
                self.at(Some(*site), cx);
                let given = self.preferring(inner, want, leading, cx);
                if outer.is_some() {
                    self.at(outer, cx);
                }
                given
            }
            hir::Expr::Remove(operand, names) => {
                self.removed(operand, names, want, Some(leading), cx)
            }
            hir::Expr::Constant(hir::Value::Relation(value)) => {
                let order = Self::constant_order(value, leading, cx);
                let ty = self.type_of(expr);
                (self.constant_in(value, &ty, order, cx), order)
            }
            _ => self.given(expr, want, cx),
        }
    }

    /// Where the relation `expr` is, computed in `order`, a sorted order: as
    /// [`FunctionBuilder::place`] computes it, in canonical order, and otherwise as
    /// [`FunctionBuilder::sorted_place`] does, which gives it in an order other than by name
    /// only as [`FunctionBuilder::sorted_order`] says, so it is put in `order` only if that
    /// isn't it.
    fn exactly(&mut self, expr: &hir::Expr, order: Order, cx: &mut Lowering) -> lir::Place {
        if order == Order::Canonical {
            return self.place(expr, cx);
        }
        let ty = self.type_of(expr);
        if let Some(value) = constant_relation(expr) {
            return self.constant_in(value, &ty, order, cx);
        }
        let (place, given) = self.sorted_place(expr, cx);
        self.in_order(place, given, order, &ty, cx)
    }

    /// Where the relation `expr` is, computed as [`FunctionBuilder::place`] does, but with
    /// its tuples sorted in whatever order is less work, which may not be by name, and
    /// that order. What it is given to has to be told that order, by its descriptor
    /// ([`Lowering::described`]), as [`lir::Function::loose_read`] checks.
    fn sorted_place(&mut self, expr: &hir::Expr, cx: &mut Lowering) -> (lir::Place, Order) {
        match expr {
            hir::Expr::At(site, inner) => {
                let outer = self.site;
                self.at(Some(*site), cx);
                let given = self.sorted_place(inner, cx);
                if outer.is_some() {
                    self.at(outer, cx);
                }
                given
            }
            hir::Expr::Local(id) => {
                let (place, order) = self.read_local(*id, cx);
                if matches!(order, Order::Loose | Order::Repeated) {
                    let ty = self.locals[id.0].ty.clone();
                    (self.canonical(place, order, &ty, cx), Order::Canonical)
                } else {
                    (place, order)
                }
            }
            hir::Expr::Rename(..) => self.renamed_in_order(expr, cx),
            hir::Expr::Combine(combination, lhs, rhs) => {
                self.combine(*combination, lhs, rhs, Want::Sorted, cx)
            }
            hir::Expr::Restrict(operand, condition) => {
                self.restrict(operand, condition, Want::Sorted, cx)
            }
            hir::Expr::Extend(operand, _, attributes) => {
                self.extend(expr, operand, attributes, Want::Sorted, cx)
            }
            hir::Expr::And(lhs, rhs) => self.joined(lhs, rhs, Want::Sorted, cx),
            hir::Expr::Semijoin(lhs, rhs) => self.matching(lhs, rhs, false, Want::Sorted, cx),
            hir::Expr::Antijoin(lhs, rhs) => self.matching(lhs, rhs, true, Want::Sorted, cx),
            _ => (self.place(expr, cx), Order::Canonical),
        }
    }

    /// `expr` as `want` asks: in canonical order, computed all the way down as
    /// [`FunctionBuilder::place`] does, or as it comes, if nothing above it depends on that.
    /// Tuples in order cost nothing to keep so, through a join as through a restriction, where
    /// leaving them out of order for a reader that wants them in order costs a sort of what
    /// comes out, which is often much larger.
    fn given(&mut self, expr: &hir::Expr, want: Want, cx: &mut Lowering) -> (lir::Place, Order) {
        match want {
            Want::Canonical => (self.place(expr, cx), Order::Canonical),
            Want::Sorted => self.sorted_place(expr, cx),
            Want::Any | Want::Repeated => self.loose_place(expr, cx),
        }
    }

    /// How a join or a semijoin reads its operand `expr`: through any renames around it, if
    /// nothing above it depends on the order of what comes out, and otherwise as it is, so that
    /// the operand is in the order that makes that cost nothing.
    fn operand_view<'e>(&self, expr: &'e hir::Expr, want: Want) -> View<'e> {
        if matches!(want, Want::Canonical | Want::Sorted) {
            self.view_as_it_is(expr)
        } else {
            self.view_of(expr)
        }
    }

    /// Where the relation `expr` is, computed as [`FunctionBuilder::place`] does, but with
    /// its tuples in no particular order, if that is less work, and how they are ordered.
    /// They are always all different.
    ///
    /// Only what doesn't depend on the order of a relation may ask for one this way, and
    /// it has to say so to [`lir::Function::loose_read`], as [`FunctionBuilder::place`]
    /// always gives the canonical order, so that what doesn't ask needn't know of this.
    /// What can fail for a tuple fails for the same one, however they come (see
    /// [`FunctionBuilder::each_at_hand_in`]).
    fn loose_place(&mut self, expr: &hir::Expr, cx: &mut Lowering) -> (lir::Place, Order) {
        match expr {
            hir::Expr::Local(id) => self.read_local(*id, cx),
            hir::Expr::At(site, inner) => {
                let outer = self.site;
                self.at(Some(*site), cx);
                let given = self.loose_place(inner, cx);
                if outer.is_some() {
                    self.at(outer, cx);
                }
                given
            }
            hir::Expr::Let(..) => self.let_chain(expr, Want::Any, cx),
            hir::Expr::AdaptedRead(read) => self.adapted_read(expr, read, Want::Any, cx),
            hir::Expr::Rename(..) => self.renamed(expr, Want::Any, cx),
            hir::Expr::Remove(operand, names) => self.removed(operand, names, Want::Any, None, cx),
            hir::Expr::And(lhs, rhs) => self.joined(lhs, rhs, Want::Any, cx),
            hir::Expr::Semijoin(lhs, rhs) => self.matching(lhs, rhs, false, Want::Any, cx),
            hir::Expr::Antijoin(lhs, rhs) => self.matching(lhs, rhs, true, Want::Any, cx),
            hir::Expr::Restrict(operand, condition) => {
                self.restrict(operand, condition, Want::Any, cx)
            }
            hir::Expr::Extend(operand, _, attributes) => {
                self.extend(expr, operand, attributes, Want::Any, cx)
            }
            hir::Expr::Combine(combination, lhs, rhs) => {
                self.combine(*combination, lhs, rhs, Want::Any, cx)
            }
            _ => (self.place(expr, cx), Order::Canonical),
        }
    }

    /// The operands of `expr` that it reads whatever order their tuples are in: which
    /// [`FunctionBuilder::loose_place`] may give them to it. A lookup in a loop has its
    /// tuples in canonical order, and its result is theirs; and a closure is found from the
    /// values of what it is matched with, in canonical order. What can fail for a tuple
    /// fails for the same one, however they come (see [`FunctionBuilder::each_at_hand_in`]).
    fn reads_loosely<'e>(&self, expr: &'e hir::Expr) -> Vec<&'e hir::Expr> {
        let is_key = |operand: &hir::Expr| matches!(operand, hir::Expr::Relation(_, tuples) if tuples.len() == 1);
        let is_closure = |operand: &hir::Expr| arcs_of_closure(operand).is_some();
        match expr {
            hir::Expr::And(lhs, rhs)
                if !is_key(lhs) && !is_key(rhs) && !is_closure(lhs) && !is_closure(rhs) =>
            {
                vec![lhs, rhs]
            }
            hir::Expr::Semijoin(lhs, rhs) if !is_closure(lhs) => vec![lhs, rhs],
            hir::Expr::Antijoin(lhs, rhs) => vec![lhs, rhs],
            hir::Expr::Remove(operand, _)
            | hir::Expr::Rename(operand, _)
            | hir::Expr::Only(operand)
            | hir::Expr::Restrict(operand, _)
            | hir::Expr::Extend(operand, ..) => vec![operand],
            hir::Expr::Call(call) => self
                .counted(call)
                .or(self.summed(call))
                .into_iter()
                .collect(),
            _ => Vec::new(),
        }
    }

    /// The relation that `call` only counts, in the end: `count`'s, or that of an operator
    /// that only asks if it has no tuples, as `is_empty` does.
    fn counted<'e>(&self, call: &'e hir::Call) -> Option<&'e hir::Expr> {
        use hir::ReadOnlyBuiltin::Count;
        let is_count = |call: &hir::Call| {
            matches!(
                self.program.operator(call.operator).kind,
                hir::OperatorKind::Builtin(hir::Builtin::ReadOnly(Count))
            )
        };
        if is_count(call) {
            return call.args.values().next();
        }
        self.asks_if_empty(call)
    }

    /// The relation `call` asks if it has no tuples, if that is all its operator does.
    fn asks_if_empty<'e>(&self, call: &'e hir::Call) -> Option<&'e hir::Expr> {
        use hir::ReadOnlyBuiltin::Count;
        let callee = self.program.operator(call.operator);
        let hir::OperatorKind::ReadOnly { value, .. } = &callee.kind else {
            return None;
        };
        if !callee.versions.is_empty() || !callee.constraints.is_empty() {
            return None;
        }
        let hir::Expr::Binary(hir::BinaryOp::Compare(hir::Comparison::Equal, _), counted, zero) =
            value
        else {
            return None;
        };
        let (hir::Expr::Call(counted), hir::Expr::Constant(hir::Value::Integer(0))) =
            (&**counted, &**zero)
        else {
            return None;
        };
        let is_count = matches!(
            self.program.operator(counted.operator).kind,
            hir::OperatorKind::Builtin(hir::Builtin::ReadOnly(Count))
        );
        let Some(hir::Expr::Parameter(name)) = counted.args.values().next() else {
            return None;
        };
        is_count.then(|| call.args.get(name)).flatten()
    }

    /// The relation `call` reads only the numbers of, by a built-in that gives the same
    /// whatever order they are in: `sum` and `product`, which are exact, and `min` and
    /// `max` where no two different values can tie.
    fn summed<'e>(&self, call: &'e hir::Call) -> Option<&'e hir::Expr> {
        use hir::ReadOnlyBuiltin::{Max, Min, Product, Sum};
        let hir::OperatorKind::Builtin(hir::Builtin::ReadOnly(builtin)) =
            self.program.operator(call.operator).kind
        else {
            return None;
        };
        let r = call.args.get("r")?;
        match builtin {
            Sum | Product => Some(r),
            Min | Max => {
                let hir::Type::Relation(heading) = self.type_of(r) else {
                    return None;
                };
                heading.get("of")?.orders_as_stored().then_some(r)
            }
            _ => None,
        }
    }

    /// Whether the local variable `id` is a relation whose keys are all its
    /// whole heading, as [`FunctionBuilder::key_checked`] checks nothing of,
    /// and which any relation has: there is then nothing to check as it is
    /// bound, which would have to be done in canonical order.
    fn is_relation_of_no_key_to_check(&self, id: hir::LocalId) -> bool {
        let local = &self.locals[id.0];
        matches!(local.ty, hir::Type::Relation(_))
            && local
                .keys
                .iter()
                .all(|key| key.len() == attribute_names(&local.ty).count())
    }

    /// What the reads of the local variable `id` in `expr`, with `around` around it, want
    /// of its order, all of them: any order, a sorted one, or canonical order. A read wants
    /// what is around it, and canonical order if it is searched more than once, or for each
    /// tuple of something, as sorting once is less work then, and if it wants a sorted order
    /// again for each tuple, or time round a loop, as each might sort it into another.
    fn read_want(
        &self,
        id: hir::LocalId,
        expr: &hir::Expr,
        around: Around,
        reads: &mut ReadsOf,
    ) -> Want {
        match expr {
            hir::Expr::Local(read) if *read == id => {
                reads.searches += usize::from(around.searched);
                let again = reads.searches > 1
                    || (around.searched && around.repeated)
                    || (around.want == Want::Sorted && around.repeated);
                if again || (around.want == Want::Canonical && around.repeated) {
                    reads.canonical_again = true;
                }
                if again { Want::Canonical } else { around.want }
            }
            hir::Expr::At(_, inner) => self.read_want(id, inner, around, reads),
            // Either form is what its reader reads.
            hir::Expr::AdaptedRead(read) => self
                .read_want(id, &read.shared, around, reads)
                .stronger(self.read_want(id, &read.narrowed, around, reads)),
            hir::Expr::Let(bound, ..) if *bound == id => Want::Canonical,
            hir::Expr::Let(_, value, body) => {
                let value_around = Around {
                    want: Want::Canonical,
                    ..around
                };
                self.read_want(id, value, value_around, reads)
                    .stronger(self.read_want(id, body, around, reads))
            }
            _ => {
                let loosely = self.reads_loosely(expr);
                let passes_order_down = self.passes_order_down(expr);
                let searches_it = matches!(
                    expr,
                    hir::Expr::And(..) | hir::Expr::Semijoin(..) | hir::Expr::Antijoin(..)
                );
                let planned = around.planned
                    || matches!(expr, hir::Expr::And(lhs, rhs) if several_joined(lhs, rhs).is_some());
                let mut wanted = Want::Any;
                expr.for_each_child(|child, _| {
                    // What a restriction looks up is read only as the lookup has it: what
                    // reads the restriction reads the run it finds, which is put in the order
                    // that wants ([`FunctionBuilder::restrict`]).
                    let looked_up = local_read(child) == Some(id) && self.looks_up(expr, child);
                    let is_loose = loosely.iter().any(|operand| core::ptr::eq(*operand, child));
                    let want = if is_loose && passes_order_down {
                        self.passed_down(expr, around.want)
                    } else if is_loose {
                        Want::Any
                    } else if matches!(expr, hir::Expr::Combine(..))
                        && around.want != Want::Canonical
                    {
                        // Merged in whatever order both come in (`FunctionBuilder::combine`).
                        Want::Sorted
                    } else {
                        Want::Canonical
                    };
                    let child_around = Around {
                        want: if looked_up { Want::Any } else { want },
                        searched: !looked_up && (around.searched || searches_it),
                        repeated: around.repeated || expr.computes_again(child),
                        planned,
                    };
                    if local_read(child) == Some(id) && !planned {
                        self.note_leading(expr, child, child_around.repeated, reads);
                    }
                    wanted = wanted.stronger(self.read_want(id, child, child_around, reads));
                });
                wanted
            }
        }
    }

    /// Whether `expr` is a restriction that looks its operand `child` up: its condition
    /// equates attributes of it to values the same for every tuple
    /// ([`FunctionBuilder::looked_up_attributes`]), whose runs are found in it sorted by them
    /// first.
    fn looks_up(&self, expr: &hir::Expr, child: &hir::Expr) -> bool {
        let (hir::Expr::Restrict(_, condition), hir::Type::Relation(heading)) =
            (expr, self.type_of(child))
        else {
            return false;
        };
        !self.looked_up_attributes(&heading, condition).is_empty()
    }

    /// The attributes of `heading` that `condition`, on the tuples of a relation of it,
    /// equates to values the same for every tuple ([`FunctionBuilder::equated`]): what a
    /// restriction looks its tuples up by, as what can fail in those values is computed once,
    /// before it does ([`FunctionBuilder::each_reading_invariants`]).
    fn looked_up_attributes(&self, heading: &hir::Heading, condition: &hir::Expr) -> Vec<String> {
        let conjuncts = condition.conjuncts();
        heading
            .attributes()
            .filter(|(name, ty)| {
                conjuncts
                    .iter()
                    .any(|conjunct| self.equated(conjunct, name, ty).is_some())
            })
            .map(|(name, _)| name.to_string())
            .collect()
    }

    /// Note in `reads` what `expr` would have its operand `child`, a local variable or a
    /// binding, sorted by first: a restriction's attributes it looks up, by name
    /// ([`FunctionBuilder::looked_up_attributes`]), and a join's or a semijoin's common
    /// attributes, by name, as it compares them. A restriction found again for each tuple, or time round a loop, if
    /// `repeated`, is looked up again.
    fn note_leading(
        &self,
        expr: &hir::Expr,
        child: &hir::Expr,
        repeated: bool,
        reads: &mut ReadsOf,
    ) {
        let hir::Type::Relation(heading) = self.type_of(child) else {
            return;
        };
        match expr {
            hir::Expr::Restrict(_, condition) => {
                let equated = self.looked_up_attributes(&heading, condition);
                if !equated.is_empty() && repeated {
                    reads.looked_up_again = true;
                }
                reads.prefer(equated);
            }
            hir::Expr::And(lhs, rhs)
            | hir::Expr::Semijoin(lhs, rhs)
            | hir::Expr::Antijoin(lhs, rhs) => {
                let other = if core::ptr::eq(lhs.as_ref(), child) {
                    rhs
                } else {
                    lhs
                };
                let hir::Type::Relation(other) = self.type_of(other) else {
                    return;
                };
                let common = heading
                    .attributes()
                    .filter(|(name, _)| other.get(name).is_some())
                    .map(|(name, _)| name.to_string())
                    .collect();
                reads.prefer(common);
            }
            _ => {}
        }
    }

    /// Note what all that reads the local variable or binding `id` wants of its order, as
    /// `reads` gathered it, and what they would have it sorted by first. One looked up again
    /// by what they would is wanted sorted, which finding it by binary search pays for.
    fn note_local(&mut self, id: hir::LocalId, want: Want, reads: ReadsOf) {
        let want = self.leading_want(id, want, reads);
        if want != Want::Canonical {
            self.local_wants.insert(id, want);
        }
    }

    /// What the local variable or binding `id`, wanted as `want` by what reads it, as `reads`
    /// gathered them, is computed in: sorted, if it is looked up again by what they would
    /// have it sorted by first, which is noted, unless it is wanted in canonical order more
    /// than once, or wanted so and not looked up again. What wants it in canonical order
    /// once sorts it once, where a lookup that runs again would go through all of it each
    /// time.
    fn leading_want(&mut self, id: hir::LocalId, want: Want, reads: ReadsOf) -> Want {
        if want == Want::Canonical && (reads.canonical_again || !reads.looked_up_again) {
            return want;
        }
        let Some(leading) = reads.leading else {
            return want;
        };
        self.local_leading.insert(id, leading);
        if reads.looked_up_again {
            Want::Sorted
        } else {
            want
        }
    }

    /// What `expr`, which makes what it reads in the order it reads it, asks its operand,
    /// asked `want` itself, as it is computed: what it is asked, but canonical order for a
    /// sorted one, of what isn't kept in any but that (`FunctionBuilder::sorted_place`): a
    /// projection, and an extension that replaces an attribute.
    fn passed_down(&self, expr: &hir::Expr, want: Want) -> Want {
        let keeps_sorted = match expr {
            hir::Expr::Remove(..) => false,
            hir::Expr::Extend(operand, _, attributes) => match self.type_of(operand) {
                hir::Type::Relation(heading) => {
                    attributes.keys().all(|name| heading.get(name).is_none())
                }
                _ => false,
            },
            _ => true,
        };
        if want == Want::Sorted && !keeps_sorted {
            Want::Canonical
        } else {
            want
        }
    }

    /// Whether what `expr` makes is in canonical order for nothing if what it reads is, so that
    /// a reader wanting it so wants that too.
    fn passes_order_down(&self, expr: &hir::Expr) -> bool {
        match expr {
            hir::Expr::Remove(operand, names) => keeps_tuples_apart(&self.view_of(operand), names),
            hir::Expr::And(..)
            | hir::Expr::Semijoin(..)
            | hir::Expr::Antijoin(..)
            | hir::Expr::Restrict(..)
            | hir::Expr::Extend(..)
            | hir::Expr::Rename(..) => true,
            _ => false,
        }
    }

    /// As [`FunctionBuilder::read_want`], of what `block` reads, its statements run again if
    /// `repeated`. What a declaration of a local variable that can be left out of canonical
    /// order is computed from is read as that is wanted; `block`'s later ones are known by
    /// now.
    fn block_read_want(
        &self,
        id: hir::LocalId,
        block: &hir::Block,
        repeated: bool,
        reads: &mut ReadsOf,
    ) -> Want {
        let mut wanted = Want::Any;
        for (_, statement) in &block.statements {
            let again = repeated
                || matches!(
                    statement,
                    hir::Statement::Loop(..) | hir::Statement::For(..)
                );
            let around = Around {
                repeated,
                ..Around::wanted_in_order()
            };
            let mut read =
                |expr: &hir::Expr, around: Around| self.read_want(id, expr, around, &mut *reads);
            let own = match statement {
                hir::Statement::Assign(assignments) => {
                    assignments.iter().fold(Want::Any, |wanted, assignment| {
                        let want = match &assignment.target {
                            hir::Target::Local(target) => self
                                .local_wants
                                .get(target)
                                .copied()
                                .unwrap_or(Want::Canonical),
                            _ => Want::Canonical,
                        };
                        wanted.stronger(read(&assignment.value, Around { want, ..around }))
                    })
                }
                hir::Statement::Call(call) => call
                    .args
                    .values()
                    .fold(Want::Any, |wanted, arg| wanted.stronger(read(arg, around))),
                // What a `return` gives is the operator's value, in its order.
                hir::Statement::If(condition, ..)
                | hir::Statement::For(_, condition, _)
                | hir::Statement::Fail(condition)
                | hir::Statement::Warn(condition)
                | hir::Statement::Return(Some(condition)) => read(condition, around),
                hir::Statement::Loop(condition, ..) => read(
                    condition,
                    Around {
                        repeated: true,
                        ..around
                    },
                ),
                hir::Statement::Declare(_)
                | hir::Statement::Transaction(..)
                | hir::Statement::Return(None)
                | hir::Statement::Leave(_)
                | hir::Statement::Iterate(_) => Want::Any,
            };
            wanted = wanted.stronger(own);
            for inner in statement.blocks() {
                wanted = wanted.stronger(self.block_read_want(id, inner, again, &mut *reads));
            }
        }
        wanted
    }

    /// How `expr`, a relation or a tuple, is read through any renames around it.
    fn view_of<'e>(&self, expr: &'e hir::Expr) -> View<'e> {
        let named = self.type_of(expr);
        let (hir::Type::Tuple(heading) | hir::Type::Relation(heading)) = &named else {
            unreachable!("only relations and tuples are renamed");
        };
        let mut underlying = Shape::plain(heading).names;
        let mut inner = expr;
        while let hir::Expr::Rename(operand, renamings) = inner {
            for own in underlying.values_mut() {
                if let Some((before, _)) = renamings.iter().find(|(_, after)| *after == own) {
                    own.clone_from(before);
                }
            }
            inner = operand;
        }
        let ty = self.type_of(inner);
        let (hir::Type::Tuple(physical) | hir::Type::Relation(physical)) = &ty else {
            unreachable!("only relations and tuples are renamed");
        };
        let shape = Shape {
            physical: physical.clone(),
            names: underlying,
        };
        View {
            inner,
            ty,
            named,
            shape,
        }
    }

    /// Where `expr`'s value is in memory: computed into a new stack slot, if
    /// it is computed.
    fn place(&mut self, expr: &hir::Expr, cx: &mut Lowering) -> lir::Place {
        match expr {
            hir::Expr::Constant(value) => {
                let field = cx.value_field(value);
                in_data(cx.constant(vec![field]))
            }
            hir::Expr::Variable(id) => in_data(self.variable_data[id.0]),
            hir::Expr::ComputedConstant(id, _) => self.constant_read(*id, cx),
            hir::Expr::AdaptedRead(read) => self.adapted_read(expr, read, Want::Canonical, cx).0,
            hir::Expr::Local(id) => {
                let (place, order) = self.read_local(*id, cx);
                let ty = self.locals[id.0].ty.clone();
                self.canonical(place, order, &ty, cx)
            }
            hir::Expr::At(site, expr) => self.place_at(*site, expr, cx),
            hir::Expr::Let(..) => self.let_chain(expr, Want::Canonical, cx).0,
            hir::Expr::Run(block, value) => self.run(block, value, cx),
            hir::Expr::Parameter(name) if self.pointers.contains_key(name) => {
                self.pointee(name, cx)
            }
            hir::Expr::Parameter(name) => self.parameter_place(name),
            hir::Expr::Unary(op, operand) => {
                let src = self.place(operand, cx);
                self.unary(*op, src)
            }
            hir::Expr::Extract(tuple, name) => self.extracted(tuple, name, cx),
            hir::Expr::Tuple(attributes) => {
                let ty = self.type_of(expr);
                let dst = self.slot(cx.layout(&ty));
                self.fill_tuple(attributes, &dst, cx);
                dst
            }
            hir::Expr::Relation(heading, tuples) => self.relation(heading, tuples, cx),
            hir::Expr::Array(element, elements) => self.array(element, elements, cx),
            hir::Expr::Index(array, position) => self.index(array, position, cx),
            hir::Expr::WithElements(array, changes) => self.with_elements(array, changes, cx),
            hir::Expr::Load(relation, loading) => self.load(expr, relation, loading, cx),
            hir::Expr::Unload(array, unloading) => self.unload(expr, array, unloading, cx),
            hir::Expr::If(condition, then, otherwise) => {
                self.conditional(expr, condition, [then, otherwise], cx)
            }
            hir::Expr::Remove(operand, names) => {
                self.removed(operand, names, Want::Canonical, None, cx).0
            }
            hir::Expr::Rename(..) => self.renamed(expr, Want::Canonical, cx).0,
            hir::Expr::And(lhs, rhs) => self.joined(lhs, rhs, Want::Canonical, cx).0,
            hir::Expr::Ungroup(operand, names) => self.ungroup(operand, names, cx),
            hir::Expr::Only(operand) => self.only(operand, cx),
            // A value of a type of the program's own is laid out as the tuple
            // of its components.
            hir::Expr::Select(..) | hir::Expr::SelectIn(..) => self.selection(expr, cx),
            hir::Expr::Components(operand) => self.components(operand, cx),
            hir::Expr::Treat(..) | hir::Expr::TreatAs(..) | hir::Expr::Is(..) => {
                self.typed(expr, cx)
            }
            // It has no value: one is never computed in its slot.
            hir::Expr::Fail(error, ty) => {
                self.fail_with(error, cx);
                self.slot(cx.layout(ty))
            }
            hir::Expr::Semijoin(lhs, rhs) => self.matching(lhs, rhs, false, Want::Canonical, cx).0,
            hir::Expr::Antijoin(lhs, rhs) => self.matching(lhs, rhs, true, Want::Canonical, cx).0,
            hir::Expr::Restrict(operand, condition) => {
                self.restrict(operand, condition, Want::Canonical, cx).0
            }
            hir::Expr::Tclose(operand) => self.closed(operand, cx),
            hir::Expr::Combine(combination, lhs, rhs) => {
                self.combine(*combination, lhs, rhs, Want::Canonical, cx).0
            }
            hir::Expr::Current(depth, _) => {
                self.tuples[self.tuples.len() - 1 - depth].place.clone()
            }
            hir::Expr::Call(call) => self.read_only_call(call, cx),
            hir::Expr::Extend(operand, _, attributes) => {
                self.extend(expr, operand, attributes, Want::Canonical, cx)
                    .0
            }
            // Decided by any operand that decides it, those that can fail included.
            hir::Expr::Connective(
                connective @ (hir::Connective::And | hir::Connective::Or),
                operands,
            ) if operands.iter().any(hir::Expr::can_fail) => {
                self.deciding_connective(*connective, operands, cx)
            }
            hir::Expr::Connective(connective, operands) => {
                self.connective(expr, *connective, operands, cx)
            }
            hir::Expr::Binary(op, lhs, rhs) => {
                let operand_ty = self.type_of(lhs);
                let lhs = self.place(lhs, cx);
                let rhs = self.place(rhs, cx);
                self.binary(*op, &operand_ty, lhs, rhs, cx)
            }
        }
    }

    /// `expr`, the value of one of `branches`, `then` if `condition` is
    /// true and else `otherwise`, copied into a new stack slot: a branch to
    /// a block of each, which computes only its own, then on to one after
    /// both.
    fn conditional(
        &mut self,
        expr: &hir::Expr,
        condition: &hir::Expr,
        branches: [&hir::Expr; 2],
        cx: &mut Lowering,
    ) -> lir::Place {
        let value = cx.layout(&self.type_of(expr));
        let condition = self.place(condition, cx);
        let dst = self.slot(value.clone());
        let (then, otherwise, after) = (self.new_block(), self.new_block(), self.new_block());
        self.end_block(
            lir::Terminator::Branch {
                condition,
                then,
                otherwise,
            },
            then,
        );
        for (branch, next) in branches.into_iter().zip([otherwise, after]) {
            let src = self.place(branch, cx);
            self.insts.push(lir::Inst::Copy {
                dst: dst.clone(),
                src,
                layout: value.clone(),
            });
            self.end_block(lir::Terminator::Jump(after), next);
        }
        dst
    }

    /// `expr`, the `connective` of `operands`: an `xor`, or an `and` or an `or` none of
    /// whose operands can fail (see [`FunctionBuilder::deciding_connective`]). Of an `and` or an
    /// `or` of those that can't, each is computed only if those before it
    /// don't decide the result: since they can't fail, that only saves
    /// work, which is worth a branch unless those after it are only read.
    /// Otherwise every operand is computed.
    fn connective(
        &mut self,
        expr: &hir::Expr,
        connective: hir::Connective,
        operands: &[hir::Expr],
        cx: &mut Lowering,
    ) -> lir::Place {
        let (first, rest) = operands.split_first().expect("a connective has operands");
        // The result when the first is true and when it is false, if that
        // decides it.
        let decided = [true, false].map(|value| {
            connective
                .decided(value)
                .map(|result| hir::Expr::Constant(hir::Value::Boolean(result)))
        });
        if !expr.can_fail() && decided.iter().any(Option::is_some) && !rest.iter().all(is_read) {
            let rest = connective.of(rest.to_vec());
            let branches = decided.each_ref().map(|d| d.as_ref().unwrap_or(&rest));
            return self.conditional(expr, first, branches, cx);
        }
        let op = match connective {
            hir::Connective::And => lir::BinaryOp::And(lir::Bits::Boolean),
            hir::Connective::Or => lir::BinaryOp::Or(lir::Bits::Boolean),
            hir::Connective::Xor => lir::BinaryOp::Xor(lir::Bits::Boolean),
        };
        let mut result = self.place(first, cx);
        for operand in rest {
            let rhs = self.place(operand, cx);
            let dst = self.slot(scalar_layout(hir::Scalar::Boolean));
            self.insts.push(lir::Inst::Binary {
                dst: dst.clone(),
                op,
                lhs: result,
                rhs,
            });
            result = dst;
        }
        result
    }

    /// `connective`, an `and` or an `or`, of `operands`, some of which can fail: decided by
    /// any operand that decides it, false for `and` and true for `or`, whatever the others
    /// do, and otherwise failing as the first operand written that fails does (see
    /// [`hir::Connective::And`]). Those that can't fail are computed first, each only if
    /// those before it don't decide the result, then those that can, in the order written,
    /// as steps ([`FunctionBuilder::steps_put_aside`]): one that fails is put aside, as one
    /// after it may yet decide the result, and if none does, the first to fail is computed
    /// again, and fails as it did. The result is in a new slot.
    fn deciding_connective(
        &mut self,
        connective: hir::Connective,
        operands: &[hir::Expr],
        cx: &mut Lowering,
    ) -> lir::Place {
        let result = self.slot(scalar_layout(hir::Scalar::Boolean));
        let (settled, done) = (self.new_block(), self.new_block());
        let decides = !connective.identity();
        let (failing, safe): (Vec<&hir::Expr>, Vec<&hir::Expr>) =
            operands.iter().partition(|operand| operand.can_fail());
        for operand in safe {
            let value = self.place(operand, cx);
            self.decide_or_on(value, decides, settled);
        }
        let calls = failing.iter().any(|operand| makes_calls(operand));
        let steps = self.steps_failing_again(
            failing.len(),
            calls,
            cx,
            |this, i, cx| this.place(failing[i], cx),
            |this, _, value, _| this.decide_or_on(value, decides, settled),
        );
        let failed = self.steps_failed(&steps.failing);
        let (again, undecided) = (self.new_block(), self.new_block());
        self.end_block(
            lir::Terminator::Branch {
                condition: failed,
                then: again,
                otherwise: undecided,
            },
            again,
        );
        self.fail_again(&steps, cx);
        self.settle_connective(&result, connective, [undecided, settled], done, cx);
        result
    }

    /// `operand` without the attributes `names`, reshaped.
    fn removed(
        &mut self,
        operand: &hir::Expr,
        names: &BTreeSet<String>,
        want: Want,
        leading: Option<&[String]>,
        cx: &mut Lowering,
    ) -> (lir::Place, Order) {
        let view = self.view_of(operand);
        // Only what can't make two tuples equal can be left as it comes: reshaping is what
        // removes any that it does make, by sorting them, whatever order they are in. What
        // can't, wanted in order, is in order for nothing if what it comes from is.
        let (order, reads) = match (want, keeps_tuples_apart(&view, names)) {
            (Want::Canonical | Want::Sorted, true) => (Order::Canonical, Want::Canonical),
            (_, true) => (Order::Loose, Want::Any),
            (_, false) => (Order::Canonical, Want::Any),
        };
        // A `replace` can make tuples equal, which this removes, so needn't be sorted first.
        let (src, _) = match view.inner {
            hir::Expr::Extend(extended, _, attributes)
                if reads == Want::Any && matches!(view.ty, hir::Type::Relation(_)) =>
            {
                self.extend(view.inner, extended, attributes, Want::Repeated, cx)
            }
            inner => self.given(inner, reads, cx),
        };
        // The kept attributes, in their own order, which is the result's too.
        let sources: Vec<usize> = view
            .shape
            .names
            .keys()
            .filter(|name| !names.contains(*name))
            .map(|name| {
                view.shape
                    .position(name)
                    .expect("a rename reads only attributes the operand has")
            })
            .collect();
        let result = view.named.without(names);
        // Sorted anyway, it is sorted by what its readers would have it sorted by first.
        let order = match (order, leading, &result) {
            (Order::Canonical, Some(leading), hir::Type::Relation(heading)) => {
                cx.leading_order(leading, heading)
            }
            _ => order,
        };
        let dst = self.reshape(
            &view.ty,
            &result,
            &sources,
            src.clone(),
            Made::Wanted(order),
            cx,
        );
        self.operand_read(view.inner, &src, &view.ty, cx);
        (dst, order)
    }

    /// `expr`, a rename, of whatever it renames, computed in any order. It is where that is,
    /// if that has the same layout, in the same order under the new names
    /// ([`Lowering::read_order`]): otherwise it is reshaped, sorted unless `want` needn't be.
    fn renamed(&mut self, expr: &hir::Expr, want: Want, cx: &mut Lowering) -> (lir::Place, Order) {
        let view = self.view_of(expr);
        let (src, order) = self.given(view.inner, want, cx);
        // Its tuples where they are, sorted by the same attributes under their new names; a
        // tuple has no order.
        if view.shape.is_as_laid_out() {
            return match view.ty {
                hir::Type::Relation(_) => (src, cx.read_order(order, &view)),
                _ => (src, order),
            };
        }
        let dst = self.reshape(
            &view.ty,
            &view.named,
            &view.shape.positions(),
            src.clone(),
            Made::Wanted(want.as_comes()),
            cx,
        );
        self.operand_read(view.inner, &src, &view.ty, cx);
        (dst, want.as_comes())
    }

    /// `expr`, a rename, of whatever it renames, computed sorted, in the order that is less
    /// work: what it renames, sorted, is sorted by the same attributes under their new
    /// names, so it is reshaped with none of its tuples moved from where they are in it,
    /// and the runtime is told so, with nothing to check.
    fn renamed_in_order(&mut self, expr: &hir::Expr, cx: &mut Lowering) -> (lir::Place, Order) {
        let view = self.view_of(expr);
        let (src, order) = self.sorted_place(view.inner, cx);
        let renamed = cx.renamed_order(order, &view);
        if view.shape.is_as_laid_out() {
            return (src, renamed);
        }
        let dst = self.reshape(
            &view.ty,
            &view.named,
            &view.shape.positions(),
            src.clone(),
            Made::Kept(renamed),
            cx,
        );
        self.operand_read(view.inner, &src, &view.ty, cx);
        (dst, renamed)
    }

    /// Copy each of `attributes`, in name order, into a field of the tuple
    /// at `dst`.
    fn fill_tuple(
        &mut self,
        attributes: &BTreeMap<String, hir::Expr>,
        dst: &lir::Place,
        cx: &mut Lowering,
    ) {
        for (i, value) in attributes.values().enumerate() {
            let ty = self.type_of(value);
            let src = self.place(value, cx);
            let mut to = dst.clone();
            to.path.push(i);
            self.insts.push(lir::Inst::Copy {
                dst: to,
                src,
                layout: cx.layout(&ty),
            });
        }
    }

    /// A relation with `heading` and `tuples`, some known only when the
    /// program runs: built one after another in a slot, then sorted and
    /// rid of duplicates by the runtime, into memory released when the
    /// statement ends.
    fn relation(
        &mut self,
        heading: &hir::Heading,
        tuples: &[hir::Expr],
        cx: &mut Lowering,
    ) -> lir::Place {
        let tuple_layout = cx.layout(&hir::Type::Tuple(heading.clone()));
        // A struct of tuples with one layout is an array: each is padded to
        // its alignment, so the stride is the tuple's size.
        let array = self.slot(lir::Layout::Struct(vec![
            tuple_layout.clone();
            tuples.len()
        ]));
        for (k, tuple) in tuples.iter().enumerate() {
            let mut element = array.clone();
            element.path.push(k);
            if let hir::Expr::Tuple(attributes) = tuple {
                self.fill_tuple(attributes, &element, cx);
            } else {
                let src = self.place(tuple, cx);
                self.insts.push(lir::Inst::Copy {
                    dst: element,
                    src,
                    layout: tuple_layout.clone(),
                });
            }
        }
        let count = cx.constant(vec![lir::Field::Usize(tuples.len() as u64)]);
        let ty = hir::Type::Relation(heading.clone());
        let descriptor = lir::Place::whole(lir::Base::Data(cx.descriptor(&ty)));
        let dst = self.slot(cx.layout(&ty));
        self.insts.push(lir::Inst::CallRuntime {
            function: lir::RuntimeFn::Relation,
            args: vec![
                dst.clone(),
                array,
                lir::Place {
                    base: lir::Base::Data(count),
                    path: vec![0],
                },
                descriptor,
            ],
        });
        self.scoped = true;
        dst
    }

    /// An array of `elements`, of type `element`, some known only when the
    /// program runs: each computed into its place in memory the runtime
    /// allocates, which is released when the statement ends.
    fn array(
        &mut self,
        element: &hir::Type,
        elements: &[hir::Expr],
        cx: &mut Lowering,
    ) -> lir::Place {
        let ty = hir::Type::Array(Box::new(element.clone()));
        let element_layout = cx.layout(element);
        let count = usize_constant(len(elements.len()), cx);
        let dst = self.allocated(&ty, count, cx);
        for (k, value) in elements.iter().enumerate() {
            let src = self.place(value, cx);
            let index = usize_constant(len(k), cx);
            let to = self.element(&dst, &index, element_layout.clone());
            self.insts.push(lir::Inst::Copy {
                dst: to,
                src,
                layout: element_layout.clone(),
            });
        }
        self.adopt(&dst, &ty, cx);
        dst
    }

    /// The element of the array `array` at `position`, counting from 1: its
    /// own place in the array, with no copy. The program stops if the array
    /// has no element there.
    fn index(&mut self, array: &hir::Expr, position: &hir::Expr, cx: &mut Lowering) -> lir::Place {
        let hir::Type::Array(element) = self.type_of(array) else {
            unreachable!("sema indexes only arrays");
        };
        let src = self.place(array, cx);
        let at = self.place(position, cx);
        let index = self.checked_index(&src, at, cx);
        self.element(&src, &index, cx.layout(&element))
    }

    /// The index, from 0, a [`lir::Layout::Usize`], of the element of the
    /// array at `src` at `at`, an integer position counting from 1. The
    /// program stops if the array has no element there.
    fn checked_index(&mut self, src: &lir::Place, at: lir::Place, cx: &mut Lowering) -> lir::Place {
        let mut count = src.clone();
        count.path.push(1);
        let length = self.slot(lir::Layout::I64);
        self.insts.push(lir::Inst::Unary {
            dst: length.clone(),
            op: lir::UnaryOp::Integer,
            src: count,
        });
        let one = self.place(&hir::Expr::Constant(hir::Value::Integer(1)), cx);
        let integers = lir::Operand::Numeric(lir::Numeric::Integer);
        let below = self.slot(lir::Layout::U8);
        self.insts.push(lir::Inst::Binary {
            dst: below.clone(),
            op: lir::BinaryOp::Compare(lir::Comparison::Less, integers),
            lhs: at.clone(),
            rhs: one.clone(),
        });
        let beyond = self.slot(lir::Layout::U8);
        self.insts.push(lir::Inst::Binary {
            dst: beyond.clone(),
            op: lir::BinaryOp::Compare(lir::Comparison::Greater, integers),
            lhs: at.clone(),
            rhs: length.clone(),
        });
        let outside = self.slot(lir::Layout::U8);
        self.insts.push(lir::Inst::Binary {
            dst: outside.clone(),
            op: lir::BinaryOp::Or(lir::Bits::Boolean),
            lhs: below,
            rhs: beyond,
        });
        let (fail, inside) = (self.new_block(), self.new_block());
        self.end_block(
            lir::Terminator::Branch {
                condition: outside,
                then: fail,
                otherwise: inside,
            },
            fail,
        );
        let failure = lir::Failure::IndexOutOfRange {
            position: at.clone(),
            length,
        };
        self.fail(failure, inside);
        // From 1 up to the count, so from 0 less 1, which can't overflow.
        let from_zero = self.slot(lir::Layout::I64);
        self.insts.push(lir::Inst::Binary {
            dst: from_zero.clone(),
            op: lir::BinaryOp::Subtract(lir::Numeric::Integer),
            lhs: at,
            rhs: one,
        });
        let index = self.slot(lir::Layout::Usize);
        self.insts.push(lir::Inst::Unary {
            dst: index.clone(),
            op: lir::UnaryOp::Usize,
            src: from_zero,
        });
        index
    }

    /// `array` with `changes` made, a new array, in a buffer of the
    /// statement's that the changes are then made in, in place.
    fn with_elements(
        &mut self,
        array: &hir::Expr,
        changes: &[hir::ElementChange],
        cx: &mut Lowering,
    ) -> lir::Place {
        let ty = self.type_of(array);
        let src = self.place(array, cx);
        let computed: Vec<Computed> = changes.iter().map(|c| self.computed(c, cx)).collect();
        self.check_overlaps(changes, &computed, cx);
        if computed.len() > 1 {
            self.keep_values(&ty, &computed, cx);
        }
        let dst = self.slot(cx.layout(&ty));
        let descriptor = lir::Place::whole(lir::Base::Data(cx.descriptor(&ty)));
        self.insts.push(lir::Inst::CallRuntime {
            function: lir::RuntimeFn::Duplicate,
            args: vec![dst.clone(), src, descriptor],
        });
        self.scoped = true;
        for change in computed {
            self.changed_at(dst.clone(), &ty, change, cx);
        }
        dst
    }

    /// The positions and the value of `change`, each computed and copied
    /// into a slot of its own, as the variables they read can be changed
    /// by the statement's other assignments.
    fn computed(&mut self, change: &hir::ElementChange, cx: &mut Lowering) -> Computed {
        let mut copied = |this: &mut Self, expr: &hir::Expr| {
            let src = this.place(expr, cx);
            let layout = cx.layout(&this.type_of(expr));
            let slot = this.slot(layout.clone());
            this.insts.push(lir::Inst::Copy {
                dst: slot.clone(),
                src,
                layout,
            });
            slot
        };
        let positions = change.path.iter().map(|p| copied(self, p)).collect();
        let value = copied(self, &change.value);
        Computed { positions, value }
    }

    /// Stop the program if two of `changes`, to one array, computed as
    /// `computed`, overlap, unless their paths show they don't: if they
    /// have the same position at each level both have, so that they set
    /// one element, or one sets an element in the other's, writing those
    /// positions.
    fn check_overlaps(
        &mut self,
        changes: &[hir::ElementChange],
        computed: &[Computed],
        cx: &mut Lowering,
    ) {
        let integers = lir::Operand::Numeric(lir::Numeric::Integer);
        for j in 0..changes.len() {
            for i in 0..j {
                if changes[i].overlaps(&changes[j]) == Some(false) {
                    continue;
                }
                let mut overlap: Option<lir::Place> = None;
                for (p, q) in computed[i].positions.iter().zip(&computed[j].positions) {
                    let same = self.slot(lir::Layout::U8);
                    self.insts.push(lir::Inst::Binary {
                        dst: same.clone(),
                        op: lir::BinaryOp::Compare(lir::Comparison::Equal, integers),
                        lhs: p.clone(),
                        rhs: q.clone(),
                    });
                    overlap = Some(match overlap {
                        None => same,
                        Some(so_far) => {
                            let both = self.slot(lir::Layout::U8);
                            self.insts.push(lir::Inst::Binary {
                                dst: both.clone(),
                                op: lir::BinaryOp::And(lir::Bits::Boolean),
                                lhs: so_far,
                                rhs: same,
                            });
                            both
                        }
                    });
                }
                let overlap = overlap.expect("a path has a position");
                let (fail, apart) = (self.new_block(), self.new_block());
                self.end_block(
                    lir::Terminator::Branch {
                        condition: overlap,
                        then: fail,
                        otherwise: apart,
                    },
                    fail,
                );
                // The positions both have, which are the same.
                let lengths = [i, j].map(|k| computed[k].positions.len());
                let shared = lengths[0].min(lengths[1]);
                let positions = self.slot(lir::Layout::Struct(vec![lir::Layout::I64; shared]));
                for (k, position) in computed[i].positions[..shared].iter().enumerate() {
                    let mut dst = positions.clone();
                    dst.path.push(k);
                    self.insts.push(lir::Inst::Copy {
                        dst,
                        src: position.clone(),
                        layout: lir::Layout::I64,
                    });
                }
                let failure = lir::Failure::ElementConflict {
                    positions,
                    count: usize_constant(len(shared), cx),
                    nested: u8_constant(u8::from(lengths[0] != lengths[1]), cx),
                };
                self.fail(failure, apart);
            }
        }
    }

    /// Have the statement keep the values of `computed`, changes to the
    /// elements of arrays of type `ty`, that point to buffers, until it
    /// ends: one change made in place must not change a buffer another's
    /// value is, or is in, which is then held by more than one, and copied.
    fn keep_values(&mut self, ty: &hir::Type, computed: &[Computed], cx: &mut Lowering) {
        for change in computed {
            let element = element_at(ty, change.positions.len());
            if cx.points(&element) {
                let descriptor = lir::Place::whole(lir::Base::Data(cx.descriptor(&element)));
                self.insts.push(lir::Inst::CallRuntime {
                    function: lir::RuntimeFn::Keep,
                    args: vec![change.value.clone(), descriptor],
                });
                self.scoped = true;
            }
        }
    }

    /// Make `change` in the array at `holder`, of type `ty`, which holds a
    /// reference to its buffer: at each level, the array is made the only
    /// holder of its buffer, then the element at the position is the next
    /// level's, until the last, which is given the value, as a variable is
    /// assigned one.
    fn changed_at(
        &mut self,
        holder: lir::Place,
        ty: &hir::Type,
        change: Computed,
        cx: &mut Lowering,
    ) {
        let mut at = holder;
        let mut ty = ty.clone();
        for position in change.positions {
            let descriptor = lir::Place::whole(lir::Base::Data(cx.descriptor(&ty)));
            let hir::Type::Array(element) = ty else {
                unreachable!("sema indexes only arrays");
            };
            let index = self.checked_index(&at, position, cx);
            self.insts.push(lir::Inst::CallRuntime {
                function: lir::RuntimeFn::Unique,
                args: vec![at.clone(), descriptor],
            });
            at = self.element(&at, &index, cx.layout(&element));
            ty = *element;
        }
        if cx.points(&ty) {
            let descriptor = lir::Place::whole(lir::Base::Data(cx.descriptor(&ty)));
            self.insts.push(lir::Inst::CallRuntime {
                function: lir::RuntimeFn::Assign,
                args: vec![at, change.value, descriptor],
            });
        } else {
            self.insts.push(lir::Inst::Copy {
                dst: at,
                src: change.value,
                layout: cx.layout(&ty),
            });
        }
        self.scoped = true;
    }

    /// `expr`, `operand` with `attributes` added or replaced, computed into
    /// a new stack slot.
    fn extend(
        &mut self,
        expr: &hir::Expr,
        operand: &hir::Expr,
        attributes: &BTreeMap<String, hir::Expr>,
        want: Want,
        cx: &mut Lowering,
    ) -> (lir::Place, Order) {
        let ty = self.type_of(operand);
        let result_ty = self.type_of(expr);
        match (&ty, &result_ty) {
            (hir::Type::Tuple(heading), hir::Type::Tuple(result)) => {
                let src = self.place(operand, cx);
                let dst = self.slot(cx.layout(&result_ty));
                self.tuples.push(AtHand::one(src.clone()));
                self.extended(heading, &src, attributes, result, &dst, cx);
                self.tuples.pop();
                (dst, Order::Canonical)
            }
            (hir::Type::Relation(heading), hir::Type::Relation(result)) => {
                // A new attribute can't make two tuples equal, as replacing one can.
                let adds = attributes.keys().all(|name| heading.get(name).is_none());
                let (src, comes, made) = match want {
                    // Made in its operand's order, then by the attributes it adds.
                    Want::Sorted if adds => {
                        let (src, order) = self.sorted_place(operand, cx);
                        (
                            src,
                            order,
                            Made::Kept(cx.extended_order(order, heading, attributes)),
                        )
                    }
                    Want::Canonical | Want::Sorted => (
                        self.place(operand, cx),
                        Order::Canonical,
                        Made::Wanted(Order::Canonical),
                    ),
                    Want::Any | Want::Repeated => {
                        let (src, comes) = self.loose_place(operand, cx);
                        let order = if adds || want == Want::Repeated {
                            want.as_comes()
                        } else {
                            Order::Canonical
                        };
                        (src, comes, Made::Wanted(order))
                    }
                };
                let dst =
                    self.extend_relation(heading, (&src, comes), attributes, result, made, cx);
                self.operand_read(operand, &src, &ty, cx);
                let (Made::Wanted(order) | Made::Kept(order)) = made;
                (dst, order)
            }
            _ => unreachable!("sema extends only tuples and relations"),
        }
    }

    /// Build at `dst` the tuple at `src`, the tuple at hand, with `heading`,
    /// with `attributes` added or replaced: each attribute of `result`, in
    /// name order, is computed, or else copied from `src`.
    fn extended(
        &mut self,
        heading: &hir::Heading,
        src: &lir::Place,
        attributes: &BTreeMap<String, hir::Expr>,
        result: &hir::Heading,
        dst: &lir::Place,
        cx: &mut Lowering,
    ) {
        let own: Vec<&str> = heading.attributes().map(|(name, _)| name).collect();
        for (j, (name, ty)) in result.attributes().enumerate() {
            let from = if let Some(value) = attributes.get(name) {
                self.place(value, cx)
            } else {
                let i = own
                    .iter()
                    .position(|a| *a == name)
                    .expect("an attribute is the operand's or added");
                let mut from = src.clone();
                from.path.push(i);
                from
            };
            let mut to = dst.clone();
            to.path.push(j);
            self.insts.push(lir::Inst::Copy {
                dst: to,
                src: from,
                layout: cx.layout(ty),
            });
        }
    }

    /// The relation at `src`, with `heading`, its tuples in the order `comes` says, with
    /// `attributes` added to or replaced in each tuple, making one with the attributes of
    /// `result`: a loop over the tuples, building each new tuple in a new relation, which
    /// the runtime allocates and then sorts into the canonical order, without the
    /// duplicates a `replace` can make, or leaves in the order they come, as `made` says.
    /// What fails for a tuple is the first tuple's in canonical order to fail, however they
    /// come (see [`FunctionBuilder::each_at_hand_in`]).
    fn extend_relation(
        &mut self,
        heading: &hir::Heading,
        (src, comes): (&lir::Place, Order),
        attributes: &BTreeMap<String, hir::Expr>,
        result: &hir::Heading,
        made: Made,
        cx: &mut Lowering,
    ) -> lir::Place {
        let tuple = hir::Type::Tuple(heading.clone());
        let result_tuple = cx.layout(&hir::Type::Tuple(result.clone()));
        let result_ty = hir::Type::Relation(result.clone());
        let mut count = src.clone();
        count.path.push(1);
        let dst = self.allocated(&result_ty, count, cx);
        let built = hir::Type::Tuple(result.clone());
        let names: Vec<&String> = attributes.keys().collect();
        let values: Vec<&hir::Expr> = attributes.values().collect();
        self.each_reading_invariants(src, &values, cx, |this, values, cx| {
            let attributes: BTreeMap<String, hir::Expr> = names
                .iter()
                .zip(values)
                .map(|(name, value)| ((*name).clone(), (*value).clone()))
                .collect();
            let fails = values.iter().any(|value| value.can_fail());
            this.each_at_hand_in(src, &tuple, comes, fails, cx, |this, at_hand, index, cx| {
                let to = this.element(&dst, index, result_tuple.clone());
                // The work on each tuple in a scope of its own, which the
                // tuple built outlives.
                this.nested_scope(cx, |this, cx| {
                    this.extended(heading, at_hand, &attributes, result, &to, cx);
                    vec![(to.clone(), built.clone())]
                });
            });
        });
        match made {
            Made::Wanted(Order::Canonical) => {
                let descriptor = lir::Place::whole(lir::Base::Data(cx.descriptor(&result_ty)));
                self.insts.push(lir::Inst::CallRuntime {
                    function: lir::RuntimeFn::Sort,
                    args: vec![dst.clone(), descriptor],
                });
            }
            Made::Wanted(_) => self.adopt(&dst, &result_ty, cx),
            Made::Kept(order) => {
                self.adopt(&dst, &result_ty, cx);
                let descriptor =
                    lir::Place::whole(lir::Base::Data(cx.described(&result_ty, order)));
                self.insts.push(lir::Inst::SortedAs {
                    relation: dst.clone(),
                    descriptor,
                });
            }
        }
        dst
    }

    /// The relation `operand`, of type `ty`, as a restriction by `condition` reads it, and
    /// how its tuples are ordered: as `want` asks, unless it is a constant, written sorted
    /// by the attributes `condition` equates to values, so that its runs are found by binary
    /// search, and if those values can fail and are all `condition` says, so that its runs
    /// leave nothing of it to compute for each tuple.
    fn restricted_operand(
        &mut self,
        operand: &hir::Expr,
        ty: &hir::Type,
        condition: &hir::Expr,
        want: Want,
        cx: &mut Lowering,
    ) -> (lir::Place, Order) {
        let hir::Type::Relation(heading) = ty else {
            unreachable!("sema restricts only relations");
        };
        let Some(value) = constant_relation(operand) else {
            return self.given(operand, want, cx);
        };
        let only = condition
            .can_fail()
            .then(|| self.only_equated_attributes(heading, condition))
            .flatten();
        let leading = only.unwrap_or_else(|| self.equated_attributes(heading, condition));
        let order = Self::constant_order(value, &leading, cx);
        (self.constant_in(value, ty, order, cx), order)
    }

    /// The tuples of the relation `operand` for which `condition`, computed
    /// with each as the tuple at hand, is true, copied into a new relation, and how they are
    /// ordered: as they are in `operand`, so they need no sorting; the relation's
    /// count is then the number copied. A condition that can fail fails for the first tuple
    /// in canonical order it fails for, whatever order `operand` comes in (see
    /// [`FunctionBuilder::each_at_hand_in`]).
    fn restrict(
        &mut self,
        operand: &hir::Expr,
        condition: &hir::Expr,
        want: Want,
        cx: &mut Lowering,
    ) -> (lir::Place, Order) {
        let ty = self.type_of(operand);
        let hir::Type::Relation(heading) = &ty else {
            unreachable!("sema restricts only relations");
        };
        let narrowed = self.closure_narrowed(operand, heading, condition, cx);
        let (whole, order, rest) = match narrowed {
            Some((closure, rest)) if rest.is_empty() => return (closure, Order::Canonical),
            Some((closure, rest)) => (
                closure,
                Order::Canonical,
                Some(hir::Connective::And.of(rest)),
            ),
            None => {
                // Asked for canonical order, the operand is taken in a sorted one, as it is if it
                // is sorted, and only what is kept put in canonical order, which costs no more
                // than sorting the whole, and nothing if it is in canonical order already; so
                // a local looked up by another attribute can be kept sorted by that one.
                let operand_want = if want == Want::Canonical {
                    Want::Sorted
                } else {
                    want
                };
                let (whole, order) =
                    self.restricted_operand(operand, &ty, condition, operand_want, cx);
                (whole, order, None)
            }
        };
        let condition = rest.as_ref().unwrap_or(condition);
        // What its condition reads that no tuple changes, computed once, is what it looks its
        // tuples up by, which then can't fail.
        let result = self.slot(cx.layout(&ty));
        self.each_reading_invariants(&whole, &[condition], cx, |this, condition, cx| {
            this.restricted_into(&whole, &ty, order, condition[0], &result, cx);
        });
        self.operand_read(operand, &whole, &ty, cx);
        if want == Want::Canonical && order != Order::Canonical {
            return (self.canonical(result, order, &ty, cx), Order::Canonical);
        }
        (result, order)
    }

    /// Store at `result` the tuples of the relation at `whole`, of type `ty`, which come as
    /// `order` says, for which `condition` is true: those of the runs it looks up, found by
    /// binary search if they are sorted by what it equates to values ([`FunctionBuilder::equal_runs`]),
    /// or by an index ([`FunctionBuilder::kept_lookup`]), or else all of them, in a loop.
    fn restricted_into(
        &mut self,
        whole: &lir::Place,
        ty: &hir::Type,
        order: Order,
        condition: &hir::Expr,
        result: &lir::Place,
        cx: &mut Lowering,
    ) {
        let hir::Type::Relation(heading) = ty else {
            unreachable!("only a relation is restricted");
        };
        let tuple = cx.layout(&hir::Type::Tuple(heading.clone()));
        // Tuples that agree are together only if they are sorted, by the attributes their
        // sort order begins with.
        let (src, condition) = if matches!(order, Order::Canonical | Order::SortedBy(_)) {
            let (src, condition) =
                self.equal_runs(whole.clone(), ty, order, heading, condition, cx);
            // What it equates that its order doesn't begin with, an index can find.
            let src = if src == *whole {
                self.kept_lookup(src, ty, order, &condition, cx)
            } else {
                src
            };
            (src, condition)
        } else {
            (whole.clone(), condition.clone())
        };
        let mut count = src.clone();
        count.path.push(1);
        let dst = self.allocated(ty, count, cx);
        let kept = self.slot(lir::Layout::Usize);
        let zero = usize_constant(0, cx);
        self.insts.push(lir::Inst::Copy {
            dst: kept.clone(),
            src: zero,
            layout: lir::Layout::Usize,
        });
        let restricted = Restricted {
            src: &src,
            heading,
            order,
            dst: &dst,
            kept: &kept,
            tuple: &tuple,
        };
        self.restriction_loop(&restricted, &condition, cx);
        let mut len = dst.clone();
        len.path.push(1);
        self.insts.push(lir::Inst::Copy {
            dst: len,
            src: kept,
            layout: lir::Layout::Usize,
        });
        self.adopt(&dst, ty, cx);
        self.insts.push(lir::Inst::InOrderOf {
            dst: dst.clone(),
            src: whole.clone(),
        });
        self.insts.push(lir::Inst::Copy {
            dst: result.clone(),
            src: dst,
            layout: cx.layout(ty),
        });
    }

    /// The tuples of `restricted` for which `condition` is true, each kept as it says, in a
    /// loop over them.
    fn restriction_loop(
        &mut self,
        restricted: &Restricted,
        condition: &hir::Expr,
        cx: &mut Lowering,
    ) {
        let Restricted {
            src,
            heading,
            order,
            dst,
            kept,
            tuple,
        } = *restricted;
        let fails = condition.can_fail();
        let at_hand_ty = hir::Type::Tuple(heading.clone());
        let ranking = self.ranking_for(condition, cx);
        self.each_at_hand_in(
            src,
            &at_hand_ty,
            order,
            fails,
            cx,
            |this, at_hand, _, cx| {
                // The condition, computed in a scope of its own for each tuple,
                // which nothing it made outlives.
                let keep = this.slot(lir::Layout::U8);
                this.nested_scope(cx, |this, cx| {
                    let computed = match &ranking {
                        Some(ranking) => this.ranked_connective(condition, ranking, cx),
                        None => this.place(condition, cx),
                    };
                    this.insts.push(lir::Inst::Copy {
                        dst: keep.clone(),
                        src: computed,
                        layout: lir::Layout::U8,
                    });
                    Vec::new()
                });
                let (copy, next) = (this.new_block(), this.new_block());
                this.end_block(
                    lir::Terminator::Branch {
                        condition: keep,
                        then: copy,
                        otherwise: next,
                    },
                    copy,
                );
                let to = this.element(dst, kept, tuple.clone());
                this.insts.push(lir::Inst::Copy {
                    dst: to,
                    src: at_hand.clone(),
                    layout: tuple.clone(),
                });
                this.insts.push(lir::Inst::Increment {
                    place: kept.clone(),
                });
                this.end_block(lir::Terminator::Jump(next), next);
            },
        );
    }

    /// A loop over the tuples of the relation at `src`, which `each` lowers, given `exprs`,
    /// what it computes for each tuple, as a restriction's condition or an extension's
    /// values. Their parts that are the same for every tuple ([`invariant_parts`]) are
    /// computed once, if there is a tuple, before the loop, as steps
    /// ([`FunctionBuilder::steps_put_aside`]), into a tuple of them at hand around the loop's,
    /// and `each` is given `exprs` with them read from it instead ([`with_invariants`]), which
    /// then may not fail at all, so that the loop needn't put failures aside. If one fails, it
    /// would for every tuple, but a tuple fails only if what it computes needs it, which an
    /// `and` or an `or` may not (see [`hir::Connective::And`]), so the loop is then the one
    /// `each` lowers given `exprs` as they are, which finds the first tuple in canonical order
    /// to fail, if one does ([`FunctionBuilder::each_at_hand_in`]).
    fn each_reading_invariants(
        &mut self,
        src: &lir::Place,
        exprs: &[&hir::Expr],
        cx: &mut Lowering,
        mut each: impl FnMut(&mut Self, &[&hir::Expr], &mut Lowering),
    ) {
        let invariants = invariant_parts(exprs);
        if invariants.is_empty() {
            each(self, exprs, cx);
            return;
        }
        let names: Vec<String> = (0..invariants.len())
            .map(|i| hir::wrapped(&format!("invariant {i}")))
            .collect();
        let heading = hir::Heading::new(
            names
                .iter()
                .zip(&invariants)
                .map(|(name, part)| (name.clone(), self.type_of(part)))
                .collect(),
        );
        let values = self.slot(cx.layout(&hir::Type::Tuple(heading.clone())));
        // Each one's attribute's place in the tuple, which has them in name order.
        let fields: Vec<(lir::Place, lir::Layout)> = names
            .iter()
            .zip(&invariants)
            .map(|(name, part)| {
                let mut field = values.clone();
                field.path.push(
                    heading
                        .attributes()
                        .position(|(attribute, _)| attribute == name)
                        .expect("each is an attribute"),
                );
                (field, cx.layout(&self.type_of(part)))
            })
            .collect();
        let (computing, fast, done) = (self.new_block(), self.new_block(), self.new_block());
        // With no tuple, nothing reads them.
        let mut count = src.clone();
        count.path.push(1);
        let zero = usize_constant(0, cx);
        let some = self.slot(lir::Layout::U8);
        self.insts.push(lir::Inst::Binary {
            dst: some.clone(),
            op: lir::BinaryOp::Compare(lir::Comparison::Less, lir::Operand::Usize),
            lhs: zero,
            rhs: count,
        });
        self.end_block(
            lir::Terminator::Branch {
                condition: some,
                then: computing,
                otherwise: fast,
            },
            computing,
        );
        // Computed before the tuples are: they read none of them, but what's further out is
        // further out from them than from here.
        self.tuples.push(AtHand::one(src.clone()));
        let copied = |this: &mut Self, i: usize, computed: lir::Place| {
            let (field, layout) = fields[i].clone();
            this.insts.push(lir::Inst::Copy {
                dst: field,
                src: computed,
                layout,
            });
        };
        if invariants.iter().any(hir::Expr::can_fail) {
            let calls = invariants.iter().any(makes_calls);
            let failing = self.steps_put_aside(
                invariants.len(),
                calls,
                cx,
                |this, i, cx| this.place(&invariants[i], cx),
                |this, i, computed, _| copied(this, i, computed),
            );
            self.tuples.pop();
            let failed = self.steps_failed(&failing);
            let whole = self.new_block();
            self.end_block(
                lir::Terminator::Branch {
                    condition: failed,
                    then: whole,
                    otherwise: fast,
                },
                whole,
            );
            each(self, exprs, cx);
            self.end_block(lir::Terminator::Jump(done), fast);
        } else {
            for (i, part) in invariants.iter().enumerate() {
                let computed = self.place(part, cx);
                copied(self, i, computed);
            }
            self.tuples.pop();
            self.end_block(lir::Terminator::Jump(fast), fast);
        }
        let read: Vec<hir::Expr> = exprs
            .iter()
            .map(|expr| with_invariants((*expr).clone(), &invariants, &names, &heading, 0))
            .collect();
        let read: Vec<&hir::Expr> = read.iter().collect();
        self.tuples.push(AtHand::one(values));
        each(self, &read, cx);
        self.tuples.pop();
        self.end_block(lir::Terminator::Jump(done), done);
    }

    /// If `operand` is a transitive closure, and `condition` says one of its
    /// attributes is equal to a value that is the same for every tuple, the tuples
    /// for which that holds, in a new slot, found by following only the paths from
    /// or to that value ([`lir::RuntimeFn::TcloseOf`]), and what is left of
    /// `condition`. The first attribute's is preferred, whose arcs are in order
    /// already. An equality of a value that can't fail narrows what the other conjuncts
    /// are computed for, as for [`FunctionBuilder::equal_runs`], and one of a value that
    /// can is taken only if nothing else is left, computed if the closure has a tuple, which it
    /// does if `operand` has one.
    fn closure_narrowed(
        &mut self,
        operand: &hir::Expr,
        heading: &hir::Heading,
        condition: &hir::Expr,
        cx: &mut Lowering,
    ) -> Option<(lir::Place, Vec<hir::Expr>)> {
        let arcs = arcs_of_closure(operand)?;
        let conjuncts = condition.conjuncts();
        let (i, at, value) = condition.looked_up(heading, &|value| self.type_of(value))?;
        let fails = value.can_fail();
        let ty = self.type_of(arcs);
        let src = self.place(arcs, cx);
        let value = if fails {
            self.value_if_any_tuple(&src, value, cx)
        } else {
            self.tuples.push(AtHand::one(src.clone()));
            let value = self.place(value, cx);
            self.tuples.pop();
            value
        };
        let dst = self.slot(cx.layout(&ty));
        let descriptor = lir::Place::whole(lir::Base::Data(cx.descriptor(&ty)));
        let attribute = usize_constant(len(i), cx);
        self.insts.push(lir::Inst::CallRuntime {
            function: lir::RuntimeFn::TcloseOf,
            args: vec![dst.clone(), src.clone(), value, descriptor, attribute],
        });
        self.scoped = true;
        self.operand_read(arcs, &src, &ty, cx);
        let rest = conjuncts
            .iter()
            .enumerate()
            .filter(|(k, _)| *k != at)
            .map(|(_, conjunct)| conjunct.clone())
            .collect();
        Some((dst, rest))
    }

    /// If `closure` is a transitive closure, and the relation `other` has attributes
    /// in common with it: the tuples of the closure whose first such attribute has a
    /// value it has in `other`, in a new slot, found by following only the paths from
    /// or to those values ([`lir::RuntimeFn::TcloseOfEach`]), and where `other` is, which
    /// the caller reads. With no attribute in common, `other` says nothing of the values
    /// of the closure.
    fn closure_matched(
        &mut self,
        closure: &hir::Expr,
        other: &hir::Expr,
        cx: &mut Lowering,
    ) -> Option<(lir::Place, lir::Place)> {
        let arcs = arcs_of_closure(closure)?;
        let (ty, other_ty) = (self.type_of(arcs), self.type_of(other));
        let (hir::Type::Relation(heading), hir::Type::Relation(other_heading)) = (&ty, &other_ty)
        else {
            return None;
        };
        let common = common_attributes(&Shape::plain(heading), &Shape::plain(other_heading), cx);
        if common.is_empty() {
            return None;
        }
        let (src, values) = (self.place(arcs, cx), self.place(other, cx));
        let dst = self.slot(cx.layout(&ty));
        let descriptor = |ty: &hir::Type, cx: &mut Lowering| {
            lir::Place::whole(lir::Base::Data(cx.descriptor(ty)))
        };
        self.insts.push(lir::Inst::CallRuntime {
            function: lir::RuntimeFn::TcloseOfEach,
            args: vec![
                dst.clone(),
                src.clone(),
                values.clone(),
                descriptor(&ty, cx),
                descriptor(&other_ty, cx),
                lir::Place::whole(lir::Base::Data(cx.constant(common))),
            ],
        });
        self.scoped = true;
        self.operand_read(arcs, &src, &ty, cx);
        Some((dst, values))
    }

    /// The part of the relation at `src`, of type `ty`, with `heading`, that
    /// `condition` needs computed for each tuple, and what is left of
    /// `condition`. A relation's tuples are sorted by their attributes in
    /// name order, so those for which the first is equal to a value are a
    /// run, found by binary search ([`lir::RuntimeFn::EqualRun`]), and
    /// among them, so are those for which the next is, and so on, while
    /// `condition` has a conjunct for it (see [`FunctionBuilder::equated`]).
    /// An equality that is false decides the `and`, so the other conjuncts are
    /// computed only for the tuples it holds for, as they are in the run. If all that is left then is such equalities of
    /// values that can fail, see [`FunctionBuilder::failable_runs`].
    fn equal_runs(
        &mut self,
        src: lir::Place,
        ty: &hir::Type,
        order: Order,
        heading: &hir::Heading,
        condition: &hir::Expr,
        cx: &mut Lowering,
    ) -> (lir::Place, hir::Expr) {
        let mut src = src;
        let mut rest: Vec<&hir::Expr> = condition.conjuncts().iter().collect();
        // Its attributes in the order its tuples are sorted by, each with its place in it.
        let names = cx.sort_names(order, heading);
        let mut attributes = names
            .iter()
            .map(|name| {
                let attribute = heading
                    .get(name)
                    .expect("a sort order names the attributes");
                (name.as_str(), attribute)
            })
            .enumerate()
            .peekable();
        while let Some(&(i, (name, attribute))) = attributes.peek() {
            let found = rest.iter().enumerate().find_map(|(at, conjunct)| {
                let value = self.equated(conjunct, name, attribute)?;
                (!value.can_fail()).then_some((at, value))
            });
            let Some((at, value)) = found else {
                break;
            };
            attributes.next();
            rest.remove(at);
            // Computed before the tuples are: `value` reads none of them,
            // but what's further out is further out from it than from here.
            self.tuples.push(AtHand::one(src.clone()));
            let value = self.place(value, cx);
            self.tuples.pop();
            src = self.equal_run(src, &value, i, ty, order, cx);
        }
        let values = self.failable_equalities(&rest, attributes);
        if let Some(values) = values {
            rest.clear();
            src = self.failable_runs(&src, &values, ty, order, cx);
        }
        let rest = rest.into_iter().cloned().collect();
        (src, hir::Connective::And.of(rest))
    }

    /// The relation at `src`, of type `ty`, sorted in `order`, or, in a new slot, the tuples
    /// of it whose attributes `condition` equates to values that can't fail are equal to
    /// those values, if the runtime keeps an index of it on them, or it has now earned one
    /// ([`lir::RuntimeFn::KeptLookup`]): so a restriction repeated by attributes its order
    /// doesn't begin with, as one that another lookup's order or a canonical order has,
    /// finds its tuples by binary search once that pays, and goes through all of them until
    /// then, as before. Either way, it goes through them with all of `condition`.
    fn kept_lookup(
        &mut self,
        src: lir::Place,
        ty: &hir::Type,
        order: Order,
        condition: &hir::Expr,
        cx: &mut Lowering,
    ) -> lir::Place {
        let hir::Type::Relation(heading) = ty else {
            unreachable!("only a relation is restricted");
        };
        let equated: Vec<(String, hir::Type, hir::Expr)> = heading
            .attributes()
            .filter_map(|(name, attribute)| {
                condition.conjuncts().iter().find_map(|conjunct| {
                    let value = self.equated(conjunct, name, attribute)?;
                    (!value.can_fail())
                        .then(|| (name.to_string(), attribute.clone(), value.clone()))
                })
            })
            .collect();
        if equated.is_empty() {
            return src;
        }
        let on = hir::Heading::new(
            equated
                .iter()
                .map(|(name, attribute, _)| (name.clone(), attribute.clone()))
                .collect(),
        );
        let values = hir::Expr::Tuple(
            equated
                .into_iter()
                .map(|(name, _, value)| (name, value))
                .collect(),
        );
        // Computed before the tuples are: the values read none of them, but what's further
        // out is further out from them than from here.
        self.tuples.push(AtHand::one(src.clone()));
        let values = self.place(&values, cx);
        self.tuples.pop();
        let common = common_attributes(&Shape::plain(heading), &Shape::plain(&on), cx);
        let common_count = usize_constant(len(common.len()), cx);
        let (run, found) = (self.slot(cx.layout(ty)), self.slot(lir::Layout::U8));
        let descriptor = lir::Place::whole(lir::Base::Data(cx.described(ty, order)));
        self.insts.push(lir::Inst::CallRuntime {
            function: lir::RuntimeFn::KeptLookup,
            args: vec![
                run.clone(),
                src.clone(),
                values,
                descriptor,
                lir::Place::whole(lir::Base::Data(cx.constant(common))),
                common_count,
                found.clone(),
            ],
        });
        self.scoped = true;
        let chosen = self.slot(cx.layout(ty));
        self.insts.push(lir::Inst::Copy {
            dst: chosen.clone(),
            src,
            layout: cx.layout(ty),
        });
        let (indexed, after) = (self.new_block(), self.new_block());
        self.end_block(
            lir::Terminator::Branch {
                condition: found,
                then: indexed,
                otherwise: after,
            },
            indexed,
        );
        self.insts.push(lir::Inst::Copy {
            dst: chosen.clone(),
            src: run,
            layout: cx.layout(ty),
        });
        self.end_block(lir::Terminator::Jump(after), after);
        chosen
    }

    /// The run of tuples of the relation or run at `src`, of type `ty`, sorted in
    /// `order`, whose attribute `i` in that order, from the first, is equal to the value at
    /// `value`, in a new slot.
    fn equal_run(
        &mut self,
        src: lir::Place,
        value: &lir::Place,
        i: usize,
        ty: &hir::Type,
        order: Order,
        cx: &mut Lowering,
    ) -> lir::Place {
        let run = self.slot(cx.layout(ty));
        let descriptor = lir::Place::whole(lir::Base::Data(cx.described(ty, order)));
        let attribute = usize_constant(len(i), cx);
        self.insts.push(lir::Inst::CallRuntime {
            function: lir::RuntimeFn::EqualRun,
            args: vec![run.clone(), src, value.clone(), descriptor, attribute],
        });
        run
    }

    /// The values, each with its attribute's place from the first in name
    /// order, if each of the `conjuncts` left of a condition says the next of
    /// `attributes` is equal to a value that can fail.
    fn failable_equalities<'e, 'h>(
        &self,
        conjuncts: &[&'e hir::Expr],
        mut attributes: impl Iterator<Item = (usize, (&'h str, &'h hir::Type))>,
    ) -> Option<Vec<(usize, &'e hir::Expr)>> {
        if conjuncts.is_empty() {
            return None;
        }
        let mut left = conjuncts.to_vec();
        let mut values = Vec::new();
        while !left.is_empty() {
            let (i, (name, attribute)) = attributes.next()?;
            let (at, value) = left.iter().enumerate().find_map(|(at, conjunct)| {
                let value = self.equated(conjunct, name, attribute)?;
                value.can_fail().then_some((at, value))
            })?;
            left.remove(at);
            values.push((i, value));
        }
        Some(values)
    }

    /// The run of tuples of the relation or run at `src`, of type `ty`, sorted in `order`,
    /// whose attributes are equal to `values`, each with its place in that order, which can
    /// fail but read no tuple, and are all that is left of a condition, in a new slot. Each
    /// is the same for every tuple, so each is computed once, if there is a tuple, as a step
    /// ([`FunctionBuilder::steps_put_aside`]), before any of them narrows the tuples down,
    /// as they would be for the first. If none fails, they find the run by binary search.
    /// If one does, a tuple's condition fails only if no other conjunct is false of it (see
    /// [`hir::Connective::And`]): so if a tuple has each value of those that didn't fail,
    /// the first to fail is computed again, and fails as it did, and otherwise no tuple is
    /// kept.
    fn failable_runs(
        &mut self,
        src: &lir::Place,
        values: &[(usize, &hir::Expr)],
        ty: &hir::Type,
        order: Order,
        cx: &mut Lowering,
    ) -> lir::Place {
        let hir::Type::Relation(heading) = ty else {
            unreachable!("only a relation has runs");
        };
        let run = self.slot(cx.layout(ty));
        let done = self.new_block();
        // With no tuple, nothing is computed, and nothing reads the values.
        let mut count = src.clone();
        count.path.push(1);
        let zero = usize_constant(0, cx);
        let some = self.slot(lir::Layout::U8);
        self.insts.push(lir::Inst::Binary {
            dst: some.clone(),
            op: lir::BinaryOp::Compare(lir::Comparison::Less, lir::Operand::Usize),
            lhs: zero,
            rhs: count,
        });
        let (computing, found) = (self.new_block(), self.new_block());
        self.end_block(
            lir::Terminator::Branch {
                condition: some,
                then: computing,
                otherwise: found,
            },
            computing,
        );
        let (steps, places) = self.failable_values(src, values, cx);
        let failed = self.steps_failed(&steps.failing);
        let failing = self.new_block();
        self.end_block(
            lir::Terminator::Branch {
                condition: failed,
                then: failing,
                otherwise: found,
            },
            found,
        );
        let narrowed = places.iter().fold(src.clone(), |src, (i, value, _)| {
            self.equal_run(src, value, *i, ty, order, cx)
        });
        self.insts.push(lir::Inst::Copy {
            dst: run.clone(),
            src: narrowed,
            layout: cx.layout(ty),
        });
        self.end_block(lir::Terminator::Jump(done), failing);
        let matched = self.has_values(src, heading, order, &places, cx);
        let (again, none) = (self.new_block(), self.new_block());
        self.end_block(
            lir::Terminator::Branch {
                condition: matched,
                then: again,
                otherwise: none,
            },
            again,
        );
        self.fail_again(&steps, cx);
        self.end_block(lir::Terminator::Jump(done), none);
        // The run of none of them.
        self.insts.push(lir::Inst::Copy {
            dst: run.clone(),
            src: src.clone(),
            layout: cx.layout(ty),
        });
        let mut length = run.clone();
        length.path.push(1);
        let zero = usize_constant(0, cx);
        self.insts.push(lir::Inst::Copy {
            dst: length,
            src: zero,
            layout: lir::Layout::Usize,
        });
        self.end_block(lir::Terminator::Jump(done), done);
        run
    }

    /// `value`, the whole of a condition, which can fail, but reads no tuple, so is the same
    /// for each, computed if the relation at `src` has a tuple, as it would be for the first,
    /// in a slot of its own, which nothing reads with no tuple to compare it to.
    fn value_if_any_tuple(
        &mut self,
        src: &lir::Place,
        value: &hir::Expr,
        cx: &mut Lowering,
    ) -> lir::Place {
        let mut count = src.clone();
        count.path.push(1);
        let zero = usize_constant(0, cx);
        let some = self.slot(lir::Layout::U8);
        self.insts.push(lir::Inst::Binary {
            dst: some.clone(),
            op: lir::BinaryOp::Compare(lir::Comparison::Less, lir::Operand::Usize),
            lhs: zero,
            rhs: count,
        });
        let (computed, after) = (self.new_block(), self.new_block());
        self.end_block(
            lir::Terminator::Branch {
                condition: some,
                then: computed,
                otherwise: after,
            },
            computed,
        );
        self.tuples.push(AtHand::one(src.clone()));
        let layout = cx.layout(&self.type_of(value));
        let place = self.place(value, cx);
        let slot = self.slot(layout.clone());
        self.insts.push(lir::Inst::Copy {
            dst: slot.clone(),
            src: place,
            layout,
        });
        self.tuples.pop();
        self.end_block(lir::Terminator::Jump(after), after);
        slot
    }

    /// `values`, of [`FunctionBuilder::failable_runs`], each computed as a step
    /// ([`FunctionBuilder::steps_put_aside`]), with the tuple at hand the relation at `src`'s,
    /// further out than they read: the steps, and of each value, its attribute's place in
    /// the sort order, the slot it is computed into, and whether it was.
    fn failable_values(
        &mut self,
        src: &lir::Place,
        values: &[(usize, &hir::Expr)],
        cx: &mut Lowering,
    ) -> (Steps, Vec<(usize, lir::Place, lir::Place)>) {
        let places: Vec<(usize, lir::Place, lir::Place)> = values
            .iter()
            .map(|&(i, value)| {
                let layout = cx.layout(&self.type_of(value));
                (i, self.slot(layout), self.slot(lir::Layout::U8))
            })
            .collect();
        let no = u8_constant(0, cx);
        for (_, _, computed) in &places {
            self.insts.push(lir::Inst::Copy {
                dst: computed.clone(),
                src: no.clone(),
                layout: lir::Layout::U8,
            });
        }
        // Computed before the tuples are: a value reads none of them, but what's further
        // out is further out from it than from here.
        self.tuples.push(AtHand::one(src.clone()));
        let calls = values.iter().any(|(_, value)| makes_calls(value));
        let steps = self.steps_failing_again(
            values.len(),
            calls,
            cx,
            |this, j, cx| this.place(values[j].1, cx),
            |this, j, computed, cx| {
                let layout = cx.layout(&this.type_of(values[j].1));
                let (_, slot, done) = &places[j];
                this.insts.push(lir::Inst::Copy {
                    dst: slot.clone(),
                    src: computed,
                    layout,
                });
                let yes = u8_constant(1, cx);
                this.insts.push(lir::Inst::Copy {
                    dst: done.clone(),
                    src: yes,
                    layout: lir::Layout::U8,
                });
            },
        );
        self.tuples.pop();
        (steps, places)
    }

    /// Whether a tuple of the relation or run at `src`, with `heading`, sorted in `order`,
    /// has each of `values` that was computed, as [`FunctionBuilder::failable_runs`] keeps
    /// them, each with the attribute's place in that order, the slot of the value, and
    /// whether it was computed: in a new slot, found by reading each tuple.
    fn has_values(
        &mut self,
        src: &lir::Place,
        heading: &hir::Heading,
        order: Order,
        values: &[(usize, lir::Place, lir::Place)],
        cx: &mut Lowering,
    ) -> lir::Place {
        let matched = self.slot(lir::Layout::U8);
        let no = u8_constant(0, cx);
        self.insts.push(lir::Inst::Copy {
            dst: matched.clone(),
            src: no,
            layout: lir::Layout::U8,
        });
        let names = cx.sort_names(order, heading);
        let tuple = cx.layout(&hir::Type::Tuple(heading.clone()));
        self.each_tuple(src, tuple, cx, |this, at_hand, _, cx| {
            let next = this.new_block();
            for (i, value, computed) in values {
                let name = &names[*i];
                let ty = heading
                    .get(name)
                    .expect("a sort order names the attributes");
                let position = heading
                    .attributes()
                    .position(|(a, _)| a == name)
                    .expect("the attribute is the relation's");
                let mut attribute = at_hand.clone();
                attribute.path.push(position);
                let (compare, after) = (this.new_block(), this.new_block());
                this.end_block(
                    lir::Terminator::Branch {
                        condition: computed.clone(),
                        then: compare,
                        otherwise: after,
                    },
                    compare,
                );
                let equal = hir::BinaryOp::Compare(hir::Comparison::Equal, hir::Compared::of(ty));
                let same = this.binary(equal, ty, attribute, value.clone(), cx);
                this.end_block(
                    lir::Terminator::Branch {
                        condition: same,
                        then: after,
                        otherwise: next,
                    },
                    after,
                );
            }
            let yes = u8_constant(1, cx);
            this.insts.push(lir::Inst::Copy {
                dst: matched.clone(),
                src: yes,
                layout: lir::Layout::U8,
            });
            this.end_block(lir::Terminator::Jump(next), next);
        });
        matched
    }

    /// The value `conjunct`, of a condition on a relation's tuples, says
    /// the tuple at hand's attribute `name`, of type `attribute`, is equal
    /// to, if it says that, of a value of its type, that's the same for
    /// every tuple: it reads none of the tuple at hand.
    fn equated<'e>(
        &self,
        conjunct: &'e hir::Expr,
        name: &str,
        attribute: &hir::Type,
    ) -> Option<&'e hir::Expr> {
        conjunct
            .equated()
            .filter(|(equated, value)| *equated == name && self.type_of(value) == *attribute)
            .map(|(_, value)| value)
    }

    /// A new relation or array of type `ty` with room for `count` tuples or
    /// elements, a [`lir::Layout::Usize`], zeroed, which the runtime
    /// allocates for the statement.
    fn allocated(&mut self, ty: &hir::Type, count: lir::Place, cx: &mut Lowering) -> lir::Place {
        let descriptor = lir::Place::whole(lir::Base::Data(cx.descriptor(ty)));
        let dst = self.slot(cx.layout(ty));
        self.insts.push(lir::Inst::CallRuntime {
            function: lir::RuntimeFn::Allocate,
            args: vec![dst.clone(), count, descriptor],
        });
        // What fills it has to say what order its tuples are in.
        if let hir::Type::Relation(_) = ty {
            self.insts.push(lir::Inst::InAnyOrder(dst.clone()));
        }
        self.scoped = true;
        dst
    }

    /// Have the buffer of `filled`, a relation or array of type `ty` that
    /// compiled code filled, hold what its tuples or elements point to, if
    /// they point to anything: until now, only the statement holds that.
    fn adopt(&mut self, filled: &lir::Place, ty: &hir::Type, cx: &mut Lowering) {
        let element = match ty {
            hir::Type::Relation(heading) => hir::Type::Tuple(heading.clone()),
            hir::Type::Array(element) => (**element).clone(),
            _ => unreachable!("only relations and arrays are filled"),
        };
        if cx.points(&element) {
            let descriptor = lir::Place::whole(lir::Base::Data(cx.descriptor(ty)));
            self.insts.push(lir::Inst::CallRuntime {
                function: lir::RuntimeFn::Adopt,
                args: vec![filled.clone(), descriptor],
            });
        }
    }

    /// A loop over the tuples of the relation at `src`, each laid out as
    /// `tuple`: `body` is lowered once, with the place of the tuple at
    /// hand and of its index, and the loop goes on after it.
    fn each_tuple(
        &mut self,
        src: &lir::Place,
        tuple: lir::Layout,
        cx: &mut Lowering,
        body: impl FnOnce(&mut Self, lir::Place, &lir::Place, &mut Lowering),
    ) {
        let mut count = src.clone();
        count.path.push(1);
        let index = self.slot(lir::Layout::Usize);
        let zero = usize_constant(0, cx);
        self.insts.push(lir::Inst::Copy {
            dst: index.clone(),
            src: zero,
            layout: lir::Layout::Usize,
        });
        let (head, first, done) = (self.new_block(), self.new_block(), self.new_block());
        self.end_block(lir::Terminator::Jump(head), head);
        let more = self.slot(lir::Layout::U8);
        self.insts.push(lir::Inst::Binary {
            dst: more.clone(),
            op: lir::BinaryOp::Compare(lir::Comparison::Less, lir::Operand::Usize),
            lhs: index.clone(),
            rhs: count,
        });
        self.end_block(
            lir::Terminator::Branch {
                condition: more,
                then: first,
                otherwise: done,
            },
            first,
        );
        let at_hand = self.element(src, &index, tuple);
        body(self, at_hand, &index, cx);
        self.insts.push(lir::Inst::Increment { place: index });
        self.end_block(lir::Terminator::Jump(head), done);
    }

    /// `body` of each tuple of the relation at `src`, laid out as `tuple`,
    /// as the tuple at hand, with its index, in a loop (see
    /// [`FunctionBuilder::each_tuple`]): the relations it looks up in are
    /// computed once, with their indexes, at the first tuple that needs
    /// them, and given up as it ends (see [`FunctionBuilder::looked_up`]).
    fn each_at_hand(
        &mut self,
        src: &lir::Place,
        tuple: lir::Layout,
        cx: &mut Lowering,
        body: impl FnOnce(&mut Self, &lir::Place, &lir::Place, &mut Lowering),
    ) {
        let mut made = Vec::new();
        self.each_tuple(src, tuple, cx, |this, at_hand, index, cx| {
            this.tuples.push(AtHand {
                place: at_hand.clone(),
                lookups: Some(Lookups::default()),
            });
            body(this, &at_hand, index, cx);
            made = this
                .tuples
                .pop()
                .and_then(|at_hand| at_hand.lookups)
                .map_or_else(Vec::new, |lookups| lookups.made);
        });
        for lookup in made {
            self.release_place(&lookup.kept, &lookup.ty, cx);
            self.release_place(&lookup.index, &index_type(), cx);
        }
    }

    /// `body` of each tuple of the relation at `src`, of `tuple`, which come as `comes`
    /// says, as [`FunctionBuilder::each_at_hand`] gives them: if `body` can fail, as `fails`
    /// says, and they aren't in canonical order, as
    /// [`FunctionBuilder::each_at_hand_failing_first`] does, so that the failure is the same
    /// whatever order they come in.
    fn each_at_hand_in(
        &mut self,
        src: &lir::Place,
        tuple: &hir::Type,
        comes: Order,
        fails: bool,
        cx: &mut Lowering,
        body: impl FnMut(&mut Self, &lir::Place, &lir::Place, &mut Lowering),
    ) {
        if fails && comes != Order::Canonical {
            self.each_at_hand_failing_first(src, tuple, cx, body);
        } else {
            let layout = cx.layout(tuple);
            self.each_at_hand(src, layout, cx, body);
        }
    }

    /// `body` of each tuple of the relation at `src`, as [`FunctionBuilder::each_at_hand`]
    /// gives it, where its tuples, of `tuple`, come in an order other than the canonical one
    /// and `body` can fail. The failure is the first tuple's in canonical order to fail, as
    /// it is whatever order they come in (see `language.md`'s "Evaluation"), found without
    /// sorting them: a tuple's failure is put aside ([`FunctionBuilder::put_aside`]), keeping
    /// where the first in canonical order to fail so far is, and the loop goes on. Then that
    /// tuple's `body` is computed again, from a copy of it of its own, which fails as it did,
    /// as what is computed for a tuple is a function of it (TTM RM Prescription 3), and that
    /// failure goes on. A copy, rather than going back into the loop, keeps what only a
    /// failure needs out of the loop: going back would make a loop of its own, out of which
    /// what the handler computes would be computed for every tuple. If none fails, nothing is
    /// done but the loop.
    fn each_at_hand_failing_first(
        &mut self,
        src: &lir::Place,
        tuple: &hir::Type,
        cx: &mut Lowering,
        mut body: impl FnMut(&mut Self, &lir::Place, &lir::Place, &mut Lowering),
    ) {
        let layout = cx.layout(tuple);
        let failing = self.first_failing(true, true, cx);
        let bound = self.lets.len();
        let handler = self.new_block();
        let mut looped = None;
        self.each_at_hand(src, layout.clone(), cx, |this, at_hand, index, cx| {
            this.catching(handler, |this| body(this, at_hand, index, cx));
            let next = this.new_block();
            this.end_block(lir::Terminator::Jump(next), next);
            looped = Some(Looped {
                next,
                at_hand: at_hand.clone(),
                index: index.clone(),
                bound: bound..this.lets.len(),
            });
        });
        let looped = looped.expect("the loop has a body");
        let failed = self.slot(lir::Layout::U8);
        self.insts.push(lir::Inst::Binary {
            dst: failed.clone(),
            op: lir::BinaryOp::Compare(lir::Comparison::NotEqual, lir::Operand::Usize),
            lhs: failing.first.clone(),
            rhs: failing.none.clone(),
        });
        let (once_more, done) = (self.new_block(), self.new_block());
        self.end_block(
            lir::Terminator::Branch {
                condition: failed,
                then: once_more,
                otherwise: done,
            },
            once_more,
        );
        // The first to fail, at hand again.
        let at_hand = self.element(src, &failing.first, layout);
        self.tuples.push(AtHand {
            place: at_hand.clone(),
            lookups: Some(Lookups::default()),
        });
        body(self, &at_hand, &failing.first, cx);
        self.tuples.pop();
        self.insts.push(lir::Inst::CallRuntime {
            function: lir::RuntimeFn::Unfailed,
            args: Vec::new(),
        });
        self.end_block(lir::Terminator::Jump(done), handler);
        self.put_aside(&failing, &looped, src, tuple, done, cx);
    }

    /// The slots of [`FirstFailing`], set as what puts failures aside begins: none has
    /// failed. The scope innermost then is kept if what it computes `nests` scopes in it,
    /// and how many calls the trace has, and the site, if it `calls` functions.
    fn first_failing(&mut self, nests: bool, calls: bool, cx: &mut Lowering) -> FirstFailing {
        let none = usize_constant(u64::MAX, cx);
        let first = self.slot(lir::Layout::Usize);
        self.insts.push(lir::Inst::Copy {
            dst: first.clone(),
            src: none.clone(),
            layout: lir::Layout::Usize,
        });
        let scope = nests.then(|| {
            let scope = self.slot(lir::Layout::Address);
            self.insts.push(lir::Inst::CallRuntime {
                function: lir::RuntimeFn::Innermost,
                args: vec![scope.clone()],
            });
            scope
        });
        let traced = calls.then(|| {
            let (traced, site) = (
                self.slot(lir::Layout::Usize),
                self.slot(lir::Layout::Address),
            );
            self.insts.push(lir::Inst::CallRuntime {
                function: lir::RuntimeFn::Traced,
                args: vec![traced.clone()],
            });
            self.insts.push(lir::Inst::SaveSite { dst: site.clone() });
            (traced, site)
        });
        FirstFailing {
            first,
            none,
            scope,
            traced,
        }
    }

    /// Put aside the failure that brought the program here, of what began as `failing`
    /// says, which made the bindings `bound` among [`FunctionBuilder::lets`]: what its
    /// scopes and bindings hold is given up (a lookup's index then null, so it is made
    /// again), unless it has no error, as running out of memory has none, when it goes
    /// on to `raise`. Then the calls it passed through are forgotten, the site is
    /// restored, and its error is given up.
    fn forget_failure(
        &mut self,
        failing: &FirstFailing,
        bound: std::ops::Range<usize>,
        raise: lir::BlockId,
        cx: &mut Lowering,
    ) {
        if let Some(scope) = &failing.scope {
            self.insts.push(lir::Inst::CallRuntime {
                function: lir::RuntimeFn::EndSince,
                args: vec![scope.clone()],
            });
        }
        for i in bound {
            let (slot, ty) = self.lets[i].clone();
            self.release_place(&slot, &ty, cx);
        }
        // An error waits if its type does, which is null where none does, as giving one up
        // makes it: so this needs none of the types it could be, each a constant whose
        // address would otherwise be computed where every handler's paths meet.
        let waiting = cx.waiting(self.program);
        let error = waiting.error();
        let mut own_type = error.clone();
        own_type.path.push(0);
        let none = self.slot(lir::Layout::U8);
        self.insts.push(lir::Inst::IsNull {
            dst: none.clone(),
            src: own_type,
        });
        let take = self.new_block();
        self.end_block(
            lir::Terminator::Branch {
                condition: none,
                then: raise,
                otherwise: take,
            },
            take,
        );
        if let Some((traced, site)) = &failing.traced {
            self.insts.push(lir::Inst::CallRuntime {
                function: lir::RuntimeFn::Untrace,
                args: vec![traced.clone()],
            });
            self.insts
                .push(lir::Inst::RestoreSite { src: site.clone() });
        }
        self.release_place(&error, &waiting.ty, cx);
    }

    /// As [`FunctionBuilder::steps_put_aside`], and what comes after them can compute the
    /// first to fail again ([`FunctionBuilder::fail_again`]), from a copy of `compute` of its
    /// own, which fails as it did, as what it computes is a function of what it reads (TTM RM
    /// Prescription 3). A copy, rather than going back to the step, keeps what only a failure
    /// needs out of the steps' code: going back would make a loop of it, out of which what a
    /// failure needs would be computed each time the steps are.
    fn steps_failing_again(
        &mut self,
        count: usize,
        calls: bool,
        cx: &mut Lowering,
        mut compute: impl FnMut(&mut Self, usize, &mut Lowering) -> lir::Place,
        after: impl FnMut(&mut Self, usize, lir::Place, &mut Lowering),
    ) -> Steps {
        let failing = self.steps_put_aside(count, calls, cx, &mut compute, after);
        let again = self.copies_to_fail_again(count, cx, compute);
        Steps { failing, again }
    }

    /// `count` steps, one after another, and what comes after them, where this leaves
    /// building: the `i`th computed by `compute`, which can fail, and then, if it didn't,
    /// what `after` does with what it computed, which can go elsewhere, as when it decides
    /// what happens. A step's failure is put aside ([`FunctionBuilder::forget_failure`]),
    /// keeping which was the first to fail, and the next is computed, as it may yet decide
    /// what happens. What comes after asks whether one failed
    /// ([`FunctionBuilder::steps_failed`]). The trace and site are kept if what the steps
    /// compute `calls` functions, which change them.
    fn steps_put_aside(
        &mut self,
        count: usize,
        calls: bool,
        cx: &mut Lowering,
        mut compute: impl FnMut(&mut Self, usize, &mut Lowering) -> lir::Place,
        mut after: impl FnMut(&mut Self, usize, lir::Place, &mut Lowering),
    ) -> FirstFailing {
        let start = self.position();
        let mut failing = self.first_failing(false, calls, cx);
        let raise = self.new_block();
        let mut lowered = Vec::new();
        for i in 0..count {
            let handler = self.new_block();
            let bound = self.lets.len();
            let mut value = None;
            self.catching(handler, |this| value = Some(compute(this, i, cx)));
            let value = value.expect("a step computes a value");
            after(self, i, value, cx);
            let next = self.new_block();
            self.end_block(lir::Terminator::Jump(next), next);
            lowered.push((handler, next, bound..self.lets.len()));
        }
        // What its steps begin, ended by a failure in one.
        if self.begins_scopes_since(start) {
            let scope = self.slot(lir::Layout::Address);
            self.insert(
                start,
                lir::Inst::CallRuntime {
                    function: lir::RuntimeFn::Innermost,
                    args: vec![scope.clone()],
                },
            );
            failing.scope = Some(scope);
        }
        let done = self.new_block();
        for (i, (handler, next, bound)) in lowered.into_iter().enumerate() {
            self.end_block(lir::Terminator::Jump(done), handler);
            self.forget_failure(&failing, bound, raise, cx);
            let first = self.slot(lir::Layout::U8);
            self.insts.push(lir::Inst::Binary {
                dst: first.clone(),
                op: lir::BinaryOp::Compare(lir::Comparison::Equal, lir::Operand::Usize),
                lhs: failing.first.clone(),
                rhs: failing.none.clone(),
            });
            let kept = self.new_block();
            self.end_block(
                lir::Terminator::Branch {
                    condition: first,
                    then: kept,
                    otherwise: next,
                },
                kept,
            );
            let index = usize_constant(len(i), cx);
            self.insts.push(lir::Inst::Copy {
                dst: failing.first.clone(),
                src: index,
                layout: lir::Layout::Usize,
            });
            self.end_block(lir::Terminator::Jump(next), raise);
        }
        self.end_block(lir::Terminator::Raise, done);
        failing
    }

    /// Where each of `count` steps, the `i`th computed by `compute`, is computed again, from a
    /// copy of its own, to fail as it did, its failure going on, reached only from
    /// [`FunctionBuilder::fail_again`]; building goes on after them.
    fn copies_to_fail_again(
        &mut self,
        count: usize,
        cx: &mut Lowering,
        mut compute: impl FnMut(&mut Self, usize, &mut Lowering) -> lir::Place,
    ) -> Vec<lir::BlockId> {
        let mut again = Vec::new();
        for i in 0..count {
            let copy = self.new_block();
            let after_copies = self.new_block();
            self.end_block(lir::Terminator::Jump(after_copies), copy);
            compute(self, i, cx);
            self.insts.push(lir::Inst::CallRuntime {
                function: lir::RuntimeFn::Unfailed,
                args: Vec::new(),
            });
            self.end_block(lir::Terminator::Jump(after_copies), after_copies);
            again.push(copy);
        }
        again
    }

    /// The ranking of `condition`, a restriction's, as its loop begins, if it is worth it: an
    /// `and` or an `or` of at least two operands that can fail, one of which calls a
    /// function, for what is cheap costs less to compute than to rank. Each is computed
    /// first in the place it is written, until it is ranked, with its cost estimated from
    /// how much it computes ([`estimated_cost`]). What it learns is kept for the next time
    /// the restriction runs, in a variable of the program's own.
    fn ranking_for(&mut self, condition: &hir::Expr, cx: &mut Lowering) -> Option<Ranking> {
        let hir::Expr::Connective(hir::Connective::And | hir::Connective::Or, operands) = condition
        else {
            return None;
        };
        let failing: Vec<&hir::Expr> = operands.iter().filter(|o| o.can_fail()).collect();
        if failing.len() < 2 || !failing.iter().any(|operand| makes_calls(operand)) {
            return None;
        }
        let count = failing.len();
        // As written, none computed yet, then the costs, then the tuples so far and when
        // it is ranked first.
        let fields: Vec<lir::Field> = (0..count)
            .map(len)
            .chain(std::iter::repeat_n(0, 2 * count))
            .chain(failing.iter().map(|operand| estimated_cost(operand)))
            .chain([0, 16])
            .map(lir::Field::Usize)
            .collect();
        let name = format!("__ranking_{}", cx.data.len());
        let state = cx.push(lir::Data {
            kind: lir::DataKind::Variable { name },
            writable: true,
            fields,
        });
        let ranking = Ranking {
            count,
            state,
            order: self.slot(lir::Layout::Struct(vec![lir::Layout::Usize; count])),
        };
        self.order_as_ranked(&ranking);
        Some(ranking)
    }

    /// Copy the order `ranking` has its operands in into the slot this run of its
    /// restriction computes them in.
    fn order_as_ranked(&mut self, ranking: &Ranking) {
        for at in 0..ranking.count {
            let mut place = ranking.order.clone();
            place.path.push(at);
            self.insts.push(lir::Inst::Copy {
                dst: place,
                src: ranking.field(at),
                layout: lir::Layout::Usize,
            });
        }
    }

    /// `condition`, an `and` or an `or` as [`FunctionBuilder::ranking_for`] ranks, computed for
    /// the tuple at hand as [`FunctionBuilder::deciding_connective`] computes one, but its
    /// operands that can fail in the order `ranking` has them in, each only until one decides
    /// it, counting how many times each is computed and decides it. Once enough tuples have
    /// been, the runtime orders them again (`trendy_rt_rank`). A failure is put aside as a
    /// step's is, keeping the first operand written to fail, as the order they are computed
    /// in changes nothing: if none decides the result, that one is computed again, from a
    /// copy, and fails as it did. The result is in a new slot.
    fn ranked_connective(
        &mut self,
        condition: &hir::Expr,
        ranking: &Ranking,
        cx: &mut Lowering,
    ) -> lir::Place {
        let hir::Expr::Connective(connective, operands) = condition else {
            unreachable!("only a connective is ranked");
        };
        let result = self.slot(scalar_layout(hir::Scalar::Boolean));
        let (settled, done) = (self.new_block(), self.new_block());
        let decides = !connective.identity();
        let (failing, safe): (Vec<&hir::Expr>, Vec<&hir::Expr>) =
            operands.iter().partition(|operand| operand.can_fail());
        for operand in safe {
            let value = self.place(operand, cx);
            self.decide_or_on(value, decides, settled);
        }
        self.rank_if_due(ranking, cx);
        let calls = failing.iter().any(|operand| makes_calls(operand));
        let start = self.position();
        let mut failed = self.first_failing(false, calls, cx);
        let place = self.slot(lir::Layout::Usize);
        let zero = usize_constant(0, cx);
        self.insts.push(lir::Inst::Copy {
            dst: place.clone(),
            src: zero,
            layout: lir::Layout::Usize,
        });
        let dispatch = self.new_block();
        self.end_block(lir::Terminator::Jump(dispatch), dispatch);
        let (operand_blocks, after_all) = self.dispatch_ranked(ranking, &place, cx);
        let raise = self.new_block();
        let mut handlers = Vec::new();
        for (i, operand) in failing.iter().enumerate() {
            // What comes before goes on to the next place in the order.
            self.end_block(lir::Terminator::Jump(dispatch), operand_blocks[i]);
            self.insts.push(lir::Inst::Increment {
                place: ranking.field(ranking.count + i),
            });
            let handler = self.new_block();
            let bound = self.lets.len();
            let mut value = None;
            self.catching(handler, |this| value = Some(this.place(operand, cx)));
            let value = value.expect("an operand has a value");
            let (counted, next) = (self.new_block(), self.new_block());
            let (then, otherwise) = if decides {
                (counted, next)
            } else {
                (next, counted)
            };
            self.end_block(
                lir::Terminator::Branch {
                    condition: value,
                    then,
                    otherwise,
                },
                counted,
            );
            self.insts.push(lir::Inst::Increment {
                place: ranking.field(2 * ranking.count + i),
            });
            self.end_block(lir::Terminator::Jump(settled), next);
            self.insts.push(lir::Inst::Increment {
                place: place.clone(),
            });
            handlers.push((i, handler, bound..self.lets.len()));
        }
        if self.begins_scopes_since(start) {
            let scope = self.slot(lir::Layout::Address);
            self.insert(
                start,
                lir::Inst::CallRuntime {
                    function: lir::RuntimeFn::Innermost,
                    args: vec![scope.clone()],
                },
            );
            failed.scope = Some(scope);
        }
        self.ranked_failures(&failed, handlers, (dispatch, &place), raise, cx);
        let undecided = self.new_block();
        self.end_block(lir::Terminator::Raise, after_all);
        let again =
            self.copies_to_fail_again(failing.len(), cx, |this, i, cx| this.place(failing[i], cx));
        let steps = Steps {
            failing: failed,
            again,
        };
        let one_failed = self.steps_failed(&steps.failing);
        let once_more = self.new_block();
        self.end_block(
            lir::Terminator::Branch {
                condition: one_failed,
                then: once_more,
                otherwise: undecided,
            },
            once_more,
        );
        self.fail_again(&steps, cx);
        self.settle_connective(&result, *connective, [undecided, settled], done, cx);
        result
    }

    /// The handlers of the failures of a ranked connective's operands, each with the
    /// operand's place as written and the bindings it made, among
    /// [`FunctionBuilder::lets`]: each puts its failure aside
    /// ([`FunctionBuilder::forget_failure`]), keeps the operand if it was written before
    /// the first to fail so far, whichever order they are computed in, counts its place
    /// as done, at `cursor`, and goes back to `dispatch` (`cursor`'s dispatch), building going
    /// on at `raise`.
    fn ranked_failures(
        &mut self,
        failed: &FirstFailing,
        handlers: Vec<(usize, lir::BlockId, std::ops::Range<usize>)>,
        (dispatch, cursor): (lir::BlockId, &lir::Place),
        raise: lir::BlockId,
        cx: &mut Lowering,
    ) {
        for (i, handler, bound) in handlers {
            self.end_block(lir::Terminator::Jump(dispatch), handler);
            self.forget_failure(failed, bound, raise, cx);
            let index = usize_constant(len(i), cx);
            let earlier = self.slot(lir::Layout::U8);
            self.insts.push(lir::Inst::Binary {
                dst: earlier.clone(),
                op: lir::BinaryOp::Compare(lir::Comparison::Greater, lir::Operand::Usize),
                lhs: failed.first.clone(),
                rhs: index.clone(),
            });
            let (kept, on) = (self.new_block(), self.new_block());
            self.end_block(
                lir::Terminator::Branch {
                    condition: earlier,
                    then: kept,
                    otherwise: on,
                },
                kept,
            );
            self.insts.push(lir::Inst::Copy {
                dst: failed.first.clone(),
                src: index,
                layout: lir::Layout::Usize,
            });
            self.end_block(lir::Terminator::Jump(on), on);
            self.insts.push(lir::Inst::Increment {
                place: cursor.clone(),
            });
        }
        self.end_block(lir::Terminator::Jump(dispatch), raise);
    }

    /// The end of a connective's computation, from a block nothing reaches: in `undecided`
    /// its result, at `result`, is the connective's identity, and in `settled` the value
    /// that decides it, and both go on to `done`, where building goes on.
    fn settle_connective(
        &mut self,
        result: &lir::Place,
        connective: hir::Connective,
        [undecided, settled]: [lir::BlockId; 2],
        done: lir::BlockId,
        cx: &mut Lowering,
    ) {
        self.end_block(lir::Terminator::Jump(done), undecided);
        let identity = u8_constant(u8::from(connective.identity()), cx);
        self.insts.push(lir::Inst::Copy {
            dst: result.clone(),
            src: identity,
            layout: lir::Layout::U8,
        });
        self.end_block(lir::Terminator::Jump(done), settled);
        let decides = u8_constant(u8::from(!connective.identity()), cx);
        self.insts.push(lir::Inst::Copy {
            dst: result.clone(),
            src: decides,
            layout: lir::Layout::U8,
        });
        self.end_block(lir::Terminator::Jump(done), done);
    }

    /// To `settled` if `value` is `decides`, which decides a connective, and else on.
    fn decide_or_on(&mut self, value: lir::Place, decides: bool, settled: lir::BlockId) {
        let next = self.new_block();
        let (then, otherwise) = if decides {
            (settled, next)
        } else {
            (next, settled)
        };
        self.end_block(
            lir::Terminator::Branch {
                condition: value,
                then,
                otherwise,
            },
            next,
        );
    }

    /// Count a tuple `ranking`'s condition is computed for, and if it is the one it is due at,
    /// have the runtime order its operands again, which this run computes them in from then
    /// on, and next at twice as many tuples, or at 4096 more once that is fewer, so that a
    /// program that runs long goes on learning.
    fn rank_if_due(&mut self, ranking: &Ranking, cx: &mut Lowering) {
        let (seen, next) = (
            ranking.field(4 * ranking.count),
            ranking.field(4 * ranking.count + 1),
        );
        self.insts.push(lir::Inst::Increment {
            place: seen.clone(),
        });
        let due = self.slot(lir::Layout::U8);
        self.insts.push(lir::Inst::Binary {
            dst: due.clone(),
            op: lir::BinaryOp::Compare(lir::Comparison::Equal, lir::Operand::Usize),
            lhs: seen,
            rhs: next.clone(),
        });
        let (rank, on) = (self.new_block(), self.new_block());
        self.end_block(
            lir::Terminator::Branch {
                condition: due,
                then: rank,
                otherwise: on,
            },
            rank,
        );
        let count = usize_constant(len(ranking.count), cx);
        self.insts.push(lir::Inst::CallRuntime {
            function: lir::RuntimeFn::Rank,
            args: vec![lir::Place::whole(lir::Base::Data(ranking.state)), count],
        });
        self.order_as_ranked(ranking);
        let most = usize_constant(4096, cx);
        let few = self.slot(lir::Layout::U8);
        self.insts.push(lir::Inst::Binary {
            dst: few.clone(),
            op: lir::BinaryOp::Compare(lir::Comparison::Less, lir::Operand::Usize),
            lhs: next.clone(),
            rhs: most.clone(),
        });
        let (doubled, steady) = (self.new_block(), self.new_block());
        self.end_block(
            lir::Terminator::Branch {
                condition: few,
                then: doubled,
                otherwise: steady,
            },
            doubled,
        );
        self.insts.push(lir::Inst::Binary {
            dst: next.clone(),
            op: lir::BinaryOp::Add(lir::Numeric::Integer),
            lhs: next.clone(),
            rhs: next.clone(),
        });
        self.end_block(lir::Terminator::Jump(on), steady);
        self.insts.push(lir::Inst::Binary {
            dst: next.clone(),
            op: lir::BinaryOp::Add(lir::Numeric::Integer),
            lhs: next,
            rhs: most,
        });
        self.end_block(lir::Terminator::Jump(on), on);
    }

    /// Where the operand computed in the place at `place` of `ranking`'s order begins, once
    /// each operand's block is built, at the blocks given for each, as written, and where
    /// the dispatch goes once every place has been: those blocks, and that one. The
    /// dispatch is built here, in the block it begins in.
    fn dispatch_ranked(
        &mut self,
        ranking: &Ranking,
        place: &lir::Place,
        cx: &mut Lowering,
    ) -> (Vec<lir::BlockId>, lir::BlockId) {
        let operands: Vec<lir::BlockId> = (0..ranking.count).map(|_| self.new_block()).collect();
        let after_all = self.new_block();
        let operand = self.slot(lir::Layout::Usize);
        let chosen = self.new_block();
        for at in 0..ranking.count {
            let is = self.slot(lir::Layout::U8);
            let index = usize_constant(len(at), cx);
            self.insts.push(lir::Inst::Binary {
                dst: is.clone(),
                op: lir::BinaryOp::Compare(lir::Comparison::Equal, lir::Operand::Usize),
                lhs: place.clone(),
                rhs: index,
            });
            let (this_one, next) = (self.new_block(), self.new_block());
            self.end_block(
                lir::Terminator::Branch {
                    condition: is,
                    then: this_one,
                    otherwise: next,
                },
                this_one,
            );
            let mut place = ranking.order.clone();
            place.path.push(at);
            self.insts.push(lir::Inst::Copy {
                dst: operand.clone(),
                src: place,
                layout: lir::Layout::Usize,
            });
            self.end_block(lir::Terminator::Jump(chosen), next);
        }
        // Past the last place, every operand has been computed.
        self.end_block(lir::Terminator::Jump(after_all), chosen);
        let (last, before) = operands.split_last().expect("there are operands");
        for (i, block) in before.iter().enumerate() {
            let is = self.slot(lir::Layout::U8);
            let index = usize_constant(len(i), cx);
            self.insts.push(lir::Inst::Binary {
                dst: is.clone(),
                op: lir::BinaryOp::Compare(lir::Comparison::Equal, lir::Operand::Usize),
                lhs: operand.clone(),
                rhs: index,
            });
            let next = self.new_block();
            self.end_block(
                lir::Terminator::Branch {
                    condition: is,
                    then: *block,
                    otherwise: next,
                },
                next,
            );
        }
        let unreached = self.new_block();
        self.end_block(lir::Terminator::Jump(*last), unreached);
        (operands, after_all)
    }

    /// Whether one of the steps that keep what they know in `failing` failed, in a new slot.
    fn steps_failed(&mut self, failing: &FirstFailing) -> lir::Place {
        let failed = self.slot(lir::Layout::U8);
        self.insts.push(lir::Inst::Binary {
            dst: failed.clone(),
            op: lir::BinaryOp::Compare(lir::Comparison::NotEqual, lir::Operand::Usize),
            lhs: failing.first.clone(),
            rhs: failing.none.clone(),
        });
        failed
    }

    /// Compute again the first of `steps` to fail, which one did, from its copy, to fail
    /// as it did, its failure going on. Building goes on in a block nothing reaches.
    fn fail_again(&mut self, steps: &Steps, cx: &mut Lowering) {
        let (last, before) = steps.again.split_last().expect("there are steps");
        for (i, copy) in before.iter().enumerate() {
            let is = self.slot(lir::Layout::U8);
            let index = usize_constant(len(i), cx);
            self.insts.push(lir::Inst::Binary {
                dst: is.clone(),
                op: lir::BinaryOp::Compare(lir::Comparison::Equal, lir::Operand::Usize),
                lhs: steps.failing.first.clone(),
                rhs: index,
            });
            let next = self.new_block();
            self.end_block(
                lir::Terminator::Branch {
                    condition: is,
                    then: *copy,
                    otherwise: next,
                },
                next,
            );
        }
        let unreached = self.new_block();
        self.end_block(lir::Terminator::Jump(*last), unreached);
    }

    /// Whether what has been built since `start`, a position, begins a scope.
    fn begins_scopes_since(&self, start: (lir::BlockId, usize)) -> bool {
        let begins = |inst: &lir::Inst| {
            matches!(
                inst,
                lir::Inst::CallRuntime {
                    function: lir::RuntimeFn::Begin,
                    ..
                }
            )
        };
        let (block, index) = start;
        let in_start = if block == self.current {
            self.insts[index..].iter().any(begins)
        } else {
            self.blocks[block.0].insts[index..].iter().any(begins)
        };
        in_start
            || self.blocks[block.0 + 1..]
                .iter()
                .any(|b| b.insts.iter().any(begins))
            || (block != self.current && self.insts.iter().any(begins))
    }

    /// The handler of a tuple's failure in the loop of
    /// [`FunctionBuilder::each_at_hand_failing_first`], ending in `done`: the failure is
    /// put aside ([`FunctionBuilder::forget_failure`]), and where the tuple is is kept if it
    /// comes before the first to fail so far in canonical order, by `trendy_rt_compare`.
    /// Then the loop goes on.
    fn put_aside(
        &mut self,
        failing: &FirstFailing,
        looped: &Looped,
        src: &lir::Place,
        tuple: &hir::Type,
        done: lir::BlockId,
        cx: &mut Lowering,
    ) {
        let raise = self.new_block();
        self.forget_failure(failing, looped.bound.clone(), raise, cx);
        let first = self.slot(lir::Layout::U8);
        self.insts.push(lir::Inst::Binary {
            dst: first.clone(),
            op: lir::BinaryOp::Compare(lir::Comparison::Equal, lir::Operand::Usize),
            lhs: failing.first.clone(),
            rhs: failing.none.clone(),
        });
        let (compare, keep) = (self.new_block(), self.new_block());
        self.end_block(
            lir::Terminator::Branch {
                condition: first,
                then: keep,
                otherwise: compare,
            },
            compare,
        );
        let kept = self.element(src, &failing.first, cx.layout(tuple));
        let order = self.slot(lir::Layout::I64);
        let descriptor = lir::Place::whole(lir::Base::Data(cx.descriptor(tuple)));
        self.insts.push(lir::Inst::CallRuntime {
            function: lir::RuntimeFn::Compare,
            args: vec![order.clone(), looped.at_hand.clone(), kept, descriptor],
        });
        let zero = self.place(&hir::Expr::Constant(hir::Value::Integer(0)), cx);
        let before = self.slot(lir::Layout::U8);
        self.insts.push(lir::Inst::Binary {
            dst: before.clone(),
            op: lir::BinaryOp::Compare(
                lir::Comparison::Less,
                lir::Operand::Numeric(lir::Numeric::Integer),
            ),
            lhs: order,
            rhs: zero,
        });
        self.end_block(
            lir::Terminator::Branch {
                condition: before,
                then: keep,
                otherwise: looped.next,
            },
            keep,
        );
        self.insts.push(lir::Inst::Copy {
            dst: failing.first.clone(),
            src: looped.index.clone(),
            layout: lir::Layout::Usize,
        });
        self.end_block(lir::Terminator::Jump(looped.next), raise);
        self.end_block(lir::Terminator::Raise, done);
    }

    /// The element at `index`, from 0, of `array`, a relation's tuples or
    /// an array's elements, laid out as `element`: its address in a new
    /// slot, as a place.
    fn element(
        &mut self,
        array: &lir::Place,
        index: &lir::Place,
        element: lir::Layout,
    ) -> lir::Place {
        let slot = self.pointer_slot();
        self.insts.push(lir::Inst::Element {
            dst: lir::Place::whole(lir::Base::Slot(slot)),
            array: array.clone(),
            index: index.clone(),
            element: element.clone(),
        });
        lir::Place::whole(lir::Base::Indirect {
            slot,
            pointee: element,
        })
    }

    /// The value of the attribute `name` of `tuple`: its own place within
    /// the tuple, with no copy.
    fn extracted(&mut self, tuple: &hir::Expr, name: &str, cx: &mut Lowering) -> lir::Place {
        let ty = self.type_of(tuple);
        let index = attribute_names(&ty)
            .position(|a| a == name)
            .expect("sema extracts only attributes the tuple has");
        let mut place = self.place(tuple, cx);
        place.path.push(index);
        place
    }

    /// `expr`, a treat or a type test: a value of a type in a hierarchy is
    /// laid out as one of each of its supertypes, so treating one up is
    /// what it is, and treating one down tests its type first.
    fn typed(&mut self, expr: &hir::Expr, cx: &mut Lowering) -> lir::Place {
        match expr {
            hir::Expr::Treat(_, operand) => self.place(operand, cx),
            hir::Expr::TreatAs(ty, operand) => self.treated_as(ty, operand, cx),
            hir::Expr::Is(operand, ty) => {
                let value = self.place(operand, cx);
                self.is_of(&value, ty, cx)
            }
            _ => unreachable!("only treats and tests are typed"),
        }
    }

    /// Whether the value at `value`, of a type in a hierarchy, is of `ty`:
    /// whether its own type's descriptor is one of the types' under `ty`
    /// with components, or `ty`'s.
    fn is_of(&mut self, value: &lir::Place, ty: &hir::Type, cx: &mut Lowering) -> lir::Place {
        let is = self.slot(lir::Layout::U8);
        let no = u8_constant(0, cx);
        self.insts.push(lir::Inst::Copy {
            dst: is.clone(),
            src: no,
            layout: lir::Layout::U8,
        });
        let mut own = value.clone();
        own.path.push(0);
        let done = self.new_block();
        for leaf in cx.families.members_under(ty) {
            let descriptor = cx.components_descriptor(&leaf);
            let address = self.slot(lir::Layout::Address);
            self.insts.push(lir::Inst::Address {
                dst: address.clone(),
                src: lir::Place::whole(lir::Base::Data(descriptor)),
            });
            let same = self.slot(lir::Layout::U8);
            self.insts.push(lir::Inst::Binary {
                dst: same.clone(),
                op: lir::BinaryOp::Compare(lir::Comparison::Equal, lir::Operand::Usize),
                lhs: own.clone(),
                rhs: address,
            });
            let (yes, next) = (self.new_block(), self.new_block());
            self.end_block(
                lir::Terminator::Branch {
                    condition: same,
                    then: yes,
                    otherwise: next,
                },
                yes,
            );
            let one = u8_constant(1, cx);
            self.insts.push(lir::Inst::Copy {
                dst: is.clone(),
                src: one,
                layout: lir::Layout::U8,
            });
            self.end_block(lir::Terminator::Jump(done), next);
        }
        self.end_block(lir::Terminator::Jump(done), done);
        is
    }

    /// The value `operand`, of a type in a hierarchy, as a value of `ty`, a
    /// subtype of its type: the program stops unless it is one (R0019).
    fn treated_as(&mut self, ty: &hir::Type, operand: &hir::Expr, cx: &mut Lowering) -> lir::Place {
        let value = self.place(operand, cx);
        let is = self.is_of(&value, ty, cx);
        let (kept, failed) = (self.new_block(), self.new_block());
        self.end_block(
            lir::Terminator::Branch {
                condition: is,
                then: kept,
                otherwise: failed,
            },
            failed,
        );
        let descriptor = lir::Place::whole(lir::Base::Data(cx.descriptor(&self.type_of(operand))));
        let name = self.text(&ty.to_string(), cx);
        self.fail(
            lir::Failure::NotOfType {
                value: value.clone(),
                descriptor,
                ty: name,
            },
            kept,
        );
        value
    }

    /// The components of the value `operand`, of a type of the program's
    /// own, as a tuple: all of it, or, in a hierarchy, after its type.
    fn components(&mut self, operand: &hir::Expr, cx: &mut Lowering) -> lir::Place {
        let mut components = self.place(operand, cx);
        if let hir::Type::Defined(defined) = self.type_of(operand) {
            components
                .path
                .extend(cx.families.components_path(&defined));
            return self.widened(&defined, components, cx);
        }
        components
    }

    /// The components of a value of `defined`, at `value`, laid out as the tuple of
    /// them: where they are, unless its constraints let some be stored narrower (see
    /// [`Families::stored`]), when a new tuple of them.
    fn widened(
        &mut self,
        defined: &hir::Defined,
        value: lir::Place,
        cx: &mut Lowering,
    ) -> lir::Place {
        let Some(stored) = cx.families.stored(defined) else {
            return value;
        };
        let wide = self.slot(cx.layout(&hir::Type::Tuple(defined.components.clone())));
        self.stored_copied(&stored, &value, &wide, true, cx);
        wide
    }

    /// The value of `defined` whose components, laid out as the tuple of them, are at
    /// `components`: where they are, unless its constraints let some be stored narrower,
    /// when in a new place, as they are known to be in range once those are checked.
    fn narrowed(
        &mut self,
        defined: &hir::Defined,
        components: lir::Place,
        cx: &mut Lowering,
    ) -> lir::Place {
        let Some(stored) = cx.families.stored(defined) else {
            return components;
        };
        let value = self.slot(cx.layout(&hir::Type::Tuple(stored.clone())));
        self.stored_copied(&stored, &components, &value, false, cx);
        value
    }

    /// Copy each component of `stored` from `from` to `to`, one laid out as `stored`
    /// and the other as the tuple of them, as `widening` says which, each narrower one
    /// converted.
    fn stored_copied(
        &mut self,
        stored: &hir::Heading,
        from: &lir::Place,
        to: &lir::Place,
        widening: bool,
        cx: &mut Lowering,
    ) {
        for (i, (_, ty)) in stored.attributes().enumerate() {
            let (mut src, mut dst) = (from.clone(), to.clone());
            src.path.push(i);
            dst.path.push(i);
            let inst = match ty {
                hir::Type::Scalar(hir::Scalar::Narrow(width)) if widening => lir::Inst::Widen {
                    dst,
                    src,
                    width: lir_width(*width),
                },
                hir::Type::Scalar(hir::Scalar::Narrow(width)) => lir::Inst::Narrow {
                    dst,
                    src,
                    width: lir_width(*width),
                },
                ty => lir::Inst::Copy {
                    dst,
                    src,
                    layout: cx.layout(ty),
                },
            };
            self.insts.push(inst);
        }
    }

    /// The value `selection`, a `Select` or a `SelectIn`, selects.
    fn selection(&mut self, selection: &hir::Expr, cx: &mut Lowering) -> lir::Place {
        match selection {
            hir::Expr::Select(defined, components) => self.selected(defined, components, cx),
            hir::Expr::SelectIn(defined, at, components) => {
                self.selected_in(defined, *at, components, cx)
            }
            other => unreachable!("{other:?} selects nothing"),
        }
    }

    /// The value of the type of the program's own `defined` selected from
    /// `components`, once each of its constraints is true of them (see
    /// [`FunctionBuilder::constrained`]): laid out as the tuple of them, or,
    /// in a hierarchy, after its own type's descriptor's address.
    fn selected(
        &mut self,
        defined: &hir::Defined,
        components: &hir::Expr,
        cx: &mut Lowering,
    ) -> lir::Place {
        let value = self.place(components, cx);
        self.selected_from(defined, value, cx)
    }

    /// The value of the type of the program's own `defined` whose
    /// components in its possible representation at `at` are `components`,
    /// selected in it: once each of the representation's constraints is
    /// true of them, as given (see [`FunctionBuilder::represented`]), the
    /// value its `to` converts them to, selected as
    /// [`FunctionBuilder::selected`] selects one.
    fn selected_in(
        &mut self,
        defined: &hir::Defined,
        at: usize,
        components: &hir::Expr,
        cx: &mut Lowering,
    ) -> lir::Place {
        let given = self.place(components, cx);
        let representation = &defined.representations[at];
        self.represented(defined, representation, &given, &Checking::Selected, cx);
        let stored = self.converted(representation.to, &representation.components, given, cx);
        self.selected_from(defined, stored, cx)
    }

    /// The value of the type of the program's own `defined` selected from
    /// the components at `value`, as [`FunctionBuilder::selected`] selects
    /// one.
    fn selected_from(
        &mut self,
        defined: &hir::Defined,
        value: lir::Place,
        cx: &mut Lowering,
    ) -> lir::Place {
        let ty = hir::Type::Defined(Box::new(defined.clone()));
        self.constrained(defined, &value, &Checking::Selected, cx);
        if ty.root().is_none() {
            return self.narrowed(defined, value, cx);
        }
        let selected = self.slot(cx.layout(&ty));
        self.made_in_hierarchy(defined, &value, &selected, &Checking::Selected, cx);
        selected
    }

    /// Each relation-valued component of the value at `value`, of
    /// `defined`, checked against its keys, as `checking` says.
    fn component_keys_checked(
        &mut self,
        defined: &hir::Defined,
        value: &lir::Place,
        checking: &Checking,
        cx: &mut Lowering,
    ) {
        for (component, keys) in &defined.keys {
            let (heading, other) = defined
                .representation(component)
                .expect("a key is of one of the type's components");
            let mut relation = match other {
                Some(other) => self.converted(other.from, &defined.components, value.clone(), cx),
                None => value.clone(),
            };
            relation.path.push(
                heading
                    .attributes()
                    .position(|(c, _)| c == component)
                    .expect("a representation has its components"),
            );
            let ty = heading
                .get(component)
                .expect("a representation has its components")
                .clone();
            for key in keys {
                self.key_checked(&relation, None, &ty, key, cx, |this, failed, cx| {
                    let Failed {
                        value,
                        descriptor,
                        key,
                        pair,
                    } = failed;
                    if let Checking::Read {
                        record,
                        skip: Some(skip),
                    } = checking
                    {
                        // The type's and the component's names, as the
                        // runtime's `ComponentOf`.
                        let names = vec![
                            cx.value_field(&hir::Value::Text(defined.name.clone())),
                            cx.value_field(&hir::Value::Text(component.clone())),
                        ];
                        let of = cx.constant(names);
                        let of = lir::Place::whole(lir::Base::Data(of));
                        return Broken::Skip {
                            function: lir::RuntimeFn::ComponentKeyError,
                            args: vec![
                                skip.errors.clone(),
                                value,
                                descriptor,
                                key,
                                pair,
                                of,
                                record.clone(),
                            ],
                            to: skip.to,
                        };
                    }
                    let ty = this.text(&defined.name, cx);
                    let component = this.text(component, cx);
                    Broken::Fail(Box::new(lir::Failure::Key(match checking {
                        Checking::Selected => lir::KeyFailure::Component {
                            value,
                            descriptor,
                            key,
                            pair,
                            ty,
                            component,
                        },
                        Checking::Read { record, .. } => lir::KeyFailure::ComponentInFile {
                            value,
                            descriptor,
                            key,
                            pair,
                            ty,
                            component,
                            record: record.clone(),
                        },
                    })))
                });
            }
        }
    }

    /// Stop the program unless each key of each relation-valued component
    /// of `defined`, and then each of its constraints, in order, is true of
    /// the value at `value`, laid out as the tuple of its components: a
    /// component of another representation computed by its conversion
    /// from the stored one, and each constraint a call of its operator with
    /// the components as its arguments. The first that is false fails as
    /// `checking` says.
    fn constrained(
        &mut self,
        defined: &hir::Defined,
        value: &lir::Place,
        checking: &Checking,
        cx: &mut Lowering,
    ) {
        self.component_keys_checked(defined, value, checking, cx);
        let descriptor = lir::Place::whole(lir::Base::Data(cx.components_descriptor(defined)));
        let checked = CheckedComponents {
            components: &defined.components,
            value,
            descriptor: &descriptor,
            ty: &defined.name,
        };
        for constraint in &defined.constraints {
            self.constraint_checked(constraint, &checked, checking, cx);
        }
    }

    /// Stop the program unless each of `representation`'s own constraints,
    /// a possible representation of `defined`, in order, is true of the
    /// components at `value`, as given, laid out as the tuple of them,
    /// before its `to` converts them. The first that is false fails as
    /// `checking` says, writing the components as its selector writes them.
    fn represented(
        &mut self,
        defined: &hir::Defined,
        representation: &hir::Representation,
        value: &lir::Place,
        checking: &Checking,
        cx: &mut Lowering,
    ) {
        if representation.constraints.is_empty() {
            return;
        }
        let descriptor = cx.representation_descriptor(defined, representation);
        let descriptor = lir::Place::whole(lir::Base::Data(descriptor));
        let checked = CheckedComponents {
            components: &representation.components,
            value,
            descriptor: &descriptor,
            ty: &defined.name,
        };
        for constraint in &representation.constraints {
            self.constraint_checked(constraint, &checked, checking, cx);
        }
    }

    /// Stop the program unless `constraint` is true of the components
    /// `checked` says, as `checking` says.
    fn constraint_checked(
        &mut self,
        constraint: &hir::Constraint,
        checked: &CheckedComponents,
        checking: &Checking,
        cx: &mut Lowering,
    ) {
        let (value, descriptor) = (checked.value, checked.descriptor);
        let holds = self.holds(constraint, checked.components, value, cx);
        let (kept, failed) = (self.new_block(), self.new_block());
        self.end_block(
            lir::Terminator::Branch {
                condition: holds,
                then: kept,
                otherwise: failed,
            },
            failed,
        );
        let (value, descriptor) = (value.clone(), descriptor.clone());
        let constraint = self.text(&constraint.text, cx);
        let ty = self.text(checked.ty, cx);
        let broken = match checking {
            Checking::Selected => Broken::Fail(Box::new(lir::Failure::BrokenConstraint {
                value,
                descriptor,
                ty,
                constraint,
            })),
            Checking::Read { record, skip: None } => {
                Broken::Fail(Box::new(lir::Failure::BrokenConstraintInFile {
                    value,
                    descriptor,
                    ty,
                    constraint,
                    record: record.clone(),
                }))
            }
            Checking::Read {
                record,
                skip: Some(skip),
            } => Broken::Skip {
                function: lir::RuntimeFn::BrokenConstraintError,
                args: vec![
                    skip.errors.clone(),
                    value,
                    descriptor,
                    ty,
                    constraint,
                    record.clone(),
                ],
                to: skip.to,
            },
        };
        self.broken(broken, kept);
    }

    /// Check that the relation at `relation`, of type `ty`, has no two
    /// tuples with the same values for `key`, doing what `broken` says of
    /// them where it has. A key of the whole heading always holds.
    fn key_checked(
        &mut self,
        relation: &lir::Place,
        added: Option<&lir::Place>,
        ty: &hir::Type,
        key: &hir::Key,
        cx: &mut Lowering,
        broken: impl FnOnce(&mut Self, Failed, &mut Lowering) -> Broken,
    ) {
        let names: Vec<&str> = attribute_names(ty).collect();
        if key.len() == names.len() {
            return;
        }
        let others: BTreeSet<String> = names
            .iter()
            .filter(|name| !key.contains(**name))
            .map(|name| (*name).to_string())
            .collect();
        let descriptor = lir::Place::whole(lir::Base::Data(cx.descriptor(ty)));
        let projected = lir::Place::whole(lir::Base::Data(cx.descriptor(&ty.without(&others))));
        let shared = self.slot(lir::Layout::U8);
        let pair = self.slot(lir::Layout::Struct(vec![
            lir::Layout::Usize,
            lir::Layout::Usize,
        ]));
        let mut args = vec![
            shared.clone(),
            pair.clone(),
            relation.clone(),
            descriptor.clone(),
            projected.clone(),
        ];
        let function = match added {
            Some(added) => {
                args.push(added.clone());
                lir::RuntimeFn::KeyAdded
            }
            None => lir::RuntimeFn::KeyShared,
        };
        self.insts.push(lir::Inst::CallRuntime { function, args });
        let (kept, failed) = (self.new_block(), self.new_block());
        self.end_block(
            lir::Terminator::Branch {
                condition: shared,
                then: failed,
                otherwise: kept,
            },
            failed,
        );
        let broken = broken(
            self,
            Failed {
                value: relation.clone(),
                descriptor,
                key: projected,
                pair,
            },
            cx,
        );
        self.broken(broken, kept);
    }

    /// A text constant's value, in a place.
    fn text(&mut self, text: &str, cx: &mut Lowering) -> lir::Place {
        self.place(&hir::Expr::Constant(hir::Value::Text(text.to_string())), cx)
    }

    /// The only tuple of the relation `operand`, copied into a new stack
    /// slot. The program stops if the relation has none, or several,
    /// writing how many.
    fn only(&mut self, operand: &hir::Expr, cx: &mut Lowering) -> lir::Place {
        let ty = self.type_of(operand);
        let hir::Type::Relation(heading) = &ty else {
            unreachable!("sema takes the only tuple of relations only");
        };
        let (src, _) = self.loose_place(operand, cx);
        let mut count = src.clone();
        count.path.push(1);
        let is_one = self.slot(lir::Layout::U8);
        self.insts.push(lir::Inst::Binary {
            dst: is_one.clone(),
            op: lir::BinaryOp::Compare(lir::Comparison::Equal, lir::Operand::Usize),
            lhs: count.clone(),
            rhs: usize_constant(1, cx),
        });
        let (single, other) = (self.new_block(), self.new_block());
        self.end_block(
            lir::Terminator::Branch {
                condition: is_one,
                then: single,
                otherwise: other,
            },
            other,
        );
        let failure = lir::Failure::NotOneTuple { count };
        self.fail(failure, single);
        let zero = usize_constant(0, cx);
        let tuple = cx.layout(&hir::Type::Tuple(heading.clone()));
        let only = self.element(&src, &zero, tuple.clone());
        let dst = self.slot(tuple.clone());
        self.insts.push(lir::Inst::Copy {
            dst: dst.clone(),
            src: only,
            layout: tuple,
        });
        dst
    }

    /// A new stack slot holding an address.
    fn pointer_slot(&mut self) -> lir::SlotId {
        self.slots.push(lir::Layout::Address);
        lir::SlotId(self.slots.len() - 1)
    }

    /// The value at `src`, of type `ty`, reshaped into one of type
    /// `result_ty`, computed into a new stack slot: each attribute of the
    /// result, in name order, is the operand's attribute at the index in
    /// `sources`. That is what removing and renaming attributes both are.
    fn reshape(
        &mut self,
        ty: &hir::Type,
        result_ty: &hir::Type,
        sources: &[usize],
        src: lir::Place,
        result: Made,
        cx: &mut Lowering,
    ) -> lir::Place {
        let (want, flags) = match result {
            Made::Wanted(order) => (order, 0),
            Made::Kept(order) => (order, lir::ORDERED_RESULT),
        };
        let (hir::Type::Tuple(heading) | hir::Type::Relation(heading)) = ty else {
            unreachable!("sema reshapes only tuples and relations");
        };
        let attribute_types: Vec<&hir::Type> = heading.attributes().map(|(_, t)| t).collect();
        let dst = self.slot(cx.layout(result_ty));
        if let hir::Type::Tuple(_) = ty {
            for (j, &i) in sources.iter().enumerate() {
                let mut from = src.clone();
                from.path.push(i);
                self.insts.push(lir::Inst::Copy {
                    dst: lir::Place {
                        base: dst.base.clone(),
                        path: vec![j],
                    },
                    src: from,
                    layout: cx.layout(attribute_types[i]),
                });
            }
            return dst;
        }
        let tuple_layout = cx.layout(&hir::Type::Tuple(heading.clone()));
        let offsets = cx.constant(
            sources
                .iter()
                .map(|&i| lir::Field::Offset(tuple_layout.clone(), vec![i]))
                .collect(),
        );
        let result_desc = lir::Place::whole(lir::Base::Data(cx.described(result_ty, want)));
        let source_desc = lir::Place::whole(lir::Base::Data(cx.descriptor(ty)));
        self.insts.push(lir::Inst::CallRuntime {
            function: lir::RuntimeFn::Reshape,
            args: vec![
                dst.clone(),
                src,
                result_desc,
                source_desc,
                lir::Place::whole(lir::Base::Data(offsets)),
                orders_constant(Order::Canonical, Order::Canonical, want, flags, cx),
            ],
        });
        self.scoped = true;
        dst
    }

    /// The AND of the relations at `lhs` and `rhs`, of types `lhs_ty` and
    /// `rhs_ty`, computed by the runtime into memory released when the
    /// statement ends. Lowering works out where each attribute is, so the
    /// runtime never matches attributes by name: for each attribute of the
    /// result, which operand it comes from and its offset there, and for
    /// each common attribute, its offset in each operand and its type.
    fn and(
        &mut self,
        lhs: &View,
        rhs: &View,
        (lhs_place, lhs_order): (lir::Place, Order),
        (rhs_place, rhs_order): (lir::Place, Order),
        want: Want,
        cx: &mut Lowering,
    ) -> (lir::Place, Order) {
        let result_ty = lhs.named.and(&rhs.named);
        let joining = Joining::of(&lhs.shape, &rhs.shape, &result_ty, cx);
        let order = match want {
            Want::Sorted => cx.joined_order(lhs, rhs, lhs_order, rhs_order),
            _ => want.as_comes(),
        };
        let described = |ty: &hir::Type, order: Order, cx: &mut Lowering| {
            lir::Place::whole(lir::Base::Data(cx.described(ty, order)))
        };
        let dst = self.slot(cx.layout(&result_ty));
        let args = vec![
            dst.clone(),
            lhs_place,
            rhs_place,
            described(&result_ty, order, cx),
            described(&lhs.ty, lhs_order, cx),
            described(&rhs.ty, rhs_order, cx),
            joining.sources,
            joining.common,
            joining.common_count,
            orders_constant(lhs_order, rhs_order, order, 0, cx),
        ];
        self.insts.push(lir::Inst::CallRuntime {
            function: lir::RuntimeFn::And,
            args,
        });
        self.scoped = true;
        (dst, order)
    }

    /// `expr`, the array of the tuples of `relation`, in the order of the
    /// `loading`'s items, each without the attributes it removes, or the
    /// value of the only one left, computed by the runtime into a new stack
    /// slot. Lowering works out each item's offset and type, and where each
    /// attribute kept goes in an element, so the runtime never finds
    /// attributes by name, and converts the values an order compares in
    /// another possible representation than the stored one (see
    /// [`sorted`]).
    fn load(
        &mut self,
        expr: &hir::Expr,
        relation: &hir::Expr,
        loading: &hir::Loading,
        cx: &mut Lowering,
    ) -> lir::Place {
        let hir::Type::Relation(written) = self.type_of(relation) else {
            unreachable!("sema loads only relations");
        };
        let Sorted {
            relation,
            heading,
            leaves,
        } = sorted(
            self.program,
            relation,
            &written,
            &loading.order,
            &cx.families,
        );
        let relation_ty = hir::Type::Relation(heading.clone());
        let tuple = cx.layout(&hir::Type::Tuple(heading.clone()));
        // Where an attribute is in the tuple, by name order.
        let attribute = |name: &str| {
            heading
                .attributes()
                .position(|(a, _)| a == name)
                .expect("sema names only the relation's attributes")
        };
        let items: Vec<lir::Field> = leaves
            .into_iter()
            .map(|leaf| {
                lir::Field::Struct(vec![
                    lir::Field::Offset(tuple.clone(), leaf.path),
                    lir::Field::Address(cx.descriptor(&leaf.ty)),
                    lir::Field::Usize(u64::from(leaf.descending)),
                ])
            })
            .collect();
        // Each attribute an element keeps: where it is in the tuple, and in
        // the element, which is the value itself if it is one.
        let array_ty = self.type_of(expr);
        let hir::Type::Array(element) = &array_ty else {
            unreachable!("a load makes an array");
        };
        let element = cx.layout(element);
        let kept: Vec<(&str, &hir::Type)> = written
            .attributes()
            .filter(|(a, _)| !loading.removed.contains(*a))
            .collect();
        let parts: Vec<lir::Field> = kept
            .iter()
            .enumerate()
            .map(|(j, (name, ty))| {
                let to = match loading.loaded {
                    hir::Loaded::Tuples => lir::Field::Offset(element.clone(), vec![j]),
                    hir::Loaded::Values => lir::Field::Usize(0),
                };
                lir::Field::Struct(vec![
                    lir::Field::Offset(tuple.clone(), vec![attribute(name)]),
                    to,
                    lir::Field::Size(cx.layout(ty)),
                ])
            })
            .collect();
        let count = usize_constant(len(items.len()), cx);
        let part_count = usize_constant(len(parts.len()), cx);
        let src = self.place(&relation, cx);
        let dst = self.slot(cx.layout(&array_ty));
        let args = vec![
            dst.clone(),
            src.clone(),
            lir::Place::whole(lir::Base::Data(cx.descriptor(&relation_ty))),
            lir::Place::whole(lir::Base::Data(cx.descriptor(&array_ty))),
            lir::Place::whole(lir::Base::Data(cx.constant(items))),
            count,
            lir::Place::whole(lir::Base::Data(cx.constant(parts))),
            part_count,
        ];
        self.insts.push(lir::Inst::CallRuntime {
            function: lir::RuntimeFn::Load,
            args,
        });
        self.scoped = true;
        self.operand_read(&relation, &src, &relation_ty, cx);
        dst
    }

    /// `expr`, the relation of the elements of `array`, each a tuple, with
    /// its position, as `unloading` says, computed by the runtime into a
    /// new stack slot. Lowering works out where each part of an element
    /// goes in a tuple, so the runtime never finds attributes by name.
    fn unload(
        &mut self,
        expr: &hir::Expr,
        array: &hir::Expr,
        unloading: &hir::Unloading,
        cx: &mut Lowering,
    ) -> lir::Place {
        let array_ty = self.type_of(array);
        let relation_ty = self.type_of(expr);
        let src = self.place(array, cx);
        let dst = self.unloaded(&src, &array_ty, &relation_ty, unloading, cx);
        self.operand_read(array, &src, &array_ty, cx);
        dst
    }

    /// The relation of type `relation_ty` the array at `src`, of type
    /// `array_ty`, makes, as `unloading` says, in a new slot, in memory the
    /// statement owns: the runtime's.
    fn unloaded(
        &mut self,
        src: &lir::Place,
        array_ty: &hir::Type,
        relation_ty: &hir::Type,
        unloading: &hir::Unloading,
        cx: &mut Lowering,
    ) -> lir::Place {
        let hir::Type::Array(element) = array_ty else {
            unreachable!("sema unloads only arrays");
        };
        let hir::Type::Relation(heading) = relation_ty else {
            unreachable!("an unload makes a relation");
        };
        let tuple = cx.layout(&hir::Type::Tuple(heading.clone()));
        // Where an attribute is in the tuple, by name order.
        let position = |name: &str| {
            heading
                .attributes()
                .position(|(a, _)| a == name)
                .expect("sema makes the relation's attributes of the elements")
        };
        let part = |from: lir::Field, to: &str, size: lir::Field| {
            lir::Field::Struct(vec![
                from,
                lir::Field::Offset(tuple.clone(), vec![position(to)]),
                size,
            ])
        };
        let parts: Vec<lir::Field> = match (&unloading.named, &**element) {
            (Some(named), _) => vec![part(
                lir::Field::Usize(0),
                named,
                lir::Field::Size(cx.layout(element)),
            )],
            (None, hir::Type::Tuple(elements)) => {
                let made = cx.layout(element);
                elements
                    .attributes()
                    .enumerate()
                    .map(|(j, (name, ty))| {
                        part(
                            lir::Field::Offset(made.clone(), vec![j]),
                            name,
                            lir::Field::Size(cx.layout(ty)),
                        )
                    })
                    .collect()
            }
            (None, other) => unreachable!("sema names an element of {other}"),
        };
        let numbers: Vec<lir::Field> = unloading
            .numbered
            .iter()
            .map(|n| lir::Field::Offset(tuple.clone(), vec![position(n)]))
            .collect();
        let part_count = usize_constant(len(parts.len()), cx);
        let number_count = usize_constant(len(numbers.len()), cx);
        let dst = self.slot(cx.layout(relation_ty));
        let args = vec![
            dst.clone(),
            src.clone(),
            lir::Place::whole(lir::Base::Data(cx.descriptor(array_ty))),
            lir::Place::whole(lir::Base::Data(cx.descriptor(relation_ty))),
            lir::Place::whole(lir::Base::Data(cx.constant(parts))),
            part_count,
            lir::Place::whole(lir::Base::Data(cx.constant(numbers))),
            number_count,
        ];
        self.insts.push(lir::Inst::CallRuntime {
            function: lir::RuntimeFn::Unload,
            args,
        });
        self.scoped = true;
        dst
    }

    /// The transitive closure of the relation `operand`, of two attributes
    /// of one type, in a new slot, in memory the statement owns: the
    /// runtime's, given the relation's descriptor, the result's too.
    fn closed(&mut self, operand: &hir::Expr, cx: &mut Lowering) -> lir::Place {
        let ty = self.type_of(operand);
        let src = self.place(operand, cx);
        let dst = self.slot(cx.layout(&ty));
        let descriptor = lir::Place::whole(lir::Base::Data(cx.descriptor(&ty)));
        self.insts.push(lir::Inst::CallRuntime {
            function: lir::RuntimeFn::Tclose,
            args: vec![dst.clone(), src.clone(), descriptor],
        });
        self.scoped = true;
        self.operand_read(operand, &src, &ty, cx);
        dst
    }

    /// The tuples of the relation `lhs` that have a match in `rhs`, or, if
    /// `negated`, those that have none, computed by the runtime into a new
    /// stack slot, in the order they are in `lhs`, and how that is. A transitive closure's
    /// are found without making the rest of it. Through a rename of `lhs`, they are found
    /// in what it renames, and then reshaped, sorted unless `want` needn't be.
    fn matching(
        &mut self,
        lhs: &hir::Expr,
        rhs: &hir::Expr,
        negated: bool,
        want: Want,
        cx: &mut Lowering,
    ) -> (lir::Place, Order) {
        let narrowed = if negated {
            None
        } else {
            self.closure_matched(lhs, rhs, cx)
        };
        let (left, right, (lhs_place, lhs_order), (rhs_place, rhs_order)) =
            if let Some((closure, other)) = narrowed {
                let (left, right) = (self.view_as_it_is(lhs), self.view_as_it_is(rhs));
                // On one attribute, those are the tuples that have a match.
                if common_attributes(&left.shape, &right.shape, cx).len() == 1 {
                    self.operand_read(rhs, &other, &right.ty, cx);
                    return (closure, Order::Canonical);
                }
                (
                    left,
                    right,
                    (closure, Order::Canonical),
                    (other, Order::Canonical),
                )
            } else {
                let (left, right) = (self.operand_view(lhs, want), self.operand_view(rhs, want));
                // Its tuples are in the order they are in its left operand, which a constant is
                // written in only where canonical order isn't wanted.
                let lhs_given = if want == Want::Canonical {
                    self.given(left.inner, want, cx)
                } else {
                    self.joined_operand(&left, &right, want, cx)
                };
                let rhs_given = self.joined_operand(&right, &left, want, cx);
                (left, right, lhs_given, rhs_given)
            };
        let common = common_attributes(&left.shape, &right.shape, cx);
        let common_count = usize_constant(len(common.len()), cx);
        let negated = usize_constant(u64::from(negated), cx);
        let described = |ty: &hir::Type, order: Order, cx: &mut Lowering| {
            lir::Place::whole(lir::Base::Data(cx.described(ty, order)))
        };
        let dst = self.slot(cx.layout(&left.ty));
        let args = vec![
            dst.clone(),
            lhs_place.clone(),
            rhs_place.clone(),
            described(&left.ty, lhs_order, cx),
            described(&right.ty, rhs_order, cx),
            lir::Place::whole(lir::Base::Data(cx.constant(common))),
            common_count,
            negated,
            orders_constant(lhs_order, rhs_order, Order::Canonical, 0, cx),
        ];
        self.insts.push(lir::Inst::CallRuntime {
            function: lir::RuntimeFn::Matching,
            args,
        });
        self.scoped = true;
        self.operand_read(left.inner, &lhs_place, &left.ty, cx);
        self.operand_read(right.inner, &rhs_place, &right.ty, cx);
        if left.shape.is_as_laid_out() {
            return (dst, cx.read_order(lhs_order, &left));
        }
        let reshaped = self.reshape(
            &left.ty,
            &left.named,
            &left.shape.positions(),
            dst.clone(),
            Made::Wanted(want.as_comes()),
            cx,
        );
        let descriptor = lir::Place::whole(lir::Base::Data(cx.descriptor(&left.ty)));
        self.insts.push(lir::Inst::CallRuntime {
            function: lir::RuntimeFn::DropOperand,
            args: vec![dst, descriptor],
        });
        (reshaped, want.as_comes())
    }

    /// The relations `lhs` and `rhs`, of the same attributes, combined as
    /// `combination` says, computed by the runtime into a new stack slot, and its order.
    /// Their tuples are laid out alike, and merged in one order, the result's: canonical
    /// order, if that is what is wanted, and otherwise any, as `want` is
    /// [`Order::Loose`]. Then, if both would be computed sorted in the same order as the
    /// least work for each ([`FunctionBuilder::sorted_order`]), they are, and otherwise each
    /// is computed in canonical order, as it would be if that were wanted, rather than
    /// computed in one order and sorted again.
    fn combine(
        &mut self,
        combination: hir::Combination,
        lhs: &hir::Expr,
        rhs: &hir::Expr,
        want: Want,
        cx: &mut Lowering,
    ) -> (lir::Place, Order) {
        let ty = combination.combined(&self.type_of(lhs), &self.type_of(rhs));
        let (lhs_ty, rhs_ty) = (self.type_of(lhs), self.type_of(rhs));
        let order = match want {
            Want::Canonical => Order::Canonical,
            _ => self.shared_order(lhs, rhs, cx),
        };
        let (lhs_place, rhs_place) = (self.exactly(lhs, order, cx), self.exactly(rhs, order, cx));
        let descriptor = lir::Place::whole(lir::Base::Data(cx.described(&ty, order)));
        let combination = match combination {
            hir::Combination::Union => lir::Combination::Union,
            hir::Combination::DisjointUnion => lir::Combination::DisjointUnion,
            hir::Combination::ExclusiveUnion => lir::Combination::ExclusiveUnion,
            hir::Combination::Minus => lir::Combination::Minus,
            hir::Combination::IncludedMinus => lir::Combination::IncludedMinus,
            hir::Combination::DisjointChanges => lir::Combination::DisjointChanges,
        };
        let combination = usize_constant(combination.code(), cx);
        let dst = self.slot(cx.layout(&ty));
        self.insts.push(lir::Inst::CallRuntime {
            function: lir::RuntimeFn::Combine,
            args: vec![
                dst.clone(),
                lhs_place.clone(),
                rhs_place.clone(),
                descriptor,
                combination,
            ],
        });
        self.scoped = true;
        self.operand_read(lhs, &lhs_place, &lhs_ty, cx);
        self.operand_read(rhs, &rhs_place, &rhs_ty, cx);
        (dst, order)
    }

    /// The relation `operand` with the relation-valued attributes `names`
    /// ungrouped, computed by the runtime into a new stack slot. Lowering
    /// says where each attribute of the result comes from, so the runtime
    /// never matches attributes by name.
    fn ungroup(
        &mut self,
        operand: &hir::Expr,
        names: &BTreeSet<String>,
        cx: &mut Lowering,
    ) -> lir::Place {
        let ty = &self.type_of(operand);
        let src = self.place(operand, cx);
        let hir::Type::Relation(heading) = ty else {
            unreachable!("sema ungroups only relations");
        };
        let result_ty = ty.ungrouped(names);
        let outer = cx.layout(&hir::Type::Tuple(heading.clone()));
        let position =
            |heading: &hir::Heading, name: &str| heading.attributes().position(|(a, _)| a == name);
        // Each ungrouped attribute's position in the outer tuple, and the
        // heading and layout of its relation's tuples.
        let inner: Vec<(usize, &hir::Heading, lir::Layout)> = names
            .iter()
            .map(|name| {
                let Some(hir::Type::Relation(inner)) = heading.get(name) else {
                    unreachable!("sema ungroups only relation-valued attributes");
                };
                let at = position(heading, name).expect("an ungrouped attribute is the operand's");
                (at, inner, cx.layout(&hir::Type::Tuple(inner.clone())))
            })
            .collect();
        let sources = attribute_names(&result_ty)
            .map(|name| {
                let (from, offset) = match position(&heading.without(names), name) {
                    Some(_) => (
                        0,
                        lir::Field::Offset(
                            outer.clone(),
                            vec![
                                position(heading, name).expect("a kept attribute is the operand's"),
                            ],
                        ),
                    ),
                    None => inner
                        .iter()
                        .enumerate()
                        .find_map(|(k, (_, inner, tuple))| {
                            let i = position(inner, name)?;
                            Some((len(k + 1), lir::Field::Offset(tuple.clone(), vec![i])))
                        })
                        .expect("each attribute of the result is kept or ungrouped"),
                };
                lir::Field::Struct(vec![lir::Field::Usize(from), offset])
            })
            .collect();
        let ungrouped: Vec<lir::Field> = inner
            .iter()
            .map(|(at, _, tuple)| {
                lir::Field::Struct(vec![
                    lir::Field::Offset(outer.clone(), vec![*at]),
                    lir::Field::Size(tuple.clone()),
                ])
            })
            .collect();
        let count = cx.constant(vec![lir::Field::Usize(len(ungrouped.len()))]);
        let descriptor = |ty: &hir::Type, cx: &mut Lowering| {
            lir::Place::whole(lir::Base::Data(cx.descriptor(ty)))
        };
        let dst = self.slot(cx.layout(&result_ty));
        let args = vec![
            dst.clone(),
            src.clone(),
            descriptor(&result_ty, cx),
            descriptor(ty, cx),
            lir::Place::whole(lir::Base::Data(cx.constant(sources))),
            lir::Place::whole(lir::Base::Data(cx.constant(ungrouped))),
            lir::Place {
                base: lir::Base::Data(count),
                path: vec![0],
            },
        ];
        self.insts.push(lir::Inst::CallRuntime {
            function: lir::RuntimeFn::Ungroup,
            args,
        });
        self.scoped = true;
        self.operand_read(operand, &src, ty, cx);
        dst
    }

    /// `lhs` and `rhs` joined, computed into a new stack slot: the AND of
    /// relations, by the runtime, or the join of tuples, and how its tuples are ordered, as
    /// `want` says, unless they are in canonical order all the same. An operand is read
    /// through any renames around it, so that none of its tuples move, and in any order.
    fn joined(
        &mut self,
        lhs: &hir::Expr,
        rhs: &hir::Expr,
        want: Want,
        cx: &mut Lowering,
    ) -> (lir::Place, Order) {
        if let Some((operand, key)) = self
            .lookup_in(lhs, rhs)
            .or_else(|| self.lookup_in(rhs, lhs))
        {
            return (self.looked_up(operand, key, cx), Order::Canonical);
        }
        // What a closure is matched with, or matched with it, is joined in canonical order,
        // as `sorted_order` says.
        let want = if want == Want::Sorted && joins_a_closure(lhs, rhs) {
            Want::Canonical
        } else {
            want
        };
        let lhs_ty = self.type_of(lhs);
        let rhs_ty = self.type_of(rhs);
        if let hir::Type::Tuple(_) = lhs_ty {
            let (lhs_place, rhs_place) = (self.place(lhs, cx), self.place(rhs, cx));
            let dst = self.tuple_join(&lhs_ty, &rhs_ty, &lhs_place, &rhs_place, cx);
            return (dst, Order::Canonical);
        }
        if let Some(leaves) = several_joined(lhs, rhs) {
            return self.joined_as_it_runs(lhs, rhs, &leaves, want, cx);
        }
        // A relation of only the attribute a closure's tuples were found by adds
        // nothing to them, and keeps no others.
        let is_one_attribute = |ty: &hir::Type| matches!(ty, hir::Type::Relation(heading) if heading.attributes().count() == 1);
        let canonical = |place: lir::Place| (place, Order::Canonical);
        let (left, right, lhs_given, rhs_given) =
            if let Some((closure, other)) = self.closure_matched(lhs, rhs, cx) {
                if is_one_attribute(&rhs_ty) {
                    self.operand_read(rhs, &other, &rhs_ty, cx);
                    return (closure, Order::Canonical);
                }
                let (left, right) = (self.view_as_it_is(lhs), self.view_as_it_is(rhs));
                (left, right, canonical(closure), canonical(other))
            } else if let Some((closure, other)) = self.closure_matched(rhs, lhs, cx) {
                if is_one_attribute(&lhs_ty) {
                    self.operand_read(lhs, &other, &lhs_ty, cx);
                    return (closure, Order::Canonical);
                }
                let (left, right) = (self.view_as_it_is(lhs), self.view_as_it_is(rhs));
                (left, right, canonical(other), canonical(closure))
            } else {
                let (left, right) = (self.operand_view(lhs, want), self.operand_view(rhs, want));
                let lhs_given = self.joined_operand(&left, &right, want, cx);
                let rhs_given = self.joined_operand(&right, &left, want, cx);
                (left, right, lhs_given, rhs_given)
            };
        let (lhs_place, rhs_place) = (lhs_given.0.clone(), rhs_given.0.clone());
        let joined = self.and(&left, &right, lhs_given, rhs_given, want, cx);
        self.operand_read(left.inner, &lhs_place, &left.ty, cx);
        self.operand_read(right.inner, &rhs_place, &right.ty, cx);
        joined
    }

    /// The AND of `lhs` and `rhs`, of the operands `leaves`, which are all computed first, in
    /// the order written: their sizes then choose the order of joining, as the program runs,
    /// if the largest is large enough for that to matter ([`lir::RuntimeFn::Join`]), and
    /// otherwise the plan fixed when compiling runs. The two give the same tuples, in the
    /// order `want` asks. What is made of them is given up as soon as it has been used.
    fn joined_as_it_runs(
        &mut self,
        lhs: &hir::Expr,
        rhs: &hir::Expr,
        leaves: &[&hir::Expr],
        want: Want,
        cx: &mut Lowering,
    ) -> (lir::Place, Order) {
        let views: Vec<View> = leaves
            .iter()
            .map(|leaf| self.operand_view(leaf, want))
            .collect();
        let given: Vec<(lir::Place, Order)> = views
            .iter()
            .map(|view| self.given(view.inner, want, cx))
            .collect();
        let result_ty = views
            .iter()
            .skip(1)
            .fold(views[0].named.clone(), |so_far, view| {
                so_far.and(&view.named)
            });
        let names: Vec<String> = attribute_names(&result_ty).map(str::to_string).collect();
        let flags: Vec<u64> = views
            .iter()
            .zip(&given)
            .map(|(view, (place, _))| self.given_up_by_a_join(view, place, cx))
            .collect();
        let operands: Vec<lir::Field> = views
            .iter()
            .zip(&given)
            .zip(&flags)
            .map(|((view, (_, order)), flags)| join_operand(view, *order, *flags, &names, cx))
            .collect();
        let relations = self.slot(lir::Layout::Struct(vec![lir::Layout::Address; given.len()]));
        for (i, (place, order)) in given.iter().enumerate() {
            self.insts.push(lir::Inst::RelationAddress {
                dst: lir::Place {
                    path: [relations.path.clone(), vec![i]].concat(),
                    ..relations.clone()
                },
                src: place.clone(),
                loose: *order != Order::Canonical,
            });
        }
        let dst = self.slot(cx.layout(&result_ty));
        let joined = self.slot(lir::Layout::U8);
        let descriptor = lir::Place::whole(lir::Base::Data(cx.descriptor(&result_ty)));
        let orders = orders_constant(Order::Canonical, Order::Canonical, want.as_comes(), 0, cx);
        self.insts.push(lir::Inst::CallRuntime {
            function: lir::RuntimeFn::Join,
            args: vec![
                dst.clone(),
                relations,
                lir::Place::whole(lir::Base::Data(cx.constant(operands))),
                usize_constant(len(given.len()), cx),
                descriptor,
                joined.clone(),
                orders,
            ],
        });
        self.scoped = true;
        let (chosen, fixed, after) = (self.new_block(), self.new_block(), self.new_block());
        self.end_block(
            lir::Terminator::Branch {
                condition: joined,
                then: chosen,
                otherwise: fixed,
            },
            chosen,
        );
        // What the runtime dropped it has. Each other operand is given up here, as whatever it
        // made may point to it. A binding it has released is zero by now, and releasing that
        // does nothing, but says so here, for what checks that nothing reads it after that.
        for ((view, (place, _)), flags) in views.iter().zip(&given).zip(&flags) {
            if flags & lir::JOIN_DROPS == 0 {
                self.operand_read(view.inner, place, &view.ty, cx);
            }
        }
        self.end_block(lir::Terminator::Jump(after), fixed);
        // Either way gives the order the runtime does, canonical for a sorted one.
        let fixed_want = if want == Want::Sorted {
            Want::Canonical
        } else {
            want
        };
        let planned = self.joined_by_the_fixed_plan(lhs, rhs, &views, &given, fixed_want, cx);
        self.insts.push(lir::Inst::Copy {
            dst: dst.clone(),
            src: planned,
            layout: cx.layout(&result_ty),
        });
        self.end_block(lir::Terminator::Jump(after), after);
        (dst, want.as_comes())
    }

    /// The AND of `lhs` and `rhs`, of the operands `views`, computed at `given`, by the plan
    /// fixed when compiling, each operand given up after it.
    fn joined_by_the_fixed_plan(
        &mut self,
        lhs: &hir::Expr,
        rhs: &hir::Expr,
        views: &[View],
        given: &[(lir::Place, Order)],
        want: Want,
        cx: &mut Lowering,
    ) -> lir::Place {
        let mut operands = PlannedOperands {
            views,
            given,
            next: 0,
        };
        let (planned, _) = self.planned_and(lhs, rhs, &mut operands, want, cx);
        for (view, (place, _)) in views.iter().zip(given) {
            self.operand_read(view.inner, place, &view.ty, cx);
        }
        planned
    }

    /// The AND of `lhs` and `rhs`, the operands in it the next of `operands`, by the plan fixed
    /// when compiling, and each AND it reads given up. An operand isn't one of those, even if
    /// it is an AND under a rename: all of them are given up after everything.
    fn planned_and<'e>(
        &mut self,
        lhs: &'e hir::Expr,
        rhs: &'e hir::Expr,
        operands: &mut PlannedOperands<'_, 'e>,
        want: Want,
        cx: &mut Lowering,
    ) -> (lir::Place, Order) {
        let (left, lhs_place, lhs_order) = self.planned_part(lhs, operands, want, cx);
        let (right, rhs_place, rhs_order) = self.planned_part(rhs, operands, want, cx);
        let planned = self.and(
            &left,
            &right,
            (lhs_place.clone(), lhs_order),
            (rhs_place.clone(), rhs_order),
            want,
            cx,
        );
        for (part, view, made) in [(lhs, &left, &lhs_place), (rhs, &right, &rhs_place)] {
            if is_and(part) {
                self.operand_read(view.inner, made, &view.ty, cx);
            }
        }
        planned
    }

    /// The part of an AND `expr`, the operands in it the next of `operands`, by the plan fixed
    /// when compiling: how it is read, where it is, and how it is ordered.
    fn planned_part<'e>(
        &mut self,
        expr: &'e hir::Expr,
        operands: &mut PlannedOperands<'_, 'e>,
        want: Want,
        cx: &mut Lowering,
    ) -> (View<'e>, lir::Place, Order) {
        let mut inner = expr;
        while let hir::Expr::At(_, within) = inner {
            inner = within;
        }
        let hir::Expr::And(lhs, rhs) = inner else {
            let at = operands.next;
            operands.next += 1;
            let (place, order) = &operands.given[at];
            return (operands.views[at].clone(), place.clone(), *order);
        };
        let (place, order) = self.planned_and(lhs, rhs, operands, want, cx);
        (self.view_as_it_is(expr), place, order)
    }

    /// How a join of several gives up the operand `view` at `place`, as
    /// [`FunctionBuilder::operand_read`] would after it, as soon as it has been joined: its
    /// `JOIN_` flag, if what is made of it holds no pointer into it, and nothing else can read
    /// it.
    fn given_up_by_a_join(&self, view: &View, place: &lir::Place, cx: &mut Lowering) -> u64 {
        let hir::Type::Relation(heading) = &view.ty else {
            return 0;
        };
        if cx.points(&hir::Type::Tuple(heading.clone())) {
            return 0;
        }
        let mut operand = view.inner;
        while let hir::Expr::At(_, inner) | hir::Expr::Treat(_, inner) = operand {
            operand = inner;
        }
        match operand {
            hir::Expr::Local(id) if self.last_read.contains(id) => {
                if self.local_place(*id).0 == *place {
                    lir::JOIN_RELEASES
                } else {
                    0
                }
            }
            hir::Expr::Local(_)
            | hir::Expr::Variable(_)
            | hir::Expr::ComputedConstant(..)
            | hir::Expr::Parameter(_)
            | hir::Expr::Constant(_)
            | hir::Expr::Current(..) => 0,
            _ if self.is_a_bindings(place) => 0,
            _ => lir::JOIN_DROPS,
        }
    }

    /// `expr`, computed as it is, with nothing read through a rename.
    fn view_as_it_is<'e>(&self, expr: &'e hir::Expr) -> View<'e> {
        let ty = self.type_of(expr);
        let (hir::Type::Tuple(heading) | hir::Type::Relation(heading)) = &ty else {
            unreachable!("only relations and tuples are joined");
        };
        let shape = Shape::plain(heading);
        View {
            inner: expr,
            named: ty.clone(),
            ty,
            shape,
        }
    }

    /// Whether the AND of `operand` and `other` is a lookup in `operand` (see
    /// [`FunctionBuilder::looked_up`]), and of what tuple, `other`'s one:
    /// in a loop over a relation's tuples, `other` is a relation of one
    /// tuple, as an image relation's is of the tuple at hand's attributes,
    /// and `operand` a relation that reads neither the tuple at hand nor a
    /// local variable bound in the loop, and so is the same for each tuple.
    fn lookup_in<'e>(
        &self,
        operand: &'e hir::Expr,
        other: &'e hir::Expr,
    ) -> Option<(&'e hir::Expr, &'e hir::Expr)> {
        let lookups = self.tuples.last()?.lookups.as_ref()?;
        let hir::Expr::Relation(_, tuples) = other else {
            return None;
        };
        let [key] = tuples.as_slice() else {
            return None;
        };
        if !matches!(self.type_of(operand), hir::Type::Relation(_)) {
            return None;
        }
        let read = operand.tuple_read(0);
        let varies = read.whole
            || !read.attributes.is_empty()
            || lookups.varying.iter().any(|id| operand.reads_local(*id));
        (!varies).then_some((operand, key))
    }

    /// The AND of the relation `operand` and a relation of the tuple `key`
    /// alone, in a loop over a relation's tuples, for each of which
    /// `operand` is the same (see [`FunctionBuilder::lookup_in`]), as an
    /// image relation is of the relation it is of and the tuple at hand's
    /// common attributes. Rather than sort `operand` again for each tuple,
    /// as the AND does, the first tuple that needs them computes it and its
    /// index on the common attributes ([`lir::RuntimeFn::Index`]), which
    /// slots keep, as they do a `Let`'s binding, until the loop ends; each
    /// tuple then finds those that agree with `key` by binary search,
    /// already in canonical order ([`lir::RuntimeFn::Lookup`]). A lookup
    /// in the same relation on the same attributes, elsewhere in the loop,
    /// shares them. `operand` is computed only where it was, if at all, and
    /// once, as its value would be the same each time.
    fn looked_up(&mut self, operand: &hir::Expr, key: &hir::Expr, cx: &mut Lowering) -> lir::Place {
        let rel_ty = self.type_of(operand);
        let (hir::Type::Relation(rel_heading), hir::Type::Tuple(key_heading)) =
            (&rel_ty, self.type_of(key))
        else {
            unreachable!("a lookup is in a relation, of a tuple");
        };
        let key_ty = hir::Type::Relation(key_heading.clone());
        let result_ty = rel_ty.and(&key_ty);
        let joining = Joining::of(
            &Shape::plain(rel_heading),
            &Shape::plain(&key_heading),
            &result_ty,
            cx,
        );
        let on: Vec<String> = rel_heading.common(&key_heading).into_iter().collect();
        let (kept, index) = self.lookup(operand, on, &rel_ty, &joining, cx);
        let key = self.place(key, cx);
        let dst = self.slot(cx.layout(&result_ty));
        let descriptor = |ty: &hir::Type, cx: &mut Lowering| {
            lir::Place::whole(lir::Base::Data(cx.descriptor(ty)))
        };
        self.insts.push(lir::Inst::CallRuntime {
            function: lir::RuntimeFn::Lookup,
            args: vec![
                dst.clone(),
                kept,
                index,
                key,
                descriptor(&result_ty, cx),
                descriptor(&rel_ty, cx),
                joining.sources,
                joining.common,
                joining.common_count,
            ],
        });
        self.scoped = true;
        dst
    }

    /// The slots of the relation `operand`, of type `rel_ty`, that the loop
    /// at hand looks up in on the attributes `on`, which `joining` says
    /// where they are, and of its index on them: the loop's lookup's, if
    /// it has one, or else a new one's. Either way, code that computes them
    /// both, if they aren't yet, comes next (see
    /// [`FunctionBuilder::looked_up`]).
    fn lookup(
        &mut self,
        operand: &hir::Expr,
        on: Vec<String>,
        rel_ty: &hir::Type,
        joining: &Joining,
        cx: &mut Lowering,
    ) -> (lir::Place, lir::Place) {
        let existing = self
            .tuples
            .last()
            .and_then(|at_hand| at_hand.lookups.as_ref())
            .and_then(|lookups| {
                lookups
                    .made
                    .iter()
                    .find(|lookup| lookup.operand == *operand && lookup.on == on)
            })
            .map(|lookup| (lookup.kept.clone(), lookup.index.clone()));
        let (kept, index) = existing.unwrap_or_else(|| {
            let kept = self.slot(cx.layout(rel_ty));
            let index = self.slot(cx.layout(&index_type()));
            self.lets.push((kept.clone(), rel_ty.clone()));
            self.lets.push((index.clone(), index_type()));
            let lookup = Lookup {
                operand: operand.clone(),
                on,
                ty: rel_ty.clone(),
                kept: kept.clone(),
                index: index.clone(),
            };
            self.tuples
                .last_mut()
                .and_then(|at_hand| at_hand.lookups.as_mut())
                .expect("a lookup is in a loop")
                .made
                .push(lookup);
            (kept, index)
        });
        let descriptor = |ty: &hir::Type, cx: &mut Lowering| {
            lir::Place::whole(lir::Base::Data(cx.descriptor(ty)))
        };
        // The first tuple that needs them computes the relation and its
        // index: until then, the index is null.
        let mut pointer = index.clone();
        pointer.path.push(0);
        let unmade = self.slot(lir::Layout::U8);
        self.insts.push(lir::Inst::IsNull {
            dst: unmade.clone(),
            src: pointer,
        });
        let (compute, computed) = (self.new_block(), self.new_block());
        self.end_block(
            lir::Terminator::Branch {
                condition: unmade,
                then: compute,
                otherwise: computed,
            },
            compute,
        );
        let src = self.place(operand, cx);
        self.insts.push(lir::Inst::Copy {
            dst: kept.clone(),
            src,
            layout: cx.layout(rel_ty),
        });
        self.insts.push(lir::Inst::CallRuntime {
            function: lir::RuntimeFn::Retain,
            args: vec![kept.clone(), descriptor(rel_ty, cx)],
        });
        self.insts.push(lir::Inst::CallRuntime {
            function: lir::RuntimeFn::Index,
            args: vec![
                index.clone(),
                kept.clone(),
                descriptor(rel_ty, cx),
                descriptor(&index_type(), cx),
                joining.common.clone(),
                joining.common_count.clone(),
            ],
        });
        self.insts.push(lir::Inst::CallRuntime {
            function: lir::RuntimeFn::Retain,
            args: vec![index.clone(), descriptor(&index_type(), cx)],
        });
        self.end_block(lir::Terminator::Jump(computed), computed);
        (kept, index)
    }

    /// The join of the tuples at `lhs` and `rhs`, of types `lhs_ty` and
    /// `rhs_ty`, computed into a new stack slot: each common attribute is
    /// compared first, and the program stops if they differ, since no
    /// tuple would do, writing it and its values; then each attribute is
    /// copied from the operand that has it.
    fn tuple_join(
        &mut self,
        lhs_ty: &hir::Type,
        rhs_ty: &hir::Type,
        lhs: &lir::Place,
        rhs: &lir::Place,
        cx: &mut Lowering,
    ) -> lir::Place {
        let (hir::Type::Tuple(lhs_heading), hir::Type::Tuple(rhs_heading)) = (lhs_ty, rhs_ty)
        else {
            unreachable!("a tuple join is of tuples");
        };
        let field = |place: &lir::Place, i: usize| {
            let mut field = place.clone();
            field.path.push(i);
            field
        };
        let position =
            |heading: &hir::Heading, name: &str| heading.attributes().position(|(a, _)| a == name);
        for (i, (name, ty)) in lhs_heading.attributes().enumerate() {
            let Some(j) = position(rhs_heading, name) else {
                continue;
            };
            let compared = hir::Compared::of(ty);
            let op = hir::BinaryOp::Compare(hir::Comparison::Equal, compared);
            let equal = self.binary(op, ty, field(lhs, i), field(rhs, j), cx);
            let (agree, mismatch) = (self.new_block(), self.new_block());
            self.end_block(
                lir::Terminator::Branch {
                    condition: equal,
                    then: agree,
                    otherwise: mismatch,
                },
                mismatch,
            );
            let attribute = hir::Expr::Constant(hir::Value::Text(name.to_string()));
            let failure = lir::Failure::TupleMismatch {
                attribute: self.place(&attribute, cx),
                lhs: field(lhs, i),
                rhs: field(rhs, j),
                descriptor: lir::Place::whole(lir::Base::Data(cx.descriptor(ty))),
            };
            self.fail(failure, agree);
        }
        let result_ty = lhs_ty.and(rhs_ty);
        let hir::Type::Tuple(result) = &result_ty else {
            unreachable!("a tuple join is a tuple");
        };
        let dst = self.slot(cx.layout(&result_ty));
        for (k, (name, ty)) in result.attributes().enumerate() {
            let src = position(lhs_heading, name).map_or_else(
                || {
                    let j = position(rhs_heading, name)
                        .expect("each attribute of the join is an operand's");
                    field(rhs, j)
                },
                |i| field(lhs, i),
            );
            self.insts.push(lir::Inst::Copy {
                dst: field(&dst, k),
                src,
                layout: cx.layout(ty),
            });
        }
        dst
    }

    /// `op src`, computed into a new stack slot.
    fn unary(&mut self, op: hir::UnaryOp, src: lir::Place) -> lir::Place {
        let op = match op {
            hir::UnaryOp::Not => lir::UnaryOp::Not(lir::Bits::Boolean),
            hir::UnaryOp::Complement => lir::UnaryOp::Not(lir::Bits::Integer),
            hir::UnaryOp::Negate(numeric) => lir::UnaryOp::Negate(lower_numeric(numeric)),
        };
        let dst = self.slot(operand_layout(op.operand()));
        self.insts.push(lir::Inst::Unary {
            dst: dst.clone(),
            op,
            src,
        });
        dst
    }

    /// Whether the relations `lhs` and `rhs`, of type `operand_ty`, are
    /// ordered as `comparison` says, as sets: `>=` and `>` are `<=` and `<`
    /// the other way around.
    fn subset(
        &mut self,
        comparison: hir::Comparison,
        operand_ty: &hir::Type,
        lhs: lir::Place,
        rhs: lir::Place,
        cx: &mut Lowering,
    ) -> lir::Place {
        let (lhs, rhs) = match comparison {
            hir::Comparison::Less | hir::Comparison::LessOrEqual => (lhs, rhs),
            _ => (rhs, lhs),
        };
        let proper = matches!(comparison, hir::Comparison::Less | hir::Comparison::Greater);
        let dst = self.slot(lir::Layout::U8);
        let descriptor = lir::Place::whole(lir::Base::Data(cx.descriptor(operand_ty)));
        let proper = usize_constant(u64::from(proper), cx);
        self.insts.push(lir::Inst::CallRuntime {
            function: lir::RuntimeFn::Subset,
            args: vec![dst.clone(), lhs, rhs, descriptor, proper],
        });
        dst
    }

    /// Whether `lhs` and `rhs`, of type `operand_ty`, are equal or, if
    /// `comparison` isn't `=`, not.
    fn equality(
        &mut self,
        comparison: hir::Comparison,
        operand_ty: &hir::Type,
        lhs: lir::Place,
        rhs: lir::Place,
        cx: &mut Lowering,
    ) -> lir::Place {
        let equal = self.slot(lir::Layout::U8);
        let descriptor = lir::Place::whole(lir::Base::Data(cx.descriptor(operand_ty)));
        self.insts.push(lir::Inst::CallRuntime {
            function: lir::RuntimeFn::Equal,
            args: vec![equal.clone(), lhs, rhs, descriptor],
        });
        if comparison == hir::Comparison::Equal {
            return equal;
        }
        let dst = self.slot(lir::Layout::U8);
        self.insts.push(lir::Inst::Unary {
            dst: dst.clone(),
            op: lir::UnaryOp::Not(lir::Bits::Boolean),
            src: equal,
        });
        dst
    }

    /// `lhs op rhs`, of operands of type `operand_ty`, computed into a new
    /// stack slot.
    fn binary(
        &mut self,
        op: hir::BinaryOp,
        operand_ty: &hir::Type,
        lhs: lir::Place,
        rhs: lir::Place,
        cx: &mut Lowering,
    ) -> lir::Place {
        let (op, lhs, rhs) = match op {
            // Relations ordered as sets: `>=` and `>` are `<=` and `<` the
            // other way around.
            hir::BinaryOp::Compare(
                comparison @ (hir::Comparison::Less
                | hir::Comparison::LessOrEqual
                | hir::Comparison::Greater
                | hir::Comparison::GreaterOrEqual),
                hir::Compared::Relation,
            ) => return self.subset(comparison, operand_ty, lhs, rhs, cx),
            // Values of a type of the program's own by its order, which is
            // then compared with 0.
            hir::BinaryOp::Compare(
                comparison @ (hir::Comparison::Less
                | hir::Comparison::LessOrEqual
                | hir::Comparison::Greater
                | hir::Comparison::GreaterOrEqual),
                hir::Compared::Defined { .. } | hir::Compared::Array { .. },
            ) => {
                let ordering = self.ordering(operand_ty, lhs, rhs, cx);
                let zero = self.place(&hir::Expr::Constant(hir::Value::Integer(0)), cx);
                let op = lir::BinaryOp::Compare(
                    lower_comparison(comparison),
                    lir::Operand::Numeric(lir::Numeric::Integer),
                );
                (op, ordering, zero)
            }
            hir::BinaryOp::Compare(
                comparison,
                hir::Compared::Defined { .. }
                | hir::Compared::Tuple
                | hir::Compared::Relation
                | hir::Compared::Array { .. },
            ) => return self.equality(comparison, operand_ty, lhs, rhs, cx),
            hir::BinaryOp::ConcatenateArrays => {
                let dst = self.slot(cx.layout(operand_ty));
                let descriptor = lir::Place::whole(lir::Base::Data(cx.descriptor(operand_ty)));
                self.insts.push(lir::Inst::CallRuntime {
                    function: lir::RuntimeFn::ConcatArrays,
                    args: vec![dst.clone(), lhs, rhs, descriptor],
                });
                self.scoped = true;
                return dst;
            }
            hir::BinaryOp::Concatenate => {
                let dst = self.slot(scalar_layout(hir::Scalar::Text));
                self.insts.push(lir::Inst::CallRuntime {
                    function: lir::RuntimeFn::Concat,
                    args: vec![dst.clone(), lhs, rhs],
                });
                self.scoped = true;
                return dst;
            }
            hir::BinaryOp::Compare(
                comparison,
                hir::Compared::Scalar(hir::Scalar::Text | hir::Scalar::Character),
            ) => {
                // The runtime orders the texts as -1, 0 or 1, which is then
                // compared with 0.
                let ordering = self.slot(lir::Layout::I64);
                self.insts.push(lir::Inst::CallRuntime {
                    function: lir::RuntimeFn::CompareText,
                    args: vec![ordering.clone(), lhs, rhs],
                });
                let zero = self.place(&hir::Expr::Constant(hir::Value::Integer(0)), cx);
                let op = lir::BinaryOp::Compare(
                    lower_comparison(comparison),
                    lir::Operand::Numeric(lir::Numeric::Integer),
                );
                (op, ordering, zero)
            }
            op => (lower_binary_op(op), lhs, rhs),
        };
        let dst = self.slot(match op {
            lir::BinaryOp::Compare(..) => scalar_layout(hir::Scalar::Boolean),
            op => operand_layout(op.operand()),
        });
        self.insts.push(lir::Inst::Binary {
            dst: dst.clone(),
            op,
            lhs,
            rhs,
        });
        dst
    }

    /// The order of the values at `lhs` and `rhs`, of the ordered type
    /// `ty`, as [`hir::compare`] has it, as an integer in a new slot: -1 if
    /// the first comes first, 1 if it comes last, and else 0. Text's is the
    /// runtime's, and other scalars' by `<` and `>`. A value of a type of
    /// the program's own's is its components', in the representation and
    /// order its order gives, converted to it if it isn't the stored one,
    /// the first that isn't 0, a descending one's the other way.
    fn ordering(
        &mut self,
        ty: &hir::Type,
        lhs: lir::Place,
        rhs: lir::Place,
        cx: &mut Lowering,
    ) -> lir::Place {
        let result = self.slot(lir::Layout::I64);
        match ty {
            hir::Type::Scalar(hir::Scalar::Text | hir::Scalar::Character) => {
                self.insts.push(lir::Inst::CallRuntime {
                    function: lir::RuntimeFn::CompareText,
                    args: vec![result.clone(), lhs, rhs],
                });
            }
            // In the runtime's order, which is its elements' (see
            // `hir::Type::orders_as_stored`).
            hir::Type::Array(_) => {
                let descriptor = lir::Place::whole(lir::Base::Data(cx.descriptor(ty)));
                self.insts.push(lir::Inst::CallRuntime {
                    function: lir::RuntimeFn::Compare,
                    args: vec![result.clone(), lhs, rhs, descriptor],
                });
            }
            hir::Type::Scalar(scalar) => {
                let compared = hir::Compared::Scalar(*scalar);
                let less = hir::BinaryOp::Compare(hir::Comparison::Less, compared);
                let is_less = self.binary(less, ty, lhs.clone(), rhs.clone(), cx);
                let (first, after, done) = (self.new_block(), self.new_block(), self.new_block());
                self.end_block(
                    lir::Terminator::Branch {
                        condition: is_less,
                        then: first,
                        otherwise: after,
                    },
                    first,
                );
                self.set_integer(&result, -1, cx);
                self.end_block(lir::Terminator::Jump(done), after);
                let greater = hir::BinaryOp::Compare(hir::Comparison::Greater, compared);
                let is_greater = self.binary(greater, ty, lhs, rhs, cx);
                let (last, same) = (self.new_block(), self.new_block());
                self.end_block(
                    lir::Terminator::Branch {
                        condition: is_greater,
                        then: last,
                        otherwise: same,
                    },
                    last,
                );
                self.set_integer(&result, 1, cx);
                self.end_block(lir::Terminator::Jump(done), same);
                self.set_integer(&result, 0, cx);
                self.end_block(lir::Terminator::Jump(done), done);
            }
            hir::Type::Defined(defined) => {
                self.components_ordering(defined, lhs, rhs, &result, cx);
            }
            other => unreachable!("only ordered values are compared, not values of `{other}`"),
        }
        result
    }

    /// The order of the values at `lhs` and `rhs`, of the type of the
    /// program's own `defined`, into `result`, as [`FunctionBuilder::ordering`]
    /// has it: their components', in the representation and order its
    /// order gives, converted to it if it isn't the stored one, the first
    /// that isn't 0, a descending one's the other way.
    fn components_ordering(
        &mut self,
        defined: &hir::Defined,
        lhs: lir::Place,
        rhs: lir::Place,
        result: &lir::Place,
        cx: &mut Lowering,
    ) {
        let (order, components) = defined
            .ordered_by()
            .expect("sema compares only ordered values");
        let (mut lhs, mut rhs) = (lhs, rhs);
        let path = cx.families.components_path(defined);
        lhs.path.extend(path.iter().copied());
        rhs.path.extend(path);
        let (lhs, rhs) = (
            self.widened(defined, lhs, cx),
            self.widened(defined, rhs, cx),
        );
        // In another representation than the stored one, the components its
        // conversion computes.
        let (lhs, rhs) = match order.from {
            None => (lhs, rhs),
            Some(from) => (
                self.converted(from, &defined.components, lhs, cx),
                self.converted(from, &defined.components, rhs, cx),
            ),
        };
        self.set_integer(result, 0, cx);
        let done = self.new_block();
        for item in &order.items {
            let (j, (_, component)) = components
                .attributes()
                .enumerate()
                .find(|(_, (name, _))| *name == item.attribute)
                .expect("an order names its representation's components");
            let (mut first, mut second) = (lhs.clone(), rhs.clone());
            first.path.push(j);
            second.path.push(j);
            if item.descending {
                std::mem::swap(&mut first, &mut second);
            }
            let ordering = self.ordering(component, first, second, cx);
            self.insts.push(lir::Inst::Copy {
                dst: result.clone(),
                src: ordering.clone(),
                layout: lir::Layout::I64,
            });
            let zero = self.place(&hir::Expr::Constant(hir::Value::Integer(0)), cx);
            let differs = self.slot(lir::Layout::U8);
            self.insts.push(lir::Inst::Binary {
                dst: differs.clone(),
                op: lir::BinaryOp::Compare(
                    lir::Comparison::NotEqual,
                    lir::Operand::Numeric(lir::Numeric::Integer),
                ),
                lhs: ordering,
                rhs: zero,
            });
            let next = self.new_block();
            self.end_block(
                lir::Terminator::Branch {
                    condition: differs,
                    then: done,
                    otherwise: next,
                },
                next,
            );
        }
        self.end_block(lir::Terminator::Jump(done), done);
    }

    /// Sets the integer at `dst` to `n`.
    fn set_integer(&mut self, dst: &lir::Place, n: i64, cx: &mut Lowering) {
        let src = self.place(&hir::Expr::Constant(hir::Value::Integer(n)), cx);
        self.insts.push(lir::Inst::Copy {
            dst: dst.clone(),
            src,
            layout: lir::Layout::I64,
        });
    }

    /// A new stack slot holding a value with `layout`.
    fn slot(&mut self, layout: lir::Layout) -> lir::Place {
        let slot = lir::SlotId(self.slots.len());
        self.slots.push(layout);
        lir::Place::whole(lir::Base::Slot(slot))
    }
}

/// The instruction for `op`, which isn't one done by a runtime call.
fn lower_binary_op(op: hir::BinaryOp) -> lir::BinaryOp {
    match op {
        hir::BinaryOp::Add(numeric) => lir::BinaryOp::Add(lower_numeric(numeric)),
        hir::BinaryOp::Subtract(numeric) => lir::BinaryOp::Subtract(lower_numeric(numeric)),
        hir::BinaryOp::Multiply(numeric) => lir::BinaryOp::Multiply(lower_numeric(numeric)),
        hir::BinaryOp::Divide => lir::BinaryOp::Divide,
        hir::BinaryOp::Div => lir::BinaryOp::Div,
        hir::BinaryOp::Mod => lir::BinaryOp::Mod,
        hir::BinaryOp::Bitwise(hir::Bitwise::And) => lir::BinaryOp::And(lir::Bits::Integer),
        hir::BinaryOp::Bitwise(hir::Bitwise::Or) => lir::BinaryOp::Or(lir::Bits::Integer),
        hir::BinaryOp::Bitwise(hir::Bitwise::Xor) => lir::BinaryOp::Xor(lir::Bits::Integer),
        hir::BinaryOp::ShiftLeft => lir::BinaryOp::ShiftLeft,
        hir::BinaryOp::ShiftRight => lir::BinaryOp::ShiftRight,
        hir::BinaryOp::Compare(comparison, scalar) => lir::BinaryOp::Compare(
            lower_comparison(comparison),
            match scalar {
                hir::Compared::Scalar(hir::Scalar::Boolean) => lir::Operand::Boolean,
                hir::Compared::Scalar(hir::Scalar::Integer) => {
                    lir::Operand::Numeric(lir::Numeric::Integer)
                }
                hir::Compared::Scalar(hir::Scalar::Approximate) => {
                    lir::Operand::Numeric(lir::Numeric::Approximate)
                }
                hir::Compared::Scalar(
                    hir::Scalar::Text | hir::Scalar::Character | hir::Scalar::Narrow(_),
                )
                | hir::Compared::Defined { .. }
                | hir::Compared::Tuple
                | hir::Compared::Relation
                | hir::Compared::Array { .. } => {
                    unreachable!("compared by a runtime call, or never: no expression is narrow")
                }
                hir::Compared::Parameter(_) => {
                    unreachable!("functions are of instantiated operators")
                }
            },
        ),
        hir::BinaryOp::Concatenate | hir::BinaryOp::ConcatenateArrays => {
            unreachable!("text and arrays are concatenated by a runtime call")
        }
    }
}

fn lower_comparison(comparison: hir::Comparison) -> lir::Comparison {
    match comparison {
        hir::Comparison::Equal => lir::Comparison::Equal,
        hir::Comparison::NotEqual => lir::Comparison::NotEqual,
        hir::Comparison::Less => lir::Comparison::Less,
        hir::Comparison::LessOrEqual => lir::Comparison::LessOrEqual,
        hir::Comparison::Greater => lir::Comparison::Greater,
        hir::Comparison::GreaterOrEqual => lir::Comparison::GreaterOrEqual,
    }
}

/// Where the attributes of a join of tuples with the headings `lhs` and
/// `rhs` are, as [`lir::RuntimeFn::And`] and [`lir::RuntimeFn::Lookup`]
/// take them: constants of, for each attribute of `result`, which operand
/// it comes from and where it is there, and of the common attributes (see
/// [`common_attributes`]), and their count.
struct Joining {
    sources: lir::Place,
    common: lir::Place,
    common_count: lir::Place,
}

impl Joining {
    fn of(lhs: &Shape, rhs: &Shape, result: &hir::Type, cx: &mut Lowering) -> Joining {
        let lhs_tuple = cx.layout(&hir::Type::Tuple(lhs.physical.clone()));
        let rhs_tuple = cx.layout(&hir::Type::Tuple(rhs.physical.clone()));
        let sources = attribute_names(result)
            .map(|name| {
                let (from_rhs, offset) = lhs.position(name).map_or_else(
                    || {
                        let i = rhs
                            .position(name)
                            .expect("each attribute of the result is an operand's");
                        (1, lir::Field::Offset(rhs_tuple.clone(), vec![i]))
                    },
                    |i| (0, lir::Field::Offset(lhs_tuple.clone(), vec![i])),
                );
                lir::Field::Struct(vec![lir::Field::Usize(from_rhs), offset])
            })
            .collect();
        let common = common_attributes(lhs, rhs, cx);
        let common_count = cx.constant(vec![lir::Field::Usize(len(common.len()))]);
        Joining {
            sources: lir::Place::whole(lir::Base::Data(cx.constant(sources))),
            common: lir::Place::whole(lir::Base::Data(cx.constant(common))),
            common_count: lir::Place {
                base: lir::Base::Data(common_count),
                path: vec![0],
            },
        }
    }
}

/// For each attribute the relations read as `lhs` and `rhs` have in common, by the name
/// it is read by, a struct of its offset in a tuple of each and the address of its type's
/// descriptor, as the runtime's joins take them.
fn common_attributes(lhs: &Shape, rhs: &Shape, cx: &mut Lowering) -> Vec<lir::Field> {
    let lhs_tuple = cx.layout(&hir::Type::Tuple(lhs.physical.clone()));
    let rhs_tuple = cx.layout(&hir::Type::Tuple(rhs.physical.clone()));
    let types: Vec<&hir::Type> = lhs.physical.attributes().map(|(_, ty)| ty).collect();
    lhs.names
        .keys()
        .filter_map(|name| {
            let (i, j) = (lhs.position(name)?, rhs.position(name)?);
            Some(lir::Field::Struct(vec![
                lir::Field::Offset(lhs_tuple.clone(), vec![i]),
                lir::Field::Offset(rhs_tuple.clone(), vec![j]),
                lir::Field::Address(cx.descriptor(types[i])),
            ]))
        })
        .collect()
}

/// The `LOOSE_` flags of an operation on relations in the orders `lhs` and `rhs`, each given
/// with the descriptor of its order ([`Lowering::described`]), of a result wanted as
/// `result` says, with `flags`.
fn orders_constant(
    lhs: Order,
    rhs: Order,
    result: Order,
    flags: u64,
    cx: &mut Lowering,
) -> lir::Place {
    // An operand is sorted as its descriptor says, unless it is loose, and a result is sorted
    // as its own descriptor says, unless wanted in no particular order.
    let operand = |order: Order, bit: u64| {
        if matches!(order, Order::Loose | Order::Repeated) {
            bit
        } else {
            0
        }
    };
    let result = if result == Order::Loose {
        lir::LOOSE_RESULT
    } else {
        0
    };
    usize_constant(
        operand(lhs, lir::LOOSE_LHS) | operand(rhs, lir::LOOSE_RHS) | result | flags,
        cx,
    )
}

/// The attributes what `view` reads has in common with what `other` reads, by their own
/// names in it, in the order a join compares them: by the names they are read by.
fn common_owns(view: &View, other: &View) -> Vec<String> {
    view.shape
        .names
        .iter()
        .filter(|(read, _)| other.shape.names.contains_key(*read))
        .map(|(_, own)| own.clone())
        .collect()
}

/// The constant relation `expr` is, if that is all it is.
fn constant_relation(mut expr: &hir::Expr) -> Option<&hir::RelationValue> {
    while let hir::Expr::At(_, inner) = expr {
        expr = inner;
    }
    match expr {
        hir::Expr::Constant(hir::Value::Relation(relation)) => Some(relation),
        _ => None,
    }
}

/// The local variable or binding `expr` reads, if that is all it is.
fn local_read(mut expr: &hir::Expr) -> Option<hir::LocalId> {
    while let hir::Expr::At(_, inner) = expr {
        expr = inner;
    }
    match expr {
        hir::Expr::Local(id) => Some(*id),
        _ => None,
    }
}

/// Whether a join of `lhs` and `rhs` joins a transitive closure.
fn joins_a_closure(lhs: &hir::Expr, rhs: &hir::Expr) -> bool {
    arcs_of_closure(lhs).is_some() || arcs_of_closure(rhs).is_some()
}

/// The relation whose transitive closure `expr` is, if it is one.
fn arcs_of_closure(mut expr: &hir::Expr) -> Option<&hir::Expr> {
    while let hir::Expr::At(_, inner) = expr {
        expr = inner;
    }
    match expr {
        hir::Expr::Tclose(arcs) => Some(arcs),
        _ => None,
    }
}

/// Whether `expr` reads the local variable `id`, directly or through what computes a local
/// constant it reads that is computed when a read first needs it, of `deferred`, as that read
/// may compute it there ([`hir::Local::deferred`]).
fn reads_through_deferred(
    expr: &hir::Expr,
    id: hir::LocalId,
    deferred: &[(hir::LocalId, hir::Expr)],
) -> bool {
    expr.reads_local(id)
        || deferred
            .iter()
            .any(|(constant, value)| value.reads_local(id) && expr.reads_local(*constant))
}

/// Each local variable of `block`, of `locals`, that the block holds to no later than one
/// of its statements, with where that is: the last that mentions it, unless `end`, what is
/// computed from them when they have run, reads it too. A statement in a loop in the block
/// runs again, but is one statement of it, so what it mentions lasts as long as the loop
/// does. One that mentions a local constant the block declares, computed when a read first
/// needs it ([`hir::Local::deferred`]), mentions what its value reads too, as a read
/// computes it there.
fn last_mentioned(
    block: &hir::Block,
    end: Option<&hir::Expr>,
    locals: &[hir::Local],
) -> Vec<(hir::LocalId, usize)> {
    let deferred: Vec<(hir::LocalId, &hir::Expr)> = block
        .statements
        .iter()
        .filter_map(|(_, statement)| match statement {
            hir::Statement::Assign(assignments) => match &assignments[..] {
                [
                    hir::Assignment {
                        target: hir::Target::Local(id),
                        value,
                    },
                ] if locals[id.0].deferred => Some((*id, value)),
                _ => None,
            },
            _ => None,
        })
        .collect();
    block
        .locals
        .iter()
        .filter(|id| end.is_none_or(|end| !end.reads_local(**id)))
        .filter_map(|id| {
            let mentions = |(_, statement): &(hir::Site, hir::Statement)| {
                statement.mentions_local(*id)
                    || deferred.iter().any(|(constant, value)| {
                        value.reads_local(*id) && statement.mentions_local(*constant)
                    })
            };
            let last = block.statements.iter().rposition(mentions)?;
            Some((*id, last))
        })
        .collect()
}

/// The operands of the AND of `lhs` and `rhs`, if it is one of at least three with nothing
/// between them, and each computed on its own and read as it is. A restriction between
/// them is a guard across operands, compiled for the tuples of the ones under it, so it stays
/// where the plan fixed when compiling puts it, and what it restricts is one operand. A closure
/// is found from what it is matched with, and a relation of one tuple can be a lookup, so what
/// has either keeps that plan.
fn several_joined<'e>(lhs: &'e hir::Expr, rhs: &'e hir::Expr) -> Option<Vec<&'e hir::Expr>> {
    let mut leaves = Vec::new();
    and_leaves(lhs, &mut leaves);
    and_leaves(rhs, &mut leaves);
    let plain = |leaf: &hir::Expr| {
        arcs_of_closure(leaf).is_none()
            && !matches!(leaf, hir::Expr::Relation(_, tuples) if tuples.len() == 1)
    };
    (leaves.len() >= 3 && leaves.iter().all(|leaf| plain(leaf))).then_some(leaves)
}

/// The operands of a join of several, each read as it is and computed at the place given, as
/// the plan fixed when compiling reads them, in turn, the next of them being at `next`.
struct PlannedOperands<'a, 'e> {
    views: &'a [View<'e>],
    given: &'a [(lir::Place, Order)],
    next: usize,
}

/// What the runtime's `JoinOperand` says of the operand `view`, in order `order`, among the
/// attributes `names` of the join's result, as they come in a tuple of it: the `JOIN_` flags
/// `flags` too.
fn join_operand(
    view: &View,
    order: Order,
    flags: u64,
    names: &[String],
    cx: &mut Lowering,
) -> lir::Field {
    let tuple = cx.layout(&hir::Type::Tuple(view.shape.physical.clone()));
    let attributes: Vec<lir::Field> = view
        .shape
        .names
        .keys()
        .map(|name| {
            let position = names
                .iter()
                .position(|own| own == name)
                .expect("each attribute of an operand is the result's");
            let at = view
                .shape
                .position(name)
                .expect("a rename reads only attributes the operand has");
            lir::Field::Struct(vec![
                lir::Field::Usize(len(position)),
                lir::Field::Offset(tuple.clone(), vec![at]),
            ])
        })
        .collect();
    let loose = if order == Order::Canonical {
        0
    } else {
        lir::JOIN_LOOSE
    };
    lir::Field::Struct(vec![
        lir::Field::Address(cx.descriptor(&view.ty)),
        lir::Field::Address(cx.constant(attributes)),
        lir::Field::Usize(len(view.shape.names.len())),
        lir::Field::Usize(loose | flags),
    ])
}

/// The operands of `expr`, in the order written, if it is an AND, and of each AND among them,
/// and otherwise `expr` itself.
fn and_leaves<'e>(expr: &'e hir::Expr, leaves: &mut Vec<&'e hir::Expr>) {
    let mut inner = expr;
    while let hir::Expr::At(_, within) = inner {
        inner = within;
    }
    if let hir::Expr::And(lhs, rhs) = inner {
        and_leaves(lhs, leaves);
        and_leaves(rhs, leaves);
    } else {
        leaves.push(expr);
    }
}

/// Whether `expr` is an AND, at a site of its own or not.
fn is_and(expr: &hir::Expr) -> bool {
    let mut inner = expr;
    while let hir::Expr::At(_, within) = inner {
        inner = within;
    }
    matches!(inner, hir::Expr::And(..))
}

/// Whether removing `names` from the relation `view` reads can't make two of its tuples
/// equal: it is an `extend` that wraps each tuple whole in an attribute that stays, as the
/// argument of a `per` parameter is (see [`hir::wrapped`]).
fn keeps_tuples_apart(view: &View<'_>, names: &BTreeSet<String>) -> bool {
    let mut inner = view.inner;
    while let hir::Expr::At(_, within) = inner {
        inner = within;
    }
    let hir::Expr::Extend(_, _, attributes) = inner else {
        return false;
    };
    attributes.iter().any(|(name, value)| {
        hir::is_wrapped(name)
            && !names.contains(name)
            && matches!(value, hir::Expr::Tuple(_) | hir::Expr::Constant(_))
    })
}

/// A constant of `layout` with every byte zero, as a place: a value whose
/// pointers are null.
fn zero_constant(layout: &lir::Layout, cx: &mut Lowering) -> lir::Place {
    lir::Place {
        base: lir::Base::Data(cx.constant(vec![zero_field(layout)])),
        path: vec![0],
    }
}

/// A field of zero bytes laid out as `layout`: null pointers, and no
/// type, for a value of a type in a hierarchy.
fn zero_field(layout: &lir::Layout) -> lir::Field {
    match layout {
        lir::Layout::U8 => lir::Field::U8(0),
        lir::Layout::U16 => lir::Field::U16(0),
        lir::Layout::U32 => lir::Field::U32(0),
        lir::Layout::I64 => lir::Field::I64(0),
        lir::Layout::F64Bits => lir::Field::F64Bits(0),
        lir::Layout::Usize | lir::Layout::Address => lir::Field::Usize(0),
        lir::Layout::Bytes(n) => lir::Field::Bytes(vec![0; *n]),
        lir::Layout::Struct(fields) => lir::Field::Struct(fields.iter().map(zero_field).collect()),
        lir::Layout::Union(variants) => {
            let first = variants
                .first()
                .map_or(lir::Field::Struct(Vec::new()), zero_field);
            lir::Field::Variant(layout.clone(), Box::new(first))
        }
    }
}

/// About what computing `expr` costs, for ranking it against others: one for each
/// expression in it, and a hundred for each call, whose work is unknown, at least one.
fn estimated_cost(expr: &hir::Expr) -> u64 {
    let own = if matches!(expr, hir::Expr::Call(_)) {
        100
    } else {
        1
    };
    let mut cost = own;
    expr.for_each_child(|child, _| cost += estimated_cost(child));
    cost
}

/// The parts of `exprs`, what a loop computes for each tuple, that are the same for every
/// tuple, each once, the largest such: each reads none of the tuple at hand's attributes, nor a
/// local variable that `exprs` bind, and is computed wherever what it is in is, not only if
/// an `if` goes its way, a block's statements reach it or an adapted read takes the form it
/// is in. An operand of an `and` or an `or` can be one, though another may decide it first,
/// as which is computed first is the compiler's choice, learned as it runs (see [`Ranking`]).
/// What is only read, of a variable, a parameter or a tuple at hand, costs no more to read
/// again than to read once, so none is one.
fn invariant_parts(exprs: &[&hir::Expr]) -> Vec<hir::Expr> {
    fn read_only(expr: &hir::Expr) -> bool {
        match expr {
            hir::Expr::Constant(_)
            | hir::Expr::Variable(_)
            | hir::Expr::ComputedConstant(..)
            | hir::Expr::Parameter(_)
            | hir::Expr::Local(_)
            | hir::Expr::Current(..) => true,
            hir::Expr::Extract(tuple, _) => read_only(tuple),
            _ => false,
        }
    }
    fn found(expr: &hir::Expr, bound: &mut Vec<hir::LocalId>, parts: &mut Vec<hir::Expr>) {
        let read = expr.tuple_read(0);
        let invariant = read.attributes.is_empty()
            && !read.whole
            && !bound.iter().any(|&id| expr.reads_local(id));
        if invariant {
            if !read_only(expr) && !parts.contains(expr) {
                parts.push(expr.clone());
            }
            return;
        }
        match expr {
            hir::Expr::If(condition, ..) => found(condition, bound, parts),
            hir::Expr::Run(..) | hir::Expr::AdaptedRead(_) => {}
            hir::Expr::Let(id, value, body) => {
                found(value, bound, parts);
                bound.push(*id);
                found(body, bound, parts);
                bound.pop();
            }
            _ => expr.for_each_child(|child, tuples| {
                if tuples == 0 {
                    found(child, bound, parts);
                }
            }),
        }
    }
    let mut parts = Vec::new();
    for expr in exprs {
        found(expr, &mut Vec::new(), &mut parts);
    }
    parts
}

/// `expr`, at `depth` tuples at hand in from what a loop computes for each tuple, with each
/// of `invariants` at its own level read from the tuple at hand around the loop's, of
/// `heading`, the attribute of its name in `names`; and what reads a tuple further out than
/// the loop's, one further out, past that one.
fn with_invariants(
    expr: hir::Expr,
    invariants: &[hir::Expr],
    names: &[String],
    heading: &hir::Heading,
    depth: usize,
) -> hir::Expr {
    if depth == 0
        && let Some(i) = invariants.iter().position(|part| *part == expr)
    {
        let values = hir::Expr::Current(1, heading.clone());
        return hir::Expr::Extract(Box::new(values), names[i].clone());
    }
    match expr {
        hir::Expr::Current(at, tuple) if at > depth => hir::Expr::Current(at + 1, tuple),
        expr => expr.map_children(|child, tuples| {
            with_invariants(child, invariants, names, heading, depth + tuples)
        }),
    }
}

/// Whether computing `expr` calls a function of the program's, an operator's or a type's,
/// whose failure is traced through it, and leaves the site where it was.
fn makes_calls(expr: &hir::Expr) -> bool {
    if matches!(
        expr,
        hir::Expr::Call(_)
            | hir::Expr::Select(..)
            | hir::Expr::SelectIn(..)
            | hir::Expr::TreatAs(..)
            | hir::Expr::Run(..)
    ) {
        return true;
    }
    let mut calls = false;
    expr.for_each_child(|child, _| calls |= makes_calls(child));
    calls
}

/// A pointer-sized constant `n`, as a place.
fn usize_constant(n: u64, cx: &mut Lowering) -> lir::Place {
    lir::Place {
        base: lir::Base::Data(cx.constant(vec![lir::Field::Usize(n)])),
        path: vec![0],
    }
}

/// A byte constant `n`, as a place.
fn u8_constant(n: u8, cx: &mut Lowering) -> lir::Place {
    lir::Place {
        base: lir::Base::Data(cx.constant(vec![lir::Field::U8(n)])),
        path: vec![0],
    }
}

/// The attribute names of a tuple or relation type, in name order.
fn attribute_names(ty: &hir::Type) -> impl Iterator<Item = &str> {
    let (hir::Type::Tuple(heading) | hir::Type::Relation(heading)) = ty else {
        unreachable!("only tuples and relations have attributes");
    };
    heading.attributes().map(|(name, _)| name)
}

/// The layout of the result of an operator on `operand`, which has its
/// operand's type: no operator but a comparison takes indexes.
fn operand_layout(operand: lir::Operand) -> lir::Layout {
    match operand {
        lir::Operand::Boolean => lir::Layout::U8,
        lir::Operand::Numeric(lir::Numeric::Integer) => lir::Layout::I64,
        lir::Operand::Numeric(lir::Numeric::Approximate) => lir::Layout::F64Bits,
        lir::Operand::Usize => unreachable!("only comparisons take indexes"),
    }
}

/// The type of what a runtime function makes for a value of `ty`: `ty`
/// itself, with each value of a type of the program's own made of its
/// components as they are stored, narrower or not, but for an array of
/// values of a type in a hierarchy, whose most specific types only compiled
/// code chooses, the array of their components, which
/// [`FunctionBuilder::made_of_components`] then makes them of.
fn made_by_runtime(ty: &hir::Type) -> hir::Type {
    match ty {
        hir::Type::Array(element) if element.root().is_some() => {
            let hir::Type::Defined(defined) = &**element else {
                unreachable!("a runtime function makes no array of a union type's values");
            };
            hir::Type::Array(Box::new(hir::Type::Tuple(defined.components.clone())))
        }
        _ => ty.clone(),
    }
}

fn lower_numeric(numeric: hir::Numeric) -> lir::Numeric {
    match numeric {
        hir::Numeric::Integer => lir::Numeric::Integer,
        hir::Numeric::Approximate => lir::Numeric::Approximate,
        hir::Numeric::Parameter(_) => unreachable!("functions are of instantiated operators"),
    }
}

fn scalar_layout(scalar: hir::Scalar) -> lir::Layout {
    match scalar {
        hir::Scalar::Boolean | hir::Scalar::Narrow(hir::Width::U8) => lir::Layout::U8,
        hir::Scalar::Integer => lir::Layout::I64,
        hir::Scalar::Approximate => lir::Layout::F64Bits,
        hir::Scalar::Text | hir::Scalar::Character => {
            lir::Layout::Struct(vec![lir::Layout::Address, lir::Layout::Usize])
        }
        hir::Scalar::Narrow(hir::Width::U16) => lir::Layout::U16,
        hir::Scalar::Narrow(hir::Width::U32) => lir::Layout::U32,
    }
}

/// The field of the integer `n`, in the range of `width`, stored in it.
fn narrow_field(width: hir::Width, n: i64) -> lir::Field {
    let in_range = "a value satisfies its type's constraints";
    match width {
        hir::Width::U8 => lir::Field::U8(u8::try_from(n).expect(in_range)),
        hir::Width::U16 => lir::Field::U16(u16::try_from(n).expect(in_range)),
        hir::Width::U32 => lir::Field::U32(u32::try_from(n).expect(in_range)),
    }
}

/// LIR's name for `width`.
fn lir_width(width: hir::Width) -> lir::Width {
    match width {
        hir::Width::U8 => lir::Width::U8,
        hir::Width::U16 => lir::Width::U16,
        hir::Width::U32 => lir::Width::U32,
    }
}

/// The bounds a read-only operator's value says each integer parameter is within when
/// it is true: the lowest and the highest, if it says one.
type IntegerBounds = BTreeMap<String, (Option<i64>, Option<i64>)>;

/// [`IntegerBounds`] of each read-only operator of `program`, of those that are a type's
/// constraints what it can store its components by (see [`Families::stored`]).
fn integer_bounds(program: &hir::Program) -> HashMap<hir::OperatorId, IntegerBounds> {
    program
        .operators
        .iter()
        .enumerate()
        .filter_map(|(i, operator)| {
            let hir::OperatorKind::ReadOnly { value, .. } = &operator.kind else {
                return None;
            };
            let mut bounds = IntegerBounds::new();
            bounded_by(value, &mut bounds);
            (!bounds.is_empty()).then_some((hir::OperatorId(i), bounds))
        })
        .collect()
}

/// Add to `bounds` what `condition`, true, says of its integer parameters: each operand
/// of an `and` does, and each comparison of a parameter with a constant.
fn bounded_by(condition: &hir::Expr, bounds: &mut IntegerBounds) {
    use hir::Comparison as C;
    fn at(expr: &hir::Expr) -> &hir::Expr {
        match expr {
            hir::Expr::At(_, inner) => at(inner),
            expr => expr,
        }
    }
    match at(condition) {
        hir::Expr::Connective(hir::Connective::And, operands) => {
            for operand in operands {
                bounded_by(operand, bounds);
            }
        }
        hir::Expr::Binary(
            hir::BinaryOp::Compare(comparison, hir::Compared::Scalar(hir::Scalar::Integer)),
            lhs,
            rhs,
        ) => {
            let (name, n, comparison) = match (at(lhs), at(rhs)) {
                (hir::Expr::Parameter(name), hir::Expr::Constant(hir::Value::Integer(n))) => {
                    (name, *n, *comparison)
                }
                (hir::Expr::Constant(hir::Value::Integer(n)), hir::Expr::Parameter(name)) => {
                    let flipped = match comparison {
                        C::Less => C::Greater,
                        C::LessOrEqual => C::GreaterOrEqual,
                        C::Greater => C::Less,
                        C::GreaterOrEqual => C::LessOrEqual,
                        same => *same,
                    };
                    (name, *n, flipped)
                }
                _ => return,
            };
            let (low, high) = bounds.entry(name.clone()).or_default();
            let raised = |low: &mut Option<i64>, to: Option<i64>| {
                *low = (*low).max(to);
            };
            let lowered = |high: &mut Option<i64>, to: Option<i64>| {
                if let Some(to) = to {
                    *high = Some(high.map_or(to, |high| high.min(to)));
                }
            };
            match comparison {
                C::Less => lowered(high, n.checked_sub(1)),
                C::LessOrEqual => lowered(high, Some(n)),
                C::Greater => raised(low, n.checked_add(1)),
                C::GreaterOrEqual => raised(low, Some(n)),
                C::Equal => {
                    raised(low, Some(n));
                    lowered(high, Some(n));
                }
                C::NotEqual => {}
            }
        }
        _ => {}
    }
}

/// The relation a load of `relation`, of `heading`, ordered by `order`,
/// sorts, and each scalar it is ordered by, as [`hir::compare`] orders
/// values. A value of a type of the program's own orders
/// by the components its order compares, in turn, a descending one the
/// other way. Of an order in another possible representation than the
/// stored one, the runtime can't call the conversion to it, so the
/// components it computes are an attribute of each tuple, which no source
/// can name, computed by an `Extend` of the relation, one more for each
/// conversion of another's components, and the runtime orders by those,
/// each a call of an operator of `program`.
fn sorted(
    program: &hir::Program,
    relation: &hir::Expr,
    heading: &hir::Heading,
    order: &[hir::OrderItem],
    families: &Families,
) -> Sorted {
    let mut keys = Keys::default();
    for item in order {
        let ty = heading
            .get(&item.attribute)
            .expect("sema orders by the relation's attributes");
        let key = Key {
            attribute: item.attribute.clone(),
            path: Vec::new(),
            ty: ty.clone(),
            descending: item.descending,
        };
        keys.ordered_by(key);
    }
    let levels = keys.converted.iter().map(|c| c.level).max().unwrap_or(0);
    let (mut relation, mut heading) = (relation.clone(), heading.clone());
    for level in 1..=levels {
        let computed: Vec<&Conversion> =
            keys.converted.iter().filter(|c| c.level == level).collect();
        let types = computed
            .iter()
            .map(|c| (c.name.clone(), hir::Type::Tuple(c.components.clone())))
            .collect();
        let values = computed
            .iter()
            .map(|c| (c.name.clone(), c.computed(&heading, program)))
            .collect();
        let added = hir::Heading::new(types);
        heading = hir::Heading::new(
            heading
                .attributes()
                .chain(added.attributes())
                .map(|(a, ty)| (a.to_string(), ty.clone()))
                .collect(),
        );
        relation = hir::Expr::Extend(Box::new(relation), added, values);
    }
    let leaves = keys
        .leaves
        .into_iter()
        .map(|key| {
            let mut path = vec![
                heading
                    .attributes()
                    .position(|(a, _)| a == key.attribute)
                    .expect("a key is an attribute of the tuples"),
            ];
            let mut ty = heading.get(&key.attribute).expect("a key is an attribute");
            // As it is stored, if a type's constraints let it be narrower.
            let mut stored = None;
            for step in &key.path {
                stored = match ty {
                    hir::Type::Defined(defined) => families
                        .stored(defined)
                        .and_then(|stored| stored.get(step).cloned()),
                    _ => None,
                };
                let fields = ty.fields().expect("a key's path is through components");
                path.push(
                    fields
                        .attributes()
                        .position(|(c, _)| c == step)
                        .expect("a key's path names components"),
                );
                ty = fields.get(step).expect("a key's path names components");
            }
            Leaf {
                path,
                ty: stored.unwrap_or(key.ty),
                descending: key.descending,
            }
        })
        .collect();
    Sorted {
        relation,
        heading,
        leaves,
    }
}

/// The relation a load sorts, with `heading`, and each scalar it orders
/// its tuples by (see [`sorted`]).
struct Sorted {
    relation: hir::Expr,
    heading: hir::Heading,
    leaves: Vec<Leaf>,
}

/// A scalar a load orders its tuples by: where it is in a tuple, its type,
/// and whether it is descending.
struct Leaf {
    path: Vec<usize>,
    ty: hir::Type,
    descending: bool,
}

/// What a load's tuples are ordered by, as [`sorted`] works it out.
#[derive(Default)]
struct Keys {
    /// Each scalar, in order.
    leaves: Vec<Key>,
    /// The values converted to another possible representation of their
    /// type, for the scalars among its components.
    converted: Vec<Conversion>,
}

impl Keys {
    /// The scalars a value, `key`, is ordered by: itself, or, of a type of
    /// the program's own, the components its order compares, each in turn,
    /// converted, if they aren't the stored ones.
    fn ordered_by(&mut self, key: Key) {
        let hir::Type::Defined(defined) = &key.ty else {
            self.leaves.push(key);
            return;
        };
        let (order, components) = defined
            .ordered_by()
            .expect("sema orders only by ordered types");
        let (attribute, path) = match order.from {
            None => (key.attribute.clone(), key.path.clone()),
            Some(from) => {
                let level = 1 + self
                    .converted
                    .iter()
                    .find(|c| c.name == key.attribute)
                    .map_or(0, |c| c.level);
                let name = hir::wrapped(&format!("key {}", self.converted.len() + 1));
                self.converted.push(Conversion {
                    name: name.clone(),
                    level,
                    of: key.clone(),
                    stored: defined.components.clone(),
                    from,
                    components: components.clone(),
                });
                (name, Vec::new())
            }
        };
        for item in &order.items {
            let ty = components
                .get(&item.attribute)
                .expect("an order names its representation's components");
            let mut path = path.clone();
            path.push(item.attribute.clone());
            self.ordered_by(Key {
                attribute: attribute.clone(),
                path,
                ty: ty.clone(),
                descending: key.descending != item.descending,
            });
        }
    }
}

/// A value a load's tuples are ordered by, of type `ty`, `descending` or
/// not: the attribute `attribute`, or a value inside it, `path` its
/// components, or attributes, one inside another.
#[derive(Clone)]
struct Key {
    attribute: String,
    path: Vec<String>,
    ty: hir::Type,
    descending: bool,
}

/// The components `components` that the conversion `from` computes from
/// the `stored` ones of the value `of`, as the attribute `name` of each
/// tuple, which the `Extend` numbered `level`, counting from 1, computes,
/// after those of lower levels, whose attributes its own are of.
struct Conversion {
    name: String,
    level: usize,
    of: Key,
    stored: hir::Heading,
    from: hir::OperatorId,
    components: hir::Heading,
}

impl Conversion {
    /// The tuple of the components, computed from the tuple at hand, of
    /// `heading`, by the conversion, an operator of `program`.
    fn computed(&self, heading: &hir::Heading, program: &hir::Program) -> hir::Expr {
        let at_hand = hir::Expr::Current(0, heading.clone());
        let mut value = hir::Expr::Extract(Box::new(at_hand), self.of.attribute.clone());
        let mut ty = heading
            .get(&self.of.attribute)
            .expect("a conversion's value is an attribute of the tuples");
        for step in &self.of.path {
            let fields = match ty {
                hir::Type::Defined(_) => hir::Expr::Components(Box::new(value)),
                _ => value,
            };
            value = hir::Expr::Extract(Box::new(fields), step.clone());
            ty = ty
                .fields()
                .and_then(|fields| fields.get(step))
                .expect("a key's path names components");
        }
        let components = hir::Expr::Components(Box::new(value));
        let args = self
            .stored
            .attributes()
            .map(|(k, _)| {
                let arg = hir::Expr::Extract(Box::new(components.clone()), k.to_string());
                (k.to_string(), arg)
            })
            .collect();
        hir::Expr::Call(program.call_of(self.from, args))
    }
}

/// Whether a value of type `ty` is or has a value of a type of the
/// program's own with constraints, or keys of components.
fn constrained(ty: &hir::Type) -> bool {
    match ty {
        hir::Type::Tuple(heading) => heading.attributes().any(|(_, ty)| constrained(ty)),
        hir::Type::Defined(defined) => {
            defined.is_constrained()
                || defined
                    .components
                    .attributes()
                    .any(|(_, ty)| constrained(ty))
        }
        _ => false,
    }
}

/// Whether a value of type `ty`, which a CSV file's record gives, can be
/// skipped, for its call's errors: whether a check of it can fail, as
/// it is or has a value of a type of the program's own with constraints,
/// or keys of components, or with subtypes defined by constraints, of
/// which it may be of two.
fn skippable(ty: &hir::Type, families: &Families) -> bool {
    match ty {
        hir::Type::Tuple(heading) => heading.attributes().any(|(_, ty)| skippable(ty, families)),
        hir::Type::Defined(defined) => {
            constrained(ty)
                || !families.constrained(defined).is_empty()
                || defined
                    .components
                    .attributes()
                    .any(|(_, ty)| skippable(ty, families))
        }
        _ => false,
    }
}

/// `name`, of a value, and `attribute`, of it, as `t.c` reads that.
fn dotted(name: &str, attribute: &str) -> String {
    if name.is_empty() {
        attribute.to_string()
    } else {
        format!("{name}.{attribute}")
    }
}

/// Where a built-in's errors go, and what its runtime function is given to
/// make them (see [`FunctionBuilder::errors`]).
struct Errors {
    value: lir::Place,
    given: lir::Place,
}

/// The CSV file `load_csv` read: its path, and the line each record starts
/// on, an array of them.
struct InFile {
    path: lir::Place,
    lines: lir::Place,
}

/// Components a constraint is checked of: their heading, a value's or a
/// representation's, where they are, laid out as the tuple of them, the
/// descriptor a failure writes them with, and the name of their type.
struct CheckedComponents<'c> {
    components: &'c hir::Heading,
    value: &'c lir::Place,
    descriptor: &'c lir::Place,
    ty: &'c str,
}

/// What a value whose components are checked against its type's keys
/// and constraints is: one selected, or one `load_csv` read from the record
/// a place laid out as [`lir::Layout::record`] describes, which is skipped
/// for a constraint it breaks, if its call gives errors.
enum Checking {
    Selected,
    Read {
        record: lir::Place,
        skip: Option<Skip>,
    },
}

/// What happens where a check of a value finds it broken: the program
/// stops with the failure, or, of one a CSV file's record gives whose call
/// gives errors, the runtime function `function` adds its error, given
/// `args`, and the record is skipped, on to the block `to`.
enum Broken {
    Fail(Box<lir::Failure>),
    Skip {
        function: lir::RuntimeFn,
        args: Vec<lir::Place>,
        to: lir::BlockId,
    },
}

/// Where a record of a CSV file whose value breaks a constraint goes:
/// its error to `errors`, the runtime's `Errors`, and on to the block
/// `to`, after the record.
#[derive(Clone)]
struct Skip {
    errors: lir::Place,
    to: lir::BlockId,
}

/// The places a key check gives the failure it makes: the relation, its
/// type's descriptor, the descriptor of its projection on the key, and the
/// positions of two tuples that share their values for it.
struct Failed {
    value: lir::Place,
    descriptor: lir::Place,
    key: lir::Place,
    pair: lir::Place,
}

/// A record of a CSV file, at `index`, from 0, among its records, and
/// where it goes if it is skipped.
struct Record<'f> {
    file: &'f InFile,
    index: lir::Place,
    skip: Option<Skip>,
}

/// A value in a record `at`, as `load_csv` reads it: where it goes, in the
/// array the statement fills, where `load_csv` read it, and its name, as
/// `t.c` reads it.
#[derive(Clone)]
struct Reading<'r> {
    dst: lir::Place,
    src: lir::Place,
    name: String,
    at: &'r Record<'r>,
}

/// How the values of the program's types are laid out. A value of a type
/// in a hierarchy, under a union type or a regular type that a type is
/// defined as a subtype of by a constraint, is laid out as every value of
/// the type at its top, its root: the address of its own type's
/// descriptor, then room for the components of any of the types under the
/// root with a stored representation of their own, its variants, so that a
/// value of a subtype is already a value of its supertypes. A type defined
/// by constraints has the components, and the variant, of the type its
/// values are stored as. A value of any other type of the program's own
/// is laid out as the tuple of its components.
struct Families {
    /// Each root's types a value can be of, in the order the program
    /// declares them.
    members: HashMap<String, Vec<hir::Defined>>,
    /// Each root's types with a stored representation of their own, in the
    /// order the program declares them.
    variants: HashMap<String, Vec<hir::Defined>>,
    /// Each regular type's subtypes defined by constraints, by its name.
    constrained: HashMap<String, Vec<hir::Defined>>,
    /// Each root's room for its values' components: a
    /// [`lir::Layout::Union`] of its variants' components, in that order.
    unions: HashMap<String, lir::Layout>,
    /// What each read-only operator's value says of its integer parameters.
    bounds: HashMap<hir::OperatorId, IntegerBounds>,
}

impl Families {
    /// The hierarchies of the types `subtypes`, each with its immediate
    /// subtypes.
    fn new(
        subtypes: &BTreeMap<String, Vec<hir::Type>>,
        bounds: HashMap<hir::OperatorId, IntegerBounds>,
    ) -> Self {
        fn collect(
            name: &str,
            subtypes: &BTreeMap<String, Vec<hir::Type>>,
            members: &mut Vec<hir::Defined>,
        ) {
            for subtype in subtypes.get(name).into_iter().flatten() {
                match subtype {
                    hir::Type::Defined(defined) => {
                        members.push((**defined).clone());
                        collect(&defined.name, subtypes, members);
                    }
                    hir::Type::Dummy(dummy) => collect(&dummy.name, subtypes, members),
                    _ => unreachable!("a type's subtypes are the program's own types"),
                }
            }
        }
        let mut constrained: HashMap<String, Vec<hir::Defined>> = HashMap::new();
        for (name, list) in subtypes {
            for subtype in list {
                if let hir::Type::Defined(defined) = subtype
                    && defined.specialization.is_some()
                {
                    constrained
                        .entry(name.clone())
                        .or_default()
                        .push((**defined).clone());
                }
            }
        }
        let mut members = HashMap::new();
        for root in subtypes.values().flatten().filter_map(hir::Type::root) {
            if members.contains_key(root) {
                continue;
            }
            let mut found = Vec::new();
            // A regular root is one of its own types, as its constrained
            // subtypes' supertype.
            if let Some(regular) = constrained
                .get(root)
                .and_then(|list| list.first())
                .and_then(|subtype| subtype.specialization.as_ref())
            {
                found.push((*regular.supertype).clone());
            }
            collect(root, subtypes, &mut found);
            members.insert(root.to_string(), found);
        }
        let variants = members
            .iter()
            .map(|(root, list)| {
                let own = list
                    .iter()
                    .filter(|d| d.specialization.is_none())
                    .cloned()
                    .collect();
                (root.clone(), own)
            })
            .collect();
        let mut families = Families {
            members,
            variants,
            constrained,
            unions: HashMap::new(),
            bounds,
        };
        let roots: Vec<String> = families.variants.keys().cloned().collect();
        for root in roots {
            families.lay_out(&root);
        }
        families
    }

    /// Lay out the hierarchy under `root`, after those its variants'
    /// components are of, which contain none of its own types.
    fn lay_out(&mut self, root: &str) {
        fn roots<'t>(ty: &'t hir::Type, out: &mut Vec<&'t str>) {
            if let Some(root) = ty.root() {
                out.push(root);
            } else if let Some(fields) = ty.fields() {
                fields.attributes().for_each(|(_, ty)| roots(ty, out));
            }
        }
        if self.unions.contains_key(root) {
            return;
        }
        let variants = self.variants[root].clone();
        let mut inside = Vec::new();
        for variant in &variants {
            variant
                .components
                .attributes()
                .for_each(|(_, ty)| roots(ty, &mut inside));
        }
        for nested in inside {
            self.lay_out(nested);
        }
        let layouts = variants
            .iter()
            .map(|variant| self.layout(&hir::Type::Tuple(variant.components.clone())))
            .collect();
        self.unions
            .insert(root.to_string(), lir::Layout::Union(layouts));
    }

    /// The components of `defined` as its values store them, if that isn't as they
    /// are: an integer its constraints keep in a range that fits fewer bytes takes those
    /// (RM Prescription 4 keeps physical representations out of the language). Not in
    /// a hierarchy, whose values are all laid out as its root's.
    fn stored(&self, defined: &hir::Defined) -> Option<hir::Heading> {
        if defined.constraints.is_empty() || defined.specialized || !defined.supertypes.is_empty() {
            return None;
        }
        let mut narrowed = false;
        let stored = defined.components.attributes().map(|(name, ty)| {
            let mut ty = ty.clone();
            if ty == hir::Type::Scalar(hir::Scalar::Integer) {
                let (mut low, mut high) = (None::<i64>, None::<i64>);
                for constraint in &defined.constraints {
                    if let Some((from, to)) = self
                        .bounds
                        .get(&constraint.operator)
                        .and_then(|bounds| bounds.get(name))
                    {
                        low = low.max(*from);
                        high = match (high, *to) {
                            (Some(a), Some(b)) => Some(a.min(b)),
                            (a, b) => a.or(b),
                        };
                    }
                }
                if let (Some(low), Some(high)) = (low, high)
                    && let Some(width) = hir::Width::of_range(low, high)
                {
                    narrowed = true;
                    ty = hir::Type::Scalar(hir::Scalar::Narrow(width));
                }
            }
            (name.to_string(), ty)
        });
        let stored = hir::Heading::new(stored.collect());
        narrowed.then_some(stored)
    }

    /// The layout of a value of type `ty`.
    fn layout(&self, ty: &hir::Type) -> lir::Layout {
        if let Some(root) = ty.root() {
            let union = self
                .unions
                .get(root)
                .expect("every hierarchy is laid out first");
            return lir::Layout::Struct(vec![lir::Layout::Address, union.clone()]);
        }
        match ty {
            hir::Type::Scalar(scalar) => scalar_layout(*scalar),
            hir::Type::Defined(defined) if let Some(stored) = self.stored(defined) => {
                self.layout(&hir::Type::Tuple(stored))
            }
            hir::Type::Relation(_) | hir::Type::Array(_) => {
                lir::Layout::Struct(vec![lir::Layout::Address, lir::Layout::Usize])
            }
            hir::Type::Tuple(_) | hir::Type::Defined(_) => {
                let fields = ty.fields().expect("tuples and types' values have fields");
                lir::Layout::Struct(fields.attributes().map(|(_, ty)| self.layout(ty)).collect())
            }
            hir::Type::Dummy(_) => unreachable!("a dummy type is in a hierarchy"),
            hir::Type::Parameter(_) => unreachable!("functions are of instantiated operators"),
        }
    }

    /// Whether a value of type `ty` points to memory: whether it is or has
    /// a text, a relation or an array, or, in a hierarchy, could.
    fn points(&self, ty: &hir::Type) -> bool {
        if let Some(root) = ty.root() {
            return self.variants[root].iter().any(|variant| {
                variant
                    .components
                    .attributes()
                    .any(|(_, ty)| self.points(ty))
            });
        }
        match ty {
            hir::Type::Scalar(scalar) => {
                matches!(scalar, hir::Scalar::Text | hir::Scalar::Character)
            }
            hir::Type::Relation(_) | hir::Type::Array(_) => true,
            hir::Type::Tuple(_) | hir::Type::Defined(_) => ty
                .fields()
                .expect("tuples and types' values have fields")
                .attributes()
                .any(|(_, ty)| self.points(ty)),
            hir::Type::Dummy(_) => unreachable!("a dummy type is in a hierarchy"),
            hir::Type::Parameter(_) => unreachable!("functions are of instantiated operators"),
        }
    }

    /// The types a value can be of that are `ty`, in a hierarchy, or one of
    /// its subtypes.
    fn members_under(&self, ty: &hir::Type) -> Vec<hir::Defined> {
        let root = ty.root().expect("a type in a hierarchy");
        self.members[root]
            .iter()
            .filter(|member| hir::Type::Defined(Box::new((*member).clone())).is_subtype_of(ty))
            .cloned()
            .collect()
    }

    /// The subtypes of the regular type `defined` defined by constraints,
    /// which a value of it may be of.
    fn constrained(&self, defined: &hir::Defined) -> &[hir::Defined] {
        self.constrained
            .get(&defined.name)
            .map_or(&[], Vec::as_slice)
    }

    /// Where the components of a value of the type of the program's own
    /// `defined` are, in its layout: in a hierarchy, its variant of the
    /// root's room, after its type's descriptor's address, which a type
    /// defined by constraints shares with the type whose components it
    /// has; otherwise, all of it.
    fn components_path(&self, defined: &hir::Defined) -> Vec<usize> {
        let ty = hir::Type::Defined(Box::new(defined.clone()));
        let Some(root) = ty.root() else {
            return Vec::new();
        };
        let stored = defined.stored_as();
        let variant = self.variants[root]
            .iter()
            .position(|variant| variant.name == stored.name)
            .expect("a type in a hierarchy is stored as one of its root's variants");
        vec![1, variant]
    }
}

/// The parameters of `op`, instantiated with `types`.
fn params_of(op: &hir::Operator, types: &[hir::Type]) -> BTreeMap<String, hir::Parameter> {
    op.params
        .iter()
        .map(|(name, p)| {
            let parameter = hir::Parameter {
                ty: p.ty.instantiated(types),
                mode: p.mode,
            };
            (name.clone(), parameter)
        })
        .collect()
}

/// The parameter struct of a user operator, if it has parameters: each
/// `const` one's value, and the address of each other one's variable.
fn params_layout(
    params: &BTreeMap<String, hir::Parameter>,
    families: &Families,
) -> Option<lir::Layout> {
    let field = |p: &hir::Parameter| match p.mode {
        hir::Mode::Const => families.layout(&p.ty),
        hir::Mode::Var | hir::Mode::Out => lir::Layout::Address,
    };
    (!params.is_empty()).then(|| lir::Layout::Struct(params.values().map(field).collect()))
}

/// Of the argument struct at `args` for `params`, each `const` parameter's
/// place and type that points to a text, relation or array.
fn held_arguments(
    params: &BTreeMap<String, hir::Parameter>,
    args: &lir::Place,
    cx: &mut Lowering,
) -> Vec<(lir::Place, hir::Type)> {
    let mut held = Vec::new();
    for (i, parameter) in params.values().enumerate() {
        if parameter.mode == hir::Mode::Const && cx.points(&parameter.ty) {
            let mut place = args.clone();
            place.path.push(i);
            held.push((place, parameter.ty.clone()));
        }
    }
    held
}

fn len(n: usize) -> u64 {
    u64::try_from(n).expect("lengths fit in 64 bits")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `statements`, each at a site of its own, on its own line.
    fn sited(statements: Vec<hir::Statement>) -> Vec<(hir::Site, hir::Statement)> {
        statements
            .into_iter()
            .enumerate()
            .map(|(i, statement)| {
                let site = hir::Site {
                    file: hir::FileId(0),
                    line: i + 1,
                    column: 5,
                };
                (site, statement)
            })
            .collect()
    }

    /// The program lowered, as if its source were `test.trendy`, without
    /// the sites it sets, saves and restores, which only
    /// [`statements_and_values_set_their_sites`] is about: a function a
    /// failure can pass through saves the one it is entered at, for its
    /// trace.
    fn lower_program(program: &hir::Program) -> lir::Program {
        let mut lowered = lower(program);
        for block in lowered.functions.iter_mut().flat_map(|f| &mut f.blocks) {
            block.insts.retain(|i| {
                !matches!(
                    i,
                    lir::Inst::Site(_) | lir::Inst::SaveSite { .. } | lir::Inst::RestoreSite { .. }
                )
            });
        }
        lowered
    }

    fn integer() -> hir::Type {
        hir::Type::Scalar(hir::Scalar::Integer)
    }

    #[test]
    fn a_read_computes_a_constant_unless_that_is_done() {
        let read = hir::Expr::ComputedConstant(hir::ConstantId(0), Box::new(integer()));
        let mut program = program(
            vec![
                debug_value(),
                user("main", &[], vec![call(0, vec![("v", read)])]),
            ],
            Vec::new(),
        );
        program.constants = vec![hir::Constant {
            name: "c".to_string(),
            ty: integer(),
            keys: Vec::new(),
            locals: Vec::new(),
            value: hir::Expr::Constant(hir::Value::Integer(1)),
        }];
        let lowered = lower(&program);
        let function = |name: &str| {
            lowered
                .functions
                .iter()
                .find(|function| function.name == name)
                .unwrap_or_else(|| panic!("`{name}` is lowered"))
        };
        // The read branches on whether it is computed, and only if it isn't,
        // calls the function, then goes on as it would have.
        let main = function("main");
        let branch = main
            .blocks
            .iter()
            .find_map(|block| match &block.terminator {
                lir::Terminator::Branch {
                    condition,
                    then,
                    otherwise,
                } => Some((condition, *then, *otherwise)),
                _ => None,
            });
        let (computed, then, otherwise) = branch.expect("the read branches");
        let compute = &main.blocks[otherwise.0];
        let called = compute.insts.iter().find_map(|inst| match inst {
            lir::Inst::Call { function, .. } => Some(*function),
            _ => None,
        });
        let called = called.expect("computing it calls its function");
        assert_eq!(lowered.functions[called.0].name, "constant c");
        assert_eq!(compute.terminator, lir::Terminator::Jump(then));
        // Its function says it is done, as the last thing it does.
        let sets: Vec<&lir::Inst> = function("constant c")
            .blocks
            .iter()
            .flat_map(|block| &block.insts)
            .filter(|inst| matches!(inst, lir::Inst::Copy { dst, .. } if dst == computed))
            .collect();
        assert_eq!(sets.len(), 1, "{sets:?}");
    }

    fn user(name: &str, params: &[&str], body: Vec<hir::Statement>) -> hir::Operator {
        hir::Operator {
            versions: Vec::new(),
            constraints: Vec::new(),
            can_fail: true,
            name: name.to_string(),
            types: Vec::new(),
            params: params
                .iter()
                .map(|p| ((*p).to_string(), hir::Parameter::constant(integer())))
                .collect(),
            kind: hir::OperatorKind::Update {
                locals: Vec::new(),
                body: hir::Block {
                    statements: sited(body),
                    locals: Vec::new(),
                },
            },
        }
    }

    #[test]
    fn constraints_are_checked_after_what_assigns_their_variables() {
        // A constraint on `stock` alone: assigning `stock` checks its part,
        // by a function of its own, and assigning `other` doesn't.
        let assign = |id, n| {
            hir::Statement::Assign(vec![hir::Assignment {
                target: hir::Target::Variable(hir::VariableId(id)),
                value: hir::Expr::Constant(hir::Value::Integer(n)),
            }])
        };
        let mut program = program(
            vec![user("main", &[], vec![assign(0, 4), assign(1, 0)])],
            vec![
                variable("stock", hir::Value::Integer(5)),
                variable("other", hir::Value::Integer(1)),
            ],
        );
        program.constraints.push(hir::VariableConstraint {
            name: "stocked".to_string(),
            variables: [hir::VariableId(0)].into(),
            parts: vec![hir::ConstraintPart {
                condition: hir::Expr::Binary(
                    hir::BinaryOp::Compare(
                        hir::Comparison::Greater,
                        hir::Compared::Scalar(hir::Scalar::Integer),
                    ),
                    Box::new(hir::Expr::Variable(hir::VariableId(0))),
                    Box::new(hir::Expr::Constant(hir::Value::Integer(0))),
                ),
                breaking: None,
                locals: Vec::new(),
                written: "stock > 0".to_string(),
            }],
            written: "stock > 0".to_string(),
            site: hir::Site {
                file: hir::FileId(0),
                line: 1,
                column: 1,
            },
            checked_at_start: false,
        });
        let lowered = lower_program(&program);
        let names: Vec<&str> = lowered.functions.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, ["main", "constraint stocked", "__entry", "message"]);
        let checks = lowered.functions[0]
            .blocks
            .iter()
            .flat_map(|b| &b.insts)
            .filter(|inst| matches!(inst, lir::Inst::Call { function, .. } if function.0 == 1))
            .count();
        assert_eq!(checks, 1);
        let failures: Vec<&lir::Failure> = lowered.functions[0]
            .blocks
            .iter()
            .filter_map(|b| match &b.terminator {
                lir::Terminator::Fail(failure) => Some(&**failure),
                _ => None,
            })
            .collect();
        assert!(
            matches!(failures[..], [lir::Failure::ConstraintViolated { .. }]),
            "{failures:#?}"
        );
    }

    fn debug_value() -> hir::Operator {
        hir::Operator {
            versions: Vec::new(),
            constraints: Vec::new(),
            can_fail: true,
            name: "debug_value".to_string(),
            types: hir::Builtin::Update(hir::UpdateBuiltin::DebugValue).types(),
            params: hir::Builtin::Update(hir::UpdateBuiltin::DebugValue).params(&library()),
            kind: hir::OperatorKind::Builtin(hir::Builtin::Update(hir::UpdateBuiltin::DebugValue)),
        }
    }

    /// The library's types as sema gives them: `code_point`, `byte` and
    /// `file_path`, of their stored components alone, and `error`.
    fn library() -> hir::Library {
        hir::Library {
            error: Box::new(hir::Dummy {
                name: hir::Library::ERROR.to_string(),
                supertypes: Vec::new(),
            }),
            code_point: Box::new(library_type(
                hir::Library::CODE_POINT,
                hir::Library::number_components(),
            )),
            byte: Box::new(library_type(
                hir::Library::BYTE,
                hir::Library::number_components(),
            )),
            file_path: Box::new(library_type(
                hir::Library::FILE_PATH,
                hir::Library::path_components(),
            )),
        }
    }

    /// The type `name` of the library, of a number alone.
    fn library_type(name: &str, components: hir::Heading) -> hir::Defined {
        hir::Defined {
            name: name.to_string(),
            unit: String::new(),
            components,
            representations: Vec::new(),
            constraints: Vec::new(),
            keys: BTreeMap::new(),
            order: None,
            supertypes: Vec::new(),
            specialized: false,
            specialization: None,
        }
    }

    /// A program of `operators` and `variables`, with what the entry's
    /// table of errors needs of the units that come with the compiler, as
    /// sema gives it: the prelude's union type `error`, and the errors the
    /// language makes, each by its name; and a `message`
    /// for the entry to write one with, which gives every error's message
    /// as one text: none of these programs' errors is written.
    fn program(mut operators: Vec<hir::Operator>, variables: Vec<hir::Variable>) -> hir::Program {
        let entry = operators.iter().position(|o| o.name == "main").unwrap();
        let errors = hir::LanguageError::ALL
            .iter()
            .map(|error| {
                hir::Type::Defined(Box::new(hir::Defined {
                    name: error.name().to_string(),
                    unit: String::new(),
                    components: error.components(),
                    representations: Vec::new(),
                    constraints: Vec::new(),
                    keys: BTreeMap::new(),
                    order: None,
                    supertypes: vec!["error".to_string()],
                    specialized: false,
                    specialization: None,
                }))
            })
            .collect();
        operators.push(hir::Operator {
            versions: Vec::new(),
            constraints: Vec::new(),
            can_fail: false,
            name: "message".to_string(),
            types: Vec::new(),
            params: [(
                "error".to_string(),
                hir::Parameter::constant(library().error_type()),
            )]
            .into(),
            kind: hir::OperatorKind::ReadOnly {
                result: hir::Type::Scalar(hir::Scalar::Text),
                locals: Vec::new(),
                value: hir::Expr::Constant(hir::Value::Text("an error".to_string())),
            },
        });
        hir::Program {
            message: hir::OperatorId(operators.len() - 1),
            operators,
            variables,
            constants: Vec::new(),
            files: vec!["test.trendy".to_string()],
            constraints: Vec::new(),
            subtypes: [("error".to_string(), errors)].into(),
            language_errors: hir::LanguageError::ALL
                .iter()
                .map(|error| (*error, error.name().to_string()))
                .collect(),
            library: library(),
            entry: hir::OperatorId(entry),
        }
    }

    /// A variable with the value `init`, and for a relation, the key sema
    /// gives one without any: its whole heading.
    fn variable(name: &str, init: hir::Value) -> hir::Variable {
        let keys = match &init {
            hir::Value::Relation(r) => {
                vec![r.heading.attributes().map(|(a, _)| a.to_string()).collect()]
            }
            _ => Vec::new(),
        };
        hir::Variable {
            name: name.to_string(),
            ty: init.ty(),
            init,
            given: None,
            keys,
        }
    }

    fn call(operator: usize, args: Vec<(&str, hir::Expr)>) -> hir::Statement {
        hir::Statement::Call(hir::Call {
            operator: hir::OperatorId(operator),
            types: Vec::new(),
            args: args.into_iter().map(|(n, e)| (n.to_string(), e)).collect(),
            updated: BTreeMap::new(),
            can_fail: true,
        })
    }

    #[test]
    fn statements_and_values_set_their_sites() {
        // `main` calls `f`, which writes `x + 1`, which can overflow, then
        // loops while `x < 1`, writing `x`; each statement on a line of its
        // own.
        let x = || hir::Expr::Variable(hir::VariableId(0));
        let next = hir::Expr::Binary(
            hir::BinaryOp::Add(hir::Numeric::Integer),
            Box::new(x()),
            Box::new(hir::Expr::Constant(hir::Value::Integer(1))),
        );
        let less = hir::Expr::Binary(
            hir::BinaryOp::Compare(
                hir::Comparison::Less,
                hir::Compared::Scalar(hir::Scalar::Integer),
            ),
            Box::new(x()),
            Box::new(hir::Expr::Constant(hir::Value::Integer(1))),
        );
        let body = hir::Block {
            statements: sited(vec![call(0, vec![("v", x())])]),
            locals: Vec::new(),
        };
        let lowered = lower(&program(
            vec![
                debug_value(),
                user("f", &[], vec![call(0, vec![("v", next)])]),
                user(
                    "main",
                    &[],
                    vec![
                        call(1, vec![]),
                        hir::Statement::Loop(less, body, hir::Check::Before),
                    ],
                ),
            ],
            vec![variable("x", hir::Value::Integer(0))],
        ));
        // Each site's line, its column 5 and its path the program's.
        let line = |inst: &lir::Inst| {
            let lir::Inst::Site(id) = inst else {
                return None;
            };
            let fields = &lowered.data[id.0].fields;
            let lir::Field::Struct(path) = &fields[0] else {
                panic!("a site's path is a text: {fields:?}");
            };
            assert_eq!(path[1], lir::Field::Usize(len("test.trendy".len())));
            assert_eq!(fields[2], lir::Field::Usize(5));
            match fields[1] {
                lir::Field::Usize(line) => Some(line),
                _ => panic!("a site's line is a number: {fields:?}"),
            }
        };
        let main = &lowered.functions[0];
        let lines = |block: &lir::Block| block.insts.iter().filter_map(line).collect::<Vec<_>>();
        // The call's statement, again once `f` returns, which sets its
        // own, then the loop's, after the site `main` is entered at is
        // saved, for a failure's trace.
        assert_eq!(lines(&main.blocks[0]), [1, 1, 2]);
        assert!(matches!(
            main.blocks[0].insts[0],
            lir::Inst::SaveSite { .. }
        ));
        assert!(matches!(main.blocks[0].insts[2], lir::Inst::Call { .. }));
        // The loop's condition is the loop's, though its body ran before.
        let test = main
            .blocks
            .iter()
            .find(|b| {
                b.insts
                    .iter()
                    .any(|i| matches!(i, lir::Inst::Binary { .. }))
            })
            .expect("the loop's condition is computed");
        assert!(line(&test.insts[0]) == Some(2), "{:?}", test.insts);
        assert_eq!(lines(&lowered.functions[1].blocks[0]), [1]);
    }

    #[test]
    fn built_ins_dont_become_functions() {
        let lowered = lower_program(&program(
            vec![debug_value(), user("main", &[], vec![])],
            vec![],
        ));
        // `main`, the entry, which calls it, and writes a failure's error
        // and trace, and `message`, which it writes the error with.
        assert_eq!(lowered.functions.len(), 3);
        assert_eq!(lowered.functions[lowered.entry.0].name, "__entry");
        assert_eq!(lowered.functions[0].params, None);
    }

    #[test]
    fn values_point_to_constant_buffers_after_a_header() {
        let lowered = lower_program(&program(
            vec![user("main", &[], vec![])],
            vec![
                variable("i", hir::Value::Integer(-7)),
                variable("t", hir::Value::Text("hé".to_string())),
                variable("b", hir::Value::Boolean(true)),
            ],
        ));
        assert_eq!(lowered.data[0].fields, [lir::Field::I64(-7)]);
        // A text points to its bytes, a buffer that is never released.
        assert_eq!(
            lowered.data[1].fields,
            [
                lir::Field::immortal_header(),
                lir::Field::Bytes("hé".as_bytes().to_vec())
            ]
        );
        assert_eq!(
            lowered.data[2].fields,
            [lir::Field::Struct(vec![
                lir::Field::FieldAddress(lir::DataId(1), 1),
                lir::Field::Usize(3)
            ])]
        );
        assert_eq!(lowered.data[3].fields, [lir::Field::U8(1)]);
    }

    #[test]
    fn relations_point_to_an_array_of_tuples() {
        let tuple = |n| hir::TupleValue::new([("a".to_string(), hir::Value::Integer(n))].into());
        let heading = hir::Heading::new([("a".to_string(), integer())].into());
        let lowered = lower_program(&program(
            vec![user("main", &[], vec![])],
            vec![variable(
                "r",
                hir::Value::Relation(hir::RelationValue {
                    heading,
                    tuples: [tuple(2), tuple(1)].into(),
                }),
            )],
        ));
        let element = |n| lir::Field::Struct(vec![lir::Field::I64(n)]);
        assert_eq!(
            lowered.data[0].fields,
            [
                lir::Field::immortal_header(),
                lir::Field::Struct(vec![element(1), element(2)])
            ]
        );
        assert!(!lowered.data[0].writable);
        assert_eq!(
            lowered.data[1].fields,
            [lir::Field::Struct(vec![
                lir::Field::FieldAddress(lir::DataId(0), 1),
                lir::Field::Usize(2)
            ])]
        );
    }

    #[test]
    fn boolean_expressions_compute_into_slots() {
        // `debug_value{v: not (a and b)}`, of two variables.
        let a = || Box::new(hir::Expr::Variable(hir::VariableId(0)));
        let b = || Box::new(hir::Expr::Variable(hir::VariableId(1)));
        let expr = hir::Expr::Unary(
            hir::UnaryOp::Not,
            Box::new(hir::Expr::Connective(
                hir::Connective::And,
                vec![*a(), *b()],
            )),
        );
        let lowered = lower_program(&program(
            vec![
                debug_value(),
                user("main", &[], vec![call(0, vec![("v", expr)])]),
            ],
            vec![
                variable("a", hir::Value::Boolean(true)),
                variable("b", hir::Value::Boolean(false)),
            ],
        ));
        let main = &lowered.functions[0];
        assert_eq!(main.slots, [lir::Layout::U8, lir::Layout::U8]);
        let slot = |i| lir::Place::whole(lir::Base::Slot(lir::SlotId(i)));
        let data = |id| lir::Place {
            base: lir::Base::Data(lir::DataId(id)),
            path: vec![0],
        };
        assert_eq!(
            main.blocks[0].insts[..2],
            [
                lir::Inst::Binary {
                    dst: slot(0),
                    op: lir::BinaryOp::And(lir::Bits::Boolean),
                    lhs: data(0),
                    rhs: data(1),
                },
                lir::Inst::Unary {
                    dst: slot(1),
                    op: lir::UnaryOp::Not(lir::Bits::Boolean),
                    src: slot(0),
                },
            ]
        );
        let lir::Inst::CallRuntime { args, .. } = &main.blocks[0].insts[2] else {
            panic!("expected a runtime call");
        };
        assert_eq!(args[0], slot(1));
    }

    #[test]
    fn integer_expressions_compute_into_integer_slots() {
        let n = || Box::new(hir::Expr::Variable(hir::VariableId(0)));
        let expr = hir::Expr::Unary(
            hir::UnaryOp::Negate(hir::Numeric::Integer),
            Box::new(hir::Expr::Binary(hir::BinaryOp::Mod, n(), n())),
        );
        let lowered = lower_program(&program(
            vec![
                debug_value(),
                user("main", &[], vec![call(0, vec![("v", expr)])]),
            ],
            vec![variable("n", hir::Value::Integer(7))],
        ));
        let main = &lowered.functions[0];
        // Then where the site `main` is entered at is saved, which its
        // failure exit records.
        assert_eq!(
            main.slots,
            [lir::Layout::I64, lir::Layout::I64, lir::Layout::Address]
        );
        assert!(matches!(
            main.blocks[0].insts[..2],
            [
                lir::Inst::Binary {
                    op: lir::BinaryOp::Mod,
                    ..
                },
                lir::Inst::Unary {
                    op: lir::UnaryOp::Negate(lir::Numeric::Integer),
                    ..
                }
            ]
        ));
    }

    #[test]
    fn concatenations_are_owned_by_the_statements_scope() {
        let t = || Box::new(hir::Expr::Variable(hir::VariableId(0)));
        let inner = hir::Expr::Binary(hir::BinaryOp::Concatenate, t(), t());
        let expr = hir::Expr::Binary(hir::BinaryOp::Concatenate, Box::new(inner), t());
        let lowered = lower_program(&program(
            vec![
                debug_value(),
                user("main", &[], vec![call(0, vec![("v", expr)])]),
            ],
            vec![variable("t", hir::Value::Text("a".to_string()))],
        ));
        let main = &lowered.functions[0];
        let text = lir::Layout::Struct(vec![lir::Layout::Address, lir::Layout::Usize]);
        // Then where the scope `main` is entered in is saved, which its
        // failure exit ends scopes back to, and the site it is entered
        // at, which the exit records.
        let slots = [
            text.clone(),
            text,
            lir::Layout::scope(),
            lir::Layout::Address,
            lir::Layout::Address,
        ];
        assert_eq!(main.slots, slots);
        let slot = |i| lir::Place::whole(lir::Base::Slot(lir::SlotId(i)));
        let runtime: Vec<_> = main.blocks[0]
            .insts
            .iter()
            .map(|inst| match inst {
                lir::Inst::CallRuntime { function, args } => (*function, args[0].clone()),
                other => panic!("unexpected {other:?}"),
            })
            .collect();
        // Released together, once, when the statement's scope ends.
        assert_eq!(
            runtime,
            [
                (lir::RuntimeFn::Innermost, slot(3)),
                (lir::RuntimeFn::Begin, slot(2)),
                (lir::RuntimeFn::Concat, slot(0)),
                (lir::RuntimeFn::Concat, slot(1)),
                (lir::RuntimeFn::DebugValue, slot(1)),
                (lir::RuntimeFn::End, slot(2)),
            ]
        );
    }

    fn assign(assignments: Vec<(usize, hir::Expr)>) -> hir::Statement {
        hir::Statement::Assign(
            assignments
                .into_iter()
                .map(|(target, value)| hir::Assignment {
                    target: hir::Target::Variable(hir::VariableId(target)),
                    value,
                })
                .collect(),
        )
    }

    #[test]
    fn assignments_read_every_value_before_assigning_any() {
        // `a := b, b := a;` swaps them.
        let var = |i| hir::Expr::Variable(hir::VariableId(i));
        let lowered = lower_program(&program(
            vec![user(
                "main",
                &[],
                vec![assign(vec![(0, var(1)), (1, var(0))])],
            )],
            vec![
                variable("a", hir::Value::Integer(1)),
                variable("b", hir::Value::Integer(2)),
            ],
        ));
        let data = |i| lir::Place {
            base: lir::Base::Data(lir::DataId(i)),
            path: vec![0],
        };
        let slot = |i| lir::Place::whole(lir::Base::Slot(lir::SlotId(i)));
        let copy = |dst, src| lir::Inst::Copy {
            dst,
            src,
            layout: lir::Layout::I64,
        };
        assert_eq!(
            lowered.functions[0].blocks[0].insts,
            [
                copy(slot(0), data(1)),
                copy(slot(1), data(0)),
                copy(data(0), slot(0)),
                copy(data(1), slot(1)),
            ]
        );
    }

    #[test]
    fn an_insert_alone_changes_its_variable() {
        let var = |i| hir::Expr::Variable(hir::VariableId(i));
        let empty = || {
            variable(
                "r",
                hir::Value::Relation(hir::RelationValue {
                    heading: hir::Heading::new(BTreeMap::new()),
                    tuples: BTreeSet::new(),
                }),
            )
        };
        let calls = |statement: hir::Statement| {
            let lowered = lower_program(&program(
                vec![user("main", &[], vec![statement])],
                vec![empty(), empty()],
            ));
            runtime_calls(&lowered.functions[0]).concat()
        };
        let union =
            |lhs, rhs| hir::Expr::Combine(hir::Combination::Union, Box::new(lhs), Box::new(rhs));
        // `insert r s;`, whichever way it is written.
        let inserts = calls(assign(vec![(0, union(var(0), var(1)))]));
        assert!(inserts.contains(&lir::RuntimeFn::Insert), "{inserts:?}");
        assert!(!inserts.contains(&lir::RuntimeFn::Combine), "{inserts:?}");
        // The other way round, it is a new relation.
        let unions = calls(assign(vec![(0, union(var(1), var(0)))]));
        assert!(unions.contains(&lir::RuntimeFn::Combine), "{unions:?}");
        assert!(!unions.contains(&lir::RuntimeFn::Insert), "{unions:?}");
        // With others at once, which read it as it was, so is it.
        let both = calls(assign(vec![(0, union(var(0), var(1))), (1, var(0))]));
        assert!(both.contains(&lir::RuntimeFn::Combine), "{both:?}");
        assert!(!both.contains(&lir::RuntimeFn::Insert), "{both:?}");
    }

    #[test]
    fn a_delete_alone_changes_its_variable() {
        let var = |i| hir::Expr::Variable(hir::VariableId(i));
        let heading = || {
            hir::Heading::new(BTreeMap::from([(
                "a".to_string(),
                hir::Type::Scalar(hir::Scalar::Integer),
            )]))
        };
        let empty = || {
            variable(
                "r",
                hir::Value::Relation(hir::RelationValue {
                    heading: heading(),
                    tuples: BTreeSet::new(),
                }),
            )
        };
        let calls = |statement: hir::Statement| {
            let lowered = lower_program(&program(
                vec![user("main", &[], vec![statement])],
                vec![empty(), empty()],
            ));
            runtime_calls(&lowered.functions[0]).concat()
        };
        let attribute =
            || hir::Expr::Extract(Box::new(hir::Expr::Current(0, heading())), "a".to_string());
        let compared = |comparison, value: i64| {
            hir::Expr::Binary(
                hir::BinaryOp::Compare(comparison, hir::Compared::Scalar(hir::Scalar::Integer)),
                Box::new(attribute()),
                Box::new(hir::Expr::Constant(hir::Value::Integer(value))),
            )
        };
        let restrict =
            |variable, condition| hir::Expr::Restrict(Box::new(variable), Box::new(condition));
        // `delete r where a = 1;`: its tuples are found, with nothing to
        // compute for each.
        let by_key = calls(assign(vec![(
            0,
            restrict(var(0), compared(hir::Comparison::NotEqual, 1)),
        )]));
        assert!(by_key.contains(&lir::RuntimeFn::EqualRun), "{by_key:?}");
        assert!(by_key.contains(&lir::RuntimeFn::RemoveRun), "{by_key:?}");
        assert!(!by_key.contains(&lir::RuntimeFn::Allocate), "{by_key:?}");
        // `delete r where a > 1;`: gone through, and then taken.
        let through = calls(assign(vec![(
            0,
            restrict(var(0), compared(hir::Comparison::LessOrEqual, 1)),
        )]));
        assert!(
            through.contains(&lir::RuntimeFn::RemoveMarked),
            "{through:?}"
        );
        assert!(!through.contains(&lir::RuntimeFn::EqualRun), "{through:?}");
        // `i_delete r s;`.
        let included = calls(assign(vec![(
            0,
            hir::Expr::Combine(
                hir::Combination::IncludedMinus,
                Box::new(var(0)),
                Box::new(var(1)),
            ),
        )]));
        assert!(included.contains(&lir::RuntimeFn::Remove), "{included:?}");
        assert!(!included.contains(&lir::RuntimeFn::Combine), "{included:?}");
        // Another's value, or with others at once, is a new relation.
        let other = calls(assign(vec![(
            0,
            restrict(var(1), compared(hir::Comparison::NotEqual, 1)),
        )]));
        assert!(!other.contains(&lir::RuntimeFn::RemoveRun), "{other:?}");
        let both = calls(assign(vec![
            (0, restrict(var(0), compared(hir::Comparison::NotEqual, 1))),
            (1, var(0)),
        ]));
        assert!(!both.contains(&lir::RuntimeFn::RemoveRun), "{both:?}");
        assert!(!both.contains(&lir::RuntimeFn::RemoveMarked), "{both:?}");
    }

    #[test]
    fn a_value_that_can_fail_finds_a_run_once_computed() {
        let heading = || {
            hir::Heading::new(BTreeMap::from([
                ("a".to_string(), hir::Type::Scalar(hir::Scalar::Integer)),
                ("z".to_string(), hir::Type::Scalar(hir::Scalar::Integer)),
            ]))
        };
        let empty = || {
            variable(
                "r",
                hir::Value::Relation(hir::RelationValue {
                    heading: heading(),
                    tuples: BTreeSet::new(),
                }),
            )
        };
        let number = || variable("n", hir::Value::Integer(1));
        let var = |i| hir::Expr::Variable(hir::VariableId(i));
        let attribute = |name: &str| {
            hir::Expr::Extract(Box::new(hir::Expr::Current(0, heading())), name.to_string())
        };
        let compared = |comparison, name: &str, value: hir::Expr| {
            hir::Expr::Binary(
                hir::BinaryOp::Compare(comparison, hir::Compared::Scalar(hir::Scalar::Integer)),
                Box::new(attribute(name)),
                Box::new(value),
            )
        };
        let failable = || {
            hir::Expr::Binary(
                hir::BinaryOp::Add(hir::Numeric::Integer),
                Box::new(var(1)),
                Box::new(hir::Expr::Constant(hir::Value::Integer(1))),
            )
        };
        let runs = |condition| {
            let restrict = hir::Expr::Restrict(Box::new(var(0)), Box::new(condition));
            let lowered = lower_program(&program(
                vec![user(
                    "main",
                    &[],
                    vec![hir::Statement::Assign(vec![hir::Assignment {
                        target: hir::Target::Variable(hir::VariableId(2)),
                        value: restrict,
                    }])],
                )],
                vec![empty(), number(), empty()],
            ));
            runtime_calls(&lowered.functions[0])
                .concat()
                .into_iter()
                .filter(|f| *f == lir::RuntimeFn::EqualRun)
                .count()
        };
        let and = |operands| hir::Connective::And.of(operands);
        let equal = hir::Comparison::Equal;
        // Each is lowered twice: with what can fail computed first, which finds the runs of
        // whatever it equates the first attributes to, beside anything else; and as it is,
        // for if that fails, which finds the runs only if nothing else is left. So: all that
        // is said, once each way.
        assert_eq!(runs(compared(equal, "a", failable())), 2);
        // Of both attributes, which are the first two.
        assert_eq!(
            runs(and(vec![
                compared(equal, "a", failable()),
                compared(equal, "z", failable()),
            ])),
            4
        );
        // After one that can't fail, which decides it where it is false.
        assert_eq!(
            runs(and(vec![
                compared(equal, "a", var(1)),
                compared(equal, "z", failable()),
            ])),
            4
        );
        // With one that can't fail left, which decides it where it is false, however few
        // tuples it holds for, only once computed.
        assert_eq!(
            runs(and(vec![
                compared(equal, "a", failable()),
                compared(hir::Comparison::Greater, "z", var(1)),
            ])),
            1
        );
        assert_eq!(
            runs(and(vec![
                compared(equal, "a", failable()),
                compared(equal, "z", var(1)),
            ])),
            2
        );
        // With another that can fail, computed for every tuple, only once computed.
        assert_eq!(
            runs(and(vec![
                compared(equal, "a", failable()),
                compared(hir::Comparison::Greater, "z", failable()),
            ])),
            1
        );
        // Not of an attribute that isn't next.
        assert_eq!(runs(compared(equal, "z", failable())), 0);
    }

    #[test]
    fn a_var_parameter_is_its_variables_address() {
        // `operator set{var n: integer} [ n := 7; ];`, called with the
        // variable `a`: `call set{var n: a};`.
        let mut set = user(
            "set",
            &[],
            vec![hir::Statement::Assign(vec![hir::Assignment {
                target: hir::Target::Parameter("n".to_string()),
                value: hir::Expr::Constant(hir::Value::Integer(7)),
            }])],
        );
        set.params = [(
            "n".to_string(),
            hir::Parameter {
                ty: integer(),
                mode: hir::Mode::Var,
            },
        )]
        .into();
        let call = hir::Statement::Call(hir::Call {
            operator: hir::OperatorId(1),
            types: Vec::new(),
            args: BTreeMap::new(),
            updated: [("n".to_string(), hir::Target::Variable(hir::VariableId(0)))].into(),
            can_fail: true,
        });
        let lowered = lower_program(&program(
            vec![user("main", &[], vec![call]), set],
            vec![variable("a", hir::Value::Integer(1))],
        ));
        let slot = |i| lir::Place::whole(lir::Base::Slot(lir::SlotId(i)));
        let field = |mut place: lir::Place, i| {
            place.path.push(i);
            place
        };
        // The caller passes the variable's address. Nothing in `set` can
        // fail, so nothing follows the call for a failure.
        assert_eq!(
            lowered.functions[0].blocks[0].insts,
            [
                lir::Inst::Address {
                    dst: field(slot(0), 0),
                    src: lir::Place {
                        base: lir::Base::Data(lir::DataId(0)),
                        path: vec![0],
                    },
                },
                lir::Inst::Call {
                    function: lir::FunctionId(1),
                    args: Some(slot(0)),
                    result: None,
                    can_fail: false,
                },
            ]
        );
        // The operator takes it from its parameters, and assigns the
        // variable there.
        let set = &lowered.functions[1];
        assert_eq!(
            set.params,
            Some(lir::Layout::Struct(vec![lir::Layout::Address]))
        );
        let insts = &set.blocks[0].insts;
        assert_eq!(
            insts[0],
            lir::Inst::Copy {
                dst: slot(0),
                src: field(lir::Place::whole(lir::Base::Params), 0),
                layout: lir::Layout::Address,
            }
        );
        let lir::Inst::Copy { dst, .. } = insts.last().expect("an assignment") else {
            panic!("{insts:?}");
        };
        assert_eq!(
            *dst,
            lir::Place::whole(lir::Base::Indirect {
                slot: lir::SlotId(0),
                pointee: lir::Layout::I64,
            })
        );
    }

    #[test]
    fn elements_are_changed_in_place() {
        // `a[2] := 5;`, of `var a := array [1, 2, 3];`.
        let integers = |ns: &[i64]| {
            hir::Value::Array(hir::ArrayValue {
                element: integer(),
                elements: ns.iter().map(|n| hir::Value::Integer(*n)).collect(),
            })
        };
        let int = |n| hir::Expr::Constant(hir::Value::Integer(n));
        let changes = vec![hir::ElementChange {
            path: vec![int(2)],
            value: int(5),
        }];
        let a = hir::Expr::Variable(hir::VariableId(0));
        let new_value = hir::Expr::WithElements(Box::new(a), changes.clone());
        let lowered = lower_program(&program(
            vec![user("main", &[], vec![assign(vec![(0, new_value)])])],
            vec![variable("a", integers(&[1, 2, 3]))],
        ));
        let runtime_calls = |function: &lir::Function| -> Vec<(lir::RuntimeFn, lir::Place)> {
            function
                .blocks
                .iter()
                .flat_map(|b| &b.insts)
                .filter_map(|inst| match inst {
                    lir::Inst::CallRuntime { function, args } => Some((*function, args[0].clone())),
                    _ => None,
                })
                .collect()
        };
        let calls = runtime_calls(&lowered.functions[0]);
        let functions: Vec<lir::RuntimeFn> = calls.iter().map(|(f, _)| *f).collect();
        // The variable is made the only holder of its buffer, which is
        // then changed: an integer is copied in, with no reference to take.
        // The position can be out of range, so the function saves the
        // scope it is entered in, which its failure exit ends scopes back
        // to.
        assert_eq!(
            functions,
            [
                lir::RuntimeFn::Innermost,
                lir::RuntimeFn::Begin,
                lir::RuntimeFn::Unique,
                lir::RuntimeFn::End,
                lir::RuntimeFn::EndSince,
                lir::RuntimeFn::Trace,
            ]
        );
        assert_eq!(
            calls[2].1,
            lir::Place {
                base: lir::Base::Data(lir::DataId(1)),
                path: vec![0],
            }
        );
        assert!(lowered.functions[0].blocks.iter().any(|b| matches!(
            &b.terminator,
            lir::Terminator::Fail(failure) if matches!(**failure, lir::Failure::IndexOutOfRange { .. })
        )));
        // As a value, as a value block computes one, the change is made in
        // a copy, which the statement holds.
        let value =
            hir::Expr::WithElements(Box::new(hir::Expr::Constant(integers(&[1, 2, 3]))), changes);
        let lowered = lower_program(&program(
            vec![
                debug_value(),
                user("main", &[], vec![call(0, vec![("v", value)])]),
            ],
            Vec::new(),
        ));
        let functions: Vec<lir::RuntimeFn> = runtime_calls(&lowered.functions[0])
            .into_iter()
            .map(|(f, _)| f)
            .collect();
        assert_eq!(
            functions,
            [
                lir::RuntimeFn::Innermost,
                lir::RuntimeFn::Begin,
                lir::RuntimeFn::Duplicate,
                lir::RuntimeFn::Unique,
                lir::RuntimeFn::DebugValue,
                lir::RuntimeFn::End,
                lir::RuntimeFn::EndSince,
                lir::RuntimeFn::Trace,
            ]
        );
    }

    #[test]
    fn changes_that_may_overlap_are_checked_when_running() {
        // `assign { a[i] := 1; a[j] := 2; a[3] := 3; };`, of parameters
        // `i` and `j`: the first two may set one element, but not the
        // third.
        let param = |name: &str| hir::Expr::Parameter(name.to_string());
        let int = |n| hir::Expr::Constant(hir::Value::Integer(n));
        let change = |position, n| hir::ElementChange {
            path: vec![position],
            value: int(n),
        };
        let changed = hir::Expr::WithElements(
            Box::new(hir::Expr::Variable(hir::VariableId(0))),
            vec![
                change(param("i"), 1),
                change(param("j"), 2),
                change(int(3), 3),
            ],
        );
        let array = hir::Value::Array(hir::ArrayValue {
            element: integer(),
            elements: vec![hir::Value::Integer(0); 3],
        });
        let lowered = lower_program(&program(
            vec![user("main", &["i", "j"], vec![assign(vec![(0, changed)])])],
            vec![variable("a", array)],
        ));
        let conflicts = lowered.functions[0]
            .blocks
            .iter()
            .filter(|b| {
                matches!(
                    &b.terminator,
                    lir::Terminator::Fail(failure)
                        if matches!(**failure, lir::Failure::ElementConflict { .. })
                )
            })
            .count();
        // `i` with `j`, and each with 3, as their values are known only
        // when running; 3 with itself never.
        assert_eq!(conflicts, 3);
    }

    #[test]
    fn assigned_texts_take_a_reference_to_their_bytes() {
        let t = hir::Expr::Variable(hir::VariableId(0));
        let lowered = lower_program(&program(
            vec![user("main", &[], vec![assign(vec![(0, t)])])],
            vec![variable("t", hir::Value::Text("a".to_string()))],
        ));
        let variable = lir::Place {
            base: lir::Base::Data(lir::DataId(1)),
            path: vec![0],
        };
        let slot = |i| lir::Place::whole(lir::Base::Slot(lir::SlotId(i)));
        // After the scope the function is entered in is saved.
        let insts = &lowered.functions[0].blocks[0].insts[1..];
        let lir::Inst::CallRuntime { function, args } = &insts[2] else {
            panic!("expected a runtime call, found {:?}", insts[2]);
        };
        assert_eq!(*function, lir::RuntimeFn::Assign);
        assert_eq!(args[..2], [variable, slot(0)]);
        // The statement has a scope, which takes over the variable's
        // references to its old value's bytes.
        let scope = |function| lir::Inst::CallRuntime {
            function,
            args: vec![slot(1)],
        };
        assert_eq!(insts[0], scope(lir::RuntimeFn::Begin));
        assert_eq!(insts[3], scope(lir::RuntimeFn::End));
    }

    #[test]
    fn joins_call_the_runtime_with_where_each_attribute_is() {
        let relation = |attributes: &[&str]| {
            hir::Value::Relation(hir::RelationValue {
                heading: hir::Heading::new(
                    attributes
                        .iter()
                        .map(|a| ((*a).to_string(), integer()))
                        .collect(),
                ),
                tuples: std::collections::BTreeSet::new(),
            })
        };
        let var = |i| Box::new(hir::Expr::Variable(hir::VariableId(i)));
        let lowered = lower_program(&program(
            vec![
                debug_value(),
                user(
                    "main",
                    &[],
                    vec![call(0, vec![("v", hir::Expr::And(var(0), var(1)))])],
                ),
            ],
            vec![
                variable("r", relation(&["a", "b"])),
                variable("s", relation(&["b", "c"])),
            ],
        ));
        let main = &lowered.functions[0];
        let slot = lir::Place::whole(lir::Base::Slot(lir::SlotId(0)));
        // After the scope `main` is entered in is saved, and its statement's
        // begun.
        let lir::Inst::CallRuntime { function, args } = &main.blocks[0].insts[2] else {
            panic!("{:?}", main.blocks[0].insts);
        };
        assert_eq!(*function, lir::RuntimeFn::And);
        assert_eq!(args[0], slot);
        assert_eq!(
            args.len(),
            10,
            "dst, the operands, three descriptors, the sources, the common attributes and the orders"
        );
        let descriptors: Vec<_> = args[3..6].iter().map(|a| &a.base).collect();
        assert!(
            descriptors[0] != descriptors[1] && descriptors[1] != descriptors[2],
            "the result's and each operand's: {descriptors:?}"
        );
        let released = main.blocks[0].insts.last().unwrap();
        assert!(matches!(
            released,
            lir::Inst::CallRuntime {
                function: lir::RuntimeFn::End,
                ..
            }
        ));
    }

    #[test]
    fn extending_a_relation_loops_over_its_tuples() {
        let heading = hir::Heading::new([("n".to_string(), integer())].into());
        let n = hir::Expr::Extract(
            Box::new(hir::Expr::Current(0, heading.clone())),
            "n".to_string(),
        );
        let added = hir::Heading::new([("m".to_string(), integer())].into());
        let extend = hir::Expr::Extend(
            Box::new(hir::Expr::Variable(hir::VariableId(0))),
            added,
            [("m".to_string(), n)].into(),
        );
        let lowered = lower_program(&program(
            vec![
                debug_value(),
                user("main", &[], vec![call(0, vec![("v", extend)])]),
            ],
            vec![variable(
                "r",
                hir::Value::Relation(hir::RelationValue {
                    heading,
                    tuples: std::collections::BTreeSet::new(),
                }),
            )],
        ));
        let main = &lowered.functions[0];
        let [entry, head, body, done, _failed] = &main.blocks[..] else {
            panic!("{:#?}", main.blocks);
        };
        let runtime = |block: &lir::Block| -> Vec<lir::RuntimeFn> {
            block
                .insts
                .iter()
                .filter_map(|inst| match inst {
                    lir::Inst::CallRuntime { function, .. } => Some(*function),
                    _ => None,
                })
                .collect()
        };
        // The result is allocated, then each tuple computed into it, then
        // sorted, in the statement's scope, which begins in a block ended
        // before it was known to need one, after the scope `main` is
        // entered in is saved.
        assert_eq!(
            runtime(entry),
            [
                lir::RuntimeFn::Innermost,
                lir::RuntimeFn::Begin,
                lir::RuntimeFn::Allocate
            ]
        );
        assert_eq!(entry.terminator, lir::Terminator::Jump(lir::BlockId(1)));
        assert!(matches!(
            head.terminator,
            lir::Terminator::Branch {
                then: lir::BlockId(2),
                otherwise: lir::BlockId(3),
                ..
            }
        ));
        assert!(
            body.insts
                .iter()
                .any(|inst| matches!(inst, lir::Inst::Element { .. }))
        );
        assert!(matches!(
            body.insts.last(),
            Some(lir::Inst::Increment { .. })
        ));
        assert_eq!(body.terminator, lir::Terminator::Jump(lir::BlockId(1)));
        assert_eq!(
            runtime(done),
            [
                lir::RuntimeFn::Sort,
                lir::RuntimeFn::DebugValue,
                lir::RuntimeFn::End
            ]
        );
        assert_eq!(done.terminator, lir::Terminator::Return);
    }

    /// The runtime functions each block of `f` calls, in order.
    fn runtime_calls(f: &lir::Function) -> Vec<Vec<lir::RuntimeFn>> {
        f.blocks
            .iter()
            .map(|block| {
                block
                    .insts
                    .iter()
                    .filter_map(|inst| match inst {
                        lir::Inst::CallRuntime { function, .. } => Some(*function),
                        _ => None,
                    })
                    .collect()
            })
            .collect()
    }

    #[test]
    fn the_work_on_each_tuple_is_in_a_scope_of_its_own() {
        use lir::RuntimeFn::{Begin, Concat, End, HandOut};
        // `r extend { u := t || "!" }` and `r where t || "!" = "a!"`.
        let text = || hir::Type::Scalar(hir::Scalar::Text);
        let heading = hir::Heading::new([("t".to_string(), text())].into());
        let shouted = || {
            let t = hir::Expr::Extract(
                Box::new(hir::Expr::Current(0, heading.clone())),
                "t".to_string(),
            );
            let bang = hir::Expr::Constant(hir::Value::Text("!".to_string()));
            hir::Expr::Binary(hir::BinaryOp::Concatenate, Box::new(t), Box::new(bang))
        };
        let r = || Box::new(hir::Expr::Variable(hir::VariableId(0)));
        let extend = hir::Expr::Extend(
            r(),
            hir::Heading::new([("u".to_string(), text())].into()),
            [("u".to_string(), shouted())].into(),
        );
        let condition = hir::Expr::Binary(
            hir::BinaryOp::Compare(
                hir::Comparison::Equal,
                hir::Compared::Scalar(hir::Scalar::Text),
            ),
            Box::new(shouted()),
            Box::new(hir::Expr::Constant(hir::Value::Text("a!".to_string()))),
        );
        let restrict = hir::Expr::Restrict(r(), Box::new(condition));
        let lowered = |value: hir::Expr| {
            lower_program(&program(
                vec![
                    debug_value(),
                    user("main", &[], vec![call(0, vec![("v", value)])]),
                ],
                vec![variable(
                    "r",
                    hir::Value::Relation(hir::RelationValue {
                        heading: heading.clone(),
                        tuples: std::collections::BTreeSet::new(),
                    }),
                )],
            ))
        };
        // The tuple's text is made in its scope, which hands out the tuple
        // built before it ends, inside the statement's.
        let extended = lowered(extend);
        let blocks = runtime_calls(&extended.functions[0]);
        assert!(
            blocks.contains(&vec![Begin, Concat, HandOut, End]),
            "{blocks:?}"
        );
        // The condition's is made in its scope, which nothing outlives.
        let restricted = lowered(restrict);
        let blocks = runtime_calls(&restricted.functions[0]);
        let per_tuple = blocks
            .iter()
            .find(|block| block.contains(&Concat))
            .expect("a block computes the condition");
        assert_eq!(per_tuple.first(), Some(&Begin), "{blocks:?}");
        assert_eq!(per_tuple.last(), Some(&End), "{blocks:?}");
        assert!(!per_tuple.contains(&HandOut), "{blocks:?}");
    }

    #[test]
    fn a_restriction_by_its_first_attributes_finds_their_run() {
        use lir::RuntimeFn::EqualRun;
        // `r where …`, of `r` with the attributes `a` and `b`.
        let heading =
            hir::Heading::new([("a".to_string(), integer()), ("b".to_string(), integer())].into());
        let attribute = |name: &str| {
            hir::Expr::Extract(
                Box::new(hir::Expr::Current(0, heading.clone())),
                name.to_string(),
            )
        };
        let one = || hir::Expr::Constant(hir::Value::Integer(1));
        let equal = |lhs: hir::Expr, rhs: hir::Expr| {
            hir::Expr::Binary(
                hir::BinaryOp::Compare(
                    hir::Comparison::Equal,
                    hir::Compared::Scalar(hir::Scalar::Integer),
                ),
                Box::new(lhs),
                Box::new(rhs),
            )
        };
        let runs = |condition: hir::Expr| {
            let restrict = hir::Expr::Restrict(
                Box::new(hir::Expr::Variable(hir::VariableId(0))),
                Box::new(condition),
            );
            let lowered = lower_program(&program(
                vec![
                    debug_value(),
                    user("main", &[], vec![call(0, vec![("v", restrict)])]),
                ],
                vec![variable(
                    "r",
                    hir::Value::Relation(hir::RelationValue {
                        heading: heading.clone(),
                        tuples: std::collections::BTreeSet::new(),
                    }),
                )],
            ));
            let calls = runtime_calls(&lowered.functions[0]).concat();
            calls.iter().filter(|call| **call == EqualRun).count()
        };
        let and = hir::Connective::And;
        // The first, on either side, and the first two in either order.
        assert_eq!(runs(equal(attribute("a"), one())), 1);
        assert_eq!(runs(equal(one(), attribute("a"))), 1);
        let both = and.of(vec![
            equal(attribute("b"), one()),
            equal(attribute("a"), one()),
        ]);
        assert_eq!(runs(both), 2);
        // Only the second, which the tuples aren't sorted by.
        assert_eq!(runs(equal(attribute("b"), one())), 0);
        // Another attribute's value differs from tuple to tuple.
        assert_eq!(runs(equal(attribute("a"), attribute("b"))), 0);
        // A value that can fail is computed once, if there is a tuple, before its run is
        // found, and found again as it is, for if it fails (see the test after this one).
        let can_fail = hir::Expr::Binary(hir::BinaryOp::Div, Box::new(one()), Box::new(one()));
        assert_eq!(runs(equal(attribute("a"), can_fail)), 2);
    }

    #[test]
    fn a_loop_looks_up_in_a_relation_the_same_for_each_tuple() {
        use lir::RuntimeFn::{And, Index, Lookup, Release};
        // `r extend { image := s join relation { tuple { a: a } } }`, as an
        // image relation of `s` is before its `remove`.
        let a = || ("a".to_string(), integer());
        let r_heading = hir::Heading::new([a()].into());
        let s_heading = hir::Heading::new([a(), ("b".to_string(), integer())].into());
        let key = || {
            let a = hir::Expr::Extract(
                Box::new(hir::Expr::Current(0, r_heading.clone())),
                "a".to_string(),
            );
            hir::Expr::Relation(
                r_heading.clone(),
                vec![hir::Expr::Tuple([("a".to_string(), a)].into())],
            )
        };
        let extended = |operand: hir::Expr| {
            let image = hir::Expr::And(Box::new(operand), Box::new(key()));
            let added = hir::Heading::new(
                [("image".to_string(), hir::Type::Relation(s_heading.clone()))].into(),
            );
            let extend = hir::Expr::Extend(
                Box::new(hir::Expr::Variable(hir::VariableId(0))),
                added,
                [("image".to_string(), image)].into(),
            );
            let empty = |heading: &hir::Heading| {
                hir::Value::Relation(hir::RelationValue {
                    heading: heading.clone(),
                    tuples: std::collections::BTreeSet::new(),
                })
            };
            let lowered = lower_program(&program(
                vec![
                    debug_value(),
                    user("main", &[], vec![call(0, vec![("v", extend)])]),
                ],
                vec![
                    variable("r", empty(&r_heading)),
                    variable("s", empty(&s_heading)),
                ],
            ));
            runtime_calls(&lowered.functions[0]).concat()
        };
        // `s` is the same for each tuple: the loop indexes it once, and
        // gives it and its index up as it ends.
        let calls = extended(hir::Expr::Variable(hir::VariableId(1)));
        let count = |f: lir::RuntimeFn| calls.iter().filter(|c| **c == f).count();
        assert_eq!(
            (count(Index), count(Lookup), count(And)),
            (1, 1, 0),
            "{calls:?}"
        );
        let after = calls.iter().position(|c| *c == Lookup).unwrap();
        assert_eq!(
            calls[after..].iter().filter(|c| **c == Release).count(),
            2,
            "{calls:?}"
        );
        // A relation that reads the tuple at hand is joined anew for each.
        let calls = extended(key());
        assert!(
            calls.contains(&And) && !calls.contains(&Lookup),
            "{calls:?}"
        );
    }

    #[test]
    fn calls_build_the_argument_tuple_in_a_slot() {
        let lowered = lower_program(&program(
            vec![
                user("f", &["a", "b"], vec![]),
                user(
                    "main",
                    &[],
                    vec![call(
                        0,
                        vec![
                            ("b", hir::Expr::Variable(hir::VariableId(0))),
                            ("a", hir::Expr::Constant(hir::Value::Integer(5))),
                        ],
                    )],
                ),
            ],
            vec![variable("x", hir::Value::Integer(1))],
        ));
        // `main` first, then what it calls.
        assert_eq!(
            lowered.functions[1].params,
            Some(lir::Layout::Struct(vec![
                lir::Layout::I64,
                lir::Layout::I64
            ]))
        );
        let main = &lowered.functions[0];
        // Nothing in `f` can fail, so `main` has no site to go back to after
        // the call, nor a failure exit, and needs no slot for the site it is
        // entered at.
        assert_eq!(
            main.slots,
            [lir::Layout::Struct(vec![
                lir::Layout::I64,
                lir::Layout::I64
            ])]
        );
        let slot = |i| lir::Place {
            base: lir::Base::Slot(lir::SlotId(0)),
            path: vec![i],
        };
        let data = |id| lir::Place {
            base: lir::Base::Data(lir::DataId(id)),
            path: vec![0],
        };
        // The constant `5`, among the other data, such as the sites'.
        let five = lowered
            .data
            .iter()
            .position(|d| d.fields == [lir::Field::I64(5)])
            .expect("`5` is a constant");
        assert_eq!(
            main.blocks[0].insts,
            [
                // `a`, then `b`: parameter name order.
                lir::Inst::Copy {
                    dst: slot(0),
                    src: data(five),
                    layout: lir::Layout::I64,
                },
                lir::Inst::Copy {
                    dst: slot(1),
                    src: data(0),
                    layout: lir::Layout::I64,
                },
                lir::Inst::Call {
                    function: lir::FunctionId(1),
                    args: Some(lir::Place::whole(lir::Base::Slot(lir::SlotId(0)))),
                    result: None,
                    can_fail: false,
                },
            ]
        );
    }

    #[test]
    fn calls_passing_a_text_keep_it() {
        // `call f{t: t};`, which makes nothing, but must keep `t`'s bytes
        // from being freed by an assignment `f` makes to `t`.
        let f = hir::Operator {
            versions: Vec::new(),
            constraints: Vec::new(),
            can_fail: true,
            types: Vec::new(),
            name: "f".to_string(),
            params: [(
                "t".to_string(),
                hir::Parameter::constant(hir::Type::Scalar(hir::Scalar::Text)),
            )]
            .into(),
            kind: hir::OperatorKind::Update {
                locals: Vec::new(),
                body: hir::Block::default(),
            },
        };
        let lowered = lower_program(&program(
            vec![
                f,
                user(
                    "main",
                    &[],
                    vec![call(
                        0,
                        vec![("t", hir::Expr::Variable(hir::VariableId(0)))],
                    )],
                ),
            ],
            vec![variable("t", hir::Value::Text("a".to_string()))],
        ));
        let insts = &lowered.functions[0].blocks[0].insts;
        let functions: Vec<_> = insts
            .iter()
            .filter_map(|inst| match inst {
                lir::Inst::CallRuntime { function, .. } => Some(*function),
                _ => None,
            })
            .collect();
        assert_eq!(
            functions,
            [
                lir::RuntimeFn::Innermost,
                lir::RuntimeFn::Begin,
                lir::RuntimeFn::Keep,
                lir::RuntimeFn::End
            ]
        );
        assert!(matches!(insts[insts.len() - 2], lir::Inst::Call { .. }));
    }

    #[test]
    fn only_a_condition_that_calls_functions_is_ranked() {
        // `r where half{n: x} > 0 and half{n: x} < 5`, whose operands each call `half`, is
        // ranked as the restriction runs; `r where x + 1 > 0 and x * 2 < 5` isn't, as its
        // operands cost less to compute than to rank.
        let heading = hir::Heading::new([("x".to_string(), integer())].into());
        let x = || {
            hir::Expr::Extract(
                Box::new(hir::Expr::Current(0, heading.clone())),
                "x".to_string(),
            )
        };
        let half = || {
            hir::Expr::Call(hir::Call {
                operator: hir::OperatorId(1),
                types: Vec::new(),
                args: [("n".to_string(), x())].into(),
                updated: BTreeMap::new(),
                can_fail: true,
            })
        };
        let compared = |comparison, lhs: hir::Expr, n: i64| {
            hir::Expr::Binary(
                hir::BinaryOp::Compare(comparison, hir::Compared::Scalar(hir::Scalar::Integer)),
                Box::new(lhs),
                Box::new(hir::Expr::Constant(hir::Value::Integer(n))),
            )
        };
        let arithmetic = |op, n: i64| {
            hir::Expr::Binary(
                op,
                Box::new(x()),
                Box::new(hir::Expr::Constant(hir::Value::Integer(n))),
            )
        };
        let ranks = |condition: hir::Expr| {
            let restricted = hir::Expr::Restrict(
                Box::new(hir::Expr::Variable(hir::VariableId(0))),
                Box::new(condition),
            );
            let half_of = hir::Operator {
                versions: Vec::new(),
                constraints: Vec::new(),
                can_fail: true,
                name: "half".to_string(),
                types: Vec::new(),
                params: [("n".to_string(), hir::Parameter::constant(integer()))].into(),
                kind: hir::OperatorKind::ReadOnly {
                    result: integer(),
                    locals: Vec::new(),
                    value: hir::Expr::Binary(
                        hir::BinaryOp::Div,
                        Box::new(hir::Expr::Parameter("n".to_string())),
                        Box::new(hir::Expr::Constant(hir::Value::Integer(2))),
                    ),
                },
            };
            let lowered = lower_program(&program(
                vec![
                    debug_value(),
                    half_of,
                    user("main", &[], vec![call(0, vec![("v", restricted)])]),
                ],
                vec![variable(
                    "r",
                    hir::Value::Relation(hir::RelationValue {
                        heading: heading.clone(),
                        tuples: std::collections::BTreeSet::new(),
                    }),
                )],
            ));
            lowered.functions[0]
                .blocks
                .iter()
                .flat_map(|block| &block.insts)
                .any(|inst| {
                    matches!(
                        inst,
                        lir::Inst::CallRuntime {
                            function: lir::RuntimeFn::Rank,
                            ..
                        }
                    )
                })
        };
        let and = |operands| hir::Expr::Connective(hir::Connective::And, operands);
        assert!(ranks(and(vec![
            compared(hir::Comparison::Greater, half(), 0),
            compared(hir::Comparison::Less, half(), 5),
        ])));
        assert!(!ranks(and(vec![
            compared(
                hir::Comparison::Greater,
                arithmetic(hir::BinaryOp::Add(hir::Numeric::Integer), 1),
                0
            ),
            compared(
                hir::Comparison::Less,
                arithmetic(hir::BinaryOp::Multiply(hir::Numeric::Integer), 2),
                5
            ),
        ])));
    }

    #[test]
    fn read_only_operators_become_functions_once_called() {
        // `shout{t: text}: text [ t || "!" ]`, called when running
        // as `debug_value{v: shout{t: t}}`, and `unused`, which isn't.
        let text = || hir::Type::Scalar(hir::Scalar::Text);
        let read_only = |name: &str, value: hir::Expr| hir::Operator {
            versions: Vec::new(),
            constraints: Vec::new(),
            can_fail: value.can_fail(),
            name: name.to_string(),
            types: Vec::new(),
            params: [("t".to_string(), hir::Parameter::constant(text()))].into(),
            kind: hir::OperatorKind::ReadOnly {
                result: text(),
                locals: Vec::new(),
                value,
            },
        };
        let t = || Box::new(hir::Expr::Parameter("t".to_string()));
        let bang = hir::Expr::Constant(hir::Value::Text("!".to_string()));
        let shout = read_only(
            "shout",
            hir::Expr::Binary(hir::BinaryOp::Concatenate, t(), Box::new(bang)),
        );
        let shouted = hir::Expr::Call(hir::Call {
            operator: hir::OperatorId(1),
            types: Vec::new(),
            args: [("t".to_string(), hir::Expr::Variable(hir::VariableId(0)))].into(),
            updated: BTreeMap::new(),
            can_fail: true,
        });
        let lowered = lower_program(&program(
            vec![
                debug_value(),
                shout,
                read_only("unused", *t()),
                user("main", &[], vec![call(0, vec![("v", shouted)])]),
            ],
            vec![variable("t", hir::Value::Text("a".to_string()))],
        ));
        let names: Vec<&str> = lowered.functions.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, ["main", "shout", "__entry", "message"]);
        // `shout` stores its value where the caller says, made in a scope
        // of the call's own, which hands it out to the caller's before it
        // ends, so that it frees whatever else the call made.
        let shout = &lowered.functions[1];
        assert_eq!(shout.result, Some(scalar_layout(hir::Scalar::Text)));
        let insts = &shout.blocks[0].insts;
        assert_eq!(
            runtime_calls(shout)[0],
            [
                lir::RuntimeFn::Innermost,
                lir::RuntimeFn::Begin,
                lir::RuntimeFn::Concat,
                lir::RuntimeFn::HandOut,
                lir::RuntimeFn::End
            ]
        );
        let stored = insts.iter().position(|inst| {
            matches!(
                inst,
                lir::Inst::Copy {
                    dst: lir::Place {
                        base: lir::Base::Result,
                        ..
                    },
                    ..
                }
            )
        });
        let handed = insts.iter().position(|inst| {
            matches!(
                inst,
                lir::Inst::CallRuntime {
                    function: lir::RuntimeFn::HandOut,
                    ..
                }
            )
        });
        assert!(stored < handed, "{insts:#?}");
        // The call passes a slot for the result, and the calling
        // statement's scope owns what `shout` hands it.
        let insts = &lowered.functions[0].blocks[0].insts;
        assert_eq!(
            runtime_calls(&lowered.functions[0])[0],
            [
                lir::RuntimeFn::Innermost,
                lir::RuntimeFn::Begin,
                lir::RuntimeFn::DebugValue,
                lir::RuntimeFn::End
            ]
        );
        let Some(lir::Inst::Call {
            function,
            result: Some(result),
            ..
        }) = insts.iter().find(|i| matches!(i, lir::Inst::Call { .. }))
        else {
            panic!("{insts:#?}");
        };
        assert_eq!(*function, lir::FunctionId(1));
        let Some(lir::Inst::CallRuntime { args, .. }) = insts.get(insts.len() - 2) else {
            panic!("{insts:#?}");
        };
        assert_eq!(&args[0], result);
    }

    #[test]
    fn generic_operators_become_a_function_for_each_list_of_types() {
        // `show{v: any type} [ call debug_value{v: v}; ]`, called with an
        // integer twice, then with a text.
        let parameter = hir::TypeParameter {
            index: 0,
            name: None,
            any: hir::Any::Type,
        };
        let show = hir::Operator {
            versions: Vec::new(),
            constraints: Vec::new(),
            can_fail: true,
            name: "show".to_string(),
            types: vec![parameter.clone()],
            params: [(
                "v".to_string(),
                hir::Parameter::constant(hir::Type::Parameter(parameter.clone())),
            )]
            .into(),
            kind: hir::OperatorKind::Update {
                locals: Vec::new(),
                body: hir::Block {
                    statements: sited(vec![hir::Statement::Call(hir::Call {
                        operator: hir::OperatorId(0),
                        types: vec![hir::Type::Parameter(parameter)],
                        args: [("v".to_string(), hir::Expr::Parameter("v".to_string()))].into(),
                        updated: BTreeMap::new(),
                        can_fail: true,
                    })]),
                    locals: Vec::new(),
                },
            },
        };
        let text = hir::Type::Scalar(hir::Scalar::Text);
        let call = |ty: &hir::Type, value| {
            hir::Statement::Call(hir::Call {
                operator: hir::OperatorId(1),
                types: vec![ty.clone()],
                args: [("v".to_string(), hir::Expr::Constant(value))].into(),
                updated: BTreeMap::new(),
                can_fail: true,
            })
        };
        let main_body = vec![
            call(&integer(), hir::Value::Integer(1)),
            call(&integer(), hir::Value::Integer(2)),
            call(&text, hir::Value::Text("Lagos".to_string())),
        ];
        let lowered = lower_program(&program(
            vec![debug_value(), show, user("main", &[], main_body)],
            vec![],
        ));
        let names: Vec<&str> = lowered.functions.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "main",
                "show{v: integer}",
                "show{v: text}",
                "__entry",
                "message"
            ]
        );
        assert_eq!(
            lowered.functions[2].params,
            Some(lir::Layout::Struct(vec![scalar_layout(hir::Scalar::Text)]))
        );
        let called: Vec<lir::FunctionId> = lowered.functions[0].blocks[0]
            .insts
            .iter()
            .filter_map(|inst| match inst {
                lir::Inst::Call { function, .. } => Some(*function),
                _ => None,
            })
            .collect();
        assert_eq!(
            called,
            [lir::FunctionId(1), lir::FunctionId(1), lir::FunctionId(2)]
        );
    }

    #[test]
    fn count_is_the_relations_count_as_an_integer() {
        // `debug_value{v: count{r: r}}`, of a relation variable.
        let count = hir::Operator {
            versions: Vec::new(),
            constraints: Vec::new(),
            can_fail: false,
            name: "count".to_string(),
            types: hir::Builtin::ReadOnly(hir::ReadOnlyBuiltin::Count).types(),
            params: hir::Builtin::ReadOnly(hir::ReadOnlyBuiltin::Count).params(&library()),
            kind: hir::OperatorKind::Builtin(hir::Builtin::ReadOnly(hir::ReadOnlyBuiltin::Count)),
        };
        let tuple = |n| hir::TupleValue::new([("a".to_string(), hir::Value::Integer(n))].into());
        let r = hir::Value::Relation(hir::RelationValue {
            heading: hir::Heading::new([("a".to_string(), integer())].into()),
            tuples: [tuple(1), tuple(2)].into(),
        });
        let counted = hir::Expr::Call(hir::Call {
            operator: hir::OperatorId(1),
            types: vec![r.ty()],
            args: [("r".to_string(), hir::Expr::Variable(hir::VariableId(0)))].into(),
            updated: BTreeMap::new(),
            can_fail: false,
        });
        let lowered = lower_program(&program(
            vec![
                debug_value(),
                count,
                user("main", &[], vec![call(0, vec![("v", counted)])]),
            ],
            vec![variable("r", r)],
        ));
        assert_eq!(
            lowered.functions.len(),
            3,
            "a built-in has no function: `main`, the entry and `message` have"
        );
        assert_eq!(
            lowered.functions[0].blocks[0].insts[0],
            lir::Inst::Unary {
                dst: lir::Place::whole(lir::Base::Slot(lir::SlotId(0))),
                op: lir::UnaryOp::Integer,
                src: lir::Place {
                    base: lir::Base::Data(lir::DataId(1)),
                    path: vec![0, 1],
                },
            }
        );
    }

    #[test]
    fn max_keeps_the_greatest_and_fails_without_tuples() {
        // `debug_value{v: max{r: r, of: of}}`, of a relation variable whose
        // tuples have `of` beside their own attributes, as sema makes it.
        let max = hir::Operator {
            versions: Vec::new(),
            constraints: Vec::new(),
            can_fail: true,
            name: "max".to_string(),
            types: hir::Builtin::ReadOnly(hir::ReadOnlyBuiltin::Max).types(),
            params: hir::Builtin::ReadOnly(hir::ReadOnlyBuiltin::Max).params(&library()),
            kind: hir::OperatorKind::Builtin(hir::Builtin::ReadOnly(hir::ReadOnlyBuiltin::Max)),
        };
        let own = hir::Heading::new(BTreeMap::new());
        let tuple = |n| {
            hir::TupleValue::new(
                [
                    ("of".to_string(), hir::Value::Integer(n)),
                    (
                        hir::wrapped("r"),
                        hir::Value::Tuple(hir::TupleValue::new(BTreeMap::new())),
                    ),
                ]
                .into(),
            )
        };
        let r = hir::Value::Relation(hir::RelationValue {
            heading: hir::Heading::new(
                [
                    ("of".to_string(), integer()),
                    (hir::wrapped("r"), hir::Type::Tuple(own.clone())),
                ]
                .into(),
            ),
            tuples: [tuple(1), tuple(2)].into(),
        });
        let greatest = hir::Expr::Call(hir::Call {
            operator: hir::OperatorId(1),
            types: vec![integer(), hir::Type::Tuple(own)],
            args: [("r".to_string(), hir::Expr::Variable(hir::VariableId(0)))].into(),
            updated: BTreeMap::new(),
            can_fail: true,
        });
        let lowered = lower_program(&program(
            vec![
                debug_value(),
                max,
                user("main", &[], vec![call(0, vec![("v", greatest)])]),
            ],
            vec![variable("r", r)],
        ));
        assert_eq!(
            lowered.functions.len(),
            3,
            "a built-in has no function: `main`, the entry and `message` have"
        );
        let blocks = &lowered.functions[0].blocks;
        assert!(
            blocks
                .iter()
                .any(|b| b.terminator == lir::Terminator::Fail(Box::new(lir::Failure::NoMax))),
            "{blocks:#?}"
        );
        // Each tuple's `of` is compared with the greatest so far.
        assert!(
            blocks.iter().flat_map(|b| &b.insts).any(|i| matches!(
                i,
                lir::Inst::Binary {
                    op: lir::BinaryOp::Compare(
                        lir::Comparison::Greater,
                        lir::Operand::Numeric(lir::Numeric::Integer)
                    ),
                    ..
                }
            )),
            "{blocks:#?}"
        );
    }

    #[test]
    fn local_variables_hold_references_until_the_body_ends() {
        // `f{} [ var r: relation { v: integer }; var n := 1; call
        // debug_value{v: r}; ]`.
        let heading = hir::Heading::new([("v".to_string(), integer())].into());
        let r = hir::Local {
            name: "r".to_string(),
            ty: hir::Type::Relation(heading.clone()),
            keys: vec![["v".to_string()].into()],
            constant: false,
            deferred: false,
        };
        let n = hir::Local {
            name: "n".to_string(),
            ty: integer(),
            keys: Vec::new(),
            constant: false,
            deferred: false,
        };
        let declared = |id, value| {
            hir::Statement::Assign(vec![hir::Assignment {
                target: hir::Target::Local(hir::LocalId(id)),
                value: hir::Expr::Constant(value),
            }])
        };
        let empty = hir::Value::Relation(hir::RelationValue {
            heading,
            tuples: BTreeSet::new(),
        });
        let f = hir::Operator {
            versions: Vec::new(),
            constraints: Vec::new(),
            can_fail: true,
            name: "f".to_string(),
            types: Vec::new(),
            params: BTreeMap::new(),
            kind: hir::OperatorKind::Update {
                locals: vec![r, n],
                body: hir::Block {
                    statements: sited(vec![
                        declared(0, empty),
                        declared(1, hir::Value::Integer(1)),
                        call(0, vec![("v", hir::Expr::Local(hir::LocalId(0)))]),
                    ]),
                    locals: vec![hir::LocalId(0), hir::LocalId(1)],
                },
            },
        };
        let lowered = lower_program(&program(
            vec![debug_value(), f, user("main", &[], vec![call(1, vec![])])],
            Vec::new(),
        ));
        let f = lowered.functions.iter().find(|f| f.name == "f").unwrap();
        // Those of the blocks before its failure exit, the last, after the
        // scope it is entered in is saved.
        let (_failed, blocks) = f.blocks.split_last().unwrap();
        let insts: Vec<&lir::Inst> = blocks.iter().flat_map(|b| &b.insts).skip(1).collect();
        // The relation starts with a null pointer, is assigned its
        // value, and is released last; the integer is copied.
        let lir::Inst::Copy { dst: value, .. } = insts[0] else {
            panic!("{insts:#?}");
        };
        let assigned = insts.iter().find_map(|i| match i {
            lir::Inst::CallRuntime {
                function: lir::RuntimeFn::Assign,
                args,
            } => Some(&args[0]),
            _ => None,
        });
        assert_eq!(assigned, Some(value), "{insts:#?}");
        let lir::Inst::CallRuntime {
            function: lir::RuntimeFn::Release,
            args,
        } = insts.last().unwrap()
        else {
            panic!("{insts:#?}");
        };
        assert_eq!(args[0], *value);
        let releases = insts
            .iter()
            .filter(|i| {
                matches!(
                    i,
                    lir::Inst::CallRuntime {
                        function: lir::RuntimeFn::Release,
                        ..
                    }
                )
            })
            .count();
        assert_eq!(releases, 1, "only the relation holds references");
    }

    /// `half{n: integer}: integer [ n div 2 ]`, whose division can fail;
    /// `f{} [ var r: relation { v: integer }; var n := 1; call
    /// debug_value{v: half{n: n}}; ]`, whose call of it can; `g{} []`,
    /// where nothing can; and `main`, which calls `f` and `g`.
    fn halving() -> hir::Program {
        let heading = hir::Heading::new([("v".to_string(), integer())].into());
        let local = |name: &str, ty| hir::Local {
            name: name.to_string(),
            ty,
            keys: Vec::new(),
            constant: false,
            deferred: false,
        };
        let declared = |id, value| {
            hir::Statement::Assign(vec![hir::Assignment {
                target: hir::Target::Local(hir::LocalId(id)),
                value: hir::Expr::Constant(value),
            }])
        };
        let empty = hir::Value::Relation(hir::RelationValue {
            heading: heading.clone(),
            tuples: BTreeSet::new(),
        });
        let half = hir::Operator {
            versions: Vec::new(),
            constraints: Vec::new(),
            can_fail: true,
            name: "half".to_string(),
            types: Vec::new(),
            params: [("n".to_string(), hir::Parameter::constant(integer()))].into(),
            kind: hir::OperatorKind::ReadOnly {
                result: integer(),
                locals: Vec::new(),
                value: hir::Expr::Binary(
                    hir::BinaryOp::Div,
                    Box::new(hir::Expr::Parameter("n".to_string())),
                    Box::new(hir::Expr::Constant(hir::Value::Integer(2))),
                ),
            },
        };
        let halved = hir::Expr::Call(hir::Call {
            operator: hir::OperatorId(1),
            types: Vec::new(),
            args: [("n".to_string(), hir::Expr::Local(hir::LocalId(1)))].into(),
            updated: BTreeMap::new(),
            can_fail: true,
        });
        let f = hir::Operator {
            versions: Vec::new(),
            constraints: Vec::new(),
            can_fail: true,
            name: "f".to_string(),
            types: Vec::new(),
            params: BTreeMap::new(),
            kind: hir::OperatorKind::Update {
                locals: vec![
                    local("r", hir::Type::Relation(heading)),
                    local("n", integer()),
                ],
                body: hir::Block {
                    statements: sited(vec![
                        declared(0, empty),
                        declared(1, hir::Value::Integer(1)),
                        call(0, vec![("v", halved)]),
                    ]),
                    locals: vec![hir::LocalId(0), hir::LocalId(1)],
                },
            },
        };
        program(
            vec![
                debug_value(),
                half,
                f,
                user("g", &[], Vec::new()),
                user("main", &[], vec![call(2, vec![]), call(3, vec![])]),
            ],
            Vec::new(),
        )
    }

    /// The failure exit of `f`, its last block, which can't fail, where
    /// each block that can sends its failures.
    fn exit_of(f: &lir::Function) -> &lir::Block {
        let exit = lir::BlockId(f.blocks.len() - 1);
        for block in &f.blocks {
            let target = block.can_fail().then_some(exit);
            assert_eq!(block.failure, target, "{block:#?}");
        }
        let exit = f.blocks.last().unwrap();
        assert_eq!(exit.terminator, lir::Terminator::Failed);
        exit
    }

    #[test]
    fn a_failure_exit_gives_up_what_the_function_holds() {
        let lowered = lower_program(&halving());
        let function = |name: &str| lowered.functions.iter().find(|f| f.name == name).unwrap();
        // `f` saves the scope it is entered in, which its exit ends scopes
        // back to, then releases the relation, its one local variable
        // that holds references, then records the call in the failure's
        // trace: where `f` was called, and its name.
        let f = function("f");
        let lir::Inst::CallRuntime {
            function: lir::RuntimeFn::Innermost,
            args: saved,
        } = &f.blocks[0].insts[0]
        else {
            panic!("{:#?}", f.blocks[0]);
        };
        let r = f.blocks[0]
            .insts
            .iter()
            .find_map(|i| match i {
                lir::Inst::CallRuntime {
                    function: lir::RuntimeFn::Assign,
                    args,
                } => Some(args[0].clone()),
                _ => None,
            })
            .unwrap();
        let runtime: Vec<_> = exit_of(f)
            .insts
            .iter()
            .map(|i| match i {
                lir::Inst::CallRuntime { function, args } => (*function, args[0].clone()),
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(
            runtime[..2],
            [
                (lir::RuntimeFn::EndSince, saved[0].clone()),
                (lir::RuntimeFn::Release, r)
            ]
        );
        assert_eq!(runtime[2].0, lir::RuntimeFn::Trace);
        // `half` begins no scope and holds nothing, and neither does
        // `main`, whose calls pass nothing: their exits only record the
        // call and fail.
        for name in ["half", "main"] {
            let f = function(name);
            let traced: Vec<_> = exit_of(f)
                .insts
                .iter()
                .map(|i| match i {
                    lir::Inst::CallRuntime { function, .. } => *function,
                    other => panic!("{other:?}"),
                })
                .collect();
            assert_eq!(traced, [lir::RuntimeFn::Trace], "{name}");
            assert!(!f.blocks[0].insts.iter().any(|i| matches!(
                i,
                lir::Inst::CallRuntime {
                    function: lir::RuntimeFn::Innermost,
                    ..
                }
            )));
        }
        // Nothing in `g` can fail, so it has no exit.
        let g = function("g");
        assert!(
            g.blocks
                .iter()
                .all(|b| !b.can_fail() && b.failure.is_none())
        );
        assert!(
            !g.blocks
                .iter()
                .any(|b| b.terminator == lir::Terminator::Failed)
        );
    }

    #[test]
    fn a_let_computes_its_value_once() {
        // `f{p: integer}: integer [ var n := p + 1; n * n ]`.
        let n = || Box::new(hir::Expr::Local(hir::LocalId(0)));
        let value = hir::Expr::Let(
            hir::LocalId(0),
            Box::new(hir::Expr::Binary(
                hir::BinaryOp::Add(hir::Numeric::Integer),
                Box::new(hir::Expr::Parameter("p".to_string())),
                Box::new(hir::Expr::Constant(hir::Value::Integer(1))),
            )),
            Box::new(hir::Expr::Binary(
                hir::BinaryOp::Multiply(hir::Numeric::Integer),
                n(),
                n(),
            )),
        );
        let f = hir::Operator {
            versions: Vec::new(),
            constraints: Vec::new(),
            can_fail: value.can_fail(),
            name: "f".to_string(),
            types: Vec::new(),
            params: [("p".to_string(), hir::Parameter::constant(integer()))].into(),
            kind: hir::OperatorKind::ReadOnly {
                result: integer(),
                locals: vec![hir::Local {
                    name: "n".to_string(),
                    ty: integer(),
                    keys: Vec::new(),
                    constant: false,
                    deferred: false,
                }],
                value,
            },
        };
        let called = hir::Expr::Call(hir::Call {
            operator: hir::OperatorId(1),
            types: Vec::new(),
            args: [("p".to_string(), hir::Expr::Variable(hir::VariableId(0)))].into(),
            updated: BTreeMap::new(),
            can_fail: f.can_fail,
        });
        let lowered = lower_program(&program(
            vec![
                debug_value(),
                f,
                user("main", &[], vec![call(0, vec![("v", called)])]),
            ],
            vec![variable("x", hir::Value::Integer(2))],
        ));
        let f = lowered.functions.iter().find(|f| f.name == "f").unwrap();
        let binaries: Vec<&lir::Inst> = f
            .blocks
            .iter()
            .flat_map(|b| &b.insts)
            .filter(|i| matches!(i, lir::Inst::Binary { .. }))
            .collect();
        // One addition, whose result the multiplication takes twice.
        let [
            lir::Inst::Binary { dst: sum, .. },
            lir::Inst::Binary { lhs, rhs, .. },
        ] = binaries.as_slice()
        else {
            panic!("{binaries:#?}");
        };
        assert_eq!((lhs, rhs), (sum, sum));
    }

    #[test]
    fn a_let_frees_its_value_after_its_last_use() {
        // `f{t: text}: text [ var a := t || "!"; var b := a || "?"; b ]`.
        use lir::RuntimeFn::{Begin, Concat, End, HandOut, Innermost, Keep, Release, Retain};
        let text = || hir::Type::Scalar(hir::Scalar::Text);
        let local = |i| Box::new(hir::Expr::Local(hir::LocalId(i)));
        let concat = |lhs, bang: &str| {
            let bang = hir::Expr::Constant(hir::Value::Text(bang.to_string()));
            Box::new(hir::Expr::Binary(
                hir::BinaryOp::Concatenate,
                lhs,
                Box::new(bang),
            ))
        };
        let value = hir::Expr::Let(
            hir::LocalId(0),
            concat(Box::new(hir::Expr::Parameter("t".to_string())), "!"),
            Box::new(hir::Expr::Let(
                hir::LocalId(1),
                concat(local(0), "?"),
                local(1),
            )),
        );
        let named = |name: &str| hir::Local {
            name: name.to_string(),
            ty: text(),
            keys: Vec::new(),
            constant: false,
            deferred: false,
        };
        let f = hir::Operator {
            versions: Vec::new(),
            constraints: Vec::new(),
            can_fail: value.can_fail(),
            name: "f".to_string(),
            types: Vec::new(),
            params: [("t".to_string(), hir::Parameter::constant(text()))].into(),
            kind: hir::OperatorKind::ReadOnly {
                result: text(),
                locals: vec![named("a"), named("b")],
                value,
            },
        };
        let called = hir::Expr::Call(hir::Call {
            operator: hir::OperatorId(1),
            types: Vec::new(),
            args: [("t".to_string(), hir::Expr::Variable(hir::VariableId(0)))].into(),
            updated: BTreeMap::new(),
            can_fail: f.can_fail,
        });
        let lowered = lower_program(&program(
            vec![
                debug_value(),
                f,
                user("main", &[], vec![call(0, vec![("v", called)])]),
            ],
            vec![variable("x", hir::Value::Text("x".to_string()))],
        ));
        let f = lowered.functions.iter().find(|f| f.name == "f").unwrap();
        // Each binding's value made in a scope of its own, which it takes
        // references from; `a` given up once `b` is bound, as nothing
        // after reads it; and `b`, which the value is, kept by the call's
        // scope, then given up, before the call hands the value out.
        assert_eq!(
            runtime_calls(f)[0],
            [
                Innermost, Begin, Begin, Concat, Retain, End, Begin, Concat, Retain, End, Release,
                Keep, Release, HandOut, End
            ]
        );
    }

    #[test]
    fn sum_and_product_are_runtime_calls() {
        for (builtin, function) in [
            (
                hir::Builtin::ReadOnly(hir::ReadOnlyBuiltin::Sum),
                lir::RuntimeFn::Sum,
            ),
            (
                hir::Builtin::ReadOnly(hir::ReadOnlyBuiltin::Product),
                lir::RuntimeFn::Product,
            ),
        ] {
            assert_eq!(
                numbers_calls(builtin),
                [(function, 4), (lir::RuntimeFn::DebugValue, 2)]
            );
        }
    }

    /// The runtime calls of `debug_value{v: f{r: r, of: of}}`, for `f`,
    /// `sum` or `product`, of a relation variable whose tuples have `of`
    /// beside their own attributes, as sema makes it, with each one's
    /// number of arguments: into an integer's slot, of the relation, its
    /// descriptor and the position of `of`.
    fn numbers_calls(builtin: hir::Builtin) -> Vec<(lir::RuntimeFn, usize)> {
        let operator = hir::Operator {
            versions: Vec::new(),
            constraints: Vec::new(),
            can_fail: builtin.can_fail(),
            name: builtin.name().to_string(),
            types: builtin.types(),
            params: builtin.params(&library()),
            kind: hir::OperatorKind::Builtin(builtin),
        };
        let own = hir::Heading::new(BTreeMap::new());
        let r = hir::Value::Relation(hir::RelationValue {
            heading: hir::Heading::new(
                [
                    ("of".to_string(), integer()),
                    (hir::wrapped("r"), hir::Type::Tuple(own.clone())),
                ]
                .into(),
            ),
            tuples: BTreeSet::new(),
        });
        let folded = hir::Expr::Call(hir::Call {
            operator: hir::OperatorId(1),
            types: vec![integer(), hir::Type::Tuple(own)],
            args: [("r".to_string(), hir::Expr::Variable(hir::VariableId(0)))].into(),
            updated: BTreeMap::new(),
            can_fail: operator.can_fail,
        });
        let lowered = lower_program(&program(
            vec![
                debug_value(),
                operator,
                user("main", &[], vec![call(0, vec![("v", folded)])]),
            ],
            vec![variable("r", r)],
        ));
        assert_eq!(
            lowered.functions.len(),
            3,
            "a built-in has no function: `main`, the entry and `message` have"
        );
        lowered.functions[0].blocks[0]
            .insts
            .iter()
            .filter_map(|i| match i {
                lir::Inst::CallRuntime { function, args } => Some((*function, args.len())),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn conversions_make_text_in_a_scope_and_round_integers() {
        // `show{p: integer} [ call debug_value{v: to_text{from: p}};
        // call debug_value{v: to_approximate{from: p}}; ]`.
        let builtin = |builtin: hir::Builtin| hir::Operator {
            versions: Vec::new(),
            constraints: Vec::new(),
            can_fail: builtin.can_fail(),
            name: builtin.name().to_string(),
            types: builtin.types(),
            params: builtin.params(&library()),
            kind: hir::OperatorKind::Builtin(builtin),
        };
        let converted = |operator, builtin: hir::ReadOnlyBuiltin, types| {
            hir::Expr::Call(hir::Call {
                operator: hir::OperatorId(operator),
                types,
                args: [("from".to_string(), hir::Expr::Parameter("p".to_string()))].into(),
                updated: BTreeMap::new(),
                can_fail: builtin.can_fail(),
            })
        };
        let show = user(
            "show",
            &["p"],
            vec![
                call(
                    0,
                    vec![(
                        "v",
                        converted(1, hir::ReadOnlyBuiltin::ToText, vec![integer()]),
                    )],
                ),
                call(
                    0,
                    vec![(
                        "v",
                        converted(2, hir::ReadOnlyBuiltin::ToApproximate, Vec::new()),
                    )],
                ),
            ],
        );
        let main_body = vec![call(
            3,
            vec![("p", hir::Expr::Constant(hir::Value::Integer(3)))],
        )];
        let lowered = lower_program(&program(
            vec![
                debug_value(),
                builtin(hir::Builtin::ReadOnly(hir::ReadOnlyBuiltin::ToText)),
                builtin(hir::Builtin::ReadOnly(hir::ReadOnlyBuiltin::ToApproximate)),
                show,
                user("main", &[], main_body),
            ],
            vec![],
        ));
        let show = lowered.functions.iter().find(|f| f.name == "show").unwrap();
        let insts: Vec<&lir::Inst> = show.blocks.iter().flat_map(|b| &b.insts).collect();
        let runtime: Vec<lir::RuntimeFn> = insts
            .iter()
            .filter_map(|i| match i {
                lir::Inst::CallRuntime { function, .. } => Some(*function),
                _ => None,
            })
            .collect();
        // The text is the statement's, so it begins a scope; the integer
        // converts inline. Making the text can fail, so the scope the
        // function is entered in is saved first, which its failure exit
        // ends scopes back to.
        assert_eq!(
            runtime,
            [
                lir::RuntimeFn::Innermost,
                lir::RuntimeFn::Begin,
                lir::RuntimeFn::ToText,
                lir::RuntimeFn::DebugValue,
                lir::RuntimeFn::End,
                lir::RuntimeFn::DebugValue,
                lir::RuntimeFn::EndSince,
                lir::RuntimeFn::Trace,
            ]
        );
        assert!(
            insts.iter().any(|i| matches!(
                i,
                lir::Inst::Unary {
                    op: lir::UnaryOp::Approximate,
                    ..
                }
            )),
            "{insts:#?}"
        );
    }

    #[test]
    fn debug_value_passes_the_value_and_a_shared_descriptor() {
        let show_param = vec![call(0, vec![("v", hir::Expr::Parameter("p".to_string()))])];
        let main_body = vec![
            call(1, vec![("p", hir::Expr::Constant(hir::Value::Integer(3)))]),
            call(0, vec![("v", hir::Expr::Constant(hir::Value::Integer(4)))]),
        ];
        let lowered = lower_program(&program(
            vec![
                debug_value(),
                user("show", &["p"], show_param),
                user("main", &[], main_body),
            ],
            vec![],
        ));
        let show = &lowered.functions[1];
        let lir::Inst::CallRuntime { function, args } = &show.blocks[0].insts[0] else {
            panic!("expected a runtime call");
        };
        assert_eq!(*function, lir::RuntimeFn::DebugValue);
        assert_eq!(
            args[0],
            lir::Place {
                base: lir::Base::Params,
                path: vec![0]
            }
        );
        let lir::Base::Data(descriptor) = args[1].base else {
            panic!("the descriptor is data");
        };
        assert_eq!(
            lowered.data[descriptor.0].fields[..4],
            [
                lir::Field::U8(KIND_INTEGER),
                lir::Field::U8(0),
                lir::Field::Usize(1),
                lir::Field::Size(lir::Layout::I64)
            ]
        );
        // `main`'s call passes the same integer descriptor.
        let main = &lowered.functions[0];
        let lir::Inst::CallRuntime { args, .. } = &main.blocks[0].insts[2] else {
            panic!("expected a runtime call");
        };
        assert_eq!(args[1].base, lir::Base::Data(descriptor));
    }

    #[test]
    fn tuple_descriptors_have_offsets() {
        let heading = hir::Heading::new(
            [
                ("a".to_string(), hir::Type::Scalar(hir::Scalar::Boolean)),
                ("b".to_string(), integer()),
            ]
            .into(),
        );
        let mut cx = Lowering::new(
            &["test.trendy".to_string()],
            &BTreeMap::new(),
            HashMap::new(),
        );
        let id = cx.descriptor(&hir::Type::Tuple(heading));
        let lir::Field::Address(attributes) = cx.data[id.0].fields[4] else {
            panic!("attributes are an address");
        };
        let tuple = lir::Layout::Struct(vec![lir::Layout::U8, lir::Layout::I64]);
        let lir::Field::Struct(b) = &cx.data[attributes.0].fields[1] else {
            panic!("an attribute is a struct");
        };
        assert_eq!(b[2], lir::Field::Offset(tuple, vec![1]));
    }

    #[test]
    fn descriptors_point_to_their_elements_type() {
        let mut cx = Lowering::new(
            &["test.trendy".to_string()],
            &BTreeMap::new(),
            HashMap::new(),
        );
        let element = |cx: &Lowering, id: lir::DataId| {
            let lir::Field::Address(element) = cx.data[id.0].fields[7] else {
                panic!("the element's descriptor is an address");
            };
            cx.data[element.0].fields[0].clone()
        };
        let array = cx.descriptor(&hir::Type::Array(Box::new(integer())));
        assert_eq!(cx.data[array.0].fields[0], lir::Field::U8(KIND_ARRAY));
        assert_eq!(element(&cx, array), lir::Field::U8(KIND_INTEGER));
        // A relation's is its tuples' type.
        let heading = hir::Heading::new([("a".to_string(), integer())].into());
        let relation = cx.descriptor(&hir::Type::Relation(heading));
        assert_eq!(element(&cx, relation), lir::Field::U8(KIND_TUPLE));
        // Other types have none.
        let scalar = cx.descriptor(&integer());
        assert_eq!(cx.data[scalar.0].fields[7], lir::Field::Usize(0));
    }

    #[test]
    fn descriptors_say_when_walking_them_can_be_skipped() {
        let mut cx = Lowering::new(
            &["test.trendy".to_string()],
            &BTreeMap::new(),
            HashMap::new(),
        );
        let text = || hir::Type::Scalar(hir::Scalar::Text);
        let boolean = || hir::Type::Scalar(hir::Scalar::Boolean);
        let heading = |attributes: Vec<(&str, hir::Type)>| {
            hir::Heading::new(
                attributes
                    .into_iter()
                    .map(|(name, ty)| (name.to_string(), ty))
                    .collect(),
            )
        };
        let two = || heading(vec![("a", integer()), ("b", integer())]);
        let nested = hir::Type::Tuple(heading(vec![
            ("a", hir::Type::Tuple(two())),
            ("b", integer()),
        ]));
        for (ty, points, integers) in [
            (integer(), 0, 1),
            (boolean(), 0, 0),
            (text(), 1, 0),
            (hir::Type::Tuple(two()), 0, 2),
            (nested, 0, 3),
            // A boolean among them isn't a run of integers.
            (
                hir::Type::Tuple(heading(vec![("a", boolean()), ("b", integer())])),
                0,
                0,
            ),
            (hir::Type::Tuple(heading(vec![("a", text())])), 1, 0),
            (
                hir::Type::Tuple(heading(vec![("a", integer()), ("b", text())])),
                1,
                0,
            ),
            // A relation's are its tuples', which it points to.
            (hir::Type::Relation(two()), 1, 2),
            (
                hir::Type::Tuple(heading(vec![("r", hir::Type::Relation(two()))])),
                1,
                0,
            ),
            (hir::Type::Array(Box::new(integer())), 1, 0),
        ] {
            let id = cx.descriptor(&ty);
            assert_eq!(
                cx.data[id.0].fields[1..3],
                [lir::Field::U8(points), lir::Field::Usize(integers)],
                "{ty}"
            );
        }
    }

    #[test]
    fn an_array_selector_fills_memory_the_runtime_allocates() {
        // `show{p: integer} [ call debug_value{v: array [p, 2]}; ]`.
        let selector = hir::Expr::Array(
            integer(),
            vec![
                hir::Expr::Parameter("p".to_string()),
                hir::Expr::Constant(hir::Value::Integer(2)),
            ],
        );
        let lowered = lower_program(&program(
            vec![
                debug_value(),
                user("show", &["p"], vec![call(0, vec![("v", selector)])]),
                user(
                    "main",
                    &[],
                    vec![call(
                        1,
                        vec![("p", hir::Expr::Constant(hir::Value::Integer(1)))],
                    )],
                ),
            ],
            vec![],
        ));
        let insts = &lowered.functions[1].blocks[0].insts;
        let runtime: Vec<lir::RuntimeFn> = insts
            .iter()
            .filter_map(|inst| match inst {
                lir::Inst::CallRuntime { function, .. } => Some(*function),
                _ => None,
            })
            .collect();
        assert_eq!(
            runtime,
            [
                lir::RuntimeFn::Innermost,
                lir::RuntimeFn::Begin,
                lir::RuntimeFn::Allocate,
                lir::RuntimeFn::DebugValue,
                lir::RuntimeFn::End
            ]
        );
        let elements = insts
            .iter()
            .filter(|inst| matches!(inst, lir::Inst::Element { .. }))
            .count();
        assert_eq!(elements, 2);
    }

    #[test]
    fn an_index_checks_its_position_and_takes_the_elements_place() {
        // `show{i: integer} [ call debug_value{v: rainfall[i]}; ]`.
        let rainfall = hir::Value::Array(hir::ArrayValue {
            element: integer(),
            elements: vec![hir::Value::Integer(1200), hir::Value::Integer(950)],
        });
        let indexed = hir::Expr::Index(
            Box::new(hir::Expr::Variable(hir::VariableId(0))),
            Box::new(hir::Expr::Parameter("i".to_string())),
        );
        let lowered = lower_program(&program(
            vec![
                debug_value(),
                user("show", &["i"], vec![call(0, vec![("v", indexed)])]),
                user(
                    "main",
                    &[],
                    vec![call(
                        1,
                        vec![("i", hir::Expr::Constant(hir::Value::Integer(1)))],
                    )],
                ),
            ],
            vec![variable("rainfall", rainfall)],
        ));
        let blocks = &lowered.functions[1].blocks;
        assert!(
            blocks.iter().any(|b| matches!(
                &b.terminator,
                lir::Terminator::Fail(failure)
                    if matches!(**failure, lir::Failure::IndexOutOfRange { .. })
            )),
            "{blocks:#?}"
        );
        let insts: Vec<&lir::Inst> = blocks.iter().flat_map(|b| &b.insts).collect();
        assert!(
            insts.iter().any(|i| matches!(
                i,
                lir::Inst::Unary {
                    op: lir::UnaryOp::Usize,
                    ..
                }
            )),
            "{insts:#?}"
        );
        // The element is passed from its own place, with no copy.
        let Some(lir::Inst::CallRuntime { args, .. }) = insts.iter().find(|i| {
            matches!(
                i,
                lir::Inst::CallRuntime {
                    function: lir::RuntimeFn::DebugValue,
                    ..
                }
            )
        }) else {
            panic!("{insts:#?}");
        };
        assert!(
            matches!(args[0].base, lir::Base::Indirect { .. }),
            "{args:#?}"
        );
    }

    #[test]
    fn an_if_computes_each_branch_in_a_block_of_its_own() {
        // `show{p: integer} [ call debug_value{v: if p = 0 then 1 else -p}; ]`.
        let p = || Box::new(hir::Expr::Parameter("p".to_string()));
        let condition = hir::Expr::Binary(
            hir::BinaryOp::Compare(
                hir::Comparison::Equal,
                hir::Compared::Scalar(hir::Scalar::Integer),
            ),
            p(),
            Box::new(hir::Expr::Constant(hir::Value::Integer(0))),
        );
        let chosen = hir::Expr::If(
            Box::new(condition),
            Box::new(hir::Expr::Constant(hir::Value::Integer(1))),
            Box::new(hir::Expr::Unary(
                hir::UnaryOp::Negate(hir::Numeric::Integer),
                p(),
            )),
        );
        let lowered = lower_program(&program(
            vec![
                debug_value(),
                user("show", &["p"], vec![call(0, vec![("v", chosen)])]),
                user(
                    "main",
                    &[],
                    vec![call(
                        1,
                        vec![("p", hir::Expr::Constant(hir::Value::Integer(1)))],
                    )],
                ),
            ],
            vec![],
        ));
        let blocks = &lowered.functions[1].blocks;
        let lir::Terminator::Branch {
            then, otherwise, ..
        } = blocks[0].terminator
        else {
            panic!("{blocks:#?}");
        };
        // Only the `else` block negates, and both go on to one block.
        let negates = |b: lir::BlockId| {
            blocks[b.0].insts.iter().any(|i| {
                matches!(
                    i,
                    lir::Inst::Unary {
                        op: lir::UnaryOp::Negate(_),
                        ..
                    }
                )
            })
        };
        assert!(!negates(then) && negates(otherwise), "{blocks:#?}");
        assert_eq!(blocks[then.0].terminator, blocks[otherwise.0].terminator);
        let lir::Terminator::Jump(after) = blocks[then.0].terminator else {
            panic!("{blocks:#?}");
        };
        assert!(
            blocks[after.0].insts.iter().any(|i| matches!(
                i,
                lir::Inst::CallRuntime {
                    function: lir::RuntimeFn::DebugValue,
                    ..
                }
            )),
            "{blocks:#?}"
        );
    }

    #[test]
    fn a_guard_computes_the_next_only_if_it_doesnt_decide_the_result() {
        // `show{p: integer} [ call debug_value{v: p = 0 or p > 1}; ]`.
        let p = || Box::new(hir::Expr::Parameter("p".to_string()));
        let compare = |comparison, n| {
            hir::Expr::Binary(
                hir::BinaryOp::Compare(comparison, hir::Compared::Scalar(hir::Scalar::Integer)),
                p(),
                Box::new(hir::Expr::Constant(hir::Value::Integer(n))),
            )
        };
        let either = hir::Expr::Connective(
            hir::Connective::Or,
            vec![
                compare(hir::Comparison::Equal, 0),
                compare(hir::Comparison::Greater, 1),
            ],
        );
        let lowered = lower_program(&program(
            vec![
                debug_value(),
                user("show", &["p"], vec![call(0, vec![("v", either)])]),
                user(
                    "main",
                    &[],
                    vec![call(
                        1,
                        vec![("p", hir::Expr::Constant(hir::Value::Integer(1)))],
                    )],
                ),
            ],
            vec![],
        ));
        let blocks = &lowered.functions[1].blocks;
        let lir::Terminator::Branch {
            then, otherwise, ..
        } = blocks[0].terminator
        else {
            panic!("{blocks:#?}");
        };
        // `p > 1` is computed only when `p = 0` is false.
        let compares = |b: lir::BlockId| {
            blocks[b.0].insts.iter().any(|i| {
                matches!(
                    i,
                    lir::Inst::Binary {
                        op: lir::BinaryOp::Compare(..),
                        ..
                    }
                )
            })
        };
        assert!(!compares(then) && compares(otherwise), "{blocks:#?}");
    }

    #[test]
    fn an_if_statements_blocks_release_their_own_local_variables() {
        // `f{p: integer} [ if p = 0 then [ var t := "zero"; call
        // debug_value{v: t}; ] else [ call debug_value{v: p}; ]; ]`.
        let t = hir::Local {
            name: "t".to_string(),
            ty: hir::Type::Scalar(hir::Scalar::Text),
            keys: Vec::new(),
            constant: false,
            deferred: false,
        };
        let condition = hir::Expr::Binary(
            hir::BinaryOp::Compare(
                hir::Comparison::Equal,
                hir::Compared::Scalar(hir::Scalar::Integer),
            ),
            Box::new(hir::Expr::Parameter("p".to_string())),
            Box::new(hir::Expr::Constant(hir::Value::Integer(0))),
        );
        let then = hir::Block {
            statements: sited(vec![
                hir::Statement::Assign(vec![hir::Assignment {
                    target: hir::Target::Local(hir::LocalId(0)),
                    value: hir::Expr::Constant(hir::Value::Text("zero".to_string())),
                }]),
                call(0, vec![("v", hir::Expr::Local(hir::LocalId(0)))]),
            ]),
            locals: vec![hir::LocalId(0)],
        };
        let otherwise = hir::Block {
            statements: sited(vec![call(
                0,
                vec![("v", hir::Expr::Parameter("p".to_string()))],
            )]),
            locals: Vec::new(),
        };
        let f = hir::Operator {
            versions: Vec::new(),
            constraints: Vec::new(),
            can_fail: true,
            name: "f".to_string(),
            types: Vec::new(),
            params: [("p".to_string(), hir::Parameter::constant(integer()))].into(),
            kind: hir::OperatorKind::Update {
                locals: vec![t],
                body: hir::Block {
                    statements: sited(vec![hir::Statement::If(condition, then, otherwise)]),
                    locals: Vec::new(),
                },
            },
        };
        let lowered = lower_program(&program(
            vec![
                debug_value(),
                f,
                user(
                    "main",
                    &[],
                    vec![call(
                        1,
                        vec![("p", hir::Expr::Constant(hir::Value::Integer(0)))],
                    )],
                ),
            ],
            vec![],
        ));
        let blocks = &lowered.functions[1].blocks;
        let runtime = |b: lir::BlockId| -> Vec<lir::RuntimeFn> {
            blocks[b.0]
                .insts
                .iter()
                .filter_map(|inst| match inst {
                    lir::Inst::CallRuntime { function, .. } => Some(*function),
                    _ => None,
                })
                .collect()
        };
        let lir::Terminator::Branch {
            then, otherwise, ..
        } = blocks[0].terminator
        else {
            panic!("{blocks:#?}");
        };
        // `t` is assigned and released in its block, each statement in a
        // scope of its own; the other block has nothing to release.
        assert_eq!(
            runtime(then),
            [
                lir::RuntimeFn::Begin,
                lir::RuntimeFn::Assign,
                lir::RuntimeFn::End,
                lir::RuntimeFn::DebugValue,
                lir::RuntimeFn::Release,
            ]
        );
        assert_eq!(runtime(otherwise), [lir::RuntimeFn::DebugValue]);
    }

    /// A parameter's comparison with a number.
    fn compared(comparison: hir::Comparison, n: i64) -> hir::Expr {
        hir::Expr::Binary(
            hir::BinaryOp::Compare(comparison, hir::Compared::Scalar(hir::Scalar::Integer)),
            Box::new(hir::Expr::Parameter("v".to_string())),
            Box::new(hir::Expr::Constant(hir::Value::Integer(n))),
        )
    }

    /// What a type of one integer `v`, of a constraint of `condition`,
    /// stores it as, if not an integer, as `in_hierarchy` says.
    fn stored_as(condition: &hir::Expr, in_hierarchy: bool) -> Option<hir::Width> {
        let operator = hir::OperatorId(0);
        let mut bounds = IntegerBounds::new();
        bounded_by(condition, &mut bounds);
        let defined = hir::Defined {
            name: "t".to_string(),
            unit: String::new(),
            components: hir::Heading::new(BTreeMap::from([(
                "v".to_string(),
                hir::Type::Scalar(hir::Scalar::Integer),
            )])),
            representations: Vec::new(),
            constraints: vec![hir::Constraint {
                operator,
                text: String::new(),
            }],
            keys: BTreeMap::new(),
            order: None,
            supertypes: Vec::new(),
            specialized: in_hierarchy,
            specialization: None,
        };
        let stored = Families::new(&BTreeMap::new(), HashMap::from([(operator, bounds)]))
            .stored(&defined)?;
        match stored.get("v") {
            Some(hir::Type::Scalar(hir::Scalar::Narrow(width))) => Some(*width),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn constraints_store_an_integer_in_as_few_bytes_as_its_range_takes() {
        use hir::Comparison as C;
        use hir::Width as W;
        let both = |low: hir::Expr, high: hir::Expr| {
            hir::Expr::Connective(hir::Connective::And, vec![low, high])
        };
        for (low, high, width) in [
            ((C::GreaterOrEqual, 0), (C::LessOrEqual, 255), Some(W::U8)),
            ((C::GreaterOrEqual, 0), (C::Less, 256), Some(W::U8)),
            ((C::Greater, -1), (C::Less, 256), Some(W::U8)),
            ((C::Equal, 7), (C::Equal, 7), Some(W::U8)),
            ((C::GreaterOrEqual, 0), (C::LessOrEqual, 256), Some(W::U16)),
            (
                (C::GreaterOrEqual, 0),
                (C::LessOrEqual, 65_535),
                Some(W::U16),
            ),
            (
                (C::GreaterOrEqual, 0),
                (C::LessOrEqual, 65_536),
                Some(W::U32),
            ),
            ((C::GreaterOrEqual, 0), (C::Less, 1 << 32), Some(W::U32)),
            // Larger than any narrower one, or possibly negative, it stays.
            ((C::GreaterOrEqual, 0), (C::LessOrEqual, 1 << 32), None),
            ((C::GreaterOrEqual, -1), (C::LessOrEqual, 1), None),
            // A side no constraint limits is no range.
            ((C::GreaterOrEqual, 0), (C::NotEqual, 9), None),
        ] {
            let condition = both(compared(low.0, low.1), compared(high.0, high.1));
            assert_eq!(stored_as(&condition, false), width, "{low:?} and {high:?}");
        }
        // Only an `and`'s operands each hold.
        let either = hir::Expr::Connective(
            hir::Connective::Or,
            vec![compared(C::Equal, 1), compared(C::Equal, 2)],
        );
        assert_eq!(stored_as(&either, false), None);
        // A hierarchy's values are all laid out as its root's.
        let byte = both(
            compared(C::GreaterOrEqual, 0),
            compared(C::LessOrEqual, 255),
        );
        assert_eq!(stored_as(&byte, true), None);
    }
}
