use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::error::VosError;
use crate::models::ObjectKind;

/// Returns the current Unix timestamp in seconds.
pub fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Generates a prefixed unique identifier.
pub fn new_id(prefix: &str) -> String {
    format!("{prefix}_{}", Uuid::new_v4().simple())
}

/// Canonicalizes a filesystem path and normalizes Windows verbatim prefixes (`\\?\`).
pub fn canonicalize_path(path: &Path) -> Result<String, VosError> {
    let canonical = std::fs::canonicalize(path)?;
    Ok(normalize_display_path(&canonical))
}

/// Converts a path to a clean UTF-8 string without Windows verbatim `\\?\` prefix.
pub fn normalize_display_path(path: &Path) -> String {
    let raw = path.to_string_lossy();
    if let Some(stripped) = raw.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{stripped}")
    } else if let Some(stripped) = raw.strip_prefix(r"\\?\") {
        stripped.to_string()
    } else {
        raw.into_owned()
    }
}

/// Returns true if `candidate` is equal to `root` or strictly inside `root`.
pub fn is_path_within(candidate: &str, root: &str) -> bool {
    let norm_cand = normalize_for_prefix(candidate);
    let norm_root = normalize_for_prefix(root);
    let root_trimmed = norm_root.trim_end_matches('/');

    if norm_cand == root_trimmed {
        return true;
    }

    norm_cand
        .strip_prefix(root_trimmed)
        .is_some_and(|rest| rest.starts_with('/'))
}

fn normalize_for_prefix(path: &str) -> String {
    let replaced = path.replace('\\', "/");
    if cfg!(windows) {
        replaced.to_lowercase()
    } else {
        replaced
    }
}

/// Classifies a file by extension into a VOS `ObjectKind`.
pub fn classify_path(path: &Path) -> ObjectKind {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase());

    match ext.as_deref() {
        Some(
            "rs" | "py" | "js" | "ts" | "tsx" | "jsx" | "c" | "h" | "cpp" | "hpp" | "cc" | "go"
            | "java" | "kt" | "swift" | "sh" | "bash" | "zsh" | "ps1" | "sql" | "toml" | "yaml"
            | "yml" | "json" | "html" | "css" | "scss" | "nix" | "lua" | "zig",
        ) => ObjectKind::Code,
        Some("md" | "markdown" | "txt" | "rst" | "org" | "adoc" | "log") => ObjectKind::Note,
        Some("pdf" | "docx" | "doc" | "odt" | "epub" | "pptx" | "xlsx" | "csv") => ObjectKind::Doc,
        Some("gguf" | "safetensors" | "onnx" | "ggml" | "pt" | "pth") => ObjectKind::Model,
        Some("png" | "jpg" | "jpeg" | "webp" | "gif" | "svg" | "mp3" | "wav" | "flac" | "mp4" | "mkv") => {
            ObjectKind::Media
        }
        _ => ObjectKind::Other,
    }
}

/// Computes the streaming SHA-256 hex digest of a file.
pub fn sha256_file(path: &Path) -> Result<String, VosError> {
    let file = File::open(path)?;
    let mut reader = BufReader::with_capacity(64 * 1024, file);
    let mut hasher = Sha256::new();
    let mut buf = [0_u8; 64 * 1024];

    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }

    let digest = hasher.finalize();
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(&mut hex, "{byte:02x}");
    }
    Ok(hex)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_recognizes_core_extensions() {
        assert_eq!(classify_path(Path::new("src/main.rs")), ObjectKind::Code);
        assert_eq!(classify_path(Path::new("SPEC.MD")), ObjectKind::Note);
        assert_eq!(classify_path(Path::new("paper.pdf")), ObjectKind::Doc);
        assert_eq!(classify_path(Path::new("qwen3.gguf")), ObjectKind::Model);
        assert_eq!(classify_path(Path::new("diagram.png")), ObjectKind::Media);
        assert_eq!(classify_path(Path::new("archive.tar.gz")), ObjectKind::Other);
    }

    #[test]
    fn path_prefix_respects_directory_boundaries() {
        assert!(is_path_within("/work/vos/src/lib.rs", "/work/vos"));
        assert!(is_path_within("/work/vos", "/work/vos"));
        assert!(!is_path_within("/work/vos2/src/lib.rs", "/work/vos"));
    }
}
