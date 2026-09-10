//! Recording receipts: durable admission, a serial generation worker, and a memory outbox.
//! Sanitized text only. This is NOT an encrypted exact-original archive.
use crate::{
    agent_loop, agentic_sql as agentic, context,
    ingest::Event,
    memory_agents::MemoryAgents,
    recording_sql as sql, safety,
    storage::{now, uid, DbStore, Recall, ScopeConfig},
    tools::Registry,
};
use anyhow::{bail, Result};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde_json::{json, Value};

pub struct CaptureInput {
    pub request: String,
    pub session: String,
    pub scope: String,
    pub prompt: String,
    pub model: String,
    pub signature: String,
    pub redacted: bool,
}
pub enum Admission {
    Saved(Value),
    Conflict,
    ScopeConflict,
    Busy,
    Full,
}
pub struct Generation {
    pub request: String,
    pub session: String,
    pub scope: String,
    pub model: String,
    pub prompt: String,
    pub events: Vec<Event>,
}

pub fn recover(c: &mut Connection) -> Result<()> {
    let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let stamp = now();
    tx.execute(sql::RECOVER_EVENTS, [&stamp])?;
    // Agentic rows first: both `RECOVER_ACTIVITY` and `RECOVER_EVENTS` select the receipts that
    // are still `generating`, so they have to run before the receipt itself is interrupted.
    // A `running` step becomes `interrupted`, a pending approval expires, and nothing is
    // retried: `claim_recording` only ever claims a `captured` receipt, so no tool and no
    // possibly billed provider call is repeated after a restart.
    tx.execute(agentic::RECOVER_STEPS, [&stamp])?;
    tx.execute(agentic::RECOVER_PERMISSIONS, [&stamp])?;
    tx.execute(agentic::RECOVER_ACTIVITY, [&stamp])?;
    tx.execute(sql::RECOVER, [&stamp])?;
    tx.execute(
        "INSERT INTO generation_events(request_id,session_id,state,content,error_code,created_at) SELECT request_id,session_id,'interrupted','', 'process_restarted', ?1 FROM chat_receipts WHERE state='interrupted' AND error_code='process_restarted'",
        [&stamp],
    )?;
    tx.execute(sql::RECOVER_MESSAGES, [])?;
    tx.execute(
        "UPDATE jobs SET status='pending' WHERE status='running'",
        [],
    )?;
    tx.commit()?;
    Ok(())
}

fn receipt(c: &Connection, request: &str) -> Result<Option<Value>> {
    let row = c.query_row(
        "SELECT r.request_id,r.session_id,r.scope,r.model,r.redacted,r.state,r.error_code,r.captured_at,r.updated_at,r.context_json,a.content,o.job_id,j.status FROM chat_receipts r LEFT JOIN messages a ON a.id=r.answer_id LEFT JOIN recording_outbox o ON o.request_id=r.request_id LEFT JOIN jobs j ON j.id=o.job_id WHERE r.request_id=?1",
        [request], |r| {
            let state: String = r.get(5)?;
            let context: Option<String> = r.get(9)?;
            let context: Value = context.and_then(|v| serde_json::from_str(&v).ok()).unwrap_or(Value::Null);
            let job_status: Option<String> = r.get(12)?;
            let memory_status = job_status.unwrap_or_else(|| if state=="captured" || state=="generating" {"waiting_for_turn".into()} else {"deferred".into()});
            Ok(json!({"request_id":r.get::<_,String>(0)?,"session_id":r.get::<_,String>(1)?,
                "scope":r.get::<_,String>(2)?,"model":r.get::<_,String>(3)?,"redacted":r.get::<_,bool>(4)?,
                "state":state,"error_code":r.get::<_,Option<String>>(6)?,"captured_at":r.get::<_,String>(7)?,
                "updated_at":r.get::<_,String>(8)?,"response":r.get::<_,Option<String>>(10)?,
                "memory_job_id":r.get::<_,Option<String>>(11)?,"memory_status":memory_status,
                "recording":"sanitized_local","context_available":!context.is_null(),
                "recalled":context.get("memories").cloned().unwrap_or(json!([])),
                "recalled_context_applied":context.get("memories").and_then(Value::as_array).is_some_and(|m|!m.is_empty()),
                "confirmation_prompt":null}))
        }).optional()?;
    Ok(row)
}

impl DbStore {
    pub async fn capture_chat(&self, input: CaptureInput) -> Result<Admission> {
        self.run(move |c| {
            let tx=c.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let prior: Option<String> = tx.query_row("SELECT signature FROM chat_receipts WHERE request_id=?1", [&input.request], |r|r.get(0)).optional()?;
            if let Some(signature)=prior {
                return if signature==input.signature {Ok(Admission::Saved(receipt(&tx,&input.request)?.unwrap()))} else {Ok(Admission::Conflict)};
            }
            // Old message identifiers cannot be reused either; no invented legacy receipt.
            if tx.query_row("SELECT EXISTS(SELECT 1 FROM messages WHERE id=?1)",[&input.request],|r|r.get::<_,bool>(0))? {return Ok(Admission::Conflict);}
            let pending:i64=tx.query_row("SELECT count(*) FROM chat_receipts WHERE state IN ('captured','generating')",[],|r|r.get(0))?;
            if pending>=100 {return Ok(Admission::Full);}
            tx.execute("INSERT INTO sessions(id,scope,created_at) VALUES(?1,?2,?3) ON CONFLICT(id) DO NOTHING",params![input.session,input.scope,now()])?;
            let actual:String=tx.query_row("SELECT scope FROM sessions WHERE id=?1",[&input.session],|r|r.get(0))?;
            if actual!=input.scope {return Ok(Admission::ScopeConflict);}
            if tx.query_row("SELECT EXISTS(SELECT 1 FROM chat_receipts WHERE session_id=?1 AND state IN ('captured','generating'))",[&input.session],|r|r.get::<_,bool>(0))? {return Ok(Admission::Busy);}
            let stamp=now();
            tx.execute(sql::INSERT_MESSAGE,params![input.request,input.session,input.prompt,stamp])?;
            tx.execute(sql::INSERT_RECEIPT,params![input.request,input.session,input.scope,input.model,input.signature,input.redacted,stamp])?;
            tx.execute(sql::INSERT_OUTBOX,params![input.request,stamp])?;
            tx.execute(sql::EVENT,params![input.request,"captured",stamp])?;
            let result=receipt(&tx,&input.request)?.unwrap();
            tx.commit()?;
            Ok(Admission::Saved(result))
        }).await
    }
    pub async fn recording_receipt(&self, request: String) -> Result<Option<Value>> {
        self.run(move |c| receipt(c, &request)).await
    }
    pub async fn recording_context(&self, request: String) -> Result<Option<Value>> {
        self.run(move |c| {
            let Some(mut result) = receipt(c, &request)? else {
                return Ok(None);
            };
            let context: Option<String> = c.query_row(
                "SELECT context_json FROM chat_receipts WHERE request_id=?1",
                [&request],
                |r| r.get(0),
            )?;
            result["context"] = context
                .map(|v| serde_json::from_str::<Value>(&v))
                .transpose()?
                .unwrap_or(Value::Null);
            let mut stmt = c.prepare(
                "SELECT kind,created_at FROM recording_events WHERE request_id=?1 ORDER BY seq",
            )?;
            result["events"] = json!(stmt
                .query_map([request], |r| Ok(
                    json!({"kind":r.get::<_,String>(0)?,"at":r.get::<_,String>(1)?})
                ))?
                .collect::<rusqlite::Result<Vec<_>>>()?);
            Ok(Some(result))
        })
        .await
    }
    pub async fn claim_recording(&self) -> Result<Option<Generation>> {
        self.run(|c| {
            let tx=c.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let row:Option<(String,String,String,String,String,i64)>=tx.query_row(
                "SELECT r.request_id,r.session_id,r.scope,r.model,m.content,m.seq FROM chat_receipts r JOIN messages m ON m.id=r.request_id WHERE r.state='captured' ORDER BY m.seq LIMIT 1",[],
                |r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?))).optional()?;
            let Some((request,session,scope,model,prompt,seq))=row else {return Ok(None)};
            // Admission permits one unfinished turn per session, so completed pairs
            // remain ordered. Other sessions can queue independently.
            let mut events={
                let mut stmt=tx.prepare("SELECT m.id,m.role,m.content FROM messages m WHERE m.session_id=?1 AND m.status='complete' AND m.seq<?2 ORDER BY m.seq DESC LIMIT 20")?;
                let mut rows=stmt.query_map(params![session,seq],|r|Ok(Event{id:r.get(0)?,role:r.get(1)?,content:r.get(2)?}))?.collect::<rusqlite::Result<Vec<_>>>()?;
                rows.reverse(); rows
            };
            while events.first().is_some_and(|e|e.role!="user") {events.remove(0);}
            events.push(Event{id:request.clone(),role:"user".into(),content:prompt.clone()});
            if tx.execute(sql::CLAIM,params![request,now()])?!=1 {bail!("recording was not captured");}
            tx.execute(sql::EVENT,params![request,"generation_started",now()])?;
            tx.commit()?;
            Ok(Some(Generation{request,session,scope,model,prompt,events}))
        }).await
    }
    pub async fn save_recording_context(&self, request: String, context: Value) -> Result<()> {
        self.run(move |c| {
            let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
            if tx.execute(sql::CONTEXT, params![request, context.to_string(), now()])? != 1 {
                bail!("context cannot be saved in this state");
            }
            tx.execute(sql::EVENT, params![request, "context_saved", now()])?;
            tx.commit()?;
            Ok(())
        })
        .await
    }
    pub async fn complete_recording(&self, request: String, answer: String) -> Result<()> {
        self.run(move |c| {
            let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let session: String = tx.query_row(
                "SELECT session_id FROM chat_receipts WHERE request_id=?1 AND state='generating'",
                [&request],
                |r| r.get(0),
            )?;
            let answer_id = uid();
            let stamp = now();
            if tx.execute(sql::COMPLETE_USER, params![request])? != 1 {
                bail!("user message not pending");
            }
            tx.execute(sql::ANSWER, params![answer_id, session, answer, stamp])?;
            if tx.execute(sql::COMPLETE, params![request, answer_id, stamp])? != 1 {
                bail!("generation not ready to complete");
            }
            tx.execute(sql::EVENT, params![request, "answer_saved", stamp])?;
            // The activity feed is written here so a saved answer and its `answer_saved` row
            // can never disagree about whether this turn finished.
            tx.execute(
                agentic::EVENT,
                params![
                    request,
                    session,
                    None::<String>,
                    "answer_saved",
                    "{}",
                    stamp
                ],
            )?;
            // No job insert here. A full/failed extraction queue cannot undo this answer.
            tx.commit()?;
            Ok(())
        })
        .await
    }
    pub async fn fail_recording(&self, request: String, code: &'static str) -> Result<()> {
        if ![
            "context_failed",
            "provider_failed",
            "generation_stream_save_failed",
            "answer_save_failed",
            "worker_failed",
        ]
        .contains(&code)
        {
            bail!("invalid failure code");
        }
        self.run(move |c| {
            let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let stamp = now();
            if tx.execute(sql::FAIL, params![request, code, stamp])? == 1 {
                tx.execute(sql::FAIL_MESSAGE, [&request])?;
                tx.execute(sql::EVENT, params![request, "generation_failed", stamp])?;
                tx.execute(
                    "INSERT INTO generation_events(request_id,session_id,state,content,error_code,created_at) SELECT request_id,session_id,'failed','',?2,?3 FROM chat_receipts WHERE request_id=?1",
                    params![request, code, stamp],
                )?;
                // Every failure path, including the worker's panic guard, lands in the feed.
                let session: Option<String> = tx
                    .query_row(agentic::SESSION_OF_REQUEST, [&request], |r| r.get(0))
                    .optional()?;
                if let Some(session) = session {
                    tx.execute(
                        agentic::EVENT,
                        params![
                            request,
                            session,
                            None::<String>,
                            "turn_failed",
                            json!({"error_code":code}).to_string(),
                            stamp
                        ],
                    )?;
                }
            }
            tx.commit()?;
            Ok(())
        })
        .await
    }
    pub async fn flush_recording_outbox(&self) -> Result<usize> {
        self.run(|c| {
            let tx=c.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let queued:i64=tx.query_row("SELECT count(*) FROM jobs WHERE status IN ('pending','running')",[],|r|r.get(0))?;
            let capacity=(1000-queued).clamp(0,32);
            let rows={
                let mut stmt=tx.prepare("SELECT o.request_id,r.scope,r.session_id,m.content FROM recording_outbox o JOIN chat_receipts r ON r.request_id=o.request_id JOIN messages m ON m.id=o.request_id WHERE o.job_id IS NULL AND r.state IN ('complete','failed','interrupted') ORDER BY m.seq LIMIT ?1")?;
                let rows=stmt.query_map([capacity],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?)))?.collect::<rusqlite::Result<Vec<_>>>()?; rows
            };
            for (request,scope,session,prompt) in &rows {
                let job=uid();let source=format!("chat:{request}");let stamp=now();
                let plan={
                    let mut stmt=tx.prepare("SELECT seq,status,text FROM plan_items WHERE session_id=?1 ORDER BY seq")?;
                    let collected=stmt.query_map([session],|r|Ok(json!({"seq":r.get::<_,i64>(0)?,"status":r.get::<_,String>(1)?,"text":r.get::<_,String>(2)?})))?.collect::<rusqlite::Result<Vec<_>>>()?;
                    collected
                };
                let mut events=vec![Event{id:request.clone(),role:"user".into(),content:prompt.clone()}];
                if !plan.is_empty(){events.push(Event{id:format!("plan:{request}"),role:"plan".into(),content:serde_json::to_string(&plan)?});}
                tx.execute(sql::ENQUEUE,params![job,source,scope,source,serde_json::to_string(&events)?,chrono::Utc::now().timestamp(),stamp])?;
                if tx.execute(sql::LINK_JOB,params![request,job])?!=1 {bail!("outbox already dispatched");}
                tx.execute(sql::EVENT,params![request,"extraction_queued",stamp])?;
            }
            let count=rows.len();tx.commit()?;Ok(count)
        }).await
    }
    pub async fn history(&self, session: String, before: Option<i64>) -> Result<Value> {
        self.run(move|c| {
            let scope:Option<String>=c.query_row("SELECT scope FROM sessions WHERE id=?1",[&session],|r|r.get(0)).optional()?;
            let mut stmt=c.prepare("SELECT m.seq,m.id,m.role,m.content,m.status,r.state,COALESCE(r.request_id,a.request_id) FROM messages m LEFT JOIN chat_receipts r ON r.request_id=m.id LEFT JOIN chat_receipts a ON a.answer_id=m.id WHERE m.session_id=?1 AND m.seq<?2 ORDER BY m.seq DESC LIMIT 101")?;
            let mut rows=stmt.query_map(params![session,before.unwrap_or(i64::MAX)],|r|Ok(json!({"seq":r.get::<_,i64>(0)?,"id":r.get::<_,String>(1)?,"role":r.get::<_,String>(2)?,"content":r.get::<_,String>(3)?,"status":r.get::<_,String>(4)?,"generation_state":r.get::<_,Option<String>>(5)?,"request_id":r.get::<_,Option<String>>(6)?})))?.collect::<rusqlite::Result<Vec<_>>>()?;
            let more=rows.len()>100;rows.truncate(100);rows.reverse();
            let cursor=if more {rows.first().and_then(|r|r["seq"].as_i64())} else {None};
            Ok(json!({"scope":scope,"messages":rows,"has_more":more,"next_before_seq":cursor}))
        }).await
    }
    pub async fn recorded_sessions(&self, before: Option<i64>) -> Result<Value> {
        self.run(move|c| {
            let mut stmt=c.prepare("SELECT s.id,s.scope,s.created_at,MAX(m.seq),COUNT(m.id),COALESCE((SELECT substr(content,1,100) FROM messages WHERE session_id=s.id AND role='user' ORDER BY seq LIMIT 1),'Conversation') FROM sessions s JOIN messages m ON m.session_id=s.id GROUP BY s.id HAVING MAX(m.seq)<?1 ORDER BY MAX(m.seq) DESC LIMIT 51")?;
            let mut rows=stmt.query_map([before.unwrap_or(i64::MAX)],|r|Ok(json!({"id":r.get::<_,String>(0)?,"scope":r.get::<_,String>(1)?,"created_at":r.get::<_,String>(2)?,"last_seq":r.get::<_,i64>(3)?,"message_count":r.get::<_,i64>(4)?,"title":r.get::<_,String>(5)?})))?.collect::<rusqlite::Result<Vec<_>>>()?;
            let more=rows.len()>50;rows.truncate(50);
            let cursor=if more {rows.last().and_then(|r|r["last_seq"].as_i64())} else {None};
            Ok(json!({"sessions":rows,"has_more":more,"next_before_seq":cursor}))
        }).await
    }
}

/// Prepare the turn, persist the window once, then hand the conversation to the agent loop.
/// This function owns the receipt state machine; `agent_loop` owns steps, tools and events.
pub(crate) async fn generate(
    store: &DbStore,
    agents: &MemoryAgents,
    turn: Generation,
) -> Result<()> {
    let recalled: Vec<Recall> = match store.recall(turn.scope.clone(), turn.prompt.clone()).await {
        Ok(r) => r,
        Err(_) => return store.fail_recording(turn.request, "context_failed").await,
    };
    // The scope decides whether this turn has tools at all (P1-T04). Without a configured
    // `root_path` the turn stays on the text-only path instead of guessing a project root.
    let scope = match store.scope_config(turn.scope.clone()).await {
        Ok(found) => found.unwrap_or_else(|| ScopeConfig::blank(&turn.scope)),
        Err(_) => return store.fail_recording(turn.request, "context_failed").await,
    };
    // Both project-derived producers run in one blocking hop: the bounded repository map (P3-T04)
    // and the skills index (P5-T02). Either one failing is `context_failed`, because a turn that
    // silently dropped them would look identical to a project that has neither.
    let (repo_parts, skill_parts) = if let Some(root) = scope.root_path.clone() {
        let produced = tokio::task::spawn_blocking(move || {
            let root = std::path::Path::new(&root);
            let map = crate::repo_map::load_or_refresh(root)?;
            Ok::<_, anyhow::Error>((map, crate::skills::index_parts(root)?))
        })
        .await;
        match produced {
            Ok(Ok((map, skills))) => (
                vec![context::NamedPart {
                    id: map.id,
                    text: map.text,
                }],
                skills
                    .into_iter()
                    .map(|(id, text)| context::NamedPart { id, text })
                    .collect::<Vec<_>>(),
            ),
            _ => return store.fail_recording(turn.request, "context_failed").await,
        }
    } else {
        (Vec::new(), Vec::new())
    };
    let plan = match store.plan(turn.session.clone()).await {
        Ok(plan) => plan,
        Err(_) => return store.fail_recording(turn.request, "context_failed").await,
    };
    let offered_tools = if scope.root_path.is_some() {
        match Registry::standard().schemas() {
            Ok(tools) => tools,
            Err(_) => return store.fail_recording(turn.request, "context_failed").await,
        }
    } else {
        Vec::new()
    };
    let Some((user_message, recent_steps)) = turn.events.split_last() else {
        return store.fail_recording(turn.request, "context_failed").await;
    };
    if user_message.id != turn.request {
        return store.fail_recording(turn.request, "context_failed").await;
    }
    let sources = context::Sources {
        skills_index: skill_parts,
        repo_map: repo_parts,
        ..context::Sources::default()
    };
    let built = match context::build(context::BuildInput {
        scope: &scope,
        tools: &offered_tools,
        sources: &sources,
        memories: &recalled,
        plan: &plan,
        recent_steps,
        user_message,
        budgets: context::Budgets::default(),
    }) {
        Ok(window) => window,
        Err(_) => return store.fail_recording(turn.request, "context_failed").await,
    };
    let context::Window {
        messages,
        tools,
        memories,
        receipt: context_receipt,
    } = built;
    let agentic_turn = !tools.is_empty();
    let receipt = json!({"format_version":2,"adapter":if agentic_turn {"tool_calls_v1"} else {"text_completion_v1"},
        "model":turn.model,"provider_messages":messages.clone(),"provider_tools":tools.clone(),"memories":memories,
        "context_receipt":context_receipt,
        "scope":{"root_path":scope.root_path.clone(),"permission_mode":scope.permission_mode.clone(),"diagnostics_cmd":scope.diagnostics_cmd.clone(),"tools_enabled":agentic_turn},
        "note":"Exact sanitized message and tool arrays prepared for the provider's FIRST call in this turn, not model reasoning or proof of provider receipt. Later calls append tool results; each one stores its own full message array and tool names in turn_steps.input_json."});
    if store
        .save_recording_context(turn.request.clone(), receipt)
        .await
        .is_err()
    {
        return store.fail_recording(turn.request, "context_failed").await;
    }
    // No provider call is allowed before context persistence succeeds.
    let outcome = agent_loop::run(agent_loop::Turn {
        store,
        agents,
        request: turn.request.clone(),
        session: turn.session.clone(),
        model: turn.model.clone(),
        scope,
        messages,
        tools,
    })
    .await?;
    let answer = match outcome {
        agent_loop::Outcome::Answer(text) => safety::redact(&text),
        agent_loop::Outcome::ProviderFailed => {
            return store.fail_recording(turn.request, "provider_failed").await
        }
    };
    // Persist the generation payload before the completed receipt becomes visible. The stream
    // transport replays these durable rows instead of depending on an in-memory provider socket.
    if store
        .append_generation(
            turn.request.clone(),
            turn.session.clone(),
            "chunk".into(),
            answer.clone(),
            None,
        )
        .await
        .is_err()
    {
        return store
            .fail_recording(turn.request, "generation_stream_save_failed")
            .await;
    }
    if store
        .append_generation(
            turn.request.clone(),
            turn.session.clone(),
            "completed".into(),
            "".into(),
            None,
        )
        .await
        .is_err()
    {
        return store
            .fail_recording(turn.request, "generation_stream_save_failed")
            .await;
    }
    if store
        .complete_recording(turn.request.clone(), answer)
        .await
        .is_err()
    {
        return store
            .fail_recording(turn.request, "answer_save_failed")
            .await;
    }
    Ok(())
}

pub async fn worker(store: DbStore, agents: MemoryAgents) {
    loop {
        match store.claim_recording().await {
            Ok(Some(turn)) => {
                let id = turn.request.clone();
                let db = store.clone();
                let provider = agents.clone();
                // Observe panics as well as returned errors; no HTTP request owns this work.
                let result =
                    tokio::spawn(async move { generate(&db, &provider, turn).await }).await;
                if !matches!(result, Ok(Ok(()))) {
                    eprintln!("{{\"event\":\"generation_worker_failed\"}}");
                    if store.fail_recording(id, "worker_failed").await.is_err() {
                        eprintln!("{{\"event\":\"recording_failure_persistence_failed\"}}");
                        // Stop generation rather than process newer turns after an unknown save.
                        return;
                    }
                }
            }
            Ok(None) => tokio::time::sleep(std::time::Duration::from_millis(200)).await,
            Err(_) => {
                eprintln!("{{\"event\":\"recording_claim_failed\"}}");
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            }
        }
    }
}
