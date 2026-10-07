use cranelift_codegen::ir::{AbiParam, Endianness, InstBuilder, types};
use cranelift_codegen::settings::{self, Configurable};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_module::{DataDescription, Linkage, Module, default_libcall_names};
use cranelift_object::{ObjectBuilder, ObjectModule};

use trendy_lir as lir;

/// Compile a program to a native object file.
///
/// Every function becomes a local symbol named `trendy_op_<name>`. A C-ABI
/// `main` is emitted alongside it that calls the program's entry function
/// and returns 0, so the object links against the platform's C startup code.
/// Variables become local data symbols named `trendy_var_<name>`, and
/// constants `trendy_const_<index>`.
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
    define_data(&mut module, &program.data)?;
    let mut ctx = module.make_context();
    let mut fctx = FunctionBuilderContext::new();
    let target = module.target_config();

    let fn_sig = module.make_signature();
    // Indexed by `lir::FunctionId`.
    let ids = program
        .functions
        .iter()
        .map(|f| {
            module
                .declare_function(&mangle(&f.name), Linkage::Local, &fn_sig)
                .map_err(|e| e.to_string())
        })
        .collect::<Result<Vec<_>, _>>()?;

    for (function, &id) in program.functions.iter().zip(&ids) {
        ctx.func.signature = fn_sig.clone();
        let mut b = FunctionBuilder::new(&mut ctx.func, &mut fctx);
        // Indexed by `lir::BlockId`.
        let blocks: Vec<_> = function.blocks.iter().map(|_| b.create_block()).collect();
        for (block, &cl_block) in function.blocks.iter().zip(&blocks) {
            b.switch_to_block(cl_block);
            match block.terminator {
                lir::Terminator::Return => b.ins().return_(&[]),
            };
        }
        b.seal_all_blocks();
        b.finalize(target);

        module
            .define_function(id, &mut ctx)
            .map_err(|e| e.to_string())?;
        module.clear_context(&mut ctx);
    }

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
        let entry_fn = module.declare_func_in_func(ids[program.entry.0], b.func);
        b.ins().call(entry_fn, &[]);
        let zero = b.ins().iconst(types::I32, 0);
        b.ins().return_(&[zero]);
        b.finalize(target);
    }
    module
        .define_function(entry_id, &mut ctx)
        .map_err(|e| e.to_string())?;
    module.clear_context(&mut ctx);

    module.finish().emit().map_err(|e| e.to_string())
}

/// Declare and define every data object, laid out for the target.
fn define_data(module: &mut ObjectModule, data: &[lir::Data]) -> Result<(), String> {
    // Indexed by `lir::DataId`.
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

    let layout = Layout {
        pointer_bytes: usize::from(module.isa().pointer_bytes()),
        endianness: module.isa().endianness(),
    };
    for (d, &id) in data.iter().zip(&ids) {
        let laid_out = layout.lay_out(&d.fields)?;
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
    Ok(())
}

/// A data object's contents, ready to emit.
struct LaidOut {
    bytes: Vec<u8>,
    align: u64,
    /// The offsets of addresses, which need relocations, and their targets.
    addresses: Vec<(usize, lir::DataId)>,
}

/// How the target stores integers and addresses.
struct Layout {
    pointer_bytes: usize,
    endianness: Endianness,
}

impl Layout {
    /// Lay out a data object's fields, each at its natural alignment.
    fn lay_out(&self, fields: &[lir::Field]) -> Result<LaidOut, String> {
        let mut bytes = Vec::new();
        let mut align = 1;
        let mut addresses = Vec::new();
        for field in fields {
            let field_align = match field {
                lir::Field::I64(_) | lir::Field::F64Bits(_) => 8,
                lir::Field::Usize(_) | lir::Field::Address(_) => self.pointer_bytes,
                lir::Field::Bytes(_) => 1,
            };
            bytes.resize(bytes.len().next_multiple_of(field_align), 0);
            align = align.max(field_align);
            match field {
                lir::Field::I64(n) => bytes.extend(self.in_target_order(n.to_le_bytes(), 8)),
                lir::Field::F64Bits(bits) => {
                    bytes.extend(self.in_target_order(bits.to_le_bytes(), 8));
                }
                lir::Field::Usize(n) => {
                    let fits = self.pointer_bytes >= 8 || *n >> (8 * self.pointer_bytes) == 0;
                    if !fits {
                        return Err(format!(
                            "{n} doesn't fit in a {}-byte usize",
                            self.pointer_bytes
                        ));
                    }
                    bytes.extend(self.in_target_order(n.to_le_bytes(), self.pointer_bytes));
                }
                lir::Field::Address(target) => {
                    addresses.push((bytes.len(), *target));
                    bytes.resize(bytes.len() + self.pointer_bytes, 0);
                }
                lir::Field::Bytes(raw) => bytes.extend(raw),
            }
        }
        // Every object occupies at least one byte, so that distinct objects
        // have distinct addresses, and its size is a multiple of its
        // alignment.
        bytes.resize(bytes.len().max(1).next_multiple_of(align), 0);
        Ok(LaidOut {
            bytes,
            align: u64::try_from(align).map_err(|e| e.to_string())?,
            addresses,
        })
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
                blocks: vec![lir::Block {
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
            lir::Data {
                kind: lir::DataKind::Constant,
                writable: false,
                fields: vec![lir::Field::Bytes(b"hey".to_vec())],
            },
            variable(
                "t",
                vec![lir::Field::Address(lir::DataId(2)), lir::Field::Usize(3)],
            ),
        ]))
        .unwrap();
        let object = object::File::parse(&*object_bytes).unwrap();

        let (i, _) = symbol_bytes(&object, "trendy_var_i", 8);
        assert_eq!(i, (-2_i64).to_le_bytes());
        let (r, _) = symbol_bytes(&object, "trendy_var_r", 8);
        assert_eq!(r, (-0.5_f64).to_le_bytes());
        let (constant, _) = symbol_bytes(&object, "trendy_const_2", 3);
        assert_eq!(constant, b"hey");
        let (t, relocated) = symbol_bytes(&object, "trendy_var_t", 16);
        assert!(relocated, "the text pointer needs a relocation");
        assert_eq!(t[8..], 3_u64.to_le_bytes());
    }

    #[test]
    fn layout_aligns_fields_and_pads_empty_objects() {
        let layout = Layout {
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
    fn big_endian_and_narrow_pointers() {
        let layout = Layout {
            pointer_bytes: 4,
            endianness: Endianness::Big,
        };
        let laid_out = layout.lay_out(&[lir::Field::Usize(1)]).unwrap();
        assert_eq!(laid_out.bytes, [0, 0, 0, 1]);
        assert!(layout.lay_out(&[lir::Field::Usize(1 << 40)]).is_err());
    }
}
