//! P21 runtime provider registry.
//!
//! Secrets live in a private, atomic JSON file under .harness rather than SQLite.
//! Public API projections never serialize API keys. Each saved provider change appends
//! a version so an already-admitted turn can keep using the provider version it recorded.

use crate::{memory_agents::MemoryAgents, storage::DbStore};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, HashMap},
    fs::{self, OpenOptions},
    io::{Read, Write},
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, RwLock},
};

const FORMAT_VERSION: u32 = 1;
const ENVIRONMENT_PROVIDER: &str = "environment";
const MAX_PROVIDERS: usize = 32;
const MAX_VERSIONS_PER_PROVIDER: usize = 64;
const MAX_STORE_BYTES: u64 = 512 * 1024;
const MAX_KEY_BYTES: usize = 4096;
const MAX_URL_BYTES: usize = 2048;

#[derive(Clone)]
pub(crate) struct ProviderRegistry {
    inner: Arc<RegistryInner>,
}

struct RegistryInner {
    path: PathBuf,
    state: RwLock<ProviderFile>,
    environment: ProviderVersion,
    default_model: String,
    spend_store: DbStore,
    allow_loopback: bool,
    cache: Mutex<HashMap<(String, u64), MemoryAgents>>,
    mutation: tokio::sync::Mutex<()>,
}

#[derive(Clone, Serialize, Deserialize)]
struct ProviderFile {
    format_version: u32,
    selected: String,
    providers: BTreeMap<String, ProviderRecord>,
}

impl Default for ProviderFile {
    fn default() -> Self {
        Self {
            format_version: FORMAT_VERSION,
            selected: ENVIRONMENT_PROVIDER.into(),
            providers: BTreeMap::new(),
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
struct ProviderRecord {
    current_version: u64,
    #[serde(default)]
    deleted: bool,
    versions: Vec<ProviderVersion>,
}

#[derive(Clone, Serialize, Deserialize)]
struct ProviderVersion {
    version: u64,
    base_url: String,
    api: String,
    discovery: Discovery,
    api_key: String,
    updated_at: String,
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct Discovery {
    #[serde(rename = "type")]
    pub kind: String,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ProviderInput {
    pub id: String,
    pub base_url: String,
    #[serde(default)]
    pub api_key: Option<String>,
    pub api: String,
    pub discovery: Discovery,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ProviderPublic {
    pub id: String,
    pub base_url: String,
    pub api: String,
    pub discovery: Discovery,
    pub version: u64,
    pub selected: bool,
    pub key_present: bool,
    pub source: &'static str,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ProviderRef {
    pub id: String,
    pub version: u64,
    pub default_model: String,
}

impl ProviderRegistry {
    pub(crate) fn open(
        environment_base_url: &str,
        environment_api_key: &str,
        default_model: &str,
        spend_store: DbStore,
    ) -> Result<Self> {
        let environment = ProviderVersion {
            version: 1,
            base_url: environment_base_url.trim_end_matches('/').to_string(),
            api: "openai-completions".into(),
            discovery: Discovery {
                kind: "proxy".into(),
            },
            api_key: environment_api_key.to_string(),
            updated_at: crate::storage::now(),
        };
        let path = PathBuf::from(".harness/providers.json");
        let state = load_store(&path)?;
        if state.selected != ENVIRONMENT_PROVIDER {
            let valid = state
                .providers
                .get(&state.selected)
                .is_some_and(|provider| !provider.deleted);
            if !valid {
                bail!("selected provider is unavailable");
            }
        }
        let environment_agents =
            MemoryAgents::new(&environment.base_url, &environment.api_key, default_model)?
                .with_spend_store(spend_store.clone());
        let mut cache = HashMap::new();
        cache.insert(
            (ENVIRONMENT_PROVIDER.to_string(), environment.version),
            environment_agents,
        );
        Ok(Self {
            inner: Arc::new(RegistryInner {
                path,
                state: RwLock::new(state),
                environment,
                default_model: default_model.to_string(),
                spend_store,
                allow_loopback: std::env::var("HARNESS_PROVIDER_ALLOW_LOOPBACK_HTTP")
                    .is_ok_and(|value| value == "1"),
                cache: Mutex::new(cache),
                mutation: tokio::sync::Mutex::new(()),
            }),
        })
    }

    #[cfg(test)]
    pub(crate) fn open_for_test(
        path: PathBuf,
        environment_base_url: &str,
        environment_api_key: &str,
        default_model: &str,
        spend_store: DbStore,
        allow_loopback: bool,
    ) -> Result<Self> {
        let environment = ProviderVersion {
            version: 1,
            base_url: environment_base_url.trim_end_matches('/').to_string(),
            api: "openai-completions".into(),
            discovery: Discovery {
                kind: "proxy".into(),
            },
            api_key: environment_api_key.to_string(),
            updated_at: crate::storage::now(),
        };
        let state = load_store(&path)?;
        let environment_agents =
            MemoryAgents::new(&environment.base_url, &environment.api_key, default_model)?
                .with_spend_store(spend_store.clone());
        let mut cache = HashMap::new();
        cache.insert(
            (ENVIRONMENT_PROVIDER.to_string(), environment.version),
            environment_agents,
        );
        Ok(Self {
            inner: Arc::new(RegistryInner {
                path,
                state: RwLock::new(state),
                environment,
                default_model: default_model.to_string(),
                spend_store,
                allow_loopback,
                cache: Mutex::new(cache),
                mutation: tokio::sync::Mutex::new(()),
            }),
        })
    }

    pub(crate) fn selected_ref(&self) -> Result<ProviderRef> {
        let state = self
            .inner
            .state
            .read()
            .map_err(|_| anyhow::anyhow!("provider registry unavailable"))?;
        let id = state.selected.clone();
        let version = if id == ENVIRONMENT_PROVIDER {
            self.inner.environment.version
        } else {
            let record = state
                .providers
                .get(&id)
                .filter(|record| !record.deleted)
                .ok_or_else(|| anyhow::anyhow!("selected provider unavailable"))?;
            record.current_version
        };
        Ok(ProviderRef {
            id,
            version,
            default_model: self.inner.default_model.clone(),
        })
    }

    pub(crate) fn public_state(&self) -> Result<Value> {
        let state = self
            .inner
            .state
            .read()
            .map_err(|_| anyhow::anyhow!("provider registry unavailable"))?;
        let mut providers = vec![ProviderPublic {
            id: ENVIRONMENT_PROVIDER.into(),
            base_url: public_url(&self.inner.environment.base_url),
            api: self.inner.environment.api.clone(),
            discovery: self.inner.environment.discovery.clone(),
            version: self.inner.environment.version,
            selected: state.selected == ENVIRONMENT_PROVIDER,
            key_present: !self.inner.environment.api_key.is_empty(),
            source: "environment",
        }];
        for (id, record) in &state.providers {
            if record.deleted {
                continue;
            }
            let version = current_version(record)?;
            providers.push(ProviderPublic {
                id: id.clone(),
                base_url: version.base_url.clone(),
                api: version.api.clone(),
                discovery: version.discovery.clone(),
                version: version.version,
                selected: state.selected == *id,
                key_present: !version.api_key.is_empty(),
                source: "saved",
            });
        }
        Ok(json!({"selected":state.selected,"providers":providers}))
    }

    pub(crate) async fn upsert(&self, input: ProviderInput) -> Result<ProviderPublic> {
        let _mutation = self.inner.mutation.lock().await;
        validate_provider_id(&input.id)?;
        if input.id == ENVIRONMENT_PROVIDER {
            bail!("environment provider cannot be overwritten");
        }
        validate_api_contract(&input.api, &input.discovery)?;
        let base_url = normalize_provider_url(&input.base_url, self.inner.allow_loopback)?;
        // Resolve before anything is persisted. The same check is repeated when the provider is
        // used, and that address is pinned into reqwest so a second DNS answer cannot redirect it.
        resolve_provider_target(&base_url, self.inner.allow_loopback).await?;

        let next = {
            let state = self
                .inner
                .state
                .read()
                .map_err(|_| anyhow::anyhow!("provider registry unavailable"))?;
            let prior = state.providers.get(&input.id).and_then(|record| {
                record
                    .versions
                    .iter()
                    .find(|version| version.version == record.current_version)
            });
            let api_key = match input.api_key {
                Some(key) => validate_api_key(&key)?.to_string(),
                None => prior
                    .map(|version| version.api_key.clone())
                    .ok_or_else(|| anyhow::anyhow!("apiKey is required for a new provider"))?,
            };
            let version = state
                .providers
                .get(&input.id)
                .map_or(1, |record| record.current_version.saturating_add(1));
            ProviderVersion {
                version,
                base_url,
                api: input.api,
                discovery: input.discovery,
                api_key,
                updated_at: crate::storage::now(),
            }
        };

        // Build once before publishing the new version. This proves the pinned client can be
        // constructed and primes the cache without sending an HTTP request.
        let agents = self.agents_from_version(&next).await?;

        let mut updated = {
            self.inner
                .state
                .read()
                .map_err(|_| anyhow::anyhow!("provider registry unavailable"))?
                .clone()
        };
        if !updated.providers.contains_key(&input.id)
            && updated
                .providers
                .values()
                .filter(|record| !record.deleted)
                .count()
                >= MAX_PROVIDERS
        {
            bail!("provider limit reached");
        }
        let record = updated
            .providers
            .entry(input.id.clone())
            .or_insert_with(|| ProviderRecord {
                current_version: 0,
                deleted: false,
                versions: Vec::new(),
            });
        if record.versions.len() >= MAX_VERSIONS_PER_PROVIDER {
            bail!("provider version limit reached");
        }
        record.current_version = next.version;
        record.deleted = false;
        record.versions.push(next.clone());
        persist_store(&self.inner.path, &updated)?;
        {
            let mut state = self
                .inner
                .state
                .write()
                .map_err(|_| anyhow::anyhow!("provider registry unavailable"))?;
            *state = updated;
        }
        self.inner
            .cache
            .lock()
            .map_err(|_| anyhow::anyhow!("provider cache unavailable"))?
            .insert((input.id.clone(), next.version), agents);
        Ok(ProviderPublic {
            id: input.id.clone(),
            base_url: next.base_url,
            api: next.api,
            discovery: next.discovery,
            version: next.version,
            selected: self.selected_ref()?.id == input.id,
            key_present: true,
            source: "saved",
        })
    }

    pub(crate) async fn select(&self, id: &str) -> Result<ProviderRef> {
        let _mutation = self.inner.mutation.lock().await;
        validate_provider_id_or_environment(id)?;
        let mut updated = self
            .inner
            .state
            .read()
            .map_err(|_| anyhow::anyhow!("provider registry unavailable"))?
            .clone();
        if id != ENVIRONMENT_PROVIDER
            && !updated
                .providers
                .get(id)
                .is_some_and(|record| !record.deleted)
        {
            bail!("provider not found");
        }
        if id != ENVIRONMENT_PROVIDER {
            let version = updated
                .providers
                .get(id)
                .ok_or_else(|| anyhow::anyhow!("provider not found"))?
                .current_version;
            self.agents_for(id, version).await?;
        }
        updated.selected = id.to_string();
        persist_store(&self.inner.path, &updated)?;
        {
            let mut state = self
                .inner
                .state
                .write()
                .map_err(|_| anyhow::anyhow!("provider registry unavailable"))?;
            *state = updated;
        }
        self.selected_ref()
    }

    pub(crate) async fn delete(&self, id: &str) -> Result<()> {
        let _mutation = self.inner.mutation.lock().await;
        validate_provider_id(id)?;
        if id == ENVIRONMENT_PROVIDER {
            bail!("environment provider cannot be deleted");
        }
        let mut updated = self
            .inner
            .state
            .read()
            .map_err(|_| anyhow::anyhow!("provider registry unavailable"))?
            .clone();
        if updated.selected == id {
            bail!("selected provider cannot be deleted");
        }
        let record = updated
            .providers
            .get_mut(id)
            .ok_or_else(|| anyhow::anyhow!("provider not found"))?;
        if record.deleted {
            bail!("provider not found");
        }
        // Historical versions remain in the private secret store so an admitted/queued turn can
        // still resolve the exact version it recorded. Public listing hides deleted providers.
        record.deleted = true;
        persist_store(&self.inner.path, &updated)?;
        let mut state = self
            .inner
            .state
            .write()
            .map_err(|_| anyhow::anyhow!("provider registry unavailable"))?;
        *state = updated;
        Ok(())
    }

    pub(crate) async fn selected_agents(&self) -> Result<MemoryAgents> {
        let selected = self.selected_ref()?;
        self.agents_for(&selected.id, selected.version).await
    }

    pub(crate) async fn agents_for(&self, id: &str, version: u64) -> Result<MemoryAgents> {
        if let Some(cached) = self
            .inner
            .cache
            .lock()
            .map_err(|_| anyhow::anyhow!("provider cache unavailable"))?
            .get(&(id.to_string(), version))
            .cloned()
        {
            return Ok(cached);
        }
        let provider = if id == ENVIRONMENT_PROVIDER {
            if version != self.inner.environment.version {
                bail!("environment provider version unavailable");
            }
            self.inner.environment.clone()
        } else {
            let state = self
                .inner
                .state
                .read()
                .map_err(|_| anyhow::anyhow!("provider registry unavailable"))?;
            let record = state
                .providers
                .get(id)
                .ok_or_else(|| anyhow::anyhow!("provider not found"))?;
            record
                .versions
                .iter()
                .find(|candidate| candidate.version == version)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("provider version not found"))?
        };
        let agents = if id == ENVIRONMENT_PROVIDER {
            MemoryAgents::new(
                &provider.base_url,
                &provider.api_key,
                &self.inner.default_model,
            )?
            .with_spend_store(self.inner.spend_store.clone())
        } else {
            self.agents_from_version(&provider).await?
        };
        self.inner
            .cache
            .lock()
            .map_err(|_| anyhow::anyhow!("provider cache unavailable"))?
            .insert((id.to_string(), version), agents.clone());
        Ok(agents)
    }

    pub(crate) async fn test_provider(&self, id: &str) -> Result<Value> {
        let reference = if id == ENVIRONMENT_PROVIDER {
            ProviderRef {
                id: ENVIRONMENT_PROVIDER.into(),
                version: self.inner.environment.version,
                default_model: self.inner.default_model.clone(),
            }
        } else {
            let state = self
                .inner
                .state
                .read()
                .map_err(|_| anyhow::anyhow!("provider registry unavailable"))?;
            let record = state
                .providers
                .get(id)
                .filter(|record| !record.deleted)
                .ok_or_else(|| anyhow::anyhow!("provider not found"))?;
            ProviderRef {
                id: id.to_string(),
                version: record.current_version,
                default_model: self.inner.default_model.clone(),
            }
        };
        let agents = self.agents_for(&reference.id, reference.version).await?;
        let models = agents.list_models().await?;
        Ok(json!({
            "status":"reachable",
            "providerId":reference.id,
            "version":reference.version,
            "modelCount":models.get("data").and_then(Value::as_array).map_or(0, Vec::len)
        }))
    }

    async fn agents_from_version(&self, provider: &ProviderVersion) -> Result<MemoryAgents> {
        let address =
            resolve_provider_target(&provider.base_url, self.inner.allow_loopback).await?;
        MemoryAgents::new_with_resolution(
            &provider.base_url,
            &provider.api_key,
            &self.inner.default_model,
            Some(address),
        )
        .map(|agents| agents.with_spend_store(self.inner.spend_store.clone()))
    }
}

fn current_version(record: &ProviderRecord) -> Result<&ProviderVersion> {
    record
        .versions
        .iter()
        .find(|version| version.version == record.current_version)
        .ok_or_else(|| anyhow::anyhow!("provider version unavailable"))
}

fn validate_provider_id(id: &str) -> Result<()> {
    if id.is_empty()
        || id.len() > 64
        || !id.is_ascii()
        || !id.bytes().enumerate().all(|(index, byte)| match byte {
            b'a'..=b'z' | b'0'..=b'9' => true,
            b'-' | b'_' => index > 0,
            _ => false,
        })
    {
        bail!("invalid provider id");
    }
    Ok(())
}

fn validate_provider_id_or_environment(id: &str) -> Result<()> {
    if id == ENVIRONMENT_PROVIDER {
        Ok(())
    } else {
        validate_provider_id(id)
    }
}

fn validate_api_contract(api: &str, discovery: &Discovery) -> Result<()> {
    if api != "openai-completions" {
        bail!("unsupported provider API");
    }
    if discovery.kind != "proxy" {
        bail!("unsupported discovery type");
    }
    Ok(())
}

fn validate_api_key(value: &str) -> Result<&str> {
    if value.is_empty()
        || value.len() > MAX_KEY_BYTES
        || !value.is_ascii()
        || value
            .chars()
            .any(|ch| ch.is_control() || ch.is_whitespace())
    {
        bail!("invalid apiKey");
    }
    Ok(value)
}

fn normalize_provider_url(value: &str, allow_loopback: bool) -> Result<String> {
    if value.is_empty() || value.len() > MAX_URL_BYTES || value.chars().any(char::is_control) {
        bail!("invalid baseUrl");
    }
    let mut url = reqwest::Url::parse(value).context("invalid baseUrl")?;
    if !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        bail!("baseUrl cannot include credentials, query, or fragment");
    }
    let host = url
        .host_str()
        .ok_or_else(|| anyhow::anyhow!("baseUrl requires a host"))?;
    let literal = host.parse::<IpAddr>().ok();
    let loopback_literal = literal.is_some_and(|ip| ip.is_loopback());
    if url.scheme() != "https" && !(allow_loopback && url.scheme() == "http" && loopback_literal) {
        // localhost is checked after DNS resolution, so permit the spelling only behind the same
        // explicit test/development opt-in.
        if !(allow_loopback && url.scheme() == "http" && host.eq_ignore_ascii_case("localhost")) {
            bail!("baseUrl must use HTTPS");
        }
    }
    if url.path().len() > 1024 {
        bail!("baseUrl path is too long");
    }
    while url.path().ends_with('/') && url.path() != "/" {
        let trimmed = url.path().trim_end_matches('/').to_string();
        url.set_path(&trimmed);
    }
    Ok(url.to_string().trim_end_matches('/').to_string())
}

async fn resolve_provider_target(base_url: &str, allow_loopback: bool) -> Result<SocketAddr> {
    let url = reqwest::Url::parse(base_url)?;
    let host = url
        .host_str()
        .ok_or_else(|| anyhow::anyhow!("provider URL has no host"))?;
    let port = url
        .port_or_known_default()
        .ok_or_else(|| anyhow::anyhow!("provider URL has no usable port"))?;
    let addresses: Vec<SocketAddr> = tokio::net::lookup_host((host, port))
        .await
        .context("provider host could not be resolved")?
        .collect();
    if addresses.is_empty() {
        bail!("provider host returned no addresses");
    }
    for address in &addresses {
        let ip = address.ip();
        if ip.is_loopback() {
            if !allow_loopback {
                bail!("provider host resolves to a forbidden address");
            }
        } else if forbidden_ip(ip) {
            bail!("provider host resolves to a forbidden address");
        }
    }
    // Pin one validated address into the HTTP client. reqwest still sends the original host for
    // TLS/SNI, but it cannot perform a second DNS lookup that lands on a different address.
    Ok(addresses[0])
}

fn forbidden_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => forbidden_v4(ip),
        IpAddr::V6(ip) => forbidden_v6(ip),
    }
}

fn forbidden_v4(ip: Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    ip.is_private()
        || ip.is_loopback()
        || ip.is_link_local()
        || ip.is_unspecified()
        || ip.is_multicast()
        || ip == Ipv4Addr::BROADCAST
        || a == 0
        || (a == 100 && (64..=127).contains(&b))
        || (a == 192 && b == 0 && c == 0)
        || (a == 198 && (b == 18 || b == 19))
        || (a == 192 && b == 0 && c == 2)
        || (a == 198 && b == 51 && c == 100)
        || (a == 203 && b == 0 && c == 113)
        || a >= 240
}

fn forbidden_v6(ip: Ipv6Addr) -> bool {
    let segments = ip.segments();
    let ipv4_compatible = segments[..6].iter().all(|segment| *segment == 0)
        && !(segments[6] == 0 && segments[7] <= 1);
    ip.is_loopback()
        || ip.is_unspecified()
        || ip.is_multicast()
        || (segments[0] & 0xfe00) == 0xfc00
        || (segments[0] & 0xffc0) == 0xfe80
        || (segments[0] == 0x2001 && segments[1] == 0x0db8)
        // Transition mechanisms can route an apparently global IPv6 destination to an
        // embedded IPv4 target after this process has made its policy decision. Refuse the
        // transition prefixes rather than trying to reproduce every gateway's translation.
        || (segments[0] == 0x0064 && segments[1] == 0xff9b)
        || segments[0] == 0x2002
        || (segments[0] == 0x2001 && segments[1] == 0)
        || ipv4_compatible
        || ip.to_ipv4_mapped().is_some_and(forbidden_v4)
}

fn public_url(raw: &str) -> String {
    let Ok(mut url) = reqwest::Url::parse(raw) else {
        return "configured".into();
    };
    let _ = url.set_username("");
    let _ = url.set_password(None);
    url.set_query(None);
    url.set_fragment(None);
    url.to_string().trim_end_matches('/').to_string()
}

fn load_store(path: &Path) -> Result<ProviderFile> {
    if !path.exists() {
        return Ok(ProviderFile::default());
    }
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        bail!("provider secret store must be a regular file");
    }
    if metadata.len() > MAX_STORE_BYTES {
        bail!("provider secret store exceeds size limit");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 {
            bail!("provider secret store permissions must be 0600");
        }
    }
    let file = OpenOptions::new().read(true).open(path)?;
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(MAX_STORE_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_STORE_BYTES {
        bail!("provider secret store exceeds size limit");
    }
    let store: ProviderFile =
        serde_json::from_slice(&bytes).context("invalid provider secret store")?;
    if store.format_version != FORMAT_VERSION {
        bail!("unsupported provider secret store version");
    }
    if store.providers.len() > MAX_PROVIDERS {
        bail!("provider secret store has too many providers");
    }
    for (id, record) in &store.providers {
        validate_provider_id(id)?;
        if record.versions.is_empty() || record.versions.len() > MAX_VERSIONS_PER_PROVIDER {
            bail!("invalid provider version history");
        }
        if !record
            .versions
            .iter()
            .any(|version| version.version == record.current_version)
        {
            bail!("provider current version is missing");
        }
        for version in &record.versions {
            validate_api_contract(&version.api, &version.discovery)?;
            validate_api_key(&version.api_key)?;
            // Do not resolve on startup. Parsing rejects secret-bearing or malformed URLs;
            // address policy is rechecked and pinned immediately before network use.
            normalize_provider_url(&version.base_url, true)?;
        }
    }
    Ok(store)
}

fn persist_store(path: &Path, state: &ProviderFile) -> Result<()> {
    let bytes = serde_json::to_vec(state)?;
    if bytes.len() as u64 > MAX_STORE_BYTES {
        bail!("provider secret store exceeds size limit");
    }
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("provider store has no parent"))?;
    if parent.exists() {
        let metadata = fs::symlink_metadata(parent)?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            bail!("provider secret directory must be a real directory");
        }
    } else {
        fs::create_dir_all(parent)?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
    }
    if path.exists() && fs::symlink_metadata(path)?.file_type().is_symlink() {
        bail!("provider secret store cannot be a symlink");
    }
    let temporary = parent.join(format!(".providers-{}.tmp", uuid::Uuid::new_v4()));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temporary)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    drop(file);
    fs::rename(&temporary, path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }
    OpenOptions::new().read(true).open(parent)?.sync_all()?;
    Ok(())
}

#[cfg(test)]
#[path = "providers_tests.rs"]
mod tests;
