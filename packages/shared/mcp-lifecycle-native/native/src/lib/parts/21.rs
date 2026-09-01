fn try_parse_wire(buffer: &mut Vec<u8>) -> io::Result<Option<(Value, bool)>> {
    while matches!(buffer.first(), Some(b'\r' | b'\n' | b' ' | b'\t')) {
        buffer.remove(0);
    }
    if buffer.is_empty() {
        return Ok(None);
    }
    if buffer.starts_with(b"Content-Length:") {
        let Some(end) = buffer.windows(4).position(|w| w == b"\r\n\r\n") else {
            return Ok(None);
        };
        let headers = String::from_utf8_lossy(&buffer[..end]);
        let length = headers
            .lines()
            .find_map(|line| {
                line.split_once(':')
                    .and_then(|(_, v)| v.trim().parse::<usize>().ok())
            })
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "invalid Content-Length"))?;
        let start = end + 4;
        if buffer.len() < start + length {
            return Ok(None);
        };
        let body = buffer[start..start + length].to_vec();
        buffer.drain(..start + length);
        let value = serde_json::from_slice(&body)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid JSON"))?;
        return Ok(Some((value, true)));
    }
    let Some(end) = buffer.iter().position(|b| *b == b'\n') else {
        return Ok(None);
    };
    let line = buffer.drain(..=end).collect::<Vec<_>>();
    let value = serde_json::from_slice(&line)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid JSON"))?;
    Ok(Some((value, false)))
}
fn write_wire<W: Write>(writer: &mut W, value: &Value, framed: bool) -> io::Result<()> {
    let body = serde_json::to_vec(value).unwrap_or_else(|_| b"{}".to_vec());
    if framed {
        write!(writer, "Content-Length: {}\r\n\r\n", body.len())?;
        writer.write_all(&body)?;
    } else {
        writer.write_all(&body)?;
        writer.write_all(b"\n")?;
    }
    writer.flush()
}

include!("../../executability_impl.rs");

include!("../../work_impl.rs");

