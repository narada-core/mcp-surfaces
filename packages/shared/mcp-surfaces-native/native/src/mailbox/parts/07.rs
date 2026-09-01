fn fact_folder_refs(fact: &MailFact) -> Vec<String> {
    let mut refs = HashSet::new();
    if let Some((_, Some(payload))) = fact_event(fact) {
        refs.extend(string_set(payload.get("folder_refs")));
        if let Some(graph) = payload
            .get("source_extensions")
            .and_then(|value| value.get("namespaces"))
            .and_then(|value| value.get("graph"))
            .and_then(Value::as_object)
        {
            for key in ["parent_folder_id", "queried_folder_ref"] {
                if let Some(value) = graph
                    .get(key)
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                {
                    refs.insert(value.to_ascii_lowercase());
                }
            }
        }
    }
    let mut values = refs.into_iter().collect::<Vec<_>>();
    values.sort();
    values
}

fn email_from(value: Option<&Value>) -> Option<String> {
    match value {
        Some(Value::String(value)) if value.contains('@') => Some(value.trim().to_ascii_lowercase()),
        Some(Value::Object(value)) => value
            .get("email")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| value.contains('@'))
            .map(|value| value.to_ascii_lowercase()),
        _ => None,
    }
}

fn fact_sender_email(fact: &MailFact) -> Option<String> {
    let (event, payload) = fact_event(fact)?;
    email_from(event.get("from").or_else(|| payload.and_then(|payload| payload.get("from"))))
        .or_else(|| {
            email_from(
                event
                    .get("sender")
                    .or_else(|| payload.and_then(|payload| payload.get("sender"))),
            )
        })
}

fn participant_emails(fact: &MailFact, fields: &[String]) -> HashSet<String> {
    let requested = if fields.iter().any(|field| field == "any_participant") {
        ["from", "sender", "to", "cc", "bcc"]
            .into_iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
    } else {
        fields.to_vec()
    };
    let mut emails = HashSet::new();
    let Some((event, payload)) = fact_event(fact) else {
        return emails;
    };
    for field in requested {
        for value in [event.get(&field), payload.and_then(|payload| payload.get(&field))]
            .into_iter()
            .flatten()
        {
            if let Some(values) = value.as_array() {
                for value in values {
                    if let Some(email) = email_from(Some(value)) {
                        emails.insert(email);
                    }
                }
            } else if let Some(email) = email_from(Some(value)) {
                emails.insert(email);
            }
        }
    }
    emails
}

enum PredicateMatch {
    Yes,
    No,
    Unknown,
}

fn predicate_match(fact: &MailFact, predicate: &Value) -> PredicateMatch {
    let Some(predicate) = predicate.as_object() else {
        return PredicateMatch::No;
    };
    if predicate.get("kind").and_then(Value::as_str) != Some("participant") {
        return PredicateMatch::No;
    }
    let fields = predicate
        .get("fields")
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .map(ToString::to_string)
                .collect::<Vec<_>>()
        })
        .filter(|values| !values.is_empty())
        .unwrap_or_else(|| vec!["any_participant".to_string()]);
    let emails = participant_emails(fact, &fields);
    if emails.is_empty() {
        return PredicateMatch::Unknown;
    }
    let addresses = string_set(predicate.get("addresses"));
    let domains = string_set(predicate.get("domains"));
    if addresses.is_empty() && domains.is_empty() {
        return PredicateMatch::Yes;
    }
    if emails.iter().any(|email| {
        addresses.contains(email)
            || email
                .rsplit_once('@')
                .is_some_and(|(_, domain)| domains.contains(&domain.to_ascii_lowercase()))
    }) {
        PredicateMatch::Yes
    } else {
        PredicateMatch::No
    }
}

fn evaluate_admission(fact: &MailFact, admission: &Value) -> AdmissionEvaluation {
    let folder_refs = fact_folder_refs(fact);
    let sender_email = fact_sender_email(fact);
    let decision = |admitted, reason| AdmissionEvaluation {
        admitted,
        reason,
        folder_refs: folder_refs.clone(),
        sender_email: sender_email.clone(),
    };
    if fact.fact_type != "mail.message.discovered" {
        return decision(true, "not_subject_to_new_message_policy");
    }
    let Some(admission) = admission.as_object() else {
        return decision(true, "no_policy_restrictions");
    };
    if admission.is_empty() {
        return decision(true, "no_policy_restrictions");
    }
    let included_folders = string_set(admission.get("included_folder_refs"));
    let excluded_folders = string_set(admission.get("excluded_folder_refs"));
    if folder_refs.iter().any(|value| excluded_folders.contains(value)) {
        return decision(false, "excluded_folder");
    }
    if !included_folders.is_empty()
        && !folder_refs.iter().any(|value| included_folders.contains(value))
    {
        return decision(false, "included_folder_not_matched");
    }
    if let Some(predicates) = admission.get("predicates").and_then(Value::as_object) {
        let unknown_admitted = predicates
            .get("unknown_participant_behavior")
            .or_else(|| admission.get("unknown_sender_behavior"))
            .and_then(Value::as_str)
            == Some("admit");
        if let Some(excluded) = predicates.get("exclude").and_then(Value::as_array) {
            if excluded.iter().any(|predicate| {
                matches!(predicate_match(fact, predicate), PredicateMatch::Yes)
                    || (unknown_admitted
                        && matches!(predicate_match(fact, predicate), PredicateMatch::Unknown))
            }) {
                return decision(false, "excluded_predicate");
            }
        }
        if let Some(included) = predicates
            .get("include")
            .and_then(Value::as_array)
            .filter(|values| !values.is_empty())
        {
            let mut saw_unknown = false;
            let mut matched = false;
            for predicate in included {
                match predicate_match(fact, predicate) {
                    PredicateMatch::Yes => matched = true,
                    PredicateMatch::Unknown => saw_unknown = true,
                    PredicateMatch::No => {}
                }
            }
            if !matched && !(saw_unknown && unknown_admitted) {
                return decision(false, "included_predicate_not_matched");
            }
        }
    }
    let addresses = string_set(admission.get("allowed_sender_addresses"));
    let domains = string_set(admission.get("allowed_sender_domains"));
    if addresses.is_empty() && domains.is_empty() {
        return decision(true, "admitted");
    }
    let Some(sender) = sender_email.as_deref() else {
        return decision(
            admission.get("unknown_sender_behavior").and_then(Value::as_str) == Some("admit"),
            if admission.get("unknown_sender_behavior").and_then(Value::as_str) == Some("admit") {
                "admitted"
            } else {
                "sender_unknown_rejected"
            },
        );
    };
    let domain = sender.rsplit_once('@').map(|(_, domain)| domain.to_ascii_lowercase());
    if addresses.contains(sender) || domain.as_ref().is_some_and(|domain| domains.contains(domain)) {
        decision(true, "admitted")
    } else {
        decision(false, "sender_not_allowed")
    }
}

fn admission_show(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let scope_id = required_bounded(args, "scope_id", "mailbox_admission_scope_id_required", 256)?;
    let fact_id = required_bounded(args, "fact_id", "mailbox_admission_fact_id_required", 256)?;
    let Some(db) = open_domain_db(root)? else {
        return Ok(json!({"schema":"narada.mailbox.admission_show.v1","status":"not_found","scope_id":scope_id,"fact_id":fact_id}));
    };
    let decision_json: Option<String> = db
        .query_row(
            "SELECT decision_json FROM mailbox_admission_receipts WHERE scope_id=? AND fact_id=?",
            params![scope_id, fact_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(|e| error("mailbox_admission_query_failed", &e.to_string()))?;
    let Some(decision_json) = decision_json else {
        return Ok(json!({"schema":"narada.mailbox.admission_show.v1","status":"not_found","scope_id":scope_id,"fact_id":fact_id}));
    };
    let admission = serde_json::from_str::<Value>(&decision_json)
        .map_err(|e| error("mailbox_admission_receipt_invalid", &e.to_string()))?;
    Ok(json!({
        "schema":"narada.mailbox.admission_show.v1",
        "status":"ok",
        "scope_id":scope_id,
        "fact_id":fact_id,
        "admission":admission
    }))
}

