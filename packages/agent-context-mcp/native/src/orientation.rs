use crate::state::Context;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use hmac::{Hmac, Mac};
use rusqlite::{params, OptionalExtension};
use serde_json::{json, Value};
use sha2::Sha256;
use std::env;

pub fn read(context: &Context, projection: &str, args: &Value) -> Result<Value, String> {
    let evidence = match evidence(context, args) {
        Ok(evidence) => evidence,
        Err(reason)
            if matches!(
                reason.as_str(),
                "agent_context_exact_admission_receipt_required"
                    | "agent_context_exact_orientation_manifest_id_required"
                    | "agent_context_exact_orientation_delivery_receipt_required"
            ) =>
        {
            return Ok(orientation_unavailable(&reason));
        }
        Err(reason) => return Err(reason),
    };
    let packet = entry_packet(context, &evidence)?;
    if projection == "admin" {
        return admin_read(context, args, &evidence, packet);
    }
    if packet.get("ordinary_work_gate").and_then(Value::as_str) == Some("open") {
        return Ok(ready(&packet, None));
    }
    if let Some(value) = args.get("continuation") {
        let decoded = match parse_continuation(
            value,
            &packet["orientation_brief"],
            &evidence.delivery,
        ) {
            Ok(value) => value,
            Err(error) => {
                return Ok(json!({
                    "schema":"narada.agent_context.orientation_recovery.v1","status":"orientation_required",
                    "ordinary_work_gate":packet["ordinary_work_gate"],"reason_code":error,
                    "remediation":"Use this response next_call exactly; do not reconstruct a continuation.",
                    "next_call":continuation_for(packet.get("next_call"),&packet["orientation_brief"],&evidence.delivery)?
                }))
            }
        };
        if decoded.get("phase").and_then(Value::as_str) == Some("acknowledge") {
            let record = acknowledge(context, &evidence, &packet)?;
            let acknowledged = entry_packet(context, &evidence)?;
            let reference = record
                .pointer("/acknowledgement/acknowledgement_id")
                .and_then(Value::as_str)
                .map(|id| format!("agent-context:orientation_acknowledgements:{id}"));
            return Ok(ready(&acknowledged, reference.as_deref()));
        }
        let result = required_read(
            context,
            &evidence,
            &packet,
            decoded.get("step_id").and_then(Value::as_str).unwrap_or(""),
            decoded.get("offset").and_then(Value::as_i64).unwrap_or(-1),
        )?;
        return occupant_material(&result, &packet, &evidence.delivery);
    }
    occupant_entry(&packet, &evidence.delivery)
}

fn orientation_unavailable(reason: &str) -> Value {
    json!({
        "schema":"narada.agent_context.orientation_unavailable.v2",
        "status":"anonymous",
        "ordinary_work_gate":"open",
        "reason_code":reason,
        "source_mutation":false,
        "missing_carrier_entry_evidence":true,
        "retry_safe":true,
        "authority_effect":{
            "identity_authority":"unavailable",
            "identity_bearing_operations":"blocked",
            "materialized_site_authority":"unaffected",
            "ordinary_non_identity_work":"allowed"
        },
        "recovery":{
            "owner":"carrier_session_launcher",
            "action":"start_an_admitted_carrier_session",
            "required_for":"identity-bearing operations only",
            "restart_required":true,
            "instruction":"Use the current materialized carrier authority for non-identity work. Start the carrier through Narada's admitted-session launcher only when a named identity is required, then call agent_orientation_read({}) again."
        }
    })
}

struct Evidence {
    admission: Value,
    delivery: Value,
    manifest_id: String,
}
fn evidence(context: &Context, args: &Value) -> Result<Evidence, String> {
    let admission = admission(context, args)?;
    let manifest_supplied = args
        .get("manifest_id")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty());
    let manifest_inherited = env::var("NARADA_ORIENTATION_MANIFEST_ID")
        .ok()
        .filter(|value| !value.trim().is_empty());
    if manifest_supplied.is_some()
        && manifest_inherited
            .as_deref()
            .is_some_and(|value| Some(value) != manifest_supplied)
    {
        return Err("agent_context_conflicting_orientation_manifest_ids".into());
    }
    let manifest_id = manifest_supplied
        .map(str::to_string)
        .or(manifest_inherited)
        .ok_or("agent_context_exact_orientation_manifest_id_required")?;
    let delivery = exact_json(
        args,
        "delivery_receipt",
        "NARADA_ORIENTATION_DELIVERY_RECEIPT",
        "agent_context_exact_orientation_delivery_receipt_required",
    )?;
    if delivery.get("schema").and_then(Value::as_str)
        != Some("narada.carrier_session.orientation_delivery_receipt.v1")
    {
        return Err("agent_context_orientation_delivery_receipt_invalid:schema_mismatch".into());
    }
    Ok(Evidence {
        admission,
        delivery,
        manifest_id,
    })
}

fn admission(context: &Context, args: &Value) -> Result<Value, String> {
    let admission = exact_json(
        args,
        "admission_receipt",
        "NARADA_CARRIER_SESSION_ADMISSION_RECEIPT",
        "agent_context_exact_admission_receipt_required",
    )?;
    if admission.get("schema").and_then(Value::as_str)
        != Some("narada.carrier_session.admission_receipt.v0")
        || admission.get("decision").and_then(Value::as_str) != Some("admitted")
    {
        return Err("agent_context_exact_admission_receipt_required".into());
    }
    let identity = env::var("NARADA_AGENT_ID").ok().unwrap_or_else(|| {
        admission
            .pointer("/agent_identity/local_agent_id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .into()
    });
    if admission
        .pointer("/agent_identity/local_agent_id")
        .and_then(Value::as_str)
        != Some(identity.as_str())
    {
        return Err("agent_context_admission_identity_mismatch".into());
    }
    let expected_site = if context.site_id.starts_with("site:") {
        context.site_id.clone()
    } else {
        format!("site:{}", context.site_id)
    };
    if admission
        .pointer("/coordinate/site_ref")
        .and_then(Value::as_str)
        != Some(expected_site.as_str())
    {
        return Err("agent_context_admission_site_mismatch".into());
    }
    if let Ok(session) = env::var("NARADA_CARRIER_SESSION_ID") {
        if admission
            .pointer("/coordinate/carrier_session_id")
            .and_then(Value::as_str)
            != Some(session.as_str())
        {
            return Err("agent_context_admission_session_mismatch".into());
        }
    }
    Ok(admission)
}

pub fn whoami(context: &Context, args: &Value) -> Result<Value, String> {
    let has_receipt = args.get("admission_receipt").is_some()
        || env::var_os("NARADA_CARRIER_SESSION_ADMISSION_RECEIPT").is_some();
    if !has_receipt {
        return Ok(
            json!({"schema":"narada.agent_context.identity_resolution.v1","status":"blocked","reason":"agent_context_exact_admission_receipt_required","rejected_fallbacks":["latest_checkpoint","latest_start_event","identity_name_inference"]}),
        );
    }
    let admission = admission(context, args)?;
    let identity = admission
        .pointer("/agent_identity/local_agent_id")
        .and_then(Value::as_str)
        .unwrap_or("");
    let hint = args.get("hint").and_then(Value::as_str);
    Ok(
        json!({"schema":"narada.agent_context.identity_resolution.v1","status":"ok","identity":identity,"canonical_agent_id":admission.pointer("/agent_identity/canonical_agent_id").cloned().unwrap_or(Value::Null),"confidence":"exact","source":"carrier_session_admission_receipt","admission_receipt_ref":admission["receipt_id"],"carrier_session":admission["coordinate"],"authority_readback_ref":admission["authority_readback_ref"],"hint_match":hint.map(|v|v==identity||Some(v)==admission.pointer("/agent_identity/canonical_agent_id").and_then(Value::as_str))}),
    )
}

pub fn startup(context: &Context, args: &Value) -> Result<Value, String> {
    for forbidden in ["checkpoint_id", "checkpoint_startup", "generated_at"] {
        if args.get(forbidden).is_some() {
            return Ok(
                json!({"schema":"narada.agent_context.orientation_delivery.v1","status":"blocked","source_mutation":false,"reason":"orientation_startup_exact_generation_only","rejected_argument":forbidden,"required_next_step":"Use agent_context_hydrate_current for a separately identified diagnostic candidate generation."}),
            );
        }
    }
    let evidence = evidence(context, args)?;
    let mut packet = entry_packet(context, &evidence)?;
    packet.as_object_mut().unwrap().insert(
        "compatibility_alias".into(),
        json!("agent_context_startup_sequence"),
    );
    packet
        .as_object_mut()
        .unwrap()
        .insert("canonical_tool".into(), json!("agent_orientation_read"));
    Ok(packet)
}

pub fn acknowledge_tool(context: &Context, args: &Value) -> Result<Value, String> {
    let evidence = evidence(context, args)?;
    let packet = entry_packet(context, &evidence)?;
    acknowledge(context, &evidence, &packet)
}
fn exact_json(args: &Value, field: &str, variable: &str, missing: &str) -> Result<Value, String> {
    let supplied = args.get(field).filter(|value| !value.is_null()).cloned();
    let inherited = env::var(variable)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .map(|raw| serde_json::from_str(&raw).map_err(|error| format!("{missing}:{error}")))
        .transpose()?;
    if supplied.is_some() && inherited.is_some() && supplied != inherited {
        return Err(format!("agent_context_conflicting_{field}s"));
    }
    supplied.or(inherited).ok_or_else(|| missing.to_string())
}

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
        json!({"schema":"narada.agent_context.orientation_entry_packet.v2","status":if acknowledgement.is_some(){"acknowledged"}else{"orientation_required"},"source_mutation":false,"ordinary_work_gate":if acknowledgement.is_some(){"open"}else{"acknowledgement_required"},"orientation_brief":brief,"manifest_ref":brief["manifest_ref"],"delivery_receipt_ref":evidence.delivery["receipt_id"],"acknowledgement_ref":acknowledgement.as_ref().and_then(|v|serde_json::from_str::<Value>(v).ok()).and_then(|v|v.get("acknowledgement_id").and_then(Value::as_str).map(|id|format!("agent-context:orientation_acknowledgements:{id}"))),"required_read_progress":{"total":progress.total,"completed":progress.completed.len(),"pending":progress.pending.len(),"completed_step_ids":progress.completed,"pending_step_ids":progress.pending,"completion_refs":progress.refs,"active_step_id":progress.active,"next_byte_offset":progress.offset},"next_call":if acknowledgement.is_some(){Value::Null}else{progress.next_call}}),
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
        json!({"schema":"narada.agent_context.orientation_entry.v3","status":"orientation_required","source_mutation":false,"ordinary_work_gate":"acknowledgement_required","orientation_brief":occupant_brief(brief),"manifest_ref":packet["manifest_ref"],"required_read_progress":{"total":packet.pointer("/required_read_progress/total").cloned().unwrap_or(json!(0)),"completed":packet.pointer("/required_read_progress/completed").cloned().unwrap_or(json!(0)),"pending":packet.pointer("/required_read_progress/pending").cloned().unwrap_or(json!(0))},"next_call":continuation_for(packet.get("next_call"),brief,delivery)?}),
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

fn required_read(
    context: &Context,
    evidence: &Evidence,
    packet: &Value,
    step_id: &str,
    offset: i64,
) -> Result<Value, String> {
    if step_id.is_empty() {
        return Err("agent_context_orientation_required_read_step_id_required".into());
    }
    if offset < 0 {
        return Err("agent_context_orientation_required_read_offset_invalid".into());
    }
    let brief = &packet["orientation_brief"];
    let step = brief
        .get("required_reads")
        .and_then(Value::as_array)
        .and_then(|steps| {
            steps
                .iter()
                .find(|step| step.get("step_id").and_then(Value::as_str) == Some(step_id))
        })
        .ok_or_else(|| format!("agent_context_orientation_required_read_step_unknown:{step_id}"))?;
    let artifact = step
        .pointer("/source/artifact_ref")
        .and_then(Value::as_str)
        .unwrap_or("");
    let relative = artifact.strip_prefix("site-file:").ok_or_else(|| {
        format!("agent_context_orientation_required_read_source_unsupported:{artifact}")
    })?;
    if relative.is_empty()
        || relative.contains("..")
        || std::path::Path::new(relative).is_absolute()
    {
        return Err(format!(
            "agent_context_orientation_required_read_source_invalid:{artifact}"
        ));
    }
    let content = std::fs::read_to_string(context.site_root.join(relative)).map_err(|error| {
        format!("agent_context_orientation_required_read_source_missing:{error}")
    })?;
    use sha2::Digest;
    let content_hash = format!("{:x}", Sha256::digest(content.as_bytes()));
    if step.pointer("/source/revision").and_then(Value::as_str) != Some(content_hash.as_str()) {
        return Err(format!("agent_context_orientation_required_read_source_stale:{step_id}:expected={}:actual={content_hash}", step.pointer("/source/revision").and_then(Value::as_str).unwrap_or("")));
    }
    let db = context.open_db()?;
    let receipt = evidence.delivery["receipt_id"].as_str().unwrap_or("");
    let existing_completion: Option<String> = db.query_row(
        "SELECT completion_json FROM orientation_required_read_completions WHERE delivery_receipt_ref=?1 AND step_id=?2 LIMIT 1",
        params![receipt, step_id], |row| row.get(0),
    ).optional().map_err(db_error)?;
    if let Some(completion_text) = existing_completion {
        let completion: Value = serde_json::from_str(&completion_text).map_err(|_| {
            format!("agent_context_orientation_required_read_completion_invalid:{step_id}")
        })?;
        let existing_page: Option<String> = db.query_row(
            "SELECT page_json FROM orientation_required_read_pages WHERE delivery_receipt_ref=?1 AND step_id=?2 AND byte_offset=?3 LIMIT 1",
            params![receipt, step_id, offset], |row| row.get(0),
        ).optional().map_err(db_error)?;
        let page = existing_page
            .map(|text| {
                serde_json::from_str::<Value>(&text).map_err(|_| {
                    format!("agent_context_orientation_required_read_page_invalid:{step_id}")
                })
            })
            .transpose()?;
        if let Some(value) = &page {
            let start = value["byte_offset"].as_u64().unwrap_or(u64::MAX) as usize;
            let end = value["next_byte_offset"].as_u64().unwrap_or(u64::MAX) as usize;
            let expected = content
                .as_bytes()
                .get(start..end)
                .and_then(|bytes| std::str::from_utf8(bytes).ok());
            if value["content_sha256"] != content_hash || value["content"].as_str() != expected {
                return Err(format!(
                    "agent_context_orientation_required_read_page_source_conflict:{step_id}"
                ));
            }
        }
        let current = progress(&db, brief, &evidence.delivery)?;
        let mut public_page = page.clone().unwrap_or(Value::Null);
        if let Some(object) = public_page.as_object_mut() {
            object.remove("content");
        }
        return Ok(
            json!({"schema":"narada.agent_context.orientation_required_read.v1","status":"already_completed","source_mutation":false,"local_persistence":true,"ordinary_work_gate":"acknowledgement_required","step_id":step_id,"source":step["source"],"content":page.as_ref().and_then(|v|v["content"].as_str()).map(Value::from).unwrap_or(Value::Null),"page":public_page,"result_evidence":completion["result_evidence"],"completion_ref":format!("agent-context:orientation_required_read_completions:orientation-read:{receipt}:{step_id}"),"required_read_progress":{"total":current.total,"completed":current.completed.len(),"pending":current.pending.len(),"completed_step_ids":current.completed,"pending_step_ids":current.pending,"completion_refs":current.refs,"active_step_id":current.active,"next_byte_offset":current.offset},"next_call":current.next_call}),
        );
    }
    let before = progress(&db, brief, &evidence.delivery)?;
    if before.active.as_deref() != Some(step_id) {
        return Err(format!(
            "agent_context_orientation_required_read_step_out_of_order:{step_id}:expected={}",
            before.active.as_deref().unwrap_or("none")
        ));
    }
    if before.offset != Some(offset) {
        return Err(format!("agent_context_orientation_required_read_offset_out_of_order:{step_id}:expected={}:actual={offset}", before.offset.unwrap_or(0)));
    }
    let bytes = content.as_bytes();
    if offset as usize > bytes.len() {
        return Err(format!("agent_context_orientation_required_read_offset_out_of_range:{step_id}:total={}:actual={offset}", bytes.len()));
    }
    let end = page_end(bytes, offset as usize);
    let page_bytes = &bytes[offset as usize..end];
    let page_content = std::str::from_utf8(page_bytes)
        .map_err(|_| "agent_context_orientation_required_read_page_boundary_invalid")?;
    let eof = end == bytes.len();
    let page_id = format!("orientation-read-page:{receipt}:{step_id}:{offset}");
    let page_ref = format!("agent-context:orientation_required_read_pages:{page_id}");
    let page = json!({"schema":"narada.agent_context.orientation_required_read_page.v1","page_id":page_id,"delivery_receipt_ref":receipt,"manifest_id":brief.pointer("/manifest_ref/manifest_id").cloned().unwrap_or(Value::Null),"brief_id":brief["brief_id"],"step_id":step_id,"byte_offset":offset,"returned_bytes":page_bytes.len(),"next_byte_offset":end,"eof":eof,"content_sha256":content_hash,"page_sha256":format!("{:x}",Sha256::digest(page_bytes)),"page_ref":page_ref,"content":page_content});
    let completed_at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    db.execute("INSERT INTO orientation_required_read_pages (page_id,delivery_receipt_ref,manifest_id,brief_id,step_id,byte_offset,next_byte_offset,page_json,delivered_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)", params![page_id,receipt,brief.pointer("/manifest_ref/manifest_id").and_then(Value::as_str),brief["brief_id"].as_str(),step_id,offset,end as i64,serde_json::to_string(&page).unwrap(),completed_at]).map_err(db_error)?;
    let normalized = content.replace("\r\n", "\n");
    let result_evidence = json!({"content_sha256":content_hash,"content_window_sha256":format!("{:x}",Sha256::digest(normalized.as_bytes())),"offset":1,"returned_lines":content.split('\n').count()});
    let completion_id = format!("orientation-read:{receipt}:{step_id}");
    let completion_ref =
        format!("agent-context:orientation_required_read_completions:{completion_id}");
    if eof {
        let completion = json!({"step_id":step_id,"tool_name":step.pointer("/tool/name").cloned().unwrap_or(Value::Null),"arguments":step.pointer("/tool/arguments").cloned().unwrap_or_else(||json!({})),"result_evidence":result_evidence,"completed_at":completed_at,"evidence_refs":[completion_ref,format!("sha256:{content_hash}")]});
        db.execute("INSERT INTO orientation_required_read_completions (completion_id,delivery_receipt_ref,manifest_id,brief_id,step_id,completion_json,completed_at) VALUES (?1,?2,?3,?4,?5,?6,?7)", params![completion_id,receipt,brief.pointer("/manifest_ref/manifest_id").and_then(Value::as_str),brief["brief_id"].as_str(),step_id,serde_json::to_string(&completion).unwrap(),completed_at]).map_err(db_error)?;
    }
    let after = progress(&db, brief, &evidence.delivery)?;
    let mut public_page = page.clone();
    public_page.as_object_mut().unwrap().remove("content");
    Ok(
        json!({"schema":"narada.agent_context.orientation_required_read.v1","status":if eof{"completed"}else{"page_emitted"},"source_mutation":false,"local_persistence":true,"ordinary_work_gate":"acknowledgement_required","step_id":step_id,"source":step["source"],"content":page_content,"page":public_page,"result_evidence":if eof{result_evidence}else{Value::Null},"completion_ref":if eof{Value::String(completion_ref)}else{Value::Null},"required_read_progress":{"total":after.total,"completed":after.completed.len(),"pending":after.pending.len(),"completed_step_ids":after.completed,"pending_step_ids":after.pending,"completion_refs":after.refs,"active_step_id":after.active,"next_byte_offset":after.offset},"next_call":after.next_call}),
    )
}

fn page_end(bytes: &[u8], offset: usize) -> usize {
    if offset == bytes.len() {
        return offset;
    }
    let mut end = (offset + 3000).min(bytes.len());
    while end > offset && end < bytes.len() && (bytes[end] & 0xc0) == 0x80 {
        end -= 1;
    }
    while end > offset
        && serde_json::to_vec(&std::str::from_utf8(&bytes[offset..end]).unwrap_or(""))
            .map(|v| v.len())
            .unwrap_or(usize::MAX)
            > 3200
    {
        end -= 1;
        while end > offset && (bytes[end] & 0xc0) == 0x80 {
            end -= 1;
        }
    }
    if end >= bytes.len() {
        return bytes.len();
    }
    let minimum = offset + (end - offset) / 2;
    if let Some(position) = bytes[minimum..end].windows(2).rposition(|v| v == b"\n\n") {
        return minimum + position + 2;
    }
    if let Some(position) = bytes[minimum..end].iter().rposition(|v| *v == b'\n') {
        return minimum + position + 1;
    }
    end
}

fn occupant_material(result: &Value, packet: &Value, delivery: &Value) -> Result<Value, String> {
    let ordinal = packet
        .pointer("/orientation_brief/required_reads")
        .and_then(Value::as_array)
        .and_then(|steps| {
            steps
                .iter()
                .position(|step| step.get("step_id") == result.get("step_id"))
        })
        .map(|index| index + 1);
    Ok(
        json!({"schema":"narada.agent_context.orientation_material.v1","status":"orientation_required","source_mutation":false,"local_persistence":true,"ordinary_work_gate":"acknowledgement_required","material":{"delivery_status":result["status"],"ordinal":ordinal,"source_ref":result.pointer("/source/artifact_ref").cloned().unwrap_or(Value::Null),"content":result["content"],"page":if result["page"].is_null(){Value::Null}else{json!({"returned_bytes":result.pointer("/page/returned_bytes").cloned().unwrap_or(Value::Null),"eof":result.pointer("/page/eof").cloned().unwrap_or(Value::Null)})}},"required_read_progress":{"total":result.pointer("/required_read_progress/total").cloned().unwrap_or(json!(0)),"completed":result.pointer("/required_read_progress/completed").cloned().unwrap_or(json!(0)),"pending":result.pointer("/required_read_progress/pending").cloned().unwrap_or(json!(0))},"next_call":continuation_for(result.get("next_call"),&packet["orientation_brief"],delivery)?}),
    )
}

fn acknowledge(context: &Context, evidence: &Evidence, packet: &Value) -> Result<Value, String> {
    let db = context.open_db()?;
    let brief = &packet["orientation_brief"];
    let current = progress(&db, brief, &evidence.delivery)?;
    if !current.pending.is_empty() {
        return Err(format!(
            "agent_context_orientation_required_reads_incomplete:{}:next={}({})",
            current.pending.join(","),
            current.next_call["tool"].as_str().unwrap_or(""),
            serde_json::to_string(&current.next_call["arguments"]).unwrap()
        ));
    }
    let receipt = evidence.delivery["receipt_id"].as_str().unwrap_or("");
    if let Some(existing) = db.query_row("SELECT acknowledgement_json FROM orientation_acknowledgements WHERE delivery_receipt_ref=?1 LIMIT 1", [receipt], |row| row.get::<_,String>(0)).optional().map_err(db_error)? {
        let acknowledgement: Value = serde_json::from_str(&existing).map_err(|error| error.to_string())?;
        project_acknowledgement(context, &acknowledgement)?;
        return Ok(json!({"schema":"narada.agent_context.orientation_acknowledgement_record.v1","status":"already_acknowledged","source_mutation":false,"local_persistence":true,"ordinary_work_gate":"open","acknowledgement":acknowledgement}));
    }
    let mut statement = db.prepare("SELECT completion_json FROM orientation_required_read_completions WHERE delivery_receipt_ref=?1 ORDER BY completed_at ASC,step_id ASC").map_err(db_error)?;
    let completions = statement
        .query_map([receipt], |row| row.get::<_, String>(0))
        .map_err(db_error)?
        .map(|value| value.map(|text| serde_json::from_str::<Value>(&text).unwrap()))
        .collect::<Result<Vec<_>, _>>()
        .map_err(db_error)?;
    drop(statement);
    let acknowledged_at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    use sha2::Digest;
    let digest_source = json!({"delivery_receipt_ref":receipt,"brief_digest":brief["brief_digest"],"acknowledged_at":acknowledged_at,"required_read_completions":completions});
    let digest = format!(
        "{:x}",
        Sha256::digest(canonical_json(&digest_source).as_bytes())
    );
    let session = evidence
        .admission
        .pointer("/coordinate/carrier_session_id")
        .and_then(Value::as_str)
        .unwrap_or("");
    let epoch = evidence
        .admission
        .pointer("/coordinate/authority_epoch")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    let mut evidence_refs = vec![json!(receipt)];
    for completion in &completions {
        for reference in completion
            .get("evidence_refs")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if !evidence_refs.contains(reference) {
                evidence_refs.push(reference.clone());
            }
        }
    }
    let acknowledgement = json!({"schema":"narada.carrier_session.orientation_acknowledgement.v1","acknowledgement_id":format!("orientation-ack:{session}:{epoch}:{}",&digest[..16]),"status":"acknowledged","coordinate":evidence.admission["coordinate"],"admission_receipt_ref":evidence.admission["receipt_id"],"delivery_receipt_ref":receipt,"manifest_id":brief.pointer("/manifest_ref/manifest_id").cloned().unwrap_or(Value::Null),"manifest_digest":brief.pointer("/manifest_ref/manifest_digest").cloned().unwrap_or(Value::Null),"brief_id":brief["brief_id"],"brief_digest":brief["brief_digest"],"acknowledged_at":acknowledged_at,"required_read_completions":completions,"acknowledgement_semantics":"receipt_and_required_reads_not_comprehension","action_admission":"separate_required","authority_readback_ref":format!("agent-context:orientation_acknowledgements:{receipt}"),"evidence_refs":evidence_refs});
    db.execute("INSERT INTO orientation_acknowledgements (acknowledgement_id,delivery_receipt_ref,manifest_id,brief_id,carrier_session_id,authority_epoch,acknowledgement_json,acknowledged_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)", params![acknowledgement["acknowledgement_id"].as_str(),receipt,brief.pointer("/manifest_ref/manifest_id").and_then(Value::as_str),brief["brief_id"].as_str(),session,epoch,serde_json::to_string(&acknowledgement).unwrap(),acknowledged_at]).map_err(db_error)?;
    project_acknowledgement(context, &acknowledgement)?;
    Ok(
        json!({"schema":"narada.agent_context.orientation_acknowledgement_record.v1","status":"acknowledged","source_mutation":false,"local_persistence":true,"ordinary_work_gate":"open","acknowledgement":acknowledgement}),
    )
}

fn canonical_json(value: &Value) -> String {
    fn sort(value: &Value) -> Value {
        match value {
            Value::Array(values) => Value::Array(values.iter().map(sort).collect()),
            Value::Object(object) => {
                let mut keys = object.keys().collect::<Vec<_>>();
                keys.sort();
                let mut result = serde_json::Map::new();
                for key in keys {
                    result.insert(key.clone(), sort(&object[key]));
                }
                Value::Object(result)
            }
            _ => value.clone(),
        }
    }
    serde_json::to_string(&sort(value)).unwrap()
}

fn project_acknowledgement(context: &Context, acknowledgement: &Value) -> Result<(), String> {
    let path = std::path::PathBuf::from(
        env::var("NARADA_ORIENTATION_ENTRY_FILE")
            .map_err(|_| "agent_context_exact_orientation_entry_file_required")?,
    );
    let admitted = context
        .site_root
        .join(".ai/runtime/orientation-entry")
        .canonicalize()
        .map_err(|_| "agent_context_orientation_entry_file_outside_admitted_root")?;
    let parent = path
        .parent()
        .ok_or("agent_context_orientation_entry_file_outside_admitted_root")?
        .canonicalize()
        .map_err(|_| "agent_context_orientation_entry_file_not_found")?;
    if !parent.starts_with(admitted) {
        return Err(format!(
            "agent_context_orientation_entry_file_outside_admitted_root:{}",
            path.display()
        ));
    }
    let projection = json!({"schema":"narada.carrier_entry.orientation_acknowledgement_projection.v1","status":"open","ordinary_work_gate":"open","delivery_receipt_ref":acknowledgement["delivery_receipt_ref"],"manifest_id":acknowledgement["manifest_id"],"manifest_digest":acknowledgement["manifest_digest"],"brief_id":acknowledgement["brief_id"],"brief_digest":acknowledgement["brief_digest"],"coordinate":acknowledgement["coordinate"],"acknowledgement_ref":acknowledgement["acknowledgement_id"],"acknowledged_at":acknowledgement["acknowledged_at"],"acknowledgement_semantics":acknowledgement["acknowledgement_semantics"],"action_admission":acknowledgement["action_admission"],"canonical_readback_ref":acknowledgement["authority_readback_ref"],"projection_posture":"derived_readback_not_independent_authority"});
    let output = parent.join("acknowledgement.json");
    let serialized = format!("{}\n", serde_json::to_string_pretty(&projection).unwrap());
    if output.exists() {
        if std::fs::read_to_string(&output).ok().as_deref() != Some(serialized.as_str()) {
            return Err("agent_context_orientation_acknowledgement_projection_conflict".into());
        }
    } else {
        std::fs::write(output, serialized).map_err(|error| {
            format!("agent_context_orientation_acknowledgement_projection_write_failed:{error}")
        })?;
    }
    Ok(())
}
fn ready(packet: &Value, ack: Option<&str>) -> Value {
    let brief = occupant_brief(&packet["orientation_brief"]);
    let mut orientation = brief.as_object().unwrap().clone();
    orientation.remove("schema");
    orientation.remove("required_reads");
    let work_call = brief
        .pointer("/work/inspection_call")
        .filter(|_| brief.pointer("/work/mode").and_then(Value::as_str) == Some("exact"))
        .cloned()
        .unwrap_or(Value::Null);
    orientation.insert(
        "schema".into(),
        json!("narada.orientation_ready_projection.v1"),
    );
    orientation.insert("orientation_status".into(), json!("acknowledged"));
    orientation.insert("next_meaningful_call".into(), work_call.clone());
    json!({"schema":"narada.agent_context.orientation_ready.v1","status":"ready","source_mutation":false,"local_persistence":true,"ordinary_work_gate":"open","orientation":orientation,"manifest_ref":packet["manifest_ref"],"acknowledgement_ref":ack.map(Value::from).unwrap_or_else(||packet["acknowledgement_ref"].clone()),"next_call":null,"suggested_next_call":work_call})
}
fn admin_read(
    context: &Context,
    args: &Value,
    evidence: &Evidence,
    packet: Value,
) -> Result<Value, String> {
    let step_id = args
        .get("step_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or("");
    let selection = args
        .get("selection")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or("");
    if !step_id.is_empty() && !selection.is_empty() {
        return Err("agent_context_orientation_read_mode_ambiguous".into());
    }
    if step_id.is_empty() && args.get("offset").is_some() {
        return Err("agent_context_orientation_required_read_step_id_required_for_offset".into());
    }
    if !selection.is_empty() {
        let field = match selection {
            "continuity" => "continuity_selection",
            "work" => "work_selection",
            _ => {
                return Err(format!(
                    "agent_context_orientation_selection_invalid:{selection}"
                ))
            }
        };
        let selected = &packet["orientation_brief"][field];
        if selected.get("mode").and_then(Value::as_str) == Some("omitted") {
            return Ok(
                json!({"schema":"narada.agent_context.orientation_selection_read.v1","status":"omitted","source_mutation":false,"ordinary_work_gate":packet["ordinary_work_gate"],"selection_kind":selection,"manifest_ref":packet["manifest_ref"],"selection":selected,"projection":null}),
            );
        }
        let db = context.open_db()?;
        let manifest_text:String=db.query_row("SELECT manifest_json FROM orientation_manifest_generations WHERE manifest_id=?1 LIMIT 1",[&evidence.manifest_id],|row|row.get(0)).map_err(db_error)?;
        let manifest: Value = serde_json::from_str(&manifest_text).map_err(|_| {
            format!(
                "agent_context_orientation_manifest_generation_json_invalid:{}",
                evidence.manifest_id
            )
        })?;
        let compartment = if selection == "continuity" {
            "continuity"
        } else {
            "work_orientation"
        };
        let entry = manifest
            .get("entries")
            .and_then(Value::as_array)
            .and_then(|values| {
                values.iter().find(|entry| {
                    entry.get("compartment").and_then(Value::as_str) == Some(compartment)
                        && entry.get("projection_status").and_then(Value::as_str)
                            == Some("available")
                })
            })
            .ok_or_else(|| {
                format!("agent_context_orientation_selection_binding_mismatch:{selection}")
            })?;
        if entry.get("artifact_ref") != selected.get("artifact_ref")
            || entry.get("revision") != selected.get("revision")
        {
            return Err(format!(
                "agent_context_orientation_selection_binding_mismatch:{selection}"
            ));
        }
        return Ok(
            json!({"schema":"narada.agent_context.orientation_selection_read.v1","status":"exact","source_mutation":false,"ordinary_work_gate":packet["ordinary_work_gate"],"selection_kind":selection,"manifest_ref":packet["manifest_ref"],"selection":selected,"projection":{"entry_id":entry["entry_id"],"source_authority_ref":entry["source_authority_ref"],"artifact_ref":entry["artifact_ref"],"revision":entry["revision"],"observed_at":entry["observed_at"],"revalidation_rule":entry["revalidation_rule"],"payload":entry["payload"],"rendered_text":entry["rendered_text"]}}),
        );
    }
    if !step_id.is_empty() {
        return required_read(
            context,
            evidence,
            &packet,
            step_id,
            args.get("offset").and_then(Value::as_i64).unwrap_or(0),
        );
    }
    Ok(packet)
}
fn db_error(error: rusqlite::Error) -> String {
    format!("agent_context_db_error:{error}")
}

#[cfg(test)]
mod ergonomics_tests {
    use super::*;

    #[test]
    fn missing_carrier_entry_evidence_is_a_bounded_recoverable_result() {
        let result = orientation_unavailable("agent_context_exact_admission_receipt_required");
        assert_eq!(result["status"], "anonymous");
        assert_eq!(result["ordinary_work_gate"], "open");
        assert_eq!(
            result["authority_effect"]["materialized_site_authority"],
            "unaffected"
        );
        assert_eq!(result["recovery"]["owner"], "carrier_session_launcher");
        assert_eq!(result["retry_safe"], true);
    }
}
