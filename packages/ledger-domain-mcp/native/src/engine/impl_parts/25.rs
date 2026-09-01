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
