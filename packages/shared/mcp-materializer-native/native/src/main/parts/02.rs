fn run() -> Result<(), Failure> {
    let mut args = env::args_os().skip(1);
    let mut command = args
        .next()
        .and_then(|value| value.into_string().ok())
        .ok_or_else(|| {
            Failure::new(
                "materializer_command_required",
                "Expected `materialize-all --input <path>`.",
            )
        })?;
    let current_executable = env::current_exe().ok();
    if current_executable
        .as_ref()
        .is_some_and(|path| path_eq(path, Path::new(&command)))
    {
        command = args
            .next()
            .and_then(|value| value.into_string().ok())
            .ok_or_else(|| {
                Failure::new(
                    "materializer_command_required",
                    "Expected a command after the compatibility entrypoint.",
                )
            })?;
    }
    if command == "--materialize-all" {
        if args.next().is_some() {
            return Err(Failure::new(
                "materializer_argument_unknown",
                "Unexpected trailing argument.",
            ));
        }
        let user_profile = env::var_os("USERPROFILE").ok_or_else(|| {
            Failure::new(
                "materializer_user_profile_required",
                "USERPROFILE is required for installed-carrier recovery.",
            )
        })?;
        let index = PathBuf::from(user_profile).join(".narada/carriers/installed-carriers.json");
        let input = derive::derive_input(derive::options_from_installed_index(&index)?)?;
        let result = materialize(input, true)?;
        println!("{result}");
        return Ok(());
    }
    if command == "publish" {
        let flag = args.next().and_then(|value| value.into_string().ok());
        if flag.as_deref() != Some("--artifact-root") {
            return Err(Failure::new(
                "materializer_artifact_root_required",
                "Expected publish --artifact-root <path>.",
            ));
        }
        let artifact_root = args.next().map(PathBuf::from).ok_or_else(|| {
            Failure::new(
                "materializer_artifact_root_required",
                "Expected an artifact root.",
            )
        })?;
        if args.next().is_some() {
            return Err(Failure::new(
                "materializer_argument_unknown",
                "Unexpected trailing argument.",
            ));
        }
        println!("{}", publish_self(&artifact_root)?);
        return Ok(());
    }
    if matches!(
        command.as_str(),
        "contract-describe"
            | "contract-fingerprint-generation"
            | "contract-merge-codex"
            | "contract-format-json"
    ) {
        let flag = args.next().and_then(|value| value.into_string().ok());
        if flag.as_deref() != Some("--input") {
            return Err(Failure::new(
                "materializer_contract_input_required",
                format!("Expected {command} --input <path>."),
            ));
        }
        let input_path = args.next().map(PathBuf::from).ok_or_else(|| {
            Failure::new(
                "materializer_contract_input_required",
                "Expected an input path.",
            )
        })?;
        if args.next().is_some() {
            return Err(Failure::new(
                "materializer_argument_unknown",
                "Unexpected trailing argument.",
            ));
        }
        if command == "contract-describe" {
            let input: ContractDescribeInput =
                serde_json::from_slice(&fs::read(&input_path).map_err(|error| {
                    Failure::new("materializer_contract_input_read_failed", error.to_string())
                })?)
                .map_err(|error| {
                    Failure::new("materializer_contract_input_invalid", error.to_string())
                })?;
            let content = fs::read(&input.config_path).map_err(|error| {
                Failure::new("materializer_config_read_failed", error.to_string())
            })?;
            let selectors = if input.carrier_kind == "codex" && input.selectors.is_empty() {
                codex_managed_selectors(&input.server_ids, &input.plugin_ids, &input.project_paths)
            } else {
                input.selectors
            };
            let description = describe_config(&input.carrier_kind, &content, &selectors)
                .map_err(|error| Failure::new("materializer_contract_describe_failed", error))?;
            println!(
                "{}",
                serde_json::to_value(description).map_err(json_failure)?
            );
        } else if command == "contract-fingerprint-generation" {
            let generation = read_json(&input_path, "materializer_contract_input_invalid")?;
            println!(
                "{}",
                json!({"generation_fingerprint": generation_fingerprint(&generation).map_err(|error| Failure::new("materializer_generation_fingerprint_failed", error))?})
            );
        } else if command == "contract-merge-codex" {
            let input: ContractMergeCodexInput =
                serde_json::from_slice(&fs::read(&input_path).map_err(|error| {
                    Failure::new("materializer_contract_input_read_failed", error.to_string())
                })?)
                .map_err(|error| {
                    Failure::new("materializer_contract_input_invalid", error.to_string())
                })?;
            let desired = fs::read(&input.desired_path).map_err(|error| {
                Failure::new("materializer_config_read_failed", error.to_string())
            })?;
            let existing = input
                .existing_path
                .as_ref()
                .map(|path| {
                    fs::read(path).map_err(|error| {
                        Failure::new("materializer_config_read_failed", error.to_string())
                    })
                })
                .transpose()?;
            let selectors =
                codex_managed_selectors(&input.server_ids, &input.plugin_ids, &input.project_paths);
            let merged = merge_codex_configuration(
                existing.as_deref(),
                &desired,
                &input.previous_selectors,
                &selectors,
            )
            .map_err(|error| Failure::new("materializer_codex_merge_failed", error))?;
            fs::write(&input.output_path, merged).map_err(|error| {
                Failure::new(
                    "materializer_contract_output_write_failed",
                    error.to_string(),
                )
            })?;
            println!("{}", json!({"status":"merged","selectors":selectors}));
        } else {
            let input: ContractFormatJsonInput =
                serde_json::from_slice(&fs::read(&input_path).map_err(|error| {
                    Failure::new("materializer_contract_input_read_failed", error.to_string())
                })?)
                .map_err(|error| {
                    Failure::new("materializer_contract_input_invalid", error.to_string())
                })?;
            let value = read_json(&input.source_path, "materializer_json_source_invalid")?;
            let mut output = pretty_json(&value)?;
            if let Some(header) = input.header {
                let mut prefix = header
                    .trim_end_matches(['\r', '\n'])
                    .replace("\r\n", "\n")
                    .replace('\r', "\n")
                    .into_bytes();
                prefix.push(b'\n');
                prefix.extend(output);
                output = prefix;
            }
            fs::write(&input.output_path, output).map_err(|error| {
                Failure::new(
                    "materializer_contract_output_write_failed",
                    error.to_string(),
                )
            })?;
            println!("{}", json!({"status":"formatted"}));
        }
        return Ok(());
    }
    if matches!(command.as_str(), "materialize-site" | "promote-site") {
        let require_fresh_validation = command == "promote-site";
        let options = derive::DeriveOptions::parse(args)?;
        let result = materialize(derive::derive_input(options)?, require_fresh_validation)?;
        println!("{result}");
        return Ok(());
    }
    if command == "recover-generation" {
        let flag = args.next().and_then(|value| value.into_string().ok());
        if flag.as_deref() != Some("--generation") {
            return Err(Failure::new(
                "materializer_generation_required",
                "Expected recover-generation --generation <path>.",
            ));
        }
        let generation = args.next().map(PathBuf::from).ok_or_else(|| {
            Failure::new(
                "materializer_generation_required",
                "Expected a generation path.",
            )
        })?;
        if args.next().is_some() {
            return Err(Failure::new(
                "materializer_argument_unknown",
                "Unexpected trailing argument.",
            ));
        }
        let options = derive::options_from_generation(&generation)?;
        let input = derive::derive_input(options)?;
        let installed_index = input.installed_carrier_index_path.clone();
        let workspace_root = input.workspace_root.clone();
        let carrier_ids = input
            .carriers
            .iter()
            .map(|carrier| carrier.carrier_id.clone())
            .collect::<Vec<_>>();
        let materialization = materialize(input, true)?;
        let verification = verify_all(&installed_index)?;
        let recovered_at = OffsetDateTime::now_utc()
            .format(&Rfc3339)
            .map_err(|error| Failure::new("materializer_clock_failed", error.to_string()))?;
        let evidence_unsigned = json!({
            "schema": "narada.mcp_materializer.recovery_evidence.v1",
            "status": "recovered",
            "recovered_at": recovered_at,
            "trigger_generation_path": path_text(&generation),
            "materialization": materialization,
            "verification": verification,
        });
        let evidence_fingerprint =
            sha256(&serde_json::to_vec(&evidence_unsigned).map_err(json_failure)?);
        let evidence_ref = format!("sha256:{evidence_fingerprint}");
        let mut evidence = evidence_unsigned;
        evidence
            .as_object_mut()
            .expect("recovery evidence is an object")
            .insert("ref".to_string(), Value::String(evidence_ref.clone()));
        let recovery_root = workspace_root.join(".ai/runtime/carrier-materialization-recovery");
        let evidence_path = recovery_root.join("latest-materialization.json");
        let pressure_path = workspace_root.join(".ai/runtime/carrier-restart-pressure.json");
        let pressure_carriers = carrier_ids
            .iter()
            .map(|carrier_id| {
                (
                    carrier_id.clone(),
                    json!({
                        "carrier_id": carrier_id,
                        "materialized_at": recovered_at,
                        "evidence_ref": evidence_ref,
                    }),
                )
            })
            .collect::<Map<String, Value>>();
        let pressure = json!({
            "schema": "narada.carrier_restart_pressure.v1",
            "updated_at": recovered_at,
            "carriers": pressure_carriers,
        });
        transactional_publish(&[
            Publication {
                path: evidence_path.clone(),
                content: pretty_json(&evidence)?,
            },
            Publication {
                path: pressure_path.clone(),
                content: pretty_json(&pressure)?,
            },
        ])?;
        println!(
            "{}",
            json!({
                "schema": evidence.get("schema"),
                "status": evidence.get("status"),
                "ref": evidence_ref,
                "recovered_at": recovered_at,
                "trigger_generation_path": path_text(&generation),
                "materialization": evidence.get("materialization"),
                "verification": evidence.get("verification"),
                "evidence_path": path_text(&evidence_path),
                "restart_pressure_path": path_text(&pressure_path),
                "restart_pressure": pressure.get("carriers"),
            })
        );
        return Ok(());
    }
    if command == "verify-all" {
        let flag = args.next().and_then(|value| value.into_string().ok());
        if flag.as_deref() != Some("--installed-index") {
            return Err(Failure::new(
                "materializer_installed_index_required",
                "Expected verify-all --installed-index <path>.",
            ));
        }
        let index = args.next().map(PathBuf::from).ok_or_else(|| {
            Failure::new(
                "materializer_installed_index_required",
                "Expected an installed carrier index path.",
            )
        })?;
        if args.next().is_some() {
            return Err(Failure::new(
                "materializer_argument_unknown",
                "Unexpected trailing argument.",
            ));
        }
        println!("{}", verify_all(&index)?);
        return Ok(());
    }
    if command == "acknowledge-restart" {
        let mut values = BTreeMap::<String, String>::new();
        while let Some(flag) = args.next() {
            let flag = flag.into_string().map_err(|_| {
                Failure::new("materializer_argument_invalid", "Argument is not UTF-8.")
            })?;
            if !matches!(
                flag.as_str(),
                "--installed-index" | "--carrier-id" | "--expected-evidence-ref"
            ) {
                return Err(Failure::new(
                    "materializer_argument_unknown",
                    format!("Unknown argument: {flag}"),
                ));
            }
            let value = args
                .next()
                .and_then(|value| value.into_string().ok())
                .ok_or_else(|| {
                    Failure::new(
                        "materializer_argument_value_required",
                        format!("{flag} requires a value."),
                    )
                })?;
            if values.insert(flag.clone(), value).is_some() {
                return Err(Failure::new(
                    "materializer_argument_duplicate",
                    format!("Duplicate argument: {flag}"),
                ));
            }
        }
        let required = |flag: &str| {
            values.get(flag).cloned().ok_or_else(|| {
                Failure::new("materializer_argument_required", format!("Missing {flag}."))
            })
        };
        let result = acknowledge_restart(
            Path::new(&required("--installed-index")?),
            &required("--carrier-id")?,
            &required("--expected-evidence-ref")?,
        )?;
        println!("{result}");
        if result.get("status").and_then(Value::as_str) == Some("stale_ack_refused") {
            std::process::exit(2);
        }
        return Ok(());
    }
    if command != "materialize-all" {
        return Err(Failure::new(
            "materializer_command_unknown",
            format!("Unknown command: {command}"),
        ));
    }
    let flag = args.next().and_then(|value| value.into_string().ok());
    if flag.as_deref() != Some("--input") {
        return Err(Failure::new(
            "materializer_input_required",
            "Expected --input <path>.",
        ));
    }
    let input_path = args
        .next()
        .map(PathBuf::from)
        .ok_or_else(|| Failure::new("materializer_input_required", "Expected --input <path>."))?;
    if args.next().is_some() {
        return Err(Failure::new(
            "materializer_argument_unknown",
            "Unexpected trailing argument.",
        ));
    }
    let raw = fs::read(&input_path).map_err(|error| {
        Failure::new("materializer_input_read_failed", error.to_string())
            .with_details(json!({"input_path": path_text(&input_path)}))
    })?;
    let input: MaterializationInput = serde_json::from_slice(&raw).map_err(|error| {
        Failure::new("materializer_input_invalid", error.to_string())
            .with_details(json!({"input_path": path_text(&input_path)}))
    })?;
    let result = materialize(input, false)?;
    println!("{result}");
    Ok(())
}

