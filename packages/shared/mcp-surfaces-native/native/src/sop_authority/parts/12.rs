fn parse_json_member(
    object: &Map<String, Value>,
    key: &str,
    fallback: Value,
) -> Result<Value, Value> {
    let Some(value) = object.get(key) else {
        return Ok(fallback);
    };
    if value.is_null() {
        return Ok(fallback);
    }
    let text = value.as_str().unwrap_or("");
    if text.is_empty() {
        return Ok(fallback);
    }
    serde_json::from_str(text).map_err(|error| {
        diagnostic(
            "sop_persisted_value_invalid",
            &error.to_string(),
            json!({"field":key}),
        )
    })
}

fn parse_nullable_member(object: &Map<String, Value>, key: &str) -> Result<Option<Value>, Value> {
    let Some(value) = object.get(key) else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    let text = value.as_str().unwrap_or("");
    if text.is_empty() {
        return Ok(None);
    }
    serde_json::from_str(text).map(Some).map_err(|error| {
        diagnostic(
            "sop_persisted_value_invalid",
            &error.to_string(),
            json!({"field":key}),
        )
    })
}

fn nullable_member<'a>(object: &'a Map<String, Value>, key: &str) -> Option<&'a Value> {
    object.get(key).filter(|value| !value.is_null())
}

fn text_member(object: &Map<String, Value>, key: &str) -> String {
    object
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

fn assert_template_bound(value: &Value) -> Result<(), Value> {
    assert_bound(
        value,
        "sop_template_definition",
        MAX_TEMPLATE_DEFINITION_BYTES,
    )
}

pub(crate) fn assert_bound(value: &Value, field: &str, max: usize) -> Result<(), Value> {
    let bytes = canonical_json(value).as_bytes().len();
    if bytes > max {
        return Err(diagnostic(
            &format!("{field}_too_large"),
            &format!("{field}_too_large"),
            json!({"field":field,"byte_length":bytes,"max_bytes":max}),
        ));
    }
    Ok(())
}

fn encode(value: &Value) -> Result<String, Value> {
    serde_json::to_string(value)
        .map_err(|error| diagnostic("sop_json_encode_failed", &error.to_string(), json!({})))
}

fn encode_optional(value: Option<&Value>) -> Result<Option<String>, Value> {
    value.map(encode).transpose()
}

pub(crate) fn canonical_json(value: &Value) -> String {
    match value {
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {
            serde_json::to_string(value).unwrap_or_else(|_| "null".to_string())
        }
        Value::Array(values) => format!(
            "[{}]",
            values
                .iter()
                .map(canonical_json)
                .collect::<Vec<_>>()
                .join(",")
        ),
        Value::Object(object) => {
            let mut keys = object.keys().collect::<Vec<_>>();
            keys.sort();
            format!(
                "{{{}}}",
                keys.into_iter()
                    .map(|key| format!(
                        "{}:{}",
                        serde_json::to_string(key).unwrap_or_else(|_| "\"\"".to_string()),
                        canonical_json(object.get(key).unwrap_or(&Value::Null))
                    ))
                    .collect::<Vec<_>>()
                    .join(",")
            )
        }
    }
}

#[allow(dead_code)]
pub(crate) fn fingerprint(value: &Value) -> String {
    Sha256::digest(canonical_json(value).as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[allow(dead_code)]
pub(crate) fn deterministic_id(prefix: &str, value: &str) -> String {
    let digest = Sha256::digest(value.as_bytes());
    let hex = digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    format!("{prefix}{}", &hex[..24])
}

pub(crate) fn now_iso() -> String {
    let value = OffsetDateTime::now_utc();
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        value.year(),
        u8::from(value.month()),
        value.day(),
        value.hour(),
        value.minute(),
        value.second(),
        value.nanosecond() / 1_000_000
    )
}

fn valid_step_id(value: &str) -> bool {
    let mut characters = value.chars();
    characters
        .next()
        .map(|character| character.is_ascii_alphanumeric())
        .unwrap_or(false)
        && characters.all(|character| {
            character.is_ascii_alphanumeric() || character == '_' || character == '-'
        })
}

fn valid_identifier(value: &str) -> bool {
    let mut characters = value.chars();
    characters
        .next()
        .map(|character| character.is_ascii_alphabetic() || character == '_')
        .unwrap_or(false)
        && characters.all(|character| character.is_ascii_alphanumeric() || character == '_')
}

pub(crate) fn diagnostic(code: &str, message: &str, details: Value) -> Value {
    json!({"schema":"narada.sop.error.v1","code":code,"message":message,"details":details})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claim_and_advance_completes_without_cross_call_lease_threading() {
        let root = std::env::temp_dir().join(format!("narada-sop-compound-{}", Uuid::new_v4()));
        template_create(
            json!({
                "sop_id":"compound-demo","title":"Compound demo","steps":[{
                    "id":"manual","executor":"agent","title":"Manual","instructions":"Complete"
                }]
            })
            .as_object()
            .unwrap(),
            &root,
        )
        .expect("create");
        template_update(
            json!({"sop_id":"compound-demo","status":"active"})
                .as_object()
                .unwrap(),
            &root,
        )
        .expect("activate");
        crate::sop_engine::call_tool(
            "sop_run_start",
            json!({
                "sop_id":"compound-demo","occurrence_key":"occ-1",
                "triggered_by":"test","input":{}
            })
            .as_object()
            .unwrap(),
            &root,
        )
        .expect("start");

        let completed = handoff_claim_and_advance(
            json!({
                "consumer_id":"test-agent","completion_key":"completion-1",
                "outcome":"completed","result":{"answer":42},"principal":"agent:test"
            })
            .as_object()
            .unwrap(),
            &root,
        )
        .expect("claim and advance");
        assert_eq!(completed["status"], "advanced");
        assert_eq!(completed["advanced"], true);
        assert_eq!(completed["result"]["status"], "completed");
        assert_eq!(completed["claim"]["lease_ms"], 60_000);
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn template_registry_mutations_are_versioned_and_bounded() {
        let root = std::env::temp_dir().join(format!("narada-sop-authority-{}", Uuid::new_v4()));
        let create = template_create(
            json!({
                "sop_id":"demo","title":"Demo","steps":[{
                    "id":"first","executor":"engine","title":"First","instructions":"Record input"
                }]
            })
            .as_object()
            .unwrap(),
            &root,
        )
        .expect("create");
        assert_eq!(create["version"], 1);
        let update = template_update(
            json!({"sop_id":"demo","title":"Demo v2","status":"active"})
                .as_object()
                .unwrap(),
            &root,
        )
        .expect("update");
        assert_eq!(update["version"], 2);
        let deprecated = template_deprecate(
            json!({"sop_id":"demo","reason":"fixture"})
                .as_object()
                .unwrap(),
            &root,
        )
        .expect("deprecate");
        assert_eq!(deprecated["status"], "deprecated");
        let removed = template_unimport(
            json!({"sop_id":"demo","version":1,"reason":"fixture cleanup","principal":"test"})
                .as_object()
                .unwrap(),
            &root,
        )
        .expect("unimport");
        assert_eq!(removed["remaining_versions"], json!([2]));
        fs::remove_dir_all(root).expect("cleanup");
    }
}
