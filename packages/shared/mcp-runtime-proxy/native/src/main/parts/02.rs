
fn safe_positive_integer(value: Option<&Value>, maximum: u64) -> bool {
    value.is_some_and(|value| {
        value
            .as_u64()
            .is_some_and(|number| number > 0 && number <= maximum)
            || value.as_f64().is_some_and(|number| {
                number.is_finite()
                    && number.fract() == 0.0
                    && number >= 1.0
                    && number <= maximum as f64
            })
    })
}

fn valid_orientation_coordinate(value: Option<&Value>) -> bool {
    let Some(coordinate) = value.and_then(Value::as_object) else {
        return false;
    };
    let integer_field = contract_string("/coordinate/positive_safe_integer_field");
    let integer_maximum = contract_value("/coordinate/positive_safe_integer_max")
        .as_u64()
        .expect("orientation_entry_enforcement_contract_integer_maximum_invalid");
    contract_string_array("/coordinate/non_empty_string_fields")
        .into_iter()
        .all(|field| non_empty_json_string(coordinate.get(field)))
        && safe_positive_integer(coordinate.get(integer_field), integer_maximum)
}

fn same_orientation_coordinate(left: Option<&Value>, right: Option<&Value>) -> bool {
    let (Some(left), Some(right)) = (
        left.and_then(Value::as_object),
        right.and_then(Value::as_object),
    ) else {
        return false;
    };
    valid_orientation_coordinate(Some(&Value::Object(left.clone())))
        && valid_orientation_coordinate(Some(&Value::Object(right.clone())))
        && contract_string_array("/coordinate/identity_fields")
            .into_iter()
            .all(|field| {
                left.get(field)
                    .zip(right.get(field))
                    .is_some_and(|(left, right)| json_equivalent(left, right))
            })
}

fn rule_set(name: &str) -> &'static Value {
    contract_value("/rule_sets")
        .get(name)
        .unwrap_or_else(|| panic!("orientation_entry_enforcement_contract_rule_set_missing:{name}"))
}

fn rule_pair<'a>(candidate: &'a Value, field: &str) -> (&'a str, &'a str) {
    let pair = candidate.as_array().unwrap_or_else(|| {
        panic!("orientation_entry_enforcement_contract_rule_pair_invalid:{field}")
    });
    assert_eq!(
        pair.len(),
        2,
        "orientation_entry_enforcement_contract_rule_pair_invalid:{field}"
    );
    (
        pair[0].as_str().unwrap_or_else(|| {
            panic!("orientation_entry_enforcement_contract_rule_pair_invalid:{field}")
        }),
        pair[1].as_str().unwrap_or_else(|| {
            panic!("orientation_entry_enforcement_contract_rule_pair_invalid:{field}")
        }),
    )
}

fn validate_rule_set(document: &Value, name: &str) -> bool {
    let rules = rule_set(name);
    let equals = rules
        .get("equals")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("orientation_entry_enforcement_contract_equals_invalid:{name}"));
    for candidate in equals {
        let rule = candidate.as_object().unwrap_or_else(|| {
            panic!("orientation_entry_enforcement_contract_equals_rule_invalid:{name}")
        });
        let path = rule.get("path").and_then(Value::as_str).unwrap_or_else(|| {
            panic!("orientation_entry_enforcement_contract_equals_path_invalid:{name}")
        });
        let Some(actual) = document.pointer(path) else {
            return false;
        };
        let expected = rule.get("value").unwrap_or_else(|| {
            panic!("orientation_entry_enforcement_contract_equals_value_invalid:{name}")
        });
        if !json_equivalent(actual, expected) {
            return false;
        }
    }
    if let Some(paths) = rules.get("non_empty_strings") {
        for path in paths.as_array().unwrap_or_else(|| {
            panic!("orientation_entry_enforcement_contract_string_rules_invalid:{name}")
        }) {
            let path = path.as_str().unwrap_or_else(|| {
                panic!("orientation_entry_enforcement_contract_string_path_invalid:{name}")
            });
            if !non_empty_json_string(document.pointer(path)) {
                return false;
            }
        }
    }
    if let Some(paths) = rules.get("coordinate_paths") {
        for path in paths.as_array().unwrap_or_else(|| {
            panic!("orientation_entry_enforcement_contract_coordinate_rules_invalid:{name}")
        }) {
            let path = path.as_str().unwrap_or_else(|| {
                panic!("orientation_entry_enforcement_contract_coordinate_path_invalid:{name}")
            });
            if !valid_orientation_coordinate(document.pointer(path)) {
                return false;
            }
        }
    }
    if let Some(pairs) = rules.get("equal_paths") {
        for candidate in pairs.as_array().unwrap_or_else(|| {
            panic!("orientation_entry_enforcement_contract_equal_path_rules_invalid:{name}")
        }) {
            let (left_path, right_path) = rule_pair(candidate, "equal_paths");
            let Some((left, right)) = document
                .pointer(left_path)
                .zip(document.pointer(right_path))
            else {
                return false;
            };
            if !json_equivalent(left, right) {
                return false;
            }
        }
    }
    if let Some(pairs) = rules.get("equal_coordinates") {
        for candidate in pairs.as_array().unwrap_or_else(|| {
            panic!("orientation_entry_enforcement_contract_equal_coordinate_rules_invalid:{name}")
        }) {
            let (left_path, right_path) = rule_pair(candidate, "equal_coordinates");
            if !same_orientation_coordinate(
                document.pointer(left_path),
                document.pointer(right_path),
            ) {
                return false;
            }
        }
    }
    true
}

fn blocked_orientation_state(
    entry_file: Option<&Path>,
    reason: &str,
    delivery_receipt_ref: Option<&str>,
) -> Value {
    json!({
        "schema": contract_string("/state/schema"),
        "required": true,
        "status": "blocked",
        "ordinary_work_gate": "acknowledgement_required",
        "reason": reason,
        "delivery_receipt_ref": delivery_receipt_ref,
        "acknowledgement_ref": Value::Null,
        "entry_file": entry_file,
        "next_call": contract_value("/state/next_call"),
    })
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum OrientationRequiredSignal {
    Absent,
    Required,
    NotRequired,
    Invalid,
}

fn orientation_required_signal() -> OrientationRequiredSignal {
    let variable = contract_string("/environment/required_signal");
    let value = match env::var(variable) {
        Ok(value) => value.trim().to_ascii_lowercase(),
        Err(env::VarError::NotPresent) => return OrientationRequiredSignal::Absent,
        Err(env::VarError::NotUnicode(_)) => return OrientationRequiredSignal::Invalid,
    };
    if value.is_empty() {
        return OrientationRequiredSignal::Absent;
    }
    if contract_string_array("/environment/required_values").contains(&value.as_str()) {
        return OrientationRequiredSignal::Required;
    }
    if contract_string_array("/environment/not_required_values").contains(&value.as_str()) {
        return OrientationRequiredSignal::NotRequired;
    }
    OrientationRequiredSignal::Invalid
}

fn orientation_entry_state() -> Value {
    let entry_variable = contract_string("/environment/entry_file");
    let configured = env::var(entry_variable)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let configured_path = configured.as_ref().map(PathBuf::from);
    let entry_file = configured_path.as_ref().map(|path| {
        lexically_normalize_path(&if path.is_absolute() {
            path.clone()
        } else {
            env::current_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .join(path)
        })
    });
    let signal = orientation_required_signal();
    if signal == OrientationRequiredSignal::Invalid {
        return blocked_orientation_state(
            entry_file.as_deref(),
            contract_reason("required_signal_invalid"),
            None,
        );
    }
    if signal == OrientationRequiredSignal::NotRequired && configured.is_some() {
        return blocked_orientation_state(
            entry_file.as_deref(),
            contract_reason("required_signal_conflict"),
            None,
        );
    }
    if signal == OrientationRequiredSignal::Required && configured.is_none() {
        return blocked_orientation_state(None, contract_reason("required_packet_missing"), None);
    }
    let Some(configured_path) = configured_path else {
        return json!({
            "schema": contract_string("/state/schema"),
            "required": false,
            "status": "not_required",
            "ordinary_work_gate": "open",
            "reason": contract_reason("not_supplied"),
            "delivery_receipt_ref": Value::Null,
            "acknowledgement_ref": Value::Null,
            "entry_file": Value::Null,
            "next_call": Value::Null,
        });
    };
    let entry_file = entry_file.expect("orientation_entry_file_resolution_missing");
    if !configured_path.is_absolute() {
        return blocked_orientation_state(
            Some(&entry_file),
            contract_reason("entry_path_invalid"),
            None,
        );
    }
    if !entry_file.exists() {
        return blocked_orientation_state(
            Some(&entry_file),
            contract_reason("entry_unavailable"),
            None,
        );
    }
    let Some(packet) = json_file(&entry_file) else {
        return blocked_orientation_state(
            Some(&entry_file),
            contract_reason("entry_invalid"),
            None,
        );
    };
    if !validate_rule_set(&packet, "packet_header") {
        return blocked_orientation_state(
            Some(&entry_file),
            contract_reason("entry_invalid"),
            None,
        );
    }
    let delivery_ref = packet
        .pointer(contract_string("/readback_paths/delivery_receipt_ref"))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty());
    if !validate_rule_set(&packet, "delivery_binding") {
        return blocked_orientation_state(
            Some(&entry_file),
            contract_reason("delivery_binding_invalid"),
            delivery_ref,
        );
    }
    if !validate_rule_set(&packet, "acknowledgement_projection_ref") {
        return blocked_orientation_state(
            Some(&entry_file),
            contract_reason("acknowledgement_ref_invalid"),
            delivery_ref,
        );
    }
    let Some(relative_path) = packet
        .pointer(contract_string(
            "/readback_paths/acknowledgement_projection_path",
        ))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
    else {
        return blocked_orientation_state(
            Some(&entry_file),
            contract_reason("acknowledgement_ref_invalid"),
            delivery_ref,
        );
    };
    let Some(parent) = entry_file.parent() else {
        return blocked_orientation_state(
            Some(&entry_file),
            contract_reason("acknowledgement_ref_invalid"),
            delivery_ref,
        );
    };
    let acknowledgement_path = lexically_normalize_path(&parent.join(relative_path));
    if acknowledgement_path.parent() != Some(parent) {
        return blocked_orientation_state(
            Some(&entry_file),
            contract_reason("acknowledgement_ref_invalid"),
            delivery_ref,
        );
    }
    if !acknowledgement_path.exists() {
        return blocked_orientation_state(
            Some(&entry_file),
            contract_reason("acknowledgement_required"),
            delivery_ref,
        );
    }
    let Some(acknowledgement) = json_file(&acknowledgement_path) else {
        return blocked_orientation_state(
            Some(&entry_file),
            contract_reason("acknowledgement_invalid"),
            delivery_ref,
        );
    };
    let combined = json!({
        "packet": packet,
        "acknowledgement": acknowledgement,
    });
    if !validate_rule_set(&combined, "acknowledgement_projection") {
        return blocked_orientation_state(
            Some(&entry_file),
            contract_reason("acknowledgement_invalid"),
            delivery_ref,
        );
    }
    let acknowledgement_ref = combined
        .pointer("/acknowledgement")
        .and_then(|value| value.pointer(contract_string("/readback_paths/acknowledgement_ref")))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty());
    let Some(acknowledgement_ref) = acknowledgement_ref else {
        return blocked_orientation_state(
            Some(&entry_file),
            contract_reason("acknowledgement_invalid"),
            delivery_ref,
        );
    };
    json!({
        "schema": contract_string("/state/schema"),
        "required": true,
        "status": "open",
        "ordinary_work_gate": "open",
        "reason": contract_reason("acknowledged"),
        "delivery_receipt_ref": delivery_ref,
        "acknowledgement_ref": acknowledgement_ref,
        "entry_file": entry_file,
        "next_call": Value::Null,
    })
}
