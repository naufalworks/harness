//! Evidence gathered for the verification pass: what the turn actually did, in the turn's own
//! words only where a record exists to back it.
//!
//! Moved verbatim from `agent_loop.rs` by P12-T01; behaviour is unchanged. Every string that
//! reaches the verifier passes through `verification_text`, so redaction and truncation are
//! applied once, here, rather than at each call site.
use crate::{
    limits::verification as vlimits,
    memory_agents::ToolCall,
    safety,
    tools::{Artifact, ToolResult, ToolStatus},
};
use serde_json::{json, Value};

pub(super) const MAX_VERIFICATION_STEPS: usize = 24;
pub(super) const MAX_VERIFICATION_ANSWER_CHARS: usize = 12_000;
pub(super) const MAX_VERIFICATION_ARGUMENT_CHARS: usize = 2_000;
pub(super) const MAX_VERIFICATION_OUTPUT_CHARS: usize = 4_000;

#[derive(Clone)]
pub(super) struct VerificationEvidence {
    pub(super) step_id: String,
    pub(super) seq: i64,
    pub(super) priority: bool,
    pub(super) value: Value,
}

pub(super) fn verification_text(text: &str, max_chars: usize) -> String {
    crate::tools::truncate_chars(&safety::redact(text), max_chars)
}

pub(super) fn tool_verification_evidence(
    step_id: String,
    seq: i64,
    call: &ToolCall,
    result: &ToolResult,
) -> VerificationEvidence {
    let status = match result.status {
        ToolStatus::Complete => "complete",
        ToolStatus::Failed => "failed",
    };
    let arguments = call
        .arguments()
        .map(|value| value.to_string())
        .unwrap_or_else(|_| call.arguments_json.clone());
    let file_changes = result
        .artifacts
        .iter()
        .filter_map(|artifact| match artifact {
            Artifact::FileChange {
                path,
                action,
                before_hash,
                after_hash,
                plus,
                minus,
                ..
            } => Some(json!({
                "path":verification_text(path,vlimits::MAX_EVIDENCE_SUMMARY_CHARS),"action":action,"before_hash":before_hash,
                "after_hash":after_hash,"plus":plus,"minus":minus,"applied":true,
            })),
            Artifact::Plan { .. } => None,
        })
        .collect::<Vec<_>>();
    let priority = call.name == "bash" || !file_changes.is_empty();
    VerificationEvidence {
        step_id: step_id.clone(),
        seq,
        priority,
        value: json!({
            "step_id":step_id,"seq":seq,"tool":call.name,"status":status,
            "arguments":verification_text(&arguments,MAX_VERIFICATION_ARGUMENT_CHARS),
            "summary":verification_text(&result.summary,vlimits::MAX_EVIDENCE_SUMMARY_CHARS),
            "output":verification_text(&result.content,MAX_VERIFICATION_OUTPUT_CHARS),
            "error_code":result.error_code,"exit_code":result.exit_code,
            "file_changes":file_changes,
        }),
    }
}

pub(super) fn select_verification_evidence(evidence: &[VerificationEvidence]) -> Vec<VerificationEvidence> {
    let mut selected = Vec::new();
    for priority in [true, false] {
        for item in evidence
            .iter()
            .rev()
            .filter(|item| item.priority == priority)
        {
            if selected.len() >= MAX_VERIFICATION_STEPS {
                break;
            }
            selected.push(item.clone());
        }
    }
    selected.sort_by_key(|item| item.seq);
    selected
}
