//! The agentic turn loop: docs/design/agentic-turn.md#loop.
//!
//! Two rules shape everything here.
//! 1. A step row is committed *before* the side effect it describes and finished *before* its
//!    output can influence anything else, so a crash leaves a readable half-turn instead of an
//!    invisible one, and a restart can honestly report `interrupted`.
//! 2. Tools never touch the database. They return `Artifact`s, and this module persists them in
//!    the same transaction that finishes the step, so an applied edit and its audit row cannot
//!    drift apart.
use crate::{
    memory_agents::{self, GenerationSink, MemoryAgents, ModelTurn, ToolCall},
    safety,
    storage::{DbStore, EffectOutcome, ScopeConfig},
    subagent,
    tools::{Artifact, PermissionMode, Registry, ToolResult, ToolStatus, MAX_OUTPUT},
};
use anyhow::Result;
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};

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

mod compaction;
mod steps;
mod verification;

use compaction::*;
pub use steps::{NewPermission, NewStep, Resolution, StepOutcome};
use verification::*;

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
    /// Exact definitions already budgeted and stored in the immutable initial context receipt.
    pub tools: Vec<Value>,
    pub observer: crate::runtime_observability::RuntimeObserver,
}

/// A finished turn. `ProviderFailed` is distinct from an `Err` so the caller can record the
/// honest reason instead of blaming the provider for a storage failure.
pub enum Outcome {
    Answer(String),
    ProviderFailed,
}

struct CompletedToolCall {
    step_id: String,
    result: ToolResult,
}

/// P5-T03: what a delegated exploration spent. A sub-agent draws from the budget of the turn
/// that spawned it, so the parent adds these to its own counters before deciding to continue.
struct Delegated {
    completed: CompletedToolCall,
    model_calls: i64,
    tool_bytes: i64,
}

struct Ctx<'a> {
    store: &'a DbStore,
    agents: &'a MemoryAgents,
    request: String,
    session: String,
    model: String,
    scope: ScopeConfig,
    registry: Arc<Registry>,
    mode: PermissionMode,
    observer: crate::runtime_observability::RuntimeObserver,
}

/// `sink` receives redacted answer text as it becomes publishable. Its accumulated `text()` is
/// the turn's answer, so a sink that published incrementally reports exactly what it published.
/// P14-T01: how often a provider wait re-checks the durable cancel intent.
const PROVIDER_CANCEL_POLL: Duration = Duration::from_millis(200);

/// The outcome of racing a provider future against the durable cancel intent.
enum Raced<T> {
    Done(T),
    Cancelled,
}

pub async fn run<S: GenerationSink>(turn: Turn<'_>, sink: &mut S) -> Result<Outcome> {
    let Turn {
        store,
        agents,
        request,
        session,
        model,
        scope,
        messages,
        tools,
        observer,
    } = turn;
    let ctx = Ctx {
        store,
        agents,
        mode: scope.mode(),
        observer,
        registry: Arc::new(Registry::standard()),
        request,
        session,
        model,
        scope,
    };
    // The context manager, receipt, and provider must share one exact definition array. Never
    // recreate or widen it after the receipt's write-once commit.
    let mut tools = tools;
    let (max_steps, max_tool_bytes, max_wall) = ctx.scope.budgets();
    let started = Instant::now();
    let mut messages = messages;
    let base_messages = messages.len();
    let context_tokens = context_token_budget();
    let mut observed_prompt_tokens = None::<u64>;
    let mut steps: i64 = 0;
    let mut tool_bytes: i64 = 0;
    let mut durable_seq: i64 = 0;
    let mut replays = Vec::<ToolReplay>::new();
    let mut read_cache = HashMap::<String, CachedRead>::new();
    let mut verification_evidence = Vec::<VerificationEvidence>::new();
    ctx.event("turn_started", json!({"model":ctx.model,"tool_count":tools.len(),
        "permission_mode":ctx.mode.as_str(),"max_steps":max_steps,"max_tool_bytes":max_tool_bytes,"max_wall_seconds":max_wall})).await?;
    // P1-T15: a tool-less turn is a configuration state, not a mystery. Record it once, next to
    // `turn_started`, so the UI and `/activity` can say why the agent only talked.
    if ctx.scope.root_path.is_none() {
        ctx.event("tools_withheld", json!({"scope":ctx.scope.scope,"reason":"no_root_path",
            "hint":"set a project root for this scope in Project & models -> Project scope settings, or POST /scopes/{scope}"})).await?;
    }

    loop {
        if ctx.cancelled().await? {
            // The cancel endpoint already signalled any live group; this covers the cooperative
            // path where the loop itself observes the intent, and is a no-op with nothing live.
            crate::processes::terminate(&ctx.request);
            return Ok(Outcome::Answer(String::new()));
        }
        let elapsed = started.elapsed().as_secs() as i64;
        if let Some(reason) = exhausted(
            steps,
            max_steps,
            tool_bytes,
            max_tool_bytes,
            elapsed,
            max_wall,
        ) {
            ctx.event("budget_exhausted", json!({"reason":reason,"steps":steps,"tool_bytes":tool_bytes,"elapsed_seconds":elapsed})).await?;
            return Ok(Outcome::Answer(budget_message(
                reason, steps, tool_bytes, elapsed,
            )));
        }

        compact_old_tool_results(&mut messages, &mut replays, steps);
        if should_compact(observed_prompt_tokens, context_tokens) {
            if let Some(split) = compaction_split(&messages, base_messages) {
                let source = messages[base_messages..split].to_vec();
                let source_json = serde_json::to_vec(&source)?;
                let source_hash =
                    crate::tools::content_hash(&String::from_utf8_lossy(&source_json));
                let mut preserved_tool_steps = replays
                    .iter()
                    .filter(|r| r.message_index >= split)
                    .map(|r| r.step_seq)
                    .collect::<Vec<_>>();
                preserved_tool_steps.sort_unstable();
                preserved_tool_steps.dedup();
                let compaction_model = ctx.store.role_model("compaction", &ctx.model).await?;
                durable_seq += 1;
                let compact_step = ctx.store.begin_step(NewStep {
                    request:ctx.request.clone(),session:ctx.session.clone(),kind:"compaction",
                    tool_name:None,tool_call_id:None,
                    input:json!({"model":compaction_model,"source_messages":source,"receipt":{
                        "trigger_percent":COMPACTION_PERCENT,"observed_prompt_tokens":observed_prompt_tokens,
                        "context_token_budget":context_tokens,"source_bytes":source_json.len(),
                        "source_hash":source_hash,"preserved_base_messages":base_messages,
                        "preserved_tool_steps":preserved_tool_steps}}),
                    event:"compaction_started",payload:json!({"observed_prompt_tokens":observed_prompt_tokens,
                        "context_token_budget":context_tokens,"trigger_percent":COMPACTION_PERCENT}),
                }).await?;
                let compacted = match ctx.agents.compact(&compaction_model, &source).await {
                    Ok(turn) => turn,
                    Err(error) => {
                        let mut failed = ctx.outcome(
                            compact_step,
                            "failed",
                            json!({"error":safety::redact(&error.to_string())}),
                            "compaction_finished",
                            json!({"status":"failed"}),
                        );
                        failed.error_code = Some("compaction_failed".into());
                        ctx.store.finish_step(failed).await?;
                        return Ok(Outcome::ProviderFailed);
                    }
                };
                let summary = clipped_summary(&safety::redact(
                    compacted.text.as_deref().unwrap_or_default(),
                ));
                if summary.is_empty() {
                    let mut failed = ctx.outcome(
                        compact_step,
                        "failed",
                        json!({"error":"empty compaction summary"}),
                        "compaction_finished",
                        json!({"status":"failed"}),
                    );
                    failed.error_code = Some("compaction_failed".into());
                    ctx.store.finish_step(failed).await?;
                    return Ok(Outcome::ProviderFailed);
                }
                let candidate_id = ctx
                    .store
                    .save_compaction_candidate(
                        ctx.scope.scope.clone(),
                        ctx.request.clone(),
                        compact_step.clone(),
                        summary.clone(),
                    )
                    .await?;
                let mut finished=ctx.outcome(compact_step,"complete",json!({"summary":summary,"candidate_id":candidate_id,
                    "receipt":{"trigger_percent":COMPACTION_PERCENT,"observed_prompt_tokens":observed_prompt_tokens,
                    "context_token_budget":context_tokens,"source_bytes":source_json.len(),"source_hash":source_hash,
                    "preserved_tool_steps":preserved_tool_steps}}),"compaction_finished",
                    json!({"status":"complete","candidate_id":candidate_id,"source_messages":source.len()}));
                finished.bytes = summary.len() as i64;
                finished.tokens_in = compacted.usage.prompt_tokens.map(|v| v as i64);
                finished.tokens_out = compacted.usage.completion_tokens.map(|v| v as i64);
                ctx.store.finish_step(finished).await?;

                let kept = messages[split..].to_vec();
                messages.truncate(base_messages);
                messages.push(compacted_history_message(&summary));
                messages.extend(kept);
                replays.retain(|r| r.message_index >= split);
                for replay in &mut replays {
                    replay.message_index = base_messages + 1 + (replay.message_index - split);
                }
                observed_prompt_tokens = None;
            }
        }

        // The full array sent on this call is stored with the step; the receipt keeps the exact
        // first window and full definitions. Tool names identify that immutable definition set.
        durable_seq += 1;
        let step = ctx.store.begin_step(NewStep {
            request: ctx.request.clone(), session: ctx.session.clone(), kind: "model_call",
            tool_name: None, tool_call_id: None,
            input: json!({"messages":messages,"tools":tool_names(&tools)}),
            event: "model_call_started", payload: json!({"attempt":steps+1,"messages":messages.len(),"tool_count":tools.len()}),
        }).await?;

        let provider_started = Instant::now();
        let replied = if tools.is_empty() {
            // P14-T01: a provider call can outlast a cancel request, so the wait is raced against
            // the durable intent. Dropping the abandoned future aborts the in-flight request
            // instead of letting a stopped turn keep consuming provider work and then committing.
            let streamed = async {
                ctx.agents
                    .stream_turn(&ctx.model, messages.clone(), sink)
                    .await?;
                let text = sink.text().to_string();
                let usage = sink.usage().cloned().unwrap_or_default();
                Ok::<ModelTurn, anyhow::Error>(ModelTurn {
                    assistant_message: json!({"role":"assistant","content":text}),
                    text: Some(text),
                    tool_calls: Vec::new(),
                    usage,
                })
            };
            match ctx.race_cancellation(streamed).await? {
                Raced::Cancelled => {
                    ctx.finish_cancelled_step(step).await?;
                    return Ok(Outcome::Answer(String::new()));
                }
                Raced::Done(replied) => replied,
            }
        } else {
            let called =
                ctx.agents
                    .complete_with_tools(&ctx.model, messages.clone(), tools.clone());
            match ctx.race_cancellation(called).await? {
                Raced::Cancelled => {
                    ctx.finish_cancelled_step(step).await?;
                    return Ok(Outcome::Answer(String::new()));
                }
                Raced::Done(replied) => replied,
            }
        };
        ctx.observer.answer_provider(provider_started.elapsed());
        if ctx.cancelled().await? {
            ctx.finish_cancelled_step(step).await?;
            return Ok(Outcome::Answer(String::new()));
        }
        steps += 1;
        let reply = match replied {
            Ok(reply) => reply,
            Err(error) => {
                // A provider that rejects `tools` must not make this release worse than the
                // text-only one it replaces: drop the tools and answer from text alone.
                let unsupported = !tools.is_empty() && memory_agents::is_tools_unsupported(&error);
                let code = if unsupported {
                    "tools_unsupported"
                } else {
                    "provider_failed"
                };
                let mut outcome = ctx.outcome(
                    step,
                    "failed",
                    json!({"error":safety::redact(&error.to_string())}),
                    "model_call_finished",
                    json!({"error_code":code}),
                );
                outcome.error_code = Some(code.to_string());
                ctx.store.finish_step(outcome).await?;
                if unsupported {
                    tools.clear();
                    continue;
                }
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
        observed_prompt_tokens = reply.usage.prompt_tokens;

        if reply.tool_calls.is_empty() {
            let text = reply
                .text
                .map(|t| t.trim().to_string())
                .filter(|t| !t.is_empty());
            let answer = safety::redact(&text.unwrap_or_else(|| NO_TEXT.to_string()));
            ctx.verify_answer(&answer, &verification_evidence).await?;
            return Ok(Outcome::Answer(answer));
        }
        // Replayed verbatim: some providers reject a tool result whose call is missing.
        messages.push(reply.assistant_message.clone());
        let deadline = started + Duration::from_secs(max_wall.max(0) as u64);
        for call in &reply.tool_calls {
            if ctx.cancelled().await? {
                return Ok(Outcome::Answer(String::new()));
            }
            durable_seq += 1;
            let tool_seq = durable_seq;
            // P5-T03: `task` is offered like any other tool but cannot run through the registry,
            // because a sub-agent needs provider calls and steps of its own. It spends this
            // turn's budget, so what it used lands on the same counters before the next pass.
            let completed = if call.name == "task" {
                let delegated = ctx
                    .run_task(
                        call,
                        &tools,
                        (max_steps - steps, max_tool_bytes - tool_bytes),
                        deadline,
                    )
                    .await?;
                steps += delegated.model_calls;
                tool_bytes += delegated.tool_bytes;
                delegated.completed
            } else {
                ctx.run_call(call, deadline).await?
            };
            let result = completed.result;
            verification_evidence.push(tool_verification_evidence(
                completed.step_id,
                tool_seq,
                call,
                &result,
            ));
            tool_bytes += result.bytes as i64;
            let hash = crate::tools::content_hash(&result.content);
            let cache_key = read_cache_key(call, &result);
            let provider_content = cache_key
                .as_ref()
                .and_then(|key| read_cache.get(key))
                .map(|cached| tool_reference("read", cached.step_seq, cached.bytes, &cached.hash))
                .unwrap_or_else(|| result.content.clone());
            if let Some(key) = cache_key {
                read_cache.entry(key).or_insert_with(|| CachedRead {
                    step_seq: tool_seq,
                    bytes: result.bytes,
                    hash: hash.clone(),
                });
            }
            let message_index = messages.len();
            messages.push(json!({"role":"tool","tool_call_id":call.id,"content":provider_content}));
            replays.push(ToolReplay {
                message_index,
                name: call.name.clone(),
                step_seq: tool_seq,
                bytes: result.bytes,
                hash,
                produced_after_call: steps,
                compacted: provider_content != result.content,
            });
        }
    }
}

impl Ctx<'_> {
    async fn cancelled(&self) -> Result<bool> {
        self.store
            .cancellation_requested(self.request.clone())
            .await
    }

    /// P14-T01: wait for a provider future while polling the durable cancel intent. Returns
    /// `Cancelled` as soon as the intent is observed; the caller then drops the future, which
    /// aborts the in-flight provider request rather than waiting for it to finish.
    async fn race_cancellation<F, T>(&self, fut: F) -> Result<Raced<T>>
    where
        F: std::future::Future<Output = T>,
    {
        tokio::pin!(fut);
        loop {
            if self.cancelled().await? {
                return Ok(Raced::Cancelled);
            }
            tokio::select! {
                value = &mut fut => return Ok(Raced::Done(value)),
                _ = tokio::time::sleep(PROVIDER_CANCEL_POLL) => {}
            }
        }
    }

    /// Close the in-flight model-call step as `interrupted`, so a cancelled turn never leaves a
    /// `running` step behind (which would read as live work and violate the restart invariant).
    async fn finish_cancelled_step(&self, step: String) -> Result<()> {
        let mut outcome = self.outcome(
            step,
            "interrupted",
            json!({"error_code":"cancelled"}),
            "model_call_finished",
            json!({"status":"interrupted","error_code":"cancelled"}),
        );
        outcome.error_code = Some("cancelled".into());
        self.store.finish_step(outcome).await
    }

    async fn event(&self, kind: &'static str, payload: Value) -> Result<()> {
        self.store
            .activity(self.request.clone(), self.session.clone(), kind, payload)
            .await
    }
    fn outcome(
        &self,
        step: String,
        status: &'static str,
        output: Value,
        event: &'static str,
        payload: Value,
    ) -> StepOutcome {
        StepOutcome {
            step,
            request: self.request.clone(),
            session: self.session.clone(),
            status,
            output,
            bytes: 0,
            truncated: false,
            tokens_in: None,
            tokens_out: None,
            error_code: None,
            event,
            payload,
            artifacts: Vec::new(),
        }
    }

    async fn verify_answer(&self, answer: &str, evidence: &[VerificationEvidence]) -> Result<()> {
        let verification_started = Instant::now();
        let selected = select_verification_evidence(evidence);
        let evidence_step_ids = selected
            .iter()
            .map(|item| item.step_id.clone())
            .collect::<Vec<_>>();
        let manifest = json!({
            "steps":selected.iter().map(|item|item.value.clone()).collect::<Vec<_>>(),
            "total_tool_steps":evidence.len(),
            "omitted_tool_steps":evidence.len().saturating_sub(selected.len()),
            "selection":"recent file changes and bash results first, then recent tool results",
        });
        let bounded_answer = verification_text(answer, MAX_VERIFICATION_ANSWER_CHARS);
        let verification_model = self.store.role_model("verification", &self.model).await?;
        let step=self.store.begin_step(NewStep {
            request:self.request.clone(),session:self.session.clone(),kind:"verification",
            tool_name:None,tool_call_id:None,
            input:json!({"model":verification_model,"answer":bounded_answer,"evidence_manifest":manifest}),
            event:"verification_started",payload:json!({"model":verification_model,"evidence_steps":evidence_step_ids.len()}),
        }).await?;
        let provider_started = Instant::now();
        let verified = self
            .agents
            .verify(
                &verification_model,
                &bounded_answer,
                manifest,
                &evidence_step_ids,
            )
            .await;
        self.observer
            .verification_provider(provider_started.elapsed());
        match verified {
            Ok(verified) => {
                let claim_count = verified.report.claims.len();
                let unverified_claims = verified
                    .report
                    .claims
                    .iter()
                    .filter(|claim| claim.status == memory_agents::VerificationStatus::Unverified)
                    .count();
                let status = if claim_count == 0 {
                    "skipped"
                } else if unverified_claims > 0 {
                    "unverified"
                } else {
                    "verified"
                };
                let output = json!({"status":status,"claims":verified.report.claims,
                    "skipped_diagnostics":verified.report.skipped_diagnostics,"model":verification_model,
                    "evidence_step_ids":evidence_step_ids});
                let mut outcome=self.outcome(step,"complete",output,"verified",
                    json!({"status":status,"unverified_claims":unverified_claims,"claim_count":claim_count}));
                outcome.bytes = outcome.output.to_string().len() as i64;
                outcome.tokens_in = verified.usage.prompt_tokens.map(|value| value as i64);
                outcome.tokens_out = verified.usage.completion_tokens.map(|value| value as i64);
                self.store.finish_step(outcome).await?;
            }
            Err(error) => {
                let error = safety::redact(&error.to_string());
                let mut outcome=self.outcome(step,"failed",json!({"status":"unavailable","claims":[],
                    "skipped_diagnostics":[],"model":verification_model,"error":error}),"verified",
                    json!({"status":"unavailable","unverified_claims":Value::Null,"error_code":"verification_failed"}));
                outcome.bytes = outcome.output.to_string().len() as i64;
                outcome.error_code = Some("verification_failed".into());
                self.store.finish_step(outcome).await?;
            }
        }
        self.observer.verification(verification_started.elapsed());
        Ok(())
    }

    /// One tool call: commit the step, gate it, run it, commit the result. Every exit path
    /// finishes the step and returns something the model can read.
    async fn run_call(&self, call: &ToolCall, deadline: Instant) -> Result<CompletedToolCall> {
        let parsed = call.arguments();
        let args = parsed
            .as_ref()
            .ok()
            .filter(|value| value.is_object())
            .cloned();
        let summary = match (self.registry.get(&call.name), &args) {
            (Some(tool), Some(args)) => tool.summary(args),
            _ => call.name.clone(),
        };
        let step = self
            .store
            .begin_step(NewStep {
                request: self.request.clone(),
                session: self.session.clone(),
                kind: "tool_call",
                tool_name: Some(call.name.clone()),
                tool_call_id: Some(call.id.clone()),
                input: args.clone().unwrap_or_else(
                    || json!({"unparsed_arguments":crate::safety::redact(&call.arguments_json)}),
                ),
                event: "tool_started",
                payload: json!({"tool":call.name,"summary":summary}),
            })
            .await?;

        // Arguments the adapter could not read never reach a tool.
        let Some(args) = args else {
            let detail = match parsed {
                Err(error) => error.to_string(),
                Ok(_) => "tool-call arguments must be a JSON object".to_string(),
            };
            return self
                .finish_tool(
                    step,
                    call,
                    ToolResult::err("invalid_arguments", detail),
                    None,
                )
                .await;
        };

        let tool_ctx = self.scope.tool_ctx(&self.request, &step);
        let gate = self
            .registry
            .get(&call.name)
            .zip(tool_ctx.as_ref())
            .filter(|(tool, _)| self.registry.requires_permission(*tool, &args, self.mode))
            .map(|(tool, ctx)| (tool.summary(&args), tool.permission_payload(ctx, &args)));
        if let Some((prompt, payload)) = gate {
            let ttl =
                permission_ttl(deadline.saturating_duration_since(Instant::now()).as_secs() as i64);
            let permission = self
                .store
                .request_permission(NewPermission {
                    request: self.request.clone(),
                    session: self.session.clone(),
                    step: step.clone(),
                    tool: call.name.clone(),
                    summary: prompt,
                    args: payload,
                    ttl_seconds: ttl,
                })
                .await?;
            let permission_started = Instant::now();
            let permission_result = self.await_permission(&permission, &step, deadline).await?;
            self.observer.permission(permission_started.elapsed());
            if let Err(reason) = permission_result {
                // A denial is a tool error, not a dead turn: the model can adapt or ask.
                let cancelled = reason == "cancelled";
                let refusal = ToolResult::err(
                    if cancelled { "cancelled" } else { "denied" },
                    format!("`{}` was not approved: {reason}. Nothing was run and nothing changed on disk.", call.name),
                );
                return self
                    .finish_tool(
                        step,
                        call,
                        refusal,
                        Some(if cancelled { "interrupted" } else { "denied" }),
                    )
                    .await;
            }
        }

        if self.cancelled().await? {
            return self
                .finish_tool(
                    step,
                    call,
                    ToolResult::err("cancelled", "the request was cancelled before the tool ran"),
                    Some("interrupted"),
                )
                .await;
        }
        // P18-T04: commands and browser input can escape the database. Reserve their stable
        // turn/step/payload identity before dispatch so a crash or takeover cannot silently run
        // the same logical effect twice. Browser reads remain outside this ledger.
        let effect_kind = tool_effect_kind(&self.registry, &call.name, &args);
        let effect = if let Some(kind) = effect_kind {
            let digest =
                safety::fingerprint(&json!({"tool":call.name,"arguments":args}).to_string());
            Some(
                self.store
                    .reserve_external_effect(
                        self.request.clone(),
                        step.clone(),
                        digest,
                        kind.into(),
                        self.store.held_lease(&self.request),
                    )
                    .await?
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "side-effecting tool call on generating turn was not recorded"
                        )
                    })?,
            )
        } else {
            None
        };
        let registry = self.registry.clone();
        let name = call.name.clone();
        let tool_started = Instant::now();
        let invoked =
            tokio::task::spawn_blocking(move || registry.invoke(tool_ctx.as_ref(), &name, args))
                .await;
        self.observer.tool(tool_started.elapsed());
        let result = match invoked {
            Ok(result) => result,
            Err(error) => {
                if let Some(effect) = effect {
                    self.store
                        .settle_external_effect(
                            effect,
                            EffectOutcome::Unknown,
                            Some(step),
                            Some(format!(
                                "tool worker stopped before reporting an outcome: {error}"
                            )),
                        )
                        .await?;
                }
                return Err(error.into());
            }
        };
        if let Some(effect) = effect {
            let reason = result
                .error_code
                .map(|code| format!("tool returned {code}"));
            self.store
                .settle_external_effect(
                    effect,
                    EffectOutcome::Succeeded,
                    Some(step.clone()),
                    reason,
                )
                .await?;
        }
        self.finish_tool(step, call, result, None).await
    }

    /// P5-T03: one delegated read-only exploration. The `task` tool-call step is the parent of a
    /// `subagent` step, and the sub-agent's own model and tool steps hang off that, so the turn
    /// stays one ordered list that can still be read back as a tree.
    ///
    /// Three properties are enforced here rather than trusted to the sub-agent:
    /// - it is offered read-only tools only, so nothing inside it can ever raise an approval;
    /// - it draws from the parent's remaining budget and reports back what it spent;
    /// - the parent model receives a bounded report, never the sub-agent's transcript.
    async fn run_task(
        &self,
        call: &ToolCall,
        tools: &[Value],
        remaining: (i64, i64),
        deadline: Instant,
    ) -> Result<Delegated> {
        let (remaining_steps, remaining_tool_bytes) = remaining;
        let parsed = call.arguments();
        let args = parsed
            .as_ref()
            .ok()
            .filter(|value| value.is_object())
            .cloned();
        let asked = args.as_ref().map(subagent::parse);
        let summary = match &asked {
            Some(Ok(ask)) => subagent::label(ask),
            _ => call.name.clone(),
        };
        let step = self
            .store
            .begin_step(NewStep {
                request: self.request.clone(),
                session: self.session.clone(),
                kind: "tool_call",
                tool_name: Some(call.name.clone()),
                tool_call_id: Some(call.id.clone()),
                input: args.unwrap_or_else(
                    || json!({"unparsed_arguments":safety::redact(&call.arguments_json)}),
                ),
                event: "tool_started",
                payload: json!({"tool":call.name,"summary":summary}),
            })
            .await?;

        let ask = match asked {
            Some(Ok(ask)) => ask,
            Some(Err(detail)) => {
                return self
                    .refuse_delegation(step, call, ToolResult::err("invalid_arguments", detail))
                    .await
            }
            None => {
                let detail = match parsed {
                    Err(error) => error.to_string(),
                    Ok(_) => "tool-call arguments must be a JSON object".to_string(),
                };
                return self
                    .refuse_delegation(step, call, ToolResult::err("invalid_arguments", detail))
                    .await;
            }
        };
        // A turn whose tools were withheld or dropped has nothing read-only to delegate.
        let mut definitions = subagent::definitions(tools);
        if definitions.is_empty() {
            return self.refuse_delegation(step, call, ToolResult::err("tools_disabled",
                "no read-only tools are available to delegate; explore with read, grep and glob directly")).await;
        }

        let sub_step = self.store.begin_child_step(step.clone(), NewStep {
            request: self.request.clone(), session: self.session.clone(), kind: "subagent",
            tool_name: None, tool_call_id: None,
            input: json!({"description":ask.description,"prompt":ask.prompt,"tools":subagent::TOOLS,
                "max_model_calls":subagent::MAX_MODEL_CALLS,"remaining_steps":remaining_steps,
                "remaining_tool_bytes":remaining_tool_bytes}),
            event: "subagent_started", payload: json!({"description":ask.description,"tools":subagent::TOOLS}),
        }).await?;

        let mut messages = subagent::messages(&ask);
        let mut model_calls: i64 = 0;
        let mut tool_calls: i64 = 0;
        let mut tool_bytes: i64 = 0;
        let mut files = Vec::<String>::new();
        let mut summary_text = String::new();
        let mut stopped = None::<subagent::Stop>;
        loop {
            if model_calls >= subagent::MAX_MODEL_CALLS {
                stopped = Some(subagent::Stop::MaxModelCalls);
                break;
            }
            if model_calls >= remaining_steps {
                stopped = Some(subagent::Stop::TurnSteps);
                break;
            }
            if tool_bytes >= remaining_tool_bytes {
                stopped = Some(subagent::Stop::TurnToolBytes);
                break;
            }
            if Instant::now() >= deadline {
                stopped = Some(subagent::Stop::Deadline);
                break;
            }

            // P14-T01: a delegation draws provider calls of its own, so it must observe the same
            // durable cancel the parent loop does. Stop before the next call and report incomplete.
            if self.cancelled().await? {
                stopped = Some(subagent::Stop::Cancelled);
                break;
            }
            let model_step = self.store.begin_child_step(sub_step.clone(), NewStep {
                request: self.request.clone(), session: self.session.clone(), kind: "model_call",
                tool_name: None, tool_call_id: None,
                input: json!({"messages":messages,"tools":tool_names(&definitions)}),
                event: "model_call_started", payload: json!({"attempt":model_calls+1,"messages":messages.len(),"subagent":true}),
            }).await?;
            // P14-T01: a delegation's provider call can outlast a cancel exactly like the parent's,
            // so race it against the durable intent and drop the abandoned future. The child step is
            // closed `interrupted` on cancellation, and the post-response guard below catches a
            // cancel that lands while the reply was in flight, so a cancelled turn can never record
            // a completed final answer.
            let called =
                self.agents
                    .complete_with_tools(&self.model, messages.clone(), definitions.clone());
            let replied = match self.race_cancellation(called).await? {
                Raced::Cancelled => {
                    self.finish_cancelled_step(model_step).await?;
                    stopped = Some(subagent::Stop::Cancelled);
                    break;
                }
                Raced::Done(replied) => replied,
            };
            if self.cancelled().await? {
                self.finish_cancelled_step(model_step).await?;
                stopped = Some(subagent::Stop::Cancelled);
                break;
            }
            model_calls += 1;
            let reply = match replied {
                Ok(reply) => reply,
                Err(error) => {
                    // P14-T04b: the parent loop degrades to text when a provider rejects `tools`,
                    // so a delegation must not fail outright on the same provider and the same
                    // error. Drop the definitions once and answer from text alone. A repeat
                    // failure then has empty definitions and stops, so this cannot spin.
                    let unsupported =
                        !definitions.is_empty() && memory_agents::is_tools_unsupported(&error);
                    let code = if unsupported {
                        "tools_unsupported"
                    } else {
                        "provider_failed"
                    };
                    let mut outcome = self.outcome(
                        model_step,
                        "failed",
                        json!({"error":safety::redact(&error.to_string())}),
                        "model_call_finished",
                        json!({"error_code":code,"subagent":true}),
                    );
                    outcome.error_code = Some(code.to_string());
                    self.store.finish_step(outcome).await?;
                    if unsupported {
                        definitions.clear();
                        continue;
                    }
                    stopped = Some(subagent::Stop::ProviderFailed);
                    break;
                }
            };
            let tokens_in = reply.usage.prompt_tokens.map(|value| value as i64);
            let tokens_out = reply.usage.completion_tokens.map(|value| value as i64);
            let mut finished = self.outcome(model_step, "complete", json!({"text":reply.text,
                "tool_calls":reply.tool_calls.iter().map(|c| json!({"id":c.id,"name":c.name,"arguments":c.arguments_json})).collect::<Vec<_>>(),
                "usage":reply.usage}), "model_call_finished",
                json!({"tokens_in":tokens_in,"tokens_out":tokens_out,"tool_call_count":reply.tool_calls.len(),"subagent":true}));
            finished.bytes = finished.output.to_string().len() as i64;
            finished.tokens_in = tokens_in;
            finished.tokens_out = tokens_out;
            self.store.finish_step(finished).await?;

            // No tool call means the sub-agent is reporting: that text is the only thing kept.
            if reply.tool_calls.is_empty() {
                summary_text = safety::redact(reply.text.as_deref().unwrap_or_default());
                break;
            }
            messages.push(reply.assistant_message.clone());
            for sub_call in &reply.tool_calls {
                if self.cancelled().await? {
                    stopped = Some(subagent::Stop::Cancelled);
                    break;
                }
                let sub_args = sub_call.arguments().ok().filter(|value| value.is_object());
                let allowed = subagent::TOOLS.contains(&sub_call.name.as_str());
                let tool_summary = match (
                    allowed.then(|| self.registry.get(&sub_call.name)).flatten(),
                    &sub_args,
                ) {
                    (Some(tool), Some(args)) => tool.summary(args),
                    _ => sub_call.name.clone(),
                };
                let tool_step = self.store.begin_child_step(sub_step.clone(), NewStep {
                    request: self.request.clone(), session: self.session.clone(), kind: "tool_call",
                    tool_name: Some(sub_call.name.clone()), tool_call_id: Some(sub_call.id.clone()),
                    input: sub_args.clone().unwrap_or_else(|| json!({"unparsed_arguments":safety::redact(&sub_call.arguments_json)})),
                    event: "tool_started", payload: json!({"tool":sub_call.name,"summary":tool_summary,"subagent":true}),
                }).await?;
                let result = match (&sub_args, allowed) {
                    // The allow-list is enforced here, not by the schema the sub-agent was sent:
                    // a model that asks for `bash` is refused without the registry being reached.
                    (_, false) => ToolResult::err(
                        "unknown_tool",
                        format!(
                            "a sub-agent may only call {}; `{}` is not available to it",
                            subagent::TOOLS.join(", "),
                            sub_call.name
                        ),
                    ),
                    (None, true) => ToolResult::err(
                        "invalid_arguments",
                        "tool-call arguments must be a JSON object",
                    ),
                    (Some(args), true) => match self.scope.tool_ctx(&self.request, &tool_step) {
                        None => ToolResult::err(
                            "tools_disabled",
                            "this scope has no root_path; configure one before using tools",
                        ),
                        Some(tool_ctx) => {
                            subagent::record_file(&mut files, &sub_call.name, args);
                            let registry = self.registry.clone();
                            let name = sub_call.name.clone();
                            let args = args.clone();
                            tokio::task::spawn_blocking(move || {
                                registry.invoke(Some(&tool_ctx), &name, args)
                            })
                            .await?
                        }
                    },
                };
                tool_calls += 1;
                tool_bytes += result.bytes as i64;
                let completed = self.finish_tool(tool_step, sub_call, result, None).await?;
                messages.push(json!({"role":"tool","tool_call_id":sub_call.id,"content":completed.result.content}));
            }
        }

        let report = subagent::Report {
            summary: summary_text,
            files,
            model_calls,
            tool_calls,
            stopped,
        };
        let content = subagent::report_content(&report);
        let file_count = report.files.len();
        let status = match stopped {
            Some(subagent::Stop::ProviderFailed) => "failed",
            // A cancelled delegation is incomplete work, never a completed exploration.
            Some(subagent::Stop::Cancelled) => "interrupted",
            _ => "complete",
        };
        let mut outcome = self.outcome(sub_step, status,
            json!({"summary":report.summary.clone(),"files":report.files.clone(),"model_calls":model_calls,
                "tool_calls":tool_calls,"stopped":stopped.map(|stop| stop.as_str()),"report_bytes":content.len()}),
            "subagent_finished",
            json!({"status":status,"model_calls":model_calls,"tool_calls":tool_calls,"files":file_count,
                "stopped":stopped.map(|stop| stop.as_str())}));
        outcome.bytes = content.len() as i64;
        if status == "failed" {
            outcome.error_code = Some("provider_failed".into());
        }
        if status == "interrupted" {
            outcome.error_code = Some("cancelled".into());
        }
        self.store.finish_step(outcome).await?;

        let cancelled = matches!(stopped, Some(subagent::Stop::Cancelled));
        let completed = self
            .finish_tool(
                step,
                call,
                if cancelled {
                    ToolResult::err(
                        "cancelled",
                        "the request was cancelled while the sub-agent ran",
                    )
                } else {
                    ToolResult::ok(subagent::label(&ask), content)
                },
                if cancelled { Some("interrupted") } else { None },
            )
            .await?;
        Ok(Delegated {
            completed,
            model_calls,
            tool_bytes,
        })
    }

    /// A delegation that never started still owes the model a readable tool result.
    async fn refuse_delegation(
        &self,
        step: String,
        call: &ToolCall,
        result: ToolResult,
    ) -> Result<Delegated> {
        Ok(Delegated {
            completed: self.finish_tool(step, call, result, None).await?,
            model_calls: 0,
            tool_bytes: 0,
        })
    }

    /// Poll for a human decision. Bounded by the turn's wall budget as well as the row's own
    /// expiry, so a forgotten approval cannot hold a turn open past its budget.
    async fn await_permission(
        &self,
        permission: &str,
        step: &str,
        deadline: Instant,
    ) -> Result<std::result::Result<(), String>> {
        loop {
            if self.cancelled().await? {
                return Ok(Err("cancelled".into()));
            }
            match self
                .store
                .permission_status(permission.to_string())
                .await?
                .as_deref()
            {
                Some("approved") => return Ok(Ok(())),
                Some("denied") => return Ok(Err("the request was denied".into())),
                Some("expired") => {
                    return Ok(Err("the approval request had already expired".into()))
                }
                None => return Ok(Err("the approval request is no longer on record".into())),
                _ => {}
            }
            if Instant::now() >= deadline {
                let status = self
                    .store
                    .expire_permission(
                        permission.to_string(),
                        self.request.clone(),
                        self.session.clone(),
                        step.to_string(),
                    )
                    .await?;
                return Ok(if status == "approved" {
                    Ok(())
                } else {
                    Err("it timed out waiting for an approval".into())
                });
            }
            tokio::time::sleep(PERMISSION_POLL).await;
        }
    }

    async fn finish_tool(
        &self,
        step: String,
        call: &ToolCall,
        result: ToolResult,
        forced: Option<&'static str>,
    ) -> Result<CompletedToolCall> {
        // Invariant 4: every `ToolResult` constructor caps and redacts its own output. `bytes`
        // stays the pre-cap size so the budget counts what the tool actually produced.
        debug_assert!(
            result.content.len() <= MAX_OUTPUT + 128,
            "a tool returned uncapped output"
        );
        let status = forced.unwrap_or(match result.status {
            ToolStatus::Complete => "complete",
            ToolStatus::Failed => "failed",
        });
        let mut payload = json!({"tool":call.name,"status":status,"bytes":result.bytes,"truncated":result.truncated,"summary":result.summary});
        if let Some(code) = result.exit_code {
            payload["exit_code"] = json!(code);
        }
        let output = json!({"content":result.content,"summary":result.summary,"error_code":result.error_code,"exit_code":result.exit_code});
        let completed_step = step.clone();
        let mut outcome = self.outcome(step, status, output, "tool_finished", payload);
        outcome.bytes = result.bytes as i64;
        outcome.truncated = result.truncated;
        outcome.error_code = result.error_code.map(str::to_string);
        let refresh_repo_map = result
            .artifacts
            .iter()
            .any(|artifact| matches!(artifact, Artifact::FileChange { .. }));
        outcome.artifacts = result.artifacts.clone();
        self.store.finish_step(outcome).await?;
        if refresh_repo_map {
            if let Some(root) = self.scope.root_path.clone() {
                let _ = tokio::task::spawn_blocking(move || {
                    crate::repo_map::load_or_refresh(std::path::Path::new(&root))
                })
                .await;
            }
        }
        Ok(CompletedToolCall {
            step_id: completed_step,
            result,
        })
    }
}

fn tool_names(tools: &[Value]) -> Vec<String> {
    tools
        .iter()
        .filter_map(|t| t["function"]["name"].as_str().map(str::to_string))
        .collect()
}

/// External-effect coverage is deliberately narrower than the permission capability: bash always
/// dispatches a process, while browser snapshots and navigation are reads and only anchored input
/// operations can change remote state.
fn tool_effect_kind(registry: &Registry, name: &str, args: &Value) -> Option<&'static str> {
    registry.get(name).and_then(|tool| match name {
        "bash" => Some("bash_command"),
        "browser" if tool.side_effecting_for(args) => Some("browser_input"),
        _ => None,
    })
}

fn exhausted(
    steps: i64,
    max_steps: i64,
    bytes: i64,
    max_bytes: i64,
    elapsed: i64,
    max_wall: i64,
) -> Option<&'static str> {
    if steps >= max_steps {
        return Some("max_steps");
    }
    if bytes >= max_bytes {
        return Some("max_tool_bytes");
    }
    if elapsed >= max_wall {
        return Some("max_wall_seconds");
    }
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
#[path = "agent_loop_tests.rs"]
mod tests;
