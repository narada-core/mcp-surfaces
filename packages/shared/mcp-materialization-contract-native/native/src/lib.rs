use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use toml_edit::{DocumentMut, Item, Table};

pub const CONTRACT_VERSION: u32 = 8;
pub const GENERATION_SCHEMA: &str = "narada.mcp_materialization_generation.v3";
pub const LEGACY_GENERATION_SCHEMA: &str = "narada.mcp_materialization_generation.v2";
pub const AMBIGUOUS_GENERATION_SCHEMA: &str = "narada.mcp_materialization_generation.v1";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConfigArtifact {
    pub bytes_sha256: String,
    pub encoding: String,
    pub bom: bool,
    pub line_endings: String,
    pub final_newline: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ManagedProjection {
    pub sha256: String,
    pub scope: String,
    pub canonicalization: String,
    pub selectors: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConfigDescription {
    pub config_artifact: ConfigArtifact,
    pub managed_projection: ManagedProjection,
}

pub fn sha256(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

pub fn canonical_json(value: &Value) -> Value {
    match value {
        Value::Array(items) => Value::Array(items.iter().map(canonical_json).collect()),
        Value::Object(items) => {
            let mut keys = items.keys().collect::<Vec<_>>();
            keys.sort();
            let mut sorted = Map::new();
            for key in keys {
                sorted.insert(key.clone(), canonical_json(&items[key]));
            }
            Value::Object(sorted)
        }
        _ => value.clone(),
    }
}

pub fn canonical_json_sha256(value: &Value) -> Result<String, String> {
    serde_json::to_vec(&canonical_json(value))
        .map(|bytes| sha256(&bytes))
        .map_err(|error| error.to_string())
}

pub fn generation_fingerprint(generation: &Value) -> Result<String, String> {
    let mut unsigned = generation.clone();
    let schema = unsigned.get("schema").and_then(Value::as_str).map(str::to_string);
    let object = unsigned
        .as_object_mut()
        .ok_or_else(|| "materialization_generation_not_object".to_string())?;
    object.remove("generation_fingerprint");
    if schema.as_deref() == Some(GENERATION_SCHEMA) {
        object.remove("generated_at");
    }
    canonical_json_sha256(&unsigned)
}

pub fn pretty_json(value: &Value) -> Result<Vec<u8>, String> {
    let mut bytes = serde_json::to_vec_pretty(value).map_err(|error| error.to_string())?;
    bytes.push(b'\n');
    Ok(bytes)
}

pub fn config_artifact(content: &[u8]) -> Result<ConfigArtifact, String> {
    let text = std::str::from_utf8(content).map_err(|error| error.to_string())?;
    let without_crlf = text.replace("\r\n", "");
    let has_crlf = text.contains("\r\n");
    let has_lf = without_crlf.contains('\n');
    let has_cr = without_crlf.contains('\r');
    let kinds = usize::from(has_crlf) + usize::from(has_lf) + usize::from(has_cr);
    let line_endings = if kinds == 0 {
        "none"
    } else if kinds > 1 {
        "mixed"
    } else if has_crlf {
        "crlf"
    } else if has_cr {
        "cr"
    } else {
        "lf"
    };
    Ok(ConfigArtifact {
        bytes_sha256: sha256(content),
        encoding: "utf-8".to_string(),
        bom: content.starts_with(&[0xef, 0xbb, 0xbf]),
        line_endings: line_endings.to_string(),
        final_newline: text.ends_with('\n') || text.ends_with('\r'),
    })
}

fn pointer_escape(value: &str) -> String {
    value.replace('~', "~0").replace('/', "~1")
}

fn pointer_unescape(value: &str) -> String {
    value.replace("~1", "/").replace("~0", "~")
}

fn pointer_segments(pointer: &str) -> Result<Vec<String>, String> {
    if !pointer.starts_with('/') {
        return Err(format!("managed_selector_invalid:{pointer}"));
    }
    Ok(pointer[1..].split('/').map(pointer_unescape).collect())
}

pub fn codex_managed_selectors(
    server_ids: impl IntoIterator<Item = impl AsRef<str>>,
    plugin_ids: impl IntoIterator<Item = impl AsRef<str>>,
    project_paths: impl IntoIterator<Item = impl AsRef<str>>,
) -> Vec<String> {
    let mut selectors = vec!["/features/apps".to_string()];
    selectors.extend(
        server_ids
            .into_iter()
            .map(|name| format!("/mcp_servers/{}", pointer_escape(name.as_ref()))),
    );
    selectors.extend(
        plugin_ids
            .into_iter()
            .map(|name| format!("/plugins/{}/enabled", pointer_escape(name.as_ref()))),
    );
    selectors.extend(
        project_paths
            .into_iter()
            .map(|project| format!("/projects/{}/trust_level", pointer_escape(project.as_ref()))),
    );
    selectors.sort();
    selectors.dedup();
    selectors
}

pub fn default_codex_managed_selectors(content: &[u8]) -> Result<Vec<String>, String> {
    let text = std::str::from_utf8(content)
        .map_err(|error| error.to_string())?
        .trim_start_matches('\u{feff}');
    let parsed: Value = toml_edit::de::from_str(text).map_err(|error| error.to_string())?;
    let mut selectors = Vec::new();
    if let Some(servers) = parsed.get("mcp_servers").and_then(Value::as_object) {
        for name in servers.keys().filter(|name| name.starts_with("narada-")) {
            selectors.push(format!("/mcp_servers/{}", pointer_escape(name)));
        }
    }
    if parsed.pointer("/features/apps").is_some() {
        selectors.push("/features/apps".to_string());
    }
    if let Some(plugins) = parsed.get("plugins").and_then(Value::as_object) {
        for (name, plugin) in plugins {
            if plugin.get("enabled").is_some() {
                selectors.push(format!("/plugins/{}/enabled", pointer_escape(name)));
            }
        }
    }
    if let Some(projects) = parsed.get("projects").and_then(Value::as_object) {
        for (path, project) in projects {
            if project.get("trust_level").is_some() {
                selectors.push(format!("/projects/{}/trust_level", pointer_escape(path)));
            }
        }
    }
    selectors.sort();
    selectors.dedup();
    Ok(selectors)
}

pub fn managed_projection(
    carrier_kind: &str,
    content: &[u8],
    selectors: &[String],
) -> Result<ManagedProjection, String> {
    if carrier_kind != "codex" {
        return Ok(ManagedProjection {
            sha256: sha256(content),
            scope: "whole_document".to_string(),
            canonicalization: "narada.whole_document_bytes.v1".to_string(),
            selectors: vec![],
        });
    }
    let text = std::str::from_utf8(content)
        .map_err(|error| error.to_string())?
        .trim_start_matches('\u{feff}');
    let parsed: Value = toml_edit::de::from_str(text).map_err(|error| error.to_string())?;
    let mut selectors = selectors.to_vec();
    selectors.sort();
    selectors.dedup();
    let mut values = Map::new();
    for selector in &selectors {
        let selected = parsed
            .pointer(selector)
            .ok_or_else(|| format!("managed_selector_missing:{selector}"))?;
        values.insert(selector.clone(), canonical_json(selected));
    }
    let payload = json!({"scope":"codex_managed_selectors","values":values});
    Ok(ManagedProjection {
        sha256: canonical_json_sha256(&payload)?,
        scope: "codex_managed_selectors".to_string(),
        canonicalization: "narada.codex_managed_projection.v1".to_string(),
        selectors,
    })
}

pub fn describe_config(
    carrier_kind: &str,
    content: &[u8],
    selectors: &[String],
) -> Result<ConfigDescription, String> {
    Ok(ConfigDescription {
        config_artifact: config_artifact(content)?,
        managed_projection: managed_projection(carrier_kind, content, selectors)?,
    })
}

fn item_at<'a>(item: &'a Item, segments: &[String]) -> Option<&'a Item> {
    let mut current = item;
    for segment in segments {
        current = current.as_table_like()?.get(segment)?;
    }
    Some(current)
}

fn ensure_parent<'a>(mut item: &'a mut Item, segments: &[String]) -> Result<&'a mut Item, String> {
    for segment in segments {
        if item
            .as_table_like()
            .and_then(|table| table.get(segment))
            .is_none()
        {
            item.as_table_like_mut()
                .ok_or_else(|| format!("managed_selector_parent_not_table:{segment}"))?
                .insert(segment, Item::Table(Table::new()));
        }
        item = item
            .as_table_like_mut()
            .and_then(|table| table.get_mut(segment))
            .ok_or_else(|| format!("managed_selector_parent_missing:{segment}"))?;
    }
    Ok(item)
}

fn set_selector(target: &mut Item, source: &Item, selector: &str) -> Result<(), String> {
    let segments = pointer_segments(selector)?;
    let (last, parents) = segments
        .split_last()
        .ok_or_else(|| format!("managed_selector_invalid:{selector}"))?;
    let value = item_at(source, &segments)
        .ok_or_else(|| format!("managed_selector_missing:{selector}"))?
        .clone();
    ensure_parent(target, parents)?
        .as_table_like_mut()
        .ok_or_else(|| format!("managed_selector_parent_not_table:{selector}"))?
        .insert(last, value);
    Ok(())
}

fn remove_selector(target: &mut Item, selector: &str) -> Result<(), String> {
    let segments = pointer_segments(selector)?;
    let (last, parents) = segments
        .split_last()
        .ok_or_else(|| format!("managed_selector_invalid:{selector}"))?;
    let mut current = target;
    for parent in parents {
        let Some(next) = current
            .as_table_like_mut()
            .and_then(|table| table.get_mut(parent))
        else {
            return Ok(());
        };
        current = next;
    }
    if let Some(table) = current.as_table_like_mut() {
        table.remove(last);
    }
    Ok(())
}

fn normalize_emitted_text(text: &str) -> Vec<u8> {
    let normalized = text
        .trim_start_matches('\u{feff}')
        .replace("\r\n", "\n")
        .replace('\r', "\n");
    let mut normalized = normalized.trim_end_matches('\n').to_string();
    normalized.push('\n');
    normalized.into_bytes()
}

pub fn merge_codex_configuration(
    existing: Option<&[u8]>,
    desired: &[u8],
    previous_selectors: &[String],
    current_selectors: &[String],
) -> Result<Vec<u8>, String> {
    let desired_text = std::str::from_utf8(desired)
        .map_err(|error| error.to_string())?
        .trim_start_matches('\u{feff}');
    let desired_doc = desired_text
        .parse::<DocumentMut>()
        .map_err(|error| error.to_string())?;
    let mut target_doc = match existing {
        Some(bytes) => {
            let text = std::str::from_utf8(bytes)
                .map_err(|error| error.to_string())?
                .trim_start_matches('\u{feff}')
                .replace(
                    "# Generated by narada-mcp-materializer. Do not hand-edit; changes will be overwritten on next materialize.",
                    "# Narada manages only recorded MCP and carrier policy settings; other Codex settings are preserved.",
                );
            text.parse::<DocumentMut>()
                .map_err(|error| error.to_string())?
        }
        None => desired_doc.clone(),
    };
    let mut effective_previous_selectors = previous_selectors.to_vec();
    if effective_previous_selectors.iter().any(|selector| selector == "/mcp_servers") {
        effective_previous_selectors.retain(|selector| selector != "/mcp_servers");
        if let Some(servers) = target_doc
            .get("mcp_servers")
            .and_then(|item| item.as_table_like())
        {
            effective_previous_selectors.extend(
                servers
                    .iter()
                    .map(|(name, _)| name)
                    .filter(|name| name.starts_with("narada-"))
                    .map(|name| format!("/mcp_servers/{}", pointer_escape(name))),
            );
        }
    }
    effective_previous_selectors.sort();
    effective_previous_selectors.dedup();
    for selector in &effective_previous_selectors {
        if !current_selectors.contains(selector) {
            remove_selector(target_doc.as_item_mut(), selector)?;
        }
    }
    for selector in current_selectors {
        set_selector(target_doc.as_item_mut(), desired_doc.as_item(), selector)?;
    }
    Ok(normalize_emitted_text(&target_doc.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Deserialize)]
    struct VectorCorpus {
        vectors: Vec<FingerprintVector>,
    }

    #[derive(Deserialize)]
    struct FingerprintVector {
        id: String,
        carrier_kind: String,
        content: String,
        selectors: Vec<String>,
        expected_bytes_sha256: String,
        expected_managed_sha256: String,
    }

    #[test]
    fn fingerprint_vectors_are_stable() {
        let corpus: VectorCorpus =
            serde_json::from_str(include_str!("../../contracts/fingerprint-vectors.json")).unwrap();
        for vector in corpus.vectors {
            let description = describe_config(
                &vector.carrier_kind,
                vector.content.as_bytes(),
                &vector.selectors,
            )
            .unwrap();
            println!(
                "VECTOR {} {} {}",
                vector.id,
                description.config_artifact.bytes_sha256,
                description.managed_projection.sha256
            );
            if !vector.expected_bytes_sha256.is_empty() {
                assert_eq!(
                    description.config_artifact.bytes_sha256, vector.expected_bytes_sha256,
                    "{} bytes",
                    vector.id
                );
            }
            if !vector.expected_managed_sha256.is_empty() {
                assert_eq!(
                    description.managed_projection.sha256, vector.expected_managed_sha256,
                    "{} managed",
                    vector.id
                );
            }
        }
    }

    #[test]
    fn codex_projection_ignores_formatting_and_unmanaged_values() {
        let selectors = vec!["/mcp_servers".to_string()];
        let lf = b"model = \"a\"\n[mcp_servers.fixture]\ncommand = \"x\"\n";
        let crlf = b"model = \"b\"\r\n\r\n[mcp_servers.fixture]\r\ncommand=\"x\"\r\n";
        assert_eq!(
            managed_projection("codex", lf, &selectors).unwrap().sha256,
            managed_projection("codex", crlf, &selectors)
                .unwrap()
                .sha256,
        );
        assert_ne!(
            config_artifact(lf).unwrap().bytes_sha256,
            config_artifact(crlf).unwrap().bytes_sha256
        );
    }

    #[test]
    fn codex_merge_preserves_unmanaged_settings_and_removes_retired_selectors() {
        let existing = b"model = \"gpt\"\n[plugins.old]\nenabled = true\ncustom = 1\n[mcp_servers.old]\ncommand = \"old\"\n";
        let desired = b"[features]\napps = false\n[mcp_servers.new]\ncommand = \"new\"\n";
        let previous = vec![
            "/mcp_servers".to_string(),
            "/plugins/old/enabled".to_string(),
        ];
        let current = vec!["/features/apps".to_string(), "/mcp_servers".to_string()];
        let merged = String::from_utf8(
            merge_codex_configuration(Some(existing), desired, &previous, &current).unwrap(),
        )
        .unwrap();
        assert!(merged.contains("model = \"gpt\""));
        assert!(merged.contains("custom = 1"));
        assert!(!merged.contains("enabled = true"));
        assert!(merged.contains("[mcp_servers.new]"));
        assert!(!merged.contains("[mcp_servers.old]"));
        assert!(!merged.contains('\r'));
        assert!(merged.ends_with('\n'));
    }

    #[test]
    fn legacy_whole_table_ownership_is_narrowed_during_generic_name_migration() {
        let existing = b"[mcp_servers.user-owned]\ncommand = \"user\"\n[mcp_servers.narada-site-old-local-filesystem]\ncommand = \"old\"\n";
        let desired = b"[mcp_servers.local-filesystem]\ncommand = \"new\"\n";
        let merged = String::from_utf8(
            merge_codex_configuration(
                Some(existing),
                desired,
                &["/mcp_servers".to_string()],
                &["/mcp_servers/local-filesystem".to_string()],
            )
            .unwrap(),
        )
        .unwrap();
        assert!(merged.contains("[mcp_servers.user-owned]"));
        assert!(merged.contains("[mcp_servers.local-filesystem]"));
        assert!(!merged.contains("narada-site-old-local-filesystem"));
    }
}
