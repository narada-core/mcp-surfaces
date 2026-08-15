import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import {
  buildBoundedToolResult,
  buildOutputRefToolContent,
  createTransportScope,
  enforceInlinePayloadLimit,
  listPayloadTools,
  listOutputResources,
  listOutputTools,
  outputShow,
  outputShowAsync,
  payloadCreate,
  payloadDerive,
  payloadShow,
  prunePayloadWorkspaces,
  readOutputResource,
  resolveToolPayloadArgs,
} from '../src/mcp-payload-file.js';
import { boundedCollection } from '../src/bounded-collection.js';

const exactly20000 = 'x'.repeat(20_000);
const over20000 = 'x'.repeat(20_001);

assert.doesNotThrow(() => enforceInlinePayloadLimit({
  toolName: 'representative_tool',
  args: { summary: exactly20000 },
}));

assert.throws(
  () => enforceInlinePayloadLimit({
    toolName: 'representative_tool',
    args: { summary: over20000 },
  }),
  /inline_payload_too_long: field=summary length=20001 threshold=20000 remediation=call mcp_payload_create then retry_with_payload_ref.*mcp_payload_create_args=.*"summary":"<move original summary here>".*retry_args=.*"payload_ref":"mcp_payload:<id>@v1"/
);

assert.throws(
  () => enforceInlinePayloadLimit({
    toolName: 'task_lifecycle_review',
    args: { findings: [{ severity: 'note', description: over20000 }] },
  }),
  /mcp_payload_create_args=.*"findings":\[\{"description":"<move original findings\.0\.description here>"\}\]/
);

assert.doesNotThrow(() => enforceInlinePayloadLimit({
  toolName: 'mcp_payload_create',
  args: { payload: { summary: over20000 } },
  allowPayloadCreation: true,
}));

assert.equal(listOutputTools()[0].name, 'mcp_output_show');
assert.equal('target_site_root' in listOutputTools()[0].inputSchema.properties, false);
assert.equal(listOutputTools()[0].inputSchema.properties.limit.maximum, 20_000);
const bounded = boundedCollection(['a', 'b', 'c'], { offset: 1, limit: 1 });
assert.deepEqual(bounded.items, ['b']);
assert.equal(bounded.total_count, 3);
assert.equal(bounded.has_more, true);
assert.equal(bounded.next_offset, 2);
assert.equal(bounded.truncation_reason, 'page_limit');
assert.throws(
  () => buildOutputRefToolContent({ value: { status: 'ok' }, limit: 30_000 }),
  /inline_output_limit_exceeds_transport_maximum/
);

const payloadTools = listPayloadTools();
const payloadTool = (name: string) => payloadTools.find((tool) => tool.name === name);
assert.equal(payloadTool('mcp_payload_create')?.annotations.readOnlyHint, false);
assert.equal(payloadTool('mcp_payload_show')?.annotations.readOnlyHint, true);
assert.equal(payloadTool('mcp_payload_derive')?.annotations.readOnlyHint, false);
assert.equal(payloadTool('mcp_payload_validate')?.annotations.readOnlyHint, true);
assert.equal(payloadTool('mcp_payload_validate')?.annotations.idempotentHint, true);
for (const tool of payloadTools) {
  assert.equal(tool.inputSchema.anyOf, undefined, `${tool.name} must not expose root anyOf to Moonshot clients`);
  assert.equal(tool.inputSchema.oneOf, undefined, `${tool.name} must not expose root oneOf to Moonshot clients`);
  assert.equal(tool.inputSchema.allOf, undefined, `${tool.name} must not expose root allOf to Moonshot clients`);
}
assert.equal(payloadTool('mcp_payload_create')?.inputSchema.properties.payload.type, 'object');
assert.equal(payloadTool('mcp_payload_create')?.inputSchema.properties.payload_json.type, 'string');
assert.equal(payloadTool('mcp_payload_derive')?.inputSchema.properties.overlay.type, 'object');
assert.equal(payloadTool('mcp_payload_derive')?.inputSchema.properties.overlay_json.type, 'string');
assert.equal(payloadTool('mcp_payload_derive')?.inputSchema.properties.delete_paths.type, 'array');

const retentionRoot = mkdtempSync(join(tmpdir(), 'narada-mcp-payload-retention-'));
try {
  for (const payloadId of ['retained-test-a', 'retained-test-b', 'retained-test-c']) {
    payloadCreate({ siteRoot: retentionRoot, args: { payload_id: payloadId, payload: { payloadId } } });
  }
  const retention = prunePayloadWorkspaces({
    siteRoot: retentionRoot,
    payloadIdPrefix: 'retained-test-',
    maxEntries: 1,
    maxAgeMs: 24 * 60 * 60 * 1000,
  });
  assert.equal(retention.considered_count, 3);
  assert.equal(retention.retained_count, 1);
  assert.equal(retention.removed_count, 2);
  assert.equal(retention.retained_payload_ids.length, 1);
  assert.equal(retention.removed_payload_ids.length, 2);
} finally {
  rmSync(retentionRoot, { recursive: true, force: true });
}

const tempRoot = mkdtempSync(join(tmpdir(), 'narada-mcp-transport-'));
try {
  const scope = createTransportScope({ siteRoot: tempRoot });
  assert.equal(Object.isFrozen(scope), true);
  assert.equal(scope.siteRoot, tempRoot);
  assert.throws(
    () => buildOutputRefToolContent({ scope, siteRoot: tempRoot, value: { status: 'ok' } }),
    /transport_scope_cannot_be_combined_with_legacy_scope_overrides/
  );
  assert.throws(
    () => createTransportScope({ siteRoot: tempRoot, outputDir: '../outside' }),
    /output_directory_outside_site_root/
  );
  const longValue = { status: 'ok', output: 'x'.repeat(5_000) };
  const longResult = buildOutputRefToolContent({
    siteRoot: tempRoot,
    toolName: 'representative_tool',
    value: longValue,
    createdBy: 'narada-test.agent',
  });
  const envelope = JSON.parse(longResult.content[0].text);
  const structuredEnvelope = (longResult as { structuredContent: Record<string, unknown> }).structuredContent;
  assert.equal(envelope.truncated, true);
  assert.match(String(envelope.output_ref), /^mcp_output:/);
  assert.equal(envelope.reader_tool, 'mcp_output_show');
  assert.equal(typeof envelope.output_text, 'string');
  assert.equal(envelope.output_text.length > 0, true);
  assert.match(String(structuredEnvelope.output_ref), /^mcp_output:/);
  assert.equal(structuredEnvelope.schema, 'narada.producer_output_page.v1');
  assert.equal(structuredEnvelope.offset, 0);
  assert.equal(typeof structuredEnvelope.next_offset, 'number');
  assert.equal(structuredEnvelope.transport_next_offset, structuredEnvelope.next_offset);
  assert.equal((structuredEnvelope.next_offset as number) <= 2_000, true);
  assert.equal((structuredEnvelope.output_text as string).length, structuredEnvelope.next_offset);
  assert.equal(typeof structuredEnvelope.site_root, 'string');
  assert.equal(structuredEnvelope.reader_tool, 'mcp_output_show');
  assert.match(String(structuredEnvelope.read_command), /mcp_output_show/);
  assert.equal(longResult.content.length, 1);
  assert.equal(longResult.content[0].type, 'text');
  assert.deepEqual((buildOutputRefToolContent({ value: { status: 'ok', compact: true } }) as { structuredContent: Record<string, unknown> }).structuredContent, { status: 'ok', compact: true });
  assert.equal(
    Buffer.byteLength(longResult.content[0].text, 'utf8')
      + Buffer.byteLength(JSON.stringify((longResult as { structuredContent: Record<string, unknown> }).structuredContent), 'utf8') <= 32 * 1024,
    true
  );

  const boundedSmall = buildBoundedToolResult({
    siteRoot: tempRoot,
    toolName: 'bounded_small_tool',
    value: { status: 'ok', compact: true },
  });
  assert.deepEqual((boundedSmall as { structuredContent: Record<string, unknown> }).structuredContent, { status: 'ok', compact: true });

  const boundedLarge = buildBoundedToolResult({
    siteRoot: tempRoot,
    toolName: 'bounded_large_tool',
    value: { status: 'ok', output: 'y'.repeat(5_000) },
    readerTool: 'surface_output_show',
  });
  const boundedLargeEnvelope = JSON.parse(boundedLarge.content[0].text);
  const boundedLargeStructured = (boundedLarge as { structuredContent: Record<string, unknown> }).structuredContent;
  assert.equal(boundedLargeEnvelope.schema, 'narada.producer_output_page.v1');
  assert.equal(boundedLargeStructured.schema, 'narada.producer_output_page.v1');
  assert.equal(boundedLargeStructured.reader_tool, 'surface_output_show');
  assert.equal(JSON.stringify(boundedLargeStructured).includes('y'.repeat(5_000)), false);
  assert.equal((boundedLarge.content[0].text as string).length <= 2_000, true);
  assert.match(String(boundedLargeStructured.output_ref), /^mcp_output:/);
  assert.equal(typeof boundedLargeStructured.output_text, 'string');
  assert.equal((boundedLargeStructured.output_text as string).length <= 2_000, true);

  const shownLongResult = outputShow({ siteRoot: tempRoot, args: { ref: structuredEnvelope.output_ref, limit: 10 } });
  assert.equal(shownLongResult.schema, 'narada.mcp_output_page.v1');
  assert.equal(shownLongResult.output_text.length, 10);
  assert.equal(shownLongResult.next_offset, 10);
  assert.throws(
    () => outputShow({ siteRoot: tempRoot, args: { ref: structuredEnvelope.output_ref, limit: 0 } }),
    /output_limit_must_be_positive_integer/
  );
  assert.throws(
    () => outputShow({ siteRoot: tempRoot, args: { ref: structuredEnvelope.output_ref, limit: 30_000 } }),
    /output_limit_exceeds_transport_maximum/
  );
  assert.throws(
    () => outputShow({ siteRoot: tempRoot, args: { ref: structuredEnvelope.output_ref, limit: '10' } }),
    /output_limit_must_be_positive_integer/
  );
  assert.throws(
    () => outputShow({ siteRoot: tempRoot, args: { ref: structuredEnvelope.output_ref, offset: '1' } }),
    /offset_must_be_non_negative_integer/
  );
  assert.throws(
    () => outputShow({ siteRoot: tempRoot, args: { ref: structuredEnvelope.output_ref, target_site_root: tempRoot } }),
    /output_target_site_root_not_supported/
  );
  assert.throws(
    () => outputShow({ siteRoot: tempRoot, outputDir: '../outside', args: { ref: structuredEnvelope.output_ref } }),
    /output_directory_outside_site_root/
  );

  const unicodeResult = buildOutputRefToolContent({
    siteRoot: tempRoot,
    toolName: 'unicode_tool',
    value: { output: '😀'.repeat(10_000) },
  });
  const unicodeEnvelope = (unicodeResult as { structuredContent: Record<string, unknown> }).structuredContent;
  const unicodePage = outputShow({ siteRoot: tempRoot, args: { ref: unicodeEnvelope.output_ref, limit: 20_000 } });
  const unicodeLastCodeUnit = unicodePage.output_text.charCodeAt(unicodePage.output_text.length - 1);
  assert.equal(unicodeLastCodeUnit >= 0xd800 && unicodeLastCodeUnit <= 0xdbff, false);
  assert.equal(unicodePage.output_truncated, true);
  const asyncPage = await outputShowAsync({ siteRoot: tempRoot, args: { ref: unicodeEnvelope.output_ref, limit: 100, timeout_ms: 1000 } });
  assert.equal(asyncPage.status, 'ok');
  assert.equal(asyncPage.ref, unicodeEnvelope.output_ref);
  await assert.rejects(
    outputShowAsync({ siteRoot: tempRoot, args: { ref: unicodeEnvelope.output_ref, timeout_ms: 0 } }),
    /output_read_timeout_must_be_positive_integer/,
  );
  const outputTool = listOutputTools().find((tool: any) => tool.name === 'mcp_output_show') as any;
  assert.equal(outputTool.inputSchema.properties.timeout_ms.maximum, 15_000);

  const bulkyResult = buildOutputRefToolContent({
    siteRoot: tempRoot,
    toolName: 'bulky_tool',
    value: { status: 'ok', payload: 'x'.repeat(5_000), ref: `mcp_payload:${'p'.repeat(30)}@v1` },
  });
  const bulkyEnvelope = JSON.parse(bulkyResult.content[0].text);
  const bulkyStructuredEnvelope = (bulkyResult as { structuredContent: Record<string, unknown> }).structuredContent;
  assert.match(String(bulkyStructuredEnvelope.output_ref), /^mcp_output:/);
  assert.match(String(bulkyStructuredEnvelope.payload_ref), /^mcp_payload:/);
  assert.equal(bulkyEnvelope.truncated, true);

  const resources = listOutputResources({ siteRoot: tempRoot }).resources;
  assert.equal(resources.length >= 2, true);
const firstResourcePage = listOutputResources({ scope, offset: 0, limit: 1 });
assert.throws(() => listOutputResources({ scope, offset: '0' }));
assert.throws(() => listOutputResources({ scope, limit: '1' }));
assert.equal(firstResourcePage.resources.length, 1);
  assert.equal(firstResourcePage.has_more, true);
  assert.equal(firstResourcePage.nextCursor, String(firstResourcePage.next_offset));
  const secondResourcePage = listOutputResources({ scope, cursor: firstResourcePage.nextCursor, limit: 1 });
  assert.equal(secondResourcePage.resources.length, 1);
  assert.notEqual(secondResourcePage.resources[0].uri, firstResourcePage.resources[0].uri);

  const wrappedFirstPage = buildOutputRefToolContent({
    siteRoot: tempRoot,
    toolName: 'representative_paged_tool',
    value: structuredEnvelope,
  });
  const wrappedFirstPageStructuredContent = (wrappedFirstPage as { structuredContent: Record<string, unknown> }).structuredContent;
  assert.equal(typeof wrappedFirstPageStructuredContent.next_offset, 'number');
  assert.equal((wrappedFirstPageStructuredContent.next_offset as number) > 0, true);
  assert.equal((wrappedFirstPageStructuredContent.next_offset as number) <= 2_000, true);
  assert.equal(wrappedFirstPageStructuredContent.output_truncated, true);

  assert.throws(
    () => readOutputResource({ siteRoot: tempRoot, uri: 'mcp-output:mcp_output%3Amissing' }),
    /output_ref_not_found/
  );
  const resourcePage = JSON.parse(readOutputResource({ siteRoot: tempRoot, uri: `mcp-output:${encodeURIComponent(String(structuredEnvelope.output_ref))}` }).contents[0].text);
  assert.equal(resourcePage.schema, 'narada.mcp_output_page.v1');
  assert.equal(typeof resourcePage.output_text, 'string');

  assert.throws(
    () => payloadCreate({ siteRoot: tempRoot, args: { payload: {} } }),
    /payload_create_empty_payload_requires_allow_empty.*Use either.*payload_json.*payload:\{\}/
  );
  const emptyPayload = payloadCreate({ siteRoot: tempRoot, args: { payload: {}, allow_empty: true, payload_id: 'empty_payload_ok' } });
  assert.equal(emptyPayload.status, 'created');
  const repeatedPayload = payloadCreate({ siteRoot: tempRoot, args: { payload: { stable: true }, payload_id: 'immutable_retry_ok' } });
  const repeatedPayloadRetry = payloadCreate({ siteRoot: tempRoot, args: { payload: { stable: true }, payload_id: 'immutable_retry_ok' } });
  assert.equal(repeatedPayloadRetry.status, 'existing');
  assert.equal(repeatedPayloadRetry.ref, repeatedPayload.ref);
  assert.throws(
    () => payloadCreate({ siteRoot: tempRoot, args: { payload: { stable: false }, payload_id: 'immutable_retry_ok' } }),
    /payload_revision_conflict/
  );
  assert.throws(
    () => payloadCreate({ siteRoot: tempRoot, payloadDir: '../outside', args: { payload: { blocked: true } } }),
    /payload_directory_outside_site_root/
  );

  const createdPayload = payloadCreate({ siteRoot: tempRoot, args: { payload: { summary: 'x'.repeat(5_000) } } });
  const createdPayloadResult = buildOutputRefToolContent({
    siteRoot: tempRoot,
    toolName: 'mcp_payload_create',
    value: createdPayload,
  });
  const createdPayloadEnvelope = JSON.parse(createdPayloadResult.content[0].text);
  assert.match(String(createdPayloadEnvelope.ref), /^mcp_payload:/);

  const jsonPayload = payloadCreate({ siteRoot: tempRoot, args: { payload_json: '{"x":"y"}', payload_id: 'json_payload_ok' } });
  assert.equal(jsonPayload.status, 'created');
  const jsonPayloadShown = payloadShow({ siteRoot: tempRoot, args: { ref: jsonPayload.ref } });
  assert.deepEqual(jsonPayloadShown.payload, { x: 'y' });

  const jsonPayloadWithEmptyObjectPlaceholder = payloadCreate({ siteRoot: tempRoot, args: { payload: {}, payload_json: '{"x":"z"}', payload_id: 'json_payload_with_placeholder_ok' } });
  assert.equal(jsonPayloadWithEmptyObjectPlaceholder.status, 'created');
  const jsonPlaceholderShown = payloadShow({ siteRoot: tempRoot, args: { ref: jsonPayloadWithEmptyObjectPlaceholder.ref } });
  assert.deepEqual(jsonPlaceholderShown.payload, { x: 'z' });

  assert.throws(
    () => payloadCreate({ siteRoot: tempRoot, args: { payload: { x: 'object' }, payload_json: '{"x":"json"}' } }),
    /payload_create_must_choose_one_of_payload_or_payload_json/
  );

  assert.throws(
    () => payloadCreate({ siteRoot: tempRoot, args: { payload_json: '[]' } }),
    /payload_create_payload_json_must_be_object/
  );

  const deletionSource = payloadCreate({
    siteRoot: tempRoot,
    args: {
      payload_id: 'derive_delete_paths',
      payload: {
        preferred_role: 'worker',
        nullable: 'before',
        constraints: { model: 'old-model', keep: true },
        escaped: { 'a/b': 1, '~key': 2, '': 3 },
      },
    },
  });
  const deletionDerived = payloadDerive({
    siteRoot: tempRoot,
    args: {
      source_ref: deletionSource.ref,
      overlay: { nullable: null, constraints: { added: true } },
      delete_paths: ['/preferred_role', '/constraints/model', '/escaped/a~1b', '/escaped/~0key', '/escaped/'],
    },
  });
  assert.equal(deletionDerived.status, 'derived');
  assert.deepEqual(payloadShow({ siteRoot: tempRoot, args: { ref: deletionDerived.ref } }).payload, {
    nullable: null,
    constraints: { keep: true, added: true },
    escaped: {},
  });
  assert.equal(payloadShow({ siteRoot: tempRoot, args: { ref: deletionSource.ref } }).payload.preferred_role, 'worker');
  const deleteOnlyDerived = payloadDerive({
    siteRoot: tempRoot,
    args: { source_ref: deletionDerived.ref, delete_paths: ['/nullable'] },
  });
  assert.deepEqual(payloadShow({ siteRoot: tempRoot, args: { ref: deleteOnlyDerived.ref } }).payload, {
    constraints: { keep: true, added: true },
    escaped: {},
  });
  assert.throws(
    () => payloadDerive({ siteRoot: tempRoot, args: { source_ref: deletionSource.ref, delete_paths: ['/missing'] } }),
    /payload_derive_delete_path_not_found/
  );
  assert.throws(
    () => payloadDerive({ siteRoot: tempRoot, args: { source_ref: deletionSource.ref } }),
    /payload_derive_requires_overlay_or_delete_paths/
  );
  assert.throws(
    () => payloadDerive({ siteRoot: tempRoot, args: { source_ref: deletionSource.ref, overlay_json: '', delete_paths: ['/preferred_role'] } }),
    /payload_derive_overlay_must_be_object/
  );

  const mergePayload = payloadCreate({ siteRoot: tempRoot, args: { payload: { task_number: 1, agent_id: 'payload.agent', summary: 'from payload' } } });
  const merged = resolveToolPayloadArgs({
    siteRoot: tempRoot,
    toolName: 'representative_tool',
    args: { payload_ref: mergePayload.ref, task_number: 2, agent_id: 'top.agent' },
    allowedTools: ['representative_tool'],
    payloadRefMode: 'merge_args',
  });
  assert.deepEqual(merged.args, { task_number: 2, agent_id: 'top.agent', summary: 'from payload' });

  const placeholderMergePayload = payloadCreate({ siteRoot: tempRoot, args: { payload: { task_number: 1, agent_id: 'payload.agent', execution_notes: 'real notes', verification: 'real verification' } } });
  const placeholderMerged = resolveToolPayloadArgs({
    siteRoot: tempRoot,
    toolName: 'representative_tool',
    args: { payload_ref: placeholderMergePayload.ref, task_number: 2, agent_id: 'top.agent', execution_notes: '<!-- placeholder notes -->', verification: '<move original verification here>' },
    allowedTools: ['representative_tool'],
    payloadRefMode: 'merge_args_prefer_payload_placeholders',
  });
  assert.deepEqual(placeholderMerged.args, { task_number: 2, agent_id: 'top.agent', execution_notes: 'real notes', verification: 'real verification' });

  const outputId = 'o_alias_test';
  const outputRef = `mcp_output:${outputId}`;
  const fullOutput = { status: 'ok', nested: { value: 42 } };
  const fullText = JSON.stringify(fullOutput, null, 2);
  const outputRecord = {
    schema: 'narada.mcp_output_ref.v1',
    ref: outputRef,
    output_id: outputId,
    tool_name: 'alias_fixture',
    created_at: new Date().toISOString(),
    created_by: 'test',
    content_type: 'application/json',
    inline_char_limit: 1,
    full_output_char_length: fullText.length,
    truncated: true,
    sha256: createHash('sha256').update(JSON.stringify({ nested: { value: 42 }, status: 'ok' })).digest('hex'),
    max_bytes: 10_000,
    full_output: fullOutput,
  };
  mkdirSync(join(tempRoot, '.ai/tmp/mcp-outputs/workspace'), { recursive: true });
  writeFileSync(join(tempRoot, '.ai/tmp/mcp-outputs/workspace', `${outputId}.json`), `${JSON.stringify(outputRecord)}\n`, 'utf8');
  assert.equal(outputShow({ siteRoot: tempRoot, args: { ref: outputRef } }).ref, outputRef);
  assert.equal(outputShow({ siteRoot: tempRoot, args: { output_ref: outputRef } }).ref, outputRef);
  assert.throws(
    () => outputShow({ siteRoot: tempRoot, args: { ref: outputRef, output_ref: 'mcp_output:o_other_alias' } }),
    /output_show_ref_alias_conflict/
  );
} finally {
  rmSync(tempRoot, { recursive: true, force: true });
}

console.log('mcp transport contract tests passed');
