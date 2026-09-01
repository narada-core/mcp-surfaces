impl LifecycleServer {
    fn task_report_blocked(&mut self, args: Value) -> Result<Value, String> {
        let number = required_i64(&args, "task_number")?;
        let agent = required_string(&args, "agent_id")?;
        let reason = required_string(&args, "reason")?;
        let connection = self.connection_mut()?;
        let task_id: String = connection
            .query_row(
                "select task_id from task_lifecycle where task_number=?1",
                params![number],
                |r| r.get(0),
            )
            .optional()
            .map_err(db_error)?
            .ok_or_else(|| format!("task_not_found: {number}"))?;
        let report_id = format!("report-{}", Uuid::new_v4());
        connection.execute("insert into task_reports(report_id,task_id,agent_id,agent_identity_ref_json,summary,changed_files_json,verification_json,submitted_at) values(?1,?2,?3,null,?4,'[]','{}',?5)", params![report_id, task_id, agent, reason, now()]).map_err(db_error)?;
        if args.get("defer").and_then(Value::as_bool) != Some(false) {
            connection
                .execute(
                    "update task_lifecycle set status='deferred',updated_at=?1 where task_id=?2",
                    params![now(), task_id],
                )
                .map_err(db_error)?;
        }
        Ok(
            json!({"status":"blocked","task_number":number,"report_id":report_id,"deferred":args.get("defer").and_then(Value::as_bool)!=Some(false)}),
        )
    }

    fn task_set_routing(&mut self, args: Value) -> Result<Value, String> {
        let number = required_i64(&args, "task_number")?;
        let actor = string_arg(&args, "actor_agent_id")
            .or_else(|| string_arg(&args, "agent_id"))
            .unwrap_or_else(|| "native".to_string());
        let preferred_role = string_arg(&args, "preferred_role");
        let target_role = string_arg(&args, "target_role");
        let preferred_agent_id = string_arg(&args, "preferred_agent_id");
        let reason = string_arg(&args, "reason").unwrap_or_else(|| "routing_updated".to_string());
        let timestamp = now();
        let connection = self.connection_mut()?;
        let task_id: String = connection
            .query_row(
                "select task_id from task_lifecycle where task_number=?1",
                params![number],
                |r| r.get(0),
            )
            .optional()
            .map_err(db_error)?
            .ok_or_else(|| format!("task_not_found: {number}"))?;
        let previous = connection
            .query_row(
                "select preferred_role,target_role,preferred_agent_id,updated_at from narada_andrey_task_role_preferences where task_id=?1",
                params![&task_id],
                |r| Ok(json!({"preferred_role":r.get::<_,Option<String>>(0)?,"target_role":r.get::<_,Option<String>>(1)?,"preferred_agent_id":r.get::<_,Option<String>>(2)?,"updated_at":r.get::<_,String>(3)?})),
            )
            .optional()
            .map_err(db_error)?
            .unwrap_or_else(|| json!({}));
        let routing = json!({"preferred_role":preferred_role,"target_role":target_role,"preferred_agent_id":preferred_agent_id,"updated_at":timestamp});
        let actor_role: Option<String> = connection
            .query_row("select role from agent_roster where agent_id=?1", params![&actor], |r| r.get(0))
            .optional()
            .map_err(db_error)?;
        connection.execute("insert into narada_andrey_task_role_preferences(task_id,preferred_role,target_role,preferred_agent_id,updated_at) values(?1,?2,?3,?4,?5) on conflict(task_id) do update set preferred_role=excluded.preferred_role,target_role=excluded.target_role,preferred_agent_id=excluded.preferred_agent_id,updated_at=excluded.updated_at", params![&task_id, preferred_role, target_role, preferred_agent_id, &timestamp]).map_err(db_error)?;
        connection.execute("insert into task_routing_events(event_id,task_id,task_number,actor_agent_id,actor_role,reason,changed_fields_json,previous_routing_json,new_routing_json,created_at) values(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)", params![format!("routing-event-{}",Uuid::new_v4()),&task_id,number,&actor,actor_role,&reason,args.get("changed_fields").cloned().unwrap_or_else(||json!(["preferred_role","target_role","preferred_agent_id"])).to_string(),previous.to_string(),routing.to_string(),&timestamp]).map_err(db_error)?;
        Ok(json!({"status":"updated","task_number":number,"actor_agent_id":actor,"routing":routing,"previous_routing":previous,"reason":reason}))
    }

    fn task_dependency_declare(&mut self, args: Value) -> Result<Value, String> {
        let parent_number = required_i64(&args, "parent_task_number")?;
        let required_number = required_i64(&args, "required_task_number")?;
        let agent = required_string(&args, "agent_id")?;
        let kind = required_string(&args, "kind")?;
        let connection = self.connection_mut()?;
        let parent: String = connection
            .query_row(
                "select task_id from task_lifecycle where task_number=?1",
                params![parent_number],
                |r| r.get(0),
            )
            .optional()
            .map_err(db_error)?
            .ok_or_else(|| format!("task_not_found: {parent_number}"))?;
        let required: String = connection
            .query_row(
                "select task_id from task_lifecycle where task_number=?1",
                params![required_number],
                |r| r.get(0),
            )
            .optional()
            .map_err(db_error)?
            .ok_or_else(|| format!("task_not_found: {required_number}"))?;
        let dependency_id = string_arg(&args, "dependency_id")
            .unwrap_or_else(|| format!("dependency-{}", Uuid::new_v4()));
        connection.execute("insert or ignore into task_dependencies(dependency_id,parent_task_id,required_task_id,kind,satisfying_outcomes_json,status,created_by,created_at) values(?1,?2,?3,?4,?5,'open',?6,?7)", params![dependency_id, parent, required, kind, args.get("satisfying_outcomes").cloned().unwrap_or_else(||json!([])).to_string(), agent, now()]).map_err(db_error)?;
        Ok(
            json!({"status":"created","dependency_id":dependency_id,"parent_task_number":parent_number,"required_task_number":required_number}),
        )
    }
    fn roster_list(&self) -> Result<Value, String> {
        let connection = self.connection()?;
        let mut stmt = connection
            .prepare("select * from agent_roster order by agent_id")
            .map_err(db_error)?;
        let rows = stmt
            .query_map([], |r| row_to_object(r))
            .map_err(db_error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(db_error)?;
        Ok(json!({"status":"ok","roster":rows}))
    }
    fn roster_admit(&mut self, args: Value) -> Result<Value, String> {
        let connection = self.connection_mut()?;
        let agent = required_string(&args, "agent_id")?;
        let role = string_arg(&args, "role").unwrap_or_else(|| "engineer".to_string());
        let capabilities = args.get("capabilities").cloned().unwrap_or_else(||json!([]));
        let requested_by = string_arg(&args, "requested_by").or_else(|| string_arg(&args, "actor_agent_id")).unwrap_or_else(|| "native".to_string());
        let authority_basis = args.get("authority_basis").cloned().unwrap_or_else(||json!({}));
        let reason = string_arg(&args, "reason").unwrap_or_else(|| "roster_admitted".to_string());
        let n = now();
        let tx = connection.transaction().map_err(db_error)?;
        tx.execute("insert into agent_roster(agent_id,role,capabilities_json,operator_identity,first_seen_at,last_active_at,status,task_number,last_done,updated_at) values(?1,?2,?3,null,?4,?4,'idle',null,null,?4) on conflict(agent_id) do update set role=excluded.role,capabilities_json=excluded.capabilities_json,last_active_at=excluded.last_active_at,updated_at=excluded.updated_at",params![&agent,&role,capabilities.to_string(),&n]).map_err(db_error)?;
        tx.execute("insert into agent_roster_events(event_id,event_type,agent_id,role,capabilities_json,operator_identity,requested_by,requested_at,authority_basis_json,admission_status,admitted_by,admitted_at,reason,payload_json,supersedes_event_id) values(?1,'admit',?2,?3,?4,null,?5,?6,?7,'admitted',?5,?6,?8,?9,null)",params![format!("roster-event-{}",Uuid::new_v4()),&agent,&role,capabilities.to_string(),&requested_by,&n,authority_basis.to_string(),&reason,args.to_string()]).map_err(db_error)?;
        tx.commit().map_err(db_error)?;
        Ok(json!({"status":"admitted","agent_id":agent,"role":role,"capabilities":capabilities,"requested_by":requested_by,"reason":reason}))
    }

    fn payload_derive(&mut self, args: Value) -> Result<Value, String> {
        let source_ref = required_string(&args, "source_ref")?;
        let (payload_id, source_revision) = parse_payload_reference(&source_ref)?;
        let source = self.payload_read("mcp_payload_show", json!({"ref": source_ref}))?;
        let mut payload = source
            .get("payload")
            .cloned()
            .ok_or("payload_ref_payload_must_be_object")?;
        let delete_paths_value = args.get("delete_paths");
        let has_overlay = args.get("overlay").is_some() || args.get("overlay_json").is_some();
        let has_delete_paths = delete_paths_value.is_some();
        if !has_overlay && !has_delete_paths {
            return Err("payload_derive_requires_overlay_or_delete_paths".to_string());
        }
        let overlay = if has_overlay {
            payload_object_from_args(&args, "overlay", "overlay_json")?
        } else {
            json!({})
        };
        merge_json_objects(&mut payload, &overlay)?;
        let mut delete_paths = Vec::new();
        if let Some(value) = delete_paths_value {
            let values = value
                .as_array()
                .ok_or("payload_derive_delete_paths_must_be_non_empty_string_array")?;
            if values.is_empty() {
                return Err(
                    "payload_derive_delete_paths_must_be_non_empty_string_array".to_string()
                );
            }
            for path in values {
                let path = path
                    .as_str()
                    .ok_or("payload_derive_delete_paths_must_be_non_empty_string_array")?;
                if delete_paths.iter().any(|existing| existing == path) {
                    return Err("payload_derive_delete_paths_must_be_unique".to_string());
                }
                delete_json_pointer(&mut payload, path)?;
                delete_paths.push(path.to_string());
            }
        }
        let revision = source_revision + 1;
        let reference = format!("mcp_payload:{payload_id}@v{revision}");
        let byte_size = payload_byte_size(&payload);
        let max_bytes = 256 * 1024usize;
        if byte_size > max_bytes {
            return Err(format!("payload_too_large: {byte_size} > {max_bytes}"));
        }
        let record = json!({
            "schema": "narada.mcp_payload.revision.v1",
            "ref": reference,
            "payload_id": payload_id,
            "revision": revision,
            "created_at": now(),
            "created_by": string_arg(&args, "created_by"),
            "source": {
                "kind": "derive",
                "source_ref": source_ref,
                "overlay_sha256": digest(&overlay),
                "delete_paths": delete_paths
            },
            "sha256": digest(&payload),
            "byte_size": byte_size,
            "max_bytes": max_bytes,
            "transient_not_authority": true,
            "immutable_revision": true,
            "payload": payload
        });
        let path = payload_revision_path(&self.options.site_root, &payload_id, revision);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| format!("payload_directory_create_failed:{e}"))?;
        }
        let serialized = format!("{}\n", payload_stable_json(&record));
        let status = match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(mut file) => {
                file.write_all(serialized.as_bytes())
                    .map_err(|e| format!("payload_write_failed:{e}"))?;
                "derived"
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                let existing: Value = serde_json::from_str(
                    &fs::read_to_string(&path)
                        .map_err(|e| format!("payload_revision_conflict:{e}"))?,
                )
                .map_err(|e| format!("payload_revision_conflict:{e}"))?;
                if existing.get("ref") == record.get("ref")
                    && existing.get("sha256") == record.get("sha256")
                    && existing.get("byte_size") == record.get("byte_size")
                {
                    "existing"
                } else {
                    return Err(format!("payload_revision_conflict: immutable revision already contains different content: {reference}"));
                }
            }
            Err(error) => return Err(format!("payload_write_failed:{error}")),
        };
        Ok(json!({
            "status": status,
            "ref": reference,
            "payload_id": payload_id,
            "revision": revision,
            "source_ref": source_ref,
            "byte_size": byte_size,
            "sha256": record.get("sha256").cloned().unwrap_or(Value::Null),
            "created_at": record.get("created_at").cloned().unwrap_or(Value::Null),
            "created_by": record.get("created_by").cloned().unwrap_or(Value::Null),
            "transient_not_authority": true,
            "immutable_revision": true
        }))
    }
}
