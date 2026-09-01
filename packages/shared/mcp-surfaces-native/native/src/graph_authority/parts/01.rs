use crate::calendar::provider::{build_request, wrap_result};
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::Path;
use std::time::Duration;
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

const DEFAULT_GRAPH_BASE_URL: &str = "https://graph.microsoft.com/v1.0";
const DEFAULT_TOKEN_SCOPE: &str = "https://graph.microsoft.com/.default";
const MAX_CONFIG_BYTES: u64 = 512 * 1024;
const MAX_ENV_BYTES: u64 = 512 * 1024;
const MAX_REQUEST_BYTES: usize = 512 * 1024;
const MAX_UPLOAD_REQUEST_BYTES: usize = 10 * 1024 * 1024;
const MAX_RESPONSE_BYTES: u64 = 4 * 1024 * 1024;
const MAX_URL_BYTES: usize = 16 * 1024;
const MAX_AUDIT_BYTES: usize = 64 * 1024;

enum GraphAuth {
    AccessToken(String),
    ClientCredentials {
        endpoint: String,
        client_id: String,
        client_secret: String,
    },
    Unavailable(Value),
    Missing,
}

/// Native Microsoft Graph provider authority shared by the calendar and mail
/// surfaces. Activation remains explicit at the surface entrypoint.
pub struct CalendarGraphAdapter {
    base_url: String,
    allowed_mailboxes: Vec<String>,
    allow_event_writes: bool,
    write_approval_token: Option<String>,
    auth: GraphAuth,
    request_timeout: Duration,
}

impl GraphAuth {
    fn access_token(&self) -> Result<String, Value> {
        match self {
            Self::AccessToken(value) => Ok(value.clone()),
            Self::ClientCredentials {
                endpoint,
                client_id,
                client_secret,
            } => request_client_credentials(endpoint, client_id, client_secret),
            Self::Unavailable(error) => Err(error.clone()),
            Self::Missing => Err(unavailable(
                "graph_access_token_missing",
                "set MS_GRAPH_ACCESS_TOKEN or GRAPH_TENANT_ID/GRAPH_CLIENT_ID/GRAPH_CLIENT_SECRET",
            )),
        }
    }
}

