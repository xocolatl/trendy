//! The prelude: the declarations of the built-in operators, checked against
//! the operators the compiler implements.

use trendy_hir as hir;
use trendy_syntax::ast::{Item, SourceFile};

use crate::{Namespaces, operators};

/// The types the compiler implements: the scalar types.
fn builtin_types() -> impl Iterator<Item = &'static str> {
    hir::Scalar::ALL.into_iter().map(hir::Scalar::name)
}

/// The loaded prelude: its built-in operators.
#[derive(Debug, Clone)]
pub struct Prelude {
    operators: Vec<hir::Operator>,
}

impl Prelude {
    /// Load the prelude, checking that it declares exactly the built-in
    /// operators the compiler implements, each with the right parameters.
    ///
    /// # Errors
    ///
    /// Describes how the prelude is wrong. The prelude is part of the
    /// compiler, so any error is a compiler bug.
    pub fn load(file: &SourceFile) -> Result<Prelude, String> {
        let no_names = Namespaces::default();
        let mut operators: Vec<hir::Operator> = Vec::new();
        let mut types: Vec<String> = Vec::new();
        for item in file.items() {
            let decl = match item {
                Item::Builtin(decl) => decl,
                Item::BuiltinType(decl) => {
                    let name = decl
                        .name()
                        .and_then(|n| n.ident())
                        .ok_or("a built-in type without a name")?;
                    let name = name.text().to_string();
                    if !builtin_types().any(|t| t == name) {
                        return Err(format!("`{name}` is not a type the compiler implements"));
                    }
                    if types.contains(&name) {
                        return Err(format!("`{name}` is declared more than once"));
                    }
                    types.push(name);
                    continue;
                }
                _ => return Err("the prelude may only declare built-ins".to_string()),
            };
            let ident = decl
                .name()
                .and_then(|n| n.ident())
                .ok_or("a built-in operator without a name")?;
            let name = ident.text();
            let builtin = hir::Builtin::from_name(name)
                .ok_or_else(|| format!("`{name}` is not a built-in the compiler implements"))?;

            let heading = decl
                .params()
                .ok_or_else(|| format!("`{name}` has no parameters"))?;
            let mut types = Vec::new();
            let params = operators::params(&heading, &mut types, &no_names, &mut Vec::new())
                .ok_or_else(|| format!("`{name}` has an invalid parameter type"))?;
            if types != builtin.types() || params != builtin.params() {
                return Err(format!(
                    "`{name}` is declared with parameters that differ from its implementation"
                ));
            }
            if operators.iter().any(|op| op.name == name) {
                return Err(format!("`{name}` is declared more than once"));
            }
            operators.push(hir::Operator {
                name: name.to_string(),
                types,
                params,
                kind: hir::OperatorKind::Builtin(builtin),
            });
        }
        if let Some(builtin) = hir::Builtin::ALL
            .into_iter()
            .find(|b| !operators.iter().any(|op| op.name == b.name()))
        {
            return Err(format!("the prelude doesn't declare `{}`", builtin.name()));
        }
        if let Some(missing) = builtin_types().find(|t| !types.iter().any(|d| d == t)) {
            return Err(format!("the prelude doesn't declare the type `{missing}`"));
        }
        Ok(Prelude { operators })
    }

    /// The built-in operators, in the order the prelude declares them.
    pub(crate) fn operators(&self) -> &[hir::Operator] {
        &self.operators
    }
}
