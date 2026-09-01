fn ready_step_ids(task: &Value) -> Vec<String> {
    task.pointer("/workflow/steps")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|step| {
            let id = step.get("id").and_then(Value::as_str)?;
            if step_status(task, id) != Some("pending") {
                return None;
            }
            let ready = step
                .get("depends_on")
                .and_then(Value::as_array)
                .map(|dependencies| {
                    dependencies
                        .iter()
                        .filter_map(Value::as_str)
                        .all(|dependency| {
                            matches!(
                                step_status(task, dependency),
                                Some("completed" | "skipped" | "noted")
                            )
                        })
                })
                .unwrap_or(true);
            (ready && condition_passes(step.get("if").and_then(Value::as_str), task))
                .then(|| id.to_string())
        })
        .collect()
}
