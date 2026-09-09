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
mod agent_loop;  // P1-T10 agentic turn loop: steps, tools, activity events, budgets
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

// P1-T04: a scope owns a project root. Tools stay disabled until root_path is set, and an
// unmentioned field keeps its stored value so a partial POST cannot silently disable them.
async fn get_scope(State(h):State<Harness>,Path(scope):Path<String>)->ApiResult<Json<storage::ScopeConfig>>{
    safety::scope(&scope).map_err(|_|invalid("Invalid scope"))?;
    Ok(Json(h.store.scope_config(scope).await.map_err(db_error)?.ok_or(ApiError(StatusCode::NOT_FOUND,"This scope has no project configuration yet"))?))
}
async fn set_scope(State(h):State<Harness>,Path(scope):Path<String>,Json(patch):Json<storage::ScopePatch>)->ApiResult<Json<storage::ScopeConfig>>{
    safety::scope(&scope).map_err(|_|invalid("Invalid scope"))?;
    let patch=patch.validate().map_err(invalid)?;
    Ok(Json(h.store.upsert_scope(scope,patch).await.map_err(db_error)?))
}

// P1-T11: the human half of the permission gate. The turn loop only ever reads the row's status,
// so these two endpoints are the only thing that can let a side-effecting tool run.
async fn permissions(State(h):State<Harness>,Query(q):Query<ScopeQuery>)->ApiResult<Json<Value>>{
    safety::scope(&q.scope).map_err(|_|invalid("Invalid scope"))?;
    Ok(Json(h.store.pending_permissions(q.scope).await.map_err(db_error)?))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DecisionRequest{decision:String,#[serde(default="default_scope")]scope:String}
async fn decide_permission(State(h):State<Harness>,Path(id):Path<String>,Json(req):Json<DecisionRequest>)->ApiResult<Json<Value>>{
    safety::scope(&req.scope).map_err(|_|invalid("Invalid scope"))?;
    Uuid::parse_str(&id).map_err(|_|invalid("Invalid approval identifier"))?;
    let decision=match req.decision.as_str(){"approve"=>"approved","deny"=>"denied",_=>return Err(invalid("Decision must be \"approve\" or \"deny\""))};
    // Re-sending the same decision is a success: a double-clicked Approve must not become an error.
    match h.store.resolve_permission(id,req.scope,decision).await.map_err(db_error)?{
        agent_loop::Resolution::Recorded=>Ok(Json(json!({"status":decision,"recorded":true}))),
        agent_loop::Resolution::Unchanged=>Ok(Json(json!({"status":decision,"recorded":false}))),
        agent_loop::Resolution::Conflict=>Err(ApiError(StatusCode::CONFLICT,"This approval was already resolved the other way; nothing was changed")),
        agent_loop::Resolution::Expired=>Err(ApiError(StatusCode::GONE,"This approval expired and the turn stopped waiting; nothing was run")),
        agent_loop::Resolution::NotFound=>Err(ApiError(StatusCode::NOT_FOUND,"Approval request not found in this scope")),
    }
}

// P1-T12: the read side. Every field below is a row the loop already committed, so what the UI
// shows is the record itself and not a second, prettier version of it.
async fn request_steps(State(h):State<Harness>,Path(id):Path<String>)->ApiResult<Json<Value>>{
    Uuid::parse_str(&id).map_err(|_|invalid("Invalid request identifier"))?;
    // 404 on an unknown turn: an empty step list would otherwise read as "this turn did nothing".
    h.store.recording_receipt(id.clone()).await.map_err(db_error)?.ok_or(ApiError(StatusCode::NOT_FOUND,"Recording receipt not found"))?;
    Ok(Json(h.store.turn_steps(id).await.map_err(db_error)?))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RequestQuery{request_id:String}
async fn request_changes(State(h):State<Harness>,Query(q):Query<RequestQuery>)->ApiResult<Json<Value>>{
    Uuid::parse_str(&q.request_id).map_err(|_|invalid("Invalid request identifier"))?;
    h.store.recording_receipt(q.request_id.clone()).await.map_err(db_error)?.ok_or(ApiError(StatusCode::NOT_FOUND,"Recording receipt not found"))?;
    Ok(Json(h.store.turn_changes(q.request_id).await.map_err(db_error)?))
}
async fn session_plan(State(h):State<Harness>,Path(session):Path<String>)->ApiResult<Json<Value>>{
    Uuid::parse_str(&session).map_err(|_|invalid("Invalid session identifier"))?;
    // A session with no plan and a session that does not exist both read as empty, exactly as
    // `/sessions/{id}/messages` does. The plan is a view of the session, not proof it exists.
    Ok(Json(h.store.plan(session).await.map_err(db_error)?))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ActivityQuery{session_id:String,#[serde(default)]after_seq:Option<i64>}
async fn activity(State(h):State<Harness>,Query(q):Query<ActivityQuery>)->ApiResult<Json<Value>>{
    Uuid::parse_str(&q.session_id).map_err(|_|invalid("Invalid session identifier"))?;
    let after=q.after_seq.unwrap_or(0);
    if after<0 {return Err(invalid("Activity cursor cannot be negative"));}
    Ok(Json(h.store.activity_since(q.session_id,after).await.map_err(db_error)?))
}

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
        .route("/scopes/{scope}",get(get_scope).post(set_scope))
        .route("/permissions",get(permissions)).route("/permissions/{id}",post(decide_permission))
        .route("/chat/requests/{id}/steps",get(request_steps)).route("/sessions/{id}/plan",get(session_plan))
        .route("/activity",get(activity)).route("/changes",get(request_changes))
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
    fn app_with(store:DbStore)->Router{router(Harness{store,agents:MemoryAgents::new("http://127.0.0.1:9","synthetic","test").unwrap(),token:Arc::new("x".repeat(32)),port:8080,api_limit:Arc::new(Semaphore::new(8))})}
    fn app()->Router{app_with(DbStore::init(":memory:").unwrap())}
    #[tokio::test]async fn api_requires_auth(){let response=app().oneshot(axum::http::Request::builder().uri("/memory/status").body(Body::empty()).unwrap()).await.unwrap();assert_eq!(response.status(),StatusCode::UNAUTHORIZED);}
    #[tokio::test]async fn authenticated_status_succeeds(){let response=app().oneshot(axum::http::Request::builder().uri("/memory/status").header("Authorization",format!("Bearer {}","x".repeat(32))).body(Body::empty()).unwrap()).await.unwrap();assert_eq!(response.status(),StatusCode::OK);}
    #[tokio::test]async fn foreign_origin_is_rejected(){let response=app().oneshot(axum::http::Request::builder().uri("/memory/status").header("Authorization",format!("Bearer {}","x".repeat(32))).header("Origin","https://untrusted.invalid").body(Body::empty()).unwrap()).await.unwrap();assert_eq!(response.status(),StatusCode::FORBIDDEN);}
    fn authorized(method:&str,uri:&str)->axum::http::request::Builder{axum::http::Request::builder().method(method).uri(uri).header("Authorization",format!("Bearer {}","x".repeat(32)))}
    async fn body_json(response:Response)->Value{serde_json::from_slice(&axum::body::to_bytes(response.into_body(),64*1024).await.unwrap()).unwrap()}
    #[tokio::test]async fn scopes_are_absent_until_configured_then_read_back_canonicalized(){
        let app=app();
        let missing=app.clone().oneshot(authorized("GET","/scopes/global").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(missing.status(),StatusCode::NOT_FOUND);
        let dir=std::env::temp_dir().join(format!("harness-api-scope-{}",storage::uid()));
        std::fs::create_dir_all(&dir).unwrap();
        let canonical=std::fs::canonicalize(&dir).unwrap();
        let request=json!({"root_path":dir.to_string_lossy(),"permission_mode":"auto_all","diagnostics_cmd":"cargo check -q"}).to_string();
        let saved=app.clone().oneshot(authorized("POST","/scopes/global").header("content-type","application/json").body(Body::from(request)).unwrap()).await.unwrap();
        assert_eq!(saved.status(),StatusCode::OK);
        assert_eq!(body_json(saved).await["root_path"],json!(canonical.to_string_lossy()));
        let stored=app.clone().oneshot(authorized("GET","/scopes/global").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(stored.status(),StatusCode::OK);
        let row=body_json(stored).await;
        assert_eq!((&row["permission_mode"],&row["diagnostics_cmd"],&row["max_steps"]),(&json!("auto_all"),&json!("cargo check -q"),&Value::Null));
        std::fs::remove_dir_all(dir).ok();
    }
    #[tokio::test]async fn scopes_refuse_a_root_path_that_is_not_an_existing_directory(){
        let request=json!({"root_path":"/definitely/not/a/real/harness/project/root"}).to_string();
        let response=app().oneshot(authorized("POST","/scopes/global").header("content-type","application/json").body(Body::from(request)).unwrap()).await.unwrap();
        assert_eq!(response.status(),StatusCode::BAD_REQUEST);
    }

    /// P1-T11: one pending approval, listed and then decided over HTTP. The loop is not involved;
    /// what matters is that the row moves exactly once and says so.
    #[tokio::test]async fn pending_permissions_are_listed_then_resolved_idempotently(){
        let store=DbStore::init(":memory:").unwrap();
        let app=app_with(store.clone());
        let (request,session)=(storage::uid(),storage::uid());
        store.capture_chat(recording::CaptureInput{request:request.clone(),session:session.clone(),scope:"global".into(),
            prompt:"overwrite my notes".into(),model:"m".into(),signature:storage::uid(),redacted:false}).await.unwrap();
        let step=store.begin_step(agent_loop::NewStep{request:request.clone(),session:session.clone(),kind:"tool_call",
            tool_name:Some("write".into()),tool_call_id:Some("call-1".into()),input:json!({"path":"notes.md"}),
            event:"tool_started",payload:json!({"tool":"write","summary":"write notes.md"})}).await.unwrap();
        let id=store.request_permission(agent_loop::NewPermission{request,session,step,tool:"write".into(),
            summary:"write notes.md (+1 -1)".into(),args:json!({"diff":"-beta\n+gamma"}),ttl_seconds:900}).await.unwrap();

        let listed=app.clone().oneshot(authorized("GET","/permissions?scope=global").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(listed.status(),StatusCode::OK);
        let pending=body_json(listed).await;
        assert_eq!((&pending["permissions"][0]["id"],&pending["permissions"][0]["tool"]),(&json!(id),&json!("write")));
        assert_eq!(pending["permissions"][0]["summary"],json!("write notes.md (+1 -1)"));
        assert_eq!(pending["permissions"][0]["args"]["diff"],json!("-beta\n+gamma"),"the card shows the tool's own diff, not model prose");

        let decide=|decision:&str|authorized("POST",&format!("/permissions/{id}")).header("content-type","application/json")
            .body(Body::from(json!({"decision":decision,"scope":"global"}).to_string())).unwrap();
        let first=app.clone().oneshot(decide("approve")).await.unwrap();
        assert_eq!(first.status(),StatusCode::OK);
        assert_eq!(body_json(first).await,json!({"status":"approved","recorded":true}));
        let replay=app.clone().oneshot(decide("approve")).await.unwrap();
        assert_eq!(replay.status(),StatusCode::OK,"a double-clicked Approve is not an error");
        assert_eq!(body_json(replay).await["recorded"],json!(false));
        let flip=app.clone().oneshot(decide("deny")).await.unwrap();
        assert_eq!(flip.status(),StatusCode::CONFLICT);
        let nonsense=app.clone().oneshot(decide("maybe")).await.unwrap();
        assert_eq!(nonsense.status(),StatusCode::BAD_REQUEST);
        let stranger=app.clone().oneshot(authorized("POST",&format!("/permissions/{}",storage::uid())).header("content-type","application/json")
            .body(Body::from(json!({"decision":"approve"}).to_string())).unwrap()).await.unwrap();
        assert_eq!(stranger.status(),StatusCode::NOT_FOUND);

        let resolved:i64=store.run(|c|Ok(c.query_row("SELECT count(*) FROM activity_events WHERE kind='permission_resolved'",[],|r|r.get(0))?)).await.unwrap();
        assert_eq!(resolved,1,"an idempotent replay must not log a second decision");
        let empty=app.oneshot(authorized("GET","/permissions?scope=global").body(Body::empty()).unwrap()).await.unwrap();
        assert!(body_json(empty).await["permissions"].as_array().unwrap().is_empty(),"a resolved approval leaves the pending list");
    }
    #[tokio::test]async fn permissions_require_auth(){
        let response=app().oneshot(axum::http::Request::builder().uri("/permissions").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(response.status(),StatusCode::UNAUTHORIZED);
    }

    /// P1-T12: steps, plan and activity are read back from the rows the loop wrote. This builds
    /// those rows directly (the loop's own coverage lives in `agent_loop`) and checks the shape,
    /// the bounds and the cursor the UI will depend on.
    #[tokio::test]async fn steps_plan_and_activity_read_back_what_the_loop_recorded(){
        let store=DbStore::init(":memory:").unwrap();
        let app=app_with(store.clone());
        let (request,session)=(storage::uid(),storage::uid());
        store.capture_chat(recording::CaptureInput{request:request.clone(),session:session.clone(),scope:"global".into(),
            prompt:"rename beta to gamma".into(),model:"m".into(),signature:storage::uid(),redacted:false}).await.unwrap();

        // A model call whose stored message array is far larger than the 2 KB preview.
        let wall="x".repeat(4096);
        let first=store.begin_step(agent_loop::NewStep{request:request.clone(),session:session.clone(),kind:"model_call",
            tool_name:None,tool_call_id:None,input:json!({"messages":[{"role":"user","content":wall}]}),
            event:"model_call_started",payload:json!({"attempt":1})}).await.unwrap();
        let mut done=agent_loop::StepOutcome{step:first,request:request.clone(),session:session.clone(),status:"complete",
            output:json!({"text":Value::Null,"tool_calls":[{"name":"edit"}]}),bytes:64,truncated:false,
            tokens_in:Some(11),tokens_out:Some(7),error_code:None,event:"model_call_finished",
            payload:json!({"tokens_in":11,"tokens_out":7,"tool_call_count":1}),artifacts:vec![]};
        store.finish_step(done).await.unwrap();

        // A tool call that changed a file and wrote the plan, exactly as `finish_step` does.
        let second=store.begin_step(agent_loop::NewStep{request:request.clone(),session:session.clone(),kind:"tool_call",
            tool_name:Some("edit".into()),tool_call_id:Some("call-1".into()),input:json!({"path":"notes.md"}),
            event:"tool_started",payload:json!({"tool":"edit","summary":"edit notes.md (+1 -1)"})}).await.unwrap();
        done=agent_loop::StepOutcome{step:second,request:request.clone(),session:session.clone(),status:"complete",
            output:json!({"content":"edited","summary":"edit notes.md (+1 -1)","error_code":Value::Null}),bytes:1234,truncated:true,
            tokens_in:None,tokens_out:None,error_code:None,event:"tool_finished",payload:json!({"tool":"edit","status":"complete"}),
            artifacts:vec![tools::Artifact::FileChange{path:"notes.md".into(),action:"modify",before_hash:Some("aaaa".into()),
                after_hash:Some("bbbb".into()),diff:"-beta\n+gamma".into(),plus:1,minus:1},
                tools::Artifact::Plan{items:vec![("read notes.md".into(),"done".into()),("rename beta".into(),"in_progress".into())]}]};
        store.finish_step(done).await.unwrap();

        let steps=body_json(app.clone().oneshot(authorized("GET",&format!("/chat/requests/{request}/steps")).body(Body::empty()).unwrap()).await.unwrap()).await;
        let steps=steps["steps"].as_array().unwrap().clone();
        assert_eq!(steps.len(),2);
        assert_eq!((&steps[0]["seq"],&steps[0]["kind"],&steps[0]["status"]),(&json!(0),&json!("model_call"),&json!("complete")));
        assert_eq!((&steps[0]["tokens_in"],&steps[0]["tokens_out"]),(&json!(11),&json!(7)));
        assert_eq!(steps[0]["input_preview"].as_str().unwrap().len(),storage::PREVIEW_BYTES,"a huge message array is cut to the preview cap");
        assert_eq!(steps[0]["previews_capped"],json!(true),"the client has to know the preview is not the whole story");
        assert_eq!(steps[0]["summary"],Value::Null,"only a tool names itself");
        assert_eq!((&steps[1]["tool_name"],&steps[1]["summary"]),(&json!("edit"),&json!("edit notes.md (+1 -1)")));
        assert_eq!((&steps[1]["output_bytes"],&steps[1]["truncated"],&steps[1]["previews_capped"]),(&json!(1234),&json!(true),&json!(false)),
            "a capped tool output and a capped preview are different facts");

        let plan=body_json(app.clone().oneshot(authorized("GET",&format!("/sessions/{session}/plan")).body(Body::empty()).unwrap()).await.unwrap()).await;
        assert_eq!(plan["items"].as_array().unwrap().len(),2);
        assert_eq!((&plan["items"][0]["text"],&plan["items"][1]["status"]),(&json!("read notes.md"),&json!("in_progress")));

        let feed=body_json(app.clone().oneshot(authorized("GET",&format!("/activity?session_id={session}")).body(Body::empty()).unwrap()).await.unwrap()).await;
        let kinds:Vec<&str>=feed["events"].as_array().unwrap().iter().map(|e|e["kind"].as_str().unwrap()).collect();
        // `activity_events` is the agentic feed only; the receipt timeline (`captured`, ...) stays
        // in `recording_events` behind `/chat/requests/{id}/context`.
        assert_eq!(kinds,vec!["model_call_started","model_call_finished","tool_started","tool_finished","file_changed","plan_updated"],"{feed:#?}");
        assert_eq!(feed["events"][2]["payload"]["summary"],json!("edit notes.md (+1 -1)"),"a running step's name lives in its event");
        assert_eq!(feed["events"][4]["payload"]["path"],json!("notes.md"));
        let cursor=feed["next_after_seq"].as_i64().unwrap();
        assert_eq!(cursor,feed["events"].as_array().unwrap().last().unwrap()["seq"].as_i64().unwrap());
        let tail=body_json(app.clone().oneshot(authorized("GET",&format!("/activity?session_id={session}&after_seq={cursor}")).body(Body::empty()).unwrap()).await.unwrap()).await;
        assert!(tail["events"].as_array().unwrap().is_empty(),"the cursor must not replay events");
        assert_eq!(tail["next_after_seq"],json!(cursor),"an empty poll leaves the cursor where it was");

        let changes=body_json(app.clone().oneshot(authorized("GET",&format!("/changes?request_id={request}")).body(Body::empty()).unwrap()).await.unwrap()).await;
        assert_eq!(changes["changes"].as_array().unwrap().len(),1);
        assert_eq!((&changes["changes"][0]["path"],&changes["changes"][0]["applied"],&changes["changes"][0]["diff"]),
            (&json!("notes.md"),&json!(true),&json!("-beta\n+gamma")));

        // Bounds and identity.
        for (uri,expected) in [
            (format!("/chat/requests/{}/steps",storage::uid()),StatusCode::NOT_FOUND),
            ("/chat/requests/not-a-uuid/steps".to_string(),StatusCode::BAD_REQUEST),
            (format!("/changes?request_id={}",storage::uid()),StatusCode::NOT_FOUND),
            (format!("/activity?session_id={session}&after_seq=-1"),StatusCode::BAD_REQUEST),
            ("/activity?session_id=nope".to_string(),StatusCode::BAD_REQUEST),
            ("/activity".to_string(),StatusCode::BAD_REQUEST),
            (format!("/sessions/{}/plan","not-a-uuid"),StatusCode::BAD_REQUEST),
        ]{
            let response=app.clone().oneshot(authorized("GET",&uri).body(Body::empty()).unwrap()).await.unwrap();
            assert_eq!(response.status(),expected,"{uri}");
        }
        for uri in [format!("/chat/requests/{request}/steps"),format!("/sessions/{session}/plan"),format!("/activity?session_id={session}"),format!("/changes?request_id={request}")]{
            let response=app.clone().oneshot(axum::http::Request::builder().uri(&uri).body(Body::empty()).unwrap()).await.unwrap();
            assert_eq!(response.status(),StatusCode::UNAUTHORIZED,"{uri}");
        }
        let unknown=body_json(app.oneshot(authorized("GET",&format!("/sessions/{}/plan",storage::uid())).body(Body::empty()).unwrap()).await.unwrap()).await;
        assert_eq!(unknown,json!({"items":[]}),"a session without a plan reads as empty, like its message history");
    }
}

#[cfg(test)]
mod recording_tests;
