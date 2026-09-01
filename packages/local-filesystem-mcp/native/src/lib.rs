//! Native implementation and contract fixtures for the local-filesystem MCP surface.

#[allow(dead_code)]
#[path = "../../../../packages/shared/mcp-runtime-proxy/native/src/filesystem.rs"]
pub mod filesystem;
#[path = "../../../../packages/shared/mcp-runtime-proxy/native/src/protocol.rs"]
pub(crate) mod protocol;

pub const EXPECTED_TOOLS: &[&str] = &[
    "fs_guidance",
    "fs_read_file",
    "fs_read_file_range",
    "fs_stat",
    "fs_glob_search",
    "fs_repository_inventory",
    "fs_file_metrics",
    "fs_search",
    "fs_search_results_read",
    "fs_grep_search",
    "fs_doctor",
    "fs_patch_outcome_show",
    "fs_write_file",
    "fs_str_replace_file",
    "fs_replace_range",
    "fs_apply_patch",
    "fs_move_path",
    "fs_create_directory",
    "fs_rename_directory",
    "fs_delete_directory",
];

#[cfg(test)]
mod contract_unit_tests {
    use super::EXPECTED_TOOLS;
    use std::collections::HashSet;

    #[test]
    fn public_tool_contract_is_unique_and_bounded() {
        assert_eq!(EXPECTED_TOOLS.len(), 20);
        assert_eq!(
            EXPECTED_TOOLS.iter().collect::<HashSet<_>>().len(),
            EXPECTED_TOOLS.len()
        );
        assert!(EXPECTED_TOOLS.iter().all(|name| name.starts_with("fs_")));
    }

    #[test]
    fn destructive_tools_are_explicitly_in_the_write_surface() {
        for name in [
            "fs_write_file",
            "fs_str_replace_file",
            "fs_replace_range",
            "fs_apply_patch",
            "fs_move_path",
            "fs_create_directory",
        ] {
            assert!(EXPECTED_TOOLS.contains(&name));
        }
    }
}
