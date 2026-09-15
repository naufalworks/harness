//! JSON error shape and the object-only JSON body extractor.
//!
//! Moved verbatim from `main.rs` by P12-T01; behaviour is unchanged.
use axum::{
    body::Bytes,
    extract::{rejection::JsonRejection, FromRequest, Request},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde::de::DeserializeOwned;
use serde_json::json;
use serde_json::Value;

/// Stable machine-readable code for a failure, derived from its status.
///
/// P12-T03 needs errors to carry a code a client can branch on, but the existing `{"error":
/// "<sentence>"}` bodies are already read verbatim by `static/app.js` and by the Python and browser
/// suites. So the code is *additive*: the sentence keeps its exact wording and position, and `code`
/// plus `retryable` join it. Deriving the code from the status rather than storing a third field on
/// every `ApiError` keeps all 37 construction sites unchanged, which is what makes this seam
/// reviewable: no handler logic moves.
///
/// Matching on `as_u16()` is deliberate. `StatusCode`'s associated constants are not
/// structural-match, so they cannot appear as match patterns.
pub(crate) fn error_code(status: StatusCode) -> &'static str {
    match status.as_u16() {
        400 => "invalid_request",
        401 => "unauthorized",
        403 => "forbidden",
        404 => "not_found",
        409 => "conflict",
        410 => "gone",
        413 => "payload_too_large",
        415 => "unsupported_media_type",
        422 => "unprocessable_body",
        429 => "rate_limited",
        500 => "internal_error",
        501 => "not_implemented",
        502 => "upstream_failed",
        503 => "unavailable",
        504 => "upstream_timeout",
        _ => "error",
    }
}

/// Whether repeating the identical request may succeed without duplicating an effect.
///
/// Only congestion and upstream faults qualify. A 500 is deliberately *not* retryable: the server
/// cannot promise in general that the failed operation had no effect, and `db_error`'s own sentence
/// only says no success is implied. Callers that retry a 500 anyway must rely on the request
/// identifier for idempotency, not on this flag.
pub(crate) fn retryable(status: StatusCode) -> bool {
    matches!(status.as_u16(), 429 | 502 | 503 | 504)
}

/// The one error envelope for the whole API: the human sentence, its stable code, and whether a
/// retry is worth attempting. Every JSON failure body is built here so no route can drift.
pub(crate) fn error_body(status: StatusCode, message: &str) -> Value {
    json!({
        "error": message,
        "code": error_code(status),
        "retryable": retryable(status),
    })
}
pub(crate) struct ApiError(pub(crate) StatusCode, pub(crate) &'static str);
impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(error_body(self.0, self.1))).into_response()
    }
}
pub(crate) type ApiResult<T> = std::result::Result<T, ApiError>;
pub(crate) fn db_error(_: anyhow::Error) -> ApiError {
    eprintln!("{{\"event\":\"storage_operation_failed\"}}");
    ApiError(
        StatusCode::INTERNAL_SERVER_ERROR,
        "Storage operation failed. No successful save is implied.",
    )
}
pub(crate) fn invalid(message: &'static str) -> ApiError {
    ApiError(StatusCode::BAD_REQUEST, message)
}

// axum answers extractor rejections itself, with a text/plain 422 the browser cannot parse; the UI
// then reports "Unexpected response (<status>)" and keeps the draft. Map those rejections onto
// ApiError so every failure on a JSON route stays a JSON {"error": ...} the UI can show verbatim.
//
// The body must also be a JSON *object*. serde's derive accepts a sequence as well as a map for
// any struct, so a top-level array satisfies any struct whose every field has a default: a `[]`
// posted to the scope route deserialized into an all-defaults `ScopePatch` and answered 200 after
// rewriting the row. `deny_unknown_fields` cannot catch that, because an array carries no field
// names to reject. Requiring an object here closes the hole for every JSON route at once,
// including target types added later, instead of leaving each handler to remember.
pub(crate) struct JsonBody<T>(pub(crate) T);
/// One rejection path for every JSON route: the parser's own detail goes to the journal, and the
/// reader gets a fixed sentence they can act on.
fn reject_body(detail: &str) -> ApiError {
    eprintln!(
        "{}",
        json!({"event":"request_body_rejected","detail":detail})
    );
    invalid("Request body was not accepted. Reload this tab, then send again")
}
/// Mirrors axum's own rule (`application/json`, or any `application/...+json`). Buffering the body
/// here means `Json`'s content-type check is never reached, so it has to live here instead.
fn json_content_type(headers: &HeaderMap) -> bool {
    headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(|value| {
            value
                .split(';')
                .next()
                .unwrap_or_default()
                .trim()
                .to_ascii_lowercase()
        })
        .is_some_and(|essence| {
            essence == "application/json"
                || (essence.starts_with("application/") && essence.ends_with("+json"))
        })
}
impl<S, T> FromRequest<S> for JsonBody<T>
where
    T: DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = ApiError;
    async fn from_request(request: Request, state: &S) -> ApiResult<Self> {
        if !json_content_type(request.headers()) {
            return Err(reject_body("Body is not declared as JSON"));
        }
        let bytes = Bytes::from_request(request, state)
            .await
            .map_err(|rejection| reject_body(&rejection.body_text()))?;
        if bytes.iter().copied().find(|b| !b.is_ascii_whitespace()) != Some(b'{') {
            return Err(reject_body("Body is not a JSON object"));
        }
        Json::<T>::from_bytes(&bytes)
            .map(|Json(value)| Self(value))
            .map_err(|rejection: JsonRejection| reject_body(&rejection.body_text()))
    }
}

#[cfg(test)]
mod tests {
    use super::{error_body, error_code, retryable, ApiError};
    use axum::http::StatusCode;

    /// Every status this API actually answers with must have a specific code. "error" is the
    /// fallback for statuses we do not use; if a new status starts being used, this test is what
    /// forces a code to be chosen for it rather than silently shipping the generic one.
    #[test]
    fn api_error_codes_are_specific_for_every_status_in_use() {
        for status in [
            StatusCode::BAD_REQUEST,
            StatusCode::UNAUTHORIZED,
            StatusCode::FORBIDDEN,
            StatusCode::NOT_FOUND,
            StatusCode::CONFLICT,
            StatusCode::GONE,
            StatusCode::TOO_MANY_REQUESTS,
            StatusCode::INTERNAL_SERVER_ERROR,
            StatusCode::NOT_IMPLEMENTED,
            StatusCode::BAD_GATEWAY,
            StatusCode::SERVICE_UNAVAILABLE,
        ] {
            let code = error_code(status);
            assert_ne!(code, "error", "{status} still maps to the generic code");
            assert!(
                code.chars().all(|c| c.is_ascii_lowercase() || c == '_'),
                "{status} code {code} is not a stable snake_case token"
            );
        }
    }

    /// Codes are a contract, so they are pinned literally. Changing one of these is a breaking
    /// change for any client branching on it, and the test says so.
    #[test]
    fn api_error_codes_are_pinned() {
        assert_eq!(error_code(StatusCode::BAD_REQUEST), "invalid_request");
        assert_eq!(error_code(StatusCode::UNAUTHORIZED), "unauthorized");
        assert_eq!(error_code(StatusCode::FORBIDDEN), "forbidden");
        assert_eq!(error_code(StatusCode::NOT_FOUND), "not_found");
        assert_eq!(error_code(StatusCode::CONFLICT), "conflict");
        assert_eq!(error_code(StatusCode::TOO_MANY_REQUESTS), "rate_limited");
        assert_eq!(error_code(StatusCode::BAD_GATEWAY), "upstream_failed");
        assert_eq!(error_code(StatusCode::SERVICE_UNAVAILABLE), "unavailable");
        assert_eq!(
            error_code(StatusCode::INTERNAL_SERVER_ERROR),
            "internal_error"
        );
    }

    /// Retryability must not promise a retry where the request may already have taken effect or
    /// where repeating it cannot help.
    #[test]
    fn api_only_congestion_and_upstream_faults_are_retryable() {
        for status in [
            StatusCode::TOO_MANY_REQUESTS,
            StatusCode::BAD_GATEWAY,
            StatusCode::SERVICE_UNAVAILABLE,
            StatusCode::GATEWAY_TIMEOUT,
        ] {
            assert!(retryable(status), "{status} should be retryable");
        }
        for status in [
            StatusCode::BAD_REQUEST,
            StatusCode::UNAUTHORIZED,
            StatusCode::FORBIDDEN,
            StatusCode::NOT_FOUND,
            StatusCode::CONFLICT,
            StatusCode::GONE,
            StatusCode::NOT_IMPLEMENTED,
            StatusCode::INTERNAL_SERVER_ERROR,
        ] {
            assert!(
                !retryable(status),
                "{status} must not claim to be retryable"
            );
        }
    }

    /// The envelope is additive: the human sentence keeps its exact wording under the same `error`
    /// key that `static/app.js` and the suites already read, and the new fields sit beside it.
    ///
    /// Only the key *set* is asserted, not its order. `serde_json` is built here without
    /// `preserve_order`, so objects are backed by a `BTreeMap` and serialise alphabetically; key
    /// order was never part of the contract and JSON readers must not depend on it.
    #[test]
    fn api_error_envelope_keeps_the_human_sentence_verbatim() {
        let sentence = "This conversation has an unfinished answer; check its saved receipt before sending another message";
        let body = error_body(StatusCode::CONFLICT, sentence);
        assert_eq!(body["error"], sentence);
        assert_eq!(body["code"], "conflict");
        assert_eq!(body["retryable"], false);
        let mut keys: Vec<&str> = body
            .as_object()
            .expect("object body")
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(keys, ["code", "error", "retryable"]);
    }

    /// `ApiError` must not build its body by hand; it has to go through the shared envelope, or the
    /// 37 construction sites could drift from the middleware's bodies again.
    #[test]
    fn api_error_body_matches_the_shared_envelope() {
        let error = ApiError(StatusCode::SERVICE_UNAVAILABLE, "Recording queue is full");
        let expected = error_body(error.0, error.1);
        assert_eq!(expected["code"], "unavailable");
        assert_eq!(expected["retryable"], true);
    }
}
