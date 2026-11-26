//! Deterministic local import/include extractors for C/C++, TypeScript/JavaScript, and Python.

use std::path::Path;

use vos_core::RelationType;

use crate::relations::{ProjectLookup, RawRef, resolve_relative_path};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CodeImportKind {
    CInclude,
    JsRelative,
    PyModule { level: usize },
}

/// Dispatches language-specific import/include extraction by file extension.
pub(crate) fn extract_language_imports(path: &Path, text: &str, out: &mut Vec<RawRef>) {
    let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
        return;
    };
    match ext.to_ascii_lowercase().as_str() {
        "c" | "cc" | "cpp" | "cxx" | "h" | "hh" | "hpp" | "hxx" => extract_c_includes(text, out),
        "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" => extract_js_imports(text, out),
        "py" => extract_python_imports(text, out),
        _ => {}
    }
}

/// Resolves a `CodeImport` reference within `src_dir`, `project_root`, and `lookup`.
pub(crate) fn resolve_code_import(
    src_dir: &Path,
    project_root: Option<&Path>,
    kind: &CodeImportKind,
    spec: &str,
    lookup: &ProjectLookup,
) -> Option<(String, RelationType, Option<String>)> {
    match kind {
        CodeImportKind::CInclude => {
            if let Some(id) = resolve_relative_path(src_dir, project_root, spec, lookup) {
                return Some((id, RelationType::CodeImport, Some(format!("include \"{spec}\""))));
            }
            if let Some(root) = project_root
                && let Some(id) = lookup.lookup_path(&root.join("include").join(spec))
            {
                return Some((
                    id.to_string(),
                    RelationType::CodeImport,
                    Some(format!("include \"{spec}\"")),
                ));
            }
            None
        }
        CodeImportKind::JsRelative => {
            resolve_js_specifier(src_dir, spec, lookup).map(|id| {
                (
                    id,
                    RelationType::CodeImport,
                    Some(format!("import {spec}")),
                )
            })
        }
        CodeImportKind::PyModule { level } => {
            resolve_python_module(src_dir, project_root, *level, spec, lookup).map(|id| {
                (
                    id,
                    RelationType::CodeImport,
                    Some(format!("import {spec}")),
                )
            })
        }
    }
}

fn extract_c_includes(text: &str, out: &mut Vec<RawRef>) {
    for line in text.lines() {
        let trimmed = line.trim_start();
        let Some(after_hash) = trimmed.strip_prefix('#') else {
            continue;
        };
        let Some(after_inc) = after_hash.trim_start().strip_prefix("include") else {
            continue;
        };
        let rest = after_inc.trim_start();
        if let Some(after_quote) = rest.strip_prefix('"')
            && let Some(end) = after_quote.find('"')
        {
            let target = after_quote[..end].trim();
            if !target.is_empty() {
                out.push(RawRef::CodeImport {
                    kind: CodeImportKind::CInclude,
                    spec: target.replace('\\', "/"),
                });
            }
        }
    }
}

fn extract_js_imports(text: &str, out: &mut Vec<RawRef>) {
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("//") || trimmed.starts_with('*') || trimmed.starts_with("/*") {
            continue;
        }
        if (trimmed.starts_with("import ") || trimmed.starts_with("export "))
            && let Some(spec) = extract_from_or_side_effect_spec(trimmed)
        {
            push_js_if_relative(spec, out);
        }
        for spec in extract_require_calls(trimmed) {
            push_js_if_relative(spec, out);
        }
    }
}

fn push_js_if_relative(spec: &str, out: &mut Vec<RawRef>) {
    if spec.starts_with("./") || spec.starts_with("../") {
        out.push(RawRef::CodeImport {
            kind: CodeImportKind::JsRelative,
            spec: spec.to_string(),
        });
    }
}

fn extract_from_or_side_effect_spec(line: &str) -> Option<&str> {
    if let Some((_, after_from)) = line.split_once(" from ") {
        return first_quoted_literal(after_from);
    }
    if line.starts_with("import ") {
        let rest = line.trim_start_matches("import").trim_start();
        if rest.starts_with('\'') || rest.starts_with('"') {
            return first_quoted_literal(rest);
        }
    }
    None
}

fn extract_require_calls(line: &str) -> Vec<&str> {
    let mut specs = Vec::new();
    let mut search = line;
    while let Some(pos) = search.find("require(") {
        let after = &search[pos + "require(".len()..];
        if let Some(lit) = first_quoted_literal(after.trim_start()) {
            specs.push(lit);
        }
        search = after;
    }
    specs
}

fn first_quoted_literal(s: &str) -> Option<&str> {
    let trimmed = s.trim_start();
    let quote = trimmed.chars().next().filter(|&c| c == '"' || c == '\'')?;
    let rest = &trimmed[1..];
    let end = rest.find(quote)?;
    Some(&rest[..end])
}

fn extract_python_imports(text: &str, out: &mut Vec<RawRef>) {
    for line in text.lines() {
        let code = line.split('#').next().unwrap_or("").trim();
        if code.is_empty() {
            continue;
        }
        if let Some(rest) = code.strip_prefix("from ")
            && let Some((module_part, _)) = rest.split_once(" import ")
        {
            let module_clean = module_part.trim();
            let level = module_clean.chars().take_while(|&c| c == '.').count();
            let mod_name = module_clean[level..].trim();
            if level > 0 || !mod_name.is_empty() {
                out.push(RawRef::CodeImport {
                    kind: CodeImportKind::PyModule { level },
                    spec: mod_name.to_string(),
                });
            }
        } else if let Some(rest) = code.strip_prefix("import ") {
            for item in rest.split(',') {
                let target = item.split_whitespace().next().unwrap_or("").trim();
                if !target.is_empty() {
                    out.push(RawRef::CodeImport {
                        kind: CodeImportKind::PyModule { level: 0 },
                        spec: target.to_string(),
                    });
                }
            }
        }
    }
}

fn resolve_js_specifier(src_dir: &Path, spec: &str, lookup: &ProjectLookup) -> Option<String> {
    let base = src_dir.join(spec);
    if let Some(id) = lookup.lookup_path(&base) {
        return Some(id.to_string());
    }
    for ext in ["ts", "tsx", "js", "jsx", "mjs", "cjs"] {
        let candidate = src_dir.join(format!("{spec}.{ext}"));
        if let Some(id) = lookup.lookup_path(&candidate) {
            return Some(id.to_string());
        }
    }
    for idx in ["index.ts", "index.tsx", "index.js", "index.jsx"] {
        let candidate = base.join(idx);
        if let Some(id) = lookup.lookup_path(&candidate) {
            return Some(id.to_string());
        }
    }
    None
}

fn resolve_python_module(
    src_dir: &Path,
    project_root: Option<&Path>,
    level: usize,
    spec: &str,
    lookup: &ProjectLookup,
) -> Option<String> {
    let rel_slashes = spec.replace('.', "/");
    if level > 0 {
        let mut base = src_dir.to_path_buf();
        for _ in 1..level {
            base = base.parent()?.to_path_buf();
        }
        return lookup_python_under(&base, &rel_slashes, lookup);
    }
    if let Some(root) = project_root {
        if let Some(id) = lookup_python_under(root, &rel_slashes, lookup) {
            return Some(id);
        }
        if let Some(id) = lookup_python_under(&root.join("src"), &rel_slashes, lookup) {
            return Some(id);
        }
    }
    lookup_python_under(src_dir, &rel_slashes, lookup)
}

fn lookup_python_under(base: &Path, rel: &str, lookup: &ProjectLookup) -> Option<String> {
    if rel.is_empty() {
        return lookup
            .lookup_path(&base.join("__init__.py"))
            .map(str::to_string);
    }
    let py_file = base.join(format!("{rel}.py"));
    if let Some(id) = lookup.lookup_path(&py_file) {
        return Some(id.to_string());
    }
    let init_file = base.join(rel).join("__init__.py");
    lookup.lookup_path(&init_file).map(str::to_string)
}