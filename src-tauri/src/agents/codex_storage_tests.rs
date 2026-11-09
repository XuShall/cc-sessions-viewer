use super::super::{discover_sessions_in, read_in, session_page_in, CodexSource};
use super::*;
use crate::agents::SessionSource;
use serde_json::json;
use std::io::{Read, Write};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let root =
            std::env::temp_dir().join(format!("ccsv-codex-storage-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        Self(root)
    }
    fn state(&self) -> Connection {
        let conn = Connection::open(self.0.join("state_5.sqlite")).unwrap();
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS threads (
            id TEXT PRIMARY KEY, rollout_path TEXT NOT NULL, cwd TEXT NOT NULL,
            title TEXT NOT NULL DEFAULT '', created_at INTEGER NOT NULL DEFAULT 1,
            updated_at INTEGER NOT NULL DEFAULT 2, archived INTEGER NOT NULL DEFAULT 0,
            source TEXT NOT NULL DEFAULT 'cli', thread_source TEXT, model TEXT
        );",
        )
        .unwrap();
        conn
    }
    fn history(&self) -> Connection {
        let conn = Connection::open(self.0.join("thread_history_1.sqlite")).unwrap();
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS thread_history_projection_state (
            thread_id TEXT PRIMARY KEY, next_rollout_byte_offset INTEGER DEFAULT 0,
            next_rollout_ordinal INTEGER DEFAULT 0
        ); CREATE TABLE IF NOT EXISTS thread_items (
            thread_id TEXT NOT NULL, turn_id TEXT NOT NULL DEFAULT 'turn', item_id TEXT NOT NULL,
            rollout_ordinal INTEGER NOT NULL, created_at_ms INTEGER NOT NULL,
            item_json TEXT NOT NULL, PRIMARY KEY(thread_id, turn_id, item_id)
        );",
        )
        .unwrap();
        conn
    }
    fn thread(&self, id: &str, archived: bool, internal: bool) -> PathBuf {
        let path = self
            .0
            .join(if archived {
                "archived_sessions"
            } else {
                "sessions/2026/10/20"
            })
            .join(format!("rollout-{id}.jsonl"));
        self.state().execute("INSERT INTO threads(id, rollout_path, cwd, title, archived, thread_source, model) VALUES (?1, ?2, '/project', ?3, ?4, ?5, 'test-model')",
            params![id, path.to_string_lossy(), format!("Title {id}"), archived, if internal { "subagent" } else { "user" }]).unwrap();
        path
    }
    fn project(&self, id: &str, items: &[Value]) {
        let conn = self.history();
        conn.execute(
            "INSERT INTO thread_history_projection_state(thread_id) VALUES (?1)",
            [id],
        )
        .unwrap();
        for (i, item) in items.iter().enumerate() {
            conn.execute("INSERT INTO thread_items(thread_id,item_id,rollout_ordinal,created_at_ms,item_json) VALUES (?1,?2,?3,?4,?5)",
                params![id, item["id"].as_str().unwrap(), i as i64, 1_000 + i as i64, item.to_string()]).unwrap();
        }
    }
    fn rollout(&self, path: &Path, id: &str, compressed: bool) -> PathBuf {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let body = [
            json!({"type":"session_meta","payload":{"id":id,"cwd":"/project","timestamp":"2026-10-20T00:00:00Z"}}),
            json!({"type":"event_msg","timestamp":"2026-10-20T00:00:01Z","payload":{"type":"user_message","message":"find the hidden session 中文"}}),
            json!({"type":"event_msg","timestamp":"2026-10-20T00:00:02Z","payload":{"type":"agent_message","message":"Found the session"}}),
            json!({"type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":100,"cached_input_tokens":20,"output_tokens":30,"reasoning_output_tokens":5,"total_tokens":130}}}}),
        ].iter().map(Value::to_string).collect::<Vec<_>>().join("\n") + "\n";
        let dest = if compressed {
            PathBuf::from(format!("{}.zst", path.display()))
        } else {
            path.to_path_buf()
        };
        fs::write(
            &dest,
            if compressed {
                zstd::stream::encode_all(body.as_bytes(), 1).unwrap()
            } else {
                body.into_bytes()
            },
        )
        .unwrap();
        dest
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn user(id: &str, text: &str) -> Value {
    json!({"type":"userMessage","id":id,"content":[{"type":"text","text":text}]})
}
fn agent(id: &str) -> Value {
    json!({"type":"agentMessage","id":id,"text":"Stored answer"})
}

#[test]
fn raw_and_zstd_readers_preserve_messages_and_usage_without_materializing_files() {
    let f = Fixture::new();
    let raw = f.0.join("sessions/raw.jsonl");
    let zipped_source = f.0.join("sessions/zipped.jsonl");
    f.rollout(&raw, "raw", false);
    let zipped = f.rollout(&zipped_source, "zipped", true);
    let before = fs::read(&zipped).unwrap();
    for path in [&raw, &zipped, &zipped_source] {
        let messages = read_in(&f.0, &path.to_string_lossy()).unwrap();
        assert!(messages.iter().any(|m| m.role == "user"
            && m.blocks
                .iter()
                .any(|b| b.text.as_deref().is_some_and(|s| s.contains("中文")))));
        let usage = super::super::usage_summary(path).unwrap();
        assert_eq!(usage.input_tokens, 80);
        assert_eq!(usage.output_tokens, 30);
        assert_eq!(usage.cache_read_input_tokens, 20);
        assert!(super::super::last_user_text(path)
            .unwrap()
            .contains("hidden session"));
    }
    assert!(!zipped_source.exists());
    assert_eq!(fs::read(&zipped).unwrap(), before);
}

#[test]
fn reply_sort_metadata_survives_raw_and_compressed_rollouts() {
    let f = Fixture::new();
    let raw = f.thread("reply-raw", false, false);
    f.rollout(&raw, "reply-raw", false);
    let compressed_raw = f.thread("reply-zipped", false, false);
    let zipped = f.rollout(&compressed_raw, "reply-zipped", true);
    let before = fs::read(&zipped).unwrap();
    let page = session_page_in(&f.0, "/project", 0, 10, false, false).unwrap();
    assert_eq!(page.total, 2);
    let reply = crate::util::parse_iso8601_ms("2026-10-20T00:00:02Z").unwrap() as u64;
    assert!(page
        .sessions
        .iter()
        .all(|session| session.last_reply_at == Some(reply)));
    assert!(!compressed_raw.exists());
    assert_eq!(fs::read(&zipped).unwrap(), before);
}

#[test]
fn projected_reply_sort_ignores_later_progress_tools_settings_and_pending_questions() {
    let f = Fixture::new();
    f.thread("reply-projected", false, false);
    f.project("reply-projected", &[
        user("question", "Question"),
        agent("legacy-answer"),
        json!({"type":"agentMessage","id":"final","phase":"final_answer","text":"Final answer"}),
        json!({"type":"agentMessage","id":"progress","phase":"commentary","text":"Working"}),
        json!({"type":"commandExecution","id":"tool","status":"completed","aggregatedOutput":"Tool output"}),
        json!({"type":"threadSettingsApplied","id":"settings"}),
        json!({"type":"agentMessage","id":"empty","text":"  \n  "}),
        json!({"type":"agentMessage","id":"partial","phase":"final_answer","status":"in_progress","text":"Partial"}),
        user("pending-question", "Another question"),
    ]);
    let page = session_page_in(&f.0, "/project", 0, 10, false, false).unwrap();
    assert_eq!(page.sessions[0].path, "codex://reply-projected");
    assert_eq!(page.sessions[0].last_reply_at, Some(1_002));
    assert_eq!(page.sessions[0].modified, 2_000);
    assert_eq!(
        serde_json::to_value(&page.sessions[0]).unwrap()["lastReplyAt"],
        1_002
    );
}

#[test]
fn projected_reply_sort_refreshes_only_after_a_reply_and_preserves_epoch_zero() {
    let f = Fixture::new();
    f.thread("reply-refresh", false, false);
    f.project("reply-refresh", &[user("pending", "Question")]);
    let read = || {
        session_page_in(&f.0, "/project", 0, 10, false, false)
            .unwrap()
            .sessions
            .remove(0)
    };
    assert_eq!(read().last_reply_at, None);
    let conn = f.history();
    conn.execute("INSERT INTO thread_items(thread_id,item_id,rollout_ordinal,created_at_ms,item_json) VALUES ('reply-refresh','epoch',1,0,?1)", [agent("epoch").to_string()]).unwrap();
    assert_eq!(read().last_reply_at, Some(0));
    conn.execute("INSERT INTO thread_items(thread_id,item_id,rollout_ordinal,created_at_ms,item_json) VALUES ('reply-refresh','settings',2,9000,?1)", [json!({"type":"threadSettingsApplied","id":"settings"}).to_string()]).unwrap();
    assert_eq!(read().last_reply_at, Some(0));
    conn.execute("INSERT INTO thread_items(thread_id,item_id,rollout_ordinal,created_at_ms,item_json) VALUES ('reply-refresh','latest',3,10000,?1)", [agent("latest").to_string()]).unwrap();
    let session = read();
    assert_eq!(session.last_reply_at, Some(10_000));
    assert_eq!(session.modified, 2_000);
}

#[test]
fn rehydrated_raw_rollout_wins_over_stale_compressed_copy_and_updates_watch_target() {
    let f = Fixture::new();
    let raw = f.0.join("sessions/demo.jsonl");
    let zipped = f.rollout(&raw, "demo", true);
    assert_eq!(
        watch_targets(&f.0, &zipped.to_string_lossy()),
        vec![zipped.clone()]
    );
    let before = source_revision(&f.0, &zipped.to_string_lossy());
    f.rollout(&raw, "demo", false);
    assert_eq!(watch_targets(&f.0, &zipped.to_string_lossy()), vec![raw]);
    assert_ne!(before, source_revision(&f.0, &zipped.to_string_lossy()));
}

#[test]
fn corrupt_zstd_propagates_decode_error_instead_of_an_empty_success() {
    let f = Fixture::new();
    let path = f.0.join("broken.jsonl.zst");
    fs::write(&path, b"not a zstd frame").unwrap();
    assert!(read_in(&f.0, &path.to_string_lossy()).is_err());
}

#[test]
fn chooses_highest_numeric_database_version_not_lexical_or_wal_suffix() {
    let f = Fixture::new();
    for name in [
        "state_4.sqlite",
        "state_11.sqlite",
        "state_9.sqlite",
        "state_99.sqlite-wal",
        "state_x.sqlite",
    ] {
        fs::write(f.0.join(name), b"").unwrap();
    }
    assert_eq!(
        newest_db(&f.0, "state_").unwrap(),
        f.0.join("state_11.sqlite")
    );
}

#[test]
fn old_state_schema_without_optional_fields_remains_supported() {
    let f = Fixture::new();
    let conn = Connection::open(f.0.join("state_3.sqlite")).unwrap();
    conn.execute_batch("CREATE TABLE threads(id TEXT, rollout_path TEXT, cwd TEXT, created_at INTEGER, updated_at INTEGER); INSERT INTO threads VALUES ('old','/old.jsonl','/project',100,200);").unwrap();
    let rows = threads(&f.0).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].created_ms, 100_000);
    assert_eq!(rows[0].updated_ms, 200_000);
    assert!(!rows[0].archived);
    assert!(rows[0].model.is_none());
}

#[test]
fn missing_databases_are_not_created_by_discovery_or_reading() {
    let f = Fixture::new();
    assert!(threads(&f.0).unwrap().is_empty());
    assert!(history_connection(&f.0).unwrap().is_none());
    assert!(read_history(&f.0, "missing").is_err());
    assert_eq!(fs::read_dir(&f.0).unwrap().count(), 0);
}

#[test]
fn database_connections_are_read_only() {
    let f = Fixture::new();
    drop(f.state());
    let conn = open_readonly(&f.0.join("state_5.sqlite")).unwrap();
    assert!(conn
        .execute(
            "INSERT INTO threads(id,rollout_path,cwd) VALUES('bad','bad','bad')",
            []
        )
        .is_err());
    assert!(threads(&f.0).unwrap().is_empty());
}

#[test]
fn discovery_merges_compressed_and_projected_sessions_without_duplicates_or_stale_metadata_only_rows(
) {
    let f = Fixture::new();
    let raw = f.thread("raw", false, false);
    f.rollout(&raw, "raw", false);
    f.rollout(&raw, "raw", true);
    let zipped = f.thread("zipped", false, false);
    f.rollout(&zipped, "zipped", true);
    f.project("zipped", &[user("u", "zip prompt"), agent("a")]);
    f.thread("database-only", false, false);
    f.project(
        "database-only",
        &[user("u", "only in the database"), agent("a")],
    );
    f.thread("empty-projection", false, false);
    f.project("empty-projection", &[]);
    f.thread("missing-legacy", false, false);
    let records = discover_sessions_in(&f.0, false, false).unwrap();
    let ids: HashSet<_> = records.iter().map(|r| r.meta.id.as_str()).collect();
    assert_eq!(
        ids,
        HashSet::from(["raw", "zipped", "database-only", "empty-projection"])
    );
    assert_eq!(
        records.iter().find(|r| r.meta.id == "raw").unwrap().path,
        raw
    );
    assert_eq!(
        records
            .iter()
            .find(|r| r.meta.id == "database-only")
            .unwrap()
            .path
            .to_string_lossy(),
        "codex://database-only"
    );
}

#[test]
fn current_rollout_wins_over_a_different_old_compressed_copy_and_stale_projection() {
    let f = Fixture::new();
    let current = f.thread("resumed", false, false);
    let old = f.rollout(
        &f.0.join("sessions/2026/10/19/rollout-a-old.jsonl"),
        "resumed",
        true,
    );
    f.rollout(&current, "resumed", false);
    {
        let mut file = fs::OpenOptions::new().append(true).open(&current).unwrap();
        writeln!(file, "{}", json!({"type":"event_msg","timestamp":"2026-10-29T01:02:03Z","payload":{"type":"agent_message","phase":"final_answer","message":"Resumed answer"}})).unwrap();
    }
    f.project(
        "resumed",
        &[user("old-question", "Old question"), agent("old-answer")],
    );
    #[cfg(windows)]
    {
        // Match the Windows aliases used by Codex's state database in practice.
        let alias = fs::canonicalize(&current)
            .unwrap()
            .to_string_lossy()
            .to_ascii_uppercase();
        f.state()
            .execute("UPDATE threads SET rollout_path=?1 WHERE id='resumed'", [alias])
            .unwrap();
    }
    let before = (fs::read(&old).unwrap(), fs::read(&current).unwrap());
    let page = session_page_in(&f.0, "/project", 0, 10, false, false).unwrap();
    assert_eq!(page.total, 1);
    let session = &page.sessions[0];
    assert_eq!(Path::new(&session.path), current.as_path());
    assert_eq!(session.message_count, 3);
    assert_eq!(
        session.last_reply_at,
        Some(crate::util::parse_iso8601_ms("2026-10-29T01:02:03Z").unwrap() as u64)
    );
    let messages = read_in(&f.0, &session.path).unwrap();
    assert!(messages.iter().any(|m| m
        .blocks
        .iter()
        .any(|b| b.text.as_deref() == Some("Resumed answer"))));
    assert_eq!(fs::read(&old).unwrap(), before.0);
    assert_eq!(fs::read(&current).unwrap(), before.1);
}

#[test]
fn current_rollout_wins_over_an_older_raw_file_with_the_same_thread_id() {
    let f = Fixture::new();
    let current = f.thread("raw-copy", false, false);
    f.rollout(&current, "raw-copy", false);
    // Written later and sorted earlier, but not the current file in state.
    f.rollout(
        &f.0.join("sessions/2026/10/19/rollout-a-obsolete.jsonl"),
        "raw-copy",
        false,
    );
    let page = session_page_in(&f.0, "/project", 0, 10, false, false).unwrap();
    assert_eq!(page.total, 1);
    assert_eq!(Path::new(&page.sessions[0].path), current.as_path());
}

#[test]
fn current_compressed_rollout_can_outrank_an_unindexed_raw_copy() {
    let f = Fixture::new();
    let current_raw = f.thread("compressed-current", false, false);
    let current = f.rollout(&current_raw, "compressed-current", true);
    f.rollout(
        &f.0.join("sessions/2026/10/19/rollout-a-obsolete.jsonl"),
        "compressed-current",
        false,
    );
    let page = session_page_in(&f.0, "/project", 0, 10, false, false).unwrap();
    assert_eq!(page.total, 1);
    assert_eq!(Path::new(&page.sessions[0].path), current.as_path());
}

#[test]
fn raw_rollout_beats_a_differently_named_compressed_copy_without_state_metadata() {
    let f = Fixture::new();
    let current = f.rollout(
        &f.0.join("sessions/rollout-z-current.jsonl"),
        "orphan-copy",
        false,
    );
    f.rollout(&f.0.join("sessions/rollout-a-old.jsonl"), "orphan-copy", true);
    let page = session_page_in(&f.0, "/project", 0, 10, false, false).unwrap();
    assert_eq!(page.total, 1);
    assert_eq!(Path::new(&page.sessions[0].path), current.as_path());
}

#[test]
fn a_missing_state_rollout_falls_back_to_an_existing_raw_copy() {
    let f = Fixture::new();
    f.thread("missing-current", false, false);
    let fallback = f.rollout(
        &f.0.join("sessions/rollout-z-fallback.jsonl"),
        "missing-current",
        false,
    );
    f.rollout(&f.0.join("sessions/rollout-a-old.jsonl"), "missing-current", true);
    let page = session_page_in(&f.0, "/project", 0, 10, false, false).unwrap();
    assert_eq!(page.total, 1);
    assert_eq!(Path::new(&page.sessions[0].path), fallback.as_path());
}

#[test]
fn archive_and_internal_visibility_preserves_existing_independent_switch_semantics() {
    let f = Fixture::new();
    for (id, archived, internal) in [
        ("normal", false, false),
        ("internal", false, true),
        ("archived", true, false),
        ("both", true, true),
    ] {
        let raw = f.thread(id, archived, internal);
        f.rollout(&raw, id, true);
        f.project(id, &[user("u", "test")]);
    }
    assert_eq!(discover_sessions_in(&f.0, false, false).unwrap().len(), 1);
    assert_eq!(discover_sessions_in(&f.0, true, false).unwrap().len(), 2);
    assert_eq!(discover_sessions_in(&f.0, false, true).unwrap().len(), 3);
    assert_eq!(discover_sessions_in(&f.0, true, true).unwrap().len(), 4);
}

#[test]
fn archived_orphan_zstd_is_discovered_without_any_database() {
    let f = Fixture::new();
    let raw = f.0.join("archived_sessions/orphan.jsonl");
    f.rollout(&raw, "orphan", true);
    assert!(discover_sessions_in(&f.0, true, false).unwrap().is_empty());
    let page = session_page_in(&f.0, "/project", 0, 10, false, true).unwrap();
    assert_eq!(page.total, 1);
    assert_eq!(page.sessions[0].message_count, 2);
    assert!(page.sessions[0].codex_archived);
}

#[test]
fn mixed_storage_pages_use_total_order_and_authoritative_title_index() {
    let f = Fixture::new();
    let zipped = f.thread("first", false, false);
    f.rollout(&zipped, "first", true);
    f.project("first", &[user("u1", "first"), agent("a1")]);
    f.thread("second", false, false);
    f.project("second", &[user("u2", "second")]);
    f.state()
        .execute("UPDATE threads SET updated_at = 9 WHERE id = 'second'", [])
        .unwrap();
    fs::write(
        f.0.join("session_index.jsonl"),
        json!({"id":"second","thread_name":"Renamed in Codex"}).to_string(),
    )
    .unwrap();
    let page = session_page_in(&f.0, "/project", 0, 1, false, false).unwrap();
    assert_eq!(page.total, 2);
    assert_eq!(page.sessions[0].id, "second");
    assert_eq!(page.sessions[0].title, "Renamed in Codex");
    assert_eq!(page.sessions[0].message_count, 1);
    let next = session_page_in(&f.0, "/project", 1, 1, false, false).unwrap();
    assert_eq!(next.sessions[0].id, "first");
    assert_eq!(next.sessions[0].message_count, 2);
    assert!(session_page_in(&f.0, "/other", 0, 10, true, true)
        .unwrap()
        .sessions
        .is_empty());
}

#[test]
fn paginated_items_preserve_user_images_reasoning_tools_order_ids_and_timestamps() {
    let f = Fixture::new();
    f.thread("projected", false, false);
    f.project("projected", &[
        json!({"type":"userMessage","id":"u","content":[{"type":"text","text":"你好"},{"type":"image","url":"data:image/png;base64,AQID"}]}),
        json!({"type":"reasoning","id":"r","summary":["Think carefully"],"content":[]}),
        json!({"type":"commandExecution","id":"cmd","command":"pwd","aggregatedOutput":"/project","exitCode":0,"status":"completed"}),
        json!({"type":"mcpToolCall","id":"mcp","server":"demo","tool":"read","arguments":{},"status":"completed","result":{"content":[{"type":"text","text":"Result"}]}}),
        json!({"type":"fileChange","id":"file","status":"completed","changes":[{"path":"/project/a.txt","kind":{"type":"update"},"diff":"@@ -1 +1 @@\n-old\n+new\n"}]}),
        json!({"type":"plan","id":"plan","text":"Do the work"}),
        agent("answer"),
        json!({"type":"contextCompaction","id":"compact"}),
    ]);
    let messages = read_history(&f.0, "projected").unwrap();
    assert_eq!(messages.len(), 7);
    assert_eq!(messages[0].uuid.as_deref(), Some("u"));
    assert_eq!(messages[0].role, "user");
    assert_eq!(
        messages[0].timestamp.as_deref(),
        Some("1970-01-01T00:00:01.000Z")
    );
    assert!(messages[0].blocks.iter().any(|b| b.kind == "image"));
    assert_eq!(messages[1].blocks[0].kind, "thinking");
    assert_eq!(messages[2].blocks[0].tool_name.as_deref(), Some("shell"));
    assert!(messages[2]
        .blocks
        .iter()
        .any(|b| b.kind == "tool_result" && b.text.as_deref() == Some("/project")));
    assert!(messages[4]
        .blocks
        .iter()
        .any(|b| b.file_path.as_deref() == Some("/project/a.txt")));
    assert_eq!(messages[6].model.as_deref(), Some("test-model"));
    assert!(messages
        .windows(2)
        .all(|pair| pair[0].timestamp <= pair[1].timestamp));
}

#[test]
fn saved_tab_can_follow_file_migration_to_database_only_history() {
    let f = Fixture::new();
    let raw = f.thread("moved", false, false);
    f.project("moved", &[user("u", "still here"), agent("a")]);
    for path in [
        raw.to_string_lossy().into_owned(),
        format!("{}.zst", raw.display()),
        "codex://moved".into(),
    ] {
        assert_eq!(read_in(&f.0, &path).unwrap().len(), 2);
    }
}

#[test]
fn empty_projected_history_is_distinguished_from_a_missing_history_id() {
    let f = Fixture::new();
    f.thread("empty", false, false);
    f.project("empty", &[]);
    assert!(read_history(&f.0, "empty").unwrap().is_empty());
    assert!(read_history(&f.0, "missing").is_err());
}

#[test]
fn corrupt_history_item_is_reported_not_silently_treated_as_empty() {
    let f = Fixture::new();
    f.thread("bad", false, false);
    f.project("bad", &[]);
    f.history().execute("INSERT INTO thread_items(thread_id,item_id,rollout_ordinal,created_at_ms,item_json) VALUES ('bad','x',0,0,'not json')", []).unwrap();
    assert!(read_history(&f.0, "bad")
        .err()
        .unwrap()
        .contains("Invalid Codex history item"));
}

#[test]
fn virtual_paths_reject_traversal_empty_ids_and_sql_text() {
    for path in [
        "codex://",
        "codex://../state",
        "codex://x?foo",
        "codex://x' OR 1=1",
        "/tmp/codex://x",
    ] {
        assert!(virtual_id(path).is_err(), "{path}");
    }
    assert_eq!(
        virtual_id("codex://01a1093e-b608-7d32-85f8-11089689129e").unwrap(),
        "01a1093e-b608-7d32-85f8-11089689129e"
    );
}

#[test]
fn wal_writes_invalidate_cache_and_are_visible_to_read_only_history_reader() {
    let f = Fixture::new();
    f.thread("live", false, false);
    f.project("live", &[user("u", "before")]);
    let writer = f.history();
    writer.pragma_update(None, "journal_mode", "WAL").unwrap();
    let before = source_revision(&f.0, "codex://live");
    writer.execute("INSERT INTO thread_items(thread_id,item_id,rollout_ordinal,created_at_ms,item_json) VALUES ('live','a',2,2000,?1)", [agent("a").to_string()]).unwrap();
    assert_ne!(before, source_revision(&f.0, "codex://live"));
    assert_eq!(read_history(&f.0, "live").unwrap().len(), 2);
    assert!(watch_targets(&f.0, "codex://live")
        .iter()
        .any(|p| p.to_string_lossy().ends_with("-wal")));
    assert!(watch_targets(&f.0, "codex://live")
        .iter()
        .all(|p| p.is_file()));
}

#[test]
fn managed_history_cannot_be_partially_deleted_and_resurrected_from_the_projection() {
    let f = Fixture::new();
    let raw = f.thread("managed", false, false);
    f.rollout(&raw, "managed", false);
    f.project("managed", &[user("u", "protected")]);
    assert!(validate_file_delete(&f.0, &raw).is_err());
    let legacy = f.thread("legacy", false, false);
    f.rollout(&legacy, "legacy", false);
    assert!(validate_file_delete(&f.0, &legacy).is_ok());
    let source = CodexSource;
    assert!(source
        .validate_session_path(Path::new("codex://managed"))
        .is_err());
    assert!(source
        .validate_session_path(Path::new("/missing.jsonl.zst"))
        .is_err());
    assert!(raw.is_file());
}

#[test]
fn resume_validation_requires_native_rollout_not_just_projected_display_items() {
    let f = Fixture::new();
    let missing = f.thread("only-history", false, false);
    f.project(
        "only-history",
        &[user("u", "viewable but not full native context")],
    );
    assert!(validate_resume(&f.0, "only-history", "codex://only-history").is_err());
    assert!(validate_resume(&f.0, "only-history", "").is_err());
    // A caller cannot bypass the ID-based check with an unrelated file path.
    let legacy = f.0.join("sessions/legacy.jsonl");
    f.rollout(&legacy, "legacy", false);
    assert!(validate_resume(&f.0, "only-history", legacy.to_str().unwrap()).is_err());
    assert!(validate_resume(&f.0, "legacy", legacy.to_str().unwrap()).is_ok());
    let zipped = f.thread("zipped-resume", false, false);
    f.rollout(&zipped, "zipped-resume", true);
    assert!(validate_resume(&f.0, "zipped-resume", "").is_ok());
    // Once the real source is restored, the backend can resume an older tab.
    f.rollout(&missing, "only-history", true);
    assert!(validate_resume(&f.0, "only-history", "codex://only-history").is_ok());
}

#[test]
fn resume_keeps_original_thread_id_and_does_not_fork_or_use_virtual_path() {
    let source = CodexSource;
    for path in [
        "codex://original-id",
        "/sessions/rollout-original-id.jsonl.zst",
    ] {
        let command = source.resume_command("original-id", path);
        assert_eq!(command.program(), "codex");
        assert_eq!(command.args(), ["resume", "original-id"]);
    }
    assert_eq!(
        source.usage_summary("codex://original-id").unwrap().total,
        0
    );
    assert!(source.read_turns("codex://original-id").unwrap().is_empty());
}

#[test]
fn search_prefilter_reads_unicode_user_text_from_compressed_and_database_storage() {
    let f = Fixture::new();
    let raw = f.thread("zipped-search", false, false);
    let zipped = f.rollout(&raw, "zipped-search", true);
    assert!(super::super::contains_user_text_in(
        &f.0,
        &zipped.to_string_lossy(),
        "中文"
    ));
    assert!(!super::super::contains_user_text_in(
        &f.0,
        &zipped.to_string_lossy(),
        "found the session"
    ));
    f.thread("db-search", false, false);
    f.project("db-search", &[user("u", "数据库里的独特问题"), agent("a")]);
    assert!(super::super::contains_user_text_in(
        &f.0,
        "codex://db-search",
        "独特问题"
    ));
    assert!(!super::super::contains_user_text_in(
        &f.0,
        "codex://db-search",
        "stored answer"
    ));
}

#[test]
fn complete_trash_roundtrips_raw_compressed_and_database_only_sessions() {
    use super::super::trash_storage::{delete, preview, restore};
    for kind in ["raw", "zipped", "database", "raw-and-zipped", "legacy"] {
        let f = Fixture::new();
        let raw = f.thread("target", false, false);
        let untouched = f.thread("untouched", false, false);
        f.rollout(&untouched, "untouched", false);
        let mut originals = Vec::new();
        if matches!(kind, "raw" | "raw-and-zipped" | "legacy") {
            originals.push(f.rollout(&raw, "target", false));
        }
        if matches!(kind, "zipped" | "raw-and-zipped") {
            originals.push(f.rollout(&raw, "target", true));
        }
        if kind != "legacy" {
            f.project("target", &[user("u", "recover me"), agent("a")]);
        }
        f.project("untouched", &[user("u", "keep me")]);
        let state = f.state();
        state.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE thread_attachments(thread_id TEXT, payload BLOB); INSERT INTO thread_attachments VALUES('target', X'00ff10'); INSERT INTO thread_attachments VALUES('untouched', X'12'); CREATE TABLE thread_spawn_edges(parent_thread_id TEXT, child_thread_id TEXT); INSERT INTO thread_spawn_edges VALUES('untouched','target');").unwrap();
        f.history().execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE thread_turns(thread_id TEXT, turn_id TEXT); INSERT INTO thread_turns VALUES('target','turn'); CREATE TABLE thread_realtime_items(thread_id TEXT, item_id TEXT); INSERT INTO thread_realtime_items VALUES('target','realtime');").unwrap();
        let index = "{\"id\":\"untouched\",\"thread_name\":\"Keep\"}\n{\"id\":\"target\",\"thread_name\":\"Saved title\"}\n";
        fs::write(f.0.join("session_index.jsonl"), index).unwrap();
        fs::write(
            f.0.join("history.jsonl"),
            "{\"session_id\":\"target\",\"text\":\"prompt\"}\n{\"session_id\":\"untouched\"}\n",
        )
        .unwrap();
        let bytes: Vec<_> = originals.iter().map(|p| fs::read(p).unwrap()).collect();
        let path = if kind == "database" {
            "codex://target".into()
        } else {
            originals[0].to_string_lossy().into_owned()
        };
        let bundle = f.0.join("trash-test");
        delete(&f.0, &path, &bundle).unwrap();
        assert!(originals.iter().all(|p| !p.exists()), "{kind}");
        assert!(discover_sessions_in(&f.0, false, false)
            .unwrap()
            .iter()
            .all(|s| s.meta.id != "target"));
        assert_eq!(threads(&f.0).unwrap().len(), 1);
        assert_eq!(
            state
                .query_row(
                    "SELECT COUNT(*) FROM thread_attachments WHERE thread_id='target'",
                    [],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
            0
        );
        assert!(!fs::read_to_string(f.0.join("history.jsonl"))
            .unwrap()
            .contains("target"));
        let (title, messages) = preview(&bundle.join("session.jsonl")).unwrap().unwrap();
        assert_eq!(title, "Saved title");
        assert!(!messages.is_empty());
        assert_eq!(
            CodexSource
                .read_session(&bundle.join("session.jsonl").to_string_lossy())
                .unwrap()
                .len(),
            messages.len()
        );
        // Changes to other sessions while this one is in trash must survive.
        state
            .execute(
                "UPDATE threads SET title='Changed elsewhere' WHERE id='untouched'",
                [],
            )
            .unwrap();
        fs::OpenOptions::new()
            .append(true)
            .open(f.0.join("session_index.jsonl"))
            .unwrap()
            .write_all(b"{\"id\":\"third\",\"thread_name\":\"New entry\"}\n")
            .unwrap();
        restore(&f.0, &bundle).unwrap();
        for (path, before) in originals.iter().zip(&bytes) {
            assert_eq!(&fs::read(path).unwrap(), before, "{kind}");
        }
        assert_eq!(threads(&f.0).unwrap().len(), 2);
        assert_eq!(
            state
                .query_row(
                    "SELECT payload FROM thread_attachments WHERE thread_id='target'",
                    [],
                    |r| r.get::<_, Vec<u8>>(0)
                )
                .unwrap(),
            vec![0, 255, 16]
        );
        assert_eq!(
            state
                .query_row("SELECT title FROM threads WHERE id='untouched'", [], |r| {
                    r.get::<_, String>(0)
                })
                .unwrap(),
            "Changed elsewhere"
        );
        assert!(fs::read_to_string(f.0.join("session_index.jsonl"))
            .unwrap()
            .contains("third"));
        if kind != "legacy" {
            assert_eq!(read_history(&f.0, "target").unwrap().len(), 2);
        }
        assert!(
            restore(&f.0, &bundle).is_err(),
            "must never overwrite an existing session"
        );
    }
}

#[test]
fn complete_trash_rolls_back_on_database_failure_and_keeps_restore_conflicts_recoverable() {
    use super::super::trash_storage::{delete, restore};
    let f = Fixture::new();
    let raw = f.thread("target", false, false);
    f.rollout(&raw, "target", false);
    f.project("target", &[user("u", "protected")]);
    let state = f.state();
    state.execute_batch("CREATE TRIGGER refuse_delete BEFORE DELETE ON threads BEGIN SELECT RAISE(ABORT,'test failure'); END;").unwrap();
    let bundle = f.0.join("trash-test");
    assert!(delete(&f.0, raw.to_str().unwrap(), &bundle).is_err());
    assert!(raw.exists());
    assert!(!bundle.exists());
    assert_eq!(read_history(&f.0, "target").unwrap().len(), 1);
    state.execute_batch("DROP TRIGGER refuse_delete;").unwrap();
    delete(&f.0, raw.to_str().unwrap(), &bundle).unwrap();
    fs::write(&raw, b"a new rollout").unwrap();
    assert!(restore(&f.0, &bundle).is_err());
    assert_eq!(fs::read(&raw).unwrap(), b"a new rollout");
    assert!(bundle.join("manifest.json").exists());
    fs::remove_file(&raw).unwrap();
    state.execute_batch("CREATE TRIGGER refuse_insert BEFORE INSERT ON threads BEGIN SELECT RAISE(ABORT,'test failure'); END;").unwrap();
    assert!(restore(&f.0, &bundle).is_err());
    assert!(!raw.exists());
    assert!(threads(&f.0).unwrap().is_empty());
    state.execute_batch("DROP TRIGGER refuse_insert;").unwrap();
    fs::write(f.0.join("state_6.sqlite"), b"new schema").unwrap();
    assert!(restore(&f.0, &bundle)
        .unwrap_err()
        .contains("schema changed"));
    fs::remove_file(f.0.join("state_6.sqlite")).unwrap();
    restore(&f.0, &bundle).unwrap();
    assert!(raw.exists());
}

#[test]
fn complete_trash_rejects_outside_paths_and_corrupted_backups() {
    use super::super::trash_storage::{delete, restore};
    let f = Fixture::new();
    let outside = f.0.join("outside.jsonl");
    f.rollout(&outside, "outside", false);
    assert!(delete(&f.0, outside.to_str().unwrap(), &f.0.join("bad")).is_err());
    assert!(outside.exists());
    let raw = f.thread("target", false, false);
    f.rollout(&raw, "target", true);
    f.project("target", &[user("u", "protected")]);
    let bundle = f.0.join("trash-test");
    delete(&f.0, "codex://target", &bundle).unwrap();
    let zipped = PathBuf::from(format!("{}.zst", raw.strip_prefix(&f.0).unwrap().display()));
    fs::write(bundle.join(zipped), b"corrupt").unwrap();
    assert!(restore(&f.0, &bundle).unwrap_err().contains("corrupted"));
    assert!(threads(&f.0).unwrap().is_empty());
}

#[test]
fn zstd_stream_decoding_does_not_require_an_external_executable() {
    let f = Fixture::new();
    let path = f.rollout(&f.0.join("sessions/test.jsonl"), "test", true);
    let mut text = String::new();
    open_rollout(&path)
        .unwrap()
        .read_to_string(&mut text)
        .unwrap();
    let header: Value = serde_json::from_str(text.lines().next().unwrap()).unwrap();
    assert_eq!(header["type"], "session_meta");
}
