/** Catalog items (channels, movies, series, feeds) are identified by source + id. */
export interface ItemRef {
  sourceId: number;
  id: string;
}

export function sameItem(a: ItemRef | null | undefined, b: ItemRef | null | undefined): boolean {
  return !!a && !!b && a.sourceId === b.sourceId && a.id === b.id;
}

/** Stable string identity ("3:1234"), e.g. for React keys. */
export function itemKey(x: ItemRef): string {
  return `${x.sourceId}:${x.id}`;
}
