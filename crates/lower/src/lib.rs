//! Lowering: HIR to LIR. It chooses how each value is represented in memory,
//! turns statements into instructions, and builds the type descriptors that
//! runtime functions taking `any` receive.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use trendy_hir as hir;
use trendy_lir as lir;

/// Translate HIR to LIR. Infallible: sema has already rejected bad programs.
#[must_use]
pub fn lower(program: &hir::Program, path: &str) -> lir::Program {
    let mut cx = Lowering::new(path, &program.unions);

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
    // Instances of one operator are told apart by their parameters' types,
    // and operators of one name by their parameters' names.
    let overloaded = program.operators.iter().filter(|o| o.name == name).count() > 1;
    let name = if types.is_empty() && !overloaded {
        name
    } else {
        format!("{name}{}", params_text(&params))
    };
    let mut builder = FunctionBuilder::new(program, name, params, variable_data);
    let (lowered, makes) = match kind {
        hir::OperatorKind::Update { locals, body } => {
            builder.declare(locals, cx);
            builder.block(&body, cx);
            (builder.finish(None, cx), false)
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
                layout: cx.layout(&result),
            });
            let makes = builder.scoped;
            let layout = cx.layout(&result);
            (builder.finish(Some(layout), cx), makes)
        }
        hir::OperatorKind::Builtin(_) => unreachable!("built-ins are runtime calls"),
    };
    cx.functions.insert(function_id.0, lowered);
    cx.functions_of.insert(key, (function_id, makes));
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
    /// How values are laid out.
    families: Families,
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
    /// The program's source's path, as a failure writes it.
    path: String,
    /// Each site's data object, made once.
    sites: HashMap<hir::Site, lir::DataId>,
    /// Each file's path, as a text, made once.
    paths: HashMap<hir::File, lir::Field>,
}

impl Lowering {
    /// For the program whose source is at `path`, with the union types
    /// `unions`.
    fn new(path: &str, unions: &BTreeMap<String, Vec<hir::Type>>) -> Self {
        Lowering {
            families: Families::new(unions),
            data: Vec::new(),
            descriptors: HashMap::new(),
            empty: None,
            functions: BTreeMap::new(),
            functions_of: HashMap::new(),
            path: path.to_string(),
            sites: HashMap::new(),
            paths: HashMap::new(),
        }
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
    /// path of its file, the program's or `<prelude>`, as a text, then its
    /// line and column.
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

    /// The path of `file`, the program's or `<prelude>`, as a text.
    fn path_of(&mut self, file: hir::File) -> lir::Field {
        if let Some(path) = self.paths.get(&file) {
            return path.clone();
        }
        let text = match file {
            hir::File::Program => self.path.clone(),
            hir::File::Prelude => "<prelude>".to_string(),
        };
        let path = self.value_field(&hir::Value::Text(text));
        self.paths.insert(file, path.clone());
        path
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
            hir::Value::Text(text) => {
                let bytes = self.buffer(lir::Field::Bytes(text.as_bytes().to_vec()));
                lir::Field::Struct(vec![bytes, lir::Field::Usize(len(text.len()))])
            }
            // A value of a type in a hierarchy: its own type's descriptor's
            // address, then its components, in its type's variant of the
            // room for them.
            hir::Value::Defined(value) if !value.ty.supertypes.is_empty() => {
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
            hir::Value::Relation(relation) => {
                // The tuples go in a read-only array, in their canonical order.
                let elements = relation
                    .tuples
                    .iter()
                    .map(|t| self.value_field(&hir::Value::Tuple(t.clone())))
                    .collect();
                let tuples = self.buffer(lir::Field::Struct(elements));
                lir::Field::Struct(vec![tuples, lir::Field::Usize(len(relation.tuples.len()))])
            }
            hir::Value::Array(array) => {
                let elements = array.elements.iter().map(|e| self.value_field(e)).collect();
                let elements = self.buffer(lir::Field::Struct(elements));
                lir::Field::Struct(vec![elements, lir::Field::Usize(len(array.elements.len()))])
            }
        }
    }

    /// The descriptor of `ty`, laid out as `trendy_runtime::TypeDesc`:
    /// `{ kind: u8, size, attributes: *Attr, count, element: *TypeDesc,
    /// name: *u8, name_len }`.
    fn descriptor(&mut self, ty: &hir::Type) -> lir::DataId {
        if let Some(&id) = self.descriptors.get(ty) {
            return id;
        }
        if ty.root().is_some() {
            return self.hierarchy_descriptor(ty);
        }
        let (kind, heading, element) = match ty {
            hir::Type::Scalar(hir::Scalar::Boolean) => (KIND_BOOLEAN, None, None),
            hir::Type::Defined(defined) => (KIND_DEFINED, Some(&defined.components), None),
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
        // A type of the program's own's name, which values are written
        // with, or none.
        let name = match ty {
            hir::Type::Defined(defined) => defined.name.as_str(),
            _ => "",
        };
        let name_bytes = if name.is_empty() {
            self.empty()
        } else {
            self.constant(vec![lir::Field::Bytes(name.as_bytes().to_vec())])
        };
        let id = self.constant(vec![
            lir::Field::U8(kind),
            lir::Field::Size(self.layout(ty)),
            lir::Field::Address(attributes),
            lir::Field::Usize(len(count)),
            element,
            lir::Field::Address(name_bytes),
            lir::Field::Usize(len(name.len())),
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
        let name = ty.to_string();
        let name_bytes = self.constant(vec![lir::Field::Bytes(name.as_bytes().to_vec())]);
        let id = self.constant(vec![
            lir::Field::U8(KIND_UNION),
            lir::Field::Size(layout),
            lir::Field::Address(components),
            lir::Field::Usize(1),
            lir::Field::Usize(0),
            lir::Field::Address(name_bytes),
            lir::Field::Usize(len(name.len())),
        ]);
        self.descriptors.insert(ty.clone(), id);
        id
    }

    /// The descriptor of the components of a value of `defined`, as its
    /// selector writes them: a `KIND_DEFINED` one, as of a value of the
    /// type outside a hierarchy, which is laid out as its components. In a
    /// hierarchy, a value starts with this one's address, as its own type.
    fn components_descriptor(&mut self, defined: &hir::Defined) -> lir::DataId {
        let alone = hir::Defined {
            supertypes: Vec::new(),
            ..defined.clone()
        };
        self.descriptor(&hir::Type::Defined(Box::new(alone)))
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
    /// Where each tuple at hand is, innermost last: what `Current` refers
    /// to, inside an `Extend`'s values.
    tuples: Vec<lir::Place>,
    /// The site the code built so far leaves the program at: the
    /// statement's being lowered, or a value's in it, if any.
    site: Option<hir::Site>,
    /// Where the site the function is entered at is saved, once a call
    /// changes it while the function has none of its own.
    entered: Option<lir::Place>,
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
            tuples: Vec::new(),
            site: None,
            entered: None,
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
    /// where its `Let`'s value is: computed once, by the expression, whose
    /// values its calling statement owns, or a variable's or a parameter's
    /// own place, which nothing changes while the expression is computed.
    fn bind(&mut self, locals: Vec<hir::Local>) {
        self.local_places = vec![None; locals.len()];
        self.locals = locals;
    }

    /// The statements of `block`, then the release of the references its
    /// local variables hold: every statement that could point into what
    /// they point to has ended, and holds its own. Releasing makes their
    /// pointers null, so that one assigned again, when the block runs
    /// again, has none to give up.
    fn block(&mut self, block: &hir::Block, cx: &mut Lowering) {
        for (site, statement) in &block.statements {
            self.at(Some(*site), cx);
            self.statement(statement, cx);
        }
        for id in &block.locals {
            let place = self.local_places[id.0]
                .as_ref()
                .expect("an update operator's local variables are placed first");
            if place.holds {
                let descriptor = cx.descriptor(&self.locals[id.0].ty);
                self.insts.push(lir::Inst::CallRuntime {
                    function: lir::RuntimeFn::Release,
                    args: vec![
                        place.value.clone(),
                        lir::Place::whole(lir::Base::Data(descriptor)),
                    ],
                });
            }
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
        let entered = if let Some(entered) = &self.entered {
            entered.clone()
        } else {
            let slot = self.slot(lir::Layout::Address);
            self.entered = Some(slot.clone());
            slot
        };
        self.insts.push(lir::Inst::RestoreSite { src: entered });
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

    /// The function, its last block ended with a return, with a result
    /// laid out as `result`, if it has one.
    fn finish(mut self, result: Option<lir::Layout>, cx: &Lowering) -> lir::Function {
        self.end_block(lir::Terminator::Return, lir::BlockId(0));
        if let Some(entered) = self.entered.take() {
            let save = lir::Inst::SaveSite { dst: entered };
            self.blocks[0].insts.insert(0, save);
        }
        lir::Function {
            name: self.name,
            params: params_layout(&self.params, &cx.families),
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

    /// End the current block by stopping the program because of
    /// `failure`, and go on building `next`.
    fn fail(&mut self, failure: lir::Failure, next: lir::BlockId) {
        self.end_block(lir::Terminator::Fail(Box::new(failure)), next);
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
            hir::Statement::Loop(condition, body, check) => {
                self.loop_statement(condition, body, *check, cx);
            }
            hir::Statement::For(variable, array, body) => {
                self.for_statement(*variable, array, body, cx);
            }
        }
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
        let makes = std::mem::take(&mut self.scoped);
        self.each_tuple(&src, cx.layout(&element), cx, |this, at_hand, _, cx| {
            this.local_places[variable.0] = Some(LocalPlace {
                value: at_hand,
                holds: false,
            });
            this.block(body, cx);
        });
        if makes {
            let scope = self.slot(lir::Layout::scope());
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

    /// A loop of `body`, run while `condition` is true, checked as `check`
    /// says: a block that computes the condition, in a scope of its own,
    /// each time, and branches to the body's block, which jumps back to
    /// it, or on to one after the loop. A condition known to be false when
    /// compiling runs a `loop`'s block once, and a `while`'s never; sema
    /// reports one known to be true, which would never end.
    fn loop_statement(
        &mut self,
        condition: &hir::Expr,
        body: &hir::Block,
        check: hir::Check,
        cx: &mut Lowering,
    ) {
        if let hir::Expr::Constant(hir::Value::Boolean(runs)) = condition {
            assert!(!runs, "sema reports a loop that never ends");
            if check == hir::Check::After {
                self.block(body, cx);
            }
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
        self.block(body, cx);
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
            hir::OperatorKind::Builtin(hir::Builtin::Update(builtin)) => match builtin {
                hir::UpdateBuiltin::DebugValue => {
                    self.write(&call.args["v"], lir::RuntimeFn::DebugValue, cx);
                }
                hir::UpdateBuiltin::DebugTable => {
                    self.write(&call.args["r"], lir::RuntimeFn::DebugTable, cx);
                }
                hir::UpdateBuiltin::LoadTextFile => {
                    let target = &call.updated["lines"];
                    let ty = self.assigned(target).0.clone();
                    let (lines, _) =
                        self.loaded(lir::RuntimeFn::LoadTextFile, call, &ty, Vec::new(), cx);
                    self.store(target, lines, cx);
                }
                hir::UpdateBuiltin::LoadCsv => {
                    // Whether the file has a header, and each attribute's
                    // column, given their tuple's type's descriptor, and
                    // where the line each record starts on goes.
                    let header = self.place(&call.args["header"], cx);
                    let columns = &call.args["columns"];
                    let columns_ty = self.type_of(columns);
                    let descriptor = lir::Place::whole(lir::Base::Data(cx.descriptor(&columns_ty)));
                    let columns = self.place(columns, cx);
                    let lines = self.slot(lir::Layout::Struct(vec![
                        lir::Layout::Address,
                        lir::Layout::Usize,
                    ]));
                    let given = vec![header, columns, descriptor, lines.clone()];
                    let target = &call.updated["rows"];
                    let ty = self.assigned(target).0.clone();
                    // Read as the columns say, then converted, each value
                    // of a type of the program's own read in another
                    // representation than the stored one.
                    let hir::Type::Array(record) = &ty else {
                        unreachable!("`load_csv` loads an array");
                    };
                    let read = hir::Type::Array(Box::new(hir::read_as(record, &columns_ty)));
                    let (records, path) =
                        self.loaded(lir::RuntimeFn::LoadCsv, call, &read, given, cx);
                    let file = InFile { path, lines };
                    let rows = self.records_checked(&records, &read, &ty, &file, cx);
                    self.store(target, rows, cx);
                }
            },
            hir::OperatorKind::Update { .. } => {
                let params = params_of(callee, &call.types);
                let (function, _) = function(
                    self.program,
                    self.variable_data,
                    call.operator,
                    &call.types,
                    cx,
                );
                let args = self.arguments(&params, call, cx);
                // An assignment the operator makes must not free what an
                // argument points to, though it can replace the value of
                // the only variable holding it.
                for (i, hir::Parameter { ty, mode }) in params.values().enumerate() {
                    if *mode == hir::Mode::Const && cx.points(ty) {
                        let mut arg = args.clone().expect("a parameter has a slot");
                        arg.path.push(i);
                        let descriptor = cx.descriptor(ty);
                        self.insts.push(lir::Inst::CallRuntime {
                            function: lir::RuntimeFn::Keep,
                            args: vec![arg, lir::Place::whole(lir::Base::Data(descriptor))],
                        });
                        self.scoped = true;
                    }
                }
                self.insts.push(lir::Inst::Call {
                    function,
                    args,
                    result: None,
                });
                // The callee leaves the program at its own site.
                self.after_call(cx);
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
        let path = self.place(&call.args["path"], cx);
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
    /// program's own in them read in another possible representation than
    /// the stored one converted to it (see [`hir::read_as`]), and each
    /// checked against its type's constraints, as selecting it would, inner
    /// values first. The program stops at the first broken, writing the
    /// line. The array is `records` itself, if nothing in them is converted,
    /// and else a new one the statement holds.
    fn records_checked(
        &mut self,
        records: &lir::Place,
        read: &hir::Type,
        ty: &hir::Type,
        file: &InFile,
        cx: &mut Lowering,
    ) -> lir::Place {
        let (hir::Type::Array(read_record), hir::Type::Array(record)) = (read, ty) else {
            unreachable!("`load_csv` loads an array");
        };
        let converts = read != ty;
        if !converts && !constrained(record) {
            return records.clone();
        }
        let rows = if converts {
            let mut count = records.clone();
            count.path.push(1);
            self.allocated(ty, count, cx)
        } else {
            records.clone()
        };
        self.each_tuple(
            records,
            cx.layout(read_record),
            cx,
            |this, read, index, cx| {
                let row = if converts {
                    this.element(&rows, index, cx.layout(record))
                } else {
                    read.clone()
                };
                let at = Record {
                    file,
                    index: index.clone(),
                };
                let value = Reading {
                    dst: row,
                    src: read,
                    name: String::new(),
                    at: &at,
                };
                this.record_value(&value, record, read_record, cx);
            },
        );
        if converts {
            self.adopt(&rows, ty, cx);
        }
        rows
    }

    /// The value of type `ty` that `value` says where to write, which
    /// `load_csv` read as `read`: written there, unless that is where it
    /// was read, converted if `read` is another type, and checked, with each
    /// value of a type of the program's own in it, inner first (see
    /// [`FunctionBuilder::records_checked`]). A value of a type of the
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
            if value.dst != value.src {
                self.insts.push(lir::Inst::Copy {
                    dst: value.dst.clone(),
                    src: value.src.clone(),
                    layout: cx.layout(ty),
                });
            }
            self.value_checked(&value.dst, ty, &value.name, value.at, cx);
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
        let target = if in_hierarchy {
            let stored = hir::Type::Tuple(defined.components.clone());
            Reading {
                dst: self.slot(cx.layout(&stored)),
                ..value.clone()
            }
        } else {
            value.clone()
        };
        let first = fields.named().next().map(|(c, _)| c);
        match first.and_then(|c| defined.representation(c)) {
            Some((own, None)) => self.attributes_read(&target, own, fields, cx),
            Some((own, Some(other))) => {
                let components = Reading {
                    dst: self.slot(cx.layout(&hir::Type::Tuple(own.clone()))),
                    ..value.clone()
                };
                self.attributes_read(&components, own, fields, cx);
                let stored = self.converted(other.to, own, components.dst, cx);
                self.insts.push(lir::Inst::Copy {
                    dst: target.dst.clone(),
                    src: stored,
                    layout: cx.layout(&hir::Type::Tuple(defined.components.clone())),
                });
            }
            None => unreachable!("a type's value is read in one of its representations"),
        }
        self.constraints_checked(defined, &target.dst, &value.name, value.at, cx);
        if in_hierarchy {
            self.made_in_hierarchy(defined, target.dst, value.dst.clone(), cx);
        }
    }

    /// The value at `dst` of `defined`, a type in a hierarchy, made of the
    /// components at `components`: its own type's descriptor's address,
    /// then them.
    fn made_in_hierarchy(
        &mut self,
        defined: &hir::Defined,
        components: lir::Place,
        dst: lir::Place,
        cx: &mut Lowering,
    ) {
        let own = lir::Place::whole(lir::Base::Data(cx.components_descriptor(defined)));
        let mut tag = dst.clone();
        tag.path.push(0);
        self.insts.push(lir::Inst::Address { dst: tag, src: own });
        let mut at = dst;
        at.path.extend(cx.families.components_path(defined));
        self.insts.push(lir::Inst::Copy {
            dst: at,
            src: components,
            layout: cx.layout(&hir::Type::Tuple(defined.components.clone())),
        });
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
        let conversion = hir::Expr::Call(hir::Call {
            operator,
            types: Vec::new(),
            args,
            updated: BTreeMap::new(),
        });
        self.tuples.push(value);
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

    /// Each value of a type of the program's own in the value of type `ty`,
    /// named `name`, at `value` in a record of `at`, checked against its
    /// type's constraints, inner values first, then the value itself.
    fn value_checked(
        &mut self,
        value: &lir::Place,
        ty: &hir::Type,
        name: &str,
        at: &Record,
        cx: &mut Lowering,
    ) {
        let Some(heading) = ty.fields() else {
            return;
        };
        for (i, (attribute, ty)) in heading.attributes().enumerate() {
            let mut inner = value.clone();
            inner.path.push(i);
            self.value_checked(&inner, ty, &dotted(name, attribute), at, cx);
        }
        if let hir::Type::Defined(defined) = ty {
            self.constraints_checked(defined, value, name, at, cx);
        }
    }

    /// The value at `value`, named `name`, of the type of the program's own
    /// `defined`, in a record of `at`, checked against its constraints: the
    /// program stops at the first broken, writing the record's line.
    fn constraints_checked(
        &mut self,
        defined: &hir::Defined,
        value: &lir::Place,
        name: &str,
        at: &Record,
        cx: &mut Lowering,
    ) {
        if defined.constraints.is_empty() {
            return;
        }
        let line = self.element(&at.file.lines, &at.index, lir::Layout::Usize);
        let descriptor = lir::Place::whole(lir::Base::Data(cx.components_descriptor(defined)));
        let attribute = self.text(name, cx);
        self.constrained(
            defined,
            value,
            |this, constraint, cx| lir::Failure::BrokenConstraintInFile {
                value: value.clone(),
                descriptor: descriptor.clone(),
                constraint: this.text(&constraint.text, cx),
                attribute: attribute.clone(),
                path: at.file.path.clone(),
                line: line.clone(),
            },
            cx,
        );
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
            hir::OperatorKind::ReadOnly { .. } | hir::OperatorKind::Update { .. } => {}
        }
        let (function, makes) = function(
            self.program,
            self.variable_data,
            call.operator,
            &call.types,
            cx,
        );
        self.scoped |= makes;
        let args = self.arguments(&params_of(callee, &call.types), call, cx);
        let dst = self.slot(cx.layout(&self.program.result(call)));
        self.insts.push(lir::Inst::Call {
            function,
            args,
            result: Some(dst.clone()),
        });
        // The callee leaves the program at its own site.
        self.after_call(cx);
        dst
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
        let r = &call.args["r"];
        let ty = self.type_of(r);
        let hir::Type::Relation(heading) = &ty else {
            unreachable!("`sum` and `product` take relations");
        };
        let of = heading.get("of").expect("`of` is an attribute");
        let index = attribute_names(&ty)
            .position(|a| a == "of")
            .expect("`of` is an attribute");
        let dst = self.slot(cx.layout(of));
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
        let tuple = cx.layout(&hir::Type::Tuple(heading.clone()));
        let value = cx.layout(&of_ty);
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
            let value = self.place(&assignment.value, cx);
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
        let (ty, keys) = self.assigned(target);
        let (ty, keys) = (ty.clone(), keys.to_vec());
        if !matches!(ty, hir::Type::Relation(_)) {
            return;
        }
        let names: Vec<&str> = attribute_names(&ty).collect();
        let variable = hir::Expr::Constant(hir::Value::Text(self.target_name(target).to_string()));
        for key in &keys {
            if key.len() == names.len() {
                continue;
            }
            let others: BTreeSet<String> = names
                .iter()
                .filter(|name| !key.contains(**name))
                .map(|name| (*name).to_string())
                .collect();
            let descriptor = lir::Place::whole(lir::Base::Data(cx.descriptor(&ty)));
            let projected = lir::Place::whole(lir::Base::Data(cx.descriptor(&ty.without(&others))));
            let variable = self.place(&variable, cx);
            self.insts.push(lir::Inst::CallRuntime {
                function: lir::RuntimeFn::CheckKey,
                args: vec![value.clone(), descriptor, projected, variable],
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
        if cx.points(&ty) {
            let descriptor = lir::Place::whole(lir::Base::Data(cx.descriptor(&ty)));
            self.insts.push(lir::Inst::CallRuntime {
                function: lir::RuntimeFn::Assign,
                args: vec![dst, value, descriptor],
            });
            self.scoped = true;
        } else {
            self.insts.push(lir::Inst::Copy {
                dst,
                src: value,
                layout: cx.layout(&ty),
            });
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
            hir::Expr::At(site, expr) => self.place_at(*site, expr, cx),
            hir::Expr::Let(id, value, body) => {
                let value = self.place(value, cx);
                self.check_keys(&hir::Target::Local(*id), &value, cx);
                self.local_places[id.0] = Some(LocalPlace {
                    value,
                    holds: false,
                });
                self.place(body, cx)
            }
            hir::Expr::Parameter(name) if self.pointers.contains_key(name) => {
                self.pointee(name, cx)
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
            hir::Expr::Remove(operand, names) => self.removed(operand, names, cx),
            hir::Expr::Rename(operand, renamings) => self.renamed(operand, renamings, cx),
            hir::Expr::And(lhs, rhs) => self.joined(lhs, rhs, cx),
            hir::Expr::Ungroup(operand, names) => self.ungroup(operand, names, cx),
            hir::Expr::Only(operand) => self.only(operand, cx),
            // A value of a type of the program's own is laid out as the tuple
            // of its components.
            hir::Expr::Select(defined, operand) => self.selected(defined, operand, cx),
            hir::Expr::Components(operand) => self.components(operand, cx),
            hir::Expr::Treat(..) | hir::Expr::TreatAs(..) | hir::Expr::Is(..) => {
                self.typed(expr, cx)
            }
            hir::Expr::Semijoin(lhs, rhs) => self.matching(lhs, rhs, false, cx),
            hir::Expr::Combine(combination, lhs, rhs) => self.combine(*combination, lhs, rhs, cx),
            hir::Expr::Antijoin(lhs, rhs) => self.matching(lhs, rhs, true, cx),
            hir::Expr::Restrict(operand, condition) => self.restrict(operand, condition, cx),
            hir::Expr::Current(depth, _) => self.tuples[self.tuples.len() - 1 - depth].clone(),
            hir::Expr::Call(call) => self.read_only_call(call, cx),
            hir::Expr::Extend(operand, _, attributes) => self.extend(expr, operand, attributes, cx),
            // Of the operands of `and` and `or`, those that can fail are
            // computed only when those that can't don't decide the result.
            hir::Expr::Connective(connective, operands)
                if let Some(guarded) = connective.guarded(operands) =>
            {
                self.place(&guarded, cx)
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

    /// `expr`, the `connective` of `operands`, all of which can fail or
    /// none of which can (see [`hir::Connective::guarded`]). Of an `and` or an
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
            hir::Connective::And => lir::BinaryOp::And,
            hir::Connective::Or => lir::BinaryOp::Or,
            hir::Connective::Xor => lir::BinaryOp::Xor,
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
                                op: lir::BinaryOp::And,
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
        cx: &mut Lowering,
    ) -> lir::Place {
        let ty = self.type_of(operand);
        let result_ty = self.type_of(expr);
        let src = self.place(operand, cx);
        match (&ty, &result_ty) {
            (hir::Type::Tuple(heading), hir::Type::Tuple(result)) => {
                let dst = self.slot(cx.layout(&result_ty));
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
                layout: cx.layout(ty),
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
        let tuple = cx.layout(&hir::Type::Tuple(heading.clone()));
        let result_tuple = cx.layout(&hir::Type::Tuple(result.clone()));
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
        let tuple = cx.layout(&hir::Type::Tuple(heading.clone()));
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
        self.adopt(&dst, &ty, cx);
        dst
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
        for leaf in cx.families.leaves_under(ty) {
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
        }
        components
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
        let ty = hir::Type::Defined(Box::new(defined.clone()));
        let descriptor = lir::Place::whole(lir::Base::Data(cx.components_descriptor(defined)));
        self.constrained(
            defined,
            &value,
            |this, constraint, cx| lir::Failure::BrokenConstraint {
                value: value.clone(),
                descriptor: descriptor.clone(),
                constraint: this.text(&constraint.text, cx),
            },
            cx,
        );
        if ty.root().is_none() {
            return value;
        }
        let selected = self.slot(cx.layout(&ty));
        self.made_in_hierarchy(defined, value, selected.clone(), cx);
        selected
    }

    /// Stop the program unless each constraint of `defined`, in order, is
    /// true of the value at `value`, laid out as the tuple of its
    /// components: each a call of its operator with the components as its
    /// arguments, the first that is false failing as `broken` says.
    fn constrained(
        &mut self,
        defined: &hir::Defined,
        value: &lir::Place,
        broken: impl Fn(&mut Self, &hir::Constraint, &mut Lowering) -> lir::Failure,
        cx: &mut Lowering,
    ) {
        let at_hand = hir::Expr::Current(0, defined.components.clone());
        let args: BTreeMap<String, hir::Expr> = defined
            .components
            .attributes()
            .map(|(name, _)| {
                let arg = hir::Expr::Extract(Box::new(at_hand.clone()), name.to_string());
                (name.to_string(), arg)
            })
            .collect();
        for constraint in &defined.constraints {
            let call = hir::Expr::Call(hir::Call {
                operator: constraint.operator,
                types: Vec::new(),
                args: args.clone(),
                updated: BTreeMap::new(),
            });
            self.tuples.push(value.clone());
            let holds = self.place(&call, cx);
            self.tuples.pop();
            let (kept, failed) = (self.new_block(), self.new_block());
            self.end_block(
                lir::Terminator::Branch {
                    condition: holds,
                    then: kept,
                    otherwise: failed,
                },
                failed,
            );
            let failure = broken(self, constraint, cx);
            self.fail(failure, kept);
        }
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
        let src = self.place(operand, cx);
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
        cx: &mut Lowering,
    ) -> lir::Place {
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
        let lhs_tuple = cx.layout(&hir::Type::Tuple(lhs_heading.clone()));
        let rhs_tuple = cx.layout(&hir::Type::Tuple(rhs_heading.clone()));
        let position =
            |heading: &hir::Heading, name: &str| heading.attributes().position(|(a, _)| a == name);
        let sources = attribute_names(&result_ty)
            .map(|name| {
                let (from_rhs, offset) = position(lhs_heading, name).map_or_else(
                    || {
                        let i = position(rhs_heading, name)
                            .expect("each attribute of the result is an operand's");
                        (1, lir::Field::Offset(rhs_tuple.clone(), vec![i]))
                    },
                    |i| (0, lir::Field::Offset(lhs_tuple.clone(), vec![i])),
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
        let dst = self.slot(cx.layout(&result_ty));
        self.insts.push(lir::Inst::CallRuntime {
            function: lir::RuntimeFn::And,
            args: [vec![dst.clone()], args].concat(),
        });
        self.scoped = true;
        dst
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
        } = sorted(relation, &written, &loading.order);
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
            src,
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
        let hir::Type::Array(element) = &array_ty else {
            unreachable!("sema unloads only arrays");
        };
        let relation_ty = self.type_of(expr);
        let hir::Type::Relation(heading) = &relation_ty else {
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
        let src = self.place(array, cx);
        let dst = self.slot(cx.layout(&relation_ty));
        let args = vec![
            dst.clone(),
            src,
            lir::Place::whole(lir::Base::Data(cx.descriptor(&array_ty))),
            lir::Place::whole(lir::Base::Data(cx.descriptor(&relation_ty))),
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
        let dst = self.slot(cx.layout(&lhs_ty));
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
        let dst = self.slot(cx.layout(&ty));
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
            // Values of a type of the program's own by its order, which is
            // then compared with 0.
            hir::BinaryOp::Compare(
                comparison @ (hir::Comparison::Less
                | hir::Comparison::LessOrEqual
                | hir::Comparison::Greater
                | hir::Comparison::GreaterOrEqual),
                hir::Compared::Defined,
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
                hir::Compared::Defined
                | hir::Compared::Tuple
                | hir::Compared::Relation
                | hir::Compared::Array,
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
            hir::Type::Scalar(hir::Scalar::Text) => {
                self.insts.push(lir::Inst::CallRuntime {
                    function: lir::RuntimeFn::CompareText,
                    args: vec![result.clone(), lhs, rhs],
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
                | hir::Compared::Defined
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
    let lhs_tuple = cx.layout(&hir::Type::Tuple(lhs.clone()));
    let rhs_tuple = cx.layout(&hir::Type::Tuple(rhs.clone()));
    lhs.attributes()
        .enumerate()
        .filter_map(|(i, (name, ty))| {
            let j = rhs.attributes().position(|(a, _)| a == name)?;
            Some(lir::Field::Struct(vec![
                lir::Field::Offset(lhs_tuple.clone(), vec![i]),
                lir::Field::Offset(rhs_tuple.clone(), vec![j]),
                lir::Field::Address(cx.descriptor(ty)),
            ]))
        })
        .collect()
}

/// A constant of `layout` with every byte zero, as a place: a value whose
/// pointers are null.
fn zero_constant(layout: &lir::Layout, cx: &mut Lowering) -> lir::Place {
    fn zero(layout: &lir::Layout) -> lir::Field {
        match layout {
            lir::Layout::U8 => lir::Field::U8(0),
            lir::Layout::I64 => lir::Field::I64(0),
            lir::Layout::F64Bits => lir::Field::F64Bits(0),
            lir::Layout::Usize | lir::Layout::Address => lir::Field::Usize(0),
            lir::Layout::Bytes(n) => lir::Field::Bytes(vec![0; *n]),
            lir::Layout::Struct(fields) => lir::Field::Struct(fields.iter().map(zero).collect()),
            lir::Layout::Union(variants) => {
                let first = variants
                    .first()
                    .map_or(lir::Field::Struct(Vec::new()), zero);
                lir::Field::Variant(layout.clone(), Box::new(first))
            }
        }
    }
    lir::Place {
        base: lir::Base::Data(cx.constant(vec![zero(layout)])),
        path: vec![0],
    }
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

fn lower_numeric(numeric: hir::Numeric) -> lir::Numeric {
    match numeric {
        hir::Numeric::Integer => lir::Numeric::Integer,
        hir::Numeric::Approximate => lir::Numeric::Approximate,
        hir::Numeric::Parameter(_) => unreachable!("functions are of instantiated operators"),
    }
}

fn scalar_layout(scalar: hir::Scalar) -> lir::Layout {
    match scalar {
        hir::Scalar::Boolean => lir::Layout::U8,
        hir::Scalar::Integer => lir::Layout::I64,
        hir::Scalar::Approximate => lir::Layout::F64Bits,
        hir::Scalar::Text => lir::Layout::Struct(vec![lir::Layout::Address, lir::Layout::Usize]),
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
/// conversion of another's components, and the runtime orders by those.
fn sorted(relation: &hir::Expr, heading: &hir::Heading, order: &[hir::OrderItem]) -> Sorted {
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
            .map(|c| (c.name.clone(), c.computed(&heading)))
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
            for step in &key.path {
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
                ty: key.ty,
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
    /// `heading`.
    fn computed(&self, heading: &hir::Heading) -> hir::Expr {
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
        hir::Expr::Call(hir::Call {
            operator: self.from,
            types: Vec::new(),
            args,
            updated: BTreeMap::new(),
        })
    }
}

/// Whether a value of type `ty` is or has a value of a type of the
/// program's own with constraints.
fn constrained(ty: &hir::Type) -> bool {
    match ty {
        hir::Type::Tuple(heading) => heading.attributes().any(|(_, ty)| constrained(ty)),
        hir::Type::Defined(defined) => {
            !defined.constraints.is_empty()
                || defined
                    .components
                    .attributes()
                    .any(|(_, ty)| constrained(ty))
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

/// The CSV file `load_csv` read: its path, and the line each record starts
/// on, an array of them.
struct InFile {
    path: lir::Place,
    lines: lir::Place,
}

/// A record of a CSV file, at `index`, from 0, among its records.
struct Record<'f> {
    file: &'f InFile,
    index: lir::Place,
}

/// A value in a record `at`, as `load_csv` reads it: where it goes, in the
/// array the statement fills, where `load_csv` read it, which is the same
/// if it needs no conversion, and its name, as `t.c` reads it.
#[derive(Clone)]
struct Reading<'r> {
    dst: lir::Place,
    src: lir::Place,
    name: String,
    at: &'r Record<'r>,
}

/// How the values of the program's types are laid out. A value of a type
/// in a hierarchy, a union type or a subtype of one, is laid out as every
/// value of the union type at its top, its root: the address of its own
/// type's descriptor, then room for the components of any of the types
/// with them under the root, its leaves, so that a value of a subtype is
/// already a value of its supertypes. A value of any other type of the
/// program's own is laid out as the tuple of its components.
struct Families {
    /// Each root's leaves, in the order the program declares them.
    leaves: HashMap<String, Vec<hir::Defined>>,
    /// Each root's room for its values' components: a
    /// [`lir::Layout::Union`] of its leaves' components, in that order.
    unions: HashMap<String, lir::Layout>,
}

impl Families {
    /// The hierarchies of the union types `unions`, each with its
    /// immediate subtypes.
    fn new(unions: &BTreeMap<String, Vec<hir::Type>>) -> Self {
        fn collect(
            name: &str,
            unions: &BTreeMap<String, Vec<hir::Type>>,
            out: &mut Vec<hir::Defined>,
        ) {
            for subtype in unions.get(name).into_iter().flatten() {
                match subtype {
                    hir::Type::Defined(defined) => out.push((**defined).clone()),
                    hir::Type::Dummy(dummy) => collect(&dummy.name, unions, out),
                    _ => unreachable!("a union type's subtypes are the program's own types"),
                }
            }
        }
        let mut leaves = HashMap::new();
        for root in unions.values().flatten().filter_map(hir::Type::root) {
            if !leaves.contains_key(root) {
                let mut found = Vec::new();
                collect(root, unions, &mut found);
                leaves.insert(root.to_string(), found);
            }
        }
        let mut families = Families {
            leaves,
            unions: HashMap::new(),
        };
        let roots: Vec<String> = families.leaves.keys().cloned().collect();
        for root in roots {
            families.lay_out(&root);
        }
        families
    }

    /// Lay out the hierarchy under `root`, after those its leaves'
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
        let leaves = self.leaves[root].clone();
        let mut inside = Vec::new();
        for leaf in &leaves {
            leaf.components
                .attributes()
                .for_each(|(_, ty)| roots(ty, &mut inside));
        }
        for nested in inside {
            self.lay_out(nested);
        }
        let variants = leaves
            .iter()
            .map(|leaf| self.layout(&hir::Type::Tuple(leaf.components.clone())))
            .collect();
        self.unions
            .insert(root.to_string(), lir::Layout::Union(variants));
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
            return self.leaves[root]
                .iter()
                .any(|leaf| leaf.components.attributes().any(|(_, ty)| self.points(ty)));
        }
        match ty {
            hir::Type::Scalar(scalar) => *scalar == hir::Scalar::Text,
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

    /// The types with components that are `ty`, in a hierarchy, or one of
    /// its subtypes.
    fn leaves_under(&self, ty: &hir::Type) -> Vec<hir::Defined> {
        let root = ty.root().expect("a type in a hierarchy");
        self.leaves[root]
            .iter()
            .filter(|leaf| hir::Type::Defined(Box::new((*leaf).clone())).is_subtype_of(ty))
            .cloned()
            .collect()
    }

    /// Where the components of a value of the type of the program's own
    /// `defined` are, in its layout: in a hierarchy, its leaf's variant of
    /// the root's room, after its type's descriptor's address; otherwise,
    /// all of it.
    fn components_path(&self, defined: &hir::Defined) -> Vec<usize> {
        let Some(root) = defined.supertypes.last() else {
            return Vec::new();
        };
        let variant = self.leaves[root]
            .iter()
            .position(|leaf| leaf.name == defined.name)
            .expect("a type in a hierarchy is one of its root's leaves");
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
                    file: hir::File::Program,
                    line: i + 1,
                    column: 5,
                };
                (site, statement)
            })
            .collect()
    }

    /// The program lowered, as if its source were `test.trendy`, without
    /// the sites it sets, which only
    /// [`statements_and_values_set_their_sites`] is about.
    fn lower_program(program: &hir::Program) -> lir::Program {
        let mut lowered = lower(program, "test.trendy");
        for block in lowered.functions.iter_mut().flat_map(|f| &mut f.blocks) {
            block.insts.retain(|i| !matches!(i, lir::Inst::Site(_)));
        }
        lowered
    }

    fn integer() -> hir::Type {
        hir::Type::Scalar(hir::Scalar::Integer)
    }

    fn user(name: &str, params: &[&str], body: Vec<hir::Statement>) -> hir::Operator {
        hir::Operator {
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

    fn debug_value() -> hir::Operator {
        hir::Operator {
            name: "debug_value".to_string(),
            types: hir::Builtin::Update(hir::UpdateBuiltin::DebugValue).types(),
            params: hir::Builtin::Update(hir::UpdateBuiltin::DebugValue).params(),
            kind: hir::OperatorKind::Builtin(hir::Builtin::Update(hir::UpdateBuiltin::DebugValue)),
        }
    }

    fn program(operators: Vec<hir::Operator>, variables: Vec<hir::Variable>) -> hir::Program {
        let entry = operators.iter().position(|o| o.name == "main").unwrap();
        hir::Program {
            operators,
            variables,
            unions: BTreeMap::new(),
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
            updated: BTreeMap::new(),
        })
    }

    #[test]
    fn statements_and_values_set_their_sites() {
        // `main` calls `f`, then loops while `x < 1`, writing `x`; each
        // statement on a line of its own.
        let x = || hir::Expr::Variable(hir::VariableId(0));
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
        let lowered = lower(
            &program(
                vec![
                    debug_value(),
                    user("f", &[], vec![call(0, vec![("v", x())])]),
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
            ),
            "test.trendy",
        );
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
        // own, then the loop's.
        assert_eq!(lines(&main.blocks[0]), [1, 1, 2]);
        assert!(matches!(main.blocks[0].insts[1], lir::Inst::Call { .. }));
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
        assert_eq!(lowered.functions.len(), 1);
        assert_eq!(lowered.functions[lowered.entry.0].name, "main");
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
        let lowered = lower_program(&program(
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
        let lowered = lower_program(&program(
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
        // The caller passes the variable's address.
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
        assert_eq!(
            functions,
            [
                lir::RuntimeFn::Begin,
                lir::RuntimeFn::Unique,
                lir::RuntimeFn::End
            ]
        );
        assert_eq!(
            calls[1].1,
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
                lir::RuntimeFn::Begin,
                lir::RuntimeFn::Duplicate,
                lir::RuntimeFn::Unique,
                lir::RuntimeFn::DebugValue,
                lir::RuntimeFn::End
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
        let insts = &lowered.functions[0].blocks[0].insts;
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
                },
            ]
        );
    }

    #[test]
    fn calls_passing_a_text_keep_it() {
        // `call f{t: t};`, which makes nothing, but must keep `t`'s bytes
        // from being freed by an assignment `f` makes to `t`.
        let f = hir::Operator {
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
                lir::RuntimeFn::Begin,
                lir::RuntimeFn::Keep,
                lir::RuntimeFn::End
            ]
        );
        assert!(matches!(insts[insts.len() - 2], lir::Inst::Call { .. }));
    }

    #[test]
    fn read_only_operators_become_functions_once_called() {
        // `shout{t: text}: text [ t || "!" ]`, called when running
        // as `debug_value{v: shout{t: t}}`, and `unused`, which isn't.
        let text = || hir::Type::Scalar(hir::Scalar::Text);
        let read_only = |name: &str, value| hir::Operator {
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
        assert_eq!(names, ["main", "shout"]);
        // `shout` stores its value where the caller says, in no scope of
        // its own.
        let shout = &lowered.functions[1];
        assert_eq!(shout.result, Some(scalar_layout(hir::Scalar::Text)));
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
        assert_eq!(names, ["main", "show{v: integer}", "show{v: text}"]);
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
            name: "count".to_string(),
            types: hir::Builtin::ReadOnly(hir::ReadOnlyBuiltin::Count).types(),
            params: hir::Builtin::ReadOnly(hir::ReadOnlyBuiltin::Count).params(),
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
        });
        let lowered = lower_program(&program(
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
            types: hir::Builtin::ReadOnly(hir::ReadOnlyBuiltin::Max).types(),
            params: hir::Builtin::ReadOnly(hir::ReadOnlyBuiltin::Max).params(),
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
        });
        let lowered = lower_program(&program(
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
        let insts: Vec<&lir::Inst> = f.blocks.iter().flat_map(|b| &b.insts).collect();
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
            name: "f".to_string(),
            types: Vec::new(),
            params: [("p".to_string(), hir::Parameter::constant(integer()))].into(),
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
            updated: BTreeMap::new(),
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
            updated: BTreeMap::new(),
        });
        let lowered = lower_program(&program(
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
                updated: BTreeMap::new(),
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
        let mut cx = Lowering::new("test.trendy", &BTreeMap::new());
        let id = cx.descriptor(&hir::Type::Tuple(heading));
        let lir::Field::Address(attributes) = cx.data[id.0].fields[2] else {
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
        let mut cx = Lowering::new("test.trendy", &BTreeMap::new());
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
}
