import { invoke } from "./bridge";
import type {
  CacheStats,
  Category,
  Channel,
  ChannelQuery,
  ChannelVariant,
  Facets,
  GuideRow,
  HistoryInput,
  HistoryItem,
  LiveNav,
  MediaKind,
  MediaTracks,
  MediaQuery,
  Movie,
  MovieDetail,
  Page,
  PlayRequest,
  Programme,
  RecentChannel,
  SearchResults,
  Series,
  SeriesDetail,
  Settings,
  Source,
  SourceInput,
  TestResult,
  TmdbStatus,
  UpNext,
  WatchedMeta,
} from "./types";

/** Thin typed wrappers around the Tauri commands (src-tauri/src/lib.rs). */
export const api = {
  // sources
  sources: () => invoke<Source[]>("sources_list"),
  /** `id`: the source being edited (an empty password then means the stored one). */
  testSource: (input: SourceInput, id?: number) => invoke<TestResult>("source_test", { input, id }),
  addSource: (input: SourceInput) => invoke<Source>("source_add", { input }),
  updateSource: (id: number, input: SourceInput) => invoke<Source>("source_update", { id, input }),
  removeSource: (id: number) => invoke<void>("source_remove", { id }),
  syncSource: (id: number, epgOnly = false) => invoke<void>("source_sync", { id, epgOnly }),

  // catalog
  categories: (kind: MediaKind) => invoke<Category[]>("categories", { kind }),
  channels: (query: ChannelQuery) => invoke<Page<Channel>>("channels", { query }),
  channel: (sourceId: number, id: string) => invoke<Channel>("channel", { sourceId, id }),
  /** Channel counts per country and genre. */
  liveNav: () => invoke<LiveNav>("live_nav"),
  /** Every feed of a channel group, best first. */
  channelVariants: (key: string) => invoke<ChannelVariant[]>("channel_variants", { key }),
  /** Makes a feed the one that plays for its channel. */
  channelPrefer: (key: string, sourceId: number, id: string) => invoke<void>("channel_prefer", { key, sourceId, id }),
  movies: (query: MediaQuery) => invoke<Page<Movie>>("movies", { query }),
  series: (query: MediaQuery) => invoke<Page<Series>>("series_list", { query }),
  movieDetail: (sourceId: number, id: string) => invoke<MovieDetail>("movie_detail", { sourceId, id }),
  seriesDetail: (sourceId: number, id: string) => invoke<SeriesDetail>("series_detail", { sourceId, id }),
  /** Browse facets with work counts. */
  workFacets: (kind: "movie" | "series", query?: MediaQuery) => invoke<Facets>("work_facets", { kind, query }),
  /** Remembers which copy of a movie/series plays. */
  workPrefer: (kind: "movie" | "series", sourceId: number, id: string) => invoke<void>("work_prefer", { kind, sourceId, id }),
  /** Opens one version briefly to read its audio/subtitle tracks (only while nothing plays). */
  versionProbe: (kind: "movie" | "series", sourceId: number, id: string) =>
    invoke<MediaTracks | null>("version_probe", { kind, sourceId, id }),
  epg: (sourceId: number, channelId: string, from: number, to: number) =>
    invoke<Programme[]>("epg_channel", { sourceId, channelId, from, to }),
  guide: (query: ChannelQuery & { from: number; to: number }) => invoke<Page<GuideRow>>("epg_grid", { query }),
  search: (q: string, limit = 30) => invoke<SearchResults>("search", { q, limit }),

  // library
  toggleFavorite: (kind: MediaKind, sourceId: number, itemId: string) =>
    invoke<boolean>("favorite_toggle", { kind, sourceId, itemId }),
  /** Heart on a channel row: all of its feeds. */
  toggleChannelFavorite: (key: string) => invoke<boolean>("channel_group_favorite", { key }),
  updateHistory: (entry: HistoryInput) => invoke<void>("history_update", { entry }),
  /** `meta` describes items without history yet (never-played episodes). */
  markWatched: (kind: "movie" | "episode", sourceId: number, itemId: string, watched: boolean, meta?: WatchedMeta) =>
    invoke<void>("mark_watched", { kind, sourceId, itemId, watched, meta }),
  removeHistory: (kind: string, sourceId: number, itemId: string) =>
    invoke<void>("history_remove", { kind, sourceId, itemId }),
  continueWatching: (limit = 20) => invoke<HistoryItem[]>("continue_watching", { limit }),
  recentChannels: (limit = 20) => invoke<RecentChannel[]>("recent_channels", { limit }),
  upNext: (limit = 20) => invoke<UpNext[]>("up_next", { limit }),

  // playback
  play: (req: PlayRequest) => invoke<void>("play", { req }),
  stop: () => invoke<void>("player_stop"),
  command: (...args: (string | number)[]) => invoke<void>("player_command", { args: args.map(String) }),
  set: (name: string, value: unknown) => invoke<void>("player_set", { name, value }),
  get: <T = unknown>(name: string) => invoke<T | null>("player_get", { name }),
  /** Starts/stops recording the live channel; the file being / last written. */
  record: (on: boolean) => invoke<string | null>("player_record", { on }),

  // settings
  settings: () => invoke<Settings>("settings_get"),
  tmdbStatus: () => invoke<TmdbStatus>("tmdb_status"),
  /** Checks the key with TMDB and saves it ("" removes it); starts fetching. */
  tmdbSetKey: (key: string) => invoke<TmdbStatus>("tmdb_set_key", { key }),
  tmdbRefresh: () => invoke<TmdbStatus>("tmdb_refresh"),
  setSetting: (key: string, value: unknown) => invoke<void>("settings_set", { key, value }),
  imageCache: () => invoke<CacheStats>("images_cache_info"),
  clearImageCache: () => invoke<CacheStats>("images_cache_clear"),
};

export function errorMessage(e: unknown): string {
  if (typeof e === "string") return e;
  if (e instanceof Error) return e.message;
  return JSON.stringify(e);
}
