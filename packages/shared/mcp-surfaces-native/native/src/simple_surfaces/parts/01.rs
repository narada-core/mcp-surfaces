use crate::operator_surface_authority;
use crate::project_state_authority;
use crate::site_lifecycle_authority;
use crate::site_registry_authority;
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};

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
        "site_admit_role",
        "narada operator-surface agent instantiate",
        false,
        true,
        true,
    ),
    (
        "site_verify_role",
        "narada operator-surface doctor",
        true,
        false,
        false,
    ),
    (
        "site_observe_runtime",
        "narada operator-surface status",
        true,
        false,
        false,
    ),
    (
        "site_bind_runtime",
        "narada operator-surface bind-focused",
        false,
        true,
        true,
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
        "retired: legacy JavaScript package-link synchronization",
        true,
        false,
        false,
    ),
    (
        "site_dependency_posture",
        "inspect native dependency posture",
        true,
        false,
        false,
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
                tool_with_schema(
                    name,
                    "Read or plan one canonical site registry operation.",
                    true,
                    registry_input_schema(name),
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
                tool_with_schema(
                    name,
                    "Read one bounded virtual project-state projection.",
                    true,
                    project_input_schema(name),
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

