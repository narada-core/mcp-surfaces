//! Shared MCP argument helpers: required strings, bounded required objects,
//! optional integers, and bounded pagination.

use serde_json::{json, Map, Value};

use crate::error::ErrorSchema;

/// Required non-empty string argument.
pub fn required(schema: ErrorSchema, args: &Map<String, Value>, key: &str) -> Result<String, Value> {
    args.get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string)
        .ok_or_else(|| {
            schema.error(
                "required_argument_missing",
                &format!("required_argument_missing:{key}"),
                json!({"field":key}),
            )
        })
}

/// Required non-empty object argument bounded to `max_bytes` when encoded.
/// `label` names the argument family in the oversized-envelope message.
pub fn required_object(
    schema: ErrorSchema,
    args: &Map<String, Value>,
    key: &str,
    max_bytes: usize,
    label: &str,
) -> Result<Value, Value> {
    let value = args
        .get(key)
        .filter(|value| value.as_object().is_some_and(|object| !object.is_empty()))
        .cloned()
        .ok_or_else(|| {
            schema.error(
                "required_argument_missing",
                &format!("required_argument_missing:{key}"),
                json!({"field":key}),
            )
        })?;
    let bytes = serde_json::to_vec(&value).map_err(|source| {
        schema.error(
            "json_encode_failed",
            &source.to_string(),
            json!({"field":key}),
        )
    })?;
    if bytes.len() > max_bytes {
        return Err(schema.error(
            "argument_too_large",
            &format!("{label} exceeds the bounded {max_bytes}-byte envelope"),
            json!({"field":key,"bytes":bytes.len(),"max_bytes":max_bytes}),
        ));
    }
    Ok(value)
}

/// Optional unsigned-integer argument with a default.
pub fn optional_u64(
    schema: ErrorSchema,
    args: &Map<String, Value>,
    key: &str,
    default: u64,
) -> Result<u64, Value> {
    match args.get(key) {
        None => Ok(default),
        Some(value) => value.as_u64().ok_or_else(|| {
            schema.error(
                "argument_invalid",
                &format!("{key} must be an unsigned integer"),
                json!({"field":key,"value":value}),
            )
        }),
    }
}

/// Bounded page limit: `limit` in 1..=100, default 100.
pub fn page_limit(schema: ErrorSchema, args: &Map<String, Value>) -> Result<usize, Value> {
    let value = optional_u64(schema, args, "limit", 100)?;
    if !(1..=100).contains(&value) {
        return Err(schema.error(
            "page_limit_invalid",
            "limit must be between 1 and 100",
            json!({"limit":value}),
        ));
    }
    Ok(value as usize)
}

/// Page offset bounded by platform `usize`.
pub fn page_offset(schema: ErrorSchema, args: &Map<String, Value>) -> Result<usize, Value> {
    usize::try_from(optional_u64(schema, args, "offset", 0)?).map_err(|_| {
        schema.error(
            "page_offset_invalid",
            "offset exceeds platform bounds",
            json!({"offset":args.get("offset")}),
        )
    })
}
