#[cfg(test)]
mod tests {
    use super::*;

    fn epistemic_descriptor_path() -> std::path::PathBuf {
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../shared/ledger-domain-epistemic/domain.json")
    }

    #[test]
    fn checked_in_epistemic_descriptor_loads() {
        let descriptor =
            Descriptor::load(&epistemic_descriptor_path()).expect("epistemic descriptor loads");
        assert_eq!(descriptor.schema, DESCRIPTOR_SCHEMA_ID);
        assert_eq!(descriptor.identity.tool_prefix, "epistemic_graph");
        assert_eq!(descriptor.tools.len(), 31);
        assert!(descriptor.tools.iter().any(|tool| tool.name == "epistemic_graph_team_work_overview"));
        assert_eq!(descriptor.entities.core_kinds.len(), 10);
        assert_eq!(descriptor.relations.core.len(), 16);
        assert_eq!(descriptor.operations.kinds.len(), 6);
        assert!(descriptor.features.proposals.enabled);
        assert!(descriptor.features.sequences.enabled);
        assert!(descriptor.features.source_inspect.enabled);
        assert!(descriptor.features.snapshot.enabled);
        assert!(descriptor.features.export.enabled);
    }

    #[test]
    fn malformed_descriptor_is_domain_invalid() {
        let failure = Descriptor::parse("{not json").expect_err("malformed JSON refuses");
        assert!(failure.starts_with("domain_invalid:"), "{failure}");

        let failure = Descriptor::parse("{\"schema\":\"narada.ledger-domain.v1\"}")
            .expect_err("incomplete descriptor refuses");
        assert!(failure.starts_with("domain_invalid:"), "{failure}");

        let mut value: Value = serde_json::from_str(
            &std::fs::read_to_string(epistemic_descriptor_path()).expect("descriptor text"),
        )
        .expect("descriptor json");
        value["identity"]["tool_prefix"] = Value::from(42);
        let failure = Descriptor::from_value(value).expect_err("wrong type refuses");
        assert!(failure.starts_with("domain_invalid:"), "{failure}");
    }

    #[test]
    fn unknown_top_level_section_is_refused() {
        let mut value: Value = serde_json::from_str(
            &std::fs::read_to_string(epistemic_descriptor_path()).expect("descriptor text"),
        )
        .expect("descriptor json");
        value["speculative_section"] = serde_json::json!({"invented": true});
        let failure =
            Descriptor::from_value(value).expect_err("unknown sections are refused by the schema");
        assert!(failure.starts_with("domain_invalid:"), "{failure}");
    }

    #[test]
    fn descriptor_schema_does_not_hardcode_epistemic_identity() {
        let mut value: Value = serde_json::from_str(
            &std::fs::read_to_string(epistemic_descriptor_path()).expect("descriptor text"),
        )
        .expect("descriptor json");
        value["identity"]["domain_id"] = Value::from("other-domain");
        value["identity"]["tool_prefix"] = Value::from("other_graph");
        value["identity"]["schema_namespace"] = Value::from("other.graph");
        value["identity"]["error_schema_id"] = Value::from("other.graph.error.v1");
        let descriptor = Descriptor::from_value(value).expect("generic descriptor identity loads");
        assert_eq!(descriptor.identity.tool_prefix, "other_graph");
        assert_eq!(descriptor.identity.schema_namespace, "other.graph");
    }
}
