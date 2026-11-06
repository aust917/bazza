import { useTranslation } from "react-i18next";
import { Circle, Library, MessageCircle, Settings as SettingsIcon, Square, Users } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { NavLink, Outlet, useLocation, useNavigate } from "react-router-dom";
import { cn } from "@/lib/cn";
import { fmtDuration } from "@/lib/format";
import { displayError } from "@/lib/i18n";
import { cmd, subscribe, type RecordingState } from "@/lib/ipc";
import { Button, Kbd } from "./ui";
import { ModelDownloadProgress } from "./ModelDownloadProgress";
import { OperationProgress } from "./ui/OperationProgress";
import { Toasts } from "./Toasts";
import { UpdateBanner } from "./UpdateBanner";

export function Shell({ serviceDesktop = false, managedLibrary = false }: { serviceDesktop?: boolean; managedLibrary?: boolean }) {
  const { t: tr } = useTranslation("main");
  const { t: recordingText } = useTranslation("recording");
  const location = useLocation();
  const navigate = useNavigate();
  const [rec, setRec] = useState<RecordingState>({ state: "idle", meeting_id: null, elapsed_ms: 0 });
  const [hotkey, setHotkey] = useState("Ctrl+Shift+R");
  const [downloads, setDownloads] = useState<Record<string, { done: number; total: number }>>({});
  const [stopError, setStopError] = useState<string | null>(null);
  const [stopStartedAt, setStopStartedAt] = useState<number>();
  const stopInFlight = useRef(false);

  useEffect(() => subscribe("model_progress", (p) => {
    setDownloads((d) => {
      const next = { ...d };
      if (p.status === "downloading") next[p.id] = { done: p.done, total: p.total };
      else delete next[p.id];
      return next;
    });
  }), []);

  useEffect(() => {
    let active = true;
    let revision = 0;
    let latestState: RecordingState["state"] = "idle";
    let timer: number;
    const update = (status: RecordingState) => {
      latestState = status.state;
      setRec((previous) => previous.state === status.state && previous.meeting_id === status.meeting_id
        && previous.elapsed_ms === status.elapsed_ms ? previous : status);
    };
    function schedule(delay = latestState === "idle" ? 5_000 : 1_000) {
      window.clearTimeout(timer);
      if (active && !document.hidden) timer = window.setTimeout(() => void refresh(), delay);
    }
    async function refresh() {
      const request = ++revision;
      try {
        const status = await cmd.recordingStatus();
        if (active && request === revision) update(status);
      } catch { /* Keep the last status and retry on the next refresh. */ }
      finally { if (active && request === revision) schedule(); }
    }
    const un = subscribe("recording_state", (status) => {
      if (!active) return;
      revision++;
      update(status);
      schedule();
    });
    const onVisible = () => schedule(0);
    window.addEventListener("focus", onVisible);
    document.addEventListener("visibilitychange", onVisible);
    void refresh();
    return () => {
      active = false;
      un();
      window.clearTimeout(timer);
      window.removeEventListener("focus", onVisible);
      document.removeEventListener("visibilitychange", onVisible);
    };
  }, []);

  useEffect(() => {
    if (serviceDesktop) return;
    let active = true;
    void cmd.getSettings().then((s) => {
      if (active) setHotkey(s.hotkey.replace("CmdOrCtrl", navigator.platform.includes("Mac") ? "Cmd" : "Ctrl"));
    }).catch(() => {});
    return () => { active = false; };
  }, [rec.state, serviceDesktop]);

  const recording = rec.state !== "idle" || stopStartedAt !== undefined;
  const transition = stopStartedAt !== undefined || rec.state === "finishing" ? "finishing"
    : rec.state === "starting" ? "starting" : null;
  const recordingSetup = location.pathname === "/recording";
  const canOpenSettings = !serviceDesktop || managedLibrary;
  const stop = async () => {
    if (stopInFlight.current || transition) return;
    stopInFlight.current = true;
    setStopStartedAt(performance.now());
    setStopError(null);
    try { await cmd.stopRecording(); }
    catch (error) { setStopError(displayError(error)); }
    finally { stopInFlight.current = false; setStopStartedAt(undefined); }
  };

  return (
    <div className="grain flex h-full">
      <aside className="relative z-10 flex w-[232px] shrink-0 flex-col border-r border-line bg-canvas-2/60 backdrop-blur">
        <div className="flex items-center px-5 pt-5 pb-4">
          <picture>
            <source media="(prefers-color-scheme: dark)" srcSet="/brand/bazza-wordmark-dark.svg" />
            <img src="/brand/bazza-wordmark-light.svg" width={117} height={35} alt="Bazza" className="block h-7 w-auto" />
          </picture>
        </div>

        <nav className="flex flex-col gap-0.5 px-3">
          <NavItem to="/" icon={<Library size={16} />} label={tr("nav.library")} />
          {!serviceDesktop && <>
            <NavItem to="/ask" icon={<MessageCircle size={16} />} label={tr("nav.ask")} />
            <NavItem to="/voices" icon={<Users size={16} />} label={tr("nav.voices")} />
          </>}
          {canOpenSettings && <NavItem to="/settings" icon={<SettingsIcon size={16} />} label={tr("nav.settings")} />}
        </nav>

        <div className="mt-auto p-3">
          {Object.keys(downloads).length > 0 && <DownloadPill downloads={downloads} canOpenSettings={canOpenSettings} />}
          <div className={cn("rounded-lg border p-3 transition-colors", recording ? "border-ember/30 bg-ember-soft" : "border-line bg-canvas")}>
            {recording ? (
              <>
                {transition ? (
                  <OperationProgress label={tr(`shell.${transition}`)} startedAt={stopStartedAt} announceStage />
                ) : (
                  <div className="flex items-center gap-2 text-[12px] font-semibold text-ember">
                    <span className="rec-dot h-2 w-2 rounded-full bg-ember" />
                    {tr(rec.state === "idle" ? "shell.ready" : `shell.${rec.state}`)}
                    <span className="ml-auto font-mono tabular-nums">{fmtDuration(rec.elapsed_ms)}</span>
                  </div>
                )}
                <Button variant="ember" size="sm" className="mt-2 w-full" disabled={transition !== null} onClick={() => void stop()}>
                  <Square size={12} fill="currentColor" /> {tr(rec.state === "pending" ? "shell.retryStop" : "shell.stop")}
                </Button>
              </>
            ) : (
              <>
                <div className="text-[12px] text-ink-2">{tr("shell.ready")}</div>
                <Button variant={recordingSetup ? "soft" : "primary"} size="sm" className="mt-2 w-full" aria-current={recordingSetup ? "page" : undefined} onClick={() => {
                  if (!recordingSetup) navigate("/recording", { state: { returnTo: location.pathname + location.search } });
                }}>
                  <Circle size={12} className="text-ember" fill="currentColor" /> {recordingSetup ? recordingText("setup.title") : tr("shell.recordNow")}
                </Button>
                {!serviceDesktop && <div className="mt-2 text-center text-[11px] text-ink-3">
                  {tr("shell.orPress")} <Kbd>{hotkey}</Kbd>
                </div>}
              </>
            )}
            {stopError && <p role="alert" className="mt-2 text-xs text-ember">{stopError}</p>}
          </div>
        </div>
      </aside>

      <main className="relative z-10 min-w-0 flex-1 overflow-y-auto">
        <Outlet />
      </main>
      <Toasts />
      {!serviceDesktop && <UpdateBanner />}
    </div>
  );
}

function NavItem({ to, icon, label }: { to: string; icon: React.ReactNode; label: string }) {
  return (
    <NavLink
      to={to}
      end={to === "/"}
      className={({ isActive }) =>
        cn(
          "focus-ring flex h-9 items-center gap-2.5 rounded-md px-3 text-sm font-medium transition-colors",
          isActive ? "bg-canvas text-ink shadow-card" : "text-ink-2 hover:bg-canvas-3 hover:text-ink",
        )
      }
    >
      {icon}
      {label}
    </NavLink>
  );
}

function DownloadPill({ downloads, canOpenSettings }: {
  downloads: Record<string, { done: number; total: number }>;
  canOpenSettings: boolean;
}) {
  const { t: tr } = useTranslation("main");
  const models = Object.entries(downloads);
  const title = tr("shell.downloading", { count: models.length });
  return (
    <div className="mb-3 rounded-lg border border-line bg-canvas px-3 py-2 text-[12px]">
      {canOpenSettings
        ? <NavLink to="/settings" className="focus-ring rounded font-medium text-ink-2 underline underline-offset-2 hover:text-ink">{title}</NavLink>
        : <p className="font-medium text-ink-2">{title}</p>}
      <ul className="mt-1 max-h-36 divide-y divide-line overflow-y-auto">
        {models.map(([id, { done, total }]) => (
          <li key={id} className="min-w-0 py-2 [overflow-wrap:anywhere]">
            <p className="font-medium text-ink">{id}</p>
            <ModelDownloadProgress modelName={id} done={done} total={total} />
          </li>
        ))}
      </ul>
    </div>
  );
}
