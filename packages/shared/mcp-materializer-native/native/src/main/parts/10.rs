fn recover_pending_transactions(carrier_root: &Path) -> Result<Value, Failure> {
    let transactions_root = carrier_root.join("transactions");
    if !transactions_root.exists() {
        return Ok(json!({"status":"nothing_to_recover","recovered":[]}));
    }
    let mut recovered = Vec::new();
    for entry in fs::read_dir(&transactions_root).map_err(|error| {
        Failure::new(
            "materializer_transaction_inventory_failed",
            error.to_string(),
        )
    })? {
        let entry = entry.map_err(|error| {
            Failure::new(
                "materializer_transaction_inventory_failed",
                error.to_string(),
            )
        })?;
        if !entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false) {
            continue;
        }
        let journal_path = entry.path().join("journal.json");
        if !journal_path.exists() {
            continue;
        }
        let mut journal = read_json(&journal_path, "materializer_transaction_journal_invalid")?;
        let state = journal.get("state").and_then(Value::as_str).unwrap_or("");
        if matches!(state, "committed" | "aborted") {
            continue;
        }
        let commit_pointer_path =
            PathBuf::from(json_field_string(&journal, "commit_pointer_path")?);
        let items = journal
            .get("items")
            .and_then(Value::as_array)
            .cloned()
            .ok_or_else(|| {
                Failure::new(
                    "materializer_transaction_journal_invalid",
                    path_text(&journal_path),
                )
            })?;
        let commit_item = items
            .iter()
            .find(|item| {
                item.get("path")
                    .and_then(Value::as_str)
                    .is_some_and(|path| path_eq(Path::new(path), &commit_pointer_path))
            })
            .ok_or_else(|| {
                Failure::new(
                    "materializer_transaction_commit_item_missing",
                    path_text(&journal_path),
                )
            })?;
        let pointer_committed = fs::read(&commit_pointer_path).ok().is_some_and(|bytes| {
            commit_item.get("candidate_sha256").and_then(Value::as_str)
                == Some(sha256(&bytes).as_str())
        });
        let ordered: Box<dyn Iterator<Item = &Value>> = if pointer_committed {
            Box::new(items.iter())
        } else {
            Box::new(items.iter().rev())
        };
        for item in ordered {
            let target = PathBuf::from(json_field_string(item, "path")?);
            let candidate_path = PathBuf::from(json_field_string(item, "candidate_path")?);
            let candidate = fs::read(&candidate_path).map_err(|error| {
                Failure::new(
                    "materializer_transaction_candidate_missing",
                    error.to_string(),
                )
            })?;
            let preimage_path = item.get("preimage_path").and_then(Value::as_str);
            let preimage = preimage_path
                .map(|path| {
                    fs::read(path).map_err(|error| {
                        Failure::new(
                            "materializer_transaction_preimage_missing",
                            error.to_string(),
                        )
                    })
                })
                .transpose()?;
            let current = fs::read(&target).ok();
            if current.as_deref() != Some(candidate.as_slice())
                && current.as_deref() != preimage.as_deref()
            {
                journal["state"] = json!("blocked_recovery");
                write_transaction_journal(&journal_path, &journal)?;
                return Err(Failure::new(
                    "materializer_transaction_recovery_cas_conflict",
                    path_text(&target),
                )
                .with_details(json!({"journal_path":path_text(&journal_path)})));
            }
            let desired = if pointer_committed {
                Some(candidate.as_slice())
            } else {
                preimage.as_deref()
            };
            match desired {
                Some(content) if current.as_deref() != Some(content) => {
                    atomic_write(&target, content).map_err(|error| {
                        Failure::new(
                            "materializer_transaction_recovery_write_failed",
                            error.to_string(),
                        )
                    })?;
                }
                None if current.is_some() => {
                    fs::remove_file(&target).map_err(|error| {
                        Failure::new(
                            "materializer_transaction_recovery_remove_failed",
                            error.to_string(),
                        )
                    })?;
                }
                _ => {}
            }
        }
        journal["state"] = json!(if pointer_committed {
            "committed"
        } else {
            "aborted"
        });
        write_transaction_journal(&journal_path, &journal)?;
        recovered.push(json!({
            "transaction_id":journal.get("transaction_id"),
            "resolution":journal.get("state"),
        }));
    }
    Ok(json!({"status":"recovered","recovered":recovered}))
}

fn same_volume(left: &Path, right: &Path) -> bool {
    #[cfg(windows)]
    {
        use std::path::Component;
        let prefix = |path: &Path| {
            path.components()
                .next()
                .and_then(|component| match component {
                    Component::Prefix(prefix) => {
                        Some(prefix.as_os_str().to_string_lossy().to_lowercase())
                    }
                    _ => None,
                })
        };
        prefix(left) == prefix(right)
    }
    #[cfg(not(windows))]
    {
        left.is_absolute() == right.is_absolute()
    }
}

fn validate_input(input: &MaterializationInput) -> Result<(), Failure> {
    if input.schema != INPUT_SCHEMA {
        return Err(Failure::new(
            "materializer_input_schema_unsupported",
            format!("Unsupported schema: {}", input.schema),
        ));
    }
    if input.carriers.is_empty() {
        return Err(Failure::new(
            "materializer_carriers_required",
            "At least one carrier is required.",
        ));
    }
    if !input.carrier_contract_path.is_absolute()
        || input.carrier_contract_fingerprint.len() != 64
        || !input
            .carrier_contract_fingerprint
            .chars()
            .all(|value| value.is_ascii_hexdigit())
    {
        return Err(Failure::new(
            "materializer_carrier_contract_source_invalid",
            path_text(&input.carrier_contract_path),
        ));
    }
    if !matches!(
        input.proxy_implementation.as_str(),
        "native" | "bun" | "node"
    ) {
        return Err(Failure::new(
            "materializer_proxy_implementation_invalid",
            "proxy_implementation must be native, bun, or node.",
        ));
    }
    let mut ids = BTreeSet::new();
    let mut paths = BTreeSet::new();
    for carrier in &input.carriers {
        validate_identifier(&carrier.carrier_id, "carrier_id")?;
        if !ids.insert(&carrier.carrier_id) {
            return Err(Failure::new(
                "materializer_carrier_id_duplicate",
                carrier.carrier_id.clone(),
            ));
        }
        if !carrier.config_path.is_absolute() {
            return Err(Failure::new(
                "materializer_config_path_not_absolute",
                path_text(&carrier.config_path),
            ));
        }
        if !paths.insert(&carrier.config_path) {
            return Err(Failure::new(
                "materializer_config_path_duplicate",
                path_text(&carrier.config_path),
            ));
        }
        let mut server_names = BTreeSet::new();
        let mut binding_ids = BTreeSet::new();
        for server in &carrier.servers {
            validate_identifier(&server.name, "server_name")?;
            if !server_names.insert(&server.name) {
                return Err(Failure::new(
                    "materializer_server_name_duplicate",
                    server.name.clone(),
                ));
            }
            if let Some(binding_id) = &server.binding_id {
                validate_identifier(binding_id, "binding_id")?;
                if !binding_ids.insert(binding_id) {
                    return Err(Failure::new(
                        "materializer_binding_id_duplicate",
                        binding_id.clone(),
                    ));
                }
            }
            if server.command.trim().is_empty() {
                return Err(Failure::new(
                    "materializer_server_command_required",
                    server.name.clone(),
                ));
            }
            if server.command.contains('\0') || server.args.iter().any(|arg| arg.contains('\0')) {
                return Err(Failure::new(
                    "materializer_nul_refused",
                    server.name.clone(),
                ));
            }
            validate_protocol_route(carrier, server)?;
            validate_proxy_launch(input, carrier, server)?;
        }
    }
    Ok(())
}

fn validate_protocol_route(carrier: &CarrierInput, server: &ServerInput) -> Result<(), Failure> {
    let carrier_protocol = "2026-07-28";
    let proxy_accepted_client_protocols = ["2026-07-28"];
    let proxy_emitted_server_protocol = carrier_protocol;
    let server_accepted_protocols: &[&str] = &["2026-07-28"];
    let translation_adapter: Option<&str> = None;
    let valid = proxy_accepted_client_protocols.contains(&carrier_protocol)
        && server_accepted_protocols.contains(&proxy_emitted_server_protocol);
    if valid {
        return Ok(());
    }
    Err(Failure::new(
        "materializer_protocol_route_incompatible",
        format!("{}:{}", carrier.carrier_id, server.name),
    )
    .with_details(json!({
        "carrier_id":carrier.carrier_id,
        "carrier_kind":carrier.carrier_kind,
        "server_name":server.name,
        "carrier_protocol":carrier_protocol,
        "proxy_accepted_client_protocols":proxy_accepted_client_protocols,
        "proxy_emitted_server_protocol":proxy_emitted_server_protocol,
        "server_accepted_protocols":server_accepted_protocols,
        "translation_adapter":translation_adapter,
        "invariant":"carrier_protocol must be accepted by proxy; proxy-emitted protocol must be accepted by server; a version-changing edge requires an explicitly admitted translation adapter"
    })))
}

