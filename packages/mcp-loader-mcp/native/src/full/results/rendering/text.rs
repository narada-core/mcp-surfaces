use crate::full::*;

pub(crate) fn render_result(result: &Value) -> String {
    let schema = result
        .get("schema")
        .and_then(Value::as_str)
        .unwrap_or("mcp_loader.result");
    let status = result.get("status").and_then(Value::as_str).unwrap_or("ok");
    if schema == "narada.mcp_loader.result_page.v1" {
        let page = result.get("result").unwrap_or(&Value::Null);
        let mut lines = vec![format!("{}: {}", schema, status)];
        for key in ["connection_id", "surface_id"] {
            if let Some(value) = result.get(key).and_then(Value::as_str) {
                lines.push(format!("{}: {}", key, value));
            }
        }
        for key in [
            "ref",
            "offset",
            "limit",
            "next_offset",
            "full_output_char_length",
        ] {
            if let Some(value) = page.get(key) {
                lines.push(format!("{}: {}", key, value));
            }
        }
        if let Some(text) = page.get("output_text").and_then(Value::as_str) {
            lines.push("output_text:".to_string());
            lines.push(text.to_string());
        }
        return lines.join("\n");
    }
    if schema == "narada.mcp_loader.tool_result.v1" {
        let mut lines = vec![format!("{}: {}", schema, status)];
        for key in [
            "connection_id",
            "surface_id",
            "details_ref",
            "details_reader",
        ] {
            if let Some(value) = result.get(key).and_then(Value::as_str) {
                lines.push(format!("{}: {}", key, value));
            }
        }
        if let Some(summary) = result.get("result_summary") {
            lines.push(format!(
                "result_summary: {}",
                serde_json::to_string(summary).unwrap_or_default()
            ));
        }
        if let Some(child) = result.get("result") {
            let child_text = pretty_json(child);
            let (excerpt, end) = bounded_page(&child_text, 0, 3_000, 6_000);
            lines.push("result:".to_string());
            lines.push(excerpt);
            if end < child_text.chars().count() {
                lines.push("result_text_truncated: true".to_string());
            }
        }
        return lines.join("\n");
    }
    if schema == "narada.mcp_loader.site_surfaces.v1" {
        let mut lines = vec![format!("{}: {}", schema, status)];
        if let Some(site_root) = result.get("site_root").and_then(Value::as_str) {
            lines.push(format!("site_root: {}", site_root));
        }
        if let Some(count) = result.get("surface_count").and_then(Value::as_u64) {
            lines.push(format!("surface_count: {}", count));
        }
        if let Some(surfaces) = result.get("surfaces").and_then(Value::as_array) {
            lines.push("bindings:".to_string());
            for surface in surfaces.iter().take(64) {
                let surface_id = surface
                    .get("surface_id")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown-surface");
                let binding_id = surface
                    .get("binding_id")
                    .and_then(Value::as_str)
                    .unwrap_or("unavailable");
                lines.push(format!("- {} [binding_id: {}]", surface_id, binding_id));
                if let Some(next_call) = surface.get("next_call") {
                    let tool_name = next_call
                        .get("tool_name")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown");
                    let arguments = next_call.get("arguments").unwrap_or(&Value::Null);
                    lines.push(format!(
                        "  next_call: {}({})",
                        tool_name,
                        serde_json::to_string(arguments).unwrap_or_default()
                    ));
                }
            }
            if surfaces.len() > 64 {
                lines.push(format!("... {} additional bindings omitted", surfaces.len() - 64));
            }
        }
        return lines.join("\n");
    }
    if schema == "narada.mcp_surface.guidance.v0" {
        let mut lines = vec![format!("{}: {}", schema, status)];
        if let Some(purpose) = result.get("purpose").and_then(Value::as_str) {
            lines.push(format!("purpose: {}", purpose));
        }
        if let Some(requested) = result.get("requested") {
            lines.push(format!(
                "requested: {}",
                serde_json::to_string(requested).unwrap_or_default()
            ));
        }
        if let Some(next_call) = result.get("next_call") {
            let tool_name = next_call
                .get("tool_name")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            let arguments = next_call.get("arguments").unwrap_or(&Value::Null);
            lines.push(format!(
                "next_call: {}({})",
                tool_name,
                serde_json::to_string(arguments).unwrap_or_default()
            ));
        }
        for key in ["first_use", "boundaries"] {
            if let Some(values) = result.get(key).and_then(Value::as_array) {
                lines.push(format!("{}:", key));
                for value in values.iter().take(8) {
                    if let Some(text) = value.as_str() {
                        lines.push(format!("- {}", text));
                    }
                }
            }
        }
        if result.get("compact").and_then(Value::as_bool) == Some(false) {
            lines.push("details: available in structuredContent; omitted from text projection".to_string());
        }
        return lines.join("\n");
    }
    if schema == "narada.mcp_loader.schema_lease.v1" {
        let mut lines = vec![format!("{}: {}", schema, status)];
        for key in [
            "connection_id",
            "surface_id",
            "tool_name",
            "generation_id",
            "schema_lease",
        ] {
            if let Some(value) = result.get(key).and_then(Value::as_str) {
                lines.push(format!("{}: {}", key, value));
            }
        }
        return lines.join("\n");
    }
    if schema == "narada.mcp_loader.site_tool_inventory_check.v1" {
        let mut lines = vec![
            format!("{}: {}", schema, status),
            format!(
                "checked_surface_count: {}",
                result
                    .get("checked_surface_count")
                    .and_then(Value::as_u64)
                    .unwrap_or(0)
            ),
            format!(
                "violation_count: {}",
                result
                    .get("violation_count")
                    .and_then(Value::as_u64)
                    .unwrap_or(0)
            ),
            format!(
                "finding_status_counts: {}",
                serde_json::to_string(result.get("finding_status_counts").unwrap_or(&json!({})))
                    .unwrap_or_default()
            ),
        ];
        if let Some(findings) = result.get("findings").and_then(Value::as_array) {
            if !findings.is_empty() {
                lines.push("findings:".to_string());
            }
            for finding in findings.iter().take(50) {
                let surface = finding
                    .get("surface_id")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown-surface");
                let status = finding
                    .get("status")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown");
                lines.push(format!("- {} [{}]", surface, status));
                for key in [
                    "missing_from_fabric",
                    "extra_in_fabric",
                    "duplicate_declared_tools",
                    "duplicate_observed_tools",
                    "unclassified_observed_tools",
                ] {
                    if let Some(values) = finding.get(key).and_then(Value::as_array) {
                        let visible: Vec<String> = values
                            .iter()
                            .filter_map(|value| value.as_str().map(String::from))
                            .take(20)
                            .collect();
                        if !visible.is_empty() {
                            lines.push(format!("  {}: {}", key, visible.join(", ")));
                        }
                    }
                }
                if let Some(error) = finding.get("error") {
                    let code = error.get("code").and_then(Value::as_str).unwrap_or("");
                    let message = error.get("message").and_then(Value::as_str).unwrap_or("");
                    if !code.is_empty() || !message.is_empty() {
                        lines.push(format!("  error: {} - {}", code, message));
                    }
                }
            }
        }
        if let Some(reference) = result.get("observation_ref").and_then(Value::as_str) {
            lines.push(format!("observation_ref: {}", reference));
        }
        return lines.join("\n");
    }
    let connection = result
        .get("connection_id")
        .and_then(Value::as_str)
        .map(|id| format!("\nconnection_id: {}", id))
        .unwrap_or_default();
    let surface = result
        .get("surface_id")
        .and_then(Value::as_str)
        .map(|id| format!("\nsurface_id: {}", id))
        .unwrap_or_default();
    format!("{}: {}{}{}", schema, status, connection, surface)
}
