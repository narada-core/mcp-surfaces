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
