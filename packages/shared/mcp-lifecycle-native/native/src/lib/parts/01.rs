use rusqlite::{params, Connection, OptionalExtension, Row};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::env;
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;
use uuid::Uuid;

const TASK_PROTOCOL_VERSION: &str = "2026-04-18";
const WORK_PROTOCOL_VERSION: &str = "2024-11-05";
const MODERN_PROTOCOL_VERSION: &str = "2026-07-28";
const SERVER_VERSION: &str = "0.1.0";
const TASK_SCHEMA_VERSION: i64 = 1;
const WORK_SCHEMA_VERSION: i64 = 2;
const TASK_SCHEMA: &str = include_str!("../../../../catalog/task-schema.sql");
const WORK_SCHEMA: &str = include_str!("../../../../catalog/work-schema.sql");
const TASK_TOOLS: &str = include_str!("../../../../catalog/task-tools.json");
const WORK_TOOLS: &str = include_str!("../../../../catalog/work-tools.json");
const TASK_GUIDANCE: &str = include_str!("../../../../catalog/task-guidance.json");
const TASK_PAYLOAD_SCHEMAS: &str = include_str!("../../../../catalog/task-payload-schemas.json");

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Surface {
    Task,
    Work,
}

impl Surface {
    pub fn from_name(value: &str) -> Result<Self, String> {
        match value {
            "task" | "task-lifecycle" | "task-lifecycle-mcp" => Ok(Self::Task),
            "work" | "work-lifecycle" | "work-lifecycle-mcp" => Ok(Self::Work),
            other => Err(format!("unknown_lifecycle_surface:{other}")),
        }
    }

    fn server_name(self) -> &'static str {
        match self {
            Self::Task => "narada-task-lifecycle-mcp",
            Self::Work => "work-lifecycle-mcp",
        }
    }

    fn database_relative_path(self) -> &'static str {
        match self {
            Self::Task => ".ai/task-lifecycle.db",
            Self::Work => ".ai/work-lifecycle.db",
        }
    }

    fn tools(self) -> Vec<Value> {
        let source = match self {
            Self::Task => TASK_TOOLS,
            Self::Work => WORK_TOOLS,
        };
        normalized_tool_catalog(source)
    }
}

fn normalized_tool_catalog(source: &str) -> Vec<Value> {
    let mut tools: Vec<Value> =
        serde_json::from_str(source).expect("checked-in lifecycle catalog must be valid JSON");
    for tool in &mut tools {
        let name = tool
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("lifecycle_tool")
            .to_string();
        if let Some(schema) = tool.get_mut("inputSchema") {
            normalize_input_schema(schema, Some(&name));
            if let Some(object) = schema.as_object_mut() {
                object.insert("title".to_string(), json!(format!("{name}.input")));
                object.insert("additionalProperties".to_string(), Value::Bool(false));
            }
        }
    }
    tools
}

fn normalize_input_schema(schema: &mut Value, field: Option<&str>) {
    let Some(object) = schema.as_object_mut() else {
        return;
    };
    match object.get("type").and_then(Value::as_str) {
        Some("string") if !object.contains_key("maxLength") && !object.contains_key("enum") => {
            let name = field.unwrap_or_default().to_ascii_lowercase();
            let maximum = if name.contains("path") || name.contains("root") || name.contains("file") {
                4096
            } else if name.contains("summary") || name.contains("body") || name.contains("context") || name.contains("work") {
                32768
            } else {
                8192
            };
            object.insert("maxLength".to_string(), json!(maximum));
        }
        Some("array") if !object.contains_key("maxItems") => {
            object.insert("maxItems".to_string(), json!(500));
        }
        _ => {}
    }
    if let Some(properties) = object.get_mut("properties").and_then(Value::as_object_mut) {
        for (name, child) in properties {
            normalize_input_schema(child, Some(name));
        }
    }
    if let Some(items) = object.get_mut("items") {
        normalize_input_schema(items, field);
    }
    for keyword in ["allOf", "anyOf", "oneOf"] {
        if let Some(branches) = object.get_mut(keyword).and_then(Value::as_array_mut) {
            for branch in branches {
                normalize_input_schema(branch, field);
            }
        }
    }
}

#[derive(Debug)]
pub struct Options {
    pub surface: Surface,
    pub site_root: PathBuf,
    pub site_root_source: String,
    pub prepare: bool,
    pub migrate_legacy: bool,
    pub source_database_path: Option<PathBuf>,
}

impl Options {
    pub fn parse(surface: Surface, argv: &[String]) -> Result<Self, String> {
        let mut site_root: Option<PathBuf> = None;
        let mut prepare = false;
        let mut migrate_legacy = false;
        let mut source_database_path = None;
        let mut i = 0;
        while i < argv.len() {
            match argv[i].as_str() {
                "--help" | "-h" => {
                    return Err("__help__".to_string());
                }
                "--prepare" => prepare = true,
                "--migrate-legacy" => migrate_legacy = true,
                "--site-root" => {
                    i += 1;
                    site_root = Some(PathBuf::from(argv.get(i).ok_or("site_root_required")?));
                }
                "--source-database-path" => {
                    i += 1;
                    source_database_path = Some(PathBuf::from(
                        argv.get(i).ok_or("source_database_path_required")?,
                    ));
                }
                unknown => return Err(format!("unknown_argument:{unknown}")),
            }
            i += 1;
        }
        let (root, site_root_source) = if let Some(root) = site_root {
            (root, "cli:--site-root".to_string())
        } else if let Some(root) = env::var_os("NARADA_SITE_ROOT") {
            (PathBuf::from(root), "env:NARADA_SITE_ROOT".to_string())
        } else {
            (env::current_dir().ok().ok_or("site_root_required")?, "cwd".to_string())
        };
        let root = if root.is_absolute() {
            root
        } else {
            env::current_dir()
                .map_err(|e| format!("site_root_resolve_failed:{e}"))?
                .join(root)
        };
        if prepare && migrate_legacy {
            return Err("prepare_and_migrate_are_mutually_exclusive".to_string());
        }
        if migrate_legacy && surface != Surface::Work {
            return Err("legacy_migration_work_surface_required".to_string());
        }
        Ok(Self {
            surface,
            site_root: root,
            site_root_source,
            prepare,
            migrate_legacy,
            source_database_path,
        })
    }

    pub fn usage(surface: Surface) -> &'static str {
        match surface {
            Surface::Task => "Usage: task-lifecycle-mcp [--prepare] --site-root <path>",
            Surface::Work => "Usage: work-lifecycle-mcp [--prepare | --migrate-legacy --source-database-path <path>] --site-root <path>",
        }
    }
}

pub struct LifecycleServer {
    pub options: Options,
    pub connection: Option<Connection>,
    pub booted_at: String,
}
