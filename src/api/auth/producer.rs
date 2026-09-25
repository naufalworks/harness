//! Ingest-only authority and immutable privacy-checked evidence. No HTTP/storage here.
use super::valid_token;
use crate::{
    safety,
    storage::{valid_external_id, ExternalScope},
};
use serde::Deserialize;
use serde_json::Value;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Grant {
    producer_id: String,
    token: String,
    projects: Vec<String>,
}

#[derive(Default)]
pub(super) struct Producers(Vec<Grant>);

/// Construction is private. Consumers cannot substitute raw data or another scope.
/// This is NOT proof of full envelope conformance or durable acceptance.
pub(crate) struct AuthorizedEvidence {
    #[cfg(test)]
    scope: ExternalScope,
    value: Value,
}
impl AuthorizedEvidence {
    pub(crate) fn value(&self) -> &Value {
        &self.value
    }
    #[cfg(test)]
    pub(crate) fn scoped_key(&self, id: &str) -> Result<(String, String, String), &'static str> {
        self.scope.key(id)
    }
}

impl Producers {
    pub(super) fn parse(
        config: &str,
        owner: &str,
        previous: Option<&str>,
    ) -> Result<Self, &'static str> {
        const ERROR: &str = "invalid external producer configuration";
        if config.len() > 65536 {
            return Err(ERROR);
        }
        let grants: Vec<Grant> = serde_json::from_str(config).map_err(|_| ERROR)?;
        if grants.len() > 64 {
            return Err(ERROR);
        }
        for (i, grant) in grants.iter().enumerate() {
            if !valid_external_id(&grant.producer_id)
                || safety::sensitive(&grant.producer_id)
                || !(32..=256).contains(&grant.token.len())
                || !grant.token.bytes().all(|b| b.is_ascii_graphic())
                || valid_token(owner, &grant.token)
                || previous.is_some_and(|p| valid_token(p, &grant.token))
                || grant.projects.is_empty()
                || grant.projects.len() > 64
                || grant
                    .projects
                    .iter()
                    .any(|p| !valid_external_id(p) || safety::sensitive(p))
                || grant
                    .projects
                    .iter()
                    .enumerate()
                    .any(|(j, p)| grant.projects[..j].contains(p))
                || grants[..i].iter().any(|g| {
                    g.producer_id == grant.producer_id || valid_token(&g.token, &grant.token)
                })
            {
                return Err(ERROR);
            }
        }
        Ok(Self(grants))
    }

    pub(super) fn contains_token(&self, token: &str) -> bool {
        self.0.iter().any(|g| valid_token(&g.token, token))
    }

    pub(super) fn authorize(
        &self,
        token: &str,
        body: &[u8],
    ) -> Result<AuthorizedEvidence, &'static str> {
        let grant = self
            .0
            .iter()
            .find(|g| valid_token(&g.token, token))
            .ok_or("producer_unauthorized")?;
        let value = safety::external_privacy(body)?;
        let obj = value.as_object().ok_or("malformed_envelope")?;
        let producer = obj
            .get("producer_id")
            .and_then(Value::as_str)
            .ok_or("malformed_envelope")?;
        let project = obj
            .get("project_id")
            .and_then(Value::as_str)
            .ok_or("malformed_envelope")?;
        if producer != grant.producer_id {
            return Err("scope_not_permitted");
        }
        let scope = ExternalScope::bind(&grant.producer_id, project, &grant.projects)?;
        for key in ["event_id", "producer_instance_id", "logical_session_id"] {
            scope.key(
                obj.get(key)
                    .and_then(Value::as_str)
                    .ok_or("malformed_envelope")?,
            )?;
        }
        for key in ["invocation_id", "task_id"] {
            if let Some(id) = obj.get(key).filter(|v| !v.is_null()) {
                scope.key(id.as_str().ok_or("malformed_envelope")?)?;
            }
        }
        // Credentials known to this process must not enter otherwise innocuous text.
        if self
            .0
            .iter()
            .any(|g| safety::external_contains(&value, &g.token))
        {
            return Err("redaction_missing");
        }
        Ok(AuthorizedEvidence {
            #[cfg(test)]
            scope,
            value,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::auth::{AuthKind, AuthState};
    use serde_json::json;
    use std::time::Duration;
    fn config() -> String {
        json!([{"producer_id":"development-mcp","token":"p".repeat(32),"projects":["proj-harness","other"]}]).to_string()
    }
    fn auth() -> AuthState {
        AuthState::new(
            "o".repeat(32),
            Some("r".repeat(32)),
            Duration::from_secs(900),
            None,
        )
        .with_producers(&config())
        .unwrap()
    }
    fn event() -> Value {
        serde_json::from_str(include_str!(
            "../../../tests/external_history/accepted/tool_completed.json"
        ))
        .unwrap()
    }
    fn check(a: &AuthState, e: &Value) -> Result<AuthorizedEvidence, &'static str> {
        a.authorize_external(&"p".repeat(32), &serde_json::to_vec(e).unwrap())
    }
    #[test]
    fn producer_scope_and_privilege_separation() {
        let a = auth();
        let e = event();
        assert!(check(&a, &e).is_ok());
        assert_eq!(a.identify(&"p".repeat(32)), None);
        assert_eq!(a.identify(&"o".repeat(32)), Some(AuthKind::Master));
        let (session, _) = a.issue_session();
        assert_eq!(a.identify(&session), Some(AuthKind::Session));
        for token in [session, "o".repeat(32), "r".repeat(32), "bad".into()] {
            assert_eq!(
                a.authorize_external(&token, b"{} ").err(),
                Some("producer_unauthorized")
            );
        }
        for (key, value) in [
            ("producer_id", "other"),
            ("project_id", "unknown"),
            ("project_id", "../proj-harness"),
            ("logical_session_id", "../other"),
        ] {
            let mut e = e.clone();
            e[key] = json!(value);
            assert!(check(&a, &e).is_err());
        }
        let first = check(&a, &e).unwrap();
        let mut other = e.clone();
        other["project_id"] = json!("other");
        assert_ne!(
            first.scoped_key("same").unwrap(),
            check(&a, &other).unwrap().scoped_key("same").unwrap()
        );
        assert_eq!(first.value(), &e);
        assert!(first.scoped_key("../bad").is_err());
    }
    #[test]
    fn invalid_config_is_closed_and_diagnostics_are_static() {
        let good: Value = serde_json::from_str(&config()).unwrap();
        let mut cases = vec![
            json!(null),
            json!({}),
            json!([good[0].clone(), good[0].clone()]),
        ];
        for (key, value) in [
            ("token", json!("o".repeat(32))),
            ("token", json!("r".repeat(32))),
            ("token", json!("short")),
            ("projects", json!([])),
            ("projects", json!(["*"])),
            ("projects", json!(["a", "a"])),
            ("producer_id", json!("../bad")),
            ("unknown", json!(true)),
        ] {
            let mut c = good.clone();
            c[0][key] = value;
            cases.push(c);
        }
        for c in cases {
            assert_eq!(
                Producers::parse(&c.to_string(), &"o".repeat(32), Some(&"r".repeat(32))).err(),
                Some("invalid external producer configuration")
            );
        }
        assert!(Producers::parse("[", "owner", None).is_err());
        assert!(Producers::default()
            .authorize(&"p".repeat(32), b"{}")
            .is_err());
    }
    #[test]
    fn all_evidence_surfaces_reject_secrets_without_echo() {
        let a = auth();
        for field in [
            "arguments",
            "title",
            "path",
            "result",
            "error",
            "output",
            "artifact_id",
        ] {
            let mut e = event();
            e["payload"][field] = json!({"nested":["Bearer synthetic-secret"]});
            assert_eq!(check(&a, &e).err(), Some("redaction_missing"));
        }
        for token in ["p".repeat(32), "o".repeat(32), "r".repeat(32)] {
            let mut e = event();
            e["payload"]["title"] = json!(token);
            assert_eq!(check(&a, &e).err(), Some("redaction_missing"));
        }
        let mut e = event();
        e["payload"]["password"] = json!("otherwise-innocuous");
        assert_eq!(check(&a, &e).err(), Some("redaction_missing"));
        e["payload"] = json!({"output":safety::redact("Bearer synthetic-secret")});
        assert!(check(&a, &e).is_ok());
    }
    #[test]
    fn escaped_credentials_and_session_secrets_do_not_pass() {
        let a = auth();
        let (session, _) = a.issue_session();
        let mut e = event();
        e["payload"]["output"] = json!(session);
        assert_eq!(check(&a, &e).err(), Some("redaction_missing"));
        let token = format!("{}\"\\", "z".repeat(32));
        let config =
            json!([{"producer_id":"development-mcp","token":token,"projects":["proj-harness"]}]);
        let a = AuthState::new("o".repeat(32), None, Duration::from_secs(900), None)
            .with_producers(&config.to_string())
            .unwrap();
        e["payload"]["output"] = json!(token);
        assert_eq!(
            a.authorize_external(&token, &serde_json::to_vec(&e).unwrap())
                .err(),
            Some("redaction_missing")
        );
    }

    #[tokio::test]
    async fn producer_cannot_read_history_approve_memory_or_access_archives() {
        use axum::{
            body::Body,
            http::{Request, StatusCode},
        };
        use std::sync::Arc;
        use tower::ServiceExt;
        let store = crate::storage::DbStore::init(":memory:").unwrap();
        let providers = crate::providers::ProviderRegistry::open_for_test(
            std::env::temp_dir().join(format!(
                "harness-provider-producer-test-{}.json",
                uuid::Uuid::new_v4()
            )),
            "http://127.0.0.1:9",
            "synthetic",
            "test",
            store.clone(),
            true,
        )
        .unwrap();
        let state = crate::Harness {
            store,
            providers,
            auth: Arc::new(auth()),
            port: 8080,
            origins: Arc::new(vec![]),
            api_limit: Arc::new(tokio::sync::Semaphore::new(8)),
            identity: Arc::new(crate::RuntimeIdentity::current().unwrap()),
            workers: Arc::new(crate::WorkerHealth::ready()),
            hsts: false,
            archive: None,
        };
        let app = crate::router(state);
        for (method, path) in [
            ("GET", "/history/search"),
            ("GET", "/sessions/s/messages"),
            ("POST", "/memory/confirm"),
            ("GET", "/archives/a"),
            ("POST", "/sources/a/archive"),
            ("POST", "/auth/session"),
        ] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method(method)
                        .uri(path)
                        .header("Authorization", format!("Bearer {}", "p".repeat(32)))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{path}");
        }
    }
    #[test]
    fn privacy_corpus_never_reaches_test_record_or_index_sinks() {
        let cases: Value =
            serde_json::from_str(include_str!("../../../tests/external_history/privacy.json"))
                .unwrap();
        let a = auth();
        let mut records = Vec::new();
        let mut index = String::new();
        let mut diagnostics = String::new();
        for case in cases.as_array().unwrap() {
            let mut e = event();
            e["payload"] = case["body"].clone();
            let result = check(&a, &e);
            assert_eq!(result.as_ref().err().copied(), case["expected"].as_str());
            match result {
                Ok(e) => {
                    index.push_str(&e.value().to_string());
                    records.push(e.value().clone());
                }
                Err(code) => diagnostics.push_str(code),
            }
        }
        assert_eq!(records.len(), 2);
        for sink in [serde_json::to_string(&records).unwrap(), index, diagnostics] {
            assert!(!sink.contains("synthetic-canary"));
        }
    }

    #[test]
    fn duplicate_keys_and_poisoned_privacy_state_fail_closed() {
        let a = auth();
        for body in [
            br#"{"output":"Bearer synthetic-canary","output":"safe"}"#.as_slice(),
            br#"{"nested":{"title":"safe","ti\u0074le":"other"}}"#,
        ] {
            assert_eq!(
                a.authorize_external(&"p".repeat(32), body).err(),
                Some("malformed_envelope")
            );
        }
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = a.sessions.lock().unwrap();
            panic!("synthetic poisoned lock");
        }));
        assert_eq!(check(&a, &event()).err(), Some("privacy_check_failed"));
    }

    #[test]
    fn equal_ids_on_different_producers_do_not_share_authority() {
        let c = json!([
            {"producer_id":"development-mcp","token":"p".repeat(32),"projects":["proj-harness"]},
            {"producer_id":"second","token":"q".repeat(32),"projects":["proj-harness"]}
        ]);
        let a = AuthState::new("o".repeat(32), None, Duration::from_secs(900), None)
            .with_producers(&c.to_string())
            .unwrap();
        let first = check(&a, &event()).unwrap();
        let mut second = event();
        second["producer_id"] = json!("second");
        assert_eq!(check(&a, &second).err(), Some("scope_not_permitted"));
        let second = a
            .authorize_external(&"q".repeat(32), &serde_json::to_vec(&second).unwrap())
            .unwrap();
        assert_ne!(
            first.scoped_key("same").unwrap(),
            second.scoped_key("same").unwrap()
        );
    }
}
