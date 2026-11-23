//! Deterministic directory walker, text extractor, and idempotent indexer for VOS.

use std::collections::HashSet;
use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::time::UNIX_EPOCH;

use ignore::WalkBuilder;
use thiserror::Error;
use tracing::{info, warn};
use vos_core::VosError;
use vos_core::util::{canonicalize_path, classify_path, fingerprint_file};
use vos_store::{Store, StoreError, UpsertObjectInput, UpsertOutcome};

const MAX_TEXT_BYTES: u64 = 2 * 1024 * 1024;
const CHUNK_CHAR_LIMIT: usize = 1000;
const IGNORED_DIRS: &[&str] = &[
    ".git",
    ".cargo",
    "target",
    "node_modules",
    ".venv",
    "venv",
    "__pycache__",
    "dist",
    ".vos",
    ".idea",
    ".vscode",
];

/// Errors returned by the indexing pipeline.
#[derive(Debug, Error)]
pub enum IndexerError {
    #[error("domain error: {0}")]
    Core(#[from] VosError),

    #[error("store error: {0}")]
    Store(#[from] StoreError),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

/// Options controlling directory traversal during `index_path`.
#[derive(Debug, Default, Clone, Copy)]
pub struct IndexOptions {
    /// When true, ignores `.gitignore` / `.vosignore` rules (hard-ignored dirs still apply).
    pub no_ignore: bool,
}

/// Summary counters produced by `index_path`.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct IndexReport {
    pub scanned: usize,
    pub added: usize,
    pub modified: usize,
    pub restored: usize,
    pub unchanged: usize,
    pub deleted: usize,
    pub skipped: usize,
}

/// Scans `root` with default options and idempotently synchronizes into `store`.
pub fn index_path(
    store: &mut Store,
    root: &Path,
    project_name: Option<&str>,
) -> Result<IndexReport, IndexerError> {
    index_path_with_options(store, root, project_name, IndexOptions::default())
}

/// Scans `root` and idempotently synchronizes objects, chunks, and deletions into `store`.
pub fn index_path_with_options(
    store: &mut Store,
    root: &Path,
    project_name: Option<&str>,
    options: IndexOptions,
) -> Result<IndexReport, IndexerError> {
    let canonical_root = canonicalize_path(root)?;
    let explicit_project_id = match project_name {
        Some(name) => {
            let project = store
                .get_project_by_name(name)?
                .ok_or_else(|| StoreError::NotFound("project", name.to_string()))?;
            Some(project.id)
        }
        None => None,
    };

    info!(root = %canonical_root, project = ?project_name, no_ignore = options.no_ignore, "starting indexing run");

    let mut report = IndexReport::default();
    let mut seen_paths = HashSet::new();

    let mut builder = WalkBuilder::new(&canonical_root);
    builder
        .hidden(false)
        .follow_links(false)
        .parents(!options.no_ignore)
        .ignore(!options.no_ignore)
        .git_ignore(!options.no_ignore)
        .git_global(!options.no_ignore)
        .git_exclude(!options.no_ignore)
        .require_git(false);

    if !options.no_ignore {
        builder.add_custom_ignore_filename(".vosignore");
    }

    builder.filter_entry(|entry| {
        let is_dir = entry.file_type().is_some_and(|ft| ft.is_dir());
        if !is_dir {
            return true;
        }
        entry
            .file_name()
            .to_str()
            .is_none_or(|name| !IGNORED_DIRS.contains(&name))
    });

    for entry_result in builder.build() {
        let entry = match entry_result {
            Ok(e) => e,
            Err(err) => {
                warn!(error = %err, "skipping unreadable directory entry");
                report.skipped += 1;
                continue;
            }
        };

        if !entry.file_type().is_some_and(|ft| ft.is_file()) {
            continue;
        }

        report.scanned += 1;
        let path = entry.path();

        let canonical_file = match canonicalize_path(path) {
            Ok(p) => p,
            Err(err) => {
                warn!(path = %path.display(), error = %err, "failed to canonicalize path");
                report.skipped += 1;
                continue;
            }
        };

        let metadata = match entry.metadata() {
            Ok(m) => m,
            Err(err) => {
                warn!(path = %canonical_file, error = %err, "failed to read metadata");
                report.skipped += 1;
                continue;
            }
        };

        let size_bytes = metadata.len();
        let modified_at = metadata
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);

        let content_hash = match fingerprint_file(path, size_bytes) {
            Ok(h) => h,
            Err(err) => {
                warn!(path = %canonical_file, error = %err, "failed to fingerprint file");
                report.skipped += 1;
                continue;
            }
        };

        seen_paths.insert(canonical_file.clone());

        let kind = classify_path(path);
        let title = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| canonical_file.clone());

        let (summary, chunks) = if kind.is_text_indexable() && size_bytes <= MAX_TEXT_BYTES {
            match read_utf8_text(path) {
                Some(text) => {
                    let sum = extract_summary(&text);
                    let ch = chunk_text(&text, CHUNK_CHAR_LIMIT);
                    (sum, ch)
                }
                None => (None, Vec::new()),
            }
        } else {
            (None, Vec::new())
        };

        let project_id = match explicit_project_id {
            Some(ref pid) => Some(pid.clone()),
            None => store.find_project_for_path(&canonical_file)?.map(|p| p.id),
        };

        let outcome = store.upsert_object(UpsertObjectInput {
            project_id,
            path: canonical_file,
            kind,
            title,
            summary,
            content_hash,
            size_bytes: size_bytes as i64,
            modified_at,
            chunks,
        })?;

        match outcome {
            UpsertOutcome::Added(_) => report.added += 1,
            UpsertOutcome::Modified(_) => report.modified += 1,
            UpsertOutcome::Restored(_) => report.restored += 1,
            UpsertOutcome::Unchanged(_) => report.unchanged += 1,
        }
    }

    report.deleted = store.mark_missing_deleted(
        &canonical_root,
        explicit_project_id.as_deref(),
        &seen_paths,
    )?;

    info!(
        scanned = report.scanned,
        added = report.added,
        modified = report.modified,
        restored = report.restored,
        unchanged = report.unchanged,
        deleted = report.deleted,
        skipped = report.skipped,
        "completed indexing run"
    );

    Ok(report)
}

fn read_utf8_text(path: &Path) -> Option<String> {
    let mut file = File::open(path).ok()?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).ok()?;
    if bytes.contains(&0) {
        return None;
    }
    String::from_utf8(bytes).ok()
}

fn extract_summary(text: &str) -> Option<String> {
    let mut out = String::new();
    let mut lines_taken = 0;

    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(trimmed);
        lines_taken += 1;

        if lines_taken >= 8 || out.chars().count() >= 400 {
            break;
        }
    }

    if out.is_empty() {
        None
    } else if out.chars().count() > 400 {
        let truncated: String = out.chars().take(400).collect();
        Some(format!("{truncated}..."))
    } else {
        Some(out)
    }
}

/// Splits text into UTF-8-safe chunks, preferring line boundaries.
pub fn chunk_text(text: &str, max_chars: usize) -> Vec<String> {
    if text.trim().is_empty() {
        return Vec::new();
    }

    let limit = max_chars.max(100);
    let mut chunks = Vec::new();
    let mut current = String::new();
    let mut current_chars = 0_usize;

    for line in text.lines() {
        let line_chars = line.chars().count();
        if line_chars > limit {
            if !current.trim().is_empty() {
                chunks.push(std::mem::take(&mut current));
                current_chars = 0;
            }
            let mut slice_buf = String::new();
            let mut slice_len = 0_usize;
            for ch in line.chars() {
                slice_buf.push(ch);
                slice_len += 1;
                if slice_len >= limit {
                    chunks.push(std::mem::take(&mut slice_buf));
                    slice_len = 0;
                }
            }
            if !slice_buf.trim().is_empty() {
                chunks.push(slice_buf);
            }
            continue;
        }

        let needed = line_chars + usize::from(!current.is_empty());
        if current_chars + needed > limit && !current.trim().is_empty() {
            chunks.push(std::mem::take(&mut current));
            current_chars = 0;
        }

        if !current.is_empty() {
            current.push('\n');
            current_chars += 1;
        }
        current.push_str(line);
        current_chars += line_chars;
    }

    if !current.trim().is_empty() {
        chunks.push(current);
    }

    chunks
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn indexing_is_idempotent_and_tracks_changes_and_deletions() {
        let tmp = TempDir::new().unwrap();
        let proj_dir = tmp.path().join("demo");
        fs::create_dir_all(proj_dir.join("src")).unwrap();
        fs::create_dir_all(proj_dir.join("target")).unwrap();

        fs::write(
            proj_dir.join("src/scheduler.rs"),
            "pub fn schedule_task() {\n    // round robin scheduler\n}\n",
        )
        .unwrap();
        fs::write(
            proj_dir.join("notes.md"),
            "# Architecture\nWait-free scheduler notes.\n",
        )
        .unwrap();
        // Should be ignored because it sits inside `target/`
        fs::write(proj_dir.join("target/ignored.rs"), "fn ignored() {}").unwrap();

        let mut store = Store::open_in_memory().unwrap();
        store
            .create_project("demo", Some(&proj_dir), Some("Demo project"))
            .unwrap();

        let first = index_path(&mut store, &proj_dir, Some("demo")).unwrap();
        assert_eq!(first.scanned, 2);
        assert_eq!(first.added, 2);
        assert_eq!(first.deleted, 0);

        let second = index_path(&mut store, &proj_dir, Some("demo")).unwrap();
        assert_eq!(second.scanned, 2);
        assert_eq!(second.unchanged, 2);
        assert_eq!(second.added, 0);

        let hits = store.search("round robin", Some("demo"), 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].title, "scheduler.rs");

        fs::write(
            proj_dir.join("src/scheduler.rs"),
            "pub fn schedule_task() {\n    // priority queue scheduler\n}\n",
        )
        .unwrap();
        fs::remove_file(proj_dir.join("notes.md")).unwrap();

        let third = index_path(&mut store, &proj_dir, Some("demo")).unwrap();
        assert_eq!(third.scanned, 1);
        assert_eq!(third.modified, 1);
        assert_eq!(third.deleted, 1);

        assert!(store.search("Wait-free", Some("demo"), 10).unwrap().is_empty());
        assert_eq!(
            store.search("priority queue", Some("demo"), 10).unwrap().len(),
            1
        );

        let ctx = store.project_context("demo", 10).unwrap();
        assert_eq!(ctx.active_objects, 1);
        assert_eq!(ctx.deleted_objects, 1);
        assert_eq!(ctx.recent_events.len(), 4);
    }

    #[test]
    fn indexer_respects_vosignore_and_no_ignore_flag() {
        let tmp = TempDir::new().unwrap();
        let proj_dir = tmp.path().join("ign");
        fs::create_dir_all(proj_dir.join("out")).unwrap();
        fs::write(proj_dir.join(".vosignore"), "out/\n").unwrap();
        fs::write(proj_dir.join("main.rs"), "fn main() {}").unwrap();
        fs::write(proj_dir.join("out/build.bin"), "binary").unwrap();

        let mut store = Store::open_in_memory().unwrap();
        store.create_project("ign", Some(&proj_dir), None).unwrap();

        let r1 = index_path(&mut store, &proj_dir, Some("ign")).unwrap();
        assert_eq!(r1.scanned, 2); // .vosignore + main.rs

        let r2 = index_path_with_options(
            &mut store,
            &proj_dir,
            Some("ign"),
            IndexOptions { no_ignore: true },
        )
        .unwrap();
        assert_eq!(r2.scanned, 3);
        assert_eq!(r2.added, 1);
    }
}
