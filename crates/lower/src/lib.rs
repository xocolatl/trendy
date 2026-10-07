use trendy_hir as hir;
use trendy_lir as lir;

/// Translate HIR to LIR. Infallible: sema has already rejected bad programs.
#[must_use]
pub fn lower(program: &hir::Program) -> lir::Program {
    let functions = program
        .operators
        .iter()
        .map(|op| lir::Function {
            name: op.name.clone(),
            blocks: vec![lir::Block {
                terminator: lir::Terminator::Return,
            }],
        })
        .collect();

    lir::Program {
        functions,
        // Operators and functions correspond one to one, in order.
        entry: lir::FunctionId(program.entry.0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn op(name: &str) -> hir::Operator {
        hir::Operator {
            name: name.to_string(),
        }
    }

    #[test]
    fn empty_operator_is_one_returning_block() {
        let program = lower(&hir::Program {
            operators: vec![op("main")],
            entry: hir::OperatorId(0),
        });
        assert_eq!(
            program.functions,
            [lir::Function {
                name: "main".to_string(),
                blocks: vec![lir::Block {
                    terminator: lir::Terminator::Return,
                }],
            }]
        );
    }

    #[test]
    fn entry_follows_operator() {
        let program = lower(&hir::Program {
            operators: vec![op("a"), op("main")],
            entry: hir::OperatorId(1),
        });
        assert_eq!(program.functions[program.entry.0].name, "main");
    }
}
