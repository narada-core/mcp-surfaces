fn validate_proxy_launch(
    input: &MaterializationInput,
    carrier: &CarrierInput,
    server: &ServerInput,
) -> Result<(), Failure> {
    let command = PathBuf::from(&server.command);
    if !command.is_absolute() || !path_eq(&command, &input.proxy_entrypoint) {
        return Err(Failure::new(
            "materializer_proxy_command_mismatch",
            server.name.clone(),
        ));
    }
    let required = [
        ("--runtime-contract-version", CONTRACT_VERSION.to_string()),
        (
            "--artifact-manifest",
            path_text(&input.artifact_manifest_path),
        ),
        ("--carrier-id", carrier.carrier_id.clone()),
        (
            "--carrier-kind",
            match carrier.carrier_kind {
                CarrierKind::Codex => "codex",
                CarrierKind::Kimi => "kimi",
                CarrierKind::Opencode => "opencode",
                CarrierKind::Pi => "pi",
            }
            .to_string(),
        ),
        (
            "--registrar-command",
            path_text(&input.registrar_entrypoint),
        ),
        (
            "--registrar-entrypoint",
            path_text(&input.registrar_entrypoint),
        ),
        (
            "--materialization-sidecar",
            path_text(&suffix_path(
                &carrier.config_path,
                ".narada-generation.json",
            )),
        ),
    ];
    for (flag, expected) in required {
        let actual = arg_value(&server.args, flag);
        let equal = if flag.contains("manifest")
            || flag.contains("registrar")
            || flag.contains("sidecar")
        {
            actual
                .map(PathBuf::from)
                .is_some_and(|value| path_eq(&value, Path::new(&expected)))
        } else {
            actual == Some(expected.as_str())
        };
        if !equal {
            return Err(Failure::new(
                "materializer_proxy_argument_mismatch",
                format!("{}:{flag}", server.name),
            )
            .with_details(json!({"expected":expected,"actual":actual})));
        }
    }
    if arg_value(&server.args, "--child-command").is_none()
        || arg_value(&server.args, "--entrypoint").is_none()
    {
        return Err(Failure::new(
            "materializer_proxy_child_contract_incomplete",
            server.name.clone(),
        ));
    }
    Ok(())
}

fn arg_value<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    args.iter()
        .position(|arg| arg == flag)
        .and_then(|index| args.get(index + 1))
        .map(String::as_str)
}

fn path_eq(left: &Path, right: &Path) -> bool {
    path_text(left).eq_ignore_ascii_case(&path_text(right))
}

fn validate_identifier(value: &str, field: &'static str) -> Result<(), Failure> {
    let valid = !value.is_empty()
        && value.len() <= 200
        && value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-'));
    if valid {
        Ok(())
    } else {
        Err(Failure::new(
            "materializer_identifier_invalid",
            format!("{field}:{value}"),
        ))
    }
}

fn emit_carrier(carrier: &CarrierInput) -> Result<Vec<u8>, Failure> {
    match carrier.carrier_kind {
        CarrierKind::Codex => emit_codex(carrier),
        CarrierKind::Kimi => emit_json_carrier(carrier, "mcpServers"),
        CarrierKind::Opencode => emit_json_carrier(carrier, "mcp"),
        CarrierKind::Pi => emit_pi(carrier),
    }
}

fn emit_pi(carrier: &CarrierInput) -> Result<Vec<u8>, Failure> {
    const TEMPLATE: &str = include_str!("../../../../assets/pi-mcp-extension.ts");
    const PRESENTATION: &str = include_str!("../../../../assets/mcp-result-presentation.ts");
    const PLACEHOLDER: &str = "__NARADA_PI_MCP_SERVERS__";
    const PRESENTATION_PLACEHOLDER: &str = "__NARADA_MCP_RESULT_PRESENTATION__";
    if TEMPLATE.matches(PLACEHOLDER).count() != 1
        || TEMPLATE.matches(PRESENTATION_PLACEHOLDER).count() != 1
    {
        return Err(Failure::new(
            "materializer_pi_template_invalid",
            "Pi extension template must contain exactly one server and presentation placeholder",
        ));
    }
    let servers = carrier
        .servers
        .iter()
        .filter(|server| {
            matches!(
                server.name.as_str(),
                "mcp-loader"
            )
        })
        .map(|server| {
            json!({
                "name": server.name,
                "command": server.command,
                "args": server.args,
                "enabled": server.enabled,
                "startupTimeoutMs": server.startup_timeout_sec.unwrap_or(60) * 1000,
            })
        })
        .collect::<Vec<_>>();
    let encoded = serde_json::to_string(&servers).map_err(json_failure)?;
    Ok(TEMPLATE
        .replace(PRESENTATION_PLACEHOLDER, PRESENTATION)
        .replace(PLACEHOLDER, &encoded)
        .into_bytes())
}

fn emit_json_carrier(carrier: &CarrierInput, field: &str) -> Result<Vec<u8>, Failure> {
    let mut servers = Map::new();
    for server in &carrier.servers {
        let value = match carrier.carrier_kind {
            CarrierKind::Kimi => {
                let mut value = json!({
                    "transport": "stdio",
                    "command": server.command,
                    "args": server.args,
                    "protocolVersion": "2026-07-28",
                });
                if let Some(mode) = &server.approval_mode {
                    value["approval_mode"] = Value::String(mode.clone());
                }
                if !server.env_vars.is_empty() {
                    value["env_vars"] = json!(server.env_vars);
                }
                value
            }
            CarrierKind::Opencode => json!({
                "type": "local",
                "command": std::iter::once(&server.command).chain(server.args.iter()).collect::<Vec<_>>(),
                "enabled": server.enabled,
            }),
            CarrierKind::Codex => unreachable!("Codex uses TOML"),
            CarrierKind::Pi => unreachable!("Pi uses its extension projection"),
        };
        servers.insert(server.name.clone(), value);
    }
    let mut root = Map::new();
    if matches!(carrier.carrier_kind, CarrierKind::Opencode) {
        root.insert(
            "$schema".to_string(),
            Value::String("https://opencode.ai/config.json".to_string()),
        );
    }
    root.insert(field.to_string(), Value::Object(servers));
    let mut output = pretty_json(&Value::Object(root))?;
    if matches!(carrier.carrier_kind, CarrierKind::Opencode) {
        output.splice(
            0..0,
            b"// Narada owns this entire OpenCode carrier document; use materialization to change it.\n".iter().copied(),
        );
    }
    Ok(output)
}

fn emit_codex(carrier: &CarrierInput) -> Result<Vec<u8>, Failure> {
    let mut out = String::from("# Narada manages only recorded MCP and carrier policy settings; other Codex settings are preserved.\n\n# Codex Apps/connectors are opt-in for profile-less launches.\n[features]\napps = false\n\n");
    for (plugin, enabled) in &carrier.codex_plugin_overrides {
        out.push_str(&format!(
            "[plugins.{}]\nenabled = {}\n\n",
            toml_key(plugin),
            enabled
        ));
    }
    for project in &carrier.trust_projects {
        out.push_str(&format!(
            "[projects.{}]\ntrust_level = \"trusted\"\n\n",
            json_string(project)?
        ));
    }
    for server in &carrier.servers {
        out.push_str(&format!("[mcp_servers.{}]\n", toml_key(&server.name)));
        out.push_str(&format!("command = {}\n", json_string(&server.command)?));
        out.push_str(&format!("args = {}\n", string_array(&server.args)?));
        out.push_str(&format!(
            "default_tools_approval_mode = {}\n",
            json_string(server.approval_mode.as_deref().unwrap_or("approve"))?
        ));
        if let Some(timeout) = server.startup_timeout_sec {
            out.push_str(&format!("startup_timeout_sec = {timeout}\n"));
        }
        if !server.env_vars.is_empty() {
            out.push_str(&format!("env_vars = {}\n", string_array(&server.env_vars)?));
        }
        out.push('\n');
    }
    Ok(out.into_bytes())
}

fn toml_key(value: &str) -> String {
    if value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-'))
    {
        value.to_string()
    } else {
        serde_json::to_string(value).expect("string serialization cannot fail")
    }
}
fn json_string(value: &str) -> Result<String, Failure> {
    serde_json::to_string(value).map_err(json_failure)
}
fn string_array(values: &[String]) -> Result<String, Failure> {
    Ok(format!(
        "[{}]",
        values
            .iter()
            .map(|value| json_string(value))
            .collect::<Result<Vec<_>, _>>()?
            .join(",")
    ))
}
fn transactional_publish(publications: &[Publication]) -> Result<(), Failure> {
    let snapshots = publications
        .iter()
        .map(|item| Snapshot {
            path: item.path.clone(),
            content: fs::read(&item.path).ok(),
        })
        .collect::<Vec<_>>();
    for publication in publications {
        if let Err(error) = atomic_write(&publication.path, &publication.content) {
            let rollback_errors = rollback(&snapshots);
            return Err(
                Failure::new("materializer_transaction_failed", error.to_string()).with_details(
                    json!({
                        "failed_path": path_text(&publication.path),
                        "rollback_errors": rollback_errors,
                    }),
                ),
            );
        }
    }
    Ok(())
}

