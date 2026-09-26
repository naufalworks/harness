#![deny(
    clippy::dbg_macro,
    clippy::todo,
    clippy::unimplemented,
    clippy::undocumented_unsafe_blocks
)]

use anyhow::{bail, Result};
use axum::http::header;
use serde_json::json;
use std::{
    env,
    fmt::Write as _,
    net::SocketAddr,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::sync::Semaphore;
mod agent_loop; // P1-T10 agentic turn loop: steps, tools, activity events, budgets
mod agentic_sql; // P1 SQL constants (schema 003); contract-tested by tests/test_agentic_sql.py
mod api; // P12-T01 HTTP surface split into cohesive modules
mod archive; // P13 opt-in exact-original encryption and privacy audit policy
mod backup; // P20 full internal recovery snapshots, separate from reviewed exports
mod context; // P3-T01 deterministic initial window and per-category byte receipts
mod embeddings;
#[allow(dead_code)]
mod experiments; // P17 contracts are test-covered but not yet wired to the production surface
mod export; // P15-T04 reviewed sanitized export/import and the continuation-packet shape
mod ingest;
mod limits; // P12-T04 bounds that two modules must agree on, defined once
mod memory_agents;
mod patch; // P12-T04 explicit absent/null/value semantics for PATCH bodies
mod plugins; // P18-T02 digest-pinned bounded extensions: providers, plugins, benchmark packs
mod process_lock;
mod processes; // P14-T01 live process-group handles so an explicit cancel can stop blocked work
mod providers; // P21 runtime provider profiles; secrets stay outside SQLite and Git
mod recording;
mod recording_sql;
mod repo_map; // P3-T04 bounded per-scope file/symbol map
mod runtime_observability; // P16-T04 bounded content-free turn timing
mod safety;
mod skills; // P5-T02 skills index and bounded SKILL.md bodies
mod storage;
mod subagent; // P5-T03 read-only exploration sub-agent: tools, bounds, report shape
mod tools; // P1-T05/T06 tool registry (needs-verify: written without cargo) // P4-T02 deterministic offline vectors and cosine scoring
use api::auth::AuthState;
use api::routes::router;
use process_lock::ProcessLock;
use providers::ProviderRegistry;
use storage::DbStore;

const BUILD_COMMIT: &str = env!("HARNESS_GIT_COMMIT");

struct RuntimeIdentity {
    commit: &'static str,
    binary_sha256: String,
    started_at: String,
}
impl RuntimeIdentity {
    fn current() -> Result<Self> {
        let bytes = std::fs::read(std::env::current_exe()?)?;
        let digest = ring::digest::digest(&ring::digest::SHA256, &bytes);
        let mut binary_sha256 = String::with_capacity(64);
        for byte in digest.as_ref() {
            write!(&mut binary_sha256, "{byte:02x}")?;
        }
        Ok(Self {
            commit: BUILD_COMMIT,
            binary_sha256,
            started_at: chrono::Utc::now().to_rfc3339(),
        })
    }
}

#[derive(Default)]
struct WorkerHealth {
    recording: AtomicBool,
    extraction: AtomicBool,
}
#[cfg(test)]
impl WorkerHealth {
    fn ready() -> Self {
        Self {
            recording: AtomicBool::new(true),
            extraction: AtomicBool::new(true),
        }
    }
}

#[derive(Clone)]
struct Harness {
    store: DbStore,
    providers: ProviderRegistry,
    project_browse_roots: Arc<Vec<std::path::PathBuf>>,
    auth: Arc<AuthState>,
    port: u16,
    origins: Arc<Vec<String>>,
    api_limit: Arc<Semaphore>,
    identity: Arc<RuntimeIdentity>,
    workers: Arc<WorkerHealth>,
    hsts: bool,
    /// `None` when no archive key is configured. The archive routes then refuse explicitly
    /// instead of implying that exact bytes were stored.
    archive: Option<Arc<archive::ArchiveStore>>,
}

static SHUTDOWN_REQUESTED: AtomicBool = AtomicBool::new(false);
#[cfg(unix)]
extern "C" fn request_shutdown(_: libc::c_int) {
    SHUTDOWN_REQUESTED.store(true, Ordering::Release);
}
async fn shutdown_signal(shutdown: tokio::sync::watch::Sender<bool>) {
    #[cfg(unix)]
    // SAFETY: both signals are installed with a C-compatible function pointer. The handler
    // performs only a lock-free AtomicBool store and does not touch allocator-backed state.
    unsafe {
        let handler = request_shutdown as *const () as libc::sighandler_t;
        libc::signal(libc::SIGINT, handler);
        libc::signal(libc::SIGTERM, handler);
    }
    while !SHUTDOWN_REQUESTED.load(Ordering::Acquire) {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    eprintln!("{}", json!({"event":"graceful_shutdown_started"}));
    let _ = shutdown.send(true);
}

#[tokio::main]
async fn main() -> Result<()> {
    if let Some(result) = backup::maybe_run_cli() {
        return result;
    }
    dotenvy::dotenv().ok();
    let addr: SocketAddr = env::var("HARNESS_ADDR")
        .unwrap_or_else(|_| "127.0.0.1:8080".into())
        .parse()?;
    if !addr.ip().is_loopback() {
        bail!("this single-user release binds only to loopback; use a separately secured deployment design for remote access");
    }
    let token = env::var("HARNESS_AUTH_TOKEN").map_err(|_| {
        anyhow::anyhow!("HARNESS_AUTH_TOKEN is required (at least 32 random characters)")
    })?;
    if token.len() < 32
        || token.len() > 256
        || !token.is_ascii()
        || token.chars().any(char::is_whitespace)
    {
        bail!("HARNESS_AUTH_TOKEN must contain 32-256 non-whitespace ASCII characters");
    }
    let previous_token = env::var("HARNESS_AUTH_TOKEN_PREVIOUS")
        .ok()
        .filter(|value| !value.is_empty());
    if previous_token.as_deref().is_some_and(|value| {
        value == token
            || value.len() < 32
            || value.len() > 256
            || !value.is_ascii()
            || value.chars().any(char::is_whitespace)
    }) {
        bail!("HARNESS_AUTH_TOKEN_PREVIOUS must be distinct and contain 32-256 non-whitespace ASCII characters");
    }
    let session_ttl = env::var("HARNESS_SESSION_TTL_SECONDS")
        .ok()
        .map(|value| value.parse::<u64>())
        .transpose()?
        .unwrap_or(900);
    if !(300..=3600).contains(&session_ttl) {
        bail!("HARNESS_SESSION_TTL_SECONDS must be between 300 and 3600");
    }
    let proxy_identity_header = env::var("HARNESS_PROXY_IDENTITY_HEADER")
        .ok()
        .filter(|value| !value.is_empty())
        .map(|value| header::HeaderName::from_bytes(value.as_bytes()))
        .transpose()?;
    let hsts = env::var("HARNESS_HTTPS_HSTS").is_ok_and(|value| value == "1");
    let key =
        env::var("HARNESS_API_KEY").map_err(|_| anyhow::anyhow!("HARNESS_API_KEY is required"))?;
    if key.trim().is_empty() {
        bail!("HARNESS_API_KEY cannot be empty");
    }
    let database = env::var("HARNESS_DB").unwrap_or_else(|_| "data/harness_v2.db".into());
    if let Some(parent) = std::path::Path::new(&database).parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    // Ownership must be proven before SQLite is opened: DbStore::init runs crash recovery, so a
    // rejected contender must never interrupt the live owner's jobs or receipts.
    let _database_lock = ProcessLock::acquire(&database)?;
    let store = DbStore::init(&database)?;
    // Capture a known-good memory floor after migrations and before workers start. The health
    // endpoint can then distinguish an empty fresh database from an unexpected memory reset.
    store.record_memory_health_baseline().await?;
    let memory_health = store.memory_health().await?;
    if memory_health["status"] == "MEMORY_RESET_DETECTED" {
        bail!("memory reset detected; refusing to start workers");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for suffix in ["", "-wal", "-shm"] {
            let path = format!("{database}{suffix}");
            if std::path::Path::new(&path).exists() {
                std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
            }
        }
    }
    let base_url =
        env::var("HARNESS_BASE_URL").unwrap_or_else(|_| "https://api.longcat.chat/openai".into());
    let default_model = env::var("HARNESS_MODEL").unwrap_or_else(|_| "LongCat-2.0".into());
    let providers = ProviderRegistry::open(&base_url, &key, &default_model, store.clone())?;
    let project_browse_roots =
        tools::paths::browse_roots(&env::var("HARNESS_PROJECT_BROWSE_ROOTS").unwrap_or_default())
            .map_err(|error| {
            anyhow::anyhow!("invalid HARNESS_PROJECT_BROWSE_ROOTS: {}", error.detail())
        })?;
    let mut origins = vec![
        format!("http://127.0.0.1:{}", addr.port()),
        format!("http://localhost:{}", addr.port()),
        format!("http://[::1]:{}", addr.port()),
    ];
    if let Ok(extra) = env::var("HARNESS_ALLOWED_ORIGINS") {
        for candidate in extra.split(',').map(str::trim).filter(|c| !c.is_empty()) {
            if !origins.iter().any(|o| o == candidate) {
                origins.push(candidate.to_string());
            }
        }
    }
    let identity = Arc::new(RuntimeIdentity::current()?);
    let workers = Arc::new(WorkerHealth::default());
    // A configured-but-broken archive key must stop startup: booting without it would leave the
    // operator believing exact originals are being retained when nothing is being stored.
    let archive = archive::ArchiveStore::open_from_env()?.map(Arc::new);
    eprintln!(
        "{}",
        json!({"event":"archive_configured","enabled":archive.is_some()})
    );
    let state = Harness {
        store: store.clone(),
        providers: providers.clone(),
        project_browse_roots: Arc::new(project_browse_roots),
        auth: Arc::new(
            AuthState::new(
                token,
                previous_token,
                Duration::from_secs(session_ttl),
                proxy_identity_header,
            )
            .with_producers_from_env()?,
        ),
        port: addr.port(),
        origins: Arc::new(origins),
        api_limit: Arc::new(Semaphore::new(8)),
        identity,
        workers: workers.clone(),
        hsts,
        archive,
    };
    let listener = tokio::net::TcpListener::bind(addr).await?;
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    workers.recording.store(true, Ordering::Release);
    let worker_health = workers.clone();
    let recording_store = store.clone();
    let recording_providers = providers.clone();
    let recording_shutdown = shutdown_rx.clone();
    let recording_handle = tokio::spawn(async move {
        recording::worker(recording_store, recording_providers, recording_shutdown).await;
        worker_health.recording.store(false, Ordering::Release);
    });
    workers.extraction.store(true, Ordering::Release);
    let worker_health = workers.clone();
    let extraction_handle = tokio::spawn(async move {
        memory_agents::worker(store, providers, shutdown_rx).await;
        worker_health.extraction.store(false, Ordering::Release);
    });
    println!("harness listening on {{http://{addr}}} (authenticated, single-user)");
    axum::serve(listener, router(state))
        .with_graceful_shutdown(shutdown_signal(shutdown_tx.clone()))
        .await?;
    let _ = shutdown_tx.send(true);
    let drain_seconds = env::var("HARNESS_SHUTDOWN_TIMEOUT_SECONDS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|v| (1..=300).contains(v))
        .unwrap_or(30);
    let drained = tokio::time::timeout(Duration::from_secs(drain_seconds), async {
        let _ = recording_handle.await;
        let _ = extraction_handle.await;
    })
    .await;
    if drained.is_err() {
        eprintln!(
            "{}",
            json!({"event":"graceful_shutdown_timeout","seconds":drain_seconds})
        );
    } else {
        eprintln!("{}", json!({"event":"graceful_shutdown_complete"}));
    }
    Ok(())
}

#[cfg(test)]
#[path = "main_tests.rs"]
mod tests;

#[cfg(test)]
mod recording_tests;
