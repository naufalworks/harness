//! The agentic turn loop: docs/design/agentic-turn.md#loop.
//!
//! Two rules shape everything here.
//! 1. A step row is committed *before* the side effect it describes and finished *before* its
//!    output can influence anything else, so a crash leaves a readable half-turn instead of an
//!    invisible one, and a restart can honestly report `interrupted`.
//! 2. Tools never touch the database. They return `Artifact`s, and this module persists them in
//!    the same transaction that finishes the step, so an applied edit and its audit row cannot
//!    drift apart.
use anyhow::{bail, Result};
use rusqlite::{params, OptionalExtension, TransactionBehavior};
use serde_json::{json, Value};
use std::{sync::Arc, time::{Duration, Instant}};
use crate::{agentic_sql as sql, ingest::Event, memory_agents::{self, MemoryAgents, ToolCall},
    safety, storage::{now, uid, DbStore, Recall, ScopeConfig},
    tools::{Artifact, PermissionMode, Registry, ToolResult, ToolStatus, MAX_OUTPUT}};

/// System prompt for a tool-enabled turn. Rendered once per turn, never model-authored.
const AGENT_PROMPT: &str = include_str!("../prompts/main_agent.md");
/// How often the loop looks for a human decision on a pending approval.
const PERMISSION_POLL: Duration = Duration::from_millis(500);
/// docs/design/agentic-turn.md#permissions gives an approval 30 minutes. A turn's wall budget
/// (15 min by default) is shorter, and P1-T11 settles that conflict the honest way: the earlier
/// deadline wins, and `expires_at` is written as that deadline, so a pending row never advertises
/// an approval window the waiting turn will not actually honour.
const PERMISSION_TTL_SECONDS: i64 = 30 * 60;
/// What the assistant says when the provider returns neither text nor a tool call. Saying this
/// is honest; inventing a summary of work that did not happen is not.
const NO_TEXT: &str = "(the model returned no answer text for this turn)";

// ---- Durable transitions ---------------------------------------------------------------

/// What a human decision did to a pending approval. The HTTP layer maps these to status codes;
/// the loop never sees them, because it only reads the stored `status`.
pub enum Resolution {
    /// This call wrote the decision and its `permission_resolved` event.
    Recorded,
    /// The same decision was already on record: a replayed click, not an error.
    Unchanged,
    /// Already resolved the other way. Nothing was changed.
    Conflict,
    /// The turn stopped waiting before the decision arrived.
    Expired,
    /// No such approval in this scope.
    NotFound,
}

/// A `running` step plus the activity event that announces it.
pub struct NewStep {
    pub request: String, pub session: String, pub kind: &'static str,
    pub tool_name: Option<String>, pub tool_call_id: Option<String>, pub input: Value,
    pub event: &'static str, pub payload: Value,
}

/// A finished step, the side effects it asked to have recorded, and its activity event.
pub struct StepOutcome {
    pub step: String, pub request: String, pub session: String, pub status: &'static str,
    pub output: Value, pub bytes: i64, pub truncated: bool,
    pub tokens_in: Option<i64>, pub tokens_out: Option<i64>, pub error_code: Option<String>,
    pub event: &'static str, pub payload: Value, pub artifacts: Vec<Artifact>,
}

impl DbStore {
    /// Commit a `running` step and its `*_started` event together, and return the step id.
    /// The sequence number comes from the same transaction, so two steps of one request can
    /// never share a `seq`.
    pub async fn begin_step(&self, step: NewStep) -> Result<String> {
        let id = uid();
        let created = id.clone();
        self.run(move |c| {
            let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let seq: i64 = tx.query_row(sql::STEP_NEXT_SEQ, [&step.request], |r| r.get(0))?;
            let stamp = now();
            tx.execute(sql::STEP_BEGIN, params![id, step.request, seq, step.kind, step.tool_name, step.tool_call_id, step.input.to_string(), stamp])?;
            tx.execute(sql::EVENT, params![step.request, step.session, id, step.event, step.payload.to_string(), stamp])?;
            tx.commit()?;
            Ok(())
        }).await?;
        Ok(created)
    }

    /// Finish a step, persist its artifacts and announce all of it in one transaction.
    /// `file_changes` rows are written with `applied=1`: the tool has already written the file
    /// by the time it hands back the artifact, so claiming anything else would be a lie.
    pub async fn finish_step(&self, done: StepOutcome) -> Result<()> {
        self.run(move |c| {
            let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let stamp = now();
            if tx.execute(sql::STEP_FINISH, params![done.step, done.status, done.output.to_string(), done.bytes, done.truncated, done.tokens_in, done.tokens_out, done.error_code, stamp])? != 1 {
                bail!("step {} is no longer running", done.step);
            }
            tx.execute(sql::EVENT, params![done.request, done.session, done.step, done.event, done.payload.to_string(), stamp])?;
            for artifact in &done.artifacts {
                match artifact {
                    Artifact::FileChange { path, action, before_hash, after_hash, diff, plus, minus } => {
                        let change = uid();
                        tx.execute(sql::FILE_CHANGE, params![change, done.request, done.step, path, action, before_hash, after_hash, diff, 1, stamp])?;
                        tx.execute(sql::EVENT, params![done.request, done.session, done.step, "file_changed",
                            json!({"change_id":change,"path":path,"action":action,"plus":plus,"minus":minus}).to_string(), stamp])?;
                    }
                    Artifact::Plan { items } => {
                        let stored = crate::storage::write_plan(&tx, &done.session, items)?;
                        tx.execute(sql::EVENT, params![done.request, done.session, done.step, "plan_updated", stored.to_string(), stamp])?;
                    }
                }
            }
            tx.commit()?;
            Ok(())
        }).await
    }

    /// An activity row for a transition that is not a step: turn start, budget stop.
    pub async fn activity(&self, request: String, session: String, kind: &'static str, payload: Value) -> Result<()> {
        self.run(move |c| {
            c.execute(sql::EVENT, params![request, session, None::<String>, kind, payload.to_string(), now()])?;
            Ok(())
        }).await
    }

    /// Create the `pending` approval a side-effecting call needs, with its event. The summary
    /// and payload come from the tool, never from model text.
    /// `ttl_seconds` is the caller's effective deadline (see `permission_ttl`), not a wish: the
    /// row expires when the turn stops waiting, so the UI and the loop agree on the window.
    pub async fn request_permission(&self, request: String, session: String, step: String, tool: String, summary: String, args: Value, ttl_seconds: i64) -> Result<String> {
        let id = uid();
        let created = id.clone();
        self.run(move |c| {
            let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let stamp = now();
            let expires = (chrono::Utc::now() + chrono::Duration::seconds(ttl_seconds)).to_rfc3339();
            tx.execute(sql::PERMISSION_CREATE, params![id, request, step, tool, summary, args.to_string(), stamp, expires])?;
            tx.execute(sql::EVENT, params![request, session, step, "permission_requested",
                json!({"permission_id":id,"tool":tool,"summary":summary,"expires_at":expires}).to_string(), stamp])?;
            tx.commit()?;
            Ok(())
        }).await?;
        Ok(created)
    }

    /// The stored decision, or `None` if the row is gone.
    pub async fn permission_status(&self, id: String) -> Result<Option<String>> {
        self.run(move |c| Ok(c.query_row(sql::PERMISSION_STATUS, [id], |r| r.get::<_, String>(0)).optional()?)).await
    }

    /// Every approval still waiting for a human in this scope, newest last. `args_json` is the
    /// tool's own bounded, redacted payload (a diff preview, a command), so the UI can show what
    /// it is about to allow without asking the model to describe it.
    pub async fn pending_permissions(&self, scope: String) -> Result<Value> {
        self.run(move |c| {
            let mut stmt = c.prepare(sql::PERMISSIONS_PENDING)?;
            let rows = stmt.query_map([scope], |r| Ok(json!({
                "id": r.get::<_, String>(0)?, "request_id": r.get::<_, String>(1)?, "step_id": r.get::<_, String>(2)?,
                "tool": r.get::<_, String>(3)?, "summary": r.get::<_, String>(4)?,
                "args": serde_json::from_str::<Value>(&r.get::<_, String>(5)?).unwrap_or(Value::Null),
                "created_at": r.get::<_, String>(6)?, "expires_at": r.get::<_, String>(7)?,
            })))?.collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(json!({"permissions": rows}))
        }).await
    }

    /// Record a human decision on a pending approval. The `pending` guard in `PERMISSION_RESOLVE`
    /// makes this idempotent: the first decision wins, a repeat of that same decision is accepted
    /// and writes no second event, and the other decision is a conflict rather than a silent flip.
    /// The waiting loop only ever reads `status`, so committing here is what unblocks the turn.
    pub async fn resolve_permission(&self, id: String, scope: String, decision: &'static str) -> Result<Resolution> {
        debug_assert!(matches!(decision, "approved" | "denied"), "only a human approve/deny reaches this");
        self.run(move |c| {
            let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let row = tx.query_row(sql::PERMISSION_GET, [&id], |r| Ok((
                r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, String>(6)?, r.get::<_, String>(10)?,
            ))).optional()?;
            // A wrong scope is a miss, not a leak: nothing tells the caller the row exists.
            let Some((request, step, status, _)) = row.filter(|row| row.3 == scope) else {
                return Ok(Resolution::NotFound);
            };
            match status.as_str() {
                "pending" => {}
                "expired" => return Ok(Resolution::Expired),
                same if same == decision => return Ok(Resolution::Unchanged),
                _ => return Ok(Resolution::Conflict),
            }
            let stamp = now();
            if tx.execute(sql::PERMISSION_RESOLVE, params![id, decision, stamp])? != 1 {
                bail!("approval {id} stopped being pending inside its own transaction");
            }
            let session: String = tx.query_row(sql::SESSION_OF_REQUEST, [&request], |r| r.get(0))?;
            tx.execute(sql::EVENT, params![request, session, step, "permission_resolved",
                json!({"permission_id":id,"decision":decision}).to_string(), stamp])?;
            tx.commit()?;
            Ok(Resolution::Recorded)
        }).await
    }

    /// Expire a still-pending approval when the loop stops waiting. Idempotent: a decision that
    /// arrived first wins, and only a real expiry writes `permission_resolved`.
    pub async fn expire_permission(&self, id: String, request: String, session: String, step: String) -> Result<String> {
        self.run(move |c| {
            let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let stamp = now();
            if tx.execute(sql::PERMISSION_EXPIRE, params![id, stamp])? == 1 {
                tx.execute(sql::EVENT, params![request, session, step, "permission_resolved",
                    json!({"permission_id":id,"decision":"expired"}).to_string(), stamp])?;
            }
            let status: String = tx.query_row(sql::PERMISSION_STATUS, [&id], |r| r.get(0))?;
            tx.commit()?;
            Ok(status)
        }).await
    }
}

// ---- The loop -------------------------------------------------------------------------

/// Everything the loop needs. `messages` is the initial window already stored in the receipt's
/// write-once `context_json`.
pub struct Turn<'a> {
    pub store: &'a DbStore,
    pub agents: &'a MemoryAgents,
    pub request: String,
    pub session: String,
    pub model: String,
    pub scope: ScopeConfig,
    pub messages: Vec<Value>,
}

/// A finished turn. `ProviderFailed` is distinct from an `Err` so the caller can record the
/// honest reason instead of blaming the provider for a storage failure.
pub enum Outcome { Answer(String), ProviderFailed }

struct Ctx<'a> {
    store: &'a DbStore,
    agents: &'a MemoryAgents,
    request: String,
    session: String,
    model: String,
    scope: ScopeConfig,
    registry: Arc<Registry>,
    mode: PermissionMode,
}

/// The initial provider window for a tool-enabled scope: the rendered agent prompt plus the
/// conversation. Recalled memories and the plan are rendered into the system message because
/// they are reviewed context, not another untrusted user turn.
pub fn window(scope: &ScopeConfig, recall: &[Recall], plan: &Value, events: &[Event]) -> Result<Vec<Value>> {
    let memories = if recall.is_empty() { "(none recalled for this turn)".to_string() } else { serde_json::to_string(recall)? };
    let items = plan.get("items").and_then(Value::as_array).cloned().unwrap_or_default();
    let plan_text = if items.is_empty() { "(no plan yet)".to_string() } else {
        items.iter().map(|item| format!("- [{}] {}", item["status"].as_str().unwrap_or("pending"), item["text"].as_str().unwrap_or("")))
            .collect::<Vec<_>>().join("\n")
    };
    let system = AGENT_PROMPT
        .replace("{{root_path}}", scope.root_path.as_deref().unwrap_or("(not configured)"))
        .replace("{{scope}}", &scope.scope)
        .replace("{{recall}}", &memories)
        .replace("{{plan}}", &plan_text);
    let mut messages = vec![json!({"role":"system","content":system})];
    for event in events { messages.push(json!({"role":event.role,"content":event.content})); }
    Ok(messages)
}

pub async fn run(turn: Turn<'_>) -> Result<Outcome> {
    let Turn { store, agents, request, session, model, scope, messages } = turn;
    let ctx = Ctx { store, agents, mode: scope.mode(), registry: Arc::new(Registry::standard()), request, session, model, scope };
    // No `root_path` means no tools: the model is told nothing it cannot use (P1-T04).
    let mut tools = if ctx.scope.root_path.is_some() { ctx.registry.schemas()? } else { Vec::new() };
    let (max_steps, max_tool_bytes, max_wall) = ctx.scope.budgets();
    let started = Instant::now();
    let mut messages = messages;
    let mut steps: i64 = 0;
    let mut tool_bytes: i64 = 0;
    ctx.event("turn_started", json!({"model":ctx.model,"tool_count":tools.len(),
        "permission_mode":ctx.mode.as_str(),"max_steps":max_steps,"max_tool_bytes":max_tool_bytes,"max_wall_seconds":max_wall})).await?;

    loop {
        let elapsed = started.elapsed().as_secs() as i64;
        if let Some(reason) = exhausted(steps, max_steps, tool_bytes, max_tool_bytes, elapsed, max_wall) {
            ctx.event("budget_exhausted", json!({"reason":reason,"steps":steps,"tool_bytes":tool_bytes,"elapsed_seconds":elapsed})).await?;
            return Ok(Outcome::Answer(budget_message(reason, steps, tool_bytes, elapsed)));
        }

        // The full array sent on this call is stored with the step; the receipt keeps only the
        // first window. Tool *names* stand in for the schemas, which are identical every call.
        let step = ctx.store.begin_step(NewStep {
            request: ctx.request.clone(), session: ctx.session.clone(), kind: "model_call",
            tool_name: None, tool_call_id: None,
            input: json!({"messages":messages,"tools":tool_names(&tools)}),
            event: "model_call_started", payload: json!({"attempt":steps+1,"messages":messages.len(),"tool_count":tools.len()}),
        }).await?;

        let replied = ctx.agents.complete_with_tools(&ctx.model, messages.clone(), tools.clone()).await;
        steps += 1;
        let reply = match replied {
            Ok(reply) => reply,
            Err(error) => {
                // A provider that rejects `tools` must not make this release worse than the
                // text-only one it replaces: drop the tools and answer from text alone.
                let unsupported = !tools.is_empty() && memory_agents::is_tools_unsupported(&error);
                let code = if unsupported { "tools_unsupported" } else { "provider_failed" };
                let mut outcome = ctx.outcome(step, "failed", json!({"error":safety::redact(&error.to_string())}), "model_call_finished", json!({"error_code":code}));
                outcome.error_code = Some(code.to_string());
                ctx.store.finish_step(outcome).await?;
                if unsupported { tools.clear(); continue; }
                return Ok(Outcome::ProviderFailed);
            }
        };

        let tokens_in = reply.usage.prompt_tokens.map(|v| v as i64);
        let tokens_out = reply.usage.completion_tokens.map(|v| v as i64);
        let output = json!({"text":reply.text,
            "tool_calls":reply.tool_calls.iter().map(|c| json!({"id":c.id,"name":c.name,"arguments":c.arguments_json})).collect::<Vec<_>>(),
            "usage":reply.usage});
        let mut finished = ctx.outcome(step, "complete", output, "model_call_finished",
            json!({"tokens_in":tokens_in,"tokens_out":tokens_out,"tool_call_count":reply.tool_calls.len()}));
        finished.bytes = finished.output.to_string().len() as i64;
        finished.tokens_in = tokens_in;
        finished.tokens_out = tokens_out;
        ctx.store.finish_step(finished).await?;

        if reply.tool_calls.is_empty() {
            let text = reply.text.map(|t| t.trim().to_string()).filter(|t| !t.is_empty());
            return Ok(Outcome::Answer(text.unwrap_or_else(|| NO_TEXT.to_string())));
        }
        // Replayed verbatim: some providers reject a tool result whose call is missing.
        messages.push(reply.assistant_message.clone());
        let deadline = started + Duration::from_secs(max_wall.max(0) as u64);
        for call in &reply.tool_calls {
            let result = ctx.run_call(call, deadline).await?;
            tool_bytes += result.bytes as i64;
            messages.push(json!({"role":"tool","tool_call_id":call.id,"content":result.content}));
        }
    }
}

impl Ctx<'_> {
    async fn event(&self, kind: &'static str, payload: Value) -> Result<()> {
        self.store.activity(self.request.clone(), self.session.clone(), kind, payload).await
    }
    fn outcome(&self, step: String, status: &'static str, output: Value, event: &'static str, payload: Value) -> StepOutcome {
        StepOutcome { step, request: self.request.clone(), session: self.session.clone(), status, output,
            bytes: 0, truncated: false, tokens_in: None, tokens_out: None, error_code: None, event, payload, artifacts: Vec::new() }
    }

    /// One tool call: commit the step, gate it, run it, commit the result. Every exit path
    /// finishes the step and returns something the model can read.
    async fn run_call(&self, call: &ToolCall, deadline: Instant) -> Result<ToolResult> {
        let parsed = call.arguments();
        let args = parsed.as_ref().ok().filter(|value| value.is_object()).cloned();
        let summary = match (self.registry.get(&call.name), &args) {
            (Some(tool), Some(args)) => tool.summary(args),
            _ => call.name.clone(),
        };
        let step = self.store.begin_step(NewStep {
            request: self.request.clone(), session: self.session.clone(), kind: "tool_call",
            tool_name: Some(call.name.clone()), tool_call_id: Some(call.id.clone()),
            input: args.clone().unwrap_or_else(|| json!({"unparsed_arguments":crate::safety::redact(&call.arguments_json)})),
            event: "tool_started", payload: json!({"tool":call.name,"summary":summary}),
        }).await?;

        // Arguments the adapter could not read never reach a tool.
        let Some(args) = args else {
            let detail = match parsed { Err(error) => error.to_string(), Ok(_) => "tool-call arguments must be a JSON object".to_string() };
            return self.finish_tool(step, call, ToolResult::err("invalid_arguments", detail), None).await;
        };

        let tool_ctx = self.scope.tool_ctx(&self.request, &step);
        let gate = self.registry.get(&call.name).zip(tool_ctx.as_ref())
            .filter(|(tool, _)| self.registry.requires_permission(*tool, &args, self.mode))
            .map(|(tool, ctx)| (tool.summary(&args), tool.permission_payload(ctx, &args)));
        if let Some((prompt, payload)) = gate {
            let ttl = permission_ttl(deadline.saturating_duration_since(Instant::now()).as_secs() as i64);
            let permission = self.store.request_permission(self.request.clone(), self.session.clone(), step.clone(), call.name.clone(), prompt, payload, ttl).await?;
            if let Err(reason) = self.await_permission(&permission, &step, deadline).await? {
                // A denial is a tool error, not a dead turn: the model can adapt or ask.
                let refusal = ToolResult::err("denied", format!("`{}` was not approved: {reason}. Nothing was run and nothing changed on disk.", call.name));
                return self.finish_tool(step, call, refusal, Some("denied")).await;
            }
        }

        let registry = self.registry.clone();
        let name = call.name.clone();
        let result = tokio::task::spawn_blocking(move || registry.invoke(tool_ctx.as_ref(), &name, args)).await?;
        self.finish_tool(step, call, result, None).await
    }

    /// Poll for a human decision. Bounded by the turn's wall budget as well as the row's own
    /// expiry, so a forgotten approval cannot hold a turn open past its budget.
    async fn await_permission(&self, permission: &str, step: &str, deadline: Instant) -> Result<std::result::Result<(), String>> {
        loop {
            match self.store.permission_status(permission.to_string()).await?.as_deref() {
                Some("approved") => return Ok(Ok(())),
                Some("denied") => return Ok(Err("the request was denied".into())),
                Some("expired") => return Ok(Err("the approval request had already expired".into())),
                None => return Ok(Err("the approval request is no longer on record".into())),
                _ => {}
            }
            if Instant::now() >= deadline {
                let status = self.store.expire_permission(permission.to_string(), self.request.clone(), self.session.clone(), step.to_string()).await?;
                return Ok(if status == "approved" { Ok(()) } else { Err("it timed out waiting for an approval".into()) });
            }
            tokio::time::sleep(PERMISSION_POLL).await;
        }
    }

    async fn finish_tool(&self, step: String, call: &ToolCall, result: ToolResult, forced: Option<&'static str>) -> Result<ToolResult> {
        // Invariant 4: every `ToolResult` constructor caps and redacts its own output. `bytes`
        // stays the pre-cap size so the budget counts what the tool actually produced.
        debug_assert!(result.content.len() <= MAX_OUTPUT + 128, "a tool returned uncapped output");
        let status = forced.unwrap_or(match result.status { ToolStatus::Complete => "complete", ToolStatus::Failed => "failed" });
        let mut payload = json!({"tool":call.name,"status":status,"bytes":result.bytes,"truncated":result.truncated,"summary":result.summary});
        if let Some(code) = result.exit_code { payload["exit_code"] = json!(code); }
        let output = json!({"content":result.content,"summary":result.summary,"error_code":result.error_code,"exit_code":result.exit_code});
        let mut outcome = self.outcome(step, status, output, "tool_finished", payload);
        outcome.bytes = result.bytes as i64;
        outcome.truncated = result.truncated;
        outcome.error_code = result.error_code.map(str::to_string);
        outcome.artifacts = result.artifacts.clone();
        self.store.finish_step(outcome).await?;
        Ok(result)
    }
}

fn tool_names(tools: &[Value]) -> Vec<String> {
    tools.iter().filter_map(|t| t["function"]["name"].as_str().map(str::to_string)).collect()
}

fn exhausted(steps: i64, max_steps: i64, bytes: i64, max_bytes: i64, elapsed: i64, max_wall: i64) -> Option<&'static str> {
    if steps >= max_steps { return Some("max_steps"); }
    if bytes >= max_bytes { return Some("max_tool_bytes"); }
    if elapsed >= max_wall { return Some("max_wall_seconds"); }
    None
}

/// How long a new approval row may live: the design TTL, or the turn's remaining wall budget when
/// that is shorter. Clamped at zero so a row is never born already expired.
fn permission_ttl(remaining_wall_seconds: i64) -> i64 {
    remaining_wall_seconds.clamp(0, PERMISSION_TTL_SECONDS)
}

/// The turn's own last message when it runs out of budget. It must not read like success.
fn budget_message(reason: &str, steps: i64, bytes: i64, seconds: i64) -> String {
    format!("I stopped this turn early: it reached its {reason} budget after {steps} model calls, \
{bytes} bytes of tool output and {seconds}s. The task is NOT finished and I am not claiming otherwise. \
Everything that did run is recorded in this turn's steps and file changes; nothing after that point was attempted. \
Send another message to continue from here, or raise the budget for this scope first.")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recording::{self, Admission, CaptureInput, Generation};
    use crate::storage::ScopePatch;
    use std::collections::VecDeque;
    use std::sync::Mutex;

    /// A scripted loopback provider. Each POST records the request and pops the next reply, so
    /// a test can drive the loop through tool calls without a paid provider.
    struct Script { replies: Mutex<VecDeque<(u16, Value)>>, seen: Mutex<Vec<Value>> }

    impl Script {
        fn new(replies: Vec<(u16, Value)>) -> Arc<Self> {
            Arc::new(Self { replies: Mutex::new(replies.into_iter().collect()), seen: Mutex::new(Vec::new()) })
        }
        fn requests(&self) -> Vec<Value> { self.seen.lock().unwrap().clone() }
    }

    fn text(body: &str) -> (u16, Value) { (200, json!({"choices":[{"message":{"role":"assistant","content":body}}]})) }
    fn calls(items: Vec<(&str, &str, &str)>) -> (u16, Value) {
        let tool_calls: Vec<Value> = items.into_iter()
            .map(|(id, name, arguments)| json!({"id":id,"type":"function","function":{"name":name,"arguments":arguments}}))
            .collect();
        (200, json!({"choices":[{"message":{"role":"assistant","content":Value::Null,"tool_calls":tool_calls}}],
            "usage":{"prompt_tokens":11,"completion_tokens":7}}))
    }

    async fn provider(script: Arc<Script>) -> MemoryAgents {
        use axum::{extract::State, response::IntoResponse, routing::post, Json, Router};
        async fn complete(State(script): State<Arc<Script>>, Json(body): Json<Value>) -> axum::response::Response {
            script.seen.lock().unwrap().push(body);
            let next = script.replies.lock().unwrap().pop_front();
            let (status, payload) = next.unwrap_or_else(|| text("the script ran out of replies"));
            (axum::http::StatusCode::from_u16(status).unwrap(), Json(payload)).into_response()
        }
        let app = Router::new().route("/chat/completions", post(complete)).with_state(script);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move { axum::serve(listener, app).await.ok(); });
        MemoryAgents::new(&format!("http://127.0.0.1:{port}"), "test-key", "test-model").unwrap()
    }

    async fn claim(db: &DbStore, prompt: &str) -> Generation {
        let request = uid();
        let admitted = db.capture_chat(CaptureInput { request: request.clone(), session: uid(), scope: "global".into(),
            prompt: prompt.into(), model: "test-model".into(), signature: uid(), redacted: false }).await.unwrap();
        assert!(matches!(admitted, Admission::Saved(_)), "the fixture turn was not admitted");
        db.claim_recording().await.unwrap().expect("a captured turn is claimable")
    }

    /// A throwaway project root with one file, plus the scope row that points at it.
    async fn project(db: &DbStore, mode: &str, patch: ScopePatch) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("harness-loop-{}", uid()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("notes.md"), "alpha\nbeta\n").unwrap();
        let patch = ScopePatch { root_path: Some(Some(dir.to_string_lossy().into())), permission_mode: Some(mode.into()), ..patch };
        db.upsert_scope("global".into(), patch.validate().unwrap()).await.unwrap();
        std::fs::canonicalize(dir).unwrap()
    }

    async fn steps(db: &DbStore, request: &str) -> Vec<Value> {
        let request = request.to_string();
        db.run(move |c| {
            let mut stmt = c.prepare("SELECT seq,kind,status,COALESCE(tool_name,''),COALESCE(error_code,''),COALESCE(input_json,''),COALESCE(output_json,''),output_bytes FROM turn_steps WHERE request_id=?1 ORDER BY seq")?;
            let rows = stmt.query_map([request], |r| Ok(json!({"seq":r.get::<_,i64>(0)?,"kind":r.get::<_,String>(1)?,"status":r.get::<_,String>(2)?,
                "tool":r.get::<_,String>(3)?,"error":r.get::<_,String>(4)?,"input":r.get::<_,String>(5)?,"output":r.get::<_,String>(6)?,"bytes":r.get::<_,i64>(7)?})))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        }).await.unwrap()
    }

    async fn kinds(db: &DbStore, request: &str) -> Vec<String> {
        let request = request.to_string();
        db.run(move |c| {
            let mut stmt = c.prepare("SELECT kind FROM activity_events WHERE request_id=?1 ORDER BY seq")?;
            let rows = stmt.query_map([request], |r| r.get::<_, String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        }).await.unwrap()
    }

    async fn receipt(db: &DbStore, request: &str) -> Value {
        db.recording_receipt(request.to_string()).await.unwrap().unwrap()
    }

    /// The human half of the gate, exactly as `POST /permissions/{id}` performs it: read the one
    /// pending approval for this scope and record the decision. False while none is waiting yet.
    async fn decide(db: &DbStore, decision: &'static str) -> bool {
        let pending = db.pending_permissions("global".into()).await.unwrap();
        let Some(id) = pending["permissions"][0]["id"].as_str().map(str::to_string) else { return false };
        matches!(db.resolve_permission(id, "global".into(), decision).await.unwrap(), Resolution::Recorded)
    }

    async fn permissions(db: &DbStore) -> Vec<Value> {
        db.run(|c| {
            let mut stmt = c.prepare("SELECT tool_name,status,summary FROM permission_requests ORDER BY created_at")?;
            let rows = stmt.query_map([], |r| Ok(json!({"tool":r.get::<_,String>(0)?,"status":r.get::<_,String>(1)?,"summary":r.get::<_,String>(2)?})))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        }).await.unwrap()
    }

    #[tokio::test] async fn a_tool_turn_records_every_step_and_change_before_answering() {
        let db = DbStore::init(":memory:").unwrap();
        let root = project(&db, "auto_all", ScopePatch::default()).await;
        let script = Script::new(vec![
            calls(vec![("call-1", "read", r#"{"path":"notes.md"}"#)]),
            calls(vec![("call-2", "write", r#"{"path":"notes.md","content":"alpha\ngamma\n","overwrite":true}"#)]),
            text("Replaced beta with gamma in notes.md."),
        ]);
        let agents = provider(script.clone()).await;
        let turn = claim(&db, "rename beta to gamma").await;
        let request = turn.request.clone();
        recording::generate(&db, &agents, turn).await.unwrap();

        let saved = receipt(&db, &request).await;
        assert_eq!(saved["state"], "complete");
        assert_eq!(saved["response"], "Replaced beta with gamma in notes.md.");
        assert_eq!(std::fs::read_to_string(root.join("notes.md")).unwrap(), "alpha\ngamma\n", "the approved write must reach the disk");

        let rows = steps(&db, &request).await;
        let shape: Vec<(&str, &str, &str)> = rows.iter()
            .map(|r| (r["kind"].as_str().unwrap(), r["status"].as_str().unwrap(), r["tool"].as_str().unwrap())).collect();
        assert_eq!(shape, vec![("model_call", "complete", ""), ("tool_call", "complete", "read"),
            ("model_call", "complete", ""), ("tool_call", "complete", "write"), ("model_call", "complete", "")], "{rows:#?}");
        assert!(rows[2]["input"].as_str().unwrap().contains(r#""role":"tool""#), "step 2 must store the array that carried the read result");
        assert!(rows[1]["bytes"].as_i64().unwrap() > 0, "a tool step records how much output it produced");

        assert_eq!(kinds(&db, &request).await, vec!["turn_started", "model_call_started", "model_call_finished",
            "tool_started", "tool_finished", "model_call_started", "model_call_finished",
            "tool_started", "tool_finished", "file_changed", "model_call_started", "model_call_finished", "answer_saved"]);

        let changes = db.run(move |c| {
            let mut stmt = c.prepare("SELECT path,action,applied,diff FROM file_changes")?;
            let rows = stmt.query_map([], |r| Ok(json!({"path":r.get::<_,String>(0)?,"action":r.get::<_,String>(1)?,"applied":r.get::<_,i64>(2)?,"diff":r.get::<_,String>(3)?})))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        }).await.unwrap();
        assert_eq!(changes.len(), 1, "{changes:#?}");
        assert_eq!((&changes[0]["path"], &changes[0]["action"], &changes[0]["applied"]), (&json!("notes.md"), &json!("modify"), &json!(1)));
        assert!(changes[0]["diff"].as_str().unwrap().contains("+alpha\n") || changes[0]["diff"].as_str().unwrap().contains("-beta"), "{:?}", changes[0]["diff"]);

        let requests = script.requests();
        assert_eq!(requests.len(), 3);
        assert!(requests[0]["tools"].as_array().unwrap().len() == 8, "every registered tool is offered");
        assert_eq!(requests[0]["tool_choice"], "auto");
        assert!(requests[1]["messages"].as_array().unwrap().iter().any(|m| m["role"] == "tool"), "tool results are fed back to the model");
        let context = db.recording_context(request).await.unwrap().unwrap();
        assert_eq!(context["context"]["provider_messages"], requests[0]["messages"], "context_json holds the FIRST window verbatim");
        assert_eq!(context["context"]["adapter"], "tool_calls_v1");
        std::fs::remove_dir_all(root).ok();
    }

    #[tokio::test] async fn an_exhausted_budget_answers_without_claiming_success() {
        let db = DbStore::init(":memory:").unwrap();
        let root = project(&db, "auto_all", ScopePatch { max_steps: Some(Some(1)), ..Default::default() }).await;
        let script = Script::new(vec![calls(vec![("call-1", "read", r#"{"path":"notes.md"}"#)])]);
        let agents = provider(script.clone()).await;
        let turn = claim(&db, "read everything").await;
        let request = turn.request.clone();
        recording::generate(&db, &agents, turn).await.unwrap();

        let saved = receipt(&db, &request).await;
        assert_eq!(saved["state"], "complete", "a budget stop still owes the user an answer");
        let answer = saved["response"].as_str().unwrap();
        assert!(answer.contains("max_steps") && answer.contains("NOT finished"), "{answer}");
        assert!(kinds(&db, &request).await.contains(&"budget_exhausted".to_string()));
        assert_eq!(script.requests().len(), 1, "the budget must stop the loop before another paid call");
        assert_eq!(steps(&db, &request).await.len(), 2);
        std::fs::remove_dir_all(root).ok();
    }

    #[tokio::test] async fn unreadable_tool_arguments_never_reach_a_tool() {
        let db = DbStore::init(":memory:").unwrap();
        let root = project(&db, "auto_all", ScopePatch::default()).await;
        let script = Script::new(vec![
            calls(vec![("call-1", "write", "{not json"), ("call-2", "read", "[1,2]")]),
            text("I could not use those arguments."),
        ]);
        let agents = provider(script.clone()).await;
        let turn = claim(&db, "break the arguments").await;
        let request = turn.request.clone();
        recording::generate(&db, &agents, turn).await.unwrap();

        let rows = steps(&db, &request).await;
        assert_eq!(rows[1]["status"], "failed");
        assert_eq!(rows[1]["error"], "invalid_arguments");
        assert_eq!(rows[2]["error"], "invalid_arguments", "a JSON array is not a tool argument object");
        assert_eq!(std::fs::read_to_string(root.join("notes.md")).unwrap(), "alpha\nbeta\n", "the file must be untouched");
        let second = &script.requests()[1]["messages"];
        assert!(second.as_array().unwrap().iter().any(|m| m["role"] == "tool" && m["content"].as_str().unwrap().contains("invalid_arguments")),
            "the model has to see why its call was refused: {second}");
        std::fs::remove_dir_all(root).ok();
    }

    #[tokio::test] async fn a_denied_tool_changes_nothing_and_is_reported_to_the_model() {
        let db = DbStore::init(":memory:").unwrap();
        let root = project(&db, "ask", ScopePatch::default()).await;
        let script = Script::new(vec![
            calls(vec![("call-1", "write", r#"{"path":"notes.md","content":"wiped\n","overwrite":true}"#)]),
            text("Understood, I left the file alone."),
        ]);
        let agents = provider(script.clone()).await;
        let turn = claim(&db, "overwrite my notes").await;
        let request = turn.request.clone();

        // The same storage call `POST /permissions/{id}` makes (P1-T11): whatever writes the
        // decision, the loop must observe it and refuse to run the tool.
        let denier = db.clone();
        tokio::spawn(async move {
            for _ in 0..400 {
                if decide(&denier, "denied").await { return; }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        });
        recording::generate(&db, &agents, turn).await.unwrap();

        assert_eq!(std::fs::read_to_string(root.join("notes.md")).unwrap(), "alpha\nbeta\n", "a denied write must never reach the disk");
        let rows = steps(&db, &request).await;
        assert_eq!((&rows[1]["status"], &rows[1]["error"]), (&json!("denied"), &json!("denied")), "{rows:#?}");
        let events = kinds(&db, &request).await;
        assert!(events.contains(&"permission_requested".to_string()), "{events:?}");
        assert_eq!(receipt(&db, &request).await["response"], "Understood, I left the file alone.");
        let second = &script.requests()[1]["messages"];
        assert!(second.as_array().unwrap().iter().any(|m| m["role"] == "tool" && m["content"].as_str().unwrap().contains("denied")), "{second}");
        assert!(db.run(|c| Ok(c.query_row("SELECT count(*) FROM file_changes", [], |r| r.get::<_, i64>(0))?)).await.unwrap() == 0);
        std::fs::remove_dir_all(root).ok();
    }

    #[tokio::test] async fn a_provider_without_tool_support_falls_back_to_text() {
        let db = DbStore::init(":memory:").unwrap();
        let root = project(&db, "auto_all", ScopePatch::default()).await;
        let script = Script::new(vec![
            (400, json!({"error":{"message":"tools are not supported by this model"}})),
            text("Answered from text alone."),
        ]);
        let agents = provider(script.clone()).await;
        let turn = claim(&db, "hello").await;
        let request = turn.request.clone();
        recording::generate(&db, &agents, turn).await.unwrap();

        let rows = steps(&db, &request).await;
        assert_eq!((&rows[0]["status"], &rows[0]["error"]), (&json!("failed"), &json!("tools_unsupported")), "{rows:#?}");
        assert_eq!(rows[1]["status"], "complete");
        assert_eq!(receipt(&db, &request).await["response"], "Answered from text alone.");
        let requests = script.requests();
        assert!(requests[0]["tools"].is_array() && requests[1]["tools"].is_null(), "the retry must drop the tools it was rejected for");
        std::fs::remove_dir_all(root).ok();
    }

    /// P1-T11: the one path P1-T10 could not reach. An approval has to actually let the tool run.
    #[tokio::test] async fn approving_permissions_unblocks_a_waiting_turn() {
        let db = DbStore::init(":memory:").unwrap();
        let root = project(&db, "ask", ScopePatch::default()).await;
        let script = Script::new(vec![
            calls(vec![("call-1", "write", r#"{"path":"notes.md","content":"approved\n","overwrite":true}"#)]),
            text("Wrote notes.md after you approved it."),
        ]);
        let agents = provider(script.clone()).await;
        let turn = claim(&db, "overwrite my notes").await;
        let request = turn.request.clone();

        let approver = db.clone();
        tokio::spawn(async move {
            for _ in 0..400 {
                if decide(&approver, "approved").await { return; }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        });
        recording::generate(&db, &agents, turn).await.unwrap();

        assert_eq!(std::fs::read_to_string(root.join("notes.md")).unwrap(), "approved\n", "an approved write must reach the disk");
        let rows = steps(&db, &request).await;
        assert_eq!((&rows[1]["kind"], &rows[1]["status"], &rows[1]["error"]), (&json!("tool_call"), &json!("complete"), &json!("")), "{rows:#?}");
        let events = kinds(&db, &request).await;
        assert_eq!(events.iter().filter(|k| k.as_str() == "permission_resolved").count(), 1, "{events:?}");
        assert!(events.contains(&"permission_requested".to_string()) && events.contains(&"file_changed".to_string()), "{events:?}");
        assert_eq!(receipt(&db, &request).await["response"], "Wrote notes.md after you approved it.");
        let rows = permissions(&db).await;
        assert_eq!((rows.len(), &rows[0]["tool"], &rows[0]["status"]), (1, &json!("write"), &json!("approved")), "{rows:#?}");
        assert!(rows[0]["summary"].as_str().unwrap().contains("notes.md"), "the card the human saw names the file: {:?}", rows[0]["summary"]);
        assert!(db.pending_permissions("global".into()).await.unwrap()["permissions"].as_array().unwrap().is_empty());
        std::fs::remove_dir_all(root).ok();
    }

    /// `auto_edit` is the whole point of having modes: writes stop asking, `bash` does not.
    #[tokio::test] async fn auto_edit_permissions_pass_a_write_and_still_stop_bash() {
        let db = DbStore::init(":memory:").unwrap();
        let root = project(&db, "auto_edit", ScopePatch::default()).await;
        let script = Script::new(vec![
            calls(vec![("call-1", "write", r#"{"path":"notes.md","content":"edited\n","overwrite":true}"#),
                       ("call-2", "bash", r#"{"command":"echo hi","description":"say hi"}"#)]),
            text("I wrote the file; the command was refused."),
        ]);
        let agents = provider(script.clone()).await;
        let turn = claim(&db, "write it then run something").await;
        let request = turn.request.clone();

        let denier = db.clone();
        tokio::spawn(async move {
            for _ in 0..400 {
                if decide(&denier, "denied").await { return; }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        });
        recording::generate(&db, &agents, turn).await.unwrap();

        assert_eq!(std::fs::read_to_string(root.join("notes.md")).unwrap(), "edited\n", "auto_edit approves an edit without a human");
        let rows = permissions(&db).await;
        assert_eq!((rows.len(), &rows[0]["tool"], &rows[0]["status"]), (1, &json!("bash"), &json!("denied")), "only bash may ask in auto_edit: {rows:#?}");
        let steps = steps(&db, &request).await;
        let shape: Vec<(&str, &str)> = steps.iter().map(|r| (r["tool"].as_str().unwrap(), r["status"].as_str().unwrap())).collect();
        assert_eq!(shape, vec![("", "complete"), ("write", "complete"), ("bash", "denied"), ("", "complete")], "{steps:#?}");
        std::fs::remove_dir_all(root).ok();
    }

    /// `auto_all` still stops at the bash deny-list from docs/design/tools.md#bash.
    #[tokio::test] async fn auto_all_permissions_still_ask_before_a_deny_listed_command() {
        let db = DbStore::init(":memory:").unwrap();
        let root = project(&db, "auto_all", ScopePatch::default()).await;
        let script = Script::new(vec![
            calls(vec![("call-1", "bash", r#"{"command":"git push --force origin main","description":"force push"}"#)]),
            text("I did not force-push."),
        ]);
        let agents = provider(script.clone()).await;
        let turn = claim(&db, "force push it").await;
        let request = turn.request.clone();

        let denier = db.clone();
        tokio::spawn(async move {
            for _ in 0..400 {
                if decide(&denier, "denied").await { return; }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        });
        recording::generate(&db, &agents, turn).await.unwrap();

        let rows = permissions(&db).await;
        assert_eq!((rows.len(), &rows[0]["status"]), (1, &json!("denied")), "a deny-listed command asks even in auto_all: {rows:#?}");
        let steps = steps(&db, &request).await;
        assert_eq!((&steps[1]["tool"], &steps[1]["status"], &steps[1]["error"]), (&json!("bash"), &json!("denied"), &json!("denied")), "{steps:#?}");
        let second = &script.requests()[1]["messages"];
        assert!(second.as_array().unwrap().iter().any(|m| m["role"] == "tool" && m["content"].as_str().unwrap().contains("denied")), "{second}");
        std::fs::remove_dir_all(root).ok();
    }

    /// The T10 question T11 had to answer: a row may not outlive the turn that is waiting on it.
    #[tokio::test] async fn permissions_expire_with_the_turn_not_thirty_minutes_later() {
        assert_eq!(permission_ttl(15 * 60), 15 * 60, "a shorter wall budget wins");
        assert_eq!(permission_ttl(45 * 60), PERMISSION_TTL_SECONDS, "the design TTL caps a generous budget");
        assert_eq!(permission_ttl(-3), 0, "a turn already out of time cannot open a live approval");
    }

    #[tokio::test] async fn resolving_permissions_is_idempotent_and_refuses_a_flip() {
        let db = DbStore::init(":memory:").unwrap();
        let turn = claim(&db, "needs approval").await;
        let step = db.begin_step(NewStep { request: turn.request.clone(), session: turn.session.clone(), kind: "tool_call",
            tool_name: Some("write".into()), tool_call_id: Some("call-1".into()), input: json!({"path":"a.md"}),
            event: "tool_started", payload: json!({"tool":"write","summary":"write a.md"}) }).await.unwrap();
        let id = db.request_permission(turn.request.clone(), turn.session.clone(), step, "write".into(), "write a.md".into(), json!({"diff":"+x"}), 900).await.unwrap();

        assert!(matches!(db.resolve_permission(id.clone(), "global".into(), "approved").await.unwrap(), Resolution::Recorded));
        assert!(matches!(db.resolve_permission(id.clone(), "global".into(), "approved").await.unwrap(), Resolution::Unchanged), "a replayed click is not an error");
        assert!(matches!(db.resolve_permission(id.clone(), "global".into(), "denied").await.unwrap(), Resolution::Conflict), "a resolved approval cannot be flipped");
        assert!(matches!(db.resolve_permission(id.clone(), "other".into(), "denied").await.unwrap(), Resolution::NotFound), "another scope cannot see this row");
        assert!(matches!(db.resolve_permission(uid(), "global".into(), "denied").await.unwrap(), Resolution::NotFound));
        let events = kinds(&db, &turn.request).await;
        assert_eq!(events.iter().filter(|k| k.as_str() == "permission_resolved").count(), 1, "one decision, one event: {events:?}");
        assert_eq!(permissions(&db).await[0]["status"], "approved");
    }

    #[tokio::test] async fn expired_permissions_cannot_be_approved_afterwards() {
        let db = DbStore::init(":memory:").unwrap();
        let turn = claim(&db, "needs approval").await;
        let step = db.begin_step(NewStep { request: turn.request.clone(), session: turn.session.clone(), kind: "tool_call",
            tool_name: Some("bash".into()), tool_call_id: Some("call-1".into()), input: json!({"command":"echo hi"}),
            event: "tool_started", payload: json!({"tool":"bash","summary":"bash echo hi"}) }).await.unwrap();
        let id = db.request_permission(turn.request.clone(), turn.session.clone(), step.clone(), "bash".into(), "bash echo hi".into(), json!({"command":"echo hi"}), 0).await.unwrap();
        // The loop gave up first, exactly as it does when the wall budget runs out.
        assert_eq!(db.expire_permission(id.clone(), turn.request.clone(), turn.session.clone(), step).await.unwrap(), "expired");
        assert!(matches!(db.resolve_permission(id, "global".into(), "approved").await.unwrap(), Resolution::Expired), "an approval that arrives too late must not run anything");
    }

    #[tokio::test] async fn a_provider_failure_fails_the_turn_without_an_invented_answer() {
        let db = DbStore::init(":memory:").unwrap();
        let agents = provider(Script::new(vec![(503, json!({"error":"upstream is down"}))])).await;
        let turn = claim(&db, "anything").await;
        let request = turn.request.clone();
        recording::generate(&db, &agents, turn).await.unwrap();

        let saved = receipt(&db, &request).await;
        assert_eq!((&saved["state"], &saved["error_code"], &saved["response"]), (&json!("failed"), &json!("provider_failed"), &json!(null)));
        let rows = steps(&db, &request).await;
        assert_eq!((&rows[0]["kind"], &rows[0]["status"], &rows[0]["error"]), (&json!("model_call"), &json!("failed"), &json!("provider_failed")));
        assert_eq!(kinds(&db, &request).await.last().unwrap(), "turn_failed");
    }

    #[tokio::test] async fn a_restart_interrupts_running_work_and_reruns_nothing() {
        let db = DbStore::init(":memory:").unwrap();
        let turn = claim(&db, "hold for restart").await;
        let request = turn.request.clone();
        let step = db.begin_step(NewStep { request: request.clone(), session: turn.session.clone(), kind: "tool_call",
            tool_name: Some("bash".into()), tool_call_id: Some("call-1".into()), input: json!({"command":"sleep 60"}),
            event: "tool_started", payload: json!({"tool":"bash","summary":"bash sleep 60"}) }).await.unwrap();
        db.request_permission(request.clone(), turn.session.clone(), step.clone(), "bash".into(), "bash sleep 60".into(), json!({"command":"sleep 60"}), 900).await.unwrap();

        // The process dies here. Startup recovery is the only thing that gets to speak next.
        db.run(|c| crate::recording::recover(c)).await.unwrap();

        let rows = steps(&db, &request).await;
        assert_eq!(rows[0]["status"], "interrupted", "a step that was running when the process died is interrupted, not failed");
        assert_eq!(receipt(&db, &request).await["state"], "interrupted");
        let pending: i64 = db.run(|c| Ok(c.query_row("SELECT count(*) FROM permission_requests WHERE status='pending'", [], |r| r.get(0))?)).await.unwrap();
        assert_eq!(pending, 0, "a pending approval cannot survive the process that was waiting on it");
        assert!(kinds(&db, &request).await.contains(&"interrupted".to_string()));
        assert!(db.claim_recording().await.unwrap().is_none(), "an interrupted turn must never be claimed again");
    }
}
