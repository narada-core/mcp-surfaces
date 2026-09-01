fn task_execute_batch(
    args: &Map<String, Value>,
    root: &Path,
    allowed_roots: &[PathBuf],
) -> Result<Value, Value> {
    let items = args
        .get("items")
        .and_then(Value::as_array)
        .filter(|items| !items.is_empty() && items.len() <= 16)
        .ok_or_else(|| error("delegated_task_batch_items_invalid", "delegated_task_batch_items_invalid"))?
        .iter()
        .map(|item| {
            item.as_object()
                .cloned()
                .ok_or_else(|| error("delegated_task_batch_item_invalid", "delegated_task_batch_item_invalid"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let max_concurrency = args
        .get("max_concurrency")
        .and_then(Value::as_u64)
        .unwrap_or(2)
        .clamp(1, 8) as usize;
    let compact = args.get("compact").and_then(Value::as_bool).unwrap_or(false);
    let worker_count = max_concurrency.min(items.len());
    let next = AtomicUsize::new(0);
    let results = Mutex::new(vec![Value::Null; items.len()]);
    let started = std::time::Instant::now();
    std::thread::scope(|scope| {
        for _ in 0..worker_count {
            scope.spawn(|| loop {
                let index = next.fetch_add(1, Ordering::Relaxed);
                let Some(item) = items.get(index) else { break };
                let value = match task_execute(item, root, allowed_roots) {
                    Ok(result) => {
                        let result = if compact { compact_batch_result(&result) } else { result };
                        json!({"index":index,"status":"ok","result":result})
                    },
                    Err(failure) => json!({"index":index,"status":"failed","error":failure}),
                };
                results.lock().expect("batch result lock")[index] = value;
            });
        }
    });
    let results = results.into_inner().expect("batch result lock");
    let failed_count = results.iter().filter(|item| item["status"] == "failed").count();
    Ok(json!({
        "schema":"narada.delegated_task.execute_batch.v1",
        "status":if failed_count == 0 {"completed"} else {"partial_failure"},
        "requested_count":results.len(),
        "completed_count":results.len() - failed_count,
        "failed_count":failed_count,
        "max_concurrency":max_concurrency,
        "compact":compact,
        "elapsed_ms":started.elapsed().as_millis() as u64,
        "results":results
    }))
}

fn compact_batch_result(result: &Value) -> Value {
    let handoff = result.pointer("/terminal/terminal_handoff").unwrap_or(&Value::Null);
    json!({
        "task_id":handoff.get("task_id").or_else(|| result.pointer("/run/task_id")),
        "task_status":handoff.get("task_status"),
        "summary":handoff.get("summary"),
        "output_contract_verdict":handoff.get("output_contract_verdict"),
        "objective_verdict":handoff.get("objective_verdict"),
        "acceptance_verdict":handoff.get("acceptance_verdict"),
        "timing":handoff.get("timing"),
        "details_ref":handoff.get("details_ref"),
        "idempotency_replay":result.get("idempotency_replay")
    })
}
#[cfg(test)]
fn task_wait(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    task_wait_with_roots(args, root, &[root.to_path_buf()])
}
fn task_wait_with_roots(
    args: &Map<String, Value>,
    root: &Path,
    allowed_roots: &[PathBuf],
) -> Result<Value, Value> {
    let id = task_id(args)?;
    let requested_timeout = args
        .get("timeout_ms")
        .and_then(Value::as_u64)
        .unwrap_or(30000);
    let timeout = requested_timeout.min(MAX_TRANSPORT_SAFE_WAIT_MS);
    let poll = args
        .get("poll_ms")
        .and_then(Value::as_u64)
        .unwrap_or(5000)
        .clamp(50, 30000);
    let started = std::time::Instant::now();
    let task = loop {
        let scope_task = read_task(root, &id)?;
        assert_mutation_scope(&scope_task, args, root)?;
        let current = advance_task_closure(root, &id, allowed_roots, &mut std::collections::BTreeSet::new())?;
        if matches!(
            current.get("status").and_then(Value::as_str),
            Some("completed" | "failed" | "cancelled")
        ) || started.elapsed().as_millis() as u64 >= timeout
        {
            break current;
        }
        std::thread::sleep(std::time::Duration::from_millis(
            poll.min(timeout.saturating_sub(started.elapsed().as_millis() as u64)),
        ))
    };
    let handoff = terminal_handoff(&task, root);
    let task_identity = json!({
        "task_id": task.get("task_id"),
        "task_status": task.get("status"),
        "details_ref": handoff.get("details_ref"),
        "details_tool": handoff.get("details_tool"),
        "role": "identity_only"
    });
    Ok(
        json!({"schema":"narada.delegated_task.wait.v1","status":if task_is_terminal(&task){"finished"}else{"timeout"},"elapsed_ms":started.elapsed().as_millis() as u64,"requested_timeout_ms":requested_timeout,"timeout_ms":timeout,"timeout_clamped_for_transport":requested_timeout > timeout,"poll_ms":poll,"task_id":id,"task_status":task.get("status"),"refresh_performed":true,"worker_execution":"native_worker_authority","canonical_terminal_handoff":task_is_terminal(&task),"readback_tool":"delegated_task_wait","recovery":{"durable":true,"task_id":id,"status_tool":"delegated_task_status","wait_tool":"delegated_task_wait","events_tool":"delegated_task_events"},"result_readback_redundant":task_is_terminal(&task),"terminal_handoff":handoff,"task":task_identity}),
    )
}
fn task_events(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let id = task_id(args)?;
    let path = task_path(root, &id)?.with_file_name("events.jsonl");
    let limit = args
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(50)
        .clamp(1, 100) as usize;
    let offset = args
        .get("offset")
        .and_then(Value::as_u64)
        .unwrap_or(0)
        .min(10000) as usize;
    let mut events = Vec::new();
    if let Ok(metadata) = fs::metadata(&path) {
        if metadata.len() > MAX_FILE_BYTES {
            return Err(error(
                "delegated_task_events_too_large",
                "delegated_task_events_too_large",
            ));
        }
        let text = fs::read_to_string(path).map_err(|_| {
            error(
                "delegated_task_events_read_failed",
                "delegated_task_events_read_failed",
            )
        })?;
        for line in text.lines().skip(offset).take(limit) {
            if let Ok(value) = serde_json::from_str::<Value>(line) {
                events.push(value);
            }
        }
    }
    Ok(
        json!({"schema":"narada.delegated_task.events.v1","status":"ok","task_id":id,"offset":offset,"limit":limit,"count":events.len(),"events":events}),
    )
}

fn now() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default()
}
fn now_ms() -> i128 {
    time::OffsetDateTime::now_utc().unix_timestamp_nanos() / 1_000_000
}
fn finalize_timing(task: &mut Value) {
    if !task_is_terminal(task) || task.get("finished_at_ms").and_then(Value::as_i64).is_some() { return; }
    let finished_ms = now_ms();
    let started_ms = task.get("started_at_ms").and_then(Value::as_i64)
        .or_else(|| task.get("created_at_ms").and_then(Value::as_i64))
        .map(i128::from).unwrap_or(finished_ms);
    task["finished_at"] = json!(now());
    task["finished_at_ms"] = json!(finished_ms);
    task["duration_ms"] = json!(finished_ms.saturating_sub(started_ms));
}
fn write_task(root: &Path, task: &Value) -> Result<(), Value> {
    let id = task
        .get("task_id")
        .and_then(Value::as_str)
        .ok_or_else(|| error("task_id_required", "task_id_required"))?;
    let path = task_path(root, id)?;
    fs::create_dir_all(path.parent().unwrap())
        .map_err(|_| error("delegated_task_write_failed", "delegated_task_write_failed"))?;
    let temp = path.with_extension(format!("tmp-{}", uuid::Uuid::new_v4()));
    fs::write(
        &temp,
        serde_json::to_vec_pretty(task)
            .map_err(|_| error("delegated_task_write_failed", "delegated_task_write_failed"))?,
    )
    .map_err(|_| error("delegated_task_write_failed", "delegated_task_write_failed"))?;
    fs::rename(temp, path)
        .map_err(|_| error("delegated_task_write_failed", "delegated_task_write_failed"))
}
fn append_event(root: &Path, id: &str, kind: &str, payload: Value) -> Result<Value, Value> {
    let event = json!({"schema":"narada.delegated_task.event.v1","event_id":format!("evt_{}",uuid::Uuid::new_v4().simple()),"task_id":id,"event_kind":kind,"recorded_at":now(),"details":payload});
    let path = task_path(root, id)?.with_file_name("events.jsonl");
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|_| {
            error(
                "delegated_task_event_write_failed",
                "delegated_task_event_write_failed",
            )
        })?;
    writeln!(file, "{}", event).map_err(|_| {
        error(
            "delegated_task_event_write_failed",
            "delegated_task_event_write_failed",
        )
    })?;
    Ok(event)
}
fn objective(args: &Map<String, Value>) -> Result<String, Value> {
    args.get("objective")
        .or_else(|| args.get("intent").and_then(|v| v.get("objective")))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
        .ok_or_else(|| {
            error(
                "delegated_task_requires_objective",
                "delegated_task_requires_objective",
            )
        })
}
fn normalize_workflow(input: Option<&Value>) -> Value {
    let mut workflow = input
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    if !workflow.get("steps").is_some_and(Value::is_array) {
        let strategy = workflow
            .get("template_id")
            .or_else(|| workflow.get("strategy"))
            .or_else(|| workflow.get("template"))
            .and_then(Value::as_str);
        let template = strategy.and_then(|id| {
            workflow_templates()
                .into_iter()
                .find(|item| item.get("template_id").and_then(Value::as_str) == Some(id))
        });
        let steps = template
            .and_then(|item| item.get("steps").cloned())
            .unwrap_or_else(|| json!([{"id":"primary","kind":"worker"}]));
        workflow.insert("steps".into(), steps);
    }
    Value::Object(workflow)
}
fn initial_step_states(workflow: &Value) -> Value {
    let mut states = Map::new();
    for step in workflow
        .get("steps")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let Some(id) = step.get("id").and_then(Value::as_str) else {
            continue;
        };
        states.insert(id.to_string(),json!({"step_id":id,"kind":step.get("kind").and_then(Value::as_str).unwrap_or("worker"),"status":"pending","attempts":0,"run_ids":[],"current_run_id":null,"started_at":null,"finished_at":null,"error":null,"summary":null}));
    }
    Value::Object(states)
}
