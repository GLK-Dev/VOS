//! Causal Git commit archaeology and temporal co-change coupling analysis.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::process::Command;

use rusqlite::params;
use vos_core::{ArchaeologyReport, CoChangePartner, CommitTouch, ObjectKind, RelationType, WorkSession};

use crate::{Store, StoreError};

const GIT_LOG_SAMPLE_LIMIT: &str = "250";

struct RawCommit {
    hash: String,
    timestamp: i64,
    author: String,
    subject: String,
    files: Vec<String>,
}

impl Store {
    /// Combines Git commit history, temporal co-change coupling, doc references, and work sessions for `target`.
    pub fn project_archaeology(
        &self,
        target: &str,
        project_name: Option<&str>,
        limit: usize,
    ) -> Result<ArchaeologyReport, StoreError> {
        let why = self.why_object(target, project_name)?;
        let cap = limit.clamp(1, 50);
        let root_prefix = why.project_root.as_deref().unwrap_or("");
        let target_rel = normalize_rel(&why.object.path, root_prefix);

        let mut static_paths: HashSet<String> = HashSet::new();
        let mut doc_references: Vec<String> = Vec::new();
        for edge in &why.outgoing_relations {
            static_paths.insert(normalize_rel(&edge.to_path, root_prefix));
        }
        for edge in &why.incoming_relations {
            let rel = normalize_rel(&edge.from_path, root_prefix);
            if matches!(edge.relation_type, RelationType::MdLink | RelationType::PathRef)
                && !doc_references.contains(&rel)
            {
                doc_references.push(rel.clone());
            }
            static_paths.insert(rel);
        }

        let mut active_kind_by_rel: HashMap<String, ObjectKind> = HashMap::new();
        for active in self.list_active_object_paths(why.object.project_id.as_deref())? {
            let rel = normalize_rel(&active.path, root_prefix);
            let kind = self.lookup_object_kind(&active.id).unwrap_or(ObjectKind::Other);
            active_kind_by_rel.insert(rel, kind);
        }

        let (total_commits_sampled, commits, co_change_partners) =
            if !root_prefix.is_empty() && has_git_repo(Path::new(root_prefix)) {
                collect_git_archaeology(
                    Path::new(root_prefix),
                    &target_rel,
                    &active_kind_by_rel,
                    &static_paths,
                    cap,
                )
            } else {
                (0, Vec::new(), Vec::new())
            };

        let related_sessions = self.find_related_sessions(
            why.object.project_id.as_deref(),
            &target_rel,
            &why.object.title,
            cap,
        )?;

        Ok(ArchaeologyReport {
            path: target_rel,
            title: why.object.title,
            project_name: why.project_name,
            total_commits_sampled,
            commits,
            co_change_partners,
            doc_references,
            related_sessions,
        })
    }

    fn lookup_object_kind(&self, object_id: &str) -> Result<ObjectKind, StoreError> {
        let raw: String = self.conn.query_row(
            "SELECT kind FROM objects WHERE id = ?1",
            params![object_id],
            |row| row.get(0),
        )?;
        Ok(ObjectKind::from_db(&raw))
    }

    fn find_related_sessions(
        &self,
        project_id: Option<&str>,
        target_rel: &str,
        title: &str,
        limit: usize,
    ) -> Result<Vec<WorkSession>, StoreError> {
        let Some(pid) = project_id else {
            return Ok(Vec::new());
        };
        let proj_name: String = self.conn.query_row(
            "SELECT name FROM projects WHERE id = ?1",
            params![pid],
            |row| row.get(0),
        )?;
        let sessions = self.list_sessions(Some(&proj_name), 30)?;
        let stem = Path::new(title)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or(title)
            .to_ascii_lowercase();
        let rel_lower = target_rel.to_ascii_lowercase();

        let matched: Vec<WorkSession> = sessions
            .into_iter()
            .filter(|s| {
                let rp = s.return_point.as_deref().unwrap_or("").replace('\\', "/").to_ascii_lowercase();
                let sn = s.start_note.as_deref().unwrap_or("").to_ascii_lowercase();
                let stn = s.stop_note.as_deref().unwrap_or("").to_ascii_lowercase();
                rp == rel_lower
                    || (!stem.is_empty() && stem.len() >= 3 && (sn.contains(&stem) || stn.contains(&stem)))
                    || sn.contains(&rel_lower)
                    || stn.contains(&rel_lower)
            })
            .take(limit)
            .collect();
        Ok(matched)
    }
}

fn has_git_repo(start: &Path) -> bool {
    let mut cur = Some(start);
    while let Some(dir) = cur {
        if dir.join(".git").exists() {
            return true;
        }
        cur = dir.parent();
    }
    false
}

fn normalize_rel(full_path: &str, root_prefix: &str) -> String {
    vos_core::compact::compact_path(full_path, (!root_prefix.is_empty()).then_some(root_prefix))
}

fn collect_git_archaeology(
    root: &Path,
    target_rel: &str,
    active_kinds: &HashMap<String, ObjectKind>,
    static_paths: &HashSet<String>,
    limit: usize,
) -> (usize, Vec<CommitTouch>, Vec<CoChangePartner>) {
    let Ok(out) = Command::new("git")
        .arg("-C")
        .arg(root)
        .args([
            "log",
            "-n",
            GIT_LOG_SAMPLE_LIMIT,
            "--name-only",
            "--format=COMMIT:%h%x09%ct%x09%an%x09%s",
        ])
        .output()
    else {
        return (0, Vec::new(), Vec::new());
    };
    if !out.status.success() {
        return (0, Vec::new(), Vec::new());
    }

    let raw = String::from_utf8_lossy(&out.stdout);
    let parsed = parse_git_log_commits(&raw);
    build_archaeology_from_commits(&parsed, target_rel, active_kinds, static_paths, limit)
}

fn parse_git_log_commits(raw: &str) -> Vec<RawCommit> {
    let mut commits = Vec::new();
    let mut current: Option<RawCommit> = None;

    for line in raw.lines() {
        let trimmed = line.trim();
        if let Some(meta) = trimmed.strip_prefix("COMMIT:") {
            if let Some(prev) = current.take() {
                commits.push(prev);
            }
            let mut parts = meta.splitn(4, '\t');
            let hash = parts.next().unwrap_or("").trim().to_string();
            let timestamp = parts.next().and_then(|s| s.trim().parse().ok()).unwrap_or(0);
            let author = parts.next().unwrap_or("").trim().to_string();
            let subject = parts.next().unwrap_or("").trim().to_string();
            current = Some(RawCommit {
                hash,
                timestamp,
                author,
                subject,
                files: Vec::new(),
            });
        } else if !trimmed.is_empty()
            && let Some(ref mut cur) = current
        {
            cur.files.push(trimmed.replace('\\', "/"));
        }
    }
    if let Some(last) = current {
        commits.push(last);
    }
    commits
}

fn build_archaeology_from_commits(
    parsed: &[RawCommit],
    target_rel: &str,
    active_kinds: &HashMap<String, ObjectKind>,
    static_paths: &HashSet<String>,
    limit: usize,
) -> (usize, Vec<CommitTouch>, Vec<CoChangePartner>) {
    let target_norm = target_rel.replace('\\', "/");
    let mut matching_commits = Vec::new();
    let mut co_counts: HashMap<String, usize> = HashMap::new();
    let mut target_commit_count = 0_usize;

    for c in parsed {
        if !c.files.iter().any(|f| f.eq_ignore_ascii_case(&target_norm)) {
            continue;
        }
        target_commit_count += 1;
        if matching_commits.len() < limit {
            matching_commits.push(CommitTouch {
                hash: c.hash.clone(),
                timestamp: c.timestamp,
                author: c.author.clone(),
                subject: c.subject.clone(),
            });
        }
        for peer in &c.files {
            if peer.eq_ignore_ascii_case(&target_norm) {
                continue;
            }
            if active_kinds.contains_key(peer) {
                *co_counts.entry(peer.clone()).or_insert(0) += 1;
            }
        }
    }

    let denom = target_commit_count.max(1) as f64;
    let mut partners: Vec<CoChangePartner> = co_counts
        .into_iter()
        .map(|(path, co_commits)| {
            let kind = active_kinds.get(&path).copied().unwrap_or(ObjectKind::Other);
            let has_static_relation = static_paths.contains(&path);
            let ratio = ((co_commits as f64 / denom) * 100.0).round() / 100.0;
            CoChangePartner {
                path,
                kind,
                co_commits,
                target_commits: target_commit_count,
                coupling_ratio: ratio,
                has_static_relation,
            }
        })
        .collect();

    partners.sort_by(|a, b| {
        b.co_commits
            .cmp(&a.co_commits)
            .then_with(|| a.has_static_relation.cmp(&b.has_static_relation))
            .then_with(|| a.path.cmp(&b.path))
    });
    partners.truncate(limit);

    (parsed.len(), matching_commits, partners)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_commits_and_ranks_hidden_temporal_co_change_partners() {
        let log = "\
COMMIT:abc1234\t1700000200\talice\tfeat: sync search and migration
crates/vos-store/src/search.rs
migrations/0001_init.sql
tests/search_live.yaml
COMMIT:def5678\t1700000100\talice\tfix: refine penalty weights
crates/vos-store/src/search.rs
tests/search_live.yaml
COMMIT:ghi9012\t1700000000\tbob\tdocs: update README
README.md
";
        let commits = parse_git_log_commits(log);
        assert_eq!(commits.len(), 3);

        let mut active = HashMap::new();
        active.insert("crates/vos-store/src/search.rs".into(), ObjectKind::Code);
        active.insert("migrations/0001_init.sql".into(), ObjectKind::Config);
        active.insert("tests/search_live.yaml".into(), ObjectKind::Config);
        active.insert("README.md".into(), ObjectKind::Doc);

        let mut static_paths = HashSet::new();
        static_paths.insert("migrations/0001_init.sql".into());

        let (sampled, touched, partners) = build_archaeology_from_commits(
            &commits,
            "crates/vos-store/src/search.rs",
            &active,
            &static_paths,
            10,
        );
        assert_eq!(sampled, 3);
        assert_eq!(touched.len(), 2);
        assert_eq!(touched[0].hash, "abc1234");
        assert_eq!(partners[0].path, "tests/search_live.yaml");
        assert_eq!(partners[0].co_commits, 2);
        assert_eq!(partners[0].coupling_ratio, 1.0);
        assert!(!partners[0].has_static_relation);
        assert!(partners[1].has_static_relation);
    }
}