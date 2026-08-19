import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { Ajv2020 } from 'ajv/dist/2020.js';

const domainPath = fileURLToPath(new URL('../../domain.json', import.meta.url));
const schemaPath = fileURLToPath(new URL('../../domain.schema.json', import.meta.url));
const domain = JSON.parse(readFileSync(domainPath, 'utf8')) as Record<string, any>;
const descriptorSchema = JSON.parse(readFileSync(schemaPath, 'utf8')) as Record<string, unknown>;

const ajv = new Ajv2020({ strict: false, allErrors: true });
const validate = ajv.compile(descriptorSchema);
const valid = validate(domain);
assert.equal(valid, true, `domain.json fails narada.ledger-domain.v1: ${JSON.stringify(validate.errors)}`);

assert.equal(domain.schema, 'narada.ledger-domain.v1');
assert.equal(domain.identity.domain_id, 'epistemic-graph');
assert.equal(domain.identity.tool_prefix, 'epistemic_graph');
assert.equal(domain.identity.error_schema_id, 'narada.epistemic.error.v1');

// Every tool name carries the domain tool prefix, and the tool list is the
// engine's generation target: 21 tools, exactly one guidance tool.
assert.equal(domain.tools.length, 21);
for (const tool of domain.tools) {
  assert.ok(tool.name.startsWith(domain.identity.tool_prefix + '_'), `tool name lacks prefix: ${tool.name}`);
  assert.equal(tool.annotations.destructiveHint, false, `${tool.name} destructiveHint`);
  assert.equal(tool.annotations.readOnlyHint, tool.annotations.idempotentHint, `${tool.name} read/idempotent annotation mismatch`);
}
assert.equal(domain.tools.filter((tool: any) => tool.class === 'guidance').length, 1);

// The operation vocabulary is closed: five operation kinds, and the embedded
// operation oneOf schema covers exactly those kinds.
assert.deepEqual(domain.operations.kinds, ['entity.declare', 'relation.declare', 'assessment.record', 'test_outcome.record', 'sweep.record']);
const variants = domain.operations.schema.oneOf as any[];
assert.equal(variants.length, 5);
assert.deepEqual(variants.map((variant) => variant.properties.op.const), domain.operations.kinds);

// Guidance stays byte-identical to narada.epistemic.guidance.v2.
assert.equal(domain.guidance.schema_id, 'narada.epistemic.guidance.v2');
assert.equal(domain.guidance.fields.minimal_example.tool, 'epistemic_graph_submit_review_admit');
assert.equal(domain.guidance.fields.minimal_example.arguments.operations[0].op, 'entity.declare');
assert.equal(domain.guidance.fields.minimal_example.arguments.operations[2].op, 'relation.declare');
assert.ok(domain.guidance.fields.concurrency_rule.includes('ledger_head'));

console.log('ledger-domain-epistemic tests passed');
