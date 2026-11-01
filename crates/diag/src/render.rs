//! The human-readable diagnostic format.
//!
//! ```text
//! error[P0002]: expected `]`, found `operator`
//!  --> main.trendy:1:15
//!   |
//! 1 | operator a{} [
//!   |               ^ expected `]` here
//! 2 | operator main{} [];
//!   | -------- the next definition starts here
//! help: insert `]`
//!   |
//! 1 | operator a{} []
//!   |               +
//! ```

use std::fmt::Write;

use crate::{Diagnostic, File, LineCol, Sources, Span, Suggestion};

/// A label, in its file.
struct Placed<'s, 'a> {
    /// Its file's place among the sources.
    index: usize,
    file: &'s File<'a>,
    /// Its span within the file.
    span: Span,
    message: &'s str,
    marker: char,
}

/// `d`, with excerpts of the files of `sources` it has labels in: the
/// primary label's first, under ` --> `, and each other's under ` ::: `.
pub(crate) fn human(d: &Diagnostic, sources: &Sources) -> String {
    let primary = sources.locate(d.primary.span);
    let mut labels: Vec<Placed> = std::iter::once((&d.primary, '^'))
        .chain(d.secondary.iter().map(|l| (l, '-')))
        .map(|(label, marker)| {
            let at = sources.locate(label.span);
            Placed {
                index: at.index,
                file: at.file,
                span: at.span,
                message: &label.message,
                marker,
            }
        })
        .collect();
    labels.sort_by_key(|l| (l.index != primary.index, l.index, l.span.start));

    let shown_suggestions: Vec<_> = d
        .suggestions
        .iter()
        .map(|s| (s, single_line_patch(s, sources)))
        .collect();
    let max_line = labels
        .iter()
        .map(|l| l.file.map.line_col(l.span.end).line)
        .chain(
            shown_suggestions
                .iter()
                .filter_map(|(_, p)| p.as_ref().map(|p| p.at.line)),
        )
        .max()
        .unwrap_or(1);
    let gutter = Gutter(max_line.to_string().len());

    let mut out = String::new();
    // Writing to a `String` cannot fail.
    let _ = writeln!(out, "error[{}]: {}", d.code, d.message);
    let at = primary.file.map.line_col(primary.span.start);
    let _ = writeln!(
        out,
        "{}--> {}:{}:{}",
        gutter.pad(),
        primary.file.path,
        at.line,
        at.col
    );
    let mut first = true;
    for group in labels.chunk_by(|a, b| a.index == b.index) {
        if !first {
            let file = group[0].file;
            let at = file.map.line_col(group[0].span.start);
            out.push_str(&gutter.empty());
            let _ = writeln!(
                out,
                "{}::: {}:{}:{}",
                gutter.pad(),
                file.path,
                at.line,
                at.col
            );
        }
        first = false;
        out.push_str(&gutter.empty());
        excerpt(&mut out, group, &gutter);
    }

    for note in &d.notes {
        let _ = writeln!(out, "{} = note: {note}", gutter.pad());
    }
    for help in &d.help {
        let _ = writeln!(out, "{} = help: {help}", gutter.pad());
    }

    for (suggestion, patch) in &shown_suggestions {
        let _ = writeln!(out, "help: {}", suggestion.message);
        if let Some(patch) = patch {
            if patch.index != primary.index {
                let _ = writeln!(
                    out,
                    "{}::: {}:{}:{}",
                    gutter.pad(),
                    patch.path,
                    patch.at.line,
                    patch.at.col
                );
            }
            out.push_str(&gutter.empty());
            out.push_str(&gutter.line(patch.at.line, &patch.text));
            out.push_str(&gutter.annotation(&patch.markers));
        }
    }
    out
}

/// The lines of one file that `labels`, all in it, start on, each with its
/// labels' markers under it.
fn excerpt(out: &mut String, labels: &[Placed], gutter: &Gutter) {
    let map = &labels[0].file.map;
    let mut prev_line = None;
    for line in lines_of(labels) {
        if prev_line.is_some_and(|prev| line > prev + 1) {
            out.push_str("...\n");
        }
        prev_line = Some(line);
        let text = display(map.line_text(line));
        out.push_str(&gutter.line(line, &text));

        for label in labels
            .iter()
            .filter(|l| map.line_col(l.span.start).line == line)
        {
            let start = map.line_col(label.span.start);
            let end = map.line_col(label.span.end);
            let width = if end.line == line {
                end.col.saturating_sub(start.col)
            } else {
                text.chars().count() + 1 - start.col
            }
            .max(1);
            let mut row = " ".repeat(start.col - 1);
            row.extend(std::iter::repeat_n(label.marker, width));
            if !label.message.is_empty() {
                row.push(' ');
                row.push_str(label.message);
            }
            out.push_str(&gutter.annotation(&row));
        }
    }
}

/// The distinct lines that labels, all in one file, start on, in order.
fn lines_of(labels: &[Placed]) -> Vec<usize> {
    let mut lines: Vec<_> = labels
        .iter()
        .map(|l| l.file.map.line_col(l.span.start).line)
        .collect();
    lines.sort_unstable();
    lines.dedup();
    lines
}

/// Tabs become single spaces so that columns line up with the markers.
fn display(text: &str) -> String {
    text.replace('\t', " ")
}

struct Gutter(usize);

impl Gutter {
    fn pad(&self) -> String {
        " ".repeat(self.0)
    }

    fn empty(&self) -> String {
        format!("{} |\n", self.pad())
    }

    fn line(&self, number: usize, text: &str) -> String {
        format!("{number:>width$} | {text}\n", width = self.0)
    }

    fn annotation(&self, row: &str) -> String {
        format!("{} | {}\n", self.pad(), row.trim_end())
    }
}

/// A suggestion applied to its line, with `+` under inserted text and `~`
/// under replaced text.
struct Patch<'a> {
    /// Its file's place among the sources, and what it is called.
    index: usize,
    path: &'a str,
    /// Where its first edit is.
    at: LineCol,
    text: String,
    markers: String,
}

/// The patched line, if every edit is an insertion or replacement within one
/// line of one file. Other suggestions are shown by message only; their
/// edits are in the JSON output.
fn single_line_patch<'a>(suggestion: &Suggestion, sources: &Sources<'a>) -> Option<Patch<'a>> {
    let first = sources.locate(suggestion.edits.first()?.span);
    let (index, file) = (first.index, first.file);
    let map = &file.map;
    let at = map.line_col(first.span.start);
    let mut edits = Vec::new();
    for edit in &suggestion.edits {
        let located = sources.locate(edit.span);
        let simple = located.index == index
            && map.line_col(located.span.start).line == at.line
            && map.line_col(located.span.end).line == at.line
            && !edit.replacement.is_empty()
            && !edit.replacement.contains('\n');
        if !simple {
            return None;
        }
        edits.push((located.span, edit.replacement.as_str()));
    }

    let line_start = map.line_start(at.line);
    let original = map.line_text(at.line);
    edits.sort_by_key(|(span, _)| span.start);

    let mut text = String::new();
    let mut markers = String::new();
    let mut pos = 0;
    for (span, replacement) in edits {
        let (start, end) = (span.start - line_start, span.end - line_start);
        let kept = display(&original[pos..start]);
        markers.push_str(&" ".repeat(kept.chars().count()));
        text.push_str(&kept);
        let marker = if start == end { '+' } else { '~' };
        markers.extend(std::iter::repeat_n(marker, replacement.chars().count()));
        text.push_str(&display(replacement));
        pos = end;
    }
    text.push_str(&display(&original[pos..]));
    Some(Patch {
        index,
        path: file.path,
        at,
        text,
        markers,
    })
}

#[cfg(test)]
mod tests {
    use crate::{Code, Diagnostic, Edit, Sources, Span};

    #[test]
    fn full_rendering() {
        let source = "operator a{} [\noperator main{} [];\n";
        let d = Diagnostic::error(
            Code::MissingToken,
            "expected `]`, found `operator`",
            Span::empty(14),
            "expected `]` here",
        )
        .with_secondary(Span::new(15, 23), "the next definition starts here")
        .with_note("a note")
        .with_suggestion(
            "insert `]`",
            vec![Edit {
                span: Span::empty(14),
                replacement: "]".to_string(),
            }],
        );
        assert_eq!(
            d.render(&Sources::one("main.trendy", source)),
            "\
error[P0002]: expected `]`, found `operator`
 --> main.trendy:1:15
  |
1 | operator a{} [
  |               ^ expected `]` here
2 | operator main{} [];
  | -------- the next definition starts here
  = note: a note
help: insert `]`
  |
1 | operator a{} []
  |               +
"
        );
    }

    #[test]
    fn labels_on_one_line_get_a_row_each() {
        let source = "operator a{}[]; operator a{}[];";
        let d = Diagnostic::error(
            Code::DuplicateName,
            "operator `a` is defined more than once",
            Span::new(25, 26),
            "redefined here",
        )
        .with_secondary(Span::new(9, 10), "first defined here");
        assert_eq!(
            d.render(&Sources::one("f.trendy", source)),
            "\
error[A0001]: operator `a` is defined more than once
 --> f.trendy:1:26
  |
1 | operator a{}[]; operator a{}[];
  |          - first defined here
  |                          ^ redefined here
"
        );
    }

    #[test]
    fn replacement_markers() {
        let source = "operator main() [];";
        let d = Diagnostic::error(Code::WrongDelimiter, "m", Span::new(13, 14), "")
            .with_suggestion(
                "use braces",
                vec![
                    Edit {
                        span: Span::new(13, 14),
                        replacement: "{".to_string(),
                    },
                    Edit {
                        span: Span::new(14, 15),
                        replacement: "}".to_string(),
                    },
                ],
            );
        assert!(
            d.render(&Sources::one("f", source))
                .ends_with("1 | operator main{} [];\n  |              ~~\n"),
            "{}",
            d.render(&Sources::one("f", source))
        );
    }

    #[test]
    fn a_label_in_another_file_has_an_excerpt_of_its_own() {
        let (program, prelude) = ("operator count{} [];\n", "\nbuiltin operator count{};\n");
        let sources = Sources::new([("main.trendy", program), ("<prelude>", prelude)]);
        let at = program.len();
        let d = Diagnostic::error(Code::DuplicateName, "m", Span::new(9, 14), "redefined here")
            .with_secondary(Span::new(at + 18, at + 23), "first defined here")
            .with_suggestion(
                "rename it",
                vec![Edit {
                    span: Span::new(9, 14),
                    replacement: "total".to_string(),
                }],
            );
        assert_eq!(
            d.render(&sources),
            "\
error[A0001]: m
 --> main.trendy:1:10
  |
1 | operator count{} [];
  |          ^^^^^ redefined here
  |
 ::: <prelude>:2:18
  |
2 | builtin operator count{};
  |                  ----- first defined here
help: rename it
  |
1 | operator total{} [];
  |          ~~~~~
"
        );
    }

    #[test]
    fn distant_lines_are_elided() {
        let source = "a\nb\nc\nd\n";
        let d = Diagnostic::error(Code::DuplicateName, "m", Span::new(6, 7), "here")
            .with_secondary(Span::new(0, 1), "there");
        assert!(
            d.render(&Sources::one("f", source))
                .contains("1 | a\n  | - there\n...\n4 | d\n")
        );
    }
}
