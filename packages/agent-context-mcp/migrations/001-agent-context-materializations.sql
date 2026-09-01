-- Migration: agent-context-materializations
-- Date: 2026-05-04
-- Author: narada-andrey.architect (Kevin)
--
-- Invariants:
-- 1. Intelligence Context materializations are ephemeral traces.
-- 2. They are always event-scoped.
-- 3. They expire.
-- 4. They are never queried as durable identity memory.
-- 5. Proposals are separate durable V→D boundary objects.
-- 6. Residuals are durable pressure markers, not obligations.
-- 7. Residual → task requires explicit promotion.
--
-- Hard validator rule:
-- No table may infer current agent belief from prior intelligence_context_materializations.
-- Allowed: show what was materialized during event X.
-- Forbidden: load latest intelligence_context as agent memory.

CREATE TABLE IF NOT EXISTS agent_start_events (
  event_id TEXT PRIMARY KEY,
  identity_id TEXT NOT NULL,
  runtime TEXT NOT NULL,
  created_at TEXT NOT NULL,
  status TEXT NOT NULL,
  resume_command TEXT,
  bootstrap_artifact_uri TEXT,
  claimed_identity_json TEXT,
  authentication_json TEXT,
  authority_json TEXT
);

CREATE TABLE IF NOT EXISTS identity_state_records (
  record_id TEXT PRIMARY KEY,
  event_id TEXT,
  session_id TEXT,
  claimed_identity_json TEXT NOT NULL,
  authentication_json TEXT NOT NULL,
  authority_json TEXT NOT NULL,
  recorded_at TEXT NOT NULL
);

CREATE TRIGGER IF NOT EXISTS identity_state_records_no_update
BEFORE UPDATE ON identity_state_records
BEGIN
  SELECT RAISE(ABORT, 'identity_state_records_append_only_no_update');
END;

CREATE TRIGGER IF NOT EXISTS identity_state_records_no_delete
BEFORE DELETE ON identity_state_records
BEGIN
  SELECT RAISE(ABORT, 'identity_state_records_append_only_no_delete');
END;

CREATE TABLE IF NOT EXISTS execution_context_materializations (
  materialization_id TEXT PRIMARY KEY,
  event_id TEXT NOT NULL,
  runtime TEXT NOT NULL,
  cwd TEXT,
  payload_json TEXT NOT NULL,
  created_at TEXT NOT NULL,
  expires_at TEXT NOT NULL,
  FOREIGN KEY (event_id) REFERENCES agent_start_events(event_id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS intelligence_context_materializations (
  materialization_id TEXT PRIMARY KEY,
  event_id TEXT NOT NULL,
  schema_id TEXT NOT NULL,
  payload_json TEXT NOT NULL,
  created_at TEXT NOT NULL,
  expires_at TEXT NOT NULL,
  FOREIGN KEY (event_id) REFERENCES agent_start_events(event_id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS proposal_records (
  proposal_id TEXT PRIMARY KEY,
  event_id TEXT NOT NULL,
  materialization_id TEXT,
  proposal_type TEXT NOT NULL, -- evaluation, decision_request, intent_request
  payload_json TEXT NOT NULL,
  verdict TEXT NOT NULL DEFAULT 'pending', -- pending, accepted, rejected, superseded, expired
  verdict_at TEXT,
  verdict_by TEXT,
  created_at TEXT NOT NULL,
  FOREIGN KEY (event_id) REFERENCES agent_start_events(event_id),
  FOREIGN KEY (materialization_id) REFERENCES intelligence_context_materializations(materialization_id) ON DELETE SET NULL
);

CREATE TABLE IF NOT EXISTS residual_records (
  residual_id TEXT PRIMARY KEY,
  event_id TEXT,
  materialization_id TEXT,
  label TEXT NOT NULL,
  payload_json TEXT NOT NULL,
  status TEXT NOT NULL DEFAULT 'noted', -- noted, promoted, dropped, expired
  promoted_task_id TEXT,
  created_at TEXT NOT NULL,
  status_at TEXT,
  FOREIGN KEY (event_id) REFERENCES agent_start_events(event_id),
  FOREIGN KEY (materialization_id) REFERENCES intelligence_context_materializations(materialization_id) ON DELETE SET NULL
);

CREATE TABLE IF NOT EXISTS artifact_refs (
  artifact_id TEXT PRIMARY KEY,
  uri TEXT NOT NULL,
  sha256 TEXT,
  mime_type TEXT,
  byte_size INTEGER,
  created_at TEXT NOT NULL
);
