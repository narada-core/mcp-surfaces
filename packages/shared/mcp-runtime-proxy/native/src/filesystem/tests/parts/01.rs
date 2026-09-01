
    use super::*;
    use std::ffi::OsString;

    fn test_state(root: &Path, mode: &str) -> State {
        State {
            mode: mode.to_string(),
            allowed_roots: vec![root.to_path_buf()],
            root_entries: Vec::new(),
            output_root: root.to_path_buf(),
            audit_log_dir: None,
            cache: HashMap::new(),
            snapshots: HashMap::new(),
            snapshot_order: Vec::new(),
        }
    }

    fn test_root(name: &str) -> PathBuf {
        let root = env::temp_dir().join(format!("narada-fs-{name}-{}", std::process::id()));
        if root.exists() {
            fs::remove_dir_all(&root).expect("remove stale exact test root");
        }
        fs::create_dir_all(&root).expect("create test root");
        root
    }

    fn fake_env(entries: &[(&str, &str)]) -> HashMap<String, OsString> {
        entries
            .iter()
            .map(|(key, value)| ((*key).to_string(), OsString::from(*value)))
            .collect()
    }

    #[test]
    fn user_home_anchor_prefers_non_empty_userprofile() {
        let values = fake_env(&[
            ("USERPROFILE", r"C:\Users\andrey"),
            ("HOME", r"C:\Users\fallback"),
        ]);
        assert_eq!(
            user_home_anchor_from(|key| values.get(key).cloned()),
            Some(PathBuf::from(r"C:\Users\andrey"))
        );
    }

    #[test]
    fn user_home_anchor_uses_home_when_userprofile_is_missing() {
        let values = fake_env(&[("HOME", r"C:\Users\andrey")]);
        assert_eq!(
            user_home_anchor_from(|key| values.get(key).cloned()),
            Some(PathBuf::from(r"C:\Users\andrey"))
        );
    }

    #[test]
    fn logical_range_reads_page_without_refusal_and_publish_same_tool_continuation() {
        let root = test_root("logical-range-pagination");
        let path = root.join("large.txt");
        let content = (1..=2_505)
            .map(|line| format!("line-{line}"))
            .collect::<Vec<_>>()
            .join("\n");
        fs::write(&path, content).unwrap();
        let state = test_state(&root, "read");

        let first = read_file(
            &state,
            &json!({"path":path,"start_line":1,"end_line":3_000}),
            true,
        )
        .expect("a large logical range must return its first bounded page");
        assert_eq!(first["returned_lines"], 1_000);
        assert_eq!(first["requested_limit"], 3_000);
        assert_eq!(first["limit"], 1_000);
        assert_eq!(first["limit_adjusted"], true);
        assert_eq!(first["has_more"], true);
        assert_eq!(first["next_start_line"], 1_001);
        assert_eq!(first["continuation"]["tool"], "fs_read_file_range");
        assert_eq!(first["continuation"]["arguments"]["start_line"], 1_001);
        assert_eq!(first["continuation"]["arguments"]["end_line"], 3_000);

        let second = read_file(&state, &first["continuation"]["arguments"], true)
            .expect("continuation arguments must be directly reusable");
        assert_eq!(second["returned_lines"], 1_000);
        assert_eq!(second["next_start_line"], 2_001);

        let third = read_file(&state, &second["continuation"]["arguments"], true)
            .expect("the final page must stop cleanly at end of file");
        assert_eq!(third["returned_lines"], 505);
        assert_eq!(third["has_more"], false);
        assert_eq!(third["requested_range_complete"], true);
        assert!(third["continuation"].is_null());

        let range_tool = list_tools("read")
            .into_iter()
            .find(|tool| tool["name"] == "fs_read_file_range")
            .expect("range tool must be published");
        assert!(range_tool["description"]
            .as_str()
            .unwrap()
            .contains("continuation.arguments"));
        fs::remove_dir_all(&root).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn user_home_anchor_uses_home_drive_and_path_fallback() {
        let values = fake_env(&[("HOMEDRIVE", "C:"), ("HOMEPATH", r"\Users\andrey")]);
        assert_eq!(
            user_home_anchor_from(|key| values.get(key).cloned()),
            Some(PathBuf::from(r"C:\Users\andrey"))
        );
    }

    #[cfg(windows)]
    #[test]
    fn user_home_anchor_uses_appdata_parent_fallback() {
        let values = fake_env(&[("APPDATA", r"C:\Users\andrey\AppData\Roaming")]);
        assert_eq!(
            user_home_anchor_from(|key| values.get(key).cloned()),
            Some(PathBuf::from(r"C:\Users\andrey"))
        );
    }

    #[test]
    fn repository_inventory_honors_root_alias_and_normalizes_paths() {
        let root = test_root("inventory-root-alias");
        let scope = root.join("scope");
        fs::create_dir_all(&scope).unwrap();
        fs::write(scope.join("only.txt"), "needle\n").unwrap();
        fs::write(root.join("outside.txt"), "outside\n").unwrap();
        let mut state = test_state(&root, "read");

        let result = repository_inventory(
            &mut state,
            &json!({"root": scope, "pattern": "**/*", "limit": 10, "cache_policy": "refresh"}),
        )
        .unwrap();
        let paths = result["candidate_source_paths"].as_array().unwrap();
        assert_eq!(paths.len(), 1);
        assert!(paths[0].as_str().unwrap().ends_with("/scope/only.txt"));
        assert!(!paths[0].as_str().unwrap().contains('\\'));
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn explicit_snapshot_is_reported_as_reused_cache() {
        let root = test_root("snapshot-reuse");
        fs::write(root.join("one.txt"), "one\n").unwrap();
        let mut state = test_state(&root, "read");
        let first = search_tool(
            &mut state,
            &json!({"directory": root, "pattern": "**/*", "limit": 1, "cache_policy": "refresh"}),
            false,
        )
        .unwrap();
        assert_eq!(first["cache_hit"], false);
        let second = search_tool(
            &mut state,
            &json!({"directory": root, "pattern": "**/*", "limit": 1, "snapshot_id": first["snapshot_id"]}),
            false,
        )
        .unwrap();
        assert_eq!(second["cache_hit"], true);
        assert_eq!(second["snapshot_reused"], true);
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn environment_variable_paths_are_refused_before_resolution() {
        let root = test_root("environment-path");
        let state = test_state(&root, "read");
        let error = resolve_allowed(&state, Some("%USERPROFILE%/src"), "fs_stat").unwrap_err();
        assert_eq!(error.code, "path_environment_expansion_not_supported");
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn guidance_recommends_the_native_patch_recovery_sequence() {
        let root = test_root("guidance");
        let state = test_state(&root, "write");
        let result = guidance(&state, &json!({"workflow": "safe_edit"})).unwrap();
        assert_eq!(result["patch_recovery"]["apply_patch_available"], true);
        assert!(result["patch_recovery"]["sequence"]
            .to_string()
            .contains("Call fs_apply_patch once"));
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn mutation_results_expose_canonical_content_hash() {
        let root = test_root("mutation-hash");
        let state = test_state(&root, "write");
        let path = root.join("value.txt");
        let written = write_file(&state, &json!({"path": path, "content": "before\n"})).unwrap();
        assert_eq!(written["sha256"], written["after_sha256"]);
        assert_eq!(written["content_sha256"], written["after_sha256"]);
        let replaced = str_replace_file(
            &state,
            &json!({"path": path, "old": "before", "new": "after", "expected_sha256": written["sha256"]}),
        )
        .unwrap();
        assert_eq!(replaced["sha256"], replaced["after_sha256"]);
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn tool_text_is_a_compact_projection_of_structured_content() {
        let result = tool_result(json!({"schema": "example.v1", "status": "ok", "large": [1,2,3]}));
        let text = result["content"][0]["text"].as_str().unwrap();
        assert!(text.len() < 100);
        assert!(!text.contains("large"));
        assert_eq!(result["structuredContent"]["large"][2], 3);
    }

    #[test]
    fn every_filesystem_schema_is_named_closed_and_bounded() {
        for mode in ["read", "write"] {
            for tool in list_tools(mode) {
                let schema = &tool["inputSchema"];
                assert!(
                    schema["title"]
                        .as_str()
                        .is_some_and(|value| !value.is_empty()),
                    "{}",
                    tool["name"]
                );
                assert_eq!(schema["additionalProperties"], false, "{}", tool["name"]);
                for (field, property) in schema["properties"].as_object().unwrap() {
                    match property["type"].as_str().unwrap_or_default() {
                        "string" => assert!(
                            property["maxLength"].is_number(),
                            "{}:{field}",
                            tool["name"]
                        ),
                        "array" => {
                            assert!(property["maxItems"].is_number(), "{}:{field}", tool["name"])
                        }
                        "integer" => {
                            assert!(property["minimum"].is_number(), "{}:{field}", tool["name"]);
                            assert!(property["maximum"].is_number(), "{}:{field}", tool["name"]);
                        }
                        "object" => assert!(
                            property["maxProperties"].is_number(),
                            "{}:{field}",
                            tool["name"]
                        ),
                        _ => {}
                    }
                }
                assert!(tool["outputSchema"]["title"].is_string());
            }
        }
    }

