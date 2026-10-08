//! The examples in every `trendyc --explain` page, and in
//! `docs/language.md`, must behave as documented: a ```` ```trendy ```` block
//! compiles cleanly, and a ```` ```trendy,error ```` block doesn't (in an
//! explanation, it must produce that page's code). Where every error in a
//! failing explanation example carries a fix, applying the fixes must resolve
//! that code.

use trendy_diag::{Code, Diagnostic};

/// Every error in `source`, as the compiler would report it.
fn errors(source: &str) -> Vec<Diagnostic> {
    let prelude_source = trendy_prelude::SOURCE;
    let prelude = trendy_parser::parse(prelude_source, trendy_lexer::lex(prelude_source));
    let prelude = trendy_sema::Prelude::load(&prelude.tree).unwrap();
    let parse = trendy_parser::parse(source, trendy_lexer::lex(source));
    if !parse.errors.is_empty() {
        return parse.errors;
    }
    trendy_sema::analyze(&prelude, &parse.tree)
        .err()
        .unwrap_or_default()
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
        assert!(
            blocks.iter().any(|(info, _)| *info == "trendy,error"),
            "{code} has no failing example"
        );
        assert!(
            blocks.iter().any(|(info, _)| *info == "trendy"),
            "{code} has no corrected example"
        );

        for (info, source) in blocks {
            let errors = errors(&source);
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
                        let remaining = self::errors(&fixed);
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
    for (info, source) in blocks {
        let errors = errors(&source);
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
