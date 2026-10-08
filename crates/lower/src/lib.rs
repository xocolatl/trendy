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

    let mut data = Vec::new();
    for variable in &program.variables {
        let fields = value_fields(&variable.init, &mut data);
        data.push(lir::Data {
            kind: lir::DataKind::Variable {
                name: variable.name.clone(),
            },
            writable: true,
            fields,
        });
    }

    lir::Program {
        functions,
        data,
        // Operators and functions correspond one to one, in order.
        entry: lir::FunctionId(program.entry.0),
    }
}

/// The fields representing `value`, adding any constants they point to.
fn value_fields(value: &hir::Value, data: &mut Vec<lir::Data>) -> Vec<lir::Field> {
    match value {
        hir::Value::Boolean(b) => vec![lir::Field::U8(u8::from(*b))],
        hir::Value::Integer(n) => vec![lir::Field::I64(*n)],
        hir::Value::Rational(r) => vec![lir::Field::F64Bits(r.to_bits())],
        hir::Value::Text(text) => {
            let bytes = lir::DataId(data.len());
            data.push(lir::Data {
                kind: lir::DataKind::Constant,
                writable: false,
                fields: vec![lir::Field::Bytes(text.as_bytes().to_vec())],
            });
            let len = u64::try_from(text.len()).expect("lengths fit in 64 bits");
            vec![lir::Field::Address(bytes), lir::Field::Usize(len)]
        }
        // Attributes come in name order, the heading's order.
        hir::Value::Tuple(tuple) => vec![lir::Field::Struct(
            tuple
                .attributes()
                .flat_map(|(_, value)| value_fields(value, data))
                .collect(),
        )],
        hir::Value::Relation(relation) => {
            // The tuples go in a read-only array, in their canonical order.
            let elements: Vec<_> = relation
                .tuples
                .iter()
                .flat_map(|t| value_fields(&hir::Value::Tuple(t.clone()), data))
                .collect();
            let array = lir::DataId(data.len());
            data.push(lir::Data {
                kind: lir::DataKind::Constant,
                writable: false,
                fields: elements,
            });
            let count = u64::try_from(relation.tuples.len()).expect("counts fit in 64 bits");
            vec![lir::Field::Address(array), lir::Field::Usize(count)]
        }
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
            variables: Vec::new(),
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
            variables: Vec::new(),
            entry: hir::OperatorId(1),
        });
        assert_eq!(program.functions[program.entry.0].name, "main");
    }

    #[test]
    fn variables_become_data() {
        let variable = |name: &str, init| hir::Variable {
            name: name.to_string(),
            ty: hir::Value::ty(&init),
            init,
        };
        let program = lower(&hir::Program {
            operators: vec![op("main")],
            variables: vec![
                variable("i", hir::Value::Integer(-7)),
                variable("t", hir::Value::Text("hé".to_string())),
                variable("r", hir::Value::Rational(hir::Rational::new(1.5))),
                variable("b", hir::Value::Boolean(true)),
            ],
            entry: hir::OperatorId(0),
        });
        assert_eq!(
            program.data,
            [
                lir::Data {
                    kind: lir::DataKind::Variable {
                        name: "i".to_string()
                    },
                    writable: true,
                    fields: vec![lir::Field::I64(-7)],
                },
                lir::Data {
                    kind: lir::DataKind::Constant,
                    writable: false,
                    fields: vec![lir::Field::Bytes("hé".as_bytes().to_vec())],
                },
                lir::Data {
                    kind: lir::DataKind::Variable {
                        name: "t".to_string()
                    },
                    writable: true,
                    fields: vec![lir::Field::Address(lir::DataId(1)), lir::Field::Usize(3)],
                },
                lir::Data {
                    kind: lir::DataKind::Variable {
                        name: "r".to_string()
                    },
                    writable: true,
                    fields: vec![lir::Field::F64Bits(1.5_f64.to_bits())],
                },
                lir::Data {
                    kind: lir::DataKind::Variable {
                        name: "b".to_string()
                    },
                    writable: true,
                    fields: vec![lir::Field::U8(1)],
                },
            ]
        );
    }

    #[test]
    fn tuples_become_structs_in_attribute_order() {
        let value = hir::Value::Tuple(hir::TupleValue::new(
            [
                ("t".to_string(), hir::Value::Text("x".to_string())),
                ("a".to_string(), hir::Value::Integer(1)),
            ]
            .into(),
        ));
        let program = lower(&hir::Program {
            operators: vec![op("main")],
            variables: vec![hir::Variable {
                name: "p".to_string(),
                ty: value.ty(),
                init: value,
            }],
            entry: hir::OperatorId(0),
        });
        assert_eq!(
            program.data[1].fields,
            [lir::Field::Struct(vec![
                lir::Field::I64(1),
                lir::Field::Address(lir::DataId(0)),
                lir::Field::Usize(1),
            ])]
        );
    }

    #[test]
    fn relations_point_to_an_array_of_tuples() {
        let tuple = |n| hir::TupleValue::new([("a".to_string(), hir::Value::Integer(n))].into());
        let value = hir::Value::Relation(hir::RelationValue {
            heading: hir::Heading::new(
                [("a".to_string(), hir::Type::Scalar(hir::Scalar::Integer))].into(),
            ),
            tuples: [tuple(2), tuple(1)].into(),
        });
        let program = lower(&hir::Program {
            operators: vec![op("main")],
            variables: vec![hir::Variable {
                name: "r".to_string(),
                ty: value.ty(),
                init: value,
            }],
            entry: hir::OperatorId(0),
        });
        let element = |n| lir::Field::Struct(vec![lir::Field::I64(n)]);
        assert_eq!(program.data[0].fields, [element(1), element(2)]);
        assert!(!program.data[0].writable);
        assert_eq!(
            program.data[1].fields,
            [lir::Field::Address(lir::DataId(0)), lir::Field::Usize(2)]
        );
    }

    #[test]
    fn relation_valued_attributes_point_to_their_own_arrays() {
        let int = |n| hir::Value::Integer(n);
        let tuple = |attributes: Vec<(&str, hir::Value)>| {
            hir::TupleValue::new(
                attributes
                    .into_iter()
                    .map(|(n, v)| (n.to_string(), v))
                    .collect(),
            )
        };
        let x_heading =
            hir::Heading::new([("x".to_string(), hir::Type::Scalar(hir::Scalar::Integer))].into());
        let inner = |xs: &[i64]| {
            hir::Value::Relation(hir::RelationValue {
                heading: x_heading.clone(),
                tuples: xs.iter().map(|&x| tuple(vec![("x", int(x))])).collect(),
            })
        };
        let outer = hir::Value::Relation(hir::RelationValue {
            heading: hir::Heading::new(
                [("inner".to_string(), hir::Type::Relation(x_heading.clone()))].into(),
            ),
            tuples: [tuple(vec![("inner", inner(&[1, 2]))])].into(),
        });
        let program = lower(&hir::Program {
            operators: vec![op("main")],
            variables: vec![hir::Variable {
                name: "r".to_string(),
                ty: outer.ty(),
                init: outer,
            }],
            entry: hir::OperatorId(0),
        });
        let element = |n| lir::Field::Struct(vec![lir::Field::I64(n)]);
        // The inner relation's array comes first, then the outer array, whose
        // one tuple points to it, then the variable.
        assert_eq!(program.data[0].fields, [element(1), element(2)]);
        assert_eq!(
            program.data[1].fields,
            [lir::Field::Struct(vec![
                lir::Field::Address(lir::DataId(0)),
                lir::Field::Usize(2),
            ])]
        );
        assert_eq!(
            program.data[2].fields,
            [lir::Field::Address(lir::DataId(1)), lir::Field::Usize(1)]
        );
    }
}
