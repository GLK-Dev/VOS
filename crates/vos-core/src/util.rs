use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::error::VosError;
use crate::models::{ObjectKind, SearchVisibility};

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
    if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
        let lower = name.to_ascii_lowercase();
        if lower == ".env"
            || lower.starts_with(".env.")
            || matches!(
                lower.as_str(),
                "cmakelists.txt" | "requirements.txt" | "constraints.txt" | "robots.txt"
            )
        {
            return ObjectKind::Config;
        }
        if lower.ends_with("_output.txt")
            || lower.ends_with("_results.txt")
            || lower.ends_with("_log.txt")
        {
            return ObjectKind::Other;
        }
    }

    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase());

    match ext.as_deref() {
        Some(
            "rs" | "py" | "js" | "ts" | "tsx" | "jsx" | "c" | "h" | "cpp" | "hpp" | "cc" | "go"
            | "java" | "kt" | "swift" | "sh" | "bash" | "zsh" | "ps1" | "bat" | "cmd" | "sql"
            | "html" | "css" | "scss" | "nix" | "lua" | "zig",
        ) => ObjectKind::Code,
        Some("json" | "yaml" | "yml" | "toml" | "ini" | "env" | "conf" | "properties") => {
            ObjectKind::Config
        }
        Some("md" | "markdown" | "txt" | "rst" | "org" | "adoc") => ObjectKind::Note,
        Some("log") => ObjectKind::Other,
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


const FAST_FINGERPRINT_THRESHOLD: u64 = 16 * 1024 * 1024;
const SAMPLE_WINDOW_BYTES: usize = 64 * 1024;

/// Computes a deterministic content fingerprint: full SHA-256 up to 16 MiB,
/// or a fast sampled SHA-256 (size + head/middle/tail 64 KiB windows) for larger blobs.
pub fn fingerprint_file(path: &Path, size_bytes: u64) -> Result<String, VosError> {
    if size_bytes <= FAST_FINGERPRINT_THRESHOLD {
        return sha256_file(path);
    }

    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    hasher.update(size_bytes.to_le_bytes());

    let mut buf = vec![0_u8; SAMPLE_WINDOW_BYTES];
    let offsets = [
        0_u64,
        size_bytes / 2,
        size_bytes.saturating_sub(SAMPLE_WINDOW_BYTES as u64),
    ];

    for offset in offsets {
        file.seek(SeekFrom::Start(offset))?;
        let n = file.read(&mut buf)?;
        hasher.update(&buf[..n]);
    }

    let digest = hasher.finalize();
    let mut hex = String::with_capacity(5 + digest.len() * 2);
    hex.push_str("fast:");
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(&mut hex, "{byte:02x}");
    }
    Ok(hex)
}

/// Returns true for lockfiles and VCS/editor housekeeping files that add noise to `vos context`.
pub fn is_noise_path(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
        return false;
    };

    matches!(
        name,
        ".gitignore"
            | ".gitattributes"
            | ".gitmodules"
            | ".editorconfig"
            | ".DS_Store"
            | "Thumbs.db"
            | "Cargo.lock"
            | "package-lock.json"
            | "pnpm-lock.yaml"
            | "yarn.lock"
            | "bun.lockb"
            | "poetry.lock"
            | "uv.lock"
            | "flake.lock"
    )
}

/// Classifies internal search/benchmark configuration artifacts separately from project knowledge.
pub fn search_visibility(path: &Path) -> SearchVisibility {
    let norm = path.to_string_lossy().replace('\\', "/").to_lowercase();
    let is_yaml = norm.ends_with(".yaml") || norm.ends_with(".yml");

    if (norm.contains("/config/") && is_yaml)
        || (norm.contains("/tests/") && is_yaml)
        || norm.contains("/benchmarks/")
        || norm.contains("/tests/fixtures/")
    {
        SearchVisibility::Internal
    } else {
        SearchVisibility::Normal
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_recognizes_core_extensions() {
        assert_eq!(classify_path(Path::new("src/main.rs")), ObjectKind::Code);
        assert_eq!(classify_path(Path::new("init.bat")), ObjectKind::Code);
        assert_eq!(classify_path(Path::new("run.cmd")), ObjectKind::Code);
        assert_eq!(classify_path(Path::new("Cargo.toml")), ObjectKind::Config);
        assert_eq!(classify_path(Path::new("locales/en.json")), ObjectKind::Config);
        assert_eq!(classify_path(Path::new(".github/workflows/ci.yml")), ObjectKind::Config);
        assert_eq!(classify_path(Path::new("SPEC.MD")), ObjectKind::Note);
        assert_eq!(classify_path(Path::new("paper.pdf")), ObjectKind::Doc);
        assert_eq!(classify_path(Path::new("qwen3.gguf")), ObjectKind::Model);
        assert_eq!(classify_path(Path::new("diagram.png")), ObjectKind::Media);
        assert_eq!(classify_path(Path::new("archive.tar.gz")), ObjectKind::Other);
        assert_eq!(classify_path(Path::new("build.log")), ObjectKind::Other);
        assert_eq!(classify_path(Path::new("benchmark_output.txt")), ObjectKind::Other);
        assert_eq!(classify_path(Path::new("requirements.txt")), ObjectKind::Config);
    }

    #[test]
    fn path_prefix_respects_directory_boundaries() {
        assert!(is_path_within("/work/vos/src/lib.rs", "/work/vos"));
        assert!(is_path_within("/work/vos", "/work/vos"));
        assert!(!is_path_within("/work/vos2/src/lib.rs", "/work/vos"));
    }

    #[test]
    fn noise_detection_matches_lockfiles_and_vcs_files() {
        assert!(is_noise_path(Path::new("/repo/.gitignore")));
        assert!(is_noise_path(Path::new("/repo/Cargo.lock")));
        assert!(!is_noise_path(Path::new("/repo/Cargo.toml")));
        assert!(!is_noise_path(Path::new("/repo/ROADMAP.md")));
    }

    #[test]
    fn visibility_marks_search_configs_and_benchmarks_internal() {
        assert_eq!(
            search_visibility(Path::new("/repo/config/concepts.yaml")),
            SearchVisibility::Internal
        );
        assert_eq!(
            search_visibility(Path::new("/repo/tests/search_live.yaml")),
            SearchVisibility::Internal
        );
        assert_eq!(
            search_visibility(Path::new("/repo/SPEC.md")),
            SearchVisibility::Normal
        );
    }
}




