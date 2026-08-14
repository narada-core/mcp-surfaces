use rusqlite::{params, Connection, OpenFlags, OptionalExtension, TransactionBehavior};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use time::{format_description::well_known::Rfc3339, OffsetDateTime, UtcOffset};

const MAX_FILES: usize = 5_000;
const MAX_BYTES: u64 = 10 * 1024 * 1024;
const MAX_ROWS: usize = 500;
const MAX_SCAN_BYTES: u64 = 64 * 1024 * 1024;
const MAX_RAW_RESPONSE_BYTES: usize = 64 * 1024;
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
    for name in MUTATING_NAMES { tools.push(tool(name, "Mutate the durable mailbox projection authority.", schema(name), false)); }
    tools
}
pub fn auxiliary(method: &str, params: &Map<String, Value>) -> Result<Value, Value> { match method { "prompts/list" => Ok(json!({"prompts":[{"name":"mailbox_workflow","title":"Mailbox Workflow","description":"Inspect finite site-local mailbox state and use governed synchronization, admission, and outbox operations.","arguments":[]}]})), "prompts/get" => { if params.get("name").and_then(Value::as_str)!=Some("mailbox_workflow"){return Err(error("unknown_prompt","unknown_prompt"));} Ok(json!({"description":"Inspect finite site-local mailbox state and use governed synchronization, admission, and outbox operations.","messages":[{"role":"user","content":{"type":"text","text":"Use bounded mailbox reads for discovery. Before mutation, inspect the exact target and policy; after synchronization, admission, or outbox acknowledgement, read back the durable state."}}]})) }, "completion/complete" => { let values=if params.get("argument").and_then(Value::as_object).and_then(|v|v.get("name")).and_then(Value::as_str)==Some("name"){list_tools().iter().filter_map(|v|v.get("name").cloned()).take(100).collect::<Vec<_>>()}else{Vec::new()}; Ok(json!({"completion":{"values":values,"total":values.len(),"hasMore":false}})) }, "logging/setLevel"=>Ok(json!({})), _=>Err(error("unsupported_mcp_method",&format!("unsupported_mcp_method:{method}"))), } }
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
        "mailbox_fact_show" => fact_show(args, root),
        "mailbox_message_fact_find" => message_fact_find(args, root),
        "mailbox_generation_show" => generation_show(args, root),
        "mailbox_admission_show" => admission_show(args, root),
        "mailbox_outbox_consumer_show" => outbox_consumer_show(args, root),
        "mailbox_outbox_list" => outbox_list(args, root),
        "mailbox_outbox_consumer_register" => outbox_consumer_register(args, root),
        "mailbox_outbox_ack" => outbox_ack(args, root),
        "mailbox_sync_generation" => crate::mailbox_sync::sync_generation(args, root),
        "mailbox_reconcile_first_observations" => reconcile_first_observations(args, root),
        "mailbox_message_admit" => admit_message(args, root),
        _ => Err(error("unknown_tool", &format!("unknown_tool:{name}"))),
    }
}

fn guidance_tool() -> Value { tool("mailbox_guidance","Show model-facing operating guidance for mailbox MCP workflows.",json!({"type":"object","properties":{"workflow":{"type":"string"},"tool":{"type":"string"}},"additionalProperties":false}),true) }
fn guidance(args: &Map<String, Value>) -> Value { json!({"schema":"narada.mailbox.guidance.v1","status":"ok","surface_id":"mailbox","requested":args,"first_use":["Call mailbox_doctor.","Use bounded list/search/show tools for discovery.","Inspect policy and the exact target before mutation; read back durable state after synchronization, admission, or outbox acknowledgement."]}) }
fn doctor(root: &Path) -> Value { let scan=scan(root); json!({"schema":"narada.mailbox_mcp.doctor.v1","status":if scan.invalid.is_empty(){"ok"}else{"degraded"},"site_root":root.to_string_lossy(),"roots":scan.roots,"scanned_files":scan.scanned_files,"skipped_non_message_records":scan.skipped,"message_count":scan.messages.len(),"invalid_count":scan.invalid.len(),"invalid_records":scan.invalid,"server_name":"mailbox-mcp"}) }
struct Scan { roots: Vec<PathBuf>, messages: Vec<Value>, scanned_files: usize, skipped: usize, invalid: Vec<Value> }
fn scan(root: &Path) -> Scan {
    let (roots, mut invalid) = configured_roots(root);
    let mut files = Vec::new();
    for base in &roots { collect_files(base, &mut files, &mut invalid); }
    let mut records = Vec::new();
    let mut skipped = 0;
    let mut scanned_files = 0;
    let mut scanned_bytes = 0_u64;
    for path in files.iter().take(MAX_FILES) {
        let size = fs::metadata(path).map(|value| value.len()).unwrap_or(0);
        if scanned_bytes.saturating_add(size) > MAX_SCAN_BYTES {
            invalid.push(json!({"path":path.to_string_lossy(),"reason":"scan_byte_limit_reached","max_bytes":MAX_SCAN_BYTES}));
            break;
        }
        scanned_bytes = scanned_bytes.saturating_add(size);
        scanned_files += 1;
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
                        if records.len() >= MAX_ROWS {
                            invalid.push(json!({"path":path.to_string_lossy(),"reason":"scan_record_limit_reached","max_records":MAX_ROWS}));
                            break;
                        }
                    }
                } else {
                    skipped += 1;
                }
            },
            Err(reason) => invalid.push(json!({"file_path": path.to_string_lossy(), "reason": reason})),
        }
        if records.len() >= MAX_ROWS { break; }
    }
    records.sort_by(|a, b| b.get("received_at").and_then(Value::as_str).cmp(&a.get("received_at").and_then(Value::as_str)));
    records.truncate(MAX_ROWS);
    Scan {
        roots,
        messages: records,
        scanned_files,
        skipped,
        invalid: invalid.into_iter().take(100).collect(),
    }
}
fn configured_roots(root:&Path)->(Vec<PathBuf>,Vec<Value>){let config=root.join(".ai/mailbox-mcp.json");if !config.exists(){return(DEFAULT_ROOTS.iter().map(|value|root.join(value)).collect(),Vec::new());}let mut invalid=Vec::new();let Ok(metadata)=fs::metadata(&config)else{return(Vec::new(),vec![json!({"path":config.to_string_lossy(),"reason":"config_stat_failed"})]);};if metadata.len()>MAX_BYTES{return(Vec::new(),vec![json!({"path":config.to_string_lossy(),"reason":"config_too_large"})]);}let Ok(text)=fs::read_to_string(&config)else{return(Vec::new(),vec![json!({"path":config.to_string_lossy(),"reason":"config_read_failed"})]);};let Ok(document)=serde_json::from_str::<Value>(&text)else{return(Vec::new(),vec![json!({"path":config.to_string_lossy(),"reason":"config_invalid_json"})]);};let Some(values)=document.get("roots").and_then(Value::as_array)else{return(Vec::new(),vec![json!({"path":config.to_string_lossy(),"reason":"config_roots_required"})]);};if values.len()>32{return(Vec::new(),vec![json!({"path":config.to_string_lossy(),"reason":"config_root_limit_exceeded"})]);}let mut roots=Vec::new();for value in values{let Some(text)=value.as_str().map(str::trim).filter(|value|!value.is_empty()&&value.chars().count()<=1024)else{invalid.push(json!({"reason":"config_root_invalid"}));continue;};let candidate=PathBuf::from(text);let admitted=if candidate.is_absolute(){candidate}else if candidate.components().any(|component|matches!(component,std::path::Component::ParentDir|std::path::Component::RootDir|std::path::Component::Prefix(_))){invalid.push(json!({"root":text,"reason":"config_root_outside_site"}));continue;}else{root.join(candidate)};if !is_within(&admitted,root){invalid.push(json!({"root":text,"reason":"config_root_outside_site"}));continue;}roots.push(admitted);}if roots.is_empty()&&invalid.is_empty(){invalid.push(json!({"reason":"config_roots_empty"}));}(roots,invalid)}
fn collect_files(path:&Path, files:&mut Vec<PathBuf>, invalid:&mut Vec<Value>){ if files.len()>=MAX_FILES{return;} let Ok(link_meta)=fs::symlink_metadata(path)else{return};if link_meta.file_type().is_symlink(){invalid.push(json!({"path":path.to_string_lossy(),"reason":"symlink_not_followed"}));return;}let Ok(meta)=fs::metadata(path)else{return}; if meta.is_file(){if path.extension().and_then(|v|v.to_str()).map(|v|matches!(v.to_ascii_lowercase().as_str(),"json"|"jsonl")).unwrap_or(false){files.push(path.to_path_buf());}return;} if !meta.is_dir(){return;} let Ok(entries)=fs::read_dir(path)else{return;}; for entry in entries.filter_map(Result::ok){if files.len()>=MAX_FILES{invalid.push(json!({"root":path.to_string_lossy(),"reason":"scan_file_limit_reached"}));break;} let name=entry.file_name().to_string_lossy().to_string(); if name=="node_modules"||name==".git"{continue;} collect_files(&entry.path(),files,invalid);} }
fn records_from_file(path:&Path)->Result<Vec<Value>,String>{let size=fs::metadata(path).map_err(|_|"stat_failed")?.len(); if size>MAX_BYTES{return Err("file_too_large".into());} let text=fs::read_to_string(path).map_err(|_|"read_failed")?.trim_start_matches('\u{feff}').to_string(); if path.extension().and_then(|v|v.to_str())==Some("jsonl"){let mut records=Vec::new();for(line_index,line)in text.lines().enumerate(){if line.trim().is_empty(){continue;}records.push(serde_json::from_str::<Value>(line).map_err(|_|format!("invalid_jsonl_line:{}",line_index+1))?);}return Ok(records);} let value=serde_json::from_str::<Value>(&text).map_err(|_|"invalid_json")?; if let Some(values)=value.as_array(){return Ok(values.clone());} if let Some(values)=value.get("messages").and_then(Value::as_array).or_else(||value.get("value").and_then(Value::as_array)){let mailbox=value.get("mailbox_id").or_else(||value.get("mailboxId")).cloned(); return Ok(values.iter().map(|v|{let mut obj=v.as_object().cloned().unwrap_or_default(); if !obj.contains_key("mailbox_id"){if let Some(id)=mailbox.clone(){obj.insert("mailbox_id".into(),id);}} Value::Object(obj)}).collect());} Ok(vec![value]) }
fn normalize_message(raw: &Value, path: &Path, _root: &Path) -> Option<Value> {
    let o = raw.as_object()?;
    let id = bounded_text(first_str(o, &["message_id", "messageId", "internetMessageId", "internet_message_id", "id", "entryId"]),1024)?;
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
    let mailbox = bounded_text(first_str(o, &["mailbox_id", "mailboxId", "account", "account_id"]),512)
        .unwrap_or_else(|| path.parent().and_then(|p| p.file_name()).and_then(|v| v.to_str()).unwrap_or("default").to_string());
    let source_path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf()).to_string_lossy().to_string();
    let source_path = source_path.strip_prefix("\\\\?\\").unwrap_or(&source_path).to_string();
    let attachments = as_array(o.get("attachments")).into_iter().map(|value| metadata_only_attachment(&value)).collect::<Vec<_>>();
    Some(json!({
        "message_id": id,
        "mailbox_id": mailbox,
        "folder": bounded_text(first_str(o, &["folder", "folder_id", "folderId", "mailFolder"]),512),
        "thread_id": bounded_text(first_str(o, &["thread_id", "threadId", "conversation_id", "conversationId", "conversationIndex"]),1024),
        "subject": bounded_text(first_str(o, &["subject", "title"]),2048).unwrap_or_else(|| "(no subject)".into()),
        "from": bounded_projection(o.get("from").or_else(|| o.get("sender")).unwrap_or(&Value::Null),0),
        "to": bounded_projection(&Value::Array(as_array(o.get("to").or_else(|| o.get("toRecipients")))),0),
        "cc": bounded_projection(&Value::Array(as_array(o.get("cc").or_else(|| o.get("ccRecipients")))),0),
        "received_at": normalized_source_timestamp(first_str(o, &["received_at", "receivedAt", "receivedDateTime", "date", "created_at"])),
        "sent_at": normalized_source_timestamp(first_str(o, &["sent_at", "sentAt", "sentDateTime"])),
        "unread": o.get("unread").or_else(|| o.get("isUnread")).cloned().or_else(|| o.get("isRead").and_then(Value::as_bool).map(|value| json!(!value))),
        "importance": bounded_text(first_str(o, &["importance", "priority"]),64),
        "categories": bounded_projection(o.get("categories").unwrap_or(&Value::Array(Vec::new())),0),
        "preview": preview,
        "body_text": body,
        "body_html": body_html,
        "attachments": attachments,
        "source_path": source_path,
        "raw": Value::Object(o.clone()),
    }))
}

fn messages(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let mut normalized_args=args.clone();
    for key in ["since","before"]{if args.get(key).is_some(){normalized_args.insert(key.to_string(),json!(required_timestamp(args,key,&format!("mailbox_{key}_timestamp_required"))?));}}
    if let(Some(since),Some(before))=(normalized_args.get("since").and_then(Value::as_str),normalized_args.get("before").and_then(Value::as_str)){if since>=before{return Err(error("mailbox_time_range_invalid","mailbox_time_range_invalid"));}}
    let scan = scan(root);
    let offset = args.get("offset").and_then(Value::as_u64).unwrap_or(0) as usize;
    let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(20).clamp(1, 100) as usize;
    let filtered = scan.messages.into_iter().filter(|value| filter_message(value, &normalized_args)).collect::<Vec<_>>();
    let total_count = filtered.len();
    let include_body = args.get("include_body").and_then(Value::as_bool).unwrap_or(false);
    let rows = filtered.into_iter().skip(offset).take(limit).map(|value| summarize_message(&value, include_body, false, false)).collect::<Vec<_>>();
    Ok(json!({
        "schema": "narada.mailbox_mcp.messages.v1",
        "status": "ok",
        "site_root": root.to_string_lossy(),
        "filters": {
            "mailbox_id": args.get("mailbox_id").cloned().unwrap_or(Value::Null),
            "folder": args.get("folder").cloned().unwrap_or(Value::Null),
            "unread": args.get("unread").cloned().unwrap_or(Value::Null),
            "since": normalized_args.get("since").cloned().unwrap_or(Value::Null),
            "before": normalized_args.get("before").cloned().unwrap_or(Value::Null),
            "query": args.get("query").cloned().unwrap_or(Value::Null),
        },
        "offset":offset,"limit":limit,"count":rows.len(),"total_count":total_count,
        "next_offset":if offset+rows.len()<total_count{Some(offset+rows.len())}else{None},
        "messages": rows,
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
    Ok(json!({"schema":"narada.mailbox_mcp.accounts.v1","status":"ok","site_root":root.to_string_lossy(),"count":accounts.len(),"accounts":accounts}))
}

fn message_show(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let id = required(args, "message_id")?;
    let row = scan(root).messages.into_iter().find(|value|
        value.get("message_id").and_then(Value::as_str) == Some(id.as_str())
            && args.get("mailbox_id").and_then(Value::as_str).map(|mailbox| value.get("mailbox_id").and_then(Value::as_str) == Some(mailbox)).unwrap_or(true)
    );
    let include_raw=args.get("include_raw").and_then(Value::as_bool).unwrap_or(false);
    if include_raw&&row.as_ref().and_then(|value|value.get("raw")).map(|value|serde_json::to_vec(value).map(|bytes|bytes.len()).unwrap_or(MAX_RAW_RESPONSE_BYTES+1)>MAX_RAW_RESPONSE_BYTES).unwrap_or(false){return Err(error("mailbox_raw_payload_too_large","mailbox_raw_payload_too_large: use the bounded normalized projection"));}
    let message = row.as_ref().map(|value| summarize_message(value, true, args.get("include_html").and_then(Value::as_bool).unwrap_or(false), include_raw));
    Ok(json!({"schema":"narada.mailbox_mcp.message.v1","status":if row.is_some(){"ok"}else{"not_found"},"site_root":root.to_string_lossy(),"message_id":id,"message":message}))
}

fn thread_show(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let id = required(args, "thread_id")?;
    let offset = args.get("offset").and_then(Value::as_u64).unwrap_or(0) as usize;
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
    let total_count = values.len();
    let messages = values.into_iter().skip(offset).take(limit).map(|value| summarize_message(&value, include_body, false, false)).collect::<Vec<_>>();
    Ok(json!({"schema":"narada.mailbox_mcp.thread.v1","status":if total_count > 0{"ok"}else{"not_found"},"site_root":root.to_string_lossy(),"thread_id":id,"offset":offset,"limit":limit,"count":messages.len(),"total_count":total_count,"next_offset":if offset+messages.len()<total_count{Some(offset+messages.len())}else{None},"messages":messages}))
}
fn output_show(args:&Map<String,Value>,root:&Path)->Result<Value,Value>{let ref_value=args.get("ref").and_then(Value::as_str).map(str::trim);let alias_value=args.get("output_ref").and_then(Value::as_str).map(str::trim);if let(Some(left),Some(right))=(ref_value,alias_value){if left!=right{return Err(error("output_show_ref_alias_conflict","output_show_ref_alias_conflict"));}}let reference=ref_value.or(alias_value).ok_or_else(||error("output_ref_required","output_ref_required"))?;let id=reference.strip_prefix("mcp_output:").ok_or_else(||error("output_ref_invalid","output_ref_invalid"))?;if id.is_empty()||id.len()>100||!id.chars().all(|c|c.is_ascii_alphanumeric()||c=='-'||c=='_'){return Err(error("output_ref_invalid","output_ref_invalid"));}let path=root.join(".ai/tmp/mcp-outputs/workspace").join(format!("{id}.json"));let value=read_bounded(&path)?;if value.get("schema").and_then(Value::as_str)!=Some("narada.mcp_output_ref.v1")||value.get("ref").and_then(Value::as_str)!=Some(reference)||value.get("output_id").and_then(Value::as_str)!=Some(id){return Err(error("output_ref_metadata_mismatch","output_ref_metadata_mismatch"));}let full=value.get("full_output").cloned().unwrap_or(Value::Null);let presentation=serde_json::to_string_pretty(&full).unwrap_or_else(|_|full.to_string());let offset=args.get("offset").and_then(Value::as_u64).unwrap_or(0)as usize;let limit=args.get("limit").and_then(Value::as_u64).unwrap_or(4000).clamp(1,10000)as usize;let chars=presentation.chars().collect::<Vec<_>>();let start=offset.min(chars.len());let output_text=chars.iter().skip(start).take(limit).collect::<String>();let end=start+output_text.chars().count();Ok(json!({"schema":"narada.mcp_output_page.v1","status":"ok","ref":reference,"tool_name":value.get("tool_name"),"full_output_char_length":chars.len(),"byte_size":fs::metadata(&path).map(|v|v.len()).unwrap_or(0),"original_truncated":value.get("truncated").and_then(Value::as_bool).unwrap_or(false),"offset":start,"limit":limit,"next_offset":if end<chars.len(){Some(end)}else{None},"output_truncated":end<chars.len(),"output_text":output_text}))}
fn domain_db_path(root:&Path)->PathBuf{root.join(DOMAIN_DB_RELATIVE)}
fn open_domain_db(root:&Path)->Result<Option<Connection>,Value>{let path=domain_db_path(root);if !path.exists(){return Ok(None);}Connection::open_with_flags(path,OpenFlags::SQLITE_OPEN_READ_ONLY).map(Some).map_err(|e|error("mailbox_domain_store_open_failed",&e.to_string()))}
fn open_domain_db_write(root: &Path) -> Result<Connection, Value> {
    let path = domain_db_path(root);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| error("mailbox_domain_store_directory_failed", &e.to_string()))?;
    }
    let db = Connection::open(path)
        .map_err(|e| error("mailbox_domain_store_open_failed", &e.to_string()))?;
    db.pragma_update(None, "journal_mode", "WAL")
        .map_err(|e| error("mailbox_domain_store_pragma_failed", &e.to_string()))?;
    db.pragma_update(None, "foreign_keys", true)
        .map_err(|e| error("mailbox_domain_store_pragma_failed", &e.to_string()))?;
    init_outbox_schema(&db)?;
    Ok(db)
}

fn init_outbox_schema(db: &Connection) -> Result<(), Value> {
    db.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS mailbox_sync_generations(
          generation_id TEXT PRIMARY KEY,
          idempotency_key TEXT NOT NULL UNIQUE,
          request_fingerprint TEXT NOT NULL,
          scope_id TEXT NOT NULL,
          config_fingerprint TEXT NOT NULL,
          status TEXT NOT NULL CHECK(status IN ('accepted','staged','completed','failed')),
          parent_cursor TEXT,
          next_cursor TEXT,
          batch_path TEXT,
          batch_sha256 TEXT,
          batch_record_count INTEGER NOT NULL DEFAULT 0,
          staged_at TEXT,
          receipt_json TEXT,
          error_message TEXT,
          lease_token TEXT,
          lease_expires_at TEXT,
          created_at TEXT NOT NULL,
          updated_at TEXT NOT NULL,
          completed_at TEXT
        );
        CREATE TABLE IF NOT EXISTS mailbox_sync_generation_records(
          generation_id TEXT NOT NULL REFERENCES mailbox_sync_generations(generation_id),
          record_id TEXT NOT NULL,
          ordinal TEXT,
          fact_id TEXT NOT NULL,
          event_kind TEXT NOT NULL,
          message_id TEXT,
          mailbox_id TEXT,
          conversation_id TEXT,
          source_version TEXT,
          application_status TEXT NOT NULL CHECK(application_status IN ('staged','already_applied','projected','not_applied','reconciled')),
          PRIMARY KEY(generation_id, record_id)
        );
        CREATE TABLE IF NOT EXISTS mailbox_sync_scope_leases(
          scope_id TEXT PRIMARY KEY,
          generation_id TEXT NOT NULL REFERENCES mailbox_sync_generations(generation_id),
          lease_token TEXT NOT NULL,
          expires_at TEXT NOT NULL,
          updated_at TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS mailbox_message_observations(
          observation_id TEXT PRIMARY KEY,
          mailbox_id TEXT NOT NULL,
          message_id TEXT NOT NULL,
          first_generation_id TEXT NOT NULL REFERENCES mailbox_sync_generations(generation_id),
          first_fact_id TEXT NOT NULL,
          observed_at TEXT NOT NULL,
          UNIQUE(mailbox_id, message_id)
        );
        CREATE TABLE IF NOT EXISTS mailbox_outbox(
          event_id TEXT PRIMARY KEY,
          scope_id TEXT NOT NULL,
          topic TEXT NOT NULL,
          aggregate_id TEXT NOT NULL,
          aggregate_revision INTEGER NOT NULL,
          schema_version INTEGER NOT NULL,
          causation_id TEXT NOT NULL,
          idempotency_key TEXT NOT NULL UNIQUE,
          partition_key TEXT NOT NULL,
          occurred_at TEXT NOT NULL,
          payload_json TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS mailbox_outbox_consumers(
          consumer_id TEXT PRIMARY KEY,
          scope_id TEXT,
          topics_json TEXT,
          start_at TEXT NOT NULL,
          created_at TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS mailbox_outbox_receipts(
          consumer_id TEXT NOT NULL REFERENCES mailbox_outbox_consumers(consumer_id),
          event_id TEXT NOT NULL REFERENCES mailbox_outbox(event_id),
          receipt_fingerprint TEXT NOT NULL,
          receipt_json TEXT NOT NULL,
          acknowledged_at TEXT NOT NULL,
          PRIMARY KEY(consumer_id, event_id)
        );
        CREATE TABLE IF NOT EXISTS mailbox_admission_receipts(
          admission_id TEXT PRIMARY KEY,
          idempotency_key TEXT NOT NULL UNIQUE,
          request_fingerprint TEXT NOT NULL,
          scope_id TEXT NOT NULL,
          fact_id TEXT NOT NULL,
          policy_version TEXT NOT NULL,
          decision_json TEXT NOT NULL,
          created_at TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS mailbox_reconciliation_operations(
          operation_id TEXT PRIMARY KEY,
          idempotency_key TEXT NOT NULL UNIQUE,
          request_fingerprint TEXT NOT NULL,
          scope_id TEXT NOT NULL,
          generation_id TEXT NOT NULL REFERENCES mailbox_sync_generations(generation_id),
          result_json TEXT NOT NULL,
          created_at TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS mailbox_outbox_order_idx
          ON mailbox_outbox(occurred_at, event_id);
        CREATE INDEX IF NOT EXISTS mailbox_outbox_subscription_idx
          ON mailbox_outbox(scope_id, topic, occurred_at, event_id);
        CREATE INDEX IF NOT EXISTS mailbox_generation_scope_idx
          ON mailbox_sync_generations(scope_id, created_at);
        CREATE UNIQUE INDEX IF NOT EXISTS mailbox_admission_scope_fact_idx
          ON mailbox_admission_receipts(scope_id, fact_id);
        PRAGMA user_version = 2;
        "#,
    )
    .map_err(|e| error("mailbox_domain_schema_failed", &e.to_string()))?;
    Ok(())
}

fn generation_show(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let generation_id = required_bounded(args, "generation_id", "mailbox_generation_id_required", 128)?;
    let offset = bounded_integer(args.get("offset"), 0, 0, 1_000_000)?;
    let limit = bounded_integer(args.get("limit"), 100, 1, 100)?;
    let Some(db) = open_domain_db(root)? else {
        let code = format!("mailbox_sync_generation_not_found:{generation_id}");
        return Err(error(&code, &code));
    };
    let row: Option<(
        String,
        String,
        String,
        Option<String>,
        Option<String>,
        Option<String>,
        i64,
        Option<String>,
        Option<String>,
        String,
        String,
        Option<String>,
    )> = db
        .query_row(
            "SELECT scope_id,config_fingerprint,status,parent_cursor,next_cursor,batch_sha256,batch_record_count,receipt_json,error_message,created_at,updated_at,completed_at FROM mailbox_sync_generations WHERE generation_id=?",
            params![generation_id],
            |row| {
                Ok((
                    row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?,
                    row.get(6)?, row.get(7)?, row.get(8)?, row.get(9)?, row.get(10)?, row.get(11)?,
                ))
            },
        )
        .optional()
        .map_err(|e| error("mailbox_generation_query_failed", &e.to_string()))?;
    let Some((
        scope_id,
        config_fingerprint,
        status,
        parent_cursor,
        next_cursor,
        batch_sha256,
        batch_record_count,
        receipt_json,
        error_message,
        created_at,
        updated_at,
        completed_at,
    )) = row
    else {
        let code = format!("mailbox_sync_generation_not_found:{generation_id}");
        return Err(error(&code, &code));
    };
    let receipt = receipt_json
        .map(|value| serde_json::from_str::<Value>(&value))
        .transpose()
        .map_err(|e| error("mailbox_generation_receipt_invalid", &e.to_string()))?
        .unwrap_or(Value::Null);
    let mut statement = db
        .prepare("SELECT record_id,fact_id,event_kind,message_id,mailbox_id,conversation_id,source_version,application_status FROM mailbox_sync_generation_records WHERE generation_id=? ORDER BY rowid LIMIT ? OFFSET ?")
        .map_err(|e| error("mailbox_generation_record_query_failed", &e.to_string()))?;
    let rows = statement
        .query_map(params![generation_id,limit,offset], |row| {
            Ok(json!({
                "record_id":row.get::<_,String>(0)?,
                "fact_id":row.get::<_,String>(1)?,
                "event_kind":row.get::<_,String>(2)?,
                "message_id":row.get::<_,Option<String>>(3)?,
                "mailbox_id":row.get::<_,Option<String>>(4)?,
                "conversation_id":row.get::<_,Option<String>>(5)?,
                "source_version":row.get::<_,Option<String>>(6)?,
                "application_status":row.get::<_,String>(7)?
            }))
        })
        .map_err(|e| error("mailbox_generation_record_query_failed", &e.to_string()))?;
    let mut records = Vec::new();
    for row in rows {
        records.push(row.map_err(|e| error("mailbox_generation_record_row_failed", &e.to_string()))?);
    }
    let records_len = records.len() as i64;
    Ok(json!({
        "schema":"narada.mailbox.sync_generation.v1",
        "generation":{
            "generation_id":generation_id,
            "scope_id":scope_id,
            "config_fingerprint":config_fingerprint,
            "status":status,
            "parent_cursor_sha256":parent_cursor.map(|value| sha256_hex(value.as_bytes())),
            "next_cursor_sha256":next_cursor.map(|value| sha256_hex(value.as_bytes())),
            "batch_sha256":batch_sha256,
            "batch_record_count":batch_record_count,
            "receipt":receipt,
            "error_message":error_message,
            "created_at":created_at,
            "updated_at":updated_at,
            "completed_at":completed_at
        },
        "offset":offset,"limit":limit,"records":records,
        "next_offset":if offset+records_len<batch_record_count{Some(offset+records_len)}else{None},
        "records_truncated":offset+records_len<batch_record_count
    }))
}

fn message_fact_find(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let scope_id = required_bounded(args, "scope_id", "mailbox_fact_find_scope_id_required", 256)?;
    let message_id = required_bounded(args, "message_id", "mailbox_fact_find_message_id_required", 1024)?;
    let Some(db) = open_domain_db(root)? else {
        return Ok(json!({
            "schema":"narada.mailbox.message_fact_lookup.v1",
            "status":"not_found",
            "scope_id":scope_id,
            "message_id":message_id
        }));
    };
    let observation: Option<Value> = db
        .query_row(
            "SELECT observation.observation_id,observation.mailbox_id,observation.message_id,observation.first_generation_id,observation.first_fact_id,observation.observed_at,event.event_id FROM mailbox_message_observations observation JOIN mailbox_sync_generations generation ON generation.generation_id=observation.first_generation_id LEFT JOIN mailbox_outbox event ON event.aggregate_id=observation.observation_id AND event.topic='mailbox.message.first_observed' WHERE generation.scope_id=? AND observation.message_id=?",
            params![scope_id, message_id],
            |row| {
                Ok(json!({
                    "observation_id":row.get::<_,String>(0)?,
                    "mailbox_id":row.get::<_,String>(1)?,
                    "message_id":row.get::<_,String>(2)?,
                    "first_generation_id":row.get::<_,String>(3)?,
                    "first_fact_id":row.get::<_,String>(4)?,
                    "observed_at":row.get::<_,String>(5)?,
                    "event_id":row.get::<_,Option<String>>(6)?
                }))
            },
        )
        .optional()
        .map_err(|e| error("mailbox_fact_find_query_failed", &e.to_string()))?;
    if let Some(observation) = observation {
        Ok(json!({
            "schema":"narada.mailbox.message_fact_lookup.v1",
            "status":"ok",
            "scope_id":scope_id,
            "message_id":message_id,
            "fact_id":observation.get("first_fact_id"),
            "source_event_id":observation.get("event_id"),
            "observation":observation
        }))
    } else {
        Ok(json!({
            "schema":"narada.mailbox.message_fact_lookup.v1",
            "status":"not_found",
            "scope_id":scope_id,
            "message_id":message_id
        }))
    }
}

fn safe_fact_payload(value: &Value) -> Value {
    match value {
        Value::Array(values) => Value::Array(values.iter().map(safe_fact_payload).collect()),
        Value::Object(object) => Value::Object(
            object
                .iter()
                .map(|(key, value)| {
                    if key.eq_ignore_ascii_case("attachments") {
                        (key.clone(), metadata_only_attachment(value))
                    } else {
                        (key.clone(), safe_fact_payload(value))
                    }
                })
                .collect(),
        ),
        _ => value.clone(),
    }
}

fn fact_show(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let fact_id = required_bounded(args, "fact_id", "mailbox_fact_id_required", 256)?;
    let scope = load_mailbox_scope(args, root)?;
    let path = scope.root_dir.join(".narada/facts.db");
    if !path.is_file() {
        return Ok(json!({
            "schema":"narada.mailbox.immutable_fact.v1",
            "status":"not_found",
            "fact_id":fact_id,
            "scope_id":scope.scope_id
        }));
    }
    let fact = match load_mail_fact(&scope, &fact_id) {
        Ok(fact) => fact,
        Err(value)
            if value
                .get("code")
                .and_then(Value::as_str)
                .is_some_and(|code| code.contains("fact_not_found")) =>
        {
            return Ok(json!({
                "schema":"narada.mailbox.immutable_fact.v1",
                "status":"not_found",
                "fact_id":fact_id,
                "scope_id":scope.scope_id
            }));
        }
        Err(value) => return Err(value),
    };
    if fact.fact_type != "mail.message.discovered" {
        let code = format!("mailbox_fact_type_invalid:{}", fact.fact_type);
        return Err(error(&code, &code));
    }
    let metadata = mail_metadata(&fact)?;
    if metadata.mailbox_id != scope.scope_id {
        let code = format!("mailbox_fact_scope_mismatch:{}:{}", metadata.mailbox_id, scope.scope_id);
        return Err(error(&code, &code));
    }
    let include_content = args.get("include_content").and_then(Value::as_bool) == Some(true);
    if include_content && fact.payload_json.as_bytes().len() > 750 * 1024 {
        let code = format!("mailbox_fact_content_too_large:{}", fact.payload_json.as_bytes().len());
        return Err(error(&code, &code));
    }
    Ok(json!({
        "schema":"narada.mailbox.immutable_fact.v1",
        "status":"ok",
        "scope_id":scope.scope_id,
        "projection":if include_content { "full" } else { "safe" },
        "fact":{
            "fact_id":fact.fact_id,
            "fact_type":fact.fact_type,
            "provenance":fact.provenance,
            "payload_sha256":sha256_hex(fact.payload_json.as_bytes()),
            "payload":if include_content { fact.payload } else { safe_fact_payload(&fact.payload) },
            "payload_content_included":include_content,
            "created_at":fact.created_at
        }
    }))
}
fn outbox_consumer_show(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let consumer_id = required_bounded(args, "consumer_id", "mailbox_outbox_consumer_id_required", 256)?;
    let Some(db) = open_domain_db(root)? else {
        return Ok(json!({"schema":"narada.mailbox.outbox_consumer_lookup.v1","status":"not_found","consumer_id":consumer_id}));
    };
    let row: Option<(String, Option<String>, Option<String>, String, String)> = db
        .query_row(
            "SELECT consumer_id,scope_id,topics_json,start_at,created_at FROM mailbox_outbox_consumers WHERE consumer_id=?",
            params![consumer_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
        )
        .optional()
        .map_err(|e| error("mailbox_outbox_consumer_query_failed", &e.to_string()))?;
    let Some((consumer_id, scope_id, topics_json, start_at, created_at)) = row else {
        return Ok(json!({"schema":"narada.mailbox.outbox_consumer_lookup.v1","status":"not_found","consumer_id":consumer_id}));
    };
    let topics = parsed_topics(topics_json.as_deref(), &consumer_id)?;
    Ok(json!({
        "schema":"narada.mailbox.outbox_consumer_lookup.v1",
        "status":"ok",
        "consumer":{
            "consumer_id":consumer_id,
            "scope_id":scope_id,
            "topics":topics,
            "start_at":start_at,
            "created_at":created_at
        }
    }))
}

fn outbox_consumer_register(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let consumer_id = required_bounded(args, "consumer_id", "mailbox_outbox_consumer_id_required", 256)?;
    let scope_id = required_bounded(args, "scope_id", "mailbox_outbox_scope_id_required", 256)?;
    let topics = required_topics(args.get("topics"))?;
    let start_at = required_timestamp(args, "start_at", "mailbox_outbox_start_at_required")?;
    let topics_json = canonical_json(&Value::Array(topics.iter().cloned().map(Value::String).collect()));
    let now = now_iso_millis();
    let mut db = open_domain_db_write(root)?;
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|e| error("mailbox_domain_transaction_failed", &e.to_string()))?;
    let result = (|| {
        let existing: Option<(Option<String>, Option<String>, String, String)> = tx
            .query_row(
                "SELECT scope_id,topics_json,start_at,created_at FROM mailbox_outbox_consumers WHERE consumer_id=?",
                params![consumer_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()
            .map_err(|e| error("mailbox_outbox_consumer_query_failed", &e.to_string()))?;
        let created_at = if let Some((existing_scope, existing_topics, existing_start, created_at)) = existing {
            if existing_scope.is_none() && existing_topics.is_none() {
                tx.execute(
                    "UPDATE mailbox_outbox_consumers SET scope_id=?,topics_json=? WHERE consumer_id=?",
                    params![scope_id, topics_json, consumer_id],
                )
                .map_err(|e| error("mailbox_outbox_consumer_update_failed", &e.to_string()))?;
                created_at
            } else {
                if existing_scope.as_deref() != Some(scope_id.as_str())
                    || existing_topics.as_deref() != Some(topics_json.as_str())
                    || existing_start != start_at
                {
                    return Err(error(
                        &format!("mailbox_outbox_consumer_conflict:{consumer_id}"),
                        &format!("mailbox_outbox_consumer_conflict:{consumer_id}"),
                    ));
                }
                created_at
            }
        } else {
            tx.execute(
                "INSERT INTO mailbox_outbox_consumers(consumer_id,scope_id,topics_json,start_at,created_at) VALUES (?,?,?,?,?)",
                params![consumer_id, scope_id, topics_json, start_at, now],
            )
            .map_err(|e| error("mailbox_outbox_consumer_insert_failed", &e.to_string()))?;
            now.clone()
        };
        Ok(json!({
            "schema":"narada.mailbox.outbox_consumer.v2",
            "consumer":{
                "consumer_id":consumer_id,
                "scope_id":scope_id,
                "topics_json":topics_json,
                "start_at":start_at,
                "created_at":created_at
            }
        }))
    })();
    match result {
        Ok(value) => {
            tx.commit()
                .map_err(|e| error("mailbox_domain_transaction_commit_failed", &e.to_string()))?;
            Ok(value)
        }
        Err(value) => Err(value),
    }
}

fn outbox_list(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let consumer_id = required_bounded(args, "consumer_id", "mailbox_outbox_consumer_id_required", 256)?;
    let limit = bounded_integer(args.get("limit"), 100, 1, 100)? as usize;
    let Some(db) = open_domain_db(root)? else {
        return Err(error(
            &format!("mailbox_outbox_consumer_not_registered:{consumer_id}"),
            &format!("mailbox_outbox_consumer_not_registered:{consumer_id}"),
        ));
    };
    let consumer: Option<(Option<String>, Option<String>, String)> = db
        .query_row(
            "SELECT scope_id,topics_json,start_at FROM mailbox_outbox_consumers WHERE consumer_id=?",
            params![consumer_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()
        .map_err(|e| error("mailbox_outbox_consumer_query_failed", &e.to_string()))?;
    let Some((Some(scope_id), Some(topics_json), start_at)) = consumer else {
        let code = if consumer.is_some() {
            format!("mailbox_outbox_consumer_v2_registration_required:{consumer_id}")
        } else {
            format!("mailbox_outbox_consumer_not_registered:{consumer_id}")
        };
        return Err(error(&code, &code));
    };
    let _topics = parsed_topics(Some(&topics_json), &consumer_id)?;
    let mut statement = db
        .prepare(
            "SELECT event_id,scope_id,topic,aggregate_id,aggregate_revision,schema_version,causation_id,idempotency_key,partition_key,occurred_at,payload_json FROM mailbox_outbox event WHERE event.occurred_at>=? AND event.scope_id=? AND event.topic IN (SELECT value FROM json_each(?)) AND NOT EXISTS (SELECT 1 FROM mailbox_outbox_receipts receipt WHERE receipt.consumer_id=? AND receipt.event_id=event.event_id) ORDER BY event.occurred_at,event.event_id LIMIT ?",
        )
        .map_err(|e| error("mailbox_outbox_query_failed", &e.to_string()))?;
    let rows = statement
        .query_map(
            params![start_at, scope_id, topics_json, consumer_id, limit + 1],
            |row| {
                let payload_json: String = row.get(10)?;
                let payload = serde_json::from_str::<Value>(&payload_json).unwrap_or(Value::Null);
                Ok(json!({
                    "schema":"narada.mailbox.outbox_event.v1",
                    "event_id":row.get::<_,String>(0)?,
                    "scope_id":row.get::<_,String>(1)?,
                    "topic":row.get::<_,String>(2)?,
                    "aggregate_id":row.get::<_,String>(3)?,
                    "aggregate_revision":row.get::<_,i64>(4)?,
                    "schema_version":row.get::<_,i64>(5)?,
                    "causation_id":row.get::<_,String>(6)?,
                    "idempotency_key":row.get::<_,String>(7)?,
                    "partition_key":row.get::<_,String>(8)?,
                    "occurred_at":row.get::<_,String>(9)?,
                    "payload":payload
                }))
            },
        )
        .map_err(|e| error("mailbox_outbox_query_failed", &e.to_string()))?;
    let mut items = Vec::new();
    for row in rows {
        items.push(row.map_err(|e| error("mailbox_outbox_row_failed", &e.to_string()))?);
    }
    let has_more = items.len() > limit;
    if has_more {
        items.truncate(limit);
    }
    Ok(json!({
        "schema":"narada.mailbox.outbox_list.v2",
        "consumer_id":consumer_id,
        "count":items.len(),
        "items":items,
        "has_more":has_more
    }))
}

fn outbox_ack(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let consumer_id = required_bounded(args, "consumer_id", "mailbox_outbox_consumer_id_required", 256)?;
    let event_id = required_bounded(args, "event_id", "mailbox_outbox_event_id_required", 256)?;
    let raw_receipt = args
        .get("receipt")
        .and_then(Value::as_object)
        .ok_or_else(|| error("mailbox_outbox_receipt_required", "mailbox_outbox_receipt_required"))?;
    if raw_receipt
        .keys()
        .any(|key| !matches!(key.as_str(), "schema" | "outcome" | "effect_ref"))
    {
        return Err(error(
            "mailbox_outbox_receipt_fields_invalid",
            "mailbox_outbox_receipt_fields_invalid",
        ));
    }
    let receipt = json!({
        "schema":required_bounded(raw_receipt, "schema", "mailbox_outbox_receipt_schema_required", 128)?,
        "outcome":required_bounded(raw_receipt, "outcome", "mailbox_outbox_receipt_outcome_required", 64)?,
        "effect_ref":required_bounded(raw_receipt, "effect_ref", "mailbox_outbox_receipt_effect_ref_required", 512)?
    });
    let receipt_json = serde_json::to_string(&receipt)
        .map_err(|e| error("mailbox_outbox_receipt_encode_failed", &e.to_string()))?;
    let receipt_fingerprint = sha256_hex(canonical_json(&receipt).as_bytes());
    let now = now_iso_millis();
    let mut db = open_domain_db_write(root)?;
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|e| error("mailbox_domain_transaction_failed", &e.to_string()))?;
    let result = (|| {
        let consumer: Option<(Option<String>, Option<String>)> = tx
            .query_row(
                "SELECT scope_id,topics_json FROM mailbox_outbox_consumers WHERE consumer_id=?",
                params![consumer_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(|e| error("mailbox_outbox_consumer_query_failed", &e.to_string()))?;
        let Some((Some(scope_id), Some(topics_json))) = consumer else {
            let code = if consumer.is_some() {
                format!("mailbox_outbox_consumer_v2_registration_required:{consumer_id}")
            } else {
                format!("mailbox_outbox_consumer_not_registered:{consumer_id}")
            };
            return Err(error(&code, &code));
        };
        let topics = parsed_topics(Some(&topics_json), &consumer_id)?;
        let event: Option<(String, String)> = tx
            .query_row(
                "SELECT scope_id,topic FROM mailbox_outbox WHERE event_id=?",
                params![event_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(|e| error("mailbox_outbox_event_query_failed", &e.to_string()))?;
        let Some((event_scope, event_topic)) = event else {
            let code = format!("mailbox_outbox_event_not_found:{event_id}");
            return Err(error(&code, &code));
        };
        if event_scope != scope_id || !topics.iter().any(|topic| topic == &event_topic) {
            let code = format!("mailbox_outbox_event_not_subscribed:{consumer_id}:{event_id}");
            return Err(error(&code, &code));
        }
        let existing: Option<(String, String)> = tx
            .query_row(
                "SELECT receipt_fingerprint,receipt_json FROM mailbox_outbox_receipts WHERE consumer_id=? AND event_id=?",
                params![consumer_id, event_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(|e| error("mailbox_outbox_receipt_query_failed", &e.to_string()))?;
        if let Some((existing_fingerprint, existing_json)) = existing {
            if existing_fingerprint != receipt_fingerprint {
                let code = format!("mailbox_outbox_ack_conflict:{consumer_id}:{event_id}");
                return Err(error(&code, &code));
            }
            let existing_receipt = serde_json::from_str::<Value>(&existing_json)
                .map_err(|e| error("mailbox_outbox_receipt_invalid", &e.to_string()))?;
            return Ok(json!({
                "schema":"narada.mailbox.outbox_ack.v1",
                "consumer_id":consumer_id,
                "event_id":event_id,
                "replayed":true,
                "receipt":existing_receipt
            }));
        }
        tx.execute(
            "INSERT INTO mailbox_outbox_receipts(consumer_id,event_id,receipt_fingerprint,receipt_json,acknowledged_at) VALUES (?,?,?,?,?)",
            params![consumer_id, event_id, receipt_fingerprint, receipt_json, now],
        )
        .map_err(|e| error("mailbox_outbox_receipt_insert_failed", &e.to_string()))?;
        Ok(json!({
            "schema":"narada.mailbox.outbox_ack.v1",
            "consumer_id":consumer_id,
            "event_id":event_id,
            "replayed":false,
            "receipt":receipt
        }))
    })();
    match result {
        Ok(value) => {
            tx.commit()
                .map_err(|e| error("mailbox_domain_transaction_commit_failed", &e.to_string()))?;
            Ok(value)
        }
        Err(value) => Err(value),
    }
}

#[derive(Clone)]
struct MailboxScope {
    scope_id: String,
    root_dir: PathBuf,
    graph_mailbox_id: Option<String>,
    admission: Value,
}

struct MailFact {
    fact_id: String,
    fact_type: String,
    provenance: Value,
    payload_json: String,
    payload: Value,
    created_at: String,
}

#[derive(Clone)]
struct FirstObservationCandidate {
    mailbox_id: String,
    message_id: String,
    fact_id: String,
    conversation_id: Option<String>,
}

struct MailMetadata {
    mailbox_id: String,
    message_id: String,
    conversation_id: Option<String>,
    internet_message_id: Option<String>,
    subject: Option<String>,
}

fn load_mailbox_scope(args: &Map<String, Value>, root: &Path) -> Result<MailboxScope, Value> {
    let config_argument = args
        .get("config_path")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("config/config.json");
    if config_argument.chars().count() > 1024 {
        return Err(error("mailbox_string_argument_too_long", "mailbox_string_argument_too_long"));
    }
    let requested = args
        .get("scope_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string);
    if requested.as_ref().is_some_and(|value| value.chars().count() > 256) {
        return Err(error("mailbox_string_argument_too_long", "mailbox_string_argument_too_long"));
    }
    let candidate = PathBuf::from(config_argument);
    let config_path = if candidate.is_absolute() { candidate } else { root.join(candidate) };
    let root_canonical = fs::canonicalize(root)
        .map_err(|e| error("mailbox_site_root_invalid", &e.to_string()))?;
    let config_canonical = fs::canonicalize(&config_path)
        .map_err(|e| error("mailbox_config_read_failed", &e.to_string()))?;
    if !config_canonical.starts_with(&root_canonical) {
        return Err(error(
            "mailbox_config_path_outside_site",
            &format!("mailbox_config_path_outside_site:{}", config_path.to_string_lossy()),
        ));
    }
    if fs::metadata(&config_canonical)
        .map_err(|e| error("mailbox_config_read_failed", &e.to_string()))?
        .len()
        > MAX_BYTES
    {
        return Err(error("mailbox_config_too_large", "mailbox_config_too_large"));
    }
    let document: Value = serde_json::from_str(
        &fs::read_to_string(&config_canonical)
            .map_err(|e| error("mailbox_config_read_failed", &e.to_string()))?,
    )
    .map_err(|e| error("mailbox_config_invalid", &e.to_string()))?;
    let scopes = document
        .get("scopes")
        .and_then(Value::as_array)
        .ok_or_else(|| error("mailbox_config_scopes_invalid", "mailbox_config_scopes_invalid"))?;
    let scope = if let Some(requested) = requested.as_deref() {
        scopes.iter().find(|scope| {
            scope.get("scope_id").and_then(Value::as_str) == Some(requested)
        })
    } else if scopes.len() == 1 {
        scopes.first()
    } else {
        None
    };
    let scope = scope.ok_or_else(|| {
        if let Some(requested) = requested.as_deref() {
            error(
                &format!("mailbox_scope_not_found:{requested}"),
                &format!("mailbox_scope_not_found:{requested}"),
            )
        } else {
            error("mailbox_scope_id_required", "mailbox_scope_id_required")
        }
    })?;
    let scope_object = scope
        .as_object()
        .ok_or_else(|| error("mailbox_scope_invalid", "mailbox_scope_invalid"))?;
    let scope_id = required_bounded(scope_object, "scope_id", "mailbox_scope_id_required", 256)?;
    let scope_root = required_bounded(scope_object, "root_dir", "mailbox_scope_root_required", 1024)?;
    let scope_root_candidate = PathBuf::from(scope_root);
    let scope_root_path = if scope_root_candidate.is_absolute() {
        scope_root_candidate
    } else {
        root.join(scope_root_candidate)
    };
    let scope_root_canonical = fs::canonicalize(&scope_root_path)
        .map_err(|e| error("mailbox_scope_root_invalid", &e.to_string()))?;
    if !scope_root_canonical.starts_with(&root_canonical) {
        return Err(error(
            "mailbox_scope_root_outside_site",
            &format!("mailbox_scope_root_outside_site:{}", scope_root_path.to_string_lossy()),
        ));
    }
    let graph = scope.get("graph").and_then(Value::as_object).or_else(|| {
        scope
            .get("sources")
            .and_then(Value::as_array)
            .and_then(|sources| {
                sources.iter().find(|source| {
                    source.get("type").and_then(Value::as_str) == Some("graph")
                })
            })
            .and_then(Value::as_object)
    });
    let graph_mailbox_id = graph.and_then(|graph| {
        graph
            .get("mailbox_id")
            .or_else(|| graph.get("user_id"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string)
    });
    let admission = scope
        .get("admission")
        .and_then(|value| value.get("mail"))
        .cloned()
        .unwrap_or_else(|| json!({}));
    Ok(MailboxScope {
        scope_id,
        root_dir: scope_root_canonical,
        graph_mailbox_id,
        admission,
    })
}

fn load_mail_fact(scope: &MailboxScope, fact_id: &str) -> Result<MailFact, Value> {
    let path = scope.root_dir.join(".narada/facts.db");
    if !path.is_file() {
        return Err(error(
            &format!("mailbox_reconciliation_fact_db_missing:{}", path.to_string_lossy()),
            &format!("mailbox_reconciliation_fact_db_missing:{}", path.to_string_lossy()),
        ));
    }
    let db = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|e| error("mailbox_fact_store_open_failed", &e.to_string()))?;
    let row: Option<(String, String, String, String)> = db
        .query_row(
            "SELECT fact_type,provenance_json,payload_json,created_at FROM facts WHERE fact_id=?",
            params![fact_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()
        .map_err(|e| error("mailbox_fact_query_failed", &e.to_string()))?;
    let Some((fact_type, provenance_json, payload_json, created_at)) = row else {
        return Err(error(
            &format!("mailbox_reconciliation_fact_not_found:{fact_id}"),
            &format!("mailbox_reconciliation_fact_not_found:{fact_id}"),
        ));
    };
    let provenance = serde_json::from_str(&provenance_json)
        .map_err(|e| error("mailbox_fact_provenance_invalid", &e.to_string()))?;
    let payload = serde_json::from_str(&payload_json)
        .map_err(|e| error("mailbox_fact_payload_invalid", &e.to_string()))?;
    Ok(MailFact {
        fact_id: fact_id.to_string(),
        fact_type,
        provenance,
        payload_json,
        payload,
        created_at,
    })
}

fn mail_metadata(fact: &MailFact) -> Result<MailMetadata, Value> {
    let envelope = fact
        .payload
        .as_object()
        .ok_or_else(|| error("mailbox_fact_payload_invalid", "mailbox_fact_payload_invalid"))?;
    let event = envelope
        .get("event")
        .and_then(Value::as_object)
        .ok_or_else(|| error("mailbox_fact_event_invalid", "mailbox_fact_event_invalid"))?;
    let payload = event
        .get("payload")
        .and_then(Value::as_object);
    let value = |key: &str| {
        event
            .get(key)
            .or_else(|| payload.and_then(|payload| payload.get(key)))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string)
    };
    let mailbox_id = value("mailbox_id")
        .ok_or_else(|| error("mailbox_fact_mailbox_id_missing", "mailbox_fact_mailbox_id_missing"))?;
    let message_id = value("message_id")
        .ok_or_else(|| error("mailbox_fact_message_id_missing", "mailbox_fact_message_id_missing"))?;
    Ok(MailMetadata {
        mailbox_id,
        message_id,
        conversation_id: value("conversation_id"),
        internet_message_id: payload
            .and_then(|payload| payload.get("internet_message_id"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string),
        subject: payload
            .and_then(|payload| payload.get("subject"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| value.chars().take(500).collect()),
    })
}

fn stable_id(prefix: &str, value: &str) -> String {
    format!("{prefix}{}", &sha256_hex(value.as_bytes())[..40])
}

fn reconcile_first_observations(
    args: &Map<String, Value>,
    root: &Path,
) -> Result<Value, Value> {
    let idempotency_key = required_bounded(
        args,
        "idempotency_key",
        "mailbox_reconciliation_idempotency_key_required",
        512,
    )?;
    let generation_id = required_bounded(
        args,
        "generation_id",
        "mailbox_reconciliation_generation_id_required",
        128,
    )?;
    let scope = load_mailbox_scope(args, root)?;
    let limit = bounded_integer(args.get("limit"), 100, 1, 100)? as usize;
    let mut db = open_domain_db_write(root)?;
    let mut observed = HashSet::new();
    {
        let mut statement = db
            .prepare("SELECT mailbox_id,message_id FROM mailbox_message_observations WHERE mailbox_id=?")
            .map_err(|e| error("mailbox_observation_query_failed", &e.to_string()))?;
        let rows = statement
            .query_map(params![scope.scope_id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(|e| error("mailbox_observation_query_failed", &e.to_string()))?;
        for row in rows {
            let (mailbox_id, message_id) =
                row.map_err(|e| error("mailbox_observation_row_failed", &e.to_string()))?;
            observed.insert(format!("{mailbox_id}\0{message_id}"));
        }
    }
    let mut candidates = Vec::new();
    let mut candidate_identities = HashSet::new();
    {
        let mut statement = db
            .prepare("SELECT fact_id,event_kind,message_id,mailbox_id,conversation_id,application_status FROM mailbox_sync_generation_records WHERE generation_id=? ORDER BY rowid")
            .map_err(|e| error("mailbox_generation_record_query_failed", &e.to_string()))?;
        let rows = statement
            .query_map(params![generation_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, String>(5)?,
                ))
            })
            .map_err(|e| error("mailbox_generation_record_query_failed", &e.to_string()))?;
        for row in rows {
            let (fact_id, event_kind, message_id, mailbox_id, conversation_id, application_status) =
                row.map_err(|e| error("mailbox_generation_record_row_failed", &e.to_string()))?;
            if application_status == "not_applied" || matches!(event_kind.as_str(), "delete" | "deleted") {
                continue;
            }
            let (Some(message_id), Some(mailbox_id)) = (message_id, mailbox_id) else {
                continue;
            };
            if mailbox_id != scope.scope_id {
                let code = format!("mailbox_reconciliation_scope_mismatch:{mailbox_id}:{}", scope.scope_id);
                return Err(error(&code, &code));
            }
            let identity = format!("{mailbox_id}\0{message_id}");
            if !observed.contains(&identity) && candidate_identities.insert(identity) {
                candidates.push(FirstObservationCandidate {
                    mailbox_id,
                    message_id,
                    fact_id,
                    conversation_id,
                });
            }
        }
    }
    let unobserved_count = candidates.len();
    candidates.truncate(limit);
    for candidate in &mut candidates {
        let fact = load_mail_fact(&scope, &candidate.fact_id).map_err(|value| {
            let message = value
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("mailbox_reconciliation_fact_validation_failed");
            error(
                &format!("mailbox_reconciliation_fact_validation_failed:{message}"),
                &format!("mailbox_reconciliation_fact_validation_failed:{message}"),
            )
        })?;
        if fact.fact_type != "mail.message.discovered" {
            let code = format!("mailbox_reconciliation_fact_type_invalid:{}", fact.fact_type);
            return Err(error(
                &format!("mailbox_reconciliation_fact_validation_failed:{code}"),
                &format!("mailbox_reconciliation_fact_validation_failed:{code}"),
            ));
        }
        let metadata = mail_metadata(&fact)?;
        if metadata.mailbox_id != candidate.mailbox_id || metadata.message_id != candidate.message_id {
            let code = format!("mailbox_reconciliation_fact_identity_mismatch:{}", candidate.fact_id);
            return Err(error(
                &format!("mailbox_reconciliation_fact_validation_failed:{code}"),
                &format!("mailbox_reconciliation_fact_validation_failed:{code}"),
            ));
        }
        if metadata.conversation_id.is_some() {
            candidate.conversation_id = metadata.conversation_id;
        }
    }
    let request_fingerprint = sha256_hex(
        canonical_json(&json!({
            "schema":"narada.mailbox.reconcile_first_observations_request.v1",
            "scope_id":scope.scope_id,
            "generation_id":generation_id,
            "limit":limit
        }))
        .as_bytes(),
    );
    let operation_id = stable_id("mbr_", &idempotency_key);
    let remaining_unobserved = unobserved_count.saturating_sub(candidates.len());
    let has_more = unobserved_count > candidates.len();
    let now = now_iso_millis();
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|e| error("mailbox_domain_transaction_failed", &e.to_string()))?;
    let result = (|| {
        let existing: Option<(String, String)> = tx
            .query_row(
                "SELECT request_fingerprint,result_json FROM mailbox_reconciliation_operations WHERE idempotency_key=?",
                params![idempotency_key],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(|e| error("mailbox_reconciliation_query_failed", &e.to_string()))?;
        if let Some((existing_fingerprint, existing_json)) = existing {
            if existing_fingerprint != request_fingerprint {
                let code = format!("mailbox_reconciliation_idempotency_conflict:{idempotency_key}");
                return Err(error(&code, &code));
            }
            let mut replay = serde_json::from_str::<Value>(&existing_json)
                .map_err(|e| error("mailbox_reconciliation_receipt_invalid", &e.to_string()))?;
            if let Some(object) = replay.as_object_mut() {
                object.insert("idempotency_replayed".to_string(), Value::Bool(true));
            }
            return Ok(json!({
                "schema":"narada.domain_operation.v1",
                "operation_ref":format!("mailbox-reconcile:{operation_id}"),
                "outcome":"completed",
                "result":replay
            }));
        }
        let generation: Option<(String, String)> = tx
            .query_row(
                "SELECT scope_id,status FROM mailbox_sync_generations WHERE generation_id=?",
                params![generation_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(|e| error("mailbox_sync_generation_query_failed", &e.to_string()))?;
        let Some((generation_scope, generation_status)) = generation else {
            let code = format!("mailbox_sync_generation_not_found:{generation_id}");
            return Err(error(&code, &code));
        };
        if generation_scope != scope.scope_id {
            let code = format!(
                "mailbox_reconciliation_scope_mismatch:{}:{generation_scope}",
                scope.scope_id
            );
            return Err(error(&code, &code));
        }
        if generation_status != "completed" {
            let code = format!("mailbox_reconciliation_generation_not_completed:{generation_status}");
            return Err(error(&code, &code));
        }
        let mut observations_recorded = 0_i64;
        let mut events_published = 0_i64;
        let mut skipped_existing = 0_i64;
        for candidate in &candidates {
            let identity = format!("{}\0{}", candidate.mailbox_id, candidate.message_id);
            let observation_id = stable_id("mobs_", &identity);
            let observation_changes = tx
                .execute(
                    "INSERT OR IGNORE INTO mailbox_message_observations(observation_id,mailbox_id,message_id,first_generation_id,first_fact_id,observed_at) VALUES (?,?,?,?,?,?)",
                    params![observation_id,candidate.mailbox_id,candidate.message_id,generation_id,candidate.fact_id,now],
                )
                .map_err(|e| error("mailbox_observation_insert_failed", &e.to_string()))?;
            if observation_changes == 1 {
                observations_recorded += 1;
            } else {
                skipped_existing += 1;
            }
            let event_id = stable_id("mbe_", &format!("first-observed\0{identity}"));
            let mut payload = json!({
                "schema":"narada.mailbox.message_first_observed.v1",
                "generation_id":generation_id,
                "observation_id":observation_id,
                "mailbox_id":candidate.mailbox_id,
                "message_id":candidate.message_id,
                "fact_id":candidate.fact_id
            });
            if let Some(conversation_id) = &candidate.conversation_id {
                payload
                    .as_object_mut()
                    .expect("payload object")
                    .insert("conversation_id".to_string(), Value::String(conversation_id.clone()));
            }
            let event_changes = tx
                .execute(
                    "INSERT OR IGNORE INTO mailbox_outbox(event_id,scope_id,topic,aggregate_id,aggregate_revision,schema_version,causation_id,idempotency_key,partition_key,occurred_at,payload_json) VALUES (?,?,'mailbox.message.first_observed',?,1,1,?,?,?,?,?)",
                    params![event_id,scope.scope_id,observation_id,generation_id,event_id,observation_id,now,serde_json::to_string(&payload).unwrap_or_else(|_| "{}".to_string())],
                )
                .map_err(|e| error("mailbox_outbox_insert_failed", &e.to_string()))?;
            if event_changes == 1 {
                events_published += 1;
            }
        }
        let receipt = json!({
            "schema":"narada.mailbox.reconcile_first_observations_receipt.v1",
            "operation_id":operation_id,
            "scope_id":scope.scope_id,
            "generation_id":generation_id,
            "candidates_scanned":candidates.len(),
            "observations_recorded":observations_recorded,
            "events_published":events_published,
            "skipped_existing":skipped_existing,
            "remaining_unobserved":remaining_unobserved,
            "has_more":has_more,
            "status":"completed"
        });
        tx.execute(
            "INSERT INTO mailbox_reconciliation_operations(operation_id,idempotency_key,request_fingerprint,scope_id,generation_id,result_json,created_at) VALUES (?,?,?,?,?,?,?)",
            params![operation_id,idempotency_key,request_fingerprint,scope.scope_id,generation_id,serde_json::to_string(&receipt).unwrap_or_else(|_| "{}".to_string()),now],
        )
        .map_err(|e| error("mailbox_reconciliation_insert_failed", &e.to_string()))?;
        let mut result = receipt;
        result
            .as_object_mut()
            .expect("receipt object")
            .insert("idempotency_replayed".to_string(), Value::Bool(false));
        Ok(json!({
            "schema":"narada.domain_operation.v1",
            "operation_ref":format!("mailbox-reconcile:{operation_id}"),
            "outcome":"completed",
            "result":result
        }))
    })();
    match result {
        Ok(value) => {
            tx.commit()
                .map_err(|e| error("mailbox_domain_transaction_commit_failed", &e.to_string()))?;
            Ok(value)
        }
        Err(value) => Err(value),
    }
}

struct AdmissionEvaluation {
    admitted: bool,
    reason: &'static str,
    folder_refs: Vec<String>,
    sender_email: Option<String>,
}

fn fact_event(fact: &MailFact) -> Option<(&Map<String, Value>, Option<&Map<String, Value>>)> {
    let event = fact.payload.get("event")?.as_object()?;
    Some((event, event.get("payload").and_then(Value::as_object)))
}

fn string_set(value: Option<&Value>) -> HashSet<String> {
    value
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.to_ascii_lowercase())
        .collect()
}

fn fact_folder_refs(fact: &MailFact) -> Vec<String> {
    let mut refs = HashSet::new();
    if let Some((_, Some(payload))) = fact_event(fact) {
        refs.extend(string_set(payload.get("folder_refs")));
        if let Some(graph) = payload
            .get("source_extensions")
            .and_then(|value| value.get("namespaces"))
            .and_then(|value| value.get("graph"))
            .and_then(Value::as_object)
        {
            for key in ["parent_folder_id", "queried_folder_ref"] {
                if let Some(value) = graph
                    .get(key)
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                {
                    refs.insert(value.to_ascii_lowercase());
                }
            }
        }
    }
    let mut values = refs.into_iter().collect::<Vec<_>>();
    values.sort();
    values
}

fn email_from(value: Option<&Value>) -> Option<String> {
    match value {
        Some(Value::String(value)) if value.contains('@') => Some(value.trim().to_ascii_lowercase()),
        Some(Value::Object(value)) => value
            .get("email")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| value.contains('@'))
            .map(|value| value.to_ascii_lowercase()),
        _ => None,
    }
}

fn fact_sender_email(fact: &MailFact) -> Option<String> {
    let (event, payload) = fact_event(fact)?;
    email_from(event.get("from").or_else(|| payload.and_then(|payload| payload.get("from"))))
        .or_else(|| {
            email_from(
                event
                    .get("sender")
                    .or_else(|| payload.and_then(|payload| payload.get("sender"))),
            )
        })
}

fn participant_emails(fact: &MailFact, fields: &[String]) -> HashSet<String> {
    let requested = if fields.iter().any(|field| field == "any_participant") {
        ["from", "sender", "to", "cc", "bcc"]
            .into_iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
    } else {
        fields.to_vec()
    };
    let mut emails = HashSet::new();
    let Some((event, payload)) = fact_event(fact) else {
        return emails;
    };
    for field in requested {
        for value in [event.get(&field), payload.and_then(|payload| payload.get(&field))]
            .into_iter()
            .flatten()
        {
            if let Some(values) = value.as_array() {
                for value in values {
                    if let Some(email) = email_from(Some(value)) {
                        emails.insert(email);
                    }
                }
            } else if let Some(email) = email_from(Some(value)) {
                emails.insert(email);
            }
        }
    }
    emails
}

enum PredicateMatch {
    Yes,
    No,
    Unknown,
}

fn predicate_match(fact: &MailFact, predicate: &Value) -> PredicateMatch {
    let Some(predicate) = predicate.as_object() else {
        return PredicateMatch::No;
    };
    if predicate.get("kind").and_then(Value::as_str) != Some("participant") {
        return PredicateMatch::No;
    }
    let fields = predicate
        .get("fields")
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .map(ToString::to_string)
                .collect::<Vec<_>>()
        })
        .filter(|values| !values.is_empty())
        .unwrap_or_else(|| vec!["any_participant".to_string()]);
    let emails = participant_emails(fact, &fields);
    if emails.is_empty() {
        return PredicateMatch::Unknown;
    }
    let addresses = string_set(predicate.get("addresses"));
    let domains = string_set(predicate.get("domains"));
    if addresses.is_empty() && domains.is_empty() {
        return PredicateMatch::Yes;
    }
    if emails.iter().any(|email| {
        addresses.contains(email)
            || email
                .rsplit_once('@')
                .is_some_and(|(_, domain)| domains.contains(&domain.to_ascii_lowercase()))
    }) {
        PredicateMatch::Yes
    } else {
        PredicateMatch::No
    }
}

fn evaluate_admission(fact: &MailFact, admission: &Value) -> AdmissionEvaluation {
    let folder_refs = fact_folder_refs(fact);
    let sender_email = fact_sender_email(fact);
    let decision = |admitted, reason| AdmissionEvaluation {
        admitted,
        reason,
        folder_refs: folder_refs.clone(),
        sender_email: sender_email.clone(),
    };
    if fact.fact_type != "mail.message.discovered" {
        return decision(true, "not_subject_to_new_message_policy");
    }
    let Some(admission) = admission.as_object() else {
        return decision(true, "no_policy_restrictions");
    };
    if admission.is_empty() {
        return decision(true, "no_policy_restrictions");
    }
    let included_folders = string_set(admission.get("included_folder_refs"));
    let excluded_folders = string_set(admission.get("excluded_folder_refs"));
    if folder_refs.iter().any(|value| excluded_folders.contains(value)) {
        return decision(false, "excluded_folder");
    }
    if !included_folders.is_empty()
        && !folder_refs.iter().any(|value| included_folders.contains(value))
    {
        return decision(false, "included_folder_not_matched");
    }
    if let Some(predicates) = admission.get("predicates").and_then(Value::as_object) {
        let unknown_admitted = predicates
            .get("unknown_participant_behavior")
            .or_else(|| admission.get("unknown_sender_behavior"))
            .and_then(Value::as_str)
            == Some("admit");
        if let Some(excluded) = predicates.get("exclude").and_then(Value::as_array) {
            if excluded.iter().any(|predicate| {
                matches!(predicate_match(fact, predicate), PredicateMatch::Yes)
                    || (unknown_admitted
                        && matches!(predicate_match(fact, predicate), PredicateMatch::Unknown))
            }) {
                return decision(false, "excluded_predicate");
            }
        }
        if let Some(included) = predicates
            .get("include")
            .and_then(Value::as_array)
            .filter(|values| !values.is_empty())
        {
            let mut saw_unknown = false;
            let mut matched = false;
            for predicate in included {
                match predicate_match(fact, predicate) {
                    PredicateMatch::Yes => matched = true,
                    PredicateMatch::Unknown => saw_unknown = true,
                    PredicateMatch::No => {}
                }
            }
            if !matched && !(saw_unknown && unknown_admitted) {
                return decision(false, "included_predicate_not_matched");
            }
        }
    }
    let addresses = string_set(admission.get("allowed_sender_addresses"));
    let domains = string_set(admission.get("allowed_sender_domains"));
    if addresses.is_empty() && domains.is_empty() {
        return decision(true, "admitted");
    }
    let Some(sender) = sender_email.as_deref() else {
        return decision(
            admission.get("unknown_sender_behavior").and_then(Value::as_str) == Some("admit"),
            if admission.get("unknown_sender_behavior").and_then(Value::as_str) == Some("admit") {
                "admitted"
            } else {
                "sender_unknown_rejected"
            },
        );
    };
    let domain = sender.rsplit_once('@').map(|(_, domain)| domain.to_ascii_lowercase());
    if addresses.contains(sender) || domain.as_ref().is_some_and(|domain| domains.contains(domain)) {
        decision(true, "admitted")
    } else {
        decision(false, "sender_not_allowed")
    }
}

fn admission_show(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let scope_id = required_bounded(args, "scope_id", "mailbox_admission_scope_id_required", 256)?;
    let fact_id = required_bounded(args, "fact_id", "mailbox_admission_fact_id_required", 256)?;
    let Some(db) = open_domain_db(root)? else {
        return Ok(json!({"schema":"narada.mailbox.admission_show.v1","status":"not_found","scope_id":scope_id,"fact_id":fact_id}));
    };
    let decision_json: Option<String> = db
        .query_row(
            "SELECT decision_json FROM mailbox_admission_receipts WHERE scope_id=? AND fact_id=?",
            params![scope_id, fact_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(|e| error("mailbox_admission_query_failed", &e.to_string()))?;
    let Some(decision_json) = decision_json else {
        return Ok(json!({"schema":"narada.mailbox.admission_show.v1","status":"not_found","scope_id":scope_id,"fact_id":fact_id}));
    };
    let admission = serde_json::from_str::<Value>(&decision_json)
        .map_err(|e| error("mailbox_admission_receipt_invalid", &e.to_string()))?;
    Ok(json!({
        "schema":"narada.mailbox.admission_show.v1",
        "status":"ok",
        "scope_id":scope_id,
        "fact_id":fact_id,
        "admission":admission
    }))
}

fn admit_message(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let idempotency_key = required_bounded(
        args,
        "idempotency_key",
        "mailbox_admission_idempotency_key_required",
        512,
    )?;
    let fact_id = required_bounded(args, "fact_id", "mailbox_admission_fact_id_required", 256)?;
    let source_event_id = required_bounded(
        args,
        "source_event_id",
        "mailbox_admission_source_event_id_required",
        256,
    )?;
    let scope = load_mailbox_scope(args, root)?;
    let policy_version = format!(
        "sha256:{}",
        sha256_hex(
            canonical_json(&json!({
                "schema":"narada.mailbox.admission_policy.v1",
                "scope_id":scope.scope_id,
                "policy":scope.admission
            }))
            .as_bytes()
        )
    );
    if let Some(expected) = args
        .get("policy_version")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        if expected != policy_version {
            let code = format!("mailbox_admission_policy_version_mismatch:{expected}:{policy_version}");
            return Err(error(&code, &code));
        }
    }
    let fact = load_mail_fact(&scope, &fact_id).map_err(|value| {
        if value
            .get("code")
            .and_then(Value::as_str)
            .is_some_and(|code| code.contains("fact_not_found"))
        {
            let code = format!("mailbox_admission_fact_not_found:{fact_id}");
            error(&code, &code)
        } else {
            value
        }
    })?;
    if fact.fact_type != "mail.message.discovered" {
        let code = format!("mailbox_admission_fact_type_invalid:{}", fact.fact_type);
        return Err(error(&code, &code));
    }
    let metadata = mail_metadata(&fact)?;
    if metadata.mailbox_id != scope.scope_id {
        let code = format!(
            "mailbox_admission_scope_mismatch:{}:{}",
            metadata.mailbox_id, scope.scope_id
        );
        return Err(error(&code, &code));
    }
    let mut db = open_domain_db_write(root)?;
    let source_event: Option<(String, String, String)> = db
        .query_row(
            "SELECT scope_id,topic,payload_json FROM mailbox_outbox WHERE event_id=?",
            params![source_event_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()
        .map_err(|e| error("mailbox_admission_source_event_query_failed", &e.to_string()))?;
    let Some((event_scope, event_topic, event_payload_json)) = source_event else {
        let code = format!("mailbox_admission_source_event_not_found:{source_event_id}");
        return Err(error(&code, &code));
    };
    let event_payload = serde_json::from_str::<Value>(&event_payload_json)
        .map_err(|e| error("mailbox_admission_source_event_invalid", &e.to_string()))?;
    if event_topic != "mailbox.message.first_observed"
        || event_scope != scope.scope_id
        || event_payload.get("fact_id").and_then(Value::as_str) != Some(fact_id.as_str())
        || event_payload.get("mailbox_id").and_then(Value::as_str) != Some(scope.scope_id.as_str())
    {
        let code = format!("mailbox_admission_source_event_mismatch:{source_event_id}");
        return Err(error(&code, &code));
    }
    let evaluation = evaluate_admission(&fact, &scope.admission);
    let request_fingerprint = sha256_hex(
        canonical_json(&json!({
            "schema":"narada.mailbox.message_admission_request.v2",
            "scope_id":scope.scope_id,
            "fact_id":fact_id,
            "source_event_id":source_event_id,
            "policy_version":policy_version
        }))
        .as_bytes(),
    );
    let admission_id = stable_id("mba_", &format!("{}\0{fact_id}", scope.scope_id));
    let provenance = fact.provenance.as_object().cloned().unwrap_or_default();
    let source_record_id = provenance
        .get("source_record_id")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let source_version = provenance
        .get("source_version")
        .cloned()
        .unwrap_or(Value::Null);
    let graph_mailbox_id = scope.graph_mailbox_id.clone().ok_or_else(|| {
        let code = format!("mailbox_scope_graph_user_id_required:{}", scope.scope_id);
        error(&code, &code)
    })?;
    let mut source_ref = json!({
        "schema":"narada.mailbox.source_ref.v1",
        "scope_id":scope.scope_id,
        "mailbox_id":graph_mailbox_id,
        "message_id":metadata.message_id,
        "fact_id":fact_id,
        "source_record_id":source_record_id,
        "source_version":source_version
    });
    if let Some(conversation_id) = &metadata.conversation_id {
        source_ref
            .as_object_mut()
            .expect("source ref")
            .insert("conversation_id".to_string(), Value::String(conversation_id.clone()));
    }
    if let Some(internet_message_id) = &metadata.internet_message_id {
        source_ref
            .as_object_mut()
            .expect("source ref")
            .insert(
                "internet_message_id".to_string(),
                Value::String(internet_message_id.clone()),
            );
    }
    let mut correlation_keys = Vec::new();
    if let Some(conversation_id) = &metadata.conversation_id {
        correlation_keys.push(json!({
            "kind":"mailbox_conversation",
            "scope":metadata.mailbox_id,
            "value":conversation_id
        }));
    }
    if let Some(internet_message_id) = &metadata.internet_message_id {
        correlation_keys.push(json!({
            "kind":"internet_message_id",
            "scope":"rfc5322",
            "value":internet_message_id
        }));
    }
    let summary = metadata
        .subject
        .as_ref()
        .map(|subject| format!("Mailbox message: {subject}").chars().take(500).collect::<String>())
        .unwrap_or_else(|| "Mailbox message".to_string());
    let source = json!({
        "source_kind":"mailbox_message",
        "source_scope":metadata.mailbox_id,
        "immutable_source_id":metadata.message_id,
        "summary":summary,
        "source_ref":source_ref,
        "correlation_keys":correlation_keys
    });
    let decision = json!({
        "schema":"narada.mailbox.message_admission_receipt.v2",
        "admission_id":admission_id,
        "decision":if evaluation.admitted { "admitted" } else { "rejected" },
        "reason":evaluation.reason,
        "policy_version":policy_version,
        "source_event_id":source_event_id,
        "scope_id":scope.scope_id,
        "fact_id":fact_id,
        "source":source,
        "evaluated_metadata":{
            "folder_refs":evaluation.folder_refs,
            "sender_email":evaluation.sender_email
        }
    });
    let event_topic = if evaluation.admitted {
        "mailbox.message.admitted"
    } else {
        "mailbox.message.rejected"
    };
    let event_payload = json!({
        "schema":if evaluation.admitted { "narada.mailbox.message_admitted.v1" } else { "narada.mailbox.message_rejected.v1" },
        "admission_id":admission_id,
        "source_event_id":source_event_id,
        "scope_id":scope.scope_id,
        "fact_id":fact_id,
        "decision":if evaluation.admitted { "admitted" } else { "rejected" },
        "reason":evaluation.reason,
        "policy_version":policy_version,
        "source":source
    });
    let now = now_iso_millis();
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|e| error("mailbox_domain_transaction_failed", &e.to_string()))?;
    let result = (|| {
        let existing: Option<(String, String, String)> = tx
            .query_row(
                "SELECT scope_id,fact_id,decision_json FROM mailbox_admission_receipts WHERE idempotency_key=?",
                params![idempotency_key],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .map_err(|e| error("mailbox_admission_query_failed", &e.to_string()))?;
        let (stored_decision, replayed) = if let Some((existing_scope, existing_fact, existing_json)) = existing {
            if existing_scope != scope.scope_id || existing_fact != fact_id {
                let code = format!("mailbox_admission_idempotency_conflict:{idempotency_key}");
                return Err(error(&code, &code));
            }
            (
                serde_json::from_str::<Value>(&existing_json)
                    .map_err(|e| error("mailbox_admission_receipt_invalid", &e.to_string()))?,
                true,
            )
        } else if let Some(existing_json) = tx
            .query_row(
                "SELECT decision_json FROM mailbox_admission_receipts WHERE scope_id=? AND fact_id=?",
                params![scope.scope_id, fact_id],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(|e| error("mailbox_admission_query_failed", &e.to_string()))?
        {
            (
                serde_json::from_str::<Value>(&existing_json)
                    .map_err(|e| error("mailbox_admission_receipt_invalid", &e.to_string()))?,
                true,
            )
        } else {
            tx.execute(
                "INSERT INTO mailbox_admission_receipts(admission_id,idempotency_key,request_fingerprint,scope_id,fact_id,policy_version,decision_json,created_at) VALUES (?,?,?,?,?,?,?,?)",
                params![admission_id,idempotency_key,request_fingerprint,scope.scope_id,fact_id,policy_version,serde_json::to_string(&decision).unwrap_or_else(|_| "{}".to_string()),now],
            )
            .map_err(|e| error("mailbox_admission_insert_failed", &e.to_string()))?;
            let event_id = stable_id("mbe_", &format!("admission\0{}\0{fact_id}", scope.scope_id));
            tx.execute(
                "INSERT INTO mailbox_outbox(event_id,scope_id,topic,aggregate_id,aggregate_revision,schema_version,causation_id,idempotency_key,partition_key,occurred_at,payload_json) VALUES (?,?,?,?,1,1,?,?,?,?,?)",
                params![event_id,scope.scope_id,event_topic,admission_id,source_event_id,event_id,admission_id,now,serde_json::to_string(&event_payload).unwrap_or_else(|_| "{}".to_string())],
            )
            .map_err(|e| error("mailbox_admission_event_insert_failed", &e.to_string()))?;
            (decision.clone(), false)
        };
        let mut result = stored_decision;
        result
            .as_object_mut()
            .expect("admission receipt")
            .insert("idempotency_replayed".to_string(), Value::Bool(replayed));
        Ok(json!({
            "schema":"narada.domain_operation.v1",
            "operation_ref":format!("mailbox-admission:{admission_id}"),
            "outcome":"completed",
            "result":result
        }))
    })();
    match result {
        Ok(value) => {
            tx.commit()
                .map_err(|e| error("mailbox_domain_transaction_commit_failed", &e.to_string()))?;
            Ok(value)
        }
        Err(value) => Err(value),
    }
}

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
        Value::Array(values) => Value::Array(values.iter().take(100).map(metadata_only_attachment).collect()),
        Value::Object(object) => Value::Object(object.iter().take(64).filter_map(|(key, nested)| {
            let normalized = key.to_ascii_lowercase();
            if matches!(normalized.as_str(), "contentbytes" | "content_bytes" | "content_base64" | "contentref" | "content_ref" | "content" | "data" | "bytes" | "raw") {
                None
            } else {
                Some((key.clone(), metadata_only_attachment(nested)))
            }
        }).collect()),
        Value::String(value) => Value::String(value.chars().take(2048).collect()),
        _ => value.clone(),
    }
}

fn bounded_text(value:Option<String>,max:usize)->Option<String>{value.map(|text|text.chars().take(max).collect())}
fn normalized_source_timestamp(value:Option<String>)->Option<String>{value.and_then(|text|OffsetDateTime::parse(text.trim(),&Rfc3339).ok().map(|time|iso_millis(time.to_offset(UtcOffset::UTC))))}
fn bounded_projection(value:&Value,depth:usize)->Value{if depth>=6{return json!({"truncated":true,"reason":"depth_limit"});}match value{Value::String(text)=>Value::String(text.chars().take(2048).collect()),Value::Array(values)=>Value::Array(values.iter().take(64).map(|value|bounded_projection(value,depth+1)).collect()),Value::Object(object)=>Value::Object(object.iter().take(64).map(|(key,value)|(key.chars().take(128).collect(),bounded_projection(value,depth+1))).collect()),_=>value.clone()}}

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
fn required_bounded(
    args: &Map<String, Value>,
    key: &str,
    code: &str,
    max: usize,
) -> Result<String, Value> {
    let value = args
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| error(code, code))?;
    if value.chars().count() > max {
        return Err(error(&format!("{code}_too_long"), &format!("{code}_too_long")));
    }
    Ok(value.to_string())
}

fn required_topics(value: Option<&Value>) -> Result<Vec<String>, Value> {
    let values = value
        .and_then(Value::as_array)
        .ok_or_else(|| error("mailbox_outbox_topics_required", "mailbox_outbox_topics_required"))?;
    if values.is_empty() || values.len() > 16 {
        return Err(error("mailbox_outbox_topics_required", "mailbox_outbox_topics_required"));
    }
    let mut topics = Vec::with_capacity(values.len());
    for value in values {
        let topic = value
            .as_str()
            .map(str::trim)
            .filter(|topic| !topic.is_empty())
            .ok_or_else(|| error("mailbox_outbox_topics_required", "mailbox_outbox_topics_required"))?;
        if topic.chars().count() > 256 {
            return Err(error(
                "mailbox_outbox_topics_required_too_long",
                "mailbox_outbox_topics_required_too_long",
            ));
        }
        topics.push(topic.to_string());
    }
    topics.sort();
    if topics.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err(error(
            "mailbox_outbox_topics_required_duplicate",
            "mailbox_outbox_topics_required_duplicate",
        ));
    }
    Ok(topics)
}

fn parsed_topics(value: Option<&str>, consumer_id: &str) -> Result<Vec<String>, Value> {
    let Some(value) = value else {
        let code = format!("mailbox_outbox_consumer_v2_registration_required:{consumer_id}");
        return Err(error(&code, &code));
    };
    let topics = serde_json::from_str::<Vec<String>>(value).map_err(|_| {
        let code = format!("mailbox_outbox_consumer_topics_invalid:{consumer_id}");
        error(&code, &code)
    })?;
    if topics.is_empty() {
        let code = format!("mailbox_outbox_consumer_topics_invalid:{consumer_id}");
        return Err(error(&code, &code));
    }
    Ok(topics)
}

fn required_timestamp(
    args: &Map<String, Value>,
    key: &str,
    code: &str,
) -> Result<String, Value> {
    let value = required_bounded(args, key, code, 64)?;
    let parsed = OffsetDateTime::parse(&value, &Rfc3339)
        .map_err(|_| error(&format!("{code}_invalid"), &format!("{code}_invalid")))?;
    Ok(iso_millis(parsed.to_offset(UtcOffset::UTC)))
}

fn bounded_integer(
    value: Option<&Value>,
    fallback: i64,
    min: i64,
    max: i64,
) -> Result<i64, Value> {
    let resolved = match value {
        None | Some(Value::Null) => fallback,
        Some(value) => value
            .as_i64()
            .ok_or_else(|| error("mailbox_integer_argument_invalid", "mailbox_integer_argument_invalid"))?,
    };
    if resolved < min || resolved > max {
        return Err(error("mailbox_integer_argument_invalid", "mailbox_integer_argument_invalid"));
    }
    Ok(resolved)
}

fn iso_millis(value: OffsetDateTime) -> String {
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        value.year(),
        u8::from(value.month()),
        value.day(),
        value.hour(),
        value.minute(),
        value.second(),
        value.nanosecond() / 1_000_000
    )
}

fn now_iso_millis() -> String {
    iso_millis(OffsetDateTime::now_utc())
}

fn canonical_json(value: &Value) -> String {
    match value {
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {
            serde_json::to_string(value).unwrap_or_else(|_| "null".to_string())
        }
        Value::Array(values) => format!(
            "[{}]",
            values.iter().map(canonical_json).collect::<Vec<_>>().join(",")
        ),
        Value::Object(object) => {
            let mut keys = object.keys().collect::<Vec<_>>();
            keys.sort();
            let entries = keys
                .into_iter()
                .map(|key| {
                    format!(
                        "{}:{}",
                        serde_json::to_string(key).unwrap_or_else(|_| "\"\"".to_string()),
                        canonical_json(object.get(key).unwrap_or(&Value::Null))
                    )
                })
                .collect::<Vec<_>>();
            format!("{{{}}}", entries.join(","))
        }
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
fn is_within(path:&Path,root:&Path)->bool{let p=path.canonicalize().unwrap_or_else(|_|path.to_path_buf());let r=root.canonicalize().unwrap_or_else(|_|root.to_path_buf());p==r||p.starts_with(&r)}
fn read_bounded(path:&Path)->Result<Value,Value>{if fs::metadata(path).map_err(|_|error("output_ref_not_found","output_ref_not_found"))?.len()>MAX_BYTES{return Err(error("output_ref_too_large","output_ref_too_large"));}let text=fs::read_to_string(path).map_err(|_|error("output_ref_read_failed","output_ref_read_failed"))?;serde_json::from_str(&text).map_err(|_|error("output_ref_invalid_json","output_ref_invalid_json"))}
fn error(code:&str,message:&str)->Value{json!({"schema":"narada.mailbox.error.v1","code":code,"message":message})}
fn schema(name: &str) -> Value {
    match name {
        "mailbox_messages_list" | "mailbox_search" => json!({"type":"object","properties":{
            "mailbox_id":{"type":"string","maxLength":512},"folder":{"type":"string","maxLength":512},"unread":{"type":"boolean"},
            "since":{"type":"string","maxLength":64,"format":"date-time"},"before":{"type":"string","maxLength":64,"format":"date-time"},"query":{"type":"string","maxLength":4096},
            "offset":{"type":"integer","minimum":0,"maximum":1000000},"limit":{"type":"integer","minimum":1,"maximum":100},"include_body":{"type":"boolean"}
        },"additionalProperties":false}),
        "mailbox_message_show" => json!({"type":"object","properties":{
            "message_id":{"type":"string","maxLength":1024},"mailbox_id":{"type":"string","maxLength":512},
            "include_html":{"type":"boolean"},"include_raw":{"type":"boolean"}
        },"required":["message_id"],"additionalProperties":false}),
        "mailbox_thread_show" => json!({"type":"object","properties":{
            "thread_id":{"type":"string","maxLength":1024},"mailbox_id":{"type":"string","maxLength":512},
            "offset":{"type":"integer","minimum":0,"maximum":1000000},"limit":{"type":"integer","minimum":1,"maximum":100},"include_body":{"type":"boolean"}
        },"required":["thread_id"],"additionalProperties":false}),
        "mailbox_generation_show" => json!({"type":"object","properties":{"generation_id":{"type":"string"},"offset":{"type":"integer","minimum":0,"maximum":1000000},"limit":{"type":"integer","minimum":1,"maximum":100}},"required":["generation_id"],"additionalProperties":false}),
        "mailbox_admission_show" => json!({"type":"object","properties":{"scope_id":{"type":"string"},"fact_id":{"type":"string"}},"required":["scope_id","fact_id"],"additionalProperties":false}),
        "mailbox_fact_show" => json!({"type":"object","properties":{
            "fact_id":{"type":"string"},"scope_id":{"type":"string"},"config_path":{"type":"string"},"include_content":{"type":"boolean","default":false}
        },"required":["fact_id"],"additionalProperties":false}),
        "mailbox_message_fact_find" => json!({"type":"object","properties":{"scope_id":{"type":"string"},"message_id":{"type":"string"}},"required":["scope_id","message_id"],"additionalProperties":false}),
        "mailbox_outbox_consumer_show" => json!({"type":"object","properties":{"consumer_id":{"type":"string"}},"required":["consumer_id"],"additionalProperties":false}),
        "mailbox_outbox_list" => json!({"type":"object","properties":{"consumer_id":{"type":"string"},"limit":{"type":"integer","minimum":1,"maximum":100}},"required":["consumer_id"],"additionalProperties":false}),
        "mailbox_outbox_consumer_register" => json!({"type":"object","properties":{
            "consumer_id":{"type":"string"},"scope_id":{"type":"string"},
            "topics":{"type":"array","minItems":1,"maxItems":16,"items":{"type":"string"}},
            "start_at":{"type":"string"}
        },"required":["consumer_id","scope_id","topics","start_at"],"additionalProperties":false}),
        "mailbox_outbox_ack" => json!({"type":"object","properties":{
            "consumer_id":{"type":"string"},"event_id":{"type":"string"},
            "receipt":{"type":"object","additionalProperties":false,"required":["schema","outcome","effect_ref"],"properties":{
                "schema":{"type":"string"},"outcome":{"type":"string"},"effect_ref":{"type":"string"}
            }}
        },"required":["consumer_id","event_id","receipt"],"additionalProperties":false}),
        "mailbox_sync_generation" => json!({"type":"object","properties":{
            "idempotency_key":{"type":"string"},"scope_id":{"type":"string"},"config_path":{"type":"string"},"timeout_ms":{"type":"integer","minimum":100,"maximum":60000}
        },"required":["idempotency_key"],"additionalProperties":false}),
        "mailbox_reconcile_first_observations" => json!({"type":"object","properties":{
            "idempotency_key":{"type":"string"},"generation_id":{"type":"string"},"scope_id":{"type":"string"},
            "config_path":{"type":"string"},"limit":{"type":"integer","minimum":1,"maximum":100}
        },"required":["idempotency_key","generation_id"],"additionalProperties":false}),
        "mailbox_message_admit" => json!({"type":"object","properties":{
            "idempotency_key":{"type":"string"},"fact_id":{"type":"string"},"source_event_id":{"type":"string"},
            "scope_id":{"type":"string"},"policy_version":{"type":"string"},"config_path":{"type":"string"}
        },"required":["idempotency_key","fact_id","source_event_id"],"additionalProperties":false}),
        "mailbox_output_show" => json!({"type":"object","properties":{"ref":{"type":"string"},"output_ref":{"type":"string"},"offset":{"type":"integer","minimum":0,"maximum":1000000},"limit":{"type":"integer","minimum":1,"maximum":10000}},"additionalProperties":false}),
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
        let result = messages(&json!({"limit":1,"include_body":false,"since":"2025-01-01T00:00:00Z"}).as_object().unwrap(), &root).expect("messages");
        assert_eq!(result["count"], 1);
        assert!(result["messages"][0].get("body_text").is_none());
        assert_eq!(result["messages"][0]["subject"], "hello");
        let doctor = doctor(&root);
        assert_eq!(doctor["skipped_non_message_records"], 1);
        let accounts = accounts(&root).expect("accounts");
        assert_eq!(accounts["accounts"][0]["folders"][0], "Inbox");
        assert_eq!(accounts["accounts"][0]["latest_message_at"], "2026-01-01T00:00:00.000Z");
        let show = message_show(&json!({"message_id":"m1"}).as_object().unwrap(), &root).expect("show");
        assert_eq!(show["message"]["body_text"], "world");
        assert_eq!(show["message"]["subject"], "hello");
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn native_mailbox_outbox_authority_is_scoped_and_idempotent() {
        let root = std::env::temp_dir().join(format!("narada-mailbox-db-{}", uuid::Uuid::new_v4()));
        let db = open_domain_db_write(&root).expect("db");
        db.execute_batch(r##"
            INSERT INTO mailbox_sync_generations(generation_id,idempotency_key,request_fingerprint,scope_id,config_fingerprint,status,batch_record_count,created_at,updated_at,completed_at)
            VALUES ('g1','k1','request','scope','cfg','completed',1,'2026-01-01','2026-01-01','2026-01-01');
            INSERT INTO mailbox_outbox(event_id,scope_id,topic,aggregate_id,aggregate_revision,schema_version,causation_id,idempotency_key,partition_key,occurred_at,payload_json)
            VALUES ('e1','scope','topic','a1',1,1,'c','k','p','2026-01-01T00:00:00.000Z','{"value":1}');
        "##).expect("schema");
        drop(db);
        let registration = json!({
            "consumer_id":"c1",
            "scope_id":"scope",
            "topics":["topic"],
            "start_at":"2026-01-01T00:00:00Z"
        });
        let registered = outbox_consumer_register(registration.as_object().unwrap(), &root)
            .expect("register");
        assert_eq!(registered["consumer"]["start_at"], "2026-01-01T00:00:00.000Z");
        assert_eq!(registered["consumer"]["topics_json"], "[\"topic\"]");
        let replay = outbox_consumer_register(registration.as_object().unwrap(), &root)
            .expect("registration replay");
        assert_eq!(replay["consumer"]["consumer_id"], "c1");
        let conflict = outbox_consumer_register(
            json!({"consumer_id":"c1","scope_id":"scope","topics":["other"],"start_at":"2026-01-01T00:00:00Z"})
                .as_object()
                .unwrap(),
            &root,
        )
        .expect_err("registration conflict");
        assert_eq!(conflict["code"], "mailbox_outbox_consumer_conflict:c1");
        assert_eq!(generation_show(&json!({"generation_id":"g1"}).as_object().unwrap(), &root).expect("generation")["generation"]["status"], "completed");
        assert_eq!(outbox_consumer_show(&json!({"consumer_id":"c1"}).as_object().unwrap(), &root).expect("consumer")["status"], "ok");
        let page = outbox_list(&json!({"consumer_id":"c1","limit":1}).as_object().unwrap(), &root).expect("outbox");
        assert_eq!(page["count"], 1);
        assert_eq!(page["items"][0]["payload"]["value"], 1);
        let acknowledgement = json!({
            "consumer_id":"c1",
            "event_id":"e1",
            "receipt":{"schema":"fixture.receipt.v1","outcome":"completed","effect_ref":"effect:1"}
        });
        let first_ack = outbox_ack(acknowledgement.as_object().unwrap(), &root).expect("ack");
        assert_eq!(first_ack["replayed"], false);
        let replayed_ack = outbox_ack(acknowledgement.as_object().unwrap(), &root).expect("ack replay");
        assert_eq!(replayed_ack["replayed"], true);
        assert_eq!(outbox_list(&json!({"consumer_id":"c1"}).as_object().unwrap(), &root).expect("drained")["count"], 0);
        let ack_conflict = outbox_ack(
            json!({"consumer_id":"c1","event_id":"e1","receipt":{"schema":"fixture.receipt.v1","outcome":"failed","effect_ref":"effect:2"}})
                .as_object()
                .unwrap(),
            &root,
        )
        .expect_err("ack conflict");
        assert_eq!(ack_conflict["code"], "mailbox_outbox_ack_conflict:c1:e1");
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn native_mailbox_reconciliation_publishes_first_observation_once() {
        let root = std::env::temp_dir().join(format!("narada-mailbox-reconcile-{}", uuid::Uuid::new_v4()));
        let scope_root = root.join(".narada/runtime/mailboxes/support");
        fs::create_dir_all(scope_root.join(".narada")).expect("scope root");
        fs::create_dir_all(root.join("config")).expect("config root");
        fs::write(
            root.join("config/config.json"),
            serde_json::to_vec(&json!({
                "scopes":[{
                    "scope_id":"support",
                    "root_dir":".narada/runtime/mailboxes/support",
                    "sources":[{"type":"graph"}],
                    "graph":{"user_id":"support@example.test","prefer_immutable_ids":true},
                    "scope":{"included_container_refs":["inbox"],"included_item_kinds":["message"]},
                    "normalize":{"attachment_policy":"metadata_only","body_policy":"text_only","include_headers":false,"tombstones_enabled":true},
                    "runtime":{"polling_interval_ms":60000,"acquire_lock_timeout_ms":1000,"cleanup_tmp_on_startup":true,"rebuild_views_after_sync":false,"rebuild_search_after_sync":false},
                    "admission":{"mail":{"included_folder_refs":["inbox"],"allowed_sender_domains":["allowed.test"],"unknown_sender_behavior":"ignore"}}
                }]
            }))
            .expect("config json"),
        )
        .expect("config");
        let domain = open_domain_db_write(&root).expect("domain");
        domain.execute(
            "INSERT INTO mailbox_sync_generations(generation_id,idempotency_key,request_fingerprint,scope_id,config_fingerprint,status,batch_record_count,created_at,updated_at,completed_at) VALUES (?,?,?,?,?,'completed',1,?,?,?)",
            params!["g1","sync-key","request","support","config","2026-01-01","2026-01-01","2026-01-01"],
        ).expect("generation");
        domain.execute(
            "INSERT INTO mailbox_sync_generation_records(generation_id,record_id,fact_id,event_kind,message_id,mailbox_id,conversation_id,source_version,application_status) VALUES (?,?,?,?,?,?,?,?,?)",
            params!["g1","record-1","fact-1","upsert","message-1","support","conversation-1","v1","projected"],
        ).expect("record");
        drop(domain);
        let facts = Connection::open(scope_root.join(".narada/facts.db")).expect("facts");
        facts.execute_batch("CREATE TABLE facts(fact_id TEXT PRIMARY KEY,fact_type TEXT NOT NULL,source_id TEXT NOT NULL,source_record_id TEXT NOT NULL,source_version TEXT,source_cursor TEXT,provenance_json TEXT NOT NULL,payload_json TEXT NOT NULL,created_at TEXT NOT NULL,admitted_at TEXT);").expect("fact schema");
        let payload = json!({
            "record_id":"record-1",
            "ordinal":"2026-01-01T00:00:00.000Z",
            "event":{
                "mailbox_id":"support",
                "message_id":"message-1",
                "event_kind":"upsert",
                "payload":{
                    "mailbox_id":"support","message_id":"message-1","conversation_id":"conversation-1",
                    "internet_message_id":"<message-1@example.test>","subject":"Fixture subject",
                    "from":{"email":"sender@allowed.test"},"folder_refs":["inbox"],
                    "body":{"text":"secret body must not cross the admission receipt"}
                }
            }
        });
        facts.execute(
            "INSERT INTO facts(fact_id,fact_type,source_id,source_record_id,source_version,provenance_json,payload_json,created_at) VALUES (?,?,?,?,?,?,?,?)",
            params!["fact-1","mail.message.discovered","support","record-1","v1",r#"{"source_id":"support","source_record_id":"record-1","source_version":"v1","source_cursor":"cursor-1","observed_at":"2026-01-01T00:00:00.000Z"}"#,serde_json::to_string(&payload).unwrap(),"2026-01-01T00:00:00.000Z"],
        ).expect("fact");
        drop(facts);
        let args = json!({"idempotency_key":"reconcile-1","generation_id":"g1","scope_id":"support"});
        let first = reconcile_first_observations(args.as_object().unwrap(), &root).expect("reconcile");
        assert_eq!(first["result"]["observations_recorded"], 1);
        assert_eq!(first["result"]["events_published"], 1);
        assert_eq!(first["result"]["idempotency_replayed"], false);
        let replay = reconcile_first_observations(args.as_object().unwrap(), &root).expect("replay");
        assert_eq!(replay["result"]["idempotency_replayed"], true);
        assert_eq!(replay["result"]["events_published"], 1);
        let db = open_domain_db(&root).expect("open").expect("db");
        let count: i64 = db.query_row(
            "SELECT COUNT(*) FROM mailbox_outbox WHERE topic='mailbox.message.first_observed'",
            [],
            |row| row.get(0),
        ).expect("event count");
        assert_eq!(count, 1);
        drop(db);
        let source_event_id = stable_id("mbe_", &format!("first-observed\0support\0message-1"));
        let admission_args = json!({
            "idempotency_key":"admit-1",
            "fact_id":"fact-1",
            "source_event_id":source_event_id,
            "scope_id":"support"
        });
        let admitted = admit_message(admission_args.as_object().unwrap(), &root).expect("admit");
        assert_eq!(admitted["result"]["decision"], "admitted");
        assert_eq!(admitted["result"]["reason"], "admitted");
        assert_eq!(admitted["result"]["idempotency_replayed"], false);
        assert!(!serde_json::to_string(&admitted).unwrap().contains("secret body"));
        let replay = admit_message(
            json!({"idempotency_key":"admit-2","fact_id":"fact-1","source_event_id":source_event_id,"scope_id":"support"})
                .as_object()
                .unwrap(),
            &root,
        )
        .expect("canonical replay");
        assert_eq!(replay["result"]["idempotency_replayed"], true);
        let shown = admission_show(
            json!({"scope_id":"support","fact_id":"fact-1"}).as_object().unwrap(),
            &root,
        )
        .expect("admission show");
        assert_eq!(shown["status"], "ok");
        assert_eq!(shown["admission"]["admission_id"], admitted["result"]["admission_id"]);
        fs::remove_dir_all(root).expect("cleanup");
    }
}
