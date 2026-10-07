//! Language server for trendy, speaking LSP over stdin and stdout.

mod analysis;
mod highlight;
mod position;

use std::collections::HashMap;
use std::error::Error;

use lsp_server::{Connection, ErrorCode, Message, Notification, Request, Response};
use lsp_types::notification::{
    DidChangeTextDocument, DidCloseTextDocument, DidOpenTextDocument,
    Notification as NotificationTrait, PublishDiagnostics,
};
use lsp_types::request::{
    CodeActionRequest, Formatting, Request as RequestTrait, SemanticTokensFullRequest,
};
use lsp_types::{
    CodeActionParams, CodeActionProviderCapability, DocumentFormattingParams, OneOf,
    PublishDiagnosticsParams, SemanticTokens, SemanticTokensFullOptions, SemanticTokensOptions,
    SemanticTokensParams, SemanticTokensResult, SemanticTokensServerCapabilities,
    ServerCapabilities, TextDocumentSyncCapability, TextDocumentSyncKind, Uri,
};

type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;

fn main() -> Result<()> {
    let (connection, io_threads) = Connection::stdio();
    let capabilities = ServerCapabilities {
        text_document_sync: Some(TextDocumentSyncCapability::Kind(TextDocumentSyncKind::FULL)),
        document_formatting_provider: Some(OneOf::Left(true)),
        code_action_provider: Some(CodeActionProviderCapability::Simple(true)),
        semantic_tokens_provider: Some(SemanticTokensServerCapabilities::SemanticTokensOptions(
            SemanticTokensOptions {
                legend: analysis::semantic_tokens_legend(),
                full: Some(SemanticTokensFullOptions::Bool(true)),
                ..SemanticTokensOptions::default()
            },
        )),
        ..ServerCapabilities::default()
    };
    connection.initialize(serde_json::to_value(capabilities)?)?;

    Server {
        connection: &connection,
        documents: HashMap::new(),
    }
    .run()?;

    drop(connection);
    io_threads.join()?;
    Ok(())
}

struct Server<'a> {
    connection: &'a Connection,
    /// Open documents, kept in sync with the editor.
    documents: HashMap<Uri, String>,
}

impl Server<'_> {
    fn run(&mut self) -> Result<()> {
        for message in &self.connection.receiver {
            match message {
                Message::Request(request) => {
                    if self.connection.handle_shutdown(&request)? {
                        return Ok(());
                    }
                    self.request(request)?;
                }
                Message::Notification(notification) => self.notification(notification)?,
                Message::Response(_) => {}
            }
        }
        Ok(())
    }

    fn request(&self, request: Request) -> Result<()> {
        let response = if request.method == Formatting::METHOD {
            let (id, params) = request.extract::<DocumentFormattingParams>(Formatting::METHOD)?;
            let edits = self
                .documents
                .get(&params.text_document.uri)
                .and_then(|text| analysis::format(text));
            Response::new_ok(id, edits)
        } else if request.method == SemanticTokensFullRequest::METHOD {
            let (id, params) =
                request.extract::<SemanticTokensParams>(SemanticTokensFullRequest::METHOD)?;
            let tokens = self.documents.get(&params.text_document.uri).map(|text| {
                SemanticTokensResult::Tokens(SemanticTokens {
                    result_id: None,
                    data: analysis::semantic_tokens(text),
                })
            });
            Response::new_ok(id, tokens)
        } else if request.method == CodeActionRequest::METHOD {
            let (id, params) = request.extract::<CodeActionParams>(CodeActionRequest::METHOD)?;
            let uri = &params.text_document.uri;
            let actions = self
                .documents
                .get(uri)
                .map(|text| analysis::code_actions(uri, text, params.range));
            Response::new_ok(id, actions)
        } else {
            Response::new_err(
                request.id,
                ErrorCode::MethodNotFound as i32,
                format!("unhandled method `{}`", request.method),
            )
        };
        self.connection.sender.send(response.into())?;
        Ok(())
    }

    fn notification(&mut self, notification: Notification) -> Result<()> {
        match notification.method.as_str() {
            DidOpenTextDocument::METHOD => {
                let params = notification
                    .extract::<lsp_types::DidOpenTextDocumentParams>(DidOpenTextDocument::METHOD)?;
                let doc = params.text_document;
                self.update(doc.uri, doc.text, Some(doc.version))
            }
            DidChangeTextDocument::METHOD => {
                let params = notification.extract::<lsp_types::DidChangeTextDocumentParams>(
                    DidChangeTextDocument::METHOD,
                )?;
                // With full sync, the last change holds the entire new text.
                let Some(change) = params.content_changes.into_iter().next_back() else {
                    return Ok(());
                };
                let doc = params.text_document;
                self.update(doc.uri, change.text, Some(doc.version))
            }
            DidCloseTextDocument::METHOD => {
                let params = notification.extract::<lsp_types::DidCloseTextDocumentParams>(
                    DidCloseTextDocument::METHOD,
                )?;
                let uri = params.text_document.uri;
                self.documents.remove(&uri);
                self.publish(uri, Vec::new(), None)
            }
            _ => Ok(()),
        }
    }

    fn update(&mut self, uri: Uri, text: String, version: Option<i32>) -> Result<()> {
        let diagnostics = analysis::diagnostics(&uri, &text);
        self.documents.insert(uri.clone(), text);
        self.publish(uri, diagnostics, version)
    }

    fn publish(
        &self,
        uri: Uri,
        diagnostics: Vec<lsp_types::Diagnostic>,
        version: Option<i32>,
    ) -> Result<()> {
        let params = PublishDiagnosticsParams::new(uri, diagnostics, version);
        let notification = Notification::new(PublishDiagnostics::METHOD.to_string(), params);
        self.connection.sender.send(notification.into())?;
        Ok(())
    }
}
