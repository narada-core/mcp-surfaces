//! Native implementation and contract fixtures for the structured-command MCP surface.

#[allow(dead_code)]
#[path = "../../../../packages/shared/mcp-runtime-proxy/native/src/filesystem.rs"]
pub(crate) mod filesystem;
#[path = "../../../../packages/shared/mcp-runtime-proxy/native/src/protocol.rs"]
pub(crate) mod protocol;
#[allow(dead_code)]
#[path = "../../../../packages/shared/mcp-runtime-proxy/native/src/structured_command.rs"]
pub mod structured_command;

pub const EXPECTED_TOOLS: &[&str] = &[
    "structured_command_guidance",
    "structured_command_execution_policy_inspect",
    "structured_command_output_show",
    "structured_command_execute",
    "structured_command_start",
    "structured_command_execution_show",
    "structured_command_powershell_parse_check",
    "structured_command_input_create",
    "structured_command_elevated_window_execute",
];

#[cfg(test)]
mod contract_unit_tests {
    use super::EXPECTED_TOOLS;
    use std::collections::HashSet;

    #[test]
    fn public_tool_contract_is_unique_and_bounded() {
        assert_eq!(EXPECTED_TOOLS.len(), 9);
        assert_eq!(
            EXPECTED_TOOLS.iter().collect::<HashSet<_>>().len(),
            EXPECTED_TOOLS.len()
        );
        assert!(EXPECTED_TOOLS
            .iter()
            .all(|name| name.starts_with("structured_command_")));
    }

    #[test]
    fn execution_contract_has_separate_sync_async_and_observation_tools() {
        assert!(EXPECTED_TOOLS.contains(&"structured_command_execute"));
        assert!(EXPECTED_TOOLS.contains(&"structured_command_start"));
        assert!(EXPECTED_TOOLS.contains(&"structured_command_execution_show"));
    }
}
