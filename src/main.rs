use anyhow::{bail, Result};
use axum::{extract::{DefaultBodyLimit,Path,Query,Request,State},http::{header,StatusCode},middleware::{self,Next},response::{IntoResponse,Response},routing::{get,post},Json,Router};
use serde::Deserialize;
use serde_json::{json,Value};
use std::{collections::BTreeMap,env,net::SocketAddr,sync::Arc};
use tokio::sync::Semaphore;
use uuid::Uuid;
mod ingest;
mod memory_agents;
mod safety;
mod storage;
mod recording;
mod recording_sql;
mod agentic_sql; // P1 SQL constants (schema 003); contract-tested by tests/test_agentic_sql.py
mod tools;       // P1-T05/T06 tool registry (needs-verify: written without cargo)
use memory_agents::MemoryAgents;
use storage::DbStore;

#[derive(Clone)]
struct Harness {store:DbStore,agents:MemoryAgents,token:Arc<String>,port:u16,api_limit:Arc<Semaphore>}
struct ApiError(StatusCode,&'static str);
impl IntoResponse for ApiError{fn into_response(self)->Response{(self.0,Json(json!({"error":self.1}))).into_response()}}
type ApiResult<T> = std::result::Result<T,ApiError>;
fn db_error(_:anyhow::Error)->ApiError{eprintln!("{{\"event\":\"storage_operation_failed\"}}");ApiError(StatusCode::INTERNAL_SERVER_ERROR,"Storage operation failed. No successful save is implied.")}
fn invalid(message:&'static str)->ApiError{ApiError(StatusCode::BAD_REQUEST,message)}
fn default_scope()->String{"global".into()}

#[allow(deprecated)]
fn valid_token(expected:&str,actual:&str)->bool{
    ring::constant_time::verify_slices_are_equal(expected.as_bytes(),actual.as_bytes()).is_ok()
}
async fn authenticate(State(h):State<Harness>,request:Request,next:Next)->Response{
    let auth=request.headers().get(header::AUTHORIZATION).and_then(|v|v.to_str().ok()).and_then(|v|v.strip_prefix("Bearer "));
    if !auth.is_some_and(|value|valid_token(&h.token,value)){return (StatusCode::UNAUTHORIZED,Json(json!({"error":"Bearer token required"}))).into_response();}
    if let Some(origin)=request.headers().get(header::ORIGIN){
        let allowed=[format!("http://127.0.0.1:{}",h.port),format!("http://localhost:{}",h.port),format!("http://[::1]:{}",h.port)];
        if !origin.to_str().ok().is_some_and(|o|allowed.iter().any(|a|a==o)){return (StatusCode::FORBIDDEN,Json(json!({"error":"Origin not allowed"}))).into_response();}
    }
    let Ok(_permit)=h.api_limit.clone().try_acquire_owned()else{return (StatusCode::TOO_MANY_REQUESTS,Json(json!({"error":"Too many concurrent requests"}))).into_response()};
    next.run(request).await
}
async fn headers(request:Request,next:Next)->Response{
    let mut response=next.run(request).await;
    let h=response.headers_mut();
    h.insert(header::CACHE_CONTROL,"no-store".parse().unwrap());
    h.insert("x-content-type-options","nosniff".parse().unwrap());
    h.insert("referrer-policy","no-referrer".parse().unwrap());
    h.insert("content-security-policy","default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; img-src 'self'; base-uri 'none'; frame-ancestors 'none'; form-action 'self'".parse().unwrap());
    response
}
async fn index()->impl IntoResponse{([(header::CONTENT_TYPE,"text/html; charset=utf-8")],include_str!("../static/index.html"))}
async fn js()->impl IntoResponse{([(header::CONTENT_TYPE,"text/javascript; charset=utf-8")],include_str!("../static/app.js"))}
async fn css()->impl IntoResponse{([(header::CONTENT_TYPE,"text/css; charset=utf-8")],include_str!("../static/style.css"))}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ChatRequest {prompt:String,#[serde(default)]model:Option<String>,#[serde(default)]session_id:Option<String>,#[serde(default)]request_id:Option<String>,#[serde(default="default_scope")]scope:String}
async fn admit_chat(h:&Harness,req:ChatRequest)->ApiResult<Value>{
    safety::scope(&req.scope).map_err(|_|invalid("Invalid scope"))?;
    if req.prompt.trim().is_empty() || req.prompt.len()>16_000{return Err(invalid("Prompt must contain 1-16000 UTF-8 bytes"));}
    let session=req.session_id.unwrap_or_else(storage::uid);let request=req.request_id.unwrap_or_else(storage::uid);
    Uuid::parse_str(&session).map_err(|_|invalid("Invalid session identifier"))?;
    Uuid::parse_str(&request).map_err(|_|invalid("Invalid request identifier"))?;
    let prompt=safety::redact(&req.prompt);let redacted=prompt!=req.prompt;
    if prompt.len()>16_000{return Err(invalid("Sanitized prompt exceeds the size budget"));}
    if req.model.as_ref().is_some_and(|m|m.is_empty() || m.len()>128 || m.chars().any(char::is_control)) {return Err(invalid("Invalid model"));}
    // Fingerprint SANITIZED content only; never retain a brute-forceable hash of a secret.
    // Default-model changes do not turn a repeated request into a new paid generation.
    let signature=safety::fingerprint(&json!({"session":session,"scope":req.scope,"prompt":prompt,"model_override":req.model,"redacted":redacted}).to_string());
    let model=match req.model {Some(m)=>m,None=>h.store.role_model("main",&h.agents.model).await.map_err(db_error)?};
    match h.store.capture_chat(recording::CaptureInput{request,session,scope:req.scope,prompt,model,signature,redacted}).await.map_err(db_error)? {
        recording::Admission::Saved(receipt)=>Ok(receipt),
        recording::Admission::Conflict=>Err(ApiError(StatusCode::CONFLICT,"Request identifier already belongs to different content; nothing new was recorded")),
        recording::Admission::ScopeConflict=>Err(ApiError(StatusCode::CONFLICT,"Session belongs to a different scope; start a new conversation")),
        recording::Admission::Busy=>Err(ApiError(StatusCode::CONFLICT,"This conversation has an unfinished answer; check its saved receipt before sending another message")),
        recording::Admission::Full=>Err(ApiError(StatusCode::SERVICE_UNAVAILABLE,"Recording queue is full. This new message was not accepted; keep your draft and try later")),
    }
}
async fn submit_chat(State(h):State<Harness>,Json(req):Json<ChatRequest>)->ApiResult<(StatusCode,Json<Value>)>{
    let receipt=admit_chat(&h,req).await?;
    let code=if receipt["state"]=="captured" || receipt["state"]=="generating" {StatusCode::ACCEPTED} else {StatusCode::OK};
    Ok((code,Json(receipt)))
}
// Compatibility endpoint: briefly wait for fast providers, otherwise return the durable
// 202 receipt. Disconnecting never owns/cancels the generation worker.
async fn chat(State(h):State<Harness>,Json(req):Json<ChatRequest>)->ApiResult<(StatusCode,Json<Value>)>{
    let mut receipt=admit_chat(&h,req).await?;
    let request=receipt["request_id"].as_str().unwrap().to_string();
    for _ in 0..20 {
        match receipt["state"].as_str() {
            Some("complete")=>return Ok((StatusCode::OK,Json(receipt))),
            Some("failed"|"interrupted")=>return Ok((StatusCode::BAD_GATEWAY,Json(receipt))),
            _=>tokio::time::sleep(std::time::Duration::from_millis(250)).await,
        }
        receipt=h.store.recording_receipt(request.clone()).await.map_err(db_error)?.ok_or(ApiError(StatusCode::NOT_FOUND,"Recording receipt not found"))?;
    }
    Ok((StatusCode::ACCEPTED,Json(receipt)))
}
async fn get_receipt(State(h):State<Harness>,Path(id):Path<String>)->ApiResult<Json<Value>>{
    Uuid::parse_str(&id).map_err(|_|invalid("Invalid request identifier"))?;
    Ok(Json(h.store.recording_receipt(id).await.map_err(db_error)?.ok_or(ApiError(StatusCode::NOT_FOUND,"Recording receipt not found"))?))
}
async fn get_context(State(h):State<Harness>,Path(id):Path<String>)->ApiResult<Json<Value>>{
    Uuid::parse_str(&id).map_err(|_|invalid("Invalid request identifier"))?;
    Ok(Json(h.store.recording_context(id).await.map_err(db_error)?.ok_or(ApiError(StatusCode::NOT_FOUND,"Recording receipt not found"))?))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HistoryQuery { before_seq:Option<i64> }
fn cursor(query:HistoryQuery)->ApiResult<Option<i64>> {
    if query.before_seq.is_some_and(|n|n<=0) {return Err(invalid("History cursor must be positive"));}
    Ok(query.before_seq)
}
async fn sessions(State(h):State<Harness>,Query(q):Query<HistoryQuery>)->ApiResult<Json<Value>>{
    Ok(Json(h.store.recorded_sessions(cursor(q)?).await.map_err(db_error)?))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ConfirmRequest {confirmation_id:String,confirm:bool,#[serde(default="default_scope")]scope:String}
async fn confirm(State(h):State<Harness>,Json(req):Json<ConfirmRequest>)->ApiResult<Json<Value>>{
    safety::scope(&req.scope).map_err(|_|invalid("Invalid scope"))?;
    let status=h.store.resolve(req.confirmation_id,req.scope,req.confirm).await.map_err(db_error)?;
    match status.as_str(){"approved"|"rejected"=>Ok(Json(json!({"status":status}))),"not_found"=>Err(ApiError(StatusCode::NOT_FOUND,"Proposal not found in this scope")),_=>Err(ApiError(StatusCode::CONFLICT,"Proposal is expired, already resolved, or conflicts with a newer revision; reload the inbox"))}
}
#[derive(Deserialize)]struct ScopeQuery{#[serde(default="default_scope")]scope:String}
async fn candidates(State(h):State<Harness>,Query(q):Query<ScopeQuery>)->ApiResult<Json<Value>>{safety::scope(&q.scope).map_err(|_|invalid("Invalid scope"))?;Ok(Json(h.store.candidates(q.scope).await.map_err(db_error)?))}
async fn status(State(h):State<Harness>)->ApiResult<Json<Value>>{Ok(Json(h.store.stats().await.map_err(db_error)?))}
async fn history(State(h):State<Harness>,Path(session):Path<String>,Query(q):Query<HistoryQuery>)->ApiResult<Json<Value>>{Uuid::parse_str(&session).map_err(|_|invalid("Invalid session identifier"))?;Ok(Json(h.store.history(session,cursor(q)?).await.map_err(db_error)?))}
async fn get_config(State(h):State<Harness>)->ApiResult<Json<Value>>{Ok(Json(h.store.settings().await.map_err(db_error)?))}
async fn set_config(State(h):State<Harness>,Json(data):Json<BTreeMap<String,String>>)->ApiResult<Json<Value>>{h.store.set_settings(data).await.map_err(db_error)?;Ok(Json(json!({"status":"saved"})))}
async fn models(State(h):State<Harness>)->ApiResult<Json<Value>>{Ok(Json(h.agents.list_models().await.map_err(|_|ApiError(StatusCode::BAD_GATEWAY,"Unable to load provider models"))?))}
async fn jobs(State(h):State<Harness>)->ApiResult<Json<Value>>{Ok(Json(h.store.jobs().await.map_err(db_error)?))}
async fn retry_job(State(h):State<Harness>,Path(id):Path<String>)->ApiResult<Json<Value>>{if !h.store.retry_job(id).await.map_err(db_error)?{return Err(ApiError(StatusCode::CONFLICT,"Only failed jobs can be retried"));}Ok(Json(json!({"status":"queued"})))}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct IngestRequest{name:String,content:String,#[serde(default)]format:Option<String>,#[serde(default="default_scope")]scope:String}
async fn ingest_memory(State(h):State<Harness>,Json(req):Json<IngestRequest>)->ApiResult<(StatusCode,Json<Value>)>{
    safety::scope(&req.scope).map_err(|_|invalid("Invalid scope"))?;
    if req.content.len()>1_048_576 || req.content.trim().is_empty() || req.name.is_empty() || req.name.len()>200 || req.name.chars().any(char::is_control){return Err(invalid("Import requires a name and 1-1048576 UTF-8 bytes"));}
    // No server-side filesystem paths are accepted. The local CLI uploads bounded files.
    let scope=req.scope;let name=safety::redact(&req.name);
    let parsed=tokio::task::spawn_blocking(move||->Result<_>{
        let fingerprint=safety::fingerprint(&format!("{}\0{}",req.format.as_deref().unwrap_or("auto"),req.content));
        let (format,mut events,mut warnings)=ingest::parse(&req.content,req.format.as_deref())?;
        // Parse first: redacting an entire JSON line before parsing could destroy valid source syntax.
        for event in &mut events {event.content=safety::redact(&event.content);}
        let content=ingest::sanitized_source(&req.content,&format);
        if content!=req.content{warnings.push("Sensitive-looking source lines were redacted; this is not an exact raw archive".into());}
        let chunks=ingest::chunks(&events);Ok((format,content,fingerprint,warnings,chunks))
    }).await.map_err(|_|invalid("Import parser failed"))?.map_err(|_|invalid("Could not parse import; verify format and content"))?;
    let(format,content,fingerprint,warnings,chunks)=parsed;
    let response=h.store.ingest(scope,name,format,content,fingerprint,warnings,chunks).await.map_err(db_error)?;
    Ok((StatusCode::ACCEPTED,Json(response)))
}

fn router(state:Harness)->Router{
    let api=Router::new().route("/chat",post(chat)).route("/chat/submit",post(submit_chat)).route("/chat/requests/{id}",get(get_receipt)).route("/chat/requests/{id}/context",get(get_context)).route("/sessions",get(sessions)).route("/models",get(models)).route("/config",get(get_config).post(set_config))
        .route("/memory/status",get(status)).route("/memory/candidates",get(candidates)).route("/memory/confirm",post(confirm))
        .route("/memory/ingest",post(ingest_memory)).route("/sessions/{id}/messages",get(history))
        .route("/jobs",get(jobs)).route("/jobs/{id}/retry",post(retry_job))
        .route_layer(middleware::from_fn_with_state(state.clone(),authenticate));
    Router::new().route("/",get(index)).route("/app.js",get(js)).route("/style.css",get(css)).merge(api)
        .layer(DefaultBodyLimit::max(2*1024*1024)).layer(middleware::from_fn(headers)).with_state(state)
}

#[tokio::main]
async fn main()->Result<()>{
    dotenvy::dotenv().ok();
    let addr:SocketAddr=env::var("HARNESS_ADDR").unwrap_or_else(|_|"127.0.0.1:8080".into()).parse()?;
    if !addr.ip().is_loopback(){bail!("this single-user release binds only to loopback; use a separately secured deployment design for remote access");}
    let token=env::var("HARNESS_AUTH_TOKEN").map_err(|_|anyhow::anyhow!("HARNESS_AUTH_TOKEN is required (at least 32 random characters)"))?;
    if token.len()<32 || token.len()>256 || !token.is_ascii() || token.chars().any(char::is_whitespace){bail!("HARNESS_AUTH_TOKEN must contain 32-256 non-whitespace ASCII characters");}
    let key=env::var("HARNESS_API_KEY").map_err(|_|anyhow::anyhow!("HARNESS_API_KEY is required"))?;
    if key.trim().is_empty(){bail!("HARNESS_API_KEY cannot be empty");}
    let database=env::var("HARNESS_DB").unwrap_or_else(|_|"data/harness_v2.db".into());
    if let Some(parent)=std::path::Path::new(&database).parent(){if !parent.as_os_str().is_empty(){std::fs::create_dir_all(parent)?;}}
    let store=DbStore::init(&database)?;
    #[cfg(unix)] {use std::os::unix::fs::PermissionsExt;for suffix in ["","-wal","-shm"]{let path=format!("{database}{suffix}");if std::path::Path::new(&path).exists(){std::fs::set_permissions(path,std::fs::Permissions::from_mode(0o600))?;}}}
    let agents=MemoryAgents::new(&env::var("HARNESS_BASE_URL").unwrap_or_else(|_|"https://api.longcat.chat/openai".into()),&key,&env::var("HARNESS_MODEL").unwrap_or_else(|_|"LongCat-2.0".into()))?;
    let state=Harness{store:store.clone(),agents:agents.clone(),token:Arc::new(token),port:addr.port(),api_limit:Arc::new(Semaphore::new(8))};
    let listener=tokio::net::TcpListener::bind(addr).await?;
    tokio::spawn(recording::worker(store.clone(),agents.clone()));
    tokio::spawn(memory_agents::worker(store,agents));
    println!("harness listening on http://{addr} (authenticated, single-user)");
    axum::serve(listener,router(state)).await?;
    Ok(())
}

#[cfg(test)]
mod tests{
    use super::*;use axum::body::Body;use tower::ServiceExt;
    fn app()->Router{router(Harness{store:DbStore::init(":memory:").unwrap(),agents:MemoryAgents::new("http://127.0.0.1:9","synthetic","test").unwrap(),token:Arc::new("x".repeat(32)),port:8080,api_limit:Arc::new(Semaphore::new(8))})}
    #[tokio::test]async fn api_requires_auth(){let response=app().oneshot(axum::http::Request::builder().uri("/memory/status").body(Body::empty()).unwrap()).await.unwrap();assert_eq!(response.status(),StatusCode::UNAUTHORIZED);}
    #[tokio::test]async fn authenticated_status_succeeds(){let response=app().oneshot(axum::http::Request::builder().uri("/memory/status").header("Authorization",format!("Bearer {}","x".repeat(32))).body(Body::empty()).unwrap()).await.unwrap();assert_eq!(response.status(),StatusCode::OK);}
    #[tokio::test]async fn foreign_origin_is_rejected(){let response=app().oneshot(axum::http::Request::builder().uri("/memory/status").header("Authorization",format!("Bearer {}","x".repeat(32))).header("Origin","https://untrusted.invalid").body(Body::empty()).unwrap()).await.unwrap();assert_eq!(response.status(),StatusCode::FORBIDDEN);}
}

#[cfg(test)]
mod recording_tests;
