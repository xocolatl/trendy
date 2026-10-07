//! The examples in every `trendyc --explain` page must behave as documented:
//! a ```` ```trendy,error ```` block produces its code, and a ```` ```trendy ````
//! block compiles cleanly. Where every error in a failing example carries a
//! fix, applying the fixes must produce a valid program.

use trendy_diag::{Code, Diagnostic};

/// Every error in `source`, as the compiler would report it.
fn errors(source: &str) -> Vec<Diagnostic> {
    let parse = trendy_parser::parse(source, trendy_lexer::lex(source));
    if !parse.errors.is_empty() {
        return parse.errors;
    }
    trendy_sema::analyze(&parse.tree).err().unwrap_or_default()
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
