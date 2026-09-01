    #[test]
    fn native_apply_patch_supports_codex_unified_replay_and_conflict() {
        let root = test_root("native-patch");
        let state = test_state(&root, "write");
        fs::write(root.join("value.txt"), "one\ntwo\n").unwrap();
        let codex = "*** Begin Patch\n*** Update File: value.txt\n@@\n-one\n+ONE\n two\n*** Add File: added.txt\n+added\n*** End Patch";
        let first =
            apply_patch_tool(&state, &json!({"patch":codex,"operation_id":"patch-one"})).unwrap();
        assert_eq!(first["status"], "patched");
        assert_eq!(
            fs::read_to_string(root.join("value.txt")).unwrap(),
            "ONE\ntwo\n"
        );
        assert_eq!(
            fs::read_to_string(root.join("added.txt")).unwrap(),
            "added\n"
        );
        let replay =
            apply_patch_tool(&state, &json!({"patch":codex,"operation_id":"patch-one"})).unwrap();
        assert_eq!(replay["operation_replayed"], true);
        assert_eq!(apply_patch_tool(&state, &json!({"patch":"*** Begin Patch\n*** Delete File: added.txt\n*** End Patch","operation_id":"patch-one"})).unwrap_err().code, "patch_operation_id_conflict");
        let unified = "--- a/value.txt\n+++ b/value.txt\n@@ -1,2 +1,2 @@\n ONE\n-two\n+TWO";
        let checked = apply_patch_tool(
            &state,
            &json!({"patch":unified,"operation_id":"patch-two","dry_run":true}),
        )
        .unwrap();
        assert_eq!(checked["status"], "checked");
        assert_eq!(
            fs::read_to_string(root.join("value.txt")).unwrap(),
            "ONE\ntwo\n"
        );
        assert_eq!(
            patch_outcome(&state, &json!({"operation_id":"patch-two"})).unwrap()["status"],
            "checked"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn native_apply_patch_rejects_malformed_delete_add_without_panicking() {
        let root = test_root("native-patch-delete-add-malformed");
        let state = test_state(&root, "write");
        fs::write(root.join("old.txt"), "old\n").unwrap();
        let malformed = "*** Begin Patch\n*** Delete File: old.txt\n*** Add File: new.txt\nmissing-plus-prefix\n*** End Patch";
        let error = apply_patch_tool(
            &state,
            &json!({"patch":malformed,"operation_id":"delete-add-malformed"}),
        )
        .unwrap_err();
        assert_eq!(error.code, "patch_context_not_found");
        assert_eq!(fs::read_to_string(root.join("old.txt")).unwrap(), "old\n");
        assert!(!root.join("new.txt").exists());
        let outcome =
            patch_outcome(&state, &json!({"operation_id":"delete-add-malformed"})).unwrap();
        assert_eq!(outcome["status"], "failed_before_mutation");
        assert_eq!(outcome["retry_safe"], true);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn native_filesystem_rejects_unknown_and_unbounded_arguments() {
        assert_eq!(
            validate_tool_arguments(
                "write",
                "fs_write_file",
                &json!({"path":"x","content":"y","surprise":true})
            )
            .unwrap_err()
            .code,
            "tool_argument_unknown"
        );
        assert_eq!(
            validate_tool_arguments("read", "fs_read_file", &json!({"path":"x","limit":300_001}))
                .unwrap_err()
                .code,
            "tool_argument_integer_out_of_range"
        );
        assert_eq!(
            validate_tool_arguments(
                "write",
                "fs_apply_patch",
                &json!({"patch":"x","operation_id":"bad/id"})
            )
            .unwrap_err()
            .code,
            "patch_operation_id_invalid"
        );
    }

    #[test]
    fn native_apply_patch_moves_deletes_guards_and_records_recovery() {
        let root = test_root("native-patch-boundaries");
        let state = test_state(&root, "write");
        fs::write(root.join("move.txt"), "move me\n").unwrap();
        let moved = apply_patch_tool(&state, &json!({
            "patch":"*** Begin Patch\n*** Update File: move.txt\n*** Move to: moved.txt\n@@\n-move me\n+moved\n*** End Patch",
            "operation_id":"move-patch","expected_sha256":{"move.txt":sha256_bytes(b"move me\n")}
        })).unwrap();
        assert_eq!(moved["status"], "patched");
        assert!(!root.join("move.txt").exists());
        assert_eq!(
            fs::read_to_string(root.join("moved.txt")).unwrap(),
            "moved\n"
        );
        let deleted = apply_patch_tool(&state, &json!({
            "patch":"*** Begin Patch\n*** Delete File: moved.txt\n*** End Patch","operation_id":"delete-patch"
        })).unwrap();
        assert_eq!(deleted["changed_files"][0]["operation"], "delete");
        assert!(!root.join("moved.txt").exists());

        fs::write(root.join("guard.txt"), "guard\n").unwrap();
        let error = apply_patch_tool(&state, &json!({
            "patch":"*** Begin Patch\n*** Update File: guard.txt\n@@\n-guard\n+changed\n*** End Patch",
            "operation_id":"guard-patch","expected_sha256":{"guard.txt":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}
        })).unwrap_err();
        assert_eq!(error.code, "fs_apply_patch_expected_sha256_mismatch");
        assert_eq!(
            fs::read_to_string(root.join("guard.txt")).unwrap(),
            "guard\n"
        );
        let recovered = patch_outcome(&state, &json!({"operation_id":"guard-patch"})).unwrap();
        assert_eq!(recovered["status"], "failed_before_mutation");
        assert_eq!(recovered["retry_safe"], true);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn native_patch_reconciles_interrupted_applying_state() {
        let root = test_root("native-patch-recovery");
        let state = test_state(&root, "write");
        let path = root.join("recovered.txt");
        fs::write(&path, "after\n").unwrap();
        write_patch_outcome(&state,"recover-applying",&json!({
            "schema":"local.filesystem.apply_patch.outcome.v1","status":"applying","operation_id":"recover-applying",
            "patch_sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","owner_pid":4294967294_u32,
            "recovery_plan":{"before_state":[{"path":path,"exists":true,"sha256":sha256_bytes(b"before\n")}],"after_state":[{"path":path,"exists":true,"sha256":sha256_bytes(b"after\n")}],"changed_files":[]}
        })).unwrap();
        let outcome = patch_outcome(&state, &json!({"operation_id":"recover-applying"})).unwrap();
        assert_eq!(outcome["status"], "patched_recovered");
        assert_eq!(outcome["retry_safe"], false);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn native_reads_and_search_snapshots_have_hard_memory_bounds() {
        let root = test_root("bounded-memory");
        let mut state = test_state(&root, "read");
        let path = root.join("huge-line.txt");
        fs::write(&path, vec![b'x'; MAX_READ_LINE_BYTES + 1]).unwrap();
        assert_eq!(
            read_file(&state, &json!({"path":path,"limit":1}), false)
                .unwrap_err()
                .code,
            "fs_read_line_too_large"
        );
        for index in 0..5 {
            let id = format!("snapshot-{index}");
            state.snapshots.insert(id.clone(), (vec![id.clone()], true));
            touch_snapshot(&mut state, &id);
        }
        assert_eq!(state.snapshots.len(), 4);
        assert!(!state.snapshots.contains_key("snapshot-0"));
        state
            .snapshots
            .insert("truncated".into(), (vec!["one".into()], false));
        let boundary = search_tool(
            &mut state,
            &json!({
                "directory": root,
                "pattern": "*",
                "snapshot_id": "truncated",
                "offset": 1
            }),
            false,
        )
        .unwrap_err();
        assert_eq!(boundary.code, "fs_glob_search_capture_boundary_reached");
        let outcome = list_tools("read")
            .into_iter()
            .find(|tool| tool["name"] == "fs_patch_outcome_show")
            .unwrap();
        assert_eq!(outcome["annotations"]["readOnlyHint"], false);
        fs::remove_dir_all(root).unwrap();
    }
