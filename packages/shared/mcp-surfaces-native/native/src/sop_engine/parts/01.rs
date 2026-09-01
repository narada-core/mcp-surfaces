use jsonschema::validator_for;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Map, Value};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use uuid::Uuid;

use crate::sop_authority::{
    assert_bound, canonical_json, deterministic_id, diagnostic, fingerprint, get_handoff,
    hydrate_outbox_event, now_iso, open_db, optional_bounded_string, optional_string, parse_iso,
    public_handoff, required_string, row_json, transactional, MAX_INLINE_VALUE_BYTES,
    MAX_OUTBOX_PAYLOAD_BYTES, MAX_RUN_STATE_BYTES, MAX_TEMPLATE_DEFINITION_BYTES,
    SOP_TERMINAL_TOPIC,
};

const RUN_STATUSES: &[&str] = &[
    "pending",
    "running",
    "completed",
    "failed",
    "cancelled",
    "awaiting_confirmation",
];
const STEP_STATUSES: &[&str] = &["pending", "running", "completed", "failed", "skipped"];

#[derive(Clone)]
struct Run {
    run_id: String,
    sop_id: String,
    sop_version: i64,
    sop_title: String,
    status: String,
    occurrence_key: String,
    request_fingerprint: String,
    definition_fingerprint: String,
    definition: Value,
    input: Value,
    input_ref: Value,
    output: Value,
    output_ref: Value,
    step_states: Vec<Value>,
    trigger_source_kind: String,
    trigger_source_ref: String,
    triggered_by: String,
    parent_run_id: Option<String>,
    parent_step_id: Option<String>,
    created_at: String,
    updated_at: String,
    completed_at: Option<String>,
}

pub fn call_tool(name: &str, args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    match name {
        "sop_run_start" => run_start(args, root),
        "sop_run_refresh" => run_refresh(args, root),
        "sop_run_advance" => run_advance(args, root),
        "sop_handoff_retry" => handoff_retry(args, root),
        "sop_action_resolve" => action_resolve(args, root),
        "sop_run_cancel" => run_cancel(args, root),
        _ => Err(diagnostic(
            "unknown_tool",
            &format!("unknown_tool:{name}"),
            json!({"tool_name":name}),
        )),
    }
}

fn run_start(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    transactional(root, |db| {
        let (run, admission) = admit_run(db, args, None, None)?;
        reconcile_run_and_ancestors(db, &run.run_id)?;
        Ok(run_result(&get_run(db, &run.run_id)?, Some(admission)))
    })
}

fn run_refresh(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    transactional(root, |db| {
        let run_id = required_string(args.get("run_id"), "sop_requires_run_id", 512)?;
        let before = get_run(db, &run_id)?;
        reconcile_run_and_ancestors(db, &run_id)?;
        let after = get_run(db, &run_id)?;
        let mut result = run_result(&after, None);
        result.as_object_mut().expect("run result").insert(
            "explicit_reconciliation".to_string(),
            json!({"changed":before.updated_at != after.updated_at,"automatic_mode":true}),
        );
        Ok(result)
    })
}

fn run_advance(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    transactional(root, |db| {
        let handoff_id = required_string(args.get("handoff_id"), "sop_handoff_id_required", 512)?;
        let run_id = required_string(args.get("run_id"), "sop_requires_run_id", 512)?;
        let step_id = required_string(args.get("step_id"), "sop_requires_step_id", 512)?;
        let consumer_id = required_string(
            args.get("consumer_id"),
            "sop_handoff_consumer_id_required",
            512,
        )?;
        let lease_token = required_string(
            args.get("lease_token"),
            "sop_handoff_lease_token_required",
            512,
        )?;
        let completion_key = required_string(
            args.get("completion_key"),
            "sop_requires_completion_key",
            512,
        )?;
        let principal = required_string(args.get("principal"), "sop_requires_principal", 512)?;
        let outcome = required_string(args.get("outcome"), "sop_requires_outcome", 64)?;
        if !matches!(outcome.as_str(), "completed" | "failed") {
            return Err(diagnostic(
                "sop_outcome_invalid",
                &format!("sop_outcome_invalid:{outcome}"),
                json!({"allowed":["completed","failed"]}),
            ));
        }
        let result = args.get("result").cloned().unwrap_or_else(|| json!({}));
        assert_bound(&result, "sop_result", MAX_INLINE_VALUE_BYTES)?;
        if !result.is_object() {
            return Err(diagnostic(
                "sop_result_must_be_object",
                "sop_result_must_be_object",
                json!({}),
            ));
        }
        let result_ref = normalize_value_ref(args.get("result_ref"), "sop_result_ref")?;
        let error_message = optional_bounded_string(
            args.get("error_message"),
            "sop_error_message_too_long",
            4096,
        )?;
        if outcome == "failed" && error_message.is_none() {
            return Err(diagnostic(
                "sop_failed_outcome_requires_error_message",
                "sop_failed_outcome_requires_error_message",
                json!({}),
            ));
        }
        let completion_fingerprint = fingerprint(&json!({
            "completion_key":completion_key,"outcome":outcome,"principal":principal,
            "result":result,"result_ref":result_ref,"error_message":error_message
        }));
        let mut run = get_run(db, &run_id)?;
        let step_index = run
            .step_states
            .iter()
            .position(|step| step.get("step_id").and_then(Value::as_str) == Some(step_id.as_str()))
            .ok_or_else(|| {
                diagnostic(
                    "sop_step_not_found",
                    &format!("sop_step_not_found:{step_id}"),
                    json!({}),
                )
            })?;
        let target = run.step_states[step_index].clone();
        let executor = step_string(&target, "executor");
        if !matches!(executor.as_str(), "agent" | "operator") {
            return Err(diagnostic(
                "sop_step_not_manual_handoff",
                &format!("sop_step_not_manual_handoff:{step_id}"),
                json!({"executor":executor}),
            ));
        }
        let recorded_fingerprint = optional_string(target.get("completion_fingerprint"));
        if recorded_fingerprint.is_none() && is_run_terminal(&run.status) {
            return Err(diagnostic(
                "sop_run_terminal",
                &format!("sop_run_terminal:{run_id}"),
                json!({"status":run.status}),
            ));
        }
        if recorded_fingerprint.is_none()
            && target.get("status").and_then(Value::as_str) != Some("running")
        {
            return Err(diagnostic(
                "sop_step_not_running",
                &format!("sop_step_not_running:{step_id}"),
                json!({"status":target.get("status")}),
            ));
        }
        if outcome == "completed" {
            validate_schema(
                target.get("result_schema").filter(|value| !value.is_null()),
                &result,
                "sop_step_result_schema_mismatch",
                json!({"run_id":run_id,"step_id":step_id}),
            )?;
        }
        let (handoff, handoff_replayed) = complete_handoff(
            db,
            &handoff_id,
            &run_id,
            &step_id,
            &consumer_id,
            &lease_token,
            &completion_key,
            &outcome,
            &principal,
            &result,
            &result_ref,
            error_message.as_deref(),
        )?;
        if let Some(recorded_fingerprint) = recorded_fingerprint {
            if optional_string(target.get("completion_key")).as_deref()
                == Some(completion_key.as_str())
                && recorded_fingerprint == completion_fingerprint
                && handoff_replayed
            {
                let mut response = run_result(&run, None);
                let object = response.as_object_mut().expect("run response object");
                object.insert("handoff".to_string(), public_handoff(handoff, false));
                object.insert("completion_replayed".to_string(), json!(true));
                return Ok(response);
            }
            return Err(diagnostic(
                "sop_step_completion_conflict",
                &format!("sop_step_completion_conflict:{run_id}:{step_id}"),
                json!({
                    "recorded_completion_key":target.get("completion_key"),
                    "supplied_completion_key":completion_key
                }),
            ));
        }
        let completed_at = handoff
            .get("completed_at")
            .and_then(Value::as_str)
            .map(ToString::to_string)
            .unwrap_or_else(now_iso);
        let step = &mut run.step_states[step_index];
        set_step(step, "status", json!(outcome));
        set_step(step, "completed_at", json!(completed_at));
        set_step(step, "result", result.clone());
        set_step(step, "result_ref", result_ref.clone());
        set_step(step, "completion_key", json!(completion_key));
        set_step(
            step,
            "completion_fingerprint",
            json!(completion_fingerprint),
        );
        set_step(
            step,
            "error_message",
            if outcome == "failed" {
                json!(error_message)
            } else {
                Value::Null
            },
        );
        append_run_event(
            db,
            &run_id,
            Some(&step_id),
            if outcome == "completed" {
                "step_completed"
            } else {
                "step_failed"
            },
            json!({
                "handoff_id":handoff_id,"consumer_id":consumer_id,"principal":principal,
                "completion_key":completion_key,"result_ref":result_ref,
                "error_message":run.step_states[step_index].get("error_message")
            }),
        )?;
        persist_run(db, &mut run)?;
        reconcile_run_and_ancestors(db, &run_id)?;
        let mut response = run_result(&get_run(db, &run_id)?, None);
        let object = response.as_object_mut().expect("run response object");
        object.insert("handoff".to_string(), public_handoff(handoff, false));
        object.insert("completion_replayed".to_string(), json!(false));
        Ok(response)
    })
}

