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

