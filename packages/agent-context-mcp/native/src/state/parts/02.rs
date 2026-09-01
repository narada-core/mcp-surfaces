fn persist_session_materialization(
    context: &Context,
    identity: &str,
    runtime: &str,
    cwd: &str,
    roster: &Value,
    admission: &Value,
    compiled: crate::materialization::Materialization,
    claimed_identity_input: Option<&Value>,
) -> Result<Value, String> {
    let manifest = compiled.manifest;
    let brief = compiled.brief;
    let now = manifest["generated_at"]
        .as_str()
        .ok_or("agent_context_native_manifest_generated_at_missing")?;
    let event_id = format!(
        "evt-{}_{}",
        now.replace([':', '.'], "-")
            .replace('T', "_")
            .chars()
            .take(19)
            .collect::<String>(),
        &Uuid::new_v4().to_string()[..8]
    );
    let event_status = if manifest["delivery"] == "deliverable" {
        "materialized"
    } else {
        "orientation_blocked"
    };
    let explicit_claim = claimed_identity_input.and_then(|value| {
        value.as_str().map(str::to_string).or_else(|| value.get("identity").and_then(Value::as_str).map(str::to_string))
    });
    let claimed_identity = explicit_claim.clone()
        .or_else(|| env::var("NARADA_CLAIMED_IDENTITY").ok())
        .or_else(|| env::var("NARADA_AGENT_ID").ok())
        .unwrap_or_else(|| identity.to_string());
    let claim_source = if explicit_claim.is_some() { "caller_assertion" } else if env::var_os("NARADA_CLAIMED_IDENTITY").is_some() || env::var_os("NARADA_AGENT_ID").is_some() { "carrier_environment" } else { "carrier_session_admission_receipt" };
    let authenticated_identity = admission.pointer("/agent_identity/local_agent_id").and_then(Value::as_str).unwrap_or_default();
    if !authenticated_identity.is_empty() && claimed_identity != authenticated_identity {
        return Err("agent_context_claimed_identity_mismatch".into());
    }
    let identity_state = json!({
        "schema":"narada.agent.identity_state.v1",
        "claimed_identity":{"identity":claimed_identity,"status":"claimed","source":claim_source,"asserted_at":now,"evidence_refs":[],"authority_granted":false},
        "authentication":{"status":"authenticated","authenticated_identity":authenticated_identity,"evidence_refs":[admission["receipt_id"].clone(),admission["authority_readback_ref"].clone()]}, 
        "authority":{"status":"not_evaluated","operation":null,"granted":false,"evidence_refs":[]}
    });
    let manifest_json = serde_json::to_string(&manifest).map_err(|e| e.to_string())?;
    let brief_json = brief
        .as_ref()
        .map(serde_json::to_string)
        .transpose()
        .map_err(|e| e.to_string())?;
    let mut db = context.open_db()?;
    let tx = db.transaction().map_err(db_error)?;
    let existing: Option<String> = tx
        .query_row(
            "SELECT manifest_json FROM orientation_manifest_generations WHERE manifest_id=?1",
            [manifest["manifest_id"].as_str()],
            |r| r.get(0),
        )
        .optional()
        .map_err(db_error)?;
    if existing.as_deref().is_some_and(|v| v != manifest_json) {
        return Err("agent_context_orientation_manifest_generation_conflict".into());
    }
    if existing.is_none() {
        tx.execute("INSERT INTO orientation_manifest_generations (manifest_id,admission_receipt_ref,carrier_session_id,authority_epoch,readiness,delivery,manifest_json,generated_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",params![manifest["manifest_id"].as_str(),admission["receipt_id"].as_str(),admission.pointer("/coordinate/carrier_session_id").and_then(Value::as_str),admission.pointer("/coordinate/authority_epoch").and_then(Value::as_i64),manifest["readiness"].as_str(),manifest["delivery"].as_str(),manifest_json,now]).map_err(db_error)?;
    }
    if let (Some(value), Some(text)) = (&brief, &brief_json) {
        let existing: Option<String> = tx
            .query_row(
                "SELECT brief_json FROM orientation_brief_generations WHERE brief_id=?1",
                [value["brief_id"].as_str()],
                |r| r.get(0),
            )
            .optional()
            .map_err(db_error)?;
        if existing.as_deref().is_some_and(|v| v != text) {
            return Err("agent_context_orientation_brief_generation_conflict".into());
        }
        if existing.is_none() {
            tx.execute("INSERT INTO orientation_brief_generations (brief_id,manifest_id,brief_digest,brief_json,generated_at) VALUES (?1,?2,?3,?4,?5)",params![value["brief_id"].as_str(),manifest["manifest_id"].as_str(),value["brief_digest"].as_str(),text,value["generated_at"].as_str()]).map_err(db_error)?;
        }
    }
    tx.execute("INSERT INTO agent_start_events (event_id,identity_id,runtime,created_at,status,resume_command,bootstrap_artifact_uri,carrier_session_id,admission_receipt_ref,authority_epoch,orientation_manifest_id,claimed_identity_json,authentication_json,authority_json) VALUES (?1,?2,?3,?4,?5,NULL,NULL,?6,?7,?8,?9,?10,?11,?12)",params![event_id,identity,runtime,now,event_status,admission.pointer("/coordinate/carrier_session_id").and_then(Value::as_str),admission["receipt_id"].as_str(),admission.pointer("/coordinate/authority_epoch").and_then(Value::as_i64),manifest["manifest_id"].as_str(),serde_json::to_string(&identity_state["claimed_identity"]).map_err(|e|e.to_string())?.as_str(),serde_json::to_string(&identity_state["authentication"]).map_err(|e|e.to_string())?.as_str(),serde_json::to_string(&identity_state["authority"]).map_err(|e|e.to_string())?.as_str()]).map_err(db_error)?;
    tx.execute("INSERT INTO identity_state_records (record_id,event_id,session_id,claimed_identity_json,authentication_json,authority_json,recorded_at) VALUES (?1,?2,?3,?4,?5,?6,?7)",params![format!("identity:{}",event_id),event_id,admission.pointer("/coordinate/carrier_session_id").and_then(Value::as_str),serde_json::to_string(&identity_state["claimed_identity"]).map_err(|e|e.to_string())?,serde_json::to_string(&identity_state["authentication"]).map_err(|e|e.to_string())?,serde_json::to_string(&identity_state["authority"]).map_err(|e|e.to_string())?,now]).map_err(db_error)?;
    tx.commit().map_err(db_error)?;
    let persisted = if brief.is_some() {
        json!([
            "orientation_manifest_generations",
            "orientation_brief_generations",
            "identity_state_records",
            "agent_start_events"
        ])
    } else {
        json!(["orientation_manifest_generations", "identity_state_records", "agent_start_events"])
    };
    let manifest_ref=brief.as_ref().map(|v|v["manifest_ref"].clone()).unwrap_or_else(||json!({"source_authority_ref":"agent-context:orientation-manifest-store","artifact_ref":format!("agent-context:orientation_manifest_generations:{}",manifest["manifest_id"].as_str().unwrap_or("")),"revision":manifest["manifest_digest"],"manifest_id":manifest["manifest_id"],"manifest_digest":manifest["manifest_digest"]}));
    let entry_procedure = manifest["entries"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|v| v["compartment"] == "entry_procedure")
        .cloned()
        .collect::<Vec<_>>();
    Ok(
        json!({"schema":"narada.agent_context.session_start.v1","status":if manifest["delivery"]=="deliverable"{"materialized"}else{"blocked"},"compatibility_facade":{"authority":"none","event_posture":"downstream_trace","source_authority_mutation":false,"local_persistence":true,"persisted_records":persisted},"agent_start_event":event_id,"identity":identity,"claimed_identity":identity_state["claimed_identity"],"authenticated_identity":identity_state["authentication"]["authenticated_identity"],"authentication":identity_state["authentication"],"authority":identity_state["authority"],"identity_state":identity_state,"role":roster["role"],"role_binding":roster["role_binding"],"runtime_request":runtime,"cwd_request":cwd,"db_path":path_text(&context.db_path),"carrier_session":admission["coordinate"],"admission_receipt":admission,"admission_receipt_ref":admission["receipt_id"],"orientation_manifest":manifest,"orientation_brief":brief,"orientation_manifest_ref":manifest_ref,"entry_procedure":entry_procedure}),
    )
}
fn roster_projection(context: &Context, identity: &str) -> Value {
    let path = context.site_root.join(".ai/agents/roster.json");
    if let Ok(bytes) = fs::read(path) {
        if let Ok(roster) = serde_json::from_slice::<Value>(&bytes) {
            if let Some(agent) = roster
                .get("agents")
                .and_then(Value::as_array)
                .and_then(|items| {
                    items
                        .iter()
                        .find(|item| item.get("agent_id").and_then(Value::as_str) == Some(identity))
                })
            {
                let role = agent.get("role").cloned().unwrap_or(Value::Null);
                return json!({"role":role,"role_binding":role_binding(identity,&role,"static_roster_config","agent_roster")});
            }
            if roster.get("enforce_session_roster") == Some(&Value::Bool(true)) {
                return json!({"role":null,"role_binding":role_binding(identity,&Value::Null,"unavailable","unavailable")});
            }
        }
    }
    let suffix = identity
        .rsplit('.')
        .next()
        .filter(|v| matches!(*v, "architect" | "builder" | "builder2" | "resident"));
    let role = suffix.map(Value::from).unwrap_or(Value::Null);
    json!({"role":role,"role_binding":role_binding(identity,&role,"identity_inference_non_authoritative","identity_inference_non_authoritative")})
}
fn role_binding(agent: &str, role: &Value, source: &str, authority: &str) -> Value {
    let semantics=match authority{"agent_roster"=>"Roster role binding is used for identity read models, routing, and eligibility; it is not activation authority or a capability grant.","identity_inference_non_authoritative"=>"Role was inferred from identity shape because the Site has not opted into session roster enforcement; this is a read-model hint, not activation authority or a capability grant.",_=>"No authoritative role binding was available. This residual projection cannot create identity, block an owner-issued admission, or grant capability."};
    json!({"schema":"narada.agent.role_binding.v0","agent_id":agent,"role_name":role,"binding_source":source,"binding_authority":authority,"semantics":semantics,"capability_policy_ref":"capability_policy"})
}

fn list_sessions(context: &Context, args: &Value) -> Result<Value, String> {
    let db = context.open_db()?;
    let identity = args.get("identity").and_then(Value::as_str);
    let substrate = args.get("substrate").and_then(Value::as_str);
    let date_from = parse_date_filter(args.get("date_from"), "date_from")?;
    let date_to = parse_date_filter(args.get("date_to"), "date_to")?;
    let limit = args
        .get("limit")
        .and_then(Value::as_i64)
        .unwrap_or(100)
        .clamp(1, 500) as usize;
    let offset = args
        .get("offset")
        .and_then(Value::as_i64)
        .unwrap_or(0)
        .clamp(0, 10_000) as usize;
    let mut stmt=db.prepare("SELECT event_id,identity_id,runtime,created_at,status,resume_command,bootstrap_artifact_uri,claimed_identity_json,authentication_json,authority_json FROM agent_start_events ORDER BY created_at DESC,event_id DESC").map_err(db_error)?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, Option<String>>(6)?,
                row.get::<_, Option<String>>(7)?,
                row.get::<_, Option<String>>(8)?,
                row.get::<_, Option<String>>(9)?,
            ))
        })
        .map_err(db_error)?;
    let now = Utc::now();
    let generated = now.to_rfc3339_opts(SecondsFormat::Millis, true);
    let mut sessions = Vec::new();
    let mut matched = 0usize;
    let mut total_matched = 0usize;
    let mut scanned = 0usize;
    for row in rows {
        scanned += 1;
        if scanned > 10_000 {
            return Err("agent_context_session_scan_limit_reached:narrow the filters".into());
        }
        let (event_id, agent, runtime, created, status, resume, bootstrap, claimed, authentication, authority) =
            row.map_err(db_error)?;
        if identity.is_some_and(|v| v != agent)
            || substrate.is_some_and(|v| v != runtime)
            || date_from
                .as_ref()
                .is_some_and(|v| created.as_str() < v.as_str())
            || date_to
                .as_ref()
                .is_some_and(|v| created.as_str() > v.as_str())
        {
            continue;
        }
        total_matched += 1;
        if matched < offset {
            matched += 1;
            continue;
        }
        let seconds = chrono::DateTime::parse_from_rfc3339(&created)
            .ok()
            .map(|start| (now.timestamp() - start.timestamp()).max(0));
        sessions.push(json!({"event_id":event_id,"identity":agent,"substrate":runtime,"runtime":runtime,"status":status,"created_at":created,"resume_command":resume,"bootstrap_artifact_uri":bootstrap,"claimed_identity":claimed.clone().and_then(|value|serde_json::from_str::<Value>(&value).ok()),"authenticated_identity":authentication.as_ref().and_then(|value|serde_json::from_str::<Value>(value).ok()).and_then(|value|value.get("authenticated_identity").cloned()),"authentication":authentication.clone().and_then(|value|serde_json::from_str::<Value>(&value).ok()).unwrap_or_else(||json!({"status":"missing","authenticated_identity":null,"evidence_refs":[]})),"authority":authority.clone().and_then(|value|serde_json::from_str::<Value>(&value).ok()).unwrap_or_else(||json!({"status":"not_evaluated","operation":null,"granted":false,"evidence_refs":[]})),"identity_state":{"schema":"narada.agent.identity_state.v1","claimed_identity":claimed.as_ref().and_then(|value|serde_json::from_str::<Value>(value).ok()).unwrap_or_else(||json!({"identity":null,"status":"unclaimed","source":null,"asserted_at":null,"evidence_refs":[],"authority_granted":false})),"authentication":authentication.as_ref().and_then(|value|serde_json::from_str::<Value>(value).ok()).unwrap_or_else(||json!({"status":"missing","authenticated_identity":null,"evidence_refs":[]})),"authority":authority.as_ref().and_then(|value|serde_json::from_str::<Value>(value).ok()).unwrap_or_else(||json!({"status":"not_evaluated","operation":null,"granted":false,"evidence_refs":[]}))},"duration_estimate":{"seconds":seconds,"basis":"elapsed_since_start_no_end_event","as_of":generated}}));
        if sessions.len() > limit {
            break;
        }
    }
    let has_more = sessions.len() > limit;
    if has_more {
        sessions.pop();
    }
    let mut latest = serde_json::Map::new();
    for session in &sessions {
        if let Some(agent) = session.get("identity").and_then(Value::as_str) {
            if !latest.contains_key(agent) {
                latest.insert(agent.into(), session.clone());
            }
        }
    }
    Ok(
        json!({"status":"ok","schema":"narada.agent_context.sessions.v0","authority":"agent_context_sqlite","generated_at":generated,"filters":{"identity":args.get("identity").cloned().unwrap_or(Value::Null),"date_from":args.get("date_from").cloned().unwrap_or(Value::Null),"date_to":args.get("date_to").cloned().unwrap_or(Value::Null),"substrate":args.get("substrate").cloned().unwrap_or(Value::Null),"limit":limit,"offset":offset},"session_count":sessions.len(),"total_count":total_matched,"has_more":offset+sessions.len()<total_matched,"next_offset":if offset+sessions.len()<total_matched{Some(offset+sessions.len())}else{None},"truncated":offset+sessions.len()<total_matched,"truncation_reason":if offset+sessions.len()<total_matched{Some("session_page_limit")}else{None},"sessions":sessions,"latest_session_per_identity":latest,"duration_estimate_note":"agent_start_events has no end timestamp; duration is elapsed time from created_at to generated_at."}),
    )
}
fn parse_date_filter(value: Option<&Value>, field: &str) -> Result<Option<String>, String> {
    let Some(value) = value.filter(|v| !v.is_null()) else {
        return Ok(None);
    };
    let text = value.as_str().unwrap_or(&value.to_string()).to_string();
    chrono::DateTime::parse_from_rfc3339(&text)
        .map(|v| Some(v.to_utc().to_rfc3339_opts(SecondsFormat::Millis, true)))
        .map_err(|_| format!("invalid_{field}: {text}"))
}

