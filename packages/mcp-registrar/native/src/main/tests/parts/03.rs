    #[test]
    fn native_materialization_receipt_is_accepted_only_for_its_carrier_and_config() {
        let nonce = format!(
            "narada-registrar-receipt-{}-{}",
            std::process::id(),
            time::OffsetDateTime::now_utc().unix_timestamp_nanos()
        );
        let config_path = env::temp_dir().join(format!("{nonce}.json"));
        let config_path_text = config_path.to_string_lossy().to_string();
        let sidecar_path = format!("{config_path_text}.narada-generation.json");
        fs::write(
            &sidecar_path,
            serde_json::to_vec(&json!({
                "carrier_id":"kimi-test",
                "config_path":config_path_text,
                "config_artifact":{"bytes_sha256":"abc123"},
                "managed_projection":{"scope":"whole_document","selectors":[]}
            }))
            .unwrap(),
        )
        .unwrap();

        let receipt = native_materialization_receipt(&config_path_text, "kimi-test").unwrap();
        assert_eq!(receipt.expected_sha256, "abc123");
        assert_eq!(receipt.scope, "whole_document");
        assert!(native_materialization_receipt(&config_path_text, "other-carrier").is_none());

        fs::remove_file(sidecar_path).unwrap();
    }
