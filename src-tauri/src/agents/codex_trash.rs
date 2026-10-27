//! Recoverable, session-scoped deletion of Codex's multi-file storage.
//! Never delete a rollout without its projection. The durable bundle is written
//! before source changes; SQLite transactions and filesystem rollback keep
//! failures recoverable. Unknown database versions are not guessed on restore.
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use rusqlite::{params_from_iter, types::Value as SqlValue, Connection, TransactionBehavior};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::storage;
use crate::types::Msg;

const PREVIEW: &str = "session.jsonl";
const INDEXES: [&str; 2] = ["session_index.jsonl", "history.jsonl"];

#[derive(Serialize, Deserialize)]
struct Preview {
    kind: String,
    title: String,
    messages: Vec<Msg>,
}

pub(super) fn preview(path: &Path) -> Result<Option<(String, Vec<Msg>)>, String> {
    if path.file_name().and_then(|s| s.to_str()) != Some(PREVIEW) {
        return Ok(None);
    }
    let file = fs::File::open(path).map_err(err)?;
    let mut line = String::new();
    BufReader::new(file).read_line(&mut line).map_err(err)?;
    let value: Value = serde_json::from_str(&line).unwrap_or(Value::Null);
    if value["kind"] != "ccsv-codex-trash-v1" {
        return Ok(None);
    }
    let data: Preview = serde_json::from_value(value).map_err(err)?;
    Ok(Some((data.title, data.messages)))
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
enum Cell {
    Null,
    Integer(i64),
    Real(f64),
    Text(String),
    Blob(Vec<u8>),
}
impl From<SqlValue> for Cell {
    fn from(v: SqlValue) -> Self {
        match v {
            SqlValue::Null => Self::Null,
            SqlValue::Integer(v) => Self::Integer(v),
            SqlValue::Real(v) => Self::Real(v),
            SqlValue::Text(v) => Self::Text(v),
            SqlValue::Blob(v) => Self::Blob(v),
        }
    }
}
impl From<&Cell> for SqlValue {
    fn from(v: &Cell) -> Self {
        match v {
            Cell::Null => Self::Null,
            Cell::Integer(v) => Self::Integer(*v),
            Cell::Real(v) => Self::Real(*v),
            Cell::Text(v) => Self::Text(v.clone()),
            Cell::Blob(v) => Self::Blob(v.clone()),
        }
    }
}

#[derive(Serialize, Deserialize)]
struct Table {
    database: String,
    name: String,
    columns: Vec<String>,
    keys: Vec<String>,
    rows: Vec<Vec<Cell>>,
}
#[derive(Serialize, Deserialize)]
struct SavedFile {
    relative: PathBuf,
    sha256: String,
}
#[derive(Serialize, Deserialize)]
struct Index {
    name: String,
    removed: String,
}
#[derive(Serialize, Deserialize)]
struct Bundle {
    version: u32,
    root: PathBuf,
    id: String,
    original_path: String,
    databases: Vec<String>,
    tables: Vec<Table>,
    files: Vec<SavedFile>,
    indexes: Vec<Index>,
}
fn err(e: impl std::fmt::Display) -> String {
    format!("Codex storage: {e}")
}
fn quote(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}
fn hash(path: &Path) -> Result<String, String> {
    let mut file = fs::File::open(path).map_err(err)?;
    let mut hash = Sha256::new();
    std::io::copy(&mut file, &mut hash).map_err(err)?;
    Ok(format!("{:x}", hash.finalize()))
}
fn safe_path(root: &Path, relative: &Path) -> Result<PathBuf, String> {
    if relative.as_os_str().is_empty()
        || relative
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
    {
        return Err(err("invalid relative storage path"));
    }
    let mut path = root.to_path_buf();
    for component in relative.components() {
        path.push(component);
        if fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_symlink()) {
            return Err(err("symlinked session storage is not supported"));
        }
    }
    Ok(path)
}
fn validate_rollout_relative(relative: &Path) -> Result<(), String> {
    if !matches!(relative.components().next(), Some(Component::Normal(v)) if v == "sessions" || v == "archived_sessions")
        || !(relative.to_string_lossy().ends_with(".jsonl") || storage::is_compressed(relative))
    {
        return Err(err("session is outside Codex's session directories"));
    }
    Ok(())
}
fn durable_write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let temporary = path.with_extension(format!("tmp-{}", uuid::Uuid::new_v4()));
    let result = (|| {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary).map_err(err)?;
        file.write_all(bytes).map_err(err)?;
        file.sync_all().map_err(err)?;
        fs::rename(&temporary, path).map_err(err)
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}

fn database_names(root: &Path) -> Vec<String> {
    ["state_", "thread_history_"]
        .into_iter()
        .filter_map(|prefix| storage::newest_db(root, prefix))
        .filter_map(|p| p.file_name().map(|s| s.to_string_lossy().into_owned()))
        .collect()
}
fn connect(root: &Path, databases: &[String]) -> Result<Connection, String> {
    let conn = Connection::open_in_memory().map_err(err)?;
    conn.busy_timeout(Duration::from_secs(3)).map_err(err)?;
    conn.pragma_update(None, "foreign_keys", true)
        .map_err(err)?;
    for (i, name) in databases.iter().enumerate() {
        let path = safe_path(root, Path::new(name))?;
        // mode=rw refuses to create a database if Codex changed it meanwhile.
        let url = tauri::Url::from_file_path(path).map_err(|_| err("invalid database path"))?;
        conn.execute(
            &format!("ATTACH DATABASE ?1 AS db{i}"),
            [format!("{url}?mode=rw")],
        )
        .map_err(err)?;
    }
    Ok(conn)
}
fn scope(keys: &[String]) -> String {
    keys.iter()
        .map(|key| format!("{} = ?1", quote(key)))
        .collect::<Vec<_>>()
        .join(" OR ")
}
fn tables(conn: &Connection, databases: &[String], id: &str) -> Result<Vec<Table>, String> {
    let mut output = Vec::new();
    for (i, database) in databases.iter().enumerate() {
        let schema = format!("db{i}");
        let mut stmt = conn.prepare(&format!("SELECT name FROM {schema}.sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name")).map_err(err)?;
        let names = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(err)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(err)?;
        for name in names {
            let mut stmt = conn
                .prepare(&format!("PRAGMA {schema}.table_info({})", quote(&name)))
                .map_err(err)?;
            let columns = stmt
                .query_map([], |r| r.get::<_, String>(1))
                .map_err(err)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(err)?;
            let keys: Vec<_> = columns
                .iter()
                .filter(|c| {
                    (name == "threads" && c.as_str() == "id")
                        || matches!(
                            c.as_str(),
                            "thread_id" | "parent_thread_id" | "child_thread_id"
                        )
                })
                .cloned()
                .collect();
            if keys.is_empty() {
                continue;
            }
            let sql = format!(
                "SELECT {} FROM {schema}.{} WHERE {}",
                columns
                    .iter()
                    .map(|c| quote(c))
                    .collect::<Vec<_>>()
                    .join(","),
                quote(&name),
                scope(&keys)
            );
            let mut stmt = conn.prepare(&sql).map_err(err)?;
            let rows = stmt
                .query_map([id], |row| {
                    (0..columns.len())
                        .map(|j| row.get::<_, SqlValue>(j).map(Cell::from))
                        .collect::<Result<Vec<_>, _>>()
                })
                .map_err(err)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(err)?;
            output.push(Table {
                database: database.clone(),
                name,
                columns,
                keys,
                rows,
            });
        }
    }
    Ok(output)
}
fn qualified(table: &Table, databases: &[String]) -> Result<String, String> {
    let i = databases
        .iter()
        .position(|n| *n == table.database)
        .ok_or_else(|| err("missing database in bundle"))?;
    Ok(format!("db{i}.{}", quote(&table.name)))
}
fn index_parts(raw: &str, id: &str) -> (String, String) {
    let mut keep = String::new();
    let mut removed = String::new();
    for line in raw.split_inclusive('\n') {
        let belongs = serde_json::from_str::<Value>(line).ok().is_some_and(|v| {
            ["id", "session_id", "thread_id"]
                .iter()
                .any(|key| v[*key].as_str() == Some(id))
        });
        if belongs {
            removed.push_str(line);
        } else {
            keep.push_str(line);
        }
    }
    (keep, removed)
}
fn read_index(path: &Path) -> Result<String, String> {
    match fs::read_to_string(path) {
        Ok(raw) => Ok(raw),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(e) => Err(err(e)),
    }
}

pub(super) fn delete(root: &Path, path: &str, destination: &Path) -> Result<(), String> {
    let input_root = root.to_path_buf();
    let root = fs::canonicalize(root).map_err(err)?;
    let record = storage::thread_for_path(&root, path)?;
    let raw = if let Some(record) = &record {
        record.rollout.clone()
    } else {
        PathBuf::from(path)
    };
    let raw = if storage::is_compressed(&raw) {
        raw.with_extension("")
    } else {
        raw
    };
    // macOS /var is an alias of /private/var; also support a symlinked
    // CODEX_HOME without following symlinks *inside* its storage directories.
    let raw = raw
        .strip_prefix(&input_root)
        .map(|relative| root.join(relative))
        .unwrap_or(raw);
    let relative = raw
        .strip_prefix(&root)
        .map_err(|_| err("session is outside CODEX_HOME"))?;
    validate_rollout_relative(relative)?;
    safe_path(&root, relative)?;
    let id = if let Some(record) = &record {
        record.id.clone()
    } else {
        let live = storage::rollout_path(&raw).ok_or_else(|| err("session not found"))?;
        super::meta(&live)
            .ok_or_else(|| err("missing session metadata"))?
            .id
    };
    if id.is_empty() {
        return Err(err("missing session ID"));
    }
    if let Some(live) = storage::rollout_path(&raw) {
        if super::meta(&live).is_none_or(|meta| meta.id != id) {
            return Err(err("rollout identity does not match the Codex thread"));
        }
    }
    if path.starts_with(storage::VPATH_PREFIX) && storage::virtual_id(path)? != id {
        return Err(err("thread ID mismatch"));
    }
    let databases = database_names(&root);
    let mut conn = connect(&root, &databases)?;
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(err)?;
    tx.pragma_update(None, "defer_foreign_keys", true)
        .map_err(err)?;
    let tables = tables(&tx, &databases, &id)?;
    let messages = super::read_in(&root, path)?;
    let title = super::load_title_index_in(&root)
        .get(&id)
        .map(|t| t.name.clone())
        .or_else(|| record.as_ref().map(|r| r.title.clone()))
        .unwrap_or_else(|| super::first_user_text(&raw));
    let mut bundle = Bundle {
        version: 1,
        root: root.clone(),
        id,
        original_path: path.into(),
        databases,
        tables,
        files: Vec::new(),
        indexes: Vec::new(),
    };
    for file in [&raw, &PathBuf::from(format!("{}.zst", raw.display()))] {
        if file.exists() {
            let relative = file.strip_prefix(&root).map_err(err)?.to_path_buf();
            safe_path(&root, &relative)?;
            bundle.files.push(SavedFile {
                relative,
                sha256: hash(file)?,
            });
        }
    }
    let mut originals = Vec::new();
    for name in INDEXES {
        let index_path = safe_path(&root, Path::new(name))?;
        let raw = read_index(&index_path)?;
        let (keep, removed) = index_parts(&raw, &bundle.id);
        if !removed.is_empty() {
            bundle.indexes.push(Index {
                name: name.into(),
                removed,
            });
            originals.push((index_path, raw, keep));
        }
    }
    fs::create_dir(destination).map_err(err)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(destination, fs::Permissions::from_mode(0o700)).map_err(err)?;
    }
    let mut moved: Vec<&SavedFile> = Vec::new();
    let mut changed_indexes: Vec<&(PathBuf, String, String)> = Vec::new();
    let result = (|| {
        durable_write(
            &destination.join(PREVIEW),
            &serde_json::to_vec(&Preview {
                kind: "ccsv-codex-trash-v1".into(),
                title,
                messages,
            })
            .map_err(err)?,
        )?;
        durable_write(
            &destination.join("manifest.json"),
            &serde_json::to_vec(&bundle).map_err(err)?,
        )?;
        for table in &bundle.tables {
            tx.execute(
                &format!(
                    "DELETE FROM {} WHERE {}",
                    qualified(table, &bundle.databases)?,
                    scope(&table.keys)
                ),
                [&bundle.id],
            )
            .map_err(err)?;
        }
        for file in &bundle.files {
            let source = safe_path(&root, &file.relative)?;
            if hash(&source)? != file.sha256 {
                return Err(err(
                    "session changed during deletion; retry when Codex is idle",
                ));
            }
            let target = safe_path(destination, &file.relative)?;
            fs::create_dir_all(target.parent().unwrap()).map_err(err)?;
            fs::rename(source, target).map_err(err)?;
            moved.push(file);
        }
        for entry @ (path, before, after) in &originals {
            if read_index(path)? != *before {
                return Err(err(
                    "session index changed during deletion; retry when Codex is idle",
                ));
            }
            durable_write(path, after.as_bytes())?;
            changed_indexes.push(entry);
        }
        tx.commit().map_err(err)
    })();
    if let Err(error) = result {
        let mut rollback_errors = Vec::new();
        for (path, before, after) in changed_indexes.into_iter().rev() {
            let result = read_index(path).and_then(|now| {
                if now == *after {
                    durable_write(path, before.as_bytes())
                } else {
                    Err(err("index changed during rollback"))
                }
            });
            if let Err(e) = result {
                rollback_errors.push(e);
            }
        }
        for file in moved.into_iter().rev() {
            let source = root.join(&file.relative);
            if source.exists() {
                rollback_errors.push(err("rollout recreated during rollback"));
                continue;
            }
            if let Err(e) = fs::rename(destination.join(&file.relative), source) {
                rollback_errors.push(err(e));
            }
        }
        if rollback_errors.is_empty() {
            let _ = fs::remove_dir_all(destination);
        }
        return Err(format!(
            "{error}{}",
            if rollback_errors.is_empty() {
                String::new()
            } else {
                format!(
                    "; backup retained at {}: {}",
                    destination.display(),
                    rollback_errors.join("; ")
                )
            }
        ));
    }
    Ok(())
}

pub(super) fn restore(root: &Path, directory: &Path) -> Result<(), String> {
    let bundle: Bundle =
        serde_json::from_slice(&fs::read(directory.join("manifest.json")).map_err(err)?)
            .map_err(err)?;
    let root = fs::canonicalize(root).map_err(err)?;
    if bundle.version != 1 || root != bundle.root || database_names(&root) != bundle.databases {
        return Err(err("Codex storage location/schema changed; backup retained, restore with the original Codex database version"));
    }
    let mut conn = connect(&root, &bundle.databases)?;
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(err)?;
    tx.pragma_update(None, "defer_foreign_keys", true)
        .map_err(err)?;
    let current = tables(&tx, &bundle.databases, &bundle.id)?;
    if current.iter().any(|t| !t.rows.is_empty()) {
        return Err(err(
            "session already exists in Codex; refusing to overwrite it",
        ));
    }
    for table in &bundle.tables {
        if !current.iter().any(|t| {
            t.database == table.database
                && t.name == table.name
                && t.columns == table.columns
                && t.keys == table.keys
        }) {
            return Err(err("Codex schema changed; refusing partial restore"));
        }
    }
    for table in &bundle.tables {
        for row in &table.rows {
            if row.len() != table.columns.len()
                || !table.keys.iter().any(|key| {
                    table
                        .columns
                        .iter()
                        .position(|c| c == key)
                        .is_some_and(|i| row.get(i) == Some(&Cell::Text(bundle.id.clone())))
                })
            {
                return Err(err("invalid session row in backup"));
            }
        }
    }
    for file in &bundle.files {
        validate_rollout_relative(&file.relative)?;
        if safe_path(&root, &file.relative)?.exists() {
            return Err(err("rollout already exists; refusing to overwrite it"));
        }
        if hash(&safe_path(directory, &file.relative)?)? != file.sha256 {
            return Err(err("rollout backup is corrupted"));
        }
    }
    let mut indexes = Vec::new();
    for index in &bundle.indexes {
        if !INDEXES.contains(&index.name.as_str()) {
            return Err(err("invalid index name"));
        }
        let path = safe_path(&root, Path::new(&index.name))?;
        let before = read_index(&path)?;
        if !index_parts(&before, &bundle.id).1.is_empty() {
            return Err(err(
                "session index entry already exists; refusing to overwrite it",
            ));
        }
        let mut after = before.clone();
        if !after.is_empty() && !after.ends_with('\n') {
            after.push('\n');
        }
        after.push_str(&index.removed);
        indexes.push((path, before, after));
    }
    let mut restored: Vec<&SavedFile> = Vec::new();
    let mut changed_indexes: Vec<&(PathBuf, String, String)> = Vec::new();
    let result = (|| {
        // Restore parent rows first; deferred FK checks also cover edges/tools.
        for table in bundle
            .tables
            .iter()
            .filter(|t| t.name == "threads")
            .chain(bundle.tables.iter().filter(|t| t.name != "threads"))
        {
            let sql = format!(
                "INSERT INTO {} ({}) VALUES ({})",
                qualified(table, &bundle.databases)?,
                table
                    .columns
                    .iter()
                    .map(|c| quote(c))
                    .collect::<Vec<_>>()
                    .join(","),
                vec!["?"; table.columns.len()].join(",")
            );
            for row in &table.rows {
                tx.execute(&sql, params_from_iter(row.iter().map(SqlValue::from)))
                    .map_err(err)?;
            }
        }
        for file in &bundle.files {
            let target = safe_path(&root, &file.relative)?;
            fs::create_dir_all(target.parent().unwrap()).map_err(err)?;
            // create_new never clobbers a rollout recreated by another client.
            let mut output = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&target)
                .map_err(err)?;
            restored.push(file);
            let mut input = fs::File::open(directory.join(&file.relative)).map_err(err)?;
            std::io::copy(&mut input, &mut output).map_err(err)?;
            output.sync_all().map_err(err)?;
        }
        for entry @ (path, before, after) in &indexes {
            if read_index(path)? != *before {
                return Err(err(
                    "session index changed during restore; retry when Codex is idle",
                ));
            }
            durable_write(path, after.as_bytes())?;
            changed_indexes.push(entry);
        }
        tx.commit().map_err(err)
    })();
    if let Err(error) = result {
        for (path, before, after) in changed_indexes.into_iter().rev() {
            if read_index(path).is_ok_and(|now| now == *after) {
                let _ = durable_write(path, before.as_bytes());
            }
        }
        for file in restored {
            let _ = fs::remove_file(root.join(&file.relative));
        }
        return Err(error);
    }
    Ok(())
}
