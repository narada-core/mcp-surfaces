fn continuation_export(context: &Context, args: &Value) -> Result<Value, String> {
    let agent_id = args
        .get("agent_id")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| env::var("NARADA_AGENT_ID").ok())
        .ok_or("agent_id_required")?;
    validate_identity(context, &agent_id)?;
    let db = context.open_db()?;
    let checkpoint = db
        .query_row(
            "SELECT * FROM agent_checkpoints WHERE agent_id=?1 ORDER BY checkpoint_at DESC LIMIT 1",
            [&agent_id],
            row_to_checkpoint,
        )
        .optional()
        .map_err(db_error)?;
    let Some(checkpoint) = checkpoint else {
        return Ok(
            json!({"status":"no_checkpoint","agent_id":agent_id,"message":"No site-local checkpoint found."}),
        );
    };
    let continuation = checkpoint.get("continuation").filter(|v| !v.is_null());
    let Some(continuation) = continuation else {
        return Ok(
            json!({"status":"no_continuation","agent_id":agent_id,"checkpoint_id":checkpoint["checkpoint_id"],"message":"The latest checkpoint has no canonical continuation state."}),
        );
    };
    let relative = continuation_export_path(
        context,
        args.get("path"),
        &agent_id,
        checkpoint["checkpoint_id"].as_str().unwrap_or(""),
    )?;
    let artifact_path = context.site_root.join(&relative);
    let markdown = render_continuation(&agent_id, &checkpoint, continuation);
    if markdown.len() > 256 * 1024 {
        return Err("continuation_export_too_large".into());
    }
    if let Some(parent) = artifact_path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("continuation_export_write_failed:{e}"))?;
    }
    let overwrite = args.get("overwrite") == Some(&Value::Bool(true));
    let wrote = if artifact_path.exists() {
        let prior =
            fs::read(&artifact_path).map_err(|e| format!("continuation_export_read_failed:{e}"))?;
        if prior == markdown.as_bytes() {
            false
        } else if !overwrite {
            return Err("continuation_export_target_exists".into());
        } else {
            fs::write(&artifact_path, markdown.as_bytes())
                .map_err(|e| format!("continuation_export_write_failed:{e}"))?;
            true
        }
    } else {
        fs::write(&artifact_path, markdown.as_bytes())
            .map_err(|e| format!("continuation_export_write_failed:{e}"))?;
        true
    };
    use sha2::{Digest, Sha256};
    let reference = json!({"schema":"narada.continuation.handoff.v1","path":relative,"sha256":format!("{:x}",Sha256::digest(markdown.as_bytes())),"created_at":timestamp()});
    let projection = continuation_projection(&agent_id, Some(&reference), None);
    let mut payload = checkpoint["payload"].clone();
    payload
        .as_object_mut()
        .ok_or("checkpoint_payload_invalid")?
        .insert("continuation_ref".into(), reference.clone());
    payload
        .as_object_mut()
        .unwrap()
        .insert("continuation_projection".into(), projection.clone());
    db.execute(
        "UPDATE agent_checkpoints SET payload_json=?1 WHERE checkpoint_id=?2",
        params![json_text(payload), checkpoint["checkpoint_id"].as_str()],
    )
    .map_err(db_error)?;
    Ok(
        json!({"status":"exported","site_id":context.site_id,"site_root":path_text(&context.site_root),"agent_id":agent_id,"checkpoint_id":checkpoint["checkpoint_id"],"checkpoint_at":checkpoint["checkpoint_at"],"continuation":continuation,"continuation_ref":reference,"continuation_projection":projection,"artifact":{"path":relative,"bytes":markdown.len(),"wrote":wrote}}),
    )
}

fn continuation_read(context: &Context, args: &Value) -> Result<Value, String> {
    let agent_id = args
        .get("agent_id")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| env::var("NARADA_AGENT_ID").ok())
        .ok_or("agent_id_required")?;
    validate_identity(context, &agent_id)?;
    let checkpoint_id = optional_string(args, "checkpoint_id")?;
    let db = context.open_db()?;
    let checkpoint = match checkpoint_id.as_ref() { Some(id) => checkpoint_by_id(&db,&agent_id,id)?, None => db.query_row("SELECT * FROM agent_checkpoints WHERE agent_id=?1 ORDER BY checkpoint_at DESC LIMIT 1",[&agent_id],row_to_checkpoint).optional().map_err(db_error)? };
    let Some(checkpoint) = checkpoint else {
        return Ok(match checkpoint_id {
            Some(id) => {
                json!({"status":"checkpoint_not_found","agent_id":agent_id,"checkpoint_id":id,"message":"No site-local current or archived checkpoint found for the requested checkpoint_id."})
            }
            None => {
                json!({"status":"no_checkpoint","agent_id":agent_id,"message":"No site-local checkpoint found."})
            }
        });
    };
    let mut base = json!({"site_id":context.site_id,"site_root":path_text(&context.site_root),"agent_id":agent_id,"checkpoint_id":checkpoint["checkpoint_id"],"checkpoint_at":checkpoint["checkpoint_at"],"continuation":checkpoint["continuation"],"continuation_ref":checkpoint["continuation_ref"],"continuation_projection":checkpoint["continuation_projection"]});
    let reference = checkpoint.get("continuation_ref").filter(|v| !v.is_null());
    if reference.is_none() {
        let has_continuation = checkpoint.get("continuation").is_some_and(|v| !v.is_null());
        let selected = checkpoint_id.as_deref();
        let message = match (has_continuation, selected) {
            (true, Some(id)) => format!("Canonical continuation exists in the checkpoint {id} but has no portable Markdown reference."),
            (true, None) => "Canonical continuation exists in the latest checkpoint but has no portable Markdown reference.".into(),
            (false, Some(id)) => format!("The checkpoint {id} has no canonical continuation state."),
            (false, None) => "The latest checkpoint has no canonical continuation state.".into(),
        };
        base.as_object_mut().unwrap().extend(json!({"status":if has_continuation{"unlinked"}else{"no_continuation"},"message":message,"next_action":checkpoint.pointer("/continuation_projection/next_action").cloned().unwrap_or(Value::Null)}).as_object().unwrap().clone());
        return Ok(base);
    }
    let reference = reference.unwrap();
    let path = reference
        .get("path")
        .and_then(Value::as_str)
        .ok_or("continuation_ref_path_must_be_site_relative")?;
    let artifact_path = context.site_root.join(path);
    let result = (|| {
        let metadata = fs::symlink_metadata(&artifact_path)?;
        if metadata.file_type().is_symlink() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "continuation artifact symlinks are refused",
            ));
        }
        if metadata.len() > 256 * 1024 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "continuation artifact exceeds 256 KiB",
            ));
        }
        fs::read_to_string(&artifact_path)
    })();
    match result {
        Ok(markdown)=>{
            let expected=checkpoint.pointer("/continuation/content_hash").and_then(Value::as_str);
            use sha2::{Digest, Sha256};
            let actual_sha256=format!("{:x}",Sha256::digest(markdown.as_bytes()));
            let reference_matches=reference.get("sha256").and_then(Value::as_str)==Some(actual_sha256.as_str());
            if !reference_matches || expected.is_some_and(|hash|!markdown.contains("<!-- narada.continuation.handoff.v1 -->")||!markdown.contains(&format!("<!-- narada.continuation.content-hash: {hash} -->"))){base.as_object_mut().unwrap().extend(json!({"continuation_ref":reference,"status":"stale","reason":if reference_matches{"continuation_artifact_content_hash_mismatch"}else{"continuation_artifact_sha256_mismatch"},"artifact":{"path":path,"verified":false,"actual_sha256":actual_sha256}}).as_object().unwrap().clone())}else{base.as_object_mut().unwrap().extend(json!({"continuation_ref":reference,"status":"ok","artifact":{"path":path,"sha256":reference["sha256"],"created_at":reference["created_at"],"bytes":markdown.len(),"verified":true,"markdown":markdown}}).as_object().unwrap().clone())}
        }
        Err(error)=>base.as_object_mut().unwrap().extend(json!({"status":"stale","reason":format!("continuation_ref_unreadable: {error}"),"artifact":{"path":path,"verified":false}}).as_object().unwrap().clone()),
    }
    Ok(base)
}

fn continuation_export_path(
    context: &Context,
    value: Option<&Value>,
    agent: &str,
    checkpoint: &str,
) -> Result<String, String> {
    let default = format!(".ai/continuations/{}-{checkpoint}.md", safe_segment(agent));
    let raw = match value {
        None | Some(Value::Null) => default,
        Some(Value::String(v)) => v.clone(),
        _ => return Err("continuation_export_path_must_be_site_relative".into()),
    };
    if raw.trim().is_empty()
        || raw.contains('\0')
        || raw.contains(':')
        || Path::new(&raw).is_absolute()
    {
        return Err("continuation_export_path_must_be_site_relative".into());
    }
    let normalized = raw.replace('\\', "/");
    if !normalized.to_ascii_lowercase().ends_with(".md") {
        return Err("continuation_export_path_must_be_markdown".into());
    }
    let parts = normalized
        .split('/')
        .filter(|p| !p.is_empty() && *p != ".")
        .collect::<Vec<_>>();
    if parts.contains(&"..")
        || parts.first() != Some(&".ai")
        || parts.get(1) != Some(&"continuations")
    {
        return Err("continuation_export_path_outside_export_root".into());
    }
    let _ = &context.site_root;
    Ok(parts.join("/"))
}
fn safe_segment(value: &str) -> String {
    let value = value
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || "_.-".contains(c) {
                c
            } else {
                '-'
            }
        })
        .collect::<String>();
    let trimmed = value.trim_matches('-');
    if trimmed.is_empty() {
        "agent".into()
    } else {
        trimmed.chars().take(80).collect()
    }
}
fn render_continuation(agent: &str, checkpoint: &Value, c: &Value) -> String {
    let mut lines = vec![
        "<!-- narada.continuation.handoff.v1 -->".into(),
        format!(
            "<!-- narada.continuation.content-hash: {} -->",
            c["content_hash"].as_str().unwrap_or("")
        ),
        format!(
            "<!-- narada.continuation.source-checkpoint-ref: {} -->",
            c["source_checkpoint_ref"].as_str().unwrap_or("")
        ),
        "".into(),
        format!("# Continuation: {}", inline(&c["objective"])),
        "".into(),
        "- **Schema:** `narada.continuation.v1`".into(),
        format!("- **Continuation ID:** `{}`", inline(&c["continuation_id"])),
        format!("- **Agent:** `{}`", inline(&json!(agent))),
        format!(
            "- **Checkpoint:** `{}`",
            inline(&checkpoint["checkpoint_id"])
        ),
        format!(
            "- **Checkpointed:** {}",
            inline(&checkpoint["checkpoint_at"])
        ),
        format!("- **Created:** {}", inline(&c["created_at"])),
        format!("- **Resume mode:** `{}`", inline(&c["resume_mode"])),
        "".into(),
        "## Current state".into(),
        "".into(),
        block(&c["current_state"]),
        "".into(),
        "## Next action".into(),
        "".into(),
        if c["next_action"].is_null() {
            "No next action recorded.".into()
        } else {
            block(&c["next_action"])
        },
        "".into(),
    ];
    for (title, key) in [
        ("Completed work", "completed_work"),
        ("Decisions", "decisions"),
        ("Evidence references", "evidence_refs"),
        ("Open blockers", "open_blockers"),
        ("Canonical sources", "canonical_sources"),
        ("Constraints", "constraints"),
    ] {
        lines.push(format!("## {title}"));
        lines.push("".into());
        if let Some(items) = c[key].as_array() {
            if items.is_empty() {
                lines.push("_None._".into())
            } else {
                for item in items {
                    lines.push(format!("- {}", inline(item)))
                }
            }
        }
        lines.push("".into())
    }
    lines.push("> This file is a bounded projection of agent-context checkpoint state. Verify live Git, task, and agent-context state before acting.".into());
    lines.push("".into());
    lines.join("\n")
}
fn inline(v: &Value) -> String {
    value_text(v).replace(['\r', '\n'], " ").trim().into()
}
fn block(v: &Value) -> String {
    value_text(v).replace("\r\n", "\n").trim().into()
}
