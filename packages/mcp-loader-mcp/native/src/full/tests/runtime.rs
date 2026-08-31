use crate::full::*;

#[test]
fn native_loader_freshness_ignores_workspace_metadata_mtime() {
    let state = LoaderState {
        policy: Policy {
            allowed_site_roots: Vec::new(),
            allowed_entrypoint_prefixes: Vec::new(),
            allowed_surface_ids: None,
            allowed_env_vars: Vec::new(),
            max_connections: 1,
            max_request_bytes: 1024,
            max_response_bytes: 1024,
            attach_timeout_ms: 1000,
            tool_call_timeout_ms: 1000,
            tool_call_grace_ms: 100,
        },
        surface_root: String::new(),
        workspace_root: env!("CARGO_MANIFEST_DIR").to_string(),
        // Freshness is anchored to the native Rust artifact graph, not legacy TS metadata.
        started_ms: 0,
        run_id: "test-loader".to_string(),
        owner_pid: 0,
        ownership_marker: "test-loader".to_string(),
        schema_lease_secret: "test-schema-lease-secret".to_string(),
        connections: std::collections::HashMap::new(),
        handles: std::collections::HashMap::new(),
        binding_admission: None,
        standalone_ambient_attachment: false,
    };
    let freshness = runtime_freshness(&state);
    assert_eq!(freshness["status"], "current");
    assert_eq!(freshness["reload_required"], false);
    assert_eq!(freshness["freshness_scope"], "native_loader_artifact");
    assert_eq!(freshness["reasons"], json!([]));
    assert_eq!(freshness["authority"], "native_rust");
    for file in freshness["source_files"].as_array().expect("source files") {
        let path = file["observation"]["path"].as_str().expect("source path");
        assert!(path.contains("native/src/"));
        assert!(!path.ends_with(".ts"));
        assert!(!path.ends_with(".js"));
    }
    for file in freshness["config_files"].as_array().expect("config files") {
        let path = file["observation"]["path"].as_str().expect("config path");
        assert!(!path.ends_with("pnpm-lock.yaml"));
        assert!(!path.ends_with(".ts"));
    }
}
