use cranelift_codegen::ir::{AbiParam, InstBuilder, types};
use cranelift_codegen::settings::{self, Configurable};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_module::{Linkage, Module, default_libcall_names};
use cranelift_object::{ObjectBuilder, ObjectModule};

use trendy_lir as lir;

/// Compile a program to a native object file.
///
/// Every function becomes a local symbol named `trendy_op_<name>`. A C-ABI
/// `main` is emitted alongside it that calls the program's entry function
/// and returns 0, so the object links against the platform's C startup code.
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

fn mangle(name: &str) -> String {
    format!("trendy_op_{name}")
}
