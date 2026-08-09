import assert from 'node:assert/strict';

const before = {
  uncaughtException: process.listenerCount('uncaughtException'),
  unhandledRejection: process.listenerCount('unhandledRejection'),
  stdinData: process.stdin.listenerCount('data'),
  argv: [...process.argv],
};

const { surfaceDefinition } = await import('../src/surface-definition.js');
const definition = surfaceDefinition();

assert.deepEqual(process.argv, before.argv);
assert.equal(process.listenerCount('uncaughtException'), before.uncaughtException);
assert.equal(process.listenerCount('unhandledRejection'), before.unhandledRejection);
assert.equal(process.stdin.listenerCount('data'), before.stdinData);
const descriptor = definition.descriptor as {
  surface_id: string;
  tools: unknown[];
  projections: Array<{
    transport: { command: string };
    exposed_tools: string[];
  }>;
};
assert.equal(descriptor.surface_id, 'agent-context');
assert.equal(descriptor.projections[0].transport.command, 'bun');
assert.equal(descriptor.projections[1].transport.command, 'bun');
assert.ok(descriptor.tools.length > 0);
assert.ok(descriptor.projections[0].exposed_tools.includes('agent_orientation_read'));

console.log('agent-context surface definition is side-effect free');
