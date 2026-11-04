import clsx from "clsx";
import { Cast, Check, Loader2, MonitorOff, Plus, Tv } from "lucide-react";
import { useEffect, useRef, useState, type FormEvent } from "react";
import { api, errorMessage } from "../lib/api";
import { listen } from "../lib/bridge";
import type { CastDevice, CastDevices } from "../lib/types";
import { usePlayer } from "../stores/player";
import { Button, IconButton } from "./ui";

/** Chromecasts on the network, kept current while a picker is open. */
function useCastDevices(active: boolean) {
  const [list, setList] = useState<CastDevices | null>(null);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    if (!active) return;
    let alive = true;
    const unlisten = listen<CastDevices>("cast://devices", (e) => alive && setList(e.payload));
    api
      .castDevices()
      .then((d) => alive && setList(d))
      .catch((e) => alive && setError(errorMessage(e)));
    return () => {
      alive = false;
      void unlisten.then((f) => f());
    };
  }, [active]);
  return { devices: list?.devices ?? null, searching: list?.searching ?? true, error };
}

/** For networks where the TV can't be found: its address from Google Home. */
function AddByAddress({ onAdded }: { onAdded: (d: CastDevice) => void }) {
  const [open, setOpen] = useState(false);
  const [address, setAddress] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const submit = (e: FormEvent) => {
    e.preventDefault();
    if (!address.trim() || busy) return;
    setBusy(true);
    setError(null);
    api
      .castAddDevice(address)
      .then((d) => {
        setAddress("");
        setOpen(false);
        onAdded(d);
      })
      .catch((err) => setError(errorMessage(err)))
      .finally(() => setBusy(false));
  };
  if (!open) {
    return (
      <button
        onClick={() => setOpen(true)}
        className="flex w-full items-center gap-3 rounded-xl px-3 py-2 text-left text-sm text-dim transition-colors hover:bg-white/[0.06] hover:text-fg"
      >
        <Plus className="size-4 shrink-0 text-faint" />
        Add a TV by IP address
      </button>
    );
  }
  return (
    <form onSubmit={submit} className="flex flex-col gap-2 px-3 py-2">
      <label className="text-xs text-faint" htmlFor="cast-address">
        The TV's IP address (Google Home → the device → Settings → Device information)
      </label>
      <div className="flex gap-2">
        <input
          id="cast-address"
          autoFocus
          inputMode="decimal"
          placeholder="192.168.1.20"
          value={address}
          onChange={(e) => setAddress(e.target.value)}
          className="min-w-0 flex-1 rounded-lg bg-white/[0.06] px-2.5 py-1.5 text-sm text-fg ring-1 ring-white/10 placeholder:text-faint focus:outline-none focus:ring-accent"
        />
        <Button type="submit" variant="primary" size="sm" loading={busy} disabled={!address.trim()}>
          Add
        </Button>
      </div>
      {error && <p className="text-xs text-live">{error}</p>}
    </form>
  );
}

/**
 * The cast button and its device picker. `placement` puts the menu above
 * (player chrome) or below (sidebar, detail pages) the button.
 */
export function CastMenu({
  placement = "above",
  className,
  triggerClassName = "text-white",
}: {
  placement?: "above" | "below";
  className?: string;
  /** the button's color when not casting */
  triggerClassName?: string;
}) {
  const [open, setOpen] = useState(false);
  const cast = usePlayer((s) => s.cast);
  const { devices, searching, error } = useCastDevices(open);
  const ref = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!open) return;
    const onDown = (e: PointerEvent) => {
      if (!ref.current?.contains(e.target as Node)) setOpen(false);
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.stopPropagation();
        setOpen(false);
      }
    };
    window.addEventListener("pointerdown", onDown);
    window.addEventListener("keydown", onKey, true);
    return () => {
      window.removeEventListener("pointerdown", onDown);
      window.removeEventListener("keydown", onKey, true);
    };
  }, [open]);

  const label = cast ? `Casting to ${cast.deviceName}` : "Cast to a TV";
  return (
    <div ref={ref} className={clsx("relative", className)}>
      <IconButton label={label} active={open || !!cast} onClick={() => setOpen((v) => !v)} className={clsx(!cast && triggerClassName)}>
        <Cast className="size-5" />
      </IconButton>
      {open && (
        <div
          className={clsx(
            "absolute z-20 w-72 rounded-2xl bg-panel/95 p-1.5 shadow-2xl ring-1 ring-white/10 backdrop-blur-xl animate-fade-in",
            placement === "above" ? "bottom-12 right-0" : "left-0 top-12",
          )}
        >
          <div className="px-3 pb-1 pt-2 text-[11px] font-bold uppercase tracking-wider text-faint">Cast to</div>
          {error ? (
            <p className="px-3 py-2 text-sm text-live">{error}</p>
          ) : !devices?.length ? (
            searching ? (
              <div className="flex flex-col gap-1 px-3 py-2 text-sm text-dim">
                <span className="flex items-center gap-2">
                  <Loader2 className="size-4 animate-spin" /> Looking for TVs…
                </span>
                <span className="text-xs text-faint">Chromecasts and Google TVs on this network show up here.</span>
              </div>
            ) : (
              <div className="flex flex-col gap-1 px-3 py-2 text-sm text-dim">
                <span>No TVs found</span>
                <span className="text-xs text-faint">
                  Make sure the TV is on and on the same network as this computer, or add it by its address.
                </span>
              </div>
            )
          ) : (
            devices.map((d) => {
              const active = cast?.deviceId === d.id;
              return (
                <button
                  key={d.id}
                  onClick={() => {
                    setOpen(false);
                    void usePlayer.getState().castTo(d);
                  }}
                  className={clsx(
                    "flex w-full items-center gap-3 rounded-xl px-3 py-2 text-left text-sm transition-colors",
                    active ? "bg-accent/20 text-fg" : "text-dim hover:bg-white/[0.06] hover:text-fg",
                  )}
                >
                  <Tv className={clsx("size-4 shrink-0", active ? "text-accent-strong" : "text-faint")} />
                  <span className="min-w-0 flex-1">
                    <span className="block truncate">{d.name}</span>
                    {d.model && <span className="block truncate text-xs text-faint">{d.model}</span>}
                  </span>
                  {active && <Check className="size-4 shrink-0 text-accent-strong" />}
                </button>
              );
            })
          )}
          {devices && devices.length > 0 && searching && (
            <div className="flex items-center gap-2 px-3 py-1.5 text-xs text-faint">
              <Loader2 className="size-3 animate-spin" /> Looking for more…
            </div>
          )}
          {!error && (
            <>
              <div className="mx-3 my-1 border-t border-white/[0.06]" />
              <AddByAddress
                onAdded={(d) => {
                  setOpen(false);
                  void usePlayer.getState().castTo(d);
                }}
              />
            </>
          )}
          {cast && (
            <>
              <button
                onClick={() => {
                  setOpen(false);
                  void usePlayer.getState().stopCasting();
                }}
                className="flex w-full items-center gap-3 rounded-xl px-3 py-2 text-left text-sm text-dim transition-colors hover:bg-white/[0.06] hover:text-fg"
              >
                <MonitorOff className="size-4 shrink-0 text-faint" />
                Stop casting
              </button>
            </>
          )}
        </div>
      )}
    </div>
  );
}

/** Fills the video area while the picture is on the TV. */
export function CastingBackdrop() {
  const now = usePlayer((s) => s.now);
  const cast = usePlayer((s) => s.cast);
  if (!cast) return null;
  const waiting = cast.state === "connecting" || cast.state === "loading";
  return (
    <div className="pointer-events-none absolute inset-0 grid place-items-center bg-bg">
      <div className="flex flex-col items-center gap-4 text-center">
        <div className="grid size-20 place-items-center rounded-3xl bg-accent/15 text-accent-strong ring-1 ring-accent/25">
          {waiting ? <Loader2 className="size-9 animate-spin" /> : <Cast className="size-9" />}
        </div>
        <div>
          <div className="text-sm font-semibold uppercase tracking-wider text-dim">
            {cast.state === "connecting" ? "Connecting to" : waiting ? "Starting on" : "Playing on"}
          </div>
          <div className="mt-1 text-2xl font-bold">{cast.deviceName}</div>
          {now && <div className="mt-2 max-w-lg truncate text-sm text-faint">{now.title}</div>}
        </div>
      </div>
    </div>
  );
}
