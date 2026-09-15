//! LSP diagnostics, references and approval-safe workspace rename.
//! Contract: docs/design/tools.md#lsp.
//!
//! A small, root-keyed stdio session pool keeps language-server startup bounded without creating
//! a daemon: at most two sessions live per registry, idle sessions expire after 30 seconds, and
//! protocol, timeout, process-exit, or pool failures tear the session down. Read-only calls never
//! ask for approval. Rename plans a workspace edit for the permission card, then queries again
//! after approval and rechecks every supplied file hash.
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashSet};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use url::Url;

use super::edit_tools::{atomic_write, describe, run_capped_for};
use super::fs_tools::read_text;
use super::{content_hash, paths, PendingChange, Tool, ToolCtx, ToolResult};

mod protocol;
use protocol::*;

mod session;
use session::*;

fn query_server(session: &mut Session, prepared: &Prepared) -> Result<Value, ToolResult> {
    session.reset_deadline(prepared.timeout);
    let result = (|| {
        if !session.is_initialized() {
            session.initialize(prepared.server)?;
        }
        session.open(prepared)?;
        match prepared.operation {
            Operation::Discover => unreachable!("discover is handled before query_server"),
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
    result
}

mod format;
use format::*;

mod rename;
use rename::*;

const SESSION_POOL_MAX: usize = 2;
const SESSION_IDLE: Duration = Duration::from_secs(30);

struct PooledSession {
    root: PathBuf,
    program: &'static str,
    session: Session,
    last_used: Instant,
}

pub struct Lsp {
    sessions: Mutex<Vec<PooledSession>>,
}

impl Default for Lsp {
    fn default() -> Self {
        Self {
            sessions: Mutex::new(Vec::new()),
        }
    }
}

impl Lsp {
    fn take_session(
        &self,
        ctx: &ToolCtx,
        server: ServerSpec,
        timeout: Duration,
    ) -> Result<Session, ToolResult> {
        let mut pool = self.sessions.lock().map_err(|_| {
            ToolResult::err(
                "lsp_unavailable",
                "language-server session pool was poisoned",
            )
        })?;
        pool.retain_mut(|item| {
            if item.last_used.elapsed() > SESSION_IDLE {
                item.session.stop();
                false
            } else {
                true
            }
        });
        if let Some(index) = pool
            .iter()
            .position(|item| item.root == ctx.root && item.program == server.program)
        {
            let mut pooled = pool.swap_remove(index);
            if pooled.session.reusable() {
                return Ok(pooled.session);
            }
            let mut session = pooled.session;
            session.stop();
        }
        Session::start(ctx, server, timeout)
    }

    fn return_session(&self, root: &Path, program: &'static str, session: Session) {
        let Ok(mut pool) = self.sessions.lock() else {
            let mut session = session;
            session.stop();
            return;
        };
        pool.retain_mut(|item| {
            if item.root == root && item.program == program {
                item.session.stop();
                false
            } else {
                true
            }
        });
        pool.push(PooledSession {
            root: root.to_path_buf(),
            program,
            session,
            last_used: Instant::now(),
        });
        while pool.len() > SESSION_POOL_MAX {
            let mut evicted = pool.remove(0).session;
            evicted.stop();
        }
    }
}

fn executable_available(program: &str) -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|dir| dir.join(program).is_file())
}

fn discover(ctx: &ToolCtx, args: &Value) -> ToolResult {
    let Some(raw) = args.get("path").and_then(Value::as_str) else {
        return ToolResult::err("invalid_arguments", "path is required");
    };
    let path = match paths::resolve(&ctx.root, raw) {
        Ok(path) => path,
        Err(error) => return ToolResult::err(error.code(), error.detail()),
    };
    let display = paths::display(&ctx.root, &path);
    let Some(server) = server_for(&path) else {
        return ToolResult::ok(
            "lsp discover",
            json!({
                "path": display,
                "supported": false,
                "reason": "no configured language server for this extension"
            })
            .to_string(),
        );
    };
    ToolResult::ok(
        "lsp discover",
        json!({
            "path": display,
            "supported": true,
            "language": server.language_id,
            "server": server.program,
            "available": executable_available(server.program),
            "session_policy": "bounded root-keyed stdio pool: 2 sessions maximum, 30s idle expiry"
        })
        .to_string(),
    )
}

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
        if args.get("operation").and_then(Value::as_str) == Some("discover") {
            return discover(ctx, &args);
        }
        let prepared = match prepare(ctx, &args) {
            Ok(prepared) => prepared,
            Err(refusal) => return refusal,
        };
        let mut session = match self.take_session(ctx, prepared.server, prepared.timeout) {
            Ok(session) => session,
            Err(refusal) => return refusal,
        };
        let response = match query_server(&mut session, &prepared) {
            Ok(response) => response,
            Err(refusal) => {
                session.stop();
                return refusal;
            }
        };
        let result = match prepared.operation {
            Operation::Discover => unreachable!("discover is handled before query_server"),
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
        };
        self.return_session(&ctx.root, prepared.server.program, session);
        result
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
    fn discovery_reports_configured_language_and_unsupported_extensions() {
        let (root, ctx) = project();
        let lsp = Lsp::default();
        let rust = lsp.run(
            &ctx,
            json!({ "operation": "discover", "path": "src/lib.rs" }),
        );
        assert_eq!(rust.status, ToolStatus::Complete, "{}", rust.content);
        let rust: Value = serde_json::from_str(&rust.content).unwrap();
        assert_eq!(rust["supported"], true);
        assert_eq!(rust["language"], "rust");
        assert_eq!(rust["server"], "rust-analyzer");
        assert_eq!(
            rust["session_policy"],
            "bounded root-keyed stdio pool: 2 sessions maximum, 30s idle expiry"
        );

        std::fs::write(root.join("README.txt"), "plain text\n").unwrap();
        let text = lsp.run(
            &ctx,
            json!({ "operation": "discover", "path": "README.txt" }),
        );
        assert_eq!(text.status, ToolStatus::Complete, "{}", text.content);
        let text: Value = serde_json::from_str(&text.content).unwrap();
        assert_eq!(text["supported"], false);
        assert_eq!(
            text["reason"],
            "no configured language server for this extension"
        );
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn only_rename_enters_the_permission_gate() {
        let registry = Registry::standard();
        let read =
            json!({ "operation": "references", "path": "src/lib.rs", "line": 1, "column": 8 });
        let rename = json!({ "operation": "rename", "path": "src/lib.rs", "line": 1, "column": 8 });
        let lsp = Lsp::default();
        assert!(lsp.side_effecting());
        assert!(!lsp.side_effecting_for(&read));
        assert!(lsp.side_effecting_for(&rename));
        assert!(!registry.requires_permission(&lsp, &read, PermissionMode::Ask));
        assert!(registry.requires_permission(&lsp, &rename, PermissionMode::Ask));
        assert!(!registry.requires_permission(&lsp, &rename, PermissionMode::AutoEdit));
    }
}
