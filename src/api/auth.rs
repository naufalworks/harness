//! Bearer authentication, browser session issuance, per-identity rate limiting, and the
//! response hardening headers.
//!
//! Moved out of `main.rs` by P12-T01 without behaviour changes.
use crate::Harness;
use axum::{
    extract::{Request, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;
use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use uuid::Uuid;

mod producer;
pub(crate) use producer::AuthorizedEvidence;

const AUTH_FAILURE_DELAY: Duration = Duration::from_millis(150);
const MAX_BROWSER_SESSIONS: usize = 128;

pub(crate) struct AuthState {
    producers: producer::Producers,
    current: String,
    previous: Option<String>,
    session_ttl: Duration,
    sessions: Mutex<HashMap<String, Instant>>,
    rates: Mutex<HashMap<String, VecDeque<Instant>>>,
    proxy_identity_header: Option<header::HeaderName>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AuthKind {
    Master,
    Session,
}

impl AuthState {
    pub(crate) fn new(
        current: String,
        previous: Option<String>,
        session_ttl: Duration,
        proxy_identity_header: Option<header::HeaderName>,
    ) -> Self {
        Self {
            producers: producer::Producers::default(),
            current,
            previous,
            session_ttl,
            sessions: Mutex::new(HashMap::new()),
            rates: Mutex::new(HashMap::new()),
            proxy_identity_header,
        }
    }
    /// Owner-supplied configuration only; never taken from an event or HTTP request.
    pub(crate) fn with_producers_from_env(self) -> anyhow::Result<Self> {
        match std::env::var("HARNESS_HISTORY_PRODUCERS") {
            Ok(config) => self.with_producers(&config),
            Err(std::env::VarError::NotPresent) => Ok(self),
            Err(_) => anyhow::bail!("invalid external producer configuration"),
        }
    }

    fn with_producers(mut self, config: &str) -> anyhow::Result<Self> {
        self.producers =
            producer::Producers::parse(config, &self.current, self.previous.as_deref())
                .map_err(|_| anyhow::anyhow!("invalid external producer configuration"))?;
        Ok(self)
    }

    /// P19-T02 boundary only: no receipt, durable write, or full envelope validation.
    #[allow(dead_code)] // Consumer is the separately tracked P19-T03 ingestion task.
    pub(crate) fn authorize_external(
        &self,
        token: &str,
        body: &[u8],
    ) -> Result<producer::AuthorizedEvidence, &'static str> {
        let evidence = self.producers.authorize(token, body)?;
        if (!self.current.is_empty()
            && crate::safety::external_contains(evidence.value(), &self.current))
            || self.previous.as_ref().is_some_and(|p| {
                !p.is_empty() && crate::safety::external_contains(evidence.value(), p)
            })
            || self
                .sessions
                .lock()
                .map_err(|_| "privacy_check_failed")?
                .keys()
                .any(|s| crate::safety::external_contains(evidence.value(), s))
        {
            return Err("redaction_missing");
        }
        Ok(evidence)
    }

    /// Header-only admission check. Scope and privacy still require authorize_external.
    pub(crate) fn is_external_producer(&self, token: &str) -> bool {
        self.producers.contains_token(token)
    }

    pub(crate) fn identify(&self, token: &str) -> Option<AuthKind> {
        if self.producers.contains_token(token) {
            return None;
        }
        if valid_token(&self.current, token)
            || self
                .previous
                .as_deref()
                .is_some_and(|value| valid_token(value, token))
        {
            return Some(AuthKind::Master);
        }
        let now = Instant::now();
        let mut sessions = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
        sessions.retain(|_, expires| *expires > now);
        sessions
            .get(token)
            .filter(|expires| **expires > now)
            .map(|_| AuthKind::Session)
    }
    fn issue_session(&self) -> (String, u64) {
        let now = Instant::now();
        let token = format!("{}.{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
        let mut sessions = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
        sessions.retain(|_, expiry| *expiry > now);
        if sessions.len() >= MAX_BROWSER_SESSIONS {
            if let Some(oldest) = sessions
                .iter()
                .min_by_key(|(_, expiry)| **expiry)
                .map(|(key, _)| key.clone())
            {
                sessions.remove(&oldest);
            }
        }
        sessions.insert(token.clone(), now + self.session_ttl);
        (token, self.session_ttl.as_secs())
    }
    pub(crate) fn allow(&self, identity: &str, class: &str, limit: usize) -> bool {
        let now = Instant::now();
        let mut rates = self.rates.lock().unwrap_or_else(|e| e.into_inner());
        let entries = rates.entry(format!("{identity}:{class}")).or_default();
        while entries
            .front()
            .is_some_and(|at| now.duration_since(*at) >= Duration::from_secs(60))
        {
            entries.pop_front();
        }
        if entries.len() >= limit {
            return false;
        }
        entries.push_back(now);
        true
    }
}

#[allow(deprecated)]
fn valid_token(expected: &str, actual: &str) -> bool {
    ring::constant_time::verify_slices_are_equal(expected.as_bytes(), actual.as_bytes()).is_ok()
}
pub(crate) async fn authenticate(
    State(h): State<Harness>,
    request: Request,
    next: Next,
) -> Response {
    let auth = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));
    if auth.and_then(|value| h.auth.identify(value)).is_none() {
        tokio::time::sleep(AUTH_FAILURE_DELAY).await;
        return (
            StatusCode::UNAUTHORIZED,
            Json(crate::api::error::error_body(
                StatusCode::UNAUTHORIZED,
                "Bearer token required",
            )),
        )
            .into_response();
    }
    let identity = if let Some(name) = &h.auth.proxy_identity_header {
        match request
            .headers()
            .get(name)
            .and_then(|value| value.to_str().ok())
            .filter(|value| {
                !value.is_empty()
                    && value.len() <= 128
                    && value.bytes().all(|byte| byte.is_ascii_graphic())
            }) {
            Some(value) => value.to_string(),
            None => {
                return (
                    StatusCode::UNAUTHORIZED,
                    Json(crate::api::error::error_body(
                        StatusCode::UNAUTHORIZED,
                        "Trusted proxy identity required",
                    )),
                )
                    .into_response()
            }
        }
    } else {
        "local".to_string()
    };
    let path = request.uri().path();
    let limit = if path == "/memory/ingest" {
        8
    } else if request.method() == axum::http::Method::POST {
        30
    } else {
        120
    };
    let route_key = format!("{}:{path}", request.method());
    if !h.auth.allow(&identity, &route_key, limit) {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            Json(crate::api::error::error_body(
                StatusCode::TOO_MANY_REQUESTS,
                "Request rate limit exceeded",
            )),
        )
            .into_response();
    }
    if let Some(origin) = request.headers().get(header::ORIGIN) {
        if !origin
            .to_str()
            .ok()
            .is_some_and(|o| h.origins.iter().any(|a| a == o))
        {
            return (
                StatusCode::FORBIDDEN,
                Json(crate::api::error::error_body(
                    StatusCode::FORBIDDEN,
                    "Origin not allowed",
                )),
            )
                .into_response();
        }
    }
    let Ok(_permit) = h.api_limit.clone().try_acquire_owned() else {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            Json(crate::api::error::error_body(
                StatusCode::TOO_MANY_REQUESTS,
                "Too many concurrent requests",
            )),
        )
            .into_response();
    };
    next.run(request).await
}
pub(crate) async fn create_browser_session(
    State(h): State<Harness>,
    headers: HeaderMap,
) -> Response {
    let Ok(_permit) = h.api_limit.clone().try_acquire_owned() else {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            Json(crate::api::error::error_body(
                StatusCode::TOO_MANY_REQUESTS,
                "Too many concurrent requests",
            )),
        )
            .into_response();
    };
    let token = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "));
    if token.and_then(|value| h.auth.identify(value)) != Some(AuthKind::Master) {
        tokio::time::sleep(AUTH_FAILURE_DELAY).await;
        return (
            StatusCode::UNAUTHORIZED,
            Json(crate::api::error::error_body(
                StatusCode::UNAUTHORIZED,
                "Master bearer token required",
            )),
        )
            .into_response();
    }
    if let Some(origin) = headers.get(header::ORIGIN) {
        if !origin
            .to_str()
            .ok()
            .is_some_and(|value| h.origins.iter().any(|allowed| allowed == value))
        {
            return (
                StatusCode::FORBIDDEN,
                Json(crate::api::error::error_body(
                    StatusCode::FORBIDDEN,
                    "Origin not allowed",
                )),
            )
                .into_response();
        }
    }
    if !h.auth.allow("local", "session", 5) {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            Json(crate::api::error::error_body(
                StatusCode::TOO_MANY_REQUESTS,
                "Session creation rate limit exceeded",
            )),
        )
            .into_response();
    }
    let (session_token, expires_in) = h.auth.issue_session();
    (
        StatusCode::CREATED,
        Json(json!({"session_token":session_token,"expires_in":expires_in})),
    )
        .into_response()
}
pub(crate) async fn headers(
    State(state): State<Harness>,
    request: Request,
    next: Next,
) -> Response {
    let mut response = next.run(request).await;
    let h = response.headers_mut();
    // P11-T05: a handler that already chose a caching policy keeps it; every other response
    // stays uncacheable. Only the fingerprinted static assets opt out of `no-store`, so no API
    // payload or receipt can be stored by a proxy because of this change.
    if !h.contains_key(header::CACHE_CONTROL) {
        h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    }
    h.insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    h.insert("referrer-policy", HeaderValue::from_static("no-referrer"));
    h.insert("content-security-policy",HeaderValue::from_static("default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; img-src 'self'; base-uri 'none'; frame-ancestors 'none'; form-action 'self'"));
    if state.hsts {
        h.insert(
            "strict-transport-security",
            HeaderValue::from_static("max-age=31536000; includeSubDomains"),
        );
    }
    response
}
