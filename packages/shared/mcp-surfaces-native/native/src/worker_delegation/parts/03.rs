fn capability_snapshot(
    authority: &str,
    cwd: &Path,
    allowed_roots: &[PathBuf],
    runtime_probe: Option<&Value>,
) -> Value {
    let writable = authority == "write";
    let write_roots = if writable {
        vec![cwd.to_string_lossy().to_string()]
    } else {
        Vec::new()
    };
    let effective_mode = if writable {
        "workspace_write"
    } else {
        "read_only"
    };
    json!({
        "schema":"narada.worker.capability_snapshot.v1",
        "authority":authority,
        "effective_mode":effective_mode,
        "validated_against_runtime":true,
        "validation_basis":if runtime_probe.is_some(){"scoped_create_read_remove_probe_plus_provider_contract"}else{"native_worker_maps_authority_to_provider_and_approval_contract"},
        "reconciliation":{"authoritative_source":"native_worker_runtime","ambient_profile_is_advisory":true,"conflict_resolution":"effective_mode_and_runtime_probe_win"},
        "runtime_probe":runtime_probe.cloned().unwrap_or(Value::Null),
        "provider_boundary":{"permission_profile":effective_mode,"writable_roots_injected":writable,"source":"native_process_environment_and_codex_cli"},
        "cwd":cwd.to_string_lossy(),
        "allowed_roots":allowed_roots.iter().map(|root|root.to_string_lossy()).collect::<Vec<_>>(),
        "read_roots":allowed_roots.iter().map(|root|root.to_string_lossy()).collect::<Vec<_>>(),
        "write_roots":write_roots,
        "network":"denied",
        "filesystem":{"read":true,"write":writable,"patch":writable},
        "commands":{"execute":true,"write_effects":writable,"direct_file_mutation":writable,"working_directory_scoped":true,"tests_may_write_build_artifacts":writable},
        "approval":{"mode":if writable{"automatic_contained_review"}else{"not_required"},"human_interaction_required":false,"sandbox":if writable{"workspace-write"}else{"read-only"}},
        "tool_bridge":{"kind":"codex_builtin_repo_tools","ordinary_file_mutation_tool":"bounded_powershell_cmdlets","supported_cmdlets":["Set-Content","Get-Content","Remove-Item","Test-Path"],"apply_patch_available":false,"mcp_projection":"none","reason":"delegated runs use an isolated config to avoid duplicating the carrier MCP fleet; apply_patch is not writable under the current Windows restricted-token carrier"},
        "workflow_primitives":{"text_file_lifecycle":{"tool":"bounded_shell_command","operations":["create","read_verify","remove","confirm_absent"],"encoding_must_be_explicit":true,"windows_recipe":"use one literal-path PowerShell cmdlet invocation per operation: Set-Content -Encoding utf8, Get-Content -Encoding utf8, Remove-Item, then Test-Path; do not use utf8NoBOM or .NET method invocation under ConstrainedLanguage"}},
        "evaluation_contract":{"schema":"narada.worker.observed_ergonomics.v1","basis":"observed_fresh_run_only","score_5":"no_material_observed_friction","score_reduction_requires":"observed_failure_retry_human_intervention_or_ambiguity_that_changed_execution","automatic_contained_review_is_human_ceremony":false,"speculative_improvements_field":"non_scoring_observations"},
        "refusal_contract":{"schema":"narada.worker.refusal.v1","required_fields":["tool","operation","cwd","target_path","declared_capability","actual_refusal"]}
    })
}
fn scoped_write_probe(cwd: &Path) -> Result<Value, Value> {
    let path = cwd.join(format!(
        ".narada-worker-probe-{}",
        uuid::Uuid::new_v4().simple()
    ));
    fs::write(&path, b"probe").map_err(|failure| {
        error(
            "worker_write_preflight_failed",
            &format!("worker_write_preflight_failed:{failure}"),
        )
    })?;
    let verified = fs::read(&path)
        .map(|value| value == b"probe")
        .unwrap_or(false);
    let removed = fs::remove_file(&path).is_ok() && !path.exists();
    if !verified || !removed {
        return Err(error(
            "worker_write_preflight_failed",
            "worker_write_preflight_failed:verification_or_cleanup",
        ));
    }
    Ok(
        json!({"schema":"narada.worker.runtime_probe.v1","operation":"create_read_remove","status":"passed","cwd":cwd.to_string_lossy(),"cleanup_verified":true}),
    )
}
fn defaults_path(root: &Path) -> PathBuf {
    root.join(".narada/worker-cognition-defaults.json")
}
fn empty_defaults() -> Value {
    json!({"low":{"provider":null,"model":null,"reasoning_effort":null},"medium":{"provider":null,"model":null,"reasoning_effort":null},"high":{"provider":null,"model":null,"reasoning_effort":null}})
}
fn cognition_defaults_for(root: &Path) -> Value {
    read_json(&defaults_path(root))
        .ok()
        .and_then(|v| {
            v.get("effective_cognition_defaults")
                .or_else(|| v.get("defaults"))
                .cloned()
        })
        .unwrap_or_else(empty_defaults)
}
fn cognition_defaults(root: &Path) -> Value {
    json!({"schema":"narada.worker.cognition_defaults.v1","status":"ok","default_cognition":DEFAULT_COGNITION,"defaults":cognition_defaults_for(root),"mapping_semantics":"cognition selects the model tier; reasoning_effort is an independent admitted setting and does not mean low latency","source":"native_contract","canonical_runtime":"narada-agent-runtime-server uses an immutable invocation plan","native_read_only":true})
}
fn config_resolve(
    args: &Map<String, Value>,
    root: &Path,
    allowed_roots: &[PathBuf],
) -> Result<Value, Value> {
    let resolved_authority = authority(args)?;
    let constraints = args.get("constraints").and_then(Value::as_object);
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
    let defaults = cognition_defaults_for(root);
    let selected = defaults.get(&cognition).cloned().unwrap_or(Value::Null);
    let cwd = constraints
        .and_then(|v| v.get("cwd"))
        .and_then(Value::as_str)
        .or_else(|| args.get("cwd").and_then(Value::as_str));
    let cwd = cwd.map(PathBuf::from).unwrap_or_else(|| root.to_path_buf());
    if !allowed_roots.iter().any(|allowed| is_within(&cwd, allowed)) {
        return Err(error(
            "worker_cwd_outside_allowed_roots",
            "worker_cwd_outside_allowed_roots",
        ));
    }
    Ok(json!({
        "schema":"narada.worker.config_resolve.v1",
        "status":"ok",
        "resolved":{
            "cwd":cwd.to_string_lossy(),
            "site_root":root.to_string_lossy(),
            "runtime":"narada-agent-runtime-server",
            "authority":resolved_authority,
            "cognition":cognition,
            "provider":selected.get("provider").cloned().unwrap_or(Value::Null),
            "provider_mode":selected.get("provider").cloned().unwrap_or(Value::Null),
            "model":selected.get("model").cloned().unwrap_or(Value::Null),
            "reasoning_effort":selected.get("reasoning_effort").cloned().unwrap_or(Value::Null),
            "resolution_source":"site_cognition_defaults",
            "canonical_plan_preflight":"deferred_to_worker_run",
            "launch":false
        },
        "capability_snapshot":capability_snapshot(resolved_authority,&cwd,allowed_roots,None),
        "diagnostics":[
            {"name":"native_execution","status":"boundary","message":"worker launch is delegated to the owning worker authority"},
            {"name":"invocation_plan","status":"deferred","message":"canonical provider/model/reasoning binding is finalized by worker_run preflight"}
        ],
        "native_read_only":true
    }))
}
fn run_status(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let id = run_id(args)?;
    let run = read_reconciled_run(root, &id)?;
    let compact = args.get("compact").and_then(Value::as_bool).unwrap_or(true);
    Ok(
        json!({"schema":"narada.worker.run_status.v1","status":"ok","run_id":id,"compact":compact,"site_scope":"current_site","run":if compact{minimal_run(&run)}else{compact_run(&run)},"native_read_only":true}),
    )
}
fn runs_list(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    require_current_site_scope(args)?;
    let compact = args.get("compact").and_then(Value::as_bool).unwrap_or(true);
    let limit = args
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(20)
        .clamp(1, 200) as usize;
    let include_running = args
        .get("include_running")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let include_completed = args
        .get("include_completed")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let mut items = Vec::new();
    if let Ok(entries) = fs::read_dir(run_root(root)) {
        for entry in entries.filter_map(Result::ok).take(MAX_RUNS) {
            if !entry.path().is_dir() {
                continue;
            }
            let Some(id) = entry.file_name().to_str().map(str::to_string) else {
                continue;
            };
            if !id.starts_with("run-") {
                continue;
            }
            if let Ok(run) = read_reconciled_run(root, &id) {
                let terminal =
                    !matches!(run.get("status").and_then(Value::as_str), Some("running"));
                if (terminal && include_completed) || (!terminal && include_running) {
                    items.push(if compact { minimal_run(&run) } else { compact_run(&run) });
                }
            }
        }
    }
    items.sort_by(|a, b| {
        b.get("updated_at")
            .and_then(Value::as_str)
            .cmp(&a.get("updated_at").and_then(Value::as_str))
    });
    items.truncate(limit);
    Ok(
        json!({"schema":"narada.worker.runs_list.v1","status":"ok","count":items.len(),"limit":limit,"scanned":items.len(),"scan_limit":MAX_RUNS,"scan_truncated":false,"site_scope":"current_site","site_root":root.to_string_lossy(),"compact":compact,"include_running":include_running,"include_completed":include_completed,"runs":items,"native_read_only":true}),
    )
}
fn wait_for_run(root: &Path, id: &str, timeout_ms: u64) -> Result<(Value, Value), Value> {
    let started = Instant::now();
    let mut run = read_reconciled_run(root, id)?;
    while run.get("status").and_then(Value::as_str) == Some("running")
        && started.elapsed() < Duration::from_millis(timeout_ms)
    {
        thread::sleep(Duration::from_millis(100).min(Duration::from_millis(
            timeout_ms.saturating_sub(started.elapsed().as_millis() as u64),
        )));
        run = read_run(root, id)?;
    }
    let running = run.get("status").and_then(Value::as_str) == Some("running");
    let waited_ms = started.elapsed().as_millis() as u64;
    let wait = json!({"status":if running{"timed_out"}else{"finished"},"waited":waited_ms>0,"waited_ms":waited_ms,"timeout_ms":timeout_ms,"native_execution":"bounded_state_poll"});
    Ok((run, wait))
}
fn run_wait(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let id = run_id(args)?;
    let compact = args.get("compact").and_then(Value::as_bool).unwrap_or(true);
    let timeout_ms = args
        .get("timeout_ms")
        .and_then(Value::as_u64)
        .unwrap_or(30_000)
        .min(MAX_INLINE_WAIT_MS);
    let (run, wait) = wait_for_run(root, &id, timeout_ms)?;
    Ok(
        json!({"schema":"narada.worker.run_wait.v1","status":"ok","wait":wait,"compact":compact,"site_scope":"current_site","run":if compact{minimal_run(&run)}else{compact_run(&run)},"native_read_only":true}),
    )
}
fn run_wait_batch(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let ids = args
        .get("run_ids")
        .and_then(Value::as_array)
        .ok_or_else(|| error("worker_run_ids_required", "worker_run_ids_required"))?;
    let timeout_ms = args.get("timeout_ms").and_then(Value::as_u64).unwrap_or(30_000).min(180_000);
    let poll_ms = args.get("poll_ms").and_then(Value::as_u64).unwrap_or(5_000).clamp(100, 30_000);
    let compact = args.get("compact").and_then(Value::as_bool).unwrap_or(true);
    let started = Instant::now();
    let runs = loop {
        let mut runs = Vec::new();
        let mut all_terminal = true;
        for id in ids.iter().take(50).filter_map(Value::as_str) {
            let mut item = json!({"run_id":id,"status":"error","error":"worker_run_not_found"});
            if let Ok(run) = read_run(root, id) {
                all_terminal &= run.get("status").and_then(Value::as_str) != Some("running");
                item = json!({"run_id":id,"status":"ok","run":if compact{minimal_run(&run)}else{compact_run(&run)}});
            }
            runs.push(item);
        }
        if all_terminal || started.elapsed() >= Duration::from_millis(timeout_ms) {
            break runs;
        }
        thread::sleep(Duration::from_millis(poll_ms.min(timeout_ms.saturating_sub(started.elapsed().as_millis() as u64))));
    };
    let timed_out = runs.iter().any(|item| item.pointer("/run/status").and_then(Value::as_str) == Some("running"));
    Ok(
        json!({"schema":"narada.worker.run_wait_batch.v1","status":"ok","requested_count":ids.len().min(50),"site_scope":"current_site","compact":compact,"wait":{"status":if timed_out{"timed_out"}else{"finished"},"timeout_ms":timeout_ms,"poll_ms":poll_ms,"waited_ms":started.elapsed().as_millis() as u64},"runs":runs,"native_read_only":true}),
    )
}
fn runs_synthesize(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let ids = args
        .get("run_ids")
        .and_then(Value::as_array)
        .ok_or_else(|| error("worker_run_ids_required", "worker_run_ids_required"))?;
    let mut counts = Map::new();
    let mut found = Vec::new();
    for id in ids.iter().take(50).filter_map(Value::as_str) {
        if let Ok(run) = read_run(root, id) {
            let status = run
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            *counts.entry(status.to_string()).or_insert(Value::from(0)) =
                Value::from(counts.get(status).and_then(Value::as_u64).unwrap_or(0) + 1);
            found.push(id);
        }
    }
    Ok(
        json!({"schema":"narada.worker.runs_synthesis.v1","status":"ok","requested_count":ids.len().min(50),"run_ids":found,"synthesis":{"counts":counts,"native_read_only":true}}),
    )
}
