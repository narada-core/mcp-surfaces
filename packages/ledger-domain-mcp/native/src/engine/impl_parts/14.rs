impl Engine {
    fn status(&self, root: &Path) -> Result<Value, Value> {
        self.prepare(root)?;
        event_ledger::verify(self.error, &self.ledger_layout(root), self.event_hash_field)?;
        let ledger_head = self.ledger_head(root)?;
        let event_count = self.ledger_files(root)?.len();
        let projection_path = self.projection_path(root);
        let projection_exists = projection_path.exists();
        let projection_current = projection_exists
            && self.projection_is_current(root, &ledger_head, event_count as u64)?;
        let projection_status = if projection_current {
            "current"
        } else if projection_exists {
            "stale"
        } else {
            "missing"
        };
        let (stored_entities, visible_entities, relations, records) = if projection_exists {
            let db = Connection::open_with_flags(
                &projection_path,
                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
            )
            .map_err(self.db_error("projection_open_failed"))?;
            let stored_entities: i64 = db
                .query_row(
                    &format!("select count(*) from {}", self.entity_table),
                    [],
                    |r| r.get(0),
                )
                .map_err(self.db_error("projection_count_failed"))?;
            let visible_entities: i64 = db
                .query_row(
                    &format!(
                        "select count(*) from {} where {}",
                        self.entity_table,
                        self.visible_entity_predicate()
                    ),
                    [],
                    |r| r.get(0),
                )
                .map_err(self.db_error("projection_count_failed"))?;
            let relations: i64 = db
                .query_row(
                    &format!("select count(*) from {}", self.relation_table),
                    [],
                    |r| r.get(0),
                )
                .map_err(self.db_error("projection_count_failed"))?;
            let records: i64 = db
                .query_row(
                    &format!("select count(*) from {}", self.records_table),
                    [],
                    |r| r.get(0),
                )
                .map_err(self.db_error("projection_count_failed"))?;
            (stored_entities, visible_entities, relations, records)
        } else {
            (0, 0, 0, 0)
        };
        Ok(json!({
            "schema":self.schema_id("status.v1"),"status":"ok",
            "implementation":self.domain.identity.implementation,
            "canonical_store":self.ledger(root).to_string_lossy(),
            "projection":projection_path.to_string_lossy(),
            "ledger_head":ledger_head,"event_count":event_count,
            "entity_count":visible_entities,"entity_count_semantics":"graph_visible",
            "stored_entity_count":stored_entities,
            "internal_entity_count":stored_entities - visible_entities,
            "relation_count":relations,"record_count":records,
            "projection_status":projection_status,
            "projection_current":projection_current,
            "projection_rebuildable":true,
            "status_rebuilds_projection":false,
            "truth_certification":false
        }))
    }
    /// Project one query row into the descriptor-listed field order. Row
    /// columns win, `"payload"` selects the full payload, anything else is
    /// looked up inside the payload (missing yields null, as before).
    fn project_row(
        row_values: &Map<String, Value>,
        payload: &Value,
        projection: &[String],
    ) -> Value {
        let mut out = Map::new();
        for field in projection {
            let value = if field == "payload" {
                payload.clone()
            } else if let Some(value) = row_values.get(field) {
                value.clone()
            } else {
                payload.get(field).cloned().unwrap_or(Value::Null)
            };
            out.insert(field.clone(), value);
        }
        Value::Object(out)
    }

    fn message_mark_read(&self, root: &Path, args: &Map<String, Value>) -> Result<Value, Value> {
        let read_receipt_kind = self.domain.query.read_receipt_kind.clone().ok_or_else(|| {
            self.error(
                "message_state_unavailable",
                "this domain does not configure durable message read receipts",
                Value::Null,
            )
        })?;
        let message_id = self.required(args, "message_id")?;
        let reader = self.required(args, "reader")?;
        let actor = self.required(args, "actor")?;
        let authority_basis = self.required_object(args, "authority_basis")?;
        let receipt_id = format!(
            "{}:{}",
            safe_name(&read_receipt_kind),
            &sha256(format!("{message_id}\0{reader}").as_bytes())[..24]
        );
        let (message_target, existing_receipt) = self.with_stable_projection(root, || {
            let db = Connection::open(self.projection_path(root))
                .map_err(self.db_error("projection_open_failed"))?;
            let entity_pk = self.table(&self.entity_table).primary_key.clone();
            let message_target = db
                .query_row(
                    &format!(
                        "select kind,payload_json from {} where {}=?1",
                        self.entity_table, entity_pk
                    ),
                    params![message_id],
                    |row| {
                        let kind = row.get::<_, String>(0)?;
                        let payload = row.get::<_, String>(1)?;
                        Ok((
                            kind,
                            serde_json::from_str::<Value>(&payload).unwrap_or(Value::Null),
                        ))
                    },
                )
                .optional()
                .map_err(self.db_error("message_read_target_lookup_failed"))?;
            let existing_receipt = db
                .query_row(
                    &format!(
                        "select kind,payload_json,event_id from {} where {}=?1",
                        self.entity_table, entity_pk
                    ),
                    params![receipt_id],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, String>(2)?,
                        ))
                    },
                )
                .optional()
                .map_err(self.db_error("message_read_receipt_lookup_failed"))?;
            Ok((message_target, existing_receipt))
        })?;
        let Some((message_kind, message_payload)) = message_target else {
            return Err(self.error(
                "message_not_found",
                "message_id must identify an existing entity",
                json!({"message_id":message_id}),
            ));
        };
        if !self.is_configured_message_kind(&message_kind) {
            return Err(self.error(
                "message_kind_invalid",
                "message_id must identify a configured communication kind",
                json!({"message_id":message_id,"kind":message_kind}),
            ));
        }
        let sender = message_payload.get("sender").and_then(Value::as_str);
        let recipient = message_payload.get("recipient").and_then(Value::as_str);
        let mut reader_is_participant = false;
        for participant in [sender, recipient].into_iter().flatten() {
            if participant == reader
                || self.participant_identities_equivalent(root, participant, &reader)?
            {
                reader_is_participant = true;
                break;
            }
        }
        if !reader_is_participant {
            return Err(self.error(
                "message_reader_not_participant",
                "reader must be the sender or recipient of the message",
                json!({"message_id":message_id,"reader":reader}),
            ));
        }
        if let Some((existing_kind, receipt_payload_json, event_id)) = existing_receipt {
            if existing_kind != read_receipt_kind {
                return Err(self.error(
                    "message_read_receipt_conflict",
                    "the deterministic read-receipt identity is already occupied by another entity kind",
                    json!({"receipt_id":receipt_id,"existing_kind":existing_kind,"expected_kind":read_receipt_kind}),
                ));
            }
            let receipt_payload =
                serde_json::from_str::<Value>(&receipt_payload_json).map_err(|_| {
                    self.error(
                        "message_read_receipt_corrupt",
                        "existing message read receipt payload is invalid",
                        json!({"receipt_id":receipt_id}),
                    )
                })?;
            if receipt_payload.get("message_id").and_then(Value::as_str)
                != Some(message_id.as_str())
                || receipt_payload.get("reader").and_then(Value::as_str) != Some(reader.as_str())
            {
                return Err(self.error(
                    "message_read_receipt_conflict",
                    "the existing read receipt does not match the requested message and reader",
                    json!({"receipt_id":receipt_id,"message_id":message_id,"reader":reader}),
                ));
            }
            let event = self.read_json(&self.ledger(root).join(format!("{event_id}.json")))?;
            let mut admission = self.admission_receipt(&event);
            if let Some(status) = admission.as_object_mut() {
                status.insert(
                    "status".into(),
                    Value::String("already_admitted".to_string()),
                );
            }
            return Ok(json!({
                "schema":self.schema_id("message_read.v1"),
                "status":"read",
                "replayed":true,
                "message_id":message_id,
                "reader":reader,
                "receipt_id":receipt_id,
                "read_at":receipt_payload.get("read_at"),
                "admission":admission
            }));
        }
        let read_at = args
            .get("read_at")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .map(str::to_string)
            .unwrap_or_else(now);
        let idempotency_key = args
            .get("idempotency_key")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| {
                format!(
                    "message-read-{}",
                    &sha256(format!("{message_id}\0{reader}").as_bytes())[..48]
                )
            });
        let operation = json!({
            "op":self.entity_op(),
            "entity_id":receipt_id,
            "kind":read_receipt_kind,
            "title":format!("Read receipt for {message_id}"),
            "message_id":message_id,
            "reader":reader,
            "read_at":read_at
        });
        let admission = self.submit_review_admit(
            root,
            &Map::from_iter([
                ("actor".into(), json!(actor)),
                ("authority_basis".into(), authority_basis),
                ("idempotency_key".into(), json!(idempotency_key)),
                ("operations".into(), json!([operation])),
            ]),
        )?;
        Ok(json!({
            "schema":self.schema_id("message_read.v1"),
            "status":"read",
            "message_id":message_id,
            "reader":reader,
            "receipt_id":receipt_id,
            "read_at":read_at,
            "admission":admission
        }))
    }

}
