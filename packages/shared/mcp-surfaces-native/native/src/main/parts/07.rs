#[allow(clippy::too_many_arguments)]
fn operator_typed_handoff(
    operation_kind: Option<&str>,
    target_runtime: &str,
    target_identity: Option<&str>,
    target_site_id: Option<&str>,
    target_site_root: Option<&str>,
    role: Option<&str>,
    agent_kind: Option<&str>,
    principal: Option<&str>,
    runtime_locus: Option<&str>,
    runtime_handle: Option<&str>,
) -> Option<Value> {
    let site_authority = target_site_root.map(|root| {
        let path = Path::new(root);
        if path
            .file_name()
            .is_some_and(|name| name.eq_ignore_ascii_case(".narada"))
        {
            path.to_path_buf()
        } else {
            path.join(".narada")
        }
    });
    match operation_kind {
        Some("role_admission") => {
            let missing = [
                ("target_site_id", target_site_id),
                ("target_site_root", target_site_root),
                ("role", role),
                ("principal", principal),
            ]
            .into_iter()
            .filter_map(|(name, value)| value.is_none().then_some(name))
            .collect::<Vec<_>>();
            Some(
                json!({"schema":"narada.mcp_handoff.v1","status":if missing.is_empty(){"ready"}else{"needs_input"},"executable":missing.is_empty(),"target_surface":"site-lifecycle","tool":"site_admit_role","authority_locus":site_authority.map(|path|path.to_string_lossy().to_string()),"arguments":{"site_id":target_site_id,"site_root":target_site_root,"role":role,"agent_kind":agent_kind.unwrap_or(target_runtime),"identity":target_site_id.zip(role).map(|(site,role)|format!("{site}.{role}")).or_else(||target_identity.map(ToOwned::to_owned)),"by":principal,"execute":true,"authority_basis":"<operator-authority-basis>"},"required_inputs":missing,"mutation_authorized":false,"reason":"Durable project-role admission is owned by the project Site lifecycle surface."}),
            )
        }
        Some("runtime_binding") => {
            let missing = [
                ("target_site_root", target_site_root),
                ("target_identity", target_identity),
                ("runtime_locus", runtime_locus),
                ("runtime_handle", runtime_handle),
            ]
            .into_iter()
            .filter_map(|(name, value)| value.is_none().then_some(name))
            .collect::<Vec<_>>();
            Some(
                json!({"schema":"narada.mcp_handoff.v1","status":if missing.is_empty(){"ready"}else{"needs_input"},"executable":missing.is_empty(),"target_surface":"site-lifecycle","tool":"site_bind_runtime","authority_locus":site_authority.map(|path|path.to_string_lossy().to_string()),"arguments":{"site_root":target_site_root,"identity":target_identity,"runtime_locus":runtime_locus,"handle":runtime_handle,"execute":true,"authority_basis":"<operator-authority-basis>"},"required_inputs":missing,"mutation_authorized":false,"reason":"Volatile runtime binding is owned by the owning runtime locus and requires observed target evidence."}),
            )
        }
        Some(_) => None,
        None => Some(
            json!({"schema":"narada.mcp_handoff.v1","status":"deferred","executable":false,"target_surface":"site-inbox","tool":"submit_to_site_inbox","authority_locus":"target Site","arguments":{"target_runtime":target_runtime,"target_identity":target_identity},"required_inputs":[],"mutation_authorized":false,"reason":"No typed project action was declared; retain the durable fallback envelope."}),
        ),
    }
}

fn append_route_record(record: &Value, options: &Options) -> Result<PathBuf, Value> {
    let path = route_log_path(options);
    let root = path.parent().map(Path::to_path_buf).ok_or_else(|| {
        diagnostic(
            "operator_route_log_path_invalid",
            "routing log has no parent directory",
            Value::Null,
        )
    })?;
    create_dir_all(&root).map_err(|error| {
        diagnostic(
            "operator_route_log_create_failed",
            &error.to_string(),
            Value::Null,
        )
    })?;
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|error| {
            diagnostic(
                "operator_route_log_open_failed",
                &error.to_string(),
                Value::Null,
            )
        })?;
    let line = serde_json::to_string(record).map_err(|error| {
        diagnostic(
            "operator_route_log_encode_failed",
            &error.to_string(),
            Value::Null,
        )
    })?;
    writeln!(file, "{line}").map_err(|error| {
        diagnostic(
            "operator_route_log_write_failed",
            &error.to_string(),
            Value::Null,
        )
    })?;
    Ok(path)
}

fn required_string(args: &Map<String, Value>, key: &str) -> Result<String, Value> {
    optional_string(args, key).ok_or_else(|| {
        diagnostic(
            "required_argument_missing",
            &format!("required_argument_missing:{key}"),
            json!({ "key": key }),
        )
    })
}

fn optional_string(args: &Map<String, Value>, key: &str) -> Option<String> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

fn diagnostic(code: &str, message: &str, details: Value) -> Value {
    let mut object = Map::new();
    object.insert("code".to_string(), Value::String(code.to_string()));
    object.insert("message".to_string(), Value::String(message.to_string()));
    if !details.is_null() {
        object.insert("details".to_string(), details);
    }
    Value::Object(object)
}

fn now_iso() -> String {
    OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
}

fn compact_timestamp() -> String {
    now_iso()
        .replace(['-', ':', '.'], "")
        .chars()
        .take(15)
        .collect()
}

