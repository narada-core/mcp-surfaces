fn normalize_handoff_status(value: &str) -> Result<String, Value> {
    if !matches!(
        value,
        "pending" | "leased" | "completed" | "failed" | "cancelled"
    ) {
        return Err(diagnostic(
            "sop_handoff_status_invalid",
            "sop_handoff_status_invalid",
            json!({"status":value}),
        ));
    }
    Ok(value.to_string())
}

fn normalize_outbox_topic(value: &str) -> Result<String, Value> {
    let topic = value.trim();
    if topic.is_empty() {
        return Err(diagnostic(
            "sop_outbox_topic_required",
            "sop_outbox_topic_required",
            json!({}),
        ));
    }
    if topic.chars().count() > 256 {
        return Err(diagnostic(
            "sop_outbox_topic_required_too_long",
            "sop_outbox_topic_required_too_long",
            json!({"max_length":256}),
        ));
    }
    if topic != SOP_TERMINAL_TOPIC {
        return Err(diagnostic(
            "sop_outbox_topic_unsupported",
            "sop_outbox_topic_unsupported",
            json!({"topic":topic,"allowed":[SOP_TERMINAL_TOPIC]}),
        ));
    }
    Ok(topic.to_string())
}

fn bounded_integer_arg(
    value: Option<&Value>,
    default: i64,
    min: i64,
    max: i64,
    code: &str,
) -> Result<i64, Value> {
    let parsed = match value {
        None | Some(Value::Null) => default,
        Some(value) => value
            .as_i64()
            .ok_or_else(|| diagnostic(code, code, json!({"value":value,"min":min,"max":max})))?,
    };
    if parsed < min || parsed > max {
        return Err(diagnostic(
            code,
            code,
            json!({"value":value,"min":min,"max":max}),
        ));
    }
    Ok(parsed)
}

fn positive_integer_member(value: Option<&Value>, code: &str) -> Result<i64, Value> {
    let parsed = value
        .and_then(Value::as_i64)
        .ok_or_else(|| diagnostic(code, code, json!({})))?;
    if parsed < 1 {
        return Err(diagnostic(code, code, json!({"value":parsed,"min":1})));
    }
    Ok(parsed)
}

fn nonnegative_integer_member(value: Option<&Value>, code: &str) -> Result<i64, Value> {
    let parsed = value
        .and_then(Value::as_i64)
        .ok_or_else(|| diagnostic(code, code, json!({})))?;
    if parsed < 0 {
        return Err(diagnostic(code, code, json!({"value":parsed,"min":0})));
    }
    Ok(parsed)
}

pub(crate) fn optional_bounded_string(
    value: Option<&Value>,
    code: &str,
    max: usize,
) -> Result<Option<String>, Value> {
    let Some(text) = optional_string(value) else {
        return Ok(None);
    };
    if text.chars().count() > max {
        return Err(diagnostic(
            code,
            code,
            json!({"length":text.chars().count(),"max_length":max}),
        ));
    }
    Ok(Some(text))
}

fn normalize_timestamp(value: &str, code: &str) -> Result<String, Value> {
    parse_iso(value)
        .map(format_iso)
        .ok_or_else(|| diagnostic(code, code, json!({"value":value})))
}

pub(crate) fn parse_iso(value: &str) -> Option<OffsetDateTime> {
    OffsetDateTime::parse(value.trim(), &Rfc3339)
        .ok()
        .map(|timestamp| timestamp.to_offset(UtcOffset::UTC))
}

pub(crate) fn format_iso(value: OffsetDateTime) -> String {
    let value = value.to_offset(UtcOffset::UTC);
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

#[allow(clippy::too_many_arguments)]
fn insert_template(
    db: &Connection,
    sop_id: &str,
    version: i64,
    title: &str,
    status: &str,
    description: &str,
    steps: &Value,
    trigger_kind: &str,
    input_schema: Option<&Value>,
    output: Option<&Value>,
    output_ref: Option<&Value>,
    output_schema: Option<&Value>,
    acceptance: &[Value],
    evidence: &[Value],
    now: &str,
) -> Result<(), Value> {
    db.execute(
        "INSERT INTO sop_templates (sop_id,version,title,status,description,steps_json,trigger_kind,input_schema_json,output_mapping_json,output_ref_mapping_json,output_schema_json,acceptance_criteria_json,evidence_requirements_json,created_at,updated_at) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
        params![
            sop_id, version, title, status, description, encode(steps)?, trigger_kind,
            encode_optional(input_schema)?, encode_optional(output)?, encode_optional(output_ref)?,
            encode_optional(output_schema)?, encode(&Value::Array(acceptance.to_vec()))?,
            encode(&Value::Array(evidence.to_vec()))?, now, now
        ],
    )
    .map_err(|error| diagnostic("sop_template_insert_failed", &error.to_string(), json!({"sop_id":sop_id,"version":version})))?;
    Ok(())
}

fn latest_template(db: &Connection, sop_id: &str) -> Result<Option<Value>, Value> {
    query_template(
        db,
        "SELECT * FROM sop_templates WHERE sop_id = ? ORDER BY version DESC LIMIT 1",
        params![sop_id],
    )
}

fn template_by_version(
    db: &Connection,
    sop_id: &str,
    version: i64,
) -> Result<Option<Value>, Value> {
    query_template(
        db,
        "SELECT * FROM sop_templates WHERE sop_id = ? AND version = ? LIMIT 1",
        params![sop_id, version],
    )
}

fn query_template<P: rusqlite::Params>(
    db: &Connection,
    sql: &str,
    params: P,
) -> Result<Option<Value>, Value> {
    db.query_row(sql, params, row_value)
        .optional()
        .map_err(|error| diagnostic("sop_template_query_failed", &error.to_string(), json!({})))
}

fn row_value(row: &rusqlite::Row<'_>) -> rusqlite::Result<Value> {
    let mut object = Map::new();
    for index in 0..row.as_ref().column_count() {
        let name = row
            .as_ref()
            .column_name(index)
            .unwrap_or("column")
            .to_string();
        let value = match row.get_ref(index)? {
            rusqlite::types::ValueRef::Null => Value::Null,
            rusqlite::types::ValueRef::Integer(value) => json!(value),
            rusqlite::types::ValueRef::Real(value) => json!(value),
            rusqlite::types::ValueRef::Text(value) => {
                Value::String(String::from_utf8_lossy(value).to_string())
            }
            rusqlite::types::ValueRef::Blob(value) => json!({"byte_length":value.len()}),
        };
        object.insert(name, value);
    }
    Ok(Value::Object(object))
}

