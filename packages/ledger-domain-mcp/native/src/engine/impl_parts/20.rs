impl Engine {
    fn load_datoms_for_query(
        &self,
        db: &Connection,
        table: &str,
        spec: &ledger_query::QuerySpec,
        subject_local_sequence: Option<(&str, u64)>,
        subject_seed_attribute: Option<&str>,
    ) -> Result<Vec<ledger_query::Datom>, Value> {
        let max_datoms = spec.limits.max_datoms_scanned as u64;
        let mut plan = QuerySeedPlan::default();
        // A subject-local sequence suffix already provides the authoritative
        // positive seed for inbox queries. Exists/not-exists clauses encode
        // read/reply decoration and must be evaluated only after the selected
        // message subjects are hydrated; treating them as global seeds
        // defeats the suffix budget.
        collect_query_seed_clauses(
            &spec.clauses,
            &spec.inputs,
            &mut plan,
            subject_local_sequence.is_none(),
        );

        // Pull decoration derives durable read/reply state from normalized
        // datoms too. Load those narrow attributes alongside the selected
        // subjects without making their subjects seed a second expansion.
        if !spec.pulls.is_empty() && subject_local_sequence.is_none() {
            for attribute in [
                self.domain.query.reply_state_attribute.as_deref(),
                self.domain.query.read_receipt_kind_attribute.as_deref(),
                self.domain.query.read_receipt_message_attribute.as_deref(),
                self.domain.query.read_receipt_reader_attribute.as_deref(),
            ]
            .into_iter()
            .flatten()
            {
                plan.push_attribute(attribute);
            }
        }

        // A variable attribute cannot be safely narrowed by the available
        // indexes. Preserve complete query semantics for that shape by using
        // the bounded full-scan path.
        if plan.unindexable
            || (plan.attribute_values.is_empty()
                && plan.subject_attributes.is_empty()
                && plan.attributes.is_empty())
        {
            return self.load_datoms(db, table, max_datoms);
        }

        let mut datoms = Vec::new();
        let mut seen = HashSet::new();
        let mut candidate_subjects = HashSet::new();
        let row_limit = max_datoms.saturating_add(1) as i64;

        for (attribute, value) in &plan.attribute_values {
            // Named inbox queries have a descriptor-declared participant
            // equality.  It is the authoritative subject seed; the complete
            // subject hydration below already supplies every other equality
            // fact, so scanning broad kind/intent predicates would only add
            // unrelated subjects and exhaust the work budget.
            if subject_local_sequence.is_some()
                && subject_seed_attribute.is_some_and(|seed| attribute != seed)
            {
                continue;
            }
            let value_json = serde_json::to_string(value).map_err(|error| {
                self.error(
                    "projection_datom_value_encode_failed",
                    &error.to_string(),
                    Value::Null,
                )
            })?;
            let before = datoms.len();
            if let Some((sequence_attribute, since_event)) = subject_local_sequence {
                let parameters: [&dyn ToSql; 5] = [
                    attribute,
                    &value_json,
                    &sequence_attribute,
                    &since_event,
                    &row_limit,
                ];
                self.append_datoms(
                    db,
                    &format!(
                        "select d.origin_id,d.subject,d.attribute,d.value_json,d.event_sequence,d.event_id from {table} d where d.attribute=?1 and d.value_json=?2 and exists (select 1 from {table} suffix where suffix.subject=d.subject and suffix.attribute=?3 and cast(suffix.value_json as integer)>?4) limit ?5"
                    ),
                    &parameters,
                    &mut datoms,
                    &mut seen,
                    max_datoms,
                )?;
            } else {
                let parameters: [&dyn ToSql; 3] = [attribute, &value_json, &row_limit];
                self.append_datoms(
                    db,
                    &format!(
                        "select origin_id,subject,attribute,value_json,event_sequence,event_id from {table} where attribute=?1 and value_json=?2 limit ?3"
                    ),
                    &parameters,
                    &mut datoms,
                    &mut seen,
                    max_datoms,
                )?;
            }
            for datom in datoms[before..].iter() {
                candidate_subjects.insert(datom.subject.clone());
            }
        }

        for (subject, attribute) in &plan.subject_attributes {
            let parameters: [&dyn ToSql; 3] = [subject, attribute, &row_limit];
            let before = datoms.len();
            self.append_datoms(
                db,
                &format!(
                    "select origin_id,subject,attribute,value_json,event_sequence,event_id from {table} where subject=?1 and attribute=?2 limit ?3"
                ),
                &parameters,
                &mut datoms,
                &mut seen,
                max_datoms,
            )?;
            for datom in datoms[before..].iter() {
                candidate_subjects.insert(datom.subject.clone());
            }
        }

        // Once a selective indexed predicate has identified subjects, load
        // their complete normalized records so joins, comparisons, and pull
        // decoration see the same subject-local facts as a full scan.
        for subject in &candidate_subjects {
            let parameters: [&dyn ToSql; 2] = [subject, &row_limit];
            self.append_datoms(
                db,
                &format!(
                    "select origin_id,subject,attribute,value_json,event_sequence,event_id from {table} where subject=?1 limit ?2"
                ),
                &parameters,
                &mut datoms,
                &mut seen,
                max_datoms,
            )?;
        }

        // Suffix-planned inbox queries must not turn pull decoration back
        // into a global attribute scan. Resolve reply targets and read
        // receipts by the already-selected message ids, then hydrate only
        // those decoration subjects.
        if !spec.pulls.is_empty() && subject_local_sequence.is_some() {
            let mut decoration_subjects = HashSet::new();
            for subject in &candidate_subjects {
                let value_json = serde_json::to_string(subject).map_err(|error| {
                    self.error(
                        "projection_datom_value_encode_failed",
                        &error.to_string(),
                        Value::Null,
                    )
                })?;
                for attribute in [
                    self.domain.query.reply_state_attribute.as_deref(),
                    self.domain.query.read_receipt_message_attribute.as_deref(),
                ]
                .into_iter()
                .flatten()
                {
                    let parameters: [&dyn ToSql; 3] = [&attribute, &value_json, &row_limit];
                    let before = datoms.len();
                    self.append_datoms(
                        db,
                        &format!(
                            "select origin_id,subject,attribute,value_json,event_sequence,event_id from {table} where attribute=?1 and value_json=?2 limit ?3"
                        ),
                        &parameters,
                        &mut datoms,
                        &mut seen,
                        max_datoms,
                    )?;
                    for datom in datoms[before..].iter() {
                        decoration_subjects.insert(datom.subject.clone());
                    }
                }
            }
            for subject in decoration_subjects {
                let parameters: [&dyn ToSql; 2] = [&subject, &row_limit];
                self.append_datoms(
                    db,
                    &format!(
                        "select origin_id,subject,attribute,value_json,event_sequence,event_id from {table} where subject=?1 limit ?2"
                    ),
                    &parameters,
                    &mut datoms,
                    &mut seen,
                    max_datoms,
                )?;
            }
        }

        // Broad attributes are still needed for joins, correlated predicates,
        // reachability, and durable message-state decoration. A named query
        // may declare one attribute subject-local when another indexed clause
        // has already selected the complete candidate records.
        if subject_local_sequence.is_none() {
            for attribute in &plan.attributes {
                let parameters: [&dyn ToSql; 2] = [attribute, &row_limit];
                self.append_datoms(
                    db,
                    &format!(
                        "select origin_id,subject,attribute,value_json,event_sequence,event_id from {table} where attribute=?1 limit ?2"
                    ),
                    &parameters,
                    &mut datoms,
                    &mut seen,
                    max_datoms,
                )?;
            }
        }
        Ok(datoms)
    }
    fn entity_kind(&self, db: &Connection, entity_id: &str) -> Result<String, Value> {
        db.query_row(
            &format!("select kind from {} where entity_id=?1", self.entity_table),
            params![entity_id],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(self.db_error("query_entity_kind_failed"))?
        .ok_or_else(|| {
            self.error(
                "query_entity_not_found",
                "pull target entity was not found",
                json!({"entity_id":entity_id}),
            )
        })
    }

}
