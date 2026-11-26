//! Project bootstrap (`vos init`) combining registration, initial indexing, and MCP config setup.

use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::{Map, Value, json};
use vos_core::util::canonicalize_path;
use vos_core::{Project, VosError};
use vos_store::{Store, StoreError};

use crate::{IndexOptions, IndexReport, IndexerError, index_path_with_options};

/// Options controlling idempotent project initialization in `init_project`.
#[derive(Debug, Clone)]
pub struct InitOptions<'a> {
    pub path: &'a Path,
    pub name: Option<&'a str>,
    pub description: Option<&'a str>,
    pub index: bool,
    pub no_ignore: bool,
    pub setup_mcp: bool,
    pub force: bool,
}

/// Result status for an individual MCP configuration file created or updated by `vos init`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct McpConfigStatus {
    pub path: String,
    pub status: String,
}

/// Complete report returned by `init_project`.
#[derive(Debug, Clone)]
pub struct InitReport {
    pub project: Project,
    pub project_created: bool,
    pub index_report: Option<IndexReport>,
    pub mcp_configs: Vec<McpConfigStatus>,
}

/// Idempotently registers a project directory, indexes it, and scaffolds `.mcp.json` and `.vscode/mcp.json`.
pub fn init_project(store: &mut Store, opts: InitOptions<'_>) -> Result<InitReport, IndexerError> {
    if !opts.path.exists() {
        return Err(VosError::InvalidInput(format!(
            "target path does not exist: {}",
            opts.path.display()
        ))
        .into());
    }
    if !opts.path.is_dir() {
        return Err(VosError::InvalidInput(format!(
            "init target must be a directory: {}",
            opts.path.display()
        ))
        .into());
    }

    let canonical = canonicalize_path(opts.path)?;
    let root_path = PathBuf::from(&canonical);

    let (project, project_created) = resolve_or_create_project(
        store,
        &root_path,
        &canonical,
        opts.name,
        opts.description,
    )?;

    let mut mcp_configs = Vec::new();
    if opts.setup_mcp {
        mcp_configs.push(ensure_mcp_config(
            &root_path.join(".mcp.json"),
            "mcpServers",
            false,
            opts.force,
        )?);
        mcp_configs.push(ensure_mcp_config(
            &root_path.join(".vscode").join("mcp.json"),
            "servers",
            true,
            opts.force,
        )?);
    }

    let index_report = if opts.index {
        Some(index_path_with_options(
            store,
            &root_path,
            Some(&project.name),
            IndexOptions {
                no_ignore: opts.no_ignore,
            },
        )?)
    } else {
        None
    };

    Ok(InitReport {
        project,
        project_created,
        index_report,
        mcp_configs,
    })
}

fn resolve_or_create_project(
    store: &Store,
    root_path: &Path,
    canonical_root: &str,
    explicit_name: Option<&str>,
    description: Option<&str>,
) -> Result<(Project, bool), IndexerError> {
    if let Some(raw_name) = explicit_name {
        let trimmed = raw_name.trim();
        if trimmed.is_empty() {
            return Err(VosError::InvalidInput("project name cannot be empty".into()).into());
        }
        if let Some(existing) = store.get_project_by_name(trimmed)? {
            return reuse_matching_project(store, existing, root_path, canonical_root, description);
        }
        let created = store.create_project(trimmed, Some(root_path), description)?;
        return Ok((created, true));
    }

    if let Some(by_path) = store.find_project_for_path(canonical_root)?
        && by_path
            .root_path
            .as_deref()
            .is_some_and(|p| p.eq_ignore_ascii_case(canonical_root))
    {
        return reuse_matching_project(store, by_path, root_path, canonical_root, description);
    }

    let derived = derive_project_name(root_path);
    if let Some(existing) = store.get_project_by_name(&derived)? {
        if existing
            .root_path
            .as_deref()
            .is_some_and(|p| !p.eq_ignore_ascii_case(canonical_root))
        {
            return Err(VosError::InvalidInput(format!(
                "derived project name '{derived}' is already bound to {}; pass --name to pick a distinct name",
                existing.root_path.as_deref().unwrap_or("(none)")
            ))
            .into());
        }
        return reuse_matching_project(store, existing, root_path, canonical_root, description);
    }

    let created = store.create_project(&derived, Some(root_path), description)?;
    Ok((created, true))
}

fn reuse_matching_project(
    store: &Store,
    existing: Project,
    root_path: &Path,
    canonical_root: &str,
    description: Option<&str>,
) -> Result<(Project, bool), IndexerError> {
    match existing.root_path.as_deref() {
        Some(bound) if !bound.eq_ignore_ascii_case(canonical_root) => Err(StoreError::InvalidInput(
            format!(
                "project '{}' is already bound to {bound}; pass --name or update the project path",
                existing.name
            ),
        )
        .into()),
        None => {
            let updated =
                store.update_project(&existing.name, None, Some(root_path), description)?;
            Ok((updated, false))
        }
        Some(_) => {
            if let Some(desc) = description {
                let updated = store.update_project(&existing.name, None, None, Some(desc))?;
                return Ok((updated, false));
            }
            Ok((existing, false))
        }
    }
}

/// Derives a sanitized lowercase project slug from a directory name.
pub fn derive_project_name(root: &Path) -> String {
    let raw = root
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("project");
    let mut slug = String::with_capacity(raw.len());
    let mut prev_dash = false;
    for ch in raw.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' {
            slug.push(ch.to_ascii_lowercase());
            prev_dash = false;
        } else if !prev_dash && !slug.is_empty() {
            slug.push('-');
            prev_dash = true;
        }
    }
    while slug.ends_with('-') {
        slug.pop();
    }
    if slug.is_empty() {
        "project".to_string()
    } else {
        slug
    }
}

fn ensure_mcp_config(
    config_path: &Path,
    root_key: &str,
    include_stdio_type: bool,
    force: bool,
) -> Result<McpConfigStatus, IndexerError> {
    let entry = if include_stdio_type {
        json!({
            "type": "stdio",
            "command": "vos",
            "args": ["mcp"]
        })
    } else {
        json!({
            "command": "vos",
            "args": ["mcp"]
        })
    };

    if let Some(parent) = config_path.parent() {
        fs::create_dir_all(parent)?;
    }

    let path_str = config_path.to_string_lossy().into_owned();
    if !config_path.exists() {
        let mut root = Map::new();
        let mut servers = Map::new();
        servers.insert("vos".to_string(), entry);
        root.insert(root_key.to_string(), Value::Object(servers));
        write_pretty_json(config_path, &Value::Object(root))?;
        return Ok(McpConfigStatus {
            path: path_str,
            status: "created".to_string(),
        });
    }

    let raw = fs::read_to_string(config_path)?;
    let mut doc: Value = match serde_json::from_str(&raw) {
        Ok(Value::Object(map)) => Value::Object(map),
        _ if force => {
            let mut root = Map::new();
            let mut servers = Map::new();
            servers.insert("vos".to_string(), entry);
            root.insert(root_key.to_string(), Value::Object(servers));
            write_pretty_json(config_path, &Value::Object(root))?;
            return Ok(McpConfigStatus {
                path: path_str,
                status: "updated".to_string(),
            });
        }
        _ => {
            return Ok(McpConfigStatus {
                path: path_str,
                status: "skipped (non-object or invalid JSON; pass --force to overwrite)"
                    .to_string(),
            });
        }
    };

    let Some(root_obj) = doc.as_object_mut() else {
        return Ok(McpConfigStatus {
            path: path_str,
            status: "skipped".to_string(),
        });
    };

    let servers_val = root_obj
        .entry(root_key.to_string())
        .or_insert_with(|| Value::Object(Map::new()));
    let Some(servers_obj) = servers_val.as_object_mut() else {
        if !force {
            return Ok(McpConfigStatus {
                path: path_str,
                status: format!("skipped ('{root_key}' is not a JSON object; pass --force)"),
            });
        }
        let mut fresh = Map::new();
        fresh.insert("vos".to_string(), entry);
        *servers_val = Value::Object(fresh);
        write_pretty_json(config_path, &doc)?;
        return Ok(McpConfigStatus {
            path: path_str,
            status: "updated".to_string(),
        });
    };

    if servers_obj.contains_key("vos") && !force {
        return Ok(McpConfigStatus {
            path: path_str,
            status: "unchanged".to_string(),
        });
    }

    servers_obj.insert("vos".to_string(), entry);
    write_pretty_json(config_path, &doc)?;
    Ok(McpConfigStatus {
        path: path_str,
        status: "updated".to_string(),
    })
}

fn write_pretty_json(path: &Path, value: &Value) -> Result<(), IndexerError> {
    let encoded = serde_json::to_string_pretty(value)
        .map_err(|e| VosError::InvalidInput(format!("failed to serialize JSON: {e}")))?;
    fs::write(path, format!("{encoded}\n"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;

    #[test]
    fn init_project_is_idempotent_and_preserves_existing_mcp_servers() {
        let tmp = TempDir::new().unwrap();
        let proj = tmp.path().join("My Cool App");
        fs::create_dir_all(proj.join("src")).unwrap();
        fs::write(proj.join("README.md"), "# My Cool App\n").unwrap();
        fs::write(proj.join("src/lib.rs"), "pub fn ok() {}\n").unwrap();
        fs::write(
            proj.join(".mcp.json"),
            r#"{"mcpServers":{"other":{"command":"echo"}}}"#,
        )
        .unwrap();

        let mut store = Store::open_in_memory().unwrap();
        let first = init_project(
            &mut store,
            InitOptions {
                path: &proj,
                name: None,
                description: Some("First init"),
                index: true,
                no_ignore: false,
                setup_mcp: true,
                force: false,
            },
        )
        .unwrap();

        assert!(first.project_created);
        assert_eq!(first.project.name, "my-cool-app");
        assert_eq!(first.index_report.unwrap().added, 3);
        assert_eq!(first.mcp_configs[0].status, "updated");
        assert_eq!(first.mcp_configs[1].status, "created");

        let mcp_val: Value =
            serde_json::from_str(&fs::read_to_string(proj.join(".mcp.json")).unwrap()).unwrap();
        assert_eq!(mcp_val["mcpServers"]["other"]["command"], "echo");
        assert_eq!(mcp_val["mcpServers"]["vos"]["command"], "vos");

        let second = init_project(
            &mut store,
            InitOptions {
                path: &proj,
                name: None,
                description: None,
                index: true,
                no_ignore: false,
                setup_mcp: true,
                force: false,
            },
        )
        .unwrap();
        assert!(!second.project_created);
        assert_eq!(second.index_report.unwrap().unchanged, 3);
        assert_eq!(second.mcp_configs[0].status, "unchanged");
        assert_eq!(second.mcp_configs[1].status, "unchanged");
    }

    #[test]
    fn init_project_skips_invalid_mcp_json_without_force() {
        let tmp = TempDir::new().unwrap();
        let proj = tmp.path().join("broken");
        fs::create_dir_all(&proj).unwrap();
        fs::write(proj.join(".mcp.json"), "{not-json").unwrap();

        let mut store = Store::open_in_memory().unwrap();
        let rep = init_project(
            &mut store,
            InitOptions {
                path: &proj,
                name: Some("broken"),
                description: None,
                index: false,
                no_ignore: false,
                setup_mcp: true,
                force: false,
            },
        )
        .unwrap();
        assert!(rep.mcp_configs[0].status.starts_with("skipped"));
        assert_eq!(fs::read_to_string(proj.join(".mcp.json")).unwrap(), "{not-json");
    }
}