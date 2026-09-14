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
pub(crate) struct ApiError(pub(crate) StatusCode, pub(crate) &'static str);
impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({"error":self.1}))).into_response()
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
