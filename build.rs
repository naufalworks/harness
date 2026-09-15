use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

fn git(args: &[&str]) -> Option<String> {
    let output = Command::new("git").args(args).output().ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
}

// P11-T06: precompress the embedded static assets at build time.
//
// The crate registry is not reachable from the build environment, so a compression crate cannot
// be added. It is not needed: gzip is a stable stream format, the assets are known at build
// time, and the system `gzip` produces the bytes deterministically with `-n` (no name, no
// timestamp). The server then serves those bytes verbatim, so no compression work happens per
// request at all - strictly better than compressing on the fly.
//
// `gzip` is treated as optional. If it is missing or fails, an empty file is emitted and the
// server serves identity bytes only, so a build can never be blocked by this step.
fn precompress(name: &str, commit: &str, out_dir: &str) {
    let source = format!("static/{name}");
    println!("cargo:rerun-if-changed={source}");
    // Compress exactly what the handler serves, including the substituted build commit.
    let text = std::fs::read_to_string(&source)
        .unwrap_or_else(|e| panic!("read {source}: {e}"))
        .replace("__HARNESS_BUILD_COMMIT__", commit);
    let mut bytes = Vec::new();
    if let Ok(mut child) = Command::new("gzip")
        .args(["-9", "-n", "-c"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    {
        {
            let mut stdin = child.stdin.take().expect("gzip stdin");
            let _ = stdin.write_all(text.as_bytes());
        }
        if let Ok(output) = child.wait_with_output() {
            if output.status.success() {
                bytes = output.stdout;
            }
        }
    }
    if bytes.is_empty() {
        println!("cargo:warning=gzip unavailable; {name} will be served uncompressed");
    }
    let target = Path::new(out_dir).join(format!("{name}.gz"));
    std::fs::write(&target, &bytes).unwrap_or_else(|e| panic!("write {}: {e}", target.display()));
}

fn main() {
    let commit = std::env::var("HARNESS_BUILD_COMMIT")
        .ok()
        .or_else(|| git(&["rev-parse", "HEAD"]))
        .unwrap_or_else(|| "unknown".into());
    println!("cargo:rustc-env=HARNESS_GIT_COMMIT={commit}");
    println!("cargo:rerun-if-env-changed=HARNESS_BUILD_COMMIT");
    let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR");
    for name in ["index.html", "api.js", "app.js", "style.css"] {
        precompress(name, &commit, &out_dir);
    }
    if let Some(head) = git(&["rev-parse", "--git-path", "HEAD"]) {
        println!("cargo:rerun-if-changed={head}");
    }
    if let Some(reference) = git(&["symbolic-ref", "-q", "HEAD"]) {
        if let Some(path) = git(&["rev-parse", "--git-path", &reference]) {
            println!("cargo:rerun-if-changed={path}");
        }
    }
}
