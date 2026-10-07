use std::collections::HashMap;

use trendy_diag::{Code, Diagnostic, Edit, Span};
use trendy_hir as hir;
use trendy_syntax::ast::{AstNode, SourceFile};

/// Check a parsed program and translate it to HIR.
///
/// This is the last layer allowed to report errors to the user. Drivers run
/// it only on trees without syntax errors; parts that are missing anyway are
/// skipped, never guessed at.
///
/// # Errors
///
/// Returns every operator defined more than once, and a missing `main`, in
/// source order.
pub fn analyze(file: &SourceFile) -> Result<hir::Program, Vec<Diagnostic>> {
    let mut errors = Vec::new();
    // Each name's id and the span of its first definition.
    let mut defined: HashMap<String, (hir::OperatorId, Span)> = HashMap::new();
    let mut operators = Vec::new();

    for ident in file.operators().filter_map(|op| op.name()?.ident()) {
        let name = ident.text();
        let span = trendy_syntax::span(ident.text_range());
        if let Some(&(_, first)) = defined.get(name) {
            errors.push(duplicate_operator(name, span, first));
            continue;
        }
        defined.insert(name.to_string(), (hir::OperatorId(operators.len()), span));
        operators.push(hir::Operator {
            name: name.to_string(),
        });
    }

    let entry = defined.get("main").map(|&(id, _)| id);
    if entry.is_none() {
        errors.push(missing_main(file, &defined));
    }

    match entry {
        Some(entry) if errors.is_empty() => Ok(hir::Program { operators, entry }),
        _ => Err(errors),
    }
}

fn duplicate_operator(name: &str, span: Span, first: Span) -> Diagnostic {
    Diagnostic::error(
        Code::DuplicateOperator,
        format!("operator `{name}` is defined more than once"),
        span,
        "redefined here",
    )
    .with_secondary(first, format!("`{name}` is first defined here"))
    .with_note("operator names must be unique within a program")
    .with_help("delete the duplicate definition, or rename one of them")
}

fn missing_main(
    file: &SourceFile,
    defined: &HashMap<String, (hir::OperatorId, Span)>,
) -> Diagnostic {
    const MESSAGE: &str = "program has no `main` operator";
    const ENTRY_NOTE: &str = "execution starts in the operator named `main`";

    // `Main` or `MAIN` is almost certainly meant to be the entry point, so the
    // fix goes there.
    let near_miss = defined
        .iter()
        .filter(|(name, _)| name.eq_ignore_ascii_case("main"))
        .map(|(name, &(_, span))| (name, span))
        .min_by_key(|&(_, span)| span.start);
    if let Some((name, span)) = near_miss {
        return Diagnostic::error(
            Code::MissingMain,
            MESSAGE,
            span,
            format!("`{name}` is not `main`: names are case-sensitive"),
        )
        .with_note(ENTRY_NOTE)
        .with_suggestion(
            format!("rename `{name}` to `main`"),
            vec![Edit {
                span,
                replacement: "main".to_string(),
            }],
        );
    }

    let end = file.span().end;
    let text = file.syntax().to_string();
    let separator = match () {
        () if text.is_empty() => "",
        () if text.ends_with('\n') => "\n",
        () => "\n\n",
    };
    Diagnostic::error(
        Code::MissingMain,
        MESSAGE,
        Span::empty(end),
        "expected an operator named `main` in this file",
    )
    .with_note(ENTRY_NOTE)
    .with_suggestion(
        "add an entry point: `operator main{} [];`",
        vec![Edit {
            span: Span::empty(end),
            replacement: format!("{separator}operator main{{}} [];\n"),
        }],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn analyze_str(source: &str) -> Result<hir::Program, Vec<Diagnostic>> {
        let parse = trendy_parser::parse(source, trendy_lexer::lex(source));
        assert!(parse.errors.is_empty(), "{:?}", parse.errors);
        analyze(&parse.tree)
    }

    fn messages(source: &str) -> Vec<String> {
        analyze_str(source)
            .unwrap_err()
            .into_iter()
            .map(|d| d.message)
            .collect()
    }

    #[test]
    fn empty_main() {
        let program = analyze_str("operator main{} [];").unwrap();
        assert_eq!(program.operator(program.entry).name, "main");
    }

    #[test]
    fn entry_is_main_wherever_it_appears() {
        let program = analyze_str("operator a{}[]; operator main{}[];").unwrap();
        assert_eq!(program.entry, hir::OperatorId(1));
    }

    #[test]
    fn missing_main() {
        assert_eq!(
            messages("operator start{} [];"),
            ["program has no `main` operator"]
        );
    }

    #[test]
    fn duplicate_operator() {
        let errors = analyze_str("operator main{}[]; operator main{}[];").unwrap_err();
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].code, Code::DuplicateOperator);
        assert_eq!(
            errors[0].message,
            "operator `main` is defined more than once"
        );
        assert_eq!(errors[0].span(), Span::new(28, 32));
        assert_eq!(errors[0].secondary[0].span, Span::new(9, 13));
    }

    #[test]
    fn missing_main_suggests_adding_one() {
        let errors = analyze_str("operator start{} [];\n").unwrap_err();
        let edit = &errors[0].suggestions[0].edits[0];
        assert_eq!(edit.span, Span::empty(21));
        assert_eq!(edit.replacement, "\noperator main{} [];\n");
    }

    #[test]
    fn missing_main_suggests_renaming_near_miss() {
        let errors = analyze_str("operator Main{} [];").unwrap_err();
        assert_eq!(errors[0].span(), Span::new(9, 13));
        let suggestion = &errors[0].suggestions[0];
        assert_eq!(suggestion.message, "rename `Main` to `main`");
        assert_eq!(suggestion.edits[0].span, Span::new(9, 13));
    }

    #[test]
    fn reports_every_error() {
        assert_eq!(
            messages("operator a{}[]; operator a{}[]; operator a{}[];"),
            [
                "operator `a` is defined more than once",
                "operator `a` is defined more than once",
                "program has no `main` operator",
            ]
        );
    }
}
