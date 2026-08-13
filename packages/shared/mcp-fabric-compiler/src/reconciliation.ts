import {
  MCP_FABRIC_SCHEMA_VERSION,
  fabricManifestDigest,
  parseCarrierProjectionV2,
  parseFabricManifestV2,
  parseRuntimeObservationV2,
  stableDigest,
  surfaceDescriptorDigest,
  surfaceToolContractDigest,
  type CarrierProjectionV2,
  type FabricManifestV2,
  type ReconciliationActionV2,
  type ReconciliationPlanV2,
  type RuntimeObservationV2,
  type RuntimeServerObservationV2,
} from '@narada-core/mcp-fabric-contracts';

export type ReconciliationInput = {
  manifest: FabricManifestV2;
  carrier_projection: CarrierProjectionV2;
  observation: RuntimeObservationV2;
  generated_at?: string;
};

export type ReconciliationApplyGuard = {
  action: ReconciliationActionV2;
  current_observation: RuntimeObservationV2;
  granted_authorities: string[];
  operation_id: string;
};

type ActionSeed = Omit<
  ReconciliationActionV2,
  'operation_id' | 'expected_state'
> & {
  expected_descriptor_digest: string | null;
};

export function reconcileFabricState(input: ReconciliationInput): ReconciliationPlanV2 {
  const manifest = parseFabricManifestV2(input.manifest);
  const carrier = parseCarrierProjectionV2(input.carrier_projection);
  const observation = parseRuntimeObservationV2(input.observation);
  const manifestDigest = fabricManifestDigest(manifest);
  const observationDigest = stableDigest(observation);
  const seed = selectAction(manifest, carrier, observation, manifestDigest);
  const operationId = operationIdFor(seed, manifestDigest, observationDigest);
  const action: ReconciliationActionV2 = {
    action: seed.action,
    server_name: seed.server_name,
    reason: seed.reason,
    actuator: seed.actuator,
    required_authority: seed.required_authority,
    operation_id: operationId,
    expected_state: {
      manifest_digest: observation.manifest_digest,
      observation_digest: observationDigest,
      descriptor_digest: seed.expected_descriptor_digest,
    },
    outcome_lookup: {
      tool_name: seed.outcome_lookup.tool_name,
      arguments: { ...seed.outcome_lookup.arguments, operation_id: operationId },
    },
    recovery: seed.recovery,
  };
  return {
    schema_version: MCP_FABRIC_SCHEMA_VERSION,
    generated_at: input.generated_at ?? observation.observed_at,
    site_id: manifest.site_id,
    carrier_kind: carrier.carrier_kind,
    manifest_digest: manifestDigest,
    observation_digest: observationDigest,
    actions: [action],
  };
}

export function assertReconciliationApplyAllowed(input: ReconciliationApplyGuard): ReconciliationActionV2 {
  const current = parseRuntimeObservationV2(input.current_observation);
  const currentDigest = stableDigest(current);
  if (input.operation_id !== input.action.operation_id) {
    throw new Error(
      `mcp_fabric_operation_id_mismatch: expected=${input.action.operation_id} actual=${input.operation_id}`,
    );
  }
  if (currentDigest !== input.action.expected_state.observation_digest) {
    throw new Error(
      `mcp_fabric_stale_precondition: expected=${input.action.expected_state.observation_digest} actual=${currentDigest}`,
    );
  }
  if (!input.granted_authorities.includes(input.action.required_authority)) {
    throw new Error(
      `mcp_fabric_authority_required: ${input.action.required_authority}; actuator=${input.action.actuator}`,
    );
  }
  return input.action;
}

function selectAction(
  manifest: FabricManifestV2,
  carrier: CarrierProjectionV2,
  observation: RuntimeObservationV2,
  manifestDigest: string,
): ActionSeed {
  if (carrier.site_id !== manifest.site_id || observation.site_id !== manifest.site_id) {
    return unsupported(null, 'Desired, projected, and observed Site identities do not match.');
  }
  if (observation.carrier_kind !== carrier.carrier_kind) {
    return unsupported(null, 'Observed carrier kind does not match the desired carrier projection.');
  }
  if (carrier.manifest_digest !== manifestDigest || observation.manifest_digest !== manifestDigest) {
    return actionSeed({
      action: 'rematerialize_all_carrier_configs',
      server_name: null,
      reason: 'Carrier materialization or runtime observation was produced from a different manifest.',
      actuator: 'narada-mcp-materializer',
      required_authority: 'fabric.config.apply',
      expected_descriptor_digest: null,
      outcome_tool: 'registrar_operation_outcome_show',
      recovery_tool: null,
      recovery_guidance: 'Run cargo native-materialize from the mcp-surfaces workspace; the native materializer rewrites the declared carrier set transactionally, then restart affected carriers.',
    });
  }

  const desiredNames = new Set(carrier.servers.map((server) => server.server_name));
  const unexpected = [...observation.servers]
    .filter((server) => !desiredNames.has(server.server_name))
    .sort((left, right) => left.server_name.localeCompare(right.server_name))[0];
  if (unexpected) {
    return unsupported(
      unexpected.server_name,
      'Observed runtime contains a server that is absent from the desired carrier projection.',
    );
  }

  for (const desired of [...carrier.servers].sort((a, b) => a.server_name.localeCompare(b.server_name))) {
    const descriptor = manifest.descriptors.find((item) => item.surface_id === desired.surface_id);
    const binding = manifest.bindings.find((item) => item.server_name === desired.server_name);
    if (!descriptor || !binding) {
      return unsupported(desired.server_name, 'Desired carrier server cannot be resolved to one manifest descriptor and binding.');
    }
    const projection = descriptor.projections.find((item) => item.id === desired.projection_id);
    if (!projection) {
      return unsupported(desired.server_name, 'Desired carrier server references a missing surface projection.');
    }
    const expectedDescriptor = surfaceDescriptorDigest(descriptor);
    const expectedTools = surfaceToolContractDigest(descriptor);
    const observed = observation.servers.find((server) => server.server_name === desired.server_name);
    if (!observed) {
      return actionForLifecycle(
        projection.lifecycle.mode,
        desired.server_name,
        expectedDescriptor,
        'Desired server has no runtime observation.',
      );
    }
    const drift = generationDrift(
      observed,
      observation.observed_at,
      expectedDescriptor,
      expectedTools,
    );
    if (drift !== null) {
      return actionForLifecycle(
        projection.lifecycle.mode,
        desired.server_name,
        expectedDescriptor,
        drift,
      );
    }
  }

  return actionSeed({
    action: 'no_op',
    server_name: null,
    reason: 'Desired carrier projection and runtime observation are equivalent.',
    actuator: 'fabric-observer',
    required_authority: 'fabric.read',
    expected_descriptor_digest: null,
    outcome_tool: 'mcp_fabric_reconciliation_outcome_show',
    recovery_tool: null,
    recovery_guidance: 'No mutation is required; retain the observation and plan digests as evidence.',
  });
}

function generationDrift(
  observed: RuntimeServerObservationV2,
  observedAt: string,
  expectedDescriptor: string,
  expectedTools: string,
): string | null {
  const active = observed.active_generation;
  if (!active) return 'Runtime server has no active generation.';
  if (active.state !== 'active') return `Runtime generation is ${active.state}, not active.`;
  if (active.descriptor_digest !== expectedDescriptor) return 'Runtime descriptor digest differs from desired state.';
  if (active.tool_contract_digest !== expectedTools) return 'Runtime tools/list contract digest differs from desired state.';
  if (active.freshness !== 'current') return `Runtime freshness is ${active.freshness}.`;
  if (active.health !== 'healthy') return `Runtime health is ${active.health}.`;
  if (Date.parse(active.lease_expires_at) <= Date.parse(observedAt)) return 'Runtime generation lease has expired.';
  return null;
}

function actionForLifecycle(
  lifecycle: 'replayable' | 'session_pinned' | 'restart_required',
  serverName: string,
  descriptorDigest: string,
  reason: string,
): ActionSeed {
  if (lifecycle === 'restart_required') {
    return actionSeed({
      action: 'reconnect_required',
      server_name: serverName,
      reason,
      actuator: 'carrier-supervisor',
      required_authority: 'carrier.restart',
      expected_descriptor_digest: descriptorDigest,
      outcome_tool: 'carrier_restart_outcome_show',
      recovery_tool: null,
      recovery_guidance: 'Restart or reconnect the carrier through its supervisor, then obtain a fresh RuntimeObservationV2 before retrying.',
    });
  }
  return actionSeed({
    action: 'replace_generation',
    server_name: serverName,
    reason,
    actuator: 'mcp-loader',
    required_authority: 'fabric.runtime.replace',
    expected_descriptor_digest: descriptorDigest,
    outcome_tool: 'mcp_loader_reconciliation_outcome_show',
    recovery_tool: 'mcp_loader_surface_restart',
    recovery_guidance: 'Call mcp_loader_surface_restart for the stable logical connection with the operation_id and expected-state preconditions.',
  });
}

function unsupported(serverName: string | null, reason: string): ActionSeed {
  return actionSeed({
    action: 'unsupported',
    server_name: serverName,
    reason,
    actuator: 'fabric-operator',
    required_authority: 'fabric.reconcile.review',
    expected_descriptor_digest: null,
    outcome_tool: 'mcp_fabric_reconciliation_outcome_show',
    recovery_tool: null,
    recovery_guidance: 'Inspect the manifest, carrier projection, and observation identities; no automatic mutation is permitted.',
  });
}

function actionSeed(input: {
  action: ReconciliationActionV2['action'];
  server_name: string | null;
  reason: string;
  actuator: string;
  required_authority: string;
  expected_descriptor_digest: string | null;
  outcome_tool: string;
  recovery_tool: string | null;
  recovery_guidance: string;
}): ActionSeed {
  return {
    action: input.action,
    server_name: input.server_name,
    reason: input.reason,
    actuator: input.actuator,
    required_authority: input.required_authority,
    expected_descriptor_digest: input.expected_descriptor_digest,
    outcome_lookup: { tool_name: input.outcome_tool, arguments: {} },
    recovery: {
      actuator: input.actuator,
      tool_name: input.recovery_tool,
      arguments: input.server_name ? { server_name: input.server_name } : {},
      guidance: input.recovery_guidance,
    },
  };
}

function operationIdFor(
  action: ActionSeed,
  manifestDigest: string,
  observationDigest: string,
): string {
  return `reconcile-${stableDigest({
    action: action.action,
    server_name: action.server_name,
    manifest_digest: manifestDigest,
    observation_digest: observationDigest,
  }).slice(0, 24)}`;
}
