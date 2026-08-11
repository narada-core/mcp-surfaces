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

#[allow(clippy::too_many_arguments)]
fn complete_handoff(
    db: &Connection,
    handoff_id: &str,
    run_id: &str,
    step_id: &str,
    consumer_id: &str,
    lease_token: &str,
    completion_key: &str,
    outcome: &str,
    principal: &str,
    result: &Value,
    result_ref: &Value,
    error_message: Option<&str>,
) -> Result<(Value, bool), Value> {
    let handoff = get_handoff(db, handoff_id)?;
    if handoff.get("run_id").and_then(Value::as_str) != Some(run_id)
        || handoff.get("step_id").and_then(Value::as_str) != Some(step_id)
    {
        return Err(diagnostic(
            "sop_handoff_run_binding_mismatch",
            "sop_handoff_run_binding_mismatch",
            json!({"handoff_id":handoff_id,"run_id":run_id,"step_id":step_id}),
        ));
    }
    let completion_fingerprint = fingerprint(&json!({
        "completion_key":completion_key,"outcome":outcome,"principal":principal,
        "result":result,"result_ref":result_ref,"error_message":error_message
    }));
    if let Some(recorded_fingerprint) = handoff
        .get("completion_fingerprint")
        .and_then(Value::as_str)
    {
        if handoff.get("completion_key").and_then(Value::as_str) == Some(completion_key)
            && recorded_fingerprint == completion_fingerprint
        {
            return Ok((handoff, true));
        }
        return Err(diagnostic(
            "sop_handoff_completion_conflict",
            "sop_handoff_completion_conflict",
            json!({
                "handoff_id":handoff_id,
                "recorded_completion_key":handoff.get("completion_key"),
                "supplied_completion_key":completion_key
            }),
        ));
    }
    if handoff.get("status").and_then(Value::as_str) != Some("leased") {
        return Err(diagnostic(
            "sop_handoff_not_leased",
            "sop_handoff_not_leased",
            json!({"handoff_id":handoff_id,"status":handoff.get("status")}),
        ));
    }
    if handoff.get("lease_owner").and_then(Value::as_str) != Some(consumer_id)
        || handoff.get("lease_token").and_then(Value::as_str) != Some(lease_token)
    {
        return Err(diagnostic(
            "sop_handoff_lease_mismatch",
            "sop_handoff_lease_mismatch",
            json!({"handoff_id":handoff_id,"lease_owner":handoff.get("lease_owner")}),
        ));
    }
    let lease_expires_at = handoff
        .get("lease_expires_at")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let expired = match (parse_iso(lease_expires_at), parse_iso(&now_iso())) {
        (Some(expires), Some(now)) => expires <= now,
        _ => true,
    };
    if expired {
        return Err(diagnostic(
            "sop_handoff_lease_expired",
            "sop_handoff_lease_expired",
            json!({"handoff_id":handoff_id,"lease_expires_at":lease_expires_at}),
        ));
    }
    let completed_at = now_iso();
    db.execute(
        "UPDATE sop_handoffs SET status = ?, lease_owner = NULL, lease_token = NULL, lease_expires_at = NULL, completion_key = ?, completion_fingerprint = ?, principal = ?, result_json = ?, result_ref_json = ?, error_message = ?, updated_at = ?, completed_at = ? WHERE handoff_id = ?",
        params![outcome,completion_key,completion_fingerprint,principal,
            canonical_json(result),nullable_json(result_ref),error_message,
            completed_at,completed_at,handoff_id],
    )
    .map_err(|error| diagnostic("sop_handoff_update_failed", &error.to_string(), json!({})))?;
    Ok((get_handoff(db, handoff_id)?, false))
}

fn handoff_retry(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    transactional(root, |db| {
        let handoff_id = required_string(args.get("handoff_id"), "sop_handoff_id_required", 512)?;
        let principal = required_string(
            args.get("principal"),
            "sop_handoff_retry_principal_required",
            512,
        )?;
        let reason = required_string(
            args.get("reason"),
            "sop_handoff_retry_reason_required",
            4096,
        )?;
        let handoff = get_handoff(db, &handoff_id)?;
        let handoff_status = handoff
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let run_id = handoff
            .get("run_id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        if matches!(handoff_status, "pending" | "leased") {
            let mut response = run_result(&get_run(db, &run_id)?, None);
            let object = response.as_object_mut().expect("run response object");
            object.insert("handoff".to_string(), public_handoff(handoff, false));
            object.insert("retry_replayed".to_string(), json!(true));
            return Ok(response);
        }
        if handoff_status != "failed" {
            return Err(diagnostic(
                "sop_handoff_retry_requires_failed",
                &format!("sop_handoff_retry_requires_failed:{handoff_id}"),
                json!({"status":handoff_status}),
            ));
        }
        if handoff.get("executor").and_then(Value::as_str) != Some("agent") {
            return Err(diagnostic(
                "sop_handoff_retry_agent_only",
                &format!("sop_handoff_retry_agent_only:{handoff_id}"),
                json!({"executor":handoff.get("executor")}),
            ));
        }
        let mut run = get_run(db, &run_id)?;
        let step_id = handoff
            .get("step_id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let step_index = run
            .step_states
            .iter()
            .position(|step| step.get("step_id").and_then(Value::as_str) == Some(step_id.as_str()));
        let valid_step = step_index.is_some_and(|index| {
            let step = &run.step_states[index];
            step.get("executor").and_then(Value::as_str) == Some("agent")
                && step.get("status").and_then(Value::as_str) == Some("failed")
                && step
                    .get("completion_fingerprint")
                    .is_some_and(|value| !value.is_null())
        });
        if !valid_step {
            return Err(diagnostic(
                "sop_handoff_retry_state_conflict",
                &format!("sop_handoff_retry_state_conflict:{handoff_id}"),
                json!({
                    "run_id":run_id,"step_id":step_id,"run_status":run.status,
                    "step_status":step_index.and_then(|index|run.step_states[index].get("status")).cloned()
                }),
            ));
        }
        let step_index = step_index.expect("validated retry step");
        if run.step_states[step_index].get("completion_fingerprint")
            != handoff.get("completion_fingerprint")
        {
            return Err(diagnostic(
                "sop_handoff_retry_completion_conflict",
                &format!("sop_handoff_retry_completion_conflict:{handoff_id}"),
                json!({"run_id":run_id,"step_id":step_id}),
            ));
        }
        let reopened_event_id = match reopen_terminal_outbox_for_retry(db, &run_id) {
            Ok(value) => value,
            Err(error)
                if error.get("code").and_then(Value::as_str)
                    == Some("sop_outbox_retry_requires_new_run") =>
            {
                return retry_failed_handoff_as_new_run(
                    db, &handoff, &run, &principal, &reason, &error,
                );
            }
            Err(error) => return Err(error),
        };
        let now = now_iso();
        let reset_step_ids = reset_retryable_dependent_steps(&mut run, &step_id);
        let retry_marker = format!("worker_retryable:reopened:{reason}")
            .chars()
            .take(4096)
            .collect::<String>();
        db.execute(
            "UPDATE sop_handoffs SET status = 'pending', lease_owner = NULL, lease_token = NULL, lease_expires_at = NULL, completion_key = NULL, completion_fingerprint = NULL, principal = NULL, result_json = '{}', result_ref_json = NULL, error_message = NULL, last_error = ?, updated_at = ?, completed_at = NULL WHERE handoff_id = ? AND status = 'failed'",
            params![retry_marker,now,handoff_id],
        )
        .map_err(|error| diagnostic("sop_handoff_update_failed", &error.to_string(), json!({})))?;
        let step = &mut run.step_states[step_index];
        set_step(step, "status", json!("running"));
        set_step(step, "started_at", json!(now));
        set_step(step, "completed_at", Value::Null);
        set_step(
            step,
            "result",
            json!({
                "handoff_id":handoff.get("handoff_id"),
                "handoff_occurrence_key":handoff.get("occurrence_key")
            }),
        );
        set_step(step, "result_ref", Value::Null);
        set_step(step, "completion_key", Value::Null);
        set_step(step, "completion_fingerprint", Value::Null);
        set_step(step, "error_message", Value::Null);
        run.status = "awaiting_confirmation".to_string();
        run.output = json!({});
        run.output_ref = Value::Null;
        run.completed_at = None;
        persist_run(db, &mut run)?;
        append_run_event(
            db,
            &run_id,
            Some(&step_id),
            "handoff_reopened",
            json!({
                "handoff_id":handoff_id,"principal":principal,"reason":reason,
                "retry_marker":retry_marker,"reset_step_ids":reset_step_ids,
                "reopened_outbox_event_id":reopened_event_id
            }),
        )?;
        reconcile_run_and_ancestors(db, &run_id)?;
        let mut response = run_result(&get_run(db, &run_id)?, None);
        let object = response.as_object_mut().expect("run response object");
        object.insert(
            "handoff".to_string(),
            public_handoff(get_handoff(db, &handoff_id)?, false),
        );
        object.insert("retry_replayed".to_string(), json!(false));
        Ok(response)
    })
}

fn reopen_terminal_outbox_for_retry(
    db: &Connection,
    run_id: &str,
) -> Result<Option<String>, Value> {
    let existing = db
        .query_row(
            "SELECT event_id, compacted_at FROM sop_outbox WHERE run_id = ?",
            params![run_id],
            row_json,
        )
        .optional()
        .map_err(|error| diagnostic("sop_outbox_query_failed", &error.to_string(), json!({})))?;
    let Some(existing) = existing else {
        return Ok(None);
    };
    let event_id = required_string(existing.get("event_id"), "sop_outbox_event_id_invalid", 512)?;
    let consumed = db
        .query_row(
            "SELECT 1 FROM sop_outbox_receipts WHERE event_id = ? LIMIT 1",
            params![event_id],
            |_| Ok(true),
        )
        .optional()
        .map_err(|error| {
            diagnostic(
                "sop_outbox_receipt_query_failed",
                &error.to_string(),
                json!({}),
            )
        })?
        .unwrap_or(false);
    let compacted = existing
        .get("compacted_at")
        .is_some_and(|value| !value.is_null());
    if consumed || compacted {
        return Err(diagnostic(
            "sop_outbox_retry_requires_new_run",
            "sop_outbox_retry_requires_new_run",
            json!({
                "event_id":event_id,"run_id":run_id,"consumed":consumed,"compacted":compacted
            }),
        ));
    }
    db.execute(
        "DELETE FROM sop_outbox WHERE event_id = ? AND run_id = ?",
        params![event_id, run_id],
    )
    .map_err(|error| diagnostic("sop_outbox_delete_failed", &error.to_string(), json!({})))?;
    Ok(Some(event_id))
}

fn reset_retryable_dependent_steps(run: &mut Run, root_step_id: &str) -> Vec<String> {
    let mut reset = HashSet::from([root_step_id.to_string()]);
    let mut changed = true;
    while changed {
        changed = false;
        for step in &mut run.step_states {
            let step_id = step_string(step, "step_id");
            if reset.contains(&step_id)
                || step.get("status").and_then(Value::as_str) != Some("failed")
                || !step
                    .get("error_message")
                    .and_then(Value::as_str)
                    .is_some_and(|message| message.starts_with("failed_dependency:"))
            {
                continue;
            }
            let depends_on_reset = step
                .get("depends_on")
                .and_then(Value::as_array)
                .is_some_and(|dependencies| {
                    dependencies.iter().any(|dependency| {
                        dependency
                            .as_str()
                            .is_some_and(|dependency| reset.contains(dependency))
                    })
                });
            if !depends_on_reset {
                continue;
            }
            set_step(step, "status", json!("pending"));
            for key in [
                "started_at",
                "completed_at",
                "result_ref",
                "completion_key",
                "completion_fingerprint",
                "error_message",
                "child_run_id",
                "action_id",
                "pinned_child_definition_fingerprint",
            ] {
                set_step(step, key, Value::Null);
            }
            set_step(step, "result", json!({}));
            reset.insert(step_id);
            changed = true;
        }
    }
    let mut dependent = reset
        .into_iter()
        .filter(|step_id| step_id != root_step_id)
        .collect::<Vec<_>>();
    dependent.sort();
    dependent
}

fn retry_failed_handoff_as_new_run(
    db: &Connection,
    handoff: &Value,
    run: &Run,
    principal: &str,
    reason: &str,
    outbox_diagnostic: &Value,
) -> Result<Value, Value> {
    let handoff_id = handoff
        .get("handoff_id")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let step_id = handoff
        .get("step_id")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let occurrence_key = deterministic_id("sop_retry_", handoff_id);
    let args = json!({
        "sop_id":run.sop_id,"sop_version":run.sop_version,
        "occurrence_key":occurrence_key,"input":run.input,"input_ref":run.input_ref,
        "trigger_source_kind":"manual",
        "trigger_source_ref":format!("sop_handoff_retry:{handoff_id}"),
        "triggered_by":"sop-handoff-retry"
    });
    let (admitted, admission) = admit_run(
        db,
        args.as_object().expect("retry admission object"),
        None,
        None,
    )?;
    reconcile_run_and_ancestors(db, &admitted.run_id)?;
    let retry_run = get_run(db, &admitted.run_id)?;
    let retry_handoff_id = db
        .query_row(
            "SELECT handoff_id FROM sop_handoffs WHERE run_id = ? AND step_id = ?",
            params![retry_run.run_id, step_id],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(|error| diagnostic("sop_handoff_query_failed", &error.to_string(), json!({})))?;
    let retry_handoff = retry_handoff_id
        .as_deref()
        .map(|id| get_handoff(db, id))
        .transpose()?;
    if admission == "created" {
        append_run_event(
            db,
            &run.run_id,
            Some(step_id),
            "handoff_retry_spawned",
            json!({
                "handoff_id":handoff_id,"principal":principal,"reason":reason,
                "retry_run_id":retry_run.run_id,
                "retry_handoff_id":retry_handoff.as_ref().and_then(|value|value.get("handoff_id")),
                "retry_occurrence_key":occurrence_key,
                "original_outbox_event_id":outbox_diagnostic.get("details").and_then(|value|value.get("event_id")),
                "original_outbox_preserved":true
            }),
        )?;
    }
    let mut response = run_result(&retry_run, Some(admission));
    let object = response.as_object_mut().expect("run response object");
    object.insert(
        "handoff".to_string(),
        retry_handoff
            .map(|value| public_handoff(value, false))
            .unwrap_or(Value::Null),
    );
    object.insert("retry_replayed".to_string(), json!(admission == "replayed"));
    object.insert("retry_mode".to_string(), json!("new_run"));
    object.insert("retry_of_run_id".to_string(), json!(run.run_id));
    object.insert("retry_of_handoff_id".to_string(), json!(handoff_id));
    object.insert("retry_reason".to_string(), json!(reason));
    object.insert("original_outbox_preserved".to_string(), json!(true));
    Ok(response)
}

fn action_resolve(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let action_id = required_string(args.get("action_id"), "sop_requires_action_id", 512)?;
    let completion_key = required_string(
        args.get("completion_key"),
        "sop_requires_completion_key",
        512,
    )?;
    let outcome = required_string(args.get("outcome"), "sop_requires_outcome", 64)?;
    if !matches!(outcome.as_str(), "completed" | "failed") {
        return Err(diagnostic(
            "sop_outcome_invalid",
            &format!("sop_outcome_invalid:{outcome}"),
            json!({"allowed":["completed","failed"]}),
        ));
    }
    let operation_ref = required_string(
        args.get("operation_ref"),
        "sop_requires_operation_ref",
        2048,
    )?;
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
        "completion_key":completion_key,"outcome":outcome,"operation_ref":operation_ref,
        "result":result,"result_ref":result_ref,"error_message":error_message
    }));
    let receipt = transactional(root, |db| {
        let existing = get_action(db, &action_id)?;
        let run_id = existing
            .get("run_id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        if let Some(recorded_fingerprint) = existing
            .get("completion_fingerprint")
            .and_then(Value::as_str)
        {
            if existing.get("completion_key").and_then(Value::as_str)
                == Some(completion_key.as_str())
                && recorded_fingerprint == completion_fingerprint
            {
                let run_status = db
                    .query_row(
                        "SELECT status FROM sop_runs WHERE run_id = ?",
                        params![run_id],
                        |row| row.get::<_, String>(0),
                    )
                    .optional()
                    .map_err(|error| {
                        diagnostic("sop_run_query_failed", &error.to_string(), json!({}))
                    })?
                    .unwrap_or_default();
                return Ok(json!({
                    "run_id":run_id,"completion_replayed":true,
                    "late_cancellation_acknowledgement":run_status=="cancelled"
                }));
            }
            return Err(diagnostic(
                "sop_action_completion_conflict",
                &format!("sop_action_completion_conflict:{action_id}"),
                json!({
                    "recorded_completion_key":existing.get("completion_key"),
                    "supplied_completion_key":completion_key
                }),
            ));
        }
        let run_status = db
            .query_row(
                "SELECT status FROM sop_runs WHERE run_id = ?",
                params![run_id],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(|error| diagnostic("sop_run_query_failed", &error.to_string(), json!({})))?
            .ok_or_else(|| {
                diagnostic(
                    "sop_run_not_found",
                    &format!("sop_run_not_found:{run_id}"),
                    json!({}),
                )
            })?;
        let late_cancellation = existing.get("status").and_then(Value::as_str) == Some("cancelled")
            && run_status == "cancelled";
        if existing.get("status").and_then(Value::as_str) != Some("pending") && !late_cancellation {
            return Err(diagnostic(
                "sop_action_not_pending",
                &format!("sop_action_not_pending:{action_id}"),
                json!({"status":existing.get("status")}),
            ));
        }
        let now = now_iso();
        db.execute(
            "UPDATE sop_actions SET status = ?, completion_key = ?, completion_fingerprint = ?, operation_ref = ?, result_json = ?, result_ref_json = ?, error_message = ?, updated_at = ?, completed_at = ? WHERE action_id = ?",
            params![outcome,completion_key,completion_fingerprint,operation_ref,
                canonical_json(&result),nullable_json(&result_ref),
                if outcome=="failed"{error_message.as_deref()}else{None},
                now,now,action_id],
        )
        .map_err(|error| diagnostic("sop_action_update_failed", &error.to_string(), json!({})))?;
        let step_id = existing
            .get("step_id")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let event_kind = match (late_cancellation, outcome.as_str()) {
            (true, "completed") => "action_completed_after_cancellation",
            (true, _) => "action_failed_after_cancellation",
            (false, "completed") => "action_completed",
            (false, _) => "action_failed",
        };
        append_run_event(
            db,
            &run_id,
            Some(step_id),
            event_kind,
            json!({
                "action_id":action_id,"completion_key":completion_key,
                "operation_ref":operation_ref,"result_ref":result_ref,
                "error_message":error_message
            }),
        )?;
        get_action(db, &action_id)?;
        Ok(json!({
            "run_id":run_id,"completion_replayed":false,
            "late_cancellation_acknowledgement":late_cancellation
        }))
    })?;
    let run_id = receipt
        .get("run_id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let late_cancellation = receipt
        .get("late_cancellation_acknowledgement")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let reconciliation_error = if late_cancellation {
        None
    } else {
        transactional(root, |db| {
            reconcile_run_and_ancestors(db, &run_id)?;
            Ok(Value::Null)
        })
        .err()
    };
    let db = open_db(root)?;
    let mut response = get_action(&db, &action_id)?;
    let object = response.as_object_mut().expect("action response object");
    object.insert(
        "completion_replayed".to_string(),
        receipt
            .get("completion_replayed")
            .cloned()
            .unwrap_or(json!(false)),
    );
    object.insert(
        "late_cancellation_acknowledgement".to_string(),
        json!(late_cancellation),
    );
    object.insert(
        "reconciliation".to_string(),
        match reconciliation_error {
            Some(error) => json!({"status":"failed","diagnostic":error}),
            None => json!({"status":"completed"}),
        },
    );
    object.insert("run".to_string(), action_resolution_run_view(&db, &run_id));
    Ok(response)
}

fn run_cancel(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    transactional(root, |db| {
        let run_id = required_string(args.get("run_id"), "sop_requires_run_id", 512)?;
        let run = get_run(db, &run_id)?;
        if run.status == "cancelled" {
            let mut response = run_result(&run, None);
            response
                .as_object_mut()
                .expect("run response object")
                .insert("cancellation_replayed".to_string(), json!(true));
            return Ok(response);
        }
        if matches!(run.status.as_str(), "completed" | "failed") {
            return Err(diagnostic(
                "sop_run_already_terminal",
                &format!("sop_run_already_terminal:{run_id}"),
                json!({"status":run.status}),
            ));
        }
        let reason =
            optional_bounded_string(args.get("reason"), "sop_cancellation_reason_too_long", 4096)?
                .unwrap_or_else(|| "cancelled_by_caller".to_string());
        cancel_run_internal(db, &run_id, &reason, &mut HashSet::new())?;
        reconcile_run_and_ancestors(db, &run_id)?;
        let mut response = run_result(&get_run(db, &run_id)?, None);
        response
            .as_object_mut()
            .expect("run response object")
            .insert("cancellation_replayed".to_string(), json!(false));
        Ok(response)
    })
}

fn cancel_run_internal(
    db: &Connection,
    run_id: &str,
    reason: &str,
    seen: &mut HashSet<String>,
) -> Result<(), Value> {
    if !seen.insert(run_id.to_string()) {
        return Ok(());
    }
    let mut run = get_run(db, run_id)?;
    if is_run_terminal(&run.status) {
        return Ok(());
    }
    let child_ids = run
        .step_states
        .iter()
        .filter_map(|step| {
            step.get("child_run_id")
                .and_then(Value::as_str)
                .map(ToString::to_string)
        })
        .collect::<Vec<_>>();
    for child_id in child_ids {
        cancel_run_internal(db, &child_id, &format!("parent_cancelled:{run_id}"), seen)?;
    }
    for step in &mut run.step_states {
        if matches!(
            step.get("status").and_then(Value::as_str),
            Some("pending" | "running")
        ) {
            skip_step(step, &format!("run_cancelled:{reason}"));
        }
    }
    let now = now_iso();
    let cancellation_error = format!("run_cancelled:{reason}");
    db.execute(
        "UPDATE sop_actions SET status = 'cancelled', error_message = ?, updated_at = ?, completed_at = ? WHERE run_id = ? AND status = 'pending'",
        params![cancellation_error,now,now,run_id],
    )
    .map_err(|error| diagnostic("sop_action_update_failed", &error.to_string(), json!({})))?;
    db.execute(
        "UPDATE sop_handoffs SET status = 'cancelled', lease_owner = NULL, lease_token = NULL, lease_expires_at = NULL, last_error = ?, updated_at = ?, completed_at = ? WHERE run_id = ? AND status IN ('pending','leased')",
        params![cancellation_error,now,now,run_id],
    )
    .map_err(|error| diagnostic("sop_handoff_update_failed", &error.to_string(), json!({})))?;
    run.status = "cancelled".to_string();
    run.completed_at = Some(now);
    run.output = json!({});
    run.output_ref = Value::Null;
    persist_run(db, &mut run)?;
    append_run_event(db, run_id, None, "run_cancelled", json!({"reason":reason}))?;
    put_terminal_outbox(db, &run)?;
    Ok(())
}

fn admit_run(
    db: &Connection,
    args: &Map<String, Value>,
    parent_run_id: Option<&str>,
    parent_step_id: Option<&str>,
) -> Result<(Run, &'static str), Value> {
    let sop_id = required_string(args.get("sop_id"), "sop_requires_sop_id", 256)?;
    let occurrence_key = required_string(
        args.get("occurrence_key"),
        "sop_requires_occurrence_key",
        512,
    )?;
    let triggered_by = required_string(args.get("triggered_by"), "sop_requires_triggered_by", 512)?;
    let trigger_source_kind = match args.get("trigger_source_kind") {
        None | Some(Value::Null) => "manual".to_string(),
        value => required_string(value, "sop_requires_trigger_source_kind", 128)?,
    };
    let trigger_source_ref = optional_string(args.get("trigger_source_ref")).unwrap_or_default();
    if trigger_source_ref.chars().count() > 2048 {
        return Err(diagnostic(
            "sop_trigger_source_ref_too_long",
            "sop_trigger_source_ref_too_long",
            json!({"max_length":2048}),
        ));
    }
    let input = args.get("input").cloned().unwrap_or_else(|| json!({}));
    assert_bound(&input, "sop_input", MAX_INLINE_VALUE_BYTES)?;
    if !input.is_object() {
        return Err(diagnostic(
            "sop_input_must_be_object",
            "sop_input_must_be_object",
            json!({}),
        ));
    }
    let input_ref = normalize_value_ref(args.get("input_ref"), "sop_input_ref")?;
    let existing = db
        .query_row(
            "SELECT * FROM sop_runs WHERE sop_id = ? AND occurrence_key = ?",
            params![sop_id, occurrence_key],
            row_json,
        )
        .optional()
        .map_err(|error| diagnostic("sop_run_query_failed", &error.to_string(), json!({})))?;
    let version = match args.get("sop_version") {
        Some(value) if !value.is_null() => {
            let version = value.as_i64().ok_or_else(|| {
                diagnostic(
                    "sop_invalid_version",
                    &format!("sop_invalid_version:{value}"),
                    json!({}),
                )
            })?;
            if version < 1 {
                return Err(diagnostic(
                    "sop_invalid_version",
                    &format!("sop_invalid_version:{version}"),
                    json!({}),
                ));
            }
            version
        }
        _ => match existing
            .as_ref()
            .and_then(|value| value.get("sop_version"))
            .and_then(Value::as_i64)
        {
            Some(version) => version,
            None => latest_runnable_template_version(db, &sop_id)?,
        },
    };
    let template = template_by_version(db, &sop_id, version)?;
    assert_no_legacy_effects(&template)?;
    validate_schema(
        template
            .get("input_schema")
            .filter(|value| !value.is_null()),
        &input,
        "sop_input_schema_mismatch",
        json!({"sop_id":sop_id,"sop_version":version}),
    )?;
    let admission_request = json!({
        "sop_id":sop_id,"sop_version":version,"occurrence_key":occurrence_key,
        "input":input,"input_ref":input_ref,"trigger_source_kind":trigger_source_kind,
        "trigger_source_ref":trigger_source_ref,"triggered_by":triggered_by,
        "parent_run_id":parent_run_id,"parent_step_id":parent_step_id
    });
    let request_fingerprint = fingerprint(&admission_request);
    if let Some(existing) = existing {
        let existing = hydrate_run(existing)?;
        if existing.request_fingerprint != request_fingerprint {
            return Err(diagnostic(
                "sop_occurrence_conflict",
                &format!("sop_occurrence_conflict:{sop_id}:{occurrence_key}"),
                json!({
                    "occurrence_key":occurrence_key,
                    "recorded_request_fingerprint":existing.request_fingerprint,
                    "supplied_request_fingerprint":request_fingerprint,
                    "recorded_sop_version":existing.sop_version,"supplied_sop_version":version
                }),
            ));
        }
        return Ok((existing, "replayed"));
    }
    if let (Some(parent_run_id), Some(_)) = (parent_run_id, parent_step_id) {
        assert_no_recursive_child(db, parent_run_id, &sop_id, version)?;
    }
    let definition = executable_definition(&template);
    assert_bound(&definition, "sop_definition", MAX_TEMPLATE_DEFINITION_BYTES)?;
    let definition_fingerprint = fingerprint(&definition);
    let step_states = initialize_step_states(db, &template)?;
    let step_states_value = Value::Array(step_states.clone());
    assert_bound(&step_states_value, "sop_run_state", MAX_RUN_STATE_BYTES)?;
    let run_id = format!(
        "sop_run_{}_{}",
        now_iso()
            .replace(['-', ':', '.'], "")
            .chars()
            .take(15)
            .collect::<String>(),
        &Uuid::new_v4().to_string()[..8]
    );
    let now = now_iso();
    let title = template
        .get("title")
        .and_then(Value::as_str)
        .unwrap_or_default();
    db.execute(
        "INSERT INTO sop_runs (run_id, sop_id, sop_version, sop_title, status, occurrence_key, request_fingerprint, definition_fingerprint, definition_json, input_json, input_ref_json, output_json, output_ref_json, step_states_json, trigger_source_kind, trigger_source_ref, triggered_by, parent_run_id, parent_step_id, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        params![
            run_id,sop_id,version,title,"pending",occurrence_key,request_fingerprint,
            definition_fingerprint,canonical_json(&definition),canonical_json(&input),
            nullable_json(&input_ref),"{}",Option::<String>::None,
            canonical_json(&step_states_value),trigger_source_kind,trigger_source_ref,
            triggered_by,parent_run_id,parent_step_id,now,now
        ],
    )
    .map_err(|error| diagnostic("sop_run_insert_failed", &error.to_string(), json!({})))?;
    append_run_event(
        db,
        &run_id,
        None,
        "run_admitted",
        json!({
            "sop_id":sop_id,"sop_version":version,"occurrence_key":occurrence_key,
            "request_fingerprint":request_fingerprint,"definition_fingerprint":definition_fingerprint,
            "triggered_by":triggered_by,"parent_run_id":parent_run_id,"parent_step_id":parent_step_id
        }),
    )?;
    Ok((get_run(db, &run_id)?, "created"))
}

fn latest_runnable_template_version(db: &Connection, sop_id: &str) -> Result<i64, Value> {
    let version = db
        .query_row(
            "SELECT COALESCE(MAX(version), 0) FROM sop_templates WHERE sop_id = ? AND status != 'deprecated'",
            params![sop_id],
            |row| row.get::<_, i64>(0),
        )
        .map_err(|error| diagnostic("sop_template_query_failed", &error.to_string(), json!({})))?;
    if version < 1 {
        return Err(diagnostic(
            "sop_no_active_version",
            &format!("sop_no_active_version:{sop_id}"),
            json!({}),
        ));
    }
    Ok(version)
}

fn template_by_version(db: &Connection, sop_id: &str, version: i64) -> Result<Value, Value> {
    let row = db
        .query_row(
            "SELECT * FROM sop_templates WHERE sop_id = ? AND version = ?",
            params![sop_id, version],
            row_json,
        )
        .optional()
        .map_err(|error| diagnostic("sop_template_query_failed", &error.to_string(), json!({})))?
        .ok_or_else(|| {
            diagnostic(
                "sop_not_found",
                &format!("sop_not_found:{sop_id}@v{version}"),
                json!({}),
            )
        })?;
    hydrate_template(row)
}

fn hydrate_template(row: Value) -> Result<Value, Value> {
    let object = row
        .as_object()
        .ok_or_else(|| diagnostic("sop_template_corrupt", "sop_template_corrupt", json!({})))?;
    let raw_steps = object
        .get("steps_json")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            diagnostic(
                "sop_template_corrupt",
                "sop_template_corrupt",
                json!({"field":"steps_json"}),
            )
        })?;
    let steps = raw_steps
        .iter()
        .map(normalize_persisted_step)
        .collect::<Result<Vec<_>, _>>()?;
    validate_step_graph(&steps)?;
    Ok(json!({
        "sop_id":object.get("sop_id").cloned().unwrap_or(Value::Null),
        "version":object.get("version").cloned().unwrap_or(Value::Null),
        "title":object.get("title").cloned().unwrap_or(Value::Null),
        "status":object.get("status").cloned().unwrap_or(Value::Null),
        "description":object.get("description").cloned().unwrap_or_else(||json!("")),
        "steps":steps,
        "trigger_kind":object.get("trigger_kind").cloned().unwrap_or_else(||json!("manual")),
        "input_schema":object.get("input_schema_json").cloned().unwrap_or(Value::Null),
        "output":object.get("output_mapping_json").cloned().unwrap_or(Value::Null),
        "output_ref":object.get("output_ref_mapping_json").cloned().unwrap_or(Value::Null),
        "output_schema":object.get("output_schema_json").cloned().unwrap_or(Value::Null),
        "acceptance_criteria":object.get("acceptance_criteria_json").cloned().unwrap_or_else(||json!([])),
        "evidence_requirements":object.get("evidence_requirements_json").cloned().unwrap_or_else(||json!([])),
        "created_at":object.get("created_at").cloned().unwrap_or(Value::Null),
        "updated_at":object.get("updated_at").cloned().unwrap_or(Value::Null)
    }))
}

fn normalize_persisted_step(value: &Value) -> Result<Value, Value> {
    let object = value.as_object().ok_or_else(|| {
        diagnostic(
            "sop_persisted_step_invalid",
            "sop_persisted_step_invalid",
            json!({}),
        )
    })?;
    let executor = object
        .get("executor")
        .and_then(Value::as_str)
        .or_else(|| match object.get("kind").and_then(Value::as_str) {
            Some("manual") => Some("operator"),
            Some("note") => Some("engine"),
            value => value,
        })
        .unwrap_or_default();
    if !matches!(executor, "engine" | "agent" | "operator" | "sop" | "action") {
        return Err(diagnostic(
            "sop_persisted_step_executor_invalid",
            &format!("sop_persisted_step_executor_invalid:{executor}"),
            json!({"executor":executor,"step_id":object.get("id")}),
        ));
    }
    let id = required_string(object.get("id"), "sop_step_requires_id", 128)?;
    let title = required_string(object.get("title"), "sop_step_requires_title", 512)?;
    let instructions = required_string(
        object.get("instructions"),
        "sop_step_requires_instructions",
        16 * 1024,
    )?;
    let depends_on = object
        .get("depends_on")
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .map(ToString::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let sop_version = object.get("sop_version").and_then(Value::as_i64);
    Ok(json!({
        "id":id,"executor":executor,"blocking":matches!(executor,"agent"|"operator"),
        "title":title,"depends_on":depends_on,"instructions":instructions,
        "when":object.get("when").cloned().unwrap_or(Value::Null),
        "input":object.get("input").cloned().unwrap_or(Value::Null),
        "input_ref":object.get("input_ref").cloned().unwrap_or(Value::Null),
        "result_schema":object.get("result_schema").cloned().unwrap_or(Value::Null),
        "action":object.get("action").cloned().unwrap_or(Value::Null),
        "sop_id":object.get("sop_id").cloned().unwrap_or(Value::Null),
        "sop_version":sop_version,"wait_policy":object.get("wait_policy").cloned().unwrap_or_else(||if executor=="sop"{json!("wait")}else{Value::Null}),
        "legacy_command":object.get("command").cloned().unwrap_or(Value::Null)
    }))
}

fn executable_definition(template: &Value) -> Value {
    json!({
        "schema":"narada.sop.definition.v2",
        "sop_id":template.get("sop_id"),"version":template.get("version"),
        "title":template.get("title"),"steps":template.get("steps"),
        "input_schema":template.get("input_schema"),"output":template.get("output"),
        "output_ref":template.get("output_ref"),"output_schema":template.get("output_schema"),
        "acceptance_criteria":template.get("acceptance_criteria"),
        "evidence_requirements":template.get("evidence_requirements")
    })
}

fn initialize_step_states(db: &Connection, template: &Value) -> Result<Vec<Value>, Value> {
    let steps = template
        .get("steps")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            diagnostic(
                "sop_definition_steps_invalid",
                "sop_definition_steps_invalid",
                json!({}),
            )
        })?;
    let mut output = Vec::with_capacity(steps.len());
    for step in steps {
        let executor = step
            .get("executor")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let mut pinned_version = step.get("sop_version").and_then(Value::as_i64);
        let mut pinned_fingerprint = None;
        if executor == "sop" {
            let child_sop_id =
                required_string(step.get("sop_id"), "sop_step_requires_child_sop_id", 256)?;
            let version = match pinned_version {
                Some(version) => version,
                None => latest_runnable_template_version(db, &child_sop_id)?,
            };
            let child = template_by_version(db, &child_sop_id, version)?;
            assert_no_legacy_effects(&child)?;
            pinned_version = Some(version);
            pinned_fingerprint = Some(fingerprint(&executable_definition(&child)));
        }
        output.push(json!({
            "step_id":step.get("id"),"executor":executor,"blocking":step.get("blocking"),
            "title":step.get("title"),"status":"pending","depends_on":step.get("depends_on"),
            "instructions":step.get("instructions"),"when":step.get("when"),
            "input":step.get("input"),"input_ref":step.get("input_ref"),
            "result_schema":step.get("result_schema"),"action":step.get("action"),
            "sop_id":step.get("sop_id"),"sop_version":pinned_version,
            "wait_policy":step.get("wait_policy"),
            "pinned_child_definition_fingerprint":pinned_fingerprint,
            "child_run_id":null,"action_id":null,"started_at":null,"completed_at":null,
            "result":{},"result_ref":null,"completion_key":null,
            "completion_fingerprint":null,"error_message":null
        }));
    }
    Ok(output)
}

fn assert_no_legacy_effects(template: &Value) -> Result<(), Value> {
    let legacy = template
        .get("steps")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|step| {
            step.get("legacy_command")
                .is_some_and(|value| !value.is_null())
        })
        .filter_map(|step| {
            step.get("id")
                .and_then(Value::as_str)
                .map(ToString::to_string)
        })
        .collect::<Vec<_>>();
    if legacy.is_empty() {
        return Ok(());
    }
    Err(diagnostic(
        "sop_legacy_command_unsupported",
        "sop_legacy_command_unsupported",
        json!({"step_ids":legacy,"remediation":"Replace each command step with a governed action step targeting the domain MCP surface that owns the effect."}),
    ))
}

fn assert_no_recursive_child(
    db: &Connection,
    parent_run_id: &str,
    child_sop_id: &str,
    child_version: i64,
) -> Result<(), Value> {
    let mut current = Some(parent_run_id.to_string());
    let mut seen = HashSet::new();
    let mut chain = Vec::new();
    while let Some(run_id) = current {
        if !seen.insert(run_id.clone()) {
            return Err(diagnostic(
                "sop_parent_chain_cycle",
                &format!("sop_parent_chain_cycle:{run_id}"),
                json!({}),
            ));
        }
        let run = get_run(db, &run_id)?;
        chain.push(json!({"run_id":run.run_id,"sop_id":run.sop_id,"sop_version":run.sop_version}));
        if run.sop_id == child_sop_id {
            return Err(diagnostic(
                "sop_recursive_child_occurrence",
                &format!("sop_recursive_child_occurrence:{child_sop_id}@v{child_version}"),
                json!({"ancestor_chain":chain}),
            ));
        }
        current = run.parent_run_id;
    }
    Ok(())
}

fn get_run(db: &Connection, run_id: &str) -> Result<Run, Value> {
    let row = db
        .query_row(
            "SELECT * FROM sop_runs WHERE run_id = ?",
            params![run_id],
            row_json,
        )
        .optional()
        .map_err(|error| diagnostic("sop_run_query_failed", &error.to_string(), json!({})))?
        .ok_or_else(|| {
            diagnostic(
                "sop_run_not_found",
                &format!("sop_run_not_found:{run_id}"),
                json!({}),
            )
        })?;
    hydrate_run(row)
}

fn hydrate_run(row: Value) -> Result<Run, Value> {
    let object = row
        .as_object()
        .ok_or_else(|| diagnostic("sop_run_corrupt", "sop_run_corrupt", json!({})))?;
    let run_id = required_string(object.get("run_id"), "sop_run_corrupt", 512)?;
    let sop_id = required_string(object.get("sop_id"), "sop_run_corrupt", 256)?;
    let sop_version = object
        .get("sop_version")
        .and_then(Value::as_i64)
        .filter(|value| *value >= 1)
        .ok_or_else(|| {
            diagnostic(
                "sop_run_corrupt",
                "sop_run_corrupt",
                json!({"run_id":run_id}),
            )
        })?;
    let status = required_string(object.get("status"), "sop_run_status_invalid", 64)?;
    if !RUN_STATUSES.contains(&status.as_str()) {
        return Err(diagnostic(
            "sop_run_status_invalid",
            &format!("sop_run_status_invalid:{status}"),
            json!({"run_id":run_id}),
        ));
    }
    let definition = object
        .get("definition_json")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let step_states = object
        .get("step_states_json")
        .and_then(Value::as_array)
        .cloned()
        .ok_or_else(|| {
            diagnostic(
                "sop_run_corrupt",
                &format!("sop_run_corrupt:{run_id}"),
                json!({"reason":"step_states_json is not an array"}),
            )
        })?;
    validate_step_graph(&step_states)?;
    for step in &step_states {
        let step_status = step
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if !STEP_STATUSES.contains(&step_status) {
            return Err(diagnostic(
                "sop_persisted_step_status_invalid",
                &format!("sop_persisted_step_status_invalid:{step_status}"),
                json!({"step_id":step.get("step_id")}),
            ));
        }
        if !step.get("result").is_some_and(Value::is_object) {
            return Err(diagnostic(
                "sop_persisted_step_result_invalid",
                "sop_persisted_step_result_invalid",
                json!({"step_id":step.get("step_id")}),
            ));
        }
    }
    let occurrence_key = object
        .get("occurrence_key")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let request_fingerprint = object
        .get("request_fingerprint")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let definition_fingerprint = object
        .get("definition_fingerprint")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let input = object
        .get("input_json")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let input_ref = normalize_value_ref(object.get("input_ref_json"), "sop_input_ref")?;
    let trigger_source_kind = object
        .get("trigger_source_kind")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let trigger_source_ref = object
        .get("trigger_source_ref")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let triggered_by = object
        .get("triggered_by")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let parent_run_id = optional_string(object.get("parent_run_id"));
    let parent_step_id = optional_string(object.get("parent_step_id"));
    if !definition_fingerprint.is_empty() {
        if definition.get("schema").and_then(Value::as_str) != Some("narada.sop.definition.v2")
            || definition.get("sop_id").and_then(Value::as_str) != Some(sop_id.as_str())
            || definition.get("version").and_then(Value::as_i64) != Some(sop_version)
        {
            return Err(diagnostic(
                "sop_definition_identity_mismatch",
                &format!("sop_definition_identity_mismatch:{run_id}"),
                json!({"run_id":run_id}),
            ));
        }
        let actual = fingerprint(&definition);
        if actual != definition_fingerprint {
            return Err(diagnostic(
                "sop_definition_fingerprint_mismatch",
                &format!("sop_definition_fingerprint_mismatch:{run_id}"),
                json!({"run_id":run_id,"expected":definition_fingerprint,"actual":actual}),
            ));
        }
    }
    if !request_fingerprint.is_empty() {
        let actual = fingerprint(&json!({
            "sop_id":sop_id,"sop_version":sop_version,"occurrence_key":occurrence_key,
            "input":input,"input_ref":input_ref,"trigger_source_kind":trigger_source_kind,
            "trigger_source_ref":trigger_source_ref,"triggered_by":triggered_by,
            "parent_run_id":parent_run_id,"parent_step_id":parent_step_id
        }));
        if actual != request_fingerprint {
            return Err(diagnostic(
                "sop_request_fingerprint_mismatch",
                &format!("sop_request_fingerprint_mismatch:{run_id}"),
                json!({"run_id":run_id,"expected":request_fingerprint,"actual":actual}),
            ));
        }
    }
    Ok(Run {
        run_id,
        sop_id,
        sop_version,
        sop_title: required_string(object.get("sop_title"), "sop_run_corrupt", 512)?,
        status,
        occurrence_key,
        request_fingerprint,
        definition_fingerprint,
        definition,
        input,
        input_ref,
        output: object
            .get("output_json")
            .cloned()
            .unwrap_or_else(|| json!({})),
        output_ref: normalize_value_ref(object.get("output_ref_json"), "sop_output_ref")?,
        step_states,
        trigger_source_kind,
        trigger_source_ref,
        triggered_by,
        parent_run_id,
        parent_step_id,
        created_at: required_string(object.get("created_at"), "sop_run_corrupt", 512)?,
        updated_at: required_string(object.get("updated_at"), "sop_run_corrupt", 512)?,
        completed_at: optional_string(object.get("completed_at")),
    })
}

fn run_result(run: &Run, admission: Option<&str>) -> Value {
    let next_steps = run
        .step_states
        .iter()
        .filter(|step| step.get("status").and_then(Value::as_str) == Some("running"))
        .map(|step| {
            let result = step.get("result").cloned().unwrap_or_else(|| json!({}));
            let instructions = result
                .get("instructions")
                .cloned()
                .unwrap_or_else(|| step.get("instructions").cloned().unwrap_or(Value::Null));
            let action_target = step.get("action").and_then(Value::as_object).map(|action| {
                json!({"surface_id":action.get("surface_id"),"tool_name":action.get("tool_name")})
            });
            json!({
                "step_id":step.get("step_id"),"executor":step.get("executor"),
                "title":step.get("title"),"instructions":instructions,
                "child_run_id":step.get("child_run_id"),"child_sop_id":step.get("sop_id"),
                "action_id":step.get("action_id"),"action_target":action_target,
                "result":result,"result_ref":step.get("result_ref")
            })
        })
        .collect::<Vec<_>>();
    let child_pins = run
        .step_states
        .iter()
        .filter(|step| step.get("executor").and_then(Value::as_str) == Some("sop"))
        .map(|step| {
            json!({
                "step_id":step.get("step_id"),"sop_id":step.get("sop_id"),
                "sop_version":step.get("sop_version"),
                "definition_fingerprint":step.get("pinned_child_definition_fingerprint")
            })
        })
        .collect::<Vec<_>>();
    json!({
        "schema":"narada.sop.run.v2","run_id":run.run_id,"sop_id":run.sop_id,
        "sop_version":run.sop_version,"sop_title":run.sop_title,"status":run.status,
        "occurrence_key":run.occurrence_key,"request_fingerprint":run.request_fingerprint,
        "definition_fingerprint":run.definition_fingerprint,"input":run.input,
        "input_ref":run.input_ref,"output":run.output,"output_ref":run.output_ref,
        "step_states":run.step_states,"step_states_parse_error":null,
        "trigger_source_kind":run.trigger_source_kind,"trigger_source_ref":run.trigger_source_ref,
        "triggered_by":run.triggered_by,"parent_run_id":run.parent_run_id,
        "parent_step_id":run.parent_step_id,"created_at":run.created_at,
        "updated_at":run.updated_at,"completed_at":run.completed_at,
        "definition_snapshot":{"stored":true,"fingerprint":run.definition_fingerprint,
            "sop_id":run.sop_id,"sop_version":run.sop_version,"child_pins":child_pins},
        "admission":admission,"next_awaits_confirmation":next_steps.iter().any(|step|matches!(step.get("executor").and_then(Value::as_str),Some("agent")|Some("operator"))),
        "next_steps":next_steps,"next_step":next_steps.first().cloned().unwrap_or(Value::Null),
        "relationship_reconciliation":{"mode":"automatic","repair_tool":"sop_run_refresh"}
    })
}

fn validate_step_graph(steps: &[Value]) -> Result<(), Value> {
    if steps.is_empty() || steps.len() > 128 {
        return Err(diagnostic(
            "sop_step_count_invalid",
            "sop_step_count_invalid",
            json!({"count":steps.len(),"min":1,"max":128}),
        ));
    }
    let mut ids = HashSet::new();
    for step in steps {
        let id = step
            .get("step_id")
            .or_else(|| step.get("id"))
            .and_then(Value::as_str)
            .unwrap_or_default();
        if id.is_empty() || !ids.insert(id.to_string()) {
            return Err(diagnostic(
                "sop_duplicate_step_id",
                "sop_duplicate_step_id",
                json!({"step_id":id}),
            ));
        }
    }
    for step in steps {
        let id = step
            .get("step_id")
            .or_else(|| step.get("id"))
            .and_then(Value::as_str)
            .unwrap_or_default();
        for dependency in step
            .get("depends_on")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
        {
            if !ids.contains(dependency) {
                return Err(diagnostic(
                    "sop_unknown_dependency",
                    "sop_unknown_dependency",
                    json!({"step_id":id,"dependency":dependency}),
                ));
            }
        }
    }
    Ok(())
}

fn normalize_value_ref(value: Option<&Value>, field: &str) -> Result<Value, Value> {
    let Some(value) = value else {
        return Ok(Value::Null);
    };
    if value.is_null() {
        return Ok(Value::Null);
    }
    let object = value.as_object().ok_or_else(|| {
        diagnostic(
            &format!("{field}_invalid"),
            &format!("{field}_invalid"),
            json!({"field":field,"reason":"must_be_object"}),
        )
    })?;
    let allowed = ["ref", "sha256", "byte_length", "media_type"];
    let unknown = object
        .keys()
        .filter(|key| !allowed.contains(&key.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    if !unknown.is_empty() {
        return Err(diagnostic(
            &format!("{field}_invalid"),
            &format!("{field}_invalid"),
            json!({"field":field,"reason":"unknown_fields","fields":unknown}),
        ));
    }
    let reference = required_string(object.get("ref"), "sop_string_required", 2048)?;
    let sha256 = required_string(object.get("sha256"), "sop_string_required", 64)?.to_lowercase();
    if sha256.len() != 64
        || !sha256
            .chars()
            .all(|character| character.is_ascii_hexdigit() && !character.is_ascii_uppercase())
    {
        return Err(diagnostic(
            &format!("{field}_invalid"),
            &format!("{field}_invalid"),
            json!({"field":field,"reason":"sha256_must_be_64_lowercase_hex"}),
        ));
    }
    let byte_length = match object.get("byte_length") {
        None | Some(Value::Null) => Value::Null,
        Some(value) => match value.as_i64() {
            Some(length) if length >= 0 => json!(length),
            _ => {
                return Err(diagnostic(
                    &format!("{field}_invalid"),
                    &format!("{field}_invalid"),
                    json!({"field":field,"reason":"byte_length_must_be_nonnegative_safe_integer"}),
                ))
            }
        },
    };
    let media_type = match object.get("media_type") {
        None | Some(Value::Null) => Value::Null,
        value => json!(required_string(value, "sop_string_required", 200)?),
    };
    Ok(json!({"ref":reference,"sha256":sha256,"byte_length":byte_length,"media_type":media_type}))
}

fn validate_schema(
    schema: Option<&Value>,
    value: &Value,
    code: &str,
    details: Value,
) -> Result<(), Value> {
    let Some(schema) = schema else {
        return Ok(());
    };
    let validator = validator_for(schema).map_err(|error| {
        diagnostic(
            "sop_json_schema_invalid",
            "sop_json_schema_invalid",
            json!({"message":error.to_string()}),
        )
    })?;
    let errors = validator
        .iter_errors(value)
        .take(20)
        .map(|error| error.to_string())
        .collect::<Vec<_>>();
    if errors.is_empty() {
        Ok(())
    } else {
        Err(diagnostic(
            code,
            code,
            merge_details(details, json!({"errors":errors})),
        ))
    }
}

fn merge_details(mut left: Value, right: Value) -> Value {
    if let (Some(left), Some(right)) = (left.as_object_mut(), right.as_object()) {
        for (key, value) in right {
            left.insert(key.clone(), value.clone());
        }
    }
    left
}

fn nullable_json(value: &Value) -> Option<String> {
    if value.is_null() {
        None
    } else {
        Some(canonical_json(value))
    }
}

fn append_run_event(
    db: &Connection,
    run_id: &str,
    step_id: Option<&str>,
    event_kind: &str,
    details: Value,
) -> Result<(), Value> {
    db.execute(
        "INSERT INTO sop_events (event_id, run_id, step_id, event_kind, details_json, recorded_at) VALUES (?, ?, ?, ?, ?, ?)",
        params![format!("soe_{}", &Uuid::new_v4().to_string()[..12]),run_id,step_id.unwrap_or(""),event_kind,canonical_json(&details),now_iso()],
    )
    .map_err(|error| diagnostic("sop_event_insert_failed", &error.to_string(), json!({})))?;
    Ok(())
}

fn persist_run(db: &Connection, run: &mut Run) -> Result<(), Value> {
    let step_states = Value::Array(run.step_states.clone());
    assert_bound(&step_states, "sop_run_state", MAX_RUN_STATE_BYTES)?;
    run.updated_at = now_iso();
    db.execute(
        "UPDATE sop_runs SET status = ?, output_json = ?, output_ref_json = ?, step_states_json = ?, updated_at = ?, completed_at = ? WHERE run_id = ?",
        params![
            run.status,
            canonical_json(&run.output),
            nullable_json(&run.output_ref),
            canonical_json(&step_states),
            run.updated_at,
            run.completed_at,
            run.run_id
        ],
    )
    .map_err(|error| diagnostic("sop_run_update_failed", &error.to_string(), json!({})))?;
    Ok(())
}

fn step_string(step: &Value, key: &str) -> String {
    step.get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn set_step(step: &mut Value, key: &str, value: Value) {
    step.as_object_mut()
        .expect("normalized step state")
        .insert(key.to_string(), value);
}

fn value_context(run: &Run) -> Value {
    let steps = run
        .step_states
        .iter()
        .map(|step| {
            json!({
                "step_id":step.get("step_id"),"status":step.get("status"),
                "result":step.get("result"),"result_ref":step.get("result_ref")
            })
        })
        .collect::<Vec<_>>();
    json!({"input":run.input,"input_ref":run.input_ref,"steps":steps})
}

fn read_reference(reference: &str, context: &Value) -> Option<Value> {
    let mut segments = reference.split('.').collect::<Vec<_>>();
    if segments.iter().any(|segment| {
        segment.is_empty() || matches!(*segment, "__proto__" | "prototype" | "constructor")
    }) {
        return None;
    }
    let mut current = if segments.first() == Some(&"input") {
        segments.remove(0);
        context.get("input")?.clone()
    } else if segments.first() == Some(&"input_ref") {
        segments.remove(0);
        context.get("input_ref")?.clone()
    } else if segments.first() == Some(&"steps") && segments.len() >= 3 {
        let step_id = segments[1];
        let step = context
            .get("steps")?
            .as_array()?
            .iter()
            .find(|step| step.get("step_id").and_then(Value::as_str) == Some(step_id))?
            .clone();
        segments.drain(0..2);
        step
    } else {
        return None;
    };
    for segment in segments {
        current = match current {
            Value::Array(values) => {
                let index = segment.parse::<usize>().ok()?;
                values.get(index)?.clone()
            }
            Value::Object(object) => object.get(segment)?.clone(),
            _ => return None,
        };
    }
    Some(current)
}

fn resolve_mapping(mapping: &Value, context: &Value) -> Result<Value, Value> {
    match mapping {
        Value::Array(values) => values
            .iter()
            .map(|value| resolve_mapping(value, context))
            .collect::<Result<Vec<_>, _>>()
            .map(Value::Array),
        Value::Object(object) if object.len() == 1 && object.contains_key("$ref") => {
            let reference = required_string(object.get("$ref"), "sop_string_required", 512)?;
            read_reference(&reference, context).ok_or_else(|| {
                diagnostic(
                    "sop_mapping_reference_missing",
                    "sop_mapping_reference_missing",
                    json!({"ref":reference}),
                )
            })
        }
        Value::Object(object) => {
            let mut output = Map::new();
            for (key, value) in object {
                output.insert(key.clone(), resolve_mapping(value, context)?);
            }
            Ok(Value::Object(output))
        }
        _ => Ok(mapping.clone()),
    }
}

fn evaluate_condition(condition: &Value, context: &Value) -> Result<bool, Value> {
    if condition.is_null() {
        return Ok(true);
    }
    let object = condition.as_object().ok_or_else(|| {
        diagnostic(
            "sop_condition_invalid",
            "sop_condition_invalid",
            json!({"reason":"condition_must_be_object"}),
        )
    })?;
    if let Some(all) = object.get("all").and_then(Value::as_array) {
        for child in all {
            if !evaluate_condition(child, context)? {
                return Ok(false);
            }
        }
        return Ok(true);
    }
    if let Some(any) = object.get("any").and_then(Value::as_array) {
        for child in any {
            if evaluate_condition(child, context)? {
                return Ok(true);
            }
        }
        return Ok(false);
    }
    if let Some(not) = object.get("not") {
        return Ok(!evaluate_condition(not, context)?);
    }
    let reference = required_string(object.get("ref"), "sop_string_required", 512)?;
    let operation = required_string(object.get("op"), "sop_string_required", 32)?;
    let resolved = read_reference(&reference, context);
    let comparison = object.get("value").cloned().unwrap_or(Value::Null);
    Ok(match operation.as_str() {
        "exists" => resolved.is_some(),
        "not_exists" => resolved.is_none(),
        "truthy" => resolved.as_ref().is_some_and(js_truthy),
        "falsy" => resolved.as_ref().is_some_and(|value| !js_truthy(value)),
        "equals" => resolved.as_ref().is_some_and(|value| value == &comparison),
        "not_equals" => resolved.as_ref().is_none_or(|value| value != &comparison),
        "in" => resolved.as_ref().is_some_and(|value| {
            comparison
                .as_array()
                .is_some_and(|values| values.iter().any(|candidate| candidate == value))
        }),
        "contains" => resolved.as_ref().is_some_and(|value| {
            value
                .as_array()
                .is_some_and(|values| values.iter().any(|candidate| candidate == &comparison))
        }),
        _ => {
            return Err(diagnostic(
                "sop_condition_invalid",
                "sop_condition_invalid",
                json!({"reason":"unsupported_operator","op":operation}),
            ))
        }
    })
}

fn js_truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::Number(value) => value.as_f64().is_some_and(|value| value != 0.0),
        Value::String(value) => !value.is_empty(),
        Value::Array(_) | Value::Object(_) => true,
    }
}

fn render_instructions(template: &str, context: &Value) -> Result<String, Value> {
    let mut output = String::new();
    let mut remaining = template;
    while let Some(start) = remaining.find("{{") {
        output.push_str(&remaining[..start]);
        let tail = &remaining[start + 2..];
        let Some(end) = tail.find("}}") else {
            output.push_str(&remaining[start..]);
            return Ok(output);
        };
        let reference = tail[..end].trim();
        let resolved = read_reference(reference, context).ok_or_else(|| {
            diagnostic(
                "sop_mapping_reference_missing",
                "sop_mapping_reference_missing",
                json!({"ref":reference}),
            )
        })?;
        match resolved {
            Value::String(value) => output.push_str(&value),
            Value::Number(value) => output.push_str(&value.to_string()),
            Value::Bool(value) => output.push_str(if value { "true" } else { "false" }),
            value => output.push_str(&canonical_json(&value)),
        }
        remaining = &tail[end + 2..];
    }
    output.push_str(remaining);
    Ok(output)
}

fn ensure_handoff_intent(
    db: &Connection,
    run: &Run,
    step: &Value,
    rendered_instructions: Option<&str>,
) -> Result<Value, Value> {
    let executor = step_string(step, "executor");
    let step_id = step_string(step, "step_id");
    if !matches!(executor.as_str(), "agent" | "operator") {
        return Err(diagnostic(
            "sop_step_not_manual_handoff",
            &format!("sop_step_not_manual_handoff:{step_id}"),
            json!({"executor":executor}),
        ));
    }
    let context = value_context(run);
    let input = match step.get("input") {
        None | Some(Value::Null) => json!({}),
        Some(mapping) => resolve_mapping(mapping, &context)?,
    };
    assert_bound(&input, "sop_handoff_input", MAX_INLINE_VALUE_BYTES)?;
    let input_ref = match step.get("input_ref") {
        None | Some(Value::Null) => Value::Null,
        Some(mapping) => {
            let resolved = resolve_mapping(mapping, &context)?;
            normalize_value_ref(Some(&resolved), "sop_handoff_input_ref")?
        }
    };
    let instructions = if let Some(rendered) = rendered_instructions {
        rendered.to_string()
    } else if let Some(recorded) = step
        .get("result")
        .and_then(|result| result.get("instructions"))
        .and_then(Value::as_str)
    {
        recorded.to_string()
    } else {
        render_instructions(
            step.get("instructions")
                .and_then(Value::as_str)
                .unwrap_or_default(),
            &context,
        )?
    };
    let title = step_string(step, "title");
    let result_schema = step.get("result_schema").cloned().unwrap_or(Value::Null);
    let identity = format!("{}\0{}", run.run_id, step_id);
    let handoff_id = deterministic_id("soh_", &identity);
    let occurrence_key = deterministic_id("sop_handoff_", &identity);
    let request_fingerprint = fingerprint(&json!({
        "run_id":run.run_id,"step_id":step_id,"sop_id":run.sop_id,
        "sop_version":run.sop_version,"executor":executor,"title":title,
        "instructions":instructions,"input":input,"input_ref":input_ref,
        "result_schema":result_schema
    }));
    let existing_id = db
        .query_row(
            "SELECT handoff_id FROM sop_handoffs WHERE run_id = ? AND step_id = ?",
            params![run.run_id, step_id],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(|error| diagnostic("sop_handoff_query_failed", &error.to_string(), json!({})))?;
    if let Some(existing_id) = existing_id {
        let existing = get_handoff(db, &existing_id)?;
        if existing.get("handoff_id").and_then(Value::as_str) != Some(handoff_id.as_str())
            || existing.get("request_fingerprint").and_then(Value::as_str)
                != Some(request_fingerprint.as_str())
        {
            return Err(diagnostic(
                "sop_handoff_intent_conflict",
                "sop_handoff_intent_conflict",
                json!({"run_id":run.run_id,"step_id":step_id}),
            ));
        }
        return Ok(existing);
    }
    let now = now_iso();
    db.execute(
        "INSERT INTO sop_handoffs(handoff_id, run_id, step_id, occurrence_key, sop_id, sop_version, executor, title, instructions, input_json, input_ref_json, result_schema_json, request_fingerprint, status, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 'pending', ?, ?)",
        params![handoff_id,run.run_id,step_id,occurrence_key,run.sop_id,run.sop_version,
            executor,title,instructions,canonical_json(&input),nullable_json(&input_ref),
            nullable_json(&result_schema),request_fingerprint,now,now],
    )
    .map_err(|error| diagnostic("sop_handoff_insert_failed", &error.to_string(), json!({})))?;
    get_handoff(db, &handoff_id)
}

fn hydrate_action(row: Value) -> Result<Value, Value> {
    let object = row
        .as_object()
        .ok_or_else(|| diagnostic("sop_action_corrupt", "sop_action_corrupt", json!({})))?;
    let action_id = required_string(object.get("action_id"), "sop_action_corrupt", 512)?;
    let run_id = required_string(object.get("run_id"), "sop_action_corrupt", 512)?;
    let step_id = required_string(object.get("step_id"), "sop_action_corrupt", 512)?;
    let occurrence_key = required_string(object.get("occurrence_key"), "sop_action_corrupt", 512)?;
    let surface_id = required_string(object.get("surface_id"), "sop_action_corrupt", 256)?;
    let tool_name = required_string(object.get("tool_name"), "sop_action_corrupt", 256)?;
    let arguments = object
        .get("arguments_json")
        .cloned()
        .unwrap_or_else(|| json!({}));
    if !arguments.is_object() {
        return Err(diagnostic(
            "sop_action_corrupt",
            "sop_action_corrupt",
            json!({"field":"arguments_json"}),
        ));
    }
    assert_bound(&arguments, "sop_action_arguments", MAX_INLINE_VALUE_BYTES)?;
    let request_fingerprint =
        required_string(object.get("request_fingerprint"), "sop_action_corrupt", 512)?;
    let expected_action_id = deterministic_id("soa_", &format!("{run_id}\0{step_id}"));
    let expected_occurrence_key = deterministic_id("sop_action_", &format!("{run_id}\0{step_id}"));
    if action_id != expected_action_id || occurrence_key != expected_occurrence_key {
        return Err(diagnostic(
            "sop_action_identity_mismatch",
            &format!("sop_action_identity_mismatch:{action_id}"),
            json!({"action_id":action_id,"expected_action_id":expected_action_id,
                "occurrence_key":occurrence_key,"expected_occurrence_key":expected_occurrence_key}),
        ));
    }
    let actual_request_fingerprint = fingerprint(&json!({
        "surface_id":surface_id,"tool_name":tool_name,"arguments":arguments
    }));
    if request_fingerprint != actual_request_fingerprint {
        return Err(diagnostic(
            "sop_action_request_fingerprint_mismatch",
            &format!("sop_action_request_fingerprint_mismatch:{action_id}"),
            json!({"action_id":action_id,"expected":request_fingerprint,"actual":actual_request_fingerprint}),
        ));
    }
    let status = required_string(object.get("status"), "sop_action_status_invalid", 64)?;
    if !matches!(
        status.as_str(),
        "pending" | "completed" | "failed" | "cancelled"
    ) {
        return Err(diagnostic(
            "sop_action_status_invalid",
            &format!("sop_action_status_invalid:{status}"),
            json!({}),
        ));
    }
    let completion_key = optional_string(object.get("completion_key"));
    let completion_fingerprint = optional_string(object.get("completion_fingerprint"));
    let operation_ref = optional_string(object.get("operation_ref"));
    let result = object
        .get("result_json")
        .cloned()
        .unwrap_or_else(|| json!({}));
    if !result.is_object() {
        return Err(diagnostic(
            "sop_action_corrupt",
            "sop_action_corrupt",
            json!({"field":"result_json"}),
        ));
    }
    let result_ref = normalize_value_ref(object.get("result_ref_json"), "sop_result_ref")?;
    let error_message = optional_string(object.get("error_message"));
    if let Some(recorded) = completion_fingerprint.as_ref() {
        if completion_key.is_none()
            || operation_ref.is_none()
            || !matches!(status.as_str(), "completed" | "failed")
        {
            return Err(diagnostic(
                "sop_action_completion_identity_invalid",
                "sop_action_completion_identity_invalid",
                json!({"action_id":action_id,"status":status}),
            ));
        }
        let actual = fingerprint(&json!({
            "completion_key":completion_key,"outcome":status,"operation_ref":operation_ref,
            "result":result,"result_ref":result_ref,"error_message":error_message
        }));
        if recorded != &actual {
            return Err(diagnostic(
                "sop_action_completion_fingerprint_mismatch",
                "sop_action_completion_fingerprint_mismatch",
                json!({"action_id":action_id}),
            ));
        }
    } else if completion_key.is_some()
        || operation_ref.is_some()
        || matches!(status.as_str(), "completed" | "failed")
    {
        return Err(diagnostic(
            "sop_action_completion_identity_invalid",
            "sop_action_completion_identity_invalid",
            json!({"action_id":action_id,"status":status}),
        ));
    }
    Ok(json!({
        "schema":"narada.sop.action.v1","action_id":action_id,"run_id":run_id,
        "step_id":step_id,"occurrence_key":occurrence_key,"surface_id":surface_id,
        "tool_name":tool_name,"arguments":arguments,"request_fingerprint":request_fingerprint,
        "status":status,"completion_key":completion_key,"completion_fingerprint":completion_fingerprint,
        "operation_ref":operation_ref,"result":result,"result_ref":result_ref,
        "error_message":error_message,
        "created_at":object.get("created_at").cloned().unwrap_or(Value::Null),
        "updated_at":object.get("updated_at").cloned().unwrap_or(Value::Null),
        "completed_at":object.get("completed_at").cloned().unwrap_or(Value::Null)
    }))
}

fn get_action(db: &Connection, action_id: &str) -> Result<Value, Value> {
    let row = db
        .query_row(
            "SELECT * FROM sop_actions WHERE action_id = ?",
            params![action_id],
            row_json,
        )
        .optional()
        .map_err(|error| diagnostic("sop_action_query_failed", &error.to_string(), json!({})))?
        .ok_or_else(|| {
            diagnostic(
                "sop_action_not_found",
                &format!("sop_action_not_found:{action_id}"),
                json!({}),
            )
        })?;
    hydrate_action(row)
}

fn action_resolution_run_view(db: &Connection, run_id: &str) -> Value {
    match get_run(db, run_id) {
        Ok(run) => run_result(&run, None),
        Err(error) => {
            let fallback = db
                .query_row(
                    "SELECT run_id, sop_id, sop_version, status, occurrence_key, updated_at FROM sop_runs WHERE run_id = ?",
                    params![run_id],
                    row_json,
                )
                .optional()
                .ok()
                .flatten()
                .unwrap_or_else(|| json!({"run_id":run_id}));
            let mut fallback = fallback.as_object().cloned().unwrap_or_default();
            fallback.insert("unavailable".to_string(), json!(true));
            fallback.insert("diagnostic".to_string(), error);
            Value::Object(fallback)
        }
    }
}

fn ensure_action_intent(db: &Connection, run: &Run, step: &Value) -> Result<Value, Value> {
    let step_id = step_string(step, "step_id");
    let action = step
        .get("action")
        .and_then(Value::as_object)
        .ok_or_else(|| {
            diagnostic(
                "sop_action_binding_required",
                &format!("sop_action_binding_required:{step_id}"),
                json!({}),
            )
        })?;
    let action_id = deterministic_id("soa_", &format!("{}\0{}", run.run_id, step_id));
    let occurrence_key = deterministic_id("sop_action_", &format!("{}\0{}", run.run_id, step_id));
    let mapped = resolve_mapping(
        action.get("arguments").unwrap_or(&json!({})),
        &value_context(run),
    )?;
    let mut arguments = mapped.as_object().cloned().ok_or_else(|| {
        diagnostic(
            "sop_action_arguments_must_be_object",
            &format!("sop_action_arguments_must_be_object:{step_id}"),
            json!({}),
        )
    })?;
    let idempotency_field = required_string(
        action.get("idempotency_key_argument"),
        "sop_action_requires_idempotency_key_argument",
        128,
    )?;
    if arguments
        .get(&idempotency_field)
        .is_some_and(|value| value != &json!(occurrence_key))
    {
        return Err(diagnostic(
            "sop_action_idempotency_argument_conflict",
            &format!("sop_action_idempotency_argument_conflict:{step_id}"),
            json!({"field":idempotency_field}),
        ));
    }
    arguments.insert(idempotency_field, json!(occurrence_key));
    let arguments = Value::Object(arguments);
    assert_bound(&arguments, "sop_action_arguments", MAX_INLINE_VALUE_BYTES)?;
    let surface_id = required_string(
        action.get("surface_id"),
        "sop_action_requires_surface_id",
        256,
    )?;
    let tool_name = required_string(
        action.get("tool_name"),
        "sop_action_requires_tool_name",
        256,
    )?;
    let request_fingerprint = fingerprint(&json!({
        "surface_id":surface_id,"tool_name":tool_name,"arguments":arguments
    }));
    let existing_id = db
        .query_row(
            "SELECT action_id FROM sop_actions WHERE run_id = ? AND step_id = ?",
            params![run.run_id, step_id],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(|error| diagnostic("sop_action_query_failed", &error.to_string(), json!({})))?;
    if let Some(existing_id) = existing_id {
        let existing = get_action(db, &existing_id)?;
        if existing.get("action_id").and_then(Value::as_str) != Some(action_id.as_str())
            || existing.get("request_fingerprint").and_then(Value::as_str)
                != Some(request_fingerprint.as_str())
        {
            return Err(diagnostic(
                "sop_action_intent_conflict",
                &format!("sop_action_intent_conflict:{}:{step_id}", run.run_id),
                json!({}),
            ));
        }
        return Ok(existing);
    }
    let now = now_iso();
    db.execute(
        "INSERT INTO sop_actions (action_id, run_id, step_id, occurrence_key, surface_id, tool_name, arguments_json, request_fingerprint, status, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, 'pending', ?, ?)",
        params![action_id,run.run_id,step_id,occurrence_key,surface_id,tool_name,
            canonical_json(&arguments),request_fingerprint,now,now],
    )
    .map_err(|error| diagnostic("sop_action_insert_failed", &error.to_string(), json!({})))?;
    get_action(db, &action_id)
}

fn reconcile_run_and_ancestors(db: &Connection, run_id: &str) -> Result<(), Value> {
    let mut current = Some(run_id.to_string());
    let mut seen = HashSet::new();
    while let Some(id) = current {
        if !seen.insert(id.clone()) {
            return Err(diagnostic(
                "sop_parent_chain_cycle",
                &format!("sop_parent_chain_cycle:{id}"),
                json!({}),
            ));
        }
        reconcile_run(db, &id, &mut HashSet::new())?;
        current = get_run(db, &id)?.parent_run_id;
    }
    Ok(())
}

fn reconcile_run(db: &Connection, run_id: &str, stack: &mut HashSet<String>) -> Result<Run, Value> {
    if !stack.insert(run_id.to_string()) {
        return Err(diagnostic(
            "sop_child_run_cycle",
            &format!("sop_child_run_cycle:{run_id}"),
            json!({}),
        ));
    }
    let result = (|| -> Result<Run, Value> {
        let mut run = get_run(db, run_id)?;
        if is_run_terminal(&run.status) {
            return Ok(run);
        }
        let mut changed = false;
        let mut progress = true;
        let mut passes = 0usize;
        while progress {
            progress = false;
            passes += 1;
            if passes > run.step_states.len() * 4 + 8 {
                return Err(diagnostic(
                    "sop_reconciliation_did_not_converge",
                    &format!("sop_reconciliation_did_not_converge:{run_id}"),
                    json!({}),
                ));
            }
            let (running_changed, running_progress) = reconcile_running_steps(db, &mut run, stack)?;
            let (pending_changed, pending_progress) = reconcile_pending_steps(db, &mut run, stack)?;
            changed |= running_changed || pending_changed;
            progress |= running_progress || pending_progress;
        }
        let prior_status = run.status.clone();
        let all_terminal = run.step_states.iter().all(|step| {
            matches!(
                step.get("status").and_then(Value::as_str),
                Some("completed" | "failed" | "skipped")
            )
        });
        if all_terminal {
            run.status = if run
                .step_states
                .iter()
                .any(|step| step.get("status").and_then(Value::as_str) == Some("failed"))
            {
                "failed".to_string()
            } else {
                "completed".to_string()
            };
            if run.status == "completed" {
                if let Err(error) = derive_run_output(&mut run) {
                    run.status = "failed".to_string();
                    run.output = json!({});
                    run.output_ref = Value::Null;
                    append_run_event(
                        db,
                        run_id,
                        None,
                        "run_output_failed",
                        json!({"diagnostic":error}),
                    )?;
                }
            } else {
                run.output = json!({});
                run.output_ref = Value::Null;
            }
            if run.completed_at.is_none() {
                run.completed_at = Some(now_iso());
            }
        } else {
            let awaiting_confirmation = run.step_states.iter().any(|step| {
                step.get("status").and_then(Value::as_str) == Some("running")
                    && matches!(
                        step.get("executor").and_then(Value::as_str),
                        Some("agent" | "operator")
                    )
            });
            run.status = if awaiting_confirmation {
                "awaiting_confirmation".to_string()
            } else {
                "running".to_string()
            };
            run.completed_at = None;
        }
        if changed || prior_status != run.status {
            persist_run(db, &mut run)?;
            if prior_status != run.status {
                let event_kind = if is_run_terminal(&run.status) {
                    if run.status == "completed" {
                        "run_completed"
                    } else {
                        "run_failed"
                    }
                } else {
                    "run_state_changed"
                };
                let states = run
                    .step_states
                    .iter()
                    .map(|step| json!({"step_id":step.get("step_id"),"status":step.get("status")}))
                    .collect::<Vec<_>>();
                append_run_event(
                    db,
                    run_id,
                    None,
                    event_kind,
                    json!({"from":prior_status,"to":run.status,"step_states":states}),
                )?;
                if is_run_terminal(&run.status) {
                    put_terminal_outbox(db, &run)?;
                }
            }
        }
        Ok(run)
    })();
    stack.remove(run_id);
    result
}

fn reconcile_running_steps(
    db: &Connection,
    run: &mut Run,
    stack: &mut HashSet<String>,
) -> Result<(bool, bool), Value> {
    let mut changed = false;
    let mut progress = false;
    for index in 0..run.step_states.len() {
        let step = run.step_states[index].clone();
        if step.get("status").and_then(Value::as_str) != Some("running") {
            continue;
        }
        let executor = step_string(&step, "executor");
        let step_id = step_string(&step, "step_id");
        match executor.as_str() {
            "agent" | "operator" => {
                let handoff = ensure_handoff_intent(db, run, &step, None)?;
                let handoff_id = handoff.get("handoff_id").cloned().unwrap_or(Value::Null);
                let occurrence_key = handoff
                    .get("occurrence_key")
                    .cloned()
                    .unwrap_or(Value::Null);
                let prior_id = step.get("result").and_then(|value| value.get("handoff_id"));
                if prior_id != Some(&handoff_id) {
                    let mut result = step
                        .get("result")
                        .and_then(Value::as_object)
                        .cloned()
                        .unwrap_or_default();
                    result.insert("handoff_id".to_string(), handoff_id);
                    result.insert("handoff_occurrence_key".to_string(), occurrence_key);
                    set_step(&mut run.step_states[index], "result", Value::Object(result));
                    changed = true;
                }
            }
            "sop" => {
                let Some(child_run_id) = step.get("child_run_id").and_then(Value::as_str) else {
                    continue;
                };
                reconcile_run(db, child_run_id, stack)?;
                let child = get_run(db, child_run_id)?;
                assert_child_run_binding(run, &step, &child)?;
                if child.status == "completed" {
                    if let Err(error) = validate_schema(
                        step.get("result_schema").filter(|value| !value.is_null()),
                        &child.output,
                        "sop_step_result_schema_mismatch",
                        json!({"run_id":run.run_id,"step_id":step_id}),
                    ) {
                        fail_step(&mut run.step_states[index], diagnostic_text(&error));
                        append_run_event(
                            db,
                            &run.run_id,
                            Some(&step_id),
                            "step_failed",
                            json!({"child_run_id":child.run_id,"diagnostic":error}),
                        )?;
                        changed = true;
                        progress = true;
                        continue;
                    }
                    let full_result = json!({
                        "child_run_id":child.run_id,"child_sop_id":child.sop_id,
                        "child_sop_version":child.sop_version,"child_status":child.status,
                        "output":child.output
                    });
                    let compact_result = json!({
                        "child_run_id":child.run_id,"child_sop_id":child.sop_id,
                        "child_sop_version":child.sop_version,"child_status":child.status
                    });
                    let completed_at = child.completed_at.clone().unwrap_or_else(now_iso);
                    let retained = complete_step_with_bounded_run_state(
                        run,
                        index,
                        &completed_at,
                        full_result,
                        child.output_ref.clone(),
                        compact_result,
                        db,
                    )?;
                    if retained {
                        append_run_event(
                            db,
                            &run.run_id,
                            Some(&step_id),
                            "child_sop_completed",
                            json!({"child_run_id":child.run_id,"child_status":child.status,"output_ref":child.output_ref}),
                        )?;
                    }
                    changed = true;
                    progress = true;
                } else if matches!(child.status.as_str(), "failed" | "cancelled") {
                    fail_step(
                        &mut run.step_states[index],
                        format!("child_sop_{}:{}", child.status, child.run_id),
                    );
                    set_step(
                        &mut run.step_states[index],
                        "result",
                        json!({
                            "child_run_id":child.run_id,"child_sop_id":child.sop_id,
                            "child_sop_version":child.sop_version,"child_status":child.status
                        }),
                    );
                    append_run_event(
                        db,
                        &run.run_id,
                        Some(&step_id),
                        "child_sop_failed",
                        json!({"child_run_id":child.run_id,"child_status":child.status}),
                    )?;
                    changed = true;
                    progress = true;
                }
            }
            "action" => {
                let Some(action_id) = step.get("action_id").and_then(Value::as_str) else {
                    continue;
                };
                let action = ensure_action_intent(db, run, &step)?;
                if action.get("action_id").and_then(Value::as_str) != Some(action_id) {
                    return Err(diagnostic(
                        "sop_action_run_binding_mismatch",
                        &format!("sop_action_run_binding_mismatch:{}:{step_id}", run.run_id),
                        json!({"action_id":action.get("action_id")}),
                    ));
                }
                assert_action_run_binding(run, &step, &action)?;
                let action_status = action
                    .get("status")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if action_status == "completed" {
                    let action_result = action.get("result").cloned().unwrap_or_else(|| json!({}));
                    if let Err(error) = validate_schema(
                        step.get("result_schema").filter(|value| !value.is_null()),
                        &action_result,
                        "sop_step_result_schema_mismatch",
                        json!({"run_id":run.run_id,"step_id":step_id}),
                    ) {
                        fail_step(&mut run.step_states[index], diagnostic_text(&error));
                        set_step(
                            &mut run.step_states[index],
                            "result",
                            json!({"action_id":action.get("action_id"),"operation_ref":action.get("operation_ref"),"surface_id":action.get("surface_id"),"tool_name":action.get("tool_name")}),
                        );
                        set_step(
                            &mut run.step_states[index],
                            "result_ref",
                            action.get("result_ref").cloned().unwrap_or(Value::Null),
                        );
                        append_run_event(
                            db,
                            &run.run_id,
                            Some(&step_id),
                            "step_failed",
                            json!({"action_id":action.get("action_id"),"diagnostic":error}),
                        )?;
                        changed = true;
                        progress = true;
                        continue;
                    }
                    let mut full = action_result.as_object().cloned().unwrap_or_default();
                    for key in ["action_id", "operation_ref", "surface_id", "tool_name"] {
                        full.insert(
                            key.to_string(),
                            action.get(key).cloned().unwrap_or(Value::Null),
                        );
                    }
                    let compact = json!({
                        "action_id":action.get("action_id"),"operation_ref":action.get("operation_ref"),
                        "surface_id":action.get("surface_id"),"tool_name":action.get("tool_name")
                    });
                    let completed_at = action
                        .get("completed_at")
                        .and_then(Value::as_str)
                        .map(ToString::to_string)
                        .unwrap_or_else(now_iso);
                    complete_step_with_bounded_run_state(
                        run,
                        index,
                        &completed_at,
                        Value::Object(full),
                        action.get("result_ref").cloned().unwrap_or(Value::Null),
                        compact,
                        db,
                    )?;
                    changed = true;
                    progress = true;
                } else if matches!(action_status, "failed" | "cancelled") {
                    let error_message = action
                        .get("error_message")
                        .and_then(Value::as_str)
                        .map(ToString::to_string)
                        .unwrap_or_else(|| {
                            format!(
                                "action_{action_status}:{}",
                                action
                                    .get("action_id")
                                    .and_then(Value::as_str)
                                    .unwrap_or_default()
                            )
                        });
                    fail_step(&mut run.step_states[index], error_message);
                    set_step(
                        &mut run.step_states[index],
                        "result",
                        json!({"action_id":action.get("action_id"),"operation_ref":action.get("operation_ref"),"surface_id":action.get("surface_id"),"tool_name":action.get("tool_name")}),
                    );
                    set_step(
                        &mut run.step_states[index],
                        "result_ref",
                        action.get("result_ref").cloned().unwrap_or(Value::Null),
                    );
                    changed = true;
                    progress = true;
                }
            }
            _ => {}
        }
    }
    Ok((changed, progress))
}

fn reconcile_pending_steps(
    db: &Connection,
    run: &mut Run,
    stack: &mut HashSet<String>,
) -> Result<(bool, bool), Value> {
    let mut changed = false;
    let mut progress = false;
    for index in 0..run.step_states.len() {
        let step = run.step_states[index].clone();
        if step.get("status").and_then(Value::as_str) != Some("pending") {
            continue;
        }
        let step_id = step_string(&step, "step_id");
        let dependencies = step
            .get("depends_on")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter_map(|value| value.as_str().map(ToString::to_string))
            .collect::<Vec<_>>();
        let statuses = run
            .step_states
            .iter()
            .filter_map(|candidate| {
                Some((
                    candidate.get("step_id")?.as_str()?.to_string(),
                    candidate.get("status")?.as_str()?.to_string(),
                ))
            })
            .collect::<HashMap<_, _>>();
        let failed = dependencies
            .iter()
            .filter(|dependency| {
                statuses
                    .get(*dependency)
                    .is_some_and(|status| status == "failed")
            })
            .cloned()
            .collect::<Vec<_>>();
        if !failed.is_empty() {
            fail_step(
                &mut run.step_states[index],
                format!("failed_dependency:{}", failed.join(",")),
            );
            append_run_event(
                db,
                &run.run_id,
                Some(&step_id),
                "step_failed",
                json!({"failed_dependencies":failed}),
            )?;
            changed = true;
            progress = true;
            continue;
        }
        if !dependencies.iter().all(|dependency| {
            statuses
                .get(dependency)
                .is_some_and(|status| matches!(status.as_str(), "completed" | "skipped"))
        }) {
            continue;
        }
        let context = value_context(run);
        let attempt = (|| -> Result<(), Value> {
            let condition = step.get("when").cloned().unwrap_or(Value::Null);
            if !evaluate_condition(&condition, &context)? {
                skip_step(&mut run.step_states[index], "condition_false");
                append_run_event(
                    db,
                    &run.run_id,
                    Some(&step_id),
                    "step_skipped",
                    json!({"reason":"condition_false","when":condition}),
                )?;
                return Ok(());
            }
            let instructions = render_instructions(
                step.get("instructions")
                    .and_then(Value::as_str)
                    .unwrap_or_default(),
                &context,
            )?;
            set_step(&mut run.step_states[index], "started_at", json!(now_iso()));
            match step
                .get("executor")
                .and_then(Value::as_str)
                .unwrap_or_default()
            {
                "engine" => {
                    set_step(&mut run.step_states[index], "status", json!("completed"));
                    set_step(
                        &mut run.step_states[index],
                        "completed_at",
                        json!(now_iso()),
                    );
                    set_step(&mut run.step_states[index], "result", json!({}));
                    append_run_event(
                        db,
                        &run.run_id,
                        Some(&step_id),
                        "step_completed",
                        json!({"executor":"engine"}),
                    )?;
                }
                executor @ ("agent" | "operator") => {
                    let handoff = ensure_handoff_intent(db, run, &step, Some(&instructions))?;
                    set_step(&mut run.step_states[index], "status", json!("running"));
                    set_step(
                        &mut run.step_states[index],
                        "result",
                        json!({
                            "instructions":instructions,"handoff_id":handoff.get("handoff_id"),
                            "handoff_occurrence_key":handoff.get("occurrence_key")
                        }),
                    );
                    append_run_event(
                        db,
                        &run.run_id,
                        Some(&step_id),
                        "step_started",
                        json!({
                            "executor":executor,"handoff":true,
                            "handoff_id":handoff.get("handoff_id"),
                            "occurrence_key":handoff.get("occurrence_key")
                        }),
                    )?;
                }
                "action" => {
                    let action = ensure_action_intent(db, run, &step)?;
                    set_step(&mut run.step_states[index], "status", json!("running"));
                    set_step(
                        &mut run.step_states[index],
                        "action_id",
                        action.get("action_id").cloned().unwrap_or(Value::Null),
                    );
                    set_step(
                        &mut run.step_states[index],
                        "result",
                        json!({
                            "instructions":instructions,"action_id":action.get("action_id"),
                            "occurrence_key":action.get("occurrence_key"),
                            "surface_id":action.get("surface_id"),"tool_name":action.get("tool_name")
                        }),
                    );
                    append_run_event(
                        db,
                        &run.run_id,
                        Some(&step_id),
                        "action_admitted",
                        json!({
                            "action_id":action.get("action_id"),"occurrence_key":action.get("occurrence_key"),
                            "surface_id":action.get("surface_id"),"tool_name":action.get("tool_name"),
                            "request_fingerprint":action.get("request_fingerprint")
                        }),
                    )?;
                }
                "sop" => {
                    let child = start_child_run(db, run, &step, stack)?;
                    set_step(&mut run.step_states[index], "status", json!("running"));
                    set_step(
                        &mut run.step_states[index],
                        "child_run_id",
                        json!(child.run_id),
                    );
                    set_step(
                        &mut run.step_states[index],
                        "result",
                        json!({
                            "instructions":instructions,"child_run_id":child.run_id,
                            "child_sop_id":child.sop_id,"child_sop_version":child.sop_version,
                            "child_status":child.status,"wait_policy":"wait"
                        }),
                    );
                    append_run_event(
                        db,
                        &run.run_id,
                        Some(&step_id),
                        "child_sop_admitted",
                        json!({
                            "child_run_id":child.run_id,"child_sop_id":child.sop_id,
                            "child_sop_version":child.sop_version,
                            "child_definition_fingerprint":child.definition_fingerprint
                        }),
                    )?;
                }
                executor => {
                    return Err(diagnostic(
                        "sop_invalid_executor",
                        &format!("sop_invalid_executor:{executor}"),
                        json!({}),
                    ));
                }
            }
            Ok(())
        })();
        if let Err(error) = attempt {
            fail_step(&mut run.step_states[index], diagnostic_text(&error));
            append_run_event(
                db,
                &run.run_id,
                Some(&step_id),
                "step_failed",
                json!({"diagnostic":error}),
            )?;
        }
        changed = true;
        progress = true;
    }
    Ok((changed, progress))
}

fn fail_step(step: &mut Value, message: String) {
    set_step(step, "status", json!("failed"));
    set_step(step, "completed_at", json!(now_iso()));
    set_step(step, "error_message", json!(message));
}

fn skip_step(step: &mut Value, reason: &str) {
    set_step(step, "status", json!("skipped"));
    set_step(step, "completed_at", json!(now_iso()));
    set_step(step, "result", json!({"reason":reason}));
    set_step(step, "error_message", Value::Null);
}

fn diagnostic_text(error: &Value) -> String {
    let code = error
        .get("code")
        .and_then(Value::as_str)
        .unwrap_or("sop_internal_error");
    let message = error
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("sop_internal_error");
    format!("{code}:{message}")
}

fn complete_step_with_bounded_run_state(
    run: &mut Run,
    step_index: usize,
    completed_at: &str,
    full_result: Value,
    result_ref: Value,
    compact_result: Value,
    db: &Connection,
) -> Result<bool, Value> {
    {
        let step = &mut run.step_states[step_index];
        set_step(step, "status", json!("completed"));
        set_step(step, "completed_at", json!(completed_at));
        set_step(step, "error_message", Value::Null);
        set_step(step, "result", full_result);
        set_step(step, "result_ref", result_ref.clone());
    }
    let state = Value::Array(run.step_states.clone());
    match assert_bound(&state, "sop_run_state", MAX_RUN_STATE_BYTES) {
        Ok(()) => Ok(true),
        Err(error)
            if error.get("code").and_then(Value::as_str) == Some("sop_run_state_too_large") =>
        {
            let step_id = step_string(&run.step_states[step_index], "step_id");
            let step = &mut run.step_states[step_index];
            fail_step(step, diagnostic_text(&error));
            let mut compact = compact_result.as_object().cloned().unwrap_or_default();
            compact.insert("inline_result_omitted".to_string(), json!(true));
            set_step(step, "result", Value::Object(compact));
            set_step(step, "result_ref", result_ref.clone());
            assert_bound(
                &Value::Array(run.step_states.clone()),
                "sop_run_state",
                MAX_RUN_STATE_BYTES,
            )?;
            append_run_event(
                db,
                &run.run_id,
                Some(&step_id),
                "step_failed",
                json!({"diagnostic":error,"result_ref":result_ref,"inline_result_omitted":true}),
            )?;
            Ok(false)
        }
        Err(error) => Err(error),
    }
}

fn derive_run_output(run: &mut Run) -> Result<(), Value> {
    let output_mapping = run.definition.get("output").cloned().unwrap_or(Value::Null);
    let output_ref_mapping = run
        .definition
        .get("output_ref")
        .cloned()
        .unwrap_or(Value::Null);
    let context = value_context(run);
    let output = if output_mapping.is_null() {
        json!({})
    } else {
        resolve_mapping(&output_mapping, &context)?
    };
    assert_bound(&output, "sop_output", MAX_INLINE_VALUE_BYTES)?;
    if !output.is_object() {
        return Err(diagnostic(
            "sop_output_must_be_object",
            "sop_output_must_be_object",
            json!({}),
        ));
    }
    validate_schema(
        run.definition
            .get("output_schema")
            .filter(|value| !value.is_null()),
        &output,
        "sop_output_schema_mismatch",
        json!({"run_id":run.run_id}),
    )?;
    let output_ref = if output_ref_mapping.is_null() {
        Value::Null
    } else {
        let resolved = resolve_mapping(&output_ref_mapping, &context)?;
        normalize_value_ref(Some(&resolved), "sop_output_ref")?
    };
    run.output = output;
    run.output_ref = output_ref;
    Ok(())
}

fn assert_child_run_binding(parent: &Run, step: &Value, child: &Run) -> Result<(), Value> {
    let step_id = step_string(step, "step_id");
    let expected_occurrence_key = deterministic_id(
        "sop_child_",
        &format!("{}\0{}\0{}", parent.occurrence_key, parent.run_id, step_id),
    );
    let identity_matches = child.parent_run_id.as_deref() == Some(parent.run_id.as_str())
        && child.parent_step_id.as_deref() == Some(step_id.as_str())
        && step.get("sop_id").and_then(Value::as_str) == Some(child.sop_id.as_str())
        && step.get("sop_version").and_then(Value::as_i64) == Some(child.sop_version)
        && child.occurrence_key == expected_occurrence_key;
    if !identity_matches {
        return Err(diagnostic(
            "sop_child_run_binding_mismatch",
            &format!("sop_child_run_binding_mismatch:{}:{step_id}", parent.run_id),
            json!({"parent_run_id":parent.run_id,"step_id":step_id,"child_run_id":child.run_id}),
        ));
    }
    let expected_pin = step
        .get("pinned_child_definition_fingerprint")
        .and_then(Value::as_str);
    if expected_pin.is_none() || expected_pin != Some(child.definition_fingerprint.as_str()) {
        return Err(diagnostic(
            "sop_child_definition_pin_mismatch",
            &format!("sop_child_definition_pin_mismatch:{step_id}"),
            json!({"expected":expected_pin,"actual":child.definition_fingerprint}),
        ));
    }
    Ok(())
}

fn assert_action_run_binding(run: &Run, step: &Value, action: &Value) -> Result<(), Value> {
    let step_id = step_string(step, "step_id");
    let binding = step.get("action").and_then(Value::as_object);
    let valid = binding.is_some()
        && step.get("action_id").and_then(Value::as_str)
            == action.get("action_id").and_then(Value::as_str)
        && action.get("run_id").and_then(Value::as_str) == Some(run.run_id.as_str())
        && action.get("step_id").and_then(Value::as_str) == Some(step_id.as_str())
        && binding
            .and_then(|value| value.get("surface_id"))
            .and_then(Value::as_str)
            == action.get("surface_id").and_then(Value::as_str)
        && binding
            .and_then(|value| value.get("tool_name"))
            .and_then(Value::as_str)
            == action.get("tool_name").and_then(Value::as_str);
    if !valid {
        return Err(diagnostic(
            "sop_action_run_binding_mismatch",
            &format!("sop_action_run_binding_mismatch:{}:{step_id}", run.run_id),
            json!({"run_id":run.run_id,"step_id":step_id,"action_id":action.get("action_id")}),
        ));
    }
    Ok(())
}

fn start_child_run(
    db: &Connection,
    parent: &Run,
    step: &Value,
    stack: &mut HashSet<String>,
) -> Result<Run, Value> {
    let step_id = step_string(step, "step_id");
    let child_sop_id = required_string(step.get("sop_id"), "sop_step_requires_pinned_child", 256)?;
    let child_version = step
        .get("sop_version")
        .and_then(Value::as_i64)
        .filter(|value| *value >= 1)
        .ok_or_else(|| {
            diagnostic(
                "sop_step_requires_pinned_child",
                &format!("sop_step_requires_pinned_child:{step_id}"),
                json!({}),
            )
        })?;
    let context = value_context(parent);
    let child_input = match step.get("input") {
        None | Some(Value::Null) => json!({}),
        Some(mapping) => resolve_mapping(mapping, &context)?,
    };
    assert_bound(&child_input, "sop_input", MAX_INLINE_VALUE_BYTES)?;
    if !child_input.is_object() {
        return Err(diagnostic(
            "sop_child_input_must_be_object",
            &format!("sop_child_input_must_be_object:{step_id}"),
            json!({}),
        ));
    }
    let child_input_ref = match step.get("input_ref") {
        None | Some(Value::Null) => Value::Null,
        Some(mapping) => {
            let resolved = resolve_mapping(mapping, &context)?;
            normalize_value_ref(Some(&resolved), "sop_input_ref")?
        }
    };
    let occurrence_key = deterministic_id(
        "sop_child_",
        &format!("{}\0{}\0{}", parent.occurrence_key, parent.run_id, step_id),
    );
    let args = json!({
        "sop_id":child_sop_id,"sop_version":child_version,
        "occurrence_key":occurrence_key,"input":child_input,"input_ref":child_input_ref,
        "trigger_source_kind":"parent_sop_step",
        "trigger_source_ref":format!("{}:{step_id}",parent.run_id),
        "triggered_by":format!("sop:{}",parent.run_id)
    });
    let (admitted, _) = admit_run(
        db,
        args.as_object().expect("child admission object"),
        Some(&parent.run_id),
        Some(&step_id),
    )?;
    assert_child_run_binding(parent, step, &admitted)?;
    reconcile_run(db, &admitted.run_id, stack)?;
    get_run(db, &admitted.run_id)
}

fn put_terminal_outbox(db: &Connection, run: &Run) -> Result<(), Value> {
    if !is_run_terminal(&run.status) {
        return Err(diagnostic(
            "sop_outbox_requires_terminal_run",
            "sop_outbox_requires_terminal_run",
            json!({"run_id":run.run_id,"status":run.status}),
        ));
    }
    if !run.output.is_object() {
        return Err(diagnostic(
            "sop_outbox_output_invalid",
            "sop_outbox_output_invalid",
            json!({"run_id":run.run_id}),
        ));
    }
    let event_id = deterministic_id("sote_", &run.run_id);
    let created_at = run.completed_at.clone().unwrap_or_else(now_iso);
    let procedure_outcome = if run.status == "completed" {
        match run.output.get("outcome").and_then(Value::as_str) {
            Some(value) => {
                if value.is_empty() || value.chars().count() > 128 {
                    return Err(diagnostic(
                        "sop_outbox_procedure_outcome_invalid",
                        "sop_outbox_procedure_outcome_invalid",
                        json!({"value":value}),
                    ));
                }
                value.to_string()
            }
            None => run.status.clone(),
        }
    } else {
        run.status.clone()
    };
    let payload = json!({
        "schema":"narada.sop.run_terminal.v2","event_id":event_id,
        "topic":SOP_TERMINAL_TOPIC,"run_id":run.run_id,"sop_id":run.sop_id,
        "sop_version":run.sop_version,"occurrence_key":run.occurrence_key,
        "run_outcome":run.status,"outcome":procedure_outcome,
        "definition_fingerprint":run.definition_fingerprint,
        "trigger_source_kind":run.trigger_source_kind,
        "trigger_source_ref":run.trigger_source_ref,"output":run.output,
        "output_ref":run.output_ref,"completed_at":created_at
    });
    assert_bound(&payload, "sop_outbox_payload", MAX_OUTBOX_PAYLOAD_BYTES)?;
    let existing = db
        .query_row(
            "SELECT * FROM sop_outbox WHERE event_id = ? OR run_id = ?",
            params![event_id, run.run_id],
            row_json,
        )
        .optional()
        .map_err(|error| diagnostic("sop_outbox_query_failed", &error.to_string(), json!({})))?;
    if let Some(existing) = existing {
        let event = hydrate_outbox_event(existing)?;
        let identity_matches = event.get("event_id").and_then(Value::as_str)
            == Some(event_id.as_str())
            && event.get("topic").and_then(Value::as_str) == Some(SOP_TERMINAL_TOPIC)
            && event.get("run_id").and_then(Value::as_str) == Some(run.run_id.as_str())
            && event.get("sop_id").and_then(Value::as_str) == Some(run.sop_id.as_str())
            && event.get("sop_version").and_then(Value::as_i64) == Some(run.sop_version)
            && event.get("occurrence_key").and_then(Value::as_str)
                == Some(run.occurrence_key.as_str())
            && event.get("outcome").and_then(Value::as_str) == Some(run.status.as_str());
        let compacted = event
            .get("compacted_at")
            .is_some_and(|value| !value.is_null());
        let stored_payload = event.get("payload").cloned().unwrap_or_else(|| json!({}));
        let payload_matches = compacted
            || (stored_payload.get("definition_fingerprint")
                == payload.get("definition_fingerprint")
                && stored_payload.get("trigger_source_kind") == payload.get("trigger_source_kind")
                && stored_payload.get("trigger_source_ref") == payload.get("trigger_source_ref")
                && stored_payload.get("run_outcome") == payload.get("run_outcome")
                && stored_payload.get("outcome") == payload.get("outcome")
                && stored_payload.get("output") == payload.get("output")
                && stored_payload.get("output_ref") == payload.get("output_ref"));
        if !identity_matches || !payload_matches {
            return Err(diagnostic(
                "sop_outbox_event_conflict",
                "sop_outbox_event_conflict",
                json!({"event_id":event_id,"run_id":run.run_id}),
            ));
        }
        return Ok(());
    }
    db.execute(
        "INSERT INTO sop_outbox(event_id, topic, partition_key, run_id, sop_id, sop_version, occurrence_key, outcome, payload_json, created_at, available_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        params![event_id,SOP_TERMINAL_TOPIC,run.sop_id,run.run_id,run.sop_id,
            run.sop_version,run.occurrence_key,run.status,canonical_json(&payload),
            created_at,created_at],
    )
    .map_err(|error| diagnostic("sop_outbox_insert_failed", &error.to_string(), json!({})))?;
    Ok(())
}

fn is_run_terminal(status: &str) -> bool {
    matches!(status, "completed" | "failed" | "cancelled")
}
