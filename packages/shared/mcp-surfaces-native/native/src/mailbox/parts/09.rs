fn filter_message(v: &Value, args: &Map<String, Value>) -> bool {
    for (key, field) in [("mailbox_id", "mailbox_id"), ("folder", "folder"), ("thread_id", "thread_id")] {
        if let Some(filter) = args.get(key).and_then(Value::as_str) {
            if v.get(field).and_then(Value::as_str) != Some(filter) { return false; }
        }
    }
    if let Some(unread) = args.get("unread").and_then(Value::as_bool) {
        if v.get("unread").and_then(Value::as_bool) != Some(unread) { return false; }
    }
    let timestamp = v.get("received_at").and_then(Value::as_str).or_else(|| v.get("sent_at").and_then(Value::as_str)).unwrap_or("");
    if let Some(since) = args.get("since").and_then(Value::as_str) {
        if timestamp < since { return false; }
    }
    if let Some(before) = args.get("before").and_then(Value::as_str) {
        if timestamp >= before { return false; }
    }
    if let Some(query) = args.get("query").and_then(Value::as_str).filter(|value| !value.trim().is_empty()) {
        if !message_matches_query(v, query) { return false; }
    }
    true
}

fn message_matches_query(value: &Value, query: &str) -> bool {
    let mut haystack = String::new();
    for field in ["subject", "preview", "body_text"] {
        haystack.push_str(value.get(field).and_then(Value::as_str).unwrap_or(""));
        haystack.push('\n');
    }
    for field in ["from", "to", "cc", "categories"] {
        haystack.push_str(&serde_json::to_string(value.get(field).unwrap_or(&Value::Null)).unwrap_or_default());
        haystack.push('\n');
    }
    haystack.to_ascii_lowercase().contains(&query.to_ascii_lowercase())
}

fn summarize_message(value: &Value, include_body: bool, include_html: bool, include_raw: bool) -> Value {
    let mut summary = Map::new();
    for field in ["message_id", "mailbox_id", "folder", "thread_id", "subject", "from", "to", "cc", "received_at", "sent_at", "unread", "importance", "categories", "preview", "source_path"] {
        summary.insert(field.to_string(), value.get(field).cloned().unwrap_or(Value::Null));
    }
    summary.insert("attachments".to_string(), metadata_only_attachment(value.get("attachments").unwrap_or(&Value::Null)));
    if include_body { summary.insert("body_text".to_string(), value.get("body_text").cloned().unwrap_or(Value::Null)); }
    if include_html { summary.insert("body_html".to_string(), value.get("body_html").cloned().unwrap_or(Value::Null)); }
    if include_raw { summary.insert("raw".to_string(), value.get("raw").cloned().unwrap_or(Value::Null)); }
    Value::Object(summary)
}

fn metadata_only_attachment(value: &Value) -> Value {
    match value {
        Value::Array(values) => Value::Array(values.iter().take(100).map(metadata_only_attachment).collect()),
        Value::Object(object) => Value::Object(object.iter().take(64).filter_map(|(key, nested)| {
            let normalized = key.to_ascii_lowercase();
            if matches!(normalized.as_str(), "contentbytes" | "content_bytes" | "content_base64" | "contentref" | "content_ref" | "content" | "data" | "bytes" | "raw") {
                None
            } else {
                Some((key.clone(), metadata_only_attachment(nested)))
            }
        }).collect()),
        Value::String(value) => Value::String(value.chars().take(2048).collect()),
        _ => value.clone(),
    }
}

fn bounded_text(value:Option<String>,max:usize)->Option<String>{value.map(|text|text.chars().take(max).collect())}
fn normalized_source_timestamp(value:Option<String>)->Option<String>{value.and_then(|text|OffsetDateTime::parse(text.trim(),&Rfc3339).ok().map(|time|iso_millis(time.to_offset(UtcOffset::UTC))))}
fn bounded_projection(value:&Value,depth:usize)->Value{if depth>=6{return json!({"truncated":true,"reason":"depth_limit"});}match value{Value::String(text)=>Value::String(text.chars().take(2048).collect()),Value::Array(values)=>Value::Array(values.iter().take(64).map(|value|bounded_projection(value,depth+1)).collect()),Value::Object(object)=>Value::Object(object.iter().take(64).map(|(key,value)|(key.chars().take(128).collect(),bounded_projection(value,depth+1))).collect()),_=>value.clone()}}

fn message_key(value: &Value) -> String {
    format!("{}\u{0}{}", value.get("mailbox_id").and_then(Value::as_str).unwrap_or("default"), value.get("message_id").and_then(Value::as_str).unwrap_or(""))
}

fn source_preference(path: &str) -> u8 {
    let parts = path.replace('\\', "/").to_ascii_lowercase();
    if parts.split('/').any(|part| part == "messages") { 0 } else if parts.split('/').any(|part| part == "views") { 10 } else { 5 }
}
fn first_str(o:&Map<String,Value>,keys:&[&str])->Option<String>{keys.iter().find_map(|k|o.get(*k).and_then(|v|if let Some(s)=v.as_str(){Some(s.trim().to_string())}else if v.is_number(){Some(v.to_string())}else{None}).filter(|s|!s.is_empty()))}
fn normalize_text(s:&str)->String{s.replace("\r\n","\n").split_whitespace().collect::<Vec<_>>().join(" ").chars().take(5000).collect()}
fn as_array(v:Option<&Value>)->Vec<Value>{match v{Some(Value::Array(a))=>a.clone(),Some(v)=>vec![v.clone()],None=>Vec::new()}}
fn required(args:&Map<String,Value>,key:&str)->Result<String,Value>{args.get(key).and_then(Value::as_str).map(str::trim).filter(|v|!v.is_empty()).map(str::to_string).ok_or_else(||error(&format!("{key}_required"),&format!("{key}_required")))}
fn required_bounded(
    args: &Map<String, Value>,
    key: &str,
    code: &str,
    max: usize,
) -> Result<String, Value> {
    let value = args
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| error(code, code))?;
    if value.chars().count() > max {
        return Err(error(&format!("{code}_too_long"), &format!("{code}_too_long")));
    }
    Ok(value.to_string())
}

fn required_topics(value: Option<&Value>) -> Result<Vec<String>, Value> {
    let values = value
        .and_then(Value::as_array)
        .ok_or_else(|| error("mailbox_outbox_topics_required", "mailbox_outbox_topics_required"))?;
    if values.is_empty() || values.len() > 16 {
        return Err(error("mailbox_outbox_topics_required", "mailbox_outbox_topics_required"));
    }
    let mut topics = Vec::with_capacity(values.len());
    for value in values {
        let topic = value
            .as_str()
            .map(str::trim)
            .filter(|topic| !topic.is_empty())
            .ok_or_else(|| error("mailbox_outbox_topics_required", "mailbox_outbox_topics_required"))?;
        if topic.chars().count() > 256 {
            return Err(error(
                "mailbox_outbox_topics_required_too_long",
                "mailbox_outbox_topics_required_too_long",
            ));
        }
        topics.push(topic.to_string());
    }
    topics.sort();
    if topics.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err(error(
            "mailbox_outbox_topics_required_duplicate",
            "mailbox_outbox_topics_required_duplicate",
        ));
    }
    Ok(topics)
}

fn parsed_topics(value: Option<&str>, consumer_id: &str) -> Result<Vec<String>, Value> {
    let Some(value) = value else {
        let code = format!("mailbox_outbox_consumer_v2_registration_required:{consumer_id}");
        return Err(error(&code, &code));
    };
    let topics = serde_json::from_str::<Vec<String>>(value).map_err(|_| {
        let code = format!("mailbox_outbox_consumer_topics_invalid:{consumer_id}");
        error(&code, &code)
    })?;
    if topics.is_empty() {
        let code = format!("mailbox_outbox_consumer_topics_invalid:{consumer_id}");
        return Err(error(&code, &code));
    }
    Ok(topics)
}

fn required_timestamp(
    args: &Map<String, Value>,
    key: &str,
    code: &str,
) -> Result<String, Value> {
    let value = required_bounded(args, key, code, 64)?;
    let parsed = OffsetDateTime::parse(&value, &Rfc3339)
        .map_err(|_| error(&format!("{code}_invalid"), &format!("{code}_invalid")))?;
    Ok(iso_millis(parsed.to_offset(UtcOffset::UTC)))
}

fn bounded_integer(
    value: Option<&Value>,
    fallback: i64,
    min: i64,
    max: i64,
) -> Result<i64, Value> {
    let resolved = match value {
        None | Some(Value::Null) => fallback,
        Some(value) => value
            .as_i64()
            .ok_or_else(|| error("mailbox_integer_argument_invalid", "mailbox_integer_argument_invalid"))?,
    };
    if resolved < min || resolved > max {
        return Err(error("mailbox_integer_argument_invalid", "mailbox_integer_argument_invalid"));
    }
    Ok(resolved)
}

fn iso_millis(value: OffsetDateTime) -> String {
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

fn now_iso_millis() -> String {
    iso_millis(OffsetDateTime::now_utc())
}

fn canonical_json(value: &Value) -> String {
    match value {
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {
            serde_json::to_string(value).unwrap_or_else(|_| "null".to_string())
        }
        Value::Array(values) => format!(
            "[{}]",
            values.iter().map(canonical_json).collect::<Vec<_>>().join(",")
        ),
        Value::Object(object) => {
            let mut keys = object.keys().collect::<Vec<_>>();
            keys.sort();
            let entries = keys
                .into_iter()
                .map(|key| {
                    format!(
                        "{}:{}",
                        serde_json::to_string(key).unwrap_or_else(|_| "\"\"".to_string()),
                        canonical_json(object.get(key).unwrap_or(&Value::Null))
                    )
                })
                .collect::<Vec<_>>();
            format!("{{{}}}", entries.join(","))
        }
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
fn is_within(path:&Path,root:&Path)->bool{let p=path.canonicalize().unwrap_or_else(|_|path.to_path_buf());let r=root.canonicalize().unwrap_or_else(|_|root.to_path_buf());p==r||p.starts_with(&r)}
fn read_bounded(path:&Path)->Result<Value,Value>{if fs::metadata(path).map_err(|_|error("output_ref_not_found","output_ref_not_found"))?.len()>MAX_BYTES{return Err(error("output_ref_too_large","output_ref_too_large"));}let text=fs::read_to_string(path).map_err(|_|error("output_ref_read_failed","output_ref_read_failed"))?;serde_json::from_str(&text).map_err(|_|error("output_ref_invalid_json","output_ref_invalid_json"))}
fn error(code:&str,message:&str)->Value{json!({"schema":"narada.mailbox.error.v1","code":code,"message":message})}
fn schema(name: &str) -> Value {
    match name {
        "mailbox_messages_list" | "mailbox_search" => json!({"type":"object","properties":{
            "mailbox_id":{"type":"string","maxLength":512},"folder":{"type":"string","maxLength":512},"unread":{"type":"boolean"},
            "since":{"type":"string","maxLength":64,"format":"date-time"},"before":{"type":"string","maxLength":64,"format":"date-time"},"query":{"type":"string","maxLength":4096},
            "offset":{"type":"integer","minimum":0,"maximum":1000000},"limit":{"type":"integer","minimum":1,"maximum":100},"include_body":{"type":"boolean"}
        },"additionalProperties":false}),
        "mailbox_message_show" => json!({"type":"object","properties":{
            "message_id":{"type":"string","maxLength":1024},"mailbox_id":{"type":"string","maxLength":512},
            "include_html":{"type":"boolean"},"include_raw":{"type":"boolean"}
        },"required":["message_id"],"additionalProperties":false}),
        "mailbox_thread_show" => json!({"type":"object","properties":{
            "thread_id":{"type":"string","maxLength":1024},"mailbox_id":{"type":"string","maxLength":512},
            "offset":{"type":"integer","minimum":0,"maximum":1000000},"limit":{"type":"integer","minimum":1,"maximum":100},"include_body":{"type":"boolean"}
        },"required":["thread_id"],"additionalProperties":false}),
        "mailbox_generation_show" => json!({"type":"object","properties":{"generation_id":{"type":"string"},"offset":{"type":"integer","minimum":0,"maximum":1000000},"limit":{"type":"integer","minimum":1,"maximum":100}},"required":["generation_id"],"additionalProperties":false}),
        "mailbox_admission_show" => json!({"type":"object","properties":{"scope_id":{"type":"string"},"fact_id":{"type":"string"}},"required":["scope_id","fact_id"],"additionalProperties":false}),
        "mailbox_fact_show" => json!({"type":"object","properties":{
            "fact_id":{"type":"string"},"scope_id":{"type":"string"},"config_path":{"type":"string"},"include_content":{"type":"boolean","default":false}
        },"required":["fact_id"],"additionalProperties":false}),
        "mailbox_message_fact_find" => json!({"type":"object","properties":{"scope_id":{"type":"string"},"message_id":{"type":"string"}},"required":["scope_id","message_id"],"additionalProperties":false}),
        "mailbox_outbox_consumer_show" => json!({"type":"object","properties":{"consumer_id":{"type":"string"}},"required":["consumer_id"],"additionalProperties":false}),
        "mailbox_outbox_list" => json!({"type":"object","properties":{"consumer_id":{"type":"string"},"limit":{"type":"integer","minimum":1,"maximum":100}},"required":["consumer_id"],"additionalProperties":false}),
        "mailbox_outbox_consumer_register" => json!({"type":"object","properties":{
            "consumer_id":{"type":"string"},"scope_id":{"type":"string"},
            "topics":{"type":"array","minItems":1,"maxItems":16,"items":{"type":"string"}},
            "start_at":{"type":"string"}
        },"required":["consumer_id","scope_id","topics","start_at"],"additionalProperties":false}),
        "mailbox_outbox_ack" => json!({"type":"object","properties":{
            "consumer_id":{"type":"string"},"event_id":{"type":"string"},
            "receipt":{"type":"object","additionalProperties":false,"required":["schema","outcome","effect_ref"],"properties":{
                "schema":{"type":"string"},"outcome":{"type":"string"},"effect_ref":{"type":"string"}
            }}
        },"required":["consumer_id","event_id","receipt"],"additionalProperties":false}),
        "mailbox_sync_generation" => json!({"type":"object","properties":{
            "idempotency_key":{"type":"string"},"scope_id":{"type":"string"},"config_path":{"type":"string"},"timeout_ms":{"type":"integer","minimum":100,"maximum":60000}
        },"required":["idempotency_key"],"additionalProperties":false}),
        "mailbox_reconcile_first_observations" => json!({"type":"object","properties":{
            "idempotency_key":{"type":"string"},"generation_id":{"type":"string"},"scope_id":{"type":"string"},
            "config_path":{"type":"string"},"limit":{"type":"integer","minimum":1,"maximum":100}
        },"required":["idempotency_key","generation_id"],"additionalProperties":false}),
        "mailbox_message_admit" => json!({"type":"object","properties":{
            "idempotency_key":{"type":"string"},"fact_id":{"type":"string"},"source_event_id":{"type":"string"},
            "scope_id":{"type":"string"},"policy_version":{"type":"string"},"config_path":{"type":"string"}
        },"required":["idempotency_key","fact_id","source_event_id"],"additionalProperties":false}),
        "mailbox_output_show" => json!({"type":"object","properties":{"ref":{"type":"string"},"output_ref":{"type":"string"},"offset":{"type":"integer","minimum":0,"maximum":1000000},"limit":{"type":"integer","minimum":1,"maximum":10000}},"additionalProperties":false}),
        _ => json!({"type":"object","additionalProperties":false}),
    }
}
fn tool(name:&str,description:&str,schema:Value,read_only:bool)->Value{json!({"name":name,"description":description,"inputSchema":schema,"annotations":{"title":name,"readOnlyHint":read_only,"destructiveHint":!read_only,"idempotentHint":read_only,"openWorldHint":false},"outputSchema":{"type":"object","additionalProperties":true}})}

