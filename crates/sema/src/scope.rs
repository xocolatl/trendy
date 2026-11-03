//! Which unit each declaration is in, which declarations each unit sees,
//! and what each is called in the program.
//!
//! A name written in a unit means a declaration it sees: one of its own,
//! one of the prelude's interface, which every unit sees, or one of the
//! interface of a unit its section uses, as the interface's `uses` are
//! what its declarations see, and the implementation's add to them. What a unit
//! uses never reaches the units that use it. A name that more than one of
//! those units declares, the using unit's own among them, is a clash where
//! it is written, which nothing settles by order, so it means none of them.
//!
//! A name declared in one unit of the program is called plainly everywhere,
//! and one declared in several, each a declaration of its own, is qualified
//! by its unit's name in every one of them, `lakes.depths::metres`, so that
//! every name the HIR has is one declaration's: what the program writes is
//! unambiguous, and lowering and the runtime can tell declarations apart by
//! name. No unit can declare one of the prelude's names, of either of its
//! sections, though every unit sees only its interface's, so those are
//! always plain.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, HashMap};

use trendy_diag::Span;
use trendy_hir as hir;
use trendy_syntax::ast::{AstNode, Section, SourceFile, VarDecl};

use crate::Namespaces;

/// The program's units, by their [`hir::FileId`].
#[derive(Clone)]
pub(crate) struct Units {
    units: Vec<Unit>,
    /// The prelude's.
    prelude: hir::FileId,
}

/// A unit, as scopes see it.
#[derive(Clone)]
struct Unit {
    name: String,
    /// Where its file's text ends in the program's.
    end: usize,
    /// Where each of its sections' headers starts, with the section it
    /// starts, in order: before the first, the implementation's.
    headers: Vec<(usize, Section)>,
    /// The units each of its sections uses, of the program's.
    uses: BTreeMap<Section, BTreeSet<hir::FileId>>,
}

/// Where a name is written: its unit, and the section of it, whose `uses`
/// say which other units' interfaces it sees.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct Viewer {
    pub(crate) unit: hir::FileId,
    pub(crate) section: Section,
}

impl Units {
    /// The units of `files`, each with its name, as the trees of one
    /// program, one after another, of which `prelude` is the prelude's.
    pub(crate) fn new(files: &[(&str, &SourceFile)], prelude: hir::FileId) -> Self {
        let ids: HashMap<&str, hir::FileId> = files
            .iter()
            .enumerate()
            .map(|(i, (name, _))| (*name, hir::FileId(i)))
            .collect();
        let units = files
            .iter()
            .map(|&(name, file)| {
                let mut uses: BTreeMap<Section, BTreeSet<hir::FileId>> = BTreeMap::new();
                for decl in file.uses() {
                    let known = decl
                        .units()
                        .filter_map(|u| ids.get(u.name()?.text().as_str()).copied());
                    uses.entry(decl.section()).or_default().extend(known);
                }
                Unit {
                    name: name.to_string(),
                    end: file.span().end,
                    headers: file
                        .headers()
                        .map(|h| (h.span().start, h.section()))
                        .collect(),
                    uses,
                }
            })
            .collect();
        Units { units, prelude }
    }

    /// The name of `unit`.
    pub(crate) fn name(&self, unit: hir::FileId) -> &str {
        &self.units[unit.0].name
    }

    /// The prelude's.
    pub(crate) fn prelude(&self) -> hir::FileId {
        self.prelude
    }

    /// The unit `span` is in.
    pub(crate) fn at(&self, span: Span) -> hir::FileId {
        hir::FileId(
            self.units
                .partition_point(|unit| unit.end <= span.start)
                .min(self.units.len() - 1),
        )
    }

    /// Who sees what is written at `span`: its unit, and the section it is
    /// in.
    pub(crate) fn viewer(&self, span: Span) -> Viewer {
        let unit = self.at(span);
        let section = self.units[unit.0]
            .headers
            .iter()
            .take_while(|(start, _)| *start <= span.start)
            .last()
            .map_or(Section::Implementation, |&(_, section)| section);
        Viewer { unit, section }
    }

    /// Whether `viewer` sees a declaration of `unit`'s, in its `section`:
    /// every one of its own unit's, and those of the interface of the
    /// prelude and of a unit its section uses, which, for the
    /// implementation, are its interface's too.
    pub(crate) fn sees(&self, viewer: Viewer, unit: hir::FileId, section: Section) -> bool {
        viewer.unit == unit
            || section == Section::Interface && (unit == self.prelude() || self.uses(viewer, unit))
    }

    /// The order in which `viewer` lists what the units `a` and `b` declare:
    /// its own unit's first, then the prelude's, then the used units', by
    /// their names, as they are in no order; one unit's in the order they
    /// are declared.
    pub(crate) fn seen_order(&self, viewer: Viewer, a: hir::FileId, b: hir::FileId) -> Ordering {
        let rank = |unit: hir::FileId| {
            if unit == viewer.unit {
                (0, "")
            } else if unit == self.prelude() {
                (1, "")
            } else {
                (2, self.name(unit))
            }
        };
        rank(a).cmp(&rank(b))
    }

    /// Whether `from` uses `to`, through any path of `uses`, in either
    /// section.
    pub(crate) fn reaches(&self, from: hir::FileId, to: hir::FileId) -> bool {
        let mut seen = BTreeSet::new();
        let mut pending = vec![from];
        while let Some(unit) = pending.pop() {
            for used in self.units[unit.0].uses.values().flatten() {
                if *used == to {
                    return true;
                }
                if seen.insert(*used) {
                    pending.push(*used);
                }
            }
        }
        false
    }

    /// Whether `viewer`'s section uses `unit`.
    fn uses(&self, viewer: Viewer, unit: hir::FileId) -> bool {
        let uses = &self.units[viewer.unit.0].uses;
        let of = |section| uses.get(&section).is_some_and(|used| used.contains(&unit));
        of(Section::Interface)
            || (viewer.section == Section::Implementation && of(Section::Implementation))
    }

    /// What the declaration named `name` in `unit` is called in the
    /// program: its name, or, if it is `ambiguous`, its name qualified by
    /// its unit's.
    pub(crate) fn identity(&self, name: &str, unit: hir::FileId, ambiguous: bool) -> String {
        if ambiguous {
            format!("{}::{name}", self.units[unit.0].name)
        } else {
            name.to_string()
        }
    }
}

/// What a name a viewer writes means, of the declarations it sees.
pub(crate) enum Seen<T> {
    /// None of them.
    Nothing,
    /// This one, in this unit.
    One(hir::FileId, T),
    /// The first of each unit's that declares it, the viewer's own first:
    /// a clash.
    Clash(Vec<(hir::FileId, T)>),
}

impl<T> Seen<T> {
    /// The declaration it is, unless it is none, or a clash.
    pub(crate) fn one(self) -> Option<(hir::FileId, T)> {
        match self {
            Seen::One(unit, value) => Some((unit, value)),
            Seen::Nothing | Seen::Clash(_) => None,
        }
    }
}

/// The declarations of a name a viewer sees, by where they come from.
pub(crate) struct Candidates<T> {
    /// The first of the viewer's own unit's.
    own: Option<(hir::FileId, T)>,
    /// The first of the prelude's.
    prelude: Option<(hir::FileId, T)>,
    /// The first of each used unit's, in its interface.
    used: Vec<(hir::FileId, T)>,
}

impl<T> Candidates<T> {
    /// Each declaration, of `f`.
    pub(crate) fn map<U>(self, f: impl Fn(T) -> U) -> Candidates<U> {
        Candidates {
            own: self.own.map(|(unit, value)| (unit, f(value))),
            prelude: self.prelude.map(|(unit, value)| (unit, f(value))),
            used: self
                .used
                .into_iter()
                .map(|(unit, value)| (unit, f(value)))
                .collect(),
        }
    }

    /// These and `other`'s, of another table of the same namespace, as
    /// types and headings are, and variables and databases.
    pub(crate) fn merged(mut self, other: Candidates<T>) -> Candidates<T> {
        self.own = self.own.or(other.own);
        self.prelude = self.prelude.or(other.prelude);
        for (unit, value) in other.used {
            if self.used.iter().all(|(u, _)| *u != unit) {
                self.used.push((unit, value));
            }
        }
        self
    }

    /// What the name means: the viewer's own, unless a used unit declares
    /// it too, a clash, or else the prelude's, which no unit can declare
    /// again, as a used unit that does is reported where it does, or else
    /// a used unit's, the only one, or a clash, or nothing.
    pub(crate) fn resolved(self) -> Seen<T> {
        let Candidates { own, prelude, used } = self;
        match (own, prelude) {
            (Some((unit, value)), prelude) if used.is_empty() || prelude.is_some() => {
                Seen::One(unit, value)
            }
            (Some(own), _) => Seen::Clash(std::iter::once(own).chain(used).collect()),
            (None, Some((unit, value))) => Seen::One(unit, value),
            (None, None) => {
                let mut used = used;
                match used.len() {
                    0 => Seen::Nothing,
                    1 => {
                        let (unit, value) = used.remove(0);
                        Seen::One(unit, value)
                    }
                    _ => Seen::Clash(used),
                }
            }
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

/// The declarations of one namespace, each by its name, with the unit and
/// the section it is in, in the order they are declared.
pub(crate) struct Declared<T> {
    by_name: BTreeMap<String, Vec<Entry<T>>>,
}

/// A declaration, in its unit's section.
struct Entry<T> {
    unit: hir::FileId,
    section: Section,
    value: T,
}

impl<T> Default for Declared<T> {
    fn default() -> Self {
        Declared {
            by_name: BTreeMap::new(),
        }
    }
}

impl<T> Declared<T> {
    /// Add `value`, the declaration named `name` in `unit`'s `section`.
    pub(crate) fn insert(&mut self, name: &str, unit: hir::FileId, section: Section, value: T) {
        self.by_name
            .entry(name.to_string())
            .or_default()
            .push(Entry {
                unit,
                section,
                value,
            });
    }

    fn entries(&self, name: &str) -> &[Entry<T>] {
        self.by_name.get(name).map_or(&[][..], Vec::as_slice)
    }

    /// The declarations named `name` that `viewer` sees, by where they come
    /// from.
    pub(crate) fn candidates(&self, name: &str, viewer: Viewer, units: &Units) -> Candidates<&T> {
        let entries = self.entries(name);
        let first = |unit: hir::FileId| {
            entries
                .iter()
                .find(|e| e.unit == unit)
                .map(|e| (e.unit, &e.value))
        };
        let prelude = entries
            .iter()
            .find(|e| e.unit == units.prelude() && units.sees(viewer, e.unit, e.section))
            .map(|e| (e.unit, &e.value));
        let mut used: Vec<(hir::FileId, &T)> = Vec::new();
        for e in entries {
            let other = e.unit != viewer.unit && e.unit != units.prelude();
            if other
                && units.sees(viewer, e.unit, e.section)
                && used.iter().all(|(u, _)| *u != e.unit)
            {
                used.push((e.unit, &e.value));
            }
        }
        // By name, as the units a section uses are in no order.
        used.sort_by(|(a, _), (b, _)| units.name(*a).cmp(units.name(*b)));
        Candidates {
            own: first(viewer.unit),
            prelude: prelude.filter(|_| viewer.unit != units.prelude()),
            used,
        }
    }

    /// Each declaration named `name` that `viewer` sees, with the unit it
    /// is in: its own unit's first, then the prelude's, then the used
    /// units', by their names, as they are in no order: an operator's
    /// overloads.
    pub(crate) fn each_visible<'a>(
        &'a self,
        name: &str,
        viewer: Viewer,
        units: &'a Units,
    ) -> impl Iterator<Item = (hir::FileId, &'a T)> {
        let mut seen: Vec<&Entry<T>> = self
            .entries(name)
            .iter()
            .filter(|e| units.sees(viewer, e.unit, e.section))
            .collect();
        seen.sort_by(|a, b| units.seen_order(viewer, a.unit, b.unit));
        seen.into_iter().map(|e| (e.unit, &e.value))
    }

    /// The first declaration named `name` in `unit`.
    pub(crate) fn first_in(&self, name: &str, unit: hir::FileId) -> Option<&T> {
        self.entries(name)
            .iter()
            .find(|e| e.unit == unit)
            .map(|e| &e.value)
    }

    /// Each declaration named `name` in `unit` or the prelude, `unit`'s
    /// first, with the unit it is in: those another of `unit`'s duplicates.
    pub(crate) fn each_own_or_prelude<'a>(
        &'a self,
        name: &str,
        unit: hir::FileId,
        units: &Units,
    ) -> impl Iterator<Item = (hir::FileId, &'a T)> {
        let prelude = units.prelude();
        let entries = self.entries(name);
        let own = entries.iter().filter(move |e| e.unit == unit);
        let preludes = entries
            .iter()
            .filter(move |e| e.unit == prelude && e.unit != unit);
        own.chain(preludes).map(|e| (e.unit, &e.value))
    }

    /// The units whose interfaces declare `name`, which `viewer` doesn't
    /// see, but whose `uses` its section could gain: none that uses the
    /// viewer's unit, through any path, as no unit uses itself.
    pub(crate) fn unseen_exporters(
        &self,
        name: &str,
        viewer: Viewer,
        units: &Units,
    ) -> BTreeSet<hir::FileId> {
        self.entries(name)
            .iter()
            .filter(|e| {
                e.section == Section::Interface
                    && e.unit != viewer.unit
                    && e.unit != units.prelude()
                    && !units.sees(viewer, e.unit, e.section)
                    && !units.reaches(e.unit, viewer.unit)
            })
            .map(|e| e.unit)
            .collect()
    }

    /// Each declaration named `name`, in any unit, with its unit.
    pub(crate) fn declaring<'a>(
        &'a self,
        name: &str,
    ) -> impl Iterator<Item = (hir::FileId, &'a T)> {
        self.entries(name).iter().map(|e| (e.unit, &e.value))
    }

    /// Remove the declarations named `name` in `unit`.
    pub(crate) fn remove_in(&mut self, name: &str, unit: hir::FileId) {
        if let Some(declared) = self.by_name.get_mut(name) {
            declared.retain(|e| e.unit != unit);
        }
    }

    /// Each declaration `viewer` sees, with its name.
    pub(crate) fn each_named_visible<'a>(
        &'a self,
        viewer: Viewer,
        units: &'a Units,
    ) -> impl Iterator<Item = (&'a str, &'a T)> {
        self.by_name.iter().flat_map(move |(name, declared)| {
            declared
                .iter()
                .filter(move |e| units.sees(viewer, e.unit, e.section))
                .map(move |e| (name.as_str(), &e.value))
        })
    }

    /// The names `viewer` sees, for suggestions.
    pub(crate) fn names_visible<'a>(
        &'a self,
        viewer: Viewer,
        units: &'a Units,
    ) -> impl Iterator<Item = &'a str> {
        self.by_name
            .iter()
            .filter(move |(_, declared)| {
                declared
                    .iter()
                    .any(|e| units.sees(viewer, e.unit, e.section))
            })
            .map(|(name, _)| name.as_str())
    }
}

/// The program's variables, each by its id, its first declaration's place
/// among them in source order, found by the name the units that see it
/// write, in the namespace they share with databases.
pub(crate) struct Variables {
    units: Units,
    ids: Declared<hir::VariableId>,
    /// The databases, by name, whose names a unit can't give a variable it
    /// sees, and the reverse.
    databases: Declared<()>,
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
            let section = trendy_syntax::ast::section_of(decl.syntax());
            ids.insert(ident.text(), unit, section, hir::VariableId(declared.len()));
            declared.push((decl, namespaces.value_identity(ident.text(), unit)));
        }
        let mut databases = Declared::default();
        for (name, entries) in &namespaces.databases.by_name {
            for e in entries {
                databases.insert(name, e.unit, e.section, ());
            }
        }
        Variables {
            units: namespaces.units().clone(),
            ids,
            databases,
            declared,
        }
    }

    /// The variable named `name` that what is written at `at` sees, unless
    /// it sees a database of that name, or several of either.
    pub(crate) fn find(&self, name: &str, at: Span) -> Option<hir::VariableId> {
        let viewer = self.units.viewer(at);
        let variables = self.ids.candidates(name, viewer, &self.units).map(Some);
        let databases = self
            .databases
            .candidates(name, viewer, &self.units)
            .map(|&()| None);
        variables
            .merged(databases)
            .resolved()
            .one()
            .and_then(|(_, id)| id.copied())
    }

    /// The first declaration of the variable `id`.
    pub(crate) fn decl(&self, id: hir::VariableId) -> &VarDecl {
        &self.declared[id.0].0
    }

    /// What the variable `id` is called in the program.
    pub(crate) fn identity(&self, id: hir::VariableId) -> &str {
        &self.declared[id.0].1
    }

    /// The names of the variables what is written at `at` sees, for
    /// suggestions.
    pub(crate) fn names(&self, at: Span) -> impl Iterator<Item = &str> {
        self.ids.names_visible(self.units.viewer(at), &self.units)
    }

    /// How many there are.
    pub(crate) fn len(&self) -> usize {
        self.declared.len()
    }
}
