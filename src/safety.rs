//! Best-effort redaction, not a secret vault or a complete DLP system.
use anyhow::{bail, Result};
use ring::digest::{digest, SHA256};

pub fn fingerprint(text: &str) -> String {
    digest(&SHA256, text.as_bytes()).as_ref().iter().map(|b| format!("{b:02x}")).collect()
}

pub fn sensitive(text: &str) -> bool {
    let lower = text.to_lowercase();
    ["private key", "private_key", "age-secret-key-", "password", "passwd", "api_key", "api-key", "api key", "access_token", "refresh_token", "client_secret", "credential", "authorization:", "bearer ", "ghp_", "github_pat_", "xoxb-", "xoxp-"].iter().any(|m| lower.contains(m))
        || text.split(|c: char| !c.is_ascii_alphanumeric() && c != '_' && c != '-').any(|w| {
            (w.starts_with("sk-") && w.len() > 12)
                || (w.starts_with("AKIA") && w.len() == 20)
        })
}

/// Private-key bodies are removed together with their delimiters.
/// Other suspicious lines are removed in full (deliberately conservative).
pub fn redact(text: &str) -> String {
    let mut output = Vec::new();
    let mut in_key = false;
    for line in text.split('\n') {
        let upper = line.to_uppercase();
        if upper.contains("-----BEGIN") && upper.contains("PRIVATE KEY-----") {
            in_key = !upper.contains("-----END");
            output.push("[REDACTED SENSITIVE CONTENT]");
        } else if in_key {
            if upper.contains("-----END") { in_key = false; }
        } else if sensitive(line) {
            output.push("[REDACTED SENSITIVE CONTENT]");
        } else {
            output.push(line);
        }
    }
    output.join("\n")
}

pub fn scope(value: &str) -> Result<()> {
    if value.is_empty() || value.len() > 80 || !value.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c,'_'|'-'|'.'|':')) {
        bail!("scope must contain 1-80 ASCII letters, digits, _, -, . or :");
    }
    Ok(())
}

pub fn validate_fact(key: &str, value: &str, category: &str) -> Result<()> {
    if key.is_empty() || key.len() > 80 || !key.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_') {
        bail!("invalid memory key");
    }
    if value.trim().is_empty() || value.chars().count() > 1000 || value.len() > 4000 {
        bail!("invalid memory value");
    }
    if !["preference","fact","project","rule","skill","decision","episodic","procedural"].contains(&category) || sensitive(key) || sensitive(value) {
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
        if words.len() == 24 { break; }
    }
    words.iter().map(|w| format!("\"{w}\"")).collect::<Vec<_>>().join(" OR ")
}

/// Explicit user language that retracts or replaces a prior choice. This only changes review
/// priority; exact user evidence and human approval remain mandatory.
pub fn is_correction(input:&str)->bool{
    let text=input.trim().to_lowercase();
    ["no ","no,","no.","actually ","correction:","instead ","rather ","don't ","do not ","not that"].iter().any(|prefix|text.starts_with(prefix))
        || ((text.starts_with("use ") || text.contains(" use ")) && text.contains(" instead"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn redacts_private_block() {
        let text = "before\n-----BEGIN PRIVATE KEY-----\nsynthetic-payload\n-----END PRIVATE KEY-----\nafter";
        let out = redact(text);
        assert!(!out.contains("synthetic-payload")); assert!(out.ends_with("after"));
    }
    #[test] fn redacts_key_without_changing_safe_unicode() {
        assert_eq!(redact("Halo 🦀\nhello"), "Halo 🦀\nhello");
        assert!(!redact("api_key=synthetic").contains("synthetic"));
    }
    #[test] fn short_words_survive_and_operators_do_not() {
        assert_eq!(fts_query("SQL API Rust: ____"), "\"sql\" OR \"api\" OR \"rust\"");
        assert_eq!(fts_query("***"), "");
    }
    #[test] fn credentials_cannot_be_facts() {
        assert!(validate_fact("api_key","synthetic","fact").is_err());
        assert!(validate_fact("editor","Rust","preference").is_ok());
    }
    #[test] fn extraction_corrections_are_detected_without_fuzzy_guessing() {
        for text in ["No, use SQLite", "Actually use Rust", "Use Postgres instead"] {assert!(is_correction(text),"{text}");}
        for text in ["I use SQLite", "This is not that large"] {assert!(!is_correction(text),"{text}");}
    }
}
