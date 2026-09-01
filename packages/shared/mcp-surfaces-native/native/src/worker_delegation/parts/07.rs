fn instruction(args: &Map<String, Value>) -> Result<String, Value> {
    let intent = args.get("intent").and_then(Value::as_object);
    for key in ["instruction", "task", "goal", "summary"] {
        if let Some(v) = intent
            .and_then(|m| m.get(key))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|v| !v.is_empty())
        {
            return Ok(v.to_string());
        }
    }
    Err(error(
        "worker_intent_instruction_required",
        "worker_intent_instruction_required",
    ))
}
fn authority(args: &Map<String, Value>) -> Result<&str, Value> {
    let value = args
        .get("constraints")
        .and_then(Value::as_object)
        .and_then(|v| v.get("authority"))
        .and_then(Value::as_str)
        .unwrap_or("read");
    if matches!(value, "read" | "write" | "command") {
        Ok(value)
    } else {
        Err(error(
            "worker_authority_invalid",
            "worker_authority_invalid",
        ))
    }
}
fn worker_run(
    args: &Map<String, Value>,
    root: &Path,
    allowed_roots: &[PathBuf],
    resume: Option<String>,
    tool_name: &str,
) -> Result<Value, Value> {
    let instruction_text = instruction(args)?;
    let auth = authority(args)?.to_string();
    let constraints = args.get("constraints").and_then(Value::as_object);
    let max_run_ms = constraints
        .and_then(|value| value.get("max_run_ms"))
        .and_then(Value::as_u64)
        .unwrap_or(300_000)
        .clamp(1, 1_800_000);
    let queue_timeout_ms = constraints
        .and_then(|value| value.get("queue_timeout_ms"))
        .and_then(Value::as_u64)
        .unwrap_or(300_000)
        .clamp(1, 1_800_000);
    let wait_for_completion = constraints
        .and_then(|value| value.get("wait_for_completion"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let wait_timeout_ms = constraints
        .and_then(|value| value.get("wait_timeout_ms"))
        .and_then(Value::as_u64)
        .unwrap_or(30_000)
        .min(MAX_INLINE_WAIT_MS);
    for key in ["provider"] {
        if constraints.and_then(|value| value.get(key)).is_some() {
            return Err(error(
                "worker_canonical_invocation_plan_override_rejected",
                "worker_canonical_invocation_plan_override_rejected",
            ));
        }
    }
    let cognition = constraints
        .and_then(|value| value.get("cognition"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(DEFAULT_COGNITION)
        .to_string();
    if !matches!(cognition.as_str(), "low" | "medium" | "high") {
        return Err(error(
            "worker_cognition_invalid",
            "worker_cognition_invalid",
        ));
    }
    let requested_plan_ref = constraints
        .and_then(|value| value.get("invocation_plan_ref"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .or_else(|| {
            std::env::var("NARADA_INTELLIGENCE_PLAN_REF")
                .ok()
                .filter(|value| !value.trim().is_empty())
        });
    if requested_plan_ref.as_deref().is_some_and(|plan_ref| {
        !plan_ref.starts_with("plan:")
            || !plan_ref[5..].chars().all(|character| {
                character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | ':' | '-')
            })
    }) {
        return Err(error(
            "worker_canonical_invocation_plan_invalid",
            "worker_canonical_invocation_plan_invalid",
        ));
    }
    let (
        plan_ref,
        provider_mode,
        provider_model,
        preflight_evidence_ref,
        reasoning_effort,
        provider_binding,
    ) = invocation_plan_binding(root, requested_plan_ref.as_deref(), Some(&cognition))?;
    let codex_transport = if provider_mode == "codex-subscription" {
        std::env::var("NARADA_WORKER_CODEX_TRANSPORT")
            .unwrap_or_else(|_| "codex-app-server".to_string())
    } else {
        "native-http".to_string()
    };
    if !matches!(codex_transport.as_str(), "codex-app-server" | "codex-exec" | "native-http") {
        return Err(error("worker_codex_transport_invalid", "worker_codex_transport_invalid"));
    }
    let codex_broker = if provider_mode == "codex-subscription" && codex_transport == "codex-app-server" {
        Some(crate::codex_app_server_broker::binding(root).map_err(|reason| {
            json!({"schema":"narada.worker.error.v1","code":"worker_codex_app_server_unavailable","message":"worker_codex_app_server_unavailable","reason":reason,"mutation_started":false})
        })?)
    } else {
        None
    };
    let cwd = constraints
        .and_then(|v| v.get("cwd"))
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .unwrap_or_else(|| root.to_path_buf());
    if !allowed_roots.iter().any(|allowed| is_within(&cwd, allowed)) {
        return Err(error(
            "worker_cwd_outside_allowed_roots",
            "worker_cwd_outside_allowed_roots",
        ));
    }
    let runtime = runtime_command(root)?;
    let runtime_probe = if auth == "write" { Some(scoped_write_probe(&cwd)?) } else { None };
    let preflight = preflight_paths(constraints, &cwd, allowed_roots)?;
    let prompt = worker_prompt(&instruction_text, &preflight);
    let task_label = instruction_text.chars().take(160).collect::<String>();
    let mut capabilities = capability_snapshot(&auth, &cwd, allowed_roots, runtime_probe.as_ref());
    if let Some(broker) = codex_broker.as_ref() {
        capabilities["provider_boundary"]["source"] = json!("native_codex_app_server_broker");
        capabilities["provider_boundary"]["transport"] = json!("codex_app_server_broker");
        capabilities["provider_boundary"]["broker_generation"] = json!(broker.broker_generation);
        capabilities["provider_boundary"]["thread_policy"] = json!("fresh_ephemeral_per_turn");
    } else if provider_mode == "codex-subscription" {
        capabilities["provider_boundary"]["source"] = json!("native_codex_exec");
        capabilities["provider_boundary"]["transport"] = json!("codex_exec");
    } else {
        capabilities["provider_boundary"]["transport"] = json!("native_http");
        capabilities["tool_bridge"]["kind"] = json!("nars_native_mcp_gateway");
    }
    capabilities["preflight"] = preflight;
    let id = format!("run-{}", uuid::Uuid::new_v4().simple());
    let session = resume.clone().unwrap_or_else(|| id.clone());
    let dir = run_root(root).join(&id);
    fs::create_dir_all(&dir)
        .map_err(|_| error("worker_run_create_failed", "worker_run_create_failed"))?;
    let provider_binding_path = provider_binding
        .as_ref()
        .map(|_| dir.join("provider-binding.json"));
    if let (Some(binding), Some(path)) = (provider_binding.as_ref(), provider_binding_path.as_ref())
    {
        write_json_atomic(path, binding)?;
    }
    let mut resolved_invocation = resolved_invocation(
        &cognition,
        &plan_ref,
        &provider_mode,
        &provider_model,
        &preflight_evidence_ref,
        reasoning_effort.as_deref(),
        provider_binding.as_ref(),
        provider_binding_path.as_deref(),
    );
    if let Some(broker) = codex_broker.as_ref() {
        resolved_invocation["provider_transport"] = json!("codex_app_server_broker");
        resolved_invocation["provider_broker_generation"] = json!(broker.broker_generation);
        resolved_invocation["provider_thread_policy"] = json!("fresh_ephemeral_per_turn");
    } else if provider_mode == "codex-subscription" {
        resolved_invocation["provider_transport"] = json!("codex_exec");
    } else {
        resolved_invocation["provider_transport"] = json!("native_http");
    }
    let started = now();
    let request = json!({"schema":"narada.worker.request.v1","run_id":id,"origin_tool":tool_name,"intent":args.get("intent"),"constraints":args.get("constraints"),"resume_worker_session_id":resume,"capability_snapshot":capabilities.clone(),"invocation_plan_ref":plan_ref,"preflight_evidence_ref":preflight_evidence_ref,"resolved_invocation":resolved_invocation.clone()});
    write_json_atomic(&dir.join("request.json"), &request)?;
    fs::write(dir.join("worker_prompt.txt"), &prompt)
        .map_err(|_| error("worker_write_failed", "worker_write_failed"))?;
    let running = json!({"schema":"narada.worker.run.v1","run_id":id,"task_label":task_label.clone(),"status":"running","completion_state":"pending","phase":"starting","heartbeat_at":started.clone(),"runtime":"narada-agent-runtime-server","authority":auth,"resolved_invocation":resolved_invocation.clone(),"capability_snapshot":capabilities.clone(),"worker_session_id":session,"origin_tool":tool_name,"pid":null,"summary":null,"result":null,"error":null,"refusals":[],"timing":{"started_at":started.clone(),"admitted_at":null,"finished_at":null,"queue_ms":null,"execution_ms":null,"duration_ms":null},"artifacts":{"request":dir.join("request.json").to_string_lossy(),"events":dir.join("events.jsonl").to_string_lossy(),"diagnostic":dir.join("diagnostic.log").to_string_lossy(),"last_message":dir.join("last_message.json").to_string_lossy()}});
    write_json_atomic(&dir.join("result.json"), &running)?;
    let root_owned = root.to_path_buf();
    let dir_owned = dir.clone();
    let id_owned = id.clone();
    let session_owned = session.clone();
    let resume_owned = resume.clone();
    let auth_owned = auth.clone();
    let allowed_roots_owned = allowed_roots.to_vec();
    thread::Builder::new()
        .name(format!("worker-{id}"))
        .spawn(move || {
            complete_native_run(
                runtime,
                cwd,
                root_owned,
                dir_owned,
                id_owned.clone(),
                id_owned,
                session_owned,
                resume_owned,
                auth_owned,
                cognition,
                resolved_invocation,
                plan_ref,
                provider_mode,
                provider_model,
                reasoning_effort,
                provider_binding_path,
                codex_broker,
                codex_transport,
                allowed_roots_owned,
                max_run_ms,
                queue_timeout_ms,
                task_label,
                started,
                format!("Effective mode: {}. This reconciled state is injected at the provider process boundary through the permission profile, CLI sandbox, and writable-root arguments; ambient labels are advisory. CWD: {}. Writable roots: {}. Scoped create/read/remove preflight: {}. Command write effects: {}. Windows text-file lifecycle: use one literal-path PowerShell cmdlet invocation per operation: Set-Content -Encoding utf8, Get-Content -Encoding utf8, Remove-Item, then Test-Path; do not use utf8NoBOM or .NET methods under ConstrainedLanguage. Delegated apply_patch is unavailable under this restricted-token carrier. Read-only command policy: issue one executable with literal arguments per probe; do not combine probes with &&, ;, pipes, redirection, $(), backticks, or generated scripts. Use separate bounded commands and report each result. For non-ASCII text, set explicit UTF-8 output before reading. Carrier MCP projection: none. On refusal return narada.worker.refusal.v1 with tool, operation, cwd, target_path, declared_capability, actual_refusal. Ergonomics ratings use narada.worker.observed_ergonomics.v1: lower a score only for observed failure, retry, human intervention, or ambiguity that changed execution; automatic contained review requires no human interaction and is not ceremony; put hypothetical improvements in non_scoring_observations.\n\nTask:\n{prompt}", capabilities["effective_mode"].as_str().unwrap_or("unknown"), capabilities["cwd"].as_str().unwrap_or("unknown"), capabilities["allowed_roots"].as_array().map(|roots| roots.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(", ")).unwrap_or_default(), capabilities["runtime_probe"]["status"].as_str().unwrap_or("not_required"), capabilities["commands"]["write_effects"].as_bool().unwrap_or(false)),
            )
        })
        .map_err(|_| error("worker_launch_failed", "worker_launch_failed"))?;
    if wait_for_completion {
        let (mut run, wait) = wait_for_run(root, &id, wait_timeout_ms)?;
        if let Some(object) = run.as_object_mut() {
            object.insert("wait".into(), wait);
        }
        return Ok(run);
    }
    Ok(running)
}
fn worker_edit(
    args: &Map<String, Value>,
    root: &Path,
    allowed_roots: &[PathBuf],
) -> Result<Value, Value> {
    let prompt =
        required_string(args, "instruction", "worker_edit_instruction_required")?.to_string();
    let mut constraints = args
        .get("constraints")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    constraints.insert("authority".into(), json!("write"));
    if let Some(cwd) = args.get("cwd") {
        constraints.insert("cwd".into(), cwd.clone());
    }
    if let Some(plan_ref) = args.get("invocation_plan_ref") {
        constraints.insert("invocation_plan_ref".into(), plan_ref.clone());
    }
    worker_run(
        json!({"intent":{"instruction":prompt,"mode":"edit"},"constraints":constraints})
            .as_object()
            .unwrap(),
        root,
        allowed_roots,
        None,
        "worker_edit",
    )
}
