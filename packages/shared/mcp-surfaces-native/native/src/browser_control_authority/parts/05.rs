fn materialize_output(root: &Path, tool_name: &str, full_output: Value) -> Result<Value, Value> {
    let id = Uuid::new_v4().simple().to_string();
    let reference = format!("mcp_output:{id}");
    let directory = root.join(".ai/tmp/mcp-outputs/workspace");
    fs::create_dir_all(&directory).map_err(|cause| {
        error(
            "output_directory_create_failed",
            &cause.to_string(),
            json!({"path":directory}),
        )
    })?;
    let presentation =
        serde_json::to_string_pretty(&full_output).unwrap_or_else(|_| full_output.to_string());
    let record = json!({"schema":"narada.mcp_output_ref.v1","ref":reference,"output_id":id,"tool_name":tool_name,"full_output_char_length":presentation.chars().count(),"truncated":false,"full_output":full_output});
    let encoded = serde_json::to_vec(&record)
        .map_err(|cause| error("output_encode_failed", &cause.to_string(), json!({})))?;
    if encoded.len() > 10 * 1024 * 1024 {
        return Err(error(
            "output_ref_too_large",
            "Materialized output exceeds the 10 MiB store limit.",
            json!({"byte_length":encoded.len()}),
        ));
    }
    let path = directory.join(format!("{id}.json"));
    fs::write(&path, encoded).map_err(|cause| {
        error(
            "output_write_failed",
            &cause.to_string(),
            json!({"path":path}),
        )
    })?;
    Ok(
        json!({"schema":"narada.mcp_output_preview.v1","status":"ok","tool_name":tool_name,"ref":reference,"output_ref":reference,"full_output_char_length":presentation.chars().count(),"output_truncated":true,"remediation":"Call mcp_output_show with output_ref and bounded offset/limit."}),
    )
}
fn receipt(root: &Path, operation: &str, details: &Value) -> Result<(), Value> {
    let path = root.join(".ai/tmp/browser-control/action-receipts.jsonl");
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|c| {
            error(
                "browser_receipt_directory_failed",
                &c.to_string(),
                json!({"path":parent}),
            )
        })?;
    }
    let line=serde_json::to_string(&json!({"schema":"narada.browser_control.action_receipt.v1","receipt_id":format!("browser-receipt-{}",Uuid::new_v4()),"operation":operation,"recorded_at":now(),"details":details})).unwrap_or_default();
    if line.len() > 65_536 {
        return Err(error(
            "browser_receipt_too_large",
            "Browser action receipt exceeds 64 KiB.",
            json!({}),
        ));
    }
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|c| {
            error(
                "browser_receipt_open_failed",
                &c.to_string(),
                json!({"path":path}),
            )
        })?;
    writeln!(file, "{line}").map_err(|c| {
        error(
            "browser_receipt_write_failed",
            &c.to_string(),
            json!({"path":path}),
        )
    })
}
fn now() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".into())
}
fn empty() -> Value {
    json!({"type":"object","properties":{},"additionalProperties":false})
}
fn object(properties: Map<String, Value>, required: &[&str]) -> Value {
    json!({"type":"object","properties":properties,"required":required,"additionalProperties":false})
}
fn tool(name: &str, description: &str, input: Value, read_only: bool) -> Value {
    json!({"name":name,"description":description,"inputSchema":input,"annotations":{"title":name,"readOnlyHint":read_only,"destructiveHint":!read_only,"idempotentHint":name!="browser_control_attach","openWorldHint":false},"outputSchema":{"type":"object","additionalProperties":true}})
}
fn result(operation: &str, value: Value) -> Value {
    json!({"schema":"narada.browser_control.result.v1","status":"ok","operation":operation,"result":value})
}
fn error(code: &str, message: &str, details: Value) -> Value {
    json!({"schema":"narada.browser_control.error.v1","status":"unavailable","code":code,"message":message,"details":details})
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn catalog_is_exact_closed_and_bounded() {
        let tools = list_tools();
        assert_eq!(tools.len(), 13);
        for t in tools {
            let s = &t["inputSchema"];
            assert!(s.get("title").is_none());
            assert_eq!(s["additionalProperties"], false);
        }
    }
    #[test]
    fn origins_are_exact_and_credentials_are_refused() {
        assert_eq!(
            normalize_origins(Some(&json!(["https://example.com"]))).unwrap(),
            vec!["https://example.com"]
        );
        assert!(normalize_origins(Some(&json!(["https://example.com/path"]))).is_err());
        assert!(validate_http_endpoint("http://user@127.0.0.1:9222").is_err());
        assert!(validate_http_endpoint("http://example.com:9222").is_err());
    }
    #[test]
    fn sensitive_fields_are_refused() {
        assert!(refuse_sensitive(
            "#password",
            &json!({"nodeName":"INPUT","attributes":["type","text"]})
        )
        .is_err());
        assert!(refuse_sensitive(
            "#name",
            &json!({"nodeName":"INPUT","attributes":["type","text"]})
        )
        .is_ok());
    }
    #[test]
    fn oversized_output_is_materialized_and_pageable() {
        let root = std::env::temp_dir().join(format!("narada-browser-output-{}", Uuid::new_v4()));
        let preview = materialize_output(
            &root,
            "browser_control_screenshot",
            json!({"data_base64":"a".repeat(70_000)}),
        )
        .unwrap();
        let page = super::super::host_contracts::output_show(
            &Map::from_iter([
                ("output_ref".to_string(), preview["output_ref"].clone()),
                ("limit".to_string(), json!(100)),
            ]),
            &root,
        )
        .unwrap();
        assert_eq!(page["output_truncated"], true);
        assert_eq!(page["limit"], 100);
        let _ = fs::remove_dir_all(root);
    }
}
