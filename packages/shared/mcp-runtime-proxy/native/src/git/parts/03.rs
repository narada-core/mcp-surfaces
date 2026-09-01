
fn tool(name: &str, description: &str, read_only: bool) -> Value {
    json!({"name": name, "description": description, "inputSchema": tool_input_schema(name), "annotations": {"title": name, "canonicalName": name, "readOnlyHint": read_only, "destructiveHint": false, "idempotentHint": read_only, "openWorldHint": false}, "outputSchema": {"type": "object", "additionalProperties": true}})
}

fn tool_input_schema(name: &str) -> Value {
    let working_directory = json!({"type": "string", "description": "Repository working directory under an allowed root; omit to use the first allowed root."});
    let path = json!({"type": "string", "description": "Repository-relative explicit path; absolute paths and parent traversal are refused."});
    let paths = json!({"type": "array", "items": path, "minItems": 1});
    let work_scope = json!({"type": "string", "description": "Live work-scope reference returned by git_begin_work_scope."});
    let schema = match name {
        "git_guidance" => {
            json!({"properties": {"workflow": {"type": "string"}, "tool": {"type": "string"}}})
        }
        "git_policy_inspect" => json!({"properties": {}}),
        "git_begin_work_scope" => {
            json!({"properties": {"working_directory": working_directory, "owner_id": {"type": "string", "minLength": 1}, "scope_kind": {"type": "string", "enum": ["paths", "repository_topology"], "default": "paths"}, "allowed_paths": paths, "base_state": {"type": "object", "additionalProperties": false, "properties": {"head": {"type": ["string", "null"]}, "index_digest": {"type": ["string", "null"]}, "worktree_digest": {"type": ["string", "null"]}}}}, "required": ["owner_id"]})
        }
        "git_end_work_scope" => {
            json!({"properties": {"working_directory": working_directory, "owner_id": {"type": "string", "minLength": 1}, "work_scope_ref": work_scope}, "required": ["owner_id", "work_scope_ref"]})
        }
        "git_workflow_record" => {
            json!({"properties": {"workflow_id": {"type": "string"}, "scope_label": {"type": "string"}, "summary": {"type":"string"}, "repositories": {"type": "array", "items": {"type": "object", "additionalProperties":false,"properties":{"working_directory":working_directory,"label":{"type":"string"},"staged_paths":{"type":"array","items":path},"committed_sha":{"type":["string","null"]},"pushed":{"type":"boolean"},"push_status":{"type":"string","enum":["pushed","not_attempted","failed","not_pushable"]},"push_reason":{"type":["string","null"]},"unrelated_dirty_paths_left":{"type":"array","items":path}},"required":["working_directory"]}, "minItems": 1}}, "required": ["scope_label","repositories"]})
        }
        "git_add" | "git_unstage" => {
            json!({"properties": {"working_directory": working_directory, "paths": paths, "work_scope_ref": work_scope}, "required": ["paths"]})
        }
        "git_commit" => {
            json!({"properties": {"working_directory": working_directory, "message": {"type": "string", "minLength": 1}, "body": {"type": "string"}, "work_scope_ref": work_scope, "expected_staged_paths": paths}, "required": ["message", "work_scope_ref"]})
        }
        "git_push" => {
            json!({"properties": {"working_directory": working_directory, "remote": {"type": "string"}, "branch": {"type": "string"}, "expected_commit": {"type": "string", "description": "Expected SHA or git_commit:<sha>."}, "work_scope_ref": work_scope}, "required": ["work_scope_ref"]})
        }
        "git_status" => {
            json!({"properties": {"working_directory": working_directory, "work_scope_ref": work_scope, "pathspec": path, "pathspecs": paths, "staged_only": {"type": "boolean"}, "include_untracked": {"type": "boolean"}, "format": {"type": "string", "enum": ["full", "paths", "summary"]}}})
        }
        "git_sync_status" => json!({"properties": {"working_directory": working_directory}}),
        "git_branch_list" => {
            json!({"properties": {"working_directory": working_directory, "scope": {"type": "string", "enum": ["local", "remote", "all"]}}})
        }
        "git_worktree_list" => json!({"properties": {"working_directory": working_directory}}),
        "git_worktree_add" => json!({"properties": {"working_directory": working_directory, "path": {"type":"string"}, "branch": {"type":"string"}, "new_branch": {"type":"string"}, "start_point": {"type":"string"}, "work_scope_ref": work_scope}, "required":["path","work_scope_ref"]}),
        "git_worktree_remove" => json!({"properties": {"working_directory": working_directory, "path": {"type":"string"}, "work_scope_ref": work_scope}, "required":["path","work_scope_ref"]}),
        "git_worktree_prune" => json!({"properties": {"working_directory": working_directory, "work_scope_ref": work_scope}, "required":["work_scope_ref"]}),
        "git_branch_delete" => json!({"properties": {"working_directory": working_directory, "branch": {"type":"string"}, "merged_into": {"type":"string"}, "work_scope_ref": work_scope}, "required":["branch","merged_into","work_scope_ref"]}),
        "git_branch_delete_remote" => json!({"properties": {"working_directory": working_directory, "remote": {"type":"string"}, "branch": {"type":"string"}, "merged_into": {"type":"string"}, "work_scope_ref": work_scope}, "required":["remote","branch","merged_into","work_scope_ref"]}),
        "git_output_show" => {
            json!({"properties": {"ref": {"type": "string", "description": "Output reference. Supply ref or output_ref; runtime validation rejects an empty request."}, "output_ref": {"type": "string", "description": "Compatibility alias for ref. Supply exactly one reference field."}, "offset": {"type": "integer", "minimum": 0}, "limit": {"type": "integer", "minimum": 1, "maximum": 20000}}})
        }
        "git_changed_summary" => {
            json!({"properties": {"working_directory": working_directory, "pathspecs": paths, "relevance_filters": paths}})
        }
        "git_repositories_summary" => {
            json!({"properties": {"working_directories": {"type": "array", "items": working_directory, "minItems": 1}, "scope_label": {"type":"string"}}, "required": ["working_directories"]})
        }
        "git_diff" => {
            json!({"properties": {"working_directory": working_directory, "scope": {"type": "string", "enum": ["working", "staged", "commit"]}, "commit": {"type": "string"}, "pathspec": path, "pathspecs": paths, "include_untracked": {"type": "boolean","description":"With working scope, append patches for matched untracked files."}, "offset": {"type": "integer", "minimum": 0}, "limit": {"type": "integer", "minimum": 1, "maximum": 50000,"default":4000,"description":"Character page size. Large structured results may be materialized by the transport and read with git_output_show."}}})
        }
        "git_log" => {
            json!({"properties": {"working_directory": working_directory, "limit": {"type": "integer", "minimum": 1, "maximum": 100}, "pathspec": path}})
        }
        "git_show" => {
            json!({"properties": {"working_directory": working_directory, "commit": {"type": "string"}, "pathspec": path, "include_patch": {"type": "boolean"}}, "required": ["commit"]})
        }
        _ => json!({"properties": {}}),
    };
    let mut object = schema;
    object["type"] = json!("object");
    object["additionalProperties"] = json!(false);
    object["title"] = json!(format!("{name}.input"));
    object["maxProperties"] = json!(64);
    bound_schema(&mut object, Some(name));
    object
}

fn bound_schema(schema: &mut Value, field: Option<&str>) {
    let Some(object) = schema.as_object_mut() else {
        return;
    };
    match object.get("type").and_then(Value::as_str) {
        Some("string") if !object.contains_key("maxLength") && !object.contains_key("enum") => {
            let maximum = if field.unwrap_or_default().contains("path")
                || field == Some("working_directory")
            {
                4096
            } else {
                8192
            };
            object.insert("maxLength".into(), json!(maximum));
        }
        Some("array") if !object.contains_key("maxItems") => {
            object.insert("maxItems".into(), json!(256));
        }
        Some("object") if !object.contains_key("maxProperties") => {
            object.insert("maxProperties".into(), json!(256));
        }
        _ => {}
    }
    if object
        .get("type")
        .and_then(Value::as_array)
        .is_some_and(|types| types.iter().any(|kind| kind == "string"))
        && !object.contains_key("maxLength")
    {
        object.insert("maxLength".into(), json!(8192));
    }
    if let Some(properties) = object.get_mut("properties").and_then(Value::as_object_mut) {
        for (name, child) in properties {
            bound_schema(child, Some(name));
        }
    }
    if let Some(items) = object.get_mut("items") {
        bound_schema(items, field);
    }
}

fn validate_tool_arguments(schema: &Value, value: &Value, path: &str) -> Result<(), GitError> {
    let invalid = |reason: String| {
        GitError::new(
            "git_invalid_arguments",
            format!("git_invalid_arguments:{path}:{reason}"),
            json!({"path":path,"reason":reason}),
        )
    };
    if schema.get("type") == Some(&json!("object")) && !value.is_object() {
        return Err(invalid("expected_object".into()));
    }
    if schema.get("type") == Some(&json!("array")) && !value.is_array() {
        return Err(invalid("expected_array".into()));
    }
    if schema.get("type") == Some(&json!("string")) && !value.is_string() {
        return Err(invalid("expected_string".into()));
    }
    if schema.get("type") == Some(&json!("integer"))
        && value.as_i64().is_none()
        && value.as_u64().is_none()
    {
        return Err(invalid("expected_integer".into()));
    }
    if let Some(text) = value.as_str() {
        if schema
            .get("maxLength")
            .and_then(Value::as_u64)
            .is_some_and(|max| text.len() > max as usize)
        {
            return Err(invalid("maxLength".into()));
        }
        if schema
            .get("enum")
            .and_then(Value::as_array)
            .is_some_and(|values| !values.iter().any(|candidate| candidate == value))
        {
            return Err(invalid("enum".into()));
        }
    }
    if let Some(array) = value.as_array() {
        if schema
            .get("minItems")
            .and_then(Value::as_u64)
            .is_some_and(|min| array.len() < min as usize)
        {
            return Err(invalid("minItems".into()));
        }
        if schema
            .get("maxItems")
            .and_then(Value::as_u64)
            .is_some_and(|max| array.len() > max as usize)
        {
            return Err(invalid("maxItems".into()));
        }
        if let Some(items) = schema.get("items") {
            for (index, item) in array.iter().enumerate() {
                validate_tool_arguments(items, item, &format!("{path}[{index}]"))?;
            }
        }
    }
    if let Some(number) = value.as_i64() {
        if schema
            .get("minimum")
            .and_then(Value::as_i64)
            .is_some_and(|min| number < min)
        {
            return Err(invalid("minimum".into()));
        }
        if schema
            .get("maximum")
            .and_then(Value::as_i64)
            .is_some_and(|max| number > max)
        {
            return Err(invalid("maximum".into()));
        }
    }
    if let Some(object) = value.as_object() {
        let properties = schema.get("properties").and_then(Value::as_object);
        if schema.get("additionalProperties") == Some(&json!(false)) {
            for key in object.keys() {
                if !properties.is_some_and(|known| known.contains_key(key)) {
                    return Err(invalid(format!("unknown_field:{key}")));
                }
            }
        }
        for required in schema
            .get("required")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
        {
            if !object.contains_key(required) {
                return Err(invalid(format!("required:{required}")));
            }
        }
        if let Some(properties) = properties {
            for (key, child) in object {
                if let Some(child_schema) = properties.get(key) {
                    validate_tool_arguments(child_schema, child, &format!("{path}.{key}"))?;
                }
            }
        }
    }
    if let Some(alternatives) = schema.get("anyOf").and_then(Value::as_array) {
        let matched = alternatives.iter().any(|alternative| {
            alternative
                .get("required")
                .and_then(Value::as_array)
                .is_some_and(|required| {
                    required
                        .iter()
                        .filter_map(Value::as_str)
                        .all(|field| value.get(field).is_some())
                })
        });
        if !matched {
            return Err(invalid("anyOf".into()));
        }
    }
    Ok(())
}
