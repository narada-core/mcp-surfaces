    #[test]
    fn project_site_falls_back_to_user_site_intelligence_context() {
        let base =
            std::env::temp_dir().join(format!("narada-worker-context-{}", uuid::Uuid::new_v4()));
        let project = base.join("src/marici");
        let user_site = base.join("Narada");
        let expected = user_site.join(".narada/intelligence-launch-context.json");
        fs::create_dir_all(expected.parent().expect("context parent")).expect("context dir");
        fs::write(&expected, "{}").expect("context");
        assert_eq!(
            resolve_intelligence_context_path(&project, None, None, Some(base.clone())),
            expected
        );
        fs::remove_dir_all(base).expect("cleanup");
    }

    #[test]
    fn project_site_discovers_sibling_narada_source_root() {
        let base =
            std::env::temp_dir().join(format!("narada-worker-source-{}", uuid::Uuid::new_v4()));
        let source_root = base.join("src");
        let project = source_root.join("marici");
        fs::create_dir_all(source_root.join("narada")).expect("narada source dir");
        fs::create_dir_all(&project).expect("project dir");
        assert_eq!(narada_source_root(&project), source_root);
        fs::remove_dir_all(base).expect("cleanup");
    }

    #[cfg(windows)]
    #[test]
    fn containment_ignores_windows_path_case() {
        assert!(path_components_equal_or_child(
            Path::new("c:/users/andrey/narada/project"),
            Path::new("C:/Users/Andrey/Narada")
        ));
    }

    #[test]
    #[ignore = "requires an explicit deployed Site root and native preflight binary"]
    fn live_native_preflight_resolves_without_caller_plan() {
        let site_root = std::env::var("NARADA_TEST_SITE_ROOT").expect("NARADA_TEST_SITE_ROOT");
        for (cognition, expected_model, expected_effort) in [
            ("low", "gpt-5.6-luna", "max"),
            ("medium", "gpt-5.6-sol", "low"),
            ("high", "gpt-5.6-sol", "max"),
        ] {
            let (plan_ref, provider_mode, model, evidence_ref, reasoning_effort, provider_binding) =
                invocation_plan_binding(Path::new(&site_root), None, Some(cognition))
                    .expect("native preflight");
            assert!(plan_ref.starts_with("plan:cognition:"));
            assert_eq!(provider_mode, "codex-subscription");
            assert_eq!(model, expected_model);
            assert!(evidence_ref.starts_with("preflight-evidence:"));
            assert_eq!(reasoning_effort.as_deref(), Some(expected_effort));
            assert!(provider_binding.is_none() || provider_binding.as_ref().is_some_and(|value| value["schema"] == "narada.native.provider_binding.v1"));
        }
    }

    #[test]
    fn native_worker_refuses_admitted_plan_without_model() {
        let admission = json!({"status":"admitted","plan_ref":"plan:test","selected":{"inference_provider":{"id":"inference-provider:codex-subscription"},"model":null},"evidence_ref":"preflight-evidence:test"});
        let refusal = admitted_plan_binding(&admission).expect_err("missing model must refuse");
        assert_eq!(refusal["code"], "worker_canonical_invocation_model_missing");
    }

    #[test]
    fn native_worker_requires_valid_http_binding_for_api_provider() {
        let admission = json!({
            "status":"admitted",
            "plan_ref":"plan:test",
            "selected":{
                "inference_provider":{"id":"inference-provider:deepseek-api"},
                "model":{"id":"model:deepseek-v4-flash"}
            },
            "evidence_ref":"preflight-evidence:test"
        });
        let refusal = admitted_plan_binding(&admission).expect_err("binding must be required");
        assert_eq!(refusal["code"], "worker_native_provider_binding_missing");

        let mut valid = admission.clone();
        valid["provider_binding"] = json!({
            "schema":"narada.native.provider_binding.v1",
            "provider":"deepseek-api",
            "protocol":"openai/chat-completions/1",
            "endpoint":"https://api.deepseek.com/v1/chat/completions",
            "model":"deepseek-v4-flash",
            "credential_secret_ref":"narada/provider/deepseek-api/api-key"
        });
        assert!(admitted_plan_binding(&valid).is_ok());

        let mut env_binding = valid;
        env_binding["provider_binding"] = json!({
            "schema":"narada.native.provider_binding.v1",
            "provider":"deepseek-api",
            "protocol":"openai/chat-completions/1",
            "endpoint":"https://api.deepseek.com/v1/chat/completions",
            "model":"deepseek-v4-flash",
            "credential_env":"DEEPSEEK_API_KEY"
        });
        let refusal = admitted_plan_binding(&env_binding).expect_err("env binding must refuse");
        assert_eq!(refusal["code"], "worker_native_provider_binding_invalid");
    }

    #[test]
    fn capability_snapshot_reports_effective_write_posture() {
        let cwd = PathBuf::from("C:/workspace/repo");
        let probe = json!({"status":"passed"});
        let snapshot = capability_snapshot("write", &cwd, std::slice::from_ref(&cwd), Some(&probe));
        assert_eq!(snapshot["filesystem"]["write"], true);
        assert_eq!(snapshot["effective_mode"], "workspace_write");
        assert_eq!(snapshot["validated_against_runtime"], true);
        assert_eq!(snapshot["approval"]["mode"], "automatic_contained_review");
        assert_eq!(snapshot["tool_bridge"]["kind"], "codex_builtin_repo_tools");
        assert_eq!(
            snapshot["tool_bridge"]["ordinary_file_mutation_tool"],
            "bounded_powershell_cmdlets"
        );
        assert_eq!(snapshot["tool_bridge"]["apply_patch_available"], false);
        assert_eq!(
            snapshot["workflow_primitives"]["text_file_lifecycle"]["windows_recipe"],
            "use one literal-path PowerShell cmdlet invocation per operation: Set-Content -Encoding utf8, Get-Content -Encoding utf8, Remove-Item, then Test-Path; do not use utf8NoBOM or .NET method invocation under ConstrainedLanguage"
        );
        assert_eq!(snapshot["write_roots"], json!(["C:/workspace/repo"]));
    }

    #[test]
    fn command_authority_does_not_escalate_to_write() {
        let cwd = PathBuf::from("C:/workspace/repo");
        let admission_root = PathBuf::from("C:/workspace");
        let snapshot = capability_snapshot("command", &cwd, &[admission_root], None);
        assert_eq!(snapshot["effective_mode"], "read_only");
        assert_eq!(snapshot["filesystem"]["write"], false);
        assert_eq!(snapshot["commands"]["execute"], true);
        assert_eq!(snapshot["commands"]["write_effects"], false);
        assert_eq!(snapshot["write_roots"], json!([]));
    }

    #[test]
    fn native_worker_reads_bounded_run_records() {
        let root = std::env::temp_dir().join(format!("narada-worker-{}", uuid::Uuid::new_v4()));
        let dir = run_root(&root).join("run-2026-01-01T00-00-00Z");
        fs::create_dir_all(&dir).expect("dir");
        fs::write(
            dir.join("result.json"),
            r#"{"run_id":"run-2026-01-01T00-00-00Z","status":"completed","summary":"done"}"#,
        )
        .expect("record");
        let listed = runs_list(&json!({"limit":1}).as_object().unwrap(), &root).expect("list");
        assert_eq!(listed["count"], 1);
        assert_eq!(
            run_status(
                &json!({"run_id":"run-2026-01-01T00-00-00Z"})
                    .as_object()
                    .unwrap(),
                &root
            )
            .expect("status")["run"]["status"],
            "completed"
        );
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn native_worker_output_supports_bounded_paging() {
        let root =
            std::env::temp_dir().join(format!("narada-worker-output-{}", uuid::Uuid::new_v4()));
        let dir = run_root(&root).join("run-2026-01-01T00-00-00Z");
        fs::create_dir_all(&dir).expect("dir");
        fs::write(dir.join("worker_prompt.txt"), "0123456789").expect("artifact");
        let page = output_show(&json!({"ref":"worker-artifact:run-2026-01-01T00-00-00Z/worker_prompt.txt","offset":3,"limit":4}).as_object().unwrap(), &root).expect("page");
        assert_eq!(page["output_text"], "3456");
        assert_eq!(page["next_offset"], 7);
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn native_worker_dashboard_respects_mode_and_terminal_filter() {
        let root =
            std::env::temp_dir().join(format!("narada-worker-dashboard-{}", uuid::Uuid::new_v4()));
        let completed = run_root(&root).join("run-2026-01-01T00-00-00Z");
        let running = run_root(&root).join("run-2026-01-01T00-00-01Z");
        fs::create_dir_all(&completed).expect("completed dir");
        fs::create_dir_all(&running).expect("running dir");
        fs::write(
            completed.join("result.json"),
            r#"{"run_id":"run-2026-01-01T00-00-00Z","status":"completed","summary":"done"}"#,
        )
        .expect("completed record");
        fs::write(
            running.join("result.json"),
            r#"{"run_id":"run-2026-01-01T00-00-01Z","status":"running","summary":"active"}"#,
        )
        .expect("running record");
        let selected = dashboard(
            &json!({"mode":"single_run","run_id":"run-2026-01-01T00-00-00Z"})
                .as_object()
                .unwrap(),
            &root,
        )
        .expect("single dashboard");
        assert_eq!(selected["mode"], "single_run");
        assert_eq!(selected["counts"]["total"], 1);
        assert_eq!(selected["runs"][0]["run_id"], "run-2026-01-01T00-00-00Z");
        let active = dashboard(
            &json!({"mode":"all_active","include_terminal":false})
                .as_object()
                .unwrap(),
            &root,
        )
        .expect("active dashboard");
        assert_eq!(active["mode"], "all_active");
        assert_eq!(active["counts"]["active"], 1);
        assert_eq!(active["runs"][0]["run_id"], "run-2026-01-01T00-00-01Z");
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn native_worker_updates_cognition_defaults_atomically() {
        let root =
            std::env::temp_dir().join(format!("narada-worker-defaults-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(root.join(".narada")).expect("site root");
        fs::write(
            root.join(".narada/provider-registry.json"),
            serde_json::to_vec(&json!({
                "schema":"narada.carrier.provider_registry.v1",
                "providers":{"fixture":{"available_models":["fixture-model"]}}
            }))
            .expect("registry"),
        )
        .expect("registry write");
        let updated = cognition_defaults_update(json!({"provider":"fixture","cognition":"high","model":"fixture-model","reasoning_effort":"max","actor":"test"}).as_object().unwrap(), &root).expect("update");
        assert_eq!(updated["status"], "updated");
        assert_eq!(
            cognition_defaults(&root)["defaults"]["high"]["model"],
            "fixture-model"
        );
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn native_worker_rejects_provider_and_model_outside_registry() {
        let root =
            std::env::temp_dir().join(format!("narada-worker-defaults-reject-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(root.join(".narada")).expect("site root");
        fs::write(
            root.join(".narada/provider-registry.json"),
            br#"{"schema":"narada.carrier.provider_registry.v1","providers":{"fixture":{"available_models":["fixture-model"]}}}"#,
        )
        .expect("registry write");
        let unknown_provider = cognition_defaults_update(
            json!({"provider":"unknown","cognition":"low","model":"fixture-model","reasoning_effort":"low"})
                .as_object()
                .unwrap(),
            &root,
        )
        .expect_err("unknown provider must be refused");
        assert_eq!(unknown_provider["code"], "worker_cognition_provider_not_allowed");
        let unknown_model = cognition_defaults_update(
            json!({"provider":"fixture","cognition":"low","model":"unknown-model","reasoning_effort":"low"})
                .as_object()
                .unwrap(),
            &root,
        )
        .expect_err("unknown model must be refused");
        assert_eq!(unknown_model["code"], "worker_cognition_model_not_allowed");
        fs::remove_dir_all(root).expect("cleanup");
    }

