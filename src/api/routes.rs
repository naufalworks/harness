//! The routing table and the request handlers it points at.
//!
//! Moved verbatim from `main.rs` by P12-T01; behaviour is unchanged.
use crate::api::assets::{css, index, js};
use crate::api::auth::{authenticate, create_browser_session, headers};
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
    routing::{get, post},
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
    let code = if receipt["state"] == "captured" || receipt["state"] == "generating" {
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
    let Some(request) = receipt["request_id"].as_str().map(str::to_string) else {
        return Ok((StatusCode::ACCEPTED, Json(receipt)));
    };
    for _ in 0..20 {
        match receipt["state"].as_str() {
            Some("complete") => return Ok((StatusCode::OK, Json(receipt))),
            Some("failed" | "interrupted") => return Ok((StatusCode::BAD_GATEWAY, Json(receipt))),
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
    match receipt["state"].as_str() {
        Some("generating") => Ok((StatusCode::ACCEPTED, Json(receipt))),
        Some("interrupted") if receipt["error_code"] == "cancelled" => {
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
    Ok(Json(
        h.store
            .recorded_sessions(cursor(q)?)
            .await
            .map_err(db_error)?,
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
/// P8-T02: bounded causal incident read model. This is diagnostic evidence, not model reasoning.
async fn request_incident(
    State(h): State<Harness>,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    Uuid::parse_str(&id).map_err(|_| invalid("Invalid request identifier"))?;
    Ok(Json(
        h.store
            .incident_graph(id)
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
    let scope = receipt["scope"].as_str().unwrap_or_default().to_string();
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
                .map(|row| revert_state(root.as_deref(), row))
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
fn revert_state(root: Option<&str>, change: &Value) -> (bool, &'static str) {
    if change["reverted_at"].as_str().is_some() {
        return (false, "Already reverted");
    }
    if change["applied"] != json!(true) {
        return (false, "Never applied, so there is nothing to undo");
    }
    let Some(root) = root else {
        return (
            false,
            "This scope has no project root, so nothing can be restored",
        );
    };
    let Some(path) = change["path"].as_str() else {
        return (false, "This change has no recorded path");
    };
    let Ok(resolved) = tools::paths::resolve(std::path::Path::new(root), path) else {
        return (false, "That path is outside the project root");
    };
    let after = change["after_hash"].as_str().unwrap_or_default();
    match std::fs::read_to_string(&resolved) {
        Ok(text) if tools::content_hash(&text) == after => (true, ""),
        Ok(_) => (false, "File changed since; revert unavailable"),
        // Forward-compatible only: no current tool emits `action=delete`. If one does, the file
        // being gone is the recorded after-state and the diff can rebuild its prior contents.
        Err(_) if change["action"] == json!("delete") => (true, ""),
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
    let (revertable, why) = revert_state(Some(&root.to_string_lossy()), change);
    if !revertable {
        return Err(if why.is_empty() {
            "This change cannot be reverted"
        } else {
            why
        });
    }
    let path = change["path"]
        .as_str()
        .ok_or("This change has no recorded path")?;
    let resolved =
        tools::paths::resolve(root, path).map_err(|_| "That path is outside the project root")?;
    let after = std::fs::read_to_string(&resolved).unwrap_or_default();
    let Some(before_hash) = change["before_hash"].as_str() else {
        // Nothing existed before: undoing a file this turn created means removing it again.
        if change["action"] != json!("create") {
            return Err("This change has no recorded previous content; revert unavailable");
        }
        std::fs::remove_file(&resolved)
            .map_err(|_| "The file could not be removed; nothing was changed")?;
        return Ok("deleted");
    };
    let before = tools::textdiff::reverse(&after, change["diff"].as_str().unwrap_or_default())
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
        change["id"].as_str().unwrap_or("revert"),
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

pub(crate) fn router(state: Harness) -> Router {
    let api = Router::new()
        .route("/chat", post(chat))
        .route("/chat/submit", post(submit_chat))
        .route("/chat/requests/{id}", get(get_receipt))
        .route("/chat/requests/{id}/cancel", post(cancel_request))
        .route("/chat/requests/{id}/retry", post(retry_request))
        .route("/chat/requests/{id}/context", get(get_context))
        .route("/sessions", get(sessions))
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
        .route(
            "/sources/{id}/archive",
            post(archive_source).layer(DefaultBodyLimit::max(2 * 1024 * 1024)),
        )
        .route("/sources/{id}/privacy", post(record_privacy_action))
        .route("/archives/{id}", get(read_archive).delete(delete_archive))
        .route_layer(middleware::from_fn_with_state(state.clone(), authenticate));
    Router::new()
        .route("/", get(index))
        .route("/app.js", get(js))
        .route("/style.css", get(css))
        .route("/auth/session", post(create_browser_session))
        .merge(api)
        .layer(DefaultBodyLimit::max(64 * 1024))
        .layer(middleware::from_fn_with_state(state.clone(), headers))
        .with_state(state)
}
