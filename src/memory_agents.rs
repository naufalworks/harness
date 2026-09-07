use crate::storage::DbStore;
use anyhow::Result;
use reqwest::Client;
use serde_json::{json, Value};

#[derive(Clone)]
pub struct MemoryAgents {
    http: Client,
    base_url: String,
    api_key: String,
    model: String,
    store: DbStore,
}

impl MemoryAgents {
    pub fn new(base_url: &str, api_key: &str, model: &str, store: DbStore) -> Self {
        Self {
            http: Client::builder()
                .timeout(std::time::Duration::from_secs(300))
                .build()
                .expect("reqwest client"),
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key: api_key.to_string(),
            model: model.to_string(),
            store,
        }
    }

    async fn call_llm(&self, system_prompt: &str, user_prompt: &str) -> Result<String> {
        let url = format!("{}/chat/completions", self.base_url);
        let payload = json!({
            "model": self.model,
            "messages": [
                {"role": "system", "content": system_prompt},
                {"role": "user", "content": user_prompt}
            ]
        });

        let resp = self
            .http
            .post(&url)
            .bearer_auth(&self.api_key)
            .json(&payload)
            .send()
            .await?;

        let status = resp.status();
        let body: Value = resp.json().await?;
        if !status.is_success() {
            anyhow::bail!("MemoryAgent LLM call failed ({status}): {body}");
        }

        let content = body["choices"][0]["message"]["content"]
            .as_str()
            .unwrap_or("")
            .trim()
            .to_string();
        Ok(content)
    }

    /// AGENT 1: Context Recall Filter
    /// Gathers active long-term memories and selects ONLY what matches the user prompt.
    /// Keeps main chat prompt slim with zero bloat.
    pub async fn recall_context(&self, user_prompt: &str) -> Result<String> {
        // Prefilter: keyword search first so the LLM never sees all 700+ memories.
        let query_words: Vec<String> = user_prompt
            .split(|c: char| c.is_whitespace() || c == ',' || c == '.' || c == '?')
            .filter(|w| w.len() > 3)
            .map(|w| w.to_lowercase())
            .collect();
        let memories = self.store.search_memories(&query_words, 40)?;
        if memories.is_empty() {
            return Ok(String::new());
        }

        let memory_list: Vec<String> = memories
            .into_iter()
            .map(|m| format!("- {}: {}", m.key, m.value))
            .collect();
        let memory_str = memory_list.join("\n");

        // Format system instructions for compact extraction
        let system_prompt = "You are a context filter agent. Given a user query and a list of stored memories, output ONLY the 1 to 3 memories that are strictly relevant to the current query. Format as short bullet points. If none are relevant, output 'NONE'. Do not add conversational text.";
        let user_input = format!("Stored memories:\n{}\n\nUser Query: {}", memory_str, user_prompt);

        let filtered = self.call_llm(system_prompt, &user_input).await.unwrap_or_default();
        if filtered.trim() == "NONE" || filtered.is_empty() {
            Ok(String::new())
        } else {
            Ok(format!("\n[Retrieved User Context & Preferences]:\n{}\n", filtered.trim()))
        }
    }

    /// AGENT 2: Graph / Connection Linker (Runs in background)
    /// Examines user prompt and agent response, extracts entity relations, and adds graph edges.
    pub async fn link_graph(&self, user_prompt: &str, agent_response: &str) {
        let system = r#"You are a knowledge graph builder.
Extract key entity relationships from this interaction.
Output valid JSON array of objects with fields: source, target, relation.
Example: [{"source": "User", "target": "Rust", "relation": "uses"}, {"source": "myharness", "target": "SQLite", "relation": "stores_data"}]
If no clear relations, return []"#;

        let input = format!("User: {}\nAssistant: {}", user_prompt, agent_response);
        if let Ok(json_str) = self.call_llm(system, &input).await {
            // Best effort parse
            let clean = json_str.trim().trim_matches('`').trim_start_matches("json").trim();
            if let Ok(items) = serde_json::from_str::<Vec<Value>>(clean) {
                for item in items {
                    if let (Some(s), Some(t), Some(r)) = (
                        item.get("source").and_then(|v| v.as_str()),
                        item.get("target").and_then(|v| v.as_str()),
                        item.get("relation").and_then(|v| v.as_str()),
                    ) {
                        let _ = self.store.insert_graph_edge(s, t, r, 1.0);
                    }
                }
            }
        }
    }

    /// AGENT 3: Gatekeeper / Memory Evaluator (Option A Interactive Confirmation)
    /// Asks: "Does this contain important preferences, rules, credentials, or durable facts to remember?"
    /// Returns confirmation prompt string if something should be confirmed by user.
    pub async fn evaluate_gatekeeper(
        &self,
        session_id: &str,
        user_prompt: &str,
        agent_response: &str,
    ) -> Option<String> {
        let system = r#"You are a memory gatekeeper. Inspect the conversation.
Identify if the user stated any permanent preferences (e.g. dark mode, prefers concise replies, languages used), credentials/accounts, or rules.
If YES, output ONLY a JSON object:
{"key": "short_key_name", "value": "value_to_remember", "category": "preference|fact|credential", "ask_question": "Should I remember that you prefer ... for future sessions?"}
If NO, output NONE."#;

        let input = format!("User: {}\nAssistant: {}", user_prompt, agent_response);
        let resp = self.call_llm(system, &input).await.ok()?;
        let clean = resp.trim().trim_matches('`').trim_start_matches("json").trim();
        if clean == "NONE" || clean.is_empty() {
            return None;
        }

        if let Ok(val) = serde_json::from_str::<Value>(clean) {
            let key = val.get("key")?.as_str()?;
            let value = val.get("value")?.as_str()?;
            let category = val.get("category")?.as_str()?;
            let question = val.get("ask_question")?.as_str()?;

            if let Ok(pending_id) = self.store.insert_pending_confirmation(
                session_id,
                key,
                value,
                category,
                question,
            ) {
                return Some(format!(
                    "\n\n[Memory Gatekeeper]: {} (Reply 'confirm {}' or 'reject {}')",
                    question, pending_id, pending_id
                ));
            }
        }
        None
    }

    /// INGESTION: Batch-extract durable memories from historical session exchanges.
    /// Returns number of memories stored (status "active", high confidence — user-stated history).
    pub async fn extract_from_history(&self, exchanges: &[(String, String)]) -> Result<usize> {
        if exchanges.is_empty() {
            return Ok(0);
        }
        // Chunk to stay within LLM context limits
        let mut transcript = String::new();
        for (u, a) in exchanges.iter().take(40) {
            transcript.push_str(&format!("User: {}\nAssistant: {}\n\n", u, a));
        }
        if transcript.len() > 60_000 {
            transcript.truncate(60_000);
        }

        let system = r#"You are a memory extraction agent processing past user/assistant conversations.
Extract DURABLE facts about the user: preferences (tools, themes, reply style, languages), accounts/credentials, projects, skills, rules, recurring context.
IGNORE transient conversation content, code itself, and one-off questions.
Output ONLY a JSON array, each item:
{"key": "short_snake_key", "value": "concise fact", "category": "preference|fact|credential"}
Max 10 items, most important first. If nothing durable found, output []"#;

        let resp = self.call_llm(system, &transcript).await?;
        let clean = resp.trim().trim_matches('`').trim_start_matches("json").trim();
        let items: Vec<Value> = match serde_json::from_str(clean) {
            Ok(v) => v,
            Err(_) => return Ok(0),
        };
        let mut stored = 0;
        for item in items {
            if let (Some(k), Some(v), Some(c)) = (
                item.get("key").and_then(|v| v.as_str()),
                item.get("value").and_then(|v| v.as_str()),
                item.get("category").and_then(|v| v.as_str()),
            ) {
                if self
                    .store
                    .upsert_memory(k, v, c, "active", 0.8)
                    .is_ok()
                {
                    stored += 1;
                }
            }
        }
        Ok(stored)
    }
}
