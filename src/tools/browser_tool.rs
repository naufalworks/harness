//! `browser`: one turn-scoped Chrome DevTools Protocol page.
//! Contract: docs/design/tools.md#browser.
//!
//! A registry lives for one agent turn, so this tool keeps one mutex-serialized page target across
//! that turn and drops it afterwards. Read-only snapshots need no approval; every input action is
//! anchored to the exact latest accessibility snapshot before it is dispatched.
use serde_json::{json, Value};
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant};
use url::Url;

use super::{content_hash, truncate_chars, Tool, ToolCtx, ToolResult};

const CDP_MESSAGE_MAX: usize = 2 * 1024 * 1024;
const HTTP_RESPONSE_MAX: usize = 1024 * 1024;
const AX_INPUT_MAX: usize = 5_000;
const SNAPSHOT_NODE_MAX: usize = 400;
const DEFAULT_NODES: usize = 200;
const DEFAULT_WAIT_MS: u64 = 500;
const MAX_WAIT_MS: u64 = 5_000;
const DEFAULT_TIMEOUT: u64 = 15;
const MAX_TIMEOUT: u64 = 30;
const STARTUP_TIMEOUT: u64 = 10;
const TEXT_MAX: usize = 8 * 1024;
const URL_MAX: usize = 2 * 1024;
const LABEL_MAX: usize = 240;
const EVENT_MAX: usize = 1_000;
const STDERR_MAX: usize = 8 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Operation {
    Open,
    Snapshot,
    Click,
    Type,
    Press,
    Close,
}

impl Operation {
    fn parse(args: &Value) -> BrowserResult<Self> {
        match args.get("operation").and_then(Value::as_str) {
            Some("open") => Ok(Self::Open),
            Some("snapshot") => Ok(Self::Snapshot),
            Some("click") => Ok(Self::Click),
            Some("type") => Ok(Self::Type),
            Some("press") => Ok(Self::Press),
            Some("close") => Ok(Self::Close),
            Some(other) => Err(Failure::invalid(format!(
                "unknown browser operation {other:?}; expected open, snapshot, click, type, press, or close"
            ))),
            None => Err(Failure::invalid("operation is required")),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Snapshot => "snapshot",
            Self::Click => "click",
            Self::Type => "type",
            Self::Press => "press",
            Self::Close => "close",
        }
    }

    fn interactive(self) -> bool {
        matches!(self, Self::Click | Self::Type | Self::Press)
    }
}

#[derive(Debug)]
struct Failure {
    code: &'static str,
    detail: String,
}

type BrowserResult<T> = Result<T, Failure>;

impl Failure {
    fn new(code: &'static str, detail: impl Into<String>) -> Self {
        Self {
            code,
            detail: detail.into(),
        }
    }

    fn invalid(detail: impl Into<String>) -> Self {
        Self::new("invalid_arguments", detail)
    }

    fn into_tool(self) -> ToolResult {
        ToolResult::err(self.code, self.detail)
    }
}

#[derive(Clone, Debug)]
struct Call {
    operation: Operation,
    url: Option<String>,
    snapshot_id: Option<String>,
    backend_ref: Option<u64>,
    text: Option<String>,
    key: Option<&'static str>,
    submit: bool,
    wait: Duration,
    max_nodes: usize,
    timeout: Duration,
}

fn whole_number(
    args: &Value,
    key: &str,
    default: u64,
    minimum: u64,
    maximum: u64,
) -> BrowserResult<u64> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(default),
        Some(value) => match value.as_u64() {
            Some(number) if (minimum..=maximum).contains(&number) => Ok(number),
            _ => Err(Failure::invalid(format!(
                "{key} must be a whole number from {minimum} through {maximum}"
            ))),
        },
    }
}

fn parse_bool(args: &Value, key: &str, default: bool) -> BrowserResult<bool> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(default),
        Some(Value::Bool(value)) => Ok(*value),
        Some(_) => Err(Failure::invalid(format!("{key} must be a boolean"))),
    }
}

fn private_destination_allowed() -> bool {
    matches!(
        std::env::var("HARNESS_BROWSER_ALLOW_PRIVATE_NETWORK").as_deref(),
        Ok("1") | Ok("true")
    )
}

fn forbidden_ip(ip: std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(ip) => {
            ip.is_private()
                || ip.is_loopback()
                || ip.is_link_local()
                || ip.is_unspecified()
                || ip.is_multicast()
                || ip.octets()[0] == 0
                || (ip.octets()[0] == 100 && (64..=127).contains(&ip.octets()[1]))
        }
        std::net::IpAddr::V6(ip) => {
            ip.is_loopback()
                || ip.is_unspecified()
                || ip.is_multicast()
                || (ip.segments()[0] & 0xfe00) == 0xfc00
                || (ip.segments()[0] & 0xffc0) == 0xfe80
        }
    }
}

fn validate_destination(raw: &str) -> BrowserResult<Url> {
    let url = Url::parse(raw).map_err(|error| Failure::invalid(format!("invalid url: {error}")))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(Failure::invalid(
            "browser open accepts only http:// or https:// URLs",
        ));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(Failure::invalid(
            "browser URLs must not contain credentials",
        ));
    }
    let host = url
        .host_str()
        .ok_or_else(|| Failure::invalid("browser URL needs a host"))?
        .trim_end_matches('.')
        .to_ascii_lowercase();
    if !private_destination_allowed()
        && (host == "localhost"
            || host.ends_with(".localhost")
            || host == "metadata.google.internal"
            || host
                .trim_start_matches('[')
                .trim_end_matches(']')
                .parse::<std::net::IpAddr>()
                .is_ok_and(forbidden_ip))
    {
        return Err(Failure::new("browser_destination_denied", "private, loopback, link-local, metadata, and special-use destinations require HARNESS_BROWSER_ALLOW_PRIVATE_NETWORK=true"));
    }
    Ok(url)
}

fn parse_open_url(raw: &str) -> BrowserResult<String> {
    if raw.is_empty() || raw.len() > URL_MAX || raw.contains('\0') {
        return Err(Failure::invalid("url must be 1-2048 bytes with no NUL"));
    }
    Ok(validate_destination(raw)?.to_string())
}

fn valid_snapshot_id(value: &str) -> bool {
    value.len() == 8
        && value
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

fn parse_backend_ref(raw: &str) -> BrowserResult<u64> {
    let Some(digits) = raw.strip_prefix('b') else {
        return Err(Failure::invalid("ref must look like b42"));
    };
    if digits.is_empty()
        || digits.starts_with('0')
        || !digits.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(Failure::invalid("ref must look like b42"));
    }
    digits
        .parse::<u64>()
        .ok()
        .filter(|value| *value > 0)
        .ok_or_else(|| Failure::invalid("ref is outside the supported numeric range"))
}

fn parse_key(raw: &str) -> BrowserResult<&'static str> {
    match raw {
        "Enter" => Ok("Enter"),
        "Tab" => Ok("Tab"),
        "Escape" => Ok("Escape"),
        "Backspace" => Ok("Backspace"),
        "ArrowUp" => Ok("ArrowUp"),
        "ArrowDown" => Ok("ArrowDown"),
        "ArrowLeft" => Ok("ArrowLeft"),
        "ArrowRight" => Ok("ArrowRight"),
        "PageUp" => Ok("PageUp"),
        "PageDown" => Ok("PageDown"),
        "Home" => Ok("Home"),
        "End" => Ok("End"),
        "Space" => Ok("Space"),
        _ => Err(Failure::invalid(
            "key must be Enter, Tab, Escape, Backspace, an arrow, PageUp/PageDown, Home/End, or Space",
        )),
    }
}

fn parse_call(args: &Value) -> BrowserResult<Call> {
    let operation = Operation::parse(args)?;
    let wait = Duration::from_millis(whole_number(
        args,
        "wait_ms",
        DEFAULT_WAIT_MS,
        0,
        MAX_WAIT_MS,
    )?);
    let max_nodes = whole_number(
        args,
        "max_nodes",
        DEFAULT_NODES as u64,
        1,
        SNAPSHOT_NODE_MAX as u64,
    )? as usize;
    let timeout = Duration::from_secs(whole_number(
        args,
        "timeout_seconds",
        DEFAULT_TIMEOUT,
        1,
        MAX_TIMEOUT,
    )?);
    let submit = parse_bool(args, "submit", false)?;

    let url = match operation {
        Operation::Open => {
            let raw = args
                .get("url")
                .and_then(Value::as_str)
                .ok_or_else(|| Failure::invalid("open requires url"))?;
            Some(parse_open_url(raw)?)
        }
        _ => None,
    };

    let snapshot_id = if operation.interactive() {
        let value = args
            .get("snapshot_id")
            .and_then(Value::as_str)
            .ok_or_else(|| Failure::invalid("click, type, and press require snapshot_id"))?;
        if !valid_snapshot_id(value) {
            return Err(Failure::invalid(
                "snapshot_id must be exactly eight lowercase hex characters",
            ));
        }
        Some(value.to_string())
    } else {
        None
    };

    let backend_ref = if matches!(operation, Operation::Click | Operation::Type) {
        let value = args
            .get("ref")
            .and_then(Value::as_str)
            .ok_or_else(|| Failure::invalid("click and type require ref"))?;
        Some(parse_backend_ref(value)?)
    } else {
        None
    };

    let text = if operation == Operation::Type {
        let value = args
            .get("text")
            .and_then(Value::as_str)
            .ok_or_else(|| Failure::invalid("type requires text"))?;
        if value.len() > TEXT_MAX || value.contains('\0') {
            return Err(Failure::invalid(
                "text must be at most 8 KiB and contain no NUL",
            ));
        }
        Some(value.to_string())
    } else {
        None
    };

    let key = if operation == Operation::Press {
        Some(parse_key(
            args.get("key")
                .and_then(Value::as_str)
                .ok_or_else(|| Failure::invalid("press requires key"))?,
        )?)
    } else {
        None
    };

    if submit && operation != Operation::Type {
        return Err(Failure::invalid("submit is only valid for type"));
    }

    Ok(Call {
        operation,
        url,
        snapshot_id,
        backend_ref,
        text,
        key,
        submit,
        wait,
        max_nodes,
        timeout,
    })
}

fn remaining(deadline: Instant) -> BrowserResult<Duration> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|duration| !duration.is_zero())
        .ok_or_else(|| Failure::new("timeout", "browser call reached its deadline"))
}

fn settle(wait: Duration, deadline: Instant) -> BrowserResult<()> {
    if wait.is_zero() {
        return Ok(());
    }
    let left = remaining(deadline)?;
    if wait > left {
        return Err(Failure::new(
            "timeout",
            "browser settle delay would exceed the call deadline",
        ));
    }
    thread::sleep(wait);
    Ok(())
}

mod snapshot;

mod cdp;

mod session;
use session::*;

pub struct Browser {
    session: Mutex<Option<Session>>,
    endpoint_override: Option<String>,
}

impl Default for Browser {
    fn default() -> Self {
        Self {
            session: Mutex::new(None),
            endpoint_override: None,
        }
    }
}

impl Browser {
    #[cfg(test)]
    fn for_endpoint(endpoint: String) -> Self {
        Self {
            session: Mutex::new(None),
            endpoint_override: Some(endpoint),
        }
    }

    fn permission(&self, args: &Value) -> Value {
        let call = match parse_call(args) {
            Ok(call) => call,
            Err(error) => {
                return json!({
                    "operation": args.get("operation"),
                    "error": error.code,
                    "detail": error.detail,
                })
            }
        };
        let mut payload = json!({
            "operation": call.operation.as_str(),
            "snapshot_id": call.snapshot_id,
            "ref": call.backend_ref.map(|backend| format!("b{backend}")),
            "text": call.text,
            "key": call.key,
            "submit": call.submit,
        });
        let Ok(session) = self.session.lock() else {
            payload["error"] = json!("browser_closed");
            return payload;
        };
        let Some(snapshot) = session.as_ref().and_then(|session| session.last.as_ref()) else {
            payload["error"] = json!("browser_closed");
            return payload;
        };
        payload["url"] = json!(snapshot.url);
        if snapshot.id != call.snapshot_id.as_deref().unwrap_or_default() {
            payload["error"] = json!("stale_anchor");
            return payload;
        }
        if let Some(backend) = call.backend_ref {
            match snapshot.refs.get(&backend) {
                Some(target) => {
                    payload["target"] = json!({
                        "role": target.role,
                        "name": target.name,
                        "value": target.value,
                        "states": target.states,
                    });
                }
                None => payload["error"] = json!("stale_anchor"),
            }
        }
        payload
    }
}

impl Tool for Browser {
    fn name(&self) -> &'static str {
        "browser"
    }

    fn schema(&self) -> &'static str {
        include_str!("../../tools/schemas/browser.json")
    }

    fn side_effecting(&self) -> bool {
        true
    }

    fn side_effecting_for(&self, args: &Value) -> bool {
        matches!(
            args.get("operation").and_then(Value::as_str),
            Some("click" | "type" | "press")
        )
    }

    fn summary(&self, args: &Value) -> String {
        summary_for(args)
    }

    fn permission_payload(&self, _ctx: &ToolCtx, args: &Value) -> Value {
        self.permission(args)
    }

    fn run(&self, ctx: &ToolCtx, args: Value) -> ToolResult {
        let summary = summary_for(&args);
        let call = match parse_call(&args) {
            Ok(call) => call,
            Err(error) => return error.into_tool(),
        };
        let mut guard = match self.session.lock() {
            Ok(guard) => guard,
            Err(_) => {
                return Failure::new(
                    "browser_closed",
                    "browser session lock was poisoned; start a new turn",
                )
                .into_tool()
            }
        };

        if call.operation == Operation::Close {
            let was_open = guard.take().is_some();
            return ToolResult::ok(
                summary,
                if was_open {
                    "browser session closed".to_string()
                } else {
                    "browser session was already closed".to_string()
                },
            );
        }

        let deadline = Instant::now() + call.timeout;
        if call.operation == Operation::Open && guard.is_none() {
            match start_session(&ctx.root, self.endpoint_override.as_deref(), deadline) {
                Ok(session) => *guard = Some(session),
                Err(error) => return error.into_tool(),
            }
        }
        let Some(session) = guard.as_mut() else {
            return Failure::new(
                "browser_closed",
                "no browser is open in this turn; call browser open first",
            )
            .into_tool();
        };

        let result = match call.operation {
            Operation::Open => session.navigate(
                call.url.as_deref().expect("open URL was validated"),
                call.wait,
                deadline,
            ),
            Operation::Snapshot => {
                settle(call.wait, deadline).and_then(|_| session.capture_and_store(deadline))
            }
            Operation::Click => anchored(session, &call, deadline).and_then(|_| {
                click(
                    session,
                    call.backend_ref.expect("click ref was validated"),
                    call.wait,
                    deadline,
                )
            }),
            Operation::Type => anchored(session, &call, deadline).and_then(|(_, target)| {
                type_text(
                    session,
                    call.backend_ref.expect("type ref was validated"),
                    target.as_ref().expect("type target was resolved"),
                    call.text.as_deref().expect("type text was validated"),
                    call.submit,
                    call.wait,
                    deadline,
                )
            }),
            Operation::Press => anchored(session, &call, deadline).and_then(|_| {
                press(
                    session,
                    call.key.expect("press key was validated"),
                    call.wait,
                    deadline,
                )
            }),
            Operation::Close => unreachable!(),
        };

        match result {
            Ok(snapshot) => ToolResult::ok(summary, snapshot.render(call.max_nodes)),
            Err(error) => {
                if reset_after(&error) {
                    guard.take();
                }
                error.into_tool()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::cdp::validate_endpoint;
    use super::snapshot::snapshot_from_ax;
    use super::*;
    use crate::tools::{PermissionMode, Registry, ToolStatus};
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use tungstenite::Message;

    fn context() -> ToolCtx {
        ToolCtx {
            root: std::env::temp_dir(),
            scope: "global".to_string(),
            request_id: "request".to_string(),
            step_id: "step".to_string(),
            diagnostics_cmd: None,
        }
    }

    struct FakeCdp {
        endpoint: String,
        clicks: Arc<AtomicUsize>,
        revision: Arc<AtomicUsize>,
        typed: Arc<Mutex<String>>,
        handle: Option<std::thread::JoinHandle<()>>,
    }

    impl FakeCdp {
        fn start() -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            let clicks = Arc::new(AtomicUsize::new(0));
            let revision = Arc::new(AtomicUsize::new(0));
            let typed = Arc::new(Mutex::new(String::new()));
            let thread_clicks = Arc::clone(&clicks);
            let thread_revision = Arc::clone(&revision);
            let thread_typed = Arc::clone(&typed);
            let handle = std::thread::spawn(move || {
                let (stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(10)))
                    .unwrap();
                let mut socket = tungstenite::accept(stream).unwrap();
                let mut page_url = "about:blank".to_string();
                loop {
                    let Ok(message) = socket.read() else {
                        break;
                    };
                    if message.is_close() {
                        break;
                    }
                    let request: Value = serde_json::from_str(message.to_text().unwrap()).unwrap();
                    let id = request["id"].as_u64().unwrap();
                    let method = request["method"].as_str().unwrap();
                    let params = &request["params"];
                    let result = match method {
                        "Page.enable"
                        | "Runtime.enable"
                        | "DOM.enable"
                        | "Accessibility.enable" => json!({}),
                        "Page.navigate" => {
                            page_url = params["url"].as_str().unwrap().to_string();
                            json!({ "frameId": "frame-1" })
                        }
                        "Runtime.evaluate" => json!({
                            "result": {
                                "type": "object",
                                "value": { "url": page_url, "title": "Fixture" }
                            }
                        }),
                        "Accessibility.getFullAXTree" => {
                            let typed = thread_typed.lock().unwrap().clone();
                            let mut nodes = vec![
                                json!({
                                    "nodeId": "1", "ignored": false,
                                    "role": { "value": "RootWebArea" },
                                    "name": { "value": "Fixture" },
                                    "backendDOMNodeId": 1,
                                    "childIds": ["2", "3"]
                                }),
                                json!({
                                    "nodeId": "2", "parentId": "1", "ignored": false,
                                    "role": { "value": "textbox" },
                                    "name": { "value": "Name" },
                                    "value": { "value": typed },
                                    "backendDOMNodeId": 8
                                }),
                                json!({
                                    "nodeId": "3", "parentId": "1", "ignored": false,
                                    "role": { "value": "button" },
                                    "name": { "value": "Save" },
                                    "backendDOMNodeId": 7
                                }),
                            ];
                            if thread_revision.load(Ordering::SeqCst) > 0 {
                                nodes.push(json!({
                                    "nodeId": "4", "parentId": "1", "ignored": false,
                                    "role": { "value": "StaticText" },
                                    "name": { "value": "Saved" },
                                    "backendDOMNodeId": 9
                                }));
                            }
                            json!({ "nodes": nodes })
                        }
                        "DOM.scrollIntoViewIfNeeded" => json!({}),
                        "DOM.getBoxModel" => json!({
                            "model": { "content": [0.0, 0.0, 100.0, 0.0, 100.0, 40.0, 0.0, 40.0] }
                        }),
                        "Input.dispatchMouseEvent" => {
                            if params["type"] == "mouseReleased" {
                                thread_clicks.fetch_add(1, Ordering::SeqCst);
                                thread_revision.fetch_add(1, Ordering::SeqCst);
                            }
                            json!({})
                        }
                        "DOM.resolveNode" => json!({ "object": { "objectId": "object-8" } }),
                        "Runtime.callFunctionOn" => {
                            *thread_typed.lock().unwrap() = params["arguments"][0]["value"]
                                .as_str()
                                .unwrap()
                                .to_string();
                            json!({ "result": { "type": "object", "value": { "ok": true } } })
                        }
                        "Runtime.releaseObject" | "Input.dispatchKeyEvent" => json!({}),
                        other => {
                            socket
                                .send(Message::text(
                                    json!({
                                        "id": id,
                                        "error": { "code": -32601, "message": format!("unknown {other}") }
                                    })
                                    .to_string(),
                                ))
                                .unwrap();
                            continue;
                        }
                    };
                    socket
                        .send(Message::text(
                            json!({ "id": id, "result": result }).to_string(),
                        ))
                        .unwrap();
                }
            });
            Self {
                endpoint: format!("ws://{address}/devtools/page/fixture"),
                clicks,
                revision,
                typed,
                handle: Some(handle),
            }
        }

        fn join(mut self) {
            if let Some(handle) = self.handle.take() {
                handle.join().unwrap();
            }
        }
    }

    fn snapshot_id(content: &str) -> String {
        content
            .lines()
            .find_map(|line| line.strip_prefix("snapshot_id: "))
            .unwrap()
            .to_string()
    }

    #[test]
    fn arguments_and_operator_endpoints_are_strictly_bounded() {
        assert!(parse_call(&json!({ "operation": "open", "url": "https://example.test" })).is_ok());
        for args in [
            json!({ "operation": "open", "url": "file:///etc/passwd" }),
            json!({ "operation": "open", "url": "https://user:secret@example.test" }),
            json!({ "operation": "click", "snapshot_id": "bad", "ref": "b7" }),
            json!({ "operation": "type", "snapshot_id": "12345678", "ref": "b0", "text": "x" }),
            json!({ "operation": "press", "snapshot_id": "12345678", "key": "F12" }),
            json!({ "operation": "snapshot", "wait_ms": 5001 }),
        ] {
            assert_eq!(
                parse_call(&args).unwrap_err().code,
                "invalid_arguments",
                "{args}"
            );
        }
        for url in [
            "http://127.0.0.1/admin",
            "http://169.254.169.254/latest/meta-data",
            "http://[::1]/",
        ] {
            assert_eq!(
                parse_call(&json!({ "operation": "open", "url": url }))
                    .unwrap_err()
                    .code,
                "browser_destination_denied"
            );
        }
        assert!(validate_endpoint("ws://127.0.0.1:9222/devtools/page/1").is_ok());
        assert_eq!(
            validate_endpoint("ws://example.test:9222/devtools/page/1")
                .unwrap_err()
                .code,
            "browser_unavailable"
        );
    }

    #[test]
    fn snapshots_are_bounded_deterministic_and_marked_untrusted() {
        let tree = json!({ "nodes": [
            { "nodeId": "1", "ignored": false, "role": { "value": "RootWebArea" }, "name": { "value": "Fixture" }, "backendDOMNodeId": 1 },
            { "nodeId": "2", "parentId": "1", "ignored": false, "role": { "value": "button" }, "name": { "value": "Save\nnow" }, "backendDOMNodeId": 7 }
        ] });
        let first =
            snapshot_from_ax("https://example.test".into(), "Fixture".into(), &tree).unwrap();
        let second =
            snapshot_from_ax("https://example.test".into(), "Fixture".into(), &tree).unwrap();
        assert_eq!(first.id, second.id);
        assert_eq!(first.id.len(), 8);
        let rendered = first.render(1);
        assert!(rendered.contains("untrusted data"), "{rendered}");
        assert!(rendered.contains("snapshot_id:"), "{rendered}");
        assert!(rendered.contains("semantic nodes omitted"), "{rendered}");
    }

    #[test]
    fn open_click_and_type_share_one_anchored_cdp_page() {
        let fake = FakeCdp::start();
        let browser = Browser::for_endpoint(fake.endpoint.clone());
        let opened = browser.run(
            &context(),
            json!({
                "operation": "open", "url": "http://example.test/form",
                "wait_ms": 0, "timeout_seconds": 5
            }),
        );
        assert_eq!(opened.status, ToolStatus::Complete, "{}", opened.content);
        assert!(
            opened.content.contains("[ref=b7] button \"Save\""),
            "{}",
            opened.content
        );
        assert!(
            opened.content.contains("[ref=b8] textbox \"Name\""),
            "{}",
            opened.content
        );
        let first_id = snapshot_id(&opened.content);

        let clicked = browser.run(
            &context(),
            json!({
                "operation": "click", "snapshot_id": first_id, "ref": "b7",
                "wait_ms": 0, "timeout_seconds": 5
            }),
        );
        assert_eq!(clicked.status, ToolStatus::Complete, "{}", clicked.content);
        assert_eq!(fake.clicks.load(Ordering::SeqCst), 1);
        assert!(clicked.content.contains("Saved"), "{}", clicked.content);
        let second_id = snapshot_id(&clicked.content);

        let typed = browser.run(
            &context(),
            json!({
                "operation": "type", "snapshot_id": second_id, "ref": "b8",
                "text": "Ada", "submit": true, "wait_ms": 0, "timeout_seconds": 5
            }),
        );
        assert_eq!(typed.status, ToolStatus::Complete, "{}", typed.content);
        assert_eq!(&*fake.typed.lock().unwrap(), "Ada");
        assert!(typed.content.contains("value=\"Ada\""), "{}", typed.content);

        let closed = browser.run(&context(), json!({ "operation": "close" }));
        assert_eq!(closed.status, ToolStatus::Complete);
        drop(browser);
        fake.join();
    }

    #[test]
    fn a_changed_page_refuses_stale_input_before_dispatch() {
        let fake = FakeCdp::start();
        let browser = Browser::for_endpoint(fake.endpoint.clone());
        let opened = browser.run(
            &context(),
            json!({ "operation": "open", "url": "http://example.test", "wait_ms": 0, "timeout_seconds": 5 }),
        );
        let old_id = snapshot_id(&opened.content);
        fake.revision.store(1, Ordering::SeqCst);
        let refused = browser.run(
            &context(),
            json!({
                "operation": "click", "snapshot_id": old_id, "ref": "b7",
                "wait_ms": 0, "timeout_seconds": 5
            }),
        );
        assert_eq!(
            refused.error_code,
            Some("stale_anchor"),
            "{}",
            refused.content
        );
        assert_eq!(fake.clicks.load(Ordering::SeqCst), 0);
        let _ = browser.run(&context(), json!({ "operation": "close" }));
        drop(browser);
        fake.join();
    }

    #[test]
    fn only_browser_input_enters_the_permission_gate() {
        let registry = Registry::standard();
        let browser = registry.get("browser").unwrap();
        let open = json!({ "operation": "open", "url": "https://example.test" });
        let snapshot = json!({ "operation": "snapshot" });
        let close = json!({ "operation": "close" });
        let click = json!({ "operation": "click", "snapshot_id": "12345678", "ref": "b7" });
        let typed =
            json!({ "operation": "type", "snapshot_id": "12345678", "ref": "b8", "text": "x" });
        let press = json!({ "operation": "press", "snapshot_id": "12345678", "key": "Enter" });
        assert!(browser.side_effecting());
        for args in [&open, &snapshot, &close] {
            assert!(!browser.side_effecting_for(args));
            assert!(!registry.requires_permission(browser, args, PermissionMode::Ask));
        }
        for args in [&click, &typed, &press] {
            assert!(browser.side_effecting_for(args));
            assert!(registry.requires_permission(browser, args, PermissionMode::Ask));
            assert!(!registry.requires_permission(browser, args, PermissionMode::AutoEdit));
        }
    }
}
