import { Circle, Video, VideoOff } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { Button } from "@/components/ui";
import { cmd, subscribe, type AudioDevice, type AudioScope, type Events, type Settings, type VideoSource, type VideoTarget } from "@/lib/ipc";

const keyOf = (t: VideoTarget) => (t.kind === "monitor" ? `m:${t.index}` : `w:${t.title}`);

/** "Call detected - Record?" window. Nothing happens unless Record is clicked. */
export function Popup() {
  const [det, setDet] = useState<Events["detection"] | null>(null);
  const [video, setVideo] = useState(true);
  const [left, setLeft] = useState(30);
  const [sources, setSources] = useState<VideoSource[]>([]);
  const [target, setTarget] = useState<VideoTarget | null>(null);
  const [audioScope, setAudioScope] = useState<AudioScope>("app");
  const [settings, setSettings] = useState<Settings | null>(null);
  const [outputs, setOutputs] = useState<AudioDevice[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const [starting, setStarting] = useState(false);
  const [loadingSources, setLoadingSources] = useState(false);
  const startPending = useRef(false);
  const manual = det?.app === "manual";
  const isWindows = navigator.userAgent.includes("Windows");

  const selected = sources.find((s) => target && keyOf(s.target) === keyOf(target)) ?? null;
  const canAppAudio = isWindows && selected?.pid != null;
  const output = settings?.loopback_device
    ? outputs.find((d) => d.id === settings.loopback_device)?.name ?? settings.loopback_device
    : outputs.find((d) => d.is_default)?.name ?? "System default output";

  useEffect(() => {
    return subscribe("detection", (d) => {
      setDet(d);
      setLeft(30);
    });
  }, []);

  // The popup window survives being hidden: reload preferences for every picker.
  useEffect(() => {
    if (!det) return;
    let cancelled = false;
    setSettings(null);
    setSources([]);
    setLoadingSources(true);
    setTarget(null);
    setError(null);
    void cmd.getSettings().then((s) => {
      if (cancelled) return;
      setSettings(s);
      setVideo(s.video_enabled);
      setAudioScope(s.audio_scope);
    }).catch((e) => { if (!cancelled) setError(`Could not load recording settings: ${String(e)}`); });
    void cmd.listAudioDevices().then((d) => { if (!cancelled) setOutputs(d.outputs); }).catch(() => {});
    void cmd.listVideoSources(det.app === "unknown" || det.app === "manual" ? null : det.app).then((s) => {
      if (cancelled) return;
      setSources(s);
      // Preserve the existing preference for the meeting window, then another window.
      setTarget(s.find((x) => x.is_meeting)?.target ?? s.find((x) => x.target.kind === "window")?.target ?? s[0]?.target ?? null);
    }).catch((e) => { if (!cancelled) setError(`Could not list screens: ${String(e)}`); })
      .finally(() => { if (!cancelled) setLoadingSources(false); });
    return () => { cancelled = true; };
  }, [det]);

  // Auto-hide countdown (detections only; a manual picker waits for you).
  useEffect(() => {
    if (!det || manual) return;
    const t = setInterval(() => setLeft((n) => n - 1), 1000);
    return () => clearInterval(t);
  }, [det, manual]);
  useEffect(() => {
    if (det && !manual && left <= 0) void cmd.dismissDetection(det.app, "now");
  }, [left, det, manual]);

  const chooseAudio = async (scope: AudioScope) => {
    const previous = audioScope;
    setAudioScope(scope);
    setSaving(true);
    setError(null);
    try {
      // Read the latest settings so another window's changes are not overwritten.
      const current = await cmd.getSettings();
      setSettings(await cmd.setSettings({ ...current, audio_scope: scope }));
    } catch (e) {
      setAudioScope(previous);
      setError(`Could not save audio choice: ${String(e)}`);
    } finally {
      setSaving(false);
    }
  };

  const record = async () => {
    if (!det || !settings || saving || loadingSources || startPending.current) return;
    startPending.current = true;
    setStarting(true);
    setError(null);
    const app = det.app === "unknown" || det.app === "manual" ? undefined : det.app;
    // The picked window always drives the audio; video only when screen capture is on.
    const audio_pid = settings.capture_system && canAppAudio && audioScope === "app" && selected?.pid != null ? selected.pid : undefined;
    try {
      await cmd.startRecording({ app, video, target: video && target ? target : undefined, audio_pid });
    } catch (e) {
      setError(String(e));
    } finally {
      startPending.current = false;
      setStarting(false);
    }
  };

  // Escape = "Not now"; Enter = Record. Include audio state to avoid stale choices.
  useEffect(() => {
    if (!det) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") void cmd.dismissDetection(det.app, "now");
      else if (e.key === "Enter" && !(e.target instanceof HTMLSelectElement) && !(e.target instanceof HTMLButtonElement)) {
        e.preventDefault();
        void record();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [det, video, target, canAppAudio, audioScope, settings, saving, loadingSources]);

  if (!det) return <div className="h-full" />;

  return (
    <div role="dialog" aria-label={`${det.label} detected. Record?`} className="flex h-full flex-col overflow-y-auto rounded-xl border border-line bg-canvas p-4 shadow-float" data-tauri-drag-region>
      <div className="flex items-start justify-between gap-3" data-tauri-drag-region>
        <div className="min-w-0" data-tauri-drag-region>
          <div className="flex items-center gap-2 text-[14px] font-semibold text-ink">
            <span className={`h-2 w-2 rounded-full ${manual ? "bg-ember" : "bg-moss"}`} />
            {manual ? det.label : `${det.label} detected`}
          </div>
          {det.event && <div className="mt-0.5 truncate text-[12px] font-medium text-ink-2" title="From your calendar">{det.event}</div>}
          <div className="mt-0.5 truncate text-[12px] text-ink-3" title={det.title}>{det.title}</div>
        </div>
        <button
          aria-pressed={video}
          aria-label="Record screen video"
          onClick={() => setVideo((v) => !v)}
          className={`focus-ring inline-flex h-7 items-center gap-1.5 rounded-full px-2.5 text-[11px] font-medium ${video ? "bg-ink text-canvas" : "bg-canvas-3 text-ink-2"}`}
          title="Toggle screen video"
        >
          {video ? <Video size={12} /> : <VideoOff size={12} />} {video ? "Screen on" : "Audio only"}
        </button>
      </div>
      {sources.length > 0 && (
        <label className="mt-2 flex items-center gap-2 text-[11px] text-ink-3">
          <span className="shrink-0">{video ? "Screen" : "Window"}</span>
          <select
            aria-label="Screen to record"
            value={target ? keyOf(target) : ""}
            onChange={(e) => setTarget(sources.find((s) => keyOf(s.target) === e.target.value)?.target ?? null)}
            className="focus-ring h-6 min-w-0 flex-1 truncate rounded border border-line-2 bg-canvas px-1.5 text-[11px] text-ink"
          >
            {sources.map((s) => (
              <option key={keyOf(s.target)} value={keyOf(s.target)}>{s.target.kind === "monitor" ? "Display: " : "Window: "}{s.label}</option>
            ))}
          </select>
        </label>
      )}
      <div className="mt-1.5 flex flex-wrap items-center gap-x-3 gap-y-1 text-[11px] text-ink-3" role="radiogroup" aria-label="Which audio to capture">
        <span className="shrink-0">Audio</span>
        <label className={`inline-flex items-center gap-1 ${canAppAudio ? "" : "opacity-50"}`} title={canAppAudio ? "Only what this app plays - no music or notifications from other apps" : isWindows ? "Pick a window to capture just that app" : "Per-app audio needs Windows; all system audio is recorded"}>
          <input type="radio" name="audio" checked={canAppAudio && audioScope === "app"} disabled={!settings?.capture_system || !canAppAudio || saving || starting} onChange={() => void chooseAudio("app")} /> This app only
        </label>
        <label className="inline-flex items-center gap-1">
          <input type="radio" name="audio" checked={!canAppAudio || audioScope === "all"} disabled={!settings?.capture_system || saving || starting} onChange={() => void chooseAudio("all")} /> All apps on output
        </label>
        <span className="ml-auto">+ your mic</span>
      </div>
      {settings && <p className="mt-1 text-[10px] leading-4 text-ink-3">
        {settings.capture_system
          ? audioScope === "app" && canAppAudio
            ? `System audio: ${selected?.label}. Microphone: ${settings.mic_device ?? "system default"}.`
            : `Output: ${output}. Audio on other outputs is not included. Microphone: ${settings.mic_device ?? "system default"}.`
          : "Microphone only. System audio is turned off in Settings."}
      </p>}
      {error && <p role="alert" className="mt-1 text-[11px] text-ember">{error}</p>}
      {settings?.stt_provider === "soniox" && <p className="mt-1 text-[10px] leading-4 text-ink-3">Soniox cloud transcription: recorded audio is sent to Soniox. Local media keeps recording if the service fails.</p>}
      <div className="mt-auto flex shrink-0 items-center gap-2 pt-2">
        <Button variant="ember" size="md" className="flex-1" disabled={!settings || saving || starting || loadingSources} onClick={() => void record()}>
          <Circle size={12} fill="currentColor" /> {starting ? "Starting…" : "Record"}
        </Button>
        <Button variant="ghost" size="md" onClick={() => void cmd.dismissDetection(det.app, "now")}>
          {manual ? "Cancel" : <>Not now <span className="ml-1 font-mono text-[11px] text-ink-3">{left}</span></>}
        </Button>
        {!manual && (
          <Button variant="ghost" size="md" onClick={() => void cmd.dismissDetection(det.app, "never")} title="Stop asking for this app">
            Never
          </Button>
        )}
      </div>
    </div>
  );
}
