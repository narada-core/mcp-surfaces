use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;
use uuid::Uuid;

const ENTITY_KINDS: &[&str] = &["problem", "conjecture", "criticism", "test", "source"];
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
            "List bounded graph entities or relations.",
            json!({"type":"object","properties":{"kind":{"type":"string"},"text":{"type":"string"},"limit":{"type":"integer","minimum":1,"maximum":100},"offset":{"type":"integer","minimum":0}},"additionalProperties":false}),
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
            "Persist an immutable atomic proposal.",
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
        return read_json(&proposals(root).join(format!("{}.json", existing.trim())));
    }
    write_new_json(
        &proposals(root).join(format!("{proposal_id}.json")),
        &stored,
    )?;
    write_new(&idem_path, proposal_id.as_bytes())?;
    Ok(stored)
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
        return read_json(&ledger(root).join(format!("{}.json", event_id.trim())));
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
    Ok(event)
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
    Ok(
        json!({"schema":"narada.epistemic.status.v1","status":"ok","implementation":"rust-native","canonical_store":ledger(root).to_string_lossy(),"projection":projection_path(root).to_string_lossy(),"ledger_head":ledger_head(root)?,"event_count":ledger_files(root)?.len(),"entity_count":entities,"relation_count":relations,"projection_rebuildable":true,"truth_certification":false}),
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
    let text = args.get("text").and_then(Value::as_str).unwrap_or("");
    let like = format!("%{text}%");
    let mut stmt = db.prepare("select entity_id,kind,payload_json,event_id from entities where (?1='' or kind=?1) and (?2='' or payload_json like ?3) order by entity_id limit ?4 offset ?5").map_err(db_error("projection_query_prepare_failed"))?;
    let rows = stmt.query_map(params![kind,text,like,limit,offset], |row| Ok(json!({"entity_id":row.get::<_,String>(0)?,"kind":row.get::<_,String>(1)?,"payload":serde_json::from_str::<Value>(&row.get::<_,String>(2)?).unwrap_or(Value::Null),"event_id":row.get::<_,String>(3)?}))).map_err(db_error("projection_query_failed"))?;
    let items = rows
        .collect::<Result<Vec<_>, _>>()
        .map_err(db_error("projection_query_row_failed"))?;
    Ok(
        json!({"schema":"narada.epistemic.query.v1","status":"ok","items":items,"offset":offset,"limit":limit,"returned":items.len(),"bounded":true}),
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
    Ok(
        json!({"schema":"narada.epistemic.neighborhood.v1","status":"ok","entity":serde_json::from_str::<Value>(&entity).unwrap_or(Value::Null),"relations":relations,"limit":limit,"bounded":true}),
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
    let context = if format == "jsonld" {
        json!({"prov":"http://www.w3.org/ns/prov#","cito":"http://purl.org/spar/cito/","fabio":"http://purl.org/spar/fabio/","narada":"https://narada.local/epistemic/"})
    } else {
        Value::Null
    };
    Ok(
        json!({"schema":"narada.epistemic.export.v1","format":format,"ledger_head":ledger_head(root)?,"@context":context,"entities":entities,"relations":relations,"bounded":true}),
    )
}

fn rebuild_projection(root: &Path) -> Result<(), Value> {
    prepare(root)?;
    let path = projection_path(root);
    let temporary = path.with_extension("sqlite.next");
    let _ = fs::remove_file(&temporary);
    let mut db = Connection::open(&temporary).map_err(db_error("projection_create_failed"))?;
    db.execute_batch("pragma journal_mode=delete; create table entities(entity_id text primary key,kind text not null,payload_json text not null,event_id text not null); create table relations(relation_id text primary key,relation_type text not null,source_id text not null,target_id text not null,payload_json text not null,event_id text not null); create table assessments(assessment_id text primary key,payload_json text not null,event_id text not null);").map_err(db_error("projection_schema_failed"))?;
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
                        "insert or replace into assessments values(?1,?2,?3)",
                        params![id, op.to_string(), event_id],
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
    json!({"schema":"narada.epistemic.guidance.v1","purpose":"Preserve evolving problem situations, not certify truth.","workflow":["query the current problem neighborhood","submit an atomic evidence-located proposal","review structural and provenance policy","admit the policy-valid record","record later criticism or supersession instead of rewriting history"],"entity_kinds":ENTITY_KINDS,"core_relations":CORE_RELATIONS,"admission_meaning":"policy-valid contribution; never truth certification","search_boundary":"Use external providers for discovery. Record a sweep only when it explains coverage or changes the graph.","problem_policy":"Transform apparent solutions into successor problems; record closure only as an attributed assessment."})
}
fn proposal_schema() -> Value {
    json!({"type":"object","properties":{"actor":{"type":"string"},"authority_basis":{"type":"object"},"idempotency_key":{"type":"string"},"expected_ledger_head":{"type":["string","null"]},"operations":{"type":"array","minItems":1,"maxItems":200,"items":{"type":"object"}}},"required":["actor","authority_basis","idempotency_key","expected_ledger_head","operations"],"additionalProperties":false})
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
    fn proposal_admission_rebuilds_projection_and_preserves_truth_boundary() {
        let root = std::env::temp_dir().join(format!("epistemic-test-{}", Uuid::new_v4()));
        let proposal=proposal_submit(&root,&Map::from_iter([("actor".into(),json!("nima")),("authority_basis".into(),json!({"kind":"operator_request"})),("idempotency_key".into(),json!("p1")),("expected_ledger_head".into(),Value::Null),("operations".into(),json!([{"op":"entity.declare","entity_id":"problem-1","kind":"problem","title":"What explains X?"}]))])).unwrap();
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
        assert_eq!(event["certifies_truth"], false);
        let result = query(&root, &Map::new()).unwrap();
        assert_eq!(result["returned"], 1);
        let _ = fs::remove_dir_all(root);
    }
}
