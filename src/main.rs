use anyhow::Result;
use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::env;

#[derive(Clone)]
struct Harness {
    http: reqwest::Client,
    base_url: String,
    api_key: String,
    default_model: String,
}

impl Harness {
    fn from_env() -> Self {
        Self {
            http: reqwest::Client::new(),
            base_url: env::var("HARNESS_BASE_URL")
                .unwrap_or_else(|_| "https://api.longcat.chat/openai".into())
                .trim_end_matches('/')
                .to_string(),
            api_key: env::var("HARNESS_API_KEY").expect("HARNESS_API_KEY must be set"),
            default_model: env::var("HARNESS_MODEL").unwrap_or_else(|_| "LongCat-2.0".into()),
        }
    }

    async fn list_models(&self) -> Result<Value> {
        let url = format!("{}/models", self.base_url);
        let resp = self
            .http
            .get(&url)
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

    async fn chat(&self, model: &str, prompt: &str) -> Result<Value> {
        let url = format!("{}/chat/completions", self.base_url);
        let payload = json!({
            "model": model,
            "messages": [{"role": "user", "content": prompt}],
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
            anyhow::bail!("upstream /chat failed ({status}): {body}");
        }
        Ok(body)
    }
}

#[derive(Deserialize)]
struct ChatRequest {
    prompt: String,
    #[serde(default)]
    model: Option<String>,
}

async fn models(State(h): State<Harness>) -> Response {
    match h.list_models().await {
        Ok(v) => (StatusCode::OK, Json(v)).into_response(),
        Err(e) => (
            StatusCode::BAD_GATEWAY,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

async fn chat(
    State(h): State<Harness>,
    Json(req): Json<ChatRequest>,
) -> Response {
    let model = req.model.unwrap_or_else(|| h.default_model.clone());
    match h.chat(&model, &req.prompt).await {
        Ok(v) => (StatusCode::OK, Json(v)).into_response(),
        Err(e) => (
            StatusCode::BAD_GATEWAY,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok(); // load .env if present; real env vars still win
    let state = Harness::from_env();
    let app = Router::new()
        .route("/models", get(models))
        .route("/chat", post(chat))
        .with_state(state);
    let addr = env::var("HARNESS_ADDR").unwrap_or_else(|_| "0.0.0.0:8080".into());
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    println!("harness listening on http://{addr}");
    axum::serve(listener, app).await?;
    Ok(())
}
