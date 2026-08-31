use crate::full::*;

pub(crate) fn admitted_binding_entry(envelope: &Value, requested: &str) -> Option<Value> {
    let aliases = [Some(requested), requested.strip_prefix("narada-")];
    envelope
        .get("bindings")
        .and_then(Value::as_array)
        .and_then(|bindings| {
            bindings
                .iter()
                .find(|binding| {
                    let candidate = binding.get("binding_id").and_then(Value::as_str);
                    aliases
                        .iter()
                        .flatten()
                        .any(|alias| candidate == Some(*alias))
                })
                .cloned()
        })
}

pub(crate) fn admitted_binding(
    state: &LoaderState,
    _site_root: &str,
    binding_id: &str,
    operation: &str,
) -> Result<Option<(Value, Value)>, Diagnostic> {
    assert_binding_admission_available(state)?;
    let Some(envelope) = &state.binding_admission else {
        return Ok(None);
    };
    let entry = admitted_binding_entry(envelope, binding_id)
        .ok_or_else(|| {
            let candidates = envelope
                .get("bindings")
                .and_then(Value::as_array)
                .map(|bindings| {
                    bindings
                        .iter()
                        .filter(|binding| {
                            let candidate = binding.get("binding_id").and_then(Value::as_str).unwrap_or_default();
                            let surface = binding.get("surface_id").and_then(Value::as_str).unwrap_or_default();
                            !surface.is_empty()
                                && (binding_id.ends_with(surface)
                                    || candidate.ends_with(binding_id)
                                    || candidate == binding_id)
                        })
                        .filter_map(|binding| binding.get("binding_id").cloned())
                        .take(10)
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            Diagnostic::new(
                "mcp_binding_not_admitted",
                format!("mcp_binding_not_admitted:{binding_id}:{operation}"),
            ).with_details(json!({
                "requested_binding_id":binding_id,
                "operation":operation,
                "candidate_binding_ids":candidates,
                "blocked_operation":"binding_activation_or_call",
                "failed_requirement":"materialized_binding_admission",
                "unaffected_authority":["other_admitted_bindings","static_carrier_surfaces"],
                "repair_owner":"site_configuration_or_carrier_materialization",
                "agent_may_repair":false,
                "restart_required":true,
                "remediation":"Use the canonical binding_id returned by mcp_loader_list_site_surfaces or registrar_site_bind. Generated server names prefixed with narada- are accepted as compatibility aliases."
            }))
        })?;
    let operation_allowed = entry
        .get("operations")
        .and_then(Value::as_array)
        .is_some_and(|operations| {
            operations
                .iter()
                .any(|value| value.as_str() == Some(operation))
        });
    if !operation_allowed {
        return Err(Diagnostic::new(
            "mcp_binding_not_admitted",
            format!("mcp_binding_not_admitted:{binding_id}:{operation}"),
        ).with_details(json!({
            "requested_binding_id":binding_id,
            "operation":operation,
            "blocked_operation":"binding_operation",
            "failed_requirement":"operation_admission",
            "unaffected_authority":["admitted_operations_on_this_binding","other_admitted_bindings"],
            "repair_owner":"site_configuration",
            "agent_may_repair":false,
            "restart_required":true
        })));
    }
    let server = entry.get("binding_identity").cloned().ok_or_else(|| {
        Diagnostic::new(
            "mcp_binding_identity_required",
            format!("mcp_binding_identity_required:{binding_id}"),
        )
    })?;
    let actual = narada_mcp_fabric_contracts::binding_admission_entry_digest_v1(&entry);
    let expected = entry
        .get("binding_digest")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if actual != expected {
        return Err(Diagnostic::new("mcp_binding_digest_mismatch", format!("mcp_binding_digest_mismatch:{binding_id}"))
            .with_details(json!({"child_spawned":false,"expected_binding_digest":expected,"actual_binding_digest":actual})));
    }
    Ok(Some((entry, server)))
}
