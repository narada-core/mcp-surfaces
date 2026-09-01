fn parse_options(args: Vec<String>) -> Result<Options, String> {
    let mut surface_id = None;
    let mut site_root = None;
    let mut log_root = None;
    let mut registry_path = None;
    let mut native_authority = false;
    let mut allowed_roots = Vec::new();
    let mut environment = Vec::new();
    let mut index = 0;
    while index < args.len() {
        let key = args[index].as_str();
        if key == "--native-authority" {
            native_authority = true;
            index += 1;
            continue;
        }
        let value = args
            .get(index + 1)
            .ok_or_else(|| format!("native_surface_argument_value_required:{key}"))?;
        match key {
            "--surface-id" => surface_id = Some(value.clone()),
            "--site-root" => site_root = Some(PathBuf::from(value)),
            "--narada-root" => site_root = Some(PathBuf::from(value)),
            "--feedback-root" | "--output-root" | "--repo-root" | "--sop-root" => {
                site_root = Some(PathBuf::from(value));
            }
            "--user-site-root" => {
                site_root = Some(PathBuf::from(value));
                environment.push(("NARADA_USER_SITE_ROOT".to_string(), value.clone()));
            }
            "--task-root" => {
                if site_root.is_none() {
                    site_root = Some(PathBuf::from(value));
                }
                environment.push(("NARADA_DELEGATED_TASK_ROOT".to_string(), value.clone()));
            }
            "--allowed-root" => {
                allowed_roots.push(PathBuf::from(value));
                if site_root.is_none() {
                    site_root = Some(PathBuf::from(value));
                }
            }
            "--log-root" => log_root = Some(PathBuf::from(value)),
            "--registry-path" => registry_path = Some(PathBuf::from(value)),
            "--projection-id" => {
                let _ = value;
            }
            "--canonical-feedback-root" => {
                environment.push(("NARADA_SURFACE_FEEDBACK_ROOT".to_string(), value.clone()))
            }
            "--task-lifecycle-root" => {
                environment.push(("NARADA_TASK_LIFECYCLE_ROOT".to_string(), value.clone()))
            }
            "--site-id" => environment.push(("NARADA_SITE_ID".to_string(), value.clone())),
            "--owned-surface-id" => {
                if let Some((_, owned)) = environment
                    .iter_mut()
                    .find(|(candidate, _)| candidate == "NARADA_OWNED_SURFACE_IDS")
                {
                    if !owned.is_empty() {
                        owned.push(',');
                    }
                    owned.push_str(value);
                } else {
                    environment.push(("NARADA_OWNED_SURFACE_IDS".to_string(), value.clone()));
                }
            }
            "--feedback-discovery-root" => {
                if let Some((_, roots)) = environment
                    .iter_mut()
                    .find(|(candidate, _)| candidate == "NARADA_FEEDBACK_DISCOVERY_ROOTS")
                {
                    if !roots.is_empty() {
                        roots.push(';');
                    }
                    roots.push_str(value);
                } else {
                    environment
                        .push(("NARADA_FEEDBACK_DISCOVERY_ROOTS".to_string(), value.clone()));
                }
            }
            "--projection" => {
                environment.push(("NARADA_NARS_SESSION_PROJECTION".to_string(), value.clone()))
            }
            "--source-kind" => {
                environment.push(("NARADA_NARS_SESSION_SOURCE_KIND".to_string(), value.clone()))
            }
            "--operator-id" => environment.push(("NARADA_OPERATOR_ID".to_string(), value.clone())),
            "--run-root" => environment.push(("NARADA_WORKER_RUN_ROOT".to_string(), value.clone())),
            "--sops-dir" => environment.push(("NARADA_SOPS_DIR".to_string(), value.clone())),
            "--provider-registry-path" => environment.push((
                "NARADA_SPEECH_PROVIDER_REGISTRY_PATH".to_string(),
                value.clone(),
            )),
            "--server-name" => {
                environment.push(("NARADA_MCP_SERVER_NAME".to_string(), value.clone()))
            }
            _ => return Err(format!("native_surface_unknown_argument:{key}")),
        }
        index += 2;
    }
    let surface_id = surface_id.ok_or("native_surface_missing_surface_id")?;
    let site_root =
        site_root.unwrap_or_else(|| env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    if surface_id != "worker-delegation"
        && !allowed_roots.iter().any(|root| root == &site_root)
    {
        allowed_roots.push(site_root.clone());
    }
    Ok(Options {
        surface_id,
        site_root,
        allowed_roots,
        log_root,
        registry_path,
        native_authority,
        environment,
    })
}

