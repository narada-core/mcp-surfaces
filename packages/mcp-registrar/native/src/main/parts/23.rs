fn read_message<R: BufRead>(input: &mut R) -> Result<Option<Value>, String> {
    let Some(first) = read_line_bounded(input, MAX_MESSAGE_BYTES)? else {
        return Ok(None);
    };
    if first.to_ascii_lowercase().starts_with("content-length:") {
        let length = first
            .split_once(':')
            .and_then(|(_, v)| v.trim().parse::<usize>().ok())
            .ok_or("invalid_content_length")?;
        if length > MAX_MESSAGE_BYTES {
            return Err("mcp_body_exceeds_byte_limit".into());
        }
        let mut header_bytes = first.len();
        loop {
            let Some(line) = read_line_bounded(input, MAX_HEADER_BYTES)? else {
                return Err("unexpected_eof_in_headers".into());
            };
            header_bytes += line.len();
            if header_bytes > MAX_HEADER_BYTES {
                return Err("mcp_headers_exceed_byte_limit".into());
            }
            if line == "\r\n" || line == "\n" {
                break;
            }
        }
        let mut body = vec![0; length];
        input.read_exact(&mut body).map_err(|e| e.to_string())?;
        serde_json::from_slice(&body)
            .map(Some)
            .map_err(|e| e.to_string())
    } else {
        if first.len() > MAX_MESSAGE_BYTES {
            return Err("mcp_body_exceeds_byte_limit".into());
        }
        serde_json::from_str(first.trim())
            .map(Some)
            .map_err(|e| e.to_string())
    }
}

