impl Engine {
    fn participant_identities_equivalent(
        &self,
        root: &Path,
        left: &str,
        right: &str,
    ) -> Result<bool, Value> {
        if left == right {
            return Ok(true);
        }
        let Some(identity) = self
            .domain
            .query
            .named_queries
            .values()
            .filter_map(Value::as_object)
            .find_map(|config| config.get("participant_identity"))
            .and_then(Value::as_object)
        else {
            return Ok(false);
        };
        let Some(kind) = identity.get("kind").and_then(Value::as_str) else {
            return Ok(false);
        };
        let fields = identity
            .get("alias_fields")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>();
        let db = Connection::open(self.projection_path(root))
            .map_err(self.db_error("projection_open_failed"))?;
        let mut statement = db
            .prepare(&format!(
                "select entity_id,payload_json from {} where kind=?1",
                self.entity_table
            ))
            .map_err(self.db_error("projection_identity_prepare_failed"))?;
        let rows = statement
            .query_map(params![kind], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(self.db_error("projection_identity_query_failed"))?;
        for row in rows {
            let (entity_id, payload_json) =
                row.map_err(self.db_error("projection_identity_row_failed"))?;
            let payload: Value = serde_json::from_str(&payload_json).unwrap_or(Value::Null);
            let matches = |candidate: &str| {
                entity_id == candidate
                    || fields
                        .iter()
                        .any(|field| payload.get(*field).and_then(Value::as_str) == Some(candidate))
            };
            if matches(left) && matches(right) {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn generic_query(&self, root: &Path, args: &Map<String, Value>) -> Result<Value, Value> {
        self.with_stable_projection(root, || self.generic_query_locked(root, args))
    }

}
