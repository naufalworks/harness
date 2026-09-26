//! Path sandbox. Rules: docs/design/tools.md#path-rules.
use serde::Serialize;
use std::path::{Component, Path, PathBuf};

#[derive(Debug)]
pub enum PathError {
    Empty,
    Escapes,
    Denied,
    Invalid,
}
impl PathError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Denied | Self::Escapes => "path_denied",
            _ => "invalid_arguments",
        }
    }
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
const DENY_BROWSE_DIRS: &[&str] = &[
    ".git",
    ".harness",
    ".ssh",
    ".gnupg",
    "archive",
    "archives",
    "backup",
    "backups",
    "credentials",
    "keys",
    "secrets",
];
const MAX_BROWSE_ROOTS: usize = 32;

pub fn is_denied_name(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    DENY_EXACT.contains(&n.as_str())
        || DENY_PREFIX.iter().any(|p| n.starts_with(p))
        || DENY_SUFFIX.iter().any(|s| n.ends_with(s))
}

pub fn is_denied_browse_dir_name(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    name.starts_with('.') || DENY_BROWSE_DIRS.contains(&n.as_str()) || is_denied_name(name)
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct BrowseDirectory {
    pub name: String,
    pub path: String,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct BrowseCrumb {
    pub name: String,
    pub path: String,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct BrowsePage {
    pub path: String,
    pub root: String,
    pub parent: Option<String>,
    pub breadcrumbs: Vec<BrowseCrumb>,
    pub directories: Vec<BrowseDirectory>,
    pub next_cursor: Option<usize>,
    pub limit: usize,
}

/// Directory holding the harness database. P1-T04 refuses it as a scope `root_path`
/// and `resolve` denies every path inside it.
pub fn harness_data_dir() -> Option<PathBuf> {
    let db = std::env::var("HARNESS_DB").unwrap_or_else(|_| "data/harness_v2.db".into());
    Path::new(&db)
        .parent()
        .and_then(|p| std::fs::canonicalize(p).ok())
}

/// Parse the operator-configured project browse roots. Empty input deliberately means deny-all.
pub fn browse_roots(raw: &str) -> Result<Vec<PathBuf>, PathError> {
    let mut roots = Vec::new();
    for item in raw
        .split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
    {
        if item.len() > 4096 || item.chars().any(char::is_control) {
            return Err(PathError::Invalid);
        }
        let requested = Path::new(item);
        if !requested.is_absolute()
            || requested
                .components()
                .any(|part| matches!(part, Component::ParentDir))
        {
            return Err(PathError::Invalid);
        }
        let canonical = std::fs::canonicalize(requested).map_err(|_| PathError::Invalid)?;
        if !canonical.is_dir() || canonical.parent().is_none() {
            return Err(PathError::Denied);
        }
        if let Some(data) = harness_data_dir() {
            if canonical.starts_with(&data) {
                return Err(PathError::Denied);
            }
        }
        if canonical.components().any(|part| {
            matches!(part, Component::Normal(name) if is_denied_browse_dir_name(&name.to_string_lossy()))
        }) {
            return Err(PathError::Denied);
        }
        if !roots.contains(&canonical) {
            if roots.len() >= MAX_BROWSE_ROOTS {
                return Err(PathError::Invalid);
            }
            roots.push(canonical);
        }
    }
    roots.sort();
    Ok(roots)
}

fn path_text(path: &Path) -> Result<String, PathError> {
    path.to_str().map(str::to_string).ok_or(PathError::Invalid)
}

fn browse_root_for<'a>(roots: &'a [PathBuf], path: &Path) -> Option<&'a PathBuf> {
    roots
        .iter()
        .filter(|root| path.starts_with(root))
        .max_by_key(|root| root.components().count())
}

fn browse_breadcrumbs(root: &Path, current: &Path) -> Result<Vec<BrowseCrumb>, PathError> {
    let mut crumbs = Vec::new();
    let mut cursor = root.to_path_buf();
    crumbs.push(BrowseCrumb {
        name: root
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "workspace".into()),
        path: path_text(root)?,
    });
    let relative = current.strip_prefix(root).map_err(|_| PathError::Escapes)?;
    for component in relative.components() {
        let Component::Normal(name) = component else {
            return Err(PathError::Invalid);
        };
        cursor.push(name);
        crumbs.push(BrowseCrumb {
            name: name.to_string_lossy().into_owned(),
            path: path_text(&cursor)?,
        });
    }
    Ok(crumbs)
}

/// Read one bounded page of real directories below an explicitly configured browse root.
/// Symlink directories and hidden/sensitive directory names are never returned.
pub fn browse_directories(
    roots: &[PathBuf],
    requested: &str,
    cursor: usize,
    limit: usize,
) -> Result<BrowsePage, PathError> {
    if roots.is_empty() || requested.is_empty() || requested.len() > 4096 {
        return Err(PathError::Denied);
    }
    let raw = Path::new(requested);
    if !raw.is_absolute()
        || raw
            .components()
            .any(|part| !matches!(part, Component::RootDir | Component::Normal(_)))
    {
        return Err(PathError::Invalid);
    }
    let canonical = std::fs::canonicalize(raw).map_err(|_| PathError::Invalid)?;
    if canonical != raw || !canonical.is_dir() {
        return Err(PathError::Denied);
    }
    let root = browse_root_for(roots, &canonical).ok_or(PathError::Escapes)?;
    if let Some(data) = harness_data_dir() {
        if canonical.starts_with(data) {
            return Err(PathError::Denied);
        }
    }
    if canonical
        .strip_prefix(root)
        .map_err(|_| PathError::Escapes)?
        .components()
        .any(|part| {
            matches!(part, Component::Normal(name) if is_denied_browse_dir_name(&name.to_string_lossy()))
        })
    {
        return Err(PathError::Denied);
    }

    let limit = limit.clamp(1, 100);
    let mut entries = Vec::new();
    for entry in std::fs::read_dir(&canonical).map_err(|_| PathError::Denied)? {
        let entry = entry.map_err(|_| PathError::Denied)?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if is_denied_browse_dir_name(&name) {
            continue;
        }
        let file_type = entry.file_type().map_err(|_| PathError::Denied)?;
        if file_type.is_symlink() || !file_type.is_dir() {
            continue;
        }
        let child = entry.path();
        let resolved = std::fs::canonicalize(&child).map_err(|_| PathError::Denied)?;
        if resolved != child || !resolved.starts_with(root) {
            continue;
        }
        entries.push(BrowseDirectory {
            name,
            path: path_text(&resolved)?,
        });
    }
    entries.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.path.cmp(&b.path)));
    if cursor > entries.len() {
        return Err(PathError::Invalid);
    }
    let end = cursor.saturating_add(limit).min(entries.len());
    let next_cursor = (end < entries.len()).then_some(end);
    let directories = entries[cursor..end].to_vec();
    let parent = if canonical == *root {
        None
    } else {
        canonical
            .parent()
            .filter(|parent| parent.starts_with(root))
            .map(path_text)
            .transpose()?
    };
    Ok(BrowsePage {
        path: path_text(&canonical)?,
        root: path_text(root)?,
        parent,
        breadcrumbs: browse_breadcrumbs(root, &canonical)?,
        directories,
        next_cursor,
        limit,
    })
}

/// Resolve `input` under `root` (root must already be canonical). Works for not-yet-existing
/// files by canonicalizing the deepest existing ancestor.
pub fn resolve(root: &Path, input: &str) -> Result<PathBuf, PathError> {
    if input.is_empty() {
        return Err(PathError::Empty);
    }
    if input.contains('\0') {
        return Err(PathError::Invalid);
    }
    let rel = Path::new(input);
    if rel.is_absolute() {
        return canonical_inside(root, rel.to_path_buf());
    }
    for c in rel.components() {
        match c {
            Component::Normal(_) | Component::CurDir => {}
            _ => return Err(PathError::Invalid),
        }
    }
    canonical_inside(root, root.join(rel))
}

fn canonical_inside(root: &Path, full: PathBuf) -> Result<PathBuf, PathError> {
    // Canonicalize the deepest existing ancestor, then re-append the missing tail.
    let mut existing = full.clone();
    let mut tail: Vec<std::ffi::OsString> = Vec::new();
    while !existing.exists() {
        let Some(name) = existing.file_name() else {
            return Err(PathError::Escapes);
        };
        tail.push(name.to_os_string());
        existing = match existing.parent() {
            Some(p) => p.to_path_buf(),
            None => return Err(PathError::Escapes),
        };
    }
    let mut canon = std::fs::canonicalize(&existing).map_err(|_| PathError::Escapes)?;
    for t in tail.iter().rev() {
        if t == ".." {
            return Err(PathError::Invalid);
        }
        canon.push(t);
    }
    if !canon.starts_with(root) {
        return Err(PathError::Escapes);
    }
    if let Some(data) = harness_data_dir() {
        if canon.starts_with(&data) {
            return Err(PathError::Denied);
        }
    }
    if canon
        .components()
        .any(|c| matches!(c, Component::Normal(n) if is_denied_name(&n.to_string_lossy())))
    {
        return Err(PathError::Denied);
    }
    Ok(canon)
}

/// Re-check the destination immediately before an atomic write. The parent must still resolve
/// inside the canonical project root, must not be a symlink, and on Unix must have the same owner
/// as the project root. This catches approval-time TOCTOU swaps before the rename is attempted.
pub fn verify_write_target(root: &Path, path: &Path) -> Result<(), PathError> {
    if !path.starts_with(root) {
        return Err(PathError::Escapes);
    }
    let parent = path.parent().ok_or(PathError::Invalid)?;
    let canonical_parent = std::fs::canonicalize(parent).map_err(|_| PathError::Escapes)?;
    if canonical_parent != parent || !canonical_parent.starts_with(root) {
        return Err(PathError::Escapes);
    }
    if std::fs::symlink_metadata(parent)
        .map_err(|_| PathError::Escapes)?
        .file_type()
        .is_symlink()
    {
        return Err(PathError::Escapes);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let root_owner = std::fs::metadata(root)
            .map_err(|_| PathError::Escapes)?
            .uid();
        let parent_owner = std::fs::metadata(parent)
            .map_err(|_| PathError::Escapes)?
            .uid();
        if root_owner != parent_owner {
            return Err(PathError::Denied);
        }
    }
    canonical_inside(root, path.to_path_buf()).map(|_| ())
}

/// Display form for the model/UI: relative to root with `/` separators.
pub fn display(root: &Path, p: &Path) -> String {
    p.strip_prefix(root)
        .unwrap_or(p)
        .to_string_lossy()
        .replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use super::*;
    fn root() -> PathBuf {
        let d = std::env::temp_dir().join(format!("harness-paths-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(d.join("src")).unwrap();
        std::fs::write(d.join("src/a.rs"), "x").unwrap();
        std::fs::canonicalize(d).unwrap()
    }
    #[test]
    fn accepts_inside() {
        let r = root();
        assert!(resolve(&r, "src/a.rs").is_ok());
        assert!(resolve(&r, "src/new.rs").is_ok());
        assert!(resolve(&r, "new/dir/file.txt").is_ok());
    }
    #[test]
    fn rejects_escape() {
        let r = root();
        assert!(resolve(&r, "../x").is_err());
        assert!(resolve(&r, "/etc/passwd").is_err());
        assert!(resolve(&r, "src/../../x").is_err());
    }
    #[test]
    fn rejects_secrets() {
        let r = root();
        assert!(matches!(resolve(&r, ".env"), Err(PathError::Denied)));
        assert!(matches!(
            resolve(&r, "certs/server.pem"),
            Err(PathError::Denied)
        ));
        assert!(resolve(&r, ".env.example").is_err());
    }
    #[test]
    fn directory_browser_defaults_to_deny_and_lists_only_safe_directories() {
        assert!(browse_roots("").unwrap().is_empty());
        let r = root();
        std::fs::create_dir_all(r.join("alpha/child")).unwrap();
        std::fs::create_dir_all(r.join("beta")).unwrap();
        std::fs::create_dir_all(r.join(".hidden")).unwrap();
        std::fs::create_dir_all(r.join("backups")).unwrap();
        std::fs::write(r.join("file.txt"), "not a directory").unwrap();
        let roots = browse_roots(r.to_str().unwrap()).unwrap();
        let first = browse_directories(&roots, r.to_str().unwrap(), 0, 1).unwrap();
        assert_eq!(first.root, r.to_str().unwrap());
        assert_eq!(first.parent, None);
        assert_eq!(first.directories.len(), 1);
        assert!(first.next_cursor.is_some());
        let second =
            browse_directories(&roots, r.to_str().unwrap(), first.next_cursor.unwrap(), 100)
                .unwrap();
        let names = first
            .directories
            .iter()
            .chain(second.directories.iter())
            .map(|entry| entry.name.as_str())
            .collect::<Vec<_>>();
        assert!(names.contains(&"alpha"));
        assert!(names.contains(&"beta"));
        assert!(names.contains(&"src"));
        assert!(!names.contains(&".hidden"));
        assert!(!names.contains(&"backups"));
        let nested = browse_directories(&roots, r.join("alpha").to_str().unwrap(), 0, 50).unwrap();
        assert_eq!(nested.parent.as_deref(), r.to_str());
        assert_eq!(nested.breadcrumbs.last().unwrap().name, "alpha");
    }
    #[test]
    fn directory_browser_refuses_escape_and_malformed_paths() {
        let r = root();
        let roots = browse_roots(r.to_str().unwrap()).unwrap();
        assert!(browse_directories(&roots, "/", 0, 50).is_err());
        assert!(browse_directories(&roots, "relative", 0, 50).is_err());
        assert!(browse_directories(&roots, &format!("{}/../", r.display()), 0, 50).is_err());
        assert!(browse_directories(&roots, r.to_str().unwrap(), usize::MAX, 50).is_err());
    }
    #[cfg(unix)]
    #[test]
    fn rejects_symlink_out() {
        let r = root();
        std::os::unix::fs::symlink("/", r.join("link")).unwrap();
        assert!(resolve(&r, "link/etc/hosts").is_err());
    }
    #[cfg(unix)]
    #[test]
    fn directory_browser_never_lists_or_enters_symlink_directories() {
        let r = root();
        let outside =
            std::env::temp_dir().join(format!("harness-browse-outside-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, r.join("link-out")).unwrap();
        std::os::unix::fs::symlink(r.join("src"), r.join("link-in")).unwrap();
        let roots = browse_roots(r.to_str().unwrap()).unwrap();
        let page = browse_directories(&roots, r.to_str().unwrap(), 0, 100).unwrap();
        assert!(!page
            .directories
            .iter()
            .any(|entry| entry.name.starts_with("link-")));
        assert!(browse_directories(&roots, r.join("link-in").to_str().unwrap(), 0, 50).is_err());
        assert!(browse_directories(&roots, r.join("link-out").to_str().unwrap(), 0, 50).is_err());
    }
    #[cfg(unix)]
    #[test]
    fn final_write_gate_rejects_a_swapped_parent_symlink() {
        let r = root();
        let destination = resolve(&r, "new/file.txt").unwrap();
        std::fs::create_dir_all(r.join("new")).unwrap();
        let outside =
            std::env::temp_dir().join(format!("harness-outside-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::remove_dir(r.join("new")).unwrap();
        std::os::unix::fs::symlink(&outside, r.join("new")).unwrap();
        assert!(verify_write_target(&r, &destination).is_err());
        assert!(!outside.join("file.txt").exists());
    }
}
