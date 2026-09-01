fn entry_packet(context: &Context, evidence: &Evidence) -> Result<Value, String> {
    let db = context.open_db()?;
    let manifest_row=db.query_row("SELECT admission_receipt_ref,carrier_session_id,authority_epoch,readiness,delivery,manifest_json,generated_at FROM orientation_manifest_generations WHERE manifest_id=?1 LIMIT 1",[&evidence.manifest_id],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,i64>(2)?,r.get::<_,String>(3)?,r.get::<_,String>(4)?,r.get::<_,String>(5)?,r.get::<_,String>(6)?))).optional().map_err(db_error)?.ok_or_else(||format!("agent_context_orientation_manifest_generation_not_found:{}",evidence.manifest_id))?;
    if manifest_row.0 != evidence.admission["receipt_id"].as_str().unwrap_or("")
        || manifest_row.1
            != evidence
                .admission
                .pointer("/coordinate/carrier_session_id")
                .and_then(Value::as_str)
                .unwrap_or("")
        || manifest_row.2
            != evidence
                .admission
                .pointer("/coordinate/authority_epoch")
                .and_then(Value::as_i64)
                .unwrap_or(-1)
    {
        return Err(format!(
            "agent_context_orientation_manifest_generation_index_mismatch:{}",
            evidence.manifest_id
        ));
    }
    let brief_json: String = db
        .query_row(
            "SELECT brief_json FROM orientation_brief_generations WHERE manifest_id=?1 LIMIT 1",
            [&evidence.manifest_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(db_error)?
        .ok_or_else(|| {
            format!(
                "agent_context_orientation_brief_generation_not_found:{}",
                evidence.manifest_id
            )
        })?;
    let brief: Value = serde_json::from_str(&brief_json).map_err(|_| {
        format!(
            "agent_context_orientation_brief_generation_json_invalid:{}",
            evidence.manifest_id
        )
    })?;
    bind_delivery(evidence, &brief)?;
    let stored: String = db
        .query_row(
            "SELECT receipt_json FROM orientation_delivery_receipts WHERE receipt_id=?1 LIMIT 1",
            [evidence.delivery["receipt_id"].as_str().unwrap_or("")],
            |r| r.get(0),
        )
        .optional()
        .map_err(db_error)?
        .ok_or_else(|| {
            format!(
                "agent_context_orientation_delivery_receipt_not_persisted:{}",
                evidence.delivery["receipt_id"].as_str().unwrap_or("")
            )
        })?;
    if serde_json::from_str::<Value>(&stored).ok().as_ref() != Some(&evidence.delivery) {
        return Err(format!(
            "agent_context_orientation_delivery_receipt_not_persisted:{}",
            evidence.delivery["receipt_id"].as_str().unwrap_or("")
        ));
    }
    let acknowledgement:Option<String>=db.query_row("SELECT acknowledgement_json FROM orientation_acknowledgements WHERE delivery_receipt_ref=?1 LIMIT 1",[evidence.delivery["receipt_id"].as_str().unwrap_or("")],|r|r.get(0)).optional().map_err(db_error)?;
    let progress = progress(&db, &brief, &evidence.delivery)?;
    Ok(
        json!({"schema":"narada.agent_context.orientation_entry_packet.v2","status":if acknowledgement.is_some(){"acknowledged"}else{"orientation_required"},"source_mutation":false,"ordinary_work_gate":if acknowledgement.is_some(){"open"}else{"acknowledgement_required"},"orientation_brief":brief,"manifest_ref":brief["manifest_ref"],"claimed_identity":{"identity":evidence.admission.pointer("/agent_identity/local_agent_id"),"status":"claimed","source":"carrier_session_admission_receipt","asserted_at":null,"evidence_refs":[],"authority_granted":false},"authenticated_identity":evidence.admission.pointer("/agent_identity/local_agent_id"),"authentication":{"status":"authenticated","authenticated_identity":evidence.admission.pointer("/agent_identity/local_agent_id"),"evidence_refs":[evidence.admission["receipt_id"].clone(),evidence.admission["authority_readback_ref"].clone()]},"authority":{"status":"not_evaluated","operation":null,"granted":false,"evidence_refs":[]},"identity_state":{"schema":"narada.agent.identity_state.v1","claimed_identity":{"identity":evidence.admission.pointer("/agent_identity/local_agent_id"),"status":"claimed","source":"carrier_session_admission_receipt","asserted_at":null,"evidence_refs":[],"authority_granted":false},"authentication":{"status":"authenticated","authenticated_identity":evidence.admission.pointer("/agent_identity/local_agent_id"),"evidence_refs":[evidence.admission["receipt_id"].clone(),evidence.admission["authority_readback_ref"].clone()]},"authority":{"status":"not_evaluated","operation":null,"granted":false,"evidence_refs":[]}},"delivery_receipt_ref":evidence.delivery["receipt_id"],"acknowledgement_ref":acknowledgement.as_ref().and_then(|v|serde_json::from_str::<Value>(v).ok()).and_then(|v|v.get("acknowledgement_id").and_then(Value::as_str).map(|id|format!("agent-context:orientation_acknowledgements:{id}"))),"required_read_progress":{"total":progress.total,"completed":progress.completed.len(),"pending":progress.pending.len(),"completed_step_ids":progress.completed,"pending_step_ids":progress.pending,"completion_refs":progress.refs,"active_step_id":progress.active,"next_byte_offset":progress.offset},"next_call":if acknowledgement.is_some(){Value::Null}else{progress.next_call}}),
    )
}
fn bind_delivery(e: &Evidence, brief: &Value) -> Result<(), String> {
    let d = &e.delivery;
    if d.get("status").and_then(Value::as_str) != Some("delivered")
        || d.get("admission_receipt_ref") != e.admission.get("receipt_id")
        || d.get("manifest_id") != brief.pointer("/manifest_ref/manifest_id")
        || d.get("manifest_digest") != brief.pointer("/manifest_ref/manifest_digest")
        || d.get("brief_id") != brief.get("brief_id")
        || d.get("brief_digest") != brief.get("brief_digest")
        || d.get("coordinate") != e.admission.get("coordinate")
    {
        return Err("orientation_delivery_receipt_binding_mismatch".into());
    }
    Ok(())
}
struct Progress {
    total: usize,
    completed: Vec<String>,
    pending: Vec<String>,
    refs: Vec<String>,
    active: Option<String>,
    offset: Option<i64>,
    next_call: Value,
}
fn progress(
    db: &rusqlite::Connection,
    brief: &Value,
    delivery: &Value,
) -> Result<Progress, String> {
    let steps = brief
        .get("required_reads")
        .and_then(Value::as_array)
        .ok_or("orientation_required_reads_missing")?;
    let receipt = delivery["receipt_id"].as_str().unwrap_or("");
    let mut completed = Vec::new();
    let mut refs = Vec::new();
    let mut pending = Vec::new();
    for step in steps {
        let id = step["step_id"].as_str().unwrap_or("");
        let completion:Option<String>=db.query_row("SELECT completion_json FROM orientation_required_read_completions WHERE delivery_receipt_ref=?1 AND step_id=?2 LIMIT 1",params![receipt,id],|r|r.get(0)).optional().map_err(db_error)?;
        if let Some(value) = completion {
            completed.push(id.into());
            if let Ok(parsed) = serde_json::from_str::<Value>(&value) {
                for reference in parsed
                    .get("evidence_refs")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .filter(|v| {
                        v.starts_with("agent-context:orientation_required_read_completions:")
                    })
                {
                    refs.push(reference.into())
                }
            }
        } else {
            pending.push(id.into())
        }
    }
    let active = pending.first().cloned();
    let offset = if let Some(id) = active.as_ref() {
        Some(db.query_row("SELECT COALESCE(MAX(next_byte_offset),0) FROM orientation_required_read_pages WHERE delivery_receipt_ref=?1 AND step_id=?2",params![receipt,id],|r|r.get(0)).map_err(db_error)?)
    } else {
        None
    };
    let next_call = if let Some(id) = active.as_ref() {
        json!({"tool":"agent_orientation_read","arguments":{"step_id":id,"offset":offset}})
    } else {
        json!({"tool":"agent_orientation_acknowledge","arguments":{}})
    };
    Ok(Progress {
        total: steps.len(),
        completed,
        pending,
        refs,
        active,
        offset,
        next_call,
    })
}

fn occupant_entry(packet: &Value, delivery: &Value) -> Result<Value, String> {
    let brief = &packet["orientation_brief"];
    Ok(
        json!({"schema":"narada.agent_context.orientation_entry.v3","status":"orientation_required","source_mutation":false,"ordinary_work_gate":"acknowledgement_required","identity_state":packet["identity_state"],"claimed_identity":packet["claimed_identity"],"authenticated_identity":packet["authenticated_identity"],"authentication":packet["authentication"],"authority":packet["authority"],"orientation_brief":occupant_brief(brief),"manifest_ref":packet["manifest_ref"],"required_read_progress":{"total":packet.pointer("/required_read_progress/total").cloned().unwrap_or(json!(0)),"completed":packet.pointer("/required_read_progress/completed").cloned().unwrap_or(json!(0)),"pending":packet.pointer("/required_read_progress/pending").cloned().unwrap_or(json!(0))},"next_call":continuation_for(packet.get("next_call"),brief,delivery)?}),
    )
}
fn occupant_brief(brief: &Value) -> Value {
    json!({"schema":"narada.orientation_occupant_brief.v1","position":{"local_agent_id":brief.pointer("/agent_identity/local_agent_id").cloned().unwrap_or(Value::Null),"canonical_agent_id":brief.pointer("/agent_identity/canonical_agent_id").cloned().unwrap_or(Value::Null),"site_ref":brief.pointer("/coordinate/site_ref").cloned().unwrap_or(Value::Null),"carrier_kind":brief["carrier_kind"],"role":occupant_role(brief.get("role_binding"))},"entry_snapshot_at":brief["generated_at"],"manifest_readiness":brief["readiness"],"continuity":occupant_selection(&brief["continuity_selection"]),"work":occupant_selection(&brief["work_selection"]),"required_reads":brief.get("required_reads").and_then(Value::as_array).into_iter().flatten().map(|s|json!({"ordinal":s["ordinal"],"source_ref":s.pointer("/source/artifact_ref").cloned().unwrap_or(Value::Null),"purpose":if s.pointer("/source/artifact_ref").and_then(Value::as_str)==Some("site-file:AGENTS.md"){"site_operating_instructions"}else{"required_orientation_material"}})).collect::<Vec<_>>(),"residual_codes":brief["residual_codes"],"authority_posture":{"continuity":"historical_context_only","selected_work":"entry_orientation_not_action_authority","consequential_action":"owning_admission_still_required"}})
}
fn occupant_role(value: Option<&Value>) -> Value {
    let Some(v) = value.filter(|v| !v.is_null()) else {
        return Value::Null;
    };
    v.get("role")
        .or_else(|| v.get("role_id"))
        .or_else(|| v.pointer("/binding/role"))
        .and_then(Value::as_str)
        .map(|v| json!(v.trim()))
        .unwrap_or(Value::Null)
}
fn occupant_selection(v: &Value) -> Value {
    if v.get("mode").and_then(Value::as_str) == Some("omitted") {
        json!({"mode":"omitted","reason_code":v.get("reason_code").and_then(Value::as_str).unwrap_or("not_selected")})
    } else {
        json!({"mode":"exact","snapshot_posture":"selected_at_carrier_entry_not_live_state","source_authority_ref":v["source_authority_ref"],"artifact_ref":v["artifact_ref"],"revision":v["revision"],"summary":v["summary"],"inspection_call":v["inspection_call"]})
    }
}
fn continuation_for(
    call: Option<&Value>,
    brief: &Value,
    delivery: &Value,
) -> Result<Value, String> {
    let Some(call) = call.filter(|v| !v.is_null()) else {
        return Ok(Value::Null);
    };
    let payload = match call.get("tool").and_then(Value::as_str) {
        Some("agent_orientation_read") => {
            json!({"schema":"narada.agent_context.orientation_continuation.v1","phase":"required_read","step_id":call.pointer("/arguments/step_id").and_then(Value::as_str).unwrap_or(""),"offset":call.pointer("/arguments/offset").and_then(Value::as_i64).unwrap_or(0)})
        }
        Some("agent_orientation_acknowledge") => {
            json!({"schema":"narada.agent_context.orientation_continuation.v1","phase":"acknowledge"})
        }
        other => {
            return Err(format!(
                "agent_context_orientation_internal_next_call_invalid:{}",
                other.unwrap_or("")
            ))
        }
    };
    let encoded = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&payload).unwrap());
    let key = format!(
        "{}\0{}",
        delivery["receipt_id"].as_str().unwrap_or(""),
        brief["brief_digest"].as_str().unwrap_or("")
    );
    let mut mac = Hmac::<Sha256>::new_from_slice(key.as_bytes()).map_err(|e| e.to_string())?;
    mac.update(encoded.as_bytes());
    let signature = URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes());
    Ok(
        json!({"surface_id":"agent-context","tool":"agent_orientation_read","arguments":{"continuation":format!("oc1.{encoded}.{signature}")}}),
    )
}

fn parse_continuation(value: &Value, brief: &Value, delivery: &Value) -> Result<Value, String> {
    let raw = value
        .as_str()
        .filter(|v| !v.trim().is_empty())
        .ok_or("agent_context_orientation_continuation_required")?;
    let parts = raw.trim().split('.').collect::<Vec<_>>();
    if parts.len() != 3 || parts[0] != "oc1" {
        return Err("agent_context_orientation_continuation_invalid".into());
    }
    let key = format!(
        "{}\0{}",
        delivery["receipt_id"].as_str().unwrap_or(""),
        brief["brief_digest"].as_str().unwrap_or("")
    );
    let mut mac = Hmac::<Sha256>::new_from_slice(key.as_bytes())
        .map_err(|_| "agent_context_orientation_continuation_invalid")?;
    mac.update(parts[1].as_bytes());
    let supplied = URL_SAFE_NO_PAD
        .decode(parts[2])
        .map_err(|_| "agent_context_orientation_continuation_binding_mismatch")?;
    if mac.verify_slice(&supplied).is_err() {
        return Err("agent_context_orientation_continuation_binding_mismatch".into());
    }
    let decoded = URL_SAFE_NO_PAD
        .decode(parts[1])
        .map_err(|_| "agent_context_orientation_continuation_invalid")?;
    let payload: Value = serde_json::from_slice(&decoded)
        .map_err(|_| "agent_context_orientation_continuation_invalid")?;
    if payload.get("schema").and_then(Value::as_str)
        != Some("narada.agent_context.orientation_continuation.v1")
        || !matches!(
            payload.get("phase").and_then(Value::as_str),
            Some("required_read" | "acknowledge")
        )
    {
        return Err("agent_context_orientation_continuation_invalid".into());
    }
    if payload.get("phase").and_then(Value::as_str) == Some("required_read")
        && (payload
            .get("step_id")
            .and_then(Value::as_str)
            .is_none_or(str::is_empty)
            || payload
                .get("offset")
                .and_then(Value::as_i64)
                .is_none_or(|v| v < 0))
    {
        return Err("agent_context_orientation_continuation_invalid".into());
    }
    Ok(payload)
}

