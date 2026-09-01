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
