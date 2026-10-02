import clsx from "clsx";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { CalendarSync, CircleAlert, Pencil, Plus, RefreshCw, Trash2, X } from "lucide-react";
import { useEffect, useState, type ReactNode } from "react";
import { useNavigate } from "react-router";
import { api, errorMessage } from "../lib/api";
import { ago, date } from "../lib/format";
import type { Source } from "../lib/types";
import { useSync } from "../stores/sync";
import { SourceForm } from "../components/SourceForm";
import type { StartPage } from "../app/Root";
import { Badge, Button, IconButton, ProgressBar, Segmented, Spinner, Switch, TextField } from "../components/ui";

export function SettingsPage() {
  const navigate = useNavigate();
  const sources = useQuery({ queryKey: ["sources"], queryFn: api.sources, refetchInterval: 5000 });
  return (
    <div className="mx-auto max-w-4xl px-10 pb-20 pt-8">
      <h1 className="mb-8 text-3xl font-bold tracking-tight">Settings</h1>

      <Section
        title="Sources"
        action={
          <Button size="sm" variant="secondary" icon={<Plus className="size-4" />} onClick={() => navigate("/onboarding?add=1")}>
            Add source
          </Button>
        }
      >
        {sources.isLoading && <Spinner />}
        <div className="flex flex-col gap-3">
          {sources.data?.map((s) => <SourceCard key={s.id} source={s} />)}
        </div>
      </Section>

      <PlaybackSettings />
      <About />
    </div>
  );
}

function Section({ title, action, children }: { title: string; action?: ReactNode; children: ReactNode }) {
  return (
    <section className="mb-10">
      <div className="mb-4 flex items-center justify-between">
        <h2 className="text-lg font-bold tracking-tight">{title}</h2>
        {action}
      </div>
      {children}
    </section>
  );
}

function Card({ children, className }: { children: ReactNode; className?: string }) {
  return <div className={clsx("rounded-2xl bg-panel p-5 ring-1 ring-white/[0.06]", className)}>{children}</div>;
}

function SourceCard({ source: s }: { source: Source }) {
  const qc = useQueryClient();
  const sync = useSync((st) => st.bySource[s.id]);
  const [editing, setEditing] = useState(false);
  const [confirm, setConfirm] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const busy = s.syncing || sync?.running;
  const host = (() => {
    try {
      return new URL(s.url).host;
    } catch {
      return s.url;
    }
  })();
  const refresh = () => void qc.invalidateQueries({ queryKey: ["sources"] });

  if (editing) {
    return (
      <Card>
        <SourceForm
          initial={s}
          submitLabel="Save"
          onCancel={() => setEditing(false)}
          onSubmit={async (input) => {
            await api.updateSource(s.id, input);
            setEditing(false);
            refresh();
            await api.syncSource(s.id);
          }}
        />
      </Card>
    );
  }

  const acct = s.account;
  return (
    <Card>
      <div className="flex items-start gap-4">
        <div className="min-w-0 flex-1">
          <div className="flex items-center gap-2">
            <h3 className="truncate text-[16px] font-semibold">{s.name}</h3>
            <Badge>{s.kind === "xtream" ? "Xtream" : "M3U"}</Badge>
            {acct && <Badge tone={acct.status.toLowerCase() === "active" ? "accent" : "live"}>{acct.status}</Badge>}
          </div>
          <p className="mt-0.5 truncate text-[13px] text-faint">
            {host}
            {s.username ? ` · ${s.username}` : ""}
            {s.altUrls.length ? ` · ${s.altUrls.length} backup server${s.altUrls.length > 1 ? "s" : ""}` : ""}
          </p>
        </div>
        <div className="flex shrink-0 gap-1.5">
          <Button size="sm" variant="ghost" icon={<RefreshCw className={clsx("size-4", busy && "animate-spin")} />} disabled={!!busy} onClick={() => void api.syncSource(s.id).then(refresh)}>
            Sync
          </Button>
          <Button size="sm" variant="ghost" icon={<CalendarSync className="size-4" />} disabled={!!busy} onClick={() => void api.syncSource(s.id, true).then(refresh)}>
            Guide
          </Button>
          <Button size="sm" variant="ghost" icon={<Pencil className="size-4" />} onClick={() => setEditing(true)}>
            Edit
          </Button>
          <Button size="sm" variant="ghost" icon={<Trash2 className="size-4" />} onClick={() => setConfirm(true)}>
            Remove
          </Button>
        </div>
      </div>
      <div className="mt-4 grid grid-cols-2 gap-x-8 gap-y-1.5 text-[13px] sm:grid-cols-4">
        <Stat label="Channels" value={s.counts.channels.toLocaleString()} />
        <Stat label="Movies" value={s.counts.movies.toLocaleString()} />
        <Stat label="Series" value={s.counts.series.toLocaleString()} />
        <Stat label="Guide entries" value={s.counts.programmes.toLocaleString()} />
        <Stat label="Library updated" value={busy ? (sync?.message ?? "Updating…") : ago(s.lastSync)} />
        <Stat label="Guide updated" value={ago(s.lastEpgSync)} />
        {acct?.expiresAt ? <Stat label="Expires" value={date(acct.expiresAt)} /> : null}
        {acct ? <Stat label="Connections" value={`${acct.maxConnections} allowed`} /> : null}
        {s.hasPassword ? (
          <Stat label="Password" value={s.passwordLocked ? "Keyring locked" : s.passwordInKeyring ? "System keyring" : "App database"} />
        ) : null}
      </div>
      {(s.syncError || sync?.error || error) && (
        <p className="mt-3 flex items-start gap-2 text-[13px] text-live">
          <CircleAlert className="mt-px size-4 shrink-0" />
          {error ?? sync?.error ?? s.syncError}
        </p>
      )}
      {confirm && (
        <div className="mt-4 flex items-center justify-between gap-4 rounded-xl bg-live/10 p-3 ring-1 ring-live/20">
          <span className="text-[13px] text-fg/90">Remove “{s.name}” with its favorites and watch history?</span>
          <div className="flex gap-2">
            <Button size="sm" variant="ghost" onClick={() => setConfirm(false)}>
              Keep
            </Button>
            <Button
              size="sm"
              variant="danger"
              onClick={async () => {
                try {
                  await api.removeSource(s.id);
                  void qc.invalidateQueries();
                } catch (e) {
                  setError(errorMessage(e));
                  setConfirm(false);
                }
              }}
            >
              Remove
            </Button>
          </div>
        </div>
      )}
    </Card>
  );
}

function Stat({ label, value }: { label: string; value: ReactNode }) {
  return (
    <div className="min-w-0">
      <div className="text-faint">{label}</div>
      <div className="truncate font-medium text-fg/90">{value}</div>
    </div>
  );
}

function PlaybackSettings() {
  const qc = useQueryClient();
  const settings = useQuery({ queryKey: ["settings"], queryFn: api.settings });
  const s = settings.data ?? {};
  const set = async (key: string, value: unknown) => {
    qc.setQueryData(["settings"], { ...s, [key]: value });
    await api.setSetting(key, value);
    if (key === "content.showAdult") void qc.invalidateQueries();
  };
  const str = (k: string) => (typeof s[k] === "string" ? (s[k] as string) : "");

  return (
    <>
      <Section title="General">
        <Card>
          <div className="flex items-center justify-between gap-6 px-1 py-2">
            <span>
              <span className="block text-[15px] font-medium">Start page</span>
              <span className="mt-0.5 block text-[13px] text-dim">Where testpattern opens.</span>
            </span>
            <Segmented<StartPage>
              value={(str("ui.startPage") || "home") as StartPage}
              options={[
                { value: "home", label: "Home" },
                { value: "live", label: "Live TV" },
                { value: "guide", label: "TV Guide" },
              ]}
              onChange={(v) => void set("ui.startPage", v)}
            />
          </div>
          <ArtworkCache limitMb={typeof s["cache.imagesMb"] === "number" ? (s["cache.imagesMb"] as number) : 1024} />
        </Card>
      </Section>
      <Section title="Playback">
        <Card className="flex flex-col gap-2">
          <Switch
            label="Hardware decoding"
            description="Uses the GPU when the driver supports the codec; falls back to software automatically."
            checked={s["player.hwdec"] !== "no"}
            onChange={(v) => void set("player.hwdec", v ? "auto-safe" : "no")}
          />
          <div className="flex items-center justify-between gap-6 px-1 py-2">
            <span>
              <span className="block text-[15px] font-medium">Live stream format</span>
              <span className="mt-0.5 block text-[13px] text-dim">MPEG-TS starts fastest; HLS can be more robust on some servers.</span>
            </span>
            <Segmented
              value={str("player.liveFormat") || "ts"}
              options={[
                { value: "ts", label: "MPEG-TS" },
                { value: "m3u8", label: "HLS" },
              ]}
              onChange={(v) => void set("player.liveFormat", v)}
            />
          </div>
          <Switch
            label="Show subtitles by default"
            description="Otherwise subtitles stay off until you pick a track."
            checked={s["player.subsEnabled"] === true}
            onChange={(v) => void set("player.subsEnabled", v)}
          />
          <div className="grid grid-cols-2 gap-4 px-1 pb-1 pt-2">
            {/* keyed so the fields pick up the stored value once settings load */}
            <LanguageField key={`a:${str("player.audioLang")}`} label="Preferred audio languages" value={str("player.audioLang")} onSave={(v) => void set("player.audioLang", v)} />
            <LanguageField key={`s:${str("player.subLang")}`} label="Preferred subtitle languages" value={str("player.subLang")} onSave={(v) => void set("player.subLang", v)} />
          </div>
        </Card>
      </Section>
      <Section title="Content">
        <Card>
          <Switch
            label="Show adult content"
            description="Categories flagged as adult by the provider stay hidden unless this is on."
            checked={s["content.showAdult"] === true}
            onChange={(v) => void set("content.showAdult", v)}
          />
        </Card>
      </Section>
      <Metadata />
    </>
  );
}

/** TMDB: the user's own API key fills in genres, languages, collections, networks. */
function Metadata() {
  const qc = useQueryClient();
  const status = useQuery({
    queryKey: ["tmdb-status"],
    queryFn: api.tmdbStatus,
    // progress also arrives as events (useBackendEvents)
    refetchInterval: (q) => (q.state.data?.running ? 5000 : false),
  });
  const [key, setKey] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const s = status.data;
  const run = async (f: () => Promise<unknown>) => {
    setBusy(true);
    setError(null);
    try {
      await f();
      setKey("");
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      setBusy(false);
      void qc.invalidateQueries({ queryKey: ["tmdb-status"] });
    }
  };
  return (
    <Section title="Metadata">
      <Card className="flex flex-col gap-3">
        <div className="px-1 pt-1">
          <span className="block text-[15px] font-medium">TMDB</span>
          <span className="mt-0.5 block text-[13px] leading-relaxed text-dim">
            Genres for every movie, original languages, movie collections and TV networks. Updates check TMDB's change
            lists instead of re-downloading, and titles the provider did not id are looked up by title once. Uses your own
            free API key or read access token from themoviedb.org.
          </span>
        </div>
        {s?.configured ? (
          <div className="flex flex-col gap-2 px-1">
            <div className="flex items-center justify-between gap-4 text-[13.5px]">
              <span className="text-dim">
                Details for <span className="font-semibold text-fg">{s.known.toLocaleString()}</span> of {s.titles.toLocaleString()} titles
                {s.unmapped > 0 ? ` · ${s.unmapped.toLocaleString()} without a TMDB match` : ""}
                {s.running ? ` · fetching ${s.done.toLocaleString()} of ${s.total.toLocaleString()}…` : s.lastRun ? ` · checked ${ago(s.lastRun)}` : ""}
              </span>
              <span className="flex shrink-0 gap-2">
                <Button size="sm" icon={<RefreshCw className="size-4" />} disabled={s.running} loading={busy} onClick={() => void run(() => api.tmdbRefresh())}>
                  Update
                </Button>
                <Button size="sm" variant="danger" icon={<Trash2 className="size-4" />} disabled={busy} onClick={() => void run(() => api.tmdbSetKey(""))}>
                  Remove key
                </Button>
              </span>
            </div>
            {s.running && s.total > 0 && <ProgressBar value={s.done / s.total} />}
            {s.error && <p className="text-[13px] text-live">{s.error}</p>}
          </div>
        ) : (
          <form
            className="flex items-end gap-3 px-1"
            onSubmit={(e) => {
              e.preventDefault();
              if (key.trim()) void run(() => api.tmdbSetKey(key.trim()));
            }}
          >
            <TextField
              className="flex-1"
              label="API key or read access token"
              type="password"
              autoComplete="off"
              value={key}
              onChange={(e) => setKey(e.target.value)}
            />
            <Button type="submit" variant="primary" loading={busy} disabled={!key.trim()}>
              Save
            </Button>
          </form>
        )}
        {error && <p className="px-1 text-[13px] text-live">{error}</p>}
        <p className="px-1 pb-1 text-[12px] text-faint">This product uses the TMDB API but is not endorsed or certified by TMDB.</p>
      </Card>
    </Section>
  );
}

function ArtworkCache({ limitMb }: { limitMb: number }) {
  const qc = useQueryClient();
  const usage = useQuery({ queryKey: ["image-cache"], queryFn: api.imageCache });
  const [clearing, setClearing] = useState(false);
  const mb = (bytes: number) => (bytes < 10 << 20 ? (bytes / (1 << 20)).toFixed(1) : Math.round(bytes / (1 << 20)));
  return (
    <div className="flex items-center justify-between gap-6 px-1 py-2">
      <span>
        <span className="block text-[15px] font-medium">Artwork cache</span>
        <span className="mt-0.5 block text-[13px] text-dim">
          {usage.data ? `${mb(usage.data.bytes)} MB in ${usage.data.files.toLocaleString()} images` : "…"} · kept below {limitMb.toLocaleString()} MB,
          least recently used first.
        </span>
      </span>
      <Button
        size="sm"
        loading={clearing}
        disabled={!usage.data?.files}
        onClick={async () => {
          setClearing(true);
          try {
            qc.setQueryData(["image-cache"], await api.clearImageCache());
          } finally {
            setClearing(false);
          }
        }}
      >
        Clear
      </Button>
    </div>
  );
}

function LanguageField({ label, value, onSave }: { label: string; value: string; onSave: (v: string) => void }) {
  const [v, setV] = useState(value);
  return (
    <TextField
      label={label}
      value={v}
      placeholder="eng,en"
      hint="Comma separated ISO codes, most preferred first."
      onChange={(e) => setV(e.target.value)}
      onBlur={() => v !== value && onSave(v.trim())}
      onKeyDown={(e) => e.key === "Enter" && (e.target as HTMLInputElement).blur()}
      spellCheck={false}
    />
  );
}

function About() {
  const versions = useQuery({
    queryKey: ["engine-versions"],
    queryFn: async () => ({ mpv: await api.get<string>("mpv-version"), ffmpeg: await api.get<string>("ffmpeg-version") }),
    staleTime: Infinity,
  });
  const [legal, setLegal] = useState<LegalDoc | null>(null);
  return (
    <Section title="About">
      <Card className="text-[13.5px] leading-relaxed text-dim">
        <p className="text-fg">testpattern 0.1.0</p>
        <p>
          Playback engine: {versions.data?.mpv ?? "mpv"} · FFmpeg {versions.data?.ffmpeg ?? ""} (built in, statically linked).
        </p>
        {/* GPLv3 §0 "Appropriate Legal Notices" */}
        <p className="mt-2">
          Copyright © 2026 the testpattern authors. testpattern is free software: you can redistribute it and/or modify it under the terms of
          the GNU General Public License, version 3 or (at your option) any later version. It comes with ABSOLUTELY NO WARRANTY.
        </p>
        <p className="mt-2 text-faint">
          It includes GPL builds of FFmpeg and mpv and other open-source components. testpattern does not provide any content; add sources
          you are entitled to use.
        </p>
        <div className="mt-4 flex gap-2">
          <Button size="sm" onClick={() => setLegal("license")}>
            License
          </Button>
          <Button size="sm" onClick={() => setLegal("notices")}>
            Third-party notices
          </Button>
        </div>
      </Card>
      {legal && <LegalDialog doc={legal} onClose={() => setLegal(null)} />}
    </Section>
  );
}

type LegalDoc = "license" | "notices";

/** LICENSE / THIRD_PARTY_NOTICES.md, embedded so they travel with the binary. */
function LegalDialog({ doc, onClose }: { doc: LegalDoc; onClose: () => void }) {
  const text = useQuery({
    queryKey: ["legal", doc],
    // separate chunks: loaded only when opened
    queryFn: async () =>
      doc === "license" ? (await import("../../LICENSE?raw")).default : (await import("../../THIRD_PARTY_NOTICES.md?raw")).default,
    staleTime: Infinity,
  });
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && onClose();
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);
  return (
    <div className="fixed inset-0 z-50 grid place-items-center bg-black/60 p-6 backdrop-blur-sm animate-fade-in" onClick={onClose}>
      <div
        role="dialog"
        aria-label={doc === "license" ? "License" : "Third-party notices"}
        className="flex h-[82vh] w-full max-w-4xl flex-col rounded-3xl bg-panel ring-1 ring-white/10 animate-rise-in"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="flex items-center justify-between px-6 pb-3 pt-5">
          <h2 className="text-lg font-bold">{doc === "license" ? "GNU General Public License v3" : "Third-party notices"}</h2>
          <IconButton label="Close" size="sm" onClick={onClose}>
            <X className="size-4" />
          </IconButton>
        </div>
        <div className="min-h-0 flex-1 overflow-y-auto px-6 pb-6">
          {text.data ? (
            <pre className="select-text whitespace-pre-wrap font-mono text-[12px] leading-relaxed text-fg/80">{text.data}</pre>
          ) : (
            <Spinner label="Loading…" />
          )}
        </div>
      </div>
    </div>
  );
}
