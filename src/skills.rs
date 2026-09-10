//! Skills with progressive disclosure: docs/design/context.md#skills-index, docs/design/tools.md#skill.
//!
//! Two levels, never more. Discovery reads `skills/*/SKILL.md` under the scope root and keeps only
//! each skill's name and description for the initial window; the body stays on disk until the
//! `skill` tool loads one, bounded to 16 KiB. Nothing here calls a provider or writes storage.
use anyhow::{Context, Result};
use std::{fs, path::Path};

/// Directory, relative to the scope root, holding one folder per skill.
pub const SKILLS_DIR: &str = "skills";
/// Producer-level cap on indexed skills. Anything past it is reported, never silently dropped.
pub const MAX_SKILLS: usize = 32;
/// A SKILL.md larger than this is not parsed at all.
pub const MAX_SKILL_FILE_BYTES: u64 = 256 * 1024;
/// Model-facing body cap for one `skill` call.
pub const MAX_BODY_BYTES: usize = 16 * 1024;
const MAX_NAME_CHARS: usize = 64;
const MAX_DESCRIPTION_CHARS: usize = 200;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Skill {
    /// The directory name: a skill's identity, and the only argument `skill` accepts.
    pub name: String,
    pub description: String,
    /// Root-relative path, so the model can `read` the file itself if it wants the raw source.
    pub path: String,
}

impl Skill {
    /// One index line. Name and description only — the point of progressive disclosure.
    pub fn entry(&self) -> String {
        format!("- {}: {} ({})", self.name, self.description, self.path)
    }
}

#[derive(Clone, Debug, Default)]
pub struct Index {
    pub skills: Vec<Skill>,
    /// Discovered skills past `MAX_SKILLS`.
    pub over_cap: usize,
    /// Directories that looked like skills but had an unusable name or SKILL.md.
    pub skipped: usize,
}

/// A skill name must be one safe path segment, so `skills/<name>/SKILL.md` can never traverse.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.chars().count() <= MAX_NAME_CHARS
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
}

/// Enumerate skills in directory-name order. A missing `skills/` directory is not an error; an
/// unreadable one is, because a turn that silently drops the index would look identical to a
/// project with no skills.
pub fn discover(root: &Path) -> Result<Index> {
    let dir = root.join(SKILLS_DIR);
    if !dir.is_dir() {
        return Ok(Index::default());
    }
    let mut entries = fs::read_dir(&dir)
        .with_context(|| format!("read {}", dir.display()))?
        .collect::<std::io::Result<Vec<_>>>()
        .with_context(|| format!("enumerate {}", dir.display()))?;
    entries.sort_by_key(|entry| entry.file_name());
    let mut index = Index::default();
    for entry in entries {
        let Ok(kind) = entry.file_type() else {
            index.skipped += 1;
            continue;
        };
        // A symlinked skill directory could point anywhere, so only real directories are indexed.
        // Plain files sitting in `skills/` (a README, say) are not skill attempts and stay quiet.
        if !kind.is_dir() {
            if kind.is_symlink() {
                index.skipped += 1;
            }
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if !valid_name(&name) {
            index.skipped += 1;
            continue;
        }
        let Some(text) = readable(&entry.path().join("SKILL.md")) else {
            index.skipped += 1;
            continue;
        };
        if index.skills.len() == MAX_SKILLS {
            index.over_cap += 1;
            continue;
        }
        index.skills.push(Skill {
            description: describe(&text),
            path: format!("{SKILLS_DIR}/{name}/SKILL.md"),
            name,
        });
    }
    Ok(index)
}

/// Named parts for the `skills_index` context category, in discovery order. One part per skill, so
/// an over-budget index excludes whole entries with `category_budget` instead of cutting text.
pub fn index_parts(root: &Path) -> Result<Vec<(String, String)>> {
    let index = discover(root)?;
    let mut parts: Vec<(String, String)> = index
        .skills
        .iter()
        .map(|skill| (format!("skill:{}", skill.name), skill.entry()))
        .collect();
    if index.over_cap > 0 || index.skipped > 0 {
        parts.push((
            "skills:not_indexed".into(),
            format!(
                "- note: {} skill(s) past the {MAX_SKILLS}-skill cap and {} directory(ies) with an unusable name or SKILL.md are not listed here.",
                index.over_cap, index.skipped
            ),
        ));
    }
    Ok(parts)
}

/// The body below the frontmatter, redacted and capped at `MAX_BODY_BYTES` on a UTF-8 boundary.
pub fn bounded_body(text: &str) -> (String, bool) {
    let (_, body) = split_frontmatter(text);
    let body = crate::safety::redact(body.trim_start());
    if body.len() <= MAX_BODY_BYTES {
        return (body, false);
    }
    let mut end = MAX_BODY_BYTES;
    while end > 0 && !body.is_char_boundary(end) {
        end -= 1;
    }
    (body[..end].to_string(), true)
}

/// A skill file must be a regular file (not a symlink out of the project), small enough to parse,
/// and valid UTF-8.
fn readable(file: &Path) -> Option<String> {
    let meta = fs::symlink_metadata(file).ok()?;
    if !meta.file_type().is_file() || meta.len() > MAX_SKILL_FILE_BYTES {
        return None;
    }
    fs::read_to_string(file).ok()
}

/// `description:` from the frontmatter, else the first non-empty body line. The result reaches the
/// provider window, so it is always redacted, single-line and bounded.
fn describe(text: &str) -> String {
    let (front, body) = split_frontmatter(text);
    let raw = front
        .and_then(|front| field(front, "description"))
        .or_else(|| {
            body.lines()
                .map(|line| line.trim_start_matches('#').trim())
                .find(|line| !line.is_empty())
                .map(str::to_string)
        })
        .unwrap_or_else(|| "(no description)".into());
    one_line(&raw)
}

fn one_line(text: &str) -> String {
    let collapsed = crate::safety::redact(text)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if collapsed.chars().count() <= MAX_DESCRIPTION_CHARS {
        return collapsed;
    }
    collapsed
        .chars()
        .take(MAX_DESCRIPTION_CHARS.saturating_sub(1))
        .collect::<String>()
        + "\u{2026}"
}

/// Leading `---` delimited block, tolerating CRLF. An unterminated block is plain body text.
fn split_frontmatter(text: &str) -> (Option<&str>, &str) {
    let mut lines = text.split_inclusive('\n');
    let Some(first) = lines.next() else {
        return (None, text);
    };
    if first.trim_end() != "---" {
        return (None, text);
    }
    let start = first.len();
    let mut offset = start;
    for line in lines {
        if line.trim_end() == "---" {
            return (Some(&text[start..offset]), &text[offset + line.len()..]);
        }
        offset += line.len();
    }
    (None, text)
}

/// One `key: value` line from the frontmatter, unquoted. No YAML parser: a skill header carries
/// scalars, and anything more elaborate is body text the `skill` tool returns verbatim.
fn field(front: &str, key: &str) -> Option<String> {
    front
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.trim()
                .eq_ignore_ascii_case(key)
                .then(|| unquote(value.trim()).to_string())
        })
        .filter(|value| !value.is_empty())
}

fn unquote(value: &str) -> &str {
    for quote in ['"', '\''] {
        if value.len() >= 2 && value.starts_with(quote) && value.ends_with(quote) {
            return &value[1..value.len() - 1];
        }
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn project(skills: &[(&str, &str)]) -> PathBuf {
        let root = std::env::temp_dir().join(format!("harness-skills-{}", crate::storage::uid()));
        fs::create_dir_all(root.join(SKILLS_DIR)).unwrap();
        for (name, text) in skills {
            let dir = root.join(SKILLS_DIR).join(name);
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join("SKILL.md"), text).unwrap();
        }
        fs::canonicalize(root).unwrap()
    }

    #[test]
    fn skills_index_carries_names_and_descriptions_but_never_a_body() {
        let root = project(&[
            (
                "review",
                "---\nname: review\ndescription: \"How this repo reviews a diff\"\n---\nStep one: read the anchors.\n",
            ),
            ("deploy", "# Deploying\n\nRun the release gate first.\n"),
        ]);
        let index = discover(&root).unwrap();
        assert_eq!(
            index
                .skills
                .iter()
                .map(|skill| skill.name.as_str())
                .collect::<Vec<_>>(),
            vec!["deploy", "review"],
            "directory-name order keeps the index deterministic"
        );
        assert_eq!(index.skills[1].description, "How this repo reviews a diff");
        assert_eq!(
            index.skills[0].description, "Deploying",
            "without frontmatter the first heading is the description"
        );
        assert_eq!(index.skills[1].path, "skills/review/SKILL.md");
        assert_eq!((index.over_cap, index.skipped), (0, 0));

        let parts = index_parts(&root).unwrap();
        assert_eq!(
            parts.iter().map(|(id, _)| id.as_str()).collect::<Vec<_>>(),
            vec!["skill:deploy", "skill:review"],
            "one named part per skill, so the receipt can exclude whole entries"
        );
        let text = parts
            .iter()
            .map(|(_, entry)| entry.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            text.contains("- review: How this repo reviews a diff (skills/review/SKILL.md)"),
            "{text}"
        );
        assert!(
            !text.contains("read the anchors"),
            "the body must stay out of the window: {text}"
        );
        assert!(!text.contains("release gate"), "{text}");
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn skills_discovery_reports_what_it_could_not_index() {
        let root = project(&[("good", "description: ok\n\nbody\n")]);
        let skills = root.join(SKILLS_DIR);
        // A name that is not one safe path segment, and a directory with no SKILL.md at all.
        fs::create_dir_all(skills.join("bad name")).unwrap();
        fs::write(skills.join("bad name/SKILL.md"), "description: nope\n").unwrap();
        fs::create_dir_all(skills.join("empty")).unwrap();
        // A stray file in skills/ is not a skill attempt and must not be reported as skipped.
        fs::write(skills.join("README.md"), "not a skill\n").unwrap();
        let index = discover(&root).unwrap();
        assert_eq!(
            index
                .skills
                .iter()
                .map(|skill| skill.name.as_str())
                .collect::<Vec<_>>(),
            vec!["good"]
        );
        assert_eq!((index.over_cap, index.skipped), (0, 2));
        let note = index_parts(&root).unwrap().pop().unwrap();
        assert_eq!(note.0, "skills:not_indexed");
        assert!(note.1.contains("2 directory(ies)"), "{}", note.1);

        // No skills directory at all is an empty index, not a failure.
        let bare = std::env::temp_dir().join(format!("harness-skills-{}", crate::storage::uid()));
        fs::create_dir_all(&bare).unwrap();
        assert!(discover(&bare).unwrap().skills.is_empty());
        assert!(index_parts(&bare).unwrap().is_empty());
        fs::remove_dir_all(bare).ok();
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn skills_index_stops_at_the_cap_and_says_so() {
        let names: Vec<String> = (0..MAX_SKILLS + 2)
            .map(|i| format!("skill-{i:03}"))
            .collect();
        let entries: Vec<(&str, &str)> = names
            .iter()
            .map(|name| (name.as_str(), "description: one of many\n\nbody\n"))
            .collect();
        let root = project(&entries);
        let index = discover(&root).unwrap();
        assert_eq!(index.skills.len(), MAX_SKILLS);
        assert_eq!((index.over_cap, index.skipped), (2, 0));
        let note = index_parts(&root).unwrap().pop().unwrap();
        assert!(
            note.1.contains("2 skill(s) past the 32-skill cap"),
            "{}",
            note.1
        );
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn skill_bodies_drop_frontmatter_and_cut_on_a_utf8_boundary() {
        let (body, truncated) = bounded_body("---\ndescription: d\n---\n\nRun `cargo test`.\n");
        assert_eq!(body, "Run `cargo test`.\n");
        assert!(!truncated);
        assert_eq!(
            bounded_body("no frontmatter here\n").0,
            "no frontmatter here\n"
        );
        assert_eq!(
            bounded_body("---\nunterminated: true\n").0,
            "---\nunterminated: true\n",
            "an unterminated header is body text, not a parse error"
        );

        let long = "\u{9577}".repeat(MAX_BODY_BYTES); // 3 bytes each, so well past the cap
        let (cut, truncated) = bounded_body(&long);
        assert!(truncated && cut.len() <= MAX_BODY_BYTES);
        assert!(long.starts_with(&cut), "the cap must keep a prefix");
        assert!(
            cut.chars().all(|c| c == '\u{9577}'),
            "and never split a character"
        );

        // A description is redacted before it can reach the provider window.
        assert!(
            !describe("---\ndescription: api_key=synthetic\n---\nbody\n").contains("synthetic")
        );
    }

    #[test]
    fn skill_names_must_be_one_safe_path_segment() {
        for name in ["review", "code-review", "code_review", "a1"] {
            assert!(valid_name(name), "{name}");
        }
        for name in [
            "",
            "..",
            ".",
            "a/b",
            "a b",
            "a.b",
            "../secrets",
            &"x".repeat(65),
        ] {
            assert!(!valid_name(name), "{name}");
        }
    }
}
