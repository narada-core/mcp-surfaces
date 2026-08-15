export type BoundedCollection<T> = {
  schema: 'narada.bounded_collection.v1';
  items: T[];
  offset: number;
  limit: number;
  returned_count: number;
  total_count: number | null;
  has_more: boolean;
  next_offset: number | null;
  truncated: boolean;
  truncation_reason: string | null;
};

export function boundedCollection<T>(
  items: readonly T[],
  options: {
    offset?: number;
    limit: number;
    truncationReason?: string | null;
    totalCountKnown?: boolean;
  },
): BoundedCollection<T> {
  const offset = Math.max(0, Math.trunc(options.offset ?? 0));
  const limit = Math.max(1, Math.trunc(options.limit));
  const page = items.slice(offset, offset + limit);
  const hasMore = offset + page.length < items.length;
  return {
    schema: 'narada.bounded_collection.v1',
    items: page,
    offset,
    limit,
    returned_count: page.length,
    total_count: options.totalCountKnown === false ? null : items.length,
    has_more: hasMore,
    next_offset: hasMore ? offset + page.length : null,
    truncated: hasMore,
    truncation_reason: hasMore ? options.truncationReason ?? 'page_limit' : null,
  };
}

