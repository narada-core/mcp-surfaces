impl LifecycleServer {
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

}
