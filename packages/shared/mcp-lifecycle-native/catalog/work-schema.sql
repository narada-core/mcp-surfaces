
    begin immediate;

    create table if not exists work_lifecycle_meta (
      singleton integer primary key check (singleton = 1),
      schema_version integer not null,
      prepared_at text not null
    );

    create table if not exists work_sequences (
      sequence_name text primary key,
      next_value integer not null check (next_value > 0)
    );

    create table if not exists tickets (
      ticket_id text primary key,
      ticket_number integer not null unique,
      status text not null check (
        status in ('actionable', 'effect_claimed', 'waiting_on_draft',
                   'waiting_on_task', 'blocked', 'resolved')
      ),
      revision integer not null check (revision > 0),
      summary text not null check (length(cast(summary as blob)) <= 2048),
      resolution_code text,
      blocker_code text,
      created_at text not null,
      updated_at text not null,
      terminal_at text
    );

    create index if not exists idx_tickets_status_updated
      on tickets(status, updated_at);

    create table if not exists ticket_sources (
      source_id text primary key,
      ticket_id text not null references tickets(ticket_id),
      source_kind text not null,
      source_scope text not null,
      immutable_source_id text not null,
      source_ref_json text not null
        check (length(cast(source_ref_json as blob)) <= 16384),
      policy_version text not null,
      receipt_id text not null unique,
      admitted_at text not null,
      unique(source_kind, source_scope, immutable_source_id)
    );

    create index if not exists idx_ticket_sources_ticket
      on ticket_sources(ticket_id, admitted_at);

    create table if not exists ticket_correlation_keys (
      kind text not null,
      scope text not null,
      value text not null,
      ticket_id text not null references tickets(ticket_id),
      policy_version text not null,
      admitted_at text not null,
      primary key(kind, scope, value)
    );

    create index if not exists idx_ticket_correlation_ticket
      on ticket_correlation_keys(ticket_id);

    create table if not exists ticket_task_links (
      ticket_id text not null references tickets(ticket_id),
      task_id text not null references task_lifecycle(task_id),
      link_kind text not null,
      operation_key text not null,
      status text not null check (status in ('active', 'terminal', 'superseded')),
      linked_at text not null,
      terminal_at text,
      primary key(ticket_id, task_id),
      unique(operation_key)
    );

    create index if not exists idx_ticket_task_links_task
      on ticket_task_links(task_id, status);

    create table if not exists ticket_effect_claims (
      claim_id text primary key,
      ticket_id text not null references tickets(ticket_id),
      ticket_revision integer not null,
      effect_kind text not null,
      operation_key text not null unique,
      request_digest text not null,
      status text not null check (status in ('claimed', 'completed', 'superseded')),
      receipt_id text,
      receipt_json text
        check (receipt_json is null or length(cast(receipt_json as blob)) <= 16384),
      claimed_at text not null,
      completed_at text
    );

    create table if not exists ticket_draft_refs (
      ticket_id text not null references tickets(ticket_id),
      draft_id text not null,
      effect_claim_id text not null references ticket_effect_claims(claim_id),
      draft_ref_json text not null
        check (length(cast(draft_ref_json as blob)) <= 16384),
      receipt_id text not null,
      disposition text,
      disposition_evidence_kind text,
      disposition_evidence_id text,
      disposition_evidence_json text
        check (disposition_evidence_json is null or length(cast(disposition_evidence_json as blob)) <= 16384),
      created_at text not null,
      disposed_at text,
      primary key(ticket_id, draft_id)
    );

    create table if not exists work_lifecycle_events (
      event_id text primary key,
      aggregate_kind text not null check (aggregate_kind in ('ticket', 'task')),
      aggregate_id text not null,
      aggregate_revision integer not null,
      event_type text not null,
      schema_version integer not null,
      causation_id text not null,
      idempotency_key text not null unique,
      payload_json text not null
        check (length(cast(payload_json as blob)) <= 16384),
      created_at text not null
    );

    create index if not exists idx_work_events_aggregate
      on work_lifecycle_events(aggregate_kind, aggregate_id, aggregate_revision);

    create table if not exists work_outbox (
      event_id text primary key references work_lifecycle_events(event_id),
      topic text not null,
      partition_key text not null,
      aggregate_kind text not null check (aggregate_kind in ('ticket', 'task')),
      aggregate_id text not null,
      aggregate_revision integer not null,
      schema_version integer not null,
      causation_id text not null,
      idempotency_key text not null unique,
      payload_json text not null
        check (length(cast(payload_json as blob)) <= 16384),
      created_at text not null,
      available_at text not null,
      compacted_at text
    );

    create index if not exists idx_work_outbox_delivery
      on work_outbox(topic, available_at, created_at);

    create table if not exists work_outbox_consumer_requirements (
      topic text not null,
      consumer_id text not null,
      registered_at text not null,
      primary key(topic, consumer_id)
    );

    create table if not exists work_outbox_receipts (
      event_id text not null references work_outbox(event_id),
      consumer_id text not null,
      processed_at text not null,
      receipt_json text not null
        check (length(cast(receipt_json as blob)) <= 16384),
      primary key(event_id, consumer_id)
    );

    create table if not exists work_operations (
      operation_key text primary key,
      operation_kind text not null,
      request_digest text not null,
      aggregate_kind text,
      aggregate_id text,
      aggregate_revision integer,
      result_json text not null
        check (length(cast(result_json as blob)) <= 32768),
      created_at text not null
    );

    create index if not exists idx_work_operations_aggregate
      on work_operations(aggregate_kind, aggregate_id, created_at);

    insert into work_sequences(sequence_name, next_value)
      values ('ticket', 1)
      on conflict(sequence_name) do nothing;

    insert into work_lifecycle_meta(singleton, schema_version, prepared_at)
      values (1, 2, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
      on conflict(singleton) do update set
        schema_version = excluded.schema_version,
        prepared_at = excluded.prepared_at;

    drop trigger if exists work_task_insert_event;
    create trigger work_task_insert_event
    after insert on task_lifecycle
    begin
      insert or ignore into work_lifecycle_events(
        event_id, aggregate_kind, aggregate_id, aggregate_revision, event_type,
        schema_version, causation_id, idempotency_key, payload_json, created_at
      ) values (
        'evt:task:' || new.task_id || ':revision:' || new.revision,
        'task', new.task_id, new.revision, 'task.created', 1,
        'task-create:' || new.task_id,
        'task:' || new.task_id || ':revision:' || new.revision,
        json_object('task_id', new.task_id, 'task_number', new.task_number,
                    'status', new.status, 'revision', new.revision),
        new.updated_at
      );
      insert or ignore into work_outbox(
        event_id, topic, partition_key, aggregate_kind, aggregate_id,
        aggregate_revision, schema_version, causation_id, idempotency_key,
        payload_json, created_at, available_at, compacted_at
      ) select event_id, 'work.task-lifecycle.v1', new.task_id, aggregate_kind,
               aggregate_id, aggregate_revision, schema_version, causation_id,
               idempotency_key, payload_json, created_at, created_at, null
        from work_lifecycle_events
       where event_id = 'evt:task:' || new.task_id || ':revision:' || new.revision;
    end;

    drop trigger if exists work_task_revision_event;
    create trigger work_task_revision_event
    after update on task_lifecycle
    when new.revision = old.revision
    begin
      update task_lifecycle
         set revision = old.revision + 1
       where task_id = new.task_id;
      insert or ignore into work_lifecycle_events(
        event_id, aggregate_kind, aggregate_id, aggregate_revision, event_type,
        schema_version, causation_id, idempotency_key, payload_json, created_at
      ) values (
        'evt:task:' || new.task_id || ':revision:' || (old.revision + 1),
        'task', new.task_id, old.revision + 1, 'task.lifecycle.changed', 1,
        'task-transition:' || new.task_id || ':' || (old.revision + 1),
        'task:' || new.task_id || ':revision:' || (old.revision + 1),
        json_object('task_id', new.task_id, 'task_number', new.task_number,
                    'status', new.status, 'previous_status', old.status,
                    'revision', old.revision + 1),
        new.updated_at
      );
      insert or ignore into work_outbox(
        event_id, topic, partition_key, aggregate_kind, aggregate_id,
        aggregate_revision, schema_version, causation_id, idempotency_key,
        payload_json, created_at, available_at, compacted_at
      ) select event_id, 'work.task-lifecycle.v1', new.task_id, aggregate_kind,
               aggregate_id, aggregate_revision, schema_version, causation_id,
               idempotency_key, payload_json, created_at, created_at, null
        from work_lifecycle_events
       where event_id = 'evt:task:' || new.task_id || ':revision:' || (old.revision + 1);
    end;

    drop trigger if exists work_task_terminal_reactivate_tickets;
    create trigger work_task_terminal_reactivate_tickets
    after update of status on task_lifecycle
    when new.status in ('closed', 'confirmed')
     and old.status not in ('closed', 'confirmed')
    begin
      update tickets
         set status = 'actionable',
             revision = revision + 1,
             blocker_code = null,
             terminal_at = null,
             updated_at = new.updated_at
       where ticket_id in (
         select ticket_id from ticket_task_links
          where task_id = new.task_id and status = 'active'
       );

      insert or ignore into work_lifecycle_events(
        event_id, aggregate_kind, aggregate_id, aggregate_revision, event_type,
        schema_version, causation_id, idempotency_key, payload_json, created_at
      )
      select
        'evt:ticket:' || link.ticket_id || ':task:' || new.task_id ||
          ':terminal:' || (old.revision + 1),
        'ticket', link.ticket_id, ticket.revision, 'ticket.task.terminal', 1,
        'task:' || new.task_id || ':revision:' || (old.revision + 1),
        'ticket:' || link.ticket_id || ':task:' || new.task_id ||
          ':terminal:' || (old.revision + 1),
        json_object('ticket_id', link.ticket_id, 'ticket_revision', ticket.revision,
                    'task_id', new.task_id, 'task_number', new.task_number,
                    'task_status', new.status, 'task_revision', old.revision + 1),
        new.updated_at
      from ticket_task_links link
      join tickets ticket on ticket.ticket_id = link.ticket_id
      where link.task_id = new.task_id and link.status = 'active';

      insert or ignore into work_outbox(
        event_id, topic, partition_key, aggregate_kind, aggregate_id,
        aggregate_revision, schema_version, causation_id, idempotency_key,
        payload_json, created_at, available_at, compacted_at
      )
      select event.event_id, 'work.ticket-work-due.v1', event.aggregate_id,
             event.aggregate_kind, event.aggregate_id, event.aggregate_revision,
             event.schema_version, event.causation_id, event.idempotency_key,
             event.payload_json, event.created_at, event.created_at, null
        from work_lifecycle_events event
       where event.event_id in (
         select 'evt:ticket:' || link.ticket_id || ':task:' || new.task_id ||
                ':terminal:' || (old.revision + 1)
           from ticket_task_links link
          where link.task_id = new.task_id and link.status = 'active'
       );

      update ticket_task_links
         set status = 'terminal', terminal_at = new.updated_at
       where task_id = new.task_id and status = 'active';
    end;

    commit;
  