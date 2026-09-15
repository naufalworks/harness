//! Offline strict replay and first-divergence reports (P17-T03).
//!
//! This module deliberately has no provider or tool client. A deterministic pipeline presents the
//! requests it actually produced; replay returns a recorded result only while every preceding
//! context/request boundary still matches the sealed tape.

use super::capsule::{BoundaryState, ReplayMode, RunCapsule};
use super::treatment::TreatmentManifest;
use crate::export::packet::canonical_json;
use crate::export::review;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeSet;

pub const FORMAT: &str = "harness-strict-replay-v1";
pub const REPORT_FORMAT: &str = "harness-replay-report-v1";
pub const MAX_STEPS: usize = 4096;
pub const MAX_TAPE_BYTES: usize = 4 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplayBoundary {
    Provider,
    Tool,
    Permission,
    Diff,
    Acceptance,
}

impl ReplayBoundary {
    fn as_str(self) -> &'static str {
        match self {
            Self::Provider => "provider",
            Self::Tool => "tool",
            Self::Permission => "permission",
            Self::Diff => "diff",
            Self::Acceptance => "acceptance",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordedReplayStep {
    pub sequence: u32,
    pub identity: String,
    pub boundary: ReplayBoundary,
    pub tool_name: Option<String>,
    pub request: Value,
    pub result: Value,
    pub request_sha256: String,
    pub result_sha256: String,
}

impl RecordedReplayStep {
    pub fn new(
        sequence: u32,
        identity: impl Into<String>,
        boundary: ReplayBoundary,
        tool_name: Option<String>,
        request: Value,
        result: Value,
    ) -> Self {
        let request_sha256 = value_sha256(&request);
        let result_sha256 = value_sha256(&result);
        Self {
            sequence,
            identity: identity.into(),
            boundary,
            tool_name,
            request,
            result,
            request_sha256,
            result_sha256,
        }
    }

    fn validate(&self, expected_sequence: usize) -> Result<()> {
        if self.sequence as usize != expected_sequence {
            bail!("replay steps must have contiguous recorded order");
        }
        bounded("step identity", &self.identity, 1, 160)?;
        match (self.boundary, self.tool_name.as_deref()) {
            (ReplayBoundary::Tool, Some(name)) => bounded("tool name", name, 1, 128)?,
            (ReplayBoundary::Tool, None) => bail!("tool replay step requires tool_name"),
            (_, Some(_)) => bail!("only tool replay steps may declare tool_name"),
            (_, None) => {}
        }
        if self.request_sha256 != value_sha256(&self.request) {
            bail!("recorded replay request digest does not match its bytes");
        }
        if self.result_sha256 != value_sha256(&self.result) {
            bail!("recorded replay result digest does not match its bytes");
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReplayTape {
    pub format: String,
    /// SHA-256 of canonical JSON after removing this field.
    pub tape_id: String,
    pub capsule_id: String,
    pub treatment_id: String,
    pub initial_context_sha256: String,
    pub steps: Vec<RecordedReplayStep>,
}

impl ReplayTape {
    fn canonical_without_id(&self) -> Result<String> {
        let mut value = serde_json::to_value(self)?;
        value
            .as_object_mut()
            .context("replay tape must serialize as an object")?
            .remove("tape_id");
        Ok(canonical_json(&value))
    }

    pub fn seal(mut self, capsule: &RunCapsule, treatment: &TreatmentManifest) -> Result<Self> {
        self.tape_id.clear();
        self.tape_id = review::checksum(&self.canonical_without_id()?);
        self.validate(capsule, treatment)?;
        Ok(self)
    }

    pub fn canonical_json(
        &self,
        capsule: &RunCapsule,
        treatment: &TreatmentManifest,
    ) -> Result<String> {
        self.validate(capsule, treatment)?;
        Ok(canonical_json(&serde_json::to_value(self)?))
    }

    pub fn validate(&self, capsule: &RunCapsule, treatment: &TreatmentManifest) -> Result<()> {
        capsule.validate()?;
        treatment.validate()?;
        if capsule.replay_mode != ReplayMode::Strict {
            bail!("offline strict replay requires a strict capsule");
        }
        if self.format != FORMAT {
            bail!("unsupported strict replay tape format");
        }
        hex_digest("replay tape id", &self.tape_id)?;
        if self.tape_id != review::checksum(&self.canonical_without_id()?) {
            bail!("replay tape content address does not match its bytes");
        }
        if self.capsule_id != capsule.capsule_id
            || treatment.parent_capsule_id != capsule.capsule_id
            || self.treatment_id != treatment.treatment_id
        {
            bail!("replay tape, capsule and treatment are not the same experiment");
        }
        if treatment.project != capsule.body.project {
            bail!("treatment project does not match the strict capsule");
        }
        if self.initial_context_sha256 != capsule.body.context.content_sha256 {
            bail!("recorded replay context does not match the strict capsule");
        }
        hex_digest("initial replay context", &self.initial_context_sha256)?;
        if self.steps.len() > MAX_STEPS {
            bail!("replay tape exceeds the 4096-step bound");
        }
        let mut identities = BTreeSet::new();
        for (index, step) in self.steps.iter().enumerate() {
            step.validate(index)?;
            if !identities.insert(step.identity.as_str()) {
                bail!("replay step identities must be unique");
            }
        }
        if canonical_json(&serde_json::to_value(self)?).len() > MAX_TAPE_BYTES {
            bail!("replay tape exceeds the 4 MiB bound");
        }

        let provider_boundary = capsule
            .body
            .boundaries
            .iter()
            .find(|item| item.name == "provider")
            .context("strict capsule has no provider boundary")?;
        if !matches!(
            provider_boundary.state,
            BoundaryState::Frozen | BoundaryState::Deterministic
        ) || provider_boundary.content_sha256.as_deref()
            != Some(provider_transcript_sha256(&self.steps)?.as_str())
        {
            bail!("provider replay transcript is not bound to the capsule boundary");
        }

        let tool_steps = self
            .steps
            .iter()
            .filter(|step| step.boundary == ReplayBoundary::Tool)
            .collect::<Vec<_>>();
        if tool_steps.len() != capsule.body.tools.results.len() {
            bail!("replay tape does not contain the complete capsule tool transcript");
        }
        for (step, expected) in tool_steps.into_iter().zip(&capsule.body.tools.results) {
            if step.tool_name.as_deref() != Some(expected.tool_name.as_str())
                || step.request_sha256 != expected.request_sha256
                || step.result_sha256 != expected.result_sha256
            {
                bail!("tool replay result is not bound to its capsule request");
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReplayRequest {
    pub identity: String,
    pub boundary: ReplayBoundary,
    pub tool_name: Option<String>,
    pub request: Value,
}

impl ReplayRequest {
    pub fn from_recorded(step: &RecordedReplayStep) -> Self {
        Self {
            identity: step.identity.clone(),
            boundary: step.boundary,
            tool_name: step.tool_name.clone(),
            request: step.request.clone(),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LiveCallCounts {
    pub provider: u64,
    pub tools: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplayStatus {
    Reproduced,
    Diverged,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceStatus {
    Replayed,
    Diverged,
    Unavailable,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FirstDivergence {
    /// `None` means divergence occurred at the initial context boundary, before step zero.
    pub sequence: Option<u32>,
    pub identity: String,
    pub boundary: String,
    pub expected_request_sha256: Option<String>,
    pub actual_request_sha256: Option<String>,
    pub reason: String,
    pub downstream_behavior: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AlignedReplayStep {
    pub sequence: u32,
    pub identity: String,
    pub boundary: ReplayBoundary,
    pub evidence: EvidenceStatus,
    pub expected_request_sha256: Option<String>,
    pub actual_request_sha256: Option<String>,
    /// Present only for an exactly matched request. Diverged/unavailable steps never expose old data.
    pub result_sha256: Option<String>,
    pub result: Option<Value>,
    pub reason: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReplayReport {
    pub format: String,
    /// SHA-256 of canonical JSON after removing this field.
    pub report_id: String,
    pub tape_id: String,
    pub capsule_id: String,
    pub treatment_id: String,
    pub status: ReplayStatus,
    pub matched_steps: u32,
    pub first_divergence: Option<FirstDivergence>,
    pub steps: Vec<AlignedReplayStep>,
    /// Structural proof: this replay API owns no live provider/tool client.
    pub live_calls: LiveCallCounts,
}

impl ReplayReport {
    fn seal(mut self) -> Result<Self> {
        self.report_id.clear();
        let mut value = serde_json::to_value(&self)?;
        value
            .as_object_mut()
            .context("replay report must serialize as an object")?
            .remove("report_id");
        self.report_id = review::checksum(&canonical_json(&value));
        Ok(self)
    }

    pub fn canonical_json(&self) -> Result<String> {
        let mut value = serde_json::to_value(self)?;
        let object = value
            .as_object_mut()
            .context("replay report must serialize as an object")?;
        let id = object.remove("report_id");
        if id.as_ref().and_then(Value::as_str) != Some(self.report_id.as_str())
            || self.report_id != review::checksum(&canonical_json(&value))
        {
            bail!("replay report content address does not match its bytes");
        }
        Ok(canonical_json(&serde_json::to_value(self)?))
    }
}

/// Replays recorded results only while actual deterministic requests match the sealed transcript.
/// There is intentionally no callback, provider, registry, shell, filesystem write, or network path.
pub fn strict_replay(
    tape: &ReplayTape,
    capsule: &RunCapsule,
    treatment: &TreatmentManifest,
    actual_context_sha256: &str,
    actual_requests: &[ReplayRequest],
) -> Result<ReplayReport> {
    tape.validate(capsule, treatment)?;
    hex_digest("actual replay context", actual_context_sha256)?;
    let mut aligned = Vec::with_capacity(tape.steps.len().max(actual_requests.len()));
    let mut first = None;
    let mut matched = 0_u32;

    if actual_context_sha256 != tape.initial_context_sha256 {
        first = Some(FirstDivergence {
            sequence: None,
            identity: "context".into(),
            boundary: "context".into(),
            expected_request_sha256: Some(tape.initial_context_sha256.clone()),
            actual_request_sha256: Some(actual_context_sha256.into()),
            reason: "deterministic context differs from the recorded request boundary".into(),
            downstream_behavior: "unavailable".into(),
        });
    }

    for (index, expected) in tape.steps.iter().enumerate() {
        if first.is_some() {
            aligned.push(unavailable_step(
                expected,
                "downstream of the first divergence",
            ));
            continue;
        }
        let Some(actual) = actual_requests.get(index) else {
            first = Some(FirstDivergence {
                sequence: Some(expected.sequence),
                identity: expected.identity.clone(),
                boundary: expected.boundary.as_str().into(),
                expected_request_sha256: Some(expected.request_sha256.clone()),
                actual_request_sha256: None,
                reason: "deterministic pipeline did not produce the recorded request".into(),
                downstream_behavior: "unavailable".into(),
            });
            aligned.push(diverged_step(
                expected,
                None,
                "recorded request was not produced",
            ));
            continue;
        };
        let actual_sha256 = value_sha256(&actual.request);
        let mismatch = if actual.identity != expected.identity {
            Some("step identity changed")
        } else if actual.boundary != expected.boundary {
            Some("request boundary changed")
        } else if actual.tool_name != expected.tool_name {
            Some("tool name changed")
        } else if actual_sha256 != expected.request_sha256 {
            Some("request bytes changed")
        } else {
            None
        };
        if let Some(reason) = mismatch {
            first = Some(FirstDivergence {
                sequence: Some(expected.sequence),
                identity: expected.identity.clone(),
                boundary: expected.boundary.as_str().into(),
                expected_request_sha256: Some(expected.request_sha256.clone()),
                actual_request_sha256: Some(actual_sha256.clone()),
                reason: reason.into(),
                downstream_behavior: "unavailable".into(),
            });
            aligned.push(diverged_step(expected, Some(actual_sha256), reason));
        } else {
            matched += 1;
            aligned.push(AlignedReplayStep {
                sequence: expected.sequence,
                identity: expected.identity.clone(),
                boundary: expected.boundary,
                evidence: EvidenceStatus::Replayed,
                expected_request_sha256: Some(expected.request_sha256.clone()),
                actual_request_sha256: Some(actual_sha256),
                result_sha256: Some(expected.result_sha256.clone()),
                result: Some(expected.result.clone()),
                reason: None,
            });
        }
    }

    if first.is_none() && actual_requests.len() > tape.steps.len() {
        let actual = &actual_requests[tape.steps.len()];
        let actual_sha256 = value_sha256(&actual.request);
        first = Some(FirstDivergence {
            sequence: Some(tape.steps.len() as u32),
            identity: actual.identity.clone(),
            boundary: actual.boundary.as_str().into(),
            expected_request_sha256: None,
            actual_request_sha256: Some(actual_sha256.clone()),
            reason: "deterministic pipeline produced an unrecorded request".into(),
            downstream_behavior: "unavailable".into(),
        });
        aligned.push(AlignedReplayStep {
            sequence: tape.steps.len() as u32,
            identity: actual.identity.clone(),
            boundary: actual.boundary,
            evidence: EvidenceStatus::Diverged,
            expected_request_sha256: None,
            actual_request_sha256: Some(actual_sha256),
            result_sha256: None,
            result: None,
            reason: Some("unrecorded request".into()),
        });
    }

    ReplayReport {
        format: REPORT_FORMAT.into(),
        report_id: String::new(),
        tape_id: tape.tape_id.clone(),
        capsule_id: capsule.capsule_id.clone(),
        treatment_id: treatment.treatment_id.clone(),
        status: if first.is_some() {
            ReplayStatus::Diverged
        } else {
            ReplayStatus::Reproduced
        },
        matched_steps: matched,
        first_divergence: first,
        steps: aligned,
        live_calls: LiveCallCounts::default(),
    }
    .seal()
}

fn unavailable_step(expected: &RecordedReplayStep, reason: &str) -> AlignedReplayStep {
    AlignedReplayStep {
        sequence: expected.sequence,
        identity: expected.identity.clone(),
        boundary: expected.boundary,
        evidence: EvidenceStatus::Unavailable,
        expected_request_sha256: Some(expected.request_sha256.clone()),
        actual_request_sha256: None,
        result_sha256: None,
        result: None,
        reason: Some(reason.into()),
    }
}

fn diverged_step(
    expected: &RecordedReplayStep,
    actual_request_sha256: Option<String>,
    reason: &str,
) -> AlignedReplayStep {
    AlignedReplayStep {
        sequence: expected.sequence,
        identity: expected.identity.clone(),
        boundary: expected.boundary,
        evidence: EvidenceStatus::Diverged,
        expected_request_sha256: Some(expected.request_sha256.clone()),
        actual_request_sha256,
        result_sha256: None,
        result: None,
        reason: Some(reason.into()),
    }
}

pub fn provider_transcript_sha256(steps: &[RecordedReplayStep]) -> Result<String> {
    #[derive(Serialize)]
    struct DigestStep<'a> {
        sequence: u32,
        identity: &'a str,
        request_sha256: &'a str,
        result_sha256: &'a str,
    }
    let transcript = steps
        .iter()
        .filter(|step| step.boundary == ReplayBoundary::Provider)
        .map(|step| DigestStep {
            sequence: step.sequence,
            identity: &step.identity,
            request_sha256: &step.request_sha256,
            result_sha256: &step.result_sha256,
        })
        .collect::<Vec<_>>();
    Ok(review::checksum(&canonical_json(&serde_json::to_value(
        transcript,
    )?)))
}

fn value_sha256(value: &Value) -> String {
    review::checksum(&canonical_json(value))
}

fn bounded(name: &str, value: &str, min: usize, max: usize) -> Result<()> {
    if value.len() < min || value.len() > max || value.trim() != value {
        bail!("{name} must be trimmed and {min}..={max} bytes");
    }
    Ok(())
}

fn hex_digest(name: &str, value: &str) -> Result<()> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        bail!("{name} must be a lowercase SHA-256 digest");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::experiments::capsule::{
        Assertion, Boundary, CapsuleBody, ContextState, EvidenceRef, MemoryRef, MemoryState,
        ModelState, ProjectState, ToolResultRef, ToolState, FORMAT as CAPSULE_FORMAT, VALIDATOR,
    };
    use crate::experiments::treatment::{FrozenMemory, TreatmentKind, FORMAT as TREATMENT_FORMAT};
    use serde_json::json;

    struct Fixture {
        capsule: RunCapsule,
        treatment: TreatmentManifest,
        tape: ReplayTape,
        requests: Vec<ReplayRequest>,
    }

    fn fixture() -> Fixture {
        let provider = RecordedReplayStep::new(
            0,
            "provider:0",
            ReplayBoundary::Provider,
            None,
            json!({"messages":[{"role":"user","content":"fix it"}]}),
            json!({"tool_calls":[{"name":"read","arguments":{"path":"main.rs"}}]}),
        );
        let tool = RecordedReplayStep::new(
            1,
            "tool:0",
            ReplayBoundary::Tool,
            Some("read".into()),
            json!({"path":"main.rs"}),
            json!({"content":"fn main() {}\n"}),
        );
        let acceptance = RecordedReplayStep::new(
            2,
            "acceptance:0",
            ReplayBoundary::Acceptance,
            None,
            json!({"command":"cargo test"}),
            json!({"exit":0}),
        );
        let steps = vec![provider, tool, acceptance];
        let memory = FrozenMemory {
            stable_id: "memory-1".into(),
            revision: 1,
            scope: "project".into(),
            key: "testing".into(),
            value: "run focused tests".into(),
            branch: "main".into(),
            category: "rule".into(),
            content_sha256: String::new(),
        }
        .seal()
        .unwrap();
        let memory_ref = MemoryRef {
            stable_id: memory.stable_id.clone(),
            revision: memory.revision,
            content_sha256: memory.content_sha256.clone(),
        };
        let context_sha256 = review::checksum("context with memory-1");
        let capsule = RunCapsule {
            format: CAPSULE_FORMAT.into(),
            validator: VALIDATOR.into(),
            capsule_id: String::new(),
            source_request_id: "request-1".into(),
            replay_mode: ReplayMode::Strict,
            body: CapsuleBody {
                task: EvidenceRef {
                    content_sha256: review::checksum("task"),
                    sanitizer: review::SANITIZER.into(),
                },
                session_history: EvidenceRef {
                    content_sha256: review::checksum("history"),
                    sanitizer: review::SANITIZER.into(),
                },
                project: ProjectState {
                    kind: "git_commit".into(),
                    reference: "a".repeat(40),
                    content_sha256: review::checksum("tree"),
                    clean: true,
                },
                model: ModelState {
                    provider: "recorded".into(),
                    model: "fixture".into(),
                    parameters: json!({"temperature":0}),
                    request_schema_sha256: review::checksum("schema"),
                },
                tools: ToolState {
                    schemas_sha256: review::checksum("tool schemas"),
                    permission_mode: "ask".into(),
                    results_complete: true,
                    results: vec![ToolResultRef {
                        sequence: 0,
                        tool_name: "read".into(),
                        request_sha256: steps[1].request_sha256.clone(),
                        result_sha256: steps[1].result_sha256.clone(),
                    }],
                },
                memory: MemoryState {
                    package_sha256: review::checksum(&canonical_json(
                        &serde_json::to_value([&memory_ref]).unwrap(),
                    )),
                    memories: vec![memory_ref],
                },
                context: ContextState {
                    receipt_id: "context-1".into(),
                    content_sha256: context_sha256.clone(),
                },
                assertions: vec![Assertion::NoUnauthorizedActions],
                boundaries: ["clock", "randomness", "provider", "tools"]
                    .into_iter()
                    .map(|name| Boundary {
                        name: name.into(),
                        state: BoundaryState::Frozen,
                        content_sha256: Some(if name == "provider" {
                            provider_transcript_sha256(&steps).unwrap()
                        } else {
                            review::checksum(name)
                        }),
                        reason: None,
                    })
                    .collect(),
                unavailable_evidence: vec![],
                strict_prefix_sha256: None,
                first_live_boundary: None,
            },
        }
        .seal()
        .unwrap();
        let treatment = TreatmentManifest::derive(
            &capsule,
            TreatmentKind::Baseline,
            std::slice::from_ref(&memory),
        )
        .unwrap();
        assert_eq!(treatment.format, TREATMENT_FORMAT);
        let tape = ReplayTape {
            format: FORMAT.into(),
            tape_id: String::new(),
            capsule_id: capsule.capsule_id.clone(),
            treatment_id: treatment.treatment_id.clone(),
            initial_context_sha256: context_sha256,
            steps,
        }
        .seal(&capsule, &treatment)
        .unwrap();
        let requests = tape
            .steps
            .iter()
            .map(ReplayRequest::from_recorded)
            .collect();
        Fixture {
            capsule,
            treatment,
            tape,
            requests,
        }
    }

    #[test]
    fn replay_reproduces_only_exactly_bound_recorded_results_without_live_calls() {
        let fixture = fixture();
        let report = strict_replay(
            &fixture.tape,
            &fixture.capsule,
            &fixture.treatment,
            &fixture.tape.initial_context_sha256,
            &fixture.requests,
        )
        .unwrap();
        assert_eq!(report.status, ReplayStatus::Reproduced);
        assert_eq!(report.matched_steps, 3);
        assert_eq!(report.live_calls, LiveCallCounts::default());
        assert!(report.first_divergence.is_none());
        assert!(report
            .steps
            .iter()
            .all(|step| { step.evidence == EvidenceStatus::Replayed && step.result.is_some() }));
        assert!(report.canonical_json().is_ok());
    }

    #[test]
    fn replay_stops_at_changed_request_and_withholds_old_downstream_results() {
        let mut fixture = fixture();
        fixture.requests[1].request = json!({"path":"different.rs"});
        let report = strict_replay(
            &fixture.tape,
            &fixture.capsule,
            &fixture.treatment,
            &fixture.tape.initial_context_sha256,
            &fixture.requests,
        )
        .unwrap();
        assert_eq!(report.status, ReplayStatus::Diverged);
        assert_eq!(report.matched_steps, 1);
        assert_eq!(report.first_divergence.as_ref().unwrap().sequence, Some(1));
        assert_eq!(report.steps[0].evidence, EvidenceStatus::Replayed);
        assert_eq!(report.steps[1].evidence, EvidenceStatus::Diverged);
        assert_eq!(report.steps[2].evidence, EvidenceStatus::Unavailable);
        assert!(report.steps[1].result.is_none());
        assert!(report.steps[2].result.is_none());
        assert_eq!(report.live_calls, LiveCallCounts::default());
    }

    #[test]
    fn replay_changed_context_makes_all_behavioral_evidence_unavailable() {
        let fixture = fixture();
        let report = strict_replay(
            &fixture.tape,
            &fixture.capsule,
            &fixture.treatment,
            &review::checksum("context without the memory"),
            &fixture.requests,
        )
        .unwrap();
        assert_eq!(report.matched_steps, 0);
        assert_eq!(report.first_divergence.as_ref().unwrap().sequence, None);
        assert!(report
            .steps
            .iter()
            .all(|step| { step.evidence == EvidenceStatus::Unavailable && step.result.is_none() }));
    }

    #[test]
    fn replay_rejects_a_tool_result_not_bound_to_the_capsule_request() {
        let fixture = fixture();
        let mut tape = fixture.tape.clone();
        tape.steps[1].result = json!({"content":"forged"});
        tape.steps[1].result_sha256 = value_sha256(&tape.steps[1].result);
        tape.tape_id = review::checksum(&tape.canonical_without_id().unwrap());
        assert!(tape.validate(&fixture.capsule, &fixture.treatment).is_err());
    }

    #[test]
    fn replay_checked_in_fixture_produces_a_reproducible_report() {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct CheckedInFixture {
            format: String,
            context: String,
            requests: Vec<ReplayRequest>,
        }

        let input: CheckedInFixture = serde_json::from_str(include_str!(
            "../../tests/capsules/strict_replay_requests.json"
        ))
        .unwrap();
        assert_eq!(input.format, "harness-strict-replay-fixture-v1");
        let fixture = fixture();
        let context_sha256 = review::checksum(&input.context);
        let first = strict_replay(
            &fixture.tape,
            &fixture.capsule,
            &fixture.treatment,
            &context_sha256,
            &input.requests,
        )
        .unwrap();
        let second = strict_replay(
            &fixture.tape,
            &fixture.capsule,
            &fixture.treatment,
            &context_sha256,
            &input.requests,
        )
        .unwrap();
        assert_eq!(first.status, ReplayStatus::Reproduced);
        assert_eq!(first.report_id, second.report_id);
        assert_eq!(
            first.canonical_json().unwrap(),
            second.canonical_json().unwrap()
        );
    }
}
