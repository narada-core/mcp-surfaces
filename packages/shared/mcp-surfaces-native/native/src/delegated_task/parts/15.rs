fn advance_task_closure(
    root: &Path,
    id: &str,
    allowed_roots: &[PathBuf],
    visiting: &mut std::collections::BTreeSet<String>,
) -> Result<Value, Value> {
    if !visiting.insert(id.to_string()) {
        return Err(json!({"schema":"narada.delegated_task.error.v1","code":"task_dependency_cycle","message":"task_dependency_cycle","task_id":id}));
    }
    let snapshot = read_task(root, id)?;
    let dependencies = snapshot.get("depends_on_task_ids").and_then(Value::as_array).cloned().unwrap_or_default();
    for dependency in dependencies.iter().filter_map(Value::as_str) {
        let _ = advance_task_closure(root, dependency, allowed_roots, visiting)?;
    }
    visiting.remove(id);
    let _lock = lock_task(root, id)?;
    let current = read_task(root, id)?;
    advance_value_with_roots(current, root, allowed_roots)
}
fn task_cancel(args: &Map<String, Value>, root: &Path, takeover: bool) -> Result<Value, Value> {
    let id = task_id(args)?;
    let _lock = lock_task(root, &id)?;
    let mut task = read_task(root, &id)?;
    let ownership = assert_mutation_scope(&task, args, root)?;
    if matches!(
        task.get("status").and_then(Value::as_str),
        Some("completed" | "failed" | "cancelled")
    ) {
        return Err(error(
            "delegated_task_terminal_status",
            "delegated_task_terminal_status",
        ));
    }
    task["status"] = json!("cancelled");
    finalize_timing(&mut task);
    task["updated_at"] = json!(now());
    let kind = if takeover {
        "task_parent_takeover"
    } else {
        "task_cancelled"
    };
    let detail = if takeover {
        json!({"parent_task_id":args.get("parent_task_id"),"reason":args.get("reason")})
    } else {
        json!({"reason":args.get("reason")})
    };
    task["result"][if takeover {
        "parent_takeover"
    } else {
        "cancellation"
    }] = detail.clone();
    write_task(root, &task)?;
    let event = append_event(root, &id, kind, detail)?;
    Ok(
        json!({"schema":if takeover{"narada.delegated_task.parent_takeover.v1"}else{"narada.delegated_task.cancel.v1"},"status":if takeover{"parent_takeover_recorded"}else{"cancelled"},"task_id":id,"task_status":"cancelled","ownership":ownership,"event":event}),
    )
}
fn task_acknowledge(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let id = task_id(args)?;
    let _lock = lock_task(root, &id)?;
    let mut task = read_task(root, &id)?;
    let ownership = assert_mutation_scope(&task, args, root)?;
    if !matches!(
        task.get("status").and_then(Value::as_str),
        Some("completed" | "failed" | "cancelled")
    ) {
        return Err(error(
            "delegated_task_not_terminal",
            "delegated_task_not_terminal",
        ));
    }
    let ack = json!({"acknowledged":true,"acknowledged_at":now(),"acknowledged_by":args.get("acknowledged_by"),"note":args.get("note")});
    task["result"]["lifecycle_acknowledgement"] = ack.clone();
    task["updated_at"] = json!(now());
    write_task(root, &task)?;
    let event = append_event(root, &id, "task_acknowledged", ack.clone())?;
    Ok(
        json!({"schema":"narada.delegated_task.acknowledge.v1","status":"acknowledged","task_id":id,"task_status":task["status"],"ownership":ownership,"acknowledgement":ack,"event":event}),
    )
}

fn id_schema(required: bool) -> Value {
    json!({"type":"object","properties":{"task_id":{"type":"string"}},"required":if required {json!(["task_id"])} else {json!([])},"additionalProperties":false})
}
fn error(code: &str, message: &str) -> Value {
    json!({"schema":"narada.delegated_task.error.v1","code":code,"message":message})
}
fn tool(name: &str, description: &str, schema: Value, read_only: bool) -> Value {
    let destructive = matches!(name, "delegated_task_cancel" | "delegated_task_parent_takeover");
    json!({"name":name,"description":description,"annotations":{"title":name,"readOnlyHint":read_only,"destructiveHint":destructive,"stateChangingHint":!read_only,"idempotentHint":read_only,"openWorldHint":false},"inputSchema":schema,"outputSchema":{"type":"object","additionalProperties":true}})
}

