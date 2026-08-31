use crate::full::*;

#[test]
fn unavailable_handle_recovery_is_executable_only_with_a_canonical_binding() {
    let handle = SurfaceHandle {
        handle: "msh_test".to_string(),
        logical_connection_id: "logical-test".to_string(),
        binding_id: Some("marici-git".to_string()),
        site_root: "C:/Users/andrey/src/marici".to_string(),
        surface_id: "git".to_string(),
        runtime_kind: Some("native".to_string()),
        created_at: "2026-08-14T00:00:00Z".to_string(),
    };
    let recovery = unavailable_handle_recovery(&handle);
    assert_eq!(recovery["tool_name"], "mcp_loader_resume_or_open_surface");
    assert_eq!(recovery["arguments"]["binding_id"], "marici-git");

    let legacy = SurfaceHandle {
        binding_id: None,
        ..handle
    };
    let unavailable = unavailable_handle_recovery(&legacy);
    assert_eq!(unavailable["status"], "unavailable");
    assert_eq!(
        unavailable["reason"],
        "surface_handle_binding_id_unavailable"
    );
}

#[test]
fn fragmented_site_surface_derives_canonical_binding_id() {
    assert_eq!(
        canonical_binding_id(Some("cintamani"), "task-lifecycle", None),
        "cintamani-task-lifecycle"
    );
    assert_eq!(
        canonical_binding_id(
            Some("cintamani"),
            "task-lifecycle",
            Some("explicit-binding")
        ),
        "explicit-binding"
    );
}

#[test]
fn generated_server_name_resolves_to_admitted_binding() {
    let envelope = json!({"bindings":[
        {"binding_id":"cintamani-task-lifecycle"},
        {"binding_id":"marici-task-lifecycle"}
    ]});
    assert_eq!(
        admitted_binding_entry(&envelope, "narada-cintamani-task-lifecycle").unwrap()["binding_id"],
        "cintamani-task-lifecycle"
    );
    assert!(admitted_binding_entry(&envelope, "narada-unknown-task-lifecycle").is_none());
}
