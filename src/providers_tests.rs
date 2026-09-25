use super::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn temp_root() -> PathBuf {
    let path = std::env::temp_dir().join(format!("harness-provider-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&path).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    path
}

fn registry(root: &Path, allow_loopback: bool) -> ProviderRegistry {
    let db = root.join("harness.sqlite");
    let store = DbStore::init(db.to_str().unwrap()).unwrap();
    ProviderRegistry::open_for_test(
        root.join("providers.json"),
        "http://127.0.0.1:9",
        "environment-secret",
        "fallback-model",
        store,
        allow_loopback,
    )
    .unwrap()
}

fn input(id: &str, base_url: String, key: Option<&str>) -> ProviderInput {
    ProviderInput {
        id: id.into(),
        base_url,
        api_key: key.map(str::to_string),
        api: "openai-completions".into(),
        discovery: Discovery {
            kind: "proxy".into(),
        },
    }
}

async fn one_response(
    status: &str,
    headers: &[(&str, &str)],
    body: &str,
) -> (SocketAddr, tokio::task::JoinHandle<String>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let status = status.to_string();
    let headers = headers
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect::<Vec<_>>();
    let body = body.to_string();
    let task = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut buffer = vec![0u8; 8192];
        let count = stream.read(&mut buffer).await.unwrap();
        let request = String::from_utf8_lossy(&buffer[..count]).to_string();
        let mut response = format!("HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n", body.len());
        for (name, value) in headers {
            response.push_str(&format!("{name}: {value}\r\n"));
        }
        response.push_str("\r\n");
        response.push_str(&body);
        stream.write_all(response.as_bytes()).await.unwrap();
        request
    });
    (address, task)
}

#[tokio::test]
async fn saved_key_is_private_and_never_in_public_projection() {
    let root = temp_root();
    let registry = registry(&root, true);
    let (address, _server) = one_response("200 OK", &[], r#"{"data":[]}"#).await;
    let secret = "sk-super-secret-provider-value";

    let first = registry
        .upsert(input(
            "local",
            format!("http://127.0.0.1:{}/v1", address.port()),
            Some(secret),
        ))
        .await
        .unwrap();
    assert_eq!(first.version, 1);
    assert!(first.key_present);
    let public = registry.public_state().unwrap().to_string();
    assert!(!public.contains(secret));
    assert!(!public.contains("sk-super"));

    let store_path = root.join("providers.json");
    let raw = std::fs::read_to_string(&store_path).unwrap();
    assert!(
        raw.contains(secret),
        "secret belongs only in the private provider store"
    );
    for suffix in ["harness.sqlite", "harness.sqlite-wal", "harness.sqlite-shm"] {
        let path = root.join(suffix);
        if path.exists() {
            assert!(
                !std::fs::read(&path)
                    .unwrap()
                    .windows(secret.len())
                    .any(|window| window == secret.as_bytes()),
                "provider secret must not enter {suffix}"
            );
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&store_path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            std::fs::metadata(&root).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }

    let second = registry
        .upsert(input(
            "local",
            format!("http://127.0.0.1:{}/v1", address.port()),
            None,
        ))
        .await
        .unwrap();
    assert_eq!(
        second.version, 2,
        "every saved edit gets a stable new version"
    );
    assert_eq!(registry.select("local").await.unwrap().version, 2);
    assert!(
        registry.delete("local").await.is_err(),
        "selected provider cannot be deleted"
    );
    registry.select(ENVIRONMENT_PROVIDER).await.unwrap();
    registry.delete("local").await.unwrap();
    assert!(!registry
        .public_state()
        .unwrap()
        .to_string()
        .contains("\"id\":\"local\""));
    // Historical versions stay usable for already-admitted work after deletion.
    registry.agents_for("local", 1).await.unwrap();

    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn connection_test_uses_bearer_key_and_does_not_follow_redirects() {
    let root = temp_root();
    let registry = registry(&root, true);
    let secret = "sk-connection-test-secret";
    let (address, request) = one_response("200 OK", &[], r#"{"data":[{"id":"m1"}]}"#).await;
    registry
        .upsert(input(
            "mock",
            format!("http://127.0.0.1:{}/v1", address.port()),
            Some(secret),
        ))
        .await
        .unwrap();
    let result = registry.test_provider("mock").await.unwrap();
    assert_eq!(result["status"], "reachable");
    assert_eq!(result["modelCount"], 1);
    let request = request.await.unwrap();
    assert!(request.starts_with("GET /v1/models HTTP/1.1"));
    assert!(request
        .to_ascii_lowercase()
        .contains(&format!("authorization: bearer {secret}").to_ascii_lowercase()));

    let (redirect_address, _redirect_request) = one_response(
        "302 Found",
        &[("Location", "http://169.254.169.254/latest/meta-data/")],
        "{}",
    )
    .await;
    registry
        .upsert(input(
            "redirect",
            format!("http://127.0.0.1:{}/v1", redirect_address.port()),
            Some("sk-redirect-test"),
        ))
        .await
        .unwrap();
    assert!(registry.test_provider("redirect").await.is_err());

    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn provider_url_policy_rejects_secret_urls_and_internal_targets() {
    let root = temp_root();
    let registry = registry(&root, false);
    for (id, url) in [
        ("plain", "http://example.com/v1"),
        ("loop", "https://127.0.0.1/v1"),
        ("metadata", "https://169.254.169.254/v1"),
        ("private", "https://10.0.0.1/v1"),
        ("userinfo", "https://user:pass@example.com/v1"),
        ("query", "https://example.com/v1?token=secret"),
        ("fragment", "https://example.com/v1#secret"),
    ] {
        let result = registry
            .upsert(input(id, url.to_string(), Some("sk-policy-test")))
            .await;
        assert!(result.is_err(), "{url} must be refused");
    }
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn ipv6_transition_addresses_cannot_bypass_private_target_policy() {
    for address in [
        "::ffff:10.0.0.1",
        "::10.0.0.1",
        "64:ff9b::0a00:0001",
        "2002:0a00:0001::",
        "2001:0000:4136:e378:8000:63bf:f5ff:fffe",
    ] {
        let parsed: Ipv6Addr = address.parse().unwrap();
        assert!(forbidden_v6(parsed), "{address} must be refused");
    }
    assert!(
        !forbidden_v6("2606:4700:4700::1111".parse().unwrap()),
        "ordinary global IPv6 must remain usable"
    );
}

#[test]
fn malformed_secret_store_fails_closed() {
    let root = temp_root();
    let path = root.join("providers.json");
    std::fs::write(
        &path,
        br#"{"format_version":999,"selected":"environment","providers":{}}"#,
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let db = root.join("db.sqlite");
    let store = DbStore::init(db.to_str().unwrap()).unwrap();
    assert!(ProviderRegistry::open_for_test(
        path,
        "http://127.0.0.1:9",
        "environment-secret",
        "model",
        store,
        true,
    )
    .is_err());
    let _ = std::fs::remove_dir_all(root);
}

#[cfg(unix)]
#[tokio::test]
async fn secret_store_refuses_symlinked_parent_directory() {
    use std::os::unix::fs::symlink;

    let root = temp_root();
    let real = root.join("real");
    std::fs::create_dir_all(&real).unwrap();
    let linked = root.join("linked");
    symlink(&real, &linked).unwrap();
    let db = root.join("db.sqlite");
    let store = DbStore::init(db.to_str().unwrap()).unwrap();
    let registry = ProviderRegistry::open_for_test(
        linked.join("providers.json"),
        "http://127.0.0.1:9",
        "environment-secret",
        "model",
        store,
        true,
    )
    .unwrap();
    let (address, _server) = one_response("200 OK", &[], r#"{\"data\":[]}"#).await;
    let result = registry
        .upsert(input(
            "linked",
            format!("http://127.0.0.1:{}/v1", address.port()),
            Some("sk-parent-symlink-test"),
        ))
        .await;
    assert!(result.is_err());
    assert!(!real.join("providers.json").exists());
    let _ = std::fs::remove_dir_all(root);
}

#[cfg(unix)]
#[tokio::test]
async fn existing_shared_parent_is_refused_without_changing_permissions() {
    use std::os::unix::fs::PermissionsExt;

    let parent = std::env::temp_dir().join(format!(
        "harness-provider-shared-parent-test-{}",
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&parent).unwrap();
    std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o755)).unwrap();
    let db = parent.join("db.sqlite");
    let store = DbStore::init(db.to_str().unwrap()).unwrap();
    let registry = ProviderRegistry::open_for_test(
        parent.join("providers.json"),
        "http://127.0.0.1:9",
        "environment-secret",
        "model",
        store,
        true,
    )
    .unwrap();
    let (address, _server) = one_response("200 OK", &[], r#"{\"data\":[]}"#).await;
    let result = registry
        .upsert(input(
            "shared",
            format!("http://127.0.0.1:{}/v1", address.port()),
            Some("sk-shared-parent-test"),
        ))
        .await;
    assert!(result.is_err());
    assert_eq!(
        std::fs::metadata(&parent).unwrap().permissions().mode() & 0o777,
        0o755,
        "the provider store must never chmod an existing shared directory"
    );
    assert!(!parent.join("providers.json").exists());
    let _ = std::fs::remove_dir_all(parent);
}
