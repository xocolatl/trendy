//! The examples in every `trendyc --explain` page, and in
//! `docs/language.md`, must behave as documented: a ```` ```trendy ```` block
//! compiles cleanly, and a ```` ```trendy,error ```` block doesn't (in an
//! explanation, it must produce that page's code). Where every error in a
//! failing explanation example carries a fix, applying the fixes must resolve
//! that code. A ```` ```trendy,unit lakes.depths ```` block is another
//! unit's file, `lakes/depths.trendy`, which each example after it is
//! compiled with, as the program's main unit, until another block of that
//! unit's.

use trendy_diag::{Code, Diagnostic};

/// Every error in `source`, as the compiler would report it, as the main
/// unit of a program with `units`, each a unit's name and its file's text.
fn errors(source: &str, units: &[(String, String)]) -> Vec<Diagnostic> {
    let parse = trendy_parser::parse(source, trendy_lexer::lex(source));
    if !parse.errors.is_empty() {
        return parse.errors;
    }
    let parsed: Vec<_> = units
        .iter()
        .map(|(name, text)| {
            let parse = trendy_parser::parse(text, trendy_lexer::lex(text));
            assert!(parse.errors.is_empty(), "unit {name}: {:#?}", parse.errors);
            (
                name,
                format!("{}.trendy", name.replace('.', "/")),
                parse.tree,
            )
        })
        .collect();
    let mut files = vec![trendy_sema::File {
        name: "program",
        path: "program.trendy",
        tree: &parse.tree,
    }];
    files.extend(
        parsed
            .iter()
            .map(|(name, path, tree)| trendy_sema::File { name, path, tree }),
    );
    let library = trendy_load::library();
    let library: Vec<trendy_sema::File> = library
        .iter()
        .map(|unit| trendy_sema::File {
            name: &unit.name,
            path: &unit.path,
            tree: &unit.parse.tree,
        })
        .collect();
    trendy_sema::analyze(&files, &library)
        .err()
        .unwrap_or_default()
}

/// The unit a ```` ```trendy,unit name ```` block is the file of.
fn unit_of(info: &str) -> Option<&str> {
    info.strip_prefix("trendy,unit ")
}

/// Apply each error's first suggestion, or `None` if some error has none.
fn apply_fixes(source: &str, errors: &[Diagnostic]) -> Option<String> {
    let mut edits = Vec::new();
    for error in errors {
        edits.extend(error.suggestions.first()?.edits.iter().cloned());
    }
    edits.sort_by_key(|e| std::cmp::Reverse(e.span.start));
    let mut fixed = source.to_string();
    for edit in edits {
        fixed.replace_range(edit.span.start..edit.span.end, &edit.replacement);
    }
    Some(fixed)
}

/// The fenced code blocks in `markdown`, as (info string, contents).
fn code_blocks(markdown: &str) -> Vec<(&str, String)> {
    let mut blocks = Vec::new();
    let mut lines = markdown.lines();
    while let Some(line) = lines.next() {
        let Some(info) = line.strip_prefix("```") else {
            continue;
        };
        let body: Vec<&str> = lines.by_ref().take_while(|l| *l != "```").collect();
        blocks.push((info, body.join("\n") + "\n"));
    }
    blocks
}

#[test]
fn explanation_examples_behave_as_documented() {
    for code in Code::ALL {
        let blocks = code_blocks(code.explanation());
        // A code the compiler never reports, such as running out of memory,
        // can't have a program that produces it.
        if code.reported_when_compiling() {
            assert!(
                blocks.iter().any(|(info, _)| *info == "trendy,error"),
                "{code} has no failing example"
            );
            assert!(
                blocks.iter().any(|(info, _)| *info == "trendy"),
                "{code} has no corrected example"
            );
        } else {
            assert!(
                blocks.iter().all(|(info, _)| *info != "trendy,error"),
                "{code} is never reported when compiling, so it can't have a failing example"
            );
        }

        let mut units = Vec::new();
        for (info, source) in blocks {
            if let Some(unit) = unit_of(info) {
                units.retain(|(name, _)| name != unit);
                units.push((unit.to_string(), source));
                continue;
            }
            let errors = errors(&source, &units);
            match info {
                "trendy" => assert!(
                    errors.is_empty(),
                    "{code}: corrected example has errors:\n{source}\n{errors:#?}"
                ),
                "trendy,error" => {
                    assert!(
                        errors.iter().any(|e| e.code == code),
                        "{code}: failing example doesn't produce {code}:\n{source}\n{errors:#?}"
                    );
                    if let Some(fixed) = apply_fixes(&source, &errors) {
                        let remaining = self::errors(&fixed, &units);
                        // Fixing a wrong keyword can uncover the next mistake,
                        // but never the same one again.
                        assert!(
                            remaining.iter().all(|e| e.code != code),
                            "{code}: fixes didn't resolve it:\n{fixed}\n{remaining:#?}"
                        );
                    }
                }
                other => panic!("{code}: unknown code block `{other}`"),
            }
        }
    }
}

#[test]
fn language_reference_examples_behave_as_documented() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/language.md");
    let reference = std::fs::read_to_string(path).unwrap();
    let blocks: Vec<_> = code_blocks(&reference)
        .into_iter()
        .filter(|(info, _)| info.starts_with("trendy"))
        .collect();
    assert!(!blocks.is_empty());
    let mut units = Vec::new();
    for (info, source) in blocks {
        if let Some(unit) = unit_of(info) {
            units.retain(|(name, _)| name != unit);
            units.push((unit.to_string(), source));
            continue;
        }
        let errors = errors(&source, &units);
        match info {
            "trendy" => assert!(
                errors.is_empty(),
                "docs/language.md example has errors:\n{source}\n{errors:#?}"
            ),
            "trendy,error" => assert!(
                !errors.is_empty(),
                "docs/language.md failing example compiles:\n{source}"
            ),
            other => panic!("docs/language.md: unknown code block `{other}`"),
        }
    }
}

#[test]
fn language_reference_shows_the_units_that_come_with_the_compiler_as_they_are() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/language.md");
    let reference = std::fs::read_to_string(path).unwrap();
    let blocks = code_blocks(&reference);
    for unit in trendy_prelude::UNITS {
        assert!(
            blocks
                .iter()
                .any(|(info, source)| info.is_empty() && source == unit.source),
            "docs/language.md's copy of `{}` differs from {}",
            unit.name,
            unit.source_path
        );
    }
}
