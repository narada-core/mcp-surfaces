impl Engine {
    fn sequence_list(&self, root: &Path, args: &Map<String, Value>) -> Result<Value, Value> {
        let limit = self.page_limit(args)?;
        let offset = self.page_offset(args)?;
        let mut items = Vec::new();
        if self.sequences(root).exists() {
            for entry in fs::read_dir(self.sequences(root))
                .map_err(self.io_error("sequence_store_read_failed"))?
            {
                let Ok(entry) = entry else { continue };
                let manifest_path = entry.path().join("sequence.json");
                if !manifest_path.exists() {
                    continue;
                }
                let hash = entry.file_name().to_string_lossy().to_string();
                let item = self.with_authority_lock(root, &format!("sequence-{hash}"), || {
                    let manifest = self.read_json(&manifest_path)?;
                    let name = manifest
                        .get("sequence_name")
                        .and_then(Value::as_str)
                        .ok_or_else(|| {
                            self.error(
                                "sequence_manifest_invalid",
                                "sequence manifest lacks sequence_name",
                                json!({"path":manifest_path.to_string_lossy()}),
                            )
                        })?;
                    self.verify_sequence_manifest(&manifest, name)?;
                    let claims = self.verified_sequence_claims(root, name, &manifest)?;
                    Ok(json!({
                        "sequence_id":manifest["sequence_id"],
                        "sequence_name":name,
                        "start_at":manifest["start_at"],
                        "claim_count":claims.len(),
                        "last_claimed_value":claims.last().and_then(|claim| claim.get("value")).cloned().unwrap_or(Value::Null),
                        "created_at":manifest["created_at"]
                    }))
                })?;
                items.push(item);
            }
        }
        items.sort_by(|left, right| {
            left["sequence_name"]
                .as_str()
                .cmp(&right["sequence_name"].as_str())
        });
        let total = items.len();
        let page = items
            .into_iter()
            .skip(offset)
            .take(limit)
            .collect::<Vec<_>>();
        let count = page.len();
        Ok(
            json!({"schema":self.domain.features.sequences.list_schema_id,"items":page,"offset":offset,"limit":limit,"count":count,"total":total,"has_more":offset+count<total}),
        )
    }

    fn sequence_claim_next(&self, root: &Path, args: &Map<String, Value>) -> Result<Value, Value> {
        self.prepare(root)?;
        let name = self.validated_sequence_name(args)?;
        let actor = self.required(args, "actor")?;
        let authority_basis = self.required_object(args, "authority_basis")?;
        let idempotency_key = self.required(args, "idempotency_key")?;
        let request_digest = self.digest_value(
            &json!({"sequence_name":name,"actor":actor,"authority_basis":authority_basis}),
        )?;
        let lock_key = format!("sequence-{}", sha256(name.as_bytes()));
        self.with_authority_lock(root, &lock_key, || {
            let manifest = self.load_sequence_manifest(root, &name)?;
            let claims = self.verified_sequence_claims(root, &name, &manifest)?;
            if let Some(claim) = Self::find_sequence_claim_by_idempotency(&claims, &idempotency_key) {
                if claim.get("request_digest").and_then(Value::as_str) != Some(request_digest.as_str())
                {
                    return Err(self.error(
                        "sequence_claim_idempotency_conflict",
                        "idempotency key already names a different claim request",
                        json!({"sequence_name":name,"idempotency_key":idempotency_key,"claim_id":claim["claim_id"]}),
                    ));
                }
                self.recover_sequence_idempotency_index(root, &name, &idempotency_key, claim)?;
                return Ok(self.sequence_claim_receipt(claim, true));
            }
            let start_at = manifest["start_at"].as_u64().unwrap();
            let value = match claims.last().and_then(|claim| claim["value"].as_u64()) {
                Some(previous) => previous.checked_add(1).ok_or_else(|| {
                    self.error(
                        "sequence_exhausted",
                        "sequence has exhausted u64 values",
                        json!({"sequence_name":name,"last_claimed_value":previous}),
                    )
                })?,
                None => start_at,
            };
            let chain_field = &self.domain.features.sequences.claim_chain_field;
            let previous_hash = claims
                .last()
                .and_then(|claim| claim[self.domain.features.sequences.claim_hash_field.clone()].as_str())
                .map(str::to_string);
            let claim_id = self.generated_claim_id(&name, &idempotency_key);
            let mut claim = json!({
                "schema":self.domain.features.sequences.claim_schema_id,
                "sequence_id":manifest["sequence_id"],
                "sequence_name":name,
                "value":value,
                "claim_id":claim_id,
                chain_field.clone():previous_hash,
                "actor":actor,
                "identity_state":Self::identity_state_for_event(&actor, "ledger.sequence_claim"),
                "authority_basis":authority_basis,
                "idempotency_key":idempotency_key,
                "request_digest":request_digest,
                "claimed_at":now()
            });
            let claim_hash = self.digest_value(&claim)?;
            claim[self.domain.features.sequences.claim_hash_field.clone()] = json!(claim_hash);
            self.write_new_json(
                &self
                    .sequence_claims_directory(root, &name)
                    .join(self.sequence_claim_file_name(value)),
                &claim,
            )?;
            self.recover_sequence_idempotency_index(root, &name, &idempotency_key, &claim)?;
            Ok(self.sequence_claim_receipt(&claim, false))
        })
    }

    fn sequence_claims(&self, root: &Path, args: &Map<String, Value>) -> Result<Value, Value> {
        let name = self.validated_sequence_name(args)?;
        let limit = self.page_limit(args)?;
        let offset = self.page_offset(args)?;
        let lock_key = format!("sequence-{}", sha256(name.as_bytes()));
        self.with_authority_lock(root, &lock_key, || {
            let manifest = self.load_sequence_manifest(root, &name)?;
            let claims = self.verified_sequence_claims(root, &name, &manifest)?;
            let total = claims.len();
            let page = claims
                .into_iter()
                .skip(offset)
                .take(limit)
                .collect::<Vec<_>>();
            let count = page.len();
            Ok(
                json!({"schema":self.domain.features.sequences.claims_schema_id,"sequence_name":name,"claims":page,"offset":offset,"limit":limit,"count":count,"total":total,"has_more":offset+count<total}),
            )
        })
    }

    fn sequence_claim_receipt(&self, claim: &Value, replay: bool) -> Value {
        let next_value = claim["value"]
            .as_u64()
            .and_then(|value| value.checked_add(1));
        json!({
            "schema":self.domain.features.sequences.claim_receipt_schema_id,
            "status":if replay{"idempotent_replay"}else{"claimed"},
            "idempotency_replay":replay,
            "sequence_id":claim["sequence_id"],
            "sequence_name":claim["sequence_name"],
            "value":claim["value"],
            "claim_id":claim["claim_id"],
            "claimed_at":claim["claimed_at"],
            "next_value":next_value,
            "exhausted":next_value.is_none()
        })
    }

}
