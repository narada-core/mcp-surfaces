fn same_feedback_path(left: &Path, right: &Path) -> bool {
    left.canonicalize().unwrap_or_else(|_| left.to_path_buf()) == right.canonicalize().unwrap_or_else(|_| right.to_path_buf())
}

fn feedback_import(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let source_path = import_source_path(args, root)?;
    if same_feedback_path(&source_path, &legacy_db_path(root)) {
        return Err(error("feedback_import_same_store", "feedback_import_same_store"));
    }
    if !source_path.exists() { return Err(error("feedback_import_source_missing", "feedback_import_source_missing")); }
    let ids = args.get("feedback_ids").and_then(Value::as_array).ok_or_else(|| error("feedback_import_requires_feedback_ids", "feedback_import_requires_feedback_ids"))?;
    let ids = ids.iter().filter_map(Value::as_str).map(str::trim).filter(|value| !value.is_empty()).map(ToOwned::to_owned).collect::<Vec<_>>();
    if ids.is_empty() || ids.len() > MAX_IMPORT_IDS { return Err(error("feedback_import_requires_feedback_ids", "feedback_import_requires_feedback_ids")); }
    let source = Connection::open_with_flags(&source_path, OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(|e| error("feedback_import_source_open_failed", &e.to_string()))?;
    ensure_migrated(root)?;
    with_authority_lock(root, || {
        let target = open_projection(root)?;
        let mut imported = Vec::new();
        let mut skipped = Vec::new();
        let mut missing = Vec::new();
        for id in &ids {
            let Some(row) = feedback_row(&source, id)? else { missing.push(id.clone()); continue; };
            if feedback_row(&target, id)?.is_some() {
                skipped.push(json!({"feedback_id": id, "reason": "already_exists"}));
                continue;
            }
            let get = |name: &str| row.get(name).cloned().unwrap_or(Value::Null);
            let text = |name: &str| get(name).as_str().unwrap_or("").to_string();
            let status = { let value = text("status"); if value.is_empty() { "submitted".to_string() } else { value } };
            let created_at = text("created_at");
            let updated_at = text("updated_at");
            let entry = json!({
                "feedback_id": text("feedback_id"), "surface_id": text("surface_id"), "submitter_site_id": text("submitter_site_id"),
                "submitter_principal": text("submitter_principal"), "kind": text("kind"), "summary": text("summary"), "details": text("details"),
                "status": status, "resolution_note": optional_text(&get("resolution_note")), "resolved_by": optional_text(&get("resolved_by")),
                "task_ref": optional_text(&get("task_ref")), "task_status": optional_text(&get("task_status")),
                "source_db_path": source_path.to_string_lossy(), "source_updated_at": updated_at, "source_sync_mode": "explicit_import",
                "created_at": created_at, "updated_at": updated_at,
            });
            let now = now_iso();
            let site = bound_site_id();
            let actor = bound_principal("surface-feedback-import");
            event_ledger::append_event(ERROR_SCHEMA, &ledger_layout(root), HASH_FIELD, None, None, |ctx| {
                json!({"schema":EVENT_SCHEMA,"sequence":ctx.sequence,"event_id":ctx.event_id,"previous_hash":ctx.previous_hash,"event_type":"imported","site_id":site,"actor_principal":actor,"created_at":now,"entry":entry})
            })?;
            imported.push(json!({"feedback_id":id,"surface_id":text("surface_id"),"submitter_site_id":text("submitter_site_id"),"submitter_principal":text("submitter_principal"),"kind":text("kind"),"summary":text("summary"),"details":text("details"),"status":status,"source_db_path":source_path.to_string_lossy().to_string(),"source_sync_mode":"explicit_import","created_at":created_at,"updated_at":updated_at}));
        }
        drop(target);
        rebuild_projection(root)?;
        Ok(json!({"schema":"narada.surface_feedback.import.v1","status":if missing.is_empty() && skipped.is_empty(){"imported"}else{"partial"},"source_db_path":source_path.to_string_lossy(),"target_db_path":projection_path(root).to_string_lossy(),"target_ledger_path":ledger_dir(root).to_string_lossy(),"requested_count":ids.len(),"imported_count":imported.len(),"skipped_count":skipped.len(),"missing_count":missing.len(),"imported":imported,"skipped":skipped,"missing_feedback_ids":missing,"native_write":true}))
    })
}

fn optional_text(value: &Value) -> Option<String> { value.as_str().map(str::to_string).filter(|value| !value.is_empty()) }

// ---------------------------------------------------------------------------
// Doctor and capabilities.
// ---------------------------------------------------------------------------

fn doctor(root: &Path) -> Result<Value, Value> {
    let legacy_path = legacy_db_path(root);
    let marker_path = migration_marker_path(root);
    let legacy_present = legacy_path.exists();
    let marker_present = marker_path.exists();
    let had_store = legacy_present || ledger_dir(root).exists();
    ensure_migrated(root)?;
    let event_count = ledger_files(root)?.len();
    let rows_migrated = if marker_present {
        ledger_io::read_json(ERROR_SCHEMA, &marker_path).ok().and_then(|marker| marker["rows_migrated"].as_u64()).unwrap_or(0)
    } else { 0 };
    let mut feedback_entries = 0_i64;
    if had_store || event_count > 0 {
        let db = open_projection(root)?;
        feedback_entries = db.query_row("SELECT COUNT(*) FROM feedback_entries", [], |row| row.get::<_, i64>(0)).unwrap_or(0);
    }
    let ready = event_count > 0 || marker_present;
    let migration = json!({
        "legacy_present": legacy_present,
        "legacy_db_path": legacy_path.to_string_lossy(),
        "marker_present": marker_present,
        "marker_path": marker_path.to_string_lossy(),
        "rows_migrated": rows_migrated,
        "legacy_db_writable": false,
    });
    Ok(json!({"schema":"narada.surface_feedback.doctor.v1","status":"ok","feedback_root":root.to_string_lossy(),"db_path":legacy_path.to_string_lossy(),"ledger_path":ledger_dir(root).to_string_lossy(),"projection_path":projection_path(root).to_string_lossy(),"store_status":if ready{"ready"}else{"missing"},"feedback_entries":feedback_entries,"ledger_events":event_count,"read_only_native":false,"native_write_available":true,"migration":migration,"capabilities":capabilities(root),"server_name":SERVER_NAME}))
}

fn capabilities(root: &Path) -> Value {
    let bound_authority = authority().ok();
    let authority_configured = bound_authority.is_some();
    let owned_empty = bound_authority.as_ref().map(|(_, _, owned)| owned.is_empty()).unwrap_or(true);
    let canonical = is_canonical_store(root);
    let task_handoff_configured = task_authority_root(root).join(".ai/task-lifecycle.db").is_file();
    let canonical_scope = |purpose: &str| json!({"available":canonical,"purpose":purpose,"reason":if canonical{Value::Null}else{json!("feedback_global_read_requires_canonical_store")}});
    let authority_scope = |purpose: &str| json!({"available":authority_configured,"purpose":purpose,"reason":if authority_configured{Value::Null}else{json!("feedback_authority_unconfigured")}});
    json!({"read_scopes":{
        "all_authorized":canonical_scope("canonical local feedback store"),
        "store_reconciliation":canonical_scope("explicit source/store reconciliation"),
        "authority_visible":authority_scope("feedback submitted by the server-bound authority Site"),
        "owned_surfaces":{"available":authority_configured && !owned_empty,"purpose":"feedback about surfaces owned by the server-bound authority","reason":if authority_configured && !owned_empty{Value::Null}else{json!("feedback_owned_surfaces_unbound")}},
        "authority_site_submissions":authority_scope("feedback submitted by the server-bound authority Site"),
    },"mutations":{
        "submit":{"available":true,"authority_site_id":bound_authority.as_ref().map(|(site,_,_)|site)},
        "import":{"available":true},
        "status_update":{"available":authority_configured,"reason":if authority_configured{Value::Null}else{json!("feedback_authority_unconfigured")}},
        "task_handoff":{"available":authority_configured && task_handoff_configured,"reason":if authority_configured && task_handoff_configured{Value::Null}else{json!("task_or_site_authority_unconfigured")}},
    }})
}

fn proof_template(args: &Map<String, Value>) -> Value {
    let optional = |key: &str| args.get(key).and_then(Value::as_str).map(str::trim).filter(|value| !value.is_empty()).map(ToOwned::to_owned).map(Value::String).unwrap_or(Value::Null);
    json!({
        "schema": "narada.surface_feedback.live_proof_template.v1",
        "status": "ok",
        "workflow": optional("workflow"),
        "surface_id": optional("surface_id"),
        "purpose": "Capture evidence expectations for live, no-mock, no-fallback E2E authority/projection behavior.",
        "recommended_feedback": {
            "kind": "observation",
            "details_format": "json_or_markdown_with_live_proof_contract"
        },
        "live_proof_contract": {
            "authority_location": {
                "deployed": "<where the deployed authority or projection state lives>",
                "local": "<where local source/test authority lives>"
            },
            "transport": {
                "live_transport_assumption": "<named live transport path and why it is expected>",
                "replay_vs_live_delivery": "<how replay evidence is distinguished from live delivery>"
            },
            "success": {
                "semantic_success_point": "<observable state/event that proves live success>",
                "saved_evidence_file": "<required artifact path or null when not applicable>"
            },
            "exclusions": {
                "no_mock": "<evidence that mocks were not used>",
                "no_fallback": "<evidence that fallback path was not used>",
                "no_shim": "<evidence that compatibility shim did not carry the behavior>"
            },
            "negative_controls": {
                "revocation_or_refusal_proof": "<how revoked/unauthorized paths fail>"
            },
            "test_alignment": {
                "unit_tests_specify_deployed_transport": "<yes/no/unknown plus file references>"
            }
        },
        "usage": [
            "Use this template in feedback details when reporting live-proof gaps or observations.",
            "Use it in task context when converting feedback into implementation work.",
            "Do not treat a completed template as proof by itself; proof requires cited artifacts and live readback."
        ]
    })
}
fn authority_boundary(name: &str) -> Value { json!({"schema":"narada.surface_feedback.authority_boundary.v1","status":"unavailable","tool_name":name,"reason":"surface_feedback_mutation_not_enabled_in_native_read_slice","remediation":"Use the configured surface-feedback authority for writes, imports, task handoffs, and status changes."}) }
fn error(code: &str, message: &str) -> Value { json!({"schema":"narada.surface_feedback.error.v1","code":code,"message":message}) }
fn tool(name: &str, description: &str, input_schema: Value, read_only: bool) -> Value { json!({"name":name,"description":description,"annotations":{"title":name,"readOnlyHint":read_only,"destructiveHint":!read_only,"idempotentHint":read_only,"openWorldHint":false},"inputSchema":input_schema,"outputSchema":{"type":"object","additionalProperties":true}}) }

