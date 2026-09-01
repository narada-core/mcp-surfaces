fn send_control_frame(stream: &mut TcpStream, opcode: u8, body: &[u8]) -> Result<(), Value> {
    if body.len() > MAX_WEBSOCKET_FRAME_BYTES { return Err(unavailable("websocket_frame_too_large", "request frame exceeds limit")); }
    let mask = Uuid::new_v4().as_bytes()[..4].to_vec();
    let mut header = Vec::new(); header.push(0x80 | opcode);
    if body.len() < 126 { header.push(0x80 | body.len() as u8); }
    else if body.len() < 65_536 { header.push(0x80 | 126); header.extend_from_slice(&(body.len() as u16).to_be_bytes()); }
    else { header.push(0x80 | 127); header.extend_from_slice(&(body.len() as u64).to_be_bytes()); }
    header.extend_from_slice(&mask);
    let masked = body.iter().enumerate().map(|(index, byte)| byte ^ mask[index % 4]).collect::<Vec<_>>();
    stream.write_all(&header).and_then(|_| stream.write_all(&masked)).and_then(|_| stream.flush()).map_err(|error| unavailable("websocket_write_failed", &error.to_string()))
}

fn timeout() -> Duration { Duration::from_millis(std::env::var("NARADA_NARS_SESSION_REQUEST_TIMEOUT_MS").ok().and_then(|value| value.parse().ok()).unwrap_or(DEFAULT_TIMEOUT_MS).clamp(500, 30_000)) }
fn optional_text(value: Option<&Value>) -> Option<String> { value.and_then(Value::as_str).map(str::trim).filter(|value| !value.is_empty()).map(str::to_string) }
fn required(args: &Map<String, Value>, key: &str) -> Result<String, Value> { optional_text(args.get(key)).ok_or_else(|| error(&format!("{key}_required"), &format!("{key} is required"))) }
fn env_true(name: &str) -> bool { matches!(std::env::var(name).ok().as_deref(), Some("1" | "true" | "TRUE" | "yes")) }
fn authority_summary(record: &Value) -> Value { json!({ "authority_runtime_id": record.get("authority_runtime_id").cloned().unwrap_or(Value::Null), "authority_epoch": record.get("authority_epoch").cloned().unwrap_or_else(|| json!(1)), "source_write_admission": record.get("source_write_admission").cloned().unwrap_or(Value::Null), "authority_transition_state": record.get("authority_transition_state").cloned().unwrap_or(Value::Null), "superseded_by_session_id": record.get("superseded_by_session_id").cloned().unwrap_or(Value::Null), "authority_locator_ref": record.get("authority_locator_ref").cloned().unwrap_or(Value::Null) }) }
fn error(code: &str, message: &str) -> Value { json!({ "schema": "narada.nars_session_mcp.error.v1", "code": code, "message": message, "details": {} }) }
fn unavailable(reason: &str, detail: &str) -> Value { json!({ "schema": "narada.nars_session_mcp.error.v1", "code": reason, "message": detail, "details": { "reason": reason, "detail": detail } }) }
fn now_iso() -> String { time::OffsetDateTime::now_utc().format(&time::format_description::well_known::Rfc3339).unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string()) }

fn sha1(input: &[u8]) -> [u8; 20] {
    let mut message = input.to_vec(); let bit_len = (message.len() as u64) * 8; message.push(0x80); while message.len() % 64 != 56 { message.push(0); } message.extend_from_slice(&bit_len.to_be_bytes());
    let mut h = [0x67452301_u32, 0xefcdab89, 0x98badcfe, 0x10325476, 0xc3d2e1f0];
    for chunk in message.chunks_exact(64) {
        let mut words = [0_u32; 80]; for index in 0..16 { words[index] = u32::from_be_bytes([chunk[index*4], chunk[index*4+1], chunk[index*4+2], chunk[index*4+3]]); }
        for index in 16..80 { words[index] = (words[index-3] ^ words[index-8] ^ words[index-14] ^ words[index-16]).rotate_left(1); }
        let (mut a, mut b, mut c, mut d, mut e) = (h[0], h[1], h[2], h[3], h[4]);
        for index in 0..80 { let (f, k) = if index < 20 { ((b & c) | ((!b) & d), 0x5a827999) } else if index < 40 { (b ^ c ^ d, 0x6ed9eba1) } else if index < 60 { ((b & c) | (b & d) | (c & d), 0x8f1bbcdc) } else { (b ^ c ^ d, 0xca62c1d6) }; let temp = a.rotate_left(5).wrapping_add(f).wrapping_add(e).wrapping_add(k).wrapping_add(words[index]); e = d; d = c; c = b.rotate_left(30); b = a; a = temp; }
        h[0] = h[0].wrapping_add(a); h[1] = h[1].wrapping_add(b); h[2] = h[2].wrapping_add(c); h[3] = h[3].wrapping_add(d); h[4] = h[4].wrapping_add(e);
    }
    let mut output = [0_u8; 20]; for (index, value) in h.iter().enumerate() { output[index*4..index*4+4].copy_from_slice(&value.to_be_bytes()); } output
}

fn base64_encode(bytes: &[u8]) -> String { const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/"; let mut output = String::new(); let mut index = 0; while index < bytes.len() { let a = bytes[index]; let b = bytes.get(index+1).copied().unwrap_or(0); let c = bytes.get(index+2).copied().unwrap_or(0); output.push(TABLE[(a >> 2) as usize] as char); output.push(TABLE[(((a & 3) << 4) | (b >> 4)) as usize] as char); output.push(if index + 1 < bytes.len() { TABLE[((b & 15) << 2 | (c >> 6)) as usize] as char } else { '=' }); output.push(if index + 2 < bytes.len() { TABLE[(c & 63) as usize] as char } else { '=' }); index += 3; } output }

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn websocket_handshake_hash_matches_rfc_example() {
        assert_eq!(
            base64_encode(&sha1(b"The quick brown fox jumps over the lazy dog")),
            "L9ThxnotKPzthJ7hu3bnORuT6xI="
        );
    }

    #[test]
    fn endpoint_parser_accepts_bounded_local_ws_endpoint() {
        let endpoint = WsEndpoint::parse("ws://127.0.0.1:4123/events").expect("endpoint");
        assert_eq!(endpoint.host, "127.0.0.1");
        assert_eq!(endpoint.port, 4123);
        assert_eq!(endpoint.path, "/events");
        assert!(WsEndpoint::parse("http://127.0.0.1:4123/events").is_err());
    }

    #[test]
    fn status_summary_prefers_terminal_evidence() {
        let summary = summarize_events(&[
            json!({"event":"input_event_queued"}),
            json!({"event":"input_event_started"}),
            json!({"event":"turn_complete", "terminal_state":"completed"}),
        ]);
        assert_eq!(summary.status, "admitted_to_turn");
        assert_eq!(summary.outcome, "completed");
        assert_eq!(summary.terminal_state.as_deref(), Some("completed"));
    }

    #[test]
    fn health_gate_requires_explicit_healthy_status() {
        assert!(health_is_healthy(&json!({"status":"healthy"})));
        assert!(health_is_healthy(&json!({"status":"degraded"})) == false);
        assert!(health_is_healthy(&json!({"status":"closing"})) == false);
        assert!(!health_is_healthy(&json!({"event":"session_health"})));
        assert!(!health_is_healthy(&json!({})));
    }

    #[test]
    fn requested_site_selects_admitted_authority_before_duplicate_check() {
        let user_root = std::env::temp_dir().join(format!("narada-nars-authority-{}", Uuid::new_v4()));
        let site_a = std::env::temp_dir().join(format!("narada-nars-site-a-{}", Uuid::new_v4()));
        let site_b = std::env::temp_dir().join(format!("narada-nars-site-b-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&user_root).expect("user root");
        std::fs::create_dir_all(site_a.join(".narada/crew/nars-sessions/duplicate")).expect("site a");
        std::fs::create_dir_all(site_b.join(".narada/crew/nars-sessions/duplicate")).expect("site b");
        let connection = Connection::open(user_root.join("registry.db")).expect("registry");
        connection.execute_batch("CREATE TABLE site_registry (site_id TEXT NOT NULL, site_root TEXT NOT NULL, created_at TEXT NOT NULL);").expect("schema");
        connection.execute("INSERT INTO site_registry (site_id, site_root, created_at) VALUES (?1, ?2, ?3)", rusqlite::params!["site-a", site_a.to_string_lossy(), "2026-01-01T00:00:00Z"]).expect("site a row");
        connection.execute("INSERT INTO site_registry (site_id, site_root, created_at) VALUES (?1, ?2, ?3)", rusqlite::params!["site-b", site_b.to_string_lossy(), "2026-01-02T00:00:00Z"]).expect("site b row");
        drop(connection);
        std::fs::write(site_a.join(".narada/crew/nars-sessions/duplicate/session-index-record.json"), json!({"session_id":"duplicate","site_id":"site-a"}).to_string()).expect("site a record");
        std::fs::write(site_b.join(".narada/crew/nars-sessions/duplicate/session-index-record.json"), json!({"session_id":"duplicate","site_id":"site-b"}).to_string()).expect("site b record");
        std::env::set_var("NARADA_NARS_SESSION_SCOPE", "user_site");
        std::env::set_var("NARADA_USER_SITE_ROOT", &user_root);
        let selected = read_session_record(&user_root, "duplicate", Some("site-b")).expect("selected record");
        assert_eq!(selected["site_id"], "site-b");
        std::env::remove_var("NARADA_NARS_SESSION_SCOPE");
        std::env::remove_var("NARADA_USER_SITE_ROOT");
        let _ = std::fs::remove_dir_all(user_root);
        let _ = std::fs::remove_dir_all(site_a);
        let _ = std::fs::remove_dir_all(site_b);
    }
}
