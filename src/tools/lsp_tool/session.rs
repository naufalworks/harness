//! One bounded stdio LSP server process per call.
//!
//! This is the transport half of the `lsp` tool: spawning the language server,
//! framing JSON-RPC over stdio, enforcing the frame and stderr caps, and
//! tearing the process group down when the call ends or times out. Argument
//! handling and result formatting stay in the parent module.
use super::*;

pub(super) type Frame = Result<Value, String>;

pub(super) struct Session {
    pub(super) child: Child,
    pub(super) input: ChildStdin,
    pub(super) frames: Receiver<Frame>,
    pub(super) stderr: Receiver<String>,
    pub(super) deadline: Instant,
    pub(super) root_uri: String,
    pub(super) program: &'static str,
    pub(super) stopped: bool,
}

impl Session {
    pub(super) fn start(
        ctx: &ToolCtx,
        spec: ServerSpec,
        timeout: Duration,
    ) -> Result<Self, ToolResult> {
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

    pub(super) fn send(&mut self, value: &Value) -> Result<(), ToolResult> {
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

    pub(super) fn request(
        &mut self,
        id: i64,
        method: &str,
        params: Value,
    ) -> Result<(), ToolResult> {
        self.send(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }))
    }

    pub(super) fn notify(&mut self, method: &str, params: Value) -> Result<(), ToolResult> {
        self.send(&json!({ "jsonrpc": "2.0", "method": method, "params": params }))
    }

    pub(super) fn disconnected(&mut self) -> ToolResult {
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

    pub(super) fn receive(&mut self) -> Result<Value, ToolResult> {
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

    pub(super) fn answer_server_request(&mut self, message: &Value) -> Result<bool, ToolResult> {
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

    pub(super) fn wait_response(&mut self, wanted: i64) -> Result<Value, ToolResult> {
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

    pub(super) fn wait_diagnostics(&mut self, uri: &str) -> Result<Value, ToolResult> {
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

    pub(super) fn initialize(&mut self, spec: ServerSpec) -> Result<(), ToolResult> {
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

    pub(super) fn open(&mut self, prepared: &Prepared) -> Result<(), ToolResult> {
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

    pub(super) fn stop(&mut self) {
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

pub(super) fn read_frame(reader: &mut impl BufRead) -> Result<Option<Value>, String> {
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
pub(super) fn kill_group(pid: u32) {
    let _ = Command::new("sh")
        .arg("-c")
        .arg(format!("kill -9 -{pid} 2>/dev/null"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}
#[cfg(not(unix))]
pub(super) fn kill_group(_pid: u32) {}
