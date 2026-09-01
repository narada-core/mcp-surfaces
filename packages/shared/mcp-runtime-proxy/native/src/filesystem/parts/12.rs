
fn parse_patch(text: &str) -> Result<Vec<ParsedPatchFile>, FsError> {
    if text
        .lines()
        .any(|line| line.trim_end() == "*** Begin Patch")
    {
        parse_codex_patch(text)
    } else {
        parse_unified_patch(text)
    }
}

fn parse_codex_patch(text: &str) -> Result<Vec<ParsedPatchFile>, FsError> {
    let lines: Vec<&str> = text.lines().collect();
    let mut files = Vec::new();
    let mut index = lines
        .iter()
        .position(|line| line.trim_end() == "*** Begin Patch")
        .ok_or_else(|| patch_parse_error("patch_begin_marker_missing", 1))?
        + 1;
    while index < lines.len() {
        let line = lines[index];
        if line == "*** End Patch" {
            return Ok(files);
        }
        let (old_path, new_path, delete) =
            if let Some(path) = line.strip_prefix("*** Update File: ") {
                (
                    Some(clean_patch_path(path)?),
                    Some(clean_patch_path(path)?),
                    false,
                )
            } else if let Some(path) = line.strip_prefix("*** Add File: ") {
                (None, Some(clean_patch_path(path)?), false)
            } else if let Some(path) = line.strip_prefix("*** Delete File: ") {
                (Some(clean_patch_path(path)?), None, true)
            } else {
                return Err(patch_parse_error("patch_file_header_invalid", index + 1));
            };
        index += 1;
        let mut move_to = None;
        if index < lines.len() {
            if let Some(path) = lines[index].strip_prefix("*** Move to: ") {
                move_to = Some(clean_patch_path(path)?);
                index += 1;
            }
        }
        let mut hunks = Vec::new();
        let mut current = ParsedPatchHunk {
            old_start: None,
            lines: Vec::new(),
        };
        while index < lines.len() && !lines[index].starts_with("*** ") {
            let item = lines[index];
            if item.starts_with("@@") {
                if !current.lines.is_empty() {
                    hunks.push(current);
                }
                current = ParsedPatchHunk {
                    old_start: parse_hunk_start(item),
                    lines: Vec::new(),
                };
            } else {
                let (kind, body) = match item.as_bytes().first().copied() {
                    Some(b'+') => ('+', &item[1..]),
                    Some(b'-') => ('-', &item[1..]),
                    Some(b' ') => (' ', &item[1..]),
                    _ => (' ', item),
                };
                current.lines.push((kind, body.to_string()));
            }
            index += 1;
        }
        if !current.lines.is_empty() {
            hunks.push(current);
        }
        files.push(ParsedPatchFile {
            old_path,
            new_path,
            move_to,
            delete,
            hunks,
        });
    }
    Err(patch_parse_error("patch_end_marker_missing", lines.len()))
}

fn parse_unified_patch(text: &str) -> Result<Vec<ParsedPatchFile>, FsError> {
    let lines: Vec<&str> = text.lines().collect();
    let mut files = Vec::new();
    let mut index = 0;
    while index < lines.len() {
        if !lines[index].starts_with("--- ") {
            index += 1;
            continue;
        }
        let old_raw = lines[index][4..].split('\t').next().unwrap_or_default();
        index += 1;
        if index >= lines.len() || !lines[index].starts_with("+++ ") {
            return Err(patch_parse_error(
                "patch_new_file_header_missing",
                index + 1,
            ));
        }
        let new_raw = lines[index][4..].split('\t').next().unwrap_or_default();
        index += 1;
        let old_path = if old_raw == "/dev/null" {
            None
        } else {
            Some(clean_patch_path(old_raw)?)
        };
        let new_path = if new_raw == "/dev/null" {
            None
        } else {
            Some(clean_patch_path(new_raw)?)
        };
        let delete = new_path.is_none();
        let mut hunks = Vec::new();
        while index < lines.len() && !lines[index].starts_with("--- ") {
            if !lines[index].starts_with("@@") {
                index += 1;
                continue;
            }
            let mut hunk = ParsedPatchHunk {
                old_start: parse_hunk_start(lines[index]),
                lines: Vec::new(),
            };
            index += 1;
            while index < lines.len()
                && !lines[index].starts_with("@@")
                && !lines[index].starts_with("--- ")
            {
                let item = lines[index];
                if item == "\\ No newline at end of file" {
                    index += 1;
                    continue;
                }
                let Some(prefix) = item.as_bytes().first().copied() else {
                    return Err(patch_parse_error("patch_hunk_line_invalid", index + 1));
                };
                if !matches!(prefix, b' ' | b'+' | b'-') {
                    return Err(patch_parse_error("patch_hunk_line_invalid", index + 1));
                }
                hunk.lines.push((prefix as char, item[1..].to_string()));
                index += 1;
            }
            hunks.push(hunk);
        }
        files.push(ParsedPatchFile {
            old_path,
            new_path,
            move_to: None,
            delete,
            hunks,
        });
    }
    Ok(files)
}

fn clean_patch_path(value: &str) -> Result<String, FsError> {
    let mut path = value.trim().replace('\\', "/");
    if path.starts_with("a/") || path.starts_with("b/") {
        path = path[2..].to_string();
    }
    if path.is_empty() || path == "/dev/null" || path.split('/').any(|part| part == "..") {
        return Err(FsError::new(
            "patch_path_invalid",
            "patch_path_invalid",
            json!({"path":value}),
        ));
    }
    Ok(path)
}

fn parse_hunk_start(header: &str) -> Option<usize> {
    let value = header
        .split_whitespace()
        .find(|part| part.starts_with('-'))?
        .trim_start_matches('-');
    value.split(',').next()?.parse().ok()
}

fn apply_patch_content(
    before: &[u8],
    hunks: &[ParsedPatchHunk],
    deleting: bool,
) -> Result<Vec<u8>, FsError> {
    let text = std::str::from_utf8(before)
        .map_err(|_| FsError::new("patch_source_not_utf8", "patch_source_not_utf8", json!({})))?;
    let newline = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let trailing = text.ends_with('\n');
    let mut lines: Vec<String> = if text.is_empty() {
        Vec::new()
    } else {
        text.trim_end_matches(['\r', '\n'])
            .split(['\n'])
            .map(|line| line.trim_end_matches('\r').to_string())
            .collect()
    };
    let mut delta: isize = 0;
    let mut cursor = 0usize;
    for hunk in hunks {
        let old: Vec<&str> = hunk
            .lines
            .iter()
            .filter(|(kind, _)| *kind != '+')
            .map(|(_, line)| line.as_str())
            .collect();
        let replacement: Vec<String> = hunk
            .lines
            .iter()
            .filter(|(kind, _)| *kind != '-')
            .map(|(_, line)| line.clone())
            .collect();
        let position = if let Some(start) = hunk.old_start {
            (start.saturating_sub(1) as isize + delta).max(0) as usize
        } else {
            find_patch_context(&lines, &old, cursor).ok_or_else(|| {
                FsError::new(
                    "patch_context_not_found",
                    "patch_context_not_found",
                    json!({"context":old}),
                )
            })?
        };
        if position + old.len() > lines.len()
            || lines[position..position + old.len()]
                .iter()
                .map(String::as_str)
                .ne(old.iter().copied())
        {
            return Err(FsError::new(
                "patch_context_mismatch",
                "patch_context_mismatch",
                json!({"line":position+1,"expected":old}),
            ));
        }
        lines.splice(position..position + old.len(), replacement.clone());
        delta += replacement.len() as isize - old.len() as isize;
        cursor = position + replacement.len();
    }
    if deleting {
        return Ok(Vec::new());
    }
    let mut output = lines.join(newline);
    if trailing || (!hunks.is_empty() && before.is_empty()) {
        output.push_str(newline);
    }
    Ok(output.into_bytes())
}
