import { existsSync, readFileSync } from 'node:fs';
import { dirname, isAbsolute, resolve } from 'node:path';

type JsonRecord = Record<string, any>;

const CONTRACT_SCHEMA = 'narada.mcp_runtime_proxy.orientation_entry_enforcement_contract.v1';

function record(value: unknown): JsonRecord | null {
  return value !== null && typeof value === 'object' && !Array.isArray(value)
    ? value as JsonRecord
    : null;
}

function loadEnforcementContract(): JsonRecord {
  const parsed = record(JSON.parse(readFileSync(
    new URL('./orientation-entry-enforcement-contract.json', import.meta.url),
    'utf8',
  )));
  if (
    parsed?.schema !== CONTRACT_SCHEMA
    || !record(parsed.environment)
    || !record(parsed.state)
    || !record(parsed.coordinate)
    || !record(parsed.rule_sets)
    || !record(parsed.readback_paths)
    || !record(parsed.raw_json)
    || !record(parsed.request_admission)
  ) {
    throw new Error('orientation_entry_enforcement_contract_invalid');
  }
  return parsed;
}

const ENFORCEMENT_CONTRACT = loadEnforcementContract();

function contractRecord(parent: JsonRecord, field: string): JsonRecord {
  const value = record(parent[field]);
  if (!value) throw new Error(`orientation_entry_enforcement_contract_invalid:${field}`);
  return value;
}

function contractString(parent: JsonRecord, field: string): string {
  const value = parent[field];
  if (typeof value !== 'string' || value.length === 0) {
    throw new Error(`orientation_entry_enforcement_contract_invalid:${field}`);
  }
  return value;
}

function contractStringArray(parent: JsonRecord, field: string, optional = false): string[] {
  const value = parent[field];
  if (optional && value === undefined) return [];
  if (!Array.isArray(value) || !value.every((item) => typeof item === 'string')) {
    throw new Error(`orientation_entry_enforcement_contract_invalid:${field}`);
  }
  return value;
}

const environmentContract = contractRecord(ENFORCEMENT_CONTRACT, 'environment');
const stateContract = contractRecord(ENFORCEMENT_CONTRACT, 'state');
const reasonContract = contractRecord(stateContract, 'reasons');
const coordinateContract = contractRecord(ENFORCEMENT_CONTRACT, 'coordinate');
const ruleSets = contractRecord(ENFORCEMENT_CONTRACT, 'rule_sets');
const readbackPaths = contractRecord(ENFORCEMENT_CONTRACT, 'readback_paths');
const rawJsonContract = contractRecord(ENFORCEMENT_CONTRACT, 'raw_json');
const requestAdmission = contractRecord(ENFORCEMENT_CONTRACT, 'request_admission');

function nonEmptyString(value: unknown): value is string {
  return typeof value === 'string' && value.trim().length > 0;
}

function hasOnlyUnicodeScalars(value: unknown): boolean {
  if (typeof value === 'string') {
    for (let index = 0; index < value.length; index += 1) {
      const code = value.charCodeAt(index);
      if (code >= 0xd800 && code <= 0xdbff) {
        const next = value.charCodeAt(index + 1);
        if (!(next >= 0xdc00 && next <= 0xdfff)) return false;
        index += 1;
      } else if (code >= 0xdc00 && code <= 0xdfff) {
        return false;
      }
    }
    return true;
  }
  if (Array.isArray(value)) return value.every(hasOnlyUnicodeScalars);
  const object = record(value);
  return object
    ? Object.entries(object).every(([key, child]) => (
        hasOnlyUnicodeScalars(key) && hasOnlyUnicodeScalars(child)
      ))
    : true;
}

function hasDuplicateObjectKeys(source: string): boolean {
  const stack: Array<{ kind: 'object'; keys: Set<string> } | { kind: 'array' }> = [];
  for (let index = 0; index < source.length; index += 1) {
    const token = source[index];
    if (token === '{') {
      stack.push({ kind: 'object', keys: new Set() });
      continue;
    }
    if (token === '[') {
      stack.push({ kind: 'array' });
      continue;
    }
    if (token === '}' || token === ']') {
      stack.pop();
      continue;
    }
    if (token !== '"') continue;
    const start = index;
    let escaped = false;
    for (index += 1; index < source.length; index += 1) {
      const character = source[index];
      if (escaped) {
        escaped = false;
        continue;
      }
      if (character === '\\') {
        escaped = true;
        continue;
      }
      if (character === '"') break;
    }
    let lookahead = index + 1;
    while (/\s/u.test(source[lookahead] ?? '')) lookahead += 1;
    const frame = stack.at(-1);
    if (source[lookahead] !== ':' || frame?.kind !== 'object') continue;
    const key = JSON.parse(source.slice(start, index + 1));
    if (frame.keys.has(key)) return true;
    frame.keys.add(key);
  }
  return false;
}

function parseJsonFile(path: string): JsonRecord | null {
  try {
    const source = readFileSync(path, 'utf8');
    if (
      contractString(rawJsonContract, 'duplicate_keys') === 'reject'
      && hasDuplicateObjectKeys(source)
    ) {
      return null;
    }
    const parsed = JSON.parse(source);
    return hasOnlyUnicodeScalars(parsed) ? record(parsed) : null;
  } catch {
    return null;
  }
}

type PointerRead = { found: true; value: unknown } | { found: false; value: undefined };

function pointerRead(document: unknown, path: string): PointerRead {
  if (path === '') return { found: true, value: document };
  if (!path.startsWith('/')) return { found: false, value: undefined };
  let current: any = document;
  for (const encoded of path.slice(1).split('/')) {
    const segment = encoded.replaceAll('~1', '/').replaceAll('~0', '~');
    if (
      current === null
      || typeof current !== 'object'
      || !Object.prototype.hasOwnProperty.call(current, segment)
    ) {
      return { found: false, value: undefined };
    }
    current = current[segment];
  }
  return { found: true, value: current };
}

function jsonEquivalent(left: unknown, right: unknown): boolean {
  if (
    left === null
    || right === null
    || typeof left !== 'object'
    || typeof right !== 'object'
  ) {
    return left === right;
  }
  return JSON.stringify(left) === JSON.stringify(right);
}

function validCoordinate(value: unknown): boolean {
  const coordinate = record(value);
  if (!coordinate) return false;
  const stringFields = contractStringArray(coordinateContract, 'non_empty_string_fields');
  const integerField = contractString(coordinateContract, 'positive_safe_integer_field');
  const integerMaximum = coordinateContract.positive_safe_integer_max;
  if (!Number.isSafeInteger(integerMaximum) || integerMaximum < 1) {
    throw new Error('orientation_entry_enforcement_contract_invalid:positive_safe_integer_max');
  }
  return stringFields.every((field) => nonEmptyString(coordinate[field]))
    && Number.isSafeInteger(coordinate[integerField])
    && coordinate[integerField] >= 1
    && coordinate[integerField] <= integerMaximum;
}

function sameCoordinate(left: unknown, right: unknown): boolean {
  if (!validCoordinate(left) || !validCoordinate(right)) return false;
  const leftCoordinate = left as JsonRecord;
  const rightCoordinate = right as JsonRecord;
  return contractStringArray(coordinateContract, 'identity_fields')
    .every((field) => jsonEquivalent(leftCoordinate[field], rightCoordinate[field]));
}

function rulePairs(rules: JsonRecord, field: string): [string, string][] {
  const value = rules[field];
  if (value === undefined) return [];
  if (
    !Array.isArray(value)
    || !value.every((pair) => (
      Array.isArray(pair)
      && pair.length === 2
      && pair.every((item) => typeof item === 'string')
    ))
  ) {
    throw new Error(`orientation_entry_enforcement_contract_invalid:${field}`);
  }
  return value as [string, string][];
}

function validateRuleSet(document: unknown, name: string): boolean {
  const rules = contractRecord(ruleSets, name);
  const equalsRules = rules.equals;
  if (!Array.isArray(equalsRules)) {
    throw new Error(`orientation_entry_enforcement_contract_invalid:${name}:equals`);
  }
  for (const candidate of equalsRules) {
    const rule = record(candidate);
    if (!rule || typeof rule.path !== 'string') {
      throw new Error(`orientation_entry_enforcement_contract_invalid:${name}:equals`);
    }
    const actual = pointerRead(document, rule.path);
    if (!actual.found || !jsonEquivalent(actual.value, rule.value)) return false;
  }
  for (const path of contractStringArray(rules, 'non_empty_strings', true)) {
    const actual = pointerRead(document, path);
    if (!actual.found || !nonEmptyString(actual.value)) return false;
  }
  for (const path of contractStringArray(rules, 'coordinate_paths', true)) {
    const actual = pointerRead(document, path);
    if (!actual.found || !validCoordinate(actual.value)) return false;
  }
  for (const [leftPath, rightPath] of rulePairs(rules, 'equal_paths')) {
    const left = pointerRead(document, leftPath);
    const right = pointerRead(document, rightPath);
    if (!left.found || !right.found || !jsonEquivalent(left.value, right.value)) return false;
  }
  for (const [leftPath, rightPath] of rulePairs(rules, 'equal_coordinates')) {
    const left = pointerRead(document, leftPath);
    const right = pointerRead(document, rightPath);
    if (!left.found || !right.found || !sameCoordinate(left.value, right.value)) return false;
  }
  return true;
}

function readContractPath(document: unknown, field: string): PointerRead {
  return pointerRead(document, contractString(readbackPaths, field));
}

function reason(field: string): string {
  return contractString(reasonContract, field);
}

type RequiredSignal = 'absent' | 'required' | 'not_required' | 'invalid';

function requiredSignal(environment: NodeJS.ProcessEnv): RequiredSignal {
  const name = contractString(environmentContract, 'required_signal');
  const raw = String(environment[name] ?? '').trim().toLowerCase();
  if (!raw) return 'absent';
  if (contractStringArray(environmentContract, 'required_values').includes(raw)) return 'required';
  if (contractStringArray(environmentContract, 'not_required_values').includes(raw)) {
    return 'not_required';
  }
  return 'invalid';
}

export interface OrientationEntryAdmissionState {
  schema: 'narada.mcp_runtime_proxy.orientation_entry_admission.v1';
  required: boolean;
  status: 'not_required' | 'blocked' | 'open';
  ordinary_work_gate: 'open' | 'acknowledgement_required';
  reason: string;
  delivery_receipt_ref: string | null;
  acknowledgement_ref: string | null;
  entry_file: string | null;
  next_call: { surface_id: string; tool: string; arguments: Record<string, never> } | null;
}

function nextCall(): OrientationEntryAdmissionState['next_call'] {
  const value = structuredClone(stateContract.next_call);
  if (
    !record(value)
    || !nonEmptyString(value.surface_id)
    || !nonEmptyString(value.tool)
    || !record(value.arguments)
  ) {
    throw new Error('orientation_entry_enforcement_contract_invalid:next_call');
  }
  return value as OrientationEntryAdmissionState['next_call'];
}

export function inspectOrientationEntryAdmission(
  environment: NodeJS.ProcessEnv = process.env,
): OrientationEntryAdmissionState {
  const stateSchema = contractString(stateContract, 'schema') as OrientationEntryAdmissionState['schema'];
  const entryEnvironmentName = contractString(environmentContract, 'entry_file');
  const configuredEntryFile = String(environment[entryEnvironmentName] ?? '').trim();
  const entryFile = configuredEntryFile ? resolve(configuredEntryFile) : null;
  const signal = requiredSignal(environment);
  const blocked = (
    blockedReason: string,
    deliveryReceiptRef: string | null = null,
  ): OrientationEntryAdmissionState => ({
    schema: stateSchema,
    required: true,
    status: 'blocked',
    ordinary_work_gate: 'acknowledgement_required',
    reason: blockedReason,
    delivery_receipt_ref: deliveryReceiptRef,
    acknowledgement_ref: null,
    entry_file: entryFile,
    next_call: nextCall(),
  });

  if (signal === 'invalid') return blocked(reason('required_signal_invalid'));
  if (signal === 'not_required' && configuredEntryFile) {
    return blocked(reason('required_signal_conflict'));
  }
  if (signal === 'required' && !configuredEntryFile) {
    return blocked(reason('required_packet_missing'));
  }
  if (!configuredEntryFile) {
    return {
      schema: stateSchema,
      required: false,
      status: 'not_required',
      ordinary_work_gate: 'open',
      reason: reason('not_supplied'),
      delivery_receipt_ref: null,
      acknowledgement_ref: null,
      entry_file: null,
      next_call: null,
    };
  }
  if (!isAbsolute(configuredEntryFile)) return blocked(reason('entry_path_invalid'));
  if (!existsSync(entryFile!)) return blocked(reason('entry_unavailable'));
  const packet = parseJsonFile(entryFile!);
  if (!packet || !validateRuleSet(packet, 'packet_header')) {
    return blocked(reason('entry_invalid'));
  }
  const deliveryRefRead = readContractPath(packet, 'delivery_receipt_ref');
  const deliveryRef = deliveryRefRead.found && nonEmptyString(deliveryRefRead.value)
    ? deliveryRefRead.value
    : null;
  if (!validateRuleSet(packet, 'delivery_binding')) {
    return blocked(reason('delivery_binding_invalid'), deliveryRef);
  }
  if (!validateRuleSet(packet, 'acknowledgement_projection_ref')) {
    return blocked(reason('acknowledgement_ref_invalid'), deliveryRef);
  }
  const projectionPathRead = readContractPath(packet, 'acknowledgement_projection_path');
  if (!projectionPathRead.found || !nonEmptyString(projectionPathRead.value)) {
    return blocked(reason('acknowledgement_ref_invalid'), deliveryRef);
  }
  const acknowledgementPath = resolve(dirname(entryFile!), projectionPathRead.value);
  if (dirname(acknowledgementPath) !== dirname(entryFile!)) {
    return blocked(reason('acknowledgement_ref_invalid'), deliveryRef);
  }
  if (!existsSync(acknowledgementPath)) {
    return blocked(reason('acknowledgement_required'), deliveryRef);
  }
  const acknowledgement = parseJsonFile(acknowledgementPath);
  if (
    !acknowledgement
    || !validateRuleSet({ packet, acknowledgement }, 'acknowledgement_projection')
  ) {
    return blocked(reason('acknowledgement_invalid'), deliveryRef);
  }
  const acknowledgementRefRead = readContractPath(
    acknowledgement,
    'acknowledgement_ref',
  );
  if (!acknowledgementRefRead.found || !nonEmptyString(acknowledgementRefRead.value)) {
    return blocked(reason('acknowledgement_invalid'), deliveryRef);
  }
  return {
    schema: stateSchema,
    required: true,
    status: 'open',
    ordinary_work_gate: 'open',
    reason: reason('acknowledged'),
    delivery_receipt_ref: deliveryRef,
    acknowledgement_ref: acknowledgementRefRead.value,
    entry_file: entryFile,
    next_call: null,
  };
}

function toolCallAdmitted(surfaceId: string | null | undefined, toolName: unknown): boolean {
  if (typeof toolName !== 'string') return false;
  if (contractStringArray(requestAdmission, 'proxy_tool_calls').includes(toolName)) return true;
  const bindings = requestAdmission.allowed_tool_calls;
  if (!Array.isArray(bindings)) {
    throw new Error('orientation_entry_enforcement_contract_invalid:allowed_tool_calls');
  }
  return bindings.some((candidate) => {
    const binding = record(candidate);
    if (!binding || binding.surface_id !== surfaceId) return false;
    return contractStringArray(binding, 'tool_names').includes(toolName);
  });
}

export function admitOrientationRequest({
  surfaceId,
  messageKind,
  method,
  params,
  environment = process.env,
}: {
  surfaceId: string | null | undefined;
  messageKind: 'request' | 'notification';
  method: string | null | undefined;
  params?: unknown;
  environment?: NodeJS.ProcessEnv;
}): { admitted: boolean; state: OrientationEntryAdmissionState } {
  const state = inspectOrientationEntryAdmission(environment);
  if (state.ordinary_work_gate === 'open') return { admitted: true, state };
  const methodName = String(method ?? '');
  if (
    messageKind === 'request'
    && contractStringArray(requestAdmission, 'allowed_request_methods').includes(methodName)
  ) {
    return { admitted: true, state };
  }
  if (
    messageKind === 'notification'
    && contractStringArray(requestAdmission, 'allowed_notification_methods').includes(methodName)
  ) {
    return { admitted: true, state };
  }
  const admitted = messageKind === 'request'
    && methodName === 'tools/call'
    && toolCallAdmitted(surfaceId, record(params)?.name);
  return { admitted, state };
}

export function orientationEntryEnforcementContract(): Readonly<JsonRecord> {
  return ENFORCEMENT_CONTRACT;
}
