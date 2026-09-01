
#[derive(Clone, Debug)]
struct ParsedPatchFile {
    old_path: Option<String>,
    new_path: Option<String>,
    move_to: Option<String>,
    delete: bool,
    hunks: Vec<ParsedPatchHunk>,
}

#[derive(Clone, Debug)]
struct ParsedPatchHunk {
    old_start: Option<usize>,
    lines: Vec<(char, String)>,
}

struct PlannedPatch {
    parsed: ParsedPatchFile,
    source: PathBuf,
    target: PathBuf,
    root: PathBuf,
    before: Option<Vec<u8>>,
    target_before: Option<Vec<u8>>,
    after: Option<Vec<u8>>,
}

fn apply_patch_tool(state: &State, args: &Value) -> Result<Value, FsError> {
    let patch = args
        .get("patch")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if patch.trim().is_empty() {
        return Err(FsError::new(
            "patch_required",
            "Patch text is required.",
            json!({}),
        ));
    }
    let operation_id = args
        .get("operation_id")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| {
            format!(
                "patch-{}-{}",
                std::process::id(),
                OffsetDateTime::now_utc().unix_timestamp_nanos()
            )
        });
    if !valid_operation_id(&operation_id) {
        return Err(FsError::new(
            "patch_operation_id_invalid",
            "patch_operation_id_invalid",
            json!({"operation_id":operation_id}),
        ));
    }
    let patch_sha256 = sha256_bytes(patch.as_bytes());
    let mut recovery_count = 0_u64;
    if let Some(previous) = read_patch_outcome(state, &operation_id)? {
        if previous.get("patch_sha256").and_then(Value::as_str) != Some(&patch_sha256) {
            return Err(FsError::new(
                "patch_operation_id_conflict",
                "patch_operation_id_conflict",
                json!({"operation_id":operation_id,"existing_patch_sha256":previous.get("patch_sha256"),"requested_patch_sha256":patch_sha256}),
            ));
        }
        if previous.get("status").and_then(Value::as_str) == Some("interrupted_before_mutation")
            && previous.get("retry_safe").and_then(Value::as_bool) == Some(true)
        {
            recovery_count = previous
                .get("recovery_count")
                .and_then(Value::as_u64)
                .unwrap_or(0)
                + 1;
        } else {
            let mut replay = previous;
            if let Some(object) = replay.as_object_mut() {
                object.insert("operation_replayed".into(), json!(true));
            }
            return Ok(replay);
        }
    }
    let timeout_ms = integer(args, "timeout_ms")
        .unwrap_or(WRITE_TIMEOUT_MS as i64)
        .clamp(1, 300_000) as u64;
    let started = std::time::Instant::now();
    write_patch_outcome(
        state,
        &operation_id,
        &json!({
            "schema":"local.filesystem.apply_patch.outcome.v1","status":"accepted","operation_id":operation_id,
            "patch_sha256":patch_sha256,"mutation_started":false,"owner_pid":std::process::id(),"timeout_ms":timeout_ms,
            "accepted_at":now_rfc3339(),"recovery_count":recovery_count
        }),
    )?;
    let parsed = match parse_patch(patch) {
        Ok(files) if !files.is_empty() => files,
        Ok(_) => {
            return patch_failure(
                state,
                &operation_id,
                &patch_sha256,
                FsError::new(
                    "patch_contains_no_files",
                    "patch_contains_no_files",
                    json!({"expected_format":"unified_diff_or_codex_apply_patch"}),
                ),
            )
        }
        Err(error) => return patch_failure(state, &operation_id, &patch_sha256, error),
    };
    macro_rules! plan_or_fail {
        ($expression:expr) => {
            match $expression {
                Ok(value) => value,
                Err(error) => return patch_failure(state, &operation_id, &patch_sha256, error),
            }
        };
    }
    let expected = plan_or_fail!(expected_patch_hashes(args));
    let mut matched = std::collections::HashSet::new();
    let mut plans = Vec::new();
    for file in parsed {
        if started.elapsed().as_millis() as u64 > timeout_ms {
            return patch_failure(
                state,
                &operation_id,
                &patch_sha256,
                FsError::new(
                    "fs_apply_patch_timed_out",
                    "fs_apply_patch_timed_out",
                    json!({"phase":"planning","timeout_ms":timeout_ms}),
                ),
            );
        }
        let source_name = file.old_path.as_deref().or(file.new_path.as_deref());
        let source_name = plan_or_fail!(source_name.ok_or_else(|| FsError::new(
            "patch_path_required",
            "patch_path_required",
            json!({})
        )));
        let target_name = plan_or_fail!(file
            .move_to
            .as_deref()
            .or(file.new_path.as_deref())
            .or(file.old_path.as_deref())
            .ok_or_else(|| FsError::new(
                "patch_target_path_required",
                "patch_target_path_required",
                json!({})
            )));
        let (source, source_root) =
            plan_or_fail!(resolve_allowed(state, Some(source_name), "fs_apply_patch"));
        let (target, target_root) =
            plan_or_fail!(resolve_allowed(state, Some(target_name), "fs_apply_patch"));
        if source_root != target_root {
            return patch_failure(
                state,
                &operation_id,
                &patch_sha256,
                FsError::new(
                    "patch_cross_root_move_refused",
                    "patch_cross_root_move_refused",
                    json!({"source":source,"target":target}),
                ),
            );
        }
        if file.delete || !same_path(&source, &target) {
            plan_or_fail!(assert_not_authority_root(
                &source,
                &source_root,
                "fs_apply_patch"
            ));
        }
        plan_or_fail!(assert_not_authority_root(
            &target,
            &target_root,
            "fs_apply_patch"
        ));
        if !file.delete {
            plan_or_fail!(assert_mutation_target_allowed(
                &target,
                &target_root,
                "fs_apply_patch"
            ));
        }
        if source.exists()
            && fs::metadata(&source).is_ok_and(|metadata| metadata.len() > MAX_TEXT_MUTATION_BYTES)
        {
            return patch_failure(
                state,
                &operation_id,
                &patch_sha256,
                FsError::new(
                    "fs_apply_patch_source_too_large",
                    "fs_apply_patch_source_too_large",
                    json!({"path":source,"max_bytes":MAX_TEXT_MUTATION_BYTES}),
                ),
            );
        }
        if !same_path(&source, &target)
            && target.exists()
            && fs::metadata(&target).is_ok_and(|metadata| metadata.len() > MAX_TEXT_MUTATION_BYTES)
        {
            return patch_failure(
                state,
                &operation_id,
                &patch_sha256,
                FsError::new(
                    "fs_apply_patch_target_too_large",
                    "fs_apply_patch_target_too_large",
                    json!({"path":target,"max_bytes":MAX_TEXT_MUTATION_BYTES}),
                ),
            );
        }
        let before = if source.exists() {
            Some(plan_or_fail!(fs::read(&source).map_err(|error| {
                FsError::new(
                    "patch_source_read_failed",
                    format!("patch_source_read_failed: {error}"),
                    path_details(&source, &source_root),
                )
            })))
        } else {
            None
        };
        let target_before = if same_path(&source, &target) {
            before.clone()
        } else if target.exists() {
            Some(plan_or_fail!(fs::read(&target).map_err(|error| {
                FsError::new(
                    "patch_target_read_failed",
                    format!("patch_target_read_failed: {error}"),
                    path_details(&target, &target_root),
                )
            })))
        } else {
            None
        };
        if file.old_path.is_some() && before.is_none() {
            return patch_failure(
                state,
                &operation_id,
                &patch_sha256,
                FsError::new(
                    "patch_source_not_found",
                    "patch_source_not_found",
                    path_details(&source, &source_root),
                ),
            );
        }
        plan_or_fail!(match_expected_patch_hash(
            &expected,
            &mut matched,
            &file,
            &source,
            &target,
            before.as_deref(),
        ));
        let after = if file.delete {
            plan_or_fail!(apply_patch_content(
                before.as_deref().unwrap_or_default(),
                &file.hunks,
                true
            )
            .map(|_| None))
        } else {
            Some(plan_or_fail!(apply_patch_content(
                before.as_deref().unwrap_or_default(),
                &file.hunks,
                false,
            )))
        };
        plans.push(PlannedPatch {
            parsed: file,
            source,
            target,
            root: target_root,
            before,
            target_before,
            after,
        });
    }
    let unmatched: Vec<_> = expected
        .keys()
        .filter(|key| !matched.contains(*key))
        .cloned()
        .collect();
    if !unmatched.is_empty() {
        return patch_failure(
            state,
            &operation_id,
            &patch_sha256,
            FsError::new(
                "fs_apply_patch_expected_sha256_unmatched",
                "fs_apply_patch_expected_sha256_unmatched",
                json!({"unmatched_expected_sha256_keys":unmatched}),
            ),
        );
    }
    let changes: Vec<Value> = plans.iter().map(patch_change).collect();
    if args
        .get("dry_run")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        let outcome = json!({"schema":"local.filesystem.apply_patch.outcome.v1","status":"checked","operation_id":operation_id,"patch_sha256":patch_sha256,"mutation_started":false,"dry_run":true,"timeout_ms":timeout_ms,"recovery_count":recovery_count,"changed_files":changes,"finished_at":now_rfc3339()});
        write_patch_outcome(state, &operation_id, &outcome)?;
        return Ok(outcome);
    }
    let recovery_plan = json!({
        "before_state":plans.iter().flat_map(|plan| patch_states(plan, false)).collect::<Vec<_>>(),
        "after_state":plans.iter().flat_map(|plan| patch_states(plan, true)).collect::<Vec<_>>(),
        "changed_files":changes
    });
    write_patch_outcome(
        state,
        &operation_id,
        &json!({"schema":"local.filesystem.apply_patch.outcome.v1","status":"applying","operation_id":operation_id,"patch_sha256":patch_sha256,"mutation_started":true,"owner_pid":std::process::id(),"timeout_ms":timeout_ms,"recovery_count":recovery_count,"started_at":now_rfc3339(),"recovery_plan":recovery_plan}),
    )?;
    let result = apply_planned_patch(state, &plans, started, timeout_ms);
    match result {
        Ok(()) => {
            let outcome = json!({"schema":"local.filesystem.apply_patch.outcome.v1","status":"patched","operation_id":operation_id,"patch_sha256":patch_sha256,"mutation_started":true,"rollback_performed":false,"recovery_count":recovery_count,"changed_files":changes,"finished_at":now_rfc3339(),"outcome_reader":{"tool":"fs_patch_outcome_show","operation_id":operation_id}});
            write_patch_outcome(state, &operation_id, &outcome)?;
            Ok(outcome)
        }
        Err(error) => {
            rollback_planned_patch(&plans);
            let outcome = json!({"schema":"local.filesystem.apply_patch.outcome.v1","status":"failed_rolled_back","operation_id":operation_id,"patch_sha256":patch_sha256,"mutation_started":true,"rollback_performed":true,"rollback_succeeded":true,"error":diagnostic(&error),"finished_at":now_rfc3339()});
            write_patch_outcome(state, &operation_id, &outcome)?;
            Err(error)
        }
    }
}
