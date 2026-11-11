// SPDX-FileCopyrightText: 2026 CASOON
// SPDX-License-Identifier: MIT

//! Talks to the built server over stdio, like an editor does.

use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use serde_json::{Value, json};

struct Client {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl Client {
    fn start() -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_html-conform-lsp"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("start server");
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        Self {
            child,
            stdin,
            stdout,
        }
    }

    fn send(&mut self, message: &Value) {
        let body = message.to_string();
        write!(self.stdin, "Content-Length: {}\r\n\r\n{body}", body.len()).unwrap();
        self.stdin.flush().unwrap();
    }

    fn read(&mut self) -> Value {
        let mut length = 0;
        loop {
            let mut line = String::new();
            assert!(
                self.stdout.read_line(&mut line).unwrap() > 0,
                "server closed stdout"
            );
            let line = line.trim();
            if line.is_empty() {
                break;
            }
            if let Some(value) = line.strip_prefix("Content-Length: ") {
                length = value.parse().unwrap();
            }
        }
        let mut body = vec![0; length];
        self.stdout.read_exact(&mut body).unwrap();
        serde_json::from_slice(&body).unwrap()
    }

    /// Reads until a message matching `wanted` arrives (skips unrelated ones).
    fn read_until(&mut self, wanted: impl Fn(&Value) -> bool) -> Value {
        loop {
            let message = self.read();
            if wanted(&message) {
                return message;
            }
        }
    }

    fn notify(&mut self, method: &str, params: Value) {
        self.send(&json!({ "jsonrpc": "2.0", "method": method, "params": params }));
    }

    fn request(&mut self, id: u64, method: &str, params: Value) -> Value {
        self.send(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }));
        self.read_until(|m| m["id"] == id)
    }

    fn initialize(&mut self) -> Value {
        let response = self.request(
            1,
            "initialize",
            json!({ "processId": null, "capabilities": {}, "rootUri": null }),
        );
        self.notify("initialized", json!({}));
        response
    }

    fn diagnostics(&mut self, uri: &str) -> Value {
        let uri = uri.to_owned();
        self.read_until(|m| {
            m["method"] == "textDocument/publishDiagnostics" && m["params"]["uri"] == uri
        })["params"]
            .clone()
    }

    fn shutdown(mut self) {
        let response = self.request(99, "shutdown", Value::Null);
        assert!(response["error"].is_null());
        self.notify("exit", Value::Null);
        assert!(self.child.wait().unwrap().success());
    }
}

const URI: &str = "file:///tmp/page.html";
// 😀 is two UTF-16 units; the `<img>` has no `alt`.
const DOC: &str = "<!DOCTYPE html>\n<html lang=\"en\"><head><title>t</title></head><body>\n<p>😀</p><img src=\"a.png\"></body></html>\n";

fn open(client: &mut Client, text: &str) {
    client.notify(
        "textDocument/didOpen",
        json!({ "textDocument": { "uri": URI, "languageId": "html", "version": 1, "text": text } }),
    );
}

#[test]
fn open_change_hover_close() {
    let mut client = Client::start();
    let init = client.initialize();
    assert_eq!(
        init["result"]["capabilities"]["textDocumentSync"]["change"],
        1
    );
    assert_eq!(init["result"]["capabilities"]["hoverProvider"], true);

    open(&mut client, DOC);
    let published = client.diagnostics(URI);
    assert_eq!(published["version"], 1);
    let diagnostics = published["diagnostics"].as_array().unwrap();
    let img = diagnostics
        .iter()
        .find(|d| d["message"].as_str().unwrap().contains("alt"))
        .unwrap_or_else(|| panic!("no alt diagnostic in {diagnostics:#?}"));
    assert_eq!(img["source"], "html-conform");
    assert_eq!(img["severity"], 1);
    assert!(img["code"].as_str().unwrap().contains('.'));
    assert!(
        img["codeDescription"]["href"]
            .as_str()
            .unwrap()
            .starts_with("https://")
    );
    // Line 2 (zero-based): `<p>😀</p>` is 3 + 2 + 4 = 9 UTF-16 units, then `<img ...>`.
    assert_eq!(img["range"]["start"], json!({ "line": 2, "character": 9 }));
    assert_eq!(img["range"]["end"], json!({ "line": 2, "character": 26 }));

    // Hover on the diagnostic returns the rule description.
    let hover = client.request(
        2,
        "textDocument/hover",
        json!({ "textDocument": { "uri": URI }, "position": { "line": 2, "character": 12 } }),
    );
    let value = hover["result"]["contents"]["value"].as_str().unwrap();
    assert!(value.contains(img["code"].as_str().unwrap()), "{value}");

    // A fix replaces the diagnostics (debounced).
    client.notify(
        "textDocument/didChange",
        json!({
            "textDocument": { "uri": URI, "version": 2 },
            "contentChanges": [{ "text": DOC.replace("<img src=\"a.png\">", "<img src=\"a.png\" alt=\"\">") }],
        }),
    );
    let fixed = client.diagnostics(URI);
    assert_eq!(fixed["version"], 2);
    assert!(
        fixed["diagnostics"].as_array().unwrap().is_empty(),
        "{fixed:#?}"
    );

    client.notify(
        "textDocument/didClose",
        json!({ "textDocument": { "uri": URI } }),
    );
    let closed = client.diagnostics(URI);
    assert!(closed["diagnostics"].as_array().unwrap().is_empty());
    assert!(closed["version"].is_null());

    client.shutdown();
}

#[test]
fn burst_of_edits_publishes_only_the_last_text() {
    let mut client = Client::start();
    client.initialize();
    open(&mut client, DOC);
    let _ = client.diagnostics(URI);

    for version in 2..=6 {
        client.notify(
            "textDocument/didChange",
            json!({
                "textDocument": { "uri": URI, "version": version },
                "contentChanges": [{ "text": format!("{DOC}<!-- {version} -->") }],
            }),
        );
    }
    let published = client.diagnostics(URI);
    assert_eq!(published["version"], 6);
    client.shutdown();
}
