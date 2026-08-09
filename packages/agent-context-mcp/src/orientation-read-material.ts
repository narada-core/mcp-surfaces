export const ORIENTATION_MANIFEST_ENTRY_ARTIFACT_PREFIX =
  'orientation-manifest-entry:';
export const ORIENTATION_REQUIRED_READ_MAX_SOURCE_BYTES = 192 * 1024;
export const ORIENTATION_REQUIRED_READ_PAGE_BYTES = 3_000;
export const ORIENTATION_REQUIRED_READ_PAGE_JSON_BYTES = 3_200;
export const ORIENTATION_REQUIRED_READ_MAX_TOTAL_PAGES = 128;

function utf8SafePageEnd(contentBytes: Buffer, byteOffset: number, candidateEnd: number) {
  const totalBytes: any = contentBytes.length;
  let pageEnd: any = Math.min(totalBytes, candidateEnd);
  while (
    pageEnd > byteOffset
    && pageEnd < totalBytes
    && (contentBytes[pageEnd] & 0xc0) === 0x80
  ) {
    pageEnd -= 1;
  }
  return pageEnd;
}

function serializedContentBytes(
  contentBytes: Buffer,
  byteOffset: number,
  pageEnd: number,
) {
  const content: any = contentBytes.subarray(byteOffset, pageEnd).toString('utf8');
  return Buffer.byteLength(JSON.stringify(content), 'utf8');
}

function transportSafeEnd(
  contentBytes: Buffer,
  byteOffset: number,
  candidateEnd: number,
) {
  if (
    serializedContentBytes(contentBytes, byteOffset, candidateEnd)
      <= ORIENTATION_REQUIRED_READ_PAGE_JSON_BYTES
  ) {
    return candidateEnd;
  }
  let low: any = byteOffset + 1;
  let high: any = candidateEnd;
  let best: any = byteOffset;
  while (low <= high) {
    const midpoint: any = Math.floor((low + high) / 2);
    const pageEnd: any = utf8SafePageEnd(contentBytes, byteOffset, midpoint);
    if (pageEnd <= byteOffset) {
      low = midpoint + 1;
      continue;
    }
    if (
      serializedContentBytes(contentBytes, byteOffset, pageEnd)
        <= ORIENTATION_REQUIRED_READ_PAGE_JSON_BYTES
    ) {
      best = Math.max(best, pageEnd);
      low = midpoint + 1;
    } else {
      high = pageEnd - 1;
    }
  }
  return best;
}

export function orientationRequiredReadPageEnd(
  contentBytes: Buffer,
  byteOffset: number,
) {
  const totalBytes: any = contentBytes.length;
  const hardEnd: any = utf8SafePageEnd(
    contentBytes,
    byteOffset,
    Math.min(totalBytes, byteOffset + ORIENTATION_REQUIRED_READ_PAGE_BYTES),
  );
  const pageEnd: any = transportSafeEnd(contentBytes, byteOffset, hardEnd);
  if (pageEnd >= totalBytes) return totalBytes;

  const minimumSemanticEnd: any = byteOffset
    + Math.floor((pageEnd - byteOffset) / 2);
  const paragraphEnd: any = contentBytes.lastIndexOf(
    Buffer.from('\n\n'),
    pageEnd - 1,
  );
  if (paragraphEnd >= minimumSemanticEnd) return paragraphEnd + 2;
  const lineEnd: any = contentBytes.lastIndexOf(0x0a, pageEnd - 1);
  if (lineEnd >= minimumSemanticEnd) return lineEnd + 1;
  return pageEnd;
}

export function orientationRequiredReadPageCount(content: string, sourceRef: string) {
  const contentBytes: any = Buffer.from(content, 'utf8');
  let byteOffset: any = 0;
  let pageCount: any = 0;
  while (byteOffset < contentBytes.length) {
    const pageEnd: any = orientationRequiredReadPageEnd(contentBytes, byteOffset);
    if (pageEnd <= byteOffset) {
      throw new Error(
        `agent_context_orientation_required_read_page_boundary_invalid:${sourceRef}`,
      );
    }
    pageCount += 1;
    if (pageCount > ORIENTATION_REQUIRED_READ_MAX_TOTAL_PAGES) {
      throw new Error(
        'agent_context_orientation_required_read_page_bound_exceeded:'
        + `${sourceRef}:max=${ORIENTATION_REQUIRED_READ_MAX_TOTAL_PAGES}`,
      );
    }
    byteOffset = pageEnd;
  }
  return pageCount;
}

export function assertOrientationRequiredReadSourceBound(
  content: string,
  sourceRef: string,
) {
  const bytes: any = Buffer.byteLength(content, 'utf8');
  if (bytes > ORIENTATION_REQUIRED_READ_MAX_SOURCE_BYTES) {
    throw new Error(
      'agent_context_orientation_required_read_source_bound_exceeded:'
      + `${sourceRef}:actual=${bytes}:max=${ORIENTATION_REQUIRED_READ_MAX_SOURCE_BYTES}`,
    );
  }
  return {
    source_bytes: bytes,
    page_count: orientationRequiredReadPageCount(content, sourceRef),
  };
}

function jsonObject(value: unknown): Record<string, unknown> {
  if (typeof value !== 'object' || value === null || Array.isArray(value)) return {};
  return JSON.parse(JSON.stringify(value)) as Record<string, unknown>;
}

export function orientationManifestEntryArtifactRef(entryId: string) {
  if (!entryId.trim()) {
    throw new Error('agent_context_orientation_manifest_entry_id_required');
  }
  return ORIENTATION_MANIFEST_ENTRY_ARTIFACT_PREFIX
    + encodeURIComponent(entryId.trim());
}

export function orientationManifestEntryIdFromArtifactRef(artifactRef: string) {
  if (!artifactRef.startsWith(ORIENTATION_MANIFEST_ENTRY_ARTIFACT_PREFIX)) {
    return null;
  }
  const encoded: any = artifactRef.slice(
    ORIENTATION_MANIFEST_ENTRY_ARTIFACT_PREFIX.length,
  );
  if (!encoded) {
    throw new Error('agent_context_orientation_manifest_entry_ref_invalid');
  }
  let entryId: any;
  try {
    entryId = decodeURIComponent(encoded);
  } catch {
    throw new Error('agent_context_orientation_manifest_entry_ref_invalid');
  }
  if (!entryId.trim()) {
    throw new Error('agent_context_orientation_manifest_entry_ref_invalid');
  }
  return entryId;
}

export function buildExactContinuityReadMaterial({
  checkpoint,
  portableContinuation,
}: {
  checkpoint: unknown;
  portableContinuation?: unknown;
}) {
  return {
    schema: 'narada.agent_context.orientation_continuity_material.v1',
    selection_posture: 'selected_at_carrier_entry_not_live_state',
    historical_advisory_only: true,
    checkpoint: jsonObject(checkpoint),
    portable_continuation: jsonObject(portableContinuation ?? {}),
    authority_posture: {
      continuity: 'historical_context_only',
      consequential_action: 'owning_admission_still_required',
    },
  };
}

export function renderExactContinuityReadMaterial(input: {
  checkpoint: unknown;
  portableContinuation?: unknown;
}) {
  return JSON.stringify(buildExactContinuityReadMaterial(input), null, 2) + '\n';
}
