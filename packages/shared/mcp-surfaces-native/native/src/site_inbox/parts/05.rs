fn enum_arg(
    args: &Map<String, Value>,
    key: &str,
    default: Option<&str>,
    allowed: &[&str],
) -> Result<Option<String>, Value> {
    let value = if let Some(raw) = args.get(key) {
        raw.as_str().map(str::to_string).ok_or_else(|| {
            error(
                "invalid_request",
                &format!("{key}_must_be_one_of: {}", allowed.join(",")),
            )
        })?
    } else {
        default.map(str::to_string).unwrap_or_default()
    };
    if value.is_empty() {
        return Ok(None);
    }
    if !allowed.contains(&value.as_str()) {
        return Err(error(
            "invalid_request",
            &format!("{key}_must_be_one_of: {}", allowed.join(",")),
        ));
    }
    Ok(Some(value))
}
fn bounded(value: Option<&Value>, default: usize, maximum: usize) -> usize {
    value
        .and_then(Value::as_i64)
        .filter(|v| *v > 0)
        .map(|v| (v as usize).min(maximum))
        .unwrap_or(default)
}
fn error(code: &str, message: &str) -> Value {
    json!({"code":code,"message":message})
}
fn db_err(code: &str, cause: rusqlite::Error) -> Value {
    error(code, &format!("{code}:{cause}"))
}
fn now_iso() -> String {
    OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn submit_next_ack_show_roundtrip() {
        let root =
            std::env::temp_dir().join(format!("narada-site-inbox-native-{}", Uuid::new_v4()));
        fs::create_dir_all(&root).expect("root");
        let args: Map<String, Value> = serde_json::from_value(json!({
            "kind":"incident","title":"Native inbox test","principal":"native-test","payload":{"summary":"test"},"idempotency_key":"native-test-submit"
        })).expect("args");
        let submitted = submit(&args, &root).expect("submit");
        let id = submitted
            .get("envelope_id")
            .and_then(Value::as_str)
            .expect("id")
            .to_string();
        let replayed = submit(&args, &root).expect("submit replay");
        assert_eq!(replayed["status"], "replayed");
        assert_eq!(replayed["envelope_id"], id);
        let mut conflict_args = args.clone();
        conflict_args.insert("title".into(), json!("Different title"));
        assert_eq!(submit(&conflict_args, &root).expect_err("conflict")["code"], "inbox_idempotency_key_conflict");
        let next_value = next(&Map::new(), &root).expect("next");
        assert_eq!(next_value["status"], "ok");
        assert_eq!(next_value["envelope"]["envelope_id"], id);
        let ack_args: Map<String, Value> =
            serde_json::from_value(json!({"envelope_id":id,"principal":"native-test"}))
                .expect("ack args");
        let acked = disposition(&ack_args, &root, "acknowledged").expect("ack");
        assert_eq!(acked["status"], "acknowledged");
        assert_eq!(disposition(&ack_args, &root, "acknowledged").expect("ack replay")["idempotency_replay"], true);
        let show_args: Map<String, Value> =
            serde_json::from_value(json!({"envelope_id":id})).expect("show args");
        let shown = show(&show_args, &root).expect("show");
        assert_eq!(shown["envelope"]["status"], "acknowledged");
        assert_eq!(shown["envelope"]["payload"]["title"], "Native inbox test");
        assert_eq!(doctor(&root).expect("doctor")["storage_mode"], "native_sqlite");
        fs::remove_dir_all(&root).expect("cleanup");
    }

    #[test]
    fn output_ref_read_refuses_oversized_files() {
        let root = std::env::temp_dir().join(format!("narada-site-inbox-output-{}", Uuid::new_v4()));
        let directory = root.join(".ai/tmp/mcp-outputs/workspace");
        fs::create_dir_all(&directory).expect("directory");
        fs::write(directory.join("large.json"), vec![b'x'; MAX_OUTPUT_BYTES as usize + 1]).expect("output");
        let args: Map<String, Value> = serde_json::from_value(json!({"ref":"mcp_output:large"})).expect("args");
        let refused = output_show(&args, &root).expect_err("oversized output must refuse");
        assert_eq!(refused["code"], "output_ref_too_large");
        fs::remove_dir_all(&root).expect("cleanup");
    }
}
