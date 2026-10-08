//! The machine-readable diagnostic format: one JSON object per diagnostic.
//!
//! ```json
//! {"code":"P0002","message":"expected `]`, found `operator`","file":"main.trendy",
//!  "primary":{"start":{"offset":14,"line":1,"column":15},"end":{…},"label":"expected `]` here"},
//!  "secondary":[…],"notes":[…],"help":[…],
//!  "suggestions":[{"message":"insert `]`","edits":[{"start":{…},"end":{…},"replacement":"]"}]}]}
//! ```
//!
//! Offsets are bytes; lines and columns are 1-based, and columns count
//! characters. The format is a stable interface: fields may be added, but
//! existing fields keep their meaning.

use serde_json::{Value, json};

use crate::{Diagnostic, Label, SourceMap, Span};

pub(crate) fn render(d: &Diagnostic, path: &str, map: &SourceMap) -> String {
    let label = |l: &Label| {
        let mut value = range(l.span, map);
        value["label"] = json!(l.message);
        value
    };
    json!({
        "code": d.code.id(),
        "message": d.message,
        "file": path,
        "primary": label(&d.primary),
        "secondary": d.secondary.iter().map(label).collect::<Vec<_>>(),
        "notes": d.notes,
        "help": d.help,
        "suggestions": d.suggestions.iter().map(|s| json!({
            "message": s.message,
            "edits": s.edits.iter().map(|e| {
                let mut value = range(e.span, map);
                value["replacement"] = json!(e.replacement);
                value
            }).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
    })
    .to_string()
}

fn range(span: Span, map: &SourceMap) -> Value {
    let position = |offset| {
        let at = map.line_col(offset);
        json!({ "offset": offset, "line": at.line, "column": at.col })
    };
    json!({ "start": position(span.start), "end": position(span.end) })
}

#[cfg(test)]
mod tests {
    use crate::{Code, Diagnostic, Edit, Span};

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
            serde_json::from_str(&d.to_json("f.trendy", "a\nbc")).unwrap();
        assert_eq!(
            value,
            serde_json::json!({
                "code": "P0002",
                "message": "msg",
                "file": "f.trendy",
                "primary": {
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
                        "start": { "offset": 3, "line": 2, "column": 2 },
                        "end": { "offset": 3, "line": 2, "column": 2 },
                        "replacement": ";",
                    }],
                }],
            })
        );
    }
}
