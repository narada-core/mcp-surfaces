use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;
use uuid::Uuid;

const ENTITY_KINDS: &[&str] = &[
    "problem",
    "conjecture",
    "claim",
    "criticism",
    "test",
    "source",
];
const CORE_RELATIONS: &[&str] = &[
    "addresses",
    "criticizes",
    "tests",
    "depends_on",
    "derived_from",
    "transforms",
    "supersedes",
];
const MAX_OPERATIONS: usize = 200;
const MAX_PAGE: u64 = 100;
const MAX_BATCH_QUERIES: usize = 20;
const MAX_SOURCE_FILES: usize = 20;
const MAX_SOURCE_BYTES: u64 = 1_048_576;

pub fn list_tools() -> Vec<Value> {
    vec![
        tool(
            "epistemic_graph_guidance",
            "Explain the problem-situation graph workflow.",
            json!({"type":"object","properties":{"workflow":{"type":"string","maxLength":256},"tool":{"type":"string","maxLength":256}},"additionalProperties":false}),
            true,
        ),
        tool(
            "epistemic_graph_status",
            "Inspect ledger and projection readiness.",
            object(&[]),
            true,
        ),
        tool(
            "epistemic_graph_query",
            "List bounded entities, or set record_kind to read assessments, test outcomes, or sweeps.",
            json!({"type":"object","properties":{"kind":{"type":"string","enum":ENTITY_KINDS,"description":"Entity kind filter."},"record_kind":{"type":"string","enum":["assessment.record","test_outcome.record","sweep.record"],"description":"When present, query durable non-entity records instead of entities."},"text":{"type":"string"},"compact":{"type":"boolean","default":false,"description":"Return identity and summary fields without full stored payloads."},"limit":{"type":"integer","minimum":1,"maximum":100},"offset":{"type":"integer","minimum":0}},"additionalProperties":false}),
            true,
        ),
        tool(
            "epistemic_graph_query_batch",
            "Run several compact bounded graph queries in one call.",
            json!({"type":"object","properties":{"queries":{"type":"array","minItems":1,"maxItems":20,"items":{"type":"object","properties":{"text":{"type":"string"},"kind":{"type":"string","enum":ENTITY_KINDS},"record_kind":{"type":"string","enum":["assessment.record","test_outcome.record","sweep.record"]}},"additionalProperties":false}},"limit_per_query":{"type":"integer","minimum":1,"maximum":20}},"required":["queries"],"additionalProperties":false}),
            true,
        ),
        tool(
            "epistemic_graph_source_inspect",
            "Inspect epistemically relevant Markdown sections in bounded site-local source files.",
            json!({"type":"object","properties":{"paths":{"type":"array","minItems":1,"maxItems":20,"items":{"type":"string","minLength":1}},"max_sections_per_file":{"type":"integer","minimum":1,"maximum":50},"max_chars_per_section":{"type":"integer","minimum":100,"maximum":4000}},"required":["paths"],"additionalProperties":false}),
            true,
        ),
        tool(
            "epistemic_graph_neighborhood",
            "Read a bounded one-hop neighborhood.",
            json!({"type":"object","properties":{"entity_id":{"type":"string"},"limit":{"type":"integer","minimum":1,"maximum":100}},"required":["entity_id"],"additionalProperties":false}),
            true,
        ),
        tool(
            "epistemic_graph_proposal_submit",
            "Persist an immutable atomic proposal of typed graph operations. Use guidance for a complete copyable example.",
            proposal_schema(),
            false,
        ),
        tool(
            "epistemic_graph_submit_review_admit",
            "Submit, policy-review, and admit one atomic contribution while preserving the immutable proposal and review gate.",
            proposal_schema(),
            false,
        ),
        tool(
            "epistemic_graph_capture_sources",
            "Create one bounded proposal from concise source descriptors plus optional typed non-source operations; reports existing identities before explicit review and admission.",
            capture_sources_schema(),
            false,
        ),
        tool(
            "epistemic_graph_proposal_read",
            "Read immutable proposal metadata and a bounded page of operations.",
            json!({"type":"object","properties":{"proposal_id":{"type":"string"},"limit":{"type":"integer","minimum":1,"maximum":100},"offset":{"type":"integer","minimum":0}},"required":["proposal_id"],"additionalProperties":false}),
            true,
        ),
        tool(
            "epistemic_graph_proposal_resubmit",
            "Create a new immutable proposal by dropping and replacing explicitly identified operations from an earlier proposal.",
            proposal_resubmit_schema(),
            false,
        ),
        tool(
            "epistemic_graph_proposal_review",
            "Validate an immutable proposal without asserting truth.",
            proposal_id_schema(),
            false,
        ),
        tool(
            "epistemic_graph_proposal_admit",
            "Append a policy-valid proposal to canonical history.",
            json!({"type":"object","properties":{"proposal_id":{"type":"string"},"actor":{"type":"string"},"authority_basis":{"type":"object"},"expected_ledger_head":{"type":["string","null"],"description":"Optional explicit CAS boundary; omitted means the current head."},"idempotency_key":{"type":"string","description":"Optional override; omitted values use deterministic proposal admission identity."}},"required":["proposal_id","actor","authority_basis"],"additionalProperties":false}),
            false,
        ),
        tool(
            "epistemic_graph_proposal_reject",
            "Reject a proposal with criticism or rationale.",
            json!({"type":"object","properties":{"proposal_id":{"type":"string"},"actor":{"type":"string"},"reason":{"type":"string"}},"required":["proposal_id","actor","reason"],"additionalProperties":false}),
            false,
        ),
        tool(
            "epistemic_graph_export",
            "Export a deterministic JSON or JSON-LD projection.",
            json!({"type":"object","properties":{"format":{"type":"string","enum":["json","jsonld"]}},"additionalProperties":false}),
            true,
        ),
    ]
}

pub fn call_tool(name: &str, args: &Map<String, Value>, site_root: &Path) -> Result<Value, Value> {
    match name {
        "epistemic_graph_guidance" => Ok(guidance_with_request(args)),
        "epistemic_graph_status" => status(site_root),
        "epistemic_graph_query" => query(site_root, args),
        "epistemic_graph_query_batch" => query_batch(site_root, args),
        "epistemic_graph_source_inspect" => source_inspect(site_root, args),
        "epistemic_graph_neighborhood" => neighborhood(site_root, args),
        "epistemic_graph_proposal_submit" => proposal_submit(site_root, args),
        "epistemic_graph_submit_review_admit" => submit_review_admit(site_root, args),
        "epistemic_graph_capture_sources" => capture_sources(site_root, args),
        "epistemic_graph_proposal_read" => proposal_read(site_root, args),
        "epistemic_graph_proposal_resubmit" => proposal_resubmit(site_root, args),
        "epistemic_graph_proposal_review" => proposal_review(site_root, args),
        "epistemic_graph_proposal_admit" => proposal_admit(site_root, args),
        "epistemic_graph_proposal_reject" => proposal_reject(site_root, args),
        "epistemic_graph_export" => export(site_root, args),
        _ => Err(error(
            "unknown_tool",
            &format!("unknown_tool:{name}"),
            Value::Null,
        )),
    }
}

fn proposal_submit(root: &Path, args: &Map<String, Value>) -> Result<Value, Value> {
    prepare(root)?;
    let actor = required(args, "actor")?;
    let supplied_operations = args
        .get("operations")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            error(
                "invalid_proposal",
                "operations must be an array",
                Value::Null,
            )
        })?;
    if supplied_operations.is_empty() || supplied_operations.len() > MAX_OPERATIONS {
        return Err(error(
            "invalid_proposal",
            "operations count must be between 1 and 200",
            json!({"count":supplied_operations.len()}),
        ));
    }
    let operations = normalize_operations(supplied_operations)?;
    validate_operations(&operations, false)?;
    let expected = resolve_expected_ledger_head(root, args.get("expected_ledger_head"))?;
    let semantic_content = json!({"actor":actor,"authority_basis":args.get("authority_basis"),"operations":operations});
    let content_fingerprint = digest_value(&semantic_content)?;
    let idempotency_key = args
        .get("idempotency_key")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| derived_idempotency_key("proposal", &semantic_content));
    let proposal_id = format!("ep_{}", Uuid::new_v4());
    let created_at = now();
    let payload = json!({
        "schema":"narada.epistemic.proposal.v1", "proposal_id":proposal_id,
        "status":"submitted", "actor":actor, "authority_basis":args.get("authority_basis"),
        "idempotency_key":idempotency_key, "expected_ledger_head":expected,
        "created_at":created_at, "content_fingerprint":content_fingerprint, "operations":operations
    });
    let digest = digest_value(&payload)?;
    let mut stored = payload;
    stored
        .as_object_mut()
        .unwrap()
        .insert("digest".into(), json!(digest));
    let idem_path = proposals(root).join(format!("idem-{}.txt", safe_name(&idempotency_key)));
    if idem_path.exists() {
        let existing =
            fs::read_to_string(&idem_path).map_err(io_error("proposal_idempotency_read_failed"))?;
        let stored = read_json(&proposals(root).join(format!("{}.json", existing.trim())))?;
        if stored
            .get("content_fingerprint")
            .and_then(Value::as_str)
            .is_some()
            && stored.get("content_fingerprint") != Some(&json!(content_fingerprint))
        {
            return Err(error(
                "proposal_idempotency_conflict",
                "idempotency key already names different proposal content",
                json!({"idempotency_key":idempotency_key,"existing_proposal_id":stored["proposal_id"]}),
            ));
        }
        return Ok(proposal_receipt(&stored));
    }
    write_new_json(
        &proposals(root).join(format!("{proposal_id}.json")),
        &stored,
    )?;
    write_new(&idem_path, proposal_id.as_bytes())?;
    Ok(proposal_receipt(&stored))
}

fn submit_review_admit(root: &Path, args: &Map<String, Value>) -> Result<Value, Value> {
    let submission = proposal_submit(root, args)?;
    let proposal_id = submission["proposal_id"].as_str().ok_or_else(|| {
        error(
            "proposal_submission_corrupt",
            "proposal id missing",
            submission.clone(),
        )
    })?;
    let lifecycle = proposal_lifecycle(root, proposal_id)?;
    if lifecycle["status"] == "admitted" {
        let review =
            read_json(&proposals(root).join(format!("{}.review.json", safe_name(proposal_id))))?;
        return Ok(json!({
            "schema":"narada.epistemic.submit_review_admit.v1",
            "status":"already_admitted",
            "submission":submission,
            "review":review,
            "admission":lifecycle,
            "review_gate_preserved":true,
            "certifies_truth":false
        }));
    }
    let review = proposal_review(
        root,
        &Map::from_iter([("proposal_id".into(), json!(proposal_id))]),
    )?;
    if review["status"] != "policy_valid" {
        return Err(error(
            "proposal_not_admissible",
            "compound contribution stopped at the preserved review gate",
            json!({"submission":submission,"review":review}),
        ));
    }
    let admission_idempotency = derived_idempotency_key(
        "admission",
        &json!({"proposal_id":proposal_id,"proposal_digest":submission["proposal_digest"]}),
    );
    let admission = proposal_admit(
        root,
        &Map::from_iter([
            ("proposal_id".into(), json!(proposal_id)),
            ("actor".into(), json!(required(args, "actor")?)),
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
        "schema":"narada.epistemic.submit_review_admit.v1",
        "status":"admitted",
        "submission":submission,
        "review":review,
        "admission":admission,
        "review_gate_preserved":true,
        "certifies_truth":false
    }))
}

fn normalize_operations(operations: &[Value]) -> Result<Vec<Value>, Value> {
    let mut local_ids = std::collections::HashMap::new();
    let mut first_pass = Vec::with_capacity(operations.len());
    for operation in operations {
        let mut normalized = operation.clone();
        if operation.get("op").and_then(Value::as_str) == Some("entity.declare") {
            let object = normalized.as_object_mut().unwrap();
            if object.get("entity_id").and_then(Value::as_str).is_none() {
                let kind = object.get("kind").and_then(Value::as_str).unwrap_or("");
                let title = object.get("title").and_then(Value::as_str).unwrap_or("");
                if !kind.is_empty() && !title.is_empty() {
                    let digest = digest_value(
                        &json!({"kind":kind,"title":title,"version":object.get("version"),"locator":object.get("locator")}),
                    )?;
                    object.insert(
                        "entity_id".into(),
                        json!(format!("{}:{}", safe_name(kind), &digest[..20])),
                    );
                }
            }
            if let (Some(local_ref), Some(entity_id)) = (
                object.get("local_ref").and_then(Value::as_str),
                object.get("entity_id").and_then(Value::as_str),
            ) {
                if local_ids
                    .insert(local_ref.to_string(), entity_id.to_string())
                    .is_some()
                {
                    return Err(error(
                        "duplicate_local_ref",
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
            if operation.get("op").and_then(Value::as_str) == Some("relation.declare") {
                let object = normalized.as_object_mut().unwrap();
                for (id_field, ref_field) in [("source_id", "source_ref"), ("target_id", "target_ref")] {
                    if object.get(id_field).and_then(Value::as_str).is_none() {
                        if let Some(reference) = object.get(ref_field).and_then(Value::as_str) {
                            let resolved = local_ids.get(reference).ok_or_else(|| error("local_ref_not_found", "relation reference does not identify an entity in this proposal", json!({"field":ref_field,"local_ref":reference})))?;
                            object.insert(id_field.into(), json!(resolved));
                        }
                    }
                }
            }
            if normalized.get("op").and_then(Value::as_str) == Some("relation.declare")
                && normalized
                    .get("relation_id")
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
                let digest =
                    sha256(format!("{relation_type}\0{source_id}\0{target_id}").as_bytes());
                normalized.as_object_mut().unwrap().insert(
                    "relation_id".into(),
                    json!(format!(
                        "rel:{}-{}",
                        safe_name(&relation_type),
                        &digest[..16]
                    )),
                );
            }
            Ok(normalized)
        })
        .collect()
}

fn resolve_expected_ledger_head(root: &Path, supplied: Option<&Value>) -> Result<Value, Value> {
    if supplied.is_none() || supplied.and_then(Value::as_str) == Some("latest") {
        return Ok(ledger_head(root)?.map(Value::String).unwrap_or(Value::Null));
    }
    Ok(supplied.cloned().unwrap_or(Value::Null))
}

fn derived_idempotency_key(kind: &str, payload: &Value) -> String {
    let canonical = serde_json::to_vec(payload).unwrap_or_default();
    format!("auto-{kind}-{}", &sha256(&canonical)[..24])
}

fn proposal_receipt(proposal: &Value) -> Value {
    json!({
        "schema":"narada.epistemic.proposal_submission.v1",
        "status":proposal["status"],
        "proposal_id":proposal["proposal_id"],
        "proposal_digest":proposal["digest"],
        "content_fingerprint":proposal["content_fingerprint"],
        "operation_count":proposal["operations"].as_array().map_or(0, Vec::len),
        "expected_ledger_head":proposal["expected_ledger_head"],
        "created_at":proposal["created_at"]
    })
}

fn capture_sources(root: &Path, args: &Map<String, Value>) -> Result<Value, Value> {
    prepare(root)?;
    rebuild_projection(root)?;
    let sources = args
        .get("sources")
        .and_then(Value::as_array)
        .ok_or_else(|| error("invalid_capture", "sources must be an array", Value::Null))?;
    let supplied = args
        .get("operations")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if sources.is_empty() {
        return Err(error(
            "invalid_capture",
            "at least one source is required",
            Value::Null,
        ));
    }
    let mut operations = Vec::with_capacity(sources.len() + supplied.len());
    for source in sources {
        let source = source.as_object().ok_or_else(|| {
            error(
                "invalid_capture",
                "each source must be an object",
                Value::Null,
            )
        })?;
        operations.push(json!({
            "op":"entity.declare",
            "entity_id":required(source, "source_id")?,
            "kind":"source",
            "title":required(source, "title")?,
            "version":required(source, "version")?,
            "locator":required(source, "locator")?
        }));
    }
    for operation in &supplied {
        if operation.get("kind").and_then(Value::as_str) == Some("source") {
            return Err(error(
                "invalid_capture",
                "declare sources through the sources field, not operations",
                Value::Null,
            ));
        }
        operations.push(operation.clone());
    }
    if operations.len() > MAX_OPERATIONS {
        return Err(error(
            "invalid_capture",
            "combined source and operation count exceeds 200",
            json!({"source_count":sources.len(),"operation_count":supplied.len(),"combined_count":operations.len()}),
        ));
    }
    let existing_identities = existing_operation_identities(root, &operations)?;
    let mut proposal_args = args.clone();
    proposal_args.remove("sources");
    proposal_args.insert("operations".into(), json!(operations));
    let receipt = proposal_submit(root, &proposal_args)?;
    Ok(json!({
        "schema":"narada.epistemic.source_capture.v1",
        "status":"draft_submitted",
        "proposal_id":receipt["proposal_id"],
        "proposal_digest":receipt["proposal_digest"],
        "expected_ledger_head":receipt["expected_ledger_head"],
        "source_count":sources.len(),
        "operation_count":receipt["operation_count"],
        "existing_identity_count":existing_identities.len(),
        "existing_identities":existing_identities,
        "next":{"review":{"tool":"epistemic_graph_proposal_review","proposal_id":receipt["proposal_id"]}},
        "admission_requires_explicit_call":true,
        "certifies_truth":false,
        "bounded":true
    }))
}

fn existing_operation_identities(root: &Path, operations: &[Value]) -> Result<Vec<Value>, Value> {
    let db = Connection::open(projection_path(root)).map_err(db_error("projection_open_failed"))?;
    let mut existing = Vec::new();
    for operation in operations {
        let (table, column, identity) = match operation.get("op").and_then(Value::as_str) {
            Some("entity.declare") => ("entities", "entity_id", operation.get("entity_id")),
            Some("relation.declare") => ("relations", "relation_id", operation.get("relation_id")),
            Some("assessment.record") => ("records", "record_id", operation.get("assessment_id")),
            Some("test_outcome.record") => ("records", "record_id", operation.get("outcome_id")),
            Some("sweep.record") => ("records", "record_id", operation.get("sweep_id")),
            _ => continue,
        };
        let Some(identity) = identity.and_then(Value::as_str) else {
            continue;
        };
        let sql = format!("select 1 from {table} where {column}=?1 limit 1");
        let found = db
            .query_row(&sql, params![identity], |_| Ok(()))
            .optional()
            .map_err(db_error("projection_duplicate_check_failed"))?
            .is_some();
        if found {
            existing.push(json!({"op":operation["op"],"identity":identity}));
        }
    }
    Ok(existing)
}

fn proposal_read(root: &Path, args: &Map<String, Value>) -> Result<Value, Value> {
    let id = required(args, "proposal_id")?;
    let proposal = load_proposal(root, &id)?;
    let operations = proposal["operations"].as_array().ok_or_else(|| {
        error(
            "proposal_corrupt",
            "proposal operations missing",
            json!({"proposal_id":id}),
        )
    })?;
    let offset = args.get("offset").and_then(Value::as_u64).unwrap_or(0) as usize;
    let limit = args
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(20)
        .min(MAX_PAGE) as usize;
    let items = operations
        .iter()
        .skip(offset)
        .take(limit)
        .cloned()
        .collect::<Vec<_>>();
    let next_offset = (offset + items.len() < operations.len()).then_some(offset + items.len());
    let lifecycle = proposal_lifecycle(root, &id)?;
    Ok(json!({
        "schema":"narada.epistemic.proposal_read.v1",
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

fn operation_identity(operation: &Value) -> Option<String> {
    let (kind, field) = match operation.get("op").and_then(Value::as_str)? {
        "entity.declare" => ("entity", "entity_id"),
        "relation.declare" => ("relation", "relation_id"),
        "assessment.record" => ("assessment", "assessment_id"),
        "test_outcome.record" => ("test_outcome", "outcome_id"),
        "sweep.record" => ("sweep", "sweep_id"),
        _ => return None,
    };
    operation
        .get(field)
        .and_then(Value::as_str)
        .map(|identity| format!("{kind}:{identity}"))
}

fn proposal_resubmit_schema() -> Value {
    let operation_items = proposal_schema()
        .pointer("/properties/operations/items")
        .cloned()
        .unwrap_or_else(|| json!({"type":"object"}));
    json!({
        "type":"object",
        "properties":{
            "source_proposal_id":{"type":"string","minLength":1},
            "actor":{"type":"string","minLength":1},
            "authority_basis":{"type":"object","minProperties":1},
            "idempotency_key":{"type":"string","minLength":1,"description":"Optional override; omitted values use deterministic content-hash idempotency."},
            "expected_ledger_head":{"type":["string","null"],"description":"Optional explicit CAS boundary; omitted means the current head."},
            "drop_operation_ids":{"type":"array","maxItems":200,"uniqueItems":true,"items":{"type":"string","minLength":1}},
            "replacements":{"type":"array","maxItems":200,"items":operation_items}
        },
        "required":["source_proposal_id","actor","authority_basis"],
        "additionalProperties":false
    })
}

fn proposal_resubmit(root: &Path, args: &Map<String, Value>) -> Result<Value, Value> {
    let source_id = required(args, "source_proposal_id")?;
    let source = load_proposal(root, &source_id)?;
    let original = source["operations"].as_array().ok_or_else(|| {
        error(
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
        return Err(error(
            "invalid_proposal_resubmission",
            "drop_operation_ids must contain unique strings",
            Value::Null,
        ));
    }
    let known = original
        .iter()
        .filter_map(operation_identity)
        .collect::<HashSet<_>>();
    let missing = drop_ids.difference(&known).cloned().collect::<Vec<_>>();
    if !missing.is_empty() {
        return Err(error(
            "proposal_operation_not_found",
            "one or more drop_operation_ids do not identify source proposal operations",
            json!({"missing":missing}),
        ));
    }
    let replacements = args
        .get("replacements")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    validate_operations(&replacements, false)?;
    let mut operations = original
        .iter()
        .filter(|operation| {
            operation_identity(operation)
                .map(|identity| !drop_ids.contains(&identity))
                .unwrap_or(true)
        })
        .cloned()
        .collect::<Vec<_>>();
    operations.extend(replacements);
    if operations.is_empty() || operations.len() > MAX_OPERATIONS {
        return Err(error(
            "invalid_proposal_resubmission",
            "resulting operations count must be between 1 and 200",
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
    let receipt = proposal_submit(root, &submit_args)?;
    Ok(json!({
        "schema":"narada.epistemic.proposal_resubmission.v1",
        "status":"draft_submitted",
        "source_proposal_id":source_id,
        "proposal_id":receipt["proposal_id"],
        "proposal_digest":receipt["proposal_digest"],
        "operation_count":receipt["operation_count"],
        "dropped_operation_ids":drop_ids,
        "replacement_count":args.get("replacements").and_then(Value::as_array).map_or(0, Vec::len),
        "expected_ledger_head":receipt["expected_ledger_head"],
        "next":{"review":{"tool":"epistemic_graph_proposal_review","proposal_id":receipt["proposal_id"]}},
        "admission_requires_explicit_call":true,
        "certifies_truth":false,
        "bounded":true
    }))
}

fn proposal_lifecycle(root: &Path, proposal_id: &str) -> Result<Value, Value> {
    for path in ledger_files(root)? {
        let event = read_json(&path)?;
        if event.get("proposal_id").and_then(Value::as_str) == Some(proposal_id) {
            return Ok(json!({
                "status":"admitted",
                "event_id":event["event_id"],
                "sequence":event["sequence"],
                "ledger_head":event["event_hash"],
                "admitted_at":event["occurred_at"]
            }));
        }
    }
    let rejection_path = proposals(root).join(format!("{}.rejection.json", safe_name(proposal_id)));
    if rejection_path.exists() {
        let rejection = read_json(&rejection_path)?;
        return Ok(json!({
            "status":"rejected",
            "rejected_at":rejection["occurred_at"],
            "reason":rejection["reason"]
        }));
    }
    let review_path = proposals(root).join(format!("{}.review.json", safe_name(proposal_id)));
    if review_path.exists() {
        let review = read_json(&review_path)?;
        return Ok(json!({"status":"reviewed","review_status":review["status"]}));
    }
    Ok(json!({"status":"submitted"}))
}

fn proposal_review(root: &Path, args: &Map<String, Value>) -> Result<Value, Value> {
    let id = required(args, "proposal_id")?;
    let proposal = load_proposal(root, &id)?;
    let operations = proposal["operations"].as_array().ok_or_else(|| {
        error(
            "proposal_corrupt",
            "proposal operations missing",
            json!({"proposal_id":id}),
        )
    })?;
    validate_operations(operations, true)?;
    validate_references(root, operations)?;
    let expected = proposal.get("expected_ledger_head").and_then(Value::as_str);
    let current = ledger_head(root)?;
    let head_matches = expected == current.as_deref();
    let review = json!({"schema":"narada.epistemic.proposal_review.v1","proposal_id":id,"status":if head_matches{"policy_valid"}else{"stale"},"certifies_truth":false,"checks":{"schema":true,"references":true,"evidence_locations":true,"graph_invariants":true,"ledger_head":head_matches},"expected_ledger_head":expected,"actual_ledger_head":current});
    write_replace_json(&proposals(root).join(format!("{id}.review.json")), &review)?;
    Ok(review)
}

fn proposal_admit(root: &Path, args: &Map<String, Value>) -> Result<Value, Value> {
    prepare(root)?;
    let id = required(args, "proposal_id")?;
    let actor = required(args, "actor")?;
    let proposal = load_proposal(root, &id)?;
    let idem = args
        .get("idempotency_key")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| {
            derived_idempotency_key(
                "admission",
                &json!({"proposal_id":id,"proposal_digest":proposal["digest"]}),
            )
        });
    let idem_path = ledger(root).join(format!("idem-{}.txt", safe_name(&idem)));
    if idem_path.exists() {
        let event_id =
            fs::read_to_string(&idem_path).map_err(io_error("ledger_idempotency_read_failed"))?;
        let event = read_json(&ledger(root).join(format!("{}.json", event_id.trim())))?;
        if event.get("proposal_id") != Some(&json!(id))
            || event.get("proposal_digest") != proposal.get("digest")
        {
            return Err(error(
                "admission_idempotency_conflict",
                "idempotency key already names a different proposal admission",
                json!({"idempotency_key":idem,"existing_event_id":event_id.trim()}),
            ));
        }
        return Ok(admission_receipt(&event));
    }
    let review = proposal_review(root, &Map::from_iter([("proposal_id".into(), json!(id))]))?;
    if review["status"] != "policy_valid" {
        return Err(error(
            "proposal_not_admissible",
            "proposal review is not policy_valid",
            review,
        ));
    }
    let expected_value = resolve_expected_ledger_head(root, args.get("expected_ledger_head"))?;
    let expected = expected_value.as_str();
    let current = ledger_head(root)?;
    if expected != current.as_deref()
        || proposal.get("expected_ledger_head").and_then(Value::as_str) != current.as_deref()
    {
        return Err(error(
            "ledger_head_conflict",
            "expected ledger head does not match",
            json!({"expected":expected,"proposal_expected":proposal.get("expected_ledger_head"),"actual":current}),
        ));
    }
    let seq = ledger_files(root)?.len() as u64 + 1;
    let event_id = format!("ev-{seq:012}-{}", Uuid::new_v4());
    let event_without_hash = json!({"schema":"narada.epistemic.event.v1","sequence":seq,"event_id":event_id,"event_kind":"proposal_admitted","previous_hash":current,"proposal_id":id,"proposal_digest":proposal["digest"],"operations":proposal["operations"],"actor":actor,"authority_basis":args.get("authority_basis"),"idempotency_key":idem,"occurred_at":now(),"certifies_truth":false});
    let event_hash = digest_value(&event_without_hash)?;
    let mut event = event_without_hash;
    event
        .as_object_mut()
        .unwrap()
        .insert("event_hash".into(), json!(event_hash));
    write_new_json(&ledger(root).join(format!("{event_id}.json")), &event)?;
    write_new(&idem_path, event_id.as_bytes())?;
    rebuild_projection(root)?;
    Ok(admission_receipt(&event))
}

fn admission_receipt(event: &Value) -> Value {
    json!({
        "schema":"narada.epistemic.proposal_admission.v1",
        "status":"admitted",
        "proposal_id":event["proposal_id"],
        "proposal_digest":event["proposal_digest"],
        "event_id":event["event_id"],
        "sequence":event["sequence"],
        "operation_count":event["operations"].as_array().map_or(0, Vec::len),
        "ledger_head":event["event_hash"],
        "certifies_truth":false
    })
}

fn proposal_reject(root: &Path, args: &Map<String, Value>) -> Result<Value, Value> {
    let id = required(args, "proposal_id")?;
    let _ = load_proposal(root, &id)?;
    let rejection = json!({"schema":"narada.epistemic.proposal_rejection.v1","proposal_id":id,"status":"rejected","actor":required(args,"actor")?,"reason":required(args,"reason")?,"occurred_at":now()});
    write_new_json(
        &proposals(root).join(format!("{id}.rejection.json")),
        &rejection,
    )?;
    Ok(rejection)
}

fn status(root: &Path) -> Result<Value, Value> {
    prepare(root)?;
    rebuild_projection(root)?;
    let db = Connection::open(projection_path(root)).map_err(db_error("projection_open_failed"))?;
    let entities: i64 = db
        .query_row("select count(*) from entities", [], |r| r.get(0))
        .map_err(db_error("projection_count_failed"))?;
    let relations: i64 = db
        .query_row("select count(*) from relations", [], |r| r.get(0))
        .map_err(db_error("projection_count_failed"))?;
    let records: i64 = db
        .query_row("select count(*) from records", [], |r| r.get(0))
        .map_err(db_error("projection_count_failed"))?;
    Ok(
        json!({"schema":"narada.epistemic.status.v1","status":"ok","implementation":"rust-native","canonical_store":ledger(root).to_string_lossy(),"projection":projection_path(root).to_string_lossy(),"ledger_head":ledger_head(root)?,"event_count":ledger_files(root)?.len(),"entity_count":entities,"relation_count":relations,"record_count":records,"projection_rebuildable":true,"truth_certification":false}),
    )
}

fn query(root: &Path, args: &Map<String, Value>) -> Result<Value, Value> {
    rebuild_projection(root)?;
    let db = Connection::open(projection_path(root)).map_err(db_error("projection_open_failed"))?;
    let limit = args
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(50)
        .min(MAX_PAGE);
    let offset = args.get("offset").and_then(Value::as_u64).unwrap_or(0);
    let kind = args.get("kind").and_then(Value::as_str).unwrap_or("");
    let compact = args
        .get("compact")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let text = args.get("text").and_then(Value::as_str).unwrap_or("");
    let like = format!("%{text}%");
    if let Some(record_kind) = args.get("record_kind").and_then(Value::as_str) {
        let mut stmt = db.prepare("select record_id,record_kind,payload_json,event_id from records where record_kind=?1 and (?2='' or payload_json like ?3) order by record_id limit ?4 offset ?5").map_err(db_error("projection_record_query_prepare_failed"))?;
        let rows = stmt.query_map(params![record_kind,text,like,limit,offset], |row| {
            let payload = serde_json::from_str::<Value>(&row.get::<_,String>(2)?).unwrap_or(Value::Null);
            Ok(if compact {
                json!({"record_id":row.get::<_,String>(0)?,"record_kind":row.get::<_,String>(1)?,"subject_id":payload.get("subject_id"),"judgment":payload.get("judgment"),"status":payload.get("status"),"event_id":row.get::<_,String>(3)?})
            } else {
                json!({"record_id":row.get::<_,String>(0)?,"record_kind":row.get::<_,String>(1)?,"payload":payload,"event_id":row.get::<_,String>(3)?})
            })
        }).map_err(db_error("projection_record_query_failed"))?;
        let items = rows
            .collect::<Result<Vec<_>, _>>()
            .map_err(db_error("projection_record_query_row_failed"))?;
        return Ok(
            json!({"schema":"narada.epistemic.query.v1","status":"ok","result_kind":"records","record_kind":record_kind,"compact":compact,"items":items,"offset":offset,"limit":limit,"returned":items.len(),"bounded":true}),
        );
    }
    let mut stmt = db.prepare("select entity_id,kind,payload_json,event_id from entities where (?1='' or kind=?1) and (?2='' or payload_json like ?3) order by entity_id limit ?4 offset ?5").map_err(db_error("projection_query_prepare_failed"))?;
    let rows = stmt.query_map(params![kind,text,like,limit,offset], |row| {
        let payload = serde_json::from_str::<Value>(&row.get::<_,String>(2)?).unwrap_or(Value::Null);
        Ok(if compact {
            json!({"entity_id":row.get::<_,String>(0)?,"kind":row.get::<_,String>(1)?,"title":payload.get("title"),"event_id":row.get::<_,String>(3)?})
        } else {
            json!({"entity_id":row.get::<_,String>(0)?,"kind":row.get::<_,String>(1)?,"payload":payload,"event_id":row.get::<_,String>(3)?})
        })
    }).map_err(db_error("projection_query_failed"))?;
    let items = rows
        .collect::<Result<Vec<_>, _>>()
        .map_err(db_error("projection_query_row_failed"))?;
    Ok(
        json!({"schema":"narada.epistemic.query.v1","status":"ok","result_kind":"entities","compact":compact,"items":items,"offset":offset,"limit":limit,"returned":items.len(),"bounded":true}),
    )
}

fn query_batch(root: &Path, args: &Map<String, Value>) -> Result<Value, Value> {
    let queries = args
        .get("queries")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            error(
                "invalid_batch_query",
                "queries must be an array",
                Value::Null,
            )
        })?;
    if queries.is_empty() || queries.len() > MAX_BATCH_QUERIES {
        return Err(error(
            "invalid_batch_query",
            "queries count must be between 1 and 20",
            json!({"count":queries.len()}),
        ));
    }
    let limit = args
        .get("limit_per_query")
        .and_then(Value::as_u64)
        .unwrap_or(5)
        .min(20);
    let mut results = Vec::with_capacity(queries.len());
    for (index, item) in queries.iter().enumerate() {
        let item = item.as_object().ok_or_else(|| {
            error(
                "invalid_batch_query",
                "each query must be an object",
                json!({"index":index}),
            )
        })?;
        let mut query_args = item.clone();
        query_args.insert("compact".into(), json!(true));
        query_args.insert("limit".into(), json!(limit));
        query_args.insert("offset".into(), json!(0));
        let result = query(root, &query_args)?;
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
        "schema":"narada.epistemic.query_batch.v1",
        "status":"ok",
        "query_count":queries.len(),
        "limit_per_query":limit,
        "results":results,
        "bounded":true
    }))
}

fn source_inspect(root: &Path, args: &Map<String, Value>) -> Result<Value, Value> {
    let paths = args.get("paths").and_then(Value::as_array).ok_or_else(|| {
        error(
            "invalid_source_inspection",
            "paths must be an array",
            Value::Null,
        )
    })?;
    if paths.is_empty() || paths.len() > MAX_SOURCE_FILES {
        return Err(error(
            "invalid_source_inspection",
            "paths count must be between 1 and 20",
            json!({"count":paths.len()}),
        ));
    }
    let max_sections = args
        .get("max_sections_per_file")
        .and_then(Value::as_u64)
        .unwrap_or(20)
        .min(50) as usize;
    let max_chars = args
        .get("max_chars_per_section")
        .and_then(Value::as_u64)
        .unwrap_or(1200)
        .clamp(100, 4000) as usize;
    let canonical_root = fs::canonicalize(root).map_err(io_error("site_root_resolve_failed"))?;
    let relevant = [
        "record",
        "status",
        "epistemic boundary",
        "decision",
        "verdict",
        "scope",
        "next",
        "subsequent",
        "forward",
        "correction",
        "update",
    ];
    let mut files = Vec::with_capacity(paths.len());
    for value in paths {
        let locator = value.as_str().ok_or_else(|| {
            error(
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
        let canonical = fs::canonicalize(&candidate).map_err(io_error("source_resolve_failed"))?;
        if !canonical.starts_with(&canonical_root) {
            return Err(error(
                "source_outside_site",
                "source path must remain inside the site root",
                json!({"path":locator}),
            ));
        }
        let metadata = fs::metadata(&canonical).map_err(io_error("source_metadata_failed"))?;
        if metadata.len() > MAX_SOURCE_BYTES {
            return Err(error(
                "source_too_large",
                "source exceeds the 1 MiB inspection limit",
                json!({"path":locator,"size":metadata.len(),"max_size":MAX_SOURCE_BYTES}),
            ));
        }
        let content = fs::read_to_string(&canonical).map_err(io_error("source_read_failed"))?;
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
        "schema":"narada.epistemic.source_inspection.v1",
        "status":"ok",
        "file_count":files.len(),
        "files":files,
        "bounded":true
    }))
}

fn neighborhood(root: &Path, args: &Map<String, Value>) -> Result<Value, Value> {
    rebuild_projection(root)?;
    let id = required(args, "entity_id")?;
    let limit = args
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(50)
        .min(MAX_PAGE);
    let db = Connection::open(projection_path(root)).map_err(db_error("projection_open_failed"))?;
    let entity: Option<String> = db
        .query_row(
            "select payload_json from entities where entity_id=?1",
            [&id],
            |r| r.get(0),
        )
        .optional()
        .map_err(db_error("projection_entity_read_failed"))?;
    let entity = entity.ok_or_else(|| {
        error(
            "entity_not_found",
            "entity not found",
            json!({"entity_id":id}),
        )
    })?;
    let mut stmt = db.prepare("select relation_id,relation_type,source_id,target_id,payload_json from relations where source_id=?1 or target_id=?1 order by relation_id limit ?2").map_err(db_error("projection_relation_prepare_failed"))?;
    let rows = stmt.query_map(params![id,limit], |r| Ok(json!({"relation_id":r.get::<_,String>(0)?,"relation_type":r.get::<_,String>(1)?,"source_id":r.get::<_,String>(2)?,"target_id":r.get::<_,String>(3)?,"payload":serde_json::from_str::<Value>(&r.get::<_,String>(4)?).unwrap_or(Value::Null)}))).map_err(db_error("projection_relation_query_failed"))?;
    let relations = rows
        .collect::<Result<Vec<_>, _>>()
        .map_err(db_error("projection_relation_row_failed"))?;
    let mut record_stmt = db.prepare("select record_id,record_kind,payload_json,event_id from records where json_extract(payload_json,'$.subject_id')=?1 or json_extract(payload_json,'$.test_id')=?1 order by record_id limit ?2").map_err(db_error("projection_neighborhood_record_prepare_failed"))?;
    let records = record_stmt.query_map(params![id,limit], |r| Ok(json!({"record_id":r.get::<_,String>(0)?,"record_kind":r.get::<_,String>(1)?,"payload":serde_json::from_str::<Value>(&r.get::<_,String>(2)?).unwrap_or(Value::Null),"event_id":r.get::<_,String>(3)?}))).map_err(db_error("projection_neighborhood_record_query_failed"))?.collect::<Result<Vec<_>, _>>().map_err(db_error("projection_neighborhood_record_row_failed"))?;
    Ok(
        json!({"schema":"narada.epistemic.neighborhood.v1","status":"ok","entity":serde_json::from_str::<Value>(&entity).unwrap_or(Value::Null),"relations":relations,"records":records,"limit":limit,"bounded":true}),
    )
}

fn export(root: &Path, args: &Map<String, Value>) -> Result<Value, Value> {
    let format = args.get("format").and_then(Value::as_str).unwrap_or("json");
    let entities = query(root, &Map::from_iter([("limit".into(), json!(100))]))?["items"].clone();
    rebuild_projection(root)?;
    let db = Connection::open(projection_path(root)).map_err(db_error("projection_open_failed"))?;
    let mut stmt = db
        .prepare("select payload_json from relations order by relation_id limit 1000")
        .map_err(db_error("projection_export_prepare_failed"))?;
    let relations = stmt
        .query_map([], |r| {
            Ok(serde_json::from_str::<Value>(&r.get::<_, String>(0)?).unwrap_or(Value::Null))
        })
        .map_err(db_error("projection_export_failed"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(db_error("projection_export_row_failed"))?;
    let mut record_stmt = db
        .prepare("select payload_json from records order by record_id limit 1000")
        .map_err(db_error("projection_export_record_prepare_failed"))?;
    let records = record_stmt
        .query_map([], |r| {
            Ok(serde_json::from_str::<Value>(&r.get::<_, String>(0)?).unwrap_or(Value::Null))
        })
        .map_err(db_error("projection_export_record_failed"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(db_error("projection_export_record_row_failed"))?;
    let context = if format == "jsonld" {
        json!({"prov":"http://www.w3.org/ns/prov#","cito":"http://purl.org/spar/cito/","fabio":"http://purl.org/spar/fabio/","narada":"https://narada.local/epistemic/"})
    } else {
        Value::Null
    };
    Ok(
        json!({"schema":"narada.epistemic.export.v1","format":format,"ledger_head":ledger_head(root)?,"@context":context,"entities":entities,"relations":relations,"records":records,"bounded":true}),
    )
}

fn rebuild_projection(root: &Path) -> Result<(), Value> {
    prepare(root)?;
    verify_ledger(root)?;
    let path = projection_path(root);
    let temporary = path.with_extension("sqlite.next");
    let _ = fs::remove_file(&temporary);
    let mut db = Connection::open(&temporary).map_err(db_error("projection_create_failed"))?;
    db.execute_batch("pragma journal_mode=delete; create table entities(entity_id text primary key,kind text not null,payload_json text not null,event_id text not null); create table relations(relation_id text primary key,relation_type text not null,source_id text not null,target_id text not null,payload_json text not null,event_id text not null); create table records(record_id text primary key,record_kind text not null,payload_json text not null,event_id text not null);").map_err(db_error("projection_schema_failed"))?;
    let tx = db
        .transaction()
        .map_err(db_error("projection_transaction_failed"))?;
    for path in ledger_files(root)? {
        let event = read_json(&path)?;
        let event_id = event["event_id"].as_str().unwrap_or_default();
        for op in event["operations"].as_array().into_iter().flatten() {
            let op_kind = op["op"].as_str().unwrap_or_default();
            match op_kind {
                "entity.declare" => {
                    let id = op["entity_id"].as_str().unwrap();
                    let kind = op["kind"].as_str().unwrap();
                    tx.execute(
                        "insert or replace into entities values(?1,?2,?3,?4)",
                        params![id, kind, op.to_string(), event_id],
                    )
                    .map_err(db_error("projection_entity_write_failed"))?;
                }
                "relation.declare" => {
                    let id = op["relation_id"].as_str().unwrap();
                    let typ = op["relation_type"].as_str().unwrap();
                    let source = op["source_id"].as_str().unwrap();
                    let target = op["target_id"].as_str().unwrap();
                    tx.execute(
                        "insert or replace into relations values(?1,?2,?3,?4,?5,?6)",
                        params![id, typ, source, target, op.to_string(), event_id],
                    )
                    .map_err(db_error("projection_relation_write_failed"))?;
                }
                "assessment.record" | "test_outcome.record" | "sweep.record" => {
                    let id = op
                        .get("assessment_id")
                        .or_else(|| op.get("outcome_id"))
                        .or_else(|| op.get("sweep_id"))
                        .and_then(Value::as_str)
                        .unwrap();
                    tx.execute(
                        "insert or replace into records values(?1,?2,?3,?4)",
                        params![id, op_kind, op.to_string(), event_id],
                    )
                    .map_err(db_error("projection_assessment_write_failed"))?;
                }
                _ => {}
            }
        }
    }
    tx.commit().map_err(db_error("projection_commit_failed"))?;
    drop(db);
    let _ = fs::remove_file(&path);
    fs::rename(&temporary, &path).map_err(io_error("projection_replace_failed"))?;
    Ok(())
}

fn verify_ledger(root: &Path) -> Result<(), Value> {
    let mut expected_previous: Option<String> = None;
    let mut expected_sequence = 1_u64;
    for path in ledger_files(root)? {
        let event = read_json(&path)?;
        if event.get("sequence").and_then(Value::as_u64) != Some(expected_sequence) {
            return Err(error(
                "ledger_sequence_invalid",
                "ledger sequence is not contiguous",
                json!({"path":path.to_string_lossy(),"expected_sequence":expected_sequence,"actual_sequence":event.get("sequence")}),
            ));
        }
        if event.get("previous_hash").and_then(Value::as_str) != expected_previous.as_deref() {
            return Err(error(
                "ledger_chain_invalid",
                "ledger previous_hash does not match",
                json!({"path":path.to_string_lossy(),"expected_previous":expected_previous,"actual_previous":event.get("previous_hash")}),
            ));
        }
        let actual = event
            .get("event_hash")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                error(
                    "ledger_hash_missing",
                    "ledger event_hash is missing",
                    json!({"path":path.to_string_lossy()}),
                )
            })?;
        let mut unhashed = event.clone();
        unhashed.as_object_mut().unwrap().remove("event_hash");
        let computed = digest_value(&unhashed)?;
        if actual != computed {
            return Err(error(
                "ledger_hash_invalid",
                "ledger event_hash does not match content",
                json!({"path":path.to_string_lossy(),"expected_hash":computed,"actual_hash":actual}),
            ));
        }
        expected_previous = Some(actual.to_string());
        expected_sequence += 1;
    }
    Ok(())
}

fn validate_references(root: &Path, operations: &[Value]) -> Result<(), Value> {
    let mut known = std::collections::HashSet::new();
    if projection_path(root).exists() {
        let db =
            Connection::open(projection_path(root)).map_err(db_error("projection_open_failed"))?;
        let mut statement = db
            .prepare("select entity_id from entities")
            .map_err(db_error("projection_reference_prepare_failed"))?;
        let rows = statement
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(db_error("projection_reference_query_failed"))?;
        for row in rows {
            known.insert(row.map_err(db_error("projection_reference_row_failed"))?);
        }
    }
    for operation in operations {
        if operation["op"] == "entity.declare" {
            known.insert(
                operation["entity_id"]
                    .as_str()
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
            Err(error(
                "dangling_reference",
                "operation references an unknown entity",
                json!({"field":field,"entity_id":id,"operation":operation}),
            ))
        }
    };
    for operation in operations {
        match operation["op"].as_str().unwrap_or_default() {
            "relation.declare" => {
                require_known("source_id", operation)?;
                require_known("target_id", operation)?;
            }
            "assessment.record" => {
                require_known("subject_id", operation)?;
            }
            "test_outcome.record" => {
                require_known("test_id", operation)?;
            }
            _ => {}
        }
        for evidence in operation
            .get("evidence")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            require_known("source_id", evidence)?;
            for field in ["locator", "paraphrase"] {
                if evidence
                    .get(field)
                    .and_then(Value::as_str)
                    .filter(|v| !v.trim().is_empty())
                    .is_none()
                {
                    return Err(error(
                        "evidence_location_incomplete",
                        "evidence requires locator and paraphrase",
                        json!({"field":field,"evidence":evidence}),
                    ));
                }
            }
        }
    }
    Ok(())
}

fn validate_operations(ops: &[Value], require_evidence: bool) -> Result<(), Value> {
    let mut declared = std::collections::HashSet::new();
    for op in ops {
        let obj = op.as_object().ok_or_else(|| {
            error(
                "invalid_operation",
                "operation must be an object",
                Value::Null,
            )
        })?;
        let kind = required(obj, "op")?;
        match kind.as_str() {
            "entity.declare" => {
                let id = required(obj, "entity_id")?;
                let typ = required(obj, "kind")?;
                if !ENTITY_KINDS.contains(&typ.as_str()) {
                    return Err(error(
                        "invalid_entity_kind",
                        "unsupported entity kind",
                        json!({"kind":typ}),
                    ));
                }
                required(obj, "title")?;
                if typ == "source" {
                    required(obj, "version")?;
                    required(obj, "locator")?;
                }
                declared.insert(id);
            }
            "relation.declare" => {
                required(obj, "relation_id")?;
                let typ = required(obj, "relation_type")?;
                if !CORE_RELATIONS.contains(&typ.as_str()) && !typ.contains(':') {
                    return Err(error(
                        "invalid_relation_type",
                        "extension relations must be namespaced",
                        json!({
                            "relation_type":typ,
                            "core_relations":CORE_RELATIONS,
                            "extension_pattern":"<namespace>:<relation>",
                            "examples":["marici:refines","marici:generalizes"]
                        }),
                    ));
                }
                required(obj, "source_id")?;
                required(obj, "target_id")?;
            }
            "assessment.record" => {
                required(obj, "assessment_id")?;
                required(obj, "subject_id")?;
                required(obj, "judgment")?;
                required(obj, "actor")?;
                required(obj, "reason")?;
            }
            "test_outcome.record" => {
                required(obj, "outcome_id")?;
                required(obj, "test_id")?;
                required(obj, "actor")?;
                required(obj, "outcome")?;
            }
            "sweep.record" => {
                required(obj, "sweep_id")?;
                required(obj, "interval_start")?;
                required(obj, "interval_end")?;
                required(obj, "method")?;
                required(obj, "result")?;
            }
            _ => {
                return Err(error(
                    "invalid_operation",
                    "unsupported operation",
                    json!({"op":kind}),
                ))
            }
        }
        if require_evidence
            && matches!(kind.as_str(), "assessment.record" | "test_outcome.record")
            && obj
                .get("evidence")
                .and_then(Value::as_array)
                .map(|v| v.is_empty())
                .unwrap_or(true)
        {
            return Err(error(
                "evidence_required",
                "assessment and outcome records require evidence",
                json!({"op":kind}),
            ));
        }
    }
    let _ = declared;
    Ok(())
}

fn prepare(root: &Path) -> Result<(), Value> {
    fs::create_dir_all(ledger(root)).map_err(io_error("ledger_create_failed"))?;
    fs::create_dir_all(proposals(root)).map_err(io_error("proposal_store_create_failed"))?;
    fs::create_dir_all(runtime(root)).map_err(io_error("projection_root_create_failed"))?;
    Ok(())
}
fn control(root: &Path) -> PathBuf {
    if root.file_name().and_then(|v| v.to_str()) == Some(".narada") {
        root.to_path_buf()
    } else {
        root.join(".narada")
    }
}
fn ledger(root: &Path) -> PathBuf {
    control(root).join("epistemic/ledger")
}
fn proposals(root: &Path) -> PathBuf {
    control(root).join("epistemic/proposals")
}
fn runtime(root: &Path) -> PathBuf {
    control(root).join(".ai/epistemic-graph")
}
fn projection_path(root: &Path) -> PathBuf {
    runtime(root).join("projection.sqlite")
}
fn ledger_files(root: &Path) -> Result<Vec<PathBuf>, Value> {
    if !ledger(root).exists() {
        return Ok(vec![]);
    }
    let mut files = fs::read_dir(ledger(root))
        .map_err(io_error("ledger_read_failed"))?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| {
            p.extension().and_then(|v| v.to_str()) == Some("json")
                && p.file_name()
                    .and_then(|v| v.to_str())
                    .map(|v| v.starts_with("ev-"))
                    .unwrap_or(false)
        })
        .collect::<Vec<_>>();
    files.sort();
    Ok(files)
}
fn ledger_head(root: &Path) -> Result<Option<String>, Value> {
    let Some(path) = ledger_files(root)?.last().cloned() else {
        return Ok(None);
    };
    Ok(read_json(&path)?["event_hash"].as_str().map(str::to_string))
}
fn load_proposal(root: &Path, id: &str) -> Result<Value, Value> {
    read_json(&proposals(root).join(format!("{}.json", safe_name(id))))
}
fn read_json(path: &Path) -> Result<Value, Value> {
    let bytes = fs::read(path).map_err(io_error("record_read_failed"))?;
    serde_json::from_slice(&bytes).map_err(|e| {
        error(
            "record_invalid_json",
            &e.to_string(),
            json!({"path":path.to_string_lossy()}),
        )
    })
}
fn write_new_json(path: &Path, value: &Value) -> Result<(), Value> {
    write_new(
        path,
        format!(
            "{}\n",
            serde_json::to_string_pretty(value).map_err(|e| error(
                "json_encode_failed",
                &e.to_string(),
                Value::Null
            ))?
        )
        .as_bytes(),
    )
}
fn write_replace_json(path: &Path, value: &Value) -> Result<(), Value> {
    fs::write(
        path,
        format!("{}\n", serde_json::to_string_pretty(value).unwrap()),
    )
    .map_err(io_error("record_write_failed"))
}
fn write_new(path: &Path, bytes: &[u8]) -> Result<(), Value> {
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(path)
        .map_err(io_error("immutable_record_exists"))?;
    file.write_all(bytes)
        .map_err(io_error("record_write_failed"))?;
    file.sync_all().map_err(io_error("record_sync_failed"))
}
fn digest_value(value: &Value) -> Result<String, Value> {
    let encoded = serde_json::to_vec(value)
        .map_err(|e| error("json_encode_failed", &e.to_string(), Value::Null))?;
    Ok(format!("{:x}", Sha256::digest(encoded)))
}
fn now() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_default()
}
fn safe_name(v: &str) -> String {
    v.chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .take(120)
        .collect()
}
fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn required(args: &Map<String, Value>, key: &str) -> Result<String, Value> {
    args.get(key)
        .and_then(Value::as_str)
        .filter(|v| !v.trim().is_empty())
        .map(str::to_string)
        .ok_or_else(|| {
            error(
                "required_argument_missing",
                &format!("required_argument_missing:{key}"),
                json!({"field":key}),
            )
        })
}
fn guidance() -> Value {
    json!({
        "schema":"narada.epistemic.guidance.v2",
        "purpose":"Preserve evolving problem situations, not certify truth.",
        "workflow":[
            {"step":1,"tool":"epistemic_graph_submit_review_admit","preferred":true,"why":"Perform the ordinary submit, preserved policy review, and admission workflow atomically. Omit expected_ledger_head to snapshot the current head and omit idempotency_key for deterministic retry safety."},
            {"step":2,"tool":"epistemic_graph_capture_sources","alternative":true,"why":"Create a reviewable source proposal when manual review before admission is intended; operations may be empty for pure source capture."},
            {"step":3,"tool":"epistemic_graph_proposal_submit","alternative":true,"why":"Persist a reviewable proposal without source batching."},
            {"step":4,"tools":["epistemic_graph_proposal_review","epistemic_graph_proposal_admit"],"manual_only":true,"why":"Use separate calls only when the operator wants an explicit pause between proposal, review, and admission."},
            {"step":5,"tool":"epistemic_graph_neighborhood","why":"Verify the admitted problem situation and its relations."}
        ],
        "entity_kinds":ENTITY_KINDS,
        "core_relations":CORE_RELATIONS,
        "extension_relation_rule":"Any relation outside core_relations must be namespaced, for example marici:refines or marici:generalizes.",
        "identity_rule":{"relations":"Omit relation_id to derive it deterministically from relation_type, source_id, and target_id. Supply an override only when parallel duplicate relations are intentional.","idempotency":"Omit idempotency_key for deterministic content-hash retry identity; supply one only to name a wider caller-defined retry scope."},
        "revision_pattern":{"entity_title_correction":"Declare a successor entity with the corrected title and connect it to the prior entity using supersedes. Keep the prior identity as immutable history.","discovery":"Query or inspect the predecessor neighborhood before declaring the successor.","reason":"The graph is append-only; revision is explicit explanation, not silent record mutation."},
        "operation_kinds":["entity.declare","relation.declare","assessment.record","test_outcome.record","sweep.record"],
        "provenance_choices":[
            "Represent a document as a versioned source entity and connect claims with derived_from.",
            "For an assessment or test outcome, include evidence entries with source_id, locator, and paraphrase.",
            "Do not manufacture an assessment merely to attach provenance; conjecture plus derived_from is valid."
        ],
        "minimal_example":{
            "tool":"epistemic_graph_submit_review_admit",
            "arguments":{"actor":"agent-id","authority_basis":{"kind":"operator_request","summary":"Capture one bounded source claim."},"operations":[
                {"op":"entity.declare","local_ref":"source","kind":"source","title":"Example source","version":"1","locator":"src/ledger/example.md"},
                {"op":"entity.declare","local_ref":"conjecture","kind":"conjecture","title":"Example explanatory conjecture"},
                {"op":"relation.declare","relation_type":"derived_from","source_ref":"conjecture","target_ref":"source"}
            ]}
        },
        "concurrency_rule":"Omit expected_ledger_head to snapshot the live head during submission while retaining CAS protection through admission. Supply a concrete status.ledger_head only when an external read must be the concurrency boundary. If review reports stale, query again and submit a new proposal; do not rewrite the immutable proposal.",
        "admission_meaning":"policy-valid contribution; never truth certification",
        "search_boundary":"Use external providers for discovery. Record a sweep only when it explains coverage or changes the graph.",
        "problem_policy":"Transform apparent solutions into successor problems; record closure only as an attributed assessment."
    })
}

fn guidance_with_request(args: &Map<String, Value>) -> Value {
    let mut value = guidance();
    value["requested"] = json!({"workflow":args.get("workflow").cloned().unwrap_or(Value::Null),"tool":args.get("tool").cloned().unwrap_or(Value::Null)});
    value
}
fn non_empty_string() -> Value {
    json!({"type":"string","minLength":1})
}
fn evidence_schema() -> Value {
    json!({"type":"object","properties":{"source_id":non_empty_string(),"locator":non_empty_string(),"paraphrase":non_empty_string()},"required":["source_id","locator","paraphrase"],"additionalProperties":false})
}
fn operation_schema() -> Value {
    json!({"oneOf":[
        {"title":"Declare entity","type":"object","properties":{"op":{"const":"entity.declare"},"entity_id":{"type":"string","minLength":1,"description":"Optional override; omit for deterministic identity from kind, title, version, and locator."},"local_ref":{"type":"string","minLength":1,"description":"Optional proposal-local name for relation source_ref/target_ref."},"kind":{"type":"string","enum":ENTITY_KINDS},"title":non_empty_string(),"version":non_empty_string(),"locator":non_empty_string()},"required":["op","kind","title"],"allOf":[{"if":{"properties":{"kind":{"const":"source"}},"required":["kind"]},"then":{"required":["version","locator"]}}],"additionalProperties":true},
        {"title":"Declare relation","type":"object","properties":{"op":{"const":"relation.declare"},"relation_id":{"type":"string","minLength":1,"description":"Optional override. Omit to derive a deterministic id from relation_type, source_id, and target_id."},"relation_type":{"oneOf":[{"type":"string","enum":CORE_RELATIONS},{"type":"string","pattern":"^[A-Za-z][A-Za-z0-9_.-]*:[A-Za-z][A-Za-z0-9_.-]*$"}],"description":"Use a listed core relation, or namespace an extension such as marici:refines."},"source_id":non_empty_string(),"target_id":non_empty_string(),"source_ref":non_empty_string(),"target_ref":non_empty_string()},"required":["op","relation_type"],"allOf":[{"anyOf":[{"required":["source_id"]},{"required":["source_ref"]}]},{"anyOf":[{"required":["target_id"]},{"required":["target_ref"]}]}],"additionalProperties":true},
        {"title":"Record assessment","type":"object","properties":{"op":{"const":"assessment.record"},"assessment_id":non_empty_string(),"subject_id":non_empty_string(),"judgment":non_empty_string(),"actor":non_empty_string(),"reason":non_empty_string(),"evidence":{"type":"array","minItems":1,"items":evidence_schema()}},"required":["op","assessment_id","subject_id","judgment","actor","reason","evidence"],"additionalProperties":true},
        {"title":"Record test outcome","type":"object","properties":{"op":{"const":"test_outcome.record"},"outcome_id":non_empty_string(),"test_id":non_empty_string(),"actor":non_empty_string(),"outcome":non_empty_string(),"evidence":{"type":"array","minItems":1,"items":evidence_schema()}},"required":["op","outcome_id","test_id","actor","outcome","evidence"],"additionalProperties":true},
        {"title":"Record bounded search sweep","type":"object","properties":{"op":{"const":"sweep.record"},"sweep_id":non_empty_string(),"interval_start":non_empty_string(),"interval_end":non_empty_string(),"method":non_empty_string(),"result":non_empty_string()},"required":["op","sweep_id","interval_start","interval_end","method","result"],"additionalProperties":true}
    ]})
}
fn proposal_schema() -> Value {
    json!({"type":"object","properties":{"actor":non_empty_string(),"authority_basis":{"type":"object","description":"Why this actor may propose the contribution.","minProperties":1},"idempotency_key":{"type":"string","minLength":1,"description":"Optional override; omitted values use deterministic content-hash idempotency."},"expected_ledger_head":{"type":["string","null"],"description":"Optional explicit CAS boundary; omitted means the current head. Use null only for an empty graph."},"operations":{"type":"array","minItems":1,"maxItems":200,"items":operation_schema()}},"required":["actor","authority_basis","operations"],"additionalProperties":false})
}
fn capture_sources_schema() -> Value {
    json!({"type":"object","properties":{
        "actor":non_empty_string(),
        "authority_basis":{"type":"object","description":"Why this actor may propose the contribution.","minProperties":1},
        "idempotency_key":{"type":"string","minLength":1,"description":"Optional override; omitted values use deterministic content-hash idempotency."},
        "expected_ledger_head":{"type":["string","null"],"description":"Optional explicit CAS boundary; omitted means the current head. Use null only for an empty graph."},
        "sources":{"type":"array","minItems":1,"maxItems":100,"items":{"type":"object","properties":{"source_id":non_empty_string(),"title":non_empty_string(),"version":non_empty_string(),"locator":non_empty_string()},"required":["source_id","title","version","locator"],"additionalProperties":false}},
        "operations":{"type":"array","maxItems":199,"default":[],"items":operation_schema()}
    },"required":["actor","authority_basis","sources"],"additionalProperties":false})
}
fn proposal_id_schema() -> Value {
    json!({"type":"object","properties":{"proposal_id":{"type":"string"}},"required":["proposal_id"],"additionalProperties":false})
}
fn object(required: &[&str]) -> Value {
    json!({"type":"object","required":required,"additionalProperties":false})
}
fn tool(name: &str, description: &str, input: Value, read_only: bool) -> Value {
    json!({"name":name,"description":description,"inputSchema":input,"annotations":{"readOnlyHint":read_only,"destructiveHint":false,"idempotentHint":read_only}})
}
fn error(code: &str, message: &str, details: Value) -> Value {
    json!({"schema":"narada.epistemic.error.v1","code":code,"message":message,"details":details})
}
fn io_error(code: &'static str) -> impl FnOnce(std::io::Error) -> Value {
    move |e| error(code, &e.to_string(), Value::Null)
}
fn db_error(code: &'static str) -> impl FnOnce(rusqlite::Error) -> Value {
    move |e| error(code, &e.to_string(), Value::Null)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn proposal_tool_schema_describes_every_operation_shape() {
        let schema = proposal_schema();
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
        let value = guidance();
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
        let tool = list_tools().into_iter().find(|tool| tool["name"] == "epistemic_graph_guidance").unwrap();
        assert_eq!(tool["inputSchema"]["additionalProperties"], false);
        assert_eq!(tool["inputSchema"]["properties"]["workflow"]["type"], "string");
        let value = guidance_with_request(json!({"workflow":"query_current_frontier"}).as_object().unwrap());
        assert_eq!(value["requested"]["workflow"], "query_current_frontier");
    }

    #[test]
    fn source_entity_requires_a_version_and_locator() {
        let operation = json!({"op":"entity.declare","entity_id":"source:unlocated","kind":"source","title":"Unlocated source","version":"1"});
        let failure =
            validate_operations(&[operation], false).expect_err("unlocated source must refuse");
        assert_eq!(failure["code"], "required_argument_missing");
        assert_eq!(failure["details"]["field"], "locator");
    }

    #[test]
    fn source_inspection_returns_all_relevant_sections_with_line_ranges() {
        let root = std::env::temp_dir().join(format!("epistemic-source-test-{}", Uuid::new_v4()));
        fs::create_dir_all(root.join("ledger")).expect("ledger directory");
        fs::write(
            root.join("ledger/example.md"),
            "# Example\n\n## Record\nA\n\n## Decision\nB\n\n## Subsequent Update\nC\n",
        )
        .expect("source");
        let result = source_inspect(
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
        let root = std::env::temp_dir().join(format!("epistemic-batch-test-{}", Uuid::new_v4()));
        let proposal = proposal_submit(
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
        let resubmitted = proposal_resubmit(
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
        let page = proposal_read(
            &root,
            &Map::from_iter([("proposal_id".into(), resubmitted["proposal_id"].clone())]),
        )
        .expect("read resubmission");
        assert_eq!(page["operations"][0]["entity_id"], "claim:keep");
        assert_eq!(page["operations"][1]["entity_id"], "claim:replacement");

        proposal_admit(
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
        let result = query_batch(
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
        let root = std::env::temp_dir().join(format!("epistemic-record-test-{}", Uuid::new_v4()));
        let operations = json!([
            {"op":"entity.declare","entity_id":"source:record-test","kind":"source","title":"Record test source","version":"1","locator":"ledger/test.md"},
            {"op":"entity.declare","entity_id":"test:record-test","kind":"test","title":"Record test"},
            {"op":"assessment.record","assessment_id":"assessment:record-test","subject_id":"test:record-test","judgment":"conditional","actor":"tester","reason":"Some gates remain open.","evidence":[{"source_id":"source:record-test","locator":"Current status","paraphrase":"The source reports a conditional result."}]}
        ]);
        let proposal = proposal_submit(
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
        proposal_admit(
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
        let records = query(
            &root,
            &Map::from_iter([("record_kind".into(), json!("assessment.record"))]),
        )
        .expect("record query");
        assert_eq!(records["returned"], 1);
        assert_eq!(status(&root).expect("status")["record_count"], 1);
        assert_eq!(
            neighborhood(
                &root,
                &Map::from_iter([("entity_id".into(), json!("test:record-test"))])
            )
            .expect("neighborhood")["records"]
                .as_array()
                .map(Vec::len),
            Some(1)
        );
        assert_eq!(
            export(&root, &Map::new()).expect("export")["records"]
                .as_array()
                .map(Vec::len),
            Some(1)
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn proposal_submission_is_compact_and_explicit_reads_are_bounded() {
        let root =
            std::env::temp_dir().join(format!("epistemic-proposal-read-test-{}", Uuid::new_v4()));
        let operations = (0..MAX_OPERATIONS)
            .map(|index| json!({"op":"entity.declare","entity_id":format!("claim:{index}"),"kind":"claim","title":format!("Claim {index}")}))
            .collect::<Vec<_>>();
        let receipt = proposal_submit(
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
        assert_eq!(receipt["operation_count"], MAX_OPERATIONS);
        assert!(receipt.get("operations").is_none());
        assert!(
            serde_json::to_vec(&receipt)
                .expect("serialize receipt")
                .len()
                < 1024
        );

        let first = proposal_read(
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

        let final_page = proposal_read(
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
        let root = std::env::temp_dir().join(format!("epistemic-test-{}", Uuid::new_v4()));
        let proposal=proposal_submit(&root,&Map::from_iter([("actor".into(),json!("nima")),("authority_basis".into(),json!({"kind":"operator_request"})),("operations".into(),json!([{"op":"entity.declare","entity_id":"problem-1","kind":"problem","title":"What explains X?"}]))])).unwrap();
        assert_eq!(
            proposal["schema"],
            "narada.epistemic.proposal_submission.v1"
        );
        assert_eq!(proposal["operation_count"], 1);
        assert!(proposal.get("operations").is_none());
        let id = proposal["proposal_id"].as_str().unwrap();
        let event = proposal_admit(
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
        let retry = proposal_admit(
            &root,
            &Map::from_iter([
                ("proposal_id".into(), json!(id)),
                ("actor".into(), json!("nima")),
                ("authority_basis".into(), json!({"kind":"operator_request"})),
            ]),
        )
        .expect("deterministic admission retry");
        assert_eq!(retry["event_id"], event["event_id"]);
        let admitted = proposal_read(&root, &Map::from_iter([("proposal_id".into(), json!(id))]))
            .expect("admitted proposal readback");
        assert_eq!(admitted["status"], "admitted");
        assert_eq!(admitted["lifecycle"]["event_id"], event["event_id"]);
        assert_eq!(admitted["lifecycle"]["ledger_head"], event["ledger_head"]);
        let result = query(&root, &Map::new()).unwrap();
        assert_eq!(result["returned"], 1);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn source_capture_builds_a_compact_deduplicated_draft_without_admitting_it() {
        let root = std::env::temp_dir().join(format!("epistemic-capture-test-{}", Uuid::new_v4()));
        let seed = proposal_submit(
            &root,
            &Map::from_iter([
                ("actor".into(), json!("tester")),
                ("authority_basis".into(), json!({"kind":"test"})),
                ("idempotency_key".into(), json!("seed-p1")),
                ("expected_ledger_head".into(), Value::Null),
                ("operations".into(), json!([{"op":"entity.declare","entity_id":"claim:existing","kind":"claim","title":"Existing claim"}])),
            ]),
        ).expect("seed proposal");
        let seed_event = proposal_admit(
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
        let capture = capture_sources(
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
        assert_eq!(ledger_files(&root).expect("ledger").len(), 1);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn claim_entities_and_compact_queries_preserve_epistemic_attribution() {
        let root = std::env::temp_dir().join(format!("epistemic-claim-test-{}", Uuid::new_v4()));
        let proposal = proposal_submit(
            &root,
            &Map::from_iter([
                ("actor".into(), json!("tester")),
                ("authority_basis".into(), json!({"kind":"test"})),
                ("idempotency_key".into(), json!("claim-p1")),
                ("expected_ledger_head".into(), Value::Null),
                ("operations".into(), json!([{"op":"entity.declare","entity_id":"claim:tree-result","kind":"claim","title":"Attributed theorem result"}])),
            ]),
        ).expect("claim proposal");
        proposal_admit(
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
        let result = query(&root, &Map::from_iter([("compact".into(), json!(true))]))
            .expect("compact query");
        assert_eq!(result["compact"], true);
        assert_eq!(result["items"][0]["entity_id"], "claim:tree-result");
        assert_eq!(result["items"][0]["title"], "Attributed theorem result");
        assert!(result["items"][0].get("payload").is_none());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn projection_refuses_a_tampered_authority_event() {
        let root = std::env::temp_dir().join(format!("epistemic-test-{}", Uuid::new_v4()));
        let proposal = proposal_submit(
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
        proposal_admit(
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
        let path = ledger_files(&root).unwrap().remove(0);
        let mut event = read_json(&path).unwrap();
        event["actor"] = json!("tampered");
        fs::write(&path, serde_json::to_vec_pretty(&event).unwrap()).unwrap();
        let failure = rebuild_projection(&root).unwrap_err();
        assert_eq!(failure["code"], "ledger_hash_invalid");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn pure_source_capture_needs_no_placeholder_operation() {
        let root = std::env::temp_dir().join(format!("epistemic-source-only-{}", Uuid::new_v4()));
        let result = capture_sources(
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
        let first = submit_review_admit(&root, &args).expect("compound admission");
        assert_eq!(first["review"]["status"], "policy_valid");
        assert_eq!(first["admission"]["status"], "admitted");
        let proposal =
            load_proposal(&root, first["submission"]["proposal_id"].as_str().unwrap()).unwrap();
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
        let retried = submit_review_admit(&root, &args).expect("idempotent compound retry");
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
}
