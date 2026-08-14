use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};
use crate::operator_surface_authority;

const SITE_LIFECYCLE_COMMANDS: &[(&str, &str, bool, bool, bool)] = &[
    (
        "site_create_presets_list",
        "narada sites create-presets",
        true,
        false,
        false,
    ),
    (
        "site_create_plan",
        "narada sites create --dry-run",
        true,
        false,
        false,
    ),
    ("site_list", "narada sites list", true, false, false),
    ("site_discover", "narada sites discover", false, true, false),
    (
        "site_show",
        "narada sites show <site-id>",
        true,
        false,
        false,
    ),
    ("site_admit_role", "narada operator-surface agent instantiate", false, true, true),
    ("site_verify_role", "narada operator-surface doctor", true, false, false),
    ("site_observe_runtime", "narada operator-surface status", true, false, false),
    ("site_bind_runtime", "narada operator-surface bind-focused", false, true, true),
    (
        "site_doctor",
        "narada sites doctor <site-id>",
        true,
        false,
        false,
    ),
    (
        "site_init",
        "narada sites init <site-id>",
        false,
        true,
        true,
    ),
    (
        "site_lifecycle_kinds",
        "narada sites lifecycle kinds",
        true,
        false,
        false,
    ),
    (
        "site_lifecycle_preflight",
        "narada sites lifecycle preflight <kind>",
        true,
        false,
        false,
    ),
    (
        "site_relation_list",
        "narada sites relation list",
        true,
        false,
        false,
    ),
    (
        "site_relation_validate",
        "narada sites relation validate",
        true,
        false,
        false,
    ),
    (
        "site_authority_preflight",
        "narada sites authority preflight",
        true,
        false,
        false,
    ),
    (
        "site_deps_sync",
        "narada sites deps sync",
        false,
        true,
        true,
    ),
];

const SITE_REGISTRY_COMMANDS: &[(&str, &str)] = &[
    ("site_registry_list", "narada sites registry list"),
    (
        "site_registry_show",
        "narada sites registry show <reference>",
    ),
    (
        "site_registry_discover_plan",
        "narada sites registry discover --dry-run",
    ),
];

const PROJECT_STATE_COMMANDS: &[(&str, &str)] = &[
    ("project_state_program_list", "program list"),
    ("project_state_program_show", "program show <program_id>"),
    ("project_state_project_list", "project list [--program <program_id>]"),
    ("project_state_project_show", "project show <project_id>"),
    ("project_state_matrix", "matrix [--project] [--object] [--lifecycle]"),
    ("project_state_gaps", "gaps [--program] [--project]"),
    ("project_state_handoff", "handoff [--program] [--project]"),
    ("project_state_standards_list", "standards list [--selection <selection>]"),
    ("project_state_standard_show", "standards show <standard_id>"),
    ("project_state_applicability", "applicability [--program] [--project] [--standard] [--status]"),
    ("project_state_standard_trace", "trace [--program] [--project] [--standard] [--obligation] [--object] [--lifecycle] [--status]"),
    ("project_state_standard_gaps", "standards gaps [--program] [--project] [--standard]"),
    ("project_state_validate", "validate"),
];

pub fn list_tools(surface_id: &str) -> Vec<Value> {
    match surface_id {
        "site-lifecycle" => {
            let mut tools = vec![
                guidance_tool("site-lifecycle"),
                lifecycle_tool(
                    "site_lifecycle_doctor",
                    "Inspect one explicitly identified Site and report resolution evidence.",
                    true,
                ),
                tool(
                    "site_lifecycle_command_map",
                    "List MCP tools and their aligned Narada site lifecycle commands.",
                    true,
                ),
            ];
            tools.extend(
                SITE_LIFECYCLE_COMMANDS
                    .iter()
                    .map(|(name, _, read_only, _, _)| {
                        lifecycle_tool(
                            name,
                            "Execute or plan one Narada site lifecycle operation.",
                            *read_only,
                        )
                    }),
            );
            tools
        }
        "site-registry" => {
            let mut tools = vec![
                guidance_tool("site-registry"),
                tool(
                    "site_registry_doctor",
                    "Inspect native site registry MCP posture and command coverage.",
                    true,
                ),
                tool(
                    "site_registry_command_map",
                    "List MCP tools and their aligned Narada site registry commands.",
                    true,
                ),
            ];
            tools.extend(SITE_REGISTRY_COMMANDS.iter().map(|(name, _)| {
                tool(
                    name,
                    "Read or plan one canonical site registry operation.",
                    true,
                )
            }));
            tools
        }
        "project-state" => {
            let mut tools = vec![
                guidance_tool("project-state"),
                tool(
                    "project_state_doctor",
                    "Inspect the native virtual project-state surface.",
                    true,
                ),
                tool(
                    "project_state_command_map",
                    "List the read-only project-state command map.",
                    true,
                ),
            ];
            tools.extend(PROJECT_STATE_COMMANDS.iter().map(|(name, _)| {
                tool(
                    name,
                    "Read one bounded virtual project-state projection.",
                    true,
                )
            }));
            tools
        }
        _ => Vec::new(),
    }
}

pub fn call_tool(
    surface_id: &str,
    name: &str,
    args: &Map<String, Value>,
    root: &Path,
) -> Result<Value, Value> {
    match surface_id {
        "site-lifecycle" => call_site_lifecycle(name, args, root),
        "site-registry" => call_site_registry(name, args, root),
        "project-state" => call_project_state(name, args, root),
        _ => Err(diagnostic(
            "unknown_surface",
            &format!("unknown_surface:{surface_id}"),
        )),
    }
}

fn call_site_lifecycle(name: &str, args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    if name == "site_lifecycle_guidance" {
        return Ok(guidance_result("site-lifecycle", args));
    }
    if name == "site_lifecycle_doctor" {
        let evidence = site_resolution_evidence(args, root);
        let status = evidence
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("attention");
        return Ok(json!({
            "schema":"narada.site_lifecycle.doctor.v1",
            "status":status,
            "server_name":"site-lifecycle-mcp",
            "implementation":"rust-native",
            "site_root":root.to_string_lossy(),
            "command_count":SITE_LIFECYCLE_COMMANDS.len(),
            "coverage":lifecycle_command_map(),
            "cli_module_exists":false,
            "cli_module_path":null,
            "inspected":evidence.get("inspected").and_then(Value::as_bool).unwrap_or(false),
            "resolution_evidence":evidence,
            "items":evidence.get("checks").cloned().unwrap_or_else(|| json!([]))
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
        _ => {}
    }
    let spec = SITE_LIFECYCLE_COMMANDS
        .iter()
        .find(|(tool, _, _, _, _)| *tool == name)
        .ok_or_else(|| diagnostic("unknown_tool", &format!("unknown_tool:{name}")))?;
    if matches!(name, "site_show" | "site_doctor") {
        require_string(args, "site_id")?;
        require_string(args, "site_root")?;
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
    let (result, resolution_status) = if matches!(name, "site_show" | "site_doctor") {
        let evidence = site_resolution_evidence(args, root);
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
        return Ok(json!({
            "schema":"narada.site_registry.doctor.v1","status":"ok","server_name":"site-registry-mcp",
            "implementation":"rust-native","narada_root":root.to_string_lossy(),
            "command_count":SITE_REGISTRY_COMMANDS.len(),"coverage":registry_command_map(),
            "cli_module_exists":false,"cli_module_path":null
        }));
    }
    if name == "site_registry_command_map" {
        return Ok(
            json!({"status":"ok","implementation":"rust-native","commands":registry_command_map(),"count":SITE_REGISTRY_COMMANDS.len()}),
        );
    }
    let (_, cli) = SITE_REGISTRY_COMMANDS
        .iter()
        .find(|(tool, _)| *tool == name)
        .ok_or_else(|| diagnostic("unknown_tool", &format!("unknown_tool:{name}")))?;
    if name == "site_registry_show" {
        let reference = require_string(args, "reference")?;
        return Ok(json!({
            "status":"ok","tool":name,"cli_command":cli,"read_only":true,
            "mutation_performed":false,"implementation":"rust-native",
            "options":{"reference":reference},"result":{"kind":"show","reference":reference,"record":null}
        }));
    }
    let mut options = args.clone();
    if name == "site_registry_discover_plan" {
        options.insert("dryRun".to_string(), Value::Bool(true));
        options.remove("apply");
    }
    let result = if name == "site_registry_list" {
        json!({"kind":"list","records":[]})
    } else {
        json!({"kind":"discover_plan","records":[]})
    };
    Ok(json!({
        "status":"ok","tool":name,"cli_command":cli,"read_only":true,
        "mutation_performed":false,"implementation":"rust-native","options":options,"result":result
    }))
}

fn call_project_state(name: &str, args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    if name == "project_state_guidance" {
        return Ok(guidance_result("project-state", args));
    }
    if name == "project_state_doctor" {
        return Ok(json!({
            "schema":"narada.project_state.doctor.v1","status":"ok","server_name":"project-state-mcp",
            "implementation":"rust-native","project_root":root.to_string_lossy(),"virtual_only":true,
            "read_only":true,"cli_exists":false,"cli_path":null,"command_count":PROJECT_STATE_COMMANDS.len()
        }));
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
    let argv = project_cli_args(name, args)?;
    Ok(json!({
        "schema":"narada.project_state.cli_result.v1","status":"ok","tool":name,"cli_command":cli,
        "read_only":true,"virtual_only":true,"mutation_performed":false,"implementation":"rust-native",
        "result":{"args":argv,"project_root":root.to_string_lossy()}
    }))
}

fn project_cli_args(name: &str, args: &Map<String, Value>) -> Result<Vec<String>, Value> {
    let (mut argv, required) = match name {
        "project_state_program_list" => (vec!["program".to_string(), "list".to_string()], None),
        "project_state_program_show" => (
            vec!["program".to_string(), "show".to_string()],
            Some("program_id"),
        ),
        "project_state_project_list" => (vec!["project".to_string(), "list".to_string()], None),
        "project_state_project_show" => (
            vec!["project".to_string(), "show".to_string()],
            Some("project_id"),
        ),
        "project_state_matrix" => (vec!["matrix".to_string()], None),
        "project_state_gaps" => (vec!["gaps".to_string()], None),
        "project_state_handoff" => (vec!["handoff".to_string()], None),
        "project_state_standards_list" => (vec!["standards".to_string(), "list".to_string()], None),
        "project_state_standard_show" => (
            vec!["standards".to_string(), "show".to_string()],
            Some("standard_id"),
        ),
        "project_state_applicability" => (vec!["applicability".to_string()], None),
        "project_state_standard_trace" => (vec!["trace".to_string()], None),
        "project_state_standard_gaps" => (vec!["standards".to_string(), "gaps".to_string()], None),
        "project_state_validate" => (vec!["validate".to_string()], None),
        _ => return Err(diagnostic("unknown_tool", &format!("unknown_tool:{name}"))),
    };
    if let Some(key) = required {
        argv.push(require_string(args, key)?);
    }
    for (key, flag) in [
        ("program_id", "--program"),
        ("project_id", "--project"),
        ("object_id", "--object"),
        ("lifecycle", "--lifecycle"),
        ("selection", "--selection"),
        ("standard_id", "--standard"),
        ("obligation_id", "--obligation"),
        ("status", "--status"),
    ] {
        if required == Some(key) {
            continue;
        }
        if let Some(value) = args
            .get(key)
            .and_then(Value::as_str)
            .filter(|v| !v.trim().is_empty())
        {
            argv.push(flag.to_string());
            argv.push(value.to_string());
        }
    }
    Ok(argv)
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
        .get("site_root")
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
    if matches!(name, "site_admit_role" | "site_verify_role" | "site_observe_runtime" | "site_bind_runtime") { return operator_surface_schema(name); }
    let mut properties = Map::new();
    properties.insert(
        "site_id".to_string(),
        json!({"type":"string","description":"Canonical Site identifier."}),
    );
    properties.insert("site_root".to_string(), json!({"type":"string","description":"Explicit workspace root or its .narada authority root."}));
    properties.insert(
        "kind".to_string(),
        json!({"type":"string","description":"Lifecycle kind or preflight selector."}),
    );
    properties.insert(
        "substrate".to_string(),
        json!({"type":"string","description":"Requested Site substrate."}),
    );
    properties.insert(
        "authority_basis".to_string(),
        json!({"type":"object","description":"Explicit authority basis for a mutation."}),
    );
    properties.insert("execute".to_string(), json!({"type":"boolean","description":"Whether an owning authority may execute the planned mutation."}));
    properties.insert(
        "dry_run".to_string(),
        json!({"type":"boolean","description":"Return a plan without mutation."}),
    );
    let required: Vec<&str> = match name {
        "site_lifecycle_doctor" | "site_show" | "site_doctor" => vec!["site_id", "site_root"],
        "site_init" => vec!["site_id", "site_root", "substrate", "authority_basis"],
        _ => Vec::new(),
    };
    json!({"type":"object","properties":properties,"required":required,"additionalProperties":false})
}

fn operator_surface_schema(name: &str) -> Value {
    let string = || json!({"type":"string","minLength":1,"maxLength":512});
    let path = || json!({"type":"string","minLength":3,"maxLength":4096});
    let authority = json!({"type":"object","minProperties":1,"maxProperties":32,"additionalProperties":true});
    let (properties, required) = match name {
        "site_admit_role" => (json!({"site_id":string(),"site_root":path(),"role":{"type":"string","enum":["architect","builder","observer"]},"agent_kind":string(),"identity":string(),"label":string(),"by":string(),"input_capabilities":{"type":"string","maxLength":1024},"submit_strategy":{"type":"string","enum":["type_only","operator_confirmed_submit","known_surface_submit"]},"execute":{"type":"boolean","const":true},"authority_basis":authority}), vec!["site_id","site_root","role","agent_kind","by","execute","authority_basis"]),
        "site_verify_role" => (json!({"site_id":string(),"site_root":path(),"runtime_locus":string(),"limit":{"type":"integer","minimum":1,"maximum":500,"default":100}}), vec!["site_id","site_root"]),
        "site_observe_runtime" => (json!({"site_id":string(),"site_root":path(),"limit":{"type":"integer","minimum":1,"maximum":500,"default":100}}), vec!["site_id","site_root"]),
        "site_bind_runtime" => (json!({"site_root":path(),"identity":string(),"runtime_locus":string(),"handle":path(),"observed_handle":path(),"stale_after":{"type":"string","format":"date-time","maxLength":64},"window_title":{"type":"string","maxLength":1024},"window_class":string(),"process_name":string(),"process_id":string(),"execute":{"type":"boolean","const":true},"authority_basis":authority}), vec!["site_root","identity","runtime_locus","handle","execute","authority_basis"]),
        _ => (json!({}), Vec::new()),
    };
    json!({"type":"object","properties":properties,"required":required,"additionalProperties":false})
}

fn tool_with_schema(name: &str, description: &str, read_only: bool, input_schema: Value) -> Value {
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
        "inputSchema":{"type":"object","properties":{},"additionalProperties":false},
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_surface_tool_sets_match_domain_contracts() {
        assert!(list_tools("site-lifecycle")
            .iter()
            .any(|tool| tool["name"] == "site_init"));
        assert!(list_tools("site-registry")
            .iter()
            .any(|tool| tool["name"] == "site_registry_show"));
        assert!(list_tools("project-state")
            .iter()
            .any(|tool| tool["name"] == "project_state_validate"));
    }

    #[test]
    fn site_lifecycle_doctor_requires_coordinates_and_reports_resolution() {
        let doctor = list_tools("site-lifecycle")
            .into_iter()
            .find(|tool| tool["name"] == "site_lifecycle_doctor")
            .expect("doctor tool");
        assert_eq!(
            doctor["inputSchema"]["required"],
            json!(["site_id", "site_root"])
        );

        let args = Map::new();
        let result = call_tool(
            "site-lifecycle",
            "site_lifecycle_doctor",
            &args,
            Path::new("C:/definitely-missing-site"),
        )
        .unwrap();
        assert_eq!(result["status"], "attention");
        assert_eq!(result["inspected"], false);
        assert!(result["items"]
            .as_array()
            .map(|items| !items.is_empty())
            .unwrap_or(false));
        assert_eq!(result["resolution_evidence"]["bound_root_match"], false);
    }

    #[test]
    fn project_state_remains_virtual_and_argument_bounded() {
        let mut args = Map::new();
        args.insert("program_id".to_string(), json!("demo"));
        let result = call_tool(
            "project-state",
            "project_state_program_show",
            &args,
            Path::new("C:/site"),
        )
        .unwrap();
        assert_eq!(result["status"], "ok");
        assert_eq!(result["virtual_only"], true);
        assert_eq!(result["result"]["args"][2], "demo");
    }
}
