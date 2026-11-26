//! Foreground filesystem watch loop triggering idempotent re-indexing on stat changes.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::Path;
use std::thread;
use std::time::{Duration, UNIX_EPOCH};

use vos_core::util::canonicalize_path;
use vos_store::Store;

use crate::{IndexOptions, IndexReport, IndexerError, index_path_with_options, walk_project_files};

/// Configuration for `watch_path_with_options`.
#[derive(Debug, Clone, Copy)]
pub struct WatchOptions {
    /// Poll interval between deterministic stat scans.
    pub interval: Duration,
    /// When true, includes files normally ignored by `.gitignore` / `.vosignore`.
    pub no_ignore: bool,
    /// Optional iteration cap (used in deterministic tests; `None` runs until interrupted).
    pub max_iterations: Option<usize>,
}

impl Default for WatchOptions {
    fn default() -> Self {
        Self {
            interval: Duration::from_millis(1500),
            no_ignore: false,
            max_iterations: None,
        }
    }
}

/// Computes a lightweight metadata digest `(path, len, modified_nanos)` across non-ignored files.
pub fn compute_tree_signature(canonical_root: &str, no_ignore: bool) -> Result<u64, IndexerError> {
    let mut entries: Vec<(String, u64, u128)> = Vec::new();
    for entry_result in walk_project_files(canonical_root, no_ignore) {
        let Ok(entry) = entry_result else {
            continue;
        };
        if !entry.file_type().is_some_and(|ft| ft.is_file()) {
            continue;
        }
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        let nanos = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        entries.push((
            entry.path().to_string_lossy().into_owned(),
            meta.len(),
            nanos,
        ));
    }
    entries.sort_by(|a, b| a.0.cmp(&b.0));

    let mut hasher = DefaultHasher::new();
    entries.len().hash(&mut hasher);
    for (p, len, nanos) in entries {
        p.hash(&mut hasher);
        len.hash(&mut hasher);
        nanos.hash(&mut hasher);
    }
    Ok(hasher.finish())
}

/// Watches `root` in the foreground, re-indexing idempotently whenever metadata changes.
///
/// Returns the number of indexing runs executed.
pub fn watch_path_with_options<F>(
    store: &mut Store,
    root: &Path,
    project_name: Option<&str>,
    options: WatchOptions,
    mut on_sync: F,
) -> Result<usize, IndexerError>
where
    F: FnMut(&IndexReport),
{
    let canonical_root = canonicalize_path(root)?;
    let idx_opts = IndexOptions {
        no_ignore: options.no_ignore,
    };

    let initial = index_path_with_options(store, root, project_name, idx_opts)?;
    let mut last_sig = compute_tree_signature(&canonical_root, options.no_ignore)?;
    on_sync(&initial);
    let mut sync_count = 1_usize;

    let mut cycle = 0_usize;
    loop {
        if let Some(limit) = options.max_iterations
            && cycle >= limit
        {
            break;
        }
        cycle += 1;
        thread::sleep(options.interval);

        let sig = compute_tree_signature(&canonical_root, options.no_ignore)?;
        if sig != last_sig {
            let report = index_path_with_options(store, root, project_name, idx_opts)?;
            last_sig = sig;
            on_sync(&report);
            sync_count += 1;
        }
    }

    Ok(sync_count)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use tempfile::TempDir;

    use super::*;

    #[test]
    fn watch_triggers_reindex_only_when_tree_changes() {
        let tmp = TempDir::new().unwrap();
        let proj = tmp.path().join("watched");
        fs::create_dir_all(&proj).unwrap();
        fs::write(proj.join("README.md"), "# Watched\n").unwrap();

        let mut store = Store::open_in_memory().unwrap();
        store.create_project("watched", Some(&proj), None).unwrap();

        let mut reports = Vec::new();
        let mut added_second = false;
        let syncs = watch_path_with_options(
            &mut store,
            &proj,
            Some("watched"),
            WatchOptions {
                interval: Duration::from_millis(15),
                no_ignore: false,
                max_iterations: Some(3),
            },
            |rep| {
                reports.push((rep.added, rep.unchanged));
                if !added_second {
                    added_second = true;
                    fs::write(proj.join("extra.rs"), "pub fn extra() {}\n").unwrap();
                }
            },
        )
        .unwrap();

        assert_eq!(syncs, 2);
        assert_eq!(reports, vec![(1, 0), (1, 1)]);
    }
}