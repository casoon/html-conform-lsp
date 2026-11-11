// SPDX-FileCopyrightText: 2026 CASOON
// SPDX-License-Identifier: MIT

//! The LSP loop: full document sync, debounced whole-document checks, diagnostics and hover.

use std::collections::HashMap;
use std::error::Error;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::OnceLock;
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, Instant};

use html_conform::RuleInfo;
use lsp_server::{Connection, ErrorCode, Message, Notification, Request, Response};
use lsp_types::notification::{
    DidChangeTextDocument, DidCloseTextDocument, DidOpenTextDocument, Exit, Notification as _,
    PublishDiagnostics,
};
use lsp_types::request::{HoverRequest, Request as _, Shutdown};
use lsp_types::{
    Diagnostic, DidChangeTextDocumentParams, DidCloseTextDocumentParams, DidOpenTextDocumentParams,
    Hover, HoverContents, HoverParams, HoverProviderCapability, MarkupContent, MarkupKind,
    NumberOrString, Position, PublishDiagnosticsParams, ServerCapabilities,
    TextDocumentSyncCapability, TextDocumentSyncKind, TextDocumentSyncOptions, Uri,
};

use crate::diagnostics;

/// Quiet time after the last edit before a check starts.
const DEBOUNCE: Duration = Duration::from_millis(150);
/// The checker recurses once per element level (up to 512); do not rely on the default stack.
const CHECK_STACK_BYTES: usize = 16 * 1024 * 1024;

type BoxError = Box<dyn Error + Send + Sync>;

enum Event {
    Message(Message),
    Checked {
        uri: Uri,
        generation: u64,
        diagnostics: Vec<Diagnostic>,
    },
}

struct Doc {
    text: String,
    version: i32,
    /// Identifies the text a check ran on; a result for an older one is dropped.
    generation: u64,
    /// When the next check may start; `None` when the text has been handed to a check.
    deadline: Option<Instant>,
    in_flight: bool,
    /// What was last published, for hover.
    diagnostics: Vec<Diagnostic>,
}

struct State {
    docs: HashMap<Uri, Doc>,
    next_generation: u64,
    events: Sender<Event>,
}

pub fn run_stdio() -> Result<(), BoxError> {
    let (connection, io_threads) = Connection::stdio();
    let (id, _params) = connection.initialize_start()?;
    let result = serde_json::json!({
        "capabilities": capabilities(),
        "serverInfo": { "name": "html-conform-lsp", "version": env!("CARGO_PKG_VERSION") },
    });
    connection.initialize_finish(id, result)?;

    let (events, inbox) = mpsc::channel();
    let receiver = connection.receiver.clone();
    let forward = events.clone();
    thread::spawn(move || {
        for message in receiver {
            if forward.send(Event::Message(message)).is_err() {
                break;
            }
        }
    });

    let mut state = State {
        docs: HashMap::new(),
        next_generation: 0,
        events,
    };
    main_loop(&connection, &mut state, &inbox)?;
    drop(connection);
    io_threads.join()?;
    Ok(())
}

fn capabilities() -> ServerCapabilities {
    ServerCapabilities {
        text_document_sync: Some(TextDocumentSyncCapability::Options(
            TextDocumentSyncOptions {
                open_close: Some(true),
                change: Some(TextDocumentSyncKind::FULL),
                ..TextDocumentSyncOptions::default()
            },
        )),
        hover_provider: Some(HoverProviderCapability::Simple(true)),
        ..ServerCapabilities::default()
    }
}

fn main_loop(
    connection: &Connection,
    state: &mut State,
    inbox: &mpsc::Receiver<Event>,
) -> Result<(), BoxError> {
    loop {
        let event = match state.next_deadline() {
            Some(deadline) => {
                match inbox.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                    Ok(event) => Some(event),
                    Err(RecvTimeoutError::Timeout) => None,
                    Err(RecvTimeoutError::Disconnected) => return Ok(()),
                }
            }
            None => match inbox.recv() {
                Ok(event) => Some(event),
                Err(_) => return Ok(()),
            },
        };
        match event {
            Some(Event::Message(Message::Request(request))) => {
                let shutdown = request.method == Shutdown::METHOD;
                connection
                    .sender
                    .send(state.handle_request(request).into())?;
                if shutdown {
                    // The client sends `exit` next; wait for it here.
                    for event in inbox {
                        if let Event::Message(Message::Notification(n)) = event
                            && n.method == Exit::METHOD
                        {
                            break;
                        }
                    }
                    return Ok(());
                }
            }
            Some(Event::Message(Message::Notification(notification))) => {
                if notification.method == Exit::METHOD {
                    return Ok(());
                }
                state.handle_notification(connection, notification)?;
            }
            Some(Event::Message(Message::Response(_))) | None => {}
            Some(Event::Checked {
                uri,
                generation,
                diagnostics,
            }) => state.finish_check(connection, uri, generation, diagnostics)?,
        }
        state.spawn_due();
    }
}

impl State {
    fn next_deadline(&self) -> Option<Instant> {
        self.docs
            .values()
            .filter(|doc| !doc.in_flight)
            .filter_map(|doc| doc.deadline)
            .min()
    }

    fn handle_request(&mut self, request: Request) -> Response {
        match request.method.as_str() {
            Shutdown::METHOD => Response::new_ok(request.id, ()),
            HoverRequest::METHOD => match serde_json::from_value::<HoverParams>(request.params) {
                Ok(params) => Response::new_ok(request.id, self.hover(&params)),
                Err(error) => Response::new_err(
                    request.id,
                    ErrorCode::InvalidParams as i32,
                    error.to_string(),
                ),
            },
            method => Response::new_err(
                request.id,
                ErrorCode::MethodNotFound as i32,
                format!("method not supported: {method}"),
            ),
        }
    }

    fn handle_notification(
        &mut self,
        connection: &Connection,
        notification: Notification,
    ) -> Result<(), BoxError> {
        match notification.method.as_str() {
            DidOpenTextDocument::METHOD => {
                let params: DidOpenTextDocumentParams =
                    serde_json::from_value(notification.params)?;
                let document = params.text_document;
                self.next_generation += 1;
                self.docs.insert(
                    document.uri,
                    Doc {
                        text: document.text,
                        version: document.version,
                        generation: self.next_generation,
                        deadline: Some(Instant::now()),
                        in_flight: false,
                        diagnostics: Vec::new(),
                    },
                );
            }
            DidChangeTextDocument::METHOD => {
                let params: DidChangeTextDocumentParams =
                    serde_json::from_value(notification.params)?;
                // Full sync: the last change carries the whole text.
                if let (Some(doc), Some(change)) = (
                    self.docs.get_mut(&params.text_document.uri),
                    params.content_changes.into_iter().last(),
                ) {
                    self.next_generation += 1;
                    doc.text = change.text;
                    doc.version = params.text_document.version;
                    doc.generation = self.next_generation;
                    doc.deadline = Some(Instant::now() + DEBOUNCE);
                }
            }
            DidCloseTextDocument::METHOD => {
                let params: DidCloseTextDocumentParams =
                    serde_json::from_value(notification.params)?;
                let uri = params.text_document.uri;
                if self.docs.remove(&uri).is_some() {
                    publish(connection, uri, None, Vec::new())?;
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// Starts a check for every document whose quiet time is over and that has none running.
    /// A document edited while its check runs is checked again afterwards; the result for the
    /// older text is dropped in [`State::finish_check`].
    fn spawn_due(&mut self) {
        let now = Instant::now();
        for (uri, doc) in &mut self.docs {
            if doc.in_flight || doc.deadline.is_none_or(|deadline| deadline > now) {
                continue;
            }
            doc.deadline = None;
            doc.in_flight = true;
            let text = doc.text.clone();
            let generation = doc.generation;
            let uri = uri.clone();
            let events = self.events.clone();
            thread::Builder::new()
                .name("html-conform-check".to_owned())
                .stack_size(CHECK_STACK_BYTES)
                .spawn(move || {
                    let diagnostics =
                        catch_unwind(AssertUnwindSafe(|| diagnostics::diagnose(&text)))
                            .unwrap_or_else(|_| {
                                vec![diagnostics::not_checked(&text, "the checker panicked")]
                            });
                    // The receiver is gone only when the server is shutting down.
                    let _ = events.send(Event::Checked {
                        uri,
                        generation,
                        diagnostics,
                    });
                })
                .expect("spawning the check thread");
        }
    }

    fn finish_check(
        &mut self,
        connection: &Connection,
        uri: Uri,
        generation: u64,
        diagnostics: Vec<Diagnostic>,
    ) -> Result<(), BoxError> {
        let Some(doc) = self.docs.get_mut(&uri) else {
            return Ok(());
        };
        doc.in_flight = false;
        if doc.generation != generation {
            return Ok(());
        }
        doc.diagnostics.clone_from(&diagnostics);
        publish(connection, uri, Some(doc.version), diagnostics)
    }

    fn hover(&self, params: &HoverParams) -> Option<Hover> {
        let at = params.text_document_position_params.position;
        let doc = self
            .docs
            .get(&params.text_document_position_params.text_document.uri)?;
        let catalogue = rule_catalogue();
        let mut seen = Vec::new();
        let mut sections = Vec::new();
        for diagnostic in &doc.diagnostics {
            let Some(NumberOrString::String(code)) = &diagnostic.code else {
                continue;
            };
            if !contains(diagnostic, at) || seen.contains(code) {
                continue;
            }
            if let Some(rule) = catalogue.get(code.as_str()) {
                seen.push(code.clone());
                sections.push(rule_markdown(rule));
            }
        }
        if sections.is_empty() {
            return None;
        }
        Some(Hover {
            contents: HoverContents::Markup(MarkupContent {
                kind: MarkupKind::Markdown,
                value: sections.join("\n\n---\n\n"),
            }),
            range: None,
        })
    }
}

/// Whether `at` is inside the diagnostic's range; the end counts, so a point range is hit
/// when the cursor is on it.
fn contains(diagnostic: &Diagnostic, at: Position) -> bool {
    let range = diagnostic.range;
    (range.start.line, range.start.character) <= (at.line, at.character)
        && (at.line, at.character) <= (range.end.line, range.end.character)
}

fn rule_markdown(rule: &RuleInfo) -> String {
    let mut text = format!("**{}**\n\n{}", rule.id, rule.description);
    if let Some(source) = &rule.normative_source {
        text.push_str(&format!("\n\nNormative source: {source}"));
    }
    if let Some(url) = &rule.help_url {
        text.push_str(&format!("\n\n[Rule guide]({url})"));
    }
    text
}

fn rule_catalogue() -> &'static HashMap<String, RuleInfo> {
    static CATALOGUE: OnceLock<HashMap<String, RuleInfo>> = OnceLock::new();
    CATALOGUE.get_or_init(|| {
        html_conform::rules()
            .unwrap_or_default()
            .into_iter()
            .map(|rule| (rule.id.clone(), rule))
            .collect()
    })
}

fn publish(
    connection: &Connection,
    uri: Uri,
    version: Option<i32>,
    diagnostics: Vec<Diagnostic>,
) -> Result<(), BoxError> {
    let params = PublishDiagnosticsParams {
        uri,
        diagnostics,
        version,
    };
    connection
        .sender
        .send(Notification::new(PublishDiagnostics::METHOD.to_owned(), params).into())?;
    Ok(())
}
