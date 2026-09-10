//! Minimal line-based unified diff (no external crate). O(n*m) LCS with a size cap;
//! beyond the cap it degrades to a whole-file replacement hunk, which is still a valid diff.

pub struct Diff {
    pub text: String,
    pub plus: usize,
    pub minus: usize,
}

const LCS_CAP: usize = 4_000_000; // n*m cells

pub fn unified(path: &str, before: &str, after: &str) -> Diff {
    let a: Vec<&str> = before.split_inclusive('\n').collect();
    let b: Vec<&str> = after.split_inclusive('\n').collect();
    let ops = if a.len().saturating_mul(b.len()) > LCS_CAP {
        replace_all(&a, &b)
    } else {
        lcs_ops(&a, &b)
    };
    render(path, &a, &b, &ops)
}

#[derive(Clone, Copy, PartialEq)]
enum Op {
    Eq(usize, usize),
    Del(usize),
    Ins(usize),
}

fn replace_all(a: &[&str], b: &[&str]) -> Vec<Op> {
    (0..a.len())
        .map(Op::Del)
        .chain((0..b.len()).map(Op::Ins))
        .collect()
}

fn lcs_ops(a: &[&str], b: &[&str]) -> Vec<Op> {
    let (n, m) = (a.len(), b.len());
    let mut dp = vec![0u32; (n + 1) * (m + 1)];
    let idx = |i: usize, j: usize| i * (m + 1) + j;
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            dp[idx(i, j)] = if a[i] == b[j] {
                dp[idx(i + 1, j + 1)] + 1
            } else {
                dp[idx(i + 1, j)].max(dp[idx(i, j + 1)])
            };
        }
    }
    let (mut i, mut j, mut ops) = (0, 0, Vec::new());
    while i < n && j < m {
        if a[i] == b[j] {
            ops.push(Op::Eq(i, j));
            i += 1;
            j += 1;
        } else if dp[idx(i + 1, j)] >= dp[idx(i, j + 1)] {
            ops.push(Op::Del(i));
            i += 1;
        } else {
            ops.push(Op::Ins(j));
            j += 1;
        }
    }
    while i < n {
        ops.push(Op::Del(i));
        i += 1;
    }
    while j < m {
        ops.push(Op::Ins(j));
        j += 1;
    }
    ops
}

fn render(path: &str, a: &[&str], b: &[&str], ops: &[Op]) -> Diff {
    const CTX: usize = 3;
    let mut out = format!("--- a/{path}\n+++ b/{path}\n");
    let (mut plus, mut minus) = (0, 0);
    let changed: Vec<bool> = ops.iter().map(|o| !matches!(o, Op::Eq(..))).collect();
    let mut k = 0;
    while k < ops.len() {
        if !changed[k] {
            k += 1;
            continue;
        }
        // Group changes whose gaps are <= 2*CTX into one hunk, padded by CTX on both sides.
        let first = k;
        let mut last = k;
        let mut p = k + 1;
        while p < ops.len() && p <= last + 2 * CTX {
            if changed[p] {
                last = p;
            }
            p += 1;
        }
        let start = first.saturating_sub(CTX);
        let end = (last + CTX + 1).min(ops.len());
        let (mut a0, mut b0, mut alen, mut blen) = (None, None, 0, 0);
        let mut body = String::new();
        for op in &ops[start..end] {
            match *op {
                Op::Eq(i, j) => {
                    a0.get_or_insert(i);
                    b0.get_or_insert(j);
                    alen += 1;
                    blen += 1;
                    body.push(' ');
                    body.push_str(a[i]);
                }
                Op::Del(i) => {
                    a0.get_or_insert(i);
                    alen += 1;
                    minus += 1;
                    body.push('-');
                    body.push_str(a[i]);
                }
                Op::Ins(j) => {
                    b0.get_or_insert(j);
                    blen += 1;
                    plus += 1;
                    body.push('+');
                    body.push_str(b[j]);
                }
            }
            if !body.ends_with('\n') {
                body.push_str("\n\\ No newline at end of file\n");
            }
        }
        let a0 = a0.unwrap_or(0);
        let b0 = b0.unwrap_or(0);
        out.push_str(&format!(
            "@@ -{},{} +{},{} @@\n",
            if alen == 0 { a0 } else { a0 + 1 },
            alen,
            if blen == 0 { b0 } else { b0 + 1 },
            blen
        ));
        out.push_str(&body);
        k = end;
    }
    Diff {
        text: out,
        plus,
        minus,
    }
}

// ---- reversing (P2-T03) ----------------------------------------------------------------

struct Hunk {
    b0: usize,
    blen: usize,
    ops: Vec<(char, String)>,
}

/// Rebuild the pre-change content by reverse-applying a diff this module rendered.
///
/// `after` must be exactly the content the diff produced: every hunk's after-side is compared
/// against it line by line, so a file that drifted yields `None` instead of a plausible-looking
/// wrong file. A truncated diff is refused for the same reason — the caller then has to say
/// "no undo" rather than write a partial restore over someone's later work.
pub fn reverse(after: &str, diff: &str) -> Option<String> {
    if diff.contains("[diff truncated]") {
        return None;
    }
    let hunks = parse(diff)?;
    let lines: Vec<&str> = after.split_inclusive('\n').collect();
    let (mut out, mut cursor) = (String::new(), 0usize);
    for h in &hunks {
        // `render` writes a 1-based line, or a bare 0 when that side of the hunk is empty.
        let start = if h.blen == 0 {
            h.b0
        } else {
            h.b0.checked_sub(1)?
        };
        if start < cursor || start > lines.len() {
            return None;
        }
        for line in &lines[cursor..start] {
            out.push_str(line);
        }
        let mut seen = 0usize;
        for (tag, text) in &h.ops {
            match *tag {
                ' ' => {
                    if *lines.get(start + seen)? != text.as_str() {
                        return None;
                    }
                    seen += 1;
                    out.push_str(text);
                }
                '+' => {
                    if *lines.get(start + seen)? != text.as_str() {
                        return None;
                    }
                    seen += 1;
                }
                '-' => out.push_str(text),
                _ => return None,
            }
        }
        if seen != h.blen {
            return None;
        }
        cursor = start + seen;
    }
    for line in &lines[cursor..] {
        out.push_str(line);
    }
    Some(out)
}

/// Split a rendered diff into hunks, folding every `\ No newline at end of file` marker back
/// into the line it describes: `render` adds the newline that the marker then takes away.
fn parse(diff: &str) -> Option<Vec<Hunk>> {
    let mut hunks: Vec<Hunk> = Vec::new();
    for line in diff.split_inclusive('\n') {
        if line.starts_with("--- ") || line.starts_with("+++ ") {
            continue;
        }
        if let Some(rest) = line.strip_prefix("@@ ") {
            let mut parts = rest.split_whitespace();
            parts.next()?;
            let (b0, blen) = parts.next()?.strip_prefix('+')?.split_once(',')?;
            hunks.push(Hunk {
                b0: b0.parse().ok()?,
                blen: blen.parse().ok()?,
                ops: Vec::new(),
            });
            continue;
        }
        let hunk = hunks.last_mut()?;
        let mut chars = line.chars();
        let tag = chars.next()?;
        if tag == '\\' {
            let last = hunk.ops.last_mut()?;
            if last.1.ends_with('\n') {
                last.1.pop();
            }
            continue;
        }
        if !matches!(tag, ' ' | '+' | '-') {
            return None;
        }
        hunk.ops.push((tag, chars.as_str().to_string()));
    }
    Some(hunks)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn counts() {
        let d = unified("f", "a\nb\nc\n", "a\nB\nc\nd\n");
        assert_eq!((d.plus, d.minus), (2, 1));
        assert!(d.text.contains("-b\n") && d.text.contains("+B\n") && d.text.contains("+d\n"));
    }
    #[test]
    fn identical() {
        let d = unified("f", "a\n", "a\n");
        assert_eq!((d.plus, d.minus), (0, 0));
        assert!(!d.text.contains("@@"));
    }
    #[test]
    fn new_file() {
        let d = unified("f", "", "x\ny\n");
        assert_eq!((d.plus, d.minus), (2, 0));
    }

    /// The undo behind a diff card: what `reverse` returns has to be the original, byte for byte.
    #[test]
    fn reverse_rebuilds_the_original_from_recorded_changes() {
        for (before, after) in [
            ("a\nb\nc\n", "a\nB\nc\n"),
            (
                "one\ntwo\nthree\nfour\nfive\nsix\nseven\neight\nnine\n",
                "one\ntwo\nTHREE\nfour\nfive\nsix\nseven\nEIGHT\nnine\n",
            ),
            ("keep\ndrop\nkeep2\n", "keep\nkeep2\n"),
            ("a\n", "a\nb\nc\n"),
            ("trailing", "trailing\nmore\n"),
            ("ends without newline", "ends without newline too"),
        ] {
            let d = unified("f", before, after);
            assert_eq!(
                reverse(after, &d.text).as_deref(),
                Some(before),
                "before={before:?} after={after:?} diff={}",
                d.text
            );
        }
    }
    /// An empty side is the whole point of create and delete cards.
    #[test]
    fn reverse_handles_created_and_emptied_files_in_recorded_changes() {
        let created = unified("f", "", "x\ny\n");
        assert_eq!(reverse("x\ny\n", &created.text).as_deref(), Some(""));
        let emptied = unified("f", "x\ny", "");
        assert_eq!(reverse("", &emptied.text).as_deref(), Some("x\ny"));
    }
    /// The guard that matters: a file whose recorded lines moved on must not be "restored" from a
    /// stale diff. Drift *outside* every hunk is invisible here by construction — a diff carries no
    /// claim about lines it never touched — so the caller proves the whole file by hash on both
    /// sides before and after rebuilding. `restore_change` in src/main.rs does exactly that.
    #[test]
    fn reverse_refuses_content_the_recorded_changes_did_not_produce() {
        let d = unified("f", "a\nb\nc\n", "a\nB\nc\n");
        assert_eq!(
            reverse("a\nEDITED\nc\n", &d.text),
            None,
            "the lines the diff claims to have written have to still be there"
        );
        assert_eq!(
            reverse("a\nB\n", &d.text),
            None,
            "a file that lost the recorded lines is not restorable"
        );
        assert_eq!(reverse("totally different\n", &d.text), None);
        assert_eq!(
            reverse(
                "a\nB\nc\n",
                "--- a/f\n+++ b/f\n@@ -1,3 +1,3 @@\n a\n-b\n+B\n c\n\n\u{2026}[diff truncated]\n"
            ),
            None
        );
        // A foreign line appended after the hunk rebuilds and is carried along, because nothing in
        // the diff describes it. The rebuilt text is therefore not the recorded original, which is
        // exactly what the caller's `before_hash` check catches before anything is written.
        let drifted = reverse("a\nB\nc\nsomeone else edited this\n", &d.text);
        assert_eq!(
            drifted.as_deref(),
            Some("a\nb\nc\nsomeone else edited this\n")
        );
        assert_ne!(
            drifted.as_deref(),
            Some("a\nb\nc\n"),
            "a drifted file must never rebuild to the recorded original"
        );
    }
}
