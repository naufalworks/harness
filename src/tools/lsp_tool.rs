//! LSP diagnostics, references and approval-safe workspace rename.
//! Contract: docs/design/tools.md#lsp.
//!
//! A fresh stdio server per call gives each recorded step one bounded lifetime and no hidden
//! daemon state. Read-only calls never ask for approval. Rename plans a workspace edit for the
//! permission card, then queries again after approval and rechecks every supplied file hash.
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashSet};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};
use url::Url;

use super::edit_tools::{atomic_write, describe, run_capped};
use super::fs_tools::read_text;
use super::{content_hash, paths, PendingChange, Tool, ToolCtx, ToolResult};

const FILE_MAX: usize = 2 * 1024 * 1024;
const FRAME_MAX: usize = 4 * 1024 * 1024;
const STDERR_MAX: usize = 8 * 1024;
const DEFAULT_TIMEOUT: u64 = 20;
const MAX_TIMEOUT: u64 = 60;
const MAX_DIAGNOSTICS: usize = 100;
const MAX_REFERENCES: usize = 100;
const MAX_FILES: usize = 20;
const MAX_EDITS: usize = 200;
const PERMISSION_DIFF_MAX: usize = 64 * 1024;
const DIAGNOSTICS_TIMEOUT: u64 = 60;
const DIAGNOSTICS_CAP: usize = 4 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Operation {
    Diagnostics,
    References,
    Rename,
}

impl Operation {
    fn parse(args: &Value) -> Result<Self, ToolResult> {
        match args.get("operation").and_then(Value::as_str) {
            Some("diagnostics") => Ok(Self::Diagnostics),
            Some("references") => Ok(Self::References),
            Some("rename") => Ok(Self::Rename),
            Some(other) => Err(ToolResult::err(
                "invalid_arguments",
                format!(
                    "unknown lsp operation {other:?}; expected diagnostics, references, or rename"
                ),
            )),
            None => Err(ToolResult::err(
                "invalid_arguments",
                "operation is required",
            )),
        }
    }
}

#[derive(Clone, Copy)]
struct ServerSpec {
    program: &'static str,
    args: &'static [&'static str],
    language_id: &'static str,
    rust: bool,
}

fn server_for(path: &Path) -> Option<ServerSpec> {
    match path.extension().and_then(|part| part.to_str()) {
        Some("rs") => Some(ServerSpec {
            program: "rust-analyzer",
            args: &[],
            language_id: "rust",
            rust: true,
        }),
        Some("c") => Some(ServerSpec {
            program: "clangd",
            args: &[],
            language_id: "c",
            rust: false,
        }),
        Some("h" | "hh" | "hpp" | "hxx" | "cc" | "cpp" | "cxx") => Some(ServerSpec {
            program: "clangd",
            args: &[],
            language_id: "cpp",
            rust: false,
        }),
        _ => None,
    }
}

struct Prepared {
    operation: Operation,
    display: String,
    text: String,
    uri: String,
    position: Option<Value>,
    new_name: Option<String>,
    expected: BTreeMap<String, String>,
    include_declaration: bool,
    timeout: Duration,
    server: ServerSpec,
}

fn positive(args: &Value, key: &str) -> Result<usize, ToolResult> {
    match args.get(key).and_then(Value::as_u64) {
        Some(value) if value > 0 && value <= usize::MAX as u64 => Ok(value as usize),
        _ => Err(ToolResult::err(
            "invalid_arguments",
            format!("{key} is required and must be a positive whole number"),
        )),
    }
}

fn valid_hash(hash: &str) -> bool {
    hash.len() == 8 && hash.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

fn parse_expected(ctx: &ToolCtx, args: &Value) -> Result<BTreeMap<String, String>, ToolResult> {
    let Some(items) = args.get("expected_files").and_then(Value::as_array) else {
        return Err(ToolResult::err(
            "invalid_arguments",
            "rename requires expected_files copied from current read/references results",
        ));
    };
    if items.is_empty() || items.len() > MAX_FILES {
        return Err(ToolResult::err(
            "invalid_arguments",
            format!("expected_files must contain 1-{MAX_FILES} path/hash pairs"),
        ));
    }
    let mut expected = BTreeMap::new();
    for (index, item) in items.iter().enumerate() {
        let Some(raw) = item.get("path").and_then(Value::as_str) else {
            return Err(ToolResult::err(
                "invalid_arguments",
                format!("expected_files item {} needs path", index + 1),
            ));
        };
        let Some(hash) = item.get("content_hash").and_then(Value::as_str) else {
            return Err(ToolResult::err(
                "invalid_arguments",
                format!("expected_files item {} needs content_hash", index + 1),
            ));
        };
        if !valid_hash(hash) {
            return Err(ToolResult::err(
                "invalid_arguments",
                format!(
                    "expected_files item {} has an invalid 8-hex content_hash",
                    index + 1
                ),
            ));
        }
        let path = paths::resolve(&ctx.root, raw)
            .map_err(|error| ToolResult::err(error.code(), error.detail()))?;
        if !path.is_file() {
            return Err(ToolResult::err(
                "not_found",
                format!(
                    "{} is not an existing file",
                    paths::display(&ctx.root, &path)
                ),
            ));
        }
        let display = paths::display(&ctx.root, &path);
        if expected.insert(display.clone(), hash.to_string()).is_some() {
            return Err(ToolResult::err(
                "invalid_arguments",
                format!("expected_files lists {display} more than once"),
            ));
        }
        let current = read_text(&path)?;
        if current.len() > FILE_MAX {
            return Err(ToolResult::err(
                "too_large",
                format!(
                    "{display} is {} bytes; lsp stops at {FILE_MAX}",
                    current.len()
                ),
            ));
        }
        let actual = content_hash(&current);
        if actual != hash {
            return Err(ToolResult::err(
                "stale_anchor",
                format!(
                "{display} changed: expected content_hash {hash}, current content_hash {actual}"
            ),
            ));
        }
    }
    Ok(expected)
}

fn prepare(ctx: &ToolCtx, args: &Value) -> Result<Prepared, ToolResult> {
    let operation = Operation::parse(args)?;
    let Some(raw) = args.get("path").and_then(Value::as_str) else {
        return Err(ToolResult::err("invalid_arguments", "path is required"));
    };
    let path = paths::resolve(&ctx.root, raw)
        .map_err(|error| ToolResult::err(error.code(), error.detail()))?;
    let display = paths::display(&ctx.root, &path);
    let text = read_text(&path)?;
    if text.len() > FILE_MAX {
        return Err(ToolResult::err(
            "too_large",
            format!("{display} is {} bytes; lsp stops at {FILE_MAX}", text.len()),
        ));
    }
    let Some(server) = server_for(&path) else {
        return Err(ToolResult::err(
            "invalid_arguments",
            format!(
                "no language server is configured for {display}; lsp supports Rust and C/C++ files"
            ),
        ));
    };
    let uri = Url::from_file_path(&path)
        .map_err(|_| {
            ToolResult::err(
                "lsp_protocol",
                format!("could not make a file URI for {display}"),
            )
        })?
        .to_string();
    let position = if operation == Operation::Diagnostics {
        None
    } else {
        let (line, column) = (positive(args, "line")?, positive(args, "column")?);
        Some(model_to_lsp(&text, line, column).map_err(|detail| {
            ToolResult::err("invalid_arguments", format!("{display}: {detail}"))
        })?)
    };
    let timeout = match args.get("timeout_seconds") {
        None => DEFAULT_TIMEOUT,
        Some(value) => match value.as_u64() {
            Some(seconds) if (1..=MAX_TIMEOUT).contains(&seconds) => seconds,
            _ => {
                return Err(ToolResult::err(
                    "invalid_arguments",
                    format!("timeout_seconds must be a whole number between 1 and {MAX_TIMEOUT}"),
                ))
            }
        },
    };
    let (new_name, expected) = if operation == Operation::Rename {
        let name = args
            .get("new_name")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .ok_or_else(|| {
                ToolResult::err("invalid_arguments", "rename requires a non-blank new_name")
            })?;
        if name.chars().count() > 128 || name.chars().any(char::is_whitespace) {
            return Err(ToolResult::err(
                "invalid_arguments",
                "new_name must be at most 128 characters and contain no whitespace",
            ));
        }
        let expected = parse_expected(ctx, args)?;
        let actual = content_hash(&text);
        match expected.get(&display) {
            Some(hash) if hash == &actual => {}
            Some(hash) => {
                return Err(ToolResult::err(
                    "stale_anchor",
                    format!(
                "{display} changed: expected content_hash {hash}, current content_hash {actual}"
            ),
                ))
            }
            None => {
                return Err(ToolResult::err(
                    "stale_anchor",
                    format!("expected_files must include {display} with content_hash {actual}"),
                ))
            }
        }
        (Some(name.to_string()), expected)
    } else {
        (None, BTreeMap::new())
    };
    Ok(Prepared {
        operation,
        display,
        text,
        uri,
        position,
        new_name,
        expected,
        include_declaration: args
            .get("include_declaration")
            .and_then(Value::as_bool)
            .unwrap_or(true),
        timeout: Duration::from_secs(timeout),
        server,
    })
}

fn line_slice(text: &str, zero_line: usize) -> Option<(usize, &str)> {
    let mut base = 0usize;
    for (index, line) in text.split('\n').enumerate() {
        if index == zero_line {
            return Some((base, line.strip_suffix('\r').unwrap_or(line)));
        }
        base = base.saturating_add(line.len() + 1);
    }
    None
}

fn model_to_lsp(text: &str, line: usize, column: usize) -> Result<Value, String> {
    let Some((_, source)) = line.checked_sub(1).and_then(|line| line_slice(text, line)) else {
        return Err(format!("line {line} is outside the file"));
    };
    let count = source.chars().count();
    if column == 0 || column > count + 1 {
        return Err(format!(
            "column {column} is outside line {line} (valid range 1-{})",
            count + 1
        ));
    }
    let units: usize = source.chars().take(column - 1).map(char::len_utf16).sum();
    Ok(json!({ "line": line - 1, "character": units }))
}

fn utf16_to_byte(text: &str, line: usize, units: usize) -> Result<usize, String> {
    let Some((base, source)) = line_slice(text, line) else {
        return Err(format!("LSP line {} is outside the file", line + 1));
    };
    let mut seen = 0usize;
    for (byte, ch) in source.char_indices() {
        if seen == units {
            return Ok(base + byte);
        }
        seen += ch.len_utf16();
        if seen > units {
            return Err("LSP position splits a UTF-16 surrogate pair".into());
        }
    }
    if seen == units {
        Ok(base + source.len())
    } else {
        Err(format!("LSP column {units} is outside line {}", line + 1))
    }
}

fn lsp_to_model(text: &str, position: &Value) -> Result<(usize, usize), String> {
    let line = position
        .get("line")
        .and_then(Value::as_u64)
        .ok_or_else(|| "LSP position has no line".to_string())? as usize;
    let units = position
        .get("character")
        .and_then(Value::as_u64)
        .ok_or_else(|| "LSP position has no character".to_string())? as usize;
    let (_, source) = line_slice(text, line)
        .ok_or_else(|| format!("LSP line {} is outside the file", line + 1))?;
    let (mut seen, mut column) = (0usize, 1usize);
    for ch in source.chars() {
        if seen == units {
            return Ok((line + 1, column));
        }
        seen += ch.len_utf16();
        column += 1;
        if seen > units {
            return Err("LSP position splits a UTF-16 surrogate pair".into());
        }
    }
    if seen == units {
        Ok((line + 1, column))
    } else {
        Err(format!("LSP column {units} is outside line {}", line + 1))
    }
}

type Frame = Result<Value, String>;

struct Session {
    child: Child,
    input: ChildStdin,
    frames: Receiver<Frame>,
    stderr: Receiver<String>,
    deadline: Instant,
    root_uri: String,
    program: &'static str,
    stopped: bool,
}

impl Session {
    fn start(ctx: &ToolCtx, spec: ServerSpec, timeout: Duration) -> Result<Self, ToolResult> {
        let mut command = Command::new(spec.program);
        command
            .args(spec.args)
            .current_dir(&ctx.root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command.env_clear();
        for key in ["PATH", "HOME", "LANG", "LC_ALL", "TERM"] {
            if let Ok(value) = std::env::var(key) {
                command.env(key, value);
            }
        }
        command.env("CARGO_NET_OFFLINE", "true");
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        let mut child = command.spawn().map_err(|error| {
            ToolResult::err(
                "lsp_unavailable",
                format!("could not start {}: {error}", spec.program),
            )
        })?;
        let input = child.stdin.take().ok_or_else(|| {
            ToolResult::err("lsp_unavailable", format!("{} has no stdin", spec.program))
        })?;
        let output = child.stdout.take().ok_or_else(|| {
            ToolResult::err("lsp_unavailable", format!("{} has no stdout", spec.program))
        })?;
        let errors = child.stderr.take().ok_or_else(|| {
            ToolResult::err("lsp_unavailable", format!("{} has no stderr", spec.program))
        })?;

        let (frame_tx, frames) = mpsc::channel();
        std::thread::spawn(move || {
            let mut reader = BufReader::new(output);
            loop {
                match read_frame(&mut reader) {
                    Ok(Some(value)) => {
                        if frame_tx.send(Ok(value)).is_err() {
                            break;
                        }
                    }
                    Ok(None) => break,
                    Err(error) => {
                        let _ = frame_tx.send(Err(error));
                        break;
                    }
                }
            }
        });
        let (stderr_tx, stderr) = mpsc::channel();
        std::thread::spawn(move || {
            let mut reader = BufReader::new(errors);
            let mut kept = Vec::new();
            let mut buffer = [0u8; 4096];
            while let Ok(count) = reader.read(&mut buffer) {
                if count == 0 {
                    break;
                }
                let room = STDERR_MAX.saturating_sub(kept.len());
                kept.extend_from_slice(&buffer[..count.min(room)]);
            }
            let _ = stderr_tx.send(String::from_utf8_lossy(&kept).trim().to_string());
        });
        let root_uri = Url::from_directory_path(&ctx.root)
            .map_err(|_| ToolResult::err("lsp_protocol", "could not make the project root URI"))?
            .to_string();
        Ok(Self {
            child,
            input,
            frames,
            stderr,
            deadline: Instant::now() + timeout,
            root_uri,
            program: spec.program,
            stopped: false,
        })
    }

    fn send(&mut self, value: &Value) -> Result<(), ToolResult> {
        let body = serde_json::to_vec(value).map_err(|error| {
            ToolResult::err(
                "lsp_protocol",
                format!("could not encode an LSP message: {error}"),
            )
        })?;
        if body.len() > FRAME_MAX {
            return Err(ToolResult::err(
                "lsp_protocol",
                format!(
                    "outgoing LSP frame is {} bytes; limit is {FRAME_MAX}",
                    body.len()
                ),
            ));
        }
        write!(self.input, "Content-Length: {}\r\n\r\n", body.len())
            .and_then(|_| self.input.write_all(&body))
            .and_then(|_| self.input.flush())
            .map_err(|error| {
                ToolResult::err(
                    "lsp_unavailable",
                    format!("{} stopped accepting messages: {error}", self.program),
                )
            })
    }

    fn request(&mut self, id: i64, method: &str, params: Value) -> Result<(), ToolResult> {
        self.send(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }))
    }

    fn notify(&mut self, method: &str, params: Value) -> Result<(), ToolResult> {
        self.send(&json!({ "jsonrpc": "2.0", "method": method, "params": params }))
    }

    fn disconnected(&mut self) -> ToolResult {
        let status = self
            .child
            .try_wait()
            .ok()
            .flatten()
            .map(|status| format!(" ({status})"))
            .unwrap_or_default();
        let stderr = self
            .stderr
            .recv_timeout(Duration::from_millis(100))
            .unwrap_or_default();
        let suffix = if stderr.is_empty() {
            status
        } else {
            format!("{status}: {stderr}")
        };
        ToolResult::err(
            "lsp_unavailable",
            format!("{} stopped before replying{suffix}", self.program),
        )
    }

    fn receive(&mut self) -> Result<Value, ToolResult> {
        let Some(remaining) = self.deadline.checked_duration_since(Instant::now()) else {
            return Err(ToolResult::err(
                "timeout",
                format!("{} exceeded its LSP deadline", self.program),
            ));
        };
        match self.frames.recv_timeout(remaining) {
            Ok(Ok(value)) => Ok(value),
            Ok(Err(error)) => Err(ToolResult::err("lsp_protocol", error)),
            Err(RecvTimeoutError::Timeout) => Err(ToolResult::err(
                "timeout",
                format!("{} did not reply before the LSP deadline", self.program),
            )),
            Err(RecvTimeoutError::Disconnected) => Err(self.disconnected()),
        }
    }

    fn answer_server_request(&mut self, message: &Value) -> Result<bool, ToolResult> {
        let (Some(id), Some(method)) = (
            message.get("id"),
            message.get("method").and_then(Value::as_str),
        ) else {
            return Ok(false);
        };
        let result = match method {
            "workspace/configuration" => {
                let count = message
                    .pointer("/params/items")
                    .and_then(Value::as_array)
                    .map(Vec::len)
                    .unwrap_or(0);
                Value::Array((0..count).map(|_| Value::Null).collect())
            }
            "workspace/workspaceFolders" => json!([{ "uri": self.root_uri, "name": "project" }]),
            "workspace/applyEdit" => json!({
                "applied": false,
                "failureReason": "Harness applies only the reviewed workspace edit returned by rename"
            }),
            "client/registerCapability"
            | "client/unregisterCapability"
            | "window/workDoneProgress/create" => Value::Null,
            _ => {
                return self
                    .send(&json!({
                        "jsonrpc": "2.0", "id": id,
                        "error": { "code": -32601, "message": "method not supported by Harness" }
                    }))
                    .map(|_| true)
            }
        };
        self.send(&json!({ "jsonrpc": "2.0", "id": id, "result": result }))?;
        Ok(true)
    }

    fn wait_response(&mut self, wanted: i64) -> Result<Value, ToolResult> {
        loop {
            let message = self.receive()?;
            if self.answer_server_request(&message)? {
                continue;
            }
            if message.get("id").and_then(Value::as_i64) != Some(wanted) {
                continue;
            }
            if let Some(error) = message.get("error") {
                return Err(ToolResult::err(
                    "lsp_protocol",
                    format!(
                        "{} rejected the request: {}",
                        self.program,
                        error
                            .get("message")
                            .and_then(Value::as_str)
                            .unwrap_or("unknown LSP error")
                    ),
                ));
            }
            return Ok(message.get("result").cloned().unwrap_or(Value::Null));
        }
    }

    fn wait_diagnostics(&mut self, uri: &str) -> Result<Value, ToolResult> {
        loop {
            let message = self.receive()?;
            if self.answer_server_request(&message)? {
                continue;
            }
            if message.get("method").and_then(Value::as_str)
                != Some("textDocument/publishDiagnostics")
            {
                continue;
            }
            let params = message.get("params").cloned().unwrap_or(Value::Null);
            if params.get("uri").and_then(Value::as_str) == Some(uri) {
                return Ok(params);
            }
        }
    }

    fn initialize(&mut self, spec: ServerSpec) -> Result<(), ToolResult> {
        let options = if spec.rust {
            json!({ "cargo": { "buildScripts": { "enable": false } },
                    "procMacro": { "enable": false }, "checkOnSave": false })
        } else {
            Value::Null
        };
        self.request(
            1,
            "initialize",
            json!({
                "processId": Value::Null,
                "rootUri": self.root_uri,
                "workspaceFolders": [{ "uri": self.root_uri, "name": "project" }],
                "clientInfo": { "name": "Harness", "version": env!("CARGO_PKG_VERSION") },
                "capabilities": {
                    "workspace": { "applyEdit": false, "configuration": true,
                        "workspaceFolders": true, "workspaceEdit": { "documentChanges": true } },
                    "textDocument": { "publishDiagnostics": { "relatedInformation": true },
                        "references": {}, "rename": { "prepareSupport": false } }
                },
                "initializationOptions": options
            }),
        )?;
        let _ = self.wait_response(1)?;
        self.notify("initialized", json!({}))
    }

    fn open(&mut self, prepared: &Prepared) -> Result<(), ToolResult> {
        self.notify(
            "textDocument/didOpen",
            json!({ "textDocument": {
                "uri": prepared.uri,
                "languageId": prepared.server.language_id,
                "version": 1,
                "text": prepared.text
            }}),
        )
    }

    fn stop(&mut self) {
        if self.stopped {
            return;
        }
        self.stopped = true;
        let _ = self.request(99, "shutdown", Value::Null);
        let _ = self.notify("exit", Value::Null);
        for _ in 0..10 {
            if self.child.try_wait().ok().flatten().is_some() {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        kill_group(self.child.id());
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.stop();
    }
}

fn read_frame(reader: &mut impl BufRead) -> Result<Option<Value>, String> {
    let mut content_length = None;
    let mut saw_header = false;
    loop {
        let mut line = String::new();
        let count = reader
            .read_line(&mut line)
            .map_err(|error| format!("could not read LSP header: {error}"))?;
        if count == 0 {
            return if saw_header {
                Err("LSP stream ended inside a header".into())
            } else {
                Ok(None)
            };
        }
        saw_header = true;
        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            if name.eq_ignore_ascii_case("Content-Length") {
                content_length = Some(
                    value
                        .trim()
                        .parse::<usize>()
                        .map_err(|_| "invalid LSP Content-Length".to_string())?,
                );
            }
        }
    }
    let length = content_length.ok_or_else(|| "LSP frame has no Content-Length".to_string())?;
    if length > FRAME_MAX {
        return Err(format!("LSP frame is {length} bytes; limit is {FRAME_MAX}"));
    }
    let mut body = vec![0u8; length];
    reader
        .read_exact(&mut body)
        .map_err(|error| format!("could not read LSP body: {error}"))?;
    serde_json::from_slice(&body)
        .map(Some)
        .map_err(|error| format!("invalid LSP JSON: {error}"))
}

#[cfg(unix)]
fn kill_group(pid: u32) {
    let _ = Command::new("sh")
        .arg("-c")
        .arg(format!("kill -9 -{pid} 2>/dev/null"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}
#[cfg(not(unix))]
fn kill_group(_pid: u32) {}

fn query_server(ctx: &ToolCtx, prepared: &Prepared) -> Result<Value, ToolResult> {
    let mut session = Session::start(ctx, prepared.server, prepared.timeout)?;
    let result = (|| {
        session.initialize(prepared.server)?;
        session.open(prepared)?;
        match prepared.operation {
            Operation::Diagnostics => session.wait_diagnostics(&prepared.uri),
            Operation::References => {
                session.request(
                    2,
                    "textDocument/references",
                    json!({
                        "textDocument": { "uri": prepared.uri },
                        "position": prepared.position,
                        "context": { "includeDeclaration": prepared.include_declaration }
                    }),
                )?;
                session.wait_response(2)
            }
            Operation::Rename => {
                session.request(
                    2,
                    "textDocument/rename",
                    json!({
                        "textDocument": { "uri": prepared.uri },
                        "position": prepared.position,
                        "newName": prepared.new_name
                    }),
                )?;
                session.wait_response(2)
            }
        }
    })();
    session.stop();
    result
}

fn one_line(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn severity(value: Option<u64>) -> &'static str {
    match value {
        Some(1) => "error",
        Some(2) => "warning",
        Some(3) => "info",
        Some(4) => "hint",
        _ => "diagnostic",
    }
}

fn code_text(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(code)) => format!(" [{code}]"),
        Some(Value::Number(code)) => format!(" [{code}]"),
        _ => String::new(),
    }
}

fn format_diagnostics(prepared: &Prepared, params: &Value) -> Result<ToolResult, ToolResult> {
    let diagnostics = params
        .get("diagnostics")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            ToolResult::err(
                "lsp_protocol",
                "publishDiagnostics has no diagnostics array",
            )
        })?;
    let mut rows = Vec::new();
    let mut invalid = 0usize;
    for diagnostic in diagnostics {
        let Some(start) = diagnostic.pointer("/range/start") else {
            invalid += 1;
            continue;
        };
        let Ok((line, column)) = lsp_to_model(&prepared.text, start) else {
            invalid += 1;
            continue;
        };
        let level = severity(diagnostic.get("severity").and_then(Value::as_u64));
        let message = one_line(
            diagnostic
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("diagnostic without a message"),
        );
        rows.push((
            line,
            column,
            level,
            format!(
                "{}:{line}:{column} {level}{} {message}",
                prepared.display,
                code_text(diagnostic.get("code"))
            ),
        ));
    }
    rows.sort_by(|left, right| (left.0, left.1, left.2).cmp(&(right.0, right.1, right.2)));
    let total = rows.len();
    let mut output = format!("diagnostics for {}: {total}\n", prepared.display);
    for (_, _, _, row) in rows.into_iter().take(MAX_DIAGNOSTICS) {
        output.push_str(&row);
        output.push('\n');
    }
    if total > MAX_DIAGNOSTICS {
        output.push_str(&format!(
            "{} diagnostics omitted by the {MAX_DIAGNOSTICS} result cap\n",
            total - MAX_DIAGNOSTICS
        ));
    }
    if invalid > 0 {
        output.push_str(&format!(
            "{invalid} malformed/out-of-range diagnostics omitted\n"
        ));
    }
    Ok(ToolResult::ok(
        format!("lsp diagnostics {} ({total})", prepared.display),
        output.trim_end().to_string(),
    ))
}

fn project_path(ctx: &ToolCtx, uri: &str) -> Result<(PathBuf, String), String> {
    let url = Url::parse(uri).map_err(|_| "invalid URI".to_string())?;
    if url.scheme() != "file" {
        return Err("non-file URI".into());
    }
    let raw = url
        .to_file_path()
        .map_err(|_| "invalid file URI".to_string())?;
    let path = paths::resolve(&ctx.root, &raw.to_string_lossy())
        .map_err(|_| "outside-root or denied path".to_string())?;
    if !path.is_file() {
        return Err("not an existing file".into());
    }
    let display = paths::display(&ctx.root, &path);
    Ok((path, display))
}

fn location_parts(location: &Value) -> Option<(&str, &Value)> {
    if let (Some(uri), Some(range)) = (
        location.get("uri").and_then(Value::as_str),
        location.get("range"),
    ) {
        return Some((uri, range));
    }
    match (
        location.get("targetUri").and_then(Value::as_str),
        location
            .get("targetSelectionRange")
            .or_else(|| location.get("targetRange")),
    ) {
        (Some(uri), Some(range)) => Some((uri, range)),
        _ => None,
    }
}

struct ReferenceFile {
    text: String,
    hash: String,
    locations: Vec<String>,
}

fn format_references(
    ctx: &ToolCtx,
    prepared: &Prepared,
    result: &Value,
) -> Result<ToolResult, ToolResult> {
    let empty = Vec::new();
    let locations = if result.is_null() {
        &empty
    } else {
        result.as_array().ok_or_else(|| {
            ToolResult::err("lsp_protocol", "references result is not an array or null")
        })?
    };
    let mut files: BTreeMap<String, ReferenceFile> = BTreeMap::new();
    let mut seen = HashSet::new();
    let mut omitted = 0usize;
    for location in locations {
        if seen.len() >= MAX_REFERENCES {
            omitted += 1;
            continue;
        }
        let Some((uri, range)) = location_parts(location) else {
            omitted += 1;
            continue;
        };
        let Ok((path, display)) = project_path(ctx, uri) else {
            omitted += 1;
            continue;
        };
        if !files.contains_key(&display) {
            if files.len() >= MAX_FILES {
                omitted += 1;
                continue;
            }
            let Ok(text) = read_text(&path) else {
                omitted += 1;
                continue;
            };
            if text.len() > FILE_MAX {
                omitted += 1;
                continue;
            }
            files.insert(
                display.clone(),
                ReferenceFile {
                    hash: content_hash(&text),
                    text,
                    locations: Vec::new(),
                },
            );
        }
        let Some(start) = range.get("start") else {
            omitted += 1;
            continue;
        };
        let Some(end) = range.get("end") else {
            omitted += 1;
            continue;
        };
        let file = files.get_mut(&display).expect("inserted above");
        let (Ok((start_line, start_column)), Ok((end_line, end_column))) = (
            lsp_to_model(&file.text, start),
            lsp_to_model(&file.text, end),
        ) else {
            omitted += 1;
            continue;
        };
        let key = format!("{display}:{start_line}:{start_column}-{end_line}:{end_column}");
        if !seen.insert(key) {
            continue;
        }
        file.locations.push(format!(
            "  {start_line}:{start_column}-{end_line}:{end_column}"
        ));
    }
    let count: usize = files.values().map(|file| file.locations.len()).sum();
    let mut output = format!("references from {}: {count}\n", prepared.display);
    for (path, file) in files {
        output.push_str(&format!("file: {path}  content_hash: {}\n", file.hash));
        for location in file.locations {
            output.push_str(&location);
            output.push('\n');
        }
    }
    if omitted > 0 {
        output.push_str(&format!(
            "{omitted} outside-root, malformed, oversized, or over-cap locations omitted\n"
        ));
    }
    Ok(ToolResult::ok(
        format!("lsp references {} ({count})", prepared.display),
        output.trim_end().to_string(),
    ))
}

fn position_number(position: &Value, key: &str) -> Result<usize, ToolResult> {
    position
        .get(key)
        .and_then(Value::as_u64)
        .filter(|value| *value <= usize::MAX as u64)
        .map(|value| value as usize)
        .ok_or_else(|| {
            ToolResult::err(
                "unsupported_edit",
                format!("text edit position has no valid {key}"),
            )
        })
}

fn collect_uri_edits(result: &Value) -> Result<BTreeMap<String, Vec<Value>>, ToolResult> {
    if result.is_null() {
        return Err(ToolResult::err(
            "no_match",
            "the language server returned no rename edit",
        ));
    }
    let object = result
        .as_object()
        .ok_or_else(|| ToolResult::err("lsp_protocol", "rename result is not a WorkspaceEdit"))?;
    let mut by_uri: BTreeMap<String, Vec<Value>> = BTreeMap::new();
    if let Some(changes) = object.get("changes") {
        let map = changes.as_object().ok_or_else(|| {
            ToolResult::err("lsp_protocol", "WorkspaceEdit.changes is not an object")
        })?;
        for (uri, edits) in map {
            let edits = edits.as_array().ok_or_else(|| {
                ToolResult::err("lsp_protocol", "WorkspaceEdit change is not an edit array")
            })?;
            by_uri
                .entry(uri.clone())
                .or_default()
                .extend(edits.iter().cloned());
        }
    }
    if let Some(document_changes) = object.get("documentChanges") {
        let list = document_changes.as_array().ok_or_else(|| {
            ToolResult::err(
                "unsupported_edit",
                "resource-operation WorkspaceEdits are not supported",
            )
        })?;
        for change in list {
            if change.get("kind").is_some() {
                return Err(ToolResult::err(
                    "unsupported_edit",
                    "rename requested a create/rename/delete resource operation",
                ));
            }
            let uri = change
                .pointer("/textDocument/uri")
                .and_then(Value::as_str)
                .ok_or_else(|| ToolResult::err("lsp_protocol", "TextDocumentEdit has no URI"))?;
            let edits = change
                .get("edits")
                .and_then(Value::as_array)
                .ok_or_else(|| {
                    ToolResult::err("lsp_protocol", "TextDocumentEdit has no edits array")
                })?;
            by_uri
                .entry(uri.to_string())
                .or_default()
                .extend(edits.iter().cloned());
        }
    }
    if by_uri.is_empty() {
        return Err(ToolResult::err(
            "no_match",
            "the language server returned an empty rename edit",
        ));
    }
    let edits: usize = by_uri.values().map(Vec::len).sum();
    if by_uri.len() > MAX_FILES || edits > MAX_EDITS {
        return Err(ToolResult::err("ambiguous_match", format!(
            "rename would edit {} files and {edits} ranges; limits are {MAX_FILES} files and {MAX_EDITS} edits", by_uri.len()
        )));
    }
    Ok(by_uri)
}

#[derive(Clone)]
struct ByteEdit {
    start: usize,
    end: usize,
    replacement: String,
}

fn apply_text_edits(before: &str, edits: &[Value]) -> Result<String, ToolResult> {
    let mut parsed = Vec::with_capacity(edits.len());
    for edit in edits {
        let range = edit.get("range").ok_or_else(|| {
            ToolResult::err(
                "unsupported_edit",
                "rename returned an InsertReplaceEdit instead of a bounded TextEdit",
            )
        })?;
        let start = range
            .get("start")
            .ok_or_else(|| ToolResult::err("unsupported_edit", "text edit has no start"))?;
        let end = range
            .get("end")
            .ok_or_else(|| ToolResult::err("unsupported_edit", "text edit has no end"))?;
        let start_byte = utf16_to_byte(
            before,
            position_number(start, "line")?,
            position_number(start, "character")?,
        )
        .map_err(|detail| ToolResult::err("unsupported_edit", detail))?;
        let end_byte = utf16_to_byte(
            before,
            position_number(end, "line")?,
            position_number(end, "character")?,
        )
        .map_err(|detail| ToolResult::err("unsupported_edit", detail))?;
        if end_byte < start_byte {
            return Err(ToolResult::err(
                "unsupported_edit",
                "text edit ends before it starts",
            ));
        }
        let replacement = edit
            .get("newText")
            .and_then(Value::as_str)
            .ok_or_else(|| ToolResult::err("unsupported_edit", "text edit has no newText"))?;
        parsed.push(ByteEdit {
            start: start_byte,
            end: end_byte,
            replacement: replacement.to_string(),
        });
    }
    parsed.sort_by_key(|edit| (edit.start, edit.end));
    let mut prior: Option<(usize, usize)> = None;
    for edit in &parsed {
        if let Some((start, end)) = prior {
            if edit.start < end || edit.start == start {
                return Err(ToolResult::err(
                    "ambiguous_match",
                    "language server returned overlapping text edits",
                ));
            }
        }
        prior = Some((edit.start, edit.end));
    }
    let mut after = before.to_string();
    for edit in parsed.iter().rev() {
        after.replace_range(edit.start..edit.end, &edit.replacement);
    }
    Ok(after)
}

fn plan_workspace(
    ctx: &ToolCtx,
    result: &Value,
    expected: &BTreeMap<String, String>,
) -> Result<Vec<PendingChange>, ToolResult> {
    let by_uri = collect_uri_edits(result)?;
    let mut changes = Vec::with_capacity(by_uri.len());
    for (uri, edits) in by_uri {
        let (path, display) = project_path(ctx, &uri).map_err(|detail| {
            ToolResult::err(
                "path_denied",
                format!("language server returned {uri}: {detail}"),
            )
        })?;
        let Some(wanted) = expected.get(&display) else {
            return Err(ToolResult::err("stale_anchor", format!(
                "rename would edit {display}, but expected_files has no current hash for it; run references/read first"
            )));
        };
        let before = read_text(&path)?;
        if before.len() > FILE_MAX {
            return Err(ToolResult::err(
                "too_large",
                format!(
                    "{display} is {} bytes; lsp stops at {FILE_MAX}",
                    before.len()
                ),
            ));
        }
        let actual = content_hash(&before);
        if &actual != wanted {
            return Err(ToolResult::err(
                "stale_anchor",
                format!(
                "{display} changed: expected content_hash {wanted}, current content_hash {actual}"
            ),
            ));
        }
        let after = apply_text_edits(&before, &edits)?;
        if after.len() > FILE_MAX {
            return Err(ToolResult::err(
                "too_large",
                format!(
                    "rename would grow {display} to {} bytes; limit is {FILE_MAX}",
                    after.len()
                ),
            ));
        }
        let change = describe(&ctx.root, path, "modify", Some(before), after);
        if !change.is_noop() {
            changes.push(change);
        }
    }
    changes.sort_by(|left, right| left.display.cmp(&right.display));
    if changes.is_empty() {
        return Err(ToolResult::err(
            "no_match",
            "the language server's rename edit changes no text",
        ));
    }
    Ok(changes)
}

fn truncate_utf8(text: &mut String, cap: usize) {
    if text.len() <= cap {
        return;
    }
    const MARKER: &str = "\n…[combined diff truncated]\n";
    let mut end = cap.saturating_sub(MARKER.len());
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    text.truncate(end);
    text.push_str(MARKER);
    if text.len() > cap {
        text.truncate(cap);
    }
}

fn rename_payload(planned: Result<Vec<PendingChange>, ToolResult>, args: &Value) -> Value {
    match planned {
        Err(refusal) => json!({
            "path": args.get("path"), "action": "rename", "error": refusal.error_code
        }),
        Ok(changes) => {
            let plus: usize = changes.iter().map(|change| change.plus).sum();
            let minus: usize = changes.iter().map(|change| change.minus).sum();
            let files: Vec<Value> = changes
                .iter()
                .map(|change| {
                    json!({
                        "path": change.display, "before_hash": change.before_hash,
                        "after_hash": change.after_hash, "plus": change.plus, "minus": change.minus
                    })
                })
                .collect();
            let mut diff = changes
                .iter()
                .map(|change| change.diff.as_str())
                .collect::<Vec<_>>()
                .join("\n");
            truncate_utf8(&mut diff, PERMISSION_DIFF_MAX);
            json!({ "path": args.get("path"), "action": "rename", "file_count": files.len(),
                "plus": plus, "minus": minus, "files": files, "diff": diff })
        }
    }
}

fn apply_workspace(ctx: &ToolCtx, new_name: &str, changes: Vec<PendingChange>) -> ToolResult {
    for change in &changes {
        let current = match read_text(&change.path) {
            Ok(text) => text,
            Err(refusal) => return refusal,
        };
        let actual = content_hash(&current);
        if change.before_hash.as_deref() != Some(actual.as_str()) {
            return ToolResult::err(
                "stale_anchor",
                format!(
                    "{} changed after rename planning; expected {:?}, current {actual}",
                    change.display, change.before_hash
                ),
            );
        }
    }
    let mut applied = Vec::new();
    for (index, change) in changes.iter().enumerate() {
        if let Err(error) = atomic_write(&change.path, &ctx.step_id, &change.after) {
            let mut rollback_errors = Vec::new();
            for applied_index in applied.iter().rev().copied() {
                let prior: &PendingChange = &changes[applied_index];
                if let Some(before) = prior.before.as_deref() {
                    if let Err(rollback) =
                        atomic_write(&prior.path, &format!("{}-rollback", ctx.step_id), before)
                    {
                        rollback_errors.push(format!("{}: {rollback}", prior.display));
                    }
                }
            }
            let rollback = if rollback_errors.is_empty() {
                "prior writes were rolled back".to_string()
            } else {
                format!("rollback also failed for {}", rollback_errors.join(", "))
            };
            return ToolResult::err(
                "write_failed",
                format!("{}: {error}; {rollback}", change.display),
            );
        }
        applied.push(index);
    }
    let plus: usize = changes.iter().map(|change| change.plus).sum();
    let minus: usize = changes.iter().map(|change| change.minus).sum();
    let summary = format!(
        "renamed to {new_name} in {} files (+{plus} −{minus})",
        changes.len()
    );
    let mut output = format!("{summary}\n");
    for change in &changes {
        output.push_str(&format!(
            "{}  content_hash: {}  (+{} −{})\n",
            change.display, change.after_hash, change.plus, change.minus
        ));
    }
    if let Some(command) = ctx.diagnostics_cmd.as_deref() {
        let run = run_capped(
            command,
            &ctx.root,
            &ctx.scope,
            DIAGNOSTICS_TIMEOUT,
            DIAGNOSTICS_CAP,
        );
        output.push_str(&format!(
            "\n[diagnostics {}]\n{}\n",
            run.label(),
            run.output
        ));
    }
    let mut result = ToolResult::ok(summary, output.trim_end().to_string());
    for change in changes {
        result = result.with_artifact(change.artifact());
    }
    result
}

fn plan_rename(ctx: &ToolCtx, args: &Value) -> Result<Vec<PendingChange>, ToolResult> {
    let prepared = prepare(ctx, args)?;
    if prepared.operation != Operation::Rename {
        return Err(ToolResult::err(
            "invalid_arguments",
            "only rename has a workspace edit to plan",
        ));
    }
    let response = query_server(ctx, &prepared)?;
    plan_workspace(ctx, &response, &prepared.expected)
}

pub struct Lsp;
impl Tool for Lsp {
    fn name(&self) -> &'static str {
        "lsp"
    }
    fn schema(&self) -> &'static str {
        include_str!("../../tools/schemas/lsp.json")
    }
    fn side_effecting(&self) -> bool {
        true
    }
    fn side_effecting_for(&self, args: &Value) -> bool {
        args.get("operation").and_then(Value::as_str) == Some("rename")
    }
    fn summary(&self, args: &Value) -> String {
        format!(
            "lsp {} {}",
            args.get("operation").and_then(Value::as_str).unwrap_or("?"),
            args.get("path").and_then(Value::as_str).unwrap_or("?")
        )
    }
    fn permission_payload(&self, ctx: &ToolCtx, args: &Value) -> Value {
        rename_payload(plan_rename(ctx, args), args)
    }
    fn run(&self, ctx: &ToolCtx, args: Value) -> ToolResult {
        let prepared = match prepare(ctx, &args) {
            Ok(prepared) => prepared,
            Err(refusal) => return refusal,
        };
        let response = match query_server(ctx, &prepared) {
            Ok(response) => response,
            Err(refusal) => return refusal,
        };
        match prepared.operation {
            Operation::Diagnostics => {
                format_diagnostics(&prepared, &response).unwrap_or_else(|refusal| refusal)
            }
            Operation::References => {
                format_references(ctx, &prepared, &response).unwrap_or_else(|refusal| refusal)
            }
            Operation::Rename => match plan_workspace(ctx, &response, &prepared.expected) {
                Ok(changes) => {
                    apply_workspace(ctx, prepared.new_name.as_deref().unwrap_or("?"), changes)
                }
                Err(refusal) => refusal,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::{PermissionMode, Registry, ToolStatus};
    use std::io::Cursor;

    fn project() -> (PathBuf, ToolCtx) {
        let dir = std::env::temp_dir().join(format!("harness-lsp-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("src/lib.rs"), "pub fn old() {}\n").unwrap();
        std::fs::write(dir.join("src/use.rs"), "fn call() { old(); }\n").unwrap();
        let root = std::fs::canonicalize(&dir).unwrap();
        let ctx = ToolCtx {
            root: root.clone(),
            scope: "global".into(),
            request_id: "request".into(),
            step_id: "step-lsp".into(),
            diagnostics_cmd: None,
        };
        (root, ctx)
    }

    fn expected(root: &Path) -> BTreeMap<String, String> {
        ["src/lib.rs", "src/use.rs"]
            .into_iter()
            .map(|path| {
                let text = std::fs::read_to_string(root.join(path)).unwrap();
                (path.to_string(), content_hash(&text))
            })
            .collect()
    }

    fn workspace(root: &Path) -> Value {
        let mut changes = serde_json::Map::new();
        let lib = Url::from_file_path(root.join("src/lib.rs"))
            .unwrap()
            .to_string();
        let usage = Url::from_file_path(root.join("src/use.rs"))
            .unwrap()
            .to_string();
        changes.insert(
            lib,
            json!([{ "range": { "start": { "line": 0, "character": 7 },
            "end": { "line": 0, "character": 10 } }, "newText": "new" }]),
        );
        changes.insert(
            usage,
            json!([{ "range": { "start": { "line": 0, "character": 12 },
            "end": { "line": 0, "character": 15 } }, "newText": "new" }]),
        );
        json!({ "changes": Value::Object(changes) })
    }

    #[test]
    fn positions_are_one_based_unicode_outside_and_utf16_inside() {
        let text = "a😀b\n";
        assert_eq!(
            model_to_lsp(text, 1, 3).unwrap(),
            json!({ "line": 0, "character": 3 })
        );
        assert_eq!(
            lsp_to_model(text, &json!({ "line": 0, "character": 3 })).unwrap(),
            (1, 3)
        );
        assert!(model_to_lsp(text, 1, 6).is_err());
        assert!(
            utf16_to_byte(text, 0, 2).is_err(),
            "must not split the emoji's surrogate pair"
        );
    }

    #[test]
    fn workspace_rename_plans_without_writing_then_applies_two_artifacts() {
        let (root, ctx) = project();
        let planned = plan_workspace(&ctx, &workspace(&root), &expected(&root)).unwrap();
        assert_eq!(planned.len(), 2);
        assert!(std::fs::read_to_string(root.join("src/lib.rs"))
            .unwrap()
            .contains("old"));
        let result = apply_workspace(&ctx, "new", planned);
        assert_eq!(result.status, ToolStatus::Complete, "{}", result.content);
        assert_eq!(result.artifacts.len(), 2);
        assert!(std::fs::read_to_string(root.join("src/lib.rs"))
            .unwrap()
            .contains("new"));
        assert!(std::fs::read_to_string(root.join("src/use.rs"))
            .unwrap()
            .contains("new"));
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn workspace_rename_refuses_unlisted_and_stale_files_without_writing() {
        let (root, ctx) = project();
        let mut one = expected(&root);
        one.remove("src/use.rs");
        assert_eq!(
            plan_workspace(&ctx, &workspace(&root), &one)
                .unwrap_err()
                .error_code,
            Some("stale_anchor")
        );
        let mut stale = expected(&root);
        stale.insert("src/use.rs".into(), "00000000".into());
        assert_eq!(
            plan_workspace(&ctx, &workspace(&root), &stale)
                .unwrap_err()
                .error_code,
            Some("stale_anchor")
        );
        assert!(std::fs::read_to_string(root.join("src/use.rs"))
            .unwrap()
            .contains("old"));
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn workspace_rename_rechecks_every_hash_before_the_first_write() {
        let (root, ctx) = project();
        let planned = plan_workspace(&ctx, &workspace(&root), &expected(&root)).unwrap();
        std::fs::write(root.join("src/use.rs"), "changed outside the tool\n").unwrap();
        let result = apply_workspace(&ctx, "new", planned);
        assert_eq!(result.status, ToolStatus::Failed);
        assert_eq!(result.error_code, Some("stale_anchor"));
        assert!(std::fs::read_to_string(root.join("src/lib.rs"))
            .unwrap()
            .contains("old"));
        assert_eq!(
            std::fs::read_to_string(root.join("src/use.rs")).unwrap(),
            "changed outside the tool\n"
        );
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn protocol_reader_is_framed_and_bounded() {
        let body = br#"{"jsonrpc":"2.0","id":1,"result":null}"#;
        let mut bytes = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
        bytes.extend_from_slice(body);
        assert_eq!(
            read_frame(&mut Cursor::new(bytes)).unwrap().unwrap()["id"],
            1
        );
        let mut missing = Cursor::new(b"Content-Type: x\r\n\r\n{}".to_vec());
        assert!(read_frame(&mut missing).is_err());
        let mut diff = "x".repeat(PERMISSION_DIFF_MAX + 100);
        truncate_utf8(&mut diff, PERMISSION_DIFF_MAX);
        assert!(diff.len() <= PERMISSION_DIFF_MAX);
    }

    #[test]
    fn missing_server_and_resource_operations_are_bounded_refusals() {
        let (_root, ctx) = project();
        let spec = ServerSpec {
            program: "harness-definitely-missing-language-server",
            args: &[],
            language_id: "rust",
            rust: true,
        };
        let error = match Session::start(&ctx, spec, Duration::from_secs(1)) {
            Ok(_) => panic!("the impossible executable unexpectedly started"),
            Err(error) => error,
        };
        assert_eq!(error.error_code, Some("lsp_unavailable"));
        let resource = json!({
            "documentChanges": [{ "kind": "create", "uri": "file:///tmp/new.rs" }]
        });
        assert_eq!(
            collect_uri_edits(&resource).unwrap_err().error_code,
            Some("unsupported_edit")
        );
        std::fs::remove_dir_all(ctx.root).ok();
    }

    #[test]
    fn only_rename_enters_the_permission_gate() {
        let registry = Registry::standard();
        let read =
            json!({ "operation": "references", "path": "src/lib.rs", "line": 1, "column": 8 });
        let rename = json!({ "operation": "rename", "path": "src/lib.rs", "line": 1, "column": 8 });
        assert!(Lsp.side_effecting());
        assert!(!Lsp.side_effecting_for(&read));
        assert!(Lsp.side_effecting_for(&rename));
        assert!(!registry.requires_permission(&Lsp, &read, PermissionMode::Ask));
        assert!(registry.requires_permission(&Lsp, &rename, PermissionMode::Ask));
        assert!(!registry.requires_permission(&Lsp, &rename, PermissionMode::AutoEdit));
    }
}
