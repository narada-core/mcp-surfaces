fn call_site_lifecycle(name: &str, args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    if name == "site_lifecycle_guidance" {
        return Ok(guidance_result("site-lifecycle", args));
    }
    if name == "site_lifecycle_doctor" {
        return Ok(json!({
            "schema":"narada.site_lifecycle.doctor.v1",
            "status":"ok",
            "server_name":"site-lifecycle-mcp",
            "implementation":"rust-native",
            "runtime_dependency":"none",
            "site_root":root.to_string_lossy(),
            "command_count":SITE_LIFECYCLE_COMMANDS.len(),
            "coverage":lifecycle_command_map(),
            "cli_module_exists":false,
            "cli_module_path":null,
            "native_authorities":["site_registry","operator_surface","site_relations","site_creation","site_discovery"],
            "legacy_dependency_sync":"retired"
        }));
    }
    if name == "site_lifecycle_command_map" {
        return Ok(
            json!({"status":"ok","implementation":"rust-native","commands":lifecycle_command_map(),"count":SITE_LIFECYCLE_COMMANDS.len()}),
        );
    }
    match name {
        "site_admit_role" => return operator_surface_authority::admit_role(args, root),
        "site_verify_role" => return operator_surface_authority::verify_role(args, root),
        "site_observe_runtime" => return operator_surface_authority::observe_runtime(args, root),
        "site_bind_runtime" => return operator_surface_authority::bind_runtime(args, root),
        "site_create_presets_list" => return Ok(site_lifecycle_authority::create_presets()),
        "site_create_plan" => return site_lifecycle_authority::create_plan(args, root),
        "site_discover" => {
            if args.get("execute").and_then(Value::as_bool) != Some(true) {
                return Err(diagnostic(
                    "site_discover_execute_required",
                    "site_discover requires execute=true",
                ));
            }
            if args
                .get("authority_basis")
                .and_then(Value::as_object)
                .is_none_or(Map::is_empty)
            {
                return Err(diagnostic(
                    "site_discover_authority_required",
                    "site_discover requires a non-empty authority_basis",
                ));
            }
            return site_registry_authority::apply_discovery(args);
        }
        "site_list" => {
            let listed = site_registry_authority::call("site_registry_list", &Map::new())?;
            let sites = listed["sites"].as_array().cloned().unwrap_or_default().into_iter().map(|site| json!({"siteId":site["site_id"],"variant":site["variant"],"substrate":site["substrate"],"health":"unknown","lastCycle":null,"failures":0})).collect::<Vec<_>>();
            return Ok(
                json!({"status":"success","sites":sites,"paging":{"count":listed["count"],"returned":listed["returned"],"has_more":listed["has_more"],"next_offset":listed["next_offset"]}}),
            );
        }
        "site_show" => {
            let site_id = require_string(args, "site_id")?;
            let shown = site_registry_authority::call(
                "site_registry_show",
                &serde_json::from_value(json!({"reference":site_id})).unwrap(),
            )?;
            if shown["status"] != "success" {
                return Ok(
                    json!({"status":"error","error":format!("Site not found: {site_id}"),"refusals":shown["refusals"]}),
                );
            }
            let site = &shown["site"];
            return Ok(
                json!({"status":"success","site":{"siteId":site["site_id"],"variant":site["variant"],"siteRoot":site["site_root"],"substrate":site["substrate"],"aimJson":site["aim_json"],"controlEndpoint":site["control_endpoint"],"lastSeenAt":site["last_seen_at"],"createdAt":site["created_at"],"health":null}}),
            );
        }
        "site_lifecycle_kinds" => return Ok(site_lifecycle_authority::kinds()),
        "site_lifecycle_preflight" => return site_lifecycle_authority::preflight(args),
        "site_relation_list" => return site_lifecycle_authority::relation_list(args, root),
        "site_relation_validate" => return site_lifecycle_authority::relation_validate(args, root),
        "site_authority_preflight" => {
            return site_lifecycle_authority::authority_preflight(args, root)
        }
        "site_dependency_posture" => return site_lifecycle_authority::dependency_posture(root),
        "site_deps_sync" => return Ok(site_lifecycle_authority::retired_dependency_sync(root)),
        "site_init" => return site_lifecycle_authority::init_site(args),
        _ => {}
    }
    let spec = SITE_LIFECYCLE_COMMANDS
        .iter()
        .find(|(tool, _, _, _, _)| *tool == name)
        .ok_or_else(|| diagnostic("unknown_tool", &format!("unknown_tool:{name}")))?;
    if name == "site_doctor" {
        require_string(args, "site_id")?;
    }
    if name == "site_init" {
        require_string(args, "site_id")?;
        require_string(args, "site_root")?;
        require_string(args, "substrate")?;
        if !args.contains_key("authority_basis") {
            return Err(diagnostic(
                "required_argument_missing",
                "required_argument_missing:authority_basis",
            ));
        }
    }
    if name == "site_lifecycle_preflight" {
        require_string(args, "kind")?;
    }
    let mutation = !spec.2;
    let (result, resolution_status) = if name == "site_doctor" {
        let site_id = require_string(args, "site_id")?;
        let mut resolved_args = args.clone();
        let resolution_source = if args.get("root").and_then(Value::as_str).is_some() {
            "explicit_root"
        } else {
            let shown = site_registry_authority::call(
                "site_registry_show",
                &serde_json::from_value(json!({"reference":site_id})).unwrap(),
            )?;
            let Some(site_root) = shown.pointer("/site/site_root").and_then(Value::as_str) else {
                return Ok(json!({
                    "schema":"narada.site_lifecycle.result.v1","status":"not_found",
                    "implementation":"rust-native","tool":name,"read_only":true,
                    "mutation_performed":false,"site_id":site_id,
                    "message":"Site is not registered and no explicit root was supplied."
                }));
            };
            resolved_args.insert("root".to_string(), Value::String(site_root.to_string()));
            "canonical_registry"
        };
        let mut evidence = site_resolution_evidence(&resolved_args, root);
        evidence["resolution_source"] = Value::String(resolution_source.to_string());
        let status = evidence
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("attention")
            .to_string();
        (
            json!({
                "items":evidence.get("checks").cloned().unwrap_or_else(|| json!([])),
                "resolution_evidence":evidence
            }),
            status,
        )
    } else {
        let result = if name == "site_create_presets_list" {
            json!({"presets":[]})
        } else if name == "site_lifecycle_kinds" {
            json!({"kinds":[]})
        } else if name == "site_relation_list" {
            json!({"relations":[]})
        } else {
            json!({"items":[]})
        };
        (
            result,
            if mutation {
                "planned".to_string()
            } else {
                "ok".to_string()
            },
        )
    };
    Ok(json!({
        "schema":"narada.site_lifecycle.result.v1",
        "status":if mutation {resolution_status.clone()} else {resolution_status.clone()},
        "implementation":"rust-native","tool":name,"cli_command":spec.1,
        "read_only":spec.2,"requires_execute":spec.3,"requires_authority":spec.4,
        "mutation_performed":false,"dry_run":args.get("dry_run").and_then(Value::as_bool).unwrap_or(mutation),
        "options":args,"result":result
    }))
}
fn call_site_registry(name: &str, args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    if name == "site_registry_guidance" {
        return Ok(guidance_result("site-registry", args));
    }
    if name == "site_registry_doctor" {
        let mut result = site_registry_authority::doctor();
        result["narada_root"] = Value::String(root.to_string_lossy().to_string());
        result["command_count"] = Value::from(SITE_REGISTRY_COMMANDS.len());
        result["coverage"] = Value::Array(registry_command_map());
        return Ok(result);
    }
    if name == "site_registry_command_map" {
        return Ok(
            json!({"status":"ok","implementation":"rust-native","commands":registry_command_map(),"count":SITE_REGISTRY_COMMANDS.len()}),
        );
    }
    SITE_REGISTRY_COMMANDS
        .iter()
        .find(|(tool, _)| *tool == name)
        .ok_or_else(|| diagnostic("unknown_tool", &format!("unknown_tool:{name}")))?;
    site_registry_authority::call(name, args)
}

fn registry_input_schema(name: &str) -> Value {
    let properties = match name {
        "site_registry_list" => {
            json!({"limit":{"type":"integer","minimum":1,"maximum":500,"default":100},"offset":{"type":"integer","minimum":0,"maximum":10000,"default":0}})
        }
        "site_registry_show" => {
            json!({"reference":{"type":"string","minLength":1,"maxLength":512}})
        }
        "site_registry_discover_plan" => {
            json!({"source":{"type":"string","enum":["filesystem","launch_registry","all"],"default":"all"},"root":{"type":"string","minLength":1,"maxLength":4096},"actor":{"type":"string","minLength":1,"maxLength":512}})
        }
        _ => json!({}),
    };
    let required = if name == "site_registry_show" {
        json!(["reference"])
    } else {
        json!([])
    };
    json!({"type":"object","properties":properties,"required":required,"additionalProperties":false})
}

fn project_input_schema(name: &str) -> Value {
    let id = || json!({"type":"string","minLength":1,"maxLength":512});
    let mut properties = Map::new();
    let mut required = Vec::new();
    match name {
        "project_state_program_show" => { properties.insert("program_id".into(),id()); required.push("program_id"); }
        "project_state_project_show" => { properties.insert("project_id".into(),id()); required.push("project_id"); }
        "project_state_standard_show" => { properties.insert("standard_id".into(),id()); required.push("standard_id"); }
        "project_state_project_list" => { properties.insert("program_id".into(),id()); }
        "project_state_matrix" => { for key in ["project_id","object_id","lifecycle"] { properties.insert(key.into(),id()); } }
        "project_state_gaps" | "project_state_handoff" => { for key in ["program_id","project_id"] { properties.insert(key.into(),id()); } }
        "project_state_standards_list" => { properties.insert("selection".into(),json!({"type":"string","enum":["core","conditional","reference"]})); }
        "project_state_applicability" => {
            for key in ["program_id","project_id","standard_id"] { properties.insert(key.into(),id()); }
            properties.insert("status".into(),json!({"type":"string","enum":["selected","conditional","reference","not_applicable"]}));
        }
        "project_state_standard_trace" => {
            for key in ["program_id","project_id","standard_id","obligation_id","object_id","lifecycle"] { properties.insert(key.into(),id()); }
            properties.insert("status".into(),json!({"type":"string","enum":["virtually_supported","open_gap","not_applicable"]}));
        }
        "project_state_standard_gaps" => { for key in ["program_id","project_id","standard_id"] { properties.insert(key.into(),id()); } }
        _ => {}
    }
    json!({"type":"object","properties":properties,"required":required,"additionalProperties":false})
}

fn call_project_state(name: &str, args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    if name == "project_state_guidance" {
        return Ok(guidance_result("project-state", args));
    }
    if name == "project_state_doctor" {
        let mut result = project_state_authority::doctor(root);
        result["server_name"] = json!("project-state-mcp");
        result["command_count"] = json!(PROJECT_STATE_COMMANDS.len());
        return Ok(result);
    }
    if name == "project_state_command_map" {
        return Ok(json!({
            "schema":"narada.project_state.command_map.v1","status":"ok","read_only":true,
            "virtual_only":true,"implementation":"rust-native","commands":project_command_map(),
            "count":PROJECT_STATE_COMMANDS.len()
        }));
    }
    let (_, cli) = PROJECT_STATE_COMMANDS
        .iter()
        .find(|(tool, _)| *tool == name)
        .ok_or_else(|| diagnostic("unknown_tool", &format!("unknown_tool:{name}")))?;
    let mut result = project_state_authority::call(name, args, root)?;
    result["tool"] = json!(name);
    result["cli_command"] = json!(cli);
    result["project_root"] = json!(root.to_string_lossy());
    Ok(result)
}

fn lifecycle_command_map() -> Vec<Value> {
    SITE_LIFECYCLE_COMMANDS
        .iter()
        .map(|(tool, cli, read_only, execute, authority)| {
            json!({
                "tool":tool,"cli_command":cli,"read_only":read_only,
                "requires_execute":execute,"requires_authority":authority
            })
        })
        .collect()
}
