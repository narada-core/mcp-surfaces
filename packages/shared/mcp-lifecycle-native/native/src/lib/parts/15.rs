impl LifecycleServer {
    fn payload_create(&mut self, args: Value) -> Result<Value, String> {
        let payload = payload_object_from_args(&args, "payload", "payload_json")?;
        if payload
            .as_object()
            .map(|value| value.is_empty())
            .unwrap_or(true)
            && args.get("allow_empty").and_then(Value::as_bool) != Some(true)
        {
            return Err("payload_create_empty_payload_requires_allow_empty".to_string());
        }
        let id = string_arg(&args, "payload_id")
            .unwrap_or_else(|| format!("p_{}", Uuid::new_v4().simple()));
        if !valid_payload_id(&id) {
            return Err(format!("payload_id_invalid: {id}"));
        }
        let byte_size = payload_byte_size(&payload);
        let max_bytes = 256 * 1024usize;
        if byte_size > max_bytes {
            return Err(format!("payload_too_large: {byte_size} > {max_bytes}"));
        }
        let revision = 1i64;
        let reference = format!("mcp_payload:{id}@v{revision}");
        let sha = digest(&payload);
        let record = json!({
            "schema": "narada.mcp_payload.revision.v1",
            "ref": reference,
            "payload_id": id,
            "revision": revision,
            "created_at": now(),
            "created_by": string_arg(&args, "created_by"),
            "source": {"kind": "create"},
            "sha256": sha,
            "byte_size": byte_size,
            "max_bytes": max_bytes,
            "transient_not_authority": true,
            "immutable_revision": true,
            "payload": payload
        });
        let path = payload_revision_path(&self.options.site_root, &id, revision);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| format!("payload_directory_create_failed:{e}"))?;
        }
        let serialized = format!("{}\n", payload_stable_json(&record));
        let status = match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(mut file) => {
                file.write_all(serialized.as_bytes())
                    .map_err(|e| format!("payload_write_failed:{e}"))?;
                "created"
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                let existing: Value = serde_json::from_str(
                    &fs::read_to_string(&path)
                        .map_err(|e| format!("payload_revision_conflict:{e}"))?,
                )
                .map_err(|e| format!("payload_revision_conflict:{e}"))?;
                if existing.get("ref") == record.get("ref")
                    && existing.get("sha256") == record.get("sha256")
                    && existing.get("byte_size") == record.get("byte_size")
                {
                    return Ok(json!({
                        "status": "existing",
                        "ref": reference,
                        "payload_id": id,
                        "revision": revision,
                        "source_ref": Value::Null,
                        "byte_size": byte_size,
                        "sha256": sha,
                        "created_at": existing.get("created_at").cloned().unwrap_or(Value::Null),
                        "created_by": existing.get("created_by").cloned().unwrap_or(Value::Null),
                        "transient_not_authority": true,
                        "immutable_revision": true
                    }));
                }
                return Err(format!("payload_revision_conflict: immutable revision already contains different content: {reference}"));
            }
            Err(error) => return Err(format!("payload_write_failed:{error}")),
        };
        Ok(json!({
            "status": status,
            "ref": reference,
            "payload_id": id,
            "revision": revision,
            "source_ref": Value::Null,
            "byte_size": byte_size,
            "sha256": sha,
            "created_at": record.get("created_at").cloned().unwrap_or(Value::Null),
            "created_by": record.get("created_by").cloned().unwrap_or(Value::Null),
            "transient_not_authority": true,
            "immutable_revision": true
        }))
    }
    fn payload_read(&self, name: &str, args: Value) -> Result<Value, String> {
        let reference = string_arg(&args, "ref")
            .or_else(|| string_arg(&args, "payload_ref"))
            .ok_or("payload_ref_required")?;
        if let Ok((id, revision)) = parse_payload_reference(&reference) {
            let path = payload_revision_path(&self.options.site_root, &id, revision);
            if path.is_file() {
                let metadata =
                    fs::metadata(&path).map_err(|e| format!("payload_ref_stat_failed:{e}"))?;
                let max_bytes = 256 * 1024usize;
                if metadata.len() > max_bytes as u64 {
                    return Err(format!(
                        "payload_ref_too_large: {} > {max_bytes}",
                        metadata.len()
                    ));
                }
                let text = fs::read_to_string(&path)
                    .map_err(|e| format!("payload_ref_read_failed:{e}"))?;
                let record: Value = serde_json::from_str(&text)
                    .map_err(|e| format!("payload_ref_invalid_json: {e}"))?;
                let object = record
                    .as_object()
                    .ok_or_else(|| format!("payload_ref_record_must_be_object: {reference}"))?;
                if object.get("schema").and_then(Value::as_str)
                    != Some("narada.mcp_payload.revision.v1")
                    || object.get("ref").and_then(Value::as_str) != Some(reference.as_str())
                    || object.get("payload_id").and_then(Value::as_str) != Some(id.as_str())
                    || object.get("revision").and_then(Value::as_i64) != Some(revision)
                {
                    return Err(format!("payload_ref_metadata_mismatch: {reference}"));
                }
                let payload = object
                    .get("payload")
                    .cloned()
                    .ok_or_else(|| format!("payload_ref_payload_must_be_object: {reference}"))?;
                if !payload.is_object() {
                    return Err(format!("payload_ref_payload_must_be_object: {reference}"));
                }
                let byte_size = payload_byte_size(&payload);
                if object.get("byte_size").and_then(Value::as_u64) != Some(byte_size as u64) {
                    return Err(format!("payload_ref_byte_size_mismatch: {reference}"));
                }
                if object.get("sha256").and_then(Value::as_str) != Some(digest(&payload).as_str()) {
                    return Err(format!("payload_ref_sha256_mismatch: {reference}"));
                }
                let mut result = json!({
                    "status": if name == "mcp_payload_validate" { "valid" } else { "ok" },
                    "ref": reference,
                    "payload_id": id,
                    "revision": revision,
                    "source_ref": object.get("source").and_then(|source| source.get("source_ref")).cloned().unwrap_or(Value::Null),
                    "byte_size": byte_size,
                    "sha256": object.get("sha256").cloned().unwrap_or(Value::Null),
                    "created_at": object.get("created_at").cloned().unwrap_or(Value::Null),
                    "created_by": object.get("created_by").cloned().unwrap_or(Value::Null),
                    "transient_not_authority": true,
                    "immutable_revision": true
                });
                if name == "mcp_payload_show" {
                    result["payload"] = payload;
                }
                return Ok(result);
            }
            return Err(format!("payload_ref_not_found: {reference}"));
        }
        let id = safe_reference_id(&reference, "mcp_payload:")?;
        let path = self
            .options
            .site_root
            .join(".ai")
            .join("mcp-payloads")
            .join(format!("{id}.json"));
        let text =
            fs::read_to_string(&path).map_err(|_| format!("payload_ref_not_found: {reference}"))?;
        let payload: Value =
            serde_json::from_str(&text).map_err(|e| format!("payload_invalid:{e}"))?;
        let mut result = json!({
            "status": if name == "mcp_payload_validate" { "valid" } else { "ok" },
            "ref": reference,
            "payload_id": id,
            "revision": Value::Null,
            "source_ref": Value::Null,
            "byte_size": payload_byte_size(&payload),
            "sha256": digest(&payload),
            "transient_not_authority": true,
            "immutable_revision": false
        });
        if name == "mcp_payload_show" {
            result["payload"] = payload;
        }
        Ok(result)
    }
    fn query_objects(
        &self,
        sql: &str,
        params: impl rusqlite::Params,
    ) -> Result<Vec<Value>, String> {
        let c = self.connection()?;
        let mut s = c.prepare(sql).map_err(db_error)?;
        let mut rows = s.query(params).map_err(db_error)?;
        let mut out = Vec::new();
        while let Some(row) = rows.next().map_err(db_error)? {
            out.push(row_to_object(row).map_err(db_error)?);
        }
        Ok(out)
    }
    fn query_one(&self, sql: &str, params: impl rusqlite::Params) -> Result<Option<Value>, String> {
        let c = self.connection()?;
        let mut s = c.prepare(sql).map_err(db_error)?;
        let mut rows = s.query(params).map_err(db_error)?;
        rows.next()
            .map_err(db_error)?
            .map(|r| row_to_object(r).map_err(db_error))
            .transpose()
    }
    fn connection_mut(&mut self) -> Result<&mut Connection, String> {
        self.connection
            .as_mut()
            .ok_or_else(|| "lifecycle_runtime_not_open".to_string())
    }
}
