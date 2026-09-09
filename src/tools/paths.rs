//! Path sandbox. Rules: docs/design/tools.md#path-rules.
use std::path::{Component, Path, PathBuf};

pub enum PathError { Empty, Escapes, Denied, Invalid }
impl PathError {
    pub fn code(&self) -> &'static str { match self { Self::Denied | Self::Escapes => "path_denied", _ => "invalid_arguments" } }
    pub fn detail(&self) -> &'static str {
        match self {
            Self::Empty => "path must not be empty",
            Self::Escapes => "path resolves outside the project root",
            Self::Denied => "path matches the secret deny-list or the harness data directory",
            Self::Invalid => "path contains invalid characters or `..` components",
        }
    }
}

const DENY_EXACT: &[&str] = &[".env", ".netrc", ".npmrc", ".pypirc"];
const DENY_PREFIX: &[&str] = &[".env.", "id_rsa", "id_ed25519"];
const DENY_SUFFIX: &[&str] = &[".pem", ".key", ".p12", ".pfx", ".kdbx"];

pub fn is_denied_name(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    DENY_EXACT.contains(&n.as_str()) || DENY_PREFIX.iter().any(|p| n.starts_with(p)) || DENY_SUFFIX.iter().any(|s| n.ends_with(s))
}

/// Directory holding the harness database. P1-T04 refuses it as a scope `root_path`
/// and `resolve` denies every path inside it.
pub fn harness_data_dir() -> Option<PathBuf> {
    let db = std::env::var("HARNESS_DB").unwrap_or_else(|_| "data/harness_v2.db".into());
    Path::new(&db).parent().and_then(|p| std::fs::canonicalize(p).ok())
}

/// Resolve `input` under `root` (root must already be canonical). Works for not-yet-existing
/// files by canonicalizing the deepest existing ancestor.
pub fn resolve(root: &Path, input: &str) -> Result<PathBuf, PathError> {
    if input.is_empty() { return Err(PathError::Empty); }
    if input.contains('\0') { return Err(PathError::Invalid); }
    let rel = Path::new(input);
    if rel.is_absolute() {
        return canonical_inside(root, rel.to_path_buf());
    }
    for c in rel.components() {
        match c { Component::Normal(_) | Component::CurDir => {}, _ => return Err(PathError::Invalid) }
    }
    canonical_inside(root, root.join(rel))
}

fn canonical_inside(root: &Path, full: PathBuf) -> Result<PathBuf, PathError> {
    // Canonicalize the deepest existing ancestor, then re-append the missing tail.
    let mut existing = full.clone();
    let mut tail: Vec<std::ffi::OsString> = Vec::new();
    while !existing.exists() {
        let Some(name) = existing.file_name() else { return Err(PathError::Escapes) };
        tail.push(name.to_os_string());
        existing = match existing.parent() { Some(p) => p.to_path_buf(), None => return Err(PathError::Escapes) };
    }
    let mut canon = std::fs::canonicalize(&existing).map_err(|_| PathError::Escapes)?;
    for t in tail.iter().rev() {
        if t == ".." { return Err(PathError::Invalid); }
        canon.push(t);
    }
    if !canon.starts_with(root) { return Err(PathError::Escapes); }
    if let Some(data) = harness_data_dir() { if canon.starts_with(&data) { return Err(PathError::Denied); } }
    if canon.components().any(|c| matches!(c, Component::Normal(n) if is_denied_name(&n.to_string_lossy()))) {
        return Err(PathError::Denied);
    }
    Ok(canon)
}

/// Display form for the model/UI: relative to root with `/` separators.
pub fn display(root: &Path, p: &Path) -> String {
    p.strip_prefix(root).unwrap_or(p).to_string_lossy().replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use super::*;
    fn root() -> PathBuf { let d = std::env::temp_dir().join(format!("harness-paths-{}", uuid::Uuid::new_v4())); std::fs::create_dir_all(d.join("src")).unwrap(); std::fs::write(d.join("src/a.rs"), "x").unwrap(); std::fs::canonicalize(d).unwrap() }
    #[test] fn accepts_inside() { let r = root(); assert!(resolve(&r, "src/a.rs").is_ok()); assert!(resolve(&r, "src/new.rs").is_ok()); assert!(resolve(&r, "new/dir/file.txt").is_ok()); }
    #[test] fn rejects_escape() { let r = root(); assert!(resolve(&r, "../x").is_err()); assert!(resolve(&r, "/etc/passwd").is_err()); assert!(resolve(&r, "src/../../x").is_err()); }
    #[test] fn rejects_secrets() { let r = root(); assert!(matches!(resolve(&r, ".env"), Err(PathError::Denied))); assert!(matches!(resolve(&r, "certs/server.pem"), Err(PathError::Denied))); assert!(resolve(&r, ".env.example").is_err()); }
    #[cfg(unix)] #[test] fn rejects_symlink_out() { let r = root(); std::os::unix::fs::symlink("/", r.join("link")).unwrap(); assert!(resolve(&r, "link/etc/hosts").is_err()); }
}
