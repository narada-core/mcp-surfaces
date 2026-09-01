
fn grep_match_object(line: &str, mode: &str) -> Value {
    let fields: Vec<&str> = line.split('\u{1f}').collect();
    match mode {
        "count_matches" => {
            json!({"path": fields.first().copied().unwrap_or(line), "count": fields.get(1).and_then(|value| value.parse::<u64>().ok()), "raw": line})
        }
        "content" => {
            json!({"path": fields.first().copied().unwrap_or(line), "line": fields.get(1).and_then(|value| value.parse::<u64>().ok()), "text": fields.get(2).copied().unwrap_or(""), "raw": line})
        }
        _ => json!({"path": line, "raw": line}),
    }
}

fn classify(path: &str) -> &'static str {
    let normalized =
        format!("/{}/", path.replace('\\', "/").trim_matches('/')).to_ascii_lowercase();
    if GENERATED_MARKERS
        .iter()
        .any(|marker| normalized.contains(marker))
    {
        "generated_artifact"
    } else {
        "candidate_source"
    }
}

fn count_lines(path: &Path) -> io::Result<(usize, bool)> {
    let mut file = fs::File::open(path)?;
    let mut buffer = [0_u8; 64 * 1024];
    let mut lines = 0_usize;
    let mut any = false;
    let mut last_newline = false;
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        let chunk = &buffer[..count];
        if chunk.contains(&0) {
            return Ok((0, true));
        }
        any = true;
        lines += chunk.iter().filter(|byte| **byte == b'\n').count();
        last_newline = chunk.last() == Some(&b'\n');
    }
    if any && !last_newline {
        lines += 1;
    }
    Ok((lines, false))
}

fn aggregate_metrics(files: &[Value]) -> Value {
    let mut bytes = 0_u64;
    let mut lines = 0_u64;
    let mut exact = true;
    let mut binary = 0_u64;
    let mut too_large = 0_u64;
    let mut unavailable = 0_u64;
    let mut budget = 0_u64;
    for file in files {
        bytes += file.get("byte_count").and_then(Value::as_u64).unwrap_or(0);
        if let Some(value) = file.get("line_count").and_then(Value::as_u64) {
            lines += value;
        } else {
            exact = false;
            match file
                .get("line_count_status")
                .and_then(Value::as_str)
                .unwrap_or_default()
            {
                "binary" => binary += 1,
                "too_large" => too_large += 1,
                "unavailable" => unavailable += 1,
                "scan_budget_exceeded" => budget += 1,
                _ => {}
            }
        }
    }
    json!({"file_count": files.len(), "byte_count": bytes, "line_count": if exact {json!(lines)} else {Value::Null}, "line_count_status": if exact {"exact"} else {"partial"}, "binary_file_count": binary, "too_large_file_count": too_large, "unavailable_file_count": unavailable, "scan_budget_exceeded_file_count": budget})
}

pub(crate) fn read_message<R: BufRead>(reader: &mut R) -> io::Result<Option<(Value, bool)>> {
    let mut first = String::new();
    loop {
        if reader.read_line(&mut first)? == 0 {
            return Ok(None);
        }
        if !first.trim().is_empty() {
            break;
        }
        first.clear();
    }
    if first.to_ascii_lowercase().starts_with("content-length:") {
        let length = first
            .split(':')
            .nth(1)
            .and_then(|value| value.trim().parse::<usize>().ok())
            .unwrap_or(0);
        let mut header = String::new();
        loop {
            header.clear();
            reader.read_line(&mut header)?;
            if header.trim().is_empty() {
                break;
            }
        }
        let mut body = vec![0_u8; length];
        reader.read_exact(&mut body)?;
        return serde_json::from_slice(&body)
            .map(|value| Some((value, true)))
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error));
    }
    serde_json::from_str(first.trim())
        .map(|value| Some((value, false)))
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

pub(crate) fn write_message<W: Write>(
    writer: &mut W,
    value: &Value,
    framed: bool,
) -> io::Result<()> {
    let body = serde_json::to_string(value)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    if framed {
        write!(writer, "Content-Length: {}\r\n\r\n{}", body.len(), body)
    } else {
        writeln!(writer, "{body}")
    }
}

