//! Turning a server's workspace edit into a reviewable, bounded rename.
//!
//! A rename is the one LSP operation that writes, so its edits are collected
//! per file, applied to in-memory text, capped, and handed to the permission
//! gate as pending changes before anything touches the disk.
use super::*;

pub(super) fn collect_uri_edits(
    result: &Value,
) -> Result<BTreeMap<String, Vec<Value>>, ToolResult> {
    if result.is_null() {
        return Err(ToolResult::err(
            "no_match",
            "the language server returned no rename edit",
        ));
    }
    let object = result
        .as_object()
        .ok_or_else(|| ToolResult::err("lsp_protocol", "rename result is not a WorkspaceEdit"))?;
    let mut by_uri: BTreeMap<String, Vec<Value>> = BTreeMap::new();
    if let Some(changes) = object.get("changes") {
        let map = changes.as_object().ok_or_else(|| {
            ToolResult::err("lsp_protocol", "WorkspaceEdit.changes is not an object")
        })?;
        for (uri, edits) in map {
            let edits = edits.as_array().ok_or_else(|| {
                ToolResult::err("lsp_protocol", "WorkspaceEdit change is not an edit array")
            })?;
            by_uri
                .entry(uri.clone())
                .or_default()
                .extend(edits.iter().cloned());
        }
    }
    if let Some(document_changes) = object.get("documentChanges") {
        let list = document_changes.as_array().ok_or_else(|| {
            ToolResult::err(
                "unsupported_edit",
                "resource-operation WorkspaceEdits are not supported",
            )
        })?;
        for change in list {
            if change.get("kind").is_some() {
                return Err(ToolResult::err(
                    "unsupported_edit",
                    "rename requested a create/rename/delete resource operation",
                ));
            }
            let uri = change
                .pointer("/textDocument/uri")
                .and_then(Value::as_str)
                .ok_or_else(|| ToolResult::err("lsp_protocol", "TextDocumentEdit has no URI"))?;
            let edits = change
                .get("edits")
                .and_then(Value::as_array)
                .ok_or_else(|| {
                    ToolResult::err("lsp_protocol", "TextDocumentEdit has no edits array")
                })?;
            by_uri
                .entry(uri.to_string())
                .or_default()
                .extend(edits.iter().cloned());
        }
    }
    if by_uri.is_empty() {
        return Err(ToolResult::err(
            "no_match",
            "the language server returned an empty rename edit",
        ));
    }
    let edits: usize = by_uri.values().map(Vec::len).sum();
    if by_uri.len() > MAX_FILES || edits > MAX_EDITS {
        return Err(ToolResult::err("ambiguous_match", format!(
            "rename would edit {} files and {edits} ranges; limits are {MAX_FILES} files and {MAX_EDITS} edits", by_uri.len()
        )));
    }
    Ok(by_uri)
}

#[derive(Clone)]
pub(super) struct ByteEdit {
    pub(super) start: usize,
    pub(super) end: usize,
    pub(super) replacement: String,
}

pub(super) fn apply_text_edits(before: &str, edits: &[Value]) -> Result<String, ToolResult> {
    let mut parsed = Vec::with_capacity(edits.len());
    for edit in edits {
        let range = edit.get("range").ok_or_else(|| {
            ToolResult::err(
                "unsupported_edit",
                "rename returned an InsertReplaceEdit instead of a bounded TextEdit",
            )
        })?;
        let start = range
            .get("start")
            .ok_or_else(|| ToolResult::err("unsupported_edit", "text edit has no start"))?;
        let end = range
            .get("end")
            .ok_or_else(|| ToolResult::err("unsupported_edit", "text edit has no end"))?;
        let start_byte = utf16_to_byte(
            before,
            position_number(start, "line")?,
            position_number(start, "character")?,
        )
        .map_err(|detail| ToolResult::err("unsupported_edit", detail))?;
        let end_byte = utf16_to_byte(
            before,
            position_number(end, "line")?,
            position_number(end, "character")?,
        )
        .map_err(|detail| ToolResult::err("unsupported_edit", detail))?;
        if end_byte < start_byte {
            return Err(ToolResult::err(
                "unsupported_edit",
                "text edit ends before it starts",
            ));
        }
        let replacement = edit
            .get("newText")
            .and_then(Value::as_str)
            .ok_or_else(|| ToolResult::err("unsupported_edit", "text edit has no newText"))?;
        parsed.push(ByteEdit {
            start: start_byte,
            end: end_byte,
            replacement: replacement.to_string(),
        });
    }
    parsed.sort_by_key(|edit| (edit.start, edit.end));
    let mut prior: Option<(usize, usize)> = None;
    for edit in &parsed {
        if let Some((start, end)) = prior {
            if edit.start < end || edit.start == start {
                return Err(ToolResult::err(
                    "ambiguous_match",
                    "language server returned overlapping text edits",
                ));
            }
        }
        prior = Some((edit.start, edit.end));
    }
    let mut after = before.to_string();
    for edit in parsed.iter().rev() {
        after.replace_range(edit.start..edit.end, &edit.replacement);
    }
    Ok(after)
}

pub(super) fn plan_workspace(
    ctx: &ToolCtx,
    result: &Value,
    expected: &BTreeMap<String, String>,
) -> Result<Vec<PendingChange>, ToolResult> {
    let by_uri = collect_uri_edits(result)?;
    let mut changes = Vec::with_capacity(by_uri.len());
    for (uri, edits) in by_uri {
        let (path, display) = project_path(ctx, &uri).map_err(|detail| {
            ToolResult::err(
                "path_denied",
                format!("language server returned {uri}: {detail}"),
            )
        })?;
        let Some(wanted) = expected.get(&display) else {
            return Err(ToolResult::err("stale_anchor", format!(
                "rename would edit {display}, but expected_files has no current hash for it; run references/read first"
            )));
        };
        let before = read_text(&path)?;
        if before.len() > FILE_MAX {
            return Err(ToolResult::err(
                "too_large",
                format!(
                    "{display} is {} bytes; lsp stops at {FILE_MAX}",
                    before.len()
                ),
            ));
        }
        let actual = content_hash(&before);
        if &actual != wanted {
            return Err(ToolResult::err(
                "stale_anchor",
                format!(
                "{display} changed: expected content_hash {wanted}, current content_hash {actual}"
            ),
            ));
        }
        let after = apply_text_edits(&before, &edits)?;
        if after.len() > FILE_MAX {
            return Err(ToolResult::err(
                "too_large",
                format!(
                    "rename would grow {display} to {} bytes; limit is {FILE_MAX}",
                    after.len()
                ),
            ));
        }
        let change = describe(&ctx.root, path, "modify", Some(before), after);
        if !change.is_noop() {
            changes.push(change);
        }
    }
    changes.sort_by(|left, right| left.display.cmp(&right.display));
    if changes.is_empty() {
        return Err(ToolResult::err(
            "no_match",
            "the language server's rename edit changes no text",
        ));
    }
    Ok(changes)
}

pub(super) fn truncate_utf8(text: &mut String, cap: usize) {
    if text.len() <= cap {
        return;
    }
    const MARKER: &str = "\n…[combined diff truncated]\n";
    let mut end = cap.saturating_sub(MARKER.len());
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    text.truncate(end);
    text.push_str(MARKER);
    if text.len() > cap {
        text.truncate(cap);
    }
}

pub(super) fn rename_payload(
    planned: Result<Vec<PendingChange>, ToolResult>,
    args: &Value,
) -> Value {
    match planned {
        Err(refusal) => json!({
            "path": args.get("path"), "action": "rename", "error": refusal.error_code
        }),
        Ok(changes) => {
            let plus: usize = changes.iter().map(|change| change.plus).sum();
            let minus: usize = changes.iter().map(|change| change.minus).sum();
            let files: Vec<Value> = changes
                .iter()
                .map(|change| {
                    json!({
                        "path": change.display, "before_hash": change.before_hash,
                        "after_hash": change.after_hash, "plus": change.plus, "minus": change.minus
                    })
                })
                .collect();
            let mut diff = changes
                .iter()
                .map(|change| change.diff.as_str())
                .collect::<Vec<_>>()
                .join("\n");
            truncate_utf8(&mut diff, PERMISSION_DIFF_MAX);
            json!({ "path": args.get("path"), "action": "rename", "file_count": files.len(),
                "plus": plus, "minus": minus, "files": files, "diff": diff })
        }
    }
}

pub(super) fn apply_workspace(
    ctx: &ToolCtx,
    new_name: &str,
    changes: Vec<PendingChange>,
) -> ToolResult {
    for change in &changes {
        let current = match read_text(&change.path) {
            Ok(text) => text,
            Err(refusal) => return refusal,
        };
        let actual = content_hash(&current);
        if change.before_hash.as_deref() != Some(actual.as_str()) {
            return ToolResult::err(
                "stale_anchor",
                format!(
                    "{} changed after rename planning; expected {:?}, current {actual}",
                    change.display, change.before_hash
                ),
            );
        }
    }
    let mut applied = Vec::new();
    for (index, change) in changes.iter().enumerate() {
        if let Err(error) = atomic_write(&ctx.root, &change.path, &ctx.step_id, &change.after) {
            let mut rollback_errors = Vec::new();
            for applied_index in applied.iter().rev().copied() {
                let prior: &PendingChange = &changes[applied_index];
                if let Some(before) = prior.before.as_deref() {
                    if let Err(rollback) = atomic_write(
                        &ctx.root,
                        &prior.path,
                        &format!("{}-rollback", ctx.step_id),
                        before,
                    ) {
                        rollback_errors.push(format!("{}: {rollback}", prior.display));
                    }
                }
            }
            let rollback = if rollback_errors.is_empty() {
                "prior writes were rolled back".to_string()
            } else {
                format!("rollback also failed for {}", rollback_errors.join(", "))
            };
            return ToolResult::err(
                "write_failed",
                format!("{}: {error}; {rollback}", change.display),
            );
        }
        applied.push(index);
    }
    let plus: usize = changes.iter().map(|change| change.plus).sum();
    let minus: usize = changes.iter().map(|change| change.minus).sum();
    let summary = format!(
        "renamed to {new_name} in {} files (+{plus} −{minus})",
        changes.len()
    );
    let mut output = format!("{summary}\n");
    for change in &changes {
        output.push_str(&format!(
            "{}  content_hash: {}  (+{} −{})\n",
            change.display, change.after_hash, change.plus, change.minus
        ));
    }
    if let Some(command) = ctx.diagnostics_cmd.as_deref() {
        // Register the diagnostics process group too, so a cancel interrupts a long check.
        let run = run_capped_for(
            command,
            &ctx.root,
            &ctx.scope,
            DIAGNOSTICS_TIMEOUT,
            DIAGNOSTICS_CAP,
            Some(&ctx.request_id),
        );
        output.push_str(&format!(
            "\n[diagnostics {}]\n{}\n",
            run.label(),
            run.output
        ));
    }
    let mut result = ToolResult::ok(summary, output.trim_end().to_string());
    for change in changes {
        result = result.with_artifact(change.artifact());
    }
    result
}

pub(super) fn plan_rename(ctx: &ToolCtx, args: &Value) -> Result<Vec<PendingChange>, ToolResult> {
    let prepared = prepare(ctx, args)?;
    if prepared.operation != Operation::Rename {
        return Err(ToolResult::err(
            "invalid_arguments",
            "only rename has a workspace edit to plan",
        ));
    }
    let response = query_server(ctx, &prepared)?;
    plan_workspace(ctx, &response, &prepared.expected)
}
