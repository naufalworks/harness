//! P12-T02 browser seam 3: the live page session and the input actions performed
//! against it. `Session` (capture/navigate), session start-up, snapshot anchoring,
//! and the click/type/press dispatch move here from `src/tools/browser_tool.rs`.
//! The code is unchanged apart from `pub(super)` markers. Every cap and every
//! argument-validation helper still lives once in the parent, so no bound, no
//! destination check and no permission decision moved.

use super::cdp::{connect_cdp, launch_browser, Cdp, OwnedBrowser};
use super::snapshot::{snapshot_from_ax, RefView, Snapshot};
use super::{settle, truncate_chars, validate_destination, BrowserResult, Call, Failure};
use serde_json::{json, Value};
use std::path::Path;
use std::time::{Duration, Instant};

pub(super) struct Session {
    pub(super) cdp: Cdp,
    /// Held, never read: dropping the session must also drop any browser this session
    /// launched, so ownership lives here even though no code inspects it.
    #[allow(dead_code)]
    pub(super) owned: Option<OwnedBrowser>,
    pub(super) last: Option<Snapshot>,
}

impl Session {
    pub(super) fn capture(&mut self, deadline: Instant) -> BrowserResult<Snapshot> {
        let info = self.cdp.call(
            "Runtime.evaluate",
            json!({
                "expression": "({url: String(location.href), title: String(document.title)})",
                "returnByValue": true,
            }),
            deadline,
        )?;
        if info.get("exceptionDetails").is_some() {
            return Err(Failure::new(
                "browser_protocol",
                "could not inspect the current page URL/title",
            ));
        }
        let value = info
            .get("result")
            .and_then(|result| result.get("value"))
            .ok_or_else(|| {
                Failure::new(
                    "browser_protocol",
                    "Runtime.evaluate returned no page value",
                )
            })?;
        let url = value
            .get("url")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        validate_destination(&url)?;
        let title = value
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let tree = self.cdp.call(
            "Accessibility.getFullAXTree",
            json!({ "depth": 24 }),
            deadline,
        )?;
        snapshot_from_ax(url, title, &tree)
    }

    pub(super) fn capture_and_store(&mut self, deadline: Instant) -> BrowserResult<Snapshot> {
        let snapshot = self.capture(deadline)?;
        self.last = Some(snapshot.clone());
        Ok(snapshot)
    }

    pub(super) fn navigate(
        &mut self,
        url: &str,
        wait: Duration,
        deadline: Instant,
    ) -> BrowserResult<Snapshot> {
        let result = self
            .cdp
            .call("Page.navigate", json!({ "url": url }), deadline)?;
        if let Some(error) = result.get("errorText").and_then(Value::as_str) {
            if !error.is_empty() {
                return Err(Failure::new(
                    "navigation_failed",
                    format!("could not open {url}: {}", truncate_chars(error, 500)),
                ));
            }
        }
        settle(wait, deadline)?;
        self.capture_and_store(deadline)
    }
}

pub(super) fn start_session(
    root: &Path,
    endpoint_override: Option<&str>,
    deadline: Instant,
) -> BrowserResult<Session> {
    if let Some(endpoint) = endpoint_override
        .map(str::to_string)
        .or_else(|| std::env::var("HARNESS_CDP_URL").ok())
    {
        return Ok(Session {
            cdp: connect_cdp(&endpoint, deadline)?,
            owned: None,
            last: None,
        });
    }
    let (cdp, owned) = launch_browser(root, deadline)?;
    Ok(Session {
        cdp,
        owned: Some(owned),
        last: None,
    })
}

pub(super) fn stale_snapshot(snapshot: &Snapshot, max_nodes: usize, detail: &str) -> Failure {
    Failure::new(
        "stale_anchor",
        format!("{detail}\n\ncurrent {}", snapshot.render(max_nodes)),
    )
}

pub(super) fn anchored(
    session: &mut Session,
    call: &Call,
    deadline: Instant,
) -> BrowserResult<(Snapshot, Option<RefView>)> {
    let requested = call
        .snapshot_id
        .as_deref()
        .ok_or_else(|| Failure::invalid("interactive operation needs snapshot_id"))?;
    let Some(last) = session.last.as_ref() else {
        return Err(Failure::new(
            "stale_anchor",
            "no snapshot exists in this browser turn; call snapshot first",
        ));
    };
    if last.id != requested {
        return Err(stale_snapshot(
            last,
            call.max_nodes,
            "snapshot_id is not the latest snapshot returned in this turn",
        ));
    }

    let fresh = session.capture_and_store(deadline)?;
    if fresh.id != requested {
        return Err(stale_snapshot(
            &fresh,
            call.max_nodes,
            "the page changed after the referenced snapshot; no input was dispatched",
        ));
    }
    let target = match call.backend_ref {
        Some(backend) => Some(fresh.refs.get(&backend).cloned().ok_or_else(|| {
            stale_snapshot(
                &fresh,
                call.max_nodes,
                &format!("ref b{backend} is no longer present; no input was dispatched"),
            )
        })?),
        None => None,
    };
    Ok((fresh, target))
}

pub(super) fn click(
    session: &mut Session,
    backend: u64,
    wait: Duration,
    deadline: Instant,
) -> BrowserResult<Snapshot> {
    session.cdp.call(
        "DOM.scrollIntoViewIfNeeded",
        json!({ "backendNodeId": backend }),
        deadline,
    )?;
    let box_model = session.cdp.call(
        "DOM.getBoxModel",
        json!({ "backendNodeId": backend }),
        deadline,
    )?;
    let quad = box_model
        .get("model")
        .and_then(|model| model.get("content"))
        .and_then(Value::as_array)
        .ok_or_else(|| {
            Failure::new(
                "browser_protocol",
                format!("ref b{backend} has no box model"),
            )
        })?;
    if quad.len() != 8 {
        return Err(Failure::new(
            "browser_protocol",
            format!("ref b{backend} returned an invalid box model"),
        ));
    }
    let numbers = quad
        .iter()
        .map(Value::as_f64)
        .collect::<Option<Vec<_>>>()
        .ok_or_else(|| {
            Failure::new(
                "browser_protocol",
                "box model contains non-numeric coordinates",
            )
        })?;
    let x = (numbers[0] + numbers[2] + numbers[4] + numbers[6]) / 4.0;
    let y = (numbers[1] + numbers[3] + numbers[5] + numbers[7]) / 4.0;
    if !x.is_finite() || !y.is_finite() {
        return Err(Failure::new(
            "browser_protocol",
            "box model contains non-finite coordinates",
        ));
    }
    for event in [
        json!({ "type": "mouseMoved", "x": x, "y": y }),
        json!({ "type": "mousePressed", "x": x, "y": y, "button": "left", "clickCount": 1 }),
        json!({ "type": "mouseReleased", "x": x, "y": y, "button": "left", "clickCount": 1 }),
    ] {
        session
            .cdp
            .call("Input.dispatchMouseEvent", event, deadline)?;
    }
    settle(wait, deadline)?;
    session.capture_and_store(deadline)
}

pub(super) struct KeySpec {
    pub(super) key: &'static str,
    pub(super) code: &'static str,
    pub(super) virtual_key: u64,
    pub(super) text: &'static str,
}

pub(super) fn key_spec(key: &str) -> KeySpec {
    match key {
        "Enter" => KeySpec {
            key: "Enter",
            code: "Enter",
            virtual_key: 13,
            text: "\r",
        },
        "Tab" => KeySpec {
            key: "Tab",
            code: "Tab",
            virtual_key: 9,
            text: "",
        },
        "Escape" => KeySpec {
            key: "Escape",
            code: "Escape",
            virtual_key: 27,
            text: "",
        },
        "Backspace" => KeySpec {
            key: "Backspace",
            code: "Backspace",
            virtual_key: 8,
            text: "",
        },
        "ArrowLeft" => KeySpec {
            key: "ArrowLeft",
            code: "ArrowLeft",
            virtual_key: 37,
            text: "",
        },
        "ArrowUp" => KeySpec {
            key: "ArrowUp",
            code: "ArrowUp",
            virtual_key: 38,
            text: "",
        },
        "ArrowRight" => KeySpec {
            key: "ArrowRight",
            code: "ArrowRight",
            virtual_key: 39,
            text: "",
        },
        "ArrowDown" => KeySpec {
            key: "ArrowDown",
            code: "ArrowDown",
            virtual_key: 40,
            text: "",
        },
        "PageUp" => KeySpec {
            key: "PageUp",
            code: "PageUp",
            virtual_key: 33,
            text: "",
        },
        "PageDown" => KeySpec {
            key: "PageDown",
            code: "PageDown",
            virtual_key: 34,
            text: "",
        },
        "End" => KeySpec {
            key: "End",
            code: "End",
            virtual_key: 35,
            text: "",
        },
        "Home" => KeySpec {
            key: "Home",
            code: "Home",
            virtual_key: 36,
            text: "",
        },
        "Space" => KeySpec {
            key: " ",
            code: "Space",
            virtual_key: 32,
            text: " ",
        },
        _ => unreachable!("keys are validated before dispatch"),
    }
}

pub(super) fn dispatch_key(
    session: &mut Session,
    key: &str,
    deadline: Instant,
) -> BrowserResult<()> {
    let spec = key_spec(key);
    session.cdp.call(
        "Input.dispatchKeyEvent",
        json!({
            "type": "keyDown",
            "key": spec.key,
            "code": spec.code,
            "windowsVirtualKeyCode": spec.virtual_key,
            "nativeVirtualKeyCode": spec.virtual_key,
            "text": spec.text,
        }),
        deadline,
    )?;
    session.cdp.call(
        "Input.dispatchKeyEvent",
        json!({
            "type": "keyUp",
            "key": spec.key,
            "code": spec.code,
            "windowsVirtualKeyCode": spec.virtual_key,
            "nativeVirtualKeyCode": spec.virtual_key,
        }),
        deadline,
    )?;
    Ok(())
}

pub(super) const SET_VALUE_FUNCTION: &str = r#"function(value) {
  const input = this instanceof HTMLInputElement;
  const area = this instanceof HTMLTextAreaElement;
  if (!input && !area && !this.isContentEditable) return {ok:false, reason:'target is not editable'};
  this.focus();
  if (input || area) {
    const proto = input ? HTMLInputElement.prototype : HTMLTextAreaElement.prototype;
    const setter = Object.getOwnPropertyDescriptor(proto, 'value').set;
    setter.call(this, value);
  } else {
    this.textContent = value;
  }
  this.dispatchEvent(new Event('input', {bubbles:true}));
  this.dispatchEvent(new Event('change', {bubbles:true}));
  return {ok:true};
}"#;

pub(super) fn type_text(
    session: &mut Session,
    backend: u64,
    target: &RefView,
    text: &str,
    submit: bool,
    wait: Duration,
    deadline: Instant,
) -> BrowserResult<Snapshot> {
    if !matches!(
        target.role.to_ascii_lowercase().as_str(),
        "textbox" | "searchbox" | "combobox" | "spinbutton"
    ) {
        return Err(Failure::invalid(format!(
            "ref b{backend} has role {:?}, not a textbox-like role",
            target.role
        )));
    }
    let resolved = session.cdp.call(
        "DOM.resolveNode",
        json!({ "backendNodeId": backend, "objectGroup": "harness-browser" }),
        deadline,
    )?;
    let object_id = resolved
        .get("object")
        .and_then(|object| object.get("objectId"))
        .and_then(Value::as_str)
        .ok_or_else(|| {
            Failure::new(
                "browser_protocol",
                format!("could not resolve ref b{backend}"),
            )
        })?
        .to_string();
    let result = session.cdp.call(
        "Runtime.callFunctionOn",
        json!({
            "objectId": object_id,
            "functionDeclaration": SET_VALUE_FUNCTION,
            "arguments": [{ "value": text }],
            "returnByValue": true,
            "awaitPromise": false,
        }),
        deadline,
    )?;
    if result.get("exceptionDetails").is_some() {
        return Err(Failure::new(
            "browser_protocol",
            format!("typing into ref b{backend} raised a page exception"),
        ));
    }
    let value = result.get("result").and_then(|result| result.get("value"));
    if value
        .and_then(|value| value.get("ok"))
        .and_then(Value::as_bool)
        != Some(true)
    {
        let reason = value
            .and_then(|value| value.get("reason"))
            .and_then(Value::as_str)
            .unwrap_or("target refused input");
        return Err(Failure::new(
            "browser_protocol",
            format!(
                "could not type into ref b{backend}: {}",
                truncate_chars(reason, 300)
            ),
        ));
    }
    let _ = session.cdp.call(
        "Runtime.releaseObject",
        json!({ "objectId": object_id }),
        deadline,
    );
    if submit {
        dispatch_key(session, "Enter", deadline)?;
    }
    settle(wait, deadline)?;
    session.capture_and_store(deadline)
}

pub(super) fn press(
    session: &mut Session,
    key: &str,
    wait: Duration,
    deadline: Instant,
) -> BrowserResult<Snapshot> {
    dispatch_key(session, key, deadline)?;
    settle(wait, deadline)?;
    session.capture_and_store(deadline)
}

pub(super) fn summary_for(args: &Value) -> String {
    let operation = args
        .get("operation")
        .and_then(Value::as_str)
        .unwrap_or("browser");
    match operation {
        "open" => format!(
            "browser open {}",
            truncate_chars(
                args.get("url").and_then(Value::as_str).unwrap_or("page"),
                80
            )
        ),
        "click" | "type" => format!(
            "browser {operation} {}",
            args.get("ref").and_then(Value::as_str).unwrap_or("target")
        ),
        "press" => format!(
            "browser press {}",
            args.get("key").and_then(Value::as_str).unwrap_or("key")
        ),
        "snapshot" => "browser snapshot".to_string(),
        "close" => "browser close".to_string(),
        _ => "browser".to_string(),
    }
}

pub(super) fn reset_after(error: &Failure) -> bool {
    matches!(
        error.code,
        "browser_protocol" | "browser_closed" | "timeout" | "browser_unavailable"
    )
}
