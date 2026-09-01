#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_surface_tool_sets_match_domain_contracts() {
        assert!(list_tools("site-lifecycle")
            .iter()
            .any(|tool| tool["name"] == "site_init"));
        assert!(list_tools("site-registry")
            .iter()
            .any(|tool| tool["name"] == "site_registry_show"));
        assert!(list_tools("project-state")
            .iter()
            .any(|tool| tool["name"] == "project_state_validate"));
    }

    #[test]
    fn site_lifecycle_doctor_reports_native_runtime_without_coordinates() {
        let doctor = list_tools("site-lifecycle")
            .into_iter()
            .find(|tool| tool["name"] == "site_lifecycle_doctor")
            .expect("doctor tool");
        assert_eq!(doctor["inputSchema"]["required"], json!([]));

        let args = Map::new();
        let result = call_tool(
            "site-lifecycle",
            "site_lifecycle_doctor",
            &args,
            Path::new("C:/definitely-missing-site"),
        )
        .unwrap();
        assert_eq!(result["status"], "ok");
        assert_eq!(result["runtime_dependency"], "none");
        assert_eq!(result["legacy_dependency_sync"], "retired");
    }

    #[test]
    fn project_state_remains_virtual_and_argument_bounded() {
        use sha2::{Digest, Sha256};
        let root = std::env::temp_dir().join(format!("narada-project-state-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("tmp")).unwrap();
        std::fs::create_dir_all(root.join("cad/nrc600/project_state")).unwrap();
        let source = b"-- canonical project-state fixture";
        std::fs::write(root.join("cad/nrc600/project_state/nrc600_project_state.sql"), source).unwrap();
        let digest = Sha256::digest(source).iter().map(|byte| format!("{byte:02x}")).collect::<String>();
        std::fs::write(root.join("tmp/nrc600_project_state.json"), serde_json::to_vec(&json!({
            "schema":"narada.project_state.registry.v5","source_sha256":digest,"project_id":"demo-project",
            "programs":[{"id":"demo"}],"projects":[{"id":"demo-project"}],"program_memberships":[{"program_id":"demo","project_id":"demo-project"}],
            "objects":[],"standards":[],"standard_applicability":[],"obligations":[],"obligation_mappings":[],"standard_gaps":[],"action_claims":[]
        })).unwrap()).unwrap();
        let mut args = Map::new();
        args.insert("program_id".to_string(), json!("demo"));
        let result = call_tool(
            "project-state",
            "project_state_program_show",
            &args,
            &root,
        )
        .unwrap();
        assert_eq!(result["status"], "ok");
        assert_eq!(result["virtual_only"], true);
        assert_eq!(result["source_hash_verified"], true);
        assert_eq!(result["result"]["program"]["id"], "demo");
        std::fs::remove_dir_all(root).unwrap();
    }
}
