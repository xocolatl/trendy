//! The human-readable diagnostic format.
//!
//! ```text
//! error[E0002]: expected `]`, found `operator`
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

use crate::{Diagnostic, Label, SourceMap, Suggestion};

pub(crate) fn human(d: &Diagnostic, path: &str, map: &SourceMap) -> String {
    let mut labels: Vec<(&Label, char)> = std::iter::once((&d.primary, '^'))
        .chain(d.secondary.iter().map(|l| (l, '-')))
        .collect();
    labels.sort_by_key(|(l, _)| l.span.start);

    let shown_suggestions: Vec<_> = d
        .suggestions
        .iter()
        .map(|s| (s, single_line_patch(s, map)))
        .collect();
    let max_line = labels
        .iter()
        .map(|(l, _)| map.line_col(l.span.end).line)
        .chain(
            shown_suggestions
                .iter()
                .filter_map(|(_, p)| p.as_ref().map(|p| p.line)),
        )
        .max()
        .unwrap_or(1);
    let gutter = Gutter(max_line.to_string().len());

    let mut out = String::new();
    // Writing to a `String` cannot fail.
    let _ = writeln!(out, "error[{}]: {}", d.code, d.message);
    let at = map.line_col(d.primary.span.start);
    let _ = writeln!(out, "{}--> {path}:{}:{}", gutter.pad(), at.line, at.col);
    out.push_str(&gutter.empty());

    let mut prev_line = None;
    for line in lines_of(&labels, map) {
        if prev_line.is_some_and(|prev| line > prev + 1) {
            out.push_str("...\n");
        }
        prev_line = Some(line);
        let text = display(map.line_text(line));
        out.push_str(&gutter.line(line, &text));

        for (label, marker) in labels
            .iter()
            .filter(|(l, _)| map.line_col(l.span.start).line == line)
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
            row.extend(std::iter::repeat_n(*marker, width));
            if !label.message.is_empty() {
                row.push(' ');
                row.push_str(&label.message);
            }
            out.push_str(&gutter.annotation(&row));
        }
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
            out.push_str(&gutter.empty());
            out.push_str(&gutter.line(patch.line, &patch.text));
            out.push_str(&gutter.annotation(&patch.markers));
        }
    }
    out
}

/// The distinct lines that labels start on, in order.
fn lines_of(labels: &[(&Label, char)], map: &SourceMap) -> Vec<usize> {
    let mut lines: Vec<_> = labels
        .iter()
        .map(|(l, _)| map.line_col(l.span.start).line)
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
struct Patch {
    line: usize,
    text: String,
    markers: String,
}

/// The patched line, if every edit is an insertion or replacement within one
/// line. Other suggestions are shown by message only; their edits are in the
/// JSON output.
fn single_line_patch(suggestion: &Suggestion, map: &SourceMap) -> Option<Patch> {
    let first = suggestion.edits.first()?;
    let line = map.line_col(first.span.start).line;
    let simple = suggestion.edits.iter().all(|e| {
        map.line_col(e.span.start).line == line
            && map.line_col(e.span.end).line == line
            && !e.replacement.is_empty()
            && !e.replacement.contains('\n')
    });
    if !simple {
        return None;
    }

    let line_start = map.line_start(line);
    let original = map.line_text(line);
    let mut edits: Vec<_> = suggestion.edits.iter().collect();
    edits.sort_by_key(|e| e.span.start);

    let mut text = String::new();
    let mut markers = String::new();
    let mut pos = 0;
    for edit in edits {
        let (start, end) = (edit.span.start - line_start, edit.span.end - line_start);
        let kept = display(&original[pos..start]);
        markers.push_str(&" ".repeat(kept.chars().count()));
        text.push_str(&kept);
        let marker = if start == end { '+' } else { '~' };
        markers.extend(std::iter::repeat_n(
            marker,
            edit.replacement.chars().count(),
        ));
        text.push_str(&display(&edit.replacement));
        pos = end;
    }
    text.push_str(&display(&original[pos..]));
    Some(Patch {
        line,
        text,
        markers,
    })
}

#[cfg(test)]
mod tests {
    use crate::{Code, Diagnostic, Edit, Span};

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
            d.render("main.trendy", source),
            "\
error[E0002]: expected `]`, found `operator`
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
            Code::DuplicateOperator,
            "operator `a` is defined more than once",
            Span::new(25, 26),
            "redefined here",
        )
        .with_secondary(Span::new(9, 10), "first defined here");
        assert_eq!(
            d.render("f.trendy", source),
            "\
error[E0101]: operator `a` is defined more than once
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
            d.render("f", source)
                .ends_with("1 | operator main{} [];\n  |              ~~\n"),
            "{}",
            d.render("f", source)
        );
    }

    #[test]
    fn distant_lines_are_elided() {
        let source = "a\nb\nc\nd\n";
        let d = Diagnostic::error(Code::DuplicateOperator, "m", Span::new(6, 7), "here")
            .with_secondary(Span::new(0, 1), "there");
        assert!(
            d.render("f", source)
                .contains("1 | a\n  | - there\n...\n4 | d\n")
        );
    }
}
