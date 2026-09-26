// Mirrors the camelCase JSON produced by the Rust commands (src-tauri/src).

export type SourceKind = "xtream" | "m3u";

export interface Counts {
  channels: number;
  movies: number;
  series: number;
  programmes: number;
}

export interface AccountInfo {
  status: string;
  message?: string | null;
  expiresAt?: number | null;
  createdAt?: number | null;
  isTrial: boolean;
  activeConnections: number;
  maxConnections: number;
  formats: string[];
  serverTimezone?: string | null;
  serverUtcOffset?: number | null;
  baseUrl: string;
}

export interface Source {
  id: number;
  kind: SourceKind;
  name: string;
  url: string;
  altUrls: string[];
  username?: string | null;
  hasPassword: boolean;
  epgUrl?: string | null;
  userAgent?: string | null;
  createdAt: number;
  lastSync?: number | null;
  lastEpgSync?: number | null;
  syncError?: string | null;
  account?: AccountInfo | null;
  /** Catch-up time correction (minutes). */
  catchupShiftMinutes: number;
  /** The password is in the system keyring, not the app database. */
  passwordInKeyring: boolean;
  /** …but the keyring was locked or unavailable so far. */
  passwordLocked: boolean;
  syncing: boolean;
  counts: Counts;
}

export interface SourceInput {
  kind: SourceKind;
  name?: string;
  url: string;
  altUrls?: string[];
  username?: string;
  password?: string;
  epgUrl?: string;
  userAgent?: string;
  catchupShiftMinutes?: number;
}

export type TestResult =
  | { kind: "xtream"; account: AccountInfo }
  | { kind: "m3u"; channels: number; movies: number; episodes: number; epgUrls: string[] };

export type SyncProgress =
  | { stage: "started"; sourceId: number }
  | { stage: "step"; sourceId: number; message: string }
  | { stage: "done"; sourceId: number; counts: Counts }
  | { stage: "failed"; sourceId: number; error: string };

export type MediaKind = "live" | "movie" | "series";

export interface Category {
  sourceId: number;
  id: string;
  title: string;
  region?: string | null;
  badges: string[];
  count: number;
}

export interface Brief {
  title: string;
  start: number;
  stop: number;
}

export interface Channel {
  sourceId: number;
  id: string;
  num?: number | null;
  title: string;
  logo?: string | null;
  epgId?: string | null;
  categoryId?: string | null;
  badges: string[];
  archive: boolean;
  archiveDays: number;
  favorite: boolean;
  now?: Brief | null;
  next?: Brief | null;
}

export interface Page<T> {
  total: number;
  items: T[];
}

export interface Movie {
  sourceId: number;
  id: string;
  title: string;
  year?: number | null;
  poster?: string | null;
  rating?: number | null;
  added?: number | null;
  tag?: string | null;
  ext?: string | null;
  favorite: boolean;
  progress?: number | null;
  watched: boolean;
}

export interface Series {
  sourceId: number;
  id: string;
  title: string;
  year?: number | null;
  cover?: string | null;
  backdrop?: string | null;
  rating?: number | null;
  genre?: string | null;
  lastModified?: number | null;
  tag?: string | null;
  favorite: boolean;
}

export interface TechInfo {
  codec?: string | null;
  width?: number | null;
  height?: number | null;
  hdr: boolean;
  channels?: number | null;
  language?: string | null;
}

export interface MovieDetail extends Movie {
  plot?: string | null;
  cast?: string | null;
  director?: string | null;
  genre?: string | null;
  country?: string | null;
  releaseDate?: string | null;
  duration?: number | null;
  backdrop?: string | null;
  trailer?: string | null;
  ageRating?: string | null;
  video?: TechInfo | null;
  audio?: TechInfo | null;
  position: number;
}

export interface Episode {
  id: string;
  season: number;
  episode: number;
  title: string;
  plot?: string | null;
  duration?: number | null;
  image?: string | null;
  ext?: string | null;
  rating?: number | null;
  airDate?: string | null;
  video?: TechInfo | null;
  position: number;
  watched: boolean;
}

export interface Season {
  season: number;
  name: string;
  cover?: string | null;
  overview?: string | null;
  airDate?: string | null;
  episodes: Episode[];
}

export interface Resume {
  episodeId: string;
  season: number;
  episode: number;
  position: number;
  started: boolean;
}

export interface SeriesDetail extends Series {
  plot?: string | null;
  cast?: string | null;
  director?: string | null;
  releaseDate?: string | null;
  trailer?: string | null;
  seasons: Season[];
  resume?: Resume | null;
}

export interface Programme {
  epgId: string;
  start: number;
  stop: number;
  title: string;
  subtitle?: string | null;
  description?: string | null;
  category?: string | null;
  episode?: string | null;
  icon?: string | null;
}

export interface GuideRow {
  channel: Channel;
  programmes: Programme[];
}

export interface SearchResults {
  channels: Channel[];
  movies: Movie[];
  series: Series[];
}

export interface HistoryItem {
  kind: "live" | "movie" | "episode";
  sourceId: number;
  itemId: string;
  seriesId?: string | null;
  season?: number | null;
  episode?: number | null;
  title: string;
  subtitle?: string | null;
  image?: string | null;
  backdrop?: string | null;
  ext?: string | null;
  position: number;
  duration: number;
  updatedAt: number;
}

export interface RecentChannel extends Channel {
  watchedAt: number;
}

export interface ChannelQuery {
  sourceId?: number;
  categoryId?: string;
  favorites?: boolean;
  withEpg?: boolean;
  q?: string;
  offset?: number;
  limit?: number;
}

export type MediaSort = "added" | "title" | "rating" | "year" | "provider";

export interface MediaQuery {
  sourceId?: number;
  categoryId?: string;
  favorites?: boolean;
  q?: string;
  sort?: MediaSort;
  offset?: number;
  limit?: number;
}

export type PlayKind = "live" | "movie" | "episode" | "catchup";

export interface PlayRequest {
  kind: PlayKind;
  sourceId: number;
  id: string;
  ext?: string | null;
  start?: number | null;
  title?: string | null;
  catchupStart?: number;
  catchupMinutes?: number;
  paused?: boolean;
}

export interface HistoryInput {
  kind: "live" | "movie" | "episode";
  sourceId: number;
  itemId: string;
  seriesId?: string | null;
  season?: number | null;
  episode?: number | null;
  title: string;
  subtitle?: string | null;
  image?: string | null;
  backdrop?: string | null;
  ext?: string | null;
  position: number;
  duration: number;
}

/** mark_watched extras (src-tauri/src/library.rs WatchedMeta). */
export interface WatchedMeta {
  seriesId?: string | null;
  season?: number | null;
  episode?: number | null;
  title?: string | null;
  subtitle?: string | null;
  image?: string | null;
  backdrop?: string | null;
  ext?: string | null;
  duration?: number | null;
}

export interface Track {
  id: number;
  type: "video" | "audio" | "sub";
  title?: string;
  lang?: string;
  codec?: string;
  default?: boolean;
  selected?: boolean;
  external?: boolean;
  "demux-channel-count"?: number;
  "demux-w"?: number;
  "demux-h"?: number;
}

export type PlayerEvent =
  | { type: "prop"; name: string; value: unknown }
  | { type: "start-file" }
  | { type: "file-loaded" }
  | { type: "end-file"; reason: "eof" | "stop" | "quit" | "error" | "redirect" | "unknown"; error?: string | null }
  | { type: "restart" }
  | { type: "reconnecting"; attempt: number }
  | { type: "log"; level: string; prefix: string; text: string };

export type Settings = Record<string, unknown>;

/** Artwork cache usage (src-tauri/src/images.rs). */
export interface CacheStats {
  files: number;
  bytes: number;
}

export interface UpNext {
  series: Series;
  episode: Episode;
}
