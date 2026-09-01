impl LifecycleServer {
    fn call_tool(&mut self, name: &str, args: Value) -> Result<Value, String> {
        let name = normalize_task_tool_name(name);

        if self.options.surface == Surface::Task
            && self.connection.is_none()
            && !matches!(
                name,
                "task_lifecycle_doctor"
                    | "task_lifecycle_guidance"
                    | "task_lifecycle_payload_schema"
                    | "task_lifecycle_restart"
                    | "task_lifecycle_chapter_show"
                    | "mcp_payload_create"
                    | "mcp_payload_show"
                    | "mcp_payload_derive"
                    | "mcp_payload_validate"
                    | "mcp_output_show"
            )
        {
            let reason = inspect_database(&self.options)
                .ok()
                .and_then(|value| value.get("reason").and_then(Value::as_str).map(ToString::to_string))
                .unwrap_or_else(|| "database_missing".to_string());
            return Err(format!(
                "{}_store_not_prepared:{reason}",
                self.options.surface.prefix()
            ));
        }
        if let Some(refusal) = self.target_locus_guard(name, &args) {
            return Ok(refusal);
        }
        if name == "task_lifecycle_doctor" || name == "work_lifecycle_doctor" {
            return self.doctor(&args);
        }
        if name == "task_lifecycle_restart" {
            return self.task_restart(args);
        }
        if self.options.surface == Surface::Work
            && name.starts_with("task_lifecycle_")
            && !is_task_read_only(name)
        {
            self.check_work_revision(&args, "task_number", "expected_revision")?;
            self.check_work_revision(&args, "parent_task_number", "expected_parent_revision")?;
            self.check_work_revision(&args, "required_task_number", "expected_required_revision")?;
        }
        if self.options.surface == Surface::Work && name.starts_with("task_lifecycle_")
            || name.starts_with("mcp_")
        {
            return self.call_task_tool(name, args);
        }
        if self.options.surface == Surface::Task {
            self.call_task_tool(name, args)
        } else {
            self.call_work_tool(name, args)
        }
    }

    fn check_work_revision(
        &self,
        args: &Value,
        number_key: &str,
        revision_key: &str,
    ) -> Result<(), String> {
        let Some(number) = args.get(number_key).and_then(Value::as_i64) else {
            return Ok(());
        };
        let expected = args
            .get(revision_key)
            .and_then(Value::as_i64)
            .ok_or_else(|| format!("{revision_key}_required"))?;
        let actual: i64 = self
            .connection()?
            .query_row(
                "select revision from task_lifecycle where task_number=?1",
                params![number],
                |r| r.get(0),
            )
            .optional()
            .map_err(db_error)?
            .ok_or_else(|| format!("task_not_found:{number}"))?;
        if actual != expected {
            return Err(format!(
                "task_revision_conflict:expected_{expected}:actual_{actual}"
            ));
        }
        Ok(())
    }
    fn connection(&self) -> Result<&Connection, String> {
        self.connection
            .as_ref()
            .ok_or_else(|| "lifecycle_runtime_not_open".to_string())
    }

    fn restart_request_path(&self) -> PathBuf {
        self.options.site_root.join(".ai").join("tmp").join("task-lifecycle-restart-request.json")
    }

    fn restart_baseline_path(&self) -> PathBuf {
        self.options.site_root.join(".ai").join("tmp").join("mcp-baseline.json")
    }

    fn task_freshness(&self) -> Result<Value, String> {
        let request_path = self.restart_request_path();
        let baseline_path = self.restart_baseline_path();
        let request = read_json_file(&request_path);
        let baseline = read_json_file(&baseline_path);
        let expected_tools = self.options.surface.tools();
        let source_digest = native_canonical_digest(&json!({
            "surface": self.options.surface.server_name(),
            "server_version": SERVER_VERSION,
            "tools": expected_tools,
        }));
        let baseline_digest = baseline.as_ref()
            .and_then(|value| value.get("source_digest"))
            .and_then(Value::as_str)
            .map(ToString::to_string);
        let source_digest_changed = baseline_digest.as_ref().is_some_and(|value| value != &source_digest);
        let pending_restart = request.is_some() || source_digest_changed;
        Ok(json!({
            "schema": "narada.mcp.live_freshness.v0",
            "server_name": self.options.surface.server_name(),
            "server_entrypoint": self.options.surface.server_name(),
            "live_process": {"booted_at": self.booted_at, "pid": std::process::id(), "self_restart_supported": false},
            "source": {"source_digest": source_digest, "source_digest_algorithm": "sha256", "source_files_count": 0, "source_manifest_paths": []},
            "baseline": {"path": baseline_path, "state": if baseline.is_some() {"present"} else {"missing"}, "payload": baseline, "source_newer_than_baseline": source_digest_changed, "source_digest": baseline_digest, "source_digest_algorithm": "sha256", "source_digest_changed": source_digest_changed, "freshness_basis": "native_catalog_digest"},
            "restart_request": {"path": request_path, "state": if request.is_some() {"restart_requested"} else {"no_restart_request"}, "payload": request},
            "host_registry_reference": {"status": "not_observed", "source": "native_stdio"},
            "tool_surface": {"expected_count": expected_tools.len(), "registered_count": expected_tools.len(), "missing_expected_tools": []},
            "pending_restart": pending_restart,
            "stale_live_surface_possible": pending_restart,
            "source_digest": source_digest,
            "baseline_source_digest": baseline_digest,
            "source_digest_changed": source_digest_changed,
            "freshness_basis": "native_catalog_digest",
            "remediation": if pending_restart {json!(["Restart the external stdio MCP carrier, then acknowledge the restart request."])} else {json!(["No pending restart signal is recorded for this MCP server."])}
        }))
    }

    fn task_restart(&self, args: Value) -> Result<Value, String> {
        let mode = args.get("mode").and_then(Value::as_str).unwrap_or("request");
        if !matches!(mode, "request" | "status" | "acknowledge" | "clear") {
            return Err(format!("invalid_restart_mode: {mode}"));
        }
        let request_path = self.restart_request_path();
        let baseline_path = self.restart_baseline_path();
        let existing = read_json_file(&request_path);
        if mode == "status" {
            return Ok(json!({
                "status": if existing.is_some() {"restart_requested"} else {"no_restart_request"},
                "schema": "narada.task_lifecycle.restart_request.v0",
                "can_self_restart": false,
                "restart_mechanism": "external_stdio_mcp_restart_required",
                "request_path": request_path,
                "baseline_path": baseline_path,
                "request": existing,
                "mcp_freshness": self.task_freshness()?,
                "message": if existing.is_some() {"Task-lifecycle MCP restart has been requested. Restart the carrier/session MCP servers externally to load new code."} else {"No task-lifecycle MCP restart request file is present."}
            }));
        }
        if mode == "request" {
            let timestamp = now();
            let note = string_arg(&args, "reason").unwrap_or_else(|| "This native tool cannot restart its own stdio MCP process. Restart the carrier/session externally.".to_string());
            let payload = json!({
                "schema": "narada.mcp.restart_request.v0",
                "requested_at": timestamp,
                "requested_by": env::var("NARADA_AGENT_ID").ok(),
                "reason": note,
                "can_self_restart": false,
                "restart_mechanism": "external_stdio_mcp_restart_required",
                "server_name": self.options.surface.server_name(),
                "target_surface": self.options.surface.server_name(),
                "target_entrypoint": self.options.surface.server_name(),
                "requested_process": {"pid": std::process::id(), "booted_at": self.booted_at},
                "note": note
            });
            write_json_file(&request_path, &payload, "restart_request")?;
            let source_digest = native_canonical_digest(&json!({"surface":self.options.surface.server_name(),"server_version":SERVER_VERSION,"tools":self.options.surface.tools()}));
            let baseline = json!({"schema":"narada.mcp.reload_request.v0","requested_at":timestamp,"surface":self.options.surface.server_name(),"target_entrypoint":self.options.surface.server_name(),"source_digest":source_digest,"note":note});
            write_json_file(&baseline_path, &baseline, "restart_baseline")?;
            return Ok(json!({"status":"restart_requested","schema":"narada.mcp.restart_request.v0","can_self_restart":false,"restart_mechanism":"external_stdio_mcp_restart_required","request_path":request_path,"baseline_path":baseline_path,"requested_at":timestamp,"message":note}));
        }
        let Some(request) = existing else {
            return Ok(json!({"status":"no_restart_request","schema":"narada.mcp.restart_acknowledgement.v0","already_cleared":true,"can_self_restart":false,"restart_mechanism":"external_stdio_mcp_restart_required","request_path":request_path,"baseline_path":baseline_path,"message":"No restart request is pending; the marker is already clear."}));
        };
        let requested_at = request.get("requested_at").and_then(Value::as_str).unwrap_or("");
        if self.booted_at.as_str() <= requested_at {
            return Ok(json!({"status":"restart_acknowledgement_rejected","schema":"narada.mcp.restart_acknowledgement_rejection.v0","can_self_restart":false,"restart_mechanism":"external_stdio_mcp_restart_required","request_path":request_path,"baseline_path":baseline_path,"rejected_at":now(),"reason":"post_request_boot_evidence_missing","validation":{"status":"rejected","reason":"post_request_boot_evidence_missing","live_process_booted_at":self.booted_at,"requested_at":requested_at},"message":"Restart acknowledgement rejected: post-request carrier boot evidence is required before clearing the marker."}));
        }
        fs::remove_file(&request_path).map_err(|e| format!("restart_request_clear_failed:{e}"))?;
        let acknowledged_at = now();
        let source_digest = native_canonical_digest(&json!({"surface":self.options.surface.server_name(),"server_version":SERVER_VERSION,"tools":self.options.surface.tools()}));
        let baseline = json!({"schema":"narada.mcp.restart_acknowledgement.v0","acknowledged_at":acknowledged_at,"acknowledged_by":env::var("NARADA_AGENT_ID").ok(),"reason":string_arg(&args,"reason"),"surface":self.options.surface.server_name(),"server_name":self.options.surface.server_name(),"source_digest":source_digest,"freshness_basis":"native_catalog_digest"});
        write_json_file(&baseline_path, &baseline, "restart_acknowledgement")?;
        Ok(json!({"status":"restart_acknowledged","schema":"narada.mcp.restart_acknowledgement.v0","can_self_restart":false,"restart_mechanism":"external_stdio_mcp_restart_required","request_path":request_path,"baseline_path":baseline_path,"acknowledged_at":acknowledged_at,"baseline":baseline,"message":"External stdio MCP restart acknowledged; restart request marker cleared."}))
    }

    fn doctor(&self, args: &Value) -> Result<Value, String> {
        let preparation = inspect_database(&self.options)?;
        let full = args.get("verbose").and_then(Value::as_bool) == Some(true)
            || args.get("detail").and_then(Value::as_str) == Some("full");
        Ok(match self.options.surface {
            Surface::Task => json!({
                "schema":"narada.task_lifecycle.doctor.v1","status":"ok","detail":if full {"full"} else {"summary"},
                "site_root":self.options.site_root.to_string_lossy(),"site_root_source":self.options.site_root_source,
                "authority_posture":"facade_only","surface_type":"task_lifecycle_mcp",
                "fabric_lifecycle":{"mode":"restart_required","restart_owner":"mcp-loader","reason":"Tool and runtime changes require mcp_loader_surface_restart for the bound task-lifecycle surface."},
                "tool_posture":{"canonical_count":self.options.surface.tools().len(),"deprecated_alias_count":38},
                "site_policy":{"source":"default","roster":{"roles_are_obligation_targets":false}},
                "mcp_freshness":self.task_freshness()?,
                "full_detail_hint":{"verbose":true,"detail":"full"},
                "preparation":preparation,
                "target_locus_guard":{"schema":"narada.task_lifecycle.target_locus_guard.v0","status":self.target_locus_status().get("status").cloned().unwrap_or_else(||json!("unknown")),"explicit_target_site_root_supported":false}
            }),
            Surface::Work => json!({
                "schema":"narada.work_lifecycle.doctor.v1",
                "status":if preparation.get("status").and_then(Value::as_str)==Some("prepared") {"ok"} else {"not_ready"},
                "site_root":self.options.site_root.to_string_lossy(),"preparation":preparation,
                "concurrency":{"database_path":self.options.database_path().to_string_lossy(),"posture":"sqlite_wal_transactional_multi_process","conflict_guards":["sqlite_write_serialization","idempotency_keys","revision_checks"]}
            }),
        })
    }

}
