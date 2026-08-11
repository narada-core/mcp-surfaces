CREATE TABLE IF NOT EXISTS orientation_manifest_generations (
  manifest_id TEXT PRIMARY KEY, admission_receipt_ref TEXT NOT NULL,
  carrier_session_id TEXT NOT NULL, authority_epoch INTEGER NOT NULL,
  readiness TEXT NOT NULL, delivery TEXT NOT NULL,
  manifest_json TEXT NOT NULL, generated_at TEXT NOT NULL
);
CREATE TRIGGER IF NOT EXISTS orientation_manifest_generations_no_update BEFORE UPDATE ON orientation_manifest_generations BEGIN SELECT RAISE(ABORT, 'orientation_manifest_generations_append_only_no_update'); END;
CREATE TRIGGER IF NOT EXISTS orientation_manifest_generations_no_delete BEFORE DELETE ON orientation_manifest_generations BEGIN SELECT RAISE(ABORT, 'orientation_manifest_generations_append_only_no_delete'); END;

CREATE TABLE IF NOT EXISTS orientation_brief_generations (
  brief_id TEXT PRIMARY KEY, manifest_id TEXT NOT NULL, brief_digest TEXT NOT NULL,
  brief_json TEXT NOT NULL, generated_at TEXT NOT NULL
);
CREATE TRIGGER IF NOT EXISTS orientation_brief_generations_no_update BEFORE UPDATE ON orientation_brief_generations BEGIN SELECT RAISE(ABORT, 'orientation_brief_generations_append_only_no_update'); END;
CREATE TRIGGER IF NOT EXISTS orientation_brief_generations_no_delete BEFORE DELETE ON orientation_brief_generations BEGIN SELECT RAISE(ABORT, 'orientation_brief_generations_append_only_no_delete'); END;

CREATE TABLE IF NOT EXISTS orientation_delivery_receipts (
  receipt_id TEXT PRIMARY KEY, manifest_id TEXT NOT NULL, brief_id TEXT NOT NULL,
  carrier_session_id TEXT NOT NULL, authority_epoch INTEGER NOT NULL,
  receipt_json TEXT NOT NULL, delivered_at TEXT NOT NULL
);
CREATE TRIGGER IF NOT EXISTS orientation_delivery_receipts_no_update BEFORE UPDATE ON orientation_delivery_receipts BEGIN SELECT RAISE(ABORT, 'orientation_delivery_receipts_append_only_no_update'); END;
CREATE TRIGGER IF NOT EXISTS orientation_delivery_receipts_no_delete BEFORE DELETE ON orientation_delivery_receipts BEGIN SELECT RAISE(ABORT, 'orientation_delivery_receipts_append_only_no_delete'); END;

CREATE TABLE IF NOT EXISTS orientation_acknowledgements (
  acknowledgement_id TEXT PRIMARY KEY, delivery_receipt_ref TEXT NOT NULL,
  manifest_id TEXT NOT NULL, brief_id TEXT NOT NULL, carrier_session_id TEXT NOT NULL,
  authority_epoch INTEGER NOT NULL, acknowledgement_json TEXT NOT NULL,
  acknowledged_at TEXT NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_orientation_acknowledgements_delivery ON orientation_acknowledgements(delivery_receipt_ref);
CREATE TRIGGER IF NOT EXISTS orientation_acknowledgements_no_update BEFORE UPDATE ON orientation_acknowledgements BEGIN SELECT RAISE(ABORT, 'orientation_acknowledgements_append_only_no_update'); END;
CREATE TRIGGER IF NOT EXISTS orientation_acknowledgements_no_delete BEFORE DELETE ON orientation_acknowledgements BEGIN SELECT RAISE(ABORT, 'orientation_acknowledgements_append_only_no_delete'); END;

CREATE TABLE IF NOT EXISTS orientation_required_read_pages (
  page_id TEXT PRIMARY KEY, delivery_receipt_ref TEXT NOT NULL, manifest_id TEXT NOT NULL,
  brief_id TEXT NOT NULL, step_id TEXT NOT NULL, byte_offset INTEGER NOT NULL,
  next_byte_offset INTEGER, page_json TEXT NOT NULL, delivered_at TEXT NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_orientation_required_read_page ON orientation_required_read_pages(delivery_receipt_ref, step_id, byte_offset);
CREATE TRIGGER IF NOT EXISTS orientation_required_read_pages_no_update BEFORE UPDATE ON orientation_required_read_pages BEGIN SELECT RAISE(ABORT, 'orientation_required_read_pages_append_only_no_update'); END;
CREATE TRIGGER IF NOT EXISTS orientation_required_read_pages_no_delete BEFORE DELETE ON orientation_required_read_pages BEGIN SELECT RAISE(ABORT, 'orientation_required_read_pages_append_only_no_delete'); END;

CREATE TABLE IF NOT EXISTS orientation_required_read_completions (
  completion_id TEXT PRIMARY KEY, delivery_receipt_ref TEXT NOT NULL,
  manifest_id TEXT NOT NULL, brief_id TEXT NOT NULL, step_id TEXT NOT NULL,
  completion_json TEXT NOT NULL, completed_at TEXT NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_orientation_required_read_completion_step ON orientation_required_read_completions(delivery_receipt_ref, step_id);
CREATE TRIGGER IF NOT EXISTS orientation_required_read_completions_no_update BEFORE UPDATE ON orientation_required_read_completions BEGIN SELECT RAISE(ABORT, 'orientation_required_read_completions_append_only_no_update'); END;
CREATE TRIGGER IF NOT EXISTS orientation_required_read_completions_no_delete BEFORE DELETE ON orientation_required_read_completions BEGIN SELECT RAISE(ABORT, 'orientation_required_read_completions_append_only_no_delete'); END;
