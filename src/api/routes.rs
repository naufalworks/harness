//! The routing table and the request handlers it points at.
//!
//! Moved verbatim from `main.rs` by P12-T01; behaviour is unchanged.
use crate::api::assets::{api_js, css, index, js};
use crate::api::auth::{authenticate, create_browser_session, headers};
use crate::api::dto::{ChangeRow, ReceiptView, UNREADABLE_CHANGE};
use crate::api::error::{db_error, invalid, ApiError, ApiResult, JsonBody};
use crate::api::stream::{activity, activity_stream, generation, generation_stream};
use crate::archive::{ArchiveStore, PrivacyAction};
use crate::{agent_loop, ingest, recording, safety, storage, tools, Harness};
use anyhow::Result;
use axum::{
    body::Bytes,
    extract::{DefaultBodyLimit, Path, Query, State},
    http::{header, StatusCode},
    middleware,
    response::{IntoResponse, Response},
    routing::{get, patch, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    sync::{atomic::Ordering, Arc},
};
use uuid::Uuid;

fn default_scope() -> String {
    "global".into()
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ChatRequest {
    prompt: String,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default)]
    request_id: Option<String>,
    #[serde(default = "default_scope")]
    scope: String,
}
async fn admit_chat(h: &Harness, req: ChatRequest) -> ApiResult<Value> {
    safety::scope(&req.scope).map_err(|_| invalid("Invalid scope"))?;
    if req.prompt.trim().is_empty() || req.prompt.len() > 16_000 {
        return Err(invalid("Prompt must contain 1-16000 UTF-8 bytes"));
    }
    let session = req.session_id.unwrap_or_else(storage::uid);
    let request = req.request_id.unwrap_or_else(storage::uid);
    Uuid::parse_str(&session).map_err(|_| invalid("Invalid session identifier"))?;
    Uuid::parse_str(&request).map_err(|_| invalid("Invalid request identifier"))?;
    let prompt = safety::redact(&req.prompt);
    let redacted = prompt != req.prompt;
    if prompt.len() > 16_000 {
        return Err(invalid("Sanitized prompt exceeds the size budget"));
    }
    if req
        .model
        .as_ref()
        .is_some_and(|m| m.is_empty() || m.len() > 128 || m.chars().any(char::is_control))
    {
        return Err(invalid("Invalid model"));
    }
    // Fingerprint SANITIZED content only; never retain a brute-forceable hash of a secret.
    // Default-model changes do not turn a repeated request into a new paid generation.
    let signature=safety::fingerprint(&json!({"session":session,"scope":req.scope,"prompt":prompt,"model_override":req.model,"redacted":redacted}).to_string());
    let model = match req.model {
        Some(m) => m,
        None => h
            .store
            .role_model("main", &h.agents.model)
            .await
            .map_err(db_error)?,
    };
    match h.store.capture_chat(recording::CaptureInput{request,session,scope:req.scope,prompt,model,signature,redacted}).await.map_err(db_error)? {
        recording::Admission::Saved(receipt)=>Ok(receipt),
        recording::Admission::Conflict=>Err(ApiError(StatusCode::CONFLICT,"Request identifier already belongs to different content; nothing new was recorded")),
        recording::Admission::ScopeConflict=>Err(ApiError(StatusCode::CONFLICT,"Session belongs to a different scope; start a new conversation")),
        recording::Admission::Busy=>Err(ApiError(StatusCode::CONFLICT,"This conversation has an unfinished answer; check its saved receipt before sending another message")),
        recording::Admission::Full=>Err(ApiError(StatusCode::SERVICE_UNAVAILABLE,"Recording queue is full. This new message was not accepted; keep your draft and try later")),
    }
}
async fn submit_chat(
    State(h): State<Harness>,
    JsonBody(req): JsonBody<ChatRequest>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let receipt = admit_chat(&h, req).await?;
    // Pending means the answer is still owed, which is the 202 case.
    let code = if ReceiptView::of(&receipt)
        .state()
        .is_some_and(recording::RequestState::is_pending)
    {
        StatusCode::ACCEPTED
    } else {
        StatusCode::OK
    };
    Ok((code, Json(receipt)))
}
// Compatibility endpoint: briefly wait for fast providers, otherwise return the durable
// 202 receipt. Disconnecting never owns/cancels the generation worker.
async fn chat(
    State(h): State<Harness>,
    JsonBody(req): JsonBody<ChatRequest>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let mut receipt = admit_chat(&h, req).await?;
    // A receipt without a request_id cannot be polled, so return it as-is rather than
    // panicking the handler; the durable 202 receipt is still a correct answer here.
    let Some(request) = ReceiptView::of(&receipt).request_id else {
        return Ok((StatusCode::ACCEPTED, Json(receipt)));
    };
    for _ in 0..20 {
        match ReceiptView::of(&receipt).state() {
            Some(recording::RequestState::Complete) => return Ok((StatusCode::OK, Json(receipt))),
            Some(recording::RequestState::Failed | recording::RequestState::Interrupted) => {
                return Ok((StatusCode::BAD_GATEWAY, Json(receipt)))
            }
            // Still pending, or a state this build does not recognise: keep
            // waiting rather than reporting a terminal outcome we invented.
            _ => tokio::time::sleep(std::time::Duration::from_millis(250)).await,
        }
        receipt = h
            .store
            .recording_receipt(request.clone())
            .await
            .map_err(db_error)?
            .ok_or(ApiError(
                StatusCode::NOT_FOUND,
                "Recording receipt not found",
            ))?;
    }
    Ok((StatusCode::ACCEPTED, Json(receipt)))
}
async fn get_receipt(State(h): State<Harness>, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    Uuid::parse_str(&id).map_err(|_| invalid("Invalid request identifier"))?;
    Ok(Json(
        h.store
            .recording_receipt(id)
            .await
            .map_err(db_error)?
            .ok_or(ApiError(
                StatusCode::NOT_FOUND,
                "Recording receipt not found",
            ))?,
    ))
}
async fn cancel_request(
    State(h): State<Harness>,
    Path(id): Path<String>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    Uuid::parse_str(&id).map_err(|_| invalid("Invalid request identifier"))?;
    let receipt = h
        .store
        .request_cancellation(id.clone())
        .await
        .map_err(db_error)?
        .ok_or(ApiError(
            StatusCode::NOT_FOUND,
            "Recording receipt not found",
        ))?;
    // The durable intent is committed first; only then stop any process group the turn is still
    // blocked on. A restart that loses this in-memory handle still leaves the turn interrupted.
    crate::processes::terminate(&id);
    let view = ReceiptView::of(&receipt);
    match view.state() {
        Some(recording::RequestState::Generating) => Ok((StatusCode::ACCEPTED, Json(receipt))),
        Some(recording::RequestState::Interrupted) if view.was_cancelled() => {
            Ok((StatusCode::OK, Json(receipt)))
        }
        _ => Err(ApiError(
            StatusCode::CONFLICT,
            "Only a captured or generating request can be cancelled",
        )),
    }
}

async fn retry_request(
    State(h): State<Harness>,
    Path(id): Path<String>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    Uuid::parse_str(&id).map_err(|_| invalid("Invalid request identifier"))?;
    match h.store.retry_recording(id).await.map_err(db_error)? {
        recording::RetryAdmission::Saved(receipt) => Ok((StatusCode::ACCEPTED, Json(receipt))),
        recording::RetryAdmission::NotFound => Err(ApiError(
            StatusCode::NOT_FOUND,
            "Recording receipt not found",
        )),
        recording::RetryAdmission::NotTerminal => Err(ApiError(
            StatusCode::CONFLICT,
            "Only failed or interrupted requests can be retried",
        )),
        recording::RetryAdmission::Busy => Err(ApiError(
            StatusCode::CONFLICT,
            "This conversation already has an unfinished answer",
        )),
        recording::RetryAdmission::Unsafe => Err(ApiError(
            StatusCode::CONFLICT,
            "Retry refused because the run crossed or may have crossed a mutating boundary",
        )),
    }
}

async fn get_context(State(h): State<Harness>, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    Uuid::parse_str(&id).map_err(|_| invalid("Invalid request identifier"))?;
    Ok(Json(
        h.store
            .recording_context(id)
            .await
            .map_err(db_error)?
            .ok_or(ApiError(
                StatusCode::NOT_FOUND,
                "Recording receipt not found",
            ))?,
    ))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HistoryQuery {
    before_seq: Option<i64>,
    #[serde(default)]
    search: Option<String>,
    #[serde(default)]
    include_archived: bool,
}
fn cursor(query: HistoryQuery) -> ApiResult<Option<i64>> {
    if query.before_seq.is_some_and(|n| n <= 0) {
        return Err(invalid("History cursor must be positive"));
    }
    Ok(query.before_seq)
}
async fn sessions(
    State(h): State<Harness>,
    Query(q): Query<HistoryQuery>,
) -> ApiResult<Json<Value>> {
    if q.search
        .as_ref()
        .is_some_and(|value| value.chars().count() > 120)
    {
        return Err(invalid("Session search must be 120 characters or fewer"));
    }
    Ok(Json(
        h.store
            .recorded_sessions_filtered(
                q.before_seq,
                q.search
                    .map(|value| value.trim().to_string())
                    .filter(|value| !value.is_empty()),
                q.include_archived,
            )
            .await
            .map_err(db_error)?,
    ))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SessionPatch {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    archived: Option<bool>,
}

async fn update_session(
    State(h): State<Harness>,
    Path(id): Path<String>,
    JsonBody(req): JsonBody<SessionPatch>,
) -> ApiResult<Json<Value>> {
    Uuid::parse_str(&id).map_err(|_| invalid("Invalid session identifier"))?;
    if req.title.is_none() && req.archived.is_none() {
        return Err(invalid("A session update needs a title or archived flag"));
    }
    let title = req.title.map(|value| value.trim().to_string());
    if title
        .as_ref()
        .is_some_and(|value| value.is_empty() || value.chars().count() > 120)
    {
        return Err(invalid("Session titles must be 1–120 characters"));
    }
    Ok(Json(
        h.store
            .update_session(id, title, req.archived)
            .await
            .map_err(db_error)?
            .ok_or(ApiError(StatusCode::NOT_FOUND, "Session not found"))?,
    ))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ForkRequest {
    #[serde(default)]
    title: Option<String>,
}

async fn fork_session(
    State(h): State<Harness>,
    Path(id): Path<String>,
    JsonBody(req): JsonBody<ForkRequest>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    Uuid::parse_str(&id).map_err(|_| invalid("Invalid session identifier"))?;
    let title = req.title.map(|value| value.trim().to_string());
    if title
        .as_ref()
        .is_some_and(|value| value.is_empty() || value.chars().count() > 120)
    {
        return Err(invalid("Session titles must be 1–120 characters"));
    }
    Ok((
        StatusCode::CREATED,
        Json(
            h.store
                .fork_session(id, title)
                .await
                .map_err(db_error)?
                .ok_or(ApiError(StatusCode::NOT_FOUND, "Session not found"))?,
        ),
    ))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ConfirmRequest {
    confirmation_id: String,
    confirm: bool,
    #[serde(default = "default_scope")]
    scope: String,
}
async fn confirm(
    State(h): State<Harness>,
    JsonBody(req): JsonBody<ConfirmRequest>,
) -> ApiResult<Json<Value>> {
    safety::scope(&req.scope).map_err(|_| invalid("Invalid scope"))?;
    let status = h
        .store
        .resolve(req.confirmation_id, req.scope, req.confirm)
        .await
        .map_err(db_error)?;
    match status.as_str(){"approved"|"rejected"=>Ok(Json(json!({"status":status}))),"not_found"=>Err(ApiError(StatusCode::NOT_FOUND,"Proposal not found in this scope")),_=>Err(ApiError(StatusCode::CONFLICT,"Proposal is expired, already resolved, or conflicts with a newer revision; reload the inbox"))}
}
#[derive(Deserialize)]
struct ScopeQuery {
    #[serde(default = "default_scope")]
    scope: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CandidateQuery {
    #[serde(default = "default_scope")]
    scope: String,
    #[serde(default)]
    request_id: Option<String>,
    #[serde(default)]
    imports_only: bool,
    #[serde(default)]
    chat_only: bool,
}
async fn candidates(
    State(h): State<Harness>,
    Query(q): Query<CandidateQuery>,
) -> ApiResult<Json<Value>> {
    safety::scope(&q.scope).map_err(|_| invalid("Invalid scope"))?;
    if q.imports_only && q.chat_only {
        return Err(invalid("Candidate filters cannot be combined"));
    }
    if let Some(id) = q.request_id.as_deref() {
        Uuid::parse_str(id).map_err(|_| invalid("Invalid request identifier"))?;
    }
    Ok(Json(
        h.store
            .candidate_feed(q.scope, q.request_id, q.imports_only, q.chat_only)
            .await
            .map_err(db_error)?,
    ))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CandidateEdit {
    value: String,
    #[serde(default = "default_scope")]
    scope: String,
}
async fn edit_candidate(
    State(h): State<Harness>,
    Path(id): Path<String>,
    JsonBody(req): JsonBody<CandidateEdit>,
) -> ApiResult<Json<Value>> {
    Uuid::parse_str(&id).map_err(|_| invalid("Invalid candidate identifier"))?;
    safety::scope(&req.scope).map_err(|_| invalid("Invalid scope"))?;
    if req.value.trim().is_empty()
        || req.value.chars().count() > 1000
        || req.value.len() > 4000
        || safety::sensitive(&req.value)
    {
        return Err(invalid("Edited memory must contain 1–1000 safe characters"));
    }
    match h
        .store
        .edit_candidate(id, req.scope, req.value)
        .await
        .map_err(db_error)?
        .as_str()
    {
        "edited" => Ok(Json(json!({"status":"edited"}))),
        "not_found" => Err(ApiError(
            StatusCode::NOT_FOUND,
            "Candidate not found in this scope",
        )),
        _ => Err(ApiError(
            StatusCode::CONFLICT,
            "Candidate is expired, resolved, or duplicates another suggestion; reload the tray",
        )),
    }
}
async fn status(State(h): State<Harness>) -> ApiResult<Json<Value>> {
    Ok(Json(h.store.stats().await.map_err(db_error)?))
}
async fn health(State(h): State<Harness>) -> Response {
    let recording = h.workers.recording.load(Ordering::Acquire);
    let extraction = h.workers.extraction.load(Ordering::Acquire);
    match h.store.readiness().await {
        Ok(database) => {
            let ready = database["ready"].as_bool() == Some(true) && recording && extraction;
            let status = if ready {
                StatusCode::OK
            } else {
                StatusCode::SERVICE_UNAVAILABLE
            };
            (status, Json(json!({"ready":ready,"error":if ready { Value::Null } else { json!("Harness is not ready") },"commit":h.identity.commit,
                "binary_sha256":h.identity.binary_sha256,"started_at":h.identity.started_at,
                "port":h.port,"schema_version":database["schema_version"],"database":database,
                "workers":{"recording":recording,"extraction":extraction}}))).into_response()
        }
        Err(_) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"ready":false,"error":"Harness is not ready",
            "commit":h.identity.commit,"binary_sha256":h.identity.binary_sha256,
            "started_at":h.identity.started_at,"port":h.port,"schema_version":Value::Null,
            "database":{"ready":false,"error":"storage probe failed"},
            "workers":{"recording":recording,"extraction":extraction}})),
        )
            .into_response(),
    }
}
async fn history(
    State(h): State<Harness>,
    Path(session): Path<String>,
    Query(q): Query<HistoryQuery>,
) -> ApiResult<Json<Value>> {
    Uuid::parse_str(&session).map_err(|_| invalid("Invalid session identifier"))?;
    Ok(Json(
        h.store
            .history(session, cursor(q)?)
            .await
            .map_err(db_error)?,
    ))
}
async fn get_config(State(h): State<Harness>) -> ApiResult<Json<Value>> {
    Ok(Json(h.store.settings().await.map_err(db_error)?))
}
async fn set_config(
    State(h): State<Harness>,
    JsonBody(data): JsonBody<BTreeMap<String, String>>,
) -> ApiResult<Json<Value>> {
    h.store.set_settings(data).await.map_err(db_error)?;
    Ok(Json(json!({"status":"saved"})))
}
async fn models(State(h): State<Harness>) -> ApiResult<Json<Value>> {
    Ok(Json(h.agents.list_models().await.map_err(|_| {
        ApiError(StatusCode::BAD_GATEWAY, "Unable to load provider models")
    })?))
}
async fn jobs(State(h): State<Harness>) -> ApiResult<Json<Value>> {
    Ok(Json(h.store.jobs().await.map_err(db_error)?))
}
async fn retry_job(State(h): State<Harness>, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    if !h.store.retry_job(id).await.map_err(db_error)? {
        return Err(ApiError(
            StatusCode::CONFLICT,
            "Only failed jobs can be retried",
        ));
    }
    Ok(Json(json!({"status":"queued"})))
}

// P1-T04: a scope owns a project root. Tools stay disabled until root_path is set, and an
// unmentioned field keeps its stored value so a partial POST cannot silently disable them.
async fn get_scope(
    State(h): State<Harness>,
    Path(scope): Path<String>,
) -> ApiResult<Json<storage::ScopeConfig>> {
    safety::scope(&scope).map_err(|_| invalid("Invalid scope"))?;
    Ok(Json(
        h.store
            .scope_config(scope)
            .await
            .map_err(db_error)?
            .ok_or(ApiError(
                StatusCode::NOT_FOUND,
                "This scope has no project configuration yet",
            ))?,
    ))
}
async fn set_scope(
    State(h): State<Harness>,
    Path(scope): Path<String>,
    JsonBody(patch): JsonBody<storage::ScopePatch>,
) -> ApiResult<Json<storage::ScopeConfig>> {
    safety::scope(&scope).map_err(|_| invalid("Invalid scope"))?;
    let patch = patch.validate().map_err(invalid)?;
    Ok(Json(
        h.store.upsert_scope(scope, patch).await.map_err(db_error)?,
    ))
}
// P1-T15: without this the scope name was an unguessable free-text field, so a first-run user
// chatted in an unconfigured scope and got a tool-less agent with no way to see that.
async fn list_scopes(State(h): State<Harness>) -> ApiResult<Json<Value>> {
    Ok(Json(
        json!({"scopes":h.store.scopes().await.map_err(db_error)?}),
    ))
}

// P1-T11: the human half of the permission gate. The turn loop only ever reads the row's status,
// so these two endpoints are the only thing that can let a side-effecting tool run.
async fn permissions(
    State(h): State<Harness>,
    Query(q): Query<ScopeQuery>,
) -> ApiResult<Json<Value>> {
    safety::scope(&q.scope).map_err(|_| invalid("Invalid scope"))?;
    Ok(Json(
        h.store
            .pending_permissions(q.scope)
            .await
            .map_err(db_error)?,
    ))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DecisionRequest {
    decision: String,
    #[serde(default = "default_scope")]
    scope: String,
}
async fn decide_permission(
    State(h): State<Harness>,
    Path(id): Path<String>,
    JsonBody(req): JsonBody<DecisionRequest>,
) -> ApiResult<Json<Value>> {
    safety::scope(&req.scope).map_err(|_| invalid("Invalid scope"))?;
    Uuid::parse_str(&id).map_err(|_| invalid("Invalid approval identifier"))?;
    let decision = match req.decision.as_str() {
        "approve" => "approved",
        "deny" => "denied",
        _ => return Err(invalid("Decision must be \"approve\" or \"deny\"")),
    };
    // Re-sending the same decision is a success: a double-clicked Approve must not become an error.
    match h
        .store
        .resolve_permission(id, req.scope, decision)
        .await
        .map_err(db_error)?
    {
        agent_loop::Resolution::Recorded => Ok(Json(json!({"status":decision,"recorded":true}))),
        agent_loop::Resolution::Unchanged => Ok(Json(json!({"status":decision,"recorded":false}))),
        agent_loop::Resolution::Conflict => Err(ApiError(
            StatusCode::CONFLICT,
            "This approval was already resolved the other way; nothing was changed",
        )),
        agent_loop::Resolution::Expired => Err(ApiError(
            StatusCode::GONE,
            "This approval expired and the turn stopped waiting; nothing was run",
        )),
        agent_loop::Resolution::NotFound => Err(ApiError(
            StatusCode::NOT_FOUND,
            "Approval request not found in this scope",
        )),
    }
}

// P1-T12: the read side. Every field below is a row the loop already committed, so what the UI
// shows is the record itself and not a second, prettier version of it.
async fn request_steps(State(h): State<Harness>, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    Uuid::parse_str(&id).map_err(|_| invalid("Invalid request identifier"))?;
    // 404 on an unknown turn: an empty step list would otherwise read as "this turn did nothing".
    h.store
        .recording_receipt(id.clone())
        .await
        .map_err(db_error)?
        .ok_or(ApiError(
            StatusCode::NOT_FOUND,
            "Recording receipt not found",
        ))?;
    Ok(Json(h.store.turn_steps(id).await.map_err(db_error)?))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct IncidentQueryParams {
    /// `causal` (default) or `chronological`.
    #[serde(default)]
    view: Option<String>,
    #[serde(default)]
    relation: Option<String>,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    tool: Option<String>,
    #[serde(default)]
    row_id: Option<String>,
    #[serde(default)]
    q: Option<String>,
    /// An `expansion_cursors` object from a previous response, serialized verbatim. It is
    /// opaque on purpose: a client that builds one by hand is refused rather than served a
    /// page computed from offsets it guessed.
    #[serde(default)]
    anchor: Option<String>,
}
/// P8-T02: bounded causal incident read model. This is diagnostic evidence, not model reasoning.
///
/// P16-T01 adds reviewer navigation on the same projection: bounded expansion via the opaque
/// anchor, filtering by relation/kind/status/path/tool/row, free-text label search, and a
/// chronological view beside the causal one. Confidence labels distinguish a recorded
/// dependency from mere temporal proximity, and say `unknown` when neither is available.
async fn request_incident(
    State(h): State<Harness>,
    Path(id): Path<String>,
    Query(q): Query<IncidentQueryParams>,
) -> ApiResult<Json<Value>> {
    Uuid::parse_str(&id).map_err(|_| invalid("Invalid request identifier"))?;
    let anchor = match q.anchor.as_deref().map(str::trim).filter(|a| !a.is_empty()) {
        Some(text) => Some(serde_json::from_str::<Value>(text).map_err(|_| {
            invalid("Expansion anchor must be the JSON object the previous response returned")
        })?),
        None => None,
    };
    let query = storage::IncidentQuery {
        view: q.view.unwrap_or_else(|| "causal".into()),
        relation: q.relation,
        kind: q.kind,
        status: q.status,
        path: q.path,
        tool: q.tool,
        row_id: q.row_id,
        q: q.q,
        anchor,
    };
    query
        .validate()
        .map_err(|_| invalid("Unsupported incident filter, view or expansion anchor"))?;
    Ok(Json(
        h.store
            .incident_view(id, query)
            .await
            .map_err(db_error)?
            .ok_or(ApiError(
                StatusCode::NOT_FOUND,
                "Recording receipt not found",
            ))?,
    ))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RequestQuery {
    request_id: String,
}
async fn request_changes(
    State(h): State<Harness>,
    Query(q): Query<RequestQuery>,
) -> ApiResult<Json<Value>> {
    Uuid::parse_str(&q.request_id).map_err(|_| invalid("Invalid request identifier"))?;
    let receipt = h
        .store
        .recording_receipt(q.request_id.clone())
        .await
        .map_err(db_error)?
        .ok_or(ApiError(
            StatusCode::NOT_FOUND,
            "Recording receipt not found",
        ))?;
    let mut changes = h.store.turn_changes(q.request_id).await.map_err(db_error)?;
    // P2-T03: whether a card's Revert can work is a fact about the file right now, so it is read
    // here rather than guessed in the browser. An offer that cannot work is worse than none.
    let scope = ReceiptView::of(&receipt).scope.unwrap_or_default();
    let root = h
        .store
        .scope_config(scope)
        .await
        .map_err(db_error)?
        .and_then(|c| c.root_path);
    if let Some(rows) = changes["changes"].as_array_mut() {
        let listed = rows.clone();
        let states = tokio::task::spawn_blocking(move || {
            listed
                .iter()
                .map(|row| match ChangeRow::of(row) {
                    Some(change) => revert_state(root.as_deref(), &change),
                    None => (false, UNREADABLE_CHANGE),
                })
                .collect::<Vec<_>>()
        })
        .await
        .map_err(|_| {
            ApiError(
                StatusCode::INTERNAL_SERVER_ERROR,
                "The recorded changes could not be inspected",
            )
        })?;
        for (row, (revertable, note)) in rows.iter_mut().zip(states) {
            row["revertable"] = json!(revertable);
            row["revert_note"] = if note.is_empty() {
                Value::Null
            } else {
                json!(note)
            };
        }
    }
    Ok(Json(changes))
}
/// Can this recorded change still be undone? The only honest answer comes from the file itself:
/// the bytes on disk have to be the bytes the edit produced. Returns the reason when they are
/// not, in the words the card shows.
fn revert_state(root: Option<&str>, change: &ChangeRow) -> (bool, &'static str) {
    if change.reverted_at.is_some() {
        return (false, "Already reverted");
    }
    if !change.applied {
        return (false, "Never applied, so there is nothing to undo");
    }
    let Some(root) = root else {
        return (
            false,
            "This scope has no project root, so nothing can be restored",
        );
    };
    let Some(path) = change.path.as_deref() else {
        return (false, "This change has no recorded path");
    };
    let Ok(resolved) = tools::paths::resolve(std::path::Path::new(root), path) else {
        return (false, "That path is outside the project root");
    };
    let after = change.after_hash.as_deref().unwrap_or_default();
    match std::fs::read_to_string(&resolved) {
        Ok(text) if tools::content_hash(&text) == after => (true, ""),
        Ok(_) => (false, "File changed since; revert unavailable"),
        // Forward-compatible only: no current tool emits `action=delete`. If one does, the file
        // being gone is the recorded after-state and the diff can rebuild its prior contents.
        Err(_) if change.is_action("delete") => (true, ""),
        Err(_) => (false, "File is no longer there; revert unavailable"),
    }
}
/// Put the recorded previous content back, or explain why not. Two proofs are required: the file
/// still has to hash to `after_hash`, and the content rebuilt by reverse-applying the recorded
/// diff has to hash to `before_hash`. `file_changes` stores hashes rather than the old bytes, so
/// without both proofs this refuses and writes nothing — a revert that guessed would silently
/// destroy whatever the user did after the edit.
fn restore_change(
    root: &std::path::Path,
    change: &Value,
) -> std::result::Result<&'static str, &'static str> {
    // An unreadable row is refused, never reverted: this function writes to the
    // user's files, so there is no safe guess about which bytes to restore.
    let change = ChangeRow::of(change).ok_or(UNREADABLE_CHANGE)?;
    let (revertable, why) = revert_state(Some(&root.to_string_lossy()), &change);
    if !revertable {
        return Err(if why.is_empty() {
            "This change cannot be reverted"
        } else {
            why
        });
    }
    let path = change
        .path
        .as_deref()
        .ok_or("This change has no recorded path")?;
    let resolved =
        tools::paths::resolve(root, path).map_err(|_| "That path is outside the project root")?;
    let after = std::fs::read_to_string(&resolved).unwrap_or_default();
    let Some(before_hash) = change.before_hash.as_deref() else {
        // Nothing existed before: undoing a file this turn created means removing it again.
        if !change.is_action("create") {
            return Err("This change has no recorded previous content; revert unavailable");
        }
        std::fs::remove_file(&resolved)
            .map_err(|_| "The file could not be removed; nothing was changed")?;
        return Ok("deleted");
    };
    let before = tools::textdiff::reverse(&after, change.diff.as_deref().unwrap_or_default())
        .ok_or("The recorded diff cannot rebuild the previous content; revert unavailable")?;
    if tools::content_hash(&before) != before_hash {
        return Err("The rebuilt content does not match the recorded hash; nothing was written");
    }
    if let Some(parent) = resolved.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|_| "The parent directory could not be created; nothing was written")?;
    }
    tools::edit_tools::atomic_write(
        root,
        &resolved,
        change.id.as_deref().unwrap_or("revert"),
        &before,
    )
    .map_err(|_| "The file could not be written; nothing was changed")?;
    Ok("restored")
}
/// P2-T03: undo one recorded change. The scope, and so the project root, comes from the change's
/// own turn: a caller cannot aim a revert at another project.
async fn revert_change(State(h): State<Harness>, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    Uuid::parse_str(&id).map_err(|_| invalid("Invalid change identifier"))?;
    let change = h
        .store
        .file_change(id.clone())
        .await
        .map_err(db_error)?
        .ok_or(ApiError(StatusCode::NOT_FOUND, "Change not found"))?;
    if change["reverted_at"].as_str().is_some() {
        return Err(ApiError(
            StatusCode::CONFLICT,
            "This change was already reverted; nothing was written",
        ));
    }
    let scope = change["scope"].as_str().unwrap_or_default().to_string();
    let root = h
        .store
        .scope_config(scope)
        .await
        .map_err(db_error)?
        .and_then(|c| c.root_path)
        .ok_or(ApiError(
            StatusCode::CONFLICT,
            "This scope has no project root, so nothing can be restored",
        ))?;
    let subject = change.clone();
    let outcome =
        tokio::task::spawn_blocking(move || restore_change(std::path::Path::new(&root), &subject))
            .await
            .map_err(|_| {
                ApiError(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "The revert did not run; nothing was written",
                )
            })?;
    let status = outcome.map_err(|why| ApiError(StatusCode::CONFLICT, why))?;
    // The file is already back; record the undo and announce it on the feed the edit used.
    let recorded = h
        .store
        .record_revert(
            id,
            change["request_id"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
            change["session_id"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
            change["step_id"].as_str().unwrap_or_default().to_string(),
            change["path"].as_str().unwrap_or_default().to_string(),
        )
        .await
        .map_err(db_error)?;
    Ok(Json(
        json!({"status":status,"path":change["path"],"recorded":recorded}),
    ))
}
async fn session_plan(
    State(h): State<Harness>,
    Path(session): Path<String>,
) -> ApiResult<Json<Value>> {
    Uuid::parse_str(&session).map_err(|_| invalid("Invalid session identifier"))?;
    // A session with no plan and a session that does not exist both read as empty, exactly as
    // `/sessions/{id}/messages` does. The plan is a view of the session, not proof it exists.
    Ok(Json(h.store.plan(session).await.map_err(db_error)?))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct IngestRequest {
    name: String,
    content: String,
    #[serde(default)]
    format: Option<String>,
    #[serde(default = "default_scope")]
    scope: String,
}
async fn ingest_memory(
    State(h): State<Harness>,
    JsonBody(req): JsonBody<IngestRequest>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    safety::scope(&req.scope).map_err(|_| invalid("Invalid scope"))?;
    if req.content.len() > 1_048_576
        || req.content.trim().is_empty()
        || req.name.is_empty()
        || req.name.len() > 200
        || req.name.chars().any(char::is_control)
    {
        return Err(invalid("Import requires a name and 1-1048576 UTF-8 bytes"));
    }
    // No server-side filesystem paths are accepted. The local CLI uploads bounded files.
    let scope = req.scope;
    let name = safety::redact(&req.name);
    let parsed = tokio::task::spawn_blocking(move || -> Result<_> {
        let fingerprint = safety::fingerprint(&format!(
            "{}\0{}",
            req.format.as_deref().unwrap_or("auto"),
            req.content
        ));
        let (format, mut events, mut warnings) =
            ingest::parse(&req.content, req.format.as_deref())?;
        // Parse first: redacting an entire JSON line before parsing could destroy valid source syntax.
        for event in &mut events {
            event.content = safety::redact(&event.content);
        }
        let content = ingest::sanitized_source(&req.content, &format);
        if content != req.content {
            warnings.push(
                "Sensitive-looking source lines were redacted; this is not an exact raw archive"
                    .into(),
            );
        }
        let chunks = ingest::chunks(&events);
        Ok((format, content, fingerprint, warnings, chunks))
    })
    .await
    .map_err(|_| invalid("Import parser failed"))?
    .map_err(|_| invalid("Could not parse import; verify format and content"))?;
    let (format, content, fingerprint, warnings, chunks) = parsed;
    let response = h
        .store
        .ingest(scope, name, format, content, fingerprint, warnings, chunks)
        .await
        .map_err(db_error)?;
    Ok((StatusCode::ACCEPTED, Json(response)))
}

/// Exact archiving is opt-in. With no key configured the honest answer names the missing
/// configuration: a 500 would imply a fault, and a 2xx would imply bytes were kept.
/// The sentence stays action-neutral because reads and deletes share it; claiming "no bytes
/// were stored" on a GET would describe a write that was never attempted.
fn archive_store(h: &Harness) -> ApiResult<Arc<ArchiveStore>> {
    h.archive.clone().ok_or(ApiError(
        StatusCode::NOT_IMPLEMENTED,
        "Exact archiving is not configured on this server; no archive was written, read, or deleted",
    ))
}
/// Archive and source identifiers are opaque to this layer, so it bounds them rather than
/// asserting a shape the archive tables do not enforce.
fn archive_identifier(value: &str) -> ApiResult<String> {
    if value.is_empty() || value.len() > 200 || value.chars().any(char::is_control) {
        return Err(invalid(
            "Identifier must be 1-200 characters with no control characters",
        ));
    }
    Ok(value.to_string())
}
/// A missing or deleted archive is a 404. A payload that fails authentication is not: that is a
/// tamper or key-rotation fault the operator must see, so it never reads as "no such archive".
fn archive_read_error(error: anyhow::Error) -> ApiError {
    let detail = error.to_string();
    if detail.contains("exact archive not found") || detail.contains("exact archive was deleted") {
        return ApiError(
            StatusCode::NOT_FOUND,
            "No readable exact archive has that identifier",
        );
    }
    // The client gets a fixed sentence; the operator needs the reason, and an archive fault the
    // logs cannot explain is not auditable.
    eprintln!("{}", json!({"event":"archive_read_failed","error":detail}));
    ApiError(
        StatusCode::INTERNAL_SERVER_ERROR,
        "Stored bytes could not be returned; they did not authenticate or are unavailable",
    )
}
/// Stores the body verbatim. The request is deliberately raw bytes rather than JSON: an exact
/// archive that had to survive a JSON string encoding would no longer be exact.
async fn archive_source(
    State(h): State<Harness>,
    Path(source): Path<String>,
    body: Bytes,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let store = archive_store(&h)?;
    let source = archive_identifier(&source)?;
    if body.is_empty() {
        return Err(invalid("Archive body must contain at least one byte"));
    }
    let archive_id = store
        .archive_exact_via(&h.store, source, body.to_vec())
        .await
        .map_err(db_error)?;
    Ok((
        StatusCode::CREATED,
        Json(json!({ "archive_id": archive_id })),
    ))
}
async fn read_archive(State(h): State<Harness>, Path(id): Path<String>) -> ApiResult<Response> {
    let store = archive_store(&h)?;
    let id = archive_identifier(&id)?;
    let bytes = store
        .read_exact_via(&h.store, id)
        .await
        .map_err(archive_read_error)?;
    Ok(([(header::CONTENT_TYPE, "application/octet-stream")], bytes).into_response())
}
/// Deleting an archive is not the same act as forgetting a source: this removes stored bytes and
/// records that removal, and leaves every other privacy state untouched.
async fn delete_archive(
    State(h): State<Harness>,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    let store = archive_store(&h)?;
    let id = archive_identifier(&id)?;
    store
        .delete_archive_via(&h.store, id)
        .await
        .map_err(archive_read_error)?;
    Ok(Json(json!({"deleted":true})))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PrivacyRequest {
    action: String,
}
async fn record_privacy_action(
    State(h): State<Harness>,
    Path(source): Path<String>,
    JsonBody(req): JsonBody<PrivacyRequest>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let store = archive_store(&h)?;
    let source = archive_identifier(&source)?;
    let action = PrivacyAction::parse(&req.action).ok_or(invalid(
        "Action must be one of forget, delete_source or purge_index",
    ))?;
    store
        .record_action_via(&h.store, source, action)
        .await
        .map_err(db_error)?;
    Ok((StatusCode::ACCEPTED, Json(json!({"recorded":req.action}))))
}
/// The recorded causal edges behind one turn. `/chat/requests/{id}/incident` projects a graph and
/// marks unlinked rows as unknown; this returns the underlying edges without interpretation.
async fn request_provenance(
    State(h): State<Harness>,
    Path(request): Path<String>,
) -> ApiResult<Json<Value>> {
    Uuid::parse_str(&request).map_err(|_| invalid("Invalid request identifier"))?;
    Ok(Json(
        h.store.provenance_edges(request).await.map_err(db_error)?,
    ))
}
/// P15-T02: the persisted retrieval explanation for one turn. Every field is a row recall wrote
/// before the provider was called, so this is what retrieval did, not a reconstruction of it.
async fn request_retrieval(
    State(h): State<Harness>,
    Path(request): Path<String>,
) -> ApiResult<Json<Value>> {
    Uuid::parse_str(&request).map_err(|_| invalid("Invalid request identifier"))?;
    Ok(Json(
        h.store
            .retrieval_receipt(request)
            .await
            .map_err(db_error)?
            .ok_or(ApiError(
                StatusCode::NOT_FOUND,
                "No retrieval receipt was recorded for this request",
            ))?,
    ))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RetrievalPreviewRequest {
    prompt: String,
    #[serde(default = "default_scope")]
    scope: String,
    #[serde(default)]
    candidate_id: Option<String>,
}
/// P15-T02 rehearsal: re-run the shipped ranking, optionally as if a pending candidate were
/// approved, and report the retrieval difference. The store applies the approval inside a
/// transaction it always rolls back, so this endpoint approves nothing and saves nothing.
async fn preview_retrieval(
    State(h): State<Harness>,
    JsonBody(req): JsonBody<RetrievalPreviewRequest>,
) -> ApiResult<Json<Value>> {
    safety::scope(&req.scope).map_err(|_| invalid("Invalid scope"))?;
    let prompt = req.prompt.trim().to_string();
    if prompt.is_empty() || prompt.len() > 16_000 {
        return Err(invalid("Preview needs a prompt of 1 to 16,000 characters"));
    }
    if let Some(id) = req.candidate_id.as_deref() {
        Uuid::parse_str(id).map_err(|_| invalid("Invalid candidate identifier"))?;
    }
    Ok(Json(
        h.store
            .preview_retrieval(req.scope, prompt, req.candidate_id)
            .await
            .map_err(db_error)?,
    ))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GovernanceQuery {
    #[serde(default = "default_scope")]
    scope: String,
}
/// P15-T03: the governance overview for one scope. The lapse sweep runs first so the branch,
/// pin and expiry state shown here is the state recall would act on, not a stale snapshot.
async fn memory_governance(
    State(h): State<Harness>,
    Query(q): Query<GovernanceQuery>,
) -> ApiResult<Json<Value>> {
    safety::scope(&q.scope).map_err(|_| invalid("Invalid scope"))?;
    h.store.lapse_expired_memories().await.map_err(db_error)?;
    Ok(Json(
        h.store.memory_governance(q.scope).await.map_err(db_error)?,
    ))
}
/// The stored review history of one memory: value revisions, decisions and usefulness verdicts.
async fn memory_timeline(
    State(h): State<Harness>,
    Path(id): Path<String>,
    Query(q): Query<GovernanceQuery>,
) -> ApiResult<Json<Value>> {
    Uuid::parse_str(&id).map_err(|_| invalid("Invalid memory identifier"))?;
    safety::scope(&q.scope).map_err(|_| invalid("Invalid scope"))?;
    Ok(Json(
        h.store
            .memory_timeline(q.scope, id)
            .await
            .map_err(db_error)?
            .ok_or(ApiError(
                StatusCode::NOT_FOUND,
                "Memory not found in this scope",
            ))?,
    ))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GovernanceAction {
    action: String,
    #[serde(default = "default_scope")]
    scope: String,
    #[serde(default)]
    expires_at: Option<i64>,
    #[serde(default)]
    target_id: Option<String>,
    #[serde(default)]
    note: Option<String>,
    #[serde(default)]
    request_id: Option<String>,
}
/// One audited governance act on one memory: pin, unpin, expire, clear_expiry, merge or a
/// usefulness verdict. Every accepted act writes a revision row, so nothing here is silent.
async fn govern_memory(
    State(h): State<Harness>,
    Path(id): Path<String>,
    JsonBody(req): JsonBody<GovernanceAction>,
) -> ApiResult<Json<Value>> {
    Uuid::parse_str(&id).map_err(|_| invalid("Invalid memory identifier"))?;
    safety::scope(&req.scope).map_err(|_| invalid("Invalid scope"))?;
    if let Some(target) = req.target_id.as_deref() {
        Uuid::parse_str(target).map_err(|_| invalid("Invalid duplicate identifier"))?;
    }
    if let Some(request) = req.request_id.as_deref() {
        Uuid::parse_str(request).map_err(|_| invalid("Invalid request identifier"))?;
    }
    if req
        .note
        .as_ref()
        .is_some_and(|note| note.chars().count() > 500 || safety::sensitive(note))
    {
        return Err(invalid("Note must be under 500 safe characters"));
    }
    // `clear_expiry` is the same audited act as `expire` with no timestamp, so the store keeps
    // one expiry code path instead of two that can disagree.
    let (action, expires_at) = match req.action.as_str() {
        "clear_expiry" => ("expire".to_string(), None),
        "expire" => {
            let at = req
                .expires_at
                .ok_or_else(|| invalid("Scheduling an expiry needs expires_at"))?;
            if at <= 0 {
                return Err(invalid("expires_at must be a positive unix timestamp"));
            }
            ("expire".to_string(), Some(at))
        }
        other @ ("pin" | "unpin" | "merge" | "useful" | "not_useful") => (other.to_string(), None),
        _ => return Err(invalid("Unsupported governance action")),
    };
    match h
        .store
        .govern_memory(
            req.scope,
            id,
            action,
            expires_at,
            req.target_id,
            req.note,
            req.request_id,
        )
        .await
        .map_err(db_error)?
        .as_str()
    {
        "not_found" => Err(ApiError(
            StatusCode::NOT_FOUND,
            "Memory not found in this scope",
        )),
        "target_not_found" => Err(ApiError(
            StatusCode::NOT_FOUND,
            "Duplicate memory not found in this scope",
        )),
        "not_active" | "same_memory" | "missing_target" | "duplicate_feedback" => Err(ApiError(
            StatusCode::CONFLICT,
            "This memory cannot take that governance action; reload the panel",
        )),
        "unsupported_action" => Err(invalid("Unsupported governance action")),
        outcome => Ok(Json(json!({"status":outcome}))),
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BranchRequest {
    branch: String,
    #[serde(default = "default_scope")]
    scope: String,
    #[serde(default)]
    note: Option<String>,
    #[serde(default)]
    activate: bool,
}
/// Create and/or check out a memory branch. Approvals then land on that branch, so a value can
/// be revised under review without overwriting the reviewed one on 'main'.
async fn checkout_memory_branch(
    State(h): State<Harness>,
    JsonBody(req): JsonBody<BranchRequest>,
) -> ApiResult<Json<Value>> {
    safety::scope(&req.scope).map_err(|_| invalid("Invalid scope"))?;
    let branch = req.branch.trim().to_string();
    if branch.is_empty()
        || branch.chars().count() > 60
        || !branch
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
    {
        return Err(invalid(
            "Branch names use 1–60 characters from a-z, 0-9, dot, dash or underscore",
        ));
    }
    if req
        .note
        .as_ref()
        .is_some_and(|note| note.chars().count() > 500 || safety::sensitive(note))
    {
        return Err(invalid("Note must be under 500 safe characters"));
    }
    Ok(Json(
        h.store
            .checkout_memory_branch(req.scope, branch, req.note, req.activate)
            .await
            .map_err(db_error)?,
    ))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HistorySearchQuery {
    q: String,
    #[serde(default = "default_scope")]
    scope: String,
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default)]
    kind: Option<String>,
}
/// P15-T04: scoped full-text search over sanitized history. Every hit carries a citation
/// naming the source row, its revision, its kind and its timestamp, so a result can be traced.
/// Forgotten and source-deleted documents are never returned, and a stored body that no longer
/// passes the shared sanitizer is suppressed rather than served.
async fn search_history(
    State(h): State<Harness>,
    Query(q): Query<HistorySearchQuery>,
) -> ApiResult<Json<Value>> {
    safety::scope(&q.scope).map_err(|_| invalid("Invalid scope"))?;
    let query = q.q.trim().to_string();
    if query.is_empty() || query.len() > 500 {
        return Err(invalid("Search needs a query of 1 to 500 characters"));
    }
    if let Some(session) = q.session_id.as_deref() {
        Uuid::parse_str(session).map_err(|_| invalid("Invalid session identifier"))?;
    }
    if let Some(kind) = q.kind.as_deref() {
        if kind != "turn" && kind != "artifact" {
            return Err(invalid("Kind must be turn or artifact"));
        }
    }
    let scope = storage::SearchScope {
        scope: q.scope,
        session_id: q.session_id,
        kind: q.kind,
    };
    Ok(Json(
        h.store
            .search_history(scope, query)
            .await
            .map_err(db_error)?,
    ))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HistoryIndexRequest {
    #[serde(default)]
    limit: Option<i64>,
}
/// Refresh the sanitized search projection. Only content the shared sanitizer accepts is
/// indexed; refused rows are reported back rather than indexed in a partly cleaned state.
async fn index_history(
    State(h): State<Harness>,
    JsonBody(req): JsonBody<HistoryIndexRequest>,
) -> ApiResult<Json<Value>> {
    let limit = req.limit.unwrap_or(2_000);
    if !(1..=20_000).contains(&limit) {
        return Err(invalid("Limit must be between 1 and 20000"));
    }
    Ok(Json(h.store.index_history(limit).await.map_err(db_error)?))
}

/// The stored audit for one indexed document, which is how the forget / source-delete
/// difference is read back: after a forget this still shows the entry and its content; after a
/// source delete it shows the deletion with the content gone.
async fn history_document(
    State(h): State<Harness>,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    Uuid::parse_str(&id).map_err(|_| invalid("Invalid document identifier"))?;
    Ok(Json(
        h.store
            .history_document_audit(id)
            .await
            .map_err(db_error)?
            .ok_or(ApiError(
                StatusCode::NOT_FOUND,
                "No indexed history document with that identifier",
            ))?,
    ))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HistoryPrivacyRequest {
    action: String,
}
/// The two operations that must not be confused. `forget` stops a document being recalled or
/// returned while its content and revision trail are retained; `restore` lifts that suppression;
/// `delete_source` removes the underlying content and keeps only the audited fact that the entry
/// existed and was deleted. No operation implies another.
async fn history_privacy(
    State(h): State<Harness>,
    Path(id): Path<String>,
    JsonBody(req): JsonBody<HistoryPrivacyRequest>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    Uuid::parse_str(&id).map_err(|_| invalid("Invalid document identifier"))?;
    let outcome = match req.action.as_str() {
        "forget" => h.store.forget_history_document(id, false).await,
        "restore" => h.store.forget_history_document(id, true).await,
        "delete_source" => h.store.delete_history_source(id).await,
        _ => {
            return Err(invalid(
                "Action must be one of forget, restore or delete_source",
            ))
        }
    }
    .map_err(db_error)?;
    let note = match outcome.as_str() {
        "forgotten" => "The entry will not be recalled or returned. Its content and revision trail are retained.",
        "restored" => "Suppression was lifted; the content was never destroyed.",
        "source_deleted" => "The source content was removed. The audited fact that this entry existed and was deleted remains.",
        "content_removed_source_retained" => "The source content was emptied. The source row is retained because durable evidence still references it, and the deletion is audited.",
        "already_forgotten" => "This entry was already forgotten; nothing was written twice.",
        "already_deleted" => "This entry's source was already deleted; nothing was written twice.",
        "not_forgotten" => "This entry was not forgotten, so there was nothing to restore.",
        _ => "No change was recorded.",
    };
    let status = match outcome.as_str() {
        "not_found" => {
            return Err(ApiError(
                StatusCode::NOT_FOUND,
                "No indexed history document with that identifier",
            ))
        }
        "source_deleted" | "forgotten" | "restored" | "content_removed_source_retained" => {
            StatusCode::ACCEPTED
        }
        _ => StatusCode::OK,
    };
    Ok((status, Json(json!({"outcome":outcome,"note":note}))))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ExportBundleRequest {
    kind: String,
    #[serde(default = "default_scope")]
    scope: String,
    audience: String,
    #[serde(default)]
    memory_ids: Vec<String>,
    #[serde(default)]
    document_ids: Vec<String>,
    #[serde(default)]
    note: Option<String>,
}
/// Assemble a draft export bundle. Nothing leaves here: the response is what *would* leave, so
/// an operator can review it before releasing it.
async fn create_export_bundle(
    State(h): State<Harness>,
    JsonBody(req): JsonBody<ExportBundleRequest>,
) -> ApiResult<Json<Value>> {
    safety::scope(&req.scope).map_err(|_| invalid("Invalid scope"))?;
    for id in req.memory_ids.iter().chain(req.document_ids.iter()) {
        Uuid::parse_str(id).map_err(|_| invalid("Invalid selected identifier"))?;
    }
    if req
        .note
        .as_ref()
        .is_some_and(|note| note.chars().count() > 500 || safety::sensitive(note))
    {
        return Err(invalid("Note must be under 500 safe characters"));
    }
    h.store
        .create_export_bundle(
            req.kind,
            req.scope,
            req.audience,
            req.memory_ids,
            req.document_ids,
            req.note,
        )
        .await
        .map(Json)
        // The store rejects an unknown kind or audience, an empty selection, an over-large
        // selection, or an identifier outside the scope. All of them are caller mistakes.
        .map_err(|_| {
            invalid("The selection, kind or audience was rejected: check the scope, at least one item, and at most 500 items")
        })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ExportReviewRequest {
    #[serde(default)]
    approve: bool,
    #[serde(default)]
    content_sha256: Option<String>,
}
/// The audience-review step. With `approve: false` this is a preview: the exact contents plus the
/// digest to approve them against. With `approve: true` and that digest it records the review;
/// a stale digest is a conflict rather than a silent re-approval.
async fn review_export_bundle(
    State(h): State<Harness>,
    Path(id): Path<String>,
    JsonBody(req): JsonBody<ExportReviewRequest>,
) -> ApiResult<Json<Value>> {
    Uuid::parse_str(&id).map_err(|_| invalid("Invalid bundle identifier"))?;
    if let Some(digest) = req.content_sha256.as_deref() {
        if digest.len() != 64 || !digest.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(invalid("content_sha256 must be a 64-character hex digest"));
        }
    }
    let result = h
        .store
        .review_export_bundle(id, req.approve, req.content_sha256)
        .await
        .map_err(db_error)?;
    match result["outcome"].as_str().unwrap_or_default() {
        "not_found" => Err(ApiError(
            StatusCode::NOT_FOUND,
            "No export bundle with that identifier",
        )),
        "stale_review" | "already_released" | "unsanitized_items" | "missing_digest" => {
            Err(ApiError(
                StatusCode::CONFLICT,
                "The export could not be reviewed as requested",
            ))
        }
        _ => Ok(Json(result)),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ExportReleaseRequest {
    #[serde(default)]
    anchor: Option<String>,
}
/// Serialize a reviewed bundle into a sealed continuation packet. Refuses an unreviewed bundle,
/// an unsanitized item, or contents whose digest no longer matches the reviewed one.
async fn release_export_bundle(
    State(h): State<Harness>,
    Path(id): Path<String>,
    JsonBody(req): JsonBody<ExportReleaseRequest>,
) -> ApiResult<Json<Value>> {
    Uuid::parse_str(&id).map_err(|_| invalid("Invalid bundle identifier"))?;
    if req.anchor.as_ref().is_some_and(|anchor| anchor.len() > 500) {
        return Err(invalid("Anchor must be under 500 characters"));
    }
    let result = h
        .store
        .release_export_bundle(id, req.anchor)
        .await
        .map_err(db_error)?;
    match result["outcome"].as_str().unwrap_or_default() {
        "not_found" => Err(ApiError(StatusCode::NOT_FOUND, "No export bundle with that identifier")),
        "released" => Ok(Json(result)),
        _ => Err(ApiError(
            StatusCode::CONFLICT,
            "The export was refused because it is not reviewed, carries unsanitized content, or changed after review",
        )),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ImportPacketRequest {
    #[serde(default)]
    origin: Option<String>,
    packet: Value,
}
/// Import a continuation packet. Verified against the local sanitizer and checksums first, then
/// merged by stable id plus revision, so re-importing the same packet is `unchanged` rather than
/// a duplicate and never overwrites newer local content.
async fn import_continuation_packet(
    State(h): State<Harness>,
    JsonBody(req): JsonBody<ImportPacketRequest>,
) -> ApiResult<Json<Value>> {
    let origin = req.origin.unwrap_or_else(|| "unspecified".to_string());
    if origin.len() > 200 || safety::sensitive(&origin) {
        return Err(invalid("Origin must be under 200 safe characters"));
    }
    h.store
        .import_continuation_packet(origin, req.packet)
        .await
        .map(Json)
        // A packet that fails its format check, its checksum or the local sanitizer is a bad
        // request, not a storage failure: nothing was written.
        .map_err(|_| {
            invalid("The packet was refused: unsupported format version, checksum mismatch, or content the local sanitizer rejects")
        })
}

async fn active_processes() -> ApiResult<Json<Value>> {
    Ok(Json(json!({
        "processes": crate::processes::active(),
        "durability": "in_memory_only; processes are never replayed after restart"
    })))
}

async fn stop_process(Path(pid): Path<u32>) -> ApiResult<Json<Value>> {
    if pid <= 1 {
        return Err(invalid("Invalid process identifier"));
    }
    match crate::processes::terminate_background(pid) {
        Some(process) => Ok(Json(json!({"stopped":true,"process":process}))),
        None => Err(ApiError(
            StatusCode::NOT_FOUND,
            "That process is not registered by this harness; nothing was stopped",
        )),
    }
}

async fn git_state(
    State(h): State<Harness>,
    Query(q): Query<ScopeQuery>,
) -> ApiResult<Json<Value>> {
    safety::scope(&q.scope).map_err(|_| invalid("Invalid scope"))?;
    let root = h
        .store
        .scope_config(q.scope.clone())
        .await
        .map_err(db_error)?
        .and_then(|config| config.root_path)
        .ok_or(ApiError(
            StatusCode::CONFLICT,
            "This scope has no project root, so Git state is unavailable",
        ))?;
    let scope = q.scope;
    let output = tokio::task::spawn_blocking(move || {
        fn command(root: &str, args: &[&str]) -> Value {
            match std::process::Command::new("git")
                .arg("-C")
                .arg(root)
                .args(args)
                .env_remove("GIT_CONFIG_GLOBAL")
                .env_remove("GIT_CONFIG_SYSTEM")
                .output()
            {
                Ok(result) => json!({
                    "exit_code": result.status.code(),
                    "output": crate::tools::truncate_chars(&format!("{}{}", String::from_utf8_lossy(&result.stdout), String::from_utf8_lossy(&result.stderr)), 8192)
                }),
                Err(error) => json!({"exit_code":Value::Null,"output":error.to_string()}),
            }
        }
        json!({
            "scope": scope,
            "root": root,
            "status": command(&root, &["status", "--short", "--branch"]),
            "diff_stat": command(&root, &["diff", "--stat", "--no-ext-diff", "--"]),
            "trusted": true
        })
    })
    .await
    .map_err(|_| ApiError(StatusCode::INTERNAL_SERVER_ERROR, "Git state could not be inspected"))?;
    Ok(Json(output))
}

pub(crate) fn router(state: Harness) -> Router {
    let api = Router::new()
        .route("/chat", post(chat))
        .route("/chat/submit", post(submit_chat))
        .route("/chat/requests/{id}", get(get_receipt))
        .route("/chat/requests/{id}/cancel", post(cancel_request))
        .route("/chat/requests/{id}/retry", post(retry_request))
        .route("/chat/requests/{id}/context", get(get_context))
        .route("/sessions", get(sessions))
        .route("/sessions/{id}", patch(update_session))
        .route("/sessions/{id}/fork", post(fork_session))
        .route("/models", get(models))
        .route("/config", get(get_config).post(set_config))
        .route("/memory/status", get(status))
        .route("/health", get(health))
        .route("/memory/candidates", get(candidates))
        .route("/memory/candidates/{id}/edit", post(edit_candidate))
        .route("/memory/confirm", post(confirm))
        .route(
            "/memory/ingest",
            post(ingest_memory).layer(DefaultBodyLimit::max(2 * 1024 * 1024)),
        )
        .route("/sessions/{id}/messages", get(history))
        .route("/jobs", get(jobs))
        .route("/jobs/{id}/retry", post(retry_job))
        .route("/processes", get(active_processes))
        .route("/processes/{pid}/stop", post(stop_process))
        .route("/git/state", get(git_state))
        .route("/scopes", get(list_scopes))
        .route("/scopes/{scope}", get(get_scope).post(set_scope))
        .route("/permissions", get(permissions))
        .route("/permissions/{id}", post(decide_permission))
        .route("/chat/requests/{id}/steps", get(request_steps))
        .route("/chat/requests/{id}/incident", get(request_incident))
        .route("/sessions/{id}/plan", get(session_plan))
        .route("/activity", get(activity))
        .route("/activity/stream", get(activity_stream))
        .route("/generation", get(generation))
        .route("/generation/stream", get(generation_stream))
        .route("/changes", get(request_changes))
        .route("/changes/{id}/revert", post(revert_change))
        .route("/chat/requests/{id}/provenance", get(request_provenance))
        .route("/chat/requests/{id}/retrieval", get(request_retrieval))
        .route("/memory/retrieval/preview", post(preview_retrieval))
        .route("/memory/governance", get(memory_governance))
        .route("/memory/entries/{id}/timeline", get(memory_timeline))
        .route("/memory/entries/{id}/governance", post(govern_memory))
        .route("/memory/branches", post(checkout_memory_branch))
        .route("/history/search", get(search_history))
        .route("/history/index", post(index_history))
        .route("/history/documents/{id}", get(history_document))
        .route("/history/documents/{id}/privacy", post(history_privacy))
        .route("/export/bundles", post(create_export_bundle))
        .route("/export/bundles/{id}/review", post(review_export_bundle))
        .route("/export/bundles/{id}/release", post(release_export_bundle))
        .route("/export/import", post(import_continuation_packet))
        .route(
            "/sources/{id}/archive",
            post(archive_source).layer(DefaultBodyLimit::max(2 * 1024 * 1024)),
        )
        .route("/sources/{id}/privacy", post(record_privacy_action))
        .route("/archives/{id}", get(read_archive).delete(delete_archive))
        .route_layer(middleware::from_fn_with_state(state.clone(), authenticate));
    Router::new()
        .route("/", get(index))
        .route("/api.js", get(api_js))
        .route("/app.js", get(js))
        .route("/style.css", get(css))
        .route("/auth/session", post(create_browser_session))
        .merge(api)
        .layer(DefaultBodyLimit::max(64 * 1024))
        .layer(middleware::from_fn_with_state(state.clone(), headers))
        .with_state(state)
}
