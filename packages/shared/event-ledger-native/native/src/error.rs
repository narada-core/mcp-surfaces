//! Structured refusal envelopes. The envelope schema string is owned by the
//! consuming surface (for example `narada.epistemic.error.v1`); the crate only
//! standardizes the `{schema, code, message, details}` shape.

use serde_json::{json, Value};

/// Error-envelope schema bound to one consuming surface.
#[derive(Clone, Copy, Debug)]
pub struct ErrorSchema(pub &'static str);

impl ErrorSchema {
    /// Build one refusal envelope.
    pub fn error(self, code: &str, message: &str, details: Value) -> Value {
        error(self.0, code, message, details)
    }

    /// Adapt an `std::io::Error` into a refusal envelope with a fixed code.
    pub fn io_error(self, code: &'static str) -> impl FnOnce(std::io::Error) -> Value {
        io_error(self.0, code)
    }

    /// Adapt a `rusqlite::Error` into a refusal envelope with a fixed code.
    pub fn db_error(self, code: &'static str) -> impl FnOnce(rusqlite::Error) -> Value {
        db_error(self.0, code)
    }
}

/// Build one refusal envelope: `{"schema", "code", "message", "details"}`.
pub fn error(schema: &str, code: &str, message: &str, details: Value) -> Value {
    json!({"schema":schema,"code":code,"message":message,"details":details})
}

/// Adapt an `std::io::Error` into a refusal envelope with a fixed code.
pub fn io_error(schema: &'static str, code: &'static str) -> impl FnOnce(std::io::Error) -> Value {
    move |source| error(schema, code, &source.to_string(), Value::Null)
}

/// Adapt a `rusqlite::Error` into a refusal envelope with a fixed code.
pub fn db_error(schema: &'static str, code: &'static str) -> impl FnOnce(rusqlite::Error) -> Value {
    move |source| error(schema, code, &source.to_string(), Value::Null)
}
