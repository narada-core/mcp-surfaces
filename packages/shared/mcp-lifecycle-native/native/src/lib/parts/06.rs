impl LifecycleServer {
    fn call_task_tool(&mut self, name: &str, args: Value) -> Result<Value, String> {
        match name {
            "task_lifecycle_list" => self.task_list(args),
            "task_lifecycle_show" => self.task_show(args),
            "task_lifecycle_create" => self.task_create(args),
            "task_lifecycle_claim" => self.task_claim(args),
            "task_lifecycle_continue" => self.task_claim(args),
            "task_lifecycle_unclaim" => self.task_unclaim(args),
            "task_lifecycle_prove_criteria" => self.task_prove_criteria(args),
            "task_lifecycle_admit_evidence" => self.task_admit_evidence(args),
            "task_lifecycle_finish" => self.task_finish(args),
            "task_lifecycle_submit_work" => self.task_submit_work(args),
            "task_lifecycle_close"
            | "task_lifecycle_closeout"
            | "task_lifecycle_disposition_closeout" => self.task_closeout(args),
            "task_lifecycle_defer" => self.task_transition(args, "deferred"),
            "task_lifecycle_un_defer" => self.task_transition(args, "opened"),
            "task_lifecycle_reopen" => self.task_transition(args, "opened"),
            "task_lifecycle_roster" => self.roster_list(),
            "task_lifecycle_roster_admit" => self.roster_admit(args),
            "task_lifecycle_next" => self.task_next(args),
            "task_lifecycle_workboard_snapshot" => self.task_workboard(args),
            "task_lifecycle_evidence_preflight" => self.task_evidence_preflight(args),
            "task_lifecycle_self_certification_preflight" => {
                self.task_self_certification_preflight(args)
            }
            "task_lifecycle_guidance" => Ok(guidance_payload(&self.options.site_root, args)),
            "task_lifecycle_payload_schema" => payload_schema_payload(args),
            "mcp_payload_create" => self.payload_create(args),
            "mcp_payload_show" | "mcp_payload_validate" => self.payload_read(name, args),
            "mcp_payload_derive" => self.payload_derive(args),
            "mcp_output_show" => self.output_show(args),
            "task_lifecycle_chapter_show" => self.task_chapter_show(args),
            "task_lifecycle_chapter_add_task" => self.task_chapter_add(args),
            "task_lifecycle_tags_update" => self.task_tags_update(args),
            "task_lifecycle_report_blocked" => self.task_report_blocked(args),
            "task_lifecycle_submit_report" => self.task_finish(args),
            "task_lifecycle_review" => self.task_review(args),
            "task_lifecycle_evidence_supersede" => self.task_evidence_supersede(args),
            "task_lifecycle_compatibility_reconcile" => self.task_compatibility_reconcile(args),
            "task_lifecycle_set_routing" => self.task_set_routing(args),
            "task_lifecycle_dependency_declare" => self.task_dependency_declare(args),
            "task_lifecycle_dependency_dispose" | "task_lifecycle_dependency_disposition_record" => {
                self.task_dependency_disposition(args)
            }
            "task_lifecycle_search" => self.task_search(args),
            "task_lifecycle_related" => self.task_related(args),
            "task_lifecycle_inspect" => self.task_show(args),
            "task_lifecycle_inspect_range" => self.task_inspect_range(args),
            "task_lifecycle_audit" => self.task_audit(args),
            "task_lifecycle_obligations" => self.task_obligations(args),
            "task_lifecycle_recurring_create" => self.task_recurring_create(args),
            "task_lifecycle_recurring_run_due" => self.task_recurring_run_due(args),
            "task_lifecycle_recurring_suspend" => {
                self.task_recurring_update_status(args, "suspended")
            }
            "task_lifecycle_recurring_retire" => self.task_recurring_update_status(args, "retired"),
            "task_lifecycle_recurring_trigger" => self.task_recurring_trigger(args),
            "task_lifecycle_recurring_list"
            | "task_lifecycle_recurring_show"
            | "task_lifecycle_recurring_runs" => self.task_recurring_read(name, args),
            "task_lifecycle_executability_request" => self.task_executability_request(args),
            "task_lifecycle_executability_status" => self.task_executability_status(args),
            "task_lifecycle_executability_requests_next" => {
                self.task_executability_requests_next(args)
            }
            "task_lifecycle_executability_complete" => self.task_executability_complete(args),
            "task_lifecycle_executability_override" => self.task_executability_override(args),
            "task_lifecycle_executability_dispatch_check" => {
                self.task_executability_dispatch_check(args)
            }
            "task_lifecycle_test_mcp_tool" => self.task_test_mcp_tool(args),
            "task_lifecycle_run_tests" => self.task_run_tests(args),
            "task_lifecycle_diagnose_task_ref" => self.task_diagnose_ref(args),
            "task_lifecycle_record_observation" | "task_lifecycle_submit_observation" => {
                self.task_record_observation(args)
            }
            "task_lifecycle_bridge_poll" => self.task_bridge_poll(args),
            "task_lifecycle_inbox_target" => self.task_inbox_target(args),
            _ => Err(format!("task_mcp_refused: {name}")),
        }
    }

    fn call_work_tool(&mut self, name: &str, args: Value) -> Result<Value, String> {
        match name {
            "work_lifecycle_doctor" => self.doctor(&args),
            "ticket_list" => native_work_ticket_list(self, args),
            "ticket_show" => native_work_ticket_show(self, args),
            "ticket_sources_list" => native_work_ticket_sources(self, args),
            "ticket_admit_source" => native_work_admit_source_tx(self, args),
            "ticket_processing_context_load" => native_work_processing_context_tx(self, args),
            "ticket_admit_proposal" => native_work_admit_proposal_tx(self, args),
            "ticket_draft_receipt_record" => native_work_record_draft_receipt_tx(self, args),
            "ticket_draft_disposition_reconcile" => native_work_reconcile_draft_tx(self, args),
            "work_outbox_list" => native_work_outbox_list(self, args),
            "work_outbox_consumer_register" => native_work_outbox_register_tx(self, args),
            "work_outbox_ack" => native_work_outbox_ack_tx(self, args),
            "work_outbox_compact" => native_work_outbox_compact_tx(self, args),
            "work_lifecycle_storage_inspect" => native_work_storage_inspect(self),
            _ => Err(format!("unknown_tool:{name}")),
        }
    }

    fn task_diagnose_ref(&self, args: Value) -> Result<Value, String> {
        let connection = self.connection()?;
        let row = if let Some(number) = args.get("task_number").and_then(Value::as_i64) {
            connection.query_row(
                "select task_id, task_number, status, revision, updated_at from task_lifecycle where task_number=?1",
                params![number],
                |r| Ok(json!({"task_id":r.get::<_,String>(0)?,"task_number":r.get::<_,i64>(1)?,"status":r.get::<_,String>(2)?,"revision":r.get::<_,Option<i64>>(3)?,"updated_at":r.get::<_,String>(4)?})),
            ).optional().map_err(db_error)?
        } else if let Some(task_id) = string_arg(&args, "task_id") {
            connection.query_row(
                "select task_id, task_number, status, revision, updated_at from task_lifecycle where task_id=?1",
                params![task_id],
                |r| Ok(json!({"task_id":r.get::<_,String>(0)?,"task_number":r.get::<_,i64>(1)?,"status":r.get::<_,String>(2)?,"revision":r.get::<_,Option<i64>>(3)?,"updated_at":r.get::<_,String>(4)?})),
            ).optional().map_err(db_error)?
        } else {
            return Ok(
                json!({"schema":"narada.task.reference_diagnosis.v1","status":"identity_required"}),
            );
        };
        Ok(json!({
            "schema":"narada.task.reference_diagnosis.v1",
            "status": if row.is_some() {"resolved"} else {"not_found"},
            "input":{"task_id":args.get("task_id"),"task_number":args.get("task_number")},
            "task":row,
            "number_authority":"task_lifecycle"
        }))
    }

    fn task_search(&self, args: Value) -> Result<Value, String> {
        let connection = self.connection()?;
        let query = required_string(&args, "query")?;
        let limit = args
            .get("limit")
            .and_then(Value::as_i64)
            .unwrap_or(20)
            .clamp(1, 100);
        let pattern = format!("%{}%", query.to_lowercase());
        let mut stmt = connection.prepare(
            "select l.task_id,l.task_number,l.status,l.updated_at,s.title,s.goal_markdown,s.required_work_markdown,s.tags_json
               from task_lifecycle l left join task_specs s on s.task_id=l.task_id
              where lower(coalesce(s.title,'')) like ?1
                 or lower(coalesce(s.goal_markdown,'')) like ?1
                 or lower(coalesce(s.required_work_markdown,'')) like ?1
              order by l.task_number desc limit ?2"
        ).map_err(db_error)?;
        let rows = stmt.query_map(params![pattern, limit], |r| {
            let tags = r.get::<_,Option<String>>(7)?.and_then(|v| serde_json::from_str(&v).ok()).unwrap_or_else(||json!([]));
            Ok(json!({"task_id":r.get::<_,String>(0)?,"task_number":r.get::<_,i64>(1)?,"status":r.get::<_,String>(2)?,"updated_at":r.get::<_,String>(3)?,"title":r.get::<_,Option<String>>(4)?,"tags":tags}))
        }).map_err(db_error)?.collect::<Result<Vec<_>,_>>().map_err(db_error)?;
        Ok(
            json!({"schema":"narada.task.search.v1","status":"ok","query":query,"count":rows.len(),"tasks":rows}),
        )
    }

    fn task_related(&self, args: Value) -> Result<Value, String> {
        let number = required_i64(&args, "task_number")?;
        let limit = args
            .get("limit")
            .and_then(Value::as_i64)
            .unwrap_or(20)
            .clamp(1, 100);
        let connection = self.connection()?;
        let source_tags: Value = connection
            .query_row(
                "select coalesce(tags_json,'[]') from task_specs where task_number=?1",
                params![number],
                |r| r.get::<_, String>(0),
            )
            .optional()
            .map_err(db_error)?
            .and_then(|v| serde_json::from_str(&v).ok())
            .unwrap_or_else(|| json!([]));
        let mut rows = Vec::new();
        let mut stmt = connection.prepare("select l.task_number,l.task_id,l.status,s.title,s.tags_json from task_lifecycle l left join task_specs s on s.task_id=l.task_id where l.task_number<>?1 order by l.task_number desc limit ?2").map_err(db_error)?;
        for row in stmt.query_map(params![number, limit], |r| {
            let tags: Value = r.get::<_,Option<String>>(4)?.and_then(|v| serde_json::from_str(&v).ok()).unwrap_or_else(||json!([]));
            Ok(json!({"task_number":r.get::<_,i64>(0)?,"task_id":r.get::<_,String>(1)?,"status":r.get::<_,String>(2)?,"title":r.get::<_,Option<String>>(3)?,"tags":tags}))
        }).map_err(db_error)? {
            let item = row.map_err(db_error)?;
            let related = match (source_tags.as_array(), item.get("tags").and_then(Value::as_array)) {
                (Some(a),Some(b)) => a.iter().any(|x| b.contains(x)),
                _ => false,
            };
            if related { rows.push(item); }
        }
        Ok(
            json!({"schema":"narada.task.related.v1","status":"ok","task_number":number,"count":rows.len(),"related":rows}),
        )
    }

    fn task_audit(&self, args: Value) -> Result<Value, String> {
        let limit = args
            .get("limit")
            .and_then(Value::as_i64)
            .unwrap_or(100)
            .clamp(1, 200);
        let events = self.query_objects(
            "select * from task_lifecycle_events order by created_at desc limit ?1",
            params![limit],
        )?;
        let reports = self.query_objects(
            "select * from task_reports order by submitted_at desc limit ?1",
            params![limit],
        )?;
        Ok(
            json!({"schema":"narada.task.lifecycle.audit.v1","status":"ok","since":args.get("since"),"until":args.get("until"),"events":events,"reports":reports,"count":events.len()+reports.len()}),
        )
    }

    fn task_obligations(&self, args: Value) -> Result<Value, String> {
        let agent = required_string(&args, "agent_id")?;
        let limit = args
            .get("limit")
            .and_then(Value::as_i64)
            .unwrap_or(50)
            .clamp(1, 200);
        let rows = self.query_objects(
            "select * from directed_obligations where (target_agent_id=?1 or target_agent_id is null) and (?2 is null or status=?2) order by created_at desc limit ?3",
            params![agent, args.get("status").and_then(Value::as_str), limit],
        )?;
        Ok(
            json!({"schema":"narada.task.obligations.v1","status":"ok","agent_id":agent,"count":rows.len(),"obligations":rows}),
        )
    }

    fn task_next(&self, args: Value) -> Result<Value, String> {
        let agent = required_string(&args, "agent_id")?;
        let mut listed =
            self.task_list(json!({"limit":args.get("limit").cloned().unwrap_or(json!(20))}))?;
        let recommended = listed
            .get_mut("tasks")
            .and_then(Value::as_array_mut)
            .and_then(|tasks| {
                tasks.iter().find(|t| {
                    matches!(
                        t.get("status").and_then(Value::as_str),
                        Some("opened" | "deferred" | "needs_continuation")
                    )
                })
            })
            .cloned();
        Ok(
            json!({"schema":"narada.task.next.v1","status":"ok","agent_id":agent,"recommended_task":recommended,"next_action":recommended.as_ref().map(|_|"claim").unwrap_or("none"),"workboard":listed}),
        )
    }

    fn task_workboard(&self, args: Value) -> Result<Value, String> {
        let agent = required_string(&args, "agent_id")?;
        let tasks =
            self.task_list(json!({"limit":args.get("limit").cloned().unwrap_or(json!(50))}))?;
        Ok(
            json!({"schema":"narada.task.workboard.v1","status":"ok","agent_id":agent,"snapshot":tasks,"generated_at":now(),"state_freshness":{"status":"fresh","last_workboard_check_at":args.get("last_workboard_check_at")}}),
        )
    }

}
