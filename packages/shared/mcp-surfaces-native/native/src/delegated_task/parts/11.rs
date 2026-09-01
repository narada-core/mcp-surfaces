fn valid_condition(condition: &str) -> bool {
    let value = condition.trim();
    if matches!(
        value,
        "always" | "on_success" | "on_failure" | "review_failed" | "no_residual_risks"
    ) {
        return true;
    }
    if let Some(suffix) = value.strip_prefix("acceptance:") {
        return !suffix.trim().is_empty();
    }
    if let Some(suffix) = value.strip_prefix("result_has:") {
        return !suffix.trim().is_empty();
    }
    let parts = value.split(':').collect::<Vec<_>>();
    if parts.first() == Some(&"step") {
        return parts.len() == 3
            && !parts[1].is_empty()
            && matches!(
                parts[2],
                "pending" | "running" | "completed" | "failed" | "skipped" | "blocked" | "noted"
            );
    }
    if parts.first() == Some(&"kind") {
        return parts.len() == 3 && !parts[1].is_empty() && !parts[2].is_empty();
    }
    parse_condition_call(value).is_some_and(|(name, args)| {
        ((name == "all" || name == "any") && args.len() >= 2 || name == "not" && args.len() == 1)
            && args.into_iter().all(valid_condition)
    })
}
fn condition_passes(condition: Option<&str>, task: &Value) -> bool {
    let Some(value) = condition.map(str::trim).filter(|value| !value.is_empty()) else {
        return true;
    };
    match value {
        "always" => true,
        "on_failure" => task
            .pointer("/result/step_states")
            .and_then(Value::as_object)
            .is_some_and(|states| {
                states.values().any(|state| {
                    matches!(
                        state.get("status").and_then(Value::as_str),
                        Some("failed" | "blocked")
                    )
                })
            }),
        "on_success" => task
            .pointer("/result/step_states")
            .and_then(Value::as_object)
            .is_none_or(|states| {
                states.values().all(|state| {
                    !matches!(
                        state.get("status").and_then(Value::as_str),
                        Some("failed" | "blocked")
                    )
                })
            }),
        "review_failed" => task
            .pointer("/result/step_states")
            .and_then(Value::as_object)
            .is_some_and(|states| {
                states.values().any(|state| {
                    state.get("kind").and_then(Value::as_str) == Some("review")
                        && state.get("status").and_then(Value::as_str) == Some("failed")
                })
            }),
        "no_residual_risks" => task
            .pointer("/result/residual_risks")
            .and_then(Value::as_array)
            .is_none_or(Vec::is_empty),
        _ if value.starts_with("acceptance:") => {
            task.pointer("/result/acceptance_verdict")
                .and_then(Value::as_str)
                == Some(&value[11..])
        }
        _ if value.starts_with("step:") => {
            let parts = value.split(':').collect::<Vec<_>>();
            parts.len() == 3 && step_status(task, parts[1]) == Some(parts[2])
        }
        _ if value.starts_with("kind:") => {
            let parts = value.split(':').collect::<Vec<_>>();
            parts.len() == 3
                && task
                    .pointer("/result/step_states")
                    .and_then(Value::as_object)
                    .is_some_and(|states| {
                        let matching = states
                            .values()
                            .filter(|state| {
                                state.get("kind").and_then(Value::as_str) == Some(parts[1])
                            })
                            .collect::<Vec<_>>();
                        !matching.is_empty()
                            && matching.iter().all(|state| {
                                state.get("status").and_then(Value::as_str) == Some(parts[2])
                            })
                    })
        }
        _ if value.starts_with("result_has:") => task
            .get("result")
            .is_some_and(|result| result.to_string().contains(&value[11..])),
        _ => parse_condition_call(value).is_some_and(|(name, args)| match name {
            "all" => args.len() >= 2 && args.iter().all(|arg| condition_passes(Some(arg), task)),
            "any" => args.len() >= 2 && args.iter().any(|arg| condition_passes(Some(arg), task)),
            "not" => args.len() == 1 && !condition_passes(Some(args[0]), task),
            _ => false,
        }),
    }
}
fn max_retries(task: &Value) -> u64 {
    task.pointer("/constraints/max_retries")
        .or_else(|| task.pointer("/execution/max_retries"))
        .and_then(Value::as_u64)
        .unwrap_or(0)
        .min(10)
}
fn max_concurrency(task: &Value) -> usize {
    task.pointer("/constraints/max_concurrency")
        .or_else(|| task.pointer("/execution/max_concurrency"))
        .and_then(Value::as_u64)
        .unwrap_or(1)
        .clamp(1, 32) as usize
}
