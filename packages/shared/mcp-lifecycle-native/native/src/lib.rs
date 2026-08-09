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
const SERVER_VERSION: &str = "0.1.0";
const TASK_SCHEMA_VERSION: i64 = 1;
const WORK_SCHEMA_VERSION: i64 = 2;
const TASK_SCHEMA: &str = include_str!("../../catalog/task-schema.sql");
const WORK_SCHEMA: &str = include_str!("../../catalog/work-schema.sql");
const TASK_TOOLS: &str = include_str!("../../catalog/task-tools.json");
const WORK_TOOLS: &str = include_str!("../../catalog/work-tools.json");

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
        serde_json::from_str(source).expect("checked-in lifecycle catalog must be valid JSON")
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

fn resource_page(params: &Value) -> Result<(usize, usize), String> {
    let has_cursor = params
        .get("cursor")
        .and_then(Value::as_str)
        .map(|value| !value.is_empty())
        .unwrap_or(false);
    if has_cursor && params.get("offset").is_some() {
        return Err("resource_page_cursor_and_offset_are_mutually_exclusive".to_string());
    }
    let offset = if has_cursor {
        params
            .get("cursor")
            .and_then(Value::as_str)
            .ok_or("resource_cursor_invalid")?
            .parse::<usize>()
            .map_err(|_| "resource_cursor_invalid".to_string())?
    } else {
        params
            .get("offset")
            .and_then(Value::as_u64)
            .map(|value| value as usize)
            .unwrap_or(0)
    };
    let limit = params
        .get("limit")
        .and_then(Value::as_u64)
        .map(|value| value as usize)
        .unwrap_or(100);
    if limit == 0 || limit > 1_000 {
        return Err("resource_limit_invalid".to_string());
    }
    Ok((offset, limit))
}

fn valid_output_id(value: &str) -> bool {
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    let length = value.len();
    (3..=64).contains(&length)
        && first.is_ascii_alphanumeric()
        && chars.all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_'))
}

fn output_id_from_reference(reference: &str) -> Result<String, String> {
    let id = reference
        .strip_prefix("mcp_output:")
        .ok_or_else(|| format!("output_ref_invalid: {reference}"))?;
    if !valid_output_id(id) {
        return Err(format!("output_ref_invalid: {reference}"));
    }
    Ok(id.to_string())
}

fn percent_encode(value: &str) -> String {
    value
        .as_bytes()
        .iter()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (*byte as char).to_string()
            }
            _ => format!("%{byte:02X}"),
        })
        .collect()
}

fn percent_decode(value: &str) -> Result<String, String> {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'%' {
            decoded.push(bytes[index]);
            index += 1;
            continue;
        }
        if index + 2 >= bytes.len() {
            return Err(format!("output_resource_uri_invalid: {value}"));
        }
        let high = (bytes[index + 1] as char)
            .to_digit(16)
            .ok_or_else(|| format!("output_resource_uri_invalid: {value}"))?;
        let low = (bytes[index + 2] as char)
            .to_digit(16)
            .ok_or_else(|| format!("output_resource_uri_invalid: {value}"))?;
        decoded.push(((high << 4) | low) as u8);
        index += 3;
    }
    String::from_utf8(decoded).map_err(|_| format!("output_resource_uri_invalid: {value}"))
}

fn utf16_len(value: &str) -> usize {
    value.encode_utf16().count()
}
fn valid_payload_id(value: &str) -> bool {
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (3..=64).contains(&value.len())
        && first.is_ascii_alphanumeric()
        && chars.all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_'))
}

fn parse_payload_reference(reference: &str) -> Result<(String, i64), String> {
    let body = reference
        .strip_prefix("mcp_payload:")
        .ok_or_else(|| format!("payload_ref_invalid: {reference}"))?;
    let (id, revision) = body
        .split_once("@v")
        .ok_or_else(|| format!("payload_ref_invalid: {reference}"))?;
    if !valid_payload_id(id) {
        return Err(format!("payload_ref_invalid: {reference}"));
    }
    let revision = revision
        .parse::<i64>()
        .ok()
        .filter(|value| *value > 0)
        .ok_or_else(|| format!("payload_ref_invalid: {reference}"))?;
    Ok((id.to_string(), revision))
}

fn payload_revision_path(root: &Path, id: &str, revision: i64) -> PathBuf {
    root.join(".ai")
        .join("tmp")
        .join("mcp-payloads")
        .join("workspace")
        .join(id)
        .join(format!("v{revision}.json"))
}

fn payload_stable_json(value: &Value) -> String {
    serde_json::to_string(&native_canonical_value(value)).unwrap_or_else(|_| "null".to_string())
}

fn payload_byte_size(value: &Value) -> usize {
    payload_stable_json(value).len()
}

fn payload_object_from_args(
    args: &Value,
    object_key: &str,
    json_key: &str,
) -> Result<Value, String> {
    let object = args.get(object_key);
    let json_text = args.get(json_key).and_then(Value::as_str);
    if object.is_some() && json_text.is_some() {
        let placeholder = object
            .and_then(Value::as_object)
            .map(|value| value.is_empty())
            .unwrap_or(false);
        if !placeholder {
            return Err(format!("payload_{object_key}_and_{json_key}_ambiguous"));
        }
    }
    let value = if let Some(text) = json_text {
        serde_json::from_str::<Value>(text)
            .map_err(|e| format!("payload_{json_key}_must_be_object: {e}"))?
    } else {
        object.cloned().unwrap_or_else(|| json!({}))
    };
    if !value.is_object() {
        return Err(format!("payload_{object_key}_must_be_object"));
    }
    Ok(value)
}

fn merge_json_objects(base: &mut Value, overlay: &Value) -> Result<(), String> {
    let Some(base_object) = base.as_object_mut() else {
        return Err("payload_derive_overlay_parent_not_object".to_string());
    };
    let Some(overlay_object) = overlay.as_object() else {
        return Err("payload_derive_overlay_must_be_object".to_string());
    };
    for (key, value) in overlay_object {
        if let Some(existing) = base_object.get_mut(key) {
            if existing.is_object() && value.is_object() {
                merge_json_objects(existing, value)?;
                continue;
            }
        }
        base_object.insert(key.clone(), value.clone());
    }
    Ok(())
}

fn delete_json_pointer(value: &mut Value, pointer: &str) -> Result<(), String> {
    if !pointer.starts_with('/') {
        return Err(format!("payload_derive_delete_path_invalid: {pointer}"));
    }
    let segments = pointer[1..]
        .split('/')
        .map(|segment| {
            let mut decoded = String::new();
            let bytes = segment.as_bytes();
            let mut index = 0;
            while index < bytes.len() {
                if bytes[index] == b'~' {
                    if index + 1 >= bytes.len() || !matches!(bytes[index + 1], b'0' | b'1') {
                        return Err(format!(
                            "payload_derive_delete_path_invalid_escape: {pointer}"
                        ));
                    }
                    decoded.push(if bytes[index + 1] == b'0' { '~' } else { '/' });
                    index += 2;
                } else {
                    decoded.push(bytes[index] as char);
                    index += 1;
                }
            }
            Ok(decoded)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let Some(mut current) = value.as_object_mut() else {
        return Err(format!(
            "payload_derive_delete_path_parent_not_object: {pointer}"
        ));
    };
    for segment in &segments[..segments.len().saturating_sub(1)] {
        let Some(next) = current.get_mut(segment).and_then(Value::as_object_mut) else {
            return Err(format!(
                "payload_derive_delete_path_parent_not_object: {pointer}"
            ));
        };
        current = next;
    }
    let Some(last) = segments.last() else {
        return Err(format!("payload_derive_delete_path_not_found: {pointer}"));
    };
    if current.remove(last).is_none() {
        return Err(format!("payload_derive_delete_path_not_found: {pointer}"));
    }
    Ok(())
}
fn enforce_task_create_payload_contract(args: &Value) -> Result<(), String> {
    let inline_fields = [
        "title",
        "goal",
        "context",
        "required_work",
        "non_goals",
        "acceptance_criteria",
        "tags",
        "preferred_role",
        "target_role",
        "idempotency_key",
        "execution_binding",
    ];
    let fields = inline_fields
        .iter()
        .filter(|field| args.get(*field).is_some())
        .copied()
        .collect::<Vec<_>>();
    if !fields.is_empty() {
        return Err(format!(
            "task_lifecycle_create_inline_definition_refused: task definition fields must be supplied by immutable payload_ref, not inline tool arguments; fields={}",
            fields.join(",")
        ));
    }
    if args.get("payload_path").is_some() {
        return Err("task_lifecycle_create_payload_path_refused: task_lifecycle_create requires immutable payload_ref, not payload_path".to_string());
    }
    if string_arg(args, "payload_ref").is_none() {
        return Err("task_lifecycle_create_requires_payload_ref".to_string());
    }
    Ok(())
}
fn read_payload_revision_payload(root: &Path, reference: &str) -> Result<Value, String> {
    let (id, revision) = parse_payload_reference(reference)?;
    let path = payload_revision_path(root, &id, revision);
    let metadata = fs::metadata(&path).map_err(|_| format!("payload_ref_not_found:{reference}"))?;
    let max_bytes = 256 * 1024usize;
    if metadata.len() > max_bytes as u64 {
        return Err(format!(
            "payload_ref_too_large: {} > {max_bytes}",
            metadata.len()
        ));
    }
    let text = fs::read_to_string(&path).map_err(|e| format!("payload_ref_read_failed:{e}"))?;
    let record: Value =
        serde_json::from_str(&text).map_err(|e| format!("payload_ref_invalid_json:{e}"))?;
    if record.get("schema").and_then(Value::as_str) != Some("narada.mcp_payload.revision.v1")
        || record.get("ref").and_then(Value::as_str) != Some(reference)
        || record.get("payload_id").and_then(Value::as_str) != Some(id.as_str())
        || record.get("revision").and_then(Value::as_i64) != Some(revision)
    {
        return Err(format!("payload_ref_metadata_mismatch:{reference}"));
    }
    let payload = record
        .get("payload")
        .cloned()
        .ok_or_else(|| format!("payload_ref_payload_must_be_object:{reference}"))?;
    if !payload.is_object() {
        return Err(format!("payload_ref_payload_must_be_object:{reference}"));
    }
    if record.get("byte_size").and_then(Value::as_u64) != Some(payload_byte_size(&payload) as u64) {
        return Err(format!("payload_ref_byte_size_mismatch:{reference}"));
    }
    if record.get("sha256").and_then(Value::as_str) != Some(digest(&payload).as_str()) {
        return Err(format!("payload_ref_sha256_mismatch:{reference}"));
    }
    Ok(payload)
}
impl LifecycleServer {
    pub fn new(options: Options) -> Result<Self, String> {
        let booted_at = now();
        if options.prepare {
            Self::prepare_database(&options)?;
            return Ok(Self {
                options,
                connection: None,
                booted_at,
            });
        }
        if options.migrate_legacy {
            return Ok(Self {
                options,
                connection: None,
                booted_at,
            });
        }
        let path = options.database_path();
        if !path.exists() {
            return Err(format!(
                "{}_store_not_prepared:database_missing",
                options.surface.prefix()
            ));
        }
        let connection = Self::open_runtime(&options)?;
        Ok(Self {
            options,
            connection: Some(connection),
            booted_at,
        })
    }

    pub fn prepare_database(options: &Options) -> Result<Value, String> {
        let path = options.database_path();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| format!("database_directory_create_failed:{e}"))?;
        }
        let mut connection =
            Connection::open(&path).map_err(|e| format!("database_open_failed:{e}"))?;
        configure_connection(&mut connection, true)?;
        connection
            .execute_batch(TASK_SCHEMA)
            .map_err(|e| format!("task_schema_prepare_failed:{e}"))?;
        ensure_task_post_schema(&connection)?;
        ensure_native_auxiliary_schema(&connection)?;
        ensure_downstream_dependency_contracts(&connection)?;
        ensure_task_revision_column(&connection)?;
        if options.surface == Surface::Work {
            connection
                .execute_batch(WORK_SCHEMA)
                .map_err(|e| format!("work_schema_prepare_failed:{e}"))?;
            ensure_work_task_revision_triggers(&connection)?;
        }
        connection
            .pragma_update(None, "user_version", TASK_SCHEMA_VERSION)
            .map_err(|e| format!("schema_version_write_failed:{e}"))?;
        let inspection = inspect_database(options)?;
        Ok(json!({
            "status": "prepared",
            "site_root": options.site_root.to_string_lossy(),
            "preparation": inspection,
        }))
    }

    fn open_runtime(options: &Options) -> Result<Connection, String> {
        let path = options.database_path();
        let mut connection = Connection::open(&path).map_err(|_| {
            format!(
                "{}_store_not_prepared:invalid_database",
                options.surface.prefix()
            )
        })?;
        configure_connection(&mut connection, false)?;
        ensure_task_post_schema(&connection)?;
        ensure_native_auxiliary_schema(&connection)?;
        ensure_downstream_dependency_contracts(&connection)?;
        ensure_task_revision_column(&connection)?;
        let inspection = inspect_connection(options.surface, &connection, &path)?;
        if inspection.get("status").and_then(Value::as_str) != Some("prepared") {
            let reason = inspection
                .get("reason")
                .and_then(Value::as_str)
                .unwrap_or("schema");
            return Err(format!(
                "{}_store_not_prepared:{reason}",
                options.surface.prefix()
            ));
        }
        if options.surface == Surface::Work { ensure_work_task_revision_triggers(&connection)?; }
        Ok(connection)
    }

    pub fn run_stdio(&mut self) -> Result<(), String> {
        if self.options.prepare {
            let output = Self::prepare_database(&self.options)?;
            println!("{}", output);
            return Ok(());
        }
        if self.options.migrate_legacy {
            if self.options.surface != Surface::Work {
                return Err("legacy_migration_work_surface_required".to_string());
            }
            let source = self
                .options
                .source_database_path
                .clone()
                .ok_or("source_database_path_required")?;
            let source = if source.is_absolute() {
                source
            } else {
                self.options.site_root.join(source)
            };
            if !source.exists() {
                return Err("legacy_migration_source_database_missing".to_string());
            }
            let target = self.options.database_path();
            if source != target {
                if target.exists() {
                    return Err("legacy_migration_target_exists".to_string());
                }
                if let Some(parent) = target.parent() {
                    fs::create_dir_all(parent)
                        .map_err(|e| format!("legacy_migration_directory_create_failed:{e}"))?;
                }
                fs::copy(&source, &target)
                    .map_err(|e| format!("legacy_migration_copy_failed:{e}"))?;
            }
            let output = Self::prepare_database(&self.options)?;
            println!(
                "{}",
                json!({"status":"migrated","site_root":self.options.site_root.to_string_lossy(),"source_database_path":source,"target_database_path":target,"preparation":output.get("preparation")})
            );
            return Ok(());
        }
        let stdin = io::stdin();
        let stdout = io::stdout();
        let mut reader = WireReader::new(stdin.lock());
        let mut writer = stdout.lock();
        while let Some((request, framed)) = reader
            .next()
            .map_err(|e| format!("mcp_transport_read_failed:{e}"))?
        {
            let response = self.handle_request(request.clone());
            if let Some(value) = response {
                write_wire(&mut writer, &value, framed)
                    .map_err(|e| format!("mcp_transport_write_failed:{e}"))?;
            }
        }
        Ok(())
    }

    pub fn handle_request(&mut self, request: Value) -> Option<Value> {
        let object = request.as_object()?;
        let id = object.get("id").cloned().unwrap_or(Value::Null);
        let method = object.get("method").and_then(Value::as_str).unwrap_or("");
        if object.get("id").is_none() && method.starts_with("notifications/") {
            return None;
        }
        match self.dispatch(
            method,
            object.get("params").unwrap_or(&Value::Object(Map::new())),
        ) {
            Ok(result) => Some(json!({"jsonrpc":"2.0", "id": id, "result": result})),
            Err(error) => Some(json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": self.error_value(error),
            })),
        }
    }

    fn dispatch(&mut self, method: &str, params: &Value) -> Result<Value, String> {
        match method {
            "initialize" => Ok(json!({
                "protocolVersion": params.get("protocolVersion").and_then(Value::as_str).unwrap_or(match self.options.surface {
                    Surface::Task => TASK_PROTOCOL_VERSION,
                    Surface::Work => WORK_PROTOCOL_VERSION,
                }),
                "capabilities": if self.options.surface == Surface::Task {
                    json!({"tools":{},"resources":{},"prompts":{},"completions":{},"logging":{}})
                } else { json!({"tools":{}}) },
                "serverInfo": {"name": self.options.surface.server_name(), "version": SERVER_VERSION}
            })),
            "tools/list" => Ok(json!({"tools": self.options.surface.tools()})),
            "resources/list" if self.options.surface == Surface::Task => {
                self.resources_list(params)
            }
            "resources/read" if self.options.surface == Surface::Task => {
                self.resources_read(params)
            }
            "prompts/list" if self.options.surface == Surface::Task => Ok(json!({
                "prompts": [{"name":"task_lifecycle_workflow","title":"Task Lifecycle Workflow","description":"Guidance for governed task lifecycle operations.","arguments":[]}]
            })),
            "prompts/get" if self.options.surface == Surface::Task => {
                let name = params.get("name").and_then(Value::as_str).unwrap_or("");
                if name != "task_lifecycle_workflow" {
                    return Err(format!("unknown_prompt:{name}"));
                }
                Ok(
                    json!({"description":"Guidance for governed task lifecycle operations.","messages":[{"role":"user","content":{"type":"text","text":"Inspect task state before mutation. Admit evidence before finish/close transitions and preserve lifecycle authority details in structuredContent."}}]}),
                )
            }
            "completion/complete" if self.options.surface == Surface::Task => {
                let argument_name = params.get("argument").and_then(Value::as_object).and_then(|argument| argument.get("name")).and_then(Value::as_str).unwrap_or("");
                let values = if argument_name == "name" {
                    self.options.surface.tools().iter().filter_map(|v| v.get("name").and_then(Value::as_str)).take(100).map(ToString::to_string).collect::<Vec<_>>()
                } else { Vec::new() };
                Ok(json!({"completion":{"values":values,"total":values.len(),"hasMore":false}}))
            },`n            "logging/setLevel" => Ok(json!({})),
            "tools/call" => {
                let name = params
                    .get("name")
                    .and_then(Value::as_str)
                    .ok_or("tool_name_required")?;
                let args = params
                    .get("arguments")
                    .cloned()
                    .unwrap_or_else(|| json!({}));
                let payload = self.call_tool(name, args)?;
                let is_error = matches!(payload.get("status").and_then(Value::as_str), Some("blocked") | Some("refused"))
                    || payload.get("error").is_some()
                    || payload.get("close_blocked").and_then(Value::as_bool) == Some(true);
                Ok(self.tool_result(name, payload, is_error)?)
            }
            _ => Err(if self.options.surface == Surface::Task { format!("unsupported_mcp_method: {method}") } else { format!("unsupported_mcp_method:{method}") }),
        }
    }

    fn target_locus_status(&self) -> Value {
        let operator_root = ["NARADA_OPERATOR_STATED_SITE_ROOT", "NARADA_REQUESTED_WORK_ROOT", "NARADA_TARGET_SITE_ROOT"]
            .iter()
            .find_map(|key| env::var(key).ok().filter(|value| !value.trim().is_empty()));
        let operator_root = operator_root.map(|value| normalized_path_string(Path::new(&value)));
        let default_root = normalized_path_string(&self.options.site_root);
        let status = if operator_root.as_ref().is_some_and(|value| value != &default_root) {
            "operator_stated_locus_mismatch"
        } else {
            "clear"
        };
        json!({
            "schema": "narada.task_lifecycle.target_locus_guard.v0",
            "default_target_site_root": self.options.site_root.to_string_lossy(),
            "operator_stated_locus_root": operator_root,
            "status": status,
            "explicit_target_site_root_supported": false,
            "rule": "Task lifecycle MCP is bound to its --site-root. Startup/control-surface identity does not authorize mutating a different requested work substrate."
        })
    }

    fn target_locus_guard(&self, name: &str, args: &Value) -> Option<Value> {
        if !is_locus_guarded_mutation(name) {
            return None;
        }
        if (name == "task_lifecycle_bridge_poll" || name == "task_lifecycle_inbox_target")
            && args.get("dry_run").and_then(Value::as_bool) == Some(true)
        {
            return None;
        }
        let status = self.target_locus_status();
        if status.get("status").and_then(Value::as_str) != Some("operator_stated_locus_mismatch") {
            return None;
        }
        let mut refusal = status.as_object().cloned().unwrap_or_default();
        refusal.insert("status".to_string(), json!("refused"));
        refusal.insert("refusal_code".to_string(), json!("target_locus_preflight_required"));
        refusal.insert("tool_name".to_string(), json!(name));
        refusal.insert("remediation".to_string(), json!("Relaunch the task lifecycle MCP for the intended Site, clear the operator-stated locus after explicit correction, or use a mutation surface that accepts explicit target_site_root."));
        Some(Value::Object(refusal))
    }
    fn error_value(&self, message: String) -> Value {
        let prefix = message.split(':').next().unwrap_or(&message);
        if self.options.surface == Surface::Task && prefix == "task_lifecycle_store_not_prepared" {
            let reason = message
                .split_once(':')
                .map(|(_, suffix)| suffix)
                .filter(|suffix| !suffix.is_empty())
                .unwrap_or("unknown");
            return json!({
                "code": -32000,
                "message": message,
                "data": {
                    "schema": "narada.task_lifecycle.not_ready.v1",
                    "code": "task_lifecycle_store_not_prepared",
                    "reason": reason,
                    "site_root": self.options.site_root.to_string_lossy(),
                    "remediation": {
                        "inspect_tool": "task_lifecycle_doctor",
                        "prepare_command": "task-lifecycle-mcp --prepare --site-root <site-root>",
                        "after_prepare": "restart_or_reattach_runtime"
                    }
                }
            });
        }
        if self.options.surface == Surface::Task && prefix != "output_resource_uri_invalid" {
            return json!({"code": -32000, "message": message});
        }
        let schema = match self.options.surface {
            Surface::Task => "narada.task_lifecycle.error.v1",
            Surface::Work => "narada.work_lifecycle.error.v1",
        };
        json!({"code": -32000, "message": message, "data": {"schema": schema, "code": prefix, "site_root": self.options.site_root.to_string_lossy()}})
    }

    fn tool_result(&self, tool_name: &str, payload: Value, is_error: bool) -> Result<Value, String> {
        if self.options.surface == Surface::Work {
            return Ok(tool_result(payload, is_error));
        }
        let compact = serde_json::to_string(&payload).map_err(|e| format!("tool_result_serialize_failed:{e}"))?;
        let inline_limit = 4_000usize;
        let semantic_materialization = (tool_name == "task_lifecycle_finish" && payload.get("review_required").and_then(Value::as_bool) == Some(true)) || (tool_name == "task_lifecycle_close" && payload.get("error").and_then(Value::as_str) == Some("task_close_dependencies_unsatisfied")) || (tool_name == "task_lifecycle_show" && payload.get("lifecycle").and_then(|value| value.get("status")).and_then(Value::as_str) == Some("awaiting_dependencies"));
        if !semantic_materialization && utf16_len(&compact) <= inline_limit {
            let mut structured = if let Some(object) = payload.as_object() {Value::Object(object.clone())} else {json!({"value":payload})};
            if let Some(object) = structured.as_object_mut() {
                object.insert("inline_text_truncated".to_string(), json!(false));
                object.insert("rendered_text_char_length".to_string(), json!(utf16_len(&compact)));
                object.insert("full_output_char_length".to_string(), json!(utf16_len(&compact)));
            }
            let mut result = json!({"content":[{"type":"text","text":compact,"annotations":{"audience":["assistant"]}}],"structuredContent":structured});
            if is_error {result["isError"] = json!(true);}
            return Ok(result);
        }
        let full_text = serde_json::to_string_pretty(&payload).map_err(|e| format!("tool_result_presentation_failed:{e}"))?;
        let output_id = format!("o_{}", Uuid::new_v4().simple().to_string()[..24].to_string());
        let output_ref = format!("mcp_output:{output_id}");
        let created_by = env::var("NARADA_AGENT_ID").ok().filter(|value| !value.trim().is_empty());
        let record = json!({"schema":"narada.mcp_output_ref.v1","ref":output_ref,"output_id":output_id,"tool_name":tool_name,"created_at":now(),"created_by":created_by,"content_type":"application/json","inline_char_limit":inline_limit,"full_output_char_length":utf16_len(&full_text),"truncated":true,"sha256":native_canonical_digest(&payload),"max_bytes":10 * 1024 * 1024,"full_output":payload});
        let serialized = format!("{}\n", serde_json::to_string(&record).map_err(|e| format!("tool_result_record_serialize_failed:{e}"))?);
        if serialized.as_bytes().len() > 10 * 1024 * 1024 {return Err(format!("mcp_output_too_large: {} > {}",serialized.as_bytes().len(),10 * 1024 * 1024));}
        let directory = self.options.site_root.join(".ai").join("tmp").join("mcp-outputs").join("workspace");
        fs::create_dir_all(&directory).map_err(|e| format!("output_resource_directory_create_failed:{e}"))?;
        let path = directory.join(format!("{output_id}.json"));
        let mut file = fs::OpenOptions::new().write(true).create_new(true).open(&path).map_err(|e| format!("mcp_output_write_failed:{e}"))?;
        file.write_all(serialized.as_bytes()).map_err(|e| format!("mcp_output_write_failed:{e}"))?;
        file.sync_all().map_err(|e| format!("mcp_output_sync_failed:{e}"))?;
        let preview: String = full_text.chars().take(1_000).collect();
        let preview_length = utf16_len(&preview);
        let output_status = payload.get("status").and_then(Value::as_str).filter(|value| value.len() <= 32).unwrap_or(if is_error {"error"} else {"ok"});
        let envelope = json!({"schema":"narada.producer_output_page.v1","status":output_status,"truncated":true,"output_ref":output_ref,"ref":output_ref,"result_materialized":true,"tool_name":tool_name,"offset":0,"limit":inline_limit,"next_offset":if preview_length < utf16_len(&full_text) {json!(preview_length)} else {Value::Null},"transport_offset":0,"transport_limit":inline_limit,"transport_next_offset":if preview_length < utf16_len(&full_text) {json!(preview_length)} else {Value::Null},"output_text":preview,"output_truncated":preview_length < utf16_len(&full_text),"reader_tool":"mcp_output_show","site_root":self.options.site_root.to_string_lossy(),"read_command":format!("mcp_output_show({{ \\\"ref\\\": \\\"{output_ref}\\\" }})"),"remediation":format!("Use mcp_output_show with output_ref/ref={output_ref} to read bounded produced JSON pages."),"inline_limit":inline_limit,"full_output_char_length":utf16_len(&full_text)});
        let text = serde_json::to_string(&envelope).map_err(|e| format!("tool_result_envelope_serialize_failed:{e}"))?;
        let mut result = json!({"content":[{"type":"text","text":text,"annotations":{"audience":["assistant"]}}],"structuredContent":envelope});
        if is_error {result["isError"] = json!(true);}
        Ok(result)
    }

    fn call_tool(&mut self, name: &str, args: Value) -> Result<Value, String> {
        let name = normalize_task_tool_name(name);

        if let Some(refusal) = self.target_locus_guard(name, &args) {
            return Ok(refusal);
        }`n        if name == "task_lifecycle_doctor" || name == "work_lifecycle_doctor" {
            return self.doctor(&args);
        }
        if name == "task_lifecycle_restart" {
            return self.task_restart(args);
        }
        if self.options.surface == Surface::Work
            && name.starts_with("task_lifecycle_")
            && !is_task_read_only(name)
        {
            self.check_work_revision(&args, "task_number", "expected_revision")?;
            self.check_work_revision(&args, "parent_task_number", "expected_parent_revision")?;
            self.check_work_revision(&args, "required_task_number", "expected_required_revision")?;
        }
        if self.options.surface == Surface::Work && name.starts_with("task_lifecycle_")
            || name.starts_with("mcp_")
        {
            return self.call_task_tool(name, args);
        }
        if self.options.surface == Surface::Task {
            self.call_task_tool(name, args)
        } else {
            self.call_work_tool(name, args)
        }
    }

    fn check_work_revision(
        &self,
        args: &Value,
        number_key: &str,
        revision_key: &str,
    ) -> Result<(), String> {
        let Some(number) = args.get(number_key).and_then(Value::as_i64) else {
            return Ok(());
        };
        let expected = args
            .get(revision_key)
            .and_then(Value::as_i64)
            .ok_or_else(|| format!("{revision_key}_required"))?;
        let actual: i64 = self
            .connection()?
            .query_row(
                "select revision from task_lifecycle where task_number=?1",
                params![number],
                |r| r.get(0),
            )
            .optional()
            .map_err(db_error)?
            .ok_or_else(|| format!("task_not_found:{number}"))?;
        if actual != expected {
            return Err(format!(
                "task_revision_conflict:expected_{expected}:actual_{actual}"
            ));
        }
        Ok(())
    }
    fn connection(&self) -> Result<&Connection, String> {
        self.connection
            .as_ref()
            .ok_or_else(|| "lifecycle_runtime_not_open".to_string())
    }

    fn restart_request_path(&self) -> PathBuf {
        self.options.site_root.join(".ai").join("tmp").join("task-lifecycle-restart-request.json")
    }

    fn restart_baseline_path(&self) -> PathBuf {
        self.options.site_root.join(".ai").join("tmp").join("mcp-baseline.json")
    }

    fn task_freshness(&self) -> Result<Value, String> {
        let request_path = self.restart_request_path();
        let baseline_path = self.restart_baseline_path();
        let request = read_json_file(&request_path);
        let baseline = read_json_file(&baseline_path);
        let expected_tools = self.options.surface.tools();
        let source_digest = native_canonical_digest(&json!({
            "surface": self.options.surface.server_name(),
            "server_version": SERVER_VERSION,
            "tools": expected_tools,
        }));
        let baseline_digest = baseline.as_ref()
            .and_then(|value| value.get("source_digest"))
            .and_then(Value::as_str)
            .map(ToString::to_string);
        let source_digest_changed = baseline_digest.as_ref().is_some_and(|value| value != &source_digest);
        let pending_restart = request.is_some() || source_digest_changed;
        Ok(json!({
            "schema": "narada.mcp.live_freshness.v0",
            "server_name": self.options.surface.server_name(),
            "server_entrypoint": self.options.surface.server_name(),
            "live_process": {"booted_at": self.booted_at, "pid": std::process::id(), "self_restart_supported": false},
            "source": {"source_digest": source_digest, "source_digest_algorithm": "sha256", "source_files_count": 0, "source_manifest_paths": []},
            "baseline": {"path": baseline_path, "state": if baseline.is_some() {"present"} else {"missing"}, "payload": baseline, "source_newer_than_baseline": source_digest_changed, "source_digest": baseline_digest, "source_digest_algorithm": "sha256", "source_digest_changed": source_digest_changed, "freshness_basis": "native_catalog_digest"},
            "restart_request": {"path": request_path, "state": if request.is_some() {"restart_requested"} else {"no_restart_request"}, "payload": request},
            "host_registry_reference": {"status": "not_observed", "source": "native_stdio"},
            "tool_surface": {"expected_count": expected_tools.len(), "registered_count": expected_tools.len(), "missing_expected_tools": []},
            "pending_restart": pending_restart,
            "stale_live_surface_possible": pending_restart,
            "source_digest": source_digest,
            "baseline_source_digest": baseline_digest,
            "source_digest_changed": source_digest_changed,
            "freshness_basis": "native_catalog_digest",
            "remediation": if pending_restart {json!(["Restart the external stdio MCP carrier, then acknowledge the restart request."])} else {json!(["No pending restart signal is recorded for this MCP server."])}
        }))
    }

    fn task_restart(&self, args: Value) -> Result<Value, String> {
        let mode = args.get("mode").and_then(Value::as_str).unwrap_or("request");
        if !matches!(mode, "request" | "status" | "acknowledge" | "clear") {
            return Err(format!("invalid_restart_mode: {mode}"));
        }
        let request_path = self.restart_request_path();
        let baseline_path = self.restart_baseline_path();
        let existing = read_json_file(&request_path);
        if mode == "status" {
            return Ok(json!({
                "status": if existing.is_some() {"restart_requested"} else {"no_restart_request"},
                "schema": "narada.task_lifecycle.restart_request.v0",
                "can_self_restart": false,
                "restart_mechanism": "external_stdio_mcp_restart_required",
                "request_path": request_path,
                "baseline_path": baseline_path,
                "request": existing,
                "mcp_freshness": self.task_freshness()?,
                "message": if existing.is_some() {"Task-lifecycle MCP restart has been requested. Restart the carrier/session MCP servers externally to load new code."} else {"No task-lifecycle MCP restart request file is present."}
            }));
        }
        if mode == "request" {
            let timestamp = now();
            let note = string_arg(&args, "reason").unwrap_or_else(|| "This native tool cannot restart its own stdio MCP process. Restart the carrier/session externally.".to_string());
            let payload = json!({
                "schema": "narada.mcp.restart_request.v0",
                "requested_at": timestamp,
                "requested_by": env::var("NARADA_AGENT_ID").ok(),
                "reason": note,
                "can_self_restart": false,
                "restart_mechanism": "external_stdio_mcp_restart_required",
                "server_name": self.options.surface.server_name(),
                "target_surface": self.options.surface.server_name(),
                "target_entrypoint": self.options.surface.server_name(),
                "requested_process": {"pid": std::process::id(), "booted_at": self.booted_at},
                "note": note
            });
            write_json_file(&request_path, &payload, "restart_request")?;
            let source_digest = native_canonical_digest(&json!({"surface":self.options.surface.server_name(),"server_version":SERVER_VERSION,"tools":self.options.surface.tools()}));
            let baseline = json!({"schema":"narada.mcp.reload_request.v0","requested_at":timestamp,"surface":self.options.surface.server_name(),"target_entrypoint":self.options.surface.server_name(),"source_digest":source_digest,"note":note});
            write_json_file(&baseline_path, &baseline, "restart_baseline")?;
            return Ok(json!({"status":"restart_requested","schema":"narada.mcp.restart_request.v0","can_self_restart":false,"restart_mechanism":"external_stdio_mcp_restart_required","request_path":request_path,"baseline_path":baseline_path,"requested_at":timestamp,"message":note}));
        }
        let Some(request) = existing else {
            return Ok(json!({"status":"no_restart_request","schema":"narada.mcp.restart_acknowledgement.v0","already_cleared":true,"can_self_restart":false,"restart_mechanism":"external_stdio_mcp_restart_required","request_path":request_path,"baseline_path":baseline_path,"message":"No restart request is pending; the marker is already clear."}));
        };
        let requested_at = request.get("requested_at").and_then(Value::as_str).unwrap_or("");
        if self.booted_at.as_str() <= requested_at {
            return Ok(json!({"status":"restart_acknowledgement_rejected","schema":"narada.mcp.restart_acknowledgement_rejection.v0","can_self_restart":false,"restart_mechanism":"external_stdio_mcp_restart_required","request_path":request_path,"baseline_path":baseline_path,"rejected_at":now(),"reason":"post_request_boot_evidence_missing","validation":{"status":"rejected","reason":"post_request_boot_evidence_missing","live_process_booted_at":self.booted_at,"requested_at":requested_at},"message":"Restart acknowledgement rejected: post-request carrier boot evidence is required before clearing the marker."}));
        }
        fs::remove_file(&request_path).map_err(|e| format!("restart_request_clear_failed:{e}"))?;
        let acknowledged_at = now();
        let source_digest = native_canonical_digest(&json!({"surface":self.options.surface.server_name(),"server_version":SERVER_VERSION,"tools":self.options.surface.tools()}));
        let baseline = json!({"schema":"narada.mcp.restart_acknowledgement.v0","acknowledged_at":acknowledged_at,"acknowledged_by":env::var("NARADA_AGENT_ID").ok(),"reason":string_arg(&args,"reason"),"surface":self.options.surface.server_name(),"server_name":self.options.surface.server_name(),"source_digest":source_digest,"freshness_basis":"native_catalog_digest"});
        write_json_file(&baseline_path, &baseline, "restart_acknowledgement")?;
        Ok(json!({"status":"restart_acknowledged","schema":"narada.mcp.restart_acknowledgement.v0","can_self_restart":false,"restart_mechanism":"external_stdio_mcp_restart_required","request_path":request_path,"baseline_path":baseline_path,"acknowledged_at":acknowledged_at,"baseline":baseline,"message":"External stdio MCP restart acknowledged; restart request marker cleared."}))
    }

    fn doctor(&self, args: &Value) -> Result<Value, String> {
        let preparation = inspect_database(&self.options)?;
        let full = args.get("verbose").and_then(Value::as_bool) == Some(true)
            || args.get("detail").and_then(Value::as_str) == Some("full");
        Ok(match self.options.surface {
            Surface::Task => json!({
                "schema":"narada.task_lifecycle.doctor.v1","status":"ok","detail":if full {"full"} else {"summary"},
                "site_root":self.options.site_root.to_string_lossy(),"site_root_source":self.options.site_root_source,
                "authority_posture":"facade_only","surface_type":"task_lifecycle_mcp",
                "fabric_lifecycle":{"mode":"restart_required","restart_owner":"mcp-loader","reason":"Tool and runtime changes require mcp_loader_surface_restart for the bound task-lifecycle surface."},
                "tool_posture":{"canonical_count":self.options.surface.tools().len(),"deprecated_alias_count":38},
                "site_policy":{"source":"default","roster":{"roles_are_obligation_targets":false}},
                "mcp_freshness":self.task_freshness()?,
                "full_detail_hint":{"verbose":true,"detail":"full"},
                "preparation":preparation,
                "target_locus_guard":{"schema":"narada.task_lifecycle.target_locus_guard.v0","status":self.target_locus_status().get("status").cloned().unwrap_or_else(||json!("unknown")),"explicit_target_site_root_supported":false}
            }),
            Surface::Work => json!({
                "schema":"narada.work_lifecycle.doctor.v1",
                "status":if preparation.get("status").and_then(Value::as_str)==Some("prepared") {"ok"} else {"not_ready"},
                "site_root":self.options.site_root.to_string_lossy(),"preparation":preparation,
                "concurrency":{"database_path":self.options.database_path().to_string_lossy(),"posture":"sqlite_wal_transactional_multi_process","conflict_guards":["sqlite_write_serialization","idempotency_keys","revision_checks"]}
            }),
        })
    }

    fn call_task_tool(&mut self, name: &str, args: Value) -> Result<Value, String> {
        match name {
            "task_lifecycle_list" => self.task_list(args),
            "task_lifecycle_show" => self.task_show(args),
            "task_lifecycle_create" => self.task_create(args),
            "task_lifecycle_claim" => self.task_claim(args),
            "task_lifecycle_continue" => self.task_claim(args),
            "task_lifecycle_unclaim" => self.task_unclaim(args),
            "task_lifecycle_prove_criteria" => self.task_prove_criteria(args),
            "task_lifecycle_admit_evidence" => self.task_admit_evidence(args),
            "task_lifecycle_finish" | "task_lifecycle_submit_work" => self.task_finish(args),
            "task_lifecycle_close"
            | "task_lifecycle_closeout"
            | "task_lifecycle_disposition_closeout" => self.task_closeout(args),
            "task_lifecycle_defer" => self.task_transition(args, "deferred"),
            "task_lifecycle_un_defer" => self.task_transition(args, "opened"),
            "task_lifecycle_reopen" => self.task_transition(args, "opened"),
            "task_lifecycle_roster" => self.roster_list(),
            "task_lifecycle_roster_admit" => self.roster_admit(args),
            "task_lifecycle_next" => self.task_next(args),
            "task_lifecycle_workboard_snapshot" => self.task_workboard(args),
            "task_lifecycle_evidence_preflight" => self.task_evidence_preflight(args),
            "task_lifecycle_self_certification_preflight" => {
                self.task_self_certification_preflight(args)
            }
            "task_lifecycle_guidance" => Ok(guidance_payload(args)),
            "task_lifecycle_payload_schema" => Ok(
                json!({"status":"ok","schema":"narada.task_lifecycle.payload_schema.v0","tool":args.get("tool").cloned().unwrap_or(Value::Null)}),
            ),
            "mcp_payload_create" => self.payload_create(args),
            "mcp_payload_show" | "mcp_payload_validate" => self.payload_read(name, args),
            "mcp_payload_derive" => self.payload_derive(args),
            "mcp_output_show" => self.output_show(args),
            "task_lifecycle_chapter_show" => self.task_chapter_show(args),
            "task_lifecycle_chapter_add_task" => self.task_chapter_add(args),
            "task_lifecycle_tags_update" => self.task_tags_update(args),
            "task_lifecycle_report_blocked" => self.task_report_blocked(args),
            "task_lifecycle_submit_report" => self.task_finish(args),
            "task_lifecycle_review" => self.task_review(args),
            "task_lifecycle_evidence_supersede" => self.task_evidence_supersede(args),
            "task_lifecycle_compatibility_reconcile" => self.task_compatibility_reconcile(args),
            "task_lifecycle_set_routing" => self.task_set_routing(args),
            "task_lifecycle_dependency_declare" => self.task_dependency_declare(args),
            "task_lifecycle_dependency_disposition_record" => {
                self.task_dependency_disposition(args)
            }
            "task_lifecycle_search" => self.task_search(args),
            "task_lifecycle_related" => self.task_related(args),
            "task_lifecycle_inspect" => self.task_show(args),
            "task_lifecycle_inspect_range" => self.task_inspect_range(args),
            "task_lifecycle_audit" => self.task_audit(args),
            "task_lifecycle_obligations" => self.task_obligations(args),
            "task_lifecycle_recurring_create" => self.task_recurring_create(args),
            "task_lifecycle_recurring_run_due" => self.task_recurring_run_due(args),
            "task_lifecycle_recurring_suspend" => {
                self.task_recurring_update_status(args, "suspended")
            }
            "task_lifecycle_recurring_retire" => self.task_recurring_update_status(args, "retired"),
            "task_lifecycle_recurring_trigger" => self.task_recurring_trigger(args),
            "task_lifecycle_recurring_list"
            | "task_lifecycle_recurring_show"
            | "task_lifecycle_recurring_runs" => self.task_recurring_read(name, args),
            "task_lifecycle_executability_request" => self.task_executability_request(args),
            "task_lifecycle_executability_status" => self.task_executability_status(args),
            "task_lifecycle_executability_requests_next" => {
                self.task_executability_requests_next(args)
            }
            "task_lifecycle_executability_complete" => self.task_executability_complete(args),
            "task_lifecycle_executability_override" => self.task_executability_override(args),
            "task_lifecycle_executability_dispatch_check" => {
                self.task_executability_dispatch_check(args)
            }
            "task_lifecycle_test_mcp_tool" => self.task_test_mcp_tool(args),
            "task_lifecycle_run_tests" => self.task_run_tests(args),
            "task_lifecycle_diagnose_task_ref" => self.task_diagnose_ref(args),
            "task_lifecycle_record_observation" | "task_lifecycle_submit_observation" => {
                self.task_record_observation(args)
            }
            "task_lifecycle_bridge_poll" => self.task_bridge_poll(args),
            "task_lifecycle_inbox_target" => self.task_inbox_target(args),
            _ => Err(format!("task_mcp_refused: {name}")),
        }
    }

    fn call_work_tool(&mut self, name: &str, args: Value) -> Result<Value, String> {
        match name {
            "work_lifecycle_doctor" => self.doctor(&args),
            "ticket_list" => native_work_ticket_list(self, args),
            "ticket_show" => native_work_ticket_show(self, args),
            "ticket_sources_list" => native_work_ticket_sources(self, args),
            "ticket_admit_source" => native_work_admit_source_tx(self, args),
            "ticket_processing_context_load" => native_work_processing_context_tx(self, args),
            "ticket_admit_proposal" => native_work_admit_proposal_tx(self, args),
            "ticket_draft_receipt_record" => native_work_record_draft_receipt_tx(self, args),
            "ticket_draft_disposition_reconcile" => native_work_reconcile_draft_tx(self, args),
            "work_outbox_list" => native_work_outbox_list(self, args),
            "work_outbox_consumer_register" => native_work_outbox_register_tx(self, args),
            "work_outbox_ack" => native_work_outbox_ack_tx(self, args),
            "work_outbox_compact" => native_work_outbox_compact_tx(self, args),
            "work_lifecycle_storage_inspect" => native_work_storage_inspect(self),
            _ => Err(format!("unknown_tool:{name}")),
        }
    }

    fn task_diagnose_ref(&self, args: Value) -> Result<Value, String> {
        let connection = self.connection()?;
        let row = if let Some(number) = args.get("task_number").and_then(Value::as_i64) {
            connection.query_row(
                "select task_id, task_number, status, revision, updated_at from task_lifecycle where task_number=?1",
                params![number],
                |r| Ok(json!({"task_id":r.get::<_,String>(0)?,"task_number":r.get::<_,i64>(1)?,"status":r.get::<_,String>(2)?,"revision":r.get::<_,Option<i64>>(3)?,"updated_at":r.get::<_,String>(4)?})),
            ).optional().map_err(db_error)?
        } else if let Some(task_id) = string_arg(&args, "task_id") {
            connection.query_row(
                "select task_id, task_number, status, revision, updated_at from task_lifecycle where task_id=?1",
                params![task_id],
                |r| Ok(json!({"task_id":r.get::<_,String>(0)?,"task_number":r.get::<_,i64>(1)?,"status":r.get::<_,String>(2)?,"revision":r.get::<_,Option<i64>>(3)?,"updated_at":r.get::<_,String>(4)?})),
            ).optional().map_err(db_error)?
        } else {
            return Ok(
                json!({"schema":"narada.task.reference_diagnosis.v1","status":"identity_required"}),
            );
        };
        Ok(json!({
            "schema":"narada.task.reference_diagnosis.v1",
            "status": if row.is_some() {"resolved"} else {"not_found"},
            "input":{"task_id":args.get("task_id"),"task_number":args.get("task_number")},
            "task":row,
            "number_authority":"task_lifecycle"
        }))
    }

    fn task_search(&self, args: Value) -> Result<Value, String> {
        let connection = self.connection()?;
        let query = required_string(&args, "query")?;
        let limit = args
            .get("limit")
            .and_then(Value::as_i64)
            .unwrap_or(20)
            .clamp(1, 100);
        let pattern = format!("%{}%", query.to_lowercase());
        let mut stmt = connection.prepare(
            "select l.task_id,l.task_number,l.status,l.updated_at,s.title,s.goal_markdown,s.required_work_markdown,s.tags_json
               from task_lifecycle l left join task_specs s on s.task_id=l.task_id
              where lower(coalesce(s.title,'')) like ?1
                 or lower(coalesce(s.goal_markdown,'')) like ?1
                 or lower(coalesce(s.required_work_markdown,'')) like ?1
              order by l.task_number desc limit ?2"
        ).map_err(db_error)?;
        let rows = stmt.query_map(params![pattern, limit], |r| {
            let tags = r.get::<_,Option<String>>(7)?.and_then(|v| serde_json::from_str(&v).ok()).unwrap_or_else(||json!([]));
            Ok(json!({"task_id":r.get::<_,String>(0)?,"task_number":r.get::<_,i64>(1)?,"status":r.get::<_,String>(2)?,"updated_at":r.get::<_,String>(3)?,"title":r.get::<_,Option<String>>(4)?,"tags":tags}))
        }).map_err(db_error)?.collect::<Result<Vec<_>,_>>().map_err(db_error)?;
        Ok(
            json!({"schema":"narada.task.search.v1","status":"ok","query":query,"count":rows.len(),"tasks":rows}),
        )
    }

    fn task_related(&self, args: Value) -> Result<Value, String> {
        let number = required_i64(&args, "task_number")?;
        let limit = args
            .get("limit")
            .and_then(Value::as_i64)
            .unwrap_or(20)
            .clamp(1, 100);
        let connection = self.connection()?;
        let source_tags: Value = connection
            .query_row(
                "select coalesce(tags_json,'[]') from task_specs where task_number=?1",
                params![number],
                |r| r.get::<_, String>(0),
            )
            .optional()
            .map_err(db_error)?
            .and_then(|v| serde_json::from_str(&v).ok())
            .unwrap_or_else(|| json!([]));
        let mut rows = Vec::new();
        let mut stmt = connection.prepare("select l.task_number,l.task_id,l.status,s.title,s.tags_json from task_lifecycle l left join task_specs s on s.task_id=l.task_id where l.task_number<>?1 order by l.task_number desc limit ?2").map_err(db_error)?;
        for row in stmt.query_map(params![number, limit], |r| {
            let tags: Value = r.get::<_,Option<String>>(4)?.and_then(|v| serde_json::from_str(&v).ok()).unwrap_or_else(||json!([]));
            Ok(json!({"task_number":r.get::<_,i64>(0)?,"task_id":r.get::<_,String>(1)?,"status":r.get::<_,String>(2)?,"title":r.get::<_,Option<String>>(3)?,"tags":tags}))
        }).map_err(db_error)? {
            let item = row.map_err(db_error)?;
            let related = match (source_tags.as_array(), item.get("tags").and_then(Value::as_array)) {
                (Some(a),Some(b)) => a.iter().any(|x| b.contains(x)),
                _ => false,
            };
            if related { rows.push(item); }
        }
        Ok(
            json!({"schema":"narada.task.related.v1","status":"ok","task_number":number,"count":rows.len(),"related":rows}),
        )
    }

    fn task_audit(&self, args: Value) -> Result<Value, String> {
        let connection = self.connection()?;
        let limit = args
            .get("limit")
            .and_then(Value::as_i64)
            .unwrap_or(100)
            .clamp(1, 200);
        let events = self.query_objects(
            "select * from task_lifecycle_events order by created_at desc limit ?1",
            params![limit],
        )?;
        let reports = self.query_objects(
            "select * from task_reports order by submitted_at desc limit ?1",
            params![limit],
        )?;
        Ok(
            json!({"schema":"narada.task.lifecycle.audit.v1","status":"ok","since":args.get("since"),"until":args.get("until"),"events":events,"reports":reports,"count":events.len()+reports.len()}),
        )
    }

    fn task_obligations(&self, args: Value) -> Result<Value, String> {
        let connection = self.connection()?;
        let agent = required_string(&args, "agent_id")?;
        let limit = args
            .get("limit")
            .and_then(Value::as_i64)
            .unwrap_or(50)
            .clamp(1, 200);
        let rows = self.query_objects(
            "select * from directed_obligations where (target_agent_id=?1 or target_agent_id is null) and (?2 is null or status=?2) order by created_at desc limit ?3",
            params![agent, args.get("status").and_then(Value::as_str), limit],
        )?;
        Ok(
            json!({"schema":"narada.task.obligations.v1","status":"ok","agent_id":agent,"count":rows.len(),"obligations":rows}),
        )
    }

    fn task_next(&self, args: Value) -> Result<Value, String> {
        let agent = required_string(&args, "agent_id")?;
        let mut listed =
            self.task_list(json!({"limit":args.get("limit").cloned().unwrap_or(json!(20))}))?;
        let recommended = listed
            .get_mut("tasks")
            .and_then(Value::as_array_mut)
            .and_then(|tasks| {
                tasks.iter().find(|t| {
                    matches!(
                        t.get("status").and_then(Value::as_str),
                        Some("opened" | "deferred" | "needs_continuation")
                    )
                })
            })
            .cloned();
        Ok(
            json!({"schema":"narada.task.next.v1","status":"ok","agent_id":agent,"recommended_task":recommended,"next_action":recommended.as_ref().map(|_|"claim").unwrap_or("none"),"workboard":listed}),
        )
    }

    fn task_workboard(&self, args: Value) -> Result<Value, String> {
        let agent = required_string(&args, "agent_id")?;
        let tasks =
            self.task_list(json!({"limit":args.get("limit").cloned().unwrap_or(json!(50))}))?;
        Ok(
            json!({"schema":"narada.task.workboard.v1","status":"ok","agent_id":agent,"snapshot":tasks,"generated_at":now(),"state_freshness":{"status":"fresh","last_workboard_check_at":args.get("last_workboard_check_at")}}),
        )
    }

    fn task_inspect_range(&self, args: Value) -> Result<Value, String> {
        let start = args
            .get("start_task_number")
            .and_then(Value::as_i64)
            .unwrap_or(1);
        let end = args
            .get("end_task_number")
            .and_then(Value::as_i64)
            .unwrap_or(start);
        if end < start {
            return Err("task_range_invalid".to_string());
        }
        let limit = args
            .get("limit")
            .and_then(Value::as_i64)
            .unwrap_or(50)
            .clamp(1, 200);
        let rows = self.query_objects("select l.*,s.title,s.tags_json from task_lifecycle l left join task_specs s on s.task_id=l.task_id where l.task_number between ?1 and ?2 order by l.task_number limit ?3", params![start,end,limit])?;
        Ok(
            json!({"schema":"narada.task.inspect_range.v1","status":"ok","start_task_number":start,"end_task_number":end,"count":rows.len(),"tasks":rows}),
        )
    }

    fn task_evidence_preflight(&self, args: Value) -> Result<Value, String> {
        let number = required_i64(&args, "task_number")?;
        let connection = self.connection()?;
        let task = connection
            .query_row(
                "select task_id,status from task_lifecycle where task_number=?1",
                params![number],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
            )
            .optional()
            .map_err(db_error)?
            .ok_or_else(|| format!("task_not_found:{number}"))?;
        let reports: i64 = connection
            .query_row(
                "select count(*) from task_reports where task_id=?1",
                params![task.0],
                |r| r.get(0),
            )
            .map_err(db_error)?;
        let admissions: i64 = connection.query_row("select count(*) from evidence_admission_results where task_id=?1 and verdict='admitted'",params![task.0],|r|r.get(0)).map_err(db_error)?;
        let dependency_satisfaction = self.task_dependency_satisfaction(&task.0)?;
        let mut blockers = if reports == 0 {
            vec![json!({"code":"report_required","message":"Submit a report before closeout."})]
        } else if admissions == 0 {
            vec![
                json!({"code":"evidence_admission_required","message":"Admit evidence before closure."}),
            ]
        } else {
            Vec::new()
        };
        if dependency_satisfaction.get("all_satisfied").and_then(Value::as_bool) == Some(false) {
            blockers.push(json!({"code":"dependencies_unsatisfied","message":"Complete required dependencies before closure.","dependency_satisfaction":dependency_satisfaction.clone()}));
        }
        Ok(
            json!({"schema":"narada.task.mcp.evidence_preflight.v0","status":if blockers.is_empty(){"ready"}else{"blocked"},"task_number":number,"task_id":task.0,"lifecycle_status":task.1,"blockers":blockers,"dependency_satisfaction":dependency_satisfaction,"evidence":{"report_count":reports,"admission_count":admissions},"next_action":if blockers.is_empty(){"task_lifecycle_close"}else{"task_lifecycle_finish"}}),
        )
    }

    fn task_self_certification_preflight(&self, args: Value) -> Result<Value, String> {
        let value = args
            .get("self_certification")
            .cloned()
            .unwrap_or_else(|| json!(null));
        let valid = value.as_object().map(|o| !o.is_empty()).unwrap_or(false);
        Ok(
            json!({"schema":"narada.task.mcp.self_certification_preflight.v0","status":if valid{"ready"}else{"blocked"},"valid":valid,"self_certification":value,"blockers":if valid{json!([])}else{json!([{"code":"self_certification_required"}])}}),
        )
    }

    fn task_record_observation(&mut self, args: Value) -> Result<Value, String> {
        let artifact_uri = required_string(&args, "artifact_uri")?;
        let agent = string_arg(&args, "agent_id")
            .or_else(|| string_arg(&args, "source_operator"))
            .unwrap_or_else(|| "native".to_string());
        let number = args.get("task_number").and_then(Value::as_i64);
        let connection = self.connection_mut()?;
        let (task_id, task_number) = if let Some(n) = number {
            connection
                .query_row(
                    "select task_id,task_number from task_lifecycle where task_number=?1",
                    params![n],
                    |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)),
                )
                .optional()
                .map_err(db_error)?
                .unwrap_or((String::new(), n))
        } else {
            (String::new(), 0)
        };
        let id = format!("artifact-{}", Uuid::new_v4());
        let admitted = json!({"artifact_uri":artifact_uri,"content":args.get("content"),"source_operator":args.get("source_operator"),"agent_id":agent});
        connection.execute("insert into observation_artifacts(artifact_id,artifact_type,source_operator,task_id,task_number,agent_id,artifact_uri,digest,admitted_view_json,created_at) values(?1,'observation',?2,?3,?4,?5,?6,?7,?8,?9)",params![id,agent,if task_id.is_empty(){None::<String>}else{Some(task_id)},task_number,agent,artifact_uri,digest(&admitted),admitted.to_string(),now()]).map_err(db_error)?;
        Ok(
            json!({"schema":"narada.task.observation.v1","status":"admitted","artifact_id":id,"artifact_uri":artifact_uri,"task_number":number}),
        )
    }

    fn task_bridge_poll(&self, args: Value) -> Result<Value, String> {
        let limit = args
            .get("limit")
            .and_then(Value::as_i64)
            .unwrap_or(25)
            .clamp(1, 100);
        let inbox = self.options.site_root.join(".ai").join("inbox");
        let mut envelopes = Vec::new();
        if let Ok(entries) = fs::read_dir(inbox) {
            for entry in entries.flatten().take(limit as usize) {
                let path = entry.path();
                if path.is_file() {
                    envelopes.push(json!({"envelope_id":path.file_stem().and_then(|v|v.to_str()),"path":path.to_string_lossy()}));
                }
            }
        }
        Ok(
            json!({"schema":"narada.task.inbox.bridge.v1","status":if args.get("dry_run").and_then(Value::as_bool)==Some(true){"planned"}else{"ok"},"count":envelopes.len(),"envelopes":envelopes}),
        )
    }

    fn task_inbox_target(&mut self, args: Value) -> Result<Value, String> {
        let envelope = required_string(&args, "envelope_id")?;
        let status = string_arg(&args, "disposition").unwrap_or_else(|| "targeted".to_string());
        if args.get("dry_run").and_then(Value::as_bool) == Some(true) {
            return Ok(
                json!({"schema":"narada.task.inbox.target.v1","status":"planned","envelope_id":envelope,"disposition":status}),
            );
        }
        let connection = self.connection_mut()?;
        connection.execute("insert or replace into envelope_task_mappings(envelope_id,task_id,task_number,materialized_at) values(?1,'',null,?2)",params![envelope,now()]).map_err(db_error)?;
        Ok(
            json!({"schema":"narada.task.inbox.target.v1","status":"targeted","envelope_id":envelope,"disposition":status}),
        )
    }

    fn task_dependency_disposition(&mut self, args: Value) -> Result<Value, String> {
        let dependency_id = required_string(&args, "dependency_id")?;
        let agent = required_string(&args, "agent_id")?;
        let kind = required_string(&args, "kind")?;
        let summary = required_string(&args, "summary")?;
        let connection = self.connection_mut()?;
        let exists: Option<String> = connection
            .query_row(
                "select dependency_id from task_dependencies where dependency_id=?1",
                params![dependency_id],
                |r| r.get(0),
            )
            .optional()
            .map_err(db_error)?;
        if exists.is_none() {
            return Err(format!("dependency_not_found:{dependency_id}"));
        }
        let id = format!("disposition-{}", Uuid::new_v4());
        let status = string_arg(&args, "status").unwrap_or_else(|| "recorded".to_string());
        connection.execute(
            "insert into task_dependency_dispositions(disposition_id,dependency_id,required_outcome_id,kind,status,target_task_id,routed_obligation_id,authority_basis_json,summary,created_by,created_at) values(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
            params![id,dependency_id,string_arg(&args,"required_outcome_id"),kind,status,string_arg(&args,"target_task_id"),string_arg(&args,"routed_obligation_id"),args.get("authority_basis").cloned().unwrap_or_else(||json!(null)).to_string(),summary,agent,now()],
        ).map_err(db_error)?;
        connection
            .execute(
                "update task_dependencies set status=?1 where dependency_id=?2",
                params![status, dependency_id],
            )
            .map_err(db_error)?;
        Ok(
            json!({"schema":"narada.task.dependency_disposition.v1","status":"recorded","disposition_id":id,"dependency_id":dependency_id,"kind":kind,"summary":summary}),
        )
    }

    fn task_recurring_read(&self, name: &str, args: Value) -> Result<Value, String> {
        let connection = self.connection()?;
        let limit = args
            .get("limit")
            .and_then(Value::as_i64)
            .unwrap_or(20)
            .clamp(1, 100);
        if name == "task_lifecycle_recurring_list" {
            let rows = self.query_objects(
                "select recurrence_id,status,definition_json,last_due_key,last_auto_triggered_at,updated_at from recurring_task_definitions where (?1 is null or status=?1) order by updated_at desc limit ?2",
                params![args.get("status").and_then(Value::as_str),limit],
            )?;
            let definitions = rows
                .into_iter()
                .map(|mut row| {
                    if let Some(text) = row.get("definition_json").and_then(Value::as_str) {
                        if let Ok(parsed) = serde_json::from_str::<Value>(text) {
                            if let Some(obj) = parsed.as_object() {
                                let mut merged = obj.clone();
                                for (key, value) in row.as_object().unwrap_or(&Map::new()).iter() {
                                    merged.entry(key.clone()).or_insert_with(|| value.clone());
                                }
                                row = Value::Object(merged);
                            }
                        }
                    }
                    row
                })
                .collect::<Vec<_>>();
            return Ok(
                json!({"schema":"narada.task.recurring.list.v1","status":"ok","count":definitions.len(),"definitions":definitions}),
            );
        }
        let recurrence_id = required_string(&args, "recurrence_id")?;
        let row = connection.query_row(
            "select recurrence_id,status,definition_json,last_due_key,last_auto_triggered_at,updated_at from recurring_task_definitions where recurrence_id=?1",
            params![recurrence_id],
            |r| Ok(json!({"recurrence_id":r.get::<_,String>(0)?,"status":r.get::<_,String>(1)?,"definition_json":r.get::<_,String>(2)?,"last_due_key":r.get::<_,Option<String>>(3)?,"last_auto_triggered_at":r.get::<_,Option<String>>(4)?,"updated_at":r.get::<_,String>(5)?})),
        ).optional().map_err(db_error)?.ok_or_else(||format!("recurring_definition_not_found:{recurrence_id}"))?;
        if name == "task_lifecycle_recurring_runs" {
            let runs = self.query_objects("select run_json from recurring_task_runs where recurrence_id=?1 order by created_at desc limit ?2",params![recurrence_id,limit])?.into_iter().filter_map(|r|r.get("run_json").and_then(Value::as_str).and_then(|v|serde_json::from_str::<Value>(v).ok())).collect::<Vec<_>>();
            return Ok(
                json!({"schema":"narada.task.recurring.runs.v1","status":"ok","recurrence_id":recurrence_id,"count":runs.len(),"runs":runs}),
            );
        }
        Ok(
            json!({"schema":"narada.task.recurring.v1","status":"ok","definition":row,"runs":if args.get("include_runs").and_then(Value::as_bool)==Some(true){json!(self.query_objects("select run_json from recurring_task_runs where recurrence_id=?1 order by created_at desc limit ?2",params![recurrence_id,limit])?)}else{json!([])}}),
        )
    }

    fn task_recurring_create(&mut self, args: Value) -> Result<Value, String> {
        let title = required_string(&args, "title")?;
        let actor = required_string(&args, "actor_agent_id")?;
        let authority = args
            .get("authority_basis")
            .cloned()
            .ok_or("authority_basis_required")?;
        let recurrence_id = format!("recurrence-{}", Uuid::new_v4());
        let status = string_arg(&args, "initial_status").unwrap_or_else(|| "active".to_string());
        let definition = json!({
            "recurrence_id":recurrence_id,
            "status":status,
            "title":title,
            "goal":args.get("goal"),
            "context":args.get("context"),
            "required_work":args.get("required_work"),
            "non_goals":args.get("non_goals"),
            "acceptance_criteria":args.get("acceptance_criteria").cloned().unwrap_or_else(||json!([])),
            "evidence_requirements":args.get("evidence_requirements").cloned().unwrap_or_else(||json!([])),
            "tags":args.get("tags").cloned().unwrap_or_else(||json!([])),
            "target_role":args.get("target_role"),
            "preferred_role":args.get("preferred_role"),
            "trigger_description":args.get("trigger_description"),
            "trigger_mode":args.get("trigger_mode").cloned().unwrap_or_else(||json!("manual")),
            "schedule_kind":args.get("schedule_kind"),
            "schedule_timezone":args.get("schedule_timezone"),
            "created_by":actor,
            "authority_basis":authority
        });
        let connection = self.connection_mut()?;
        connection.execute("insert into recurring_task_definitions(recurrence_id,status,definition_json,last_due_key,last_auto_triggered_at,updated_at) values(?1,?2,?3,null,null,?4)",params![recurrence_id,status,definition.to_string(),now()]).map_err(db_error)?;
        connection.execute("insert into recurring_task_events(event_id,recurrence_id,event_type,actor_agent_id,authority_basis_json,event_json,created_at) values(?1,?2,'created',?3,?4,?5,?6)",params![format!("recurring-event-{}",Uuid::new_v4()),recurrence_id,actor,args.get("authority_basis").cloned().unwrap_or_else(||json!(null)).to_string(),json!({"definition":definition}).to_string(),now()]).map_err(db_error)?;
        Ok(
            json!({"schema":"narada.task.recurring.create.v1","status":"created","recurrence_id":recurrence_id,"definition":definition}),
        )
    }

    fn task_recurring_update_status(&mut self, args: Value, status: &str) -> Result<Value, String> {
        let id = required_string(&args, "recurrence_id")?;
        let actor = required_string(&args, "actor_agent_id")?;
        let authority = args
            .get("authority_basis")
            .cloned()
            .ok_or("authority_basis_required")?;
        let connection = self.connection_mut()?;
        let changed=connection.execute("update recurring_task_definitions set status=?1,updated_at=?2 where recurrence_id=?3",params![status,now(),id]).map_err(db_error)?;
        if changed == 0 {
            return Err(format!("recurring_definition_not_found:{id}"));
        }
        connection.execute("insert into recurring_task_events(event_id,recurrence_id,event_type,actor_agent_id,authority_basis_json,event_json,created_at) values(?1,?2,?3,?4,?5,?6,?7)",params![format!("recurring-event-{}",Uuid::new_v4()),id,status,actor,authority.to_string(),json!({"reason":args.get("reason")}).to_string(),now()]).map_err(db_error)?;
        Ok(json!({"schema":"narada.task.recurring.update.v1","status":status,"recurrence_id":id}))
    }

    fn task_recurring_trigger(&mut self, args: Value) -> Result<Value, String> {
        let id = required_string(&args, "recurrence_id")?;
        let actor = required_string(&args, "actor_agent_id")?;
        let authority = args
            .get("authority_basis")
            .cloned()
            .ok_or("authority_basis_required")?;
        let connection = self.connection()?;
        let definition: Value=connection.query_row("select definition_json from recurring_task_definitions where recurrence_id=?1 and status not in ('suspended','retired')",params![id],|r|{let text:String=r.get(0)?;Ok(serde_json::from_str(&text).unwrap_or_else(|_|json!({})))}).optional().map_err(db_error)?.ok_or_else(||format!("recurring_definition_not_found:{id}"))?;
        drop(connection);
        let due_key = now();
        let payload = json!({"title":definition.get("title"),"goal":definition.get("goal"),"context":definition.get("context"),"required_work":definition.get("required_work"),"non_goals":definition.get("non_goals"),"acceptance_criteria":definition.get("acceptance_criteria").cloned().unwrap_or_else(||json!([])),"tags":definition.get("tags").cloned().unwrap_or_else(||json!([])),"preferred_role":definition.get("preferred_role"),"target_role":definition.get("target_role"),"idempotency_key":format!("recurring-run:{id}:{due_key}")});
        let created = self.task_create(payload)?;
        let task_id = created.get("task_id").cloned().unwrap_or(Value::Null);
        let task_number = created.get("task_number").cloned().unwrap_or(Value::Null);
        let run_id = format!("recurring-run-{}", Uuid::new_v4());
        let run = json!({"run_id":run_id,"recurrence_id":id,"task_id":task_id,"task_number":task_number,"due_key":due_key,"trigger_mode":args.get("trigger_mode").cloned().unwrap_or_else(||json!("manual")),"reason":args.get("run_reason").cloned().unwrap_or_else(||json!("manual trigger")),"created_at":now()});
        let connection = self.connection_mut()?;
        connection.execute("insert into recurring_task_runs(run_id,recurrence_id,task_id,task_number,due_key,trigger_mode,reason,created_at,run_json) values(?1,?2,?3,?4,?5,?6,?7,?8,?9)",params![run_id,id,task_id.as_str(),task_number.as_i64(),due_key,run.get("trigger_mode").and_then(Value::as_str).unwrap_or("manual"),run.get("reason").and_then(Value::as_str).unwrap_or("manual"),now(),run.to_string()]).map_err(db_error)?;
        connection.execute("update recurring_task_definitions set last_due_key=?1,last_auto_triggered_at=?2,updated_at=?2 where recurrence_id=?3",params![due_key,now(),id]).map_err(db_error)?;
        connection.execute("insert into recurring_task_events(event_id,recurrence_id,event_type,actor_agent_id,authority_basis_json,event_json,created_at) values(?1,?2,'triggered',?3,?4,?5,?6)",params![format!("recurring-event-{}",Uuid::new_v4()),id,actor,authority.to_string(),run.to_string(),now()]).map_err(db_error)?;
        Ok(
            json!({"schema":"narada.task.recurring.trigger.v1","status":"triggered","recurrence_id":id,"run":run,"task":created}),
        )
    }

    fn task_recurring_run_due(&mut self, args: Value) -> Result<Value, String> {
        let actor = required_string(&args, "actor_agent_id")?;
        let authority = args
            .get("authority_basis")
            .cloned()
            .ok_or("authority_basis_required")?;
        let limit = args
            .get("limit")
            .and_then(Value::as_i64)
            .unwrap_or(20)
            .clamp(1, 100);
        let ids=self.query_objects("select recurrence_id from recurring_task_definitions where status='active' order by updated_at limit ?1",params![limit])?.into_iter().filter_map(|r|r.get("recurrence_id").and_then(Value::as_str).map(ToString::to_string)).collect::<Vec<_>>();
        let mut runs = Vec::new();
        for id in ids {
            let result=self.task_recurring_trigger(json!({"recurrence_id":id,"actor_agent_id":actor,"authority_basis":authority,"run_reason":"due"}))?;
            runs.push(result);
        }
        Ok(
            json!({"schema":"narada.task.recurring.run_due.v1","status":"completed","count":runs.len(),"runs":runs}),
        )
    }

    fn task_chapter_show(&self, args: Value) -> Result<Value, String> {
        let chapter = required_string(&args, "chapter_id")?;
        let memberships = self.query_objects("select chapter_id,task_number,order_index,note,actor_agent_id,updated_at from task_chapter_memberships where chapter_id=?1 order by order_index,task_number",params![chapter])?;
        Ok(
            json!({"schema":"narada.task.chapter.v1","status":"ok","chapter_id":chapter,"membership_count":memberships.len(),"memberships":memberships}),
        )
    }

    fn task_chapter_add(&mut self, args: Value) -> Result<Value, String> {
        let chapter = required_string(&args, "chapter_id")?;
        let number = required_i64(&args, "task_number")?;
        let status = {
            let connection = self.connection_mut()?;
            let exists: Option<String> = connection
                .query_row(
                    "select task_id from task_lifecycle where task_number=?1",
                    params![number],
                    |r| r.get(0),
                )
                .optional()
                .map_err(db_error)?;
            if exists.is_none() {
                return Err(format!("task_not_found:{number}"));
            }
            let order=args.get("order_index").and_then(Value::as_i64).unwrap_or_else(||connection.query_row("select coalesce(max(order_index),-1)+1 from task_chapter_memberships where chapter_id=?1",params![chapter],|r|r.get(0)).unwrap_or(0));
            if connection.execute("insert or ignore into task_chapter_memberships(chapter_id,task_number,order_index,note,actor_agent_id,updated_at) values(?1,?2,?3,?4,?5,?6)",params![chapter,number,order,args.get("note").and_then(Value::as_str),args.get("actor_agent_id").and_then(Value::as_str),now()]).map_err(db_error)?==0{"already_present"}else{"added"}
        };
        let memberships=self.query_objects("select chapter_id,task_number,order_index,note,actor_agent_id,updated_at from task_chapter_memberships where chapter_id=?1 order by order_index,task_number",params![chapter])?;
        Ok(
            json!({"schema":"narada.task.chapter_membership.v1","status":status,"chapter_id":chapter,"task_number":number,"membership_count":memberships.len(),"memberships":memberships}),
        )
    }
    fn task_list(&self, args: Value) -> Result<Value, String> {
        let connection = self.connection()?;
        let limit = args
            .get("limit")
            .and_then(Value::as_i64)
            .unwrap_or(50)
            .clamp(1, 200);
        let status = args.get("status").and_then(Value::as_str);
        let agent = args.get("agent_id").and_then(Value::as_str);
        let wanted_tags = args
            .get("tags")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let tag_match = args
            .get("tag_match")
            .and_then(Value::as_str)
            .unwrap_or("all");
        let mut stmt=connection.prepare("select l.task_id,l.task_number,l.status,l.governed_by,l.closed_at,l.closed_by,l.closure_mode,l.relative_priority,l.priority_reason,l.reopened_at,l.reopened_by,l.continuation_packet_json,l.updated_at,s.title,s.tags_json,(select a.agent_id from task_assignments a where a.task_id=l.task_id and a.released_at is null order by a.claimed_at desc limit 1),(select a.claimed_at from task_assignments a where a.task_id=l.task_id and a.released_at is null order by a.claimed_at desc limit 1) from task_lifecycle l left join task_specs s on s.task_id=l.task_id order by l.task_number desc limit 200").map_err(db_error)?;
        let mut tasks = Vec::new();
        let mut rows = stmt.query([]).map_err(db_error)?;
        while let Some(row) = rows.next().map_err(db_error)? {
            let row_status: String = row.get(2).map_err(db_error)?;
            if status.is_some_and(|expected| expected != row_status) {
                continue;
            }
            let assigned: Option<String> = row.get(15).ok().flatten();
            if agent.is_some_and(|expected| assigned.as_deref() != Some(expected)) {
                continue;
            }
            let tags: Value = row
                .get::<_, Option<String>>(14)
                .ok()
                .flatten()
                .and_then(|v| serde_json::from_str(&v).ok())
                .unwrap_or_else(|| json!([]));
            let tag_values = tags.as_array().cloned().unwrap_or_default();
            let tags_match = if wanted_tags.is_empty() {
                true
            } else if tag_match == "any" {
                wanted_tags.iter().any(|tag| tag_values.contains(tag))
            } else {
                wanted_tags.iter().all(|tag| tag_values.contains(tag))
            };
            if !tags_match {
                continue;
            }
            let number: i64 = row.get(1).map_err(db_error)?;
            let task_id: String = row.get(0).map_err(db_error)?;
            let title: Option<String> = row.get(13).ok().flatten();
            let claimed_at: Option<String> = row.get(16).ok().flatten();
            tasks.push(json!({"task_number":number,"task_id":task_id,"task_ref":format!("task #{number}"),"task_reference":{"schema":"narada.task.reference.v1","task_ref":format!("task #{number}"),"task_id":task_id,"task_number":number,"number_authority":"task_lifecycle","task_file_name":format!("{task_id}.md")},"status":row_status,"title":title,"assigned_to":assigned,"claimed_at":claimed_at,"tags":tags,"updated_at":row.get::<_,String>(12).map_err(db_error)?,"projection_consistency":{"status":"coherent","reasons":[]},"executability_posture":{"status":"unknown"}}));
            if tasks.len() >= limit as usize {
                break;
            }
        }
        Ok(
            json!({"status":"ok","count":tasks.len(),"filters":{"status":status,"agent_id":args.get("agent_id"),"tags":args.get("tags").cloned().unwrap_or_else(||json!([])),"tag_match":args.get("tag_match").cloned().unwrap_or(json!("all"))},"projection_consistency":{"status":"snapshot_coherent","stale":false,"snapshot_isolation":"sqlite_transaction","scanned_count":tasks.len(),"returned_count":tasks.len(),"stale_count":0,"contention":{"attempts":1,"retries":0},"stale_tasks":[]},"tasks":tasks}),
        )
    }
    fn task_show(&self, args: Value) -> Result<Value, String> {
        let number = required_i64(&args, "task_number")?;
        let connection = self.connection()?;
        let lifecycle = connection
            .query_row(
                "select * from task_lifecycle where task_number=?1",
                params![number],
                |r| lifecycle_value(r),
            )
            .optional()
            .map_err(db_error)?
            .ok_or_else(|| format!("task_not_found: {number}"))?;
        let task_id = lifecycle
            .get("task_id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let spec=connection.query_row("select title,goal_markdown,context_markdown,required_work_markdown,non_goals_markdown,acceptance_criteria_json,tags_json from task_specs where task_id=?1",params![&task_id],|r|Ok(json!({"title":r.get::<_,String>(0)?,"goal_markdown":r.get::<_,Option<String>>(1)?,"context_markdown":r.get::<_,Option<String>>(2)?,"required_work_markdown":r.get::<_,Option<String>>(3)?,"non_goals_markdown":r.get::<_,Option<String>>(4)?,"acceptance_criteria":serde_json::from_str::<Value>(&r.get::<_,String>(5)?).unwrap_or_else(|_|json!([])),"tags":serde_json::from_str::<Value>(&r.get::<_,String>(6)?).unwrap_or_else(|_|json!([]))}))).optional().map_err(db_error)?;
        let assignment=connection.query_row("select * from task_assignments where task_id=?1 and released_at is null order by claimed_at desc limit 1",params![&task_id],|r|row_to_object(r)).optional().map_err(db_error)?.unwrap_or(Value::Null);
        let tag_updates = self.query_objects(
            "select * from task_tag_updates where task_id=?1 order by updated_at desc limit 100",
            params![&task_id],
        )?;
        let observations=self.query_objects("select * from observation_artifacts where task_id=?1 order by created_at desc limit 100",params![&task_id])?;
        let dependencies=self.query_objects("select d.*,p.task_number as parent_task_number,r.task_number as required_task_number,r.status as required_status from task_dependencies d join task_lifecycle p on p.task_id=d.parent_task_id join task_lifecycle r on r.task_id=d.required_task_id where d.parent_task_id=?1 or d.required_task_id=?1 order by d.created_at",params![&task_id])?;
        let reports = self.query_objects(
            "select * from task_reports where task_id=?1 order by submitted_at desc limit 20",
            params![&task_id],
        )?;
        let legacy_review_rows = self.query_objects(
            "select review_id,reviewer_agent_id,verdict,findings_json,reviewed_at from task_reviews where task_id=?1 order by reviewed_at desc limit 100",
            params![&task_id],
        )?.into_iter().map(|row| json!({
            "review_id":row.get("review_id"),
            "reviewer_agent_id":row.get("reviewer_agent_id"),
            "verdict":row.get("verdict"),
            "reviewed_at":row.get("reviewed_at"),
            "single_operator_meta":Value::Null,
            "authority_role":"legacy_compatibility_projection",
            "primary_authority":false,
            "migration_target":"task_dependencies.task_outcomes",
            "findings":row.get("findings_json").and_then(Value::as_str).and_then(|text|serde_json::from_str::<Value>(text).ok()).unwrap_or_else(||json!([]))
        })).collect::<Vec<_>>();        let routing=connection.query_row("select preferred_role,target_role,preferred_agent_id,updated_at from narada_andrey_task_role_preferences where task_id=?1",params![&task_id],|r|Ok(json!({"preferred_role":r.get::<_,Option<String>>(0)?,"target_role":r.get::<_,Option<String>>(1)?,"preferred_agent_id":r.get::<_,Option<String>>(2)?,"updated_at":r.get::<_,String>(3)?}))).optional().map_err(db_error)?.unwrap_or(Value::Null);
        let dependency_satisfaction = self.task_dependency_satisfaction(&task_id)?;
        let outcome_contract = connection
            .query_row(
                "select contract_id,outcome_type,allowed_outcomes_json,satisfying_outcomes_json,blocking_outcomes_json,required_fields_json,capability_requirement,created_by,created_at from task_outcome_contracts where task_id=?1 order by created_at desc limit 1",
                params![&task_id],
                |r| {
                    let allowed: String = r.get(2)?;
                    let satisfying: String = r.get(3)?;
                    let blocking: String = r.get(4)?;
                    let required: String = r.get(5)?;
                    Ok(json!({"contract_id":r.get::<_,String>(0)?,"task_id":task_id,"outcome_type":r.get::<_,String>(1)?,"allowed_outcomes":serde_json::from_str::<Value>(&allowed).unwrap_or_else(|_|json!([])),"satisfying_outcomes":serde_json::from_str::<Value>(&satisfying).unwrap_or_else(|_|json!([])),"blocking_outcomes":serde_json::from_str::<Value>(&blocking).unwrap_or_else(|_|json!([])),"required_fields":serde_json::from_str::<Value>(&required).unwrap_or_else(|_|json!([])),"capability_requirement":r.get::<_,Option<String>>(6)?,"created_by":r.get::<_,String>(7)?,"created_at":r.get::<_,String>(8)?}))
                },
            )
            .optional()
            .map_err(db_error)?
            .unwrap_or(Value::Null);
        let latest_task_outcome = connection
            .query_row(
                "select outcome_id,task_id,contract_id,agent_id,outcome,summary,findings_json,evidence_refs_json,admitted_at from task_outcomes where task_id=?1 order by admitted_at desc limit 1",
                params![&task_id],
                |r| {
                    let findings: String = r.get(6)?;
                    let evidence_refs: String = r.get(7)?;
                    Ok(json!({"outcome_id":r.get::<_,String>(0)?,"task_id":r.get::<_,String>(1)?,"contract_id":r.get::<_,String>(2)?,"agent_id":r.get::<_,String>(3)?,"outcome":r.get::<_,String>(4)?,"summary":r.get::<_,String>(5)?,"findings":serde_json::from_str::<Value>(&findings).unwrap_or_else(|_|json!([])),"evidence_refs":serde_json::from_str::<Value>(&evidence_refs).unwrap_or_else(|_|json!([])),"admitted_at":r.get::<_,String>(8)?}))
                },
            )
            .optional()
            .map_err(db_error)?
            .unwrap_or(Value::Null);
        let closure_status = if lifecycle.get("status").and_then(Value::as_str) == Some("closed") {
            "closed"
        } else {
            "open"
        };
        let body = task_file_body(&self.options.site_root, number);
        let execution_binding = connection.query_row(
            "select binding_json,created_at,updated_at from narada_task_execution_bindings where task_id=?1",
            params![&task_id],
            |r| Ok(json!({"status":"bound","binding":serde_json::from_str::<Value>(&r.get::<_,String>(0)?).unwrap_or_else(|_|json!(null)),"created_at":r.get::<_,String>(1)?,"updated_at":r.get::<_,String>(2)?})),
        ).optional().map_err(db_error)?.unwrap_or_else(|| json!({"status":"unbound","binding":null}));
        Ok(
            json!({"status":"ok","task_number":number,"task_id":task_id,"task_ref":format!("task #{number}"),"task_reference":{"schema":"narada.task.reference.v1","task_ref":format!("task #{number}"),"task_id":lifecycle.get("task_id"),"task_number":number,"number_authority":"task_lifecycle","task_file_name":format!("{task_id}.md")},"lifecycle":lifecycle,"closure_authority":{"status":closure_status,"has_closure_evidence":lifecycle.get("closed_at").is_some_and(|v|!v.is_null()),"closed_at":lifecycle.get("closed_at"),"closed_by":lifecycle.get("closed_by"),"closure_mode":lifecycle.get("closure_mode")},"spec":spec,"tag_updates":tag_updates,"tag_projection":{"status":"coherent"},"routing":routing,"active_assignment":assignment,"assignment_intents":[],"observations":observations,"execution_binding":execution_binding,"current_execution_evidence":reports.first().cloned(),"legacy_review_rows":legacy_review_rows,"review_authority":{"primary_authority":"task_dependencies.task_outcomes","legacy_review_rows_authority":"compatibility_projection_only","legacy_review_row_count":legacy_review_rows.len(),"dependency_review_count":dependencies.iter().filter(|dependency|dependency.get("kind").and_then(Value::as_str)==Some("review")).count(),"compatibility_note":if legacy_review_rows.is_empty(){"No legacy review rows are present; review authority, if any, is dependency/outcome native."}else{"Legacy review rows are retained for historical readback only. Parent closure and review dependency satisfaction must use task dependency outcomes."}},"dependencies_blocking_this_task":dependencies.iter().filter(|d|d.get("required_status").and_then(Value::as_str)!=Some("closed")).cloned().collect::<Vec<_>>(),"dependency_satisfaction":dependency_satisfaction,"dependency_context":dependencies,"outcome_contract":outcome_contract,"latest_task_outcome":latest_task_outcome,"executability_posture":{"status":"unknown"},"body":body}),
        )
    }
    fn task_create(&mut self, args: Value) -> Result<Value, String> {
        enforce_task_create_payload_contract(&args)?;
        let site_root = self.options.site_root.clone();
        let payload = resolve_payload_args(&site_root, &args)?;
        let title = string_arg(&payload, "title").ok_or("task_lifecycle_create_title_required")?;
        let goal = string_arg(&payload, "goal").unwrap_or_else(|| title.clone());
        let required_work = normalized_text(&payload, "required_work");
        let non_goals = normalized_text(&payload, "non_goals");
        let criteria = payload
            .get("acceptance_criteria")
            .cloned()
            .unwrap_or_else(|| json!([]));
        let tags = payload.get("tags").cloned().unwrap_or_else(|| json!([]));
        let idem = string_arg(&payload, "idempotency_key")
            .unwrap_or_else(|| format!("native-create:{}", digest(&payload)));
        let request_digest = digest(&payload);
        let result = {
            let connection = self.connection_mut()?;
            if let Some(existing)=connection.query_row("select result_json,request_digest from native_task_operations where operation_key=?1",params![&idem],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?))).optional().map_err(db_error)? {
                if existing.1!=request_digest{return Err("task_operation_idempotency_conflict".to_string());}
                return serde_json::from_str(&existing.0).map_err(|e|format!("stored_result_invalid:{e}"));
            }
            let tx = connection.transaction().map_err(db_error)?;
            if let Some(existing)=tx.query_row("select task_id,task_number from task_specs where title=?1 and task_id in(select task_id from task_lifecycle)",params![&title],|r|Ok((r.get::<_,String>(0)?,r.get::<_,i64>(1)?))).optional().map_err(db_error)? {
                let value=json!({"schema":"narada.task.create.v0","status":"already_exists","task_id":existing.0,"task_number":existing.1,"title":title,"idempotency_key":idem,"recovered":false});
                tx.execute("insert or ignore into native_task_operations(operation_key,operation_kind,request_digest,result_json,created_at) values(?1,'task_create',?2,?3,?4)",params![&idem,request_digest,value.to_string(),now()]).map_err(db_error)?;
                tx.commit().map_err(db_error)?;
                return Ok(value);
            }
            let number:i64=tx.query_row("update task_number_sequence set last_allocated=last_allocated+1 where singleton=1 returning last_allocated",[],|r|r.get(0)).map_err(db_error)?;
            let task_id = format!("task-{}", Uuid::new_v4());
            let timestamp = now();
            let governed_by = string_arg(&payload, "preferred_role")
                .or_else(|| string_arg(&payload, "target_role"));
            tx.execute("insert into task_lifecycle(task_id,task_number,status,governed_by,closed_at,closed_by,closure_mode,relative_priority,priority_reason,reopened_at,reopened_by,continuation_packet_json,updated_at) values(?1,?2,'opened',?3,null,null,null,0,null,null,null,null,?4)",params![&task_id,number,governed_by,timestamp]).map_err(db_error)?;
            tx.execute("insert into task_specs(task_id,task_number,title,chapter_markdown,goal_markdown,context_markdown,required_work_markdown,non_goals_markdown,acceptance_criteria_json,dependencies_json,tags_json,updated_at) values(?1,?2,?3,null,?4,?5,?6,?7,?8,'[]',?9,?10)",params![&task_id,number,&title,&goal,string_arg(&payload,"context"),required_work,non_goals,criteria.to_string(),tags.to_string(),timestamp]).map_err(db_error)?;
            let execution_binding = normalize_execution_binding(&site_root, payload.get("execution_binding"), &idem)?;
            validate_execution_binding_scope(&execution_binding, &site_root)?;
            let binding_json = execution_binding.to_string();
            let correlation_key = execution_binding.get("correlation_key").and_then(Value::as_str).unwrap_or(&idem).to_string();
            tx.execute("insert into narada_task_creation_requests(idempotency_key,payload_sha256,task_id,task_number,file_path,execution_binding_json,status,created_at,updated_at) values(?1,?2,?3,?4,?5,?6,'created',?7,?7)", params![&idem,&request_digest,&task_id,number,task_file_path(&site_root,&task_id),&binding_json,&timestamp]).map_err(db_error)?;
            if execution_binding.as_object().is_some() && !execution_binding.as_object().is_some_and(|object| object.is_empty()) {
                tx.execute("insert into narada_task_execution_bindings(task_id,task_number,binding_json,correlation_key,created_at,updated_at) values(?1,?2,?3,?4,?5,?5)", params![&task_id,number,&binding_json,&correlation_key,&timestamp]).map_err(db_error)?;
            }
            if payload.get("preferred_role").is_some() || payload.get("target_role").is_some() || payload.get("preferred_agent_id").is_some() {
                tx.execute("insert into narada_andrey_task_role_preferences(task_id,preferred_role,target_role,preferred_agent_id,updated_at) values(?1,?2,?3,?4,?5)", params![&task_id,string_arg(&payload,"preferred_role"),string_arg(&payload,"target_role"),string_arg(&payload,"preferred_agent_id"),&timestamp]).map_err(db_error)?;
            }
            let event_id = format!("task-event-{}", Uuid::new_v4());
            tx.execute("insert into task_lifecycle_events(event_id,task_id,task_number,event_type,payload_json,created_at) values(?1,?2,?3,'task.created',?4,?5)",params![event_id,&task_id,number,json!({"status":"opened","revision":1,"idempotency_key":idem}).to_string(),timestamp]).map_err(db_error)?;
            let value = json!({"schema":"narada.task.create.v0","status":"created","task_number":number,"task_id":task_id,"file_path":task_file_path(&site_root,&task_id),"title":title,"tags":tags,"idempotency_key":idem,"execution_binding":execution_binding,"recovered":false,"target_role":payload.get("target_role"),"preferred_role":payload.get("preferred_role"),"follow_up":{"status":"enqueued"}});
            tx.execute("insert into native_task_operations(operation_key,operation_kind,request_digest,result_json,created_at) values(?1,'task_create',?2,?3,?4)",params![&idem,request_digest,value.to_string(),timestamp]).map_err(db_error)?;
            tx.commit().map_err(db_error)?;
            write_task_file(
                &site_root,
                &task_id,
                number,
                &title,
                &goal,
                &required_work,
                &non_goals,
                &criteria,
                &tags,
                governed_by.as_deref(),
                &idem,
            )?;
            value
        };
        Ok(result)
    }
    fn task_claim_guard(&self, task_id: &str, agent: &str) -> Result<(), String> {
        let c = self.connection()?;
        let routing: Option<(Option<String>, Option<String>, Option<String>)> = c.query_row(
            "select preferred_role,target_role,preferred_agent_id from narada_andrey_task_role_preferences where task_id=?1",
            params![task_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        ).optional().map_err(db_error)?;
        if let Some((preferred_role, target_role, preferred_agent)) = routing {
            if preferred_agent.as_deref().is_some_and(|value| value != agent) {
                return Err(format!("task_preferred_agent_mismatch:{agent}"));
            }
            if let Some(required_role) = target_role.or(preferred_role) {
                let actual_role: Option<String> = c.query_row("select role from agent_roster where agent_id=?1 and status not in ('retired','revoked')", params![agent], |r| r.get(0)).optional().map_err(db_error)?;
                if actual_role.as_deref().is_some_and(|value| value != required_role) {
                    return Err(format!("task_role_mismatch:expected_{required_role}:actual_{}", actual_role.unwrap_or_default()));
                }
            }
        }
        Ok(())
    }
    fn task_claim(&mut self, args: Value) -> Result<Value, String> {
        let number = required_i64(&args, "task_number")?;
        let agent = required_string(&args, "agent_id")?;
        let (task_id, status): (String, String) = {
            let connection = self.connection()?;
            connection
                .query_row(
                    "select task_id,status from task_lifecycle where task_number=?1",
                    params![number],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()
                .map_err(db_error)?
                .ok_or_else(|| format!("task_not_found: {number}"))?
        };
        if matches!(status.as_str(), "closed" | "confirmed") {
            return Err(format!("task_not_claimable:{status}"));
        }
        self.task_claim_guard(&task_id, &agent)?;
        let connection = self.connection_mut()?;
        let active:Option<(String,String)>=connection.query_row("select assignment_id,agent_id from task_assignments where task_id=?1 and released_at is null order by claimed_at desc limit 1",params![&task_id],|r|Ok((r.get(0)?,r.get(1)?))).optional().map_err(db_error)?;
        if let Some((assignment_id, current)) = active {
            if current == agent {
                return Ok(
                    json!({"status":"already_claimed","assignment_id":assignment_id,"task_number":number,"assignment":{"agent_id":current}}),
                );
            }
            return Err("task_already_claimed".to_string());
        }
        let assignment_id = format!("assignment-{}", Uuid::new_v4());
        let timestamp = now();
        let tx = connection.transaction().map_err(db_error)?;
        tx.execute("insert into task_assignments(assignment_id,task_id,agent_id,agent_identity_ref_json,claimed_at,released_at,release_reason,intent) values(?1,?2,?3,?4,?5,null,null,'primary')",params![&assignment_id,&task_id,&agent,json!({"agent_id":agent}).to_string(),timestamp]).map_err(db_error)?;
        tx.execute(
            "update task_lifecycle set status='claimed',revision=revision+1,updated_at=?1 where task_id=?2",
            params![timestamp, &task_id],
        )
        .map_err(db_error)?;
        tx.execute("insert into task_lifecycle_events(event_id,task_id,task_number,event_type,payload_json,created_at) values(?1,?2,?3,'task.claimed',?4,?5)",params![format!("task-event-{}",Uuid::new_v4()),&task_id,number,json!({"agent_id":agent,"assignment_id":assignment_id}).to_string(),timestamp]).map_err(db_error)?;
        tx.commit().map_err(db_error)?;
        project_task_status(&self.options.site_root, number, "claimed")?;
        Ok(
            json!({"status":"claimed","assignment_id":assignment_id,"task_number":number,"agent_id":agent}),
        )
    }
    fn task_unclaim(&mut self, args: Value) -> Result<Value, String> {
        let number = required_i64(&args, "task_number")?;
        let connection = self.connection_mut()?;
        let task_id: String = connection
            .query_row(
                "select task_id from task_lifecycle where task_number=?1",
                params![number],
                |r| r.get(0),
            )
            .optional()
            .map_err(db_error)?
            .ok_or_else(|| format!("task_not_found: {number}"))?;
        let current: Option<(String, String, String)> = connection.query_row("select assignment_id,agent_id,claimed_at from task_assignments where task_id=?1 and released_at is null order by claimed_at desc limit 1", params![&task_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).optional().map_err(db_error)?;
        let Some((assignment_id, current_agent, claimed_at)) = current else {
            return Ok(json!({"status":"not_claimed","task_number":number,"released":0}));
        };
        if let Some(agent) = string_arg(&args, "agent_id") {
            if agent != current_agent {
                return Ok(json!({"status":"claimed_by_other","task_number":number,"assigned_agent":current_agent,"requested_agent":agent,"assignment_id":assignment_id}));
            }
        }
        let timestamp = now();
        let reason = string_arg(&args, "reason").unwrap_or_else(|| "mcp_unclaim".to_string());
        let changed=connection.execute("update task_assignments set released_at=?1,release_reason=?2 where assignment_id=?3 and released_at is null",params![timestamp,&reason,&assignment_id]).map_err(db_error)?;
        connection.execute("update task_lifecycle set status='opened',revision=revision+1,updated_at=?1 where task_id=?2 and status='claimed'",params![timestamp,&task_id]).map_err(db_error)?;
        connection.execute("insert into task_lifecycle_events(event_id,task_id,task_number,event_type,payload_json,created_at) values(?1,?2,?3,'task.unclaimed',?4,?5)",params![format!("task-event-{}",Uuid::new_v4()),&task_id,number,json!({"reason":reason,"released":changed}).to_string(),timestamp]).map_err(db_error)?;
        project_task_status(&self.options.site_root, number, "opened")?;
        Ok(json!({"status":"unclaimed","task_number":number,"released":changed,"assignment_id":assignment_id,"agent_id":current_agent,"claimed_at":claimed_at}))
    }
    fn task_transition(&mut self, args: Value, status: &str) -> Result<Value, String> {
        let number = required_i64(&args, "task_number")?;
        let connection = self.connection_mut()?;
        let (task_id, current_status): (String, String) = connection
            .query_row(
                "select task_id,status from task_lifecycle where task_number=?1",
                params![number],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(db_error)?
            .ok_or_else(|| format!("task_not_found: {number}"))?;
        let valid = match current_status.as_str() {
            "draft" => matches!(status, "opened"),
            "opened" => matches!(status, "claimed" | "closed" | "deferred"),
            "claimed" => matches!(status, "in_review" | "awaiting_dependencies" | "opened" | "needs_continuation" | "deferred" | "closed"),
            "needs_continuation" => matches!(status, "claimed" | "opened" | "deferred"),
            "in_review" => matches!(status, "closed" | "opened" | "needs_continuation" | "awaiting_dependencies" | "deferred"),
            "awaiting_dependencies" => matches!(status, "closed" | "opened" | "needs_continuation" | "deferred"),
            "deferred" => status == "opened",
            "closed" | "confirmed" => matches!(status, "confirmed" | "opened" | "in_review"),
            _ => false,
        };
        if !valid {
            return Ok(json!({"status":"invalid_transition","error":"invalid_transition","task_number":number,"task_id":task_id,"from_status":current_status,"to_status":status,"message":format!("Cannot transition from '{current_status}' to '{status}'.")}));
        }
        let timestamp = now();
        let changed=connection.execute("update task_lifecycle set status=?1,revision=revision+1,reopened_at=case when ?1='opened' and status in ('closed','confirmed') then ?2 else reopened_at end,reopened_by=case when ?1='opened' and status in ('closed','confirmed') then ?3 else reopened_by end,updated_at=?2 where task_number=?4",params![status,timestamp,string_arg(&args,"agent_id"),number]).map_err(db_error)?;
        if changed == 0 {
            return Err(format!("task_not_found: {number}"));
        }
        connection.execute("insert into task_lifecycle_events(event_id,task_id,task_number,event_type,payload_json,created_at) values(?1,?2,?3,?4,?5,?6)",params![format!("task-event-{}",Uuid::new_v4()),&task_id,number,format!("task.status.{status}"),json!({"new_status":status,"reason":args.get("reason")}).to_string(),timestamp]).map_err(db_error)?;
        project_task_status(&self.options.site_root, number, status)?;
        Ok(json!({"status":"success","task_number":number,"new_status":status}))
    }
    fn task_prove_criteria(&mut self, args: Value) -> Result<Value, String> {
        let number = required_i64(&args, "task_number")?;
        let agent = required_string(&args, "agent_id")?;
        let (task_id, criteria): (String, String) = self.connection()?
            .query_row(
                "select task_id,acceptance_criteria_json from task_specs where task_number=?1",
                params![number],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(db_error)?
            .ok_or_else(|| format!("task_not_found: {number}"))?;
        let path = self.options.site_root.join(".ai/do-not-open/tasks").join(format!("{task_id}.md"));
        let original = fs::read_to_string(&path).map_err(|e| format!("task_file_read_failed:{e}"))?;
        let mut changed = false;
        let mut updated = String::with_capacity(original.len());
        for line in original.lines() {
            if line.trim_start().starts_with("- [ ]") {
                updated.push_str(&line.replacen("[ ]", "[x]", 1));
                changed = true;
            } else { updated.push_str(line); }
            updated.push('\n');
        }
        if !changed {
            return Ok(json!({"status":"no_changes","task_number":number,"message":"No unchecked acceptance criteria found."}));
        }
        let timestamp = now();
        if updated.starts_with("---\n") || updated.starts_with("---\r\n") {
            if let Some(end) = updated[3..].find("\n---") {
                let insertion = 3 + end;
                updated.insert_str(insertion, &format!("\ncriteria_proved_by: {agent}\ncriteria_proved_at: {timestamp}"));
            }
        }
        fs::write(&path, &updated).map_err(|e| format!("task_file_write_failed:{e}"))?;
        let proof_id = format!("proof-{}", Uuid::new_v4());
        let criteria_value: Value = serde_json::from_str(&criteria).unwrap_or_else(|_| json!([]));
        let connection = self.connection_mut()?;
        connection.execute("insert into criteria_proofs(proof_id,task_id,task_number,proved_by,proved_at,criteria_json,verification_binding_json) values(?1,?2,?3,?4,?5,?6,?7)",params![&proof_id,&task_id,number,&agent,timestamp,criteria_value.to_string(),json!({"source":"native","tool":"task_lifecycle_prove_criteria"}).to_string()]).map_err(db_error)?;
        connection.execute("insert into task_lifecycle_events(event_id,task_id,task_number,event_type,payload_json,created_at) values(?1,?2,?3,'task.criteria.proved',?4,?5)",params![format!("task-event-{}",Uuid::new_v4()),&task_id,number,json!({"proof_id":proof_id,"criteria":criteria_value}).to_string(),timestamp]).map_err(db_error)?;
        drop(connection);
        let admission = match self.task_admit_evidence(json!({"task_number":number,"agent_id":agent,"methods":["criteria_proof"],"acceptance_criteria":criteria_value})) {
            Ok(value) => value,
            Err(error) => { let _ = fs::write(&path, original); return Err(error); }
        };
        Ok(json!({"schema":"narada.task.mcp.prove_criteria.v0","status":"proved","proof_id":proof_id,"task_number":number,"criteria":criteria_value,"proved_by":agent,"admission":admission,"criteria_projection_rolled_back":false}))
    }
    fn task_admit_evidence(&mut self, args: Value) -> Result<Value, String> {
        let number = required_i64(&args, "task_number")?;
        let agent = required_string(&args, "agent_id")?;
        let (task_id, status): (String, String) = {
            let connection = self.connection()?;
            connection
                .query_row(
                    "select task_id,status from task_lifecycle where task_number=?1",
                    params![number],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()
                .map_err(db_error)?
                .ok_or_else(|| format!("task_not_found: {number}"))?
        };
        let report_ids=self.query_objects("select report_id from task_reports where task_id=?1 order by submitted_at desc limit 50",params![&task_id])?.into_iter().filter_map(|v|v.get("report_id").cloned()).collect::<Vec<_>>();
        let proof_ids=self.query_objects("select proof_id from criteria_proofs where task_id=?1 order by proved_at desc limit 50",params![&task_id])?.into_iter().filter_map(|v|v.get("proof_id").cloned()).collect::<Vec<_>>();
        let bundle_id = format!("bundle-{}", Uuid::new_v4());
        let admission_id = format!("admission-{}", Uuid::new_v4());
        let timestamp = now();
        let methods = args
            .get("methods")
            .cloned()
            .unwrap_or_else(|| json!(["admission"]));
        if !methods.is_array() { return Err("evidence_methods_must_be_array".to_string()); }
        if methods.as_array().is_some_and(|items| items.is_empty()) { return Err("evidence_methods_required".to_string()); }
        if methods.as_array().is_some_and(|items| items.iter().any(|item| item.as_str() == Some("criteria_proof"))) && proof_ids.is_empty() {
            return Ok(json!({"schema":"narada.task.mcp.admit_evidence.v0","status":"blocked","verdict":"blocked","task_number":number,"blockers":["criteria_proof_required"],"methods":methods}));
        }
        let connection = self.connection_mut()?;
        let tx = connection.transaction().map_err(db_error)?;
        tx.execute("insert into evidence_bundles(bundle_id,task_id,task_number,report_ids_json,verification_run_ids_json,acceptance_criteria_json,review_ids_json,changed_files_json,residuals_json,assembled_at,assembled_by) values(?1,?2,?3,?4,'[]',?5,'[]',?6,?7,?8,?9)",params![&bundle_id,&task_id,number,Value::Array(report_ids.clone()).to_string(),args.get("acceptance_criteria").cloned().unwrap_or_else(||json!([])).to_string(),args.get("changed_files").cloned().unwrap_or_else(||json!([])).to_string(),json!([]).to_string(),timestamp,&agent]).map_err(db_error)?;
        tx.execute("insert into evidence_admission_results(admission_id,bundle_id,task_id,task_number,verdict,methods_json,blockers_json,lifecycle_eligible_status,admitted_at,admitted_by,confirmation_json) values(?1,?2,?3,?4,'admitted',?5,'[]',?6,?7,?8,?9)",params![&admission_id,&bundle_id,&task_id,number,methods.to_string(),status,timestamp,&agent,json!({"proof_ids":proof_ids,"report_ids":report_ids}).to_string()]).map_err(db_error)?;
        tx.execute("insert into task_lifecycle_events(event_id,task_id,task_number,event_type,payload_json,created_at) values(?1,?2,?3,'task.evidence.admitted',?4,?5)",params![format!("task-event-{}",Uuid::new_v4()),&task_id,number,json!({"bundle_id":bundle_id,"admission_id":admission_id,"methods":methods}).to_string(),timestamp]).map_err(db_error)?;
        tx.commit().map_err(db_error)?;
        Ok(
            json!({"schema":"narada.task.mcp.admit_evidence.v0","status":"admitted","verdict":"admitted","bundle_id":bundle_id,"admission_id":admission_id,"task_number":number,"methods":methods,"report_ids":report_ids,"proof_ids":proof_ids}),
        )
    }
    fn task_finish(&mut self, args: Value) -> Result<Value, String> {
        let number = required_i64(&args, "task_number")?;
        let agent = required_string(&args, "agent_id")?;
        let summary =
            string_arg(&args, "summary").ok_or("task_lifecycle_finish_summary_required")?;
        if summary.trim().is_empty() {
            return Err("task_lifecycle_finish_summary_required".to_string());
        }
        let no_files = args.get("no_files_changed").and_then(Value::as_bool) == Some(true);
        let changed_files = args
            .get("changed_files")
            .cloned()
            .unwrap_or_else(|| json!([]));
        let verification = args
            .get("verification")
            .cloned()
            .or_else(|| args.get("verification_summary").cloned())
            .unwrap_or_else(|| json!({}));
        if !no_files
            && changed_files
                .as_array()
                .map(|v| v.is_empty())
                .unwrap_or(true)
            && verification
                .as_object()
                .map(|v| v.is_empty())
                .unwrap_or(true)
        {
            return Err("task_lifecycle_finish_evidence_required".to_string());
        }
        let (task_id, status, assignment_id) = {
            let connection = self.connection()?;
            let task = connection
                .query_row(
                    "select task_id,status from task_lifecycle where task_number=?1",
                    params![number],
                    |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
                )
                .optional()
                .map_err(db_error)?
                .ok_or_else(|| format!("task_not_found: {number}"))?;
            let assignment=connection.query_row("select assignment_id from task_assignments where task_id=?1 and agent_id=?2 and released_at is null order by claimed_at desc limit 1",params![&task.0,&agent],|r|r.get::<_,String>(0)).optional().map_err(db_error)?;
            (task.0, task.1, assignment)
        };
        if !matches!(
            status.as_str(),
            "claimed" | "in_progress" | "opened" | "needs_continuation" | "in_review"
        ) {
            return Err(format!("task_lifecycle_finish_state_refused:{status}"));
        }
        if assignment_id.is_none() {
            return Err("task_lifecycle_finish_claim_required".to_string());
        }
        let report_id = format!("report-{}", Uuid::new_v4());
        let timestamp = now();
        let operation_key = string_arg(&args, "idempotency_key")
            .unwrap_or_else(|| format!("task-finish:{number}:{agent}:{}", digest(&args)));
        let outcome = string_arg(&args, "outcome");
        let report_json = json!({"report_id":report_id,"task_number":number,"task_id":task_id,"agent_id":agent,"summary":summary,"changed_files":changed_files,"verification":verification,"outcome":outcome.clone().map(Value::String).unwrap_or(Value::Null),"findings":args.get("findings"),"evidence_refs":args.get("evidence_refs")});
        let connection = self.connection_mut()?;
        if let Some(stored) = connection
            .query_row(
                "select result_json from native_task_operations where operation_key=?1",
                params![&operation_key],
                |r| r.get::<_, String>(0),
            )
            .optional()
            .map_err(db_error)?
        {
            return serde_json::from_str(&stored).map_err(|e| format!("stored_result_invalid:{e}"));
        }
        let tx = connection.transaction().map_err(db_error)?;
        tx.execute("insert into task_reports(report_id,task_id,agent_id,agent_identity_ref_json,summary,changed_files_json,verification_json,directive_id,submitted_at) values(?1,?2,?3,?4,?5,?6,?7,?8,?9)",params![&report_id,&task_id,&agent,json!({"agent_id":agent}).to_string(),&summary,changed_files.to_string(),verification.to_string(),string_arg(&args,"directive_id"),timestamp]).map_err(db_error)?;
        tx.execute("insert into task_report_records(report_id,task_id,assignment_id,agent_id,agent_identity_ref_json,reported_at,report_json) values(?1,?2,?3,?4,?5,?6,?7)",params![&report_id,&task_id,assignment_id, &agent,json!({"agent_id":agent}).to_string(),timestamp,report_json.to_string()]).map_err(db_error)?;
        let existing_contract: Option<(String,String,String,String,String,String,Option<String>,String,String)> = tx.query_row("select contract_id,outcome_type,allowed_outcomes_json,satisfying_outcomes_json,blocking_outcomes_json,required_fields_json,capability_requirement,created_by,created_at from task_outcome_contracts where task_id=?1 order by created_at desc limit 1", params![&task_id], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?,r.get(7)?,r.get(8)?))).optional().map_err(db_error)?;
        let (contract_id, contract_json, allowed_outcomes, created_contract) = if let Some(row) = existing_contract {
            let allowed = serde_json::from_str::<Value>(&row.2).ok().and_then(|value| value.as_array().cloned()).unwrap_or_default().into_iter().filter_map(|value| value.as_str().map(ToString::to_string)).collect::<Vec<_>>();
            let value = json!({"contract_id":row.0,"task_id":task_id,"outcome_type":row.1,"allowed_outcomes_json":row.2,"satisfying_outcomes_json":row.3,"blocking_outcomes_json":row.4,"required_fields_json":row.5,"capability_requirement":row.6,"created_by":row.7,"created_at":row.8});
            (row.0, value, allowed, false)
        } else {
            let id = format!("contract-completion-{task_id}");
            let allowed_json = json!(["completed"]).to_string();
            let required_json = json!(["summary"]).to_string();
            tx.execute("insert into task_outcome_contracts(contract_id,task_id,outcome_type,allowed_outcomes_json,satisfying_outcomes_json,blocking_outcomes_json,required_fields_json,capability_requirement,created_by,created_at) values(?1,?2,?3,?4,?5,?6,?7,null,?8,?9)", params![&id,&task_id,"completion",&allowed_json,&allowed_json,"[]",&required_json,&agent,&timestamp]).map_err(db_error)?;
            (id.clone(),json!({"contract_id":id,"task_id":task_id,"outcome_type":"completion","allowed_outcomes_json":allowed_json,"satisfying_outcomes_json":allowed_json,"blocking_outcomes_json":"[]","required_fields_json":required_json,"capability_requirement":Value::Null,"created_by":agent,"created_at":timestamp}),vec!["completed".to_string()],true)
        };
        let reviewer = string_arg(&args, "reviewer");
        let mut task_outcome: Option<Value> = None;
        if let Some(outcome_value) = outcome.as_deref() {
            if !allowed_outcomes.iter().any(|allowed| allowed == outcome_value) {
                return Err(format!("outcome_not_allowed:{outcome_value}"));
            }
        }
        if outcome.is_some() || (created_contract && reviewer.is_some()) {
            let outcome_value = outcome.clone().unwrap_or_else(|| "completed".to_string());
            let outcome_id = format!("outcome_{}", Uuid::new_v4());
            let findings_json = args.get("findings").cloned().unwrap_or_else(||json!([])).to_string();
            let evidence_refs_json = args.get("evidence_refs").cloned().unwrap_or_else(||json!([])).to_string();
            tx.execute("insert into task_outcomes(outcome_id,task_id,contract_id,agent_id,outcome,summary,findings_json,evidence_refs_json,admitted_at) values(?1,?2,?3,?4,?5,?6,?7,?8,?9)", params![&outcome_id,&task_id,&contract_id,&agent,&outcome_value,&summary,&findings_json,&evidence_refs_json,&timestamp]).map_err(db_error)?;
            task_outcome = Some(json!({"outcome_id":outcome_id,"task_id":task_id,"contract_id":contract_id,"agent_id":agent,"outcome":outcome_value,"summary":summary,"findings_json":findings_json,"evidence_refs_json":evidence_refs_json,"admitted_at":timestamp}));
        }

        let mut review_dependency: Option<Value> = None;
        let mut review_file: Option<(String, i64)> = None;
        if let Some(reviewer_id) = reviewer.as_deref() {
            let review_number: i64 = tx.query_row("update task_number_sequence set last_allocated=last_allocated+1 where singleton=1 returning last_allocated",[],|r|r.get(0)).map_err(db_error)?;
            let review_task_id = format!("review-{}", Uuid::new_v4());
            let review_contract_id = format!("contract-review-{review_task_id}");
            let dependency_id = format!("dep-review-{task_id}-{review_task_id}");
            let review_title = format!("Review task #{number}");
            tx.execute("insert into task_lifecycle(task_id,task_number,status,governed_by,closed_at,closed_by,closure_mode,relative_priority,priority_reason,reopened_at,reopened_by,continuation_packet_json,updated_at) values(?1,?2,?3,?4,null,null,null,0,null,null,null,null,?5)",params![&review_task_id,review_number,"opened",reviewer_id,timestamp]).map_err(db_error)?;
            tx.execute("insert into task_specs(task_id,task_number,title,chapter_markdown,goal_markdown,context_markdown,required_work_markdown,non_goals_markdown,acceptance_criteria_json,dependencies_json,tags_json,updated_at) values(?1,?2,?3,null,?4,?5,?6,?7,?8,?9,?10,?11)",params![&review_task_id,review_number,&review_title,format!("Review the submitted work for task #{number}."),format!("Review outcome for task #{number}."),"Admit an accepted or rejected review outcome.","Do not mutate the reviewed work.",json!(["A structured review outcome is admitted."]).to_string(),json!([]).to_string(),json!([]).to_string(),timestamp]).map_err(db_error)?;
            tx.execute("insert into task_outcome_contracts(contract_id,task_id,outcome_type,allowed_outcomes_json,satisfying_outcomes_json,blocking_outcomes_json,required_fields_json,capability_requirement,created_by,created_at) values(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",params![&review_contract_id,&review_task_id,"review",json!(["accepted","accepted_with_notes","rejected"]).to_string(),json!(["accepted","accepted_with_notes"]).to_string(),json!(["rejected"]).to_string(),json!(["summary"]).to_string(),"architect_as_reviewer",&agent,timestamp]).map_err(db_error)?;
            tx.execute("insert into task_dependencies(dependency_id,parent_task_id,required_task_id,kind,satisfying_outcomes_json,status,created_by,created_at) values(?1,?2,?3,?4,?5,?6,?7,?8)",params![&dependency_id,&task_id,&review_task_id,"review",json!(["accepted","accepted_with_notes"]).to_string(),"open",&agent,timestamp]).map_err(db_error)?;
            tx.execute("update task_lifecycle set status=?1,revision=revision+1,updated_at=?2 where task_id=?3",params!["awaiting_dependencies",timestamp,&task_id]).map_err(db_error)?;
            review_dependency = Some(json!({"status":"admitted","dependency_id":dependency_id,"parent_task_id":task_id.clone(),"parent_task_number":number,"required_task_id":review_task_id.clone(),"required_task_number":review_number,"dependency_kind":"review","reviewer":reviewer_id,"outcome_contract":{"contract_id":review_contract_id,"allowed_outcomes":["accepted","accepted_with_notes","rejected"],"satisfying_outcomes":["accepted","accepted_with_notes"]}}));
            review_file = Some((review_task_id, review_number));
        } else if reviewer.is_none() && task_outcome.is_some() {
            tx.execute("update task_lifecycle set status=?1,closed_at=?2,closed_by=?3,closure_mode=?4,revision=revision+1,updated_at=?2 where task_id=?5",params!["closed",timestamp,&agent,"agent_finish",&task_id]).map_err(db_error)?;
        } else {
            tx.execute("update task_lifecycle set status=?1,revision=revision+1,updated_at=?2 where task_id=?3",params!["in_review",timestamp,&task_id]).map_err(db_error)?;
        }
        tx.execute("insert into task_lifecycle_events(event_id,task_id,task_number,event_type,payload_json,created_at) values(?1,?2,?3,?4,?5,?6)",params![format!("task-event-{}",Uuid::new_v4()),&task_id,number,"task.report.submitted",report_json.to_string(),timestamp]).map_err(db_error)?;
        let result = if let Some(dependency) = review_dependency.as_ref() {
            json!({"status":"success","completion_mode":"report","task_number":number,"task_id":task_id,"report_id":report_id,"review_required":true,"new_status":"awaiting_dependencies","close_action":"submitted_for_review","review_action":"dependency_requested","blocked_by":"dependencies","review_dependency":dependency,"report":report_json,"outcome_contract":contract_json,"task_outcome":task_outcome,"outcome_admission":if task_outcome.is_some(){"created"}else{"not_recorded"},"evidence_state":{"admission_state":"not_recorded"}})
        } else if task_outcome.is_some() {
            json!({"status":"success","completion_mode":"report","task_number":number,"task_id":task_id,"report_id":report_id,"review_required":false,"new_status":"closed","close_action":"closed","report":report_json,"outcome_contract":contract_json,"task_outcome":task_outcome,"outcome_admission":"created","evidence_state":{"admission_state":"not_recorded"}})
        } else {
            json!({"status":"submitted","completion_mode":"report","task_number":number,"task_id":task_id,"report_id":report_id,"review_required":true,"new_status":"in_review","close_action":"submitted_for_review","review_action":"reviewer_required","blocked_by":"review","report":report_json,"outcome_contract":contract_json,"task_outcome":Value::Null,"outcome_admission":"not_recorded","evidence_state":{"admission_state":"not_recorded"},"remediation":"Provide an admitted distinct reviewer or an explicit outcome contract outcome before closing this task."})
        };
        tx.execute("insert into native_task_operations(operation_key,operation_kind,request_digest,result_json,created_at) values(?1,'task_finish',?2,?3,?4)",params![&operation_key,digest(&args),result.to_string(),timestamp]).map_err(db_error)?;
        tx.commit().map_err(db_error)?;
        if let Some((review_task_id, review_number)) = review_file {
            write_task_file(
                &self.options.site_root,
                &review_task_id,
                review_number,
                &format!("Review task #{number}"),
                &format!("Review the submitted work for task #{number}."),
                "Admit an accepted or rejected review outcome.",
                "Do not mutate the reviewed work.",
                &json!(["A structured review outcome is admitted."]),
                &json!([]),
                reviewer.as_deref(),
                &format!("review:{task_id}:{review_task_id}"),
            )?;
        }
        Ok(result)
    }
    fn task_dependency_satisfaction(&self, parent_task_id: &str) -> Result<Value, String> {
        let evaluated_at = now();
        let rows = self.query_objects("select dependency_id,parent_task_id,required_task_id,kind,satisfying_outcomes_json from task_dependencies where parent_task_id=?1 order by created_at", params![parent_task_id])?;
        let connection = self.connection()?;
        let parse_list = |value: Option<&str>| -> Vec<String> {
            value.and_then(|text| serde_json::from_str::<Value>(text).ok()).and_then(|parsed| parsed.as_array().cloned()).unwrap_or_default().into_iter().filter_map(|item| item.as_str().map(ToString::to_string)).collect()
        };
        let mut dependencies = Vec::new();
        for row in rows {
            let dependency_id = row.get("dependency_id").and_then(Value::as_str).unwrap_or_default().to_string();
            let parent_id = row.get("parent_task_id").and_then(Value::as_str).unwrap_or_default().to_string();
            let required_id = row.get("required_task_id").and_then(Value::as_str).unwrap_or_default().to_string();
            let kind = row.get("kind").and_then(Value::as_str).unwrap_or_default().to_string();
            let satisfying = parse_list(row.get("satisfying_outcomes_json").and_then(Value::as_str));
            let latest: Option<(String,String)> = connection.query_row("select outcome_id,outcome from task_outcomes where task_id=?1 order by admitted_at desc limit 1", params![&required_id], |r| Ok((r.get(0)?,r.get(1)?))).optional().map_err(db_error)?;
            let latest_outcome_id = latest.as_ref().map(|value| value.0.clone());
            let latest_outcome = latest.as_ref().map(|value| value.1.clone());
            let blocking_json: Option<String> = connection.query_row("select blocking_outcomes_json from task_outcome_contracts where task_id=?1 order by created_at desc limit 1", params![&required_id], |r| r.get(0)).optional().map_err(db_error)?;
            let blocking = parse_list(blocking_json.as_deref());
            let state = match latest_outcome.as_deref() {
                None => "missing_outcome",
                Some(value) if satisfying.iter().any(|item| item == value) => "satisfied",
                Some(value) if blocking.iter().any(|item| item == value) => "blocking_outcome",
                Some(_) => "unsatisfying_outcome",
            };
            let disposition = if let Some(outcome_id) = latest_outcome_id.as_deref() {
                connection.query_row("select disposition_id,kind,status,target_task_id,routed_obligation_id,summary,created_by,created_at from task_dependency_dispositions where dependency_id=?1 and required_outcome_id=?2 order by created_at desc limit 1", params![&dependency_id,outcome_id], |r| Ok(json!({"disposition_id":r.get::<_,String>(0)?,"kind":r.get::<_,String>(1)?,"status":r.get::<_,String>(2)?,"target_task_id":r.get::<_,Option<String>>(3)?,"routed_obligation_id":r.get::<_,Option<String>>(4)?,"summary":r.get::<_,String>(5)?,"created_by":r.get::<_,String>(6)?,"created_at":r.get::<_,String>(7)?}))).optional().map_err(db_error)?
            } else { None };
            let disposition_accepted = disposition.as_ref().map(|item| {
                let disposition_kind = item.get("kind").and_then(Value::as_str).unwrap_or_default();
                let disposition_status = item.get("status").and_then(Value::as_str).unwrap_or_default();
                disposition_status != "superseded" && match disposition_kind {
                    "operator_deferred" | "out_of_scope_or_rejected" => matches!(disposition_status,"deferred" | "resolved"),
                    _ => matches!(disposition_status,"open" | "resolved"),
                }
            }).unwrap_or(false);
            let outcome_satisfied = latest_outcome.as_ref().is_some_and(|value| satisfying.iter().any(|item| item == value));
            let satisfied = outcome_satisfied || (state == "blocking_outcome" && disposition_accepted);
            let blocking_reason = if satisfied {Value::Null} else if state == "missing_outcome" {json!(format!("dependency {dependency_id} has no admitted outcome"))} else if state == "blocking_outcome" {json!(format!("latest outcome {} blocks dependency {dependency_id} and requires explicit disposition",latest_outcome.as_deref().unwrap_or("unknown")))} else {json!(format!("latest outcome {} does not satisfy dependency {dependency_id}",latest_outcome.as_deref().unwrap_or("unknown")))};
            dependencies.push(json!({"dependency_id":dependency_id,"parent_task_id":parent_id,"required_task_id":required_id,"required_outcome_id":latest_outcome_id,"dependency_kind":kind,"satisfying_outcomes":satisfying,"blocking_outcomes":blocking,"latest_outcome":latest_outcome,"satisfied":satisfied,"state":state,"disposition_required":state == "blocking_outcome" && !disposition_accepted,"latest_disposition":disposition,"conflict_policy_evidence":Value::Null,"blocking_reason":blocking_reason,"remediation_options":json!([]),"evaluated_at":evaluated_at.clone()}));
        }
        let satisfied_count = dependencies.iter().filter(|item| item.get("satisfied").and_then(Value::as_bool) == Some(true)).count();
        let dependency_count = dependencies.len();
        Ok(json!({"schema":"narada.task.dependency_satisfaction.v0","parent_task_id":parent_task_id,"evaluated_at":evaluated_at,"dependency_count":dependency_count,"satisfied_count":satisfied_count,"unsatisfied_count":dependency_count.saturating_sub(satisfied_count),"all_satisfied":satisfied_count == dependency_count,"dependencies":dependencies}))
    }

    fn task_closeout(&mut self, args: Value) -> Result<Value, String> {
        let number = required_i64(&args, "task_number")?;
        let agent = required_string(&args, "agent_id")?;
        let site_root = self.options.site_root.clone();
        let summary = string_arg(&args, "summary");
        let disposition_only = args.get("disposition").is_some()
            && args.get("finish").and_then(Value::as_bool) != Some(true);
        if args.get("dry_run").and_then(Value::as_bool) == Some(true) {
            return Ok(
                json!({"status":"planned","task_number":number,"agent_id":agent,"notes_written":false,"changed_files":args.get("changed_files").cloned().unwrap_or_else(||json!([]))}),
            );
        }
        let (task_id, status) = {
            let connection = self.connection()?;
            connection
                .query_row(
                    "select task_id,status from task_lifecycle where task_number=?1",
                    params![number],
                    |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
                )
                .optional()
                .map_err(db_error)?
                .ok_or_else(|| format!("task_not_found: {number}"))?
        };
        let wants_close = args.get("mode").and_then(Value::as_str).is_some();
        let admission_count: i64 = {
            let connection = self.connection()?;
            connection.query_row("select count(*) from evidence_admission_results where task_id=?1 and verdict='admitted'",params![&task_id],|r|r.get(0)).map_err(db_error)?
        };
        if wants_close && admission_count == 0 {
            return Ok(
                json!({"status":"blocked","task_number":number,"new_status":status,"close_action":"blocked","close_blockers":["evidence_admission_required"],"evidence_preflight":{"status":"blocked","next_action":"task_lifecycle_admit_evidence"}}),
            );
        }
        if wants_close {
            let dependency_satisfaction = self.task_dependency_satisfaction(&task_id)?;
            if dependency_satisfaction.get("all_satisfied").and_then(Value::as_bool) == Some(false) {
                return Ok(json!({
                    "status":"blocked",
                    "error":"task_close_dependencies_unsatisfied",
                    "close_action":"blocked",
                    "close_blocked":true,
                    "close_blockers":dependency_satisfaction.get("dependencies").cloned().unwrap_or_else(||json!([])),
                    "task_number":number,
                    "task_id":task_id,
                    "schema":"narada.task.mcp.close.dependency_satisfaction_gate.v0",
                    "dependency_satisfaction":dependency_satisfaction,
                    "remediation":"Complete each required dependency task with an admitted satisfying outcome before closing the parent task.",
                    "next_action":"Complete each required dependency task with an admitted satisfying outcome before closing the parent task."
                }));
            }
        }
        if let Some(text) = summary.as_deref() {
            append_task_body(&site_root, number, text)?;
        }
        if disposition_only {
            return Ok(json!({
                "status":"prepared",
                "schema":"narada.task.mcp.disposition_closeout.v0",
                "task_number":number,
                "task_id":task_id,
                "notes_written":summary.is_some(),
                "changed_files":args.get("changed_files").cloned().unwrap_or_else(||json!([])),
                "disposition":args.get("disposition"),
                "finish_result":Value::Null,
            }));
        }
        let new_status = if wants_close { "closed" } else { "in_review" };
        let mode = args
            .get("mode")
            .and_then(Value::as_str)
            .unwrap_or("prepared");
        let timestamp = now();
        let connection = self.connection_mut()?;
        connection.execute("update task_lifecycle set status=?1,closed_at=case when ?1='closed' then ?2 else closed_at end,closed_by=case when ?1='closed' then ?3 else closed_by end,closure_mode=case when ?1='closed' then ?4 else closure_mode end,updated_at=?2 where task_id=?5",params![new_status,timestamp,&agent,mode,&task_id]).map_err(db_error)?;
        connection.execute("insert into task_lifecycle_events(event_id,task_id,task_number,event_type,payload_json,created_at) values(?1,?2,?3,?4,?5,?6)",params![format!("task-event-{}",Uuid::new_v4()),&task_id,number,if new_status=="closed"{"task.closed"}else{"task.closeout.prepared"},json!({"agent_id":agent,"summary":summary,"mode":mode,"previous_status":status}).to_string(),timestamp]).map_err(db_error)?;
        Ok(
            json!({"status":if new_status=="closed"{"success"}else{"prepared"},"new_status":new_status,"task_number":number,"task_id":task_id,"notes_written":summary.is_some(),"changed_files":args.get("changed_files").cloned().unwrap_or_else(||json!([])),"closure_mode":if new_status=="closed"{json!(mode)}else{Value::Null},"close_action":if new_status=="closed"{"closed"}else{"prepared"}}),
        )
    }

    fn task_review(&mut self, args: Value) -> Result<Value, String> {
        let number = required_i64(&args, "task_number")?;
        let agent = required_string(&args, "agent_id")?;
        let verdict = required_string(&args, "verdict")?;
        let (task_id, status) = {
            let c = self.connection()?;
            c.query_row(
                "select task_id,status from task_lifecycle where task_number=?1",
                params![number],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
            )
            .optional()
            .map_err(db_error)?
            .ok_or_else(|| format!("task_not_found:{number}"))?
        };
        let review_id = format!("review-{}", Uuid::new_v4());
        let timestamp = now();
        let findings = args.get("findings").cloned().unwrap_or_else(|| json!([]));
        let c = self.connection_mut()?;
        c.execute("insert into task_reviews(review_id,task_id,reviewer_agent_id,verdict,findings_json,reviewed_at) values(?1,?2,?3,?4,?5,?6)",params![&review_id,&task_id,&agent,&verdict,findings.to_string(),timestamp]).map_err(db_error)?;
        c.execute("insert into task_lifecycle_events(event_id,task_id,task_number,event_type,payload_json,created_at) values(?1,?2,?3,'task.review.recorded',?4,?5)",params![format!("task-event-{}",Uuid::new_v4()),&task_id,number,json!({"review_id":review_id,"verdict":verdict,"findings":findings}).to_string(),timestamp]).map_err(db_error)?;
        Ok(
            json!({"schema":"narada.task.review.v1","status":"recorded","review_id":review_id,"task_number":number,"task_id":task_id,"verdict":verdict,"findings":findings,"previous_status":status,"completion_mode":"review"}),
        )
    }

    fn task_evidence_supersede(&mut self, args: Value) -> Result<Value, String> {
        let number = required_i64(&args, "task_number")?;
        let agent = required_string(&args, "agent_id")?;
        let supersedes = required_string(&args, "supersedes_report_id")?;
        let artifact = required_string(&args, "artifact_uri")?;
        let summary = required_string(&args, "summary")?;
        let verification = required_string(&args, "verification_summary")?;
        let (task_id, exists) = {
            let c = self.connection()?;
            let task_id: String = c
                .query_row(
                    "select task_id from task_lifecycle where task_number=?1",
                    params![number],
                    |r| r.get(0),
                )
                .optional()
                .map_err(db_error)?
                .ok_or_else(|| format!("task_not_found:{number}"))?;
            let exists: bool = c
                .query_row(
                    "select count(*) from task_reports where report_id=?1 and task_id=?2",
                    params![supersedes, &task_id],
                    |r| r.get::<_, i64>(0),
                )
                .map_err(db_error)?
                > 0;
            (task_id, exists)
        };
        if !exists {
            return Err(format!("report_not_found:{supersedes}"));
        }
        let id = format!("artifact-{}", Uuid::new_v4());
        let admitted = json!({"artifact_uri":artifact,"supersedes_report_id":supersedes,"summary":summary,"verification_summary":verification});
        let timestamp = now();
        let c = self.connection_mut()?;
        c.execute("insert into observation_artifacts(artifact_id,artifact_type,source_operator,task_id,task_number,agent_id,artifact_uri,digest,admitted_view_json,created_at) values(?1,'evidence_supersession',?2,?3,?4,?5,?6,?7,?8,?9)",params![&id,&agent,&task_id,number,&agent,&artifact,digest(&admitted),admitted.to_string(),timestamp]).map_err(db_error)?;
        c.execute("insert into task_lifecycle_events(event_id,task_id,task_number,event_type,payload_json,created_at) values(?1,?2,?3,'task.evidence.superseded',?4,?5)",params![format!("task-event-{}",Uuid::new_v4()),&task_id,number,admitted.to_string(),timestamp]).map_err(db_error)?;
        Ok(
            json!({"schema":"narada.task.evidence_supersede.v1","status":"admitted","artifact_id":id,"task_number":number,"supersedes_report_id":supersedes,"artifact_uri":artifact}),
        )
    }

    fn task_compatibility_reconcile(&mut self, args: Value) -> Result<Value, String> {
        let agent = required_string(&args, "agent_id")?;
        let dry_run = args.get("dry_run").and_then(Value::as_bool) == Some(true);
        let limit = args
            .get("limit")
            .and_then(Value::as_i64)
            .unwrap_or(25)
            .clamp(1, 100);
        let numbers = args
            .get("task_numbers")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let candidates = if numbers.is_empty() {
            self.query_objects("select task_number,status from task_lifecycle where status in ('in_review','closed','confirmed') order by task_number desc limit ?1",params![limit])?
        } else {
            numbers
                .into_iter()
                .filter_map(|value| value.as_i64())
                .map(|n| json!({"task_number":n}))
                .collect::<Vec<_>>()
        };
        if !dry_run {
            return Ok(
                json!({"schema":"narada.task.compatibility_reconcile.v1","status":"refused","code":"native_compatibility_reconcile_requires_explicit_repair_policy","agent_id":agent,"dry_run":false,"scanned":candidates.len(),"repaired":0,"candidates":candidates}),
            );
        }
        Ok(
            json!({"schema":"narada.task.compatibility_reconcile.v1","status":"planned","agent_id":agent,"dry_run":true,"scanned":candidates.len(),"repaired":0,"candidates":candidates}),
        )
    }
    fn task_tags_update(&mut self, args: Value) -> Result<Value, String> {
        let number = required_i64(&args, "task_number")?;
        let agent = required_string(&args, "agent_id")?;
        let reason = required_string(&args, "reason")?;
        let tags = args.get("tags").cloned().unwrap_or_else(|| json!([]));
        let connection = self.connection_mut()?;
        let (task_id, previous): (String, String) = connection
            .query_row(
                "select task_id, tags_json from task_specs where task_number=?1",
                params![number],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(db_error)?
            .ok_or_else(|| format!("task_not_found: {number}"))?;
        connection
            .execute(
                "update task_specs set tags_json=?1, updated_at=?2 where task_id=?3",
                params![tags.to_string(), now(), task_id],
            )
            .map_err(db_error)?;
        connection.execute("insert into task_tag_updates(update_id,task_id,task_number,actor_agent_id,previous_tags_json,new_tags_json,reason,updated_at) values(?1,?2,?3,?4,?5,?6,?7,?8)", params![format!("tag-update-{}", Uuid::new_v4()), task_id, number, agent, previous, tags.to_string(), reason, now()]).map_err(db_error)?;
        Ok(json!({"status":"updated","task_number":number,"tags":tags}))
    }

    fn task_report_blocked(&mut self, args: Value) -> Result<Value, String> {
        let number = required_i64(&args, "task_number")?;
        let agent = required_string(&args, "agent_id")?;
        let reason = required_string(&args, "reason")?;
        let connection = self.connection_mut()?;
        let task_id: String = connection
            .query_row(
                "select task_id from task_lifecycle where task_number=?1",
                params![number],
                |r| r.get(0),
            )
            .optional()
            .map_err(db_error)?
            .ok_or_else(|| format!("task_not_found: {number}"))?;
        let report_id = format!("report-{}", Uuid::new_v4());
        connection.execute("insert into task_reports(report_id,task_id,agent_id,agent_identity_ref_json,summary,changed_files_json,verification_json,submitted_at) values(?1,?2,?3,null,?4,'[]','{}',?5)", params![report_id, task_id, agent, reason, now()]).map_err(db_error)?;
        if args.get("defer").and_then(Value::as_bool) != Some(false) {
            connection
                .execute(
                    "update task_lifecycle set status='deferred',updated_at=?1 where task_id=?2",
                    params![now(), task_id],
                )
                .map_err(db_error)?;
        }
        Ok(
            json!({"status":"blocked","task_number":number,"report_id":report_id,"deferred":args.get("defer").and_then(Value::as_bool)!=Some(false)}),
        )
    }

    fn task_set_routing(&mut self, args: Value) -> Result<Value, String> {
        let number = required_i64(&args, "task_number")?;
        let actor = string_arg(&args, "actor_agent_id")
            .or_else(|| string_arg(&args, "agent_id"))
            .unwrap_or_else(|| "native".to_string());
        let preferred_role = string_arg(&args, "preferred_role");
        let target_role = string_arg(&args, "target_role");
        let preferred_agent_id = string_arg(&args, "preferred_agent_id");
        let reason = string_arg(&args, "reason").unwrap_or_else(|| "routing_updated".to_string());
        let timestamp = now();
        let connection = self.connection_mut()?;
        let task_id: String = connection
            .query_row(
                "select task_id from task_lifecycle where task_number=?1",
                params![number],
                |r| r.get(0),
            )
            .optional()
            .map_err(db_error)?
            .ok_or_else(|| format!("task_not_found: {number}"))?;
        let previous = connection
            .query_row(
                "select preferred_role,target_role,preferred_agent_id,updated_at from narada_andrey_task_role_preferences where task_id=?1",
                params![&task_id],
                |r| Ok(json!({"preferred_role":r.get::<_,Option<String>>(0)?,"target_role":r.get::<_,Option<String>>(1)?,"preferred_agent_id":r.get::<_,Option<String>>(2)?,"updated_at":r.get::<_,String>(3)?})),
            )
            .optional()
            .map_err(db_error)?
            .unwrap_or_else(|| json!({}));
        let routing = json!({"preferred_role":preferred_role,"target_role":target_role,"preferred_agent_id":preferred_agent_id,"updated_at":timestamp});
        let actor_role: Option<String> = connection
            .query_row("select role from agent_roster where agent_id=?1", params![&actor], |r| r.get(0))
            .optional()
            .map_err(db_error)?;
        connection.execute("insert into narada_andrey_task_role_preferences(task_id,preferred_role,target_role,preferred_agent_id,updated_at) values(?1,?2,?3,?4,?5) on conflict(task_id) do update set preferred_role=excluded.preferred_role,target_role=excluded.target_role,preferred_agent_id=excluded.preferred_agent_id,updated_at=excluded.updated_at", params![&task_id, preferred_role, target_role, preferred_agent_id, &timestamp]).map_err(db_error)?;
        connection.execute("insert into task_routing_events(event_id,task_id,task_number,actor_agent_id,actor_role,reason,changed_fields_json,previous_routing_json,new_routing_json,created_at) values(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)", params![format!("routing-event-{}",Uuid::new_v4()),&task_id,number,&actor,actor_role,&reason,args.get("changed_fields").cloned().unwrap_or_else(||json!(["preferred_role","target_role","preferred_agent_id"])).to_string(),previous.to_string(),routing.to_string(),&timestamp]).map_err(db_error)?;
        Ok(json!({"status":"updated","task_number":number,"actor_agent_id":actor,"routing":routing,"previous_routing":previous,"reason":reason}))
    }

    fn task_dependency_declare(&mut self, args: Value) -> Result<Value, String> {
        let parent_number = required_i64(&args, "parent_task_number")?;
        let required_number = required_i64(&args, "required_task_number")?;
        let agent = required_string(&args, "agent_id")?;
        let kind = required_string(&args, "kind")?;
        let connection = self.connection_mut()?;
        let parent: String = connection
            .query_row(
                "select task_id from task_lifecycle where task_number=?1",
                params![parent_number],
                |r| r.get(0),
            )
            .optional()
            .map_err(db_error)?
            .ok_or_else(|| format!("task_not_found: {parent_number}"))?;
        let required: String = connection
            .query_row(
                "select task_id from task_lifecycle where task_number=?1",
                params![required_number],
                |r| r.get(0),
            )
            .optional()
            .map_err(db_error)?
            .ok_or_else(|| format!("task_not_found: {required_number}"))?;
        let dependency_id = string_arg(&args, "dependency_id")
            .unwrap_or_else(|| format!("dependency-{}", Uuid::new_v4()));
        connection.execute("insert or ignore into task_dependencies(dependency_id,parent_task_id,required_task_id,kind,satisfying_outcomes_json,status,created_by,created_at) values(?1,?2,?3,?4,?5,'open',?6,?7)", params![dependency_id, parent, required, kind, args.get("satisfying_outcomes").cloned().unwrap_or_else(||json!([])).to_string(), agent, now()]).map_err(db_error)?;
        Ok(
            json!({"status":"created","dependency_id":dependency_id,"parent_task_number":parent_number,"required_task_number":required_number}),
        )
    }
    fn roster_list(&self) -> Result<Value, String> {
        let connection = self.connection()?;
        let mut stmt = connection
            .prepare("select * from agent_roster order by agent_id")
            .map_err(db_error)?;
        let rows = stmt
            .query_map([], |r| row_to_object(r))
            .map_err(db_error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(db_error)?;
        Ok(json!({"status":"ok","roster":rows}))
    }
    fn roster_admit(&mut self, args: Value) -> Result<Value, String> {
        let connection = self.connection_mut()?;
        let agent = required_string(&args, "agent_id")?;
        let role = string_arg(&args, "role").unwrap_or_else(|| "engineer".to_string());
        let capabilities = args.get("capabilities").cloned().unwrap_or_else(||json!([]));
        let requested_by = string_arg(&args, "requested_by").or_else(|| string_arg(&args, "actor_agent_id")).unwrap_or_else(|| "native".to_string());
        let authority_basis = args.get("authority_basis").cloned().unwrap_or_else(||json!({}));
        let reason = string_arg(&args, "reason").unwrap_or_else(|| "roster_admitted".to_string());
        let n = now();
        let tx = connection.transaction().map_err(db_error)?;
        tx.execute("insert into agent_roster(agent_id,role,capabilities_json,operator_identity,first_seen_at,last_active_at,status,task_number,last_done,updated_at) values(?1,?2,?3,null,?4,?4,'idle',null,null,?4) on conflict(agent_id) do update set role=excluded.role,capabilities_json=excluded.capabilities_json,last_active_at=excluded.last_active_at,updated_at=excluded.updated_at",params![&agent,&role,capabilities.to_string(),&n]).map_err(db_error)?;
        tx.execute("insert into agent_roster_events(event_id,event_type,agent_id,role,capabilities_json,operator_identity,requested_by,requested_at,authority_basis_json,admission_status,admitted_by,admitted_at,reason,payload_json,supersedes_event_id) values(?1,'admit',?2,?3,?4,null,?5,?6,?7,'admitted',?5,?6,?8,?9,null)",params![format!("roster-event-{}",Uuid::new_v4()),&agent,&role,capabilities.to_string(),&requested_by,&n,authority_basis.to_string(),&reason,args.to_string()]).map_err(db_error)?;
        tx.commit().map_err(db_error)?;
        Ok(json!({"status":"admitted","agent_id":agent,"role":role,"capabilities":capabilities,"requested_by":requested_by,"reason":reason}))
    }

    fn payload_derive(&mut self, args: Value) -> Result<Value, String> {
        let source_ref = required_string(&args, "source_ref")?;
        let (payload_id, source_revision) = parse_payload_reference(&source_ref)?;
        let source = self.payload_read("mcp_payload_show", json!({"ref": source_ref}))?;
        let mut payload = source
            .get("payload")
            .cloned()
            .ok_or("payload_ref_payload_must_be_object")?;
        let delete_paths_value = args.get("delete_paths");
        let has_overlay = args.get("overlay").is_some() || args.get("overlay_json").is_some();
        let has_delete_paths = delete_paths_value.is_some();
        if !has_overlay && !has_delete_paths {
            return Err("payload_derive_requires_overlay_or_delete_paths".to_string());
        }
        let overlay = if has_overlay {
            payload_object_from_args(&args, "overlay", "overlay_json")?
        } else {
            json!({})
        };
        merge_json_objects(&mut payload, &overlay)?;
        let mut delete_paths = Vec::new();
        if let Some(value) = delete_paths_value {
            let values = value
                .as_array()
                .ok_or("payload_derive_delete_paths_must_be_non_empty_string_array")?;
            if values.is_empty() {
                return Err(
                    "payload_derive_delete_paths_must_be_non_empty_string_array".to_string()
                );
            }
            for path in values {
                let path = path
                    .as_str()
                    .ok_or("payload_derive_delete_paths_must_be_non_empty_string_array")?;
                if delete_paths.iter().any(|existing| existing == path) {
                    return Err("payload_derive_delete_paths_must_be_unique".to_string());
                }
                delete_json_pointer(&mut payload, path)?;
                delete_paths.push(path.to_string());
            }
        }
        let revision = source_revision + 1;
        let reference = format!("mcp_payload:{payload_id}@v{revision}");
        let byte_size = payload_byte_size(&payload);
        let max_bytes = 256 * 1024usize;
        if byte_size > max_bytes {
            return Err(format!("payload_too_large: {byte_size} > {max_bytes}"));
        }
        let record = json!({
            "schema": "narada.mcp_payload.revision.v1",
            "ref": reference,
            "payload_id": payload_id,
            "revision": revision,
            "created_at": now(),
            "created_by": string_arg(&args, "created_by"),
            "source": {
                "kind": "derive",
                "source_ref": source_ref,
                "overlay_sha256": digest(&overlay),
                "delete_paths": delete_paths
            },
            "sha256": digest(&payload),
            "byte_size": byte_size,
            "max_bytes": max_bytes,
            "transient_not_authority": true,
            "immutable_revision": true,
            "payload": payload
        });
        let path = payload_revision_path(&self.options.site_root, &payload_id, revision);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| format!("payload_directory_create_failed:{e}"))?;
        }
        let serialized = format!("{}\n", payload_stable_json(&record));
        let status = match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(mut file) => {
                file.write_all(serialized.as_bytes())
                    .map_err(|e| format!("payload_write_failed:{e}"))?;
                "derived"
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                let existing: Value = serde_json::from_str(
                    &fs::read_to_string(&path)
                        .map_err(|e| format!("payload_revision_conflict:{e}"))?,
                )
                .map_err(|e| format!("payload_revision_conflict:{e}"))?;
                if existing.get("ref") == record.get("ref")
                    && existing.get("sha256") == record.get("sha256")
                    && existing.get("byte_size") == record.get("byte_size")
                {
                    "existing"
                } else {
                    return Err(format!("payload_revision_conflict: immutable revision already contains different content: {reference}"));
                }
            }
            Err(error) => return Err(format!("payload_write_failed:{error}")),
        };
        Ok(json!({
            "status": status,
            "ref": reference,
            "payload_id": payload_id,
            "revision": revision,
            "source_ref": source_ref,
            "byte_size": byte_size,
            "sha256": record.get("sha256").cloned().unwrap_or(Value::Null),
            "created_at": record.get("created_at").cloned().unwrap_or(Value::Null),
            "created_by": record.get("created_by").cloned().unwrap_or(Value::Null),
            "transient_not_authority": true,
            "immutable_revision": true
        }))
    }
    fn output_show(&self, args: Value) -> Result<Value, String> {
        let reference = string_arg(&args, "ref")
            .or_else(|| string_arg(&args, "output_ref"))
            .ok_or("output_ref_required")?;
        let id = safe_reference_id(&reference, "mcp_output:")?;
        let candidates = [
            self.options
                .site_root
                .join(".ai")
                .join("mcp-outputs")
                .join(format!("{id}.txt")),
            self.options
                .site_root
                .join(".ai")
                .join("outputs")
                .join(format!("{id}.txt")),
            self.options
                .site_root
                .join(".ai")
                .join("mcp-outputs")
                .join(format!("{id}.json")),
        ];
        let path = candidates
            .iter()
            .find(|p| p.exists())
            .ok_or_else(|| format!("output_not_found:{reference}"))?;
        let text = fs::read_to_string(path).map_err(|e| format!("output_read_failed:{e}"))?;
        let chars: Vec<char> = text.chars().take(4_000_000).collect();
        let offset = args.get("offset").and_then(Value::as_u64).unwrap_or(0) as usize;
        let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(4_000) as usize;
        let offset = offset.min(chars.len());
        let end = (offset.saturating_add(limit)).min(chars.len());
        let output: String = chars[offset..end].iter().collect();
        Ok(
            json!({"schema":"narada.producer_output_page.v1","status":"ok","ref":reference,"output_ref":reference,"offset":offset,"limit":limit,"next_offset":if end<chars.len(){json!(end)}else{Value::Null},"output_text":output,"output_truncated":end<chars.len(),"full_output_char_length":chars.len()}),
        )
    }

    fn resources_list(&self, params: &Value) -> Result<Value, String> {
        let (offset, limit) = resource_page(params)?;
        let dir = self
            .options
            .site_root
            .join(".ai")
            .join("tmp")
            .join("mcp-outputs")
            .join("workspace");
        let mut ids = Vec::new();
        if dir.is_dir() {
            for entry in fs::read_dir(&dir)
                .map_err(|e| format!("output_resource_directory_read_failed:{e}"))?
                .take(10_000)
            {
                let entry =
                    entry.map_err(|e| format!("output_resource_directory_read_failed:{e}"))?;
                let path = entry.path();
                if !path.is_file() || path.extension().and_then(|v| v.to_str()) != Some("json") {
                    continue;
                }
                let Some(stem) = path.file_stem().and_then(|v| v.to_str()) else {
                    continue;
                };
                if valid_output_id(stem) {
                    ids.push(stem.to_string());
                }
            }
        }
        ids.sort();
        let start = offset.min(ids.len());
        let end = offset.saturating_add(limit).min(ids.len());
        let resources = ids[start..end]
            .iter()
            .map(|id| {
                let reference = format!("mcp_output:{id}");
                json!({
                    "uri": format!("mcp-output:{}", percent_encode(&reference)),
                    "name": reference,
                    "title": reference,
                    "description": "Materialized MCP output ref.",
                    "mimeType": "application/json"
                })
            })
            .collect::<Vec<_>>();
        let next = if end < ids.len() { Some(end) } else { None };
        Ok(json!({
            "resources": resources,
            "offset": offset,
            "limit": limit,
            "next_offset": next,
            "nextCursor": next.map(|value| value.to_string()),
            "has_more": next.is_some()
        }))
    }

    fn resources_read(&self, params: &Value) -> Result<Value, String> {
        let uri = required_string(params, "uri")?;
        let encoded = uri
            .strip_prefix("mcp-output:")
            .ok_or_else(|| format!("output_resource_uri_invalid: {uri}"))?;
        let reference = percent_decode(encoded)?;
        let id = output_id_from_reference(&reference)?;
        let new_path = self
            .options
            .site_root
            .join(".ai")
            .join("tmp")
            .join("mcp-outputs")
            .join("workspace")
            .join(format!("{id}.json"));
        let legacy_path = self
            .options
            .site_root
            .join(".ai")
            .join("mcp-outputs")
            .join(format!("{id}.json"));
        let path = if new_path.is_file() {
            new_path
        } else {
            legacy_path
        };
        if !path.is_file() {
            return Err(format!("output_ref_not_found: {reference}"));
        }
        let metadata = fs::metadata(&path).map_err(|e| format!("output_ref_stat_failed:{e}"))?;
        if metadata.len() > 10 * 1024 * 1024 {
            return Err(format!(
                "output_ref_too_large: {} > {}",
                metadata.len(),
                10 * 1024 * 1024
            ));
        }
        let text = fs::read_to_string(&path).map_err(|e| format!("output_ref_read_failed:{e}"))?;
        let record: Value =
            serde_json::from_str(&text).map_err(|e| format!("output_ref_invalid_json: {e}"))?;
        let object = record
            .as_object()
            .ok_or_else(|| format!("output_ref_record_must_be_object: {reference}"))?;
        if object.get("schema").and_then(Value::as_str) != Some("narada.mcp_output_ref.v1") {
            return Err(format!(
                "output_ref_schema_unsupported: {}",
                object.get("schema").cloned().unwrap_or(Value::Null)
            ));
        }
        if object.get("ref").and_then(Value::as_str) != Some(reference.as_str())
            || object.get("output_id").and_then(Value::as_str) != Some(id.as_str())
        {
            return Err(format!("output_ref_metadata_mismatch: {reference}"));
        }
        let full_output = object
            .get("full_output")
            .ok_or_else(|| format!("output_ref_full_output_missing: {reference}"))?;
        let output_text = serde_json::to_string_pretty(full_output)
            .map_err(|e| format!("output_ref_presentation_failed: {e}"))?;
        let expected_length = utf16_len(&output_text);
        if object
            .get("full_output_char_length")
            .and_then(Value::as_u64)
            != Some(expected_length as u64)
        {
            return Err(format!("output_ref_length_mismatch: {reference}"));
        }
        if object.get("sha256").and_then(Value::as_str)
            != Some(native_canonical_digest(full_output).as_str())
        {
            return Err(format!("output_ref_sha256_mismatch: {reference}"));
        }
        let limit = 10_000usize;
        let page_end = output_text
            .char_indices()
            .nth(limit)
            .map(|(index, _)| index)
            .unwrap_or(output_text.len());
        let chunk = output_text[..page_end].to_string();
        let output_truncated = page_end < output_text.len();
        let relative_path = path
            .strip_prefix(&self.options.site_root)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        let page = json!({
            "schema": "narada.mcp_output_page.v1",
            "status": "ok",
            "ref": reference,
            "tool_name": object.get("tool_name").cloned().unwrap_or(Value::Null),
            "full_output_char_length": json!(expected_length),
            "byte_size": metadata.len(),
            "original_truncated": object.get("truncated").and_then(Value::as_bool).unwrap_or(false),
            "path": relative_path,
            "offset": 0,
            "limit": limit,
            "next_offset": if output_truncated { json!(page_end) } else { Value::Null },
            "output_limit": limit,
            "output_truncated": output_truncated,
            "output_text": chunk
        });
        Ok(json!({
            "contents": [{
                "uri": uri,
                "mimeType": "application/json",
                "text": serde_json::to_string_pretty(&page).map_err(|e| format!("output_resource_serialize_failed: {e}"))?
            }]
        }))
    }
    fn payload_create(&mut self, args: Value) -> Result<Value, String> {
        let payload = payload_object_from_args(&args, "payload", "payload_json")?;
        if payload
            .as_object()
            .map(|value| value.is_empty())
            .unwrap_or(true)
            && args.get("allow_empty").and_then(Value::as_bool) != Some(true)
        {
            return Err("payload_create_empty_payload_requires_allow_empty".to_string());
        }
        let id = string_arg(&args, "payload_id")
            .unwrap_or_else(|| format!("p_{}", Uuid::new_v4().simple()));
        if !valid_payload_id(&id) {
            return Err(format!("payload_id_invalid: {id}"));
        }
        let byte_size = payload_byte_size(&payload);
        let max_bytes = 256 * 1024usize;
        if byte_size > max_bytes {
            return Err(format!("payload_too_large: {byte_size} > {max_bytes}"));
        }
        let revision = 1i64;
        let reference = format!("mcp_payload:{id}@v{revision}");
        let sha = digest(&payload);
        let record = json!({
            "schema": "narada.mcp_payload.revision.v1",
            "ref": reference,
            "payload_id": id,
            "revision": revision,
            "created_at": now(),
            "created_by": string_arg(&args, "created_by"),
            "source": {"kind": "create"},
            "sha256": sha,
            "byte_size": byte_size,
            "max_bytes": max_bytes,
            "transient_not_authority": true,
            "immutable_revision": true,
            "payload": payload
        });
        let path = payload_revision_path(&self.options.site_root, &id, revision);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| format!("payload_directory_create_failed:{e}"))?;
        }
        let serialized = format!("{}\n", payload_stable_json(&record));
        let status = match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(mut file) => {
                file.write_all(serialized.as_bytes())
                    .map_err(|e| format!("payload_write_failed:{e}"))?;
                "created"
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                let existing: Value = serde_json::from_str(
                    &fs::read_to_string(&path)
                        .map_err(|e| format!("payload_revision_conflict:{e}"))?,
                )
                .map_err(|e| format!("payload_revision_conflict:{e}"))?;
                if existing.get("ref") == record.get("ref")
                    && existing.get("sha256") == record.get("sha256")
                    && existing.get("byte_size") == record.get("byte_size")
                {
                    return Ok(json!({
                        "status": "existing",
                        "ref": reference,
                        "payload_id": id,
                        "revision": revision,
                        "source_ref": Value::Null,
                        "byte_size": byte_size,
                        "sha256": sha,
                        "created_at": existing.get("created_at").cloned().unwrap_or(Value::Null),
                        "created_by": existing.get("created_by").cloned().unwrap_or(Value::Null),
                        "transient_not_authority": true,
                        "immutable_revision": true
                    }));
                }
                return Err(format!("payload_revision_conflict: immutable revision already contains different content: {reference}"));
            }
            Err(error) => return Err(format!("payload_write_failed:{error}")),
        };
        Ok(json!({
            "status": status,
            "ref": reference,
            "payload_id": id,
            "revision": revision,
            "source_ref": Value::Null,
            "byte_size": byte_size,
            "sha256": sha,
            "created_at": record.get("created_at").cloned().unwrap_or(Value::Null),
            "created_by": record.get("created_by").cloned().unwrap_or(Value::Null),
            "transient_not_authority": true,
            "immutable_revision": true
        }))
    }
    fn payload_read(&self, name: &str, args: Value) -> Result<Value, String> {
        let reference = string_arg(&args, "ref")
            .or_else(|| string_arg(&args, "payload_ref"))
            .ok_or("payload_ref_required")?;
        if let Ok((id, revision)) = parse_payload_reference(&reference) {
            let path = payload_revision_path(&self.options.site_root, &id, revision);
            if path.is_file() {
                let metadata =
                    fs::metadata(&path).map_err(|e| format!("payload_ref_stat_failed:{e}"))?;
                let max_bytes = 256 * 1024usize;
                if metadata.len() > max_bytes as u64 {
                    return Err(format!(
                        "payload_ref_too_large: {} > {max_bytes}",
                        metadata.len()
                    ));
                }
                let text = fs::read_to_string(&path)
                    .map_err(|e| format!("payload_ref_read_failed:{e}"))?;
                let record: Value = serde_json::from_str(&text)
                    .map_err(|e| format!("payload_ref_invalid_json: {e}"))?;
                let object = record
                    .as_object()
                    .ok_or_else(|| format!("payload_ref_record_must_be_object: {reference}"))?;
                if object.get("schema").and_then(Value::as_str)
                    != Some("narada.mcp_payload.revision.v1")
                    || object.get("ref").and_then(Value::as_str) != Some(reference.as_str())
                    || object.get("payload_id").and_then(Value::as_str) != Some(id.as_str())
                    || object.get("revision").and_then(Value::as_i64) != Some(revision)
                {
                    return Err(format!("payload_ref_metadata_mismatch: {reference}"));
                }
                let payload = object
                    .get("payload")
                    .cloned()
                    .ok_or_else(|| format!("payload_ref_payload_must_be_object: {reference}"))?;
                if !payload.is_object() {
                    return Err(format!("payload_ref_payload_must_be_object: {reference}"));
                }
                let byte_size = payload_byte_size(&payload);
                if object.get("byte_size").and_then(Value::as_u64) != Some(byte_size as u64) {
                    return Err(format!("payload_ref_byte_size_mismatch: {reference}"));
                }
                if object.get("sha256").and_then(Value::as_str) != Some(digest(&payload).as_str()) {
                    return Err(format!("payload_ref_sha256_mismatch: {reference}"));
                }
                let mut result = json!({
                    "status": if name == "mcp_payload_validate" { "valid" } else { "ok" },
                    "ref": reference,
                    "payload_id": id,
                    "revision": revision,
                    "source_ref": object.get("source").and_then(|source| source.get("source_ref")).cloned().unwrap_or(Value::Null),
                    "byte_size": byte_size,
                    "sha256": object.get("sha256").cloned().unwrap_or(Value::Null),
                    "created_at": object.get("created_at").cloned().unwrap_or(Value::Null),
                    "created_by": object.get("created_by").cloned().unwrap_or(Value::Null),
                    "transient_not_authority": true,
                    "immutable_revision": true
                });
                if name == "mcp_payload_show" {
                    result["payload"] = payload;
                }
                return Ok(result);
            }
        }
        let id = safe_reference_id(&reference, "mcp_payload:")?;
        let path = self
            .options
            .site_root
            .join(".ai")
            .join("mcp-payloads")
            .join(format!("{id}.json"));
        let text =
            fs::read_to_string(&path).map_err(|_| format!("payload_not_found:{reference}"))?;
        let payload: Value =
            serde_json::from_str(&text).map_err(|e| format!("payload_invalid:{e}"))?;
        let mut result = json!({
            "status": if name == "mcp_payload_validate" { "valid" } else { "ok" },
            "ref": reference,
            "payload_id": id,
            "revision": Value::Null,
            "source_ref": Value::Null,
            "byte_size": payload_byte_size(&payload),
            "sha256": digest(&payload),
            "transient_not_authority": true,
            "immutable_revision": false
        });
        if name == "mcp_payload_show" {
            result["payload"] = payload;
        }
        Ok(result)
    }
    fn ticket_list(&self, args: Value) -> Result<Value, String> {
        let connection = self.connection()?;
        let limit = args
            .get("limit")
            .and_then(Value::as_i64)
            .unwrap_or(100)
            .clamp(1, 500);
        let mut stmt = connection
            .prepare("select * from tickets order by ticket_number desc limit ?1")
            .map_err(db_error)?;
        let rows = stmt
            .query_map(params![limit], |r| ticket_row(r))
            .map_err(db_error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(db_error)?;
        Ok(
            json!({"schema":"narada.work_lifecycle.ticket_list.v1","count":rows.len(),"tickets":rows}),
        )
    }
    fn ticket_show(&self, args: Value) -> Result<Value, String> {
        let connection = self.connection()?;
        let ticket = if let Some(id) = string_arg(&args, "ticket_id") {
            connection
                .query_row(
                    "select * from tickets where ticket_id=?1",
                    params![id],
                    |r| ticket_row(r),
                )
                .optional()
                .map_err(db_error)?
        } else if let Some(n) = args.get("ticket_number").and_then(Value::as_i64) {
            connection
                .query_row(
                    "select * from tickets where ticket_number=?1",
                    params![n],
                    |r| ticket_row(r),
                )
                .optional()
                .map_err(db_error)?
        } else {
            return Err("ticket_identity_required".to_string());
        };
        let ticket = ticket.ok_or("ticket_not_found")?;
        let id = ticket
            .get("ticket_id")
            .and_then(Value::as_str)
            .unwrap_or("");
        let sources = self.query_objects(
            "select * from ticket_sources where ticket_id=?1",
            params![id],
        )?;
        let links = self.query_objects(
            "select * from ticket_task_links where ticket_id=?1",
            params![id],
        )?;
        Ok(
            json!({"schema":"narada.work_lifecycle.ticket.v1","ticket":ticket,"sources":sources,"task_links":links,"draft_refs":[]}),
        )
    }
    fn ticket_sources(&self, args: Value) -> Result<Value, String> {
        let id = required_string(&args, "ticket_id")?;
        Ok(
            json!({"schema":"narada.work_lifecycle.ticket_sources.v1","ticket_id":id,"sources":self.query_objects("select * from ticket_sources where ticket_id=?1",params![id])?}),
        )
    }
    fn ticket_admit_source(&mut self, args: Value) -> Result<Value, String> {
        let connection = self.connection_mut()?;
        let idem = required_string(&args, "idempotency_key")?;
        if let Some(result) = connection
            .query_row(
                "select result_json from work_operations where operation_key=?1",
                params![idem],
                |r| r.get::<_, String>(0),
            )
            .optional()
            .map_err(db_error)?
        {
            return serde_json::from_str(&result).map_err(|e| e.to_string());
        }
        let n:i64=connection.query_row("update work_sequences set next_value=next_value+1 where sequence_name='ticket' returning next_value-1",[],|r|r.get(0)).map_err(db_error)?;
        let id = format!("ticket-{}", Uuid::new_v4());
        let event = format!("event-{}", Uuid::new_v4());
        let receipt = format!("receipt-{}", Uuid::new_v4());
        let nowv = now();
        let summary = required_string(&args, "summary")?;
        connection.execute("insert into tickets(ticket_id,ticket_number,status,revision,summary,resolution_code,blocker_code,created_at,updated_at,terminal_at) values(?1,?2,'actionable',1,?3,null,null,?4,?4,null)",params![id,n,summary,nowv]).map_err(db_error)?;
        connection.execute("insert into ticket_sources(source_id,ticket_id,source_kind,source_scope,immutable_source_id,source_ref_json,policy_version,receipt_id,admitted_at) values(?1,?2,?3,?4,?5,?6,?7,?8,?9)",params![format!("source-{}",Uuid::new_v4()),id,required_string(&args,"source_kind")?,required_string(&args,"source_scope")?,required_string(&args,"immutable_source_id")?,args.get("source_ref").cloned().unwrap_or_else(||json!({})).to_string(),required_string(&args,"policy_version")?,receipt,nowv]).map_err(db_error)?;
        let topic = if args.get("work_due_policy").and_then(Value::as_str) == Some("inline") {
            "work.ticket-inline-processing.v1"
        } else {
            "work.ticket-work-due.v1"
        };
        let event_payload = json!({"ticket_id":id,"ticket_number":n,"status":"actionable","revision":1,"summary":summary});
        connection.execute("insert into work_lifecycle_events(event_id,aggregate_kind,aggregate_id,aggregate_revision,event_type,schema_version,causation_id,idempotency_key,payload_json,created_at) values(?1,'ticket',?2,1,'ticket.source.admitted',1,?3,?4,?5,?6)",params![event,id,required_string(&args,"causation_id")?,format!("event:{idem}"),event_payload.to_string(),nowv]).map_err(db_error)?;
        connection.execute("insert into work_outbox(event_id,topic,partition_key,aggregate_kind,aggregate_id,aggregate_revision,schema_version,causation_id,idempotency_key,payload_json,created_at,available_at,compacted_at) values(?1,?2,?3,'ticket',?3,1,1,?4,?5,?6,?7,?7,null)",params![event,topic,id,required_string(&args,"causation_id")?,format!("event:{idem}"),event_payload.to_string(),nowv]).map_err(db_error)?;
        let result = json!({"schema":"narada.domain_operation.v1","operation_key":idem,"outcome":"completed","event_id":event,"ticket_id":id,"result":{"status":"created","ticket_id":id,"ticket_number":n,"event_id":event,"receipt_id":receipt}});
        connection.execute("insert into work_operations(operation_key,operation_kind,request_digest,aggregate_kind,aggregate_id,aggregate_revision,result_json,created_at) values(?1,'ticket_admit_source',?2,'ticket',?3,1,?4,?5)",params![idem,digest(&args),id,result.to_string(),nowv]).map_err(db_error)?;
        Ok(result)
    }
    fn ticket_processing_context(&self, args: Value) -> Result<Value, String> {
        let id = required_string(&args, "ticket_id")?;
        let event_id = required_string(&args, "triggering_event_id")?;
        let connection = self.connection()?;
        let ticket = connection
            .query_row(
                "select * from tickets where ticket_id=?1",
                params![id],
                |r| ticket_row(r),
            )
            .optional()
            .map_err(db_error)?
            .ok_or("ticket_not_found")?;
        let event = self
            .query_one(
                "select * from work_lifecycle_events where event_id=?1",
                params![event_id],
            )?
            .ok_or("triggering_event_not_found")?;
        Ok(
            json!({"schema":"narada.domain_operation.v1","operation_key":args.get("idempotency_key"),"outcome":"completed","result":{"ticket":ticket,"triggering_event":event}}),
        )
    }
    fn ticket_admit_proposal(&mut self, _args: Value) -> Result<Value, String> {
        Ok(
            json!({"schema":"narada.domain_operation.v1","outcome":"completed","result":{"status":"accepted"}}),
        )
    }
    fn outbox_list(&self, args: Value) -> Result<Value, String> {
        let consumer = required_string(&args, "consumer_id")?;
        let rows=self.query_objects("select * from work_outbox where event_id not in(select event_id from work_outbox_receipts where consumer_id=?1) order by created_at limit 100",params![consumer])?;
        Ok(json!({"schema":"narada.work_lifecycle.outbox.v1","count":rows.len(),"events":rows}))
    }
    fn outbox_register(&mut self, args: Value) -> Result<Value, String> {
        let c = self.connection_mut()?;
        c.execute("insert or ignore into work_outbox_consumer_requirements(topic,consumer_id,registered_at) values(?1,?2,?3)",params![required_string(&args,"topic")?,required_string(&args,"consumer_id")?,now()]).map_err(db_error)?;
        Ok(json!({"status":"registered"}))
    }
    fn outbox_ack(&mut self, args: Value) -> Result<Value, String> {
        let c = self.connection_mut()?;
        c.execute("insert or replace into work_outbox_receipts(event_id,consumer_id,processed_at,receipt_json) values(?1,?2,?3,?4)",params![required_string(&args,"event_id")?,required_string(&args,"consumer_id")?,now(),args.get("receipt").cloned().unwrap_or_else(||json!({})).to_string()]).map_err(db_error)?;
        Ok(json!({"status":"acknowledged"}))
    }
    fn storage_inspect(&self) -> Result<Value, String> {
        let c = self.connection()?;
        let tables = [
            "task_lifecycle",
            "tickets",
            "work_lifecycle_events",
            "work_outbox",
            "work_operations",
        ];
        let mut counts = Map::new();
        for table in tables {
            let count: i64 = c
                .query_row(&format!("select count(*) from {table}"), [], |r| r.get(0))
                .unwrap_or(0);
            counts.insert(table.to_string(), json!(count));
        }
        Ok(json!({"schema":"narada.work_lifecycle.storage.v1","status":"ok","tables":counts}))
    }
    fn query_objects(
        &self,
        sql: &str,
        params: impl rusqlite::Params,
    ) -> Result<Vec<Value>, String> {
        let c = self.connection()?;
        let mut s = c.prepare(sql).map_err(db_error)?;
        let mut rows = s.query(params).map_err(db_error)?;
        let mut out = Vec::new();
        while let Some(row) = rows.next().map_err(db_error)? {
            out.push(row_to_object(row).map_err(db_error)?);
        }
        Ok(out)
    }
    fn query_one(&self, sql: &str, params: impl rusqlite::Params) -> Result<Option<Value>, String> {
        let c = self.connection()?;
        let mut s = c.prepare(sql).map_err(db_error)?;
        let mut rows = s.query(params).map_err(db_error)?;
        rows.next()
            .map_err(db_error)?
            .map(|r| row_to_object(r).map_err(db_error))
            .transpose()
    }
    fn connection_mut(&mut self) -> Result<&mut Connection, String> {
        self.connection
            .as_mut()
            .ok_or_else(|| "lifecycle_runtime_not_open".to_string())
    }
    fn database_path(&self) -> PathBuf {
        self.options.database_path()
    }
}

impl Options {
    fn database_path(&self) -> PathBuf {
        self.site_root.join(self.surface.database_relative_path())
    }
}
impl Surface {
    fn prefix(self) -> &'static str {
        match self {
            Self::Task => "task_lifecycle",
            Self::Work => "work_lifecycle",
        }
    }
}

fn inspect_database(options: &Options) -> Result<Value, String> {
    let path = options.database_path();
    if !path.exists() {
        return Ok(
            json!({"status":"missing","db_path":path,"schema_version":null,"reason":"database_missing"}),
        );
    }
    let mut c = Connection::open(&path).map_err(|_| "invalid_database".to_string())?;
    configure_connection(&mut c, false).ok();
    inspect_connection(options.surface, &c, &path)
}
fn inspect_connection(surface: Surface, c: &Connection, path: &Path) -> Result<Value, String> {
    let mut tables = Vec::new();
    let mut st = c
        .prepare("select name from sqlite_master where type='table'")
        .map_err(db_error)?;
    let mut rows = st.query([]).map_err(db_error)?;
    while let Some(r) = rows.next().map_err(db_error)? {
        tables.push(r.get::<_, String>(0).map_err(db_error)?);
    }
    let required = if surface == Surface::Task {
        vec!["task_lifecycle", "task_specs", "task_assignments"]
    } else {
        vec![
            "task_lifecycle",
            "task_specs",
            "tickets",
            "work_lifecycle_meta",
            "work_outbox",
        ]
    };
    if required.iter().any(|x| !tables.iter().any(|v| v == x)) {
        return Ok(
            json!({"status":"stale","db_path":path,"schema_version":null,"reason":"schema"}),
        );
    }
    if surface == Surface::Work {
        let version: Option<i64> = c
            .query_row(
                "select schema_version from work_lifecycle_meta where singleton=1",
                [],
                |r| r.get(0),
            )
            .optional()
            .map_err(db_error)?;
        if version != Some(WORK_SCHEMA_VERSION) {
            return Ok(
                json!({"status":"stale","db_path":path,"work_schema_version":version,"task_schema_version":TASK_SCHEMA_VERSION,"reason":"work_schema_version"}),
            );
        }
        return Ok(
            json!({"status":"prepared","db_path":path,"work_schema_version":version,"task_schema_version":TASK_SCHEMA_VERSION}),
        );
    }
    Ok(json!({"status":"prepared","db_path":path,"schema_version":TASK_SCHEMA_VERSION}))
}
fn configure_connection(c: &mut Connection, prepare: bool) -> Result<(), String> {
    c.busy_timeout(std::time::Duration::from_millis(5000))
        .map_err(db_error)?;
    c.execute_batch("pragma foreign_keys=on; pragma recursive_triggers=off;")
        .map_err(db_error)?;
    if prepare {
        c.execute_batch("pragma journal_mode=wal; pragma synchronous=normal;")
            .map_err(db_error)?;
    }
    Ok(())
}
fn ensure_work_task_revision_triggers(c: &Connection) -> Result<(), String> {
    let mut statement = c.prepare("select name from sqlite_master where type=?1").map_err(db_error)?;
    let tables = statement.query_map(params!["table"], |row| row.get::<_, String>(0)).map_err(db_error)?.collect::<Result<Vec<_>, _>>().map_err(db_error)?;
    for table in tables {
        if table == "task_lifecycle" || table.starts_with("sqlite_") || table.starts_with("ticket_") || table.starts_with("work_") || !table.chars().all(|value| value.is_ascii_alphanumeric() || value == '_') { continue; }
        if !has_column(c, &table, "task_id")? { continue; }
        for (operation, reference) in [("insert", "new"), ("update", "new"), ("delete", "old")] {
            let trigger = format!("work_task_revision_{table}_{operation}");
            let sql = format!("drop trigger if exists {trigger}; create trigger {trigger} after {operation} on {table} when {reference}.task_id is not null begin update task_lifecycle set updated_at=strftime(\"%Y-%m-%dT%H:%M:%fZ\",\"now\") where task_id={reference}.task_id; end;");
            c.execute_batch(&sql).map_err(db_error)?;
        }
    }
    Ok(())
}
fn ensure_task_post_schema(c: &Connection) -> Result<(), String> {
    for (column, ty) in [
        ("closure_mode", "text"),
        ("relative_priority", "integer default 0"),
        ("priority_reason", "text"),
    ] {
        let exists = has_column(c, "task_lifecycle", column)?;
        if !exists {
            c.execute(
                &format!("alter table task_lifecycle add column {column} {ty}"),
                [],
            )
            .map_err(db_error)?;
        }
    }
    if !has_column(c, "task_specs", "tags_json")? {
        c.execute(
            "alter table task_specs add column tags_json text not null default '[]'",
            [],
        )
        .map_err(db_error)?;
    }
    if !has_column(c, "task_reports", "directive_id")? {
        c.execute("alter table task_reports add column directive_id text", [])
            .map_err(db_error)?;
    }
    for (table, column) in [("task_reports", "agent_identity_ref_json"), ("task_report_records", "agent_identity_ref_json")] {
        if !has_column(c, table, column)? {
            c.execute(&format!("alter table {table} add column {column} text"), []).map_err(db_error)?;
        }
    }
    c.execute_batch("create index if not exists idx_task_reports_directive_id on task_reports(directive_id); create table if not exists task_tag_updates(update_id text primary key,task_id text not null,task_number integer not null,actor_agent_id text not null,previous_tags_json text not null,new_tags_json text not null,reason text not null,updated_at text not null);").map_err(db_error)?;
    c.execute_batch("create table if not exists narada_task_creation_requests(idempotency_key text primary key,payload_sha256 text not null,task_id text not null unique,task_number integer not null unique,file_path text not null,execution_binding_json text not null,status text not null check(status in ('reserved','created','failed')),created_at text not null,updated_at text not null);
        create index if not exists idx_narada_task_creation_requests_status on narada_task_creation_requests(status);
        create table if not exists narada_task_execution_bindings(task_id text primary key,task_number integer not null unique,binding_json text not null,correlation_key text not null unique,created_at text not null,updated_at text not null);
        create table if not exists narada_andrey_task_role_preferences(task_id text primary key,preferred_role text,target_role text,preferred_agent_id text,updated_at text not null);
        create table if not exists task_routing_events(event_id text primary key,task_id text not null,task_number integer not null,actor_agent_id text not null,actor_role text,reason text not null,changed_fields_json text not null,previous_routing_json text not null,new_routing_json text not null,created_at text not null);
        create index if not exists idx_task_routing_events_task_id on task_routing_events(task_id);
        create table if not exists agent_roster_events(event_id text primary key,event_type text not null,agent_id text not null,role text,capabilities_json text,operator_identity text,requested_by text not null,requested_at text not null,authority_basis_json text not null,admission_status text not null,admitted_by text,admitted_at text,reason text,payload_json text,supersedes_event_id text);
        create index if not exists idx_agent_roster_events_agent_id on agent_roster_events(agent_id,requested_at);
        create index if not exists idx_agent_roster_events_status on agent_roster_events(admission_status,requested_at);").map_err(db_error)?;
    Ok(())
}
fn ensure_native_auxiliary_schema(c: &Connection) -> Result<(), String> {
    c.execute_batch(
        "create table if not exists native_task_operations (
            operation_key text primary key,
            operation_kind text not null,
            request_digest text not null,
            result_json text not null,
            created_at text not null
        );
        create table if not exists task_lifecycle_events (
            event_id text primary key,
            task_id text,
            task_number integer,
            event_type text not null,
            payload_json text not null,
            created_at text not null
        );
        create table if not exists task_chapter_memberships (
            chapter_id text not null,
            task_number integer not null,
            order_index integer not null,
            note text,
            actor_agent_id text,
            updated_at text not null,
            primary key (chapter_id, task_number)
        );
        create table if not exists recurring_task_definitions (
            recurrence_id text primary key,
            status text not null,
            definition_json text not null,
            last_due_key text,
            last_auto_triggered_at text,
            updated_at text not null
        );
        create table if not exists recurring_task_events (
            event_id text primary key,
            recurrence_id text not null,
            event_type text not null,
            actor_agent_id text not null,
            authority_basis_json text not null,
            event_json text not null,
            created_at text not null
        );
        create table if not exists recurring_task_runs (
            run_id text primary key,
            recurrence_id text not null,
            task_id text,
            task_number integer,
            due_key text,
            trigger_mode text not null,
            reason text not null,
            created_at text not null,
            run_json text not null
        );
        create index if not exists idx_recurring_task_definitions_status
            on recurring_task_definitions(status);
        create index if not exists idx_recurring_task_runs_recurrence
            on recurring_task_runs(recurrence_id, created_at desc);",
    )
    .map_err(db_error)
}
fn ensure_downstream_dependency_contracts(c: &Connection) -> Result<(), String> {
    let mut statement = c
        .prepare("select required_task_id,satisfying_outcomes_json,created_by,created_at from task_dependencies where kind='downstream_work'")
        .map_err(db_error)?;
    let dependencies = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })
        .map_err(db_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(db_error)?;
    for (task_id, satisfying_json, created_by, created_at) in dependencies {
        let exists: bool = c
            .query_row("select count(*) from task_outcome_contracts where task_id=?1 and outcome_type='completion'", params![&task_id], |row| row.get::<_, i64>(0))
            .map_err(db_error)? > 0;
        if exists { continue; }
        let satisfying = serde_json::from_str::<Value>(&satisfying_json).unwrap_or_else(|_| json!(["completed"]));
        let satisfying = if satisfying.as_array().is_some_and(|items| !items.is_empty()) { satisfying } else { json!(["completed"]) };
        let allowed = json!(["completed","blocked","failed"]);
        c.execute("insert or ignore into task_outcome_contracts(contract_id,task_id,outcome_type,allowed_outcomes_json,satisfying_outcomes_json,blocking_outcomes_json,required_fields_json,capability_requirement,created_by,created_at) values(?1,?2,'completion',?3,?4,?5,?6,null,?7,?8)", params![format!("contract-downstream_work-{task_id}"), &task_id, allowed.to_string(), satisfying.to_string(), json!(["blocked","failed"]).to_string(), json!(["summary"]).to_string(), &created_by, &created_at]).map_err(db_error)?;
    }
    Ok(())
}
fn ensure_task_revision_column(c: &Connection) -> Result<(), String> {
    if !has_column(c, "task_lifecycle", "revision")? {
        c.execute(
            "alter table task_lifecycle add column revision integer not null default 1",
            [],
        )
        .map_err(db_error)?;
    }
    Ok(())
}
fn has_column(c: &Connection, table: &str, column: &str) -> Result<bool, String> {
    let mut s = c
        .prepare(&format!("pragma table_info({table})"))
        .map_err(db_error)?;
    let mut rows = s.query([]).map_err(db_error)?;
    while let Some(r) = rows.next().map_err(db_error)? {
        if r.get::<_, String>(1).map_err(db_error)? == column {
            return Ok(true);
        }
    }
    Ok(false)
}
fn lifecycle_value(r: &Row<'_>) -> rusqlite::Result<Value> {
    let mut m = Map::new();
    for (i, name) in [
        "task_id",
        "task_number",
        "status",
        "governed_by",
        "closed_at",
        "closed_by",
        "closure_mode",
        "relative_priority",
        "priority_reason",
        "reopened_at",
        "reopened_by",
        "continuation_packet_json",
        "updated_at",
        "revision",
    ]
    .iter()
    .enumerate()
    .take(r.as_ref().column_count())
    {
        let v: rusqlite::types::Value = r.get(i)?;
        m.insert((*name).to_string(), sql_value(v));
    }
    Ok(Value::Object(m))
}
fn row_to_object(r: &Row<'_>) -> rusqlite::Result<Value> {
    let mut m = Map::new();
    for i in 0..r.as_ref().column_count() {
        let name = r.as_ref().column_name(i)?.to_string();
        let v: rusqlite::types::Value = r.get(i)?;
        m.insert(name, sql_value(v));
    }
    Ok(Value::Object(m))
}
fn ticket_row(r: &Row<'_>) -> rusqlite::Result<Value> {
    row_to_object(r)
}
fn sql_value(v: rusqlite::types::Value) -> Value {
    match v {
        rusqlite::types::Value::Null => Value::Null,
        rusqlite::types::Value::Integer(v) => json!(v),
        rusqlite::types::Value::Real(v) => json!(v),
        rusqlite::types::Value::Text(v) => Value::String(v),
        rusqlite::types::Value::Blob(v) => json!(base64_like(&v)),
    }
}
fn base64_like(v: &[u8]) -> String {
    v.iter().map(|b| format!("{b:02x}")).collect()
}
fn db_error<E: std::fmt::Display>(e: E) -> String {
    format!("sqlite_error:{e}")
}
fn now() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
}
fn read_json_file(path: &Path) -> Option<Value> {
    let text = fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}
fn write_json_file(path: &Path, value: &Value, label: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("{label}_directory_create_failed:{e}"))?;
    }
    let bytes = serde_json::to_vec_pretty(value).map_err(|e| format!("{label}_serialize_failed:{e}"))?;
    fs::write(path, bytes).map_err(|e| format!("{label}_write_failed:{e}"))
}
fn digest(value: &Value) -> String {
    native_canonical_digest(value)
}
fn required_string(args: &Value, key: &str) -> Result<String, String> {
    string_arg(args, key).ok_or_else(|| format!("{key}_required"))
}
fn required_i64(args: &Value, key: &str) -> Result<i64, String> {
    args.get(key)
        .and_then(Value::as_i64)
        .ok_or_else(|| format!("{key}_required"))
}
fn string_arg(args: &Value, key: &str) -> Option<String> {
    args.get(key)
        .and_then(Value::as_str)
        .map(ToString::to_string)
}
fn safe_reference_id(reference: &str, prefix: &str) -> Result<String, String> {
    let id = reference.strip_prefix(prefix).unwrap_or(reference);
    if id.is_empty()
        || id.len() > 200
        || id.contains("..")
        || id.contains('/')
        || id.contains('\\')
        || id.contains('\0')
    {
        return Err(format!("invalid_reference:{reference}"));
    }
    if !id
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | ':' | '.'))
    {
        return Err(format!("invalid_reference:{reference}"));
    }
    Ok(id.to_string())
}
fn normalized_text(args: &Value, key: &str) -> String {
    match args.get(key) {
        Some(Value::Array(v)) => v
            .iter()
            .filter_map(Value::as_str)
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .collect::<Vec<_>>()
            .join("\n"),
        Some(Value::String(v)) => v.clone(),
        _ => String::new(),
    }
}
fn binding_string(input: &Map<String, Value>, key: &str, required: bool) -> Result<Option<String>, String> {
    let Some(value) = input.get(key) else { return Ok(None); };
    if value.is_null() && !required { return Ok(None); }
    let Some(value) = value.as_str() else {
        return Err(format!("execution_binding_{key}_must_be_string"));
    };
    let value = value.trim();
    if value.is_empty() {
        if required { return Err(format!("execution_binding_{key}_required")); }
        return Ok(None);
    }
    Ok(Some(value.to_string()))
}
fn absolute_binding_path(value: &str, field: &str) -> Result<String, String> {
    let path = PathBuf::from(value);
    if !path.is_absolute() { return Err(format!("{field}_must_be_absolute")); }
    Ok(path.to_string_lossy().to_string())
}
fn normalize_execution_binding(root: &Path, value: Option<&Value>, correlation_key: &str) -> Result<Value, String> {
    let input = value.and_then(Value::as_object).cloned().unwrap_or_default();
    for key in input.keys() {
        if !matches!(key.as_str(), "workspace_root" | "executor_kind" | "executor_profile" | "executor_id" | "repository_root" | "site_root" | "correlation_key") {
            return Err(format!("execution_binding_unknown_fields: {key}"));
        }
    }
    let workspace_root = binding_string(&input, "workspace_root", true)?
        .unwrap_or_else(|| root.to_string_lossy().to_string());
    let workspace_root = absolute_binding_path(&workspace_root, "execution_binding_workspace_root")?;
    let executor_kind = binding_string(&input, "executor_kind", true)?
        .unwrap_or_else(|| "manual".to_string());
    if !matches!(executor_kind.as_str(), "manual" | "operator" | "worker_delegation" | "delegated_task" | "site_loop") {
        return Err(format!("execution_binding_executor_kind_invalid: {executor_kind}"));
    }
    let correlation_key = binding_string(&input, "correlation_key", true)?
        .unwrap_or_else(|| correlation_key.to_string());
    let executor_profile = binding_string(&input, "executor_profile", false)?;
    let executor_id = binding_string(&input, "executor_id", false)?;
    let repository_root = binding_string(&input, "repository_root", false)?
        .map(|value| absolute_binding_path(&value, "execution_binding_repository_root"))
        .transpose()?;
    let site_root = binding_string(&input, "site_root", false)?.or_else(|| Some(root.to_string_lossy().to_string()))
        .map(|value| absolute_binding_path(&value, "execution_binding_site_root"))
        .transpose()?;
    Ok(json!({
        "workspace_root": workspace_root,
        "executor_kind": executor_kind,
        "executor_profile": executor_profile,
        "executor_id": executor_id,
        "repository_root": repository_root,
        "site_root": site_root,
        "correlation_key": correlation_key,
    }))
}
fn path_within_root(candidate: &str, root: &Path) -> bool {
    let candidate = normalized_path_string(Path::new(candidate));
    let root = normalized_path_string(root);
    candidate == root || candidate.starts_with(&(root + "/"))
}
fn validate_execution_binding_scope(binding: &Value, site_root: &Path) -> Result<(), String> {
    let Some(binding) = binding.as_object() else { return Err("execution_binding_invalid".to_string()); };
    let current_root = normalized_path_string(site_root);
    if let Some(value) = binding.get("site_root").and_then(Value::as_str) {
        if normalized_path_string(Path::new(value)) != current_root {
            return Err("task_lifecycle_execution_binding_site_root_mismatch".to_string());
        }
    }
    let workspace = binding.get("workspace_root").and_then(Value::as_str).ok_or("execution_binding_workspace_root_required")?;
    let site_is_narada = site_root.file_name().and_then(|value| value.to_str()).is_some_and(|value| value.eq_ignore_ascii_case(".narada"));
    let workspace_authorized = path_within_root(workspace, site_root)
        || (site_is_narada && normalized_path_string(Path::new(workspace)) == normalized_path_string(site_root.parent().unwrap_or(site_root)) && site_root.parent().is_some_and(|value| value.join(".git").exists()));
    if !workspace_authorized { return Err("task_lifecycle_execution_binding_workspace_outside_site_root".to_string()); }
    if let Some(repository) = binding.get("repository_root").and_then(Value::as_str) {
        if !path_within_root(repository, site_root)
            && !(site_is_narada && normalized_path_string(Path::new(repository)) == normalized_path_string(site_root.parent().unwrap_or(site_root)) && site_root.parent().is_some_and(|value| value.join(".git").exists()))
        {
            return Err("task_lifecycle_execution_binding_repository_outside_site_root".to_string());
        }
    }
    Ok(())
}
fn resolve_payload_args(root: &Path, args: &Value) -> Result<Value, String> {
    let Some(reference) = string_arg(args, "payload_ref") else {
        return Ok(args.clone());
    };
    let payload = if parse_payload_reference(&reference).is_ok() {
        read_payload_revision_payload(root, &reference)?
    } else {
        let id = safe_reference_id(&reference, "mcp_payload:")?;
        let path = root
            .join(".ai")
            .join("mcp-payloads")
            .join(format!("{id}.json"));
        let text =
            fs::read_to_string(path).map_err(|_| format!("payload_not_found:{reference}"))?;
        let payload: Value =
            serde_json::from_str(&text).map_err(|e| format!("payload_invalid:{e}"))?;
        if !payload.is_object() {
            return Err(format!("payload_ref_payload_must_be_object:{reference}"));
        }
        payload
    };
    let mut merged = payload
        .as_object()
        .cloned()
        .ok_or_else(|| format!("payload_ref_payload_must_be_object:{reference}"))?;
    if let Some(object) = args.as_object() {
        for (key, value) in object {
            if !matches!(
                key.as_str(),
                "payload_ref" | "payload_path" | "payload" | "payload_file"
            ) {
                merged.insert(key.clone(), value.clone());
            }
        }
    }
    Ok(Value::Object(merged))
}
fn task_file_path(root: &Path, task_id: &str) -> String {
    root.join(".ai/do-not-open/tasks")
        .join(format!("{task_id}.md"))
        .to_string_lossy()
        .to_string()
}
fn task_file_body(root: &Path, number: i64) -> Option<String> {
    let dir = root.join(".ai/do-not-open/tasks");
    let entries = fs::read_dir(dir).ok()?;
    for e in entries.flatten().take(200) {
        let path = e.path();
        if path.extension().and_then(|v| v.to_str()) == Some("md") {
            let text = fs::read_to_string(path).ok()?;
            if text
                .lines()
                .any(|l| l.trim() == format!("number: {number}"))
            {
                return Some(text);
            }
        }
    }
    None
}
fn project_task_status(root: &Path, number: i64, status: &str) -> Result<(), String> {
    let dir = root.join(".ai/do-not-open/tasks");
    let entries = fs::read_dir(&dir).map_err(|e| format!("task_projection_read_failed:{e}"))?;
    for entry in entries.flatten().take(200) {
        let path = entry.path();
        if path.extension().and_then(|v| v.to_str()) != Some("md") { continue; }
        let text = fs::read_to_string(&path).map_err(|e| format!("task_projection_read_failed:{e}"))?;
        if !text.lines().any(|line| line.trim() == format!("number: {number}")) { continue; }
        let mut replaced = false;
        let mut output = String::with_capacity(text.len() + status.len());
        for line in text.lines() {
            if line.starts_with("status:") && !replaced {
                output.push_str(&format!("status: {status}"));
                replaced = true;
            } else { output.push_str(line); }
            output.push('\n');
        }
        if !replaced { output = format!("status: {status}\n{output}"); }
        fs::write(path, output).map_err(|e| format!("task_projection_write_failed:{e}"))?;
        return Ok(());
    }
    Ok(())
}
fn write_task_file(
    root: &Path,
    task_id: &str,
    number: i64,
    title: &str,
    goal: &str,
    work: &str,
    non_goals: &str,
    criteria: &Value,
    tags: &Value,
    role: Option<&str>,
    idem: &str,
) -> Result<(), String> {
    let dir = root.join(".ai/do-not-open/tasks");
    fs::create_dir_all(&dir).map_err(|e| format!("task_projection_directory_create_failed:{e}"))?;
    let path = dir.join(format!("{task_id}.md"));
    let tags_text = tags
        .as_array()
        .map(|v| {
            v.iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default();
    let body=format!("---\nnumber: {number}\ngoverned_by: {}\nstatus: opened\n{}{}tags: {tags_text}\nidempotency_key: {idem}\n---\n# {title}\n\n## Goal\n{goal}\n\n## Required Work\n{work}\n\n## Non-Goals\n{non_goals}\n\n## Acceptance Criteria\n{}\n\n## Execution Notes\n\n## Verification\n",role.unwrap_or("unknown"),role.map(|v|format!("preferred_role: {v}\n")).unwrap_or_default(),if tags_text.is_empty(){String::new()}else{String::new()},criteria.as_array().map(|v|v.iter().filter_map(Value::as_str).map(|v|format!("- [ ] {v}\n")).collect::<String>()).unwrap_or_default());
    fs::write(path, body).map_err(|e| format!("task_projection_write_failed:{e}"))
}
fn append_task_body(root: &Path, number: i64, summary: &str) -> Result<(), String> {
    let dir = root.join(".ai/do-not-open/tasks");
    for e in fs::read_dir(dir)
        .map_err(|e| e.to_string())?
        .flatten()
        .take(200)
    {
        let path = e.path();
        if path.extension().and_then(|v| v.to_str()) == Some("md") {
            let text = fs::read_to_string(&path).unwrap_or_default();
            if text
                .lines()
                .any(|l| l.trim() == format!("number: {number}"))
            {
                let next = format!("{text}\n{summary}\n");
                fs::write(path, next).map_err(|e| e.to_string())?;
                return Ok(());
            }
        }
    }
    Ok(())
}
fn name_is_close(args: &Value) -> bool {
    args.get("mode")
        .and_then(Value::as_str)
        .map(|v| {
            matches!(
                v,
                "operator_direct" | "peer_reviewed" | "agent_finish" | "emergency"
            )
        })
        .unwrap_or(false)
}
fn tool_result(payload: Value, is_error: bool) -> Value {
    let text = serde_json::to_string(&payload).unwrap_or_else(|_| "{}".to_string());
    let mut result = json!({"content":[{"type":"text","text":text}],"structuredContent":payload});
    if is_error {
        result["isError"] = json!(true)
    }
    result
}
fn normalized_path_string(path: &Path) -> String {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        env::current_dir().unwrap_or_else(|_| PathBuf::from(".")).join(path)
    };
    let text = absolute.to_string_lossy().replace('\\', "/");
    let text = text.trim_end_matches('/');
    if cfg!(windows) { text.to_ascii_lowercase() } else { text.to_string() }
}
fn normalize_task_tool_name(name: &str) -> &str {
    match name {
        "task_lifecycle_closeout" => "task_lifecycle_disposition_closeout",
        "task_lifecycle_record_observation" => "task_lifecycle_submit_observation",
        "task_lifecycle_submit_report" => "task_lifecycle_finish",
        "task_lifecycle_d_af077406ea2f" => "task_lifecycle_disposition_closeout",
        "task_lifecycle_s_f5e0b1532dcf" => "task_lifecycle_submit_observation",
        "task_mcp_doctor" => "task_lifecycle_doctor",
        "task_mcp_restart" => "task_lifecycle_restart",
        "task_mcp_list" => "task_lifecycle_list",
        "task_mcp_show" => "task_lifecycle_show",
        "task_mcp_roster" => "task_lifecycle_roster",
        "task_mcp_roster_admit" => "task_lifecycle_roster_admit",
        "task_mcp_claim" => "task_lifecycle_claim",
        "task_mcp_continue" => "task_lifecycle_continue",
        "task_mcp_unclaim" => "task_lifecycle_unclaim",
        "task_mcp_next" => "task_lifecycle_next",
        "task_mcp_workboard_snapshot" => "task_lifecycle_workboard_snapshot",
        "task_mcp_obligations" => "task_lifecycle_obligations",
        "task_mcp_inspect" => "task_lifecycle_inspect",
        "task_mcp_evidence_preflight" => "task_lifecycle_evidence_preflight",
        "task_mcp_admit_evidence" => "task_lifecycle_admit_evidence",
        "task_mcp_prove_criteria" => "task_lifecycle_prove_criteria",
        "task_mcp_audit" => "task_lifecycle_audit",
        "task_mcp_finish" => "task_lifecycle_finish",
        "task_mcp_close" => "task_lifecycle_close",
        "task_mcp_search" => "task_lifecycle_search",
        "task_mcp_defer" => "task_lifecycle_defer",
        "task_mcp_un_defer" | "task_mcp_undefer" => "task_lifecycle_un_defer",
        "task_mcp_reopen" => "task_lifecycle_reopen",
        "task_mcp_review" => "task_lifecycle_review",
        "task_mcp_submit_observation" => "task_lifecycle_submit_observation",
        "task_mcp_bridge_poll" => "task_lifecycle_bridge_poll",
        "task_mcp_inbox_target" => "task_lifecycle_inbox_target",
        "task_mcp_create" => "task_lifecycle_create",
        "task_mcp_set_routing" => "task_lifecycle_set_routing",
        "task_mcp_tags_update" => "task_lifecycle_tags_update",
        "task_mcp_test_tool" => "task_lifecycle_test_mcp_tool",
        "task_mcp_run_tests" => "task_lifecycle_run_tests",
        _ => name,
    }
}fn is_locus_guarded_mutation(name: &str) -> bool {
    matches!(
        name,
        "task_lifecycle_claim"
            | "task_lifecycle_continue"
            | "task_lifecycle_unclaim"
            | "task_lifecycle_admit_evidence"
            | "task_lifecycle_prove_criteria"
            | "task_lifecycle_finish"
            | "task_lifecycle_submit_work"
            | "task_lifecycle_report_blocked"
            | "task_lifecycle_close"
            | "task_lifecycle_defer"
            | "task_lifecycle_un_defer"
            | "task_lifecycle_reopen"
            | "task_lifecycle_review"
            | "task_lifecycle_submit_observation"
            | "task_lifecycle_evidence_supersede"
            | "task_lifecycle_bridge_poll"
            | "task_lifecycle_inbox_target"
            | "task_lifecycle_create"
            | "task_lifecycle_tags_update"
            | "task_lifecycle_set_routing"
            | "task_lifecycle_dependency_declare"
            | "task_lifecycle_dependency_disposition_record"
            | "task_lifecycle_compatibility_reconcile"
            | "task_lifecycle_recurring_create"
            | "task_lifecycle_recurring_run_due"
            | "task_lifecycle_recurring_suspend"
            | "task_lifecycle_recurring_retire"
    )
}
fn is_task_read_only(name: &str) -> bool {
    matches!(
        name,
        "task_lifecycle_list"
            | "task_lifecycle_show"
            | "task_lifecycle_roster"
            | "task_lifecycle_guidance"
            | "task_lifecycle_payload_schema"
            | "task_lifecycle_evidence_preflight"
            | "task_lifecycle_self_certification_preflight"
            | "task_lifecycle_next"
            | "task_lifecycle_workboard_snapshot"
            | "task_lifecycle_obligations"
            | "task_lifecycle_inspect"
            | "task_lifecycle_inspect_range"
            | "task_lifecycle_audit"
            | "task_lifecycle_search"
            | "task_lifecycle_related"
            | "task_lifecycle_recurring_list"
            | "task_lifecycle_recurring_show"
            | "task_lifecycle_recurring_runs"
            | "task_lifecycle_diagnose_task_ref"
            | "mcp_payload_show"
            | "mcp_payload_validate"
            | "mcp_output_show"
    )
}
fn guidance_payload(args: Value) -> Value {
    json!({"status":"ok","workflow":args.get("workflow").cloned().unwrap_or(json!("all")),"first_use_decision_tree":[{"sequence":["task_lifecycle_show","task_lifecycle_claim","task_lifecycle_submit_work"]}]})
}

pub struct WireReader<R> {
    reader: R,
    buffer: Vec<u8>,
    eof: bool,
}
impl<R: Read> WireReader<R> {
    pub fn new(reader: R) -> Self {
        Self {
            reader,
            buffer: Vec::new(),
            eof: false,
        }
    }
    pub fn next(&mut self) -> io::Result<Option<(Value, bool)>> {
        loop {
            if let Some(v) = try_parse_wire(&mut self.buffer)? {
                return Ok(Some(v));
            }
            if self.eof {
                if self.buffer.iter().all(|b| b.is_ascii_whitespace()) {
                    self.buffer.clear();
                    return Ok(None);
                }
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "incomplete MCP message",
                ));
            }
            let mut chunk = [0u8; 8192];
            let n = self.reader.read(&mut chunk)?;
            if n == 0 {
                self.eof = true
            } else {
                self.buffer.extend_from_slice(&chunk[..n]);
            }
        }
    }
}
fn try_parse_wire(buffer: &mut Vec<u8>) -> io::Result<Option<(Value, bool)>> {
    while matches!(buffer.first(), Some(b'\r' | b'\n' | b' ' | b'\t')) {
        buffer.remove(0);
    }
    if buffer.is_empty() {
        return Ok(None);
    }
    if buffer.starts_with(b"Content-Length:") {
        let Some(end) = buffer.windows(4).position(|w| w == b"\r\n\r\n") else {
            return Ok(None);
        };
        let headers = String::from_utf8_lossy(&buffer[..end]);
        let length = headers
            .lines()
            .find_map(|line| {
                line.split_once(':')
                    .and_then(|(_, v)| v.trim().parse::<usize>().ok())
            })
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "invalid Content-Length"))?;
        let start = end + 4;
        if buffer.len() < start + length {
            return Ok(None);
        };
        let body = buffer[start..start + length].to_vec();
        buffer.drain(..start + length);
        let value = serde_json::from_slice(&body)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid JSON"))?;
        return Ok(Some((value, true)));
    }
    let Some(end) = buffer.iter().position(|b| *b == b'\n') else {
        return Ok(None);
    };
    let line = buffer.drain(..=end).collect::<Vec<_>>();
    let value = serde_json::from_slice(&line)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid JSON"))?;
    Ok(Some((value, false)))
}
fn write_wire<W: Write>(writer: &mut W, value: &Value, framed: bool) -> io::Result<()> {
    let body = serde_json::to_vec(value).unwrap_or_else(|_| b"{}".to_vec());
    if framed {
        write!(writer, "Content-Length: {}\r\n\r\n", body.len())?;
        writer.write_all(&body)?;
    } else {
        writer.write_all(&body)?;
        writer.write_all(b"\n")?;
    }
    writer.flush()
}

include!("executability_impl.rs");

include!("work_impl.rs");
