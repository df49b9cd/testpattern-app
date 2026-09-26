import { useQuery } from "@tanstack/react-query";
import { useEffect } from "react";
import { Navigate, Outlet, useLocation } from "react-router";
import { api } from "../lib/api";
import { useBackendEvents } from "../hooks/useBackendEvents";
import { useProgressSaver } from "../hooks/useProgressSaver";
import { Spinner } from "../components/ui";
import { setVideoViewport, usePlayer } from "../stores/player";

/** App-wide effects + first-run redirect. */
export function Root() {
  useBackendEvents();
  useProgressSaver();
  const location = useLocation();
  const sources = useQuery({ queryKey: ["sources"], queryFn: api.sources });

  // Leaving the live preview / player: video goes back to full size, and
  // live previews stop unless the player is being opened.
  useEffect(() => {
    if (location.pathname !== "/live") void setVideoViewport(null);
    const { now, stop } = usePlayer.getState();
    if (now?.kind === "live" && location.pathname !== "/live" && location.pathname !== "/player") void stop();
  }, [location.pathname]);

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
  return <Outlet />;
}
