fn continuity_summary(checkpoint: &Value) -> Value {
    let continuation = checkpoint
        .get("continuation")
        .filter(|v| v.is_object())
        .unwrap_or(&Value::Null);
    let active = checkpoint
        .get("active_task")
        .filter(|v| v.is_object())
        .unwrap_or(&Value::Null);
    let choose = |values: &[Option<&Value>], limit: usize| {
        values
            .iter()
            .flatten()
            .find(|v| !v.is_null())
            .and_then(|v| bounded_text(v, limit))
    };
    json!({
        "checkpoint_id":checkpoint["checkpoint_id"],
        "checkpoint_at":bounded_text(&checkpoint["checkpoint_at"],80),
        "objective":choose(&[continuation.get("objective"),active.get("objective"),active.get("title")],320),
        "current_state":choose(&[continuation.get("current_state"),checkpoint.get("tactical_resume_notes")],320),
        "next_action":choose(&[continuation.get("next_action"),checkpoint.get("next_intended_action")],320),
        "blocker_count":checkpoint["continuation_blockers"].as_array().map(Vec::len).unwrap_or(0),
        "historical_advisory_only":true
    })
}

fn bounded_text(value: &Value, max: usize) -> Option<String> {
    if value.is_null() {
        return None;
    }
    let text = value
        .as_str()
        .map(str::trim)
        .map(str::to_string)
        .unwrap_or_else(|| serde_json::to_string(value).unwrap_or_default());
    if text.is_empty() {
        None
    } else if text.chars().count() <= max {
        Some(text)
    } else {
        Some(format!(
            "{}…",
            text.chars()
                .take(max.saturating_sub(1))
                .collect::<String>()
                .trim_end()
        ))
    }
}

fn validate_admission(context: &Context, a: &Value, at: &str) -> Result<(), String> {
    if a["schema"] != "narada.carrier_session.admission_receipt.v0" || a["decision"] != "admitted" {
        return Err("agent_context_exact_admission_receipt_required".into());
    }
    if a.pointer("/coordinate/site_ref").and_then(Value::as_str)
        != Some(format!("site:{}", context.site_id).as_str())
        && a.pointer("/coordinate/site_ref").and_then(Value::as_str)
            != Some(context.site_id.as_str())
    {
        return Err("agent_context_admission_site_mismatch".into());
    }
    if let Some(until) = a["valid_until"].as_str() {
        if until <= at {
            return Err("agent_context_admission_receipt_expired".into());
        }
    }
    Ok(())
}

fn validate_activation(
    admission: &Value,
    activation: Option<&Value>,
    observed_at: &str,
) -> Result<Value, String> {
    let Some(value) = activation else {
        return Ok(Value::Null);
    };
    if value["schema"] != "narada.carrier_session.activation_receipt.v0" {
        return Err("agent_context_activation_receipt_invalid".into());
    }
    if value["coordinate"] != admission["coordinate"] {
        return Err("agent_context_activation_session_binding_mismatch".into());
    }
    if value["admission_receipt_ref"] != admission["receipt_id"] {
        return Err("agent_context_activation_admission_receipt_mismatch".into());
    }
    let issued = value["issued_at"]
        .as_str()
        .ok_or("agent_context_activation_receipt_invalid")?;
    if issued < admission["issued_at"].as_str().unwrap_or("") || issued > observed_at {
        return Err("agent_context_activation_receipt_temporally_invalid".into());
    }
    if value["decision"] != "activated" {
        return Ok(Value::Null);
    }
    let binding = value
        .get("runtime_binding")
        .filter(|v| !v.is_null())
        .ok_or("agent_context_activation_runtime_binding_required")?;
    if binding["owning_site_ref"] != admission["coordinate"]["site_ref"] {
        return Err("agent_context_runtime_binding_site_mismatch".into());
    }
    if binding["observed_at"].as_str().unwrap_or("") > issued {
        return Err("agent_context_activation_runtime_observation_after_receipt".into());
    }
    Ok(binding.clone())
}
fn law_path(context: &Context) -> (PathBuf, String) {
    let direct = context.site_root.join("AGENTS.md");
    if direct.exists() {
        return (direct, "AGENTS.md".into());
    }
    let contained = context.site_root.join(".narada/AGENTS.md");
    if contained.exists() && context.site_root.join(".narada/config.json").exists() {
        (contained, ".narada/AGENTS.md".into())
    } else {
        (direct, "AGENTS.md".into())
    }
}
fn mcp_servers(context: &Context) -> Vec<Value> {
    let dir = context.site_root.join(".ai/mcp");
    let mut files = fs::read_dir(dir)
        .ok()
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|v| v.to_str()) == Some("json"))
        .collect::<Vec<_>>();
    files.sort();
    let mut out = vec![];
    for p in files {
        if let Ok(v) = fs::read_to_string(p)
            .ok()
            .and_then(|s| serde_json::from_str::<Value>(&s).ok())
            .ok_or(())
        {
            if let Some(map) = v["mcpServers"].as_object() {
                for (name, server) in map {
                    out.push(json!({"name":name,"transport":server["transport"].as_str().unwrap_or("stdio")}))
                }
            }
        }
    }
    out.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
    out
}
fn projection_key(v: &Value) -> (u8, u8, String, String, String, String) {
    let compartments = [
        "embodiment_coordinates",
        "office_and_role",
        "law_and_constraints",
        "entry_procedure",
        "continuity",
        "work_orientation",
        "capability_projection",
        "authority_references",
        "obligations",
        "negative_claims",
    ];
    (
        (v["criticality"] == "optional") as u8,
        compartments
            .iter()
            .position(|x| v["compartment"] == *x)
            .unwrap_or(255) as u8,
        v["entry_kind"].as_str().unwrap_or("").into(),
        v["source_authority_ref"].as_str().unwrap_or("").into(),
        v["artifact_ref"].as_str().unwrap_or("").into(),
        v["entry_id"].as_str().unwrap_or("").into(),
    )
}
pub fn canonical_json(v: &Value) -> String {
    match v {
        Value::Array(a) => format!(
            "[{}]",
            a.iter().map(canonical_json).collect::<Vec<_>>().join(",")
        ),
        Value::Object(o) => {
            let mut keys = o.keys().collect::<Vec<_>>();
            keys.sort();
            format!(
                "{{{}}}",
                keys.iter()
                    .map(|k| format!(
                        "{}:{}",
                        serde_json::to_string(k).unwrap(),
                        canonical_json(&o[*k])
                    ))
                    .collect::<Vec<_>>()
                    .join(",")
            )
        }
        _ => serde_json::to_string(v).unwrap(),
    }
}
fn sha256(v: &str) -> String {
    format!("{:x}", Sha256::digest(v.as_bytes()))
}
fn stabilize_bytes(v: &mut Value, parent: &str, field: &str) -> Result<(), String> {
    for _ in 0..8 {
        let n = serde_json::to_vec(v).map_err(|e| e.to_string())?.len();
        if v[parent][field].as_u64() == Some(n as u64) {
            return Ok(());
        }
        v[parent][field] = json!(n)
    }
    Err("manifest_byte_count_unstable".into())
}
fn stabilize_top_bytes(v: &mut Value, field: &str) -> Result<(), String> {
    for _ in 0..8 {
        let n = serde_json::to_vec(v).map_err(|e| e.to_string())?.len();
        if v[field].as_u64() == Some(n as u64) {
            return Ok(());
        }
        v[field] = json!(n)
    }
    Err("orientation_brief_byte_count_unstable".into())
}
