import { defineNativeSurface, type DefinedSurface, type McpToolDefinition } from '@narada-core/mcp-fabric-contracts';

const inputSchema = { type: 'object', properties: { url: { type: 'string' }, job_id: { type: 'string' }, start_seconds: { type: 'number' }, end_seconds: { type: 'number' }, duration_seconds: { type: 'number' }, quality: { type: 'string' }, audio_format: { type: 'string' }, transcript_format: { type: 'string' }, language: { type: 'string' }, wait: { type: 'boolean' }, output: { type: 'string' } } };
const names = ['media_capabilities','youtube_inspect','youtube_download_video','youtube_download_audio','youtube_clip','youtube_transcript','youtube_thumbnail','x_inspect','x_download_media','x_clip_video','media_job_get','media_job_cancel','media_artifact_fetch'] as const;
const tools = names.map((name) => ({ name, description: `Narada media operation: ${name}`, inputSchema })) as McpToolDefinition[];

export function surfaceDefinition(): DefinedSurface {
  return defineNativeSurface({
    surface_id: 'media-operations', surface_version: '0.1.0', package: '@narada-core/media-operations-mcp',
    entrypoint: '{mcp_surfaces_root}/media-operations-mcp/native/target/release/narada-media', tools,
    read_only_tools: ['media_capabilities','youtube_inspect','x_inspect','media_job_get'], default_effect: 'external_write',
    projections: [{ id: 'default', transport: { kind: 'stdio', command: 'narada-media', args: ['mcp'], env: ['NARADA_MEDIA_API_URL','NARADA_MEDIA_API_TOKEN','NARADA_MEDIA_OUTPUT_ROOT'] }, injection_scope: 'host', default_injection: 'disabled', runtime_requirements: ['network'], authority_requirements: ['scope.host'], lifecycle: { mode: 'restart_required', restart_owner: 'host', reason: 'The authenticated stdio client is process scoped.' } }],
  });
}
