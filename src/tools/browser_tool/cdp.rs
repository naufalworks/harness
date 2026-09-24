//! P12-T02 browser seam 2: the Chrome DevTools Protocol transport and the browser
//! process this tool owns. The `Cdp` request/response loop, the endpoint and
//! destination checks around it, and the launched-Chrome lifetime (`OwnedBrowser`,
//! its `Drop`, and `launch_browser`) move here from `src/tools/browser_tool.rs`.
//! The code is unchanged apart from `pub(super)` markers, the minimum needed for
//! the parent to keep calling it. Every cap (`CDP_MESSAGE_MAX`, `EVENT_MAX`,
//! `HTTP_RESPONSE_MAX`, `STARTUP_TIMEOUT`, `STDERR_MAX`, `URL_MAX`) is still
//! defined once in the parent, so no bound moved or changed.

use super::{
    remaining, truncate_chars, BrowserResult, Failure, CDP_MESSAGE_MAX, EVENT_MAX,
    HTTP_RESPONSE_MAX, STARTUP_TIMEOUT, STDERR_MAX,
};
use serde_json::{json, Value};
use std::fs::{self, File};
use std::io::{Read, Write};
use std::net::{IpAddr, SocketAddr, TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};
use tungstenite::client::client_with_config;
use tungstenite::protocol::WebSocketConfig;
use tungstenite::{Error as WsError, Message, WebSocket};
use url::Url;

pub(super) type CdpSocket = WebSocket<TcpStream>;

pub(super) struct Cdp {
    pub(super) socket: CdpSocket,
    pub(super) next_id: u64,
}

pub(super) fn socket_timeout(socket: &mut CdpSocket, timeout: Duration) -> BrowserResult<()> {
    socket
        .get_mut()
        .set_read_timeout(Some(timeout))
        .and_then(|_| socket.get_mut().set_write_timeout(Some(timeout)))
        .map_err(|error| {
            Failure::new(
                "browser_protocol",
                format!("could not bound CDP socket: {error}"),
            )
        })
}

pub(super) fn websocket_failure(error: WsError, method: &str) -> Failure {
    match error {
        WsError::Io(io)
            if matches!(
                io.kind(),
                std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
            ) =>
        {
            Failure::new("timeout", format!("CDP {method} timed out"))
        }
        WsError::ConnectionClosed | WsError::AlreadyClosed => Failure::new(
            "browser_closed",
            format!("CDP target closed while running {method}; call browser open again"),
        ),
        other => Failure::new("browser_protocol", format!("CDP {method} failed: {other}")),
    }
}

impl Cdp {
    pub(super) fn call(
        &mut self,
        method: &str,
        params: Value,
        deadline: Instant,
    ) -> BrowserResult<Value> {
        let id = self.next_id;
        self.next_id = self
            .next_id
            .checked_add(1)
            .ok_or_else(|| Failure::new("browser_protocol", "CDP request id overflow"))?;
        let body = json!({ "id": id, "method": method, "params": params }).to_string();
        if body.len() > CDP_MESSAGE_MAX {
            return Err(Failure::new(
                "browser_protocol",
                format!("CDP request exceeds the {CDP_MESSAGE_MAX}-byte cap"),
            ));
        }
        socket_timeout(&mut self.socket, remaining(deadline)?)?;
        self.socket
            .send(Message::text(body))
            .map_err(|error| websocket_failure(error, method))?;

        let mut events = 0usize;
        loop {
            if events >= EVENT_MAX {
                return Err(Failure::new(
                    "browser_protocol",
                    format!("CDP emitted more than {EVENT_MAX} unrelated messages"),
                ));
            }
            socket_timeout(&mut self.socket, remaining(deadline)?)?;
            let frame = self
                .socket
                .read()
                .map_err(|error| websocket_failure(error, method))?;
            let text = match frame {
                Message::Text(text) => text.to_string(),
                Message::Binary(bytes) => String::from_utf8(bytes.to_vec()).map_err(|_| {
                    Failure::new("browser_protocol", "CDP sent non-UTF-8 binary JSON")
                })?,
                Message::Ping(_) | Message::Pong(_) => {
                    let _ = self.socket.flush();
                    events += 1;
                    continue;
                }
                Message::Close(_) => {
                    return Err(Failure::new(
                        "browser_closed",
                        format!(
                            "CDP target closed while running {method}; call browser open again"
                        ),
                    ));
                }
                Message::Frame(_) => {
                    events += 1;
                    continue;
                }
            };
            if text.len() > CDP_MESSAGE_MAX {
                return Err(Failure::new(
                    "browser_protocol",
                    format!("CDP response exceeds the {CDP_MESSAGE_MAX}-byte cap"),
                ));
            }
            let message: Value = serde_json::from_str(&text).map_err(|error| {
                Failure::new(
                    "browser_protocol",
                    format!("CDP sent invalid JSON: {error}"),
                )
            })?;
            if message.get("id").and_then(Value::as_u64) != Some(id) {
                events += 1;
                continue;
            }
            if let Some(error) = message.get("error") {
                let code = error
                    .get("code")
                    .and_then(Value::as_i64)
                    .unwrap_or_default();
                let detail = error
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown CDP error");
                return Err(Failure::new(
                    "browser_protocol",
                    format!("CDP {method} error {code}: {}", truncate_chars(detail, 500)),
                ));
            }
            return Ok(message.get("result").cloned().unwrap_or(Value::Null));
        }
    }
}

pub(super) fn validate_endpoint(raw: &str) -> BrowserResult<(String, SocketAddr)> {
    if raw.is_empty() || raw.len() > 4096 || raw.contains('\0') {
        return Err(Failure::new(
            "browser_unavailable",
            "HARNESS_CDP_URL must be a non-empty loopback ws:// URL",
        ));
    }
    let url = Url::parse(raw).map_err(|error| {
        Failure::new(
            "browser_unavailable",
            format!("invalid HARNESS_CDP_URL: {error}"),
        )
    })?;
    if url.scheme() != "ws" || !url.username().is_empty() || url.password().is_some() {
        return Err(Failure::new(
            "browser_unavailable",
            "HARNESS_CDP_URL must be a credential-free loopback ws:// URL",
        ));
    }
    let host = url
        .host_str()
        .ok_or_else(|| Failure::new("browser_unavailable", "HARNESS_CDP_URL needs a host"))?;
    let port = url
        .port_or_known_default()
        .ok_or_else(|| Failure::new("browser_unavailable", "HARNESS_CDP_URL needs a port"))?;
    let mut addresses = (host, port).to_socket_addrs().map_err(|error| {
        Failure::new(
            "browser_unavailable",
            format!("could not resolve HARNESS_CDP_URL: {error}"),
        )
    })?;
    let address = addresses
        .find(SocketAddr::is_ipv4)
        .or_else(|| {
            (host, port)
                .to_socket_addrs()
                .ok()?
                .find(SocketAddr::is_ipv6)
        })
        .ok_or_else(|| {
            Failure::new(
                "browser_unavailable",
                "HARNESS_CDP_URL resolved to no address",
            )
        })?;
    let loopback = match address.ip() {
        IpAddr::V4(ip) => ip.is_loopback(),
        IpAddr::V6(ip) => ip.is_loopback(),
    };
    if host != "localhost" && !loopback {
        return Err(Failure::new(
            "browser_unavailable",
            "HARNESS_CDP_URL must resolve to loopback",
        ));
    }
    if !loopback {
        return Err(Failure::new(
            "browser_unavailable",
            "HARNESS_CDP_URL must resolve to loopback",
        ));
    }
    Ok((url.to_string(), address))
}

pub(super) fn connect_cdp(endpoint: &str, deadline: Instant) -> BrowserResult<Cdp> {
    let (endpoint, address) = validate_endpoint(endpoint)?;
    let timeout = remaining(deadline)?;
    let stream = TcpStream::connect_timeout(&address, timeout).map_err(|error| {
        Failure::new(
            "browser_unavailable",
            format!("could not connect to the CDP page target: {error}"),
        )
    })?;
    stream.set_nodelay(true).map_err(|error| {
        Failure::new(
            "browser_protocol",
            format!("could not configure CDP socket: {error}"),
        )
    })?;
    stream
        .set_read_timeout(Some(remaining(deadline)?))
        .and_then(|_| {
            stream.set_write_timeout(Some(remaining(deadline).unwrap_or(Duration::from_secs(1))))
        })
        .map_err(|error| {
            Failure::new(
                "browser_protocol",
                format!("could not bound CDP handshake: {error}"),
            )
        })?;
    let config = WebSocketConfig::default()
        .read_buffer_size(16 * 1024)
        .write_buffer_size(0)
        .max_write_buffer_size(CDP_MESSAGE_MAX + 4096)
        .max_message_size(Some(CDP_MESSAGE_MAX))
        .max_frame_size(Some(CDP_MESSAGE_MAX));
    let (socket, _) =
        client_with_config(endpoint.as_str(), stream, Some(config)).map_err(|error| {
            Failure::new(
                "browser_unavailable",
                format!("CDP WebSocket handshake failed: {error}"),
            )
        })?;
    let mut cdp = Cdp { socket, next_id: 1 };
    for domain in [
        "Page.enable",
        "Runtime.enable",
        "DOM.enable",
        "Accessibility.enable",
    ] {
        cdp.call(domain, json!({}), deadline)?;
    }
    Ok(cdp)
}

#[cfg(unix)]
unsafe extern "C" {
    fn setpgid(pid: i32, pgid: i32) -> i32;
    fn kill(pid: i32, signal: i32) -> i32;
}

#[cfg(unix)]
pub(super) fn isolate_process_group(command: &mut Command) {
    use std::os::unix::process::CommandExt;
    // SAFETY: pre_exec runs after fork, where only async-signal-safe operations are allowed.
    // The closure calls only setpgid(0, 0) and converts errno into io::Error; it captures
    // no mutable Rust state and allocates nothing on the success path.
    unsafe {
        command.pre_exec(|| {
            if setpgid(0, 0) == 0 {
                Ok(())
            } else {
                Err(std::io::Error::last_os_error())
            }
        });
    }
}

#[cfg(not(unix))]
pub(super) fn isolate_process_group(_command: &mut Command) {}

pub(super) struct OwnedBrowser {
    pub(super) child: Child,
    pub(super) profile: PathBuf,
    pub(super) stderr_path: PathBuf,
}

impl OwnedBrowser {
    pub(super) fn stderr_tail(&self) -> String {
        let Ok(bytes) = fs::read(&self.stderr_path) else {
            return String::new();
        };
        let start = bytes.len().saturating_sub(STDERR_MAX);
        String::from_utf8_lossy(&bytes[start..]).trim().to_string()
    }
}

impl Drop for OwnedBrowser {
    fn drop(&mut self) {
        #[cfg(unix)]
        if let Ok(pid) = i32::try_from(self.child.id()) {
            // Never negate an out-of-range pid: a wrapped value would name some other group.
            // SAFETY: pid came from this owned child and was range-checked before negation.
            // kill takes integers only and does not access Rust-managed memory.
            unsafe {
                let _ = kill(-pid, 9);
            }
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = fs::remove_dir_all(&self.profile);
    }
}

pub(super) fn path_program(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|directory| directory.join(name))
        .find(|candidate| candidate.is_file())
}

pub(super) fn browser_program() -> BrowserResult<PathBuf> {
    if let Some(configured) = std::env::var_os("HARNESS_BROWSER_PATH") {
        let candidate = PathBuf::from(configured);
        if candidate.is_file() {
            return Ok(candidate);
        }
        return Err(Failure::new(
            "browser_unavailable",
            format!(
                "HARNESS_BROWSER_PATH is not a file: {}",
                candidate.display()
            ),
        ));
    }

    for candidate in [
        "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
        "/Applications/Chromium.app/Contents/MacOS/Chromium",
        "/usr/bin/google-chrome",
        "/usr/bin/google-chrome-stable",
        "/usr/bin/chromium",
        "/usr/bin/chromium-browser",
    ] {
        let path = PathBuf::from(candidate);
        if path.is_file() {
            return Ok(path);
        }
    }
    for candidate in [
        "google-chrome",
        "google-chrome-stable",
        "chromium",
        "chromium-browser",
        "chrome",
    ] {
        if let Some(path) = path_program(candidate) {
            return Ok(path);
        }
    }
    Err(Failure::new(
        "browser_unavailable",
        "no Chrome/Chromium binary found; set HARNESS_BROWSER_PATH to an executable or HARNESS_CDP_URL to a loopback page target",
    ))
}

pub(super) fn http_json_list(port: u16) -> std::io::Result<Vec<Value>> {
    let address = SocketAddr::from(([127, 0, 0, 1], port));
    let mut stream = TcpStream::connect_timeout(&address, Duration::from_millis(300))?;
    stream.set_read_timeout(Some(Duration::from_millis(500)))?;
    stream.set_write_timeout(Some(Duration::from_millis(500)))?;
    write!(
        stream,
        "GET /json/list HTTP/1.0\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n"
    )?;
    let mut bytes = Vec::new();
    stream
        .take((HTTP_RESPONSE_MAX + 1) as u64)
        .read_to_end(&mut bytes)?;
    if bytes.len() > HTTP_RESPONSE_MAX {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "CDP /json/list response exceeded 1 MiB",
        ));
    }
    let split = bytes
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, "invalid HTTP response")
        })?;
    let head = String::from_utf8_lossy(&bytes[..split]);
    if !head.lines().next().unwrap_or_default().contains(" 200 ") {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "CDP /json/list returned {}",
                head.lines().next().unwrap_or("no status")
            ),
        ));
    }
    serde_json::from_slice(&bytes[split + 4..])
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))
}

pub(super) fn page_endpoint(port: u16) -> BrowserResult<Option<String>> {
    let targets = match http_json_list(port) {
        Ok(targets) => targets,
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::ConnectionRefused
                    | std::io::ErrorKind::ConnectionReset
                    | std::io::ErrorKind::TimedOut
                    | std::io::ErrorKind::WouldBlock
                    | std::io::ErrorKind::UnexpectedEof
            ) =>
        {
            return Ok(None)
        }
        Err(error) => {
            return Err(Failure::new(
                "browser_protocol",
                format!("could not read Chrome target list: {error}"),
            ))
        }
    };
    let endpoint = targets
        .iter()
        .find(|target| target.get("type").and_then(Value::as_str) == Some("page"))
        .and_then(|target| target.get("webSocketDebuggerUrl"))
        .and_then(Value::as_str);
    match endpoint {
        Some(endpoint) => Ok(Some(validate_endpoint(endpoint)?.0)),
        None => Ok(None),
    }
}

pub(super) fn launch_browser(root: &Path, deadline: Instant) -> BrowserResult<(Cdp, OwnedBrowser)> {
    let program = browser_program()?;
    let profile = std::env::temp_dir().join(format!("harness-browser-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&profile).map_err(|error| {
        Failure::new(
            "browser_unavailable",
            format!("could not create isolated browser profile: {error}"),
        )
    })?;
    let stderr_path = profile.join("browser-stderr.log");
    let stderr = File::create(&stderr_path).map_err(|error| {
        let _ = fs::remove_dir_all(&profile);
        Failure::new(
            "browser_unavailable",
            format!("could not create browser startup log: {error}"),
        )
    })?;

    let mut command = Command::new(&program);
    command
        .current_dir(root)
        .args([
            "--headless=new",
            "--no-first-run",
            "--no-default-browser-check",
            "--disable-background-networking",
            "--disable-component-update",
            "--disable-sync",
            "--disable-default-apps",
            "--disable-extensions",
            "--metrics-recording-only",
            "--mute-audio",
            "--hide-scrollbars",
            "--remote-debugging-address=127.0.0.1",
            "--remote-debugging-port=0",
        ])
        .arg(format!("--user-data-dir={}", profile.display()))
        .arg("about:blank")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(stderr))
        .env_clear();
    for key in ["PATH", "HOME", "LANG", "LC_ALL", "TERM", "TMPDIR"] {
        if let Some(value) = std::env::var_os(key) {
            command.env(key, value);
        }
    }
    isolate_process_group(&mut command);
    let child = command.spawn().map_err(|error| {
        let _ = fs::remove_dir_all(&profile);
        Failure::new(
            "browser_unavailable",
            format!("could not start {}: {error}", program.display()),
        )
    })?;
    let mut owned = OwnedBrowser {
        child,
        profile,
        stderr_path,
    };

    let startup_deadline = std::cmp::min(
        deadline,
        Instant::now() + Duration::from_secs(STARTUP_TIMEOUT),
    );
    let active_port = owned.profile.join("DevToolsActivePort");
    loop {
        if let Some(status) = owned.child.try_wait().map_err(|error| {
            Failure::new(
                "browser_unavailable",
                format!("could not inspect Chrome: {error}"),
            )
        })? {
            let stderr = owned.stderr_tail();
            return Err(Failure::new(
                "browser_unavailable",
                format!(
                    "Chrome exited during startup ({status}){}",
                    if stderr.is_empty() {
                        String::new()
                    } else {
                        format!(": {stderr}")
                    }
                ),
            ));
        }
        if let Ok(text) = fs::read_to_string(&active_port) {
            if let Some(port) = text
                .lines()
                .next()
                .and_then(|line| line.parse::<u16>().ok())
            {
                if let Some(endpoint) = page_endpoint(port)? {
                    let cdp = connect_cdp(&endpoint, deadline)?;
                    return Ok((cdp, owned));
                }
            }
        }
        if Instant::now() >= startup_deadline {
            let stderr = owned.stderr_tail();
            return Err(Failure::new(
                "browser_unavailable",
                format!(
                    "Chrome did not expose a page target within {STARTUP_TIMEOUT}s{}",
                    if stderr.is_empty() {
                        String::new()
                    } else {
                        format!(": {stderr}")
                    }
                ),
            ));
        }
        thread::sleep(Duration::from_millis(50));
    }
}
