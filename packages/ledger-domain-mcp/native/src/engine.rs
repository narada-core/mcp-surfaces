//! Generic ledger-domain engine: hosts one `narada.ledger-domain.v1`
//! descriptor as a complete event-ledger MCP domain.
//!
//! The behavior is lifted from the epistemic-graph implementation
//! (`packages/shared/mcp-surfaces-native/native/src/epistemic_graph.rs`) with
//! every domain constant replaced by descriptor reads: vocabulary, operation
//! validation and ID derivation, projection DDL and fold, query behavior,
//! numeric caps, schema ids, storage layout, guidance text, and the five
//! feature modules (proposals, sequences, source_inspect, snapshot, export).
//! Digest-bearing JSON emission keeps its original field insertion order.

use crate::descriptor::{Descriptor, DerivedKeyRecipe};
use narada_mcp_event_ledger::digest::{now, safe_name, sha256};
use narada_mcp_event_ledger::ledger::LedgerLayout;
use narada_mcp_event_ledger::{
    args as ledger_args, chain, io as ledger_io, ledger as event_ledger, lock,
    projection as ledger_projection, ErrorSchema,
};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Map, Value};
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use uuid::Uuid;

/// One `create table` statement parsed out of the projection DDL.
#[derive(Clone, Debug)]
struct TableSpec {
    name: String,
    columns: Vec<String>,
    primary_key: String,
}

/// A loaded domain: the descriptor plus derived parse products.
pub struct Engine {
    pub domain: Descriptor,
    error: ErrorSchema,
    event_hash_field: &'static str,
    tables: Vec<TableSpec>,
    entity_table: String,
    relation_table: String,
    records_table: String,
}

/// Parse the `[..N]` truncation out of an id-recipe template.
fn template_truncation(template: &str, fallback: usize) -> usize {
    if let Some(start) = template.find("[..") {
        let rest = &template[start + 3..];
        if let Some(end) = rest.find(']') {
            if let Ok(value) = rest[..end].parse::<usize>() {
                return value;
            }
        }
    }
    fallback
}

/// Parse the literal prefix before the first `{` placeholder of a template.
fn template_prefix(template: &str) -> &str {
    match template.find('{') {
        Some(index) => &template[..index],
        None => template,
    }
}

/// Parse the projection DDL into table specs. Column names and the primary
/// key come from each `create table` statement; non-table segments (pragma)
/// are skipped.
fn parse_ddl_tables(ddl: &str) -> Result<Vec<TableSpec>, String> {
    let mut tables = Vec::new();
    for segment in ddl.split(';') {
        let segment = segment.trim();
        let Some(rest) = segment.strip_prefix("create table ") else {
            continue;
        };
        let Some(open) = rest.find('(') else {
            return Err(format!("domain_invalid:projection_ddl:{segment}"));
        };
        let name = rest[..open].trim().to_string();
        let body = rest[open + 1..].trim_end_matches(')').trim();
        let mut columns = Vec::new();
        let mut primary_key = None;
        for column in body.split(',') {
            let column = column.trim();
            let Some(column_name) = column.split_whitespace().next() else {
                continue;
            };
            columns.push(column_name.to_string());
            if column.contains("primary key") {
                primary_key = Some(column_name.to_string());
            }
        }
        let primary_key = primary_key
            .ok_or_else(|| format!("domain_invalid:projection_ddl_no_primary_key:{name}"))?;
        tables.push(TableSpec {
            name,
            columns,
            primary_key,
        });
    }
    if tables.is_empty() {
        return Err("domain_invalid:projection_ddl_no_tables".to_string());
    }
    Ok(tables)
}

impl Engine {
    pub fn new(domain: Descriptor) -> Result<Engine, String> {
        let tables = parse_ddl_tables(&domain.projection.ddl)?;
        let entity_op = &domain.id_derivation.entity.applies_to;
        let relation_op = &domain.id_derivation.relation.applies_to;
        let fold_table = |operation: &str| {
            domain
                .projection
                .fold
                .iter()
                .find(|entry| entry.operation == operation)
                .map(|entry| entry.table.clone())
                .ok_or_else(|| format!("domain_invalid:projection_fold_missing:{operation}"))
        };
        let entity_table = fold_table(entity_op)?;
        let relation_table = fold_table(relation_op)?;
        let records_table = domain
            .projection
            .fold
            .iter()
            .find(|entry| entry.table != entity_table && entry.table != relation_table)
            .map(|entry| entry.table.clone())
            .ok_or_else(|| "domain_invalid:projection_fold_missing_records".to_string())?;
        for table in [&entity_table, &relation_table, &records_table] {
            if !tables.iter().any(|spec| &spec.name == table) {
                return Err(format!("domain_invalid:projection_fold_unknown_table:{table}"));
            }
        }
        let error_schema: &'static str =
            Box::leak(domain.identity.error_schema_id.clone().into_boxed_str());
        let event_hash_field: &'static str =
            Box::leak(domain.storage.event_hash_field.clone().into_boxed_str());
        Ok(Engine {
            domain,
            error: ErrorSchema(error_schema),
            event_hash_field,
            tables,
            entity_table,
            relation_table,
            records_table,
        })
    }

    fn table(&self, name: &str) -> &TableSpec {
        self.tables
            .iter()
            .find(|spec| spec.name == name)
            .expect("fold tables are validated at load")
    }

    fn entity_op(&self) -> &str {
        &self.domain.id_derivation.entity.applies_to
    }

    fn relation_op(&self) -> &str {
        &self.domain.id_derivation.relation.applies_to
    }

    fn max_operations(&self) -> usize {
        self.domain.caps.operations_per_proposal.max as usize
    }

    /// Schema id derived from the domain namespace: `<namespace>.<name>`.
    fn schema_id(&self, name: &str) -> String {
        format!("{}.{}", self.domain.identity.schema_namespace, name)
    }

    /// Tool name derived from the tool prefix: `<prefix>_<verb>`.
    fn tool_name(&self, verb: &str) -> String {
        format!("{}_{}", self.domain.identity.tool_prefix, verb)
    }

    pub fn list_tools(&self) -> Vec<Value> {
        self.domain
            .tools
            .iter()
            .filter(|tool| {
                tool.feature
                    .as_deref()
                    .map(|feature| self.domain.features.enabled(feature))
                    .unwrap_or(true)
            })
            .map(|tool| {
                json!({
                    "name": tool.name,
                    "description": tool.description,
                    "inputSchema": tool.input_schema,
                    "annotations": tool.annotations,
                })
            })
            .collect()
    }

    pub fn call_tool(&self, name: &str, args: &Map<String, Value>, site_root: &Path) -> Result<Value, Value> {
        let prefix = format!("{}_", self.domain.identity.tool_prefix);
        let unknown = || {
            Err(self.error(
                "unknown_tool",
                &format!("unknown_tool:{name}"),
                Value::Null,
            ))
        };
        let Some(verb) = name.strip_prefix(&prefix) else {
            return unknown();
        };
        // Feature-owned verbs dispatch only when the feature is enabled.
        let feature = match verb {
            "source_inspect" => Some("source_inspect"),
            "snapshot" => Some("snapshot"),
            "export" => Some("export"),
            "sequence_create" | "sequence_status" | "sequence_list" | "sequence_claim_next"
            | "sequence_claims" => Some("sequences"),
            "proposal_submit" | "submit_review_admit" | "capture_sources" | "proposal_read"
            | "proposal_resubmit" | "proposal_review" | "proposal_admit" | "proposal_reject" => {
                Some("proposals")
            }
            _ => None,
        };
        if let Some(feature) = feature {
            if !self.domain.features.enabled(feature) {
                return unknown();
            }
        }
        match verb {
            "guidance" => Ok(self.guidance_with_request(args)),
            "status" => self.status(site_root),
            "query" => self.query(site_root, args),
            "query_batch" => self.query_batch(site_root, args),
            "source_inspect" => self.source_inspect(site_root, args),
            "neighborhood" => self.neighborhood(site_root, args),
            "snapshot" => self.snapshot(site_root, args),
            "sequence_create" => self.sequence_create(site_root, args),
            "sequence_status" => self.sequence_status(site_root, args),
            "sequence_list" => self.sequence_list(site_root, args),
            "sequence_claim_next" => self.sequence_claim_next(site_root, args),
            "sequence_claims" => self.sequence_claims(site_root, args),
            "proposal_submit" => self.proposal_submit(site_root, args),
            "submit_review_admit" => self.submit_review_admit(site_root, args),
            "capture_sources" => self.capture_sources(site_root, args),
            "proposal_read" => self.proposal_read(site_root, args),
            "proposal_resubmit" => self.proposal_resubmit(site_root, args),
            "proposal_review" => self.proposal_review(site_root, args),
            "proposal_admit" => self.proposal_admit(site_root, args),
            "proposal_reject" => self.proposal_reject(site_root, args),
            "export" => self.export(site_root, args),
            _ => unknown(),
        }
    }

    fn sequence_create(&self, root: &Path, args: &Map<String, Value>) -> Result<Value, Value> {
        self.prepare(root)?;
        let name = self.validated_sequence_name(args)?;
        let actor = self.required(args, "actor")?;
        let authority_basis = self.required_object(args, "authority_basis")?;
        let start_at = self.optional_u64(args, "start_at", 1)?;
        if start_at < self.domain.features.sequences.start_at_min {
            return Err(self.error(
                "sequence_start_invalid",
                "sequence start_at must be at least 1",
                json!({"start_at":start_at}),
            ));
        }
        let lock_key = format!("sequence-{}", sha256(name.as_bytes()));
        self.with_authority_lock(root, &lock_key, || {
            let directory = self.sequence_directory(root, &name);
            let manifest_path = directory.join("sequence.json");
            if manifest_path.exists() {
                let manifest = self.read_json(&manifest_path)?;
                self.verify_sequence_manifest(&manifest, &name)?;
                if manifest.get("start_at").and_then(Value::as_u64) != Some(start_at) {
                    return Err(self.error(
                        "sequence_configuration_conflict",
                        "sequence already exists with a different start_at",
                        json!({"sequence_name":name,"existing_start_at":manifest.get("start_at"),"requested_start_at":start_at}),
                    ));
                }
                return self.sequence_status_value(root, &name, "already_exists");
            }
            fs::create_dir_all(directory.join("claims"))
                .map_err(self.io_error("sequence_claim_store_create_failed"))?;
            fs::create_dir_all(directory.join("idempotency"))
                .map_err(self.io_error("sequence_idempotency_store_create_failed"))?;
            let sequences = &self.domain.features.sequences;
            let mut manifest = json!({
                "schema":sequences.manifest_schema_id,
                "sequence_id":self.generated_sequence_id(&name),
                "sequence_name":name,
                "start_at":start_at,
                "step":sequences.step,
                "created_by":actor,
                "authority_basis":authority_basis,
                "idempotency_key":args.get("idempotency_key").cloned().unwrap_or(Value::Null),
                "created_at":now()
            });
            let hash = self.digest_value(&manifest)?;
            manifest[self.domain.features.sequences.manifest_hash_field.clone()] = json!(hash);
            self.write_new_json(&manifest_path, &manifest)?;
            self.sequence_status_value(root, &name, "created")
        })
    }

    fn sequence_status(&self, root: &Path, args: &Map<String, Value>) -> Result<Value, Value> {
        let name = self.validated_sequence_name(args)?;
        let lock_key = format!("sequence-{}", sha256(name.as_bytes()));
        self.with_authority_lock(root, &lock_key, || {
            self.sequence_status_value(root, &name, "ready")
        })
    }

    fn sequence_status_value(&self, root: &Path, name: &str, status: &str) -> Result<Value, Value> {
        let manifest = self.load_sequence_manifest(root, name)?;
        let claims = self.verified_sequence_claims(root, name, &manifest)?;
        let start_at = manifest["start_at"].as_u64().unwrap();
        let last_claim = claims.last().cloned().unwrap_or(Value::Null);
        let last_value = last_claim.get("value").and_then(Value::as_u64);
        let next_value = match last_value {
            Some(value) => value.checked_add(1).map(Value::from).unwrap_or(Value::Null),
            None => Value::from(start_at),
        };
        Ok(json!({
            "schema":self.domain.features.sequences.status_schema_id,
            "status":status,
            "sequence_id":manifest["sequence_id"],
            "sequence_name":name,
            "start_at":start_at,
            "step":self.domain.features.sequences.step,
            "claim_count":claims.len(),
            "last_claimed_value":last_value,
            "next_value":next_value,
            "exhausted":next_value.is_null(),
            "latest_claim":last_claim,
            "integrity_status":"valid"
        }))
    }

    fn sequence_list(&self, root: &Path, args: &Map<String, Value>) -> Result<Value, Value> {
        let limit = self.page_limit(args)?;
        let offset = self.page_offset(args)?;
        let mut items = Vec::new();
        if self.sequences(root).exists() {
            for entry in
                fs::read_dir(self.sequences(root)).map_err(self.io_error("sequence_store_read_failed"))?
            {
                let Ok(entry) = entry else { continue };
                let manifest_path = entry.path().join("sequence.json");
                if !manifest_path.exists() {
                    continue;
                }
                let hash = entry.file_name().to_string_lossy().to_string();
                let item = self.with_authority_lock(root, &format!("sequence-{hash}"), || {
                    let manifest = self.read_json(&manifest_path)?;
                    let name = manifest
                        .get("sequence_name")
                        .and_then(Value::as_str)
                        .ok_or_else(|| {
                            self.error(
                                "sequence_manifest_invalid",
                                "sequence manifest lacks sequence_name",
                                json!({"path":manifest_path.to_string_lossy()}),
                            )
                        })?;
                    self.verify_sequence_manifest(&manifest, name)?;
                    let claims = self.verified_sequence_claims(root, name, &manifest)?;
                    Ok(json!({
                        "sequence_id":manifest["sequence_id"],
                        "sequence_name":name,
                        "start_at":manifest["start_at"],
                        "claim_count":claims.len(),
                        "last_claimed_value":claims.last().and_then(|claim| claim.get("value")).cloned().unwrap_or(Value::Null),
                        "created_at":manifest["created_at"]
                    }))
                })?;
                items.push(item);
            }
        }
        items.sort_by(|left, right| {
            left["sequence_name"]
                .as_str()
                .cmp(&right["sequence_name"].as_str())
        });
        let total = items.len();
        let page = items
            .into_iter()
            .skip(offset)
            .take(limit)
            .collect::<Vec<_>>();
        let count = page.len();
        Ok(
            json!({"schema":self.domain.features.sequences.list_schema_id,"items":page,"offset":offset,"limit":limit,"count":count,"total":total,"has_more":offset+count<total}),
        )
    }

    fn sequence_claim_next(&self, root: &Path, args: &Map<String, Value>) -> Result<Value, Value> {
        self.prepare(root)?;
        let name = self.validated_sequence_name(args)?;
        let actor = self.required(args, "actor")?;
        let authority_basis = self.required_object(args, "authority_basis")?;
        let idempotency_key = self.required(args, "idempotency_key")?;
        let request_digest = self.digest_value(
            &json!({"sequence_name":name,"actor":actor,"authority_basis":authority_basis}),
        )?;
        let lock_key = format!("sequence-{}", sha256(name.as_bytes()));
        self.with_authority_lock(root, &lock_key, || {
            let manifest = self.load_sequence_manifest(root, &name)?;
            let claims = self.verified_sequence_claims(root, &name, &manifest)?;
            if let Some(claim) = Self::find_sequence_claim_by_idempotency(&claims, &idempotency_key) {
                if claim.get("request_digest").and_then(Value::as_str) != Some(request_digest.as_str())
                {
                    return Err(self.error(
                        "sequence_claim_idempotency_conflict",
                        "idempotency key already names a different claim request",
                        json!({"sequence_name":name,"idempotency_key":idempotency_key,"claim_id":claim["claim_id"]}),
                    ));
                }
                self.recover_sequence_idempotency_index(root, &name, &idempotency_key, claim)?;
                return Ok(self.sequence_claim_receipt(claim, true));
            }
            let start_at = manifest["start_at"].as_u64().unwrap();
            let value = match claims.last().and_then(|claim| claim["value"].as_u64()) {
                Some(previous) => previous.checked_add(1).ok_or_else(|| {
                    self.error(
                        "sequence_exhausted",
                        "sequence has exhausted u64 values",
                        json!({"sequence_name":name,"last_claimed_value":previous}),
                    )
                })?,
                None => start_at,
            };
            let chain_field = &self.domain.features.sequences.claim_chain_field;
            let previous_hash = claims
                .last()
                .and_then(|claim| claim[self.domain.features.sequences.claim_hash_field.clone()].as_str())
                .map(str::to_string);
            let claim_id = self.generated_claim_id(&name, &idempotency_key);
            let mut claim = json!({
                "schema":self.domain.features.sequences.claim_schema_id,
                "sequence_id":manifest["sequence_id"],
                "sequence_name":name,
                "value":value,
                "claim_id":claim_id,
                chain_field.clone():previous_hash,
                "actor":actor,
                "authority_basis":authority_basis,
                "idempotency_key":idempotency_key,
                "request_digest":request_digest,
                "claimed_at":now()
            });
            let claim_hash = self.digest_value(&claim)?;
            claim[self.domain.features.sequences.claim_hash_field.clone()] = json!(claim_hash);
            self.write_new_json(
                &self
                    .sequence_claims_directory(root, &name)
                    .join(self.sequence_claim_file_name(value)),
                &claim,
            )?;
            self.recover_sequence_idempotency_index(root, &name, &idempotency_key, &claim)?;
            Ok(self.sequence_claim_receipt(&claim, false))
        })
    }

    fn sequence_claims(&self, root: &Path, args: &Map<String, Value>) -> Result<Value, Value> {
        let name = self.validated_sequence_name(args)?;
        let limit = self.page_limit(args)?;
        let offset = self.page_offset(args)?;
        let lock_key = format!("sequence-{}", sha256(name.as_bytes()));
        self.with_authority_lock(root, &lock_key, || {
            let manifest = self.load_sequence_manifest(root, &name)?;
            let claims = self.verified_sequence_claims(root, &name, &manifest)?;
            let total = claims.len();
            let page = claims
                .into_iter()
                .skip(offset)
                .take(limit)
                .collect::<Vec<_>>();
            let count = page.len();
            Ok(
                json!({"schema":self.domain.features.sequences.claims_schema_id,"sequence_name":name,"claims":page,"offset":offset,"limit":limit,"count":count,"total":total,"has_more":offset+count<total}),
            )
        })
    }

    fn sequence_claim_receipt(&self, claim: &Value, replay: bool) -> Value {
        let next_value = claim["value"]
            .as_u64()
            .and_then(|value| value.checked_add(1));
        json!({
            "schema":self.domain.features.sequences.claim_receipt_schema_id,
            "status":if replay{"idempotent_replay"}else{"claimed"},
            "idempotency_replay":replay,
            "sequence_id":claim["sequence_id"],
            "sequence_name":claim["sequence_name"],
            "value":claim["value"],
            "claim_id":claim["claim_id"],
            "claimed_at":claim["claimed_at"],
            "next_value":next_value,
            "exhausted":next_value.is_none()
        })
    }

    fn proposal_submit(&self, root: &Path, args: &Map<String, Value>) -> Result<Value, Value> {
        self.prepare(root)?;
        let actor = self.required(args, "actor")?;
        let supplied_operations = args
            .get("operations")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                self.error(
                    "invalid_proposal",
                    "operations must be an array",
                    Value::Null,
                )
            })?;
        let count = &self.domain.caps.operations_per_proposal;
        if supplied_operations.len() < count.min as usize
            || supplied_operations.len() > count.max as usize
        {
            return Err(self.error(
                "invalid_proposal",
                &format!(
                    "operations count must be between {} and {}",
                    count.min, count.max
                ),
                json!({"count":supplied_operations.len()}),
            ));
        }
        let operations = self.normalize_operations(supplied_operations)?;
        self.validate_operations(&operations, false)?;
        let expected = self.resolve_expected_ledger_head(root, args.get("expected_ledger_head"))?;
        let semantic_content = json!({"actor":actor,"authority_basis":args.get("authority_basis"),"operations":operations});
        let content_fingerprint = self.digest_value(&semantic_content)?;
        let idempotency_key = args
            .get("idempotency_key")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| {
                self.derived_idempotency_key(
                    &self.domain.id_derivation.derived_idempotency_keys.proposal,
                    &semantic_content,
                )
            });
        let proposal_id = format!(
            "{}{}",
            template_prefix(&self.domain.id_derivation.generated_ids.proposal_id),
            Uuid::new_v4()
        );
        let created_at = now();
        let proposals_feature = &self.domain.features.proposals;
        let payload = json!({
            "schema":proposals_feature.proposal_schema_id, "proposal_id":proposal_id,
            "status":"submitted", "actor":actor, "authority_basis":args.get("authority_basis"),
            "idempotency_key":idempotency_key, "expected_ledger_head":expected,
            "created_at":created_at, "content_fingerprint":content_fingerprint, "operations":operations
        });
        let digest = self.digest_value(&payload)?;
        let mut stored = payload;
        stored
            .as_object_mut()
            .unwrap()
            .insert("digest".into(), json!(digest));
        let idem_path = self.proposals(root).join(format!("idem-{}.txt", safe_name(&idempotency_key)));
        if idem_path.exists() {
            let existing =
                fs::read_to_string(&idem_path).map_err(self.io_error("proposal_idempotency_read_failed"))?;
            let stored = self.read_json(&self.proposals(root).join(format!("{}.json", existing.trim())))?;
            if stored
                .get("content_fingerprint")
                .and_then(Value::as_str)
                .is_some()
                && stored.get("content_fingerprint") != Some(&json!(content_fingerprint))
            {
                return Err(self.error(
                    "proposal_idempotency_conflict",
                    "idempotency key already names different proposal content",
                    json!({"idempotency_key":idempotency_key,"existing_proposal_id":stored["proposal_id"]}),
                ));
            }
            return Ok(self.proposal_receipt(&stored));
        }
        self.write_new_json(
            &self.proposals(root).join(format!("{proposal_id}.json")),
            &stored,
        )?;
        self.write_new(&idem_path, proposal_id.as_bytes())?;
        Ok(self.proposal_receipt(&stored))
    }

    fn submit_review_admit(&self, root: &Path, args: &Map<String, Value>) -> Result<Value, Value> {
        let proposals_feature = &self.domain.features.proposals;
        let submission = self.proposal_submit(root, args)?;
        let proposal_id = submission["proposal_id"].as_str().ok_or_else(|| {
            self.error(
                "proposal_submission_corrupt",
                "proposal id missing",
                submission.clone(),
            )
        })?;
        let lifecycle = self.proposal_lifecycle(root, proposal_id)?;
        if lifecycle["status"] == "admitted" {
            let review =
                self.read_json(&self.proposals(root).join(format!("{}.review.json", safe_name(proposal_id))))?;
            return Ok(json!({
                "schema":proposals_feature.compound_schema_id,
                "status":"already_admitted",
                "submission":submission,
                "review":review,
                "admission":lifecycle,
                "review_gate_preserved":proposals_feature.review_gate_preserved,
                "certifies_truth":proposals_feature.certifies_truth
            }));
        }
        let review = self.proposal_review(
            root,
            &Map::from_iter([("proposal_id".into(), json!(proposal_id))]),
        )?;
        if review["status"] != "policy_valid" {
            return Err(self.error(
                "proposal_not_admissible",
                "compound contribution stopped at the preserved review gate",
                json!({"submission":submission,"review":review}),
            ));
        }
        let admission_idempotency = self.derived_idempotency_key(
            &self.domain.id_derivation.derived_idempotency_keys.admission,
            &json!({"proposal_id":proposal_id,"proposal_digest":submission["proposal_digest"]}),
        );
        let admission = self.proposal_admit(
            root,
            &Map::from_iter([
                ("proposal_id".into(), json!(proposal_id)),
                ("actor".into(), json!(self.required(args, "actor")?)),
                (
                    "authority_basis".into(),
                    args.get("authority_basis").cloned().unwrap_or(Value::Null),
                ),
                (
                    "expected_ledger_head".into(),
                    submission["expected_ledger_head"].clone(),
                ),
                ("idempotency_key".into(), json!(admission_idempotency)),
            ]),
        )?;
        Ok(json!({
            "schema":proposals_feature.compound_schema_id,
            "status":"admitted",
            "submission":submission,
            "review":review,
            "admission":admission,
            "review_gate_preserved":proposals_feature.review_gate_preserved,
            "certifies_truth":proposals_feature.certifies_truth
        }))
    }

    fn normalize_operations(&self, operations: &[Value]) -> Result<Vec<Value>, Value> {
        let entity_op = self.domain.id_derivation.entity.applies_to.clone();
        let relation_op = self.domain.id_derivation.relation.applies_to.clone();
        let wiring = &self.domain.id_derivation.local_ref_wiring;
        let entity_key_field = self
            .domain
            .projection
            .fold
            .iter()
            .find(|entry| entry.operation == entity_op)
            .map(|entry| entry.key_field.clone())
            .expect("entity fold entry validated at load");
        let mut local_ids = std::collections::HashMap::new();
        let mut first_pass = Vec::with_capacity(operations.len());
        for operation in operations {
            let mut normalized = operation.clone();
            if operation.get("op").and_then(Value::as_str) == Some(entity_op.as_str()) {
                let object = normalized.as_object_mut().unwrap();
                if object.get(&entity_key_field).and_then(Value::as_str).is_none() {
                    let kind = object.get("kind").and_then(Value::as_str).unwrap_or("");
                    let title = object.get("title").and_then(Value::as_str).unwrap_or("");
                    if !kind.is_empty() && !title.is_empty() {
                        let recipe = &self.domain.id_derivation.entity;
                        let mut digest_input = Map::new();
                        for field in &recipe.digest_input_fields {
                            digest_input.insert(
                                field.clone(),
                                object.get(field).cloned().unwrap_or(Value::Null),
                            );
                        }
                        let digest = self.digest_value(&Value::Object(digest_input))?;
                        object.insert(
                            entity_key_field.clone(),
                            json!(format!(
                                "{}:{}",
                                safe_name(kind),
                                &digest[..template_truncation(&recipe.template, 20)]
                            )),
                        );
                    }
                }
                if let (Some(local_ref), Some(entity_id)) = (
                    object.get(&wiring.declare_field).and_then(Value::as_str),
                    object.get(&entity_key_field).and_then(Value::as_str),
                ) {
                    if local_ids
                        .insert(local_ref.to_string(), entity_id.to_string())
                        .is_some()
                    {
                        return Err(self.error(
                            &wiring.duplicate_refusal_code,
                            "entity local_ref must be unique within a proposal",
                            json!({"local_ref":local_ref}),
                        ));
                    }
                }
            }
            first_pass.push(normalized);
        }
        first_pass
            .iter()
            .map(|operation| {
                let mut normalized = operation.clone();
                if operation.get("op").and_then(Value::as_str) == Some(relation_op.as_str()) {
                    let object = normalized.as_object_mut().unwrap();
                    for (ref_field, id_field) in &wiring.reference_fields {
                        if object.get(id_field).and_then(Value::as_str).is_none() {
                            if let Some(reference) = object.get(ref_field).and_then(Value::as_str) {
                                let resolved = local_ids.get(reference).ok_or_else(|| self.error(&wiring.unresolved_refusal_code, "relation reference does not identify an entity in this proposal", json!({"field":ref_field,"local_ref":reference})))?;
                                object.insert(id_field.clone(), json!(resolved));
                            }
                        }
                    }
                }
                let relation_key_field = self
                    .domain
                    .projection
                    .fold
                    .iter()
                    .find(|entry| entry.operation == relation_op)
                    .map(|entry| entry.key_field.clone())
                    .expect("relation fold entry validated at load");
                if normalized.get("op").and_then(Value::as_str) == Some(relation_op.as_str())
                    && normalized
                        .get(&relation_key_field)
                        .and_then(Value::as_str)
                        .is_none()
                {
                    let relation_type = normalized
                        .get("relation_type")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    let source_id = normalized
                        .get("source_id")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    let target_id = normalized
                        .get("target_id")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    if relation_type.is_empty() || source_id.is_empty() || target_id.is_empty() {
                        return Ok(normalized);
                    }
                    let recipe = &self.domain.id_derivation.relation;
                    let mut hash_input = Vec::new();
                    for (index, segment) in recipe.hash_input.split("\\0").enumerate() {
                        if index > 0 {
                            hash_input.push(0_u8);
                        }
                        let field = segment
                            .trim_start_matches('{')
                            .trim_end_matches('}')
                            .to_string();
                        let value = match field.as_str() {
                            "relation_type" => relation_type.clone(),
                            "source_id" => source_id.clone(),
                            "target_id" => target_id.clone(),
                            _ => normalized
                                .get(&field)
                                .and_then(Value::as_str)
                                .unwrap_or("")
                                .to_string(),
                        };
                        hash_input.extend_from_slice(value.as_bytes());
                    }
                    let digest = sha256(&hash_input);
                    normalized.as_object_mut().unwrap().insert(
                        relation_key_field,
                        json!(format!(
                            "{}{}-{}",
                            template_prefix(&recipe.template),
                            safe_name(&relation_type),
                            &digest[..template_truncation(&recipe.template, 16)]
                        )),
                    );
                }
                Ok(normalized)
            })
            .collect()
    }

    fn resolve_expected_ledger_head(&self, root: &Path, supplied: Option<&Value>) -> Result<Value, Value> {
        if supplied.is_none() || supplied.and_then(Value::as_str) == Some("latest") {
            return Ok(self.ledger_head(root)?.map(Value::String).unwrap_or(Value::Null));
        }
        Ok(supplied.cloned().unwrap_or(Value::Null))
    }

    fn derived_idempotency_key(&self, recipe: &DerivedKeyRecipe, source: &Value) -> String {
        let mut object = Map::new();
        for field in &recipe.input_fields {
            object.insert(field.clone(), source.get(field).cloned().unwrap_or(Value::Null));
        }
        let canonical = serde_json::to_vec(&Value::Object(object)).unwrap_or_default();
        format!(
            "{}{}",
            template_prefix(&recipe.template),
            &sha256(&canonical)[..template_truncation(&recipe.template, 24)]
        )
    }

    fn proposal_receipt(&self, proposal: &Value) -> Value {
        json!({
            "schema":self.domain.features.proposals.submission_receipt_schema_id,
            "status":proposal["status"],
            "proposal_id":proposal["proposal_id"],
            "proposal_digest":proposal["digest"],
            "content_fingerprint":proposal["content_fingerprint"],
            "operation_count":proposal["operations"].as_array().map_or(0, Vec::len),
            "expected_ledger_head":proposal["expected_ledger_head"],
            "created_at":proposal["created_at"]
        })
    }

    fn capture_sources(&self, root: &Path, args: &Map<String, Value>) -> Result<Value, Value> {
        self.prepare(root)?;
        self.rebuild_projection(root)?;
        let caps = &self.domain.caps.capture_sources;
        let sources = args
            .get("sources")
            .and_then(Value::as_array)
            .ok_or_else(|| self.error("invalid_capture", "sources must be an array", Value::Null))?;
        let supplied = args
            .get("operations")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if (sources.len() as u64) < caps.sources_min {
            return Err(self.error(
                "invalid_capture",
                "at least one source is required",
                Value::Null,
            ));
        }
        let mut operations = Vec::with_capacity(sources.len() + supplied.len());
        for source in sources {
            let source = source.as_object().ok_or_else(|| {
                self.error(
                    "invalid_capture",
                    "each source must be an object",
                    Value::Null,
                )
            })?;
            operations.push(json!({
                "op":self.entity_op(),
                "entity_id":self.required(source, "source_id")?,
                "kind":"source",
                "title":self.required(source, "title")?,
                "version":self.required(source, "version")?,
                "locator":self.required(source, "locator")?
            }));
        }
        for operation in &supplied {
            if operation.get("kind").and_then(Value::as_str) == Some("source") {
                return Err(self.error(
                    "invalid_capture",
                    "declare sources through the sources field, not operations",
                    Value::Null,
                ));
            }
            operations.push(operation.clone());
        }
        if operations.len() as u64 > caps.combined_max {
            return Err(self.error(
                "invalid_capture",
                &format!("combined source and operation count exceeds {}", caps.combined_max),
                json!({"source_count":sources.len(),"operation_count":supplied.len(),"combined_count":operations.len()}),
            ));
        }
        let existing_identities = self.existing_operation_identities(root, &operations)?;
        let mut proposal_args = args.clone();
        proposal_args.remove("sources");
        proposal_args.insert("operations".into(), json!(operations));
        let receipt = self.proposal_submit(root, &proposal_args)?;
        Ok(json!({
            "schema":self.domain.features.proposals.source_capture_schema_id,
            "status":"draft_submitted",
            "proposal_id":receipt["proposal_id"],
            "proposal_digest":receipt["proposal_digest"],
            "expected_ledger_head":receipt["expected_ledger_head"],
            "source_count":sources.len(),
            "operation_count":receipt["operation_count"],
            "existing_identity_count":existing_identities.len(),
            "existing_identities":existing_identities,
            "next":{"review":{"tool":self.tool_name("proposal_review"),"proposal_id":receipt["proposal_id"]}},
            "admission_requires_explicit_call":self.domain.features.proposals.capture_sources.admission_requires_explicit_call,
            "certifies_truth":self.domain.features.proposals.certifies_truth,
            "bounded":true
        }))
    }

    fn existing_operation_identities(&self, root: &Path, operations: &[Value]) -> Result<Vec<Value>, Value> {
        let db = Connection::open(self.projection_path(root)).map_err(self.db_error("projection_open_failed"))?;
        let mut existing = Vec::new();
        for operation in operations {
            let Some(op_kind) = operation.get("op").and_then(Value::as_str) else {
                continue;
            };
            let Some(fold) = self
                .domain
                .projection
                .fold
                .iter()
                .find(|entry| entry.operation == op_kind)
            else {
                continue;
            };
            let Some(identity) = operation
                .get(&fold.key_field)
                .and_then(Value::as_str)
            else {
                continue;
            };
            let table = self.table(&fold.table);
            let sql = format!(
                "select 1 from {} where {}=?1 limit 1",
                table.name, table.primary_key
            );
            let found = db
                .query_row(&sql, params![identity], |_| Ok(()))
                .optional()
                .map_err(self.db_error("projection_duplicate_check_failed"))?
                .is_some();
            if found {
                existing.push(json!({"op":operation["op"],"identity":identity}));
            }
        }
        Ok(existing)
    }

    fn proposal_read(&self, root: &Path, args: &Map<String, Value>) -> Result<Value, Value> {
        let id = self.required(args, "proposal_id")?;
        let proposal = self.load_proposal(root, &id)?;
        let operations = proposal["operations"].as_array().ok_or_else(|| {
            self.error(
                "proposal_corrupt",
                "proposal operations missing",
                json!({"proposal_id":id}),
            )
        })?;
        let offset = args.get("offset").and_then(Value::as_u64).unwrap_or(0) as usize;
        let caps = &self.domain.caps.proposal_read_limit;
        let limit = args
            .get("limit")
            .and_then(Value::as_u64)
            .unwrap_or(caps.default)
            .min(caps.max) as usize;
        let items = operations
            .iter()
            .skip(offset)
            .take(limit)
            .cloned()
            .collect::<Vec<_>>();
        let next_offset = (offset + items.len() < operations.len()).then_some(offset + items.len());
        let lifecycle = self.proposal_lifecycle(root, &id)?;
        Ok(json!({
            "schema":self.domain.features.proposals.read_schema_id,
            "status":lifecycle["status"],
            "lifecycle":lifecycle,
            "proposal_id":proposal["proposal_id"],
            "proposal_digest":proposal["digest"],
            "actor":proposal["actor"],
            "authority_basis":proposal["authority_basis"],
            "idempotency_key":proposal["idempotency_key"],
            "expected_ledger_head":proposal["expected_ledger_head"],
            "created_at":proposal["created_at"],
            "operation_count":operations.len(),
            "offset":offset,
            "limit":limit,
            "returned":items.len(),
            "operations":items,
            "has_more":next_offset.is_some(),
            "next_offset":next_offset,
            "bounded":true
        }))
    }

    fn operation_identity(&self, operation: &Value) -> Option<String> {
        let op_kind = operation.get("op").and_then(Value::as_str)?;
        let identity = self
            .domain
            .id_derivation
            .operation_identity_prefixes
            .get(op_kind)?;
        operation
            .get(&identity.id_field)
            .and_then(Value::as_str)
            .map(|value| format!("{}:{value}", identity.prefix))
    }

    fn proposal_resubmit(&self, root: &Path, args: &Map<String, Value>) -> Result<Value, Value> {
        let source_id = self.required(args, "source_proposal_id")?;
        let source = self.load_proposal(root, &source_id)?;
        let original = source["operations"].as_array().ok_or_else(|| {
            self.error(
                "proposal_corrupt",
                "proposal operations missing",
                json!({"proposal_id":source_id}),
            )
        })?;
        let requested_drops = args
            .get("drop_operation_ids")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let drop_ids = requested_drops
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect::<HashSet<_>>();
        if drop_ids.len() != requested_drops.len() {
            return Err(self.error(
                "invalid_proposal_resubmission",
                "drop_operation_ids must contain unique strings",
                Value::Null,
            ));
        }
        let known = original
            .iter()
            .filter_map(|operation| self.operation_identity(operation))
            .collect::<HashSet<_>>();
        let missing = drop_ids.difference(&known).cloned().collect::<Vec<_>>();
        if !missing.is_empty() {
            return Err(self.error(
                &self.domain.features.proposals.resubmit.missing_drop_refusal_code,
                "one or more drop_operation_ids do not identify source proposal operations",
                json!({"missing":missing}),
            ));
        }
        let replacements = args
            .get("replacements")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        self.validate_operations(&replacements, false)?;
        let mut operations = original
            .iter()
            .filter(|operation| {
                self.operation_identity(operation)
                    .map(|identity| !drop_ids.contains(&identity))
                    .unwrap_or(true)
            })
            .cloned()
            .collect::<Vec<_>>();
        operations.extend(replacements);
        let resubmit_caps = &self.domain.caps.resubmit;
        if (operations.len() as u64) < resubmit_caps.resulting_min
            || operations.len() as u64 > resubmit_caps.resulting_max
        {
            return Err(self.error(
                "invalid_proposal_resubmission",
                &format!(
                    "resulting operations count must be between {} and {}",
                    resubmit_caps.resulting_min, resubmit_caps.resulting_max
                ),
                json!({"count":operations.len()}),
            ));
        }
        let mut submit_args = Map::new();
        for key in [
            "actor",
            "authority_basis",
            "idempotency_key",
            "expected_ledger_head",
        ] {
            if let Some(value) = args.get(key) {
                submit_args.insert(key.to_string(), value.clone());
            }
        }
        submit_args.insert("operations".into(), json!(operations));
        let receipt = self.proposal_submit(root, &submit_args)?;
        Ok(json!({
            "schema":self.domain.features.proposals.resubmission_schema_id,
            "status":"draft_submitted",
            "source_proposal_id":source_id,
            "proposal_id":receipt["proposal_id"],
            "proposal_digest":receipt["proposal_digest"],
            "operation_count":receipt["operation_count"],
            "dropped_operation_ids":drop_ids,
            "replacement_count":args.get("replacements").and_then(Value::as_array).map_or(0, Vec::len),
            "expected_ledger_head":receipt["expected_ledger_head"],
            "next":{"review":{"tool":self.tool_name("proposal_review"),"proposal_id":receipt["proposal_id"]}},
            "admission_requires_explicit_call":true,
            "certifies_truth":self.domain.features.proposals.certifies_truth,
            "bounded":true
        }))
    }

    fn proposal_lifecycle(&self, root: &Path, proposal_id: &str) -> Result<Value, Value> {
        for path in self.ledger_files(root)? {
            let event = self.read_json(&path)?;
            if event.get("proposal_id").and_then(Value::as_str) == Some(proposal_id) {
                return Ok(json!({
                    "status":"admitted",
                    "event_id":event["event_id"],
                    "sequence":event["sequence"],
                    "ledger_head":event[self.domain.storage.event_hash_field.clone()],
                    "admitted_at":event["occurred_at"]
                }));
            }
        }
        let rejection_path = self.proposals(root).join(format!("{}.rejection.json", safe_name(proposal_id)));
        if rejection_path.exists() {
            let rejection = self.read_json(&rejection_path)?;
            return Ok(json!({
                "status":"rejected",
                "rejected_at":rejection["occurred_at"],
                "reason":rejection["reason"]
            }));
        }
        let review_path = self.proposals(root).join(format!("{}.review.json", safe_name(proposal_id)));
        if review_path.exists() {
            let review = self.read_json(&review_path)?;
            return Ok(json!({"status":"reviewed","review_status":review["status"]}));
        }
        Ok(json!({"status":"submitted"}))
    }

    fn proposal_review(&self, root: &Path, args: &Map<String, Value>) -> Result<Value, Value> {
        let id = self.required(args, "proposal_id")?;
        let proposal = self.load_proposal(root, &id)?;
        let operations = proposal["operations"].as_array().ok_or_else(|| {
            self.error(
                "proposal_corrupt",
                "proposal operations missing",
                json!({"proposal_id":id}),
            )
        })?;
        self.validate_operations(operations, true)?;
        self.validate_references(root, operations)?;
        let expected = proposal.get("expected_ledger_head").and_then(Value::as_str);
        let current = self.ledger_head(root)?;
        let head_matches = expected == current.as_deref();
        let review = json!({"schema":self.domain.features.proposals.review_schema_id,"proposal_id":id,"status":if head_matches{"policy_valid"}else{"stale"},"certifies_truth":self.domain.features.proposals.certifies_truth,"checks":{"schema":true,"references":true,"evidence_locations":true,"graph_invariants":true,"ledger_head":head_matches},"expected_ledger_head":expected,"actual_ledger_head":current});
        self.write_replace_json(&self.proposals(root).join(format!("{id}.review.json")), &review)?;
        Ok(review)
    }

    fn proposal_admit(&self, root: &Path, args: &Map<String, Value>) -> Result<Value, Value> {
        self.prepare(root)?;
        self.with_authority_lock(root, "ledger", || self.proposal_admit_locked(root, args))
    }

    fn proposal_admit_locked(&self, root: &Path, args: &Map<String, Value>) -> Result<Value, Value> {
        self.prepare(root)?;
        let id = self.required(args, "proposal_id")?;
        let actor = self.required(args, "actor")?;
        let proposal = self.load_proposal(root, &id)?;
        let idem = args
            .get("idempotency_key")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| {
                self.derived_idempotency_key(
                    &self.domain.id_derivation.derived_idempotency_keys.admission,
                    &json!({"proposal_id":id,"proposal_digest":proposal["digest"]}),
                )
            });
        let idem_path = self.ledger(root).join(format!("idem-{}.txt", safe_name(&idem)));
        if idem_path.exists() {
            let event_id =
                fs::read_to_string(&idem_path).map_err(self.io_error("ledger_idempotency_read_failed"))?;
            let event = self.read_json(&self.ledger(root).join(format!("{}.json", event_id.trim())))?;
            if event.get("proposal_id") != Some(&json!(id))
                || event.get("proposal_digest") != proposal.get("digest")
            {
                return Err(self.error(
                    "admission_idempotency_conflict",
                    "idempotency key already names a different proposal admission",
                    json!({"idempotency_key":idem,"existing_event_id":event_id.trim()}),
                ));
            }
            return Ok(self.admission_receipt(&event));
        }
        if let Some(event) = self.find_ledger_event_by_idempotency(root, &idem)? {
            if event.get("proposal_id") != Some(&json!(id))
                || event.get("proposal_digest") != proposal.get("digest")
            {
                return Err(self.error(
                    "admission_idempotency_conflict",
                    "idempotency key already names a different proposal admission",
                    json!({"idempotency_key":idem,"existing_event_id":event["event_id"]}),
                ));
            }
            if !idem_path.exists() {
                self.write_new(&idem_path, event["event_id"].as_str().unwrap().as_bytes())?;
            }
            return Ok(self.admission_receipt(&event));
        }
        let review = self.proposal_review(root, &Map::from_iter([("proposal_id".into(), json!(id))]))?;
        if review["status"] != "policy_valid" {
            return Err(self.error(
                "proposal_not_admissible",
                "proposal review is not policy_valid",
                review,
            ));
        }
        let expected_value = self.resolve_expected_ledger_head(root, args.get("expected_ledger_head"))?;
        let expected = expected_value.as_str();
        let current = self.ledger_head(root)?;
        if expected != current.as_deref()
            || proposal.get("expected_ledger_head").and_then(Value::as_str) != current.as_deref()
        {
            return Err(self.error(
                "ledger_head_conflict",
                "expected ledger head does not match",
                json!({"expected":expected,"proposal_expected":proposal.get("expected_ledger_head"),"actual":current}),
            ));
        }
        let event_hash_field = self.domain.storage.event_hash_field.clone();
        let outcome = event_ledger::append_event(
            self.error,
            &self.ledger_layout(root),
            &event_hash_field,
            None,
            Some(&idem),
            |ctx| {
                json!({"schema":self.domain.storage.event_schema_id,"sequence":ctx.sequence,"event_id":ctx.event_id,"event_kind":self.domain.features.proposals.event_kind,"previous_hash":ctx.previous_hash,"proposal_id":id,"proposal_digest":proposal["digest"],"operations":proposal["operations"],"actor":actor,"authority_basis":args.get("authority_basis"),"idempotency_key":idem,"occurred_at":now(),"certifies_truth":self.domain.features.proposals.certifies_truth})
            },
        )?;
        self.rebuild_projection(root)?;
        Ok(self.admission_receipt(&outcome.event))
    }

    fn admission_receipt(&self, event: &Value) -> Value {
        json!({
            "schema":self.domain.features.proposals.admission_receipt_schema_id,
            "status":"admitted",
            "proposal_id":event["proposal_id"],
            "proposal_digest":event["proposal_digest"],
            "event_id":event["event_id"],
            "sequence":event["sequence"],
            "operation_count":event["operations"].as_array().map_or(0, Vec::len),
            "ledger_head":event[self.domain.storage.event_hash_field.clone()],
            "certifies_truth":self.domain.features.proposals.certifies_truth
        })
    }

    fn proposal_reject(&self, root: &Path, args: &Map<String, Value>) -> Result<Value, Value> {
        let id = self.required(args, "proposal_id")?;
        let _ = self.load_proposal(root, &id)?;
        let rejection = json!({"schema":self.domain.features.proposals.rejection_schema_id,"proposal_id":id,"status":"rejected","actor":self.required(args,"actor")?,"reason":self.required(args,"reason")?,"occurred_at":now()});
        self.write_new_json(
            &self.proposals(root).join(format!("{id}.rejection.json")),
            &rejection,
        )?;
        Ok(rejection)
    }

    fn status(&self, root: &Path) -> Result<Value, Value> {
        self.prepare(root)?;
        self.rebuild_projection(root)?;
        let db = Connection::open(self.projection_path(root))
            .map_err(self.db_error("projection_open_failed"))?;
        let entities: i64 = db
            .query_row(&format!("select count(*) from {}", self.entity_table), [], |r| r.get(0))
            .map_err(self.db_error("projection_count_failed"))?;
        let relations: i64 = db
            .query_row(&format!("select count(*) from {}", self.relation_table), [], |r| r.get(0))
            .map_err(self.db_error("projection_count_failed"))?;
        let records: i64 = db
            .query_row(&format!("select count(*) from {}", self.records_table), [], |r| r.get(0))
            .map_err(self.db_error("projection_count_failed"))?;
        Ok(
            json!({"schema":self.schema_id("status.v1"),"status":"ok","implementation":self.domain.identity.implementation,"canonical_store":self.ledger(root).to_string_lossy(),"projection":self.projection_path(root).to_string_lossy(),"ledger_head":self.ledger_head(root)?,"event_count":self.ledger_files(root)?.len(),"entity_count":entities,"relation_count":relations,"record_count":records,"projection_rebuildable":true,"truth_certification":false}),
        )
    }

    /// Project one query row into the descriptor-listed field order. Row
    /// columns win, `"payload"` selects the full payload, anything else is
    /// looked up inside the payload (missing yields null, as before).
    fn project_row(
        row_values: &Map<String, Value>,
        payload: &Value,
        projection: &[String],
    ) -> Value {
        let mut out = Map::new();
        for field in projection {
            let value = if field == "payload" {
                payload.clone()
            } else if let Some(value) = row_values.get(field) {
                value.clone()
            } else {
                payload.get(field).cloned().unwrap_or(Value::Null)
            };
            out.insert(field.clone(), value);
        }
        Value::Object(out)
    }

    fn query(&self, root: &Path, args: &Map<String, Value>) -> Result<Value, Value> {
        self.rebuild_projection(root)?;
        let db = Connection::open(self.projection_path(root))
            .map_err(self.db_error("projection_open_failed"))?;
        let limit = args
            .get("limit")
            .and_then(Value::as_u64)
            .unwrap_or(self.domain.caps.query_limit.default)
            .min(self.domain.caps.query_limit.max);
        let offset = args.get("offset").and_then(Value::as_u64).unwrap_or(0);
        let kind = args.get("kind").and_then(Value::as_str).unwrap_or("");
        let compact = args
            .get("compact")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let text = args.get("text").and_then(Value::as_str).unwrap_or("");
        let like = format!("%{text}%");
        if let Some(record_kind) = args.get("record_kind").and_then(Value::as_str) {
            let sql = format!("select record_id,record_kind,payload_json,event_id from {} where record_kind=?1 and (?2='' or payload_json like ?3) order by record_id limit ?4 offset ?5", self.records_table);
            let mut stmt = db.prepare(&sql).map_err(self.db_error("projection_record_query_prepare_failed"))?;
            let projection = if compact {
                &self.domain.query.record_compact_projection
            } else {
                &self.domain.query.record_full_projection
            };
            let rows = stmt.query_map(params![record_kind,text,like,limit,offset], |row| {
                let payload = serde_json::from_str::<Value>(&row.get::<_,String>(2)?).unwrap_or(Value::Null);
                let mut row_values = Map::new();
                row_values.insert("record_id".into(), json!(row.get::<_,String>(0)?));
                row_values.insert("record_kind".into(), json!(row.get::<_,String>(1)?));
                row_values.insert("event_id".into(), json!(row.get::<_,String>(3)?));
                Ok(Self::project_row(&row_values, &payload, projection))
            }).map_err(self.db_error("projection_record_query_failed"))?;
            let items = rows
                .collect::<Result<Vec<_>, _>>()
                .map_err(self.db_error("projection_record_query_row_failed"))?;
            return Ok(
                json!({"schema":self.schema_id("query.v1"),"status":"ok","result_kind":"records","record_kind":record_kind,"compact":compact,"items":items,"offset":offset,"limit":limit,"returned":items.len(),"bounded":true}),
            );
        }
        let sql = format!("select entity_id,kind,payload_json,event_id from {} where (?1='' or kind=?1) and (?2='' or payload_json like ?3) order by entity_id limit ?4 offset ?5", self.entity_table);
        let mut stmt = db.prepare(&sql).map_err(self.db_error("projection_query_prepare_failed"))?;
        let projection = if compact {
            &self.domain.query.entity_compact_projection
        } else {
            &self.domain.query.entity_full_projection
        };
        let rows = stmt.query_map(params![kind,text,like,limit,offset], |row| {
            let payload = serde_json::from_str::<Value>(&row.get::<_,String>(2)?).unwrap_or(Value::Null);
            let mut row_values = Map::new();
            row_values.insert("entity_id".into(), json!(row.get::<_,String>(0)?));
            row_values.insert("kind".into(), json!(row.get::<_,String>(1)?));
            row_values.insert("event_id".into(), json!(row.get::<_,String>(3)?));
            Ok(Self::project_row(&row_values, &payload, projection))
        }).map_err(self.db_error("projection_query_failed"))?;
        let items = rows
            .collect::<Result<Vec<_>, _>>()
            .map_err(self.db_error("projection_query_row_failed"))?;
        Ok(
            json!({"schema":self.schema_id("query.v1"),"status":"ok","result_kind":"entities","compact":compact,"items":items,"offset":offset,"limit":limit,"returned":items.len(),"bounded":true}),
        )
    }

    fn snapshot(&self, root: &Path, args: &Map<String, Value>) -> Result<Value, Value> {
        let feature = &self.domain.features.snapshot;
        let mut snapshot_head = None;
        let mut stable = false;
        for _ in 0..feature.stability_retries {
            let before = self.ledger_head(root)?;
            self.rebuild_projection(root)?;
            let after = self.ledger_head(root)?;
            if before == after {
                snapshot_head = after;
                stable = true;
                break;
            }
        }
        let ledger_head = snapshot_head;
        if !stable {
            return Err(self.error(
                &feature.unstable_refusal_code,
                "The graph changed repeatedly while the query projection was rebuilt.",
                Value::Null,
            ));
        }
        if let Some(expected) = args.get("expected_ledger_head") {
            let expected = expected.as_str();
            if expected != ledger_head.as_deref() {
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
        let entity_count: i64 = db
            .query_row(&format!("select count(*) from {}", self.entity_table), [], |row| row.get(0))
            .map_err(self.db_error("projection_count_failed"))?;
        let relation_count: i64 = db
            .query_row(&format!("select count(*) from {}", self.relation_table), [], |row| row.get(0))
            .map_err(self.db_error("projection_count_failed"))?;

        let mut entity_statement = db
            .prepare(&format!("select entity_id,kind,payload_json,event_id from {} order by entity_id limit ?1 offset ?2", self.entity_table))
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
        let caps = &self.domain.caps.query_batch;
        let queries = args
            .get("queries")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                self.error(
                    "invalid_batch_query",
                    "queries must be an array",
                    Value::Null,
                )
            })?;
        if (queries.len() as u64) < caps.min_queries || (queries.len() as u64) > caps.max_queries {
            return Err(self.error(
                "invalid_batch_query",
                &format!(
                    "queries count must be between {} and {}",
                    caps.min_queries, caps.max_queries
                ),
                json!({"count":queries.len()}),
            ));
        }
        let limit = args
            .get("limit_per_query")
            .and_then(Value::as_u64)
            .unwrap_or(caps.limit_per_query_default)
            .min(caps.limit_per_query_max);
        let mut results = Vec::with_capacity(queries.len());
        for (index, item) in queries.iter().enumerate() {
            let item = item.as_object().ok_or_else(|| {
                self.error(
                    "invalid_batch_query",
                    "each query must be an object",
                    json!({"index":index}),
                )
            })?;
            let mut query_args = item.clone();
            query_args.insert("compact".into(), json!(true));
            query_args.insert("limit".into(), json!(limit));
            query_args.insert("offset".into(), json!(0));
            let result = self.query(root, &query_args)?;
            results.push(json!({
                "index":index,
                "text":item.get("text"),
                "kind":item.get("kind"),
                "record_kind":item.get("record_kind"),
                "returned":result["returned"],
                "items":result["items"]
            }));
        }
        Ok(json!({
            "schema":self.schema_id("query_batch.v1"),
            "status":"ok",
            "query_count":queries.len(),
            "limit_per_query":limit,
            "results":results,
            "bounded":true
        }))
    }

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
        self.rebuild_projection(root)?;
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
                &format!("select payload_json from {} where {}=?1", self.entity_table, entity_pk),
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
        let rows = stmt.query_map(params![id,limit], |r| {
            let payload = serde_json::from_str::<Value>(&r.get::<_,String>(4)?).unwrap_or(Value::Null);
            let mut row_values = Map::new();
            row_values.insert("relation_id".into(), json!(r.get::<_,String>(0)?));
            row_values.insert("relation_type".into(), json!(r.get::<_,String>(1)?));
            row_values.insert("source_id".into(), json!(r.get::<_,String>(2)?));
            row_values.insert("target_id".into(), json!(r.get::<_,String>(3)?));
            Ok(Self::project_row(&row_values, &payload, relation_fields))
        }).map_err(self.db_error("projection_relation_query_failed"))?;
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
        let mut record_stmt = db.prepare(&record_sql).map_err(self.db_error("projection_neighborhood_record_prepare_failed"))?;
        let records = record_stmt.query_map(params![id,limit], |r| {
            let payload = serde_json::from_str::<Value>(&r.get::<_,String>(2)?).unwrap_or(Value::Null);
            let mut row_values = Map::new();
            row_values.insert("record_id".into(), json!(r.get::<_,String>(0)?));
            row_values.insert("record_kind".into(), json!(r.get::<_,String>(1)?));
            row_values.insert("event_id".into(), json!(r.get::<_,String>(3)?));
            Ok(Self::project_row(&row_values, &payload, record_fields))
        }).map_err(self.db_error("projection_neighborhood_record_query_failed"))?.collect::<Result<Vec<_>, _>>().map_err(self.db_error("projection_neighborhood_record_row_failed"))?;
        Ok(
            json!({"schema":self.schema_id("neighborhood.v1"),"status":"ok","entity":serde_json::from_str::<Value>(&entity).unwrap_or(Value::Null),"relations":relations,"records":records,"limit":limit,"bounded":true}),
        )
    }

    fn export(&self, root: &Path, args: &Map<String, Value>) -> Result<Value, Value> {
        let feature = &self.domain.features.export;
        let caps = &self.domain.caps.export;
        let format = args
            .get("format")
            .and_then(Value::as_str)
            .unwrap_or(&feature.default_format);
        let entities = self.query(root, &Map::from_iter([("limit".into(), json!(caps.entities))]))?["items"].clone();
        self.rebuild_projection(root)?;
        let db = Connection::open(self.projection_path(root))
            .map_err(self.db_error("projection_open_failed"))?;
        let mut stmt = db
            .prepare(&format!("select payload_json from {} order by relation_id limit {}", self.relation_table, caps.relations))
            .map_err(self.db_error("projection_export_prepare_failed"))?;
        let relations = stmt
            .query_map([], |r| {
                Ok(serde_json::from_str::<Value>(&r.get::<_, String>(0)?).unwrap_or(Value::Null))
            })
            .map_err(self.db_error("projection_export_failed"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(self.db_error("projection_export_row_failed"))?;
        let mut record_stmt = db
            .prepare(&format!("select payload_json from {} order by record_id limit {}", self.records_table, caps.records))
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
        let ddl = self.domain.projection.ddl.clone();
        let hash_field = self.event_hash_field;
        ledger_projection::rebuild_projection(
            self.error,
            &self.ledger_layout(root),
            hash_field,
            &self.projection_path(root),
            &ddl,
            |tx, event, event_id| {
                for op in event["operations"].as_array().into_iter().flatten() {
                    let op_kind = op["op"].as_str().unwrap_or_default();
                    let Some(fold) = self
                        .domain
                        .projection
                        .fold
                        .iter()
                        .find(|entry| entry.operation == op_kind)
                    else {
                        continue;
                    };
                    let table = self.table(&fold.table);
                    let placeholders = (1..=table.columns.len())
                        .map(|index| format!("?{index}"))
                        .collect::<Vec<_>>()
                        .join(",");
                    let sql = format!(
                        "{} into {} values({})",
                        self.domain.projection.write_mode, table.name, placeholders
                    );
                    let mut values = Vec::with_capacity(table.columns.len());
                    for column in &table.columns {
                        let value = if *column == table.primary_key {
                            op.get(&fold.key_field)
                                .and_then(Value::as_str)
                                .unwrap()
                                .to_string()
                        } else if column == "payload_json" {
                            op.to_string()
                        } else if column == "event_id" {
                            event_id.to_string()
                        } else {
                            let mapping = fold
                                .columns
                                .get(column)
                                .and_then(Value::as_str)
                                .unwrap_or_default();
                            op.get(mapping)
                                .and_then(Value::as_str)
                                .unwrap_or(mapping)
                                .to_string()
                        };
                        values.push(value);
                    }
                    let code = if table.name == self.entity_table {
                        "projection_entity_write_failed"
                    } else if table.name == self.relation_table {
                        "projection_relation_write_failed"
                    } else {
                        "projection_assessment_write_failed"
                    };
                    tx.execute(&sql, rusqlite::params_from_iter(values))
                        .map_err(self.db_error(code))?;
                }
                Ok(())
            },
        )
    }

    #[cfg(test)]
    pub(crate) fn verify_ledger(&self, root: &Path) -> Result<(), Value> {
        event_ledger::verify(self.error, &self.ledger_layout(root), self.event_hash_field)
    }

    fn validate_references(&self, root: &Path, operations: &[Value]) -> Result<(), Value> {
        let mut known = std::collections::HashSet::new();
        if self.projection_path(root).exists() {
            let db = Connection::open(self.projection_path(root))
                .map_err(self.db_error("projection_open_failed"))?;
            let entity_pk = self.table(&self.entity_table).primary_key.clone();
            let mut statement = db
                .prepare(&format!("select {} from {}", entity_pk, self.entity_table))
                .map_err(self.db_error("projection_reference_prepare_failed"))?;
            let rows = statement
                .query_map([], |row| row.get::<_, String>(0))
                .map_err(self.db_error("projection_reference_query_failed"))?;
            for row in rows {
                known.insert(row.map_err(self.db_error("projection_reference_row_failed"))?);
            }
        }
        let entity_key_field = self
            .domain
            .projection
            .fold
            .iter()
            .find(|entry| entry.table == self.entity_table)
            .map(|entry| entry.key_field.clone())
            .unwrap_or_else(|| "entity_id".to_string());
        for operation in operations {
            if operation["op"] == self.entity_op() {
                known.insert(
                    operation
                        .get(&entity_key_field)
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                );
            }
        }
        let require_known = |field: &str, operation: &Value| -> Result<(), Value> {
            let id = operation
                .get(field)
                .and_then(Value::as_str)
                .unwrap_or_default();
            if known.contains(id) {
                Ok(())
            } else {
                Err(self.error(
                    "dangling_reference",
                    "operation references an unknown entity",
                    json!({"field":field,"entity_id":id,"operation":operation}),
                ))
            }
        };
        let evidence_required_fields = self
            .domain
            .operations
            .evidence_entry
            .get("required")
            .and_then(Value::as_array)
            .and_then(|fields| {
                fields
                    .iter()
                    .map(|field| field.as_str().map(str::to_string))
                    .collect::<Option<Vec<_>>>()
            })
            .unwrap_or_default();
        for operation in operations {
            let op_kind = operation["op"].as_str().unwrap_or_default();
            for binding in &self.domain.operations.reference_bindings {
                if binding.operation == "*" {
                    for field in &binding.fields {
                        let Some((array_field, sub_field)) = field.split_once("[].") else {
                            continue;
                        };
                        for entry in operation
                            .get(array_field)
                            .and_then(Value::as_array)
                            .into_iter()
                            .flatten()
                        {
                            require_known(sub_field, entry)?;
                            for required_field in &evidence_required_fields {
                                if required_field == sub_field {
                                    continue;
                                }
                                if entry
                                    .get(required_field)
                                    .and_then(Value::as_str)
                                    .filter(|value| !value.trim().is_empty())
                                    .is_none()
                                {
                                    return Err(self.error(
                                        "evidence_location_incomplete",
                                        "evidence requires locator and paraphrase",
                                        json!({"field":required_field,"evidence":entry}),
                                    ));
                                }
                            }
                        }
                    }
                } else if binding.operation == op_kind {
                    for field in &binding.fields {
                        require_known(field, operation)?;
                    }
                }
            }
        }
        Ok(())
    }

    fn validate_operations(&self, ops: &[Value], require_evidence: bool) -> Result<(), Value> {
        for op in ops {
            let obj = op.as_object().ok_or_else(|| {
                self.error(
                    "invalid_operation",
                    "operation must be an object",
                    Value::Null,
                )
            })?;
            let kind = self.required(obj, "op")?;
            let Some(required_fields) = self.domain.operations.required_fields.get(kind.as_str())
            else {
                return Err(self.error(
                    "invalid_operation",
                    "unsupported operation",
                    json!({"op":kind}),
                ));
            };
            for field in required_fields.clone() {
                if field == "op" {
                    continue;
                }
                let value = self.required(obj, &field)?;
                if field == "kind" && kind == self.entity_op() {
                    let rule = &self.domain.entities.extension_rule;
                    if !self.domain.entities.core_kinds.contains(&value)
                        && !value.contains(&rule.must_contain)
                    {
                        return Err(self.error(
                            &rule.refusal_code,
                            "extension entity kinds must be namespaced",
                            json!({"kind":value,"core_entity_kinds":self.domain.entities.core_kinds,"extension_pattern":rule.pattern,"examples":rule.examples}),
                        ));
                    }
                }
                if field == "relation_type" && kind == self.relation_op() {
                    let rule = &self.domain.relations.extension_rule;
                    if !self.domain.relations.core.contains(&value)
                        && !value.contains(&rule.must_contain)
                    {
                        return Err(self.error(
                            &rule.refusal_code,
                            "extension relations must be namespaced",
                            json!({
                                "relation_type":value,
                                "core_relations":self.domain.relations.core,
                                "extension_pattern":rule.pattern,
                                "examples":rule.examples
                            }),
                        ));
                    }
                }
            }
            if kind == self.entity_op() {
                for conditional in &self.domain.entities.required_fields.conditional {
                    if obj.get("kind").and_then(Value::as_str)
                        == Some(conditional.when_kind.as_str())
                    {
                        for field in &conditional.requires {
                            self.required(obj, field)?;
                        }
                    }
                }
            }
            if require_evidence
                && self
                    .domain
                    .operations
                    .evidence_required_at_review
                    .contains(&kind)
                && obj
                    .get("evidence")
                    .and_then(Value::as_array)
                    .map(|value| value.is_empty())
                    .unwrap_or(true)
            {
                return Err(self.error(
                    "evidence_required",
                    "assessment and outcome records require evidence",
                    json!({"op":kind}),
                ));
            }
        }
        Ok(())
    }

    fn with_authority_lock<T>(
        &self,
        root: &Path,
        key: &str,
        action: impl FnOnce() -> Result<T, Value>,
    ) -> Result<T, Value> {
        lock::with_authority_lock(
            self.error,
            &self.runtime(root).join("locks"),
            key,
            lock::AuthorityLockPolicy::default(),
            action,
        )
    }

    fn validated_sequence_name(&self, args: &Map<String, Value>) -> Result<String, Value> {
        let name = self.required(args, "sequence_name")?;
        if name.trim() != name
            || name.chars().count() as u64 > self.domain.caps.sequence_name_chars.max
            || name.chars().any(char::is_control)
        {
            return Err(self.error(
                "sequence_name_invalid",
                "sequence_name must be 1-120 non-control characters without surrounding whitespace",
                json!({"sequence_name":name}),
            ));
        }
        Ok(name)
    }

    fn required_object(&self, args: &Map<String, Value>, key: &str) -> Result<Value, Value> {
        ledger_args::required_object(
            self.error,
            args,
            key,
            self.domain.caps.authority_basis_bytes,
            "authority_basis",
        )
    }

    fn optional_u64(
        &self,
        args: &Map<String, Value>,
        key: &str,
        default: u64,
    ) -> Result<u64, Value> {
        ledger_args::optional_u64(self.error, args, key, default)
    }

    fn page_limit(&self, args: &Map<String, Value>) -> Result<usize, Value> {
        ledger_args::page_limit(self.error, args)
    }

    fn page_offset(&self, args: &Map<String, Value>) -> Result<usize, Value> {
        ledger_args::page_offset(self.error, args)
    }

    fn sequence_directory(&self, root: &Path, name: &str) -> PathBuf {
        self.sequences(root).join(sha256(name.as_bytes()))
    }

    fn sequence_claims_directory(&self, root: &Path, name: &str) -> PathBuf {
        self.sequence_directory(root, name).join("claims")
    }

    fn load_sequence_manifest(&self, root: &Path, name: &str) -> Result<Value, Value> {
        let path = self.sequence_directory(root, name).join("sequence.json");
        if !path.exists() {
            return Err(self.error(
                "sequence_not_found",
                "sequence does not exist",
                json!({"sequence_name":name}),
            ));
        }
        let manifest = self.read_json(&path)?;
        self.verify_sequence_manifest(&manifest, name)?;
        Ok(manifest)
    }

    fn verify_sequence_manifest(&self, manifest: &Value, expected_name: &str) -> Result<(), Value> {
        let sequences = &self.domain.features.sequences;
        let expected_id = self.generated_sequence_id(expected_name);
        if manifest.get("schema") != Some(&json!(sequences.manifest_schema_id))
            || manifest.get("sequence_name").and_then(Value::as_str) != Some(expected_name)
            || manifest.get("sequence_id").and_then(Value::as_str) != Some(expected_id.as_str())
            || manifest
                .get("start_at")
                .and_then(Value::as_u64)
                .is_none_or(|value| value < sequences.start_at_min)
            || manifest.get("step").and_then(Value::as_u64) != Some(sequences.step)
        {
            return Err(self.error(
                "sequence_manifest_invalid",
                "sequence manifest has invalid identity or configuration",
                json!({"sequence_name":expected_name}),
            ));
        }
        let hash_field = sequences.manifest_hash_field.clone();
        let Some(recomputed) = chain::recompute_hash(self.error, manifest, &hash_field)? else {
            return Err(self.error(
                "sequence_manifest_invalid",
                "sequence manifest lacks creation_hash",
                json!({"sequence_name":expected_name}),
            ));
        };
        if recomputed.stored != recomputed.computed {
            return Err(self.error(
                "sequence_manifest_hash_invalid",
                "sequence manifest hash does not match",
                json!({"sequence_name":expected_name,"expected_hash":recomputed.computed,"actual_hash":recomputed.stored}),
            ));
        }
        Ok(())
    }

    fn verified_sequence_claims(
        &self,
        root: &Path,
        name: &str,
        manifest: &Value,
    ) -> Result<Vec<Value>, Value> {
        let sequences = &self.domain.features.sequences;
        let directory = self.sequence_claims_directory(root, name);
        if !directory.exists() {
            return Ok(Vec::new());
        }
        let mut paths = fs::read_dir(&directory)
            .map_err(self.io_error("sequence_claim_store_read_failed"))?
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.extension().and_then(|value| value.to_str()) == Some("json"))
            .collect::<Vec<_>>();
        paths.sort();
        let total = paths.len();
        let mut claims = Vec::with_capacity(total);
        let mut expected_value = manifest["start_at"].as_u64().unwrap();
        let mut previous_hash: Option<String> = None;
        let mut idempotency_keys = HashSet::new();
        let mut claim_ids = HashSet::new();
        for (index, path) in paths.into_iter().enumerate() {
            let claim = self.read_json(&path)?;
            let hash_field = sequences.claim_hash_field.clone();
            let Some(chain::RecomputedHash {
                stored: actual_hash,
                computed: computed_hash,
            }) = chain::recompute_hash(self.error, &claim, &hash_field)?
            else {
                return Err(self.error(
                    "sequence_claim_invalid",
                    "sequence claim lacks claim_hash",
                    json!({"path":path.to_string_lossy()}),
                ));
            };
            let idempotency_key = claim.get("idempotency_key").and_then(Value::as_str);
            let claim_id = claim.get("claim_id").and_then(Value::as_str);
            if claim.get("schema") != Some(&json!(sequences.claim_schema_id))
                || claim.get("sequence_name").and_then(Value::as_str) != Some(name)
                || claim.get("sequence_id") != manifest.get("sequence_id")
                || claim.get("value").and_then(Value::as_u64) != Some(expected_value)
                || claim
                    .get(&sequences.claim_chain_field)
                    .and_then(Value::as_str)
                    != previous_hash.as_deref()
                || claim
                    .get("request_digest")
                    .and_then(Value::as_str)
                    .is_none()
                || idempotency_key.is_none_or(str::is_empty)
                || claim_id.is_none_or(str::is_empty)
                || !idempotency_keys.insert(idempotency_key.unwrap().to_string())
                || !claim_ids.insert(claim_id.unwrap().to_string())
                || actual_hash != computed_hash
            {
                return Err(self.error(
                    "sequence_claim_chain_invalid",
                    "sequence claim chain is not contiguous and hash-valid",
                    json!({"sequence_name":name,"path":path.to_string_lossy(),"expected_value":expected_value}),
                ));
            }
            previous_hash = Some(actual_hash.to_string());
            claims.push(claim);
            if index + 1 < total {
                expected_value = expected_value.checked_add(1).ok_or_else(|| {
                    self.error(
                        "sequence_claim_chain_invalid",
                        "claim exists after u64 exhaustion",
                        json!({"sequence_name":name}),
                    )
                })?;
            }
        }
        Ok(claims)
    }

    fn find_sequence_claim_by_idempotency<'a>(
        claims: &'a [Value],
        key: &str,
    ) -> Option<&'a Value> {
        claims
            .iter()
            .find(|claim| claim.get("idempotency_key").and_then(Value::as_str) == Some(key))
    }

    fn recover_sequence_idempotency_index(
        &self,
        root: &Path,
        name: &str,
        key: &str,
        claim: &Value,
    ) -> Result<(), Value> {
        let directory = self.sequence_directory(root, name).join("idempotency");
        fs::create_dir_all(&directory)
            .map_err(self.io_error("sequence_idempotency_store_create_failed"))?;
        let path = directory.join(format!("{}.json", sha256(key.as_bytes())));
        if path.exists() {
            let existing = self.read_json(&path)?;
            if existing.get("claim_id") != claim.get("claim_id") {
                return Err(self.error(
                    "sequence_claim_idempotency_conflict",
                    "idempotency index names a different claim",
                    json!({"sequence_name":name,"idempotency_key":key,"existing_claim_id":existing.get("claim_id"),"claim_id":claim.get("claim_id")}),
                ));
            }
            return Ok(());
        }
        self.write_new_json(
            &path,
            &json!({"schema":self.domain.features.sequences.idempotency_schema_id,"idempotency_key":key,"claim_id":claim["claim_id"],"value":claim["value"]}),
        )
    }

    fn find_ledger_event_by_idempotency(
        &self,
        root: &Path,
        key: &str,
    ) -> Result<Option<Value>, Value> {
        event_ledger::find_event_by_idempotency(self.error, &self.ledger_layout(root), key)
    }

    fn prepare(&self, root: &Path) -> Result<(), Value> {
        fs::create_dir_all(self.ledger(root)).map_err(self.io_error("ledger_create_failed"))?;
        fs::create_dir_all(self.proposals(root))
            .map_err(self.io_error("proposal_store_create_failed"))?;
        fs::create_dir_all(self.runtime(root))
            .map_err(self.io_error("projection_root_create_failed"))?;
        Ok(())
    }

    /// Site control root: the site root itself when its basename is
    /// `.narada`, otherwise `<site_root>/.narada` (engine constant).
    fn control(&self, root: &Path) -> PathBuf {
        if root.file_name().and_then(|value| value.to_str()) == Some(".narada") {
            root.to_path_buf()
        } else {
            root.join(".narada")
        }
    }

    // Storage subdirs join as one '/'-separated segment so rendered paths stay
    // byte-identical to the reference implementations on every platform.
    fn ledger(&self, root: &Path) -> PathBuf {
        self.control(root).join(format!(
            "{}/{}",
            self.domain.storage.control_root_subdir, self.domain.storage.subdirs.ledger
        ))
    }

    fn proposals(&self, root: &Path) -> PathBuf {
        self.control(root).join(format!(
            "{}/{}",
            self.domain.storage.control_root_subdir, self.domain.storage.subdirs.proposals
        ))
    }

    fn sequences(&self, root: &Path) -> PathBuf {
        self.control(root).join(format!(
            "{}/{}",
            self.domain.storage.control_root_subdir, self.domain.storage.subdirs.sequences
        ))
    }

    fn runtime(&self, root: &Path) -> PathBuf {
        self.control(root).join(&self.domain.storage.runtime_subdir)
    }

    fn projection_path(&self, root: &Path) -> PathBuf {
        self.runtime(root).join("projection.sqlite")
    }

    fn ledger_layout(&self, root: &Path) -> LedgerLayout {
        LedgerLayout::new(self.ledger(root), &self.domain.storage.ledger_file_prefix)
    }

    fn ledger_files(&self, root: &Path) -> Result<Vec<PathBuf>, Value> {
        event_ledger::files(self.error, &self.ledger_layout(root))
    }

    fn ledger_head(&self, root: &Path) -> Result<Option<String>, Value> {
        event_ledger::head(
            self.error,
            &self.ledger_layout(root),
            &self.domain.storage.event_hash_field,
        )
    }

    fn load_proposal(&self, root: &Path, id: &str) -> Result<Value, Value> {
        self.read_json(
            &self
                .proposals(root)
                .join(format!("{}.json", safe_name(id))),
        )
    }

    fn read_json(&self, path: &Path) -> Result<Value, Value> {
        ledger_io::read_json(self.error, path)
    }

    fn write_new_json(&self, path: &Path, value: &Value) -> Result<(), Value> {
        ledger_io::write_new_json(self.error, path, value)
    }

    fn write_replace_json(&self, path: &Path, value: &Value) -> Result<(), Value> {
        ledger_io::write_replace_json(self.error, path, value)
    }

    fn write_new(&self, path: &Path, bytes: &[u8]) -> Result<(), Value> {
        ledger_io::write_new(self.error, path, bytes)
    }

    fn digest_value(&self, value: &Value) -> Result<String, Value> {
        narada_mcp_event_ledger::digest::digest_value(self.error, value)
    }

    fn required(&self, args: &Map<String, Value>, key: &str) -> Result<String, Value> {
        ledger_args::required(self.error, args, key)
    }

    fn generated_sequence_id(&self, name: &str) -> String {
        let template = &self.domain.id_derivation.generated_ids.sequence_id;
        format!(
            "{}{}",
            template_prefix(template),
            &sha256(name.as_bytes())[..template_truncation(template, 24)]
        )
    }

    fn generated_claim_id(&self, name: &str, idempotency_key: &str) -> String {
        let template = &self.domain.id_derivation.generated_ids.claim_id;
        format!(
            "{}{}",
            template_prefix(template),
            &sha256(format!("{name}\0{idempotency_key}").as_bytes())
                [..template_truncation(template, 24)]
        )
    }

    /// Render one claim file name from the descriptor's
    /// `claim_file_pattern` (for example `claims/claim-{value:020}.json`).
    /// Only the file-name portion is returned; the caller joins the claims
    /// directory.
    fn sequence_claim_file_name(&self, value: u64) -> String {
        let pattern = &self.domain.features.sequences.claim_file_pattern;
        let Some((left, right)) = pattern.split_once("{value:") else {
            return format!("claim-{value:020}.json");
        };
        let prefix = left.rsplit('/').next().unwrap_or(left);
        let Some((width_text, suffix)) = right.split_once('}') else {
            return format!("claim-{value:020}.json");
        };
        let Ok(width) = width_text.parse::<usize>() else {
            return format!("claim-{value:020}.json");
        };
        format!("{prefix}{value:0width$}{suffix}")
    }

    fn guidance(&self) -> Value {
        let mut object = Map::new();
        for key in &self.domain.guidance.emission_order {
            let value = match key.as_str() {
                "schema" => json!(self.domain.guidance.schema_id),
                "entity_kinds" => json!(self.domain.entities.core_kinds),
                "core_relations" => json!(self.domain.relations.core),
                "operation_kinds" => json!(self.domain.operations.kinds),
                "extension_relation_rule" | "extension_entity_kind_rule" => self
                    .domain
                    .guidance
                    .engine_derived_fields
                    .get(key)
                    .and_then(|entry| entry.get("text"))
                    .cloned()
                    .unwrap_or(Value::Null),
                _ => self
                    .domain
                    .guidance
                    .fields
                    .get(key)
                    .cloned()
                    .unwrap_or(Value::Null),
            };
            object.insert(key.clone(), value);
        }
        Value::Object(object)
    }

    fn guidance_with_request(&self, args: &Map<String, Value>) -> Value {
        let mut value = self.guidance();
        value["requested"] = json!({"workflow":args.get("workflow").cloned().unwrap_or(Value::Null),"tool":args.get("tool").cloned().unwrap_or(Value::Null)});
        value
    }

    fn error(&self, code: &str, message: &str, details: Value) -> Value {
        self.error.error(code, message, details)
    }

    fn io_error(&self, code: &'static str) -> impl FnOnce(std::io::Error) -> Value {
        self.error.io_error(code)
    }

    fn db_error(&self, code: &'static str) -> impl FnOnce(rusqlite::Error) -> Value {
        self.error.db_error(code)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn descriptor_path() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../shared/ledger-domain-epistemic/domain.json")
    }

    fn engine() -> Engine {
        Engine::new(Descriptor::load(&descriptor_path()).expect("epistemic descriptor"))
            .expect("engine")
    }

    #[test]
    fn storage_layout_matches_the_epistemic_control_root_convention() {
        let engine = engine();
        let root = Path::new("site");
        assert_eq!(
            engine.ledger(root),
            Path::new("site").join(".narada/epistemic/ledger")
        );
        assert_eq!(
            engine.proposals(root),
            Path::new("site").join(".narada/epistemic/proposals")
        );
        assert_eq!(
            engine.sequences(root),
            Path::new("site").join(".narada/epistemic/sequences")
        );
        assert_eq!(
            engine.runtime(root),
            Path::new("site").join(".narada/.ai/epistemic-graph")
        );
        assert_eq!(
            engine.projection_path(root),
            Path::new("site").join(".narada/.ai/epistemic-graph/projection.sqlite")
        );
        let narada = Path::new("site/.narada");
        assert_eq!(engine.ledger(narada), narada.join("epistemic/ledger"));
        assert_eq!(engine.runtime(narada), narada.join(".ai/epistemic-graph"));
    }

    #[test]
    fn proposal_tool_schema_describes_every_operation_shape() {
        let engine = engine();
        let schema = &engine
            .domain
            .tools
            .iter()
            .find(|tool| tool.name == "epistemic_graph_proposal_submit")
            .expect("proposal tool")
            .input_schema;
        let variants = schema
            .pointer("/properties/operations/items/oneOf")
            .and_then(Value::as_array)
            .expect("operation variants");
        assert_eq!(variants.len(), 5);
        assert_eq!(
            variants[0].pointer("/properties/op/const"),
            Some(&json!("entity.declare"))
        );
        assert_eq!(
            variants[1].pointer("/properties/op/const"),
            Some(&json!("relation.declare"))
        );
        assert_eq!(
            variants[2].pointer("/properties/evidence/items/required/2"),
            Some(&json!("paraphrase"))
        );
    }

    #[test]
    fn guidance_contains_copyable_end_to_end_workflow() {
        let engine = engine();
        let value = engine.guidance();
        assert_eq!(value["schema"], "narada.epistemic.guidance.v2");
        assert_eq!(
            value.pointer("/minimal_example/tool"),
            Some(&json!("epistemic_graph_submit_review_admit"))
        );
        assert_eq!(
            value.pointer("/minimal_example/arguments/operations/0/op"),
            Some(&json!("entity.declare"))
        );
        assert_eq!(
            value.pointer("/minimal_example/arguments/operations/2/op"),
            Some(&json!("relation.declare"))
        );
        assert!(value["concurrency_rule"]
            .as_str()
            .unwrap_or_default()
            .contains("ledger_head"));
    }

    #[test]
    fn guidance_schema_accepts_declared_routing_hints() {
        let engine = engine();
        let tool = engine
            .list_tools()
            .into_iter()
            .find(|tool| tool["name"] == "epistemic_graph_guidance")
            .unwrap();
        assert_eq!(tool["inputSchema"]["additionalProperties"], false);
        assert_eq!(
            tool["inputSchema"]["properties"]["workflow"]["type"],
            "string"
        );
        let value = engine.guidance_with_request(
            json!({"workflow":"query_current_frontier"})
                .as_object()
                .unwrap(),
        );
        assert_eq!(value["requested"]["workflow"], "query_current_frontier");
    }

    #[test]
    fn disabled_feature_tools_are_hidden_and_refused() {
        let mut value: Value = serde_json::from_str(
            &fs::read_to_string(descriptor_path()).expect("descriptor text"),
        )
        .expect("descriptor json");
        value["features"]["source_inspect"]["enabled"] = json!(false);
        let engine = Engine::new(Descriptor::from_value(value).expect("descriptor")).expect("engine");
        assert!(!engine
            .list_tools()
            .iter()
            .any(|tool| tool["name"] == "epistemic_graph_source_inspect"));
        let failure = engine
            .call_tool(
                "epistemic_graph_source_inspect",
                &Map::new(),
                Path::new("."),
            )
            .expect_err("disabled feature refuses");
        assert_eq!(failure["code"], "unknown_tool");
        assert_eq!(
            failure["message"],
            "unknown_tool:epistemic_graph_source_inspect"
        );
    }

    #[test]
    fn source_entity_requires_a_version_and_locator() {
        let engine = engine();
        let operation = json!({"op":"entity.declare","entity_id":"source:unlocated","kind":"source","title":"Unlocated source","version":"1"});
        let failure = engine
            .validate_operations(&[operation], false)
            .expect_err("unlocated source must refuse");
        assert_eq!(failure["code"], "required_argument_missing");
        assert_eq!(failure["details"]["field"], "locator");
    }

    #[test]
    fn extension_entity_kinds_must_be_namespaced() {
        let engine = engine();
        let extension = json!({"op":"entity.declare","entity_id":"exp:demo","kind":"cintamani:experiment","title":"Demo experiment","version":"1","payload":{"intent":"falsification"}});
        engine
            .validate_operations(&[extension], false)
            .expect("namespaced extension kind must validate");
        let bare = json!({"op":"entity.declare","entity_id":"exp:demo","kind":"experiment","title":"Demo experiment"});
        let failure = engine
            .validate_operations(&[bare], false)
            .expect_err("unnamespaced extension kind must refuse");
        assert_eq!(failure["code"], "invalid_entity_kind");
        assert_eq!(failure["details"]["kind"], "experiment");
    }

    #[test]
    fn source_inspection_returns_all_relevant_sections_with_line_ranges() {
        let engine = engine();
        let root = std::env::temp_dir().join(format!("epistemic-source-test-{}", Uuid::new_v4()));
        fs::create_dir_all(root.join("ledger")).expect("ledger directory");
        fs::write(
            root.join("ledger/example.md"),
            "# Example\n\n## Record\nA\n\n## Decision\nB\n\n## Subsequent Update\nC\n",
        )
        .expect("source");
        let result = engine
            .source_inspect(
                &root,
                &Map::from_iter([("paths".into(), json!(["ledger/example.md"]))]),
            )
            .expect("inspection");
        assert_eq!(result["files"][0]["title"], "Example");
        assert_eq!(result["files"][0]["section_count"], 3);
        assert_eq!(result["files"][0]["sections"][1]["start_line"], 6);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn batch_query_and_resubmission_are_bounded_and_identity_driven() {
        let engine = engine();
        let root = std::env::temp_dir().join(format!("epistemic-batch-test-{}", Uuid::new_v4()));
        let proposal = engine
            .proposal_submit(
                &root,
                &Map::from_iter([
                    ("actor".into(), json!("tester")),
                    ("authority_basis".into(), json!({"kind":"test"})),
                    ("idempotency_key".into(), json!("batch-p1")),
                    ("expected_ledger_head".into(), Value::Null),
                    ("operations".into(), json!([
                        {"op":"entity.declare","entity_id":"claim:keep","kind":"claim","title":"Keep alpha"},
                        {"op":"entity.declare","entity_id":"claim:drop","kind":"claim","title":"Drop beta"}
                    ])),
                ]),
            )
            .expect("proposal");
        let resubmitted = engine
            .proposal_resubmit(
                &root,
                &Map::from_iter([
                    ("source_proposal_id".into(), proposal["proposal_id"].clone()),
                    ("actor".into(), json!("tester")),
                    ("authority_basis".into(), json!({"kind":"test"})),
                    ("idempotency_key".into(), json!("batch-p2")),
                    ("expected_ledger_head".into(), Value::Null),
                    ("drop_operation_ids".into(), json!(["entity:claim:drop"])),
                    ("replacements".into(), json!([
                        {"op":"entity.declare","entity_id":"claim:replacement","kind":"claim","title":"Replacement beta"}
                    ])),
                ]),
            )
            .expect("resubmit");
        assert_eq!(resubmitted["operation_count"], 2);
        let page = engine
            .proposal_read(
                &root,
                &Map::from_iter([("proposal_id".into(), resubmitted["proposal_id"].clone())]),
            )
            .expect("read resubmission");
        assert_eq!(page["operations"][0]["entity_id"], "claim:keep");
        assert_eq!(page["operations"][1]["entity_id"], "claim:replacement");

        engine
            .proposal_admit(
                &root,
                &Map::from_iter([
                    ("proposal_id".into(), resubmitted["proposal_id"].clone()),
                    ("actor".into(), json!("tester")),
                    ("authority_basis".into(), json!({"kind":"test"})),
                    ("expected_ledger_head".into(), Value::Null),
                    ("idempotency_key".into(), json!("batch-a1")),
                ]),
            )
            .expect("admit");
        let result = engine
            .query_batch(
                &root,
                &Map::from_iter([
                    (
                        "queries".into(),
                        json!([{"text":"alpha"},{"text":"replacement"}]),
                    ),
                    ("limit_per_query".into(), json!(1)),
                ]),
            )
            .expect("batch query");
        assert_eq!(result["query_count"], 2);
        assert_eq!(result["results"][0]["returned"], 1);
        assert_eq!(result["results"][1]["returned"], 1);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn admitted_assessments_are_queryable_in_neighborhood_status_and_export() {
        let engine = engine();
        let root = std::env::temp_dir().join(format!("epistemic-record-test-{}", Uuid::new_v4()));
        let operations = json!([
            {"op":"entity.declare","entity_id":"source:record-test","kind":"source","title":"Record test source","version":"1","locator":"ledger/test.md"},
            {"op":"entity.declare","entity_id":"test:record-test","kind":"test","title":"Record test"},
            {"op":"assessment.record","assessment_id":"assessment:record-test","subject_id":"test:record-test","judgment":"conditional","actor":"tester","reason":"Some gates remain open.","evidence":[{"source_id":"source:record-test","locator":"Current status","paraphrase":"The source reports a conditional result."}]}
        ]);
        let proposal = engine
            .proposal_submit(
                &root,
                &Map::from_iter([
                    ("actor".into(), json!("tester")),
                    ("authority_basis".into(), json!({"kind":"test"})),
                    ("idempotency_key".into(), json!("record-p1")),
                    ("expected_ledger_head".into(), Value::Null),
                    ("operations".into(), operations),
                ]),
            )
            .expect("proposal");
        engine
            .proposal_admit(
                &root,
                &Map::from_iter([
                    ("proposal_id".into(), proposal["proposal_id"].clone()),
                    ("actor".into(), json!("tester")),
                    ("authority_basis".into(), json!({"kind":"test"})),
                    ("expected_ledger_head".into(), Value::Null),
                    ("idempotency_key".into(), json!("record-a1")),
                ]),
            )
            .expect("admit");
        let records = engine
            .query(
                &root,
                &Map::from_iter([("record_kind".into(), json!("assessment.record"))]),
            )
            .expect("record query");
        assert_eq!(records["returned"], 1);
        assert_eq!(engine.status(&root).expect("status")["record_count"], 1);
        assert_eq!(
            engine
                .neighborhood(
                    &root,
                    &Map::from_iter([("entity_id".into(), json!("test:record-test"))])
                )
                .expect("neighborhood")["records"]
                .as_array()
                .map(Vec::len),
            Some(1)
        );
        assert_eq!(
            engine.export(&root, &Map::new()).expect("export")["records"]
                .as_array()
                .map(Vec::len),
            Some(1)
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn engine_written_ledger_verifies_through_the_shared_crate() {
        let engine = engine();
        let root = std::env::temp_dir().join(format!("epistemic-shared-verify-{}", Uuid::new_v4()));
        let proposal = engine
            .proposal_submit(
                &root,
                &Map::from_iter([
                    ("actor".into(), json!("tester")),
                    ("authority_basis".into(), json!({"kind":"test"})),
                    ("idempotency_key".into(), json!("shared-p1")),
                    ("expected_ledger_head".into(), Value::Null),
                    ("operations".into(), json!([
                        {"op":"entity.declare","entity_id":"claim:shared","kind":"claim","title":"Shared verify claim"}
                    ])),
                ]),
            )
            .expect("proposal");
        engine
            .proposal_admit(
                &root,
                &Map::from_iter([
                    ("proposal_id".into(), proposal["proposal_id"].clone()),
                    ("actor".into(), json!("tester")),
                    ("authority_basis".into(), json!({"kind":"test"})),
                    ("expected_ledger_head".into(), Value::Null),
                    ("idempotency_key".into(), json!("shared-a1")),
                ]),
            )
            .expect("admit");
        narada_mcp_event_ledger::ledger::verify(
            narada_mcp_event_ledger::ErrorSchema("narada.epistemic.error.v1"),
            &narada_mcp_event_ledger::ledger::LedgerLayout::new(
                root.join(".narada/epistemic/ledger"),
                "ev",
            ),
            "event_hash",
        )
        .expect("shared crate verifies the engine-written ledger");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn graph_snapshot_pages_nodes_and_edges_under_one_ledger_head() {
        let engine = engine();
        let root = std::env::temp_dir().join(format!("epistemic-snapshot-test-{}", Uuid::new_v4()));
        let proposal = engine
            .proposal_submit(
                &root,
                &Map::from_iter([
                    ("actor".into(), json!("tester")),
                    ("authority_basis".into(), json!({"kind":"test"})),
                    ("idempotency_key".into(), json!("snapshot-p1")),
                    ("expected_ledger_head".into(), Value::Null),
                    ("operations".into(), json!([
                        {"op":"entity.declare","entity_id":"problem:snapshot","kind":"problem","title":"Snapshot problem"},
                        {"op":"entity.declare","entity_id":"claim:snapshot","kind":"claim","title":"Snapshot claim"},
                        {"op":"relation.declare","relation_id":"relation:snapshot","relation_type":"addresses","source_id":"claim:snapshot","target_id":"problem:snapshot"}
                    ])),
                ]),
            )
            .expect("proposal");
        engine
            .proposal_admit(
                &root,
                &Map::from_iter([
                    ("proposal_id".into(), proposal["proposal_id"].clone()),
                    ("actor".into(), json!("tester")),
                    ("authority_basis".into(), json!({"kind":"test"})),
                    ("expected_ledger_head".into(), Value::Null),
                    ("idempotency_key".into(), json!("snapshot-a1")),
                ]),
            )
            .expect("admit");

        let first = engine
            .snapshot(&root, &Map::from_iter([("limit".into(), json!(1))]))
            .expect("first page");
        assert_eq!(first["entity_count"], 2);
        assert_eq!(first["relation_count"], 1);
        assert_eq!(first["entities"].as_array().map(Vec::len), Some(1));
        assert_eq!(first["relations"].as_array().map(Vec::len), Some(1));
        assert_eq!(first["next_entity_offset"], 1);
        assert!(first["next_relation_offset"].is_null());

        let second = engine
            .snapshot(
                &root,
                &Map::from_iter([
                    ("limit".into(), json!(1)),
                    ("entity_offset".into(), json!(1)),
                    ("relation_offset".into(), json!(1)),
                    ("expected_ledger_head".into(), first["ledger_head"].clone()),
                ]),
            )
            .expect("second page");
        assert_eq!(second["entities"].as_array().map(Vec::len), Some(1));
        assert!(second["next_entity_offset"].is_null());
        assert!(second["relations"].as_array().is_some_and(Vec::is_empty));

        let mismatch = engine
            .snapshot(
                &root,
                &Map::from_iter([("expected_ledger_head".into(), json!("sha256:stale"))]),
            )
            .expect_err("stale snapshot");
        assert_eq!(mismatch["code"], "ledger_head_mismatch");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn proposal_submission_is_compact_and_explicit_reads_are_bounded() {
        let engine = engine();
        let root =
            std::env::temp_dir().join(format!("epistemic-proposal-read-test-{}", Uuid::new_v4()));
        let operations = (0..engine.max_operations())
            .map(|index| json!({"op":"entity.declare","entity_id":format!("claim:{index}"),"kind":"claim","title":format!("Claim {index}")}))
            .collect::<Vec<_>>();
        let receipt = engine
            .proposal_submit(
                &root,
                &Map::from_iter([
                    ("actor".into(), json!("tester")),
                    ("authority_basis".into(), json!({"kind":"test"})),
                    ("idempotency_key".into(), json!("compact-p1")),
                    ("expected_ledger_head".into(), Value::Null),
                    ("operations".into(), json!(operations)),
                ]),
            )
            .expect("proposal");
        assert_eq!(receipt["operation_count"], engine.max_operations());
        assert!(receipt.get("operations").is_none());
        assert!(
            serde_json::to_vec(&receipt)
                .expect("serialize receipt")
                .len()
                < 1024
        );

        let first = engine
            .proposal_read(
                &root,
                &Map::from_iter([
                    ("proposal_id".into(), receipt["proposal_id"].clone()),
                    ("limit".into(), json!(7)),
                ]),
            )
            .expect("first page");
        assert_eq!(first["returned"], 7);
        assert_eq!(first["offset"], 0);
        assert_eq!(first["next_offset"], 7);
        assert_eq!(first["bounded"], true);

        let final_page = engine
            .proposal_read(
                &root,
                &Map::from_iter([
                    ("proposal_id".into(), receipt["proposal_id"].clone()),
                    ("offset".into(), json!(195)),
                    ("limit".into(), json!(100)),
                ]),
            )
            .expect("final page");
        assert_eq!(final_page["returned"], 5);
        assert_eq!(final_page["has_more"], false);
        assert!(final_page["next_offset"].is_null());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn proposal_admission_rebuilds_projection_and_preserves_truth_boundary() {
        let engine = engine();
        let root = std::env::temp_dir().join(format!("epistemic-test-{}", Uuid::new_v4()));
        let proposal=engine.proposal_submit(&root,&Map::from_iter([("actor".into(),json!("nima")),("authority_basis".into(),json!({"kind":"operator_request"})),("operations".into(),json!([{"op":"entity.declare","entity_id":"problem-1","kind":"problem","title":"What explains X?"}]))])).unwrap();
        assert_eq!(
            proposal["schema"],
            "narada.epistemic.proposal_submission.v1"
        );
        assert_eq!(proposal["operation_count"], 1);
        assert!(proposal.get("operations").is_none());
        let id = proposal["proposal_id"].as_str().unwrap();
        let event = engine
            .proposal_admit(
                &root,
                &Map::from_iter([
                    ("proposal_id".into(), json!(id)),
                    ("actor".into(), json!("nima")),
                    ("authority_basis".into(), json!({"kind":"operator_request"})),
                ]),
            )
            .unwrap();
        assert_eq!(event["schema"], "narada.epistemic.proposal_admission.v1");
        assert_eq!(event["status"], "admitted");
        assert_eq!(event["operation_count"], 1);
        assert!(event.get("operations").is_none());
        assert_eq!(event["ledger_head"].as_str().map(str::len), Some(64));
        assert_eq!(event["certifies_truth"], false);
        let retry = engine
            .proposal_admit(
                &root,
                &Map::from_iter([
                    ("proposal_id".into(), json!(id)),
                    ("actor".into(), json!("nima")),
                    ("authority_basis".into(), json!({"kind":"operator_request"})),
                ]),
            )
            .expect("deterministic admission retry");
        assert_eq!(retry["event_id"], event["event_id"]);
        let admitted = engine
            .proposal_read(&root, &Map::from_iter([("proposal_id".into(), json!(id))]))
            .expect("admitted proposal readback");
        assert_eq!(admitted["status"], "admitted");
        assert_eq!(admitted["lifecycle"]["event_id"], event["event_id"]);
        assert_eq!(admitted["lifecycle"]["ledger_head"], event["ledger_head"]);
        let result = engine.query(&root, &Map::new()).unwrap();
        assert_eq!(result["returned"], 1);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn source_capture_builds_a_compact_deduplicated_draft_without_admitting_it() {
        let engine = engine();
        let root = std::env::temp_dir().join(format!("epistemic-capture-test-{}", Uuid::new_v4()));
        let seed = engine.proposal_submit(
            &root,
            &Map::from_iter([
                ("actor".into(), json!("tester")),
                ("authority_basis".into(), json!({"kind":"test"})),
                ("idempotency_key".into(), json!("seed-p1")),
                ("expected_ledger_head".into(), Value::Null),
                ("operations".into(), json!([{"op":"entity.declare","entity_id":"claim:existing","kind":"claim","title":"Existing claim"}])),
            ]),
        ).expect("seed proposal");
        let seed_event = engine
            .proposal_admit(
                &root,
                &Map::from_iter([
                    ("proposal_id".into(), seed["proposal_id"].clone()),
                    ("actor".into(), json!("tester")),
                    ("authority_basis".into(), json!({"kind":"test"})),
                    ("expected_ledger_head".into(), Value::Null),
                    ("idempotency_key".into(), json!("seed-a1")),
                ]),
            )
            .expect("seed admission");
        let capture = engine.capture_sources(
            &root,
            &Map::from_iter([
                ("actor".into(), json!("tester")),
                ("authority_basis".into(), json!({"kind":"test"})),
                ("idempotency_key".into(), json!("capture-p1")),
                ("expected_ledger_head".into(), seed_event["ledger_head"].clone()),
                ("sources".into(), json!([{"source_id":"source:ledger-1","title":"Ledger one","version":"1","locator":"src/ledger/1.md"}])),
                ("operations".into(), json!([
                    {"op":"entity.declare","entity_id":"claim:existing","kind":"claim","title":"Existing claim"},
                    {"op":"relation.declare","relation_id":"rel:existing-source","relation_type":"derived_from","source_id":"claim:existing","target_id":"source:ledger-1"}
                ])),
            ]),
        ).expect("source capture");
        assert_eq!(capture["status"], "draft_submitted");
        assert_eq!(capture["source_count"], 1);
        assert_eq!(capture["operation_count"], 3);
        assert_eq!(capture["existing_identity_count"], 1);
        assert_eq!(
            capture["existing_identities"][0]["identity"],
            "claim:existing"
        );
        assert_eq!(capture["admission_requires_explicit_call"], true);
        assert_eq!(engine.ledger_files(&root).expect("ledger").len(), 1);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn claim_entities_and_compact_queries_preserve_epistemic_attribution() {
        let engine = engine();
        let root = std::env::temp_dir().join(format!("epistemic-claim-test-{}", Uuid::new_v4()));
        let proposal = engine.proposal_submit(
            &root,
            &Map::from_iter([
                ("actor".into(), json!("tester")),
                ("authority_basis".into(), json!({"kind":"test"})),
                ("idempotency_key".into(), json!("claim-p1")),
                ("expected_ledger_head".into(), Value::Null),
                ("operations".into(), json!([{"op":"entity.declare","entity_id":"claim:tree-result","kind":"claim","title":"Attributed theorem result"}])),
            ]),
        ).expect("claim proposal");
        engine
            .proposal_admit(
                &root,
                &Map::from_iter([
                    ("proposal_id".into(), proposal["proposal_id"].clone()),
                    ("actor".into(), json!("tester")),
                    ("authority_basis".into(), json!({"kind":"test"})),
                    ("expected_ledger_head".into(), Value::Null),
                    ("idempotency_key".into(), json!("claim-a1")),
                ]),
            )
            .expect("claim admission");
        let result = engine
            .query(&root, &Map::from_iter([("compact".into(), json!(true))]))
            .expect("compact query");
        assert_eq!(result["compact"], true);
        assert_eq!(result["items"][0]["entity_id"], "claim:tree-result");
        assert_eq!(result["items"][0]["title"], "Attributed theorem result");
        assert!(result["items"][0].get("payload").is_none());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn projection_refuses_a_tampered_authority_event() {
        let engine = engine();
        let root = std::env::temp_dir().join(format!("epistemic-test-{}", Uuid::new_v4()));
        let proposal = engine
            .proposal_submit(
                &root,
                &Map::from_iter([
                    ("actor".into(), json!("nima")),
                    ("authority_basis".into(), json!({"kind":"test"})),
                    ("idempotency_key".into(), json!("p1")),
                    ("expected_ledger_head".into(), Value::Null),
                    ("operations".into(), json!([{"op":"entity.declare","entity_id":"problem-1","kind":"problem","title":"What explains X?"}])),
                ]),
            )
            .unwrap();
        let id = proposal["proposal_id"].as_str().unwrap();
        engine
            .proposal_admit(
                &root,
                &Map::from_iter([
                    ("proposal_id".into(), json!(id)),
                    ("actor".into(), json!("nima")),
                    ("authority_basis".into(), json!({"kind":"test"})),
                    ("expected_ledger_head".into(), Value::Null),
                    ("idempotency_key".into(), json!("a1")),
                ]),
            )
            .unwrap();
        let path = engine.ledger_files(&root).unwrap().remove(0);
        let mut event = engine.read_json(&path).unwrap();
        event["actor"] = json!("tampered");
        fs::write(&path, serde_json::to_vec_pretty(&event).unwrap()).unwrap();
        let failure = engine.rebuild_projection(&root).unwrap_err();
        assert_eq!(failure["code"], "ledger_hash_invalid");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn pure_source_capture_needs_no_placeholder_operation() {
        let engine = engine();
        let root = std::env::temp_dir().join(format!("epistemic-source-only-{}", Uuid::new_v4()));
        let result = engine
            .capture_sources(
                &root,
                &Map::from_iter([
                    ("actor".into(), json!("tester")),
                    ("authority_basis".into(), json!({"kind":"test"})),
                    ("sources".into(), json!([{"source_id":"source:only","title":"Only source","version":"1","locator":"ledger/only.md"}])),
                ]),
            )
            .expect("source-only capture");
        assert_eq!(result["source_count"], 1);
        assert_eq!(result["operation_count"], 1);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn compound_workflow_derives_relation_and_retry_identities() {
        let engine = engine();
        let root = std::env::temp_dir().join(format!("epistemic-compound-{}", Uuid::new_v4()));
        let args = Map::from_iter([
            ("actor".into(), json!("tester")),
            ("authority_basis".into(), json!({"kind":"test"})),
            (
                "operations".into(),
                json!([
                    {"op":"entity.declare","local_ref":"claim","kind":"claim","title":"A"},
                    {"op":"entity.declare","local_ref":"source","kind":"source","title":"Source A","version":"1","locator":"ledger/a.md"},
                    {"op":"relation.declare","relation_type":"derived_from","source_ref":"claim","target_ref":"source"}
                ]),
            ),
        ]);
        let first = engine.submit_review_admit(&root, &args).expect("compound admission");
        assert_eq!(first["review"]["status"], "policy_valid");
        assert_eq!(first["admission"]["status"], "admitted");
        let proposal = engine
            .load_proposal(&root, first["submission"]["proposal_id"].as_str().unwrap())
            .unwrap();
        assert!(proposal["operations"][0]["entity_id"]
            .as_str()
            .unwrap()
            .starts_with("claim:"));
        assert!(proposal["operations"][1]["entity_id"]
            .as_str()
            .unwrap()
            .starts_with("source:"));
        assert_eq!(
            proposal["operations"][2]["source_id"],
            proposal["operations"][0]["entity_id"]
        );
        assert_eq!(
            proposal["operations"][2]["target_id"],
            proposal["operations"][1]["entity_id"]
        );
        assert!(proposal["operations"][2]["relation_id"]
            .as_str()
            .unwrap()
            .starts_with("rel:derived_from-"));
        let retried = engine
            .submit_review_admit(&root, &args)
            .expect("idempotent compound retry");
        assert_eq!(
            retried["submission"]["proposal_id"],
            first["submission"]["proposal_id"]
        );
        assert_eq!(
            retried["admission"]["event_id"],
            first["admission"]["event_id"]
        );
        let _ = fs::remove_dir_all(root);
    }

    fn sequence_test_create(engine: &Engine, root: &Path, name: &str, start_at: u64) -> Value {
        engine
            .sequence_create(
                root,
                &Map::from_iter([
                    ("sequence_name".into(), json!(name)),
                    ("actor".into(), json!("tester")),
                    ("authority_basis".into(), json!({"kind":"test"})),
                    ("start_at".into(), json!(start_at)),
                ]),
            )
            .expect("create sequence")
    }

    fn sequence_test_claim(
        engine: &Engine,
        root: &Path,
        name: &str,
        key: &str,
    ) -> Result<Value, Value> {
        engine.sequence_claim_next(
            root,
            &Map::from_iter([
                ("sequence_name".into(), json!(name)),
                ("actor".into(), json!("tester")),
                ("authority_basis".into(), json!({"kind":"test"})),
                ("idempotency_key".into(), json!(key)),
            ]),
        )
    }

    #[test]
    fn sequences_create_claim_replay_and_page_immutable_history() {
        let engine = engine();
        let root = std::env::temp_dir().join(format!("epistemic-sequence-{}", Uuid::new_v4()));
        let created = sequence_test_create(&engine, &root, "ledger-entry", 40);
        assert_eq!(created["status"], "created");
        assert_eq!(created["next_value"], 40);
        let first =
            sequence_test_claim(&engine, &root, "ledger-entry", "entry-a").expect("first claim");
        let second =
            sequence_test_claim(&engine, &root, "ledger-entry", "entry-b").expect("second claim");
        let replay =
            sequence_test_claim(&engine, &root, "ledger-entry", "entry-a").expect("claim replay");
        assert_eq!(first["value"], 40);
        assert_eq!(second["value"], 41);
        assert_eq!(replay["value"], 40);
        assert_eq!(replay["idempotency_replay"], true);
        let status = engine
            .sequence_status(
                &root,
                &Map::from_iter([("sequence_name".into(), json!("ledger-entry"))]),
            )
            .expect("status");
        assert_eq!(status["claim_count"], 2);
        assert_eq!(status["next_value"], 42);
        let page = engine
            .sequence_claims(
                &root,
                &Map::from_iter([
                    ("sequence_name".into(), json!("ledger-entry")),
                    ("limit".into(), json!(1)),
                ]),
            )
            .expect("claims page");
        assert_eq!(page["count"], 1);
        assert_eq!(page["has_more"], true);
        let listed = engine.sequence_list(&root, &Map::new()).expect("sequence list");
        assert_eq!(listed["items"][0]["sequence_name"], "ledger-entry");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn sequence_claim_idempotency_is_recovered_from_canonical_history() {
        let engine = engine();
        let root =
            std::env::temp_dir().join(format!("epistemic-sequence-recovery-{}", Uuid::new_v4()));
        sequence_test_create(&engine, &root, "research-item", 1);
        let first =
            sequence_test_claim(&engine, &root, "research-item", "research-a").expect("claim");
        fs::remove_file(
            engine
                .sequence_directory(&root, "research-item")
                .join("idempotency")
                .join(format!("{}.json", sha256(b"research-a"))),
        )
        .expect("remove disposable index");
        let replay = sequence_test_claim(&engine, &root, "research-item", "research-a")
            .expect("recover replay");
        assert_eq!(replay["claim_id"], first["claim_id"]);
        assert_eq!(replay["idempotency_replay"], true);
        assert!(engine
            .sequence_directory(&root, "research-item")
            .join("idempotency")
            .join(format!("{}.json", sha256(b"research-a")))
            .exists());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn concurrent_sequence_claims_are_unique_and_contiguous() {
        let engine = std::sync::Arc::new(engine());
        let root =
            std::env::temp_dir().join(format!("epistemic-sequence-concurrent-{}", Uuid::new_v4()));
        sequence_test_create(&engine, &root, "parallel", 1);
        let root = std::sync::Arc::new(root);
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(12));
        let handles = (0..12)
            .map(|index| {
                let engine = engine.clone();
                let root = root.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    sequence_test_claim(&engine, &root, "parallel", &format!("parallel-{index}"))
                        .expect("parallel claim")["value"]
                        .as_u64()
                        .unwrap()
                })
            })
            .collect::<Vec<_>>();
        let mut values = handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect::<Vec<_>>();
        values.sort_unstable();
        assert_eq!(values, (1..=12).collect::<Vec<_>>());
        assert_eq!(
            engine
                .sequence_status(
                    &root,
                    &Map::from_iter([("sequence_name".into(), json!("parallel"))])
                )
                .unwrap()["integrity_status"],
            "valid"
        );
        let _ = fs::remove_dir_all(root.as_path());
    }

    #[test]
    fn sequence_refuses_reconfiguration_conflicting_replay_and_tampering() {
        let engine = engine();
        let root =
            std::env::temp_dir().join(format!("epistemic-sequence-invalid-{}", Uuid::new_v4()));
        sequence_test_create(&engine, &root, "audit", 5);
        let conflict = engine
            .sequence_create(
                &root,
                &Map::from_iter([
                    ("sequence_name".into(), json!("audit")),
                    ("actor".into(), json!("tester")),
                    ("authority_basis".into(), json!({"kind":"test"})),
                    ("start_at".into(), json!(6)),
                ]),
            )
            .expect_err("configuration conflict");
        assert_eq!(conflict["code"], "sequence_configuration_conflict");
        sequence_test_claim(&engine, &root, "audit", "same-key").expect("claim");
        let replay_conflict = engine
            .sequence_claim_next(
                &root,
                &Map::from_iter([
                    ("sequence_name".into(), json!("audit")),
                    ("actor".into(), json!("other")),
                    ("authority_basis".into(), json!({"kind":"test"})),
                    ("idempotency_key".into(), json!("same-key")),
                ]),
            )
            .expect_err("replay conflict");
        assert_eq!(
            replay_conflict["code"],
            "sequence_claim_idempotency_conflict"
        );
        let claim_path = engine
            .sequence_claims_directory(&root, "audit")
            .join("claim-00000000000000000005.json");
        let mut claim = engine.read_json(&claim_path).unwrap();
        claim["actor"] = json!("tampered");
        fs::write(&claim_path, serde_json::to_vec_pretty(&claim).unwrap()).unwrap();
        let corrupt = engine
            .sequence_status(
                &root,
                &Map::from_iter([("sequence_name".into(), json!("audit"))]),
            )
            .expect_err("tampered claim");
        assert_eq!(corrupt["code"], "sequence_claim_chain_invalid");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn sequence_refuses_invalid_names_and_reports_exhaustion() {
        let engine = engine();
        let root =
            std::env::temp_dir().join(format!("epistemic-sequence-exhausted-{}", Uuid::new_v4()));
        let invalid = engine
            .sequence_create(
                &root,
                &Map::from_iter([
                    ("sequence_name".into(), json!(" bad ")),
                    ("actor".into(), json!("tester")),
                    ("authority_basis".into(), json!({"kind":"test"})),
                ]),
            )
            .expect_err("invalid name");
        assert_eq!(invalid["code"], "sequence_name_invalid");
        sequence_test_create(&engine, &root, "finite", u64::MAX);
        let final_claim =
            sequence_test_claim(&engine, &root, "finite", "last").expect("last claim");
        assert_eq!(final_claim["value"], u64::MAX);
        assert_eq!(final_claim["exhausted"], true);
        let exhausted = sequence_test_claim(&engine, &root, "finite", "past-end")
            .expect_err("sequence exhausted");
        assert_eq!(exhausted["code"], "sequence_exhausted");
        let status = engine
            .sequence_status(
                &root,
                &Map::from_iter([("sequence_name".into(), json!("finite"))]),
            )
            .expect("exhausted status");
        assert_eq!(status["next_value"], Value::Null);
        assert_eq!(status["exhausted"], true);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn ledger_admission_lock_serializes_writers_and_recovers_idempotency_index() {
        let engine = std::sync::Arc::new(engine());
        let root = std::env::temp_dir().join(format!("epistemic-ledger-lock-{}", Uuid::new_v4()));
        let proposals = (0..2)
            .map(|index| engine.proposal_submit(&root, &Map::from_iter([("actor".into(), json!("tester")), ("authority_basis".into(), json!({"kind":"test"})), ("idempotency_key".into(), json!(format!("proposal-{index}"))), ("expected_ledger_head".into(), Value::Null), ("operations".into(), json!([{"op":"entity.declare","entity_id":format!("claim:lock-{index}"),"kind":"claim","title":format!("Lock {index}")}]))])).expect("proposal"))
            .collect::<Vec<_>>();
        let root = std::sync::Arc::new(root);
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let handles = proposals
            .into_iter()
            .enumerate()
            .map(|(index, proposal)| {
                let engine = engine.clone();
                let root = root.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    engine.proposal_admit(
                        &root,
                        &Map::from_iter([
                            ("proposal_id".into(), proposal["proposal_id"].clone()),
                            ("actor".into(), json!("tester")),
                            ("authority_basis".into(), json!({"kind":"test"})),
                            ("expected_ledger_head".into(), Value::Null),
                            (
                                "idempotency_key".into(),
                                json!(format!("admission-{index}")),
                            ),
                        ]),
                    )
                })
            })
            .collect::<Vec<_>>();
        let results = handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        assert_eq!(
            results
                .iter()
                .filter(|result| {
                    result.as_ref().is_err_and(|failure| {
                        failure["code"] == "ledger_head_conflict"
                            || failure["code"] == "proposal_not_admissible"
                    })
                })
                .count(),
            1
        );
        engine.verify_ledger(&root).expect("serialized ledger");
        assert_eq!(engine.ledger_files(&root).unwrap().len(), 1);
        let admitted = results.into_iter().find_map(Result::ok).unwrap();
        let event = engine
            .read_json(
                &engine
                    .ledger(&root)
                    .join(format!("{}.json", admitted["event_id"].as_str().unwrap())),
            )
            .unwrap();
        let key = event["idempotency_key"].as_str().unwrap();
        fs::remove_file(engine.ledger(&root).join(format!("idem-{}.txt", safe_name(key))))
            .expect("remove disposable ledger index");
        let replay = engine
            .proposal_admit(
                &root,
                &Map::from_iter([
                    ("proposal_id".into(), event["proposal_id"].clone()),
                    ("actor".into(), json!("tester")),
                    ("authority_basis".into(), json!({"kind":"test"})),
                    ("idempotency_key".into(), json!(key)),
                ]),
            )
            .expect("recover ledger replay");
        assert_eq!(replay["event_id"], admitted["event_id"]);
        assert_eq!(engine.ledger_files(&root).unwrap().len(), 1);
        let _ = fs::remove_dir_all(root.as_path());
    }

    fn fixture_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/epistemic-ledger")
    }

    fn copy_directory(source: &Path, target: &Path) {
        fs::create_dir_all(target).expect("create copy target");
        for entry in fs::read_dir(source).expect("read copy source") {
            let entry = entry.expect("copy entry");
            let destination = target.join(entry.file_name());
            if entry.file_type().expect("copy entry type").is_dir() {
                copy_directory(&entry.path(), &destination);
            } else {
                fs::copy(entry.path(), &destination).expect("copy file");
            }
        }
    }

    #[test]
    #[ignore = "rewrites the golden fixture on disk; run explicitly with --ignored"]
    fn regenerate_golden_fixture() {
        let engine = engine();
        let fixture = fixture_root();
        let _ = fs::remove_dir_all(&fixture);
        let root = std::env::temp_dir().join(format!("epistemic-fixture-gen-{}", Uuid::new_v4()));
        let admit = |operations: Value, proposal_key: &str, admission_key: &str, expected_head: Value| -> Value {
            let proposal = engine.proposal_submit(
                &root,
                &Map::from_iter([
                    ("actor".into(), json!("fixture")),
                    (
                        "authority_basis".into(),
                        json!({"kind":"fixture","summary":"Golden event-ledger fixture."}),
                    ),
                    ("idempotency_key".into(), json!(proposal_key)),
                    ("expected_ledger_head".into(), expected_head),
                    ("operations".into(), operations),
                ]),
            )
            .expect("fixture proposal");
            engine.proposal_admit(
                &root,
                &Map::from_iter([
                    ("proposal_id".into(), proposal["proposal_id"].clone()),
                    ("actor".into(), json!("fixture")),
                    (
                        "authority_basis".into(),
                        json!({"kind":"fixture","summary":"Golden event-ledger fixture."}),
                    ),
                    ("expected_ledger_head".into(), proposal["expected_ledger_head"].clone()),
                    ("idempotency_key".into(), json!(admission_key)),
                ]),
            )
            .expect("fixture admission")
        };
        let first = admit(
            json!([
                {"op":"entity.declare","entity_id":"problem:fixture","kind":"problem","title":"Fixture problem"},
                {"op":"entity.declare","entity_id":"source:fixture","kind":"source","title":"Fixture source","version":"1","locator":"docs/fixture.md"}
            ]),
            "fixture-p1",
            "fixture-a1",
            Value::Null,
        );
        let second = admit(
            json!([
                {"op":"entity.declare","entity_id":"claim:fixture","kind":"claim","title":"Fixture claim"},
                {"op":"relation.declare","relation_id":"rel:fixture-addresses","relation_type":"addresses","source_id":"claim:fixture","target_id":"problem:fixture"}
            ]),
            "fixture-p2",
            "fixture-a2",
            first["ledger_head"].clone(),
        );
        let third = admit(
            json!([
                {"op":"assessment.record","assessment_id":"assessment:fixture","subject_id":"claim:fixture","judgment":"supported","actor":"fixture","reason":"Fixture assessment.","evidence":[{"source_id":"source:fixture","locator":"docs/fixture.md","paraphrase":"The fixture source supports the claim."}]}
            ]),
            "fixture-p3",
            "fixture-a3",
            second["ledger_head"].clone(),
        );
        engine
            .sequence_create(
                &root,
                &Map::from_iter([
                    ("sequence_name".into(), json!("fixture-ledger-entry")),
                    ("actor".into(), json!("fixture")),
                    ("authority_basis".into(), json!({"kind":"fixture"})),
                    ("start_at".into(), json!(40)),
                ]),
            )
            .expect("fixture sequence");
        for key in ["fixture-c1", "fixture-c2"] {
            engine
                .sequence_claim_next(
                    &root,
                    &Map::from_iter([
                        ("sequence_name".into(), json!("fixture-ledger-entry")),
                        ("actor".into(), json!("fixture")),
                        ("authority_basis".into(), json!({"kind":"fixture"})),
                        ("idempotency_key".into(), json!(key)),
                    ]),
                )
                .expect("fixture claim");
        }
        let head = engine
            .ledger_head(&root)
            .expect("fixture head")
            .expect("non-empty fixture ledger");
        let mut event_ids = Vec::new();
        let mut event_hashes = Vec::new();
        for path in engine.ledger_files(&root).expect("fixture ledger files") {
            let event = engine.read_json(&path).expect("fixture event");
            event_ids.push(event["event_id"].clone());
            event_hashes.push(event["event_hash"].clone());
        }
        let manifest = engine
            .load_sequence_manifest(&root, "fixture-ledger-entry")
            .expect("manifest");
        let claims = engine
            .verified_sequence_claims(&root, "fixture-ledger-entry", &manifest)
            .expect("claims");
        let expected = json!({
            "schema":"narada.epistemic.golden-fixture.v1",
            "ledger_head":head,
            "event_ids":event_ids,
            "event_hashes":event_hashes,
            "replay":{"proposal_id":second["proposal_id"],"idempotency_key":"fixture-a2","event_id":second["event_id"]},
            "scan":{"idempotency_key":"fixture-a3","event_id":third["event_id"]},
            "sequence":{
                "name":"fixture-ledger-entry",
                "sequence_id":manifest["sequence_id"],
                "creation_hash":manifest["creation_hash"],
                "claim_ids":claims.iter().map(|claim| claim["claim_id"].clone()).collect::<Vec<_>>(),
                "claim_hashes":claims.iter().map(|claim| claim["claim_hash"].clone()).collect::<Vec<_>>(),
                "values":claims.iter().map(|claim| claim["value"].clone()).collect::<Vec<_>>()
            }
        });
        fs::create_dir_all(&fixture).expect("fixture directory");
        for (name, directory) in [
            ("ledger", engine.ledger(&root)),
            ("proposals", engine.proposals(&root)),
            ("sequences", engine.sequences(&root)),
        ] {
            copy_directory(&directory, &fixture.join(name));
        }
        fs::write(
            fixture.join("expected.json"),
            format!("{}\n", serde_json::to_string_pretty(&expected).unwrap()),
        )
        .expect("write expected fixture metadata");
        println!(
            "digest golden vector: {}",
            engine
                .digest_value(&json!({"alpha":1,"beta":"x","gamma":[1,2],"nested":{"z":true,"a":null}}))
                .unwrap()
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn golden_fixture_verifies_identically() {
        let engine = engine();
        let fixture = fixture_root();
        let expected = engine
            .read_json(&fixture.join("expected.json"))
            .expect("fixture metadata");
        let root = std::env::temp_dir().join(format!("epistemic-fixture-verify-{}", Uuid::new_v4()));
        for (name, directory) in [
            ("ledger", engine.ledger(&root)),
            ("proposals", engine.proposals(&root)),
            ("sequences", engine.sequences(&root)),
        ] {
            copy_directory(&fixture.join(name), &directory);
        }
        engine
            .verify_ledger(&root)
            .expect("fixture ledger chain verifies");
        assert_eq!(
            engine.ledger_head(&root).expect("fixture head").as_deref(),
            expected["ledger_head"].as_str()
        );
        let files = engine.ledger_files(&root).expect("fixture ledger files");
        assert_eq!(files.len(), expected["event_ids"].as_array().unwrap().len());
        for (index, path) in files.iter().enumerate() {
            let event = engine.read_json(path).expect("fixture event");
            assert_eq!(event["event_id"], expected["event_ids"][index]);
            assert_eq!(event["event_hash"], expected["event_hashes"][index]);
            assert_eq!(event["sequence"], (index + 1) as u64);
        }
        let scanned = engine
            .find_ledger_event_by_idempotency(
                &root,
                expected["scan"]["idempotency_key"].as_str().unwrap(),
            )
            .expect("idempotency scan")
            .expect("fixture event recovered by scan");
        assert_eq!(scanned["event_id"], expected["scan"]["event_id"]);
        let replay = engine
            .proposal_admit(
                &root,
                &Map::from_iter([
                    ("proposal_id".into(), expected["replay"]["proposal_id"].clone()),
                    ("actor".into(), json!("fixture")),
                    ("authority_basis".into(), json!({"kind":"fixture"})),
                    (
                        "idempotency_key".into(),
                        expected["replay"]["idempotency_key"].clone(),
                    ),
                ]),
            )
            .expect("fixture admission replay");
        assert_eq!(replay["event_id"], expected["replay"]["event_id"]);
        assert_eq!(replay["ledger_head"], expected["event_hashes"][1]);
        let name = expected["sequence"]["name"].as_str().unwrap();
        let manifest = engine
            .load_sequence_manifest(&root, name)
            .expect("fixture manifest verifies");
        assert_eq!(manifest["creation_hash"], expected["sequence"]["creation_hash"]);
        let claims = engine
            .verified_sequence_claims(&root, name, &manifest)
            .expect("fixture claim chain verifies");
        let expected_hashes = expected["sequence"]["claim_hashes"].as_array().unwrap();
        assert_eq!(claims.len(), expected_hashes.len());
        for (claim, hash) in claims.iter().zip(expected_hashes.iter()) {
            assert_eq!(&claim["claim_hash"], hash);
        }
        let _ = fs::remove_dir_all(root);
    }
}
