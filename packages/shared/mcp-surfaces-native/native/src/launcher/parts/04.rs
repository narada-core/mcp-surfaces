fn read_bounded(path: &Path) -> Result<String, Value> {
    let mut file = File::open(path).map_err(|error| {
        diagnostic(
            "launch_registry_missing",
            &format!("launch_registry_missing:{}", path_text(path)),
            json!({"path":path_text(path),"error":error.to_string()}),
        )
    })?;
    let size = file
        .metadata()
        .map_err(|error| {
            diagnostic(
                "launch_registry_stat_failed",
                &error.to_string(),
                json!({"path":path_text(path)}),
            )
        })?
        .len();
    if size > MAX_REGISTRY_BYTES {
        return Err(diagnostic(
            "launch_registry_too_large",
            "launch registry exceeds bounded parser input",
            json!({"bytes":size,"maximum":MAX_REGISTRY_BYTES}),
        ));
    }
    let mut source = String::with_capacity(size as usize);
    file.seek(SeekFrom::Start(0)).ok();
    file.read_to_string(&mut source).map_err(|error| {
        diagnostic(
            "launch_registry_read_failed",
            &error.to_string(),
            json!({"path":path_text(path)}),
        )
    })?;
    Ok(source)
}

fn registry_path(root: &Path, configured: Option<&Path>, requested: Option<&str>) -> PathBuf {
    requested
        .map(PathBuf::from)
        .or_else(|| configured.map(PathBuf::from))
        .unwrap_or_else(|| root.join("config").join("launch").join("agents.psd1"))
}

fn required_field(object: &Map<String, Value>, key: &str) -> Result<String, Value> {
    string_field(object, key).ok_or_else(|| {
        diagnostic(
            "psd1_field_missing",
            &format!("agent_missing:{key}"),
            json!({"field":key}),
        )
    })
}
fn string_field(object: &Map<String, Value>, key: &str) -> Option<String> {
    object
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .filter(|value| !value.is_empty())
}
fn string_array(value: Option<&Value>) -> Vec<String> {
    match value {
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .filter(|v| !v.is_empty())
            .collect(),
        Some(Value::String(value)) if !value.is_empty() => vec![value.clone()],
        _ => Vec::new(),
    }
}
fn nonempty_filter(value: Option<&Value>) -> Option<Vec<String>> {
    let values = string_array(value);
    if values.is_empty() {
        None
    } else {
        Some(values)
    }
}
fn optional_string(args: &Map<String, Value>, key: &str) -> Option<String> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
}
fn clamp(value: Option<i64>, fallback: usize, min: usize, max: usize) -> usize {
    value
        .map(|v| v.max(min as i64).min(max as i64) as usize)
        .unwrap_or(fallback)
        .clamp(min, max)
}
fn strip_digits(value: &str) -> String {
    value
        .trim_end_matches(|ch: char| ch.is_ascii_digit())
        .to_string()
}
fn normalize_path(value: &str) -> String {
    value.replace('\\', "/")
}
fn resolve_dependency_narada_root(narada_root: &str) -> String {
    let target = PathBuf::from(narada_root);
    let parent = target.parent().unwrap_or_else(|| Path::new(""));
    let sibling = parent.join("narada");
    let user_source = parent.join("src").join("narada");
    for candidate in [&target, &sibling, &user_source] {
        if candidate.join("package.json").is_file()
            && candidate
                .join("packages")
                .join("layers")
                .join("cli")
                .join("package.json")
                .is_file()
        {
            return path_text(candidate);
        }
    }
    if target
        .file_name()
        .map(|name| name.to_string_lossy().eq_ignore_ascii_case("narada"))
        .unwrap_or(false)
    {
        return path_text(&target);
    }
    if parent
        .file_name()
        .map(|name| {
            let value = name.to_string_lossy();
            value.eq_ignore_ascii_case("src") || value.eq_ignore_ascii_case("code")
        })
        .unwrap_or(false)
    {
        return path_text(&sibling);
    }
    path_text(&user_source)
}
fn join_path(root: &str, child: &str) -> String {
    let path = PathBuf::from(child);
    if path.is_absolute() {
        path_text(&path)
    } else {
        path_text(&PathBuf::from(root).join(path))
    }
}
fn path_text(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn native_runtime_binary(record: &AgentRecord) -> PathBuf {
    if let Some(path) = std::env::var_os("NARADA_AGENT_RUNTIME_SERVER_NATIVE") {
        return PathBuf::from(path);
    }
    let executable = if cfg!(windows) {
        "narada-agent-runtime-server-rust.exe"
    } else {
        "narada-agent-runtime-server-rust"
    };
    PathBuf::from(&record.dependency_narada_root)
        .join("packages")
        .join("agent-runtime-server")
        .join("native")
        .join("target")
        .join("release")
        .join(executable)
}
fn site_aliases(record: &AgentRecord) -> Vec<String> {
    let prefix = record.agent.split('.').next().unwrap_or(&record.agent);
    vec![
        record.site.clone(),
        record
            .site
            .strip_prefix("narada-")
            .unwrap_or(&record.site)
            .to_string(),
        if record.site.starts_with("narada-") {
            record.site.clone()
        } else {
            format!("narada-{}", record.site)
        },
        prefix.to_string(),
        if prefix.starts_with("narada-") {
            prefix.to_string()
        } else {
            format!("narada-{}", prefix)
        },
    ]
}
fn scope_loci(scope: &str) -> Vec<&'static str> {
    match scope {
        "none" => vec![],
        "host" => vec!["host"],
        "user-site" => vec!["user-site"],
        "local-site" => vec!["local-site"],
        _ => vec!["host", "user-site", "local-site"],
    }
}
fn finding(severity: &str, code: &str, message: &str, path: &Path) -> Value {
    json!({"severity":severity,"code":code,"message":message,"path":path_text(path)})
}
fn diagnostic(code: &str, message: &str, details: Value) -> Value {
    let mut value = json!({"code":code,"message":message});
    if !details.is_null() {
        value["details"] = details;
    }
    value
}

struct Parser<'a> {
    tokens: Vec<Token>,
    position: usize,
    _source: &'a str,
}
#[derive(Clone, Debug)]
enum Token {
    Word(String),
    At,
    OpenBrace,
    CloseBrace,
    OpenParen,
    CloseParen,
    Equals,
    Separator,
}
