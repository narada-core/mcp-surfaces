impl Engine {
    fn resolve_payload_arguments(
        &self,
        root: &Path,
        args: &Map<String, Value>,
    ) -> Result<Map<String, Value>, Value> {
        let Some(reference) = args.get("payload_ref").and_then(Value::as_str) else {
            return Ok(args.clone());
        };
        if args.len() != 1 {
            return Err(self.error(
                "payload_ref_ambiguous",
                "payload_ref cannot be combined with inline proposal arguments",
                json!({"payload_ref":reference}),
            ));
        }
        let body = reference.strip_prefix("mcp_payload:").ok_or_else(|| {
            self.error(
                "payload_ref_invalid",
                "payload_ref must use mcp_payload:<id>@v<revision>",
                json!({"payload_ref":reference}),
            )
        })?;
        let (payload_id, revision_text) = body.split_once("@v").ok_or_else(|| {
            self.error(
                "payload_ref_invalid",
                "payload_ref must include an immutable revision",
                json!({"payload_ref":reference}),
            )
        })?;
        if !(3..=64).contains(&payload_id.len())
            || !payload_id
                .chars()
                .next()
                .is_some_and(|ch| ch.is_ascii_alphanumeric())
            || !payload_id
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_'))
        {
            return Err(self.error(
                "payload_ref_invalid",
                "payload_ref id is invalid",
                json!({"payload_ref":reference}),
            ));
        }
        let revision = revision_text
            .parse::<u64>()
            .ok()
            .filter(|value| *value > 0)
            .ok_or_else(|| {
                self.error(
                    "payload_ref_invalid",
                    "payload_ref revision must be a positive integer",
                    json!({"payload_ref":reference}),
                )
            })?;
        let path = root
            .join(".ai")
            .join("tmp")
            .join("mcp-payloads")
            .join("workspace")
            .join(payload_id)
            .join(format!("v{revision}.json"));
        let metadata = fs::metadata(&path).map_err(|_| {
            self.error(
                "payload_ref_not_found",
                "immutable payload revision was not found",
                json!({"payload_ref":reference}),
            )
        })?;
        const MAX_PAYLOAD_BYTES: u64 = 256 * 1024;
        if metadata.len() > MAX_PAYLOAD_BYTES {
            return Err(self.error(
                "payload_ref_too_large",
                "immutable payload revision exceeds the transport ceiling",
                json!({"payload_ref":reference,"byte_size":metadata.len(),"max_bytes":MAX_PAYLOAD_BYTES}),
            ));
        }
        let record = self.read_json(&path)?;
        if record.get("schema").and_then(Value::as_str) != Some("narada.mcp_payload.revision.v1")
            || record.get("ref").and_then(Value::as_str) != Some(reference)
            || record.get("payload_id").and_then(Value::as_str) != Some(payload_id)
            || record.get("revision").and_then(Value::as_u64) != Some(revision)
        {
            return Err(self.error(
                "payload_ref_metadata_mismatch",
                "immutable payload metadata does not match its reference",
                json!({"payload_ref":reference}),
            ));
        }
        let payload = record
            .get("payload")
            .and_then(Value::as_object)
            .ok_or_else(|| {
                self.error(
                    "payload_ref_payload_must_be_object",
                    "proposal payload must be a JSON object",
                    json!({"payload_ref":reference}),
                )
            })?;
        if payload.contains_key("payload_ref") {
            return Err(self.error(
                "payload_ref_recursive",
                "payload-backed arguments cannot contain another payload_ref",
                json!({"payload_ref":reference}),
            ));
        }
        let canonical = serde_json::to_vec(&canonical_json(&Value::Object(payload.clone())))
            .unwrap_or_default();
        if record.get("byte_size").and_then(Value::as_u64) != Some(canonical.len() as u64) {
            return Err(self.error(
                "payload_ref_byte_size_mismatch",
                "immutable payload byte size verification failed",
                json!({"payload_ref":reference}),
            ));
        }
        let actual_sha256 = sha256(&canonical);
        if record.get("sha256").and_then(Value::as_str) != Some(actual_sha256.as_str()) {
            return Err(self.error(
                "payload_ref_sha256_mismatch",
                "immutable payload digest verification failed",
                json!({"payload_ref":reference}),
            ));
        }
        Ok(payload.clone())
    }

    fn enrich_payload_ref_refusal(
        &self,
        mut error: Value,
        payload_ref: Option<&str>,
        retry_tool: &str,
    ) -> Value {
        let Some(reference) = payload_ref else {
            return error;
        };
        if error.get("code").and_then(Value::as_str)
            != Some(
                self.domain
                    .query
                    .communication
                    .legacy_write_refusal_code
                    .as_str(),
            )
        {
            return error;
        }
        let Some((payload_id, revision)) = reference
            .strip_prefix("mcp_payload:")
            .and_then(|body| body.rsplit_once("@v"))
            .and_then(|(id, revision)| revision.parse::<u64>().ok().map(|value| (id, value)))
        else {
            return error;
        };
        let canonical = self.domain.query.communication.canonical_kind.clone();
        let supplied = error
            .pointer("/details/supplied_kind")
            .cloned()
            .unwrap_or(Value::Null);
        if let Some(details) = error.get_mut("details").and_then(Value::as_object_mut) {
            details.insert("input_transport".into(), json!("immutable_payload_ref"));
            details.insert("payload_ref".into(), json!(reference));
            details.insert("payload_revision_mutable".into(), json!(false));
            details.insert("graph_mutation_committed".into(), json!(false));
            details.insert(
                "remediation".into(),
                json!("Create a successor immutable payload revision with canonical communication kinds, then retry the same submission tool. Do not edit or retry the rejected revision."),
            );
            details.insert(
                "recovery".into(),
                json!({
                    "action":"create_successor_payload_revision",
                    "source_payload_ref":reference,
                    "suggested_payload_ref":format!("mcp_payload:{payload_id}@v{}", revision + 1),
                    "preserve_source_revision":true,
                    "replace":{"entity.kind":{"from":supplied,"to":canonical}},
                    "payload_revision_tools":{
                        "read":"mcp_payload_show",
                        "derive":"mcp_payload_derive",
                        "validate":"mcp_payload_validate",
                        "surface":"task-lifecycle"
                    },
                    "then_retry_with":{"argument":"payload_ref","tool":retry_tool}
                }),
            );
        }
        error
    }

}
