//! Read-only access to Codex's versioned metadata/history databases and Zstd
//! rollouts. Never materialize a decompressed rollout in CODEX_HOME: Codex owns
//! the migration, projection cursors, and the writer locks.

use std::collections::{hash_map::DefaultHasher, HashSet};
use std::fs;
use std::hash::{Hash, Hasher};
use std::io::{self, BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::{params, Connection, OpenFlags};
use serde_json::Value;

use crate::types::{Block, Msg};
use crate::util::{home, text_block};

pub(super) const VPATH_PREFIX: &str = "codex://";

pub(super) fn codex_home() -> PathBuf {
    std::env::var_os("CODEX_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".codex"))
}

pub(super) fn newest_db(root: &Path, prefix: &str) -> Option<PathBuf> {
    fs::read_dir(root)
        .ok()?
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            let version = name
                .strip_prefix(prefix)?
                .strip_suffix(".sqlite")?
                .parse::<u64>()
                .ok()?;
            entry.path().is_file().then_some((version, entry.path()))
        })
        .max_by_key(|(version, _)| *version)
        .map(|(_, path)| path)
}

fn open_readonly(path: &Path) -> Result<Connection, String> {
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|error| format!("Cannot read Codex database {}: {error}", path.display()))?;
    conn.busy_timeout(Duration::from_millis(1500))
        .map_err(|error| error.to_string())?;
    Ok(conn)
}

fn columns(conn: &Connection, table: &str) -> Result<HashSet<String>, String> {
    let mut stmt = conn
        .prepare(&format!("PRAGMA table_info({table})"))
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(|e| e.to_string())?;
    rows.collect::<Result<_, _>>().map_err(|e| e.to_string())
}

#[derive(Debug, Clone)]
pub(super) struct ThreadRecord {
    pub id: String,
    pub rollout: PathBuf,
    pub cwd: String,
    pub title: String,
    pub created_ms: i64,
    pub updated_ms: u64,
    pub archived: bool,
    pub source: String,
    pub thread_source: Option<String>,
    pub model: Option<String>,
}

pub(super) fn threads(root: &Path) -> Result<Vec<ThreadRecord>, String> {
    let Some(path) = newest_db(root, "state_") else {
        return Ok(Vec::new());
    };
    let conn = open_readonly(&path)?;
    let cols = columns(&conn, "threads")?;
    // Older state schemas lack model/thread_source and millisecond timestamps.
    // Select only known columns; don't silently discard the entire flags index.
    let expr = |name: &str, fallback: &str| {
        if cols.contains(name) {
            name.to_string()
        } else {
            fallback.to_string()
        }
    };
    let created = expr(
        "created_at_ms",
        &format!("{} * 1000", expr("created_at", "0")),
    );
    let updated = expr(
        "updated_at_ms",
        &format!("{} * 1000", expr("updated_at", "0")),
    );
    let sql = format!(
        "SELECT id, rollout_path, cwd, {}, COALESCE({}, {} * 1000), \
         COALESCE({}, {} * 1000), {}, {}, {}, {} FROM threads",
        expr("title", "''"),
        created,
        expr("created_at", "0"),
        updated,
        expr("updated_at", "0"),
        expr("archived", "0"),
        expr("source", "''"),
        expr("thread_source", "NULL"),
        expr("model", "NULL")
    );
    let mut stmt = conn
        .prepare(&sql)
        .map_err(|e| format!("Unsupported Codex state schema: {e}"))?;
    let rows = stmt
        .query_map([], |row| {
            Ok(ThreadRecord {
                id: row.get(0)?,
                rollout: PathBuf::from(row.get::<_, String>(1)?),
                cwd: row.get(2)?,
                title: row.get(3)?,
                created_ms: row.get(4)?,
                updated_ms: row.get::<_, i64>(5)?.max(0) as u64,
                archived: row.get::<_, i64>(6)? != 0,
                source: row.get(7)?,
                thread_source: row.get(8)?,
                model: row.get(9)?,
            })
        })
        .map_err(|e| e.to_string())?;
    rows.collect::<Result<_, _>>().map_err(|e| e.to_string())
}

pub(super) fn is_compressed(path: &Path) -> bool {
    path.to_string_lossy().ends_with(".jsonl.zst")
}

/// A saved tab may still refer to the pre-migration filename, or the CLI may
/// have rehydrated a .zst while the viewer was open. Prefer the live raw file.
pub(super) fn rollout_path(path: &Path) -> Option<PathBuf> {
    let raw = if is_compressed(path) {
        path.with_extension("")
    } else {
        path.to_path_buf()
    };
    if raw.is_file() {
        return Some(raw);
    }
    let compressed = PathBuf::from(format!("{}.zst", raw.to_string_lossy()));
    compressed.is_file().then_some(compressed)
}

pub(super) fn open_rollout(path: &Path) -> io::Result<Box<dyn BufRead>> {
    let path = rollout_path(path)
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "Codex rollout not found"))?;
    let file = fs::File::open(&path)?;
    if is_compressed(&path) {
        Ok(Box::new(BufReader::new(zstd::stream::read::Decoder::new(
            file,
        )?)))
    } else {
        Ok(Box::new(BufReader::new(file)))
    }
}

pub(super) fn virtual_id(path: &str) -> Result<&str, String> {
    path.strip_prefix(VPATH_PREFIX)
        .filter(|id| {
            !id.is_empty()
                && id.len() <= 128
                && id
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        })
        .ok_or_else(|| "Invalid Codex history path".to_string())
}

pub(super) fn thread_for_path(root: &Path, path: &str) -> Result<Option<ThreadRecord>, String> {
    let id = path
        .strip_prefix(VPATH_PREFIX)
        .map(|_| virtual_id(path))
        .transpose()?;
    Ok(threads(root)?.into_iter().find(|record| {
        id == Some(record.id.as_str())
            || record.rollout == Path::new(path)
            || format!("{}.zst", record.rollout.to_string_lossy()) == path
    }))
}

pub(super) fn validate_resume(root: &Path, id: &str, path: &str) -> Result<(), String> {
    let record = threads(root)?.into_iter().find(|record| record.id == id);
    if let Some(record) = record {
        if rollout_path(&record.rollout).is_some() {
            return Ok(());
        }
        return Err("The original Codex rollout is missing. Recovered display history can be viewed/exported but cannot be resumed.".into());
    }
    if path.starts_with(VPATH_PREFIX) {
        return Err("The original Codex rollout is missing. Recovered display history can be viewed/exported but cannot be resumed.".into());
    }
    // Pre-SQLite Codex sessions are still resolved by the native CLI's ID scan.
    Ok(())
}

pub(super) fn validate_file_delete(root: &Path, path: &Path) -> Result<(), String> {
    if let Some(record) = thread_for_path(root, &path.to_string_lossy())? {
        if let Some(conn) = history_connection(root)? {
            if history_ids(&conn)?.contains(&record.id) {
                return Err("Codex manages this session in paginated storage; delete it with Codex to keep history consistent".into());
            }
        }
    }
    Ok(())
}

pub(super) fn history_connection(root: &Path) -> Result<Option<Connection>, String> {
    newest_db(root, "thread_history_")
        .map(|path| open_readonly(&path))
        .transpose()
}

pub(super) fn history_ids(conn: &Connection) -> Result<HashSet<String>, String> {
    let projection = columns(conn, "thread_history_projection_state")?;
    let sql = if projection.contains("thread_id") {
        "SELECT thread_id FROM thread_history_projection_state"
    } else {
        "SELECT DISTINCT thread_id FROM thread_items"
    };
    let mut stmt = conn
        .prepare(sql)
        .map_err(|e| format!("Unsupported Codex history schema: {e}"))?;
    let rows = stmt
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|e| e.to_string())?;
    rows.collect::<Result<_, _>>().map_err(|e| e.to_string())
}

#[derive(Debug, Default)]
pub(super) struct HistorySummary {
    pub message_count: usize,
    pub size: u64,
    pub last_reply_at: Option<u64>,
}

pub(super) fn history_summary(conn: &Connection, id: &str) -> Result<HistorySummary, String> {
    let mut summary = conn.query_row(
        "SELECT COALESCE(SUM(json_extract(item_json, '$.type') IN ('userMessage','agentMessage')), 0), \
         COALESCE(SUM(length(CAST(item_json AS BLOB))), 0) FROM thread_items WHERE thread_id = ?1",
        [id], |row| Ok(HistorySummary { message_count: row.get::<_, i64>(0)?.max(0) as usize, size: row.get::<_, i64>(1)?.max(0) as u64, last_reply_at: None })
    ).map_err(|e| format!("Cannot read Codex history summary: {e}"))?;
    // Projected history has its own reply timestamps. Metadata updates and later
    // questions must not advance the optional reply sort after rollout migration.
    let mut stmt = conn
        .prepare(
            "SELECT item_json, created_at_ms FROM thread_items WHERE thread_id = ?1 \
         AND json_extract(item_json, '$.type') = 'agentMessage' \
         AND created_at_ms >= 0 ORDER BY created_at_ms DESC",
        )
        .map_err(|e| format!("Cannot read Codex reply time: {e}"))?;
    let rows = stmt
        .query_map([id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })
        .map_err(|e| e.to_string())?;
    for row in rows {
        let (json, timestamp) = row.map_err(|e| e.to_string())?;
        let item: Value = serde_json::from_str(&json).map_err(|e| e.to_string())?;
        if matches!(
            super::agent_message_phase(&item),
            None | Some("final_answer")
        ) && matches!(
            item.get("status").and_then(Value::as_str),
            None | Some("completed")
        ) && (super::reply_text_present(item.get("text"))
            || super::reply_text_present(item.get("content")))
        {
            summary.last_reply_at = Some(timestamp as u64);
            break;
        }
    }
    Ok(summary)
}

fn input_blocks(content: &Value) -> Vec<Block> {
    let mut blocks = Vec::new();
    let Some(items) = content.as_array() else {
        return blocks;
    };
    for item in items {
        match item.get("type").and_then(Value::as_str) {
            Some("text" | "input_text") => {
                if let Some(text) = item
                    .get("text")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                {
                    let (mut files, body) =
                        super::extract_codex_files(crate::util::visible_deferred_follow_up(text));
                    super::bind_codex_pasted_file_placeholders(&body, &mut files);
                    blocks.extend(files);
                    if !body.is_empty() {
                        blocks.push(text_block("text", &body));
                    }
                }
            }
            Some("image" | "input_image" | "localImage") => {
                if let Some(src) = item
                    .get("imageUrl")
                    .or_else(|| item.get("image_url"))
                    .or_else(|| item.get("url"))
                    .or_else(|| item.get("path"))
                    .and_then(Value::as_str)
                {
                    blocks.push(Block {
                        kind: "image".into(),
                        image_src: Some(src.into()),
                        ..Default::default()
                    });
                }
            }
            _ => {}
        }
    }
    blocks
}

pub(super) fn history_item_to_msg(item: &Value) -> Option<Msg> {
    match item.get("type").and_then(Value::as_str)? {
        "userMessage" => {
            let blocks = input_blocks(item.get("content").unwrap_or(&Value::Null));
            (!blocks.is_empty()).then(|| Msg {
                role: "user".into(),
                blocks,
                ..Default::default()
            })
        }
        "reasoning" => {
            let parts = |key| {
                item.get(key)
                    .and_then(Value::as_array)
                    .map(|xs| {
                        xs.iter()
                            .filter_map(Value::as_str)
                            .collect::<Vec<_>>()
                            .join("\n")
                    })
                    .unwrap_or_default()
            };
            let summary = parts("summary");
            let text = if summary.is_empty() {
                parts("content")
            } else {
                summary
            };
            (!text.is_empty()).then(|| Msg {
                role: "assistant".into(),
                blocks: vec![text_block("thinking", &text)],
                ..Default::default()
            })
        }
        // The projection does not preserve the compaction summary. Don't invent one.
        "contextCompaction" => None,
        _ => crate::agent_chat::codex_item_to_msg(item),
    }
}

pub(super) fn read_history(root: &Path, id: &str) -> Result<Vec<Msg>, String> {
    virtual_id(&format!("{VPATH_PREFIX}{id}"))?;
    let conn = history_connection(root)?.ok_or("Codex paginated history database not found")?;
    if !history_ids(&conn)?.contains(id) {
        return Err("Codex paginated history not found for this session".into());
    }
    let model = threads(root)?
        .into_iter()
        .find(|thread| thread.id == id)
        .and_then(|thread| thread.model);
    let mut stmt = conn.prepare(
        "SELECT item_json, created_at_ms FROM thread_items WHERE thread_id = ?1 ORDER BY rollout_ordinal, item_id"
    ).map_err(|e| format!("Cannot read Codex paginated history: {e}"))?;
    let rows = stmt
        .query_map(params![id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })
        .map_err(|e| e.to_string())?;
    let mut messages = Vec::new();
    for row in rows {
        let (json, timestamp) = row.map_err(|e| e.to_string())?;
        let item: Value =
            serde_json::from_str(&json).map_err(|e| format!("Invalid Codex history item: {e}"))?;
        if let Some(mut msg) = history_item_to_msg(&item) {
            msg.uuid = item.get("id").and_then(Value::as_str).map(str::to_owned);
            msg.timestamp = chrono::DateTime::from_timestamp_millis(timestamp)
                .map(|dt| dt.to_rfc3339_opts(chrono::SecondsFormat::Millis, true));
            if msg.role == "assistant" {
                msg.model = model.clone();
            }
            messages.push(msg);
        }
    }
    Ok(messages)
}

pub(super) fn watch_targets(root: &Path, path: &str) -> Vec<PathBuf> {
    if !path.starts_with(VPATH_PREFIX) {
        if let Some(file) = rollout_path(Path::new(path)) {
            return vec![file];
        }
    }
    let mut paths = Vec::new();
    for prefix in ["state_", "thread_history_"] {
        if let Some(db) = newest_db(root, prefix) {
            let wal = PathBuf::from(format!("{}-wal", db.to_string_lossy()));
            paths.push(db);
            if wal.is_file() {
                paths.push(wal);
            }
        }
    }
    paths
}

/// An opaque cache revision, not a displayed timestamp. Include WAL metadata:
/// committed writes may not touch the main SQLite file until checkpointing.
pub(super) fn source_revision(root: &Path, path: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    let mut paths = watch_targets(root, path);
    if !path.starts_with(VPATH_PREFIX) {
        paths.push(PathBuf::from(path));
    }
    for prefix in ["state_", "thread_history_"] {
        if let Some(db) = newest_db(root, prefix) {
            paths.push(PathBuf::from(format!("{}-wal", db.to_string_lossy())));
            paths.push(db);
        }
    }
    for path in paths {
        path.hash(&mut hasher);
        if let Ok(md) = fs::metadata(path) {
            md.len().hash(&mut hasher);
            md.modified().ok().hash(&mut hasher);
        }
    }
    hasher.finish()
}

#[cfg(test)]
#[path = "codex_storage_tests.rs"]
mod tests;
