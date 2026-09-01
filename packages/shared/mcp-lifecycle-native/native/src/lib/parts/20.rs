fn normalize_task_tool_name(name: &str) -> &str {
    match name {
        "task_lifecycle_closeout" => "task_lifecycle_disposition_closeout",
        "task_lifecycle_record_observation" => "task_lifecycle_submit_observation",
        "task_lifecycle_submit_report" => "task_lifecycle_finish",
        "task_lifecycle_d_af077406ea2f" => "task_lifecycle_disposition_closeout",
        "task_lifecycle_s_f5e0b1532dcf" => "task_lifecycle_submit_observation",
        "task_mcp_doctor" => "task_lifecycle_doctor",
        "task_mcp_restart" => "task_lifecycle_restart",
        "task_mcp_list" => "task_lifecycle_list",
        "task_mcp_show" => "task_lifecycle_show",
        "task_mcp_roster" => "task_lifecycle_roster",
        "task_mcp_roster_admit" => "task_lifecycle_roster_admit",
        "task_mcp_claim" => "task_lifecycle_claim",
        "task_mcp_continue" => "task_lifecycle_continue",
        "task_mcp_unclaim" => "task_lifecycle_unclaim",
        "task_mcp_next" => "task_lifecycle_next",
        "task_mcp_workboard_snapshot" => "task_lifecycle_workboard_snapshot",
        "task_mcp_obligations" => "task_lifecycle_obligations",
        "task_mcp_inspect" => "task_lifecycle_inspect",
        "task_mcp_evidence_preflight" => "task_lifecycle_evidence_preflight",
        "task_mcp_admit_evidence" => "task_lifecycle_admit_evidence",
        "task_mcp_prove_criteria" => "task_lifecycle_prove_criteria",
        "task_mcp_audit" => "task_lifecycle_audit",
        "task_mcp_finish" => "task_lifecycle_finish",
        "task_mcp_close" => "task_lifecycle_close",
        "task_mcp_search" => "task_lifecycle_search",
        "task_mcp_defer" => "task_lifecycle_defer",
        "task_mcp_un_defer" | "task_mcp_undefer" => "task_lifecycle_un_defer",
        "task_mcp_reopen" => "task_lifecycle_reopen",
        "task_mcp_review" => "task_lifecycle_review",
        "task_mcp_submit_observation" => "task_lifecycle_submit_observation",
        "task_mcp_bridge_poll" => "task_lifecycle_bridge_poll",
        "task_mcp_inbox_target" => "task_lifecycle_inbox_target",
        "task_mcp_create" => "task_lifecycle_create",
        "task_mcp_set_routing" => "task_lifecycle_set_routing",
        "task_mcp_tags_update" => "task_lifecycle_tags_update",
        "task_mcp_test_tool" => "task_lifecycle_test_mcp_tool",
        "task_mcp_run_tests" => "task_lifecycle_run_tests",
        _ => name,
    }
}fn is_locus_guarded_mutation(name: &str) -> bool {
    matches!(
        name,
        "task_lifecycle_claim"
            | "task_lifecycle_continue"
            | "task_lifecycle_unclaim"
            | "task_lifecycle_admit_evidence"
            | "task_lifecycle_prove_criteria"
            | "task_lifecycle_finish"
            | "task_lifecycle_submit_work"
            | "task_lifecycle_report_blocked"
            | "task_lifecycle_close"
            | "task_lifecycle_defer"
            | "task_lifecycle_un_defer"
            | "task_lifecycle_reopen"
            | "task_lifecycle_review"
            | "task_lifecycle_submit_observation"
            | "task_lifecycle_evidence_supersede"
            | "task_lifecycle_bridge_poll"
            | "task_lifecycle_inbox_target"
            | "task_lifecycle_create"
            | "task_lifecycle_tags_update"
            | "task_lifecycle_set_routing"
            | "task_lifecycle_dependency_declare"
            | "task_lifecycle_dependency_dispose"
            | "task_lifecycle_dependency_disposition_record"
            | "task_lifecycle_compatibility_reconcile"
            | "task_lifecycle_recurring_create"
            | "task_lifecycle_recurring_run_due"
            | "task_lifecycle_recurring_suspend"
            | "task_lifecycle_recurring_retire"
    )
}
fn is_task_read_only(name: &str) -> bool {
    matches!(
        name,
        "task_lifecycle_list"
            | "task_lifecycle_show"
            | "task_lifecycle_roster"
            | "task_lifecycle_guidance"
            | "task_lifecycle_payload_schema"
            | "task_lifecycle_evidence_preflight"
            | "task_lifecycle_self_certification_preflight"
            | "task_lifecycle_next"
            | "task_lifecycle_workboard_snapshot"
            | "task_lifecycle_obligations"
            | "task_lifecycle_inspect"
            | "task_lifecycle_inspect_range"
            | "task_lifecycle_audit"
            | "task_lifecycle_search"
            | "task_lifecycle_related"
            | "task_lifecycle_recurring_list"
            | "task_lifecycle_recurring_show"
            | "task_lifecycle_recurring_runs"
            | "task_lifecycle_diagnose_task_ref"
            | "mcp_payload_show"
            | "mcp_payload_validate"
            | "mcp_output_show"
    )
}
fn task_lifecycle_tool_guidance(tool: &str) -> Value {
    match tool {
        "task_lifecycle_submit_work" => json!({
            "preferred_for": "Ordinary task completion with execution notes, verification, evidence admission, and finish/report in one call.",
            "caveat": "A successful submit_work can still return in_review or awaiting_dependencies rather than closed. Use resume_existing_work:true only to continue prior same-agent admitted work without rewriting notes or duplicating proof/admission. Inline companion fields are accepted up to the governed threshold; use payload_ref or opt in with auto_materialize_payload:true for larger artifacts."
        }),
        "task_lifecycle_finish" => json!({
            "preferred_for": "Finishing a claimed task or admitting an outcome for an outcome-contract dependency task.",
            "caveat": "Use inline recovery_truthfulness for ordinary recovery packets under the governed threshold; use payload_ref for larger summary/findings/guard packets and include changed_files or no_files_changed for implementation work. When evidence_refs are supplied, use a successful structured_command_execution:<execution_ref> or test_mcp_artifact:<artifact_id>; the finish gate dereferences and verifies those refs. mcp_output refs are diagnostic only, and copied logs, exit files, wrappers, transient paths, and untyped narrative refs are refused."
        }),
        "task_lifecycle_report_blocked" => json!({
            "preferred_for": "Recording unresolved blockers with exact next action.",
            "caveat": "Do not use completion tools when the blocker prevents truthful finish."
        }),
        "task_lifecycle_claim" => json!({
            "preferred_for": "Taking responsibility for unassigned work.",
            "caveat": "Use authority_basis when crossing role, preferred-agent, or operator gates."
        }),
        "task_lifecycle_tags_update" => json!({
            "preferred_for": "Replacing a task's complete site-local tag set with an auditable before/after record.",
            "caveat": "Pass the complete desired set, including [] to clear tags. Tags do not route, prioritize, authorize, review, or close work."
        }),
        _ => json!({
            "preferred_for": Value::Null,
            "caveat": "No tool-specific guidance is registered; use the workflow sections and tool schema together."
        }),
    }
}
fn guidance_payload(site_root: &Path, args: Value) -> Value {
    let mut result: Value =
        serde_json::from_str(TASK_GUIDANCE).expect("checked-in task guidance must be valid JSON");
    let requested_workflow = string_arg(&args, "workflow").unwrap_or_else(|| "all".to_string());
    let tool = string_arg(&args, "tool");
    let normalized_workflow = result
        .get("sections")
        .and_then(Value::as_object)
        .filter(|sections| sections.contains_key(&requested_workflow))
        .map(|_| requested_workflow.clone())
        .unwrap_or_else(|| "all".to_string());
    result["requested"] = json!({"workflow": requested_workflow, "tool": tool});
    result["workflow"] = json!(normalized_workflow);
    result["tool"] = json!(string_arg(&args, "tool"));
    if normalized_workflow != "all" {
        let selected = result
            .get("sections")
            .and_then(Value::as_object)
            .and_then(|sections| sections.get(&normalized_workflow))
            .cloned()
            .unwrap_or(Value::Null);
        result["sections"] = json!({normalized_workflow: selected});
    }
    result["site_policy"] = json!({
        "roster": {"roles_are_obligation_targets": false},
        "source": "default",
        "path": site_root.join(".narada").join("task-lifecycle.toml").to_string_lossy()
    });
    result["recommended_first_call"] = if tool.is_some() {
        Value::Null
    } else {
        json!("task_lifecycle_guidance({ workflow: \"ordinary_task\" })")
    };
    result["tool_specific_note"] = match tool.as_deref() {
        Some(tool_name) => task_lifecycle_tool_guidance(tool_name),
        None => Value::Null,
    };
    let detail = string_arg(&args, "detail").unwrap_or_else(|| "compact".to_string());
    result["detail"] = json!(detail);
    if detail != "full" {
        let keep = ["status","schema","common_guidance_contract_schema","surface_id","guidance_tool","purpose","requested","workflow","tool","first_use","sections","site_policy","recommended_first_call","tool_specific_note","detail"];
        if let Some(object) = result.as_object_mut() { object.retain(|key, _| keep.contains(&key.as_str())); }
    }
    result
}

fn payload_schema_payload(args: Value) -> Result<Value, String> {
    let mut result: Value = serde_json::from_str(TASK_PAYLOAD_SCHEMAS)
        .map_err(|error| format!("task_payload_schema_catalog_invalid:{error}"))?;
    let tool = string_arg(&args, "tool");
    result["tool"] = json!(tool);
    if let Some(tool_name) = tool {
        let selected = result
            .get("schemas")
            .and_then(Value::as_object)
            .and_then(|schemas| schemas.get(&tool_name))
            .cloned()
            .unwrap_or(Value::Null);
        result["schemas"] = json!({tool_name: selected});
    }
    Ok(result)
}

pub struct WireReader<R> {
    reader: R,
    buffer: Vec<u8>,
    eof: bool,
}
impl<R: Read> WireReader<R> {
    pub fn new(reader: R) -> Self {
        Self {
            reader,
            buffer: Vec::new(),
            eof: false,
        }
    }
    pub fn next(&mut self) -> io::Result<Option<(Value, bool)>> {
        loop {
            if let Some(v) = try_parse_wire(&mut self.buffer)? {
                return Ok(Some(v));
            }
            if self.eof {
                if self.buffer.iter().all(|b| b.is_ascii_whitespace()) {
                    self.buffer.clear();
                    return Ok(None);
                }
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "incomplete MCP message",
                ));
            }
            let mut chunk = [0u8; 8192];
            let n = self.reader.read(&mut chunk)?;
            if n == 0 {
                self.eof = true
            } else {
                self.buffer.extend_from_slice(&chunk[..n]);
            }
        }
    }
}
