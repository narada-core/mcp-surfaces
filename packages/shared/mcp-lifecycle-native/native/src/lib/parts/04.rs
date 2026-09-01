impl LifecycleServer {
    pub fn run_stdio(&mut self) -> Result<(), String> {
        if self.options.prepare {
            let output = Self::prepare_database(&self.options)?;
            println!("{}", output);
            return Ok(());
        }
        if self.options.migrate_legacy {
            if self.options.surface != Surface::Work {
                return Err("legacy_migration_work_surface_required".to_string());
            }
            let source = self
                .options
                .source_database_path
                .clone()
                .ok_or("source_database_path_required")?;
            let source = if source.is_absolute() {
                source
            } else {
                self.options.site_root.join(source)
            };
            if !source.exists() {
                return Err("legacy_migration_source_database_missing".to_string());
            }
            let target = self.options.database_path();
            if source != target {
                if target.exists() {
                    return Err("legacy_migration_target_exists".to_string());
                }
                if let Some(parent) = target.parent() {
                    fs::create_dir_all(parent)
                        .map_err(|e| format!("legacy_migration_directory_create_failed:{e}"))?;
                }
                fs::copy(&source, &target)
                    .map_err(|e| format!("legacy_migration_copy_failed:{e}"))?;
            }
            let output = Self::prepare_database(&self.options)?;
            println!(
                "{}",
                json!({"status":"migrated","site_root":self.options.site_root.to_string_lossy(),"source_database_path":source,"target_database_path":target,"preparation":output.get("preparation")})
            );
            return Ok(());
        }
        let stdin = io::stdin();
        let stdout = io::stdout();
        let mut reader = WireReader::new(stdin.lock());
        let mut writer = stdout.lock();
        while let Some((request, framed)) = reader
            .next()
            .map_err(|e| format!("mcp_transport_read_failed:{e}"))?
        {
            let response = self.handle_request(request.clone());
            if let Some(value) = response {
                write_wire(&mut writer, &value, framed)
                    .map_err(|e| format!("mcp_transport_write_failed:{e}"))?;
            }
        }
        Ok(())
    }

    pub fn handle_request(&mut self, request: Value) -> Option<Value> {
        let object = request.as_object()?;
        let id = object.get("id").cloned().unwrap_or(Value::Null);
        let method = object.get("method").and_then(Value::as_str).unwrap_or("");
        if object.get("id").is_none() && method.starts_with("notifications/") {
            return None;
        }
        let default_params = Value::Object(Map::new());
        let params = object.get("params").unwrap_or(&default_params);
        let modern = is_modern_request(params);
        let result = if modern {
            validate_modern_request(params).and_then(|_| match method {
                "server/discover" => Ok(modernize_result(server_discover(self.options.surface), method, self.options.surface)),
                "initialize" => Err("initialize_removed:The 2026-07-28 protocol has no initialize handshake.".to_string()),
                _ => self.dispatch(method, params).map(|value| modernize_result(value, method, self.options.surface)),
            })
        } else {
            self.dispatch(method, params)
        };
        match result {
            Ok(result) => Some(json!({"jsonrpc":"2.0", "id": id, "result": result})),
            Err(error) => Some(json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": self.error_value(error),
            })),
        }
    }
    fn dispatch(&mut self, method: &str, params: &Value) -> Result<Value, String> {
        match method {
            "initialize" => Ok(json!({
                "protocolVersion": params.get("protocolVersion").and_then(Value::as_str).unwrap_or(match self.options.surface {
                    Surface::Task => TASK_PROTOCOL_VERSION,
                    Surface::Work => WORK_PROTOCOL_VERSION,
                }),
                "capabilities": if self.options.surface == Surface::Task {
                    json!({"tools":{},"resources":{},"prompts":{},"completions":{},"logging":{}})
                } else { json!({"tools":{}}) },
                "serverInfo": {"name": self.options.surface.server_name(), "version": SERVER_VERSION}
            })),
            "tools/list" => Ok(json!({"tools": self.options.surface.tools()})),
            "resources/list" if self.options.surface == Surface::Task => {
                self.resources_list(params)
            }
            "resources/read" if self.options.surface == Surface::Task => {
                self.resources_read(params)
            }
            "prompts/list" if self.options.surface == Surface::Task => Ok(json!({
                "prompts": [{"name":"task_lifecycle_workflow","title":"Task Lifecycle Workflow","description":"Guidance for governed task lifecycle operations.","arguments":[]}]
            })),
            "prompts/get" if self.options.surface == Surface::Task => {
                let name = params.get("name").and_then(Value::as_str).unwrap_or("");
                if name != "task_lifecycle_workflow" {
                    return Err(format!("unknown_prompt:{name}"));
                }
                Ok(
                    json!({"description":"Guidance for governed task lifecycle operations.","messages":[{"role":"user","content":{"type":"text","text":"Inspect task state before mutation. Admit evidence before finish/close transitions and preserve lifecycle authority details in structuredContent."}}]}),
                )
            }
            "completion/complete" if self.options.surface == Surface::Task => {
                let argument_name = params.get("argument").and_then(Value::as_object).and_then(|argument| argument.get("name")).and_then(Value::as_str).unwrap_or("");
                let values = if argument_name == "name" {
                    self.options.surface.tools().iter().filter_map(|v| v.get("name").and_then(Value::as_str)).take(100).map(ToString::to_string).collect::<Vec<_>>()
                } else { Vec::new() };
                Ok(json!({"completion":{"values":values,"total":values.len(),"hasMore":false}}))
            },
            "logging/setLevel" => Ok(json!({})),
            "tools/call" => {
                let name = params
                    .get("name")
                    .and_then(Value::as_str)
                    .ok_or("tool_name_required")?;
                let args = params
                    .get("arguments")
                    .cloned()
                    .unwrap_or_else(|| json!({}));
                let tool = self
                    .options
                    .surface
                    .tools()
                    .into_iter()
                    .find(|tool| tool.get("name").and_then(Value::as_str) == Some(name))
                    .ok_or_else(|| format!("unknown_tool:{name}"))?;
                validate_input(
                    tool.get("inputSchema").unwrap_or(&Value::Null),
                    &args,
                    "/arguments",
                )?;
                let payload = self.call_tool(name, args)?;
                let is_error = matches!(payload.get("status").and_then(Value::as_str), Some("blocked") | Some("refused"))
                    || payload.get("error").is_some()
                    || payload.get("close_blocked").and_then(Value::as_bool) == Some(true);
                Ok(self.tool_result(name, payload, is_error)?)
            }
            _ => Err(if self.options.surface == Surface::Task { format!("unsupported_mcp_method: {method}") } else { format!("unsupported_mcp_method:{method}") }),
        }
    }

    fn target_locus_status(&self) -> Value {
        let operator_root = ["NARADA_OPERATOR_STATED_SITE_ROOT", "NARADA_REQUESTED_WORK_ROOT", "NARADA_TARGET_SITE_ROOT"]
            .iter()
            .find_map(|key| env::var(key).ok().filter(|value| !value.trim().is_empty()));
        let operator_root = operator_root.map(|value| normalized_path_string(Path::new(&value)));
        let default_root = normalized_path_string(&self.options.site_root);
        let status = if operator_root.as_ref().is_some_and(|value| value != &default_root) {
            "operator_stated_locus_mismatch"
        } else {
            "clear"
        };
        json!({
            "schema": "narada.task_lifecycle.target_locus_guard.v0",
            "default_target_site_root": self.options.site_root.to_string_lossy(),
            "operator_stated_locus_root": operator_root,
            "status": status,
            "explicit_target_site_root_supported": false,
            "rule": "Task lifecycle MCP is bound to its --site-root. Startup/control-surface identity does not authorize mutating a different requested work substrate."
        })
    }

    fn target_locus_guard(&self, name: &str, args: &Value) -> Option<Value> {
        if !is_locus_guarded_mutation(name) {
            return None;
        }
        if (name == "task_lifecycle_bridge_poll" || name == "task_lifecycle_inbox_target")
            && args.get("dry_run").and_then(Value::as_bool) == Some(true)
        {
            return None;
        }
        let status = self.target_locus_status();
        if status.get("status").and_then(Value::as_str) != Some("operator_stated_locus_mismatch") {
            return None;
        }
        let mut refusal = status.as_object().cloned().unwrap_or_default();
        refusal.insert("status".to_string(), json!("refused"));
        refusal.insert("refusal_code".to_string(), json!("target_locus_preflight_required"));
        refusal.insert("tool_name".to_string(), json!(name));
        refusal.insert("remediation".to_string(), json!("Relaunch the task lifecycle MCP for the intended Site, clear the operator-stated locus after explicit correction, or use a mutation surface that accepts explicit target_site_root."));
        Some(Value::Object(refusal))
    }
    fn error_value(&self, message: String) -> Value {
        let prefix = message.split(':').next().unwrap_or(&message);
        if self.options.surface == Surface::Task && prefix == "task_lifecycle_store_not_prepared" {
            let reason = message
                .split_once(':')
                .map(|(_, suffix)| suffix)
                .filter(|suffix| !suffix.is_empty())
                .unwrap_or("unknown");
            return json!({
                "code": -32000,
                "message": message,
                "data": {
                    "schema": "narada.task_lifecycle.not_ready.v1",
                    "code": "task_lifecycle_store_not_prepared",
                    "reason": reason,
                    "site_root": self.options.site_root.to_string_lossy(),
                    "remediation": {
                        "inspect_tool": "task_lifecycle_doctor",
                        "prepare_command": "task-lifecycle-mcp --prepare --site-root <site-root>",
                        "after_prepare": "restart_or_reattach_runtime"
                    }
                }
            });
        }
        let schema = match self.options.surface {
            Surface::Task => "narada.task_lifecycle.error.v1",
            Surface::Work => "narada.work_lifecycle.error.v1",
        };
        json!({"code": -32000, "message": message, "data": {"schema": schema, "code": prefix, "site_root": self.options.site_root.to_string_lossy()}})
    }

    fn tool_result(&self, tool_name: &str, payload: Value, is_error: bool) -> Result<Value, String> {
        let compact = serde_json::to_string(&payload).map_err(|e| format!("tool_result_serialize_failed:{e}"))?;
        let inline_limit = 4_000usize;
        let semantic_materialization = (tool_name == "task_lifecycle_finish" && payload.get("review_required").and_then(Value::as_bool) == Some(true)) || (tool_name == "task_lifecycle_close" && payload.get("error").and_then(Value::as_str) == Some("task_close_dependencies_unsatisfied")) || (tool_name == "task_lifecycle_show" && payload.get("lifecycle").and_then(|value| value.get("status")).and_then(Value::as_str) == Some("awaiting_dependencies"));
        if !semantic_materialization && utf16_len(&compact) <= inline_limit {
            let mut structured = if let Some(object) = payload.as_object() {Value::Object(object.clone())} else {json!({"value":payload})};
            if let Some(object) = structured.as_object_mut() {
                object.insert("inline_text_truncated".to_string(), json!(false));
                object.insert("rendered_text_char_length".to_string(), json!(utf16_len(&compact)));
                object.insert("full_output_char_length".to_string(), json!(utf16_len(&compact)));
            }
            let mut result = json!({"content":[{"type":"text","text":compact,"annotations":{"audience":["assistant"]}}],"structuredContent":structured});
            if is_error {result["isError"] = json!(true);}
            return Ok(result);
        }
        let full_text = serde_json::to_string_pretty(&payload).map_err(|e| format!("tool_result_presentation_failed:{e}"))?;
        let output_id = format!("o_{}", Uuid::new_v4().simple().to_string()[..24].to_string());
        let output_ref = format!("mcp_output:{output_id}");
        let created_by = env::var("NARADA_AGENT_ID").ok().filter(|value| !value.trim().is_empty());
        let record = json!({"schema":"narada.mcp_output_ref.v1","ref":output_ref,"output_id":output_id,"tool_name":tool_name,"created_at":now(),"created_by":created_by,"content_type":"application/json","inline_char_limit":inline_limit,"full_output_char_length":utf16_len(&full_text),"truncated":true,"sha256":native_canonical_digest(&payload),"max_bytes":10 * 1024 * 1024,"full_output":payload});
        let serialized = format!("{}\n", serde_json::to_string(&record).map_err(|e| format!("tool_result_record_serialize_failed:{e}"))?);
        if serialized.as_bytes().len() > 10 * 1024 * 1024 {return Err(format!("mcp_output_too_large: {} > {}",serialized.as_bytes().len(),10 * 1024 * 1024));}
        let directory = self.options.site_root.join(".ai").join("tmp").join("mcp-outputs").join("workspace");
        fs::create_dir_all(&directory).map_err(|e| format!("output_resource_directory_create_failed:{e}"))?;
        let path = directory.join(format!("{output_id}.json"));
        let mut file = fs::OpenOptions::new().write(true).create_new(true).open(&path).map_err(|e| format!("mcp_output_write_failed:{e}"))?;
        file.write_all(serialized.as_bytes()).map_err(|e| format!("mcp_output_write_failed:{e}"))?;
        file.sync_all().map_err(|e| format!("mcp_output_sync_failed:{e}"))?;
        let preview: String = full_text.chars().take(1_000).collect();
        let preview_length = utf16_len(&preview);
        let output_status = payload.get("status").and_then(Value::as_str).filter(|value| value.len() <= 32).unwrap_or(if is_error {"error"} else {"ok"});
        let envelope = json!({"schema":"narada.producer_output_page.v1","status":output_status,"truncated":true,"output_ref":output_ref,"ref":output_ref,"result_materialized":true,"tool_name":tool_name,"offset":0,"limit":inline_limit,"next_offset":if preview_length < utf16_len(&full_text) {json!(preview_length)} else {Value::Null},"transport_offset":0,"transport_limit":inline_limit,"transport_next_offset":if preview_length < utf16_len(&full_text) {json!(preview_length)} else {Value::Null},"output_text":preview,"output_truncated":preview_length < utf16_len(&full_text),"reader_tool":"mcp_output_show","site_root":self.options.site_root.to_string_lossy(),"read_command":format!("mcp_output_show({{ \\\"ref\\\": \\\"{output_ref}\\\" }})"),"remediation":format!("Use mcp_output_show with output_ref/ref={output_ref} to read bounded produced JSON pages."),"inline_limit":inline_limit,"full_output_char_length":utf16_len(&full_text)});
        let text = serde_json::to_string(&envelope).map_err(|e| format!("tool_result_envelope_serialize_failed:{e}"))?;
        let mut result = json!({"content":[{"type":"text","text":text,"annotations":{"audience":["assistant"]}}],"structuredContent":envelope});
        if is_error {result["isError"] = json!(true);}
        Ok(result)
    }

}
