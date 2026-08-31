use crate::full::*;

#[test]
fn child_error_is_projected_once_as_domain_diagnostic() {
    let diagnostic = child_error_diagnostic(&json!({
        "code": -32000,
        "message": "git_push_head_mismatch",
        "data": {
            "code": "git_push_head_mismatch",
            "details": {"expected_commit": "a", "actual_head": "b"}
        }
    }));
    assert_eq!(diagnostic.code, "git_push_head_mismatch");
    assert_eq!(diagnostic.message, "git_push_head_mismatch");
    assert_eq!(diagnostic.details["child_jsonrpc_code"], -32000);
    assert_eq!(diagnostic.details["child_details"]["actual_head"], "b");
    assert!(diagnostic.details.get("child_error").is_none());
    let merged = request_error_details(&diagnostic.details, "tools/call", 120_000);
    assert_eq!(merged["child_details"]["expected_commit"], "a");
    assert_eq!(merged["method"], "tools/call");
    assert_eq!(merged["timeout_ms"], 120_000);
}

#[test]
fn proxy_child_args_require_separator() {
    assert_eq!(extract_proxy_child_args(&["proxy".to_string()]), None);
    assert_eq!(
        extract_proxy_child_args(&[
            "proxy".to_string(),
            "--".to_string(),
            "--site-root".to_string()
        ]),
        Some(vec!["--site-root".to_string()]),
    );
}
