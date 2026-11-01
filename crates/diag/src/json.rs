//! The machine-readable diagnostic format: one JSON object per diagnostic.
//!
//! ```json
//! {"code":"P0002","message":"expected `]`, found `operator`","file":"main.trendy",
//!  "primary":{"file":"main.trendy","start":{"offset":14,"line":1,"column":15},"end":{…},"label":"expected `]` here"},
//!  "secondary":[…],"notes":[…],"help":[…],
//!  "suggestions":[{"message":"insert `]`","edits":[{"file":"main.trendy","start":{…},"end":{…},"replacement":"]"}]}]}
//! ```
//!
//! `file` is the primary label's file, and each label and edit has its
//! own, which offsets, lines and columns are within. Offsets are bytes;
//! lines and columns are 1-based, and columns count characters. The format
//! is a stable interface: fields may be added, but existing fields keep
//! their meaning.

use serde_json::{Value, json};

use crate::{Diagnostic, Label, Sources, Span};

pub(crate) fn render(d: &Diagnostic, sources: &Sources) -> String {
    let label = |l: &Label| {
        let mut value = range(l.span, sources);
        value["label"] = json!(l.message);
        value
    };
    json!({
        "code": d.code.id(),
        "message": d.message,
        "file": sources.locate(d.primary.span).file.path,
        "primary": label(&d.primary),
        "secondary": d.secondary.iter().map(label).collect::<Vec<_>>(),
        "notes": d.notes,
        "help": d.help,
        "suggestions": d.suggestions.iter().map(|s| json!({
            "message": s.message,
            "edits": s.edits.iter().map(|e| {
                let mut value = range(e.span, sources);
                value["replacement"] = json!(e.replacement);
                value
            }).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
    })
    .to_string()
}

/// `span`'s file, and where it starts and ends in it.
fn range(span: Span, sources: &Sources) -> Value {
    let located = sources.locate(span);
    let position = |offset| {
        let at = located.file.map.line_col(offset);
        json!({ "offset": offset, "line": at.line, "column": at.col })
    };
    json!({
        "file": located.file.path,
        "start": position(located.span.start),
        "end": position(located.span.end),
    })
}

#[cfg(test)]
mod tests {
    use crate::{Code, Diagnostic, Edit, Sources, Span};

    #[test]
    fn shape() {
        let d = Diagnostic::error(Code::MissingToken, "msg", Span::empty(3), "here")
            .with_help("try this")
            .with_suggestion(
                "insert `;`",
                vec![Edit {
                    span: Span::empty(3),
                    replacement: ";".to_string(),
                }],
            );
        let value: serde_json::Value =
            serde_json::from_str(&d.to_json(&Sources::one("f.trendy", "a\nbc"))).unwrap();
        assert_eq!(
            value,
            serde_json::json!({
                "code": "P0002",
                "message": "msg",
                "file": "f.trendy",
                "primary": {
                    "file": "f.trendy",
                    "start": { "offset": 3, "line": 2, "column": 2 },
                    "end": { "offset": 3, "line": 2, "column": 2 },
                    "label": "here",
                },
                "secondary": [],
                "notes": [],
                "help": ["try this"],
                "suggestions": [{
                    "message": "insert `;`",
                    "edits": [{
                        "file": "f.trendy",
                        "start": { "offset": 3, "line": 2, "column": 2 },
                        "end": { "offset": 3, "line": 2, "column": 2 },
                        "replacement": ";",
                    }],
                }],
            })
        );
    }

    #[test]
    fn labels_have_their_files() {
        let sources = Sources::new([("main.trendy", "a\n"), ("<prelude>", "b\nc")]);
        let d = Diagnostic::error(Code::DuplicateName, "m", Span::new(0, 1), "here")
            .with_secondary(Span::new(4, 5), "there");
        let value: serde_json::Value = serde_json::from_str(&d.to_json(&sources)).unwrap();
        assert_eq!(value["file"], "main.trendy");
        assert_eq!(value["primary"]["file"], "main.trendy");
        assert_eq!(
            value["secondary"][0],
            serde_json::json!({
                "file": "<prelude>",
                "start": { "offset": 2, "line": 2, "column": 1 },
                "end": { "offset": 3, "line": 2, "column": 2 },
                "label": "there",
            })
        );
    }
}
