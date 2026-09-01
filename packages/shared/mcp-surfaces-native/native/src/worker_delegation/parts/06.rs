fn admitted_plan_binding(
    admission: &Value,
) -> Result<(String, String, String, String, Option<Value>), Value> {
    let plan_ref = admission
        .get("plan_ref")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            error(
                "worker_canonical_invocation_plan_invalid",
                "worker_canonical_invocation_plan_invalid",
            )
        })?;
    let provider = admission
        .pointer("/selected/inference_provider/id")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            error(
                "worker_canonical_invocation_provider_missing",
                "worker_canonical_invocation_provider_missing",
            )
        })?;
    let provider_binding = admission
        .get("provider_binding")
        .filter(|value| !value.is_null())
        .cloned();
    let mode = match provider {
        "inference-provider:codex-subscription" => "codex-subscription",
        "inference-provider:deepseek-api" | "inference-provider:openrouter-api" => {
            validate_native_provider_binding(provider_binding.as_ref())?;
            provider
                .strip_prefix("inference-provider:")
                .unwrap_or(provider)
        }
        _ => {
            return Err(error(
                "worker_native_provider_unsupported",
                "worker_native_provider_unsupported",
            ))
        }
    };
    let model = admission
        .pointer("/selected/model/id")
        .and_then(Value::as_str)
        .and_then(|value| value.strip_prefix("model:"))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            error(
                "worker_canonical_invocation_model_missing",
                "worker_canonical_invocation_model_missing",
            )
        })?;
    let evidence_ref = admission
        .get("evidence_ref")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    Ok((
        plan_ref.to_string(),
        mode.to_string(),
        model.to_string(),
        evidence_ref,
        provider_binding,
    ))
}

fn validate_native_provider_binding(binding: Option<&Value>) -> Result<(), Value> {
    let binding = binding.ok_or_else(|| {
        error(
            "worker_native_provider_binding_missing",
            "worker_native_provider_binding_missing",
        )
    })?;
    let provider = binding
        .get("provider")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let expected_secret_ref = match provider {
        "deepseek-api" => "narada/provider/deepseek-api/api-key",
        "openrouter-api" => "narada/provider/openrouter-api/api-key",
        _ => {
            return Err(error(
                "worker_native_provider_binding_invalid",
                "worker_native_provider_binding_invalid",
            ))
        }
    };
    let endpoint = binding
        .get("endpoint")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let endpoint_ok = match provider {
        "deepseek-api" => endpoint.starts_with("https://api.deepseek.com/"),
        "openrouter-api" => endpoint.starts_with("https://openrouter.ai/"),
        _ => false,
    };
    if binding.get("schema").and_then(Value::as_str) != Some("narada.native.provider_binding.v1")
        || binding.get("protocol").and_then(Value::as_str) != Some("openai/chat-completions/1")
        || binding.get("credential_secret_ref").and_then(Value::as_str) != Some(expected_secret_ref)
        || !endpoint_ok
        || binding
            .get("model")
            .and_then(Value::as_str)
            .is_none_or(str::is_empty)
    {
        return Err(error(
            "worker_native_provider_binding_invalid",
            "worker_native_provider_binding_invalid",
        ));
    }
    Ok(())
}
fn resolve_intelligence_context_path(
    root: &Path,
    explicit_context: Option<PathBuf>,
    user_site_root: Option<PathBuf>,
    home: Option<PathBuf>,
) -> PathBuf {
    if let Some(path) = explicit_context.filter(|path| path.is_file()) {
        return path;
    }
    if let Some(path) = user_site_root
        .map(|path| path.join(".narada/intelligence-launch-context.json"))
        .filter(|path| path.is_file())
    {
        return path;
    }
    let local = root.join(".narada/intelligence-launch-context.json");
    if local.is_file() {
        return local;
    }
    home.map(|home| home.join("Narada/.narada/intelligence-launch-context.json"))
        .filter(|path| path.is_file())
        .unwrap_or(local)
}
fn intelligence_context_path(root: &Path) -> PathBuf {
    resolve_intelligence_context_path(
        root,
        std::env::var_os("NARADA_INTELLIGENCE_CONTEXT_PATH").map(PathBuf::from),
        std::env::var_os("NARADA_USER_SITE_ROOT").map(PathBuf::from),
        std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(PathBuf::from),
    )
}
fn invocation_plan_binding(
    root: &Path,
    requested_plan_ref: Option<&str>,
    cognition: Option<&str>,
) -> Result<
    (
        String,
        String,
        String,
        String,
        Option<String>,
        Option<Value>,
    ),
    Value,
> {
    let context_path = intelligence_context_path(root);
    let context = read_json(&context_path).map_err(|_| {
        error(
            "worker_intelligence_context_required",
            "worker_intelligence_context_required",
        )
    })?;
    let registry = context
        .get("registry_db_path")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            error(
                "worker_intelligence_registry_required",
                "worker_intelligence_registry_required",
            )
        })?;
    let registry = PathBuf::from(registry);
    let context_site_root = context_path.parent().and_then(Path::parent).unwrap_or(root);
    let registry = if registry.is_absolute() {
        registry
    } else {
        context_site_root.join(registry)
    };
    let principal = context
        .get("principal_id")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            error(
                "worker_intelligence_principal_required",
                "worker_intelligence_principal_required",
            )
        })?;
    let defaults_path = root.join(".narada/worker-cognition-defaults.json");
    let request = json!({
        "schema":"narada.invokable-intelligence.preflight-request.v1",
        "intent_id":"",
        "purpose":"local-agent-runtime",
        "principal":principal,
        "requested_plan_id":requested_plan_ref,
        "evaluated_at":now(),
        "clock_authority_ref":"execution-site-clock:worker-delegation",
        "mode":"immediate",
        "cognition":cognition,
        "cognition_defaults_path":if cognition.is_some(){json!(defaults_path)}else{Value::Null}
    });
    let executable = preflight_command(root)?;
    let mut child = Command::new(executable)
        .args(["--registry", &registry.to_string_lossy()])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|_| {
            error(
                "worker_intelligence_preflight_launch_failed",
                "worker_intelligence_preflight_launch_failed",
            )
        })?;
    if let Some(mut stdin) = child.stdin.take() {
        writeln!(stdin, "{request}").map_err(|_| {
            error(
                "worker_intelligence_preflight_write_failed",
                "worker_intelligence_preflight_write_failed",
            )
        })?;
    }
    let output = child.wait_with_output().map_err(|_| {
        error(
            "worker_intelligence_preflight_wait_failed",
            "worker_intelligence_preflight_wait_failed",
        )
    })?;
    if output.stdout.len() > MAX_FILE_BYTES {
        return Err(error(
            "worker_intelligence_preflight_response_too_large",
            "worker_intelligence_preflight_response_too_large",
        ));
    }
    let admission: Value = serde_json::from_slice(&output.stdout).map_err(|_| {
        error(
            "worker_intelligence_preflight_response_invalid",
            "worker_intelligence_preflight_response_invalid",
        )
    })?;
    if !output.status.success()
        || admission.get("status").and_then(Value::as_str) != Some("admitted")
    {
        return Err(
            json!({"schema":"narada.worker.error.v1","code":"worker_intelligence_preflight_refused","message":"worker_intelligence_preflight_refused","preflight":admission}),
        );
    }
    let (plan_ref, mode, model, evidence_ref, provider_binding) =
        admitted_plan_binding(&admission)?;
    let reasoning_effort = admission
        .pointer("/options/reasoning_effort")
        .and_then(Value::as_str)
        .map(str::to_string);
    Ok((
        plan_ref,
        mode,
        model,
        evidence_ref,
        reasoning_effort,
        provider_binding,
    ))
}
fn codex_command() -> Option<PathBuf> {
    if let Some(command) = std::env::var_os("NARADA_NATIVE_CODEX_COMMAND") {
        return Some(PathBuf::from(command));
    }
    std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path).find_map(|directory| {
            ["codex.exe", "codex.cmd", "codex"]
                .into_iter()
                .map(|name| directory.join(name))
                .find(|candidate| candidate.is_file())
        })
    })
}
