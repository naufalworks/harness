use anyhow::{bail, Result};
use axum::{
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
    env,
    fmt::Write as _,
    net::SocketAddr,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};
use tokio::sync::Semaphore;
use uuid::Uuid;
mod agent_loop; // P1-T10 agentic turn loop: steps, tools, activity events, budgets
mod agentic_sql; // P1 SQL constants (schema 003); contract-tested by tests/test_agentic_sql.py
mod api; // P12-T01 HTTP surface split into cohesive modules
mod archive; // P13 opt-in exact-original encryption and privacy audit policy
mod context; // P3-T01 deterministic initial window and per-category byte receipts
mod embeddings;
mod ingest;
mod memory_agents;
mod process_lock;
mod processes; // P14-T01 live process-group handles so an explicit cancel can stop blocked work
mod recording;
mod recording_sql;
mod repo_map; // P3-T04 bounded per-scope file/symbol map
mod safety;
mod skills; // P5-T02 skills index and bounded SKILL.md bodies
mod storage;
mod subagent; // P5-T03 read-only exploration sub-agent: tools, bounds, report shape
mod tools; // P1-T05/T06 tool registry (needs-verify: written without cargo) // P4-T02 deterministic offline vectors and cosine scoring
use memory_agents::MemoryAgents;
use api::assets::{css, index, js};
use api::error::{db_error, invalid, ApiError, ApiResult, JsonBody};
use api::auth::{authenticate, create_browser_session, headers, AuthState};
use api::stream::{activity, activity_stream, generation, generation_stream};
use process_lock::ProcessLock;
use storage::DbStore;

const BUILD_COMMIT: &str = env!("HARNESS_GIT_COMMIT");

struct RuntimeIdentity {
    commit: &'static str,
    binary_sha256: String,
    started_at: String,
}
impl RuntimeIdentity {
    fn current() -> Result<Self> {
        let bytes = std::fs::read(std::env::current_exe()?)?;
        let digest = ring::digest::digest(&ring::digest::SHA256, &bytes);
        let mut binary_sha256 = String::with_capacity(64);
        for byte in digest.as_ref() {
            write!(&mut binary_sha256, "{byte:02x}")?;
        }
        Ok(Self {
            commit: BUILD_COMMIT,
            binary_sha256,
            started_at: chrono::Utc::now().to_rfc3339(),
        })
    }
}

#[derive(Default)]
struct WorkerHealth {
    recording: AtomicBool,
    extraction: AtomicBool,
}
#[cfg(test)]
impl WorkerHealth {
    fn ready() -> Self {
        Self {
            recording: AtomicBool::new(true),
            extraction: AtomicBool::new(true),
        }
    }
}

#[derive(Clone)]
struct Harness {
    store: DbStore,
    agents: MemoryAgents,
    auth: Arc<AuthState>,
    port: u16,
    origins: Arc<Vec<String>>,
    api_limit: Arc<Semaphore>,
    identity: Arc<RuntimeIdentity>,
    workers: Arc<WorkerHealth>,
    hsts: bool,
}

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
    let request = receipt["request_id"].as_str().unwrap().to_string();
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
        recording::RetryAdmission::NotFound => Err(ApiError(StatusCode::NOT_FOUND, "Recording receipt not found")),
        recording::RetryAdmission::NotTerminal => Err(ApiError(StatusCode::CONFLICT, "Only failed or interrupted requests can be retried")),
        recording::RetryAdmission::Busy => Err(ApiError(StatusCode::CONFLICT, "This conversation already has an unfinished answer")),
        recording::RetryAdmission::Unsafe => Err(ApiError(StatusCode::CONFLICT, "Retry refused because the run crossed or may have crossed a mutating boundary")),
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

fn router(state: Harness) -> Router {
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

static SHUTDOWN_REQUESTED: AtomicBool = AtomicBool::new(false);
#[cfg(unix)]
extern "C" fn request_shutdown(_: libc::c_int) {
    SHUTDOWN_REQUESTED.store(true, Ordering::Release);
}
async fn shutdown_signal(shutdown: tokio::sync::watch::Sender<bool>) {
    #[cfg(unix)]
    unsafe {
        let handler = request_shutdown as *const () as libc::sighandler_t;
        libc::signal(libc::SIGINT, handler);
        libc::signal(libc::SIGTERM, handler);
    }
    while !SHUTDOWN_REQUESTED.load(Ordering::Acquire) {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    eprintln!("{}", json!({"event":"graceful_shutdown_started"}));
    let _ = shutdown.send(true);
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    let addr: SocketAddr = env::var("HARNESS_ADDR")
        .unwrap_or_else(|_| "127.0.0.1:8080".into())
        .parse()?;
    if !addr.ip().is_loopback() {
        bail!("this single-user release binds only to loopback; use a separately secured deployment design for remote access");
    }
    let token = env::var("HARNESS_AUTH_TOKEN").map_err(|_| {
        anyhow::anyhow!("HARNESS_AUTH_TOKEN is required (at least 32 random characters)")
    })?;
    if token.len() < 32
        || token.len() > 256
        || !token.is_ascii()
        || token.chars().any(char::is_whitespace)
    {
        bail!("HARNESS_AUTH_TOKEN must contain 32-256 non-whitespace ASCII characters");
    }
    let previous_token = env::var("HARNESS_AUTH_TOKEN_PREVIOUS")
        .ok()
        .filter(|value| !value.is_empty());
    if previous_token.as_deref().is_some_and(|value| {
        value == token
            || value.len() < 32
            || value.len() > 256
            || !value.is_ascii()
            || value.chars().any(char::is_whitespace)
    }) {
        bail!("HARNESS_AUTH_TOKEN_PREVIOUS must be distinct and contain 32-256 non-whitespace ASCII characters");
    }
    let session_ttl = env::var("HARNESS_SESSION_TTL_SECONDS")
        .ok()
        .map(|value| value.parse::<u64>())
        .transpose()?
        .unwrap_or(900);
    if !(300..=3600).contains(&session_ttl) {
        bail!("HARNESS_SESSION_TTL_SECONDS must be between 300 and 3600");
    }
    let proxy_identity_header = env::var("HARNESS_PROXY_IDENTITY_HEADER")
        .ok()
        .filter(|value| !value.is_empty())
        .map(|value| header::HeaderName::from_bytes(value.as_bytes()))
        .transpose()?;
    let hsts = env::var("HARNESS_HTTPS_HSTS").is_ok_and(|value| value == "1");
    let key =
        env::var("HARNESS_API_KEY").map_err(|_| anyhow::anyhow!("HARNESS_API_KEY is required"))?;
    if key.trim().is_empty() {
        bail!("HARNESS_API_KEY cannot be empty");
    }
    let database = env::var("HARNESS_DB").unwrap_or_else(|_| "data/harness_v2.db".into());
    if let Some(parent) = std::path::Path::new(&database).parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    // Ownership must be proven before SQLite is opened: DbStore::init runs crash recovery, so a
    // rejected contender must never interrupt the live owner's jobs or receipts.
    let _database_lock = ProcessLock::acquire(&database)?;
    let store = DbStore::init(&database)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for suffix in ["", "-wal", "-shm"] {
            let path = format!("{database}{suffix}");
            if std::path::Path::new(&path).exists() {
                std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
            }
        }
    }
    let agents = MemoryAgents::new(
        &env::var("HARNESS_BASE_URL").unwrap_or_else(|_| "https://api.longcat.chat/openai".into()),
        &key,
        &env::var("HARNESS_MODEL").unwrap_or_else(|_| "LongCat-2.0".into()),
    )?
    .with_spend_store(store.clone());
    let mut origins = vec![
        format!("http://127.0.0.1:{}", addr.port()),
        format!("http://localhost:{}", addr.port()),
        format!("http://[::1]:{}", addr.port()),
    ];
    if let Ok(extra) = env::var("HARNESS_ALLOWED_ORIGINS") {
        for candidate in extra.split(',').map(str::trim).filter(|c| !c.is_empty()) {
            if !origins.iter().any(|o| o == candidate) {
                origins.push(candidate.to_string());
            }
        }
    }
    let identity = Arc::new(RuntimeIdentity::current()?);
    let workers = Arc::new(WorkerHealth::default());
    let state = Harness {
        store: store.clone(),
        agents: agents.clone(),
        auth: Arc::new(AuthState::new(
            token,
            previous_token,
            Duration::from_secs(session_ttl),
            proxy_identity_header,
        )),
        port: addr.port(),
        origins: Arc::new(origins),
        api_limit: Arc::new(Semaphore::new(8)),
        identity,
        workers: workers.clone(),
        hsts,
    };
    let listener = tokio::net::TcpListener::bind(addr).await?;
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    workers.recording.store(true, Ordering::Release);
    let worker_health = workers.clone();
    let recording_store = store.clone();
    let recording_agents = agents.clone();
    let recording_shutdown = shutdown_rx.clone();
    let recording_handle = tokio::spawn(async move {
        recording::worker(recording_store, recording_agents, recording_shutdown).await;
        worker_health.recording.store(false, Ordering::Release);
    });
    workers.extraction.store(true, Ordering::Release);
    let worker_health = workers.clone();
    let extraction_handle = tokio::spawn(async move {
        memory_agents::worker(store, agents, shutdown_rx).await;
        worker_health.extraction.store(false, Ordering::Release);
    });
    println!("harness listening on {{http://{addr}}} (authenticated, single-user)");
    axum::serve(listener, router(state))
        .with_graceful_shutdown(shutdown_signal(shutdown_tx.clone()))
        .await?;
    let _ = shutdown_tx.send(true);
    let drain_seconds = env::var("HARNESS_SHUTDOWN_TIMEOUT_SECONDS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|v| (1..=300).contains(v))
        .unwrap_or(30);
    let drained = tokio::time::timeout(Duration::from_secs(drain_seconds), async {
        let _ = recording_handle.await;
        let _ = extraction_handle.await;
    })
    .await;
    if drained.is_err() {
        eprintln!(
            "{}",
            json!({"event":"graceful_shutdown_timeout","seconds":drain_seconds})
        );
    } else {
        eprintln!("{}", json!({"event":"graceful_shutdown_complete"}));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use crate::api::assets::{APP_JS_GZ, INDEX_GZ, STYLE_CSS_GZ};
    use crate::api::stream::{heartbeat_due, STREAM_HEARTBEAT};
    use crate::api::auth::AuthKind;
    use tower::ServiceExt;
    fn test_identity() -> Arc<RuntimeIdentity> {
        Arc::new(RuntimeIdentity::current().unwrap())
    }
    fn test_workers() -> Arc<WorkerHealth> {
        Arc::new(WorkerHealth::ready())
    }
    fn app_with_workers(store: DbStore, workers: Arc<WorkerHealth>) -> Router {
        router(Harness {
            store,
            agents: MemoryAgents::new("http://127.0.0.1:9", "synthetic", "test").unwrap(),
            auth: Arc::new(AuthState::new(
                "x".repeat(32),
                None,
                Duration::from_secs(900),
                None,
            )),
            port: 8080,
            origins: Arc::new(vec![
                "http://127.0.0.1:8080".into(),
                "http://localhost:8080".into(),
                "http://[::1]:8080".into(),
            ]),
            api_limit: Arc::new(Semaphore::new(8)),
            identity: test_identity(),
            workers,
            hsts: false,
        })
    }
    fn app_with(store: DbStore) -> Router {
        app_with_workers(store, test_workers())
    }
    fn app() -> Router {
        app_with(DbStore::init(":memory:").unwrap())
    }
    #[tokio::test]
    async fn api_requires_auth() {
        let response = app()
            .oneshot(
                axum::http::Request::builder()
                    .uri("/memory/status")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }
    #[tokio::test]
    async fn authenticated_status_succeeds() {
        let response = app()
            .oneshot(
                axum::http::Request::builder()
                    .uri("/memory/status")
                    .header("Authorization", format!("Bearer {}", "x".repeat(32)))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }
    #[tokio::test]
    async fn auth_failure_is_delayed() {
        let started = Instant::now();
        let response = app()
            .oneshot(
                axum::http::Request::builder()
                    .uri("/memory/status")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert!(started.elapsed() >= Duration::from_millis(140));
    }
    #[test]
    fn auth_accepts_previous_token_during_rotation() {
        let auth = AuthState::new(
            "c".repeat(32),
            Some("p".repeat(32)),
            Duration::from_secs(900),
            None,
        );
        assert_eq!(auth.identify(&"c".repeat(32)), Some(AuthKind::Master));
        assert_eq!(auth.identify(&"p".repeat(32)), Some(AuthKind::Master));
        assert_eq!(auth.identify(&"x".repeat(32)), None);
    }
    #[tokio::test]
    async fn auth_browser_session_is_short_lived_and_cannot_mint_sessions() {
        let app = app();
        let response = app
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/auth/session")
                    .header("Authorization", format!("Bearer {}", "x".repeat(32)))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
        let bytes = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .unwrap();
        let payload: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(payload["expires_in"], 900);
        let session = payload["session_token"].as_str().unwrap();
        let status = app
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .uri("/memory/status")
                    .header("Authorization", format!("Bearer {session}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(status.status(), StatusCode::OK);
        let recursive = app
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/auth/session")
                    .header("Authorization", format!("Bearer {session}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(recursive.status(), StatusCode::UNAUTHORIZED);
    }
    #[test]
    fn auth_rate_classes_are_bounded_per_identity() {
        let auth = AuthState::new("x".repeat(32), None, Duration::from_secs(900), None);
        assert!((0..8).all(|_| auth.allow("one", "import", 8)));
        assert!(!auth.allow("one", "import", 8));
        assert!(auth.allow("two", "import", 8));
    }
    #[tokio::test]
    async fn auth_default_body_limit_rejects_oversized_json() {
        let response = app()
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/config")
                    .header("Authorization", format!("Bearer {}", "x".repeat(32)))
                    .header("Content-Type", "application/json")
                    .body(Body::from(format!(
                        "{{\"padding\":\"{}\"}}",
                        "x".repeat(70 * 1024)
                    )))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
    #[tokio::test]
    async fn auth_proxy_identity_and_hsts_are_explicit_opt_ins() {
        let state = Harness {
            store: DbStore::init(":memory:").unwrap(),
            agents: MemoryAgents::new("http://127.0.0.1:9", "synthetic", "test").unwrap(),
            auth: Arc::new(AuthState::new(
                "x".repeat(32),
                None,
                Duration::from_secs(900),
                Some(header::HeaderName::from_static("x-harness-user")),
            )),
            port: 8080,
            origins: Arc::new(vec!["http://127.0.0.1:8080".into()]),
            api_limit: Arc::new(Semaphore::new(8)),
            identity: test_identity(),
            workers: test_workers(),
            hsts: true,
        };
        let app = router(state);
        let missing = app
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .uri("/memory/status")
                    .header("Authorization", format!("Bearer {}", "x".repeat(32)))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(missing.status(), StatusCode::UNAUTHORIZED);
        let trusted = app
            .oneshot(
                axum::http::Request::builder()
                    .uri("/memory/status")
                    .header("Authorization", format!("Bearer {}", "x".repeat(32)))
                    .header("x-harness-user", "owner")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(trusted.status(), StatusCode::OK);
        assert_eq!(
            trusted.headers().get("strict-transport-security").unwrap(),
            "max-age=31536000; includeSubDomains"
        );
    }
    #[test]
    fn auth_frontend_keeps_credentials_out_of_url_and_storage() {
        let source = include_str!("../static/app.js");
        assert!(source.contains("fetch('/auth/session'"));
        assert!(!source.contains("sessionStorage.setItem('token"));
        assert!(!source.contains("localStorage.setItem('harness_token"));
        assert!(!source.contains("sessionStorage.setItem('harness_token"));
        assert!(!source.contains("?token="));
    }
    #[tokio::test]
    async fn health_reports_identity_database_queue_and_workers() {
        let response = app()
            .oneshot(
                axum::http::Request::builder()
                    .uri("/health")
                    .header("Authorization", format!("Bearer {}", "x".repeat(32)))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .unwrap();
        let payload: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(payload["ready"], true);
        assert_eq!(payload["commit"], BUILD_COMMIT);
        assert_eq!(payload["binary_sha256"].as_str().unwrap().len(), 64);
        assert_eq!(payload["schema_version"], 10);
        assert_eq!(payload["database"]["quick_check"], "ok");
        assert_eq!(payload["database"]["queue"]["jobs_pending"], 0);
        assert_eq!(payload["workers"]["recording"], true);
        assert_eq!(payload["workers"]["extraction"], true);
    }
    #[tokio::test]
    async fn health_refuses_readiness_when_a_worker_is_not_running() {
        let response = app_with_workers(
            DbStore::init(":memory:").unwrap(),
            Arc::new(WorkerHealth::default()),
        )
        .oneshot(
            axum::http::Request::builder()
                .uri("/health")
                .header("Authorization", format!("Bearer {}", "x".repeat(32)))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    }
    #[tokio::test]
    async fn health_contract_is_embedded_in_served_javascript() {
        let response = app()
            .oneshot(
                axum::http::Request::builder()
                    .uri("/app.js")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let script = String::from_utf8(body.to_vec()).unwrap();
        assert!(script.contains(BUILD_COMMIT));
        assert!(!script.contains("__HARNESS_BUILD_COMMIT__"));
        assert!(script.contains("Stale UI detected"));
    }
    // P11-T05: delivery evidence. The document must revalidate, the fingerprinted assets must
    // be cacheable and answer a matching validator with 304 and no body, and nothing else may
    // lose `no-store` because the middleware now defers to the handler.
    #[tokio::test]
    async fn fingerprinted_assets_are_cacheable_and_revalidate_without_a_body() {
        let document = app()
            .oneshot(
                axum::http::Request::builder()
                    .uri("/")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(document.status(), StatusCode::OK);
        assert_eq!(
            document.headers().get(header::CACHE_CONTROL).unwrap(),
            "no-cache"
        );
        let html = String::from_utf8(
            axum::body::to_bytes(document.into_body(), 1024 * 1024)
                .await
                .unwrap()
                .to_vec(),
        )
        .unwrap();
        assert!(html.contains(&format!("/app.js?v={BUILD_COMMIT}")));
        assert!(html.contains(&format!("/style.css?v={BUILD_COMMIT}")));
        assert!(!html.contains("__HARNESS_BUILD_COMMIT__"));

        for (uri, expected_type) in [
            (
                format!("/app.js?v={BUILD_COMMIT}"),
                "text/javascript; charset=utf-8",
            ),
            (
                format!("/style.css?v={BUILD_COMMIT}"),
                "text/css; charset=utf-8",
            ),
        ] {
            let first = app()
                .oneshot(
                    axum::http::Request::builder()
                        .uri(&uri)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(first.status(), StatusCode::OK);
            assert_eq!(
                first.headers().get(header::CONTENT_TYPE).unwrap(),
                expected_type
            );
            assert_eq!(
                first.headers().get(header::CACHE_CONTROL).unwrap(),
                "public, max-age=31536000, immutable"
            );
            let etag = first
                .headers()
                .get(header::ETAG)
                .unwrap()
                .to_str()
                .unwrap()
                .to_string();
            assert!(etag.contains(BUILD_COMMIT));

            let cached = app()
                .oneshot(
                    axum::http::Request::builder()
                        .uri(&uri)
                        .header(header::IF_NONE_MATCH, format!("W/{etag}"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(cached.status(), StatusCode::NOT_MODIFIED);
            assert_eq!(cached.headers().get(header::ETAG).unwrap(), etag.as_str());
            let body = axum::body::to_bytes(cached.into_body(), 1024 * 1024)
                .await
                .unwrap();
            assert!(body.is_empty());

            let stale = app()
                .oneshot(
                    axum::http::Request::builder()
                        .uri(&uri)
                        .header(header::IF_NONE_MATCH, "\"some-other-build\"")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(stale.status(), StatusCode::OK);
        }
    }
    #[tokio::test]
    async fn api_responses_are_still_never_stored() {
        let response = app()
            .oneshot(
                axum::http::Request::builder()
                    .uri("/health")
                    .header("Authorization", format!("Bearer {}", "x".repeat(32)))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(header::CACHE_CONTROL).unwrap(),
            "no-store"
        );
    }
    // P11-T05: the idle clock must stay consolidated and visibility-aware. A reintroduced
    // always-on timer is exactly the regression this task removed, so assert the shape.
    #[test]
    fn frontend_runs_one_visibility_aware_clock() {
        let source = include_str!("../static/app.js");
        assert_eq!(source.matches("setInterval(").count(), 1);
        assert!(source.contains("function stopIdleClock()"));
        assert!(source.contains("if (document.hidden) { stopIdleClock(); return; }"));
        assert!(source.contains("if (idleClock.timer || document.hidden) return;"));
    }
    // P11-T06: gzip is verified without a decompression crate. A gzip member ends with the
    // CRC32 and the byte length of the original input, so recomputing both over the identity
    // body proves the precompressed asset decodes to exactly what the server would send
    // otherwise. That is the property that actually matters: a stale or mismatched .gz would
    // serve a different build's script than the ETag claims.
    fn crc32(data: &[u8]) -> u32 {
        let mut crc = 0xFFFF_FFFFu32;
        for byte in data {
            crc ^= *byte as u32;
            for _ in 0..8 {
                crc = if crc & 1 != 0 {
                    (crc >> 1) ^ 0xEDB8_8320
                } else {
                    crc >> 1
                };
            }
        }
        !crc
    }
    async fn fetch_asset(
        uri: &str,
        accept_encoding: Option<&str>,
        if_none_match: Option<&str>,
    ) -> Response {
        let mut request = axum::http::Request::builder().uri(uri);
        if let Some(encoding) = accept_encoding {
            request = request.header(header::ACCEPT_ENCODING, encoding);
        }
        if let Some(validator) = if_none_match {
            request = request.header(header::IF_NONE_MATCH, validator);
        }
        app()
            .oneshot(request.body(Body::empty()).unwrap())
            .await
            .unwrap()
    }
    fn validator(response: &Response) -> String {
        response
            .headers()
            .get(header::ETAG)
            .unwrap()
            .to_str()
            .unwrap()
            .to_string()
    }
    async fn asset_bytes(response: Response) -> Vec<u8> {
        axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024)
            .await
            .unwrap()
            .to_vec()
    }
    #[tokio::test]
    async fn precompressed_assets_are_negotiated_and_decode_to_the_identity_bytes() {
        for uri in [
            format!("/app.js?v={BUILD_COMMIT}"),
            format!("/style.css?v={BUILD_COMMIT}"),
            "/".to_string(),
        ] {
            let identity = fetch_asset(&uri, None, None).await;
            assert_eq!(identity.status(), StatusCode::OK, "{uri}");
            // A client that asks for nothing must never receive encoded bytes, and every
            // representation must advertise that the body varies by encoding so a shared cache
            // cannot hand gzip to a client that did not ask for it.
            assert!(identity.headers().get(header::CONTENT_ENCODING).is_none(), "{uri}");
            assert_eq!(identity.headers().get(header::VARY).unwrap(), "accept-encoding");
            let identity_etag = validator(&identity);
            assert!(!identity_etag.contains("-gzip"), "{uri}");
            let identity_bytes = asset_bytes(identity).await;

            let compressed = fetch_asset(&uri, Some("gzip, deflate, br"), None).await;
            assert_eq!(compressed.status(), StatusCode::OK, "{uri}");
            assert_eq!(compressed.headers().get(header::CONTENT_ENCODING).unwrap(), "gzip");
            assert_eq!(compressed.headers().get(header::VARY).unwrap(), "accept-encoding");
            let gzip_etag = validator(&compressed);
            assert!(gzip_etag.ends_with("-gzip\""), "{uri}: {gzip_etag}");
            assert_ne!(gzip_etag, identity_etag, "{uri}");
            let gzip_bytes = asset_bytes(compressed).await;
            assert_eq!(&gzip_bytes[..3], &[0x1f, 0x8b, 0x08], "{uri} is not a gzip member");
            assert!(
                gzip_bytes.len() < identity_bytes.len(),
                "{uri}: {} compressed vs {} identity",
                gzip_bytes.len(),
                identity_bytes.len()
            );
            let trailer = &gzip_bytes[gzip_bytes.len() - 8..];
            assert_eq!(
                u32::from_le_bytes(trailer[..4].try_into().unwrap()),
                crc32(&identity_bytes),
                "{uri}: gzip CRC32 does not match the identity body"
            );
            assert_eq!(
                u32::from_le_bytes(trailer[4..].try_into().unwrap()) as usize,
                identity_bytes.len(),
                "{uri}: gzip length does not match the identity body"
            );

            // Revalidation is per representation: the gzip validator answers 304, and a
            // validator from the other representation must not.
            let cached = fetch_asset(&uri, Some("gzip"), Some(&gzip_etag)).await;
            assert_eq!(cached.status(), StatusCode::NOT_MODIFIED, "{uri}");
            assert!(asset_bytes(cached).await.is_empty(), "{uri}");
            let mismatched = fetch_asset(&uri, Some("gzip"), Some(&identity_etag)).await;
            assert_eq!(mismatched.status(), StatusCode::OK, "{uri}");
        }
    }
    #[tokio::test]
    async fn clients_that_do_not_accept_gzip_receive_identity_bytes() {
        let uri = format!("/app.js?v={BUILD_COMMIT}");
        for encoding in ["identity", "br", "gzip;q=0", "gzip; q=0.0", "deflate"] {
            let response = fetch_asset(&uri, Some(encoding), None).await;
            assert_eq!(response.status(), StatusCode::OK, "{encoding}");
            assert!(
                response.headers().get(header::CONTENT_ENCODING).is_none(),
                "{encoding} must not be answered with encoded bytes"
            );
        }
    }
    #[test]
    fn precompressed_assets_are_embedded_by_the_build() {
        // A silently empty .gz would disable compression without failing any other test.
        for (name, bytes) in [
            ("index.html", INDEX_GZ),
            ("app.js", APP_JS_GZ),
            ("style.css", STYLE_CSS_GZ),
        ] {
            assert!(bytes.len() > 18, "{name}.gz is empty; gzip was unavailable at build time");
        }
    }
    #[test]
    // P11-T05: long views must stay bounded so a tab left open for days cannot grow the DOM
    // without limit.
    fn frontend_bounds_long_lists() {
        let source = include_str!("../static/app.js");
        assert!(source.contains("const MAX_RENDERED_MESSAGES = 300, MAX_RENDERED_SESSIONS = 200;"));
        assert!(source.contains("function boundLog("));
        assert!(source.contains("boundLog(older);"));
        assert!(source.contains("if ($('sessionlist').childNodes.length >= MAX_RENDERED_SESSIONS) break;"));
    }
    #[tokio::test]
    async fn configured_origin_is_allowed() {
        let state = Harness {
            store: DbStore::init(":memory:").unwrap(),
            agents: MemoryAgents::new("http://127.0.0.1:9", "synthetic", "test").unwrap(),
            auth: Arc::new(AuthState::new(
                "x".repeat(32),
                None,
                Duration::from_secs(900),
                None,
            )),
            port: 8080,
            origins: Arc::new(vec!["https://upcloud-dev.example.ts.net:8443".into()]),
            api_limit: Arc::new(Semaphore::new(8)),
            identity: test_identity(),
            workers: test_workers(),
            hsts: false,
        };
        let response = router(state)
            .oneshot(
                axum::http::Request::builder()
                    .uri("/memory/status")
                    .header("Authorization", format!("Bearer {}", "x".repeat(32)))
                    .header("Origin", "https://upcloud-dev.example.ts.net:8443")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }
    #[tokio::test]
    async fn foreign_origin_is_rejected() {
        let response = app()
            .oneshot(
                axum::http::Request::builder()
                    .uri("/memory/status")
                    .header("Authorization", format!("Bearer {}", "x".repeat(32)))
                    .header("Origin", "https://untrusted.invalid")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }
    fn authorized(method: &str, uri: &str) -> axum::http::request::Builder {
        axum::http::Request::builder()
            .method(method)
            .uri(uri)
            .header("Authorization", format!("Bearer {}", "x".repeat(32)))
    }
    async fn body_json(response: Response) -> Value {
        serde_json::from_slice(
            &axum::body::to_bytes(response.into_body(), 64 * 1024)
                .await
                .unwrap(),
        )
        .unwrap()
    }
    #[tokio::test]
    async fn queued_cancellation_is_durable_and_idempotent() {
        let store = DbStore::init(":memory:").unwrap();
        let app = app_with(store.clone());
        let (request, session) = (storage::uid(), storage::uid());
        store
            .capture_chat(recording::CaptureInput {
                request: request.clone(),
                session,
                scope: "global".into(),
                prompt: "cancel this".into(),
                model: "m".into(),
                signature: storage::uid(),
                redacted: false,
            })
            .await
            .unwrap();
        let cancel = || {
            authorized("POST", &format!("/chat/requests/{request}/cancel"))
                .body(Body::empty())
                .unwrap()
        };
        let first = app.clone().oneshot(cancel()).await.unwrap();
        assert_eq!(first.status(), StatusCode::OK);
        let receipt = body_json(first).await;
        assert_eq!((receipt["state"].as_str(), receipt["error_code"].as_str()), (Some("interrupted"), Some("cancelled")));
        assert!(receipt["cancel_requested_at"].is_string());
        assert!(receipt["cancelled_at"].is_string());
        let replay = app.clone().oneshot(cancel()).await.unwrap();
        assert_eq!(replay.status(), StatusCode::OK);
        assert!(store.claim_recording().await.unwrap().is_none());
        let counts: (i64, i64) = store.run(move |c| Ok((
            c.query_row("SELECT count(*) FROM activity_events WHERE request_id=?1 AND kind='cancel_requested'", [&request], |r| r.get(0))?,
            c.query_row("SELECT count(*) FROM activity_events WHERE request_id=?1 AND kind='turn_cancelled'", [&request], |r| r.get(0))?,
        ))).await.unwrap();
        assert_eq!(counts, (1, 1));
    }

    #[tokio::test]
    async fn active_cancellation_records_intent_until_a_safe_boundary() {
        let store = DbStore::init(":memory:").unwrap();
        let app = app_with(store.clone());
        let (request, session) = (storage::uid(), storage::uid());
        store.capture_chat(recording::CaptureInput {
            request: request.clone(), session, scope: "global".into(), prompt: "stop later".into(),
            model: "m".into(), signature: storage::uid(), redacted: false,
        }).await.unwrap();
        store.claim_recording().await.unwrap().unwrap();
        let response = app.oneshot(
            authorized("POST", &format!("/chat/requests/{request}/cancel"))
                .body(Body::empty()).unwrap(),
        ).await.unwrap();
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        let receipt = body_json(response).await;
        assert_eq!(receipt["state"], json!("generating"));
        assert!(receipt["cancel_requested_at"].is_string());
        assert!(store.cancellation_requested(request.clone()).await.unwrap());
        assert!(store.finalize_cancellation(request.clone()).await.unwrap());
        let terminal = store.recording_receipt(request).await.unwrap().unwrap();
        assert_eq!((terminal["state"].as_str(), terminal["error_code"].as_str()), (Some("interrupted"), Some("cancelled")));
    }

    async fn assert_cancelled_once(store: &DbStore, request: &str) -> Value {
        let receipt = store.recording_receipt(request.into()).await.unwrap().unwrap();
        assert_eq!(receipt["state"], "interrupted");
        assert_eq!(receipt["error_code"], "cancelled");
        assert!(receipt["cancel_requested_at"].is_string());
        assert!(receipt["cancelled_at"].is_string());
        assert!(receipt["response"].is_null());
        let request = request.to_string();
        store.run(move |c| {
            let cancelled: i64 = c.query_row(
                "SELECT count(*) FROM generation_events WHERE request_id=?1 AND state='interrupted' AND error_code='cancelled'",
                [&request], |r| r.get(0),
            )?;
            let other_terminal: i64 = c.query_row(
                "SELECT count(*) FROM generation_events WHERE request_id=?1 AND (state IN ('completed','failed') OR error_code='process_restarted')",
                [&request], |r| r.get(0),
            )?;
            let cancelled_activity: i64 = c.query_row(
                "SELECT count(*) FROM activity_events WHERE request_id=?1 AND kind='turn_cancelled'",
                [&request], |r| r.get(0),
            )?;
            let interrupted: i64 = c.query_row(
                "SELECT count(*) FROM recording_events WHERE request_id=?1 AND kind='interrupted'",
                [&request], |r| r.get(0),
            )?;
            let answers: i64 = c.query_row(
                "SELECT count(*) FROM messages WHERE role='assistant' AND session_id=(SELECT session_id FROM chat_receipts WHERE request_id=?1)",
                [&request], |r| r.get(0),
            )?;
            let message_status: String = c.query_row(
                "SELECT status FROM messages WHERE id=?1", [&request], |r| r.get(0),
            )?;
            assert_eq!((cancelled, other_terminal, cancelled_activity, interrupted, answers), (1, 0, 1, 1, 0));
            assert_eq!(message_status, "failed");
            Ok(())
        }).await.unwrap();
        receipt
    }

    #[tokio::test]
    async fn cancellation_committed_after_worker_check_wins_over_completion() {
        let store = DbStore::init(":memory:").unwrap();
        let (request, session) = (storage::uid(), storage::uid());
        capture_turn(&store, &request, &session, "stop before saving").await;
        store.claim_recording().await.unwrap().unwrap();
        store.save_recording_context(request.clone(), json!({})).await.unwrap();
        // Deterministic ordering at the race boundary: the worker checked, then Stop committed.
        assert!(!store.cancellation_requested(request.clone()).await.unwrap());
        let response = app_with(store.clone()).oneshot(
            authorized("POST", &format!("/chat/requests/{request}/cancel"))
                .body(Body::empty()).unwrap(),
        ).await.unwrap();
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        store.complete_recording(request.clone(), "must not be saved".into()).await.unwrap();
        let terminal = assert_cancelled_once(&store, &request).await;
        assert!(!store.finalize_cancellation(request.clone()).await.unwrap());
        store.fail_recording(request.clone(), "answer_save_failed").await.unwrap();
        assert_eq!(assert_cancelled_once(&store, &request).await, terminal);
        assert!(store.claim_recording().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn cancellation_committed_after_worker_check_wins_over_every_failure() {
        for code in ["context_failed", "provider_failed", "generation_stream_save_failed", "answer_save_failed", "worker_failed"] {
            let store = DbStore::init(":memory:").unwrap();
            let (request, session) = (storage::uid(), storage::uid());
            capture_turn(&store, &request, &session, "stop before failure").await;
            store.claim_recording().await.unwrap().unwrap();
            assert!(!store.cancellation_requested(request.clone()).await.unwrap());
            store.request_cancellation(request.clone()).await.unwrap();
            store.fail_recording(request.clone(), code).await.unwrap();
            let terminal = assert_cancelled_once(&store, &request).await;
            store.fail_recording(request.clone(), code).await.unwrap();
            assert_eq!(assert_cancelled_once(&store, &request).await, terminal);
        }
    }

    #[tokio::test]
    async fn cancellation_does_not_override_an_already_committed_terminal_result() {
        for complete in [true, false] {
            let store = DbStore::init(":memory:").unwrap();
            let (request, session) = (storage::uid(), storage::uid());
            capture_turn(&store, &request, &session, "finish first").await;
            store.claim_recording().await.unwrap().unwrap();
            if complete {
                store.save_recording_context(request.clone(), json!({})).await.unwrap();
                store.complete_recording(request.clone(), "saved answer".into()).await.unwrap();
            } else {
                store.fail_recording(request.clone(), "provider_failed").await.unwrap();
            }
            let terminal = store.recording_receipt(request.clone()).await.unwrap().unwrap();
            let response = app_with(store.clone()).oneshot(
                authorized("POST", &format!("/chat/requests/{request}/cancel"))
                    .body(Body::empty()).unwrap(),
            ).await.unwrap();
            assert_eq!(response.status(), StatusCode::CONFLICT);
            assert_eq!(store.recording_receipt(request.clone()).await.unwrap().unwrap(), terminal);
            assert!(!store.cancellation_requested(request).await.unwrap());
        }
    }

    #[tokio::test]
    async fn cancellation_recovery_finishes_intent_once_without_reclaiming_work() {
        let dir = std::env::temp_dir().join(format!("harness-cancel-recovery-{}", storage::uid()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("test.db");
        let store = DbStore::init(path.to_str().unwrap()).unwrap();
        let (request, session) = (storage::uid(), storage::uid());
        capture_turn(&store, &request, &session, "cancel before restart").await;
        store.claim_recording().await.unwrap().unwrap();
        let step = store.begin_step(agent_loop::NewStep {
            request: request.clone(), session: session.clone(), kind: "tool_call",
            tool_name: Some("write".into()), tool_call_id: Some(storage::uid()),
            input: json!({}), event: "tool_started", payload: json!({}),
        }).await.unwrap();
        let accepted = store.request_cancellation(request.clone()).await.unwrap().unwrap();
        assert_eq!(accepted["state"], "generating");
        let other = storage::uid();
        capture_turn(&store, &other, &storage::uid(), "ordinary restart").await;
        assert_eq!(store.claim_recording().await.unwrap().unwrap().request, other);
        let queued = storage::uid();
        capture_turn(&store, &queued, &storage::uid(), "still queued").await;
        drop(store);
        let mut first_terminal = None;
        for _ in 0..2 {
            let store = DbStore::init(path.to_str().unwrap()).unwrap();
            let terminal = assert_cancelled_once(&store, &request).await;
            assert_eq!(terminal["cancel_requested_at"], accepted["cancel_requested_at"]);
            if let Some(first) = &first_terminal { assert_eq!(&terminal, first); }
            first_terminal = Some(terminal);
            let response = app_with(store.clone()).oneshot(
                authorized("POST", &format!("/chat/requests/{request}/cancel"))
                    .body(Body::empty()).unwrap(),
            ).await.unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let step = step.clone();
            store.run(move |c| {
                let state: (String, Option<String>) = c.query_row(
                    "SELECT status,error_code FROM turn_steps WHERE id=?1", [step],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )?;
                assert_eq!(state, ("interrupted".into(), Some("cancelled".into())));
                Ok(())
            }).await.unwrap();
            let restarted = store.recording_receipt(other.clone()).await.unwrap().unwrap();
            assert_eq!(restarted["state"], "interrupted");
            assert_eq!(restarted["error_code"], "process_restarted");
            assert_eq!(store.recording_receipt(queued.clone()).await.unwrap().unwrap()["state"], "captured");
        }
        let store = DbStore::init(path.to_str().unwrap()).unwrap();
        assert_eq!(store.claim_recording().await.unwrap().unwrap().request, queued);
        assert!(store.claim_recording().await.unwrap().is_none());
        drop(store);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// P14-T01 test helpers: a captured turn, and a completed read-only tool step.
    async fn capture_turn(store: &DbStore, request: &str, session: &str, prompt: &str) {
        store
            .capture_chat(recording::CaptureInput {
                request: request.into(),
                session: session.into(),
                scope: "global".into(),
                prompt: prompt.into(),
                model: "m".into(),
                signature: storage::uid(),
                redacted: false,
            })
            .await
            .unwrap();
    }
    async fn complete_tool_step(store: &DbStore, request: &str, session: &str, tool: &str) {
        let step = store
            .begin_step(agent_loop::NewStep {
                request: request.into(),
                session: session.into(),
                kind: "tool_call",
                tool_name: Some(tool.into()),
                tool_call_id: Some(storage::uid()),
                input: json!({ "tool": tool }),
                event: "tool_started",
                payload: json!({}),
            })
            .await
            .unwrap();
        store
            .finish_step(agent_loop::StepOutcome {
                step,
                request: request.into(),
                session: session.into(),
                status: "complete",
                output: json!({ "tool": tool }),
                bytes: 0,
                truncated: false,
                tokens_in: None,
                tokens_out: None,
                error_code: None,
                event: "tool_finished",
                payload: json!({}),
                artifacts: Vec::new(),
            })
            .await
            .unwrap();
    }

    /// P14-T01: a retry from a turn whose only recorded tool calls are non-mutating is admitted,
    /// records its lineage, and never replays the (absent) side effect.
    #[tokio::test]
    async fn retry_is_admitted_from_a_recorded_non_mutating_boundary() {
        let store = DbStore::init(":memory:").unwrap();
        let app = app_with(store.clone());
        let (request, session) = (storage::uid(), storage::uid());
        capture_turn(&store, &request, &session, "explain the module").await;
        store.claim_recording().await.unwrap().unwrap();
        complete_tool_step(&store, &request, &session, "read").await;
        store
            .fail_recording(request.clone(), "provider_failed")
            .await
            .unwrap();

        let response = app
            .clone()
            .oneshot(
                authorized("POST", &format!("/chat/requests/{request}/retry"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        let receipt = body_json(response).await;
        assert_eq!(receipt["state"], json!("captured"));
        assert_eq!(receipt["retry_of"], json!(request));
        let retry_id = receipt["request_id"].as_str().unwrap().to_string();
        assert_ne!(retry_id, request);
        // The safe boundary is recorded on the *source* turn, and the source points at the retry.
        let source = request.clone();
        let retry_key = retry_id.clone();
        let (boundary, retried_by, lineage): (Option<i64>, Option<String>, Option<String>) = store
            .run(move |c| {
                Ok((
                    c.query_row(
                        "SELECT safe_boundary_seq FROM run_controls WHERE request_id=?1",
                        [&source],
                        |r| r.get(0),
                    )?,
                    c.query_row(
                        "SELECT retried_by FROM run_controls WHERE request_id=?1",
                        [&source],
                        |r| r.get(0),
                    )?,
                    c.query_row(
                        "SELECT retry_of FROM run_controls WHERE request_id=?1",
                        [&retry_key],
                        |r| r.get(0),
                    )?,
                ))
            })
            .await
            .unwrap();
        assert!(boundary.is_some(), "a completed non-mutating step must be a safe boundary");
        assert_eq!(retried_by.as_deref(), Some(retry_id.as_str()));
        assert_eq!(lineage.as_deref(), Some(request.as_str()));
    }

    /// P14-T01: a retry is refused when a completed tool call could have mutated state, because
    /// replaying it would duplicate a side effect.
    #[tokio::test]
    async fn retry_is_refused_when_a_side_effecting_tool_completed() {
        let store = DbStore::init(":memory:").unwrap();
        let app = app_with(store.clone());
        let (request, session) = (storage::uid(), storage::uid());
        capture_turn(&store, &request, &session, "run the migration").await;
        store.claim_recording().await.unwrap().unwrap();
        complete_tool_step(&store, &request, &session, "bash").await;
        store
            .fail_recording(request.clone(), "provider_failed")
            .await
            .unwrap();

        let response = app
            .oneshot(
                authorized("POST", &format!("/chat/requests/{request}/retry"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CONFLICT);
        // No retry turn was created for the source.
        let source = request.clone();
        let retried: bool = store
            .run(move |c| {
                Ok(c.query_row(
                    "SELECT EXISTS(SELECT 1 FROM run_controls WHERE request_id=?1 AND retried_by IS NOT NULL)",
                    [&source],
                    |r| r.get(0),
                )?)
            })
            .await
            .unwrap();
        assert!(!retried, "a refused retry must not create a lineage");
    }

    /// P14-T01: retry is refused while the conversation still owns an unfinished turn, so two
    /// provider turns can never interleave in one session.
    #[tokio::test]
    async fn retry_is_refused_while_the_conversation_has_an_unfinished_answer() {
        let store = DbStore::init(":memory:").unwrap();
        let app = app_with(store.clone());
        let (request, session) = (storage::uid(), storage::uid());
        capture_turn(&store, &request, &session, "first").await;
        store.claim_recording().await.unwrap().unwrap();
        complete_tool_step(&store, &request, &session, "read").await;
        store
            .fail_recording(request.clone(), "provider_failed")
            .await
            .unwrap();
        // A second, still-captured turn in the same session blocks the retry.
        let other = storage::uid();
        capture_turn(&store, &other, &session, "second").await;

        let response = app
            .oneshot(
                authorized("POST", &format!("/chat/requests/{request}/retry"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CONFLICT);
    }

    /// P14-T01: only a terminal turn can be retried.
    #[tokio::test]
    async fn retry_is_refused_for_a_turn_that_has_not_reached_a_terminal_state() {
        let store = DbStore::init(":memory:").unwrap();
        let app = app_with(store.clone());
        let (request, session) = (storage::uid(), storage::uid());
        capture_turn(&store, &request, &session, "still queued").await;

        let response = app
            .oneshot(
                authorized("POST", &format!("/chat/requests/{request}/retry"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CONFLICT);
    }

    /// P14-T01: retrying a turn that already has a retry returns that same retry instead of
    /// creating a second lineage (idempotent admission).
    #[tokio::test]
    async fn retry_is_idempotent_for_an_already_retried_turn() {
        let store = DbStore::init(":memory:").unwrap();
        let app = app_with(store.clone());
        let (request, session) = (storage::uid(), storage::uid());
        capture_turn(&store, &request, &session, "retry twice").await;
        store.claim_recording().await.unwrap().unwrap();
        complete_tool_step(&store, &request, &session, "glob").await;
        store
            .fail_recording(request.clone(), "provider_failed")
            .await
            .unwrap();

        let retry = || {
            authorized("POST", &format!("/chat/requests/{request}/retry"))
                .body(Body::empty())
                .unwrap()
        };
        let first = app.clone().oneshot(retry()).await.unwrap();
        assert_eq!(first.status(), StatusCode::ACCEPTED);
        let first_id = body_json(first).await["request_id"]
            .as_str()
            .unwrap()
            .to_string();
        let second = app.oneshot(retry()).await.unwrap();
        assert_eq!(second.status(), StatusCode::ACCEPTED);
        assert_eq!(body_json(second).await["request_id"], json!(first_id));
    }


    #[tokio::test]
    async fn scopes_are_absent_until_configured_then_read_back_canonicalized() {
        let app = app();
        let missing = app
            .clone()
            .oneshot(
                authorized("GET", "/scopes/global")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(missing.status(), StatusCode::NOT_FOUND);
        let dir = std::env::temp_dir().join(format!("harness-api-scope-{}", storage::uid()));
        std::fs::create_dir_all(&dir).unwrap();
        let canonical = std::fs::canonicalize(&dir).unwrap();
        let request=json!({"root_path":dir.to_string_lossy(),"permission_mode":"auto_all","diagnostics_cmd":"cargo check -q"}).to_string();
        let saved = app
            .clone()
            .oneshot(
                authorized("POST", "/scopes/global")
                    .header("content-type", "application/json")
                    .body(Body::from(request))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(saved.status(), StatusCode::OK);
        assert_eq!(
            body_json(saved).await["root_path"],
            json!(canonical.to_string_lossy())
        );
        let stored = app
            .clone()
            .oneshot(
                authorized("GET", "/scopes/global")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(stored.status(), StatusCode::OK);
        let row = body_json(stored).await;
        assert_eq!(
            (
                &row["permission_mode"],
                &row["diagnostics_cmd"],
                &row["max_steps"]
            ),
            (&json!("auto_all"), &json!("cargo check -q"), &Value::Null)
        );
        std::fs::remove_dir_all(dir).ok();
    }
    #[tokio::test]
    async fn scopes_refuse_a_root_path_that_is_not_an_existing_directory() {
        let request =
            json!({"root_path":"/definitely/not/a/real/harness/project/root"}).to_string();
        let response = app()
            .oneshot(
                authorized("POST", "/scopes/global")
                    .header("content-type", "application/json")
                    .body(Body::from(request))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
    /// A JSON body has to be an object. serde's derive also accepts a sequence for a struct, so
    /// before this guard `[]` deserialized into an all-defaults `ScopePatch` and the scope route
    /// answered 200 after rewriting the row. The rule belongs to every JSON route, not just that
    /// one, so probe a handler for each shape of target type: patch struct, chat struct, string
    /// map, and ingest struct.
    #[tokio::test]
    async fn non_object_json_bodies_are_refused_with_json() {
        for (uri, body) in [
            ("/scopes/global", "[]"),
            ("/scopes/global", r#"[{"permission_mode":"ask"}]"#),
            ("/scopes/global", r#""ask""#),
            ("/scopes/global", "  \n "),
            ("/chat/submit", "[]"),
            ("/config", "[]"),
            ("/memory/ingest", "17"),
        ] {
            let response = app()
                .oneshot(
                    authorized("POST", uri)
                        .header("content-type", "application/json")
                        .body(Body::from(body))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                StatusCode::BAD_REQUEST,
                "{uri} must refuse the body {body}"
            );
            assert!(
                body_json(response).await["error"].is_string(),
                "{uri} must answer JSON for the body {body}"
            );
        }
    }
    /// `{}` is a valid patch that names no field. It still creates a scope that does not exist,
    /// because that is what posting to a new scope asks for; `storage` owns the proof that it
    /// does not rewrite a row that already exists.
    #[tokio::test]
    async fn an_empty_patch_still_creates_a_missing_scope() {
        let app = app();
        let created = app
            .clone()
            .oneshot(
                authorized("POST", "/scopes/fresh")
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(created.status(), StatusCode::OK);
        let stored = app
            .clone()
            .oneshot(
                authorized("GET", "/scopes/fresh")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(stored.status(), StatusCode::OK);
        assert_eq!(body_json(stored).await["permission_mode"], json!("ask"));
    }
    /// P1-T15: the picker's data. A fresh install lists nothing (so the UI can say "set one up")
    /// and a configured scope is listed with the root path that decides whether tools exist.
    #[tokio::test]
    async fn configured_scopes_are_listed_for_the_picker() {
        let app = app();
        let empty = app
            .clone()
            .oneshot(authorized("GET", "/scopes").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(empty.status(), StatusCode::OK);
        assert_eq!(body_json(empty).await["scopes"], json!([]));
        let dir = std::env::temp_dir().join(format!("harness-api-list-{}", storage::uid()));
        std::fs::create_dir_all(&dir).unwrap();
        let canonical = std::fs::canonicalize(&dir).unwrap();
        let request = json!({"root_path":dir.to_string_lossy()}).to_string();
        app.clone()
            .oneshot(
                authorized("POST", "/scopes/myproject")
                    .header("content-type", "application/json")
                    .body(Body::from(request))
                    .unwrap(),
            )
            .await
            .unwrap();
        app.clone()
            .oneshot(
                authorized("POST", "/scopes/blank")
                    .header("content-type", "application/json")
                    .body(Body::from(json!({"permission_mode":"ask"}).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let listed = body_json(
            app.clone()
                .oneshot(authorized("GET", "/scopes").body(Body::empty()).unwrap())
                .await
                .unwrap(),
        )
        .await;
        let rows = listed["scopes"].as_array().unwrap();
        assert_eq!(
            rows.iter()
                .map(|row| row["scope"].as_str().unwrap())
                .collect::<Vec<_>>(),
            vec!["blank", "myproject"]
        );
        assert_eq!(
            rows[0]["root_path"],
            Value::Null,
            "a scope with no root must still be listed, as a scope that cannot run tools"
        );
        assert_eq!(rows[1]["root_path"], json!(canonical.to_string_lossy()));
        std::fs::remove_dir_all(dir).ok();
    }

    /// P1-T11: one pending approval, listed and then decided over HTTP. The loop is not involved;
    /// what matters is that the row moves exactly once and says so.
    #[tokio::test]
    async fn pending_permissions_are_listed_then_resolved_idempotently() {
        let store = DbStore::init(":memory:").unwrap();
        let app = app_with(store.clone());
        let (request, session) = (storage::uid(), storage::uid());
        store
            .capture_chat(recording::CaptureInput {
                request: request.clone(),
                session: session.clone(),
                scope: "global".into(),
                prompt: "overwrite my notes".into(),
                model: "m".into(),
                signature: storage::uid(),
                redacted: false,
            })
            .await
            .unwrap();
        let step = store
            .begin_step(agent_loop::NewStep {
                request: request.clone(),
                session: session.clone(),
                kind: "tool_call",
                tool_name: Some("write".into()),
                tool_call_id: Some("call-1".into()),
                input: json!({"path":"notes.md"}),
                event: "tool_started",
                payload: json!({"tool":"write","summary":"write notes.md"}),
            })
            .await
            .unwrap();
        let id = store
            .request_permission(agent_loop::NewPermission {
                request,
                session,
                step,
                tool: "write".into(),
                summary: "write notes.md (+1 -1)".into(),
                args: json!({"diff":"-beta\n+gamma"}),
                ttl_seconds: 900,
            })
            .await
            .unwrap();

        let listed = app
            .clone()
            .oneshot(
                authorized("GET", "/permissions?scope=global")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(listed.status(), StatusCode::OK);
        let pending = body_json(listed).await;
        assert_eq!(
            (
                &pending["permissions"][0]["id"],
                &pending["permissions"][0]["tool"]
            ),
            (&json!(id), &json!("write"))
        );
        assert_eq!(
            pending["permissions"][0]["summary"],
            json!("write notes.md (+1 -1)")
        );
        assert_eq!(
            pending["permissions"][0]["args"]["diff"],
            json!("-beta\n+gamma"),
            "the card shows the tool's own diff, not model prose"
        );

        let decide = |decision: &str| {
            authorized("POST", &format!("/permissions/{id}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"decision":decision,"scope":"global"}).to_string(),
                ))
                .unwrap()
        };
        let first = app.clone().oneshot(decide("approve")).await.unwrap();
        assert_eq!(first.status(), StatusCode::OK);
        assert_eq!(
            body_json(first).await,
            json!({"status":"approved","recorded":true})
        );
        let replay = app.clone().oneshot(decide("approve")).await.unwrap();
        assert_eq!(
            replay.status(),
            StatusCode::OK,
            "a double-clicked Approve is not an error"
        );
        assert_eq!(body_json(replay).await["recorded"], json!(false));
        let flip = app.clone().oneshot(decide("deny")).await.unwrap();
        assert_eq!(flip.status(), StatusCode::CONFLICT);
        let nonsense = app.clone().oneshot(decide("maybe")).await.unwrap();
        assert_eq!(nonsense.status(), StatusCode::BAD_REQUEST);
        let stranger = app
            .clone()
            .oneshot(
                authorized("POST", &format!("/permissions/{}", storage::uid()))
                    .header("content-type", "application/json")
                    .body(Body::from(json!({"decision":"approve"}).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(stranger.status(), StatusCode::NOT_FOUND);

        let resolved: i64 = store
            .run(|c| {
                Ok(c.query_row(
                    "SELECT count(*) FROM activity_events WHERE kind='permission_resolved'",
                    [],
                    |r| r.get(0),
                )?)
            })
            .await
            .unwrap();
        assert_eq!(
            resolved, 1,
            "an idempotent replay must not log a second decision"
        );
        let empty = app
            .oneshot(
                authorized("GET", "/permissions?scope=global")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(
            body_json(empty).await["permissions"]
                .as_array()
                .unwrap()
                .is_empty(),
            "a resolved approval leaves the pending list"
        );
    }
    #[tokio::test]
    async fn permissions_require_auth() {
        let response = app()
            .oneshot(
                axum::http::Request::builder()
                    .uri("/permissions")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    /// P1-T12: steps, plan and activity are read back from the rows the loop wrote. This builds
    /// those rows directly (the loop's own coverage lives in `agent_loop`) and checks the shape,
    /// the bounds and the cursor the UI will depend on.
    #[tokio::test]
    async fn steps_plan_and_activity_read_back_what_the_loop_recorded() {
        let store = DbStore::init(":memory:").unwrap();
        let app = app_with(store.clone());
        let (request, session) = (storage::uid(), storage::uid());
        store
            .capture_chat(recording::CaptureInput {
                request: request.clone(),
                session: session.clone(),
                scope: "global".into(),
                prompt: "rename beta to gamma".into(),
                model: "m".into(),
                signature: storage::uid(),
                redacted: false,
            })
            .await
            .unwrap();

        // A model call whose stored message array is far larger than the 2 KB preview.
        let wall = "x".repeat(4096);
        let first = store
            .begin_step(agent_loop::NewStep {
                request: request.clone(),
                session: session.clone(),
                kind: "model_call",
                tool_name: None,
                tool_call_id: None,
                input: json!({"messages":[{"role":"user","content":wall}]}),
                event: "model_call_started",
                payload: json!({"attempt":1}),
            })
            .await
            .unwrap();
        let mut done = agent_loop::StepOutcome {
            step: first,
            request: request.clone(),
            session: session.clone(),
            status: "complete",
            output: json!({"text":Value::Null,"tool_calls":[{"name":"edit"}]}),
            bytes: 64,
            truncated: false,
            tokens_in: Some(11),
            tokens_out: Some(7),
            error_code: None,
            event: "model_call_finished",
            payload: json!({"tokens_in":11,"tokens_out":7,"tool_call_count":1}),
            artifacts: vec![],
        };
        store.finish_step(done).await.unwrap();

        // A tool call that changed a file and wrote the plan, exactly as `finish_step` does.
        let second = store
            .begin_step(agent_loop::NewStep {
                request: request.clone(),
                session: session.clone(),
                kind: "tool_call",
                tool_name: Some("edit".into()),
                tool_call_id: Some("call-1".into()),
                input: json!({"path":"notes.md"}),
                event: "tool_started",
                payload: json!({"tool":"edit","summary":"edit notes.md (+1 -1)"}),
            })
            .await
            .unwrap();
        done = agent_loop::StepOutcome {
            step: second,
            request: request.clone(),
            session: session.clone(),
            status: "complete",
            output: json!({"content":"edited","summary":"edit notes.md (+1 -1)","error_code":Value::Null}),
            bytes: 1234,
            truncated: true,
            tokens_in: None,
            tokens_out: None,
            error_code: None,
            event: "tool_finished",
            payload: json!({"tool":"edit","status":"complete"}),
            artifacts: vec![
                tools::Artifact::FileChange {
                    path: "notes.md".into(),
                    action: "modify",
                    before_hash: Some("aaaa".into()),
                    after_hash: Some("bbbb".into()),
                    diff: "-beta\n+gamma".into(),
                    plus: 1,
                    minus: 1,
                },
                tools::Artifact::Plan {
                    items: vec![
                        ("read notes.md".into(), "done".into()),
                        ("rename beta".into(), "in_progress".into()),
                    ],
                },
            ],
        };
        store.finish_step(done).await.unwrap();

        let steps = body_json(
            app.clone()
                .oneshot(
                    authorized("GET", &format!("/chat/requests/{request}/steps"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap(),
        )
        .await;
        let steps = steps["steps"].as_array().unwrap().clone();
        assert_eq!(steps.len(), 2);
        assert_eq!(
            (&steps[0]["seq"], &steps[0]["kind"], &steps[0]["status"]),
            (&json!(0), &json!("model_call"), &json!("complete"))
        );
        assert_eq!(
            (&steps[0]["tokens_in"], &steps[0]["tokens_out"]),
            (&json!(11), &json!(7))
        );
        assert_eq!(
            steps[0]["input_preview"].as_str().unwrap().len(),
            storage::PREVIEW_BYTES,
            "a huge message array is cut to the preview cap"
        );
        assert_eq!(
            steps[0]["previews_capped"],
            json!(true),
            "the client has to know the preview is not the whole story"
        );
        assert_eq!(steps[0]["summary"], Value::Null, "only a tool names itself");
        assert_eq!(
            (&steps[1]["tool_name"], &steps[1]["summary"]),
            (&json!("edit"), &json!("edit notes.md (+1 -1)"))
        );
        assert_eq!(
            (
                &steps[1]["output_bytes"],
                &steps[1]["truncated"],
                &steps[1]["previews_capped"]
            ),
            (&json!(1234), &json!(true), &json!(false)),
            "a capped tool output and a capped preview are different facts"
        );

        let plan = body_json(
            app.clone()
                .oneshot(
                    authorized("GET", &format!("/sessions/{session}/plan"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(plan["items"].as_array().unwrap().len(), 2);
        assert_eq!(
            (&plan["items"][0]["text"], &plan["items"][1]["status"]),
            (&json!("read notes.md"), &json!("in_progress"))
        );

        let feed = body_json(
            app.clone()
                .oneshot(
                    authorized("GET", &format!("/activity?session_id={session}"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap(),
        )
        .await;
        let kinds: Vec<&str> = feed["events"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["kind"].as_str().unwrap())
            .collect();
        // `activity_events` is the agentic feed only; the receipt timeline (`captured`, ...) stays
        // in `recording_events` behind `/chat/requests/{id}/context`.
        assert_eq!(
            kinds,
            vec![
                "model_call_started",
                "model_call_finished",
                "tool_started",
                "tool_finished",
                "file_changed",
                "plan_updated"
            ],
            "{feed:#?}"
        );
        assert_eq!(
            feed["events"][2]["payload"]["summary"],
            json!("edit notes.md (+1 -1)"),
            "a running step's name lives in its event"
        );
        assert_eq!(feed["events"][4]["payload"]["path"], json!("notes.md"));
        let cursor = feed["next_after_seq"].as_i64().unwrap();
        assert_eq!(
            cursor,
            feed["events"].as_array().unwrap().last().unwrap()["seq"]
                .as_i64()
                .unwrap()
        );
        let tail = body_json(
            app.clone()
                .oneshot(
                    authorized(
                        "GET",
                        &format!("/activity?session_id={session}&after_seq={cursor}"),
                    )
                    .body(Body::empty())
                    .unwrap(),
                )
                .await
                .unwrap(),
        )
        .await;
        assert!(
            tail["events"].as_array().unwrap().is_empty(),
            "the cursor must not replay events"
        );
        assert_eq!(
            tail["next_after_seq"],
            json!(cursor),
            "an empty poll leaves the cursor where it was"
        );

        let changes = body_json(
            app.clone()
                .oneshot(
                    authorized("GET", &format!("/changes?request_id={request}"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(changes["changes"].as_array().unwrap().len(), 1);
        assert_eq!(
            (
                &changes["changes"][0]["path"],
                &changes["changes"][0]["applied"],
                &changes["changes"][0]["diff"]
            ),
            (&json!("notes.md"), &json!(true), &json!("-beta\n+gamma"))
        );

        // Bounds and identity.
        for (uri, expected) in [
            (
                format!("/chat/requests/{}/steps", storage::uid()),
                StatusCode::NOT_FOUND,
            ),
            (
                "/chat/requests/not-a-uuid/steps".to_string(),
                StatusCode::BAD_REQUEST,
            ),
            (
                format!("/changes?request_id={}", storage::uid()),
                StatusCode::NOT_FOUND,
            ),
            (
                format!("/activity?session_id={session}&after_seq=-1"),
                StatusCode::BAD_REQUEST,
            ),
            (
                "/activity?session_id=nope".to_string(),
                StatusCode::BAD_REQUEST,
            ),
            ("/activity".to_string(), StatusCode::BAD_REQUEST),
            (
                format!("/sessions/{}/plan", "not-a-uuid"),
                StatusCode::BAD_REQUEST,
            ),
        ] {
            let response = app
                .clone()
                .oneshot(authorized("GET", &uri).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), expected, "{uri}");
        }
        for uri in [
            format!("/chat/requests/{request}/steps"),
            format!("/sessions/{session}/plan"),
            format!("/activity?session_id={session}"),
            format!("/changes?request_id={request}"),
        ] {
            let response = app
                .clone()
                .oneshot(
                    axum::http::Request::builder()
                        .uri(&uri)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{uri}");
        }
        let unknown = body_json(
            app.oneshot(
                authorized("GET", &format!("/sessions/{}/plan", storage::uid()))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap(),
        )
        .await;
        assert_eq!(
            unknown,
            json!({"items":[]}),
            "a session without a plan reads as empty, like its message history"
        );
    }

    /// P2-T03: the diff card's footer and the undo behind it. A revert is offered only while the
    /// file still hashes to `after_hash`, and what it writes back is proved against `before_hash`.
    /// A file created by the turn has no `before_hash`, so its honest undo is removing that file.
    #[tokio::test]
    async fn reverting_recorded_changes_restores_the_file_and_refuses_once_it_moved_on() {
        let store = DbStore::init(":memory:").unwrap();
        let app = app_with(store.clone());
        let dir = std::env::temp_dir().join(format!("harness-revert-{}", storage::uid()));
        std::fs::create_dir_all(&dir).unwrap();
        let root = std::fs::canonicalize(&dir).unwrap();
        app.clone()
            .oneshot(
                authorized("POST", "/scopes/global")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({"root_path":root.to_string_lossy()}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        let (request, session) = (storage::uid(), storage::uid());
        store
            .capture_chat(recording::CaptureInput {
                request: request.clone(),
                session: session.clone(),
                scope: "global".into(),
                prompt: "uppercase beta".into(),
                model: "m".into(),
                signature: storage::uid(),
                redacted: false,
            })
            .await
            .unwrap();
        let step = store
            .begin_step(agent_loop::NewStep {
                request: request.clone(),
                session: session.clone(),
                kind: "tool_call",
                tool_name: Some("edit".into()),
                tool_call_id: Some("call-1".into()),
                input: json!({"path":"notes.md"}),
                event: "tool_started",
                payload: json!({"tool":"edit"}),
            })
            .await
            .unwrap();

        // Two files edited in one turn. One is left alone; the other is edited again afterwards.
        let mut artifacts = vec![];
        for (name, before, after) in [
            ("notes.md", "alpha\nbeta\ngamma\n", "alpha\nBETA\ngamma\n"),
            ("keep.md", "one\ntwo\n", "one\nTWO\n"),
        ] {
            std::fs::write(root.join(name), after).unwrap();
            let diff = tools::textdiff::unified(name, before, after);
            artifacts.push(tools::Artifact::FileChange {
                path: name.into(),
                action: "modify",
                before_hash: Some(tools::content_hash(before)),
                after_hash: Some(tools::content_hash(after)),
                diff: diff.text.clone(),
                plus: diff.plus,
                minus: diff.minus,
            });
        }
        let (created_name, created_after) = ("new.md", "created by the turn\n");
        std::fs::write(root.join(created_name), created_after).unwrap();
        let created_diff = tools::textdiff::unified(created_name, "", created_after);
        artifacts.push(tools::Artifact::FileChange {
            path: created_name.into(),
            action: "create",
            before_hash: None,
            after_hash: Some(tools::content_hash(created_after)),
            diff: created_diff.text.clone(),
            plus: created_diff.plus,
            minus: created_diff.minus,
        });
        store
            .finish_step(agent_loop::StepOutcome {
                step,
                request: request.clone(),
                session: session.clone(),
                status: "complete",
                output: json!({"content":"edited"}),
                bytes: 12,
                truncated: false,
                tokens_in: None,
                tokens_out: None,
                error_code: None,
                event: "tool_finished",
                payload: json!({"tool":"edit","status":"complete"}),
                artifacts,
            })
            .await
            .unwrap();
        std::fs::write(root.join("keep.md"), "one\ntwo\nthree\n").unwrap();

        let listed = body_json(
            app.clone()
                .oneshot(
                    authorized("GET", &format!("/changes?request_id={request}"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap(),
        )
        .await;
        let rows = listed["changes"].as_array().unwrap().clone();
        assert_eq!(rows.len(), 3);
        let card = rows
            .iter()
            .find(|row| row["path"] == json!("notes.md"))
            .unwrap()
            .clone();
        let stale = rows
            .iter()
            .find(|row| row["path"] == json!("keep.md"))
            .unwrap()
            .clone();
        let created = rows
            .iter()
            .find(|row| row["path"] == json!(created_name))
            .unwrap()
            .clone();
        assert_eq!(
            (&card["revertable"], &card["revert_note"]),
            (&json!(true), &Value::Null)
        );
        assert_eq!(
            (&created["revertable"], &created["revert_note"]),
            (&json!(true), &Value::Null)
        );
        assert_eq!(
            (&stale["revertable"], &stale["revert_note"]),
            (
                &json!(false),
                &json!("File changed since; revert unavailable")
            ),
            "the card says why on its own, without the user pressing anything"
        );

        let id = card["id"].as_str().unwrap().to_string();
        let done = app
            .clone()
            .oneshot(
                authorized("POST", &format!("/changes/{id}/revert"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(done.status(), StatusCode::OK);
        assert_eq!(
            body_json(done).await,
            json!({"status":"restored","path":"notes.md","recorded":true})
        );
        assert_eq!(
            std::fs::read_to_string(root.join("notes.md")).unwrap(),
            "alpha\nbeta\ngamma\n",
            "the file is byte for byte what it was"
        );

        let created_id = created["id"].as_str().unwrap().to_string();
        let removed = app
            .clone()
            .oneshot(
                authorized("POST", &format!("/changes/{created_id}/revert"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(removed.status(), StatusCode::OK);
        assert_eq!(
            body_json(removed).await,
            json!({"status":"deleted","path":created_name,"recorded":true})
        );
        assert!(
            !root.join(created_name).exists(),
            "undoing a create removes the file instead of leaving an empty one"
        );
        let removed_again = app
            .clone()
            .oneshot(
                authorized("POST", &format!("/changes/{created_id}/revert"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            removed_again.status(),
            StatusCode::CONFLICT,
            "a second create undo neither reappears nor records another event"
        );
        assert!(!root.join(created_name).exists());

        let again = app
            .clone()
            .oneshot(
                authorized("POST", &format!("/changes/{id}/revert"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            again.status(),
            StatusCode::CONFLICT,
            "a double-clicked Revert writes nothing"
        );
        let feed = body_json(
            app.clone()
                .oneshot(
                    authorized("GET", &format!("/activity?session_id={session}"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap(),
        )
        .await;
        let reverts: Vec<&Value> = feed["events"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|e| e["kind"] == json!("file_reverted"))
            .collect();
        assert_eq!(
            reverts.len(),
            2,
            "each undo is announced once, on the feed the edit used: {feed:#?}"
        );
        assert!(reverts
            .iter()
            .any(|e| e["payload"]["path"] == json!("notes.md")));
        assert!(reverts
            .iter()
            .any(|e| e["payload"]["path"] == json!(created_name)));

        let relisted = body_json(
            app.clone()
                .oneshot(
                    authorized("GET", &format!("/changes?request_id={request}"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap(),
        )
        .await;
        let reverted = relisted["changes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["path"] == json!("notes.md"))
            .unwrap()
            .clone();
        assert!(reverted["reverted_at"].is_string());
        assert_eq!(
            (&reverted["revertable"], &reverted["revert_note"]),
            (&json!(false), &json!("Already reverted"))
        );
        let removed_card = relisted["changes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["path"] == json!(created_name))
            .unwrap()
            .clone();
        assert!(removed_card["reverted_at"].is_string());
        assert_eq!(
            (&removed_card["revertable"], &removed_card["revert_note"]),
            (&json!(false), &json!("Already reverted"))
        );

        let stale_id = stale["id"].as_str().unwrap().to_string();
        let refused = app
            .clone()
            .oneshot(
                authorized("POST", &format!("/changes/{stale_id}/revert"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(refused.status(), StatusCode::CONFLICT);
        assert_eq!(
            std::fs::read_to_string(root.join("keep.md")).unwrap(),
            "one\ntwo\nthree\n",
            "a refused revert leaves the later edit alone"
        );

        for (uri, expected) in [
            (
                format!("/changes/{}/revert", storage::uid()),
                StatusCode::NOT_FOUND,
            ),
            (
                "/changes/not-a-uuid/revert".to_string(),
                StatusCode::BAD_REQUEST,
            ),
        ] {
            let response = app
                .clone()
                .oneshot(authorized("POST", &uri).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), expected, "{uri}");
        }
        let unauthenticated = app
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri(format!("/changes/{id}/revert"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(unauthenticated.status(), StatusCode::UNAUTHORIZED);
        std::fs::remove_dir_all(dir).ok();
    }

    /// Read the stream until `until` frames carry an `id`, or the deadline passes. A stream never
    /// ends on its own, so a test must say how much it wants and how long it will wait for it.
    async fn read_frames(response: Response, until: usize, millis: u64) -> String {
        use futures_core::Stream;
        let mut stream = response.into_body().into_data_stream();
        let mut text = String::new();
        let deadline = tokio::time::Instant::now() + Duration::from_millis(millis);
        while text.matches("id: ").count() < until {
            let Ok(Some(Ok(bytes))) = tokio::time::timeout_at(
                deadline,
                std::future::poll_fn(|cx| std::pin::Pin::new(&mut stream).poll_next(cx)),
            )
            .await
            else {
                break;
            };
            text.push_str(&String::from_utf8_lossy(&bytes));
        }
        text
    }
    /// P2-T01: the live feed carries the same rows `/activity` returns, keyed by the DB sequence.
    /// The cursor is the whole recovery story: reconnecting with the last `id` must not replay it.
    #[tokio::test]
    async fn activity_stream_frames_recorded_rows_and_resumes_from_the_cursor() {
        let store = DbStore::init(":memory:").unwrap();
        let app = app_with(store.clone());
        let (request, session) = (storage::uid(), storage::uid());
        store
            .capture_chat(recording::CaptureInput {
                request: request.clone(),
                session: session.clone(),
                scope: "global".into(),
                prompt: "stream this turn".into(),
                model: "m".into(),
                signature: storage::uid(),
                redacted: false,
            })
            .await
            .unwrap();
        let step = store
            .begin_step(agent_loop::NewStep {
                request: request.clone(),
                session: session.clone(),
                kind: "model_call",
                tool_name: None,
                tool_call_id: None,
                input: json!({"messages":[]}),
                event: "model_call_started",
                payload: json!({"attempt":1}),
            })
            .await
            .unwrap();
        store
            .finish_step(agent_loop::StepOutcome {
                step,
                request: request.clone(),
                session: session.clone(),
                status: "complete",
                output: json!({"text":"done","tool_calls":[]}),
                bytes: 4,
                truncated: false,
                tokens_in: Some(3),
                tokens_out: Some(2),
                error_code: None,
                event: "model_call_finished",
                payload: json!({"tokens_in":3,"tokens_out":2}),
                artifacts: vec![],
            })
            .await
            .unwrap();

        let live = app
            .clone()
            .oneshot(
                authorized("GET", &format!("/activity/stream?session_id={session}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(live.status(), StatusCode::OK);
        assert_eq!(
            live.headers().get(header::CONTENT_TYPE).unwrap(),
            "text/event-stream; charset=utf-8"
        );
        let frames = read_frames(live, 2, 5000).await;
        let ids: Vec<i64> = frames
            .lines()
            .filter_map(|line| line.strip_prefix("id: "))
            .filter_map(|seq| seq.parse().ok())
            .collect();
        let kinds: Vec<&str> = frames
            .lines()
            .filter_map(|line| line.strip_prefix("event: "))
            .collect();
        assert_eq!(
            kinds,
            vec!["model_call_started", "model_call_finished"],
            "{frames}"
        );
        assert_eq!(ids.len(), 2);
        assert!(ids[1] > ids[0], "the id is the row's own sequence: {ids:?}");
        let payloads: Vec<Value> = frames
            .lines()
            .filter_map(|line| line.strip_prefix("data: "))
            .map(|data| serde_json::from_str(data).unwrap())
            .collect();
        assert_eq!(
            (
                &payloads[0]["seq"],
                &payloads[0]["kind"],
                &payloads[0]["request_id"]
            ),
            (
                &json!(ids[0]),
                &json!("model_call_started"),
                &json!(request)
            ),
            "a frame is the /activity row itself, not a second rendering of it"
        );
        assert_eq!(payloads[1]["payload"]["tokens_in"], json!(3));

        let resumed = app
            .clone()
            .oneshot(
                authorized(
                    "GET",
                    &format!("/activity/stream?session_id={session}&after_seq={}", ids[1]),
                )
                .body(Body::empty())
                .unwrap(),
            )
            .await
            .unwrap();
        let replayed = read_frames(resumed, 1, 500).await;
        assert!(
            !replayed.contains("id: "),
            "a resumed stream must not replay a delivered event: {replayed}"
        );

        for (uri, expected) in [
            (
                format!("/activity/stream?session_id={session}&after_seq=-1"),
                StatusCode::BAD_REQUEST,
            ),
            (
                "/activity/stream?session_id=nope".to_string(),
                StatusCode::BAD_REQUEST,
            ),
            ("/activity/stream".to_string(), StatusCode::BAD_REQUEST),
        ] {
            let response = app
                .clone()
                .oneshot(authorized("GET", &uri).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), expected, "{uri}");
        }
        let anonymous = app
            .oneshot(
                axum::http::Request::builder()
                    .uri(format!("/activity/stream?session_id={session}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            anonymous.status(),
            StatusCode::UNAUTHORIZED,
            "the stream is behind the same bearer token as the feed"
        );
    }
    /// A quiet turn still owes the client a sign of life every 15 s. Virtual time keeps that
    /// assertion real without a 15-second test.
    #[tokio::test(start_paused = true)]
    async fn activity_stream_heartbeats_a_quiet_session() {
        assert!(!heartbeat_due(STREAM_HEARTBEAT - Duration::from_millis(1)));
        assert!(heartbeat_due(STREAM_HEARTBEAT));
        let response = app()
            .oneshot(
                authorized(
                    "GET",
                    &format!("/activity/stream?session_id={}", storage::uid()),
                )
                .body(Body::empty())
                .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "a session with no events yet is empty, not missing"
        );
        // Nothing carries an `id`, so this reads until the virtual clock passes the deadline.
        let quiet = read_frames(response, 1, 20_000).await;
        assert!(
            !quiet.contains("id: "),
            "a session with no rows must not invent events: {quiet}"
        );
        assert!(
            quiet
                .split_inclusive("\n\n")
                .all(|frame| frame == ": heartbeat\n\n"),
            "an idle stream sends comments only: {quiet}"
        );
        assert!(
            quiet.contains(": heartbeat\n\n"),
            "an idle stream must distinguish a quiet agent from a dead socket: {quiet}"
        );
    }
}

#[cfg(test)]
mod recording_tests;
