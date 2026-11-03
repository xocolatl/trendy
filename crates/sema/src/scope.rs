//! Which unit each declaration is in, which declarations each unit sees,
//! and what each is called in the program.
//!
//! A name written in a unit means a declaration it sees: one of its own,
//! which hides every other unit's, so that a unit never qualifies its own
//! names, or else one of the prelude's interface, which every unit sees,
//! or of the interface of a unit its section uses, as the interface's
//! `uses` are what its declarations see, and the implementation's add to
//! them. What a unit uses never reaches the units that use it. A name
//! that more than one of those other units declares is a clash where it is
//! written, which nothing settles by order, so it means none of them.
//!
//! A name declared in one unit of the program is called plainly everywhere,
//! and one declared in several, each a declaration of its own, is qualified
//! by its unit's name in every one of them, the prelude's too,
//! `lakes.depths::metres` and `trendy.prelude::count`, so that every name
//! the HIR has is one declaration's: what the program writes is
//! unambiguous, and lowering and the runtime can tell declarations apart by
//! name.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, HashMap};

use trendy_diag::Span;
use trendy_hir as hir;
use trendy_syntax::ast::{AstNode, Section, SourceFile, VarDecl};

use crate::Namespaces;

/// The program's units, by their [`hir::FileId`].
#[derive(Clone)]
pub(crate) struct Units {
    all: Vec<Unit>,
    /// The prelude's.
    prelude: hir::FileId,
    /// Each unit's, by its name.
    ids: HashMap<String, hir::FileId>,
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
    /// The units each of its sections uses, by the name each is used by
    /// there: the one its `uses` gives it after `as`, or else its own.
    named: BTreeMap<Section, BTreeMap<String, hir::FileId>>,
}

/// The unit a qualifier names, by the name written in it, of those a
/// viewer sees (see [`Units::qualified`]).
pub(crate) enum Qualified {
    /// A unit it sees: the prelude, or one its section uses, by the name
    /// it uses it by.
    Seen(hir::FileId),
    /// Its own unit, whose names are never qualified.
    Own(hir::FileId),
    /// A unit its section uses by another name, this one.
    Aliased(hir::FileId, String),
    /// A unit of the program its section doesn't use.
    Unused(hir::FileId),
    /// No unit of the program.
    Unknown,
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
        let ids: HashMap<String, hir::FileId> = files
            .iter()
            .enumerate()
            .map(|(i, (name, _))| ((*name).to_string(), hir::FileId(i)))
            .collect();
        let units = files
            .iter()
            .map(|&(name, file)| {
                let mut uses: BTreeMap<Section, BTreeSet<hir::FileId>> = BTreeMap::new();
                let mut named: BTreeMap<Section, BTreeMap<String, hir::FileId>> = BTreeMap::new();
                for decl in file.uses() {
                    for used in decl.units() {
                        let Some(&unit) = used.name().and_then(|n| ids.get(&n.text())) else {
                            continue;
                        };
                        let by = match used.alias().and_then(|a| a.ident()) {
                            Some(alias) => alias.text().to_string(),
                            None => used.name().map(|n| n.text()).unwrap_or_default(),
                        };
                        uses.entry(decl.section()).or_default().insert(unit);
                        named.entry(decl.section()).or_default().insert(by, unit);
                    }
                }
                Unit {
                    name: name.to_string(),
                    end: file.span().end,
                    headers: file
                        .headers()
                        .map(|h| (h.span().start, h.section()))
                        .collect(),
                    uses,
                    named,
                }
            })
            .collect();
        Units {
            all: units,
            prelude,
            ids,
        }
    }

    /// The unit a qualifier written `name`, at `viewer`, names, as
    /// [`Qualified`] says: by the name its section uses it by, or the
    /// prelude by its own, unless its section uses it by another.
    pub(crate) fn qualified(&self, viewer: Viewer, name: &str) -> Qualified {
        let named = &self.all[viewer.unit.0].named;
        let sections: &[Section] = match viewer.section {
            Section::Interface => &[Section::Interface],
            Section::Implementation => &[Section::Interface, Section::Implementation],
        };
        let used = sections
            .iter()
            .filter_map(|section| named.get(section))
            .flatten();
        let mut aliased = None;
        for (by, &unit) in used {
            if by == name {
                return Qualified::Seen(unit);
            }
            if self.name(unit) == name {
                aliased = Some((unit, by.clone()));
            }
        }
        if let Some((unit, by)) = aliased {
            return Qualified::Aliased(unit, by);
        }
        match self.ids.get(name).copied() {
            Some(unit) if unit == viewer.unit => Qualified::Own(unit),
            Some(unit) if unit == self.prelude => Qualified::Seen(unit),
            Some(unit) => Qualified::Unused(unit),
            None => Qualified::Unknown,
        }
    }

    /// The name a qualifier written at `viewer` names `unit` by: the one its
    /// section uses it by, or else its own.
    pub(crate) fn used_as(&self, viewer: Viewer, unit: hir::FileId) -> &str {
        let named = &self.all[viewer.unit.0].named;
        named
            .iter()
            .filter(|(section, _)| {
                **section == Section::Interface || viewer.section == Section::Implementation
            })
            .flat_map(|(_, used)| used.iter())
            .find(|&(_, &u)| u == unit)
            .map_or_else(|| self.name(unit), |(by, _)| by.as_str())
    }

    /// The names a qualifier written at `viewer` can mean, for a
    /// misspelling of one: those its section uses units by, and the
    /// program's units', those it doesn't use among them.
    pub(crate) fn qualifiers(&self, viewer: Viewer) -> Vec<&str> {
        let used_by = &self.all[viewer.unit.0].named;
        let mut names: Vec<&str> = used_by
            .iter()
            .filter(|(section, _)| {
                **section == Section::Interface || viewer.section == Section::Implementation
            })
            .flat_map(|(_, used)| used.keys().map(String::as_str))
            .chain(self.ids.keys().map(String::as_str))
            .collect();
        names.sort_unstable();
        names.dedup();
        names
    }

    /// The name of `unit`.
    pub(crate) fn name(&self, unit: hir::FileId) -> &str {
        &self.all[unit.0].name
    }

    /// The prelude's.
    pub(crate) fn prelude(&self) -> hir::FileId {
        self.prelude
    }

    /// The unit `span` is in.
    pub(crate) fn at(&self, span: Span) -> hir::FileId {
        hir::FileId(
            self.all
                .partition_point(|unit| unit.end <= span.start)
                .min(self.all.len() - 1),
        )
    }

    /// Who sees what is written at `span`: its unit, and the section it is
    /// in.
    pub(crate) fn viewer(&self, span: Span) -> Viewer {
        let unit = self.at(span);
        let section = self.all[unit.0]
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
            for used in self.all[unit.0].uses.values().flatten() {
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
        let uses = &self.all[viewer.unit.0].uses;
        let of = |section| uses.get(&section).is_some_and(|used| used.contains(&unit));
        of(Section::Interface)
            || (viewer.section == Section::Implementation && of(Section::Implementation))
    }

    /// What the declaration named `name` in `unit` is called in the
    /// program: its name, or, if it is `ambiguous`, its name qualified by
    /// its unit's.
    pub(crate) fn identity(&self, name: &str, unit: hir::FileId, ambiguous: bool) -> String {
        if ambiguous {
            format!("{}::{name}", self.all[unit.0].name)
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
    /// The first of each other unit's whose interface it sees, the
    /// prelude's and each used unit's, by their units' names.
    used: Vec<(hir::FileId, T)>,
}

impl<T> Candidates<T> {
    /// Each declaration, of `f`.
    pub(crate) fn map<U>(self, f: impl Fn(T) -> U) -> Candidates<U> {
        Candidates {
            own: self.own.map(|(unit, value)| (unit, f(value))),
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
        for (unit, value) in other.used {
            if self.used.iter().all(|(u, _)| *u != unit) {
                self.used.push((unit, value));
            }
        }
        self
    }

    /// What the name means: the viewer's own, which hides the other
    /// units', so that a unit never qualifies its own names, or else the
    /// only other unit's, or else, of several, a clash, which nothing
    /// settles by order, or nothing.
    pub(crate) fn resolved(self) -> Seen<T> {
        let Candidates { own, mut used } = self;
        if let Some((unit, value)) = own {
            return Seen::One(unit, value);
        }
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

/// Whether a name is ambiguous, declared in each of `declaring`: in more
/// than one unit, the prelude among them.
pub(crate) fn ambiguous(declaring: impl Iterator<Item = hir::FileId>) -> bool {
    let mut first = None;
    let mut several = false;
    for unit in declaring {
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
        let mut used: Vec<(hir::FileId, &T)> = Vec::new();
        for e in entries {
            if e.unit != viewer.unit
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

    /// The first declaration named `name` in `unit` that `viewer` sees: in
    /// its interface, unless it is the viewer's own, for a qualified name.
    pub(crate) fn seen_in(
        &self,
        name: &str,
        unit: hir::FileId,
        viewer: Viewer,
        units: &Units,
    ) -> Option<&T> {
        self.entries(name)
            .iter()
            .find(|e| e.unit == unit && units.sees(viewer, e.unit, e.section))
            .map(|e| &e.value)
    }

    /// Each name `unit` declares that `viewer` sees, for misspellings of a
    /// name it qualifies by that unit.
    pub(crate) fn names_in<'a>(
        &'a self,
        unit: hir::FileId,
        viewer: Viewer,
        units: &'a Units,
    ) -> impl Iterator<Item = &'a str> {
        self.by_name
            .iter()
            .filter(move |(_, entries)| {
                entries
                    .iter()
                    .any(|e| e.unit == unit && units.sees(viewer, e.unit, e.section))
            })
            .map(|(name, _)| name.as_str())
    }

    /// Whether `unit` declares `name` in a section `viewer` doesn't see:
    /// its implementation, which other units don't.
    pub(crate) fn hidden_in(
        &self,
        name: &str,
        unit: hir::FileId,
        viewer: Viewer,
        units: &Units,
    ) -> bool {
        self.entries(name)
            .iter()
            .any(|e| e.unit == unit && !units.sees(viewer, e.unit, e.section))
    }

    /// The first declaration named `name` in `unit`.
    pub(crate) fn first_in(&self, name: &str, unit: hir::FileId) -> Option<&T> {
        self.entries(name)
            .iter()
            .find(|e| e.unit == unit)
            .map(|e| &e.value)
    }

    /// Each declaration named `name` in `unit`, in the order they are
    /// declared: those another of `unit`'s duplicates, as another unit's
    /// of that name are only hidden by it.
    pub(crate) fn each_in<'a>(
        &'a self,
        name: &str,
        unit: hir::FileId,
    ) -> impl Iterator<Item = &'a T> {
        self.entries(name)
            .iter()
            .filter(move |e| e.unit == unit)
            .map(|e| &e.value)
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
