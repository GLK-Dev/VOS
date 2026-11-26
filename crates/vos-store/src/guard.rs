//! Pre-flight blast simulation, affected test detection, and architectural guardrails (`v1.5`).

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};

use vos_core::compact::compact_path;
use vos_core::{
    AffectedTest, AffectedTestsReport, AreaLease, CoChangePartner, GraphDirection, GuardReport,
    GuardViolation, ObjectKind, PreflightReport, Project, ProjectFact, RelationType,
};

use crate::{ActiveObjectRef, Store, StoreError};

/// Single architectural boundary rule parsed from `vos.rules.toml`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GuardRule {
    pub name: String,
    pub from_prefixes: Vec<String>,
    pub forbid_to_prefixes: Vec<String>,
    pub forbid_imports: Vec<String>,
    pub reason: Option<String>,
}

impl Store {
    /// Checks declarative architectural guardrail rules (`vos.rules.toml`) for a project.
    pub fn check_guard_rules(
        &self,
        project_name: &str,
        rules_path: Option<&Path>,
    ) -> Result<GuardReport, StoreError> {
        let project = self
            .get_project_by_name(project_name)?
            .ok_or_else(|| StoreError::NotFound("project", project_name.to_string()))?;

        let resolved_rules = resolve_rules_path(&project, rules_path)?;
        let rules_raw = std::fs::read_to_string(&resolved_rules).map_err(|err| {
            if err.kind() == std::io::ErrorKind::NotFound {
                StoreError::NotFound("guard rules", resolved_rules.display().to_string())
            } else {
                StoreError::Io(err)
            }
        })?;
        let rules = parse_vos_rules_toml(&rules_raw)?;

        let root = project.root_path.as_deref();
        let active = self.list_active_object_paths(Some(&project.id))?;
        let mut violations = Vec::new();
        let mut seen_violations = HashSet::new();

        for rule in &rules {
            for obj in &active {
                let rel_from = normalize_rel(&compact_path(&obj.path, root));
                if !matches_any_prefix(&rel_from, &rule.from_prefixes) {
                    continue;
                }

                if !rule.forbid_to_prefixes.is_empty() {
                    for edge in self.object_relations(&obj.id, GraphDirection::Outgoing)? {
                        if !matches!(
                            edge.relation_type,
                            RelationType::RustMod | RelationType::RustUse | RelationType::CodeImport
                        ) {
                            continue;
                        }
                        let rel_to = normalize_rel(&compact_path(&edge.to_path, root));
                        if matches_any_prefix(&rel_to, &rule.forbid_to_prefixes) {
                            let desc = format!("{} -> {}", edge.relation_type.as_str(), rel_to);
                            let key = (rule.name.clone(), rel_from.clone(), "edge", desc.clone());
                            if seen_violations.insert(key) {
                                violations.push(GuardViolation {
                                    rule: rule.name.clone(),
                                    from_path: rel_from.clone(),
                                    kind: "edge".to_string(),
                                    violation: desc,
                                    reason: rule.reason.clone(),
                                });
                            }
                        }
                    }
                }

                if !rule.forbid_imports.is_empty()
                    && obj.kind == ObjectKind::Code
                    && let Ok(source) = std::fs::read_to_string(&obj.path)
                {
                    for stmt in extract_import_statements(&source) {
                        for pat in &rule.forbid_imports {
                            if import_matches_forbidden(&stmt, pat) {
                                let key = (rule.name.clone(), rel_from.clone(), "import", pat.clone());
                                if seen_violations.insert(key) {
                                    violations.push(GuardViolation {
                                        rule: rule.name.clone(),
                                        from_path: rel_from.clone(),
                                        kind: "import".to_string(),
                                        violation: format!("forbidden import `{}` ({})", pat, stmt),
                                        reason: rule.reason.clone(),
                                    });
                                }
                            }
                        }
                    }
                }
            }
        }

        violations.sort_by(|a, b| {
            a.rule
                .cmp(&b.rule)
                .then_with(|| a.from_path.cmp(&b.from_path))
                .then_with(|| a.violation.cmp(&b.violation))
        });

        Ok(GuardReport {
            project_name: project.name,
            rules_path: normalize_rel(&compact_path(&resolved_rules.to_string_lossy(), root)),
            rules_checked: rules.len(),
            passed: violations.is_empty(),
            violations,
        })
    }

    /// Detects tests impacted by modifying `targets` (or dirty/recent project files when omitted).
    pub fn affected_tests(
        &self,
        project_name: &str,
        targets: &[String],
    ) -> Result<AffectedTestsReport, StoreError> {
        let project = self
            .get_project_by_name(project_name)?
            .ok_or_else(|| StoreError::NotFound("project", project_name.to_string()))?;
        let active = self.list_active_object_paths(Some(&project.id))?;
        let resolved_targets = self.resolve_preflight_targets(&project, &active, targets)?;
        let blast = self.compute_reverse_blast(&project, &active, &resolved_targets)?;
        let (affected_tests, suggested_commands) =
            collect_affected_tests(&project, &active, &resolved_targets, &blast);

        Ok(AffectedTestsReport {
            project_name: project.name,
            targets: resolved_targets.into_iter().map(|t| t.rel_path).collect(),
            affected_tests,
            suggested_commands,
        })
    }

    /// Runs a unified pre-edit briefing combining blast radius, co-changes, facts, leases, and tests.
    pub fn preflight(
        &self,
        project_name: &str,
        targets: &[String],
    ) -> Result<PreflightReport, StoreError> {
        let project = self
            .get_project_by_name(project_name)?
            .ok_or_else(|| StoreError::NotFound("project", project_name.to_string()))?;
        let active = self.list_active_object_paths(Some(&project.id))?;
        let resolved_targets = self.resolve_preflight_targets(&project, &active, targets)?;
        let blast = self.compute_reverse_blast(&project, &active, &resolved_targets)?;

        let mut direct_dependents = Vec::new();
        let mut transitive_dependents = Vec::new();
        let mut dep_entries: Vec<(usize, String)> = blast
            .distances
            .iter()
            .filter_map(|(id, dist)| {
                if *dist == 0 {
                    return None;
                }
                blast.rel_by_id.get(id).map(|p| (*dist, p.clone()))
            })
            .collect();
        dep_entries.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));

        for (dist, rel) in dep_entries {
            if dist == 1 {
                direct_dependents.push(rel);
            } else if transitive_dependents.len() < 30 {
                transitive_dependents.push(rel);
            }
        }

        let target_paths: Vec<String> = resolved_targets.iter().map(|t| t.rel_path.clone()).collect();
        let target_set: HashSet<String> = target_paths.iter().cloned().collect();
        let mut co_map: HashMap<String, CoChangePartner> = HashMap::new();
        for target_rel in target_paths.iter().take(3) {
            if let Ok(arch) = self.project_archaeology(target_rel, Some(&project.name), 8) {
                for partner in arch.co_change_partners {
                    let norm = normalize_rel(&partner.path);
                    if target_set.contains(&norm) {
                        continue;
                    }
                    co_map
                        .entry(norm.clone())
                        .and_modify(|existing| {
                            if partner.co_commits > existing.co_commits {
                                *existing = partner.clone();
                            }
                        })
                        .or_insert(CoChangePartner { path: norm, ..partner });
                }
            }
        }
        let mut co_change_partners: Vec<CoChangePartner> = co_map.into_values().collect();
        co_change_partners.sort_by(|a, b| {
            b.co_commits
                .cmp(&a.co_commits)
                .then_with(|| a.path.cmp(&b.path))
        });
        co_change_partners.truncate(12);

        let mut scope_paths = target_paths.clone();
        scope_paths.extend(direct_dependents.iter().cloned());

        let anchored_facts: Vec<ProjectFact> = self
            .list_facts(Some(&project.name), false, 80)?
            .into_iter()
            .filter(|f| match f.path.as_deref() {
                Some(fp) => {
                    let norm = normalize_rel(fp);
                    scope_paths.iter().any(|sp| paths_overlap(sp, &norm))
                }
                None => false,
            })
            .collect();

        let active_leases: Vec<AreaLease> = self
            .list_leases(Some(&project.name))?
            .into_iter()
            .filter(|l| {
                let area = normalize_rel(&l.area);
                scope_paths.iter().any(|sp| paths_overlap(sp, &area))
            })
            .collect();

        let status = self.project_status(&project.name)?;
        let keywords: Vec<String> = target_paths
            .iter()
            .filter_map(|p| Path::new(p).file_stem().and_then(|s| s.to_str()))
            .map(|s| s.to_lowercase())
            .collect();
        let mut open_questions: Vec<String> = status
            .open_questions
            .iter()
            .filter(|q| {
                let ql = q.to_lowercase();
                keywords.iter().any(|k| k.len() >= 3 && ql.contains(k))
            })
            .cloned()
            .collect();
        if open_questions.is_empty() {
            open_questions = status.open_questions.into_iter().take(5).collect();
        } else {
            open_questions.truncate(5);
        }

        let (affected_tests, suggested_commands) =
            collect_affected_tests(&project, &active, &resolved_targets, &blast);

        Ok(PreflightReport {
            project_name: project.name,
            targets: target_paths,
            direct_dependents,
            transitive_dependents,
            co_change_partners,
            anchored_facts,
            active_leases,
            open_questions,
            affected_tests,
            suggested_commands,
        })
    }
}

#[derive(Debug, Clone)]
struct TargetObj {
    id: String,
    rel_path: String,
}

struct BlastMap {
    distances: HashMap<String, usize>,
    rel_by_id: HashMap<String, String>,
}

impl Store {
    fn resolve_preflight_targets(
        &self,
        project: &Project,
        active: &[ActiveObjectRef],
        targets: &[String],
    ) -> Result<Vec<TargetObj>, StoreError> {
        let root = project.root_path.as_deref();
        let mut by_rel: HashMap<String, String> = HashMap::new();
        for obj in active {
            by_rel.insert(normalize_rel(&compact_path(&obj.path, root)), obj.id.clone());
        }

        let mut out = Vec::new();
        let mut seen = HashSet::new();

        if targets.is_empty() {
            if let Some(git) = crate::git::inspect_git_context(root) {
                for changed in git.changed_files {
                    let norm = normalize_rel(&changed);
                    if let Some(id) = by_rel.get(&norm)
                        && seen.insert(id.clone())
                    {
                        out.push(TargetObj {
                            id: id.clone(),
                            rel_path: norm,
                        });
                    }
                }
            }
            if out.is_empty()
                && let Ok(resume) = self.project_resume(&project.name)
            {
                for rec in resume.recently_modified.into_iter().take(3) {
                    let norm = normalize_rel(&compact_path(&rec.path, root));
                    if seen.insert(rec.id.clone()) {
                        out.push(TargetObj {
                            id: rec.id,
                            rel_path: norm,
                        });
                    }
                }
            }
            return Ok(out);
        }

        for raw in targets {
            let norm = normalize_rel(raw);
            if let Ok(details) = self.show_object_scoped(raw, Some(&project.name)) {
                let rel = normalize_rel(&compact_path(&details.object.path, root));
                if seen.insert(details.object.id.clone()) {
                    out.push(TargetObj {
                        id: details.object.id,
                        rel_path: rel,
                    });
                }
                continue;
            }
            let suffix = format!("/{norm}");
            let mut matched = active
                .iter()
                .filter(|obj| {
                    let rel = normalize_rel(&compact_path(&obj.path, root));
                    rel == norm || rel.ends_with(&suffix) || rel.starts_with(&format!("{norm}/"))
                })
                .collect::<Vec<_>>();
            matched.sort_by(|a, b| a.path.cmp(&b.path));
            if matched.is_empty() {
                return Err(StoreError::NotFound("target", raw.clone()));
            }
            for obj in matched {
                if seen.insert(obj.id.clone()) {
                    out.push(TargetObj {
                        id: obj.id.clone(),
                        rel_path: normalize_rel(&compact_path(&obj.path, root)),
                    });
                }
            }
        }
        Ok(out)
    }

    fn compute_reverse_blast(
        &self,
        project: &Project,
        active: &[ActiveObjectRef],
        targets: &[TargetObj],
    ) -> Result<BlastMap, StoreError> {
        let root = project.root_path.as_deref();
        let mut rel_by_id = HashMap::with_capacity(active.len());
        for obj in active {
            rel_by_id.insert(obj.id.clone(), normalize_rel(&compact_path(&obj.path, root)));
        }

        let mut distances = HashMap::new();
        let mut queue = VecDeque::new();
        for target in targets {
            distances.insert(target.id.clone(), 0usize);
            queue.push_back((target.id.clone(), 0usize));
        }

        while let Some((curr_id, dist)) = queue.pop_front() {
            if dist >= 3 {
                continue;
            }
            for edge in self.object_relations(&curr_id, GraphDirection::Incoming)? {
                if !rel_by_id.contains_key(&edge.from_object_id) {
                    continue;
                }
                let next_dist = dist + 1;
                if !distances.contains_key(&edge.from_object_id) {
                    distances.insert(edge.from_object_id.clone(), next_dist);
                    queue.push_back((edge.from_object_id, next_dist));
                }
            }
        }

        Ok(BlastMap {
            distances,
            rel_by_id,
        })
    }
}

fn collect_affected_tests(
    project: &Project,
    active: &[ActiveObjectRef],
    targets: &[TargetObj],
    blast: &BlastMap,
) -> (Vec<AffectedTest>, Vec<String>) {
    let root = project.root_path.as_deref();
    let mut touched_pkg_dirs: HashSet<PathBuf> = HashSet::new();
    for (id, dist) in &blast.distances {
        if *dist <= 1
            && let Some(rel) = blast.rel_by_id.get(id)
            && let Some(pkg_dir) = find_nearest_cargo_dir(root, rel)
        {
            touched_pkg_dirs.insert(pkg_dir);
        }
    }
    for target in targets {
        if let Some(pkg_dir) = find_nearest_cargo_dir(root, &target.rel_path) {
            touched_pkg_dirs.insert(pkg_dir);
        }
    }

    let mut tests = Vec::new();
    let mut cmds = BTreeSet::new();

    for obj in active {
        let rel = normalize_rel(&compact_path(&obj.path, root));
        if !is_test_file_path(&rel) {
            continue;
        }
        let reason_opt = if let Some(&dist) = blast.distances.get(&obj.id) {
            Some((
                dist,
                match dist {
                    0 => "target test file".to_string(),
                    1 => "direct reverse dependency of target".to_string(),
                    n => format!("transitive reverse dependency (depth {n})"),
                },
            ))
        } else if let Some(pkg_dir) = find_nearest_cargo_dir(root, &rel) {
            if touched_pkg_dirs.contains(&pkg_dir) {
                let pkg_label = pkg_dir
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("package");
                Some((1usize, format!("same package test suite ({pkg_label})")))
            } else {
                None
            }
        } else {
            None
        };

        if let Some((distance, reason)) = reason_opt {
            if let Some(cmd) = suggest_command_for_test(root, &rel) {
                cmds.insert(cmd);
            }
            tests.push(AffectedTest {
                path: rel,
                distance,
                reason,
            });
        }
    }

    if cmds.is_empty() {
        for pkg_dir in touched_pkg_dirs {
            if let Some(pkg_name) = read_cargo_package_name(&pkg_dir) {
                cmds.insert(format!("cargo test -p {pkg_name}"));
            }
        }
    }

    tests.sort_by(|a, b| a.distance.cmp(&b.distance).then_with(|| a.path.cmp(&b.path)));
    (tests, cmds.into_iter().collect())
}

fn suggest_command_for_test(root: Option<&str>, rel: &str) -> Option<String> {
    if rel.ends_with(".rs") {
        let pkg_dir = find_nearest_cargo_dir(root, rel)?;
        let pkg_name = read_cargo_package_name(&pkg_dir)?;
        if (rel.contains("/tests/") || rel.starts_with("tests/"))
            && let Some(stem) = Path::new(rel).file_stem().and_then(|s| s.to_str())
        {
            return Some(format!("cargo test -p {pkg_name} --test {stem}"));
        }
        return Some(format!("cargo test -p {pkg_name}"));
    }
    if rel.ends_with(".ps1") {
        return Some(format!("pwsh -NoProfile -File {rel}"));
    }
    if rel.ends_with(".py") {
        return Some(format!("pytest {rel}"));
    }
    if rel.ends_with(".ts") || rel.ends_with(".tsx") || rel.ends_with(".js") || rel.ends_with(".jsx")
    {
        return Some(format!("npm test -- {rel}"));
    }
    None
}

fn find_nearest_cargo_dir(root: Option<&str>, rel: &str) -> Option<PathBuf> {
    let base = root.map(PathBuf::from)?;
    let full = base.join(rel);
    let mut dir = full.parent()?;
    loop {
        if dir.join("Cargo.toml").is_file() {
            return Some(dir.to_path_buf());
        }
        if dir == base {
            break;
        }
        dir = dir.parent()?;
    }
    None
}

fn read_cargo_package_name(pkg_dir: &Path) -> Option<String> {
    let raw = std::fs::read_to_string(pkg_dir.join("Cargo.toml")).ok()?;
    let mut in_pkg = false;
    for line in raw.lines() {
        let trimmed = strip_toml_comment(line).trim();
        if trimmed.starts_with('[') {
            in_pkg = trimmed == "[package]";
            continue;
        }
        if in_pkg
            && let Some((k, v)) = trimmed.split_once('=')
            && k.trim() == "name"
        {
            return unquote_toml_string(v.trim());
        }
    }
    None
}

fn is_test_file_path(rel: &str) -> bool {
    let lower = rel.to_ascii_lowercase();
    lower.contains("/tests/")
        || lower.starts_with("tests/")
        || lower.starts_with("scripts/test_")
        || lower.ends_with("_test.rs")
        || lower.ends_with("_test.py")
        || lower.ends_with(".test.ts")
        || lower.ends_with(".test.tsx")
        || lower.ends_with(".test.js")
        || lower.ends_with(".spec.ts")
        || lower.ends_with(".spec.js")
        || Path::new(&lower)
            .file_name()
            .and_then(|s| s.to_str())
            .is_some_and(|n| n.starts_with("test_") && (n.ends_with(".py") || n.ends_with(".rs")))
}

fn resolve_rules_path(project: &Project, rules_path: Option<&Path>) -> Result<PathBuf, StoreError> {
    if let Some(p) = rules_path {
        return Ok(p.to_path_buf());
    }
    if let Some(root) = project.root_path.as_deref() {
        return Ok(Path::new(root).join("vos.rules.toml"));
    }
    Ok(PathBuf::from("vos.rules.toml"))
}

/// Parses declarative `[[rule]]` blocks from `vos.rules.toml` without extra dependencies.
pub fn parse_vos_rules_toml(raw: &str) -> Result<Vec<GuardRule>, StoreError> {
    let mut rules: Vec<GuardRule> = Vec::new();
    let mut current: Option<GuardRule> = None;
    let mut pending_key: Option<String> = None;
    let mut pending_buf = String::new();

    for (line_idx, raw_line) in raw.lines().enumerate() {
        let cleaned = strip_toml_comment(raw_line).trim();
        if cleaned.is_empty() {
            continue;
        }

        if let Some(key) = pending_key.take() {
            pending_buf.push(' ');
            pending_buf.push_str(cleaned);
            if pending_buf.contains(']') {
                let rule = current.as_mut().ok_or_else(|| {
                    StoreError::InvalidInput(format!("line {}: key outside [[rule]]", line_idx + 1))
                })?;
                assign_rule_field(rule, &key, &pending_buf, line_idx + 1)?;
                pending_buf.clear();
            } else {
                pending_key = Some(key);
            }
            continue;
        }

        if cleaned == "[[rule]]" || cleaned == "[rule]" {
            if let Some(prev) = current.take() {
                rules.push(finalize_rule(prev, rules.len())?);
            }
            current = Some(GuardRule::default());
            continue;
        }

        let Some((k, v)) = cleaned.split_once('=') else {
            return Err(StoreError::InvalidInput(format!(
                "invalid syntax in vos.rules.toml line {}: `{cleaned}`",
                line_idx + 1
            )));
        };
        let key = k.trim().to_string();
        let val = v.trim();
        let rule = current.as_mut().ok_or_else(|| {
            StoreError::InvalidInput(format!(
                "line {}: `{key}` must appear inside a [[rule]] block",
                line_idx + 1
            ))
        })?;
        if val.starts_with('[') && !val.contains(']') {
            pending_key = Some(key);
            pending_buf = val.to_string();
        } else {
            assign_rule_field(rule, &key, val, line_idx + 1)?;
        }
    }

    if pending_key.is_some() {
        return Err(StoreError::InvalidInput(
            "unclosed array literal in vos.rules.toml".to_string(),
        ));
    }
    if let Some(last) = current.take() {
        rules.push(finalize_rule(last, rules.len())?);
    }
    if rules.is_empty() {
        return Err(StoreError::InvalidInput(
            "vos.rules.toml contains no [[rule]] entries".to_string(),
        ));
    }
    Ok(rules)
}

fn finalize_rule(mut rule: GuardRule, idx: usize) -> Result<GuardRule, StoreError> {
    if rule.name.is_empty() {
        rule.name = format!("rule-{}", idx + 1);
    }
    if rule.from_prefixes.is_empty() {
        return Err(StoreError::InvalidInput(format!(
            "guard rule `{}` is missing `from` / `from_prefixes`",
            rule.name
        )));
    }
    if rule.forbid_to_prefixes.is_empty() && rule.forbid_imports.is_empty() {
        return Err(StoreError::InvalidInput(format!(
            "guard rule `{}` must define `forbid_to*` or `forbid_import*`",
            rule.name
        )));
    }
    Ok(rule)
}

fn assign_rule_field(
    rule: &mut GuardRule,
    key: &str,
    val: &str,
    line_no: usize,
) -> Result<(), StoreError> {
    match key {
        "name" => {
            rule.name = parse_toml_string_or_err(val, line_no)?;
        }
        "reason" => {
            rule.reason = Some(parse_toml_string_or_err(val, line_no)?);
        }
        "from" | "from_prefix" | "from_prefixes" => {
            for item in parse_toml_string_list(val, line_no)? {
                rule.from_prefixes.push(normalize_rel(&item));
            }
        }
        "forbid_to" | "forbid_to_prefix" | "forbid_to_prefixes" => {
            for item in parse_toml_string_list(val, line_no)? {
                rule.forbid_to_prefixes.push(normalize_rel(&item));
            }
        }
        "forbid_import" | "forbid_imports" => {
            rule.forbid_imports.extend(parse_toml_string_list(val, line_no)?);
        }
        other => {
            return Err(StoreError::InvalidInput(format!(
                "unknown key `{other}` in vos.rules.toml line {line_no}"
            )));
        }
    }
    Ok(())
}

fn parse_toml_string_list(val: &str, line_no: usize) -> Result<Vec<String>, StoreError> {
    let t = val.trim();
    if t.starts_with('[') && t.ends_with(']') {
        let inner = &t[1..t.len() - 1];
        let mut items = Vec::new();
        for part in inner.split(',') {
            let p = part.trim();
            if p.is_empty() {
                continue;
            }
            items.push(parse_toml_string_or_err(p, line_no)?);
        }
        return Ok(items);
    }
    Ok(vec![parse_toml_string_or_err(t, line_no)?])
}

fn parse_toml_string_or_err(val: &str, line_no: usize) -> Result<String, StoreError> {
    unquote_toml_string(val).ok_or_else(|| {
        StoreError::InvalidInput(format!(
            "expected quoted string in vos.rules.toml line {line_no}, found `{val}`"
        ))
    })
}

fn unquote_toml_string(val: &str) -> Option<String> {
    let t = val.trim();
    if (t.starts_with('"') && t.ends_with('"') && t.len() >= 2)
        || (t.starts_with('\'') && t.ends_with('\'') && t.len() >= 2)
    {
        Some(t[1..t.len() - 1].to_string())
    } else {
        None
    }
}

fn strip_toml_comment(line: &str) -> &str {
    let mut in_double = false;
    let mut in_single = false;
    for (idx, ch) in line.char_indices() {
        match ch {
            '"' if !in_single => in_double = !in_double,
            '\'' if !in_double => in_single = !in_single,
            '#' if !in_double && !in_single => return &line[..idx],
            _ => {}
        }
    }
    line
}

fn normalize_rel(path: &str) -> String {
    let s = path.replace('\\', "/");
    let s = s.strip_prefix("//?/").unwrap_or(&s);
    s.trim_start_matches("./").to_string()
}

fn matches_any_prefix(path: &str, prefixes: &[String]) -> bool {
    prefixes.iter().any(|pref| {
        let p = pref.trim_end_matches('/');
        !p.is_empty() && (path == p || path.starts_with(&format!("{p}/")))
    })
}

fn paths_overlap(a: &str, b: &str) -> bool {
    let an = a.trim_end_matches('/');
    let bn = b.trim_end_matches('/');
    an == bn || an.starts_with(&format!("{bn}/")) || bn.starts_with(&format!("{an}/"))
}

fn extract_import_statements(source: &str) -> Vec<String> {
    let mut stmts = Vec::new();
    let mut in_block_comment = false;
    let mut buf = String::new();
    let mut collecting_use = false;

    for raw_line in source.lines() {
        let mut line = raw_line.trim();
        if in_block_comment {
            if let Some(end) = line.find("*/") {
                line = line[end + 2..].trim();
                in_block_comment = false;
            } else {
                continue;
            }
        }
        if let Some(start) = line.find("/*") {
            if !line[start..].contains("*/") {
                in_block_comment = true;
            }
            line = line[..start].trim();
        }
        if line.is_empty() || line.starts_with("//") {
            continue;
        }
        if collecting_use {
            buf.push(' ');
            buf.push_str(line);
            if line.contains(';') {
                stmts.push(std::mem::take(&mut buf));
                collecting_use = false;
            }
            continue;
        }
        if line.starts_with("use ") || line.starts_with("pub use ") || line.starts_with("extern crate ") {
            if line.contains(';') {
                stmts.push(line.to_string());
            } else {
                buf = line.to_string();
                collecting_use = true;
            }
        } else if line.starts_with("import ")
            || line.starts_with("from ")
            || line.starts_with("#include ")
            || line.contains("require(")
        {
            stmts.push(line.to_string());
        }
    }
    stmts
}

fn import_matches_forbidden(stmt: &str, pattern: &str) -> bool {
    let pat = pattern.trim();
    if pat.is_empty() {
        return false;
    }
    let bytes = stmt.as_bytes();
    let pat_bytes = pat.as_bytes();
    let mut start = 0;
    while let Some(pos) = stmt[start..].find(pat) {
        let abs = start + pos;
        let before_ok = abs == 0 || !is_ident_byte(bytes[abs - 1]);
        let after = abs + pat_bytes.len();
        let after_ok = after >= bytes.len() || !is_ident_byte(bytes[after]);
        if before_ok && after_ok {
            return true;
        }
        start = abs + 1;
    }
    false
}

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{RelationInput, UpsertObjectInput};
    use tempfile::TempDir;
    use vos_core::RelationType;

    #[test]
    fn guard_detects_forbidden_import_and_edge_violations() {
        let tmp = TempDir::new().unwrap();
        let root = vos_core::util::canonicalize_path(tmp.path()).unwrap_or_else(|_| tmp.path().to_string_lossy().into_owned());
        let root_path = Path::new(&root);
        let cli_dir = root_path.join("apps/cli/src");
        let core_dir = root_path.join("crates/core/src");
        let store_dir = root_path.join("crates/store/src");
        std::fs::create_dir_all(&cli_dir).unwrap();
        std::fs::create_dir_all(&core_dir).unwrap();
        std::fs::create_dir_all(&store_dir).unwrap();

        let cli_main = cli_dir.join("main.rs");
        let store_batch = store_dir.join("batch.rs");
        std::fs::write(&cli_main, "use rusqlite::Connection;\nfn main() {}\n").unwrap();
        std::fs::write(&store_batch, "pub fn commit() {}\n").unwrap();

        let rules_path = root_path.join("vos.rules.toml");
        std::fs::write(
            &rules_path,
            r#"
[[rule]]
name = "cli-no-sqlite"
from = "apps/cli"
forbid_to = ["crates/store/src/batch.rs"]
forbid_import = "rusqlite"
reason = "CLI must not touch SQLite directly"
"#,
        )
        .unwrap();

        let mut store = Store::open_in_memory().unwrap();
        let proj = store.create_project("demo", Some(tmp.path()), None).unwrap();

        let mut batch = store.begin_index_batch().unwrap();
        batch
            .upsert_object(UpsertObjectInput {
                project_id: Some(proj.id.clone()),
                path: cli_main.to_string_lossy().into_owned(),
                kind: ObjectKind::Code,
                title: "main.rs".into(),
                summary: None,
                content_hash: "h1".into(),
                size_bytes: 32,
                modified_at: 100,
                is_noise: false,
                chunks: vec!["use rusqlite::Connection;".into()],
            })
            .unwrap();
        batch
            .upsert_object(UpsertObjectInput {
                project_id: Some(proj.id.clone()),
                path: store_batch.to_string_lossy().into_owned(),
                kind: ObjectKind::Code,
                title: "batch.rs".into(),
                summary: None,
                content_hash: "h2".into(),
                size_bytes: 20,
                modified_at: 100,
                is_noise: false,
                chunks: vec!["pub fn commit() {}".into()],
            })
            .unwrap();
        batch.commit().unwrap();

        let active = store.list_active_object_paths(Some(&proj.id)).unwrap();
        let cli_id = active
            .iter()
            .find(|a| a.title == "main.rs")
            .unwrap()
            .id
            .clone();
        let batch_id = active
            .iter()
            .find(|a| a.title == "batch.rs")
            .unwrap()
            .id
            .clone();

        let mut rel_batch = store.begin_index_batch().unwrap();
        rel_batch
            .sync_relations(
                &[cli_id.clone(), batch_id.clone()],
                &[RelationInput {
                    from_object_id: cli_id,
                    to_object_id: batch_id,
                    relation_type: RelationType::RustUse,
                    confidence: 1.0,
                    detail: None,
                }],
            )
            .unwrap();
        rel_batch.commit().unwrap();

        let report = store.check_guard_rules("demo", None).unwrap();
        assert!(!report.passed);
        assert_eq!(report.rules_checked, 1);
        assert_eq!(report.violations.len(), 2);
        assert!(report.violations.iter().any(|v| v.kind == "edge"));
        assert!(report.violations.iter().any(|v| v.kind == "import"));
    }

    #[test]
    fn affected_tests_and_preflight_surface_impacted_tests_and_packages() {
        let tmp = TempDir::new().unwrap();
        let root = vos_core::util::canonicalize_path(tmp.path()).unwrap_or_else(|_| tmp.path().to_string_lossy().into_owned());
        let root_path = Path::new(&root);
        let pkg_src = root_path.join("crates/core/src");
        let pkg_tests = root_path.join("crates/core/tests");
        std::fs::create_dir_all(&pkg_src).unwrap();
        std::fs::create_dir_all(&pkg_tests).unwrap();
        std::fs::write(
            root_path.join("crates/core/Cargo.toml"),
            "[package]\nname = \"my-core\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        let lib_rs = pkg_src.join("lib.rs");
        let e2e_rs = pkg_tests.join("smoke.rs");
        std::fs::write(&lib_rs, "pub fn ping() -> i32 { 1 }\n").unwrap();
        std::fs::write(&e2e_rs, "fn test_ping() {}\n").unwrap();

        let mut store = Store::open_in_memory().unwrap();
        let proj = store.create_project("demo", Some(tmp.path()), None).unwrap();
        let mut batch = store.begin_index_batch().unwrap();
        batch
            .upsert_object(UpsertObjectInput {
                project_id: Some(proj.id.clone()),
                path: lib_rs.to_string_lossy().into_owned(),
                kind: ObjectKind::Code,
                title: "lib.rs".into(),
                summary: None,
                content_hash: "c1".into(),
                size_bytes: 26,
                modified_at: 100,
                is_noise: false,
                chunks: vec!["pub fn ping() -> i32 { 1 }".into()],
            })
            .unwrap();
        batch
            .upsert_object(UpsertObjectInput {
                project_id: Some(proj.id.clone()),
                path: e2e_rs.to_string_lossy().into_owned(),
                kind: ObjectKind::Code,
                title: "smoke.rs".into(),
                summary: None,
                content_hash: "c2".into(),
                size_bytes: 18,
                modified_at: 100,
                is_noise: false,
                chunks: vec!["fn test_ping() {}".into()],
            })
            .unwrap();
        batch.commit().unwrap();

        store
            .add_fact(
                Some("demo"),
                "ping returns 1",
                Some(&lib_rs.to_string_lossy()),
                Some("ping"),
                Some("tester"),
            )
            .unwrap();
        store
            .claim_area("demo", "crates/core/src", "worker-1", 30, Some("editing"))
            .unwrap();

        let aff = store
            .affected_tests("demo", &["crates/core/src/lib.rs".to_string()])
            .unwrap();
        assert_eq!(aff.affected_tests.len(), 1);
        assert_eq!(aff.affected_tests[0].path, "crates/core/tests/smoke.rs");
        assert!(aff
            .suggested_commands
            .iter()
            .any(|c| c == "cargo test -p my-core --test smoke"));

        let pre = store
            .preflight("demo", &["crates/core/src/lib.rs".to_string()])
            .unwrap();
        assert_eq!(pre.anchored_facts.len(), 1);
        assert_eq!(pre.active_leases.len(), 1);
        assert_eq!(pre.affected_tests.len(), 1);
    }
}
