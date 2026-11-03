//! A program's units, loaded from their files: main's, and each unit it
//! uses, through any path, each at its path from the directory of `main`'s
//! file, as a unit is named by its path from there (`lakes.depths` is
//! `lakes/depths.trendy`). The names under `trendy.` are those of the units
//! that come with the compiler, which are no files of the program, but the
//! compiler's own text ([`library`]), which every program has. A unit
//! whose file there is none of isn't loaded: sema reports each `uses` of
//! it, with the file it would be. A unit that declares a database brings
//! each unit under its directory, used or not, as they are the database's
//! own, which add to it, and every program that uses the database has all
//! of its constraints.

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::Path;

use trendy_diag::Diagnostic;
use trendy_parser::Parse;
use trendy_syntax::unit;

/// A unit of the program, its file read and parsed.
pub struct Unit {
    /// Its name: its path from the directory of `main`'s file.
    pub name: String,
    /// Where its file is, as errors and failures write it.
    pub path: String,
    pub source: String,
    /// Its tree, and its lexer's and parser's errors, in its own text.
    pub parse: Parse,
}

/// A program's units, as sema checks them.
pub struct Program {
    /// Main's unit first, then those it uses, through any path, by their
    /// names, as they are in no order.
    pub units: Vec<Unit>,
    /// Each file of a unit used that is there but couldn't be read, with
    /// why.
    pub unreadable: Vec<(String, io::Error)>,
}

impl Program {
    /// Each unit's lexer's and parser's errors, in the spans of the
    /// program, whose units' texts are one after another.
    #[must_use]
    pub fn errors(&self) -> Vec<Diagnostic> {
        let mut start = 0;
        let mut errors = Vec::new();
        for unit in &self.units {
            errors.extend(unit.parse.errors.iter().cloned().map(|e| e.shifted(start)));
            start += unit.source.len();
        }
        errors
    }

    /// Each unit's path and text, in order, as `trendy_diag::Sources`
    /// takes them.
    pub fn sources(&self) -> impl Iterator<Item = (&str, &str)> {
        self.units
            .iter()
            .map(|unit| (unit.path.as_str(), unit.source.as_str()))
    }
}

/// The program whose main unit is the file at `main`, of the text
/// `source`, with each unit it uses, through any path, whose file there
/// is, and each under the directory of a unit that declares a database.
/// Main's unit is named by its file's stem.
#[must_use]
pub fn program(main: &Path, source: String) -> Program {
    let root = main.parent().unwrap_or(Path::new(""));
    let name = main.file_stem().map_or_else(
        || main.display().to_string(),
        |stem| stem.to_string_lossy().into_owned(),
    );
    let first = parsed(name, main.display().to_string(), source);
    let mut seen: BTreeSet<String> = BTreeSet::from([first.name.clone()]);
    let mut loaded: BTreeMap<String, Unit> = BTreeMap::new();
    let mut unreadable = Vec::new();
    let mut pending = used(&first);
    if declares_database(&first) {
        pending.extend(under(root, &first.name, &mut unreadable));
    }
    while let Some(name) = pending.pop() {
        if name.starts_with(unit::COMPILERS) || !seen.insert(name.clone()) {
            continue;
        }
        let path = root.join(unit::path_of(&name));
        match std::fs::read_to_string(&path) {
            Ok(source) => {
                let unit = parsed(name.clone(), path.display().to_string(), source);
                pending.extend(used(&unit));
                if declares_database(&unit) {
                    pending.extend(under(root, &name, &mut unreadable));
                }
                loaded.insert(name, unit);
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => unreadable.push((path.display().to_string(), e)),
        }
    }
    let mut units = vec![first];
    units.extend(loaded.into_values());
    Program { units, unreadable }
}

/// The program whose main unit is at `main`, of the text `source`, alone,
/// without the units it uses: for a text in no file, which has no
/// directory to find them in.
#[must_use]
pub fn alone(main: &Path, source: String) -> Program {
    let name = main.file_stem().map_or_else(
        || main.display().to_string(),
        |stem| stem.to_string_lossy().into_owned(),
    );
    Program {
        units: vec![parsed(name, main.display().to_string(), source)],
        unreadable: Vec::new(),
    }
}

/// The units that come with the compiler, as `trendy_prelude::UNITS`
/// lists them, each parsed: every program has them, and sema checks them
/// with its own units, their texts after the units' in the program's
/// spans. They are part of the compiler, so an error in one is the
/// compiler's.
#[must_use]
pub fn library() -> Vec<Unit> {
    trendy_prelude::UNITS
        .iter()
        .map(|unit| {
            parsed(
                unit.name.to_string(),
                unit.path.to_string(),
                unit.source.to_string(),
            )
        })
        .collect()
}

/// The unit `name`, of the file at `path`, of the text `source`, parsed.
fn parsed(name: String, path: String, source: String) -> Unit {
    let parse = trendy_parser::parse(&source, trendy_lexer::lex(&source));
    Unit {
        name,
        path,
        source,
        parse,
    }
}

/// Whether `unit` declares a database.
fn declares_database(unit: &Unit) -> bool {
    unit.parse.tree.databases().next().is_some()
}

/// The names of the units under the directory of the unit `owner`, from
/// `root`: of each file `*.trendy` there, through any depth, whose path's
/// parts are words, as `library/loans.trendy` is `library.loans`. A
/// directory there is that can't be read is added to `unreadable`.
fn under(root: &Path, owner: &str, unreadable: &mut Vec<(String, io::Error)>) -> Vec<String> {
    let path = unit::path_of(owner);
    let directory = root.join(path.strip_suffix(".trendy").unwrap_or(&path));
    let mut names = Vec::new();
    let mut pending = vec![directory.clone()];
    while let Some(at) = pending.pop() {
        let entries = match std::fs::read_dir(&at) {
            Ok(entries) => entries,
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            Err(e) => {
                unreadable.push((at.display().to_string(), e));
                continue;
            }
        };
        for path in entries.flatten().map(|entry| entry.path()) {
            if path.is_dir() {
                pending.push(path);
            } else if path.extension().is_some_and(|e| e == "trendy") {
                let within = path.strip_prefix(&directory).ok().and_then(Path::to_str);
                if let Some(name) = within.and_then(unit::named_by) {
                    names.push(format!("{owner}.{name}"));
                }
            }
        }
    }
    names
}

/// The names of the units `unit` uses, in either section.
fn used(unit: &Unit) -> Vec<String> {
    unit.parse
        .tree
        .uses()
        .flat_map(|uses| {
            uses.units()
                .filter_map(|u| u.name())
                .map(|n| n.text())
                .collect::<Vec<_>>()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A directory of its own, under the target directory, with `files`,
    /// each a path from it and a text.
    fn with_files(test: &str, files: &[(&str, &str)]) -> std::path::PathBuf {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/load-tests")
            .join(test);
        let _ = std::fs::remove_dir_all(&root);
        for (path, text) in files {
            let path = root.join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        }
        root
    }

    #[test]
    fn a_program_is_its_main_unit_and_those_it_uses() {
        let root = with_files(
            "uses",
            &[
                (
                    "lakes/depths.trendy",
                    "uses { lakes.names, trendy.catalog };",
                ),
                ("lakes/names.trendy", "uses { lakes.depths };"),
                ("unused.trendy", ""),
            ],
        );
        let main = "uses { lakes.depths, missing };\n\noperator main{} [];";
        let loaded = program(&root.join("main.trendy"), main.to_string());
        let names: Vec<&str> = loaded.units.iter().map(|u| u.name.as_str()).collect();
        assert_eq!(names, ["main", "lakes.depths", "lakes.names"]);
        assert!(loaded.unreadable.is_empty());
        assert!(loaded.units[1].path.ends_with("lakes/depths.trendy"));
        // A database's own units, used or not.
        let root = with_files(
            "databases",
            &[
                ("library.trendy", "interface\n\ndatabase library;"),
                ("library/loans.trendy", "uses { library };"),
                ("library/loans/late.trendy", ""),
                ("library/notes.txt", ""),
            ],
        );
        let loaded = program(&root.join("main.trendy"), "uses { library };".to_string());
        let names: Vec<&str> = loaded.units.iter().map(|u| u.name.as_str()).collect();
        assert_eq!(
            names,
            ["main", "library", "library.loans", "library.loans.late"]
        );
        // Parse errors, in the program's spans.
        let root = with_files("errors", &[("broken.trendy", "var x := ;")]);
        let main = "uses { broken };";
        let loaded = program(&root.join("main.trendy"), main.to_string());
        let errors = loaded.errors();
        assert_eq!(errors.len(), 1, "{errors:#?}");
        assert!(errors[0].span().start >= main.len());
    }

    #[test]
    fn the_units_that_come_with_the_compiler_are_its_own_text() {
        let library = library();
        let names: Vec<&str> = library.iter().map(|u| u.name.as_str()).collect();
        assert_eq!(names, ["trendy.files", "trendy.catalog", "trendy.prelude"]);
        for unit in &library {
            assert!(
                unit.parse.errors.is_empty(),
                "{}: {:#?}",
                unit.name,
                unit.parse.errors
            );
        }
        // Never a program's files, though a unit uses them.
        let root = with_files("compilers", &[("trendy/files.trendy", "")]);
        let main = "uses { trendy.files };";
        let loaded = program(&root.join("main.trendy"), main.to_string());
        assert_eq!(loaded.units.len(), 1);
    }
}
