import { createHash, randomUUID } from 'node:crypto';
import { existsSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { dirname, relative, resolve } from 'node:path';
import { DatabaseSync } from 'node:sqlite';

type JsonRecord = Record<string, unknown>;
const BODY_FILE_THRESHOLD = 20_000;
const STORE_RELATIVE_ROOT = '.narada/runtime/operator-communication';

export type StoredResponse = {
  ref: string;
  sequence: number;
  response_sha256: string;
  schema_sha256: string;
  char_length: number;
  storage_kind: 'sqlite' | 'file';
  body_path: string | null;
};

export function appendResponse(siteRoot: string, response: JsonRecord, validationSchema: JsonRecord, schemaSource: string, createdBy: string | null): StoredResponse {
  const root = resolve(siteRoot, STORE_RELATIVE_ROOT);
  const dbPath = resolve(root, 'operator-communication.sqlite');
  mkdirSync(root, { recursive: true });
  const responseJson = stableJson(response);
  const schemaJson = stableJson(validationSchema);
  const responseSha = sha256(responseJson);
  const schemaSha = sha256(schemaJson);
  const storageKind = responseJson.length > BODY_FILE_THRESHOLD ? 'file' : 'sqlite';
  const bodyPath = storageKind === 'file' ? persistBodyFile(root, responseSha, responseJson) : null;
  const responseRef = `operator_response:${randomUUID()}`;
  const createdAt = new Date().toISOString();
  const db = openStore(dbPath);
  try {
    const previous = db.prepare('SELECT sequence, record_hash FROM response_log ORDER BY sequence DESC LIMIT 1').get() as JsonRecord | undefined;
    const previousHash = previous ? String(previous.record_hash) : '0'.repeat(64);
    const material = {
      response_ref: responseRef, created_at: createdAt, created_by: createdBy,
      response_sha256: responseSha, schema_sha256: schemaSha, schema_source: schemaSource,
      char_length: responseJson.length, storage_kind: storageKind, body_path: bodyPath,
      previous_hash: previousHash,
    };
    const recordHash = sha256(stableJson(material));
    db.prepare(`INSERT INTO response_log
      (response_ref, created_at, created_by, response_sha256, schema_sha256, schema_source,
       char_length, storage_kind, body_json, body_path, schema_json, previous_hash, record_hash)
      VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)`).run(
      responseRef, createdAt, createdBy, responseSha, schemaSha, schemaSource,
      responseJson.length, storageKind, storageKind === 'sqlite' ? responseJson : null,
      bodyPath, schemaJson, previousHash, recordHash,
    );
    const row = db.prepare('SELECT sequence FROM response_log WHERE response_ref = ?').get(responseRef) as JsonRecord;
    return { ref: responseRef, sequence: Number(row.sequence), response_sha256: responseSha, schema_sha256: schemaSha, char_length: responseJson.length, storage_kind: storageKind, body_path: bodyPath };
  } finally {
    db.close();
  }
}

export function readResponse(siteRoot: string, ref: string): { response: JsonRecord; validationSchema: JsonRecord; schemaSource: string; stored: StoredResponse } {
  if (!/^operator_response:[0-9a-f-]{36}$/.test(ref)) throw new Error('operator_response_ref_invalid');
  const root = resolve(siteRoot, STORE_RELATIVE_ROOT);
  const db = openStore(resolve(root, 'operator-communication.sqlite'));
  try {
    const row = db.prepare('SELECT * FROM response_log WHERE response_ref = ?').get(ref) as JsonRecord | undefined;
    if (!row) throw new Error('operator_response_ref_not_found');
    verifyRow(db, row);
    const responseJson = row.storage_kind === 'file'
      ? readBodyFile(root, String(row.body_path), String(row.response_sha256))
      : String(row.body_json);
    if (responseJson.length !== Number(row.char_length) || sha256(responseJson) !== row.response_sha256) throw new Error('operator_response_body_integrity_failed');
    const schemaJson = String(row.schema_json);
    if (sha256(schemaJson) !== row.schema_sha256) throw new Error('operator_response_schema_integrity_failed');
    return {
      response: parseRecord(responseJson, 'operator_response_body_invalid'),
      validationSchema: parseRecord(schemaJson, 'operator_response_schema_invalid'),
      schemaSource: String(row.schema_source),
      stored: {
        ref, sequence: Number(row.sequence), response_sha256: String(row.response_sha256),
        schema_sha256: String(row.schema_sha256), char_length: Number(row.char_length),
        storage_kind: row.storage_kind as 'sqlite' | 'file',
        body_path: row.body_path === null ? null : String(row.body_path),
      },
    };
  } finally {
    db.close();
  }
}

function openStore(path: string): DatabaseSync {
  mkdirSync(dirname(path), { recursive: true });
  const db = new DatabaseSync(path);
  db.exec(`
    PRAGMA journal_mode = WAL;
    PRAGMA synchronous = FULL;
    CREATE TABLE IF NOT EXISTS response_log (
      sequence INTEGER PRIMARY KEY AUTOINCREMENT,
      response_ref TEXT NOT NULL UNIQUE,
      created_at TEXT NOT NULL,
      created_by TEXT,
      response_sha256 TEXT NOT NULL,
      schema_sha256 TEXT NOT NULL,
      schema_source TEXT NOT NULL,
      char_length INTEGER NOT NULL CHECK(char_length >= 0),
      storage_kind TEXT NOT NULL CHECK(storage_kind IN ('sqlite','file')),
      body_json TEXT,
      body_path TEXT,
      schema_json TEXT NOT NULL,
      previous_hash TEXT NOT NULL,
      record_hash TEXT NOT NULL UNIQUE,
      CHECK((storage_kind='sqlite' AND body_json IS NOT NULL AND body_path IS NULL)
         OR (storage_kind='file' AND body_json IS NULL AND body_path IS NOT NULL))
    );
    CREATE TRIGGER IF NOT EXISTS response_log_no_update
      BEFORE UPDATE ON response_log BEGIN SELECT RAISE(ABORT, 'response_log_is_immutable'); END;
    CREATE TRIGGER IF NOT EXISTS response_log_no_delete
      BEFORE DELETE ON response_log BEGIN SELECT RAISE(ABORT, 'response_log_is_immutable'); END;
  `);
  return db;
}

function verifyRow(db: DatabaseSync, row: JsonRecord): void {
  const sequence = Number(row.sequence);
  const previous = sequence > 1 ? db.prepare('SELECT record_hash FROM response_log WHERE sequence = ?').get(sequence - 1) as JsonRecord | undefined : undefined;
  const expectedPrevious = previous ? String(previous.record_hash) : '0'.repeat(64);
  if (row.previous_hash !== expectedPrevious) throw new Error('operator_response_chain_link_invalid');
  const material = {
    response_ref: row.response_ref, created_at: row.created_at, created_by: row.created_by,
    response_sha256: row.response_sha256, schema_sha256: row.schema_sha256, schema_source: row.schema_source,
    char_length: row.char_length, storage_kind: row.storage_kind, body_path: row.body_path,
    previous_hash: row.previous_hash,
  };
  if (sha256(stableJson(material)) !== row.record_hash) throw new Error('operator_response_record_hash_invalid');
}

function persistBodyFile(root: string, digest: string, content: string): string {
  const path = resolve(root, 'bodies', `${digest}.json`);
  mkdirSync(dirname(path), { recursive: true });
  if (existsSync(path)) {
    if (readFileSync(path, 'utf8') !== content) throw new Error('operator_response_body_digest_collision');
  } else {
    writeFileSync(path, content, { encoding: 'utf8', flag: 'wx', flush: true });
  }
  return relative(root, path).replace(/\\/g, '/');
}

function readBodyFile(root: string, relativePath: string, digest: string): string {
  if (relativePath !== `bodies/${digest}.json`) throw new Error('operator_response_body_path_invalid');
  return readFileSync(resolve(root, relativePath), 'utf8');
}

function parseRecord(text: string, code: string): JsonRecord {
  let value: unknown;
  try { value = JSON.parse(text); } catch { throw new Error(code); }
  if (!value || typeof value !== 'object' || Array.isArray(value)) throw new Error(code);
  return value as JsonRecord;
}

function stableJson(value: unknown): string {
  if (Array.isArray(value)) return '[' + value.map(stableJson).join(',') + ']';
  if (value && typeof value === 'object') return '{' + Object.entries(value as JsonRecord).sort(([a],[b]) => a.localeCompare(b)).map(([key, child]) => JSON.stringify(key) + ':' + stableJson(child)).join(',') + '}';
  return JSON.stringify(value) ?? 'null';
}
function sha256(value: string): string { return createHash('sha256').update(value, 'utf8').digest('hex'); }
