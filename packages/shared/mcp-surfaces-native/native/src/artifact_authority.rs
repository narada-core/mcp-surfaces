use serde_json::{json, Map, Value};
use std::io::Read;
use std::path::Path;
use std::time::Duration;

const MAX_RESPONSE_BYTES: u64 = 4 * 1024 * 1024;
const REQUEST_TIMEOUT_SECONDS: u64 = 30;

pub fn register(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let (base, session_id) = endpoint(args, root)?.ok_or_else(|| {
        super::authority_boundary(
            "artifacts",
            "artifact_register_file",
            "nars_artifact_write_authority_not_configured",
            "Configure the owning NARS runtime endpoint and retry.",
        )
    })?;
    let source_path = admitted_source_path(root, &required(args, "path")?)?;
    let kind = normalized_kind(args)?;
    let render_hint = normalized_render_hint(args)?;
    let mut body = Map::new();
    body.insert("source_path".into(), json!(source_path.to_string_lossy()));
    body.insert("kind".into(), json!(kind));
    body.insert("render_hint".into(), json!(render_hint.clone()));
    body.insert("idempotency_key".into(), json!(required(args, "idempotency_key")?));
    for (from, to) in [
        ("title", "title"),
        ("content_type", "content_type"),
        ("access_scope", "access_scope"),
    ] {
        if let Some(value) = args.get(from).and_then(Value::as_str).filter(|v| !v.trim().is_empty()) {
            body.insert(to.into(), json!(value.trim()));
        }
    }
    let response = request(&base, &artifact_path(&session_id), "POST", Some(&Value::Object(body)))?;
    let artifact = response.get("artifact").cloned().unwrap_or(Value::Null);
    let part = message_part(&artifact, Some(&kind), args.get("title"), Some(&render_hint))?;
    let artifact_id = part
        .get("artifact_id")
        .and_then(Value::as_str)
        .ok_or_else(|| super::error("artifact_record_missing_artifact_id", "artifact_record_missing_artifact_id"))?;
    Ok(json!({
        "schema":"narada.artifacts.register_file.v1",
        "status":"registered",
        "idempotent_replay":response.get("idempotent_replay").cloned().unwrap_or(json!(false)),
        "artifact":artifact,
        "artifact_url":format!("{base}{}/{}", artifact_path(&session_id), encode_component(artifact_id)),
        "content_url":format!("{base}{}/{}/content", artifact_path(&session_id), encode_component(artifact_id)),
        "message_part":part.clone(),
        "assistant_content_parts":[part],
        "operator_message":format!("Artifact ready: {}", artifact.get("title").and_then(Value::as_str).unwrap_or(artifact_id)),
        "projection_instruction":"Emit assistant_content_parts as structured assistant content when the operator should see the artifact in agent-web-ui. Do not paste the JSON object as plain text."
    }))
}

pub fn list(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let Some((base, session_id)) = endpoint(args, root)? else {
        return super::artifact_list(args, root);
    };
    let response = request(&base, &artifact_path(&session_id), "GET", None)?;
    super::artifact_list_projection(&session_id,&response,args,None)
}

pub fn read(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let Some((base, session_id)) = endpoint(args, root)? else {
        return super::artifact_read(args, root);
    };
    let artifact_id = required(args, "artifact_id")?;
    let path = format!("{}/{}", artifact_path(&session_id), encode_component(&artifact_id));
    let response = request(&base, &path, "GET", None)?;
    let artifact = response.get("artifact").cloned().unwrap_or(response);
    let part = message_part(&artifact, None, None, None)?;
    let title = artifact.get("title").and_then(Value::as_str).unwrap_or(&artifact_id);
    Ok(json!({
        "schema":"narada.artifacts.read.v1",
        "status":"ok",
        "artifact":artifact,
        "message_part":part.clone(),
        "assistant_content_parts":[part],
        "operator_message":format!("Artifact ready: {title}")
    }))
}

pub fn present(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let (base, session_id) = endpoint(args, root)?.ok_or_else(|| {
        super::authority_boundary(
            "artifacts",
            "artifact_present",
            "nars_artifact_write_authority_not_configured",
            "Configure the owning NARS runtime endpoint and retry.",
        )
    })?;
    let artifact_id = required(args, "artifact_id")?;
    let mut body = Map::new();
    for (key, value) in [("text", args.get("text")), ("title", args.get("title"))] {
        if let Some(value) = value.and_then(Value::as_str).filter(|v| !v.trim().is_empty()) {
            body.insert(key.into(), json!(value.trim()));
        }
    }
    body.insert("render_hint".into(), json!(normalized_render_hint(args)?));
    body.insert("request_id".into(), json!(required(args, "idempotency_key")?));
    let path = format!("{}/{}/message", artifact_path(&session_id), encode_component(&artifact_id));
    let response = request(&base, &path, "POST", Some(&Value::Object(body)))?;
    let artifact = response.get("artifact").cloned().unwrap_or(Value::Null);
    let message = response.get("message_part").cloned().filter(|value| value.get("artifact_id").is_some()).unwrap_or(message_part(&artifact, None, None, args.get("render_hint").and_then(Value::as_str))?);
    Ok(json!({
        "schema":"narada.artifacts.present.v1",
        "status":"presented",
        "idempotent_replay":response.get("idempotent_replay").cloned().unwrap_or(json!(false)),
        "artifact":artifact,
        "event":response.get("event").cloned().unwrap_or(Value::Null),
        "message_part":message,
        "operator_message":"Artifact presented in the NARS session event stream.",
        "projection_instruction":"No assistant-side JSON emission is required; NARS has already emitted a structured assistant_message event."
    }))
}

fn endpoint(args: &Map<String, Value>, root: &Path) -> Result<Option<(String, String)>, Value> {
    let session_id = match super::current_session_id(args)? {
        Some(value) => value,
        None => return Ok(None),
    };
    let configured = std::env::var("NARADA_NARS_BASE_URL").ok().filter(|value| !value.trim().is_empty())
        .or_else(|| std::env::var("NARADA_AGENT_RUNTIME_SERVER_URL").ok().filter(|value| !value.trim().is_empty()))
        .or_else(|| std::env::var("NARADA_RUNTIME_SERVER_URL").ok().filter(|value| !value.trim().is_empty()));
    let base = if let Some(value) = configured {
        normalize_base(&value).ok_or_else(|| super::error("nars_endpoint_invalid", "nars_endpoint_invalid"))?
    } else {
        let mut discovered = None;
        for path in super::session_index_paths(root, &session_id) {
            let Ok(record) = super::read_bounded_json(&path) else { continue };
            if let Some(value) = record.get("health_endpoint").and_then(Value::as_str).and_then(origin) {
                discovered = Some(value);
                break;
            }
        }
        let Some(value) = discovered else { return Ok(None) };
        value
    };
    Ok(Some((base, session_id)))
}

fn request(base: &str, path: &str, method: &str, body: Option<&Value>) -> Result<Value, Value> {
    let url = format!("{}{}", base.trim_end_matches('/'), path);
    let agent = ureq::AgentBuilder::new().timeout(Duration::from_secs(REQUEST_TIMEOUT_SECONDS)).build();
    let mut request = agent.request(method, &url).set("Accept", "application/json");
    let response = if let Some(body) = body {
        let bytes = serde_json::to_vec(body).map_err(|_| super::error("nars_request_encode_failed", "nars_request_encode_failed"))?;
        if bytes.len() > super::MAX_BYTES { return Err(super::error("nars_request_too_large", "nars_request_too_large")); }
        request = request.set("Content-Type", "application/json");
        request.send_bytes(&bytes)
    } else {
        request.call()
    };
    match response {
        Ok(response) => parse_response(response),
        Err(ureq::Error::Status(status, response)) => {
            let payload = parse_response(response).unwrap_or_else(|_| Value::Null);
            let code = payload.get("error").and_then(Value::as_str).unwrap_or("nars_artifact_request_failed");
            Err(json!({"schema":"narada.artifacts.authority_error.v1","status":"error","error":code,"http_status":status,"response":payload}))
        }
        Err(error) => Err(json!({"schema":"narada.artifacts.authority_error.v1","status":"unavailable","error":"nars_request_failed","message":error.to_string()})),
    }
}

fn parse_response(response: ureq::Response) -> Result<Value, Value> {
    let mut bytes = Vec::new();
    response.into_reader().take(MAX_RESPONSE_BYTES + 1).read_to_end(&mut bytes).map_err(|_| super::error("nars_response_read_failed", "nars_response_read_failed"))?;
    if bytes.len() as u64 > MAX_RESPONSE_BYTES { return Err(super::error("nars_response_too_large", "nars_response_too_large")); }
    if bytes.is_empty() { return Ok(json!({})); }
    serde_json::from_slice(&bytes).map_err(|_| super::error("nars_response_not_json", "nars_response_not_json"))
}

fn required(args: &Map<String, Value>, key: &str) -> Result<String, Value> {
    args.get(key).and_then(Value::as_str).map(str::trim).filter(|value| !value.is_empty()).map(str::to_string).ok_or_else(|| super::error(&format!("{key}_required"), &format!("{key}_required")))
}

fn normalized_kind(args: &Map<String, Value>) -> Result<String, Value> {
    let value = required(args, "kind")?.to_ascii_lowercase();
    if ["html", "markdown", "json", "text", "image", "audio"].contains(&value.as_str()) { Ok(value) } else { Err(super::error("artifact_kind_invalid", "artifact_kind_invalid")) }
}

fn admitted_source_path(root:&Path,value:&str)->Result<std::path::PathBuf,Value>{let candidate=Path::new(value);let candidate=if candidate.is_absolute(){candidate.to_path_buf()}else{root.join(candidate)};let canonical=candidate.canonicalize().map_err(|_|super::error("artifact_source_file_not_found","artifact_source_file_not_found"))?;let admitted=root.canonicalize().map_err(|_|super::error("artifact_site_root_unavailable","artifact_site_root_unavailable"))?;if !canonical.is_file(){return Err(super::error("artifact_source_not_file","artifact_source_not_file"));}if !canonical.starts_with(&admitted){return Err(super::error("artifact_source_outside_site_root","artifact_source_outside_site_root"));}Ok(canonical)}

fn normalized_render_hint(args: &Map<String, Value>) -> Result<String, Value> {
    let value = args.get("render_hint").and_then(Value::as_str).unwrap_or("inline").trim().to_ascii_lowercase();
    if ["inline", "link"].contains(&value.as_str()) { Ok(value) } else { Err(super::error("artifact_render_hint_invalid", "artifact_render_hint_invalid")) }
}

fn message_part(artifact: &Value, fallback_kind: Option<&str>, fallback_title: Option<&Value>, fallback_render_hint: Option<&str>) -> Result<Value, Value> {
    let id = artifact.get("artifact_id").or_else(|| artifact.get("artifactId")).or_else(|| artifact.get("id")).and_then(Value::as_str).ok_or_else(|| super::error("artifact_record_missing_artifact_id", "artifact_record_missing_artifact_id"))?;
    Ok(super::artifact_message_part(id, artifact.get("kind").cloned().or_else(|| fallback_kind.map(|value| json!(value))), artifact.get("title").cloned().or_else(|| fallback_title.cloned()), artifact.get("render_hint").cloned().or_else(|| fallback_render_hint.map(|value| json!(value)))))
}

fn artifact_path(session_id: &str) -> String { format!("/sessions/{}/artifacts", encode_component(session_id)) }

fn encode_component(value: &str) -> String {
    value.bytes().map(|byte| if byte.is_ascii_alphanumeric() || b"-_.~".contains(&byte) { (byte as char).to_string() } else { format!("%{byte:02X}") }).collect()
}

fn normalize_base(value: &str) -> Option<String> {
    let trimmed = value.trim().trim_end_matches('/');
    if (trimmed.starts_with("http://") || trimmed.starts_with("https://")) && !trimmed[trimmed.find("://")? + 3..].contains('/') { Some(trimmed.to_string()) } else { None }
}

fn origin(value: &str) -> Option<String> {
    let marker = value.find("://")?;
    let authority_start = marker + 3;
    let authority_end = value[authority_start..].find('/').map(|offset| authority_start + offset).unwrap_or(value.len());
    normalize_base(&value[..authority_end])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_origin_is_bounded() {
        assert_eq!(origin("http://127.0.0.1:8080/health"), Some("http://127.0.0.1:8080".into()));
        assert!(origin("file:///tmp/health").is_none());
        assert!(normalize_base("http://127.0.0.1:8080/path").is_none());
    }

    #[test]
    fn component_encoding_is_stable() {
        assert_eq!(encode_component("artifact/a"), "artifact%2Fa");
    }
}
