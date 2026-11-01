//! Editor features as pure functions of document text.

use std::fmt::Write;

use lsp_types::{
    CodeAction, CodeActionKind, CodeActionOrCommand, Diagnostic, DiagnosticRelatedInformation,
    DiagnosticSeverity, Location, NumberOrString, Range, SemanticToken, SemanticTokenModifier,
    SemanticTokenType, SemanticTokensLegend, TextEdit, Uri, WorkspaceEdit,
};

use crate::highlight::{self, Class};
use crate::position::LineIndex;

thread_local! {
    /// The prelude, parsed once, which sema checks every program with. It
    /// ships with the server, and its tests in the compiler keep it valid.
    static PRELUDE: trendy_parser::Parse = {
        let source = trendy_prelude::SOURCE;
        let parse = trendy_parser::parse(source, trendy_lexer::lex(source));
        assert!(parse.errors.is_empty(), "the prelude doesn't parse");
        parse
    };
}

/// Every lexer and parser error in the document at `uri`, or if there are
/// none, every sema error: as the prelude's, if it is the prelude, and
/// otherwise as a program's, checked with the prelude.
fn errors(uri: &Uri, source: &str) -> Vec<trendy_diag::Diagnostic> {
    let parse = trendy_parser::parse(source, trendy_lexer::lex(source));
    if !parse.errors.is_empty() {
        return parse.errors;
    }
    if is_prelude(uri) {
        trendy_sema::check_prelude(&parse.tree)
    } else {
        PRELUDE.with(|prelude| {
            trendy_sema::analyze(&parse.tree, &prelude.tree)
                .err()
                .unwrap_or_default()
        })
    }
}

/// Where the spans of the errors in a document are: in it, or, as sema
/// checks a program with the prelude, in the prelude, after it.
struct Files<'a> {
    /// The document's, then the prelude's.
    uris: [Uri; 2],
    indexes: [LineIndex<'a>; 2],
    sources: trendy_diag::Sources<'a>,
}

impl<'a> Files<'a> {
    /// The files of the errors in the document at `uri`, `source`.
    fn new(uri: &'a Uri, source: &'a str) -> Self {
        let prelude = trendy_prelude::SOURCE;
        Files {
            uris: [uri.clone(), prelude_uri()],
            indexes: [LineIndex::new(source), LineIndex::new(prelude)],
            sources: trendy_diag::Sources::new([(uri.as_str(), source), ("<prelude>", prelude)]),
        }
    }

    /// Where `span` is.
    fn location(&self, span: trendy_diag::Span) -> Location {
        let located = self.sources.locate(span);
        Location::new(
            self.uris[located.index].clone(),
            self.indexes[located.index].range(located.span),
        )
    }
}

/// The `file` URI of the prelude's source, which the server was built from.
fn prelude_uri() -> Uri {
    let mut uri = String::from("file://");
    for byte in trendy_prelude::SOURCE_PATH.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~/".contains(&byte) {
            uri.push(char::from(byte));
        } else {
            // Writing to a `String` cannot fail.
            let _ = write!(uri, "%{byte:02X}");
        }
    }
    uri.parse().expect("a path, percent-encoded, is in a URI")
}

/// Whether `uri` is the prelude's source, which the server was built from.
fn is_prelude(uri: &Uri) -> bool {
    let is_file = uri
        .scheme()
        .is_some_and(|s| s.as_str().eq_ignore_ascii_case("file"));
    let path = uri.path().as_estr().decode().into_string_lossy();
    let same = |a: &std::path::Path, b: &std::path::Path| match (a.canonicalize(), b.canonicalize())
    {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    };
    is_file
        && same(
            std::path::Path::new(path.as_ref()),
            std::path::Path::new(trendy_prelude::SOURCE_PATH),
        )
}

/// Errors to show in the editor.
pub fn diagnostics(uri: &Uri, source: &str) -> Vec<Diagnostic> {
    let files = Files::new(uri, source);
    errors(uri, source)
        .iter()
        .map(|d| to_lsp(&files, d))
        .collect()
}

/// An LSP diagnostic carrying everything the compiler would print: the
/// message gets the notes, help, and what each fix does, and secondary labels
/// become related locations, in the prelude too. An error is in the
/// document, but for one in the prelude, which is the compiler's own, at
/// the document's start, and related to where it is.
fn to_lsp(files: &Files, d: &trendy_diag::Diagnostic) -> Diagnostic {
    let message = std::iter::once(d.message.clone())
        .chain(d.notes.iter().map(|note| format!("note: {note}")))
        .chain(d.help.iter().map(|help| format!("help: {help}")))
        .chain(d.suggestions.iter().map(|s| format!("help: {}", s.message)))
        .collect::<Vec<_>>()
        .join("\n");

    let primary = files.location(d.span());
    let in_document = primary.uri == files.uris[0];
    let elsewhere = (!in_document).then_some(&d.primary);
    let related: Vec<_> = elsewhere
        .into_iter()
        .chain(&d.secondary)
        .map(|label| DiagnosticRelatedInformation {
            location: files.location(label.span),
            message: label.message.clone(),
        })
        .collect();

    Diagnostic {
        range: if in_document {
            primary.range
        } else {
            Range::default()
        },
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
    let files = Files::new(uri, source);
    let mut actions = Vec::new();
    for d in errors(uri, source) {
        let lsp = to_lsp(&files, &d);
        if lsp.range.start > range.end || range.start > lsp.range.end {
            continue;
        }
        let only_one = d.suggestions.len() == 1;
        for suggestion in &d.suggestions {
            let edits: Vec<(Uri, TextEdit)> = suggestion
                .edits
                .iter()
                .map(|edit| {
                    let location = files.location(edit.span);
                    let text = TextEdit::new(location.range, edit.replacement.clone());
                    (location.uri, text)
                })
                .collect();
            actions.push(CodeActionOrCommand::CodeAction(CodeAction {
                title: suggestion.message.clone(),
                kind: Some(CodeActionKind::QUICKFIX),
                diagnostics: Some(vec![lsp.clone()]),
                edit: Some(WorkspaceEdit {
                    // Each file's edits.
                    changes: Some(
                        files
                            .uris
                            .iter()
                            .filter_map(|uri| {
                                let in_it: Vec<TextEdit> = edits
                                    .iter()
                                    .filter(|(at, _)| at == uri)
                                    .map(|(_, edit)| edit.clone())
                                    .collect();
                                (!in_it.is_empty()).then(|| (uri.clone(), in_it))
                            })
                            .collect(),
                    ),
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
            SemanticTokenType::COMMENT,
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
        Class::Comment => 9,
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
        // A token can't span lines, since clients needn't support it, so a
        // multi-line comment is one token per line.
        let mut offset = h.span.start;
        for line in source[h.span.start..h.span.end].split_inclusive('\n') {
            let text = line.trim_end_matches(['\n', '\r']);
            let start = index.position(offset);
            offset += line.len();
            if text.is_empty() {
                continue;
            }
            let length = text.encode_utf16().count();
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
    fn the_prelude_is_checked_as_the_prelude() {
        let prelude = prelude_uri();
        assert!(is_prelude(&prelude));
        assert_eq!(diagnostics(&prelude, trendy_prelude::SOURCE), []);
        // As a program, it has built-ins it can't declare, operators the
        // prelude already has, and no `main`.
        assert!(!diagnostics(&uri(), trendy_prelude::SOURCE).is_empty());
        // A declaration the compiler doesn't implement is an internal error
        // there.
        let wrong =
            trendy_prelude::SOURCE.replace("debug_value{v: any type}", "debug_value{v: integer}");
        let diags = diagnostics(&prelude, &wrong);
        assert_eq!(diags.len(), 1, "{diags:#?}");
        assert_eq!(
            diags[0].code,
            Some(NumberOrString::String("X0000".to_string()))
        );
        // And an error in an operator it defines is reported as a program's.
        let wrong = trendy_prelude::SOURCE.replace("r where of}", "r where 1}");
        let diags = diagnostics(&prelude, &wrong);
        assert_eq!(
            diags[0].code,
            Some(NumberOrString::String("A0004".to_string()))
        );
    }

    #[test]
    fn parse_error_is_located_where_the_fix_goes() {
        let diags = diagnostics(&uri(), "operator main{}\n  [;");
        assert_eq!(diags.len(), 1);
        assert_eq!(
            diags[0].code,
            Some(NumberOrString::String("P0002".to_string()))
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
    fn labels_in_the_prelude_are_related_information_there() {
        let source = "operator debug_value{v: integer} [];\noperator main{} [];";
        let diags = diagnostics(&uri(), source);
        assert_eq!(diags.len(), 1, "{diags:#?}");
        assert_eq!(
            diags[0].range,
            Range::new(Position::new(0, 9), Position::new(0, 20))
        );
        let related = diags[0].related_information.as_ref().unwrap();
        assert_eq!(related[0].location.uri, prelude_uri());
        let line = trendy_prelude::SOURCE
            .lines()
            .position(|line| line.starts_with("builtin operator debug_value{"))
            .unwrap();
        assert_eq!(
            related[0].location.range,
            Range::new(
                Position::new(u32::try_from(line).unwrap(), 17),
                Position::new(u32::try_from(line).unwrap(), 28)
            )
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
    fn multi_line_comments_are_a_token_per_line() {
        let token = |delta_line, delta_start, length| SemanticToken {
            delta_line,
            delta_start,
            length,
            token_type: token_type(Class::Comment),
            token_modifiers_bitset: 0,
        };
        assert_eq!(
            semantic_tokens("  /* a\r\n\n  bé */"),
            [token(0, 2, 4), token(2, 0, 7)],
            "blank lines in between have no token"
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
