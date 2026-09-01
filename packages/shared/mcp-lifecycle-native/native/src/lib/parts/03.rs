fn enforce_task_create_payload_contract(args: &Value) -> Result<(), String> {
    let inline_fields = [
        "title",
        "goal",
        "context",
        "required_work",
        "non_goals",
        "acceptance_criteria",
        "tags",
        "preferred_role",
        "target_role",
        "idempotency_key",
        "execution_binding",
    ];
    let fields = inline_fields
        .iter()
        .filter(|field| args.get(*field).is_some())
        .copied()
        .collect::<Vec<_>>();
    if !fields.is_empty() {
        return Err(format!(
            "task_lifecycle_create_inline_definition_refused: task definition fields must be supplied by immutable payload_ref, not inline tool arguments; fields={}",
            fields.join(",")
        ));
    }
    if args.get("payload_path").is_some() {
        return Err("task_lifecycle_create_payload_path_refused: task_lifecycle_create requires immutable payload_ref, not payload_path".to_string());
    }
    if string_arg(args, "payload_ref").is_none() {
        return Err("task_lifecycle_create_requires_payload_ref".to_string());
    }
    Ok(())
}
fn read_payload_revision_payload(root: &Path, reference: &str) -> Result<Value, String> {
    let (id, revision) = parse_payload_reference(reference)?;
    let path = payload_revision_path(root, &id, revision);
    let metadata = fs::metadata(&path).map_err(|_| format!("payload_ref_not_found: {reference}"))?;
    let max_bytes = 256 * 1024usize;
    if metadata.len() > max_bytes as u64 {
        return Err(format!(
            "payload_ref_too_large: {} > {max_bytes}",
            metadata.len()
        ));
    }
    let text = fs::read_to_string(&path).map_err(|e| format!("payload_ref_read_failed:{e}"))?;
    let record: Value =
        serde_json::from_str(&text).map_err(|e| format!("payload_ref_invalid_json:{e}"))?;
    if record.get("schema").and_then(Value::as_str) != Some("narada.mcp_payload.revision.v1")
        || record.get("ref").and_then(Value::as_str) != Some(reference)
        || record.get("payload_id").and_then(Value::as_str) != Some(id.as_str())
        || record.get("revision").and_then(Value::as_i64) != Some(revision)
    {
        return Err(format!("payload_ref_metadata_mismatch:{reference}"));
    }
    let payload = record
        .get("payload")
        .cloned()
        .ok_or_else(|| format!("payload_ref_payload_must_be_object:{reference}"))?;
    if !payload.is_object() {
        return Err(format!("payload_ref_payload_must_be_object:{reference}"));
    }
    if record.get("byte_size").and_then(Value::as_u64) != Some(payload_byte_size(&payload) as u64) {
        return Err(format!("payload_ref_byte_size_mismatch:{reference}"));
    }
    if record.get("sha256").and_then(Value::as_str) != Some(digest(&payload).as_str()) {
        return Err(format!("payload_ref_sha256_mismatch:{reference}"));
    }
    Ok(payload)
}

fn project_recurring_definition(row: Value) -> Result<Value, String> {
    let text = row
        .get("definition_json")
        .and_then(Value::as_str)
        .ok_or("recurring_definition_json_missing")?;
    let parsed: Value = serde_json::from_str(text)
        .map_err(|error| format!("recurring_definition_json_invalid:{error}"))?;
    let mut projected = parsed
        .as_object()
        .cloned()
        .ok_or("recurring_definition_json_must_be_object")?;
    if let Some(columns) = row.as_object() {
        for (key, value) in columns {
            if key != "definition_json" {
                projected.insert(key.clone(), value.clone());
            }
        }
    }
    Ok(Value::Object(projected))
}
fn compact_recurring_definition(value: &Value) -> Value {
    json!({"recurrence_id":value.get("recurrence_id"),"status":value.get("status"),"title":value.get("title"),"trigger_mode":value.get("trigger_mode"),"schedule_kind":value.get("schedule_kind"),"schedule_timezone":value.get("schedule_timezone"),"last_due_key":value.get("last_due_key"),"last_auto_triggered_at":value.get("last_auto_triggered_at"),"updated_at":value.get("updated_at")})
}
fn project_recurring_run(row: Value) -> Result<Value, String> {
    let text = row
        .get("run_json")
        .and_then(Value::as_str)
        .ok_or("recurring_run_json_missing")?;
    let run = serde_json::from_str::<Value>(text)
        .map_err(|error| format!("recurring_run_json_invalid:{error}"))?;
    if !run.is_object() {
        return Err("recurring_run_json_must_be_object".to_string());
    }
    Ok(run)
}
fn is_modern_request(params: &Value) -> bool {
    params
        .get("_meta")
        .and_then(Value::as_object)
        .and_then(|meta| meta.get("io.modelcontextprotocol/protocolVersion"))
        .and_then(Value::as_str)
        == Some(MODERN_PROTOCOL_VERSION)
}

fn validate_modern_request(params: &Value) -> Result<(), String> {
    let meta = params
        .get("_meta")
        .and_then(Value::as_object)
        .ok_or_else(|| "modern_metadata_required:Modern MCP requests require _meta.".to_string())?;
    if meta
        .get("io.modelcontextprotocol/clientInfo")
        .and_then(Value::as_object)
        .is_none()
    {
        return Err("modern_metadata_required:Modern MCP requests require clientInfo metadata.".to_string());
    }
    if meta
        .get("io.modelcontextprotocol/clientCapabilities")
        .and_then(Value::as_object)
        .is_none()
    {
        return Err("modern_metadata_required:Modern MCP requests require clientCapabilities metadata.".to_string());
    }
    Ok(())
}

fn server_discover(surface: Surface) -> Value {
    let legacy = match surface {
        Surface::Task => TASK_PROTOCOL_VERSION,
        Surface::Work => WORK_PROTOCOL_VERSION,
    };
    let capabilities = if surface == Surface::Task {
        json!({"tools":{},"resources":{},"prompts":{},"completions":{},"logging":{}})
    } else {
        json!({"tools":{}})
    };
    json!({
        "supportedVersions": [MODERN_PROTOCOL_VERSION, legacy],
        "capabilities": capabilities,
        "ttlMs": 3_600_000,
        "cacheScope": "public"
    })
}

fn modernize_result(value: Value, method: &str, surface: Surface) -> Value {
    let mut result = value.as_object().cloned().unwrap_or_default();
    result.insert("resultType".to_string(), json!("complete"));
    if matches!(method, "tools/list" | "resources/list" | "resources/read") {
        result.entry("ttlMs".to_string()).or_insert(json!(300_000));
        result.entry("cacheScope".to_string()).or_insert(json!("public"));
    }
    let mut meta = result
        .remove("_meta")
        .and_then(|entry| entry.as_object().cloned())
        .unwrap_or_default();
    meta.insert(
        "io.modelcontextprotocol/serverInfo".to_string(),
        json!({"name": surface.server_name(), "version": SERVER_VERSION}),
    );
    result.insert("_meta".to_string(), Value::Object(meta));
    Value::Object(result)
}
impl LifecycleServer {
    pub fn new(options: Options) -> Result<Self, String> {
        let booted_at = now();
        if options.prepare {
            Self::prepare_database(&options)?;
            return Ok(Self {
                options,
                connection: None,
                booted_at,
            });
        }
        if options.migrate_legacy {
            return Ok(Self {
                options,
                connection: None,
                booted_at,
            });
        }
        let path = options.database_path();
        if !path.exists() {
            if options.surface == Surface::Task {
                return Ok(Self {
                    options,
                    connection: None,
                    booted_at,
                });
            }
            return Err(format!(
                "{}_store_not_prepared:database_missing",
                options.surface.prefix()
            ));
        }
        let connection = match Self::open_runtime(&options) {
            Ok(connection) => Some(connection),
            Err(error)
                if options.surface == Surface::Task
                    && error.starts_with("task_lifecycle_store_not_prepared:") => None,
            Err(error) => return Err(error),
        };
        Ok(Self {
            options,
            connection,
            booted_at,
        })
    }

    pub fn prepare_database(options: &Options) -> Result<Value, String> {
        let path = options.database_path();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| format!("database_directory_create_failed:{e}"))?;
        }
        let mut connection =
            Connection::open(&path).map_err(|e| format!("database_open_failed:{e}"))?;
        configure_connection(&mut connection, true)?;
        connection
            .execute_batch(TASK_SCHEMA)
            .map_err(|e| format!("task_schema_prepare_failed:{e}"))?;
        ensure_task_post_schema(&connection)?;
        ensure_native_auxiliary_schema(&connection)?;
        ensure_downstream_dependency_contracts(&connection)?;
        ensure_task_revision_column(&connection)?;
        if options.surface == Surface::Work {
            connection
                .execute_batch(WORK_SCHEMA)
                .map_err(|e| format!("work_schema_prepare_failed:{e}"))?;
            ensure_work_task_revision_triggers(&connection)?;
        }
        connection
            .pragma_update(None, "user_version", TASK_SCHEMA_VERSION)
            .map_err(|e| format!("schema_version_write_failed:{e}"))?;
        let inspection = inspect_database(options)?;
        Ok(json!({
            "status": "prepared",
            "site_root": options.site_root.to_string_lossy(),
            "preparation": inspection,
        }))
    }

    fn open_runtime(options: &Options) -> Result<Connection, String> {
        let path = options.database_path();
        let mut connection = Connection::open(&path).map_err(|_| {
            format!(
                "{}_store_not_prepared:invalid_database",
                options.surface.prefix()
            )
        })?;
        configure_connection(&mut connection, false)?;
        ensure_task_post_schema(&connection)?;
        ensure_native_auxiliary_schema(&connection)?;
        ensure_downstream_dependency_contracts(&connection)?;
        ensure_task_revision_column(&connection)?;
        let inspection = inspect_connection(options.surface, &connection, &path)?;
        if inspection.get("status").and_then(Value::as_str) != Some("prepared") {
            let reason = inspection
                .get("reason")
                .and_then(Value::as_str)
                .unwrap_or("schema");
            return Err(format!(
                "{}_store_not_prepared:{reason}",
                options.surface.prefix()
            ));
        }
        if options.surface == Surface::Work { ensure_work_task_revision_triggers(&connection)?; }
        Ok(connection)
    }

}
