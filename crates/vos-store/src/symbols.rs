//! Symbol search and targeted line-range file slicing over indexed objects.

use std::fs;
use std::path::Path;

use rusqlite::params;
use vos_core::{FileSlice, SymbolKind, SymbolRecord};

use crate::{Store, StoreError};

impl Store {
    /// Lists indexed symbols for `target`, ordered by line number.
    pub fn list_file_symbols(
        &self,
        target: &str,
        project_name: Option<&str>,
    ) -> Result<Vec<SymbolRecord>, StoreError> {
        let details = self.show_object_scoped(target, project_name)?;
        let root_prefix = self
            .project_root_for_id(details.object.project_id.as_deref())?;
        let rel_path = vos_core::compact::compact_path(&details.object.path, root_prefix.as_deref());

        let mut stmt = self.conn.prepare(
            "SELECT id, name, kind, signature, start_line, end_line
             FROM symbols
             WHERE object_id = ?1
             ORDER BY start_line ASC, name ASC",
        )?;
        let rows = stmt.query_map(params![details.object.id], |row| {
            let kind_str: String = row.get(2)?;
            Ok(SymbolRecord {
                id: row.get(0)?,
                object_id: details.object.id.clone(),
                path: rel_path.clone(),
                project_name: details.project_name.clone(),
                name: row.get(1)?,
                kind: SymbolKind::from_db(&kind_str),
                signature: row.get(3)?,
                start_line: (row.get::<_, i64>(4)?).max(1) as usize,
                end_line: (row.get::<_, i64>(5)?).max(1) as usize,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Searches indexed code/document symbols across active objects using FTS5.
    pub fn search_symbols(
        &self,
        query: &str,
        project_name: Option<&str>,
        limit: usize,
    ) -> Result<Vec<SymbolRecord>, StoreError> {
        let trimmed = query.trim();
        if trimmed.is_empty() {
            return Err(StoreError::InvalidInput("symbol query cannot be empty".into()));
        }
        let tokens: Vec<String> = trimmed
            .split_whitespace()
            .filter(|t| !t.is_empty())
            .map(|t| format!("\"{}\"", t.replace('"', "\"\"")))
            .collect();
        let fts_query = tokens.join(" AND ");
        let capped = limit.clamp(1, 100) as i64;

        let mut stmt = self.conn.prepare(
            "SELECT s.id, s.object_id, o.path, p.name, p.root_path, s.name, s.kind, s.signature, s.start_line, s.end_line
             FROM symbols_fts
             JOIN symbols s ON s.rowid = symbols_fts.rowid
             JOIN objects o ON o.id = s.object_id
             LEFT JOIN projects p ON p.id = o.project_id
             WHERE symbols_fts MATCH ?1
               AND o.is_deleted = 0
               AND (?2 IS NULL OR p.name = ?2)
             ORDER BY bm25(symbols_fts, 4.0, 2.0, 0.5) ASC,
                      CASE WHEN lower(s.name) = lower(?3) THEN 0 ELSE 1 END ASC,
                      s.start_line ASC
             LIMIT ?4",
        )?;

        let rows = stmt.query_map(params![fts_query, project_name, trimmed, capped], |row| {
            let full_path: String = row.get(2)?;
            let proj_name: Option<String> = row.get(3)?;
            let root: Option<String> = row.get(4)?;
            let kind_str: String = row.get(6)?;
            Ok(SymbolRecord {
                id: row.get(0)?,
                object_id: row.get(1)?,
                path: vos_core::compact::compact_path(&full_path, root.as_deref()),
                project_name: proj_name,
                name: row.get(5)?,
                kind: SymbolKind::from_db(&kind_str),
                signature: row.get(7)?,
                start_line: (row.get::<_, i64>(8)?).max(1) as usize,
                end_line: (row.get::<_, i64>(9)?).max(1) as usize,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Reads an exact symbol span or line window from disk, or returns the symbol outline when both are omitted.
    pub fn slice_object(
        &self,
        target: &str,
        project_name: Option<&str>,
        symbol: Option<&str>,
        start_line: Option<usize>,
        end_line: Option<usize>,
    ) -> Result<FileSlice, StoreError> {
        let details = self.show_object_scoped(target, project_name)?;
        let symbols = self.list_file_symbols(target, project_name)?;
        let root_prefix = self
            .project_root_for_id(details.object.project_id.as_deref())?;
        let rel_path = vos_core::compact::compact_path(&details.object.path, root_prefix.as_deref());

        let file_path = Path::new(&details.object.path);
        let raw_text = fs::read_to_string(file_path)?;
        let all_lines: Vec<&str> = raw_text.lines().collect();
        let total_lines = all_lines.len().max(1);

        let (slice_start, slice_end, matched_symbol, include_content) = if let Some(sym_query) =
            symbol.map(str::trim).filter(|s| !s.is_empty())
        {
            let matched = symbols
                .iter()
                .find(|s| s.name.eq_ignore_ascii_case(sym_query))
                .or_else(|| {
                    let needle = sym_query.to_ascii_lowercase();
                    symbols
                        .iter()
                        .find(|s| s.name.to_ascii_lowercase().contains(&needle))
                })
                .ok_or_else(|| {
                    StoreError::NotFound(
                        "symbol",
                        format!("{sym_query} in {rel_path}"),
                    )
                })?;
            (
                matched.start_line.clamp(1, total_lines),
                matched.end_line.clamp(1, total_lines),
                Some(matched.name.clone()),
                true,
            )
        } else if start_line.is_some() || end_line.is_some() {
            let s = start_line.unwrap_or(1).clamp(1, total_lines);
            let e = end_line.unwrap_or((s + 79).min(total_lines)).clamp(s, total_lines);
            (s, e, None, true)
        } else {
            (1, total_lines, None, false)
        };

        let content = if include_content && !all_lines.is_empty() {
            let lines_fmt: Vec<String> = all_lines[slice_start - 1..=slice_end - 1]
                .iter()
                .enumerate()
                .map(|(offset, text)| format!("{:>4}: {text}", slice_start + offset))
                .collect();
            Some(lines_fmt.join("\n"))
        } else {
            None
        };

        Ok(FileSlice {
            path: rel_path,
            title: details.object.title,
            project_name: details.project_name,
            total_lines: all_lines.len(),
            start_line: slice_start,
            end_line: slice_end,
            matched_symbol,
            symbols,
            content,
        })
    }

    fn project_root_for_id(&self, project_id: Option<&str>) -> Result<Option<String>, StoreError> {
        let Some(pid) = project_id else {
            return Ok(None);
        };
        let root: Option<String> = self
            .conn
            .query_row(
                "SELECT root_path FROM projects WHERE id = ?1",
                params![pid],
                |row| row.get(0),
            )
            .ok()
            .flatten();
        Ok(root)
    }
}