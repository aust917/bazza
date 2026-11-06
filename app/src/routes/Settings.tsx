import { LanguageSelect } from "@/components/LanguageSelect";
import { displayError } from "@/lib/i18n";
import { useTranslation } from "react-i18next";
import { open, save } from "@tauri-apps/plugin-dialog";
import { openPath } from "@tauri-apps/plugin-opener";
import { Download, Trash2, X } from "lucide-react";
import { useCallback, useEffect, useState, useSyncExternalStore } from "react";
import { useInRouterContext, useLocation } from "react-router-dom";
import { TemplatesEditor } from "@/components/TemplatesEditor";
import { DiscordSettings } from "@/components/DiscordSettings";
import { AttendeeSettings } from "@/components/AttendeeSettings";
import { ModelDownloadFailure, ModelDownloadProgress } from "@/components/ModelDownloadProgress";
import { SubscriptionStatuses } from "@/components/SubscriptionProviders";
import { Button, Card, Input, Meter, SectionTitle, Select, Spinner, Toggle } from "@/components/ui";
import { OperationProgress } from "@/components/ui/OperationProgress";
import { appLabel, fmtBytes } from "@/lib/format";
import { LANGUAGES } from "@/lib/languages";
import { useAudioLevels } from "@/lib/useAudioLevels";
import { useModelDownloads } from "@/lib/useModelDownloads";
import { cmd, type AudioDevice, type Hardware, type ModelRow, type Settings, type ShareInfo, type Tier } from "@/lib/ipc";



type Feedback = { error: unknown } | {
  key: "settings.saved" | "settings.keySaved" | "settings.testing" | "settings.endpointFound"
    | "settings.endpointMissing" | "settings.webhookOk" | "settings.copying" | "settings.copied"
    | "settings.exporting" | "settings.exportResult" | "library.importing" | "settings.importResult";
  values?: Record<string, string | number>;
};

type DataOperation = {
  message: Feedback;
  startedAt: number;
  finishedAt?: number;
  succeeded?: boolean;
};
let dataOperation: DataOperation | null = null;
const dataOperationListeners = new Set<() => void>();
const readDataOperation = () => dataOperation;
const subscribeDataOperation = (listener: () => void) => {
  dataOperationListeners.add(listener);
  return () => { dataOperationListeners.delete(listener); };
};

async function runDataOperation(message: Feedback, run: () => Promise<{ message: Feedback; succeeded?: boolean }>) {
  if (dataOperation && dataOperation.finishedAt === undefined) return;
  const startedAt = performance.now();
  dataOperation = { message, startedAt };
  dataOperationListeners.forEach((listener) => listener());
  try {
    const result = await run();
    dataOperation = { ...result, startedAt, finishedAt: performance.now(), succeeded: result.succeeded ?? true };
  } catch (error) {
    dataOperation = { message: { error }, startedAt, finishedAt: performance.now(), succeeded: false };
  }
  dataOperationListeners.forEach((listener) => listener());
}

function FocusIntegrationSection() {
  const { hash, key } = useLocation();
  useEffect(() => {
    if (hash !== "#discord" && hash !== "#google-meet") return;
    const section = document.getElementById(hash.slice(1));
    section?.focus({ preventScroll: true });
    section?.scrollIntoView?.({ block: "start" });
  }, [hash, key]);
  return null;
}

export function SettingsPage({ serviceDesktop = false }: { serviceDesktop?: boolean } = {}) {
  const { t: tr, i18n } = useTranslation("main");
  const routed = useInRouterContext();
  const TIER_LABEL: Record<Tier, string> = { cpu_low: tr("settings.tierCpuLow"), cpu_high: tr("settings.tierCpuHigh"), gpu: tr("settings.tierGpu") };
  const ROLE_LABEL: Record<string, string> = { live: tr("roles.live"), quality: tr("roles.quality"), llm: tr("roles.llm"), speakers: tr("roles.speakers"), search: tr("roles.search") };
  const MODEL_NOTES: Record<string, string> = {
    "whisper-base": tr("modelNotes.whisperBase"),
    "whisper-small": tr("modelNotes.whisperSmall"),
    "whisper-medium": tr("modelNotes.whisperMedium"),
    "whisper-large-v3-turbo": tr("modelNotes.whisperTurbo"),
    "qwen3-1.7b-q4": tr("modelNotes.qwenSmall"),
    "qwen3-4b-q4": tr("modelNotes.qwenMedium"),
    "qwen3-8b-q4": tr("modelNotes.qwenLarge"),
    "pyannote-seg-3": tr("modelNotes.segmentation"),
    "nemotron3-q8": tr("modelNotes.nemotron"),
    "titanet-small": tr("modelNotes.voiceprint"),
    "bge-small-en-v1.5-q8": tr("modelNotes.search"),
  };
  const MODEL_NAMES: Record<string, string> = { "nemotron3-q8": tr("modelNames.nemotron"), "pyannote-seg-3": tr("modelNames.segmentation"), "titanet-small": tr("modelNames.voiceprint") };
  const languageNames = new Intl.DisplayNames([i18n.resolvedLanguage ?? "ru"], { type: "language" });
  const [s, setS] = useState<Settings | null>(null);
  const [devices, setDevices] = useState<{ inputs: AudioDevice[]; outputs: AudioDevice[] }>({ inputs: [], outputs: [] });
  const levels = useAudioLevels({ enabled: !serviceDesktop && s !== null, mic: s?.mic_device, loopback: s?.loopback_device, onlyWhenIdle: true });
  const [hw, setHw] = useState<{ hardware: Hardware; tier: Tier; effective_tier: Tier } | null>(null);
  const [calendarDraft, setCalendarDraft] = useState("");
  const [calendarErrors, setCalendarErrors] = useState<string[]>([]);
  const [calendarCount, setCalendarCount] = useState<number | null>(null);
  const [webhookMsg, setWebhookMsg] = useState<Feedback | null>(null);
  const [data, setData] = useState<{ data_dir: string; recordings_bytes: number; models_bytes: number } | null>(null);
  const [hotkeyDraft, setHotkeyDraft] = useState("");
  const [saved, setSaved] = useState<Feedback | null>(null);
  const [endpointTest, setEndpointTest] = useState<Feedback | null>(null);
  const dataWork = useSyncExternalStore(subscribeDataOperation, readDataOperation);
  const dataBusy = dataWork !== null && dataWork.finishedAt === undefined;
  const [shares, setShares] = useState<ShareInfo[]>([]);
  const [sonioxKey, setSonioxKey] = useState("");
  const [sonioxKeySet, setSonioxKeySet] = useState(false);
  const [sonioxModel, setSonioxModel] = useState("");
  const [showSubscriptionStatuses, setShowSubscriptionStatuses] = useState(false);

  const feedbackText = (message: Feedback | null) => !message ? "" : "error" in message ? displayError(message.error) : tr(message.key, message.values);

  const refreshModelStorage = useCallback(() => {
    if (!serviceDesktop) void cmd.dataInfo().then(setData).catch(() => {});
  }, [serviceDesktop]);
  const { models, reloadModels, getDownload, download } = useModelDownloads(refreshModelStorage);

  useEffect(() => {
    void cmd.getSettings().then((x) => {
      setS(x);
      setHotkeyDraft(x.hotkey);
      setCalendarDraft(x.calendar_sources.join("\n"));
      setSonioxModel(x.soniox_model);
    }).catch((error) => setSaved({ error }));
    void cmd.listAudioDevices().then(setDevices);
    void cmd.probeHardware().then(setHw);
    if (!serviceDesktop) {
      void cmd.dataInfo().then(setData);
      void cmd.listShares().then(setShares);
    }
    void cmd.sonioxKeyStatus().then(setSonioxKeySet).catch((error) => setSaved({ error }));
  }, [serviceDesktop]);

  const update = async (patch: Partial<Settings>) => {
    if (!s) return;
    const next = { ...s, ...patch };
    setS(next);
    try {
      setS(await cmd.setSettings(next));
      setSaved({ key: "settings.saved" });
      setTimeout(() => setSaved(null), 1200);
      if (patch.tier_override !== undefined || patch.live_asr_model !== undefined || patch.quality_asr_model !== undefined || patch.llm_model !== undefined) {
        void reloadModels().catch(() => {});
        void cmd.probeHardware().then(setHw);
      }
    } catch (e) {
      setS(s);
      if (patch.soniox_model !== undefined) setSonioxModel(s.soniox_model);
      setSaved({ error: e });
    }
  };

  if (!s) return <div className="p-8">{saved && "error" in saved ? <p role="alert">{feedbackText(saved)}</p> : <Spinner />}</div>;

  const asr = models.filter((m) => m.spec.kind === "asr");
  const llm = models.filter((m) => m.spec.kind === "llm");

  return (
    <div className="mx-auto max-w-3xl px-8 py-8">
      {routed && <FocusIntegrationSection />}
      <header className="mb-8 flex items-end justify-between">
        <div>
          <h1 className="font-sans font-semibold text-[34px] leading-none tracking-tight">{tr("nav.settings")}</h1>
          <p className="mt-1.5 text-[13px] text-ink-3">{tr("settings.description")}</p>
        </div>
        <span className="text-[12px] text-moss">{feedbackText(saved)}</span>
      </header>

      {!serviceDesktop && <section className="mb-10"><LanguageSelect /></section>}
      {serviceDesktop && (
        <>
          <McpAccessSection
            title={tr("settings.codexMcp")}
            description={tr("settings.codexMcpDescription")}
            hint={tr("settings.codexMcpHint")}
            getStatus={cmd.getCodexMcpStatus}
            setEnabled={setCodexAccess}
            clients={[{ id: "codex", enabled: (status) => status.enabled }]}
          />
          <McpAccessSection
            title={tr("settings.claudeMcp")}
            description={tr("settings.claudeMcpDescription")}
            hint={tr("settings.claudeMcpHint")}
            getStatus={cmd.getClaudeMcpStatus}
            setEnabled={cmd.setClaudeMcpEnabled}
            clients={[
              { id: "code", label: "Claude Code", enabled: (status) => status.code_enabled },
              { id: "desktop", label: "Claude Desktop", enabled: (status) => status.desktop_enabled },
            ]}
          />
        </>
      )}

      <section className="mb-10">
        <SectionTitle>{tr("settings.audio")}</SectionTitle>
        <Card className="divide-y divide-line px-5">
          <div className="grid grid-cols-2 gap-4 py-4">
            <label className="block">
              <span className="mb-1 block text-[12px] font-medium text-ink-2">{tr("settings.microphone")}</span>
              <Select value={s.mic_device ?? ""} onChange={(e) => void update({ mic_device: e.target.value || null })}>
                <option value="">{tr("settings.systemDefault")}</option>
                {devices.inputs.map((d) => (
                  <option key={d.id} value={d.id}>{d.name}</option>
                ))}
              </Select>
              {!serviceDesktop && <Meter db={levels[0]} className="mt-2" />}
            </label>
            <label className="block">
              <span className="mb-1 block text-[12px] font-medium text-ink-2">{tr("settings.systemAudio")}</span>
              <Select value={s.loopback_device ?? ""} onChange={(e) => void update({ loopback_device: e.target.value || null })}>
                <option value="">{tr("settings.defaultOutput")}</option>
                {devices.outputs.map((d) => (
                  <option key={d.id} value={d.id}>{d.name}</option>
                ))}
              </Select>
              {!serviceDesktop && <Meter db={levels[1]} className="mt-2" />}
            </label>
          </div>
          <Toggle label={tr("settings.captureSystem")} description={tr("settings.captureSystemHint")} checked={s.capture_system} onChange={(v) => void update({ capture_system: v })} />
          <div className="py-4">
            <span className="mb-1 block text-[12px] font-medium text-ink-2">{tr("settings.userName")}</span>
            <Input value={s.user_name} onChange={(e) => setS({ ...s, user_name: e.target.value })} onBlur={() => void update({ user_name: s.user_name })} className="max-w-xs" />
          </div>
          <div className="py-4">
            <span className="mb-1 block text-[12px] font-medium text-ink-2">{tr("settings.spokenLanguage")}</span>
            <Select value={s.language ?? ""} onChange={(e) => void update({ language: e.target.value || null })} className="max-w-xs">
              {LANGUAGES.map((l) => <option key={l.code ?? "auto"} value={l.code ?? ""}>{l.code ? languageNames.of(l.code) ?? l.name : tr("settings.autoDetect")}</option>)}
            </Select>
            <span className="mt-1 block text-[12px] text-ink-3">{tr("settings.languageHint")}</span>
          </div>
        </Card>
      </section>

      <section className="mb-10">
        <SectionTitle>{tr("settings.video")}</SectionTitle>
        <Card className="divide-y divide-line px-5">
          <Toggle label={tr("settings.videoDefault")} description={tr("settings.videoDefaultHint")} checked={s.video_enabled} onChange={(v) => void update({ video_enabled: v })} />
          <div className="grid grid-cols-2 gap-4 py-4">
            <label className="block">
              <span className="mb-1 block text-[12px] font-medium text-ink-2">{tr("settings.fps")}</span>
              <Select value={s.video_fps} onChange={(e) => void update({ video_fps: Number(e.target.value) })}>
                {![5, 10, 15, 24, 30, 60].includes(s.video_fps) && <option value={s.video_fps}>{tr("settings.fpsCurrent", { count: s.video_fps })}</option>}
                {[5, 10, 15, 24, 30, 60].map((fps) => <option key={fps} value={fps}>{tr("settings.fpsValue", { count: fps })}{fps === 15 ? tr("settings.defaultSuffix") : ""}</option>)}
              </Select>
            </label>
            <label className="block">
              <span className="mb-1 block text-[12px] font-medium text-ink-2">{tr("settings.height")}</span>
              <Select value={s.video_max_height} onChange={(e) => void update({ video_max_height: Number(e.target.value) })}>
                {![480, 720, 1080, 1440, 2160, 4320].includes(s.video_max_height) && <option value={s.video_max_height}>{tr("settings.heightCurrent", { count: s.video_max_height })}</option>}
                {[480, 720, 1080, 1440, 2160, 4320].map((height) => <option key={height} value={height}>{tr("settings.heightValue", { count: height })}{height === 1080 ? tr("settings.defaultSuffix") : ""}</option>)}
              </Select>
            </label>
          </div>
          <p className="py-4 text-[12px] text-ink-3">{tr("settings.videoHint")}</p>
        </Card>
      </section>

      {!serviceDesktop && <section className="mb-10">
        <SectionTitle>{tr("settings.detection")}</SectionTitle>
        <Card className="divide-y divide-line px-5">
          <Toggle label={tr("settings.detect")} description={tr("settings.detectHint")} checked={s.detection_enabled} onChange={(v) => void update({ detection_enabled: v })} />
          <Toggle label={tr("settings.audioActivity")} description={tr("settings.audioActivityHint")} checked={s.audio_activity_enabled} onChange={(v) => void update({ audio_activity_enabled: v })} />
          <div className="py-4">
            <span className="mb-1 block text-[12px] font-medium text-ink-2">{tr("settings.hotkey")}</span>
            <form
              className="flex max-w-md gap-2"
              onSubmit={(e) => {
                e.preventDefault();
                void update({ hotkey: hotkeyDraft });
              }}
            >
              <Input value={hotkeyDraft} onChange={(e) => setHotkeyDraft(e.target.value)} placeholder="CmdOrCtrl+Shift+R" className="font-mono" />
              <Button type="submit" variant="outline" size="md">{tr("settings.apply")}</Button>
            </form>
          </div>
          {s.never_apps.length > 0 && (
            <div className="py-4">
              <span className="mb-2 block text-[12px] font-medium text-ink-2">{tr("settings.neverAsk")}</span>
              <div className="flex flex-wrap gap-2">
                {s.never_apps.map((a) => (
                  <button key={a} onClick={() => void update({ never_apps: s.never_apps.filter((x) => x !== a) })} className="inline-flex items-center gap-1 rounded-full bg-canvas-3 px-2.5 py-1 text-[12px] hover:bg-line-2">
                    {appLabel(a)} <X size={12} />
                  </button>
                ))}
              </div>
            </div>
          )}
          <Toggle label={tr("settings.closeTray")} checked={s.close_to_tray} onChange={(v) => void update({ close_to_tray: v })} />
        </Card>
      </section>}

      <DiscordSettings />
      <AttendeeSettings />

      <section className="mb-10">
        <SectionTitle>{tr("settings.transcription")}</SectionTitle>
        <Card className="divide-y divide-line px-5">
          <label className="block py-4">
            <span className="mb-1 block text-[12px] font-medium text-ink-2">{tr("settings.provider")}</span>
            <Select aria-label={tr("settings.provider")} value={s.stt_provider} onChange={(e) => void update({ stt_provider: e.target.value as Settings["stt_provider"] })} className="max-w-sm">
              <option value="local">{tr("settings.localProvider")}</option>
              <option value="soniox" disabled={!s.soniox_consent || !sonioxKeySet}>{tr("settings.sonioxProvider")}</option>
            </Select>
            <p className="mt-1 text-[12px] text-ink-3">{tr("settings.providerHint")}</p>
          </label>
          <Toggle label={tr("settings.sonioxConsent")} description={tr("settings.sonioxConsentHint")} checked={s.soniox_consent} onChange={(v) => void update({ soniox_consent: v, ...(!v ? { stt_provider: "local" as const } : {}) })} />
          <div className="py-4">
            <label className="mb-1 block text-[12px] font-medium text-ink-2" htmlFor="soniox-key">{tr("settings.sonioxKey")}</label>
            <div className="flex gap-2">
              <Input id="soniox-key" type="password" autoComplete="off" value={sonioxKey} onChange={(e) => setSonioxKey(e.target.value)} placeholder={sonioxKeySet ? tr("settings.keySavedHint") : tr("settings.keyPlaceholder")} />
              <Button variant="outline" size="md" disabled={!sonioxKey.trim()} onClick={() => void cmd.saveSonioxKey(sonioxKey).then((exists) => { setSonioxKeySet(exists); setSonioxKey(""); setSaved({ key: "settings.keySaved" }); }).catch((e) => setSaved({ error: e }))}>{tr("settings.saveKey")}</Button>
              {sonioxKeySet && <Button variant="ghost" size="md" onClick={() => void cmd.saveSonioxKey(null).then(() => { setSonioxKeySet(false); setSonioxKey(""); }).catch((e) => setSaved({ error: e }))}>{tr("settings.clear")}</Button>}
            </div>
            <p className="mt-1 text-[12px] text-ink-3">{tr("settings.keyPrivacy")}</p>
          </div>
          <label className="block py-4">
            <span className="mb-1 block text-[12px] font-medium text-ink-2">{tr("settings.sonioxModel")}</span>
            <Input value={sonioxModel} onChange={(e) => setSonioxModel(e.target.value)} onBlur={() => void update({ soniox_model: sonioxModel })} className="max-w-sm" />
            <p className="mt-1 text-[12px] text-ink-3">{tr("settings.sonioxHint")}</p>
          </label>
        </Card>
      </section>

      <section className="mb-10">
        <SectionTitle hint={hw ? tr("settings.hardware", { cpu: hw.hardware.cpu_name || "CPU", ram: Math.round(hw.hardware.ram_gb), gpu: hw.hardware.gpu ? tr("settings.hardwareGpu", { gpu: hw.hardware.gpu.name, vram: Math.round(hw.hardware.gpu.vram_gb) }) : "" }) : ""}>
          {tr("settings.aiModels")}
        </SectionTitle>
        <Card className="px-5">
          <div className="grid grid-cols-[repeat(auto-fit,minmax(min(100%,18rem),1fr))] gap-4 py-4">
            <label className="block min-w-0">
              <span className="mb-1 block text-[12px] font-medium text-ink-2">{tr("settings.tier")}</span>
              <Select value={s.tier_override ?? ""} onChange={(e) => void update({ tier_override: (e.target.value || null) as Tier | null })}>
                <option value="">{tr("settings.auto")}{hw ? ` (${TIER_LABEL[hw.tier]})` : ""}</option>
                {(["cpu_low", "cpu_high", "gpu"] as Tier[]).map((t) => (
                  <option key={t} value={t}>{TIER_LABEL[t]}</option>
                ))}
              </Select>
            </label>
            <div className="grid min-w-0 gap-3">
              <ModelPick label={tr("settings.liveCaptions")} value={s.live_asr_model} options={asr} onChange={(v) => void update({ live_asr_model: v })} />
              <ModelPick label={tr("settings.finalTranscript")} value={s.quality_asr_model} options={asr} onChange={(v) => void update({ quality_asr_model: v })} />
              <ModelPick label={tr("settings.summariesChat")} value={s.llm_model} options={llm} onChange={(v) => void update({ llm_model: v })} />
            </div>
          </div>
          <div className="space-y-1 border-t border-line py-4">
            <h3 className="text-[13px] font-semibold">{tr("settings.subscriptionSummary")}</h3>
            <p className="text-[13px] text-ink-2">{tr("settings.subscriptionSummaryHint")}</p>
            <p className="text-[12px] text-ink-3">{tr(serviceDesktop ? "settings.serviceSummaryPrivacy" : "settings.subscriptionSummaryPrivacy")}</p>
            <details className="pt-1" onToggle={(event) => setShowSubscriptionStatuses(event.currentTarget.open)}>
              <summary className="focus-ring w-fit cursor-pointer rounded text-[13px] font-medium">{tr("settings.subscriptionCli")}</summary>
              {showSubscriptionStatuses && <SubscriptionStatuses />}
            </details>
          </div>
          <ul className="divide-y divide-line border-t border-line">
            {models.map((m) => {
              const progress = getDownload(m);
              const name = MODEL_NAMES[m.spec.id] ?? m.spec.name;
              return (
                <li key={m.spec.id} className="flex items-center gap-4 py-3">
                  <div className="min-w-0 flex-1">
                    <div className="flex items-center gap-2 text-[14px] font-medium">
                      {name}
                      {m.roles.map((r) => (
                        <span key={r} className="rounded bg-canvas-3 px-1.5 py-px text-[10px] font-semibold tracking-wide text-ink-2 uppercase">{ROLE_LABEL[r] ?? r}</span>
                      ))}
                    </div>
                    <div className="text-[12px] text-ink-3">{MODEL_NOTES[m.spec.id] ?? m.spec.note} {fmtBytes(m.spec.size_bytes)}</div>
                    {progress?.status === "downloading" && <ModelDownloadProgress modelName={name} {...progress} />}
                    {progress?.status === "failed" && <ModelDownloadFailure error={progress.error} />}
                  </div>
                  {m.present ? (
                    <Button variant="ghost" size="sm" onClick={() => cmd.removeModel(m.spec.id).then(reloadModels)} title={tr("settings.deleteDisk")}>
                      <Trash2 size={14} /> {tr("settings.remove")}
                    </Button>
                  ) : progress?.status === "downloading" ? (
                    <Button variant="ghost" size="sm" onClick={() => void cmd.cancelDownload(m.spec.id)}>
                      <X size={14} /> {tr("modelDownload.cancel")}
                    </Button>
                  ) : (
                    <Button variant="outline" size="sm" onClick={() => void download(m)}>
                      <Download size={14} /> {tr(progress?.status === "failed" ? "modelDownload.retry" : "actions.download")}
                    </Button>
                  )}
                </li>
              );
            })}
          </ul>
        </Card>
      </section>

      <section className="mb-10">
        <SectionTitle>{tr("settings.assistant")}</SectionTitle>
        <Card className="divide-y divide-line px-5">
          {!serviceDesktop && <div className="py-4">
            <span className="mb-2 block text-[12px] font-medium text-ink-2">{tr("settings.llmRuns")}</span>
            <div className="grid grid-cols-2 gap-2">
              {([
                ["bundled", tr("settings.bundled"), tr("settings.bundledHint")],
                ["openai", tr("settings.endpoint"), tr("settings.endpointHint")],
              ] as const).map(([v, title, body]) => (
                <button
                  key={v}
                  onClick={() => void update({ llm_backend: v })}
                  className={`focus-ring rounded-lg border p-3 text-left transition-colors ${s.llm_backend === v ? "border-ink bg-canvas shadow-card" : "border-line hover:bg-canvas-2"}`}
                >
                  <div className="text-[13px] font-semibold">{title}</div>
                  <div className="text-[12px] text-ink-3">{body}</div>
                </button>
              ))}
            </div>
            {s.llm_backend === "openai" && (
              <div className="mt-3 grid grid-cols-[1fr_1fr_auto] gap-2">
                <Input value={s.llm_endpoint} placeholder="http://localhost:11434/v1" onChange={(e) => setS({ ...s, llm_endpoint: e.target.value })} onBlur={() => void update({ llm_endpoint: s.llm_endpoint })} className="font-mono text-[12px]" />
                <Input value={s.llm_endpoint_model} placeholder={tr("settings.modelPlaceholder")} onChange={(e) => setS({ ...s, llm_endpoint_model: e.target.value })} onBlur={() => void update({ llm_endpoint_model: s.llm_endpoint_model })} className="font-mono text-[12px]" />
                <Button
                  variant="outline"
                  size="md"
                  onClick={() => {
                    setEndpointTest({ key: "settings.testing" });
                    cmd.testLlmEndpoint(s.llm_endpoint, s.llm_api_key, s.llm_endpoint_model).then(
                      (models) => setEndpointTest(models.includes(s.llm_endpoint_model)
                        ? { key: "settings.endpointFound", values: { count: models.length, model: s.llm_endpoint_model } }
                        : { key: "settings.endpointMissing", values: { model: s.llm_endpoint_model, models: models.slice(0, 6).join(", ") } }),
                      (e) => setEndpointTest({ error: e }),
                    );
                  }}
                >
                  {tr("settings.test")}
                </Button>
                <Input value={s.llm_api_key ?? ""} type="password" placeholder={tr("settings.apiKey")} onChange={(e) => setS({ ...s, llm_api_key: e.target.value || null })} onBlur={() => void update({ llm_api_key: s.llm_api_key })} className="col-span-2 font-mono text-[12px]" />
                {endpointTest && <div className="col-span-3 text-[12px] text-ink-2">{feedbackText(endpointTest)}</div>}
              </div>
            )}
          </div>}
          <Toggle label={tr("settings.identifySpeakers")} description={tr("settings.identifyHint")} checked={s.diarize_enabled} onChange={(v) => void update({ diarize_enabled: v })} />
          <Toggle label={tr("settings.cleanup")} description={tr("settings.cleanupHint")} checked={s.cleanup_enabled} onChange={(v) => void update({ cleanup_enabled: v })} />
          <Toggle label={tr("settings.summarize")} description={tr("settings.summarizeHint")} checked={s.summary_enabled} onChange={(v) => void update({ summary_enabled: v })} />
        </Card>
      </section>

      {!serviceDesktop && <>
      <section className="mb-10">
        <SectionTitle hint={tr("settings.templatesHint")}>{tr("settings.templates")}</SectionTitle>
        <Card className="p-5">
          <TemplatesEditor defaultId={s.default_template_id} onDefaultChange={(id) => void update({ default_template_id: id })} />
        </Card>
      </section>

      <section className="mb-10">
        <SectionTitle hint={tr("settings.calendarHint")}>{tr("settings.calendar")}</SectionTitle>
        <Card className="px-5 py-4 text-[13px]">
          <p className="text-ink-3">
            {tr("settings.calendarHelp", { count: s.calendar_refresh_min })}
          </p>
          <textarea
            value={calendarDraft}
            onChange={(e) => setCalendarDraft(e.target.value)}
            onBlur={() => {
              const list = calendarDraft.split("\n").map((l) => l.trim()).filter(Boolean);
              if (JSON.stringify(list) !== JSON.stringify(s.calendar_sources)) void update({ calendar_sources: list }).then(() => cmd.refreshCalendar().then(setCalendarErrors));
            }}
            placeholder={"https://calendar.google.com/calendar/ical/.../private-.../basic.ics\nC:\\Users\\me\\work.ics"}
            rows={3}
            spellCheck={false}
            className="focus-ring mt-3 w-full rounded-md border border-line-2 bg-canvas px-3 py-2 font-mono text-[12px] text-ink placeholder:text-ink-3"
          />
          <div className="mt-2 flex items-center gap-3">
            <Button variant="outline" size="sm" onClick={() => void cmd.refreshCalendar().then((e) => { setCalendarErrors(e); void cmd.listUpcoming(24 * 7).then((ev) => setCalendarCount(ev.length)); })}>{tr("settings.refresh")}</Button>
            {calendarCount !== null && <span className="text-ink-3">{tr("settings.calendarEvents", { count: calendarCount })}</span>}
          </div>
          {calendarErrors.map((e) => <div key={e} className="mt-2 text-ember">{displayError(e)}</div>)}
        </Card>
      </section>

      <section className="mb-10">
        <SectionTitle hint={tr("settings.webhookHint")}>{tr("settings.webhook")}</SectionTitle>
        <Card className="divide-y divide-line px-5 text-[13px]">
          <Toggle label={tr("settings.webhookSend")} description={tr("settings.webhookSendHint")} checked={s.webhook_enabled} onChange={(v) => void update({ webhook_enabled: v })} />
          <div className="py-4">
            <span className="mb-1 block text-[12px] font-medium text-ink-2">URL</span>
            <div className="flex gap-2">
              <Input value={s.webhook_url} placeholder="https://hooks.zapier.com/hooks/catch/..." onChange={(e) => setS({ ...s, webhook_url: e.target.value })} onBlur={() => void update({ webhook_url: s.webhook_url })} className="font-mono text-[12px]" />
              <Button variant="outline" size="md" className="shrink-0" disabled={!s.webhook_url.trim()} onClick={() => void cmd.testWebhook(s.webhook_url).then((st) => setWebhookMsg({ key: "settings.webhookOk", values: { status: st } }), (e) => setWebhookMsg({ error: e }))}>{tr("settings.sendTest")}</Button>
            </div>
            {webhookMsg && <div className={`mt-1.5 text-[12px] ${"key" in webhookMsg && webhookMsg.key === "settings.webhookOk" ? "text-moss" : "text-ember"}`}>{feedbackText(webhookMsg)}</div>}
          </div>
        </Card>
      </section>

      <section className="mb-10">
        <SectionTitle>{tr("settings.updates")}</SectionTitle>
        <Card className="divide-y divide-line px-5 text-[13px]">
          <p className="py-4 text-ink-3">{tr("settings.updatesHint")}</p>
        </Card>
      </section>

      <section className="mb-10">
        <SectionTitle>{tr("settings.data")}</SectionTitle>
        <Card className="divide-y divide-line px-5 text-[13px]">
          <div className="flex items-center justify-between gap-4 py-4">
            <div className="min-w-0">
              <div className="truncate font-mono text-[12px] text-ink-2">{data?.data_dir}</div>
              <div className="mt-1 text-ink-3">
                {tr("settings.dataSize", { recordings: fmtBytes(data?.recordings_bytes ?? 0), models: fmtBytes(data?.models_bytes ?? 0) })}
              </div>
            </div>
            <div className="flex shrink-0 gap-2">
              <Button variant="outline" size="sm" onClick={() => data && void openPath(data.data_dir)}>{tr("settings.openFolder")}</Button>
              <Button variant="outline" size="sm" disabled={dataBusy} onClick={() => void (async () => {
                const dir = await open({ directory: true, title: tr("settings.dataFolderDialog") });
                if (!dir) return;
                await runDataOperation({ key: "settings.copying" }, async () => {
                  const to = await cmd.changeDataDir(dir as string);
                  return { message: { key: "settings.copied", values: { path: to } } };
                });
              })()}>{tr("settings.changeFolder")}</Button>
            </div>
          </div>
          <div className="flex flex-wrap items-center gap-2 py-4">
            <span className="mr-2 text-ink-2">{tr("settings.moveMeetings")}</span>
            <Button variant="outline" size="sm" disabled={dataBusy} onClick={() => void (async () => {
              const out = await save({ defaultPath: "bazza-library.bazza", filters: [{ name: tr("settings.bundleFilter"), extensions: ["bazza"] }] });
              if (!out) return;
              await runDataOperation({ key: "settings.exporting" }, async () => {
                const n = await cmd.exportMeetings([], out, true);
                return { message: { key: "settings.exportResult", values: { count: n, path: out } } };
              });
            })()}>{tr("settings.exportAll")}</Button>
            <Button variant="outline" size="sm" disabled={dataBusy} onClick={() => void (async () => {
              const p = await open({ filters: [{ name: tr("settings.bundleFilter"), extensions: ["bazza"] }], multiple: false });
              if (!p) return;
              await runDataOperation({ key: "library.importing" }, async () => {
                const r = await cmd.importMeetings(p as string);
                void cmd.dataInfo().then(setData, () => {});
                return {
                  message: { key: "settings.importResult", values: { imported: r.imported, skipped: r.skipped_existing, errors: r.errors.length ? r.errors.join("; ") : "0" } },
                  succeeded: r.errors.length === 0,
                };
              });
            })()}>{tr("settings.importBundle")}</Button>
            {dataWork && (dataBusy || dataWork.succeeded
              ? <OperationProgress announceStage label={feedbackText(dataWork.message)} value={dataWork.succeeded ? 1 : undefined}
                  startedAt={dataWork.startedAt} finishedAt={dataWork.finishedAt} className="basis-full" />
              : <p role="alert" className="basis-full text-[12px] text-ember">{feedbackText(dataWork.message)}</p>)}
          </div>
          <div className="py-4">
            <div className="mb-2 flex items-center justify-between">
              <span className="text-ink-2">{tr("settings.shareLinks", { port: s.share_port })}</span>
              <span className="text-[12px] text-ink-3">{tr("settings.activeLinks", { count: shares.filter((x) => x.share.enabled).length })}</span>
            </div>
            {shares.length === 0 ? (
              <div className="text-[12px] text-ink-3">{tr("settings.noShares")}</div>
            ) : (
              <ul className="flex flex-col gap-1">
                {shares.map((x) => (
                  <li key={x.share.token} className="flex items-center gap-2 text-[12px]">
                    <span className="min-w-0 flex-1 truncate">{x.share.meeting_title} <span className="text-ink-3">({x.share.kind === "clip" ? tr("settings.clipKind") : tr("settings.meetingKind")})</span></span>
                    <span className="truncate font-mono text-ink-3">{x.url}</span>
                    <Button variant="ghost" size="sm" onClick={() => void cmd.setShareEnabled(x.share.token, !x.share.enabled).then(() => cmd.listShares()).then(setShares)}>{x.share.enabled ? tr("settings.disable") : tr("settings.enable")}</Button>
                    <Button variant="danger" size="sm" aria-label={tr("settings.deleteLink", { title: x.share.meeting_title ?? "" })} onClick={() => void cmd.deleteShare(x.share.token).then(() => cmd.listShares()).then(setShares)}><Trash2 size={13} /></Button>
                  </li>
                ))}
              </ul>
            )}
          </div>
        </Card>
      </section>

      </>}
      <p className="text-[12px] text-ink-3">{tr("settings.footer")}</p>
    </div>
  );
}

function setCodexAccess(_client: "codex", enabled: boolean) {
  return cmd.setCodexMcpEnabled(enabled);
}

type McpAccessProps<Status, Client extends string> = {
  title: string;
  description: string;
  hint: string;
  getStatus: () => Promise<Status | null>;
  setEnabled: (client: Client, enabled: boolean) => Promise<Status>;
  clients: { id: Client; label?: string; enabled: (status: Status) => boolean }[];
};

function McpAccessSection<Status, Client extends string>({
  title, description, hint, getStatus, setEnabled, clients,
}: McpAccessProps<Status, Client>) {
  const { t: tr } = useTranslation("main");
  const [status, setStatus] = useState<Status | null>();
  const [error, setError] = useState<unknown>(null);
  const [saving, setSaving] = useState(false);
  const [readAttempt, setReadAttempt] = useState(0);

  useEffect(() => {
    let cancelled = false;
    void getStatus().then(
      (result) => { if (!cancelled) setStatus(result); },
      (failure) => { if (!cancelled) setError(failure); },
    );
    return () => { cancelled = true; };
  }, [getStatus, readAttempt]);

  const changeAccess = async (client: Client, enabled: boolean) => {
    setSaving(true);
    setError(null);
    try {
      setStatus(await setEnabled(client, enabled));
    } catch (failure) {
      setError(failure);
      // A grant can be revoked before removing its client configuration.
      setStatus(await getStatus().catch(() => undefined));
    } finally {
      setSaving(false);
    }
  };

  if (status === null) return null;
  return (
    <section className="mb-10" aria-label={title} aria-busy={saving || (status === undefined && error === null)}>
      <SectionTitle>{title}</SectionTitle>
      <Card className="px-5 py-4">
        <p className="text-sm text-ink-2">{description}</p>
        {status !== undefined && clients.map((client) => {
          const enabled = client.enabled(status);
          return (
            <div key={client.id} role={client.label ? "group" : undefined} aria-label={client.label}
              className="mt-3 flex items-center justify-between gap-4">
              <div>
                {client.label && <p className="text-sm font-medium text-ink">{client.label}</p>}
                <p role="status" className="text-sm text-ink-2">{tr(enabled ? "settings.mcpEnabled" : "settings.mcpDisabled")}</p>
              </div>
              <Button variant="outline" size="md" disabled={saving} onClick={() => void changeAccess(client.id, !enabled)}>
                {tr(enabled ? "settings.mcpDisable" : "settings.mcpEnable")}
              </Button>
            </div>
          );
        })}
        {!status && error === null && <Spinner className="mt-3" />}
        {error !== null && <p role="alert" className="mt-3 text-[13px] text-ember">{displayError(error)}</p>}
        {!status && error !== null && !saving && (
          <Button className="mt-3" variant="outline" size="md" onClick={() => {
            setError(null);
            setReadAttempt((attempt) => attempt + 1);
          }}>{tr("settings.mcpRetry")}</Button>
        )}
        <p className="mt-3 text-[12px] text-ink-3">{hint}</p>
      </Card>
    </section>
  );
}

function ModelPick({ label, value, options, onChange }: { label: string; value: string | null; options: ModelRow[]; onChange: (v: string | null) => void }) {
  const { t: tr } = useTranslation("main");
  return (
    <label className="grid min-w-0 gap-1">
      <span className="text-[12px] font-medium text-ink-2">{label}</span>
      <Select value={value ?? ""} onChange={(e) => onChange(e.target.value || null)} className="min-w-0 truncate text-[13px]">
        <option value="">{tr("settings.tierDefault")}</option>
        {options.map((o) => (
          <option key={o.spec.id} value={o.spec.id}>{o.spec.name}{o.present ? "" : tr("settings.notDownloaded")}</option>
        ))}
      </Select>
    </label>
  );
}
