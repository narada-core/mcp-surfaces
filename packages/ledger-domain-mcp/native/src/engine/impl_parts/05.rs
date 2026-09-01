impl Engine {
    fn communication_migration_preflight(
        &self,
        root: &Path,
        args: &Map<String, Value>,
    ) -> Result<Value, Value> {
        self.prepare(root)?;
        let requested = args.get("limit").and_then(Value::as_u64).unwrap_or(50);
        let limit = requested.clamp(1, self.domain.caps.operations_per_proposal.max) as usize;
        let head = self
            .status(root)?
            .get("ledger_head")
            .cloned()
            .unwrap_or(Value::Null);
        let communication = &self.domain.query.communication;
        let cursor_schema = self.schema_id("communication_migration_cursor.v1");
        let query_digest = sha256(
            &serde_json::to_vec(&json!({
                "canonical_kind": communication.canonical_kind,
                "legacy_read_aliases": communication.legacy_read_aliases,
                "contract_version": communication.contract_version
            }))
            .unwrap_or_default(),
        );
        let cursor = if let Some(raw_cursor) = args.get("cursor") {
            let decoded = decode_cursor_token(raw_cursor, &cursor_schema).map_err(|_| {
                self.error(
                    "communication_migration_cursor_invalid",
                    "migration cursor is malformed or belongs to another operation",
                    json!({}),
                )
            })?;
            if decoded.get("ledger_head") != Some(&head)
                || decoded.get("query_digest").and_then(Value::as_str)
                    != Some(query_digest.as_str())
            {
                return Err(self.error(
                    "communication_migration_cursor_stale",
                    "migration cursor is not bound to the current ledger head and descriptor query",
                    json!({"cursor_ledger_head":decoded.get("ledger_head"),"actual_ledger_head":head}),
                ));
            }
            decoded
                .get("entity_id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        } else {
            String::new()
        };
        let db = Connection::open(self.projection_path(root))
            .map_err(self.db_error("projection_open_failed"))?;
        let placeholders = (0..communication.legacy_read_aliases.len())
            .map(|index| format!("?{}", index + 2))
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!("select entity_id,kind,payload_json,event_id,event_sequence from {} where entity_id>?1 and kind in ({placeholders}) order by entity_id limit {}", self.entity_table, limit + 1);
        let mut parameters = Vec::<String>::new();
        parameters.push(cursor);
        parameters.extend(communication.legacy_read_aliases.iter().cloned());
        let mut statement = db
            .prepare(&sql)
            .map_err(self.db_error("communication_migration_prepare_failed"))?;
        let rows = statement
            .query_map(rusqlite::params_from_iter(parameters.iter()), |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, u64>(4)?,
                ))
            })
            .map_err(self.db_error("communication_migration_query_failed"))?;
        let mut candidates = rows
            .collect::<Result<Vec<_>, _>>()
            .map_err(self.db_error("communication_migration_row_failed"))?;
        let has_more = candidates.len() > limit;
        candidates.truncate(limit);
        let mut by_kind = BTreeMap::<String, u64>::new();
        let mut by_sender = BTreeMap::<String, u64>::new();
        let mut by_recipient = BTreeMap::<String, u64>::new();
        let mut operations = Vec::new();
        let mut census = Vec::new();
        for (entity_id, kind, payload_json, event_id, event_sequence) in candidates {
            let payload: Value = serde_json::from_str(&payload_json).map_err(|error| {
                self.error(
                    "communication_migration_payload_invalid",
                    &error.to_string(),
                    json!({"entity_id":entity_id}),
                )
            })?;
            let sender = payload
                .get("sender")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let recipient = payload
                .get("recipient")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            *by_kind.entry(kind.clone()).or_default() += 1;
            *by_sender.entry(sender.clone()).or_default() += 1;
            *by_recipient.entry(recipient.clone()).or_default() += 1;
            let thread_member: bool = db.query_row(&format!("select exists(select 1 from {} where relation_type='replies_to' and (source_id=?1 or target_id=?1))", self.relation_table), params![entity_id], |row| row.get(0)).map_err(self.db_error("communication_migration_thread_census_failed"))?;
            let payload_sha256 = sha256(payload_json.as_bytes());
            operations.push(json!({"op":communication.canonicalization_operation,"entity_id":entity_id,"legacy_kind":kind,"canonical_kind":communication.canonical_kind,"equivalence_evidence":{"payload_sha256":payload_sha256,"originating_event_id":event_id}}));
            census.push(json!({"entity_id":entity_id,"kind":kind,"sender":sender,"recipient":recipient,"thread_member":thread_member,"event_id":event_id,"event_sequence":event_sequence,"payload_sha256":payload_sha256}));
        }
        let next_cursor = if has_more {
            census.last().and_then(|item| item.get("entity_id")).and_then(Value::as_str).map(|entity_id| {
                encode_cursor_token(&json!({"schema":cursor_schema,"ledger_head":head,"query_digest":query_digest,"entity_id":entity_id}))
            })
        } else {
            None
        };
        Ok(
            json!({"schema":self.schema_id("communication_migration_preflight.v1"),"status":"ok","ledger_head":head,"query_digest":query_digest,"canonical_kind":communication.canonical_kind,"contract_version":communication.contract_version,"bounded":{"limit":limit,"returned":census.len(),"has_more":has_more,"next_cursor":next_cursor},"census":{"scope":"page","by_kind":by_kind,"by_sender":by_sender,"by_recipient":by_recipient,"messages":census},"proposed_operations":operations}),
        )
    }

    fn communication_migrate(
        &self,
        root: &Path,
        args: &Map<String, Value>,
    ) -> Result<Value, Value> {
        let actor = self.required(args, "actor")?;
        let authority_basis = self.required_object(args, "authority_basis")?;
        let preflight = self.communication_migration_preflight(root, args)?;
        let operations = preflight
            .get("proposed_operations")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if operations.is_empty() {
            return Ok(
                json!({"schema":self.schema_id("communication_migration.v1"),"status":"complete","migrated":0,"preflight":preflight}),
            );
        }
        let mut submit = Map::new();
        submit.insert("actor".into(), Value::String(actor));
        submit.insert("authority_basis".into(), authority_basis);
        submit.insert("operations".into(), Value::Array(operations.clone()));
        submit.insert(
            "expected_ledger_head".into(),
            preflight.get("ledger_head").cloned().unwrap_or(Value::Null),
        );
        submit.insert(
            "idempotency_key".into(),
            Value::String(format!(
                "communication-migration-{}",
                &sha256(&serde_json::to_vec(&operations).unwrap_or_default())[..24]
            )),
        );
        let admission = self.submit_review_admit(root, &submit)?;
        Ok(
            json!({"schema":self.schema_id("communication_migration.v1"),"status":"migrated","migrated":operations.len(),"preflight":preflight,"admission":admission}),
        )
    }

    fn sequence_create(&self, root: &Path, args: &Map<String, Value>) -> Result<Value, Value> {
        self.prepare(root)?;
        let name = self.validated_sequence_name(args)?;
        let actor = self.required(args, "actor")?;
        let authority_basis = self.required_object(args, "authority_basis")?;
        let start_at = self.optional_u64(args, "start_at", 1)?;
        if start_at < self.domain.features.sequences.start_at_min {
            return Err(self.error(
                "sequence_start_invalid",
                "sequence start_at must be at least 1",
                json!({"start_at":start_at}),
            ));
        }
        let lock_key = format!("sequence-{}", sha256(name.as_bytes()));
        self.with_authority_lock(root, &lock_key, || {
            let directory = self.sequence_directory(root, &name);
            let manifest_path = directory.join("sequence.json");
            if manifest_path.exists() {
                let manifest = self.read_json(&manifest_path)?;
                self.verify_sequence_manifest(&manifest, &name)?;
                if manifest.get("start_at").and_then(Value::as_u64) != Some(start_at) {
                    return Err(self.error(
                        "sequence_configuration_conflict",
                        "sequence already exists with a different start_at",
                        json!({"sequence_name":name,"existing_start_at":manifest.get("start_at"),"requested_start_at":start_at}),
                    ));
                }
                return self.sequence_status_value(root, &name, "already_exists");
            }
            fs::create_dir_all(directory.join("claims"))
                .map_err(self.io_error("sequence_claim_store_create_failed"))?;
            fs::create_dir_all(directory.join("idempotency"))
                .map_err(self.io_error("sequence_idempotency_store_create_failed"))?;
            let sequences = &self.domain.features.sequences;
            let mut manifest = json!({
                "schema":sequences.manifest_schema_id,
                "sequence_id":self.generated_sequence_id(&name),
                "sequence_name":name,
                "start_at":start_at,
                "step":sequences.step,
                "created_by":actor,
                "identity_state":Self::identity_state_for_event(&actor, "ledger.sequence_create"),
                "authority_basis":authority_basis,
                "idempotency_key":args.get("idempotency_key").cloned().unwrap_or(Value::Null),
                "created_at":now()
            });
            let hash = self.digest_value(&manifest)?;
            manifest[self.domain.features.sequences.manifest_hash_field.clone()] = json!(hash);
            self.write_new_json(&manifest_path, &manifest)?;
            self.sequence_status_value(root, &name, "created")
        })
    }

    fn sequence_status(&self, root: &Path, args: &Map<String, Value>) -> Result<Value, Value> {
        let name = self.validated_sequence_name(args)?;
        let lock_key = format!("sequence-{}", sha256(name.as_bytes()));
        self.with_authority_lock(root, &lock_key, || {
            self.sequence_status_value(root, &name, "ready")
        })
    }

    fn sequence_status_value(&self, root: &Path, name: &str, status: &str) -> Result<Value, Value> {
        let manifest = self.load_sequence_manifest(root, name)?;
        let claims = self.verified_sequence_claims(root, name, &manifest)?;
        let start_at = manifest["start_at"].as_u64().unwrap();
        let last_claim = claims.last().cloned().unwrap_or(Value::Null);
        let last_value = last_claim.get("value").and_then(Value::as_u64);
        let next_value = match last_value {
            Some(value) => value.checked_add(1).map(Value::from).unwrap_or(Value::Null),
            None => Value::from(start_at),
        };
        Ok(json!({
            "schema":self.domain.features.sequences.status_schema_id,
            "status":status,
            "sequence_id":manifest["sequence_id"],
            "sequence_name":name,
            "start_at":start_at,
            "step":self.domain.features.sequences.step,
            "claim_count":claims.len(),
            "last_claimed_value":last_value,
            "next_value":next_value,
            "exhausted":next_value.is_null(),
            "latest_claim":last_claim,
            "integrity_status":"valid"
        }))
    }

}
