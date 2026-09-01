fn lock_task(root: &Path, id: &str) -> Result<TaskLock, Value> {
    let path = task_path(root, id)?.with_file_name("mutation.lockdir");
    fs::create_dir_all(path.parent().unwrap())
        .map_err(|_| error("delegated_task_lock_failed", "delegated_task_lock_failed"))?;
    let timeout_ms = std::env::var("NARADA_DELEGATED_TASK_LOCK_TIMEOUT_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(30_000)
        .clamp(100, 30_000);
    let stale_ms = std::env::var("NARADA_DELEGATED_TASK_LOCK_STALE_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(300_000)
        .clamp(1_000, 86_400_000);
    let started = std::time::Instant::now();
    loop {
        match fs::create_dir(&path) {
            Ok(()) => {
                let owner_path = path.join("owner.json");
                let token = format!(
                    "{}-{}",
                    std::process::id(),
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_nanos()
                );
                fs::write(&owner_path, json!({"schema":"narada.delegated_task.mutation_lock.v1","token":token,"pid":std::process::id(),"heartbeat_at":now()}).to_string())
                    .map_err(|_| error("delegated_task_lock_failed", "delegated_task_lock_failed"))?;
                let stop = Arc::new(AtomicBool::new(false));
                let heartbeat_stop = Arc::clone(&stop);
                let heartbeat_path = path.clone();
                let heartbeat_token = token.clone();
                let heartbeat_interval =
                    std::time::Duration::from_millis((stale_ms / 3).clamp(100, 1_000));
                let heartbeat = std::thread::spawn(move || {
                    while !heartbeat_stop.load(Ordering::Acquire) {
                        std::thread::park_timeout(heartbeat_interval);
                        if heartbeat_stop.load(Ordering::Acquire) {
                            break;
                        }
                        if !lock_owner_matches(&heartbeat_path, &heartbeat_token) {
                            break;
                        }
                        let _ = fs::write(&owner_path, json!({"schema":"narada.delegated_task.mutation_lock.v1","token":heartbeat_token,"pid":std::process::id(),"heartbeat_at":now()}).to_string());
                    }
                });
                return Ok(TaskLock {
                    path,
                    token,
                    stop,
                    heartbeat: Some(heartbeat),
                });
            }
            Err(error_value)
                if error_value.kind() == std::io::ErrorKind::AlreadyExists
                    && lock_stale(&path, stale_ms) =>
            {
                let _ = reclaim_stale_lock(&path);
            }
            Err(error_value)
                if error_value.kind() == std::io::ErrorKind::AlreadyExists
                    && started.elapsed() < std::time::Duration::from_millis(timeout_ms) =>
            {
                std::thread::sleep(std::time::Duration::from_millis(25))
            }
            Err(_) => {
                return Err(error(
                    "delegated_task_lock_failed",
                    "delegated_task_lock_failed",
                ))
            }
        }
    }
}
fn read_task(root: &Path, id: &str) -> Result<Value, Value> {
    let path = task_path(root, id)?;
    let size = fs::metadata(&path)
        .map_err(|_| error("delegated_task_not_found", "delegated_task_not_found"))?
        .len();
    if size > MAX_FILE_BYTES {
        return Err(error(
            "delegated_task_record_too_large",
            "delegated_task_record_too_large",
        ));
    }
    let text = fs::read_to_string(&path)
        .map_err(|_| error("delegated_task_not_found", "delegated_task_not_found"))?;
    serde_json::from_str(&text).map_err(|e| error("delegated_task_invalid_json", &e.to_string()))
}
fn task_id(args: &Map<String, Value>) -> Result<String, Value> {
    args.get("task_id")
        .and_then(Value::as_str)
        .filter(|v| !v.trim().is_empty())
        .map(|v| v.trim().to_string())
        .ok_or_else(|| error("task_id_required", "task_id_required"))
}

#[cfg(test)]
fn policy(root: &Path) -> Value {
    policy_with_roots(root, &[root.to_path_buf()])
}
fn policy_with_roots(root: &Path, allowed_roots: &[PathBuf]) -> Value {
    json!({"schema":"narada.delegated_task.policy.v1","status":"ok","server_name":SERVER_NAME,"task_root":task_root(root).to_string_lossy(),"site_root":root.to_string_lossy(),"allowed_roots":allowed_roots.iter().map(|path| path.to_string_lossy().to_string()).collect::<Vec<_>>(),"default_cognition":DEFAULT_COGNITION,"list_defaults":{"view":"active_queue","site_scope":"current_site"},"workflow_engine":"native_authority","worker_execution":"native_worker_authority","result_compaction":{"max_worker_refs":50,"max_list_items":200}})
}

fn assessment_output_schema() -> Value {
    json!({"schema":"narada.delegated_task.output_schema.v1","name":"task_executability_assessment_v1","version":1,"required":["dimensions","first_actions","reference_resolutions","acceptance_mappings","required_decisions","findings","assessment_result","evaluator_provenance"],"fields":{"dimensions":"array<object>","first_actions":"array<object>","reference_resolutions":"array<object>","acceptance_mappings":"array<object>","required_decisions":"array<object>","findings":"array<object>","assessment_result":"object {status: executable|blocked|not_executable, implementation_ready: boolean, blockers: array<object>, reason: string when not_executable}","evaluator_provenance":"object"},"conditional_rules":[{"when":"assessment_result.status=executable","requires":["assessment_result.implementation_ready=true","assessment_result.blockers=[]"]},{"when":"assessment_result.status=blocked","requires":["assessment_result.implementation_ready=false","assessment_result.blockers nonempty"]},{"when":"assessment_result.status=not_executable","requires":["assessment_result.implementation_ready=false","assessment_result.reason nonempty"]}],"provenance_required":["runtime","provider","model","cognition","profile_version"],"rejection_rules":["missing_required_field","prose_only","invalid_schema","invalid_provenance"]})
}

fn assessment_template() -> Value {
    let output_schema = assessment_output_schema();
    json!({"template_id":"task_executability_assessment_v1","strategy":"task_executability_assessment_v1","title":"Bounded Shoshin task executability assessment","profile_version":"shoshin-task-executability-v1","purpose":"Assess one canonical task snapshot without changing it.","idempotency":{"schema":"narada.task.executability.idempotency.v1","inputs":["request_id","task_digest","environment_digest","profile_version"],"formula":"sha256(canonical_json({request_id, task_digest, environment_digest, profile_version}))"},"bounds":{"authority":"read","cognition":"low","runtime":"narada-agent-runtime-server","max_worker_runs":1,"max_run_ms":300000,"max_retries":0,"max_result_items":32,"max_events":32,"write_set":[]},"result_policy":{"expose_worker_refs":true,"compact_completed_worker_refs":true,"max_events":32,"max_worker_refs":1,"max_result_items":32},"output_schema":output_schema,"milestones":[{"id":"assessment","title":"Assess canonical task snapshot","step_ids":["assessment"]}],"steps":[{"id":"assessment","kind":"worker","profile":"shoshin-task-executability-v1","milestone_id":"assessment","write_set":[],"constraints":{"authority":"read","cognition":"low","runtime":"narada-agent-runtime-server","max_run_ms":300000,"max_retries":0,"max_concurrency":1,"wait_for_completion":false,"resumable":false,"required_mcp_tools":[],"preflight_paths":[],"overrides":{"skip_git_repo_check":true}},"output_schema":output_schema}],"worker_delegation_contract":{"surface_id":"worker-delegation","caller_sets_worker_constraints":true,"worker_run_is_child_execution":true,"required_worker_output_fields":["summary","structured_outputs","verification","target_state_changed"],"forbidden_authorities":["write","command"],"required_structured_output":"task_executability_assessment_v1"}})
}

fn worker_contract(step_kinds: &[&str]) -> Value {
    json!({"surface_id":"worker-delegation","routed_feedback_ids":["sfb_7e043d77-074"],"caller_sets_worker_constraints":true,"worker_run_is_child_execution":true,"required_worker_output_fields":["summary","changes","verification","residual_risks","observed_incoherencies"],"step_kinds":step_kinds})
}

fn authority_gates(commit_reason: &str, push_reason: &str) -> Value {
    json!({"commit":{"operation":"commit","mode":"requires_explicit_authority","reason":commit_reason,"required_authority":"write"},"push":{"operation":"push","mode":"requires_explicit_authority","reason":push_reason,"required_authority":"command"}})
}

fn workflow_templates() -> Vec<Value> {
    vec![
        assessment_template(),
        json!({"template_id":"implement","strategy":"implement","title":"Single implementation worker","feedback_ids":["sfb_f1ea42cb-062","sfb_ac8a8731-f1c"],"milestones":[{"id":"implement","title":"Implement","step_ids":["implement"]}],"steps":[{"id":"implement","kind":"worker","milestone_id":"implement"}],"worker_delegation_contract":worker_contract(&["worker"])}),
        json!({"template_id":"implement_review","strategy":"implement_review","title":"Implementation with review quorum evidence","feedback_ids":["sfb_f1ea42cb-062","sfb_ac8a8731-f1c","sfb_7e043d77-074"],"milestones":[{"id":"implement","title":"Implement","step_ids":["implement"]},{"id":"review","title":"Review","depends_on":["implement"],"step_ids":["review"]}],"steps":[{"id":"implement","kind":"worker","milestone_id":"implement"},{"id":"review","kind":"review","milestone_id":"review","depends_on":["implement"]}],"worker_delegation_contract":worker_contract(&["worker","review"])}),
        json!({"template_id":"research_synthesize","strategy":"research_synthesize","title":"Research, synthesize, and review","feedback_ids":["sfb_074b9629-4a8","sfb_f1ea42cb-062"],"milestones":[{"id":"research","title":"Research","step_ids":["research"]},{"id":"synthesize","title":"Synthesize","depends_on":["research"],"step_ids":["synthesize","review"]}],"steps":[{"id":"research","kind":"research","milestone_id":"research"},{"id":"synthesize","kind":"worker","milestone_id":"synthesize","depends_on":["research"]},{"id":"review","kind":"review","milestone_id":"synthesize","depends_on":["synthesize"]}],"worker_delegation_contract":worker_contract(&["research","worker","review"])}),
        json!({"template_id":"implement_review_repair_verify","strategy":"implement_review_repair_verify","title":"Implementation, review, conditional repair, and verify","feedback_ids":["sfb_6924c7b3-48f","sfb_074b9629-4a8","sfb_f1ea42cb-062","sfb_ac8a8731-f1c","sfb_7e043d77-074"],"milestones":[{"id":"implement","title":"Implement","step_ids":["implement"]},{"id":"review","title":"Review","depends_on":["implement"],"step_ids":["review"]},{"id":"repair","title":"Repair if needed","depends_on":["review"],"step_ids":["repair"]},{"id":"verify","title":"Verify","depends_on":["repair"],"step_ids":["verify"]}],"steps":[{"id":"implement","kind":"worker","milestone_id":"implement"},{"id":"review","kind":"review","milestone_id":"review","depends_on":["implement"]},{"id":"repair","kind":"repair","milestone_id":"repair","depends_on":["review"],"if":"review_failed"},{"id":"verify","kind":"verify","milestone_id":"verify","depends_on":["repair"]}],"authority_gates":authority_gates("commit is modeled as an explicit gate and is never executed by delegated-task-mcp","push must stay opt-in and owned by caller policy or worker constraints"),"worker_delegation_contract":worker_contract(&["worker","review","repair","verify"])}),
        json!({"template_id":"commit_push_guarded","strategy":"commit_push_guarded","title":"Review-gated commit and push publication handoff","feedback_ids":["sfb_98a64342-379","sfb_7e043d77-074"],"milestones":[{"id":"prepare","title":"Prepare evidence","step_ids":["prepare"]},{"id":"review","title":"Review publication readiness","depends_on":["prepare"],"step_ids":["review"]},{"id":"publication-gate","title":"Publication authority gate","depends_on":["review"],"step_ids":["commit-gate","push-gate"]}],"authority_gates":authority_gates("commit only after explicit caller authority","push only after explicit command authority"),"steps":[{"id":"prepare","kind":"worker","milestone_id":"prepare"},{"id":"review","kind":"review","milestone_id":"review","depends_on":["prepare"]},{"id":"commit-gate","kind":"gate","milestone_id":"publication-gate","depends_on":["review"],"if":"all(step:review:completed,no_residual_risks)","authority_gate":{"operation":"commit","mode":"requires_explicit_authority","required_authority":"write"}},{"id":"push-gate","kind":"gate","milestone_id":"publication-gate","depends_on":["commit-gate"],"if":"acceptance:passed","authority_gate":{"operation":"push","mode":"requires_explicit_authority","required_authority":"command"}}],"worker_delegation_contract":worker_contract(&["worker","review"])}),
    ]
}

fn compact_template(template: &Value) -> Value {
    let stages = template
        .get("milestones")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .map(|item| {
                    item.get("title")
                        .or_else(|| item.get("id"))
                        .cloned()
                        .unwrap_or(Value::Null)
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    json!({
        "template_id":template.get("template_id"),
        "title":template.get("title"),
        "stages":stages,
        "authority":if template.get("authority_gates").is_some(){"native task authority with explicit publication gates"}else{"native worker authority"},
        "best_for":template_fit(template.get("template_id").and_then(Value::as_str)).0,
        "avoid_when":template_fit(template.get("template_id").and_then(Value::as_str)).1,
        "detail_available":true
    })
}

fn template_fit(id: Option<&str>) -> (Value, Value) {
    match id.unwrap_or_default() {
        "task_executability_assessment_v1" => (json!(["bounded pre-implementation feasibility and blocker assessment"]), json!(["the objective is already approved for direct implementation"])),
        "implement" => (json!(["one bounded implementation or verification step"]), json!(["independent review or repair loops are required"])),
        "implement_review" => (json!(["implementation requiring an independent review gate"]), json!(["a single trivial worker result is sufficient"])),
        "research_synthesize" => (json!(["evidence gathering followed by synthesis and review"]), json!(["the task is a deterministic code edit"])),
        "implement_review_repair_verify" => (json!(["high-risk changes needing review, repair, and final verification"]), json!(["latency matters more than redundant assurance"])),
        "commit_push_guarded" => (json!(["explicitly authorized commit and push workflows"]), json!(["publication authority has not been granted"])),
        _ => (json!([]), json!([])),
    }
}

fn template_catalog(args: &Map<String, Value>) -> Value {
    let id = args
        .get("template_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let mode = args
        .get("mode")
        .and_then(Value::as_str)
        .unwrap_or(if id.is_some() { "detail" } else { "compact" });
    let templates = workflow_templates()
        .into_iter()
        .filter(|value| id.is_none() || value.get("template_id").and_then(Value::as_str) == id)
        .collect::<Vec<_>>();
    let details = mode == "detail" || id.is_some();
    let projected = if details {
        templates.into_iter().map(|mut template| {
            let (best_for, avoid_when) = template_fit(template.get("template_id").and_then(Value::as_str));
            template["best_for"] = best_for;
            template["avoid_when"] = avoid_when;
            template
        }).collect()
    } else {
        templates.iter().map(compact_template).collect::<Vec<_>>()
    };
    json!({"schema":"narada.delegated_task.template_catalog.v1","status":if id.is_some() && projected.is_empty(){"not_found"}else{"ok"},"mode":mode,"template_id":id,"count":projected.len(),"templates":projected})
}

fn workflow_diagnostics(workflow: &Value) -> Vec<Value> {
    let steps = workflow
        .get("steps")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut diagnostics = Vec::new();
    let mut ids = std::collections::HashSet::new();
    for step in &steps {
        let Some(id) = step.get("id").and_then(Value::as_str) else {
            diagnostics.push(json!({"severity":"error","code":"step_id_required"}));
            continue;
        };
        if !ids.insert(id.to_string()) {
            diagnostics.push(json!({"severity":"error","code":"duplicate_step_id","step_id":id}));
        }
    }
    for step in &steps {
        let Some(id) = step.get("id").and_then(Value::as_str) else {
            continue;
        };
        let kind = step.get("kind").and_then(Value::as_str).unwrap_or("worker");
        if !matches!(
            kind,
            "worker" | "review" | "repair" | "verify" | "research" | "gate" | "join" | "note"
        ) {
            diagnostics.push(json!({"severity":"error","code":"workflow_policy_violation","step_id":id,"kind":kind}));
        }
        if let Some(condition) = step.get("if").and_then(Value::as_str) {
            if !valid_condition(condition) {
                diagnostics.push(json!({"severity":"error","code":"invalid_condition","step_id":id,"condition":condition}));
            }
        }
        for dependency in step
            .get("depends_on")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
        {
            if !ids.contains(dependency) {
                diagnostics.push(json!({"severity":"error","code":"unknown_dependency","step_id":id,"dependency":dependency}));
            }
        }
    }
    let mut resolved = std::collections::HashSet::new();
    loop {
        let before = resolved.len();
        for step in &steps {
            let Some(id) = step.get("id").and_then(Value::as_str) else {
                continue;
            };
            if resolved.contains(id) {
                continue;
            }
            let ready = step
                .get("depends_on")
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(Value::as_str)
                        .all(|dependency| resolved.contains(dependency))
                })
                .unwrap_or(true);
            if ready {
                resolved.insert(id.to_string());
            }
        }
        if resolved.len() == before {
            break;
        }
    }
    if resolved.len() < ids.len() {
        diagnostics.push(json!({"severity":"error","code":"workflow_cycle"}));
    }
    diagnostics
}
