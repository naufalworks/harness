//! Best-effort redaction, not a secret vault or a complete DLP system.
use anyhow::{bail, Result};
use ring::digest::{digest, SHA256};

pub fn fingerprint(text: &str) -> String {
    digest(&SHA256, text.as_bytes())
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

pub fn sensitive(text: &str) -> bool {
    let lower = text.to_lowercase();
    [
        "private key",
        "private_key",
        "age-secret-key-",
        "password",
        "passwd",
        "api_key",
        "api-key",
        "api key",
        "access_token",
        "refresh_token",
        "client_secret",
        "credential",
        "authorization:",
        "bearer ",
        "ghp_",
        "github_pat_",
        "xoxb-",
        "xoxp-",
    ]
    .iter()
    .any(|m| lower.contains(m))
        || text
            .split(|c: char| !c.is_ascii_alphanumeric() && c != '_' && c != '-')
            .any(|w| {
                (w.starts_with("sk-") && w.len() > 12) || (w.starts_with("AKIA") && w.len() == 20)
            })
}

/// The exact placeholder a dropped line is replaced with.
pub const REDACTION_MARKER: &str = "[REDACTED SENSITIVE CONTENT]";

/// The per-line branch ladder shared by whole-answer and incremental redaction, so the two
/// can never drift. `None` drops the line silently (inside a private-key body).
fn classify_line(line: &str, in_key: bool) -> (Option<String>, bool) {
    let upper = line.to_uppercase();
    if upper.contains("-----BEGIN") && upper.contains("PRIVATE KEY-----") {
        return (
            Some(REDACTION_MARKER.to_string()),
            !upper.contains("-----END"),
        );
    }
    if in_key {
        return (None, !upper.contains("-----END"));
    }
    if sensitive(line) {
        return (Some(REDACTION_MARKER.to_string()), false);
    }
    (Some(line.to_string()), false)
}

/// Private-key bodies are removed together with their delimiters.
/// Other suspicious lines are removed in full (deliberately conservative).
pub fn redact(text: &str) -> String {
    let mut output = Vec::new();
    let mut in_key = false;
    for line in text.split('\n') {
        let (emit, next) = classify_line(line, in_key);
        in_key = next;
        if let Some(value) = emit {
            output.push(value);
        }
    }
    output.join("\n")
}

/// Publishes a redacted answer incrementally without ever releasing a partial line.
///
/// `redact` decides per line, drops a matched line whole, and carries private-key state across
/// lines. A pattern can therefore still be completed by later bytes of the same line, and
/// publication is append-only and durable, so a partial line can never be retracted. The only
/// safe publication unit is a *completed* line.
///
/// For every possible chunking, the concatenation of all `push` results plus `finish` equals
/// `redact` of the whole answer. Dropping the value without calling `finish` discards the
/// withheld tail, which is exactly the failure behaviour this task promises.
pub struct StreamRedactor {
    pending: String,
    in_key: bool,
    emitted: bool,
}

impl Default for StreamRedactor {
    fn default() -> Self {
        Self::new()
    }
}

impl StreamRedactor {
    pub fn new() -> Self {
        Self {
            pending: String::new(),
            in_key: false,
            emitted: false,
        }
    }

    /// Join published lines with the same separator `redact` uses, so dropped key-body lines
    /// leave no blank line behind.
    fn emit(&mut self, value: Option<String>) -> String {
        let Some(value) = value else {
            return String::new();
        };
        let text = if self.emitted {
            format!("\n{value}")
        } else {
            value
        };
        self.emitted = true;
        text
    }

    /// Append provider text and return the lines that became publishable (often empty).
    pub fn push(&mut self, text: &str) -> String {
        self.pending.push_str(text);
        let mut out = String::new();
        while let Some(index) = self.pending.find('\n') {
            let line = self.pending[..index].to_string();
            self.pending.drain(..index + 1);
            let (emit, in_key) = classify_line(&line, self.in_key);
            self.in_key = in_key;
            out.push_str(&self.emit(emit));
        }
        out
    }

    /// Flush the final unterminated line. A failure path must not call this.
    pub fn finish(&mut self) -> String {
        let line = std::mem::take(&mut self.pending);
        let (emit, in_key) = classify_line(&line, self.in_key);
        self.in_key = in_key;
        self.emit(emit)
    }

    /// Bytes still withheld because no line terminator has been observed.
    pub fn pending_len(&self) -> usize {
        self.pending.len()
    }
}

pub fn scope(value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 80
        || !value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | ':'))
    {
        bail!("scope must contain 1-80 ASCII letters, digits, _, -, . or :");
    }
    Ok(())
}

pub fn validate_fact(key: &str, value: &str, category: &str) -> Result<()> {
    if key.is_empty()
        || key.len() > 80
        || !key
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
    {
        bail!("invalid memory key");
    }
    if value.trim().is_empty() || value.chars().count() > 1000 || value.len() > 4000 {
        bail!("invalid memory value");
    }
    if ![
        "preference",
        "fact",
        "project",
        "rule",
        "skill",
        "decision",
        "episodic",
        "procedural",
    ]
    .contains(&category)
        || sensitive(key)
        || sensitive(value)
    {
        bail!("sensitive or unsupported memory category");
    }
    Ok(())
}

pub fn fts_query(input: &str) -> String {
    let mut words = Vec::new();
    for word in input.split(|c: char| !c.is_alphanumeric()) {
        let word = word.to_lowercase();
        if !word.is_empty() && word.len() <= 100 && !words.contains(&word) {
            words.push(word);
        }
        if words.len() == 24 {
            break;
        }
    }
    words
        .iter()
        .map(|w| format!("\"{w}\""))
        .collect::<Vec<_>>()
        .join(" OR ")
}

/// Explicit user language that retracts or replaces a prior choice. This only changes review
/// priority; exact user evidence and human approval remain mandatory.
pub fn is_correction(input: &str) -> bool {
    let text = input.trim().to_lowercase();
    [
        "no ",
        "no,",
        "no.",
        "actually ",
        "correction:",
        "instead ",
        "rather ",
        "don't ",
        "do not ",
        "not that",
    ]
    .iter()
    .any(|prefix| text.starts_with(prefix))
        || ((text.starts_with("use ") || text.contains(" use ")) && text.contains(" instead"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn redacts_private_block() {
        let text = "before\n-----BEGIN PRIVATE KEY-----\nsynthetic-payload\n-----END PRIVATE KEY-----\nafter";
        let out = redact(text);
        assert!(!out.contains("synthetic-payload"));
        assert!(out.ends_with("after"));
    }
    #[test]
    fn redacts_key_without_changing_safe_unicode() {
        assert_eq!(redact("Halo 🦀\nhello"), "Halo 🦀\nhello");
        assert!(!redact("api_key=synthetic").contains("synthetic"));
    }
    #[test]
    fn short_words_survive_and_operators_do_not() {
        assert_eq!(
            fts_query("SQL API Rust: ____"),
            "\"sql\" OR \"api\" OR \"rust\""
        );
        assert_eq!(fts_query("***"), "");
    }
    #[test]
    fn credentials_cannot_be_facts() {
        assert!(validate_fact("api_key", "synthetic", "fact").is_err());
        assert!(validate_fact("editor", "Rust", "preference").is_ok());
    }
    #[test]
    fn extraction_corrections_are_detected_without_fuzzy_guessing() {
        for text in [
            "No, use SQLite",
            "Actually use Rust",
            "Use Postgres instead",
        ] {
            assert!(is_correction(text), "{text}");
        }
        for text in ["I use SQLite", "This is not that large"] {
            assert!(!is_correction(text), "{text}");
        }
    }

    /// I1: for every chunking, published + finish equals redact(whole).
    #[test]
    fn stream_redactor_equals_redact_for_every_chunking() {
        let corpus = [
            "plain answer",
            "line one\nline two\nline three",
            "Halo 🦀\nhello",
            "api_key=synthetic\nsafe tail",
            "before\n-----BEGIN PRIVATE KEY-----\nsynthetic-payload\n-----END PRIVATE KEY-----\nafter",
            "no trailing newline at all",
            "pass\nword=hidden\nsafe tail",
            "",
        ];
        for text in corpus {
            // every single-cut split
            let bytes = text.len();
            for cut in 0..=bytes {
                if !text.is_char_boundary(cut) {
                    continue;
                }
                let mut r = StreamRedactor::new();
                let mut out = r.push(&text[..cut]);
                out.push_str(&r.push(&text[cut..]));
                out.push_str(&r.finish());
                assert_eq!(out, redact(text), "cut={cut} text={text:?}");
            }
            // one character per chunk
            let mut r = StreamRedactor::new();
            let mut out = String::new();
            for ch in text.chars() {
                out.push_str(&r.push(&ch.to_string()));
            }
            out.push_str(&r.finish());
            assert_eq!(out, redact(text), "charwise text={text:?}");
        }
    }

    /// I3/I4: a line split across pushes is withheld until its newline arrives, and a split
    /// secret is never released even though its prefix was already buffered.
    #[test]
    fn split_secret_is_held_back_and_never_published() {
        let mut r = StreamRedactor::new();
        // "pass" alone must not be published: the line is unfinished.
        assert_eq!(r.push("pass"), "");
        assert_eq!(r.push("word=hidden"), "");
        assert_eq!(r.pending_len(), "password=hidden".len());
        // Only once the terminator arrives does the completed line become publishable,
        // and then only as the marker.
        assert_eq!(r.push("\n"), REDACTION_MARKER);
        assert_eq!(r.push("safe tail"), "");
        assert_eq!(r.finish(), "\nsafe tail");
    }

    /// I4: abandoning the redactor (failure path) publishes nothing of the withheld tail.
    #[test]
    fn abandoned_redactor_discards_the_withheld_tail() {
        let mut r = StreamRedactor::new();
        assert_eq!(r.push("visible"), "");
        drop(r);
    }

    /// Emitting must not introduce a blank line where a key body was dropped.
    #[test]
    fn dropped_key_body_leaves_no_blank_line() {
        let text = "before\n-----BEGIN PRIVATE KEY-----\npayload\n-----END PRIVATE KEY-----\nafter";
        let mut r = StreamRedactor::new();
        let mut out = r.push(text);
        out.push_str(&r.finish());
        assert_eq!(out, redact(text));
        assert!(!out.contains("\n\n"), "{out:?}");
    }
}
