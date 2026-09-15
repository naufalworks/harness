//! Argument parsing, bounds and destination policy for the `browser` tool.
//!
//! Every operation enters through `parse_call`, so this module is the single
//! place where untrusted arguments become a validated `Call`: caps, key
//! allowlist, snapshot-id shape and the loopback/private-address rules all
//! live here rather than next to the CDP transport.
use serde_json::Value;
use std::thread;
use std::time::{Duration, Instant};
use url::Url;

use crate::tools::ToolResult;

pub(super) const CDP_MESSAGE_MAX: usize = 2 * 1024 * 1024;
pub(super) const HTTP_RESPONSE_MAX: usize = 1024 * 1024;
pub(super) const AX_INPUT_MAX: usize = 5_000;
pub(super) const SNAPSHOT_NODE_MAX: usize = 400;
pub(super) const DEFAULT_NODES: usize = 200;
pub(super) const DEFAULT_WAIT_MS: u64 = 500;
pub(super) const MAX_WAIT_MS: u64 = 5_000;
pub(super) const DEFAULT_TIMEOUT: u64 = 15;
pub(super) const MAX_TIMEOUT: u64 = 30;
pub(super) const STARTUP_TIMEOUT: u64 = 10;
pub(super) const TEXT_MAX: usize = 8 * 1024;
pub(super) const URL_MAX: usize = 2 * 1024;
pub(super) const LABEL_MAX: usize = 240;
pub(super) const EVENT_MAX: usize = 1_000;
pub(super) const STDERR_MAX: usize = 8 * 1024;
pub(super) const SCREENSHOT_MAX_BYTES: usize = 1_500_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Operation {
    Open,
    Snapshot,
    Click,
    Type,
    Press,
    Close,
    Screenshot,
}

impl Operation {
    pub(super) fn parse(args: &Value) -> BrowserResult<Self> {
        match args.get("operation").and_then(Value::as_str) {
            Some("open") => Ok(Self::Open),
            Some("snapshot") => Ok(Self::Snapshot),
            Some("click") => Ok(Self::Click),
            Some("type") => Ok(Self::Type),
            Some("press") => Ok(Self::Press),
            Some("close") => Ok(Self::Close),
            Some("screenshot") => Ok(Self::Screenshot),
            Some(other) => Err(Failure::invalid(format!(
                "unknown browser operation {other:?}; expected open, snapshot, click, type, press, screenshot, or close"
            ))),
            None => Err(Failure::invalid("operation is required")),
        }
    }

    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Snapshot => "snapshot",
            Self::Click => "click",
            Self::Type => "type",
            Self::Press => "press",
            Self::Close => "close",
            Self::Screenshot => "screenshot",
        }
    }

    pub(super) fn interactive(self) -> bool {
        matches!(self, Self::Click | Self::Type | Self::Press)
    }
}

#[derive(Debug)]
pub(super) struct Failure {
    pub(super) code: &'static str,
    pub(super) detail: String,
}

pub(super) type BrowserResult<T> = Result<T, Failure>;

impl Failure {
    pub(super) fn new(code: &'static str, detail: impl Into<String>) -> Self {
        Self {
            code,
            detail: detail.into(),
        }
    }

    pub(super) fn invalid(detail: impl Into<String>) -> Self {
        Self::new("invalid_arguments", detail)
    }

    pub(super) fn into_tool(self) -> ToolResult {
        ToolResult::err(self.code, self.detail)
    }
}

#[derive(Clone, Debug)]
pub(super) struct Call {
    pub(super) operation: Operation,
    pub(super) url: Option<String>,
    pub(super) snapshot_id: Option<String>,
    pub(super) backend_ref: Option<u64>,
    pub(super) text: Option<String>,
    pub(super) key: Option<&'static str>,
    pub(super) submit: bool,
    pub(super) wait: Duration,
    pub(super) max_nodes: usize,
    pub(super) timeout: Duration,
}

pub(super) fn whole_number(
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

pub(super) fn parse_bool(args: &Value, key: &str, default: bool) -> BrowserResult<bool> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(default),
        Some(Value::Bool(value)) => Ok(*value),
        Some(_) => Err(Failure::invalid(format!("{key} must be a boolean"))),
    }
}

pub(super) fn private_destination_allowed() -> bool {
    matches!(
        std::env::var("HARNESS_BROWSER_ALLOW_PRIVATE_NETWORK").as_deref(),
        Ok("1") | Ok("true")
    )
}

pub(super) fn forbidden_ip(ip: std::net::IpAddr) -> bool {
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

pub(super) fn validate_destination(raw: &str) -> BrowserResult<Url> {
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

pub(super) fn parse_open_url(raw: &str) -> BrowserResult<String> {
    if raw.is_empty() || raw.len() > URL_MAX || raw.contains('\0') {
        return Err(Failure::invalid("url must be 1-2048 bytes with no NUL"));
    }
    Ok(validate_destination(raw)?.to_string())
}

pub(super) fn valid_snapshot_id(value: &str) -> bool {
    value.len() == 8
        && value
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

pub(super) fn parse_backend_ref(raw: &str) -> BrowserResult<u64> {
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

pub(super) fn parse_key(raw: &str) -> BrowserResult<&'static str> {
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

pub(super) fn parse_call(args: &Value) -> BrowserResult<Call> {
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

pub(super) fn remaining(deadline: Instant) -> BrowserResult<Duration> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|duration| !duration.is_zero())
        .ok_or_else(|| Failure::new("timeout", "browser call reached its deadline"))
}

pub(super) fn settle(wait: Duration, deadline: Instant) -> BrowserResult<()> {
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
