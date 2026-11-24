use std::collections::{HashMap, HashSet};
use std::path::{Component, Path, PathBuf};

use vos_core::{ObjectKind, RelationType};
use vos_store::{ActiveObjectRef, RelationInput};

/// Unresolved structural reference extracted from a single source file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RawRef {
    RustMod {
        name: String,
        explicit_path: Option<String>,
    },
    RustUse {
        segments: Vec<String>,
    },
    MdLink {
        target: String,
    },
    PathRef {
        raw: String,
    },
    UniqueTitleRef {
        title: String,
    },
    CargoLibIdent {
        ident: String,
    },
}

/// Scanned source object metadata and extracted references waiting for ID resolution.
#[derive(Debug, Clone)]
pub(crate) struct ScannedObjectRefs {
    pub object_id: String,
    pub project_id: Option<String>,
    pub path: String,
    pub raw_refs: Vec<RawRef>,
}

#[derive(Debug, Default)]
struct ProjectLookup {
    by_norm_path: HashMap<String, String>,
    by_title: HashMap<String, Vec<String>>,
    by_workspace_crate: HashMap<String, Option<(PathBuf, String)>>,
}

impl ProjectLookup {
    fn insert(&mut self, object_id: String, raw_path: &str, title: &str) {
        let norm = normalize_lexical_key(raw_path);
        self.by_norm_path.insert(norm, object_id.clone());

        let title_key = title.to_ascii_lowercase();
        let entry = self.by_title.entry(title_key).or_default();
        if !entry.contains(&object_id) {
            entry.push(object_id.clone());
        }

        if title.eq_ignore_ascii_case("lib.rs") {
            let clean = raw_path.replace('\\', "/");
            let p = Path::new(&clean);
            if let Some(src_dir) = p.parent()
                && src_dir
                    .file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.eq_ignore_ascii_case("src"))
                && let Some(crate_dir) = src_dir.parent()
                && let Some(dir_name) = crate_dir.file_name().and_then(|n| n.to_str())
            {
                let crate_ident = dir_name.replace('-', "_").to_ascii_lowercase();
                if is_ident(&crate_ident) {
                    self.register_workspace_crate(crate_ident, src_dir.to_path_buf(), object_id);
                }
            }
        }
    }

    fn register_workspace_crate(&mut self, ident: String, src_dir: PathBuf, lib_id: String) {
        self.by_workspace_crate
            .entry(ident)
            .and_modify(|slot| {
                if let Some((existing_dir, _)) = slot
                    && existing_dir == &src_dir
                {
                    return;
                }
                *slot = None;
            })
            .or_insert_with(|| Some((src_dir, lib_id)));
    }

    fn lookup_workspace_crate(&self, ident: &str) -> Option<(&Path, &str)> {
        let key = ident.to_ascii_lowercase();
        self.by_workspace_crate
            .get(&key)?
            .as_ref()
            .map(|(dir, id)| (dir.as_path(), id.as_str()))
    }

    fn lookup_path(&self, candidate: &Path) -> Option<&str> {
        let key = normalize_lexical_key(&candidate.to_string_lossy());
        self.by_norm_path.get(&key).map(String::as_str)
    }

    fn lookup_unique_title(&self, title: &str) -> Option<&str> {
        let key = title.to_ascii_lowercase();
        let matches = self.by_title.get(&key)?;
        if matches.len() == 1 {
            matches.first().map(String::as_str)
        } else {
            None
        }
    }
}

/// Extracts deterministic candidate references from UTF-8 file text without touching disk again.
pub(crate) fn extract_refs(path: &Path, kind: ObjectKind, text: &str) -> Vec<RawRef> {
    let mut refs = Vec::new();
    let is_rust = path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("rs"));
    let is_markdown = path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| matches!(e.to_ascii_lowercase().as_str(), "md" | "markdown"));

    if is_rust {
        extract_rust_refs(text, &mut refs);
    }
    if is_markdown {
        extract_markdown_links(text, &mut refs);
    }
    if path
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.eq_ignore_ascii_case("Cargo.toml"))
    {
        for ident in extract_cargo_lib_idents(text) {
            refs.push(RawRef::CargoLibIdent { ident });
        }
    }

    let allow_backslash = matches!(kind, ObjectKind::Note | ObjectKind::Config)
        || path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|ext| matches!(ext.to_ascii_lowercase().as_str(), "ps1" | "bat" | "cmd"));
    extract_path_mentions(text, kind, allow_backslash, &mut refs);
    refs
}

/// Resolves extracted references into persisted `RelationInput` edges scoped per project.
pub(crate) fn resolve_relations(
    scanned: &[ScannedObjectRefs],
    active_objects: &[ActiveObjectRef],
    project_roots: &HashMap<String, String>,
) -> Vec<RelationInput> {
    let mut lookups: HashMap<Option<String>, ProjectLookup> = HashMap::new();

    for obj in active_objects {
        lookups.entry(obj.project_id.clone()).or_default().insert(
            obj.id.clone(),
            &obj.path,
            &obj.title,
        );
    }

    // Register explicit crate identifiers declared in `[package]` / `[lib]` of scanned `Cargo.toml`.
    for src in scanned {
        let Some(lookup) = lookups.get_mut(&src.project_id) else {
            continue;
        };
        let Some(manifest_dir) = Path::new(&src.path).parent() else {
            continue;
        };
        let src_dir = manifest_dir.join("src");
        let lib_path = src_dir.join("lib.rs");
        let Some(lib_id) = lookup.lookup_path(&lib_path).map(str::to_string) else {
            continue;
        };
        for raw in &src.raw_refs {
            if let RawRef::CargoLibIdent { ident } = raw {
                lookup.register_workspace_crate(ident.clone(), src_dir.clone(), lib_id.clone());
            }
        }
    }

    let mut edges = Vec::new();
    let mut seen: HashSet<(String, String, RelationType)> = HashSet::new();

    for src in scanned {
        let Some(lookup) = lookups.get(&src.project_id) else {
            continue;
        };
        let src_path = Path::new(&src.path);
        let src_dir = src_path.parent().unwrap_or_else(|| Path::new(""));
        let proj_root = src
            .project_id
            .as_ref()
            .and_then(|pid| project_roots.get(pid))
            .map(PathBuf::from);

        for raw in &src.raw_refs {
            let resolved: Option<(String, RelationType, Option<String>)> = match raw {
                RawRef::RustMod {
                    name,
                    explicit_path,
                } => resolve_rust_mod(src_path, src_dir, name, explicit_path.as_deref(), lookup)
                    .map(|to_id| (to_id, RelationType::RustMod, Some(format!("mod {name}")))),
                RawRef::RustUse { segments } => {
                    resolve_rust_use(src_path, src_dir, segments, lookup).map(|to_id| {
                        (
                            to_id,
                            RelationType::RustUse,
                            Some(format!("use {}", segments.join("::"))),
                        )
                    })
                }
                RawRef::MdLink { target } => {
                    resolve_relative_path(src_dir, proj_root.as_deref(), target, lookup)
                        .map(|to_id| (to_id, RelationType::MdLink, Some(format!("link: {target}"))))
                }
                RawRef::PathRef { raw: rel } => {
                    resolve_relative_path(src_dir, proj_root.as_deref(), rel, lookup).map(|to_id| {
                        (
                            to_id,
                            RelationType::PathRef,
                            Some(format!("references {rel}")),
                        )
                    })
                }
                RawRef::UniqueTitleRef { title } => {
                    lookup.lookup_unique_title(title).map(|to_id| {
                        (
                            to_id.to_string(),
                            RelationType::PathRef,
                            Some(format!("mentions {title}")),
                        )
                    })
                }
                RawRef::CargoLibIdent { .. } => None,
            };

            let Some((to_id, rel_type, detail)) = resolved else {
                continue;
            };
            if to_id == src.object_id {
                continue;
            }
            // Suppress redundant PathRef when an explicit MdLink already connects the same objects.
            if rel_type == RelationType::PathRef
                && seen.contains(&(src.object_id.clone(), to_id.clone(), RelationType::MdLink))
            {
                continue;
            }
            if seen.insert((src.object_id.clone(), to_id.clone(), rel_type)) {
                edges.push(RelationInput {
                    from_object_id: src.object_id.clone(),
                    to_object_id: to_id,
                    relation_type: rel_type,
                    confidence: rel_type.default_confidence(),
                    detail,
                });
            }
        }
    }

    edges
}

fn extract_rust_refs(text: &str, out: &mut Vec<RawRef>) {
    let mut pending_attr_path: Option<String> = None;
    let mut collecting_use: Option<String> = None;

    for line in text.lines() {
        let code = strip_rust_line_comment(line).trim();
        if code.is_empty() {
            continue;
        }

        if let Some(ref mut buf) = collecting_use {
            buf.push(' ');
            buf.push_str(code);
            if let Some((before_semi, _)) = buf.split_once(';') {
                parse_use_statement(before_semi, out);
                collecting_use = None;
            } else if buf.len() > 1024 {
                collecting_use = None;
            }
            continue;
        }

        if code.starts_with("#[") {
            if let Some(attr_path) = parse_path_attribute(code) {
                pending_attr_path = Some(attr_path);
            }
            continue;
        }

        if let Some(mod_name) = parse_mod_decl(code) {
            out.push(RawRef::RustMod {
                name: mod_name,
                explicit_path: pending_attr_path.take(),
            });
            continue;
        }
        pending_attr_path = None;

        if let Some(rest) = strip_use_prefix(code) {
            if let Some((before_semi, _)) = rest.split_once(';') {
                parse_use_statement(before_semi, out);
            } else {
                collecting_use = Some(rest.to_string());
            }
        }
    }
}

fn strip_rust_line_comment(line: &str) -> &str {
    let mut in_str = false;
    let mut escape = false;
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if escape {
            escape = false;
            i += 1;
            continue;
        }
        if in_str {
            if b == b'\\' {
                escape = true;
            } else if b == b'"' {
                in_str = false;
            }
            i += 1;
            continue;
        }
        if b == b'"' {
            in_str = true;
        } else if b == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'/' {
            return &line[..i];
        }
        i += 1;
    }
    line
}

fn parse_path_attribute(code: &str) -> Option<String> {
    let inner = code.strip_prefix("#[")?.strip_suffix(']')?.trim();
    let after_path = inner.strip_prefix("path")?.trim_start();
    let after_eq = after_path.strip_prefix('=')?.trim_start();
    let quoted = after_eq.strip_prefix('"')?;
    let end = quoted.find('"')?;
    Some(quoted[..end].to_string())
}

fn parse_mod_decl(code: &str) -> Option<String> {
    let after_vis = strip_visibility_prefix(code);
    let rest = after_vis.strip_prefix("mod ")?.trim_start();
    if rest.contains('{') {
        return None;
    }
    let ident = rest.strip_suffix(';')?.trim();
    if is_ident(ident) {
        Some(ident.to_string())
    } else {
        None
    }
}

fn strip_use_prefix(code: &str) -> Option<&str> {
    let after_vis = strip_visibility_prefix(code);
    after_vis.strip_prefix("use ")
}

fn strip_visibility_prefix(code: &str) -> &str {
    let trimmed = code.trim_start();
    if let Some(after_pub) = trimmed.strip_prefix("pub") {
        let after_ws = after_pub.trim_start();
        if after_ws.starts_with('(')
            && let Some(close_idx) = after_ws.find(')')
        {
            return after_ws[close_idx + 1..].trim_start();
        }
        if after_pub.starts_with(char::is_whitespace) {
            return after_ws;
        }
    }
    trimmed
}

fn parse_use_statement(stmt: &str, out: &mut Vec<RawRef>) {
    let mut expanded = Vec::new();
    expand_use_tree(stmt.trim(), &[], &mut expanded);
    for segments in expanded {
        let Some(first) = segments.first() else {
            continue;
        };
        let is_relative =
            matches!(first.as_str(), "crate" | "super" | "self") && segments.len() >= 2;
        let is_crate_ident = !matches!(
            first.as_str(),
            "crate" | "super" | "self" | "std" | "core" | "alloc"
        ) && is_ident(first);
        if is_relative || is_crate_ident {
            out.push(RawRef::RustUse { segments });
        }
    }
}

fn expand_use_tree(tree: &str, prefix: &[String], out: &mut Vec<Vec<String>>) {
    let trimmed = tree.trim();
    if trimmed.is_empty() || trimmed == "*" {
        return;
    }
    if let Some(brace_start) = trimmed.find('{') {
        let Some(brace_end) = trimmed.rfind('}') else {
            return;
        };
        let head = trimmed[..brace_start].trim().trim_end_matches("::");
        let mut next_prefix = prefix.to_vec();
        for seg in head.split("::").map(str::trim).filter(|s| !s.is_empty()) {
            next_prefix.push(seg.to_string());
        }
        let inner = &trimmed[brace_start + 1..brace_end];
        for item in split_top_level_commas(inner) {
            expand_use_tree(item, &next_prefix, out);
        }
        return;
    }

    let without_alias = trimmed
        .split_once(" as ")
        .map_or(trimmed, |(left, _)| left)
        .trim();
    if without_alias == "self" {
        if !prefix.is_empty() {
            out.push(prefix.to_vec());
        }
        return;
    }
    let mut full = prefix.to_vec();
    for seg in without_alias
        .split("::")
        .map(str::trim)
        .filter(|s| !s.is_empty() && *s != "*")
    {
        full.push(seg.to_string());
    }
    if !full.is_empty() {
        out.push(full);
    }
}

fn split_top_level_commas(input: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut depth = 0_usize;
    let mut start = 0_usize;
    for (idx, ch) in input.char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                parts.push(input[start..idx].trim());
                start = idx + 1;
            }
            _ => {}
        }
    }
    let tail = input[start..].trim();
    if !tail.is_empty() {
        parts.push(tail);
    }
    parts
}

fn extract_markdown_links(text: &str, out: &mut Vec<RawRef>) {
    let mut in_fence = false;
    for line in text.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }

        let mut rest = line;
        while let Some(open_paren) = rest.find("](") {
            let after = &rest[open_paren + 2..];
            let Some(close_paren) = after.find(')') else {
                break;
            };
            let raw_target = after[..close_paren].trim();
            if let Some(clean) = clean_md_target(raw_target) {
                out.push(RawRef::MdLink { target: clean });
            }
            rest = &after[close_paren + 1..];
        }
    }
}

fn clean_md_target(raw: &str) -> Option<String> {
    let unquoted = raw
        .strip_prefix('<')
        .and_then(|s| s.strip_suffix('>'))
        .unwrap_or(raw);
    let target = unquoted
        .split_whitespace()
        .next()
        .unwrap_or("")
        .split('#')
        .next()
        .unwrap_or("")
        .split('?')
        .next()
        .unwrap_or("")
        .trim();
    if target.is_empty()
        || target.contains("://")
        || target.starts_with("mailto:")
        || target.starts_with('#')
    {
        return None;
    }
    Some(target.to_string())
}

fn extract_path_mentions(
    text: &str,
    kind: ObjectKind,
    allow_backslash: bool,
    out: &mut Vec<RawRef>,
) {
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let scan_line = match kind {
            ObjectKind::Code => {
                // Only inspect comments or lines containing path-like separators for general PathRef.
                trimmed
            }
            ObjectKind::Note | ObjectKind::Config => trimmed,
            _ => continue,
        };

        for raw_token in scan_line.split(|c: char| {
            c.is_whitespace()
                || matches!(
                    c,
                    '"' | '\'' | '`' | '(' | ')' | '[' | ']' | '<' | '>' | ',' | ';' | '{' | '}'
                )
        }) {
            let token = clean_candidate_token(raw_token);
            if token.is_empty() || token.contains("://") {
                continue;
            }
            if (token.contains('/') || (allow_backslash && token.contains('\\')))
                && looks_like_file_path(token)
            {
                let norm_raw = if allow_backslash {
                    token.replace('\\', "/")
                } else {
                    token.to_string()
                };
                out.push(RawRef::PathRef { raw: norm_raw });
            } else if matches!(kind, ObjectKind::Note | ObjectKind::Config)
                && looks_like_filename_only(token)
            {
                out.push(RawRef::UniqueTitleRef {
                    title: token.to_string(),
                });
            }
        }
    }
}

fn clean_candidate_token(raw: &str) -> &str {
    let trimmed = raw.trim_matches(|c: char| matches!(c, '*' | '_' | '!' | '?'));
    let no_frag = trimmed.split('#').next().unwrap_or(trimmed);
    // Strip trailing `:line` or `:line:col` suffixes.
    if let Some((head, tail)) = no_frag.rsplit_once(':')
        && !tail.is_empty()
        && tail.chars().all(|c| c.is_ascii_digit())
    {
        return head.trim_end_matches(':');
    }
    no_frag.trim_end_matches(&['.', ':'][..])
}

fn looks_like_file_path(token: &str) -> bool {
    if (token.starts_with('/') || token.starts_with('\\')) && !token.contains('.') {
        return false;
    }
    let Some(last) = token.rsplit(['/', '\\']).next() else {
        return false;
    };
    looks_like_filename_only(last)
}

fn looks_like_filename_only(token: &str) -> bool {
    if token.contains('/') || token.contains('\\') || token.len() < 4 || token.len() > 80 {
        return false;
    }
    let Some((stem, ext)) = token.rsplit_once('.') else {
        return false;
    };
    if stem.is_empty()
        || !stem
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
    {
        return false;
    }
    (1..=10).contains(&ext.len()) && ext.chars().all(|c| c.is_ascii_alphanumeric())
}

fn resolve_rust_mod(
    src_path: &Path,
    src_dir: &Path,
    name: &str,
    explicit_path: Option<&str>,
    lookup: &ProjectLookup,
) -> Option<String> {
    if let Some(rel) = explicit_path {
        return lookup.lookup_path(&src_dir.join(rel)).map(str::to_string);
    }

    let search_base = rust_submodule_base_dir(src_path, src_dir);
    let direct_file = search_base.join(format!("{name}.rs"));
    if let Some(id) = lookup.lookup_path(&direct_file) {
        return Some(id.to_string());
    }
    let nested_mod = search_base.join(name).join("mod.rs");
    lookup.lookup_path(&nested_mod).map(str::to_string)
}

fn resolve_rust_use(
    src_path: &Path,
    src_dir: &Path,
    segments: &[String],
    lookup: &ProjectLookup,
) -> Option<String> {
    let first = segments.first()?.as_str();
    match first {
        "crate" => {
            let base_dir = find_rust_crate_src_root(src_dir, lookup)?;
            resolve_module_path(&base_dir, &segments[1..], lookup)
        }
        "self" => {
            let base_dir = rust_submodule_base_dir(src_path, src_dir);
            resolve_module_path(&base_dir, &segments[1..], lookup)
        }
        "super" => {
            let mut dir = rust_submodule_base_dir(src_path, src_dir);
            let mut idx = 0;
            while idx < segments.len() && segments[idx] == "super" {
                dir = dir.parent()?.to_path_buf();
                idx += 1;
            }
            resolve_module_path(&dir, &segments[idx..], lookup)
        }
        crate_ident => {
            let (crate_src_dir, lib_id) = lookup.lookup_workspace_crate(crate_ident)?;
            resolve_module_path(crate_src_dir, &segments[1..], lookup)
                .or_else(|| Some(lib_id.to_string()))
        }
    }
}

fn resolve_module_path(base_dir: &Path, tail: &[String], lookup: &ProjectLookup) -> Option<String> {
    if tail.is_empty() {
        return None;
    }
    // Check progressively shorter prefixes so `use crate::models::GraphEdge` resolves `src/models.rs`.
    for len in (1..=tail.len()).rev() {
        let mut candidate_dir = base_dir.to_path_buf();
        for seg in &tail[..len - 1] {
            candidate_dir = candidate_dir.join(seg);
        }
        let leaf = &tail[len - 1];
        if let Some(id) = lookup.lookup_path(&candidate_dir.join(format!("{leaf}.rs"))) {
            return Some(id.to_string());
        }
        if let Some(id) = lookup.lookup_path(&candidate_dir.join(leaf).join("mod.rs")) {
            return Some(id.to_string());
        }
    }
    None
}

fn rust_submodule_base_dir(src_path: &Path, src_dir: &Path) -> PathBuf {
    let file_name = src_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default();
    if matches!(file_name, "lib.rs" | "main.rs" | "mod.rs") {
        return src_dir.to_path_buf();
    }
    let parent_name = src_dir
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default();
    if matches!(parent_name, "bin" | "tests" | "examples") {
        return src_dir.to_path_buf();
    }
    let stem = src_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or_default();
    src_dir.join(stem)
}

fn find_rust_crate_src_root(start_dir: &Path, lookup: &ProjectLookup) -> Option<PathBuf> {
    for ancestor in start_dir.ancestors() {
        if ancestor.file_name().and_then(|n| n.to_str()) == Some("src")
            || lookup.lookup_path(&ancestor.join("lib.rs")).is_some()
            || lookup.lookup_path(&ancestor.join("main.rs")).is_some()
        {
            return Some(ancestor.to_path_buf());
        }
    }
    None
}

fn resolve_relative_path(
    src_dir: &Path,
    project_root: Option<&Path>,
    raw_rel: &str,
    lookup: &ProjectLookup,
) -> Option<String> {
    let cleaned = raw_rel.trim_start_matches('/');
    if let Some(id) = lookup.lookup_path(&src_dir.join(cleaned)) {
        return Some(id.to_string());
    }
    if let Some(root) = project_root
        && let Some(id) = lookup.lookup_path(&root.join(cleaned))
    {
        return Some(id.to_string());
    }
    None
}

fn is_ident(s: &str) -> bool {
    let mut chars = s.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first.is_ascii_alphabetic() || first == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn extract_cargo_lib_idents(text: &str) -> Vec<String> {
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Section {
        Other,
        Package,
        Lib,
    }

    let mut section = Section::Other;
    let mut pkg_name: Option<String> = None;
    let mut lib_name: Option<String> = None;

    for line in text.lines() {
        let trimmed = line.split('#').next().unwrap_or("").trim();
        if trimmed.is_empty() {
            continue;
        }
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            let header = trimmed.trim_matches(|c| c == '[' || c == ']').trim();
            section = if trimmed.starts_with("[[") {
                Section::Other
            } else {
                match header {
                    "package" => Section::Package,
                    "lib" => Section::Lib,
                    _ => Section::Other,
                }
            };
            continue;
        }
        if matches!(section, Section::Package | Section::Lib)
            && let Some((key, value)) = trimmed.split_once('=')
            && key.trim() == "name"
        {
            let clean = value
                .trim()
                .trim_matches(|c| c == '"' || c == '\'')
                .replace('-', "_")
                .to_ascii_lowercase();
            if is_ident(&clean) {
                match section {
                    Section::Package => pkg_name = Some(clean),
                    Section::Lib => lib_name = Some(clean),
                    Section::Other => {}
                }
            }
        }
    }

    if let Some(lib) = lib_name {
        vec![lib]
    } else if let Some(pkg) = pkg_name {
        vec![pkg]
    } else {
        Vec::new()
    }
}

fn normalize_lexical_key(raw: &str) -> String {
    let normalized_slashes = raw.replace('\\', "/");
    let mut parts: Vec<String> = Vec::new();
    let mut prefix = String::new();

    for comp in Path::new(&normalized_slashes).components() {
        match comp {
            Component::Prefix(p) => {
                prefix = p.as_os_str().to_string_lossy().into_owned();
            }
            Component::RootDir => {
                if !prefix.ends_with('/') {
                    prefix.push('/');
                }
            }
            Component::CurDir => {}
            Component::ParentDir => {
                parts.pop();
            }
            Component::Normal(s) => {
                parts.push(s.to_string_lossy().into_owned());
            }
        }
    }

    let joined = format!("{prefix}{}", parts.join("/")).replace('\\', "/");
    if cfg!(windows) {
        joined.to_ascii_lowercase()
    } else {
        joined
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    #[test]
    fn extracts_rust_mod_use_and_markdown_links() {
        let rs_code = r#"
#[path = "custom/helper.rs"]
mod helper;
pub mod graph;
use crate::models::{GraphEdge, GraphHub};
use super::util;
"#;
        let refs = extract_refs(Path::new("src/lib.rs"), ObjectKind::Code, rs_code);
        assert!(refs.contains(&RawRef::RustMod {
            name: "helper".to_string(),
            explicit_path: Some("custom/helper.rs".to_string()),
        }));
        assert!(refs.contains(&RawRef::RustMod {
            name: "graph".to_string(),
            explicit_path: None,
        }));
        assert!(refs.contains(&RawRef::RustUse {
            segments: vec![
                "crate".to_string(),
                "models".to_string(),
                "GraphEdge".to_string()
            ],
        }));
        assert!(refs.contains(&RawRef::RustUse {
            segments: vec!["super".to_string(), "util".to_string()],
        }));

        let md = "See [Spec](docs/SPEC.md#schema) and [Site](https://example.com).";
        let md_refs = extract_refs(Path::new("README.md"), ObjectKind::Note, md);
        assert!(md_refs.contains(&RawRef::MdLink {
            target: "docs/SPEC.md".to_string(),
        }));
        assert!(
            !md_refs
                .iter()
                .any(|r| matches!(r, RawRef::MdLink { target } if target.contains("example.com")))
        );
    }

    #[test]
    fn resolves_workspace_crate_use_edges_across_crates() {
        let scanned = vec![ScannedObjectRefs {
            object_id: "obj_cli".to_string(),
            project_id: Some("p1".to_string()),
            path: "/ws/apps/vos-cli/src/main.rs".to_string(),
            raw_refs: extract_refs(
                Path::new("/ws/apps/vos-cli/src/main.rs"),
                ObjectKind::Code,
                "use vos_core::util::now_unix;\nuse vos_store::Store;\nuse anyhow::Result;\n",
            ),
        }];
        let active = vec![
            ActiveObjectRef {
                id: "obj_cli".to_string(),
                path: "/ws/apps/vos-cli/src/main.rs".to_string(),
                kind: ObjectKind::Code,
                title: "main.rs".to_string(),
                project_id: Some("p1".to_string()),
            },
            ActiveObjectRef {
                id: "obj_core_lib".to_string(),
                path: "/ws/crates/vos-core/src/lib.rs".to_string(),
                kind: ObjectKind::Code,
                title: "lib.rs".to_string(),
                project_id: Some("p1".to_string()),
            },
            ActiveObjectRef {
                id: "obj_core_util".to_string(),
                path: "/ws/crates/vos-core/src/util.rs".to_string(),
                kind: ObjectKind::Code,
                title: "util.rs".to_string(),
                project_id: Some("p1".to_string()),
            },
            ActiveObjectRef {
                id: "obj_store_lib".to_string(),
                path: "/ws/crates/vos-store/src/lib.rs".to_string(),
                kind: ObjectKind::Code,
                title: "lib.rs".to_string(),
                project_id: Some("p1".to_string()),
            },
        ];

        let edges = resolve_relations(&scanned, &active, &HashMap::new());
        assert!(
            edges
                .iter()
                .any(|e| e.to_object_id == "obj_core_util"
                    && e.relation_type == RelationType::RustUse)
        );
        assert!(
            edges
                .iter()
                .any(|e| e.to_object_id == "obj_store_lib"
                    && e.relation_type == RelationType::RustUse)
        );
        assert_eq!(edges.len(), 2);
    }

    #[test]
    fn deduplicates_md_link_and_path_ref_for_same_pair() {
        let md = "See [Main](apps/vos-cli/src/main.rs) and apps/vos-cli/src/main.rs.";
        let scanned = vec![ScannedObjectRefs {
            object_id: "obj_readme".to_string(),
            project_id: Some("p1".to_string()),
            path: "/ws/README.md".to_string(),
            raw_refs: extract_refs(Path::new("/ws/README.md"), ObjectKind::Note, md),
        }];
        let active = vec![
            ActiveObjectRef {
                id: "obj_readme".to_string(),
                path: "/ws/README.md".to_string(),
                kind: ObjectKind::Note,
                title: "README.md".to_string(),
                project_id: Some("p1".to_string()),
            },
            ActiveObjectRef {
                id: "obj_main".to_string(),
                path: "/ws/apps/vos-cli/src/main.rs".to_string(),
                kind: ObjectKind::Code,
                title: "main.rs".to_string(),
                project_id: Some("p1".to_string()),
            },
        ];

        let edges = resolve_relations(&scanned, &active, &HashMap::new());
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].relation_type, RelationType::MdLink);
    }

    #[test]
    fn extracts_windows_backslash_paths_from_notes_and_powershell_scripts() {
        let ps1 = "pwsh -NoProfile -File scripts\\run_v112_sprint.ps1\n";
        let ps1_refs = extract_refs(Path::new("scripts/test_all.ps1"), ObjectKind::Code, ps1);
        assert!(ps1_refs.contains(&RawRef::PathRef {
            raw: "scripts/run_v112_sprint.ps1".to_string(),
        }));
    }

    #[test]
    fn resolves_cargo_toml_package_and_lib_crate_names() {
        let manifest = "[package]\nname = \"forge-editor\"\nversion = \"0.1.0\"\n[[bin]]\nname = \"tessera-forge\"\npath = \"src/main.rs\"\n";
        let main_src = "use forge_editor::ForgeEditor;\n";
        let lib_src = "pub struct ForgeEditor;\n";

        let scanned = vec![
            ScannedObjectRefs {
                object_id: "obj_toml".to_string(),
                project_id: Some("p1".to_string()),
                path: "/ws/apps/forge/Cargo.toml".to_string(),
                raw_refs: extract_refs(
                    Path::new("/ws/apps/forge/Cargo.toml"),
                    ObjectKind::Config,
                    manifest,
                ),
            },
            ScannedObjectRefs {
                object_id: "obj_main".to_string(),
                project_id: Some("p1".to_string()),
                path: "/ws/apps/forge/src/main.rs".to_string(),
                raw_refs: extract_refs(
                    Path::new("/ws/apps/forge/src/main.rs"),
                    ObjectKind::Code,
                    main_src,
                ),
            },
            ScannedObjectRefs {
                object_id: "obj_lib".to_string(),
                project_id: Some("p1".to_string()),
                path: "/ws/apps/forge/src/lib.rs".to_string(),
                raw_refs: extract_refs(
                    Path::new("/ws/apps/forge/src/lib.rs"),
                    ObjectKind::Code,
                    lib_src,
                ),
            },
        ];
        let active = vec![
            ActiveObjectRef {
                id: "obj_toml".to_string(),
                path: "/ws/apps/forge/Cargo.toml".to_string(),
                kind: ObjectKind::Config,
                title: "Cargo.toml".to_string(),
                project_id: Some("p1".to_string()),
            },
            ActiveObjectRef {
                id: "obj_main".to_string(),
                path: "/ws/apps/forge/src/main.rs".to_string(),
                kind: ObjectKind::Code,
                title: "main.rs".to_string(),
                project_id: Some("p1".to_string()),
            },
            ActiveObjectRef {
                id: "obj_lib".to_string(),
                path: "/ws/apps/forge/src/lib.rs".to_string(),
                kind: ObjectKind::Code,
                title: "lib.rs".to_string(),
                project_id: Some("p1".to_string()),
            },
        ];

        let edges = resolve_relations(&scanned, &active, &HashMap::new());
        assert!(edges.iter().any(|e| {
            e.from_object_id == "obj_main"
                && e.to_object_id == "obj_lib"
                && e.relation_type == RelationType::RustUse
        }));
        assert!(!scanned[0].raw_refs.contains(&RawRef::CargoLibIdent {
            ident: "tessera_forge".to_string(),
        }));
    }
}
