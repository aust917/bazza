import { LanguageSelect } from "@/components/LanguageSelect";
import { useTranslation } from "react-i18next";
import { ArrowRight, Check, Download } from "lucide-react";
import { useCallback, useEffect, useRef, useState } from "react";
import { useNavigate } from "react-router-dom";
import { ModelDownloadFailure, ModelDownloadProgress } from "@/components/ModelDownloadProgress";
import { Button, Card, Meter, Spinner } from "@/components/ui";
import { fmtBytes } from "@/lib/format";
import { displayError } from "@/lib/i18n";
import { useAudioLevels } from "@/lib/useAudioLevels";
import { useModelDownloads } from "@/lib/useModelDownloads";
import { cmd, type Hardware, type ModelRow, type Settings, type Tier } from "@/lib/ipc";


export function Onboarding({ onDone, serviceDesktop = false }: { onDone?: () => void; serviceDesktop?: boolean }) {
  const { t: tr } = useTranslation("main");
  const { t: meetingTr } = useTranslation("meeting");
  const TIER_COPY: Record<Tier, string> = {
    cpu_low: tr("onboarding.tierCpuLow"),
    cpu_high: tr("onboarding.tierCpuHigh"),
    gpu: tr("onboarding.tierGpu"),
  };
  const MODEL_NAMES: Record<string, string> = { "nemotron3-q8": tr("modelNames.nemotron"), "pyannote-seg-3": tr("modelNames.segmentation"), "titanet-small": tr("modelNames.voiceprint") };
  const navigate = useNavigate();
  const [step, setStep] = useState(serviceDesktop ? 1 : 0);
  const [settings, setSettings] = useState<Settings | null>(null);
  const levels = useAudioLevels({ enabled: !serviceDesktop && step === 0 });
  const [hw, setHw] = useState<{ hardware: Hardware; tier: Tier; effective_tier: Tier } | null>(null);
  const [hardwareError, setHardwareError] = useState<unknown>(null);
  const hardwarePending = useRef(false);
  const { models, reloadModels, getDownload, download } = useModelDownloads();
  const [settingsError, setSettingsError] = useState<unknown>(null);
  const [saving, setSaving] = useState(false);

  const loadHardware = useCallback(() => {
    if (hardwarePending.current) return;
    hardwarePending.current = true;
    setHardwareError(null);
    void cmd.probeHardware().then(setHw).catch((error: unknown) => {
      setHardwareError(error);
    }).finally(() => { hardwarePending.current = false; });
  }, []);

  useEffect(() => {
    void cmd.getSettings().then(setSettings).catch(setSettingsError);
    loadHardware();
  }, [loadHardware]);

  const save = async (finish: boolean) => {
    setSaving(true);
    setSettingsError(null);
    try {
      const current = settings ?? await cmd.getSettings();
      setSettings(await cmd.setSettings(finish ? { ...current, onboarded: true } : current));
      if (finish) {
        onDone?.();
        navigate("/", { replace: true });
      } else {
        await reloadModels();
        setStep(2);
      }
    } catch (error) {
      setSettingsError(error);
    } finally {
      setSaving(false);
    }
  };

  const needed = models.filter((m) => m.roles.length > 0);
  const allPresent = needed.length > 0 && needed.every((m) => m.present);
  // Small models that make recording useful right away; the big ones can finish in the background.
  const isEssential = (m: ModelRow) => m.roles.some((r) => r === "live" || r === "speakers" || r === "search");
  const essentials = needed.filter(isEssential);
  const later = needed.filter((m) => !isEssential(m));
  const essentialsPresent = essentials.length > 0 && essentials.every((m) => m.present);
  const bytes = (xs: ModelRow[]) => xs.filter((m) => !m.present).reduce((n, m) => n + m.spec.size_bytes, 0);
  const isDownloading = (model: ModelRow) => getDownload(model)?.status === "downloading";
  const startAll = (xs: ModelRow[]) => xs.forEach((model) => void download(model));
  const ROLE_LABEL: Record<string, string> = { live: tr("roles.live"), quality: tr("roles.quality"), llm: tr("roles.llm"), speakers: tr("roles.speakers"), search: tr("roles.search") };
  const steps = [tr("onboarding.audio"), tr("onboarding.hardware"), tr("onboarding.models")];

  return (
    <div className="grain flex h-full overflow-y-auto p-8">
      <div className="relative z-10 m-auto w-full max-w-xl">
        {!serviceDesktop && <div className="mb-5"><LanguageSelect /></div>}
        <div className="mb-8 flex items-center gap-3 text-[12px] font-medium text-ink-3">
          {steps.map((label, i) => serviceDesktop && i === 0 ? null : (
            <span key={label} className="flex items-center gap-3">
              <span className={i === step ? "text-ink" : i < step ? "text-moss" : ""}>{i < step ? <Check size={12} className="mr-1 inline" /> : null}{label}</span>
              {i < steps.length - 1 && <span className="h-px w-8 bg-line-2" />}
            </span>
          ))}
        </div>

        {step === 0 && (
          <div className="rise">
            <h1 className="font-sans font-semibold text-[40px] leading-none tracking-tight">{tr("onboarding.title")}</h1>
            <p className="mt-3 text-[15px] text-ink-2">{tr("onboarding.intro")}</p>
            <Card className="mt-6 p-5">
              <div className="grid grid-cols-[auto_minmax(0,1fr)] items-center gap-x-3 gap-y-3 text-[11px] font-medium text-ink-3">
                <span>{tr("onboarding.mic")}</span>
                <Meter db={levels[0]} />
                <span>{tr("onboarding.system")}</span>
                <Meter db={levels[1]} />
              </div>
              <p className="mt-3 text-[12px] text-ink-3">
                {tr("onboarding.audioHint")}
              </p>
            </Card>
            <Button variant="primary" size="lg" className="mt-6" onClick={() => setStep(1)}>
              {tr("actions.continue")} <ArrowRight size={16} />
            </Button>
          </div>
        )}

        {step === 1 && (
          <div className="rise">
            <h1 className="font-sans font-semibold text-[40px] leading-none tracking-tight">{tr("onboarding.machineTitle")}</h1>
            {!hw && (hardwareError != null ? (
              <p role="alert" className="mt-3 text-[15px] text-ember">{displayError(hardwareError)}</p>
            ) : (
              <p role="status" className="mt-3 flex items-center gap-2 text-[15px] text-ink-2"><Spinner /> {tr("library.loading")}</p>
            ))}
            {hw && (
              <>
                <p className="mt-3 text-[15px] text-ink-2">
                  {tr("onboarding.machine", { cpu: hw.hardware.cpu_name || "CPU", ram: Math.round(hw.hardware.ram_gb) })}
                  {hw.hardware.gpu ? tr("onboarding.gpu", { gpu: hw.hardware.gpu.name, vram: Math.round(hw.hardware.gpu.vram_gb) }) : tr("onboarding.noGpu")}.
                </p>
                <div className="mt-6 grid gap-2">
                  {(["gpu", "cpu_high", "cpu_low"] as Tier[]).map((t) => {
                    const selected = (settings?.tier_override ?? hw.tier) === t;
                    return (
                      <button
                        key={t}
                        disabled={saving}
                        onClick={() => settings && setSettings({ ...settings, tier_override: t === hw.tier ? null : t })}
                        className={`focus-ring rounded-lg border p-4 text-left transition-colors ${selected ? "border-ink bg-canvas shadow-card" : "border-line hover:bg-canvas-2"}`}
                      >
                        <div className="flex items-center gap-2 text-[14px] font-semibold">
                          {t === "gpu" ? "GPU" : t === "cpu_high" ? tr("onboarding.cpuRoomy") : tr("onboarding.cpuLight")}
                          {t === hw.tier && <span className="rounded bg-moss-soft px-1.5 text-[10px] font-semibold tracking-wide text-moss uppercase">{tr("onboarding.recommended")}</span>}
                        </div>
                        <div className="text-[13px] text-ink-2">{TIER_COPY[t]}</div>
                      </button>
                    );
                  })}
                </div>
              </>
            )}
            <div className="mt-6 flex flex-wrap gap-2">
              {!serviceDesktop && <Button variant="ghost" size="lg" disabled={saving} onClick={() => setStep(0)}>{tr("actions.back")}</Button>}
              {hw ? (
                <Button
                  variant="primary"
                  size="lg"
                  disabled={saving || (serviceDesktop && settings === null)}
                  onClick={() => void save(false)}
                >
                  {tr("actions.continue")} <ArrowRight size={16} />
                </Button>
              ) : hardwareError != null ? (
                <Button variant="primary" size="lg" onClick={loadHardware}>{meetingTr("actions.retry")}</Button>
              ) : null}
            </div>
          </div>
        )}

        {step === 2 && (
          <div className="rise">
            <h1 className="font-sans font-semibold text-[40px] leading-none tracking-tight">{tr("onboarding.downloadTitle")}</h1>
            <p className="mt-3 text-[15px] text-ink-2">
              {tr("onboarding.downloadHint", { essentials: fmtBytes(bytes(essentials)), later: fmtBytes(bytes(later)) })}
            </p>
            <Card className="mt-6 divide-y divide-line px-5">
              {needed.length === 0 && <div className="py-6"><Spinner /></div>}
              {[...essentials, ...later].map((m) => {
                const progress = getDownload(m);
                const name = MODEL_NAMES[m.spec.id] ?? m.spec.name;
                return (
                  <div key={m.spec.id} className="flex items-center gap-4 py-3">
                    <div className="min-w-0 flex-1">
                      <div className="text-[14px] font-medium">{name} <span className="text-ink-3">{fmtBytes(m.spec.size_bytes)}</span></div>
                      <div className="text-[12px] text-ink-3">{isEssential(m) ? tr("onboarding.essential") : tr("onboarding.later")}{m.roles.map((r) => ROLE_LABEL[r] ?? r).join(", ")}</div>
                      {progress?.status === "downloading" && <ModelDownloadProgress modelName={name} {...progress} />}
                      {progress?.status === "failed" && <ModelDownloadFailure error={progress.error} />}
                    </div>
                    {m.present ? (
                      <span className="inline-flex items-center gap-1 text-[12px] font-medium text-moss"><Check size={14} /> {tr("onboarding.ready")}</span>
                    ) : progress?.status !== "downloading" && (
                      <Button variant="outline" size="sm" disabled={saving} onClick={() => void download(m)}>
                        <Download size={14} /> {tr(progress?.status === "failed" ? "modelDownload.retry" : "actions.download")}
                      </Button>
                    )}
                  </div>
                );
              })}
            </Card>
            <div className="mt-6 flex flex-wrap gap-2">
              <Button variant="ghost" size="lg" disabled={saving} onClick={() => setStep(1)}>{tr("actions.back")}</Button>
              {!allPresent && (
                <Button variant="ghost" size="lg" disabled={saving} onClick={() => void save(true)}>{tr("onboarding.skip")}</Button>
              )}
              {!allPresent && !needed.some(isDownloading) && (
                <Button variant="outline" size="lg" disabled={saving} onClick={() => startAll(needed)}>
                  {tr("onboarding.downloadAll")}
                </Button>
              )}
              {!essentialsPresent && !essentials.some(isDownloading) && (
                <Button variant="primary" size="lg" disabled={saving} onClick={() => startAll(essentials)}>
                  <Download size={16} /> {tr("onboarding.startEssential")}
                </Button>
              )}
              {essentialsPresent && (
                <Button
                  variant="primary"
                  size="lg"
                  disabled={saving}
                  onClick={() => {
                    if (!allPresent) startAll(later);
                    void save(true);
                  }}
                >
                  {allPresent ? tr("onboarding.open") : tr("onboarding.openBackground")} <ArrowRight size={16} />
                </Button>
              )}
            </div>
          </div>
        )}
        {settingsError !== null && <p role="alert" className="mt-3 text-sm text-ember">{displayError(settingsError)}</p>}
      </div>
    </div>
  );
}
