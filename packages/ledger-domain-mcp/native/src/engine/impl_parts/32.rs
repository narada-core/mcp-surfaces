#[derive(Clone)]
struct TeamWorkMember {
    identity: String,
    entity_id: String,
}

#[derive(Clone)]
struct TeamWorkAttribution {
    member: String,
    tree_id: String,
    leaf_id: Option<String>,
    basis: String,
    event_id: String,
    sequence: u64,
    occurred_at: Value,
}

impl Engine {
    fn team_work_config(&self) -> Result<&Map<String, Value>, Value> {
        self.domain
            .query
            .named_queries
            .get("epistemic:team-work-overview")
            .and_then(Value::as_object)
            .ok_or_else(|| self.error(
                "team_work_overview_unavailable",
                "the domain does not configure the team-work read model",
                Value::Null,
            ))
    }

    fn team_work_members(&self) -> Result<Vec<TeamWorkMember>, Value> {
        let configured = self.team_work_config()?
            .get("canonical_members")
            .and_then(Value::as_array)
            .ok_or_else(|| self.error(
                "team_work_overview_invalid",
                "canonical team members are not configured",
                Value::Null,
            ))?;
        let mut members = Vec::new();
        for value in configured {
            let identity = value.get("identity").and_then(Value::as_str).unwrap_or_default();
            let entity_id = value.get("entity_id").and_then(Value::as_str).unwrap_or_default();
            if identity.is_empty() || entity_id.is_empty() {
                return Err(self.error(
                    "team_work_overview_invalid",
                    "a configured team member lacks identity or entity_id",
                    Value::Null,
                ));
            }
            members.push(TeamWorkMember { identity: identity.to_string(), entity_id: entity_id.to_string() });
        }
        Ok(members)
    }

    fn parse_team_work_cursor(&self, cursor: Option<&str>, head: &str) -> Result<usize, Value> {
        let Some(cursor) = cursor else { return Ok(0); };
        let prefix = "team-work-overview:";
        let rest = cursor.strip_prefix(prefix).ok_or_else(|| self.error(
            "team_work_cursor_invalid",
            "continuation cursor is invalid",
            json!({"cursor":cursor}),
        ))?;
        let (cursor_head, offset) = rest.rsplit_once(':').ok_or_else(|| self.error(
            "team_work_cursor_invalid",
            "continuation cursor is invalid",
            json!({"cursor":cursor}),
        ))?;
        if cursor_head != head {
            return Err(self.error(
                "ledger_head_mismatch",
                "team-work continuation is bound to another ledger head",
                json!({"cursor_ledger_head":cursor_head,"actual_ledger_head":head,"retry_safe":true}),
            ));
        }
        offset.parse::<usize>().map_err(|_| self.error(
            "team_work_cursor_invalid",
            "continuation cursor offset is invalid",
            json!({"cursor":cursor}),
        ))
    }

    fn team_work_overview(&self, root: &Path, args: &Map<String, Value>) -> Result<Value, Value> {
        let config = self.team_work_config()?.clone();
        let members = self.team_work_members()?;
        let current_head = self.ledger_head(root)?.unwrap_or_else(|| "none".to_string());
        if let Some(expected) = args.get("expected_ledger_head").and_then(Value::as_str) {
            if expected != current_head {
                return Err(self.error(
                    "ledger_head_mismatch",
                    "team-work expected_ledger_head does not match the canonical ledger head",
                    json!({"expected_ledger_head":expected,"actual_ledger_head":current_head}),
                ));
            }
        }
        let offset = self.parse_team_work_cursor(args.get("cursor").and_then(Value::as_str), &current_head)?;
        let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(20).clamp(1, 100) as usize;
        let compact = args.get("compact").and_then(Value::as_bool).unwrap_or(true);
        let requested_members = args.get("member_ids").and_then(Value::as_array).cloned().unwrap_or_default();
        let requested_trees = args.get("tree_ids").and_then(Value::as_array).cloned().unwrap_or_default();
        let statuses = args.get("statuses").and_then(Value::as_array).cloned().unwrap_or_default();
        let status_filter = statuses.iter().filter_map(Value::as_str).collect::<HashSet<_>>();

        let mut member_aliases = BTreeMap::<String, String>::new();
        for member in &members {
            member_aliases.insert(member.identity.clone(), member.identity.clone());
            member_aliases.insert(member.entity_id.clone(), member.identity.clone());
        }
        let mut selected_members = HashSet::<String>::new();
        let mut unknown_member_ids = Vec::new();
        if requested_members.is_empty() {
            selected_members.extend(members.iter().map(|member| member.identity.clone()));
        } else {
            for value in &requested_members {
                let id = value.as_str().unwrap_or_default();
                if let Some(identity) = member_aliases.get(id) {
                    selected_members.insert(identity.clone());
                } else {
                    unknown_member_ids.push(id.to_string());
                }
            }
        }
        let tree_filter = requested_trees.iter().filter_map(Value::as_str).map(str::to_string).collect::<HashSet<_>>();

        let mut trees = BTreeMap::<String, Value>::new();
        let mut issues = BTreeMap::<String, Value>::new();
        let mut issue_events = BTreeMap::<String, Value>::new();
        let mut relations = BTreeMap::<String, Value>::new();
        let mut attributions = BTreeMap::<String, TeamWorkAttribution>::new();
        let ledger_files = self.ledger_files(root)?;
        let mut ledger_sequence = 0_u64;
        for path in &ledger_files {
            let event = self.read_json(path)?;
            let sequence = event.get("sequence").and_then(Value::as_u64).unwrap_or(0);
            ledger_sequence = ledger_sequence.max(sequence);
            let event_id = event.get("event_id").and_then(Value::as_str).unwrap_or_default().to_string();
            let occurred_at = event.get("occurred_at").cloned().unwrap_or(Value::Null);
            let actor = event.get("actor").and_then(Value::as_str).and_then(|actor| member_aliases.get(actor)).cloned();
            for operation in event.get("operations").and_then(Value::as_array).into_iter().flatten() {
                match operation.get("op").and_then(Value::as_str) {
                    Some("entity.declare") => {
                        let entity_id = operation.get("entity_id").and_then(Value::as_str).unwrap_or_default();
                        match operation.get("kind").and_then(Value::as_str) {
                            Some("research_issue_tree") if !entity_id.is_empty() => {
                                trees.insert(entity_id.to_string(), operation.clone());
                            }
                            Some("research_issue") if !entity_id.is_empty() => {
                                issues.insert(entity_id.to_string(), operation.clone());
                                issue_events.insert(entity_id.to_string(), json!({"event_id":event_id,"sequence":sequence,"occurred_at":occurred_at}));
                                if let (Some(member), Some(tree_id)) = (actor.as_ref(), operation.get("tree_id").and_then(Value::as_str)) {
                                    let key = format!("{member}\u{0}{tree_id}");
                                    let candidate = TeamWorkAttribution {
                                        member: member.clone(), tree_id: tree_id.to_string(), leaf_id: Some(entity_id.to_string()),
                                        basis: "canonical_transition_actor".to_string(), event_id: event_id.clone(), sequence,
                                        occurred_at: occurred_at.clone(),
                                    };
                                    if attributions.get(&key).map(|current| current.sequence <= sequence).unwrap_or(true) {
                                        attributions.insert(key, candidate);
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                    Some("relation.declare") => {
                        let relation_type = operation.get("relation_type").and_then(Value::as_str).unwrap_or_default();
                        let source = operation.get("source_id").and_then(Value::as_str).unwrap_or_default();
                        let target = operation.get("target_id").and_then(Value::as_str).unwrap_or_default();
                        let relation_id = operation.get("relation_id").and_then(Value::as_str)
                            .map(str::to_string)
                            .unwrap_or_else(|| format!("{relation_type}\u{0}{source}\u{0}{target}"));
                        relations.insert(relation_id, json!({
                            "relation_type":relation_type,"source_id":source,"target_id":target,
                            "event_id":event_id,"sequence":sequence,"occurred_at":occurred_at
                        }));
                    }
                    _ => {}
                }
            }
        }
        let ending_head = self.ledger_head(root)?.unwrap_or_else(|| "none".to_string());
        if ending_head != current_head {
            return Err(self.error(
                "ledger_head_drift",
                "canonical ledger head changed while constructing the team-work page",
                json!({"starting_ledger_head":current_head,"ending_ledger_head":ending_head,"retry_safe":true}),
            ));
        }

        let attribution_relations = config.get("attribution_relations").and_then(Value::as_array)
            .into_iter().flatten().filter_map(Value::as_str).collect::<HashSet<_>>();
        let accepted_handoff_relations = config.get("accepted_handoff_relations").and_then(Value::as_array)
            .into_iter().flatten().filter_map(Value::as_str).collect::<HashSet<_>>();
        let pending_handoff_relations = config.get("pending_handoff_relations").and_then(Value::as_array)
            .into_iter().flatten().filter_map(Value::as_str).collect::<HashSet<_>>();
        for relation in relations.values() {
            let relation_type = relation.get("relation_type").and_then(Value::as_str).unwrap_or_default();
            if !attribution_relations.contains(relation_type) && !accepted_handoff_relations.contains(relation_type) { continue; }
            let source = relation.get("source_id").and_then(Value::as_str).unwrap_or_default();
            let target = relation.get("target_id").and_then(Value::as_str).unwrap_or_default();
            let (member, leaf_id) = if let Some(member) = member_aliases.get(target) {
                (member.clone(), source)
            } else if let Some(member) = member_aliases.get(source) {
                (member.clone(), target)
            } else { continue; };
            let Some(issue) = issues.get(leaf_id) else { continue; };
            let Some(tree_id) = issue.get("tree_id").and_then(Value::as_str) else { continue; };
            let sequence = relation.get("sequence").and_then(Value::as_u64).unwrap_or(0);
            let key = format!("{member}\u{0}{tree_id}");
            let candidate = TeamWorkAttribution {
                member, tree_id: tree_id.to_string(), leaf_id: Some(leaf_id.to_string()),
                basis: if accepted_handoff_relations.contains(relation_type) { "accepted_directed_handoff" } else { "explicit_assignment_or_claim" }.to_string(),
                event_id: relation.get("event_id").and_then(Value::as_str).unwrap_or_default().to_string(), sequence,
                occurred_at: relation.get("occurred_at").cloned().unwrap_or(Value::Null),
            };
            if attributions.get(&key).map(|current| current.sequence <= sequence).unwrap_or(true) {
                attributions.insert(key, candidate);
            }
        }

        let superseded = relations.values().filter(|relation| relation.get("relation_type").and_then(Value::as_str) == Some("supersedes"))
            .filter_map(|relation| relation.get("target_id").and_then(Value::as_str)).collect::<HashSet<_>>();
        let mut unresolved_by_tree = BTreeMap::<String, Vec<Value>>::new();
        for (issue_id, issue) in &issues {
            let Some(tree_id) = issue.get("tree_id").and_then(Value::as_str) else { continue; };
            let state = issue.get("state").and_then(Value::as_str).unwrap_or("open");
            if state != "disposed" && !superseded.contains(issue_id.as_str()) {
                unresolved_by_tree.entry(tree_id.to_string()).or_default().push(json!({
                    "node_id":issue_id,"title":issue.get("title"),"version":issue.get("version"),
                    "state":if state == "active" {"open"} else {state},"score":issue.get("score").cloned().unwrap_or(json!(0.0)),
                    "disposition":issue.get("disposition").cloned().unwrap_or(Value::Null)
                }));
            }
        }
        for nodes in unresolved_by_tree.values_mut() {
            nodes.sort_by(|left, right| {
                let left_selected = left.get("state").and_then(Value::as_str) == Some("selected");
                let right_selected = right.get("state").and_then(Value::as_str) == Some("selected");
                right_selected.cmp(&left_selected)
                    .then_with(|| right.get("score").and_then(Value::as_f64).unwrap_or(0.0).partial_cmp(&left.get("score").and_then(Value::as_f64).unwrap_or(0.0)).unwrap_or(std::cmp::Ordering::Equal))
                    .then_with(|| left.get("node_id").and_then(Value::as_str).cmp(&right.get("node_id").and_then(Value::as_str)))
            });
        }

        let freshness_gap = config.get("freshness_max_event_gap").and_then(Value::as_u64).unwrap_or(100);
        let mut rows = Vec::new();
        let mut attributed_trees = HashSet::<String>::new();
        for attribution in attributions.values() {
            if !selected_members.contains(&attribution.member) { continue; }
            if !tree_filter.is_empty() && !tree_filter.contains(&attribution.tree_id) { continue; }
            attributed_trees.insert(attribution.tree_id.clone());
            let unresolved = unresolved_by_tree.get(&attribution.tree_id).cloned().unwrap_or_default();
            let attributed_leaf = attribution.leaf_id.as_ref().and_then(|id| issues.get(id));
            let attributed_disposition = attributed_leaf.and_then(|leaf| leaf.get("disposition")).and_then(Value::as_str);
            let leaf = if attributed_disposition == Some("deferred") {
                attribution.leaf_id.as_ref().and_then(|id| issues.get(id).map(|issue| json!({
                    "node_id":id,"title":issue.get("title"),"version":issue.get("version"),"state":issue.get("state"),
                    "score":issue.get("score").cloned().unwrap_or(json!(0.0)),"disposition":issue.get("disposition")
                })))
            } else { unresolved.first().cloned() };
            let leaf_id = leaf.as_ref().and_then(|value| value.get("node_id")).and_then(Value::as_str).unwrap_or_default();
            let blockers = relations.values().filter(|relation| relation.get("relation_type").and_then(Value::as_str) == Some("blocked_by") && relation.get("source_id").and_then(Value::as_str) == Some(leaf_id)).cloned().collect::<Vec<_>>();
            let handoffs = relations.values().filter(|relation| pending_handoff_relations.contains(relation.get("relation_type").and_then(Value::as_str).unwrap_or_default()) && relation.get("source_id").and_then(Value::as_str) == Some(leaf_id)).cloned().collect::<Vec<_>>();
            let is_stale = ledger_sequence.saturating_sub(attribution.sequence) > freshness_gap;
            let status = if attributed_disposition == Some("deferred") { "deferred" }
                else if leaf.as_ref().and_then(|value| value.get("state")).and_then(Value::as_str) == Some("blocked") || !blockers.is_empty() || !handoffs.is_empty() { "blocked" }
                else if leaf.is_none() { "none" }
                else if is_stale { "stale" }
                else { "active" };
            if !status_filter.is_empty() && !status_filter.contains(status) { continue; }
            let tree = trees.get(&attribution.tree_id);
            let mut row = json!({
                "tree_id":attribution.tree_id,
                "objective":tree.and_then(|value| value.get("objective")).or_else(|| tree.and_then(|value| value.get("title"))).cloned().unwrap_or(Value::Null),
                "member":attribution.member,
                "status":status,
                "leaf":leaf,
                "latest_attributable_transition":{"event_id":attribution.event_id,"sequence":attribution.sequence,"timestamp":attribution.occurred_at},
                "attribution_basis":attribution.basis,
                "freshness":{"classification":if is_stale {"stale"} else {"recent"},"rule":"ledger_sequence_gap","current_sequence":ledger_sequence,"attribution_sequence":attribution.sequence,"max_gap":freshness_gap},
                "blocker_count":blockers.len(),"directed_handoff_count":handoffs.len(),
                "live_presence":{"claimed":false,"capability":"unavailable","reason":"the epistemic graph has no typed heartbeat capability"}
            });
            if !compact {
                row["blockers"] = Value::Array(blockers);
                row["directed_handoffs"] = Value::Array(handoffs);
                row["distinctions"] = json!({"unresolved":!unresolved.is_empty(),"assigned":attribution.basis != "canonical_transition_actor","recently_transitioned":!is_stale,"live_process_presence":false});
            }
            rows.push(row);
        }

        for unknown in &unknown_member_ids {
            let status = "unknown";
            if status_filter.is_empty() || status_filter.contains(status) {
                rows.push(json!({
                    "tree_id":Value::Null,"objective":Value::Null,"member":unknown,"status":status,"leaf":Value::Null,
                    "latest_attributable_transition":Value::Null,"attribution_basis":"insufficient_canonical_identity_evidence",
                    "freshness":{"classification":"unknown","rule":"canonical_identity_required"},
                    "blocker_count":0,"directed_handoff_count":0,
                    "live_presence":{"claimed":false,"capability":"unavailable","reason":"the epistemic graph has no typed heartbeat capability"}
                }));
            }
        }
        if !requested_members.is_empty() {
            for member in selected_members.iter() {
                if rows.iter().any(|row| row.get("member").and_then(Value::as_str) == Some(member)) { continue; }
                let status = if unknown_member_ids.is_empty() { "none" } else { "unknown" };
                if status_filter.is_empty() || status_filter.contains(status) {
                    rows.push(json!({
                        "tree_id":Value::Null,"objective":Value::Null,"member":member,"status":status,"leaf":Value::Null,
                        "latest_attributable_transition":Value::Null,"attribution_basis":if status == "none" {"complete_canonical_scan_verified_absence"} else {"incomplete_coverage"},
                        "freshness":{"classification":"not_applicable","rule":"no_attributed_work"},
                        "blocker_count":0,"directed_handoff_count":0,
                        "live_presence":{"claimed":false,"capability":"unavailable","reason":"the epistemic graph has no typed heartbeat capability"}
                    }));
                }
            }
        }
        rows.sort_by(|left, right| left.get("member").and_then(Value::as_str).cmp(&right.get("member").and_then(Value::as_str))
            .then_with(|| left.get("tree_id").and_then(Value::as_str).cmp(&right.get("tree_id").and_then(Value::as_str))));
        let filtered_total = rows.len();
        let page = rows.into_iter().skip(offset).take(limit).collect::<Vec<_>>();
        let next_offset = offset + page.len();
        let has_more = next_offset < filtered_total;
        let next_cursor = has_more.then(|| format!("team-work-overview:{current_head}:{next_offset}"));
        let unattributed_active_tree_count = unresolved_by_tree.keys()
            .filter(|tree| (tree_filter.is_empty() || tree_filter.contains(*tree)) && !attributed_trees.contains(*tree))
            .count();
        let coverage_complete = unknown_member_ids.is_empty();
        Ok(json!({
            "schema":"narada.epistemic.team_work_overview.v1","status":"ok","mode":if compact {"compact"} else {"detailed"},
            "query_origin":"named_template","template":"epistemic:team-work-overview",
            "ledger_head":current_head,"ledger_sequence":ledger_sequence,"items":page,"returned":page.len(),"limit":limit,
            "has_more":has_more,"next_cursor":next_cursor,
            "coverage":{
                "queried_members":if requested_members.is_empty() {members.iter().map(|member| Value::String(member.identity.clone())).collect::<Vec<_>>()} else {requested_members},
                "queried_trees":if requested_trees.is_empty() {trees.keys().map(|tree| Value::String(tree.clone())).collect::<Vec<_>>()} else {requested_trees},
                "complete":coverage_complete,"total_matching":filtered_total,"omitted_count":filtered_total.saturating_sub(page.len()),
                "unattributed_active_tree_count":unattributed_active_tree_count,
                "partial_evidence_classes":if coverage_complete {json!([])} else {json!(["canonical_member_resolution"])},
                "unavailable_evidence_classes":["live_process_heartbeat"]
            },
            "semantics":{
                "frontier":"unresolved only; never ownership or current activity",
                "communications":"coordination and attribution only; never scientific evidence",
                "live_presence":"not claimed without a separate typed heartbeat capability"
            },
            "bounded":true
        }))
    }
}
