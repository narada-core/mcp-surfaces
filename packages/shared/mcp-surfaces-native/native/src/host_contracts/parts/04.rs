#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_root(label: &str) -> PathBuf {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        std::env::temp_dir().join(format!("narada-host-contracts-{label}-{suffix}"))
    }

    #[test]
    fn operator_status_projects_persisted_overlay_state() {
        let root = temp_root("operator");
        let state_root = root.join("overlay-state");
        let state_directory = state_root.join("operator-console");
        fs::create_dir_all(&state_directory).expect("directory");
        fs::write(state_directory.join("document.json"), r#"{"schema":"narada.window_surface_overlay.document.v1","id":"operator-console","title":"Fixture","title_tone":"default","subtitle":null,"rows":[],"actions":[],"updated_at":"2026-08-09T00:00:00Z"}"#).expect("document");
        fs::write(state_directory.join("action-state.json"), r#"{"schema":"narada.window_surface_overlay.action_state.v1","action_id":"refresh","request_id":"request-1","status":"succeeded"}"#).expect("action");
        fs::write(
            state_directory.join("visibility.state.json"),
            r#"{"schema":"narada.window_surface_overlay.visibility_state.v1","state":"visible"}"#,
        )
        .expect("visibility");
        fs::write(
            state_root.join("surface.snapshot.json"),
            r#"{"schema":"narada.window_surface_overlay.surface_snapshot.v1","status":"ready"}"#,
        )
        .expect("snapshot");
        fs::write(
            state_root.join("focus.owner.json"),
            r#"{"schema":"narada.window_surface_overlay.focus_owner.v1","owner":"fixture"}"#,
        )
        .expect("focus");
        let response = operator_status_at(&root, &state_root);
        assert_eq!(
            response["schema"],
            "narada.operator_console_overlay.mcp_result.v1"
        );
        assert_eq!(
            response["overlay"]["schema"],
            "narada.window_surface_overlay.result.v1"
        );
        assert_eq!(response["overlay"]["state"], "stopped");
        assert_eq!(response["overlay"]["document"]["title"], "Fixture");
        assert_eq!(response["overlay"]["action_state"]["status"], "succeeded");
        let _ = fs::remove_dir_all(root);
    }
}
