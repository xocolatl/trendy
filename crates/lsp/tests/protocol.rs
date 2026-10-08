//! Drives the `trendy-lsp` binary over stdio, as an editor would.

use std::io::{BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use lsp_server::{Message, Notification, Request, RequestId, Response};
use serde_json::{Value, json};

const URI: &str = "file:///project/main.trendy";

struct Client {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: i32,
}

impl Client {
    fn start() -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_trendy-lsp"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        let mut client = Client {
            child,
            stdin,
            stdout,
            next_id: 0,
        };
        let result = client.request("initialize", json!({ "capabilities": {} }));
        assert_eq!(result["capabilities"]["documentFormattingProvider"], true);
        assert_eq!(result["capabilities"]["codeActionProvider"], true);
        assert_eq!(
            result["capabilities"]["semanticTokensProvider"]["legend"]["tokenTypes"],
            json!([
                "keyword", "function", "variable", "type", "number", "string", "property"
            ])
        );
        client.notify("initialized", json!({}));
        client
    }

    fn send(&mut self, message: &Message) {
        message.write(&mut self.stdin).unwrap();
        self.stdin.flush().unwrap();
    }

    fn receive(&mut self) -> Message {
        Message::read(&mut self.stdout).unwrap().unwrap()
    }

    fn notify(&mut self, method: &str, params: Value) {
        self.send(&Notification::new(method.to_string(), params).into());
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        self.next_id += 1;
        let id = RequestId::from(self.next_id);
        self.send(&Request::new(id.clone(), method.to_string(), params).into());
        match self.receive() {
            Message::Response(Response {
                id: got,
                response_result: Ok(result),
            }) if got == id => result,
            other => panic!("expected response to {method}, got {other:?}"),
        }
    }

    /// The next `publishDiagnostics` notification's diagnostics.
    fn diagnostics(&mut self) -> Value {
        match self.receive() {
            Message::Notification(n) if n.method == "textDocument/publishDiagnostics" => {
                assert_eq!(n.params["uri"], URI);
                n.params["diagnostics"].clone()
            }
            other => panic!("expected diagnostics, got {other:?}"),
        }
    }

    fn shutdown(mut self) {
        self.request("shutdown", Value::Null);
        self.notify("exit", Value::Null);
        assert!(self.child.wait().unwrap().success());
    }
}

fn open(client: &mut Client, text: &str) {
    client.notify(
        "textDocument/didOpen",
        json!({ "textDocument": {
            "uri": URI, "languageId": "trendy", "version": 1, "text": text,
        }}),
    );
}

#[test]
fn diagnostics_follow_edits() {
    let mut client = Client::start();

    open(&mut client, "operator main{} [];");
    assert_eq!(client.diagnostics(), json!([]));

    client.notify(
        "textDocument/didChange",
        json!({
            "textDocument": { "uri": URI, "version": 2 },
            "contentChanges": [{ "text": "operator main{}\n  [;" }],
        }),
    );
    let diagnostics = client.diagnostics();
    assert_eq!(diagnostics[0]["code"], "E0002");
    assert!(
        diagnostics[0]["message"]
            .as_str()
            .unwrap()
            .starts_with("expected `]`, found `;`\n")
    );
    assert_eq!(
        diagnostics[0]["range"]["start"],
        json!({ "line": 1, "character": 3 })
    );

    client.notify(
        "textDocument/didClose",
        json!({ "textDocument": { "uri": URI } }),
    );
    assert_eq!(client.diagnostics(), json!([]));

    client.shutdown();
}

#[test]
fn formatting() {
    let mut client = Client::start();
    open(&mut client, "operator  main {} [] ;");
    client.diagnostics();

    let edits = client.request(
        "textDocument/formatting",
        json!({
            "textDocument": { "uri": URI },
            "options": { "tabSize": 4, "insertSpaces": true },
        }),
    );
    assert_eq!(
        edits,
        json!([{
            "range": {
                "start": { "line": 0, "character": 0 },
                "end": { "line": 0, "character": 22 },
            },
            "newText": "operator main{} [];\n",
        }])
    );

    client.shutdown();
}

#[test]
fn quick_fix() {
    let mut client = Client::start();
    open(&mut client, "operator main{} []");
    client.diagnostics();

    let actions = client.request(
        "textDocument/codeAction",
        json!({
            "textDocument": { "uri": URI },
            "range": {
                "start": { "line": 0, "character": 18 },
                "end": { "line": 0, "character": 18 },
            },
            "context": { "diagnostics": [] },
        }),
    );
    assert_eq!(actions[0]["title"], "insert `;`");
    assert_eq!(actions[0]["kind"], "quickfix");
    assert_eq!(
        actions[0]["edit"]["changes"][URI],
        json!([{
            "range": {
                "start": { "line": 0, "character": 18 },
                "end": { "line": 0, "character": 18 },
            },
            "newText": ";",
        }])
    );

    client.shutdown();
}

#[test]
fn semantic_tokens() {
    let mut client = Client::start();
    open(&mut client, "operator main{} [];");
    client.diagnostics();

    let tokens = client.request(
        "textDocument/semanticTokens/full",
        json!({ "textDocument": { "uri": URI } }),
    );
    // `operator` (keyword) at 0:0, then `main` (function, declaration and
    // definition) 9 columns later.
    assert_eq!(tokens["data"], json!([0, 0, 8, 0, 0, 0, 9, 4, 1, 3]));

    client.shutdown();
}
