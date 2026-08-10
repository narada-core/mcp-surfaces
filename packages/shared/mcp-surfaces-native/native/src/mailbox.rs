use rusqlite::{params, Connection, OpenFlags, OptionalExtension};
use serde_json::{json, Map, Value};
use std::fs;
use std::path::{Path, PathBuf};

const MAX_FILES: usize = 5_000;
const MAX_BYTES: u64 = 10 * 1024 * 1024;
const MAX_ROWS: usize = 500;
const DOMAIN_DB_RELATIVE: &str = ".narada/runtime/mailbox-domain/mailbox-domain.db";
const DEFAULT_ROOTS: &[&str] = &[".ai/mailboxes", ".ai/synced-mailboxes", "operator-surfaces/mailboxes"];
const READ_NAMES: &[&str] = &[
    "mailbox_doctor", "mailbox_accounts_list", "mailbox_messages_list", "mailbox_message_show",
    "mailbox_output_show", "mailbox_fact_show", "mailbox_message_fact_find", "mailbox_admission_show",
    "mailbox_search", "mailbox_thread_show", "mailbox_generation_show", "mailbox_outbox_consumer_show",
    "mailbox_outbox_list",
];
const MUTATING_NAMES: &[&str] = &[
    "mailbox_sync_generation", "mailbox_reconcile_first_observations", "mailbox_message_admit",
    "mailbox_outbox_consumer_register", "mailbox_outbox_ack",
];

pub fn list_tools() -> Vec<Value> {
    let mut tools = vec![guidance_tool()];
    for name in READ_NAMES { tools.push(tool(name, "Read the bounded site-local mailbox projection.", schema(name), true)); }
    for name in MUTATING_NAMES { tools.push(tool(name, "Mailbox projection mutation remains owned by the mailbox authority.", json!({"type":"object","additionalProperties":true}), false)); }
    tools
}
pub fn auxiliary(method: &str, params: &Map<String, Value>) -> Result<Value, Value> { match method { "prompts/list" => Ok(json!({"prompts":[{"name":"mailbox_read_workflow","title":"Mailbox Workflow","description":"Inspect finite site-local mailbox projection reads before synchronization or admission.","arguments":[]}]})), "prompts/get" => { if params.get("name").and_then(Value::as_str)!=Some("mailbox_read_workflow"){return Err(error("unknown_prompt","unknown_prompt"));} Ok(json!({"description":"Inspect finite site-local mailbox projection reads before synchronization or admission.","messages":[{"role":"user","content":{"type":"text","text":"Use mailbox_doctor, mailbox_accounts_list, mailbox_messages_list, mailbox_message_show, mailbox_search, and mailbox_thread_show for bounded local reads. Keep sync, admission, and outbox writes with the owning authority."}}]})) }, "completion/complete" => { let values=if params.get("argument").and_then(Value::as_object).and_then(|v|v.get("name")).and_then(Value::as_str)==Some("name"){list_tools().iter().filter_map(|v|v.get("name").cloned()).take(100).collect::<Vec<_>>()}else{Vec::new()}; Ok(json!({"completion":{"values":values,"total":values.len(),"hasMore":false}})) }, "logging/setLevel"=>Ok(json!({})), _=>Err(error("unsupported_mcp_method",&format!("unsupported_mcp_method:{method}"))), } }
pub fn call_tool(name: &str, args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    match name {
        "mailbox_guidance" => Ok(guidance(args)),
        "mailbox_doctor" => Ok(doctor(root)),
        "mailbox_accounts_list" => accounts(root),
        "mailbox_messages_list" => messages(args, root),
        "mailbox_message_show" => message_show(args, root),
        "mailbox_search" => { required(args, "query")?; messages(args, root) },
        "mailbox_thread_show" => thread_show(args, root),
        "mailbox_output_show" => output_show(args, root),
        "mailbox_generation_show" => generation_show(args, root),
        "mailbox_outbox_consumer_show" => outbox_consumer_show(args, root),
        "mailbox_outbox_list" => outbox_list(args, root),
        name if READ_NAMES.contains(&name) => Err(authority_boundary(name)),
        name if MUTATING_NAMES.contains(&name) => Err(authority_boundary(name)),
        _ => Err(error("unknown_tool", &format!("unknown_tool:{name}"))),
    }
}

fn guidance_tool() -> Value { tool("mailbox_guidance","Show model-facing operating guidance for mailbox MCP workflows.",json!({"type":"object","properties":{"workflow":{"type":"string"},"tool":{"type":"string"}},"additionalProperties":false}),true) }
fn guidance(args: &Map<String, Value>) -> Value { json!({"schema":"narada.mailbox.guidance.v1","status":"ok","surface_id":"mailbox","requested":args,"first_use":["Call mailbox_doctor.","Read accounts/messages/search/thread with bounded limits.","Keep synchronization, admission, and outbox writes with the owning authority."],"native_read_only":true}) }
fn doctor(root: &Path) -> Value { let scan=scan(root); json!({"schema":"narada.mailbox_mcp.doctor.v1","status":"ok","site_root":root.to_string_lossy(),"roots":scan.roots,"scanned_files":scan.scanned_files,"skipped_non_message_records":scan.skipped,"message_count":scan.messages.len(),"invalid_count":scan.invalid.len(),"invalid_records":scan.invalid,"server_name":"mailbox-mcp","native_read_only":true}) }
struct Scan { roots: Vec<PathBuf>, messages: Vec<Value>, scanned_files: usize, skipped: usize, invalid: Vec<Value> }
fn scan(root: &Path) -> Scan {
    let roots = configured_roots(root);
    let mut files = Vec::new();
    let mut invalid = Vec::new();
    for base in &roots { collect_files(base, &mut files, &mut invalid); }
    let mut records = Vec::new();
    let mut skipped = 0;
    for path in files.iter().take(MAX_FILES) {
        match records_from_file(path) {
            Ok(values) => for raw in values {
                if let Some(message) = normalize_message(&raw, path, root) {
                    let key = message_key(&message);
                    if let Some(index) = records.iter().position(|candidate| message_key(candidate) == key) {
                        let current_path = records[index].get("source_path").and_then(Value::as_str).unwrap_or_default();
                        let candidate_path = message.get("source_path").and_then(Value::as_str).unwrap_or_default();
                        if source_preference(candidate_path) < source_preference(current_path) {
                            records[index] = message;
                        }
                    } else {
                        records.push(message);
                    }
                } else {
                    skipped += 1;
                }
            },
            Err(reason) => invalid.push(json!({"file_path": path.to_string_lossy(), "reason": reason})),
        }
    }
    records.sort_by(|a, b| b.get("received_at").and_then(Value::as_str).cmp(&a.get("received_at").and_then(Value::as_str)));
    records.truncate(MAX_ROWS);
    Scan {
        roots,
        messages: records,
        scanned_files: files.len().min(MAX_FILES),
        skipped,
        invalid: invalid.into_iter().take(100).collect(),
    }
}
fn configured_roots(root:&Path)->Vec<PathBuf>{ let config=root.join(".ai/mailbox-mcp.json"); if fs::metadata(&config).ok().map(|metadata|metadata.len()<=MAX_BYTES).unwrap_or(false){ if let Ok(text)=fs::read_to_string(&config){ if let Ok(value)=serde_json::from_str::<Value>(&text){ if let Some(values)=value.get("roots").and_then(Value::as_array){let roots=values.iter().filter_map(Value::as_str).filter(|v|!v.trim().is_empty()).map(|v|root.join(v)).filter(|p|is_within(p,root)).collect::<Vec<_>>(); if !roots.is_empty(){return roots;} } } } } DEFAULT_ROOTS.iter().map(|v|root.join(v)).collect() }
fn collect_files(path:&Path, files:&mut Vec<PathBuf>, invalid:&mut Vec<Value>){ if files.len()>=MAX_FILES{return;} let Ok(meta)=fs::metadata(path)else{return}; if meta.is_file(){if path.extension().and_then(|v|v.to_str()).map(|v|matches!(v.to_ascii_lowercase().as_str(),"json"|"jsonl")).unwrap_or(false){files.push(path.to_path_buf());}return;} if !meta.is_dir(){return;} let Ok(entries)=fs::read_dir(path)else{return;}; for entry in entries.filter_map(Result::ok){if files.len()>=MAX_FILES{invalid.push(json!({"root":path.to_string_lossy(),"reason":"scan_file_limit_reached"}));break;} let name=entry.file_name().to_string_lossy().to_string(); if name=="node_modules"||name==".git"{continue;} collect_files(&entry.path(),files,invalid);} }
fn records_from_file(path:&Path)->Result<Vec<Value>,String>{let size=fs::metadata(path).map_err(|_|"stat_failed")?.len(); if size>MAX_BYTES{return Err("file_too_large".into());} let text=fs::read_to_string(path).map_err(|_|"read_failed")?.trim_start_matches('\u{feff}').to_string(); if path.extension().and_then(|v|v.to_str())==Some("jsonl"){return Ok(text.lines().filter(|l|!l.trim().is_empty()).filter_map(|l|serde_json::from_str::<Value>(l).ok()).collect());} let value=serde_json::from_str::<Value>(&text).map_err(|_|"invalid_json")?; if let Some(values)=value.as_array(){return Ok(values.clone());} if let Some(values)=value.get("messages").and_then(Value::as_array).or_else(||value.get("value").and_then(Value::as_array)){let mailbox=value.get("mailbox_id").or_else(||value.get("mailboxId")).cloned(); return Ok(values.iter().map(|v|{let mut obj=v.as_object().cloned().unwrap_or_default(); if !obj.contains_key("mailbox_id"){if let Some(id)=mailbox.clone(){obj.insert("mailbox_id".into(),id);}} Value::Object(obj)}).collect());} Ok(vec![value]) }
fn normalize_message(raw: &Value, path: &Path, _root: &Path) -> Option<Value> {
    let o = raw.as_object()?;
    let id = first_str(o, &["message_id", "messageId", "internetMessageId", "internet_message_id", "id", "entryId"])?;
    let body_object = o.get("body").and_then(Value::as_object);
    let has_shape = [
        "subject", "title", "body_text", "bodyText", "text", "body_html", "bodyHtml",
        "html", "preview", "body_preview", "bodyPreview", "snippet", "from", "sender",
        "to", "toRecipients", "received_at", "receivedAt", "receivedDateTime", "sent_at",
        "sentAt", "sentDateTime", "conversation_id", "conversationId", "thread_id", "threadId",
    ].iter().any(|key| o.contains_key(*key))
        || body_object.and_then(|body| body.get("content")).is_some();
    if !has_shape { return None; }
    let body = first_str(o, &["body_text", "bodyText", "text"])
        .or_else(|| body_object.and_then(|value| value.get("text")).and_then(Value::as_str).map(ToOwned::to_owned))
        .or_else(|| body_object.and_then(|value| value.get("content")).and_then(Value::as_str).map(ToOwned::to_owned))
        .map(|value| normalize_text(&value));
    let body_html = first_str(o, &["body_html", "bodyHtml", "html"])
        .or_else(|| body_object.filter(|value| value.get("contentType").and_then(Value::as_str).map(|kind| kind.eq_ignore_ascii_case("html")).unwrap_or(false))
            .and_then(|value| value.get("content")).and_then(Value::as_str).map(ToOwned::to_owned))
        .map(|value| normalize_text(&value));
    let preview = first_str(o, &["preview", "body_preview", "bodyPreview", "snippet"])
        .or_else(|| body.clone())
        .or_else(|| body_html.clone());
    let mailbox = first_str(o, &["mailbox_id", "mailboxId", "account", "account_id"])
        .unwrap_or_else(|| path.parent().and_then(|p| p.file_name()).and_then(|v| v.to_str()).unwrap_or("default").to_string());
    let source_path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf()).to_string_lossy().to_string();
    let source_path = source_path.strip_prefix("\\\\?\\").unwrap_or(&source_path).to_string();
    let attachments = as_array(o.get("attachments")).into_iter().map(|value| metadata_only_attachment(&value)).collect::<Vec<_>>();
    Some(json!({
        "message_id": id,
        "mailbox_id": mailbox,
        "folder": first_str(o, &["folder", "folder_id", "folderId", "mailFolder"]),
        "thread_id": first_str(o, &["thread_id", "threadId", "conversation_id", "conversationId", "conversationIndex"]),
        "subject": first_str(o, &["subject", "title"]).unwrap_or_else(|| "(no subject)".into()),
        "from": o.get("from").or_else(|| o.get("sender")).cloned().unwrap_or(Value::Null),
        "to": as_array(o.get("to").or_else(|| o.get("toRecipients"))),
        "cc": as_array(o.get("cc").or_else(|| o.get("ccRecipients"))),
        "received_at": first_str(o, &["received_at", "receivedAt", "receivedDateTime", "date", "created_at"]),
        "sent_at": first_str(o, &["sent_at", "sentAt", "sentDateTime"]),
        "unread": o.get("unread").or_else(|| o.get("isUnread")).cloned().or_else(|| o.get("isRead").and_then(Value::as_bool).map(|value| json!(!value))),
        "importance": first_str(o, &["importance", "priority"]),
        "categories": o.get("categories").and_then(Value::as_array).cloned().unwrap_or_default(),
        "preview": preview,
        "body_text": body,
        "body_html": body_html,
        "attachments": attachments,
        "source_path": source_path,
        "raw": Value::Object(o.clone()),
    }))
}

fn messages(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let scan = scan(root);
    let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(20).clamp(1, 100) as usize;
    let filtered = scan.messages.into_iter().filter(|value| filter_message(value, args)).collect::<Vec<_>>();
    let count = filtered.len();
    let include_body = args.get("include_body").and_then(Value::as_bool).unwrap_or(false);
    let rows = filtered.into_iter().take(limit).map(|value| summarize_message(&value, include_body, false, false)).collect::<Vec<_>>();
    Ok(json!({
        "schema": "narada.mailbox_mcp.messages.v1",
        "status": "ok",
        "site_root": root.to_string_lossy(),
        "filters": {
            "mailbox_id": args.get("mailbox_id").cloned().unwrap_or(Value::Null),
            "folder": args.get("folder").cloned().unwrap_or(Value::Null),
            "unread": args.get("unread").cloned().unwrap_or(Value::Null),
            "since": args.get("since").cloned().unwrap_or(Value::Null),
            "before": args.get("before").cloned().unwrap_or(Value::Null),
            "query": args.get("query").cloned().unwrap_or(Value::Null),
        },
        "count": count,
        "messages": rows,
        "native_read_only": true,
    }))
}

fn accounts(root: &Path) -> Result<Value, Value> {
    let scan = scan(root);
    let mut map = std::collections::BTreeMap::<String, (usize, usize, std::collections::BTreeSet<String>, Option<String>)>::new();
    for row in scan.messages {
        let id = row.get("mailbox_id").and_then(Value::as_str).unwrap_or("default").to_string();
        let entry = map.entry(id).or_insert_with(|| (0, 0, std::collections::BTreeSet::new(), None));
        entry.0 += 1;
        if row.get("unread").and_then(Value::as_bool) == Some(true) { entry.1 += 1; }
        if let Some(folder) = row.get("folder").and_then(Value::as_str).filter(|value| !value.is_empty()) { entry.2.insert(folder.to_string()); }
        let timestamp = row.get("received_at").and_then(Value::as_str).or_else(|| row.get("sent_at").and_then(Value::as_str));
        if let Some(timestamp) = timestamp {
            if entry.3.as_deref().map(|current| timestamp > current).unwrap_or(true) { entry.3 = Some(timestamp.to_string()); }
        }
    }
    let accounts = map.into_iter().map(|(mailbox_id, (message_count, unread_count, folders, latest_message_at))| json!({
        "mailbox_id": mailbox_id,
        "message_count": message_count,
        "unread_count": unread_count,
        "folders": folders.into_iter().collect::<Vec<_>>(),
        "latest_message_at": latest_message_at,
    })).collect::<Vec<_>>();
    Ok(json!({"schema":"narada.mailbox_mcp.accounts.v1","status":"ok","site_root":root.to_string_lossy(),"count":accounts.len(),"accounts":accounts,"native_read_only":true}))
}

fn message_show(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let id = required(args, "message_id")?;
    let row = scan(root).messages.into_iter().find(|value|
        value.get("message_id").and_then(Value::as_str) == Some(id.as_str())
            && args.get("mailbox_id").and_then(Value::as_str).map(|mailbox| value.get("mailbox_id").and_then(Value::as_str) == Some(mailbox)).unwrap_or(true)
    );
    let message = row.as_ref().map(|value| summarize_message(value, true, args.get("include_html").and_then(Value::as_bool).unwrap_or(false), args.get("include_raw").and_then(Value::as_bool).unwrap_or(false)));
    Ok(json!({"schema":"narada.mailbox_mcp.message.v1","status":if row.is_some(){"ok"}else{"not_found"},"site_root":root.to_string_lossy(),"message_id":id,"message":message,"native_read_only":true}))
}

fn thread_show(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let id = required(args, "thread_id")?;
    let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(50).clamp(1, 100) as usize;
    let include_body = args.get("include_body").and_then(Value::as_bool).unwrap_or(true);
    let mailbox_id = args.get("mailbox_id").and_then(Value::as_str);
    let mut values = scan(root).messages.into_iter().filter(|value|
        value.get("thread_id").and_then(Value::as_str) == Some(id.as_str())
            && mailbox_id.map(|mailbox| value.get("mailbox_id").and_then(Value::as_str) == Some(mailbox)).unwrap_or(true)
    ).collect::<Vec<_>>();
    values.sort_by(|left, right| {
        let left_time = left.get("received_at").and_then(Value::as_str).or_else(|| left.get("sent_at").and_then(Value::as_str)).unwrap_or("");
        let right_time = right.get("received_at").and_then(Value::as_str).or_else(|| right.get("sent_at").and_then(Value::as_str)).unwrap_or("");
        left_time.cmp(right_time)
    });
    let count = values.len();
    let messages = values.into_iter().take(limit).map(|value| summarize_message(&value, include_body, false, false)).collect::<Vec<_>>();
    Ok(json!({"schema":"narada.mailbox_mcp.thread.v1","status":if count > 0{"ok"}else{"not_found"},"site_root":root.to_string_lossy(),"thread_id":id,"count":count,"messages":messages,"native_read_only":true}))
}
fn output_show(args:&Map<String,Value>,root:&Path)->Result<Value,Value>{let reference=args.get("ref").or_else(||args.get("output_ref")).and_then(Value::as_str).ok_or_else(||error("output_ref_required","output_ref_required"))?;let id=reference.strip_prefix("mcp_output:").ok_or_else(||error("output_ref_invalid","output_ref_invalid"))?;if id.is_empty()||id.len()>100||!id.chars().all(|c|c.is_ascii_alphanumeric()||c=='-'||c=='_'){return Err(error("output_ref_invalid","output_ref_invalid"));}let path=root.join(".ai/tmp/mcp-outputs/workspace").join(format!("{id}.json"));let value=read_bounded(&path)?;Ok(json!({"schema":"narada.mcp_output_page.v1","status":"ok","ref":reference,"output":value,"native_read_only":true}))}
fn domain_db_path(root:&Path)->PathBuf{root.join(DOMAIN_DB_RELATIVE)}
fn open_domain_db(root:&Path)->Result<Option<Connection>,Value>{let path=domain_db_path(root);if !path.exists(){return Ok(None);}Connection::open_with_flags(path,OpenFlags::SQLITE_OPEN_READ_ONLY).map(Some).map_err(|e|error("mailbox_domain_store_open_failed",&e.to_string()))}
fn generation_show(args:&Map<String,Value>,root:&Path)->Result<Value,Value>{let id=required(args,"generation_id")?;let Some(db)=open_domain_db(root)? else{return Ok(json!({"schema":"narada.mailbox.sync_generation.v1","status":"not_found","generation_id":id}));};let row:Option<Value>=db.query_row("SELECT generation_id,idempotency_key,scope_id,config_fingerprint,status,parent_cursor,next_cursor,batch_sha256,batch_record_count,staged_at,error_message,created_at,updated_at,completed_at FROM mailbox_sync_generations WHERE generation_id=? LIMIT 1",params![id],|row|Ok(json!({"generation_id":row.get::<_,String>(0)?,"idempotency_key":row.get::<_,String>(1)?,"scope_id":row.get::<_,String>(2)?,"config_fingerprint":row.get::<_,String>(3)?,"status":row.get::<_,String>(4)?,"parent_cursor_sha256":row.get::<_,Option<String>>(5)?.map(|v|v.len()),"next_cursor_sha256":row.get::<_,Option<String>>(6)?.map(|v|v.len()),"batch_sha256":row.get::<_,Option<String>>(7)?,"batch_record_count":row.get::<_,i64>(8)?,"staged_at":row.get::<_,Option<String>>(9)?,"error_present":row.get::<_,Option<String>>(10)?.is_some(),"created_at":row.get::<_,String>(11)?,"updated_at":row.get::<_,String>(12)?,"completed_at":row.get::<_,Option<String>>(13)?}))).optional().map_err(|e|error("mailbox_generation_query_failed",&e.to_string()))?;Ok(json!({"schema":"narada.mailbox.sync_generation.v1","status":if row.is_some(){"ok"}else{"not_found"},"generation":row,"metadata_only":true}))}
fn outbox_consumer_show(args:&Map<String,Value>,root:&Path)->Result<Value,Value>{let id=required(args,"consumer_id")?;let Some(db)=open_domain_db(root)? else{return Ok(json!({"schema":"narada.mailbox.outbox_consumer_lookup.v1","status":"not_found","consumer_id":id}));};let row:Option<Value>=db.query_row("SELECT consumer_id,scope_id,topics_json,start_at,created_at FROM mailbox_outbox_consumers WHERE consumer_id=? LIMIT 1",params![id],|row|{let topics:String=row.get(2)?;Ok(json!({"consumer_id":row.get::<_,String>(0)?,"scope_id":row.get::<_,Option<String>>(1)?,"topics":serde_json::from_str::<Value>(&topics).unwrap_or(Value::Null),"start_at":row.get::<_,String>(3)?,"created_at":row.get::<_,String>(4)?}))}).optional().map_err(|e|error("mailbox_outbox_consumer_query_failed",&e.to_string()))?;Ok(json!({"schema":"narada.mailbox.outbox_consumer_lookup.v1","status":if row.is_some(){"ok"}else{"not_found"},"consumer_id":id,"consumer":row,"metadata_only":true}))}
fn outbox_list(args:&Map<String,Value>,root:&Path)->Result<Value,Value>{let id=required(args,"consumer_id")?;let limit=args.get("limit").and_then(Value::as_u64).unwrap_or(100).clamp(1,100) as usize;let Some(db)=open_domain_db(root)? else{return Ok(json!({"schema":"narada.mailbox.outbox_list.v2","status":"not_found","consumer_id":id,"items":[],"has_more":false}));};let consumer:Option<(Option<String>,String,String)>=db.query_row("SELECT scope_id,topics_json,start_at FROM mailbox_outbox_consumers WHERE consumer_id=?",params![id],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?))).optional().map_err(|e|error("mailbox_outbox_consumer_query_failed",&e.to_string()))?;let Some((scope,topics_json,start_at))=consumer else{return Ok(json!({"schema":"narada.mailbox.outbox_list.v2","status":"not_found","consumer_id":id,"items":[],"has_more":false}));};let topics:Vec<String>=serde_json::from_str(&topics_json).unwrap_or_default();let mut statement=db.prepare("SELECT event_id,scope_id,topic,aggregate_id,aggregate_revision,schema_version,causation_id,idempotency_key,partition_key,occurred_at FROM mailbox_outbox event WHERE event.occurred_at>=? AND (? IS NULL OR event.scope_id=?) AND NOT EXISTS (SELECT 1 FROM mailbox_outbox_receipts receipt WHERE receipt.consumer_id=? AND receipt.event_id=event.event_id) ORDER BY event.occurred_at,event.event_id LIMIT 500").map_err(|e|error("mailbox_outbox_query_failed",&e.to_string()))?;let rows=statement.query_map(params![start_at,scope,scope,id],|row|Ok(json!({"schema":"narada.mailbox.outbox_event.v1","event_id":row.get::<_,String>(0)?,"scope_id":row.get::<_,String>(1)?,"topic":row.get::<_,String>(2)?,"aggregate_id":row.get::<_,String>(3)?,"aggregate_revision":row.get::<_,i64>(4)?,"schema_version":row.get::<_,i64>(5)?,"causation_id":row.get::<_,String>(6)?,"idempotency_key":row.get::<_,String>(7)?,"partition_key":row.get::<_,String>(8)?,"occurred_at":row.get::<_,String>(9)?,"metadata_only":true}))).map_err(|e|error("mailbox_outbox_query_failed",&e.to_string()))?;let mut items=Vec::new();for row in rows{let value=row.map_err(|e|error("mailbox_outbox_row_failed",&e.to_string()))?;if topics.is_empty()||topics.iter().any(|topic|value.get("topic").and_then(Value::as_str)==Some(topic)){items.push(value);if items.len()>limit{break;}}}let has_more=items.len()>limit;if has_more{items.truncate(limit);}Ok(json!({"schema":"narada.mailbox.outbox_list.v2","status":"ok","consumer_id":id,"count":items.len(),"items":items,"has_more":has_more,"metadata_only":true}))}
fn filter_message(v: &Value, args: &Map<String, Value>) -> bool {
    for (key, field) in [("mailbox_id", "mailbox_id"), ("folder", "folder"), ("thread_id", "thread_id")] {
        if let Some(filter) = args.get(key).and_then(Value::as_str) {
            if v.get(field).and_then(Value::as_str) != Some(filter) { return false; }
        }
    }
    if let Some(unread) = args.get("unread").and_then(Value::as_bool) {
        if v.get("unread").and_then(Value::as_bool) != Some(unread) { return false; }
    }
    let timestamp = v.get("received_at").and_then(Value::as_str).or_else(|| v.get("sent_at").and_then(Value::as_str)).unwrap_or("");
    if let Some(since) = args.get("since").and_then(Value::as_str) {
        if timestamp < since { return false; }
    }
    if let Some(before) = args.get("before").and_then(Value::as_str) {
        if timestamp >= before { return false; }
    }
    if let Some(query) = args.get("query").and_then(Value::as_str).filter(|value| !value.trim().is_empty()) {
        if !message_matches_query(v, query) { return false; }
    }
    true
}

fn message_matches_query(value: &Value, query: &str) -> bool {
    let mut haystack = String::new();
    for field in ["subject", "preview", "body_text"] {
        haystack.push_str(value.get(field).and_then(Value::as_str).unwrap_or(""));
        haystack.push('\n');
    }
    for field in ["from", "to", "cc", "categories"] {
        haystack.push_str(&serde_json::to_string(value.get(field).unwrap_or(&Value::Null)).unwrap_or_default());
        haystack.push('\n');
    }
    haystack.to_ascii_lowercase().contains(&query.to_ascii_lowercase())
}

fn summarize_message(value: &Value, include_body: bool, include_html: bool, include_raw: bool) -> Value {
    let mut summary = Map::new();
    for field in ["message_id", "mailbox_id", "folder", "thread_id", "subject", "from", "to", "cc", "received_at", "sent_at", "unread", "importance", "categories", "preview", "source_path"] {
        summary.insert(field.to_string(), value.get(field).cloned().unwrap_or(Value::Null));
    }
    summary.insert("attachments".to_string(), metadata_only_attachment(value.get("attachments").unwrap_or(&Value::Null)));
    if include_body { summary.insert("body_text".to_string(), value.get("body_text").cloned().unwrap_or(Value::Null)); }
    if include_html { summary.insert("body_html".to_string(), value.get("body_html").cloned().unwrap_or(Value::Null)); }
    if include_raw { summary.insert("raw".to_string(), value.get("raw").cloned().unwrap_or(Value::Null)); }
    Value::Object(summary)
}

fn metadata_only_attachment(value: &Value) -> Value {
    match value {
        Value::Array(values) => Value::Array(values.iter().map(metadata_only_attachment).collect()),
        Value::Object(object) => Value::Object(object.iter().filter_map(|(key, nested)| {
            let normalized = key.to_ascii_lowercase();
            if matches!(normalized.as_str(), "contentbytes" | "content_bytes" | "content_base64" | "contentref" | "content_ref" | "content" | "data" | "bytes" | "raw") {
                None
            } else {
                Some((key.clone(), metadata_only_attachment(nested)))
            }
        }).collect()),
        _ => value.clone(),
    }
}

fn message_key(value: &Value) -> String {
    format!("{}\u{0}{}", value.get("mailbox_id").and_then(Value::as_str).unwrap_or("default"), value.get("message_id").and_then(Value::as_str).unwrap_or(""))
}

fn source_preference(path: &str) -> u8 {
    let parts = path.replace('\\', "/").to_ascii_lowercase();
    if parts.split('/').any(|part| part == "messages") { 0 } else if parts.split('/').any(|part| part == "views") { 10 } else { 5 }
}
fn first_str(o:&Map<String,Value>,keys:&[&str])->Option<String>{keys.iter().find_map(|k|o.get(*k).and_then(|v|if let Some(s)=v.as_str(){Some(s.trim().to_string())}else if v.is_number(){Some(v.to_string())}else{None}).filter(|s|!s.is_empty()))}
fn normalize_text(s:&str)->String{s.replace("\r\n","\n").split_whitespace().collect::<Vec<_>>().join(" ").chars().take(5000).collect()}
fn as_array(v:Option<&Value>)->Vec<Value>{match v{Some(Value::Array(a))=>a.clone(),Some(v)=>vec![v.clone()],None=>Vec::new()}}
fn required(args:&Map<String,Value>,key:&str)->Result<String,Value>{args.get(key).and_then(Value::as_str).map(str::trim).filter(|v|!v.is_empty()).map(str::to_string).ok_or_else(||error(&format!("{key}_required"),&format!("{key}_required")))}
fn is_within(path:&Path,root:&Path)->bool{let p=path.canonicalize().unwrap_or_else(|_|path.to_path_buf());let r=root.canonicalize().unwrap_or_else(|_|root.to_path_buf());p==r||p.starts_with(&r)}
fn read_bounded(path:&Path)->Result<Value,Value>{if fs::metadata(path).map_err(|_|error("output_ref_not_found","output_ref_not_found"))?.len()>MAX_BYTES{return Err(error("output_ref_too_large","output_ref_too_large"));}let text=fs::read_to_string(path).map_err(|_|error("output_ref_read_failed","output_ref_read_failed"))?;serde_json::from_str(&text).map_err(|_|error("output_ref_invalid_json","output_ref_invalid_json"))}
fn authority_boundary(name:&str)->Value{json!({"schema":"narada.mailbox.authority_boundary.v1","status":"unavailable","tool_name":name,"reason":"mailbox_projection_authority_not_enabled_in_native_read_slice","remediation":"Use the configured mailbox authority for synchronization, admission, and outbox operations."})}
fn error(code:&str,message:&str)->Value{json!({"schema":"narada.mailbox.error.v1","code":code,"message":message})}
fn schema(name: &str) -> Value {
    match name {
        "mailbox_messages_list" | "mailbox_search" => json!({"type":"object","properties":{
            "mailbox_id":{"type":"string"},"folder":{"type":"string"},"unread":{"type":"boolean"},
            "since":{"type":"string"},"before":{"type":"string"},"query":{"type":"string"},
            "limit":{"type":"integer","minimum":1,"maximum":100},"include_body":{"type":"boolean"}
        },"additionalProperties":false}),
        "mailbox_message_show" => json!({"type":"object","properties":{
            "message_id":{"type":"string"},"mailbox_id":{"type":"string"},
            "include_html":{"type":"boolean"},"include_raw":{"type":"boolean"}
        },"required":["message_id"],"additionalProperties":false}),
        "mailbox_thread_show" => json!({"type":"object","properties":{
            "thread_id":{"type":"string"},"mailbox_id":{"type":"string"},
            "limit":{"type":"integer","minimum":1,"maximum":100},"include_body":{"type":"boolean"}
        },"required":["thread_id"],"additionalProperties":false}),
        "mailbox_generation_show" => json!({"type":"object","properties":{"generation_id":{"type":"string"}},"required":["generation_id"],"additionalProperties":false}),
        "mailbox_outbox_consumer_show" => json!({"type":"object","properties":{"consumer_id":{"type":"string"}},"required":["consumer_id"],"additionalProperties":false}),
        "mailbox_outbox_list" => json!({"type":"object","properties":{"consumer_id":{"type":"string"},"limit":{"type":"integer","minimum":1,"maximum":100}},"required":["consumer_id"],"additionalProperties":false}),
        "mailbox_output_show" => json!({"type":"object","properties":{"ref":{"type":"string"},"output_ref":{"type":"string"},"offset":{"type":"integer","minimum":0},"limit":{"type":"integer","minimum":0}},"additionalProperties":false}),
        _ => json!({"type":"object","additionalProperties":false}),
    }
}
fn tool(name:&str,description:&str,schema:Value,read_only:bool)->Value{json!({"name":name,"description":description,"inputSchema":schema,"annotations":{"title":name,"readOnlyHint":read_only,"destructiveHint":!read_only,"idempotentHint":read_only,"openWorldHint":false},"outputSchema":{"type":"object","additionalProperties":true}})}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_mailbox_scans_bounded_projection() {
        let root = std::env::temp_dir().join(format!("narada-mailbox-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(root.join(".ai/mailboxes/acct")).expect("root");
        fs::write(root.join(".ai/mailboxes/acct/messages.json"), r#"[{"id":"m1","subject":"hello","folder":"Inbox","body":{"content":"world"},"receivedDateTime":"2026-01-01T00:00:00Z","isRead":false}]"#).expect("file");
        fs::write(root.join(".ai/mailboxes/acct/settings.json"), r#"{"id":"settings","enabled":true}"#).expect("settings");
        fs::create_dir_all(root.join(".ai/mailboxes/acct/views/by-thread")).expect("views");
        fs::write(root.join(".ai/mailboxes/acct/views/by-thread/m1.json"), r#"{"id":"m1","subject":"derived view should lose","conversationId":"thread-1","text":"view"}"#).expect("view");
        let result = messages(&json!({"limit":1,"include_body":false,"since":"2025-01-01"}).as_object().unwrap(), &root).expect("messages");
        assert_eq!(result["count"], 1);
        assert!(result["messages"][0].get("body_text").is_none());
        assert_eq!(result["messages"][0]["subject"], "hello");
        let doctor = doctor(&root);
        assert_eq!(doctor["skipped_non_message_records"], 1);
        let accounts = accounts(&root).expect("accounts");
        assert_eq!(accounts["accounts"][0]["folders"][0], "Inbox");
        assert_eq!(accounts["accounts"][0]["latest_message_at"], "2026-01-01T00:00:00Z");
        let show = message_show(&json!({"message_id":"m1"}).as_object().unwrap(), &root).expect("show");
        assert_eq!(show["message"]["body_text"], "world");
        assert_eq!(show["message"]["subject"], "hello");
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn native_mailbox_domain_reads_are_metadata_only() {
        let root = std::env::temp_dir().join(format!("narada-mailbox-db-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(root.join(".narada/runtime/mailbox-domain")).expect("root");
        let db = Connection::open(domain_db_path(&root)).expect("db");
        db.execute_batch(r##"
            CREATE TABLE mailbox_sync_generations (generation_id TEXT,idempotency_key TEXT,scope_id TEXT,config_fingerprint TEXT,status TEXT,parent_cursor TEXT,next_cursor TEXT,batch_sha256 TEXT,batch_record_count INTEGER,staged_at TEXT,error_message TEXT,created_at TEXT,updated_at TEXT,completed_at TEXT);
            CREATE TABLE mailbox_outbox_consumers (consumer_id TEXT,scope_id TEXT,topics_json TEXT,start_at TEXT,created_at TEXT);
            CREATE TABLE mailbox_outbox (event_id TEXT,scope_id TEXT,topic TEXT,aggregate_id TEXT,aggregate_revision INTEGER,schema_version INTEGER,causation_id TEXT,idempotency_key TEXT,partition_key TEXT,occurred_at TEXT);
            CREATE TABLE mailbox_outbox_receipts (consumer_id TEXT,event_id TEXT);
            INSERT INTO mailbox_sync_generations VALUES ('g1','k1','scope','cfg','completed',NULL,NULL,'hash',1,NULL,NULL,'2026-01-01','2026-01-01','2026-01-01');
            INSERT INTO mailbox_outbox_consumers VALUES ('c1','scope','["topic"]','2026-01-01','2026-01-01');
            INSERT INTO mailbox_outbox VALUES ('e1','scope','topic','a1',1,1,'c','k','p','2026-01-01');
        "##).expect("schema");
        drop(db);
        assert_eq!(generation_show(&json!({"generation_id":"g1"}).as_object().unwrap(), &root).expect("generation")["status"], "ok");
        assert_eq!(outbox_consumer_show(&json!({"consumer_id":"c1"}).as_object().unwrap(), &root).expect("consumer")["status"], "ok");
        let page = outbox_list(&json!({"consumer_id":"c1","limit":1}).as_object().unwrap(), &root).expect("outbox");
        assert_eq!(page["count"], 1);
        assert!(page["items"][0].get("metadata_only").and_then(Value::as_bool).unwrap_or(false));
        fs::remove_dir_all(root).expect("cleanup");
    }
}
