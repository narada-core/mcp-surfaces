pub(crate) fn options_from_installed_index(path: &Path) -> Result<DeriveOptions, Failure> {
    require_absolute(path, "installed_index")?;
    let bytes = read_required(path, "materializer_installed_index_read_failed")?;
    let index: Value = serde_json::from_slice(&bytes)
        .map_err(|error| Failure::new("materializer_installed_index_invalid", error.to_string()))?;
    if index.get("schema").and_then(Value::as_str) != Some("narada.installed_carrier_index.v1") {
        return Err(Failure::new(
            "materializer_installed_index_schema_unsupported",
            path_text(path),
        ));
    }
    let generation = index
        .get("carriers")
        .and_then(Value::as_array)
        .and_then(|carriers| carriers.first())
        .and_then(|carrier| carrier.get("generation_sidecar_path"))
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .ok_or_else(|| {
            Failure::new(
                "materializer_installed_index_generation_required",
                path_text(path),
            )
        })?;
    options_from_generation(&generation)
}

fn read_required(path: &Path, code: &'static str) -> Result<Vec<u8>, Failure> {
    fs::read(path).map_err(|error| {
        Failure::new(code, error.to_string()).with_details(json!({"path": path_text(path)}))
    })
}

fn require_absolute(path: &Path, field: &'static str) -> Result<(), Failure> {
    if path.is_absolute() {
        Ok(())
    } else {
        Err(Failure::new(
            "materializer_derived_path_not_absolute",
            format!("{field}:{}", path_text(path)),
        ))
    }
}

fn required_string(value: &Value, field: &'static str) -> Result<String, Failure> {
    value
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string)
        .ok_or_else(|| Failure::new("materializer_registry_field_required", field))
}

fn string_array(value: &Value, field: &'static str) -> Result<Vec<String>, Failure> {
    value
        .get(field)
        .and_then(Value::as_array)
        .ok_or_else(|| Failure::new("materializer_registry_array_required", field))?
        .iter()
        .map(|item| {
            item.as_str()
                .map(str::to_string)
                .ok_or_else(|| Failure::new("materializer_registry_string_required", field))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn surface(scope: &str) -> Value {
        json!({
            "catalog_surface_id": "fixture",
            "binding_id": "fixture-binding",
            "injection_scope": scope,
            "authority_locus": {"kind": scope},
            "runtime_binding": {
                "transport": {
                    "type": "stdio",
                    "command": "fixture.exe",
                    "args": ["serve"]
                }
            },
            "surface_projection": {
                "projection_id": "default",
                "surface_descriptor": {
                    "projections": [{
                        "id": "default",
                        "transport": {"env": ["PATH"]}
                    }]
                }
            }
        })
    }

    #[test]
    fn ambient_admission_excludes_local_site_bindings() {
        assert!(ambient_binding_entry("site", &surface("local_site"), false)
            .unwrap()
            .is_none());
    }

    #[test]
    fn ambient_admission_carries_exact_host_and_user_bindings() {
        for scope in ["host", "user_site"] {
            let entry = ambient_binding_entry("site", &surface(scope), false)
                .unwrap()
                .expect("ambient binding");
            assert_eq!(entry["injection_scope"], scope);
            assert_eq!(entry["binding_id"], "fixture-binding");
            assert_eq!(
                entry["binding_digest"],
                binding_admission_entry_digest_v1(&entry)
            );
            assert_eq!(entry["binding_identity"]["command"], "fixture.exe");
        }
    }
}
