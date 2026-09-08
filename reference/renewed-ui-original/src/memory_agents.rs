use crate::storage::DbStore;
use anyhow::Result;
use futures_util::{Stream, StreamExt};
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

    /// Raw GET /models passthrough (for the /models endpoint).
    pub async fn list_models(&self) -> Result<Value> {
        let resp = self
            .http
            .get(format!("{}/models", self.base_url))
            .bearer_auth(&self.api_key)
            .send()
            .await?;
        let status = resp.status();
        let body: Value = resp.json().await?;
        if !status.is_success() {
            anyhow::bail!("upstream /models failed ({status}): {body}");
        }
        Ok(body)
    }

    /// Chat completion returning the raw response JSON (for the /chat endpoint).
    pub async fn chat(&self, model: &str, system: &str, prompt: &str) -> Result<Value> {
        let mut messages = Vec::new();
        if !system.is_empty() {
            messages.push(json!({"role": "system", "content": system}));
        }
        messages.push(json!({"role": "user", "content": prompt}));
        let resp = self
            .http
            .post(format!("{}/chat/completions", self.base_url))
            .bearer_auth(&self.api_key)
            .json(&json!({"model": model, "messages": messages}))
            .send()
            .await?;
        let status = resp.status();
        let body: Value = resp.json().await?;
        if !status.is_success() {
            anyhow::bail!("upstream /chat failed ({status}): {body}");
        }
        Ok(body)
    }

    /// Role-keyed model override: settings["model.<role>"] wins over the default model. pub for /chat's main role.
    pub fn role_model(&self, role: &str) -> String {
        self.store
            .get_setting(&format!("model.{role}"))
            .ok()
            .flatten()
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| self.model.clone())
    }

    async fn call_llm(&self, role: &str, system_prompt: &str, user_prompt: &str) -> Result<String> {
        let url = format!("{}/chat/completions", self.base_url);
        let payload = json!({
            "model": self.role_model(role),
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

        let filtered = self.call_llm("recall", system_prompt, &user_input).await.unwrap_or_default();
        if filtered.trim() == "NONE" || filtered.is_empty() {
            Ok(String::new())
        } else {
            Ok(format!("\n[Retrieved User Context & Preferences]:\n{}\n", filtered.trim()))
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
        let resp = self.call_llm("gatekeeper", system, &input).await.ok()?;
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

        let resp = self.call_llm("extraction", system, &transcript).await?;
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

    /// Streaming chat completion (for the /chat/stream endpoint).
    /// Yields text deltas as they arrive from the upstream OpenAI-compatible SSE stream.
    pub async fn chat_stream(
        &self,
        model: &str,
        system: &str,
        prompt: &str,
    ) -> Result<impl Stream<Item = Result<String>>> {
        let mut messages = Vec::new();
        if !system.is_empty() {
            messages.push(json!({"role": "system", "content": system}));
        }
        messages.push(json!({"role": "user", "content": prompt}));
        let resp = self
            .http
            .post(format!("{}/chat/completions", self.base_url))
            .bearer_auth(&self.api_key)
            .json(&json!({"model": model, "messages": messages, "stream": true}))
            .send()
            .await?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await?;
            anyhow::bail!("upstream /chat stream failed ({status}): {body}");
        }
        let stream = async_stream::try_stream! {
            let mut buf = String::new();
            let mut bytes = resp.bytes_stream();
            while let Some(chunk) = bytes.next().await {
                let chunk = chunk.map_err(|e| anyhow::anyhow!("{e}"))?;
                buf.push_str(&String::from_utf8_lossy(&chunk));
                while let Some(pos) = buf.find('\n') {
                    let line: String = buf.drain(..=pos).collect();
                    let line = line.trim_end();
                    let Some(data) = line.strip_prefix("data:") else { continue };
                    let data = data.trim();
                    if data == "[DONE]" { return; }
                    if let Ok(v) = serde_json::from_str::<Value>(data) {
                        if let Some(t) = v["choices"][0]["delta"]["content"].as_str() {
                            yield t.to_string();
                        }
                    }
                }
            }
        };
        Ok(stream)
    }
}
