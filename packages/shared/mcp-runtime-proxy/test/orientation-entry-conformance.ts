import { existsSync, mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { dirname, join, relative, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import type { OrientationEntryAdmissionState } from '../src/orientation-entry-admission.js';

type Mutation = {
  op: 'set' | 'remove';
  path: string;
  value?: unknown;
};

type MaterialMode = 'environment_absent' | 'missing' | 'absent' | 'malformed' | 'literal' | 'base' | 'raw_replace';

type RawReplacement = {
  search: string;
  replacement: string;
  replace_all?: boolean;
};

type MaterialSpec = {
  mode: MaterialMode;
  environment?: 'absolute' | 'relative' | 'padded_absolute';
  value?: unknown;
  mutations?: Mutation[];
  raw_replace?: RawReplacement;
};

export type OrientationEntryConformanceCase = {
  id: string;
  required_signal?: 'required' | 'not_required' | 'invalid' | 'absent';
  entry: MaterialSpec;
  acknowledgement: MaterialSpec;
  expected: {
    status: 'not_required' | 'blocked' | 'open';
    reason: string;
    delivery_receipt_ref: string | null;
    acknowledgement_ref: string | null;
    call_posture: 'open' | 'orientation_only';
  };
};

type OrientationEntryConformanceCorpus = {
  schema: 'narada.mcp_runtime_proxy.orientation_entry_conformance.v1';
  base: {
    entry_packet: Record<string, unknown>;
    acknowledgement: Record<string, unknown>;
  };
  cases: OrientationEntryConformanceCase[];
};

export type MaterializedOrientationEntryCase = {
  entryRoot: string;
  entryFile: string;
  acknowledgementFile: string;
  environment: Record<string, string>;
};

const CORPUS_FILE = 'orientation-entry-admission.v1.json';

function corpusPath(): string {
  const here = dirname(fileURLToPath(import.meta.url));
  const candidates = [
    join(here, 'fixtures', CORPUS_FILE),
    resolve(here, '..', '..', 'test', 'fixtures', CORPUS_FILE),
  ];
  const found = candidates.find((candidate) => existsSync(candidate));
  if (!found) throw new Error(`orientation_entry_conformance_corpus_missing:${candidates.join(',')}`);
  return found;
}

function parseCorpus(value: unknown): OrientationEntryConformanceCorpus {
  if (!value || typeof value !== 'object' || Array.isArray(value)) {
    throw new Error('orientation_entry_conformance_corpus_invalid');
  }
  const corpus = value as OrientationEntryConformanceCorpus;
  if (
    corpus.schema !== 'narada.mcp_runtime_proxy.orientation_entry_conformance.v1'
    || !corpus.base?.entry_packet
    || !corpus.base?.acknowledgement
    || !Array.isArray(corpus.cases)
    || corpus.cases.length === 0
  ) {
    throw new Error('orientation_entry_conformance_corpus_invalid');
  }
  const ids = new Set<string>();
  for (const testCase of corpus.cases) {
    if (!testCase?.id || ids.has(testCase.id)) {
      throw new Error(`orientation_entry_conformance_case_id_invalid:${String(testCase?.id)}`);
    }
    ids.add(testCase.id);
    if (!['not_required', 'blocked', 'open'].includes(testCase.expected?.status)) {
      throw new Error(`orientation_entry_conformance_case_expected_invalid:${testCase.id}`);
    }
  }
  return corpus;
}

export function loadOrientationEntryConformanceCorpus(): OrientationEntryConformanceCorpus {
  return parseCorpus(JSON.parse(readFileSync(corpusPath(), 'utf8')));
}

function pointerSegments(path: string): string[] {
  if (!path.startsWith('/') || path === '/') {
    throw new Error(`orientation_entry_conformance_pointer_invalid:${path}`);
  }
  return path.slice(1).split('/').map((segment) => (
    segment.replaceAll('~1', '/').replaceAll('~0', '~')
  ));
}

function applyMutations(value: unknown, mutations: Mutation[] = []): unknown {
  const result: any = structuredClone(value);
  for (const mutation of mutations) {
    const segments = pointerSegments(mutation.path);
    let parent: any = result;
    for (const segment of segments.slice(0, -1)) {
      if (!parent || typeof parent !== 'object' || !(segment in parent)) {
        throw new Error(`orientation_entry_conformance_pointer_missing:${mutation.path}`);
      }
      parent = parent[segment];
    }
    const field = segments.at(-1)!;
    if (!parent || typeof parent !== 'object') {
      throw new Error(`orientation_entry_conformance_pointer_parent_invalid:${mutation.path}`);
    }
    if (mutation.op === 'remove') {
      if (!(field in parent)) {
        throw new Error(`orientation_entry_conformance_pointer_missing:${mutation.path}`);
      }
      delete parent[field];
      continue;
    }
    parent[field] = structuredClone(mutation.value);
  }
  return result;
}

function materialize(path: string, spec: MaterialSpec, base: unknown): void {
  if (spec.mode === 'missing' || spec.mode === 'absent' || spec.mode === 'environment_absent') return;
  if (spec.mode === 'malformed') {
    writeFileSync(path, '{', 'utf8');
    return;
  }
  const value = spec.mode === 'literal'
    ? spec.value
    : applyMutations(base, spec.mutations);
  let source = JSON.stringify(value);
  if (spec.mode === 'raw_replace') {
    const replacement = spec.raw_replace;
    if (!replacement || !source.includes(replacement.search)) {
      throw new Error(`orientation_entry_conformance_raw_search_missing:${path}`);
    }
    if (!replacement.replace_all && source.indexOf(replacement.search) !== source.lastIndexOf(replacement.search)) {
      throw new Error(`orientation_entry_conformance_raw_search_ambiguous:${path}`);
    }
    source = replacement.replace_all
      ? source.split(replacement.search).join(replacement.replacement)
      : source.replace(replacement.search, replacement.replacement);
  }
  writeFileSync(path, source, 'utf8');
}

export function materializeOrientationEntryCase({
  root,
  corpus,
  testCase,
}: {
  root: string;
  corpus: OrientationEntryConformanceCorpus;
  testCase: OrientationEntryConformanceCase;
}): MaterializedOrientationEntryCase {
  const entryRoot = join(root, '.ai', 'runtime', 'orientation-entry', 'carrier-fixture');
  const entryFile = join(entryRoot, 'entry.json');
  const acknowledgementFile = join(entryRoot, 'acknowledgement.json');
  mkdirSync(entryRoot, { recursive: true });
  rmSync(entryFile, { force: true });
  rmSync(acknowledgementFile, { force: true });
  materialize(entryFile, testCase.entry, corpus.base.entry_packet);
  materialize(acknowledgementFile, testCase.acknowledgement, corpus.base.acknowledgement);
  const environment: Record<string, string> = {};
  if (testCase.entry.mode !== 'environment_absent') {
    const style = testCase.entry.environment ?? 'absolute';
    environment.NARADA_ORIENTATION_ENTRY_FILE = style === 'relative'
      ? relative(process.cwd(), entryFile)
      : style === 'padded_absolute'
        ? `  ${entryFile}  `
        : entryFile;
  }
  const requiredSignal = testCase.required_signal
    ?? (testCase.entry.mode === 'environment_absent' ? 'absent' : 'required');
  if (requiredSignal !== 'absent') {
    environment.NARADA_ORIENTATION_REQUIRED = requiredSignal === 'required'
      ? '1'
      : requiredSignal === 'not_required'
        ? '0'
        : 'invalid';
  }
  return { entryRoot, entryFile, acknowledgementFile, environment };
}

export function expectedOrientationEntryState(
  testCase: OrientationEntryConformanceCase,
  entryFile: string,
): OrientationEntryAdmissionState {
  const blocked = testCase.expected.status === 'blocked';
  return {
    schema: 'narada.mcp_runtime_proxy.orientation_entry_admission.v1',
    required: testCase.expected.status !== 'not_required',
    status: testCase.expected.status,
    ordinary_work_gate: blocked ? 'acknowledgement_required' : 'open',
    reason: testCase.expected.reason,
    delivery_receipt_ref: testCase.expected.delivery_receipt_ref,
    acknowledgement_ref: testCase.expected.acknowledgement_ref,
    entry_file: testCase.entry.mode === 'environment_absent' ? null : resolve(entryFile),
    next_call: blocked ? {
      surface_id: 'agent-context',
      tool: 'agent_orientation_read',
      arguments: {},
    } : null,
  };
}

export function expectedOrientationCallAdmission(
  testCase: OrientationEntryConformanceCase,
  call: 'ordinary' | 'orientation_read' | 'orientation_acknowledge' | 'transport' | 'hidden',
): boolean {
  if (testCase.expected.call_posture === 'open') return true;
  return call === 'orientation_read'
    || call === 'transport';
}
