fn required_read(
    context: &Context,
    evidence: &Evidence,
    packet: &Value,
    step_id: &str,
    offset: i64,
) -> Result<Value, String> {
    if step_id.is_empty() {
        return Err("agent_context_orientation_required_read_step_id_required".into());
    }
    if offset < 0 {
        return Err("agent_context_orientation_required_read_offset_invalid".into());
    }
    let brief = &packet["orientation_brief"];
    let step = brief
        .get("required_reads")
        .and_then(Value::as_array)
        .and_then(|steps| {
            steps
                .iter()
                .find(|step| step.get("step_id").and_then(Value::as_str) == Some(step_id))
        })
        .ok_or_else(|| format!("agent_context_orientation_required_read_step_unknown:{step_id}"))?;
    let artifact = step
        .pointer("/source/artifact_ref")
        .and_then(Value::as_str)
        .unwrap_or("");
    let relative = artifact.strip_prefix("site-file:").ok_or_else(|| {
        format!("agent_context_orientation_required_read_source_unsupported:{artifact}")
    })?;
    if relative.is_empty()
        || relative.contains("..")
        || std::path::Path::new(relative).is_absolute()
    {
        return Err(format!(
            "agent_context_orientation_required_read_source_invalid:{artifact}"
        ));
    }
    let content = std::fs::read_to_string(context.site_root.join(relative)).map_err(|error| {
        format!("agent_context_orientation_required_read_source_missing:{error}")
    })?;
    use sha2::Digest;
    let content_hash = format!("{:x}", Sha256::digest(content.as_bytes()));
    if step.pointer("/source/revision").and_then(Value::as_str) != Some(content_hash.as_str()) {
        return Err(format!("agent_context_orientation_required_read_source_stale:{step_id}:expected={}:actual={content_hash}", step.pointer("/source/revision").and_then(Value::as_str).unwrap_or("")));
    }
    let db = context.open_db()?;
    let receipt = evidence.delivery["receipt_id"].as_str().unwrap_or("");
    let existing_completion: Option<String> = db.query_row(
        "SELECT completion_json FROM orientation_required_read_completions WHERE delivery_receipt_ref=?1 AND step_id=?2 LIMIT 1",
        params![receipt, step_id], |row| row.get(0),
    ).optional().map_err(db_error)?;
    if let Some(completion_text) = existing_completion {
        let completion: Value = serde_json::from_str(&completion_text).map_err(|_| {
            format!("agent_context_orientation_required_read_completion_invalid:{step_id}")
        })?;
        let existing_page: Option<String> = db.query_row(
            "SELECT page_json FROM orientation_required_read_pages WHERE delivery_receipt_ref=?1 AND step_id=?2 AND byte_offset=?3 LIMIT 1",
            params![receipt, step_id, offset], |row| row.get(0),
        ).optional().map_err(db_error)?;
        let page = existing_page
            .map(|text| {
                serde_json::from_str::<Value>(&text).map_err(|_| {
                    format!("agent_context_orientation_required_read_page_invalid:{step_id}")
                })
            })
            .transpose()?;
        if let Some(value) = &page {
            let start = value["byte_offset"].as_u64().unwrap_or(u64::MAX) as usize;
            let end = value["next_byte_offset"].as_u64().unwrap_or(u64::MAX) as usize;
            let expected = content
                .as_bytes()
                .get(start..end)
                .and_then(|bytes| std::str::from_utf8(bytes).ok());
            if value["content_sha256"] != content_hash || value["content"].as_str() != expected {
                return Err(format!(
                    "agent_context_orientation_required_read_page_source_conflict:{step_id}"
                ));
            }
        }
        let current = progress(&db, brief, &evidence.delivery)?;
        let mut public_page = page.clone().unwrap_or(Value::Null);
        if let Some(object) = public_page.as_object_mut() {
            object.remove("content");
        }
        return Ok(
            json!({"schema":"narada.agent_context.orientation_required_read.v1","status":"already_completed","source_mutation":false,"local_persistence":true,"ordinary_work_gate":"acknowledgement_required","identity_state":packet["identity_state"],"claimed_identity":packet["claimed_identity"],"authenticated_identity":packet["authenticated_identity"],"authentication":packet["authentication"],"authority":packet["authority"],"step_id":step_id,"source":step["source"],"content":page.as_ref().and_then(|v|v["content"].as_str()).map(Value::from).unwrap_or(Value::Null),"page":public_page,"result_evidence":completion["result_evidence"],"completion_ref":format!("agent-context:orientation_required_read_completions:orientation-read:{receipt}:{step_id}"),"required_read_progress":{"total":current.total,"completed":current.completed.len(),"pending":current.pending.len(),"completed_step_ids":current.completed,"pending_step_ids":current.pending,"completion_refs":current.refs,"active_step_id":current.active,"next_byte_offset":current.offset},"next_call":current.next_call}),
        );
    }
    let before = progress(&db, brief, &evidence.delivery)?;
    if before.active.as_deref() != Some(step_id) {
        return Err(format!(
            "agent_context_orientation_required_read_step_out_of_order:{step_id}:expected={}",
            before.active.as_deref().unwrap_or("none")
        ));
    }
    if before.offset != Some(offset) {
        return Err(format!("agent_context_orientation_required_read_offset_out_of_order:{step_id}:expected={}:actual={offset}", before.offset.unwrap_or(0)));
    }
    let bytes = content.as_bytes();
    if offset as usize > bytes.len() {
        return Err(format!("agent_context_orientation_required_read_offset_out_of_range:{step_id}:total={}:actual={offset}", bytes.len()));
    }
    let end = page_end(bytes, offset as usize);
    let page_bytes = &bytes[offset as usize..end];
    let page_content = std::str::from_utf8(page_bytes)
        .map_err(|_| "agent_context_orientation_required_read_page_boundary_invalid")?;
    let eof = end == bytes.len();
    let page_id = format!("orientation-read-page:{receipt}:{step_id}:{offset}");
    let page_ref = format!("agent-context:orientation_required_read_pages:{page_id}");
    let page = json!({"schema":"narada.agent_context.orientation_required_read_page.v1","page_id":page_id,"delivery_receipt_ref":receipt,"manifest_id":brief.pointer("/manifest_ref/manifest_id").cloned().unwrap_or(Value::Null),"brief_id":brief["brief_id"],"step_id":step_id,"byte_offset":offset,"returned_bytes":page_bytes.len(),"next_byte_offset":end,"eof":eof,"content_sha256":content_hash,"page_sha256":format!("{:x}",Sha256::digest(page_bytes)),"page_ref":page_ref,"content":page_content});
    let completed_at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    db.execute("INSERT INTO orientation_required_read_pages (page_id,delivery_receipt_ref,manifest_id,brief_id,step_id,byte_offset,next_byte_offset,page_json,delivered_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)", params![page_id,receipt,brief.pointer("/manifest_ref/manifest_id").and_then(Value::as_str),brief["brief_id"].as_str(),step_id,offset,end as i64,serde_json::to_string(&page).unwrap(),completed_at]).map_err(db_error)?;
    let normalized = content.replace("\r\n", "\n");
    let result_evidence = json!({"content_sha256":content_hash,"content_window_sha256":format!("{:x}",Sha256::digest(normalized.as_bytes())),"offset":1,"returned_lines":content.split('\n').count()});
    let completion_id = format!("orientation-read:{receipt}:{step_id}");
    let completion_ref =
        format!("agent-context:orientation_required_read_completions:{completion_id}");
    if eof {
        let completion = json!({"step_id":step_id,"tool_name":step.pointer("/tool/name").cloned().unwrap_or(Value::Null),"arguments":step.pointer("/tool/arguments").cloned().unwrap_or_else(||json!({})),"result_evidence":result_evidence,"completed_at":completed_at,"evidence_refs":[completion_ref,format!("sha256:{content_hash}")]});
        db.execute("INSERT INTO orientation_required_read_completions (completion_id,delivery_receipt_ref,manifest_id,brief_id,step_id,completion_json,completed_at) VALUES (?1,?2,?3,?4,?5,?6,?7)", params![completion_id,receipt,brief.pointer("/manifest_ref/manifest_id").and_then(Value::as_str),brief["brief_id"].as_str(),step_id,serde_json::to_string(&completion).unwrap(),completed_at]).map_err(db_error)?;
    }
    let after = progress(&db, brief, &evidence.delivery)?;
    let mut public_page = page.clone();
    public_page.as_object_mut().unwrap().remove("content");
    Ok(
        json!({"schema":"narada.agent_context.orientation_required_read.v1","status":if eof{"completed"}else{"page_emitted"},"source_mutation":false,"local_persistence":true,"ordinary_work_gate":"acknowledgement_required","identity_state":packet["identity_state"],"claimed_identity":packet["claimed_identity"],"authenticated_identity":packet["authenticated_identity"],"authentication":packet["authentication"],"authority":packet["authority"],"step_id":step_id,"source":step["source"],"content":page_content,"page":public_page,"result_evidence":if eof{result_evidence}else{Value::Null},"completion_ref":if eof{Value::String(completion_ref)}else{Value::Null},"required_read_progress":{"total":after.total,"completed":after.completed.len(),"pending":after.pending.len(),"completed_step_ids":after.completed,"pending_step_ids":after.pending,"completion_refs":after.refs,"active_step_id":after.active,"next_byte_offset":after.offset},"next_call":after.next_call}),
    )
}

fn page_end(bytes: &[u8], offset: usize) -> usize {
    if offset == bytes.len() {
        return offset;
    }
    let mut end = (offset + 3000).min(bytes.len());
    while end > offset && end < bytes.len() && (bytes[end] & 0xc0) == 0x80 {
        end -= 1;
    }
    while end > offset
        && serde_json::to_vec(&std::str::from_utf8(&bytes[offset..end]).unwrap_or(""))
            .map(|v| v.len())
            .unwrap_or(usize::MAX)
            > 3200
    {
        end -= 1;
        while end > offset && (bytes[end] & 0xc0) == 0x80 {
            end -= 1;
        }
    }
    if end >= bytes.len() {
        return bytes.len();
    }
    let minimum = offset + (end - offset) / 2;
    if let Some(position) = bytes[minimum..end].windows(2).rposition(|v| v == b"\n\n") {
        return minimum + position + 2;
    }
    if let Some(position) = bytes[minimum..end].iter().rposition(|v| *v == b'\n') {
        return minimum + position + 1;
    }
    end
}

fn occupant_material(result: &Value, packet: &Value, delivery: &Value) -> Result<Value, String> {
    let ordinal = packet
        .pointer("/orientation_brief/required_reads")
        .and_then(Value::as_array)
        .and_then(|steps| {
            steps
                .iter()
                .position(|step| step.get("step_id") == result.get("step_id"))
        })
        .map(|index| index + 1);
    Ok(
        json!({"schema":"narada.agent_context.orientation_material.v1","status":"orientation_required","source_mutation":false,"local_persistence":true,"ordinary_work_gate":"acknowledgement_required","identity_state":packet["identity_state"],"claimed_identity":packet["claimed_identity"],"authenticated_identity":packet["authenticated_identity"],"authentication":packet["authentication"],"authority":packet["authority"],"material":{"delivery_status":result["status"],"ordinal":ordinal,"source_ref":result.pointer("/source/artifact_ref").cloned().unwrap_or(Value::Null),"content":result["content"],"page":if result["page"].is_null(){Value::Null}else{json!({"returned_bytes":result.pointer("/page/returned_bytes").cloned().unwrap_or(Value::Null),"eof":result.pointer("/page/eof").cloned().unwrap_or(Value::Null)})}},"required_read_progress":{"total":result.pointer("/required_read_progress/total").cloned().unwrap_or(json!(0)),"completed":result.pointer("/required_read_progress/completed").cloned().unwrap_or(json!(0)),"pending":result.pointer("/required_read_progress/pending").cloned().unwrap_or(json!(0))},"next_call":continuation_for(result.get("next_call"),&packet["orientation_brief"],delivery)?}),
    )
}

fn acknowledge(context: &Context, evidence: &Evidence, packet: &Value) -> Result<Value, String> {
    let db = context.open_db()?;
    let brief = &packet["orientation_brief"];
    let current = progress(&db, brief, &evidence.delivery)?;
    if !current.pending.is_empty() {
        return Err(format!(
            "agent_context_orientation_required_reads_incomplete:{}:next={}({})",
            current.pending.join(","),
            current.next_call["tool"].as_str().unwrap_or(""),
            serde_json::to_string(&current.next_call["arguments"]).unwrap()
        ));
    }
    let receipt = evidence.delivery["receipt_id"].as_str().unwrap_or("");
    if let Some(existing) = db.query_row("SELECT acknowledgement_json FROM orientation_acknowledgements WHERE delivery_receipt_ref=?1 LIMIT 1", [receipt], |row| row.get::<_,String>(0)).optional().map_err(db_error)? {
        let acknowledgement: Value = serde_json::from_str(&existing).map_err(|error| error.to_string())?;
        project_acknowledgement(context, &acknowledgement)?;
        return Ok(json!({"schema":"narada.agent_context.orientation_acknowledgement_record.v1","status":"already_acknowledged","source_mutation":false,"local_persistence":true,"ordinary_work_gate":"open","acknowledgement":acknowledgement}));
    }
    let mut statement = db.prepare("SELECT completion_json FROM orientation_required_read_completions WHERE delivery_receipt_ref=?1 ORDER BY completed_at ASC,step_id ASC").map_err(db_error)?;
    let completions = statement
        .query_map([receipt], |row| row.get::<_, String>(0))
        .map_err(db_error)?
        .map(|value| value.map(|text| serde_json::from_str::<Value>(&text).unwrap()))
        .collect::<Result<Vec<_>, _>>()
        .map_err(db_error)?;
    drop(statement);
    let acknowledged_at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    use sha2::Digest;
    let digest_source = json!({"delivery_receipt_ref":receipt,"brief_digest":brief["brief_digest"],"acknowledged_at":acknowledged_at,"required_read_completions":completions});
    let digest = format!(
        "{:x}",
        Sha256::digest(canonical_json(&digest_source).as_bytes())
    );
    let session = evidence
        .admission
        .pointer("/coordinate/carrier_session_id")
        .and_then(Value::as_str)
        .unwrap_or("");
    let epoch = evidence
        .admission
        .pointer("/coordinate/authority_epoch")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    let mut evidence_refs = vec![json!(receipt)];
    for completion in &completions {
        for reference in completion
            .get("evidence_refs")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if !evidence_refs.contains(reference) {
                evidence_refs.push(reference.clone());
            }
        }
    }
    let acknowledgement = json!({"schema":"narada.carrier_session.orientation_acknowledgement.v1","acknowledgement_id":format!("orientation-ack:{session}:{epoch}:{}",&digest[..16]),"status":"acknowledged","coordinate":evidence.admission["coordinate"],"admission_receipt_ref":evidence.admission["receipt_id"],"delivery_receipt_ref":receipt,"manifest_id":brief.pointer("/manifest_ref/manifest_id").cloned().unwrap_or(Value::Null),"manifest_digest":brief.pointer("/manifest_ref/manifest_digest").cloned().unwrap_or(Value::Null),"brief_id":brief["brief_id"],"brief_digest":brief["brief_digest"],"acknowledged_at":acknowledged_at,"required_read_completions":completions,"acknowledgement_semantics":"receipt_and_required_reads_not_comprehension","action_admission":"separate_required","authority_readback_ref":format!("agent-context:orientation_acknowledgements:{receipt}"),"evidence_refs":evidence_refs});
    db.execute("INSERT INTO orientation_acknowledgements (acknowledgement_id,delivery_receipt_ref,manifest_id,brief_id,carrier_session_id,authority_epoch,acknowledgement_json,acknowledged_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)", params![acknowledgement["acknowledgement_id"].as_str(),receipt,brief.pointer("/manifest_ref/manifest_id").and_then(Value::as_str),brief["brief_id"].as_str(),session,epoch,serde_json::to_string(&acknowledgement).unwrap(),acknowledged_at]).map_err(db_error)?;
    project_acknowledgement(context, &acknowledgement)?;
    Ok(
        json!({"schema":"narada.agent_context.orientation_acknowledgement_record.v1","status":"acknowledged","source_mutation":false,"local_persistence":true,"ordinary_work_gate":"open","acknowledgement":acknowledgement}),
    )
}

fn canonical_json(value: &Value) -> String {
    fn sort(value: &Value) -> Value {
        match value {
            Value::Array(values) => Value::Array(values.iter().map(sort).collect()),
            Value::Object(object) => {
                let mut keys = object.keys().collect::<Vec<_>>();
                keys.sort();
                let mut result = serde_json::Map::new();
                for key in keys {
                    result.insert(key.clone(), sort(&object[key]));
                }
                Value::Object(result)
            }
            _ => value.clone(),
        }
    }
    serde_json::to_string(&sort(value)).unwrap()
}

