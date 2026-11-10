//! Editor features as pure functions of document text.

use std::fmt::Write;

use lsp_types::{
    CodeAction, CodeActionKind, CodeActionOrCommand, Diagnostic, DiagnosticRelatedInformation,
    DiagnosticSeverity, Location, NumberOrString, Position, Range, SemanticToken,
    SemanticTokenModifier, SemanticTokenType, SemanticTokensLegend, TextEdit, Uri, WorkspaceEdit,
};

use crate::highlight::{self, Class};
use crate::position::LineIndex;

thread_local! {
    /// The units that come with the compiler, parsed once, which sema
    /// checks every program with. They ship with the server, and their
    /// tests in the compiler keep them valid.
    static LIBRARY: Vec<trendy_load::Unit> = {
        let library = trendy_load::library();
        assert!(
            library.iter().all(|unit| unit.parse.errors.is_empty()),
            "a unit of the compiler's doesn't parse"
        );
        library
    };
}

/// The files of `units`, as sema takes them.
fn files(units: &[trendy_load::Unit]) -> Vec<trendy_sema::File<'_>> {
    units
        .iter()
        .map(|unit| trendy_sema::File {
            name: &unit.name,
            path: &unit.path,
            tree: &unit.parse.tree,
        })
        .collect()
}

/// What a document is checked as.
enum Checked {
    /// The source of a unit that comes with the compiler, which the server
    /// was built from, with the others, as the compiler checks them.
    Library(trendy_prelude::Unit),
    /// A program's main unit, with the other units of its program, and
    /// those that come with the compiler.
    Program(trendy_load::Program),
}

impl Checked {
    /// What the document at `uri`, of the text `source`, is checked as.
    fn of(uri: &Uri, source: &str) -> Self {
        match trendy_prelude::UNITS
            .iter()
            .find(|unit| is_source(uri, unit.source_path))
        {
            Some(unit) => Checked::Library(*unit),
            None => Checked::Program(program(uri, source)),
        }
    }
}

/// The program whose main unit is the document at `uri`, of the text
/// `source`: with the units it uses, from their files, if it is a file,
/// and else alone, as it has no directory to find them in.
fn program(uri: &Uri, source: &str) -> trendy_load::Program {
    let path = uri.path().as_estr().decode().into_string_lossy();
    let path = std::path::Path::new(path.as_ref());
    if is_file(uri) {
        trendy_load::program(path, source.to_string())
    } else {
        trendy_load::alone(path, source.to_string())
    }
}

/// Every lexer and parser error in the document at `uri`, of the text
/// `source`, or in the other units of its program, or if there are none,
/// every sema error, as `checked` says it is checked.
fn errors(uri: &Uri, source: &str, checked: &Checked) -> Vec<trendy_diag::Diagnostic> {
    match checked {
        Checked::Program(program) => {
            let errors = program.errors();
            if !errors.is_empty() {
                return errors;
            }
            LIBRARY.with(|library| trendy_sema::check_unit(&files(&program.units), &files(library)))
        }
        Checked::Library(unit) => {
            let parse = trendy_parser::parse(source, trendy_lexer::lex(source));
            if !parse.errors.is_empty() {
                return parse.errors;
            }
            LIBRARY.with(|library| {
                trendy_sema::check_library(&in_its_place(library, *unit, uri, &parse.tree))
            })
        }
    }
}

/// The files of the units that come with the compiler, `library`, with the document at
/// `uri`, `tree`, in the place of its unit, `unit`.
fn in_its_place<'a>(
    library: &'a [trendy_load::Unit],
    unit: trendy_prelude::Unit,
    uri: &'a Uri,
    tree: &'a trendy_syntax::ast::SourceFile,
) -> Vec<trendy_sema::File<'a>> {
    files(library)
        .into_iter()
        .map(|file| {
            if file.name == unit.name {
                trendy_sema::File {
                    name: unit.name,
                    path: uri.as_str(),
                    tree,
                }
            } else {
                file
            }
        })
        .collect()
}

/// Where what the name at `position` in the document at `uri`, of the text `source`, refers
/// to is declared, as sema finds it (see `trendy_sema::definitions`), checking the
/// document as [`diagnostics`] does: in the document, another unit of its program, or a
/// unit that comes with the compiler. Nothing for a document with syntax errors, which
/// sema doesn't check.
pub fn definition(uri: &Uri, source: &str, position: Position) -> Option<Location> {
    let checked = Checked::of(uri, source);
    let found = match &checked {
        Checked::Program(program) => {
            if !program.errors().is_empty() {
                return None;
            }
            LIBRARY
                .with(|library| trendy_sema::definitions(&files(&program.units), &files(library)))
        }
        Checked::Library(unit) => {
            let parse = trendy_parser::parse(source, trendy_lexer::lex(source));
            if !parse.errors.is_empty() {
                return None;
            }
            LIBRARY.with(|library| {
                trendy_sema::definitions(&[], &in_its_place(library, *unit, uri, &parse.tree))
            })
        }
    };
    let files = Files::new(uri, source, &checked);
    let offset = LineIndex::new(source).offset(position);
    let name = found.iter().find(|definition| {
        files
            .in_document(definition.name)
            .is_some_and(|name| name.start <= offset && offset <= name.end)
    })?;
    Some(files.location(name.declaration))
}

/// Where the spans of the errors in a document are: in it, or in another
/// unit of its program, or, as sema checks it with the units that come with
/// the compiler, in one of those, after them.
struct Files<'a> {
    /// The document's, at its place among them.
    document: usize,
    /// Each file's: the units of the document's program, the document's
    /// first, if it is a program's main unit, then those that come with
    /// the compiler, the document in its unit's place if it is one of them.
    uris: Vec<Uri>,
    indexes: Vec<LineIndex<'a>>,
    sources: trendy_diag::Sources<'a>,
}

impl<'a> Files<'a> {
    /// The files of the errors in the document at `uri`, `source`, checked
    /// as `checked` says.
    fn new(uri: &'a Uri, source: &'a str, checked: &'a Checked) -> Self {
        let mut all: Vec<(Uri, &'a str, &'a str)> = match checked {
            Checked::Program(program) => program
                .units
                .iter()
                .enumerate()
                .map(|(i, unit)| {
                    let at = if i == 0 {
                        uri.clone()
                    } else {
                        source_uri(&unit.path)
                    };
                    (at, unit.path.as_str(), unit.source.as_str())
                })
                .collect(),
            Checked::Library(_) => Vec::new(),
        };
        let mut document = 0;
        for unit in trendy_prelude::UNITS {
            match checked {
                Checked::Library(checked) if *checked == unit => {
                    document = all.len();
                    all.push((uri.clone(), uri.as_str(), source));
                }
                _ => all.push((source_uri(unit.source_path), unit.path, unit.source)),
            }
        }
        Files {
            document,
            indexes: all
                .iter()
                .map(|(_, _, text)| LineIndex::new(text))
                .collect(),
            sources: trendy_diag::Sources::new(all.iter().map(|&(_, name, text)| (name, text))),
            uris: all.into_iter().map(|(uri, _, _)| uri).collect(),
        }
    }

    /// Where `span` is in the document, if it is in it.
    fn in_document(&self, span: trendy_diag::Span) -> Option<trendy_diag::Span> {
        let located = self.sources.locate(span);
        (located.index == self.document).then_some(located.span)
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

/// The `file` URI of `path`, a source of the compiler's, which the server
/// was built from.
fn source_uri(path: &str) -> Uri {
    let mut uri = String::from("file://");
    for byte in path.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~/".contains(&byte) {
            uri.push(char::from(byte));
        } else {
            // Writing to a `String` cannot fail.
            let _ = write!(uri, "%{byte:02X}");
        }
    }
    uri.parse().expect("a path, percent-encoded, is in a URI")
}

/// Whether `uri` is a file's.
fn is_file(uri: &Uri) -> bool {
    uri.scheme()
        .is_some_and(|s| s.as_str().eq_ignore_ascii_case("file"))
}

/// Whether `uri` is the source at `path`, which the server was built from.
fn is_source(uri: &Uri, path: &str) -> bool {
    let is_file = is_file(uri);
    let written = uri.path().as_estr().decode().into_string_lossy();
    let same = |a: &std::path::Path, b: &std::path::Path| match (a.canonicalize(), b.canonicalize())
    {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    };
    is_file
        && same(
            std::path::Path::new(written.as_ref()),
            std::path::Path::new(path),
        )
}

/// Errors to show in the editor.
pub fn diagnostics(uri: &Uri, source: &str) -> Vec<Diagnostic> {
    let checked = Checked::of(uri, source);
    let files = Files::new(uri, source, &checked);
    errors(uri, source, &checked)
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
    let in_document = primary.uri == files.uris[files.document];
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
    let checked = Checked::of(uri, source);
    let files = Files::new(uri, source, &checked);
    let mut actions = Vec::new();
    for d in errors(uri, source, &checked) {
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
            SemanticTokenType::NAMESPACE,
            SemanticTokenType::new("label"),
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
        // Units are trendy's namespaces.
        Class::Unit => 10,
        Class::Label => 11,
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
    fn a_document_is_checked_with_the_units_it_uses() {
        let root =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/lsp-tests/units");
        std::fs::create_dir_all(root.join("lakes")).unwrap();
        let root = root.canonicalize().unwrap();
        let depths = root.join("lakes/depths.trendy");
        std::fs::write(&depths, "interface\n\nheading lake := { name: text };").unwrap();
        let main = source_uri(&root.join("main.trendy").display().to_string());
        let source = "uses { lakes.depths };\n\nvar l: relation lake;\n\noperator main{} [];";
        assert_eq!(diagnostics(&main, source), []);
        // An error in the unit used is at the document's start, related to
        // where it is.
        std::fs::write(&depths, "interface\n\nheading lake := { name: txt };").unwrap();
        let diags = diagnostics(&main, source);
        assert_eq!(diags.len(), 1, "{diags:#?}");
        let related = diags[0].related_information.as_ref().unwrap();
        assert_eq!(
            related[0].location.uri,
            source_uri(&depths.display().to_string())
        );
    }

    #[test]
    fn a_unit_for_others_to_use_needs_no_main() {
        let source = "interface\n\noperator f{};\n\nimplementation\n\noperator f{} [];";
        assert_eq!(diagnostics(&uri(), source), []);
        // With neither, nothing can run it or use it.
        let diags = diagnostics(&uri(), "operator f{} [];");
        assert_eq!(diags.len(), 1, "{diags:#?}");
        assert!(
            diags[0]
                .message
                .starts_with("program has no `main` operator")
        );
    }

    #[test]
    fn a_unit_is_checked_from_where_the_units_it_uses_are() {
        let root = std::env::temp_dir().join(format!("trendy-lsp-rooted-{}", std::process::id()));
        std::fs::create_dir_all(root.join("library")).unwrap();
        let root = root.canonicalize().unwrap();
        std::fs::write(
            root.join("library.trendy"),
            "interface\n\ndatabase library;\n\nvar library.books: relation { isbn: text } key { isbn };\n",
        )
        .unwrap();
        let loans = root.join("library/loans.trendy");
        let source = "interface\n\nuses { library };\n\nvar library.loans: relation { isbn: text } key { isbn };\n\nconstraint library.lent := loans{isbn} <= books{isbn};\n";
        // The document's text, as it is being edited, not the file's, and in
        // the name its path from there gives it, which a database's rule reads.
        std::fs::write(&loans, "").unwrap();
        let uri = source_uri(&loans.display().to_string());
        assert_eq!(diagnostics(&uri, source), []);
        let diags = diagnostics(&uri, &source.replace("books{isbn}", "boks{isbn}"));
        assert!(
            diags.iter().any(|d| d.message.contains("boks")),
            "{diags:#?}"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn the_catalog_is_checked_as_the_catalog() {
        let catalog = trendy_prelude::CATALOG;
        let at = source_uri(catalog.source_path);
        assert!(matches!(
            Checked::of(&at, catalog.source),
            Checked::Library(unit) if unit == catalog
        ));
        assert_eq!(diagnostics(&at, catalog.source), []);
        // A constraint the compiler doesn't report is an internal error.
        let wrong = format!(
            "{}\nconstraint catalog.few := count{{r: units}} < 3;",
            catalog.source
        );
        let diags = diagnostics(&at, &wrong);
        assert_eq!(diags.len(), 1, "{diags:#?}");
        assert_eq!(
            diags[0].code,
            Some(NumberOrString::String("X0000".to_string()))
        );
        // A program's unknown unit is reported in it.
        let diags = diagnostics(&uri(), "uses { lakes.depths };\n\noperator main{} [];");
        assert_eq!(
            diags[0].code,
            Some(NumberOrString::String("A0015".to_string()))
        );
    }

    #[test]
    fn the_prelude_is_checked_as_the_prelude() {
        let prelude = trendy_prelude::PRELUDE;
        let at = source_uri(prelude.source_path);
        assert!(matches!(
            Checked::of(&at, prelude.source),
            Checked::Library(unit) if unit == prelude
        ));
        assert_eq!(diagnostics(&at, prelude.source), []);
        // As a program, it has built-ins it can't declare, operators the
        // prelude already has, and no `main`.
        assert!(!diagnostics(&uri(), prelude.source).is_empty());
        // A declaration the compiler doesn't implement is an internal error
        // there.
        let wrong = prelude
            .source
            .replace("debug_value{v: any type}", "debug_value{v: integer}");
        let diags = diagnostics(&at, &wrong);
        assert_eq!(diags.len(), 1, "{diags:#?}");
        assert_eq!(
            diags[0].code,
            Some(NumberOrString::String("X0000".to_string()))
        );
        // And an error in an operator it defines is reported as a program's.
        let wrong = prelude.source.replace("r where of}", "r where 1}");
        let diags = diagnostics(&at, &wrong);
        assert_eq!(
            diags[0].code,
            Some(NumberOrString::String("A0004".to_string()))
        );
    }

    #[test]
    fn trendy_files_is_checked_as_itself() {
        let files = trendy_prelude::FILES;
        let at = source_uri(files.source_path);
        assert!(matches!(
            Checked::of(&at, files.source),
            Checked::Library(unit) if unit == files
        ));
        assert_eq!(diagnostics(&at, files.source), []);
        // A declaration the compiler doesn't implement is an internal error,
        // in the document, though the prelude comes after it.
        let wrong = files.source.replacen(
            "load_text_file{path: file_path, out lines: array text}",
            "load_text_file{path: integer, out lines: array text}",
            1,
        );
        assert_ne!(wrong, files.source);
        let diags = diagnostics(&at, &wrong);
        assert_eq!(diags.len(), 1, "{diags:#?}");
        assert_eq!(
            diags[0].code,
            Some(NumberOrString::String("X0000".to_string()))
        );
        let line = wrong
            .lines()
            .position(|line| line.contains("load_text_file{path: integer"))
            .unwrap();
        assert_eq!(diags[0].range.start.line, u32::try_from(line).unwrap());
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
        // The parameter's type, where the prelude declares `some`.
        let source = "var x := some{r: relation { { x: 1 } }, of: 1};\noperator main{} [];";
        let diags = diagnostics(&uri(), source);
        assert_eq!(diags.len(), 1, "{diags:#?}");
        assert_eq!(
            diags[0].range,
            Range::new(Position::new(0, 44), Position::new(0, 45))
        );
        let related = diags[0].related_information.as_ref().unwrap();
        assert_eq!(
            related[0].location.uri,
            source_uri(trendy_prelude::PRELUDE.source_path)
        );
        let (line, text) = trendy_prelude::PRELUDE
            .source
            .lines()
            .enumerate()
            .find(|(_, line)| line.starts_with("operator some{"))
            .unwrap();
        let column = text.find("boolean").unwrap();
        let at = |column: usize| {
            Position::new(u32::try_from(line).unwrap(), u32::try_from(column).unwrap())
        };
        assert_eq!(
            related[0].location.range,
            Range::new(at(column), at(column + "boolean".len()))
        );
    }

    #[test]
    fn definitions_in_and_of_the_prelude() {
        let prelude = trendy_prelude::PRELUDE;
        let uri = source_uri(prelude.source_path);
        let line_of = |start: &str| {
            let line = prelude
                .source
                .lines()
                .position(|line| line.starts_with(start))
                .unwrap();
            u32::try_from(line).unwrap()
        };
        let count = Range::new(
            Position::new(line_of("builtin operator count{"), 17),
            Position::new(line_of("builtin operator count{"), 22),
        );
        // A built-in operator, from a program's call.
        let source = "var n := count{r: relation { { x: 1 } }};\noperator main{} [];";
        let found = definition(&self::uri(), source, Position::new(0, 11)).unwrap();
        assert_eq!((found.uri, found.range), (uri.clone(), count));
        // The same, from a call in the prelude itself, checked as the library is.
        let found = definition(
            &uri,
            prelude.source,
            Position::new(line_of("    count{r: r} = 0"), 4),
        )
        .unwrap();
        assert_eq!((found.uri, found.range), (uri.clone(), count));
        // A type the prelude declares, from its use there.
        let found = definition(
            &uri,
            prelude.source,
            Position::new(line_of("heading transaction_collection :="), 44),
        )
        .unwrap();
        assert_eq!(
            found.range.start,
            Position::new(line_of("type error union;"), 5)
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
        assert_eq!(named(Class::Unit), "namespace");
        assert_eq!(named(Class::Label), "label");
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
