fn load_scope(args: &Map<String, Value>, site_root: &Path) -> Result<ScopeConfig, Value> {
    let config_argument = args
        .get("config_path")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("config/config.json");
    if config_argument.chars().count() > 1024 {
        return Err(error(
            "mailbox_string_argument_too_long",
            "mailbox_string_argument_too_long",
        ));
    }
    let config_candidate = PathBuf::from(config_argument);
    let config_path = if config_candidate.is_absolute() {
        config_candidate
    } else {
        site_root.join(config_candidate)
    };
    let site_canonical = fs::canonicalize(site_root)
        .map_err(|e| error("mailbox_site_root_invalid", &e.to_string()))?;
    let config_canonical = fs::canonicalize(&config_path)
        .map_err(|e| error("mailbox_config_read_failed", &e.to_string()))?;
    if !config_canonical.starts_with(&site_canonical) {
        return Err(error(
            "mailbox_config_path_outside_site",
            &format!(
                "mailbox_config_path_outside_site:{}",
                config_path.to_string_lossy()
            ),
        ));
    }
    if fs::metadata(&config_canonical)
        .map_err(|e| error("mailbox_config_read_failed", &e.to_string()))?
        .len()
        > MAX_CONFIG_BYTES
    {
        return Err(error(
            "mailbox_config_too_large",
            "mailbox_config_too_large",
        ));
    }
    let document: Value = serde_json::from_str(
        &fs::read_to_string(&config_canonical)
            .map_err(|e| error("mailbox_config_read_failed", &e.to_string()))?,
    )
    .map_err(|e| error("mailbox_config_invalid", &e.to_string()))?;
    let scopes = document
        .get("scopes")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            error(
                "mailbox_config_scopes_invalid",
                "mailbox_config_scopes_invalid",
            )
        })?;
    let requested = args
        .get("scope_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let raw = if let Some(requested) = requested {
        scopes.iter().find(|scope| {
            scope
                .get("scope_id")
                .or_else(|| scope.get("id"))
                .or_else(|| scope.get("mailbox_id"))
                .and_then(Value::as_str)
                == Some(requested)
        })
    } else if scopes.len() == 1 {
        scopes.first()
    } else {
        None
    }
    .ok_or_else(|| {
        if let Some(requested) = requested {
            let code = format!("mailbox_scope_not_found:{requested}");
            error(&code, &code)
        } else {
            error("mailbox_scope_id_required", "mailbox_scope_id_required")
        }
    })?;
    normalize_scope(raw, site_root, &site_canonical, args.get("timeout_ms").and_then(Value::as_u64).unwrap_or(20_000).clamp(100,60_000))
}

fn normalize_scope(
    raw: &Value,
    site_root: &Path,
    site_canonical: &Path,
    request_timeout_ms: u64,
) -> Result<ScopeConfig, Value> {
    let object = raw
        .as_object()
        .ok_or_else(|| error("mailbox_scope_invalid", "mailbox_scope_invalid"))?;
    let scope_id = object
        .get("scope_id")
        .or_else(|| object.get("id"))
        .or_else(|| object.get("mailbox_id"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| error("mailbox_scope_id_required", "mailbox_scope_id_required"))?
        .to_string();
    if scope_id.chars().count() > 256 {
        return Err(error(
            "mailbox_string_argument_too_long",
            "mailbox_string_argument_too_long",
        ));
    }
    let root_text = object
        .get("root_dir")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| error("mailbox_scope_root_required", "mailbox_scope_root_required"))?;
    let candidate = PathBuf::from(root_text);
    let root_dir = if candidate.is_absolute() {
        candidate
    } else {
        site_root.join(candidate)
    };
    fs::create_dir_all(&root_dir)
        .map_err(|e| error("mailbox_scope_root_invalid", &e.to_string()))?;
    let root_canonical = fs::canonicalize(&root_dir)
        .map_err(|e| error("mailbox_scope_root_invalid", &e.to_string()))?;
    if !root_canonical.starts_with(site_canonical) {
        return Err(error(
            "mailbox_scope_root_outside_site",
            &format!(
                "mailbox_scope_root_outside_site:{}",
                root_dir.to_string_lossy()
            ),
        ));
    }

    let legacy_graph = object.get("graph").and_then(Value::as_object);
    let source_graph = object
        .get("sources")
        .and_then(Value::as_array)
        .and_then(|sources| {
            sources
                .iter()
                .find(|source| source.get("type").and_then(Value::as_str) == Some("graph"))
        })
        .and_then(Value::as_object);
    let graph = legacy_graph.or(source_graph).ok_or_else(|| {
        let code = format!("mailbox_scope_graph_source_required:{scope_id}");
        error(&code, &code)
    })?;
    let graph_string = |key: &str| {
        graph
            .get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string)
    };
    let user_id = graph_string("user_id").ok_or_else(|| {
        let code = format!("mailbox_scope_graph_source_required:{scope_id}");
        error(&code, &code)
    })?;
    let configured_base_url = graph_string("base_url");
    let base_url = configured_base_url
        .clone()
        .unwrap_or_else(|| "https://graph.microsoft.com/v1.0".to_string())
        .trim_end_matches('/')
        .to_string();
    validate_graph_base_url(&base_url)?;
    let filter = object.get("scope").and_then(Value::as_object);
    let included_container_refs = string_array(
        filter.and_then(|value| value.get("included_container_refs")),
        &["inbox", "sentitems", "drafts", "archive"],
        "mailbox_scope_container_refs_invalid",
    )?;
    if included_container_refs.is_empty() {
        return Err(error(
            "mailbox_scope_container_refs_invalid",
            "mailbox_scope_container_refs_invalid",
        ));
    }
    let included_item_kinds = string_array(
        filter.and_then(|value| value.get("included_item_kinds")),
        &["message"],
        "mailbox_scope_item_kinds_invalid",
    )?;
    let normalize = object.get("normalize").and_then(Value::as_object);
    let attachment_policy =
        optional_string(normalize.and_then(|value| value.get("attachment_policy")))
            .unwrap_or_else(|| "metadata_only".to_string());
    if !matches!(
        attachment_policy.as_str(),
        "exclude" | "metadata_only" | "include_content"
    ) {
        return Err(error(
            "mailbox_attachment_policy_invalid",
            "mailbox_attachment_policy_invalid",
        ));
    }
    let body_policy = optional_string(normalize.and_then(|value| value.get("body_policy")))
        .unwrap_or_else(|| "text_only".to_string());
    if !matches!(
        body_policy.as_str(),
        "original"
            | "best_effort"
            | "plain_text_only"
            | "text_only"
            | "html_only"
            | "text_and_html"
    ) {
        return Err(error(
            "mailbox_body_policy_invalid",
            "mailbox_body_policy_invalid",
        ));
    }
    let runtime = object.get("runtime").and_then(Value::as_object);
    let acquire_lock_timeout_ms = runtime
        .and_then(|value| value.get("acquire_lock_timeout_ms"))
        .and_then(Value::as_u64)
        .unwrap_or(30_000)
        .min(300_000);
    Ok(ScopeConfig {
        scope_id,
        root_dir_text: normalized_path_text(&root_dir),
        root_dir,
        graph: GraphConfig {
            auth_mode: graph_string("auth_mode"),
            mailbox_id: graph_string("mailbox_id"),
            tenant_id: graph_string("tenant_id"),
            client_id: graph_string("client_id"),
            client_secret: graph_string("client_secret"),
            user_id,
            base_url,
            configured_base_url,
            prefer_immutable_ids: graph
                .get("prefer_immutable_ids")
                .and_then(Value::as_bool)
                .unwrap_or(true),
            request_timeout_ms,
        },
        included_container_refs,
        included_item_kinds,
        attachment_policy,
        body_policy,
        include_headers: normalize
            .and_then(|value| value.get("include_headers"))
            .and_then(Value::as_bool)
            .unwrap_or(false),
        tombstones_enabled: normalize
            .and_then(|value| value.get("tombstones_enabled"))
            .and_then(Value::as_bool)
            .unwrap_or(true),
        acquire_lock_timeout_ms,
        cleanup_tmp_on_startup: runtime
            .and_then(|value| value.get("cleanup_tmp_on_startup"))
            .and_then(Value::as_bool)
            .unwrap_or(true),
    })
}

