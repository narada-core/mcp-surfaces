use serde_json::{json, Map, Value};
use std::path::Path;

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
                tool(
                    "site_lifecycle_doctor",
                    "Inspect native site lifecycle MCP posture and command coverage.",
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
                        tool(
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
        return Ok(json!({
            "schema":"narada.site_lifecycle.doctor.v1","status":"ok","server_name":"site-lifecycle-mcp",
            "implementation":"rust-native","site_root":root.to_string_lossy(),
            "command_count":SITE_LIFECYCLE_COMMANDS.len(),"coverage":lifecycle_command_map(),
            "cli_module_exists":false,"cli_module_path":null
        }));
    }
    if name == "site_lifecycle_command_map" {
        return Ok(
            json!({"status":"ok","implementation":"rust-native","commands":lifecycle_command_map(),"count":SITE_LIFECYCLE_COMMANDS.len()}),
        );
    }
    let spec = SITE_LIFECYCLE_COMMANDS
        .iter()
        .find(|(tool, _, _, _, _)| *tool == name)
        .ok_or_else(|| diagnostic("unknown_tool", &format!("unknown_tool:{name}")))?;
    if matches!(name, "site_show" | "site_doctor") {
        require_string(args, "site_id")?;
    }
    if name == "site_init" {
        require_string(args, "site_id")?;
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
    let result = if name == "site_create_presets_list" {
        json!({"presets":[]})
    } else if name == "site_lifecycle_kinds" {
        json!({"kinds":[]})
    } else if name == "site_relation_list" {
        json!({"relations":[]})
    } else {
        json!({"items":[]})
    };
    Ok(json!({
        "schema":"narada.site_lifecycle.result.v1",
        "status":if mutation {"planned"} else {"ok"},
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
        assert!(list_tools("site-lifecycle").iter().any(|tool| tool["name"] == "site_init"));
        assert!(list_tools("site-registry").iter().any(|tool| tool["name"] == "site_registry_show"));
        assert!(list_tools("project-state").iter().any(|tool| tool["name"] == "project_state_validate"));
    }

    #[test]
    fn project_state_remains_virtual_and_argument_bounded() {
        let mut args = Map::new();
        args.insert("program_id".to_string(), json!("demo"));
        let result = call_tool("project-state", "project_state_program_show", &args, Path::new("C:/site")).unwrap();
        assert_eq!(result["status"], "ok");
        assert_eq!(result["virtual_only"], true);
        assert_eq!(result["result"]["args"][2], "demo");
    }
}
