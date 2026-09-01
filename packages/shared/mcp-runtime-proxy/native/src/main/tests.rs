
    use super::{
        registrar_carrier_compatibility_notification, registrar_carrier_compatibility_response,
        resolve_child_command,
    };
    use serde_json::json;

    #[test]
    fn refuses_javascript_interpreters_as_native_proxy_children() {
        for command in ["node", "node.exe", "bun", "bun.exe"] {
            let error = resolve_child_command(command).expect_err("interpreter must be refused");
            assert_eq!(
                error,
                format!("native_proxy_interpreter_child_refused:{command}")
            );
        }
    }

    #[test]
    fn registrar_compatibility_synthesizes_only_the_naked_initialize_pair() {
        let initialize = json!({
            "jsonrpc": "2.0",
            "id": 7,
            "method": "initialize",
            "params": {}
        });
        let response = registrar_carrier_compatibility_response(
            Some("mcp-registrar"),
            Some("codex"),
            &initialize,
        )
        .expect("naked initialize must be synthesized at the carrier edge");
        assert_eq!(response["result"]["protocolVersion"], "2026-07-28");
        assert_eq!(response["result"]["resultType"], "complete");
        let kimi = registrar_carrier_compatibility_response(
            Some("mcp-registrar"),
            Some("kimi"),
            &initialize,
        )
        .expect("Kimi initialize must receive the legacy protocol version");
        assert_eq!(kimi["result"]["protocolVersion"], "2024-11-05");
        assert!(kimi["result"].get("resultType").is_none());
        assert!(registrar_carrier_compatibility_notification(
            Some("mcp-registrar"),
            &json!({ "jsonrpc": "2.0", "method": "notifications/initialized" })
        ));
    }

    #[test]
    fn registrar_compatibility_does_not_mask_modern_initialize_or_other_surfaces() {
        let modern = json!({
            "jsonrpc": "2.0",
            "id": 8,
            "method": "initialize",
            "params": {
                "_meta": {
                    "io.modelcontextprotocol/protocolVersion": "2026-07-28",
                    "io.modelcontextprotocol/clientInfo": { "name": "test", "version": "1" },
                    "io.modelcontextprotocol/clientCapabilities": {}
                }
            }
        });
        assert!(registrar_carrier_compatibility_response(
            Some("mcp-registrar"),
            Some("kimi"),
            &modern
        )
        .is_none());
        assert!(registrar_carrier_compatibility_response(
            Some("git"),
            Some("kimi"),
            &json!({
                "jsonrpc": "2.0",
                "id": 9,
                "method": "initialize",
                "params": {}
            })
        )
        .is_none());
    }
