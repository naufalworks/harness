//! Rendering LSP results into the tool's bounded, model-facing text.
//!
//! Diagnostics and references arrive as unbounded server JSON, so everything
//! that turns them into output lives here: severity and code labels, the
//! per-file grouping, the project-relative paths, and the result caps that
//! keep one call's output bounded.
use super::*;

pub(super) fn one_line(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub(super) fn severity(value: Option<u64>) -> &'static str {
    match value {
        Some(1) => "error",
        Some(2) => "warning",
        Some(3) => "info",
        Some(4) => "hint",
        _ => "diagnostic",
    }
}

pub(super) fn code_text(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(code)) => format!(" [{code}]"),
        Some(Value::Number(code)) => format!(" [{code}]"),
        _ => String::new(),
    }
}

pub(super) fn format_diagnostics(
    prepared: &Prepared,
    params: &Value,
) -> Result<ToolResult, ToolResult> {
    let diagnostics = params
        .get("diagnostics")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            ToolResult::err(
                "lsp_protocol",
                "publishDiagnostics has no diagnostics array",
            )
        })?;
    let mut rows = Vec::new();
    let mut invalid = 0usize;
    for diagnostic in diagnostics {
        let Some(start) = diagnostic.pointer("/range/start") else {
            invalid += 1;
            continue;
        };
        let Ok((line, column)) = lsp_to_model(&prepared.text, start) else {
            invalid += 1;
            continue;
        };
        let level = severity(diagnostic.get("severity").and_then(Value::as_u64));
        let message = one_line(
            diagnostic
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("diagnostic without a message"),
        );
        rows.push((
            line,
            column,
            level,
            format!(
                "{}:{line}:{column} {level}{} {message}",
                prepared.display,
                code_text(diagnostic.get("code"))
            ),
        ));
    }
    rows.sort_by(|left, right| (left.0, left.1, left.2).cmp(&(right.0, right.1, right.2)));
    let total = rows.len();
    let mut output = format!("diagnostics for {}: {total}\n", prepared.display);
    for (_, _, _, row) in rows.into_iter().take(MAX_DIAGNOSTICS) {
        output.push_str(&row);
        output.push('\n');
    }
    if total > MAX_DIAGNOSTICS {
        output.push_str(&format!(
            "{} diagnostics omitted by the {MAX_DIAGNOSTICS} result cap\n",
            total - MAX_DIAGNOSTICS
        ));
    }
    if invalid > 0 {
        output.push_str(&format!(
            "{invalid} malformed/out-of-range diagnostics omitted\n"
        ));
    }
    Ok(ToolResult::ok(
        format!("lsp diagnostics {} ({total})", prepared.display),
        output.trim_end().to_string(),
    ))
}

pub(super) fn project_path(ctx: &ToolCtx, uri: &str) -> Result<(PathBuf, String), String> {
    let url = Url::parse(uri).map_err(|_| "invalid URI".to_string())?;
    if url.scheme() != "file" {
        return Err("non-file URI".into());
    }
    let raw = url
        .to_file_path()
        .map_err(|_| "invalid file URI".to_string())?;
    let path = paths::resolve(&ctx.root, &raw.to_string_lossy())
        .map_err(|_| "outside-root or denied path".to_string())?;
    if !path.is_file() {
        return Err("not an existing file".into());
    }
    let display = paths::display(&ctx.root, &path);
    Ok((path, display))
}

pub(super) fn location_parts(location: &Value) -> Option<(&str, &Value)> {
    if let (Some(uri), Some(range)) = (
        location.get("uri").and_then(Value::as_str),
        location.get("range"),
    ) {
        return Some((uri, range));
    }
    match (
        location.get("targetUri").and_then(Value::as_str),
        location
            .get("targetSelectionRange")
            .or_else(|| location.get("targetRange")),
    ) {
        (Some(uri), Some(range)) => Some((uri, range)),
        _ => None,
    }
}

pub(super) struct ReferenceFile {
    pub(super) text: String,
    pub(super) hash: String,
    pub(super) locations: Vec<String>,
}

pub(super) fn format_references(
    ctx: &ToolCtx,
    prepared: &Prepared,
    result: &Value,
) -> Result<ToolResult, ToolResult> {
    let empty = Vec::new();
    let locations = if result.is_null() {
        &empty
    } else {
        result.as_array().ok_or_else(|| {
            ToolResult::err("lsp_protocol", "references result is not an array or null")
        })?
    };
    let mut files: BTreeMap<String, ReferenceFile> = BTreeMap::new();
    let mut seen = HashSet::new();
    let mut omitted = 0usize;
    for location in locations {
        if seen.len() >= MAX_REFERENCES {
            omitted += 1;
            continue;
        }
        let Some((uri, range)) = location_parts(location) else {
            omitted += 1;
            continue;
        };
        let Ok((path, display)) = project_path(ctx, uri) else {
            omitted += 1;
            continue;
        };
        if !files.contains_key(&display) {
            if files.len() >= MAX_FILES {
                omitted += 1;
                continue;
            }
            let Ok(text) = read_text(&path) else {
                omitted += 1;
                continue;
            };
            if text.len() > FILE_MAX {
                omitted += 1;
                continue;
            }
            files.insert(
                display.clone(),
                ReferenceFile {
                    hash: content_hash(&text),
                    text,
                    locations: Vec::new(),
                },
            );
        }
        let Some(start) = range.get("start") else {
            omitted += 1;
            continue;
        };
        let Some(end) = range.get("end") else {
            omitted += 1;
            continue;
        };
        let file = files.get_mut(&display).expect("inserted above");
        let (Ok((start_line, start_column)), Ok((end_line, end_column))) = (
            lsp_to_model(&file.text, start),
            lsp_to_model(&file.text, end),
        ) else {
            omitted += 1;
            continue;
        };
        let key = format!("{display}:{start_line}:{start_column}-{end_line}:{end_column}");
        if !seen.insert(key) {
            continue;
        }
        file.locations.push(format!(
            "  {start_line}:{start_column}-{end_line}:{end_column}"
        ));
    }
    let count: usize = files.values().map(|file| file.locations.len()).sum();
    let mut output = format!("references from {}: {count}\n", prepared.display);
    for (path, file) in files {
        output.push_str(&format!("file: {path}  content_hash: {}\n", file.hash));
        for location in file.locations {
            output.push_str(&location);
            output.push('\n');
        }
    }
    if omitted > 0 {
        output.push_str(&format!(
            "{omitted} outside-root, malformed, oversized, or over-cap locations omitted\n"
        ));
    }
    Ok(ToolResult::ok(
        format!("lsp references {} ({count})", prepared.display),
        output.trim_end().to_string(),
    ))
}

pub(super) fn position_number(position: &Value, key: &str) -> Result<usize, ToolResult> {
    position
        .get(key)
        .and_then(Value::as_u64)
        .filter(|value| *value <= usize::MAX as u64)
        .map(|value| value as usize)
        .ok_or_else(|| {
            ToolResult::err(
                "unsupported_edit",
                format!("text edit position has no valid {key}"),
            )
        })
}
