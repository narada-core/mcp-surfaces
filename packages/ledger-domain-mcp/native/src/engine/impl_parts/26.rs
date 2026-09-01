impl Engine {
    fn projection_is_current(
        &self,
        root: &Path,
        ledger_head: &Option<String>,
        ledger_sequence: u64,
    ) -> Result<bool, Value> {
        let Some(table) = &self.projection_meta_table else {
            return Ok(false);
        };
        let path = self.projection_path(root);
        if !path.exists() {
            return Ok(false);
        }
        let db = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(self.db_error("projection_open_failed"))?;
        let stored = db
            .query_row(
                &format!("select ledger_head,ledger_sequence from {table} where meta_id='current'"),
                [],
                |row| Ok((row.get::<_, Option<String>>(0)?, row.get::<_, i64>(1)?)),
            )
            .optional();
        let Ok(Some((stored_head, stored_sequence))) = stored else {
            // A projection built by an older descriptor is disposable. A
            // missing metadata table/row therefore means rebuild, not a
            // surfaced corruption error.
            return Ok(false);
        };
        Ok(stored_head.as_deref() == ledger_head.as_deref()
            && stored_sequence == ledger_sequence as i64)
    }

    fn rebuild_projection_locked(&self, root: &Path) -> Result<(), Value> {
        event_ledger::verify(self.error, &self.ledger_layout(root), self.event_hash_field)?;
        let ledger_files = self.ledger_files(root)?;
        let ledger_head = self.ledger_head(root)?;
        let ledger_sequence = ledger_files.len() as u64;
        if self.projection_is_current(root, &ledger_head, ledger_sequence)? {
            return Ok(());
        }
        if self.catch_up_projection_locked(root, &ledger_files)? {
            return Ok(());
        }
        let ddl = self.domain.projection.ddl.clone();
        ledger_projection::rebuild_projection(
            self.error,
            &self.ledger_layout(root),
            self.event_hash_field,
            &self.projection_path(root),
            &ddl,
            |tx, event, event_id| self.fold_projection_event(tx, event, event_id),
        )
    }

    fn catch_up_projection_locked(
        &self,
        root: &Path,
        ledger_files: &[PathBuf],
    ) -> Result<bool, Value> {
        let Some(meta_table) = &self.projection_meta_table else {
            return Ok(false);
        };
        let projection_path = self.projection_path(root);
        if !projection_path.exists() {
            return Ok(false);
        }
        let mut db =
            Connection::open(&projection_path).map_err(self.db_error("projection_open_failed"))?;
        let stored = db
            .query_row(
                &format!(
                    "select ledger_head,ledger_sequence from {meta_table} where meta_id='current'"
                ),
                [],
                |row| Ok((row.get::<_, Option<String>>(0)?, row.get::<_, i64>(1)?)),
            )
            .optional();
        let Ok(Some((stored_head, stored_sequence))) = stored else {
            return Ok(false);
        };
        if stored_sequence < 0 || stored_sequence as usize > ledger_files.len() {
            return Ok(false);
        }
        let prefix_head = if stored_sequence == 0 {
            None
        } else {
            let event = self.read_json(&ledger_files[stored_sequence as usize - 1])?;
            event
                .get(self.event_hash_field)
                .and_then(Value::as_str)
                .map(str::to_string)
        };
        if prefix_head != stored_head {
            return Ok(false);
        }

        let tx = db
            .transaction()
            .map_err(self.db_error("projection_increment_begin_failed"))?;
        for path in ledger_files.iter().skip(stored_sequence as usize) {
            let event = self.read_json(path)?;
            let event_id = event["event_id"].as_str().ok_or_else(|| {
                self.error(
                    "projection_event_invalid",
                    "ledger event lacks event_id",
                    json!({"path":path}),
                )
            })?;
            self.fold_projection_event(&tx, &event, event_id)?;
        }
        tx.commit()
            .map_err(self.db_error("projection_increment_commit_failed"))?;
        Ok(true)
    }

    fn fold_projection_event(
        &self,
        tx: &Transaction<'_>,
        event: &Value,
        event_id: &str,
    ) -> Result<(), Value> {
        for op in event["operations"].as_array().into_iter().flatten() {
            let op_kind = op["op"].as_str().unwrap_or_default();
            if op_kind == self.domain.query.communication.canonicalization_operation {
                let entity_id = op
                    .get("entity_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let canonical_kind = op
                    .get("canonical_kind")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let changed = tx
                    .execute(
                        &format!(
                            "update {} set kind=?1 where entity_id=?2",
                            self.entity_table
                        ),
                        params![canonical_kind, entity_id],
                    )
                    .map_err(self.db_error("projection_entity_canonicalization_failed"))?;
                if changed != 1 {
                    return Err(self.error(
                        "projection_entity_canonicalization_missing",
                        "canonicalization target is absent from the projection",
                        json!({"entity_id":entity_id}),
                    ));
                }
                if let Some(table) = &self.datoms_table {
                    tx.execute(
                        &format!("delete from {table} where origin_id=?1 and attribute='narada.ledger:entity/kind'"),
                        params![entity_id],
                    ).map_err(self.db_error("projection_datom_delete_failed"))?;
                    let sequence = event["sequence"].as_u64().unwrap_or_default();
                    self.write_datom(
                        tx,
                        table,
                        entity_id,
                        entity_id,
                        "narada.ledger:entity/kind",
                        &Value::String(canonical_kind.to_string()),
                        sequence,
                        event_id,
                    )?;
                    self.write_datom(
                        tx,
                        table,
                        entity_id,
                        entity_id,
                        "narada.ledger:entity/kind_canonicalized_from",
                        op.get("legacy_kind").unwrap_or(&Value::Null),
                        sequence,
                        event_id,
                    )?;
                    self.write_datom(
                        tx,
                        table,
                        entity_id,
                        entity_id,
                        "narada.ledger:entity/kind_canonicalization_event",
                        &Value::String(event_id.to_string()),
                        sequence,
                        event_id,
                    )?;
                }
                continue;
            }
            let Some(fold) = self
                .domain
                .projection
                .fold
                .iter()
                .find(|entry| entry.operation == op_kind)
            else {
                continue;
            };
            let table = self.table(&fold.table);
            let placeholders = (1..=table.columns.len())
                .map(|index| format!("?{index}"))
                .collect::<Vec<_>>()
                .join(",");
            let sql = format!(
                "{} into {} values({})",
                self.domain.projection.write_mode, table.name, placeholders
            );
            let mut values = Vec::with_capacity(table.columns.len());
            for column in &table.columns {
                let value = if *column == table.primary_key {
                    op.get(&fold.key_field)
                        .and_then(Value::as_str)
                        .unwrap()
                        .to_string()
                } else if column == "payload_json" {
                    op.to_string()
                } else if column == "event_id" {
                    event_id.to_string()
                } else if column == "event_sequence" {
                    event["sequence"].as_u64().unwrap_or_default().to_string()
                } else {
                    let mapping = fold
                        .columns
                        .get(column)
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    op.get(mapping)
                        .and_then(Value::as_str)
                        .unwrap_or(mapping)
                        .to_string()
                };
                values.push(value);
            }
            let code = if table.name == self.entity_table {
                "projection_entity_write_failed"
            } else if table.name == self.relation_table {
                "projection_relation_write_failed"
            } else {
                "projection_assessment_write_failed"
            };
            tx.execute(&sql, rusqlite::params_from_iter(values))
                .map_err(self.db_error(code))?;
            self.emit_datoms(tx, op, event, event_id, op_kind)?;
        }
        if let Some(table) = &self.projection_meta_table {
            tx.execute(
                &format!(
                    "insert or replace into {table}(meta_id,ledger_head,ledger_sequence,updated_event_id) values('current',?1,?2,?3)"
                ),
                params![
                    event.get(self.event_hash_field).and_then(Value::as_str),
                    event["sequence"].as_u64().unwrap_or_default() as i64,
                    event_id,
                ],
            ).map_err(self.db_error("projection_metadata_write_failed"))?;
        }
        Ok(())
    }

}
