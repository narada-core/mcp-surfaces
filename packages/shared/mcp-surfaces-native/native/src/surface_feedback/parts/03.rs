fn feedback_list(args: &Map<String, Value>, root: &Path, actionable: bool) -> Result<Value, Value> {
    let scope = read_scope(args, root)?;
    let submitter_site = scope_filters(args, &scope)?;
    ensure_migrated(root)?;
    let db = open_projection(root)?;
    let surface_id = args.get("surface_id").and_then(Value::as_str);
    let kind = args.get("kind").and_then(Value::as_str);
    let requested_status = args.get("status").and_then(Value::as_str);
    let since = args.get("since").and_then(Value::as_str);
    let until = args.get("until").and_then(Value::as_str);
    let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(50).clamp(1, 200) as i64;
    let offset = args.get("offset").and_then(Value::as_u64).unwrap_or(0).min(10_000) as i64;
    let fetch_limit = limit + 1;
    let status = if actionable { Some("submitted") } else { requested_status };
    let status2 = if actionable { Some("acknowledged") } else { None };
    let owned = scope.owned_json();
    let mut stmt = db.prepare("SELECT feedback_id,surface_id,submitter_site_id,submitter_principal,kind,summary,details,status,resolution_note,resolved_by,task_ref,task_status,created_at,updated_at FROM feedback_entries WHERE (?1 IS NULL OR surface_id=?1) AND (?2 IS NULL OR submitter_site_id=?2) AND (?3 IS NULL OR kind=?3) AND (?4 IS NULL OR status=?4 OR status=?5) AND (?6 IS NULL OR created_at>=?6) AND (?7 IS NULL OR created_at<=?7) AND (?8 IS NULL OR surface_id IN (SELECT value FROM json_each(?8))) ORDER BY created_at DESC LIMIT ?9 OFFSET ?10").map_err(|e| error("feedback_query_prepare_failed", &e.to_string()))?;
    let rows = stmt.query_map(params![surface_id, submitter_site, kind, status, status2, since, until, owned, fetch_limit, offset], |row| Ok(json!({"feedback_id":row.get::<_,String>(0)?,"surface_id":row.get::<_,String>(1)?,"submitter_site_id":row.get::<_,String>(2)?,"submitter_principal":row.get::<_,String>(3)?,"kind":row.get::<_,String>(4)?,"summary":row.get::<_,String>(5)?,"details":row.get::<_,String>(6)?,"status":row.get::<_,String>(7)?,"resolution_note":row.get::<_,Option<String>>(8)?,"resolved_by":row.get::<_,Option<String>>(9)?,"task_ref":row.get::<_,Option<String>>(10)?,"task_status":row.get::<_,Option<String>>(11)?,"created_at":row.get::<_,String>(12)?,"updated_at":row.get::<_,String>(13)?}))).map_err(|e| error("feedback_query_failed", &e.to_string()))?;
    let mut entries = Vec::new(); for row in rows.take(201) { entries.push(row.map_err(|e| error("feedback_row_decode_failed", &e.to_string()))?); }
    let has_more = entries.len() > limit as usize;
    entries.truncate(limit as usize);
    let next_offset = has_more.then_some(offset + entries.len() as i64);
    Ok(json!({"schema":"narada.surface_feedback.list.v1","status":"ok","scope":scope.name,"count":entries.len(),"returned":entries.len(),"offset":offset,"limit":limit,"has_more":has_more,"next_offset":next_offset,"entries":entries,"read_only_native":true}))
}

fn feedback_show(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let scope = read_scope(args, root)?;
    let id = args.get("feedback_id").and_then(Value::as_str).filter(|v|!v.is_empty()).ok_or_else(||error("feedback_id_required","feedback_id_required"))?;
    ensure_migrated(root)?;
    let db = open_projection(root)?;
    let value: Option<Value> = db.query_row("SELECT feedback_id,surface_id,submitter_site_id,submitter_principal,kind,summary,details,status,resolution_note,resolved_by,task_ref,task_status,created_at,updated_at FROM feedback_entries WHERE feedback_id=?1", params![id], |row| Ok(json!({"feedback_id":row.get::<_,String>(0)?,"surface_id":row.get::<_,String>(1)?,"submitter_site_id":row.get::<_,String>(2)?,"submitter_principal":row.get::<_,String>(3)?,"kind":row.get::<_,String>(4)?,"summary":row.get::<_,String>(5)?,"details":row.get::<_,String>(6)?,"status":row.get::<_,String>(7)?,"resolution_note":row.get::<_,Option<String>>(8)?,"resolved_by":row.get::<_,Option<String>>(9)?,"task_ref":row.get::<_,Option<String>>(10)?,"task_status":row.get::<_,Option<String>>(11)?,"created_at":row.get::<_,String>(12)?,"updated_at":row.get::<_,String>(13)?}))).optional().map_err(|e|error("feedback_query_failed",&e.to_string()))?;
    let value = value.filter(|entry| match scope.name.as_str() {
        "authority_visible" | "authority_site_submissions" => scope.authority_site.as_deref().is_some_and(|site| entry["submitter_site_id"] == site),
        "owned_surfaces" => scope.owned_surfaces.as_ref().is_some_and(|owned| entry["surface_id"].as_str().is_some_and(|surface| owned.iter().any(|value| value == surface))),
        _ => true,
    });
    value.map(|entry|json!({"schema":"narada.surface_feedback.show.v1","status":"ok","scope":scope.name,"entry":entry,"read_only_native":true})).ok_or_else(||error("feedback_not_found","feedback_not_found"))
}

fn feedback_stats(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let scope = read_scope(args, root)?;
    let surface_id = args.get("surface_id").and_then(Value::as_str);
    let authority_site = scope.authority_site.as_deref();
    let owned = scope.owned_json();
    ensure_migrated(root)?;
    let db = open_projection(root)?;
    let total = db.query_row("SELECT COUNT(*) FROM feedback_entries WHERE (?1 IS NULL OR surface_id=?1) AND (?2 IS NULL OR submitter_site_id=?2) AND (?3 IS NULL OR surface_id IN (SELECT value FROM json_each(?3)))", params![surface_id,authority_site,owned], |row| row.get::<_,i64>(0)).map_err(|e|error("feedback_stats_query_failed",&e.to_string()))?;
    let mut by_surface = Vec::new(); let mut stmt = db.prepare("SELECT surface_id,COUNT(*) FROM feedback_entries WHERE (?1 IS NULL OR surface_id=?1) AND (?2 IS NULL OR submitter_site_id=?2) AND (?3 IS NULL OR surface_id IN (SELECT value FROM json_each(?3))) GROUP BY surface_id ORDER BY COUNT(*) DESC LIMIT 100").map_err(|e|error("feedback_stats_prepare_failed",&e.to_string()))?; let rows = stmt.query_map(params![surface_id,authority_site,owned], |row| Ok(json!({"surface_id":row.get::<_,String>(0)?,"count":row.get::<_,i64>(1)?}))).map_err(|e|error("feedback_stats_query_failed",&e.to_string()))?; for row in rows { by_surface.push(row.map_err(|e|error("feedback_stats_row_failed",&e.to_string()))?); }
    let mut by_status = Vec::new(); let mut stmt = db.prepare("SELECT status,COUNT(*) FROM feedback_entries WHERE (?1 IS NULL OR surface_id=?1) AND (?2 IS NULL OR submitter_site_id=?2) AND (?3 IS NULL OR surface_id IN (SELECT value FROM json_each(?3))) GROUP BY status ORDER BY COUNT(*) DESC LIMIT 20").map_err(|e|error("feedback_stats_prepare_failed",&e.to_string()))?; let rows = stmt.query_map(params![surface_id,authority_site,owned], |row| Ok(json!({"status":row.get::<_,String>(0)?,"count":row.get::<_,i64>(1)?}))).map_err(|e|error("feedback_stats_query_failed",&e.to_string()))?; for row in rows { by_status.push(row.map_err(|e|error("feedback_stats_row_failed",&e.to_string()))?); }
    Ok(json!({"schema":"narada.surface_feedback.stats.v1","status":"ok","scope":scope.name,"total":total,"by_surface":by_surface,"by_status":by_status,"read_only_native":true}))
}

// ---------------------------------------------------------------------------
// Mutations: fail-hard event appends under the authority lock.
// ---------------------------------------------------------------------------

fn feedback_submit(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let surface_id = required_arg(args, "surface_id", "feedback_requires_surface_id")?;
    let (submitter_site_id, submitter_principal, _) = authority()?;
    if let Some(asserted) = args.get("submitter_site_id").and_then(Value::as_str).map(str::trim).filter(|v| !v.is_empty()) {
        if asserted != submitter_site_id { return Err(error("feedback_submitter_site_authority_mismatch", "feedback_submitter_site_authority_mismatch")); }
    }
    if let Some(asserted) = args.get("submitter_principal").and_then(Value::as_str).map(str::trim).filter(|v| !v.is_empty()) {
        if asserted != submitter_principal { return Err(error("feedback_submitter_principal_authority_mismatch", "feedback_submitter_principal_authority_mismatch")); }
    }
    let kind = required_arg(args, "kind", "feedback_requires_kind")?;
    if !FEEDBACK_KINDS.contains(&kind.as_str()) { return Err(error("feedback_invalid_kind", "feedback_invalid_kind")); }
    let summary = required_arg(args, "summary", "feedback_requires_summary")?;
    let details = args.get("details").and_then(Value::as_str).unwrap_or("").to_string();
    let idempotency_key = args.get("idempotency_key").and_then(Value::as_str).map(str::trim).filter(|v|!v.is_empty()).map(ToOwned::to_owned);
    let id = idempotency_key.as_deref().map(|key| { let digest=Sha256::digest(format!("{submitter_site_id}\0{submitter_principal}\0{key}").as_bytes()); format!("sfb_{:x}",digest)[..16].to_string() }).unwrap_or_else(||format!("sfb_{}",&Uuid::new_v4().to_string()[..12]));
    ensure_migrated(root)?;
    with_authority_lock(root, || {
        if let Some(key) = idempotency_key.as_deref() {
            if let Some(existing) = event_ledger::find_event_by_idempotency(ERROR_SCHEMA, &ledger_layout(root), key)? {
                let entry = &existing["entry"];
                let fields = ["surface_id", "submitter_site_id", "submitter_principal", "kind", "summary", "details"];
                let request = [&surface_id, &submitter_site_id, &submitter_principal, &kind, &summary, &details];
                let identical = fields.iter().zip(request).all(|(field, expected)| entry[field].as_str().unwrap_or("") == *expected)
                    && entry["feedback_id"].as_str() == Some(id.as_str());
                if !identical { return Err(error("feedback_idempotency_conflict","feedback_idempotency_conflict")); }
                return Ok(json!({"schema":"narada.surface_feedback.submit.v1","status":"submitted","feedback_id":id,"surface_id":surface_id,"submitter_site_id":submitter_site_id,"kind":kind,"summary":summary,"created_at":entry["created_at"],"native_write":true,"idempotency_replay":true}));
            }
        }
        let now = now_iso();
        let site = bound_site_id();
        let principal = submitter_principal.clone();
        let entry = json!({"feedback_id":id,"surface_id":surface_id,"submitter_site_id":submitter_site_id,"submitter_principal":submitter_principal,"kind":kind,"summary":summary,"details":details,"status":"submitted","resolution_note":null,"resolved_by":null,"task_ref":null,"task_status":null,"source_db_path":null,"source_updated_at":null,"source_sync_mode":null,"created_at":now,"updated_at":now});
        let event_idempotency = idempotency_key.clone();
        event_ledger::append_event(ERROR_SCHEMA, &ledger_layout(root), HASH_FIELD, None, idempotency_key.as_deref(), |ctx| {
            json!({"schema":EVENT_SCHEMA,"sequence":ctx.sequence,"event_id":ctx.event_id,"previous_hash":ctx.previous_hash,"event_type":"submitted","site_id":site,"actor_principal":principal,"created_at":now,"idempotency_key":event_idempotency,"entry":entry})
        })?;
        rebuild_projection(root)?;
        Ok(json!({"schema":"narada.surface_feedback.submit.v1","status":"submitted","feedback_id":id,"surface_id":surface_id,"submitter_site_id":submitter_site_id,"kind":kind,"summary":summary,"created_at":now,"native_write":true,"idempotency_replay":false}))
    })
}

fn feedback_update_status(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let id = required_arg(args, "feedback_id", "feedback_requires_feedback_id")?;
    let status = required_arg(args, "status", "feedback_requires_status")?;
    if !FEEDBACK_STATUSES.contains(&status.as_str()) { return Err(error("feedback_invalid_status", "feedback_invalid_status")); }
    let note = required_arg(args, "resolution_note", "feedback_requires_resolution_note")?;
    let (authority_site, principal, owned_surfaces) = authority()?;
    ensure_migrated(root)?;
    with_authority_lock(root, || {
        let db = open_projection(root)?;
        let row: Option<(String, String, String)> = db.query_row("SELECT submitter_site_id,surface_id,status FROM feedback_entries WHERE feedback_id=?1", params![id], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?))).optional().map_err(|e| error("feedback_query_failed", &e.to_string()))?;
        drop(db);
        let Some((submitter_site, surface_id, previous_status)) = row else { return Err(error("feedback_not_found", "feedback_not_found")); };
        let owns_surface = owned_surfaces.iter().any(|value| value == &surface_id);
        if submitter_site != authority_site && !owns_surface && !is_canonical_store(root) { return Err(error("feedback_not_visible", "feedback_not_visible")); }
        let now = now_iso();
        let task_ref = args.get("task_ref").and_then(Value::as_str).map(ToOwned::to_owned);
        let task_status = args.get("task_status").and_then(Value::as_str).map(ToOwned::to_owned);
        let site = bound_site_id();
        let actor = principal.clone();
        let event_authority_site = authority_site.clone();
        event_ledger::append_event(ERROR_SCHEMA, &ledger_layout(root), HASH_FIELD, None, None, |ctx| {
            json!({"schema":EVENT_SCHEMA,"sequence":ctx.sequence,"event_id":ctx.event_id,"previous_hash":ctx.previous_hash,"event_type":"status_updated","site_id":site,"actor_principal":actor,"created_at":now,"feedback_id":id,"status":status,"resolution_note":note,"task_ref":task_ref,"task_status":task_status,"previous_status":previous_status,"authority_site_id":event_authority_site})
        })?;
        rebuild_projection(root)?;
        Ok(json!({"schema":"narada.surface_feedback.update_status.v1","status":"updated","feedback_id":id,"new_status":status,"resolved_by":principal,"resolution_note":note,"updated_at":now,"native_write":true}))
    })
}

fn feedback_update_status_batch(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let updates = args.get("updates").and_then(Value::as_array).ok_or_else(|| error("feedback_batch_requires_updates", "feedback_batch_requires_updates"))?;
    if updates.is_empty() || updates.len() > MAX_IMPORT_IDS {
        return Err(error("feedback_batch_invalid_size", "feedback_batch_invalid_size"));
    }
    let mut succeeded = Vec::new();
    let mut failed = Vec::new();
    for (index, update) in updates.iter().enumerate() {
        let object = update.as_object();
        let feedback_id = object.and_then(|value| value.get("feedback_id")).and_then(Value::as_str).map(str::trim).filter(|value| !value.is_empty()).map(ToOwned::to_owned);
        let result = object.map(|value| feedback_update_status(value, root)).unwrap_or_else(|| Err(error("feedback_update_must_be_object", "feedback_update_must_be_object")));
        match result {
            Ok(value) => succeeded.push(json!({
                "feedback_id": feedback_id.or_else(|| value.get("feedback_id").and_then(Value::as_str).map(ToOwned::to_owned)),
                "status": value.get("new_status").cloned().unwrap_or(Value::Null),
                "resolution_note": value.get("resolution_note").cloned().unwrap_or(Value::Null),
                "updated_at": value.get("updated_at").cloned().unwrap_or(Value::Null),
                "result": value,
            })),
            Err(diagnostic) => failed.push(json!({
                "index": index,
                "feedback_id": feedback_id,
                "code": diagnostic.get("code").cloned().unwrap_or_else(|| json!("feedback_update_failed")),
                "message": diagnostic.get("message").cloned().unwrap_or_else(|| json!("feedback_update_failed")),
                "details": diagnostic.get("details").cloned().unwrap_or_else(|| json!({})),
            })),
        }
    }
    let status = if failed.is_empty() { "updated" } else if succeeded.is_empty() { "failed" } else { "partial" };
    Ok(json!({
        "schema": "narada.surface_feedback.status_batch.v1",
        "status": status,
        "requested_count": updates.len(),
        "updated_count": succeeded.len(),
        "failed_count": failed.len(),
        "updates": succeeded,
        "failures": failed,
        "native_write": true,
    }))
}

fn configured_task_call(root: &Path, name: &str, arguments: Value) -> Result<Option<Value>, Value> {
    let options = LifecycleOptions { surface: LifecycleSurface::Task, site_root: task_authority_root(root), site_root_source: "surface-feedback-authority".to_string(), prepare: false, migrate_legacy: false, source_database_path: None };
    let mut authority = LifecycleServer::new(options).map_err(|detail| error("task_lifecycle_authority_unavailable", &detail))?;
    let response = authority.handle_request(json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":name,"arguments":arguments}})).ok_or_else(|| error("task_lifecycle_authority_no_response", "task_lifecycle_authority_no_response"))?;
    if let Some(authority_error) = response.get("error") { return Err(json!({"schema":"narada.authority_adapter.error.v1","status":"error","authority":"task-lifecycle","error":authority_error})); }
    let value = response.get("result").cloned().unwrap_or(response);
    if value.get("isError").and_then(Value::as_bool) == Some(true) {
        return Err(error("task_lifecycle_tool_refused", "task_lifecycle_tool_refused"));
    }
    Ok(Some(value.get("structuredContent").cloned().unwrap_or(value)))
}

fn task_authority_root(root: &Path) -> PathBuf {
    std::env::var("NARADA_TASK_LIFECYCLE_ROOT")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| root.to_path_buf())
}

fn feedback_convert_to_task(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let feedback_id = required_arg(args, "feedback_id", "feedback_requires_feedback_id")?;
    ensure_migrated(root)?;
    with_authority_lock(root, || {
        let db = open_projection(root)?;
        let row: Option<(String, String, String, String, String, Option<String>, Option<String>, Option<String>)> = db.query_row(
            "SELECT surface_id,submitter_site_id,summary,details,status,task_ref,task_status,resolution_note FROM feedback_entries WHERE feedback_id=?1",
            params![feedback_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?, row.get(6)?, row.get(7)?)),
        ).optional().map_err(|e| error("feedback_query_failed", &e.to_string()))?;
        drop(db);
        let Some((surface_id, submitter_site_id, summary, details, status, existing_task_ref, existing_task_status, existing_note)) = row else {
            return Err(error("feedback_not_found", "feedback_not_found"));
        };
        if let Some(task_ref) = existing_task_ref {
            if status != "converted_to_task" { return Err(error("feedback_task_link_conflict", "feedback_task_link_conflict")); }
            return Ok(json!({"schema":"narada.surface_feedback.convert_to_task.v1","status":"already_linked","feedback_id":feedback_id,"task_ref":task_ref,"task_status":existing_task_status,"resolution_note":existing_note,"handoff_authorization":{"scope":"canonical_user_site_handoff","authorization_basis":"canonical_feedback_store_and_server_binding"}}));
        }
        let payload = json!({
            "title": args.get("task_title").and_then(Value::as_str).filter(|value| !value.trim().is_empty()).unwrap_or_else(|| "Address feedback"),
            "goal": format!("Address feedback {feedback_id} for {surface_id}: {summary}"),
            "context": format!("Source feedback: {feedback_id}\nSurface: {surface_id}\nSubmitter site: {submitter_site_id}\nDetails: {details}"),
            "required_work": format!("Inspect feedback {feedback_id}; implement the smallest coherent fix; add focused tests; record verification evidence."),
            "non_goals": "Do not execute the task from surface-feedback; task execution remains owned by task-lifecycle and worker surfaces.",
            "acceptance_criteria": [format!("The concern described by feedback {feedback_id} is addressed or an exact blocker is recorded."), "Focused tests cover the changed behavior."],
            "idempotency_key": format!("surface-feedback:{feedback_id}"),
        });
        let payload_args = json!({"payload_id":format!("surface-feedback-{feedback_id}-task"),"payload":payload,"created_by":std::env::var("NARADA_AGENT_ID").ok().unwrap_or_else(|| "surface-feedback".to_string())});
        let created = (|| -> Result<Value, Value> {
            let Some(payload_result) = configured_task_call(root, "mcp_payload_create", payload_args)? else { return Err(authority_boundary("surface_feedback_convert_to_task")); };
            let payload_ref = payload_result.get("ref").or_else(|| payload_result.get("payload_ref")).and_then(Value::as_str).ok_or_else(|| error("feedback_task_payload_ref_missing", "feedback_task_payload_ref_missing"))?;
            let Some(task_result) = configured_task_call(root, "task_lifecycle_create", json!({"payload_ref":payload_ref}))? else { return Err(authority_boundary("surface_feedback_convert_to_task")); };
            let task_number = task_result.get("task_number").and_then(Value::as_i64);
            let task_id = task_result.get("task_id").and_then(Value::as_str);
            let task_ref = task_result.get("task_ref").and_then(Value::as_str).map(ToOwned::to_owned)
                .or_else(|| task_number.map(|value| format!("task #{value}")))
                .or_else(|| task_id.map(ToOwned::to_owned))
                .ok_or_else(|| error("feedback_task_create_result_invalid", "feedback_task_create_result_invalid"))?;
            let task_status = task_result.get("task_status").and_then(Value::as_str).or_else(|| task_result.get("status").and_then(Value::as_str)).unwrap_or("opened");
            Ok(json!({"task_ref":task_ref,"task_number":task_number,"task_id":task_id,"task_status":task_status,"payload_ref":payload_ref}))
        })();
        let task = match created {
            Ok(task) => task,
            Err(failure) => {
                // Crash-safe record of the failed handoff: fail-hard append, then report.
                let site = bound_site_id();
                let actor = bound_principal("surface-feedback");
                let detail = failure.get("message").and_then(Value::as_str).unwrap_or("task handoff failed").to_string();
                let code = failure.get("code").and_then(Value::as_str).unwrap_or("feedback_task_link_failed").to_string();
                let event_feedback_id = feedback_id.clone();
                event_ledger::append_event(ERROR_SCHEMA, &ledger_layout(root), HASH_FIELD, None, None, |ctx| {
                    json!({"schema":EVENT_SCHEMA,"sequence":ctx.sequence,"event_id":ctx.event_id,"previous_hash":ctx.previous_hash,"event_type":"task_link_failed","site_id":site,"actor_principal":actor,"created_at":now_iso(),"feedback_id":event_feedback_id,"error":detail,"error_code":code})
                })?;
                rebuild_projection(root)?;
                return Err(failure);
            }
        };
        let task_ref = task["task_ref"].as_str().unwrap_or_default().to_string();
        let task_status = task["task_status"].as_str().unwrap_or("opened").to_string();
        let task_number = task.get("task_number").cloned().unwrap_or(Value::Null);
        let task_id = task.get("task_id").cloned().unwrap_or(Value::Null);
        let payload_ref = task["payload_ref"].as_str().unwrap_or_default().to_string();
        let resolved_by = std::env::var("NARADA_AGENT_ID").ok().filter(|value| !value.trim().is_empty()).unwrap_or_else(|| "surface-feedback".to_string());
        let note = args.get("resolution_note").and_then(Value::as_str).filter(|value| !value.trim().is_empty()).map(ToOwned::to_owned).unwrap_or_else(|| format!("Created {task_ref} from feedback via surface_feedback_convert_to_task."));
        let now = now_iso();
        let site = bound_site_id();
        let actor = resolved_by.clone();
        let event_feedback_id = feedback_id.clone();
        let event_task_ref = task_ref.clone();
        let event_task_status = task_status.clone();
        let event_note = note.clone();
        event_ledger::append_event(ERROR_SCHEMA, &ledger_layout(root), HASH_FIELD, None, None, |ctx| {
            json!({"schema":EVENT_SCHEMA,"sequence":ctx.sequence,"event_id":ctx.event_id,"previous_hash":ctx.previous_hash,"event_type":"converted_to_task","site_id":site,"actor_principal":actor,"created_at":now,"feedback_id":event_feedback_id,"task_ref":event_task_ref,"task_number":task_number,"task_id":task_id,"task_status":event_task_status,"resolution_note":event_note,"payload_ref":payload_ref})
        })?;
        rebuild_projection(root)?;
        Ok(json!({"schema":"narada.surface_feedback.convert_to_task.v1","status":"converted","feedback_id":feedback_id,"task_ref":task_ref,"task_number":task["task_number"],"task_id":task["task_id"],"task_status":task_status,"task_creation":{"status":"created_or_recovered","payload_ref":task["payload_ref"],"idempotency_key":format!("surface-feedback:{feedback_id}")},"handoff_authorization":{"scope":"canonical_user_site_handoff","authorization_basis":"canonical_feedback_store_and_server_binding","authority_site_id":std::env::var("NARADA_SITE_ID").ok()},"next_action":{"surface_id":"task-lifecycle","tool":"task_lifecycle_show","arguments":{"task_number":task["task_number"]}}}))
    })
}

fn import_source_path(args: &Map<String, Value>, root: &Path) -> Result<PathBuf, Value> {
    let source_root = args.get("source_feedback_root").and_then(Value::as_str).filter(|value| !value.trim().is_empty());
    let source_db = args.get("source_db_path").and_then(Value::as_str).filter(|value| !value.trim().is_empty());
    if source_root.is_some() && source_db.is_some() { return Err(error("feedback_import_source_ambiguous", "feedback_import_source_ambiguous")); }
    let path = if let Some(value) = source_root {
        PathBuf::from(value).join(".feedback").join("surface-feedback.db")
    } else if let Some(value) = source_db {
        PathBuf::from(value)
    } else {
        return Err(error("feedback_import_requires_source", "feedback_import_requires_source"));
    };
    let path = if path.is_absolute() { path } else { root.join(path) };
    Ok(path)
}

