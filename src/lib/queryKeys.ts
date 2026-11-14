import type { ItemRef } from "./item";
import type { WorkKind } from "./types";

/** Each work kind's catalog queries ("movies", …) and detail pages ("movie-detail", …). */
const WORK_KEYS: Record<WorkKind, { list: string; detail: string }> = {
  movie: { list: "movies", detail: "movie-detail" },
  series: { list: "series", detail: "series-detail" },
};

/**
 * Query keys shared by more than one reader or writer. A key without its
 * item (`workDetail("movie")`) is the prefix that invalidates all of them.
 */
export const queryKeys = {
  sources: ["sources"],
  /** continue watching + up next */
  continue: ["continue"],
  channels: ["channels"],
  works: (kind: WorkKind) => [WORK_KEYS[kind].list],
  workDetail: (kind: WorkKind, ref?: Partial<ItemRef>) => (ref ? [WORK_KEYS[kind].detail, ref.sourceId, ref.id] : [WORK_KEYS[kind].detail]),
};
