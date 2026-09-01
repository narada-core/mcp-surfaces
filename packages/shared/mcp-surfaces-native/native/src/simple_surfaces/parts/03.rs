fn registry_command_map() -> Vec<Value> {
    SITE_REGISTRY_COMMANDS.iter().map(|(tool, cli)| json!({
        "tool":tool,"cli_command":cli,"read_only":true,"requires_execute":false,"requires_authority":false
    })).collect()
}
fn project_command_map() -> Vec<Value> {
    PROJECT_STATE_COMMANDS.iter().map(|(tool, cli)| json!({
        "tool":tool,"cli_command":cli,"read_only":true,"requires_execute":false,"requires_authority":false
    })).collect()
}
fn guidance_result(surface_id: &str, args: &Map<String, Value>) -> Value {
    json!({
        "schema":"narada.mcp_surface.guidance.v0","status":"ok","surface_id":surface_id,
        "guidance_tool":format!("{}_guidance",surface_id.replace('-', "_")),
        "purpose":format!("Native Rust {surface_id} MCP surface with explicit bounded authority."),
        "requested":{"workflow":args.get("workflow").cloned().unwrap_or(Value::Null),"tool":args.get("tool").cloned().unwrap_or(Value::Null)},
        "boundaries":["Guidance is read-only model-facing operating advice.","Mutation-shaped operations remain plans until an owning authority performs them.","Structured content is authoritative evidence."]
    })
}
fn guidance_tool(surface_id: &str) -> Value {
    tool(
        &format!("{}_guidance", surface_id.replace('-', "_")),
        "Show model-facing operating guidance for the native surface.",
        true,
    )
}
fn site_resolution_evidence(args: &Map<String, Value>, root: &Path) -> Value {
    let requested_site_id = args
        .get("site_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string);
    let requested_site_root = args
        .get("root")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string);
    let bound_workspace = workspace_root_for(root);
    let requested_path = requested_site_root.as_ref().map(PathBuf::from);
    let requested_workspace = requested_path.as_deref().map(workspace_root_for);
    let requested_control = requested_path.as_ref().map(|path| {
        if is_site_authority_path(path) {
            path.clone()
        } else {
            path.join(".narada")
        }
    });
    let site_root_exists = requested_path
        .as_ref()
        .map(|path| path.exists())
        .unwrap_or(false);
    let control_root_exists = requested_control
        .as_ref()
        .map(|path| path.exists())
        .unwrap_or(false);
    let bound_root_match = requested_workspace
        .as_ref()
        .map(|path| path_key(path) == path_key(&bound_workspace))
        .unwrap_or(false);
    let site_id_resolved = requested_site_id.is_some();
    let inspected = site_id_resolved
        && requested_path.is_some()
        && site_root_exists
        && control_root_exists
        && bound_root_match;
    let status = if inspected { "ok" } else { "attention" };
    json!({
        "status":status,
        "inspected":inspected,
        "requested_site_id":requested_site_id,
        "requested_site_root":requested_site_root,
        "bound_site_root":bound_workspace.to_string_lossy(),
        "bound_root_match":bound_root_match,
        "site_root_exists":site_root_exists,
        "control_root":requested_control.map(|path|path.to_string_lossy().to_string()),
        "control_root_exists":control_root_exists,
        "checks":[
            {"check":"site_id_resolution","status":if site_id_resolved {"pass"} else {"attention"}},
            {"check":"site_root_resolution","status":if requested_path.is_some() {"pass"} else {"attention"}},
            {"check":"site_root_exists","status":if site_root_exists {"pass"} else {"attention"}},
            {"check":"control_root_exists","status":if control_root_exists {"pass"} else {"attention"}},
            {"check":"bound_root_match","status":if bound_root_match {"pass"} else {"attention"}}
        ]
    })
}

fn is_site_authority_path(path: &Path) -> bool {
    path.file_name()
        .and_then(|value| value.to_str())
        .map(|value| value.eq_ignore_ascii_case(".narada"))
        .unwrap_or(false)
}

fn workspace_root_for(path: &Path) -> PathBuf {
    if is_site_authority_path(path) {
        path.parent().unwrap_or(path).to_path_buf()
    } else {
        path.to_path_buf()
    }
}

fn path_key(path: &Path) -> String {
    path.to_string_lossy()
        .replace('\\', "/")
        .trim_end_matches('/')
        .to_ascii_lowercase()
}

fn lifecycle_tool(name: &str, description: &str, read_only: bool) -> Value {
    tool_with_schema(name, description, read_only, lifecycle_input_schema(name))
}

fn lifecycle_input_schema(name: &str) -> Value {
    if matches!(
        name,
        "site_admit_role" | "site_verify_role" | "site_observe_runtime" | "site_bind_runtime"
    ) {
        return operator_surface_schema(name);
    }
    let string = || json!({"type":"string","minLength":1,"maxLength":512});
    let path = || json!({"type":"string","minLength":1,"maxLength":4096});
    let authority =
        json!({"type":"object","minProperties":1,"maxProperties":32,"additionalProperties":true});
    let (properties, required) = match name {
        "site_create_plan" => (
            json!({"config":path(),"preset":{"type":"string","enum":["minimal","agent-site-core","agent-memory","task-lifecycle","site-machinery"]},"site_id":string(),"root":path(),"site_kind":string(),"authority_locus":string()}),
            vec![],
        ),
        "site_discover" => (
            json!({"execute":{"type":"boolean"},"dry_run":{"type":"boolean"},"authority_basis":authority}),
            vec![],
        ),
        "site_show" => (json!({"site_id":string()}), vec!["site_id"]),
        "site_doctor" => (
            json!({"site_id":string(),"root":path(),"authority_locus":{"type":"string","enum":["user","pc","project","client_service"]},"kind":{"type":"string","enum":["windows","client","project","linux","linux-user","linux-system"]},"role":string(),"role_required":{"type":"boolean"}}),
            vec!["site_id"],
        ),
        "site_init" => (
            json!({"site_id":string(),"substrate":{"type":"string","enum":["windows-native","windows-wsl","macos","linux-user","linux-system"]},"operation":string(),"root":path(),"authority_locus":{"type":"string","enum":["user","pc"]},"sync":{"type":"string","enum":["local_only","cloud_synced_folder","git_backed","hybrid","hybrid_capable_plain_folder"]},"execution_surface":{"type":"string","enum":["windows_native","wsl_assisted","wsl_native","linux_user","linux_system","macos_native"]},"dry_run":{"type":"boolean"},"execute":{"type":"boolean"},"authority_basis":authority}),
            vec!["site_id", "substrate"],
        ),
        "site_lifecycle_preflight" => (
            json!({"kind":{"type":"string","enum":["clone","fork","split","absorb","migrate","re-instantiate","archive"]},"source_site":path(),"target_site":path(),"authority_mode":string()}),
            vec!["kind"],
        ),
        "site_relation_list" => (
            json!({"kind":{"type":"string","enum":["absorbed","absorbed_by","references","routes_to","subscribes_to","publishes_to"]},"source_site":string(),"target_site":string(),"status":{"type":"string","enum":["active","superseded","rejected"]},"limit":{"type":"integer","minimum":1,"maximum":500,"default":20},"cwd":path()}),
            vec![],
        ),
        "site_relation_validate" => (json!({"cwd":path()}), vec![]),
        "site_authority_preflight" => (
            json!({"cwd":path(),"mutation_family":{"type":"string","enum":["task_lifecycle","inbox","publication","secret","site"]}}),
            vec![],
        ),
        "site_deps_sync" => (
            json!({"root":path(),"apply":{"type":"boolean"},"execute":{"type":"boolean"},"authority_basis":authority}),
            vec![],
        ),
        _ => (json!({}), vec![]),
    };
    json!({"title":format!("{name}.input"),"type":"object","properties":properties,"required":required,"additionalProperties":false})
}

fn operator_surface_schema(name: &str) -> Value {
    let string = || json!({"type":"string","minLength":1,"maxLength":512});
    let path = || json!({"type":"string","minLength":3,"maxLength":4096});
    let authority =
        json!({"type":"object","minProperties":1,"maxProperties":32,"additionalProperties":true});
    let (properties, required) = match name {
        "site_admit_role" => (
            json!({"site_id":string(),"site_root":path(),"role":{"type":"string","enum":["architect","builder","observer"]},"agent_kind":string(),"identity":string(),"label":string(),"by":string(),"input_capabilities":{"type":"string","maxLength":1024},"submit_strategy":{"type":"string","enum":["type_only","operator_confirmed_submit","known_surface_submit"]},"execute":{"type":"boolean","const":true},"authority_basis":authority}),
            vec![
                "site_id",
                "site_root",
                "role",
                "agent_kind",
                "by",
                "execute",
                "authority_basis",
            ],
        ),
        "site_verify_role" => (
            json!({"site_id":string(),"site_root":path(),"runtime_locus":string(),"limit":{"type":"integer","minimum":1,"maximum":500,"default":100}}),
            vec!["site_id", "site_root"],
        ),
        "site_observe_runtime" => (
            json!({"site_id":string(),"site_root":path(),"limit":{"type":"integer","minimum":1,"maximum":500,"default":100}}),
            vec!["site_id", "site_root"],
        ),
        "site_bind_runtime" => (
            json!({"site_root":path(),"identity":string(),"runtime_locus":string(),"handle":path(),"observed_handle":path(),"stale_after":{"type":"string","format":"date-time","maxLength":64},"window_title":{"type":"string","maxLength":1024},"window_class":string(),"process_name":string(),"process_id":string(),"execute":{"type":"boolean","const":true},"authority_basis":authority}),
            vec![
                "site_root",
                "identity",
                "runtime_locus",
                "handle",
                "execute",
                "authority_basis",
            ],
        ),
        _ => (json!({}), Vec::new()),
    };
    json!({"type":"object","properties":properties,"required":required,"additionalProperties":false})
}

fn tool_with_schema(name: &str, description: &str, read_only: bool, input_schema: Value) -> Value {
    let mut input_schema = input_schema;
    if let Some(schema) = input_schema.as_object_mut() {
        schema
            .entry("title".to_string())
            .or_insert_with(|| Value::String(format!("{name}.input")));
    }
    json!({
        "name":name,"description":description,
        "inputSchema":input_schema,
        "annotations":{"title":name,"readOnlyHint":read_only,"destructiveHint":!read_only,"idempotentHint":true,"openWorldHint":false},
        "outputSchema":{"type":"object","additionalProperties":true}
    })
}

fn tool(name: &str, description: &str, read_only: bool) -> Value {
    json!({
        "name":name,"description":description,
        "inputSchema":{"title":format!("{name}.input"),"type":"object","properties":{},"additionalProperties":false},
        "annotations":{"title":name,"readOnlyHint":read_only,"destructiveHint":!read_only,"idempotentHint":true,"openWorldHint":false},
        "outputSchema":{"type":"object","additionalProperties":true}
    })
}
fn require_string(args: &Map<String, Value>, key: &str) -> Result<String, Value> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(ToString::to_string)
        .ok_or_else(|| {
            diagnostic(
                "required_argument_missing",
                &format!("required_argument_missing:{key}"),
            )
        })
}
fn diagnostic(code: &str, message: &str) -> Value {
    json!({"code":code,"message":message})
}

