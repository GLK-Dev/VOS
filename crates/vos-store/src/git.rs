//! Lightweight, deterministic Git repository inspection for context recovery briefings.

use std::path::Path;
use std::process::Command;

use vos_core::GitContext;

/// Inspects Git branch, dirty counters, and last commit when `root_path` lies in a Git repository.
pub fn inspect_git_context(root_path: Option<&str>) -> Option<GitContext> {
    let raw_root = root_path?.trim();
    if raw_root.is_empty() {
        return None;
    }
    let root = Path::new(raw_root);
    if !root.exists() || !has_git_marker(root) {
        return None;
    }

    let status_out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["status", "--porcelain=v1", "--branch"])
        .output()
        .ok()?;
    if !status_out.status.success() {
        return None;
    }
    let status_text = String::from_utf8_lossy(&status_out.stdout);
    let mut ctx = parse_status_porcelain(&status_text)?;

    if let Ok(log_out) = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["log", "-1", "--format=%h%x09%ct%x09%s"])
        .output()
        && log_out.status.success()
    {
        let log_text = String::from_utf8_lossy(&log_out.stdout);
        if let Some((hash, ts, subject)) = parse_last_commit(&log_text) {
            ctx.last_commit_hash = Some(hash);
            ctx.last_commit_at = Some(ts);
            ctx.last_commit_subject = Some(subject);
        }
    }

    Some(ctx)
}

fn has_git_marker(start: &Path) -> bool {
    let mut cur = Some(start);
    while let Some(dir) = cur {
        if dir.join(".git").exists() {
            return true;
        }
        cur = dir.parent();
    }
    false
}

pub(crate) fn parse_status_porcelain(raw: &str) -> Option<GitContext> {
    let mut lines = raw.lines();
    let header = lines.next()?;
    let branch = parse_branch_header(header);

    let mut staged_count = 0_usize;
    let mut modified_count = 0_usize;
    let mut untracked_count = 0_usize;

    for line in lines {
        if line.len() < 2 {
            continue;
        }
        let bytes = line.as_bytes();
        let x = bytes[0] as char;
        let y = bytes[1] as char;
        if x == '?' && y == '?' {
            untracked_count += 1;
            continue;
        }
        if x != ' ' {
            staged_count += 1;
        }
        if y != ' ' {
            modified_count += 1;
        }
    }

    let is_dirty = staged_count + modified_count + untracked_count > 0;
    Some(GitContext {
        branch,
        is_dirty,
        staged_count,
        modified_count,
        untracked_count,
        last_commit_hash: None,
        last_commit_subject: None,
        last_commit_at: None,
    })
}

fn parse_branch_header(header: &str) -> String {
    let stripped = header.strip_prefix("## ").unwrap_or(header).trim();
    if let Some(rest) = stripped.strip_prefix("No commits yet on ") {
        return rest.trim().to_string();
    }
    if let Some(rest) = stripped.strip_prefix("Initial commit on ") {
        return rest.trim().to_string();
    }
    stripped
        .split("...")
        .next()
        .unwrap_or(stripped)
        .split(" [")
        .next()
        .unwrap_or(stripped)
        .trim()
        .to_string()
}

pub(crate) fn parse_last_commit(raw: &str) -> Option<(String, i64, String)> {
    let line = raw.lines().next()?.trim();
    if line.is_empty() {
        return None;
    }
    let mut parts = line.splitn(3, '\t');
    let hash = parts.next()?.trim().to_string();
    let ts: i64 = parts.next()?.trim().parse().ok()?;
    let subject = parts.next().unwrap_or("").trim().to_string();
    if hash.is_empty() {
        return None;
    }
    Some((hash, ts, subject))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_porcelain_branch_and_dirty_counts() {
        let sample = "\
## experimental/recovery-predictor...origin/experimental/recovery-predictor [ahead 1]
 M apps/vos-cli/src/main.rs
M  README.md
MM SPEC.md
?? crates/vos-mcp/src/lib.rs
";
        let ctx = parse_status_porcelain(sample).unwrap();
        assert_eq!(ctx.branch, "experimental/recovery-predictor");
        assert!(ctx.is_dirty);
        assert_eq!(ctx.staged_count, 2);
        assert_eq!(ctx.modified_count, 2);
        assert_eq!(ctx.untracked_count, 1);
    }

    #[test]
    fn parses_clean_no_commits_branch_and_log_line() {
        let ctx = parse_status_porcelain("## No commits yet on main\n").unwrap();
        assert_eq!(ctx.branch, "main");
        assert!(!ctx.is_dirty);

        let (h, ts, subj) =
            parse_last_commit("e525004\t1795673200\tdocs(mcp): document vos-mcp crate").unwrap();
        assert_eq!(h, "e525004");
        assert_eq!(ts, 1795673200);
        assert_eq!(subj, "docs(mcp): document vos-mcp crate");
    }
}