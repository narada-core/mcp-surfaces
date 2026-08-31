use super::*;

impl<R: Read> WireReader<R> {
    pub(crate) fn new(reader: R, max_message_bytes: usize) -> Self {
        Self {
            reader,
            buffer: Vec::new(),
            eof: false,
            max_message_bytes,
        }
    }

    pub(crate) fn next(&mut self) -> io::Result<Option<(Value, bool)>> {
        loop {
            if let Some(message) = try_parse_wire(&mut self.buffer, self.max_message_bytes)? {
                return Ok(Some(message));
            }
            if self.buffer.len() > self.max_message_bytes + MAX_WIRE_HEADER_BYTES {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "MCP message exceeds configured byte limit",
                ));
            }
            if self.eof {
                if self.buffer.iter().all(|byte| byte.is_ascii_whitespace()) {
                    self.buffer.clear();
                    return Ok(None);
                }
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "incomplete MCP message",
                ));
            }
            let mut chunk = [0_u8; 8192];
            let count = self.reader.read(&mut chunk)?;
            if count == 0 {
                self.eof = true;
            } else {
                self.buffer.extend_from_slice(&chunk[..count]);
            }
        }
    }
}

pub(crate) fn try_parse_wire(
    buffer: &mut Vec<u8>,
    max_message_bytes: usize,
) -> io::Result<Option<(Value, bool)>> {
    if buffer.len() > max_message_bytes.saturating_add(MAX_WIRE_HEADER_BYTES) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "MCP message exceeds configured byte limit",
        ));
    }
    let whitespace = buffer
        .iter()
        .position(|byte| !byte.is_ascii_whitespace())
        .unwrap_or(buffer.len());
    if whitespace > 0 {
        buffer.drain(..whitespace);
    }
    if buffer.is_empty() {
        return Ok(None);
    }
    if buffer.len() >= 15 && buffer[..15].eq_ignore_ascii_case(b"content-length:") {
        let (header_end, separator_len) = match find_header_end(buffer) {
            Some(found) => found,
            None if buffer.len() > MAX_WIRE_HEADER_BYTES => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "MCP headers exceed configured byte limit",
                ))
            }
            None => return Ok(None),
        };
        if header_end > MAX_WIRE_HEADER_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "MCP headers exceed configured byte limit",
            ));
        }
        let header = String::from_utf8_lossy(&buffer[..header_end]);
        let length = header
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                if name.trim().eq_ignore_ascii_case("content-length") {
                    value.trim().parse::<usize>().ok()
                } else {
                    None
                }
            })
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing Content-Length"))?;
        if length > max_message_bytes {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "MCP body exceeds configured byte limit",
            ));
        }
        let body_start = header_end + separator_len;
        let body_end = body_start.saturating_add(length);
        if buffer.len() < body_end {
            return Ok(None);
        }
        let value = serde_json::from_slice::<Value>(&buffer[body_start..body_end])
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?;
        buffer.drain(..body_end);
        return Ok(Some((value, true)));
    }
    let newline = match buffer.iter().position(|byte| *byte == b'\n') {
        Some(position) => position,
        None if buffer.len() > max_message_bytes => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "MCP JSONL message exceeds configured byte limit",
            ))
        }
        None => return Ok(None),
    };
    if newline > max_message_bytes {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "MCP JSONL message exceeds configured byte limit",
        ));
    }
    let mut line = buffer.drain(..=newline).collect::<Vec<_>>();
    if line.last() == Some(&b'\n') {
        line.pop();
    }
    if line.last() == Some(&b'\r') {
        line.pop();
    }
    if line.iter().all(|byte| byte.is_ascii_whitespace()) {
        return Ok(None);
    }
    let value = serde_json::from_slice::<Value>(&line)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?;
    Ok(Some((value, false)))
}

pub(crate) fn find_header_end(buffer: &[u8]) -> Option<(usize, usize)> {
    if let Some(position) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
        return Some((position, 4));
    }
    buffer
        .windows(2)
        .position(|window| window == b"\n\n")
        .map(|position| (position, 2))
}

pub(crate) fn write_wire<W: Write>(writer: &mut W, value: &Value, framed: bool) -> io::Result<()> {
    let body = serde_json::to_vec(value)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?;
    if framed {
        write!(writer, "Content-Length: {}\r\n\r\n", body.len())?;
        writer.write_all(&body)?;
    } else {
        writer.write_all(&body)?;
        writer.write_all(b"\n")?;
    }
    writer.flush()
}
