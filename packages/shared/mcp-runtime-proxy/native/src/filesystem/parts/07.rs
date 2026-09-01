
fn delete_directory(state: &State, args: &Value) -> Result<Value, FsError> {
    let (path, root) = resolve_allowed(
        state,
        args.get("path").and_then(Value::as_str),
        "fs_delete_directory",
    )?;
    assert_mutation_target_allowed(&path, &root, "fs_delete_directory")?;
    assert_not_authority_root(&path, &root, "fs_delete_directory")?;
    let recursive = args
        .get("recursive")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if !path.exists() {
        return Err(FsError::new(
            "delete_directory_not_found",
            "delete_directory_not_found",
            path_details(&path, &root),
        ));
    }
    if !path.is_dir() {
        return Err(FsError::new(
            "delete_directory_target_not_directory",
            "delete_directory_target_not_directory",
            path_details(&path, &root),
        ));
    }
    metadata_guard(
        args,
        Some("expected"),
        "expected",
        &path,
        &root,
        "fs_delete_directory",
    )?;
    let entry_count = fs::read_dir(&path)
        .map(|entries| entries.count())
        .unwrap_or(0);
    if entry_count > 0 && !recursive {
        return Err(FsError::new(
            "delete_directory_not_empty",
            "delete_directory_not_empty",
            json!({"path": path, "root": root, "entry_count": entry_count}),
        ));
    }
    let result = if recursive {
        fs::remove_dir_all(&path)
    } else {
        fs::remove_dir(&path)
    };
    result.map_err(|error| {
        FsError::new(
            "delete_directory_failed",
            format!("delete_directory_failed: {error}"),
            path_details(&path, &root),
        )
    })?;
    append_audit(
        state,
        "fs_delete_directory",
        &path,
        &root,
        json!({"recursive": recursive, "entry_count": entry_count}),
    )?;
    Ok(
        json!({"schema": "local.filesystem.delete_directory.v1", "status": "deleted", "path": path, "root": root, "relative_path": relative_path(&root, &path), "recursive": recursive}),
    )
}

fn move_path(state: &State, args: &Value, directory_only: bool) -> Result<Value, FsError> {
    let operation = if directory_only {
        "fs_rename_directory"
    } else {
        "fs_move_path"
    };
    let (from, from_root) =
        resolve_allowed(state, args.get("from").and_then(Value::as_str), operation)?;
    let (to, to_root) = resolve_allowed(state, args.get("to").and_then(Value::as_str), operation)?;
    assert_mutation_target_allowed(&to, &to_root, operation)?;
    assert_not_authority_root(&from, &from_root, operation)?;
    assert_not_authority_root(&to, &to_root, operation)?;
    if same_path(&from, &to) {
        return Err(FsError::new(
            "move_source_and_destination_same",
            "move_source_and_destination_same",
            json!({"operation": operation, "from": path_details(&from, &from_root), "to": path_details(&to, &to_root)}),
        ));
    }
    if !from.exists() {
        return Err(FsError::new(
            "move_source_not_found",
            "move_source_not_found",
            json!({"operation": operation, "from": path_details(&from, &from_root)}),
        ));
    }
    let from_is_dir = from.is_dir();
    if directory_only && !from_is_dir {
        return Err(FsError::new(
            "rename_directory_source_not_directory",
            "rename_directory_source_not_directory",
            path_details(&from, &from_root),
        ));
    }
    metadata_guard(
        args,
        Some("expected_from"),
        "expected_from",
        &from,
        &from_root,
        operation,
    )?;
    if from_is_dir && within(&from, &to) {
        return Err(FsError::new(
            "move_destination_inside_source",
            "move_destination_inside_source",
            json!({"operation": operation, "from": path_details(&from, &from_root), "to": path_details(&to, &to_root)}),
        ));
    }
    let overwrite = args
        .get("overwrite")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let destination_exists = to.exists();
    let backup = if destination_exists {
        if !overwrite {
            return Err(FsError::new(
                "move_destination_exists",
                "move_destination_exists",
                json!({"operation": operation, "to": path_details(&to, &to_root)}),
            ));
        }
        if from_is_dir != to.is_dir() {
            return Err(FsError::new(
                "move_destination_type_mismatch",
                "move_destination_type_mismatch",
                json!({"operation": operation, "to": path_details(&to, &to_root)}),
            ));
        }
        metadata_guard(
            args,
            Some("expected_to"),
            "expected_to",
            &to,
            &to_root,
            operation,
        )?;
        let candidate = backup_sibling(&to);
        fs::rename(&to, &candidate).map_err(|error| {
            FsError::new(
                "move_destination_backup_failed",
                format!("move_destination_backup_failed: {error}"),
                json!({"operation": operation, "to": to, "backup": candidate}),
            )
        })?;
        Some(candidate)
    } else {
        None
    };
    if let Some(parent) = to.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            FsError::new(
                "move_destination_parent_failed",
                format!("move_destination_parent_failed: {error}"),
                json!({"operation": operation, "parent": parent}),
            )
        })?;
    }
    if let Err(error) = fs::rename(&from, &to) {
        if let Some(backup_path) = backup.as_ref() {
            let _ = fs::rename(backup_path, &to);
        }
        return Err(FsError::new(
            "move_path_failed",
            format!("move_path_failed: {error}"),
            json!({"operation": operation, "from": from, "to": to}),
        ));
    }
    if let Some(backup_path) = backup.as_ref() {
        let _ = if from_is_dir {
            fs::remove_dir_all(backup_path)
        } else {
            fs::remove_file(backup_path)
        };
    }
    append_audit(
        state,
        operation,
        &to,
        &to_root,
        json!({"from": from, "from_root": from_root, "to": to, "to_root": to_root, "overwrite": overwrite}),
    )?;
    Ok(
        json!({"schema": if directory_only { "local.filesystem.rename_directory.v1" } else { "local.filesystem.move_path.v1" }, "status": "moved", "from": path_details(&from, &from_root), "to": path_details(&to, &to_root), "overwrite": overwrite}),
    )
}

fn metadata_guard(
    args: &Value,
    object_key: Option<&str>,
    prefix: &str,
    path: &Path,
    root: &Path,
    operation: &str,
) -> Result<(), FsError> {
    let object = object_key
        .and_then(|key| args.get(key))
        .and_then(Value::as_object);
    let value = |name: &str| {
        object
            .and_then(|entry| entry.get(name))
            .or_else(|| args.get(format!("{prefix}_{name}")))
    };
    let expected_size = value("size").and_then(Value::as_u64);
    let expected_mtime = value("mtime").and_then(Value::as_str);
    let expected_sha = value("sha256").and_then(Value::as_str);
    let expected_tree = value("tree_sha256").and_then(Value::as_str);
    let expected_entries = value("entry_count").and_then(Value::as_u64);
    if expected_mtime.is_none()
        && expected_size.is_none()
        && expected_sha.is_none()
        && expected_tree.is_none()
        && expected_entries.is_none()
    {
        return Ok(());
    }
    let metadata = fs::metadata(path).map_err(|error| {
        FsError::new(
            format!("{operation}_expected_metadata_mismatch"),
            format!("{operation}_expected_metadata_mismatch: {error}"),
            path_details(path, root),
        )
    })?;
    let actual_size = metadata.len();
    let actual_mtime = mtime_iso(&metadata);
    let (actual_tree, actual_entries) = if metadata.is_dir() {
        let (entries, _tree_entries, tree, _truncated) = directory_fingerprint(path, path);
        (Some(tree), Some(entries as u64))
    } else {
        (None, None)
    };
    let details = json!({"operation": operation, "path": path, "root": root, "expected_mtime":expected_mtime,"actual_mtime":actual_mtime,"expected_size": expected_size, "actual_size": actual_size, "expected_sha256": expected_sha, "expected_tree_sha256": expected_tree, "actual_tree_sha256": actual_tree, "expected_entry_count": expected_entries, "actual_entry_count": actual_entries});
    if expected_mtime.is_some_and(|expected| expected != actual_mtime)
        || expected_size.is_some_and(|expected| expected != actual_size)
        || expected_entries.is_some_and(|expected| Some(expected) != actual_entries)
        || expected_tree.is_some_and(|expected| Some(expected) != actual_tree.as_deref())
    {
        return Err(FsError::new(
            format!("{operation}_expected_metadata_mismatch"),
            format!("{operation}_expected_metadata_mismatch: {}", path.display()),
            details,
        ));
    }
    if let Some(expected) = expected_sha {
        if !metadata.is_file() {
            return Err(FsError::new(
                format!("{operation}_expected_sha256_not_supported"),
                format!(
                    "{operation}_expected_sha256_not_supported: {}",
                    path.display()
                ),
                details,
            ));
        }
        let actual = sha256_file_with_timeout(path, 60_000, operation)?;
        if actual != expected {
            return Err(FsError::new(
                format!("{operation}_expected_metadata_mismatch"),
                format!("{operation}_expected_metadata_mismatch: {}", path.display()),
                json!({"expected_sha256": expected, "actual_sha256": actual, "path": path, "root": root}),
            ));
        }
    }
    Ok(())
}
