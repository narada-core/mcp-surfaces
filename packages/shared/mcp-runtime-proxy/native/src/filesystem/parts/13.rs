
fn find_patch_context(lines: &[String], context: &[&str], start: usize) -> Option<usize> {
    if context.is_empty() {
        return Some(start.min(lines.len()));
    }
    if context.len() > lines.len() || start > lines.len() - context.len() {
        return None;
    }
    (start..=lines.len() - context.len()).find(|index| {
        lines[*index..*index + context.len()]
            .iter()
            .map(String::as_str)
            .eq(context.iter().copied())
    })
}

fn expected_patch_hashes(args: &Value) -> Result<HashMap<String, String>, FsError> {
    let Some(value) = args.get("expected_sha256") else {
        return Ok(HashMap::new());
    };
    let object = value.as_object().ok_or_else(|| {
        FsError::new(
            "expected_sha256_must_be_object",
            "expected_sha256_must_be_object",
            json!({}),
        )
    })?;
    let mut result = HashMap::new();
    for (key, value) in object {
        let hash = value.as_str().unwrap_or_default();
        if !valid_sha256(hash) {
            return Err(FsError::new(
                "expected_sha256_value_invalid",
                "expected_sha256_value_invalid",
                json!({"key":key}),
            ));
        }
        result.insert(key.replace('\\', "/"), hash.to_ascii_lowercase());
    }
    Ok(result)
}

fn match_expected_patch_hash(
    expected: &HashMap<String, String>,
    matched: &mut std::collections::HashSet<String>,
    file: &ParsedPatchFile,
    source: &Path,
    target: &Path,
    before: Option<&[u8]>,
) -> Result<(), FsError> {
    for key in [
        file.old_path.as_deref(),
        file.new_path.as_deref(),
        Some(normalize_path(source).as_str()),
        Some(normalize_path(target).as_str()),
    ]
    .into_iter()
    .flatten()
    {
        if let Some(want) = expected.get(key) {
            let actual = before.map(sha256_bytes);
            if actual.as_deref() != Some(want) {
                return Err(FsError::new(
                    "fs_apply_patch_expected_sha256_mismatch",
                    "fs_apply_patch_expected_sha256_mismatch",
                    json!({"key":key,"expected_sha256":want,"actual_sha256":actual}),
                ));
            }
            matched.insert(key.to_string());
        }
    }
    Ok(())
}

fn patch_change(plan: &PlannedPatch) -> Value {
    json!({"path":plan.target,"root":plan.root,"relative_path":relative_path(&plan.root,&plan.target),"operation":if plan.parsed.delete{"delete"}else if plan.parsed.old_path.is_none(){"add"}else if !same_path(&plan.source,&plan.target){"move"}else{"update"},"hunks":plan.parsed.hunks.len(),"deleted":plan.parsed.delete,"before_sha256":plan.before.as_deref().map(sha256_bytes),"after_sha256":plan.after.as_deref().map(sha256_bytes)})
}

fn patch_states(plan: &PlannedPatch, after: bool) -> Vec<Value> {
    let mut values = Vec::new();
    let content = if after {
        plan.after.as_deref()
    } else {
        plan.target_before.as_deref()
    };
    values.push(
        json!({"path":plan.target,"exists":content.is_some(),"sha256":content.map(sha256_bytes)}),
    );
    if !same_path(&plan.source, &plan.target) {
        let source_content = if after { None } else { plan.before.as_deref() };
        values.push(json!({"path":plan.source,"exists":source_content.is_some(),"sha256":source_content.map(sha256_bytes)}));
    }
    values
}

fn apply_planned_patch(
    state: &State,
    plans: &[PlannedPatch],
    started: std::time::Instant,
    timeout_ms: u64,
) -> Result<(), FsError> {
    for plan in plans {
        if started.elapsed().as_millis() as u64 > timeout_ms {
            return Err(FsError::new(
                "fs_apply_patch_timed_out",
                "fs_apply_patch_timed_out",
                json!({"phase":"mutation","timeout_ms":timeout_ms}),
            ));
        }
        if let Some(after) = &plan.after {
            if let Some(parent) = plan.target.parent() {
                fs::create_dir_all(parent).map_err(|e| {
                    FsError::new(
                        "patch_parent_create_failed",
                        format!("patch_parent_create_failed: {e}"),
                        json!({"path":parent}),
                    )
                })?;
            }
            fs::write(&plan.target, after).map_err(|e| {
                FsError::new(
                    "patch_write_failed",
                    format!("patch_write_failed: {e}"),
                    path_details(&plan.target, &plan.root),
                )
            })?;
            if !same_path(&plan.source, &plan.target) && plan.source.exists() {
                fs::remove_file(&plan.source).map_err(|e| {
                    FsError::new(
                        "patch_move_source_remove_failed",
                        format!("patch_move_source_remove_failed: {e}"),
                        path_details(&plan.source, &plan.root),
                    )
                })?;
            }
        } else {
            fs::remove_file(&plan.source).map_err(|e| {
                FsError::new(
                    "patch_delete_failed",
                    format!("patch_delete_failed: {e}"),
                    path_details(&plan.source, &plan.root),
                )
            })?;
        }
        append_audit(
            state,
            "fs_apply_patch",
            &plan.target,
            &plan.root,
            json!({"before_sha256":plan.before.as_deref().map(sha256_bytes),"after_sha256":plan.after.as_deref().map(sha256_bytes),"hunks":plan.parsed.hunks.len()}),
        )?;
    }
    Ok(())
}

fn rollback_planned_patch(plans: &[PlannedPatch]) {
    for plan in plans.iter().rev() {
        if let Some(before) = &plan.before {
            if let Some(parent) = plan.source.parent() {
                let _ = fs::create_dir_all(parent);
            }
            let _ = fs::write(&plan.source, before);
        } else if plan.source.exists() {
            let _ = fs::remove_file(&plan.source);
        }
        if !same_path(&plan.source, &plan.target) {
            if let Some(before) = &plan.target_before {
                if let Some(parent) = plan.target.parent() {
                    let _ = fs::create_dir_all(parent);
                }
                let _ = fs::write(&plan.target, before);
            } else if plan.target.exists() {
                let _ = fs::remove_file(&plan.target);
            }
        }
    }
}

fn patch_outcome_path(state: &State, operation: &str) -> PathBuf {
    state
        .output_root
        .join(".narada/local-filesystem-mcp/patch-outcomes")
        .join(format!("{operation}.json"))
}
fn read_patch_outcome(state: &State, operation: &str) -> Result<Option<Value>, FsError> {
    let path = patch_outcome_path(state, operation);
    match fs::read(&path) {
        Ok(bytes) => {
            if bytes.len() > 2 * 1024 * 1024 {
                return Err(FsError::new(
                    "fs_patch_outcome_too_large",
                    "fs_patch_outcome_too_large",
                    json!({"path":path}),
                ));
            }
            serde_json::from_slice(&bytes).map(Some).map_err(|e| {
                FsError::new(
                    "fs_patch_outcome_invalid",
                    format!("fs_patch_outcome_invalid: {e}"),
                    json!({"path":path}),
                )
            })
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(FsError::new(
            "fs_patch_outcome_read_failed",
            format!("fs_patch_outcome_read_failed: {e}"),
            json!({"path":path}),
        )),
    }
}
