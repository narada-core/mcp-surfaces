impl Engine {
    fn datom_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ledger_query::Datom> {
        let value_json: String = row.get(3)?;
        Ok(ledger_query::Datom {
            origin_id: row.get(0)?,
            subject: row.get(1)?,
            attribute: row.get(2)?,
            value: serde_json::from_str(&value_json).unwrap_or(Value::String(value_json)),
            event_sequence: row.get::<_, i64>(4)? as u64,
            event_id: row.get(5)?,
        })
    }

    fn load_datoms_sql(
        &self,
        db: &Connection,
        sql: &str,
        parameters: &[&dyn ToSql],
    ) -> Result<Vec<ledger_query::Datom>, Value> {
        let mut statement = db
            .prepare(sql)
            .map_err(self.db_error("projection_datom_prepare_failed"))?;
        let rows = statement
            .query_map(
                rusqlite::params_from_iter(parameters.iter().copied()),
                |row| Self::datom_from_row(row),
            )
            .map_err(self.db_error("projection_datom_query_failed"))?;
        rows.map(|row| row.map_err(self.db_error("projection_datom_row_failed")))
            .collect::<Result<Vec<_>, _>>()
    }

    fn append_datoms(
        &self,
        db: &Connection,
        sql: &str,
        parameters: &[&dyn ToSql],
        datoms: &mut Vec<ledger_query::Datom>,
        seen: &mut HashSet<String>,
        max_datoms: u64,
    ) -> Result<(), Value> {
        for datom in self.load_datoms_sql(db, sql, parameters)? {
            let key = format!(
                "{}\u{1}{}\u{1}{}\u{1}{}\u{1}{}\u{1}{}",
                datom.origin_id,
                datom.subject,
                datom.attribute,
                serde_json::to_string(&datom.value).unwrap_or_default(),
                datom.event_sequence,
                datom.event_id,
            );
            if seen.insert(key) {
                datoms.push(datom);
                if datoms.len() as u64 > max_datoms {
                    return Err(self.error(
                        "query_datom_scan_limit",
                        "normalized datom projection exceeds the descriptor scan budget",
                        json!({"max_datoms_scanned":max_datoms,"planner_mode":"indexed_seed_with_broad_join","cause":"an indexed clause or broad join exceeded the work budget","suggestion":"make the most selective indexed equality clause explicit and inspect broad join attributes"}),
                    ));
                }
            }
        }
        Ok(())
    }

    fn load_datoms(
        &self,
        db: &Connection,
        table: &str,
        max_datoms: u64,
    ) -> Result<Vec<ledger_query::Datom>, Value> {
        let row_limit = max_datoms.saturating_add(1) as i64;
        let sql = format!(
            "select origin_id,subject,attribute,value_json,event_sequence,event_id from {table} limit ?1"
        );
        let parameters: [&dyn ToSql; 1] = [&row_limit];
        let datoms = self.load_datoms_sql(db, &sql, &parameters)?;
        if datoms.len() as u64 > max_datoms {
            return Err(self.error(
                "query_datom_scan_limit",
                "normalized datom projection exceeds the descriptor scan budget",
                json!({"max_datoms_scanned":max_datoms,"planner_mode":"bounded_full_scan","cause":"query has no usable indexed seed","suggestion":"add an equality predicate on an indexed attribute before increasing the caller budget"}),
            ));
        }
        Ok(datoms)
    }

}
