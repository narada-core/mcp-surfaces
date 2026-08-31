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
// engine's generation target: 27 tools, exactly one guidance tool.
assert.equal(domain.tools.length, 27);
for (const tool of domain.tools) {
  assert.ok(tool.name.startsWith(domain.identity.tool_prefix + '_'), `tool name lacks prefix: ${tool.name}`);
  assert.equal(tool.annotations.destructiveHint, false, `${tool.name} destructiveHint`);
  if (tool.annotations.readOnlyHint) {
    assert.equal(tool.annotations.idempotentHint, true, `${tool.name} read-only tools must be idempotent`);
  }
}
assert.equal(domain.tools.filter((tool: any) => tool.class === 'guidance').length, 1);

const queryTool = domain.tools.find((tool: any) => tool.name === 'epistemic_graph_query');
const queryShape = queryTool.inputSchema.properties.query;
const queryFind = queryShape.properties.find;
assert.equal(queryFind.maxItems, 64);
assert.equal(queryShape.properties.inputs.maxProperties, 64);
assert.equal(queryShape.properties.order_by.maxItems, 64);
assert.equal(queryShape.properties.order_by.items.properties.direction.enum.join(','), 'asc,desc');
assert.equal(queryTool.inputSchema.properties.kinds.maxItems, 64);
assert.equal(queryTool.inputSchema.properties.kinds.minItems, 1);
assert.equal(queryTool.inputSchema.properties.match.properties.kinds.maxItems, 64);
assert.equal(queryTool.inputSchema.properties.match.properties.kinds.minItems, 1);
assert.deepEqual(queryShape.properties.where.items.$ref, '#/properties/query/$defs/clause');
assert.ok(queryFind.items.oneOf.some((branch: any) => branch.properties?.pull), 'raw pull form is discoverable');
assert.ok(queryFind.items.oneOf.some((branch: any) => branch.properties?.one_of), 'raw one_of term form is discoverable');
assert.deepEqual(queryFind.items.oneOf.find((branch: any) => branch.properties?.pull).properties.pull.properties.target_kind.enum, ['entity', 'relation', 'record']);
const batchTool = domain.tools.find((tool: any) => tool.name === 'epistemic_graph_query_batch');
const batchShape = batchTool.inputSchema.properties.queries.items.properties.query;
const batchFind = batchShape.properties.find;
assert.ok(batchFind.items.oneOf.some((branch: any) => branch.properties?.pull), 'batch raw pull form is discoverable');
assert.ok(batchFind.items.oneOf.some((branch: any) => branch.properties?.one_of), 'batch raw one_of term form is discoverable');
assert.equal(batchShape.properties.inputs.maxProperties, 64);
assert.equal(batchShape.properties.order_by.maxItems, 64);
assert.equal(batchTool.inputSchema.properties.queries.items.properties.kinds.maxItems, 64);
assert.equal(batchTool.inputSchema.properties.queries.items.properties.kinds.minItems, 1);
assert.equal(batchTool.inputSchema.properties.queries.items.properties.match.properties.kinds.maxItems, 64);
assert.equal(batchTool.inputSchema.properties.queries.items.properties.match.properties.kinds.minItems, 1);
assert.ok(batchTool.inputSchema.properties.queries.items.properties.match.properties.participant, 'batch match participant is discoverable');
assert.deepEqual(batchShape.properties.where.items.$ref, '#/properties/queries/items/properties/query/$defs/clause');
const validRawQuery = {
  find: ['?message'],
  where: [{ triple: { subject: '?message', attribute: 'kind', object: 'claim' } }],
};
const validateQueryInput = ajv.compile(queryTool.inputSchema);
assert.equal(validateQueryInput({ query: validRawQuery, template: 'inbox' }), true);
assert.equal(validateQueryInput({ template: 'inbox' }), true);
assert.equal(validateQueryInput({ template: 'inbox', match: { participant: 'marici.Nima' } }), true);
assert.equal(validateQueryInput({ template: 'thread', root: 'communication:root' }), true);
assert.equal(validateQueryInput({ template: 'thread' }), true);
const validateBatchInput = ajv.compile(batchTool.inputSchema);
assert.equal(validateBatchInput({ queries: [{ query: validRawQuery, template: 'inbox' }] }), true);
assert.equal(
  validateBatchInput({ queries: [{ template: 'inbox', match: { participant: 'marici.Nima' } }] }),
  true,
);
assert.equal(domain.query.max_one_of_values, 64);
assert.equal(domain.query.max_predicate_depth, 8);

// The operation vocabulary is closed: six operation kinds, and the embedded
// operation oneOf schema covers exactly those kinds.
assert.deepEqual(domain.operations.kinds, ['entity.declare', 'entity.kind_canonicalize', 'relation.declare', 'assessment.record', 'test_outcome.record', 'sweep.record']);
const variants = domain.operations.schema.oneOf as any[];
assert.equal(variants.length, 6);
assert.deepEqual(variants.map((variant) => variant.properties.op.const), domain.operations.kinds);
assert.equal(domain.query.communication.canonical_kind, 'narada.epistemic:communication');
assert.deepEqual(domain.query.communication.legacy_read_aliases, ['communication', 'marici:communication']);
assert.equal(domain.query.communication.legacy_write_policy, 'reject_with_replacement');
assert.ok(domain.tools.some((tool: any) => tool.name === 'epistemic_graph_communication_migration_preflight'));
assert.ok(domain.tools.some((tool: any) => tool.name === 'epistemic_graph_communication_migrate'));
assert.ok(domain.entities.core_kinds.includes('research_issue'));
assert.ok(domain.relations.core.includes('issue_child_of'));
assert.ok(domain.relations.core.includes('blocked_by'));
const issueTransition = domain.tools.find((tool: any) => tool.name === 'epistemic_graph_issue_tree_transition');
const validateIssueTransition = ajv.compile(issueTransition.inputSchema);
assert.equal(validateIssueTransition({
  actor: 'operator',
  authority_basis: { kind: 'operator_direct_instruction' },
  tree_id: 'rh-program',
  nodes: [{ node_id: 'issue:1', title: 'Establish seam control', version: 1, score: 0.8 }],
}), true);
assert.equal(validateIssueTransition({
  actor: 'operator',
  authority_basis: { kind: 'operator_direct_instruction' },
  tree_id: 'rh-program',
  nodes: [{ node_id: 'issue:1', title: 'Bad state', version: 1, state: 'unknown' }],
}), false);
const submitReviewAdmit = domain.tools.find((tool: any) => tool.name === 'epistemic_graph_submit_review_admit');
const validateSubmitReviewAdmit = ajv.compile(submitReviewAdmit.inputSchema);
assert.equal(validateSubmitReviewAdmit({ payload_ref: 'mcp_payload:epistemic-submit@v1' }), true);
assert.equal(
  validateSubmitReviewAdmit({
    payload_ref: 'mcp_payload:epistemic-submit@v1',
    actor: 'ambiguous',
  }),
  false,
);
assert.equal(validateSubmitReviewAdmit({ payload_ref: 'not-a-payload-ref' }), false);
const proposalSubmit = domain.tools.find((tool: any) => tool.name === 'epistemic_graph_proposal_submit');
const validateProposalSubmit = ajv.compile(proposalSubmit.inputSchema);
assert.equal(validateProposalSubmit({ payload_ref: 'mcp_payload:epistemic-proposal@v1' }), true);
const operationsBatch = domain.tools.find((tool: any) => tool.name === 'epistemic_graph_operations_batch');
const validateOperationsBatch = ajv.compile(operationsBatch.inputSchema);
assert.equal(validateOperationsBatch({
  actor: 'operator',
  authority_basis: { kind: 'operator_direct_instruction' },
  batches: [{
    defaults: { op: 'entity.declare', kind: 'claim', version: 'v1' },
    columns: ['title', 'locator'],
    rows: [['First claim', 'urn:claim:first'], ['Second claim', 'urn:claim:second']],
  }],
}), true);
assert.equal(validateOperationsBatch({
  actor: 'operator',
  authority_basis: { kind: 'operator_direct_instruction' },
  batches: [{ defaults: {}, columns: [], rows: [] }],
}), false);

// Guidance stays byte-identical to narada.epistemic.guidance.v2.
assert.equal(domain.guidance.schema_id, 'narada.epistemic.guidance.v2');
assert.equal(domain.guidance.fields.minimal_example.tool, 'epistemic_graph_submit_review_admit');
assert.equal(domain.guidance.fields.minimal_example.arguments.operations[0].op, 'entity.declare');
assert.equal(domain.guidance.fields.minimal_example.arguments.operations[2].op, 'relation.declare');
assert.ok(domain.guidance.fields.concurrency_rule.includes('ledger_head'));

console.log('ledger-domain-epistemic tests passed');
