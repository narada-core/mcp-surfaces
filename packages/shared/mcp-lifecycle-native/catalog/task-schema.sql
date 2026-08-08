
      begin;

      create table if not exists task_lifecycle (
        task_id text primary key,
        task_number integer not null unique,
        status text not null,
        governed_by text,
        closed_at text,
        closed_by text,
        closure_mode text,
        relative_priority integer default 0,
        priority_reason text,
        reopened_at text,
        reopened_by text,
        continuation_packet_json text,
        updated_at text not null
      );

      create index if not exists idx_task_lifecycle_status
        on task_lifecycle(status);

      create table if not exists task_assignments (
        assignment_id text primary key,
        task_id text not null,
        agent_id text not null,
        agent_identity_ref_json text,
        claimed_at text not null,
        released_at text,
        release_reason text,
        intent text not null,
        foreign key (task_id) references task_lifecycle(task_id)
      );

      create index if not exists idx_task_assignments_task_id
        on task_assignments(task_id);

      create table if not exists assignment_intents (
        request_id text primary key,
        kind text not null,
        task_id text,
        task_number integer not null,
        agent_id text not null,
        requested_by text not null,
        requested_at text not null,
        reason text,
        no_claim integer not null default 0,
        status text not null,
        rejection_reason text,
        assignment_id text,
        previous_agent_id text,
        lifecycle_status_before text,
        lifecycle_status_after text,
        roster_status_after text,
        confirmation_json text,
        warnings_json text,
        updated_at text not null
      );

      create index if not exists idx_assignment_intents_task_id
        on assignment_intents(task_id);

      create index if not exists idx_assignment_intents_requested_at
        on assignment_intents(requested_at);

      create table if not exists evidence_bundles (
        bundle_id text primary key,
        task_id text not null,
        task_number integer not null,
        report_ids_json text not null,
        verification_run_ids_json text not null,
        acceptance_criteria_json text not null,
        review_ids_json text not null,
        changed_files_json text not null,
        residuals_json text not null,
        assembled_at text not null,
        assembled_by text not null,
        foreign key (task_id) references task_lifecycle(task_id)
      );

      create index if not exists idx_evidence_bundles_task_id
        on evidence_bundles(task_id);

      create table if not exists evidence_admission_results (
        admission_id text primary key,
        bundle_id text not null,
        task_id text not null,
        task_number integer not null,
        verdict text not null,
        methods_json text not null,
        blockers_json text not null,
        lifecycle_eligible_status text,
        admitted_at text not null,
        admitted_by text not null,
        confirmation_json text not null,
        foreign key (bundle_id) references evidence_bundles(bundle_id),
        foreign key (task_id) references task_lifecycle(task_id)
      );

      create index if not exists idx_evidence_admission_results_task_id
        on evidence_admission_results(task_id);

      create table if not exists criteria_proofs (
        proof_id text primary key,
        task_id text not null,
        task_number integer not null,
        proved_by text not null,
        proved_at text not null,
        criteria_json text not null,
        verification_binding_json text not null,
        foreign key (task_id) references task_lifecycle(task_id)
      );

      create index if not exists idx_criteria_proofs_task_id
        on criteria_proofs(task_id);

      create index if not exists idx_evidence_admission_results_admitted_at
        on evidence_admission_results(admitted_at);

      create table if not exists observation_artifacts (
        artifact_id text primary key,
        artifact_type text not null,
        source_operator text not null,
        task_id text,
        task_number integer,
        agent_id text,
        artifact_uri text not null,
        digest text not null,
        admitted_view_json text not null,
        created_at text not null
      );

      create index if not exists idx_observation_artifacts_created_at
        on observation_artifacts(created_at);

      create index if not exists idx_observation_artifacts_source_operator
        on observation_artifacts(source_operator);

      create table if not exists reconciliation_findings (
        finding_id text primary key,
        task_id text,
        task_number integer,
        surfaces_json text not null,
        expected_authority text not null,
        observed_mismatch_json text not null,
        severity text not null,
        proposed_repair_json text not null,
        status text not null,
        detected_at text not null
      );

      create index if not exists idx_reconciliation_findings_status
        on reconciliation_findings(status);

      create table if not exists reconciliation_repairs (
        repair_id text primary key,
        finding_id text not null,
        applied integer not null,
        changed_surfaces_json text not null,
        before_json text not null,
        after_json text not null,
        verification_json text not null,
        repaired_at text not null,
        repaired_by text not null,
        foreign key (finding_id) references reconciliation_findings(finding_id)
      );

      create table if not exists task_reports (
        report_id text primary key,
        task_id text not null,
        agent_id text not null,
        agent_identity_ref_json text,
        summary text not null,
        changed_files_json text,
        verification_json text,
        directive_id text,
        submitted_at text not null,
        foreign key (task_id) references task_lifecycle(task_id)
      );

      create index if not exists idx_task_reports_task_id
        on task_reports(task_id);

      create table if not exists task_report_records (
        report_id text primary key,
        task_id text not null,
        assignment_id text not null,
        agent_id text not null,
        agent_identity_ref_json text,
        reported_at text not null,
        report_json text not null,
        foreign key (task_id) references task_lifecycle(task_id)
      );

      create index if not exists idx_task_report_records_task_id
        on task_report_records(task_id);

      create table if not exists task_promotion_records (
        promotion_id text primary key,
        task_id text not null,
        task_number integer,
        agent_id text not null,
        requested_by text not null,
        requested_at text not null,
        status text not null,
        promotion_json text not null,
        foreign key (task_id) references task_lifecycle(task_id)
      );

      create index if not exists idx_task_promotion_records_task_id
        on task_promotion_records(task_id);

      create index if not exists idx_task_promotion_records_requested_at
        on task_promotion_records(requested_at);

      create table if not exists task_reviews (
        review_id text primary key,
        task_id text not null,
        reviewer_agent_id text not null,
        verdict text not null,
        findings_json text,
        reviewed_at text not null,
        foreign key (task_id) references task_lifecycle(task_id)
      );

      create index if not exists idx_task_reviews_task_id
        on task_reviews(task_id);

      create table if not exists task_dependencies (
        dependency_id text primary key,
        parent_task_id text not null,
        required_task_id text not null,
        kind text not null,
        satisfying_outcomes_json text not null,
        status text not null,
        created_by text not null,
        created_at text not null,
        foreign key (parent_task_id) references task_lifecycle(task_id),
        foreign key (required_task_id) references task_lifecycle(task_id)
      );

      create index if not exists idx_task_dependencies_parent
        on task_dependencies(parent_task_id, status);

      create index if not exists idx_task_dependencies_required
        on task_dependencies(required_task_id, status);

      create table if not exists task_outcome_contracts (
        contract_id text primary key,
        task_id text not null,
        outcome_type text not null,
        allowed_outcomes_json text not null,
        satisfying_outcomes_json text not null,
        blocking_outcomes_json text not null,
        required_fields_json text not null,
        capability_requirement text,
        created_by text not null,
        created_at text not null,
        foreign key (task_id) references task_lifecycle(task_id)
      );

      create index if not exists idx_task_outcome_contracts_task
        on task_outcome_contracts(task_id, created_at desc);

      create table if not exists task_outcomes (
        outcome_id text primary key,
        task_id text not null,
        contract_id text not null,
        agent_id text not null,
        outcome text not null,
        summary text not null,
        findings_json text not null,
        evidence_refs_json text not null,
        admitted_at text not null,
        foreign key (task_id) references task_lifecycle(task_id),
        foreign key (contract_id) references task_outcome_contracts(contract_id)
      );

      create index if not exists idx_task_outcomes_task
        on task_outcomes(task_id, admitted_at desc);

      create index if not exists idx_task_outcomes_contract
        on task_outcomes(contract_id, admitted_at desc);

      create table if not exists task_dependency_dispositions (
        disposition_id text primary key,
        dependency_id text not null,
        required_outcome_id text not null,
        kind text not null,
        status text not null,
        target_task_id text,
        routed_obligation_id text,
        authority_basis_json text not null,
        summary text not null,
        created_by text not null,
        created_at text not null,
        foreign key (dependency_id) references task_dependencies(dependency_id),
        foreign key (required_outcome_id) references task_outcomes(outcome_id)
      );

      create index if not exists idx_task_dependency_dispositions_dependency
        on task_dependency_dispositions(dependency_id, required_outcome_id, created_at desc);

      create table if not exists task_conflict_policy_evidence (
        evidence_id text primary key,
        dependency_id text not null,
        required_task_id text not null,
        required_outcome_id text not null,
        agent_id text not null,
        effective_operator_identity text,
        gated_work_operator_identity text,
        conflict_detected integer not null,
        policy_mode text not null,
        authorization_required integer not null,
        authorization_basis_json text,
        annotation_recorded integer not null,
        created_at text not null,
        foreign key (dependency_id) references task_dependencies(dependency_id),
        foreign key (required_task_id) references task_lifecycle(task_id),
        foreign key (required_outcome_id) references task_outcomes(outcome_id)
      );

      create index if not exists idx_task_conflict_policy_evidence_dependency
        on task_conflict_policy_evidence(dependency_id, required_outcome_id, created_at desc);

      create table if not exists task_number_sequence (
        singleton integer primary key check (singleton = 1),
        last_allocated integer not null default 0
      );

      insert or ignore into task_number_sequence (singleton, last_allocated)
      values (1, 0);

      create table if not exists dispatch_packets (
        packet_id text primary key,
        task_id text not null,
        assignment_id text not null,
        agent_id text not null,
        picked_up_at text not null,
        lease_expires_at text not null,
        heartbeat_at text,
        dispatch_status text not null,
        sequence integer not null default 1,
        created_by text not null,
        target_session_id text,
        target_session_title text,
        foreign key (task_id) references task_lifecycle(task_id)
        -- assignment_id FK deferred: assignments are still in JSON files (Task 564 follow-up)
      );

      create index if not exists idx_dispatch_packets_task_id
        on dispatch_packets(task_id);

      create index if not exists idx_dispatch_packets_assignment_id
        on dispatch_packets(assignment_id);

      create index if not exists idx_dispatch_packets_agent_status
        on dispatch_packets(agent_id, dispatch_status);

      create index if not exists idx_dispatch_packets_lease_expires
        on dispatch_packets(lease_expires_at)
        where dispatch_status in ('picked_up', 'renewed');

      create table if not exists verification_runs (
        run_id text primary key,
        request_id text not null,
        task_id text,
        target_command text not null,
        scope text not null,
        timeout_seconds integer not null,
        requester_identity text not null,
        requested_at text not null,
        status text not null,
        exit_code integer,
        duration_ms integer,
        metrics_json text,
        stdout_digest text,
        stderr_digest text,
        stdout_excerpt text,
        stderr_excerpt text,
        completed_at text
      );

      create index if not exists idx_verification_runs_task_id
        on verification_runs(task_id);

      create index if not exists idx_verification_runs_status
        on verification_runs(status);

      create index if not exists idx_verification_runs_requested_at
        on verification_runs(requested_at);

      create table if not exists command_runs (
        run_id text primary key,
        request_id text not null,
        requester_id text not null,
        requester_kind text not null,
        command_argv_json text not null,
        cwd text not null,
        env_policy_json text not null,
        timeout_seconds integer not null,
        stdin_policy_json text not null,
        task_id text,
        task_number integer,
        agent_id text,
        side_effect_class text not null,
        approval_posture text not null,
        output_admission_profile text not null,
        idempotency_key text not null,
        requested_at text not null,
        rationale text,
        status text not null,
        exit_code integer,
        signal text,
        started_at text,
        completed_at text,
        duration_ms integer,
        stdout_digest text,
        stderr_digest text,
        stdout_admitted_excerpt text,
        stderr_admitted_excerpt text,
        full_output_artifact_uri text,
        error_class text,
        approval_outcome text not null,
        telemetry_json text,
        updated_at text not null
      );

      create index if not exists idx_command_runs_task_id
        on command_runs(task_id);

      create index if not exists idx_command_runs_agent_id
        on command_runs(agent_id);

      create index if not exists idx_command_runs_status
        on command_runs(status);

      create index if not exists idx_command_runs_requested_at
        on command_runs(requested_at);

      create table if not exists repo_publications (
        publication_id text primary key,
        repo_root text not null,
        branch text not null,
        remote text not null,
        commit_hash text not null,
        base_ref text,
        bundle_path text not null,
        patch_path text,
        task_number integer,
        requester_id text not null,
        requested_at text not null,
        status text not null,
        pushed_at text,
        confirmed_by text,
        confirmation_json text,
        failure_reason text,
        updated_at text not null
      );

      create index if not exists idx_repo_publications_status
        on repo_publications(status);

      create index if not exists idx_repo_publications_requested_at
        on repo_publications(requested_at);

      create table if not exists agent_roster (
        agent_id text primary key,
        role text not null,
        capabilities_json text not null,
        operator_identity text,
        first_seen_at text not null,
        last_active_at text not null,
        status text not null default 'idle',
        task_number integer,
        last_done integer,
        updated_at text not null
      );

      create index if not exists idx_agent_roster_status
        on agent_roster(status);

      create table if not exists directed_obligations (
        obligation_id text primary key,
        source_kind text not null,
        source_ref text not null,
        source_agent_id text,
        target_agent_id text,
        target_role text,
        target_ref text,
        kind text not null,
        status text not null,
        task_id text,
        task_number integer,
        evidence_json text not null,
        consumption_rule_json text not null,
        created_at text not null,
        updated_at text not null,
        consumed_at text,
        consumed_by text,
        consumption_ref text,
        constraint directed_obligations_no_role_ref_dup
          check (target_role is null or target_ref is null or target_ref <> ('role:' || target_role)),
        foreign key (task_id) references task_lifecycle(task_id)
      );

      create index if not exists idx_directed_obligations_target
        on directed_obligations(target_agent_id, target_role, status, created_at);

      create index if not exists idx_directed_obligations_task
        on directed_obligations(task_id, status);

      create table if not exists task_number_reservations (
        range_start integer not null,
        range_end integer not null,
        purpose text not null,
        reserved_by text not null,
        reserved_at text not null,
        expires_at text not null,
        status text not null,
        primary key (range_start, range_end)
      );

      create index if not exists idx_task_number_reservations_status
        on task_number_reservations(status);

      create table if not exists task_specs (
        task_id text primary key,
        task_number integer not null unique,
        title text not null,
        chapter_markdown text,
        goal_markdown text,
        context_markdown text,
        required_work_markdown text,
        non_goals_markdown text,
        acceptance_criteria_json text not null,
        dependencies_json text not null,
        tags_json text not null default '[]',
        updated_at text not null,
        foreign key (task_id) references task_lifecycle(task_id)
      );

      create index if not exists idx_task_specs_task_number
        on task_specs(task_number);

      create table if not exists task_tag_updates (
        update_id text primary key,
        task_id text not null,
        task_number integer not null,
        actor_agent_id text not null,
        previous_tags_json text not null,
        new_tags_json text not null,
        reason text not null,
        updated_at text not null,
        foreign key (task_id) references task_lifecycle(task_id)
      );

      create index if not exists idx_task_tag_updates_task
        on task_tag_updates(task_id, updated_at desc);

      create table if not exists envelope_task_mappings (
        envelope_id text primary key,
        task_id text not null,
        task_number integer not null,
        materialized_at text not null,
        foreign key (task_id) references task_lifecycle(task_id)
      );

      create index if not exists idx_envelope_task_mappings_task_id
        on envelope_task_mappings(task_id, materialized_at desc);

      create table if not exists task_executability_requests (
        request_id text primary key,
        task_id text not null,
        task_number integer not null,
        state text not null,
        task_spec_digest text not null,
        environment_digest text not null,
        evaluator_profile text not null,
        evaluator_profile_version text not null,
        assessment_id text,
        lease_owner text,
        lease_expires_at text,
        attempt_count integer not null default 0,
        superseded_by_request_id text,
        created_at text not null,
        updated_at text not null,
        foreign key (task_id) references task_lifecycle(task_id)
      );

      create index if not exists idx_task_executability_requests_task
        on task_executability_requests(task_id);

      create index if not exists idx_task_executability_requests_state_lease
        on task_executability_requests(state, lease_expires_at);

      create table if not exists task_executability_assessments (
        assessment_id text primary key,
        request_id text not null,
        task_id text not null,
        task_number integer not null,
        task_spec_digest text not null,
        environment_digest text not null,
        verdict text not null,
        findings_json text not null,
        evaluator_json text not null,
        created_at text not null,
        foreign key (request_id) references task_executability_requests(request_id),
        foreign key (task_id) references task_lifecycle(task_id)
      );

      create index if not exists idx_task_executability_assessments_task
        on task_executability_assessments(task_id);

      create index if not exists idx_task_executability_assessments_request
        on task_executability_assessments(request_id);

      create table if not exists task_executability_overrides (
        override_id text primary key,
        task_id text not null,
        task_spec_digest text not null,
        dispatch_fingerprint text not null,
        actor text not null,
        reason text not null,
        authority_basis_json text not null,
        created_at text not null,
        consumed_at text,
        foreign key (task_id) references task_lifecycle(task_id)
      );

      create index if not exists idx_task_executability_overrides_task
        on task_executability_overrides(task_id);

      create table if not exists task_executability_attempts (
        attempt_id text primary key,
        request_id text not null,
        actor text not null,
        leased_at text not null,
        lease_expires_at text not null,
        state text not null,
        delegated_task_id text,
        worker_run_id text,
        error_json text,
        created_at text not null,
        foreign key (request_id) references task_executability_requests(request_id)
      );

      create index if not exists idx_task_executability_attempts_request
        on task_executability_attempts(request_id);

      create table if not exists narada_andrey_task_role_preferences (
        task_id text primary key,
        preferred_role text,
        target_role text,
        preferred_agent_id text,
        updated_at text not null
      );

      commit;
    