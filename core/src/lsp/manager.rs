//! Synchronous LSP manager used by both frontends.
//!
//! `LspManager` owns a Tokio runtime internally and exposes a plain
//! blocking API. The frontends call `poll()` once per frame to drain
//! incoming messages and apply debounced document changes.

use std::collections::{BTreeMap, HashMap};
use std::time::{Duration, Instant};

use lsp_types::{
    ClientCapabilities, ClientInfo, CodeActionContext, CodeActionParams, CodeActionResponse,
    CodeActionTriggerKind, Command as LspCommand, CompletionList, CompletionParams, Diagnostic,
    DidChangeTextDocumentParams, DidCloseTextDocumentParams, DidOpenTextDocumentParams,
    DidSaveTextDocumentParams, DocumentSymbolParams, ExecuteCommandParams, GotoDefinitionResponse,
    Hover, HoverContents, InitializeParams, MarkedString, Position, Range, RenameParams,
    ServerCapabilities, TextDocumentContentChangeEvent, TextDocumentIdentifier, TextDocumentItem,
    TextDocumentPositionParams, VersionedTextDocumentIdentifier, WorkspaceEdit, WorkspaceFolder,
};
use tokio::runtime::Runtime;
use tokio::sync::mpsc;
use url::Url;

use super::client::LspClient;
use super::protocol::{Id, Message};

const DEBOUNCE_MS: u64 = 300;
const SPAWN_RETRY_COOLDOWN: Duration = Duration::from_secs(5);

/// Snapshot of the language server associated with a document.
#[derive(Debug, Clone)]
pub struct ServerStatus {
    /// The LSP language identifier (e.g. `"rust"`).
    pub language_id: String,
    /// Whether the server process has been spawned and is still tracked
    /// by the manager. `true` means running or initializing; `false`
    /// means the document was opened but the server is not present
    /// (usually a spawn failure).
    pub running: bool,
}

/// Synchronous manager for all LSP servers.
pub struct LspManager {
    runtime: Runtime,
    servers: HashMap<ServerKey, ServerState>,
    docs: HashMap<Url, DocumentState>,
    pending: HashMap<u64, PendingRequest>,
    diagnostics: HashMap<Url, Vec<Diagnostic>>,
    /// Per-line max-severity index maintained alongside `diagnostics` so
    /// the GUI can resolve a gutter stripe in O(1) instead of folding
    /// every diagnostic into every visible line, every frame.
    diag_severity: HashMap<Url, BTreeMap<usize, lsp_types::DiagnosticSeverity>>,
    completion_results: HashMap<Url, CompletionList>,
    hover_results: HashMap<Url, (Hover, Position)>,
    definition_results: HashMap<Url, (GotoDefinitionResponse, Position)>,
    rename_results: HashMap<Url, (WorkspaceEdit, Position)>,
    formatting_results: HashMap<Url, Vec<lsp_types::TextEdit>>,
    symbol_results: HashMap<Url, Vec<lsp_types::DocumentSymbol>>,
    code_action_results: HashMap<Url, CodeActionResponse>,
    pending_change: Option<(Url, Instant, String)>,
    last_status: Option<String>,
    server_spawn_failures: HashMap<ServerKey, Instant>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ServerKey {
    root_uri: Url,
    language_id: String,
}

struct ServerState {
    client: LspClient,
    in_rx: mpsc::UnboundedReceiver<Message>,
    capabilities: Option<ServerCapabilities>,
    init_id: Option<u64>,
    initialized: bool,
}

impl ServerState {
    fn init_id_matches(&self, id: &Id) -> bool {
        match (self.init_id, id) {
            (Some(n), Id::Number(m)) => n == *m,
            _ => false,
        }
    }
}

struct DocumentState {
    version: i32,
    server_key: ServerKey,
    /// Latest full text of the document. Kept so we can re-send
    /// `textDocument/didOpen` if the language server restarts.
    text: String,
}

#[derive(Debug)]
enum PendingRequest {
    Completion(Url),
    Hover(Url, Position),
    Definition(Url, Position),
    Rename(Url, Position),
    Formatting(Url),
    DocumentSymbols(Url),
    CodeActions(Url),
    ExecuteCommand,
}

impl Default for LspManager {
    fn default() -> Self {
        Self::new()
    }
}

impl LspManager {
    pub fn new() -> Self {
        let runtime = Runtime::new().expect("tokio runtime");
        LspManager {
            runtime,
            servers: HashMap::new(),
            docs: HashMap::new(),
            pending: HashMap::new(),
            diagnostics: HashMap::new(),
            diag_severity: HashMap::new(),
            completion_results: HashMap::new(),
            hover_results: HashMap::new(),
            definition_results: HashMap::new(),
            rename_results: HashMap::new(),
            formatting_results: HashMap::new(),
            symbol_results: HashMap::new(),
            code_action_results: HashMap::new(),
            pending_change: None,
            last_status: None,
            server_spawn_failures: HashMap::new(),
        }
    }

    /// Return the last status message (error or info) and clear it.
    pub fn take_status(&mut self) -> Option<String> {
        self.last_status.take()
    }

    /// True if the manager knows how to start a server for this language.
    pub fn is_language_supported(language_id: &str) -> bool {
        server_command(language_id).is_some()
    }

    /// Human-friendly display name for a supported language server, if any.
    pub fn server_display_name(language_id: &str) -> Option<&'static str> {
        match language_id {
            "rust" => Some("rust-analyzer"),
            "go" => Some("gopls"),
            "javascript" | "javascriptreact" | "typescript" | "typescriptreact" => {
                Some("typescript-language-server")
            }
            "python" => Some("pylsp"),
            "c" | "cpp" => Some("clangd"),
            "shellscript" => Some("bash-language-server"),
            "toml" => Some("taplo"),
            "html" => Some("vscode-html-language-server"),
            "css" => Some("vscode-css-language-server"),
            _ => None,
        }
    }

    /// Status of the language server for the given document.
    pub fn document_server_status(&self, uri: &Url) -> Option<ServerStatus> {
        let doc = self.docs.get(uri)?;
        let running = self.servers.contains_key(&doc.server_key);
        Some(ServerStatus {
            language_id: doc.server_key.language_id.clone(),
            running,
        })
    }

    /// Open a document on its language server. Spawns the server if this
    /// is the first document for `(root_uri, language_id)`, and restarts
    /// it if the previous process died.
    pub fn open_document(&mut self, uri: Url, language_id: String, root_uri: Url, text: String) {
        let Some(command) = server_command(&language_id) else {
            return;
        };

        let key = ServerKey {
            root_uri,
            language_id,
        };

        if let Some(doc) = self.docs.get_mut(&uri) {
            // Keep the stored text in sync so a server restart can re-open
            // the document with the latest content.
            doc.text = text.clone();
            self.ensure_server(&key, command);
            return;
        }

        let version = 1;
        self.docs.insert(
            uri.clone(),
            DocumentState {
                version,
                server_key: key.clone(),
                text: text.clone(),
            },
        );

        if self.ensure_server(&key, command) {
            if let Some(server) = self.servers.get(&key) {
                // Only send didOpen if the server is fully initialized.
                // rust-analyzer exits if it receives didOpen before
                // the initialized notification. If not ready yet, the
                // doc is stored and will be opened when initialized fires.
                if server.initialized {
                    server.client.notify(
                        "textDocument/didOpen",
                        serde_json::to_value(DidOpenTextDocumentParams {
                            text_document: TextDocumentItem {
                                uri,
                                language_id: key.language_id.clone(),
                                version,
                                text,
                            },
                        })
                        .unwrap_or_default(),
                    );
                }
            }
        }
    }

    /// Ensure a server for `key` is running, (re)spawning it if necessary.
    /// Returns `true` if a running server is available when the call ends.
    /// On a successful respawn, all tracked documents for this server are
    /// re-opened so the new process starts in sync.
    fn ensure_server(&mut self, key: &ServerKey, command: Vec<String>) -> bool {
        let alive = self
            .servers
            .get_mut(key)
            .map(|state| state.client.is_alive())
            .unwrap_or(false);
        if alive {
            return true;
        }

        if self.servers.contains_key(key) {
            let name =
                LspManager::server_display_name(&key.language_id).unwrap_or(&key.language_id);
            self.last_status = Some(format!("{name} stopped; restarting…"));
            self.servers.remove(key);
        }

        if let Some(&last_fail) = self.server_spawn_failures.get(key) {
            if last_fail.elapsed() < SPAWN_RETRY_COOLDOWN {
                return false;
            }
        }

        match self.runtime.block_on(LspClient::spawn(&command)) {
            Ok((client, in_rx)) => {
                self.server_spawn_failures.remove(key);
                let folder_name = key
                    .root_uri
                    .path_segments()
                    .and_then(|mut s| s.next_back())
                    .unwrap_or("")
                    .to_string();
                #[allow(deprecated)]
                let init_params = serde_json::to_value(InitializeParams {
                    process_id: Some(std::process::id()),
                    client_info: Some(ClientInfo {
                        name: "Skerry".to_string(),
                        version: Some(env!("CARGO_PKG_VERSION").to_string()),
                    }),
                    root_uri: None,
                    capabilities: ClientCapabilities::default(),
                    workspace_folders: Some(vec![WorkspaceFolder {
                        uri: key.root_uri.clone(),
                        name: folder_name,
                    }]),
                    ..Default::default()
                })
                .unwrap_or_default();
                let init_id = client.request("initialize", init_params);
                self.servers.insert(
                    key.clone(),
                    ServerState {
                        client,
                        in_rx,
                        capabilities: None,
                        init_id: Some(init_id),
                        initialized: false,
                    },
                );
                self.reopen_documents_for_server(key);
                true
            }
            Err(e) => {
                self.server_spawn_failures
                    .insert(key.clone(), Instant::now());
                self.last_status = Some(format!("LSP spawn error: {e}"));
                false
            }
        }
    }

    /// Re-send `textDocument/didOpen` for every document tied to `key`.
    /// Used after a server restart so the new process has the current
    /// buffers.
    fn reopen_documents_for_server(&mut self, key: &ServerKey) {
        // Only send didOpen if the server is initialized. Otherwise the
        // docs will be opened when the initialized notification fires.
        let Some(server) = self.servers.get(key) else {
            return;
        };
        if !server.initialized {
            return;
        }
        let docs_to_open: Vec<(Url, i32, String)> = self
            .docs
            .iter()
            .filter(|(_, doc)| doc.server_key == *key)
            .map(|(uri, doc)| (uri.clone(), doc.version, doc.text.clone()))
            .collect();
        for (uri, version, text) in docs_to_open {
            server.client.notify(
                "textDocument/didOpen",
                serde_json::to_value(DidOpenTextDocumentParams {
                    text_document: TextDocumentItem {
                        uri,
                        language_id: key.language_id.clone(),
                        version,
                        text,
                    },
                })
                .unwrap_or_default(),
            );
        }
    }

    /// Queue a full-text change notification. The change is sent after a
    /// short debounce to avoid spamming the server on every keystroke.
    ///
    /// Calling this repeatedly with the same text does not push back the
    /// deadline; calling it with new text resets the debounce so the
    /// server receives the latest content once the user stops typing.
    pub fn change_document(&mut self, uri: Url, text: String) {
        if let Some(doc) = self.docs.get_mut(&uri) {
            doc.text = text.clone();
        } else {
            return;
        }
        if let Some((pending_uri, deadline, pending_text)) = self.pending_change.as_mut() {
            if *pending_uri == uri {
                if *pending_text != text {
                    *pending_text = text;
                    *deadline = Instant::now() + Duration::from_millis(DEBOUNCE_MS);
                }
                return;
            }
        }
        let deadline = Instant::now() + Duration::from_millis(DEBOUNCE_MS);
        self.pending_change = Some((uri, deadline, text));
    }

    /// Send a `textDocument/didSave` notification.
    pub fn save_document(&mut self, uri: &Url) {
        let Some(doc) = self.docs.get(uri) else {
            return;
        };
        if let Some(server) = self.servers.get(&doc.server_key) {
            server.client.notify(
                "textDocument/didSave",
                serde_json::to_value(DidSaveTextDocumentParams {
                    text_document: TextDocumentIdentifier { uri: uri.clone() },
                    text: None,
                })
                .unwrap_or_default(),
            );
        }
    }

    /// Close a document and notify the server.
    pub fn close_document(&mut self, uri: &Url) {
        let Some(doc) = self.docs.remove(uri) else {
            return;
        };
        self.diagnostics.remove(uri);
        self.diag_severity.remove(uri);
        self.completion_results.remove(uri);
        self.hover_results.remove(uri);
        self.definition_results.remove(uri);
        self.rename_results.remove(uri);
        self.formatting_results.remove(uri);
        self.symbol_results.remove(uri);
        self.code_action_results.remove(uri);
        if let Some(server) = self.servers.get(&doc.server_key) {
            server.client.notify(
                "textDocument/didClose",
                serde_json::to_value(DidCloseTextDocumentParams {
                    text_document: TextDocumentIdentifier { uri: uri.clone() },
                })
                .unwrap_or_default(),
            );
        }
    }

    /// Diagnostics most recently published for this document.
    pub fn diagnostics(&self, uri: &Url) -> &[Diagnostic] {
        self.diagnostics
            .get(uri)
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    /// Most severe diagnostic on `line` for this document, if any.
    ///
    /// Served from the per-line index rebuilt on every
    /// `publishDiagnostics`, so callers pay O(1) per line instead of
    /// scanning the full diagnostic list.
    pub fn diagnostic_severity_on_line(
        &self,
        uri: &Url,
        line: usize,
    ) -> Option<lsp_types::DiagnosticSeverity> {
        self.diag_severity
            .get(uri)
            .and_then(|by_line| by_line.get(&line).copied())
    }

    /// Peek at the most recently received completion list for a URI.
    pub fn completion_result(&self, uri: &Url) -> Option<&CompletionList> {
        self.completion_results.get(uri)
    }

    /// Request completions at the given position. The result will be
    /// available after the server responds and the next `poll()` drains
    /// the channel.
    pub fn request_completion(&mut self, uri: &Url, position: Position) -> Option<&CompletionList> {
        let doc = self.docs.get(uri)?;
        let server = self.servers.get(&doc.server_key)?;
        if self
            .pending
            .values()
            .any(|p| matches!(p, PendingRequest::Completion(u) if u == uri))
        {
            return self.completion_results.get(uri);
        }
        let id = server.client.request(
            "textDocument/completion",
            serde_json::to_value(CompletionParams {
                text_document_position: TextDocumentPositionParams {
                    text_document: TextDocumentIdentifier { uri: uri.clone() },
                    position,
                },
                work_done_progress_params: Default::default(),
                partial_result_params: Default::default(),
                context: None,
            })
            .unwrap_or_default(),
        );
        self.pending
            .insert(id, PendingRequest::Completion(uri.clone()));
        self.completion_results.get(uri)
    }

    /// Peek at the most recently received hover for a URI and position.
    pub fn hover_result(&self, uri: &Url, position: Position) -> Option<&Hover> {
        self.hover_results
            .get(uri)
            .and_then(|(hover, pos)| if *pos == position { Some(hover) } else { None })
    }

    /// Request hover at the given position.
    pub fn request_hover(&mut self, uri: &Url, position: Position) -> Option<&Hover> {
        let doc = self.docs.get(uri)?;
        let server = self.servers.get(&doc.server_key)?;
        if self
            .pending
            .values()
            .any(|p| matches!(p, PendingRequest::Hover(u, pos) if u == uri && *pos == position))
        {
            return self.hover_result(uri, position);
        }
        let id = server.client.request(
            "textDocument/hover",
            serde_json::to_value(lsp_types::HoverParams {
                text_document_position_params: TextDocumentPositionParams {
                    text_document: TextDocumentIdentifier { uri: uri.clone() },
                    position,
                },
                work_done_progress_params: Default::default(),
            })
            .unwrap_or_default(),
        );
        self.pending
            .insert(id, PendingRequest::Hover(uri.clone(), position));
        self.hover_result(uri, position)
    }

    /// Peek at the most recently received go-to-definition result for a URI and position.
    pub fn definition_result(
        &self,
        uri: &Url,
        position: Position,
    ) -> Option<&GotoDefinitionResponse> {
        self.definition_results
            .get(uri)
            .and_then(|(def, pos)| if *pos == position { Some(def) } else { None })
    }

    /// Request go-to-definition at the given position.
    pub fn request_definition(
        &mut self,
        uri: &Url,
        position: Position,
    ) -> Option<&GotoDefinitionResponse> {
        let doc = self.docs.get(uri)?;
        let server = self.servers.get(&doc.server_key)?;
        if self.pending.values().any(
            |p| matches!(p, PendingRequest::Definition(u, pos) if u == uri && *pos == position),
        ) {
            return self.definition_result(uri, position);
        }
        let id = server.client.request(
            "textDocument/definition",
            serde_json::to_value(lsp_types::GotoDefinitionParams {
                text_document_position_params: TextDocumentPositionParams {
                    text_document: TextDocumentIdentifier { uri: uri.clone() },
                    position,
                },
                work_done_progress_params: Default::default(),
                partial_result_params: Default::default(),
            })
            .unwrap_or_default(),
        );
        self.pending
            .insert(id, PendingRequest::Definition(uri.clone(), position));
        self.definition_result(uri, position)
    }

    /// Extract a single target location from a go-to-definition response.
    pub fn definition_target(resp: &GotoDefinitionResponse) -> Option<(Url, Position)> {
        match resp {
            GotoDefinitionResponse::Scalar(loc) => Some((loc.uri.clone(), loc.range.start)),
            GotoDefinitionResponse::Array(locs) => {
                locs.first().map(|loc| (loc.uri.clone(), loc.range.start))
            }
            GotoDefinitionResponse::Link(links) => links.first().map(|link| {
                let uri = link.target_uri.clone();
                let pos = link.target_range.start;
                (uri, pos)
            }),
        }
    }

    /// True if the server for `uri` advertises rename support.
    pub fn supports_rename(&self, uri: &Url) -> bool {
        let Some(doc) = self.docs.get(uri) else {
            return false;
        };
        let Some(server) = self.servers.get(&doc.server_key) else {
            return false;
        };
        let Some(caps) = &server.capabilities else {
            return false;
        };
        caps.rename_provider.is_some()
    }

    /// Request a rename of the symbol at `position` to `new_name`. The
    /// result lands asynchronously; poll [`Self::rename_result`].
    pub fn request_rename(
        &mut self,
        uri: &Url,
        position: Position,
        new_name: &str,
    ) -> Option<&WorkspaceEdit> {
        let doc = self.docs.get(uri)?;
        let server = self.servers.get(&doc.server_key)?;
        if self
            .pending
            .values()
            .any(|p| matches!(p, PendingRequest::Rename(u, pos) if u == uri && *pos == position))
        {
            return self.rename_result(uri, position);
        }
        let id = server.client.request(
            "textDocument/rename",
            serde_json::to_value(RenameParams {
                text_document_position: TextDocumentPositionParams {
                    text_document: TextDocumentIdentifier { uri: uri.clone() },
                    position,
                },
                new_name: new_name.to_string(),
                work_done_progress_params: Default::default(),
            })
            .unwrap_or_default(),
        );
        self.pending
            .insert(id, PendingRequest::Rename(uri.clone(), position));
        self.rename_result(uri, position)
    }

    /// Peek at the most recently received rename edit for a URI + position.
    pub fn rename_result(&self, uri: &Url, position: Position) -> Option<&WorkspaceEdit> {
        self.rename_results
            .get(uri)
            .and_then(|(edit, pos)| if *pos == position { Some(edit) } else { None })
    }

    /// Consume the rename result for `uri` so it isn't applied twice.
    pub fn take_rename_result(&mut self, uri: &Url) -> Option<WorkspaceEdit> {
        self.rename_results.remove(uri).map(|(edit, _)| edit)
    }

    /// True if the server for `uri` advertises document formatting support.
    pub fn supports_formatting(&self, uri: &Url) -> bool {
        let Some(doc) = self.docs.get(uri) else {
            return false;
        };
        let Some(server) = self.servers.get(&doc.server_key) else {
            return false;
        };
        let Some(caps) = &server.capabilities else {
            return false;
        };
        caps.document_formatting_provider.is_some()
    }

    /// Request full-document formatting. The result lands asynchronously.
    pub fn request_formatting(&mut self, uri: &Url) {
        let Some(doc) = self.docs.get(uri) else {
            return;
        };
        let Some(server) = self.servers.get(&doc.server_key) else {
            return;
        };
        // Don't stack multiple formatting requests for the same doc.
        if self
            .pending
            .values()
            .any(|p| matches!(p, PendingRequest::Formatting(u) if u == uri))
        {
            return;
        }
        let id = server.client.request(
            "textDocument/formatting",
            serde_json::to_value(lsp_types::DocumentFormattingParams {
                text_document: TextDocumentIdentifier { uri: uri.clone() },
                options: lsp_types::FormattingOptions {
                    tab_size: 4,
                    insert_spaces: true,
                    ..Default::default()
                },
                work_done_progress_params: Default::default(),
            })
            .unwrap_or_default(),
        );
        self.pending
            .insert(id, PendingRequest::Formatting(uri.clone()));
    }

    /// Peek at formatting edits if they've arrived.
    pub fn formatting_result(&self, uri: &Url) -> Option<&Vec<lsp_types::TextEdit>> {
        self.formatting_results.get(uri)
    }

    /// Consume the formatting result for `uri`.
    pub fn take_formatting_result(&mut self, uri: &Url) -> Option<Vec<lsp_types::TextEdit>> {
        self.formatting_results.remove(uri)
    }

    /// Store formatting edits from an external (non-LSP) formatter so
    /// the frontend's `apply_pending_format` picks them up through the
    /// same pipeline as LSP formatting results.
    pub fn store_formatting_result(&mut self, uri: &Url, edits: Vec<lsp_types::TextEdit>) {
        self.formatting_results.insert(uri.clone(), edits);
    }

    /// Request document symbols (outline) for `uri`.
    pub fn request_document_symbols(&mut self, uri: &Url) {
        let Some(doc) = self.docs.get(uri) else {
            return;
        };
        let Some(server) = self.servers.get(&doc.server_key) else {
            return;
        };
        if self
            .pending
            .values()
            .any(|p| matches!(p, PendingRequest::DocumentSymbols(u) if u == uri))
        {
            return;
        }
        let id = server.client.request(
            "textDocument/documentSymbol",
            serde_json::to_value(DocumentSymbolParams {
                text_document: TextDocumentIdentifier { uri: uri.clone() },
                work_done_progress_params: Default::default(),
                partial_result_params: Default::default(),
            })
            .unwrap_or_default(),
        );
        self.pending
            .insert(id, PendingRequest::DocumentSymbols(uri.clone()));
    }

    /// Peek at document symbols if they've arrived.
    pub fn document_symbol_result(&self, uri: &Url) -> Option<&Vec<lsp_types::DocumentSymbol>> {
        self.symbol_results.get(uri)
    }

    /// Consume the document symbols for `uri`.
    pub fn take_document_symbol_result(
        &mut self,
        uri: &Url,
    ) -> Option<Vec<lsp_types::DocumentSymbol>> {
        self.symbol_results.remove(uri)
    }

    /// True if the server for `uri` advertises code-action support.
    pub fn supports_code_actions(&self, uri: &Url) -> bool {
        let Some(doc) = self.docs.get(uri) else {
            return false;
        };
        let Some(server) = self.servers.get(&doc.server_key) else {
            return false;
        };
        let Some(caps) = &server.capabilities else {
            return false;
        };
        caps.code_action_provider.is_some()
    }

    /// Request code actions (quick fixes) at the given position. The
    /// result lands asynchronously; poll [`Self::code_action_result`].
    /// The stored diagnostics for the document are passed as context so
    /// servers can offer fixes for errors on the line.
    pub fn request_code_action(
        &mut self,
        uri: &Url,
        position: Position,
    ) -> Option<&CodeActionResponse> {
        let doc = self.docs.get(uri)?;
        let server = self.servers.get(&doc.server_key)?;
        if self
            .pending
            .values()
            .any(|p| matches!(p, PendingRequest::CodeActions(u) if u == uri))
        {
            return self.code_action_result(uri);
        }
        let id = server.client.request(
            "textDocument/codeAction",
            serde_json::to_value(CodeActionParams {
                text_document: TextDocumentIdentifier { uri: uri.clone() },
                range: Range {
                    start: position,
                    end: position,
                },
                context: CodeActionContext {
                    diagnostics: self.diagnostics(uri).to_vec(),
                    only: None,
                    trigger_kind: Some(CodeActionTriggerKind::INVOKED),
                },
                work_done_progress_params: Default::default(),
                partial_result_params: Default::default(),
            })
            .unwrap_or_default(),
        );
        self.pending
            .insert(id, PendingRequest::CodeActions(uri.clone()));
        self.code_action_result(uri)
    }

    /// Peek at the code actions for `uri` if they've arrived.
    pub fn code_action_result(&self, uri: &Url) -> Option<&CodeActionResponse> {
        self.code_action_results.get(uri)
    }

    /// Consume the code actions for `uri`.
    pub fn take_code_action_result(&mut self, uri: &Url) -> Option<CodeActionResponse> {
        self.code_action_results.remove(uri)
    }

    /// Run a server command (used for code actions that carry a command
    /// instead of an edit). The response carries no payload we need, so
    /// nothing is stored; the pending entry is simply retired when the
    /// response arrives.
    ///
    /// Limitation: command-only actions are best-effort. If the server
    /// answers with a `workspace/applyEdit` request, it is dropped
    /// (server-initiated requests are not handled today), so the edit
    /// never lands. Edit-bearing actions — the common case — apply
    /// client-side and work fully.
    pub fn request_execute_command(&mut self, uri: &Url, command: &LspCommand) {
        let Some(doc) = self.docs.get(uri) else {
            return;
        };
        let Some(server) = self.servers.get(&doc.server_key) else {
            return;
        };
        let id = server.client.request(
            "workspace/executeCommand",
            serde_json::to_value(ExecuteCommandParams {
                command: command.command.clone(),
                arguments: command.arguments.clone().unwrap_or_default(),
                work_done_progress_params: Default::default(),
            })
            .unwrap_or_default(),
        );
        self.pending.insert(id, PendingRequest::ExecuteCommand);
    }

    /// Drain incoming messages and apply debounced changes. Frontends
    /// should call this once per frame.
    pub fn poll(&mut self) {
        // Send any debounced didChange.
        if let Some((uri, deadline, text)) = self.pending_change.take() {
            if Instant::now() >= deadline {
                if let Some(doc) = self.docs.get_mut(&uri) {
                    doc.version += 1;
                    if let Some(server) = self.servers.get(&doc.server_key) {
                        server.client.notify(
                            "textDocument/didChange",
                            serde_json::to_value(DidChangeTextDocumentParams {
                                text_document: VersionedTextDocumentIdentifier {
                                    uri: uri.clone(),
                                    version: doc.version,
                                },
                                content_changes: vec![TextDocumentContentChangeEvent {
                                    range: None,
                                    range_length: None,
                                    text,
                                }],
                            })
                            .unwrap_or_default(),
                        );
                    }
                }
            } else {
                self.pending_change = Some((uri, deadline, text));
            }
        }

        // Drain each server's incoming message channel. Collect first
        // so `handle_message` can mutate `self` without double borrows.
        let mut incoming = Vec::new();
        for (key, server) in self.servers.iter_mut() {
            while let Ok(msg) = server.in_rx.try_recv() {
                incoming.push((key.clone(), msg));
            }
        }
        for (key, msg) in incoming {
            self.handle_message(&key, msg);
        }

        // Reap dead server processes so `open_document` will respawn them
        // on the next frame.
        let mut dead_keys = Vec::new();
        for (key, server) in self.servers.iter_mut() {
            if !server.client.is_alive() {
                dead_keys.push(key.clone());
            }
        }
        for key in dead_keys {
            self.servers.remove(&key);
        }
    }

    fn handle_message(&mut self, key: &ServerKey, msg: Message) {
        match msg {
            Message::Response(resp) => {
                let is_init = self
                    .servers
                    .get(key)
                    .map(|s| s.init_id_matches(&resp.id))
                    .unwrap_or(false);
                if is_init {
                    if let Some(result) = resp.result {
                        if let Ok(init) =
                            serde_json::from_value::<lsp_types::InitializeResult>(result)
                        {
                            let server = self.servers.get_mut(key).unwrap();
                            server.capabilities = Some(init.capabilities);
                            server.initialized = true;
                            server.init_id = None;
                            server.client.notify(
                                "initialized",
                                serde_json::to_value(lsp_types::InitializedParams {})
                                    .unwrap_or_default(),
                            );
                            // Now that the server is initialized, send
                            // any pending didOpen notifications for
                            // documents on this server.
                            self.reopen_documents_for_server(key);
                        }
                    }
                } else if let Some(pending) = self.pending.remove(&id_number(&resp.id)) {
                    match pending {
                        PendingRequest::Completion(uri) => {
                            if let Some(result) = resp.result {
                                let list: CompletionList = if result.is_array() {
                                    CompletionList {
                                        is_incomplete: false,
                                        items: serde_json::from_value(result).unwrap_or_default(),
                                    }
                                } else {
                                    serde_json::from_value(result).unwrap_or_default()
                                };
                                self.completion_results.insert(uri, list);
                            }
                        }
                        PendingRequest::Hover(uri, position) => {
                            if let Some(result) = resp.result {
                                if let Ok(hover) = serde_json::from_value::<Hover>(result) {
                                    self.hover_results.insert(uri, (hover, position));
                                }
                            }
                        }
                        PendingRequest::Definition(uri, position) => {
                            if let Some(result) = resp.result {
                                if let Ok(def) =
                                    serde_json::from_value::<GotoDefinitionResponse>(result)
                                {
                                    self.definition_results.insert(uri, (def, position));
                                }
                            }
                        }
                        PendingRequest::Rename(uri, position) => {
                            if let Some(result) = resp.result {
                                if let Ok(edit) = serde_json::from_value::<WorkspaceEdit>(result) {
                                    self.rename_results.insert(uri, (edit, position));
                                }
                            }
                        }
                        PendingRequest::Formatting(uri) => {
                            if let Some(result) = resp.result {
                                let edits: Vec<lsp_types::TextEdit> =
                                    serde_json::from_value(result).unwrap_or_default();
                                self.formatting_results.insert(uri, edits);
                            }
                        }
                        PendingRequest::DocumentSymbols(uri) => {
                            if let Some(result) = resp.result {
                                // The response can be DocumentSymbol[] or
                                // SymbolInformation[]. We try DocumentSymbol
                                // first (hierarchical), fall back to flat.
                                let symbols: Vec<lsp_types::DocumentSymbol> =
                                    serde_json::from_value::<Vec<lsp_types::DocumentSymbol>>(
                                        result.clone(),
                                    )
                                    .or_else(|_| {
                                        let flat: Vec<lsp_types::SymbolInformation> =
                                            serde_json::from_value(result)?;
                                        Ok::<_, serde_json::Error>(
                                            flat.into_iter()
                                                .map(|si| lsp_types::DocumentSymbol {
                                                    name: si.name,
                                                    detail: None,
                                                    kind: si.kind,
                                                    tags: si.tags,
                                                    #[allow(deprecated)]
                                                    deprecated: si.deprecated,
                                                    range: si.location.range,
                                                    selection_range: si.location.range,
                                                    children: None,
                                                })
                                                .collect(),
                                        )
                                    })
                                    .unwrap_or_default();
                                self.symbol_results.insert(uri, symbols);
                            }
                        }
                        PendingRequest::CodeActions(uri) => {
                            if let Some(result) = resp.result {
                                let actions: CodeActionResponse =
                                    serde_json::from_value(result).unwrap_or_default();
                                self.code_action_results.insert(uri, actions);
                            }
                        }
                        PendingRequest::ExecuteCommand => {
                            // Response carries no payload we use; retired.
                        }
                    }
                }
            }
            Message::Notification(notif) => {
                if notif.method == "textDocument/publishDiagnostics" {
                    if let Ok(params) =
                        serde_json::from_value::<lsp_types::PublishDiagnosticsParams>(notif.params)
                    {
                        // Rebuild the per-line max-severity index.
                        let mut by_line: BTreeMap<usize, lsp_types::DiagnosticSeverity> =
                            BTreeMap::new();
                        for d in &params.diagnostics {
                            let Some(sev) = d.severity else {
                                continue;
                            };
                            let start = d.range.start.line as usize;
                            let end = d.range.end.line as usize;
                            if end < start {
                                continue;
                            }
                            for line in start..=end {
                                // Same overlap rule the GUI uses: a
                                // diagnostic ending at column 0 of its
                                // final line does not mark that line.
                                if line == end && d.range.end.character == 0 && start != end {
                                    continue;
                                }
                                let replaces = by_line
                                    .get(&line)
                                    .map_or(true, |cur| severity_rank(*cur) < severity_rank(sev));
                                if replaces {
                                    by_line.insert(line, sev);
                                }
                            }
                        }
                        let uri = params.uri;
                        self.diagnostics.insert(uri.clone(), params.diagnostics);
                        self.diag_severity.insert(uri, by_line);
                    }
                }
            }
            Message::Request(_) => {}
        }
    }
}

/// Severity ordering for the per-line max-severity map: higher wins.
fn severity_rank(sev: lsp_types::DiagnosticSeverity) -> u8 {
    match sev {
        lsp_types::DiagnosticSeverity::ERROR => 4,
        lsp_types::DiagnosticSeverity::WARNING => 3,
        lsp_types::DiagnosticSeverity::INFORMATION => 2,
        _ => 1,
    }
}

fn id_number(id: &Id) -> u64 {
    match id {
        Id::Number(n) => *n,
        Id::String(_) => 0,
    }
}

/// Return a plain-text representation of an LSP hover response.
pub fn hover_text(hover: &Hover) -> String {
    match &hover.contents {
        HoverContents::Scalar(MarkedString::String(s)) => s.clone(),
        HoverContents::Scalar(MarkedString::LanguageString(ls)) => ls.value.clone(),
        HoverContents::Array(parts) => parts
            .iter()
            .map(|part| match part {
                MarkedString::String(s) => s.clone(),
                MarkedString::LanguageString(ls) => ls.value.clone(),
            })
            .collect::<Vec<_>>()
            .join("\n"),
        HoverContents::Markup(content) => content.value.clone(),
    }
}

fn server_command(language_id: &str) -> Option<Vec<String>> {
    match language_id {
        "rust" => Some(vec!["rust-analyzer".to_string()]),
        "go" => Some(vec!["gopls".to_string()]),
        "javascript" | "javascriptreact" | "typescript" | "typescriptreact" => Some(vec![
            "typescript-language-server".to_string(),
            "--stdio".to_string(),
        ]),
        "python" => Some(vec!["pylsp".to_string()]),
        "c" | "cpp" => Some(vec!["clangd".to_string()]),
        "shellscript" => Some(vec![
            "bash-language-server".to_string(),
            "start".to_string(),
        ]),
        "toml" => Some(vec![
            "taplo".to_string(),
            "lsp".to_string(),
            "stdio".to_string(),
        ]),
        "html" => Some(vec![
            "vscode-html-language-server".to_string(),
            "--stdio".to_string(),
        ]),
        "css" => Some(vec![
            "vscode-css-language-server".to_string(),
            "--stdio".to_string(),
        ]),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lsp_types::{
        CodeActionOrCommand, Location, MarkedString, MarkupContent, MarkupKind, Position, Range,
        Url,
    };

    #[test]
    fn hover_text_extracts_plain_string() {
        let hover = Hover {
            contents: HoverContents::Scalar(MarkedString::String("hello".to_string())),
            range: None,
        };
        assert_eq!(hover_text(&hover), "hello");
    }

    #[test]
    fn hover_text_extracts_language_string_value() {
        let hover = Hover {
            contents: HoverContents::Scalar(MarkedString::LanguageString(
                lsp_types::LanguageString {
                    language: "rust".to_string(),
                    value: "let x = 1;".to_string(),
                },
            )),
            range: None,
        };
        assert_eq!(hover_text(&hover), "let x = 1;");
    }

    #[test]
    fn hover_text_extracts_markup_content() {
        let hover = Hover {
            contents: HoverContents::Markup(MarkupContent {
                kind: MarkupKind::Markdown,
                value: "some docs".to_string(),
            }),
            range: None,
        };
        assert_eq!(hover_text(&hover), "some docs");
    }

    #[test]
    fn hover_result_requires_matching_position() {
        let mut manager = LspManager::new();
        let uri = Url::parse("file:///tmp/test.rs").unwrap();
        let pos = Position::new(1, 5);
        let other_pos = Position::new(2, 0);
        manager.hover_results.insert(
            uri.clone(),
            (
                Hover {
                    contents: HoverContents::Scalar(MarkedString::String("tip".to_string())),
                    range: None,
                },
                pos,
            ),
        );
        assert!(manager.hover_result(&uri, pos).is_some());
        assert!(manager.hover_result(&uri, other_pos).is_none());
    }

    #[test]
    fn definition_result_requires_matching_position() {
        let mut manager = LspManager::new();
        let uri = Url::parse("file:///tmp/test.rs").unwrap();
        let pos = Position::new(1, 5);
        let other_pos = Position::new(2, 0);
        let target_uri = Url::parse("file:///tmp/lib.rs").unwrap();
        manager.definition_results.insert(
            uri.clone(),
            (
                GotoDefinitionResponse::Scalar(Location {
                    uri: target_uri,
                    range: Range::new(Position::new(10, 0), Position::new(10, 1)),
                }),
                pos,
            ),
        );
        assert!(manager.definition_result(&uri, pos).is_some());
        assert!(manager.definition_result(&uri, other_pos).is_none());
    }

    #[test]
    fn code_action_response_decodes_and_takes() {
        // The wire shape servers return for textDocument/codeAction is a
        // JSON array of CodeAction objects. Verify the response arm's
        // serde_json::from_value::<CodeActionResponse> decode path.
        let mut manager = LspManager::new();
        let uri = Url::parse("file:///tmp/test.rs").unwrap();
        let raw = serde_json::json!([{
            "title": "Remove unused import",
            "kind": "quickfix",
            "edit": {
                "changes": {
                    uri.as_str(): [{
                        "range": {
                            "start": { "line": 0, "character": 0 },
                            "end": { "line": 0, "character": 20 }
                        },
                        "newText": ""
                    }]
                }
            }
        }]);
        let actions: CodeActionResponse = serde_json::from_value(raw).unwrap();
        assert_eq!(actions.len(), 1);
        match &actions[0] {
            CodeActionOrCommand::CodeAction(action) => {
                assert_eq!(action.title, "Remove unused import");
                assert!(action.edit.is_some());
            }
            other => panic!("expected CodeAction, got {other:?}"),
        }
        manager.code_action_results.insert(uri.clone(), actions);
        assert!(manager.code_action_result(&uri).is_some());
        let taken = manager.take_code_action_result(&uri).unwrap();
        assert_eq!(taken.len(), 1);
        assert!(manager.code_action_result(&uri).is_none());
    }

    #[test]
    fn language_support_matches_server_command() {
        assert!(LspManager::is_language_supported("rust"));
        assert!(LspManager::is_language_supported("go"));
        assert!(LspManager::is_language_supported("typescript"));
        assert!(LspManager::is_language_supported("javascript"));
        assert!(LspManager::is_language_supported("python"));
        assert!(LspManager::is_language_supported("cpp"));
        assert!(!LspManager::is_language_supported("swift"));
        assert!(!LspManager::is_language_supported(""));
    }

    #[test]
    fn server_display_name_for_supported_languages() {
        assert_eq!(
            LspManager::server_display_name("rust"),
            Some("rust-analyzer")
        );
        assert_eq!(LspManager::server_display_name("go"), Some("gopls"));
        assert_eq!(
            LspManager::server_display_name("typescript"),
            Some("typescript-language-server")
        );
        assert_eq!(LspManager::server_display_name("python"), Some("pylsp"));
        assert_eq!(LspManager::server_display_name("cpp"), Some("clangd"));
        assert_eq!(LspManager::server_display_name("swift"), None);
    }

    #[test]
    fn document_server_status_reflects_server_presence() {
        let mut manager = LspManager::new();
        let uri = Url::parse("file:///tmp/test.rs").unwrap();
        let root = Url::parse("file:///tmp/").unwrap();

        // Unknown document => no status.
        assert!(manager.document_server_status(&uri).is_none());

        // Opening a supported language creates a status entry. The
        // `running` flag reflects whether the server process is present,
        // which depends on whether the binary is installed, so we only
        // assert the language id here.
        manager.open_document(
            uri.clone(),
            "rust".to_string(),
            root,
            "fn main() {}".to_string(),
        );
        let status = manager.document_server_status(&uri).unwrap();
        assert_eq!(status.language_id, "rust");
    }

    #[test]
    fn definition_target_extracts_scalar() {
        let uri = Url::parse("file:///tmp/lib.rs").unwrap();
        let pos = Position::new(10, 4);
        let resp = GotoDefinitionResponse::Scalar(Location {
            uri: uri.clone(),
            range: Range::new(pos, pos),
        });
        let (target_uri, target_pos) = LspManager::definition_target(&resp).unwrap();
        assert_eq!(target_uri, uri);
        assert_eq!(target_pos, pos);
    }
}
