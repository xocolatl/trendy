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
use std::path::{Path, PathBuf};

use trendy_diag::Diagnostic;
use trendy_parser::Parse;
use trendy_syntax::unit;

/// A unit of the program, its file read and parsed.
pub struct Unit {
    /// Its name: its path from the program's root (see [`program`]).
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
/// Main's unit is named by its path from their root: of a file with `main`, its
/// directory, and of one without, the nearest directory above it where the units it
/// uses are.
#[must_use]
pub fn program(main: &Path, source: String) -> Program {
    let stem = main.file_stem().map_or_else(
        || main.display().to_string(),
        |stem| stem.to_string_lossy().into_owned(),
    );
    let mut first = parsed(stem, main.display().to_string(), source);
    let (root, name) = rooted(main, &first);
    let root = root.as_path();
    first.name = name;
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

/// Where the units `unit`, the file at `path`, uses are named from, and
/// its name from there. A unit with `main` is a program's main unit, whose
/// root is the directory of its file. One without is a unit of some program
/// above it, whose path from that program's root names it, so its root is the
/// nearest directory above it, as far as its path can name it, where the
/// most of the units it uses have files; or, if none has, its own.
fn rooted(path: &Path, unit: &Unit) -> (PathBuf, String) {
    let own = path.parent().unwrap_or(Path::new("")).to_path_buf();
    let uses: Vec<String> = used(unit)
        .into_iter()
        .filter(|name| !name.starts_with(unit::COMPILERS))
        .collect();
    let found = |root: &Path| {
        uses.iter()
            .filter(|name| root.join(unit::path_of(name)).is_file())
            .count()
    };
    let mut best = (own.clone(), unit.name.clone(), found(&own));
    if unit.parse.tree.declares_main() || !trendy_syntax::is_word(&unit.name) {
        return (best.0, best.1);
    }
    let (mut at, mut name) = match std::path::absolute(&own) {
        Ok(absolute) => (absolute, unit.name.clone()),
        Err(_) => return (best.0, best.1),
    };
    while best.2 < uses.len() {
        let Some(directory) = at.file_name().and_then(|d| d.to_str()) else {
            break;
        };
        if !trendy_syntax::is_word(directory) {
            break;
        }
        name = format!("{directory}.{name}");
        at.pop();
        let count = found(&at);
        if count > best.2 {
            best = (at.clone(), name.clone(), count);
        }
    }
    (best.0, best.1)
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

    /// The names of the program of the file at `path`, of the text `source`.
    fn names_of(path: &Path, source: &str) -> Vec<String> {
        program(path, source.to_string())
            .units
            .into_iter()
            .map(|unit| unit.name)
            .collect()
    }

    #[test]
    fn a_unit_without_main_is_rooted_where_the_units_it_uses_are() {
        let names = "interface\n\nheading name := { n: text };";
        let root = with_files(
            "rooted",
            &[
                ("lakes/names.trendy", names),
                ("lakes/deep/more.trendy", names),
                ("rivers.trendy", names),
            ],
        );
        // Above it, named from there, its text as given, not its file's.
        let depths = root.join("lakes/depths.trendy");
        let uses = "uses { lakes.names };\n";
        assert_eq!(names_of(&depths, uses), ["lakes.depths", "lakes.names"]);
        let loaded = program(&depths, uses.to_string());
        assert_eq!(loaded.units[0].path, depths.display().to_string());
        assert_eq!(loaded.units[0].source, uses);
        // Further above.
        let more = root.join("lakes/deep/more.trendy");
        assert_eq!(
            names_of(&more, "uses { rivers };\n"),
            ["lakes.deep.more", "rivers"]
        );
        // Its own directory first, if they are there.
        assert_eq!(names_of(&depths, "uses { names };\n"), ["depths", "names"]);
        // Those that come with the compiler are no file of any directory.
        assert_eq!(
            names_of(&depths, "uses { trendy.files, lakes.names };\n"),
            ["lakes.depths", "lakes.names"]
        );
        assert_eq!(names_of(&depths, "uses { trendy.files };\n"), ["depths"]);
        // The most of them, of those that have files anywhere, so that the
        // error is of the one misspelled.
        assert_eq!(
            names_of(&depths, "uses { lakes.nemes, lakes.names };\n"),
            ["lakes.depths", "lakes.names"]
        );
        // None anywhere, or none used: its own.
        assert_eq!(names_of(&depths, "uses { nowhere };\n"), ["depths"]);
        assert_eq!(names_of(&depths, "interface\n"), ["depths"]);
        // A program's root is the directory of its file, as the compiler has it,
        // so a unit above it isn't used.
        let main = "uses { lakes.names };\n\noperator main{} [];";
        assert_eq!(names_of(&depths, main), ["depths"]);
    }

    #[test]
    fn the_root_with_the_most_of_them_is_chosen_and_the_nearest_of_those() {
        let unit = "interface\n";
        let root = with_files(
            "rooted-most",
            &[
                ("a/near.trendy", unit),
                ("near.trendy", unit),
                ("far.trendy", unit),
                ("trendy/files.trendy", unit),
            ],
        );
        let path = root.join("a/b/unit.trendy");
        // All of them only further up, past a directory with one.
        assert_eq!(
            names_of(&path, "uses { near, far };\n"),
            ["a.b.unit", "far", "near"]
        );
        // As many in either, of those there are anywhere: the nearest.
        assert_eq!(
            names_of(&path, "uses { near, nowhere };\n"),
            ["b.unit", "near"]
        );
        // What comes with the compiler is no file, even of a directory that has one.
        assert_eq!(names_of(&path, "uses { trendy.files };\n"), ["unit"]);
    }

    #[test]
    fn a_path_that_cant_name_a_unit_ends_the_search() {
        let root = with_files(
            "rooted-words",
            &[
                ("names.trendy", "interface\n"),
                ("not-a-word/deep/names.trendy", "interface\n"),
            ],
        );
        // `not-a-word` can be a root, but no part of a name, so `names.trendy`
        // above it isn't reached.
        let unit = root.join("not-a-word/unit.trendy");
        assert_eq!(names_of(&unit, "uses { names };\n"), ["unit"]);
        let unit = root.join("not-a-word/deep/unit.trendy");
        assert_eq!(
            names_of(&unit, "uses { deep.names };\n"),
            ["deep.unit", "deep.names"]
        );
        // A stem that can't be one has no name from above.
        let unit = root.join("not-a-word/deep/2015-01.trendy");
        assert_eq!(names_of(&unit, "uses { deep.names };\n"), ["2015-01"]);
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
        assert_eq!(
            names,
            [
                "trendy.process",
                "trendy.time",
                "trendy.uuid",
                "trendy.files",
                "trendy.catalog",
                "trendy.prelude"
            ]
        );
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
