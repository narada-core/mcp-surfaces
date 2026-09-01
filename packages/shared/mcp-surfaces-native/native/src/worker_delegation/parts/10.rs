fn error(code: &str, message: &str) -> Value {
    json!({"schema":"narada.worker.error.v1","code":code,"message":message})
}
fn input_schema(name: &str) -> Value {
    let short_string = || json!({"type":"string","minLength":1,"maxLength":512});
    let run_id =
        || json!({"type":"string","minLength":5,"maxLength":160,"pattern":"^run-[A-Za-z0-9_-]+$"});
    let run_ids = || json!({"type":"array","minItems":1,"maxItems":50,"items":run_id()});
    let intent = || {
        json!({
            "type":"object",
            "properties":{
                "instruction":{"type":"string","minLength":1,"maxLength":65536},
                "task":{"type":"string","minLength":1,"maxLength":65536},
                "goal":{"type":"string","minLength":1,"maxLength":65536},
                "summary":{"type":"string","minLength":1,"maxLength":65536},
                "mode":short_string()
            },
            "additionalProperties":false,
            "anyOf":[{"required":["instruction"]},{"required":["task"]},{"required":["goal"]},{"required":["summary"]}]
        })
    };
    let constraints = || {
        json!({
            "type":"object",
            "properties":{
                "authority":{"type":"string","enum":["read","write","command"]},
                "cognition":{"type":"string","enum":["low","medium","high"],"default":"low"},
                "cwd":{"type":"string","minLength":1,"maxLength":4096},
                "preflight_paths":{"type":"array","maxItems":64,"items":{"type":"object","properties":{"path":{"type":"string","minLength":1,"maxLength":4096},"access":{"type":"string","enum":["read","write","create"],"default":"read"}},"required":["path"],"additionalProperties":false}},
                "invocation_plan_ref":{"type":"string","minLength":6,"maxLength":512,"pattern":"^plan:[A-Za-z0-9._:-]+$"},
                "max_run_ms":{"type":"integer","minimum":1,"maximum":1800000,"default":300000,"description":"Hard worker runtime deadline enforced by the native authority."},
                "queue_timeout_ms":{"type":"integer","minimum":1,"maximum":1800000,"default":300000,"description":"Bounded provider-admission wait. This clock is separate from max_run_ms, which begins only after admission."},
                "wait_for_completion":{"type":"boolean","default":false,"description":"Return after bounded child completion polling when true; false returns the accepted running record immediately."},
                "wait_timeout_ms":{"type":"integer","minimum":0,"maximum":100000,"default":30000,"description":"Maximum transport-safe inline completion wait when wait_for_completion is true. Longer work remains durable and must be recovered with worker_run_wait or worker_run_status."}
            },
            "additionalProperties":false
        })
    };
    let run_request = || {
        json!({
            "type":"object",
            "properties":{"intent":intent(),"constraints":constraints()},
            "required":["intent"],
            "additionalProperties":false
        })
    };
    match name {
        "worker_guidance" => {
            json!({"type":"object","properties":{"workflow":short_string(),"tool":short_string()},"additionalProperties":false})
        }
        "worker_policy_inspect"
        | "worker_cognition_defaults_inspect"
        | "worker_operator_affordances" => json!({"type":"object","additionalProperties":false}),
        "worker_config_resolve" => {
            json!({"type":"object","properties":{"cwd":{"type":"string","minLength":1,"maxLength":4096},"constraints":constraints()},"additionalProperties":false})
        }
        "worker_run_status" => {
            json!({"type":"object","properties":{"run_id":run_id(),"compact":{"type":"boolean","default":true}},"required":["run_id"],"additionalProperties":false})
        }
        "worker_run_wait" => {
            json!({"type":"object","properties":{"run_id":run_id(),"compact":{"type":"boolean","default":true},"timeout_ms":{"type":"integer","minimum":0,"maximum":100000,"default":30000,"description":"Maximum transport-safe bounded state-file polling interval. Repeat this read-only call for longer work."}},"required":["run_id"],"additionalProperties":false})
        }
        "worker_runs_list" => {
            json!({"type":"object","properties":{"limit":{"type":"integer","minimum":1,"maximum":200},"compact":{"type":"boolean","default":true},"site_scope":{"type":"string","enum":["current_site"],"default":"current_site","description":"Runs are filtered by the server-bound Site root; caller-supplied cross-site identity is not accepted."},"include_running":{"type":"boolean"},"include_completed":{"type":"boolean"}},"additionalProperties":false})
        }
        "worker_run_wait_batch" => {
            json!({"type":"object","properties":{"run_ids":run_ids(),"compact":{"type":"boolean","default":true},"timeout_ms":{"type":"integer","minimum":0,"maximum":180000,"default":30000},"poll_ms":{"type":"integer","minimum":100,"maximum":30000,"default":5000}},"required":["run_ids"],"additionalProperties":false})
        }
        "worker_runs_synthesize" => {
            json!({"type":"object","properties":{"run_ids":run_ids()},"required":["run_ids"],"additionalProperties":false})
        }
        "worker_dashboard_describe" => {
            json!({"type":"object","properties":{"mode":{"type":"string","enum":["all_active","single_run"]},"run_id":run_id(),"include_terminal":{"type":"boolean"},"limit":{"type":"integer","minimum":1,"maximum":200}},"additionalProperties":false})
        }
        "worker_output_show" => {
            json!({"type":"object","properties":{"ref":{"type":"string","minLength":1,"maxLength":512},"output_ref":{"type":"string","minLength":1,"maxLength":512},"offset":{"type":"integer","minimum":0,"maximum":256000},"limit":{"type":"integer","minimum":1,"maximum":256000}},"anyOf":[{"required":["ref"]},{"required":["output_ref"]}],"additionalProperties":false})
        }
        "worker_result_show" => {
            json!({"type":"object","properties":{"run_id":run_id(),"offset":{"type":"integer","minimum":0,"maximum":256000},"limit":{"type":"integer","minimum":1,"maximum":256000}},"required":["run_id"],"additionalProperties":false})
        }
        "worker_cognition_defaults_update" => {
            json!({"type":"object","properties":{"provider":short_string(),"cognition":{"type":"string","enum":["low","medium","high"]},"model":short_string(),"reasoning_effort":short_string(),"actor":short_string()},"required":["provider","cognition","model","reasoning_effort"],"additionalProperties":false})
        }
        "worker_run" => run_request(),
        "worker_edit" => {
            json!({"type":"object","properties":{"instruction":{"type":"string","minLength":1,"maxLength":65536},"cwd":{"type":"string","minLength":1,"maxLength":4096},"invocation_plan_ref":{"type":"string","minLength":6,"maxLength":512,"pattern":"^plan:[A-Za-z0-9._:-]+$"},"constraints":constraints()},"required":["instruction"],"additionalProperties":false})
        }
        "worker_resume" => {
            json!({"type":"object","properties":{"worker_session_id":{"type":"string","minLength":1,"maxLength":512},"intent":intent(),"constraints":constraints()},"required":["worker_session_id","intent"],"additionalProperties":false})
        }
        "worker_run_reap" => {
            json!({"type":"object","properties":{"run_id":run_id(),"reason":{"type":"string","minLength":1,"maxLength":2048},"force":{"type":"boolean"}},"required":["run_id","reason","force"],"additionalProperties":false})
        }
        "worker_run_batch" => {
            json!({"type":"object","properties":{"requests":{"type":"array","minItems":1,"maxItems":50,"items":run_request()}},"required":["requests"],"additionalProperties":false})
        }
        "worker_command_run" => {
            json!({"type":"object","properties":{"authority":{"type":"string","const":"command"},"command":{"type":"string","minLength":1,"maxLength":512},"args":{"type":"array","maxItems":64,"items":{"type":"string","maxLength":4096}},"cwd":{"type":"string","minLength":1,"maxLength":4096},"timeout_ms":{"type":"integer","minimum":1,"maximum":60000,"default":10000},"stdout_limit":{"type":"integer","minimum":1,"maximum":65536,"default":4096},"stderr_limit":{"type":"integer","minimum":1,"maximum":65536,"default":4096}},"required":["authority","command"],"additionalProperties":false})
        }
        _ => json!({"type":"object","additionalProperties":false}),
    }
}
fn tool(name: &str, description: &str, schema: Value, read_only: bool) -> Value {
    json!({"name":name,"description":description,"annotations":{"title":name,"readOnlyHint":read_only,"destructiveHint":!read_only,"idempotentHint":read_only,"openWorldHint":false},"inputSchema":schema,"outputSchema":{"type":"object","additionalProperties":true}})
}
fn command_tool(name: &str, description: &str, schema: Value) -> Value {
    json!({"name":name,"description":description,"annotations":{"title":name,"readOnlyHint":false,"destructiveHint":false,"idempotentHint":false,"openWorldHint":false},"inputSchema":schema,"outputSchema":{"type":"object","additionalProperties":true}})
}

