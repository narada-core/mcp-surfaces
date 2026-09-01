impl Engine {
    fn pull_target(
        &self,
        db: &Connection,
        target_id: &str,
        fields: &[String],
        target_kind: Option<&str>,
    ) -> Result<(Value, bool), Value> {
        if let Some(target_kind) = target_kind {
            if !matches!(target_kind, "entity" | "relation" | "record") {
                return Err(self.error(
                    "query_pull_target_invalid",
                    "pull target_kind must be entity, relation, or record",
                    json!({"target_id":target_id,"target_kind":target_kind}),
                ));
            }
        }
        let mut matches: Vec<(&str, Value, bool)> = Vec::new();
        if target_kind.is_none() || target_kind == Some("entity") {
            let entity = db
                .query_row(
                    &format!("select entity_id,kind,payload_json,event_id,event_sequence from {} where entity_id=?1", self.entity_table),
                    params![target_id],
                    |row| {
                        let payload_json: String = row.get(2)?;
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            payload_json,
                            row.get::<_, String>(3)?,
                            row.get::<_, i64>(4)?,
                        ))
                    },
                )
                .optional()
                .map_err(self.db_error("projection_pull_query_failed"))?;
            if let Some((entity_id, kind, payload_json, event_id, event_sequence)) = entity {
                let payload = self.parse_pull_payload(target_id, &payload_json)?;
                let base = Map::from_iter([
                    ("entity_id".into(), json!(entity_id)),
                    ("kind".into(), json!(kind)),
                    ("event_id".into(), json!(event_id)),
                    ("event_sequence".into(), json!(event_sequence)),
                    ("payload".into(), payload.clone()),
                ]);
                matches.push((
                    "entity",
                    self.render_pull_fields(fields, &base, &payload),
                    true,
                ));
            }
        }
        if target_kind.is_none() || target_kind == Some("relation") {
            let relation = db
                .query_row(
                    &format!("select relation_id,relation_type,source_id,target_id,payload_json,event_id,event_sequence from {} where relation_id=?1", self.relation_table),
                    params![target_id],
                    |row| {
                        let payload_json: String = row.get(4)?;
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, String>(2)?,
                            row.get::<_, String>(3)?,
                            payload_json,
                            row.get::<_, String>(5)?,
                            row.get::<_, i64>(6)?,
                        ))
                    },
                )
                .optional()
                .map_err(self.db_error("projection_pull_query_failed"))?;
            if let Some((
                relation_id,
                relation_type,
                source_id,
                relation_target_id,
                payload_json,
                event_id,
                event_sequence,
            )) = relation
            {
                let payload = self.parse_pull_payload(target_id, &payload_json)?;
                let base = Map::from_iter([
                    ("relation_id".into(), json!(relation_id)),
                    ("relation_type".into(), json!(relation_type)),
                    ("source_id".into(), json!(source_id)),
                    ("target_id".into(), json!(relation_target_id)),
                    ("event_id".into(), json!(event_id)),
                    ("event_sequence".into(), json!(event_sequence)),
                    ("payload".into(), payload.clone()),
                ]);
                matches.push((
                    "relation",
                    self.render_pull_fields(fields, &base, &payload),
                    false,
                ));
            }
        }
        if target_kind.is_none() || target_kind == Some("record") {
            let record = db
                .query_row(
                    &format!("select record_id,record_kind,payload_json,event_id,event_sequence from {} where record_id=?1", self.records_table),
                    params![target_id],
                    |row| {
                        let payload_json: String = row.get(2)?;
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            payload_json,
                            row.get::<_, String>(3)?,
                            row.get::<_, i64>(4)?,
                        ))
                    },
                )
                .optional()
                .map_err(self.db_error("projection_pull_query_failed"))?;
            if let Some((record_id, record_kind, payload_json, event_id, event_sequence)) = record {
                let payload = self.parse_pull_payload(target_id, &payload_json)?;
                let base = Map::from_iter([
                    ("record_id".into(), json!(record_id)),
                    ("record_kind".into(), json!(record_kind)),
                    ("event_id".into(), json!(event_id)),
                    ("event_sequence".into(), json!(event_sequence)),
                    ("payload".into(), payload.clone()),
                ]);
                matches.push((
                    "record",
                    self.render_pull_fields(fields, &base, &payload),
                    false,
                ));
            }
        }
        if matches.len() > 1 {
            let target_kinds = matches.iter().map(|(kind, _, _)| *kind).collect::<Vec<_>>();
            return Err(self.error(
                "query_pull_target_ambiguous",
                "pull target id exists in more than one projection; specify target_kind",
                json!({"target_id":target_id,"target_kinds":target_kinds}),
            ));
        }
        if let Some((_, value, is_entity)) = matches.pop() {
            return Ok((value, is_entity));
        }
        Err(self.error(
            "query_pull_target_not_found",
            "pull target was not found in the requested entity, relation, or record projection",
            json!({"target_id":target_id,"target_kind":target_kind,"target_kinds":["entity","relation","record"]}),
        ))
    }

    fn query(&self, root: &Path, args: &Map<String, Value>) -> Result<Value, Value> {
        self.with_stable_projection(root, || self.query_locked(root, args))
    }

    fn query_locked(&self, root: &Path, args: &Map<String, Value>) -> Result<Value, Value> {
        let ledger_head = self.ledger_head(root)?;
        if let Some(expected) = args.get("expected_ledger_head").and_then(Value::as_str) {
            if Some(expected) != ledger_head.as_deref() {
                return Err(self.error(
                    "ledger_head_mismatch",
                    "query expected_ledger_head does not match the current ledger head",
                    json!({"expected_ledger_head":expected,"actual_ledger_head":ledger_head}),
                ));
            }
        }
        let db = Connection::open(self.projection_path(root))
            .map_err(self.db_error("projection_open_failed"))?;
        let limit = args
            .get("limit")
            .and_then(Value::as_u64)
            .unwrap_or(self.domain.caps.query_limit.default)
            .clamp(
                self.domain.caps.query_limit.min,
                self.domain.caps.query_limit.max,
            );
        let offset = args.get("offset").and_then(Value::as_u64).unwrap_or(0);
        let kind = args.get("kind").and_then(Value::as_str).unwrap_or("");
        let compact = args
            .get("compact")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let text = args.get("text").and_then(Value::as_str).unwrap_or("");
        let like = format!("%{text}%");
        if let Some(record_kind) = args.get("record_kind").and_then(Value::as_str) {
            let sql = format!("select record_id,record_kind,payload_json,event_id from {} where record_kind=?1 and (?2='' or payload_json like ?3) order by record_id limit ?4 offset ?5", self.records_table);
            let mut stmt = db
                .prepare(&sql)
                .map_err(self.db_error("projection_record_query_prepare_failed"))?;
            let projection = if compact {
                &self.domain.query.record_compact_projection
            } else {
                &self.domain.query.record_full_projection
            };
            let rows = stmt
                .query_map(params![record_kind, text, like, limit, offset], |row| {
                    let payload = serde_json::from_str::<Value>(&row.get::<_, String>(2)?)
                        .unwrap_or(Value::Null);
                    let mut row_values = Map::new();
                    row_values.insert("record_id".into(), json!(row.get::<_, String>(0)?));
                    row_values.insert("record_kind".into(), json!(row.get::<_, String>(1)?));
                    row_values.insert("event_id".into(), json!(row.get::<_, String>(3)?));
                    Ok(Self::project_row(&row_values, &payload, projection))
                })
                .map_err(self.db_error("projection_record_query_failed"))?;
            let items = rows
                .collect::<Result<Vec<_>, _>>()
                .map_err(self.db_error("projection_record_query_row_failed"))?;
            let mut response = json!({"schema":self.schema_id("query.v1"),"status":"ok","result_kind":"records","record_kind":record_kind,"compact":compact,"ledger_head":ledger_head,"items":items,"offset":offset,"limit":limit,"returned":items.len(),"bounded":true,"max_output_bytes":self.domain.caps.query_execution.max_output_bytes});
            self.finalize_bounded_output(&mut response)?;
            return Ok(response);
        }
        let accepted_kind_values = if kind.is_empty() {
            Vec::new()
        } else {
            self.expand_legacy_kind_value(kind)?
        };
        let accepted_kinds = accepted_kind_values
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect::<Vec<_>>();
        let kind_predicate = if accepted_kinds.is_empty() {
            self.visible_entity_predicate()
        } else {
            let literals = accepted_kinds
                .iter()
                .map(|value| Self::sql_quote(value))
                .collect::<Vec<_>>()
                .join(",");
            format!("kind in ({literals})")
        };
        let sql = format!("select entity_id,kind,payload_json,event_id from {} where {kind_predicate} and (?1='' or payload_json like ?2) order by entity_id limit ?3 offset ?4", self.entity_table);
        let mut stmt = db
            .prepare(&sql)
            .map_err(self.db_error("projection_query_prepare_failed"))?;
        let projection = if compact {
            &self.domain.query.entity_compact_projection
        } else {
            &self.domain.query.entity_full_projection
        };
        let rows = stmt
            .query_map(params![text, like, limit, offset], |row| {
                let payload =
                    serde_json::from_str::<Value>(&row.get::<_, String>(2)?).unwrap_or(Value::Null);
                let mut row_values = Map::new();
                row_values.insert("entity_id".into(), json!(row.get::<_, String>(0)?));
                row_values.insert("kind".into(), json!(row.get::<_, String>(1)?));
                row_values.insert("event_id".into(), json!(row.get::<_, String>(3)?));
                Ok(Self::project_row(&row_values, &payload, projection))
            })
            .map_err(self.db_error("projection_query_failed"))?;
        let items = rows
            .collect::<Result<Vec<_>, _>>()
            .map_err(self.db_error("projection_query_row_failed"))?;
        let mut response = json!({"schema":self.schema_id("query.v1"),"status":"ok","result_kind":"entities","kind":kind,"expanded_kinds":accepted_kinds,"compact":compact,"ledger_head":ledger_head,"items":items,"offset":offset,"limit":limit,"returned":items.len(),"bounded":true,"max_output_bytes":self.domain.caps.query_execution.max_output_bytes});
        self.finalize_bounded_output(&mut response)?;
        Ok(response)
    }

    fn snapshot(&self, root: &Path, args: &Map<String, Value>) -> Result<Value, Value> {
        self.with_stable_projection(root, || self.snapshot_locked(root, args))
    }

}
