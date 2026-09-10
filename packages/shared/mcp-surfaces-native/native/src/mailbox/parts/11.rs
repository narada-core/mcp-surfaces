fn thread_attention_list(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let offset = bounded_integer(args.get("offset"), 0, 0, 1_000_000)?;
    let limit = bounded_integer(args.get("limit"), 100, 1, 100)?;
    let state = args.get("state").and_then(Value::as_str);
    if state.is_some_and(|value| !matches!(value, "required" | "cleared" | "excluded" | "indeterminate")) {
        return Err(error("mailbox_thread_attention_state_invalid", "mailbox_thread_attention_state_invalid"));
    }
    let scope_id = args.get("scope_id").and_then(Value::as_str);
    let Some(db) = open_domain_db(root)? else {
        return Ok(json!({"schema":"narada.mailbox.thread_attention_list.v1","status":"ok","count":0,"total_count":0,"items":[],"offset":offset,"limit":limit}));
    };
    let total_count: i64 = db.query_row(
        "SELECT COUNT(*) FROM mailbox_thread_attention WHERE (?1 IS NULL OR scope_id=?1) AND (?2 IS NULL OR attention_state=?2)",
        params![scope_id,state], |row| row.get(0),
    ).map_err(|e| error("mailbox_thread_attention_query_failed", &e.to_string()))?;
    let mut statement = db.prepare(
        "SELECT thread_id,scope_id,thread_key,revision,latest_message_id,latest_fact_id,latest_at,direction,admission_decision,attention_state,subject,updated_at FROM mailbox_thread_attention WHERE (?1 IS NULL OR scope_id=?1) AND (?2 IS NULL OR attention_state=?2) ORDER BY latest_at DESC,thread_id LIMIT ?3 OFFSET ?4"
    ).map_err(|e| error("mailbox_thread_attention_query_failed", &e.to_string()))?;
    let rows = statement.query_map(params![scope_id,state,limit,offset], |row| Ok(json!({
        "thread_id":row.get::<_,String>(0)?,"scope_id":row.get::<_,String>(1)?,"thread_key":row.get::<_,String>(2)?,
        "revision":row.get::<_,i64>(3)?,"latest_message_id":row.get::<_,String>(4)?,"latest_fact_id":row.get::<_,String>(5)?,
        "latest_at":row.get::<_,String>(6)?,"direction":row.get::<_,String>(7)?,"admission_decision":row.get::<_,String>(8)?,
        "attention_state":row.get::<_,String>(9)?,"subject":row.get::<_,Option<String>>(10)?,"updated_at":row.get::<_,String>(11)?
    }))).map_err(|e| error("mailbox_thread_attention_query_failed", &e.to_string()))?;
    let mut items=Vec::new();
    for row in rows { items.push(row.map_err(|e| error("mailbox_thread_attention_row_failed", &e.to_string()))?); }
    Ok(json!({"schema":"narada.mailbox.thread_attention_list.v1","status":"ok","count":items.len(),"total_count":total_count,"items":items,"offset":offset,"limit":limit,"next_offset":if offset+(items.len() as i64)<total_count{Some(offset+items.len() as i64)}else{None}}))
}

fn fact_payload_event_payload(fact: &MailFact) -> Option<&Map<String, Value>> {
    fact.payload.get("event")?.get("payload")?.as_object()
}

fn attention_timestamp(fact: &MailFact) -> String {
    fact_payload_event_payload(fact).and_then(|payload| {
        ["received_at","sent_at","last_modified_at","created_at"].iter()
            .find_map(|key| payload.get(*key).and_then(Value::as_str))
    }).unwrap_or(&fact.created_at).to_string()
}

fn attention_subject(fact: &MailFact) -> Option<String> {
    fact_payload_event_payload(fact).and_then(|payload| payload.get("subject")).and_then(Value::as_str)
        .map(|value| value.chars().take(500).collect())
}

pub(crate) fn project_thread_attention_fact(
    tx: &Transaction<'_>, site_root: &Path, config_path: &Path, scope_id: &str, fact_id: &str, now: &str,
) -> Result<bool, Value> {
    let mut args=Map::new();
    args.insert("scope_id".to_string(),json!(scope_id));
    args.insert("config_path".to_string(),json!(config_path.to_string_lossy()));
    let scope=load_mailbox_scope(&args,site_root)?;
    let fact=load_mail_fact(&scope,fact_id)?;
    if !matches!(fact.fact_type.as_str(),"mail.message.discovered"|"mail.message.changed") { return Ok(false); }
    let metadata=mail_metadata(&fact)?;
    let thread_key=metadata.conversation_id.clone().unwrap_or_else(||format!("singleton:{}",metadata.message_id));
    let thread_id=stable_id("mthread_",&format!("{}\0{}",scope.scope_id,thread_key));
    let folders=fact_folder_refs(&fact);
    let sender=fact_sender_email(&fact).map(|value|value.to_ascii_lowercase());
    let own=scope.graph_mailbox_id.as_deref().unwrap_or_default().to_ascii_lowercase();
    let sent_folder=folders.iter().any(|value|value.eq_ignore_ascii_case("sentitems"));
    let inbox_folder=folders.iter().any(|value|value.eq_ignore_ascii_case("inbox"));
    let sender_is_own=!own.is_empty() && sender.as_deref()==Some(own.as_str());
    let direction=if (sent_folder||sender_is_own) && !(inbox_folder&&!sender_is_own) {"outbound"}
        else if (inbox_folder||sender.is_some()) && !sent_folder && !sender_is_own {"inbound"}
        else {"indeterminate"};
    let evaluation=evaluate_admission(&fact,&scope.admission);
    let admission=if direction=="outbound"{"not_applicable"}else if direction=="indeterminate"{"indeterminate"}else if evaluation.admitted{"admitted"}else{"rejected"};
    let state=match (direction,admission){("outbound",_)=>"cleared",("inbound","admitted")=>"required",("inbound","rejected")=>"excluded",_=>"indeterminate"};
    let latest_at=attention_timestamp(&fact);
    let existing:Option<(i64,String,String,String)>=tx.query_row(
        "SELECT revision,latest_at,latest_message_id,latest_fact_id FROM mailbox_thread_attention WHERE thread_id=?",
        params![thread_id],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?))
    ).optional().map_err(|e|error("mailbox_thread_attention_query_failed",&e.to_string()))?;
    if let Some((_,existing_at,existing_message,existing_fact))=&existing {
        if existing_at>&latest_at || (existing_at==&latest_at && existing_message>&metadata.message_id) || (existing_message==&metadata.message_id && existing_fact==fact_id) { return Ok(false); }
    }
    let revision=existing.as_ref().map(|value|value.0+1).unwrap_or(1);
    tx.execute("INSERT INTO mailbox_thread_attention(thread_id,scope_id,thread_key,revision,latest_message_id,latest_fact_id,latest_at,direction,admission_decision,attention_state,subject,updated_at) VALUES(?,?,?,?,?,?,?,?,?,?,?,?) ON CONFLICT(thread_id) DO UPDATE SET revision=excluded.revision,latest_message_id=excluded.latest_message_id,latest_fact_id=excluded.latest_fact_id,latest_at=excluded.latest_at,direction=excluded.direction,admission_decision=excluded.admission_decision,attention_state=excluded.attention_state,subject=excluded.subject,updated_at=excluded.updated_at",
        params![thread_id,scope.scope_id,thread_key,revision,metadata.message_id,fact_id,latest_at,direction,admission,state,attention_subject(&fact),now]
    ).map_err(|e|error("mailbox_thread_attention_update_failed",&e.to_string()))?;
    let topic=match state{"required"=>"mailbox.thread.attention_required","cleared"=>"mailbox.thread.attention_cleared","indeterminate"=>"mailbox.thread.attention_indeterminate",_=>return Ok(true)};
    let event_id=stable_id("mte_",&format!("{}\0{}\0{}",thread_id,revision,state));
    let first_event_id=stable_id("mbe_",&format!("first-observed\0{}\0{}",scope.scope_id,metadata.message_id));
    let schema=match state{"required"=>"narada.mailbox.thread_attention_required.v1","cleared"=>"narada.mailbox.thread_attention_cleared.v1",_=>"narada.mailbox.thread_attention_indeterminate.v1"};
    let payload=json!({"schema":schema,"thread_id":thread_id,"thread_key":thread_key,"thread_revision":revision,"scope_id":scope.scope_id,"message_id":metadata.message_id,"fact_id":fact_id,"first_observed_event_id":first_event_id,"attention_state":state,"direction":direction,"admission_decision":admission,"latest_at":latest_at,"subject":attention_subject(&fact)});
    tx.execute("INSERT OR IGNORE INTO mailbox_outbox(event_id,scope_id,topic,aggregate_id,aggregate_revision,schema_version,causation_id,idempotency_key,partition_key,occurred_at,payload_json) VALUES(?,?,?,?,?,1,?,?,?,?,?)",
        params![event_id,scope.scope_id,topic,thread_id,revision,fact_id,event_id,thread_id,now,serde_json::to_string(&payload).unwrap_or_else(|_|"{}".to_string())]
    ).map_err(|e|error("mailbox_thread_attention_outbox_failed",&e.to_string()))?;
    Ok(true)
}

fn thread_attention_rebuild(args:&Map<String,Value>,root:&Path)->Result<Value,Value>{
    let idempotency_key=required_bounded(args,"idempotency_key","mailbox_thread_attention_idempotency_key_required",512)?;
    let scope=load_mailbox_scope(args,root)?;
    let config_arg=args.get("config_path").and_then(Value::as_str).unwrap_or("config/config.json");
    let config_path=if Path::new(config_arg).is_absolute(){PathBuf::from(config_arg)}else{root.join(config_arg)};
    let facts=Connection::open(scope.root_dir.join(".narada/facts.db")).map_err(|e|error("mailbox_fact_store_open_failed",&e.to_string()))?;
    let mut statement=facts.prepare("SELECT fact_id FROM facts WHERE fact_type IN ('mail.message.discovered','mail.message.changed') ORDER BY created_at,fact_id")
        .map_err(|e|error("mailbox_fact_query_failed",&e.to_string()))?;
    let rows=statement.query_map([],|row|row.get::<_,String>(0)).map_err(|e|error("mailbox_fact_query_failed",&e.to_string()))?;
    let mut fact_ids=Vec::new();
    for row in rows{fact_ids.push(row.map_err(|e|error("mailbox_fact_row_failed",&e.to_string()))?)}
    let mut db=open_domain_db_write(root)?;
    let tx=db.transaction_with_behavior(TransactionBehavior::Immediate).map_err(|e|error("mailbox_domain_transaction_failed",&e.to_string()))?;
    let now=now_iso_millis();let mut changed=0;
    for fact_id in &fact_ids{if project_thread_attention_fact(&tx,root,&config_path,&scope.scope_id,fact_id,&now)?{changed+=1;}}
    tx.commit().map_err(|e|error("mailbox_domain_transaction_commit_failed",&e.to_string()))?;
    Ok(json!({"schema":"narada.mailbox.thread_attention_rebuild.v1","status":"completed","operation_ref":format!("mailbox-thread-rebuild:{}",stable_id("mtr_",&idempotency_key)),"scope_id":scope.scope_id,"facts_scanned":fact_ids.len(),"thread_changes":changed}))
}
