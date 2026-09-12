impl Engine {
    fn source_inspect(&self, root: &Path, args: &Map<String, Value>) -> Result<Value, Value> {
        let feature = &self.domain.features.source_inspect;
        let caps = &self.domain.caps.source_inspect;
        let paths = args.get("paths").and_then(Value::as_array).ok_or_else(|| {
            self.error(
                "invalid_source_inspection",
                "paths must be an array",
                Value::Null,
            )
        })?;
        if (paths.len() as u64) < caps.paths_min || (paths.len() as u64) > caps.paths_max {
            return Err(self.error(
                "invalid_source_inspection",
                &format!(
                    "paths count must be between {} and {}",
                    caps.paths_min, caps.paths_max
                ),
                json!({"count":paths.len()}),
            ));
        }
        let max_sections = args
            .get("max_sections_per_file")
            .and_then(Value::as_u64)
            .unwrap_or(caps.sections_default)
            .min(caps.sections_max) as usize;
        let max_chars = args
            .get("max_chars_per_section")
            .and_then(Value::as_u64)
            .unwrap_or(caps.chars_default)
            .clamp(caps.chars_min, caps.chars_max) as usize;
        let canonical_root =
            fs::canonicalize(root).map_err(self.io_error("site_root_resolve_failed"))?;
        let relevant = &feature.keywords;
        let mut files = Vec::with_capacity(paths.len());
        for value in paths {
            let locator = value.as_str().ok_or_else(|| {
                self.error(
                    "invalid_source_inspection",
                    "each path must be a string",
                    Value::Null,
                )
            })?;
            let requested = PathBuf::from(locator);
            let candidate = if requested.is_absolute() {
                requested
            } else {
                canonical_root.join(requested)
            };
            let canonical =
                fs::canonicalize(&candidate).map_err(self.io_error("source_resolve_failed"))?;
            if !canonical.starts_with(&canonical_root) {
                return Err(self.error(
                    &feature.outside_refusal_code,
                    "source path must remain inside the site root",
                    json!({"path":locator}),
                ));
            }
            let metadata =
                fs::metadata(&canonical).map_err(self.io_error("source_metadata_failed"))?;
            if metadata.len() > caps.file_bytes_max {
                return Err(self.error(
                    &feature.too_large_refusal_code,
                    "source exceeds the 1 MiB inspection limit",
                    json!({"path":locator,"size":metadata.len(),"max_size":caps.file_bytes_max}),
                ));
            }
            let content =
                fs::read_to_string(&canonical).map_err(self.io_error("source_read_failed"))?;
            let lines = content.lines().collect::<Vec<_>>();
            let headings = lines
                .iter()
                .enumerate()
                .filter_map(|(index, line)| {
                    let trimmed = line.trim_start();
                    trimmed
                        .starts_with('#')
                        .then_some((index, trimmed.trim_start_matches('#').trim()))
                })
                .collect::<Vec<_>>();
            let title = headings.first().map(|(_, heading)| *heading);
            let mut sections = Vec::new();
            for (heading_index, (start, heading)) in headings.iter().enumerate() {
                let normalized = heading.to_ascii_lowercase();
                if !relevant.iter().any(|needle| normalized.contains(needle)) {
                    continue;
                }
                let end = headings
                    .get(heading_index + 1)
                    .map(|(line, _)| *line)
                    .unwrap_or(lines.len());
                let full = lines[*start..end].join("\n");
                let excerpt = full.chars().take(max_chars).collect::<String>();
                sections.push(json!({
                    "heading":heading,
                    "start_line":start + 1,
                    "end_line":end,
                    "excerpt":excerpt,
                    "truncated":full.chars().count() > max_chars
                }));
                if sections.len() == max_sections {
                    break;
                }
            }
            files.push(json!({
                "path":locator,
                "title":title,
                "line_count":lines.len(),
                "sections":sections,
                "section_count":sections.len(),
                "sections_truncated":headings.iter().filter(|(_, heading)| {
                    let normalized = heading.to_ascii_lowercase();
                    relevant.iter().any(|needle| normalized.contains(needle))
                }).count() > sections.len()
            }));
        }
        Ok(json!({
            "schema":feature.response_schema_id,
            "status":"ok",
            "file_count":files.len(),
            "files":files,
            "bounded":true
        }))
    }

    fn concept_resolve(&self, root: &Path, args: &Map<String, Value>) -> Result<Value, Value> {
        self.with_stable_projection(root, || self.concept_resolve_locked(root, args))
    }

    fn concept_resolve_locked(&self, root: &Path, args: &Map<String, Value>) -> Result<Value, Value> {
        const COMPONENT_LIMIT: usize = 16;
        const PROVENANCE_LIMIT: usize = 4;
        const OUTPUT_LIMIT: usize = 6_000;

        let requested = self.required(args, "canonical_name")?;
        let normalized = requested.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase();
        let db = Connection::open(self.projection_path(root))
            .map_err(self.db_error("projection_open_failed"))?;
        let visible = self.visible_entity_predicate();
        let mut entity_stmt = db.prepare(&format!(
            "select entity_id,payload_json from {} where kind='marici:concept' and {visible} order by entity_id",
            self.entity_table
        )).map_err(self.db_error("concept_resolve_prepare_failed"))?;
        let candidates = entity_stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        }).map_err(self.db_error("concept_resolve_query_failed"))?
          .collect::<Result<Vec<_>, _>>()
          .map_err(self.db_error("concept_resolve_row_failed"))?
          .into_iter()
          .filter_map(|(id, raw)| {
              let payload = serde_json::from_str::<Value>(&raw).ok()?;
              let name = payload.get("canonical_name")?.as_str()?;
              let candidate = name.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase();
              (candidate == normalized).then_some((id, payload))
          }).take(2).collect::<Vec<_>>();

        if candidates.is_empty() {
            return Ok(json!({"entity_id":Value::Null,"canonical_name":requested,"definition":Value::Null,"version":Value::Null,"status":"not_found","components":[],"provenance":[],"non_equivalences":[],"bounded":true,"truncated":false}));
        }
        if candidates.len() > 1 {
            return Ok(json!({"entity_id":Value::Null,"canonical_name":requested,"definition":Value::Null,"version":Value::Null,"status":"ambiguous","components":[],"provenance":[],"non_equivalences":[],"bounded":true,"truncated":false}));
        }

        let (entity_id, payload) = &candidates[0];
        let mut relation_stmt = db.prepare(&format!(
            "select relation_type,source_id,target_id,payload_json from {} where source_id=?1 or target_id=?1 order by relation_id limit 256",
            self.relation_table
        )).map_err(self.db_error("concept_resolve_relations_prepare_failed"))?;
        let relation_rows = relation_stmt.query_map([entity_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?, row.get::<_, String>(3)?))
        }).map_err(self.db_error("concept_resolve_relations_failed"))?
          .collect::<Result<Vec<_>, _>>()
          .map_err(self.db_error("concept_resolve_relation_row_failed"))?;

        let entity_pk = self.table(&self.entity_table).primary_key.clone();
        let mut components = Vec::new();
        let mut provenance = Vec::new();
        let mut truncated = relation_rows.len() == 256;
        for (relation, source_id, target_id, relation_payload) in relation_rows {
            let other_id = if source_id == *entity_id { target_id } else { source_id };
            let other: Option<(String, String)> = db.query_row(
                &format!("select kind,payload_json from {} where {}=?1", self.entity_table, entity_pk),
                [&other_id], |row| Ok((row.get(0)?, row.get(1)?))
            ).optional().map_err(self.db_error("concept_resolve_related_entity_failed"))?;
            let Some((kind, raw)) = other else { continue };
            let related = serde_json::from_str::<Value>(&raw).unwrap_or(Value::Null);
            let relation_meta = serde_json::from_str::<Value>(&relation_payload).unwrap_or(Value::Null);
            let is_provenance = kind == "source" || relation == "derived_from" || relation == "promotes_to_evidence";
            if is_provenance {
                if provenance.len() >= PROVENANCE_LIMIT { truncated = true; continue; }
                provenance.push(json!({"relation":relation,"entity_id":other_id,"title":related.get("title").cloned().unwrap_or(Value::Null),"locator":related.get("locator").cloned().unwrap_or(Value::Null)}));
            } else {
                if components.len() >= COMPONENT_LIMIT { truncated = true; continue; }
                components.push(json!({"relation":relation,"multiplicity":relation_meta.get("multiplicity").cloned().unwrap_or(Value::Null),"entity_id":other_id,"title":related.get("title").cloned().unwrap_or(Value::Null)}));
            }
        }
        let mut response = json!({
            "entity_id":entity_id,
            "canonical_name":payload.get("canonical_name").cloned().unwrap_or(Value::Null),
            "definition":payload.get("definition").cloned().unwrap_or(Value::Null),
            "version":payload.get("version").cloned().unwrap_or(Value::Null),
            "status":"resolved",
            "components":components,
            "provenance":provenance,
            "non_equivalences":payload.get("non_equivalences").and_then(Value::as_array).map(|items| items.iter().take(16).cloned().collect::<Vec<_>>()).unwrap_or_default(),
            "bounded":true,
            "truncated":truncated
        });
        if serde_json::to_vec(&response).map(|bytes| bytes.len()).unwrap_or(OUTPUT_LIMIT + 1) > OUTPUT_LIMIT {
            response["components"] = json!([]);
            response["provenance"] = json!([]);
            response["non_equivalences"] = json!([]);
            response["truncated"] = json!(true);
        }
        Ok(response)
    }

    fn neighborhood(&self, root: &Path, args: &Map<String, Value>) -> Result<Value, Value> {
        self.with_stable_projection(root, || self.neighborhood_locked(root, args))
    }

    fn neighborhood_locked(&self, root: &Path, args: &Map<String, Value>) -> Result<Value, Value> {
        let id = self.required(args, "entity_id")?;
        let limit = args
            .get("limit")
            .and_then(Value::as_u64)
            .unwrap_or(self.domain.caps.neighborhood_limit.default)
            .min(self.domain.caps.neighborhood_limit.max);
        let db = Connection::open(self.projection_path(root))
            .map_err(self.db_error("projection_open_failed"))?;
        let entity_pk = self.table(&self.entity_table).primary_key.clone();
        let entity: Option<String> = db
            .query_row(
                &format!(
                    "select payload_json from {} where {}=?1",
                    self.entity_table, entity_pk
                ),
                [&id],
                |r| r.get(0),
            )
            .optional()
            .map_err(self.db_error("projection_entity_read_failed"))?;
        let entity = entity.ok_or_else(|| {
            self.error(
                "entity_not_found",
                "entity not found",
                json!({"entity_id":id}),
            )
        })?;
        let mut stmt = db.prepare(&format!("select relation_id,relation_type,source_id,target_id,payload_json from {} where source_id=?1 or target_id=?1 order by relation_id limit ?2", self.relation_table)).map_err(self.db_error("projection_relation_prepare_failed"))?;
        let relation_fields = &self.domain.query.neighborhood_relation_fields;
        let rows = stmt
            .query_map(params![id, limit], |r| {
                let payload =
                    serde_json::from_str::<Value>(&r.get::<_, String>(4)?).unwrap_or(Value::Null);
                let mut row_values = Map::new();
                row_values.insert("relation_id".into(), json!(r.get::<_, String>(0)?));
                row_values.insert("relation_type".into(), json!(r.get::<_, String>(1)?));
                row_values.insert("source_id".into(), json!(r.get::<_, String>(2)?));
                row_values.insert("target_id".into(), json!(r.get::<_, String>(3)?));
                Ok(Self::project_row(&row_values, &payload, relation_fields))
            })
            .map_err(self.db_error("projection_relation_query_failed"))?;
        let relations = rows
            .collect::<Result<Vec<_>, _>>()
            .map_err(self.db_error("projection_relation_row_failed"))?;
        let match_clause = self
            .domain
            .query
            .neighborhood_record_match_fields
            .iter()
            .map(|field| format!("json_extract(payload_json,'$.{field}')=?1"))
            .collect::<Vec<_>>()
            .join(" or ");
        let record_sql = format!("select record_id,record_kind,payload_json,event_id from {} where {} order by record_id limit ?2", self.records_table, match_clause);
        let record_fields = &self.domain.query.neighborhood_record_fields;
        let mut record_stmt = db
            .prepare(&record_sql)
            .map_err(self.db_error("projection_neighborhood_record_prepare_failed"))?;
        let records = record_stmt
            .query_map(params![id, limit], |r| {
                let payload =
                    serde_json::from_str::<Value>(&r.get::<_, String>(2)?).unwrap_or(Value::Null);
                let mut row_values = Map::new();
                row_values.insert("record_id".into(), json!(r.get::<_, String>(0)?));
                row_values.insert("record_kind".into(), json!(r.get::<_, String>(1)?));
                row_values.insert("event_id".into(), json!(r.get::<_, String>(3)?));
                Ok(Self::project_row(&row_values, &payload, record_fields))
            })
            .map_err(self.db_error("projection_neighborhood_record_query_failed"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(self.db_error("projection_neighborhood_record_row_failed"))?;
        Ok(
            json!({"schema":self.schema_id("neighborhood.v1"),"status":"ok","entity":serde_json::from_str::<Value>(&entity).unwrap_or(Value::Null),"relations":relations,"records":records,"limit":limit,"bounded":true}),
        )
    }

    fn export(&self, root: &Path, args: &Map<String, Value>) -> Result<Value, Value> {
        self.with_stable_projection(root, || self.export_locked(root, args))
    }

    fn export_locked(&self, root: &Path, args: &Map<String, Value>) -> Result<Value, Value> {
        let feature = &self.domain.features.export;
        let caps = &self.domain.caps.export;
        let format = args
            .get("format")
            .and_then(Value::as_str)
            .unwrap_or(&feature.default_format);
        let entities = self.query_locked(
            root,
            &Map::from_iter([("limit".into(), json!(caps.entities))]),
        )?["items"]
            .clone();
        let db = Connection::open(self.projection_path(root))
            .map_err(self.db_error("projection_open_failed"))?;
        let mut stmt = db
            .prepare(&format!(
                "select payload_json from {} order by relation_id limit {}",
                self.relation_table, caps.relations
            ))
            .map_err(self.db_error("projection_export_prepare_failed"))?;
        let relations = stmt
            .query_map([], |r| {
                Ok(serde_json::from_str::<Value>(&r.get::<_, String>(0)?).unwrap_or(Value::Null))
            })
            .map_err(self.db_error("projection_export_failed"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(self.db_error("projection_export_row_failed"))?;
        let mut record_stmt = db
            .prepare(&format!(
                "select payload_json from {} order by record_id limit {}",
                self.records_table, caps.records
            ))
            .map_err(self.db_error("projection_export_record_prepare_failed"))?;
        let records = record_stmt
            .query_map([], |r| {
                Ok(serde_json::from_str::<Value>(&r.get::<_, String>(0)?).unwrap_or(Value::Null))
            })
            .map_err(self.db_error("projection_export_record_failed"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(self.db_error("projection_export_record_row_failed"))?;
        let context = if format == "jsonld" {
            json!(feature.jsonld_context)
        } else {
            Value::Null
        };
        Ok(
            json!({"schema":feature.response_schema_id,"format":format,"ledger_head":self.ledger_head(root)?,"@context":context,"entities":entities,"relations":relations,"records":records,"bounded":true}),
        )
    }

    fn rebuild_projection(&self, root: &Path) -> Result<(), Value> {
        self.prepare(root)?;
        self.with_authority_lock(root, "projection", || self.rebuild_projection_locked(root))
    }

    fn with_stable_projection<T>(
        &self,
        root: &Path,
        action: impl FnOnce() -> Result<T, Value>,
    ) -> Result<T, Value> {
        self.prepare(root)?;
        // Ledger first, projection second: proposal admission already holds
        // the ledger lock while refreshing the projection, so every stable
        // read uses the same lock order and cannot observe a moving head.
        self.with_authority_lock(root, "ledger", || {
            self.with_authority_lock(root, "projection", || {
                self.rebuild_projection_locked(root)?;
                action()
            })
        })
    }

}
