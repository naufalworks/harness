use anyhow::Result;
use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::env;
use std::io::BufRead;
use std::path::{Path, PathBuf};
use uuid::Uuid;

mod memory_agents;
mod storage;

use memory_agents::MemoryAgents;
use storage::DbStore;

#[derive(Clone)]
struct Harness {
    default_model: String,
    store: DbStore,
    agents: MemoryAgents,
}

impl Harness {
    fn from_env() -> Self {
        dotenvy::dotenv().ok();
        let base_url = env::var("HARNESS_BASE_URL")
            .unwrap_or_else(|_| "https://api.longcat.chat/openai".into())
            .trim_end_matches('/')
            .to_string();
        let api_key = env::var("HARNESS_API_KEY").expect("HARNESS_API_KEY must be set");
        let default_model = env::var("HARNESS_MODEL").unwrap_or_else(|_| "LongCat-2.0".into());
        let db_path = env::var("HARNESS_DB").unwrap_or_else(|_| "harness_memory.db".into());

        let store = DbStore::init(&db_path).expect("Failed to initialize SQLite storage");
        let agents = MemoryAgents::new(&base_url, &api_key, &default_model, store.clone());

        Self {
            default_model,
            store,
            agents,
        }
    }
}

#[derive(Deserialize)]
struct ChatRequest {
    prompt: String,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    session_id: Option<String>,
}

#[derive(Serialize)]
struct ChatResponse {
    response: String,
    session_id: String,
    confirmation_prompt: Option<String>,
    recalled_context_applied: bool,
    background_status: String,
}

#[derive(Deserialize)]
struct ConfirmRequest {
    confirmation_id: String,
    confirm: bool,
}


async fn memory_status(State(h): State<Harness>) -> Response {
    match h.store.get_stats() {
        Ok(stats) => (StatusCode::OK, Json(stats)).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

async fn confirm_memory(
    State(h): State<Harness>,
    Json(req): Json<ConfirmRequest>,
) -> Response {
    match h.store.resolve_confirmation(&req.confirmation_id, req.confirm) {
        Ok(true) => (
            StatusCode::OK,
            Json(json!({
                "status": "success",
                "message": if req.confirm { "Memory confirmed and retained!" } else { "Memory discarded." }
            })),
        )
            .into_response(),
        Ok(false) => (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "Pending confirmation ID not found or already resolved"})),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

#[derive(Deserialize)]
struct IngestRequest {
    /// Absolute path to a .jsonl session file OR a directory; directory = all *.jsonl and junie transcript.md inside (recursive).
    path: String,
    /// "omp" | "claude" | "codex" | "junie" — omit to auto-detect per file.
    #[serde(default)]
    format: Option<String>,
}

async fn models(State(h): State<Harness>) -> Response {
    match h.agents.list_models().await {
        Ok(v) => (StatusCode::OK, Json(v)).into_response(),
        Err(e) => (
            StatusCode::BAD_GATEWAY,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

async fn index() -> Response {
    (
        [(axum::http::header::CONTENT_TYPE, "text/html; charset=utf-8")],
        include_str!("../static/index.html"),
    )
        .into_response()
}

fn parse_message(v: &Value, format: &str) -> Option<(String, String)> {
    let (role, content) = match format {
        // omp / claude: {"type":"message"|"user"|"assistant","message":{"role":..,"content":str|[blocks]}}
        "omp" | "claude" => {
            let t = v.get("type").and_then(|t| t.as_str())?;
            if !matches!(t, "message" | "user" | "assistant") {
                return None;
            }
            let m = v.get("message")?;
            let role = m.get("role").and_then(|r| r.as_str())?.to_string();
            let content = match m.get("content")? {
                Value::String(s) => Some(s.clone()),
                Value::Array(a) => Some(
                    a.iter()
                        .filter_map(|c| c.get("text").and_then(|t| t.as_str()))
                        .collect::<Vec<_>>()
                        .join(" "),
                ),
                _ => None,
            }?;
            (role, content)
        }
        // codex: {"type":"response_item","payload":{"type":"message","role":..,"content":[{...,"text":...}]}}
        "codex" => {
            if v.get("type")?.as_str()? != "response_item" {
                return None;
            }
            let p = v.get("payload")?;
            if p.get("type")?.as_str()? != "message" {
                return None;
            }
            let role = p.get("role")?.as_str()?.to_string();
            let content = p
                .get("content")?
                .as_array()?
                .iter()
                .filter_map(|c| c.get("text").and_then(|t| t.as_str()))
                .collect::<Vec<_>>()
                .join(" ");
            (role, content)
        }
        _ => return None,
    };
    let content = content.trim().to_string();
    if content.is_empty() || content.starts_with('<') || content.starts_with("Caveat:") {
        return None;
    }
    Some((role, content))
}

fn extract_exchanges(path: &Path, format: &str) -> Result<Vec<(String, String)>> {
    if format == "junie" {
        return parse_junie_file(path);
    }
    let file = std::fs::File::open(path)?;
    let mut pairs = Pairing::default();
    for line in std::io::BufReader::new(file).lines() {
        let line = line?;
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if let Some((role, content)) = parse_message(&v, format) {
            pairs.push(role, content);
        }
    }
    Ok(pairs.finish())
}

/// Streams (role, content) into user→assistant pairs; unanswered users are dropped.
#[derive(Default)]
struct Pairing {
    out: Vec<(String, String)>,
    pending_user: Option<String>,
}

impl Pairing {
    fn push(&mut self, role: String, content: String) {
        if role == "user" {
            if let Some(u) = self.pending_user.take() {
                self.out.push((u, String::new()));
            }
            self.pending_user = Some(content);
        } else if role == "assistant" {
            if let Some(u) = self.pending_user.take() {
                self.out.push((u, content));
            }
        }
    }
    fn finish(mut self) -> Vec<(String, String)> {
        // Drop exchanges with no answer (tool noise etc.)
        self.out.retain(|(_, a)| !a.is_empty());
        self.out
    }
}

fn parse_junie_file(path: &Path) -> Result<Vec<(String, String)>> {
    let content = std::fs::read_to_string(path)?;
    let mut pairs = Pairing::default();
    let mut role = "";
    let mut buf = String::new();

    for line in content.lines().chain(std::iter::once("## ")) {
        if let Some(h) = line.strip_prefix("## ") {
            let text = buf.trim().to_string();
            if !text.is_empty() {
                match role {
                    "User" => pairs.push("user".into(), text),
                    "Assistant" => pairs.push("assistant".into(), text),
                    _ => {}
                }
            }
            buf.clear();
            role = h;
        } else if !role.is_empty() {
            buf.push_str(line);
            buf.push('\n');
        }
    }
    Ok(pairs.finish())
}

fn detect_format(path: &Path) -> Option<String> {
    if path.file_name().is_some_and(|n| n == "transcript.md") {
        return Some("junie".into());
    }
    let mut first = String::new();
    std::io::BufReader::new(std::fs::File::open(path).ok()?)
        .read_line(&mut first)
        .ok()?;
    let v: Value = serde_json::from_str(first.trim()).ok()?;
    match v.get("type").and_then(|t| t.as_str()) {
        Some("session_meta") | Some("response_item") | Some("turn_context") => {
            Some("codex".into())
        }
        Some("title") | Some("session") | Some("model_change") => Some("omp".into()),
        _ if v.get("sessionId").is_some() => Some("claude".into()),
        _ => None,
    }
}

async fn ingest_memory(State(h): State<Harness>, Json(req): Json<IngestRequest>) -> Response {
    let p = PathBuf::from(&req.path);
    let files: Vec<PathBuf> = if p.is_dir() {
        let mut found = Vec::new();
        fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
            let Ok(rd) = std::fs::read_dir(dir) else {
                return;
            };
            for e in rd.flatten() {
                let path = e.path();
                if path.is_dir() {
                    walk(&path, out);
                } else if path.extension().map(|x| x == "jsonl").unwrap_or(false)
                    || path.file_name().is_some_and(|n| n == "transcript.md")
                {
                    out.push(path);
                }
            }
        }
        walk(&p, &mut found);
        found.sort();
        found
    } else {
        vec![p.clone()]
    };

    let mut total_files = 0;
    let mut total_exchanges = 0;
    let mut total_memories = 0;
    let mut errors = Vec::new();

    for f in &files {
        let format = match &req.format {
            Some(f) => f.clone(),
            None => match detect_format(f) {
                Some(f) => f,
                None => {
                    errors.push(json!({"file": f.display().to_string(), "error": "unknown format"}));
                    continue;
                }
            },
        };
        match extract_exchanges(f, &format) {
            Ok(exchanges) if !exchanges.is_empty() => {
                let src = f.display().to_string();
                let _ = h.store.insert_artifact(
                    &src,
                    "session_ingest",
                    &format!("format={format} exchanges={}", exchanges.len()),
                );
                total_files += 1;
                total_exchanges += exchanges.len();
                match h.agents.extract_from_history(&exchanges).await {
                    Ok(n) => total_memories += n,
                    Err(e) => errors.push(json!({"file": src, "error": e.to_string()})),
                }
            }
            Ok(_) => {}
            Err(e) => errors.push(json!({"file": f.display().to_string(), "error": e.to_string()})),
        }
    }

    (
        StatusCode::OK,
        Json(json!({
            "files_ingested": total_files,
            "exchanges": total_exchanges,
            "memories_extracted": total_memories,
            "errors": errors,
        })),
    )
        .into_response()
}

async fn chat(
    State(h): State<Harness>,
    Json(req): Json<ChatRequest>,
) -> Response {
    let session_id = req.session_id.unwrap_or_else(|| Uuid::new_v4().to_string());
    let prompt = req.prompt.trim().to_string();
    let model = req.model.unwrap_or_else(|| h.default_model.clone());

    // 0. Check if user is replying to a Gatekeeper confirmation (e.g. "confirm <id>" or "reject <id>")
    if prompt.starts_with("confirm ") || prompt.starts_with("reject ") {
        let parts: Vec<&str> = prompt.split_whitespace().collect();
        if parts.len() >= 2 {
            let is_confirm = parts[0].eq_ignore_ascii_case("confirm");
            let target_id = parts[1];
            if let Ok(true) = h.store.resolve_confirmation(target_id, is_confirm) {
                let msg = if is_confirm {
                    format!("Memory [{target_id}] confirmed and retained in long-term memory.")
                } else {
                    format!("Memory [{target_id}] rejected.")
                };
                return (
                    StatusCode::OK,
                    Json(ChatResponse {
                        response: msg,
                        session_id,
                        confirmation_prompt: None,
                        recalled_context_applied: false,
                        background_status: "synced".to_string(),
                    }),
                )
                    .into_response();
            }
        }
    }

    // 1. RAW STORE: Log untouched user prompt into Artifact Hub
    let _ = h.store.insert_artifact(&session_id, "user_prompt", &prompt);

    // 2. AGENT 1: Recall context (distills relevant memories, zero bloated prompt)
    let recalled_context = h.agents.recall_context(&prompt).await.unwrap_or_default();
    let context_applied = !recalled_context.is_empty();

    let system_prompt = if context_applied {
        format!(
            "You are a helpful, capable assistant. Tailor your behavior to user preferences.\n{}",
            recalled_context
        )
    } else {
        "You are a helpful, capable assistant.".to_string()
    };

    // 3. MAIN AGENT: Call LLM with lean context
    let llm_res = match h.agents.chat(&model, &system_prompt, &prompt).await {
        Ok(v) => v,
        Err(e) => {
            return (
                StatusCode::BAD_GATEWAY,
                Json(json!({"error": e.to_string()})),
            )
                .into_response();
        }
    };

    let answer = llm_res["choices"][0]["message"]["content"]
        .as_str()
        .unwrap_or("")
        .to_string();

    // 4. RAW STORE: Log untouched agent response into Artifact Hub
    let _ = h.store.insert_artifact(&session_id, "agent_response", &answer);
    // 5. AGENT 3: Gatekeeper check (sync evaluation for interactive Option A)
    let gatekeeper_prompt = h
        .agents
        .evaluate_gatekeeper(&session_id, &prompt, &answer)
        .await;

    let mut full_response = answer;
    if let Some(ask) = &gatekeeper_prompt {
        full_response.push_str(ask);
    }
    (
        StatusCode::OK,
        Json(ChatResponse {
            response: full_response,
            session_id,
            confirmation_prompt: gatekeeper_prompt,
            recalled_context_applied: context_applied,
            background_status: "synced".to_string(),
        }),
    )
        .into_response()
}

#[tokio::main]
async fn main() -> Result<()> {
    let state = Harness::from_env();
    let app = Router::new()
        .route("/", get(index))
        .route("/models", get(models))
        .route("/chat", post(chat))
        .route("/memory/status", get(memory_status))
        .route("/memory/confirm", post(confirm_memory))
        .route("/memory/ingest", post(ingest_memory))
        .with_state(state);

    let addr = env::var("HARNESS_ADDR").unwrap_or_else(|_| "0.0.0.0:8080".into());
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    println!("harness listening on http://{addr}");
    axum::serve(listener, app).await?;
    Ok(())
}
