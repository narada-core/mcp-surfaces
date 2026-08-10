use serde_json::{json, Map, Value};
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

pub const MAX_AUTHORITY_OUTPUT_BYTES: usize = 4 * 1024 * 1024;

/// A bounded, explicit adapter boundary for a Rust surface to invoke an
/// owning MCP authority. The adapter is deliberately one-shot: the authority
/// receives one request, stdin closes, and the response is parsed before the
/// child exits. This keeps ownership explicit without introducing a hidden
/// long-lived worker or a Bun dependency into the Rust surface.
pub trait AuthorityAdapter {
    fn call(&self, method: &str, params: &Map<String, Value>) -> Result<Value, Value>;
}

/// Forward a boundary operation only when its owning authority has been
/// explicitly configured.  An absent entrypoint returns `None`, preserving
/// the surface's structured refusal and the Bun fallback selected by the
/// runtime matrix.  A configured entrypoint is authoritative for that call.
pub fn call_if_configured(
    surface_id: &str,
    method: &str,
    params: &Map<String, Value>,
) -> Option<Result<Value, Value>> {
    let key = format!(
        "NARADA_{}_AUTHORITY_ENTRYPOINT",
        surface_id.replace('-', "_").to_ascii_uppercase()
    );
    let configured = std::env::var(&key)
        .ok()
        .filter(|value| !value.trim().is_empty());
    configured.map(|_| StdioAuthorityAdapter::from_env(surface_id).and_then(|adapter| adapter.call(method, params)))
}

pub struct StdioAuthorityAdapter {
    executable: String,
    args: Vec<String>,
    site_root: Option<String>,
    label: String,
}

impl StdioAuthorityAdapter {
    pub fn from_env(surface_id: &str) -> Result<Self, Value> {
        let key = format!(
            "NARADA_{}_AUTHORITY_ENTRYPOINT",
            surface_id.replace('-', "_").to_ascii_uppercase()
        );
        let executable = std::env::var(&key)
            .ok()
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| unavailable(surface_id, "authority_entrypoint_unconfigured", &key))?;
        let args_key = format!(
            "NARADA_{}_AUTHORITY_ARGS",
            surface_id.replace('-', "_").to_ascii_uppercase()
        );
        let args = std::env::var(args_key)
            .ok()
            .map(|value| value.split('\u{1f}').map(ToOwned::to_owned).collect())
            .unwrap_or_default();
        let site_root = std::env::var("NARADA_SITE_ROOT")
            .ok()
            .filter(|value| !value.trim().is_empty());
        Ok(Self {
            executable,
            args,
            site_root,
            label: surface_id.to_string(),
        })
    }

    pub fn new(executable: impl Into<String>, args: Vec<String>, label: impl Into<String>) -> Self {
        Self {
            executable: executable.into(),
            args,
            site_root: None,
            label: label.into(),
        }
    }
}

impl AuthorityAdapter for StdioAuthorityAdapter {
    fn call(&self, method: &str, params: &Map<String, Value>) -> Result<Value, Value> {
        let request = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": method,
            "params": params,
        });
        let mut child = Command::new(&self.executable)
            .args(&self.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .envs(self.site_root.as_ref().map(|root| [("NARADA_SITE_ROOT", root)]).into_iter().flatten())
            .spawn()
            .map_err(|error| unavailable(&self.label, "authority_spawn_failed", &error.to_string()))?;
        if let Some(mut stdin) = child.stdin.take() {
            let body = serde_json::to_vec(&request)
                .map_err(|error| unavailable(&self.label, "authority_request_encode_failed", &error.to_string()))?;
            stdin
                .write_all(&body)
                .and_then(|_| stdin.write_all(b"\n"))
                .map_err(|error| unavailable(&self.label, "authority_request_write_failed", &error.to_string()))?;
        }
        let output = child
            .wait_with_output()
            .map_err(|error| unavailable(&self.label, "authority_wait_failed", &error.to_string()))?;
        if output.stdout.len() > MAX_AUTHORITY_OUTPUT_BYTES {
            return Err(unavailable(
                &self.label,
                "authority_response_too_large",
                &MAX_AUTHORITY_OUTPUT_BYTES.to_string(),
            ));
        }
        let response = parse_response(&output.stdout)
            .map_err(|code| unavailable(&self.label, code, &tail(&output.stderr)))?;
        if let Some(error) = response.get("error") {
            return Err(json!({
                "schema": "narada.authority_adapter.error.v1",
                "status": "error",
                "authority": self.label,
                "error": error,
            }));
        }
        Ok(response.get("result").cloned().unwrap_or(response))
    }
}

fn parse_response(bytes: &[u8]) -> Result<Value, &'static str> {
    let text = std::str::from_utf8(bytes).map_err(|_| "authority_response_not_utf8")?;
    for line in text.lines().rev() {
        let line = line.trim();
        if line.is_empty() || line.starts_with("Content-Length:") {
            continue;
        }
        if let Ok(value) = serde_json::from_str::<Value>(line) {
            return Ok(value);
        }
    }
    Err("authority_response_json_missing")
}

fn tail(bytes: &[u8]) -> String {
    let start = bytes.len().saturating_sub(4_000);
    String::from_utf8_lossy(&bytes[start..]).to_string()
}

pub fn unavailable(surface_id: &str, reason: &str, detail: &str) -> Value {
    json!({
        "schema": "narada.authority_adapter.boundary.v1",
        "status": "unavailable",
        "surface_id": surface_id,
        "reason": reason,
        "detail": detail,
        "remediation": "Configure the owning Rust authority adapter and retry.",
    })
}

pub fn bounded_path(root: &Path, candidate: &str) -> Result<std::path::PathBuf, Value> {
    let path = std::path::PathBuf::from(candidate);
    let resolved = if path.is_absolute() { path } else { root.join(path) };
    let root = root
        .canonicalize()
        .map_err(|error| unavailable("authority", "authority_root_unavailable", &error.to_string()))?;
    let resolved = resolved
        .canonicalize()
        .map_err(|error| unavailable("authority", "authority_path_unavailable", &error.to_string()))?;
    if !resolved.starts_with(&root) {
        return Err(unavailable("authority", "authority_path_outside_root", &resolved.to_string_lossy()));
    }
    Ok(resolved)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn adapter_boundary_is_structured_and_bounded() {
        let value = unavailable("calendar", "authority_entrypoint_unconfigured", "NARADA_CALENDAR_AUTHORITY_ENTRYPOINT");
        assert_eq!(value["status"], "unavailable");
        assert_eq!(value["surface_id"], "calendar");
        assert!(value["remediation"].as_str().unwrap().contains("adapter"));
    }

    #[test]
    fn bounded_path_rejects_escape() {
        let root = std::env::temp_dir().join(format!("narada-authority-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).expect("root");
        fs::write(root.join("inside.json"), "{}").expect("inside");
        assert!(bounded_path(&root, "inside.json").is_ok());
        assert!(bounded_path(&root, "..\\outside.json").is_err());
        fs::remove_dir_all(root).expect("cleanup");
    }
}
