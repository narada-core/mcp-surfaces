use crate::state::Context;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{fs, path::PathBuf};

const NEGATIVE_CLAIMS: [(&str, &str); 5] = [
    (
        "orientation_is_not_authorization",
        "This Orientation Manifest does not authorize any later action.",
    ),
    (
        "capability_is_not_authority",
        "Projected tools and capabilities are availability evidence, not authority grants.",
    ),
    (
        "work_reference_is_not_claim",
        "A work reference does not claim, activate, defer, review, or close work.",
    ),
    (
        "checkpoint_is_not_live_truth",
        "Continuity material is historical evidence and must not replace live authority readback.",
    ),
    (
        "acknowledgement_is_not_comprehension",
        "Delivery or acknowledgement does not prove comprehension, competence, or compliance.",
    ),
];

pub struct Materialization {
    pub manifest: Value,
    pub brief: Option<Value>,
}

pub fn compile(
    context: &Context,
    admission: &Value,
    activation: Option<&Value>,
    role_binding: &Value,
    observed_at: &str,
    exact_checkpoint: Option<&Value>,
    portable_continuation: Option<&Value>,
) -> Result<Materialization, String> {
    validate_admission(context, admission, observed_at)?;
    let runtime_binding = validate_activation(admission, activation, observed_at)?;
    let mut entries = projections(
        context,
        admission,
        role_binding,
        observed_at,
        exact_checkpoint,
        portable_continuation,
    )?;
    entries.sort_by_key(projection_key);
    let residuals = entries.iter().filter(|entry| {
        entry.get("projection_status").and_then(Value::as_str) != Some("available")
    }).map(|entry| {
        let status = entry["projection_status"].as_str().unwrap_or("unavailable");
        let kind = entry["entry_kind"].as_str().unwrap_or("projection");
        let code = if entry["compartment"] == "law_and_constraints" && status == "unavailable" {
            "law_source_unavailable".to_string()
        } else { format!("{kind}_{status}") };
        json!({"code":code,"compartment":entry["compartment"],"criticality":entry["criticality"],"message":format!("Projection is {status}."),"source_authority_ref":entry["source_authority_ref"],"artifact_ref":entry["artifact_ref"],"evidence_refs":entry["evidence_refs"]})
    }).collect::<Vec<_>>();
    let blocked = entries.iter().any(|entry| {
        entry["criticality"] == "required" && entry["projection_status"] != "available"
    });
    let degraded = !blocked && !residuals.is_empty();
    let readiness = if blocked {
        "blocked"
    } else if degraded {
        "degraded"
    } else {
        "ready"
    };
    let delivery = if blocked { "withheld" } else { "deliverable" };
    let reason_codes = if blocked {
        vec![json!("required_law_projection_unavailable")]
    } else {
        vec![]
    };
    let rendered_bytes: usize = entries
        .iter()
        .filter_map(|v| v["rendered_text"].as_str())
        .map(str::len)
        .sum();
    let mut source = json!({
        "schema":"narada.orientation_manifest.v0","generated_at":observed_at,
        "coordinate":admission["coordinate"],"admission_receipt_ref":admission["receipt_id"],
        "agent_identity":admission["agent_identity"],"carrier_kind":admission["carrier_kind"],
        "assembly_policy":{"source_authority_ref":"orientation-assembly-policy","artifact_ref":"orientation-policy:agent-context-compatibility","revision":"1"},
        "runtime_binding":runtime_binding,"readiness":readiness,"delivery":delivery,
        "action_admission":"separate_required","entries":entries,"residuals":residuals,
        "negative_claims":NEGATIVE_CLAIMS.iter().map(|(id, statement)| json!({"claim_id":id,"statement":statement,"source_authority_ref":"orientation-policy:agent-context-compatibility","revision":"1"})).collect::<Vec<_>>(),
        "reason_codes":reason_codes,
        "bounds":{"max_entries":24,"max_rendered_bytes":65536,"max_manifest_bytes":262144,"included_entries":entries.len(),"rendered_bytes":rendered_bytes,"manifest_bytes":0,"omitted_entries":0}
    });
    let digest = sha256(&canonical_json(&source));
    let session = admission
        .pointer("/coordinate/carrier_session_id")
        .and_then(Value::as_str)
        .unwrap_or("")
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || "._-".contains(c) {
                c
            } else {
                '_'
            }
        })
        .collect::<String>();
    let epoch = admission
        .pointer("/coordinate/authority_epoch")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    let manifest_id = format!("orientation:{session}:{epoch}:{}", &digest[..16]);
    let object = source
        .as_object_mut()
        .ok_or("agent_context_native_manifest_invalid")?;
    object.insert("manifest_id".into(), json!(manifest_id));
    object.insert("manifest_digest".into(), json!(digest));
    stabilize_bytes(&mut source, "bounds", "manifest_bytes")?;
    let brief = if delivery == "deliverable" {
        Some(build_brief(&source)?)
    } else {
        None
    };
    Ok(Materialization {
        manifest: source,
        brief,
    })
}

fn projections(
    context: &Context,
    admission: &Value,
    role_binding: &Value,
    observed_at: &str,
    exact_checkpoint: Option<&Value>,
    portable_continuation: Option<&Value>,
) -> Result<Vec<Value>, String> {
    let subject = json!({"site_ref":admission.pointer("/coordinate/site_ref"),"agent_ref":admission.pointer("/agent_identity/artifact_ref"),"carrier_session_id":admission.pointer("/coordinate/carrier_session_id")});
    let receipt = admission["receipt_id"].clone();
    let evidence = admission["evidence_refs"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let mut identity_evidence = vec![receipt.clone()];
    identity_evidence.extend(evidence);
    let mut entries = vec![json!({
        "entry_id":"orientation:agent-identity","compartment":"office_and_role","entry_kind":"agent_identity",
        "source_authority_ref":admission.pointer("/agent_identity/source_authority_ref"),"artifact_ref":admission.pointer("/agent_identity/artifact_ref"),"revision":admission.pointer("/agent_identity/revision"),
        "observed_at":observed_at,"valid_until":admission["valid_until"],"criticality":"required","projection_status":"available",
        "revalidation_rule":"on_agent_identity_revision_or_status_change","evidence_refs":identity_evidence,
        "payload":{"local_agent_id":admission.pointer("/agent_identity/local_agent_id"),"canonical_agent_id":admission.pointer("/agent_identity/canonical_agent_id")},
        "rendered_text":format!("Admitted Agent: {}", admission.pointer("/agent_identity/canonical_agent_id").and_then(Value::as_str).unwrap_or("")),"subject":subject
    })];
    if role_binding
        .as_object()
        .map(|v| !v.is_empty())
        .unwrap_or(false)
    {
        let authority = role_binding
            .get("binding_authority")
            .or_else(|| role_binding.get("binding_source"))
            .and_then(Value::as_str)
            .unwrap_or("unavailable");
        let authoritative = authority != "unavailable" && !authority.contains("non_authoritative");
        let revision = sha256(&canonical_json(role_binding));
        entries.push(json!({"entry_id":"orientation:role-binding","compartment":"office_and_role","entry_kind":"role_binding","source_authority_ref":format!("agent-role-binding:{authority}"),"artifact_ref":format!("agent-role-binding:{}",admission.pointer("/agent_identity/canonical_agent_id").and_then(Value::as_str).unwrap_or("")),"revision":revision,"observed_at":observed_at,"valid_until":null,"criticality":"optional","projection_status":if authoritative{"available"}else{"rejected"},"revalidation_rule":if authoritative{"on_role_binding_revision_or_status_change"}else{"replace_with_owner_issued_role_binding"},"evidence_refs":if authoritative{vec![json!(format!("sha256:{revision}"))]}else{vec![]},"payload":{"role_binding":role_binding,"authoritative":authoritative,"grants_capability":false},"rendered_text":if authoritative{json!(format!("Role binding projected from {authority}."))}else{Value::Null},"subject":subject}));
    }
    let (law_path, relative) = law_path(context);
    let mut required_reads = vec![];
    if law_path.exists() {
        let law = fs::read_to_string(&law_path)
            .map_err(|e| format!("agent_context_site_law_read_failed:{e}"))?;
        let revision = sha256(&law);
        let lines = law.replace("\r\n", "\n").split('\n').count().max(1);
        required_reads.push(json!({"step_id":"read:site-law","ordinal":1,"required":true,"source":{"source_authority_ref":format!("site-law:{}",admission.pointer("/coordinate/site_ref").and_then(Value::as_str).unwrap_or("")),"artifact_ref":format!("site-file:{relative}"),"revision":revision},"tool":{"name":"agent_orientation_read","arguments":{"step_id":"read:site-law"}},"completion":{"kind":"tool_result_fields","expected_result":{"content_sha256":revision,"offset":1,"returned_lines":lines},"evidence_fields":["content_sha256","content_window_sha256","offset","returned_lines"]}}));
        entries.push(json!({"entry_id":"orientation:site-law","compartment":"law_and_constraints","entry_kind":"site_law","source_authority_ref":format!("site-law:{}",admission.pointer("/coordinate/site_ref").and_then(Value::as_str).unwrap_or("")),"artifact_ref":format!("site-file:{relative}"),"revision":revision,"observed_at":observed_at,"valid_until":null,"criticality":"required","projection_status":"available","revalidation_rule":"on_sha256_change","evidence_refs":[format!("sha256:{revision}")],"payload":{"site_relative_path":relative,"sha256":revision,"content_included":false,"read_required":true,"required_read_step_ids":["read:site-law"]},"rendered_text":format!("Applicable Site instructions: AGENTS.md (sha256 {revision})."),"subject":subject}));
    } else {
        entries.push(json!({"entry_id":"orientation:site-law","compartment":"law_and_constraints","entry_kind":"site_law","source_authority_ref":format!("site-law:{}",admission.pointer("/coordinate/site_ref").and_then(Value::as_str).unwrap_or("")),"artifact_ref":format!("site-file:{relative}"),"revision":"unavailable","observed_at":observed_at,"valid_until":null,"criticality":"required","projection_status":"unavailable","revalidation_rule":"before_orientation_delivery","evidence_refs":[],"payload":{"site_relative_path":relative,"missing":true},"rendered_text":null,"subject":subject}));
    }
    if let Some(checkpoint) = exact_checkpoint {
        if checkpoint["status"] == "ok" {
            let checkpoint_id = checkpoint["checkpoint_id"]
                .as_str()
                .ok_or("agent_context_exact_checkpoint_id_required")?;
            let portable = portable_continuation.cloned().unwrap_or_else(|| json!({}));
            let entry_id = format!("orientation:continuity:{checkpoint_id}");
            let material = json!({"schema":"narada.agent_context.orientation_continuity_material.v1","selection_posture":"selected_at_carrier_entry_not_live_state","historical_advisory_only":true,"checkpoint":checkpoint,"portable_continuation":portable,"authority_posture":{"continuity":"historical_context_only","consequential_action":"owning_admission_still_required"}});
            let rendered = format!(
                "{}\n",
                serde_json::to_string_pretty(&material).map_err(|e| e.to_string())?
            );
            let revision = sha256(&rendered);
            let step_id = format!("read:continuity:{}", &sha256(&entry_id)[..16]);
            let artifact_ref = format!(
                "orientation-manifest-entry:{}",
                entry_id.replace(':', "%3A")
            );
            let line_count = rendered.replace("\r\n", "\n").split('\n').count().max(1);
            required_reads.push(json!({"step_id":step_id,"ordinal":required_reads.len()+1,"required":true,"source":{"source_authority_ref":format!("agent-continuity:{}",admission.pointer("/coordinate/site_ref").and_then(Value::as_str).unwrap_or("")),"artifact_ref":artifact_ref,"revision":revision},"tool":{"name":"agent_orientation_read","arguments":{"step_id":step_id}},"completion":{"kind":"tool_result_fields","expected_result":{"content_sha256":revision,"offset":1,"returned_lines":line_count},"evidence_fields":["content_sha256","content_window_sha256","offset","returned_lines"]}}));
            let summary = continuity_summary(checkpoint);
            let mut evidence_refs = vec![
                json!(format!("checkpoint:{checkpoint_id}")),
                json!(format!("sha256:{revision}")),
            ];
            if let Some(hash) = portable.pointer("/artifact/sha256").and_then(Value::as_str) {
                evidence_refs.push(json!(format!("sha256:{hash}")))
            }
            entries.push(json!({"entry_id":entry_id,"compartment":"continuity","entry_kind":"exact_continuity","source_authority_ref":format!("agent-continuity:{}",admission.pointer("/coordinate/site_ref").and_then(Value::as_str).unwrap_or("")),"artifact_ref":format!("checkpoint:{checkpoint_id}"),"revision":revision,"observed_at":observed_at,"valid_until":null,"criticality":"required","projection_status":"available","revalidation_rule":"never_as_live_truth;verify_exact_hash_on_read","evidence_refs":evidence_refs,"payload":{"checkpoint":checkpoint,"portable_continuation":portable,"historical_advisory_only":true,"occupant_summary":summary,"inspection_call":null,"required_read_step_ids":[step_id]},"rendered_text":format!("Exact continuity checkpoint: {checkpoint_id}."),"subject":subject}));
        } else if let Some(checkpoint_id) = checkpoint["checkpoint_id"].as_str() {
            let unavailable = checkpoint["status"] == "checkpoint_not_found";
            entries.push(json!({"entry_id":format!("orientation:continuity:{checkpoint_id}"),"compartment":"continuity","entry_kind":"exact_continuity","source_authority_ref":format!("agent-continuity:{}",admission.pointer("/coordinate/site_ref").and_then(Value::as_str).unwrap_or("")),"artifact_ref":format!("checkpoint:{checkpoint_id}"),"revision":"unavailable","observed_at":observed_at,"valid_until":null,"criticality":"required","projection_status":if unavailable{"unavailable"}else{"incompatible"},"revalidation_rule":"resolve_exact_checkpoint_before_reassembly","evidence_refs":[],"payload":{"requested_checkpoint_id":checkpoint_id,"source_status":checkpoint["status"].as_str().unwrap_or("unknown"),"source_message":checkpoint["message"].as_str().unwrap_or(""),"historical_advisory_only":true},"rendered_text":null,"subject":subject}));
        }
    }
    entries.push(json!({"entry_id":"orientation:entry-procedure","compartment":"entry_procedure","entry_kind":"entry_procedure","source_authority_ref":"carrier-entry-procedure:agent-context","artifact_ref":"agent-context:orientation-entry-procedure","revision":"1","observed_at":observed_at,"valid_until":null,"criticality":"required","projection_status":"available","revalidation_rule":"on_entry_procedure_revision","evidence_refs":[receipt],"payload":{"required_reads":required_reads,"ordered_steps":[{"step":"complete_required_reads","effect":"read","required":true,"completion_evidence":"orientation_required_read_completion"},{"step":"inspect_named_live_authorities_before_work_mutation","effect":"read","required":true},{"step":"obtain_owner_specific_action_admission_before_consequence","effect":"separate_governed_crossing","required":true}],"self_referential_tool_call":false},"rendered_text":"Review this manifest, inspect live owners, and obtain separate admission before consequential action.","subject":subject}));
    let servers = mcp_servers(context);
    if !servers.is_empty() {
        let revision = sha256(&canonical_json(&Value::Array(servers.clone())));
        entries.push(json!({"entry_id":"orientation:capability-projection","compartment":"capability_projection","entry_kind":"mcp_capability_projection","source_authority_ref":format!("mcp-fabric:{}",admission.pointer("/coordinate/site_ref").and_then(Value::as_str).unwrap_or("")),"artifact_ref":format!("mcp-fabric:carrier-session:{}",admission.pointer("/coordinate/carrier_session_id").and_then(Value::as_str).unwrap_or("")),"revision":revision,"observed_at":observed_at,"valid_until":null,"criticality":"optional","projection_status":"available","revalidation_rule":"on_mcp_fabric_generation_or_runtime_posture_change","evidence_refs":[format!("sha256:{revision}")],"payload":{"servers":servers,"availability_only":true,"authority_granted":false},"rendered_text":format!("Projected MCP servers: {}.",servers.iter().filter_map(|v|v["name"].as_str()).collect::<Vec<_>>().join(", ")),"subject":subject}));
    }
    Ok(entries)
}

fn build_brief(manifest: &Value) -> Result<Value, String> {
    let entries = manifest["entries"]
        .as_array()
        .ok_or("agent_context_native_manifest_entries_invalid")?;
    let role = entries
        .iter()
        .find(|v| v["entry_kind"] == "role_binding" && v["projection_status"] == "available")
        .map(|v| {
            v.pointer("/payload/role_binding")
                .cloned()
                .unwrap_or(Value::Null)
        })
        .unwrap_or(Value::Null);
    let required = entries
        .iter()
        .find(|v| v["entry_kind"] == "entry_procedure" && v["projection_status"] == "available")
        .and_then(|v| v.pointer("/payload/required_reads"))
        .cloned()
        .ok_or("orientation_required_reads_missing")?;
    let selection = |compartment: &str, reason: &str| {
        entries.iter().find(|v|v["compartment"]==compartment&&v["projection_status"]=="available").map(|v|json!({"mode":"exact","source_authority_ref":v["source_authority_ref"],"artifact_ref":v["artifact_ref"],"revision":v["revision"],"reason_code":null,"summary":v.pointer("/payload/occupant_summary").cloned().unwrap_or_else(||json!({"label":v["rendered_text"]})),"inspection_call":v.pointer("/payload/inspection_call").cloned().unwrap_or(Value::Null)})).unwrap_or_else(||json!({"mode":"omitted","source_authority_ref":null,"artifact_ref":null,"revision":null,"reason_code":reason,"summary":null,"inspection_call":null}))
    };
    let encoded_manifest_id = manifest["manifest_id"]
        .as_str()
        .unwrap_or("")
        .replace(':', "%3A");
    let mut unsigned = json!({"schema":"narada.orientation_brief.v1","brief_id":format!("orientation-brief:{}",manifest["manifest_id"].as_str().unwrap_or("")),"generated_at":manifest["generated_at"],"coordinate":manifest["coordinate"],"admission_receipt_ref":manifest["admission_receipt_ref"],"agent_identity":manifest["agent_identity"],"carrier_kind":manifest["carrier_kind"],"readiness":manifest["readiness"],"entry_state":"orientation_required","action_admission":"separate_required","manifest_ref":{"source_authority_ref":"agent-context:orientation-manifest-store","artifact_ref":format!("narada-agent-context://orientation-manifest/{encoded_manifest_id}"),"revision":manifest["manifest_digest"],"manifest_id":manifest["manifest_id"],"manifest_digest":manifest["manifest_digest"]},"role_binding":role,"continuity_selection":selection("continuity","continuity_not_selected_at_entry"),"work_selection":selection("work_orientation","work_not_selected_at_entry"),"required_reads":required,"residual_codes":manifest["residuals"].as_array().into_iter().flatten().filter_map(|v|v["code"].as_str()).collect::<std::collections::BTreeSet<_>>(),"negative_claims":manifest["negative_claims"].as_array().into_iter().flatten().map(|v|v["statement"].clone()).collect::<Vec<_>>(),"max_inline_bytes":8192});
    let digest = sha256(&canonical_json(&unsigned));
    let obj = unsigned.as_object_mut().unwrap();
    obj.insert("brief_digest".into(), json!(digest));
    obj.insert("inline_bytes".into(), json!(1));
    stabilize_top_bytes(&mut unsigned, "inline_bytes")?;
    Ok(unsigned)
}

