//! Lowering: HIR to LIR. It chooses how each value is represented in memory,
//! turns statements into instructions, and builds the type descriptors that
//! runtime functions taking `any` receive.

use std::collections::{BTreeMap, HashMap};

use trendy_hir as hir;
use trendy_lir as lir;

/// Translate HIR to LIR. Infallible: sema has already rejected bad programs.
#[must_use]
pub fn lower(program: &hir::Program) -> lir::Program {
    let mut cx = Lowering {
        data: Vec::new(),
        descriptors: HashMap::new(),
        empty: None,
    };

    let mut variable_data = Vec::new();
    for variable in &program.variables {
        let field = cx.value_field(&variable.init);
        variable_data.push(cx.push(lir::Data {
            kind: lir::DataKind::Variable {
                name: variable.name.clone(),
            },
            writable: true,
            fields: vec![field],
        }));
    }

    // Only user operators become functions; built-ins are runtime calls.
    let mut function_ids = HashMap::new();
    for (i, op) in program.operators.iter().enumerate() {
        if matches!(op.kind, hir::OperatorKind::User { .. }) {
            function_ids.insert(hir::OperatorId(i), lir::FunctionId(function_ids.len()));
        }
    }
    let mut functions = Vec::new();
    for (i, op) in program.operators.iter().enumerate() {
        let hir::OperatorKind::User { body } = &op.kind else {
            continue;
        };
        let mut function = FunctionBuilder {
            program,
            within: hir::OperatorId(i),
            function_ids: &function_ids,
            variable_data: &variable_data,
            slots: Vec::new(),
            insts: Vec::new(),
        };
        for statement in body {
            function.statement(statement, &mut cx);
        }
        functions.push(lir::Function {
            name: op.name.clone(),
            params: params_layout(&op.params),
            slots: function.slots,
            blocks: vec![lir::Block {
                insts: function.insts,
                terminator: lir::Terminator::Return,
            }],
        });
    }

    lir::Program {
        functions,
        data: cx.data,
        entry: function_ids[&program.entry],
    }
}

/// The program-wide part of lowering: the data objects.
struct Lowering {
    data: Vec<lir::Data>,
    /// Each type's descriptor, built once.
    descriptors: HashMap<hir::Type, lir::DataId>,
    /// An empty constant, for addresses that are never read.
    empty: Option<lir::DataId>,
}

impl Lowering {
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
            hir::Value::Rational(r) => lir::Field::F64Bits(r.to_bits()),
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
        }
    }

    /// The descriptor of `ty`, laid out as `trendy_runtime::TypeDesc`:
    /// `{ kind: u8, size, attributes: *Attr, count, element_size }`.
    fn descriptor(&mut self, ty: &hir::Type) -> lir::DataId {
        if let Some(&id) = self.descriptors.get(ty) {
            return id;
        }
        let (kind, heading, element) = match ty {
            hir::Type::Scalar(hir::Scalar::Boolean) => (KIND_BOOLEAN, None, None),
            hir::Type::Scalar(hir::Scalar::Integer) => (KIND_INTEGER, None, None),
            hir::Type::Scalar(hir::Scalar::Rational) => (KIND_RATIONAL, None, None),
            hir::Type::Scalar(hir::Scalar::Text) => (KIND_TEXT, None, None),
            hir::Type::Tuple(heading) => (KIND_TUPLE, Some(heading), None),
            hir::Type::Relation(heading) => (
                KIND_RELATION,
                Some(heading),
                Some(layout(&hir::Type::Tuple(heading.clone()))),
            ),
        };
        let (attributes, count) = match heading {
            Some(heading) => (self.attributes(heading), heading.attributes().count()),
            None => (self.empty(), 0),
        };
        let id = self.constant(vec![
            lir::Field::U8(kind),
            lir::Field::Size(layout(ty)),
            lir::Field::Address(attributes),
            lir::Field::Usize(len(count)),
            element.map_or(lir::Field::Usize(0), lir::Field::Size),
        ]);
        self.descriptors.insert(ty.clone(), id);
        id
    }

    /// A heading's attributes, laid out as an array of `trendy_runtime::Attr`:
    /// `{ name: *u8, name_len, offset, desc: *TypeDesc }`, in name order.
    fn attributes(&mut self, heading: &hir::Heading) -> lir::DataId {
        let tuple = layout(&hir::Type::Tuple(heading.clone()));
        let entries = heading
            .attributes()
            .enumerate()
            .map(|(i, (name, ty))| {
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
const KIND_RATIONAL: u8 = 2;
const KIND_TEXT: u8 = 3;
const KIND_TUPLE: u8 = 4;
const KIND_RELATION: u8 = 5;

/// The part of lowering for one function.
struct FunctionBuilder<'a> {
    program: &'a hir::Program,
    /// The operator this function is.
    within: hir::OperatorId,
    function_ids: &'a HashMap<hir::OperatorId, lir::FunctionId>,
    variable_data: &'a [lir::DataId],
    slots: Vec<lir::Layout>,
    insts: Vec<lir::Inst>,
}

impl FunctionBuilder<'_> {
    fn statement(&mut self, statement: &hir::Statement, cx: &mut Lowering) {
        let hir::Statement::Call { operator, args } = statement;
        let callee = self.program.operator(*operator);
        match &callee.kind {
            hir::OperatorKind::Builtin(hir::Builtin::DebugValue) => {
                let value = &args["v"];
                let ty = self.program.type_of(value, self.within);
                let value = self.place(value, cx);
                let descriptor = lir::Place::whole(lir::Base::Data(cx.descriptor(&ty)));
                self.insts.push(lir::Inst::CallRuntime {
                    function: lir::RuntimeFn::DebugValue,
                    args: vec![value, descriptor],
                });
            }
            hir::OperatorKind::User { .. } => {
                // The arguments are a tuple: build it in a stack slot, in
                // parameter name order, and pass its address.
                let args_place = params_layout(&callee.params).map(|params| {
                    let slot = self.slot(params);
                    for (i, (name, param)) in callee.params.iter().enumerate() {
                        let src = self.place(&args[name], cx);
                        self.insts.push(lir::Inst::Copy {
                            dst: lir::Place {
                                base: slot.base,
                                path: vec![i],
                            },
                            src,
                            layout: param_layout(param),
                        });
                    }
                    slot
                });
                self.insts.push(lir::Inst::Call {
                    function: self.function_ids[operator],
                    args: args_place,
                });
            }
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
            hir::Expr::Parameter(name) => {
                let params = &self.program.operator(self.within).params;
                let index = params
                    .keys()
                    .position(|p| p == name)
                    .expect("sema resolved the parameter");
                lir::Place {
                    base: lir::Base::Params,
                    path: vec![index],
                }
            }
            hir::Expr::Not(operand) => {
                let src = self.place(operand, cx);
                let dst = self.slot(lir::Layout::U8);
                self.insts.push(lir::Inst::Not {
                    dst: dst.clone(),
                    src,
                });
                dst
            }
            hir::Expr::Binary(op, lhs, rhs) => {
                let lhs = self.place(lhs, cx);
                let rhs = self.place(rhs, cx);
                let dst = self.slot(lir::Layout::U8);
                let op = match op {
                    hir::BoolOp::And => lir::BoolOp::And,
                    hir::BoolOp::Or => lir::BoolOp::Or,
                    hir::BoolOp::Xor => lir::BoolOp::Xor,
                    hir::BoolOp::Implies => lir::BoolOp::Implies,
                };
                self.insts.push(lir::Inst::Bool {
                    dst: dst.clone(),
                    op,
                    lhs,
                    rhs,
                });
                dst
            }
        }
    }

    /// A new stack slot holding a value with `layout`.
    fn slot(&mut self, layout: lir::Layout) -> lir::Place {
        let slot = lir::SlotId(self.slots.len());
        self.slots.push(layout);
        lir::Place::whole(lir::Base::Slot(slot))
    }
}

/// The layout of a value of type `ty`.
fn layout(ty: &hir::Type) -> lir::Layout {
    match ty {
        hir::Type::Scalar(hir::Scalar::Boolean) => lir::Layout::U8,
        hir::Type::Scalar(hir::Scalar::Integer) => lir::Layout::I64,
        hir::Type::Scalar(hir::Scalar::Rational) => lir::Layout::F64Bits,
        hir::Type::Scalar(hir::Scalar::Text) | hir::Type::Relation(_) => {
            lir::Layout::Struct(vec![lir::Layout::Address, lir::Layout::Usize])
        }
        hir::Type::Tuple(heading) => {
            lir::Layout::Struct(heading.attributes().map(|(_, ty)| layout(ty)).collect())
        }
    }
}

fn param_layout(param: &hir::ParamType) -> lir::Layout {
    match param {
        hir::ParamType::Type(ty) => layout(ty),
        hir::ParamType::Any => unreachable!("only built-ins have `any` parameters"),
    }
}

/// The parameter struct of a user operator, if it has parameters.
fn params_layout(params: &BTreeMap<String, hir::ParamType>) -> Option<lir::Layout> {
    (!params.is_empty()).then(|| lir::Layout::Struct(params.values().map(param_layout).collect()))
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
            params: params
                .iter()
                .map(|p| ((*p).to_string(), hir::ParamType::Type(integer())))
                .collect(),
            kind: hir::OperatorKind::User { body },
        }
    }

    fn debug_value() -> hir::Operator {
        hir::Operator {
            name: "debug_value".to_string(),
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

    fn variable(name: &str, init: hir::Value) -> hir::Variable {
        hir::Variable {
            name: name.to_string(),
            ty: init.ty(),
            init,
        }
    }

    fn call(operator: usize, args: Vec<(&str, hir::Expr)>) -> hir::Statement {
        hir::Statement::Call {
            operator: hir::OperatorId(operator),
            args: args.into_iter().map(|(n, e)| (n.to_string(), e)).collect(),
        }
    }

    #[test]
    fn only_user_operators_become_functions() {
        let lowered = lower(&program(
            vec![debug_value(), user("main", &[], vec![])],
            vec![],
        ));
        assert_eq!(lowered.functions.len(), 1);
        assert_eq!(lowered.functions[lowered.entry.0].name, "main");
        assert_eq!(lowered.functions[0].params, None);
    }

    #[test]
    fn values_are_single_fields() {
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
        assert_eq!(
            lowered.data[2].fields,
            [lir::Field::Struct(vec![
                lir::Field::Address(lir::DataId(1)),
                lir::Field::Usize(3)
            ])]
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
            [lir::Field::Struct(vec![
                lir::Field::Address(lir::DataId(0)),
                lir::Field::Usize(2)
            ])]
        );
    }

    #[test]
    fn boolean_expressions_compute_into_slots() {
        // `debug_value{v: not (a and b)}`, of two variables.
        let a = || Box::new(hir::Expr::Variable(hir::VariableId(0)));
        let b = || Box::new(hir::Expr::Variable(hir::VariableId(1)));
        let expr = hir::Expr::Not(Box::new(hir::Expr::Binary(hir::BoolOp::And, a(), b())));
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
                lir::Inst::Bool {
                    dst: slot(0),
                    op: lir::BoolOp::And,
                    lhs: data(0),
                    rhs: data(1),
                },
                lir::Inst::Not {
                    dst: slot(1),
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
        assert_eq!(
            lowered.functions[0].params,
            Some(lir::Layout::Struct(vec![
                lir::Layout::I64,
                lir::Layout::I64
            ]))
        );
        let main = &lowered.functions[1];
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
                    function: lir::FunctionId(0),
                    args: Some(lir::Place::whole(lir::Base::Slot(lir::SlotId(0)))),
                },
            ]
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
        let show = &lowered.functions[0];
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
        let main = &lowered.functions[1];
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
        let mut cx = Lowering {
            data: Vec::new(),
            descriptors: HashMap::new(),
            empty: None,
        };
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
}
