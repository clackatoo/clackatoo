//! A language server for tests: speaks LSP, keeps its own copy of each
//! file by making the changes it's sent, and says every `bad` in a file is
//! an error, counting columns as it agreed to. So if the changes or places
//! it's sent are wrong, its errors land in the wrong places.
//!
//! Asked where something is defined, it says the first `bad` in the file,
//! when asked about its first line, or else the first `bad` on the second
//! line of `defs.fk` beside it, or of the file `--defs` names, as a link,
//! read from disk.
//!
//! | Flag | Does |
//! |---|---|
//! | `--utf16` | Counts columns in UTF-16, as LSP does unless asked |
//! | `--full` | Asks for the whole text after each edit |
//! | `--log FILE` | Writes the method of each notification there, a line each |
//! | `--defs FILE` | Answers where things are defined with FILE, not `defs.fk` |
//! | `--crash-once FILE` | Exits, as if it crashed, the first time a file has `crash` in it, making FILE so the next one started doesn't |

use std::collections::BTreeMap;
use std::io::{BufRead, Write};

use serde_json::{Value, json};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let flag = |name: &str| args.iter().any(|arg| arg == name);
    let mut server = Fake {
        utf16: flag("--utf16"),
        full: flag("--full"),
        log: (args.iter().position(|arg| arg == "--log")).and_then(|at| args.get(at + 1).cloned()),
        defs: (args.iter().position(|arg| arg == "--defs"))
            .and_then(|at| args.get(at + 1).cloned()),
        crash_once: (args.iter().position(|arg| arg == "--crash-once"))
            .and_then(|at| args.get(at + 1).cloned()),
        files: BTreeMap::new(),
    };
    let mut input = std::io::stdin().lock();
    while let Some(message) = read(&mut input) {
        if !server.receive(&message) {
            break;
        }
    }
}

struct Fake {
    utf16: bool,
    full: bool,
    log: Option<String>,
    crash_once: Option<String>,
    defs: Option<String>,
    /// Each file's text, by URI.
    files: BTreeMap<String, String>,
}

fn read(input: &mut impl BufRead) -> Option<Value> {
    let mut length = None;
    loop {
        let mut line = String::new();
        if input.read_line(&mut line).ok()? == 0 {
            return None;
        }
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some(value) = line.strip_prefix("Content-Length:") {
            length = value.trim().parse().ok();
        }
    }
    let mut body = vec![0; length?];
    input.read_exact(&mut body).ok()?;
    serde_json::from_slice(&body).ok()
}

fn send(message: &Value) {
    let body = message.to_string();
    let mut out = std::io::stdout().lock();
    write!(out, "Content-Length: {}\r\n\r\n{body}", body.len()).unwrap();
    out.flush().unwrap();
}

impl Fake {
    /// Acts on `message`. Returns whether to go on.
    fn receive(&mut self, message: &Value) -> bool {
        let method = message["method"].as_str().unwrap_or_default();
        let params = &message["params"];
        if message.get("id").is_none()
            && let Some(log) = &self.log
        {
            let mut file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(log)
                .unwrap();
            writeln!(file, "{method}").unwrap();
        }
        match method {
            "initialize" => {
                let mut capabilities = json!({ "textDocumentSync": {
                    "openClose": true,
                    "change": if self.full { 1 } else { 2 },
                    "save": { "includeText": false },
                }});
                if !self.utf16 {
                    capabilities["positionEncoding"] = json!("utf-8");
                }
                send(&json!({
                    "jsonrpc": "2.0",
                    "id": message["id"],
                    "result": { "capabilities": capabilities },
                }));
                // Asks something of its client, as servers do.
                send(&json!({
                    "jsonrpc": "2.0",
                    "id": "config",
                    "method": "workspace/configuration",
                    "params": { "items": [{ "section": "fake" }] },
                }));
            }
            "textDocument/didOpen" => {
                let document = &params["textDocument"];
                let uri = document["uri"].as_str().unwrap().to_owned();
                self.files
                    .insert(uri.clone(), document["text"].as_str().unwrap().to_owned());
                self.publish(&uri, &document["version"]);
            }
            "textDocument/didChange" => {
                let document = &params["textDocument"];
                let uri = document["uri"].as_str().unwrap().to_owned();
                let utf16 = self.utf16;
                let text = self.files.get_mut(&uri).unwrap();
                for change in params["contentChanges"].as_array().unwrap() {
                    let new = change["text"].as_str().unwrap();
                    match change.get("range") {
                        Some(range) => {
                            let start = byte(text, &range["start"], utf16);
                            let end = byte(text, &range["end"], utf16);
                            text.replace_range(start..end, new);
                        }
                        None => new.clone_into(text),
                    }
                }
                self.publish(&uri, &document["version"]);
            }
            "textDocument/definition" => {
                let uri = params["textDocument"]["uri"].as_str().unwrap();
                let result = if params["position"]["line"] == 0 {
                    let text = &self.files[uri];
                    let at = text.find("bad").unwrap_or(0);
                    json!({ "uri": uri, "range": { "start": place(text, at, self.utf16), "end": place(text, at, self.utf16) } })
                } else {
                    let defs = self.defs.as_ref().map_or_else(
                        || format!("{}/defs.fk", &uri[..uri.rfind('/').unwrap()]),
                        |path| format!("file://{path}"),
                    );
                    let file =
                        std::fs::read_to_string(defs.strip_prefix("file://").unwrap()).unwrap();
                    let line = file.lines().nth(1).unwrap();
                    let column =
                        place(line, line.find("bad").unwrap(), self.utf16)["character"].clone();
                    let start = json!({ "line": 1, "character": column });
                    json!([{
                        "targetUri": defs,
                        "targetRange": { "start": start, "end": start },
                        "targetSelectionRange": { "start": start, "end": start },
                    }])
                };
                send(&json!({ "jsonrpc": "2.0", "id": message["id"], "result": result }));
            }
            "exit" => return false,
            _ => {}
        }
        true
    }

    /// Says where each `bad` in the file at `uri` is.
    fn publish(&self, uri: &str, version: &Value) {
        let text = &self.files[uri];
        if let Some(marker) = &self.crash_once
            && text.contains("crash")
            && !std::path::Path::new(marker).exists()
        {
            std::fs::write(marker, "").unwrap();
            std::process::exit(3);
        }
        let diagnostics: Vec<Value> = (text.match_indices("bad"))
            .map(|(at, _)| {
                json!({
                    "range": { "start": place(text, at, self.utf16), "end": place(text, at + 3, self.utf16) },
                    "severity": 1,
                    "message": "bad is bad",
                    "source": "fake",
                    "code": 7,
                })
            })
            .collect();
        send(&json!({
            "jsonrpc": "2.0",
            "method": "textDocument/publishDiagnostics",
            "params": { "uri": uri, "version": version, "diagnostics": diagnostics },
        }));
    }
}

/// Byte `at` of `text` as a line and column.
fn place(text: &str, at: usize, utf16: bool) -> Value {
    let line_start = text[..at].rfind('\n').map_or(0, |newline| newline + 1);
    let before = &text[line_start..at];
    let character = if utf16 {
        before.encode_utf16().count()
    } else {
        before.len()
    };
    json!({ "line": text[..at].matches('\n').count(), "character": character })
}

/// A line and column of `text` as a byte.
fn byte(text: &str, position: &Value, utf16: bool) -> usize {
    let line = usize::try_from(position["line"].as_u64().unwrap()).unwrap();
    let character = usize::try_from(position["character"].as_u64().unwrap()).unwrap();
    let line_start: usize = text.split_inclusive('\n').take(line).map(str::len).sum();
    if !utf16 {
        return line_start + character;
    }
    let mut units = 0;
    for (offset, ch) in text[line_start..].char_indices() {
        if units >= character || ch == '\n' {
            return line_start + offset;
        }
        units += ch.len_utf16();
    }
    text.len()
}
