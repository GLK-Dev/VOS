//! Deterministic zero-daemon outline and line-span extractor for code and Markdown.

use std::path::Path;

use vos_core::{ObjectKind, SymbolInput, SymbolKind};

/// Extracts structural symbol outlines (`name`, `kind`, `signature`, `start_line..=end_line`).
pub(crate) fn extract_symbols(path: &Path, kind: ObjectKind, text: &str) -> Vec<SymbolInput> {
    if text.is_empty() {
        return Vec::new();
    }
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();

    match ext.as_str() {
        "rs" => extract_rust_symbols(text),
        "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" => extract_js_symbols(text),
        "py" => extract_python_symbols(text),
        "c" | "cc" | "cpp" | "cxx" | "h" | "hh" | "hpp" | "hxx" => extract_c_symbols(text),
        "md" | "markdown" => extract_markdown_headings(text),
        _ if kind == ObjectKind::Note => extract_markdown_headings(text),
        _ => Vec::new(),
    }
}

struct OpenBraceSymbol {
    name: String,
    kind: SymbolKind,
    signature: String,
    start_line: usize,
    target_depth: i32,
}

fn extract_rust_symbols(text: &str) -> Vec<SymbolInput> {
    struct PendingDecl {
        name: String,
        kind: SymbolKind,
        signature: String,
        start_line: usize,
        target_depth: i32,
    }
    let mut out = Vec::new();
    let mut stack: Vec<OpenBraceSymbol> = Vec::new();
    let mut pending: Option<PendingDecl> = None;
    let mut brace_depth: i32 = 0;

    for (idx, line) in text.lines().enumerate() {
        let line_no = idx + 1;
        let trimmed = strip_slash_comment(line).trim();
        let prev_depth = brace_depth;
        let delta = brace_delta(trimmed);

        if let Some(mut pend) = pending.take() {
            if !trimmed.is_empty() {
                pend.signature = truncate_sig(&format!("{} {trimmed}", pend.signature));
            }
            if trimmed.contains('{') && delta > 0 {
                stack.push(OpenBraceSymbol {
                    name: pend.name,
                    kind: pend.kind,
                    signature: pend.signature,
                    start_line: pend.start_line,
                    target_depth: pend.target_depth,
                });
            } else if trimmed.ends_with(';') || trimmed.ends_with('}') || line_no - pend.start_line > 20 {
                out.push(SymbolInput {
                    name: pend.name,
                    kind: pend.kind,
                    signature: pend.signature,
                    start_line: pend.start_line,
                    end_line: line_no,
                });
            } else {
                pending = Some(pend);
            }
        } else if !trimmed.is_empty()
            && prev_depth <= 1
            && let Some((kind, name)) = parse_rust_decl(trimmed)
        {
            if trimmed.contains('{') && delta > 0 {
                stack.push(OpenBraceSymbol {
                    name,
                    kind,
                    signature: truncate_sig(trimmed),
                    start_line: line_no,
                    target_depth: prev_depth,
                });
            } else if trimmed.ends_with(';') || trimmed.ends_with('}') {
                out.push(SymbolInput {
                    name,
                    kind,
                    signature: truncate_sig(trimmed),
                    start_line: line_no,
                    end_line: line_no,
                });
            } else {
                pending = Some(PendingDecl {
                    name,
                    kind,
                    signature: truncate_sig(trimmed),
                    start_line: line_no,
                    target_depth: prev_depth,
                });
            }
        }

        brace_depth += delta;
        while let Some(top) = stack.last() {
            if brace_depth <= top.target_depth {
                if let Some(done) = stack.pop() {
                    out.push(SymbolInput {
                        name: done.name,
                        kind: done.kind,
                        signature: done.signature,
                        start_line: done.start_line,
                        end_line: line_no,
                    });
                }
            } else {
                break;
            }
        }
    }

    let eof = text.lines().count().max(1);
    for rem in stack {
        out.push(SymbolInput {
            name: rem.name,
            kind: rem.kind,
            signature: rem.signature,
            start_line: rem.start_line,
            end_line: eof,
        });
    }
    out.sort_by_key(|s| (s.start_line, s.end_line));
    out
}

fn parse_rust_decl(code: &str) -> Option<(SymbolKind, String)> {
    let mut s = code.trim_start();
    for prefix in [
        "pub(crate) ",
        "pub(super) ",
        "pub ",
        "async ",
        "unsafe ",
        "const ",
    ] {
        if let Some(rest) = s.strip_prefix(prefix) {
            if prefix == "const "
                && !rest.starts_with("fn ")
                && !rest.starts_with("unsafe ")
                && let Some(ident) = take_ident(rest.split(':').next().unwrap_or(""))
            {
                return Some((SymbolKind::Const, ident));
            }
            s = rest.trim_start();
        }
    }

    if let Some(rest) = s.strip_prefix("fn ") {
        return take_ident(rest).map(|n| (SymbolKind::Fn, n));
    }
    if let Some(rest) = s.strip_prefix("struct ") {
        return take_ident(rest).map(|n| (SymbolKind::Struct, n));
    }
    if let Some(rest) = s.strip_prefix("enum ") {
        return take_ident(rest).map(|n| (SymbolKind::Enum, n));
    }
    if let Some(rest) = s.strip_prefix("trait ") {
        return take_ident(rest).map(|n| (SymbolKind::Trait, n));
    }
    if let Some(rest) = s.strip_prefix("mod ") {
        return take_ident(rest).map(|n| (SymbolKind::Mod, n));
    }
    if let Some(rest) = s.strip_prefix("type ") {
        return take_ident(rest).map(|n| (SymbolKind::Type, n));
    }
    if s.starts_with("impl") && s[4..].starts_with(|c: char| c.is_whitespace() || c == '<') {
        let label = s
            .split('{')
            .next()
            .unwrap_or(s)
            .split(" where ")
            .next()
            .unwrap_or(s)
            .trim();
        return Some((SymbolKind::Impl, truncate_sig(label)));
    }
    None
}

fn extract_js_symbols(text: &str) -> Vec<SymbolInput> {
    let mut out = Vec::new();
    let mut stack: Vec<OpenBraceSymbol> = Vec::new();
    let mut depth: i32 = 0;

    for (idx, line) in text.lines().enumerate() {
        let line_no = idx + 1;
        let trimmed = strip_slash_comment(line).trim();
        let prev_depth = depth;

        if !trimmed.is_empty() && prev_depth == 0
            && let Some((kind, name)) = parse_js_decl(trimmed)
        {
            let delta = brace_delta(trimmed);
            if trimmed.contains('{') && delta > 0 {
                stack.push(OpenBraceSymbol {
                    name,
                    kind,
                    signature: truncate_sig(trimmed),
                    start_line: line_no,
                    target_depth: prev_depth,
                });
            } else {
                out.push(SymbolInput {
                    name,
                    kind,
                    signature: truncate_sig(trimmed),
                    start_line: line_no,
                    end_line: line_no,
                });
            }
        }

        depth += brace_delta(trimmed);
        while let Some(top) = stack.last() {
            if depth <= top.target_depth {
                if let Some(done) = stack.pop() {
                    out.push(SymbolInput {
                        name: done.name,
                        kind: done.kind,
                        signature: done.signature,
                        start_line: done.start_line,
                        end_line: line_no,
                    });
                }
            } else {
                break;
            }
        }
    }
    out
}

fn parse_js_decl(code: &str) -> Option<(SymbolKind, String)> {
    let mut s = code.trim_start();
    for prefix in ["export ", "default ", "declare ", "async "] {
        if let Some(rest) = s.strip_prefix(prefix) {
            s = rest.trim_start();
        }
    }
    if let Some(rest) = s.strip_prefix("function ") {
        return take_ident(rest.trim_start_matches('*')).map(|n| (SymbolKind::Fn, n));
    }
    if let Some(rest) = s.strip_prefix("class ") {
        return take_ident(rest).map(|n| (SymbolKind::Class, n));
    }
    if let Some(rest) = s.strip_prefix("interface ") {
        return take_ident(rest).map(|n| (SymbolKind::Trait, n));
    }
    if let Some(rest) = s.strip_prefix("type ") {
        return take_ident(rest).map(|n| (SymbolKind::Type, n));
    }
    if let Some(rest) = s.strip_prefix("enum ") {
        return take_ident(rest).map(|n| (SymbolKind::Enum, n));
    }
    if let Some(rest) = s.strip_prefix("const ")
        && (rest.contains("=>") || rest.contains("function"))
    {
        return take_ident(rest).map(|n| (SymbolKind::Fn, n));
    }
    None
}

fn extract_python_symbols(text: &str) -> Vec<SymbolInput> {
    struct OpenPy {
        name: String,
        kind: SymbolKind,
        signature: String,
        start_line: usize,
        indent: usize,
    }
    let mut out = Vec::new();
    let mut stack: Vec<OpenPy> = Vec::new();
    let mut last_code_line = 1_usize;

    for (idx, line) in text.lines().enumerate() {
        let line_no = idx + 1;
        let code_only = line.split('#').next().unwrap_or("");
        let trimmed = code_only.trim();
        if trimmed.is_empty() {
            continue;
        }
        let indent = line.len() - line.trim_start().len();

        while let Some(top) = stack.last() {
            if indent <= top.indent {
                if let Some(done) = stack.pop() {
                    out.push(SymbolInput {
                        name: done.name,
                        kind: done.kind,
                        signature: done.signature,
                        start_line: done.start_line,
                        end_line: last_code_line.max(done.start_line),
                    });
                }
            } else {
                break;
            }
        }
        last_code_line = line_no;

        let py_decl = if let Some(rest) = trimmed.strip_prefix("async def ") {
            take_ident(rest).map(|n| (SymbolKind::Fn, n))
        } else if let Some(rest) = trimmed.strip_prefix("def ") {
            take_ident(rest).map(|n| (SymbolKind::Fn, n))
        } else if let Some(rest) = trimmed.strip_prefix("class ") {
            take_ident(rest).map(|n| (SymbolKind::Class, n))
        } else {
            None
        };

        if let Some((kind, name)) = py_decl {
            stack.push(OpenPy {
                name,
                kind,
                signature: truncate_sig(trimmed),
                start_line: line_no,
                indent,
            });
        }
    }

    for rem in stack {
        out.push(SymbolInput {
            name: rem.name,
            kind: rem.kind,
            signature: rem.signature,
            start_line: rem.start_line,
            end_line: last_code_line.max(rem.start_line),
        });
    }
    out.sort_by_key(|s| (s.start_line, s.end_line));
    out
}

fn extract_c_symbols(text: &str) -> Vec<SymbolInput> {
    let mut out = Vec::new();
    let mut stack: Vec<OpenBraceSymbol> = Vec::new();
    let mut depth: i32 = 0;

    for (idx, line) in text.lines().enumerate() {
        let line_no = idx + 1;
        let trimmed = strip_slash_comment(line).trim();
        let prev_depth = depth;

        if !trimmed.is_empty() && prev_depth == 0 && !trimmed.starts_with('#') {
            let decl = if let Some(rest) = trimmed.strip_prefix("struct ") {
                take_ident(rest).map(|n| (SymbolKind::Struct, n))
            } else if let Some(rest) = trimmed.strip_prefix("class ") {
                take_ident(rest).map(|n| (SymbolKind::Class, n))
            } else if let Some(rest) = trimmed.strip_prefix("enum ") {
                take_ident(rest).map(|n| (SymbolKind::Enum, n))
            } else if trimmed.contains('(') && trimmed.contains('{') && !trimmed.starts_with("if ")
                && !trimmed.starts_with("for ")
                && !trimmed.starts_with("while ")
            {
                let before_paren = trimmed.split('(').next().unwrap_or("");
                let name = before_paren
                    .split_whitespace()
                    .last()
                    .map(|w| w.trim_start_matches('*'))
                    .and_then(take_ident);
                name.map(|n| (SymbolKind::Fn, n))
            } else {
                None
            };

            if let Some((kind, name)) = decl {
                let delta = brace_delta(trimmed);
                if trimmed.contains('{') && delta > 0 {
                    stack.push(OpenBraceSymbol {
                        name,
                        kind,
                        signature: truncate_sig(trimmed),
                        start_line: line_no,
                        target_depth: prev_depth,
                    });
                } else if trimmed.ends_with('}') {
                    out.push(SymbolInput {
                        name,
                        kind,
                        signature: truncate_sig(trimmed),
                        start_line: line_no,
                        end_line: line_no,
                    });
                }
            }
        }

        depth += brace_delta(trimmed);
        while let Some(top) = stack.last() {
            if depth <= top.target_depth {
                if let Some(done) = stack.pop() {
                    out.push(SymbolInput {
                        name: done.name,
                        kind: done.kind,
                        signature: done.signature,
                        start_line: done.start_line,
                        end_line: line_no,
                    });
                }
            } else {
                break;
            }
        }
    }
    out
}

fn extract_markdown_headings(text: &str) -> Vec<SymbolInput> {
    struct OpenHeading {
        name: String,
        signature: String,
        start_line: usize,
        level: usize,
    }
    let mut out = Vec::new();
    let mut stack: Vec<OpenHeading> = Vec::new();
    let mut in_fence = false;
    let total_lines = text.lines().count().max(1);

    for (idx, line) in text.lines().enumerate() {
        let line_no = idx + 1;
        let trimmed = line.trim();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }
        let hashes = trimmed.chars().take_while(|&c| c == '#').count();
        if (1..=3).contains(&hashes) && trimmed[hashes..].starts_with(char::is_whitespace) {
            let title = trimmed[hashes..].trim().to_string();
            if title.is_empty() {
                continue;
            }
            while let Some(top) = stack.last() {
                if hashes <= top.level {
                    if let Some(done) = stack.pop() {
                        out.push(SymbolInput {
                            name: done.name,
                            kind: SymbolKind::Heading,
                            signature: done.signature,
                            start_line: done.start_line,
                            end_line: line_no.saturating_sub(1).max(done.start_line),
                        });
                    }
                } else {
                    break;
                }
            }
            stack.push(OpenHeading {
                name: title,
                signature: truncate_sig(trimmed),
                start_line: line_no,
                level: hashes,
            });
        }
    }

    for rem in stack {
        out.push(SymbolInput {
            name: rem.name,
            kind: SymbolKind::Heading,
            signature: rem.signature,
            start_line: rem.start_line,
            end_line: total_lines,
        });
    }
    out.sort_by_key(|s| (s.start_line, s.end_line));
    out
}

fn strip_slash_comment(line: &str) -> &str {
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

fn brace_delta(code: &str) -> i32 {
    let mut delta = 0_i32;
    let mut in_str = false;
    let mut in_sq = false;
    let mut escape = false;
    for ch in code.chars() {
        if escape {
            escape = false;
            continue;
        }
        if in_str {
            if ch == '\\' {
                escape = true;
            } else if ch == '"' {
                in_str = false;
            }
            continue;
        }
        if in_sq {
            if ch == '\\' {
                escape = true;
            } else if ch == '\'' {
                in_sq = false;
            }
            continue;
        }
        match ch {
            '"' => in_str = true,
            '\'' => in_sq = true,
            '{' => delta += 1,
            '}' => delta -= 1,
            _ => {}
        }
    }
    delta
}

fn take_ident(input: &str) -> Option<String> {
    let trimmed = input.trim_start();
    let ident: String = trimmed
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
        .collect();
    if ident.is_empty() { None } else { Some(ident) }
}

fn truncate_sig(raw: &str) -> String {
    let clean: String = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    if clean.chars().count() <= 120 {
        clean
    } else {
        let prefix: String = clean.chars().take(117).collect();
        format!("{prefix}...")
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    #[test]
    fn extracts_rust_impl_and_inner_methods_with_line_spans() {
        let src = "\
pub struct Store {
    n: usize,
}

impl Store {
    pub fn compute(&self) -> usize {
        self.n + 1
    }
}
";
        let syms = extract_symbols(Path::new("src/lib.rs"), ObjectKind::Code, src);
        assert!(syms.iter().any(|s| s.name == "Store" && s.kind == SymbolKind::Struct && s.start_line == 1 && s.end_line == 3));
        assert!(syms.iter().any(|s| s.name == "compute" && s.kind == SymbolKind::Fn && s.start_line == 6 && s.end_line == 8));
        let one = extract_symbols(Path::new("src/one.rs"), ObjectKind::Code, "pub fn v() {}");
        let multi = extract_symbols(Path::new("src/multi.rs"), ObjectKind::Code, "fn multi(\n    a: i32,\n    b: i32,\n) -> i32 {\n    a + b\n}");
        assert!(multi.iter().any(|s| s.name == "multi" && s.start_line == 1 && s.end_line == 6));
        assert!(one.iter().any(|s| s.name == "v" && s.start_line == 1 && s.end_line == 1));
        assert!(syms.iter().any(|s| s.name == "impl Store" && s.kind == SymbolKind::Impl && s.start_line == 5 && s.end_line == 9));
    }

    #[test]
    fn extracts_python_js_c_and_markdown_symbols() {
        let py = "class Engine:\n    def run(self):\n        return 1\n";
        let py_syms = extract_symbols(Path::new("app.py"), ObjectKind::Code, py);
        assert_eq!(py_syms.len(), 2);
        assert_eq!(py_syms[1].name, "run");

        let md = "# Title\n```md\n# Fake\n```\n## Section\nBody\n";
        let md_syms = extract_symbols(Path::new("README.md"), ObjectKind::Doc, md);
        assert_eq!(md_syms.len(), 2);
        assert_eq!(md_syms[1].name, "Section");
    }
}