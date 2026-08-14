use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;
use uuid::Uuid;

const ENTITY_KINDS: &[&str] = &["problem", "conjecture", "claim", "criticism", "test", "source"];
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

pub fn list_tools() -> Vec<Value> {
    vec![
        tool(
            "epistemic_graph_guidance",
            "Explain the problem-situation graph workflow.",
            object(&[]),
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
            "epistemic_graph_proposal_review",
            "Validate an immutable proposal without asserting truth.",
            proposal_id_schema(),
            false,
        ),
        tool(
            "epistemic_graph_proposal_admit",
            "Append a policy-valid proposal to canonical history.",
            json!({"type":"object","properties":{"proposal_id":{"type":"string"},"actor":{"type":"string"},"authority_basis":{"type":"object"},"expected_ledger_head":{"type":["string","null"]},"idempotency_key":{"type":"string"}},"required":["proposal_id","actor","authority_basis","expected_ledger_head","idempotency_key"],"additionalProperties":false}),
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
        "epistemic_graph_guidance" => Ok(guidance()),
        "epistemic_graph_status" => status(site_root),
        "epistemic_graph_query" => query(site_root, args),
        "epistemic_graph_neighborhood" => neighborhood(site_root, args),
        "epistemic_graph_proposal_submit" => proposal_submit(site_root, args),
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
    let idempotency_key = required(args, "idempotency_key")?;
    let operations = args
        .get("operations")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            error(
                "invalid_proposal",
                "operations must be an array",
                Value::Null,
            )
        })?;
    if operations.is_empty() || operations.len() > MAX_OPERATIONS {
        return Err(error(
            "invalid_proposal",
            "operations count must be between 1 and 200",
            json!({"count":operations.len()}),
        ));
    }
    validate_operations(operations, false)?;
    let expected = args
        .get("expected_ledger_head")
        .cloned()
        .unwrap_or(Value::Null);
    let proposal_id = format!("ep_{}", Uuid::new_v4());
    let created_at = now();
    let payload = json!({
        "schema":"narada.epistemic.proposal.v1", "proposal_id":proposal_id,
        "status":"submitted", "actor":actor, "authority_basis":args.get("authority_basis"),
        "idempotency_key":idempotency_key, "expected_ledger_head":expected,
        "created_at":created_at, "operations":operations
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
        return Ok(proposal_receipt(&stored));
    }
    write_new_json(
        &proposals(root).join(format!("{proposal_id}.json")),
        &stored,
    )?;
    write_new(&idem_path, proposal_id.as_bytes())?;
    Ok(proposal_receipt(&stored))
}

fn proposal_receipt(proposal: &Value) -> Value {
    json!({
        "schema":"narada.epistemic.proposal_submission.v1",
        "status":proposal["status"],
        "proposal_id":proposal["proposal_id"],
        "proposal_digest":proposal["digest"],
        "operation_count":proposal["operations"].as_array().map_or(0, Vec::len),
        "expected_ledger_head":proposal["expected_ledger_head"],
        "created_at":proposal["created_at"]
    })
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
    let idem = required(args, "idempotency_key")?;
    let proposal = load_proposal(root, &id)?;
    let review = proposal_review(root, &Map::from_iter([("proposal_id".into(), json!(id))]))?;
    if review["status"] != "policy_valid" {
        return Err(error(
            "proposal_not_admissible",
            "proposal review is not policy_valid",
            review,
        ));
    }
    let expected = args.get("expected_ledger_head").and_then(Value::as_str);
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
    let idem_path = ledger(root).join(format!("idem-{}.txt", safe_name(&idem)));
    if idem_path.exists() {
        let event_id =
            fs::read_to_string(&idem_path).map_err(io_error("ledger_idempotency_read_failed"))?;
        let event = read_json(&ledger(root).join(format!("{}.json", event_id.trim())))?;
        return Ok(admission_receipt(&event));
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
    let compact = args.get("compact").and_then(Value::as_bool).unwrap_or(false);
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
        let items = rows.collect::<Result<Vec<_>, _>>().map_err(db_error("projection_record_query_row_failed"))?;
        return Ok(json!({"schema":"narada.epistemic.query.v1","status":"ok","result_kind":"records","record_kind":record_kind,"compact":compact,"items":items,"offset":offset,"limit":limit,"returned":items.len(),"bounded":true}));
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
    let mut record_stmt = db.prepare("select payload_json from records order by record_id limit 1000").map_err(db_error("projection_export_record_prepare_failed"))?;
    let records = record_stmt.query_map([], |r| Ok(serde_json::from_str::<Value>(&r.get::<_,String>(0)?).unwrap_or(Value::Null))).map_err(db_error("projection_export_record_failed"))?.collect::<Result<Vec<_>, _>>().map_err(db_error("projection_export_record_row_failed"))?;
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
                        json!({"relation_type":typ}),
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
            {"step":1,"tool":"epistemic_graph_status","why":"Read the current ledger_head for optimistic concurrency."},
            {"step":2,"tool":"epistemic_graph_query","why":"Avoid duplicate entities and inspect the current problem neighborhood."},
            {"step":3,"tool":"epistemic_graph_proposal_submit","why":"Persist one atomic, idempotent set of typed operations."},
            {"step":4,"tool":"epistemic_graph_proposal_review","why":"Validate schema, references, provenance locations, invariants, and ledger head without asserting truth."},
            {"step":5,"tool":"epistemic_graph_proposal_admit","why":"Append a policy-valid proposal to immutable canonical history."},
            {"step":6,"tool":"epistemic_graph_neighborhood","why":"Verify the admitted problem situation and its relations."}
        ],
        "entity_kinds":ENTITY_KINDS,
        "core_relations":CORE_RELATIONS,
        "operation_kinds":["entity.declare","relation.declare","assessment.record","test_outcome.record","sweep.record"],
        "provenance_choices":[
            "Represent a document as a versioned source entity and connect claims with derived_from.",
            "For an assessment or test outcome, include evidence entries with source_id, locator, and paraphrase.",
            "Do not manufacture an assessment merely to attach provenance; conjecture plus derived_from is valid."
        ],
        "minimal_example":{
            "submit":{"actor":"agent-id","authority_basis":{"kind":"operator_request","summary":"Capture one bounded source claim."},"idempotency_key":"stable-capture-key-v1","expected_ledger_head":null,"operations":[
                {"op":"entity.declare","entity_id":"source:example-v1","kind":"source","title":"Example source","version":"1","locator":"src/ledger/example.md"},
                {"op":"entity.declare","entity_id":"conjecture:example","kind":"conjecture","title":"Example explanatory conjecture"},
                {"op":"relation.declare","relation_id":"rel:example-derived-from-source","relation_type":"derived_from","source_id":"conjecture:example","target_id":"source:example-v1"}
            ]},
            "review":{"proposal_id":"<proposal_id from submit>"},
            "admit":{"proposal_id":"<proposal_id>","actor":"agent-id","authority_basis":{"kind":"operator_request","summary":"Admit the reviewed contribution; this does not certify truth."},"expected_ledger_head":null,"idempotency_key":"stable-admission-key-v1"}
        },
        "concurrency_rule":"Use status.ledger_head as expected_ledger_head. For an empty graph use null. If review reports stale, query again and submit a new proposal; do not rewrite the immutable proposal.",
        "admission_meaning":"policy-valid contribution; never truth certification",
        "search_boundary":"Use external providers for discovery. Record a sweep only when it explains coverage or changes the graph.",
        "problem_policy":"Transform apparent solutions into successor problems; record closure only as an attributed assessment."
    })
}
fn non_empty_string() -> Value {
    json!({"type":"string","minLength":1})
}
fn evidence_schema() -> Value {
    json!({"type":"object","properties":{"source_id":non_empty_string(),"locator":non_empty_string(),"paraphrase":non_empty_string()},"required":["source_id","locator","paraphrase"],"additionalProperties":false})
}
fn operation_schema() -> Value {
    json!({"oneOf":[
        {"title":"Declare entity","type":"object","properties":{"op":{"const":"entity.declare"},"entity_id":non_empty_string(),"kind":{"type":"string","enum":ENTITY_KINDS},"title":non_empty_string(),"version":non_empty_string(),"locator":non_empty_string()},"required":["op","entity_id","kind","title"],"allOf":[{"if":{"properties":{"kind":{"const":"source"}},"required":["kind"]},"then":{"required":["version","locator"]}}],"additionalProperties":true},
        {"title":"Declare relation","type":"object","properties":{"op":{"const":"relation.declare"},"relation_id":non_empty_string(),"relation_type":{"type":"string","description":"One core relation or a namespaced extension such as marici:refines."},"source_id":non_empty_string(),"target_id":non_empty_string()},"required":["op","relation_id","relation_type","source_id","target_id"],"additionalProperties":true},
        {"title":"Record assessment","type":"object","properties":{"op":{"const":"assessment.record"},"assessment_id":non_empty_string(),"subject_id":non_empty_string(),"judgment":non_empty_string(),"actor":non_empty_string(),"reason":non_empty_string(),"evidence":{"type":"array","minItems":1,"items":evidence_schema()}},"required":["op","assessment_id","subject_id","judgment","actor","reason","evidence"],"additionalProperties":true},
        {"title":"Record test outcome","type":"object","properties":{"op":{"const":"test_outcome.record"},"outcome_id":non_empty_string(),"test_id":non_empty_string(),"actor":non_empty_string(),"outcome":non_empty_string(),"evidence":{"type":"array","minItems":1,"items":evidence_schema()}},"required":["op","outcome_id","test_id","actor","outcome","evidence"],"additionalProperties":true},
        {"title":"Record bounded search sweep","type":"object","properties":{"op":{"const":"sweep.record"},"sweep_id":non_empty_string(),"interval_start":non_empty_string(),"interval_end":non_empty_string(),"method":non_empty_string(),"result":non_empty_string()},"required":["op","sweep_id","interval_start","interval_end","method","result"],"additionalProperties":true}
    ]})
}
fn proposal_schema() -> Value {
    json!({"type":"object","properties":{"actor":non_empty_string(),"authority_basis":{"type":"object","description":"Why this actor may propose the contribution.","minProperties":1},"idempotency_key":non_empty_string(),"expected_ledger_head":{"type":["string","null"],"description":"Current status.ledger_head; null only for an empty graph."},"operations":{"type":"array","minItems":1,"maxItems":200,"items":operation_schema()}},"required":["actor","authority_basis","idempotency_key","expected_ledger_head","operations"],"additionalProperties":false})
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
            value.pointer("/minimal_example/submit/operations/0/op"),
            Some(&json!("entity.declare"))
        );
        assert_eq!(
            value.pointer("/minimal_example/submit/operations/2/op"),
            Some(&json!("relation.declare"))
        );
        assert!(value["concurrency_rule"]
            .as_str()
            .unwrap_or_default()
            .contains("ledger_head"));
    }

    #[test]
    fn source_entity_requires_a_version_and_locator() {
        let operation = json!({"op":"entity.declare","entity_id":"source:unlocated","kind":"source","title":"Unlocated source","version":"1"});
        let failure = validate_operations(&[operation], false).expect_err("unlocated source must refuse");
        assert_eq!(failure["code"], "required_argument_missing");
        assert_eq!(failure["details"]["field"], "locator");
    }

    #[test]
    fn admitted_assessments_are_queryable_in_neighborhood_status_and_export() {
        let root = std::env::temp_dir().join(format!("epistemic-record-test-{}", Uuid::new_v4()));
        let operations = json!([
            {"op":"entity.declare","entity_id":"source:record-test","kind":"source","title":"Record test source","version":"1","locator":"ledger/test.md"},
            {"op":"entity.declare","entity_id":"test:record-test","kind":"test","title":"Record test"},
            {"op":"assessment.record","assessment_id":"assessment:record-test","subject_id":"test:record-test","judgment":"conditional","actor":"tester","reason":"Some gates remain open.","evidence":[{"source_id":"source:record-test","locator":"Current status","paraphrase":"The source reports a conditional result."}]}
        ]);
        let proposal = proposal_submit(&root, &Map::from_iter([("actor".into(),json!("tester")),("authority_basis".into(),json!({"kind":"test"})),("idempotency_key".into(),json!("record-p1")),("expected_ledger_head".into(),Value::Null),("operations".into(),operations)])).expect("proposal");
        proposal_admit(&root, &Map::from_iter([("proposal_id".into(),proposal["proposal_id"].clone()),("actor".into(),json!("tester")),("authority_basis".into(),json!({"kind":"test"})),("expected_ledger_head".into(),Value::Null),("idempotency_key".into(),json!("record-a1"))])).expect("admit");
        let records = query(&root, &Map::from_iter([("record_kind".into(),json!("assessment.record"))])).expect("record query");
        assert_eq!(records["returned"], 1);
        assert_eq!(status(&root).expect("status")["record_count"], 1);
        assert_eq!(neighborhood(&root, &Map::from_iter([("entity_id".into(),json!("test:record-test"))])).expect("neighborhood")["records"].as_array().map(Vec::len), Some(1));
        assert_eq!(export(&root, &Map::new()).expect("export")["records"].as_array().map(Vec::len), Some(1));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn proposal_admission_rebuilds_projection_and_preserves_truth_boundary() {
        let root = std::env::temp_dir().join(format!("epistemic-test-{}", Uuid::new_v4()));
        let proposal=proposal_submit(&root,&Map::from_iter([("actor".into(),json!("nima")),("authority_basis".into(),json!({"kind":"operator_request"})),("idempotency_key".into(),json!("p1")),("expected_ledger_head".into(),Value::Null),("operations".into(),json!([{"op":"entity.declare","entity_id":"problem-1","kind":"problem","title":"What explains X?"}]))])).unwrap();
        assert_eq!(proposal["schema"], "narada.epistemic.proposal_submission.v1");
        assert_eq!(proposal["operation_count"], 1);
        assert!(proposal.get("operations").is_none());
        let id = proposal["proposal_id"].as_str().unwrap();
        let event = proposal_admit(
            &root,
            &Map::from_iter([
                ("proposal_id".into(), json!(id)),
                ("actor".into(), json!("nima")),
                ("authority_basis".into(), json!({"kind":"operator_request"})),
                ("expected_ledger_head".into(), Value::Null),
                ("idempotency_key".into(), json!("a1")),
            ]),
        )
        .unwrap();
        assert_eq!(event["schema"], "narada.epistemic.proposal_admission.v1");
        assert_eq!(event["status"], "admitted");
        assert_eq!(event["operation_count"], 1);
        assert!(event.get("operations").is_none());
        assert_eq!(event["ledger_head"].as_str().map(str::len), Some(64));
        assert_eq!(event["certifies_truth"], false);
        let result = query(&root, &Map::new()).unwrap();
        assert_eq!(result["returned"], 1);
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
        ).expect("claim admission");
        let result = query(&root, &Map::from_iter([("compact".into(), json!(true))])).expect("compact query");
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
}
