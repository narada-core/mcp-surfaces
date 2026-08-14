use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};
use time::{format_description::well_known::Rfc3339, OffsetDateTime};

const MAX_REGISTRY_BYTES: u64 = 4 * 1024 * 1024;
const IDENTITY_SCHEMA: &str = "https://narada.dev/schemas/operator-surface-identities/v1";

pub fn admit_role(args: &Map<String, Value>, invocation_root: &Path) -> Result<Value, Value> {
    require_authorized_mutation(args)?;
    let site_id = required(args, "site_id")?;
    if site_id.contains(['/', '\\']) { return Err(error("site_id_invalid", "site_id must be canonical identity text, not a filesystem path")); }
    let site_root = authority_root(args, invocation_root)?;
    let role = required(args, "role")?;
    if !["architect", "builder", "observer"].contains(&role.as_str()) { return Err(error("site_role_invalid", "role must be architect, builder, or observer")); }
    let agent_kind = required(args, "agent_kind")?;
    let admitted_by = required(args, "by")?;
    let identity_id = optional(args, "identity").unwrap_or_else(|| format!("{site_id}.{role}"));
    let now = now_iso();
    let path = identity_path(&site_root);
    let mut registry = read_object_or(&path, json!({"schema":IDENTITY_SCHEMA,"updated_at":now,"identities":[]}))?;
    let identities = registry.get_mut("identities").and_then(Value::as_array_mut).ok_or_else(|| error("operator_surface_identity_registry_invalid", "identities must be an array"))?;
    if let Some(existing) = identities.iter().find(|entry| entry.get("identity_id").and_then(Value::as_str) == Some(identity_id.as_str())) {
        let same = existing.get("site_id").and_then(Value::as_str) == Some(site_id.as_str())
            && existing.get("role").and_then(Value::as_str) == Some(role.as_str())
            && existing.get("agent_kind").and_then(Value::as_str) == Some(agent_kind.as_str());
        if !same { return Err(error("operator_surface_identity_conflict", "identity_id is already admitted with different site, role, or agent_kind")); }
        return Ok(json!({"schema":"narada.operator_surface.identity_admission.v1","status":"reused","mutation_performed":false,"identity":existing,"registry_path":path,"authority_root":site_root}));
    }
    let input_capabilities = optional(args, "input_capabilities").map(|value| value.split(',').map(str::trim).filter(|item| !item.is_empty()).map(str::to_string).collect::<Vec<_>>()).unwrap_or_else(|| vec!["focus".into(),"type_text".into(),"clear_pending_input".into(),"recover_surface_state".into()]);
    let submit_strategy = optional(args, "submit_strategy").unwrap_or_else(|| "type_only".into());
    if !["type_only","operator_confirmed_submit","known_surface_submit"].contains(&submit_strategy.as_str()) { return Err(error("operator_surface_submit_strategy_invalid", "unsupported submit_strategy")); }
    let record = json!({
        "identity_id":identity_id,"site_id":site_id,"role":role,"agent_kind":agent_kind,
        "label":optional(args,"label").unwrap_or_else(||identity_id.clone()),"input_capabilities":input_capabilities,
        "submit_strategy":submit_strategy,"admitted_by":admitted_by,"admitted_at":now,"updated_at":now,
        "authority_limits":["identity_record_is_site_authority","runtime_handle_binding_is_not_admitted_here","operator_surface_does_not_grant_effect_capability"]
    });
    identities.push(record.clone());
    registry["updated_at"] = json!(now);
    write_json(&path, &registry)?;
    Ok(json!({"schema":"narada.operator_surface.identity_admission.v1","status":"admitted","mutation_performed":true,"identity":record,"registry_path":path,"authority_root":site_root,"runtime_binding_mutated":false}))
}

pub fn verify_role(args: &Map<String, Value>, invocation_root: &Path) -> Result<Value, Value> {
    let site_id = required(args, "site_id")?;
    let site_root = authority_root(args, invocation_root)?;
    let registry = read_object_or(&identity_path(&site_root), json!({"schema":IDENTITY_SCHEMA,"identities":[]}))?;
    let bindings = read_bindings(&site_root)?;
    let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(100).clamp(1, 500) as usize;
    let runtime_filter = optional(args, "runtime_locus");
    let identities = registry.get("identities").and_then(Value::as_array).into_iter().flatten()
        .filter(|entry| entry.get("site_id").and_then(Value::as_str) == Some(site_id.as_str())).take(limit)
        .map(|entry| {
            let identity_id = entry.get("identity_id").and_then(Value::as_str).unwrap_or_default();
            let binding = bindings.iter().find(|binding| binding.get("identity_id").and_then(Value::as_str) == Some(identity_id)
                && binding.get("status").and_then(Value::as_str) == Some("active")
                && runtime_filter.as_deref().map(|filter| binding.get("runtime_locus").and_then(Value::as_str) == Some(filter)).unwrap_or(true));
            json!({"identity":entry,"runtime_binding":binding,"durably_admitted":true,"runtime_bound":binding.is_some()})
        }).collect::<Vec<_>>();
    Ok(json!({"schema":"narada.operator_surface.role_verification.v1","status":if identities.is_empty(){"not_found"}else{"ok"},"site_id":site_id,"authority_root":site_root,"count":identities.len(),"identities":identities,"authority_split":{"durable_identity_authority":identity_path(&site_root),"volatile_binding_authority":runtime_binding_path(&site_root)}}))
}

pub fn observe_runtime(args: &Map<String, Value>, invocation_root: &Path) -> Result<Value, Value> {
    let site_id = required(args, "site_id")?;
    let site_root = authority_root(args, invocation_root)?;
    let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(100).clamp(1, 500) as usize;
    let registry = read_object_or(&identity_path(&site_root), json!({"schema":IDENTITY_SCHEMA,"identities":[]}))?;
    let bindings = read_bindings(&site_root)?;
    let identities = registry.get("identities").and_then(Value::as_array).into_iter().flatten().filter(|entry| entry.get("site_id").and_then(Value::as_str) == Some(site_id.as_str())).take(limit).cloned().collect::<Vec<_>>();
    let admitted = identities.iter().filter_map(|entry| entry.get("identity_id").and_then(Value::as_str)).collect::<Vec<_>>();
    let bindings = bindings.into_iter().filter(|binding| binding.get("identity_id").and_then(Value::as_str).is_some_and(|id| admitted.contains(&id))).take(limit).collect::<Vec<_>>();
    Ok(json!({"schema":"narada.operator_surface.runtime_observation.v1","status":"ok","site_id":site_id,"authority_root":site_root,"identity_count":identities.len(),"binding_count":bindings.len(),"identities":identities,"bindings":bindings,"ambient_foreground_used":false}))
}

pub fn bind_runtime(args: &Map<String, Value>, invocation_root: &Path) -> Result<Value, Value> {
    require_authorized_mutation(args)?;
    let site_root = authority_root(args, invocation_root)?;
    let identity_id = required(args, "identity")?;
    let runtime_locus = required(args, "runtime_locus")?;
    let handle = required(args, "handle")?;
    let observed_handle = optional(args, "observed_handle").unwrap_or_else(|| handle.clone());
    if observed_handle != handle { return Err(error("runtime_binding_postcondition_mismatch", "observed_handle must equal the requested handle")); }
    if let Some(stale_after) = optional(args, "stale_after") { OffsetDateTime::parse(&stale_after, &Rfc3339).map_err(|_| error("runtime_binding_stale_after_invalid", "stale_after must be RFC 3339"))?; }
    let registry = read_object_or(&identity_path(&site_root), json!({"schema":IDENTITY_SCHEMA,"identities":[]}))?;
    if !registry.get("identities").and_then(Value::as_array).into_iter().flatten().any(|entry| entry.get("identity_id").and_then(Value::as_str) == Some(identity_id.as_str())) { return Err(error("identity_not_admitted", "identity must be durably admitted before runtime binding")); }
    let mut bindings = read_bindings(&site_root)?;
    let mut digest = Sha256::new(); digest.update(format!("{identity_id}:{runtime_locus}:{handle}"));
    let binding_id = format!("bind_{:x}", digest.finalize())[..21].to_string();
    if let Some(existing) = bindings.iter().find(|binding| binding.get("binding_id").and_then(Value::as_str) == Some(binding_id.as_str())) {
        return Ok(json!({"schema":"narada.operator_surface.runtime_binding.v1","status":"reused","mutation_performed":false,"runtime_binding_mutated":false,"binding":existing,"binding_path":runtime_binding_path(&site_root),"authority_root":site_root}));
    }
    let now = now_iso();
    let binding = json!({"binding_id":binding_id,"identity_id":identity_id,"runtime_locus":runtime_locus,"handle":handle,"transport":if handle.starts_with("hwnd:"){"windows_hwnd"}else{"explicit_runtime_handle"},"submit_strategy":"known_surface_submit","input_capabilities":["type_text","submit"],"status":"active","stale_after":optional(args,"stale_after"),"target_evidence":{"requested_handle":handle,"observed_handle":observed_handle,"handle_source":"explicit_mcp_argument","window_title":optional(args,"window_title"),"window_class":optional(args,"window_class"),"process_name":optional(args,"process_name"),"process_id":optional(args,"process_id"),"ambient_foreground_used":false,"asserted_identity":identity_id,"runtime_locus":runtime_locus,"captured_at":now},"postcondition_evidence":{"asserted_identity":identity_id,"bound_handle":observed_handle,"binding_id":binding_id,"verified_at":now,"ambient_foreground_used":false}});
    bindings.retain(|entry| entry.get("identity_id").and_then(Value::as_str) != Some(identity_id.as_str())); bindings.push(binding.clone());
    let path = runtime_binding_path(&site_root); write_json(&path, &json!({"bindings":bindings}))?;
    Ok(json!({"schema":"narada.operator_surface.runtime_binding.v1","status":"admitted","reason":"runtime_binding_admitted","mutation_performed":true,"runtime_binding_mutated":true,"binding":binding,"binding_path":path,"authority_root":site_root,"ambient_foreground_refused":true,"authority_split":{"durable_identity_authority":identity_path(&site_root),"volatile_handle_authority":runtime_locus}}))
}

fn authority_root(args: &Map<String, Value>, invocation_root: &Path) -> Result<PathBuf, Value> {
    let supplied = required(args, "site_root")?; let root = PathBuf::from(supplied);
    let resolved = if root.file_name().is_some_and(|name| name.eq_ignore_ascii_case(".narada")) { root } else { root.join(".narada") };
    if !resolved.is_absolute() { return Err(error("site_root_absolute_required", "site_root must be absolute")); }
    let invocation = if invocation_root.file_name().is_some_and(|name| name.eq_ignore_ascii_case(".narada")) { invocation_root.to_path_buf() } else { invocation_root.join(".narada") };
    if invocation.exists() && fs::canonicalize(&invocation).ok() != fs::canonicalize(&resolved).ok() { return Err(error("site_authority_root_mismatch", "site_root does not match this surface's configured Site authority")); }
    Ok(resolved)
}
fn identity_path(root: &Path) -> PathBuf { root.join("operator-surfaces").join("identities.json") }
fn runtime_binding_path(root: &Path) -> PathBuf { root.join("operator-surfaces").join("runtime-bindings.json") }
fn read_bindings(root: &Path) -> Result<Vec<Value>, Value> { let value=read_object_or(&runtime_binding_path(root),json!({"bindings":[]}))?; value.get("bindings").and_then(Value::as_array).cloned().ok_or_else(||error("operator_surface_runtime_bindings_invalid","bindings must be an array")) }
fn read_object_or(path: &Path, fallback: Value) -> Result<Value, Value> { if !path.exists(){return Ok(fallback)}; let meta=fs::metadata(path).map_err(|e|error("operator_surface_read_failed",&e.to_string()))?; if meta.len()>MAX_REGISTRY_BYTES{return Err(error("operator_surface_artifact_too_large","operator-surface artifact exceeds 4 MiB"))}; let value:Value=serde_json::from_str(&fs::read_to_string(path).map_err(|e|error("operator_surface_read_failed",&e.to_string()))?).map_err(|e|error("operator_surface_artifact_invalid",&e.to_string()))?; if !value.is_object(){return Err(error("operator_surface_artifact_invalid","operator-surface artifact must be an object"))} Ok(value) }
fn write_json(path:&Path,value:&Value)->Result<(),Value>{ if let Some(parent)=path.parent(){fs::create_dir_all(parent).map_err(|e|error("operator_surface_directory_failed",&e.to_string()))?}; let text=serde_json::to_string_pretty(value).map_err(|e|error("operator_surface_encode_failed",&e.to_string()))?+"\n"; if text.len() as u64>MAX_REGISTRY_BYTES{return Err(error("operator_surface_artifact_too_large","operator-surface artifact exceeds 4 MiB"))}; fs::write(path,text).map_err(|e|error("operator_surface_write_failed",&e.to_string())) }
fn require_authorized_mutation(args:&Map<String,Value>)->Result<(),Value>{ if args.get("execute").and_then(Value::as_bool)!=Some(true){return Err(error("site_lifecycle_execute_required","execute=true is required for mutation"))}; if !args.get("authority_basis").is_some_and(Value::is_object){return Err(error("site_lifecycle_authority_basis_required","authority_basis object is required for mutation"))} Ok(()) }
fn required(args:&Map<String,Value>,key:&str)->Result<String,Value>{optional(args,key).ok_or_else(||error("required_argument_missing",&format!("{key}_required")))}
fn optional(args:&Map<String,Value>,key:&str)->Option<String>{args.get(key).and_then(Value::as_str).map(str::trim).filter(|v|!v.is_empty()).map(str::to_string)}
fn now_iso()->String{OffsetDateTime::now_utc().format(&Rfc3339).unwrap_or_else(|_|"1970-01-01T00:00:00Z".into())}
fn error(code:&str,message:&str)->Value{json!({"schema":"narada.operator_surface.error.v1","code":code,"message":message})}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn admission_binding_observation_and_replay_are_durable() {
        let site=std::env::temp_dir().join(format!("narada-operator-authority-{}",uuid::Uuid::new_v4())); fs::create_dir_all(site.join(".narada")).expect("site");
        let admit=json!({"site_id":"fixture","site_root":site,"role":"builder","agent_kind":"codex_cli","by":"operator","execute":true,"authority_basis":{"kind":"operator_request"}}).as_object().cloned().unwrap();
        assert_eq!(admit_role(&admit,&site).unwrap()["status"],"admitted"); assert_eq!(admit_role(&admit,&site).unwrap()["status"],"reused");
        let bind=json!({"site_root":site,"identity":"fixture.builder","runtime_locus":"user-pc","handle":"codex-thread:fixture","observed_handle":"codex-thread:fixture","execute":true,"authority_basis":{"kind":"operator_request"}}).as_object().cloned().unwrap();
        assert_eq!(bind_runtime(&bind,&site).unwrap()["status"],"admitted"); assert_eq!(bind_runtime(&bind,&site).unwrap()["status"],"reused");
        let read=json!({"site_id":"fixture","site_root":site}).as_object().cloned().unwrap(); assert_eq!(verify_role(&read,&site).unwrap()["identities"][0]["runtime_bound"],true); assert_eq!(observe_runtime(&read,&site).unwrap()["binding_count"],1);
        fs::remove_dir_all(site).unwrap();
    }
}
