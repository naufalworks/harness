use super::*;
use crate::patch::Patch;
use crate::recording::{self, Admission, CaptureInput, Generation};
use crate::storage::{uid, ScopePatch};
use std::collections::VecDeque;
use std::sync::Mutex;

/// A scripted loopback provider. Ordinary calls consume scripted replies; verification calls
/// are recorded separately and get an evidence-bound report without disturbing that script.
struct Script {
    replies: Mutex<VecDeque<(u16, Value)>>,
    verification_replies: Mutex<VecDeque<(u16, Value)>>,
    seen: Mutex<Vec<Value>>,
    verification_seen: Mutex<Vec<Value>>,
}

impl Script {
    fn new(replies: Vec<(u16, Value)>) -> Arc<Self> {
        Arc::new(Self {
            replies: Mutex::new(replies.into_iter().collect()),
            verification_replies: Mutex::new(VecDeque::new()),
            seen: Mutex::new(Vec::new()),
            verification_seen: Mutex::new(Vec::new()),
        })
    }
    fn requests(&self) -> Vec<Value> {
        self.seen.lock().unwrap().clone()
    }
    fn verification_requests(&self) -> Vec<Value> {
        self.verification_seen.lock().unwrap().clone()
    }
    fn push_verification_reply(&self, reply: (u16, Value)) {
        self.verification_replies.lock().unwrap().push_back(reply);
    }
}

fn text(body: &str) -> (u16, Value) {
    (
        200,
        json!({"choices":[{"message":{"role":"assistant","content":body}}]}),
    )
}
fn calls_with_prompt(items: Vec<(&str, &str, &str)>, prompt_tokens: u64) -> (u16, Value) {
    let tool_calls: Vec<Value> = items.into_iter()
            .map(|(id, name, arguments)| json!({"id":id,"type":"function","function":{"name":name,"arguments":arguments}}))
            .collect();
    (
        200,
        json!({"choices":[{"message":{"role":"assistant","content":Value::Null,"tool_calls":tool_calls}}],
            "usage":{"prompt_tokens":prompt_tokens,"completion_tokens":7}}),
    )
}
fn calls(items: Vec<(&str, &str, &str)>) -> (u16, Value) {
    calls_with_prompt(items, 11)
}

fn verification_request(body: &Value) -> bool {
    body["messages"].as_array().is_some_and(|messages| {
        messages.iter().any(|message| {
            message["content"]
                .as_str()
                .is_some_and(|content| content.contains(memory_agents::VERIFICATION_MARKER))
        })
    })
}

fn default_verification(body: &Value) -> (u16, Value) {
    let input = body["messages"]
        .as_array()
        .and_then(|messages| messages.last())
        .and_then(|message| message["content"].as_str())
        .and_then(|content| serde_json::from_str::<Value>(content).ok())
        .unwrap_or(Value::Null);
    let step_id = input["evidence_manifest"]["steps"]
        .as_array()
        .and_then(|steps| steps.first())
        .and_then(|step| step["step_id"].as_str());
    let claims=step_id.map(|id|vec![json!({"claim":"The answer has recorded tool evidence.","status":"verified",
            "evidence_step_ids":[id],"reason":"The cited current-turn tool step is present in the manifest."})]).unwrap_or_default();
    text(&json!({"claims":claims,"skipped_diagnostics":[]}).to_string())
}

async fn provider(script: Arc<Script>) -> MemoryAgents {
    use axum::{extract::State, response::IntoResponse, routing::post, Json, Router};
    async fn complete(
        State(script): State<Arc<Script>>,
        Json(body): Json<Value>,
    ) -> axum::response::Response {
        if verification_request(&body) {
            script.verification_seen.lock().unwrap().push(body.clone());
            let next = script
                .verification_replies
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| default_verification(&body));
            return (
                axum::http::StatusCode::from_u16(next.0).unwrap(),
                Json(next.1),
            )
                .into_response();
        }
        script.seen.lock().unwrap().push(body);
        let next = script.replies.lock().unwrap().pop_front();
        let (status, payload) = next.unwrap_or_else(|| text("the script ran out of replies"));
        (
            axum::http::StatusCode::from_u16(status).unwrap(),
            Json(payload),
        )
            .into_response()
    }
    let app = Router::new()
        .route("/chat/completions", post(complete))
        .with_state(script);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });
    MemoryAgents::new(
        &format!("http://127.0.0.1:{port}"),
        "test-key",
        "test-model",
    )
    .unwrap()
}

/// P14-REVIEW-01: a provider that answers the parent call immediately but holds the first
/// delegated call open until the test releases it, so a cancel can be requested while a
/// sub-agent provider wait is genuinely in flight. Every request is counted, so a regression can
/// assert no call is made after the cancel.
struct Held {
    calls: Mutex<usize>,
    arrived: tokio::sync::Semaphore,
    release: tokio::sync::Semaphore,
}

async fn held_provider() -> (MemoryAgents, Arc<Held>) {
    use axum::{extract::State, response::IntoResponse, routing::post, Json, Router};
    async fn complete(
        State(held): State<Arc<Held>>,
        Json(_body): Json<Value>,
    ) -> axum::response::Response {
        let call = {
            let mut calls = held.calls.lock().unwrap();
            *calls += 1;
            *calls
        };
        if call == 1 {
            // The parent turn delegates one read-only exploration.
            return Json(json!({"choices":[{"message":{"role":"assistant","content":Value::Null,
                    "tool_calls":[{"id":"call-1","type":"function","function":{"name":"task",
                    "arguments":"{\"description\":\"find beta\",\"prompt\":\"say which line of notes.md holds beta\"}"}}]}}]}))
                    .into_response();
        }
        if call == 2 {
            // The sub-agent's provider call: announce arrival and hold until released.
            held.arrived.add_permits(1);
            held.release.acquire().await.unwrap().forget();
        }
        Json(json!({"choices":[{"message":{"role":"assistant","content":"beta is on line 2 of notes.md."}}]}))
                .into_response()
    }
    let held = Arc::new(Held {
        calls: Mutex::new(0),
        arrived: tokio::sync::Semaphore::new(0),
        release: tokio::sync::Semaphore::new(0),
    });
    let app = Router::new()
        .route("/chat/completions", post(complete))
        .with_state(held.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });
    let agents = MemoryAgents::new(
        &format!("http://127.0.0.1:{port}"),
        "test-key",
        "test-model",
    )
    .unwrap();
    (agents, held)
}

async fn claim(db: &DbStore, prompt: &str) -> Generation {
    let request = uid();
    let admitted = db
        .capture_chat(CaptureInput {
            request: request.clone(),
            session: uid(),
            scope: "global".into(),
            prompt: prompt.into(),
            model: "test-model".into(),
            signature: uid(),
            redacted: false,
        })
        .await
        .unwrap();
    assert!(
        matches!(admitted, Admission::Saved(_)),
        "the fixture turn was not admitted"
    );
    db.claim_recording()
        .await
        .unwrap()
        .expect("a captured turn is claimable")
}

/// A throwaway project root with one file, plus the scope row that points at it.
async fn project(db: &DbStore, mode: &str, patch: ScopePatch) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("harness-loop-{}", uid()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("notes.md"), "alpha\nbeta\n").unwrap();
    let patch = ScopePatch {
        root_path: Patch::Set(dir.to_string_lossy().into()),
        permission_mode: Some(mode.into()),
        ..patch
    };
    db.upsert_scope("global".into(), patch.validate().unwrap())
        .await
        .unwrap();
    std::fs::canonicalize(dir).unwrap()
}

/// P1-T15: a scope with no project root must say so in the system prompt. The old prompt
/// promised tools unconditionally, so the model reported "no terminal in this conversation"
/// instead of the actual cause the user could fix.
#[test]
fn the_prompt_names_the_missing_project_root_instead_of_promising_tools() {
    let text = crate::context::system_rules(&ScopeConfig::blank("global"));
    assert!(text.contains("You have NO tools"), "{text}");
    assert!(
        text.contains("scope `global` has no project root configured"),
        "{text}"
    );
    assert!(text.contains("Project scope settings"), "{text}");
    let configured = ScopeConfig {
        root_path: Some("/tmp".into()),
        ..ScopeConfig::blank("global")
    };
    let ready_text = crate::context::system_rules(&configured);
    assert!(ready_text.contains("You have tools."), "{ready_text}");
    assert!(!ready_text.contains("NO tools"), "{ready_text}");
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
        let mut stmt =
            c.prepare("SELECT kind FROM activity_events WHERE request_id=?1 ORDER BY seq")?;
        let rows = stmt
            .query_map([request], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    })
    .await
    .unwrap()
}

async fn receipt(db: &DbStore, request: &str) -> Value {
    db.recording_receipt(request.to_string())
        .await
        .unwrap()
        .unwrap()
}

/// The human half of the gate, exactly as `POST /permissions/{id}` performs it: read the one
/// pending approval for this scope and record the decision. False while none is waiting yet.
async fn decide(db: &DbStore, decision: &'static str) -> bool {
    let pending = db.pending_permissions("global".into()).await.unwrap();
    let Some(id) = pending["permissions"][0]["id"].as_str().map(str::to_string) else {
        return false;
    };
    matches!(
        db.resolve_permission(id, "global".into(), decision)
            .await
            .unwrap(),
        Resolution::Recorded
    )
}

async fn permissions(db: &DbStore) -> Vec<Value> {
    db.run(|c| {
            let mut stmt = c.prepare("SELECT tool_name,status,summary FROM permission_requests ORDER BY created_at")?;
            let rows = stmt.query_map([], |r| Ok(json!({"tool":r.get::<_,String>(0)?,"status":r.get::<_,String>(1)?,"summary":r.get::<_,String>(2)?})))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        }).await.unwrap()
}

#[test]
fn only_bash_and_browser_input_enter_the_tool_effect_ledger() {
    let registry = Registry::standard();
    assert_eq!(
        tool_effect_kind(&registry, "bash", &json!({"command":"printf ok"})),
        Some("bash_command")
    );
    for operation in ["open", "snapshot", "screenshot", "close"] {
        assert_eq!(
            tool_effect_kind(&registry, "browser", &json!({"operation":operation})),
            None,
            "browser {operation} is not remote input"
        );
    }
    for operation in ["click", "type", "press"] {
        assert_eq!(
            tool_effect_kind(&registry, "browser", &json!({"operation":operation})),
            Some("browser_input"),
            "browser {operation} can change remote state"
        );
    }
    assert_eq!(
        tool_effect_kind(&registry, "write", &json!({"path":"notes.md"})),
        None,
        "filesystem writes need truthful local receipts, not the external-effect ledger"
    );
}

#[tokio::test]
async fn a_bash_command_is_reserved_and_settled_once() {
    let db = DbStore::init(":memory:").unwrap();
    let root = project(&db, "auto_all", ScopePatch::default()).await;
    let script = Script::new(vec![
        calls(vec![(
            "call-1",
            "bash",
            r#"{"command":"printf ok","description":"print ok"}"#,
        )]),
        text("The command printed ok."),
    ]);
    let agents = provider(script).await;
    let turn = claim(&db, "print ok").await;
    let request = turn.request.clone();
    recording::generate(&db, &agents, turn).await.unwrap();

    let effect: (i64, String, String, i64) = db
            .read(move |c| {
                Ok(c.query_row(
                    "SELECT count(*),state,kind,fence FROM external_effects WHERE request_id=?1 AND kind='bash_command'",
                    [&request],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                )?)
            })
            .await
            .unwrap();
    assert_eq!(effect, (1, "succeeded".into(), "bash_command".into(), 1));
    assert!(permissions(&db).await.is_empty());
    std::fs::remove_dir_all(root).ok();
}

#[tokio::test]
async fn turn_compaction_records_a_receipt_keeps_two_tools_and_proposes_episode() {
    let db = DbStore::init(":memory:").unwrap();
    let root = project(&db, "auto_all", ScopePatch::default()).await;
    let mut settings = std::collections::BTreeMap::new();
    settings.insert("compaction".to_string(), "cheap-model".to_string());
    db.set_settings(settings).await.unwrap();
    let script = Script::new(vec![
        calls_with_prompt(
            vec![("call-1", "think", r#"{"thought":"old result"}"#)],
            2_000_000,
        ),
        calls_with_prompt(
            vec![("call-2", "think", r#"{"thought":"keep result two"}"#)],
            2_000_000,
        ),
        calls_with_prompt(
            vec![("call-3", "think", r#"{"thought":"keep result three"}"#)],
            2_000_000,
        ),
        text("Older work completed; continue with the two retained tool results."),
        text("Finished after compaction."),
    ]);
    let agents = provider(script.clone()).await;
    let turn = claim(&db, "compact this long turn").await;
    let request = turn.request.clone();
    recording::generate(&db, &agents, turn).await.unwrap();

    let requests = script.requests();
    assert_eq!(requests.len(), 5);
    assert_eq!(requests[3]["model"], "cheap-model");
    assert!(requests[3].get("tools").is_none());
    assert!(requests[3]["messages"][0]["content"]
        .as_str()
        .unwrap()
        .contains("untrusted quoted data"));
    let final_messages = requests[4]["messages"].as_array().unwrap();
    let joined = serde_json::to_string(final_messages).unwrap();
    assert!(
        joined.contains("## Compacted history") && joined.contains("Older work completed"),
        "{joined}"
    );
    assert!(
        joined.contains("keep result two") && joined.contains("keep result three"),
        "{joined}"
    );
    assert!(!joined.contains("old result"), "{joined}");

    let rows = steps(&db, &request).await;
    let compact = rows
        .iter()
        .find(|r| r["kind"] == "compaction")
        .expect("durable compaction step");
    let input: Value = serde_json::from_str(compact["input"].as_str().unwrap()).unwrap();
    let output: Value = serde_json::from_str(compact["output"].as_str().unwrap()).unwrap();
    assert_eq!(input["receipt"]["trigger_percent"], 70);
    assert_eq!(
        input["receipt"]["context_token_budget"],
        context_token_budget()
    );
    assert_eq!(output["receipt"]["preserved_tool_steps"], json!([4, 6]));
    assert_eq!(
        output["summary"],
        "Older work completed; continue with the two retained tool results."
    );
    let candidates = db.candidates("global".into()).await.unwrap();
    assert!(candidates["candidates"]
        .as_array()
        .unwrap()
        .iter()
        .any(|c| c["category"] == "episodic" && c["evidence"]["request_id"] == request));
    let saved = db
        .recording_context(request.clone())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        saved["context"]["provider_messages"], requests[0]["messages"],
        "the first-call receipt stays immutable after turn compaction"
    );
    std::fs::remove_dir_all(root).ok();
}

#[tokio::test]
async fn repeated_unchanged_reads_reference_the_first_step_but_keep_full_audit_output() {
    let db = DbStore::init(":memory:").unwrap();
    let root = project(&db, "auto_all", ScopePatch::default()).await;
    let script = Script::new(vec![
        calls(vec![("call-1", "read", r#"{"path":"notes.md"}"#)]),
        calls(vec![("call-2", "read", r#"{"path":"notes.md"}"#)]),
        text("Read it twice."),
    ]);
    let agents = provider(script.clone()).await;
    let turn = claim(&db, "read it twice").await;
    let request = turn.request.clone();
    recording::generate(&db, &agents, turn).await.unwrap();

    let requests = script.requests();
    let second_read = requests[2]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["tool_call_id"] == "call-2")
        .unwrap()["content"]
        .as_str()
        .unwrap();
    assert!(
        second_read.starts_with("[tool read step 2, ")
            && second_read.ends_with("; call read again if needed]"),
        "{second_read}"
    );

    let rows = steps(&db, &request).await;
    assert!(
        rows[3]["output"].as_str().unwrap().contains("alpha"),
        "the duplicate tool step remains a full audit record: {rows:#?}"
    );
    std::fs::remove_dir_all(root).ok();
}

#[tokio::test]
async fn changed_reads_are_not_replaced_by_the_turn_cache() {
    let db = DbStore::init(":memory:").unwrap();
    let root = project(&db, "auto_all", ScopePatch::default()).await;
    let script = Script::new(vec![
        calls(vec![("call-1", "read", r#"{"path":"notes.md"}"#)]),
        calls(vec![(
            "call-2",
            "write",
            r#"{"path":"notes.md","content":"changed\n","overwrite":true}"#,
        )]),
        calls(vec![("call-3", "read", r#"{"path":"notes.md"}"#)]),
        text("Read the change."),
    ]);
    let agents = provider(script.clone()).await;
    let turn = claim(&db, "change then reread").await;
    recording::generate(&db, &agents, turn).await.unwrap();

    let requests = script.requests();
    let changed = requests[3]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["tool_call_id"] == "call-3")
        .unwrap()["content"]
        .as_str()
        .unwrap();
    assert!(
        changed.contains("changed") && !changed.starts_with("[tool read step"),
        "{changed}"
    );
    std::fs::remove_dir_all(root).ok();
}

#[tokio::test]
async fn a_tool_turn_records_every_step_and_change_before_answering() {
    let db = DbStore::init(":memory:").unwrap();
    let root = project(&db, "auto_all", ScopePatch::default()).await;
    let script = Script::new(vec![
        calls(vec![("call-1", "read", r#"{"path":"notes.md"}"#)]),
        calls(vec![(
            "call-2",
            "write",
            r#"{"path":"notes.md","content":"alpha\ngamma\n","overwrite":true}"#,
        )]),
        text("Replaced beta with gamma in notes.md."),
    ]);
    let agents = provider(script.clone()).await;
    let turn = claim(&db, "rename beta to gamma").await;
    let request = turn.request.clone();
    recording::generate(&db, &agents, turn).await.unwrap();

    let saved = receipt(&db, &request).await;
    assert_eq!(saved["state"], "complete");
    assert_eq!(saved["response"], "Replaced beta with gamma in notes.md.");
    assert_eq!(
        std::fs::read_to_string(root.join("notes.md")).unwrap(),
        "alpha\ngamma\n",
        "the approved write must reach the disk"
    );

    let rows = steps(&db, &request).await;
    let shape: Vec<(&str, &str, &str)> = rows
        .iter()
        .map(|r| {
            (
                r["kind"].as_str().unwrap(),
                r["status"].as_str().unwrap(),
                r["tool"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        shape,
        vec![
            ("model_call", "complete", ""),
            ("tool_call", "complete", "read"),
            ("model_call", "complete", ""),
            ("tool_call", "complete", "write"),
            ("model_call", "complete", ""),
            ("verification", "complete", "")
        ],
        "{rows:#?}"
    );
    assert!(
        rows[2]["input"]
            .as_str()
            .unwrap()
            .contains(r#""role":"tool""#),
        "step 2 must store the array that carried the read result"
    );
    assert!(
        rows[1]["bytes"].as_i64().unwrap() > 0,
        "a tool step records how much output it produced"
    );

    assert_eq!(
        kinds(&db, &request).await,
        vec![
            "turn_started",
            "model_call_started",
            "model_call_finished",
            "tool_started",
            "tool_finished",
            "model_call_started",
            "model_call_finished",
            "tool_started",
            "tool_finished",
            "file_changed",
            "model_call_started",
            "model_call_finished",
            "verification_started",
            "verified",
            "answer_saved"
        ]
    );

    let changes = db.run(move |c| {
            let mut stmt = c.prepare("SELECT path,action,applied,diff FROM file_changes")?;
            let rows = stmt.query_map([], |r| Ok(json!({"path":r.get::<_,String>(0)?,"action":r.get::<_,String>(1)?,"applied":r.get::<_,i64>(2)?,"diff":r.get::<_,String>(3)?})))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        }).await.unwrap();
    assert_eq!(changes.len(), 1, "{changes:#?}");
    assert_eq!(
        (
            &changes[0]["path"],
            &changes[0]["action"],
            &changes[0]["applied"]
        ),
        (&json!("notes.md"), &json!("modify"), &json!(1))
    );
    assert!(
        changes[0]["diff"].as_str().unwrap().contains("+alpha\n")
            || changes[0]["diff"].as_str().unwrap().contains("-beta"),
        "{:?}",
        changes[0]["diff"]
    );

    let requests = script.requests();
    assert_eq!(requests.len(), 3);
    assert_eq!(
        requests[0]["tools"].as_array().unwrap().len(),
        crate::tools::Registry::standard()
            .schemas()
            .expect("every schema file parses")
            .len(),
        "every registered tool is offered"
    );
    assert_eq!(requests[0]["tool_choice"], "auto");
    assert!(
        requests[1]["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["role"] == "tool"),
        "tool results are fed back to the model"
    );
    let verification_requests = script.verification_requests();
    assert_eq!(verification_requests.len(), 1);
    assert!(
        verification_requests[0].get("tools").is_none(),
        "the verifier is text-only"
    );
    assert!(verification_requests[0]["messages"][0]["content"]
        .as_str()
        .unwrap()
        .contains(memory_agents::VERIFICATION_MARKER));
    let verification_output: Value =
        serde_json::from_str(rows.last().unwrap()["output"].as_str().unwrap()).unwrap();
    assert_eq!(verification_output["status"], "verified");
    assert_eq!(
        verification_output["claims"][0]["evidence_step_ids"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert!(
        !rows.last().unwrap()["input"]
            .as_str()
            .unwrap()
            .contains("\"diff\""),
        "verifier evidence must omit file diffs"
    );
    let context = db.recording_context(request).await.unwrap().unwrap();
    assert_eq!(
        context["context"]["provider_messages"], requests[0]["messages"],
        "context_json holds the FIRST window verbatim"
    );
    assert_eq!(context["context"]["adapter"], "tool_calls_v1");
    std::fs::remove_dir_all(root).ok();
}

/// P5-T03: a delegated exploration is one `subagent` step under the `task` tool-call step, with
/// the sub-agent's own model and tool calls hanging off it. The sub-agent stays read-only even
/// in `auto_all`: a `write` it asks for is refused before the registry is reached, so the file
/// is untouched and no approval is ever raised. The parent model receives the bounded report,
/// not the sub-agent's transcript.
#[tokio::test]
async fn a_sub_agent_explores_under_its_own_steps_and_cannot_write() {
    let db = DbStore::init(":memory:").unwrap();
    let root = project(&db, "auto_all", ScopePatch::default()).await;
    let script = Script::new(vec![
        calls(vec![(
            "call-1",
            "task",
            r#"{"description":"find beta","prompt":"say which line of notes.md holds beta"}"#,
        )]),
        calls(vec![
            ("sub-1", "read", r#"{"path":"notes.md"}"#),
            (
                "sub-2",
                "write",
                r#"{"path":"notes.md","content":"nope\n","overwrite":true}"#,
            ),
        ]),
        text("notes.md line 2 holds beta."),
        text("beta is on line 2 of notes.md."),
    ]);
    let agents = provider(script.clone()).await;
    let turn = claim(&db, "which line holds beta").await;
    let request = turn.request.clone();
    recording::generate(&db, &agents, turn).await.unwrap();

    let saved = receipt(&db, &request).await;
    assert_eq!(saved["state"], "complete");
    assert_eq!(saved["response"], "beta is on line 2 of notes.md.");
    assert_eq!(
        std::fs::read_to_string(root.join("notes.md")).unwrap(),
        "alpha\nbeta\n",
        "delegation must not become a way to reach a side-effecting tool"
    );
    assert!(
        permissions(&db).await.is_empty(),
        "nothing inside a read-only sub-agent can raise an approval"
    );

    let rows = steps(&db, &request).await;
    let shape: Vec<(&str, &str, &str, &str)> = rows
        .iter()
        .map(|r| {
            (
                r["kind"].as_str().unwrap(),
                r["status"].as_str().unwrap(),
                r["tool"].as_str().unwrap(),
                r["error"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        shape,
        vec![
            ("model_call", "complete", "", ""),
            ("tool_call", "complete", "task", ""),
            ("subagent", "complete", "", ""),
            ("model_call", "complete", "", ""),
            ("tool_call", "complete", "read", ""),
            ("tool_call", "failed", "write", "unknown_tool"),
            ("model_call", "complete", "", ""),
            ("model_call", "complete", "", ""),
            ("verification", "complete", "", "")
        ],
        "{rows:#?}"
    );

    // The whole point of `parent_step_id`: one ordered step list that still reads back as a tree.
    let tree = db.run({
            let request = request.clone();
            move |c| {
                let mut stmt = c.prepare("SELECT COALESCE((SELECT p.seq FROM turn_steps p WHERE p.id=s.parent_step_id),-1) \
                    FROM turn_steps s WHERE s.request_id=?1 ORDER BY s.seq")?;
                let rows = stmt.query_map([request], |r| r.get::<_, i64>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
                Ok(rows)
            }
        }).await.unwrap();
    assert_eq!(tree, vec![-1, -1, 1, 2, 2, 2, 2, -1, -1],
            "the subagent step hangs off the task step, and the sub-agent's work hangs off the subagent step");

    assert_eq!(
        kinds(&db, &request).await,
        vec![
            "turn_started",
            "model_call_started",
            "model_call_finished",
            "tool_started",
            "subagent_started",
            "model_call_started",
            "model_call_finished",
            "tool_started",
            "tool_finished",
            "tool_started",
            "tool_finished",
            "model_call_started",
            "model_call_finished",
            "subagent_finished",
            "tool_finished",
            "model_call_started",
            "model_call_finished",
            "verification_started",
            "verified",
            "answer_saved"
        ]
    );

    let requests = script.requests();
    assert_eq!(
        requests.len(),
        4,
        "one parent call, two sub-agent calls, then the parent's answer"
    );
    let offered: Vec<&str> = requests[1]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["function"]["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        offered,
        subagent::TOOLS,
        "a sub-agent is offered read-only tools only"
    );
    assert_eq!(
        requests[1]["messages"].as_array().unwrap().len(),
        2,
        "a sub-agent starts from its own context, not the parent's history"
    );

    let report = requests[3]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .rfind(|message| message["role"] == "tool")
        .cloned()
        .expect("the report reaches the parent");
    let content = report["content"].as_str().unwrap();
    assert!(
        content.contains("sub-agent report") && content.contains("notes.md line 2 holds beta."),
        "{content}"
    );
    assert!(
        content.contains("files read:") && content.contains("notes.md"),
        "{content}"
    );
    assert!(
        !content.contains("alpha"),
        "the parent gets the report, never the transcript: {content}"
    );
    assert!(
        content.chars().count() <= subagent::MAX_SUMMARY_CHARS + 200,
        "{content}"
    );
    std::fs::remove_dir_all(root).ok();
}

/// P5-T01: verification is advisory. A report the parser refuses must never rewrite or discard
/// the answer the turn already earned — it is recorded as an unavailable verification, nothing more.
#[tokio::test]
async fn a_broken_verifier_leaves_the_answer_intact_and_records_unavailable() {
    let db = DbStore::init(":memory:").unwrap();
    let root = project(&db, "auto_all", ScopePatch::default()).await;
    let script = Script::new(vec![text("notes.md still starts with alpha.")]);
    script.push_verification_reply(text("sure thing! ```json {\"claims\":[]}```"));
    let agents = provider(script.clone()).await;
    let turn = claim(&db, "check the notes").await;
    let request = turn.request.clone();
    recording::generate(&db, &agents, turn).await.unwrap();

    let saved = receipt(&db, &request).await;
    assert_eq!(
        saved["state"], "complete",
        "an advisory verifier must never fail the turn"
    );
    assert_eq!(
        saved["response"], "notes.md still starts with alpha.",
        "the answer survives a broken verifier"
    );
    let rows = steps(&db, &request).await;
    let last = rows.last().unwrap();
    assert_eq!(
        (&last["kind"], &last["status"], &last["error"]),
        (
            &json!("verification"),
            &json!("failed"),
            &json!("verification_failed")
        ),
        "{rows:#?}"
    );
    let output: Value = serde_json::from_str(last["output"].as_str().unwrap()).unwrap();
    assert_eq!(output["status"], "unavailable");
    assert_eq!(output["claims"].as_array().unwrap().len(), 0);
    assert_eq!(
        kinds(&db, &request).await,
        vec![
            "turn_started",
            "model_call_started",
            "model_call_finished",
            "verification_started",
            "verified",
            "answer_saved"
        ]
    );
    std::fs::remove_dir_all(root).ok();
}

/// P5-T01: a claim this turn never evidenced is reported as unverified, with the verifier's own
/// reason kept for the rail. The answer is still delivered unchanged; the badge does the warning.
#[tokio::test]
async fn an_unevidenced_claim_is_recorded_as_unverified() {
    let db = DbStore::init(":memory:").unwrap();
    let root = project(&db, "auto_all", ScopePatch::default()).await;
    let script = Script::new(vec![text("Every test passes on main.")]);
    script.push_verification_reply(text(&json!({"claims":[{"claim":"Every test passes on main.",
            "status":"unverified","evidence_step_ids":[],"reason":"No recorded step in this turn ran the suite."}],
            "skipped_diagnostics":["this turn recorded no tool steps"]}).to_string()));
    let agents = provider(script.clone()).await;
    let turn = claim(&db, "did the tests pass").await;
    let request = turn.request.clone();
    recording::generate(&db, &agents, turn).await.unwrap();

    assert_eq!(
        receipt(&db, &request).await["response"],
        "Every test passes on main.",
        "the verifier advises, it never edits"
    );
    let rows = steps(&db, &request).await;
    let last = rows.last().unwrap();
    assert_eq!(
        (&last["kind"], &last["status"], &last["error"]),
        (&json!("verification"), &json!("complete"), &json!("")),
        "{rows:#?}"
    );
    let output: Value = serde_json::from_str(last["output"].as_str().unwrap()).unwrap();
    assert_eq!(output["status"], "unverified");
    assert_eq!(output["claims"][0]["status"], "unverified");
    assert_eq!(
        output["claims"][0]["reason"],
        "No recorded step in this turn ran the suite."
    );
    assert_eq!(
        output["skipped_diagnostics"][0],
        "this turn recorded no tool steps"
    );
    assert_eq!(
        script.verification_requests().len(),
        1,
        "one answer means exactly one verifier call"
    );
    std::fs::remove_dir_all(root).ok();
}

#[tokio::test]
async fn an_exhausted_budget_answers_without_claiming_success() {
    let db = DbStore::init(":memory:").unwrap();
    let root = project(
        &db,
        "auto_all",
        ScopePatch {
            max_steps: Patch::Set(1),
            ..Default::default()
        },
    )
    .await;
    let script = Script::new(vec![calls(vec![(
        "call-1",
        "read",
        r#"{"path":"notes.md"}"#,
    )])]);
    let agents = provider(script.clone()).await;
    let turn = claim(&db, "read everything").await;
    let request = turn.request.clone();
    recording::generate(&db, &agents, turn).await.unwrap();

    let saved = receipt(&db, &request).await;
    assert_eq!(
        saved["state"], "complete",
        "a budget stop still owes the user an answer"
    );
    let answer = saved["response"].as_str().unwrap();
    assert!(
        answer.contains("max_steps") && answer.contains("NOT finished"),
        "{answer}"
    );
    assert!(kinds(&db, &request)
        .await
        .contains(&"budget_exhausted".to_string()));
    assert_eq!(
        script.requests().len(),
        1,
        "the budget must stop the loop before another paid call"
    );
    assert_eq!(steps(&db, &request).await.len(), 2);
    std::fs::remove_dir_all(root).ok();
}

#[tokio::test]
async fn unreadable_tool_arguments_never_reach_a_tool() {
    let db = DbStore::init(":memory:").unwrap();
    let root = project(&db, "auto_all", ScopePatch::default()).await;
    let script = Script::new(vec![
        calls(vec![
            ("call-1", "write", "{not json"),
            ("call-2", "read", "[1,2]"),
        ]),
        text("I could not use those arguments."),
    ]);
    let agents = provider(script.clone()).await;
    let turn = claim(&db, "break the arguments").await;
    let request = turn.request.clone();
    recording::generate(&db, &agents, turn).await.unwrap();

    let rows = steps(&db, &request).await;
    assert_eq!(rows[1]["status"], "failed");
    assert_eq!(rows[1]["error"], "invalid_arguments");
    assert_eq!(
        rows[2]["error"], "invalid_arguments",
        "a JSON array is not a tool argument object"
    );
    assert_eq!(
        std::fs::read_to_string(root.join("notes.md")).unwrap(),
        "alpha\nbeta\n",
        "the file must be untouched"
    );
    let second = &script.requests()[1]["messages"];
    assert!(
        second
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["role"] == "tool"
                && m["content"].as_str().unwrap().contains("invalid_arguments")),
        "the model has to see why its call was refused: {second}"
    );
    std::fs::remove_dir_all(root).ok();
}

#[tokio::test]
async fn a_denied_tool_changes_nothing_and_is_reported_to_the_model() {
    let db = DbStore::init(":memory:").unwrap();
    let root = project(&db, "ask", ScopePatch::default()).await;
    let script = Script::new(vec![
        calls(vec![(
            "call-1",
            "write",
            r#"{"path":"notes.md","content":"wiped\n","overwrite":true}"#,
        )]),
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
            if decide(&denier, "denied").await {
                return;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    });
    recording::generate(&db, &agents, turn).await.unwrap();

    assert_eq!(
        std::fs::read_to_string(root.join("notes.md")).unwrap(),
        "alpha\nbeta\n",
        "a denied write must never reach the disk"
    );
    let rows = steps(&db, &request).await;
    assert_eq!(
        (&rows[1]["status"], &rows[1]["error"]),
        (&json!("denied"), &json!("denied")),
        "{rows:#?}"
    );
    let events = kinds(&db, &request).await;
    assert!(
        events.contains(&"permission_requested".to_string()),
        "{events:?}"
    );
    assert_eq!(
        receipt(&db, &request).await["response"],
        "Understood, I left the file alone."
    );
    let second = &script.requests()[1]["messages"];
    assert!(
        second
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["role"] == "tool" && m["content"].as_str().unwrap().contains("denied")),
        "{second}"
    );
    assert!(
        db.run(
            |c| Ok(c.query_row("SELECT count(*) FROM file_changes", [], |r| r
                .get::<_, i64>(0))?)
        )
        .await
        .unwrap()
            == 0
    );
    std::fs::remove_dir_all(root).ok();
}

#[tokio::test]
async fn a_provider_without_tool_support_falls_back_to_text() {
    let db = DbStore::init(":memory:").unwrap();
    let root = project(&db, "auto_all", ScopePatch::default()).await;
    let script = Script::new(vec![
        (
            400,
            json!({"error":{"message":"tools are not supported by this model"}}),
        ),
        text("Answered from text alone."),
    ]);
    let agents = provider(script.clone()).await;
    let turn = claim(&db, "hello").await;
    let request = turn.request.clone();
    recording::generate(&db, &agents, turn).await.unwrap();

    let rows = steps(&db, &request).await;
    assert_eq!(
        (&rows[0]["status"], &rows[0]["error"]),
        (&json!("failed"), &json!("tools_unsupported")),
        "{rows:#?}"
    );
    assert_eq!(rows[1]["status"], "complete");
    assert_eq!(
        receipt(&db, &request).await["response"],
        "Answered from text alone."
    );
    let requests = script.requests();
    assert!(
        requests[0]["tools"].is_array() && requests[1]["tools"].is_null(),
        "the retry must drop the tools it was rejected for"
    );
    let request_for_events = request.clone();
    let chunks: Vec<String> = db.run(move |c| {
            let mut stmt = c.prepare("SELECT content FROM generation_events WHERE request_id=?1 AND state='chunk' ORDER BY seq")?;
            let rows = stmt.query_map([request_for_events], |r| r.get(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        }).await.unwrap();
    assert_eq!(chunks, vec!["Answered from text alone."]);
    std::fs::remove_dir_all(root).ok();
}

/// P1-T11: the one path P1-T10 could not reach. An approval has to actually let the tool run.
#[tokio::test]
async fn approving_permissions_unblocks_a_waiting_turn() {
    let db = DbStore::init(":memory:").unwrap();
    let root = project(&db, "ask", ScopePatch::default()).await;
    let script = Script::new(vec![
        calls(vec![(
            "call-1",
            "write",
            r#"{"path":"notes.md","content":"approved\n","overwrite":true}"#,
        )]),
        text("Wrote notes.md after you approved it."),
    ]);
    let agents = provider(script.clone()).await;
    let turn = claim(&db, "overwrite my notes").await;
    let request = turn.request.clone();

    let approver = db.clone();
    tokio::spawn(async move {
        for _ in 0..400 {
            if decide(&approver, "approved").await {
                return;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    });
    recording::generate(&db, &agents, turn).await.unwrap();

    assert_eq!(
        std::fs::read_to_string(root.join("notes.md")).unwrap(),
        "approved\n",
        "an approved write must reach the disk"
    );
    let rows = steps(&db, &request).await;
    assert_eq!(
        (&rows[1]["kind"], &rows[1]["status"], &rows[1]["error"]),
        (&json!("tool_call"), &json!("complete"), &json!("")),
        "{rows:#?}"
    );
    let events = kinds(&db, &request).await;
    assert_eq!(
        events
            .iter()
            .filter(|k| k.as_str() == "permission_resolved")
            .count(),
        1,
        "{events:?}"
    );
    assert!(
        events.contains(&"permission_requested".to_string())
            && events.contains(&"file_changed".to_string()),
        "{events:?}"
    );
    assert_eq!(
        receipt(&db, &request).await["response"],
        "Wrote notes.md after you approved it."
    );
    let rows = permissions(&db).await;
    assert_eq!(
        (rows.len(), &rows[0]["tool"], &rows[0]["status"]),
        (1, &json!("write"), &json!("approved")),
        "{rows:#?}"
    );
    assert!(
        rows[0]["summary"].as_str().unwrap().contains("notes.md"),
        "the card the human saw names the file: {:?}",
        rows[0]["summary"]
    );
    assert!(
        db.pending_permissions("global".into()).await.unwrap()["permissions"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    std::fs::remove_dir_all(root).ok();
}

/// `auto_edit` is the whole point of having modes: writes stop asking, `bash` does not.
#[tokio::test]
async fn auto_edit_permissions_pass_a_write_and_still_stop_bash() {
    let db = DbStore::init(":memory:").unwrap();
    let root = project(&db, "auto_edit", ScopePatch::default()).await;
    let script = Script::new(vec![
        calls(vec![
            (
                "call-1",
                "write",
                r#"{"path":"notes.md","content":"edited\n","overwrite":true}"#,
            ),
            (
                "call-2",
                "bash",
                r#"{"command":"echo hi","description":"say hi"}"#,
            ),
        ]),
        text("I wrote the file; the command was refused."),
    ]);
    let agents = provider(script.clone()).await;
    let turn = claim(&db, "write it then run something").await;
    let request = turn.request.clone();

    let denier = db.clone();
    tokio::spawn(async move {
        for _ in 0..400 {
            if decide(&denier, "denied").await {
                return;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    });
    recording::generate(&db, &agents, turn).await.unwrap();

    assert_eq!(
        std::fs::read_to_string(root.join("notes.md")).unwrap(),
        "edited\n",
        "auto_edit approves an edit without a human"
    );
    let rows = permissions(&db).await;
    assert_eq!(
        (rows.len(), &rows[0]["tool"], &rows[0]["status"]),
        (1, &json!("bash"), &json!("denied")),
        "only bash may ask in auto_edit: {rows:#?}"
    );
    let steps = steps(&db, &request).await;
    let shape: Vec<(&str, &str)> = steps
        .iter()
        .map(|r| (r["tool"].as_str().unwrap(), r["status"].as_str().unwrap()))
        .collect();
    assert_eq!(
        shape,
        vec![
            ("", "complete"),
            ("write", "complete"),
            ("bash", "denied"),
            ("", "complete"),
            ("", "complete")
        ],
        "{steps:#?}"
    );
    std::fs::remove_dir_all(root).ok();
}

/// `auto_all` still stops at the bash deny-list from docs/design/tools.md#bash.
#[tokio::test]
async fn auto_all_permissions_still_ask_before_a_deny_listed_command() {
    let db = DbStore::init(":memory:").unwrap();
    let root = project(&db, "auto_all", ScopePatch::default()).await;
    let script = Script::new(vec![
        calls(vec![(
            "call-1",
            "bash",
            r#"{"command":"git push --force origin main","description":"force push"}"#,
        )]),
        text("I did not force-push."),
    ]);
    let agents = provider(script.clone()).await;
    let turn = claim(&db, "force push it").await;
    let request = turn.request.clone();

    let denier = db.clone();
    tokio::spawn(async move {
        for _ in 0..400 {
            if decide(&denier, "denied").await {
                return;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    });
    recording::generate(&db, &agents, turn).await.unwrap();

    let rows = permissions(&db).await;
    assert_eq!(
        (rows.len(), &rows[0]["status"]),
        (1, &json!("denied")),
        "a deny-listed command asks even in auto_all: {rows:#?}"
    );
    let steps = steps(&db, &request).await;
    assert_eq!(
        (&steps[1]["tool"], &steps[1]["status"], &steps[1]["error"]),
        (&json!("bash"), &json!("denied"), &json!("denied")),
        "{steps:#?}"
    );
    let second = &script.requests()[1]["messages"];
    assert!(
        second
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["role"] == "tool" && m["content"].as_str().unwrap().contains("denied")),
        "{second}"
    );
    std::fs::remove_dir_all(root).ok();
}

/// The T10 question T11 had to answer: a row may not outlive the turn that is waiting on it.
#[tokio::test]
async fn permissions_expire_with_the_turn_not_thirty_minutes_later() {
    assert_eq!(
        permission_ttl(15 * 60),
        15 * 60,
        "a shorter wall budget wins"
    );
    assert_eq!(
        permission_ttl(45 * 60),
        PERMISSION_TTL_SECONDS,
        "the design TTL caps a generous budget"
    );
    assert_eq!(
        permission_ttl(-3),
        0,
        "a turn already out of time cannot open a live approval"
    );
}

#[tokio::test]
async fn resolving_permissions_is_idempotent_and_refuses_a_flip() {
    let db = DbStore::init(":memory:").unwrap();
    let turn = claim(&db, "needs approval").await;
    let step = db
        .begin_step(NewStep {
            request: turn.request.clone(),
            session: turn.session.clone(),
            kind: "tool_call",
            tool_name: Some("write".into()),
            tool_call_id: Some("call-1".into()),
            input: json!({"path":"a.md"}),
            event: "tool_started",
            payload: json!({"tool":"write","summary":"write a.md"}),
        })
        .await
        .unwrap();
    let id = db
        .request_permission(NewPermission {
            request: turn.request.clone(),
            session: turn.session.clone(),
            step,
            tool: "write".into(),
            summary: "write a.md".into(),
            args: json!({"diff":"+x"}),
            ttl_seconds: 900,
        })
        .await
        .unwrap();

    assert!(matches!(
        db.resolve_permission(id.clone(), "global".into(), "approved")
            .await
            .unwrap(),
        Resolution::Recorded
    ));
    assert!(
        matches!(
            db.resolve_permission(id.clone(), "global".into(), "approved")
                .await
                .unwrap(),
            Resolution::Unchanged
        ),
        "a replayed click is not an error"
    );
    assert!(
        matches!(
            db.resolve_permission(id.clone(), "global".into(), "denied")
                .await
                .unwrap(),
            Resolution::Conflict
        ),
        "a resolved approval cannot be flipped"
    );
    assert!(
        matches!(
            db.resolve_permission(id.clone(), "other".into(), "denied")
                .await
                .unwrap(),
            Resolution::NotFound
        ),
        "another scope cannot see this row"
    );
    assert!(matches!(
        db.resolve_permission(uid(), "global".into(), "denied")
            .await
            .unwrap(),
        Resolution::NotFound
    ));
    let events = kinds(&db, &turn.request).await;
    assert_eq!(
        events
            .iter()
            .filter(|k| k.as_str() == "permission_resolved")
            .count(),
        1,
        "one decision, one event: {events:?}"
    );
    assert_eq!(permissions(&db).await[0]["status"], "approved");
}

#[tokio::test]
async fn expired_permissions_cannot_be_approved_afterwards() {
    let db = DbStore::init(":memory:").unwrap();
    let turn = claim(&db, "needs approval").await;
    let step = db
        .begin_step(NewStep {
            request: turn.request.clone(),
            session: turn.session.clone(),
            kind: "tool_call",
            tool_name: Some("bash".into()),
            tool_call_id: Some("call-1".into()),
            input: json!({"command":"echo hi"}),
            event: "tool_started",
            payload: json!({"tool":"bash","summary":"bash echo hi"}),
        })
        .await
        .unwrap();
    let id = db
        .request_permission(NewPermission {
            request: turn.request.clone(),
            session: turn.session.clone(),
            step: step.clone(),
            tool: "bash".into(),
            summary: "bash echo hi".into(),
            args: json!({"command":"echo hi"}),
            ttl_seconds: 0,
        })
        .await
        .unwrap();
    // The loop gave up first, exactly as it does when the wall budget runs out.
    assert_eq!(
        db.expire_permission(id.clone(), turn.request.clone(), turn.session.clone(), step)
            .await
            .unwrap(),
        "expired"
    );
    assert!(
        matches!(
            db.resolve_permission(id, "global".into(), "approved")
                .await
                .unwrap(),
            Resolution::Expired
        ),
        "an approval that arrives too late must not run anything"
    );
}

#[tokio::test]
async fn a_provider_failure_fails_the_turn_without_an_invented_answer() {
    let db = DbStore::init(":memory:").unwrap();
    let agents = provider(Script::new(vec![(
        503,
        json!({"error":"upstream is down"}),
    )]))
    .await;
    let turn = claim(&db, "anything").await;
    let request = turn.request.clone();
    recording::generate(&db, &agents, turn).await.unwrap();

    let saved = receipt(&db, &request).await;
    assert_eq!(
        (&saved["state"], &saved["error_code"], &saved["response"]),
        (&json!("failed"), &json!("provider_failed"), &json!(null))
    );
    let rows = steps(&db, &request).await;
    assert_eq!(
        (&rows[0]["kind"], &rows[0]["status"], &rows[0]["error"]),
        (
            &json!("model_call"),
            &json!("failed"),
            &json!("provider_failed")
        )
    );
    assert_eq!(kinds(&db, &request).await.last().unwrap(), "turn_failed");
}

#[tokio::test]
async fn a_restart_interrupts_running_work_and_reruns_nothing() {
    let db = DbStore::init(":memory:").unwrap();
    let turn = claim(&db, "hold for restart").await;
    let request = turn.request.clone();
    let step = db
        .begin_step(NewStep {
            request: request.clone(),
            session: turn.session.clone(),
            kind: "tool_call",
            tool_name: Some("bash".into()),
            tool_call_id: Some("call-1".into()),
            input: json!({"command":"sleep 60"}),
            event: "tool_started",
            payload: json!({"tool":"bash","summary":"bash sleep 60"}),
        })
        .await
        .unwrap();
    db.request_permission(NewPermission {
        request: request.clone(),
        session: turn.session.clone(),
        step: step.clone(),
        tool: "bash".into(),
        summary: "bash sleep 60".into(),
        args: json!({"command":"sleep 60"}),
        ttl_seconds: 900,
    })
    .await
    .unwrap();

    // The process dies here. Startup recovery is the only thing that gets to speak next.
    db.run(crate::recording::recover).await.unwrap();

    let rows = steps(&db, &request).await;
    assert_eq!(
        rows[0]["status"], "interrupted",
        "a step that was running when the process died is interrupted, not failed"
    );
    assert_eq!(receipt(&db, &request).await["state"], "interrupted");
    let pending: i64 = db
        .run(|c| {
            Ok(c.query_row(
                "SELECT count(*) FROM permission_requests WHERE status='pending'",
                [],
                |r| r.get(0),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(
        pending, 0,
        "a pending approval cannot survive the process that was waiting on it"
    );
    assert!(kinds(&db, &request)
        .await
        .contains(&"interrupted".to_string()));
    assert!(
        db.claim_recording().await.unwrap().is_none(),
        "an interrupted turn must never be claimed again"
    );
}

/// P14-REVIEW-01: a cancel that lands while a sub-agent's provider call is in flight must
/// interrupt the child model step, the delegation and its parent step, and must not let the turn
/// record a completed answer or make another child provider call.
#[tokio::test]
async fn cancelling_a_held_sub_agent_provider_call_interrupts_without_a_further_call() {
    let db = DbStore::init(":memory:").unwrap();
    let _root = project(&db, "auto_all", ScopePatch::default()).await;
    let (agents, held) = held_provider().await;
    let turn = claim(&db, "cancel a delegated exploration").await;
    let request = turn.request.clone();

    let mut generate = Box::pin(recording::generate(&db, &agents, turn));
    loop {
        tokio::select! {
            result = &mut generate => {
                result.unwrap();
                break;
            }
            permit = held.arrived.acquire() => {
                // The sub-agent's provider call is now held open; cancel the run and let the
                // durable race observe the intent and drop the in-flight future. Releasing the
                // handler as well means a fix-less direct await would still return and record a
                // completed answer, so this test fails (rather than hangs) without the race.
                permit.unwrap().forget();
                db.request_cancellation(request.clone()).await.unwrap();
                held.release.add_permits(1);
            }
        }
    }
    held.release.add_permits(1); // let the abandoned provider handler unwind

    let saved = receipt(&db, &request).await;
    assert_eq!(
        (saved["state"].as_str(), saved["error_code"].as_str()),
        (Some("interrupted"), Some("cancelled")),
        "a cancelled sub-agent turn must not record a completed answer"
    );

    let rows = steps(&db, &request).await;
    let shape: Vec<(&str, &str, &str)> = rows
        .iter()
        .map(|r| {
            (
                r["kind"].as_str().unwrap(),
                r["status"].as_str().unwrap(),
                r["error"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        shape,
        vec![
            ("model_call", "complete", ""),
            ("tool_call", "interrupted", "cancelled"),
            ("subagent", "interrupted", "cancelled"),
            ("model_call", "interrupted", "cancelled"),
        ],
        "the child model call, its delegation and the parent step are all interrupted: {rows:#?}"
    );
    assert_eq!(
        *held.calls.lock().unwrap(),
        2,
        "a cancelled sub-agent made no further provider call"
    );
}

/// P14-T04b: the parent loop already degrades to text when a provider rejects `tools`, but the
/// delegation path used to fail the whole sub-agent on the identical error from the identical
/// provider. A capability the turn already discovered must apply to every role, so the
/// delegated call drops its definitions and answers from text instead of reporting a provider
/// failure.
#[tokio::test]
/// The name deliberately contains `provider` so the task's own verify command,
/// `cargo test --locked provider`, actually runs this test instead of filtering it out.
async fn a_delegated_provider_call_falls_back_to_text_like_the_parent_loop() {
    let db = DbStore::init(":memory:").unwrap();
    let root = project(&db, "auto_all", ScopePatch::default()).await;
    let script = Script::new(vec![
        calls(vec![(
            "call-1",
            "task",
            r#"{"description":"find beta","prompt":"say which line of notes.md holds beta"}"#,
        )]),
        (
            400,
            json!({"error":{"message":"tools are not supported by this model"}}),
        ),
        text("notes.md line 2 holds beta."),
        text("beta is on line 2 of notes.md."),
    ]);
    let agents = provider(script.clone()).await;
    let turn = claim(&db, "which line holds beta").await;
    let request = turn.request.clone();
    recording::generate(&db, &agents, turn).await.unwrap();

    let saved = receipt(&db, &request).await;
    assert_eq!(
        (saved["state"].as_str(), saved["response"].as_str()),
        (Some("complete"), Some("beta is on line 2 of notes.md.")),
        "a tools-rejecting provider must not fail the turn through a delegation: {saved:#?}"
    );

    let rows = steps(&db, &request).await;
    let errors: Vec<&str> = rows
        .iter()
        .map(|r| r["error"].as_str().unwrap())
        .filter(|error| !error.is_empty())
        .collect();
    assert_eq!(
        errors,
        vec!["tools_unsupported"],
        "the delegated call records the capability, not a provider failure: {rows:#?}"
    );
    assert!(
        rows.iter()
            .any(|r| r["kind"] == "subagent" && r["status"] == "complete"),
        "the delegation itself still completes: {rows:#?}"
    );

    let requests = script.requests();
    assert!(
        requests[1]["tools"].is_array() && requests[2]["tools"].is_null(),
        "the delegated retry must drop the tools it was rejected for: {requests:#?}"
    );
    std::fs::remove_dir_all(root).ok();
}
