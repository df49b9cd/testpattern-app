import { createHashRouter } from "react-router";
import { Root } from "./Root";
import { Layout } from "./Layout";
import { HomePage } from "../pages/Home";
import { LivePage } from "../pages/Live";
import { GuidePage } from "../pages/Guide";
import { MoviesPage } from "../pages/Movies";
import { MovieDetailPage } from "../pages/MovieDetail";
import { SeriesPage } from "../pages/Series";
import { SeriesDetailPage } from "../pages/SeriesDetail";
import { SearchPage } from "../pages/Search";
import { SettingsPage } from "../pages/Settings";
import { OnboardingPage } from "../pages/Onboarding";
import { PlayerPage } from "../pages/Player";
import { ROUTES } from "../lib/routes";

export interface RouteHandle {
  /** Page leaves the main area unpainted so the native video shows through. */
  transparent?: boolean;
}

export const router = createHashRouter([
  {
    element: <Root />,
    children: [
      { path: ROUTES.onboarding, element: <OnboardingPage /> },
      { path: ROUTES.player, element: <PlayerPage />, handle: { transparent: true } satisfies RouteHandle },
      {
        element: <Layout />,
        children: [
          { index: true, element: <HomePage /> },
          { path: ROUTES.live, element: <LivePage />, handle: { transparent: true } satisfies RouteHandle },
          { path: ROUTES.guide, element: <GuidePage /> },
          { path: ROUTES.movieList, element: <MoviesPage /> },
          { path: "/movies/:sourceId/:id", element: <MovieDetailPage /> },
          { path: ROUTES.seriesList, element: <SeriesPage /> },
          { path: "/series/:sourceId/:id", element: <SeriesDetailPage /> },
          { path: ROUTES.search, element: <SearchPage /> },
          { path: ROUTES.settings, element: <SettingsPage /> },
        ],
      },
    ],
  },
]);
