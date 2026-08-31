use super::*;

#[test]
fn modern_loader_results_are_self_describing() {
    let params = modern_request_params();
    assert!(is_modern_request(&params));
    assert!(validate_modern_request(&params).is_ok());
    let result = modernize_result(json!({"tools": []}), "tools/list");
    assert_eq!(result["resultType"], "complete");
    assert_eq!(result["cacheScope"], "public");
    assert!(result["ttlMs"].as_u64().unwrap_or_default() > 0);
    assert_eq!(
        result["_meta"]["io.modelcontextprotocol/serverInfo"]["name"],
        SERVER_NAME
    );
    let discovery = modernize_result(modern_discover_result(), "server/discover");
    assert_eq!(discovery["supportedVersions"][0], MODERN_PROTOCOL_VERSION);
    assert!(modern_discovery_is_valid(&discovery));
}

#[test]
fn modern_loader_requests_require_client_metadata() {
    let missing =
        json!({"_meta": {"io.modelcontextprotocol/protocolVersion": MODERN_PROTOCOL_VERSION}});
    let error = validate_modern_request(&missing).expect_err("missing metadata must refuse");
    assert_eq!(error.code, "modern_metadata_required");
}

fn payload_test_policy(roots: Vec<String>) -> Policy {
    Policy {
        allowed_site_roots: roots,
        allowed_entrypoint_prefixes: Vec::new(),
        allowed_surface_ids: None,
        allowed_env_vars: Vec::new(),
        max_connections: 1,
        max_request_bytes: 1024 * 1024,
        max_response_bytes: 4 * 1024 * 1024,
        attach_timeout_ms: 1000,
        tool_call_timeout_ms: 1000,
        tool_call_grace_ms: 100,
    }
}

fn write_payload(root: &std::path::Path, reference: &str, payload: Value) {
    let body = reference.trim_start_matches("mcp_payload:");
    let (id, revision) = body.rsplit_once("@v").unwrap();
    let canonical = stable_json(&payload);
    let record = json!({
        "schema":"narada.mcp_payload.revision.v1","ref":reference,"payload_id":id,
        "revision":revision.parse::<u64>().unwrap(),"sha256":sha256(&canonical),
        "byte_size":canonical.len(),"payload":payload
    });
    let directory = root.join(".ai/tmp/mcp-payloads/workspace").join(id);
    fs::create_dir_all(&directory).unwrap();
    fs::write(
        directory.join(format!("v{revision}.json")),
        stable_json(&record),
    )
    .unwrap();
}

#[test]
fn payload_ref_is_staged_from_one_admitted_site_into_target_site() {
    let base = std::env::temp_dir().join(format!("narada-loader-payload-{}", std::process::id()));
    let source = base.join("source");
    let target = base.join("target");
    fs::create_dir_all(&target).unwrap();
    let reference = "mcp_payload:cross-site-test@v1";
    write_payload(
        &source,
        reference,
        json!({"operations":[{"op":"entity.declare"}]}),
    );
    let policy = payload_test_policy(vec![
        source.to_string_lossy().to_string(),
        target.to_string_lossy().to_string(),
    ]);
    let result = stage_admitted_payload_ref(
        &target.to_string_lossy(),
        &json!({"payload_ref":reference}),
        &policy,
    )
    .unwrap()
    .unwrap();
    assert_eq!(result["status"], "staged_from_admitted_site");
    assert!(target
        .join(".ai/tmp/mcp-payloads/workspace/cross-site-test/v1.json")
        .is_file());
    fs::remove_dir_all(base).ok();
}

#[test]
fn payload_ref_refuses_divergent_admitted_site_collision() {
    let base = std::env::temp_dir().join(format!(
        "narada-loader-payload-collision-{}",
        std::process::id()
    ));
    let left = base.join("left");
    let right = base.join("right");
    let target = base.join("target");
    fs::create_dir_all(&target).unwrap();
    let reference = "mcp_payload:collision-test@v1";
    write_payload(&left, reference, json!({"value":"left"}));
    write_payload(&right, reference, json!({"value":"right"}));
    let policy = payload_test_policy(vec![
        left.to_string_lossy().to_string(),
        right.to_string_lossy().to_string(),
    ]);
    let error = stage_admitted_payload_ref(
        &target.to_string_lossy(),
        &json!({"payload_ref":reference}),
        &policy,
    )
    .unwrap_err();
    assert_eq!(error.code, "payload_ref_admitted_site_collision");
    fs::remove_dir_all(base).ok();
}
