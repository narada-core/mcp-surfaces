fn error(code: &str, message: &str) -> Value {
    json!({"schema":"narada.mailbox.error.v1","code":code,"message":message})
}
