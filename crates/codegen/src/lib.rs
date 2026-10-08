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
/// Every function becomes a local symbol named `trendy_op_<name>`. A C-ABI
/// `main` is emitted alongside it that calls the program's entry function
/// and returns 0, so the object links against the platform's C startup code.
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
            if f.params.is_some() {
                signature.params.push(AbiParam::new(pointer));
            }
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
        b.ins().call(entry_fn, &[]);
        let zero = b.ins().iconst(types::I32, 0);
        b.ins().return_(&[zero]);
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
        b.append_block_params_for_function_params(blocks[0]);
        b.switch_to_block(blocks[0]);
        let params = b.block_params(blocks[0]).first().copied();

        let frame = Frame {
            function,
            slots: &slots,
            params,
        };
        for (i, (block, &cl_block)) in function.blocks.iter().zip(&blocks).enumerate() {
            if i > 0 {
                b.switch_to_block(cl_block);
            }
            for inst in &block.insts {
                self.inst(inst, &frame, &mut b)?;
            }
            match block.terminator {
                lir::Terminator::Return => b.ins().return_(&[]),
            };
        }
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
            lir::Inst::Call { function, args } => {
                let callee = self
                    .module
                    .declare_func_in_func(self.function_ids[function.0], b.func);
                let args = match args {
                    Some(place) => vec![self.address(place, frame, b)?],
                    None => Vec::new(),
                };
                b.ins().call(callee, &args);
            }
            lir::Inst::CallRuntime { function, args } => {
                let id = self.runtime_function(*function, args.len())?;
                let callee = self.module.declare_func_in_func(id, b.func);
                let args = args
                    .iter()
                    .map(|place| self.address(place, frame, b))
                    .collect::<Result<Vec<_>, _>>()?;
                b.ins().call(callee, &args);
            }
            lir::Inst::Not { dst, src } => {
                let value = self.load_bool(src, frame, b)?;
                let result = b.ins().bxor_imm_u(value, 1);
                self.store_bool(dst, result, frame, b)?;
            }
            lir::Inst::Bool { dst, op, lhs, rhs } => {
                let lhs = self.load_bool(lhs, frame, b)?;
                let rhs = self.load_bool(rhs, frame, b)?;
                let result = match op {
                    lir::BoolOp::And => b.ins().band(lhs, rhs),
                    lir::BoolOp::Or => b.ins().bor(lhs, rhs),
                    lir::BoolOp::Xor => b.ins().bxor(lhs, rhs),
                    lir::BoolOp::Implies => {
                        let not_lhs = b.ins().bxor_imm_u(lhs, 1);
                        b.ins().bor(not_lhs, rhs)
                    }
                };
                self.store_bool(dst, result, frame, b)?;
            }
        }
        Ok(())
    }

    /// The boolean at `place`: `0` or `1`, as an `i8`.
    fn load_bool(
        &mut self,
        place: &lir::Place,
        frame: &Frame,
        b: &mut FunctionBuilder,
    ) -> Result<Value, String> {
        let address = self.address(place, frame, b)?;
        Ok(b.ins().load(types::I8, MemFlagsData::trusted(), address, 0))
    }

    fn store_bool(
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

    /// Import a runtime function, which takes `params` pointers.
    fn runtime_function(
        &mut self,
        function: lir::RuntimeFn,
        params: usize,
    ) -> Result<FuncId, String> {
        if let Some(&id) = self.runtime.get(function.symbol()) {
            return Ok(id);
        }
        let mut signature = self.module.make_signature();
        for _ in 0..params {
            signature
                .params
                .push(AbiParam::new(self.config.pointer_type()));
        }
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
        let base_layout = match place.base {
            lir::Base::Data(id) => &self.data_layouts[id.0],
            lir::Base::Slot(slot) => &frame.function.slots[slot.0],
            lir::Base::Params => frame
                .function
                .params
                .as_ref()
                .ok_or("parameters used in a function without them")?,
        };
        let offset = i64::try_from(self.target.path_offset(base_layout, &place.path)?)
            .map_err(|e| e.to_string())?;
        Ok(match place.base {
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
        })
    }
}

/// The function being built: its stack slots and its parameter pointer.
struct Frame<'a> {
    function: &'a lir::Function,
    slots: &'a [cranelift_codegen::ir::StackSlot],
    params: Option<Value>,
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
        for (offset, target) in laid_out.addresses {
            let target = module.declare_data_in_data(ids[target.0], &mut description);
            let offset = u32::try_from(offset).map_err(|e| e.to_string())?;
            description.write_data_addr(offset, target, 0);
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
    /// The offsets of addresses, which need relocations, and their targets.
    addresses: Vec<(usize, lir::DataId)>,
}

/// The layout of a data field.
fn field_layout(field: &lir::Field) -> lir::Layout {
    match field {
        lir::Field::U8(_) => lir::Layout::U8,
        lir::Field::I64(_) => lir::Layout::I64,
        lir::Field::F64Bits(_) => lir::Layout::F64Bits,
        lir::Field::Usize(_) | lir::Field::Size(_) | lir::Field::Offset(..) => lir::Layout::Usize,
        lir::Field::Address(_) => lir::Layout::Address,
        lir::Field::Bytes(raw) => lir::Layout::Bytes(raw.len()),
        lir::Field::Struct(inner) => lir::Layout::Struct(inner.iter().map(field_layout).collect()),
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
            lir::Layout::Struct(fields) => fields.iter().map(|f| self.align(f)).max().unwrap_or(1),
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
            let lir::Layout::Struct(fields) = current else {
                return Err(format!("a path goes into a non-struct {current:?}"));
            };
            let field = fields
                .get(index)
                .ok_or_else(|| format!("field {index} is out of range"))?;
            offset += self.offsets(fields)[index];
            current = field;
        }
        Ok(offset)
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
            lir::Field::Offset(layout, index) => {
                let offset = self.path_offset(layout, &[*index])?;
                self.usize(u64::try_from(offset).map_err(|e| e.to_string())?, out)?;
            }
            lir::Field::Address(target) => {
                out.addresses.push((out.bytes.len(), *target));
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
                slots: Vec::new(),
                blocks: vec![lir::Block {
                    insts: Vec::new(),
                    terminator: lir::Terminator::Return,
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
            }],
        };
        let main = lir::Function {
            name: "main".to_string(),
            params: None,
            slots: vec![args],
            blocks: vec![lir::Block {
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
                    },
                ],
                terminator: lir::Terminator::Return,
            }],
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
                lir::Field::Offset(tuple, 1),
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
        assert_eq!(laid_out.addresses, [(8, lir::DataId(4))]);

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
        assert_eq!(laid_out.addresses, [(8, lir::DataId(5))]);
        assert_eq!(laid_out.bytes.len(), 24);
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
