fn stable_json(value: &Value) -> String {
    match value {
        Value::Array(v) => {
            serde_json::to_string(&v.iter().map(sort_json).collect::<Vec<_>>()).unwrap()
        }
        Value::Object(_) => serde_json::to_string(&sort_json(value)).unwrap(),
        _ => serde_json::to_string(value).unwrap(),
    }
}
fn sort_json(value: &Value) -> Value {
    match value {
        Value::Array(v) => Value::Array(v.iter().map(sort_json).collect()),
        Value::Object(v) => {
            let mut keys = v.keys().collect::<Vec<_>>();
            keys.sort();
            let mut out = serde_json::Map::new();
            for key in keys {
                out.insert(key.clone(), sort_json(&v[key]));
            }
            Value::Object(out)
        }
        _ => value.clone(),
    }
}

fn guidance(args: &Value) -> Result<Value, String> {
    let mut result = crate::contract::guidance()?;
    let requested = json!({
        "workflow": args.get("workflow").and_then(Value::as_str).filter(|v|!v.trim().is_empty()).map(str::trim),
        "tool": args.get("tool").and_then(Value::as_str).filter(|v|!v.trim().is_empty()).map(str::trim)
    });
    let object = result
        .as_object_mut()
        .ok_or("agent_context_native_guidance_invalid")?;
    object.insert("requested".into(), requested);
    for key in ["path_resolution", "workflows", "tool_inventory", "examples", "anti_patterns", "recovery", "feedback", "tool_call_timeout"] {
        object.remove(key);
    }
    for key in ["first_use", "tool_preference", "boundaries"] {
        if let Some(Value::Array(items)) = object.get_mut(key) {
            items.truncate(3);
        }
    }
    object.insert("compact".into(), Value::Bool(true));
    Ok(result)
}

fn hydrate_current(context: &Context, args: &Value) -> Result<Value, String> {
    if args.get("checkpoint_startup") == Some(&Value::Bool(true)) {
        return Ok(
            json!({"schema":"narada.agent_context.orientation_hydration.v1","status":"blocked","reason":"orientation_assembly_read_only","required_next_step":"Use agent_context_checkpoint as a separate explicit mutation."}),
        );
    }
    let admission = match exact_evidence(
        args,
        "admission_receipt",
        "NARADA_CARRIER_SESSION_ADMISSION_RECEIPT",
    )? {
        Some(value) => value,
        None => {
            return Ok(
                json!({"schema":"narada.agent_context.orientation_hydration.v1","status":"blocked","reason":"agent_context_exact_admission_receipt_required","rejected_fallbacks":["latest_checkpoint","latest_start_event","identity_name_inference"]}),
            )
        }
    };
    let identity = admission
        .pointer("/agent_identity/local_agent_id")
        .and_then(Value::as_str)
        .ok_or("agent_context_admission_identity_mismatch")?;
    if let Ok(expected) = env::var("NARADA_AGENT_ID") {
        if expected != identity {
            return Err("agent_context_admission_identity_mismatch".into());
        }
    }
    let generated_at = match args.get("generated_at").and_then(Value::as_str) {
        Some(value) => chrono::DateTime::parse_from_rfc3339(value)
            .map_err(|_| "agent_context_invalid_generated_at")?
            .with_timezone(&Utc)
            .to_rfc3339_opts(SecondsFormat::Millis, true),
        None => timestamp(),
    };
    let roster = roster_projection(context, identity);
    let activation = exact_evidence(
        args,
        "activation_receipt",
        "NARADA_CARRIER_SESSION_ACTIVATION_RECEIPT",
    )?;
    let checkpoint_id = optional_string(args, "checkpoint_id")?;
    let (checkpoint, portable) = if let Some(id) = checkpoint_id.as_deref() {
        let selection = json!({"agent_id":identity,"checkpoint_id":id});
        (
            Some(rehydrate(context, &selection)?),
            Some(continuation_read(context, &selection)?),
        )
    } else {
        (None, None)
    };
    let compiled = crate::materialization::compile(
        context,
        &admission,
        activation.as_ref(),
        &roster["role_binding"],
        &generated_at,
        checkpoint.as_ref(),
        portable.as_ref(),
    )?;
    let whoami = json!({"schema":"narada.agent_context.identity_resolution.v1","status":"ok","identity":identity,"canonical_agent_id":admission.pointer("/agent_identity/canonical_agent_id"),"confidence":"exact","source":"carrier_session_admission_receipt","admission_receipt_ref":admission["receipt_id"],"carrier_session":admission["coordinate"],"authority_readback_ref":admission["authority_readback_ref"],"hint_match":true});
    let omitted =
        json!({"status":"omitted","reason":"exact_checkpoint_not_selected","checkpoint_id":null});
    let checkpoint_result = checkpoint.unwrap_or_else(|| omitted.clone());
    let portable_result = portable.unwrap_or_else(|| omitted.clone());
    let advisory = checkpoint_result
        .get("next_intended_action")
        .cloned()
        .unwrap_or(Value::Null);
    Ok(
        json!({"schema":"narada.agent_context.orientation_hydration.v1","status":if compiled.manifest["delivery"]=="deliverable"{"ok"}else{"blocked"},"source_mutation":false,"site_id":context.site_id,"site_root":path_text(&context.site_root),"hydrated_at":compiled.manifest["generated_at"],"whoami":whoami,"admission_receipt_ref":admission["receipt_id"],"orientation_manifest":compiled.manifest,"continuity_selection":if let Some(id)=checkpoint_id{json!({"mode":"exact","checkpoint_id":id})}else{json!({"mode":"omitted","checkpoint_id":null})},"checkpoint":checkpoint_result,"portable_continuation":portable_result,"continuity_advisory_next_action":advisory}),
    )
}

fn doctor(context: &Context) -> Result<Value, String> {
    let db = context.open_db()?;
    let tables = [
        "agent_start_events",
        "agent_events",
        "agent_checkpoints",
        "agent_checkpoint_history",
        "orientation_manifest_generations",
        "identity_state_records",
    ]
    .iter()
    .map(|name| {
        let exists = db
            .query_row(
                "SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1",
                [name],
                |_| Ok(true),
            )
            .optional()
            .unwrap_or(None)
            .unwrap_or(false);
        json!({"table":name,"exists":exists})
    })
    .collect::<Vec<_>>();
    let ok = tables
        .iter()
        .all(|v| v.get("exists") == Some(&Value::Bool(true)));
    Ok(
        json!({"status":if ok {"ok"} else {"degraded"},"site_id":context.site_id,"server_name":context.server_name,"site_root":path_text(&context.site_root),"db_path":path_text(&context.db_path),"tables":tables}),
    )
}

fn checkpoint(context: &Context, args: &Value) -> Result<Value, String> {
    let agent_id = args
        .get("agent_id")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| env::var("NARADA_AGENT_ID").ok())
        .ok_or("agent_id_required")?;
    validate_identity(context, &agent_id)?;
    let mut db = context.open_db()?;
    let now = timestamp();
    let checkpoint_id = id("chk");
    let existing = db
        .query_row(
            "SELECT * FROM agent_checkpoints WHERE agent_id=?1 ORDER BY checkpoint_at DESC LIMIT 1",
            [&agent_id],
            row_to_checkpoint,
        )
        .optional()
        .map_err(db_error)?;
    let continuation = normalize_continuation(args.get("continuation"), &checkpoint_id, &now)?;
    let continuation_ref = args
        .get("continuation_ref")
        .cloned()
        .filter(|v| !v.is_null());
    let projection = continuation
        .as_ref()
        .map(|_| continuation_projection(&agent_id, continuation_ref.as_ref(), existing.as_ref()));
    let claimed_identity = args.get("claimed_identity")
        .and_then(|value| value.as_str().map(str::to_string).or_else(||value.get("identity").and_then(Value::as_str).map(str::to_string)))
        .unwrap_or_else(||agent_id.clone());
    let identity_state = json!({
        "schema":"narada.agent.identity_state.v1",
        "claimed_identity":{"identity":claimed_identity,"status":"claimed","source":"caller_assertion","asserted_at":now,"evidence_refs":[],"authority_granted":false},
        "authentication":{"status":"missing","authenticated_identity":null,"evidence_refs":[]},
        "authority":{"status":"not_evaluated","operation":null,"granted":false,"evidence_refs":[]}
    });
    let payload = json!({
        "schema":"narada.agent_context.checkpoint.v1","site_id":context.site_id,"site_root":path_text(&context.site_root),"agent_id":agent_id,"identity_state":identity_state,"checkpoint_at":now,
        "active_task":field_or_null(args,"active_task"),"files_touched":array(args,"files_touched"),"key_decisions":array(args,"key_decisions"),"open_questions":array(args,"open_questions"),
        "git_head":field_or_null(args,"git_head"),"last_workboard_check_at":field_or_null(args,"last_workboard_check_at"),"next_intended_action":field_or_null(args,"next_intended_action"),
        "authority_basis":field_or_null(args,"authority_basis"),"continuation_blockers":array(args,"continuation_blockers"),"evidence_refs":array(args,"evidence_refs"),
        "worktree_state":field_or_null(args,"worktree_state"),"tactical_resume_notes":array(args,"tactical_resume_notes"),"continuation":continuation,"continuation_ref":continuation_ref,"continuation_projection":projection
    });
    let transaction = db.transaction().map_err(db_error)?;
    if let Some(previous) = &existing {
        transaction.execute("INSERT INTO agent_checkpoint_history (history_id,checkpoint_id,agent_id,session_id,checkpoint_at,active_task_json,files_touched_json,key_decisions_json,open_questions_json,git_head,payload_json,archived_at) SELECT ?1,checkpoint_id,agent_id,session_id,checkpoint_at,active_task_json,files_touched_json,key_decisions_json,open_questions_json,git_head,payload_json,?2 FROM agent_checkpoints WHERE checkpoint_id=?3", params![id("hist"),now,previous["checkpoint_id"].as_str()]).map_err(db_error)?;
        transaction
            .execute(
                "DELETE FROM agent_checkpoints WHERE checkpoint_id=?1",
                [previous["checkpoint_id"].as_str()],
            )
            .map_err(db_error)?;
    }
    let session_id = args
        .get("session_id")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| env::var("NARADA_AGENT_START_EVENT_ID").ok());
    transaction.execute("INSERT INTO agent_checkpoints (checkpoint_id,agent_id,session_id,checkpoint_at,active_task_json,files_touched_json,key_decisions_json,open_questions_json,git_head,payload_json) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)", params![checkpoint_id,agent_id,session_id,now,json_db(args.get("active_task")),json_text(array(args,"files_touched")),json_text(array(args,"key_decisions")),json_text(array(args,"open_questions")),args.get("git_head").and_then(Value::as_str),json_text(payload.clone())]).map_err(db_error)?;
    transaction.execute("INSERT INTO identity_state_records (record_id,event_id,session_id,claimed_identity_json,authentication_json,authority_json,recorded_at) VALUES (?1,?2,?3,?4,?5,?6,?7)", params![format!("identity:{}",checkpoint_id),checkpoint_id,session_id,serde_json::to_string(&payload["identity_state"]["claimed_identity"]).map_err(|e|e.to_string())?,serde_json::to_string(&payload["identity_state"]["authentication"]).map_err(|e|e.to_string())?,serde_json::to_string(&payload["identity_state"]["authority"]).map_err(|e|e.to_string())?,now]).map_err(db_error)?;
    transaction.commit().map_err(db_error)?;
    Ok(
        json!({"status":"checkpointed","checkpoint_id":checkpoint_id,"archived_prior":existing.as_ref().and_then(|v|v["checkpoint_id"].as_str()),"agent_id":agent_id,"checkpoint_at":now,"db_path":path_text(&context.db_path),"site_root":path_text(&context.site_root),"continuation":payload["continuation"],"continuation_ref":payload["continuation_ref"],"continuation_projection":payload["continuation_projection"]}),
    )
}

fn rehydrate(context: &Context, args: &Value) -> Result<Value, String> {
    let agent_id = required_string(args, "agent_id")?;
    validate_identity(context, &agent_id)?;
    let db = context.open_db()?;
    let checkpoint_id = optional_string(args, "checkpoint_id")?;
    let limit = args
        .get("limit")
        .and_then(Value::as_i64)
        .unwrap_or(1)
        .clamp(1, 50);
    if let Some(ref requested) = checkpoint_id {
        let found = checkpoint_by_id(&db, &agent_id, requested)?;
        return Ok(found.map(|v| merge(json!({"status":"ok"}),v)).unwrap_or_else(||json!({"status":"checkpoint_not_found","agent_id":agent_id,"checkpoint_id":requested,"message":"No site-local current or archived checkpoint found for the requested checkpoint_id."})));
    }
    if args.get("history") == Some(&Value::Bool(true)) || limit > 1 {
        let mut stmt=db.prepare("SELECT checkpoint_id,agent_id,session_id,checkpoint_at,active_task_json,files_touched_json,key_decisions_json,open_questions_json,git_head,payload_json FROM agent_checkpoint_history WHERE agent_id=?1 ORDER BY archived_at DESC LIMIT ?2").map_err(db_error)?;
        let rows = stmt
            .query_map(params![agent_id, limit], row_to_checkpoint)
            .map_err(db_error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(db_error)?;
        return Ok(
            json!({"status":if rows.is_empty(){"no_checkpoint_history"}else{"ok"},"agent_id":agent_id,"count":rows.len(),"checkpoints":rows}),
        );
    }
    let row = db
        .query_row(
            "SELECT * FROM agent_checkpoints WHERE agent_id=?1 ORDER BY checkpoint_at DESC LIMIT 1",
            [&agent_id],
            row_to_checkpoint,
        )
        .optional()
        .map_err(db_error)?;
    Ok(row.map(|v|merge(json!({"status":"ok"}),v)).unwrap_or_else(||json!({"status":"no_checkpoint","agent_id":agent_id,"message":"No site-local checkpoint found."})))
}

