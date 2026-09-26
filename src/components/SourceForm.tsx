import clsx from "clsx";
import { CheckCircle2, CircleAlert, Link2, List, Lock, Server, Tag, User } from "lucide-react";
import { useState, type FormEvent } from "react";
import { api, errorMessage } from "../lib/api";
import { date } from "../lib/format";
import type { Source, SourceInput, SourceKind, TestResult } from "../lib/types";
import { Button, Segmented, TextField } from "./ui";

/** Add/edit form for Xtream Codes accounts and M3U playlists. */
export function SourceForm({
  initial,
  submitLabel,
  onSubmit,
  onCancel,
}: {
  initial?: Source;
  submitLabel: string;
  onSubmit: (input: SourceInput) => Promise<void>;
  onCancel?: () => void;
}) {
  const [kind, setKind] = useState<SourceKind>(initial?.kind ?? "xtream");
  const [name, setName] = useState(initial?.name ?? "");
  const [url, setUrl] = useState(initial?.url ?? "");
  const [username, setUsername] = useState(initial?.username ?? "");
  const [password, setPassword] = useState("");
  const [altUrls, setAltUrls] = useState((initial?.altUrls ?? []).join("\n"));
  const [epgUrl, setEpgUrl] = useState(initial?.epgUrl ?? "");
  const [userAgent, setUserAgent] = useState(initial?.userAgent ?? "");
  const [advanced, setAdvanced] = useState(false);
  const [test, setTest] = useState<{ ok: true; result: TestResult } | { ok: false; error: string } | null>(null);
  const [busy, setBusy] = useState<"test" | "submit" | null>(null);

  const input = (): SourceInput => ({
    kind,
    name: name.trim() || undefined,
    url: url.trim(),
    username: kind === "xtream" ? username.trim() : undefined,
    password: kind === "xtream" && password ? password : undefined,
    altUrls: altUrls
      .split(/\s+/)
      .map((u) => u.trim())
      .filter(Boolean),
    epgUrl: epgUrl.trim() || undefined,
    userAgent: userAgent.trim() || undefined,
  });

  // Pasting an Xtream playlist link into the M3U tab is recognised by the
  // backend and upgraded to a full Xtream source.
  const looksLikeXtreamLink = kind === "m3u" && /get\.php\?.*username=.*password=/i.test(url);

  const runTest = async () => {
    setBusy("test");
    setTest(null);
    try {
      setTest({ ok: true, result: await api.testSource(input(), initial?.id) });
    } catch (e) {
      setTest({ ok: false, error: errorMessage(e) });
    } finally {
      setBusy(null);
    }
  };

  const submit = async (e: FormEvent) => {
    e.preventDefault();
    setBusy("submit");
    try {
      await onSubmit(input());
    } catch (err) {
      setTest({ ok: false, error: errorMessage(err) });
    } finally {
      setBusy(null);
    }
  };

  const canSubmit = url.trim() && (kind === "m3u" || (username.trim() && (password || initial?.hasPassword)));

  return (
    <form onSubmit={submit} className="flex flex-col gap-5">
      <Segmented<SourceKind>
        value={kind}
        onChange={(k) => {
          setKind(k);
          setTest(null);
        }}
        options={[
          { value: "xtream", label: "Xtream Codes" },
          { value: "m3u", label: "M3U playlist" },
        ]}
        className="self-start"
      />
      {kind === "xtream" ? (
        <>
          <TextField label="Server URL" icon={<Server />} placeholder="http://provider.example:8080" value={url} onChange={(e) => setUrl(e.target.value)} autoFocus spellCheck={false} />
          <div className="grid grid-cols-2 gap-4">
            <TextField label="Username" icon={<User />} value={username} onChange={(e) => setUsername(e.target.value)} autoComplete="off" spellCheck={false} />
            <TextField
              label="Password"
              icon={<Lock />}
              type="password"
              value={password}
              placeholder={initial?.hasPassword ? "unchanged" : ""}
              onChange={(e) => setPassword(e.target.value)}
              autoComplete="new-password"
            />
          </div>
        </>
      ) : (
        <>
          <TextField
            label="Playlist URL"
            icon={<List />}
            placeholder="https://provider.example/playlist.m3u"
            value={url}
            onChange={(e) => setUrl(e.target.value)}
            autoFocus
            spellCheck={false}
            hint={looksLikeXtreamLink ? "This is an Xtream Codes link — it will be added as an Xtream account for richer data." : undefined}
          />
          <TextField label="TV guide (XMLTV) URL — optional" icon={<Link2 />} placeholder="Taken from the playlist when empty" value={epgUrl} onChange={(e) => setEpgUrl(e.target.value)} spellCheck={false} />
        </>
      )}
      <TextField label="Name — optional" icon={<Tag />} placeholder="My provider" value={name} onChange={(e) => setName(e.target.value)} />

      <button type="button" onClick={() => setAdvanced((v) => !v)} className="self-start text-[13px] font-semibold text-dim hover:text-fg">
        {advanced ? "Hide" : "Show"} advanced options
      </button>
      {advanced && (
        <div className="flex flex-col gap-4 rounded-2xl bg-white/[0.03] p-4 ring-1 ring-white/[0.06]">
          {kind === "xtream" && (
            <label className="flex flex-col gap-1.5">
              <span className="text-[13px] font-medium text-dim">Backup server URLs (one per line)</span>
              <textarea
                value={altUrls}
                onChange={(e) => setAltUrls(e.target.value)}
                rows={3}
                spellCheck={false}
                placeholder="http://backup1.example&#10;http://backup2.example"
                className="rounded-xl bg-white/[0.06] px-3.5 py-2.5 text-sm outline-none ring-1 ring-white/10 placeholder:text-faint focus:ring-2 focus:ring-accent"
              />
              <span className="text-xs text-faint">Used automatically when the main server fails.</span>
            </label>
          )}
          {kind === "xtream" && (
            <TextField label="TV guide (XMLTV) URL override" value={epgUrl} onChange={(e) => setEpgUrl(e.target.value)} placeholder="Provider guide is used when empty" spellCheck={false} />
          )}
          <TextField label="User agent" value={userAgent} onChange={(e) => setUserAgent(e.target.value)} placeholder="testpattern default" spellCheck={false} />
        </div>
      )}

      {test && <TestOutcome test={test} />}

      <div className="mt-1 flex items-center gap-3">
        <Button type="submit" variant="primary" size="lg" loading={busy === "submit"} disabled={!canSubmit || busy !== null}>
          {submitLabel}
        </Button>
        <Button type="button" size="lg" loading={busy === "test"} disabled={!url.trim() || busy !== null} onClick={() => void runTest()}>
          Test connection
        </Button>
        {onCancel && (
          <Button type="button" variant="ghost" size="lg" onClick={onCancel}>
            Cancel
          </Button>
        )}
      </div>
    </form>
  );
}

function TestOutcome({ test }: { test: { ok: true; result: TestResult } | { ok: false; error: string } }) {
  if (!test.ok) {
    return (
      <div className="flex items-start gap-3 rounded-2xl bg-live/10 p-4 text-sm text-live ring-1 ring-live/20">
        <CircleAlert className="mt-0.5 size-5 shrink-0" />
        <span className="leading-relaxed">{test.error}</span>
      </div>
    );
  }
  const r = test.result;
  return (
    <div className="flex items-start gap-3 rounded-2xl bg-ok/10 p-4 text-sm ring-1 ring-ok/20">
      <CheckCircle2 className="mt-0.5 size-5 shrink-0 text-ok" />
      {r.kind === "xtream" ? (
        <div className="grid grid-cols-[auto_1fr] gap-x-4 gap-y-1 text-fg/85">
          <span className="text-faint">Status</span>
          <span className={clsx(r.account.status.toLowerCase() === "active" ? "text-ok" : "text-live")}>{r.account.status}</span>
          {r.account.expiresAt ? (
            <>
              <span className="text-faint">Expires</span>
              <span>{date(r.account.expiresAt)}</span>
            </>
          ) : null}
          <span className="text-faint">Connections</span>
          <span>
            {r.account.activeConnections} of {r.account.maxConnections} in use
          </span>
          {r.account.message && (
            <>
              <span className="text-faint">Message</span>
              <span>{r.account.message}</span>
            </>
          )}
        </div>
      ) : (
        <div className="text-fg/85">
          Found {r.channels.toLocaleString()} channels, {r.movies.toLocaleString()} movies and {r.episodes.toLocaleString()} episodes
          {r.epgUrls.length ? " · includes a TV guide" : ""}.
        </div>
      )}
    </div>
  );
}
