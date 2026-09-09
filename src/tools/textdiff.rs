//! Minimal line-based unified diff (no external crate). O(n*m) LCS with a size cap;
//! beyond the cap it degrades to a whole-file replacement hunk, which is still a valid diff.

pub struct Diff { pub text: String, pub plus: usize, pub minus: usize }

const LCS_CAP: usize = 4_000_000; // n*m cells

pub fn unified(path: &str, before: &str, after: &str) -> Diff {
    let a: Vec<&str> = before.split_inclusive('\n').collect();
    let b: Vec<&str> = after.split_inclusive('\n').collect();
    let ops = if a.len().saturating_mul(b.len()) > LCS_CAP { replace_all(&a, &b) } else { lcs_ops(&a, &b) };
    render(path, &a, &b, &ops)
}

#[derive(Clone, Copy, PartialEq)]
enum Op { Eq(usize, usize), Del(usize), Ins(usize) }

fn replace_all(a: &[&str], b: &[&str]) -> Vec<Op> {
    (0..a.len()).map(Op::Del).chain((0..b.len()).map(Op::Ins)).collect()
}

fn lcs_ops(a: &[&str], b: &[&str]) -> Vec<Op> {
    let (n, m) = (a.len(), b.len());
    let mut dp = vec![0u32; (n + 1) * (m + 1)];
    let idx = |i: usize, j: usize| i * (m + 1) + j;
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            dp[idx(i, j)] = if a[i] == b[j] { dp[idx(i + 1, j + 1)] + 1 } else { dp[idx(i + 1, j)].max(dp[idx(i, j + 1)]) };
        }
    }
    let (mut i, mut j, mut ops) = (0, 0, Vec::new());
    while i < n && j < m {
        if a[i] == b[j] { ops.push(Op::Eq(i, j)); i += 1; j += 1; }
        else if dp[idx(i + 1, j)] >= dp[idx(i, j + 1)] { ops.push(Op::Del(i)); i += 1; }
        else { ops.push(Op::Ins(j)); j += 1; }
    }
    while i < n { ops.push(Op::Del(i)); i += 1; }
    while j < m { ops.push(Op::Ins(j)); j += 1; }
    ops
}

fn render(path: &str, a: &[&str], b: &[&str], ops: &[Op]) -> Diff {
    const CTX: usize = 3;
    let mut out = format!("--- a/{path}\n+++ b/{path}\n");
    let (mut plus, mut minus) = (0, 0);
    let changed: Vec<bool> = ops.iter().map(|o| !matches!(o, Op::Eq(..))).collect();
    let mut k = 0;
    while k < ops.len() {
        if !changed[k] { k += 1; continue; }
        // Group changes whose gaps are <= 2*CTX into one hunk, padded by CTX on both sides.
        let first = k;
        let mut last = k;
        let mut p = k + 1;
        while p < ops.len() && p <= last + 2 * CTX { if changed[p] { last = p; } p += 1; }
        let start = first.saturating_sub(CTX);
        let end = (last + CTX + 1).min(ops.len());
        let (mut a0, mut b0, mut alen, mut blen) = (None, None, 0, 0);
        let mut body = String::new();
        for op in &ops[start..end] {
            match *op {
                Op::Eq(i, j) => { a0.get_or_insert(i); b0.get_or_insert(j); alen += 1; blen += 1; body.push(' '); body.push_str(a[i]); }
                Op::Del(i) => { a0.get_or_insert(i); alen += 1; minus += 1; body.push('-'); body.push_str(a[i]); }
                Op::Ins(j) => { b0.get_or_insert(j); blen += 1; plus += 1; body.push('+'); body.push_str(b[j]); }
            }
            if !body.ends_with('\n') { body.push_str("\n\\ No newline at end of file\n"); }
        }
        let a0 = a0.unwrap_or(0); let b0 = b0.unwrap_or(0);
        out.push_str(&format!("@@ -{},{} +{},{} @@\n", if alen == 0 { a0 } else { a0 + 1 }, alen, if blen == 0 { b0 } else { b0 + 1 }, blen));
        out.push_str(&body);
        k = end;
    }
    Diff { text: out, plus, minus }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn counts() { let d = unified("f", "a\nb\nc\n", "a\nB\nc\nd\n"); assert_eq!((d.plus, d.minus), (2, 1)); assert!(d.text.contains("-b\n") && d.text.contains("+B\n") && d.text.contains("+d\n")); }
    #[test] fn identical() { let d = unified("f", "a\n", "a\n"); assert_eq!((d.plus, d.minus), (0, 0)); assert!(!d.text.contains("@@")); }
    #[test] fn new_file() { let d = unified("f", "", "x\ny\n"); assert_eq!((d.plus, d.minus), (2, 0)); }
}
