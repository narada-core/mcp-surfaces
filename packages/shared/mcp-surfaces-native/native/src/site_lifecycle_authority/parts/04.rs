pub(crate) fn init_site(args: &Map<String, Value>) -> Result<Value, Value> {
    let site_id = required(args, "site_id")?;
    if site_id.contains(['\\', '/']) {
        return Err(error(
            "site_id_invalid",
            "site_id must be an identifier, not a path",
        ));
    }
    let substrate = required(args, "substrate")?;
    if ![
        "windows-native",
        "windows-wsl",
        "macos",
        "linux-user",
        "linux-system",
    ]
    .contains(&substrate.as_str())
    {
        return Ok(
            json!({"status":"error","error":format!("Unsupported substrate: \"{substrate}\". Valid substrates: windows-native, windows-wsl, macos, linux-user, linux-system"),"remediation":"Choose a supported substrate."}),
        );
    }
    let execute = args.get("execute").and_then(Value::as_bool) == Some(true);
    let dry = args
        .get("dry_run")
        .and_then(Value::as_bool)
        .unwrap_or(!execute);
    if !execute || dry {
        return Ok(init_plan(&site_id, &substrate, args, true)?);
    }
    if args
        .get("authority_basis")
        .and_then(Value::as_object)
        .is_none_or(Map::is_empty)
    {
        return Err(error(
            "authority_basis_required",
            "site_init requires a non-empty authority_basis",
        ));
    }
    let plan = init_plan(&site_id, &substrate, args, false)?;
    let root = PathBuf::from(plan["siteRoot"].as_str().unwrap_or_default());
    let config_path = root.join("config.json");
    if config_path.is_file() {
        let existing =
            read_bounded_json(&config_path, 4 * 1024 * 1024, "site_init_existing_config")?;
        if existing != plan["config"] {
            return Err(
                json!({"code":"site_init_conflict","message":"target Site already has a different config","path":config_path.to_string_lossy()}),
            );
        }
        let repaired = register_site(
            &site_id,
            &substrate,
            &root,
            args.get("operation").and_then(Value::as_str),
        )?;
        let mut replay = plan;
        replay["status"] = Value::String(
            if repaired {
                "repaired_registry"
            } else {
                "reused"
            }
            .to_string(),
        );
        replay["dryRun"] = Value::Bool(false);
        replay["mutation_performed"] = Value::Bool(repaired);
        replay["idempotency_replay"] = Value::Bool(!repaired);
        return Ok(replay);
    }
    ensure_registry_compatible(&site_id, &substrate, &root)?;
    if root.exists()
        && fs::read_dir(&root)
            .map_err(io_error("site_init_target_read_failed"))?
            .take(1)
            .next()
            .is_some()
    {
        return Err(
            json!({"code":"site_init_collision_refused","message":"site_init refuses a non-empty target without an identical config","path":root.to_string_lossy()}),
        );
    }
    for directory in [
        "state",
        "messages",
        "tombstones",
        "views",
        "blobs",
        "tmp",
        "db",
        "logs",
        "traces",
        ".ai",
    ] {
        fs::create_dir_all(root.join(directory))
            .map_err(io_error("site_init_directory_create_failed"))?;
    }
    write_new_json(&config_path, &plan["config"])?;
    write_new_text(
        &root.join("AGENTS.md"),
        &site_agents_contract(&site_id, &substrate, &root),
    )?;
    register_site(
        &site_id,
        &substrate,
        &root,
        args.get("operation").and_then(Value::as_str),
    )?;
    let mut result = plan;
    result["status"] = Value::String("success".to_string());
    result["dryRun"] = Value::Bool(false);
    result["mutation_performed"] = Value::Bool(true);
    result["idempotency_replay"] = Value::Bool(false);
    Ok(result)
}

fn init_plan(
    site_id: &str,
    substrate: &str,
    args: &Map<String, Value>,
    dry: bool,
) -> Result<Value, Value> {
    let authority = text(args, "authority_locus").unwrap_or_else(|| "user".to_string());
    if matches!(substrate, "windows-native" | "windows-wsl")
        && !["user", "pc"].contains(&authority.as_str())
    {
        return Err(error(
            "authority_locus_invalid",
            "Windows authority_locus must be user or pc",
        ));
    }
    let sync = text(args, "sync")
        .or_else(|| (authority == "user").then_some("hybrid_capable_plain_folder".to_string()));
    if sync.as_deref().is_some_and(|value| {
        ![
            "local_only",
            "cloud_synced_folder",
            "git_backed",
            "hybrid",
            "hybrid_capable_plain_folder",
        ]
        .contains(&value)
    }) {
        return Err(error(
            "sync_posture_invalid",
            "unsupported Site sync posture",
        ));
    }
    let root = text(args, "root")
        .map(PathBuf::from)
        .unwrap_or_else(|| default_site_root(site_id, substrate, &authority));
    let execution = text(args, "execution_surface").unwrap_or_else(|| {
        match substrate {
            "windows-native" | "windows-wsl" => "windows_native",
            "linux-user" => "linux_user",
            "linux-system" => "linux_system",
            _ => "macos_native",
        }
        .to_string()
    });
    if ![
        "windows_native",
        "wsl_assisted",
        "wsl_native",
        "linux_user",
        "linux_system",
        "macos_native",
    ]
    .contains(&execution.as_str())
    {
        return Err(error(
            "execution_surface_invalid",
            "unsupported execution_surface",
        ));
    }
    let variant = match substrate {
        "windows-native" => "native",
        "windows-wsl" => "wsl",
        "linux-user" => "linux-user",
        "linux-system" => "linux-system",
        _ => "macos",
    };
    let config = json!({"site_id":site_id,"variant":variant,"substrate":substrate,"site_root":root.to_string_lossy(),"config_path":root.join("config.json").to_string_lossy(),"locus":{"authority_locus":authority},"sync":sync.as_ref().map(|posture|json!({"posture":posture,"git_initialized":false,"cloud_sync":"external_if_configured"})),"execution":{"surface":execution,"inferred":!args.contains_key("execution_surface"),"executor_runtime":if cfg!(windows){"windows"}else if cfg!(target_os="macos"){"macos"}else{"linux"},"target_authority_locus":if substrate.starts_with("windows-"){format!("windows_{authority}")}else{substrate.to_string()},"target_root":root.to_string_lossy(),"permission_posture":if authority=="pc"{"pc_locus_programdata_write_required"}else{"site_locus_write_required"}},"cycle_interval_minutes":5,"lock_ttl_ms":310000,"ceiling_ms":300000});
    Ok(
        json!({"status":if dry{"planned"}else{"initializing"},"siteId":site_id,"substrate":substrate,"siteRoot":root.to_string_lossy(),"configPath":root.join("config.json").to_string_lossy(),"dryRun":dry,"mutation_performed":false,"config":config,"planned_directories":["state","messages","tombstones","views","blobs","tmp","db","logs","traces",".ai"],"planned_files":[root.join("config.json").to_string_lossy(),root.join("AGENTS.md").to_string_lossy()],"nextSteps":[format!("narada doctor --site {site_id}"),format!("narada cycle --site {site_id}"),format!("narada sites enable {site_id}")]}),
    )
}
fn default_site_root(site_id: &str, substrate: &str, authority: &str) -> PathBuf {
    if let Some(root) = std::env::var_os("NARADA_SITE_ROOT") {
        return if authority == "user" && substrate.starts_with("windows-") {
            PathBuf::from(root)
        } else {
            PathBuf::from(root).join(site_id)
        };
    }
    match substrate {
        "windows-native" if authority == "user" => std::env::var_os("NARADA_USER_SITE_ROOT")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("USERPROFILE").map(|v| PathBuf::from(v).join("Narada")))
            .unwrap_or_else(|| PathBuf::from("Narada")),
        "windows-native" => std::env::var_os("NARADA_PC_SITE_ROOT")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                std::env::var_os("ProgramData")
                    .map(PathBuf::from)
                    .unwrap_or_else(|| PathBuf::from("C:/ProgramData"))
                    .join("Narada/sites/pc")
            })
            .join(site_id),
        "windows-wsl" if authority == "user" => home_dir().join(".narada"),
        "windows-wsl" => PathBuf::from("/var/lib/narada/sites/pc").join(site_id),
        "linux-system" => PathBuf::from("/var/lib/narada").join(site_id),
        "linux-user" => std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home_dir().join(".local/share"))
            .join("narada")
            .join(site_id),
        _ => home_dir()
            .join("Library/Application Support/Narada")
            .join(site_id),
    }
}
fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}
fn path_key(value: &str) -> String {
    value
        .replace('\\', "/")
        .trim_end_matches('/')
        .to_ascii_lowercase()
}
fn site_agents_contract(site_id: &str, substrate: &str, root: &Path) -> String {
    format!("# {site_id} Site Agent Contract\n\nThis is the Site-local execution contract for `{}`.\n\n- Authority is local to `{}`.\n- Architect specifies governed work; Builder executes admitted work; Observer reports without mutation.\n- Runtime presence does not grant Operator or Site authority.\n- Use canonical inbox, lifecycle, evidence, and publication surfaces.\n- Incoming material is inert until admitted by this Site.\n",substrate,root.display())
}
fn write_new_json(path: &Path, value: &Value) -> Result<(), Value> {
    write_new_text(
        path,
        &format!(
            "{}\n",
            serde_json::to_string_pretty(value)
                .map_err(|cause| error("site_init_config_encode_failed", &cause.to_string()))?
        ),
    )
}
fn write_new_text(path: &Path, content: &str) -> Result<(), Value> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(io_error("site_init_parent_create_failed"))?;
    }
    use std::io::Write;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(io_error("site_init_file_create_failed"))?;
    file.write_all(content.as_bytes())
        .map_err(io_error("site_init_file_write_failed"))?;
    file.sync_all()
        .map_err(io_error("site_init_file_sync_failed"))
}
fn read_bounded_json(path: &Path, max: u64, code: &'static str) -> Result<Value, Value> {
    if fs::metadata(path).map_err(io_error(code))?.len() > max {
        return Err(error(code, "JSON artifact exceeds its size bound"));
    }
    serde_json::from_slice(&fs::read(path).map_err(io_error(code))?)
        .map_err(|cause| error(code, &cause.to_string()))
}
