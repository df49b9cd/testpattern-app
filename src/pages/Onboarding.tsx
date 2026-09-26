import { useQueryClient } from "@tanstack/react-query";
import { CircleAlert, Loader2 } from "lucide-react";
import { useEffect, useState } from "react";
import { useNavigate, useSearchParams } from "react-router";
import { api, errorMessage } from "../lib/api";
import type { Source } from "../lib/types";
import { useSync } from "../stores/sync";
import { SourceForm } from "../components/SourceForm";
import { Button } from "../components/ui";

const BARS = ["#c0c0c0", "#c0c000", "#00c0c0", "#00c000", "#c000c0", "#c00000", "#0000c0"];

export function OnboardingPage() {
  const navigate = useNavigate();
  const qc = useQueryClient();
  const [params] = useSearchParams();
  const adding = params.get("add") === "1";
  const [source, setSource] = useState<Source | null>(null);

  return (
    <div className="relative grid h-full place-items-center overflow-y-auto bg-bg px-6 py-10">
      {/* test pattern glow */}
      <div className="pointer-events-none absolute inset-x-0 top-0 flex h-72 opacity-[0.18] blur-3xl">
        {BARS.map((c) => (
          <div key={c} className="flex-1" style={{ background: c }} />
        ))}
      </div>
      <div className="relative w-full max-w-[560px] animate-rise-in">
        <div className="mb-8 flex flex-col items-center text-center">
          <div className="mb-5 flex h-14 overflow-hidden rounded-2xl shadow-2xl ring-1 ring-white/15">
            {BARS.map((c) => (
              <span key={c} className="h-full w-[10px]" style={{ background: c }} />
            ))}
          </div>
          <h1 className="text-4xl font-extrabold tracking-tight">{adding ? "Add a source" : "Welcome to testpattern"}</h1>
          <p className="mt-2 text-[15px] text-dim">
            {adding ? "Connect another IPTV provider." : "Add your IPTV provider to start watching live TV, movies and series."}
          </p>
        </div>
        <div className="rounded-3xl bg-panel/90 p-7 shadow-[0_40px_80px_-30px_rgb(0_0_0/0.9)] ring-1 ring-white/[0.08] backdrop-blur-xl">
          {source ? (
            <SyncProgress
              source={source}
              onDone={() => {
                void qc.invalidateQueries();
                navigate("/", { replace: true });
              }}
              onRetry={() => setSource(null)}
            />
          ) : (
            <SourceForm
              submitLabel="Add source"
              onCancel={adding ? () => navigate(-1) : undefined}
              onSubmit={async (input) => {
                const s = await api.addSource(input);
                setSource(s);
              }}
            />
          )}
        </div>
      </div>
    </div>
  );
}

/** Follows the first sync (events in the app, polling in the browser preview). */
function SyncProgress({ source, onDone, onRetry }: { source: Source; onDone: () => void; onRetry: () => void }) {
  const live = useSync((s) => s.bySource[source.id]);
  const [polled, setPolled] = useState<Source | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let alive = true;
    const tick = async () => {
      try {
        const all = await api.sources();
        const s = all.find((x) => x.id === source.id) ?? null;
        if (!alive) return;
        setPolled(s);
        if (s && !s.syncing) {
          if (s.syncError) setError(s.syncError);
          else if (s.lastSync) onDone();
        }
      } catch (e) {
        if (alive) setError(errorMessage(e));
      }
    };
    const i = window.setInterval(() => void tick(), 1500);
    return () => {
      alive = false;
      window.clearInterval(i);
    };
  }, [source.id, onDone]);

  const failed = error ?? live?.error;
  if (failed) {
    return (
      <div className="flex flex-col items-center gap-4 py-4 text-center">
        <CircleAlert className="size-10 text-live" />
        <div>
          <h2 className="text-lg font-bold">Couldn’t load {source.name}</h2>
          <p className="mt-1 text-sm text-dim">{failed}</p>
        </div>
        <Button variant="primary" onClick={onRetry}>
          Check the details
        </Button>
      </div>
    );
  }
  const counts = polled?.counts;
  return (
    <div className="flex flex-col items-center gap-4 py-6 text-center">
      <Loader2 className="size-10 animate-spin text-accent-strong" />
      <div>
        <h2 className="text-lg font-bold">Setting up {source.name}</h2>
        <p className="mt-1 text-sm text-dim">{live?.message || "Downloading your channels, movies and series…"}</p>
      </div>
      {counts && counts.channels + counts.movies + counts.series > 0 && (
        <p className="text-[13px] text-faint">
          {counts.channels.toLocaleString()} channels · {counts.movies.toLocaleString()} movies · {counts.series.toLocaleString()} series
        </p>
      )}
    </div>
  );
}
