use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};

const MAX_PROJECTION_BYTES: u64 = 4 * 1024 * 1024;
const MAX_SOURCE_BYTES: u64 = 4 * 1024 * 1024;
const PROJECTION_SCHEMA: &str = "narada.project_state.registry.v5";

pub fn doctor(root: &Path) -> Value {
    match load(root) {
        Ok(state) => json!({
            "schema":"narada.project_state.doctor.v1","status":"ok","implementation":"rust-native",
            "project_root":root.to_string_lossy(),"read_only":true,"virtual_only":true,
            "projection_path":state.projection_path.to_string_lossy(),"source_path":state.source_path.to_string_lossy(),
            "source_hash_verified":true,"runtime_dependencies":[],"node_required":false,"bun_required":false
        }),
        Err(error) => json!({
            "schema":"narada.project_state.doctor.v1","status":"attention","implementation":"rust-native",
            "project_root":root.to_string_lossy(),"read_only":true,"virtual_only":true,
            "error":error,"runtime_dependencies":[],"node_required":false,"bun_required":false
        }),
    }
}

pub fn call(name: &str, args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let state = load(root)?;
    let payload = &state.payload;
    let result = match name {
        "project_state_program_list" => {
            with_summary(payload, json!({"programs":array(payload,"programs")}))
        }
        "project_state_program_show" => {
            let id = required(args, "program_id")?;
            let program = find(payload, "programs", &id, "program_not_found")?;
            let memberships = filtered(payload, "program_memberships", |item| {
                text(item, "program_id") == Some(id.as_str())
            });
            let projects = filtered(payload, "projects", |item| {
                memberships
                    .iter()
                    .any(|membership| text(membership, "project_id") == text(item, "id"))
            });
            json!({"program":program,"memberships":memberships,"projects":projects})
        }
        "project_state_project_list" => {
            let program = optional(args, "program_id");
            if let Some(id) = program.as_deref() {
                find(payload, "programs", id, "program_not_found")?;
            }
            let projects = filtered(payload, "projects", |item| {
                program.as_deref().is_none_or(|id| {
                    array(payload, "program_memberships")
                        .iter()
                        .any(|membership| {
                            text(membership, "program_id") == Some(id)
                                && text(membership, "project_id") == text(item, "id")
                        })
                })
            });
            with_summary(payload, json!({"projects":projects}))
        }
        "project_state_project_show" => {
            let id = required(args, "project_id")?;
            let project = find(payload, "projects", &id, "project_not_found")?;
            let objects = if text(payload, "project_id") == Some(id.as_str()) {
                array(payload, "objects")
            } else {
                Vec::new()
            };
            json!({"project":project,"objects":objects})
        }
        "project_state_standards_list" => {
            let selection = optional(args, "selection");
            if selection
                .as_deref()
                .is_some_and(|value| !["core", "conditional", "reference"].contains(&value))
            {
                return Err(error(
                    "standard_selection_invalid",
                    "invalid standard selection",
                ));
            }
            let standards = filtered(payload, "standards", |item| {
                selection
                    .as_deref()
                    .is_none_or(|value| text(item, "selection") == Some(value))
            });
            with_summary(payload, json!({"standards":standards}))
        }
        "project_state_standard_show" => {
            let id = required(args, "standard_id")?;
            let standard = find(payload, "standards", &id, "standard_not_found")?;
            json!({"standard":standard,"obligations":filtered(payload,"obligations",|item|text(item,"standard_id")==Some(id.as_str())),"mappings":filtered(payload,"obligation_mappings",|item|text(item,"standard_id")==Some(id.as_str()))})
        }
        "project_state_applicability" => filtered_query(
            payload,
            args,
            "standard_applicability",
            &[
                ("program_id", "program_id"),
                ("project_id", "project_id"),
                ("standard_id", "standard_id"),
                ("status", "applicability"),
            ],
        )?,
        "project_state_standard_trace" => filtered_query(
            payload,
            args,
            "obligation_mappings",
            &[
                ("program_id", "program_id"),
                ("project_id", "project_id"),
                ("standard_id", "standard_id"),
                ("obligation_id", "obligation_id"),
                ("object_id", "object_id"),
                ("lifecycle", "lifecycle"),
                ("status", "alignment_status"),
            ],
        )?,
        "project_state_standard_gaps" => filtered_query(
            payload,
            args,
            "standard_gaps",
            &[
                ("program_id", "program_id"),
                ("project_id", "project_id"),
                ("standard_id", "standard_id"),
            ],
        )?,
        "project_state_matrix" => matrix(payload, args)?,
        "project_state_gaps" => gaps(payload, args)?,
        "project_state_handoff" => handoff(payload, args)?,
        "project_state_validate" => with_summary(payload, json!({"payload":payload})),
        _ => return Err(error("unknown_tool", &format!("unknown_tool:{name}"))),
    };
    Ok(json!({
        "schema":"narada.project_state.cli_result.v1","status":"ok","virtual_only":true,
        "read_only":true,"mutation_performed":false,"implementation":"rust-native",
        "source_hash_verified":true,"result":result
    }))
}

struct State {
    payload: Value,
    projection_path: PathBuf,
    source_path: PathBuf,
}

fn load(root: &Path) -> Result<State, Value> {
    let projection_path = root.join("tmp").join("nrc600_project_state.json");
    let source_path = root
        .join("cad")
        .join("nrc600")
        .join("project_state")
        .join("nrc600_project_state.sql");
    let projection = read_bounded(
        &projection_path,
        MAX_PROJECTION_BYTES,
        "project_state_projection",
    )?;
    let payload: Value = serde_json::from_slice(&projection)
        .map_err(|cause| error("project_state_projection_invalid_json", &cause.to_string()))?;
    if text(&payload, "schema") != Some(PROJECTION_SCHEMA) {
        return Err(error(
            "project_state_projection_schema_unsupported",
            "project-state projection schema is unsupported",
        ));
    }
    let source = read_bounded(&source_path, MAX_SOURCE_BYTES, "project_state_source")?;
    let actual = hex(&Sha256::digest(&source));
    let expected = text(&payload, "source_sha256").unwrap_or_default();
    if actual != expected {
        return Err(
            json!({"schema":"narada.project_state.error.v1","code":"project_state_projection_stale","message":"project-state projection does not match the canonical SQL source","expected_source_sha256":expected,"actual_source_sha256":actual,"remediation":"Rebuild the project-owned projection through its owning authoring workflow."}),
        );
    }
    Ok(State {
        payload,
        projection_path,
        source_path,
    })
}

fn read_bounded(path: &Path, max: u64, label: &str) -> Result<Vec<u8>, Value> {
    let metadata = fs::metadata(path)
        .map_err(|cause| error(&format!("{label}_missing"), &cause.to_string()))?;
    if !metadata.is_file() || metadata.len() > max {
        return Err(error(
            &format!("{label}_invalid"),
            &format!("{label} must be a file of at most {max} bytes"),
        ));
    }
    fs::read(path).map_err(|cause| error(&format!("{label}_read_failed"), &cause.to_string()))
}
fn matrix(payload: &Value, args: &Map<String, Value>) -> Result<Value, Value> {
    let project_id = optional(args, "project_id")
        .or_else(|| text(payload, "project_id").map(ToString::to_string))
        .ok_or_else(|| error("project_id_missing", "project id is missing"))?;
    let project = find(payload, "projects", &project_id, "project_not_found")?;
    let object_id = optional(args, "object_id");
    let lifecycle = optional(args, "lifecycle");
    let mut objects = filtered(payload, "objects", |item| {
        object_id
            .as_deref()
            .is_none_or(|id| text(item, "id") == Some(id))
    });
    if object_id.is_some() && objects.is_empty() {
        return Err(error("object_not_found", "object not found"));
    }
    if let Some(lifecycle) = lifecycle {
        for object in &mut objects {
            if let Some(cells) = object.get_mut("cells").and_then(Value::as_array_mut) {
                cells.retain(|cell| text(cell, "lifecycle") == Some(lifecycle.as_str()));
            }
        }
    }
    Ok(
        json!({"project":project,"objects":objects,"lifecycle":payload.pointer("/axes/lifecycle").cloned().unwrap_or(Value::Null)}),
    )
}
fn gaps(payload: &Value, args: &Map<String, Value>) -> Result<Value, Value> {
    let project = optional(args, "project_id")
        .or_else(|| text(payload, "project_id").map(ToString::to_string));
    if let Some(id) = project.as_deref() {
        find(payload, "projects", id, "project_not_found")?;
    }
    if let Some(id) = optional(args, "program_id").as_deref() {
        find(payload, "programs", id, "program_not_found")?;
    }
    let mut gaps = Vec::new();
    for object in array(payload, "objects") {
        for cell in array(&object, "cells") {
            if text(&cell, "coverage") == Some("gap") {
                let mut row = cell.as_object().cloned().unwrap_or_default();
                row.insert(
                    "object_id".into(),
                    object.get("id").cloned().unwrap_or(Value::Null),
                );
                row.insert(
                    "object_label".into(),
                    object.get("label").cloned().unwrap_or(Value::Null),
                );
                gaps.push(Value::Object(row));
            }
        }
    }
    Ok(with_summary(payload, json!({"gaps":gaps})))
}
fn handoff(payload: &Value, args: &Map<String, Value>) -> Result<Value, Value> {
    let project_id = optional(args, "project_id")
        .or_else(|| text(payload, "project_id").map(ToString::to_string))
        .ok_or_else(|| error("project_id_missing", "project id is missing"))?;
    let project = find(payload, "projects", &project_id, "project_not_found")?;
    let program = optional(args, "program_id");
    if let Some(id) = program.as_deref() {
        find(payload, "programs", id, "program_not_found")?;
    }
    let gap_result = gaps(payload, args)?;
    let standard_gaps = array(payload, "standard_gaps");
    Ok(
        json!({"schema":"narada.project_state.virtual_handoff.v1","status":"ready_for_virtual_handoff","virtual_only":true,"project":project,"program_id":program,"summary":summary(payload),"lifecycle_gaps":gap_result["gaps"],"standard_gaps":standard_gaps,"boundary":payload.get("boundary").cloned().unwrap_or(Value::Null),"reentry_triggers":["canonical SQL source changes","generated projection source hash changes","new lifecycle or standards gap"]}),
    )
}
fn filtered_query(
    payload: &Value,
    args: &Map<String, Value>,
    key: &str,
    fields: &[(&str, &str)],
) -> Result<Value, Value> {
    let values = filtered(payload, key, |item| {
        fields.iter().all(|(argument, field)| {
            optional(args, argument)
                .as_deref()
                .is_none_or(|expected| text(item, field) == Some(expected))
        })
    });
    let output = match key {
        "standard_applicability" => json!({"applicability":values}),
        "obligation_mappings" => json!({"mappings":values}),
        _ => json!({"gaps":values}),
    };
    Ok(with_summary(payload, output))
}
fn with_summary(payload: &Value, value: Value) -> Value {
    let mut result = summary(payload).as_object().cloned().unwrap_or_default();
    if let Some(fields) = value.as_object() {
        result.extend(fields.clone());
    }
    Value::Object(result)
}
fn summary(payload: &Value) -> Value {
    let objects = array(payload, "objects");
    let cells = objects
        .iter()
        .map(|object| array(object, "cells").len())
        .sum::<usize>();
    let gaps = objects
        .iter()
        .flat_map(|object| array(object, "cells"))
        .filter(|cell| text(cell, "coverage") == Some("gap"))
        .count();
    json!({"programs":array(payload,"programs").len(),"projects":array(payload,"projects").len(),"objects":objects.len(),"cells":cells,"gaps":gaps,"standards":array(payload,"standards").len(),"selectedStandards":array(payload,"standard_applicability").iter().filter(|item|text(item,"applicability")==Some("selected")).count(),"obligations":array(payload,"obligations").len(),"obligationMappings":array(payload,"obligation_mappings").len(),"standardGaps":array(payload,"standard_gaps").len(),"actionClaims":array(payload,"action_claims").len(),"virtualOnly":true})
}
fn find(payload: &Value, key: &str, id: &str, code: &str) -> Result<Value, Value> {
    array(payload, key)
        .into_iter()
        .find(|item| text(item, "id") == Some(id))
        .ok_or_else(|| error(code, &format!("{code}:{id}")))
}
fn filtered<F: Fn(&Value) -> bool>(payload: &Value, key: &str, predicate: F) -> Vec<Value> {
    array(payload, key).into_iter().filter(predicate).collect()
}
fn array(value: &Value, key: &str) -> Vec<Value> {
    value
        .get(key)
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}
fn text<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}
fn required(args: &Map<String, Value>, key: &str) -> Result<String, Value> {
    optional(args, key).ok_or_else(|| {
        error(
            "required_argument_missing",
            &format!("required_argument_missing:{key}"),
        )
    })
}
fn optional(args: &Map<String, Value>, key: &str) -> Option<String> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
fn error(code: &str, message: &str) -> Value {
    json!({"schema":"narada.project_state.error.v1","code":code,"message":message})
}
