//! Editor features as pure functions of document text.

use std::collections::HashMap;
use std::sync::OnceLock;

use lsp_types::{
    CodeAction, CodeActionKind, CodeActionOrCommand, Diagnostic, DiagnosticRelatedInformation,
    DiagnosticSeverity, Location, NumberOrString, Range, SemanticToken, SemanticTokenModifier,
    SemanticTokenType, SemanticTokensLegend, TextEdit, Uri, WorkspaceEdit,
};

use crate::highlight::{self, Class};
use crate::position::LineIndex;

/// The prelude, parsed and checked once. It ships with the server, and its
/// tests in the compiler keep it valid.
fn prelude() -> &'static trendy_sema::Prelude {
    static PRELUDE: OnceLock<trendy_sema::Prelude> = OnceLock::new();
    PRELUDE.get_or_init(|| {
        let source = trendy_prelude::SOURCE;
        let parse = trendy_parser::parse(source, trendy_lexer::lex(source));
        assert!(parse.errors.is_empty(), "the prelude doesn't parse");
        trendy_sema::Prelude::load(&parse.tree).expect("the prelude is valid")
    })
}

/// Every lexer and parser error, or if there are none, every sema error.
fn errors(source: &str) -> Vec<trendy_diag::Diagnostic> {
    let parse = trendy_parser::parse(source, trendy_lexer::lex(source));
    if parse.errors.is_empty() {
        trendy_sema::analyze(prelude(), &parse.tree)
            .err()
            .unwrap_or_default()
    } else {
        parse.errors
    }
}

/// Errors to show in the editor.
pub fn diagnostics(uri: &Uri, source: &str) -> Vec<Diagnostic> {
    let index = LineIndex::new(source);
    errors(source)
        .iter()
        .map(|d| to_lsp(uri, &index, d))
        .collect()
}

/// An LSP diagnostic carrying everything the compiler would print: the
/// message gets the notes, help, and what each fix does, and secondary labels
/// become related locations.
fn to_lsp(uri: &Uri, index: &LineIndex, d: &trendy_diag::Diagnostic) -> Diagnostic {
    let message = std::iter::once(d.message.clone())
        .chain(d.notes.iter().map(|note| format!("note: {note}")))
        .chain(d.help.iter().map(|help| format!("help: {help}")))
        .chain(d.suggestions.iter().map(|s| format!("help: {}", s.message)))
        .collect::<Vec<_>>()
        .join("\n");

    let related: Vec<_> = d
        .secondary
        .iter()
        .map(|label| DiagnosticRelatedInformation {
            location: Location::new(uri.clone(), index.range(label.span)),
            message: label.message.clone(),
        })
        .collect();

    Diagnostic {
        range: index.range(d.span()),
        severity: Some(DiagnosticSeverity::ERROR),
        code: Some(NumberOrString::String(d.code.id().to_string())),
        source: Some("trendy".to_string()),
        message,
        related_information: (!related.is_empty()).then_some(related),
        ..Diagnostic::default()
    }
}

/// A quick fix for each suggestion on an error that touches `range`.
pub fn code_actions(uri: &Uri, source: &str, range: Range) -> Vec<CodeActionOrCommand> {
    let index = LineIndex::new(source);
    let mut actions = Vec::new();
    for d in errors(source) {
        let lsp = to_lsp(uri, &index, &d);
        if lsp.range.start > range.end || range.start > lsp.range.end {
            continue;
        }
        let only_one = d.suggestions.len() == 1;
        for suggestion in &d.suggestions {
            let edits = suggestion
                .edits
                .iter()
                .map(|e| TextEdit::new(index.range(e.span), e.replacement.clone()))
                .collect();
            actions.push(CodeActionOrCommand::CodeAction(CodeAction {
                title: suggestion.message.clone(),
                kind: Some(CodeActionKind::QUICKFIX),
                diagnostics: Some(vec![lsp.clone()]),
                edit: Some(WorkspaceEdit {
                    changes: Some(HashMap::from([(uri.clone(), edits)])),
                    ..WorkspaceEdit::default()
                }),
                is_preferred: Some(only_one),
                ..CodeAction::default()
            }));
        }
    }
    actions
}

/// The semantic token types and modifiers the server uses. Token data refers
/// to them by index.
pub fn semantic_tokens_legend() -> SemanticTokensLegend {
    SemanticTokensLegend {
        token_types: vec![
            SemanticTokenType::KEYWORD,
            SemanticTokenType::FUNCTION,
            SemanticTokenType::VARIABLE,
            SemanticTokenType::TYPE,
            SemanticTokenType::NUMBER,
            SemanticTokenType::STRING,
            SemanticTokenType::PROPERTY,
            SemanticTokenType::PARAMETER,
            SemanticTokenType::OPERATOR,
        ],
        token_modifiers: vec![
            SemanticTokenModifier::DECLARATION,
            SemanticTokenModifier::DEFINITION,
        ],
    }
}

/// Index into the legend's `token_types`.
fn token_type(class: Class) -> u32 {
    match class {
        Class::Keyword => 0,
        // Operators are trendy's functions.
        Class::Operator => 1,
        Class::Variable => 2,
        Class::Type => 3,
        Class::Number => 4,
        Class::Text => 5,
        Class::Attribute => 6,
        Class::Parameter => 7,
        Class::ExprOperator => 8,
    }
}

/// Bitset of the legend's `token_modifiers`: a definition is also a
/// declaration.
const DEFINITION_MODIFIERS: u32 = 0b11;

/// Semantic tokens for the whole document, delta-encoded as LSP requires.
pub fn semantic_tokens(source: &str) -> Vec<SemanticToken> {
    let parse = trendy_parser::parse(source, trendy_lexer::lex(source));
    let index = LineIndex::new(source);
    let mut previous = lsp_types::Position::new(0, 0);
    let mut tokens = Vec::new();
    for h in highlight::highlights(&parse.tree) {
        let start = index.position(h.span.start);
        let length = source[h.span.start..h.span.end].encode_utf16().count();
        let delta_line = start.line - previous.line;
        tokens.push(SemanticToken {
            delta_line,
            delta_start: if delta_line == 0 {
                start.character - previous.character
            } else {
                start.character
            },
            length: u32::try_from(length).unwrap_or(u32::MAX),
            token_type: token_type(h.class),
            token_modifiers_bitset: if h.definition {
                DEFINITION_MODIFIERS
            } else {
                0
            },
        });
        previous = start;
    }
    tokens
}

/// Edits that bring the document to canonical format, or `None` if it has
/// syntax errors. Formatting needs only syntax, so sema errors don't block it.
pub fn format(source: &str) -> Option<Vec<TextEdit>> {
    let parse = trendy_parser::parse(source, trendy_lexer::lex(source));
    if !parse.errors.is_empty() {
        return None;
    }
    let formatted = trendy_fmt::format(&parse.tree);
    if formatted == source {
        return Some(Vec::new());
    }
    Some(vec![TextEdit::new(
        LineIndex::new(source).full_range(),
        formatted,
    )])
}

#[cfg(test)]
mod tests {
    use super::*;
    use lsp_types::Position;

    fn uri() -> Uri {
        "file:///main.trendy".parse().unwrap()
    }

    #[test]
    fn valid_program_has_no_diagnostics() {
        assert_eq!(diagnostics(&uri(), "operator main{} [];"), []);
    }

    #[test]
    fn parse_error_is_located_where_the_fix_goes() {
        let diags = diagnostics(&uri(), "operator main{}\n  [;");
        assert_eq!(diags.len(), 1);
        assert_eq!(
            diags[0].code,
            Some(NumberOrString::String("E0002".to_string()))
        );
        assert!(diags[0].message.starts_with("expected `]`, found `;`\n"));
        assert!(diags[0].message.ends_with("\nhelp: insert `]`"));
        assert_eq!(
            diags[0].range,
            Range::new(Position::new(1, 3), Position::new(1, 3))
        );
    }

    #[test]
    fn secondary_labels_are_related_information() {
        let diags = diagnostics(&uri(), "operator main{}[];\noperator main{}[];");
        let related = diags[0].related_information.as_ref().unwrap();
        assert_eq!(
            related[0].message,
            "`main` is first defined here as an operator"
        );
        assert_eq!(
            related[0].location.range,
            Range::new(Position::new(0, 9), Position::new(0, 13))
        );
    }

    #[test]
    fn reports_every_syntax_error() {
        let diags = diagnostics(&uri(), "operator a{} [ operator # b{}[];");
        assert_eq!(diags.len(), 2);
    }

    #[test]
    fn sema_errors_are_reported() {
        let diags = diagnostics(&uri(), "operator start{} [];");
        assert!(
            diags[0]
                .message
                .starts_with("program has no `main` operator")
        );
    }

    #[test]
    fn quick_fix_for_error_in_range() {
        let source = "operator main() [];";
        let at_paren = Range::new(Position::new(0, 13), Position::new(0, 13));
        let actions = code_actions(&uri(), source, at_paren);
        assert_eq!(actions.len(), 1);
        let CodeActionOrCommand::CodeAction(action) = &actions[0] else {
            panic!("expected a code action");
        };
        assert_eq!(action.title, "use `{}`");
        let edits = &action.edit.as_ref().unwrap().changes.as_ref().unwrap()[&uri()];
        assert_eq!(
            edits,
            &[
                TextEdit::new(
                    Range::new(Position::new(0, 13), Position::new(0, 14)),
                    "{".to_string()
                ),
                TextEdit::new(
                    Range::new(Position::new(0, 14), Position::new(0, 15)),
                    "}".to_string()
                ),
            ]
        );
    }

    #[test]
    fn no_quick_fix_outside_range() {
        let source = "operator main() [];\n\n\n";
        let elsewhere = Range::new(Position::new(3, 0), Position::new(3, 0));
        assert!(code_actions(&uri(), source, elsewhere).is_empty());
    }

    #[test]
    fn semantic_tokens_are_delta_encoded() {
        let token = |delta_line, delta_start, length, token_type, modifiers| SemanticToken {
            delta_line,
            delta_start,
            length,
            token_type,
            token_modifiers_bitset: modifiers,
        };
        assert_eq!(
            // `é` is an error token: 2 bytes, 1 UTF-16 unit, not highlighted.
            semantic_tokens("operator main{} [];\n\né operator x{} [];"),
            [
                token(0, 0, 8, 0, 0),
                token(0, 9, 4, 1, DEFINITION_MODIFIERS),
                token(2, 2, 8, 0, 0),
                token(0, 9, 1, 1, DEFINITION_MODIFIERS),
            ]
        );
    }

    #[test]
    fn token_types_match_the_legend() {
        let legend = semantic_tokens_legend();
        let named = |class| {
            legend.token_types[token_type(class) as usize]
                .as_str()
                .to_string()
        };
        assert_eq!(named(Class::Keyword), "keyword");
        assert_eq!(named(Class::Operator), "function");
        assert_eq!(named(Class::Variable), "variable");
        assert_eq!(named(Class::Type), "type");
        assert_eq!(named(Class::Number), "number");
        assert_eq!(named(Class::Text), "string");
        assert_eq!(named(Class::Attribute), "property");
        assert_eq!(named(Class::Parameter), "parameter");
    }

    #[test]
    fn formats_whole_document() {
        let edits = format("operator  main {} [] ;").unwrap();
        assert_eq!(
            edits,
            [TextEdit::new(
                Range::new(Position::new(0, 0), Position::new(0, 22)),
                "operator main{} [];\n".to_string(),
            )]
        );
    }

    #[test]
    fn formatted_document_needs_no_edits() {
        assert_eq!(format("operator main{} [];\n"), Some(Vec::new()));
    }

    #[test]
    fn formats_despite_sema_errors() {
        assert_eq!(format("operator start{}[];").unwrap().len(), 1);
    }

    #[test]
    fn does_not_format_unparsable_document() {
        assert_eq!(format("operator main{"), None);
    }
}
