use crate::full::*;

pub(crate) fn normalize_path(raw: &str) -> String {
    let input = PathBuf::from(raw.replace('\\', "/"));
    let absolute = if input.is_absolute() {
        input
    } else {
        env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(input)
    };
    let mut parts: Vec<String> = Vec::new();
    for component in absolute.components() {
        match component {
            Component::Prefix(prefix) => {
                parts.push(prefix.as_os_str().to_string_lossy().to_string())
            }
            Component::RootDir => {}
            Component::CurDir => {}
            Component::ParentDir => {
                let _ = parts.pop();
            }
            Component::Normal(part) => parts.push(part.to_string_lossy().to_string()),
        }
    }
    let mut output = String::new();
    if raw.len() >= 2 && raw.as_bytes()[1] == b':' {
        output.push_str(&raw[..2]);
    }
    if output.is_empty() {
        if raw.starts_with('/') {
            output.push('/');
        }
    } else if !output.ends_with('/') {
        output.push('/');
    }
    output.push_str(
        &parts
            .iter()
            .skip(if raw.len() >= 2 && raw.as_bytes()[1] == b':' {
                1
            } else {
                0
            })
            .cloned()
            .collect::<Vec<_>>()
            .join("/"),
    );
    if output.is_empty() {
        ".".to_string()
    } else {
        output.trim_end_matches('/').to_string()
    }
}

pub(crate) fn join_path(root: &str, child: &str) -> String {
    normalize_path(&format!(
        "{}/{}",
        root.trim_end_matches(['\\', '/']),
        child.trim_start_matches(['\\', '/'])
    ))
}

pub(crate) fn normalize_policy_prefix(prefix: &str) -> String {
    let normalized = prefix.replace('\\', "/").trim_end_matches('/').to_string();
    if normalized == "{site_root}" || normalized.starts_with("{site_root}/") {
        normalized
    } else {
        normalize_path(&normalized)
    }
}

pub(crate) fn is_under_path(child: &str, parent: &str) -> bool {
    let c = normalize_path(child);
    let p = normalize_path(parent);
    c == p || c.starts_with(&(p + "/"))
}

pub(crate) fn derive_site_id(site_root: &str) -> Result<String, Diagnostic> {
    let normalized = site_root.replace('\\', "/");
    let value = normalized
        .rsplit('/')
        .find(|part| !part.is_empty())
        .unwrap_or("site");
    let id = value
        .strip_prefix("narada.")
        .or_else(|| value.strip_prefix("narada-"))
        .unwrap_or(value)
        .to_string();
    if id == "andrey"
        || id == "user-site"
        || value == "narada-andrey"
        || value == "narada-user-site"
    {
        return Err(Diagnostic::new(
            "site_fabric_legacy_site_id_rejected",
            format!("site_fabric_legacy_site_id_rejected:{}:site_root", value),
        ));
    }
    Ok(id)
}

pub(crate) fn interpolate_site_arg(value: &str, site_root: &str) -> Result<String, Diagnostic> {
    let site_control_root = if site_root.ends_with("/.narada") {
        site_root.to_string()
    } else {
        join_path(site_root, ".narada")
    };
    let site_id = derive_site_id(site_root)?;
    Ok(value
        .replace("{site_root}", site_root)
        .replace("{site_control_root}", &site_control_root)
        .replace(
            "{site_runtime_root}",
            &join_path(&site_control_root, "runtime"),
        )
        .replace("{site_id}", &site_id))
}

pub(crate) fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default()
}

pub(crate) fn now_iso() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
}

pub(crate) fn new_id(prefix: &str) -> String {
    format!(
        "{}-{}-{}",
        prefix,
        now_ms(),
        ID_COUNTER.fetch_add(1, Ordering::SeqCst)
    )
}

pub(crate) struct FabricBundle {
    pub(crate) fabric: JsonObject,
    pub(crate) paths: Vec<String>,
    pub(crate) source_by_surface: HashMap<String, String>,
}
