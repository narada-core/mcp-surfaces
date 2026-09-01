fn discover(args: &Map<String, Value>) -> Result<Value, Value> {
    let source = args.get("source").and_then(Value::as_str).unwrap_or("all");
    if !matches!(source, "filesystem" | "launch_registry" | "all") {
        return Err(error(
            "site_registry_source_invalid",
            "source must be filesystem, launch_registry, or all",
        ));
    }
    let actor = args
        .get("actor")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .unwrap_or("operator");
    let root_filter = args
        .get("root")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty());
    let path = registry_path();
    let db = ready_registry(&path)?;
    let existing = load_all_sites(&db)?;
    let mut candidates = Vec::new();
    let mut ignored = 0usize;
    if matches!(source, "filesystem" | "all") {
        let (found, skipped) = filesystem_candidates(root_filter)?;
        candidates.extend(found);
        ignored += skipped;
    }
    if matches!(source, "launch_registry" | "all") {
        let (found, skipped) = launch_candidates(root_filter)?;
        candidates.extend(found);
        ignored += skipped;
    }
    let mut by_root: BTreeMap<String, Value> = BTreeMap::new();
    for candidate in candidates {
        let key = path_key(candidate["site_root"].as_str().unwrap_or_default());
        by_root
            .entry(key)
            .and_modify(|current| merge_candidate(current, &candidate))
            .or_insert(candidate);
    }
    let mut entries = Vec::new();
    let mut added = Vec::new();
    let mut updated = Vec::new();
    let mut unchanged = Vec::new();
    let mut retired = 0usize;
    for candidate in by_root.into_values() {
        let root = candidate["site_root"].as_str().unwrap_or_default();
        let candidate_id = candidate["site_id"].as_str().unwrap_or_default();
        let matched = existing.iter().find(|site| {
            path_key(site["site_root"].as_str().unwrap_or_default()) == path_key(root)
                || site["site_id"]
                    .as_str()
                    .is_some_and(|id| id.eq_ignore_ascii_case(candidate_id))
        });
        let (status, operation, changes, refusals, after) = match matched {
            None => {
                added.push(candidate_id.to_string());
                (
                    "planned",
                    "add",
                    json!(["site_record"]),
                    json!([]),
                    candidate.clone(),
                )
            }
            Some(site) if site["lifecycle_status"] == "retired" => {
                retired += 1;
                (
                    "advisory",
                    "add",
                    json!([]),
                    json!(["retired_record_requires_restore_or_re_admit"]),
                    site.clone(),
                )
            }
            Some(site) => {
                let missing_alias = site["site_id"]
                    .as_str()
                    .is_some_and(|id| !id.eq_ignore_ascii_case(candidate_id))
                    && !candidate_id.is_empty();
                let missing_source = candidate["sources"]
                    .as_array()
                    .unwrap_or(&Vec::new())
                    .iter()
                    .any(|incoming| {
                        !site["sources"]
                            .as_array()
                            .unwrap_or(&Vec::new())
                            .iter()
                            .any(|stored| {
                                stored["kind"] == incoming["kind"]
                                    && stored["ref"] == incoming["ref"]
                            })
                    });
                if missing_alias || missing_source {
                    updated.push(site["site_id"].as_str().unwrap_or_default().to_string());
                    let mut merged = site.clone();
                    merge_candidate(&mut merged, &candidate);
                    (
                        "planned",
                        "edit",
                        json!(["discovery_evidence"]),
                        json!([]),
                        merged,
                    )
                } else {
                    unchanged.push(site["site_id"].as_str().unwrap_or_default().to_string());
                    ("unchanged", "add", json!([]), json!([]), site.clone())
                }
            }
        };
        entries.push(json!({"schema":"narada.site_registry.management.v0","status":status,"operation":operation,"mutation_performed":false,"site_id":matched.and_then(|v|v["site_id"].as_str()).unwrap_or(candidate_id),"registry_path":path.to_string_lossy(),"catalog_source":"user_site_site_registry","before":matched,"after":after,"changes":changes,"conflicts":[],"refusals":refusals,"audit_ref":null,"actor":actor}));
    }
    let status = if retired > 0 { "advisory" } else { "planned" };
    Ok(
        json!({"schema":"narada.site_registry.management.v0","status":status,"operation":"discover","mutation_performed":false,"dry_run":true,"registry_path":path.to_string_lossy(),"catalog_source":"user_site_site_registry","source":source,"root_filter":root_filter,"counts":{"added":added.len(),"updated":updated.len(),"unchanged":unchanged.len(),"ignored":ignored,"retired_source_present":retired,"conflicted":0},"added":added,"updated":updated,"unchanged":unchanged,"entries":entries,"conflicts":[],"refusals":[],"audit_ref":null,"audit_refs":[]}),
    )
}

fn load_all_sites(db: &Connection) -> Result<Vec<Value>, Value> {
    let count = db
        .query_row("SELECT COUNT(*) FROM site_registry", [], |row| {
            row.get::<_, i64>(0)
        })
        .map_err(db_error("site_registry_count_failed"))?;
    if count > MAX_REGISTRY_ROWS {
        return Err(error(
            "site_registry_row_bound_exceeded",
            "site registry exceeds the 10000-record safety bound",
        ));
    }
    let mut stmt = db.prepare("SELECT site_id,variant,site_root,substrate,aim_json,control_endpoint,last_seen_at,created_at,lifecycle_status,observation_status,sources_json,aliases_json,revision,updated_at,retired_at,retire_reason FROM site_registry ORDER BY created_at ASC,site_id ASC LIMIT 10001").map_err(db_error("site_registry_query_prepare_failed"))?;
    let result = stmt
        .query_map([], site_from_row)
        .map_err(db_error("site_registry_query_failed"))?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(db_error("site_registry_row_failed"));
    result
}

fn filesystem_candidates(root_filter: Option<&str>) -> Result<(Vec<Value>, usize), Value> {
    let base = env::var_os("NARADA_NATIVE_SITES_BASE_DIR")
        .map(PathBuf::from)
        .or_else(|| env::var_os("LOCALAPPDATA").map(|v| PathBuf::from(v).join("Narada")))
        .or_else(|| {
            env::var_os("USERPROFILE").map(|v| PathBuf::from(v).join("AppData/Local/Narada"))
        });
    let Some(base) = base else {
        return Ok((Vec::new(), 0));
    };
    if !base.is_dir() {
        return Ok((Vec::new(), 0));
    }
    let mut entries = fs::read_dir(&base)
        .map_err(io_error("site_registry_filesystem_scan_failed"))?
        .take(MAX_DISCOVERY_ENTRIES + 1)
        .collect::<Result<Vec<_>, _>>()
        .map_err(io_error("site_registry_filesystem_scan_failed"))?;
    if entries.len() > MAX_DISCOVERY_ENTRIES {
        return Err(error(
            "site_registry_discovery_bound_exceeded",
            "filesystem discovery exceeds 1000 entries",
        ));
    }
    entries.sort_by_key(|entry| entry.file_name());
    let mut found = Vec::new();
    let mut ignored = 0;
    for entry in entries {
        let root = entry.path();
        if !root.is_dir()
            || entry.file_name().to_string_lossy().starts_with('.')
            || entry.file_name() == "node_modules"
        {
            ignored += 1;
            continue;
        }
        let config = root.join("config.json");
        if !config.is_file() {
            ignored += 1;
            continue;
        }
        if root_filter.is_some_and(|filter| path_key(filter) != path_key(&root.to_string_lossy())) {
            continue;
        }
        let (aim, substrate) = read_candidate_config(&config);
        found.push(candidate(
            entry.file_name().to_string_lossy().as_ref(),
            &root,
            "native",
            &substrate,
            aim,
            "filesystem",
        ));
    }
    Ok((found, ignored))
}

fn launch_candidates(root_filter: Option<&str>) -> Result<(Vec<Value>, usize), Value> {
    let launch = user_site_root().join("config/launch/agents.psd1");
    if !launch.is_file() {
        return Ok((Vec::new(), 0));
    }
    let content = fs::read_to_string(&launch)
        .map_err(io_error("site_registry_launch_registry_read_failed"))?;
    if content.len() > 4 * 1024 * 1024 {
        return Err(error(
            "site_registry_launch_registry_too_large",
            "launch registry exceeds 4 MiB",
        ));
    }
    let mut records = Vec::<Map<String, Value>>::new();
    let mut current: Option<Map<String, Value>> = None;
    let mut ignored = 0;
    for line in content.lines().take(50_001) {
        let trimmed = line.trim();
        if trimmed == "@{" {
            current = Some(Map::new());
            continue;
        }
        if trimmed == "}" {
            if let Some(record) = current.take() {
                records.push(record);
            }
            continue;
        }
        if let Some(record) = current.as_mut() {
            if let Some((key, value)) = parse_ps_assignment(trimmed) {
                record.insert(key.to_string(), Value::String(value));
            }
        }
    }
    if content.lines().count() > 50_000 {
        return Err(error(
            "site_registry_launch_registry_bound_exceeded",
            "launch registry exceeds 50000 lines",
        ));
    }
    let mut found = Vec::new();
    for record in records.into_iter().take(MAX_DISCOVERY_ENTRIES + 1) {
        let Some(root) = record.get("SiteRoot").and_then(Value::as_str) else {
            ignored += 1;
            continue;
        };
        if root_filter.is_some_and(|filter| path_key(filter) != path_key(root)) {
            continue;
        }
        let id = record
            .get("Site")
            .and_then(Value::as_str)
            .map(ToString::to_string)
            .or_else(|| site_id_from_root(Path::new(root)))
            .or_else(|| {
                Path::new(root)
                    .file_name()
                    .map(|v| v.to_string_lossy().to_ascii_lowercase())
            });
        let Some(id) = id.filter(|v| !v.is_empty() && v != ".narada") else {
            ignored += 1;
            continue;
        };
        found.push(candidate(
            &id,
            Path::new(root),
            "native",
            "windows-launch-registry",
            None,
            "launch_registry",
        ));
    }
    if found.len() > MAX_DISCOVERY_ENTRIES {
        return Err(error(
            "site_registry_discovery_bound_exceeded",
            "launch registry discovery exceeds 1000 entries",
        ));
    }
    Ok((found, ignored))
}

