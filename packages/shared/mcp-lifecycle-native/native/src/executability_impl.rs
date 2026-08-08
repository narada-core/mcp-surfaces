/*
 * Native task-executability implementation.
 *
 * This file is included in lib.rs so the implementation remains part of the
 * lifecycle authority and can use the server's private SQLite/query helpers.
 */

fn native_canonical_value(value: &Value) -> Value {
    match value {
        Value::Array(values) => {
            Value::Array(values.iter().map(native_canonical_value).collect())
        }
        Value::Object(object) => {
            let mut keys = object.keys().cloned().collect::<Vec<_>>();
            keys.sort();
            let mut sorted = Map::new();
            for key in keys {
                if let Some(value) = object.get(&key) {
                    sorted.insert(key, native_canonical_value(value));
                }
            }
            Value::Object(sorted)
        }
        _ => value.clone(),
    }
}

fn native_canonical_digest(value: &Value) -> String {
    let mut hasher = Sha256::new();
    hasher.update(
        serde_json::to_vec(&native_canonical_value(value))
            .unwrap_or_default(),
    );
    format!("{:x}", hasher.finalize())
}

fn native_node_platform() -> &'static str {
    if cfg!(windows) {
        "win32"
    } else if cfg!(target_os = "macos") {
        "darwin"
    } else if cfg!(target_os = "linux") {
        "linux"
    } else {
        env::consts::OS
    }
}

fn native_site_id(root: &Path) -> String {
    if let Ok(value) = env::var("NARADA_SITE_ID") {
        if !value.trim().is_empty() {
            return value;
        }
    }
    root.file_name()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_string()
}

fn native_environment(root: &Path) -> Value {
    let mut environment = json!({
        "schema": "narada.task_executability_declared_environment.v1",
        "site_id": native_site_id(root),
        "substrate": native_node_platform(),
        "declared_tools": [],
        "declared_authority": []
    });
    if let Ok(variant) = env::var("NARADA_SUBSTRATE_VARIANT") {
        if !variant.trim().is_empty() {
            if let Some(object) = environment.as_object_mut() {
                object.insert("variant".to_string(), Value::String(variant));
            }
        }
    }
    environment
}

fn native_environment_digest(root: &Path) -> String {
    let environment = native_environment(root);
    native_canonical_digest(&json!({
        "kind": "declared_environment",
        "site_id": environment.get("site_id"),
        "substrate": environment.get("substrate"),
        "variant": environment.get("variant"),
        "declared_tools": environment.get("declared_tools"),
        "declared_authority": environment.get("declared_authority")
    }))
}

fn native_policy(root: &Path) -> Result<(String, Value), String> {
    let defaults = [
        ("trigger", "manual"),
        ("enforcement", "off"),
        ("evaluator_profile", "shoshin-v1"),
    ];
    let mut values = Map::new();
    let mut provenance = Vec::new();
    let loci = [
        ("target_site", root.to_path_buf()),
        (
            "user_site",
            env::var_os("NARADA_USER_SITE_ROOT")
                .map(PathBuf::from)
                .unwrap_or_default(),
        ),
        (
            "host_site",
            env::var_os("NARADA_HOST_SITE_ROOT")
                .map(PathBuf::from)
                .unwrap_or_default(),
        ),
    ];
    for (field, default_value) in defaults {
        let mut selected: Option<(String, String, String)> = None;
        for (source, locus) in &loci {
            if locus.as_os_str().is_empty() {
                continue;
            }
            let path = locus.join(".ai").join("task-executability-policy.json");
            if !path.exists() {
                continue;
            }
            let text = fs::read_to_string(&path)
                .map_err(|_| format!("task_executability_policy_invalid:{source}:read:{path:?}"))?;
            let document: Value = serde_json::from_str(&text).map_err(|_| {
                format!("task_executability_policy_invalid:{source}:not_json:{path:?}")
            })?;
            let object = document.as_object().ok_or_else(|| {
                format!("task_executability_policy_invalid:{source}:not_object:{path:?}")
            })?;
            if object.get("schema").and_then(Value::as_str)
                != Some("narada.task_executability_policy.v1")
            {
                return Err(format!(
                    "task_executability_policy_invalid:{source}:policy_schema_mismatch:{path:?}"
                ));
            }
            for forbidden in [
                "provider",
                "model",
                "provider_id",
                "model_id",
                "reasoning_effort",
                "cognition",
            ] {
                if object.contains_key(forbidden) {
                    return Err(format!(
                        "task_executability_policy_invalid:{source}:policy_field_forbidden:{forbidden}:{path:?}"
                    ));
                }
            }
            if let Some(candidate) = object.get(field) {
                let value = candidate.as_str().ok_or_else(|| {
                    format!(
                        "task_executability_policy_invalid:{source}:{field}_invalid:{path:?}"
                    )
                })?;
                if field == "trigger" && !matches!(value, "manual" | "on_create") {
                    return Err(format!(
                        "task_executability_policy_invalid:{source}:policy_trigger_invalid:{path:?}"
                    ));
                }
                if field == "enforcement" && !matches!(value, "off" | "warn" | "strict") {
                    return Err(format!(
                        "task_executability_policy_invalid:{source}:policy_enforcement_invalid:{path:?}"
                    ));
                }
                if value.trim().is_empty() {
                    return Err(format!(
                        "task_executability_policy_invalid:{source}:{field}_invalid:{path:?}"
                    ));
                }
                selected = Some((
                    value.to_string(),
                    source.to_string(),
                    path.to_string_lossy().to_string(),
                ));
                break;
            }
        }
        let (value, source, source_ref) = selected.unwrap_or_else(|| {
            (
                default_value.to_string(),
                "product_default".to_string(),
                "product-defaults".to_string(),
            )
        });
        values.insert(field.to_string(), Value::String(value.clone()));
        provenance.push(json!({
            "field": field,
            "value": value,
            "source": source,
            "source_ref": source_ref
        }));
    }
    let profile = values
        .get("evaluator_profile")
        .and_then(Value::as_str)
        .unwrap_or("shoshin-v1")
        .to_string();
    Ok((
        profile,
        json!({
            "schema": "narada.task_executability_resolved_policy.v1",
            "trigger": values.get("trigger"),
            "enforcement": values.get("enforcement"),
            "evaluator_profile": values.get("evaluator_profile"),
            "provenance": provenance
        }),
    ))
}

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

    fn task_executability_status(&self, args: Value) -> Result<Value, String> {
        let task_number = required_i64(&args, "task_number")?;
        let (task_id, _title, task_spec_digest, environment_digest) =
            native_task_executability_context(self, task_number)?;
        let (_, policy) = native_policy(&self.options.site_root)?;
        let request = self.query_one(
            "select request_id,task_id,task_number,state,task_spec_digest,
                    environment_digest,evaluator_profile,evaluator_profile_version,
                    assessment_id,lease_owner,lease_expires_at,attempt_count,
                    superseded_by_request_id,created_at,updated_at
               from task_executability_requests
              where task_id=?1 order by created_at desc,rowid desc limit 1",
            params![&task_id],
        )?;
        let Some(request) = request else {
            return Ok(json!({
                "schema":"narada.task_executability.status.v0",
                "status":"ok",
                "task_number":task_number,
                "task_id":task_id,
                "executable":false,
                "currency":"stale",
                "verdict":null,
                "reason":"No executability request has been enqueued for this task.",
                "policy":policy,
                "request":null,
                "assessment":null,
                "assessment_detail":null,
                "findings":null
            }));
        };
        let assessment_id = request
            .get("assessment_id")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let assessment = if assessment_id.is_empty() {
            None
        } else {
            self.query_one(
                "select assessment_id,request_id,task_id,task_number,
                        task_spec_digest,environment_digest,verdict,
                        findings_json,evaluator_json,created_at
                   from task_executability_assessments
                  where assessment_id=?1",
                params![assessment_id],
            )?
        };
        let superseded = request
            .get("superseded_by_request_id")
            .and_then(Value::as_str)
            .is_some_and(|value| !value.is_empty());
        let current = !superseded
            && assessment.as_ref().is_some_and(|row| {
                row.get("task_spec_digest").and_then(Value::as_str)
                    == Some(task_spec_digest.as_str())
                    && row.get("environment_digest").and_then(Value::as_str)
                        == Some(environment_digest.as_str())
            });
        let currency = if superseded {
            "superseded"
        } else if current {
            "current"
        } else {
            "stale"
        };
        let verdict = if current {
            assessment
                .as_ref()
                .and_then(|row| row.get("verdict"))
                .cloned()
                .unwrap_or(Value::Null)
        } else {
            Value::Null
        };
        let executable = current && verdict.as_str() == Some("executable");
        let reason = if executable {
            "Current assessment verdict is executable."
        } else if superseded {
            "Request was superseded by a newer request."
        } else if assessment.is_none() {
            "No current executable assessment is available."
        } else {
            "Assessment is stale because the task spec or environment has changed."
        };
        let request_projection = json!({
            "request_id":request.get("request_id"),
            "state":request.get("state"),
            "task_spec_digest":request.get("task_spec_digest"),
            "environment_digest":request.get("environment_digest"),
            "attempt_count":request.get("attempt_count"),
            "lease_owner":request.get("lease_owner"),
            "lease_expires_at":request.get("lease_expires_at"),
            "superseded_by_request_id":request.get("superseded_by_request_id"),
            "created_at":request.get("created_at"),
            "updated_at":request.get("updated_at")
        });
        let assessment_projection = assessment.as_ref().map(|row| json!({
            "assessment_id":row.get("assessment_id"),
            "request_id":row.get("request_id"),
            "verdict":row.get("verdict"),
            "task_spec_digest":row.get("task_spec_digest"),
            "environment_digest":row.get("environment_digest"),
            "created_at":row.get("created_at")
        })).unwrap_or(Value::Null);
        let assessment_detail = if args.get("include_assessment").and_then(Value::as_bool) == Some(true) {
            assessment.as_ref().map(|row| {
                let findings = row.get("findings_json")
                    .and_then(Value::as_str)
                    .and_then(|value| serde_json::from_str::<Value>(value).ok())
                    .filter(|value| value.is_array())
                    .unwrap_or_else(|| json!([]));
                let evaluator = row.get("evaluator_json")
                    .and_then(Value::as_str)
                    .and_then(|value| serde_json::from_str::<Value>(value).ok())
                    .filter(|value| value.is_object())
                    .unwrap_or_else(|| json!({}));
                json!({
                    "schema":"narada.task_executability_assessment.v1",
                    "assessment_id":row.get("assessment_id"),
                    "request_id":row.get("request_id"),
                    "task_id":row.get("task_id"),
                    "task_number":row.get("task_number"),
                    "task_spec_digest":row.get("task_spec_digest"),
                    "environment_digest":row.get("environment_digest"),
                    "verdict":row.get("verdict"),
                    "findings":findings,
                    "evaluator":evaluator,
                    "created_at":row.get("created_at")
                })
            }).unwrap_or(Value::Null)
        } else {
            Value::Null
        };
        let findings = assessment.as_ref().and_then(|row| {
            row.get("findings_json")
                .and_then(Value::as_str)
                .and_then(|value| serde_json::from_str::<Value>(value).ok())
        }).unwrap_or(Value::Null);
        Ok(json!({
            "schema":"narada.task_executability.status.v0",
            "status":"ok",
            "task_number":task_number,
            "task_id":task_id,
            "executable":executable,
            "currency":currency,
            "verdict":verdict,
            "reason":reason,
            "policy":policy,
            "request":request_projection,
            "assessment":assessment_projection,
            "assessment_detail":assessment_detail,
            "findings":findings
        }))
    }

    fn task_executability_requests_next(&mut self, args: Value) -> Result<Value, String> {
        let consumer_id = required_string(&args, "consumer_id")?;
        let lease_minutes = args
            .get("lease_duration_minutes")
            .and_then(Value::as_i64)
            .unwrap_or(10)
            .clamp(1, 120);
        let limit = args
            .get("limit")
            .and_then(Value::as_i64)
            .unwrap_or(1)
            .clamp(1, 20);
        let now_value = now();
        let candidates = self.query_objects(
            "select request_id,state,lease_expires_at,lease_owner
               from task_executability_requests
              where superseded_by_request_id is null
                and (state='pending'
                  or (state in ('leased','failed_retryable','dispatched')
                      and lease_expires_at < ?1)
                  or (state='dispatched' and lease_owner=?2 and lease_expires_at >= ?1))
              order by created_at asc,rowid asc limit ?3",
            params![&now_value, &consumer_id, limit],
        )?;
        let mut leased = Vec::new();
        for candidate in candidates {
            let request_id = candidate
                .get("request_id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            if request_id.is_empty() {
                continue;
            }
            let lease_expires_at = native_lease_expiry(lease_minutes);
            let leased_now = {
                let connection = self.connection_mut()?;
                let changes = connection
                    .execute(
                        "update task_executability_requests
                            set state='leased',lease_owner=?1,lease_expires_at=?2,
                                attempt_count=attempt_count+1,updated_at=?3
                          where request_id=?4
                            and (state='pending'
                              or (state in ('leased','failed_retryable','dispatched')
                                  and lease_expires_at < ?3))",
                        params![
                            &consumer_id,
                            &lease_expires_at,
                            &now_value,
                            &request_id
                        ],
                    )
                    .map_err(db_error)?;
                if changes == 0 {
                    false
                } else {
                    let attempt_id = native_canonical_digest(&json!({
                        "kind":"attempt",
                        "request_id":request_id,
                        "actor":consumer_id,
                        "leased_at":now_value
                    }));
                    let attempt_id = format!("texm_{}", &attempt_id[..32]);
                    connection
                        .execute(
                            "insert into task_executability_attempts
                             (attempt_id,request_id,actor,leased_at,lease_expires_at,
                              state,delegated_task_id,worker_run_id,error_json,created_at)
                             values(?1,?2,?3,?4,?5,'leased',null,null,null,?4)",
                            params![
                                &attempt_id,
                                &request_id,
                                &consumer_id,
                                &now_value,
                                &lease_expires_at
                            ],
                        )
                        .map_err(db_error)?;
                    true
                }
            };
            if !leased_now {
                continue;
            }
            let row = self.query_one(
                "select request_id,task_number,task_id,task_spec_digest,
                        environment_digest,evaluator_profile,
                        evaluator_profile_version,lease_expires_at
                   from task_executability_requests where request_id=?1",
                params![&request_id],
            )?;
            let Some(row) = row else { continue };
            let title: Option<String> = self
                .connection()?
                .query_row(
                    "select title from task_specs where task_id=?1",
                    params![row.get("task_id").and_then(Value::as_str).unwrap_or_default()],
                    |r| r.get(0),
                )
                .optional()
                .map_err(db_error)?;
            leased.push(json!({
                "request_id":row.get("request_id"),
                "task_number":row.get("task_number"),
                "task_id":row.get("task_id"),
                "task_spec_digest":row.get("task_spec_digest"),
                "environment_digest":row.get("environment_digest"),
                "evaluator_profile":row.get("evaluator_profile"),
                "evaluator_profile_version":row.get("evaluator_profile_version"),
                "lease_expires_at":row.get("lease_expires_at"),
                "title":title
            }));
        }
        Ok(json!({
            "schema":"narada.task_executability.requests_next.v0",
            "status":if leased.is_empty() {"empty"} else {"leased"},
            "consumer_id":consumer_id,
            "lease_duration_minutes":lease_minutes,
            "leased_count":leased.len(),
            "leased":leased
        }))
    }

    fn task_executability_complete(&mut self, args: Value) -> Result<Value, String> {
        let request_id = required_string(&args, "request_id")?;
        let assessment = args
            .get("assessment")
            .filter(|value| value.is_object())
            .ok_or("assessment_required")?
            .clone();
        let request = self.query_one(
            "select request_id,task_id,task_number,state,task_spec_digest,
                    environment_digest,evaluator_profile,evaluator_profile_version
               from task_executability_requests where request_id=?1",
            params![&request_id],
        )?;
        let Some(request) = request else {
            return Ok(json!({
                "schema":"narada.task_executability.complete.v0",
                "status":"failed",
                "reason":format!("task_executability_request_not_found:{request_id}"),
                "request_id":request_id
            }));
        };
        let fail = |reason: String| {
            json!({
                "schema":"narada.task_executability.complete.v0",
                "status":"failed",
                "reason":reason,
                "request_id":request_id
            })
        };
        let task_id = assessment.get("task_id").and_then(Value::as_str);
        let task_number = assessment.get("task_number").and_then(Value::as_i64);
        let task_spec_digest = assessment
            .get("task_spec_digest")
            .and_then(Value::as_str);
        let environment_digest = assessment
            .get("environment_digest")
            .and_then(Value::as_str);
        let verdict = assessment.get("verdict").and_then(Value::as_str);
        let findings = assessment
            .get("findings")
            .filter(|value| value.is_array())
            .cloned();
        let evaluator = assessment
            .get("evaluator")
            .filter(|value| value.is_object())
            .cloned();
        let created_at = assessment.get("created_at").and_then(Value::as_str);
        let Some(task_id) = task_id else { return Ok(fail("task_executability_assessment_invalid:task_id_required".to_string())) };
        let Some(task_number) = task_number else { return Ok(fail("task_executability_assessment_invalid:task_number_required".to_string())) };
        let Some(task_spec_digest) = task_spec_digest else { return Ok(fail("task_executability_assessment_invalid:task_spec_digest_required".to_string())) };
        let Some(environment_digest) = environment_digest else { return Ok(fail("task_executability_assessment_invalid:environment_digest_required".to_string())) };
        let Some(verdict) = verdict else { return Ok(fail("task_executability_assessment_invalid:verdict_required".to_string())) };
        let Some(findings) = findings else { return Ok(fail("task_executability_assessment_invalid:findings_required".to_string())) };
        let Some(evaluator) = evaluator else { return Ok(fail("task_executability_assessment_invalid:evaluator_required".to_string())) };
        let Some(created_at) = created_at else { return Ok(fail("task_executability_assessment_invalid:created_at_required".to_string())) };
        let evaluator_profile = evaluator.get("profile").and_then(Value::as_str);
        let evaluator_version = evaluator.get("profile_version").and_then(Value::as_str);
        let cognition = evaluator.get("cognition").and_then(Value::as_str);
        if evaluator_profile.is_none() || evaluator_version.is_none() || cognition != Some("low") {
            return Ok(fail("task_executability_assessment_invalid:evaluator_provenance_invalid".to_string()));
        }
        if request.get("task_id").and_then(Value::as_str) != Some(task_id)
            || request.get("task_number").and_then(Value::as_i64) != Some(task_number)
        {
            return Ok(fail("task_executability_assessment_task_identity_mismatch".to_string()));
        }
        if request.get("task_spec_digest").and_then(Value::as_str) != Some(task_spec_digest)
            || request.get("environment_digest").and_then(Value::as_str)
                != Some(environment_digest)
        {
            return Ok(fail("task_executability_assessment_digest_mismatch".to_string()));
        }
        if request.get("evaluator_profile").and_then(Value::as_str) != evaluator_profile
            || request.get("evaluator_profile_version").and_then(Value::as_str)
                != evaluator_version
        {
            return Ok(fail("task_executability_assessment_profile_mismatch".to_string()));
        }
        let derived = native_verdict(&findings);
        if derived != verdict {
            return Ok(fail(format!(
                "task_executability_assessment_verdict_mismatch:derived={derived}:claimed={verdict}"
            )));
        }
        if !matches!(verdict, "executable" | "needs_revision" | "not_executable") {
            return Ok(fail("task_executability_assessment_invalid:verdict_invalid".to_string()));
        }
        let assessment_id = assessment
            .get("assessment_id")
            .and_then(Value::as_str)
            .map(ToString::to_string)
            .unwrap_or_else(|| native_assessment_id(&request_id, created_at));
        let connection = self.connection_mut()?;
        connection
            .execute(
                "insert into task_executability_assessments
                 (assessment_id,request_id,task_id,task_number,task_spec_digest,
                  environment_digest,verdict,findings_json,evaluator_json,created_at)
                 values(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)
                 on conflict(assessment_id) do update set
                   request_id=excluded.request_id,task_id=excluded.task_id,
                   task_number=excluded.task_number,
                   task_spec_digest=excluded.task_spec_digest,
                   environment_digest=excluded.environment_digest,
                   verdict=excluded.verdict,findings_json=excluded.findings_json,
                   evaluator_json=excluded.evaluator_json,created_at=excluded.created_at",
                params![
                    &assessment_id,
                    &request_id,
                    task_id,
                    task_number,
                    task_spec_digest,
                    environment_digest,
                    verdict,
                    findings.to_string(),
                    evaluator.to_string(),
                    created_at
                ],
            )
            .map_err(db_error)?;
        connection
            .execute(
                "update task_executability_requests
                    set state='completed',assessment_id=?1,updated_at=?2
                  where request_id=?3",
                params![&assessment_id, now(), &request_id],
            )
            .map_err(db_error)?;
        Ok(json!({
            "schema":"narada.task_executability.complete.v0",
            "status":"completed",
            "assessment_id":assessment_id,
            "request_id":request_id,
            "task_number":task_number,
            "task_id":task_id,
            "verdict":verdict
        }))
    }

    fn task_executability_override(&mut self, args: Value) -> Result<Value, String> {
        let task_number = required_i64(&args, "task_number")?;
        let actor = required_string(&args, "agent_id")?;
        let reason = required_string(&args, "reason")?;
        let basis = args
            .get("authority_basis")
            .filter(|value| value.is_object())
            .ok_or("override_authority_basis_required")?;
        let kind = basis
            .get("kind")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .ok_or("override_authority_basis_requires_kind_and_summary")?;
        let summary = basis
            .get("summary")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .ok_or("override_authority_basis_requires_kind_and_summary")?;
        let (task_id, _title, task_spec_digest, environment_digest) =
            native_task_executability_context(self, task_number)?;
        let dispatch_fingerprint = args
            .get("dispatch_fingerprint")
            .and_then(Value::as_str)
            .map(ToString::to_string)
            .unwrap_or_else(|| {
                native_dispatch_fingerprint(&task_id, &task_spec_digest, &environment_digest, ".")
            });
        let created_at = now();
        let override_id = native_override_id(
            &task_id,
            &dispatch_fingerprint,
            &actor,
            &created_at,
        );
        let connection = self.connection_mut()?;
        connection
            .execute(
                "insert into task_executability_overrides
                 (override_id,task_id,task_spec_digest,dispatch_fingerprint,
                  actor,reason,authority_basis_json,created_at,consumed_at)
                 values(?1,?2,?3,?4,?5,?6,?7,?8,null)
                 on conflict(override_id) do update set
                   task_id=excluded.task_id,task_spec_digest=excluded.task_spec_digest,
                   dispatch_fingerprint=excluded.dispatch_fingerprint,actor=excluded.actor,
                   reason=excluded.reason,authority_basis_json=excluded.authority_basis_json,
                   created_at=excluded.created_at,consumed_at=excluded.consumed_at",
                params![
                    &override_id,
                    &task_id,
                    &task_spec_digest,
                    &dispatch_fingerprint,
                    &actor,
                    &reason,
                    json!({"kind":kind,"summary":summary}).to_string(),
                    &created_at
                ],
            )
            .map_err(db_error)?;
        Ok(json!({
            "schema":"narada.task_executability.override.v0",
            "status":"admitted",
            "task_number":task_number,
            "task_id":task_id,
            "override_id":override_id,
            "task_spec_digest":task_spec_digest,
            "environment_digest":environment_digest,
            "dispatch_fingerprint":dispatch_fingerprint,
            "actor":actor,
            "reason":reason,
            "authority_basis":{"kind":kind,"summary":summary},
            "consumed_at":null
        }))
    }

    fn task_executability_dispatch_check(&mut self, args: Value) -> Result<Value, String> {
        let task_number = required_i64(&args, "task_number")?;
        let (task_id, _title, task_spec_digest, environment_digest) =
            native_task_executability_context(self, task_number)?;
        let dispatch_fingerprint = args
            .get("dispatch_fingerprint")
            .and_then(Value::as_str)
            .map(ToString::to_string)
            .unwrap_or_else(|| {
                native_dispatch_fingerprint(&task_id, &task_spec_digest, &environment_digest, ".")
            });
        let latest = self.query_one(
            "select assessment_id,superseded_by_request_id
               from task_executability_requests
              where task_id=?1 order by created_at desc,rowid desc limit 1",
            params![&task_id],
        )?;
        let assessment = latest
            .as_ref()
            .and_then(|row| {
                if row
                    .get("superseded_by_request_id")
                    .and_then(Value::as_str)
                    .is_some_and(|value| !value.is_empty())
                {
                    None
                } else {
                    row.get("assessment_id").and_then(Value::as_str)
                }
            })
            .map(|id| id.to_string())
            .map(|id| {
                self.query_one(
                    "select assessment_id,task_spec_digest,environment_digest,verdict
                       from task_executability_assessments where assessment_id=?1",
                    params![id],
                )
            })
            .transpose()?.flatten();
        let assessment_current = assessment.as_ref().is_some_and(|row| {
            row.get("task_spec_digest").and_then(Value::as_str)
                == Some(task_spec_digest.as_str())
                && row.get("environment_digest").and_then(Value::as_str)
                    == Some(environment_digest.as_str())
                && row.get("verdict").and_then(Value::as_str) == Some("executable")
        });
        if assessment_current {
            return Ok(json!({
                "schema":"narada.task_executability.dispatch_check.v0",
                "executable":true,
                "basis":"assessment",
                "assessment_id":assessment.as_ref().and_then(|row|row.get("assessment_id")),
                "override_consumed":false,
                "dispatch_fingerprint":dispatch_fingerprint,
                "task_spec_digest":task_spec_digest,
                "environment_digest":environment_digest
            }));
        }
        let match_id: Option<String> = self
            .connection()?
            .query_row(
                "select override_id from task_executability_overrides
                  where task_id=?1 and task_spec_digest=?2
                    and dispatch_fingerprint=?3 and consumed_at is null
                  order by created_at asc,rowid asc limit 1",
                params![&task_id, &task_spec_digest, &dispatch_fingerprint],
                |row| row.get(0),
            )
            .optional()
            .map_err(db_error)?;
        if let Some(override_id) = match_id {
            self.connection_mut()?
                .execute(
                    "update task_executability_overrides set consumed_at=?1 where override_id=?2 and consumed_at is null",
                    params![now(), &override_id],
                )
                .map_err(db_error)?;
            return Ok(json!({
                "schema":"narada.task_executability.dispatch_check.v0",
                "executable":true,
                "basis":"override",
                "assessment_id":assessment.as_ref().and_then(|row|row.get("assessment_id")).cloned().unwrap_or(Value::Null),
                "override_consumed":true,
                "dispatch_fingerprint":dispatch_fingerprint,
                "task_spec_digest":task_spec_digest,
                "environment_digest":environment_digest
            }));
        }
        Ok(json!({
            "schema":"narada.task_executability.dispatch_check.v0",
            "executable":false,
            "basis":"none",
            "assessment_id":assessment.as_ref().and_then(|row|row.get("assessment_id")).cloned().unwrap_or(Value::Null),
            "override_consumed":false,
            "dispatch_fingerprint":dispatch_fingerprint,
            "task_spec_digest":task_spec_digest,
            "environment_digest":environment_digest
        }))
    }

    fn task_test_mcp_tool(&self, args: Value) -> Result<Value, String> {
        let server_path = required_string(&args, "server_path")?;
        let tool_name = required_string(&args, "tool_name")?;
        Ok(json!({
            "schema":"narada.task_lifecycle.fresh_server_path_admission.v1",
            "status":"refused",
            "error":"native_lifecycle_external_mcp_test_requires_explicit_native_child_adapter",
            "server_path":server_path,
            "tool_name":tool_name,
            "remediation":"Use the native protocol test harness or an explicitly admitted native MCP child; the lifecycle authority will not launch an implicit Node/Bun child."
        }))
    }

    fn task_run_tests(&self, args: Value) -> Result<Value, String> {
        let selector = args
            .get("selector")
            .and_then(Value::as_str)
            .unwrap_or("task-lifecycle");
        let task_number = args.get("task_number").and_then(Value::as_i64);
        let agent_id = args
            .get("agent_id")
            .and_then(Value::as_str)
            .unwrap_or("");
        if agent_id.is_empty() {
            return Err("agent_id_required".to_string());
        }
        Ok(json!({
            "schema":"narada.task_lifecycle.run_tests.v0",
            "status":"blocked",
            "error":"native_lifecycle_external_test_mcp_not_configured",
            "selector":selector,
            "task_number":task_number,
            "task_id":null,
            "agent_id":agent_id,
            "total":0,
            "passed":0,
            "failed":0,
            "results":[],
            "remediation":"Run the native Rust parity/refusal suite directly and submit its structured result as evidence; no Node/Bun test child is launched by the native lifecycle surface."
        }))
    }
}
