fn observed_array(input: &Value, keys: &[&str]) -> Option<Vec<String>> {
    keys.iter().find_map(|key| {
        input.get(key).and_then(Value::as_array).map(|values| {
            values
                .iter()
                .map(|value| value.as_str().unwrap_or("").to_string())
                .collect()
        })
    })
}
fn strings(value: &Value) -> Vec<String> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .map(|value| value.as_str().unwrap_or("").to_string())
        .collect()
}
fn unique_strings(values: &[String]) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    values
        .iter()
        .filter(|value| seen.insert((*value).clone()))
        .cloned()
        .collect()
}
fn duplicate_strings(values: &[String]) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut duplicates = vec![];
    for value in values {
        if !seen.insert(value.clone()) && !duplicates.contains(value) {
            duplicates.push(value.clone());
        }
    }
    duplicates.sort();
    duplicates
}
fn compare_sets(
    add: &mut impl FnMut(&str, &str, Value),
    layer: &str,
    code: &str,
    expected: &[String],
    actual: &[String],
) {
    let mut left = expected.to_vec();
    left.sort();
    left.dedup();
    let mut right = actual.to_vec();
    right.sort();
    right.dedup();
    if left == right {
        return;
    }
    let missing = left
        .iter()
        .filter(|value| !right.contains(value))
        .cloned()
        .collect::<Vec<_>>();
    let extra = right
        .iter()
        .filter(|value| !left.contains(value))
        .cloned()
        .collect::<Vec<_>>();
    add(
        layer,
        code,
        json!({"missing":missing,"extra":extra,"expected_count":left.len(),"actual_count":right.len()}),
    );
}
fn comparable_path(value: &str) -> String {
    value.replace('\\', "/").to_lowercase()
}
fn json_type_name(value: Option<&Value>) -> &'static str {
    match value {
        Some(Value::Array(_)) => "object",
        Some(Value::Object(_)) => "object",
        Some(Value::String(_)) => "string",
        Some(Value::Bool(_)) => "boolean",
        Some(Value::Number(_)) => "number",
        _ => "undefined",
    }
}

fn carrier_diff(contract: &Value, args: &Value) -> Result<Value, String> {
    let carrier_id = required_argument(args, "carrier_id", "registrar_requires_carrier_id")?;
    let plan = contract
        .pointer(&format!(
            "/read_models/registrar_carrier_projection_plans/{carrier_id}"
        ))
        .ok_or_else(|| format!("registrar_unknown_carrier:{carrier_id}"))?;
    let config_path = plan["config_path"].as_str().unwrap_or("");
    let generated_content = plan["generated_content"].as_str().unwrap_or("");
    let generated_structured = &plan["generated_structured"];
    let current_content = fs::read_to_string(config_path).ok();
    let current_structured = current_content
        .as_deref()
        .and_then(|content| parse_carrier_config(plan["kind"].as_str().unwrap_or(""), content));
    let generated_servers = carrier_servers(generated_structured);
    let current_servers = current_structured
        .as_ref()
        .map(carrier_servers)
        .unwrap_or_default();
    if let (Some(current_content), Some(receipt)) = (
        current_content.as_deref(),
        native_materialization_receipt(config_path, &carrier_id),
    ) {
        let current_sha256 = sha256_text(current_content);
        if receipt.matches(
            plan["kind"].as_str().unwrap_or(""),
            current_content.as_bytes(),
        ) {
            let mut unchanged = current_servers.keys().cloned().collect::<Vec<_>>();
            unchanged.sort();
            return Ok(json!({
                "schema":"narada.registrar.carrier_projection_diff.v1",
                "status":"clean",
                "carrier_id":carrier_id,
                "config_path":config_path,
                "current_exists":true,
                "projection_changed":false,
                "server_projection_changed":false,
                "carrier_metadata_or_format_only":false,
                "change_scopes":[],
                "explanation_code":"carrier_projection_matches_native_materialization_receipt",
                "comparison_authority":"native_materialization_receipt",
                "comparison_scope":receipt.scope,
                "generation_sidecar_path":receipt.sidecar_path,
                "generated_sha256":receipt.expected_sha256,
                "current_sha256":current_sha256,
                "generated_byte_size":current_content.len(),
                "current_byte_size":current_content.len(),
                "added":[],
                "removed":[],
                "changed":[],
                "unchanged":unchanged.clone(),
                "added_count":0,
                "removed_count":0,
                "changed_count":0,
                "server_changed_count":0,
                "count_semantics":"added_removed_changed_counts_cover_server_definitions_only",
                "server_changes":{"added":[],"removed":[],"changed":[],"unchanged":unchanged,"added_count":0,"removed_count":0,"changed_count":0},
                "runtime_contract_version":plan["runtime_contract_version"],
                "materialization_validation":plan["materialization_validation"]
            }));
        }
    }
    let mut added = vec![];
    let mut removed = vec![];
    let mut changed = vec![];
    let mut unchanged = vec![];
    for (key, generated) in &generated_servers {
        match current_servers.get(key) {
            None => added.push(key.clone()),
            Some(current) if canonical_json(generated) != canonical_json(current) => {
                changed.push(key.clone())
            }
            Some(_) => unchanged.push(key.clone()),
        }
    }
    for key in current_servers.keys() {
        if !generated_servers.contains_key(key) {
            removed.push(key.clone())
        }
    }
    let current_exists = current_content.is_some();
    let projection_changed = current_content.as_deref() != Some(generated_content);
    let server_projection_changed = !added.is_empty() || !removed.is_empty() || !changed.is_empty();
    let metadata_only = current_exists && projection_changed && !server_projection_changed;
    let change_scopes = if !current_exists {
        json!(["full_projection_missing"])
    } else if !projection_changed {
        json!([])
    } else if server_projection_changed {
        json!(["full_projection", "server_definitions"])
    } else {
        json!(["full_projection", "carrier_metadata_or_format"])
    };
    let result = json!({
        "schema":"narada.registrar.carrier_projection_diff.v1",
        "status":if !current_exists{"missing"}else if projection_changed{"diff"}else{"clean"},
        "carrier_id":carrier_id,
        "config_path":config_path,
        "current_exists":current_exists,
        "projection_changed":projection_changed,
        "server_projection_changed":server_projection_changed,
        "carrier_metadata_or_format_only":metadata_only,
        "change_scopes":change_scopes,
        "explanation_code":if !current_exists{"carrier_projection_missing"}else if !projection_changed{"carrier_projection_exact_match"}else if metadata_only{"carrier_metadata_or_format_changed_without_server_definition_change"}else{"carrier_server_definition_change"},
        "generated_sha256":sha256_text(generated_content),
        "current_sha256":current_content.as_deref().map(sha256_text),
        "generated_byte_size":generated_content.len(),
        "current_byte_size":current_content.as_ref().map(|value|value.len()),
        "added":added,
        "removed":removed,
        "changed":changed,
        "unchanged":unchanged,
        "added_count":added.len(),
        "removed_count":removed.len(),
        "changed_count":changed.len(),
        "server_changed_count":changed.len(),
        "count_semantics":"added_removed_changed_counts_cover_server_definitions_only",
        "server_changes":{"added":added,"removed":removed,"changed":changed,"unchanged":unchanged,"added_count":added.len(),"removed_count":removed.len(),"changed_count":changed.len()},
        "runtime_contract_version":plan["runtime_contract_version"],
        "materialization_validation":plan["materialization_validation"]
    });
    Ok(result)
}

struct NativeMaterializationReceipt {
    sidecar_path: String,
    expected_sha256: String,
    scope: String,
    selectors: Vec<String>,
}

impl NativeMaterializationReceipt {
    fn matches(&self, kind: &str, content: &[u8]) -> bool {
        if self.scope == "whole_document" {
            return format!("{:x}", Sha256::digest(content)) == self.expected_sha256;
        }
        describe_config(kind, content, &self.selectors)
            .ok()
            .is_some_and(|description| {
                description.managed_projection.sha256 == self.expected_sha256
            })
    }
}

fn native_materialization_receipt(
    config_path: &str,
    carrier_id: &str,
) -> Option<NativeMaterializationReceipt> {
    let sidecar_path = format!("{config_path}.narada-generation.json");
    let sidecar: Value = serde_json::from_str(&fs::read_to_string(&sidecar_path).ok()?).ok()?;
    if sidecar.get("carrier_id").and_then(Value::as_str) != Some(carrier_id)
        || sidecar
            .get("config_path")
            .and_then(Value::as_str)
            .is_none_or(|declared| comparable_path(declared) != comparable_path(config_path))
    {
        return None;
    }
    let scope = sidecar
        .pointer("/managed_projection/scope")
        .and_then(Value::as_str)?
        .to_string();
    let expected_sha256 = sidecar
        .pointer(if scope == "whole_document" {
            "/config_artifact/bytes_sha256"
        } else {
            "/managed_projection/sha256"
        })
        .and_then(Value::as_str)?
        .to_string();
    let selectors = sidecar
        .pointer("/managed_projection/selectors")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|selector| selector.as_str().map(str::to_string))
        .collect::<Option<Vec<_>>>()?;
    Some(NativeMaterializationReceipt {
        sidecar_path,
        expected_sha256,
        scope,
        selectors,
    })
}

fn sha256_text(text: &str) -> String {
    format!("{:x}", Sha256::digest(text.as_bytes()))
}

