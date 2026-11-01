use std::collections::HashMap;

use cranelift_codegen::ir::{
    AbiParam, Endianness, InstBuilder, MemFlagsData, StackSlotData, StackSlotKind, Value, types,
};
use cranelift_codegen::isa::TargetFrontendConfig;
use cranelift_codegen::settings::{self, Configurable};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_module::{DataDescription, DataId, FuncId, Linkage, Module, default_libcall_names};
use cranelift_object::{ObjectBuilder, ObjectModule};

use trendy_lir as lir;

/// Compile a program to a native object file.
///
/// Every function becomes a local symbol named `trendy_op_<name>`, which
/// returns whether it failed, a byte. A C-ABI `main` is emitted alongside
/// it that calls the program's entry function and returns 1 if it failed
/// and 0 if not, so the object links against the platform's C startup
/// code.
/// Variables become local data symbols named `trendy_var_<name>`, and
/// constants `trendy_const_<index>`. Runtime functions are imported by their
/// symbols, for the linker to resolve against the runtime library.
///
/// # Errors
///
/// Returns a message if Cranelift fails. Valid LIR should never cause this, so
/// any error is a compiler bug.
pub fn compile(program: &lir::Program) -> Result<Vec<u8>, String> {
    let mut flags = settings::builder();
    flags.set("opt_level", "speed").map_err(|e| e.to_string())?;
    flags.set("is_pic", "true").map_err(|e| e.to_string())?;
    let isa = cranelift_native::builder()?
        .finish(settings::Flags::new(flags))
        .map_err(|e| e.to_string())?;

    let builder =
        ObjectBuilder::new(isa, "trendy", default_libcall_names()).map_err(|e| e.to_string())?;
    let mut module = ObjectModule::new(builder);
    let target = Target {
        pointer_bytes: usize::from(module.isa().pointer_bytes()),
        endianness: module.isa().endianness(),
    };
    let data_ids = define_data(&mut module, &target, &program.data)?;
    let data_layouts: Vec<lir::Layout> = program
        .data
        .iter()
        .map(|d| lir::Layout::Struct(d.fields.iter().map(field_layout).collect()))
        .collect();

    let config = module.target_config();
    let pointer = config.pointer_type();
    // Indexed by `lir::FunctionId`.
    let function_ids = program
        .functions
        .iter()
        .map(|f| {
            let mut signature = module.make_signature();
            // The parameters' address, then the result's.
            for _ in f.params.iter().chain(&f.result) {
                signature.params.push(AbiParam::new(pointer));
            }
            // Whether it failed.
            signature.returns.push(AbiParam::new(types::I8));
            module
                .declare_function(&mangle(&f.name), Linkage::Local, &signature)
                .map_err(|e| e.to_string())
        })
        .collect::<Result<Vec<_>, _>>()?;

    let mut functions = Functions {
        module,
        target,
        config,
        data_ids,
        data_layouts,
        function_ids,
        runtime: HashMap::new(),
        site: None,
        failure: None,
    };
    let mut ctx = functions.module.make_context();
    let mut fctx = FunctionBuilderContext::new();
    for (index, function) in program.functions.iter().enumerate() {
        ctx.func.signature = functions
            .module
            .declarations()
            .get_function_decl(functions.function_ids[index])
            .signature
            .clone();
        functions.build(function, &mut ctx.func, &mut fctx)?;
        let id = functions.function_ids[index];
        functions
            .module
            .define_function(id, &mut ctx)
            .map_err(|e| e.to_string())?;
        functions.module.clear_context(&mut ctx);
    }

    let mut module = functions.module;
    let mut entry_sig = module.make_signature();
    entry_sig.returns.push(AbiParam::new(types::I32));
    let entry_id = module
        .declare_function("main", Linkage::Export, &entry_sig)
        .map_err(|e| e.to_string())?;
    ctx.func.signature = entry_sig;
    {
        let mut b = FunctionBuilder::new(&mut ctx.func, &mut fctx);
        let entry = b.create_block();
        b.switch_to_block(entry);
        b.seal_block(entry);
        let entry_fn = module.declare_func_in_func(functions.function_ids[program.entry.0], b.func);
        let call = b.ins().call(entry_fn, &[]);
        let failed = b.inst_results(call)[0];
        let status = b.ins().uextend(types::I32, failed);
        b.ins().return_(&[status]);
        b.finalize(config);
    }
    module
        .define_function(entry_id, &mut ctx)
        .map_err(|e| e.to_string())?;
    module.clear_context(&mut ctx);

    module.finish().emit().map_err(|e| e.to_string())
}

/// Everything needed to build function bodies.
struct Functions {
    module: ObjectModule,
    target: Target,
    config: TargetFrontendConfig,
    /// Indexed by `lir::DataId`.
    data_ids: Vec<DataId>,
    /// Each data object's layout: the struct of its fields.
    data_layouts: Vec<lir::Layout>,
    /// Indexed by `lir::FunctionId`.
    function_ids: Vec<FuncId>,
    /// Runtime functions, imported when first called.
    runtime: HashMap<&'static str, FuncId>,
    /// The runtime's `trendy_rt_site`, imported when first set.
    site: Option<DataId>,
    /// Where a failure in the block being built goes, its
    /// [`lir::Block::failure`], if anything in it can fail.
    failure: Option<cranelift_codegen::ir::Block>,
}

impl Functions {
    fn build(
        &mut self,
        function: &lir::Function,
        func: &mut cranelift_codegen::ir::Function,
        fctx: &mut FunctionBuilderContext,
    ) -> Result<(), String> {
        let mut b = FunctionBuilder::new(func, fctx);
        let slots = function
            .slots
            .iter()
            .map(|layout| {
                let align = self.target.align(layout);
                let size = u32::try_from(self.target.size(layout)).map_err(|e| e.to_string())?;
                let align_shift =
                    u8::try_from(align.trailing_zeros()).map_err(|e| e.to_string())?;
                Ok(b.create_sized_stack_slot(StackSlotData::new(
                    StackSlotKind::ExplicitSlot,
                    size,
                    align_shift,
                )))
            })
            .collect::<Result<Vec<_>, String>>()?;
        // Indexed by `lir::BlockId`.
        let blocks: Vec<_> = function.blocks.iter().map(|_| b.create_block()).collect();
        for target in function.blocks.iter().filter_map(|block| block.failure) {
            b.set_cold_block(blocks[target.0]);
        }
        b.append_block_params_for_function_params(blocks[0]);
        b.switch_to_block(blocks[0]);
        let mut addresses = b.block_params(blocks[0]).iter().copied();
        let params = function.params.as_ref().and_then(|_| addresses.next());
        let result = function.result.as_ref().and_then(|_| addresses.next());

        let frame = Frame {
            function,
            slots: &slots,
            params,
            result,
        };
        for (i, (block, &cl_block)) in function.blocks.iter().zip(&blocks).enumerate() {
            if i > 0 {
                b.switch_to_block(cl_block);
            }
            self.failure = block.failure.map(|target| blocks[target.0]);
            for inst in &block.insts {
                self.inst(inst, &frame, &mut b)?;
            }
            match &block.terminator {
                lir::Terminator::Return => returned(false, &mut b),
                lir::Terminator::Failed => returned(true, &mut b),
                lir::Terminator::Jump(target) => {
                    b.ins().jump(blocks[target.0], &[]);
                }
                lir::Terminator::Branch {
                    condition,
                    then,
                    otherwise,
                } => {
                    let condition = self.load(types::I8, condition, &frame, &mut b)?;
                    b.ins()
                        .brif(condition, blocks[then.0], &[], blocks[otherwise.0], &[]);
                }
                lir::Terminator::Fail(failure) => self.fail(failure, &frame, &mut b)?,
            }
        }
        self.failure = None;
        b.seal_all_blocks();
        b.finalize(self.config);
        Ok(())
    }

    fn inst(
        &mut self,
        inst: &lir::Inst,
        frame: &Frame,
        b: &mut FunctionBuilder,
    ) -> Result<(), String> {
        match inst {
            lir::Inst::Copy { dst, src, layout } => {
                let dst = self.address(dst, frame, b)?;
                let src = self.address(src, frame, b)?;
                let size = u64::try_from(self.target.size(layout)).map_err(|e| e.to_string())?;
                let align = u8::try_from(self.target.align(layout)).map_err(|e| e.to_string())?;
                b.emit_small_memory_copy(
                    self.config,
                    dst,
                    src,
                    size,
                    align,
                    align,
                    true,
                    MemFlagsData::new(),
                );
            }
            lir::Inst::Call {
                function,
                args,
                result,
            } => {
                let callee = self
                    .module
                    .declare_func_in_func(self.function_ids[function.0], b.func);
                let addresses = args
                    .iter()
                    .chain(result)
                    .map(|place| self.address(place, frame, b))
                    .collect::<Result<Vec<_>, _>>()?;
                let call = b.ins().call(callee, &addresses);
                let failed = b.inst_results(call)[0];
                let failure = self.failure_target()?;
                let carry_on = b.create_block();
                b.ins().brif(failed, failure, &[], carry_on, &[]);
                b.switch_to_block(carry_on);
            }
            lir::Inst::Site(_) | lir::Inst::SaveSite { .. } | lir::Inst::RestoreSite { .. } => {
                self.site_inst(inst, frame, b)?;
            }
            lir::Inst::CallRuntime { function, args } => {
                let args: Vec<&lir::Place> = args.iter().collect();
                self.call_runtime(*function, &args, frame, b)?;
            }
            lir::Inst::Unary { dst, op, src } => {
                let ty = match op.operand() {
                    lir::Operand::Usize => self.config.pointer_type(),
                    operand => operand_type(operand),
                };
                let value = self.load(ty, src, frame, b)?;
                let result = self.unary(*op, value, ty, b)?;
                self.store(dst, result, frame, b)?;
            }
            lir::Inst::Element {
                dst,
                array,
                index,
                element,
            } => {
                let pointer = self.config.pointer_type();
                let elements = self.load(pointer, array, frame, b)?;
                let index = self.load(pointer, index, frame, b)?;
                let size = i64::try_from(self.target.size(element)).map_err(|e| e.to_string())?;
                let offset = b.ins().imul_imm_s(index, size);
                let address = b.ins().iadd(elements, offset);
                self.store(dst, address, frame, b)?;
            }
            lir::Inst::Increment { place } => {
                let pointer = self.config.pointer_type();
                let value = self.load(pointer, place, frame, b)?;
                let next = b.ins().iadd_imm_s(value, 1);
                self.store(place, next, frame, b)?;
            }
            lir::Inst::Address { dst, src } => {
                let address = self.address(src, frame, b)?;
                self.store(dst, address, frame, b)?;
            }
            lir::Inst::Binary { dst, op, lhs, rhs } => {
                let ty = match op {
                    lir::BinaryOp::Compare(_, lir::Operand::Usize) => self.config.pointer_type(),
                    _ => operand_type(op.operand()),
                };
                let lhs = self.load(ty, lhs, frame, b)?;
                let rhs = self.load(ty, rhs, frame, b)?;
                let result = self.binary(*op, lhs, rhs, b)?;
                self.store(dst, result, frame, b)?;
            }
        }
        Ok(())
    }

    /// `op value`, of `value`'s type `ty`, failing the program where an
    /// arithmetic result doesn't exist.
    fn unary(
        &mut self,
        op: lir::UnaryOp,
        value: Value,
        ty: types::Type,
        b: &mut FunctionBuilder,
    ) -> Result<Value, String> {
        Ok(match op {
            lir::UnaryOp::Not => b.ins().bxor_imm_u(value, 1),
            lir::UnaryOp::Negate(lir::Numeric::Integer) => {
                let zero = b.ins().iconst(types::I64, 0);
                let (result, overflow) = b.ins().ssub_overflow(zero, value);
                let negation = Operation::integer(lir::Arithmetic::Negate, zero, value);
                self.fail_if(overflow, negation, b)?;
                result
            }
            // Exact: only the sign changes, and 0.0 stays 0.0.
            lir::UnaryOp::Negate(lir::Numeric::Approximate) => {
                let negated = b.ins().fneg(value);
                one_zero(negated, b)
            }
            lir::UnaryOp::Integer if ty == types::I64 => value,
            lir::UnaryOp::Integer => b.ins().uextend(types::I64, value),
            lir::UnaryOp::Usize if self.config.pointer_type() == types::I64 => value,
            lir::UnaryOp::Usize => b.ins().ireduce(self.config.pointer_type(), value),
            // Rounded to nearest, ties to even, as IEEE 754 rounds.
            lir::UnaryOp::Approximate => b.ins().fcvt_from_sint(types::F64, value),
        })
    }

    /// `lhs op rhs`, failing the program where an arithmetic result doesn't
    /// exist.
    fn binary(
        &mut self,
        op: lir::BinaryOp,
        lhs: Value,
        rhs: Value,
        b: &mut FunctionBuilder,
    ) -> Result<Value, String> {
        use cranelift_codegen::ir::condcodes::{FloatCC, IntCC};
        use lir::Arithmetic as A;
        use lir::Numeric::{Approximate, Integer};
        let (integer, approximate) = (
            |op| Operation::integer(op, lhs, rhs),
            |op| Operation::approximate(op, lhs, rhs),
        );
        let checked = |this: &mut Self, op, (result, overflow), b: &mut FunctionBuilder| {
            this.fail_if(overflow, integer(op), b).map(|()| result)
        };
        // The operands are finite, so an infinite result is an overflow, and
        // there is never a NaN.
        let finite = |this: &mut Self, op, result: Value, b: &mut FunctionBuilder| {
            let magnitude = b.ins().fabs(result);
            let infinity = b.ins().f64const(f64::INFINITY);
            let overflow = b.ins().fcmp(FloatCC::Equal, magnitude, infinity);
            this.fail_if(overflow, approximate(op), b)
                .map(|()| one_zero(result, b))
        };
        Ok(match op {
            lir::BinaryOp::Compare(comparison, operand) => match operand {
                // `false` and `true` are 0 and 1, so signed order is their
                // order.
                lir::Operand::Boolean | lir::Operand::Numeric(Integer) => {
                    b.ins().icmp(condition(comparison, true), lhs, rhs)
                }
                lir::Operand::Usize => b.ins().icmp(condition(comparison, false), lhs, rhs),
                // Without NaN or `-0.0`, IEEE 754 comparison is equal exactly
                // for identical values, and orders them all.
                lir::Operand::Numeric(Approximate) => {
                    b.ins().fcmp(float_condition(comparison), lhs, rhs)
                }
            },
            lir::BinaryOp::And => b.ins().band(lhs, rhs),
            lir::BinaryOp::Or => b.ins().bor(lhs, rhs),
            lir::BinaryOp::Xor => b.ins().bxor(lhs, rhs),
            lir::BinaryOp::Add(Integer) => {
                let sum = b.ins().sadd_overflow(lhs, rhs);
                checked(self, A::Add, sum, b)?
            }
            lir::BinaryOp::Subtract(Integer) => {
                let difference = b.ins().ssub_overflow(lhs, rhs);
                checked(self, A::Subtract, difference, b)?
            }
            lir::BinaryOp::Multiply(Integer) => {
                let product = b.ins().smul_overflow(lhs, rhs);
                checked(self, A::Multiply, product, b)?
            }
            lir::BinaryOp::Add(Approximate) => {
                let sum = b.ins().fadd(lhs, rhs);
                finite(self, A::Add, sum, b)?
            }
            lir::BinaryOp::Subtract(Approximate) => {
                let difference = b.ins().fsub(lhs, rhs);
                finite(self, A::Subtract, difference, b)?
            }
            lir::BinaryOp::Multiply(Approximate) => {
                let product = b.ins().fmul(lhs, rhs);
                finite(self, A::Multiply, product, b)?
            }
            lir::BinaryOp::Divide => {
                // Equal to 0.0 for both zeros.
                let zero = b.ins().f64const(0.0);
                let by_zero = b.ins().fcmp(FloatCC::Equal, rhs, zero);
                self.fail_if(by_zero, approximate(A::Divide), b)?;
                let quotient = b.ins().fdiv(lhs, rhs);
                finite(self, A::Divide, quotient, b)?
            }
            lir::BinaryOp::Div => {
                let zero = b.ins().icmp_imm_s(IntCC::Equal, rhs, 0);
                self.fail_if(zero, integer(A::Div), b)?;
                // The one quotient out of range: the smallest integer
                // divided by -1.
                let min = b.ins().icmp_imm_s(IntCC::Equal, lhs, i64::MIN);
                let minus_one = b.ins().icmp_imm_s(IntCC::Equal, rhs, -1);
                let overflow = b.ins().band(min, minus_one);
                self.fail_if(overflow, integer(A::Div), b)?;
                b.ins().sdiv(lhs, rhs)
            }
            lir::BinaryOp::Mod => {
                let zero = b.ins().icmp_imm_s(IntCC::Equal, rhs, 0);
                self.fail_if(zero, integer(A::Mod), b)?;
                // Cranelift defines `srem` of the smallest integer by -1 as
                // 0, which is the result.
                b.ins().srem(lhs, rhs)
            }
        })
    }

    /// Fail when `condition` is nonzero, because `operation` has no result,
    /// in a cold block that writes its error, then goes where the block's
    /// failures go, and carry on in a new block otherwise.
    fn fail_if(
        &mut self,
        condition: Value,
        operation: Operation,
        b: &mut FunctionBuilder,
    ) -> Result<(), String> {
        let fail = b.create_block();
        let carry_on = b.create_block();
        b.set_cold_block(fail);
        b.ins().brif(condition, fail, &[], carry_on, &[]);

        b.switch_to_block(fail);
        let (function, ty) = match operation.numeric {
            lir::Numeric::Integer => (lir::RuntimeFn::IntegerFailed, types::I64),
            lir::Numeric::Approximate => (lir::RuntimeFn::ApproximateFailed, types::F64),
        };
        let id = self.runtime_function(function, &[types::I32, ty, ty])?;
        let callee = self.module.declare_func_in_func(id, b.func);
        let op = b.ins().iconst(types::I32, i64::from(operation.op.code()));
        b.ins().call(callee, &[op, operation.lhs, operation.rhs]);
        let failure = self.failure_target()?;
        b.ins().jump(failure, &[]);

        b.switch_to_block(carry_on);
        Ok(())
    }

    /// End the current block by failing because of `failure`: a call of
    /// its runtime function, which writes its error, then a jump to where
    /// the block's failures go.
    fn fail(
        &mut self,
        failure: &lir::Failure,
        frame: &Frame,
        b: &mut FunctionBuilder,
    ) -> Result<(), String> {
        let (function, args) = failure.call();
        self.call_runtime(function, &args, frame, b)?;
        let target = self.failure_target()?;
        b.ins().jump(target, &[]);
        Ok(())
    }

    /// Where a failure in the block being built goes, which LIR gives
    /// every block that can fail.
    fn failure_target(&self) -> Result<cranelift_codegen::ir::Block, String> {
        self.failure
            .ok_or_else(|| "a block that can fail has no failure target".to_string())
    }

    /// Call the runtime function `function`, passing the address of each
    /// of `args`.
    fn call_runtime(
        &mut self,
        function: lir::RuntimeFn,
        args: &[&lir::Place],
        frame: &Frame,
        b: &mut FunctionBuilder,
    ) -> Result<(), String> {
        let pointers = vec![self.config.pointer_type(); args.len()];
        let id = self.runtime_function(function, &pointers)?;
        let callee = self.module.declare_func_in_func(id, b.func);
        let args = args
            .iter()
            .map(|place| self.address(place, frame, b))
            .collect::<Result<Vec<_>, _>>()?;
        b.ins().call(callee, &args);
        Ok(())
    }

    /// The value of type `ty` at `place`.
    fn load(
        &mut self,
        ty: cranelift_codegen::ir::Type,
        place: &lir::Place,
        frame: &Frame,
        b: &mut FunctionBuilder,
    ) -> Result<Value, String> {
        let address = self.address(place, frame, b)?;
        Ok(b.ins().load(ty, MemFlagsData::trusted(), address, 0))
    }

    fn store(
        &mut self,
        place: &lir::Place,
        value: Value,
        frame: &Frame,
        b: &mut FunctionBuilder,
    ) -> Result<(), String> {
        let address = self.address(place, frame, b)?;
        b.ins().store(MemFlagsData::trusted(), value, address, 0);
        Ok(())
    }

    /// Import a runtime function, which takes parameters of types `params`.
    /// `inst`, which sets, saves or restores the site the program is at,
    /// in the runtime's `trendy_rt_site`.
    fn site_inst(
        &mut self,
        inst: &lir::Inst,
        frame: &Frame,
        b: &mut FunctionBuilder,
    ) -> Result<(), String> {
        let pointer = self.config.pointer_type();
        let flags = MemFlagsData::trusted();
        let at = self.site_address(b)?;
        match inst {
            lir::Inst::Site(site) => {
                let address = self.address(&lir::Place::whole(lir::Base::Data(*site)), frame, b)?;
                b.ins().store(flags, address, at, 0);
            }
            lir::Inst::SaveSite { dst } => {
                let site = b.ins().load(pointer, flags, at, 0);
                let dst = self.address(dst, frame, b)?;
                b.ins().store(flags, site, dst, 0);
            }
            lir::Inst::RestoreSite { src } => {
                let src = self.address(src, frame, b)?;
                let site = b.ins().load(pointer, flags, src, 0);
                b.ins().store(flags, site, at, 0);
            }
            other => unreachable!("{other:?} doesn't set the site"),
        }
        Ok(())
    }

    /// The address of the runtime's `trendy_rt_site`, in the function `b`
    /// builds.
    fn site_address(&mut self, b: &mut FunctionBuilder) -> Result<Value, String> {
        let global = self.site_global()?;
        let global = self.module.declare_data_in_func(global, b.func);
        Ok(b.ins().symbol_value(self.config.pointer_type(), global))
    }

    /// The runtime's `trendy_rt_site`, which [`lir::Inst::Site`] sets,
    /// imported when first set.
    fn site_global(&mut self) -> Result<DataId, String> {
        if let Some(id) = self.site {
            return Ok(id);
        }
        let id = self
            .module
            .declare_data("trendy_rt_site", Linkage::Import, true, false)
            .map_err(|e| e.to_string())?;
        self.site = Some(id);
        Ok(id)
    }

    fn runtime_function(
        &mut self,
        function: lir::RuntimeFn,
        params: &[cranelift_codegen::ir::Type],
    ) -> Result<FuncId, String> {
        if let Some(&id) = self.runtime.get(function.symbol()) {
            return Ok(id);
        }
        let mut signature = self.module.make_signature();
        signature
            .params
            .extend(params.iter().map(|&ty| AbiParam::new(ty)));
        let id = self
            .module
            .declare_function(function.symbol(), Linkage::Import, &signature)
            .map_err(|e| e.to_string())?;
        self.runtime.insert(function.symbol(), id);
        Ok(id)
    }

    /// The address of `place`.
    fn address(
        &mut self,
        place: &lir::Place,
        frame: &Frame,
        b: &mut FunctionBuilder,
    ) -> Result<Value, String> {
        let pointer = self.config.pointer_type();
        let base_layout = match &place.base {
            lir::Base::Data(id) => &self.data_layouts[id.0],
            lir::Base::Slot(slot) => &frame.function.slots[slot.0],
            lir::Base::Params => frame
                .function
                .params
                .as_ref()
                .ok_or("parameters used in a function without them")?,
            lir::Base::Result => frame
                .function
                .result
                .as_ref()
                .ok_or("a result stored by a function without one")?,
            lir::Base::Indirect { pointee, .. } => pointee,
        };
        let offset = i64::try_from(self.target.path_offset(base_layout, &place.path)?)
            .map_err(|e| e.to_string())?;
        Ok(match &place.base {
            lir::Base::Data(id) => {
                let global = self
                    .module
                    .declare_data_in_func(self.data_ids[id.0], b.func);
                let base = b.ins().symbol_value(pointer, global);
                b.ins().iadd_imm_s(base, offset)
            }
            lir::Base::Slot(slot) => {
                let offset = i32::try_from(offset).map_err(|e| e.to_string())?;
                b.ins().stack_addr(pointer, frame.slots[slot.0], offset)
            }
            lir::Base::Params => {
                let base = frame
                    .params
                    .ok_or("parameters used in a function without them")?;
                b.ins().iadd_imm_s(base, offset)
            }
            lir::Base::Result => {
                let base = frame
                    .result
                    .ok_or("a result stored by a function without one")?;
                b.ins().iadd_imm_s(base, offset)
            }
            lir::Base::Indirect { slot, .. } => {
                let base = b.ins().stack_load(pointer, pointer, frame.slots[slot.0], 0);
                b.ins().iadd_imm_s(base, offset)
            }
        })
    }
}

/// The integer comparison `comparison`, of signed integers if `signed`, and
/// of unsigned ones otherwise.
fn condition(comparison: lir::Comparison, signed: bool) -> cranelift_codegen::ir::condcodes::IntCC {
    use cranelift_codegen::ir::condcodes::IntCC;
    match (comparison, signed) {
        (lir::Comparison::Equal, _) => IntCC::Equal,
        (lir::Comparison::NotEqual, _) => IntCC::NotEqual,
        (lir::Comparison::Less, true) => IntCC::SignedLessThan,
        (lir::Comparison::LessOrEqual, true) => IntCC::SignedLessThanOrEqual,
        (lir::Comparison::Greater, true) => IntCC::SignedGreaterThan,
        (lir::Comparison::GreaterOrEqual, true) => IntCC::SignedGreaterThanOrEqual,
        (lir::Comparison::Less, false) => IntCC::UnsignedLessThan,
        (lir::Comparison::LessOrEqual, false) => IntCC::UnsignedLessThanOrEqual,
        (lir::Comparison::Greater, false) => IntCC::UnsignedGreaterThan,
        (lir::Comparison::GreaterOrEqual, false) => IntCC::UnsignedGreaterThanOrEqual,
    }
}

/// The float comparison `comparison`.
fn float_condition(comparison: lir::Comparison) -> cranelift_codegen::ir::condcodes::FloatCC {
    use cranelift_codegen::ir::condcodes::FloatCC;
    match comparison {
        lir::Comparison::Equal => FloatCC::Equal,
        lir::Comparison::NotEqual => FloatCC::NotEqual,
        lir::Comparison::Less => FloatCC::LessThan,
        lir::Comparison::LessOrEqual => FloatCC::LessThanOrEqual,
        lir::Comparison::Greater => FloatCC::GreaterThan,
        lir::Comparison::GreaterOrEqual => FloatCC::GreaterThanOrEqual,
    }
}

/// An arithmetic operation that may have no result, for the message of the
/// failure if it hasn't: the operator, and its operands, of `numeric`.
#[derive(Clone, Copy)]
struct Operation {
    numeric: lir::Numeric,
    op: lir::Arithmetic,
    lhs: Value,
    rhs: Value,
}

impl Operation {
    fn integer(op: lir::Arithmetic, lhs: Value, rhs: Value) -> Self {
        Operation {
            numeric: lir::Numeric::Integer,
            op,
            lhs,
            rhs,
        }
    }

    fn approximate(op: lir::Arithmetic, lhs: Value, rhs: Value) -> Self {
        Operation {
            numeric: lir::Numeric::Approximate,
            op,
            lhs,
            rhs,
        }
    }
}

/// End the current block by returning whether the function `failed`.
fn returned(failed: bool, b: &mut FunctionBuilder) {
    let failed = b.ins().iconst(types::I8, i64::from(failed));
    b.ins().return_(&[failed]);
}

/// The approximate number `value`, with IEEE 754's `-0.0` made `0.0`, since
/// approximate numbers have one zero. Adding `0.0` does that, and leaves
/// every other number as it is (IEEE 754, 6.3).
fn one_zero(value: Value, b: &mut FunctionBuilder) -> Value {
    let zero = b.ins().f64const(0.0);
    b.ins().fadd(value, zero)
}

/// The Cranelift type an instruction's operands are loaded as.
fn operand_type(operand: lir::Operand) -> cranelift_codegen::ir::Type {
    match operand {
        lir::Operand::Boolean => types::I8,
        lir::Operand::Numeric(lir::Numeric::Integer) => types::I64,
        lir::Operand::Numeric(lir::Numeric::Approximate) => types::F64,
        lir::Operand::Usize => unreachable!("a `Usize` is pointer-sized, as the target says"),
    }
}

/// The function being built: its stack slots, and the addresses of its
/// parameters and its result.
struct Frame<'a> {
    function: &'a lir::Function,
    slots: &'a [cranelift_codegen::ir::StackSlot],
    params: Option<Value>,
    result: Option<Value>,
}

/// Declare and define every data object, laid out for the target. Returns
/// their ids, indexed by `lir::DataId`.
fn define_data(
    module: &mut ObjectModule,
    target: &Target,
    data: &[lir::Data],
) -> Result<Vec<DataId>, String> {
    let ids = data
        .iter()
        .enumerate()
        .map(|(i, d)| {
            let name = match &d.kind {
                lir::DataKind::Variable { name } => format!("trendy_var_{name}"),
                lir::DataKind::Constant => format!("trendy_const_{i}"),
            };
            module
                .declare_data(&name, Linkage::Local, d.writable, false)
                .map_err(|e| e.to_string())
        })
        .collect::<Result<Vec<_>, _>>()?;

    for (d, &id) in data.iter().zip(&ids) {
        let laid_out = target.lay_out(&d.fields)?;
        let mut description = DataDescription::new();
        description.define(laid_out.bytes.into_boxed_slice());
        description.set_align(laid_out.align);
        for (offset, to, field) in laid_out.addresses {
            let addend = match field {
                Some(index) => target.field_offset(&data[to.0].fields, index)?,
                None => 0,
            };
            let addend = i64::try_from(addend).map_err(|e| e.to_string())?;
            let to = module.declare_data_in_data(ids[to.0], &mut description);
            let offset = u32::try_from(offset).map_err(|e| e.to_string())?;
            description.write_data_addr(offset, to, addend);
        }
        module
            .define_data(id, &description)
            .map_err(|e| e.to_string())?;
    }
    Ok(ids)
}

/// A data object's contents, ready to emit.
struct LaidOut {
    bytes: Vec<u8>,
    align: u64,
    /// The offsets of addresses, which need relocations, their targets,
    /// and the index of the field of the target each is the address of, if
    /// not of the whole.
    addresses: Vec<(usize, lir::DataId, Option<usize>)>,
}

/// The layout of a data field.
fn field_layout(field: &lir::Field) -> lir::Layout {
    match field {
        lir::Field::U8(_) => lir::Layout::U8,
        lir::Field::I64(_) => lir::Layout::I64,
        lir::Field::F64Bits(_) => lir::Layout::F64Bits,
        lir::Field::Usize(_) | lir::Field::Size(_) | lir::Field::Offset(..) => lir::Layout::Usize,
        lir::Field::Address(_) | lir::Field::FieldAddress(..) => lir::Layout::Address,
        lir::Field::Bytes(raw) => lir::Layout::Bytes(raw.len()),
        lir::Field::Struct(inner) => lir::Layout::Struct(inner.iter().map(field_layout).collect()),
        lir::Field::Variant(union, _) => union.clone(),
    }
}

/// How the target lays out memory: the one place sizes, alignments and
/// offsets are computed, for data, stack slots and places alike. Every field
/// is at its natural alignment, and every struct is padded to its alignment.
struct Target {
    pointer_bytes: usize,
    endianness: Endianness,
}

impl Target {
    fn align(&self, layout: &lir::Layout) -> usize {
        match layout {
            lir::Layout::U8 | lir::Layout::Bytes(_) => 1,
            lir::Layout::I64 | lir::Layout::F64Bits => 8,
            lir::Layout::Usize | lir::Layout::Address => self.pointer_bytes,
            lir::Layout::Struct(fields) | lir::Layout::Union(fields) => {
                fields.iter().map(|f| self.align(f)).max().unwrap_or(1)
            }
        }
    }

    fn size(&self, layout: &lir::Layout) -> usize {
        match layout {
            lir::Layout::U8 => 1,
            lir::Layout::I64 | lir::Layout::F64Bits => 8,
            lir::Layout::Usize | lir::Layout::Address => self.pointer_bytes,
            lir::Layout::Bytes(n) => *n,
            lir::Layout::Struct(fields) => {
                let end = self.offsets(fields).last().map_or(0, |&last| {
                    last + self.size(fields.last().expect("offsets match fields"))
                });
                end.next_multiple_of(self.align(layout))
            }
            lir::Layout::Union(variants) => {
                let end = variants.iter().map(|v| self.size(v)).max().unwrap_or(0);
                end.next_multiple_of(self.align(layout))
            }
        }
    }

    /// The offset of each field of a struct.
    fn offsets(&self, fields: &[lir::Layout]) -> Vec<usize> {
        let mut offsets = Vec::with_capacity(fields.len());
        let mut end: usize = 0;
        for field in fields {
            let offset = end.next_multiple_of(self.align(field));
            offsets.push(offset);
            end = offset + self.size(field);
        }
        offsets
    }

    /// The offset of the field at `path` within `layout`.
    fn path_offset(&self, layout: &lir::Layout, path: &[usize]) -> Result<usize, String> {
        let mut offset = 0;
        let mut current = layout;
        for &index in path {
            match current {
                lir::Layout::Struct(fields) => {
                    let field = fields
                        .get(index)
                        .ok_or_else(|| format!("field {index} is out of range"))?;
                    offset += self.offsets(fields)[index];
                    current = field;
                }
                // Each variant is at the union's start.
                lir::Layout::Union(variants) => {
                    current = variants
                        .get(index)
                        .ok_or_else(|| format!("variant {index} is out of range"))?;
                }
                _ => return Err(format!("a path goes into a non-struct {current:?}")),
            }
        }
        Ok(offset)
    }

    /// The offset of field `index` of a data object of `fields`.
    fn field_offset(&self, fields: &[lir::Field], index: usize) -> Result<usize, String> {
        let layouts: Vec<lir::Layout> = fields.iter().map(field_layout).collect();
        self.path_offset(&lir::Layout::Struct(layouts), &[index])
    }

    /// Lay out a data object's fields, each at its natural alignment.
    fn lay_out(&self, fields: &[lir::Field]) -> Result<LaidOut, String> {
        let mut out = LaidOut {
            bytes: Vec::new(),
            align: 1,
            addresses: Vec::new(),
        };
        self.place_all(fields, &mut out)?;
        let align = self.align(&lir::Layout::Struct(
            fields.iter().map(field_layout).collect(),
        ));
        // Every object occupies at least one byte, so that distinct objects
        // have distinct addresses, and its size is a multiple of its
        // alignment.
        out.bytes
            .resize(out.bytes.len().max(1).next_multiple_of(align), 0);
        out.align = u64::try_from(align).map_err(|e| e.to_string())?;
        Ok(out)
    }

    /// Append `fields` in order, each at its natural alignment.
    fn place_all(&self, fields: &[lir::Field], out: &mut LaidOut) -> Result<(), String> {
        for field in fields {
            self.place(field, out)?;
        }
        Ok(())
    }

    /// Append `field` at its natural alignment.
    fn place(&self, field: &lir::Field, out: &mut LaidOut) -> Result<(), String> {
        let layout = field_layout(field);
        let align = self.align(&layout);
        out.bytes.resize(out.bytes.len().next_multiple_of(align), 0);
        match field {
            lir::Field::U8(n) => out.bytes.push(*n),
            lir::Field::I64(n) => out.bytes.extend(self.in_target_order(n.to_le_bytes(), 8)),
            lir::Field::F64Bits(bits) => {
                out.bytes
                    .extend(self.in_target_order(bits.to_le_bytes(), 8));
            }
            lir::Field::Usize(n) => self.usize(*n, out)?,
            lir::Field::Size(layout) => {
                let size = u64::try_from(self.size(layout)).map_err(|e| e.to_string())?;
                self.usize(size, out)?;
            }
            lir::Field::Offset(layout, path) => {
                let offset = self.path_offset(layout, path)?;
                self.usize(u64::try_from(offset).map_err(|e| e.to_string())?, out)?;
            }
            lir::Field::Address(target) => {
                out.addresses.push((out.bytes.len(), *target, None));
                out.bytes.resize(out.bytes.len() + self.pointer_bytes, 0);
            }
            lir::Field::FieldAddress(target, index) => {
                out.addresses.push((out.bytes.len(), *target, Some(*index)));
                out.bytes.resize(out.bytes.len() + self.pointer_bytes, 0);
            }
            lir::Field::Bytes(raw) => out.bytes.extend(raw),
            lir::Field::Struct(inner) => {
                // A struct is padded to its own alignment, so that it has the
                // same layout embedded as it has on its own.
                let start = out.bytes.len();
                self.place_all(inner, out)?;
                out.bytes.resize(start + self.size(&layout), 0);
            }
            lir::Field::Variant(union, value) => {
                let start = out.bytes.len();
                self.place(value, out)?;
                out.bytes.resize(start + self.size(union), 0);
            }
        }
        Ok(())
    }

    fn usize(&self, n: u64, out: &mut LaidOut) -> Result<(), String> {
        let fits = self.pointer_bytes >= 8 || n >> (8 * self.pointer_bytes) == 0;
        if !fits {
            return Err(format!(
                "{n} doesn't fit in a {}-byte usize",
                self.pointer_bytes
            ));
        }
        out.bytes
            .extend(self.in_target_order(n.to_le_bytes(), self.pointer_bytes));
        Ok(())
    }

    /// The low `width` bytes of a little-endian value, in target order.
    fn in_target_order(&self, little_endian: [u8; 8], width: usize) -> Vec<u8> {
        let mut bytes = little_endian[..width].to_vec();
        if self.endianness == Endianness::Big {
            bytes.reverse();
        }
        bytes
    }
}

fn mangle(name: &str) -> String {
    format!("trendy_op_{name}")
}

#[cfg(test)]
mod tests {
    use object::{Object, ObjectSection, ObjectSymbol};

    use super::*;

    fn program(data: Vec<lir::Data>) -> lir::Program {
        lir::Program {
            functions: vec![lir::Function {
                name: "main".to_string(),
                params: None,
                result: None,
                slots: Vec::new(),
                blocks: vec![lir::Block {
                    insts: Vec::new(),
                    terminator: lir::Terminator::Return,
                    failure: None,
                }],
            }],
            data,
            entry: lir::FunctionId(0),
        }
    }

    fn variable(name: &str, fields: Vec<lir::Field>) -> lir::Data {
        lir::Data {
            kind: lir::DataKind::Variable {
                name: name.to_string(),
            },
            writable: true,
            fields,
        }
    }

    /// The bytes of `size` at the symbol whose name ends with `suffix`, and
    /// whether a relocation starts there. Mach-O prefixes symbols with `_`.
    fn symbol_bytes(object: &object::File, suffix: &str, size: usize) -> (Vec<u8>, bool) {
        let symbol = object
            .symbols()
            .find(|s| s.name().is_ok_and(|n| n.ends_with(suffix)))
            .unwrap_or_else(|| panic!("no symbol {suffix}"));
        let section = object
            .section_by_index(symbol.section_index().unwrap())
            .unwrap();
        let offset = usize::try_from(symbol.address() - section.address()).unwrap();
        let bytes = section.data().unwrap()[offset..offset + size].to_vec();
        let relocated = section
            .relocations()
            .any(|(at, _)| at == u64::try_from(offset).unwrap());
        (bytes, relocated)
    }

    #[test]
    fn variables_are_emitted_as_data() {
        let object_bytes = compile(&program(vec![
            variable("i", vec![lir::Field::I64(-2)]),
            variable("r", vec![lir::Field::F64Bits((-0.5_f64).to_bits())]),
            variable("b", vec![lir::Field::U8(1)]),
            lir::Data {
                kind: lir::DataKind::Constant,
                writable: false,
                fields: vec![lir::Field::Bytes(b"hey".to_vec())],
            },
            variable(
                "t",
                vec![lir::Field::Address(lir::DataId(3)), lir::Field::Usize(3)],
            ),
        ]))
        .unwrap();
        let object = object::File::parse(&*object_bytes).unwrap();

        let (i, _) = symbol_bytes(&object, "trendy_var_i", 8);
        assert_eq!(i, (-2_i64).to_le_bytes());
        let (r, _) = symbol_bytes(&object, "trendy_var_r", 8);
        assert_eq!(r, (-0.5_f64).to_le_bytes());
        let (b, _) = symbol_bytes(&object, "trendy_var_b", 1);
        assert_eq!(b, [1]);
        let (constant, _) = symbol_bytes(&object, "trendy_const_3", 3);
        assert_eq!(constant, b"hey");
        let (t, relocated) = symbol_bytes(&object, "trendy_var_t", 16);
        assert!(relocated, "the text pointer needs a relocation");
        assert_eq!(t[8..], 3_u64.to_le_bytes());
    }

    #[test]
    fn addresses_in_read_only_arrays_are_relocated() {
        // A relation whose one tuple has a relation-valued attribute: the
        // outer array holds an address of the inner array.
        let constant = |fields| lir::Data {
            kind: lir::DataKind::Constant,
            writable: false,
            fields,
        };
        let element = |n| lir::Field::Struct(vec![lir::Field::I64(n)]);
        let object_bytes = compile(&program(vec![
            constant(vec![element(1), element(2)]),
            constant(vec![lir::Field::Struct(vec![
                lir::Field::Address(lir::DataId(0)),
                lir::Field::Usize(2),
            ])]),
            variable(
                "r",
                vec![lir::Field::Address(lir::DataId(1)), lir::Field::Usize(1)],
            ),
        ]))
        .unwrap();
        let object = object::File::parse(&*object_bytes).unwrap();

        let (inner, _) = symbol_bytes(&object, "trendy_const_0", 16);
        assert_eq!(inner[..8], 1_i64.to_le_bytes());
        assert_eq!(inner[8..], 2_i64.to_le_bytes());
        let (outer, relocated) = symbol_bytes(&object, "trendy_const_1", 16);
        assert!(relocated, "the inner relation's address needs a relocation");
        assert_eq!(outer[8..], 2_u64.to_le_bytes());
        let (r, relocated) = symbol_bytes(&object, "trendy_var_r", 16);
        assert!(relocated);
        assert_eq!(r[8..], 1_u64.to_le_bytes());
    }

    #[test]
    fn calls_copies_and_runtime_imports() {
        let args = lir::Layout::Struct(vec![lir::Layout::I64]);
        let callee = lir::Function {
            name: "show".to_string(),
            params: Some(args.clone()),
            result: None,
            slots: Vec::new(),
            blocks: vec![lir::Block {
                insts: vec![lir::Inst::CallRuntime {
                    function: lir::RuntimeFn::DebugValue,
                    args: vec![
                        lir::Place {
                            base: lir::Base::Params,
                            path: vec![0],
                        },
                        lir::Place::whole(lir::Base::Data(lir::DataId(0))),
                    ],
                }],
                terminator: lir::Terminator::Return,
                failure: None,
            }],
        };
        let main = lir::Function {
            name: "main".to_string(),
            params: None,
            result: None,
            slots: vec![args],
            blocks: vec![
                lir::Block {
                    insts: vec![
                        lir::Inst::Copy {
                            dst: lir::Place {
                                base: lir::Base::Slot(lir::SlotId(0)),
                                path: vec![0],
                            },
                            src: lir::Place {
                                base: lir::Base::Data(lir::DataId(0)),
                                path: vec![0],
                            },
                            layout: lir::Layout::I64,
                        },
                        lir::Inst::Call {
                            function: lir::FunctionId(0),
                            args: Some(lir::Place::whole(lir::Base::Slot(lir::SlotId(0)))),
                            result: None,
                        },
                    ],
                    terminator: lir::Terminator::Return,
                    failure: Some(lir::BlockId(1)),
                },
                lir::Block {
                    insts: Vec::new(),
                    terminator: lir::Terminator::Failed,
                    failure: None,
                },
            ],
        };
        let object_bytes = compile(&lir::Program {
            functions: vec![callee, main],
            data: vec![variable("x", vec![lir::Field::I64(5)])],
            entry: lir::FunctionId(1),
        })
        .unwrap();
        let object = object::File::parse(&*object_bytes).unwrap();
        let imported = object
            .symbols()
            .filter(ObjectSymbol::is_undefined)
            .filter_map(|s| s.name().ok().map(str::to_string))
            .collect::<Vec<_>>();
        assert!(
            imported
                .iter()
                .any(|n| n.ends_with("trendy_rt_debug_value")),
            "{imported:?}"
        );
    }

    #[test]
    fn descriptor_fields_resolve_sizes_and_offsets() {
        let layout = Target {
            pointer_bytes: 8,
            endianness: Endianness::Little,
        };
        let tuple = lir::Layout::Struct(vec![lir::Layout::U8, lir::Layout::I64]);
        let laid_out = layout
            .lay_out(&[
                lir::Field::Size(tuple.clone()),
                lir::Field::Offset(tuple, vec![1]),
            ])
            .unwrap();
        assert_eq!(laid_out.bytes[..8], 16_u64.to_le_bytes());
        assert_eq!(laid_out.bytes[8..], 8_u64.to_le_bytes());
    }

    #[test]
    fn layout_aligns_fields_and_pads_empty_objects() {
        let layout = Target {
            pointer_bytes: 8,
            endianness: Endianness::Little,
        };
        let laid_out = layout
            .lay_out(&[
                lir::Field::Bytes(vec![1]),
                lir::Field::Address(lir::DataId(4)),
            ])
            .unwrap();
        assert_eq!(laid_out.bytes.len(), 16);
        assert_eq!(laid_out.align, 8);
        assert_eq!(laid_out.addresses, [(8, lir::DataId(4), None)]);

        let empty = layout.lay_out(&[lir::Field::Bytes(Vec::new())]).unwrap();
        assert_eq!((empty.bytes.len(), empty.align), (1, 1));
    }

    #[test]
    fn structs_are_padded_to_their_alignment() {
        let layout = Target {
            pointer_bytes: 8,
            endianness: Endianness::Little,
        };
        // tuple { a: tuple { x: integer, y: boolean }, b: boolean }: the inner
        // tuple is 16 bytes on its own, so `b` comes after its padding.
        let laid_out = layout
            .lay_out(&[lir::Field::Struct(vec![
                lir::Field::Struct(vec![lir::Field::I64(7), lir::Field::U8(1)]),
                lir::Field::U8(1),
            ])])
            .unwrap();
        assert_eq!(laid_out.bytes.len(), 24);
        assert_eq!(laid_out.align, 8);
        assert_eq!(laid_out.bytes[8], 1, "inner `y`");
        assert_eq!(laid_out.bytes[16], 1, "outer `b`, after the inner padding");

        let empty = layout.lay_out(&[lir::Field::Struct(Vec::new())]).unwrap();
        assert_eq!((empty.bytes.len(), empty.align), (1, 1));
    }

    #[test]
    fn unions_are_as_big_as_their_biggest_variant() {
        let layout = Target {
            pointer_bytes: 8,
            endianness: Endianness::Little,
        };
        // A boolean, or two integers: 16 bytes, aligned to 8, each variant
        // at the start.
        let union = lir::Layout::Union(vec![
            lir::Layout::Struct(vec![lir::Layout::U8]),
            lir::Layout::Struct(vec![lir::Layout::I64, lir::Layout::I64]),
        ]);
        assert_eq!((layout.size(&union), layout.align(&union)), (16, 8));
        let tagged = lir::Layout::Struct(vec![lir::Layout::Address, union.clone()]);
        assert_eq!(layout.path_offset(&tagged, &[1, 1, 1]), Ok(16));
        assert_eq!(layout.path_offset(&tagged, &[1, 0, 0]), Ok(8));
        // A value of the smaller variant is padded to the union's size, so
        // that what follows it is where it is for the bigger one.
        let laid_out = layout
            .lay_out(&[lir::Field::Struct(vec![
                lir::Field::Variant(union, Box::new(lir::Field::Struct(vec![lir::Field::U8(1)]))),
                lir::Field::U8(2),
            ])])
            .unwrap();
        assert_eq!(laid_out.bytes.len(), 24);
        assert_eq!((laid_out.bytes[0], laid_out.bytes[16]), (1, 2));
    }

    #[test]
    fn addresses_inside_structs_are_relocated_at_their_offset() {
        let layout = Target {
            pointer_bytes: 8,
            endianness: Endianness::Little,
        };
        let laid_out = layout
            .lay_out(&[lir::Field::Struct(vec![
                lir::Field::U8(1),
                lir::Field::Struct(vec![
                    lir::Field::Address(lir::DataId(5)),
                    lir::Field::Usize(2),
                ]),
            ])])
            .unwrap();
        assert_eq!(laid_out.addresses, [(8, lir::DataId(5), None)]);
        assert_eq!(laid_out.bytes.len(), 24);
    }

    #[test]
    fn a_constant_buffer_is_after_its_header() {
        let layout = Target {
            pointer_bytes: 8,
            endianness: Endianness::Little,
        };
        // The text "é": a value addresses its bytes, not their header.
        let buffer = [
            lir::Field::immortal_header(),
            lir::Field::Bytes("é".as_bytes().to_vec()),
        ];
        let value = layout
            .lay_out(&[lir::Field::Struct(vec![
                lir::Field::FieldAddress(lir::DataId(0), 1),
                lir::Field::Usize(2),
            ])])
            .unwrap();
        assert_eq!(value.addresses, [(0, lir::DataId(0), Some(1))]);
        assert_eq!(layout.field_offset(&buffer, 1), Ok(24));
        let laid_out = layout.lay_out(&buffer).unwrap();
        assert_eq!(
            laid_out.bytes[..8],
            u64::MAX.to_le_bytes(),
            "never released"
        );
        assert!(layout.field_offset(&buffer, 2).is_err());
    }

    #[test]
    fn big_endian_and_narrow_pointers() {
        let layout = Target {
            pointer_bytes: 4,
            endianness: Endianness::Big,
        };
        let laid_out = layout.lay_out(&[lir::Field::Usize(1)]).unwrap();
        assert_eq!(laid_out.bytes, [0, 0, 0, 1]);
        assert!(layout.lay_out(&[lir::Field::Usize(1 << 40)]).is_err());
    }
}
