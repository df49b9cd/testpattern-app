import { useQuery } from "@tanstack/react-query";
import { useEffect } from "react";
import { Navigate, Outlet, useLocation, useNavigate } from "react-router";
import { api } from "../lib/api";
import { useBackendEvents } from "../hooks/useBackendEvents";
import { useProgressSaver } from "../hooks/useProgressSaver";
import { Spinner } from "../components/ui";
import { setVideoViewport, usePlayer } from "../stores/player";
import { PipPlayer } from "../components/PipPlayer";

/** Start pages the "ui.startPage" setting can choose (settings.rs). */
export const START_PAGES = { home: "/", live: "/live", guide: "/guide" } as const;
export type StartPage = keyof typeof START_PAGES;

// once per launch, not every time the user comes back to Home
let startPageApplied = false;

/** App-wide effects + first-run redirect. */
export function Root() {
  useBackendEvents();
  useProgressSaver();
  const location = useLocation();
  const navigate = useNavigate();
  const sources = useQuery({ queryKey: ["sources"], queryFn: api.sources });
  const settings = useQuery({ queryKey: ["settings"], queryFn: api.settings });

  // Leaving the live preview / player: video goes back to full size, and
  // live previews stop unless the player is being opened.
  // Picture-in-picture (T-034) keeps playing and positions the video itself.
  useEffect(() => {
    const { now, stop, pip } = usePlayer.getState();
    if (pip && location.pathname !== "/player") return;
    if (location.pathname !== "/live") void setVideoViewport(null);
    if (now?.kind === "live" && location.pathname !== "/live" && location.pathname !== "/player") void stop();
  }, [location.pathname]);

  useEffect(() => {
    if (startPageApplied || !settings.data || !sources.data?.length) return;
    startPageApplied = true;
    const page = START_PAGES[settings.data["ui.startPage"] as StartPage];
    if (page && page !== "/" && location.pathname === "/") navigate(page, { replace: true });
  }, [settings.data, sources.data, location.pathname, navigate]);

  if (sources.isLoading) {
    return (
      <div className="grid h-full place-items-center bg-bg">
        <Spinner label="Loading…" />
      </div>
    );
  }
  if (sources.data && sources.data.length === 0 && location.pathname !== "/onboarding") {
    return <Navigate to="/onboarding" replace />;
  }
  return (
    <>
      <Outlet />
      <PipPlayer />
    </>
  );
}
