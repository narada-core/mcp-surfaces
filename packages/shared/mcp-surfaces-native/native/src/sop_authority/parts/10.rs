fn validate_step_references(steps: &[Value]) -> Result<(), Value> {
    let by_id = steps
        .iter()
        .filter_map(|step| Some((step.get("id")?.as_str()?.to_string(), step)))
        .collect::<HashMap<_, _>>();
    fn ancestors<'a>(id: &str, by_id: &'a HashMap<String, &'a Value>, found: &mut HashSet<String>) {
        for dependency in by_id
            .get(id)
            .and_then(|step| step.get("depends_on"))
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
        {
            if found.insert(dependency.to_string()) {
                ancestors(dependency, by_id, found);
            }
        }
    }
    for step in steps {
        let id = step.get("id").and_then(Value::as_str).unwrap_or("");
        let mut allowed = HashSet::new();
        ancestors(id, &by_id, &mut allowed);
        let mut referenced = HashSet::new();
        for field in ["when", "input", "input_ref"] {
            collect_step_references(step.get(field), &mut referenced)?;
        }
        collect_step_references(
            step.get("action").and_then(|value| value.get("arguments")),
            &mut referenced,
        )?;
        collect_instruction_references(
            step.get("instructions")
                .and_then(Value::as_str)
                .unwrap_or(""),
            &mut referenced,
        )?;
        for target in referenced {
            if !by_id.contains_key(&target) {
                return Err(diagnostic(
                    "sop_step_reference_unknown",
                    "sop_step_reference_unknown",
                    json!({"step_id":id,"referenced_step_id":target}),
                ));
            }
            if !allowed.contains(&target) {
                return Err(diagnostic(
                    "sop_step_reference_not_dependency",
                    "sop_step_reference_not_dependency",
                    json!({"step_id":id,"referenced_step_id":target}),
                ));
            }
        }
    }
    Ok(())
}

fn validate_output_references(mapping: Option<&Value>, steps: &Value) -> Result<(), Value> {
    let Some(mapping) = mapping else {
        return Ok(());
    };
    let ids = steps
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|step| step.get("id").and_then(Value::as_str))
        .collect::<HashSet<_>>();
    let mut referenced = HashSet::new();
    collect_step_references(Some(mapping), &mut referenced)?;
    for target in referenced {
        if !ids.contains(target.as_str()) {
            return Err(diagnostic(
                "sop_output_reference_unknown",
                &format!("sop_output_reference_unknown:{target}"),
                json!({}),
            ));
        }
    }
    Ok(())
}

fn collect_step_references(
    value: Option<&Value>,
    output: &mut HashSet<String>,
) -> Result<(), Value> {
    let Some(value) = value else {
        return Ok(());
    };
    match value {
        Value::Array(values) => {
            for value in values {
                collect_step_references(Some(value), output)?;
            }
        }
        Value::Object(object) => {
            if let Some(reference) = object.get("ref").and_then(Value::as_str) {
                validate_reference(reference)?;
                add_step_reference(reference, output);
            }
            if object.len() == 1 {
                if let Some(reference) = object.get("$ref").and_then(Value::as_str) {
                    validate_reference(reference)?;
                    add_step_reference(reference, output);
                }
            }
            for value in object.values() {
                collect_step_references(Some(value), output)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn collect_instruction_references(text: &str, output: &mut HashSet<String>) -> Result<(), Value> {
    let mut remaining = text;
    while let Some(open) = remaining.find("{{") {
        let after = &remaining[open + 2..];
        let Some(close) = after.find("}}") else { break };
        let reference = after[..close].trim();
        validate_reference(reference)?;
        add_step_reference(reference, output);
        remaining = &after[close + 2..];
    }
    Ok(())
}

fn validate_reference(reference: &str) -> Result<(), Value> {
    let parts = reference.split('.').collect::<Vec<_>>();
    let safe = !parts.is_empty()
        && parts.iter().all(|part| {
            !part.is_empty()
                && *part != "__proto__"
                && *part != "prototype"
                && *part != "constructor"
                && part.chars().all(|character| {
                    character.is_ascii_alphanumeric() || character == '_' || character == '-'
                })
        });
    let valid = reference == "input"
        || reference == "input_ref"
        || (safe && matches!(parts.first(), Some(&"input" | &"input_ref")))
        || (safe
            && parts.len() >= 3
            && parts[0] == "steps"
            && matches!(parts[2], "status" | "result" | "result_ref"));
    if !valid {
        return Err(diagnostic(
            "sop_reference_invalid",
            "sop_reference_invalid",
            json!({"ref":reference}),
        ));
    }
    Ok(())
}

fn add_step_reference(reference: &str, output: &mut HashSet<String>) {
    let parts = reference.split('.').collect::<Vec<_>>();
    if parts.first() == Some(&"steps") && parts.len() >= 2 {
        output.insert(parts[1].to_string());
    }
}

fn normalize_condition(
    value: Option<&Value>,
    depth: usize,
    nodes: &mut usize,
) -> Result<Option<Value>, Value> {
    let Some(value) = value else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    *nodes += 1;
    if depth > 12 || *nodes > 64 {
        return Err(diagnostic(
            "sop_condition_too_complex",
            "sop_condition_too_complex",
            json!({"max_depth":12,"max_nodes":64}),
        ));
    }
    let object = value.as_object().ok_or_else(|| {
        diagnostic(
            "sop_condition_invalid",
            "sop_condition_invalid",
            json!({"reason":"condition_must_be_object"}),
        )
    })?;
    if object.len() == 1 {
        for key in ["all", "any"] {
            if let Some(raw) = object.get(key) {
                let values = raw
                    .as_array()
                    .filter(|values| !values.is_empty())
                    .ok_or_else(|| {
                        diagnostic(
                            "sop_condition_invalid",
                            "sop_condition_invalid",
                            json!({"reason":format!("{key}_requires_nonempty_array")}),
                        )
                    })?;
                let mut normalized = Vec::new();
                for value in values {
                    normalized.push(
                        normalize_condition(Some(value), depth + 1, nodes)?.unwrap_or(Value::Null),
                    );
                }
                return Ok(Some(json!({key:normalized})));
            }
        }
        if let Some(raw) = object.get("not") {
            let normalized =
                normalize_condition(Some(raw), depth + 1, nodes)?.ok_or_else(|| {
                    diagnostic("sop_condition_invalid", "sop_condition_invalid", json!({}))
                })?;
            return Ok(Some(json!({"not":normalized})));
        }
    }
    if object
        .keys()
        .any(|key| !matches!(key.as_str(), "ref" | "op" | "value"))
    {
        return Err(diagnostic(
            "sop_condition_invalid",
            "sop_condition_invalid",
            json!({"reason":"unknown_fields"}),
        ));
    }
    let reference = required_string(object.get("ref"), "sop_condition_invalid", 512)?;
    validate_reference(&reference)?;
    let op = required_string(object.get("op"), "sop_condition_invalid", 32)?;
    if !matches!(
        op.as_str(),
        "equals" | "not_equals" | "exists" | "not_exists" | "truthy" | "falsy" | "in" | "contains"
    ) {
        return Err(diagnostic(
            "sop_condition_invalid",
            "sop_condition_invalid",
            json!({"reason":"unsupported_operator","op":op}),
        ));
    }
    if !matches!(op.as_str(), "exists" | "not_exists" | "truthy" | "falsy")
        && !object.contains_key("value")
    {
        return Err(diagnostic(
            "sop_condition_invalid",
            "sop_condition_invalid",
            json!({"reason":"operator_requires_value","op":op}),
        ));
    }
    if op == "in" && !object.get("value").map(Value::is_array).unwrap_or(false) {
        return Err(diagnostic(
            "sop_condition_invalid",
            "sop_condition_invalid",
            json!({"reason":"in_value_must_be_array"}),
        ));
    }
    let mut normalized = Map::new();
    normalized.insert("ref".to_string(), json!(reference));
    normalized.insert("op".to_string(), json!(op));
    if let Some(value) = object.get("value") {
        assert_bound(value, "sop_condition_value", MAX_INLINE_VALUE_BYTES)?;
        normalized.insert("value".to_string(), value.clone());
    }
    Ok(Some(Value::Object(normalized)))
}

