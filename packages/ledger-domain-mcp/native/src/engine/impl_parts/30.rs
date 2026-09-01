impl Engine {
    fn verified_sequence_claims(
        &self,
        root: &Path,
        name: &str,
        manifest: &Value,
    ) -> Result<Vec<Value>, Value> {
        let sequences = &self.domain.features.sequences;
        let directory = self.sequence_claims_directory(root, name);
        if !directory.exists() {
            return Ok(Vec::new());
        }
        let mut paths = fs::read_dir(&directory)
            .map_err(self.io_error("sequence_claim_store_read_failed"))?
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.extension().and_then(|value| value.to_str()) == Some("json"))
            .collect::<Vec<_>>();
        paths.sort();
        let total = paths.len();
        let mut claims = Vec::with_capacity(total);
        let mut expected_value = manifest["start_at"].as_u64().unwrap();
        let mut previous_hash: Option<String> = None;
        let mut idempotency_keys = HashSet::new();
        let mut claim_ids = HashSet::new();
        for (index, path) in paths.into_iter().enumerate() {
            let claim = self.read_json(&path)?;
            let hash_field = sequences.claim_hash_field.clone();
            let Some(chain::RecomputedHash {
                stored: actual_hash,
                computed: computed_hash,
            }) = chain::recompute_hash(self.error, &claim, &hash_field)?
            else {
                return Err(self.error(
                    "sequence_claim_invalid",
                    "sequence claim lacks claim_hash",
                    json!({"path":path.to_string_lossy()}),
                ));
            };
            let idempotency_key = claim.get("idempotency_key").and_then(Value::as_str);
            let claim_id = claim.get("claim_id").and_then(Value::as_str);
            if claim.get("schema") != Some(&json!(sequences.claim_schema_id))
                || claim.get("sequence_name").and_then(Value::as_str) != Some(name)
                || claim.get("sequence_id") != manifest.get("sequence_id")
                || claim.get("value").and_then(Value::as_u64) != Some(expected_value)
                || claim
                    .get(&sequences.claim_chain_field)
                    .and_then(Value::as_str)
                    != previous_hash.as_deref()
                || claim
                    .get("request_digest")
                    .and_then(Value::as_str)
                    .is_none()
                || idempotency_key.is_none_or(str::is_empty)
                || claim_id.is_none_or(str::is_empty)
                || !idempotency_keys.insert(idempotency_key.unwrap().to_string())
                || !claim_ids.insert(claim_id.unwrap().to_string())
                || actual_hash != computed_hash
            {
                return Err(self.error(
                    "sequence_claim_chain_invalid",
                    "sequence claim chain is not contiguous and hash-valid",
                    json!({"sequence_name":name,"path":path.to_string_lossy(),"expected_value":expected_value}),
                ));
            }
            previous_hash = Some(actual_hash.to_string());
            claims.push(claim);
            if index + 1 < total {
                expected_value = expected_value.checked_add(1).ok_or_else(|| {
                    self.error(
                        "sequence_claim_chain_invalid",
                        "claim exists after u64 exhaustion",
                        json!({"sequence_name":name}),
                    )
                })?;
            }
        }
        Ok(claims)
    }

    fn find_sequence_claim_by_idempotency<'a>(claims: &'a [Value], key: &str) -> Option<&'a Value> {
        claims
            .iter()
            .find(|claim| claim.get("idempotency_key").and_then(Value::as_str) == Some(key))
    }

    fn recover_sequence_idempotency_index(
        &self,
        root: &Path,
        name: &str,
        key: &str,
        claim: &Value,
    ) -> Result<(), Value> {
        let directory = self.sequence_directory(root, name).join("idempotency");
        fs::create_dir_all(&directory)
            .map_err(self.io_error("sequence_idempotency_store_create_failed"))?;
        let path = directory.join(format!("{}.json", sha256(key.as_bytes())));
        if path.exists() {
            let existing = self.read_json(&path)?;
            if existing.get("claim_id") != claim.get("claim_id") {
                return Err(self.error(
                    "sequence_claim_idempotency_conflict",
                    "idempotency index names a different claim",
                    json!({"sequence_name":name,"idempotency_key":key,"existing_claim_id":existing.get("claim_id"),"claim_id":claim.get("claim_id")}),
                ));
            }
            return Ok(());
        }
        self.write_new_json(
            &path,
            &json!({"schema":self.domain.features.sequences.idempotency_schema_id,"idempotency_key":key,"claim_id":claim["claim_id"],"value":claim["value"]}),
        )
    }

    fn find_ledger_event_by_idempotency(
        &self,
        root: &Path,
        key: &str,
    ) -> Result<Option<Value>, Value> {
        event_ledger::find_event_by_idempotency(self.error, &self.ledger_layout(root), key)
    }

    fn prepare(&self, root: &Path) -> Result<(), Value> {
        fs::create_dir_all(self.ledger(root)).map_err(self.io_error("ledger_create_failed"))?;
        fs::create_dir_all(self.proposals(root))
            .map_err(self.io_error("proposal_store_create_failed"))?;
        fs::create_dir_all(self.runtime(root))
            .map_err(self.io_error("projection_root_create_failed"))?;
        Ok(())
    }

    /// Site control root: the site root itself when its basename is
    /// `.narada`, otherwise `<site_root>/.narada` (engine constant).
    fn control(&self, root: &Path) -> PathBuf {
        if root.file_name().and_then(|value| value.to_str()) == Some(".narada") {
            root.to_path_buf()
        } else {
            root.join(".narada")
        }
    }

    // Storage subdirs join as one '/'-separated segment so rendered paths stay
    // byte-identical to the reference implementations on every platform.
    fn ledger(&self, root: &Path) -> PathBuf {
        self.control(root).join(format!(
            "{}/{}",
            self.domain.storage.control_root_subdir, self.domain.storage.subdirs.ledger
        ))
    }

    fn proposals(&self, root: &Path) -> PathBuf {
        self.control(root).join(format!(
            "{}/{}",
            self.domain.storage.control_root_subdir, self.domain.storage.subdirs.proposals
        ))
    }

    fn sequences(&self, root: &Path) -> PathBuf {
        self.control(root).join(format!(
            "{}/{}",
            self.domain.storage.control_root_subdir, self.domain.storage.subdirs.sequences
        ))
    }

    fn runtime(&self, root: &Path) -> PathBuf {
        self.control(root).join(&self.domain.storage.runtime_subdir)
    }

    fn projection_path(&self, root: &Path) -> PathBuf {
        self.runtime(root).join("projection.sqlite")
    }

    fn ledger_layout(&self, root: &Path) -> LedgerLayout {
        LedgerLayout::new(self.ledger(root), &self.domain.storage.ledger_file_prefix)
    }

    fn ledger_files(&self, root: &Path) -> Result<Vec<PathBuf>, Value> {
        event_ledger::files(self.error, &self.ledger_layout(root))
    }

    fn ledger_head(&self, root: &Path) -> Result<Option<String>, Value> {
        event_ledger::head(
            self.error,
            &self.ledger_layout(root),
            &self.domain.storage.event_hash_field,
        )
    }

    fn load_proposal(&self, root: &Path, id: &str) -> Result<Value, Value> {
        self.read_json(&self.proposals(root).join(format!("{}.json", safe_name(id))))
    }

    fn read_json(&self, path: &Path) -> Result<Value, Value> {
        ledger_io::read_json(self.error, path)
    }

    fn write_new_json(&self, path: &Path, value: &Value) -> Result<(), Value> {
        ledger_io::write_new_json(self.error, path, value)
    }

    fn write_replace_json(&self, path: &Path, value: &Value) -> Result<(), Value> {
        ledger_io::write_replace_json(self.error, path, value)
    }

    fn write_new(&self, path: &Path, bytes: &[u8]) -> Result<(), Value> {
        ledger_io::write_new(self.error, path, bytes)
    }

    fn digest_value(&self, value: &Value) -> Result<String, Value> {
        narada_mcp_event_ledger::digest::digest_value(self.error, value)
    }

    fn required(&self, args: &Map<String, Value>, key: &str) -> Result<String, Value> {
        ledger_args::required(self.error, args, key)
    }

    fn generated_sequence_id(&self, name: &str) -> String {
        let template = &self.domain.id_derivation.generated_ids.sequence_id;
        format!(
            "{}{}",
            template_prefix(template),
            &sha256(name.as_bytes())[..template_truncation(template, 24)]
        )
    }

    fn generated_claim_id(&self, name: &str, idempotency_key: &str) -> String {
        let template = &self.domain.id_derivation.generated_ids.claim_id;
        format!(
            "{}{}",
            template_prefix(template),
            &sha256(format!("{name}\0{idempotency_key}").as_bytes())
                [..template_truncation(template, 24)]
        )
    }

    /// Render one claim file name from the descriptor's
    /// `claim_file_pattern` (for example `claims/claim-{value:020}.json`).
    /// Only the file-name portion is returned; the caller joins the claims
    /// directory.
    fn sequence_claim_file_name(&self, value: u64) -> String {
        let pattern = &self.domain.features.sequences.claim_file_pattern;
        let Some((left, right)) = pattern.split_once("{value:") else {
            return format!("claim-{value:020}.json");
        };
        let prefix = left.rsplit('/').next().unwrap_or(left);
        let Some((width_text, suffix)) = right.split_once('}') else {
            return format!("claim-{value:020}.json");
        };
        let Ok(width) = width_text.parse::<usize>() else {
            return format!("claim-{value:020}.json");
        };
        format!("{prefix}{value:0width$}{suffix}")
    }

}
