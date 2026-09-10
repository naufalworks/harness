//! P5-T03: the read-only exploration sub-agent. Contract: docs/design/tools.md#task.
//!
//! This module owns the sub-agent's *contract*: which tools it may be offered, how far it may
//! go, what it is told, and the bounded shape of what comes back. The orchestration — steps,
//! activity events, shared budgets — lives in `src/agent_loop.rs`, next to the counters and the
//! step helpers it has to share with the parent turn.
//!
//! Two properties matter more than anything here:
//! - **Read-only.** `TOOLS` holds nothing side-effecting, so no call inside a sub-agent can ever
//!   raise an approval. A sub-agent cannot be used to route around a denied edit.
//! - **Bounded return.** The parent model gets `report_content`: a capped summary plus the paths
//!   that were read. The sub-agent's transcript never enters the parent's context — that is the
//!   entire point of delegating the exploration.
use serde_json::{json, Value};

use crate::tools::truncate_chars;

/// The only tools a sub-agent is ever offered, all read-only.
pub const TOOLS: [&str; 3] = ["read", "grep", "glob"];
/// Provider calls one delegation may make before it must report what it has.
pub const MAX_MODEL_CALLS: i64 = 8;
/// Cap on the summary handed back to the parent model.
pub const MAX_SUMMARY_CHARS: usize = 1000;
/// Cap on the file references handed back with it.
pub const MAX_FILES: usize = 12;
pub const MAX_DESCRIPTION_CHARS: usize = 80;
pub const MAX_PROMPT_CHARS: usize = 2000;

const NO_FINDINGS: &str = "the sub-agent returned no findings";

pub const SYSTEM: &str = "\
You are a read-only exploration sub-agent. Your only tools are read, grep and glob: you cannot \
edit files, run commands, or change anything.\n\n\
You are given one exploration task. The conversation that asked for it is not visible to you, and \
your intermediate work is never shown back to it — only your final message is.\n\n\
How to work:\n\
1. Locate before reading: glob or grep first, then read small ranges. Never read whole large files.\n\
2. Stop as soon as you can answer. Your budget is small and shared with the agent that sent you.\n\
3. Finish with a plain final message and no tool call: what you found, with the exact paths and \
line numbers that support it. If you could not find it, say so and say where you looked.\n\n\
Report only what the tool output actually shows. Never guess, and never invent a path.";

/// A validated `task` call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ask {
    pub description: String,
    pub prompt: String,
}

/// Read the model's arguments. The error string is shown to the model as `invalid_arguments`.
pub fn parse(args: &Value) -> Result<Ask, String> {
    let description = args
        .get("description")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or_default();
    let prompt = args
        .get("prompt")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or_default();
    if prompt.is_empty() {
        return Err("prompt is required: a sub-agent cannot see this conversation, so it needs self-contained instructions for what to find and what to report back".to_string());
    }
    if prompt.chars().count() > MAX_PROMPT_CHARS {
        return Err(format!("prompt must be at most {MAX_PROMPT_CHARS} characters; send one focused exploration instead"));
    }
    Ok(Ask {
        description: if description.is_empty() {
            "explore".to_string()
        } else {
            truncate_chars(description, MAX_DESCRIPTION_CHARS)
        },
        prompt: prompt.to_string(),
    })
}

/// Narrow the parent's tool array to the read-only set, keeping the parent's exact definitions so
/// both agents describe a tool identically. Order follows `TOOLS`, not the parent array.
pub fn definitions(parent_tools: &[Value]) -> Vec<Value> {
    TOOLS
        .iter()
        .filter_map(|name| {
            parent_tools
                .iter()
                .find(|t| t["function"]["name"].as_str() == Some(*name))
        })
        .cloned()
        .collect()
}

/// The sub-agent's whole context: its own system prompt and the parent's instructions. It never
/// inherits the parent's history — a fresh context is why delegating is cheaper than exploring.
pub fn messages(ask: &Ask) -> Vec<Value> {
    vec![
        json!({"role":"system","content":SYSTEM}),
        json!({"role":"user","content":format!("Exploration: {}\n\n{}", ask.description, ask.prompt)}),
    ]
}

/// Remember a path the sub-agent actually read, in first-read order and without duplicates.
pub fn record_file(files: &mut Vec<String>, tool: &str, args: &Value) {
    if tool != "read" {
        return;
    }
    let Some(path) = args
        .get("path")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|p| !p.is_empty())
    else {
        return;
    };
    if files.iter().any(|seen| seen == path) || files.len() >= MAX_FILES {
        return;
    }
    files.push(path.to_string());
}

/// Why a delegation stopped, when it was not the sub-agent's own decision to answer.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Stop {
    MaxModelCalls,
    TurnSteps,
    TurnToolBytes,
    Deadline,
    ProviderFailed,
}

impl Stop {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::MaxModelCalls => "max_model_calls",
            Self::TurnSteps => "max_steps",
            Self::TurnToolBytes => "max_tool_bytes",
            Self::Deadline => "max_wall_seconds",
            Self::ProviderFailed => "provider_failed",
        }
    }
    /// What the parent model is told, so a partial exploration is never read as a complete one.
    pub fn note(self) -> &'static str {
        match self {
            Self::MaxModelCalls => "stopped at its own step limit, so this may be incomplete",
            Self::TurnSteps => {
                "stopped because the turn ran out of steps, so this may be incomplete"
            }
            Self::TurnToolBytes => {
                "stopped because the turn ran out of tool output budget, so this may be incomplete"
            }
            Self::Deadline => "stopped at the turn's time limit, so this may be incomplete",
            Self::ProviderFailed => {
                "stopped because the model call failed, so this may be incomplete"
            }
        }
    }
}

#[derive(Clone, Debug)]
pub struct Report {
    pub summary: String,
    pub files: Vec<String>,
    pub model_calls: i64,
    pub tool_calls: i64,
    pub stopped: Option<Stop>,
}

/// Everything the parent model is allowed to see. Bounded by construction.
pub fn report_content(report: &Report) -> String {
    let summary = report.summary.trim();
    let summary = if summary.is_empty() {
        NO_FINDINGS
    } else {
        summary
    };
    let mut out = format!(
        "sub-agent report ({} model calls, {} tool calls",
        report.model_calls, report.tool_calls
    );
    if let Some(stop) = report.stopped {
        out.push_str(&format!("; {}", stop.note()));
    }
    out.push_str(")\n");
    out.push_str(&truncate_chars(summary, MAX_SUMMARY_CHARS));
    if !report.files.is_empty() {
        out.push_str("\n\nfiles read:");
        for file in &report.files {
            out.push_str(&format!("\n- {file}"));
        }
    }
    out
}

/// One-line label for the step summary and the activity feed.
pub fn label(ask: &Ask) -> String {
    format!("explore: {}", ask.description)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sub_agent_is_only_ever_offered_read_only_tools() {
        let registry = crate::tools::Registry::standard();
        for name in TOOLS {
            let tool = registry
                .get(name)
                .expect("a sub-agent tool must exist in the registry");
            assert!(
                !tool.side_effecting(),
                "{name} is side-effecting: a sub-agent could then raise an approval"
            );
        }
        assert!(
            !TOOLS.contains(&"task"),
            "a sub-agent must not be able to spawn another sub-agent"
        );
    }

    #[test]
    fn definitions_follow_the_parents_exact_schemas() {
        let parent = crate::tools::Registry::standard()
            .schemas()
            .expect("schemas parse");
        let offered = definitions(&parent);
        let names: Vec<&str> = offered
            .iter()
            .filter_map(|t| t["function"]["name"].as_str())
            .collect();
        assert_eq!(names, TOOLS);
        let parent_read = parent
            .iter()
            .find(|t| t["function"]["name"] == "read")
            .unwrap();
        assert_eq!(
            &offered[0], parent_read,
            "both agents must describe a tool identically"
        );
        assert!(
            definitions(&[]).is_empty(),
            "a turn with no tools cannot offer any to a sub-agent"
        );
    }

    #[test]
    fn a_prompt_is_required_and_the_label_is_bounded() {
        assert!(parse(&json!({"description":"d"})).is_err());
        assert!(parse(&json!({"description":"d","prompt":"   "})).is_err());
        assert!(parse(&json!({"prompt":"x".repeat(MAX_PROMPT_CHARS + 1)})).is_err());
        let ask =
            parse(&json!({"description":"D".repeat(200),"prompt":" find it "})).expect("valid");
        assert_eq!(ask.prompt, "find it");
        assert!(ask.description.chars().count() <= MAX_DESCRIPTION_CHARS);
        assert_eq!(
            parse(&json!({"prompt":"p"})).unwrap().description,
            "explore"
        );
    }

    #[test]
    fn the_sub_agent_starts_from_its_own_context_not_the_parents() {
        let ask = parse(&json!({"description":"where is X","prompt":"find X"})).unwrap();
        let messages = messages(&ask);
        assert_eq!(
            messages.len(),
            2,
            "system prompt and the task, nothing inherited"
        );
        assert_eq!(messages[0]["role"], "system");
        assert!(messages[0]["content"]
            .as_str()
            .unwrap()
            .contains("read-only"));
        assert!(messages[1]["content"].as_str().unwrap().contains("find X"));
    }

    #[test]
    fn only_files_actually_read_are_reported_once_each_and_capped() {
        let mut files = Vec::new();
        record_file(&mut files, "read", &json!({"path":"src/a.rs"}));
        record_file(&mut files, "read", &json!({"path":"src/a.rs"}));
        record_file(&mut files, "grep", &json!({"path":"src/b.rs"}));
        record_file(&mut files, "read", &json!({"path":"  "}));
        assert_eq!(files, ["src/a.rs"]);
        for i in 0..40 {
            record_file(&mut files, "read", &json!({"path":format!("src/f{i}.rs")}));
        }
        assert_eq!(files.len(), MAX_FILES);
    }

    #[test]
    fn the_report_is_capped_and_says_when_it_is_partial() {
        let full = Report {
            summary: "y".repeat(5000),
            files: vec!["src/a.rs".into()],
            model_calls: 3,
            tool_calls: 5,
            stopped: None,
        };
        let content = report_content(&full);
        assert!(
            content.chars().count() < MAX_SUMMARY_CHARS + 200,
            "the parent must never receive the transcript"
        );
        assert!(content.contains("files read:") && content.contains("src/a.rs"));
        assert!(!content.contains("incomplete"));

        let partial = Report {
            summary: String::new(),
            files: Vec::new(),
            model_calls: 8,
            tool_calls: 9,
            stopped: Some(Stop::MaxModelCalls),
        };
        let content = report_content(&partial);
        assert!(content.contains(NO_FINDINGS));
        assert!(content.contains("may be incomplete"));
        assert_eq!(Stop::TurnToolBytes.as_str(), "max_tool_bytes");
    }
}
