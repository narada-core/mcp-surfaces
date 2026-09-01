impl LifecycleServer {
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
