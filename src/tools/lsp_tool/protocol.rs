//! The bounded contract between tool arguments and an LSP server.
//!
//! Every limit the tool enforces, the table of supported servers, and the
//! translation between model-facing one-based line/column positions and the
//! UTF-16 positions the protocol speaks live here, so the rest of the tool
//! only ever sees an already validated request.
use super::*;

pub(super) const FILE_MAX: usize = 2 * 1024 * 1024;
pub(super) const FRAME_MAX: usize = 4 * 1024 * 1024;
pub(super) const STDERR_MAX: usize = 8 * 1024;
pub(super) const DEFAULT_TIMEOUT: u64 = 20;
pub(super) const MAX_TIMEOUT: u64 = 60;
pub(super) const MAX_DIAGNOSTICS: usize = 100;
pub(super) const MAX_REFERENCES: usize = 100;
pub(super) const MAX_FILES: usize = 20;
pub(super) const MAX_EDITS: usize = 200;
pub(super) const PERMISSION_DIFF_MAX: usize = 64 * 1024;
pub(super) const DIAGNOSTICS_TIMEOUT: u64 = 60;
pub(super) const DIAGNOSTICS_CAP: usize = 4 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Operation {
    Discover,
    Diagnostics,
    References,
    Rename,
}

impl Operation {
    pub(super) fn parse(args: &Value) -> Result<Self, ToolResult> {
        match args.get("operation").and_then(Value::as_str) {
            Some("discover") => Ok(Self::Discover),
            Some("diagnostics") => Ok(Self::Diagnostics),
            Some("references") => Ok(Self::References),
            Some("rename") => Ok(Self::Rename),
            Some(other) => Err(ToolResult::err(
                "invalid_arguments",
                format!(
                    "unknown lsp operation {other:?}; expected discover, diagnostics, references, or rename"
                ),
            )),
            None => Err(ToolResult::err(
                "invalid_arguments",
                "operation is required",
            )),
        }
    }
}

#[derive(Clone, Copy)]
pub(super) struct ServerSpec {
    pub(super) program: &'static str,
    pub(super) args: &'static [&'static str],
    pub(super) language_id: &'static str,
    pub(super) rust: bool,
}

pub(super) fn server_for(path: &Path) -> Option<ServerSpec> {
    match path.extension().and_then(|part| part.to_str()) {
        Some("rs") => Some(ServerSpec {
            program: "rust-analyzer",
            args: &[],
            language_id: "rust",
            rust: true,
        }),
        Some("c") => Some(ServerSpec {
            program: "clangd",
            args: &[],
            language_id: "c",
            rust: false,
        }),
        Some("h" | "hh" | "hpp" | "hxx" | "cc" | "cpp" | "cxx") => Some(ServerSpec {
            program: "clangd",
            args: &[],
            language_id: "cpp",
            rust: false,
        }),
        _ => None,
    }
}

pub(super) struct Prepared {
    pub(super) operation: Operation,
    pub(super) display: String,
    pub(super) text: String,
    pub(super) uri: String,
    pub(super) position: Option<Value>,
    pub(super) new_name: Option<String>,
    pub(super) expected: BTreeMap<String, String>,
    pub(super) include_declaration: bool,
    pub(super) timeout: Duration,
    pub(super) server: ServerSpec,
}

pub(super) fn positive(args: &Value, key: &str) -> Result<usize, ToolResult> {
    match args.get(key).and_then(Value::as_u64) {
        Some(value) if value > 0 && value <= usize::MAX as u64 => Ok(value as usize),
        _ => Err(ToolResult::err(
            "invalid_arguments",
            format!("{key} is required and must be a positive whole number"),
        )),
    }
}

pub(super) fn valid_hash(hash: &str) -> bool {
    hash.len() == 8 && hash.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

pub(super) fn parse_expected(
    ctx: &ToolCtx,
    args: &Value,
) -> Result<BTreeMap<String, String>, ToolResult> {
    let Some(items) = args.get("expected_files").and_then(Value::as_array) else {
        return Err(ToolResult::err(
            "invalid_arguments",
            "rename requires expected_files copied from current read/references results",
        ));
    };
    if items.is_empty() || items.len() > MAX_FILES {
        return Err(ToolResult::err(
            "invalid_arguments",
            format!("expected_files must contain 1-{MAX_FILES} path/hash pairs"),
        ));
    }
    let mut expected = BTreeMap::new();
    for (index, item) in items.iter().enumerate() {
        let Some(raw) = item.get("path").and_then(Value::as_str) else {
            return Err(ToolResult::err(
                "invalid_arguments",
                format!("expected_files item {} needs path", index + 1),
            ));
        };
        let Some(hash) = item.get("content_hash").and_then(Value::as_str) else {
            return Err(ToolResult::err(
                "invalid_arguments",
                format!("expected_files item {} needs content_hash", index + 1),
            ));
        };
        if !valid_hash(hash) {
            return Err(ToolResult::err(
                "invalid_arguments",
                format!(
                    "expected_files item {} has an invalid 8-hex content_hash",
                    index + 1
                ),
            ));
        }
        let path = paths::resolve(&ctx.root, raw)
            .map_err(|error| ToolResult::err(error.code(), error.detail()))?;
        if !path.is_file() {
            return Err(ToolResult::err(
                "not_found",
                format!(
                    "{} is not an existing file",
                    paths::display(&ctx.root, &path)
                ),
            ));
        }
        let display = paths::display(&ctx.root, &path);
        if expected.insert(display.clone(), hash.to_string()).is_some() {
            return Err(ToolResult::err(
                "invalid_arguments",
                format!("expected_files lists {display} more than once"),
            ));
        }
        let current = read_text(&path)?;
        if current.len() > FILE_MAX {
            return Err(ToolResult::err(
                "too_large",
                format!(
                    "{display} is {} bytes; lsp stops at {FILE_MAX}",
                    current.len()
                ),
            ));
        }
        let actual = content_hash(&current);
        if actual != hash {
            return Err(ToolResult::err(
                "stale_anchor",
                format!(
                "{display} changed: expected content_hash {hash}, current content_hash {actual}"
            ),
            ));
        }
    }
    Ok(expected)
}

pub(super) fn prepare(ctx: &ToolCtx, args: &Value) -> Result<Prepared, ToolResult> {
    let operation = Operation::parse(args)?;
    let Some(raw) = args.get("path").and_then(Value::as_str) else {
        return Err(ToolResult::err("invalid_arguments", "path is required"));
    };
    let path = paths::resolve(&ctx.root, raw)
        .map_err(|error| ToolResult::err(error.code(), error.detail()))?;
    let display = paths::display(&ctx.root, &path);
    let text = read_text(&path)?;
    if text.len() > FILE_MAX {
        return Err(ToolResult::err(
            "too_large",
            format!("{display} is {} bytes; lsp stops at {FILE_MAX}", text.len()),
        ));
    }
    let Some(server) = server_for(&path) else {
        return Err(ToolResult::err(
            "invalid_arguments",
            format!(
                "no language server is configured for {display}; lsp supports Rust and C/C++ files"
            ),
        ));
    };
    let uri = Url::from_file_path(&path)
        .map_err(|_| {
            ToolResult::err(
                "lsp_protocol",
                format!("could not make a file URI for {display}"),
            )
        })?
        .to_string();
    let position = if operation == Operation::Diagnostics {
        None
    } else {
        let (line, column) = (positive(args, "line")?, positive(args, "column")?);
        Some(model_to_lsp(&text, line, column).map_err(|detail| {
            ToolResult::err("invalid_arguments", format!("{display}: {detail}"))
        })?)
    };
    let timeout = match args.get("timeout_seconds") {
        None => DEFAULT_TIMEOUT,
        Some(value) => match value.as_u64() {
            Some(seconds) if (1..=MAX_TIMEOUT).contains(&seconds) => seconds,
            _ => {
                return Err(ToolResult::err(
                    "invalid_arguments",
                    format!("timeout_seconds must be a whole number between 1 and {MAX_TIMEOUT}"),
                ))
            }
        },
    };
    let (new_name, expected) = if operation == Operation::Rename {
        let name = args
            .get("new_name")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .ok_or_else(|| {
                ToolResult::err("invalid_arguments", "rename requires a non-blank new_name")
            })?;
        if name.chars().count() > 128 || name.chars().any(char::is_whitespace) {
            return Err(ToolResult::err(
                "invalid_arguments",
                "new_name must be at most 128 characters and contain no whitespace",
            ));
        }
        let expected = parse_expected(ctx, args)?;
        let actual = content_hash(&text);
        match expected.get(&display) {
            Some(hash) if hash == &actual => {}
            Some(hash) => {
                return Err(ToolResult::err(
                    "stale_anchor",
                    format!(
                "{display} changed: expected content_hash {hash}, current content_hash {actual}"
            ),
                ))
            }
            None => {
                return Err(ToolResult::err(
                    "stale_anchor",
                    format!("expected_files must include {display} with content_hash {actual}"),
                ))
            }
        }
        (Some(name.to_string()), expected)
    } else {
        (None, BTreeMap::new())
    };
    Ok(Prepared {
        operation,
        display,
        text,
        uri,
        position,
        new_name,
        expected,
        include_declaration: args
            .get("include_declaration")
            .and_then(Value::as_bool)
            .unwrap_or(true),
        timeout: Duration::from_secs(timeout),
        server,
    })
}

pub(super) fn line_slice(text: &str, zero_line: usize) -> Option<(usize, &str)> {
    let mut base = 0usize;
    for (index, line) in text.split('\n').enumerate() {
        if index == zero_line {
            return Some((base, line.strip_suffix('\r').unwrap_or(line)));
        }
        base = base.saturating_add(line.len() + 1);
    }
    None
}

pub(super) fn model_to_lsp(text: &str, line: usize, column: usize) -> Result<Value, String> {
    let Some((_, source)) = line.checked_sub(1).and_then(|line| line_slice(text, line)) else {
        return Err(format!("line {line} is outside the file"));
    };
    let count = source.chars().count();
    if column == 0 || column > count + 1 {
        return Err(format!(
            "column {column} is outside line {line} (valid range 1-{})",
            count + 1
        ));
    }
    let units: usize = source.chars().take(column - 1).map(char::len_utf16).sum();
    Ok(json!({ "line": line - 1, "character": units }))
}

pub(super) fn utf16_to_byte(text: &str, line: usize, units: usize) -> Result<usize, String> {
    let Some((base, source)) = line_slice(text, line) else {
        return Err(format!("LSP line {} is outside the file", line + 1));
    };
    let mut seen = 0usize;
    for (byte, ch) in source.char_indices() {
        if seen == units {
            return Ok(base + byte);
        }
        seen += ch.len_utf16();
        if seen > units {
            return Err("LSP position splits a UTF-16 surrogate pair".into());
        }
    }
    if seen == units {
        Ok(base + source.len())
    } else {
        Err(format!("LSP column {units} is outside line {}", line + 1))
    }
}

pub(super) fn lsp_to_model(text: &str, position: &Value) -> Result<(usize, usize), String> {
    let line = position
        .get("line")
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .ok_or_else(|| "LSP position has no line".to_string())?;
    let units = position
        .get("character")
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .ok_or_else(|| "LSP position has no character".to_string())?;
    let (_, source) = line_slice(text, line)
        .ok_or_else(|| format!("LSP line {} is outside the file", line + 1))?;
    let (mut seen, mut column) = (0usize, 1usize);
    for ch in source.chars() {
        if seen == units {
            return Ok((line + 1, column));
        }
        seen += ch.len_utf16();
        column += 1;
        if seen > units {
            return Err("LSP position splits a UTF-16 surrogate pair".into());
        }
    }
    if seen == units {
        Ok((line + 1, column))
    } else {
        Err(format!("LSP column {units} is outside line {}", line + 1))
    }
}
