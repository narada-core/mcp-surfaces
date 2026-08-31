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
