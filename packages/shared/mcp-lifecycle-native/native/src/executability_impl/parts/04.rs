impl LifecycleServer {
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

}
