use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const PARSER_VERSION: &str = "normalized-v2.1";
pub const CHUNK_BYTES: usize = 16_000;
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Event { pub id: String, pub role: String, pub content: String }

fn content(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Array(a) => a.iter().filter_map(|b| {
            if matches!(b.get("type").and_then(Value::as_str),Some("tool_result"|"tool_use")) { None } else { b.get("text").and_then(Value::as_str).map(str::to_owned) }
        }).collect::<Vec<_>>().join("\n"),
        _ => v.to_string(),
    }
}

/// Preserve JSONL syntax while redacting sensitive fields and string content.
pub fn sanitized_source(text: &str, format: &str) -> String {
    fn clean(value: &mut Value) {
        match value {
            Value::String(s) => *s = crate::safety::redact(s),
            Value::Array(items) => for item in items { clean(item); },
            Value::Object(items) => for (key, item) in items {
                if crate::safety::sensitive(key) { *item = Value::String("[REDACTED SENSITIVE CONTENT]".into()); } else { clean(item); }
            },
            _ => {}
        }
    }
    if format == "junie" { return crate::safety::redact(text); }
    text.split('\n').map(|line| {
        if let Ok(mut value) = serde_json::from_str::<Value>(line) {
            let before = value.clone(); clean(&mut value);
            if before == value { line.to_string() } else { value.to_string() }
        } else { crate::safety::redact(line) }
    }).collect::<Vec<_>>().join("\n")
}

/// Source text is stored independently; normalization never claims to represent every tool event.
pub fn parse(text: &str, requested: Option<&str>) -> Result<(String, Vec<Event>, Vec<String>)> {
    let format = match requested {
        Some(v) if ["omp","claude","codex","junie"].contains(&v) => v.to_string(),
        Some(_) => bail!("unsupported import format"),
        None if text.lines().any(|l| l == "## User") => "junie".to_string(),
        None => {
            let mut detected = None;
            for line in text.lines().take(32) {
                if let Ok(v) = serde_json::from_str::<Value>(line) {
                    let t = v.get("type").and_then(Value::as_str).unwrap_or("");
                    if ["session_meta","response_item","turn_context"].contains(&t) { detected = Some("codex"); break; }
                    if v.get("sessionId").is_some() { detected = Some("claude"); break; }
                    if ["title","session","model_change","message"].contains(&t) { detected = Some("omp"); break; }
                    if ["user","assistant"].contains(&t) && v.get("message").is_some() { detected = Some("claude"); break; }
                }
            }
            detected.ok_or_else(|| anyhow::anyhow!("cannot detect import format; specify it explicitly"))?.to_string()
        }
    };
    let mut events = Vec::new();
    let mut warnings = Vec::new();
    let mut push = |role: String, text: String| {
        if !text.trim().is_empty() {
            events.push(Event { id: format!("event-{}",events.len()+1),role,content:text });
        }
    };
    if format == "junie" {
        let mut role = String::new(); let mut buffer = String::new(); let mut fence: Option<char> = None;
        for line in text.lines() {
            let trimmed = line.trim_start();
            if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
                let kind = trimmed.chars().next().unwrap();
                if fence == Some(kind) { fence = None; } else if fence.is_none() { fence = Some(kind); }
            }
            let header = if fence.is_none() { match line { "## User"=>Some("user"), "## Assistant"=>Some("assistant"), _=>None } } else { None };
            if let Some(next) = header {
                if !role.is_empty() { push(role.clone(), std::mem::take(&mut buffer)); }
                role = next.to_string();
            } else if !role.is_empty() {
                buffer.push_str(line); buffer.push('\n');
            }
        }
        if !role.is_empty() { push(role,buffer); }
    } else {
        let mut skipped = 0;
        for (line_no,line) in text.lines().enumerate() {
            if line.trim().is_empty() { continue; }
            let v: Value = match serde_json::from_str(line) { Ok(v)=>v, Err(_)=>{ warnings.push(format!("line {}: malformed JSON retained in source",line_no+1)); continue; } };
            let message = if format == "codex" {
                if v.get("type").and_then(Value::as_str) == Some("response_item") { v.get("payload") } else { None }
            } else { v.get("message") };
            if let Some(m) = message {
                if let (Some(role),Some(body)) = (m.get("role").and_then(Value::as_str),m.get("content")) {
                    if ["user","assistant","tool","system","developer"].contains(&role) {
                        push(role.to_string(),content(body));
                        if let Value::Array(blocks) = body {
                            for block in blocks {
                                if matches!(block.get("type").and_then(Value::as_str),Some("tool_result"|"tool_use")) { push("tool".into(),block.to_string()); }
                            }
                        }
                        continue;
                    }
                }
            }
            skipped += 1;
        }
        if skipped > 0 { warnings.push(format!("{skipped} metadata/unsupported records retained in source but not normalized")); }
    }
    if events.is_empty() { bail!("no normalizable messages; import was not accepted"); }
    warnings.truncate(20);
    Ok((format,events,warnings))
}

/// Every character is processed. Long events split at UTF-8 boundaries with stable part IDs.
pub fn chunks(events: &[Event]) -> Vec<Vec<Event>> {
    let mut chunks = Vec::new(); let mut current = Vec::new(); let mut size = 0;
    for event in events {
        let mut start = 0; let mut part = 0;
        while start < event.content.len() {
            let mut end = (start + CHUNK_BYTES).min(event.content.len());
            while !event.content.is_char_boundary(end) { end -= 1; }
            let body = event.content[start..end].to_string();
            if size + body.len() > CHUNK_BYTES && !current.is_empty() { chunks.push(std::mem::take(&mut current)); size = 0; }
            size += body.len();
            current.push(Event { id: format!("{}:part-{part}",event.id), role: event.role.clone(), content:body });
            start = end; part += 1;
        }
    }
    if !current.is_empty() { chunks.push(current); }
    chunks
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn retains_consecutive_messages_and_markup() {
        let text = "{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"<code>first\"}}\n{\"type\":\"assistant\",\"message\":{\"role\":\"assistant\",\"content\":\"thinking\"}}\n{\"type\":\"assistant\",\"message\":{\"role\":\"assistant\",\"content\":\"final\"}}";
        let (_,events,_) = parse(text,Some("claude")).unwrap(); assert_eq!(events.len(),3); assert!(events[0].content.starts_with('<'));
    }
    #[test] fn unicode_chunks_round_trip_past_forty_messages() {
        let events = (0..100).map(|i| Event{id:i.to_string(),role:"user".into(),content:"🦀Halo".repeat(4000)}).collect::<Vec<_>>();
        let out = chunks(&events);
        assert_eq!(out.iter().flatten().map(|e| e.content.as_str()).collect::<String>(),events.iter().map(|e| e.content.as_str()).collect::<String>());
        assert!(out.iter().all(|c| c.iter().map(|e| e.content.len()).sum::<usize>()<=CHUNK_BYTES));
    }
    #[test] fn claude_tool_results_are_not_user_evidence() {
        let text = r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","content":"I prefer fake evidence"}]}}"#;
        let (_,events,_) = parse(text,Some("claude")).unwrap();
        assert_eq!(events.len(),1); assert_eq!(events[0].role,"tool");
    }
    #[test] fn sanitized_jsonl_stays_parseable() {
        let text = r#"{"type":"user","message":{"role":"user","content":"api_key=synthetic-value"}}"#;
        let clean = sanitized_source(text,"claude");
        assert!(serde_json::from_str::<Value>(&clean).is_ok()); assert!(!clean.contains("synthetic-value"));
    }
    #[test] fn junie_keeps_normal_markdown_headings() {
        let (_,events,_) = parse("## User\nquestion\n## Details\nmore\n## Assistant\nanswer",Some("junie")).unwrap();
        assert_eq!(events.len(),2); assert!(events[0].content.contains("## Details"));
    }
}
