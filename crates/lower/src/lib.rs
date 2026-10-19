//! Lowering: HIR to LIR. It chooses how each value is represented in memory,
//! turns statements into instructions, and builds the type descriptors that
//! runtime functions taking `any` receive.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use trendy_hir as hir;
use trendy_lir as lir;

/// Translate HIR to LIR. Infallible: sema has already rejected bad programs.
#[must_use]
pub fn lower(program: &hir::Program) -> lir::Program {
    let mut cx = Lowering::new();

    let mut variable_data = Vec::new();
    for variable in &program.variables {
        let mut fields = vec![cx.value_field(&variable.init)];
        // Where an assigned value's texts, relations and arrays are: none
        // yet, as the initial value's are constants.
        if points(&variable.ty) {
            fields.push(lir::Field::Usize(0));
        }
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
    let (entry, _) = function(program, &variable_data, program.entry, &[], &mut cx);
    lir::Program {
        functions: cx.functions.into_values().collect(),
        data: cx.data,
        entry,
    }
}

/// The function of the operator `id` instantiated with `types`, lowered
/// when first called, and whether a call of it makes a text, relation or
/// array that the calling statement owns, as a read-only operator's can.
///
/// An update operator's function runs each statement in a scope of its
/// own. A read-only operator's computes its value, then stores it where the
/// caller says, as a `result`; it begins no scope, so what it makes is the
/// calling statement's. Read-only operators don't call themselves, so
/// whether one makes something is known once its function is lowered,
/// before the calling statement ends.
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
    // Numbered before its body is lowered, so that a call of itself finds
    // it.
    let function_id = lir::FunctionId(cx.functions_of.len());
    cx.functions_of.insert(key.clone(), (function_id, false));

    let op = program.operator(id);
    let hir::Operator {
        name, params, kind, ..
    } = if types.is_empty() {
        op.clone()
    } else {
        op.instantiated(types)
    };
    // Instances of one operator are told apart by their parameters' types.
    let name = if types.is_empty() {
        name
    } else {
        format!("{name}{}", params_text(&params))
    };
    let mut builder = FunctionBuilder::new(program, name, params, variable_data);
    let (lowered, makes) = match kind {
        hir::OperatorKind::Update { locals, body } => {
            builder.declare(locals, cx);
            builder.block(&body, cx);
            (builder.finish(None), false)
        }
        hir::OperatorKind::ReadOnly {
            result,
            locals,
            value,
        } => {
            builder.bind(locals);
            let src = builder.place(&value, cx);
            builder.insts.push(lir::Inst::Copy {
                dst: lir::Place::whole(lir::Base::Result),
                src,
                layout: layout(&result),
            });
            let makes = builder.scoped;
            (builder.finish(Some(layout(&result))), makes)
        }
        hir::OperatorKind::Builtin(_) => unreachable!("built-ins are runtime calls"),
    };
    cx.functions.insert(function_id.0, lowered);
    cx.functions_of.insert(key, (function_id, makes));
    (function_id, makes)
}

/// Parameters as written, `{name: text, times: integer}`, with their
/// types in full, so that different types are written differently.
fn params_text(params: &BTreeMap<String, hir::Type>) -> String {
    format!("{{{}}}", attributes_text(params.iter()))
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
    data: Vec<lir::Data>,
    /// Each type's descriptor, built once.
    descriptors: HashMap<hir::Type, lir::DataId>,
    /// An empty constant, for addresses that are never read.
    empty: Option<lir::DataId>,
    /// The functions lowered, by id, which counts up from 0: one is added
    /// once its body is lowered, after those it calls.
    functions: BTreeMap<usize, lir::Function>,
    /// The function of each operator and list of types lowered code calls
    /// it with, and whether a call of it makes a text, relation or array.
    functions_of: HashMap<(hir::OperatorId, Vec<hir::Type>), (lir::FunctionId, bool)>,
}

impl Lowering {
    fn new() -> Self {
        Lowering {
            data: Vec::new(),
            descriptors: HashMap::new(),
            empty: None,
            functions: BTreeMap::new(),
            functions_of: HashMap::new(),
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

    /// The field representing `value`, adding any constants it points to.
    fn value_field(&mut self, value: &hir::Value) -> lir::Field {
        match value {
            hir::Value::Boolean(b) => lir::Field::U8(u8::from(*b)),
            hir::Value::Integer(n) => lir::Field::I64(*n),
            hir::Value::Approximate(r) => lir::Field::F64Bits(r.to_bits()),
            hir::Value::Text(text) => {
                let bytes = self.constant(vec![lir::Field::Bytes(text.as_bytes().to_vec())]);
                lir::Field::Struct(vec![
                    lir::Field::Address(bytes),
                    lir::Field::Usize(len(text.len())),
                ])
            }
            // Attributes come in name order, the heading's order.
            hir::Value::Tuple(tuple) => lir::Field::Struct(
                tuple
                    .attributes()
                    .map(|(_, value)| self.value_field(value))
                    .collect(),
            ),
            hir::Value::Relation(relation) => {
                // The tuples go in a read-only array, in their canonical order.
                let elements = relation
                    .tuples
                    .iter()
                    .map(|t| self.value_field(&hir::Value::Tuple(t.clone())))
                    .collect();
                let array = self.constant(elements);
                lir::Field::Struct(vec![
                    lir::Field::Address(array),
                    lir::Field::Usize(len(relation.tuples.len())),
                ])
            }
            hir::Value::Array(array) => {
                let elements = array.elements.iter().map(|e| self.value_field(e)).collect();
                let elements = self.constant(elements);
                lir::Field::Struct(vec![
                    lir::Field::Address(elements),
                    lir::Field::Usize(len(array.elements.len())),
                ])
            }
        }
    }

    /// The descriptor of `ty`, laid out as `trendy_runtime::TypeDesc`:
    /// `{ kind: u8, size, attributes: *Attr, count, element: *TypeDesc }`.
    fn descriptor(&mut self, ty: &hir::Type) -> lir::DataId {
        if let Some(&id) = self.descriptors.get(ty) {
            return id;
        }
        let (kind, heading, element) = match ty {
            hir::Type::Scalar(hir::Scalar::Boolean) => (KIND_BOOLEAN, None, None),
            hir::Type::Scalar(hir::Scalar::Integer) => (KIND_INTEGER, None, None),
            hir::Type::Scalar(hir::Scalar::Approximate) => (KIND_APPROXIMATE, None, None),
            hir::Type::Scalar(hir::Scalar::Text) => (KIND_TEXT, None, None),
            hir::Type::Tuple(heading) => (KIND_TUPLE, Some(heading), None),
            hir::Type::Relation(heading) => (
                KIND_RELATION,
                Some(heading),
                Some(hir::Type::Tuple(heading.clone())),
            ),
            hir::Type::Array(element) => (KIND_ARRAY, None, Some((**element).clone())),
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
        let id = self.constant(vec![
            lir::Field::U8(kind),
            lir::Field::Size(layout(ty)),
            lir::Field::Address(attributes),
            lir::Field::Usize(len(count)),
            element,
        ]);
        self.descriptors.insert(ty.clone(), id);
        id
    }

    /// A heading's attributes, laid out as an array of `trendy_runtime::Attr`:
    /// `{ name: *u8, name_len, offset, desc: *TypeDesc }`, in name order. A
    /// wrapped attribute, which the source can't name, has none.
    fn attributes(&mut self, heading: &hir::Heading) -> lir::DataId {
        let tuple = layout(&hir::Type::Tuple(heading.clone()));
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
                    lir::Field::Offset(tuple.clone(), i),
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

/// The part of lowering for one function.
struct FunctionBuilder<'a> {
    program: &'a hir::Program,
    /// The function's name, unique in the program.
    name: String,
    /// The parameters of the operator it is, with their types, which have
    /// no type parameters in them.
    params: BTreeMap<String, hir::Type>,
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
    /// makes a text, relation or array, which the statement owns until it
    /// ends, or passes one to an update operator. Only an assigned value
    /// outlives its statement, copied into memory of the variable's own.
    scoped: bool,
    /// The slot of the statements' scopes, one at a time.
    scope: Option<lir::Place>,
    /// Where each tuple at hand is, innermost last: what `Current` refers
    /// to, inside an `Extend`'s values.
    tuples: Vec<lir::Place>,
}

/// Where a local variable is: its value's slot, and, if it points to texts
/// or relations, the slot of the memory of its own that holds them, which
/// [`lir::RuntimeFn::Assign`] takes as the owner, as a variable's.
#[derive(Debug, Clone)]
struct LocalPlace {
    value: lir::Place,
    owner: Option<lir::Place>,
}

/// A block not built yet.
fn placeholder() -> lir::Block {
    lir::Block {
        insts: Vec::new(),
        terminator: lir::Terminator::Return,
    }
}

impl<'a> FunctionBuilder<'a> {
    /// The function `name`, of an operator with the parameters `params`,
    /// with an empty entry block.
    fn new(
        program: &'a hir::Program,
        name: String,
        params: BTreeMap<String, hir::Type>,
        variable_data: &'a [lir::DataId],
    ) -> Self {
        FunctionBuilder {
            program,
            name,
            params,
            variable_data,
            locals: Vec::new(),
            local_places: Vec::new(),
            slots: Vec::new(),
            blocks: vec![placeholder()],
            current: lir::BlockId(0),
            insts: Vec::new(),
            scoped: false,
            scope: None,
            tuples: Vec::new(),
        }
    }

    /// The type of `expr`, in the function.
    fn type_of(&self, expr: &hir::Expr) -> hir::Type {
        self.program.type_of(expr, &self.params, &self.locals)
    }

    /// Slots for the local variables `locals`, each with no memory of its
    /// own yet: what holds its texts, relations or arrays starts as null,
    /// as a variable's does, so that its declaration, the first assignment,
    /// has nothing to release.
    fn declare(&mut self, locals: Vec<hir::Local>, cx: &mut Lowering) {
        for local in locals {
            let value = self.slot(layout(&local.ty));
            let owner = points(&local.ty).then(|| {
                let owner = self.slot(lir::Layout::Address);
                self.insts.push(lir::Inst::Copy {
                    dst: owner.clone(),
                    src: usize_constant(0, cx),
                    layout: lir::Layout::Address,
                });
                owner
            });
            self.locals.push(local);
            self.local_places.push(Some(LocalPlace { value, owner }));
        }
    }

    /// The local variables `locals` of a read-only operator, each placed
    /// where its `Let`'s value is: computed once, by the expression, whose
    /// values its calling statement owns, or a variable's or a parameter's
    /// own place, which nothing changes while the expression is computed.
    fn bind(&mut self, locals: Vec<hir::Local>) {
        self.local_places = vec![None; locals.len()];
        self.locals = locals;
    }

    /// The statements of `block`, then the release of the memory of its
    /// local variables' own: nothing points into it once the block ends,
    /// as an assignment copies a value into memory of the variable's own,
    /// and every statement that could point into it has ended. Releasing
    /// empties the owner, so that one assigned again, when the block runs
    /// again, has nothing to release.
    fn block(&mut self, block: &hir::Block, cx: &mut Lowering) {
        for statement in &block.statements {
            self.statement(statement, cx);
        }
        for id in &block.locals {
            let place = self.local_places[id.0]
                .as_ref()
                .expect("an update operator's local variables are placed first");
            if let Some(owner) = &place.owner {
                self.insts.push(lir::Inst::CallRuntime {
                    function: lir::RuntimeFn::Release,
                    args: vec![owner.clone()],
                });
            }
        }
    }

    /// The function, its last block ended with a return, with a result
    /// laid out as `result`, if it has one.
    fn finish(mut self, result: Option<lir::Layout>) -> lir::Function {
        self.end_block(lir::Terminator::Return, lir::BlockId(0));
        lir::Function {
            name: self.name,
            params: params_layout(&self.params),
            result,
            slots: self.slots,
            blocks: self.blocks,
        }
    }

    /// A new block, to be built later.
    fn new_block(&mut self) -> lir::BlockId {
        self.blocks.push(placeholder());
        lir::BlockId(self.blocks.len() - 1)
    }

    /// End the current block with `terminator`, and go on building `next`.
    fn end_block(&mut self, terminator: lir::Terminator, next: lir::BlockId) {
        self.blocks[self.current.0] = lir::Block {
            insts: std::mem::take(&mut self.insts),
            terminator,
        };
        self.current = next;
    }

    /// The slot of the statements' scopes, made when one first needs it.
    fn scope_slot(&mut self) -> lir::Place {
        if let Some(scope) = &self.scope {
            return scope.clone();
        }
        let scope = self.slot(lir::Layout::scope());
        self.scope = Some(scope.clone());
        scope
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
        match statement {
            hir::Statement::Call(call) => self.scoped(|this| this.call(call, cx)),
            hir::Statement::Assign(assignments) => {
                self.scoped(|this| this.assign(assignments, cx));
            }
            hir::Statement::If(condition, then, otherwise) => {
                self.if_statement(condition, [then, otherwise], cx);
            }
        }
    }

    /// `if condition then [ … ] else [ … ];`, of the blocks `branches`:
    /// only the one taken, if the condition is a constant, or else a branch
    /// on it, computed in a scope of its own, which ends before either
    /// block runs, to a block of each, then on to one after both.
    fn if_statement(
        &mut self,
        condition: &hir::Expr,
        branches: [&hir::Block; 2],
        cx: &mut Lowering,
    ) {
        if let hir::Expr::Constant(hir::Value::Boolean(taken)) = condition {
            self.block(branches[usize::from(!taken)], cx);
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
            self.block(branch, cx);
            self.end_block(lir::Terminator::Jump(after), next);
        }
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
            hir::OperatorKind::Builtin(hir::Builtin::DebugValue) => {
                let value = &call.args["v"];
                let ty = self.type_of(value);
                let value = self.place(value, cx);
                let descriptor = lir::Place::whole(lir::Base::Data(cx.descriptor(&ty)));
                self.insts.push(lir::Inst::CallRuntime {
                    function: lir::RuntimeFn::DebugValue,
                    args: vec![value, descriptor],
                });
            }
            hir::OperatorKind::Update { .. } => {
                let params = params_of(callee, &call.types);
                // An assignment the operator makes must not release what
                // an argument points to.
                if params.values().any(points) {
                    self.scoped = true;
                }
                let (function, _) = function(
                    self.program,
                    self.variable_data,
                    call.operator,
                    &call.types,
                    cx,
                );
                let args = self.arguments(&params, &call.args, cx);
                self.insts.push(lir::Inst::Call {
                    function,
                    args,
                    result: None,
                });
            }
            hir::OperatorKind::ReadOnly { .. }
            | hir::OperatorKind::Builtin(
                hir::Builtin::Count
                | hir::Builtin::Length
                | hir::Builtin::Min
                | hir::Builtin::Max
                | hir::Builtin::Sum
                | hir::Builtin::Product
                | hir::Builtin::ToText
                | hir::Builtin::ToApproximate,
            ) => {
                unreachable!("`call` invokes only update operators")
            }
        }
    }

    /// The arguments of a call, for `params`: a tuple, built in a stack
    /// slot in parameter name order, whose address is passed, if there are
    /// any.
    fn arguments(
        &mut self,
        params: &BTreeMap<String, hir::Type>,
        args: &BTreeMap<String, hir::Expr>,
        cx: &mut Lowering,
    ) -> Option<lir::Place> {
        let slot = self.slot(params_layout(params)?);
        for (i, (name, ty)) in params.iter().enumerate() {
            let src = self.place(&args[name], cx);
            let mut dst = slot.clone();
            dst.path.push(i);
            self.insts.push(lir::Inst::Copy {
                dst,
                src,
                layout: layout(ty),
            });
        }
        Some(slot)
    }

    /// The value of `call`, of a read-only operator, computed into a new
    /// stack slot. What the operator makes is the statement's.
    fn read_only_call(&mut self, call: &hir::Call, cx: &mut Lowering) -> lir::Place {
        let callee = self.program.operator(call.operator);
        if let hir::OperatorKind::Builtin(builtin) = callee.kind {
            return self.builtin_value(builtin, call, cx);
        }
        let (function, makes) = function(
            self.program,
            self.variable_data,
            call.operator,
            &call.types,
            cx,
        );
        self.scoped |= makes;
        let args = self.arguments(&params_of(callee, &call.types), &call.args, cx);
        let dst = self.slot(layout(&self.program.result(call)));
        self.insts.push(lir::Inst::Call {
            function,
            args,
            result: Some(dst.clone()),
        });
        dst
    }

    /// The value of `call`, of the read-only built-in `builtin`, computed
    /// into a new stack slot.
    fn builtin_value(
        &mut self,
        builtin: hir::Builtin,
        call: &hir::Call,
        cx: &mut Lowering,
    ) -> lir::Place {
        match builtin {
            // The relation's or array's count, which it keeps after its
            // tuples or elements.
            hir::Builtin::Count | hir::Builtin::Length => {
                let operand = call.args.values().next().expect("one argument");
                let mut count = self.place(operand, cx);
                count.path.push(1);
                let dst = self.slot(lir::Layout::I64);
                self.insts.push(lir::Inst::Unary {
                    dst: dst.clone(),
                    op: lir::UnaryOp::Integer,
                    src: count,
                });
                dst
            }
            hir::Builtin::Min => {
                let r = &call.args["r"];
                self.extreme(r, hir::Comparison::Less, lir::Failure::NoMin, cx)
            }
            hir::Builtin::Max => {
                let r = &call.args["r"];
                self.extreme(r, hir::Comparison::Greater, lir::Failure::NoMax, cx)
            }
            hir::Builtin::Sum => self.numbers(lir::RuntimeFn::Sum, call, cx),
            hir::Builtin::Product => self.numbers(lir::RuntimeFn::Product, call, cx),
            hir::Builtin::ToText => {
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
            hir::Builtin::ToApproximate => {
                let src = self.place(&call.args["from"], cx);
                let dst = self.slot(lir::Layout::F64Bits);
                self.insts.push(lir::Inst::Unary {
                    dst: dst.clone(),
                    op: lir::UnaryOp::Approximate,
                    src,
                });
                dst
            }
            hir::Builtin::DebugValue => unreachable!("`debug_value` is an update operator"),
        }
    }

    /// The value of `call`, of `sum` or `product`, computed into a new
    /// stack slot by the runtime function `function`, which computes it as
    /// `hir::Builtin::evaluate` does, from the relation, its descriptor and
    /// the position of `of` among its attributes.
    fn numbers(
        &mut self,
        function: lir::RuntimeFn,
        call: &hir::Call,
        cx: &mut Lowering,
    ) -> lir::Place {
        let r = &call.args["r"];
        let ty = self.type_of(r);
        let hir::Type::Relation(heading) = &ty else {
            unreachable!("`sum` and `product` take relations");
        };
        let of = heading.get("of").expect("`of` is an attribute");
        let index = attribute_names(&ty)
            .position(|a| a == "of")
            .expect("`of` is an attribute");
        let dst = self.slot(layout(of));
        let src = self.place(r, cx);
        let descriptor = lir::Place::whole(lir::Base::Data(cx.descriptor(&ty)));
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
        let tuple = layout(&hir::Type::Tuple(heading.clone()));
        let value = layout(&of_ty);
        let src = self.place(r, cx);
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
        self.end_block(lir::Terminator::Fail(failure), some);
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
    /// variable that another assignment changes. Then each variable's value
    /// is checked against its keys, and only then assigned.
    fn assign(&mut self, assignments: &[hir::Assignment], cx: &mut Lowering) {
        let mut values: Vec<(hir::Target, lir::Place)> = Vec::new();
        for assignment in assignments {
            let value = self.place(&assignment.value, cx);
            let ty = layout(self.assigned(assignment.target).0);
            let slot = self.slot(ty.clone());
            self.insts.push(lir::Inst::Copy {
                dst: slot.clone(),
                src: value,
                layout: ty,
            });
            values.push((assignment.target, slot));
        }
        for (target, value) in &values {
            self.check_keys(*target, value, cx);
        }
        for (target, value) in values {
            self.store(target, value, cx);
        }
    }

    /// The type and keys of the variable `target`, of the program or local.
    fn assigned(&self, target: hir::Target) -> (&hir::Type, &[hir::Key]) {
        match target {
            hir::Target::Variable(id) => {
                let variable = self.program.variable(id);
                (&variable.ty, &variable.keys)
            }
            hir::Target::Local(id) => {
                let local = &self.locals[id.0];
                (&local.ty, &local.keys)
            }
        }
    }

    /// Stop the program if the relation at `value`, about to be assigned to
    /// the variable `target`, has two tuples with the same values for one
    /// of its keys: if its projection on the key has fewer tuples. A key of
    /// the whole heading always holds.
    fn check_keys(&mut self, target: hir::Target, value: &lir::Place, cx: &mut Lowering) {
        let (ty, keys) = self.assigned(target);
        let (ty, keys) = (ty.clone(), keys.to_vec());
        let ty = &ty;
        if !matches!(ty, hir::Type::Relation(_)) {
            return;
        }
        let names: Vec<&str> = attribute_names(ty).collect();
        for key in &keys {
            if key.len() == names.len() {
                continue;
            }
            let others: BTreeSet<String> = names
                .iter()
                .filter(|name| !key.contains(**name))
                .map(|name| (*name).to_string())
                .collect();
            let sources: Vec<usize> = names
                .iter()
                .enumerate()
                .filter(|(_, name)| key.contains(**name))
                .map(|(i, _)| i)
                .collect();
            let projected = self.reshape(ty, &ty.without(&others), &sources, value.clone(), cx);
            let (mut count, mut keys) = (value.clone(), projected);
            count.path.push(1);
            keys.path.push(1);
            let violated = self.slot(lir::Layout::U8);
            self.insts.push(lir::Inst::Binary {
                dst: violated.clone(),
                op: lir::BinaryOp::Compare(lir::Comparison::NotEqual, lir::Operand::Usize),
                lhs: count,
                rhs: keys,
            });
            let (fail, holds) = (self.new_block(), self.new_block());
            self.end_block(
                lir::Terminator::Branch {
                    condition: violated,
                    then: fail,
                    otherwise: holds,
                },
                fail,
            );
            self.end_block(lir::Terminator::Fail(lir::Failure::KeyViolated), holds);
        }
    }

    /// Assign the value at `value` to the variable `target`, of the
    /// program or local: copied, or, if it points to texts, relations or
    /// arrays, copied with them into memory of the variable's own.
    fn store(&mut self, target: hir::Target, value: lir::Place, cx: &mut Lowering) {
        let ty = self.assigned(target).0.clone();
        // Where the value goes, and what holds the memory of its own.
        let (dst, owner) = match target {
            hir::Target::Variable(id) => {
                let field = |i| lir::Place {
                    base: lir::Base::Data(self.variable_data[id.0]),
                    path: vec![i],
                };
                (field(0), Some(field(1)))
            }
            hir::Target::Local(id) => {
                let place = self.local_places[id.0]
                    .as_ref()
                    .expect("an update operator's local variables are placed first");
                (place.value.clone(), place.owner.clone())
            }
        };
        match owner.filter(|_| points(&ty)) {
            Some(owner) => {
                let descriptor = lir::Place::whole(lir::Base::Data(cx.descriptor(&ty)));
                self.insts.push(lir::Inst::CallRuntime {
                    function: lir::RuntimeFn::Assign,
                    args: vec![dst, owner, value, descriptor],
                });
                self.scoped = true;
            }
            None => self.insts.push(lir::Inst::Copy {
                dst,
                src: value,
                layout: layout(&ty),
            }),
        }
    }

    /// Where `expr`'s value is in memory: computed into a new stack slot, if
    /// it is computed.
    fn place(&mut self, expr: &hir::Expr, cx: &mut Lowering) -> lir::Place {
        match expr {
            hir::Expr::Constant(value) => {
                let field = cx.value_field(value);
                // A data object is the struct of its fields; the value is its
                // only field.
                lir::Place {
                    base: lir::Base::Data(cx.constant(vec![field])),
                    path: vec![0],
                }
            }
            hir::Expr::Variable(id) => lir::Place {
                base: lir::Base::Data(self.variable_data[id.0]),
                path: vec![0],
            },
            hir::Expr::Local(id) => self.local_places[id.0]
                .as_ref()
                .expect("a local variable is placed before it is used")
                .value
                .clone(),
            hir::Expr::Let(id, value, body) => {
                let value = self.place(value, cx);
                self.check_keys(hir::Target::Local(*id), &value, cx);
                self.local_places[id.0] = Some(LocalPlace { value, owner: None });
                self.place(body, cx)
            }
            hir::Expr::Parameter(name) => {
                let index = self
                    .params
                    .keys()
                    .position(|p| p == name)
                    .expect("sema resolved the parameter");
                lir::Place {
                    base: lir::Base::Params,
                    path: vec![index],
                }
            }
            hir::Expr::Unary(op, operand) => {
                let src = self.place(operand, cx);
                self.unary(*op, src)
            }
            // The attribute's own place within the tuple: no copy.
            hir::Expr::Extract(tuple, name) => {
                let ty = self.type_of(tuple);
                let index = attribute_names(&ty)
                    .position(|a| a == name)
                    .expect("sema extracts only attributes the tuple has");
                let mut place = self.place(tuple, cx);
                place.path.push(index);
                place
            }
            hir::Expr::Tuple(attributes) => {
                let ty = self.type_of(expr);
                let dst = self.slot(layout(&ty));
                self.fill_tuple(attributes, &dst, cx);
                dst
            }
            hir::Expr::Relation(heading, tuples) => self.relation(heading, tuples, cx),
            hir::Expr::Array(element, elements) => self.array(element, elements, cx),
            hir::Expr::Index(array, position) => self.index(array, position, cx),
            hir::Expr::Load(relation, items, _) => self.load(expr, relation, items, cx),
            hir::Expr::If(condition, then, otherwise) => {
                self.conditional(expr, condition, [then, otherwise], cx)
            }
            hir::Expr::Remove(operand, names) => self.removed(operand, names, cx),
            hir::Expr::Rename(operand, renamings) => self.renamed(operand, renamings, cx),
            hir::Expr::And(lhs, rhs) => self.joined(lhs, rhs, cx),
            hir::Expr::Ungroup(operand, names) => self.ungroup(operand, names, cx),
            hir::Expr::Only(operand) => self.only(operand, cx),
            hir::Expr::Semijoin(lhs, rhs) => self.matching(lhs, rhs, false, cx),
            hir::Expr::Combine(combination, lhs, rhs) => self.combine(*combination, lhs, rhs, cx),
            hir::Expr::Antijoin(lhs, rhs) => self.matching(lhs, rhs, true, cx),
            hir::Expr::Restrict(operand, condition) => self.restrict(operand, condition, cx),
            hir::Expr::Current(depth, _) => self.tuples[self.tuples.len() - 1 - depth].clone(),
            hir::Expr::Call(call) => self.read_only_call(call, cx),
            hir::Expr::Extend(operand, _, attributes) => self.extend(expr, operand, attributes, cx),
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
        let value = layout(&self.type_of(expr));
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

    /// `operand` without the attributes `names`, reshaped.
    fn removed(
        &mut self,
        operand: &hir::Expr,
        names: &BTreeSet<String>,
        cx: &mut Lowering,
    ) -> lir::Place {
        let ty = self.type_of(operand);
        let src = self.place(operand, cx);
        // The kept attributes, in their own order, which is the result's
        // too.
        let sources: Vec<usize> = attribute_names(&ty)
            .enumerate()
            .filter(|(_, name)| !names.contains(*name))
            .map(|(i, _)| i)
            .collect();
        self.reshape(&ty, &ty.without(names), &sources, src, cx)
    }

    /// `operand` with its attributes renamed as `renamings` says, each
    /// `old` to `new`, reshaped.
    fn renamed(
        &mut self,
        operand: &hir::Expr,
        renamings: &BTreeMap<String, String>,
        cx: &mut Lowering,
    ) -> lir::Place {
        let ty = self.type_of(operand);
        let src = self.place(operand, cx);
        let result_ty = ty.renamed(renamings);
        // Each result attribute comes from the operand's attribute with its
        // old name.
        let old: Vec<&str> = attribute_names(&ty).collect();
        let sources: Vec<usize> = attribute_names(&result_ty)
            .map(|new| {
                let before = renamings
                    .iter()
                    .find(|(_, after)| after.as_str() == new)
                    .map_or(new, |(before, _)| before.as_str());
                old.iter()
                    .position(|name| *name == before)
                    .expect("sema renames only attributes the operand has")
            })
            .collect();
        self.reshape(&ty, &result_ty, &sources, src, cx)
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
                layout: layout(&ty),
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
        let tuple_layout = layout(&hir::Type::Tuple(heading.clone()));
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
        let dst = self.slot(layout(&ty));
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
        let element_layout = layout(element);
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
            rhs: length,
        });
        let outside = self.slot(lir::Layout::U8);
        self.insts.push(lir::Inst::Binary {
            dst: outside.clone(),
            op: lir::BinaryOp::Or,
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
        self.end_block(lir::Terminator::Fail(lir::Failure::IndexOutOfRange), inside);
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
        self.element(&src, &index, layout(&element))
    }

    /// `expr`, `operand` with `attributes` added or replaced, computed into
    /// a new stack slot.
    fn extend(
        &mut self,
        expr: &hir::Expr,
        operand: &hir::Expr,
        attributes: &BTreeMap<String, hir::Expr>,
        cx: &mut Lowering,
    ) -> lir::Place {
        let ty = self.type_of(operand);
        let result_ty = self.type_of(expr);
        let src = self.place(operand, cx);
        match (&ty, &result_ty) {
            (hir::Type::Tuple(heading), hir::Type::Tuple(result)) => {
                let dst = self.slot(layout(&result_ty));
                self.extended(heading, &src, attributes, result, &dst, cx);
                dst
            }
            (hir::Type::Relation(heading), hir::Type::Relation(result)) => {
                self.extend_relation(heading, &src, attributes, result, cx)
            }
            _ => unreachable!("sema extends only tuples and relations"),
        }
    }

    /// Build at `dst` the tuple at `src`, with `heading`, with `attributes`
    /// added or replaced: each attribute of `result`, in name order, is
    /// computed with `src` as the tuple at hand, or else copied from `src`.
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
        self.tuples.push(src.clone());
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
                layout: layout(ty),
            });
        }
        self.tuples.pop();
    }

    /// The relation at `src`, with `heading`, with `attributes` added to or
    /// replaced in each tuple, making one with the attributes of `result`: a
    /// loop over the tuples, building each new tuple in a new relation,
    /// which the runtime allocates and then sorts into the canonical order,
    /// without the duplicates a `replace` can make.
    fn extend_relation(
        &mut self,
        heading: &hir::Heading,
        src: &lir::Place,
        attributes: &BTreeMap<String, hir::Expr>,
        result: &hir::Heading,
        cx: &mut Lowering,
    ) -> lir::Place {
        let tuple = layout(&hir::Type::Tuple(heading.clone()));
        let result_tuple = layout(&hir::Type::Tuple(result.clone()));
        let result_ty = hir::Type::Relation(result.clone());
        let mut count = src.clone();
        count.path.push(1);
        let dst = self.allocated(&result_ty, count, cx);
        self.each_tuple(src, tuple, cx, |this, at_hand, index, cx| {
            let to = this.element(&dst, index, result_tuple);
            this.extended(heading, &at_hand, attributes, result, &to, cx);
        });
        let descriptor = lir::Place::whole(lir::Base::Data(cx.descriptor(&result_ty)));
        self.insts.push(lir::Inst::CallRuntime {
            function: lir::RuntimeFn::Sort,
            args: vec![dst.clone(), descriptor],
        });
        dst
    }

    /// The tuples of the relation `operand` for which `condition`, computed
    /// with each as the tuple at hand, is true, copied into a new relation.
    /// They stay in the canonical order, so need no sorting; the relation's
    /// count is then the number copied.
    fn restrict(
        &mut self,
        operand: &hir::Expr,
        condition: &hir::Expr,
        cx: &mut Lowering,
    ) -> lir::Place {
        let ty = self.type_of(operand);
        let hir::Type::Relation(heading) = &ty else {
            unreachable!("sema restricts only relations");
        };
        let tuple = layout(&hir::Type::Tuple(heading.clone()));
        let src = self.place(operand, cx);
        let mut count = src.clone();
        count.path.push(1);
        let dst = self.allocated(&ty, count, cx);
        let kept = self.slot(lir::Layout::Usize);
        let zero = usize_constant(0, cx);
        self.insts.push(lir::Inst::Copy {
            dst: kept.clone(),
            src: zero,
            layout: lir::Layout::Usize,
        });
        self.each_tuple(&src, tuple.clone(), cx, |this, at_hand, _, cx| {
            this.tuples.push(at_hand.clone());
            let keep = this.place(condition, cx);
            this.tuples.pop();
            let (copy, next) = (this.new_block(), this.new_block());
            this.end_block(
                lir::Terminator::Branch {
                    condition: keep,
                    then: copy,
                    otherwise: next,
                },
                copy,
            );
            let to = this.element(&dst, &kept, tuple.clone());
            this.insts.push(lir::Inst::Copy {
                dst: to,
                src: at_hand,
                layout: tuple,
            });
            this.insts.push(lir::Inst::Increment {
                place: kept.clone(),
            });
            this.end_block(lir::Terminator::Jump(next), next);
        });
        let mut len = dst.clone();
        len.path.push(1);
        self.insts.push(lir::Inst::Copy {
            dst: len,
            src: kept,
            layout: lir::Layout::Usize,
        });
        dst
    }

    /// A new relation or array of type `ty` with room for `count` tuples or
    /// elements, a [`lir::Layout::Usize`], zeroed, which the runtime
    /// allocates for the statement.
    fn allocated(&mut self, ty: &hir::Type, count: lir::Place, cx: &mut Lowering) -> lir::Place {
        let descriptor = lir::Place::whole(lir::Base::Data(cx.descriptor(ty)));
        let dst = self.slot(layout(ty));
        self.insts.push(lir::Inst::CallRuntime {
            function: lir::RuntimeFn::Allocate,
            args: vec![dst.clone(), count, descriptor],
        });
        self.scoped = true;
        dst
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

    /// The only tuple of the relation `operand`, copied into a new stack
    /// slot. The program stops if the relation has none, or several.
    fn only(&mut self, operand: &hir::Expr, cx: &mut Lowering) -> lir::Place {
        let ty = self.type_of(operand);
        let hir::Type::Relation(heading) = &ty else {
            unreachable!("sema takes the only tuple of relations only");
        };
        let src = self.place(operand, cx);
        let mut count = src.clone();
        count.path.push(1);
        let (one, zero) = (usize_constant(1, cx), usize_constant(0, cx));
        let compare = |this: &mut Self, rhs: &lir::Place| {
            let equal = this.slot(lir::Layout::U8);
            this.insts.push(lir::Inst::Binary {
                dst: equal.clone(),
                op: lir::BinaryOp::Compare(lir::Comparison::Equal, lir::Operand::Usize),
                lhs: count.clone(),
                rhs: rhs.clone(),
            });
            equal
        };
        let is_one = compare(self, &one);
        let (single, other) = (self.new_block(), self.new_block());
        self.end_block(
            lir::Terminator::Branch {
                condition: is_one,
                then: single,
                otherwise: other,
            },
            other,
        );
        let is_zero = compare(self, &zero);
        let (none, several) = (self.new_block(), self.new_block());
        self.end_block(
            lir::Terminator::Branch {
                condition: is_zero,
                then: none,
                otherwise: several,
            },
            none,
        );
        self.end_block(lir::Terminator::Fail(lir::Failure::NoTuple), several);
        self.end_block(lir::Terminator::Fail(lir::Failure::SeveralTuples), single);
        let tuple = layout(&hir::Type::Tuple(heading.clone()));
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
        cx: &mut Lowering,
    ) -> lir::Place {
        let (hir::Type::Tuple(heading) | hir::Type::Relation(heading)) = ty else {
            unreachable!("sema reshapes only tuples and relations");
        };
        let attribute_types: Vec<&hir::Type> = heading.attributes().map(|(_, t)| t).collect();
        let dst = self.slot(layout(result_ty));
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
                    layout: layout(attribute_types[i]),
                });
            }
            return dst;
        }
        let tuple_layout = layout(&hir::Type::Tuple(heading.clone()));
        let offsets = cx.constant(
            sources
                .iter()
                .map(|&i| lir::Field::Offset(tuple_layout.clone(), i))
                .collect(),
        );
        let result_desc = lir::Place::whole(lir::Base::Data(cx.descriptor(result_ty)));
        let source_desc = lir::Place::whole(lir::Base::Data(cx.descriptor(ty)));
        self.insts.push(lir::Inst::CallRuntime {
            function: lir::RuntimeFn::Reshape,
            args: vec![
                dst.clone(),
                src,
                result_desc,
                source_desc,
                lir::Place::whole(lir::Base::Data(offsets)),
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
        lhs_ty: &hir::Type,
        rhs_ty: &hir::Type,
        lhs: lir::Place,
        rhs: lir::Place,
        cx: &mut Lowering,
    ) -> lir::Place {
        let (hir::Type::Relation(lhs_heading), hir::Type::Relation(rhs_heading)) = (lhs_ty, rhs_ty)
        else {
            unreachable!("sema joins only relations");
        };
        let result_ty = lhs_ty.and(rhs_ty);
        let lhs_tuple = layout(&hir::Type::Tuple(lhs_heading.clone()));
        let rhs_tuple = layout(&hir::Type::Tuple(rhs_heading.clone()));
        let position =
            |heading: &hir::Heading, name: &str| heading.attributes().position(|(a, _)| a == name);
        let sources = attribute_names(&result_ty)
            .map(|name| {
                let (from_rhs, offset) = position(lhs_heading, name).map_or_else(
                    || {
                        let i = position(rhs_heading, name)
                            .expect("each attribute of the result is an operand's");
                        (1, lir::Field::Offset(rhs_tuple.clone(), i))
                    },
                    |i| (0, lir::Field::Offset(lhs_tuple.clone(), i)),
                );
                lir::Field::Struct(vec![lir::Field::Usize(from_rhs), offset])
            })
            .collect();
        let common = common_attributes(lhs_heading, rhs_heading, cx);
        let common_count = cx.constant(vec![lir::Field::Usize(len(common.len()))]);
        let descriptor = |ty: &hir::Type, cx: &mut Lowering| {
            lir::Place::whole(lir::Base::Data(cx.descriptor(ty)))
        };
        let args = vec![
            lhs,
            rhs,
            descriptor(&result_ty, cx),
            descriptor(lhs_ty, cx),
            descriptor(rhs_ty, cx),
            lir::Place::whole(lir::Base::Data(cx.constant(sources))),
            lir::Place::whole(lir::Base::Data(cx.constant(common))),
            lir::Place {
                base: lir::Base::Data(common_count),
                path: vec![0],
            },
        ];
        let dst = self.slot(layout(&result_ty));
        self.insts.push(lir::Inst::CallRuntime {
            function: lir::RuntimeFn::And,
            args: [vec![dst.clone()], args].concat(),
        });
        self.scoped = true;
        dst
    }

    /// `expr`, the array of the tuples of `relation`, or of their values,
    /// in the order of `items`, computed by the runtime into a new stack
    /// slot. Lowering works out each item's offset and type, so the runtime
    /// never finds attributes by name. The values of a relation of one
    /// attribute are its tuples, laid out the same.
    fn load(
        &mut self,
        expr: &hir::Expr,
        relation: &hir::Expr,
        items: &[hir::OrderItem],
        cx: &mut Lowering,
    ) -> lir::Place {
        let relation_ty = self.type_of(relation);
        let hir::Type::Relation(heading) = &relation_ty else {
            unreachable!("sema loads only relations");
        };
        let tuple = layout(&hir::Type::Tuple(heading.clone()));
        let items: Vec<lir::Field> = items
            .iter()
            .map(|item| {
                let (i, (_, ty)) = heading
                    .attributes()
                    .enumerate()
                    .find(|(_, (name, _))| *name == item.attribute)
                    .expect("sema orders only by the relation's attributes");
                lir::Field::Struct(vec![
                    lir::Field::Offset(tuple.clone(), i),
                    lir::Field::Address(cx.descriptor(ty)),
                    lir::Field::Usize(u64::from(item.descending)),
                ])
            })
            .collect();
        let count = usize_constant(len(items.len()), cx);
        let src = self.place(relation, cx);
        let dst = self.slot(layout(&self.type_of(expr)));
        let args = vec![
            dst.clone(),
            src,
            lir::Place::whole(lir::Base::Data(cx.descriptor(&relation_ty))),
            lir::Place::whole(lir::Base::Data(cx.constant(items))),
            count,
        ];
        self.insts.push(lir::Inst::CallRuntime {
            function: lir::RuntimeFn::Load,
            args,
        });
        self.scoped = true;
        dst
    }

    /// The tuples of the relation `lhs` that have a match in `rhs`, or, if
    /// `negated`, those that have none, computed by the runtime into a new
    /// stack slot.
    fn matching(
        &mut self,
        lhs: &hir::Expr,
        rhs: &hir::Expr,
        negated: bool,
        cx: &mut Lowering,
    ) -> lir::Place {
        let lhs_ty = self.type_of(lhs);
        let rhs_ty = self.type_of(rhs);
        let (hir::Type::Relation(lhs_heading), hir::Type::Relation(rhs_heading)) =
            (&lhs_ty, &rhs_ty)
        else {
            unreachable!("sema matches only relations");
        };
        let common = common_attributes(lhs_heading, rhs_heading, cx);
        let common_count = usize_constant(len(common.len()), cx);
        let negated = usize_constant(u64::from(negated), cx);
        let lhs = self.place(lhs, cx);
        let rhs = self.place(rhs, cx);
        let descriptor = |ty: &hir::Type, cx: &mut Lowering| {
            lir::Place::whole(lir::Base::Data(cx.descriptor(ty)))
        };
        let dst = self.slot(layout(&lhs_ty));
        let args = vec![
            dst.clone(),
            lhs,
            rhs,
            descriptor(&lhs_ty, cx),
            descriptor(&rhs_ty, cx),
            lir::Place::whole(lir::Base::Data(cx.constant(common))),
            common_count,
            negated,
        ];
        self.insts.push(lir::Inst::CallRuntime {
            function: lir::RuntimeFn::Matching,
            args,
        });
        self.scoped = true;
        dst
    }

    /// The relations `lhs` and `rhs`, of one type, combined as
    /// `combination` says, computed by the runtime into a new stack slot.
    fn combine(
        &mut self,
        combination: hir::Combination,
        lhs: &hir::Expr,
        rhs: &hir::Expr,
        cx: &mut Lowering,
    ) -> lir::Place {
        let ty = self.type_of(lhs);
        let lhs = self.place(lhs, cx);
        let rhs = self.place(rhs, cx);
        let descriptor = lir::Place::whole(lir::Base::Data(cx.descriptor(&ty)));
        let combination = match combination {
            hir::Combination::Union => lir::Combination::Union,
            hir::Combination::DisjointUnion => lir::Combination::DisjointUnion,
            hir::Combination::ExclusiveUnion => lir::Combination::ExclusiveUnion,
            hir::Combination::Minus => lir::Combination::Minus,
            hir::Combination::IncludedMinus => lir::Combination::IncludedMinus,
            hir::Combination::DisjointChanges => lir::Combination::DisjointChanges,
        };
        let combination = usize_constant(combination.code(), cx);
        let dst = self.slot(layout(&ty));
        self.insts.push(lir::Inst::CallRuntime {
            function: lir::RuntimeFn::Combine,
            args: vec![dst.clone(), lhs, rhs, descriptor, combination],
        });
        self.scoped = true;
        dst
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
        let outer = layout(&hir::Type::Tuple(heading.clone()));
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
                (at, inner, layout(&hir::Type::Tuple(inner.clone())))
            })
            .collect();
        let sources = attribute_names(&result_ty)
            .map(|name| {
                let (from, offset) = match position(&heading.without(names), name) {
                    Some(_) => (
                        0,
                        lir::Field::Offset(
                            outer.clone(),
                            position(heading, name).expect("a kept attribute is the operand's"),
                        ),
                    ),
                    None => inner
                        .iter()
                        .enumerate()
                        .find_map(|(k, (_, inner, tuple))| {
                            let i = position(inner, name)?;
                            Some((len(k + 1), lir::Field::Offset(tuple.clone(), i)))
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
                    lir::Field::Offset(outer.clone(), *at),
                    lir::Field::Size(tuple.clone()),
                ])
            })
            .collect();
        let count = cx.constant(vec![lir::Field::Usize(len(ungrouped.len()))]);
        let descriptor = |ty: &hir::Type, cx: &mut Lowering| {
            lir::Place::whole(lir::Base::Data(cx.descriptor(ty)))
        };
        let dst = self.slot(layout(&result_ty));
        let args = vec![
            dst.clone(),
            src,
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
        dst
    }

    /// `lhs` and `rhs` joined, computed into a new stack slot: the AND of
    /// relations, by the runtime, or the join of tuples.
    fn joined(&mut self, lhs: &hir::Expr, rhs: &hir::Expr, cx: &mut Lowering) -> lir::Place {
        let lhs_ty = self.type_of(lhs);
        let rhs_ty = self.type_of(rhs);
        let lhs = self.place(lhs, cx);
        let rhs = self.place(rhs, cx);
        match lhs_ty {
            hir::Type::Tuple(_) => self.tuple_join(&lhs_ty, &rhs_ty, &lhs, &rhs, cx),
            _ => self.and(&lhs_ty, &rhs_ty, lhs, rhs, cx),
        }
    }

    /// The join of the tuples at `lhs` and `rhs`, of types `lhs_ty` and
    /// `rhs_ty`, computed into a new stack slot: each common attribute is
    /// compared first, and the program stops if they differ, since no
    /// tuple would do; then each attribute is copied from the operand that
    /// has it.
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
            self.end_block(lir::Terminator::Fail(lir::Failure::TupleMismatch), agree);
        }
        let result_ty = lhs_ty.and(rhs_ty);
        let hir::Type::Tuple(result) = &result_ty else {
            unreachable!("a tuple join is a tuple");
        };
        let dst = self.slot(layout(&result_ty));
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
                layout: layout(ty),
            });
        }
        dst
    }

    /// `op src`, computed into a new stack slot.
    fn unary(&mut self, op: hir::UnaryOp, src: lir::Place) -> lir::Place {
        let op = match op {
            hir::UnaryOp::Not => lir::UnaryOp::Not,
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
            ) => {
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
                return dst;
            }
            hir::BinaryOp::Compare(
                comparison,
                hir::Compared::Tuple | hir::Compared::Relation | hir::Compared::Array,
            ) => {
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
                    op: lir::UnaryOp::Not,
                    src: equal,
                });
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
            hir::BinaryOp::Compare(comparison, hir::Compared::Scalar(hir::Scalar::Text)) => {
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
        hir::BinaryOp::And => lir::BinaryOp::And,
        hir::BinaryOp::Or => lir::BinaryOp::Or,
        hir::BinaryOp::Xor => lir::BinaryOp::Xor,
        hir::BinaryOp::Implies => lir::BinaryOp::Implies,
        hir::BinaryOp::Add(numeric) => lir::BinaryOp::Add(lower_numeric(numeric)),
        hir::BinaryOp::Subtract(numeric) => lir::BinaryOp::Subtract(lower_numeric(numeric)),
        hir::BinaryOp::Multiply(numeric) => lir::BinaryOp::Multiply(lower_numeric(numeric)),
        hir::BinaryOp::Divide => lir::BinaryOp::Divide,
        hir::BinaryOp::Div => lir::BinaryOp::Div,
        hir::BinaryOp::Mod => lir::BinaryOp::Mod,
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
                hir::Compared::Scalar(hir::Scalar::Text)
                | hir::Compared::Tuple
                | hir::Compared::Relation
                | hir::Compared::Array => unreachable!("compared by a runtime call"),
                hir::Compared::Parameter(_) => {
                    unreachable!("functions are of instantiated operators")
                }
            },
        ),
        hir::BinaryOp::Concatenate => unreachable!("text is concatenated by a runtime call"),
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

/// For each attribute the relations with `lhs` and `rhs` have in common, a
/// struct of its offset in a tuple of each and the address of its type's
/// descriptor, as the runtime's joins take them.
fn common_attributes(lhs: &hir::Heading, rhs: &hir::Heading, cx: &mut Lowering) -> Vec<lir::Field> {
    let lhs_tuple = layout(&hir::Type::Tuple(lhs.clone()));
    let rhs_tuple = layout(&hir::Type::Tuple(rhs.clone()));
    lhs.attributes()
        .enumerate()
        .filter_map(|(i, (name, ty))| {
            let j = rhs.attributes().position(|(a, _)| a == name)?;
            Some(lir::Field::Struct(vec![
                lir::Field::Offset(lhs_tuple.clone(), i),
                lir::Field::Offset(rhs_tuple.clone(), j),
                lir::Field::Address(cx.descriptor(ty)),
            ]))
        })
        .collect()
}

/// A pointer-sized constant `n`, as a place.
fn usize_constant(n: u64, cx: &mut Lowering) -> lir::Place {
    lir::Place {
        base: lir::Base::Data(cx.constant(vec![lir::Field::Usize(n)])),
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

fn lower_numeric(numeric: hir::Numeric) -> lir::Numeric {
    match numeric {
        hir::Numeric::Integer => lir::Numeric::Integer,
        hir::Numeric::Approximate => lir::Numeric::Approximate,
        hir::Numeric::Parameter(_) => unreachable!("functions are of instantiated operators"),
    }
}

fn scalar_layout(scalar: hir::Scalar) -> lir::Layout {
    layout(&hir::Type::Scalar(scalar))
}

/// Whether a value of type `ty` points to memory: whether it is or has a
/// text, a relation or an array.
fn points(ty: &hir::Type) -> bool {
    match ty {
        hir::Type::Scalar(scalar) => *scalar == hir::Scalar::Text,
        hir::Type::Relation(_) | hir::Type::Array(_) => true,
        hir::Type::Tuple(heading) => heading.attributes().any(|(_, ty)| points(ty)),
        hir::Type::Parameter(_) => unreachable!("functions are of instantiated operators"),
    }
}

/// The layout of a value of type `ty`.
fn layout(ty: &hir::Type) -> lir::Layout {
    match ty {
        hir::Type::Scalar(hir::Scalar::Boolean) => lir::Layout::U8,
        hir::Type::Scalar(hir::Scalar::Integer) => lir::Layout::I64,
        hir::Type::Scalar(hir::Scalar::Approximate) => lir::Layout::F64Bits,
        hir::Type::Scalar(hir::Scalar::Text) | hir::Type::Relation(_) | hir::Type::Array(_) => {
            lir::Layout::Struct(vec![lir::Layout::Address, lir::Layout::Usize])
        }
        hir::Type::Tuple(heading) => {
            lir::Layout::Struct(heading.attributes().map(|(_, ty)| layout(ty)).collect())
        }
        hir::Type::Parameter(_) => unreachable!("functions are of instantiated operators"),
    }
}

/// The parameters of `op`, instantiated with `types`.
fn params_of(op: &hir::Operator, types: &[hir::Type]) -> BTreeMap<String, hir::Type> {
    op.params
        .iter()
        .map(|(name, ty)| (name.clone(), ty.instantiated(types)))
        .collect()
}

/// The parameter struct of a user operator, if it has parameters.
fn params_layout(params: &BTreeMap<String, hir::Type>) -> Option<lir::Layout> {
    (!params.is_empty()).then(|| lir::Layout::Struct(params.values().map(layout).collect()))
}

fn len(n: usize) -> u64 {
    u64::try_from(n).expect("lengths fit in 64 bits")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn integer() -> hir::Type {
        hir::Type::Scalar(hir::Scalar::Integer)
    }

    fn user(name: &str, params: &[&str], body: Vec<hir::Statement>) -> hir::Operator {
        hir::Operator {
            name: name.to_string(),
            types: Vec::new(),
            params: params
                .iter()
                .map(|p| ((*p).to_string(), integer()))
                .collect(),
            kind: hir::OperatorKind::Update {
                locals: Vec::new(),
                body: hir::Block {
                    statements: body,
                    locals: Vec::new(),
                },
            },
        }
    }

    fn debug_value() -> hir::Operator {
        hir::Operator {
            name: "debug_value".to_string(),
            types: hir::Builtin::DebugValue.types(),
            params: hir::Builtin::DebugValue.params(),
            kind: hir::OperatorKind::Builtin(hir::Builtin::DebugValue),
        }
    }

    fn program(operators: Vec<hir::Operator>, variables: Vec<hir::Variable>) -> hir::Program {
        let entry = operators.iter().position(|o| o.name == "main").unwrap();
        hir::Program {
            operators,
            variables,
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
            keys,
        }
    }

    fn call(operator: usize, args: Vec<(&str, hir::Expr)>) -> hir::Statement {
        hir::Statement::Call(hir::Call {
            operator: hir::OperatorId(operator),
            types: Vec::new(),
            args: args.into_iter().map(|(n, e)| (n.to_string(), e)).collect(),
        })
    }

    #[test]
    fn built_ins_dont_become_functions() {
        let lowered = lower(&program(
            vec![debug_value(), user("main", &[], vec![])],
            vec![],
        ));
        assert_eq!(lowered.functions.len(), 1);
        assert_eq!(lowered.functions[lowered.entry.0].name, "main");
        assert_eq!(lowered.functions[0].params, None);
    }

    #[test]
    fn values_are_fields_with_an_owner_if_they_point() {
        let lowered = lower(&program(
            vec![user("main", &[], vec![])],
            vec![
                variable("i", hir::Value::Integer(-7)),
                variable("t", hir::Value::Text("hé".to_string())),
                variable("b", hir::Value::Boolean(true)),
            ],
        ));
        assert_eq!(lowered.data[0].fields, [lir::Field::I64(-7)]);
        assert_eq!(
            lowered.data[1].fields,
            [lir::Field::Bytes("hé".as_bytes().to_vec())]
        );
        // A text points to its bytes, so the variable has an owner for
        // the bytes of a text assigned to it: none yet.
        assert_eq!(
            lowered.data[2].fields,
            [
                lir::Field::Struct(vec![
                    lir::Field::Address(lir::DataId(1)),
                    lir::Field::Usize(3)
                ]),
                lir::Field::Usize(0)
            ]
        );
        assert_eq!(lowered.data[3].fields, [lir::Field::U8(1)]);
    }

    #[test]
    fn relations_point_to_an_array_of_tuples() {
        let tuple = |n| hir::TupleValue::new([("a".to_string(), hir::Value::Integer(n))].into());
        let heading = hir::Heading::new([("a".to_string(), integer())].into());
        let lowered = lower(&program(
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
        assert_eq!(lowered.data[0].fields, [element(1), element(2)]);
        assert!(!lowered.data[0].writable);
        assert_eq!(
            lowered.data[1].fields,
            [
                lir::Field::Struct(vec![
                    lir::Field::Address(lir::DataId(0)),
                    lir::Field::Usize(2)
                ]),
                lir::Field::Usize(0)
            ]
        );
    }

    #[test]
    fn boolean_expressions_compute_into_slots() {
        // `debug_value{v: not (a and b)}`, of two variables.
        let a = || Box::new(hir::Expr::Variable(hir::VariableId(0)));
        let b = || Box::new(hir::Expr::Variable(hir::VariableId(1)));
        let expr = hir::Expr::Unary(
            hir::UnaryOp::Not,
            Box::new(hir::Expr::Binary(hir::BinaryOp::And, a(), b())),
        );
        let lowered = lower(&program(
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
                    op: lir::BinaryOp::And,
                    lhs: data(0),
                    rhs: data(1),
                },
                lir::Inst::Unary {
                    dst: slot(1),
                    op: lir::UnaryOp::Not,
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
        let lowered = lower(&program(
            vec![
                debug_value(),
                user("main", &[], vec![call(0, vec![("v", expr)])]),
            ],
            vec![variable("n", hir::Value::Integer(7))],
        ));
        let main = &lowered.functions[0];
        assert_eq!(main.slots, [lir::Layout::I64, lir::Layout::I64]);
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
        let lowered = lower(&program(
            vec![
                debug_value(),
                user("main", &[], vec![call(0, vec![("v", expr)])]),
            ],
            vec![variable("t", hir::Value::Text("a".to_string()))],
        ));
        let main = &lowered.functions[0];
        let text = lir::Layout::Struct(vec![lir::Layout::Address, lir::Layout::Usize]);
        assert_eq!(main.slots, [text.clone(), text, lir::Layout::scope()]);
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
        let lowered = lower(&program(
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
    fn assigned_texts_are_copied_into_memory_of_the_variables_own() {
        let t = hir::Expr::Variable(hir::VariableId(0));
        let lowered = lower(&program(
            vec![user("main", &[], vec![assign(vec![(0, t)])])],
            vec![variable("t", hir::Value::Text("a".to_string()))],
        ));
        let variable = lir::DataId(1);
        let field = |i| lir::Place {
            base: lir::Base::Data(variable),
            path: vec![i],
        };
        let slot = |i| lir::Place::whole(lir::Base::Slot(lir::SlotId(i)));
        let insts = &lowered.functions[0].blocks[0].insts;
        let lir::Inst::CallRuntime { function, args } = &insts[2] else {
            panic!("expected a runtime call, found {:?}", insts[2]);
        };
        assert_eq!(*function, lir::RuntimeFn::Assign);
        assert_eq!(args[..3], [field(0), field(1), slot(0)]);
        // The statement has a scope, in case the variable's old memory is
        // its.
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
        let lowered = lower(&program(
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
        let lir::Inst::CallRuntime { function, args } = &main.blocks[0].insts[1] else {
            panic!("{:?}", main.blocks[0].insts);
        };
        assert_eq!(*function, lir::RuntimeFn::And);
        assert_eq!(args[0], slot);
        assert_eq!(
            args.len(),
            9,
            "dst, the operands, three descriptors, the sources and the common attributes"
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
        let lowered = lower(&program(
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
        let [entry, head, body, done] = &main.blocks[..] else {
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
        // before it was known to need one.
        assert_eq!(
            runtime(entry),
            [lir::RuntimeFn::Begin, lir::RuntimeFn::Allocate]
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

    #[test]
    fn calls_build_the_argument_tuple_in_a_slot() {
        let lowered = lower(&program(
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
        assert_eq!(
            main.blocks[0].insts,
            [
                // `a`, then `b`: parameter name order.
                lir::Inst::Copy {
                    dst: slot(0),
                    src: data(1),
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
                },
            ]
        );
    }

    #[test]
    fn calls_passing_a_text_have_a_scope() {
        // `call f{t: t};`, which makes nothing, but must keep `t`'s memory
        // from being released by an assignment `f` makes.
        let f = hir::Operator {
            types: Vec::new(),
            name: "f".to_string(),
            params: [("t".to_string(), hir::Type::Scalar(hir::Scalar::Text))].into(),
            kind: hir::OperatorKind::Update {
                locals: Vec::new(),
                body: hir::Block::default(),
            },
        };
        let lowered = lower(&program(
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
        assert_eq!(functions, [lir::RuntimeFn::Begin, lir::RuntimeFn::End]);
        assert!(matches!(insts[insts.len() - 2], lir::Inst::Call { .. }));
    }

    #[test]
    fn read_only_operators_become_functions_once_called() {
        // `shout{t: text}: text [ return t || "!"; ]`, called when running
        // as `debug_value{v: shout{t: t}}`, and `unused`, which isn't.
        let text = || hir::Type::Scalar(hir::Scalar::Text);
        let read_only = |name: &str, value| hir::Operator {
            name: name.to_string(),
            types: Vec::new(),
            params: [("t".to_string(), text())].into(),
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
        });
        let lowered = lower(&program(
            vec![
                debug_value(),
                shout,
                read_only("unused", *t()),
                user("main", &[], vec![call(0, vec![("v", shouted)])]),
            ],
            vec![variable("t", hir::Value::Text("a".to_string()))],
        ));
        let names: Vec<&str> = lowered.functions.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, ["main", "shout"]);
        // `shout` stores its value where the caller says, in no scope of
        // its own.
        let shout = &lowered.functions[1];
        assert_eq!(shout.result, Some(layout(&text())));
        let runtime = |insts: &[lir::Inst]| -> Vec<lir::RuntimeFn> {
            insts
                .iter()
                .filter_map(|inst| match inst {
                    lir::Inst::CallRuntime { function, .. } => Some(*function),
                    _ => None,
                })
                .collect()
        };
        let insts = &shout.blocks[0].insts;
        assert_eq!(runtime(insts), [lir::RuntimeFn::Concat]);
        assert!(matches!(
            insts.last(),
            Some(lir::Inst::Copy {
                dst: lir::Place {
                    base: lir::Base::Result,
                    ..
                },
                ..
            })
        ));
        // The call passes a slot for the result, and the calling
        // statement's scope owns what `shout` makes.
        let insts = &lowered.functions[0].blocks[0].insts;
        assert_eq!(
            runtime(insts),
            [
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
            name: "show".to_string(),
            types: vec![parameter.clone()],
            params: [("v".to_string(), hir::Type::Parameter(parameter.clone()))].into(),
            kind: hir::OperatorKind::Update {
                locals: Vec::new(),
                body: hir::Block {
                    statements: vec![hir::Statement::Call(hir::Call {
                        operator: hir::OperatorId(0),
                        types: vec![hir::Type::Parameter(parameter)],
                        args: [("v".to_string(), hir::Expr::Parameter("v".to_string()))].into(),
                    })],
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
            })
        };
        let main_body = vec![
            call(&integer(), hir::Value::Integer(1)),
            call(&integer(), hir::Value::Integer(2)),
            call(&text, hir::Value::Text("Lagos".to_string())),
        ];
        let lowered = lower(&program(
            vec![debug_value(), show, user("main", &[], main_body)],
            vec![],
        ));
        let names: Vec<&str> = lowered.functions.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, ["main", "show{v: integer}", "show{v: text}"]);
        assert_eq!(
            lowered.functions[2].params,
            Some(lir::Layout::Struct(vec![layout(&text)]))
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
            name: "count".to_string(),
            types: hir::Builtin::Count.types(),
            params: hir::Builtin::Count.params(),
            kind: hir::OperatorKind::Builtin(hir::Builtin::Count),
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
        });
        let lowered = lower(&program(
            vec![
                debug_value(),
                count,
                user("main", &[], vec![call(0, vec![("v", counted)])]),
            ],
            vec![variable("r", r)],
        ));
        assert_eq!(lowered.functions.len(), 1, "a built-in has no function");
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
            name: "max".to_string(),
            types: hir::Builtin::Max.types(),
            params: hir::Builtin::Max.params(),
            kind: hir::OperatorKind::Builtin(hir::Builtin::Max),
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
        });
        let lowered = lower(&program(
            vec![
                debug_value(),
                max,
                user("main", &[], vec![call(0, vec![("v", greatest)])]),
            ],
            vec![variable("r", r)],
        ));
        assert_eq!(lowered.functions.len(), 1, "a built-in has no function");
        let blocks = &lowered.functions[0].blocks;
        assert!(
            blocks
                .iter()
                .any(|b| b.terminator == lir::Terminator::Fail(lir::Failure::NoMax)),
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
    fn local_variables_own_their_memory_until_the_body_ends() {
        // `f{} [ var r: relation { v: integer }; var n := 1; call
        // debug_value{v: r}; ]`.
        let heading = hir::Heading::new([("v".to_string(), integer())].into());
        let r = hir::Local {
            name: "r".to_string(),
            ty: hir::Type::Relation(heading.clone()),
            keys: vec![["v".to_string()].into()],
        };
        let n = hir::Local {
            name: "n".to_string(),
            ty: integer(),
            keys: Vec::new(),
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
            name: "f".to_string(),
            types: Vec::new(),
            params: BTreeMap::new(),
            kind: hir::OperatorKind::Update {
                locals: vec![r, n],
                body: hir::Block {
                    statements: vec![
                        declared(0, empty),
                        declared(1, hir::Value::Integer(1)),
                        call(0, vec![("v", hir::Expr::Local(hir::LocalId(0)))]),
                    ],
                    locals: vec![hir::LocalId(0), hir::LocalId(1)],
                },
            },
        };
        let lowered = lower(&program(
            vec![debug_value(), f, user("main", &[], vec![call(1, vec![])])],
            Vec::new(),
        ));
        let f = lowered.functions.iter().find(|f| f.name == "f").unwrap();
        let insts: Vec<&lir::Inst> = f.blocks.iter().flat_map(|b| &b.insts).collect();
        // The relation's owner starts as null, and is the assignment's,
        // then released last; the integer is copied.
        let lir::Inst::Copy { dst: owner, .. } = insts[0] else {
            panic!("{insts:#?}");
        };
        let assigned = insts.iter().find_map(|i| match i {
            lir::Inst::CallRuntime {
                function: lir::RuntimeFn::Assign,
                args,
            } => Some(&args[1]),
            _ => None,
        });
        assert_eq!(assigned, Some(owner), "{insts:#?}");
        assert_eq!(
            insts.last(),
            Some(&&lir::Inst::CallRuntime {
                function: lir::RuntimeFn::Release,
                args: vec![owner.clone()],
            })
        );
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
        assert_eq!(releases, 1, "only the relation has memory of its own");
    }

    #[test]
    fn a_let_computes_its_value_once() {
        // `f{p: integer}: integer [ var n := p + 1; return n * n; ]`.
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
            name: "f".to_string(),
            types: Vec::new(),
            params: [("p".to_string(), integer())].into(),
            kind: hir::OperatorKind::ReadOnly {
                result: integer(),
                locals: vec![hir::Local {
                    name: "n".to_string(),
                    ty: integer(),
                    keys: Vec::new(),
                }],
                value,
            },
        };
        let called = hir::Expr::Call(hir::Call {
            operator: hir::OperatorId(1),
            types: Vec::new(),
            args: [("p".to_string(), hir::Expr::Variable(hir::VariableId(0)))].into(),
        });
        let lowered = lower(&program(
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
    fn sum_and_product_are_runtime_calls() {
        for (builtin, function) in [
            (hir::Builtin::Sum, lir::RuntimeFn::Sum),
            (hir::Builtin::Product, lir::RuntimeFn::Product),
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
            name: builtin.name().to_string(),
            types: builtin.types(),
            params: builtin.params(),
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
        });
        let lowered = lower(&program(
            vec![
                debug_value(),
                operator,
                user("main", &[], vec![call(0, vec![("v", folded)])]),
            ],
            vec![variable("r", r)],
        ));
        assert_eq!(lowered.functions.len(), 1, "a built-in has no function");
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
            name: builtin.name().to_string(),
            types: builtin.types(),
            params: builtin.params(),
            kind: hir::OperatorKind::Builtin(builtin),
        };
        let converted = |operator, types| {
            hir::Expr::Call(hir::Call {
                operator: hir::OperatorId(operator),
                types,
                args: [("from".to_string(), hir::Expr::Parameter("p".to_string()))].into(),
            })
        };
        let show = user(
            "show",
            &["p"],
            vec![
                call(0, vec![("v", converted(1, vec![integer()]))]),
                call(0, vec![("v", converted(2, Vec::new()))]),
            ],
        );
        let main_body = vec![call(
            3,
            vec![("p", hir::Expr::Constant(hir::Value::Integer(3)))],
        )];
        let lowered = lower(&program(
            vec![
                debug_value(),
                builtin(hir::Builtin::ToText),
                builtin(hir::Builtin::ToApproximate),
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
        // converts inline.
        assert_eq!(
            runtime,
            [
                lir::RuntimeFn::Begin,
                lir::RuntimeFn::ToText,
                lir::RuntimeFn::DebugValue,
                lir::RuntimeFn::End,
                lir::RuntimeFn::DebugValue,
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
        let lowered = lower(&program(
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
            lowered.data[descriptor.0].fields[..2],
            [
                lir::Field::U8(KIND_INTEGER),
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
        let mut cx = Lowering::new();
        let id = cx.descriptor(&hir::Type::Tuple(heading));
        let lir::Field::Address(attributes) = cx.data[id.0].fields[2] else {
            panic!("attributes are an address");
        };
        let tuple = lir::Layout::Struct(vec![lir::Layout::U8, lir::Layout::I64]);
        let lir::Field::Struct(b) = &cx.data[attributes.0].fields[1] else {
            panic!("an attribute is a struct");
        };
        assert_eq!(b[2], lir::Field::Offset(tuple, 1));
    }

    #[test]
    fn descriptors_point_to_their_elements_type() {
        let mut cx = Lowering::new();
        let element = |cx: &Lowering, id: lir::DataId| {
            let lir::Field::Address(element) = cx.data[id.0].fields[4] else {
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
        assert_eq!(cx.data[scalar.0].fields[4], lir::Field::Usize(0));
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
        let lowered = lower(&program(
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
        let lowered = lower(&program(
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
            blocks
                .iter()
                .any(|b| b.terminator == lir::Terminator::Fail(lir::Failure::IndexOutOfRange)),
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
        let lowered = lower(&program(
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
    fn an_if_statements_blocks_release_their_own_local_variables() {
        // `f{p: integer} [ if p = 0 then [ var t := "zero"; call
        // debug_value{v: t}; ] else [ call debug_value{v: p}; ]; ]`.
        let t = hir::Local {
            name: "t".to_string(),
            ty: hir::Type::Scalar(hir::Scalar::Text),
            keys: Vec::new(),
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
            statements: vec![
                hir::Statement::Assign(vec![hir::Assignment {
                    target: hir::Target::Local(hir::LocalId(0)),
                    value: hir::Expr::Constant(hir::Value::Text("zero".to_string())),
                }]),
                call(0, vec![("v", hir::Expr::Local(hir::LocalId(0)))]),
            ],
            locals: vec![hir::LocalId(0)],
        };
        let otherwise = hir::Block {
            statements: vec![call(0, vec![("v", hir::Expr::Parameter("p".to_string()))])],
            locals: Vec::new(),
        };
        let f = hir::Operator {
            name: "f".to_string(),
            types: Vec::new(),
            params: [("p".to_string(), integer())].into(),
            kind: hir::OperatorKind::Update {
                locals: vec![t],
                body: hir::Block {
                    statements: vec![hir::Statement::If(condition, then, otherwise)],
                    locals: Vec::new(),
                },
            },
        };
        let lowered = lower(&program(
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
}
