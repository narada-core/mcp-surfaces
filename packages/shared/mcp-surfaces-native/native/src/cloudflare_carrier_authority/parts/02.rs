fn carrier_health(args: &Map<String, Value>, state: &State) -> Result<Value, Value> {
    let id = args
        .get("projection_id")
        .and_then(Value::as_str)
        .unwrap_or("");
    let observed = now();
    let Some(entry) = projection(state, id, &observed)? else {
        return Ok(joined(
            "missing",
            Some("projection_registry_entry_missing"),
            id,
            json!({"status":"not_checked","site_id":null,"operation_id":null,"auth_source":null}),
            json!({"status":"not_checked","projection_id":id,"lineage_status":"unknown","last_event_sequence":null,"last_projected_at":null,"observed_at":observed}),
            "Configure the server-bound projection registry and retry.",
        ));
    };
    let mut projection = json!({"status":if entry.lifecycle=="active"{"not_checked"}else{entry.lifecycle},"projection_id":id,"lineage_status":entry.lineage,"last_event_sequence":null,"last_projected_at":null,"observed_at":observed,"expires_at":entry.expires,"revoked_at":entry.revoked});
    let mut carrier = json!({"status":"not_checked","site_id":entry.site_id,"operation_id":entry.operation_id,"auth_source":null});
    if entry.lifecycle == "active" {
        if let (Some(api), Some(token)) = (entry.api.as_ref(), entry.token.as_ref()) {
            let headers = Some(("x-narada-browser-token-fingerprint", token.as_str()));
            let (status, body) =
                get_json(&format!("{api}/api/nars/projections/{id}/health"), headers)?;
            if (200..300).contains(&status)
                && body.get("status").and_then(Value::as_str) == Some("healthy")
            {
                projection["status"] = json!("healthy");
                projection["last_event_sequence"] = body
                    .get("last_event_sequence")
                    .cloned()
                    .unwrap_or(Value::Null);
                projection["last_projected_at"] = body
                    .get("last_projected_at")
                    .cloned()
                    .unwrap_or(Value::Null);
                if projection["last_event_sequence"].is_null()
                    || projection["last_projected_at"].is_null()
                {
                    let (_,events)=get_json(&format!("{api}/api/nars/projections/{id}/events?direction=backward&max_events=1"),headers)?;
                    projection["last_event_sequence"] = events
                        .pointer("/cursor/last_sequence")
                        .cloned()
                        .unwrap_or(Value::Null);
                    projection["last_projected_at"] = events
                        .pointer("/events/0/projected_at")
                        .cloned()
                        .unwrap_or(Value::Null);
                }
            } else {
                projection["status"] = json!("unavailable");
                projection["code"] = json!(projection_unavailable(status));
            }
        } else {
            projection["status"] = json!("unavailable");
            projection["code"] = json!(if entry.api.is_some() {
                "projection_browser_credential_missing"
            } else {
                "projection_api_base_url_missing"
            });
        }
    }
    if projection["status"] == "healthy" && entry.lineage == "matched" {
        if let Some(site) = entry.site_id.as_ref() {
            let operation = if entry.operation_id.is_some() {
                "operation.read"
            } else {
                "site.read"
            };
            let mut params = json!({"site_id":site});
            if let Some(op) = entry.operation_id.as_ref() {
                params["operation_id"] = json!(op);
            }
            let body = json!({"operation":operation,"request_id":format!("mcp_carrier_health_{}",Uuid::new_v4()),"params":params});
            let (status, response) = request_json(
                "POST",
                &format!("{}/api/carrier", state.worker_url),
                cookie(state).as_deref(),
                Some(&body),
            )?;
            carrier["auth_source"] = if cookie(state).is_some() {
                json!("operator_session_file")
            } else {
                Value::Null
            };
            if (200..300).contains(&status) {
                carrier["status"] = json!("ok");
                carrier["product_health"] = response
                    .pointer("/site_product_status/health")
                    .or_else(|| response.pointer("/product_status/health"))
                    .cloned()
                    .unwrap_or(Value::Null);
                carrier["next_action"] = response
                    .pointer("/site_product_status/next_action")
                    .or_else(|| response.pointer("/product_status/next_action"))
                    .cloned()
                    .unwrap_or(Value::Null);
            } else {
                carrier["status"] = json!(if status == 401 {
                    "unauthorized"
                } else if status == 403 {
                    "forbidden"
                } else {
                    "unavailable"
                });
            }
        }
    }
    let (status, code, next) = if projection["status"] == "healthy" {
        if entry.lineage != "matched" {
            (
                "unverified",
                Some(if entry.lineage == "unknown" {
                    "projection_lineage_unknown"
                } else {
                    "projection_lineage_mismatched"
                }),
                "Register explicit Cloudflare carrier lineage before claiming joined health.",
            )
        } else if carrier["status"] == "ok" {
            ("healthy", None, "")
        } else if carrier["status"] == "unauthorized" {
            (
                "degraded",
                Some("carrier_api_unauthorized_projection_available"),
                "Refresh the operator session, then retry.",
            )
        } else if carrier["status"] == "forbidden" {
            (
                "degraded",
                Some("carrier_api_forbidden_projection_available"),
                "Inspect carrier Site membership.",
            )
        } else {
            (
                "degraded",
                Some("carrier_api_unavailable_projection_available"),
                "Inspect the carrier worker and network.",
            )
        }
    } else if matches!(entry.lifecycle, "revoked" | "expired") {
        (
            "degraded",
            Some(if entry.lifecycle == "revoked" {
                "projection_revoked"
            } else {
                "projection_expired"
            }),
            "Re-register or renew the projection.",
        )
    } else {
        (
            "unverified",
            projection.get("code").and_then(Value::as_str),
            "Repair projection readback before relying on joined health.",
        )
    };
    let code = code.map(str::to_string);
    Ok(joined(
        status,
        code.as_deref(),
        id,
        carrier,
        projection,
        next,
    ))
}

fn projection(state: &State, id: &str, observed: &str) -> Result<Option<Projection>, Value> {
    if id.is_empty()
        || id.len() > 256
        || !id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    {
        return Err(error(
            "projection_id_invalid",
            "projection_id must use 1-256 ASCII letters, digits, dot, underscore, or hyphen.",
            json!({}),
        ));
    }
    let root = state.projection_root.join(id);
    if !confined(&root, &state.projection_root) {
        return Err(error(
            "projection_path_refused",
            "Projection path escaped the server-bound registry.",
            json!({}),
        ));
    }
    let intent = optional_json(&root.join("intent.json"));
    let remote = optional_json(&root.join("remote-access.json"));
    if intent.is_none() && remote.is_none() {
        return Ok(None);
    }
    let a = intent.as_ref().unwrap_or(&Value::Null);
    let b = remote.as_ref().unwrap_or(&Value::Null);
    let site = string(a.get("site_id")).or_else(|| string(b.get("site_id")));
    let source = a.get("source_ref").or_else(|| b.get("source_ref"));
    let source_obj = source.and_then(Value::as_object);
    let kind = source_obj
        .and_then(|v| v.get("kind"))
        .and_then(Value::as_str);
    let operation_id = source_obj
        .and_then(|v| v.get("operation_id"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let carrier_id = source_obj
        .and_then(|v| v.get("carrier_session_id"))
        .and_then(Value::as_str);
    let lineage = if source.is_none() {
        "unknown"
    } else if kind == Some("cloudflare_carrier")
        && site.is_some()
        && (operation_id.is_some() || carrier_id.is_some())
    {
        "matched"
    } else {
        "mismatched"
    };
    let api = string(a.get("projection_api_base_url"))
        .or_else(|| string(b.get("projection_api_base_url")))
        .and_then(|v| validate_base_url(&v, false).ok())
        .or_else(|| legacy_projection_base(a).or_else(|| legacy_projection_base(b)));
    let tokens = b.get("browser_access_tokens").and_then(Value::as_array);
    let token = tokens
        .and_then(|v| {
            v.iter().find(|x| {
                x.get("kind").and_then(Value::as_str) == Some("browser")
                    && x.get("status")
                        .and_then(Value::as_str)
                        .is_none_or(|s| s == "active")
            })
        })
        .and_then(|v| string(v.get("token_fingerprint")));
    let expires = string(b.get("expires_at")).or_else(|| string(a.get("expires_at")));
    let revoked = string(b.get("revoked_at")).or_else(|| string(a.get("revoked_at")));
    let declared = string(b.get("lifecycle_state")).or_else(|| string(a.get("lifecycle_state")));
    let lifecycle = if revoked.is_some() || declared.as_deref() == Some("revoked") {
        "revoked"
    } else if expires
        .as_ref()
        .and_then(|v| OffsetDateTime::parse(v, &Rfc3339).ok())
        .is_some_and(|v| {
            v <= OffsetDateTime::parse(observed, &Rfc3339).unwrap_or(OffsetDateTime::UNIX_EPOCH)
        })
    {
        "expired"
    } else {
        "active"
    };
    Ok(Some(Projection {
        site_id: site,
        operation_id,
        lineage,
        api,
        token,
        lifecycle,
        expires,
        revoked,
    }))
}

