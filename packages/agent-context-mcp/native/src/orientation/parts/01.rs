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
        "identity_state":{
            "schema":"narada.agent.identity_state.v1",
            "claimed_identity":{"identity":env::var("NARADA_CLAIMED_IDENTITY").ok().or_else(||env::var("NARADA_AGENT_ID").ok()),"status":if env::var_os("NARADA_CLAIMED_IDENTITY").is_some() || env::var_os("NARADA_AGENT_ID").is_some(){"claimed"}else{"unclaimed"},"source":if env::var_os("NARADA_CLAIMED_IDENTITY").is_some() || env::var_os("NARADA_AGENT_ID").is_some(){json!("carrier_environment")}else{Value::Null},"asserted_at":null,"evidence_refs":[],"authority_granted":false},
            "authentication":{"status":"missing","authenticated_identity":null,"evidence_refs":[]},
            "authority":{"status":"not_evaluated","operation":null,"granted":false,"evidence_refs":[]}
        },
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
    let explicit_claim = args.get("claimed_identity").and_then(|value| {
        value.as_str().map(str::to_string).or_else(|| value.get("identity").and_then(Value::as_str).map(str::to_string))
    });
    let has_receipt = args.get("admission_receipt").is_some()
        || env::var_os("NARADA_CARRIER_SESSION_ADMISSION_RECEIPT").is_some();
    if !has_receipt {
        let claimed = explicit_claim.clone().or_else(|| env::var("NARADA_CLAIMED_IDENTITY").ok()).or_else(|| env::var("NARADA_AGENT_ID").ok());
        return Ok(
            json!({"schema":"narada.agent_context.identity_resolution.v1","status":if claimed.is_some(){"claimed"}else{"blocked"},"identity":claimed.clone(),"canonical_agent_id":null,"confidence":if claimed.is_some(){json!("claimed_only")}else{Value::Null},"source":if claimed.is_some(){json!("carrier_environment")}else{Value::Null},"claimed_identity":{"identity":claimed.clone(),"status":if claimed.is_some(){"claimed"}else{"unclaimed"},"source":if env::var_os("NARADA_CLAIMED_IDENTITY").is_some() || env::var_os("NARADA_AGENT_ID").is_some(){json!("carrier_environment")}else{Value::Null},"asserted_at":null,"evidence_refs":[],"authority_granted":false},"authenticated_identity":null,"authentication":{"status":"missing","authenticated_identity":null,"evidence_refs":[]},"authority":{"status":"not_evaluated","operation":null,"granted":false,"evidence_refs":[]},"identity_state":{"schema":"narada.agent.identity_state.v1","claimed_identity":{"identity":claimed.clone(),"status":if claimed.is_some(){"claimed"}else{"unclaimed"},"source":if env::var_os("NARADA_CLAIMED_IDENTITY").is_some() || env::var_os("NARADA_AGENT_ID").is_some(){json!("carrier_environment")}else{Value::Null},"asserted_at":null,"evidence_refs":[],"authority_granted":false},"authentication":{"status":"missing","authenticated_identity":null,"evidence_refs":[]},"authority":{"status":"not_evaluated","operation":null,"granted":false,"evidence_refs":[]}},"reason":"agent_context_exact_admission_receipt_required","rejected_fallbacks":["latest_checkpoint","latest_start_event","identity_name_inference"]}),
        );
    }
    let admission = admission(context, args)?;
    let identity = admission
        .pointer("/agent_identity/local_agent_id")
        .and_then(Value::as_str)
        .unwrap_or("");
    let hint = args.get("hint").and_then(Value::as_str);
    let claimed_identity = explicit_claim.clone().unwrap_or_else(|| identity.to_string());
    if claimed_identity != identity {
        return Err("agent_context_claimed_identity_mismatch".into());
    }
    let claim_source = if explicit_claim.is_some() { "caller_assertion" } else { "carrier_session_admission_receipt" };
    Ok(
        json!({"schema":"narada.agent_context.identity_resolution.v1","status":"ok","identity":identity,"canonical_agent_id":admission.pointer("/agent_identity/canonical_agent_id").cloned().unwrap_or(Value::Null),"confidence":"exact","source":"carrier_session_admission_receipt","claimed_identity":{"identity":claimed_identity,"status":"claimed","source":claim_source,"asserted_at":null,"evidence_refs":[],"authority_granted":false},"authenticated_identity":identity,"authentication":{"status":"authenticated","authenticated_identity":identity,"evidence_refs":[admission["receipt_id"].clone(),admission["authority_readback_ref"].clone()]},"authority":{"status":"not_evaluated","operation":null,"granted":false,"evidence_refs":[]},"identity_state":{"schema":"narada.agent.identity_state.v1","claimed_identity":{"identity":claimed_identity,"status":"claimed","source":claim_source,"asserted_at":null,"evidence_refs":[],"authority_granted":false},"authentication":{"status":"authenticated","authenticated_identity":identity,"evidence_refs":[admission["receipt_id"].clone(),admission["authority_readback_ref"].clone()]},"authority":{"status":"not_evaluated","operation":null,"granted":false,"evidence_refs":[]}},"admission_receipt_ref":admission["receipt_id"],"carrier_session":admission["coordinate"],"authority_readback_ref":admission["authority_readback_ref"],"hint_match":hint.map(|v|v==identity||Some(v)==admission.pointer("/agent_identity/canonical_agent_id").and_then(Value::as_str))}),
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

