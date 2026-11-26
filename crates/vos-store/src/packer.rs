//! Deterministic token-budgeted Context Compiler (`vos pack`, `v2.0`).

use std::collections::HashSet;

use vos_core::compact::compact_path;
use vos_core::{ContextPack, GraphDirection, PackedItem};

use crate::{SearchFilters, Store, StoreError};

/// Estimates LLM token count deterministically (`~4` bytes per token, minimum `1`).
pub fn estimate_tokens(text: &str) -> usize {
    text.len().div_ceil(4).max(1)
}

impl Store {
    /// Compiles a deterministic value-per-token context bundle strictly within `token_budget`.
    pub fn compile_context_pack(
        &self,
        project_name: &str,
        goal: &str,
        token_budget: usize,
    ) -> Result<ContextPack, StoreError> {
        let clean_goal = goal.trim();
        if clean_goal.is_empty() {
            return Err(StoreError::InvalidInput("goal must not be empty".to_string()));
        }
        let budget = token_budget.clamp(128, 128_000);

        let cont = self.project_continue(project_name)?;
        let root = cont.resume.project.root_path.as_deref();
        let goal_terms = tokenize_goal(clean_goal);

        let mut candidates: Vec<PackedItem> = Vec::new();
        let mut seen_ids = HashSet::new();

        // 1. Core orientation header (Git state + active/last session + handoff).
        let mut orient_lines = Vec::new();
        if let Some(git) = &cont.git {
            let dirty = if git.is_dirty {
                format!("dirty ({} changed)", git.changed_files.len())
            } else {
                "clean".to_string()
            };
            orient_lines.push(format!("Git: branch `{}` ({dirty})", git.branch));
            if let Some(subj) = &git.last_commit_subject {
                orient_lines.push(format!("Last commit: {subj}"));
            }
        }
        if let Some(handoff) = &cont.latest_handoff {
            let next = handoff.next_step.as_deref().unwrap_or("-");
            orient_lines.push(format!(
                "Latest handoff ({}): {} | Next: {next}",
                handoff.from_agent, handoff.summary
            ));
        } else if let Some(sess) = &cont.last_session
            && let Some(note) = sess.stop_note.as_deref().or(sess.start_note.as_deref())
        {
            orient_lines.push(format!("Last session note: {note}"));
        }
        if !cont.continue_from.is_empty() {
            let ret: Vec<String> = cont
                .continue_from
                .iter()
                .take(3)
                .map(|c| compact_path(&c.object.path, root))
                .collect();
            orient_lines.push(format!("Resume focus: {}", ret.join(", ")));
        }
        if !orient_lines.is_empty() {
            push_candidate(
                &mut candidates,
                &mut seen_ids,
                "session_git",
                "session_git",
                None,
                None,
                "Workspace & Session Pulse",
                &orient_lines.join("\n"),
                125.0,
            );
        }

        // 2. Active multi-agent leases (collision warnings).
        if !cont.active_leases.is_empty() {
            let lines: Vec<String> = cont
                .active_leases
                .iter()
                .map(|l| format!("- `{}` claimed by `{}`", l.area, l.agent_id))
                .collect();
            push_candidate(
                &mut candidates,
                &mut seen_ids,
                "leases",
                "lease",
                None,
                None,
                "Active Area Leases",
                &lines.join("\n"),
                105.0,
            );
        }

        // 3. Verified / Stale hash-anchored facts.
        for fact in self.list_facts(Some(project_name), false, 30)? {
            let path = fact.path.as_deref().map(|p| compact_path(p, root));
            let hay = format!(
                "{} {} {}",
                fact.fact_text,
                path.as_deref().unwrap_or(""),
                fact.symbol.as_deref().unwrap_or("")
            );
            let relevance = term_overlap_score(&hay, &goal_terms);
            let val = 64.0 + relevance * 36.0;
            let body = format!(
                "[{:?}]{} {}",
                fact.status,
                path.as_deref()
                    .map(|p| format!(" ({p})"))
                    .unwrap_or_default(),
                fact.fact_text
            );
            push_candidate(
                &mut candidates,
                &mut seen_ids,
                &format!("fact:{}", fact.id),
                "fact",
                path,
                fact.symbol,
                "Verified Fact",
                &body,
                val,
            );
        }

        // 4. Ranked AST Symbol slices matching goal.
        let sym_hits = self
            .search_symbols(clean_goal, Some(project_name), 10)
            .unwrap_or_default();
        let mut anchor_path: Option<String> = None;
        for (idx, sym) in sym_hits.into_iter().take(6).enumerate() {
            let rel = compact_path(&sym.path, root);
            if anchor_path.is_none() {
                anchor_path = Some(rel.clone());
            }
            let capped_end = sym.end_line.min(sym.start_line.saturating_add(70));
            let slice_body = self
                .slice_object(
                    &sym.path,
                    Some(project_name),
                    None,
                    Some(sym.start_line),
                    Some(capped_end),
                )
                .ok()
                .and_then(|s| s.content)
                .unwrap_or_else(|| sym.signature.clone());
            let body = format!(
                "// {}:{}..{} ({})\n{}",
                rel, sym.start_line, sym.end_line, sym.signature, slice_body
            );
            push_candidate(
                &mut candidates,
                &mut seen_ids,
                &format!("sym:{}:{}", rel, sym.name),
                "symbol",
                Some(rel),
                Some(sym.name.clone()),
                &format!("Symbol `{}`", sym.name),
                &body,
                96.0 - (idx as f64) * 5.0,
            );
        }

        // 5. Ranked FTS5 file/chunk matches.
        let search_hits = self
            .search_with_filters(
                Some(clean_goal),
                SearchFilters {
                    project: Some(project_name),
                    limit: 8,
                    ..SearchFilters::default()
                },
            )
            .unwrap_or_default();
        for (idx, hit) in search_hits.into_iter().take(6).enumerate() {
            let rel = compact_path(&hit.path, root);
            if anchor_path.is_none() {
                anchor_path = Some(rel.clone());
            }
            let body = format!("Path: `{rel}`\nExcerpt: {}", hit.snippet);
            push_candidate(
                &mut candidates,
                &mut seen_ids,
                &format!("search:{}", hit.object_id),
                "search",
                Some(rel.clone()),
                None,
                &format!("Match `{rel}`"),
                &body,
                74.0 - (idx as f64) * 4.0,
            );
        }

        // 6. Open Questions tied to goal or project.
        if !cont.open_questions.is_empty() {
            let mut qs: Vec<(f64, &String)> = cont
                .open_questions
                .iter()
                .map(|q| (term_overlap_score(q, &goal_terms), q))
                .collect();
            qs.sort_by(|a, b| b.0.total_cmp(&a.0));
            let top_qs: Vec<String> = qs
                .into_iter()
                .take(5)
                .map(|(_, q)| format!("- {q}"))
                .collect();
            push_candidate(
                &mut candidates,
                &mut seen_ids,
                "open_questions",
                "question",
                None,
                None,
                "Open Questions",
                &top_qs.join("\n"),
                68.0,
            );
        }

        // 7. 1-hop Graph & Co-Change neighbors for primary hit.
        if let Some(target) = anchor_path
            && let Ok(details) = self.show_object_scoped(&target, Some(project_name))
        {
            let mut nlines = Vec::new();
            if let Ok(edges) = self.object_relations(&details.object.id, GraphDirection::Both) {
                for edge in edges.into_iter().take(8) {
                    let from_r = compact_path(&edge.from_path, root);
                    let to_r = compact_path(&edge.to_path, root);
                    nlines.push(format!("- {from_r} -[{}]-> {to_r}", edge.relation_type.as_str()));
                }
            }
            if let Ok(arch) = self.project_archaeology(&target, Some(project_name), 6) {
                for partner in arch.co_change_partners.into_iter().take(4) {
                    nlines.push(format!(
                        "- co-change: `{}` ({} commits, ratio {:.2})",
                        partner.path, partner.co_commits, partner.coupling_ratio
                    ));
                }
            }
            if !nlines.is_empty() {
                push_candidate(
                    &mut candidates,
                    &mut seen_ids,
                    &format!("neighbors:{target}"),
                    "neighbors",
                    Some(target.clone()),
                    None,
                    &format!("Neighbors of `{target}`"),
                    &nlines.join("\n"),
                    56.0,
                );
            }
        }

        let items_considered = candidates.len();
        let header_reserve = 28usize;
        let effective_budget = budget.saturating_sub(header_reserve);

        // Sort strictly by value-per-token efficiency, breaking ties by raw value then shorter cost.
        candidates.sort_by(|a, b| {
            b.efficiency
                .total_cmp(&a.efficiency)
                .then_with(|| b.value.total_cmp(&a.value))
                .then_with(|| a.tokens.cmp(&b.tokens))
        });

        let mut selected = Vec::new();
        let mut used_tokens = header_reserve.min(budget);

        // Anchor workspace orientation if it fits within 45% of total budget.
        if let Some(pos) = candidates.iter().position(|c| c.kind == "session_git") {
            let anchor_cost = candidates[pos].tokens;
            if anchor_cost <= budget * 45 / 100 && used_tokens + anchor_cost <= budget {
                used_tokens += anchor_cost;
                selected.push(candidates.remove(pos));
            }
        }

        for item in candidates {
            if used_tokens + item.tokens <= effective_budget + header_reserve {
                used_tokens += item.tokens;
                selected.push(item);
            }
        }

        selected.sort_by_key(|i| match i.kind.as_str() {
            "session_git" => 0,
            "lease" => 1,
            "fact" => 2,
            "question" => 3,
            "symbol" => 4,
            "search" => 5,
            _ => 6,
        });

        let items_selected = selected.len();
        let omitted_count = items_considered.saturating_sub(items_selected);
        let utilization_ratio = ((used_tokens as f64 / budget as f64) * 1000.0).round() / 1000.0;

        let mut md = format!(
            "# VOS Context Pack: `{}`\nGoal: {}\nBudget: {} / {} tokens ({} item(s) packed, {} omitted)\n",
            cont.resume.project.name, clean_goal, used_tokens, budget, items_selected, omitted_count
        );
        for item in &selected {
            md.push_str(&format!("\n## {} [{}t]\n{}\n", item.title, item.tokens, item.content));
        }

        Ok(ContextPack {
            project_name: cont.resume.project.name,
            goal: clean_goal.to_string(),
            token_budget: budget,
            used_tokens,
            utilization_ratio,
            items_considered,
            items_selected,
            omitted_count,
            items: selected,
            rendered_markdown: md,
        })
    }
}

#[allow(clippy::too_many_arguments)]
fn push_candidate(
    out: &mut Vec<PackedItem>,
    seen_ids: &mut HashSet<String>,
    id: &str,
    kind: &str,
    path: Option<String>,
    symbol: Option<String>,
    title: &str,
    content: &str,
    value: f64,
) {
    if content.trim().is_empty() || !seen_ids.insert(id.to_string()) {
        return;
    }
    let tokens = estimate_tokens(title) + estimate_tokens(content) + 4;
    let efficiency = (value / tokens as f64 * 1000.0).round() / 1000.0;
    out.push(PackedItem {
        id: id.to_string(),
        kind: kind.to_string(),
        path,
        symbol,
        title: title.to_string(),
        content: content.to_string(),
        tokens,
        value,
        efficiency,
    });
}

fn tokenize_goal(goal: &str) -> Vec<String> {
    goal.split(|c: char| !c.is_alphanumeric() && c != '_')
        .map(str::to_lowercase)
        .filter(|w| w.len() >= 3)
        .collect()
}

fn term_overlap_score(haystack: &str, terms: &[String]) -> f64 {
    if terms.is_empty() {
        return 0.0;
    }
    let lower = haystack.to_lowercase();
    let hits = terms.iter().filter(|t| lower.contains(t.as_str())).count();
    hits as f64 / terms.len() as f64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{UpsertObjectInput};
    use tempfile::TempDir;
    use vos_core::{ObjectKind, SymbolInput, SymbolKind};

    #[test]
    fn context_compiler_packs_high_value_slices_strictly_within_budget() {
        let tmp = TempDir::new().unwrap();
        let root = vos_core::util::canonicalize_path(tmp.path()).unwrap();
        let src_dir = std::path::Path::new(&root).join("src");
        std::fs::create_dir_all(&src_dir).unwrap();
        let file = src_dir.join("router.rs");
        let code = "pub fn route_request(path: &str) -> bool {\n    path.starts_with(\"/api\")\n}\n";
        std::fs::write(&file, code).unwrap();

        let mut store = Store::open_in_memory().unwrap();
        let proj = store.create_project("web", Some(std::path::Path::new(&root)), None).unwrap();
        let mut batch = store.begin_index_batch().unwrap();
        let outcome = batch
            .upsert_object(UpsertObjectInput {
                project_id: Some(proj.id.clone()),
                path: file.to_string_lossy().into_owned(),
                kind: ObjectKind::Code,
                title: "router.rs".into(),
                summary: Some("HTTP router".into()),
                content_hash: "r1".into(),
                size_bytes: code.len() as i64,
                modified_at: 100,
                is_noise: false,
                chunks: vec![code.to_string()],
            })
            .unwrap();
        batch
            .sync_symbols(
                outcome.object_id(),
                &[SymbolInput {
                    name: "route_request".into(),
                    kind: SymbolKind::Fn,
                    signature: "pub fn route_request(path: &str) -> bool".into(),
                    start_line: 1,
                    end_line: 3,
                }],
                true,
            )
            .unwrap();
        batch.commit().unwrap();
        store
            .add_fact(Some("web"), "All /api routes require auth header", Some("src/router.rs"), Some("route_request"), None)
            .unwrap();

        let pack = store.compile_context_pack("web", "route_request api", 1500).unwrap();
        assert!(pack.used_tokens <= 1500);
        assert!(pack.items_selected >= 2);
        assert!(pack.rendered_markdown.contains("route_request"));

        let tiny = store.compile_context_pack("web", "route_request api", 200).unwrap();
        assert!(tiny.used_tokens <= 200);
    }
}
