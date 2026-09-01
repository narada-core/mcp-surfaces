
fn native_task_executability_context(
    server: &LifecycleServer,
    task_number: i64,
) -> Result<(String, String, String, String), String> {
    let row: Option<(
        String,
        String,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        String,
        String,
    )> = server
        .connection()?
        .query_row(
            "select l.task_id, s.title, s.goal_markdown, s.context_markdown,
                    s.required_work_markdown, s.non_goals_markdown,
                    s.acceptance_criteria_json, s.dependencies_json
               from task_lifecycle l
               join task_specs s on s.task_id = l.task_id
              where l.task_number = ?1",
            params![task_number],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                ))
            },
        )
        .optional()
        .map_err(db_error)?;
    let Some((
        task_id,
        title,
        goal,
        context,
        required_work,
        non_goals,
        criteria_json,
        dependencies_json,
    )) = row
    else {
        return Err(format!("task_not_found:{task_number}"));
    };
    let criteria = serde_json::from_str::<Value>(&criteria_json).unwrap_or_else(|_| json!([]));
    let dependencies =
        serde_json::from_str::<Value>(&dependencies_json).unwrap_or_else(|_| json!([]));
    let spec_digest = native_canonical_digest(&json!({
        "kind": "task_spec",
        "title": title,
        "goal": goal,
        "context": context,
        "required_work": required_work,
        "non_goals": non_goals,
        "acceptance_criteria": criteria,
        "dependencies": dependencies
    }));
    let environment_digest = native_environment_digest(&server.options.site_root);
    Ok((task_id, title, spec_digest, environment_digest))
}

fn native_request_id(
    task_id: &str,
    task_spec_digest: &str,
    environment_digest: &str,
    evaluator_profile: &str,
    evaluator_profile_version: &str,
) -> String {
    let digest = native_canonical_digest(&json!({
        "kind": "request",
        "task_id": task_id,
        "task_spec_digest": task_spec_digest,
        "environment_digest": environment_digest,
        "evaluator_profile": evaluator_profile,
        "evaluator_profile_version": evaluator_profile_version
    }));
    format!("texr_{}", &digest[..32])
}

fn native_assessment_id(request_id: &str, created_at: &str) -> String {
    let digest = native_canonical_digest(&json!({
        "kind": "assessment",
        "request_id": request_id,
        "created_at": created_at
    }));
    format!("texa_{}", &digest[..32])
}

fn native_dispatch_fingerprint(
    task_id: &str,
    task_spec_digest: &str,
    environment_digest: &str,
    site_id: &str,
) -> String {
    let digest = native_canonical_digest(&json!({
        "kind": "dispatch",
        "task_id": task_id,
        "task_spec_digest": task_spec_digest,
        "environment_digest": environment_digest,
        "workflow": "implement",
        "site_id": site_id
    }));
    format!("texd_{}", &digest[..32])
}

fn native_override_id(
    task_id: &str,
    dispatch_fingerprint: &str,
    actor: &str,
    created_at: &str,
) -> String {
    let digest = native_canonical_digest(&json!({
        "kind": "override",
        "task_id": task_id,
        "dispatch_fingerprint": dispatch_fingerprint,
        "actor": actor,
        "created_at": created_at
    }));
    format!("texo_{}", &digest[..32])
}

fn native_verdict(findings: &Value) -> &'static str {
    let blocking = findings
        .as_array()
        .into_iter()
        .flatten()
        .filter(|finding| finding.get("severity").and_then(Value::as_str) == Some("blocking"));
    if blocking.clone().any(|finding| {
        matches!(
            finding.get("kind").and_then(Value::as_str),
            Some("unavailable_authority" | "unavailable_tool")
        )
    }) {
        "not_executable"
    } else if blocking.count() > 0 {
        "needs_revision"
    } else {
        "executable"
    }
}

fn native_lease_expiry(minutes: i64) -> String {
    (OffsetDateTime::now_utc() + time::Duration::minutes(minutes))
        .format(&Rfc3339)
        .unwrap_or_else(|_| now())
}

impl LifecycleServer {
    fn task_executability_request(&mut self, args: Value) -> Result<Value, String> {
        let task_number = required_i64(&args, "task_number")?;
        let _agent_id = required_string(&args, "agent_id")?;
        let (task_id, _title, task_spec_digest, environment_digest) =
            native_task_executability_context(self, task_number)?;
        let (profile, _) = native_policy(&self.options.site_root)?;
        let profile_version = "1.0.0";
        let request_id = native_request_id(
            &task_id,
            &task_spec_digest,
            &environment_digest,
            &profile,
            profile_version,
        );
        let timestamp = now();
        let status = {
            let connection = self.connection_mut()?;
            let existing: Option<String> = connection
                .query_row(
                    "select request_id from task_executability_requests where request_id=?1",
                    params![&request_id],
                    |row| row.get(0),
                )
                .optional()
                .map_err(db_error)?;
            if existing.is_none() {
                connection
                    .execute(
                        "insert into task_executability_requests
                         (request_id,task_id,task_number,state,task_spec_digest,
                          environment_digest,evaluator_profile,evaluator_profile_version,
                          assessment_id,lease_owner,lease_expires_at,attempt_count,
                          superseded_by_request_id,created_at,updated_at)
                         values(?1,?2,?3,'pending',?4,?5,?6,?7,null,null,null,0,null,?8,?8)",
                        params![
                            &request_id,
                            &task_id,
                            task_number,
                            &task_spec_digest,
                            &environment_digest,
                            &profile,
                            profile_version,
                            &timestamp
                        ],
                    )
                    .map_err(db_error)?;
            }
            connection
                .execute(
                    "update task_executability_requests
                        set superseded_by_request_id=?1,updated_at=?2
                      where task_id=?3
                        and request_id<>?1
                        and superseded_by_request_id is null
                        and state not in ('completed','failed_terminal')",
                    params![&request_id, &timestamp, &task_id],
                )
                .map_err(db_error)?;
            if existing.is_some() {
                "existing"
            } else {
                "enqueued"
            }
        };
        let row = self
            .query_one(
                "select request_id,task_id,task_number,state,task_spec_digest,
                        environment_digest,evaluator_profile,evaluator_profile_version,
                        attempt_count,lease_owner,lease_expires_at,
                        superseded_by_request_id,created_at,updated_at
                   from task_executability_requests where request_id=?1",
                params![&request_id],
            )?
            .ok_or_else(|| format!("task_executability_request_not_found:{request_id}"))?;
        Ok(json!({
            "schema": "narada.task_executability.request.v0",
            "status": status,
            "request_id": row.get("request_id"),
            "task_number": row.get("task_number"),
            "task_id": row.get("task_id"),
            "state": row.get("state"),
            "task_spec_digest": row.get("task_spec_digest"),
            "environment_digest": row.get("environment_digest"),
            "evaluator_profile": row.get("evaluator_profile"),
            "evaluator_profile_version": row.get("evaluator_profile_version")
        }))
    }

}
