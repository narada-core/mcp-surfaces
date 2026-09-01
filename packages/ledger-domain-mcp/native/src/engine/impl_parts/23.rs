impl Engine {
    fn snapshot_locked(&self, root: &Path, args: &Map<String, Value>) -> Result<Value, Value> {
        let feature = &self.domain.features.snapshot;
        let ledger_head = self.ledger_head(root)?;
        if let Some(expected) = args.get("expected_ledger_head").and_then(Value::as_str) {
            if Some(expected) != ledger_head.as_deref() {
                return Err(self.error(
                    &feature.head_mismatch_refusal_code,
                    "The graph changed after the requested snapshot began.",
                    json!({"expected_ledger_head":expected,"actual_ledger_head":ledger_head}),
                ));
            }
        }
        let caps = &self.domain.caps.snapshot_limit;
        let limit = args
            .get("limit")
            .and_then(Value::as_u64)
            .unwrap_or(caps.default)
            .clamp(caps.min, caps.max);
        let entity_offset = args
            .get("entity_offset")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let relation_offset = args
            .get("relation_offset")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let db = Connection::open(self.projection_path(root))
            .map_err(self.db_error("projection_open_failed"))?;
        let visible_entities = self.visible_entity_predicate();
        let entity_count: i64 = db
            .query_row(
                &format!(
                    "select count(*) from {} where {visible_entities}",
                    self.entity_table
                ),
                [],
                |row| row.get(0),
            )
            .map_err(self.db_error("projection_count_failed"))?;
        let relation_count: i64 = db
            .query_row(
                &format!("select count(*) from {}", self.relation_table),
                [],
                |row| row.get(0),
            )
            .map_err(self.db_error("projection_count_failed"))?;

        let mut entity_statement = db
            .prepare(&format!("select entity_id,kind,payload_json,event_id from {} where {visible_entities} order by entity_id limit ?1 offset ?2", self.entity_table))
            .map_err(self.db_error("projection_snapshot_entities_prepare_failed"))?;
        let entities = entity_statement
            .query_map(params![limit, entity_offset], |row| {
                let payload =
                    serde_json::from_str::<Value>(&row.get::<_, String>(2)?).unwrap_or(Value::Null);
                Ok(json!({
                    "entity_id":row.get::<_,String>(0)?,
                    "kind":row.get::<_,String>(1)?,
                    "title":payload.get("title"),
                    "payload":payload,
                    "event_id":row.get::<_,String>(3)?
                }))
            })
            .map_err(self.db_error("projection_snapshot_entities_failed"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(self.db_error("projection_snapshot_entity_row_failed"))?;

        let mut relation_statement = db
            .prepare(&format!("select relation_id,relation_type,source_id,target_id,payload_json,event_id from {} order by relation_id limit ?1 offset ?2", self.relation_table))
            .map_err(self.db_error("projection_snapshot_relations_prepare_failed"))?;
        let relations = relation_statement
            .query_map(params![limit, relation_offset], |row| {
                let payload =
                    serde_json::from_str::<Value>(&row.get::<_, String>(4)?).unwrap_or(Value::Null);
                Ok(json!({
                    "relation_id":row.get::<_,String>(0)?,
                    "relation_type":row.get::<_,String>(1)?,
                    "source_id":row.get::<_,String>(2)?,
                    "target_id":row.get::<_,String>(3)?,
                    "payload":payload,
                    "event_id":row.get::<_,String>(5)?
                }))
            })
            .map_err(self.db_error("projection_snapshot_relations_failed"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(self.db_error("projection_snapshot_relation_row_failed"))?;

        let next_entity_offset = entity_offset + entities.len() as u64;
        let next_relation_offset = relation_offset + relations.len() as u64;
        Ok(json!({
            "schema":feature.response_schema_id,
            "status":"ok",
            "ledger_head":ledger_head,
            "entity_count":entity_count,
            "relation_count":relation_count,
            "entities":entities,
            "relations":relations,
            "entity_offset":entity_offset,
            "relation_offset":relation_offset,
            "next_entity_offset":(next_entity_offset < entity_count as u64).then_some(next_entity_offset),
            "next_relation_offset":(next_relation_offset < relation_count as u64).then_some(next_relation_offset),
            "limit":limit,
            "bounded":true
        }))
    }

    fn query_batch(&self, root: &Path, args: &Map<String, Value>) -> Result<Value, Value> {
        self.with_stable_projection(root, || self.query_batch_locked(root, args))
    }

}
