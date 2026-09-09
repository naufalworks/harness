//! read / grep / glob. Contract: docs/design/tools.md.
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;
use super::{content_hash, paths, render_line, Tool, ToolCtx, ToolResult};

const READ_MAX_FILE: u64 = 2 * 1024 * 1024;

fn arg_str<'a>(args: &'a Value, key: &str) -> Option<&'a str> { args.get(key).and_then(Value::as_str) }
fn arg_usize(args: &Value, key: &str) -> Option<usize> { args.get(key).and_then(Value::as_u64).map(|v| v as usize) }

pub(crate) fn read_text(path: &Path) -> Result<String, ToolResult> {
    let meta = std::fs::metadata(path).map_err(|_| ToolResult::err("not_found", format!("no such file: {}", path.display())))?;
    if meta.is_dir() { return Err(ToolResult::err("is_directory", "path is a directory; use glob")); }
    if meta.len() > READ_MAX_FILE { return Err(ToolResult::err("too_large", format!("file is {} bytes; use grep", meta.len()))); }
    let bytes = std::fs::read(path).map_err(|e| ToolResult::err("not_found", e))?;
    if bytes.iter().take(8192).any(|b| *b == 0) { return Err(ToolResult::err("binary_file", "file looks binary")); }
    String::from_utf8(bytes).map_err(|_| ToolResult::err("binary_file", "file is not valid UTF-8"))
}

pub struct Read;
impl Tool for Read {
    fn name(&self) -> &'static str { "read" }
    fn schema(&self) -> &'static str { include_str!("../../tools/schemas/read.json") }
    fn side_effecting(&self) -> bool { false }
    fn summary(&self, a: &Value) -> String {
        let off = arg_usize(a, "offset").unwrap_or(1); let lim = arg_usize(a, "limit").unwrap_or(200).min(400);
        format!("read {} {}-{}", arg_str(a, "path").unwrap_or("?"), off, off + lim - 1)
    }
    fn run(&self, ctx: &ToolCtx, args: Value) -> ToolResult {
        let Some(rel) = arg_str(&args, "path") else { return ToolResult::err("invalid_arguments", "path is required") };
        let path = match paths::resolve(&ctx.root, rel) { Ok(p) => p, Err(e) => return ToolResult::err(e.code(), e.detail()) };
        let text = match read_text(&path) { Ok(t) => t, Err(r) => return r };
        let offset = arg_usize(&args, "offset").unwrap_or(1).max(1);
        let limit = arg_usize(&args, "limit").unwrap_or(200).clamp(1, 400);
        let lines: Vec<&str> = text.lines().collect();
        let total = lines.len();
        let start = offset - 1;
        let end = (start + limit).min(total);
        let shown = paths::display(&ctx.root, &path);
        let mut out = format!("file: {shown}  lines {}-{} of {total}  content_hash: {}\n", if start < total { offset } else { 0 }, end, content_hash(&text));
        for (i, line) in lines.iter().enumerate().take(end).skip(start) { out.push_str(&render_line(i + 1, line)); out.push('\n'); }
        if start >= total && total > 0 { out.push_str("(offset is past the end of the file)\n"); }
        let summary = format!("read {shown} {}-{}", offset, end);
        ToolResult::ok(summary, out)
    }
}

pub struct Grep;
impl Tool for Grep {
    fn name(&self) -> &'static str { "grep" }
    fn schema(&self) -> &'static str { include_str!("../../tools/schemas/grep.json") }
    fn side_effecting(&self) -> bool { false }
    fn summary(&self, a: &Value) -> String { format!("grep {:?}", arg_str(a, "pattern").unwrap_or("")) }
    fn run(&self, ctx: &ToolCtx, args: Value) -> ToolResult {
        let Some(pattern) = arg_str(&args, "pattern").filter(|p| !p.is_empty()) else { return ToolResult::err("invalid_arguments", "pattern is required") };
        let base = match paths::resolve(&ctx.root, arg_str(&args, "path").unwrap_or(".")) { Ok(p) => p, Err(e) => return ToolResult::err(e.code(), e.detail()) };
        let max = arg_usize(&args, "max_results").unwrap_or(50).clamp(1, 100);
        let context = arg_usize(&args, "context").unwrap_or(1).min(3);
        let ci = args.get("case_insensitive").and_then(Value::as_bool).unwrap_or(false);
        let glob = arg_str(&args, "glob");
        let hits = match rg_available() {
            true => grep_rg(&ctx.root, &base, pattern, glob, ci, max, context),
            false => grep_fallback(&ctx.root, &base, pattern, glob, ci, max, context),
        };
        let (lines, count, capped, engine) = match hits { Ok(v) => v, Err(r) => return r };
        let mut out = lines.join("\n");
        out.push_str(&format!("\n{count} matches{}{}", if capped { " (capped)" } else { "" }, if engine == "literal" { "  [note: ripgrep not installed; pattern was matched literally, not as a regex]" } else { "" }));
        ToolResult::ok(format!("grep {pattern:?} → {count} hits"), out)
    }
}

fn rg_available() -> bool { Command::new("rg").arg("--version").output().map(|o| o.status.success()).unwrap_or(false) }

type GrepOut = Result<(Vec<String>, usize, bool, &'static str), ToolResult>;

fn grep_rg(root: &Path, base: &Path, pattern: &str, glob: Option<&str>, ci: bool, max: usize, context: usize) -> GrepOut {
    let mut cmd = Command::new("rg");
    cmd.arg("--json").arg("--max-count").arg("200").arg("-C").arg(context.to_string()).arg("--no-messages");
    if ci { cmd.arg("-i"); }
    if let Some(g) = glob { cmd.arg("-g").arg(g); }
    for name in ["!.env", "!.env.*", "!*.pem", "!*.key", "!id_rsa*", "!id_ed25519*", "!*.p12", "!*.pfx", "!.netrc", "!.npmrc", "!.pypirc", "!*.kdbx"] { cmd.arg("-g").arg(name); }
    cmd.arg("-e").arg(pattern).arg(base).current_dir(root);
    let out = cmd.output().map_err(|e| ToolResult::err("invalid_arguments", format!("ripgrep failed to start: {e}")))?;
    if !out.status.success() && !out.stderr.is_empty() {
        return Err(ToolResult::err("invalid_arguments", String::from_utf8_lossy(&out.stderr).trim().to_string()));
    }
    let (mut lines, mut count, mut capped) = (Vec::new(), 0usize, false);
    for raw in String::from_utf8_lossy(&out.stdout).lines() {
        let Ok(v) = serde_json::from_str::<Value>(raw) else { continue };
        let kind = v.get("type").and_then(Value::as_str).unwrap_or("");
        if kind != "match" && kind != "context" { continue; }
        let d = &v["data"];
        let path = d["path"]["text"].as_str().unwrap_or("?");
        let path = paths::display(root, Path::new(path));
        let no = d["line_number"].as_u64().unwrap_or(0) as usize;
        let text = d["lines"]["text"].as_str().unwrap_or("").trim_end_matches('\n');
        if kind == "match" {
            if count >= max { capped = true; break; }
            count += 1;
            lines.push(format!("{path}:{}", render_line(no, text)));
        } else {
            lines.push(format!("{path}-{}", render_line(no, text)));
        }
    }
    Ok((lines, count, capped, "rg"))
}

fn grep_fallback(root: &Path, base: &Path, pattern: &str, glob: Option<&str>, ci: bool, max: usize, context: usize) -> GrepOut {
    let needle = if ci { pattern.to_lowercase() } else { pattern.to_string() };
    let mut files = Vec::new();
    if base.is_file() { files.push(base.to_path_buf()); } else { walk(root, base, &mut files, 20_000); }
    let (mut lines, mut count, mut capped) = (Vec::new(), 0usize, false);
    'outer: for f in files {
        let rel = paths::display(root, &f);
        if let Some(g) = glob { if !glob_match(g, &rel) && !glob_match(g, f.file_name().map(|n| n.to_string_lossy()).as_deref().unwrap_or("")) { continue; } }
        let Ok(text) = read_text(&f) else { continue };
        let all: Vec<&str> = text.lines().collect();
        for (i, line) in all.iter().enumerate() {
            let hay = if ci { line.to_lowercase() } else { line.to_string() };
            if !hay.contains(&needle) { continue; }
            if count >= max { capped = true; break 'outer; }
            count += 1;
            for c in i.saturating_sub(context)..i { lines.push(format!("{rel}-{}", render_line(c + 1, all[c]))); }
            lines.push(format!("{rel}:{}", render_line(i + 1, line)));
            for c in i + 1..(i + 1 + context).min(all.len()) { lines.push(format!("{rel}-{}", render_line(c + 1, all[c]))); }
        }
    }
    Ok((lines, count, capped, "literal"))
}

const SKIP_DIRS: &[&str] = &[".git", "node_modules", "target", "dist", "build", ".venv", "__pycache__", ".harness"];

/// Bounded recursive walk that skips VCS/build dirs, symlinks that leave root, and denied names.
pub(crate) fn walk(root: &Path, dir: &Path, out: &mut Vec<PathBuf>, cap: usize) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut entries: Vec<_> = rd.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        if out.len() >= cap { return; }
        let p = e.path();
        let name = e.file_name().to_string_lossy().to_string();
        if paths::is_denied_name(&name) { continue; }
        let Ok(ft) = e.file_type() else { continue };
        if ft.is_symlink() { match std::fs::canonicalize(&p) { Ok(c) if c.starts_with(root) => {}, _ => continue } }
        if p.is_dir() { if SKIP_DIRS.contains(&name.as_str()) { continue; } walk(root, &p, out, cap); }
        else if p.is_file() { out.push(p); }
    }
}

/// gitignore-style matcher: `**` any depth, `*` within a segment, `?` one char. Anchored to the whole path.
pub(crate) fn glob_match(pattern: &str, path: &str) -> bool {
    fn seg_match(p: &[char], s: &[char]) -> bool {
        match (p.first(), s.first()) {
            (None, None) => true,
            (Some('*'), _) => seg_match(&p[1..], s) || (!s.is_empty() && seg_match(p, &s[1..])),
            (Some('?'), Some(_)) => seg_match(&p[1..], &s[1..]),
            (Some(a), Some(b)) if a == b => seg_match(&p[1..], &s[1..]),
            _ => false,
        }
    }
    fn parts_match(p: &[&str], s: &[&str]) -> bool {
        match p.first() {
            None => s.is_empty(),
            Some(&"**") => (0..=s.len()).any(|i| parts_match(&p[1..], &s[i..])),
            Some(seg) => !s.is_empty() && seg_match(&seg.chars().collect::<Vec<_>>(), &s[0].chars().collect::<Vec<_>>()) && parts_match(&p[1..], &s[1..]),
        }
    }
    let pat: Vec<&str> = pattern.trim_start_matches("./").split('/').filter(|s| !s.is_empty()).collect();
    let segs: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    // A pattern without a slash matches the basename anywhere (gitignore semantics).
    if pat.len() == 1 { return segs.last().is_some_and(|b| parts_match(&pat, &[b])); }
    parts_match(&pat, &segs)
}

pub struct Glob;
impl Tool for Glob {
    fn name(&self) -> &'static str { "glob" }
    fn schema(&self) -> &'static str { include_str!("../../tools/schemas/glob.json") }
    fn side_effecting(&self) -> bool { false }
    fn summary(&self, a: &Value) -> String { format!("glob {}", arg_str(a, "pattern").unwrap_or("?")) }
    fn run(&self, ctx: &ToolCtx, args: Value) -> ToolResult {
        let Some(pattern) = arg_str(&args, "pattern").filter(|p| !p.is_empty()) else { return ToolResult::err("invalid_arguments", "pattern is required") };
        let base = match paths::resolve(&ctx.root, arg_str(&args, "path").unwrap_or(".")) { Ok(p) => p, Err(e) => return ToolResult::err(e.code(), e.detail()) };
        let mut files = Vec::new();
        walk(&ctx.root, &base, &mut files, 50_000);
        let mut matched: Vec<(std::time::SystemTime, String)> = files.into_iter().filter_map(|f| {
            let rel = paths::display(&ctx.root, &f);
            let rel_to_base = paths::display(&base, &f);
            if glob_match(pattern, &rel) || glob_match(pattern, &rel_to_base) {
                let mtime = std::fs::metadata(&f).and_then(|m| m.modified()).unwrap_or(std::time::UNIX_EPOCH);
                Some((mtime, rel))
            } else { None }
        }).collect();
        matched.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
        let total = matched.len();
        let mut out: Vec<String> = matched.into_iter().take(500).map(|(_, p)| p).collect();
        out.push(format!("{total} total{}", if total > 500 { " (showing 500)" } else { "" }));
        ToolResult::ok(format!("glob {pattern} → {total} files"), out.join("\n"))
    }
}

#[cfg(test)]
mod tests {
    use super::glob_match;
    #[test] fn globs() {
        assert!(glob_match("src/**/*.rs", "src/main.rs"));
        assert!(glob_match("src/**/*.rs", "src/a/b/c.rs"));
        assert!(!glob_match("src/**/*.rs", "tests/x.rs"));
        assert!(glob_match("*.json", "tools/schemas/read.json"));
        assert!(glob_match("**/package.json", "package.json"));
        assert!(glob_match("**/package.json", "a/b/package.json"));
        assert!(!glob_match("*.rs", "src/main.py"));
    }
}
