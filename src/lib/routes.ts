import type { ItemRef } from "./item";

/** Paths the code navigates to by name (app/router.tsx). */
export const ROUTES = {
  home: "/",
  live: "/live",
  guide: "/guide",
  movieList: "/movies",
  seriesList: "/series",
  search: "/search",
  settings: "/settings",
  onboarding: "/onboarding",
  /** onboarding as "add another source" (from Settings) */
  addSource: "/onboarding?add=1",
  /** the fullscreen player */
  player: "/player",
  /** a movie's / a series' detail page */
  movie: (ref: ItemRef) => `/movies/${ref.sourceId}/${ref.id}`,
  series: (ref: ItemRef) => `/series/${ref.sourceId}/${ref.id}`,
} as const;

/** Start pages the "ui.startPage" setting can choose (settings.rs). */
export const START_PAGES = { home: ROUTES.home, live: ROUTES.live, guide: ROUTES.guide } as const;
export type StartPage = keyof typeof START_PAGES;
