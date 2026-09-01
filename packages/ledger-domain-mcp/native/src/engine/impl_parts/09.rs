impl Engine {
    fn issue_tree_frontier_nodes(
        &self,
        root: &Path,
        tree_id: &str,
    ) -> Result<(Vec<Value>, Option<Value>), Value> {
        self.rebuild_projection(root)?;
        let db = Connection::open(self.projection_path(root))
            .map_err(self.db_error("projection_open_failed"))?;
        let mut superseded = HashSet::new();
        let mut relation_statement = db
            .prepare(&format!(
                "select target_id from {} where relation_type='supersedes'",
                self.relation_table
            ))
            .map_err(self.db_error("issue_tree_frontier_prepare_failed"))?;
        for id in relation_statement
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(self.db_error("issue_tree_frontier_failed"))?
        {
            superseded.insert(id.map_err(self.db_error("issue_tree_frontier_row_failed"))?);
        }
        let mut statement = db
            .prepare(&format!(
                "select entity_id,payload_json,event_id from {} where kind='research_issue'",
                self.entity_table
            ))
            .map_err(self.db_error("issue_tree_frontier_prepare_failed"))?;
        let mut nodes = statement.query_map([], |row| {
            let payload = serde_json::from_str::<Value>(&row.get::<_, String>(1)?).unwrap_or(Value::Null);
            Ok(json!({"node_id":row.get::<_,String>(0)?,"payload":payload,"event_id":row.get::<_,String>(2)?}))
        }).map_err(self.db_error("issue_tree_frontier_failed"))?
          .collect::<Result<Vec<_>, _>>().map_err(self.db_error("issue_tree_frontier_row_failed"))?;
        nodes.retain(|node| {
            node["payload"]["tree_id"].as_str() == Some(tree_id)
                && node["payload"]["state"].as_str() != Some("disposed")
                && !superseded.contains(node["node_id"].as_str().unwrap_or_default())
        });
        let clip = |value: &str, maximum: usize| {
            let clipped = value.chars().count() > maximum;
            let text = if clipped {
                format!("{}…", value.chars().take(maximum).collect::<String>())
            } else {
                value.to_owned()
            };
            (text, clipped)
        };
        for node in &mut nodes {
            let payload = &node["payload"];
            let (title, title_clipped) = clip(payload["title"].as_str().unwrap_or_default(), 300);
            let (rationale_excerpt, rationale_clipped) =
                clip(payload["rationale"].as_str().unwrap_or_default(), 500);
            let score = payload["score"].as_f64().unwrap_or(0.0);
            let stored_state = payload["state"].as_str().unwrap_or("open");
            let state = if stored_state == "active" {
                "open"
            } else {
                stored_state
            };
            *node = json!({
                "node_id":node["node_id"],
                "version":payload["version"],
                "title":title,
                "title_clipped":title_clipped,
                "state":state,
                "score":score,
                "display_score_out_of_10":(score * 100.0).round() / 10.0,
                "disposition":payload["disposition"],
                "rationale_excerpt":rationale_excerpt,
                "rationale_clipped":rationale_clipped,
                "blocker_count":payload["blockers"].as_array().map(Vec::len).unwrap_or(0),
                "evidence_reference_count":payload["evidence_refs"].as_array().map(Vec::len).unwrap_or(0),
                "event_id":node["event_id"]
            });
        }
        nodes.sort_by(|left, right| {
            right["score"]
                .as_f64()
                .unwrap_or(0.0)
                .partial_cmp(&left["score"].as_f64().unwrap_or(0.0))
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| left["node_id"].as_str().cmp(&right["node_id"].as_str()))
        });
        let selected = nodes
            .iter()
            .filter(|node| node["state"].as_str() == Some("selected"))
            .cloned()
            .collect::<Vec<_>>();
        if selected.len() > 1 {
            return Err(self.error("issue_tree_selected_conflict", "more than one selected leaf exists", json!({"tree_id":tree_id,"selected_node_ids":selected.iter().map(|node|node["node_id"].clone()).collect::<Vec<_>>()})));
        }
        Ok((nodes, selected.into_iter().next()))
    }

    fn issue_tree_capture_path(&self, root: &Path, result_ref: &str) -> PathBuf {
        self.runtime(root)
            .join("issue-tree-captures")
            .join(format!("{}.json", safe_name(result_ref)))
    }

    fn issue_tree_frontier_page(
        &self,
        _root: &Path,
        capture: &Value,
        offset: usize,
        limit: usize,
    ) -> Result<Value, Value> {
        let items = capture["items"].as_array().ok_or_else(|| {
            self.error(
                "issue_tree_capture_invalid",
                "capture items are invalid",
                Value::Null,
            )
        })?;
        let page = items
            .iter()
            .skip(offset)
            .take(limit)
            .cloned()
            .collect::<Vec<_>>();
        let next_offset = offset + page.len();
        let has_more = next_offset < items.len();
        let result_ref = capture["result_ref"].as_str().unwrap_or_default();
        let continuation = has_more.then(|| json!({"tool":"epistemic_graph_issue_tree_frontier_read","arguments":{"result_ref":result_ref,"offset":next_offset,"limit":limit}}));
        Ok(json!({
            "schema":"narada.epistemic.issue_tree_frontier.v1",
            "status":"ok",
            "tree_id":capture["tree_id"],
            "ledger_head":capture["ledger_head"],
            "captured_at_event":capture["captured_at_event"],
            "frontier":{"items":page,"returned":page.len(),"complete":!has_more,"total":items.len(),"total_exact":true,"offset":offset},
            "selected":capture["selected"],
            "continuation":continuation,
            "result_ref":result_ref,
            "ordering":"score_desc_then_node_id",
            "selection":"non_disposed_and_not_superseded",
            "certifies_truth":false,
            "noncertification":"coordination state; not evidence"
        }))
    }

    fn issue_tree_frontier(&self, root: &Path, args: &Map<String, Value>) -> Result<Value, Value> {
        let tree_id = args
            .get("tree_id")
            .and_then(Value::as_str)
            .filter(|v| !v.is_empty())
            .ok_or_else(|| self.error("issue_tree_invalid", "tree_id is required", Value::Null))?;
        let limit = args
            .get("limit")
            .and_then(Value::as_u64)
            .unwrap_or(20)
            .clamp(1, 100) as usize;
        let (nodes, selected) = self.issue_tree_frontier_nodes(root, tree_id)?;
        let ledger_head = self.ledger_head(root)?;
        let capture_seed =
            serde_json::to_vec(&json!({"tree_id":tree_id,"ledger_head":ledger_head,"items":nodes}))
                .unwrap_or_default();
        let result_ref = format!("issue-tree-frontier:{}", &sha256(&capture_seed)[..24]);
        let capture = json!({
            "schema":"narada.epistemic.issue_tree_frontier_capture.v1",
            "result_ref":result_ref,
            "tree_id":tree_id,
            "ledger_head":ledger_head,
            "captured_at_event":self.ledger_files(root)?.last().and_then(|path| self.read_json(path).ok()).and_then(|event| event.get("event_id").cloned()),
            "selected":selected,
            "items":nodes
        });
        let path = self.issue_tree_capture_path(root, &result_ref);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(self.io_error("issue_tree_capture_create_failed"))?;
        }
        fs::write(
            &path,
            serde_json::to_vec_pretty(&capture).unwrap_or_default(),
        )
        .map_err(self.io_error("issue_tree_capture_write_failed"))?;
        self.issue_tree_frontier_page(root, &capture, 0, limit)
    }

    fn issue_tree_frontier_read(
        &self,
        root: &Path,
        args: &Map<String, Value>,
    ) -> Result<Value, Value> {
        let result_ref = args
            .get("result_ref")
            .and_then(Value::as_str)
            .filter(|v| v.starts_with("issue-tree-frontier:"))
            .ok_or_else(|| {
                self.error(
                    "issue_tree_result_ref_invalid",
                    "a valid issue-tree frontier result_ref is required",
                    Value::Null,
                )
            })?;
        let capture = self
            .read_json(&self.issue_tree_capture_path(root, result_ref))
            .map_err(|_| {
                self.error(
                    "issue_tree_result_expired",
                    "the captured frontier is unavailable",
                    json!({"result_ref":result_ref,"retry_safe":true}),
                )
            })?;
        let offset = args.get("offset").and_then(Value::as_u64).unwrap_or(0) as usize;
        let limit = args
            .get("limit")
            .and_then(Value::as_u64)
            .unwrap_or(20)
            .clamp(1, 100) as usize;
        self.issue_tree_frontier_page(root, &capture, offset, limit)
    }

}
