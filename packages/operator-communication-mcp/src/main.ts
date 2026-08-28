#!/usr/bin/env node
import { readFileSync, existsSync } from 'node:fs';
import { dirname, isAbsolute, relative, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { appendResponse, readResponse, type StoredResponse } from './persistence.js';

const SERVER_NAME = 'operator-communication-mcp';
const SERVER_VERSION = '0.1.0';
const PROTOCOL_VERSION = '2024-11-05';
const DEFAULT_SCHEMA_PATH = resolve(dirname(fileURLToPath(import.meta.url)), '../../schema/typed-response.v1.toml');
const DEFAULT_DISPLAY_PATH = resolve(dirname(fileURLToPath(import.meta.url)), '../../display/operator-display-preferences.v1.toml');
const SITE_SCHEMA_RELATIVE_PATH = '.narada/schemas/operator-communication.toml';
const SITE_DISPLAY_RELATIVE_PATH = '.narada/preferences/operator-communication.toml';
const DISPLAY_POLICIES = ['minimal', 'short', 'medium', 'all-limited', 'all-unlimited'] as const;

type RecordValue = Record<string, unknown>;
export type ServerState = { siteRoot: string; siteSchemaPath: string; defaultSchemaPath: string; siteDisplayPath: string; defaultDisplayPath: string };
type DisplayFormat = 'prose' | 'code' | string[];
type DisplayPreferences = { source: string; policy: string; format: DisplayFormat; fields: string[]; maxChars: number | null; maxArrayItems: number | null };

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  runStdioServer(parseArgs(process.argv.slice(2))).catch((error) => {
    process.stderr.write(`${error instanceof Error ? error.message : String(error)}\n`);
    process.exit(1);
  });
}

export function createServerState(options: RecordValue = {}): ServerState {
  const siteRoot = resolve(String(options.siteRoot ?? options.site_root ?? process.cwd()));
  const siteSchemaPath = resolve(String(options.siteSchemaPath ?? options.site_schema_path ?? resolve(siteRoot, SITE_SCHEMA_RELATIVE_PATH)));
  const siteDisplayPath = resolve(String(options.siteDisplayPath ?? options.site_display_path ?? resolve(siteRoot, SITE_DISPLAY_RELATIVE_PATH)));
  if (!isWithin(siteRoot, siteSchemaPath)) throw new Error('site_schema_path_outside_site_root');
  if (!isWithin(siteRoot, siteDisplayPath)) throw new Error('site_display_path_outside_site_root');
  return { siteRoot, siteSchemaPath, defaultSchemaPath: DEFAULT_SCHEMA_PATH, siteDisplayPath, defaultDisplayPath: DEFAULT_DISPLAY_PATH };
}

export function listTools(): RecordValue[] {
  return [
    tool('operator_communication_guidance', 'Explain response validation, persistence, reference replay, and operator-only projection.', {
      workflow: { type: 'string' }, tool: { type: 'string' },
    }),
    {
      ...tool('operator_communication_project', 'Validate one inline or persisted typed response and return only its operator projection.', {}),
      inputSchema: {
        type: 'object',
        properties: {
          response: { type: 'object', description: 'Complete inline response containing operator and agent tables. Mutually exclusive with response_ref.' },
          response_ref: { type: 'string', pattern: '^operator_response:[0-9a-f-]{36}$', description: 'Site-bound immutable response-log reference. Mutually exclusive with response.' },
          schema: { oneOf: [{ type: 'object' }, { type: 'string' }], description: 'Inline-response schema override as a parsed object or TOML string.' },
          persist: { type: 'boolean', default: true, description: 'Persist inline response; defaults to true.' },
          created_by: { type: 'string', minLength: 1, description: 'Principal recorded on persisted inline response.' },
          display_policy: { type: 'string', enum: [...DISPLAY_POLICIES], description: 'Operator display policy. Precedence: input, Site-local preference, bundled default short.' },
          format: { oneOf: [{ type: 'string', enum: ['prose', 'code'] }, { type: 'array', minItems: 1, uniqueItems: true, items: { type: 'string', minLength: 1 } }], default: 'prose', description: 'Display as prose, code, or an exact field list.' },
        },
        oneOf: [
          { required: ['response'], not: { required: ['response_ref'] } },
          { required: ['response_ref'], not: { anyOf: [{ required: ['response'] }, { required: ['schema'] }, { required: ['persist'] }, { required: ['created_by'] }] } },
        ],
        additionalProperties: false,
      },
    },
  ];
}

export function projectOperator(args: RecordValue, state: ServerState): RecordValue {
  const executed = executeProject(args, state);
  return applyDisplayPreferences(executed.operator, resolveDisplayPreferences(args, state)).operator;
}

function executeProject(args: RecordValue, state: ServerState): { operator: RecordValue; persistence: StoredResponse | null } {
  const loaded = loadResponse(args, state);
  const response = loaded.response;
  const selected = loaded.schema ?? resolveSchema(args.schema, state);
  validateDocument(response, selected.document);
  const persistence = 'response' in args && args.persist !== false
    ? appendResponse(state.siteRoot, response, selected.document, selected.source, persistencePrincipal(args))
    : null;
  return { operator: asRecord(response.operator, 'operator_must_be_table'), persistence };
}

export function persistResponse(args: RecordValue, state: ServerState): StoredResponse | null {
  if (!('response' in args)) return null;
  if (args.persist === false) return null;
  const response = asRecord(args.response, 'response_must_be_object');
  const selected = resolveSchema(args.schema, state);
  validateDocument(response, selected.document);
  return appendResponse(state.siteRoot, response, selected.document, selected.source, persistencePrincipal(args));
}

function loadResponse(args: RecordValue, state: ServerState): { response: RecordValue; schema: { source: string; document: RecordValue } | null } {
  const hasInline = 'response' in args;
  const hasRef = typeof args.response_ref === 'string';
  if (hasInline === hasRef) fail('choose_exactly_one_of_response_or_response_ref: provide one complete inline response or one operator_response reference');
  if (hasInline && args.persist === false && 'created_by' in args) {
    fail('created_by_requires_persistence: remove created_by or omit persist so the default true applies');
  }
  if (hasRef && ['schema', 'persist', 'created_by'].some((field) => field in args)) {
    fail('response_ref_companion_arguments_forbidden: persisted responses replay their recorded schema and persistence identity');
  }
  if (hasInline) return { response: asRecord(args.response, 'response_must_be_object'), schema: null };
  const stored = readResponse(state.siteRoot, String(args.response_ref));
  return {
    response: stored.response,
    schema: { source: 'response_log_snapshot', document: stored.validationSchema },
  };
}

function resolveSchema(input: unknown, state: ServerState): { source: string; document: RecordValue } {
  if (input !== undefined && input !== null) {
    return { source: 'input', document: typeof input === 'string' ? parseToml(input) : asRecord(input, 'schema_must_be_table') };
  }
  if (existsSync(state.siteSchemaPath)) {
    return { source: 'site', document: parseToml(readFileSync(state.siteSchemaPath, 'utf8')) };
  }
  return { source: 'default', document: parseToml(readFileSync(state.defaultSchemaPath, 'utf8')) };
}

export function resolveDisplayPreferences(args: RecordValue, state: ServerState): DisplayPreferences {
  const bundled = parseToml(readFileSync(state.defaultDisplayPath, 'utf8'));
  const site = existsSync(state.siteDisplayPath) ? parseToml(readFileSync(state.siteDisplayPath, 'utf8')) : null;
  const source = ('display_policy' in args || 'format' in args) ? 'input' : site ? 'site' : 'default';
  const defaults = { ...asRecord(bundled.defaults, 'display_defaults_missing'), ...(site ? asRecord(site.defaults ?? {}, 'invalid_site_display_defaults') : {}) };
  const policy = String(args.display_policy ?? defaults.policy ?? 'short');
  if (!(DISPLAY_POLICIES as readonly string[]).includes(policy)) fail(`invalid_display_policy:${policy}`);
  const rawFormat = args.format ?? defaults.format ?? 'prose';
  const format = normalizeDisplayFormat(rawFormat);
  const bundledPolicies = asRecord(bundled.policies, 'display_policies_missing');
  const sitePolicies = site ? asRecord(site.policies ?? {}, 'invalid_site_display_policies') : {};
  const policySpec = { ...asRecord(bundledPolicies[policy], `missing_display_policy:${policy}`), ...(policy in sitePolicies ? asRecord(sitePolicies[policy], `invalid_site_display_policy:${policy}`) : {}) };
  const fields = Array.isArray(format) ? format : stringArray(policySpec.fields, `display_policy_fields_missing:${policy}`);
  requireUnique(fields, 'display_fields');
  return {
    source,
    policy,
    format,
    fields,
    maxChars: policy === 'all-unlimited' ? null : optionalNonnegativeInteger(policySpec.max_chars, `invalid_display_max_chars:${policy}`),
    maxArrayItems: policy === 'all-unlimited' ? null : optionalNonnegativeInteger(policySpec.max_array_items, `invalid_display_max_array_items:${policy}`),
  };
}

function normalizeDisplayFormat(value: unknown): DisplayFormat {
  if (value === 'prose' || value === 'code') return value;
  if (Array.isArray(value)) {
    const fields = stringArray(value, 'format_field_list_must_be_strings');
    if (fields.length === 0 || fields.some((field) => field.length === 0)) fail('format_field_list_must_be_nonempty');
    return fields;
  }
  fail('invalid_format: expected prose, code, or a nonempty field list');
}

function applyDisplayPreferences(operator: RecordValue, preferences: DisplayPreferences): { operator: RecordValue; text: string } {
  const selected = new Set(preferences.fields);
  const items = Array.isArray(operator.items) ? operator.items.map((value) => {
    const item = asRecord(value, 'operator_item_must_be_table');
    const projected: RecordValue = {};
    for (const [field, raw] of Object.entries(item)) {
      if (!(selected.has('*') || selected.has(field))) continue;
      if (field === 'epistemic_status' && raw === 'verified' && (preferences.policy === 'minimal' || preferences.policy === 'short')) continue;
      projected[field] = limitDisplayValue(raw, preferences);
    }
    return projected;
  }) : [];
  const projection = { items };
  const text = preferences.format === 'prose' ? renderProse(projection) : renderToml(projection);
  return { operator: projection, text };
}

function limitDisplayValue(value: unknown, preferences: DisplayPreferences): unknown {
  if (typeof value === 'string' && preferences.maxChars !== null && value.length > preferences.maxChars) return `${value.slice(0, preferences.maxChars)}…`;
  if (Array.isArray(value) && preferences.maxArrayItems !== null && value.length > preferences.maxArrayItems) return [...value.slice(0, preferences.maxArrayItems), `… ${value.length - preferences.maxArrayItems} more`];
  return value;
}

function renderProse(operator: RecordValue): string {
  const items = Array.isArray(operator.items) ? operator.items : [];
  return items.map((value) => {
    const item = asRecord(value, 'operator_item_must_be_table');
    const statement = typeof item.statement === 'string' ? displayScalar(item.statement) : '';
    const labelled = Object.entries(item).flatMap(([field, raw]) => {
      if (field === 'kind' && raw === 'result') return [];
      if (field === 'statement') return [];
      return [`${humanizeField(field)}: ${displayScalar(raw)}`];
    }).join('\n');
    return [statement, labelled].filter((part) => part.length > 0).join('\n\n');
  }).join('\n\n');
}

function humanizeField(field: string): string { return field.replace(/_/g, ' ').replace(/^./, (char) => char.toUpperCase()); }
function displayScalar(value: unknown): string { return Array.isArray(value) ? value.map(String).join('; ') : String(value); }
function optionalNonnegativeInteger(value: unknown, code: string): number | null {
  if (value === undefined) return null;
  if (!Number.isInteger(value) || Number(value) < 0) fail(code);
  return Number(value);
}

export function validateDocument(value: RecordValue, schema: RecordValue): void {
  const root = asRecord(schema.root, 'schema_root_missing');
  validateTable(value, root, schema, '$');
  applyConstraints(value, schema);
}

function validateTable(value: RecordValue, definition: RecordValue, schema: RecordValue, path: string): void {
  const fields = asRecord(definition.fields ?? {}, `invalid_fields:${path}`);
  const fieldSetName = optionalString(definition.field_set);
  const fieldSet = fieldSetName ? asRecord(asRecord(schema.field_sets ?? {}, 'invalid_field_sets')[fieldSetName], `missing_field_set:${fieldSetName}`) : {};
  const constants = asRecord(definition.constants ?? {}, `invalid_constants:${path}`);
  const required = stringArray(definition.required ?? [], `invalid_required:${path}`);
  const optional = stringArray(definition.optional ?? [], `invalid_optional:${path}`);
  for (const name of required) if (!(name in value)) fail(`required_field_missing:${path}.${name}`);
  const allowed = new Set([...required, ...optional, ...Object.keys(fields), ...Object.keys(fieldSet), ...Object.keys(constants)]);
  if (schema.unknown_fields === 'reject') for (const name of Object.keys(value)) if (!allowed.has(name)) fail(`unknown_field:${path}.${name}`);
  for (const [name, expected] of Object.entries(constants)) if (value[name] !== expected) fail(`constant_mismatch:${path}.${name}`);
  for (const [name, spec] of Object.entries(fields)) if (name in value) validateSpec(value[name], asRecord(spec, `invalid_field_spec:${name}`), schema, `${path}.${name}`);
  for (const [name, typeName] of Object.entries(fieldSet)) if (name in value) validateNamedType(value[name], String(typeName), schema, `${path}.${name}`);
  const itemKind = typeof value.kind === 'string' ? value.kind : null;
  if (itemKind) for (const [name, expected] of Object.entries(constants)) if (name === 'kind' && itemKind !== expected) fail(`constant_mismatch:${path}.kind`);
}

function validateSpec(value: unknown, spec: RecordValue, schema: RecordValue, path: string): void {
  const type = String(spec.type ?? '');
  if (type === 'literal') { if (value !== spec.value) fail(`literal_mismatch:${path}`); return; }
  if (type === 'string' || type === 'nonempty_string') { validateString(value, spec, path); return; }
  if (type === 'datetime-rfc3339') { if (typeof value !== 'string' || Number.isNaN(Date.parse(value))) fail(`invalid_datetime:${path}`); return; }
  if (type === 'nonnegative_integer') { if (!Number.isInteger(value) || Number(value) < 0) fail(`invalid_nonnegative_integer:${path}`); return; }
  if (type === 'enum') { if (!stringArray(spec.values ?? [], 'invalid_enum').includes(String(value))) fail(`invalid_enum_value:${path}`); return; }
  if (type === 'table') { validateTable(asRecord(value, `table_required:${path}`), table(schema, String(spec.schema_ref)), schema, path); return; }
  if (type === 'array') {
    if (!Array.isArray(value)) fail(`array_required:${path}`);
    const items = value as unknown[];
    if (Number.isInteger(spec.min_items) && items.length < Number(spec.min_items)) fail(`too_few_items:${path}`);
    const ref = optionalString(spec.item_schema_ref);
    if (ref) items.forEach((item, index) => validateSchemaRef(item, ref, schema, `${path}[${index}]`));
    const uniqueBy = optionalString(spec.unique_by);
    if (uniqueBy) requireUnique(items.map((item) => asRecord(item, 'array_item_must_be_table')[uniqueBy]), path);
    return;
  }
  validateNamedType(value, type, schema, path);
}

function validateSchemaRef(value: unknown, ref: string, schema: RecordValue, path: string): void {
  const unions = asRecord(schema.unions ?? {}, 'invalid_unions');
  if (ref in unions) {
    const union = asRecord(unions[ref], `invalid_union:${ref}`);
    const record = asRecord(value, `union_item_must_be_table:${path}`);
    const discriminator = String(union.discriminator);
    const variant = stringArray(union.variants, `invalid_variants:${ref}`).find((name) => asRecord(table(schema, name).constants ?? {}, 'invalid_constants')[discriminator] === record[discriminator]);
    if (!variant) fail(`unknown_union_variant:${path}`);
    validateTable(record, table(schema, variant), schema, path);
    return;
  }
  validateTable(asRecord(value, `table_required:${path}`), table(schema, ref), schema, path);
}

function validateNamedType(value: unknown, name: string, schema: RecordValue, path: string): void {
  const definition = asRecord(asRecord(schema.types ?? {}, 'invalid_types')[name], `unknown_type:${name}`);
  const base = String(definition.base ?? '');
  if (base === 'string') { validateString(value, definition, path); return; }
  if (base === 'enum') { if (!stringArray(definition.values, 'invalid_enum').includes(String(value))) fail(`invalid_enum_value:${path}`); return; }
  if (base === 'array') {
    if (!Array.isArray(value)) fail(`array_required:${path}`);
    const items = value as unknown[];
    if (Number.isInteger(definition.min_items) && items.length < Number(definition.min_items)) fail(`too_few_items:${path}`);
    items.forEach((item, index) => validateNamedType(item, String(definition.items), schema, `${path}[${index}]`));
    if (definition.unique === true) requireUnique(items, path);
    return;
  }
  fail(`unsupported_type:${name}`);
}

function validateString(value: unknown, spec: RecordValue, path: string): void {
  if (typeof value !== 'string') fail(`string_required:${path}`);
  if (Number.isInteger(spec.min_length) && value.length < Number(spec.min_length)) fail(`string_too_short:${path}`);
  if (typeof spec.pattern === 'string' && !new RegExp(spec.pattern).test(value)) fail(`pattern_mismatch:${path}`);
}

function applyConstraints(value: RecordValue, schema: RecordValue): void {
  const constraintIds = new Set(
    Array.isArray(schema.constraints)
      ? schema.constraints.map((item) => optionalString(asRecord(item, 'constraint_must_be_table').id)).filter((item): item is string => item !== null)
      : [],
  );
  const needsAgent = ['agent-resume-required', 'turn-order', 'git-reference-consistency'].some((id) => constraintIds.has(id));
  const agent = needsAgent ? asRecord(value.agent, 'agent_must_be_table') : null;
  if (agent && constraintIds.has('agent-resume-required')) {
    const needsResume = ['unstarted','rehydrated','worked','closable','blocked'].includes(String(agent.state));
    if (needsResume !== ('resume_from' in agent)) fail('agent_resume_constraint_failed');
  }
  if (constraintIds.has('uncertainty-required')) {
    const operator = asRecord(value.operator, 'operator_must_be_table');
    const items = Array.isArray(operator.items) ? operator.items.map((item) => asRecord(item, 'operator_item_must_be_table')) : [];
    for (const item of items) {
      if (['result','correction','risk'].includes(String(item.kind))) {
        const needs = ['inferred','speculative','unknown'].includes(String(item.epistemic_status));
        if (needs !== ('uncertainty' in item)) fail(`uncertainty_constraint_failed:${String(item.id)}`);
      }
    }
  }
  if (agent && constraintIds.has('turn-order')) {
    const communication = asRecord(agent.communication, 'communication_must_be_table');
    if (Number(communication.closing_sequence) < Number(communication.opening_sequence)) fail('turn_order_constraint_failed');
  }
  const git = agent && constraintIds.has('git-reference-consistency') && agent.execution ? asRecord(asRecord(agent.execution, 'execution_must_be_table').git, 'git_must_be_table') : null;
  if (git && constraintIds.has('git-reference-consistency')) {
    if ((git.commit === 'committed') !== ('commit_ref' in git)) fail('commit_ref_constraint_failed');
    if ((git.push === 'pushed') !== ('push_ref' in git)) fail('push_ref_constraint_failed');
  }
}

export function handleRequest(request: RecordValue, state: ServerState): unknown {
  if (!request.id && typeof request.method === 'string' && request.method.startsWith('notifications/')) return null;
  try {
    const method = String(request.method);
    let result: unknown;
    if (method === 'initialize') result = { protocolVersion: asRecord(request.params ?? {}, 'params').protocolVersion ?? PROTOCOL_VERSION, capabilities: { tools: {} }, serverInfo: { name: SERVER_NAME, version: SERVER_VERSION } };
    else if (method === 'tools/list') result = { tools: listTools() };
    else if (method === 'tools/call') result = callTool(asRecord(request.params, 'params_required'), state);
    else throw new Error(`unsupported_mcp_method:${method}`);
    return { jsonrpc: '2.0', id: request.id ?? null, result };
  } catch (error) {
    return { jsonrpc: '2.0', id: request.id ?? null, error: { code: -32000, message: error instanceof Error ? error.message : String(error) } };
  }
}

function callTool(params: RecordValue, state: ServerState): RecordValue {
  const name = String(params.name ?? '');
  const args = asRecord(params.arguments ?? {}, 'arguments_must_be_object');
  if (name === 'operator_communication_guidance') {
    const operator = { items: [{ id: 'operator-communication-guidance', kind: 'result', statement: 'For a new response, pass response and optionally schema, persist, created_by, display_policy, and format. Persistence defaults to true. Schema precedence is input, Site-local, then bundled default. Display preference precedence is input flags, .narada/preferences/operator-communication.toml, then bundled defaults. The default display is short prose. format accepts prose, code, or an exact field-name array.', impact: 'The complete response is validated and persisted before display filtering. Replays may choose new display preferences without changing the immutable response. all-unlimited is the only unbounded display policy.', epistemic_status: 'verified', evidence: ['source:operator-communication-mcp'] }] };
    return { content: [{ type: 'text', text: renderProse(operator) }], structuredContent: operator };
  }
  if (name !== 'operator_communication_project') throw new Error(`unknown_tool:${name}`);
  const executed = executeProject(args, state);
  const display = resolveDisplayPreferences(args, state);
  const rendered = applyDisplayPreferences(executed.operator, display);
  const projection = rendered.operator;
  const persistence = executed.persistence;
  const persistenceMeta = 'response' in args
    ? persistence
      ? { status: 'persisted', response_ref: persistence.ref, sequence: persistence.sequence, response_sha256: persistence.response_sha256, schema_sha256: persistence.schema_sha256, char_length: persistence.char_length, storage_kind: persistence.storage_kind, body_path: persistence.body_path }
      : { status: 'disabled' }
    : { status: 'replayed', response_ref: args.response_ref };
  return {
    content: [{ type: 'text', text: rendered.text }],
    structuredContent: projection,
    _meta: { persistence: persistenceMeta, display: { source: display.source, policy: display.policy, format: display.format, fields: display.fields } },
  };
}

export async function runStdioServer(options: RecordValue): Promise<void> {
  const state = createServerState(options);
  let buffer = '';
  process.stdin.setEncoding('utf8');
  for await (const chunk of process.stdin) {
    buffer += chunk;
    let newline = buffer.indexOf('\n');
    while (newline >= 0) {
      const line = buffer.slice(0, newline).trim(); buffer = buffer.slice(newline + 1);
      if (line) process.stdout.write(JSON.stringify(handleRequest(JSON.parse(line), state)) + '\n');
      newline = buffer.indexOf('\n');
    }
  }
}

function tool(name: string, description: string, properties: RecordValue, required: string[] = []): RecordValue {
  return {
    name,
    description,
    inputSchema: { type: 'object', properties, required, additionalProperties: false },
    outputSchema: {
      type: 'object',
      required: ['items'],
      properties: { items: { type: 'array', items: { type: 'object' } } },
      additionalProperties: false,
    },
    annotations: { readOnlyHint: name !== 'operator_communication_project', destructiveHint: false, idempotentHint: name !== 'operator_communication_project', openWorldHint: false },
  };
}
function parseArgs(args: string[]): RecordValue { const out: RecordValue = {}; for (let i=0;i<args.length;i++) { if (args[i] === '--site-root') out.siteRoot=args[++i]; else if (args[i] === '--site-schema-path') out.siteSchemaPath=args[++i]; else if (args[i] === '--site-display-path') out.siteDisplayPath=args[++i]; } return out; }
function asRecord(value: unknown, code = 'object_required'): RecordValue { if (!value || typeof value !== 'object' || Array.isArray(value)) fail(code); return value as RecordValue; }
function table(schema: RecordValue, name: string): RecordValue { return asRecord(asRecord(schema.tables ?? {}, 'invalid_tables')[name], `missing_table:${name}`); }
function stringArray(value: unknown, code: string): string[] { if (!Array.isArray(value) || value.some((x) => typeof x !== 'string')) fail(code); return value as string[]; }
function optionalString(value: unknown): string | null { return typeof value === 'string' && value.length ? value : null; }
function persistencePrincipal(args: RecordValue): string | null { return optionalString(args.created_by) ?? optionalString(process.env.NARADA_AGENT_ID); }
function requireUnique(values: unknown[], path: string): void { if (new Set(values.map((v) => JSON.stringify(v))).size !== values.length) fail(`duplicate_items:${path}`); }
function isWithin(root: string, candidate: string): boolean {
  const rel = relative(resolve(root), resolve(candidate));
  return rel === '' || (!isAbsolute(rel) && rel !== '..' && !rel.startsWith('../') && !rel.startsWith('..\\'));
}
function fail(message: string): never { throw new Error(message); }

export function parseToml(source: string): RecordValue {
  const root: RecordValue = {};
  let current: RecordValue = root;
  const lines = source.replace(/\r\n/g, '\n').split('\n');
  for (let index = 0; index < lines.length; index++) {
    let line = stripComment(lines[index]).trim();
    if (!line) continue;
    if (line.startsWith('[[') && line.endsWith(']]')) {
      const path = line.slice(2, -2).trim().split('.');
      const parent = ensurePath(root, path.slice(0, -1));
      const name = path.at(-1)!;
      const array = Array.isArray(parent[name]) ? parent[name] as unknown[] : [];
      current = {}; array.push(current); parent[name] = array;
      continue;
    }
    if (line.startsWith('[') && line.endsWith(']')) {
      current = ensurePath(root, line.slice(1, -1).trim().split('.'));
      continue;
    }
    const equals = line.indexOf('=');
    if (equals < 1) fail(`toml_invalid_assignment:${index + 1}`);
    const key = line.slice(0, equals).trim();
    let raw = line.slice(equals + 1).trim();
    while (!valueComplete(raw) && index + 1 < lines.length) raw += '\n' + stripComment(lines[++index]).trim();
    current[key] = parseTomlValue(raw, index + 1);
  }
  return root;
}

function parseTomlValue(raw: string, line: number): unknown {
  const value = raw.trim();
  if (value.startsWith('"')) {
    try { return JSON.parse(value); } catch { fail(`toml_invalid_string:${line}`); }
  }
  if (value.startsWith('[')) {
    if (!value.endsWith(']')) fail(`toml_unclosed_array:${line}`);
    return splitTomlArray(value.slice(1, -1)).map((item) => parseTomlValue(item, line));
  }
  if (value === 'true' || value === 'false') return value === 'true';
  if (/^-?\d+$/.test(value)) return Number(value);
  fail(`toml_unsupported_value:${line}`);
}

function splitTomlArray(source: string): string[] {
  const items: string[] = []; let start = 0; let quoted = false; let escaped = false; let depth = 0;
  for (let i = 0; i < source.length; i++) {
    const char = source[i];
    if (quoted) { if (escaped) escaped = false; else if (char === '\\\\') escaped = true; else if (char === '"') quoted = false; continue; }
    if (char === '"') quoted = true;
    else if (char === '[') depth++;
    else if (char === ']') depth--;
    else if (char === ',' && depth === 0) { const item = source.slice(start, i).trim(); if (item) items.push(item); start = i + 1; }
  }
  const last = source.slice(start).trim(); if (last) items.push(last);
  return items;
}

function valueComplete(source: string): boolean {
  let quoted = false; let escaped = false; let depth = 0;
  for (const char of source) {
    if (quoted) { if (escaped) escaped = false; else if (char === '\\\\') escaped = true; else if (char === '"') quoted = false; }
    else if (char === '"') quoted = true; else if (char === '[') depth++; else if (char === ']') depth--;
  }
  return !quoted && depth === 0;
}

function stripComment(line: string): string {
  let quoted = false; let escaped = false;
  for (let i=0;i<line.length;i++) {
    const char=line[i];
    if (quoted) { if (escaped) escaped=false; else if (char==='\\\\') escaped=true; else if (char==='"') quoted=false; }
    else if (char==='"') quoted=true; else if (char==='#') return line.slice(0,i);
  }
  return line;
}

function ensurePath(root: RecordValue, path: string[]): RecordValue {
  let current = root;
  for (const part of path) {
    const next = current[part];
    if (next === undefined) current[part] = {};
    current = asRecord(current[part], `toml_path_collision:${part}`);
  }
  return current;
}

function renderToml(operator: RecordValue): string {
  const lines: string[] = [];
  const items = Array.isArray(operator.items) ? operator.items : [];
  for (const item of items) {
    lines.push('[[items]]');
    for (const [key, value] of Object.entries(asRecord(item, 'operator_item_must_be_table'))) lines.push(`${key} = ${tomlScalar(value)}`);
  }
  return lines.join('\n');
}

function tomlScalar(value: unknown): string {
  if (typeof value === 'string') return JSON.stringify(value);
  if (typeof value === 'boolean' || typeof value === 'number') return String(value);
  if (Array.isArray(value)) return '[' + value.map(tomlScalar).join(', ') + ']';
  fail('operator_projection_contains_non_scalar_table');
}
