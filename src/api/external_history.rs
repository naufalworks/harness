//! Dedicated producer-only bounded ingestion; never enters the agent loop.
use crate::{storage::external_history::ValidatedEvent, Harness};
use axum::{
    body::to_bytes,
    extract::{Request, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::{json, Value};
fn failure(status: StatusCode, code: &'static str) -> Response {
    (status,Json(json!({"error":"External history was not acknowledged.","code":code,"retryable":status==StatusCode::SERVICE_UNAVAILABLE}))).into_response()
}
pub(crate) async fn ingest(State(h): State<Harness>, request: Request) -> Response {
    let token = request
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or("")
        .to_owned();
    // Reject absent/invalid producer credentials without polling the body or
    // reserving capacity shared with authenticated owner requests.
    if !h.auth.is_external_producer(&token) {
        return failure(StatusCode::UNAUTHORIZED, "producer_unauthorized");
    }
    let Ok(_permit) = h.api_limit.clone().try_acquire_owned() else {
        return failure(StatusCode::SERVICE_UNAVAILABLE, "ingestion_busy");
    };
    let body = match tokio::time::timeout(
        std::time::Duration::from_secs(10),
        to_bytes(request.into_body(), 65536),
    )
    .await
    {
        Ok(Ok(b)) => b,
        _ => {
            return failure(
                StatusCode::PAYLOAD_TOO_LARGE,
                "body_unreadable_or_too_large",
            )
        }
    };
    let evidence = match h.auth.authorize_external(&token, &body) {
        Ok(e) => e,
        Err(code) => {
            return failure(
                match code {
                    "producer_unauthorized" => StatusCode::UNAUTHORIZED,
                    "scope_not_permitted" => StatusCode::FORBIDDEN,
                    "privacy_check_failed" => StatusCode::SERVICE_UNAVAILABLE,
                    _ => StatusCode::BAD_REQUEST,
                },
                code,
            )
        }
    };
    let event = match ValidatedEvent::validate(evidence) {
        Ok(e) => e,
        Err(code) => return failure(StatusCode::BAD_REQUEST, code),
    };
    match h.store.record_external(event).await {
        Ok(Ok(receipt)) => (
            if receipt["replay"] == true {
                StatusCode::OK
            } else {
                StatusCode::CREATED
            },
            Json(receipt),
        )
            .into_response(),
        Ok(Err(code)) => failure(StatusCode::CONFLICT, code),
        Err(_) => failure(StatusCode::SERVICE_UNAVAILABLE, "storage_unavailable"),
    }
}
fn member(value: &Value, choices: &[&str]) -> bool {
    value.as_str().is_some_and(|s| choices.contains(&s))
}
fn hex(value: &Value) -> bool {
    value.as_str().is_some_and(|s| {
        s.len() == 64
            && s.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}
pub(crate) fn validate(e: &Value) -> Result<(), &'static str> {
    let required = [
        "schema_version",
        "event_id",
        "content_digest",
        "producer_id",
        "producer_instance_id",
        "producer_sequence",
        "project_id",
        "logical_session_id",
        "occurred_at",
        "event_type",
        "outcome",
        "capture",
        "payload",
    ];
    let o = e.as_object().ok_or("malformed_envelope")?;
    if required.iter().any(|k| e[*k].is_null()) {
        return Err("missing_required_field");
    }
    if e["schema_version"] != "harness.external-history/v1" {
        return Err("unsupported_schema_version");
    }
    if o.keys().any(|k| {
        !required.contains(&k.as_str()) && !["invocation_id", "task_id"].contains(&k.as_str())
    }) {
        return Err("malformed_envelope");
    }
    for k in [
        "event_id",
        "producer_id",
        "producer_instance_id",
        "project_id",
        "logical_session_id",
    ] {
        if !e[k].as_str().is_some_and(crate::storage::valid_external_id) {
            return Err("malformed_envelope");
        }
    }
    if !e["producer_sequence"].as_i64().is_some_and(|n| n > 0) {
        return Err("malformed_envelope");
    }
    let kind = e["event_type"].as_str().ok_or("unknown_event_type")?;
    if ![
        "session.opened",
        "session.closed",
        "tool.admitted",
        "tool.started",
        "tool.completed",
        "tool.failed",
        "request.rejected",
        "task.started",
        "task.output",
        "task.completed",
        "task.interrupted",
        "artifact.recorded",
        "capture.gap",
        "message.observed",
    ]
    .contains(&kind)
    {
        return Err("unknown_event_type");
    }
    for (prefix, k) in [("tool.", "invocation_id"), ("task.", "task_id")] {
        if kind.starts_with(prefix) && e[k].is_null() {
            return Err("missing_required_field");
        }
        if !e[k].is_null() && !e[k].as_str().is_some_and(crate::storage::valid_external_id) {
            return Err("malformed_envelope");
        }
    }
    let stamp = e["occurred_at"].as_str().ok_or("invalid_timestamp")?;
    let b = stamp.as_bytes();
    if !stamp.is_ascii()
        || b.len() < 20
        || !stamp.ends_with('Z')
        || b.get(10) != Some(&b'T')
        || b.get(4) != Some(&b'-')
        || b.get(7) != Some(&b'-')
        || b.get(13) != Some(&b':')
        || b.get(16) != Some(&b':')
        || !(b.len() == 20
            || ((22..=27).contains(&b.len())
                && b[19] == b'.'
                && b[20..b.len() - 1].iter().all(u8::is_ascii_digit)))
        || chrono::DateTime::parse_from_rfc3339(stamp).is_err()
        || &stamp[17..19] == "60"
    {
        return Err("invalid_timestamp");
    }
    let outcome = e["outcome"].as_object().ok_or("malformed_envelope")?;
    let capture = e["capture"].as_object().ok_or("malformed_envelope")?;
    let p = e["payload"].as_object().ok_or("malformed_envelope")?;
    if outcome.len() != 3
        || !["transport", "execution", "exit_code"]
            .iter()
            .all(|k| outcome.contains_key(*k))
        || capture.len() != 3
        || !["conversation", "payload", "truncated"]
            .iter()
            .all(|k| capture.contains_key(*k))
    {
        return Err("malformed_envelope");
    }
    let out = &e["outcome"];
    let cap = &e["capture"];
    if !member(&out["transport"], &["ok", "error"])
        || !member(
            &out["execution"],
            &["succeeded", "failed", "unknown", "not_applicable"],
        )
        || !member(
            &cap["conversation"],
            &["client_supplied", "unavailable", "not_supported"],
        )
        || !member(&cap["payload"], &["full", "summarized", "omitted"])
        || !cap["truncated"].is_boolean()
    {
        return Err("malformed_envelope");
    }
    let code = &out["exit_code"];
    if !code.is_null()
        && (!code.as_i64().is_some_and(|n| i32::try_from(n).is_ok())
            || (code != 0 && out["execution"] == "succeeded"))
    {
        return Err("malformed_envelope");
    }
    if kind == "task.interrupted" && out["execution"] != "unknown" {
        return Err("malformed_envelope");
    }
    if kind == "message.observed"
        && (cap["conversation"] != "client_supplied"
            || !member(&e["payload"]["role"], &["user", "assistant"])
            || !p.get("text").is_some_and(Value::is_string)
            || !p
                .get("source_client")
                .and_then(Value::as_str)
                .is_some_and(|s| !s.is_empty()))
    {
        return Err("malformed_envelope");
    }
    if kind == "capture.gap" {
        let a = e["payload"]["missing_from_sequence"].as_i64().unwrap_or(0);
        let z = e["payload"]["missing_to_sequence"].as_i64().unwrap_or(0);
        if a < 1 || z < a {
            return Err("malformed_envelope");
        }
    }
    if kind == "artifact.recorded"
        && (!e["payload"]["artifact_id"]
            .as_str()
            .is_some_and(crate::storage::valid_external_id)
            || !e["payload"]["media_type"].is_string()
            || !e["payload"]["byte_count"].as_i64().is_some_and(|n| n >= 0)
            || !hex(&e["payload"]["digest"])
            || !e["payload"]["truncated"].is_boolean())
    {
        return Err("malformed_envelope");
    }
    let mut canonical = e.clone();
    canonical
        .as_object_mut()
        .ok_or("malformed_envelope")?
        .remove("content_digest");
    if !hex(&e["content_digest"])
        || crate::safety::fingerprint(&canonical.to_string()) != e["content_digest"]
    {
        return Err("invalid_digest");
    }
    Ok(())
}

// These reads are mounted only inside the owner/browser-authenticated router.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ExternalHistoryQuery {
    project_id: Option<String>,
    producer_id: Option<String>,
    logical_session_id: Option<String>,
    event_id: Option<String>,
    after: Option<i64>,
    limit: Option<i64>,
}
impl ExternalHistoryQuery {
    fn check(&self) -> crate::api::error::ApiResult<()> {
        use crate::api::error::invalid;
        for id in [
            &self.project_id,
            &self.producer_id,
            &self.logical_session_id,
            &self.event_id,
        ]
        .into_iter()
        .flatten()
        {
            if !crate::storage::valid_external_id(id) {
                return Err(invalid("Invalid external history identifier"));
            }
        }
        if !(0..=9_007_199_254_740_991).contains(&self.after.unwrap_or(0))
            || !(1..=100).contains(&self.limit.unwrap_or(50))
        {
            return Err(invalid("Invalid history cursor or page size"));
        }
        Ok(())
    }
    fn scope(&self) -> crate::api::error::ApiResult<(String, String, String)> {
        use crate::api::error::invalid;
        self.check()?;
        Ok((
            self.project_id
                .clone()
                .ok_or_else(|| invalid("project_id required"))?,
            self.producer_id
                .clone()
                .ok_or_else(|| invalid("producer_id required"))?,
            self.logical_session_id
                .clone()
                .ok_or_else(|| invalid("logical_session_id required"))?,
        ))
    }
}
pub(crate) async fn sessions(
    State(h): State<Harness>,
    axum::extract::Query(q): axum::extract::Query<ExternalHistoryQuery>,
) -> crate::api::error::ApiResult<Json<Value>> {
    q.check()?;
    Ok(Json(
        h.store
            .external_sessions(
                q.project_id,
                q.producer_id,
                q.after.unwrap_or(0),
                q.limit.unwrap_or(50),
            )
            .await
            .map_err(crate::api::error::db_error)?,
    ))
}
pub(crate) async fn activity(
    State(h): State<Harness>,
    axum::extract::Query(q): axum::extract::Query<ExternalHistoryQuery>,
) -> crate::api::error::ApiResult<Json<Value>> {
    let (project, producer, session) = q.scope()?;
    Ok(Json(
        h.store
            .external_activity(
                project,
                producer,
                session,
                q.after.unwrap_or(0),
                q.limit.unwrap_or(50),
            )
            .await
            .map_err(crate::api::error::db_error)?,
    ))
}
pub(crate) async fn artifact(
    State(h): State<Harness>,
    axum::extract::Query(q): axum::extract::Query<ExternalHistoryQuery>,
) -> crate::api::error::ApiResult<Json<Value>> {
    let (project, producer, session) = q.scope()?;
    let event = q
        .event_id
        .ok_or_else(|| crate::api::error::invalid("event_id required"))?;
    let result = h
        .store
        .external_artifact(project, producer, session, event)
        .await
        .map_err(crate::api::error::db_error)?;
    Ok(Json(result.ok_or(crate::api::error::ApiError(
        StatusCode::NOT_FOUND,
        "Artifact evidence not found in this session",
    ))?))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn contract_corpus_validates_and_invalid_cases_reject() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/external_history");
        for entry in std::fs::read_dir(root.join("accepted")).unwrap() {
            let path = entry.unwrap().path();
            let e: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
            assert_eq!(validate(&e), Ok(()), "{}", path.display());
        }
        for entry in std::fs::read_dir(root.join("rejected")).unwrap() {
            let path = entry.unwrap().path();
            let mut e: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
            let expected = e.as_object_mut().unwrap().remove("__expect").unwrap();
            // Replay conflict is a durable-storage decision, not envelope validation.
            if expected == "duplicate_event_id_conflict" {
                continue;
            }
            assert!(validate(&e).is_err(), "{}", path.display());
        }
    }
    #[test]
    fn timestamp_unicode_and_calendar_errors_are_rejected_without_panic() {
        let mut e: Value = serde_json::from_str(include_str!(
            "../../tests/external_history/accepted/tool_completed.json"
        ))
        .unwrap();
        for stamp in [
            "2026-02-30T00:00:00Z",
            "2026-09-21T00:00:60Z",
            "2026-09-21T00:00:éZ",
            "2026-09-21T00:00:00.1234567Z",
        ] {
            e["occurred_at"] = json!(stamp);
            assert_eq!(validate(&e), Err("invalid_timestamp"));
        }
    }
}
