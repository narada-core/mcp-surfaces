
    use super::*;

    #[test]
    fn site_extra_allowed_roots_extend_structured_command_authority() {
        let site_root = env::temp_dir().join(format!(
            "narada-structured-command-site-roots-{}",
            unique_id("test")
        ));
        let worktree_root = site_root.with_extension("worktrees");
        fs::create_dir_all(site_root.join(".narada")).expect("control root");
        fs::create_dir_all(&worktree_root).expect("worktree root");
        fs::write(
            site_root.join(".narada/allowed-roots.json"),
            serde_json::to_vec(&json!({
                "schema": "narada.site.allowed_roots.v1",
                "extra_allowed_roots": [worktree_root.to_string_lossy()]
            }))
            .expect("config"),
        )
        .expect("write config");

        let state = parse_state(&[
            "--allowed-root".into(),
            site_root.to_string_lossy().to_string(),
            "--site-root".into(),
            site_root.to_string_lossy().to_string(),
        ])
        .expect("state");
        assert!(state
            .allowed_roots
            .iter()
            .any(|root| root == &worktree_root));

        fs::remove_dir_all(site_root).expect("cleanup site");
        fs::remove_dir_all(worktree_root).expect("cleanup worktrees");
    }

    #[test]
    fn default_policy_allows_native_cargo_workflows() {
        let root = env::temp_dir().join(format!(
            "narada-structured-command-cargo-policy-{}",
            unique_id("test")
        ));
        fs::create_dir_all(&root).expect("root");
        let state = parse_state(&[
            "--allowed-root".into(),
            root.to_string_lossy().to_string(),
        ])
        .expect("state");

        for (subcommand, args) in [
            ("fmt", vec!["--check"]),
            ("check", vec!["--locked"]),
            ("test", vec!["--locked"]),
            ("run", vec!["--locked"]),
            ("native-release", Vec::<&str>::new()),
        ] {
            let mut argv = vec![subcommand.to_string()];
            argv.extend(args.into_iter().map(String::from));
            let decision = decide(&state, "cargo", &argv, &root);
            assert_eq!(decision["status"], "allowed", "cargo {subcommand}: {decision}");
        }

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn refused_git_command_routes_to_site_git_mcp() {
        let root = env::temp_dir().join(format!(
            "narada-structured-command-git-routing-{}",
            unique_id("test")
        ));
        fs::create_dir_all(&root).expect("root");
        let state = parse_state(&[
            "--allowed-root".into(),
            root.to_string_lossy().to_string(),
        ])
        .expect("state");

        let decision = decide(
            &state,
            "git",
            &["status".into(), "--short".into(), "--branch".into()],
            &root,
        );
        assert_eq!(decision["status"], "refused");
        assert_eq!(decision["mcp_fallbacks"][0]["surface_id"], "git");
        assert_eq!(
            decision["mcp_fallbacks"][0]["activation_tool"],
            "mcp_loader_resume_or_open_surface"
        );
        assert_eq!(decision["mcp_fallbacks"][0]["child_tool_name"], "git_status");
        assert!(decision["remediation_hints"][0]
            .as_str()
            .is_some_and(|value| value.contains("Git MCP")));

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn refused_shell_discovery_routes_to_filesystem_mcp() {
        let root = env::temp_dir().join(format!(
            "narada-structured-command-filesystem-routing-{}",
            unique_id("test")
        ));
        fs::create_dir_all(&root).expect("root");
        let state = parse_state(&[
            "--allowed-root".into(),
            root.to_string_lossy().to_string(),
        ])
        .expect("state");

        let search = decide(&state, "rg", &["needle".into(), ".".into()], &root);
        assert_eq!(search["status"], "refused");
        assert_eq!(search["mcp_fallbacks"][0]["tool_name"], "fs_grep_search");
        assert_eq!(search["mcp_fallbacks"][0]["arguments"]["pattern"], "needle");

        let listing = decide(&state, "rg", &["--files".into()], &root);
        assert_eq!(listing["status"], "refused");
        assert_eq!(listing["mcp_fallbacks"][0]["tool_name"], "fs_glob_search");

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[cfg(windows)]
    #[test]
    fn windows_execution_environment_hydrates_complete_x64_msvc_toolchain() {
        let root = env::temp_dir().join(format!(
            "narada-structured-command-msvc-{}",
            unique_id("test")
        ));
        let visual_studio = root
            .join("Microsoft Visual Studio")
            .join("2022")
            .join("BuildTools");
        let msvc = visual_studio
            .join("VC")
            .join("Tools")
            .join("MSVC")
            .join("14.44.35207");
        let msvc_bin = msvc.join("bin").join("Hostx64").join("x64");
        fs::create_dir_all(&msvc_bin).expect("msvc bin");
        fs::create_dir_all(msvc.join("lib").join("x64")).expect("msvc lib");
        fs::create_dir_all(msvc.join("include")).expect("msvc include");
        fs::write(msvc_bin.join("link.exe"), b"fixture").expect("link fixture");

        let sdk = root.join("Windows Kits").join("10");
        let version = "10.0.26100.0";
        for include in ["ucrt", "shared", "um", "winrt", "cppwinrt"] {
            fs::create_dir_all(sdk.join("Include").join(version).join(include))
                .expect("sdk include");
        }
        fs::create_dir_all(sdk.join("Lib").join(version).join("ucrt").join("x64"))
            .expect("sdk ucrt lib");
        let sdk_um = sdk.join("Lib").join(version).join("um").join("x64");
        fs::create_dir_all(&sdk_um).expect("sdk um lib");
        fs::write(sdk_um.join("kernel32.lib"), b"fixture").expect("kernel fixture");
        fs::create_dir_all(sdk.join("bin").join(version).join("x64")).expect("sdk bin");

        let mut environment = std::collections::HashMap::from([
            (
                "ProgramFiles(x86)".to_string(),
                root.to_string_lossy().to_string(),
            ),
            ("Path".to_string(), r"C:\existing".to_string()),
        ]);
        augment_windows_msvc_environment(&mut environment);

        let path = environment_value(&environment, "PATH").expect("path");
        assert_eq!(
            env::split_paths(path).next().as_deref(),
            Some(msvc_bin.as_path())
        );
        let lib = environment_value(&environment, "LIB").expect("lib");
        assert!(env::split_paths(lib).any(|path| path == sdk_um));
        assert_eq!(
            environment_value(&environment, "VCToolsInstallDir"),
            Some(msvc.to_string_lossy().as_ref())
        );
        assert_eq!(
            environment_value(&environment, "WindowsSDKVersion"),
            Some("10.0.26100.0\\")
        );

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn command_tools_publish_closed_alternative_aware_schemas() {
        let tools = list_tools();
        for name in [
            "structured_command_execute",
            "structured_command_start",
            "structured_command_input_create",
        ] {
            let tool = tools
                .iter()
                .find(|tool| tool["name"] == name)
                .expect("tool");
            assert_eq!(tool["inputSchema"]["additionalProperties"], false);
        }
        let execute = tools
            .iter()
            .find(|tool| tool["name"] == "structured_command_execute")
            .expect("execute");
        assert_eq!(
            execute["inputSchema"]["oneOf"].as_array().map(Vec::len),
            Some(3)
        );
        let start = tools
            .iter()
            .find(|tool| tool["name"] == "structured_command_start")
            .expect("start");
        assert_eq!(
            start["inputSchema"]["oneOf"].as_array().map(Vec::len),
            Some(2)
        );
    }

    #[test]
    fn execution_selector_refuses_ambiguous_calls() {
        let root = env::temp_dir();
        let state = State {
            allowed_roots: vec![root.clone()],
            allowed_commands: vec![],
            allowed_prefixes: vec![],
            blocked_commands: vec![],
            max_timeout_ms: 10_000,
            max_output_bytes: 4096,
            audit_log_dir: None,
            site_root: root.clone(),
            storage_root: root,
            env: env::vars().collect(),
        };
        let error = execute(
            &state,
            &json!({"command":"cargo","input_ref":"structured_command_input:fixture01"}),
            None,
            false,
        )
        .expect_err("ambiguous selector");
        assert_eq!(error.code, "structured_command_execution_selector_invalid");
        let error = execute(
            &state,
            &json!({"execution_ref":"structured_command_execution:fixture01"}),
            None,
            true,
        )
        .expect_err("start cannot read");
        assert_eq!(error.code, "structured_command_execution_selector_invalid");
    }

    #[test]
    fn background_runner_completes_durable_execution_record() {
        let root = env::temp_dir().join(format!(
            "narada-structured-command-background-{}",
            unique_id("test")
        ));
        fs::create_dir_all(&root).expect("root");
        let state = State {
            allowed_roots: vec![root.clone()],
            allowed_commands: vec![],
            allowed_prefixes: vec![],
            blocked_commands: vec![],
            max_timeout_ms: 10_000,
            max_output_bytes: 4096,
            audit_log_dir: None,
            site_root: root.clone(),
            storage_root: root.clone(),
            env: env::vars().collect(),
        };
        let pending = json!({"schema":"narada.structured_command.execution_result.v0","status":"running","executed":true,"pending":true,"stdout":"","stderr":""});
        let execution_ref = create_execution_record(&state, &pending).expect("execution");
        let request_path = root.join("background-request.json");
        let request = json!({
            "schema":"narada.structured_command.background_request.v1","execution_ref":execution_ref,
            "command":"cargo","args":["--version"],"working_directory":root.to_string_lossy(),
            "timeout_ms":10_000,"max_output_bytes":4096,"storage_root":root.to_string_lossy(),
            "audit_log_dir":Value::Null,"started_at":now_rfc3339(),
            "execution_posture":{"test_scope":"focused","expected_cost":"low"},"input_ref":Value::Null,
        });
        write_json_record(&request_path, &request).expect("request");
        let digest = hex::encode(Sha256::digest(fs::read(&request_path).expect("bytes")));
        run_background(&[
            "--request".into(),
            request_path.to_string_lossy().to_string(),
            "--sha256".into(),
            digest,
        ])
        .expect("background runner");
        let completed = read_execution_record(&state, &execution_ref).expect("completed");
        assert_eq!(completed["status"], "ok");
        assert_eq!(completed["pending"], false);
        assert!(completed["stdout"]
            .as_str()
            .is_some_and(|stdout| stdout.starts_with("cargo ")));
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn pnpm_corepack_shim_resolves_without_shell_or_terminal() {
        let root = env::temp_dir().join(format!(
            "narada-structured-command-resolver-{}",
            std::process::id()
        ));
        let entrypoint = root.join("node_modules/corepack/dist/pnpm.js");
        fs::create_dir_all(entrypoint.parent().unwrap()).unwrap();
        fs::write(root.join("node.exe"), b"fixture").unwrap();
        fs::write(&entrypoint, b"fixture").unwrap();
        let path = env::join_paths([&root])
            .unwrap()
            .to_string_lossy()
            .to_string();
        let requested = vec!["exec".to_string(), "cargo".to_string()];

        let (executable, arguments) =
            resolve_corepack_pnpm("pnpm", &path, &requested).expect("direct Corepack launch");

        assert_eq!(executable, root.join("node.exe"));
        assert_eq!(arguments[0], entrypoint.to_string_lossy());
        assert_eq!(&arguments[1..], requested);
        assert!(!executable
            .to_string_lossy()
            .to_ascii_lowercase()
            .contains("wt.exe"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn pnpm_must_not_wrap_cargo() {
        assert!(wraps_cargo_with_pnpm(
            "pnpm",
            &["exec".to_string(), "cargo".to_string(), "check".to_string()]
        ));
        assert!(!wraps_cargo_with_pnpm("cargo", &["check".to_string()]));
    }
