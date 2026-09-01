fn durable_bundle_publish(
    publications: &[Publication],
    carrier_root: &Path,
    commit_pointer_path: &Path,
    bundle_id: &str,
) -> Result<Value, Failure> {
    if publications
        .last()
        .is_none_or(|publication| !path_eq(&publication.path, commit_pointer_path))
    {
        return Err(Failure::new(
            "materializer_commit_pointer_not_last",
            path_text(commit_pointer_path),
        ));
    }
    let _publication_lock = acquire_publication_lock(carrier_root)?;
    recover_pending_transactions(carrier_root)?;
    let current_pointer_hash = fs::read(commit_pointer_path)
        .ok()
        .map(|bytes| sha256(&bytes))
        .unwrap_or_else(|| "absent".to_string());
    let transaction_id = sha256(
        format!(
            "{bundle_id}:{current_pointer_hash}:{}:{}",
            std::process::id(),
            OffsetDateTime::now_utc().unix_timestamp_nanos()
        )
        .as_bytes(),
    );
    let transaction_root = carrier_root.join("transactions").join(&transaction_id);
    fs::create_dir_all(&transaction_root).map_err(|error| {
        Failure::new(
            "materializer_transaction_directory_failed",
            error.to_string(),
        )
    })?;
    for publication in publications {
        if !same_volume(&transaction_root, &publication.path) {
            return Err(Failure::new(
                "materializer_transaction_cross_volume_refused",
                path_text(&publication.path),
            )
            .with_details(json!({"transaction_root":path_text(&transaction_root)})));
        }
    }
    let candidate_root = transaction_root.join("candidates");
    let preimage_root = transaction_root.join("preimages");
    fs::create_dir_all(&candidate_root).map_err(|error| {
        Failure::new(
            "materializer_transaction_directory_failed",
            error.to_string(),
        )
    })?;
    fs::create_dir_all(&preimage_root).map_err(|error| {
        Failure::new(
            "materializer_transaction_directory_failed",
            error.to_string(),
        )
    })?;
    let mut items = Vec::new();
    let mut snapshots = Vec::new();
    for (index, publication) in publications.iter().enumerate() {
        let preimage = fs::read(&publication.path).ok();
        let candidate_path = candidate_root.join(format!("{index}.bin"));
        atomic_write(&candidate_path, &publication.content).map_err(|error| {
            Failure::new(
                "materializer_transaction_candidate_write_failed",
                error.to_string(),
            )
        })?;
        let preimage_path = preimage
            .as_ref()
            .map(|content| {
                let path = preimage_root.join(format!("{index}.bin"));
                atomic_write(&path, content).map(|_| path).map_err(|error| {
                    Failure::new(
                        "materializer_transaction_preimage_write_failed",
                        error.to_string(),
                    )
                })
            })
            .transpose()?;
        items.push(json!({
            "order": index,
            "path": path_text(&publication.path),
            "candidate_path": path_text(&candidate_path),
            "candidate_sha256": sha256(&publication.content),
            "preimage_path": preimage_path.as_ref().map(|path|path_text(path)),
            "preimage_sha256": preimage.as_ref().map(|content|sha256(content)),
            "state": "prepared",
        }));
        snapshots.push(Snapshot {
            path: publication.path.clone(),
            content: preimage,
        });
    }
    let journal_path = transaction_root.join("journal.json");
    let mut journal = json!({
        "schema": "narada.carrier_generation_transaction.v1",
        "transaction_id": transaction_id,
        "bundle_id": bundle_id,
        "state": "prepared",
        "commit_pointer_path": path_text(commit_pointer_path),
        "items": items,
        "threat_model": "cooperating_same_user_processes_and_crash_recovery",
    });
    write_transaction_journal(&journal_path, &journal)?;
    journal["state"] = json!("promoting");
    write_transaction_journal(&journal_path, &journal)?;
    for (index, publication) in publications.iter().enumerate() {
        let current = fs::read(&publication.path).ok();
        let preimage = &snapshots[index].content;
        if current.as_deref() != preimage.as_deref()
            && current.as_deref() != Some(publication.content.as_slice())
        {
            journal["state"] = json!("blocked_recovery");
            write_transaction_journal(&journal_path, &journal)?;
            return Err(Failure::new(
                "materializer_transaction_cas_conflict",
                path_text(&publication.path),
            )
            .with_details(json!({"transaction_id":transaction_id})));
        }
        if current.as_deref() != Some(publication.content.as_slice()) {
            if let Err(error) = atomic_write(&publication.path, &publication.content) {
                journal["state"] = json!("recovery_required");
                write_transaction_journal(&journal_path, &journal)?;
                if index + 1 < publications.len() {
                    let rollback_errors = rollback(&snapshots);
                    if rollback_errors.is_empty() {
                        journal["state"] = json!("aborted");
                        write_transaction_journal(&journal_path, &journal)?;
                    }
                }
                return Err(
                    Failure::new("materializer_transaction_failed", error.to_string())
                        .with_details(json!({
                            "failed_path":path_text(&publication.path),
                            "transaction_id":transaction_id,
                        })),
                );
            }
        }
        let installed = fs::read(&publication.path).map_err(|error| {
            Failure::new("materializer_transaction_verify_failed", error.to_string())
        })?;
        if sha256(&installed) != sha256(&publication.content) {
            journal["state"] = json!("recovery_required");
            write_transaction_journal(&journal_path, &journal)?;
            return Err(Failure::new(
                "materializer_transaction_verify_failed",
                path_text(&publication.path),
            ));
        }
        journal["items"][index]["state"] = json!("published");
        write_transaction_journal(&journal_path, &journal)?;
        if env::var("NARADA_MATERIALIZER_CRASH_AFTER_WRITE")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            == Some(index + 1)
        {
            std::process::exit(86);
        }
    }
    journal["state"] = json!("committed");
    write_transaction_journal(&journal_path, &journal)?;
    Ok(json!({
        "schema":"narada.carrier_generation_transaction_result.v1",
        "status":"committed",
        "transaction_id":transaction_id,
        "journal_path":path_text(&journal_path),
        "publication_count":publications.len(),
    }))
}

fn write_transaction_journal(path: &Path, journal: &Value) -> Result<(), Failure> {
    let content = pretty_json(journal)?;
    atomic_write(path, &content).map_err(|error| {
        Failure::new(
            "materializer_transaction_journal_write_failed",
            error.to_string(),
        )
    })?;
    let installed = fs::read(path).map_err(|error| {
        Failure::new(
            "materializer_transaction_journal_verify_failed",
            error.to_string(),
        )
    })?;
    if installed != content {
        return Err(Failure::new(
            "materializer_transaction_journal_verify_failed",
            path_text(path),
        ));
    }
    Ok(())
}

