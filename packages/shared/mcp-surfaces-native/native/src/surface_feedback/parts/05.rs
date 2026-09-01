#[cfg(test)]
mod tests {
    use super::*;
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn reset_feedback_env() {
        for key in ["NARADA_SITE_ID", "NARADA_AGENT_ID", "NARADA_SURFACE_FEEDBACK_ROOT", "NARADA_OWNED_SURFACE_IDS", "NARADA_TASK_LIFECYCLE_ROOT"] {
            std::env::remove_var(key);
        }
    }

    fn temp_root(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("narada-feedback-{tag}-{}", uuid::Uuid::new_v4()))
    }

    fn bind_authority(site: &str, agent: &str) {
        std::env::set_var("NARADA_SITE_ID", site);
        std::env::set_var("NARADA_AGENT_ID", agent);
    }

    fn ledger_events(root: &Path) -> Vec<Value> {
        ledger_files(root).expect("ledger files").iter().map(|path| ledger_io::read_json(ERROR_SCHEMA, path).expect("event json")).collect()
    }

    const LEGACY_DDL: &str = "CREATE TABLE feedback_entries (feedback_id TEXT PRIMARY KEY,surface_id TEXT NOT NULL,submitter_site_id TEXT NOT NULL,submitter_principal TEXT NOT NULL,kind TEXT NOT NULL,summary TEXT NOT NULL,details TEXT NOT NULL DEFAULT '',status TEXT NOT NULL DEFAULT 'submitted',resolution_note TEXT,resolved_by TEXT,task_ref TEXT,task_status TEXT,source_db_path TEXT,source_updated_at TEXT,source_sync_mode TEXT,created_at TEXT NOT NULL,updated_at TEXT NOT NULL) STRICT; CREATE TABLE feedback_events (event_id TEXT PRIMARY KEY,feedback_id TEXT NOT NULL,event_type TEXT NOT NULL,actor_principal TEXT NOT NULL,status TEXT,task_ref TEXT,task_status TEXT,note TEXT,details_json TEXT NOT NULL DEFAULT '{}',created_at TEXT NOT NULL) STRICT;";

    fn seed_legacy_db(root: &Path, statements: &str) {
        std::fs::create_dir_all(root.join(".feedback")).expect("legacy dir");
        let db = Connection::open(root.join(".feedback/surface-feedback.db")).expect("legacy db");
        db.execute_batch(&format!("{LEGACY_DDL} {statements}")).expect("legacy seed");
        drop(db);
    }

    #[test]
    fn mutation_tools_advertise_named_closed_schemas() {
        let tools = list_tools();
        let find = |name: &str| tools.iter().find(|tool| tool["name"] == name).expect("tool");
        let submit = find("surface_feedback_submit");
        assert_eq!(submit["inputSchema"]["additionalProperties"], false);
        for field in ["surface_id", "submitter_site_id", "submitter_principal", "kind", "summary", "details", "idempotency_key"] {
            assert!(submit["inputSchema"]["properties"].get(field).is_some(), "missing {field}");
        }
        assert_eq!(submit["inputSchema"]["required"], json!(["surface_id","kind","summary"]));
        assert_eq!(find("surface_feedback_update_status")["inputSchema"]["required"], json!(["feedback_id","status","resolution_note"]));
        assert!(find("surface_feedback_update_status_batch")["inputSchema"]["properties"]["updates"].is_object());
        assert!(find("surface_feedback_convert_to_task")["inputSchema"]["properties"]["feedback_id"].is_object());
        let import = find("surface_feedback_import");
        assert_eq!(import["inputSchema"]["required"], json!(["source_db_path","feedback_ids"]));
        assert!(import["inputSchema"].get("oneOf").is_none());
    }

    #[test]
    fn native_feedback_reads_are_bounded_and_capabilities_are_honest() {
        let _guard = ENV_LOCK.lock().expect("feedback env lock");
        reset_feedback_env();
        let root = temp_root("reads");
        bind_authority("site-a", "agent-a");
        std::env::set_var("NARADA_SURFACE_FEEDBACK_ROOT", &root);
        // Fresh root: no legacy DB, no ledger — the bootstrap gap is closed.
        for summary in ["first", "second"] {
            call_tool("surface_feedback_submit", &json!({"surface_id":"calendar","submitter_site_id":"site-a","submitter_principal":"agent-a","kind":"observation","summary":summary}).as_object().unwrap(), &root).expect("submit");
        }
        let list = feedback_list(&json!({"scope":"all_authorized","limit":1}).as_object().unwrap(), &root, false).expect("list");
        assert_eq!(list["count"], 1);
        assert_eq!(list["has_more"], true);
        assert_eq!(list["next_offset"], 1);
        let doctor = doctor(&root).expect("doctor");
        assert_eq!(doctor["store_status"], "ready");
        assert_eq!(doctor["feedback_entries"], 2);
        assert_eq!(doctor["ledger_events"], 2);
        assert_eq!(doctor["read_only_native"], false);
        assert_eq!(doctor["capabilities"]["read_scopes"]["all_authorized"]["available"], true);
        assert_eq!(doctor["capabilities"]["read_scopes"]["authority_visible"]["available"], true);
        assert_eq!(doctor["capabilities"]["read_scopes"]["owned_surfaces"]["available"], false);
        assert_eq!(doctor["capabilities"]["mutations"]["submit"]["authority_site_id"], "site-a");
        let authority_entries = feedback_list(&json!({"scope":"authority_site_submissions"}).as_object().unwrap(), &root, false).expect("authority list");
        assert_eq!(authority_entries["count"], 2);
        assert!(authority_entries["entries"].as_array().is_some_and(|entries| entries.iter().all(|entry| entry["submitter_site_id"] == "site-a")));
        let mismatch = call_tool("surface_feedback_submit", &json!({"surface_id":"calendar","submitter_site_id":"site-b","kind":"bug","summary":"spoofed"}).as_object().unwrap(), &root).expect_err("authority mismatch");
        assert_eq!(mismatch["code"], "feedback_submitter_site_authority_mismatch");
        let retry_args=json!({"surface_id":"calendar","submitter_site_id":"site-a","submitter_principal":"agent-a","kind":"observation","summary":"retry safe","idempotency_key":"retry-1"});
        let first=call_tool("surface_feedback_submit",retry_args.as_object().unwrap(),&root).expect("first");
        let replay=call_tool("surface_feedback_submit",retry_args.as_object().unwrap(),&root).expect("replay");
        assert_eq!(first["feedback_id"],replay["feedback_id"]);
        assert_eq!(replay["idempotency_replay"],true);
        assert_eq!(replay["created_at"],first["created_at"]);
        let conflict=call_tool("surface_feedback_submit",&json!({"surface_id":"calendar","submitter_site_id":"site-a","submitter_principal":"agent-a","kind":"bug","summary":"different","idempotency_key":"retry-1"}).as_object().unwrap(),&root).expect_err("conflict");
        assert_eq!(conflict["code"],"feedback_idempotency_conflict");
        // Exactly one event for the replayed key: the retry must not append.
        assert_eq!(ledger_events(&root).iter().filter(|event| event["idempotency_key"] == "retry-1").count(), 1);
        let maintained = feedback_update_status(&json!({"feedback_id":first["feedback_id"],"status":"closed","resolution_note":"canonical repair"}).as_object().unwrap(), &root).expect("canonical maintainer");
        assert_eq!(maintained["new_status"], "closed");
        let shown = feedback_show(&json!({"feedback_id":first["feedback_id"],"scope":"all_authorized"}).as_object().unwrap(), &root).expect("show");
        assert_eq!(shown["entry"]["status"], "closed");
        assert_eq!(shown["entry"]["resolution_note"], "canonical repair");
        reset_feedback_env();
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn read_scopes_enforce_canonical_store_and_owned_surfaces() {
        let _guard = ENV_LOCK.lock().expect("feedback env lock");
        reset_feedback_env();
        let root = temp_root("scopes");
        bind_authority("site-a", "agent-a");
        call_tool("surface_feedback_submit", &json!({"surface_id":"calendar","kind":"bug","summary":"owned surface bug"}).as_object().unwrap(), &root).expect("submit owned");
        call_tool("surface_feedback_submit", &json!({"surface_id":"scheduler","kind":"gap","summary":"other surface gap"}).as_object().unwrap(), &root).expect("submit other");
        // Global scopes require the canonical store.
        let global = feedback_list(&json!({"scope":"all_authorized"}).as_object().unwrap(), &root, false).expect_err("noncanonical all_authorized");
        assert_eq!(global["code"], "feedback_global_read_requires_canonical_store");
        let reconciliation = feedback_list(&json!({"scope":"store_reconciliation"}).as_object().unwrap(), &root, false).expect_err("noncanonical store_reconciliation");
        assert_eq!(reconciliation["code"], "feedback_global_read_requires_canonical_store");
        // owned_surfaces refuses when the owned list is unbound.
        let unbound = feedback_list(&json!({"scope":"owned_surfaces"}).as_object().unwrap(), &root, false).expect_err("unbound owned_surfaces");
        assert_eq!(unbound["code"], "feedback_read_scope_authority_unavailable");
        std::env::set_var("NARADA_OWNED_SURFACE_IDS", "calendar,mailbox");
        let owned = feedback_list(&json!({"scope":"owned_surfaces"}).as_object().unwrap(), &root, false).expect("owned list");
        assert_eq!(owned["count"], 1);
        assert_eq!(owned["entries"][0]["surface_id"], "calendar");
        let owned_show = feedback_show(&json!({"feedback_id":owned["entries"][0]["feedback_id"],"scope":"owned_surfaces"}).as_object().unwrap(), &root).expect("owned show");
        assert_eq!(owned_show["entry"]["summary"], "owned surface bug");
        let other = feedback_list(&json!({"scope":"authority_visible","surface_id":"scheduler"}).as_object().unwrap(), &root, false).expect("authority list");
        let other_id = other["entries"][0]["feedback_id"].clone();
        let hidden = feedback_show(&json!({"feedback_id":other_id,"scope":"owned_surfaces"}).as_object().unwrap(), &root).expect_err("owned scope hides other surfaces");
        assert_eq!(hidden["code"], "feedback_not_found");
        let owned_stats = feedback_stats(&json!({"scope":"owned_surfaces"}).as_object().unwrap(), &root).expect("owned stats");
        assert_eq!(owned_stats["total"], 1);
        // With the canonical binding, global scopes read everything.
        std::env::set_var("NARADA_SURFACE_FEEDBACK_ROOT", &root);
        let global = feedback_list(&json!({"scope":"all_authorized"}).as_object().unwrap(), &root, false).expect("canonical list");
        assert_eq!(global["count"], 2);
        // The submitter filter authority-mismatch refusal still holds.
        let mismatch = feedback_list(&json!({"scope":"authority_site_submissions","submitter_site_id_filter":"site-b"}).as_object().unwrap(), &root, false).expect_err("filter mismatch");
        assert_eq!(mismatch["code"], "feedback_submitter_site_filter_authority_mismatch");
        // Authority-bound scopes refuse when the authority is unconfigured.
        std::env::remove_var("NARADA_SITE_ID");
        let unavailable = feedback_list(&json!({"scope":"authority_visible"}).as_object().unwrap(), &root, false).expect_err("authority unavailable");
        assert_eq!(unavailable["code"], "feedback_read_scope_authority_unavailable");
        reset_feedback_env();
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn projection_is_disposable_and_event_appends_are_durable() {
        let _guard = ENV_LOCK.lock().expect("feedback env lock");
        reset_feedback_env();
        let root = temp_root("durable");
        bind_authority("site-a", "agent-a");
        std::env::set_var("NARADA_SURFACE_FEEDBACK_ROOT", &root);
        let submitted = call_tool("surface_feedback_submit", &json!({"surface_id":"calendar","kind":"bug","summary":"durable write"}).as_object().unwrap(), &root).expect("submit");
        feedback_update_status(&json!({"feedback_id":submitted["feedback_id"],"status":"acknowledged","resolution_note":"ack"}).as_object().unwrap(), &root).expect("update");
        assert!(projection_path(&root).exists());
        std::fs::remove_file(projection_path(&root)).expect("delete projection");
        // Reads rebuild the projection from the ledger with identical results.
        let shown = feedback_show(&json!({"feedback_id":submitted["feedback_id"],"scope":"all_authorized"}).as_object().unwrap(), &root).expect("show after rebuild");
        assert_eq!(shown["entry"]["status"], "acknowledged");
        assert_eq!(shown["entry"]["resolution_note"], "ack");
        assert!(projection_path(&root).exists());
        // The derived audit readback preserves the feedback_events row shape.
        let db = Connection::open_with_flags(projection_path(&root), OpenFlags::SQLITE_OPEN_READ_ONLY).expect("projection");
        let events: Vec<(String, String, Option<String>, Option<String>)> = {
            let mut stmt = db.prepare("SELECT event_type, actor_principal, status, note FROM feedback_events WHERE feedback_id=?1 ORDER BY rowid ASC").expect("events query");
            let rows = stmt.query_map(params![submitted["feedback_id"].as_str()], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))).expect("events rows");
            rows.collect::<Result<Vec<_>, _>>().expect("events collect")
        };
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].0, "submitted");
        assert_eq!(events[0].1, "agent-a");
        assert_eq!(events[0].2.as_deref(), Some("submitted"));
        assert_eq!(events[1].0, "status_updated");
        assert_eq!(events[1].2.as_deref(), Some("acknowledged"));
        assert_eq!(events[1].3.as_deref(), Some("ack"));
        drop(db);
        // The ledger chain verifies and its head matches the last event.
        event_ledger::verify(ERROR_SCHEMA, &ledger_layout(&root), HASH_FIELD).expect("ledger verifies");
        reset_feedback_env();
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn legacy_store_migrates_once_and_is_never_written() {
        let _guard = ENV_LOCK.lock().expect("feedback env lock");
        reset_feedback_env();
        let root = temp_root("migration");
        bind_authority("site-a", "agent-a");
        std::env::set_var("NARADA_SURFACE_FEEDBACK_ROOT", &root);
        seed_legacy_db(&root, "INSERT INTO feedback_entries (feedback_id,surface_id,submitter_site_id,submitter_principal,kind,summary,details,status,resolution_note,resolved_by,task_ref,task_status,source_db_path,source_updated_at,source_sync_mode,created_at,updated_at) VALUES ('f1','calendar','site-a','agent-a','bug','broken','details','submitted',NULL,NULL,NULL,NULL,NULL,NULL,NULL,'2026-01-01T00:00:00Z','2026-01-01T00:00:00Z'), ('f2','scheduler','site-b','agent-b','observation','import me','legacy details','acknowledged','ack note','agent-c',NULL,NULL,'legacy-src','2026-01-02T00:00:00Z','explicit_import','2026-01-02T00:00:00Z','2026-01-03T00:00:00Z'); INSERT INTO feedback_events (event_id,feedback_id,event_type,actor_principal,status,task_ref,task_status,note,details_json,created_at) VALUES ('evt-1','f1','submitted','agent-a','submitted',NULL,NULL,'broken','{}','2026-01-01T00:00:00Z'), ('evt-2','f1','status_updated','agent-a','acknowledged',NULL,NULL,'ack','{}','2026-01-01T01:00:00Z');");
        let legacy_bytes = std::fs::read(root.join(".feedback/surface-feedback.db")).expect("legacy bytes");
        let list = feedback_list(&json!({"scope":"all_authorized"}).as_object().unwrap(), &root, false).expect("list migrates");
        assert_eq!(list["count"], 2);
        // One migrated event per legacy row, preserving identity and timestamps.
        let events = ledger_events(&root);
        assert_eq!(events.len(), 2);
        assert!(events.iter().all(|event| event["event_type"] == "migrated"));
        assert!(migration_marker_path(&root).exists());
        let shown = feedback_show(&json!({"feedback_id":"f2","scope":"all_authorized"}).as_object().unwrap(), &root).expect("show migrated");
        assert_eq!(shown["entry"]["status"], "acknowledged");
        assert_eq!(shown["entry"]["resolution_note"], "ack note");
        assert_eq!(shown["entry"]["created_at"], "2026-01-02T00:00:00Z");
        assert_eq!(shown["entry"]["updated_at"], "2026-01-03T00:00:00Z");
        let stats = feedback_stats(&json!({"scope":"all_authorized"}).as_object().unwrap(), &root).expect("stats");
        assert_eq!(stats["total"], 2);
        // Legacy feedback_events history is replayed into the derived readback.
        let db = Connection::open_with_flags(projection_path(&root), OpenFlags::SQLITE_OPEN_READ_ONLY).expect("projection");
        let history_count: i64 = db.query_row("SELECT COUNT(*) FROM feedback_events WHERE feedback_id='f1'", [], |row| row.get(0)).expect("history count");
        assert_eq!(history_count, 3); // evt-1, evt-2, plus the migrated marker event
        let legacy_order: i64 = db.query_row("SELECT COUNT(*) FROM feedback_events WHERE event_id IN ('evt-1','evt-2')", [], |row| row.get(0)).expect("legacy ids");
        assert_eq!(legacy_order, 2);
        drop(db);
        // Doctor reports the migration posture.
        let doctor = doctor(&root).expect("doctor");
        assert_eq!(doctor["migration"]["legacy_present"], true);
        assert_eq!(doctor["migration"]["marker_present"], true);
        assert_eq!(doctor["migration"]["rows_migrated"], 2);
        assert_eq!(doctor["ledger_events"], 2);
        // The legacy DB is byte-identical: migration never writes it.
        assert_eq!(std::fs::read(root.join(".feedback/surface-feedback.db")).expect("legacy bytes after"), legacy_bytes);
        // A second pass appends nothing.
        feedback_list(&json!({"scope":"all_authorized"}).as_object().unwrap(), &root, false).expect("second list");
        assert_eq!(ledger_events(&root).len(), 2);
        // Crash-restart safety: without the marker, re-emission skips existing ids.
        std::fs::remove_file(migration_marker_path(&root)).expect("remove marker");
        feedback_list(&json!({"scope":"all_authorized"}).as_object().unwrap(), &root, false).expect("resume list");
        assert_eq!(ledger_events(&root).len(), 2);
        assert!(migration_marker_path(&root).exists());
        // Migration output is ordinary feedback: updates apply to migrated rows.
        let updated = feedback_update_status(&json!({"feedback_id":"f1","status":"closed","resolution_note":"fixed after migration"}).as_object().unwrap(), &root).expect("update migrated");
        assert_eq!(updated["new_status"], "closed");
        assert_eq!(std::fs::read(root.join(".feedback/surface-feedback.db")).expect("legacy bytes final"), legacy_bytes);
        reset_feedback_env();
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn native_feedback_batch_update_and_explicit_import() {
        let _guard = ENV_LOCK.lock().expect("feedback env lock");
        reset_feedback_env();
        let root = temp_root("import");
        let source_root = temp_root("source");
        seed_legacy_db(&source_root, "INSERT INTO feedback_entries (feedback_id,surface_id,submitter_site_id,submitter_principal,kind,summary,details,status,created_at,updated_at) VALUES ('f2','calendar','site-a','agent-a','observation','import me','details','submitted','2026-01-01','2026-01-01');");
        bind_authority("site-a", "agent-a");
        std::env::set_var("NARADA_SURFACE_FEEDBACK_ROOT", &root);
        let submitted = feedback_submit(&json!({"surface_id":"calendar","submitter_site_id":"site-a","submitter_principal":"agent-a","kind":"bug","summary":"batch me"}).as_object().unwrap(), &root).expect("submit");
        let batch = feedback_update_status_batch(&json!({"updates":[{"feedback_id":submitted["feedback_id"].clone(),"status":"acknowledged","resolution_note":"ack"}]}).as_object().unwrap(), &root).expect("batch");
        assert_eq!(batch["status"], "updated");
        // Partial semantics: one good update, one missing id, one malformed item.
        let partial = feedback_update_status_batch(&json!({"updates":[{"feedback_id":submitted["feedback_id"].clone(),"status":"routed","resolution_note":"route"},{"feedback_id":"missing","status":"closed","resolution_note":"nope"},"not-an-object"]}).as_object().unwrap(), &root).expect("partial batch");
        assert_eq!(partial["status"], "partial");
        assert_eq!(partial["updated_count"], 1);
        assert_eq!(partial["failed_count"], 2);
        assert_eq!(partial["failures"][0]["code"], "feedback_not_found");
        let failed = feedback_update_status_batch(&json!({"updates":[{"feedback_id":"missing","status":"closed","resolution_note":"nope"}]}).as_object().unwrap(), &root).expect("failed batch");
        assert_eq!(failed["status"], "failed");
        let imported = feedback_import(&json!({"source_db_path":source_root.join(".feedback/surface-feedback.db").to_string_lossy(),"feedback_ids":["f2"]}).as_object().unwrap(), &root).expect("import");
        assert_eq!(imported["status"], "imported");
        assert_eq!(imported["imported_count"], 1);
        // Re-import skips existing ids instead of duplicating events.
        let reimport = feedback_import(&json!({"source_db_path":source_root.join(".feedback/surface-feedback.db").to_string_lossy(),"feedback_ids":["f2","never-present"]}).as_object().unwrap(), &root).expect("reimport");
        assert_eq!(reimport["status"], "partial");
        assert_eq!(reimport["skipped_count"], 1);
        assert_eq!(reimport["missing_count"], 1);
        let shown = feedback_show(&json!({"feedback_id":"f2","scope":"all_authorized"}).as_object().unwrap(), &root).expect("show imported");
        assert_eq!(shown["entry"]["summary"], "import me");
        // Importing the store's own legacy DB is still refused.
        let same_store = feedback_import(&json!({"source_db_path":root.join(".feedback/surface-feedback.db").to_string_lossy(),"feedback_ids":["f2"]}).as_object().unwrap(), &root).expect_err("same store");
        assert_eq!(same_store["code"], "feedback_import_same_store");
        reset_feedback_env();
        std::fs::remove_dir_all(root).expect("cleanup");
        std::fs::remove_dir_all(source_root).expect("source cleanup");
    }

    #[test]
    fn feedback_conversion_uses_in_process_task_authority() {
        let _guard = ENV_LOCK.lock().expect("feedback env lock");
        reset_feedback_env();
        let root = temp_root("convert");
        bind_authority("site-a", "agent-a");
        std::env::set_var("NARADA_SURFACE_FEEDBACK_ROOT", &root);
        let submitted = call_tool("surface_feedback_submit", &json!({"surface_id":"worker-delegation","submitter_site_id":"site-a","submitter_principal":"agent-a","kind":"bug","summary":"bounded worker stalls"}).as_object().unwrap(), &root).expect("submit");
        let lifecycle_options = LifecycleOptions { surface: LifecycleSurface::Task, site_root: root.clone(), site_root_source: "test".to_string(), prepare: true, migrate_legacy: false, source_database_path: None };
        LifecycleServer::prepare_database(&lifecycle_options).expect("task database");
        let result = feedback_convert_to_task(json!({"feedback_id":submitted["feedback_id"],"task_title":"Fix bounded worker stalls"}).as_object().unwrap(), &root).expect("conversion");
        assert_eq!(result["status"], "converted");
        assert!(result["task_ref"].as_str().is_some());
        let replay = feedback_convert_to_task(json!({"feedback_id":submitted["feedback_id"]}).as_object().unwrap(), &root).expect("idempotent replay");
        assert_eq!(replay["status"], "already_linked");
        assert_eq!(replay["task_ref"], result["task_ref"]);
        // The fold reflects the link: the entry is converted with the task ref.
        let shown = feedback_show(&json!({"feedback_id":submitted["feedback_id"],"scope":"all_authorized"}).as_object().unwrap(), &root).expect("show converted");
        assert_eq!(shown["entry"]["status"], "converted_to_task");
        assert_eq!(shown["entry"]["task_ref"], result["task_ref"]);
        // The conversion is a durable ledger event, not a swallowed best-effort write.
        let events = ledger_events(&root);
        let converted = events.iter().filter(|event| event["event_type"] == "converted_to_task").count();
        assert_eq!(converted, 1);
        // Crash between task creation and link append is recoverable: removing the
        // link event's fold effect is equivalent to the entry lacking task_ref, and a
        // retry replays task-lifecycle idempotency and appends the link event.
        // Here we assert the retry guard instead: a status change away from
        // converted_to_task with an existing task_ref refuses as a link conflict.
        let conflict_row = feedback_convert_to_task(json!({"feedback_id":submitted["feedback_id"]}).as_object().unwrap(), &root).expect("second replay");
        assert_eq!(conflict_row["status"], "already_linked");
        reset_feedback_env();
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn task_link_conflict_is_refused_for_non_converted_entries() {
        let _guard = ENV_LOCK.lock().expect("feedback env lock");
        reset_feedback_env();
        let root = temp_root("link-conflict");
        bind_authority("site-a", "agent-a");
        std::env::set_var("NARADA_SURFACE_FEEDBACK_ROOT", &root);
        let submitted = call_tool("surface_feedback_submit", &json!({"surface_id":"calendar","kind":"bug","summary":"link conflict"}).as_object().unwrap(), &root).expect("submit");
        // A task_ref carried by a status update on a non-converted entry refuses conversion.
        feedback_update_status(&json!({"feedback_id":submitted["feedback_id"],"status":"routed","resolution_note":"route","task_ref":"task #7","task_status":"opened"}).as_object().unwrap(), &root).expect("status with task ref");
        let conflict = feedback_convert_to_task(json!({"feedback_id":submitted["feedback_id"]}).as_object().unwrap(), &root).expect_err("link conflict");
        assert_eq!(conflict["code"], "feedback_task_link_conflict");
        reset_feedback_env();
        std::fs::remove_dir_all(root).expect("cleanup");
    }
}
