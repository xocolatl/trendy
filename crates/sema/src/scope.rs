//! Which unit each declaration is in, which declarations each unit sees,
//! and what each is called in the program.
//!
//! A unit sees its own declarations and the prelude's. A name declared in
//! one unit of the program is called plainly everywhere, and one declared
//! in several, each a declaration of its own, is qualified by its unit's
//! name in every one of them, `lakes.depths::metres`, so that every name the
//! HIR has is one declaration's: what the program writes is unambiguous,
//! and lowering and the runtime can tell declarations apart by name. No
//! unit can declare one of the prelude's names, which every unit sees, so
//! those are always plain.

use std::collections::HashMap;

use trendy_diag::Span;
use trendy_hir as hir;
use trendy_syntax::ast::VarDecl;

use crate::Namespaces;

/// The program's units: each one's name, and where its file's text ends in
/// the program's, by its [`hir::FileId`], the prelude's last.
#[derive(Clone)]
pub(crate) struct Units {
    units: Vec<(String, usize)>,
}

impl Units {
    pub(crate) fn new(units: Vec<(String, usize)>) -> Self {
        Units { units }
    }

    /// The name of `unit`.
    pub(crate) fn name(&self, unit: hir::FileId) -> &str {
        &self.units[unit.0].0
    }

    /// The prelude's.
    pub(crate) fn prelude(&self) -> hir::FileId {
        hir::FileId(self.units.len() - 1)
    }

    /// The unit `span` is in.
    pub(crate) fn at(&self, span: Span) -> hir::FileId {
        hir::FileId(
            self.units
                .partition_point(|&(_, end)| end <= span.start)
                .min(self.units.len() - 1),
        )
    }

    /// Whether the unit `viewer` sees the declarations of `unit`: its own,
    /// and the prelude's.
    pub(crate) fn sees(&self, viewer: hir::FileId, unit: hir::FileId) -> bool {
        viewer == unit || unit == self.prelude()
    }

    /// What the declaration named `name` in `unit` is called in the
    /// program: its name, or, if it is `ambiguous`, its name qualified by
    /// its unit's.
    pub(crate) fn identity(&self, name: &str, unit: hir::FileId, ambiguous: bool) -> String {
        if ambiguous {
            format!("{}::{name}", self.units[unit.0].0)
        } else {
            name.to_string()
        }
    }
}

/// Whether a name is ambiguous, declared in each of `declaring`: in more
/// than one unit, none of them the prelude, whose names no other unit can
/// have.
pub(crate) fn ambiguous(declaring: impl Iterator<Item = hir::FileId>, units: &Units) -> bool {
    let mut first = None;
    let mut several = false;
    for unit in declaring {
        if unit == units.prelude() {
            return false;
        }
        match first {
            None => first = Some(unit),
            Some(other) => several |= other != unit,
        }
    }
    several
}

/// The declarations of one namespace, each by its name, with the unit it is
/// in, in the order they are declared.
pub(crate) struct Declared<T> {
    by_name: HashMap<String, Vec<(hir::FileId, T)>>,
}

impl<T> Default for Declared<T> {
    fn default() -> Self {
        Declared {
            by_name: HashMap::new(),
        }
    }
}

impl<T> Declared<T> {
    /// Add `value`, the declaration named `name` in `unit`.
    pub(crate) fn insert(&mut self, name: &str, unit: hir::FileId, value: T) {
        self.by_name
            .entry(name.to_string())
            .or_default()
            .push((unit, value));
    }

    /// The declaration named `name` that the unit `viewer` sees, its own or
    /// else the prelude's, with the unit it is in: the first, if its unit
    /// declares it twice, which is an error.
    pub(crate) fn visible(
        &self,
        name: &str,
        viewer: hir::FileId,
        units: &Units,
    ) -> Option<(hir::FileId, &T)> {
        let declared = self.by_name.get(name)?;
        let own = declared.iter().find(|(unit, _)| *unit == viewer);
        own.or_else(|| declared.iter().find(|(unit, _)| units.sees(viewer, *unit)))
            .map(|(unit, value)| (*unit, value))
    }

    /// Each declaration named `name` that the unit `viewer` sees, its own
    /// first, then the prelude's, with the unit it is in.
    pub(crate) fn each_visible<'a>(
        &'a self,
        name: &str,
        viewer: hir::FileId,
        units: &'a Units,
    ) -> impl Iterator<Item = (hir::FileId, &'a T)> {
        let declared = self.by_name.get(name).map_or(&[][..], Vec::as_slice);
        let own = declared.iter().filter(move |(unit, _)| *unit == viewer);
        let others = declared
            .iter()
            .filter(move |(unit, _)| *unit != viewer && units.sees(viewer, *unit));
        own.chain(others).map(|(unit, value)| (*unit, value))
    }

    /// Each declaration named `name`, in any unit, with its unit.
    pub(crate) fn declaring<'a>(
        &'a self,
        name: &str,
    ) -> impl Iterator<Item = (hir::FileId, &'a T)> {
        self.by_name
            .get(name)
            .map_or(&[][..], Vec::as_slice)
            .iter()
            .map(|(unit, value)| (*unit, value))
    }

    /// Remove the declarations named `name` in `unit`.
    pub(crate) fn remove_in(&mut self, name: &str, unit: hir::FileId) {
        if let Some(declared) = self.by_name.get_mut(name) {
            declared.retain(|(u, _)| *u != unit);
        }
    }

    /// Each declaration the unit `viewer` sees, with its name.
    pub(crate) fn each_named_visible<'a>(
        &'a self,
        viewer: hir::FileId,
        units: &'a Units,
    ) -> impl Iterator<Item = (&'a str, &'a T)> {
        self.by_name.iter().flat_map(move |(name, declared)| {
            declared
                .iter()
                .filter(move |(unit, _)| units.sees(viewer, *unit))
                .map(move |(_, value)| (name.as_str(), value))
        })
    }

    /// The names the unit `viewer` sees, for suggestions.
    pub(crate) fn names_visible<'a>(
        &'a self,
        viewer: hir::FileId,
        units: &'a Units,
    ) -> impl Iterator<Item = &'a str> {
        self.by_name
            .iter()
            .filter(move |(_, declared)| declared.iter().any(|(unit, _)| units.sees(viewer, *unit)))
            .map(|(name, _)| name.as_str())
    }
}

/// The program's variables, each by its id, its first declaration's place
/// among them in source order, found by the name the units that see it
/// write.
pub(crate) struct Variables {
    units: Units,
    ids: Declared<hir::VariableId>,
    /// Each one's first declaration, and what it is called in the program,
    /// by id.
    declared: Vec<(VarDecl, String)>,
}

impl Variables {
    /// The variables `first` declares, each the first declaration of its
    /// name in its unit, in source order, as `namespaces` has them.
    pub(crate) fn new(first: impl Iterator<Item = VarDecl>, namespaces: &Namespaces) -> Self {
        let mut ids = Declared::default();
        let mut declared = Vec::new();
        for decl in first {
            let Some(ident) = decl.name().and_then(|n| n.ident()) else {
                continue;
            };
            let unit = namespaces.unit_at(trendy_syntax::span(ident.text_range()));
            ids.insert(ident.text(), unit, hir::VariableId(declared.len()));
            declared.push((decl, namespaces.value_identity(ident.text(), unit)));
        }
        Variables {
            units: namespaces.units().clone(),
            ids,
            declared,
        }
    }

    /// The variable named `name` that the unit `at` is in sees.
    pub(crate) fn find(&self, name: &str, at: Span) -> Option<hir::VariableId> {
        self.seen(name, self.units.at(at))
    }

    /// The variable named `name` that the unit `viewer` sees.
    pub(crate) fn seen(&self, name: &str, viewer: hir::FileId) -> Option<hir::VariableId> {
        self.ids
            .visible(name, viewer, &self.units)
            .map(|(_, &id)| id)
    }

    /// The first declaration of the variable `id`.
    pub(crate) fn decl(&self, id: hir::VariableId) -> &VarDecl {
        &self.declared[id.0].0
    }

    /// What the variable `id` is called in the program.
    pub(crate) fn identity(&self, id: hir::VariableId) -> &str {
        &self.declared[id.0].1
    }

    /// The names of the variables the unit `at` is in sees, for
    /// suggestions.
    pub(crate) fn names(&self, at: Span) -> impl Iterator<Item = &str> {
        self.ids.names_visible(self.units.at(at), &self.units)
    }

    /// How many there are.
    pub(crate) fn len(&self) -> usize {
        self.declared.len()
    }
}
