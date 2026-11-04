import { useEffect } from "react";
import { Navigate, Outlet, useLocation, useNavigate } from "react-router";
import { useSources } from "../lib/queries";
import { ROUTES, START_PAGES } from "../lib/routes";
import { useSettings } from "../lib/settings";
import { useBackendEvents } from "../hooks/useBackendEvents";
import { useProgressSaver } from "../hooks/useProgressSaver";
import { Spinner } from "../components/ui";
import { setVideoViewport, usePlayer } from "../stores/player";
import { PipPlayer } from "../components/PipPlayer";

// once per launch, not every time the user comes back to Home
let startPageApplied = false;

/** App-wide effects + first-run redirect. */
export function Root() {
  useBackendEvents();
  useProgressSaver();
  const location = useLocation();
  const navigate = useNavigate();
  const sources = useSources();
  const settings = useSettings();

  // Leaving the live preview / player: video goes back to full size, and
  // live previews stop unless the player is being opened.
  // Picture-in-picture (T-034) keeps playing and positions the video itself.
  useEffect(() => {
    const { now, stop, pip } = usePlayer.getState();
    if (pip && location.pathname !== ROUTES.player) return;
    if (location.pathname !== ROUTES.live) void setVideoViewport(null);
    if (now?.kind === "live" && location.pathname !== ROUTES.live && location.pathname !== ROUTES.player) void stop();
  }, [location.pathname]);

  useEffect(() => {
    if (startPageApplied || !settings.data || !sources.data?.length) return;
    startPageApplied = true;
    const choice = settings.data["ui.startPage"];
    const page = choice && START_PAGES[choice];
    if (page && page !== ROUTES.home && location.pathname === ROUTES.home) navigate(page, { replace: true });
  }, [settings.data, sources.data, location.pathname, navigate]);

  if (sources.isLoading) {
    return (
      <div className="grid h-full place-items-center bg-bg">
        <Spinner label="Loading…" />
      </div>
    );
  }
  if (sources.data && sources.data.length === 0 && location.pathname !== ROUTES.onboarding) {
    return <Navigate to={ROUTES.onboarding} replace />;
  }
  return (
    <>
      <Outlet />
      <PipPlayer />
    </>
  );
}
